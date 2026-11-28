//! 通过本机 Clash/mihomo API 查询策略组与切换节点。
//!
//! 传输层同时支持两种控制面形态：
//! - TCP 回环端口（mihomo、ClashX、Clash for Windows 默认）
//! - Unix domain socket（Clash Verge Rev 默认：`verge-mihomo -ext-ctl-unix ...`）
//!
//! 手写 HTTP/1.1（一次请求一个连接，`Connection: close`），因此不需要 HTTP
//! 客户端依赖；同时显式绕开系统代理——只连本机 socket / 回环端口。

use std::time::Duration;

use localproxy_core::config::{ClashController, ClashEndpoint};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 64 * 1024;

pub struct ClashClient {
    endpoint: ClashEndpoint,
    /// 请求行里展示的端点（错误提示用）。
    label: String,
    host_header: String,
    secret: String,
}

#[derive(Debug, Serialize)]
pub struct ClashState {
    mode: String,
    groups: Vec<ProxyGroup>,
}

#[derive(Debug, Serialize)]
struct ProxyGroup {
    name: String,
    selected: String,
    choices: Vec<String>,
}

#[derive(Deserialize)]
struct ClashConfig {
    mode: String,
}

#[derive(Deserialize)]
struct ProxiesResponse {
    proxies: std::collections::BTreeMap<String, ProxyEntry>,
}

#[derive(Deserialize)]
struct ProxyEntry {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    now: String,
    #[serde(default)]
    all: Vec<String>,
    #[serde(default)]
    hidden: bool,
}

impl ClashClient {
    pub fn new(controller: &ClashController) -> Result<Self, String> {
        let endpoint = controller.endpoint()?;
        let (label, host_header) = match &endpoint {
            ClashEndpoint::Tcp(addr) => (addr.to_string(), addr.to_string()),
            ClashEndpoint::Unix(path) => {
                (format!("unix:{}", path.display()), "localhost".to_string())
            }
        };
        Ok(Self {
            endpoint,
            label,
            host_header,
            secret: controller.secret.trim().to_owned(),
        })
    }

    /// 读取内核版本，仅用于校验控制面可用（自动检测）。
    pub async fn version(&self) -> Result<String, String> {
        #[derive(Deserialize)]
        struct VersionInfo {
            #[serde(default)]
            version: String,
        }
        let info: VersionInfo = self.get_json("version").await?;
        if info.version.is_empty() {
            Ok("unknown".to_string())
        } else {
            Ok(info.version)
        }
    }

    pub async fn snapshot(&self) -> Result<ClashState, String> {
        // 逐个请求：本机两次往返可忽略，换来确定的失败点与可测性
        // （并发只省不到 1ms，却让错误来源依赖调度顺序）
        let config: ClashConfig = self.get_json("configs").await?;
        let proxies: ProxiesResponse = self.get_json("proxies").await?;
        let mode = config.mode.to_ascii_lowercase();
        let groups = proxies
            .proxies
            .into_iter()
            .filter(|(name, entry)| {
                entry.kind.eq_ignore_ascii_case("Selector")
                    && !entry.all.is_empty()
                    && if name == "GLOBAL" {
                        mode == "global"
                    } else {
                        !entry.hidden
                    }
            })
            .map(|(name, entry)| ProxyGroup {
                name,
                selected: entry.now,
                choices: entry.all,
            })
            .collect();
        Ok(ClashState { mode, groups })
    }

    pub async fn select(&self, group: &str, choice: &str) -> Result<(), String> {
        if group.is_empty() || matches!(group, "." | "..") || choice.is_empty() {
            return Err("请选择有效的 Clash 策略组和节点".to_string());
        }
        let path = format!("proxies/{}", encode_segment(group));
        let body = serde_json::json!({ "name": choice }).to_string();
        let (status, _) = self.request("PUT", &path, Some(&body)).await?;
        // mihomo 返回 204，个别实现返回 200
        self.ensure_status(status, &[200, 204])
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let (status, body) = self.request("GET", path, None).await?;
        self.ensure_status(status, &[200])?;
        serde_json::from_slice(&body).map_err(|_| "Clash 控制响应格式无效".to_string())
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<(u16, Vec<u8>), String> {
        match tokio::time::timeout(REQUEST_TIMEOUT, self.exchange(method, path, body)).await {
            Ok(result) => result,
            Err(_) => Err(format!("Clash 控制请求超时（{}）", self.label)),
        }
    }

    async fn exchange(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<(u16, Vec<u8>), String> {
        match &self.endpoint {
            ClashEndpoint::Tcp(addr) => {
                let stream = tokio::net::TcpStream::connect(addr)
                    .await
                    .map_err(|error| self.connect_error(&addr.to_string(), error))?;
                http_exchange(stream, &self.host_header, method, path, &self.secret, body)
                    .await
                    .map_err(|error| format!("{error}（{}）", self.label))
            }
            #[cfg(unix)]
            ClashEndpoint::Unix(socket) => {
                let stream = tokio::net::UnixStream::connect(socket).await.map_err(|error| {
                    format!(
                        "无法连接 Clash 控制 socket {}：请确认 Clash 正在运行，或点「自动检测」：{error}",
                        socket.display()
                    )
                })?;
                http_exchange(stream, &self.host_header, method, path, &self.secret, body)
                    .await
                    .map_err(|error| format!("{error}（{}）", self.label))
            }
            #[cfg(not(unix))]
            ClashEndpoint::Unix(socket) => Err(format!(
                "当前平台不支持 Unix socket 形式的 Clash 控制地址（{}）",
                socket.display()
            )),
        }
    }

    fn connect_error(&self, endpoint: &str, error: std::io::Error) -> String {
        format!(
            "无法连接 Clash 控制端口 {endpoint}：请确认 Clash 已开启 external-controller\
             （Clash Verge Rev 默认使用 Unix socket，可点「自动检测」）：{error}"
        )
    }

    fn ensure_status(&self, status: u16, expected: &[u16]) -> Result<(), String> {
        if expected.contains(&status) {
            return Ok(());
        }
        match status {
            401 | 403 => Err(format!(
                "Clash 控制鉴权失败：Secret 不正确（{}）。可在 Clash 设置中查看，或用「自动检测」",
                self.label
            )),
            404 => Err(format!(
                "Clash 控制面不支持该 API（HTTP 404，{}），请确认地址指向 Clash/mihomo 控制面",
                self.label
            )),
            other => Err(format!(
                "Clash 控制请求被拒绝（HTTP {other}，{}），请刷新策略组后重试",
                self.label
            )),
        }
    }
}

/// 发出一次 HTTP/1.1 请求并读取完整响应。
async fn http_exchange<S>(
    mut stream: S,
    host: &str,
    method: &str,
    path: &str,
    secret: &str,
    body: Option<&str>,
) -> Result<(u16, Vec<u8>), String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut head = format!(
        "{method} /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nAccept: application/json\r\n"
    );
    if !secret.is_empty() {
        head.push_str(&format!("Authorization: Bearer {secret}\r\n"));
    }
    if let Some(body) = body {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    head.push_str("\r\n");

    stream
        .write_all(head.as_bytes())
        .await
        .map_err(|error| format!("写入 Clash 控制请求失败: {error}"))?;
    if let Some(body) = body {
        stream
            .write_all(body.as_bytes())
            .await
            .map_err(|error| format!("写入 Clash 控制请求失败: {error}"))?;
    }
    stream
        .flush()
        .await
        .map_err(|error| format!("写入 Clash 控制请求失败: {error}"))?;

    read_response(&mut stream).await
}

async fn read_response<S>(stream: &mut S) -> Result<(u16, Vec<u8>), String>
where
    S: AsyncRead + Unpin,
{
    let mut buffer = Vec::with_capacity(1024);
    let header_end = loop {
        if let Some(position) = find(&buffer, b"\r\n\r\n") {
            break position;
        }
        if buffer.len() > MAX_HEADER_BYTES {
            return Err("Clash 控制响应头过大".to_string());
        }
        read_more(stream, &mut buffer).await?;
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut rest: Vec<u8> = buffer[header_end + 4..].to_vec();

    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| "无法解析 Clash 控制响应状态".to_string())?;

    let chunked = head.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("transfer-encoding:") && lower.contains("chunked")
    });
    let content_length = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.eq_ignore_ascii_case("content-length") {
            return None;
        }
        value.trim().parse::<usize>().ok()
    });

    let body = if matches!(status, 204 | 304) {
        // 规范上无 body；不读可以避免服务端 keep-alive 时挂到超时
        Vec::new()
    } else if chunked {
        read_chunked(stream, &mut rest).await?
    } else if let Some(length) = content_length {
        if length > MAX_RESPONSE_BYTES {
            return Err("Clash 控制响应超过大小限制".to_string());
        }
        while rest.len() < length {
            read_more(stream, &mut rest).await?;
        }
        rest.truncate(length);
        rest
    } else {
        read_to_end(stream, &mut rest).await?;
        rest
    };
    Ok((status, body))
}

async fn read_chunked<S>(stream: &mut S, buffer: &mut Vec<u8>) -> Result<Vec<u8>, String>
where
    S: AsyncRead + Unpin,
{
    let mut body = Vec::new();
    loop {
        let line = read_line(stream, buffer).await?;
        let size_text = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| "Clash 控制响应分块长度无效".to_string())?;
        if size == 0 {
            break;
        }
        if body.len() + size > MAX_RESPONSE_BYTES {
            return Err("Clash 控制响应超过大小限制".to_string());
        }
        while buffer.len() < size + 2 {
            read_more(stream, buffer).await?;
        }
        body.extend_from_slice(&buffer[..size]);
        buffer.drain(..size + 2);
    }
    Ok(body)
}

async fn read_line<S>(stream: &mut S, buffer: &mut Vec<u8>) -> Result<String, String>
where
    S: AsyncRead + Unpin,
{
    loop {
        if let Some(position) = find(buffer, b"\r\n") {
            let line = String::from_utf8_lossy(&buffer[..position]).to_string();
            buffer.drain(..position + 2);
            return Ok(line);
        }
        if buffer.len() > MAX_HEADER_BYTES {
            return Err("Clash 控制响应行过长".to_string());
        }
        read_more(stream, buffer).await?;
    }
}

async fn read_more<S>(stream: &mut S, buffer: &mut Vec<u8>) -> Result<(), String>
where
    S: AsyncRead + Unpin,
{
    let mut chunk = [0u8; 4096];
    let read = stream
        .read(&mut chunk)
        .await
        .map_err(|error| format!("读取 Clash 控制响应失败: {error}"))?;
    if read == 0 {
        return Err("Clash 控制连接提前关闭".to_string());
    }
    buffer.extend_from_slice(&chunk[..read]);
    if buffer.len() > MAX_RESPONSE_BYTES {
        return Err("Clash 控制响应超过大小限制".to_string());
    }
    Ok(())
}

async fn read_to_end<S>(stream: &mut S, buffer: &mut Vec<u8>) -> Result<(), String>
where
    S: AsyncRead + Unpin,
{
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| format!("读取 Clash 控制响应失败: {error}"))?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_RESPONSE_BYTES {
            return Err("Clash 控制响应超过大小限制".to_string());
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// HTTP 路径段编码（保留 RFC 3986 unreserved，其余转 `%XX`）。
fn encode_segment(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::read_request;
    use tokio::io::BufReader;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn selector_reads_current_choice_and_switches_with_bearer_auth() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let controller = ClashController {
            address: listener.local_addr().unwrap().to_string(),
            secret: "CaseSensitive".to_string(),
        };
        let server = tokio::spawn(async move {
            let mut selected = "初始节点";
            for _ in 0..5 {
                let (socket, _) = listener.accept().await.unwrap();
                let mut stream = BufReader::new(socket);
                let request = read_request(&mut stream).await;
                assert_eq!(request.authorization(), "Bearer CaseSensitive");
                let body = request.body;
                let reply = match request.line.as_str() {
                    "GET /configs HTTP/1.1" => {
                        let body = r#"{"mode":"Rule"}"#;
                        format!(
                            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                            body.len()
                        )
                    }
                    "GET /proxies HTTP/1.1" => {
                        let body = serde_json::json!({"proxies": {
                            "策略/组": {"type": "Selector", "now": selected, "all": ["初始节点", "新节点"]},
                            "Auto": {"type": "URLTest", "all": ["初始节点"]},
                            "GLOBAL": {"type": "Selector", "all": ["策略/组"]}
                        }})
                        .to_string();
                        format!(
                            "HTTP/1.1 200 OK\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n{:X}\r\n{body}\r\n0\r\n\r\n",
                            body.len()
                        )
                    }
                    "PUT /proxies/%E7%AD%96%E7%95%A5%2F%E7%BB%84 HTTP/1.1" => {
                        assert_eq!(
                            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                            serde_json::json!({"name": "新节点"})
                        );
                        selected = "新节点";
                        "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_string()
                    }
                    other => panic!("意外请求: {other}"),
                };
                stream.get_mut().write_all(reply.as_bytes()).await.unwrap();
            }
        });
        let client = ClashClient::new(&controller).unwrap();
        let state = client.snapshot().await.unwrap();
        assert_eq!(state.mode, "rule");
        assert_eq!(state.groups.len(), 1);
        assert_eq!(state.groups[0].selected, "初始节点");
        client.select("策略/组", "新节点").await.unwrap();
        assert_eq!(
            client.snapshot().await.unwrap().groups[0].selected,
            "新节点"
        );
        server.await.unwrap();
    }

    #[test]
    fn encodes_path_segments_like_url_builders() {
        assert_eq!(encode_segment("GLOBAL"), "GLOBAL");
        assert_eq!(encode_segment("策略/组"), "%E7%AD%96%E7%95%A5%2F%E7%BB%84");
        assert_eq!(encode_segment("a b"), "a%20b");
    }

    #[tokio::test]
    async fn reports_unauthorized_secret() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let controller = ClashController {
            address: listener.local_addr().unwrap().to_string(),
            secret: "wrong".to_string(),
        };
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(socket);
            read_request(&mut stream).await;
            let reply =
                "HTTP/1.1 401 Unauthorized\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
            stream.get_mut().write_all(reply.as_bytes()).await.unwrap();
        });
        let client = ClashClient::new(&controller).unwrap();
        let error = client.snapshot().await.unwrap_err();
        // 服务端只处理一个连接：串行请求保证首个（configs）就拿到 401
        server.await.unwrap();
        assert!(error.contains("Secret"), "应提示 Secret 问题: {error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_socket_control_plane_is_supported() {
        // 临时目录路径可能超过 SUN_LEN（macOS 104 字节），测试用短路径
        let token = localproxy_core::config::generate_token();
        let socket_path =
            std::path::PathBuf::from("/tmp").join(format!("lp-clash-{}.sock", &token[..8]));
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(socket);
            let request = read_request(&mut stream).await;
            assert_eq!(request.line, "GET /version HTTP/1.1");
            assert_eq!(request.authorization(), "Bearer unix-secret");
            let body = r#"{"meta":true,"version":"v1.19.33"}"#;
            let reply = format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            stream.get_mut().write_all(reply.as_bytes()).await.unwrap();
        });

        let controller = ClashController {
            address: format!("unix:{}", socket_path.display()),
            secret: "unix-secret".to_string(),
        };
        let client = ClashClient::new(&controller).unwrap();
        assert_eq!(client.version().await.unwrap(), "v1.19.33");
        server.await.unwrap();
        std::fs::remove_file(&socket_path).ok();
    }
}
