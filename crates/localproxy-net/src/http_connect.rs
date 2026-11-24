//! HTTP CONNECT 隧道客户端（同步 IO）。
//!
//! 移植自旧项目 `src/network/HttpConnect.hpp`：
//! - `CONNECT host:port HTTP/1.1` 请求构造（IPv6 字面量加方括号）
//! - `Proxy-Authorization: Basic` 认证
//! - 状态码解析与 407 专门提示
//! - 协议错配提示（HTTP 配到 SOCKS5 端口）

use std::io::{Read, Write};
use std::net::{Ipv6Addr, TcpStream, ToSocketAddrs};

use crate::socks5::Timeouts;

/// Base64 编码（无第三方依赖）。
pub fn base64_encode(input: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((n >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((n >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            CHARS[((n >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            CHARS[(n & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// HTTP CONNECT 错误。
#[derive(Debug)]
pub enum HttpConnectError {
    Io(String),
    /// 状态码非 200（407 单独提示认证）
    Status(u16),
    /// 响应无法解析
    MalformedResponse(String),
}

impl std::fmt::Display for HttpConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HttpConnectError::Io(msg) => write!(f, "IO 失败: {msg}"),
            HttpConnectError::Status(407) => {
                write!(f, "代理需要身份认证或认证失败 (407 Proxy Authentication Required)，请检查代理用户名/密码")
            }
            HttpConnectError::Status(code) => write!(f, "代理返回状态码 {code}"),
            HttpConnectError::MalformedResponse(msg) => write!(f, "响应解析失败: {msg}"),
        }
    }
}

impl std::error::Error for HttpConnectError {}

/// 构造 CONNECT 请求（IPv6 字面量加方括号，符合 HTTP CONNECT 语法）。
pub fn build_connect_request(
    target_host: &str,
    target_port: u16,
    auth: &super::socks5::ProxyAuth,
) -> String {
    let host_for_header = if target_host.parse::<Ipv6Addr>().is_ok() {
        format!("[{target_host}]")
    } else {
        target_host.to_string()
    };
    let mut request = format!(
        "CONNECT {host_for_header}:{target_port} HTTP/1.1\r\nHost: {host_for_header}:{target_port}\r\n"
    );
    if auth.has_credentials() {
        let credentials = format!("{}:{}", auth.username, auth.password);
        request.push_str(&format!(
            "Proxy-Authorization: Basic {}\r\n",
            base64_encode(credentials.as_bytes())
        ));
    }
    request.push_str("\r\n");
    request
}

/// 解析响应状态码（`HTTP/1.x NNN ...` 的首行三数字）。
pub fn parse_status_code(response: &str) -> Option<u16> {
    let space = response.find(' ')?;
    let code = response.get(space + 1..space + 4)?;
    code.parse().ok()
}

/// 协议错配提示：对端按 SOCKS5 应答（首字节 0x05）时说明配错了类型。
pub fn mismatch_hint(response: &str) -> String {
    if response.as_bytes().first() == Some(&0x05) {
        return "（该端口按 SOCKS5 协议应答，是 SOCKS5 端口：请把代理类型改为 socks5）".to_string();
    }
    String::new()
}

/// 响应首行（截断到 \r\n，用于日志）。
pub fn first_line(s: &str) -> &str {
    s.split("\r\n").next().unwrap_or(s)
}

/// 执行 HTTP CONNECT 握手（移植旧 `Handshake`）。
///
/// `stream` 需已连接到 HTTP 代理；成功返回隧道可用的流。
pub fn handshake(
    stream: &mut TcpStream,
    target_host: &str,
    target_port: u16,
    auth: &super::socks5::ProxyAuth,
    timeouts: &Timeouts,
) -> Result<(), HttpConnectError> {
    stream
        .set_nodelay(true)
        .map_err(|e| HttpConnectError::Io(e.to_string()))?;
    let request = build_connect_request(target_host, target_port, auth);

    stream
        .set_write_timeout(Some(timeouts.send_timeout()))
        .map_err(|e| HttpConnectError::Io(e.to_string()))?;
    stream
        .write_all(request.as_bytes())
        .map_err(|e| HttpConnectError::Io(format!("发送 CONNECT 请求失败: {e}")))?;

    // 接收响应头直到 \r\n\r\n（上限 1024 字节）
    stream
        .set_read_timeout(Some(timeouts.recv_timeout()))
        .map_err(|e| HttpConnectError::Io(e.to_string()))?;
    let mut response = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        if response.len() > 1024 {
            return Err(HttpConnectError::MalformedResponse(
                "响应头过长或不完整".to_string(),
            ));
        }
        match stream.read(&mut byte) {
            Ok(1) => {
                response.push(byte[0]);
                if response.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            Ok(_) => {
                return Err(HttpConnectError::Io("连接被对端关闭".to_string()));
            }
            Err(e) => {
                return Err(HttpConnectError::Io(format!("接收响应失败: {e}")));
            }
        }
    }

    let response_text = String::from_utf8_lossy(&response).to_string();
    let Some(status) = parse_status_code(&response_text) else {
        return Err(HttpConnectError::MalformedResponse(format!(
            "解析响应状态码失败, line=\"{}\"{}",
            first_line(&response_text),
            mismatch_hint(&response_text)
        )));
    };
    if status != 200 {
        return Err(HttpConnectError::Status(status));
    }
    Ok(())
}

/// 连接 HTTP 代理并完成 CONNECT 握手，返回隧道流（移植旧 `Handshake` 外层）。
pub fn connect<A: ToSocketAddrs>(
    proxy: A,
    target_host: &str,
    target_port: u16,
    auth: &super::socks5::ProxyAuth,
    timeouts: &Timeouts,
) -> Result<TcpStream, HttpConnectError> {
    let addr = proxy
        .to_socket_addrs()
        .map_err(|e| HttpConnectError::Io(e.to_string()))?
        .next()
        .ok_or_else(|| HttpConnectError::Io("代理地址解析失败".to_string()))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeouts.connect_timeout())
        .map_err(|e| HttpConnectError::Io(format!("连接代理失败: {e}")))?;
    handshake(&mut stream, target_host, target_port, auth, timeouts)?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn connect_request_shape() {
        let req = build_connect_request("example.com", 443, &crate::socks5::ProxyAuth::default());
        assert!(req.starts_with("CONNECT example.com:443 HTTP/1.1\r\n"));
        assert!(req.contains("Host: example.com:443\r\n"));
        assert!(req.ends_with("\r\n\r\n"));
        assert!(!req.contains("Proxy-Authorization"));

        let req = build_connect_request(
            "::1",
            8080,
            &crate::socks5::ProxyAuth {
                username: "u".into(),
                password: "p".into(),
            },
        );
        assert!(req.starts_with("CONNECT [::1]:8080 HTTP/1.1\r\n"));
        assert!(req.contains("Proxy-Authorization: Basic dTpw"));
    }

    #[test]
    fn status_code_parse() {
        assert_eq!(
            parse_status_code("HTTP/1.1 200 Connection Established\r\n\r\n"),
            Some(200)
        );
        assert_eq!(parse_status_code("HTTP/1.0 407 Proxy Auth\r\n"), Some(407));
        assert_eq!(parse_status_code("garbage"), None);
    }

    #[test]
    fn mismatch_hint_detects_socks5() {
        assert!(mismatch_hint("\u{5}\u{0}\u{0}...").contains("SOCKS5"));
        assert!(mismatch_hint("HTTP/1.1 400").is_empty());
    }

    /// mock HTTP 代理：完整 CONNECT 隧道建立。
    #[test]
    fn handshake_against_mock() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 512];
            let n = stream.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(req.starts_with("CONNECT example.com:443 "));
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .unwrap();
            let mut sink = [0u8; 8];
            let _ = stream.read(&mut sink);
        });

        let stream = connect(
            addr,
            "example.com",
            443,
            &crate::socks5::ProxyAuth::default(),
            &Timeouts::default(),
        )
        .unwrap();
        assert!(stream.peer_addr().is_ok());
        drop(stream);
        handle.join().unwrap();
    }

    /// 407 时错误信息指向认证配置。
    #[test]
    fn handshake_407_message() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 512];
            let _ = stream.read(&mut buf);
            stream
                .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                .unwrap();
        });

        let err = connect(
            addr,
            "example.com",
            443,
            &crate::socks5::ProxyAuth::default(),
            &Timeouts::default(),
        )
        .unwrap_err();
        assert!(matches!(err, HttpConnectError::Status(407)));
        assert!(err.to_string().contains("407"));
        assert!(err.to_string().contains("认证"));
        handle.join().unwrap();
    }
}
