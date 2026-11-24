//! LocalProxy 共享注入原语（Windows）。
//!
//! 被 `localproxy-launcher`（初始注入）与 `localproxy-shim-win`
//! （CreateProcess hook 子进程注入）共用，消除重复实现。
//!
//! 提供：
//! - `build_env_block`：环境变量块构造（排序 + 双 NUL 结尾）
//! - `inject_dll`：远程线程 `LoadLibraryW` 注入
//! - `call_remote_export`：远程调用目标进程内 DLL 的导出函数
//! - `ProcessInfo`：挂起进程句柄集合

// 整个 crate 仅在 Windows target 编译（macOS 上构建空壳）
#![cfg(windows)]
#![allow(clippy::missing_safety_doc)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Memory::{
    VirtualAllocEx, VirtualFreeEx, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows::Win32::System::Threading::{
    CreateProcessW, CreateRemoteThreadEx, GetExitCodeThread, ResumeThread, TerminateProcess,
    WaitForSingleObject, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, STARTUPINFOW,
};

/// 注入错误。
#[derive(Debug)]
pub enum InjectError {
    /// CreateProcess 失败
    Spawn(String),
    /// 内存分配/写入失败
    Memory(String),
    /// 远程线程创建/等待失败
    RemoteThread(String),
    /// 超时
    Timeout(String),
}

impl std::fmt::Display for InjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InjectError::Spawn(msg) => write!(f, "创建进程失败: {msg}"),
            InjectError::Memory(msg) => write!(f, "进程内存操作失败: {msg}"),
            InjectError::RemoteThread(msg) => write!(f, "远程线程失败: {msg}"),
            InjectError::Timeout(msg) => write!(f, "等待超时: {msg}"),
        }
    }
}

impl std::error::Error for InjectError {}

/// 挂起创建的进程信息。
pub struct ProcessInfo {
    pub process: HANDLE,
    pub thread: HANDLE,
    pub pid: u32,
}

impl ProcessInfo {
    /// 恢复主线程。
    pub fn resume(&self) {
        unsafe { ResumeThread(self.thread) };
    }

    /// 终止并关闭句柄。
    pub fn terminate(&self, exit_code: u32) {
        unsafe {
            let _ = TerminateProcess(self.process, exit_code);
            let _ = CloseHandle(self.thread);
            let _ = CloseHandle(self.process);
        }
    }
}

/// 路径转宽字符串（带 NUL）。
pub fn to_wide(path: &Path) -> Vec<u16> {
    OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// 构造环境变量块（按 key 大写排序，`K=V\0`，双 NUL 结尾）。
pub fn build_env_block(pairs: &[(String, String)]) -> Vec<u16> {
    let mut sorted: Vec<&(String, String)> = pairs.iter().collect();
    sorted.sort_by(|a, b| a.0.to_uppercase().cmp(&b.0.to_uppercase()));
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in sorted {
        let entry = format!("{k}={v}");
        block.extend(OsStr::new(&entry).encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

/// 挂起创建进程（`CREATE_SUSPENDED` + Unicode 环境）。
pub fn spawn_suspended(
    exe: &Path,
    cmd_line: &str,
    env_pairs: &[(String, String)],
) -> Result<ProcessInfo, InjectError> {
    let mut cmd_line_wide: Vec<u16> = OsStr::new(cmd_line)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let env_block = build_env_block(env_pairs);

    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    let created = unsafe {
        CreateProcessW(
            None,
            windows::core::PWSTR(cmd_line_wide.as_mut_ptr()),
            Some(&SECURITY_ATTRIBUTES::default()),
            Some(&SECURITY_ATTRIBUTES::default()),
            false,
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            Some(env_block.as_ptr() as *const std::ffi::c_void),
            exe.parent(),
            &si,
            &mut pi,
        )
    };
    if created.is_err() {
        return Err(InjectError::Spawn(format!("{:?}", created.err())));
    }
    Ok(ProcessInfo {
        process: pi.hProcess,
        thread: pi.hThread,
        pid: pi.dwProcessId,
    })
}

/// 远程线程 `LoadLibraryW` 注入 DLL。
pub fn inject_dll(process: HANDLE, dll_path: &Path) -> Result<(), InjectError> {
    let path_wide = to_wide(dll_path);
    let size = path_wide.len() * std::mem::size_of::<u16>();

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
        return Err(InjectError::Memory("VirtualAllocEx 失败".to_string()));
    }

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
        return Err(InjectError::Memory("WriteProcessMemory 失败".to_string()));
    }

    let kernel32 = match unsafe { GetModuleHandleW(windows::core::w!("kernel32.dll")) } {
        Ok(h) if !h.is_invalid() => h,
        _ => {
            unsafe { VirtualFreeEx(process, remote, 0, MEM_RELEASE) };
            return Err(InjectError::RemoteThread(
                "GetModuleHandleW(kernel32) 失败".to_string(),
            ));
        }
    };
    let Some(load_library) =
        (unsafe { GetProcAddress(kernel32, windows::core::s!("LoadLibraryW")) })
    else {
        unsafe { VirtualFreeEx(process, remote, 0, MEM_RELEASE) };
        return Err(InjectError::RemoteThread(
            "LoadLibraryW 地址获取失败".to_string(),
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
            >(load_library as *mut std::ffi::c_void)),
            Some(remote),
            0,
            Some(&LPPROC_THREAD_ATTRIBUTE_LIST::default()),
            Some(&mut thread_id),
        )
    };
    let Ok(thread) = thread else {
        unsafe { VirtualFreeEx(process, remote, 0, MEM_RELEASE) };
        return Err(InjectError::RemoteThread(
            "CreateRemoteThread(LoadLibraryW) 失败".to_string(),
        ));
    };

    let wait = unsafe { WaitForSingleObject(thread, 10_000) };
    let mut load_result = 0u32;
    let _ = unsafe { GetExitCodeThread(thread, &mut load_result) };
    unsafe { CloseHandle(thread) };
    unsafe { VirtualFreeEx(process, remote, 0, MEM_RELEASE) };

    if wait != WAIT_OBJECT_0 {
        return Err(InjectError::Timeout(
            "等待 LoadLibraryW 远程线程超时".to_string(),
        ));
    }
    if load_result == 0 {
        return Err(InjectError::RemoteThread(
            "LoadLibraryW 返回 NULL（DLL 加载失败）".to_string(),
        ));
    }
    Ok(())
}

/// 远程调用目标进程内已加载 DLL 的导出函数（`localproxy_shim_init` 等）。
///
/// 先在本地加载同一 DLL 取得导出地址，再以「基址 + RVA」方式在远进程调用
/// （同一 DLL 在两个进程中的 RVA 一致）。
pub fn call_remote_export(
    process: HANDLE,
    dll_path: &Path,
    export: &str,
) -> Result<u32, InjectError> {
    let wide = to_wide(dll_path);
    let module = unsafe {
        windows::Win32::System::LibraryLoader::LoadLibraryW(windows::core::PCWSTR(wide.as_ptr()))
    };
    let Ok(module) = module else {
        return Err(InjectError::RemoteThread(format!(
            "本地加载 DLL 失败（获取 {export} 地址）: {:?}",
            module.err()
        )));
    };
    let Some(func) =
        (unsafe { GetProcAddress(module, windows::core::PCSTR::from_raw(export.as_ptr())) })
    else {
        return Err(InjectError::RemoteThread(format!("未找到导出 {export}")));
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
            >(func as *mut std::ffi::c_void)),
            None,
            0,
            Some(&LPPROC_THREAD_ATTRIBUTE_LIST::default()),
            Some(&mut thread_id),
        )
    };
    let Ok(thread) = thread else {
        return Err(InjectError::RemoteThread(format!("远程调用 {export} 失败")));
    };
    let wait = unsafe { WaitForSingleObject(thread, 15_000) };
    let mut result = 0u32;
    let _ = unsafe { GetExitCodeThread(thread, &mut result) };
    unsafe { CloseHandle(thread) };
    if wait != WAIT_OBJECT_0 {
        return Err(InjectError::Timeout(format!("等待 {export} 超时")));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_block_shape() {
        let pairs = vec![
            ("PATH".to_string(), "C:\\Windows".to_string()),
            ("LP_CORE_PORT".to_string(), "1080".to_string()),
        ];
        let block = build_env_block(&pairs);
        assert_eq!(block[block.len() - 1], 0);
        assert_eq!(block[block.len() - 2], 0);
        let text = String::from_utf16_lossy(&block);
        assert!(text.contains("LP_CORE_PORT=1080"));
        assert!(text.contains("PATH=C:\\Windows"));
    }

    #[test]
    fn env_block_filters_nothing_by_itself() {
        // 过滤（代理变量清理）由调用方负责，本函数只做排序与编码
        let block = build_env_block(&[("http_proxy".to_string(), "x".to_string())]);
        let text = String::from_utf16_lossy(&block);
        assert!(text.contains("http_proxy=x"));
    }

    #[test]
    fn to_wide_has_nul_terminator() {
        let wide = to_wide(Path::new("C:\\shim.dll"));
        assert_eq!(*wide.last().unwrap(), 0);
        assert!(wide.len() > 1);
    }
}
