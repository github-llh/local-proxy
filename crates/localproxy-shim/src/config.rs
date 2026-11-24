//! shim 运行配置（来自 launcher 注入的环境变量）。
//!
//! shim 零磁盘配置：所有参数经 `LP_*` 环境变量传入，
//! 缺省值保证在未注入任何变量时行为可预期（连 127.0.0.1:1080）。

use localproxy_net::fakeip::FakeIp;

/// 环境变量前缀。
pub const ENV_PREFIX: &str = "LP_";

/// 环境变量名。
pub mod env_keys {
    pub const CORE_HOST: &str = "LP_CORE_HOST";
    pub const CORE_PORT: &str = "LP_CORE_PORT";
    pub const FAKEIP_CIDR: &str = "LP_FAKEIP_CIDR";
    pub const MODE: &str = "LP_MODE";
    pub const LOG: &str = "LP_LOG";
    pub const UDP_MODE: &str = "LP_UDP_MODE";
    pub const IPV6_MODE: &str = "LP_IPV6_MODE";
}

/// shim 工作模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShimMode {
    /// 代理模式（默认）：拦截并送往本地核心
    #[default]
    Proxy,
    /// 直连模式：仅记录日志，不拦截（排障对照用）
    Direct,
    /// 调试模式：代理 + 详细日志
    Debug,
}

impl ShimMode {
    pub fn from_env_value(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "direct" => ShimMode::Direct,
            "debug" => ShimMode::Debug,
            _ => ShimMode::Proxy,
        }
    }
}

/// UDP 策略（由 launcher 从 core 的 `udp_mode` × 上游类型解析后注入）。
///
/// - `Block`（默认）：阻断公网 UDP，返回标准 errno 促使应用回退 TCP
///   （QUIC→TCP、WebRTC 降级 TCP candidate，行业标准实践）
/// - `Direct`：UDP 直连放行
/// - `Proxy`：SOCKS5 UDP Associate 全代理（S2 实现）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UdpMode {
    /// 阻断公网 UDP（默认，最防泄漏）
    #[default]
    Block,
    /// UDP 直连
    Direct,
    /// UDP 走代理（S2）
    Proxy,
}

impl UdpMode {
    pub fn from_env_value(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "direct" => UdpMode::Direct,
            "proxy" => UdpMode::Proxy,
            _ => UdpMode::Block,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            UdpMode::Block => "block",
            UdpMode::Direct => "direct",
            UdpMode::Proxy => "proxy",
        }
    }
}

/// IPv6 策略（由 launcher 从 core 的 `ipv6_mode` 注入）。
///
/// - `Proxy`（默认）：IPv6 走代理
/// - `Direct`：IPv6 直连
/// - `Block`：阻断公网 IPv6（WebRTC host candidate 的 IPv6 全局地址是
///   「代理从未触碰」的稳定标识，阻断它是防泄漏必需项）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Ipv6Mode {
    /// IPv6 走代理（默认）
    #[default]
    Proxy,
    /// IPv6 直连
    Direct,
    /// 阻断公网 IPv6
    Block,
}

impl Ipv6Mode {
    pub fn from_env_value(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "direct" => Ipv6Mode::Direct,
            "block" => Ipv6Mode::Block,
            _ => Ipv6Mode::Proxy,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Ipv6Mode::Proxy => "proxy",
            Ipv6Mode::Direct => "direct",
            Ipv6Mode::Block => "block",
        }
    }
}

/// shim 配置。
#[derive(Debug, Clone)]
pub struct ShimConfig {
    /// 本地核心地址（默认 127.0.0.1）
    pub core_host: String,
    /// 本地核心 SOCKS5 端口（默认 1080）
    pub core_port: u16,
    /// FakeIP 网段（默认 198.18.0.0/15）
    pub fakeip_cidr: String,
    /// 工作模式
    pub mode: ShimMode,
    /// 日志开关（off/info/debug）
    pub log: String,
    /// UDP 策略（默认 block）
    pub udp_mode: UdpMode,
    /// IPv6 策略（默认 proxy）
    pub ipv6_mode: Ipv6Mode,
}

impl Default for ShimConfig {
    fn default() -> Self {
        Self {
            core_host: "127.0.0.1".to_string(),
            core_port: 1080,
            fakeip_cidr: FakeIp::DEFAULT_CIDR.to_string(),
            mode: ShimMode::Proxy,
            log: "off".to_string(),
            udp_mode: UdpMode::Block,
            ipv6_mode: Ipv6Mode::Proxy,
        }
    }
}

impl ShimConfig {
    /// 从环境变量构造（`LP_CORE_HOST` 等）。
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(host) = std::env::var(env_keys::CORE_HOST) {
            let host = host.trim();
            if !host.is_empty() {
                cfg.core_host = host.to_string();
            }
        }
        if let Ok(port) = std::env::var(env_keys::CORE_PORT) {
            if let Ok(port) = port.trim().parse::<u16>() {
                if port != 0 {
                    cfg.core_port = port;
                }
            }
        }
        if let Ok(cidr) = std::env::var(env_keys::FAKEIP_CIDR) {
            let cidr = cidr.trim();
            if !cidr.is_empty() && FakeIp::new(cidr).is_ok() {
                cfg.fakeip_cidr = cidr.to_string();
            }
        }
        if let Ok(mode) = std::env::var(env_keys::MODE) {
            cfg.mode = ShimMode::from_env_value(&mode);
        }
        if let Ok(log) = std::env::var(env_keys::LOG) {
            cfg.log = log.trim().to_ascii_lowercase();
        }
        if let Ok(udp) = std::env::var(env_keys::UDP_MODE) {
            cfg.udp_mode = UdpMode::from_env_value(&udp);
        }
        if let Ok(ipv6) = std::env::var(env_keys::IPV6_MODE) {
            cfg.ipv6_mode = Ipv6Mode::from_env_value(&ipv6);
        }
        cfg
    }

    /// 核心 SOCKS5 端点字符串（`host:port`）。
    pub fn core_endpoint(&self) -> String {
        format!("{}:{}", self.core_host, self.core_port)
    }

    /// 是否启用日志。
    pub fn log_enabled(&self) -> bool {
        self.log != "off"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let cfg = ShimConfig::default();
        assert_eq!(cfg.core_host, "127.0.0.1");
        assert_eq!(cfg.core_port, 1080);
        assert_eq!(cfg.fakeip_cidr, "198.18.0.0/15");
        assert_eq!(cfg.mode, ShimMode::Proxy);
        assert_eq!(cfg.core_endpoint(), "127.0.0.1:1080");
        assert!(!cfg.log_enabled());
        // 安全默认：UDP block、IPv6 proxy
        assert_eq!(cfg.udp_mode, UdpMode::Block);
        assert_eq!(cfg.ipv6_mode, Ipv6Mode::Proxy);
    }

    #[test]
    fn mode_parsing() {
        assert_eq!(ShimMode::from_env_value("direct"), ShimMode::Direct);
        assert_eq!(ShimMode::from_env_value("DEBUG"), ShimMode::Debug);
        assert_eq!(ShimMode::from_env_value("proxy"), ShimMode::Proxy);
        assert_eq!(ShimMode::from_env_value(""), ShimMode::Proxy);
    }

    #[test]
    fn udp_mode_parsing_defaults_to_block() {
        assert_eq!(UdpMode::from_env_value("direct"), UdpMode::Direct);
        assert_eq!(UdpMode::from_env_value("proxy"), UdpMode::Proxy);
        assert_eq!(UdpMode::from_env_value("block"), UdpMode::Block);
        // 未识别值 → block（fail-closed）
        assert_eq!(UdpMode::from_env_value(""), UdpMode::Block);
        assert_eq!(UdpMode::from_env_value("garbage"), UdpMode::Block);
        assert_eq!(UdpMode::Block.name(), "block");
    }

    #[test]
    fn ipv6_mode_parsing_defaults_to_proxy() {
        assert_eq!(Ipv6Mode::from_env_value("direct"), Ipv6Mode::Direct);
        assert_eq!(Ipv6Mode::from_env_value("block"), Ipv6Mode::Block);
        assert_eq!(Ipv6Mode::from_env_value(""), Ipv6Mode::Proxy);
        assert_eq!(Ipv6Mode::from_env_value("garbage"), Ipv6Mode::Proxy);
        assert_eq!(Ipv6Mode::Block.name(), "block");
    }
}
