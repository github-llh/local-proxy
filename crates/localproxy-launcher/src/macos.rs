//! macOS 平台启动器。
//!
//! - `.app` bundle 解析：读 `Contents/Info.plist` 取 `CFBundleExecutable`，
//!   直接 exec `Contents/MacOS/<binary>`（**不能用 `open`**：`open` 会清理
//!   `DYLD_INSERT_LIBRARIES` 环境变量，导致注入失效）；
//! - hardened runtime 检测：被注入 app 若启用 hardened runtime，dyld 会拒绝
//!   加载库（launcher 提前检测并给出明确错误）；
//! - 子进程经环境变量自动继承注入（无需额外处理）。

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::app_entry::AppEntry;

/// 解析后的可执行目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBinary {
    /// 实际 exec 的 Mach-O 路径
    pub executable: PathBuf,
    /// bundle 根（`.app` 时为 bundle 路径；裸二进制为 None）
    pub bundle_root: Option<PathBuf>,
}

/// 启动一个被代理应用的会话。
pub struct Session {
    pub child: Child,
    pub entry_id: String,
    /// 本次启动是注入还是原生代理（供 UI 如实展示）
    pub mode: crate::LaunchMode,
}

/// launcher 错误。
#[derive(Debug)]
pub enum LaunchError {
    /// 路径不存在
    NotFound(String),
    /// `.app` 缺少 Info.plist 或 CFBundleExecutable
    InvalidBundle(String),
    /// 可执行文件不存在
    MissingExecutable(String),
    /// hardened runtime 应用无法注入，且不支持原生代理参数
    HardenedRuntime(String),
    /// 注入模式缺少 shim
    MissingShim(String),
    /// 原生代理实例已在运行（独立配置目录被占用）
    ProfileInUse(String),
    /// SIP 保护的系统二进制：注入会被静默忽略
    SipProtected(String),
    /// 启动失败
    SpawnFailed(String),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LaunchError::NotFound(p) => write!(f, "应用不存在: {p}"),
            LaunchError::InvalidBundle(p) => write!(f, "无效的 .app bundle: {p}"),
            LaunchError::MissingExecutable(p) => write!(f, "可执行文件不存在: {p}"),
            LaunchError::HardenedRuntime(p) => {
                write!(
                    f,
                    "{p} 启用了 hardened runtime，dyld 会拒绝加载注入库，且它不在自动原生代理（Chromium 系）的支持范围内。可改用 Chrome/Edge/Brave 等 Chromium 浏览器（LocalProxy 会自动用 --proxy-server 启动），或改用系统代理/TUN 代理它"
                )
            }
            LaunchError::SipProtected(p) => {
                write!(
                    f,
                    "{p} 是受 SIP 保护的系统二进制，dyld 会静默忽略注入（进程能启动但不会走代理）。请改用 Homebrew/nvm 等非系统安装的版本"
                )
            }
            LaunchError::MissingShim(p) => {
                write!(
                    f,
                    "找不到代理 shim: {p}（注入模式必需；原生代理模式不需要）"
                )
            }
            LaunchError::ProfileInUse(p) => {
                write!(
                    f,
                    "该应用已有一个原生代理实例在运行（独立配置目录 {p} 被占用）。请先在 LocalProxy 中停止它，再重新启动"
                )
            }
            LaunchError::SpawnFailed(msg) => write!(f, "启动失败: {msg}"),
        }
    }
}

impl std::error::Error for LaunchError {}

/// 解析应用路径为可执行的 Mach-O 二进制。
pub fn resolve_binary(app_path: &Path) -> Result<ResolvedBinary, LaunchError> {
    if !app_path.exists() {
        return Err(LaunchError::NotFound(app_path.display().to_string()));
    }

    // .app bundle：读 Info.plist 取 CFBundleExecutable
    let is_bundle = app_path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("app"))
        .unwrap_or(false);

    if is_bundle {
        let plist_path = app_path.join("Contents/Info.plist");
        if !plist_path.exists() {
            return Err(LaunchError::InvalidBundle(format!(
                "缺少 {}",
                plist_path.display()
            )));
        }
        let executable_name =
            read_plist_string(&plist_path, "CFBundleExecutable").ok_or_else(|| {
                LaunchError::InvalidBundle(format!(
                    "{} 中缺少 CFBundleExecutable",
                    plist_path.display()
                ))
            })?;
        let executable = app_path.join("Contents/MacOS").join(&executable_name);
        if !executable.exists() {
            return Err(LaunchError::MissingExecutable(
                executable.display().to_string(),
            ));
        }
        return Ok(ResolvedBinary {
            executable,
            bundle_root: Some(app_path.to_path_buf()),
        });
    }

    // 裸二进制（CLI 工具等）
    if !app_path.is_file() {
        return Err(LaunchError::MissingExecutable(
            app_path.display().to_string(),
        ));
    }
    Ok(ResolvedBinary {
        executable: app_path.to_path_buf(),
        bundle_root: None,
    })
}

/// 从 Info.plist 提取字符串值（简易 XML 文本解析，无第三方依赖）。
///
/// 匹配 `<key>NAME</key>` 之后最近的 `<string>VALUE</string>`。
pub fn read_plist_string(plist_path: &Path, key: &str) -> Option<String> {
    let text = std::fs::read_to_string(plist_path).ok()?;
    let key_pattern = format!("<key>{key}</key>");
    let key_pos = text.find(&key_pattern)?;
    let after_key = &text[key_pos + key_pattern.len()..];
    let string_start = after_key.find("<string>")? + "<string>".len();
    let rest = &after_key[string_start..];
    let string_end = rest.find("</string>")?;
    let value = rest[..string_end].trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// 检测 Mach-O 是否启用 hardened runtime。
///
/// 只看 `codesign --display --verbose` 里 `flags=0x...` 的 `CS_RUNTIME`（0x10000）位。
/// 旧实现是 `text.contains("runtime")`：把任何含 "runtime" 的行都当成命中，
/// 既可能误判（把可注入的应用挡在门外），也没法解释判断依据。
pub fn has_hardened_runtime(binary: &Path) -> Result<bool, LaunchError> {
    let output = Command::new("codesign")
        .args(["--display", "--verbose"])
        .arg(binary)
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .map_err(|e| LaunchError::SpawnFailed(format!("codesign 执行失败: {e}")))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(parse_runtime_flag(&text))
}

/// 从 `codesign` 输出里解析 `CS_RUNTIME` 位（纯函数，可单测）。
pub fn parse_runtime_flag(text: &str) -> bool {
    const CS_RUNTIME: u64 = 0x1_0000;
    for line in text.lines() {
        let Some(index) = line.find("flags=") else {
            continue;
        };
        let rest = &line[index + "flags=".len()..];
        let rest = rest
            .strip_prefix("0x")
            .or_else(|| rest.strip_prefix("0X"))
            .unwrap_or(rest);
        let hex: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        if hex.is_empty() {
            continue;
        }
        if let Ok(value) = u64::from_str_radix(&hex, 16) {
            return value & CS_RUNTIME != 0;
        }
    }
    false
}

/// 原生代理模式的命令行参数（纯函数，可单测）。
///
/// `--disable-quic` 不是性能调优：`--proxy-server` 只接管 HTTP(S)/CONNECT，
/// QUIC 走 UDP，不关掉就会出现"网页走了代理、视频/部分请求漏直连"。
pub fn native_proxy_args(endpoint: std::net::SocketAddr, profile: &Path) -> Vec<String> {
    vec![
        format!("--proxy-server=http://{endpoint}"),
        format!("--user-data-dir={}", profile.display()),
        "--disable-quic".to_string(),
        "--no-first-run".to_string(),
    ]
}

/// 受 SIP 保护的系统二进制。
///
/// 这类进程照常启动、却会忽略 `DYLD_INSERT_LIBRARIES` —— 用户看到"启动成功"、
/// 实际一点流量都没被代理。比直接报错更难排查，所以在启动前拒绝。
///
/// 只按路径前缀判断（覆盖文档承诺的 `/usr/bin` 等）；`/Applications` 里的 Apple
/// 应用（Safari 等）不在此列，它们本就不是代理目标。
pub fn is_sip_protected(binary: &Path) -> bool {
    const PREFIXES: &[&str] = &["/usr/bin/", "/bin/", "/sbin/", "/usr/sbin/", "/System/"];
    let path = binary.to_string_lossy();
    PREFIXES.iter().any(|prefix| path.starts_with(prefix))
}

/// 该应用是否属于会自动走 `--proxy-server` 的 Chromium 家族。
///
/// 只认 `CFBundleIdentifier` 精确匹配：Electron 应用大多也接受 `--proxy-server`，
/// 但支持程度不一，不能拿"看起来像 Chromium"当依据。
pub fn native_proxy_supported(resolved: &ResolvedBinary) -> bool {
    const CHROMIUM_BUNDLE_IDS: &[&str] = &[
        "com.google.Chrome",
        "com.google.Chrome.beta",
        "com.google.Chrome.dev",
        "com.google.Chrome.canary",
        "org.chromium.Chromium",
        "com.microsoft.edgemac",
        "com.microsoft.edgemac.Beta",
        "com.microsoft.edgemac.Dev",
        "com.brave.Browser",
        "com.brave.Browser.beta",
        "com.vivaldi.Vivaldi",
        "com.operasoftware.Opera",
        "com.operasoftware.OperaGX",
    ];
    let Some(bundle) = &resolved.bundle_root else {
        return false;
    };
    let plist = bundle.join("Contents/Info.plist");
    let Some(id) = read_plist_string(&plist, "CFBundleIdentifier") else {
        return false;
    };
    CHROMIUM_BUNDLE_IDS.iter().any(|known| id == *known)
}

/// 启动器不得用应用自身的默认 profile：Chrome 已有实例时 `--proxy-server` 会被
/// 静默忽略（新命令只是给现有进程开个窗口），用户会以为在走代理而实际没有。
/// 每个应用一个独立目录，既不干扰日常浏览器，也让"是否已在运行"可判定。
fn profile_in_use(dir: &Path) -> bool {
    let lock = dir.join("SingletonLock");
    let Ok(target) = std::fs::read_link(&lock) else {
        return false;
    };
    // Chrome 的 SingletonLock 指向 "<hostname>-<pid>"
    let name = target.to_string_lossy();
    let pid: Option<i32> = name.rsplit('-').next().and_then(|p| p.parse().ok());
    match pid {
        // 加括号：`unsafe {}` 紧跟 `==` 在 match arm 里会被当成语句边界
        Some(pid) if pid > 0 => (unsafe { libc::kill(pid, 0) }) == 0,
        _ => false,
    }
}

fn sanitize_profile_name(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect();
    if cleaned.is_empty() {
        "default".to_string()
    } else {
        cleaned
    }
}

/// 以代理模式启动应用。
pub fn launch(
    entry: &AppEntry,
    shim_path: &Path,
    config: &crate::LaunchConfig<'_>,
) -> Result<Session, LaunchError> {
    let resolved = resolve_binary(&entry.path_buf())?;

    // hardened runtime 预检（bundle 根或二进制本身）
    let check_target = resolved
        .bundle_root
        .as_deref()
        .unwrap_or(&resolved.executable);
    let hardened = match has_hardened_runtime(check_target) {
        Ok(flag) => flag,
        Err(e) => {
            tracing::warn!("hardened runtime 检测失败（按未启用处理）: {e}");
            false
        }
    };
    if hardened {
        if native_proxy_supported(&resolved) {
            return launch_native_proxy(entry, &resolved, config);
        }
        return Err(LaunchError::HardenedRuntime(
            check_target.display().to_string(),
        ));
    }

    if is_sip_protected(&resolved.executable) {
        return Err(LaunchError::SipProtected(
            resolved.executable.display().to_string(),
        ));
    }

    if !shim_path.exists() {
        return Err(LaunchError::MissingShim(shim_path.display().to_string()));
    }

    let mut command = Command::new(&resolved.executable);
    // 注入环境（绝对路径；DYLD_INSERT_LIBRARIES 对相对路径不可靠）
    command.env("DYLD_INSERT_LIBRARIES", shim_path);
    command.envs(config.environment());
    // 清理系统代理环境变量：shim 全权接管，避免与 Clash 等代理软件双跳
    crate::proxy_env::remove_from(&mut command);
    // bundle 内应用的工作目录设为 bundle 根（资源加载兼容）
    if let Some(root) = &resolved.bundle_root {
        command.current_dir(root);
    }
    for arg in &entry.extra_args {
        command.arg(arg);
    }

    // 独立进程组：停止时可整组终止（.app 主进程退出时子进程不残留）
    command.process_group(0);
    let child = command
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| LaunchError::SpawnFailed(e.to_string()))?;

    tracing::info!(
        "launcher: 代理启动 {} (pid={})",
        resolved.executable.display(),
        child.id()
    );
    Ok(Session {
        child,
        entry_id: entry.id.clone(),
        mode: crate::LaunchMode::Injected,
    })
}

/// 原生代理启动：不注入，直接给应用传 `--proxy-server`。
///
/// 约束：
/// - 端点必须是回环地址（否则等于把本地流量交给远端）；
/// - 必须使用独立 `--user-data-dir`，否则已有实例会复用旧进程、静默忽略参数；
/// - 同一配置目录被占用时明确报错，而不是"启动成功但没走代理"。
fn launch_native_proxy(
    entry: &AppEntry,
    resolved: &ResolvedBinary,
    config: &crate::LaunchConfig<'_>,
) -> Result<Session, LaunchError> {
    let endpoint = config.http;
    if !endpoint.ip().is_loopback() {
        return Err(LaunchError::SpawnFailed(format!(
            "原生代理端点必须是回环地址: {endpoint}"
        )));
    }
    let profile = config.profile_root.join(sanitize_profile_name(&entry.id));
    if profile_in_use(&profile) {
        return Err(LaunchError::ProfileInUse(profile.display().to_string()));
    }
    std::fs::create_dir_all(&profile)
        .map_err(|e| LaunchError::SpawnFailed(format!("创建原生代理配置目录失败: {e}")))?;

    let mut command = Command::new(&resolved.executable);
    for arg in native_proxy_args(endpoint, &profile) {
        command.arg(arg);
    }
    // 用户额外参数放在后面：Chromium 以最后一个同名参数为准，这样用户能覆盖
    for arg in &entry.extra_args {
        command.arg(arg);
    }
    if let Some(root) = &resolved.bundle_root {
        command.current_dir(root);
    }
    command.process_group(0);
    let child = command
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| LaunchError::SpawnFailed(e.to_string()))?;

    tracing::info!(
        "launcher: 原生代理启动 {} (pid={}, proxy={}, profile={})",
        resolved.executable.display(),
        child.id(),
        endpoint,
        profile.display()
    );
    Ok(Session {
        child,
        entry_id: entry.id.clone(),
        mode: crate::LaunchMode::NativeProxy,
    })
}

impl Session {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// 进程是否仍在运行。
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// 等待进程退出，返回退出码。
    pub fn wait(&mut self) -> std::io::Result<Option<i32>> {
        Ok(self.child.wait()?.code())
    }

    /// 终止进程。
    pub fn kill(&mut self) -> std::io::Result<()> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }
        // 组长仍由 Child 持有且未被回收，组号不会被其他应用复用。
        let result = unsafe { libc::killpg(self.child.id() as libc::pid_t, libc::SIGTERM) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn plist_string_extraction() {
        let dir = std::env::temp_dir().join(format!("lp-plist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let plist = dir.join("Info.plist");
        let mut f = std::fs::File::create(&plist).unwrap();
        write!(
            f,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>TestApp</string>
    <key>CFBundleExecutable</key>
    <string>testapp</string>
    <key>CFBundleIdentifier</key>
    <string>com.test.app</string>
</dict>
</plist>"#
        )
        .unwrap();
        assert_eq!(
            read_plist_string(&plist, "CFBundleExecutable"),
            Some("testapp".to_string())
        );
        assert_eq!(
            read_plist_string(&plist, "CFBundleName"),
            Some("TestApp".to_string())
        );
        assert_eq!(read_plist_string(&plist, "Missing"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_nonexistent_app_errors() {
        let err = resolve_binary(Path::new("/nonexistent/App.app")).unwrap_err();
        assert!(matches!(err, LaunchError::NotFound(_)));
        assert!(err.to_string().contains("不存在"));
    }

    #[test]
    fn resolve_plain_binary() {
        // /bin/echo 是裸 Mach-O
        let resolved = resolve_binary(Path::new("/bin/echo")).unwrap();
        assert_eq!(resolved.executable, PathBuf::from("/bin/echo"));
        assert!(resolved.bundle_root.is_none());
    }

    #[test]
    fn resolve_app_bundle_without_plist_errors() {
        let dir = std::env::temp_dir().join(format!("lp-app-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Fake.app")).unwrap();
        let err = resolve_binary(&dir.join("Fake.app")).unwrap_err();
        assert!(matches!(err, LaunchError::InvalidBundle(_)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn builds_native_proxy_args() {
        let args = native_proxy_args(
            "127.0.0.1:1081".parse().unwrap(),
            Path::new("/tmp/lp-profile"),
        );
        assert_eq!(args[0], "--proxy-server=http://127.0.0.1:1081");
        assert_eq!(args[1], "--user-data-dir=/tmp/lp-profile");
        assert!(args.iter().any(|a| a == "--disable-quic"));
    }

    #[test]
    fn detects_sip_protected_paths() {
        assert!(is_sip_protected(Path::new("/usr/bin/curl")));
        assert!(is_sip_protected(Path::new(
            "/System/Library/CoreServices/Finder.app"
        )));
        assert!(!is_sip_protected(Path::new("/opt/homebrew/bin/curl")));
        assert!(!is_sip_protected(Path::new(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
        )));
    }

    /// 本机装了 Chrome 就验证真实签名与 bundle id（CI 没装则跳过）。
    #[test]
    fn detects_real_chrome_when_installed() {
        let path = Path::new("/Applications/Google Chrome.app");
        if !path.exists() {
            return;
        }
        let resolved = resolve_binary(path).unwrap();
        assert!(
            native_proxy_supported(&resolved),
            "Chrome 应被识别为 Chromium 家族（原生代理可用）"
        );
        assert!(
            has_hardened_runtime(path).unwrap(),
            "Chrome 应启用 hardened runtime（必须走原生代理而不是注入）"
        );
    }

    #[test]
    fn parses_codesign_runtime_flag() {
        assert!(parse_runtime_flag(
            "Executable=/Applications/X.app/Contents/MacOS/X\nCodeDirectory v=20400 size=1 flags=0x10000(runtime) hashes=1+4"
        ));
        assert!(parse_runtime_flag(
            "flags=0x10002(library-validation,runtime)"
        ));
        assert!(!parse_runtime_flag("flags=0x0(none)"));
        // 只有 "Runtime Version" 之类、没有 flags= 行时不能误判
        assert!(!parse_runtime_flag("Runtime Version=14.0.0"));
        assert!(!parse_runtime_flag(""));
    }

    #[test]
    fn detects_chromium_bundles_for_native_proxy() {
        let dir = std::env::temp_dir().join(format!("lp-native-{}", std::process::id()));
        let bundle = dir.join("Fake.app");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        std::fs::write(bundle.join("Contents/MacOS/fake"), b"x").unwrap();
        std::fs::write(
            bundle.join("Contents/Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
    <key>CFBundleExecutable</key><string>fake</string>
    <key>CFBundleIdentifier</key><string>com.google.Chrome</string>
</dict></plist>"#,
        )
        .unwrap();
        let resolved = resolve_binary(&bundle).unwrap();
        assert!(native_proxy_supported(&resolved));

        std::fs::write(
            bundle.join("Contents/Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
    <key>CFBundleExecutable</key><string>fake</string>
    <key>CFBundleIdentifier</key><string>com.example.ElectronApp</string>
</dict></plist>"#,
        )
        .unwrap();
        assert!(!native_proxy_supported(&resolved));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn profile_lock_detects_live_and_dead_owner() {
        let dir = std::env::temp_dir().join(format!("lp-profile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!profile_in_use(&dir));

        // 指向当前进程 → 视为占用
        let live = dir.join("SingletonLock");
        std::os::unix::fs::symlink(format!("anyhost-{}", std::process::id()), &live).unwrap();
        assert!(profile_in_use(&dir));
        std::fs::remove_file(&live).unwrap();

        // 指向不存在的 pid → 视为未占用（可以抢占）
        std::os::unix::fs::symlink("anyhost-999999", &live).unwrap();
        assert!(!profile_in_use(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stop_only_signals_unreaped_session_group() {
        let mut finished = Command::new("/usr/bin/true")
            .process_group(0)
            .spawn()
            .unwrap();
        finished.wait().unwrap();
        let mut same_scope = Session {
            child: finished,
            entry_id: "finished".to_string(),
            mode: crate::LaunchMode::Injected,
        };
        let mut other = Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let result = same_scope.kill();
        let other_alive = matches!(other.try_wait(), Ok(None));
        other.kill().unwrap();
        other.wait().unwrap();
        assert!(result.is_ok());
        assert!(other_alive);
    }
}
