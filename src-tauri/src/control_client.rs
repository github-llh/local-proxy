//! 本地核心控制面 HTTP 客户端。
//!
//! App 通过它读取核心的连接诊断日志与统计（仅本机 127.0.0.1，
//! Bearer token 鉴权）。手写 HTTP/1.1（无第三方依赖）。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use localproxy_core::diagnostics::ConnectionRecord;

const CONTROL_TIMEOUT: Duration = Duration::from_secs(6);

/// 控制面客户端。
pub struct ControlClient {
    endpoint: String,
    token: String,
}

/// 控制面响应解析错误。
#[derive(Debug)]
pub enum ControlError {
    /// 连接失败（核心可能未运行）
    Unavailable(String),
    /// HTTP 状态非 200
    Status(u16, String),
    /// 响应解析失败
    Parse(String),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlError::Unavailable(m) => write!(f, "控制面不可达: {m}"),
            ControlError::Status(code, body) => write!(f, "控制面返回 {code}: {body}"),
            ControlError::Parse(m) => write!(f, "响应解析失败: {m}"),
        }
    }
}

// stats_json/health 为完整 API 面保留（popover 后续消费）
#[allow(dead_code)]
impl ControlClient {
    pub fn new(endpoint: String, token: String) -> Self {
        Self { endpoint, token }
    }

    fn request(&self, method: &str, path: &str) -> Result<(u16, String), ControlError> {
        let endpoint: SocketAddr = self
            .endpoint
            .parse::<SocketAddr>()
            .map_err(|e| ControlError::Unavailable(e.to_string()))?;
        let mut stream = TcpStream::connect_timeout(&endpoint, CONTROL_TIMEOUT)
            .map_err(|e| ControlError::Unavailable(e.to_string()))?;
        stream
            .set_read_timeout(Some(CONTROL_TIMEOUT))
            .and_then(|_| stream.set_write_timeout(Some(CONTROL_TIMEOUT)))
            .map_err(|e| ControlError::Unavailable(e.to_string()))?;

        let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {}\r\n", self.endpoint);
        if !self.token.trim().is_empty() {
            request.push_str(&format!("Authorization: Bearer {}\r\n", self.token.trim()));
        }
        request.push_str("Content-Length: 0\r\nConnection: close\r\n\r\n");
        stream
            .write_all(request.as_bytes())
            .map_err(|e| ControlError::Unavailable(e.to_string()))?;

        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .map_err(|e| ControlError::Unavailable(e.to_string()))?;

        // 状态码 + body（body 为独立 String，避免临时值生命周期问题）
        let body = response.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        let status = response
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| ControlError::Parse("无法解析状态行".to_string()))?;
        Ok((status, body))
    }

    pub fn reload(&self, expected_upstream: &str) -> Result<(), ControlError> {
        #[derive(serde::Deserialize)]
        struct ReloadResponse {
            reloaded: bool,
            active_upstream: String,
        }

        let (status, body) = self.request("POST", "/v1/config/reload")?;
        if status != 200 {
            return Err(ControlError::Status(status, body));
        }
        let response: ReloadResponse = serde_json::from_str(&body)
            .map_err(|_| ControlError::Parse("核心未确认生效的上游，请检查核心版本".to_string()))?;
        if !response.reloaded || response.active_upstream != expected_upstream {
            return Err(ControlError::Parse(
                "核心返回的激活上游与配置不一致".to_string(),
            ));
        }
        Ok(())
    }

    /// 获取最近连接记录。
    pub fn connections(&self, limit: usize) -> Result<Vec<ConnectionRecord>, ControlError> {
        let (status, body) = self.request("GET", &format!("/v1/connections?limit={limit}"))?;
        if status != 200 {
            return Err(ControlError::Status(status, body));
        }
        serde_json::from_str(&body).map_err(|e| ControlError::Parse(e.to_string()))
    }

    /// 获取统计快照 JSON。
    pub fn stats_json(&self) -> Result<String, ControlError> {
        let (status, body) = self.request("GET", "/v1/stats")?;
        if status != 200 {
            return Err(ControlError::Status(status, body));
        }
        Ok(body)
    }

    /// 健康检查。
    pub fn health(&self) -> bool {
        matches!(self.request("GET", "/v1/health"), Ok((200, _)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_construction() {
        let client = ControlClient::new("127.0.0.1:11082".to_string(), "tok".to_string());
        assert_eq!(client.endpoint, "127.0.0.1:11082");
        assert_eq!(client.token, "tok");
    }

    #[test]
    fn unavailable_when_core_down() {
        // 未占用端口 → 连接失败
        let client = ControlClient::new("127.0.0.1:1".to_string(), String::new());
        let err = client.health();
        assert!(!err, "核心未运行时应不可达");
        assert!(client.connections(10).is_err());
    }
}
