//! FakeIP 分配器（Ring Buffer 策略）。
//!
//! 移植自旧项目 `src/network/FakeIP.hpp`：
//! - 默认网段 198.18.0.0/15（RFC 2544 基准测试保留段，不易冲突）
//! - 域名 ↔ IP 双向映射，IP 用尽时按游标循环复用（淘汰最旧映射）
//! - 不再使用 Windows 共享内存跨进程表：shim 进程内映射即可满足
//!   （core 为无状态标准代理，真实域名经 SOCKS5 ATYP_DOMAIN 透传）

use std::collections::HashMap;
use std::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;

/// FakeIP 配置/解析错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeIpError {
    /// CIDR 缺少 `/prefix`
    MissingPrefix,
    /// IPv4 地址非法
    InvalidAddress(String),
    /// 前缀长度非法（>32）
    InvalidPrefix(u32),
    /// 网段可用地址不足（至少需要 2 个）
    RangeTooSmall,
}

impl fmt::Display for FakeIpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FakeIpError::MissingPrefix => write!(f, "CIDR 缺少 /prefix"),
            FakeIpError::InvalidAddress(s) => write!(f, "IPv4 地址非法: {s}"),
            FakeIpError::InvalidPrefix(p) => write!(f, "前缀长度非法: {p} (有效范围 0-32)"),
            FakeIpError::RangeTooSmall => write!(f, "网段可用地址不足（至少 2 个）"),
        }
    }
}

impl std::error::Error for FakeIpError {}

/// 解析点分十进制 IPv4 为 host order u32（移植旧 `ParseIPv4`）。
pub fn parse_ipv4(s: &str) -> Option<u32> {
    let parts: Vec<&str> = s.trim().split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut value: u32 = 0;
    for part in parts {
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n: u32 = part.parse().ok()?;
        if n > 255 {
            return None;
        }
        value = (value << 8) | n;
    }
    Some(value)
}

/// host order u32 转点分十进制。
pub fn format_ipv4(ip: u32) -> String {
    Ipv4Addr::from(ip).to_string()
}

/// FakeIP 分配器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeIp {
    cidr: String,
    base_ip: u32,
    network_size: u32,
    cursor: u32,
    ip_to_domain: HashMap<u32, String>,
    domain_to_ip: HashMap<String, u32>,
}

impl FakeIp {
    /// 默认网段 198.18.0.0/15。
    pub const DEFAULT_CIDR: &'static str = "198.18.0.0/15";

    /// 从 CIDR 构造，如 `198.18.0.0/15`。
    pub fn new(cidr: &str) -> Result<Self, FakeIpError> {
        let cidr = cidr.trim();
        let slash = cidr.find('/').ok_or(FakeIpError::MissingPrefix)?;
        let ip_part = cidr[..slash].trim();
        let prefix_part = cidr[slash + 1..].trim();
        let prefix: u32 = prefix_part
            .parse()
            .map_err(|_| FakeIpError::InvalidPrefix(0))?;
        if prefix > 32 {
            return Err(FakeIpError::InvalidPrefix(prefix));
        }
        let raw_ip =
            parse_ipv4(ip_part).ok_or_else(|| FakeIpError::InvalidAddress(ip_part.to_string()))?;
        let mask: u32 = if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - prefix)
        };
        let base_ip = raw_ip & mask;
        let network_size: u32 = if prefix >= 31 {
            2
        } else {
            1u32 << (32 - prefix)
        };
        Ok(Self {
            cidr: cidr.to_string(),
            base_ip,
            network_size,
            cursor: 0,
            ip_to_domain: HashMap::new(),
            domain_to_ip: HashMap::new(),
        })
    }

    /// 默认网段构造。
    pub fn with_default_cidr() -> Self {
        Self::new(Self::DEFAULT_CIDR).expect("默认 CIDR 合法")
    }

    /// 当前 CIDR。
    pub fn cidr(&self) -> &str {
        &self.cidr
    }

    /// 映射条目数。
    pub fn len(&self) -> usize {
        self.domain_to_ip.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.domain_to_ip.is_empty()
    }

    /// 为域名分配（或取回既有）FakeIP。
    ///
    /// Ring Buffer 语义：新域名取游标位置；该位置被旧域名占用时先淘汰旧映射。
    /// 返回 None 表示域名非法（空或仅由点组成）。
    pub fn assign(&mut self, domain: &str) -> Option<u32> {
        let domain = normalize_domain(domain)?;
        if let Some(existing) = self.domain_to_ip.get(&domain) {
            return Some(*existing);
        }
        let ip = self.base_ip.wrapping_add(self.cursor);
        self.cursor = (self.cursor + 1) % self.network_size;
        if let Some(old_domain) = self.ip_to_domain.insert(ip, domain.clone()) {
            self.domain_to_ip.remove(&old_domain);
        }
        self.domain_to_ip.insert(domain, ip);
        Some(ip)
    }

    /// 由 FakeIP 反查域名。
    pub fn lookup_domain(&self, ip: u32) -> Option<&str> {
        self.ip_to_domain.get(&ip).map(String::as_str)
    }

    /// 由域名反查 FakeIP。
    pub fn lookup_ip(&self, domain: &str) -> Option<u32> {
        let domain = normalize_domain(domain)?;
        self.domain_to_ip.get(&domain).copied()
    }

    /// 移除域名映射。
    pub fn remove_domain(&mut self, domain: &str) -> bool {
        let Some(domain) = normalize_domain(domain) else {
            return false;
        };
        match self.domain_to_ip.remove(&domain) {
            Some(ip) => {
                self.ip_to_domain.remove(&ip);
                true
            }
            None => false,
        }
    }

    /// IP 是否落在本分配器网段内（用于 hook 侧快速判定）。
    pub fn contains(&self, ip: u32) -> bool {
        ip >= self.base_ip && ip < self.base_ip.saturating_add(self.network_size)
    }

    /// 网段起始 IP（host order）。
    pub fn base_ip(&self) -> u32 {
        self.base_ip
    }
}

/// 域名规范化：小写、去首尾空白与尾点；空域名返回 None。
fn normalize_domain(domain: &str) -> Option<String> {
    let trimmed = domain.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_trailing = trimmed.trim_end_matches('.');
    if without_trailing.is_empty() {
        return None;
    }
    Some(without_trailing.to_ascii_lowercase())
}

impl FromStr for FakeIp {
    type Err = FakeIpError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cidr_basic() {
        let f = FakeIp::new("198.18.0.0/15").unwrap();
        assert_eq!(f.cidr(), "198.18.0.0/15");
        assert_eq!(f.base_ip(), parse_ipv4("198.18.0.0").unwrap());
        assert_eq!(f.network_size, 1 << 17);
    }

    #[test]
    fn parse_cidr_errors() {
        assert_eq!(FakeIp::new("198.18.0.0"), Err(FakeIpError::MissingPrefix));
        assert!(matches!(
            FakeIp::new("999.1.1.1/24"),
            Err(FakeIpError::InvalidAddress(_))
        ));
        assert!(matches!(
            FakeIp::new("198.18.0.0/33"),
            Err(FakeIpError::InvalidPrefix(33))
        ));
    }

    #[test]
    fn assign_is_stable_and_bidirectional() {
        let mut f = FakeIp::with_default_cidr();
        let ip = f.assign("example.com").unwrap();
        assert_eq!(f.lookup_domain(ip), Some("example.com"));
        assert_eq!(f.lookup_ip("example.com"), Some(ip));
        // 重复分配返回同一 IP
        assert_eq!(f.assign("EXAMPLE.com."), Some(ip));
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn ring_buffer_recycles_oldest() {
        let mut f = FakeIp::new("198.18.0.0/29").unwrap(); // 8 个可用地址
        let first = f.assign("a.com").unwrap();
        let second = f.assign("b.com").unwrap();
        assert_ne!(first, second);
        // 占满剩余 6 个地址
        for i in 0..6 {
            f.assign(&format!("x{i}.com")).unwrap();
        }
        // 游标回到 first：最旧的 a.com 被回收给新域名
        let recycled = f.assign("new.com").unwrap();
        assert_eq!(recycled, first);
        assert_eq!(f.lookup_domain(first), Some("new.com"));
        assert_eq!(f.lookup_ip("a.com"), None);
        // b.com 的 IP 尚未被轮到
        assert_eq!(f.lookup_domain(second), Some("b.com"));
    }

    #[test]
    fn contains_and_remove() {
        let mut f = FakeIp::with_default_cidr();
        let ip = f.assign("example.com").unwrap();
        assert!(f.contains(ip));
        assert!(!f.contains(parse_ipv4("8.8.8.8").unwrap()));
        assert!(f.remove_domain("example.com"));
        assert_eq!(f.lookup_domain(ip), None);
        assert!(!f.remove_domain("example.com"));
        assert!(!f.remove_domain(""));
    }

    #[test]
    fn parse_ipv4_edges() {
        assert_eq!(parse_ipv4("0.0.0.0"), Some(0));
        assert_eq!(parse_ipv4("255.255.255.255"), Some(u32::MAX));
        assert_eq!(
            parse_ipv4(" 10.0.0.1 "),
            Some(parse_ipv4("10.0.0.1").unwrap())
        );
        assert_eq!(parse_ipv4("10.0.0"), None);
        assert_eq!(parse_ipv4("10.0.0.256"), None);
        assert_eq!(parse_ipv4("a.b.c.d"), None);
        assert_eq!(parse_ipv4(""), None);
    }
}
