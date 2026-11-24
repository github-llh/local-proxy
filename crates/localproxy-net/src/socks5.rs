//! SOCKS5 协议客户端（同步 IO，适用于 shim 注入热路径）。
//!
//! 移植自旧项目 `src/network/Socks5.hpp`：
//! - 方法协商（AUTH_NONE / RFC 1929 AUTH_USERPASS）
//! - CONNECT 请求（ATYP: IPV4 / DOMAIN / IPV6）
//! - UDP ASSOCIATE
//! - 错误码文案与协议错配提示（SOCKS5 配到 HTTP 端口等常见事故）

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::time::Duration;

/// SOCKS5 协议常量。
pub mod consts {
    pub const VERSION: u8 = 0x05;
    pub const AUTH_NONE: u8 = 0x00;
    pub const AUTH_USERPASS: u8 = 0x02;
    pub const CMD_CONNECT: u8 = 0x01;
    pub const CMD_UDP_ASSOCIATE: u8 = 0x03;
    pub const ATYP_IPV4: u8 = 0x01;
    pub const ATYP_DOMAIN: u8 = 0x03;
    pub const ATYP_IPV6: u8 = 0x04;
    pub const REPLY_SUCCESS: u8 = 0x00;
}

/// 代理认证信息。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProxyAuth {
    pub username: String,
    pub password: String,
}

impl ProxyAuth {
    pub fn has_credentials(&self) -> bool {
        !self.username.is_empty() || !self.password.is_empty()
    }
}

/// 超时配置（毫秒，非正值回退默认 5000）。
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub connect_ms: i64,
    pub send_ms: i64,
    pub recv_ms: i64,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect_ms: 5000,
            send_ms: 5000,
            recv_ms: 5000,
        }
    }
}

impl Timeouts {
    fn normalize(ms: i64) -> u64 {
        if ms > 0 {
            ms as u64
        } else {
            5000
        }
    }
    pub fn connect_timeout(&self) -> Duration {
        Duration::from_millis(Self::normalize(self.connect_ms))
    }
    pub fn send_timeout(&self) -> Duration {
        Duration::from_millis(Self::normalize(self.send_ms))
    }
    pub fn recv_timeout(&self) -> Duration {
        Duration::from_millis(Self::normalize(self.recv_ms))
    }
}

/// SOCKS5 错误。
#[derive(Debug)]
pub enum Socks5Error {
    /// IO 失败（含超时）
    Io(String),
    /// 认证协商失败
    Auth(String),
    /// 协议响应非法（附收到的首字节，用于错配提示）
    Protocol(String),
    /// 代理返回非成功 REPLY（附 REP 码与文案）
    Reply(u8),
    /// 目标主机为空/过长等参数问题
    InvalidTarget(String),
}

impl std::fmt::Display for Socks5Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Socks5Error::Io(msg) => write!(f, "IO 失败: {msg}"),
            Socks5Error::Auth(msg) => write!(f, "认证失败: {msg}"),
            Socks5Error::Protocol(msg) => write!(f, "协议错误: {msg}"),
            Socks5Error::Reply(rep) => write!(f, "代理拒绝 (REP={}, {})", rep, reply_to_text(*rep)),
            Socks5Error::InvalidTarget(msg) => write!(f, "目标非法: {msg}"),
        }
    }
}

impl std::error::Error for Socks5Error {}

/// REPLY 码中文文案（移植旧 `ReplyToText`）。
pub fn reply_to_text(rep: u8) -> &'static str {
    match rep {
        0x00 => "成功",
        0x01 => "通用失败",
        0x02 => "连接不允许(规则集)",
        0x03 => "网络不可达",
        0x04 => "主机不可达",
        0x05 => "连接被拒绝",
        0x06 => "TTL 已过期",
        0x07 => "不支持的命令",
        0x08 => "不支持的地址类型",
        _ => "未知",
    }
}

/// 协议错配提示：代理未按 SOCKS5 应答时（超时/重置）给出常见原因。
pub fn mismatch_hint_for_error() -> &'static str {
    "（提示：代理未按 SOCKS5 应答，该端口可能是 HTTP 代理端口：请把代理类型改为 http，\
     或改用代理软件的 SOCKS5 / 混合端口）"
}

fn read_exact(
    stream: &mut TcpStream,
    buf: &mut [u8],
    timeout: Duration,
) -> Result<(), Socks5Error> {
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| Socks5Error::Io(e.to_string()))?;
    stream
        .read_exact(buf)
        .map_err(|e| Socks5Error::Io(format!("读取 {} 字节失败: {e}", buf.len())))
}

fn send_all(stream: &mut TcpStream, buf: &[u8], timeout: Duration) -> Result<(), Socks5Error> {
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| Socks5Error::Io(e.to_string()))?;
    stream
        .write_all(buf)
        .map_err(|e| Socks5Error::Io(format!("发送 {} 字节失败: {e}", buf.len())))
}

/// 方法协商 + RFC 1929 账质子协商（移植旧 `Authenticate`）。
fn authenticate(
    stream: &mut TcpStream,
    auth: &ProxyAuth,
    send_timeout: Duration,
    recv_timeout: Duration,
) -> Result<(), Socks5Error> {
    let has_auth = auth.has_credentials();
    let mut methods = vec![consts::AUTH_NONE];
    if has_auth {
        methods.push(consts::AUTH_USERPASS);
    }
    let mut request = Vec::with_capacity(2 + methods.len());
    request.push(consts::VERSION);
    request.push(methods.len() as u8);
    request.extend_from_slice(&methods);
    send_all(stream, &request, send_timeout)?;

    let mut response = [0u8; 2];
    read_exact(stream, &mut response, recv_timeout)?;

    if response[0] != consts::VERSION {
        let looks_http = response[0] == b'H';
        return Err(Socks5Error::Protocol(format!(
            "方法协商版本无效: {:#04x}{}",
            response[0],
            if looks_http {
                "（该端口返回了 HTTP 响应，是 HTTP 代理端口：请把代理类型改为 http）"
            } else {
                ""
            }
        )));
    }

    if response[1] == consts::AUTH_USERPASS {
        if !has_auth {
            return Err(Socks5Error::Auth(
                "代理服务器要求用户名密码认证，但未提供 credentials".to_string(),
            ));
        }
        if auth.username.len() > 255 || auth.password.len() > 255 {
            return Err(Socks5Error::Auth(
                "用户名或密码长度超出 255 字节限制".to_string(),
            ));
        }
        let mut up = Vec::with_capacity(3 + auth.username.len() + auth.password.len());
        up.push(0x01); // 子协商版本
        up.push(auth.username.len() as u8);
        up.extend_from_slice(auth.username.as_bytes());
        up.push(auth.password.len() as u8);
        up.extend_from_slice(auth.password.as_bytes());
        send_all(stream, &up, send_timeout)?;

        let mut up_resp = [0u8; 2];
        read_exact(stream, &mut up_resp, recv_timeout)?;
        if up_resp[1] != 0x00 {
            return Err(Socks5Error::Auth(format!(
                "用户名或密码认证失败, status={}",
                up_resp[1]
            )));
        }
        return Ok(());
    }

    if response[1] == consts::AUTH_NONE {
        return Ok(());
    }

    Err(Socks5Error::Auth(format!(
        "不支持的认证方式: {:#04x}",
        response[1]
    )))
}

/// 编码 CONNECT/UDP-ASSOCIATE 请求的地址部分，返回 (ATYP, 编码字节)。
pub fn encode_address(host: &str) -> Result<(u8, Vec<u8>), Socks5Error> {
    if let Ok(v4) = host.parse::<Ipv4Addr>() {
        return Ok((consts::ATYP_IPV4, v4.octets().to_vec()));
    }
    if let Ok(v6) = host.parse::<Ipv6Addr>() {
        return Ok((consts::ATYP_IPV6, v6.octets().to_vec()));
    }
    if host.is_empty() {
        return Err(Socks5Error::InvalidTarget("目标主机为空".to_string()));
    }
    if host.len() > 255 {
        return Err(Socks5Error::InvalidTarget(format!(
            "目标域名过长: len={}",
            host.len()
        )));
    }
    let mut out = Vec::with_capacity(1 + host.len());
    out.push(host.len() as u8);
    out.extend_from_slice(host.as_bytes());
    Ok((consts::ATYP_DOMAIN, out))
}

// ============= 纯协议构造/解析（供 shim 就地协商复用） =============
//
// shim 注入场景下，被拦截的 fd/socket 已由应用创建，无法替换 fd 号
// （Windows 无 dup2 等价物）。因此握手改为「原 socket 就地协商」：
// 平台层负责 IO，协议字节的构造与解析统一收敛到这里。

/// 构造方法协商问候（VER NMETHODS METHODS...）。
pub fn build_greeting(auth: &ProxyAuth) -> Vec<u8> {
    let has_auth = auth.has_credentials();
    let mut methods = vec![consts::AUTH_NONE];
    if has_auth {
        methods.push(consts::AUTH_USERPASS);
    }
    let mut req = Vec::with_capacity(2 + methods.len());
    req.push(consts::VERSION);
    req.push(methods.len() as u8);
    req.extend_from_slice(&methods);
    req
}

/// 方法协商结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodChoice {
    /// 无需认证
    None,
    /// 需要 RFC 1929 账密子协商
    UserPass,
}

/// 解析方法协商响应（2 字节）。
pub fn parse_method_response(buf: &[u8; 2]) -> Result<MethodChoice, Socks5Error> {
    if buf[0] != consts::VERSION {
        let looks_http = buf[0] == b'H';
        return Err(Socks5Error::Protocol(format!(
            "方法协商版本无效: {:#04x}{}",
            buf[0],
            if looks_http {
                "（该端口返回了 HTTP 响应，是 HTTP 代理端口：请把代理类型改为 http）"
            } else {
                ""
            }
        )));
    }
    match buf[1] {
        consts::AUTH_NONE => Ok(MethodChoice::None),
        consts::AUTH_USERPASS => Ok(MethodChoice::UserPass),
        other => Err(Socks5Error::Auth(format!(
            "不支持的认证方式: {:#04x}",
            other
        ))),
    }
}

/// 构造 RFC 1929 账密子协商请求。
pub fn build_userpass_request(auth: &ProxyAuth) -> Result<Vec<u8>, Socks5Error> {
    if auth.username.len() > 255 || auth.password.len() > 255 {
        return Err(Socks5Error::Auth(
            "用户名或密码长度超出 255 字节限制".to_string(),
        ));
    }
    let mut req = Vec::with_capacity(3 + auth.username.len() + auth.password.len());
    req.push(0x01);
    req.push(auth.username.len() as u8);
    req.extend_from_slice(auth.username.as_bytes());
    req.push(auth.password.len() as u8);
    req.extend_from_slice(auth.password.as_bytes());
    Ok(req)
}

/// 解析账密子协商响应（2 字节，status 在 [1]）。
pub fn parse_userpass_response(buf: &[u8; 2]) -> Result<(), Socks5Error> {
    if buf[1] != 0x00 {
        return Err(Socks5Error::Auth(format!(
            "用户名或密码认证失败, status={}",
            buf[1]
        )));
    }
    Ok(())
}

/// 构造 CONNECT 请求。
pub fn build_connect_request(host: &str, port: u16) -> Result<Vec<u8>, Socks5Error> {
    let (atyp, addr_bytes) = encode_address(host)?;
    let mut request = Vec::with_capacity(4 + addr_bytes.len() + 2);
    request.push(consts::VERSION);
    request.push(consts::CMD_CONNECT);
    request.push(0x00);
    request.push(atyp);
    request.extend_from_slice(&addr_bytes);
    request.extend_from_slice(&port.to_be_bytes());
    Ok(request)
}

/// CONNECT/UDP-ASSOCIATE 响应解析结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplyHeader {
    /// REPLY 码（0x00 = 成功）
    pub rep: u8,
    /// BND.ATYP
    pub atyp: u8,
    /// 地址部分字节数（ATYP_DOMAIN 需再读 1 字节长度）
    pub addr_len: usize,
}

/// 解析响应头（4 字节：VER REP RSV ATYP）。
pub fn parse_reply_header(buf: &[u8; 4]) -> Result<ReplyHeader, Socks5Error> {
    if buf[0] != consts::VERSION {
        return Err(Socks5Error::Protocol(format!(
            "响应版本无效: {:#04x}",
            buf[0]
        )));
    }
    let addr_len = match buf[3] {
        consts::ATYP_IPV4 => 4,
        consts::ATYP_IPV6 => 16,
        consts::ATYP_DOMAIN => usize::MAX, // 长度字节待读
        other => return Err(Socks5Error::Protocol(format!("未知 ATYP: {other}"))),
    };
    Ok(ReplyHeader {
        rep: buf[1],
        atyp: buf[3],
        addr_len,
    })
}

/// 响应成功后需要消费的剩余字节数（addr + port）。
pub fn reply_tail_len(atyp: u8, domain_len: Option<u8>) -> Option<usize> {
    match atyp {
        consts::ATYP_IPV4 => Some(4 + 2),
        consts::ATYP_IPV6 => Some(16 + 2),
        consts::ATYP_DOMAIN => {
            let len = domain_len? as usize;
            Some(len + 2)
        }
        _ => None,
    }
}

/// 读取 CONNECT/UDP-ASSOCIATE 响应，消费 BND.ADDR/PORT，返回 (REP, BND 地址)。
fn read_reply(
    stream: &mut TcpStream,
    recv_timeout: Duration,
) -> Result<(u8, Option<SocketAddr>), Socks5Error> {
    let mut header = [0u8; 4];
    read_exact(stream, &mut header, recv_timeout)?;
    if header[0] != consts::VERSION {
        return Err(Socks5Error::Protocol(format!(
            "响应版本无效: {:#04x}",
            header[0]
        )));
    }
    let (rep, atyp) = (header[1], header[3]);
    let mut bnd: Option<SocketAddr> = None;
    if rep == consts::REPLY_SUCCESS {
        let addr_len = match atyp {
            consts::ATYP_IPV4 => 4,
            consts::ATYP_IPV6 => 16,
            consts::ATYP_DOMAIN => {
                let mut len_byte = [0u8; 1];
                read_exact(stream, &mut len_byte, recv_timeout)?;
                len_byte[0] as usize
            }
            other => {
                return Err(Socks5Error::Protocol(format!("未知 ATYP: {other}")));
            }
        };
        let mut addr_buf = vec![0u8; addr_len];
        if addr_len > 0 {
            read_exact(stream, &mut addr_buf, recv_timeout)?;
        }
        let mut port_buf = [0u8; 2];
        read_exact(stream, &mut port_buf, recv_timeout)?;
        let port = u16::from_be_bytes(port_buf);
        bnd = match atyp {
            consts::ATYP_IPV4 => Some(SocketAddr::from((
                Ipv4Addr::new(addr_buf[0], addr_buf[1], addr_buf[2], addr_buf[3]),
                port,
            ))),
            consts::ATYP_IPV6 => {
                let mut octets = [0u8; 16];
                octets.copy_from_slice(&addr_buf);
                Some(SocketAddr::from((Ipv6Addr::from(octets), port)))
            }
            consts::ATYP_DOMAIN => String::from_utf8(addr_buf)
                .ok()
                .and_then(|d| d.to_socket_addrs().ok())
                .and_then(|mut it| it.next()),
            _ => None,
        };
    }
    Ok((rep, bnd))
}

/// SOCKS5 CONNECT 隧道（移植旧 `Handshake`）。
///
/// 成功时返回已通过代理建立到目标的 TCP 流。
pub fn connect<A: ToSocketAddrs>(
    proxy: A,
    target_host: &str,
    target_port: u16,
    auth: &ProxyAuth,
    timeouts: &Timeouts,
) -> Result<TcpStream, Socks5Error> {
    if target_host.is_empty() {
        return Err(Socks5Error::InvalidTarget("目标主机为空".to_string()));
    }
    let mut stream = TcpStream::connect_timeout(
        &proxy
            .to_socket_addrs()
            .map_err(|e| Socks5Error::Io(e.to_string()))?
            .next()
            .ok_or_else(|| Socks5Error::Io("代理地址解析失败".to_string()))?,
        timeouts.connect_timeout(),
    )
    .map_err(|e| Socks5Error::Io(format!("连接代理失败: {e}")))?;

    stream
        .set_nodelay(true)
        .map_err(|e| Socks5Error::Io(e.to_string()))?;
    authenticate(
        &mut stream,
        auth,
        timeouts.send_timeout(),
        timeouts.recv_timeout(),
    )?;

    let (atyp, addr_bytes) = encode_address(target_host)?;
    let mut request = Vec::with_capacity(4 + addr_bytes.len() + 2);
    request.push(consts::VERSION);
    request.push(consts::CMD_CONNECT);
    request.push(0x00); // RSV
    request.push(atyp);
    request.extend_from_slice(&addr_bytes);
    request.extend_from_slice(&target_port.to_be_bytes());
    send_all(&mut stream, &request, timeouts.send_timeout())?;

    let (rep, _bnd) = read_reply(&mut stream, timeouts.recv_timeout())?;
    if rep != consts::REPLY_SUCCESS {
        return Err(Socks5Error::Reply(rep));
    }
    Ok(stream)
}

/// UDP ASSOCIATE 结果：UDP 套接字 + 中继地址。
pub struct UdpAssociation {
    pub socket: UdpSocket,
    pub relay: SocketAddr,
}

/// SOCKS5 UDP ASSOCIATE（移植旧 Socks5Udp 流程）。
///
/// `bind` 为本地 UDP 绑定地址；`auth`/`timeouts` 同 CONNECT。
pub fn udp_associate<A: ToSocketAddrs>(
    proxy: A,
    bind: &SocketAddr,
    auth: &ProxyAuth,
    timeouts: &Timeouts,
) -> Result<UdpAssociation, Socks5Error> {
    let mut control = TcpStream::connect_timeout(
        &proxy
            .to_socket_addrs()
            .map_err(|e| Socks5Error::Io(e.to_string()))?
            .next()
            .ok_or_else(|| Socks5Error::Io("代理地址解析失败".to_string()))?,
        timeouts.connect_timeout(),
    )
    .map_err(|e| Socks5Error::Io(format!("连接代理失败: {e}")))?;
    authenticate(
        &mut control,
        auth,
        timeouts.send_timeout(),
        timeouts.recv_timeout(),
    )?;

    // 请求头中 DST.ADDR/PORT 填 0.0.0.0:0（客户端尚未知中继地址）
    let mut request = Vec::with_capacity(10);
    request.push(consts::VERSION);
    request.push(consts::CMD_UDP_ASSOCIATE);
    request.push(0x00); // RSV
    request.push(consts::ATYP_IPV4);
    request.extend_from_slice(&[0, 0, 0, 0]);
    request.extend_from_slice(&[0, 0]);
    send_all(&mut control, &request, timeouts.send_timeout())?;

    let (_rep, bnd) = read_reply(&mut control, timeouts.recv_timeout())?;
    let relay =
        bnd.ok_or_else(|| Socks5Error::Protocol("UDP ASSOCIATE 响应缺少 BND.ADDR".to_string()))?;

    let socket =
        UdpSocket::bind(bind).map_err(|e| Socks5Error::Io(format!("绑定 UDP 失败: {e}")))?;
    // 控制连接必须保持存活；将其交给调用方管理（此处遗忘会导致中继失效），
    // 因此把 control stream 的 fd 泄漏防护交给 UdpAssociation：保持持有。
    std::mem::forget(control); // 控制连接生命周期 = 关联生命周期
    Ok(UdpAssociation { socket, relay })
}

/// 编码 SOCKS5 UDP 请求包：RSV(2) FRAG(0) ATYP DST.ADDR DST.PORT DATA。
pub fn encode_udp_packet(
    target_host: &str,
    target_port: u16,
    payload: &[u8],
) -> Result<Vec<u8>, Socks5Error> {
    let (atyp, addr_bytes) = encode_address(target_host)?;
    let mut out = Vec::with_capacity(4 + addr_bytes.len() + 2 + payload.len());
    out.extend_from_slice(&[0x00, 0x00]); // RSV
    out.push(0x00); // FRAG = 0（不支持分片）
    out.push(atyp);
    out.extend_from_slice(&addr_bytes);
    out.extend_from_slice(&target_port.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// 解码 SOCKS5 UDP 响应包，返回 (源主机, 源端口, 数据)。
pub fn decode_udp_packet(buf: &[u8]) -> Option<(String, u16, &[u8])> {
    if buf.len() < 4 {
        return None;
    }
    if buf[0] != 0 || buf[1] != 0 {
        return None; // RSV
    }
    // FRAG != 0 暂不支持
    if buf[2] != 0 {
        return None;
    }
    let atyp = buf[3];
    let (host, addr_len) = match atyp {
        consts::ATYP_IPV4 => {
            if buf.len() < 4 + 4 + 2 {
                return None;
            }
            (Ipv4Addr::new(buf[4], buf[5], buf[6], buf[7]).to_string(), 4)
        }
        consts::ATYP_IPV6 => {
            if buf.len() < 4 + 16 + 2 {
                return None;
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&buf[4..20]);
            (Ipv6Addr::from(octets).to_string(), 16)
        }
        consts::ATYP_DOMAIN => {
            let len = *buf.get(4)? as usize;
            if buf.len() < 4 + 1 + len + 2 {
                return None;
            }
            (
                String::from_utf8_lossy(&buf[5..5 + len]).to_string(),
                1 + len,
            )
        }
        _ => return None,
    };
    let port_off = 4 + addr_len;
    let port = u16::from_be_bytes([buf[port_off], buf[port_off + 1]]);
    Some((host, port, &buf[port_off + 2..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_texts_match_legacy() {
        assert_eq!(reply_to_text(0x00), "成功");
        assert_eq!(reply_to_text(0x05), "连接被拒绝");
        assert_eq!(reply_to_text(0xFF), "未知");
    }

    #[test]
    fn encode_address_variants() {
        let (atyp, bytes) = encode_address("1.2.3.4").unwrap();
        assert_eq!(atyp, consts::ATYP_IPV4);
        assert_eq!(bytes, vec![1, 2, 3, 4]);

        let (atyp, bytes) = encode_address("example.com").unwrap();
        assert_eq!(atyp, consts::ATYP_DOMAIN);
        assert_eq!(bytes[0], 11);
        assert_eq!(&bytes[1..], b"example.com");

        let (atyp, bytes) = encode_address("::1").unwrap();
        assert_eq!(atyp, consts::ATYP_IPV6);
        assert_eq!(bytes.len(), 16);

        assert!(encode_address("").is_err());
        assert!(encode_address(&"a".repeat(256)).is_err());
    }

    #[test]
    fn udp_packet_roundtrip() {
        let packet = encode_udp_packet("example.com", 443, b"hello").unwrap();
        assert_eq!(packet[0..3], [0, 0, 0]); // RSV + FRAG
        let (host, port, data) = decode_udp_packet(&packet).unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 443);
        assert_eq!(data, b"hello");

        let packet = encode_udp_packet("8.8.8.8", 53, &[1, 2, 3]).unwrap();
        let (host, port, data) = decode_udp_packet(&packet).unwrap();
        assert_eq!(host, "8.8.8.8");
        assert_eq!(port, 53);
        assert_eq!(data, &[1, 2, 3]);

        assert!(decode_udp_packet(&[0, 0]).is_none());
        assert!(decode_udp_packet(&[1, 0, 0, 1, 0, 0, 0, 0, 0]).is_none()); // RSV 非法
    }

    #[test]
    fn timeouts_default_and_normalize() {
        let t = Timeouts::default();
        assert_eq!(t.connect_timeout(), Duration::from_millis(5000));
        let bad = Timeouts {
            connect_ms: 0,
            send_ms: -5,
            recv_ms: 100,
        };
        assert_eq!(bad.connect_timeout(), Duration::from_millis(5000));
        assert_eq!(bad.send_timeout(), Duration::from_millis(5000));
        assert_eq!(bad.recv_timeout(), Duration::from_millis(100));
    }

    /// 本地 mock SOCKS5 服务器：验证完整 CONNECT 握手（含域名 ATYP 与响应消费）。
    #[test]
    fn connect_handshake_against_mock() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 64];
            // 1) 方法协商
            let n = stream.read(&mut buf).unwrap();
            assert_eq!(buf[0], consts::VERSION);
            assert_eq!(buf[1], 1); // 无认证
            assert_eq!(buf[2], consts::AUTH_NONE);
            let _ = n;
            stream
                .write_all(&[consts::VERSION, consts::AUTH_NONE])
                .unwrap();
            // 2) CONNECT 请求（example.com）
            let n = stream.read(&mut buf).unwrap();
            assert_eq!(buf[1], consts::CMD_CONNECT);
            assert_eq!(buf[3], consts::ATYP_DOMAIN);
            let len = buf[4] as usize;
            assert_eq!(&buf[5..5 + len], b"example.com");
            let _ = n;
            // 3) 响应：VER REP RSV ATYP + BND.ADDR(4) + BND.PORT(2)
            stream
                .write_all(&[
                    consts::VERSION,
                    consts::REPLY_SUCCESS,
                    0x00,
                    consts::ATYP_IPV4,
                    10,
                    0,
                    0,
                    1,
                    0x1F,
                    0x90,
                ])
                .unwrap();
            // 保持连接直到客户端 drop
            let mut sink = [0u8; 8];
            let _ = stream.read(&mut sink);
        });

        let stream = connect(
            addr,
            "example.com",
            443,
            &ProxyAuth::default(),
            &Timeouts::default(),
        )
        .unwrap();
        assert!(stream.peer_addr().is_ok());
        drop(stream);
        handle.join().unwrap();
    }

    /// 代理拒绝时错误携带 REP 文案。
    #[test]
    fn connect_reply_failure_surfaces_rep() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 64];
            let _ = stream.read(&mut buf);
            stream
                .write_all(&[consts::VERSION, consts::AUTH_NONE])
                .unwrap();
            let _ = stream.read(&mut buf);
            // REP=0x02 连接不允许
            stream
                .write_all(&[
                    consts::VERSION,
                    0x02,
                    0x00,
                    consts::ATYP_IPV4,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                ])
                .unwrap();
        });

        let err = connect(
            addr,
            "example.com",
            443,
            &ProxyAuth::default(),
            &Timeouts::default(),
        )
        .unwrap_err();
        assert!(matches!(err, Socks5Error::Reply(0x02)));
        assert!(err.to_string().contains("连接不允许"));
        handle.join().unwrap();
    }
}
