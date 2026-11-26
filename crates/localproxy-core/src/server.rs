//! SOCKS5 / HTTP CONNECT 代理服务（tokio）。
//!
//! 入站协议：
//! - SOCKS5（`socks5` 端口）：方法协商（仅本机监听，无认证）→ CONNECT 请求
//! - HTTP CONNECT（`http` 端口）：`CONNECT host:port HTTP/1.1`
//!
//! 分流：`RuleEngine::decide` → `Direct`（本地拨号）/ `Proxy`（上游级联）。
//! 拒绝时返回对应协议的错误响应（SOCKS5 REPLY 码 / HTTP 状态码）。

use std::sync::Arc;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
};
use std::{io::Write, path::PathBuf};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use localproxy_net::socks5::{self, consts, reply_to_text, Timeouts};
use localproxy_net::udp::UdpAction;

use crate::config::{AppConfig, UpstreamServer};
use crate::diagnostics::{self, ConnectionLog, ConnectionRecord};
use crate::managed::ManagedRuntime;
use crate::rules::{Action, RouteDecision, RuleEngine};
use crate::stats::Stats;
use crate::upstream::{UdpSession, Upstream, UpstreamError};

/// 服务共享上下文。
pub struct ServerContext {
    /// 新连接从同一份快照读取配置与编译后的规则。
    pub(crate) settings: tokio::sync::RwLock<Arc<RuntimeSettings>>,
    pub(crate) reload_lock: tokio::sync::Mutex<()>,
    _policy_lease: Option<std::fs::File>,
    policy_directory: Option<tempfile::TempDir>,
    /// 统计
    pub stats: Arc<Stats>,
    /// 连接诊断日志（环形缓冲）
    pub connections: Arc<ConnectionLog>,
}

pub(crate) struct RuntimeSettings {
    pub config: AppConfig,
    rules: RuleEngine,
    managed: Option<Arc<ManagedRuntime>>,
}

impl RuntimeSettings {
    pub fn new(config: AppConfig) -> Self {
        let rules = RuleEngine::compile(&config.routing);
        tracing::info!("规则引擎: {}", rules.summary());
        Self {
            config,
            rules,
            managed: None,
        }
    }
    pub async fn prepare(config: AppConfig, previous: Option<&Self>) -> Result<Self, String> {
        let mut settings = Self::new(config);
        if let Some(server) = settings
            .config
            .upstream
            .active_server()
            .filter(|s| s.managed.is_some())
        {
            if let Some(previous) = previous.filter(|old| {
                old.config.mihomo_path == settings.config.mihomo_path
                    && old.config.upstream.active_server() == Some(server)
                    && old
                        .managed
                        .as_ref()
                        .is_some_and(|engine| engine.is_running())
            }) {
                settings.managed.clone_from(&previous.managed);
            } else {
                settings.managed = Some(Arc::new(
                    ManagedRuntime::start(server, &settings.config.mihomo_path).await?,
                ));
            }
        }
        Ok(settings)
    }

    fn active_proxy(&self) -> Option<UpstreamServer> {
        if let Some(runtime) = &self.managed {
            return Some(runtime.server.clone());
        }
        self.config
            .upstream
            .active_server()
            .filter(|s| s.managed.is_none())
            .cloned()
    }
}

impl ServerContext {
    /// 由配置构造。
    pub fn new(config: AppConfig) -> Self {
        Self::from_settings(RuntimeSettings::new(config))
    }

    pub async fn start(config: AppConfig) -> Result<Self, String> {
        let settings = RuntimeSettings::prepare(config, None).await?;
        let mut ctx = Self::from_settings(settings);
        let directory = tempfile::Builder::new()
            .prefix("localproxy-policy-")
            .tempdir()
            .map_err(|error| format!("创建策略目录失败: {error}"))?;
        let lease = std::fs::File::create(
            directory
                .path()
                .join(localproxy_net::policy::POLICY_LEASE_FILENAME),
        )
        .map_err(|error| format!("创建策略租约失败: {error}"))?;
        lease
            .lock()
            .map_err(|error| format!("锁定策略租约失败: {error}"))?;
        ctx._policy_lease = Some(lease);
        ctx.policy_directory = Some(directory);
        ctx.publish_policy(&ctx.settings.read().await.config)?;
        Ok(ctx)
    }

    fn from_settings(settings: RuntimeSettings) -> Self {
        Self {
            settings: tokio::sync::RwLock::new(Arc::new(settings)),
            reload_lock: tokio::sync::Mutex::new(()),
            policy_directory: None,
            _policy_lease: None,
            stats: Arc::new(Stats::new()),
            connections: Arc::new(ConnectionLog::with_default_capacity()),
        }
    }

    pub fn policy_path(&self) -> Option<PathBuf> {
        self.policy_directory
            .as_ref()
            .map(|directory| directory.path().join("active"))
    }

    pub(crate) fn publish_policy(&self, config: &AppConfig) -> Result<(), String> {
        if let Some(directory) = &self.policy_directory {
            let result = (|| -> std::io::Result<()> {
                let mut next = tempfile::NamedTempFile::new_in(directory.path())?;
                next.write_all(config.network_policy().encode().as_bytes())?;
                next.persist(directory.path().join("active"))?;
                Ok(())
            })();
            result.map_err(|error| format!("更新运行期策略失败: {error}"))?;
        }
        Ok(())
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
    async fn dispatch(
        &self,
        host: &str,
        port: u16,
    ) -> Result<(TcpStream, Action, String, Option<Arc<ManagedRuntime>>), DispatchError> {
        let (decision, server, lease) = {
            let settings = self.settings.read().await;
            if !settings.config.proxy_rules.is_port_allowed(port) {
                return Err(DispatchError::NotAllowed);
            }
            let decision = settings.rules.decide(host, None, port, "tcp");
            let server = if decision.action == Action::Proxy {
                settings.active_proxy()
            } else {
                None
            };
            let lease = server.as_ref().and(settings.managed.as_ref()).cloned();
            (decision, server, lease)
        };
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
                Ok((stream, Action::Direct, decision.rule, None))
            }
            Action::Proxy => {
                self.stats.on_routed_proxy();
                let Some(server) = server else {
                    return Err(DispatchError::Protocol("无可用上游".to_string()));
                };
                let upstream = Upstream::new(server, Self::timeouts());
                let stream = upstream.connect(host, port).await?;
                Ok((stream, Action::Proxy, decision.rule, lease))
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
    if req_head[0] == consts::VERSION && req_head[1] == consts::CMD_UDP_ASSOCIATE {
        handle_udp_associate(stream, peer, req_head[3], &ctx).await;
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
        Ok((upstream, action, rule, _lease)) => {
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

/// 保持客户端与上游的 UDP 关联，并在任一控制连接关闭时释放中继。
async fn handle_udp_associate(
    mut control: TcpStream,
    peer: std::net::SocketAddr,
    atyp: u8,
    ctx: &ServerContext,
) {
    if read_socks5_address(&mut control, atyp).await.is_none() {
        let _ = send_socks5_reply(&mut control, 0x08).await;
        return;
    }

    // 已建立的代理关联保留原上游的生命周期，报文按最新配置分流。
    let initial = ctx.settings.read().await.clone();
    let udp = initial.config.network_policy().udp;
    if udp == UdpAction::Block {
        let _ = send_socks5_reply(&mut control, 0x02).await;
        return;
    }
    let mut upstream = None;
    if udp == UdpAction::Proxy {
        match open_udp_upstream(initial.clone()).await {
            Ok(session) => upstream = Some(session),
            Err(error) if initial.has_explicit_udp_direct() => {
                tracing::debug!("UDP 上游不可用，仍处理显式直连规则: {error}");
            }
            Err(error) => {
                let reply = dispatch_error_to_reply(&error);
                let _ = send_socks5_reply(&mut control, reply).await;
                return;
            }
        }
    }
    let relay_socket = match control.local_addr() {
        Ok(addr) => tokio::net::UdpSocket::bind((addr.ip(), 0)).await,
        Err(error) => Err(error),
    };
    let relay = match relay_socket {
        Ok(socket) => socket,
        Err(error) => {
            tracing::debug!("UDP ASSOCIATE: 绑定 relay 失败: {error}");
            let _ = send_socks5_reply(&mut control, 0x01).await;
            return;
        }
    };
    let Ok(addr) = relay.local_addr() else {
        let _ = send_socks5_reply(&mut control, 0x01).await;
        return;
    };
    let (atyp, encoded) = socks5::encode_address(&addr.ip().to_string()).expect("有效 IP 地址");
    let mut response = vec![consts::VERSION, consts::REPLY_SUCCESS, 0, atyp];
    response.extend_from_slice(&encoded);
    response.extend_from_slice(&addr.port().to_be_bytes());
    if control.write_all(&response).await.is_err() {
        return;
    }
    ctx.stats.on_connect_started();
    let mut client_addr = None;
    let mut from_client = vec![0; 65536];
    let mut from_upstream = vec![0; 65536];
    let mut from_direct = vec![0; 65536];
    let mut from_direct_v6 = vec![0; 65536];
    let mut client_control = [0];
    let mut upstream_control = [0];
    let mut direct = DirectUdp::default();
    loop {
        let (upstream_tcp, upstream_udp) = match upstream.as_mut() {
            Some(proxy) => (
                Some(&mut proxy.session.control),
                Some(&proxy.session.socket),
            ),
            None => (None, None),
        };
        tokio::select! {
            _ = control.read(&mut client_control) => break,
            _ = read_optional(upstream_tcp, &mut upstream_control) => {
                upstream = None;
            }
            packet = relay.recv_from(&mut from_client) => {
                let Ok((len, addr)) = packet else { break };
                if addr.ip() != peer.ip() || client_addr.is_some_and(|client| client != addr) {
                    continue;
                }
                let data = &from_client[..len];
                let Some((host, port, payload)) = socks5::decode_udp_packet(data) else { continue };
                let current = ctx.settings.read().await.clone();
                let Some(decision) = current.udp_route(&host, port) else { continue };
                client_addr = Some(addr);
                let sent = match decision.action {
                    Action::Direct => {
                        let ipv6 = current.config.network_policy().ipv6 != UdpAction::Block;
                        direct.send(ctx, &host, port, payload, ipv6).await
                    }
                    Action::Proxy => {
                        if upstream.is_none() {
                            match open_udp_upstream(current.clone()).await {
                                Ok(session) => upstream = Some(session),
                                Err(error) => {
                                    tracing::debug!("UDP 上游会话建立失败: {error}");
                                    continue;
                                }
                            }
                        }
                        let settings = ctx.settings.read().await;
                        if !settings.allows_udp_action(&host, port, Action::Proxy) {
                            continue;
                        }
                        let socket = &upstream.as_ref().expect("上游已建立").session.socket;
                        match tokio::time::timeout(ServerContext::timeouts().send_timeout(), socket.send(data)).await {
                            Ok(result) => result,
                            Err(error) => Err(error.into()),
                        }
                    }
                };
                if let Err(error) = sent {
                    ctx.record_connection("udp", &host, port, decision.action.name(), &decision.rule, "failed", &error.to_string());
                }
            }
            packet = recv_optional(upstream_udp, &mut from_upstream) => {
                let Ok((len, _)) = packet else {
                    upstream = None;
                    continue;
                };
                let data = &from_upstream[..len];
                let Some((_, port, _)) = socks5::decode_udp_packet(data) else { continue };
                let current = ctx.settings.read().await;
                if current.config.network_policy().udp == UdpAction::Block
                    || !current.config.proxy_rules.is_port_allowed(port) {
                    continue;
                }
                if let Some(addr) = client_addr {
                    let _ = tokio::time::timeout(ServerContext::timeouts().send_timeout(), relay.send_to(data, addr)).await;
                }
            }
            packet = recv_optional(direct.ipv4.as_ref(), &mut from_direct) => {
                if let Ok((len, source)) = packet {
                    direct.forward_reply(ctx, &relay, client_addr, source, &from_direct[..len]).await;
                }
            }
            packet = recv_optional(direct.ipv6.as_ref(), &mut from_direct_v6) => {
                if let Ok((len, source)) = packet {
                    direct.forward_reply(ctx, &relay, client_addr, source, &from_direct_v6[..len]).await;
                }
            }
        }
    }
    ctx.stats.on_connect_finished();
    tracing::debug!("UDP ASSOCIATE {peer} 结束");
}

impl RuntimeSettings {
    fn has_explicit_udp_direct(&self) -> bool {
        self.config.routing.default_action == "direct"
            || (self.config.routing.enabled
                && self.config.routing.rules.iter().any(|rule| {
                    rule.enabled
                        && rule.action == "direct"
                        && (rule.protocols.is_empty() || rule.protocols.iter().any(|p| p == "udp"))
                }))
    }

    fn udp_route(&self, host: &str, port: u16) -> Option<RouteDecision> {
        if !self.config.proxy_rules.is_port_allowed(port) {
            return None;
        }
        let policy = self.config.network_policy();
        if policy.ipv6 == UdpAction::Block && host.parse::<IpAddr>().is_ok_and(|ip| ip.is_ipv6()) {
            return None;
        }
        match policy.udp {
            UdpAction::Block => None,
            UdpAction::Direct => Some(RouteDecision {
                action: Action::Direct,
                rule: String::new(),
            }),
            UdpAction::Proxy => Some(self.rules.decide(host, None, port, "udp")),
        }
    }

    fn allows_udp_action(&self, host: &str, port: u16, action: Action) -> bool {
        self.udp_route(host, port)
            .is_some_and(|decision| decision.action == action)
    }
}

struct ProxyUdp {
    session: UdpSession,
    // 关联仍持有原内核，新关联才切换到新上游。
    _settings: Arc<RuntimeSettings>,
}

async fn open_udp_upstream(settings: Arc<RuntimeSettings>) -> Result<ProxyUdp, DispatchError> {
    let server = settings
        .active_proxy()
        .ok_or_else(|| DispatchError::Protocol("无可用上游".to_string()))?;
    let session = Upstream::new(server, ServerContext::timeouts())
        .udp_associate()
        .await?;
    Ok(ProxyUdp {
        session,
        _settings: settings,
    })
}

async fn read_optional(stream: Option<&mut TcpStream>, buf: &mut [u8]) -> std::io::Result<usize> {
    match stream {
        Some(stream) => stream.read(buf).await,
        None => std::future::pending().await,
    }
}

async fn recv_optional(
    socket: Option<&UdpSocket>,
    buf: &mut [u8],
) -> std::io::Result<(usize, SocketAddr)> {
    match socket {
        Some(socket) => socket.recv_from(buf).await,
        None => std::future::pending().await,
    }
}

#[derive(Default)]
struct DirectUdp {
    ipv4: Option<UdpSocket>,
    ipv6: Option<UdpSocket>,
    /// 只把明确发送过的目标的回包交还客户端。
    peers: HashMap<SocketAddr, String>,
}

impl DirectUdp {
    async fn send(
        &mut self,
        ctx: &ServerContext,
        host: &str,
        port: u16,
        data: &[u8],
        ipv6: bool,
    ) -> std::io::Result<usize> {
        const MAX_DIRECT_PEERS: usize = 128;
        let target = if let Ok(ip) = host.parse::<IpAddr>() {
            SocketAddr::new(ip, port)
        } else {
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                tokio::net::lookup_host((host, port)),
            )
            .await??
            .find(|addr| ipv6 || addr.is_ipv4())
            .ok_or(std::io::ErrorKind::AddrNotAvailable)?
        };
        if self.peers.len() >= MAX_DIRECT_PEERS && !self.peers.contains_key(&target) {
            return Err(std::io::Error::other("UDP 直连会话目标数达到上限"));
        }
        let socket = if target.is_ipv4() {
            &mut self.ipv4
        } else {
            &mut self.ipv6
        };
        if socket.is_none() {
            let bind = if target.is_ipv4() {
                "0.0.0.0:0"
            } else {
                "[::]:0"
            };
            *socket = Some(UdpSocket::bind(bind).await?);
        }
        // DNS 解析和 bind 可能跨过一次热重载，实际发送前重新确认策略。
        let settings = ctx.settings.read().await;
        if !settings.allows_udp_action(host, port, Action::Direct)
            || (target.is_ipv6() && settings.config.network_policy().ipv6 == UdpAction::Block)
        {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
        let socket = socket.as_ref().expect("直连 socket 已绑定");
        let size = tokio::time::timeout(
            ServerContext::timeouts().send_timeout(),
            socket.send_to(data, target),
        )
        .await??;
        self.peers.insert(target, host.to_string());
        Ok(size)
    }

    async fn forward_reply(
        &self,
        ctx: &ServerContext,
        relay: &UdpSocket,
        client: Option<SocketAddr>,
        source: SocketAddr,
        data: &[u8],
    ) {
        let (Some(client), Some(host)) = (client, self.peers.get(&source)) else {
            return;
        };
        let settings = ctx.settings.read().await;
        if !settings.allows_udp_action(host, source.port(), Action::Direct)
            || (source.is_ipv6() && settings.config.network_policy().ipv6 == UdpAction::Block)
        {
            return;
        }
        if let Ok(packet) = socks5::encode_udp_packet(&source.ip().to_string(), source.port(), data)
        {
            let _ = tokio::time::timeout(
                ServerContext::timeouts().send_timeout(),
                relay.send_to(&packet, client),
            )
            .await;
        }
    }
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
        Ok((upstream, action, rule, _lease)) => {
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
    use crate::config::UpstreamSettings;
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
                clash: None,
                managed: None,
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
    async fn upstream_reload_switches_new_connections_and_keeps_existing_tunnel() {
        let old_seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let new_seen = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let old_upstream = spawn_mock_upstream(old_seen.clone()).await;
        let new_upstream = spawn_mock_upstream(new_seen.clone()).await;
        let mut config = config_with_upstream(old_upstream);
        config.control_token = "TestToken".to_string();
        let mut second = config.upstream.servers[0].clone();
        second.id = "second".to_string();
        second.port = new_upstream.port();
        config.upstream.servers.push(second);
        let (core_addr, ctx) = start_core(config.clone()).await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let control_addr = listener.local_addr().unwrap();
        let path = std::env::temp_dir().join(format!(
            "localproxy-reload-{}.json",
            crate::config::generate_token()
        ));
        tokio::spawn(crate::control::run(listener, ctx, Some(path.clone())));

        let mut existing = socks5_client_connect(core_addr, "old.example", 443)
            .await
            .unwrap();
        config.upstream.active = "second".to_string();
        config.save(&path).unwrap();

        for (token, status) in [("testtoken", "401"), ("TestToken", "200")] {
            let mut control = TcpStream::connect(control_addr).await.unwrap();
            control
                .write_all(
                    format!(
                        "POST /v1/config/reload HTTP/1.1\r\nAuthorization: Bearer {token}\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let mut response = String::new();
            control.read_to_string(&mut response).await.unwrap();
            assert!(
                response.starts_with(&format!("HTTP/1.1 {status}")),
                "{response}"
            );
            if status == "200" {
                assert!(response.contains(r#""active_upstream":"second""#));
            }
        }
        std::fs::remove_file(path).unwrap();

        let _new = socks5_client_connect(core_addr, "new.example", 443)
            .await
            .unwrap();
        existing.write_all(b"old tunnel").await.unwrap();
        let mut echo = [0; 10];
        existing.read_exact(&mut echo).await.unwrap();
        assert_eq!(&echo, b"old tunnel");
        assert_eq!(*old_seen.lock().await, ["old.example:443"]);
        assert_eq!(*new_seen.lock().await, ["new.example:443"]);
    }

    #[tokio::test]
    async fn socks5_udp_preserves_datagram_and_releases_association_on_close() {
        use tokio::net::UdpSocket;

        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = config_with_upstream(upstream.local_addr().unwrap());
        let (core_addr, _) = start_core(config).await;
        let relay = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let relay_port = relay.local_addr().unwrap().port();
        let upstream_task = tokio::spawn(async move {
            let (mut control, _) = upstream.accept().await.unwrap();
            let mut greeting = [0; 3];
            control.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting, [5, 1, 0]);
            control.write_all(&[5, 0]).await.unwrap();
            let mut request = [0; 10];
            control.read_exact(&mut request).await.unwrap();
            assert_eq!(request, [5, 3, 0, 1, 0, 0, 0, 0, 0, 0]);
            let mut reply = vec![5, 0, 0, 1, 0, 0, 0, 0];
            reply.extend_from_slice(&relay_port.to_be_bytes());
            control.write_all(&reply).await.unwrap();
            let mut packet = [0; 1024];
            let (len, sender) = relay.recv_from(&mut packet).await.unwrap();
            let (host, port, payload) = socks5::decode_udp_packet(&packet[..len]).unwrap();
            assert_eq!(
                (host.as_str(), port, payload),
                ("udp.example", 443, &b"hello"[..])
            );
            let response = socks5::encode_udp_packet("203.0.113.1", 443, b"reply").unwrap();
            relay.send_to(&response, sender).await.unwrap();
            assert_eq!(control.read(&mut [0; 1]).await.unwrap(), 0);
        });

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut client = TcpStream::connect(core_addr).await.unwrap();
            client.write_all(&[5, 1, 0]).await.unwrap();
            let mut method = [0; 2];
            client.read_exact(&mut method).await.unwrap();
            assert_eq!(method, [5, 0]);
            client
                .write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            let mut reply = [0; 10];
            client.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply[..8], &[5, 0, 0, 1, 127, 0, 0, 1]);
            let port = u16::from_be_bytes([reply[8], reply[9]]);
            let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let request = socks5::encode_udp_packet("udp.example", 443, b"hello").unwrap();
            udp.send_to(&request, (std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            let mut packet = [0; 1024];
            let (len, _) = udp.recv_from(&mut packet).await.unwrap();
            let (host, port, payload) = socks5::decode_udp_packet(&packet[..len]).unwrap();
            assert_eq!(
                (host.as_str(), port, payload),
                ("203.0.113.1", 443, &b"reply"[..])
            );
            drop(client);
            upstream_task.await.unwrap();
        })
        .await
        .unwrap();
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

    #[tokio::test]
    async fn udp_associate_reports_upstream_rejection() {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (core_addr, _) = start_core(config_with_upstream(upstream.local_addr().unwrap())).await;
        let upstream_task = tokio::spawn(async move {
            let (mut control, _) = upstream.accept().await.unwrap();
            let mut greeting = [0; 3];
            control.read_exact(&mut greeting).await.unwrap();
            control.write_all(&[5, 0xff]).await.unwrap();
        });

        let mut control = TcpStream::connect(core_addr).await.unwrap();
        control.write_all(&[5, 1, 0]).await.unwrap();
        let mut method = [0; 2];
        control.read_exact(&mut method).await.unwrap();
        assert_eq!(method, [5, 0]);
        control
            .write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        let mut reply = [0; 10];
        control.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply[..2], &[5, 2]);
        upstream_task.await.unwrap();
    }

    #[tokio::test]
    async fn udp_direct_rule_is_applied_and_running_association_respects_block() {
        let target = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target_addr = target.local_addr().unwrap();
        let unavailable = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = config_with_upstream(unavailable.local_addr().unwrap());
        drop(unavailable);
        config.routing.use_default_private = false;
        config.proxy_rules.allowed_ports = vec![target_addr.port()];
        config.routing.rules.push(crate::config::RoutingRule {
            name: "UDP 局部直连".to_string(),
            action: "direct".to_string(),
            domains: vec!["127.0.0.1".to_string()],
            ports: vec![target_addr.port().to_string()],
            protocols: vec!["udp".to_string()],
            ..Default::default()
        });
        let (core, ctx) = start_core(config.clone()).await;
        let mut control = TcpStream::connect(core).await.unwrap();
        control.write_all(&[5, 1, 0]).await.unwrap();
        let mut method = [0; 2];
        control.read_exact(&mut method).await.unwrap();
        assert_eq!(method, [5, 0]);
        control
            .write_all(&[5, 3, 0, 1, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        let mut reply = [0; 10];
        control.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply[..4], &[5, 0, 0, 1]);
        let relay = SocketAddr::new(core.ip(), u16::from_be_bytes([reply[8], reply[9]]));
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let packet = socks5::encode_udp_packet("127.0.0.1", target_addr.port(), b"direct").unwrap();
        client.send_to(&packet, relay).await.unwrap();
        let mut buffer = [0; 128];
        let (len, source) = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            target.recv_from(&mut buffer),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(&buffer[..len], b"direct");
        target.send_to(b"reply", source).await.unwrap();
        let len = tokio::time::timeout(std::time::Duration::from_secs(1), client.recv(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        let (_, _, payload) = socks5::decode_udp_packet(&buffer[..len]).unwrap();
        assert_eq!(payload, b"reply");

        config.proxy_rules.udp_mode = "block".to_string();
        *ctx.settings.write().await = Arc::new(RuntimeSettings::new(config));
        client.send_to(&packet, relay).await.unwrap();
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(100),
            target.recv(&mut buffer)
        )
        .await
        .is_err());
    }
}
