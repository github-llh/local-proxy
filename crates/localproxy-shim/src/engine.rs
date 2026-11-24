//! shim 引擎：connect 代理、getaddrinfo FakeIP、重入防护。
//!
//! 平台 hook 层（macOS interpose / Windows MinHook）只负责拦截，
//! 所有决策逻辑收敛到这里，保证两平台行为一致。
//!
//! # TCP 代理原理
//! 1. `getaddrinfo("example.com")` 被拦截 → 分配 FakeIP（198.18.x.x）并记录
//!    `FakeIP → example.com`，应用随后 `connect(FakeIP, 443)`；
//! 2. `connect` 被拦截 → 查表得真实域名 → 经本地核心 SOCKS5 拨号 →
//!    `dup2` 把已连接的 fd 替换到原 fd 号 → 返回 0（成功）；
//! 3. 应用后续 `send`/`recv` 对原 fd 的操作自然流经代理。
//!
//! 全程应用不发出真实 DNS 查询（非局域网目标）。

use std::net::{Ipv4Addr, SocketAddr};

use localproxy_net::fakeip::FakeIp;
use localproxy_net::local_target;
use localproxy_net::socks5::{self, ProxyAuth, Timeouts};

use crate::config::{ShimConfig, ShimMode};

/// 全局配置（首次 install 后不变）。
static CONFIG: std::sync::OnceLock<ShimConfig> = std::sync::OnceLock::new();

/// 引擎是否已安装。
static INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

// 重入防护：hook 函数内部若再次触发网络调用，直接放行原始调用。
thread_local! {
    static IN_HOOK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 安装引擎（幂等）。返回 false 表示已安装或配置非法。
pub fn install(cfg: &ShimConfig) -> bool {
    if INSTALLED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return false;
    }
    if FakeIp::new(&cfg.fakeip_cidr).is_err() {
        tracing::error!("shim: FakeIP CIDR 非法: {}", cfg.fakeip_cidr);
        INSTALLED.store(false, std::sync::atomic::Ordering::SeqCst);
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

/// 取当前配置的 FakeIP 分配器（Mutex 保护：分配/回收需可变访问）。
pub fn fakeip() -> &'static std::sync::Mutex<FakeIp> {
    static FAKEIP: std::sync::OnceLock<std::sync::Mutex<FakeIp>> = std::sync::OnceLock::new();
    FAKEIP.get_or_init(|| {
        std::sync::Mutex::new(
            FakeIp::new(&config().fakeip_cidr).unwrap_or_else(|_| FakeIp::with_default_cidr()),
        )
    })
}

/// FakeIP 锁辅助：忽略 poison（panic 后映射表仍可用）。
pub fn fakeip_lock() -> std::sync::MutexGuard<'static, FakeIp> {
    fakeip().lock().unwrap_or_else(|e| e.into_inner())
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
    INSTALLED.load(std::sync::atomic::Ordering::SeqCst)
}

// ============= 地址判定 =============

/// 私网 IPv4（含回环、链路本地）：shim 侧直连，语义对齐 core 的 default-private 规则。
pub fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    match o[0] {
        10 | 127 => true,
        172 => o[1] & 0xF0 == 16, // 172.16.0.0/12
        192 => o[1] == 168,       // 192.168.0.0/16
        169 => o[1] == 254,       // 169.254.0.0/16
        _ => false,
    }
}

/// 私网 IPv6（fc00::/7 ULA、fe80::/10 链路本地、::1）。
pub fn is_private_ipv6(ip: std::net::Ipv6Addr) -> bool {
    let seg = ip.segments();
    (seg[0] & 0xFE00) == 0xFC00 || (seg[0] & 0xFFC0) == 0xFE80 || ip.is_loopback()
}

/// SocketAddr 是否应直连（回环 / 私网 / mDNS）。
pub fn should_bypass(addr: &SocketAddr) -> bool {
    match addr {
        SocketAddr::V4(v4) => local_target::is_ipv4_loopback(*v4.ip()) || is_private_ipv4(*v4.ip()),
        SocketAddr::V6(v6) => local_target::is_ipv6_loopback(*v6.ip()) || is_private_ipv6(*v6.ip()),
    }
}

// ============= connect 代理 =============

/// 代理一次 TCP connect。
///
/// - 返回 `Ok(Some(0))`：已代为完成代理连接（fd 已被替换），hook 层应返回 0
/// - 返回 `Ok(None)`：直连放行，hook 层应调用原始 connect
/// - 返回 `Err(e)`：代理失败，hook 层应设置 errno 并返回 -1
pub fn proxy_connect(fd: RawFd, addr: &SocketAddr) -> std::io::Result<Option<i32>> {
    let cfg = config();
    if cfg.mode == ShimMode::Direct || !is_installed() {
        return Ok(None);
    }
    if should_bypass(addr) {
        return Ok(None);
    }

    // FakeIP → 域名；无映射（IP 字面量）则直接用 IP
    let host = match addr {
        SocketAddr::V4(v4) => {
            let ip = *v4.ip();
            match fakeip_lock().lookup_domain(u32::from(ip)) {
                Some(domain) => domain.to_string(),
                None => ip.to_string(),
            }
        }
        SocketAddr::V6(v6) => v6.ip().to_string(),
    };
    let port = addr.port();

    match dial_via_core(&host, port) {
        Ok(core_fd) => {
            // 用已连接的 fd 替换原 fd 号
            // SAFETY: core_fd 有效；dup2 后原 fd 号指向同一 socket
            let rc = unsafe { libc::dup2(core_fd, fd) };
            if rc < 0 {
                let err = std::io::Error::last_os_error();
                unsafe { libc::close(core_fd) };
                return Err(err);
            }
            unsafe { libc::close(core_fd) };
            tracing::debug!("shim: 代理 connect fd={fd} -> {host}:{port}");
            Ok(Some(0))
        }
        Err(e) => {
            tracing::warn!("shim: 代理 connect 失败 {host}:{port}: {e}");
            Err(e)
        }
    }
}

/// 经本地核心建立 SOCKS5 连接，返回已连接的 fd（所有权转移给调用方）。
pub fn dial_via_core(host: &str, port: u16) -> std::io::Result<RawFd> {
    let cfg = config();
    let auth = ProxyAuth::default();
    let timeouts = Timeouts::default();
    let stream = socks5::connect(cfg.core_endpoint(), host, port, &auth, &timeouts)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    use std::os::fd::IntoRawFd;
    Ok(stream.into_raw_fd())
}

// ============= getaddrinfo FakeIP =============

/// 域名是否应该分配 FakeIP（排除回环/本地域名）。
pub fn should_fakeip(node: &str) -> bool {
    !local_target::should_bypass_proxy_host(node)
}

/// 由 service 字符串解析端口（"80"/"http"/NULL）。
pub fn parse_service_port(service: Option<&str>) -> u16 {
    let Some(service) = service else {
        return 0;
    };
    if let Ok(port) = service.parse::<u16>() {
        return port;
    }
    // 常见服务名
    match service.to_ascii_lowercase().as_str() {
        "http" | "www" | "www-http" => 80,
        "https" | "ssl" => 443,
        "ftp" => 21,
        "ssh" => 22,
        "dns" | "domain" => 53,
        "smtp" => 25,
        _ => 0,
    }
}

/// Raw fd 类型别名（平台无关）。
pub type RawFd = std::os::fd::RawFd;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_ipv4_ranges() {
        assert!(is_private_ipv4(Ipv4Addr::new(10, 1, 2, 3)));
        assert!(is_private_ipv4(Ipv4Addr::new(172, 16, 0, 1)));
        assert!(is_private_ipv4(Ipv4Addr::new(172, 31, 255, 255)));
        assert!(is_private_ipv4(Ipv4Addr::new(192, 168, 0, 1)));
        assert!(is_private_ipv4(Ipv4Addr::new(169, 254, 1, 1)));
        assert!(is_private_ipv4(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(!is_private_ipv4(Ipv4Addr::new(172, 32, 0, 1)));
        assert!(!is_private_ipv4(Ipv4Addr::new(8, 8, 8, 8)));
    }

    #[test]
    fn private_ipv6_ranges() {
        assert!(is_private_ipv6("fd00::1".parse().unwrap()));
        assert!(is_private_ipv6("fe80::1".parse().unwrap()));
        assert!(is_private_ipv6("::1".parse().unwrap()));
        assert!(!is_private_ipv6("2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn should_bypass_cases() {
        assert!(should_bypass(&"127.0.0.1:80".parse().unwrap()));
        assert!(should_bypass(&"192.168.1.5:443".parse().unwrap()));
        assert!(should_bypass(&"10.0.0.1:22".parse().unwrap()));
        assert!(!should_bypass(&"8.8.8.8:443".parse().unwrap()));
        assert!(!should_bypass(&"1.1.1.1:53".parse().unwrap()));
    }

    #[test]
    fn should_fakeip_excludes_local() {
        assert!(should_fakeip("example.com"));
        assert!(!should_fakeip("localhost"));
        assert!(!should_fakeip("printer.local"));
        assert!(!should_fakeip("127.0.0.1"));
    }

    #[test]
    fn service_port_parsing() {
        assert_eq!(parse_service_port(Some("443")), 443);
        assert_eq!(parse_service_port(Some("http")), 80);
        assert_eq!(parse_service_port(Some("https")), 443);
        assert_eq!(parse_service_port(Some("ssh")), 22);
        assert_eq!(parse_service_port(Some("weird")), 0);
        assert_eq!(parse_service_port(None), 0);
    }
}
