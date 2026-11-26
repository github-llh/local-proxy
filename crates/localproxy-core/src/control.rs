//! 本地控制面 HTTP 服务（仅监听 127.0.0.1，Bearer token 鉴权）。
//!
//! 端点：
//! - `GET  /v1/health`               核心健康状态
//! - `GET  /v1/stats`                流量统计快照
//! - `GET  /v1/connections?limit=N`  最近连接记录（诊断日志）
//! - `POST /v1/config/reload`        重新加载配置并重建规则
//!
//! 手写 HTTP/1.1（无第三方依赖）：一次连接一个请求，Connection: close。

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::{
    config::AppConfig,
    server::{RuntimeSettings, ServerContext},
};

/// 运行控制面服务（阻塞直到 accept 失败）。
pub async fn run(
    listener: TcpListener,
    ctx: Arc<ServerContext>,
    config_path: Option<PathBuf>,
) -> std::io::Result<()> {
    let config_path = config_path.map(Arc::new);
    loop {
        let (stream, peer) = listener.accept().await?;
        let ctx = ctx.clone();
        let config_path = config_path.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, &ctx, config_path.as_deref().map(PathBuf::as_path)).await
            {
                tracing::debug!("控制面连接 {peer} 结束: {e}");
            }
        });
    }
}

/// 处理单个控制面请求。
async fn handle(
    mut stream: TcpStream,
    ctx: &ServerContext,
    config_path: Option<&Path>,
) -> std::io::Result<()> {
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
    let token = ctx.settings.read().await.config.control_token.clone();
    if !token.trim().is_empty() {
        let authorized = lines.take_while(|line| !line.is_empty()).any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("authorization")
                    && value.trim().split_once(' ').is_some_and(|(scheme, value)| {
                        scheme.eq_ignore_ascii_case("bearer") && value == token.trim()
                    })
            })
        });
        if !authorized {
            return respond(&mut stream, 401, "{\"error\":\"unauthorized\"}").await;
        }
    }

    let (status, body) = match (method, path) {
        ("GET", "/v1/health") => {
            let settings = ctx.settings.read().await;
            let cfg = &settings.config;
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
        ("POST", "/v1/config/reload") => match reload(ctx, config_path).await {
            Ok(id) => (
                200,
                serde_json::json!({ "reloaded": true, "active_upstream": id }).to_string(),
            ),
            Err(error) => (409, serde_json::json!({ "error": error }).to_string()),
        },
        _ => (404, "{\"error\":\"not found\"}".to_string()),
    };
    respond(&mut stream, status, &body).await
}

/// 校验完成后一次替换配置和规则，运行中的监听和已建立的连接继续使用原配置。
async fn reload(ctx: &ServerContext, path: Option<&Path>) -> Result<String, String> {
    let path = path.ok_or("热重载需要使用 --config 启动核心")?;
    let _reload = ctx.reload_lock.lock().await;
    let previous = ctx.settings.read().await.clone();
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| format!("读取热重载配置失败: {e}"))?;
    let mut config = AppConfig::from_json_str(&text)?;
    config.use_running_listeners(&previous.config);
    config.validate()?;
    let id = config.upstream.active.clone();
    let settings = RuntimeSettings::prepare(config, Some(&previous)).await?;
    *ctx.settings.write().await = Arc::new(settings);
    Ok(id)
}

/// 发送 JSON 响应。
async fn respond(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await
}
