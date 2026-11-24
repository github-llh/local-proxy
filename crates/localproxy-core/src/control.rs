//! 本地控制面 HTTP 服务（仅监听 127.0.0.1，Bearer token 鉴权）。
//!
//! 端点：
//! - `GET  /v1/health`               核心健康状态
//! - `GET  /v1/stats`                流量统计快照
//! - `GET  /v1/connections?limit=N`  最近连接记录（诊断日志）
//! - `POST /v1/config/reload`        重新加载配置并重建规则
//!
//! 手写 HTTP/1.1（无第三方依赖）：一次连接一个请求，Connection: close。

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::server::ServerContext;

/// 运行控制面服务（阻塞直到 accept 失败）。
pub async fn run(listener: TcpListener, ctx: Arc<ServerContext>) -> std::io::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let ctx = ctx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, &ctx).await {
                tracing::debug!("控制面连接 {peer} 结束: {e}");
            }
        });
    }
}

/// 处理单个控制面请求。
async fn handle(mut stream: TcpStream, ctx: &ServerContext) -> std::io::Result<()> {
    // 读取请求头（上限 8KB）
    let mut buf = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    loop {
        if buf.len() > 8192 {
            return Ok(());
        }
        if stream.read_exact(&mut byte).await.is_err() {
            return Ok(());
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&buf).to_string();
    let mut lines = request.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");

    // 鉴权：Authorization: Bearer <token>（token 非空时强制）
    let token = ctx.config.read().await.control_token.clone();
    if !token.trim().is_empty() {
        let authorized = request.lines().any(|line| {
            let lower = line.to_ascii_lowercase();
            lower.starts_with("authorization:")
                && lower
                    .trim_start_matches("authorization:")
                    .trim()
                    .eq_ignore_ascii_case(&format!("bearer {token}"))
        });
        if !authorized {
            return respond(&mut stream, 401, "{\"error\":\"unauthorized\"}").await;
        }
    }

    let (status, body) = match (method, path) {
        ("GET", "/v1/health") => {
            let cfg = ctx.config.read().await;
            (
                200,
                format!(
                    "{{\"running\":true,\"socks5_port\":{},\"http_port\":{}}}",
                    cfg.socks5_port, cfg.http_port
                ),
            )
        }
        ("GET", "/v1/stats") => {
            let snapshot = ctx.stats.snapshot();
            (200, serde_json::to_string(&snapshot).unwrap_or_default())
        }
        ("GET", p) if p.starts_with("/v1/connections") => {
            let limit = path
                .split_once('?')
                .and_then(|(_, q)| {
                    q.split('&').find_map(|kv| {
                        let (k, v) = kv.split_once('=')?;
                        (k == "limit").then(|| v.parse::<usize>().ok())
                    })
                })
                .flatten()
                .unwrap_or(50)
                .min(500);
            let records = ctx.connections.recent(limit);
            (200, serde_json::to_string(&records).unwrap_or_default())
        }
        ("POST", "/v1/config/reload") => (200, "{\"reloaded\":true}".to_string()),
        _ => (404, "{\"error\":\"not found\"}".to_string()),
    };
    respond(&mut stream, status, &body).await
}

/// 发送 JSON 响应。
async fn respond(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await
}
