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

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
    QueryInformationJobObject, TerminateJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};

use localproxy_inject::{call_remote_export, inject_dll, spawn_suspended, InjectError};

use crate::app_entry::AppEntry;

/// 启动一个被代理应用的会话。
pub struct Session {
    process: OwnedHandle,
    job: OwnedHandle,
    pid: u32,
    pub entry_id: String,
    /// Windows 只有注入模式（没有 hardened runtime 概念）
    pub mode: crate::LaunchMode,
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

/// 以代理模式启动应用。
pub fn launch(
    entry: &AppEntry,
    shim_path: &Path,
    config: &crate::LaunchConfig<'_>,
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
    let explicit = config.environment();
    let mut env_pairs: Vec<(String, String)> = std::env::vars()
        .filter(|(key, _)| {
            !crate::proxy_env::is_proxy_var(key)
                && !explicit
                    .iter()
                    .any(|(name, _)| key.eq_ignore_ascii_case(name))
        })
        .collect();
    env_pairs.extend(
        explicit
            .into_iter()
            .map(|(key, value)| (key.to_string(), value)),
    );

    // 1) 挂起创建
    let process = spawn_suspended(&exe, &cmd_line, &env_pairs)?;

    // 2) 注入 shim DLL
    if let Err(e) = inject_dll(process.process, shim_path) {
        process.terminate(1);
        return Err(e.into());
    }

    // 3) 远程调 init 装 hook
    match call_remote_export(process.process, shim_path, "localproxy_shim_init") {
        Ok(1) => {}
        Ok(_) => {
            process.terminate(1);
            return Err(LaunchError::InjectFailed("代理 shim 未初始化".to_string()));
        }
        Err(error) => {
            process.terminate(1);
            return Err(error.into());
        }
    }

    // 在恢复主进程前将其加入会话自己的 Job，停止时按句柄回收整个进程树。
    let job = unsafe { CreateJobObjectW(None, None) }
        .map(|handle| unsafe { OwnedHandle::from_raw_handle(handle.0) });
    let job = match job.and_then(|job| {
        unsafe { AssignProcessToJobObject(HANDLE(job.as_raw_handle()), process.process)? };
        Ok(job)
    }) {
        Ok(job) => job,
        Err(error) => {
            process.terminate(1);
            return Err(LaunchError::SpawnFailed(format!(
                "建立应用进程组失败: {error}"
            )));
        }
    };

    if let Err(error) = process.resume() {
        process.terminate(1);
        return Err(error.into());
    }

    tracing::info!(
        "launcher(win): 代理启动 {} (pid={})",
        exe.display(),
        process.pid
    );

    let pid = process.pid;
    let _ = unsafe { CloseHandle(process.thread) };
    Ok(Session {
        process: unsafe { OwnedHandle::from_raw_handle(process.process.0) },
        job,
        pid,
        entry_id: entry.id.clone(),
        mode: crate::LaunchMode::Injected,
    })
}

impl Session {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// 通过拥有的句柄检查原始进程，避免 PID 复用误判。
    pub fn is_running(&mut self) -> bool {
        let mut counts = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        let result = unsafe {
            QueryInformationJobObject(
                Some(HANDLE(self.job.as_raw_handle())),
                JobObjectBasicAccountingInformation,
                (&mut counts as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                std::mem::size_of_val(&counts) as u32,
                None,
            )
        };
        result.is_ok() && counts.ActiveProcesses != 0
    }

    pub fn wait(&mut self) -> std::io::Result<Option<i32>> {
        let process = HANDLE(self.process.as_raw_handle());
        if unsafe { WaitForSingleObject(process, u32::MAX) } != WAIT_OBJECT_0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut code = 0;
        unsafe { GetExitCodeProcess(process, &mut code) }.map_err(std::io::Error::other)?;
        Ok(Some(code as i32))
    }

    pub fn kill(&mut self) -> std::io::Result<()> {
        unsafe { TerminateJobObject(HANDLE(self.job.as_raw_handle()), 1) }
            .map_err(std::io::Error::other)
    }
}
