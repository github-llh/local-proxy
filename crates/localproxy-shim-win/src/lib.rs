//! LocalProxy Windows 注入 shim。
//!
//! 经远程线程 `LoadLibrary` 注入目标进程，
//! 使用 vendored MinHook hook ws2_32 网络函数。
//!
//! 注意：DllMain 中不做任何重活（规避 loader lock），
//! 真正的 hook 安装延迟到首个被 hook 函数调用时。

#![allow(clippy::missing_safety_doc)]

use localproxy_shim::config::ShimConfig;

/// DLL 加载完成后的初始化入口（由被 hook 函数首次调用触发）。
#[no_mangle]
pub extern "C" fn localproxy_shim_init() -> bool {
    let cfg = ShimConfig::from_env();
    localproxy_shim::engine::install(&cfg)
}
