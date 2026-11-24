//! SOCKS5 / HTTP CONNECT 代理服务（tokio）。
//!
//! 入站协议：
//! - SOCKS5（`socks5` 端口）：方法协商（仅本机监听，无认证）→ CONNECT 请求
//! - HTTP CONNECT（`http` 端口）：`CONNECT host:port HTTP/1.1`
//!
//! 分流：`RuleEngine::decide` → `Direct`（本地拨号）/ `Proxy`（上游级联）。
//! 拒绝时返回对应协议的错误响应（SOCKS5 REPLY 码 / HTTP 状态码）。

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use localproxy_net::socks5::{consts, reply_to_text, Timeouts};

use crate::config::AppConfig;
use crate::diagnostics::{self, ConnectionLog, ConnectionRecord};
use crate::rules::{Action, RuleEngine};
use crate::stats::Stats;
use crate::upstream::{Upstream, UpstreamError};

/// 服务共享上下文。
pub struct ServerContext {
    /// 配置（可变；控制面热重载时替换）
    pub config: Arc<tokio::sync::RwLock<AppConfig>>,
    /// 规则引擎（配置变更时重建）
    pub rules: Arc<RuleEngine>,
    /// 统计
    pub stats: Arc<Stats>,
    /// 连接诊断日志（环形缓冲）
    pub connections: Arc<ConnectionLog>,
}

impl ServerContext {
    /// 由配置构造。
    pub fn new(config: AppConfig) -> Self {
        let rules = RuleEngine::compile(&config.routing);
        Self {
            config: Arc::new(tokio::sync::RwLock::new(config)),
            rules: Arc::new(rules),
            stats: Arc::new(Stats::new()),
            connections: Arc::new(ConnectionLog::with_default_capacity()),
        }
    }

    /// 记录一条连接（诊断日志）。
    #[allow(clippy::too_many_arguments)]
    pub fn record_connection(
        &self,
        protocol: &'static str,
        host: &str,
        port: u16,
        action: &'static str,
        rule: &str,
        result: &'static str,
        error: &str,
    ) {
        self.connections.record(ConnectionRecord {
            ts_ms: diagnostics::now_ms(),
            protocol: protocol.to_string(),
            host: host.to_string(),
            port,
            action: action.to_string(),
            rule: rule.to_string(),
            result: result.to_string(),
            error: error.to_string(),
        });
    }

    /// 当前超时配置（M3 起从 config 的 timeout 节读取）。
    pub fn timeouts() -> Timeouts {
        Timeouts {
            connect_ms: 5000,
            send_ms: 5000,
            recv_ms: 5000,
        }
    }
}

/// 分流错误 → SOCKS5 REPLY 码。
fn dispatch_error_to_reply(err: &DispatchError) -> u8 {
    match err {
        DispatchError::Refused => 0x05,     // 连接被拒绝
        DispatchError::Unreachable => 0x03, // 网络不可达
        DispatchError::Timeout => 0x04,     // 主机不可达
        DispatchError::NotAllowed => 0x02,  // 规则不允许
        DispatchError::Protocol(_) | DispatchError::Io(_) => 0x01, // 通用失败
    }
}

/// 分流错误。
#[derive(Debug)]
pub enum DispatchError {
    Refused,
    Unreachable,
    Timeout,
    NotAllowed,
    Protocol(String),
    Io(String),
}

impl std::fmt::Display for DispatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DispatchError::Refused => write!(f, "连接被拒绝"),
            DispatchError::Unreachable => write!(f, "网络不可达"),
            DispatchError::Timeout => write!(f, "连接超时"),
            DispatchError::NotAllowed => write!(f, "规则不允许"),
            DispatchError::Protocol(m) => write!(f, "协议错误: {m}"),
            DispatchError::Io(m) => write!(f, "IO 错误: {m}"),
        }
    }
}

impl From<UpstreamError> for DispatchError {
    fn from(e: UpstreamError) -> Self {
        match e {
            UpstreamError::Io(msg) => {
                if msg.contains("超时") {
                    DispatchError::Timeout
                } else {
                    DispatchError::Io(msg)
                }
            }
            UpstreamError::Reply(rep) => match rep {
                0x05 => DispatchError::Refused,
                0x03 => DispatchError::Unreachable,
                0x04 => DispatchError::Timeout,
                0x02 => DispatchError::NotAllowed,
                _ => DispatchError::Protocol(format!("REP={}", rep)),
            },
            UpstreamError::Protocol(msg) => DispatchError::Protocol(msg),
            UpstreamError::Auth(msg) => {
                tracing::debug!("上游认证失败: {msg}");
                DispatchError::NotAllowed
            }
            UpstreamError::InvalidTarget(msg) => DispatchError::Protocol(msg),
        }
    }
}

impl ServerContext {
    /// 分流到上游或直连。
    pub async fn dispatch(
        &self,
        host: &str,
        port: u16,
    ) -> Result<(TcpStream, Action, String), DispatchError> {
        // 端口白名单（空 = 全部）
        {
            let cfg = self.config.read().await;
            if !cfg.proxy_rules.is_port_allowed(port) {
                return Err(DispatchError::NotAllowed);
            }
        }

        let decision = self.rules.decide(host, None, port, "tcp");
        match decision.action {
            Action::Direct => {
                self.stats.on_routed_direct();
                let stream = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    TcpStream::connect((host, port)),
                )
                .await
                .map_err(|_| DispatchError::Timeout)?
                .map_err(|e| {
                    let msg = e.to_string();
                    if msg.contains("refused") {
                        DispatchError::Refused
                    } else {
                        DispatchError::Io(msg)
                    }
                })?;
                Ok((stream, Action::Direct, decision.rule))
            }
            Action::Proxy => {
                self.stats.on_routed_proxy();
                let (upstream_server, timeouts) = {
                    let cfg = self.config.read().await;
                    match cfg.upstream.active_server() {
                        Some(server) => (
                            server.clone(),
                            Timeouts {
                                connect_ms: 5000,
                                send_ms: 5000,
                                recv_ms: 5000,
                            },
                        ),
                        None => return Err(DispatchError::Protocol("无可用上游".to_string())),
                    }
                };
                let upstream = Upstream::new(upstream_server, timeouts);
                let stream = upstream.connect(host, port).await?;
                Ok((stream, Action::Proxy, decision.rule))
            }
        }
    }
}

/// 双向 relay（统计字节）。
async fn relay(mut client: TcpStream, mut upstream: TcpStream, stats: Arc<Stats>) {
    let (up, down) = match tokio::io::copy_bidirectional(&mut client, &mut upstream).await {
        Ok((u, d)) => (u, d),
        Err(e) => {
            tracing::debug!("relay 结束: {e}");
            (0, 0)
        }
    };
    stats.add_up(up as usize);
    stats.add_down(down as usize);
}

// ============= SOCKS5 入站 =============

/// 运行 SOCKS5 服务（阻塞直到 accept 失败）。
pub async fn run_socks5(listener: TcpListener, ctx: Arc<ServerContext>) -> std::io::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let ctx = ctx.clone();
        tokio::spawn(async move {
            handle_socks5(stream, peer, ctx).await;
        });
    }
}

async fn handle_socks5(mut stream: TcpStream, peer: std::net::SocketAddr, ctx: Arc<ServerContext>) {
    // 1) greeting: VER NMETHODS METHODS
    let mut header = [0u8; 2];
    if stream.read_exact(&mut header).await.is_err() {
        return;
    }
    if header[0] != consts::VERSION {
        return;
    }
    let mut methods = vec![0u8; header[1] as usize];
    if stream.read_exact(&mut methods).await.is_err() {
        return;
    }
    // 仅监听 127.0.0.1：直接选无认证
    if stream
        .write_all(&[consts::VERSION, consts::AUTH_NONE])
        .await
        .is_err()
    {
        return;
    }

    // 2) request: VER CMD RSV ATYP ADDR PORT
    let mut req_head = [0u8; 4];
    if stream.read_exact(&mut req_head).await.is_err() {
        return;
    }
    if req_head[0] != consts::VERSION || req_head[1] != consts::CMD_CONNECT {
        let _ = send_socks5_reply(&mut stream, 0x07).await; // 不支持的命令
        return;
    }
    let (host, port) = match read_socks5_address(&mut stream, req_head[3]).await {
        Some(hp) => hp,
        None => {
            let _ = send_socks5_reply(&mut stream, 0x08).await; // 不支持的地址类型
            return;
        }
    };

    tracing::debug!("SOCKS5 入站 {peer} -> {host}:{port}");
    ctx.stats.on_connect_started();
    match ctx.dispatch(&host, port).await {
        Ok((upstream, action, rule)) => {
            let _ = send_socks5_reply(&mut stream, consts::REPLY_SUCCESS).await;
            tracing::info!(
                "SOCKS5 {peer} -> {host}:{port} [{}{}]",
                action.name(),
                if rule.is_empty() {
                    String::new()
                } else {
                    format!(" rule={rule}")
                }
            );
            ctx.record_connection("tcp", &host, port, action.name(), &rule, "ok", "");
            relay(stream, upstream, ctx.stats.clone()).await;
        }
        Err(e) => {
            ctx.stats.on_connect_failed();
            let rep = dispatch_error_to_reply(&e);
            tracing::warn!(
                "SOCKS5 {peer} -> {host}:{port} 失败: {e} (REP={}, {})",
                rep,
                reply_to_text(rep)
            );
            ctx.record_connection("tcp", &host, port, "blocked", "", "failed", &e.to_string());
            let _ = send_socks5_reply(&mut stream, rep).await;
        }
    }
    ctx.stats.on_connect_finished();
}

/// 读取 SOCKS5 请求的 ATYP + ADDR + PORT。
async fn read_socks5_address(stream: &mut TcpStream, atyp: u8) -> Option<(String, u16)> {
    let host = match atyp {
        consts::ATYP_IPV4 => {
            let mut buf = [0u8; 4];
            stream.read_exact(&mut buf).await.ok()?;
            std::net::Ipv4Addr::from(buf).to_string()
        }
        consts::ATYP_IPV6 => {
            let mut buf = [0u8; 16];
            stream.read_exact(&mut buf).await.ok()?;
            std::net::Ipv6Addr::from(buf).to_string()
        }
        consts::ATYP_DOMAIN => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await.ok()?;
            let mut buf = vec![0u8; len[0] as usize];
            stream.read_exact(&mut buf).await.ok()?;
            String::from_utf8(buf).ok()?
        }
        _ => return None,
    };
    let mut port_buf = [0u8; 2];
    stream.read_exact(&mut port_buf).await.ok()?;
    Some((host, u16::from_be_bytes(port_buf)))
}

/// 发送 SOCKS5 响应（BND 填 0.0.0.0:0）。
async fn send_socks5_reply(stream: &mut TcpStream, rep: u8) -> std::io::Result<()> {
    stream
        .write_all(&[
            consts::VERSION,
            rep,
            0x00,
            consts::ATYP_IPV4,
            0,
            0,
            0,
            0,
            0,
            0,
        ])
        .await
}

// ============= HTTP CONNECT 入站 =============

/// 运行 HTTP CONNECT 服务。
pub async fn run_http(listener: TcpListener, ctx: Arc<ServerContext>) -> std::io::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let ctx = ctx.clone();
        tokio::spawn(async move {
            handle_http(stream, peer, ctx).await;
        });
    }
}

async fn handle_http(mut stream: TcpStream, peer: std::net::SocketAddr, ctx: Arc<ServerContext>) {
    // 读请求头（上限 8KB）
    let mut buf = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    loop {
        if buf.len() > 8192 {
            return;
        }
        if stream.read_exact(&mut byte).await.is_err() {
            return;
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    let first_line = text.lines().next().unwrap_or("");
    let Some(rest) = first_line.strip_prefix("CONNECT ") else {
        let _ = stream
            .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n")
            .await;
        return;
    };
    let Some((host, port_str)) = rest.split_once(' ') else {
        return;
    };
    // host 可能带端口；端口段形如 "HTTP/1.1"
    let (host, port) = match host.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => (
            h.trim_matches(|c| c == '[' || c == ']').to_string(),
            p.parse::<u16>().unwrap_or(443),
        ),
        _ => (host.trim_matches(|c| c == '[' || c == ']').to_string(), 443),
    };
    let _ = port_str;

    tracing::debug!("HTTP 入站 {peer} -> {host}:{port}");
    ctx.stats.on_connect_started();
    match ctx.dispatch(&host, port).await {
        Ok((upstream, action, rule)) => {
            let _ = stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await;
            tracing::info!(
                "HTTP {peer} -> {host}:{port} [{}{}]",
                action.name(),
                if rule.is_empty() {
                    String::new()
                } else {
                    format!(" rule={rule}")
                }
            );
            ctx.record_connection("tcp", &host, port, action.name(), &rule, "ok", "");
            relay(stream, upstream, ctx.stats.clone()).await;
        }
        Err(e) => {
            ctx.stats.on_connect_failed();
            tracing::warn!("HTTP {peer} -> {host}:{port} 失败: {e}");
            ctx.record_connection("tcp", &host, port, "blocked", "", "failed", &e.to_string());
            let body = format!("proxy error: {e}");
            let _ = stream
                .write_all(
                    format!(
                        "HTTP/1.1 502 Bad Gateway\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                )
                .await;
        }
    }
    ctx.stats.on_connect_finished();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, UpstreamServer, UpstreamSettings};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// 测试客户端：完整 SOCKS5 CONNECT 握手，返回可读写的流。
    async fn socks5_client_connect(
        proxy: std::net::SocketAddr,
        host: &str,
        port: u16,
    ) -> Option<TcpStream> {
        let mut stream = TcpStream::connect(proxy).await.ok()?;
        // greeting
        stream.write_all(&[0x05, 0x01, 0x00]).await.ok()?;
        let mut resp = [0u8; 2];
        stream.read_exact(&mut resp).await.ok()?;
        assert_eq!(resp, [0x05, 0x00]);
        // request
        let mut req = vec![0x05, 0x01, 0x00, 0x03];
        req.push(host.len() as u8);
        req.extend_from_slice(host.as_bytes());
        req.extend_from_slice(&port.to_be_bytes());
        stream.write_all(&req).await.ok()?;
        let mut reply = [0u8; 10];
        stream.read_exact(&mut reply).await.ok()?;
        assert_eq!(reply[1], 0x00, "SOCKS5 握手应成功");
        Some(stream)
    }

    /// mock SOCKS5 上游：完成协商并回显随后收到的数据。
    async fn spawn_mock_upstream(
        seen: Arc<tokio::sync::Mutex<Vec<String>>>,
    ) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // 注意：不可对 spawn 的 JoinHandle await（accept 循环永不返回，会死锁测试）
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let seen = seen.clone();
                tokio::spawn(async move {
                    let mut greeting = [0u8; 3];
                    if stream.read_exact(&mut greeting).await.is_err() {
                        return;
                    }
                    let _ = stream.write_all(&[0x05, 0x00]).await;
                    // CONNECT 请求
                    let mut head = [0u8; 4];
                    if stream.read_exact(&mut head).await.is_err() {
                        return;
                    }
                    let host = match head[3] {
                        0x01 => {
                            let mut b = [0u8; 4];
                            let _ = stream.read_exact(&mut b).await;
                            std::net::Ipv4Addr::from(b).to_string()
                        }
                        0x03 => {
                            let mut len = [0u8; 1];
                            let _ = stream.read_exact(&mut len).await;
                            let mut b = vec![0u8; len[0] as usize];
                            let _ = stream.read_exact(&mut b).await;
                            String::from_utf8(b).unwrap_or_default()
                        }
                        0x04 => {
                            let mut b = [0u8; 16];
                            let _ = stream.read_exact(&mut b).await;
                            std::net::Ipv6Addr::from(b).to_string()
                        }
                        _ => return,
                    };
                    let mut port_buf = [0u8; 2];
                    let _ = stream.read_exact(&mut port_buf).await;
                    let port = u16::from_be_bytes(port_buf);
                    seen.lock().await.push(format!("{host}:{port}"));
                    let _ = stream
                        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                        .await;
                    // 回显
                    let mut buf = vec![0u8; 64];
                    while let Ok(n) = stream.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        if stream.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        addr
    }

    /// 起一个本地 echo target（direct 用）。
    async fn spawn_echo_target(seen: Arc<tokio::sync::Mutex<Vec<String>>>) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let seen = seen.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 64];
                    if let Ok(n) = stream.read(&mut buf).await {
                        seen.lock()
                            .await
                            .push(String::from_utf8_lossy(&buf[..n]).to_string());
                        let _ = stream.write_all(&buf[..n]).await;
                    }
                });
            }
        });
        addr
    }

    async fn start_core(config: AppConfig) -> (std::net::SocketAddr, Arc<ServerContext>) {
        let ctx = Arc::new(ServerContext::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let ctx2 = ctx.clone();
        tokio::spawn(async move {
            let _ = run_socks5(listener, ctx2).await;
        });
        (addr, ctx)
    }

    fn config_with_upstream(upstream_addr: std::net::SocketAddr) -> AppConfig {
        let mut config = AppConfig::default();
        config.routing.default_action = "proxy".to_string();
        config.routing.use_default_private = true;
        config.upstream = UpstreamSettings {
            active: "mock".to_string(),
            servers: vec![UpstreamServer {
                id: "mock".to_string(),
                name: "Mock".to_string(),
                server_type: "socks5".to_string(),
                host: upstream_addr.ip().to_string(),
                port: upstream_addr.port(),
                username: String::new(),
                password: String::new(),
            }],
        };
        config
    }

    #[tokio::test]
    async fn socks5_inbound_relays_via_upstream() {
        let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let upstream_addr = spawn_mock_upstream(seen.clone()).await;
        let (core_addr, _ctx) = start_core(config_with_upstream(upstream_addr)).await;

        let mut client = socks5_client_connect(core_addr, "example.com", 443)
            .await
            .expect("SOCKS5 握手应成功");

        // 数据经 core → 上游 → 回显
        client.write_all(b"hello-proxy").await.unwrap();
        let mut buf = [0u8; 11];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello-proxy");

        // 上游看到的域名目标
        let targets = seen.lock().await.clone();
        assert_eq!(targets, vec!["example.com:443".to_string()]);
    }

    #[tokio::test]
    async fn socks5_inbound_direct_target() {
        let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let echo_addr = spawn_echo_target(seen.clone()).await;

        let mut config = AppConfig::default();
        config.routing.default_action = "direct".to_string();
        config.routing.use_default_private = false;
        config.proxy_rules.allowed_ports = Vec::new(); // 测试目标端口随机，清空白名单
        let (core_addr, _ctx) = start_core(config).await;

        let mut client = socks5_client_connect(core_addr, "127.0.0.1", echo_addr.port())
            .await
            .expect("直连目标握手应成功");
        client.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
        assert_eq!(seen.lock().await.clone(), vec!["ping".to_string()]);
    }

    #[tokio::test]
    async fn socks5_inbound_replies_error_when_upstream_refused() {
        // 上游端口未监听 → core 应回 REP=0x05
        let dead = std::net::SocketAddr::from(([127, 0, 0, 1], 1));
        let (core_addr, _ctx) = start_core(config_with_upstream(dead)).await;

        let mut stream = TcpStream::connect(core_addr).await.unwrap();
        stream.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut resp = [0u8; 2];
        stream.read_exact(&mut resp).await.unwrap();
        assert_eq!(resp, [0x05, 0x00]);

        let mut req = vec![0x05, 0x01, 0x00, 0x03, 11];
        req.extend_from_slice(b"example.com");
        req.extend_from_slice(&443u16.to_be_bytes());
        stream.write_all(&req).await.unwrap();

        let mut reply = [0u8; 10];
        stream.read_exact(&mut reply).await.unwrap();
        assert_ne!(reply[1], 0x00, "上游不可达应回错误 REPLY");
    }

    #[tokio::test]
    async fn port_not_allowed_is_rejected() {
        let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let upstream_addr = spawn_mock_upstream(seen.clone()).await;
        let mut config = config_with_upstream(upstream_addr);
        config.proxy_rules.allowed_ports = vec![443]; // 只允许 443
        let (core_addr, _ctx) = start_core(config).await;

        // 8080 不在白名单 → 被拒（REP=0x02 规则不允许）
        let mut stream = TcpStream::connect(core_addr).await.unwrap();
        stream.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut resp = [0u8; 2];
        stream.read_exact(&mut resp).await.unwrap();
        let mut req = vec![0x05, 0x01, 0x00, 0x03, 11];
        req.extend_from_slice(b"example.com");
        req.extend_from_slice(&8080u16.to_be_bytes());
        stream.write_all(&req).await.unwrap();
        let mut reply = [0u8; 10];
        stream.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[1], 0x02, "端口不在白名单应回规则不允许");
    }

    #[tokio::test]
    async fn http_inbound_relays_via_upstream() {
        let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let upstream_addr = spawn_mock_upstream(seen.clone()).await;
        let config = config_with_upstream(upstream_addr);

        let ctx = Arc::new(ServerContext::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let core_addr = listener.local_addr().unwrap();
        let ctx2 = ctx.clone();
        tokio::spawn(async move {
            let _ = run_http(listener, ctx2).await;
        });

        let mut stream = TcpStream::connect(core_addr).await.unwrap();
        stream
            .write_all(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n")
            .await
            .unwrap();
        let mut resp = [0u8; 64];
        let n = stream.read(&mut resp).await.unwrap();
        let text = String::from_utf8_lossy(&resp[..n]);
        assert!(text.contains("200"), "HTTP 入站应回 200, got: {text}");

        stream.write_all(b"http-echo").await.unwrap();
        let mut buf = [0u8; 9];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"http-echo");
        assert_eq!(
            seen.lock().await.clone(),
            vec!["example.com:443".to_string()]
        );
    }

    #[tokio::test]
    async fn stats_track_connections() {
        let seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let upstream_addr = spawn_mock_upstream(seen.clone()).await;
        let (core_addr, ctx) = start_core(config_with_upstream(upstream_addr)).await;

        let client = socks5_client_connect(core_addr, "example.com", 443)
            .await
            .unwrap();
        drop(client);
        // 等待 relay task 收尾
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let snap = ctx.stats.snapshot();
        assert_eq!(snap.connections_total, 1);
        assert_eq!(snap.connections_proxy, 1);
        assert_eq!(snap.connections_active, 0);
    }
}
