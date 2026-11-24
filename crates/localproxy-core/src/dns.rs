//! DNS 决策与解析。
//!
//! - `mode=proxy`：域名交上游解析（shim 已拦截真实查询）
//! - `mode=direct`：本地系统解析
//! - `mode=remote`（可选）：经指定 DNS-over-HTTPS/UDP 上游
//!
//! M1 将填充：direct 走 `std::net::ToSocketAddrs` / tokio 解析，
//! proxy 模式由上游 SOCKS5 协商完成，本模块只做缓存与策略。
