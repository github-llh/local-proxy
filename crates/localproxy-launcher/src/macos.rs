//! macOS 平台启动器。
//!
//! - `.app` bundle 解析：读 `Contents/Info.plist` 取 `CFBundleExecutable`，
//!   直接 exec `Contents/MacOS/<binary>`（**不能用 `open`**：`open` 会清理
//!   `DYLD_INSERT_LIBRARIES` 环境变量，导致注入失效）；
//! - hardened runtime 检测：被注入 app 若启用 hardened runtime，dyld 会拒绝
//!   加载库（launcher 提前检测并给出明确错误）；
//! - 子进程经环境变量自动继承注入（无需额外处理）。

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
    /// hardened runtime 应用无法注入
    HardenedRuntime(String),
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
                write!(f, "{p} 启用了 hardened runtime，无法注入 shim。请使用支持 --proxy-server 的启动参数，或对该应用关闭 hardened runtime 后重试")
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

/// 检测 Mach-O 是否启用 hardened runtime（`codesign --display` 解析 flags）。
///
/// 返回 `Ok(true)` 表示启用了 hardened runtime（不可注入）。
/// `codesign` 不可用或解析失败时返回 `Ok(false)`（不阻塞启动，由 dyld 最终裁决）。
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
    // flags 行形如: flags=0x10000(runtime)
    Ok(text.contains("runtime"))
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

/// 以代理模式启动应用（注入 shim dylib）。
///
/// - `shim_path`：shim dylib 绝对路径（`DYLD_INSERT_LIBRARIES`）
/// - `core_host` / `core_port` / `fakeip_cidr`：shim 环境变量
/// - `udp_mode` / `ipv6_mode`：安全策略（由 core 配置解析后注入，缺省 block/proxy）
pub fn launch(
    entry: &AppEntry,
    shim_path: &Path,
    core_host: &str,
    core_port: u16,
    fakeip_cidr: &str,
    udp_mode: &str,
    ipv6_mode: &str,
) -> Result<Session, LaunchError> {
    let resolved = resolve_binary(&entry.path_buf())?;

    // hardened runtime 预检（bundle 根或二进制本身）
    let check_target = resolved
        .bundle_root
        .as_deref()
        .unwrap_or(&resolved.executable);
    match has_hardened_runtime(check_target) {
        Ok(true) => {
            return Err(LaunchError::HardenedRuntime(
                check_target.display().to_string(),
            ));
        }
        Ok(false) => {}
        Err(e) => {
            tracing::warn!("hardened runtime 检测失败（继续启动）: {e}");
        }
    }

    let mut command = Command::new(&resolved.executable);
    // 注入环境（绝对路径；DYLD_INSERT_LIBRARIES 对相对路径不可靠）
    command.env("DYLD_INSERT_LIBRARIES", shim_path);
    command.env("LP_CORE_HOST", core_host);
    command.env("LP_CORE_PORT", core_port.to_string());
    command.env("LP_FAKEIP_CIDR", fakeip_cidr);
    command.env("LP_MODE", "proxy");
    command.env("LP_UDP_MODE", udp_mode);
    command.env("LP_IPV6_MODE", ipv6_mode);
    // 清理系统代理环境变量：shim 全权接管，避免与 Clash 等代理软件双跳
    for var in PROXY_ENV_VARS {
        command.env_remove(var);
    }
    // bundle 内应用的工作目录设为 bundle 根（资源加载兼容）
    if let Some(root) = &resolved.bundle_root {
        command.current_dir(root);
    }
    for arg in &entry.extra_args {
        command.arg(arg);
    }

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
    })
}

impl Session {
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
        self.child.kill()
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
}
