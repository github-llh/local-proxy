//! 核心配置模型。
//!
//! 语义移植自旧项目 `src/core/Config.hpp`：
//! - 所有非法值 warn + 回退默认，绝不因坏配置崩溃（fail-safe）
//! - 宽松反序列化：端口越界/枚举拼写错误均自动修正
//! - 单一事实来源：App（Tauri）与 core（CLI）共用本模型读写 config.json

use serde::{Deserialize, Serialize};

use localproxy_net::udp::resolve_udp_action;

// ============= 默认值 =============

pub const DEFAULT_LISTEN_HOST: &str = "127.0.0.1";
pub const DEFAULT_SOCKS5_PORT: u16 = 1080;
pub const DEFAULT_HTTP_PORT: u16 = 1081;
pub const DEFAULT_CONTROL_PORT: u16 = 1082;
pub const DEFAULT_UPSTREAM_TYPE: &str = "socks5";
pub const DEFAULT_UPSTREAM_HOST: &str = "127.0.0.1";
pub const DEFAULT_UPSTREAM_PORT: u16 = 7890;
pub const DEFAULT_LOG_LEVEL: &str = "info";
pub const DEFAULT_DNS_MODE: &str = "proxy";
pub const DEFAULT_UDP_MODE: &str = "auto";
pub const DEFAULT_UDP_FALLBACK: &str = "block";
pub const DEFAULT_IPV6_MODE: &str = "proxy";

// ============= 上游 =============

/// 上游代理服务器。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpstreamServer {
    pub id: String,
    pub name: String,
    /// `socks5` / `http`
    #[serde(rename = "type")]
    pub server_type: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

impl Default for UpstreamServer {
    fn default() -> Self {
        Self {
            id: "default".to_string(),
            name: "默认上游".to_string(),
            server_type: DEFAULT_UPSTREAM_TYPE.to_string(),
            host: DEFAULT_UPSTREAM_HOST.to_string(),
            port: DEFAULT_UPSTREAM_PORT,
            username: String::new(),
            password: String::new(),
        }
    }
}

impl UpstreamServer {
    /// 端点字符串 `host:port`。
    pub fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// 上游设置：当前激活项 + 全部候选。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpstreamSettings {
    /// 激活的上游 id
    pub active: String,
    pub servers: Vec<UpstreamServer>,
}

impl Default for UpstreamSettings {
    fn default() -> Self {
        Self {
            active: "default".to_string(),
            servers: vec![UpstreamServer::default()],
        }
    }
}

impl UpstreamSettings {
    /// 当前激活的上游（找不到时回退第一个）。
    pub fn active_server(&self) -> Option<&UpstreamServer> {
        self.servers
            .iter()
            .find(|s| s.id == self.active)
            .or_else(|| self.servers.first())
    }
}

// ============= 路由规则 =============

/// 单条路由规则（支持 IP/CIDR/域名通配符/端口/协议）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoutingRule {
    pub name: String,
    pub enabled: bool,
    /// `proxy` / `direct`
    pub action: String,
    /// priority_mode=number 时使用
    pub priority: i64,
    pub ip_cidrs_v4: Vec<String>,
    pub ip_cidrs_v6: Vec<String>,
    /// 支持通配符与后缀（`.example.com`）
    pub domains: Vec<String>,
    /// `80` / `443` / `10000-20000`
    pub ports: Vec<String>,
    /// `tcp` / `udp`
    pub protocols: Vec<String>,
}

impl Default for RoutingRule {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            action: "proxy".to_string(),
            priority: 0,
            ip_cidrs_v4: Vec::new(),
            ip_cidrs_v6: Vec::new(),
            domains: Vec::new(),
            ports: Vec::new(),
            protocols: Vec::new(),
        }
    }
}

/// 路由设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RoutingSettings {
    pub enabled: bool,
    /// `order`（按数组序）/ `number`（按 priority 降序）
    pub priority_mode: String,
    /// `proxy` / `direct`
    pub default_action: String,
    /// 自动注入内网直连规则
    pub use_default_private: bool,
    pub rules: Vec<RoutingRule>,
}

impl Default for RoutingSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            priority_mode: "order".to_string(),
            default_action: "proxy".to_string(),
            use_default_private: true,
            rules: Vec::new(),
        }
    }
}

// ============= 代理规则 =============

/// 代理行为规则（端口白名单 / DNS / IPv6 / UDP）。
///
/// 字段语义移植旧 `Core::ProxyRules`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyRules {
    /// 允许代理的目标端口白名单（空 = 全部）
    pub allowed_ports: Vec<u16>,
    /// `direct` / `proxy`
    pub dns_mode: String,
    /// `proxy` / `direct` / `block`
    pub ipv6_mode: String,
    /// `auto` / `block` / `direct` / `proxy`
    pub udp_mode: String,
    /// UDP 代理失败降级：`block` / `direct`
    pub udp_fallback: String,
    /// 路由规则
    pub routing: RoutingSettings,
}

impl Default for ProxyRules {
    fn default() -> Self {
        Self {
            allowed_ports: vec![80, 443],
            dns_mode: DEFAULT_DNS_MODE.to_string(),
            ipv6_mode: DEFAULT_IPV6_MODE.to_string(),
            udp_mode: DEFAULT_UDP_MODE.to_string(),
            udp_fallback: DEFAULT_UDP_FALLBACK.to_string(),
            routing: RoutingSettings::default(),
        }
    }
}

impl ProxyRules {
    /// 端口是否允许代理（空白名单 = 全部允许）。
    pub fn is_port_allowed(&self, port: u16) -> bool {
        self.allowed_ports.is_empty() || self.allowed_ports.contains(&port)
    }

    /// 依据当前上游类型解析有效 UDP 动作。
    pub fn effective_udp_action(&self, upstream_type: &str) -> localproxy_net::udp::UdpAction {
        resolve_udp_action(&self.udp_mode, upstream_type, &self.udp_fallback)
    }
}

// ============= UI =============

/// 界面设置（App 使用；core 只透传不解释）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    /// `zh-CN` / `en`
    pub language: String,
    /// 开机自启
    pub launch_at_login: bool,
    /// App 退出后核心是否继续常驻
    pub keep_core_alive: bool,
    /// 出口 IP 探测（默认关闭，避免无谓联网）
    pub exit_ip_probe: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            language: "zh-CN".to_string(),
            launch_at_login: false,
            keep_core_alive: false,
            exit_ip_probe: false,
        }
    }
}

// ============= 顶层 =============

/// 完整配置文件模型（`config.json`）。
///
/// 端口等字段使用宽松反序列化：越界/类型不符时回退默认，
/// 与旧 `Config::Load` 的 warn+fallback 行为一致。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub listen_host: String,
    pub socks5_port: u16,
    pub http_port: u16,
    pub control_port: u16,
    pub control_token: String,
    pub log_level: String,
    pub upstream: UpstreamSettings,
    pub routing: RoutingSettings,
    pub dns_mode: String,
    pub proxy_rules: ProxyRules,
    pub apps: Vec<localproxy_launcher::app_entry::AppEntry>,
    pub ui: UiSettings,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            listen_host: DEFAULT_LISTEN_HOST.to_string(),
            socks5_port: DEFAULT_SOCKS5_PORT,
            http_port: DEFAULT_HTTP_PORT,
            control_port: DEFAULT_CONTROL_PORT,
            control_token: String::new(),
            log_level: DEFAULT_LOG_LEVEL.to_string(),
            upstream: UpstreamSettings::default(),
            routing: RoutingSettings::default(),
            dns_mode: DEFAULT_DNS_MODE.to_string(),
            proxy_rules: ProxyRules::default(),
            apps: Vec::new(),
            ui: UiSettings::default(),
        }
    }
}

impl AppConfig {
    /// 从 JSON 字符串解析并 sanitize。
    pub fn from_json_str(text: &str) -> Result<Self, String> {
        let mut config: Self =
            serde_json::from_str(text).map_err(|e| format!("JSON 解析失败: {e}"))?;
        config.sanitize();
        Ok(config)
    }

    /// 从文件加载。
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("读取配置失败 ({}): {e}", path.display()))?;
        Self::from_json_str(&text)
    }

    /// 序列化为格式化 JSON。
    pub fn to_json_pretty(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| format!("序列化配置失败: {e}"))
    }

    /// 原子写入（临时文件 + rename）。
    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建配置目录失败: {e}"))?;
        }
        let text = self.to_json_pretty()?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &text).map_err(|e| format!("写临时配置失败: {e}"))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("替换配置失败: {e}"))
    }

    /// 生成随机 control token（32 hex）。
    pub fn ensure_control_token(&mut self) -> &str {
        if self.control_token.trim().is_empty() {
            self.control_token = generate_token();
        }
        &self.control_token
    }

    /// 规范化所有字段（非法值回退默认）。
    pub fn sanitize(&mut self) {
        let mut warnings: Vec<String> = Vec::new();

        // ---- 端口 ----
        for (name, port) in [
            ("socks5_port", &mut self.socks5_port),
            ("http_port", &mut self.http_port),
            ("control_port", &mut self.control_port),
        ] {
            if *port == 0 {
                warnings.push(format!("配置: {name} 为 0，已回退默认"));
                *port = match name {
                    "socks5_port" => DEFAULT_SOCKS5_PORT,
                    "http_port" => DEFAULT_HTTP_PORT,
                    _ => DEFAULT_CONTROL_PORT,
                };
            }
        }
        if self.socks5_port == self.http_port
            || self.socks5_port == self.control_port
            || self.http_port == self.control_port
        {
            warnings.push("配置: 端口互相冲突，http/control 已顺延".to_string());
            self.http_port = self.socks5_port.wrapping_add(1);
            self.control_port = self.socks5_port.wrapping_add(2);
        }

        // ---- 监听地址 ----
        if self.listen_host.trim().is_empty() {
            warnings.push("配置: listen_host 为空，已回退 127.0.0.1".to_string());
            self.listen_host = DEFAULT_LISTEN_HOST.to_string();
        }

        // ---- 日志级别 ----
        self.log_level = self.log_level.trim().to_ascii_lowercase();
        if !["error", "warn", "info", "debug", "trace"]
            .iter()
            .any(|level| *level == self.log_level)
        {
            warnings.push(format!(
                "配置: log_level 无效({})，已回退 info",
                self.log_level
            ));
            self.log_level = DEFAULT_LOG_LEVEL.to_string();
        }

        // ---- 上游 ----
        if self.upstream.servers.is_empty() {
            warnings.push("配置: 无上游服务器，已注入默认".to_string());
            self.upstream.servers.push(UpstreamServer::default());
        }
        for server in &mut self.upstream.servers {
            if server.id.trim().is_empty() {
                server.id = format!("upstream-{}", server.endpoint());
            }
            server.server_type = server.server_type.trim().to_ascii_lowercase();
            // 常见误写：https 指 HTTP CONNECT
            if server.server_type == "https" {
                warnings.push(format!(
                    "配置: 上游 {}({}) type=https 按 http(CONNECT) 处理",
                    server.name, server.id
                ));
                server.server_type = "http".to_string();
            }
            if server.server_type != "socks5" && server.server_type != "http" {
                warnings.push(format!(
                    "配置: 上游 {}({}) type 无效({})，已回退 socks5",
                    server.name, server.id, server.server_type
                ));
                server.server_type = DEFAULT_UPSTREAM_TYPE.to_string();
            }
            if server.host.trim().is_empty() {
                server.host = DEFAULT_UPSTREAM_HOST.to_string();
            }
            if server.port == 0 {
                server.port = DEFAULT_UPSTREAM_PORT;
            }
        }
        if self.upstream.active.trim().is_empty()
            || !self
                .upstream
                .servers
                .iter()
                .any(|s| s.id == self.upstream.active)
        {
            warnings.push("配置: upstream.active 无效，已回退第一个上游".to_string());
            self.upstream.active = self.upstream.servers[0].id.clone();
        }

        // ---- 路由 ----
        self.routing.priority_mode = self.routing.priority_mode.trim().to_ascii_lowercase();
        if self.routing.priority_mode != "order" && self.routing.priority_mode != "number" {
            warnings.push(format!(
                "配置: routing.priority_mode 无效({})，已回退 order",
                self.routing.priority_mode
            ));
            self.routing.priority_mode = "order".to_string();
        }
        self.routing.default_action = self.routing.default_action.trim().to_ascii_lowercase();
        if self.routing.default_action != "proxy" && self.routing.default_action != "direct" {
            warnings.push(format!(
                "配置: routing.default_action 无效({})，已回退 proxy",
                self.routing.default_action
            ));
            self.routing.default_action = "proxy".to_string();
        }
        for rule in &mut self.routing.rules {
            rule.action = rule.action.trim().to_ascii_lowercase();
            if rule.action != "proxy" && rule.action != "direct" {
                warnings.push(format!(
                    "配置: 路由规则 action 无效({}), rule={}",
                    rule.action,
                    if rule.name.is_empty() {
                        "(unnamed)"
                    } else {
                        &rule.name
                    }
                ));
                rule.action = self.routing.default_action.clone();
            }
            if rule.name.trim().is_empty() {
                rule.name = "(unnamed)".to_string();
            }
        }

        // ---- 代理规则 ----
        let rules = &mut self.proxy_rules;
        rules.allowed_ports.sort_unstable();
        rules.allowed_ports.dedup();
        rules.dns_mode = rules.dns_mode.trim().to_ascii_lowercase();
        if rules.dns_mode != "direct" && rules.dns_mode != "proxy" {
            warnings.push(format!(
                "配置: proxy_rules.dns_mode 无效({})，已回退 proxy",
                rules.dns_mode
            ));
            rules.dns_mode = DEFAULT_DNS_MODE.to_string();
        }
        rules.ipv6_mode = rules.ipv6_mode.trim().to_ascii_lowercase();
        if rules.ipv6_mode != "proxy" && rules.ipv6_mode != "direct" && rules.ipv6_mode != "block" {
            warnings.push(format!(
                "配置: proxy_rules.ipv6_mode 无效({})，已回退 proxy",
                rules.ipv6_mode
            ));
            rules.ipv6_mode = DEFAULT_IPV6_MODE.to_string();
        }
        rules.udp_mode = rules.udp_mode.trim().to_ascii_lowercase();
        if !["auto", "block", "direct", "proxy"].contains(&rules.udp_mode.as_str()) {
            warnings.push(format!(
                "配置: proxy_rules.udp_mode 无效({})，已回退 auto",
                rules.udp_mode
            ));
            rules.udp_mode = DEFAULT_UDP_MODE.to_string();
        }
        rules.udp_fallback = rules.udp_fallback.trim().to_ascii_lowercase();
        if rules.udp_fallback != "block" && rules.udp_fallback != "direct" {
            warnings.push(format!(
                "配置: proxy_rules.udp_fallback 无效({})，已回退 block",
                rules.udp_fallback
            ));
            rules.udp_fallback = DEFAULT_UDP_FALLBACK.to_string();
        }

        // ---- DNS ----
        self.dns_mode = self.dns_mode.trim().to_ascii_lowercase();
        if self.dns_mode != "proxy" && self.dns_mode != "direct" {
            warnings.push(format!(
                "配置: dns_mode 无效({})，已回退 proxy",
                self.dns_mode
            ));
            self.dns_mode = DEFAULT_DNS_MODE.to_string();
        }

        if !warnings.is_empty() {
            for w in &warnings {
                tracing::warn!("{w}");
            }
        }
    }

    /// SOCKS5 端点字符串。
    pub fn socks5_endpoint(&self) -> String {
        format!("{}:{}", self.listen_host, self.socks5_port)
    }

    /// 控制面端点字符串。
    pub fn control_endpoint(&self) -> String {
        format!("{}:{}", self.listen_host, self.control_port)
    }
}

/// 生成随机 hex token（时间熵 + 栈地址熵，非密码学强度但足够本机鉴权）。
pub fn generate_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let entropy = &nanos as *const u128 as usize;
    let mut state = nanos as u64 ^ (entropy as u64).rotate_left(32);
    let mut out = String::with_capacity(32);
    for _ in 0..32 {
        // xorshift64*
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let v = state.wrapping_mul(0x2545F4914F6CDD1D);
        out.push(char::from_digit((v >> 60) as u32 & 0xF, 16).unwrap());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.socks5_port, 1080);
        assert_eq!(cfg.http_port, 1081);
        assert_eq!(cfg.control_port, 1082);
        assert_eq!(cfg.listen_host, "127.0.0.1");
        assert_eq!(cfg.proxy_rules.allowed_ports, vec![80, 443]);
        assert_eq!(cfg.upstream.active, "default");
        assert!(cfg.upstream.active_server().is_some());
    }

    #[test]
    fn port_conflict_resolves() {
        let mut cfg = AppConfig {
            socks5_port: 1080,
            http_port: 1080,
            control_port: 1080,
            ..AppConfig::default()
        };
        cfg.sanitize();
        assert_ne!(cfg.http_port, cfg.socks5_port);
        assert_ne!(cfg.control_port, cfg.socks5_port);
        assert_ne!(cfg.http_port, cfg.control_port);
    }

    #[test]
    fn invalid_enums_fall_back() {
        let json = r#"{
            "log_level": "LOUD",
            "dns_mode": "wire",
            "proxy_rules": { "udp_mode": "sometimes", "ipv6_mode": "maybe", "udp_fallback": "panic" },
            "upstream": { "active": "nope", "servers": [{ "id": "a", "type": "socks6", "port": 0 }] }
        }"#;
        let mut cfg = AppConfig::from_json_str(json).unwrap();
        cfg.sanitize();
        assert_eq!(cfg.log_level, "info");
        assert_eq!(cfg.dns_mode, "proxy");
        assert_eq!(cfg.proxy_rules.udp_mode, "auto");
        assert_eq!(cfg.proxy_rules.ipv6_mode, "proxy");
        assert_eq!(cfg.proxy_rules.udp_fallback, "block");
        assert_eq!(cfg.upstream.servers[0].server_type, "socks5");
        assert_eq!(cfg.upstream.servers[0].port, 7890);
        assert_eq!(cfg.upstream.active, "a"); // 无效 active 回退第一个
    }

    #[test]
    fn https_upstream_treated_as_http() {
        let json = r#"{"upstream": {"servers": [{"id":"x","type":"https"}]}}"#;
        let mut cfg = AppConfig::from_json_str(json).unwrap();
        cfg.sanitize();
        assert_eq!(cfg.upstream.servers[0].server_type, "http");
    }

    #[test]
    fn roundtrip_save_load() {
        let dir = std::env::temp_dir().join(format!("localproxy-test-{}", generate_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let mut cfg = AppConfig::default();
        cfg.ensure_control_token();
        cfg.save(&path).unwrap();
        let loaded = AppConfig::load(&path).unwrap();
        assert_eq!(cfg, loaded);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn allowed_ports_sorted_deduped() {
        let json = r#"{"proxy_rules": {"allowed_ports": [443, 80, 443, 8080]}}"#;
        let mut cfg = AppConfig::from_json_str(json).unwrap();
        cfg.sanitize();
        assert_eq!(cfg.proxy_rules.allowed_ports, vec![80, 443, 8080]);
        assert!(cfg.proxy_rules.is_port_allowed(443));
        assert!(!cfg.proxy_rules.is_port_allowed(8443));
    }

    #[test]
    fn control_token_generation() {
        let mut cfg = AppConfig::default();
        assert!(cfg.control_token.is_empty());
        let token = cfg.ensure_control_token().to_string();
        assert_eq!(token.len(), 32);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        // 幂等
        assert_eq!(cfg.ensure_control_token(), token);
    }

    #[test]
    fn effective_udp_action_follows_upstream_type() {
        let cfg = AppConfig::default(); // udp_mode=auto
        assert_eq!(
            cfg.proxy_rules.effective_udp_action("socks5"),
            localproxy_net::udp::UdpAction::Proxy
        );
        assert_eq!(
            cfg.proxy_rules.effective_udp_action("http"),
            localproxy_net::udp::UdpAction::Block
        );
    }
}
