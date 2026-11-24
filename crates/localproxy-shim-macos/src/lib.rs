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
use localproxy_shim::engine::SockIo;

// ============= 原始函数声明 =============

extern "C" {
    fn connect(fd: c_int, addr: *const libc::sockaddr, len: libc::socklen_t) -> c_int;
    fn getaddrinfo(
        node: *const c_char,
        service: *const c_char,
        hints: *const libc::addrinfo,
        res: *mut *mut libc::addrinfo,
    ) -> c_int;
    fn sendto(
        fd: c_int,
        msg: *const c_void,
        len: usize,
        flags: c_int,
        to: *const libc::sockaddr,
        tolen: libc::socklen_t,
    ) -> isize;
    fn sendmsg(fd: c_int, msg: *const libc::msghdr, flags: c_int) -> isize;
    fn recvfrom(
        fd: c_int,
        buf: *mut c_void,
        len: usize,
        flags: c_int,
        from: *mut libc::sockaddr,
        fromlen: *mut libc::socklen_t,
    ) -> isize;
    fn close(fd: c_int) -> c_int;
    fn gethostbyname(name: *const c_char) -> *mut libc::hostent;
    fn gethostbyname2(name: *const c_char, af: c_int) -> *mut libc::hostent;
    fn getnameinfo(
        sa: *const libc::sockaddr,
        salen: libc::socklen_t,
        host: *mut c_char,
        hostlen: libc::socklen_t,
        serv: *mut c_char,
        servlen: libc::socklen_t,
        flags: c_int,
    ) -> c_int;
}

/// getnameinfo flags（macOS ABI 稳定值）。
const NI_NOFQDN: c_int = 1;
const NI_NUMERICHOST: c_int = 2;
const NI_NAMEREQD: c_int = 4;
const NI_NUMERICSERV: c_int = 8;

/// 查询 socket 类型（SO_TYPE）；失败返回 None。
unsafe fn getsockopt_type(fd: RawFd) -> Option<c_int> {
    let mut val: c_int = 0;
    let mut len = std::mem::size_of::<c_int>() as libc::socklen_t;
    // SAFETY: 栈上有效缓冲区
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            &mut val as *mut c_int as *mut libc::c_void,
            &mut len,
        )
    };
    if rc == 0 {
        Some(val)
    } else {
        None
    }
}

/// SOCK_DGRAM = 2 / SOCK_STREAM = 1（各平台稳定 ABI 常量）。
const SOCK_STREAM: c_int = 1;
const SOCK_DGRAM: c_int = 2;

/// 设置 errno（macOS: *__error()）。
unsafe fn set_errno(code: c_int) {
    unsafe { *libc::__error() = code };
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

    fn raw_socket(&self) -> i64 {
        // SAFETY: libc socket 无内存安全前提
        (unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) }) as i64
    }

    fn raw_close(&self, fd: i64) {
        // SAFETY: fd 由 raw_socket 创建
        unsafe { libc::close(fd as i32) };
    }
}

static IO: MacOsIo = MacOsIo;

// ============= hook: connect =============

/// `connect` 替换实现。
///
/// SOCK_STREAM → TCP 就地协商；SOCK_DGRAM（connected UDP socket）→ UDP 策略。
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

    // connected DGRAM socket：走 UDP 策略（block/direct）
    // SAFETY: fd 为有效 socket
    if unsafe { getsockopt_type(fd) } == Some(SOCK_DGRAM) {
        return match engine::proxy_udp(&target) {
            engine::UdpOutcome::Direct => unsafe { connect(fd, addr, len) },
            engine::UdpOutcome::Blocked(e) => {
                unsafe { set_errno(e.raw_os_error().unwrap_or(engine::ERR_NET_UNREACH)) };
                -1
            }
            // S2 落地前 engine 已将 Proxy 收敛为 Blocked；双保险
            engine::UdpOutcome::Proxy => {
                unsafe { set_errno(engine::ERR_NET_UNREACH) };
                -1
            }
        };
    }

    let _ = SOCK_STREAM;
    // 非阻塞 socket 适配：现代应用（Electron/Chromium）默认非阻塞 connect，
    // 期望 EINPROGRESS + 后续可写通知。就地协商是同步的，因此临时清除
    // O_NONBLOCK、协商完成后恢复——与 proxychains 同款策略。
    // SAFETY: fd 为有效 socket
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    let was_nonblocking = flags >= 0 && (flags & libc::O_NONBLOCK) != 0;
    if was_nonblocking {
        // SAFETY: fd 有效，flags 来自 F_GETFL
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) };
    }

    let result = engine::proxy_connect(&IO, fd as i64, &target);

    if was_nonblocking {
        // SAFETY: fd 有效
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    }

    match result {
        Ok(true) => 0,                                  // 已就地代理成功（含非阻塞适配）
        Ok(false) => unsafe { connect(fd, addr, len) }, // 直连放行
        Err(e) => {
            // 保留原有 errno 语义（ECONNREFUSED 兜底，应用侧表现与普通连接失败一致）
            unsafe { set_errno(e.raw_os_error().unwrap_or(libc::ECONNREFUSED)) };
            -1
        }
    }
}

// ============= hook: sendto / sendmsg（UDP 封堵 P0.1） =============

/// `sendto` 替换实现：公网 UDP 按 `udp_mode` 阻断（默认 block）。
#[no_mangle]
pub extern "C" fn localproxy_sendto(
    fd: RawFd,
    msg: *const c_void,
    len: usize,
    flags: c_int,
    to: *const libc::sockaddr,
    tolen: libc::socklen_t,
) -> isize {
    ensure_installed();

    let Some(_guard) = engine::HookGuard::enter() else {
        return unsafe { sendto(fd, msg, len, flags, to, tolen) };
    };

    // 无目标地址（connected socket 的 sendto 语义）：决策已在 connect hook 做出
    if to.is_null() {
        return unsafe { sendto(fd, msg, len, flags, to, tolen) };
    }
    // SAFETY: 调用方保证 to/tolen 有效
    let Some(target) = (unsafe { parse_sockaddr(to, tolen) }) else {
        return unsafe { sendto(fd, msg, len, flags, to, tolen) };
    };

    match engine::proxy_udp(&target) {
        engine::UdpOutcome::Direct => unsafe { sendto(fd, msg, len, flags, to, tolen) },
        engine::UdpOutcome::Blocked(e) => {
            tracing::debug!("shim: sendto 阻断 {}:{}", target.ip(), target.port());
            unsafe { set_errno(e.raw_os_error().unwrap_or(engine::ERR_NET_UNREACH)) };
            -1
        }
        engine::UdpOutcome::Proxy => {
            // S2 全代理：ASSOCIATE + 封装目标 + 经 relay 发送
            let Some((local_fd, relay)) = udp_session_get_or_create(fd) else {
                // SAFETY: errno 设置
                unsafe { set_errno(libc::ECONNREFUSED) };
                return -1;
            };
            let host = udp_target_host(&target);
            let port = target.port();
            // SAFETY: msg/len 由调用方保证
            let payload = unsafe { std::slice::from_raw_parts(msg as *const u8, len) };
            let packet = match localproxy_net::socks5::encode_udp_packet(&host, port, payload) {
                Ok(p) => p,
                Err(_) => {
                    // SAFETY: errno 设置
                    unsafe { set_errno(libc::EINVAL) };
                    return -1;
                }
            };
            // SAFETY: relay 为有效 sockaddr_in；local_fd 为已绑定 UDP socket
            let n = unsafe {
                sendto(
                    local_fd,
                    packet.as_ptr() as *const c_void,
                    packet.len(),
                    0,
                    &relay as *const _ as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                )
            };
            if n < 0 {
                return -1;
            }
            // 对应用户数据长度（不含 SOCKS5 封装头）
            len as isize
        }
    }
}

/// `sendmsg` 替换实现：等价 sendto（目标在 msghdr.msg_name）。
#[no_mangle]
pub extern "C" fn localproxy_sendmsg(fd: RawFd, msg: *const libc::msghdr, flags: c_int) -> isize {
    ensure_installed();

    let Some(_guard) = engine::HookGuard::enter() else {
        return unsafe { sendmsg(fd, msg, flags) };
    };
    if msg.is_null() {
        return unsafe { sendmsg(fd, msg, flags) };
    }
    // SAFETY: 调用方保证 msg 有效
    let m = unsafe { &*msg };
    if m.msg_name.is_null() {
        // connected socket：决策已在 connect hook 做出
        return unsafe { sendmsg(fd, msg, flags) };
    }
    // SAFETY: msg_name/msg_namelen 构成有效 sockaddr
    let Some(target) =
        (unsafe { parse_sockaddr(m.msg_name as *const libc::sockaddr, m.msg_namelen) })
    else {
        return unsafe { sendmsg(fd, msg, flags) };
    };

    match engine::proxy_udp(&target) {
        engine::UdpOutcome::Direct => unsafe { sendmsg(fd, msg, flags) },
        engine::UdpOutcome::Blocked(e) => {
            tracing::debug!("shim: sendmsg 阻断 {}:{}", target.ip(), target.port());
            unsafe { set_errno(e.raw_os_error().unwrap_or(engine::ERR_NET_UNREACH)) };
            -1
        }
        engine::UdpOutcome::Proxy => {
            unsafe { set_errno(engine::ERR_NET_UNREACH) };
            -1
        }
    }
}

// ============= UDP 代理会话（S2） =============

/// 一个 UDP 代理会话：控制连接 + 本地 UDP socket + 核心 relay 地址。
struct UdpSession {
    control_fd: c_int,
    local_fd: c_int,
    relay: libc::sockaddr_in,
}

/// 应用 fd → UDP 会话。
static UDP_SESSIONS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<c_int, UdpSession>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// 查询或建立 UDP 会话（按应用 fd）。
///
/// 首次 sendto 时触发：向核心 UDP ASSOCIATE → 本地 UDP socket → 记录。
/// 返回 `(本地 socket fd, relay sockaddr)`；调用方**不得**跨调用持有引用
/// （会话只可在持锁期间使用；本函数复制出值返回，避免悬垂引用）。
// 允许 contains_key + insert 两步模式：建联失败时不能留下半初始化条目
#[allow(clippy::map_entry)]
fn udp_session_get_or_create(app_fd: c_int) -> Option<(c_int, libc::sockaddr_in)> {
    let mut sessions = UDP_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    if !sessions.contains_key(&app_fd) {
        let (control_fd, relay_addr) = match engine::core_udp_associate(&IO) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("shim: UDP ASSOCIATE 失败: {e}");
                return None;
            }
        };
        // 本地 UDP socket（随机端口）
        // SAFETY: libc socket
        let local_fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        if local_fd < 0 {
            IO.raw_close(control_fd);
            return None;
        }
        // 绑定 127.0.0.1:0
        let mut local_addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        local_addr.sin_family = libc::AF_INET as libc::sa_family_t;
        local_addr.sin_port = 0u16.to_be();
        local_addr.sin_addr.s_addr = u32::from(std::net::Ipv4Addr::LOCALHOST).to_be();
        // SAFETY: local_addr 为有效 sockaddr_in
        let rc = unsafe {
            libc::bind(
                local_fd,
                &local_addr as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        };
        if rc != 0 {
            // SAFETY: local_fd 有效
            unsafe { libc::close(local_fd) };
            IO.raw_close(control_fd);
            return None;
        }

        // relay sockaddr（IPv4）
        let mut relay: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        relay.sin_family = libc::AF_INET as libc::sa_family_t;
        if let std::net::SocketAddr::V4(v4) = relay_addr {
            relay.sin_port = v4.port().to_be();
            relay.sin_addr.s_addr = u32::from(*v4.ip()).to_be();
        }

        sessions.insert(
            app_fd,
            UdpSession {
                control_fd: control_fd as c_int,
                local_fd,
                relay,
            },
        );
        tracing::debug!("shim: UDP 会话建立 fd={app_fd} relay={relay_addr}");
    }
    // 复制值返回（不返回引用，避免锁外悬垂）
    let session = sessions.get(&app_fd)?;
    Some((session.local_fd, session.relay))
}

/// 移除 UDP 会话（close 时）。
fn udp_session_remove(app_fd: c_int) {
    let mut sessions = UDP_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(session) = sessions.remove(&app_fd) {
        // SAFETY: fd 归本会话所有
        unsafe {
            libc::close(session.local_fd);
            libc::close(session.control_fd);
        }
        tracing::debug!("shim: UDP 会话移除 fd={app_fd}");
    }
}

/// 从目标地址还原（域名, 端口）：FakeIP → 映射域名；IP 字面量 → IP 字符串。
fn udp_target_host(target: &SocketAddr) -> String {
    match target {
        SocketAddr::V4(v4) => {
            let ip = *v4.ip();
            match engine::fakeip_lock().lookup_domain(u32::from(ip)) {
                Some(domain) => domain.to_string(),
                None => ip.to_string(),
            }
        }
        SocketAddr::V6(v6) => v6.ip().to_string(),
    }
}

// ============= hook: recvfrom / close（UDP 会话） =============

/// `recvfrom` 替换实现：有 UDP 会话时从本地 relay socket 收并解封装。
#[no_mangle]
pub extern "C" fn localproxy_recvfrom(
    fd: RawFd,
    buf: *mut c_void,
    len: usize,
    flags: c_int,
    from: *mut libc::sockaddr,
    fromlen: *mut libc::socklen_t,
) -> isize {
    if !engine::is_installed() {
        return unsafe { recvfrom(fd, buf, len, flags, from, fromlen) };
    }

    // 无会话 → 原函数
    let has_session = {
        let sessions = UDP_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
        sessions.contains_key(&fd)
    };
    if !has_session {
        return unsafe { recvfrom(fd, buf, len, flags, from, fromlen) };
    }

    let Some((local_fd, _relay)) = udp_session_get_or_create(fd) else {
        return unsafe { recvfrom(fd, buf, len, flags, from, fromlen) };
    };

    // 从本地 relay socket 收（透传应用的阻塞/非阻塞语义由 socket 自身决定）
    let mut packet = vec![0u8; 65536];
    // SAFETY: local_fd 为已绑定 UDP socket；packet 可写
    let n = unsafe {
        recvfrom(
            local_fd,
            packet.as_mut_ptr() as *mut c_void,
            packet.len(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if n <= 0 {
        return n;
    }
    let Some((src_host, src_port, payload)) =
        localproxy_net::socks5::decode_udp_packet(&packet[..n as usize])
    else {
        // SAFETY: errno 设置
        unsafe { set_errno(libc::EAGAIN) };
        return -1;
    };

    // 回填数据（截断到调用方缓冲区）
    let copy_len = payload.len().min(len);
    // SAFETY: buf 可写 len 字节；payload 可读 copy_len 字节
    unsafe { std::ptr::copy_nonoverlapping(payload.as_ptr(), buf as *mut u8, copy_len) };

    // 回填源地址（IPv4 sockaddr_in）
    if !from.is_null() && !fromlen.is_null() {
        let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        addr.sin_family = libc::AF_INET as libc::sa_family_t;
        addr.sin_port = src_port.to_be();
        if let Ok(ip) = src_host.parse::<std::net::Ipv4Addr>() {
            addr.sin_addr.s_addr = u32::from(ip).to_be();
        }
        // SAFETY: from 缓冲区容量由 fromlen 给出
        unsafe {
            let cap = *fromlen as usize;
            let write = std::mem::size_of::<libc::sockaddr_in>().min(cap);
            std::ptr::copy_nonoverlapping(&addr as *const _ as *const u8, from as *mut u8, write);
            *fromlen = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        }
    }
    copy_len as isize
}

/// `close` 替换实现：清理 UDP 会话。
///
/// 注意：`close` 会被 stdio/malloc 在极早期调用，此处**绝不能**触发
/// 引擎安装（其堆分配会在 malloc 重入上下文中损坏堆）；只做无分配的就绪检查。
#[no_mangle]
pub extern "C" fn localproxy_close(fd: RawFd) -> c_int {
    if engine::is_installed() {
        udp_session_remove(fd);
    }
    unsafe { close(fd) }
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

// ============= hook: gethostbyname / getnameinfo（DNS API 补齐 P0.3） =============

/// 线程局部 FakeIP hostent 存储（自引用结构，Box 固定地址）。
struct FakeHostent {
    name: std::ffi::CString,
    addr: [u8; 4],
    aliases: [*mut c_char; 1],
    addr_list: [*mut c_char; 2],
    hostent: libc::hostent,
}

thread_local! {
    static FAKE_HOSTENT: std::cell::RefCell<Option<*mut FakeHostent>> =
        const { std::cell::RefCell::new(None) };
}

/// 构造（或重建）指向 FakeIP 的 hostent，返回稳定指针。
///
/// 与 getaddrinfo 完全一致的放行条件；`localhost`/`.local`/回环/私网走原函数。
unsafe fn build_fake_hostent(host: &str, ip: [u8; 4]) -> Option<*mut libc::hostent> {
    let name = std::ffi::CString::new(host).ok()?;
    let mut cell = Box::new(FakeHostent {
        name,
        addr: ip,
        aliases: [std::ptr::null_mut()],
        addr_list: [std::ptr::null_mut(), std::ptr::null_mut()],
        // SAFETY: hostent 为 POD，零初始化合法
        hostent: unsafe { std::mem::zeroed() },
    });
    cell.addr_list[0] = cell.addr.as_mut_ptr() as *mut c_char;
    // unsafe fn 体内默认处于 unsafe 上下文（edition 2024 语义）
    cell.hostent.h_name = cell.name.as_ptr() as *mut c_char;
    cell.hostent.h_addrtype = 2; // AF_INET
    cell.hostent.h_length = 4;
    cell.hostent.h_aliases = cell.aliases.as_mut_ptr();
    cell.hostent.h_addr_list = cell.addr_list.as_mut_ptr();
    let box_ptr = Box::into_raw(cell);
    let hostent_ptr: *mut libc::hostent = &raw mut (*box_ptr).hostent;
    FAKE_HOSTENT.with(|slot| {
        // 释放上一次的 Box（保持每线程至多一份）
        if let Some(old) = slot.borrow_mut().replace(box_ptr) {
            // SAFETY: old 来自上一次 Box::into_raw
            drop(unsafe { Box::from_raw(old) });
        }
    });
    Some(hostent_ptr)
}

/// `gethostbyname` 替换实现：非本地域名返回 FakeIP hostent。
#[no_mangle]
pub extern "C" fn localproxy_gethostbyname(name: *const c_char) -> *mut libc::hostent {
    ensure_installed();

    let Some(_guard) = engine::HookGuard::enter() else {
        return unsafe { gethostbyname(name) };
    };
    if name.is_null() {
        return unsafe { gethostbyname(name) };
    }
    // SAFETY: name 为 NUL 结尾字符串
    let Ok(host) = (unsafe { std::ffi::CStr::from_ptr(name) }).to_str() else {
        return unsafe { gethostbyname(name) };
    };
    if !engine::should_fakeip(host) {
        return unsafe { gethostbyname(name) };
    }
    match engine::fakeip_assign(host) {
        Some(ip) => unsafe { build_fake_hostent(host, ip.to_be_bytes()) }
            .unwrap_or_else(|| unsafe { gethostbyname(name) }),
        None => unsafe { gethostbyname(name) },
    }
}

/// `gethostbyname2` 替换实现：仅处理 AF_INET。
#[no_mangle]
pub extern "C" fn localproxy_gethostbyname2(name: *const c_char, af: c_int) -> *mut libc::hostent {
    ensure_installed();

    let Some(_guard) = engine::HookGuard::enter() else {
        return unsafe { gethostbyname2(name, af) };
    };
    if af != 2 || name.is_null() {
        return unsafe { gethostbyname2(name, af) };
    }
    // SAFETY: name 为 NUL 结尾字符串
    let Ok(host) = (unsafe { std::ffi::CStr::from_ptr(name) }).to_str() else {
        return unsafe { gethostbyname2(name, af) };
    };
    if !engine::should_fakeip(host) {
        return unsafe { gethostbyname2(name, af) };
    }
    match engine::fakeip_assign(host) {
        Some(ip) => unsafe { build_fake_hostent(host, ip.to_be_bytes()) }
            .unwrap_or_else(|| unsafe { gethostbyname2(name, af) }),
        None => unsafe { gethostbyname2(name, af) },
    }
}

/// `getnameinfo` 替换实现：反查 FakeIP → 返回映射域名（消除 198.18.x.x 字面量暴露）。
#[no_mangle]
pub extern "C" fn localproxy_getnameinfo(
    sa: *const libc::sockaddr,
    salen: libc::socklen_t,
    host: *mut c_char,
    hostlen: libc::socklen_t,
    serv: *mut c_char,
    servlen: libc::socklen_t,
    flags: c_int,
) -> c_int {
    ensure_installed();

    let Some(_guard) = engine::HookGuard::enter() else {
        return unsafe { getnameinfo(sa, salen, host, hostlen, serv, servlen, flags) };
    };

    // 解析地址；非 FakeIP → 原函数
    // SAFETY: 调用方保证 sa/salen 有效
    let target = unsafe { parse_sockaddr(sa, salen) };
    let fake_domain = match &target {
        Some(SocketAddr::V4(v4)) => {
            let ip = u32::from(*v4.ip());
            if engine::is_fakeip(ip) {
                engine::fakeip_domain_for(ip)
            } else {
                None
            }
        }
        _ => None,
    };
    let Some(domain) = fake_domain else {
        return unsafe { getnameinfo(sa, salen, host, hostlen, serv, servlen, flags) };
    };
    // NI_NUMERICHOST 明确要数字形式：放行原函数
    if flags & NI_NUMERICHOST != 0 {
        return unsafe { getnameinfo(sa, salen, host, hostlen, serv, servlen, flags) };
    }

    // host ← 映射域名
    if !host.is_null() && hostlen > 0 {
        let domain_c = std::ffi::CString::new(domain.clone()).unwrap_or_default();
        // SAFETY: host 缓冲区由调用方提供，hostlen 为其容量
        unsafe {
            libc::strncpy(
                host,
                domain_c.as_ptr(),
                (hostlen as usize).saturating_sub(1),
            );
            *host.add(hostlen as usize - 1) = 0;
        }
    }
    // serv ← 端口数字（NI_NAMEREQD 语义下服务名查询对 FakeIP 无意义）
    if !serv.is_null() && servlen > 0 {
        if let Some(SocketAddr::V4(v4)) = &target {
            let port = v4.port().to_string();
            let port_c = std::ffi::CString::new(port).unwrap_or_default();
            // SAFETY: serv 缓冲区由调用方提供
            unsafe {
                libc::strncpy(serv, port_c.as_ptr(), (servlen as usize).saturating_sub(1));
                *serv.add(servlen as usize - 1) = 0;
            }
        }
    }
    let _ = (NI_NOFQDN, NI_NAMEREQD, NI_NUMERICSERV);
    0
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

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_SENDTO: InterposePair = InterposePair {
    replacement: localproxy_sendto as *const c_void,
    original: sendto as *const c_void,
};

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_SENDMSG: InterposePair = InterposePair {
    replacement: localproxy_sendmsg as *const c_void,
    original: sendmsg as *const c_void,
};

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_RECVFROM: InterposePair = InterposePair {
    replacement: localproxy_recvfrom as *const c_void,
    original: recvfrom as *const c_void,
};

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_CLOSE: InterposePair = InterposePair {
    replacement: localproxy_close as *const c_void,
    original: close as *const c_void,
};

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_GETHOSTBYNAME: InterposePair = InterposePair {
    replacement: localproxy_gethostbyname as *const c_void,
    original: gethostbyname as *const c_void,
};

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_GETHOSTBYNAME2: InterposePair = InterposePair {
    replacement: localproxy_gethostbyname2 as *const c_void,
    original: gethostbyname2 as *const c_void,
};

#[link_section = "__DATA,__interpose"]
#[used]
static INTERPOSE_GETNAMEINFO: InterposePair = InterposePair {
    replacement: localproxy_getnameinfo as *const c_void,
    original: getnameinfo as *const c_void,
};

/// shim 构造入口（供测试或手动初始化）。
#[no_mangle]
pub extern "C" fn localproxy_shim_init() -> bool {
    ensure_installed();
    engine::is_installed()
}
