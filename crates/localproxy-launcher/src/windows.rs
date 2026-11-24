//! Windows 平台启动器。
//!
//! 流程（复用 `localproxy-inject` 共享原语）：
//! 1. `CreateProcessW(CREATE_SUSPENDED)` 创建挂起的目标进程；
//! 2. `inject_dll` 远程线程 `LoadLibraryW` 注入 shim DLL；
//! 3. `call_remote_export` 远程调用 `localproxy_shim_init` 完成 hook 安装
//!    （规避 DllMain loader lock）；
//! 4. `ResumeThread` 恢复目标进程。
//!
//! 限制：对 PPL（受保护轻量进程）与拦截注入的安全软件无效，
//! 调用方会收到明确错误。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use localproxy_inject::{
    call_remote_export, inject_dll, spawn_suspended, InjectError, ProcessInfo,
};

use crate::app_entry::AppEntry;

/// 启动一个被代理应用的会话。
pub struct Session {
    process: ProcessInfo,
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

impl From<InjectError> for LaunchError {
    fn from(e: InjectError) -> Self {
        match e {
            InjectError::Spawn(msg) => LaunchError::SpawnFailed(msg),
            InjectError::Memory(msg) | InjectError::RemoteThread(msg) => {
                LaunchError::InjectFailed(msg)
            }
            InjectError::Timeout(msg) => LaunchError::Timeout(msg),
        }
    }
}

/// 需要清理的代理环境变量（大小写组合）。
///
/// Clash/V2RayN 等会设置系统代理与这些环境变量；若不清理，被注入应用会把
/// 连接发往代理软件的系统代理端口（双跳且行为不可预期）。清理后应用处于
/// 「无代理」环境，由 shim 全权接管。
const PROXY_ENV_VARS: [&str; 10] = [
    "http_proxy",
    "HTTP_PROXY",
    "https_proxy",
    "HTTPS_PROXY",
    "all_proxy",
    "ALL_PROXY",
    "ftp_proxy",
    "FTP_PROXY",
    "no_proxy",
    "NO_PROXY",
];

/// 以代理模式启动应用（注入 shim DLL）。
///
/// - `shim_path`：shim DLL 绝对路径
/// - `core_host` / `core_port` / `fakeip_cidr`：经环境变量传给 shim
/// - `udp_mode` / `ipv6_mode`：安全策略（block/direct/proxy、proxy/direct/block）
pub fn launch(
    entry: &AppEntry,
    shim_path: &Path,
    core_host: &str,
    core_port: u16,
    fakeip_cidr: &str,
    udp_mode: &str,
    ipv6_mode: &str,
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

    // 环境变量（继承当前环境 + LP_* 注入配置，清理系统代理变量）
    let mut env_pairs: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| !PROXY_ENV_VARS.contains(&k.as_str()))
        .collect();
    env_pairs.push(("LP_CORE_HOST".into(), core_host.into()));
    env_pairs.push(("LP_CORE_PORT".into(), core_port.to_string()));
    env_pairs.push(("LP_FAKEIP_CIDR".into(), fakeip_cidr.into()));
    env_pairs.push(("LP_MODE".into(), "proxy".into()));
    env_pairs.push(("LP_UDP_MODE".into(), udp_mode.into()));
    env_pairs.push(("LP_IPV6_MODE".into(), ipv6_mode.into()));

    // 1) 挂起创建
    let process = spawn_suspended(&exe, &cmd_line, &env_pairs)?;

    // 2) 注入 shim DLL
    if let Err(e) = inject_dll(process.process, shim_path) {
        process.terminate(1);
        return Err(e.into());
    }

    // 3) 远程调 init 装 hook
    if let Err(e) = call_remote_export(process.process, shim_path, "localproxy_shim_init") {
        process.terminate(1);
        return Err(e.into());
    }

    // 4) 恢复目标进程
    process.resume();

    tracing::info!(
        "launcher(win): 代理启动 {} (pid={})",
        exe.display(),
        process.pid
    );

    Ok(Session {
        process,
        entry_id: entry.id.clone(),
    })
}

impl Session {
    /// 进程是否仍在运行。
    pub fn is_running(&mut self) -> bool {
        let mut code = 0u32;
        if unsafe {
            windows::Win32::System::Threading::GetExitCodeProcess(self.process.process, &mut code)
        }
        .is_err()
        {
            return false;
        }
        code == 259 // STILL_ACTIVE
    }

    /// 等待进程退出，返回退出码。
    pub fn wait(&mut self) -> std::io::Result<Option<i32>> {
        unsafe {
            windows::Win32::System::Threading::WaitForSingleObject(self.process.process, u32::MAX)
        };
        let mut code = 0u32;
        if unsafe {
            windows::Win32::System::Threading::GetExitCodeProcess(self.process.process, &mut code)
        }
        .is_err()
        {
            return Ok(None);
        }
        Ok(Some(code as i32))
    }

    /// 终止进程。
    pub fn kill(&mut self) -> std::io::Result<()> {
        unsafe { windows::Win32::System::Threading::TerminateProcess(self.process.process, 1) }
            .map_err(|e| std::io::Error::from_raw_os_error(e.code().0))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.process.thread);
            let _ = windows::Win32::Foundation::CloseHandle(self.process.process);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_line_quoting() {
        let entry = AppEntry {
            id: "x".into(),
            name: "App".into(),
            path: "C:\\Program Files\\App\\app.exe".into(),
            enabled: true,
            extra_args: vec!["--flag".into(), "value".into()],
            note: String::new(),
        };
        let mut cmd_line = format!("\"{}\"", entry.path_buf().display());
        for arg in &entry.extra_args {
            cmd_line.push(' ');
            cmd_line.push_str(arg);
        }
        assert_eq!(cmd_line, "\"C:\\Program Files\\App\\app.exe\" --flag value");
    }

    #[test]
    fn launch_missing_exe_errors() {
        let entry = AppEntry {
            id: "x".into(),
            name: "Nope".into(),
            path: "C:\\definitely\\not\\exist.exe".into(),
            enabled: true,
            extra_args: vec![],
            note: String::new(),
        };
        let err = launch(
            &entry,
            Path::new("C:\\shim.dll"),
            "127.0.0.1",
            1080,
            "198.18.0.0/15",
            "block",
            "proxy",
        )
        .unwrap_err();
        assert!(matches!(err, LaunchError::NotFound(_)));
    }
}
