//! LocalProxy 本地代理核心。
//!
//! 提供：
//! - SOCKS5 / HTTP CONNECT 代理服务
//! - 规则路由（CIDR / 域名 / 端口 / 协议）
//! - 上游级联（SOCKS5 / HTTP + 认证，多上游热切换）
//! - DNS 决策与 UDP 转发
//! - 本地控制面（健康检查 / 统计 / 日志 / 配置重载）
//!
//! 可作为库被 Tauri App 使用，也可通过 `localproxy-core` CLI 独立运行。

pub mod config;
pub mod control;
pub mod dns;
pub mod rules;
pub mod server;
pub mod stats;
pub mod upstream;
