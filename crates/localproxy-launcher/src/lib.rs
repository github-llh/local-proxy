//! LocalProxy 平台启动器。
//!
//! 负责以「代理模式」启动用户登记的应用：
//! - macOS：解析 `.app` 主二进制，`posix_spawn` + `DYLD_INSERT_LIBRARIES` 注入 shim dylib
//! - Windows：`CreateProcessW(CREATE_SUSPENDED)` + 远程线程注入 shim DLL（两步初始化）

pub mod app_entry;
pub mod proxy_env;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "macos")]
pub use macos::{
    has_hardened_runtime, launch, native_proxy_supported, parse_runtime_flag, read_plist_string,
    resolve_binary, LaunchError, ResolvedBinary, Session,
};

#[cfg(target_os = "windows")]
pub use windows::{launch, LaunchError, Session};

/// 本次启动采用的方式。
///
/// - `Injected`：注入 shim，覆盖 getaddrinfo/connect，支持 FakeIP 与 UDP 语义；
/// - `NativeProxy`：目标应用自己支持 `--proxy-server`（Chromium 系），不注入，
///   用独立配置目录保证新实例真正带上代理参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchMode {
    Injected,
    NativeProxy,
}

impl LaunchMode {
    pub fn as_str(self) -> &'static str {
        match self {
            LaunchMode::Injected => "injected",
            LaunchMode::NativeProxy => "native-proxy",
        }
    }
}

/// 代理启动所需的实际监听端点与本次核心的运行期策略。
pub struct LaunchConfig<'a> {
    /// 核心 SOCKS5 端点（注入模式的 LP_CORE_*）
    pub core: std::net::SocketAddr,
    /// 核心 HTTP 端点（原生代理模式的 `--proxy-server`，必须回环）
    pub http: std::net::SocketAddr,
    pub policy_path: &'a std::path::Path,
    /// 原生代理模式的独立配置目录根（每个应用一个子目录）
    pub profile_root: &'a std::path::Path,
}

impl LaunchConfig<'_> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn environment(&self) -> [(&'static str, String); 5] {
        use localproxy_net::{fakeip::FakeIp, policy::POLICY_PATH_ENV};

        [
            ("LP_CORE_HOST", self.core.ip().to_string()),
            ("LP_CORE_PORT", self.core.port().to_string()),
            ("LP_FAKEIP_CIDR", FakeIp::DEFAULT_CIDR.to_string()),
            ("LP_MODE", "proxy".to_string()),
            (
                POLICY_PATH_ENV,
                self.policy_path.to_string_lossy().into_owned(),
            ),
        ]
    }
}
