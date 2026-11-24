//! `localproxy-core` CLI 入口：独立运行代理核心（无 UI 场景）。
//!
//! 典型用法：
//! ```text
//! localproxy-core                          # 默认配置路径
//! localproxy-core --config ./config.json   # 指定配置
//! ```

use std::sync::Arc;

use clap::Parser;
use localproxy_core::config::AppConfig;
use localproxy_core::control;
use localproxy_core::server::{run_http, run_socks5, ServerContext};
use tokio::net::TcpListener;

/// LocalProxy 本地代理核心
#[derive(Parser, Debug)]
#[command(name = "localproxy-core", version, about)]
struct Cli {
    /// 配置文件路径（默认平台配置目录下的 config.json）
    #[arg(long)]
    config: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // 加载配置
    let config = match &cli.config {
        Some(path) => AppConfig::load(std::path::Path::new(path))?,
        None => AppConfig::default(),
    };

    // 启动前校验：防自环（上游指向自身监听端口）
    config.validate()?;

    // 控制面 token：为空则生成并落盘（避免控制面无鉴权）
    let mut config = config;
    let token_changed = config.control_token.trim().is_empty();
    config.ensure_control_token();
    if token_changed {
        if let Some(path) = &cli.config {
            let path = std::path::Path::new(path);
            if let Err(e) = config.save(path) {
                tracing::warn!("保存生成的 control_token 失败: {e}");
            }
        }
    }
    // 端口预检：与 Clash/V2RayN 等代理软件共存时给出友好错误
    config.check_ports_available()?;

    let listen_host = config.listen_host.clone();
    let socks5_port = config.socks5_port;
    let http_port = config.http_port;

    let control_port = config.control_port;
    let ctx = Arc::new(ServerContext::new(config));
    tracing::info!("规则引擎: {}", ctx.rules.summary());

    // 监听
    let socks5_listener = TcpListener::bind((listen_host.as_str(), socks5_port)).await?;
    let http_listener = TcpListener::bind((listen_host.as_str(), http_port)).await?;
    let control_listener = TcpListener::bind((listen_host.as_str(), control_port)).await?;
    tracing::info!(
        "SOCKS5 监听 {}:{}，HTTP CONNECT 监听 {}:{}，控制面监听 {}:{}",
        listen_host,
        socks5_port,
        listen_host,
        http_port,
        listen_host,
        control_port
    );

    let socks5_task = {
        let ctx = ctx.clone();
        tokio::spawn(async move { run_socks5(socks5_listener, ctx).await })
    };
    let http_task = {
        let ctx = ctx.clone();
        tokio::spawn(async move { run_http(http_listener, ctx).await })
    };
    let control_task = {
        let ctx = ctx.clone();
        tokio::spawn(async move { control::run(control_listener, ctx).await })
    };

    // 等待退出信号或服务异常
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("收到退出信号，localproxy-core 停止");
        }
        result = socks5_task => {
            if let Ok(Err(e)) = result {
                tracing::error!("SOCKS5 服务异常退出: {e}");
            }
        }
        result = http_task => {
            if let Ok(Err(e)) = result {
                tracing::error!("HTTP 服务异常退出: {e}");
            }
        }
        result = control_task => {
            if let Ok(Err(e)) = result {
                tracing::error!("控制面服务异常退出: {e}");
            }
        }
    }
    Ok(())
}
