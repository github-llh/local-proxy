//! 出口 IP 探测（风控自检）。
//!
//! 经本地核心 SOCKS5 → 上游代理访问 HTTP 明文 IP 查询端点，
//! 解析 JSON 得到出口 IP / 国家 / 城市 / ISP。
//!
//! 隐私：仅在使用者显式开启（`ui.exit_ip_probe`）并点击「探测」时发起；
//! 使用 HTTP 明文端点（避免 TLS 依赖），请求经上游出口——探测结果反映的
//! 就是被代理应用看到的出口身份。

use serde::Serialize;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use localproxy_net::socks5::{self, ProxyAuth, Timeouts};

/// 出口 IP 信息。
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ExitIpInfo {
    pub ip: String,
    pub country_code: String,
    pub country: String,
    pub city: String,
    pub isp: String,
}

/// 探测错误。
#[derive(Debug)]
pub enum ProbeError {
    /// 配置/参数问题
    Config(String),
    /// 网络失败
    Network(String),
    /// 响应解析失败
    Parse(String),
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeError::Config(m) => write!(f, "配置错误: {m}"),
            ProbeError::Network(m) => write!(f, "网络失败: {m}"),
            ProbeError::Parse(m) => write!(f, "响应解析失败: {m}"),
        }
    }
}

/// 从端点 URL 解析 (host, port, path)。
fn parse_http_url(url: &str) -> Result<(String, u16, String), ProbeError> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| ProbeError::Config(format!("仅支持 http:// 端点: {url}")))?;
    let (host_port, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => {
            let port: u16 = p
                .parse()
                .map_err(|_| ProbeError::Config(format!("端点端口无效: {url}")))?;
            (h.to_string(), port)
        }
        None => (host_port.to_string(), 80),
    };
    if host.is_empty() {
        return Err(ProbeError::Config(format!("端点缺少主机: {url}")));
    }
    Ok((host, port, path.to_string()))
}

/// 解析 IP 查询端点的 JSON 响应体。
///
/// 兼容 ip-api.com（`query`/`countryCode`/`country`/`city`/`isp`）与
/// 纯文本 IP 响应（members.3322.org 等）。
pub fn parse_exit_ip_response(body: &str) -> Result<ExitIpInfo, ProbeError> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(ProbeError::Parse("响应为空".to_string()));
    }
    // JSON 路径
    if trimmed.starts_with('{') {
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| ProbeError::Parse(format!("JSON 解析失败: {e}")))?;
        let field = |key: &str| -> String {
            value
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let info = ExitIpInfo {
            ip: field("query"),
            country_code: field("countryCode"),
            country: field("country"),
            city: field("city"),
            isp: field("isp"),
        };
        if info.ip.is_empty() {
            return Err(ProbeError::Parse("响应缺少 query 字段".to_string()));
        }
        return Ok(info);
    }
    // 纯文本 IP（首行）
    let first = trimmed.lines().next().unwrap_or("").trim();
    if first.parse::<std::net::IpAddr>().is_ok() {
        return Ok(ExitIpInfo {
            ip: first.to_string(),
            ..Default::default()
        });
    }
    Err(ProbeError::Parse(format!(
        "无法识别的响应: {}",
        &trimmed[..trimmed.len().min(64)]
    )))
}

/// 执行探测：经核心 SOCKS5 访问第一个可用端点。
pub fn probe_exit_ip(
    core_endpoint: &str,
    endpoints: &[String],
    timeout_ms: u64,
) -> Result<ExitIpInfo, ProbeError> {
    let mut last_err: Option<ProbeError> = None;
    for url in endpoints {
        match probe_one(core_endpoint, url, timeout_ms) {
            Ok(info) => return Ok(info),
            Err(e) => {
                tracing::debug!("探测端点 {url} 失败: {e}");
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| ProbeError::Config("未配置探测端点".to_string())))
}

/// 单端点探测。
fn probe_one(core_endpoint: &str, url: &str, timeout_ms: u64) -> Result<ExitIpInfo, ProbeError> {
    let (host, port, path) = parse_http_url(url)?;

    // 经核心 SOCKS5 建立到端点的隧道（上游出口）
    let timeouts = Timeouts {
        connect_ms: timeout_ms as i64,
        send_ms: timeout_ms as i64,
        recv_ms: timeout_ms as i64,
    };
    let mut stream: TcpStream =
        socks5::connect(core_endpoint, &host, port, &ProxyAuth::default(), &timeouts)
            .map_err(|e| ProbeError::Network(e.to_string()))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(timeout_ms)))
        .map_err(|e| ProbeError::Network(e.to_string()))?;

    // HTTP/1.1 请求（Connection: close，一次性读取）
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUser-Agent: LocalProxy/0.1\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| ProbeError::Network(format!("发送请求失败: {e}")))?;

    // 读取完整响应（close 语义）
    let mut response = Vec::with_capacity(2048);
    stream
        .read_to_end(&mut response)
        .map_err(|e| ProbeError::Network(format!("读取响应失败: {e}")))?;
    let text = String::from_utf8_lossy(&response).to_string();

    // 分离 body（\r\n\r\n 之后）
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").trim();
    // chunked 兜底：截取首个 '{' 到末个 '}' 之间的 JSON 段（无则用原 body）
    let body = match (body.find('{'), body.rfind('}')) {
        (Some(s), Some(e)) if s < e => &body[s..=e],
        _ => body,
    };
    parse_exit_ip_response(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_http_url_variants() {
        assert_eq!(
            parse_http_url("http://ip-api.com/json?lang=zh-CN").unwrap(),
            ("ip-api.com".to_string(), 80, "/json?lang=zh-CN".to_string())
        );
        assert_eq!(
            parse_http_url("http://127.0.0.1:8080/api").unwrap(),
            ("127.0.0.1".to_string(), 8080, "/api".to_string())
        );
        assert!(parse_http_url("https://example.com").is_err(), "仅 HTTP");
        assert!(parse_http_url("http://").is_err(), "缺主机");
    }

    #[test]
    fn parse_ip_api_json() {
        let body = r#"{"query":"1.2.3.4","countryCode":"JP","country":"Japan","city":"Tokyo","isp":"Example ISP"}"#;
        let info = parse_exit_ip_response(body).unwrap();
        assert_eq!(info.ip, "1.2.3.4");
        assert_eq!(info.country_code, "JP");
        assert_eq!(info.country, "Japan");
        assert_eq!(info.city, "Tokyo");
        assert_eq!(info.isp, "Example ISP");
    }

    #[test]
    fn parse_plain_text_ip() {
        let info = parse_exit_ip_response("1.2.3.4\n").unwrap();
        assert_eq!(info.ip, "1.2.3.4");
        assert!(info.country_code.is_empty());
    }

    #[test]
    fn parse_failures() {
        assert!(parse_exit_ip_response("").is_err());
        assert!(parse_exit_ip_response("not-an-ip").is_err());
        assert!(parse_exit_ip_response("{broken").is_err());
        assert!(parse_exit_ip_response(r#"{"no":"query"}"#).is_err());
    }

    #[test]
    fn probe_uses_first_success_endpoint() {
        // 无网络/无核心时全部失败 → 返回最后一个错误
        let err = probe_exit_ip("127.0.0.1:1", &["http://127.0.0.1:1/x".to_string()], 200);
        assert!(err.is_err());
    }
}
