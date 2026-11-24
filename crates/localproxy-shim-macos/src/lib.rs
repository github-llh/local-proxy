//! LocalProxy macOS 注入 shim。
//!
//! 经 `DYLD_INSERT_LIBRARIES` 注入目标进程，
//! 使用 `__DATA,__interpose` 段 hook BSD socket 系列函数。
//!
//! 注意：dylib 产物需 adhoc 签名（`codesign -s -`）方可在 arm64 加载。

#![allow(clippy::missing_safety_doc)]

use localproxy_shim::config::ShimConfig;

/// shim 构造入口（由 interpose 函数首次调用时惰性初始化触发）。
#[no_mangle]
pub extern "C" fn localproxy_shim_init() -> bool {
    let cfg = ShimConfig::from_env();
    localproxy_shim::engine::install(&cfg)
}
