//! shim 引擎：FakeIP 映射 + SOCKS5 拨号 + 重入防护。
//!
//! 平台 hook 层（macOS interpose / Windows MinHook）只负责拦截，
//! 所有决策逻辑收敛到这里，保证两平台行为一致。
//!
//! M2 将填充：`connect` 拦截、`getaddrinfo` 拦截、UDP 拦截。

use crate::config::ShimConfig;
use localproxy_net::fakeip::FakeIp;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

/// 全局配置（首次 install 后不变）。
static CONFIG: OnceLock<ShimConfig> = OnceLock::new();

/// 引擎是否已安装。
static INSTALLED: AtomicBool = AtomicBool::new(false);

// 重入防护：hook 函数内部若再次触发网络调用，直接放行原始调用。
thread_local! {
    static IN_HOOK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 安装引擎（幂等）。返回 false 表示已安装或配置非法。
pub fn install(cfg: &ShimConfig) -> bool {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return false;
    }
    if FakeIp::new(&cfg.fakeip_cidr).is_err() {
        tracing::error!("shim: FakeIP CIDR 非法: {}", cfg.fakeip_cidr);
        INSTALLED.store(false, Ordering::SeqCst);
        return false;
    }
    let _ = CONFIG.set(cfg.clone());
    tracing::info!(
        "shim: 引擎已安装, core={}, mode={:?}",
        cfg.core_endpoint(),
        cfg.mode
    );
    true
}

/// 当前配置（未安装时为默认值）。
pub fn config() -> &'static ShimConfig {
    CONFIG.get_or_init(ShimConfig::default)
}

/// 取当前配置的 FakeIP 分配器。
pub fn fakeip() -> &'static FakeIp {
    static FAKEIP: OnceLock<FakeIp> = OnceLock::new();
    FAKEIP.get_or_init(|| {
        FakeIp::new(&config().fakeip_cidr).unwrap_or_else(|_| FakeIp::with_default_cidr())
    })
}

/// 重入防护 RAII guard。
pub struct HookGuard;

impl HookGuard {
    /// 进入 hook 处理；若已在 hook 中返回 None（调用方应放行原始调用）。
    pub fn enter() -> Option<Self> {
        IN_HOOK.with(|in_hook| {
            if in_hook.get() {
                None
            } else {
                in_hook.set(true);
                Some(HookGuard)
            }
        })
    }
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        IN_HOOK.with(|in_hook| in_hook.set(false));
    }
}

/// 引擎是否已安装。
pub fn is_installed() -> bool {
    INSTALLED.load(Ordering::SeqCst)
}
