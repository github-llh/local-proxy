//! Windows 平台启动器。
//!
//! 流程（经典远程线程注入，两步初始化）：
//! 1. `CreateProcessW(CREATE_SUSPENDED)` 创建挂起的目标进程；
//! 2. `VirtualAllocEx` + `WriteProcessMemory` 写入 shim DLL 宽路径，
//!    `CreateRemoteThread(LoadLibraryW)` 注入；
//! 3. 远程调用导出函数 `localproxy_shim_init` 完成 hook 安装
//!    （规避 DllMain loader lock）；
//! 4. `ResumeThread` 恢复目标进程。
//!
//! 限制：对 PPL（受保护轻量进程）与拦截注入的安全软件无效，
//! 调用方会收到明确错误。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows::Win32::System::Threading::{
    CreateProcessW, CreateRemoteThreadEx, GetExitCodeProcess, ResumeThread, TerminateProcess,
    WaitForSingleObject, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, STARTUPINFOW,
};

use crate::app_entry::AppEntry;

/// 启动一个被代理应用的会话。
pub struct Session {
    process: HANDLE,
    thread: HANDLE,
    pub entry_id: String,
}

/// launcher 错误。
#[derive(Debug)]
pub enum LaunchError {
    /// 路径不存在
    NotFound(String),
    /// CreateProcess 失败
    SpawnFailed(String),
    /// 注入失败（分配/写入/远程线程）
    InjectFailed(String),
    /// 等待超时
    Timeout(String),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchError::NotFound(p) => write!(f, "应用不存在: {p}"),
            LaunchError::SpawnFailed(msg) => write!(f, "启动进程失败: {msg}"),
            LaunchError::InjectFailed(msg) => {
                write!(
                    f,
                    "注入失败: {msg}（目标可能受安全软件保护或为系统保护进程）"
                )
            }
            LaunchError::Timeout(msg) => write!(f, "等待超时: {msg}"),
        }
    }
}

impl std::error::Error for LaunchError {}

/// 路径转宽字符串（带 NUL 结尾的 Vec<u16>）。
fn to_wide(path: &Path) -> Vec<u16> {
    OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// 以代理模式启动应用（注入 shim DLL）。
///
/// - `shim_path`：shim DLL 绝对路径
/// - `core_host` / `core_port` / `fakeip_cidr`：经环境变量传给 shim
pub fn launch(
    entry: &AppEntry,
    shim_path: &Path,
    core_host: &str,
    core_port: u16,
    fakeip_cidr: &str,
) -> Result<Session, LaunchError> {
    let exe = entry.path_buf();
    if !exe.exists() {
        return Err(LaunchError::NotFound(exe.display().to_string()));
    }

    // 构造命令行（程序路径 + 额外参数，引号包裹路径）
    let mut cmd_line = format!("\"{}\"", exe.display());
    for arg in &entry.extra_args {
        cmd_line.push(' ');
        cmd_line.push_str(arg);
    }
    let mut cmd_line_wide: Vec<u16> = OsStr::new(&cmd_line)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // 环境变量块（含 LP_* 注入配置）
    let mut env_pairs: Vec<(String, String)> = std::env::vars().collect();
    env_pairs.push(("LP_CORE_HOST".into(), core_host.into()));
    env_pairs.push(("LP_CORE_PORT".into(), core_port.to_string()));
    env_pairs.push(("LP_FAKEIP_CIDR".into(), fakeip_cidr.into()));
    env_pairs.push(("LP_MODE".into(), "proxy".into()));
    let env_block = build_environment_block(&env_pairs);

    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    // 1) 创建挂起进程
    let created = unsafe {
        CreateProcessW(
            None,
            windows::core::PWSTR(cmd_line_wide.as_mut_ptr()),
            Some(&SECURITY_ATTRIBUTES::default()),
            Some(&SECURITY_ATTRIBUTES::default()),
            false,
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            Some(env_block.as_ptr() as *const std::ffi::c_void),
            None,
            &si,
            &mut pi,
        )
    };
    if created.is_err() {
        return Err(LaunchError::SpawnFailed(format!(
            "CreateProcessW 失败: {:?}",
            created.err()
        )));
    }

    // 2) 注入 shim DLL
    if let Err(e) = inject_dll(pi.hProcess, shim_path) {
        unsafe {
            let _ = TerminateProcess(pi.hProcess, 1);
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(pi.hProcess);
        }
        return Err(e);
    }

    // 3) 远程调用 localproxy_shim_init（hook 安装）
    if let Err(e) = call_remote_init(pi.hProcess, shim_path) {
        unsafe {
            let _ = TerminateProcess(pi.hProcess, 1);
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(pi.hProcess);
        }
        return Err(e);
    }

    // 4) 恢复目标进程
    unsafe { ResumeThread(pi.hThread) };

    tracing::info!(
        "launcher(win): 代理启动 {} (pid={})",
        exe.display(),
        pi.dwProcessId
    );

    Ok(Session {
        process: pi.hProcess,
        thread: pi.hThread,
        entry_id: entry.id.clone(),
    })
}

/// 构造 Windows 环境变量块（NUL 分隔的宽字符串，双 NUL 结尾，需按 key 排序）。
fn build_environment_block(pairs: &[(String, String)]) -> Vec<u16> {
    let mut sorted: Vec<&(String, String)> = pairs.iter().collect();
    sorted.sort_by(|a, b| a.0.to_uppercase().cmp(&b.0.to_uppercase()));
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in sorted {
        let entry = format!("{k}={v}");
        block.extend(OsStr::new(&entry).encode_wide());
        block.push(0);
    }
    block.push(0); // 结束双 NUL
    block
}

/// 远程线程 LoadLibraryW 注入 DLL。
fn inject_dll(process: HANDLE, dll_path: &Path) -> Result<(), LaunchError> {
    let path_wide = to_wide(dll_path);
    let size = path_wide.len() * std::mem::size_of::<u16>();

    // 目标进程分配内存
    let remote = unsafe {
        VirtualAllocEx(
            process,
            None,
            size,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        )
    };
    if remote.is_null() {
        return Err(LaunchError::InjectFailed("VirtualAllocEx 失败".to_string()));
    }

    // 写入 DLL 路径
    let mut written = 0usize;
    let write_ok = unsafe {
        windows::Win32::System::Diagnostics::Debug::WriteProcessMemory(
            process,
            remote,
            path_wide.as_ptr() as *const std::ffi::c_void,
            size,
            Some(&mut written),
        )
    };
    if write_ok.is_err() || written != size {
        unsafe { VirtualFreeEx(process, remote, 0, MEM_RELEASE) };
        return Err(LaunchError::InjectFailed(
            "WriteProcessMemory 失败".to_string(),
        ));
    }

    // kernel32!LoadLibraryW 地址（跨进程一致）
    let kernel32 = unsafe {
        windows::Win32::System::LibraryLoader::GetModuleHandleW(windows::core::w!("kernel32.dll"))
    };
    let kernel32 = match kernel32 {
        Ok(h) if !h.is_invalid() => h,
        _ => {
            unsafe { VirtualFreeEx(process, remote, 0, MEM_RELEASE) };
            return Err(LaunchError::InjectFailed(
                "GetModuleHandleW 失败".to_string(),
            ));
        }
    };
    let load_library = unsafe { GetProcAddress(kernel32, windows::core::s!("LoadLibraryW")) };
    let Some(load_library) = load_library else {
        unsafe { VirtualFreeEx(process, remote, 0, MEM_RELEASE) };
        return Err(LaunchError::InjectFailed(
            "LoadLibraryW 地址获取失败".to_string(),
        ));
    };

    // 远程线程执行
    let mut thread_id = 0u32;
    let thread = unsafe {
        CreateRemoteThreadEx(
            process,
            None,
            0,
            Some(std::mem::transmute::<
                *mut std::ffi::c_void,
                unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
            >(load_library as *mut std::ffi::c_void)),
            Some(remote),
            0,
            Some(&LPPROC_THREAD_ATTRIBUTE_LIST::default()),
            Some(&mut thread_id),
        )
    };
    let Ok(thread) = thread else {
        unsafe { VirtualFreeEx(process, remote, 0, MEM_RELEASE) };
        return Err(LaunchError::InjectFailed(
            "CreateRemoteThread 失败".to_string(),
        ));
    };

    let wait = unsafe { WaitForSingleObject(thread, 10_000) };
    let mut load_result = 0u32;
    let _ =
        unsafe { windows::Win32::System::Threading::GetExitCodeThread(thread, &mut load_result) };
    unsafe { CloseHandle(thread) };
    unsafe { VirtualFreeEx(process, remote, 0, MEM_RELEASE) };

    if wait != WAIT_OBJECT_0 {
        return Err(LaunchError::Timeout("等待注入线程超时".to_string()));
    }
    if load_result == 0 {
        return Err(LaunchError::InjectFailed(
            "LoadLibraryW 返回 NULL（DLL 加载失败）".to_string(),
        ));
    }
    Ok(())
}

/// 远程调用 shim 导出的 `localproxy_shim_init`（安装 hook）。
fn call_remote_init(process: HANDLE, dll_path: &Path) -> Result<(), LaunchError> {
    // 在本地加载同一 DLL 获取 init 函数地址（远进程中同一 DLL 基址 + RVA 一致）
    let wide = to_wide(dll_path);
    let module = unsafe { LoadLibraryW(windows::core::PCWSTR(wide.as_ptr())) };
    let Ok(module) = module else {
        return Err(LaunchError::InjectFailed(format!(
            "本地加载 shim 失败（获取 init 地址）: {:?}",
            module.err()
        )));
    };
    let init = unsafe { GetProcAddress(module, windows::core::s!("localproxy_shim_init")) };
    let Some(init) = init else {
        return Err(LaunchError::InjectFailed(
            "未找到导出 localproxy_shim_init".to_string(),
        ));
    };

    let mut thread_id = 0u32;
    let thread = unsafe {
        CreateRemoteThreadEx(
            process,
            None,
            0,
            Some(std::mem::transmute::<
                *mut std::ffi::c_void,
                unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
            >(init as *mut std::ffi::c_void)),
            None,
            0,
            Some(&LPPROC_THREAD_ATTRIBUTE_LIST::default()),
            Some(&mut thread_id),
        )
    };
    let Ok(thread) = thread else {
        return Err(LaunchError::InjectFailed("远程调用 init 失败".to_string()));
    };
    let wait = unsafe { WaitForSingleObject(thread, 15_000) };
    let mut init_result = 0u32;
    let _ =
        unsafe { windows::Win32::System::Threading::GetExitCodeThread(thread, &mut init_result) };
    unsafe { CloseHandle(thread) };
    if wait != WAIT_OBJECT_0 {
        return Err(LaunchError::Timeout("等待 init 超时".to_string()));
    }
    if init_result == 0 {
        return Err(LaunchError::InjectFailed(
            "hook 安装失败（localproxy_shim_init 返回 false）".to_string(),
        ));
    }
    Ok(())
}

impl Session {
    /// 进程是否仍在运行。
    pub fn is_running(&mut self) -> bool {
        let mut code = 0u32;
        if unsafe { GetExitCodeProcess(self.process, &mut code) }.is_err() {
            return false;
        }
        code == 259 // STILL_ACTIVE
    }

    /// 等待进程退出，返回退出码。
    pub fn wait(&mut self) -> std::io::Result<Option<i32>> {
        unsafe { WaitForSingleObject(self.process, u32::MAX) };
        let mut code = 0u32;
        if unsafe { GetExitCodeProcess(self.process, &mut code) }.is_err() {
            return Ok(None);
        }
        Ok(Some(code as i32))
    }

    /// 终止进程。
    pub fn kill(&mut self) -> std::io::Result<()> {
        unsafe { TerminateProcess(self.process, 1) }
            .map_err(|e| std::io::Error::from_raw_os_error(e.code().0))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.thread);
            let _ = CloseHandle(self.process);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_block_shape() {
        let pairs = vec![
            ("PATH".to_string(), "C:\\Windows".to_string()),
            ("LP_CORE_PORT".to_string(), "1080".to_string()),
        ];
        let block = build_environment_block(&pairs);
        // 双 NUL 结尾
        assert_eq!(block[block.len() - 1], 0);
        assert_eq!(block[block.len() - 2], 0);
        // 转回字符串校验（大写 key 排序：LP_CORE_PORT < PATH）
        let text: String = String::from_utf16_lossy(&block);
        assert!(text.contains("LP_CORE_PORT=1080"));
        assert!(text.contains("PATH=C:\\Windows"));
    }

    #[test]
    fn to_wide_has_nul_terminator() {
        let wide = to_wide(Path::new("C:\\shim.dll"));
        assert_eq!(*wide.last().unwrap(), 0);
        assert!(wide.len() > 1);
    }
}
