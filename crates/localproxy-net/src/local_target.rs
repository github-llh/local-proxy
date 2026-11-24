//! 本地/回环目标判定。
//!
//! 移植自旧项目 `src/network/LocalTargetPolicy.hpp`：
//! 回环目标必须直连，绝不能送入代理（否则形成环路）。
//! 另补充 mDNS/链路本地域名判定，语义与旧 Hooks.cpp 的本地放行一致。

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

/// IPv4 回环判定（127.0.0.0/8）。
pub fn is_ipv4_loopback(addr: Ipv4Addr) -> bool {
    addr.octets()[0] == 127
}

/// IPv6 回环判定（::1，含 IPv4-mapped 的 127.x）。
pub fn is_ipv6_loopback(addr: Ipv6Addr) -> bool {
    if addr.is_loopback() {
        return true;
    }
    if let Some(mapped) = addr.to_ipv4_mapped() {
        return is_ipv4_loopback(mapped);
    }
    false
}

/// 规范化主机名：小写、去尾点、去 IPv6 方括号。
pub fn normalize_loopback_host(host: &str) -> String {
    let mut normalized = host.trim().to_ascii_lowercase();
    while normalized.ends_with('.') {
        normalized.pop();
    }
    if normalized.len() >= 2 && normalized.starts_with('[') && normalized.ends_with(']') {
        normalized = normalized[1..normalized.len() - 1].to_string();
    }
    normalized
}

/// 主机名是否指向本机（localhost / 127.x / ::1）。
pub fn is_loopback_host(host: &str) -> bool {
    let normalized = normalize_loopback_host(host);
    if normalized == "localhost" {
        return true;
    }
    if let Ok(v4) = normalized.parse::<Ipv4Addr>() {
        return is_ipv4_loopback(v4);
    }
    if let Ok(v6) = normalized.parse::<Ipv6Addr>() {
        return is_ipv6_loopback(v6);
    }
    false
}

/// SocketAddr 是否指向本机。
pub fn is_loopback_sockaddr(addr: &SocketAddr) -> bool {
    match addr {
        SocketAddr::V4(v4) => is_ipv4_loopback(*v4.ip()),
        SocketAddr::V6(v6) => is_ipv6_loopback(*v6.ip()),
    }
}

/// mDNS / 链路本地域名：必须直连（送入代理反而无法解析）。
pub fn is_local_domain(host: &str) -> bool {
    let normalized = normalize_loopback_host(host);
    if normalized.is_empty() {
        return false;
    }
    const LOCAL_SUFFIXES: [&str; 5] = [".local", ".localhost", ".lan", ".home", ".internal"];
    LOCAL_SUFFIXES
        .iter()
        .any(|suffix| normalized.ends_with(*suffix) || normalized == suffix[1..])
}

/// 综合判定：该主机是否应绕过代理（回环 + mDNS 本地域名）。
pub fn should_bypass_proxy_host(host: &str) -> bool {
    is_loopback_host(host) || is_local_domain(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_ipv4_and_ipv6() {
        assert!(is_ipv4_loopback(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(is_ipv4_loopback(Ipv4Addr::new(127, 255, 255, 254)));
        assert!(!is_ipv4_loopback(Ipv4Addr::new(128, 0, 0, 1)));
        assert!(is_ipv6_loopback(Ipv6Addr::LOCALHOST));
        assert!(is_ipv6_loopback("::ffff:127.0.0.1".parse().unwrap()));
        assert!(!is_ipv6_loopback("2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn loopback_host_normalization() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("LOCALHOST."));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("[::1]"));
        assert!(is_loopback_host("::ffff:127.0.0.1"));
        assert!(!is_loopback_host("example.com"));
        assert!(!is_loopback_host("1270.0.0.1"));
    }

    #[test]
    fn local_domains_bypass() {
        assert!(is_local_domain("printer.local"));
        assert!(is_local_domain("myhost.home"));
        assert!(is_local_domain("nas.internal"));
        assert!(!is_local_domain("example.com"));
        assert!(!is_local_domain("notlocal.com"));
        assert!(should_bypass_proxy_host("localhost"));
        assert!(should_bypass_proxy_host("router.local"));
        assert!(!should_bypass_proxy_host("github.com"));
    }
}
