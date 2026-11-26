//! 上游代理客户端（异步，tokio）。
//!
//! 与 `localproxy-net` 的同步客户端共享协议编解码（`encode_address` /
//! `build_connect_request` / `parse_status_code` / `reply_to_text`），
//! 仅 IO 层不同：shim 热路径用同步 std，core 用 tokio。
//!
//! 支持：
//! - SOCKS5 上游（含 RFC 1929 账密认证）
//! - HTTP CONNECT 上游（含 Basic 认证）
//! - 连接/发送/接收分级超时（移植旧 `Timeouts` 语义）

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use localproxy_net::http_connect;
use localproxy_net::socks5::{self, consts, ProxyAuth, Timeouts};

use crate::config::UpstreamServer;

/// 上游拨号错误。
#[derive(Debug)]
pub enum UpstreamError {
    /// 连接/IO 失败
    Io(String),
    /// 认证失败
    Auth(String),
    /// 协议错误（版本不符等）
    Protocol(String),
    /// 上游返回非成功 REPLY
    Reply(u8),
    /// 目标参数非法
    InvalidTarget(String),
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpstreamError::Io(msg) => write!(f, "IO 失败: {msg}"),
            UpstreamError::Auth(msg) => write!(f, "认证失败: {msg}"),
            UpstreamError::Protocol(msg) => write!(f, "协议错误: {msg}"),
            UpstreamError::Reply(rep) => {
                write!(f, "上游拒绝 (REP={}, {})", rep, socks5::reply_to_text(*rep))
            }
            UpstreamError::InvalidTarget(msg) => write!(f, "目标非法: {msg}"),
        }
    }
}

impl std::error::Error for UpstreamError {}

/// UDP Associate 会话：控制 TCP 与连接到上游 relay 的 UDP socket。
pub struct UdpSession {
    /// SOCKS5 控制连接（必须保持存活，关闭即关联终止）
    pub control: TcpStream,
    pub socket: tokio::net::UdpSocket,
}

impl Upstream {
    /// SOCKS5 UDP ASSOCIATE（异步）。
    ///
    /// 仅 SOCKS5 上游支持；HTTP 上游返回错误（调用方按 `udp_fallback` 处理）。
    pub async fn udp_associate(&self) -> Result<UdpSession, UpstreamError> {
        if self.server.server_type != "socks5" {
            return Err(UpstreamError::Protocol(format!(
                "上游类型 {} 不支持 UDP Associate",
                self.server.server_type
            )));
        }
        let mut control = self.open_socks5().await?;

        // UDP ASSOCIATE 请求（DST 0.0.0.0:0）
        let mut request = Vec::with_capacity(10);
        request.push(consts::VERSION);
        request.push(consts::CMD_UDP_ASSOCIATE);
        request.push(0x00);
        request.push(consts::ATYP_IPV4);
        request.extend_from_slice(&[0, 0, 0, 0]);
        request.extend_from_slice(&[0, 0]);
        write_timeout(&mut control, &request, self.timeouts.send_timeout()).await?;

        // 响应
        let mut header = [0u8; 4];
        read_exact_timeout(&mut control, &mut header, self.timeouts.recv_timeout()).await?;
        let reply = socks5::parse_reply_header(&header).map_err(UpstreamError::from)?;
        if reply.rep != consts::REPLY_SUCCESS {
            return Err(UpstreamError::Reply(reply.rep));
        }
        let domain_len = if reply.atyp == consts::ATYP_DOMAIN {
            let mut len_byte = [0u8; 1];
            read_exact_timeout(&mut control, &mut len_byte, self.timeouts.recv_timeout()).await?;
            Some(len_byte[0])
        } else {
            None
        };
        let tail = socks5::reply_tail_len(reply.atyp, domain_len)
            .ok_or_else(|| UpstreamError::Protocol("响应 ATYP 未知".to_string()))?;
        let mut tail_buf = vec![0u8; tail];
        read_exact_timeout(&mut control, &mut tail_buf, self.timeouts.recv_timeout()).await?;

        // 解析 relay 地址
        let mut relay = match reply.atyp {
            consts::ATYP_IPV4 => std::net::SocketAddr::from((
                std::net::Ipv4Addr::new(tail_buf[0], tail_buf[1], tail_buf[2], tail_buf[3]),
                u16::from_be_bytes([tail_buf[4], tail_buf[5]]),
            )),
            consts::ATYP_IPV6 => {
                let mut octets = [0u8; 16];
                octets.copy_from_slice(&tail_buf[..16]);
                std::net::SocketAddr::from((
                    std::net::Ipv6Addr::from(octets),
                    u16::from_be_bytes([tail_buf[16], tail_buf[17]]),
                ))
            }
            consts::ATYP_DOMAIN => {
                let name = String::from_utf8_lossy(&tail_buf[..domain_len.unwrap_or(0) as usize])
                    .to_string();
                let port_off = domain_len.unwrap_or(0) as usize;
                let port = u16::from_be_bytes([tail_buf[port_off], tail_buf[port_off + 1]]);
                tokio::time::timeout(
                    self.timeouts.connect_timeout(),
                    tokio::net::lookup_host((name, port)),
                )
                .await
                .map_err(|_| UpstreamError::Protocol("BND 域名解析超时".to_string()))?
                .map_err(|e| UpstreamError::Protocol(format!("BND 域名解析失败: {e}")))?
                .next()
                .ok_or_else(|| UpstreamError::Protocol("BND 域名无地址".to_string()))?
            }
            other => return Err(UpstreamError::Protocol(format!("ATYP 未知: {other}"))),
        };

        // 上游返回通配地址时，使用同一 TCP 连接的对端地址。
        if relay.ip().is_unspecified() {
            let peer = control
                .peer_addr()
                .map_err(|e| UpstreamError::Io(e.to_string()))?;
            relay = SocketAddr::new(peer.ip(), relay.port());
        }
        if relay.port() == 0 {
            return Err(UpstreamError::Protocol("UDP relay 端口无效".to_string()));
        }
        let bind = if relay.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let socket = tokio::net::UdpSocket::bind(bind)
            .await
            .map_err(|e| UpstreamError::Io(format!("绑定 UDP 失败: {e}")))?;
        socket
            .connect(relay)
            .await
            .map_err(|e| UpstreamError::Io(format!("连接 UDP relay 失败: {e}")))?;
        Ok(UdpSession { control, socket })
    }
}

impl From<socks5::Socks5Error> for UpstreamError {
    fn from(e: socks5::Socks5Error) -> Self {
        match e {
            socks5::Socks5Error::Io(msg) => UpstreamError::Io(msg),
            socks5::Socks5Error::Auth(msg) => UpstreamError::Auth(msg),
            socks5::Socks5Error::Protocol(msg) => UpstreamError::Protocol(msg),
            socks5::Socks5Error::Reply(rep) => UpstreamError::Reply(rep),
            socks5::Socks5Error::InvalidTarget(msg) => UpstreamError::InvalidTarget(msg),
        }
    }
}

/// 上游代理客户端。
#[derive(Debug, Clone)]
pub struct Upstream {
    server: UpstreamServer,
    timeouts: Timeouts,
}

impl Upstream {
    pub fn new(server: UpstreamServer, timeouts: Timeouts) -> Self {
        Self { server, timeouts }
    }

    /// 上游配置。
    pub fn server(&self) -> &UpstreamServer {
        &self.server
    }

    /// 经上游建立到 `host:port` 的 TCP 连接。
    pub async fn connect(&self, host: &str, port: u16) -> Result<TcpStream, UpstreamError> {
        match self.server.server_type.as_str() {
            "http" => self.connect_http(host, port).await,
            _ => self.connect_socks5(host, port).await,
        }
    }

    async fn connect_socks5(&self, host: &str, port: u16) -> Result<TcpStream, UpstreamError> {
        if host.is_empty() {
            return Err(UpstreamError::InvalidTarget("目标主机为空".to_string()));
        }
        let mut stream = self.open_socks5().await?;

        // 2) CONNECT 请求
        let (atyp, addr_bytes) = socks5::encode_address(host)?;
        let mut request = Vec::with_capacity(4 + addr_bytes.len() + 2);
        request.push(consts::VERSION);
        request.push(consts::CMD_CONNECT);
        request.push(0x00);
        request.push(atyp);
        request.extend_from_slice(&addr_bytes);
        request.extend_from_slice(&port.to_be_bytes());
        write_timeout(&mut stream, &request, self.timeouts.send_timeout()).await?;

        // 3) 消费响应
        let mut header = [0u8; 4];
        read_exact_timeout(&mut stream, &mut header, self.timeouts.recv_timeout()).await?;
        if header[0] != consts::VERSION {
            return Err(UpstreamError::Protocol(format!(
                "上游响应版本无效: {:#04x}",
                header[0]
            )));
        }
        if header[1] != consts::REPLY_SUCCESS {
            return Err(UpstreamError::Reply(header[1]));
        }
        let addr_len = match header[3] {
            consts::ATYP_IPV4 => 4,
            consts::ATYP_IPV6 => 16,
            consts::ATYP_DOMAIN => {
                let mut len_byte = [0u8; 1];
                read_exact_timeout(&mut stream, &mut len_byte, self.timeouts.recv_timeout())
                    .await?;
                len_byte[0] as usize
            }
            other => return Err(UpstreamError::Protocol(format!("上游 ATYP 未知: {other}"))),
        };
        discard(&mut stream, addr_len + 2, self.timeouts.recv_timeout()).await?;
        Ok(stream)
    }

    pub(crate) async fn open_socks5(&self) -> Result<TcpStream, UpstreamError> {
        let mut stream = tokio::time::timeout(
            self.timeouts.connect_timeout(),
            TcpStream::connect(self.server.endpoint()),
        )
        .await
        .map_err(|_| UpstreamError::Io("连接上游超时".to_string()))?
        .map_err(|e| UpstreamError::Io(format!("连接上游失败: {e}")))?;
        stream
            .set_nodelay(true)
            .map_err(|e| UpstreamError::Io(e.to_string()))?;

        // 1) 方法协商
        let auth = ProxyAuth {
            username: self.server.username.clone(),
            password: self.server.password.clone(),
        };
        let require_auth = self.server.managed.is_some();
        let mut methods = if require_auth {
            Vec::new()
        } else {
            vec![consts::AUTH_NONE]
        };
        if auth.has_credentials() {
            methods.push(consts::AUTH_USERPASS);
        }
        let mut greeting = Vec::with_capacity(2 + methods.len());
        greeting.push(consts::VERSION);
        greeting.push(methods.len() as u8);
        greeting.extend_from_slice(&methods);
        write_timeout(&mut stream, &greeting, self.timeouts.send_timeout()).await?;

        let mut method_resp = [0u8; 2];
        read_exact_timeout(&mut stream, &mut method_resp, self.timeouts.recv_timeout()).await?;
        if method_resp[0] != consts::VERSION {
            return Err(UpstreamError::Protocol(format!(
                "上游方法协商版本无效: {:#04x}{}",
                method_resp[0],
                if method_resp[0] == b'H' {
                    "（该端口是 HTTP 代理：请把上游类型改为 http）"
                } else {
                    ""
                }
            )));
        }
        if method_resp[1] == consts::AUTH_USERPASS {
            self.authenticate_userpass(&mut stream, &auth).await?;
        } else if require_auth || method_resp[1] != consts::AUTH_NONE {
            return Err(UpstreamError::Auth(format!(
                "上游要求不支持的认证方式: {:#04x}",
                method_resp[1]
            )));
        }

        Ok(stream)
    }

    /// RFC 1929 账密协商。
    async fn authenticate_userpass(
        &self,
        stream: &mut TcpStream,
        auth: &ProxyAuth,
    ) -> Result<(), UpstreamError> {
        if !auth.has_credentials() {
            return Err(UpstreamError::Auth(
                "上游要求账密认证但未提供 credentials".to_string(),
            ));
        }
        if auth.username.len() > 255 || auth.password.len() > 255 {
            return Err(UpstreamError::Auth(
                "用户名或密码长度超出 255 字节限制".to_string(),
            ));
        }
        let mut req = Vec::with_capacity(3 + auth.username.len() + auth.password.len());
        req.push(0x01);
        req.push(auth.username.len() as u8);
        req.extend_from_slice(auth.username.as_bytes());
        req.push(auth.password.len() as u8);
        req.extend_from_slice(auth.password.as_bytes());
        write_timeout(stream, &req, self.timeouts.send_timeout()).await?;

        let mut resp = [0u8; 2];
        read_exact_timeout(stream, &mut resp, self.timeouts.recv_timeout()).await?;
        if resp[1] != 0x00 {
            return Err(UpstreamError::Auth(format!(
                "上游账密认证失败, status={}",
                resp[1]
            )));
        }
        Ok(())
    }

    async fn connect_http(&self, host: &str, port: u16) -> Result<TcpStream, UpstreamError> {
        let auth = ProxyAuth {
            username: self.server.username.clone(),
            password: self.server.password.clone(),
        };
        let mut stream = tokio::time::timeout(
            self.timeouts.connect_timeout(),
            TcpStream::connect(self.server.endpoint()),
        )
        .await
        .map_err(|_| UpstreamError::Io("连接上游超时".to_string()))?
        .map_err(|e| UpstreamError::Io(format!("连接上游失败: {e}")))?;
        stream
            .set_nodelay(true)
            .map_err(|e| UpstreamError::Io(e.to_string()))?;

        let request = http_connect::build_connect_request(host, port, &auth);
        write_timeout(
            &mut stream,
            request.as_bytes(),
            self.timeouts.send_timeout(),
        )
        .await?;

        // 读响应头直到 \r\n\r\n（上限 1024）
        let mut response = Vec::with_capacity(256);
        let mut byte = [0u8; 1];
        loop {
            if response.len() > 1024 {
                return Err(UpstreamError::Protocol("上游响应头过长".to_string()));
            }
            read_exact_timeout(&mut stream, &mut byte, self.timeouts.recv_timeout()).await?;
            response.push(byte[0]);
            if response.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let text = String::from_utf8_lossy(&response).to_string();
        match http_connect::parse_status_code(&text) {
            Some(200) => Ok(stream),
            Some(code) => Err(UpstreamError::Protocol(format!(
                "上游 CONNECT 返回 {code}{}",
                http_connect::mismatch_hint(&text)
            ))),
            None => Err(UpstreamError::Protocol(format!(
                "上游响应解析失败, line=\"{}\"",
                http_connect::first_line(&text)
            ))),
        }
    }
}

async fn write_timeout(
    stream: &mut TcpStream,
    buf: &[u8],
    timeout: Duration,
) -> Result<(), UpstreamError> {
    tokio::time::timeout(timeout, stream.write_all(buf))
        .await
        .map_err(|_| UpstreamError::Io("发送超时".to_string()))?
        .map_err(|e| UpstreamError::Io(format!("发送失败: {e}")))
}

async fn read_exact_timeout(
    stream: &mut TcpStream,
    buf: &mut [u8],
    timeout: Duration,
) -> Result<(), UpstreamError> {
    tokio::time::timeout(timeout, stream.read_exact(buf))
        .await
        .map_err(|_| UpstreamError::Io("读取超时".to_string()))?
        .map(|_| ())
        .map_err(|e| UpstreamError::Io(format!("读取失败: {e}")))
}

async fn discard(
    stream: &mut TcpStream,
    len: usize,
    timeout: Duration,
) -> Result<(), UpstreamError> {
    let mut buf = vec![0u8; len];
    tokio::time::timeout(timeout, stream.read_exact(&mut buf))
        .await
        .map_err(|_| UpstreamError::Io("读取超时".to_string()))?
        .map(|_| ())
        .map_err(|e| UpstreamError::Io(format!("读取失败: {e}")))
}
