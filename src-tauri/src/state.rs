//! 应用共享状态：配置、核心子进程、被代理应用会话。

use std::path::PathBuf;
use std::process::Child;
use std::sync::Mutex;

use localproxy_core::config::AppConfig;

/// 核心进程句柄。
pub struct CoreProcess {
    pub child: Child,
    pub started_at: std::time::Instant,
}

/// 一个被代理应用的运行会话。
pub struct AppSession {
    pub entry_id: String,
    pub entry_name: String,
    pub pid: u32,
    pub started_at: std::time::Instant,
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

/// 本地核心二进制路径。
///
/// - dev（debug_assertions）：workspace target/debug/localproxy-core
/// - release：与 App 可执行文件同目录（打包时 sidecar 放置处）
pub fn core_binary_path() -> PathBuf {
    if cfg!(debug_assertions) {
        // 仓库根/target/debug/localproxy-core
        let exe = std::env::current_exe().unwrap_or_default();
        // target/debug/localproxy-app → 上三级为仓库根
        let root = exe
            .ancestors()
            .nth(3)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        root.join("target/debug/localproxy-core")
    } else {
        let exe = std::env::current_exe().unwrap_or_default();
        exe.with_file_name("localproxy-core")
    }
}

/// shim 二进制路径（macOS：target/debug 或 App Resources；Windows：App 同目录）。
pub fn shim_binary_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    if cfg!(target_os = "macos") {
        if cfg!(debug_assertions) {
            // 仓库根/target/debug/liblocalproxy_shim_macos.dylib
            let root = exe
                .ancestors()
                .nth(3)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            root.join("target/debug/liblocalproxy_shim_macos.dylib")
        } else {
            // App.app/Contents/MacOS/app → ../Resources
            exe.parent()
                .map(|p| p.join("../Resources/liblocalproxy_shim_macos.dylib"))
                .unwrap_or_default()
        }
    } else if cfg!(target_os = "windows") {
        if cfg!(debug_assertions) {
            let root = exe
                .ancestors()
                .nth(3)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            root.join("target/debug/localproxy_shim_win.dll")
        } else {
            // 与 App 可执行同目录（打包 sidecar 放置处）
            exe.with_file_name("localproxy_shim_win.dll")
        }
    } else {
        PathBuf::from("localproxy-shim")
    }
}

/// 启动本地核心子进程。
pub fn spawn_core(state: &AppState) -> Result<u32, String> {
    let mut guard = state.core.lock().map_err(|e| e.to_string())?;
    if let Some(core) = guard.as_mut() {
        // 已启动：检查存活
        if matches!(core.child.try_wait(), Ok(None)) {
            return Ok(core.child.id());
        }
    }

    let binary = core_binary_path();
    if !binary.exists() {
        return Err(format!("核心二进制不存在: {}", binary.display()));
    }

    let child = std::process::Command::new(&binary)
        .arg("--config")
        .arg(&state.config_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("启动核心失败: {e}"))?;
    let pid = child.id();
    *guard = Some(CoreProcess {
        child,
        started_at: std::time::Instant::now(),
    });
    tracing::info!("核心已启动 pid={pid}");
    Ok(pid)
}

/// 停止本地核心。
pub fn kill_core(state: &AppState) -> Result<(), String> {
    let mut guard = state.core.lock().map_err(|e| e.to_string())?;
    if let Some(mut core) = guard.take() {
        core.child
            .kill()
            .map_err(|e| format!("停止核心失败: {e}"))?;
        tracing::info!("核心已停止");
    }
    Ok(())
}

/// 核心是否在运行。
pub fn core_running(state: &AppState) -> bool {
    let mut guard = match state.core.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    match guard.as_mut() {
        Some(core) => matches!(core.child.try_wait(), Ok(None)),
        None => false,
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
