//! `localproxy-core` CLI 入口：独立运行代理核心（无 UI 场景）。
//!
//! 典型用法：
//! ```text
//! localproxy-core                          # 默认配置路径
//! localproxy-core --config ./config.json   # 指定配置
//! ```

use std::{path::PathBuf, sync::Arc};

use clap::Parser;
use localproxy_core::config::{AppConfig, DEFAULT_LISTEN_HOST};
use localproxy_core::control;
use localproxy_core::server::{run_http, run_socks5, ServerContext};
use tokio::net::TcpListener;

/// LocalProxy 本地代理核心
#[derive(Parser, Debug)]
#[command(name = "localproxy-core", version, about)]
struct Cli {
    /// 配置文件路径（默认平台配置目录下的 config.json）
    #[arg(long)]
    config: Option<PathBuf>,

    /// 桌面子进程协议：端口避让、输出实际监听信息、父进程关闭 stdin 后退出。
    #[arg(long, hide = true)]
    desktop: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let desktop = cli.desktop;
    let result = serve(cli).await;
    if desktop {
        if let Err(error) = &result {
            println!("{}", serde_json::json!({"error": error.to_string()}));
        }
    }
    result
}

async fn serve(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // 加载配置
    let config = match &cli.config {
        Some(path) => AppConfig::load(path)?,
        None => AppConfig::default(),
    };

    if cli.desktop {
        config.validate_desktop()?;
    } else {
        config.validate()?;
    }

    // 控制面 token：为空则生成并落盘（避免控制面无鉴权）
    let mut config = config;
    let token_changed = config.control_token.trim().is_empty();
    config.ensure_control_token();
    if token_changed {
        if let Some(path) = &cli.config {
            if let Err(e) = config.save(path) {
                tracing::warn!("保存生成的 control_token 失败: {e}");
            }
        }
    }
    if config.listen_host.eq_ignore_ascii_case("localhost") {
        config.listen_host = DEFAULT_LISTEN_HOST.to_string();
    }
    let reserved: Vec<_> = config
        .upstream
        .servers
        .iter()
        .flat_map(|server| {
            std::iter::once(server.port).chain(
                server
                    .clash
                    .iter()
                    .filter_map(|controller| controller.socket_addr().ok().map(|addr| addr.port())),
            )
        })
        .collect();
    let socks5_listener = bind_listener(
        &config.listen_host,
        config.socks5_port,
        &reserved,
        cli.desktop,
    )
    .await?;
    let http_listener = bind_listener(
        &config.listen_host,
        config.http_port,
        &reserved,
        cli.desktop,
    )
    .await?;
    let control_listener = bind_listener(
        DEFAULT_LISTEN_HOST,
        config.control_port,
        &reserved,
        cli.desktop,
    )
    .await?;
    config.listen_host = socks5_listener.local_addr()?.ip().to_string();
    config.socks5_port = socks5_listener.local_addr()?.port();
    config.http_port = http_listener.local_addr()?.port();
    config.control_port = control_listener.local_addr()?.port();
    config.validate()?;
    let listeners = config.listener_info();
    let ctx = Arc::new(ServerContext::start(config).await?);

    if cli.desktop {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{}", serde_json::to_string(&listeners)?)?;
        stdout.flush()?;
    }
    tracing::info!(
        "SOCKS5 监听 {}:{}，HTTP CONNECT 监听 {}:{}，控制面监听 {}:{}",
        listeners.listen_host,
        listeners.socks5_port,
        listeners.listen_host,
        listeners.http_port,
        DEFAULT_LISTEN_HOST,
        listeners.control_port
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
        tokio::spawn(async move { control::run(control_listener, ctx, cli.config).await })
    };

    // 等待退出信号或服务异常
    tokio::select! {
        _ = termination_signal() => {
            tracing::info!("收到终止信号，localproxy-core 停止");
        }
        _ = desktop_closed(cli.desktop) => {
            tracing::info!("桌面连接已关闭，localproxy-core 停止");
        }
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

async fn bind_listener(
    host: &str,
    port: u16,
    reserved: &[u16],
    auto: bool,
) -> std::io::Result<TcpListener> {
    if !auto || !reserved.contains(&port) {
        match TcpListener::bind((host, port)).await {
            Ok(listener) => return Ok(listener),
            Err(error) if auto && error.kind() == std::io::ErrorKind::AddrInUse => {}
            Err(error) => return Err(error),
        }
    }
    loop {
        let listener = TcpListener::bind((host, 0)).await?;
        if !reserved.contains(&listener.local_addr()?.port()) {
            return Ok(listener);
        }
    }
}

async fn desktop_closed(desktop: bool) {
    if !desktop {
        return std::future::pending().await;
    }
    let (send, receive) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let _ = std::io::stdin().read(&mut [0; 1]);
        let _ = send.send(());
    });
    let _ = receive.await;
}

#[cfg(unix)]
async fn termination_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let Ok(mut signal) = signal(SignalKind::terminate()) else {
        return std::future::pending().await;
    };
    signal.recv().await;
}

#[cfg(not(unix))]
async fn termination_signal() {
    std::future::pending().await
}
