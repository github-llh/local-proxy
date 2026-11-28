//! Tauri 命令层：前端与 Rust 状态的唯一接口。
//!
//! 设计约定：
//! - 所有命令返回 `Result<T, String>`（错误信息直接展示给用户）；
//! - 配置写操作自动落盘（用户偏好：写入类操作默认落盘、可撤销）；
//! - 前端不直接读文件，全部经本层；
//! - 命令实现不跨 IO 持有配置/会话锁：启动应用、探测、停止进程均在锁外执行。

use std::path::Path;

use localproxy_core::config::{AppConfig, UpstreamServer};
use localproxy_launcher::app_entry::AppEntry;
use tauri::State;

use crate::clash::{ClashClient, ClashState};
use crate::state::{
    kill_core, lock_recover, running_core, shim_binary_path, spawn_core, AppSession, AppState,
};

/// 核心运行状态（popover 展示）。
#[derive(serde::Serialize)]
pub struct CoreStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub uptime_secs: u64,
    pub listen_host: String,
    pub socks5_port: u16,
    pub http_port: u16,
    pub control_port: u16,
    pub active_upstream: String,
    pub active_upstream_id: String,
    pub upstream_type: String,
    pub exit_ip_probe_enabled: bool,
    pub upstreams: Vec<UpstreamSummary>,
}

#[derive(serde::Serialize)]
pub struct UpstreamSummary {
    id: String,
    name: String,
    endpoint: String,
    server_type: String,
    managed: bool,
    clash_enabled: bool,
}

impl From<&UpstreamServer> for UpstreamSummary {
    fn from(server: &UpstreamServer) -> Self {
        Self {
            id: server.id.clone(),
            name: server.name.clone(),
            endpoint: server.endpoint(),
            server_type: display_protocol(server).to_string(),
            managed: server.managed.is_some(),
            clash_enabled: server.clash.is_some(),
        }
    }
}

fn display_protocol(server: &UpstreamServer) -> &str {
    server
        .managed
        .as_ref()
        .map(|node| node.protocol.as_str())
        .unwrap_or(&server.server_type)
}

#[tauri::command]
pub fn find_mihomo(path: String) -> Result<String, String> {
    localproxy_core::managed::mihomo_path(&path).map(|path| path.to_string_lossy().into_owned())
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
    let config = lock_recover(&state.config);
    let mut guard = lock_recover(&state.core);
    let core = running_core(&mut guard);
    let listeners = core.map_or(&*config, |core| &core.startup_config);
    let upstream = config.upstream.active_server();
    CoreStatus {
        running: core.is_some(),
        pid: core.map(|core| core.child.id()),
        uptime_secs: core.map_or(0, |core| core.started_at.elapsed().as_secs()),
        listen_host: listeners.listen_host.clone(),
        socks5_port: listeners.socks5_port,
        http_port: listeners.http_port,
        control_port: listeners.control_port,
        active_upstream: upstream
            .map(|s| {
                if s.name.is_empty() {
                    s.id.clone()
                } else {
                    s.name.clone()
                }
            })
            .unwrap_or_default(),
        active_upstream_id: upstream.map(|s| s.id.clone()).unwrap_or_default(),
        upstream_type: upstream
            .map(display_protocol)
            .unwrap_or_default()
            .to_string(),
        exit_ip_probe_enabled: config.ui.exit_ip_probe,
        upstreams: config
            .upstream
            .servers
            .iter()
            .map(UpstreamSummary::from)
            .collect(),
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
    lock_recover(&state.config).apps.clone()
}

/// 运行中的会话列表。
#[tauri::command]
pub fn list_sessions(state: State<'_, AppState>) -> Vec<SessionInfo> {
    prune_sessions(&state);
    let sessions = lock_recover(&state.sessions);
    sessions
        .iter()
        .map(|s: &AppSession| SessionInfo {
            entry_id: s.entry_id.clone(),
            entry_name: s.entry_name.clone(),
            pid: s.pid(),
            uptime_secs: s.started_at.elapsed().as_secs(),
        })
        .collect()
}

/// 登记新应用（.app / .exe 路径）。
#[tauri::command]
pub fn add_app(state: State<'_, AppState>, path: String) -> Result<AppEntry, String> {
    let entry = AppEntry::from_path(Path::new(&path))
        .ok_or_else(|| format!("路径不存在或不是有效应用: {path}"))?;
    state.mutate_local(|config| {
        if config
            .apps
            .iter()
            .any(|registered| registered.path == entry.path)
        {
            return Err("该应用已登记".to_string());
        }
        config.apps.push(entry.clone());
        Ok(entry.clone())
    })
}

/// 移除应用登记。
#[tauri::command]
pub fn remove_app(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.mutate_local(move |config| {
        let before = config.apps.len();
        config.apps.retain(|app| app.id != id);
        if config.apps.len() == before {
            return Err("应用不存在".to_string());
        }
        Ok(())
    })
}

/// 更新应用登记（名称 / 备注 / 附加参数 / 启用状态）。
#[tauri::command]
pub fn update_app(
    state: State<'_, AppState>,
    id: String,
    name: Option<String>,
    note: Option<String>,
    extra_args: Option<Vec<String>>,
    enabled: Option<bool>,
) -> Result<AppEntry, String> {
    state.mutate_local(move |config| {
        let app = config
            .apps
            .iter_mut()
            .find(|app| app.id == id)
            .ok_or_else(|| "应用不存在".to_string())?;
        if let Some(name) = name {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                app.name = trimmed.to_string();
            }
        }
        if let Some(note) = note {
            app.note = note;
        }
        if let Some(extra_args) = extra_args {
            app.extra_args = extra_args;
        }
        if let Some(enabled) = enabled {
            app.enabled = enabled;
        }
        Ok(app.clone())
    })
}

/// 以代理模式启动应用（注入 shim）。
#[tauri::command]
pub fn launch_app(state: State<'_, AppState>, id: String) -> Result<SessionInfo, String> {
    prune_sessions(&state);

    let entry = lock_recover(&state.config)
        .apps
        .iter()
        .find(|app| app.id == id && app.enabled)
        .cloned()
        .ok_or_else(|| "应用不存在或已禁用".to_string())?;
    if lock_recover(&state.sessions)
        .iter()
        .any(|session| session.entry_id == entry.id)
    {
        return Err("应用已在运行".to_string());
    }

    spawn_core(&state)?;
    let (core_addr, policy_path) = {
        let mut guard = lock_recover(&state.core);
        let running = running_core(&mut guard).ok_or("LocalProxy 核心已退出")?;
        let core_addr = running
            .startup_config
            .socks5_endpoint()
            .parse()
            .map_err(|e| format!("核心地址无效: {e}"))?;
        (core_addr, running.policy_path.clone())
    };
    let launch = localproxy_launcher::LaunchConfig {
        core: core_addr,
        policy_path: &policy_path,
    };

    let shim = shim_binary_path();
    if !shim.exists() {
        return Err(format!("找不到当前平台的代理 shim: {}", shim.display()));
    }

    // 平台注入启动耗时较长，且不持有任何状态锁。
    let session = launch_via_platform(&entry, &shim, &launch)?;
    let info = SessionInfo {
        entry_id: session.entry_id.clone(),
        entry_name: session.entry_name.clone(),
        pid: session.pid(),
        uptime_secs: 0,
    };
    {
        let mut sessions = lock_recover(&state.sessions);
        if sessions
            .iter()
            .any(|existing| existing.entry_id == entry.id)
        {
            // 并发启动：回收本次进程，保留先到者。
            let mut duplicated = session;
            let _ = duplicated.stop();
            return Err("应用已在运行".to_string());
        }
        sessions.push(session);
    }

    // 记录最近启动时间（「最近使用」排序）
    state
        .mutate_local(move |config| {
            if let Some(app) = config.apps.iter_mut().find(|app| app.id == id) {
                app.last_launched_at = Some(now_unix_secs());
            }
            Ok(())
        })
        .ok();
    Ok(info)
}

/// 出口 IP 探测（风控自检；经上游 SOCKS5 访问 HTTP 端点）。
#[tauri::command]
pub fn probe_exit_ip(state: State<'_, AppState>) -> Result<crate::probe::ExitIpInfo, String> {
    let (endpoints, timeout_ms) = {
        let config = lock_recover(&state.config);
        if !config.ui.exit_ip_probe {
            return Err("请先在设置中启用出口 IP 探测".to_string());
        }
        (config.probe.endpoints.clone(), config.probe.timeout_ms)
    };
    // 探测经核心的 SOCKS5 端口发起，端口以运行时实际监听为准
    let listeners = state.running_startup_config()?;
    crate::probe::probe_exit_ip(&listeners.socks5_endpoint(), &endpoints, timeout_ms)
        .map_err(|e| e.to_string())
}

/// 最近连接记录（控制面诊断日志）。
#[tauri::command]
pub fn list_recent_connections(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<localproxy_core::diagnostics::ConnectionRecord>, String> {
    state
        .control_client()?
        .connections(limit.unwrap_or(50).min(500))
        .map_err(|e| e.to_string())
}

/// 当前 Unix 时间戳（秒）。
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 设置应用启用状态（popover 快速开关）。
#[tauri::command]
pub fn set_app_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    state.mutate_local(move |config| {
        let app = config
            .apps
            .iter_mut()
            .find(|app| app.id == id)
            .ok_or_else(|| "应用不存在".to_string())?;
        app.enabled = enabled;
        Ok(())
    })
}

#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UpstreamTest {
    EngineReady,
    Port { elapsed_ms: u64 },
}

/// 测试上游连通性（TCP 连接延迟，毫秒）。
///
/// 轻量探测：对上游 host:port 建立 TCP 连接并计时；
/// 不发送任何业务数据，不经过代理链。独立节点会真实启动内核验证配置，
/// 因此同一上游的并发测试串行执行，避免重复拉起多个内核。
#[tauri::command]
pub async fn test_upstream(
    state: State<'_, AppState>,
    id: String,
    draft: Option<UpstreamServer>,
    mihomo_path: Option<String>,
) -> Result<UpstreamTest, String> {
    let (server, configured_engine) = match draft {
        Some(server) => {
            let engine = lock_recover(&state.config).mihomo_path.clone();
            (server, engine)
        }
        None => {
            let config = lock_recover(&state.config);
            let server = config
                .upstream
                .servers
                .iter()
                .find(|s| s.id == id)
                .cloned()
                .ok_or_else(|| "上游不存在".to_string())?;
            (server, config.mihomo_path.clone())
        }
    };

    let probe = state.probe_lock(&server.id);
    let _serial = probe.lock().await;

    if server.managed.is_some() {
        let path = match mihomo_path {
            Some(path) => path,
            None => configured_engine,
        };
        localproxy_core::managed::ManagedRuntime::start(&server, &path).await?;
        return Ok(UpstreamTest::EngineReady);
    }
    let host = server
        .host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']');
    let start = std::time::Instant::now();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect((host, server.port)),
    )
    .await
    .map_err(|_| "连接上游超时".to_string())?
    .map_err(|e| format!("连接失败: {e}"))?;
    Ok(UpstreamTest::Port {
        elapsed_ms: start.elapsed().as_millis() as u64,
    })
}

/// 启动后保留系统进程句柄，停止和清理均不按裸 PID 查找。
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn launch_via_platform(
    entry: &AppEntry,
    shim: &Path,
    config: &localproxy_launcher::LaunchConfig<'_>,
) -> Result<AppSession, String> {
    let process =
        localproxy_launcher::launch(entry, shim, config).map_err(|error| error.to_string())?;
    Ok(AppSession {
        entry_id: entry.id.clone(),
        entry_name: entry.name.clone(),
        process,
        started_at: std::time::Instant::now(),
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn launch_via_platform(
    _entry: &AppEntry,
    _shim: &Path,
    _config: &localproxy_launcher::LaunchConfig<'_>,
) -> Result<AppSession, String> {
    Err("当前平台不支持启动注入".to_string())
}

#[tauri::command]
pub fn stop_app(state: State<'_, AppState>, id: String) -> Result<(), String> {
    // 先把会话移出列表再等待退出，避免界面读取被进程回收阻塞。
    let mut session = {
        let mut sessions = lock_recover(&state.sessions);
        sessions.retain_mut(AppSession::is_running);
        let index = sessions
            .iter()
            .position(|session| session.entry_id == id)
            .ok_or("应用未在运行")?;
        sessions.remove(index)
    };
    session
        .stop()
        .map_err(|error| format!("停止应用失败: {error}"))
}

/// 读取完整配置（设置窗口）。
#[tauri::command]
pub fn get_config(state: State<'_, AppState>) -> Result<AppConfig, String> {
    Ok(lock_recover(&state.config).clone())
}

/// 保存完整配置（设置窗口保存按钮）。
///
/// 未主动切换上游时保留当前激活项，避免旧窗口的快照覆盖菜单栏的新选择；
/// `apps` 由独立命令维护，这里跟随当前值。
/// 若 `ui.launch_at_login` 与系统实际状态不一致，同步系统开机自启设置。
#[tauri::command]
pub async fn save_config(
    state: State<'_, AppState>,
    config: AppConfig,
    previous_active: Option<String>,
) -> Result<AppConfig, String> {
    let launch_at_login = config.ui.launch_at_login;
    let (applied, login_changed) = state
        .commit_config(move |current| {
            let mut next = config;
            if previous_active.as_ref() == Some(&next.upstream.active)
                && current
                    .upstream
                    .servers
                    .iter()
                    .any(|s| s.id == current.upstream.active)
            {
                next.upstream.active.clone_from(&current.upstream.active);
            }
            next.apps.clone_from(&current.apps);
            let login_changed = current.ui.launch_at_login != launch_at_login;
            Ok((next, login_changed))
        })
        .await?;

    if login_changed {
        let result = if launch_at_login {
            crate::autostart::enable()
        } else {
            crate::autostart::disable()
        };
        if let Err(e) = result {
            // 自启设置失败时同步回滚内存和配置文件中的标记。
            state
                .mutate_local(move |current| {
                    current.ui.launch_at_login = !launch_at_login;
                    Ok(())
                })
                .ok();
            return Err(format!("开机自启设置失败: {e}"));
        }
    }
    Ok(applied)
}

/// 查询系统开机自启实际状态。
#[tauri::command]
pub fn get_launch_at_login() -> bool {
    crate::autostart::is_enabled()
}

/// 切换激活上游（热切换，不影响已建连接）。
#[tauri::command]
pub async fn select_upstream(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state
        .commit_config(move |current| {
            if !current.upstream.servers.iter().any(|s| s.id == id) {
                return Err("上游不存在".to_string());
            }
            let mut next = current.clone();
            next.upstream.active = id;
            Ok((next, ()))
        })
        .await
        .map(|_| ())
}

fn active_clash(state: &AppState, upstream_id: &str) -> Result<ClashClient, String> {
    let config = lock_recover(&state.config);
    let server = config
        .upstream
        .active_server()
        .filter(|s| s.id == upstream_id)
        .ok_or_else(|| "激活上游已变化，请刷新后重试".to_string())?;
    let controller = server
        .clash
        .as_ref()
        .ok_or_else(|| "当前上游尚未配置 Clash 控制地址".to_string())?;
    ClashClient::new(controller)
}

#[tauri::command]
pub async fn get_clash_state(
    state: State<'_, AppState>,
    upstream_id: String,
) -> Result<ClashState, String> {
    active_clash(&state, &upstream_id)?.snapshot().await
}

#[tauri::command]
pub async fn select_clash_proxy(
    state: State<'_, AppState>,
    upstream_id: String,
    group: String,
    choice: String,
) -> Result<(), String> {
    active_clash(&state, &upstream_id)?
        .select(&group, &choice)
        .await
}

/// 清理已退出的会话。
fn prune_sessions(state: &AppState) {
    lock_recover(&state.sessions).retain_mut(AppSession::is_running);
}
