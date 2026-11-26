//! LocalProxy 共享网络协议库。
//!
//! 提供 shim 与 core 共用的纯协议逻辑：
//! - SOCKS5 客户端（CONNECT / UDP ASSOCIATE）
//! - HTTP CONNECT 客户端
//! - FakeIP 分配器与域名映射
//! - 本地/回环目标判定（局域网直连语义）
//!
//! 本 crate 不依赖异步运行时，可在注入 shim 的同步热路径中使用。

pub mod fakeip;
pub mod http_connect;
pub mod local_target;
pub mod socks5;
pub mod udp;

pub mod policy;
