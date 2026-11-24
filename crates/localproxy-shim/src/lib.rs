//! LocalProxy 注入 shim 公共逻辑。
//!
//! shim 被注入到目标进程后，负责：
//! - 读取 `LP_*` 环境变量配置
//! - FakeIP 分配与域名映射
//! - 拦截 connect 后将目标经 SOCKS5 送往本地核心
//! - 重入防护（hook 函数内部不得再次触发自身）

pub mod config;
pub mod engine;
