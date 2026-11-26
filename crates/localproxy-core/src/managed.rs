//! 独立节点由 LocalProxy 启动的 mihomo 实例承载，实例仅监听带认证的回环 SOCKS5。

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::{
    config::{UpstreamServer, DEFAULT_LISTEN_HOST},
    server::ServerContext,
    upstream::{Upstream, UpstreamError},
};

const START_TIMEOUT: Duration = Duration::from_secs(4);
const READINESS_INTERVAL: Duration = Duration::from_millis(35);
const OUTBOUND_NAME: &str = "localproxy-outbound";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedNode {
    pub protocol: String,
    #[serde(default)]
    pub options: Map<String, Value>,
}

impl ManagedNode {
    pub fn supports_udp(&self) -> bool {
        !matches!(self.protocol.as_str(), "http" | "ssh")
            && self.options.get("udp").and_then(Value::as_bool) != Some(false)
    }

    pub fn validate(&self) -> Result<(), String> {
        let required: &[&str] = match self.protocol.as_str() {
            "ss" => &["cipher", "password"],
            "ssr" => &["cipher", "password", "protocol", "obfs"],
            "vmess" | "vless" => &["uuid"],
            "trojan" | "anytls" | "hysteria2" => &["password"],
            "tuic" if self.options.contains_key("token") => &["token"],
            "tuic" => &["uuid", "password"],
            "wireguard" => &["private-key"],
            "snell" => &["psk"],
            "ssh" => &["username"],
            "http" | "socks5" => &[],
            _ => return Err("独立节点的协议不受支持".to_string()),
        };
        for field in required {
            if self
                .options
                .get(*field)
                .and_then(Value::as_str)
                .is_none_or(|value| value.trim().is_empty())
            {
                return Err(format!("独立节点缺少参数 {field}"));
            }
        }
        for field in [
            "name",
            "type",
            "server",
            "port",
            "dialer-proxy",
            "interface-name",
            "routing-mark",
            "private-key-path",
            "certificate",
            "certificate-private-key",
        ] {
            if self.options.contains_key(field) {
                return Err(format!("独立节点参数 {field} 由 LocalProxy 管理"));
            }
        }
        Ok(())
    }
}

pub struct ManagedRuntime {
    _directory: tempfile::TempDir,
    child: std::sync::Mutex<Child>,
    pub server: UpstreamServer,
}

impl Drop for ManagedRuntime {
    fn drop(&mut self) {
        // 只终止 LocalProxy 创建并持有的进程。
        if let Ok(child) = self.child.get_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl ManagedRuntime {
    pub fn is_running(&self) -> bool {
        self.child
            .lock()
            .is_ok_and(|mut child| matches!(child.try_wait(), Ok(None)))
    }

    pub async fn start(server: &UpstreamServer, path: &str) -> Result<Self, String> {
        let node = server.managed.as_ref().ok_or("未配置独立节点")?;
        node.validate()?;
        if server.host.trim().is_empty() || server.port == 0 {
            return Err("独立节点需要服务器地址和有效端口".to_string());
        }
        let executable = mihomo_path(path)?;
        let directory = tempfile::Builder::new()
            .prefix("localproxy-mihomo-")
            .tempdir()
            .map_err(|e| format!("创建内核工作目录失败: {e}"))?;
        let config_path = directory.path().join("config.json");
        // 代理端口由外部进程绑定，认证握手可以识别端口竞争后的错误进程。
        let port = std::net::TcpListener::bind((DEFAULT_LISTEN_HOST, 0))
            .and_then(|socket| socket.local_addr())
            .map_err(|e| format!("分配本机端口失败: {e}"))?
            .port();
        let mut key = [0; 24];
        getrandom::fill(&mut key).map_err(|_| "无法生成独立内核的认证密钥")?;
        let password: String = key.iter().map(|byte| format!("{byte:02x}")).collect();
        let username = "localproxy";
        let config = engine_config(server, node, port, username, &password);
        std::fs::write(
            &config_path,
            serde_json::to_vec(&config).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("写入独立内核配置失败: {e}"))?;

        let mut command = Command::new(executable);
        command
            .arg("-d")
            .arg(directory.path())
            .arg("-f")
            .arg(&config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        localproxy_launcher::proxy_env::remove_from(&mut command);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let child = command
            .spawn()
            .map_err(|e| format!("启动独立 mihomo 失败: {e}"))?;
        let runtime = Self {
            _directory: directory,
            child: std::sync::Mutex::new(child),
            server: UpstreamServer {
                host: DEFAULT_LISTEN_HOST.to_string(),
                port,
                username: username.to_string(),
                password,
                ..server.clone()
            },
        };
        let proxy = Upstream::new(runtime.server.clone(), ServerContext::timeouts());
        let mut invalid = runtime.server.clone();
        invalid.password = "invalid".to_string();
        let invalid = Upstream::new(invalid, ServerContext::timeouts());
        tokio::time::timeout(START_TIMEOUT, async {
            let mut interval = tokio::time::interval(READINESS_INTERVAL);
            loop {
                interval.tick().await;
                if !runtime.is_running() {
                    return Err("独立 mihomo 启动失败，请检查节点参数和内核版本".to_string());
                }
                let authenticated = tokio::time::timeout(Duration::from_millis(200), async {
                    let denied = matches!(invalid.open_socks5().await, Err(UpstreamError::Auth(_)));
                    denied && proxy.open_socks5().await.is_ok()
                })
                .await
                .unwrap_or(false);
                if authenticated {
                    return Ok(());
                }
            }
        })
        .await
        .map_err(|_| "独立 mihomo 未能在预期时间内启动")??;
        Ok(runtime)
    }
}

fn engine_config(
    server: &UpstreamServer,
    node: &ManagedNode,
    port: u16,
    username: &str,
    password: &str,
) -> Value {
    let mut proxy = node.options.clone();
    if node.supports_udp() {
        proxy.entry("udp").or_insert(json!(true));
    }
    if node.protocol == "vmess" {
        proxy.entry("cipher").or_insert(json!("auto"));
        proxy.entry("alterId").or_insert(json!(0));
    }
    proxy.insert("name".to_string(), json!(OUTBOUND_NAME));
    proxy.insert("type".to_string(), json!(node.protocol));
    proxy.insert(
        "server".to_string(),
        json!(server.host.trim().trim_matches(['[', ']'])),
    );
    proxy.insert("port".to_string(), json!(server.port));
    json!({
        "mode": "rule",
        "log-level": "silent",
        "socks-port": port,
        "bind-address": DEFAULT_LISTEN_HOST,
        "allow-lan": false,
        "authentication": [format!("{username}:{password}")],
        "skip-auth-prefixes": [],
        "tun": { "enable": false },
        "dns": { "enable": false },
        "profile": { "store-selected": false, "store-fake-ip": false },
        "geo-auto-update": false,
        "proxies": [proxy],
        "rules": [format!("MATCH,{OUTBOUND_NAME}")],
    })
}

/// 仅使用用户指定、同目录、PATH 或已知桌面安装位置的内核。
pub fn mihomo_path(configured: &str) -> Result<PathBuf, String> {
    let path = configured.trim();
    if !path.is_empty() {
        let path = PathBuf::from(path);
        if path.is_absolute() && path.is_file() {
            return Ok(path);
        }
        return Err("请选择有效的 mihomo 可执行文件绝对路径".to_string());
    }
    let canonical_name = if cfg!(windows) {
        "mihomo.exe"
    } else {
        "mihomo"
    };
    let verge_name = if cfg!(windows) {
        "verge-mihomo.exe"
    } else {
        "verge-mihomo"
    };
    let files = [canonical_name, verge_name];
    let locate = |folder: &Path| {
        files
            .iter()
            .map(|name| folder.join(name))
            .find(|file| file.is_file())
    };
    if let Ok(current) = std::env::current_exe() {
        if let Some(path) = current.parent().and_then(locate) {
            return Ok(path);
        }
    }
    if let Some(path) = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .filter(|dir| dir.is_absolute())
            .find_map(|dir| locate(&dir))
    }) {
        return Ok(path);
    }
    let install_folders: &[Option<PathBuf>] = &[
        #[cfg(target_os = "macos")]
        Some(PathBuf::from(
            "/Applications/Clash Verge.app/Contents/MacOS",
        )),
        #[cfg(target_os = "macos")]
        std::env::var_os("HOME")
            .map(|base| PathBuf::from(base).join("Applications/Clash Verge.app/Contents/MacOS")),
        #[cfg(windows)]
        std::env::var_os("ProgramFiles").map(|base| PathBuf::from(base).join("Clash Verge")),
        #[cfg(windows)]
        std::env::var_os("LOCALAPPDATA")
            .map(|base| PathBuf::from(base).join("Programs/Clash Verge")),
    ];
    install_folders
        .iter()
        .flatten()
        .find_map(|dir| locate(dir))
        .ok_or_else(|| "未找到 mihomo，请在设置中选择已安装的内核可执行文件".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_config_only_exposes_authenticated_socks_on_loopback() {
        let server = UpstreamServer {
            host: "node.example".to_string(),
            port: 443,
            ..UpstreamServer::default()
        };
        let node = ManagedNode {
            protocol: "ss".to_string(),
            options: Map::from_iter([
                ("cipher".to_string(), json!("aes-128-gcm")),
                ("password".to_string(), json!("demo")),
            ]),
        };
        assert!(node.validate().is_ok());
        let config = engine_config(&server, &node, 39119, "localproxy", "local-key");
        assert_eq!(config["bind-address"], "127.0.0.1");
        assert_eq!(config["authentication"], json!(["localproxy:local-key"]));
        assert_eq!(config["allow-lan"], false);
        assert_eq!(config["tun"]["enable"], false);
        assert_eq!(config["dns"]["enable"], false);
        assert!(config.get("external-controller").is_none());
        assert_eq!(config["rules"], json!(["MATCH,localproxy-outbound"]));
        assert_eq!(config["proxies"][0]["udp"], true);
        assert_eq!(config["proxies"][0]["server"], "node.example");
    }

    #[test]
    fn rejects_policy_overrides_and_incomplete_credentials() {
        let mut node = ManagedNode {
            protocol: "vless".to_string(),
            options: Map::new(),
        };
        assert!(node.validate().unwrap_err().contains("uuid"));
        node.options.insert(
            "uuid".to_string(),
            json!("3744d6fe-28a4-4faa-9fd4-2d7ac0422f42"),
        );
        assert!(node.validate().is_ok());
        node.options
            .insert("dialer-proxy".to_string(), json!("DIRECT"));
        assert!(node.validate().unwrap_err().contains("dialer-proxy"));
    }
}
