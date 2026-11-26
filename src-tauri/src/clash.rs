//! 通过本机 Clash/mihomo API 查询策略组与切换节点。

use std::{collections::BTreeMap, time::Duration};

use localproxy_core::config::ClashController;
use reqwest::{Client, RequestBuilder, Response, StatusCode, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

pub struct ClashClient {
    client: Client,
    base_url: Url,
    secret: String,
}

#[derive(Serialize)]
pub struct ClashState {
    mode: String,
    groups: Vec<ProxyGroup>,
}

#[derive(Serialize)]
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
    proxies: BTreeMap<String, ProxyEntry>,
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
        let socket = controller.socket_addr()?;
        let base_url = Url::parse(&format!("http://{socket}/"))
            .map_err(|_| "Clash 控制地址无效".to_string())?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| "无法初始化 Clash 控制客户端".to_string())?;
        Ok(Self {
            client,
            base_url,
            secret: controller.secret.trim().to_owned(),
        })
    }

    pub async fn snapshot(&self) -> Result<ClashState, String> {
        let configs = self.base_url.join("configs").expect("固定 API 路径");
        let proxies = self.base_url.join("proxies").expect("固定 API 路径");
        let (config, proxies) = tokio::try_join!(
            self.read_json::<ClashConfig>(self.client.get(configs)),
            self.read_json::<ProxiesResponse>(self.client.get(proxies)),
        )?;
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
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .expect("HTTP URL 支持路径段")
            .extend(["proxies", group]);
        let request = self
            .client
            .put(url)
            .json(&serde_json::json!({"name": choice}));
        self.send(request, StatusCode::NO_CONTENT).await?;
        Ok(())
    }

    async fn read_json<T: DeserializeOwned>(&self, request: RequestBuilder) -> Result<T, String> {
        let mut response = self.send(request, StatusCode::OK).await?;
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| "读取 Clash 控制响应失败")?
        {
            if chunk.len() > MAX_RESPONSE_BYTES - body.len() {
                return Err("Clash 控制响应超过大小限制".to_string());
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| "Clash 控制响应格式无效".to_string())
    }

    async fn send(
        &self,
        mut request: RequestBuilder,
        expected: StatusCode,
    ) -> Result<Response, String> {
        if !self.secret.is_empty() {
            request = request.bearer_auth(&self.secret);
        }
        let response = request.send().await.map_err(|e| {
            if e.is_timeout() {
                "Clash 控制请求超时".to_string()
            } else {
                "Clash 控制请求失败，请检查控制地址和 Secret".to_string()
            }
        })?;
        match response.status() {
            status if status == expected => Ok(response),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                Err("Clash 控制鉴权失败，请检查 Secret".to_string())
            }
            status => Err(format!(
                "Clash 控制请求被拒绝（HTTP {status}），请刷新策略组后重试"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
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
                let mut request = String::new();
                stream.read_line(&mut request).await.unwrap();
                let mut authorized = false;
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    stream.read_line(&mut line).await.unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.trim().split_once(':') {
                        if name.eq_ignore_ascii_case("authorization") {
                            authorized = value.trim() == "Bearer CaseSensitive";
                        }
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap();
                        }
                    }
                }
                assert!(authorized);
                let reply = match request
                    .split_whitespace()
                    .take(2)
                    .collect::<Vec<_>>()
                    .as_slice()
                {
                    ["GET", "/configs"] => {
                        let body = r#"{"mode":"Rule"}"#;
                        format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len())
                    }
                    ["GET", "/proxies"] => {
                        let body = serde_json::json!({"proxies": {
                            "策略/组": {"type": "Selector", "now": selected, "all": ["初始节点", "新节点"]},
                            "Auto": {"type": "URLTest", "all": ["初始节点"]},
                            "GLOBAL": {"type": "Selector", "all": ["策略/组"]}
                        }}).to_string();
                        format!("HTTP/1.1 200 OK\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n{:X}\r\n{body}\r\n0\r\n\r\n", body.len())
                    }
                    ["PUT", "/proxies/%E7%AD%96%E7%95%A5%2F%E7%BB%84"] => {
                        let mut body = vec![0; length];
                        stream.read_exact(&mut body).await.unwrap();
                        assert_eq!(
                            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                            serde_json::json!({"name": "新节点"})
                        );
                        selected = "新节点";
                        "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_string()
                    }
                    _ => panic!("意外请求: {request}"),
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
}
