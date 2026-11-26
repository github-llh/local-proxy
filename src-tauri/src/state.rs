//! 应用共享状态：配置、核心子进程、被代理应用会话。

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Child;
use std::sync::Mutex;
use std::time::Duration;

use localproxy_core::config::{AppConfig, ListenerInfo};

use crate::control_client::ControlClient;

/// 核心进程句柄。
pub struct CoreProcess {
    pub child: Child,
    pub started_at: std::time::Instant,
    pub startup_config: AppConfig,
    pub policy_path: PathBuf,
}

/// 一个被代理应用的运行会话。
pub struct AppSession {
    pub entry_id: String,
    pub entry_name: String,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub process: localproxy_launcher::Session,
    pub started_at: std::time::Instant,
}

impl AppSession {
    pub fn pid(&self) -> u32 {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.process.pid()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            0
        }
    }

    pub fn is_running(&mut self) -> bool {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.process.is_running()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            false
        }
    }

    pub fn stop(&mut self) -> std::io::Result<()> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.process.kill()
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Err(std::io::ErrorKind::Unsupported.into())
        }
    }
}

/// 应用全局状态。
pub struct AppState {
    /// 配置（内存副本；写操作同步落盘）
    pub config: Mutex<AppConfig>,
    /// 配置文件路径
    pub config_path: PathBuf,
    /// 本地核心子进程
    pub core: Mutex<Option<CoreProcess>>,
    /// 被代理应用会话
    pub sessions: Mutex<Vec<AppSession>>,
}

impl AppState {
    /// 构造：加载（或创建默认）配置。
    pub fn new() -> Self {
        let config_path = default_config_path();
        let mut config = AppConfig::load(&config_path).unwrap_or_else(|e| {
            tracing::warn!("加载配置失败，使用默认值: {e}");
            AppConfig::default()
        });
        // 控制面 token：为空则生成并立即落盘（core 启动时读取同一配置）
        let token_was_empty = config.control_token.trim().is_empty();
        config.ensure_control_token();
        if token_was_empty {
            if let Err(e) = config.save(&config_path) {
                tracing::warn!("保存生成的 control_token 失败: {e}");
            }
        }
        Self {
            config: Mutex::new(config),
            config_path,
            core: Mutex::new(None),
            sessions: Mutex::new(Vec::new()),
        }
    }

    /// 持久化当前配置（原子写）。
    pub fn save_config(&self) -> Result<(), String> {
        let config = self.config.lock().map_err(|e| e.to_string())?;
        config.save(&self.config_path)
    }

    /// 先落盘供核心重载，核心确认后更新应用配置。调用方持有配置锁。
    pub fn apply_config(&self, current: &mut AppConfig, mut next: AppConfig) -> Result<(), String> {
        next.sanitize();
        next.ensure_control_token();
        next.validate_desktop()?;
        let mut core = self.core.lock().map_err(|e| e.to_string())?;
        let running = running_core(&mut core).map(|core| &core.startup_config);
        if let Some(running) = running {
            let mut live = next.clone();
            live.use_running_listeners(running);
            live.validate()?;
        }
        next.save(&self.config_path)?;
        if let Some(running) = running {
            let client =
                ControlClient::new(running.control_endpoint(), running.control_token.clone());
            if let Err(e) = client.reload(&next.upstream.active) {
                current.save(&self.config_path).map_err(|rollback| {
                    format!("核心未确认配置: {e}。恢复配置文件失败: {rollback}")
                })?;
                // 超时也可能发生在核心应用配置后，恢复文件后再次同步运行态。
                if let Err(rollback) = client.reload(&current.upstream.active) {
                    return Err(format!(
                        "核心未确认配置: {e}。已恢复配置文件，运行状态请重启核心同步: {rollback}"
                    ));
                }
                return Err(format!("核心未应用配置，已恢复原配置: {e}"));
            }
        }
        *current = next;
        Ok(())
    }
}

/// 平台配置目录下的 config.json（无第三方依赖，读环境变量）。
pub fn default_config_path() -> PathBuf {
    let base = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"))
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.unwrap_or_else(|| PathBuf::from("."))
        .join("local-proxy")
        .join("config.json")
}

/// 安装包与开发构建使用同名核心和 shim。
pub fn core_binary_path() -> PathBuf {
    packaged_or_development(if cfg!(windows) {
        "localproxy-core.exe"
    } else {
        "localproxy-core"
    })
}

pub fn shim_binary_path() -> PathBuf {
    let name = if cfg!(target_os = "macos") {
        "liblocalproxy_shim_macos.dylib"
    } else if cfg!(windows) {
        "localproxy_shim_win.dll"
    } else {
        "localproxy-shim"
    };
    packaged_or_development(name)
}

fn packaged_or_development(name: &str) -> PathBuf {
    let executable = std::env::current_exe().unwrap_or_default();
    let packaged = if cfg!(target_os = "macos") {
        executable
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("../Resources")
            .join(name)
    } else {
        executable.with_file_name(name)
    };
    if packaged.is_file() || !cfg!(debug_assertions) {
        return packaged;
    }
    executable.with_file_name(name)
}

/// 启动本地核心子进程。
pub fn spawn_core(state: &AppState) -> Result<u32, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let mut guard = state.core.lock().map_err(|e| e.to_string())?;
    if let Some(core) = running_core(&mut guard) {
        return Ok(core.child.id());
    }

    let binary = core_binary_path();
    if !binary.exists() {
        return Err(format!("核心二进制不存在: {}", binary.display()));
    }

    let mut command = std::process::Command::new(&binary);
    localproxy_launcher::proxy_env::remove_from(&mut command);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = command
        .arg("--config")
        .arg(&state.config_path)
        .arg("--desktop")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("启动核心失败: {e}"))?;
    let stdout = child.stdout.take().expect("核心 stdout 已配置");
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut line = String::new();
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Startup {
            Ready(ListenerInfo),
            Failed { error: String },
        }
        let info = BufReader::new(stdout)
            .read_line(&mut line)
            .ok()
            .and_then(|_| serde_json::from_str::<Startup>(&line).ok())
            .map(|startup| match startup {
                Startup::Ready(info) => Ok(info),
                Startup::Failed { error } => Err(error),
            });
        let _ = send.send(info);
    });
    let info = match receive.recv_timeout(Duration::from_secs(7)) {
        Ok(Some(Ok(info))) => info,
        Ok(Some(Err(error))) => {
            let _ = stop_child(&mut child);
            return Err(error);
        }
        _ => {
            let _ = stop_child(&mut child);
            return Err("代理核心未能就绪，请检查监听配置、活动节点和内核版本".to_string());
        }
    };
    let Some(policy_path) = info.policy_path.clone() else {
        let _ = stop_child(&mut child);
        return Err("核心未提供运行期策略，请更新核心程序".to_string());
    };
    let mut startup_config = config.clone();
    startup_config.set_listener_info(info);
    let pid = child.id();
    *guard = Some(CoreProcess {
        child,
        started_at: std::time::Instant::now(),
        startup_config,
        policy_path,
    });
    tracing::info!("核心已启动 pid={pid}");
    Ok(pid)
}

/// 停止本地核心。
pub fn kill_core(state: &AppState) -> Result<(), String> {
    let mut guard = state.core.lock().map_err(|e| e.to_string())?;
    if let Some(mut core) = guard.take() {
        let _ = std::fs::remove_file(&core.policy_path);
        stop_child(&mut core.child).map_err(|e| format!("停止核心失败: {e}"))?;
        tracing::info!("核心已停止");
    }
    Ok(())
}

fn stop_child(child: &mut Child) -> std::io::Result<()> {
    // 先关闭父子进程管道，让核心回收独立内核和临时配置。
    drop(child.stdin.take());
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while child.try_wait()?.is_none() {
        if std::time::Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            break;
        }
        std::thread::park_timeout(Duration::from_millis(20));
    }
    Ok(())
}

pub fn running_core(core: &mut Option<CoreProcess>) -> Option<&CoreProcess> {
    let core = core.as_mut()?;
    if matches!(core.child.try_wait(), Ok(None)) {
        Some(core)
    } else {
        let _ = std::fs::remove_file(&core.policy_path);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_path_ends_with_json() {
        let p = default_config_path();
        assert!(p.ends_with("config.json"));
        assert!(p.to_string_lossy().contains("local-proxy"));
    }

    #[test]
    fn core_binary_path_resolves() {
        let p = core_binary_path();
        assert!(p.to_string_lossy().contains("localproxy-core"));
    }
}
