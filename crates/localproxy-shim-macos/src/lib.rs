//! LocalProxy macOS 注入 shim。
//!
//! 经 `DYLD_INSERT_LIBRARIES` 注入目标进程，
//! 使用 `__DATA,__interpose` 段 hook BSD socket 系列函数：
//!
//! - `connect`：TCP 连接 → 经本地核心 SOCKS5 代理（dup2 替换 fd）
//! - `getaddrinfo`：域名解析 → 返回 FakeIP（198.18.x.x），应用不发出真实 DNS
//!
//! 注意：
//! - 产物需 adhoc 签名（`codesign -s -`）方可在 arm64 加载；
//! - 被注入的 app 若启用 hardened runtime，系统会拒绝加载（launcher 会提前检测）；
//! - 子进程经 `DYLD_INSERT_LIBRARIES` 环境变量自动继承注入。

// 整个 crate 仅在 macOS target 编译（与 shim-win 的 #![cfg(windows)] 对称：
// 保证在任一台机器上 `cargo check --workspace` 都不会尝试构建对方平台源码）。
#![cfg(target_os = "macos")]
#![allow(clippy::missing_safety_doc)]
// FFI 导出函数（interpose 替换目标）：指针有效性由 C 调用约定保证，
// 函数体内已用 unsafe 块解引用；不能标 unsafe fn（dyld 替换表需普通符号）。
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(non_camel_case_types)]

use std::net::{Ipv4Addr, SocketAddr};
use std::os::fd::RawFd;
use std::os::raw::{c_char, c_int, c_void};

use localproxy_shim::config::ShimConfig;
use localproxy_shim::engine;

// ============= 原始函数声明 =============

extern "C" {
    fn connect(fd: c_int, addr: *const libc::sockaddr, len: libc::socklen_t) -> c_int;
    fn getaddrinfo(
        node: *const c_char,
        service: *const c_char,
        hints: *const libc::addrinfo,
        res: *mut *mut libc::addrinfo,
    ) -> c_int;
}

// ============= 初始化（懒加载） =============

/// 确保引擎已安装（首次 hook 调用时触发；环境变量由 launcher 注入）。
fn ensure_installed() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let cfg = ShimConfig::from_env();
        engine::install(&cfg);
    });
}

// ============= sockaddr 解析 =============

/// 把 libc sockaddr 解析为 Rust SocketAddr（仅支持 INET/INET6）。
unsafe fn parse_sockaddr(addr: *const libc::sockaddr, len: libc::socklen_t) -> Option<SocketAddr> {
    if addr.is_null() {
        return None;
    }
    let family = (*addr).sa_family as i32;
    if family == libc::AF_INET {
        if (len as usize) < std::mem::size_of::<libc::sockaddr_in>() {
            return None;
        }
        let sin = &*(addr as *const libc::sockaddr_in);
        let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
        let port = u16::from_be(sin.sin_port);
        Some(SocketAddr::from((ip, port)))
    } else if family == libc::AF_INET6 {
        if (len as usize) < std::mem::size_of::<libc::sockaddr_in6>() {
            return None;
        }
        let sin6 = &*(addr as *const libc::sockaddr_in6);
        let ip = std::net::Ipv6Addr::from(sin6.sin6_addr.s6_addr);
        let port = u16::from_be(sin6.sin6_port);
        Some(SocketAddr::from((ip, port)))
    } else {
        None
    }
}

// ============= 平台 SockIo 实现 =============

/// macOS 平台 IO（libc connect/read/write）。
struct MacOsIo;

impl engine::SockIo for MacOsIo {
    unsafe fn raw_connect(&self, fd: i64, addr: *const u8, len: u32) -> i32 {
        // SAFETY: 由引擎约定 addr/len 有效
        unsafe {
            libc::connect(
                fd as i32,
                addr as *const libc::sockaddr,
                len as libc::socklen_t,
            )
        }
    }

    unsafe fn raw_read(&self, fd: i64, buf: *mut u8, len: usize) -> isize {
        // SAFETY: 由引擎约定 buf 可写 len 字节
        unsafe { libc::read(fd as i32, buf as *mut libc::c_void, len) as isize }
    }

    unsafe fn raw_write(&self, fd: i64, buf: *const u8, len: usize) -> isize {
        // SAFETY: 由引擎约定 buf 可读 len 字节
        unsafe { libc::write(fd as i32, buf as *const libc::c_void, len) as isize }
    }

    fn core_addr(&self) -> Option<SocketAddr> {
        // 核心地址必须是 IP 字面量（域名会经 getaddrinfo hook 拿到 FakeIP，形成环）
        engine::config().core_endpoint().parse().ok()
    }
}

static IO: MacOsIo = MacOsIo;

// ============= hook: connect =============

/// `connect` 替换实现。
#[no_mangle]
pub extern "C" fn localproxy_connect(
    fd: RawFd,
    addr: *const libc::sockaddr,
    len: libc::socklen_t,
) -> c_int {
    ensure_installed();

    // 重入防护：核心拨号内部会调 connect，必须放行
    let Some(_guard) = engine::HookGuard::enter() else {
        return unsafe { connect(fd, addr, len) };
    };

    // SAFETY: 调用方保证 addr/len 有效
    let Some(target) = (unsafe { parse_sockaddr(addr, len) }) else {
        return unsafe { connect(fd, addr, len) };
    };

    match engine::proxy_connect(&IO, fd as i64, &target) {
        Ok(true) => 0,                                  // 已就地代理成功
        Ok(false) => unsafe { connect(fd, addr, len) }, // 直连放行
        Err(_) => {
            // 以 ECONNREFUSED 语义失败（应用侧表现与普通连接失败一致）
            unsafe { *libc::__error() = libc::ECONNREFUSED };
            -1
        }
    }
}

// ============= hook: getaddrinfo =============

/// 构造指向 FakeIP 的 addrinfo 链表（单节点，AI_NUMERICHOST 之外的调用均可）。
unsafe fn build_fake_addrinfo(ip: Ipv4Addr, port: u16) -> *mut libc::addrinfo {
    let ai = libc::calloc(1, std::mem::size_of::<libc::addrinfo>()) as *mut libc::addrinfo;
    if ai.is_null() {
        return std::ptr::null_mut();
    }
    (*ai).ai_family = libc::AF_INET;
    (*ai).ai_socktype = libc::SOCK_STREAM;
    (*ai).ai_protocol = libc::IPPROTO_TCP;
    (*ai).ai_flags = 0;
    (*ai).ai_next = std::ptr::null_mut();
    (*ai).ai_canonname = std::ptr::null_mut();

    let sa = libc::calloc(1, std::mem::size_of::<libc::sockaddr_in>()) as *mut libc::sockaddr_in;
    if sa.is_null() {
        libc::free(ai as *mut c_void);
        return std::ptr::null_mut();
    }
    (*sa).sin_family = libc::AF_INET as libc::sa_family_t;
    (*sa).sin_port = port.to_be();
    (*sa).sin_addr.s_addr = u32::from(ip).to_be();
    (*ai).ai_addr = sa as *mut libc::sockaddr;
    (*ai).ai_addrlen = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    ai
}

/// `getaddrinfo` 替换实现：非本地域名返回 FakeIP。
#[no_mangle]
pub extern "C" fn localproxy_getaddrinfo(
    node: *const c_char,
    service: *const c_char,
    hints: *const libc::addrinfo,
    res: *mut *mut libc::addrinfo,
) -> c_int {
    ensure_installed();

    let Some(_guard) = engine::HookGuard::enter() else {
        return unsafe { getaddrinfo(node, service, hints, res) };
    };

    // 只处理域名场景（node 为 NULL 时按原函数走）
    if node.is_null() || res.is_null() {
        return unsafe { getaddrinfo(node, service, hints, res) };
    }
    // SAFETY: node 以 NUL 结尾
    let node_str = match unsafe { std::ffi::CStr::from_ptr(node) }.to_str() {
        Ok(s) => s,
        Err(_) => return unsafe { getaddrinfo(node, service, hints, res) },
    };
    if !engine::should_fakeip(node_str) {
        // localhost / .local / 回环：真实解析
        return unsafe { getaddrinfo(node, service, hints, res) };
    }

    // hints 要求 AI_NUMERICHOST / AI_PASSIVE 等场景不代理
    if !hints.is_null() {
        // SAFETY: hints 有效
        let h = unsafe { &*hints };
        if h.ai_flags & libc::AI_NUMERICHOST != 0 || h.ai_flags & libc::AI_PASSIVE != 0 {
            return unsafe { getaddrinfo(node, service, hints, res) };
        }
    }

    // service → 端口
    let service_str = if service.is_null() {
        None
    } else {
        // SAFETY: service 以 NUL 结尾
        unsafe { std::ffi::CStr::from_ptr(service) }.to_str().ok()
    };
    let port = engine::parse_service_port(service_str);

    // 分配 FakeIP 并记录映射
    let ip_u32 = match engine::fakeip_lock().assign(node_str) {
        Some(ip) => ip,
        None => return unsafe { getaddrinfo(node, service, hints, res) },
    };
    let fake_ip = Ipv4Addr::from(ip_u32);

    // SAFETY: 构造标准 addrinfo 单节点链表，可被 freeaddrinfo 释放
    let ai = unsafe { build_fake_addrinfo(fake_ip, port) };
    if ai.is_null() {
        return libc::EAI_MEMORY;
    }
    // SAFETY: res 由调用方提供且非空
    unsafe { *res = ai };
    tracing::debug!("shim: FakeIP {node_str} -> {fake_ip}:{port}");
    0 // 成功
}

// ============= interpose 段 =============

/// `__DATA,__interpose` 段条目：(replacement, original)。
/// dyld 在镜像加载时据此完成符号替换。
///
/// 注意：
/// - `*const c_void` 非 Sync，不能直接进 static，用 wrapper struct + `unsafe impl Sync`；
/// - fn item → 函数指针的 cast 在 const 上下文中合法（指针转整数才非法）。
#[repr(C)]
struct InterposePair {
    replacement: *const c_void,
    original: *const c_void,
}

// SAFETY: interpose 表中的地址在镜像生命周期内恒定，跨线程只读
unsafe impl Sync for InterposePair {}

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_CONNECT: InterposePair = InterposePair {
    replacement: localproxy_connect as *const c_void,
    original: connect as *const c_void,
};

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_GETADDRINFO: InterposePair = InterposePair {
    replacement: localproxy_getaddrinfo as *const c_void,
    original: getaddrinfo as *const c_void,
};

/// shim 构造入口（供测试或手动初始化）。
#[no_mangle]
pub extern "C" fn localproxy_shim_init() -> bool {
    ensure_installed();
    engine::is_installed()
}
