//! 自动发现本机 Clash / mihomo 控制面。
//!
//! 痛点：控制地址与实际客户端不一致时，用户只能看到一条失败提示。实际部署里
//! 有三种常见形态：
//! - mihomo / ClashX：TCP 回环端口（9090 等），`external-controller` 写在配置里；
//! - Clash Verge Rev：默认 **Unix domain socket**（`verge-mihomo -ext-ctl-unix
//!   /var/folders/.../verge-mihomo.sock`），配置里的 9097 只是模板值；
//! - 未设 Secret 的本地内核：直接探测常见端口即可。
//!
//! 发现顺序：配置文件里的 TCP 控制面 → Unix socket（进程参数 + 临时目录）
//! → 常见端口空 Secret 探测。全程只连本机 socket / 回环端口，不经过系统代理，
//! 也不要求用户开启 Clash 的系统代理或 TUN。

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use localproxy_core::config::ClashController;
use serde::Serialize;
use tokio::time::timeout;

use crate::clash::ClashClient;

/// 单个控制面校验的超时（连接被拒绝时立即返回，超时只兜底丢包场景）。
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);
/// 配置文件读取上限，避免误读大文件。
const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;
/// 无配置文件命中时探测的常见控制端口。
const COMMON_PORTS: &[u16] = &[9090, 9097, 9091, 6170];

/// 自动检测结果。
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredClash {
    /// `127.0.0.1:9097` 或 `unix:/path/to.sock`
    pub address: String,
    pub secret: String,
    /// 来源说明（配置文件、socket 路径或端口探测）
    pub source: String,
    pub version: String,
}

/// 自动发现控制面。
pub async fn discover() -> Result<DiscoveredClash, String> {
    discover_in(
        &candidate_configs(),
        &unix_socket_candidates(),
        COMMON_PORTS,
    )
    .await
}

/// 可注入候选来源的发现实现（便于测试）。
async fn discover_in(
    configs: &[PathBuf],
    sockets: &[PathBuf],
    ports: &[u16],
) -> Result<DiscoveredClash, String> {
    let mut failures: Vec<String> = Vec::new();
    let mut secrets: Vec<String> = Vec::new();

    // 1) 配置文件：先收集 Secret 池，再尝试其中声明的 TCP 控制面
    for path in configs {
        let Some(text) = read_config(path) else {
            continue;
        };
        let Some(parsed) = parse_config(&text) else {
            continue;
        };
        if !parsed.secret.is_empty() && !secrets.contains(&parsed.secret) {
            secrets.push(parsed.secret.clone());
        }
        let Some(address) = parsed.controller else {
            continue;
        };
        let controller = ClashController {
            address: address.clone(),
            secret: parsed.secret.clone(),
        };
        match verify(&controller).await {
            Ok(version) => {
                return Ok(DiscoveredClash {
                    address,
                    secret: parsed.secret,
                    source: path.display().to_string(),
                    version,
                })
            }
            Err(error) => failures.push(format!("{} → {error}", path.display())),
        }
    }

    // 2) Unix socket：Clash Verge Rev 的默认形态，Secret 取自上面的配置文件
    if !sockets.is_empty() {
        let candidates: Vec<String> = if secrets.is_empty() {
            vec![String::new()]
        } else {
            secrets.clone()
        };
        for socket in sockets {
            for secret in &candidates {
                let address = format!("unix:{}", socket.display());
                let controller = ClashController {
                    address: address.clone(),
                    secret: secret.clone(),
                };
                match verify(&controller).await {
                    Ok(version) => {
                        return Ok(DiscoveredClash {
                            address,
                            secret: secret.clone(),
                            source: format!("Unix socket {}", socket.display()),
                            version,
                        })
                    }
                    Err(error) => failures.push(format!("unix:{} → {error}", socket.display())),
                }
            }
        }
    }

    // 3) 常见端口 + 空 Secret 探测
    for port in ports {
        let address = format!("127.0.0.1:{port}");
        let controller = ClashController {
            address: address.clone(),
            secret: String::new(),
        };
        if let Ok(version) = verify(&controller).await {
            return Ok(DiscoveredClash {
                address,
                secret: String::new(),
                source: format!("端口探测 {port}"),
                version,
            });
        }
    }

    let mut message = String::from(
        "未找到可用的 Clash 控制面。请确认 Clash 正在运行并开启了 external-controller\
         （Clash Verge Rev 使用 Unix socket 时会自动识别），或手动填写控制地址与 Secret",
    );
    if !failures.is_empty() {
        let shown = failures
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("；");
        message.push_str(&format!("。已尝试：{shown}"));
    }
    Err(message)
}

/// 校验控制面可用，返回内核版本。
async fn verify(controller: &ClashController) -> Result<String, String> {
    let client = ClashClient::new(controller)?;
    timeout(PROBE_TIMEOUT, client.version())
        .await
        .map_err(|_| "控制面响应超时".to_string())?
}

fn read_config(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// 从配置文件里提取的候选。
#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedConfig {
    /// `external-controller`（可能为空，或被 Unix socket 覆盖而失效）
    controller: Option<String>,
    secret: String,
}

/// 从一个 YAML 文本里取出控制地址与 Secret。
///
/// 只做逐行键值提取（不引入 YAML 依赖）。注意 `verge_port` / `port` 是代理端口，
/// 不是控制端口，不能作为候选。
fn parse_config(text: &str) -> Option<ParsedConfig> {
    let mut controller: Option<String> = None;
    let mut secret = String::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = unquote(value);
        match key.trim() {
            "external-controller" | "external_controller" if !value.is_empty() => {
                controller = Some(value)
            }
            "secret" if !value.is_empty() => secret = value,
            _ => {}
        }
    }

    if controller.is_none() && secret.is_empty() {
        return None;
    }
    let controller =
        controller.and_then(|raw| normalize_address(&raw).map(|addr| addr.to_string()));
    Some(ParsedConfig { controller, secret })
}

/// 去掉行内注释与首尾引号。
fn unquote(raw: &str) -> String {
    let raw = raw.split(" #").next().unwrap_or("").trim();
    raw.trim_matches(|c| c == '\'' || c == '"')
        .trim()
        .to_string()
}

/// 归一化 TCP 控制地址：`0.0.0.0` / `[::]` 转回环；非回环地址不接受。
fn normalize_address(raw: &str) -> Option<SocketAddr> {
    let raw = raw.trim();
    // unix socket 形式（`/tmp/clash.sock`）不是 TCP 地址
    if raw.is_empty() || raw.starts_with('/') {
        return None;
    }
    let addr: SocketAddr = raw.parse().ok()?;
    if addr.port() == 0 {
        return None;
    }
    let ip = match addr.ip() {
        IpAddr::V4(v4) if v4.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v6) if v6.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        other => other,
    };
    if !ip.is_loopback() {
        return None;
    }
    Some(SocketAddr::new(ip, addr.port()))
}

/// 收集 Unix socket 候选：进程参数优先，其次临时目录下的 mihomo/clash socket。
#[cfg(unix)]
fn unix_socket_candidates() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = socket_from_process_arguments() {
        candidates.push(path);
    }
    if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if !name.ends_with(".sock") {
                continue;
            }
            if !name.contains("mihomo") && !name.contains("clash") {
                continue;
            }
            let path = entry.path();
            if is_socket(&path) {
                candidates.push(path);
            }
        }
    }
    dedup(candidates)
}

#[cfg(not(unix))]
fn unix_socket_candidates() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(unix)]
fn is_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.file_type().is_socket())
        .unwrap_or(false)
}

/// 从 `verge-mihomo -ext-ctl-unix <path>` 的进程参数里取控制 socket。
#[cfg(unix)]
fn socket_from_process_arguments() -> Option<PathBuf> {
    let output = std::process::Command::new("ps")
        .args(["-eo", "args="])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if !line.contains("-ext-ctl-unix") {
            continue;
        }
        let mut tokens = line.split_whitespace();
        while let Some(token) = tokens.next() {
            // 兼容 `-ext-ctl-unix /path` 与 `-ext-ctl-unix=/path` 两种写法
            let value = if token == "-ext-ctl-unix" {
                tokens.next()
            } else {
                token.strip_prefix("-ext-ctl-unix=")
            };
            if let Some(path) = value {
                let path = PathBuf::from(path);
                if path.is_absolute() && is_socket(&path) {
                    return Some(path);
                }
            }
        }
    }
    None
}

#[cfg(unix)]
fn dedup(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut unique: Vec<PathBuf> = Vec::with_capacity(paths.len());
    for path in paths {
        if !unique.contains(&path) {
            unique.push(path);
        }
    }
    unique
}

/// 已知客户端的运行配置路径（按优先级）。
fn candidate_configs() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut push = |path: PathBuf| {
        if path.is_file() {
            paths.push(path);
        }
    };

    if cfg!(target_os = "macos") {
        if let Some(home) = std::env::var_os("HOME") {
            let support = PathBuf::from(&home).join("Library/Application Support");
            for dir in [
                "io.github.clash-verge-rev.clash-verge-rev",
                "clash-verge",
                "io.github.clash-verge-rev.clash-verge",
            ] {
                for name in ["config.yaml", "clash-verge.yaml"] {
                    push(support.join(dir).join(name));
                }
            }
            for dir in [".config/clash", ".config/mihomo", ".config/clash-verge"] {
                push(PathBuf::from(&home).join(dir).join("config.yaml"));
            }
        }
    } else if cfg!(target_os = "windows") {
        if let Some(base) = std::env::var_os("APPDATA").map(PathBuf::from) {
            for dir in ["io.github.clash-verge-rev.clash-verge-rev", "clash-verge"] {
                for name in ["config.yaml", "clash-verge.yaml"] {
                    push(base.join(dir).join(name));
                }
            }
        }
        if let Some(home) = std::env::var_os("USERPROFILE") {
            for dir in [".config\\clash", ".config\\mihomo"] {
                push(PathBuf::from(&home).join(dir).join("config.yaml"));
            }
        }
    } else {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
        if let Some(base) = base {
            for dir in [
                "io.github.clash-verge-rev.clash-verge-rev",
                "clash-verge",
                "clash",
                "mihomo",
            ] {
                for name in ["config.yaml", "clash-verge.yaml"] {
                    push(base.join(dir).join(name));
                }
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::read_request;
    use tokio::io::{AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    #[test]
    fn parses_controller_and_secret_with_quotes_and_comments() {
        let text = r#"
# 注释行
mixed-port: 7897
external-controller: '127.0.0.1:9097'   # 行内注释
secret: "s3cr3t"
proxies:
  - name: node
"#;
        let parsed = parse_config(text).expect("应能解析");
        assert_eq!(parsed.controller.as_deref(), Some("127.0.0.1:9097"));
        assert_eq!(parsed.secret, "s3cr3t");
    }

    #[test]
    fn proxy_port_is_not_a_control_port() {
        // `verge_port` / `port` 是代理端口，不能当成控制端口
        let parsed = parse_config("verge_port: 7899\nport: 7899\nsecret: abc\n").unwrap();
        assert!(parsed.controller.is_none());
        assert_eq!(parsed.secret, "abc");
    }

    #[test]
    fn secret_only_config_still_contributes_secret_pool() {
        let parsed = parse_config("external-controller: ''\nsecret: 'only'\n").unwrap();
        assert!(parsed.controller.is_none());
        assert_eq!(parsed.secret, "only");
        assert!(parse_config("mixed-port: 7897\n").is_none());
    }

    #[test]
    fn normalizes_unspecified_and_rejects_remote_addresses() {
        assert_eq!(
            normalize_address("0.0.0.0:9090").unwrap().to_string(),
            "127.0.0.1:9090"
        );
        assert_eq!(
            normalize_address("[::]:9090").unwrap().to_string(),
            "127.0.0.1:9090"
        );
        assert_eq!(
            normalize_address("127.0.0.1:9097").unwrap().to_string(),
            "127.0.0.1:9097"
        );
        assert!(normalize_address("192.168.1.9:9090").is_none());
        assert!(normalize_address("127.0.0.1:0").is_none());
        assert!(normalize_address("/tmp/clash.sock").is_none());
    }

    #[tokio::test]
    async fn selects_controller_from_running_config() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(socket);
            assert_eq!(
                read_request(&mut stream).await.authorization(),
                "Bearer discovered"
            );
            let body = r#"{"version":"v1.18.0"}"#;
            let reply = format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            stream.get_mut().write_all(reply.as_bytes()).await.unwrap();
        });

        let directory = std::env::temp_dir().join(format!(
            "localproxy-clash-{}",
            localproxy_core::config::generate_token()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config = directory.join("config.yaml");
        std::fs::write(
            &config,
            format!("external-controller: 127.0.0.1:{port}\nsecret: discovered\n"),
        )
        .unwrap();

        let found = discover_in(std::slice::from_ref(&config), &[], &[])
            .await
            .expect("应发现控制面");
        assert_eq!(found.address, format!("127.0.0.1:{port}"));
        assert_eq!(found.secret, "discovered");
        assert_eq!(found.version, "v1.18.0");
        assert_eq!(found.source, config.display().to_string());
        server.await.unwrap();
        std::fs::remove_dir_all(&directory).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn selects_unix_socket_with_secret_from_config() {
        let token = localproxy_core::config::generate_token();
        let socket_path =
            std::path::PathBuf::from("/tmp").join(format!("lp-discover-{}.sock", &token[..8]));
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(socket);
            assert_eq!(
                read_request(&mut stream).await.authorization(),
                "Bearer from-config"
            );
            let body = r#"{"version":"v1.19.33"}"#;
            let reply = format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            stream.get_mut().write_all(reply.as_bytes()).await.unwrap();
        });

        // 配置文件只有 secret（external-controller 是失效模板），socket 从参数/扫描传入
        let directory = std::env::temp_dir().join(format!("localproxy-clash-{}", &token[..8]));
        std::fs::create_dir_all(&directory).unwrap();
        let config = directory.join("config.yaml");
        std::fs::write(
            &config,
            "external-controller: 127.0.0.1:9097\nsecret: from-config\n",
        )
        .unwrap();

        let found = discover_in(
            std::slice::from_ref(&config),
            std::slice::from_ref(&socket_path),
            &[],
        )
        .await
        .expect("应通过 Unix socket 发现控制面");
        assert_eq!(found.address, format!("unix:{}", socket_path.display()));
        assert_eq!(found.secret, "from-config");
        assert_eq!(found.version, "v1.19.33");
        assert!(found.source.contains("Unix socket"), "{}", found.source);
        server.await.unwrap();
        std::fs::remove_file(&socket_path).ok();
        std::fs::remove_dir_all(&directory).ok();
    }
}
