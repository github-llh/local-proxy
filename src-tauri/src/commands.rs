//! Tauri 命令层：前端与 Rust 状态的唯一接口。
//!
//! 设计约定：
//! - 所有命令返回 `Result<T, String>`（错误信息直接展示给用户）；
//! - 配置写操作自动落盘（用户偏好：写入类操作默认落盘、可撤销）；
//! - 前端不直接读文件，全部经本层。

use std::path::Path;

use localproxy_core::config::AppConfig;
use localproxy_launcher::app_entry::AppEntry;
use tauri::State;

use crate::state::{core_running, kill_core, shim_binary_path, spawn_core, AppSession, AppState};

/// 核心运行状态（popover 展示）。
#[derive(serde::Serialize)]
pub struct CoreStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub uptime_secs: u64,
    pub socks5_port: u16,
    pub http_port: u16,
    pub active_upstream: String,
    pub upstream_type: String,
}

/// 运行中应用会话（前端展示）。
#[derive(serde::Serialize, Clone)]
pub struct SessionInfo {
    pub entry_id: String,
    pub entry_name: String,
    pub pid: u32,
    pub uptime_secs: u64,
}

/// 查询核心状态。
#[tauri::command]
pub fn get_status(state: State<'_, AppState>) -> CoreStatus {
    let config = state.config.lock().unwrap();
    let running = core_running(&state);
    let (pid, uptime) = {
        let guard = state.core.lock().unwrap();
        match guard.as_ref() {
            Some(core) if running => (Some(core.child.id()), core.started_at.elapsed().as_secs()),
            _ => (None, 0),
        }
    };
    let upstream = config.upstream.active_server();
    CoreStatus {
        running,
        pid,
        uptime_secs: uptime,
        socks5_port: config.socks5_port,
        http_port: config.http_port,
        active_upstream: upstream
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "未配置".to_string()),
        upstream_type: upstream.map(|s| s.server_type.clone()).unwrap_or_default(),
    }
}

/// 启动本地核心。
#[tauri::command]
pub fn start_core(state: State<'_, AppState>) -> Result<u32, String> {
    spawn_core(&state)
}

/// 停止本地核心。
#[tauri::command]
pub fn stop_core(state: State<'_, AppState>) -> Result<(), String> {
    kill_core(&state)
}

/// 应用列表（含运行状态）。
#[tauri::command]
pub fn list_apps(state: State<'_, AppState>) -> Vec<AppEntry> {
    // 先清理已退出的会话
    prune_sessions(&state);
    let config = state.config.lock().unwrap();
    config.apps.clone()
}

/// 运行中的会话列表。
#[tauri::command]
pub fn list_sessions(state: State<'_, AppState>) -> Vec<SessionInfo> {
    prune_sessions(&state);
    let sessions = state.sessions.lock().unwrap();
    sessions
        .iter()
        .map(|s: &AppSession| SessionInfo {
            entry_id: s.entry_id.clone(),
            entry_name: s.entry_name.clone(),
            pid: s.pid,
            uptime_secs: s.started_at.elapsed().as_secs(),
        })
        .collect()
}

/// 登记新应用（.app / .exe 路径）。
#[tauri::command]
pub fn add_app(state: State<'_, AppState>, path: String) -> Result<AppEntry, String> {
    let entry = AppEntry::from_path(Path::new(&path))
        .ok_or_else(|| format!("路径不存在或不是有效应用: {path}"))?;
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    // 去重（同路径）
    if config.apps.iter().any(|a| a.path == entry.path) {
        return Err("该应用已登记".to_string());
    }
    config.apps.push(entry.clone());
    drop(config);
    state.save_config()?;
    Ok(entry)
}

/// 移除应用登记。
#[tauri::command]
pub fn remove_app(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    config.apps.retain(|a| a.id != id);
    drop(config);
    state.save_config()
}

/// 以代理模式启动应用（注入 shim）。
#[tauri::command]
pub fn launch_app(state: State<'_, AppState>, id: String) -> Result<SessionInfo, String> {
    prune_sessions(&state);

    let (entry, core_host, core_port, fakeip_cidr) = {
        let config = state.config.lock().map_err(|e| e.to_string())?;
        let entry = config
            .apps
            .iter()
            .find(|a| a.id == id)
            .cloned()
            .ok_or_else(|| "应用不存在".to_string())?;
        (
            entry,
            config.listen_host.clone(),
            config.socks5_port,
            "198.18.0.0/15".to_string(),
        )
    };

    // 核心必须在线（shim 拨号目标）
    if !core_running(&state) {
        spawn_core(&state)?;
    }

    let shim = shim_binary_path();
    if !shim.exists() {
        return Err(format!(
            "shim 不存在: {}（开发模式请先 cargo build -p localproxy-shim-macos）",
            shim.display()
        ));
    }

    // 平台分发
    let session = launch_via_platform(&entry, &shim, &core_host, core_port, &fakeip_cidr)?;

    let info = SessionInfo {
        entry_id: session.entry_id.clone(),
        entry_name: session.entry_name.clone(),
        pid: session.pid,
        uptime_secs: 0,
    };
    state
        .sessions
        .lock()
        .map_err(|e| e.to_string())?
        .push(session);
    Ok(info)
}

/// 平台启动分发（macOS：DYLD 注入；Windows：挂起进程 + DLL 注入）。
#[cfg(target_os = "macos")]
fn launch_via_platform(
    entry: &AppEntry,
    shim: &Path,
    core_host: &str,
    core_port: u16,
    fakeip_cidr: &str,
) -> Result<crate::state::AppSession, String> {
    use localproxy_launcher::macos;

    let session =
        macos::launch(entry, shim, core_host, core_port, fakeip_cidr).map_err(|e| e.to_string())?;
    Ok(crate::state::AppSession {
        entry_id: session.entry_id,
        entry_name: entry.name.clone(),
        pid: session.child.id(),
        started_at: std::time::Instant::now(),
    })
}

#[cfg(target_os = "windows")]
fn launch_via_platform(
    entry: &AppEntry,
    shim: &Path,
    core_host: &str,
    core_port: u16,
    fakeip_cidr: &str,
) -> Result<crate::state::AppSession, String> {
    use localproxy_launcher::windows;
    use windows::Win32::System::Threading::GetProcessId;

    let session = windows::launch(entry, shim, core_host, core_port, fakeip_cidr)
        .map_err(|e| e.to_string())?;
    let pid = unsafe { GetProcessId(session.process) };
    Ok(crate::state::AppSession {
        entry_id: session.entry_id,
        entry_name: entry.name.clone(),
        pid,
        started_at: std::time::Instant::now(),
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn launch_via_platform(
    _entry: &AppEntry,
    _shim: &Path,
    _core_host: &str,
    _core_port: u16,
    _fakeip_cidr: &str,
) -> Result<crate::state::AppSession, String> {
    Err("当前平台不支持启动注入".to_string())
}

/// 停止运行中的应用（按 entry id）。
#[tauri::command]
pub fn stop_app(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let mut sessions = state.sessions.lock().map_err(|e| e.to_string())?;
    let target = sessions.iter().find(|s| s.entry_id == id).map(|s| s.pid);
    let Some(pid) = target else {
        return Err("应用未在运行".to_string());
    };
    // SIGTERM（Unix）/ TerminateProcess（Windows）
    #[cfg(unix)]
    {
        // SAFETY: pid 来自本进程启动的子进程
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }
    #[cfg(not(unix))]
    {
        // Windows: 用 taskkill 兜底（M4 换 TerminateProcess）
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
    }
    sessions.retain(|s| s.entry_id != id);
    Ok(())
}

/// 读取完整配置（设置窗口）。
#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> Result<AppConfig, String> {
    Ok(state.config.lock().map_err(|e| e.to_string())?.clone())
}

/// 保存完整配置（设置窗口保存按钮）。
///
/// 若 `ui.launch_at_login` 与系统实际状态不一致，同步系统开机自启设置。
#[tauri::command]
pub fn save_config(state: State<'_, AppState>, config: AppConfig) -> Result<(), String> {
    let launch_at_login = config.ui.launch_at_login;
    let mut current = state.config.lock().map_err(|e| e.to_string())?;
    let login_changed = current.ui.launch_at_login != launch_at_login;
    *current = config;
    drop(current);
    state.save_config()?;

    if login_changed {
        let result = if launch_at_login {
            crate::autostart::enable()
        } else {
            crate::autostart::disable()
        };
        if let Err(e) = result {
            // 自启设置失败不阻塞配置保存，回滚内存标记
            let mut current = state.config.lock().map_err(|e| e.to_string())?;
            current.ui.launch_at_login = !launch_at_login;
            return Err(format!("开机自启设置失败: {e}"));
        }
    }
    Ok(())
}

/// 查询系统开机自启实际状态。
#[tauri::command]
pub fn get_launch_at_login() -> bool {
    crate::autostart::is_enabled()
}

/// 切换激活上游（热切换，不影响已建连接）。
#[tauri::command]
pub fn select_upstream(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let mut config = state.config.lock().map_err(|e| e.to_string())?;
    if !config.upstream.servers.iter().any(|s| s.id == id) {
        return Err("上游不存在".to_string());
    }
    config.upstream.active = id;
    drop(config);
    state.save_config()
}

/// 清理已退出的会话（按 pid 存活判断）。
fn prune_sessions(state: &AppState) {
    let mut sessions = match state.sessions.lock() {
        Ok(s) => s,
        Err(_) => return,
    };
    sessions.retain(|s| pid_alive(s.pid));
}

/// pid 是否存活。
fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: kill(pid, 0) 仅探测存在性
        unsafe { libc::kill(pid as i32, 0) == 0 || *libc::__error() == libc::EPERM }
    }
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // SAFETY: OpenProcess 失败返回 NULL
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) };
        match process {
            Ok(h) if !h.is_invalid() => {
                let mut code = 0u32;
                let ok = unsafe { GetExitCodeProcess(h, &mut code) };
                unsafe { CloseHandle(h) };
                // STILL_ACTIVE = 259
                ok.is_ok() && code == 259
            }
            _ => false,
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}
