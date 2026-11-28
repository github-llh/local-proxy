//! 应用共享状态：配置、核心子进程、被代理应用会话。
//!
//! 并发约定：
//! - 读命令只短暂持有 `config` / `core` / `sessions`，绝不跨 IO 或等待子进程；
//! - 需要核心重载的配置提交统一走 [`AppState::commit_config`]，
//!   只在 `apply_lock` 内串行，网络等待（控制面 reload）不持有配置锁；
//! - 不影响内核的本地写入（应用登记、最近启动时间）走 [`AppState::mutate_local`]，
//!   全程持锁完成「改内存 + 落盘」，避免读改写窗口丢失其他改动；
//! - 应用为单实例：配置目录下的 `app.lock` 由进程持有，第二个实例直接退出。

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use localproxy_core::config::{AppConfig, ListenerInfo};

use crate::control_client::ControlClient;

/// 单实例锁文件名（与 `config.json` 同目录）。
const INSTANCE_LOCK_FILENAME: &str = "app.lock";
/// 停止被代理应用时等待进程退出的上限。
const STOP_GRACE: Duration = Duration::from_secs(2);
/// 单实例锁的取用重试窗口（覆盖退出后立即重启）。
const LOCK_RETRY: Duration = Duration::from_millis(1500);
/// 单实例锁重试间隔。
const LOCK_RETRY_STEP: Duration = Duration::from_millis(100);
/// 配置目录覆盖环境变量（便于多实例/测试）。
const CONFIG_DIR_ENV: &str = "LOCALPROXY_CONFIG_DIR";

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

    /// 终止本次启动的进程组，并在有限时间内等待其退出。
    ///
    /// 调用方不应持有 `sessions` 锁，避免界面读取被等待阻塞。
    pub fn stop(&mut self) -> std::io::Result<()> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.process.kill()?;
            let deadline = std::time::Instant::now() + STOP_GRACE;
            while std::time::Instant::now() < deadline {
                if !self.is_running() {
                    break;
                }
                std::thread::park_timeout(Duration::from_millis(25));
            }
            Ok(())
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
    /// 配置提交串行化：核心重载期间不持有配置锁
    apply_lock: tokio::sync::Mutex<()>,
    /// 核心启动串行化（命令为同步执行，使用 std 锁）
    spawn_lock: Mutex<()>,
    /// 同一上游的连通性测试串行化，避免并发拉起多个独立内核
    probe_keys: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// 单实例文件锁句柄：进程存活期间持有，退出时由系统释放
    _instance_lock: std::fs::File,
}

impl AppState {
    /// 构造：加载（或创建默认）配置。
    pub fn new(instance_lock: std::fs::File) -> Self {
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
            apply_lock: tokio::sync::Mutex::new(()),
            spawn_lock: Mutex::new(()),
            probe_keys: Mutex::new(HashMap::new()),
            _instance_lock: instance_lock,
        }
    }

    /// 本地写入：不影响内核运行，全程持锁完成「改内存 + 落盘」。
    ///
    /// 适用于应用登记、启用状态、最近启动时间等字段；`apps` 由
    /// `save_config` 主动保留，因此无需触发核心重载。
    pub fn mutate_local<T>(
        &self,
        edit: impl FnOnce(&mut AppConfig) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut config = lock_recover(&self.config);
        let value = edit(&mut config)?;
        config.save(&self.config_path)?;
        Ok(value)
    }

    /// 提交需要内核生效的配置。
    ///
    /// `build` 在配置读快照之后、落盘之前执行，因此可以基于最新配置做合并
    /// （例如保留 `apps`、修正激活上游）；提交串行化由 `apply_lock` 保证。
    pub async fn commit_config<T>(
        &self,
        build: impl FnOnce(&AppConfig) -> Result<(AppConfig, T), String>,
    ) -> Result<(AppConfig, T), String> {
        let _apply = self.apply_lock.lock().await;
        let current = lock_recover(&self.config).clone();
        let (next, value) = build(&current)?;
        let applied = self.apply_locked(next).await?;
        Ok((applied, value))
    }

    /// 校验 → 落盘 → 更新内存 → 通知运行中的核心重载。
    ///
    /// 控制面等待发生在配置锁之外；失败时文件与内存同时回滚。
    async fn apply_locked(&self, mut next: AppConfig) -> Result<AppConfig, String> {
        let (previous, running) = {
            let mut current = lock_recover(&self.config);
            let previous = current.clone();
            next.sanitize();
            next.ensure_control_token();
            next.validate_desktop()?;
            let running = {
                let mut core = lock_recover(&self.core);
                running_core(&mut core).map(|core| core.startup_config.clone())
            };
            if let Some(running) = &running {
                let mut live = next.clone();
                live.use_running_listeners(running);
                live.validate()?;
            }
            next.save(&self.config_path)?;
            *current = next.clone();
            (previous, running)
        };

        let Some(running) = running else {
            return Ok(next);
        };
        let client = ControlClient::new(running.control_endpoint(), running.control_token.clone());
        if let Err(error) = client.reload(&next.upstream.active) {
            // 回滚：先恢复文件与内存，再让核心回到旧配置。
            // `apps` 由独立命令维护，回滚时保留期间发生的变化。
            let mut restored = previous.clone();
            restored.apps.clone_from(&lock_recover(&self.config).apps);
            let restore = restored.save(&self.config_path);
            *lock_recover(&self.config) = restored.clone();
            let mut message = format!("核心未应用配置: {error}");
            if let Err(restore) = restore {
                message.push_str(&format!("。恢复配置文件失败: {restore}"));
            }
            match client.reload(&restored.upstream.active) {
                Ok(()) => message.push_str("。已恢复原配置"),
                Err(rollback) => message.push_str(&format!(
                    "。已恢复配置文件，运行状态请重启核心同步: {rollback}"
                )),
            }
            return Err(message);
        }
        Ok(next)
    }

    /// 运行中核心的启动配置快照（实际监听端口、控制面地址与令牌）。
    pub fn running_startup_config(&self) -> Result<AppConfig, String> {
        let mut guard = lock_recover(&self.core);
        running_core(&mut guard)
            .map(|core| core.startup_config.clone())
            .ok_or_else(|| "LocalProxy 核心尚未运行".to_string())
    }

    /// 控制面客户端（核心未运行时返回错误）。
    pub fn control_client(&self) -> Result<ControlClient, String> {
        let config = self.running_startup_config()?;
        Ok(ControlClient::new(
            config.control_endpoint(),
            config.control_token.clone(),
        ))
    }

    /// 取得某个上游的探测锁，同一上游的并发测试合并为串行。
    ///
    /// key 取自配置中的上游 id，数量随上游数有界，无需回收。
    pub fn probe_lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        lock_recover(&self.probe_keys)
            .entry(key.to_string())
            .or_default()
            .clone()
    }
}

/// 取锁并在锁中毒时继续使用内部数据（读命令不应因一次 panic 全线失败）。
pub fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// 单实例锁的取用结果。
enum LockState {
    Acquired(std::fs::File),
    /// 已有实例持有该锁
    Busy,
    Failed(String),
}

/// 获取单实例锁。
///
/// `Ok` 表示当前进程是唯一实例，句柄需随进程存活；`Err` 说明已有实例在运行。
///
/// 退出后立刻重启（旧进程仍在收尾）时锁会短暂不可用，因此在
/// [`LOCK_RETRY`] 时间内重试，避免把「重启」误判成「已有实例」。
pub fn acquire_instance_lock() -> Result<std::fs::File, String> {
    let path = config_directory().join(INSTANCE_LOCK_FILENAME);
    let deadline = std::time::Instant::now() + LOCK_RETRY;
    loop {
        match try_lock_file(&path) {
            LockState::Acquired(file) => return Ok(file),
            LockState::Busy if std::time::Instant::now() < deadline => {
                std::thread::park_timeout(LOCK_RETRY_STEP);
            }
            LockState::Busy => {
                return Err(
                    "LocalProxy 已在运行，请从菜单栏图标打开面板（可用 LOCALPROXY_CONFIG_DIR 指定独立配置目录以并行运行）"
                        .to_string(),
                )
            }
            LockState::Failed(message) => return Err(message),
        }
    }
}

/// 对指定路径取独占锁。
fn try_lock_file(path: &std::path::Path) -> LockState {
    if let Some(parent) = path.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            return LockState::Failed(format!("创建配置目录失败: {error}"));
        }
    }
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
    {
        Ok(file) => file,
        Err(error) => {
            return LockState::Failed(format!("打开单实例锁失败 ({}): {error}", path.display()))
        }
    };
    match file.try_lock() {
        Ok(()) => LockState::Acquired(file),
        Err(std::fs::TryLockError::WouldBlock) => LockState::Busy,
        Err(std::fs::TryLockError::Error(error)) => {
            LockState::Failed(format!("单实例锁不可用: {error}"))
        }
    }
}

/// 配置目录：默认平台配置目录，可用 `LOCALPROXY_CONFIG_DIR` 覆盖。
///
/// 覆盖后可并行运行多个实例（各自独立配置与单实例锁），
/// 便于测试、多账号或与默认安装共存。
pub fn config_directory() -> PathBuf {
    config_directory_from(std::env::var_os(CONFIG_DIR_ENV))
}

/// 覆盖值优先；未设置或为空时回退平台配置目录。
fn config_directory_from(override_dir: Option<std::ffi::OsString>) -> PathBuf {
    if let Some(directory) = override_dir {
        if !directory.is_empty() {
            return PathBuf::from(directory);
        }
    }
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
}

/// 平台配置目录下的 config.json。
pub fn default_config_path() -> PathBuf {
    config_directory().join("config.json")
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
///
/// 启动串行化，且等待核心首行期间不持有配置锁与核心锁，
/// 避免菜单栏轮询在启动窗口内被阻塞。
pub fn spawn_core(state: &AppState) -> Result<u32, String> {
    let _spawn = lock_recover(&state.spawn_lock);
    {
        let mut guard = lock_recover(&state.core);
        if let Some(core) = running_core(&mut guard) {
            return Ok(core.child.id());
        }
    }

    let binary = core_binary_path();
    if !binary.exists() {
        return Err(format!("核心二进制不存在: {}", binary.display()));
    }
    let config_snapshot = lock_recover(&state.config).clone();

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
    let mut startup_config = config_snapshot;
    startup_config.set_listener_info(info);
    let pid = child.id();

    let mut guard = lock_recover(&state.core);
    if let Some(existing) = running_core(&mut guard) {
        // 并发路径已启动核心：回收本次启动的进程，复用既有句柄。
        let _ = stop_child(&mut child);
        let _ = std::fs::remove_file(&policy_path);
        return Ok(existing.child.id());
    }
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
///
/// 先摘出句柄再等待子进程退出：等待期间菜单栏轮询看到的是「已停止」，
/// 不会被 2 秒的收尾过程阻塞。
pub fn kill_core(state: &AppState) -> Result<(), String> {
    let taken = lock_recover(&state.core).take();
    let Some(mut core) = taken else {
        return Ok(());
    };
    let _ = std::fs::remove_file(&core.policy_path);
    stop_child(&mut core.child).map_err(|e| format!("停止核心失败: {e}"))?;
    tracing::info!("核心已停止");
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

    #[test]
    fn instance_lock_rejects_second_holder() {
        let directory = std::env::temp_dir().join(format!(
            "localproxy-lock-{}",
            localproxy_core::config::generate_token()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(INSTANCE_LOCK_FILENAME);

        let first = match try_lock_file(&path) {
            LockState::Acquired(file) => file,
            LockState::Busy => panic!("首个实例不应被判定为占用"),
            LockState::Failed(message) => panic!("首个实例取锁失败: {message}"),
        };
        // 同一文件的第二个持有者在 unix 文件锁语义下被拒绝；
        // Windows 的字节范围锁语义由系统决定，这里不做断言。
        let second = try_lock_file(&path);
        #[cfg(unix)]
        assert!(
            matches!(second, LockState::Busy),
            "第二个实例应被判定为占用"
        );
        #[cfg(not(unix))]
        drop(second);

        // 其它路径互不影响（LOCALPROXY_CONFIG_DIR 可让多实例各用一份配置）
        let other = directory.join("other.lock");
        assert!(
            matches!(try_lock_file(&other), LockState::Acquired(_)),
            "其它路径应可加锁"
        );

        drop(first);
        std::fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn config_directory_honours_override() {
        // 纯函数形式便于断言，避免测试之间争用进程环境变量
        assert_eq!(
            config_directory_from(Some("/tmp/localproxy-override".into())),
            PathBuf::from("/tmp/localproxy-override")
        );
        assert!(config_directory_from(None).ends_with("local-proxy"));
        assert_eq!(
            config_directory_from(Some(std::ffi::OsString::new())),
            config_directory_from(None),
            "空覆盖值应回退默认目录"
        );
    }
}
