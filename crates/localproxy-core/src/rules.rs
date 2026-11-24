//! 路由规则引擎。
//!
//! 逐函数移植自旧项目 `src/core/Config.hpp` 的编译与匹配逻辑：
//! - CIDR v4/v6、域名后缀/通配符、端口范围、协议匹配
//! - `priority_mode=order` 按数组序；`number` 按 priority 降序
//! - `use_default_private` 自动注入内网直连规则（含 UDP，语义对齐旧 #169 修复）
//! - 编译统计供启动日志使用（快速判断规则是否生效）

use crate::config::RoutingRule;

/// 路由动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Proxy,
    Direct,
}

impl Action {
    pub fn name(self) -> &'static str {
        match self {
            Action::Proxy => "proxy",
            Action::Direct => "direct",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "proxy" => Some(Action::Proxy),
            "direct" => Some(Action::Direct),
            _ => None,
        }
    }
}

/// 匹配结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecision {
    pub action: Action,
    /// 命中的规则名（默认动作为空串）
    pub rule: String,
}

/// CIDR v4（host order）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CidrV4 {
    network: u32,
    mask: u32,
}

/// CIDR v6。
#[derive(Debug, Clone, PartialEq, Eq)]
struct CidrV6 {
    network: [u8; 16],
    prefix: u8,
}

/// 端口范围（对外暴露，便于 server 层复用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortRange {
    pub start: u16,
    pub end: u16,
}

/// 编译后的规则。
#[derive(Debug, Clone)]
struct CompiledRule {
    raw: RoutingRule,
    v4: Vec<CidrV4>,
    v6: Vec<CidrV6>,
    domains: Vec<String>, // 已小写
    port_ranges: Vec<PortRange>,
    protocols: Vec<String>, // 已小写
}

/// 规则引擎。
#[derive(Debug, Clone)]
pub struct RuleEngine {
    rules: Vec<CompiledRule>,
    /// 求值顺序（索引数组）
    order: Vec<usize>,
    default_action: Action,
    enabled: bool,
    /// 编译统计
    pub valid_cidr_v4: usize,
    pub valid_cidr_v6: usize,
    pub valid_port_ranges: usize,
    pub skipped_invalid_items: usize,
}

// ============= 解析辅助（移植旧 ProxyRules 静态函数） =============

/// 解析点分十进制 IPv4 为 host order u32。
pub fn parse_ipv4(s: &str) -> Option<u32> {
    let parts: Vec<&str> = s.trim().split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut value = 0u32;
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

/// 解析 IPv6 为 16 字节（支持 `::` 压缩与 IPv4 嵌入）。
pub fn parse_ipv6(s: &str) -> Option<[u8; 16]> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let addr: std::net::Ipv6Addr = s.parse().ok()?;
    Some(addr.octets())
}

fn parse_cidr_v4(cidr: &str) -> Option<CidrV4> {
    let slash = cidr.find('/')?;
    let ip = parse_ipv4(cidr[..slash].trim())?;
    let bits: u32 = cidr[slash + 1..].trim().parse().ok()?;
    if bits > 32 {
        return None;
    }
    let mask = if bits == 0 {
        0
    } else {
        u32::MAX << (32 - bits)
    };
    Some(CidrV4 {
        network: ip & mask,
        mask,
    })
}

fn parse_cidr_v6(cidr: &str) -> Option<CidrV6> {
    let slash = cidr.find('/')?;
    let addr = parse_ipv6(cidr[..slash].trim())?;
    let bits: u32 = cidr[slash + 1..].trim().parse().ok()?;
    if bits > 128 {
        return None;
    }
    let mut network = addr;
    if bits < 128 {
        let full_bytes = (bits / 8) as usize;
        let rem = bits % 8;
        if rem > 0 {
            let mask = (0xFFu16 << (8 - rem)) as u8;
            network[full_bytes] &= mask;
        }
        for byte in network.iter_mut().skip(full_bytes + usize::from(rem > 0)) {
            *byte = 0;
        }
    }
    Some(CidrV6 {
        network,
        prefix: bits as u8,
    })
}

fn match_cidr_v4(ip: u32, rule: &CidrV4) -> bool {
    (ip & rule.mask) == rule.network
}

fn match_cidr_v6(ip: &[u8; 16], rule: &CidrV6) -> bool {
    let bits = rule.prefix as usize;
    let full_bytes = bits / 8;
    let rem = bits % 8;
    for i in 0..full_bytes {
        if ip[i] != rule.network[i] {
            return false;
        }
    }
    if rem == 0 {
        return true;
    }
    let mask = (0xFFu16 << (8 - rem)) as u8;
    (ip[full_bytes] & mask) == (rule.network[full_bytes] & mask)
}

/// 端口范围解析：`80` / `10000-20000`（自动去空白、起止互换）。
pub fn parse_port_range(token: &str) -> Option<PortRange> {
    let t: String = token.chars().filter(|c| !c.is_whitespace()).collect();
    if t.is_empty() {
        return None;
    }
    let dash = t.find('-');
    match dash {
        None => {
            let v: u32 = t.parse().ok()?;
            if v == 0 || v > 65535 {
                return None;
            }
            Some(PortRange {
                start: v as u16,
                end: v as u16,
            })
        }
        Some(d) => {
            let a: u32 = t[..d].parse().ok()?;
            let b: u32 = t[d + 1..].parse().ok()?;
            if a == 0 || b == 0 || a > 65535 || b > 65535 {
                return None;
            }
            let (start, end) = if a <= b { (a, b) } else { (b, a) };
            Some(PortRange {
                start: start as u16,
                end: end as u16,
            })
        }
    }
}

fn match_port(port: u16, ranges: &[PortRange]) -> bool {
    if ranges.is_empty() {
        return true;
    }
    if port == 0 {
        return false;
    }
    ranges.iter().any(|r| port >= r.start && port <= r.end)
}

fn match_protocol(protocol: &str, protocols: &[String]) -> bool {
    if protocols.is_empty() {
        return true;
    }
    protocols.contains(&protocol.to_ascii_lowercase())
}

/// 域名 glob 匹配（`*` `?`，移植旧 `GlobMatch`）。
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            mark = ti;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// 域名模式匹配（后缀 `.example.com` 同时匹配裸域与子域，移植旧 `MatchDomainPattern`）。
fn match_domain_pattern(pattern: &str, host: &str) -> bool {
    if pattern.is_empty() || host.is_empty() {
        return false;
    }
    let has_wildcard = pattern.contains('*') || pattern.contains('?');
    if !has_wildcard && pattern.starts_with('.') {
        // 规则 ".example.com" 需同时匹配 "example.com" 与 "*.example.com"
        let root = &pattern[1..];
        if host.len() == root.len() && host == root {
            return true;
        }
        return host.ends_with(pattern);
    }
    if !has_wildcard {
        return host == pattern;
    }
    glob_match(pattern, host)
}

// ============= 引擎 =============

impl RuleEngine {
    /// 由配置编译。
    pub fn compile(settings: &crate::config::RoutingSettings) -> Self {
        let mut engine = Self {
            rules: Vec::new(),
            order: Vec::new(),
            default_action: Action::from_str_opt(&settings.default_action).unwrap_or(Action::Proxy),
            enabled: settings.enabled,
            valid_cidr_v4: 0,
            valid_cidr_v6: 0,
            valid_port_ranges: 0,
            skipped_invalid_items: 0,
        };

        let mut src_rules: Vec<RoutingRule> = settings.rules.clone();
        if settings.use_default_private {
            src_rules.insert(0, default_private_rule());
        }

        for rule in &src_rules {
            let mut compiled = CompiledRule {
                raw: rule.clone(),
                v4: Vec::new(),
                v6: Vec::new(),
                domains: Vec::new(),
                port_ranges: Vec::new(),
                protocols: Vec::new(),
            };
            for cidr in &rule.ip_cidrs_v4 {
                match parse_cidr_v4(cidr) {
                    Some(r) => {
                        compiled.v4.push(r);
                        engine.valid_cidr_v4 += 1;
                    }
                    None => {
                        tracing::warn!("路由规则: IPv4 CIDR 无效({}), rule={}", cidr, rule.name);
                        engine.skipped_invalid_items += 1;
                    }
                }
            }
            for cidr in &rule.ip_cidrs_v6 {
                match parse_cidr_v6(cidr) {
                    Some(r) => {
                        compiled.v6.push(r);
                        engine.valid_cidr_v6 += 1;
                    }
                    None => {
                        tracing::warn!("路由规则: IPv6 CIDR 无效({}), rule={}", cidr, rule.name);
                        engine.skipped_invalid_items += 1;
                    }
                }
            }
            for d in &rule.domains {
                let norm = d.trim().to_ascii_lowercase();
                if !norm.is_empty() {
                    compiled.domains.push(norm);
                }
            }
            for p in &rule.ports {
                match parse_port_range(p) {
                    Some(r) => {
                        compiled.port_ranges.push(r);
                        engine.valid_port_ranges += 1;
                    }
                    None => {
                        tracing::warn!("路由规则: 端口范围无效({}), rule={}", p, rule.name);
                        engine.skipped_invalid_items += 1;
                    }
                }
            }
            for proto in &rule.protocols {
                let norm = proto.trim().to_ascii_lowercase();
                if !norm.is_empty() {
                    compiled.protocols.push(norm);
                }
            }
            engine.rules.push(compiled);
        }

        // 求值顺序
        let use_number = settings.priority_mode.trim().eq_ignore_ascii_case("number");
        engine.order = (0..engine.rules.len()).collect();
        if use_number {
            engine.order.sort_by(|&a, &b| {
                engine.rules[b]
                    .raw
                    .priority
                    .cmp(&engine.rules[a].raw.priority)
            });
        }
        engine
    }

    /// 路由决策。
    ///
    /// - `host`：域名（或 IP 字面量）
    /// - `ip`：已解析的 IP（若有；缺省时尝试从 host 解析）
    /// - `port`：目标端口（0 = 未知）
    /// - `protocol`：`tcp` / `udp`
    pub fn decide(&self, host: &str, ip: Option<&str>, port: u16, protocol: &str) -> RouteDecision {
        if !self.enabled {
            return RouteDecision {
                action: self.default_action,
                rule: String::new(),
            };
        }

        let mut host_str = host.trim().to_ascii_lowercase();
        while host_str.ends_with('.') {
            host_str.pop();
        }
        let has_host = !host_str.is_empty();

        // IP 判定：优先显式 ip，否则尝试把 host 当字面量
        let mut ip4 = None;
        let mut ip6 = None;
        let ip_text = ip.map(|s| s.trim()).filter(|s| !s.is_empty());
        if let Some(ip_text) = ip_text {
            if let Some(v4) = parse_ipv4(ip_text) {
                ip4 = Some(v4);
            } else if let Some(v6) = parse_ipv6(ip_text) {
                ip6 = Some(v6);
            }
        } else if has_host {
            if let Some(v4) = parse_ipv4(&host_str) {
                ip4 = Some(v4);
            } else if let Some(v6) = parse_ipv6(&host_str) {
                ip6 = Some(v6);
            }
        }

        for &idx in &self.order {
            let rule = &self.rules[idx];
            if !rule.raw.enabled {
                continue;
            }
            if !match_protocol(protocol, &rule.protocols) {
                continue;
            }
            if !match_port(port, &rule.port_ranges) {
                continue;
            }

            let mut matched = false;
            if has_host && !rule.domains.is_empty() {
                matched = rule
                    .domains
                    .iter()
                    .any(|pattern| match_domain_pattern(pattern, &host_str));
            }
            if !matched {
                if let Some(v4) = ip4 {
                    matched = rule.v4.iter().any(|r| match_cidr_v4(v4, r));
                }
            }
            if !matched {
                if let Some(v6) = &ip6 {
                    matched = rule.v6.iter().any(|r| match_cidr_v6(v6, r));
                }
            }

            if matched {
                return RouteDecision {
                    action: Action::from_str_opt(&rule.raw.action).unwrap_or(self.default_action),
                    rule: rule.raw.name.clone(),
                };
            }
        }

        RouteDecision {
            action: self.default_action,
            rule: String::new(),
        }
    }

    /// 启动日志摘要。
    pub fn summary(&self) -> String {
        format!(
            "规则: {} 条(内网直连={}), v4_cidr={}, v6_cidr={}, ports={}, 跳过无效={}, 默认={}",
            self.rules.len(),
            self.rules
                .first()
                .map(|r| r.raw.name == "default-private")
                .unwrap_or(false),
            self.valid_cidr_v4,
            self.valid_cidr_v6,
            self.valid_port_ranges,
            self.skipped_invalid_items,
            self.default_action.name(),
        )
    }
}

/// 默认内网直连规则（移植旧 `default-private`，含 UDP 覆盖）。
fn default_private_rule() -> RoutingRule {
    RoutingRule {
        name: "default-private".to_string(),
        enabled: true,
        action: "direct".to_string(),
        priority: 0,
        ip_cidrs_v4: vec![
            "10.0.0.0/8".to_string(),
            "172.16.0.0/12".to_string(),
            "192.168.0.0/16".to_string(),
            "127.0.0.0/8".to_string(),
            "169.254.0.0/16".to_string(),
        ],
        ip_cidrs_v6: vec![
            "fc00::/7".to_string(),
            "fe80::/10".to_string(),
            "::1/128".to_string(),
        ],
        domains: Vec::new(),
        ports: Vec::new(),
        // 内网直连同时覆盖 UDP（局域网 WebRTC P2P 不送 SOCKS5 relay）
        protocols: vec!["tcp".to_string(), "udp".to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(rules: Vec<RoutingRule>, default: &str, mode: &str, private: bool) -> RuleEngine {
        RuleEngine::compile(&crate::config::RoutingSettings {
            enabled: true,
            priority_mode: mode.to_string(),
            default_action: default.to_string(),
            use_default_private: private,
            rules,
        })
    }

    // ---- IPv4/IPv6/CIDR 解析（对照旧 test_ipv6_parser.cpp） ----

    #[test]
    fn parse_ipv4_valid_and_invalid() {
        assert_eq!(parse_ipv4("0.0.0.0"), Some(0));
        assert_eq!(parse_ipv4("192.168.1.1"), Some(0xC0A80101));
        assert_eq!(parse_ipv4("255.255.255.255"), Some(u32::MAX));
        assert_eq!(parse_ipv4(" 10.0.0.1 "), Some(0x0A000001));
        assert_eq!(parse_ipv4("1.2.3"), None);
        assert_eq!(parse_ipv4("1.2.3.4.5"), None);
        assert_eq!(parse_ipv4("256.1.1.1"), None);
        assert_eq!(parse_ipv4("a.b.c.d"), None);
    }

    #[test]
    fn parse_ipv6_variants() {
        assert_eq!(
            parse_ipv6("::1"),
            Some([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1])
        );
        assert_eq!(
            parse_ipv6("2001:db8::1"),
            Some([0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1])
        );
        // IPv4 嵌入
        assert_eq!(
            parse_ipv6("::ffff:192.168.0.1"),
            Some([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 192, 168, 0, 1])
        );
        assert_eq!(parse_ipv6("not-an-ip"), None);
        assert_eq!(parse_ipv6(""), None);
        assert_eq!(parse_ipv6("12345::"), None);
    }

    #[test]
    fn cidr_v4_matching() {
        let rule = parse_cidr_v4("192.168.0.0/16").unwrap();
        assert!(match_cidr_v4(parse_ipv4("192.168.1.1").unwrap(), &rule));
        assert!(match_cidr_v4(parse_ipv4("192.168.255.255").unwrap(), &rule));
        assert!(!match_cidr_v4(parse_ipv4("192.169.0.1").unwrap(), &rule));
        assert!(!match_cidr_v4(parse_ipv4("10.0.0.1").unwrap(), &rule));

        let any = parse_cidr_v4("0.0.0.0/0").unwrap();
        assert!(match_cidr_v4(parse_ipv4("8.8.8.8").unwrap(), &any));

        assert!(parse_cidr_v4("10.0.0.0/33").is_none());
        assert!(parse_cidr_v4("10.0.0.0").is_none());
    }

    #[test]
    fn cidr_v6_matching() {
        let rule = parse_cidr_v6("fc00::/7").unwrap();
        assert!(match_cidr_v6(&parse_ipv6("fd12:3456::1").unwrap(), &rule));
        assert!(!match_cidr_v6(&parse_ipv6("2001:db8::1").unwrap(), &rule));

        let v4mapped = parse_cidr_v6("::ffff:0:0/96").unwrap();
        assert!(match_cidr_v6(
            &parse_ipv6("::ffff:1.2.3.4").unwrap(),
            &v4mapped
        ));
    }

    // ---- 域名匹配 ----

    #[test]
    fn domain_pattern_suffix_and_exact() {
        assert!(match_domain_pattern(".example.com", "example.com"));
        assert!(match_domain_pattern(".example.com", "www.example.com"));
        assert!(match_domain_pattern(".example.com", "a.b.example.com"));
        assert!(!match_domain_pattern(".example.com", "notexample.com"));
        assert!(!match_domain_pattern(".example.com", "example.org"));

        assert!(match_domain_pattern("example.com", "example.com"));
        assert!(!match_domain_pattern("example.com", "www.example.com"));
    }

    #[test]
    fn domain_pattern_glob() {
        assert!(match_domain_pattern("*.example.com", "www.example.com"));
        assert!(match_domain_pattern("*.example.com", "a.b.example.com"));
        assert!(!match_domain_pattern("*.example.com", "example.com"));
        assert!(match_domain_pattern("example?.com", "example1.com"));
        assert!(!match_domain_pattern("example?.com", "example12.com"));
        assert!(match_domain_pattern("*", "anything"));
    }

    // ---- 端口 / 协议 ----

    #[test]
    fn port_range_parsing_and_matching() {
        let single = parse_port_range("443").unwrap();
        assert!(match_port(443, &[single]));
        assert!(!match_port(80, &[single]));

        let range = parse_port_range("10000-20000").unwrap();
        assert!(match_port(10000, &[range]));
        assert!(match_port(20000, &[range]));
        assert!(!match_port(9999, &[range]));
        assert!(!match_port(20001, &[range]));

        // 起止互换自动修正
        let swapped = parse_port_range("20000-10000").unwrap();
        assert!(match_port(15000, &[swapped]));

        assert!(parse_port_range("0").is_none());
        assert!(parse_port_range("70000").is_none());
        assert!(parse_port_range("").is_none());
        assert!(parse_port_range("-5").is_none());

        // 空范围 = 全部
        assert!(match_port(12345, &[]));
        assert!(!match_port(0, &[single]));
    }

    #[test]
    fn protocol_matching() {
        assert!(match_protocol("tcp", &[]));
        assert!(match_protocol("TCP", &["tcp".to_string()]));
        assert!(!match_protocol("udp", &["tcp".to_string()]));
    }

    // ---- 引擎决策 ----

    #[test]
    fn default_action_applies() {
        let eng = engine(Vec::new(), "proxy", "order", false);
        let d = eng.decide("example.com", None, 443, "tcp");
        assert_eq!(d.action, Action::Proxy);
        assert!(d.rule.is_empty());

        let eng = engine(Vec::new(), "direct", "order", false);
        let d = eng.decide("example.com", None, 443, "tcp");
        assert_eq!(d.action, Action::Direct);
    }

    #[test]
    fn default_private_rule_directs_lan() {
        let eng = engine(Vec::new(), "proxy", "order", true);
        // 内网 IP 直连（默认规则在最前）
        let d = eng.decide("192.168.1.10", None, 443, "tcp");
        assert_eq!(d.action, Action::Direct);
        assert_eq!(d.rule, "default-private");
        let d = eng.decide("", Some("10.0.0.5"), 80, "tcp");
        assert_eq!(d.action, Action::Direct);
        // 内网 UDP 也直连（#169 语义）
        let d = eng.decide("192.168.1.10", None, 5353, "udp");
        assert_eq!(d.action, Action::Direct);
        // 公网仍走代理
        let d = eng.decide("example.com", None, 443, "tcp");
        assert_eq!(d.action, Action::Proxy);
    }

    #[test]
    fn domain_rule_beats_default() {
        let eng = engine(
            vec![RoutingRule {
                name: "cn-direct".to_string(),
                action: "direct".to_string(),
                domains: vec![".cn".to_string()],
                ..RoutingRule::default()
            }],
            "proxy",
            "order",
            false,
        );
        assert_eq!(
            eng.decide("www.baidu.cn", None, 443, "tcp").action,
            Action::Direct
        );
        assert_eq!(
            eng.decide("www.google.com", None, 443, "tcp").action,
            Action::Proxy
        );
    }

    #[test]
    fn first_match_wins_in_order_mode() {
        let eng = engine(
            vec![
                RoutingRule {
                    name: "r1".to_string(),
                    action: "direct".to_string(),
                    domains: vec![".example.com".to_string()],
                    ..RoutingRule::default()
                },
                RoutingRule {
                    name: "r2".to_string(),
                    action: "proxy".to_string(),
                    domains: vec!["*.example.com".to_string()],
                    ..RoutingRule::default()
                },
            ],
            "proxy",
            "order",
            false,
        );
        assert_eq!(eng.decide("a.example.com", None, 443, "tcp").rule, "r1");
    }

    #[test]
    fn number_mode_sorts_by_priority_desc() {
        let eng = engine(
            vec![
                RoutingRule {
                    name: "low".to_string(),
                    action: "proxy".to_string(),
                    priority: 1,
                    domains: vec![".example.com".to_string()],
                    ..RoutingRule::default()
                },
                RoutingRule {
                    name: "high".to_string(),
                    action: "direct".to_string(),
                    priority: 10,
                    domains: vec![".example.com".to_string()],
                    ..RoutingRule::default()
                },
            ],
            "proxy",
            "number",
            false,
        );
        assert_eq!(eng.decide("a.example.com", None, 443, "tcp").rule, "high");
    }

    #[test]
    fn disabled_rules_skipped() {
        let eng = engine(
            vec![RoutingRule {
                name: "off".to_string(),
                enabled: false,
                action: "direct".to_string(),
                domains: vec![".example.com".to_string()],
                ..RoutingRule::default()
            }],
            "proxy",
            "order",
            false,
        );
        assert_eq!(
            eng.decide("a.example.com", None, 443, "tcp").action,
            Action::Proxy
        );
    }

    #[test]
    fn port_filter_in_rule() {
        let eng = engine(
            vec![RoutingRule {
                name: "no-web".to_string(),
                action: "direct".to_string(),
                ports: vec!["80".to_string(), "443".to_string()],
                domains: vec![".example.com".to_string()],
                ..RoutingRule::default()
            }],
            "proxy",
            "order",
            false,
        );
        assert_eq!(
            eng.decide("a.example.com", None, 443, "tcp").action,
            Action::Direct
        );
        assert_eq!(
            eng.decide("a.example.com", None, 8443, "tcp").action,
            Action::Proxy
        );
    }

    #[test]
    fn ip_literal_host_matches_cidr() {
        let eng = engine(
            vec![RoutingRule {
                name: "lan".to_string(),
                action: "direct".to_string(),
                ip_cidrs_v4: vec!["192.168.0.0/16".to_string()],
                ..RoutingRule::default()
            }],
            "proxy",
            "order",
            false,
        );
        // host 即 IP 字面量
        assert_eq!(
            eng.decide("192.168.5.5", None, 443, "tcp").action,
            Action::Direct
        );
        assert_eq!(
            eng.decide("8.8.8.8", None, 443, "tcp").action,
            Action::Proxy
        );
        // 域名 + 显式 IP
        assert_eq!(
            eng.decide("router.local", Some("192.168.0.1"), 443, "tcp")
                .action,
            Action::Direct
        );
    }

    #[test]
    fn summary_reports_stats() {
        let eng = engine(
            vec![RoutingRule {
                name: "mixed".to_string(),
                ip_cidrs_v4: vec!["10.0.0.0/8".to_string(), "bad".to_string()],
                ports: vec!["443".to_string()],
                ..RoutingRule::default()
            }],
            "proxy",
            "order",
            true,
        );
        assert_eq!(eng.valid_cidr_v4, 6); // 5 private + 1 valid
        assert_eq!(eng.valid_port_ranges, 1);
        assert_eq!(eng.skipped_invalid_items, 1);
        let s = eng.summary();
        assert!(s.contains("规则:"));
        assert!(s.contains("默认=proxy"));
    }
}
