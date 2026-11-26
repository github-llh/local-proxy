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
    has_hardened_runtime, launch, read_plist_string, resolve_binary, LaunchError, ResolvedBinary,
    Session,
};

#[cfg(target_os = "windows")]
pub use windows::{launch, LaunchError, Session};

/// 代理启动所需的实际监听端点与本次核心的运行期策略。
pub struct LaunchConfig<'a> {
    pub core: std::net::SocketAddr,
    pub policy_path: &'a std::path::Path,
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
