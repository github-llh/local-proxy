//! LocalProxy 平台启动器。
//!
//! 负责以「代理模式」启动用户登记的应用：
//! - macOS：解析 `.app` 主二进制，`posix_spawn` + `DYLD_INSERT_LIBRARIES` 注入 shim dylib
//! - Windows：`CreateProcessW(CREATE_SUSPENDED)` + 远程线程注入 shim DLL

pub mod app_entry;
