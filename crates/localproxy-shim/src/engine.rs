//! shim 引擎：connect 就地代理、getaddrinfo FakeIP、重入防护。
//!
//! 平台 hook 层（macOS interpose / Windows MinHook）只负责拦截与 IO，
//! 所有决策与协议逻辑收敛到这里，保证两平台行为一致。
//!
//! # TCP 代理原理（原 socket 就地协商）
//! 1. `getaddrinfo("example.com")` 被拦截 → 分配 FakeIP（198.18.x.x）并记录
//!    `FakeIP → example.com`，应用随后 `connect(FakeIP, 443)`；
//! 2. `connect` 被拦截 → 查表得真实域名 → 把**原 socket** 连到本地核心，
//!    在其上完成 SOCKS5 协商（ATYP_DOMAIN 透传域名）→ 返回成功；
//! 3. 应用后续 `send`/`recv` 对原 socket 的操作自然流经代理。
//!
//! 选「就地协商」而非「dup2 换 fd」的原因：Windows 的 SOCKET 没有 fd 替换
//! 等价物，就地协商是唯一两平台一致的方案。
//!
//! 全程应用不发出真实 DNS 查询（非局域网目标）。

use std::net::{Ipv4Addr, SocketAddr};

use localproxy_net::fakeip::FakeIp;
use localproxy_net::local_target;
use localproxy_net::socks5::{self, consts, ProxyAuth, Timeouts};

use crate::config::{Ipv6Mode, ShimConfig, ShimMode, UdpMode};

/// 全局配置（首次 install 后不变）。
static CONFIG: std::sync::OnceLock<ShimConfig> = std::sync::OnceLock::new();

/// 引擎是否已安装。
static INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

// 重入防护：hook 函数内部若再次触发网络调用，直接放行原始调用。
thread_local! {
    static IN_HOOK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// 平台 socket IO 抽象。
///
/// 平台层实现（macOS: libc；Windows: ws2_32）。fd 统一用 `i64` 承载
/// （unix `RawFd` 为 i32、Windows `SOCKET` 为 UINT_PTR，平台内部转换）。
pub trait SockIo: Send + Sync + 'static {
    /// 在已有 fd 上发起连接（对应 `connect(2)` / `connect` winsock）。
    ///
    /// # Safety
    /// `addr` 必须指向有效的 sockaddr，`addr_len` 为其字节数。
    unsafe fn raw_connect(&self, fd: i64, addr: *const u8, addr_len: u32) -> i32;

    /// 读取（对应 `read(2)` / `recv`）。
    ///
    /// # Safety
    /// `buf` 必须可写 `len` 字节。
    unsafe fn raw_read(&self, fd: i64, buf: *mut u8, len: usize) -> isize;

    /// 写入（对应 `write(2)` / `send`）。
    ///
    /// # Safety
    /// `buf` 必须可读 `len` 字节。
    unsafe fn raw_write(&self, fd: i64, buf: *const u8, len: usize) -> isize;

    /// 本地核心的 socket 地址（由配置的 host:port 解析）。
    fn core_addr(&self) -> Option<SocketAddr>;

    /// 创建 TCP socket（UDP Associate 的控制连接用）；失败返回负值。
    fn raw_socket(&self) -> i64;

    /// 关闭 fd。
    fn raw_close(&self, fd: i64);
}

/// 安装引擎（幂等）。返回 false 表示已安装或配置非法。
pub fn install(cfg: &ShimConfig) -> bool {
    if INSTALLED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return false;
    }
    if FakeIp::new(&cfg.fakeip_cidr).is_err() {
        tracing::error!("shim: FakeIP CIDR 非法: {}", cfg.fakeip_cidr);
        INSTALLED.store(false, std::sync::atomic::Ordering::SeqCst);
        return false;
    }
    let _ = CONFIG.set(cfg.clone());
    tracing::info!(
        "shim: 引擎已安装, core={}, mode={:?}",
        cfg.core_endpoint(),
        cfg.mode
    );
    true
}

/// 当前配置（未安装时为默认值）。
pub fn config() -> &'static ShimConfig {
    CONFIG.get_or_init(ShimConfig::default)
}

/// 取当前配置的 FakeIP 分配器（Mutex 保护：分配/回收需可变访问）。
pub fn fakeip() -> &'static std::sync::Mutex<FakeIp> {
    static FAKEIP: std::sync::OnceLock<std::sync::Mutex<FakeIp>> = std::sync::OnceLock::new();
    FAKEIP.get_or_init(|| {
        std::sync::Mutex::new(
            FakeIp::new(&config().fakeip_cidr).unwrap_or_else(|_| FakeIp::with_default_cidr()),
        )
    })
}

/// FakeIP 锁辅助：忽略 poison（panic 后映射表仍可用）。
pub fn fakeip_lock() -> std::sync::MutexGuard<'static, FakeIp> {
    fakeip().lock().unwrap_or_else(|e| e.into_inner())
}

/// 重入防护 RAII guard。
pub struct HookGuard;

impl HookGuard {
    /// 进入 hook 处理；若已在 hook 中返回 None（调用方应放行原始调用）。
    pub fn enter() -> Option<Self> {
        IN_HOOK.with(|in_hook| {
            if in_hook.get() {
                None
            } else {
                in_hook.set(true);
                Some(HookGuard)
            }
        })
    }
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        IN_HOOK.with(|in_hook| in_hook.set(false));
    }
}

/// 引擎是否已安装。
pub fn is_installed() -> bool {
    INSTALLED.load(std::sync::atomic::Ordering::SeqCst)
}

// ============= 地址判定 =============

/// 私网 IPv4（含回环、链路本地）：shim 侧直连，语义对齐 core 的 default-private 规则。
pub fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    match o[0] {
        10 | 127 => true,
        172 => o[1] & 0xF0 == 16, // 172.16.0.0/12
        192 => o[1] == 168,       // 192.168.0.0/16
        169 => o[1] == 254,       // 169.254.0.0/16
        _ => false,
    }
}

/// 私网 IPv6（fc00::/7 ULA、fe80::/10 链路本地、::1）。
pub fn is_private_ipv6(ip: std::net::Ipv6Addr) -> bool {
    // IPv4-mapped（::ffff:a.b.c.d）按 v4 语义判定（含回环 127.x）
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_private_ipv4(mapped);
    }
    let seg = ip.segments();
    (seg[0] & 0xFE00) == 0xFC00 || (seg[0] & 0xFFC0) == 0xFE80 || ip.is_loopback()
}

/// SocketAddr 是否应直连（回环 / 私网）。
pub fn should_bypass(addr: &SocketAddr) -> bool {
    match addr {
        SocketAddr::V4(v4) => local_target::is_ipv4_loopback(*v4.ip()) || is_private_ipv4(*v4.ip()),
        SocketAddr::V6(v6) => local_target::is_ipv6_loopback(*v6.ip()) || is_private_ipv6(*v6.ip()),
    }
}

/// 域名是否应该分配 FakeIP（排除回环/本地域名）。
pub fn should_fakeip(node: &str) -> bool {
    !local_target::should_bypass_proxy_host(node)
}

/// 由 service 字符串解析端口（"80"/"http"/NULL）。
pub fn parse_service_port(service: Option<&str>) -> u16 {
    let Some(service) = service else {
        return 0;
    };
    if let Ok(port) = service.parse::<u16>() {
        return port;
    }
    match service.to_ascii_lowercase().as_str() {
        "http" | "www" | "www-http" => 80,
        "https" | "ssl" => 443,
        "ftp" => 21,
        "ssh" => 22,
        "dns" | "domain" => 53,
        "smtp" => 25,
        _ => 0,
    }
}

// ============= DNS API 辅助（P0.3） =============

/// 判断 IPv4（host order）是否落在 FakeIP 网段内。
pub fn is_fakeip(ip: u32) -> bool {
    fakeip_lock().contains(ip)
}

/// FakeIP 反查域名（getnameinfo 用）；无映射返回 None。
pub fn fakeip_domain_for(ip: u32) -> Option<String> {
    fakeip_lock().lookup_domain(ip).map(str::to_string)
}

/// 为域名分配 FakeIP（gethostbyname 用）；返回 host order u32。
pub fn fakeip_assign(domain: &str) -> Option<u32> {
    fakeip_lock().assign(domain)
}

// ============= connect 就地代理 =============

/// 平台 errno：网络不可达（促使 QUIC→TCP、WebRTC→TCP candidate 的标准降级信号）。
/// macOS/Linux: ENETUNREACH=51；Windows: WSAENETUNREACH=10051。
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "ios"))]
pub const ERR_NET_UNREACH: i32 = 51;
#[cfg(target_os = "windows")]
pub const ERR_NET_UNREACH: i32 = 10051;

/// 平台 errno：地址族不支持（阻断公网 IPv6）。
/// macOS/Linux: EAFNOSUPPORT=97；Windows: WSAEAFNOSUPPORT=10047。
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "ios"))]
pub const ERR_AF_NOT_SUPPORTED: i32 = 97;
#[cfg(target_os = "windows")]
pub const ERR_AF_NOT_SUPPORTED: i32 = 10047;

/// UDP 阻断错误（应用视角：网络不可达 → 回退 TCP）。
pub fn udp_blocked_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(ERR_NET_UNREACH)
}

/// IPv6 阻断错误。
pub fn ipv6_blocked_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(ERR_AF_NOT_SUPPORTED)
}

/// IPv6 策略判定（P0.2）。
///
/// 在 `should_bypass`（回环/私网已放行）之后调用：
/// - `Ok(false)`：直连放行（`ipv6_mode=direct`）
/// - `Ok(true)`：继续走代理（`ipv6_mode=proxy`）
/// - `Err`：阻断（`ipv6_mode=block`，公网 v6）
pub fn check_ipv6_policy(target: &SocketAddr) -> std::io::Result<bool> {
    let SocketAddr::V6(v6) = target else {
        return Ok(true); // v4 不受 v6 策略影响
    };
    let ip = *v6.ip();
    if is_private_ipv6(ip) || ip.is_loopback() {
        return Ok(false); // 内网/回环 v6 直连
    }
    match config().ipv6_mode {
        Ipv6Mode::Block => Err(ipv6_blocked_error()),
        Ipv6Mode::Direct => Ok(false),
        Ipv6Mode::Proxy => Ok(true),
    }
}

/// UDP 发送决策（P0.1）。
///
/// 覆盖 `sendto`/`sendmsg`/`WSASendTo` 与 connected DGRAM socket 的 `connect`：
/// - 回环/私网/mDNS → `Direct`（正常应用行为不受影响）
/// - `udp_mode=direct` → `Direct`
/// - `udp_mode=block`（默认）→ `Blocked`（`ENETUNREACH` 促使应用回退 TCP）
/// - `udp_mode=proxy` → `Proxy`（S2 实现；当前回落 Blocked 并记录）
#[derive(Debug)]
pub enum UdpOutcome {
    /// 放行原始 sendto
    Direct,
    /// 阻断（附错误，hook 层设置 errno 并返回 -1）
    Blocked(std::io::Error),
    /// 走 UDP 代理（S2）
    Proxy,
}

pub fn proxy_udp(target: &SocketAddr) -> UdpOutcome {
    let cfg = config();
    if cfg.mode == ShimMode::Direct || !is_installed() {
        return UdpOutcome::Direct;
    }
    if should_bypass(target) {
        return UdpOutcome::Direct;
    }
    match cfg.udp_mode {
        UdpMode::Direct => UdpOutcome::Direct,
        UdpMode::Block => {
            tracing::debug!(
                "shim: UDP block {}:{}（回退 TCP 信号）",
                target.ip(),
                target.port()
            );
            UdpOutcome::Blocked(udp_blocked_error())
        }
        UdpMode::Proxy => {
            // S2 落地前按 block 处理（fail-closed），并明确记录
            tracing::debug!(
                "shim: UDP proxy 尚未实现，临时按 block {}:{}",
                target.ip(),
                target.port()
            );
            UdpOutcome::Blocked(udp_blocked_error())
        }
    }
}

/// 代理一次 TCP connect（在原 fd 上就地协商）。
///
/// - `Ok(true)`：已代为完成代理连接，hook 层应返回 0
/// - `Ok(false)`：直连放行，hook 层应调用原始 connect
/// - `Err(e)`：代理失败，hook 层应返回 -1
pub fn proxy_connect(io: &dyn SockIo, fd: i64, target: &SocketAddr) -> std::io::Result<bool> {
    let cfg = config();
    if cfg.mode == ShimMode::Direct || !is_installed() {
        return Ok(false);
    }
    if should_bypass(target) {
        return Ok(false);
    }

    // IPv6 策略（P0.2）：公网 v6 按 ipv6_mode 决策
    if !check_ipv6_policy(target)? {
        return Ok(false);
    }

    // FakeIP → 域名；无映射（IP 字面量）则直接用 IP
    let host = match target {
        SocketAddr::V4(v4) => {
            let ip = *v4.ip();
            match fakeip_lock().lookup_domain(u32::from(ip)) {
                Some(domain) => domain.to_string(),
                None => ip.to_string(),
            }
        }
        SocketAddr::V6(v6) => v6.ip().to_string(),
    };
    let port = target.port();

    match dial_core_in_place(io, fd, &host, port) {
        Ok(()) => {
            tracing::debug!("shim: 代理 connect fd={fd} -> {host}:{port}");
            Ok(true)
        }
        Err(e) => {
            tracing::warn!("shim: 代理 connect 失败 {host}:{port}: {e}");
            Err(e)
        }
    }
}

/// 向本地核心发起 SOCKS5 UDP ASSOCIATE（S2）。
///
/// 返回 `(控制连接 fd, 中继地址)`；控制连接必须保持存活
/// （关闭即关联终止），由调用方持有并在会话结束时 `raw_close`。
pub fn core_udp_associate(io: &dyn SockIo) -> std::io::Result<(i64, SocketAddr)> {
    let cfg = config();
    let core = io.core_addr().ok_or_else(|| {
        std::io::Error::other(format!("核心地址解析失败: {}", cfg.core_endpoint()))
    })?;

    let fd = io.raw_socket();
    if fd < 0 {
        return Err(std::io::Error::other("创建 socket 失败"));
    }
    let (addr_bytes, addr_len) = encode_sockaddr(&core);
    // SAFETY: addr_bytes 构成有效 sockaddr
    let rc = unsafe { io.raw_connect(fd, addr_bytes.as_ptr(), addr_len) };
    if rc != 0 {
        io.raw_close(fd);
        return Err(std::io::Error::other("连接本地核心失败"));
    }

    let auth = ProxyAuth::default();
    let timeouts = Timeouts::default();

    // 方法协商
    let greeting = socks5::build_greeting(&auth);
    if let Err(e) = write_all(io, fd, &greeting, timeouts) {
        io.raw_close(fd);
        return Err(e);
    }
    let mut method_resp = [0u8; 2];
    if let Err(e) = read_exact(io, fd, &mut method_resp, timeouts) {
        io.raw_close(fd);
        return Err(e);
    }
    if let Err(e) = socks5::parse_method_response(&method_resp) {
        io.raw_close(fd);
        return Err(std::io::Error::other(e.to_string()));
    }

    // UDP ASSOCIATE 请求（DST 0.0.0.0:0）
    let mut request = Vec::with_capacity(10);
    request.push(consts::VERSION);
    request.push(consts::CMD_UDP_ASSOCIATE);
    request.push(0x00);
    request.push(consts::ATYP_IPV4);
    request.extend_from_slice(&[0, 0, 0, 0]);
    request.extend_from_slice(&[0, 0]);
    if let Err(e) = write_all(io, fd, &request, timeouts) {
        io.raw_close(fd);
        return Err(e);
    }

    // 响应
    let mut header = [0u8; 4];
    if let Err(e) = read_exact(io, fd, &mut header, timeouts) {
        io.raw_close(fd);
        return Err(e);
    }
    let reply = match socks5::parse_reply_header(&header) {
        Ok(r) => r,
        Err(e) => {
            io.raw_close(fd);
            return Err(std::io::Error::other(e.to_string()));
        }
    };
    if reply.rep != consts::REPLY_SUCCESS {
        io.raw_close(fd);
        return Err(std::io::Error::other(format!(
            "核心拒绝 UDP ASSOCIATE (REP={}, {})",
            reply.rep,
            socks5::reply_to_text(reply.rep)
        )));
    }
    let domain_len = if reply.atyp == consts::ATYP_DOMAIN {
        let mut len_byte = [0u8; 1];
        if read_exact(io, fd, &mut len_byte, timeouts).is_err() {
            io.raw_close(fd);
            return Err(std::io::Error::other("读取 BND 域名长度失败"));
        }
        Some(len_byte[0])
    } else {
        None
    };
    let tail = match socks5::reply_tail_len(reply.atyp, domain_len) {
        Some(t) => t,
        None => {
            io.raw_close(fd);
            return Err(std::io::Error::other("响应 ATYP 未知"));
        }
    };
    let mut tail_buf = vec![0u8; tail];
    if let Err(e) = read_exact(io, fd, &mut tail_buf, timeouts) {
        io.raw_close(fd);
        return Err(e);
    }

    // 解析 relay 地址（core 回 IPv4 127.0.0.1）
    let relay = match reply.atyp {
        consts::ATYP_IPV4 => SocketAddr::from((
            std::net::Ipv4Addr::new(tail_buf[0], tail_buf[1], tail_buf[2], tail_buf[3]),
            u16::from_be_bytes([tail_buf[4], tail_buf[5]]),
        )),
        consts::ATYP_IPV6 => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&tail_buf[..16]);
            SocketAddr::from((
                std::net::Ipv6Addr::from(octets),
                u16::from_be_bytes([tail_buf[16], tail_buf[17]]),
            ))
        }
        _ => {
            io.raw_close(fd);
            return Err(std::io::Error::other("core relay 域名形式不支持"));
        }
    };
    Ok((fd, relay))
}

/// 在原 fd 上连接本地核心并完成 SOCKS5 协商。
fn dial_core_in_place(io: &dyn SockIo, fd: i64, host: &str, port: u16) -> std::io::Result<()> {
    let cfg = config();
    let core = io.core_addr().ok_or_else(|| {
        std::io::Error::other(format!("核心地址解析失败: {}", cfg.core_endpoint()))
    })?;

    // 序列化 core sockaddr（平台字节序由平台层处理，这里用 std 生成标准字节）
    let (addr_bytes, addr_len) = encode_sockaddr(&core);

    // 1) 原 socket 连到核心
    // SAFETY: addr_bytes 是有效的 sockaddr 内存，长度由 addr_len 给出
    let rc = unsafe { io.raw_connect(fd, addr_bytes.as_ptr(), addr_len) };
    if rc != 0 {
        return Err(std::io::Error::other("连接本地核心失败"));
    }

    let auth = ProxyAuth::default();
    let timeouts = Timeouts::default();

    // 2) 方法协商
    let greeting = socks5::build_greeting(&auth);
    write_all(io, fd, &greeting, timeouts)?;
    let mut method_resp = [0u8; 2];
    read_exact(io, fd, &mut method_resp, timeouts)?;
    let method = socks5::parse_method_response(&method_resp)
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    // 3) 账密子协商（默认无认证；配置了才走）
    if method == socks5::MethodChoice::UserPass {
        let req = socks5::build_userpass_request(&auth)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        write_all(io, fd, &req, timeouts)?;
        let mut up_resp = [0u8; 2];
        read_exact(io, fd, &mut up_resp, timeouts)?;
        socks5::parse_userpass_response(&up_resp)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
    }

    // 4) CONNECT 请求
    let request = socks5::build_connect_request(host, port)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    write_all(io, fd, &request, timeouts)?;

    // 5) 响应头
    let mut header = [0u8; 4];
    read_exact(io, fd, &mut header, timeouts)?;
    let reply =
        socks5::parse_reply_header(&header).map_err(|e| std::io::Error::other(e.to_string()))?;
    if reply.rep != consts::REPLY_SUCCESS {
        return Err(std::io::Error::other(format!(
            "核心拒绝 CONNECT (REP={}, {})",
            reply.rep,
            socks5::reply_to_text(reply.rep)
        )));
    }

    // 6) 消费 BND.ADDR/PORT
    let domain_len = if reply.atyp == consts::ATYP_DOMAIN {
        let mut len_byte = [0u8; 1];
        read_exact(io, fd, &mut len_byte, timeouts)?;
        Some(len_byte[0])
    } else {
        None
    };
    let tail = socks5::reply_tail_len(reply.atyp, domain_len)
        .ok_or_else(|| std::io::Error::other("响应 ATYP 未知"))?;
    let mut tail_buf = vec![0u8; tail];
    read_exact(io, fd, &mut tail_buf, timeouts)?;
    Ok(())
}

/// 序列化 SocketAddr 为标准 sockaddr 字节（AF_INET / AF_INET6，网络字节序）。
///
/// 返回 (字节, 长度)。macOS 与 Windows 的 sockaddr 内存布局一致
/// （sa_family 按宿主字节序；sin_port/sin6_addr 网络字节序）。
/// AF_INET=2 / AF_INET6=30 为各平台稳定的 ABI 常量。
pub fn encode_sockaddr(addr: &SocketAddr) -> (Vec<u8>, u32) {
    const AF_INET: u16 = 2;
    const AF_INET6: u16 = 30;
    match addr {
        SocketAddr::V4(v4) => {
            let port = v4.port().to_be_bytes();
            let ip = v4.ip().octets();
            let mut bytes = Vec::with_capacity(16);
            bytes.extend_from_slice(&AF_INET.to_le_bytes()); // sa_family 宿主字节序
            bytes.extend_from_slice(&port); // sin_port 网络字节序
            bytes.extend_from_slice(&ip);
            bytes.resize(16, 0); // sin_zero 填充
            (bytes, 16)
        }
        SocketAddr::V6(v6) => {
            let port = v6.port().to_be_bytes();
            let ip = v6.ip().octets();
            let mut bytes = Vec::with_capacity(28);
            bytes.extend_from_slice(&AF_INET6.to_le_bytes());
            bytes.extend_from_slice(&port);
            bytes.extend_from_slice(&[0u8; 4]); // sin6_flowinfo
            bytes.extend_from_slice(&ip); // sin6_addr
            bytes.extend_from_slice(&v6.scope_id().to_ne_bytes()); // sin6_scope_id
            (bytes, 28)
        }
    }
}

// ============= fd IO 辅助 =============

fn write_all(io: &dyn SockIo, fd: i64, buf: &[u8], timeouts: Timeouts) -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + timeouts.send_timeout();
    let mut written = 0usize;
    while written < buf.len() {
        let remaining = (deadline - std::time::Instant::now()).max(std::time::Duration::ZERO);
        if remaining.is_zero() {
            return Err(std::io::Error::other("发送超时"));
        }
        // SAFETY: buf+written 可读，长度为剩余字节
        let n = unsafe { io.raw_write(fd, buf[written..].as_ptr(), buf.len() - written) };
        if n <= 0 {
            return Err(std::io::Error::other("发送失败"));
        }
        written += n as usize;
    }
    Ok(())
}

fn read_exact(io: &dyn SockIo, fd: i64, buf: &mut [u8], timeouts: Timeouts) -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + timeouts.recv_timeout();
    let mut read = 0usize;
    while read < buf.len() {
        let remaining = (deadline - std::time::Instant::now()).max(std::time::Duration::ZERO);
        if remaining.is_zero() {
            return Err(std::io::Error::other("读取超时"));
        }
        // SAFETY: buf+read 可写，长度为剩余字节
        let n = unsafe { io.raw_read(fd, buf[read..].as_mut_ptr(), buf.len() - read) };
        if n <= 0 {
            return Err(std::io::Error::other("读取失败或连接关闭"));
        }
        read += n as usize;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用 SockIo：内存回环（connect 后 echo 一个 mock SOCKS5 协商）。
    struct MockIo {
        core: SocketAddr,
        /// 预置的响应流（协商字节）
        script: std::sync::Mutex<Vec<u8>>,
        /// 记录收到的请求
        received: std::sync::Mutex<Vec<u8>>,
        connected_to: std::sync::Mutex<Vec<SocketAddr>>,
    }

    impl SockIo for MockIo {
        unsafe fn raw_connect(&self, _fd: i64, _addr: *const u8, _len: u32) -> i32 {
            self.connected_to.lock().unwrap().push(self.core);
            0
        }
        unsafe fn raw_read(&self, _fd: i64, buf: *mut u8, len: usize) -> isize {
            let mut script = self.script.lock().unwrap();
            if script.is_empty() {
                return 0;
            }
            let n = len.min(script.len());
            // SAFETY: 测试缓冲区有效
            unsafe {
                std::ptr::copy_nonoverlapping(script.as_ptr(), buf, n);
            }
            script.drain(..n);
            n as isize
        }
        unsafe fn raw_write(&self, _fd: i64, buf: *const u8, len: usize) -> isize {
            // SAFETY: 测试缓冲区有效
            let slice = unsafe { std::slice::from_raw_parts(buf, len) };
            self.received.lock().unwrap().extend_from_slice(slice);
            len as isize
        }
        fn core_addr(&self) -> Option<SocketAddr> {
            Some(self.core)
        }

        fn raw_socket(&self) -> i64 {
            42 // mock：任意正 fd
        }

        fn raw_close(&self, _fd: i64) {}
    }

    #[test]
    fn private_ipv4_ranges() {
        assert!(is_private_ipv4(Ipv4Addr::new(10, 1, 2, 3)));
        assert!(is_private_ipv4(Ipv4Addr::new(172, 16, 0, 1)));
        assert!(is_private_ipv4(Ipv4Addr::new(172, 31, 255, 255)));
        assert!(is_private_ipv4(Ipv4Addr::new(192, 168, 0, 1)));
        assert!(is_private_ipv4(Ipv4Addr::new(169, 254, 1, 1)));
        assert!(is_private_ipv4(Ipv4Addr::new(127, 0, 0, 1)));
        assert!(!is_private_ipv4(Ipv4Addr::new(172, 32, 0, 1)));
        assert!(!is_private_ipv4(Ipv4Addr::new(8, 8, 8, 8)));
    }

    #[test]
    fn private_ipv6_ranges() {
        assert!(is_private_ipv6("fd00::1".parse().unwrap()));
        assert!(is_private_ipv6("fe80::1".parse().unwrap()));
        assert!(is_private_ipv6("::1".parse().unwrap()));
        assert!(!is_private_ipv6("2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn should_bypass_cases() {
        assert!(should_bypass(&"127.0.0.1:80".parse().unwrap()));
        assert!(should_bypass(&"192.168.1.5:443".parse().unwrap()));
        assert!(should_bypass(&"10.0.0.1:22".parse().unwrap()));
        assert!(!should_bypass(&"8.8.8.8:443".parse().unwrap()));
        assert!(!should_bypass(&"1.1.1.1:53".parse().unwrap()));
    }

    #[test]
    fn should_fakeip_excludes_local() {
        assert!(should_fakeip("example.com"));
        assert!(!should_fakeip("localhost"));
        assert!(!should_fakeip("printer.local"));
        assert!(!should_fakeip("127.0.0.1"));
    }

    #[test]
    fn service_port_parsing() {
        assert_eq!(parse_service_port(Some("443")), 443);
        assert_eq!(parse_service_port(Some("http")), 80);
        assert_eq!(parse_service_port(Some("https")), 443);
        assert_eq!(parse_service_port(Some("ssh")), 22);
        assert_eq!(parse_service_port(Some("weird")), 0);
        assert_eq!(parse_service_port(None), 0);
    }

    #[test]
    fn encode_sockaddr_ipv4_shape() {
        let addr: SocketAddr = "1.2.3.4:443".parse().unwrap();
        let (bytes, len) = encode_sockaddr(&addr);
        assert_eq!(len, 16);
        assert_eq!(bytes.len(), 16);
        // family（小端 u16）= AF_INET(2)
        assert_eq!(u16::from_le_bytes([bytes[0], bytes[1]]), 2);
        // port 网络字节序
        assert_eq!(&bytes[2..4], &443u16.to_be_bytes());
        // ip
        assert_eq!(&bytes[4..8], &[1, 2, 3, 4]);
    }

    #[test]
    fn encode_sockaddr_ipv6_shape() {
        let addr: SocketAddr = "[::1]:80".parse().unwrap();
        let (bytes, len) = encode_sockaddr(&addr);
        assert_eq!(len, 28);
        assert_eq!(u16::from_le_bytes([bytes[0], bytes[1]]), 30); // AF_INET6
        assert_eq!(&bytes[2..4], &80u16.to_be_bytes());
        assert_eq!(
            &bytes[8..24],
            &[0u8; 16][..16]
                .iter()
                .enumerate()
                .map(|(i, _)| if i == 15 { 1 } else { 0 })
                .collect::<Vec<u8>>()[..]
        );
    }

    /// 完整就地协商（mock IO）：域名经 ATYP_DOMAIN 透传。
    #[test]
    fn proxy_connect_in_place_full_handshake() {
        install(&ShimConfig::default());
        let core: SocketAddr = "127.0.0.1:1080".parse().unwrap();
        let io = MockIo {
            core,
            // 预置响应：方法选择(05 00) + CONNECT 成功(05 00 00 01 + BND 4 + port 2)
            script: std::sync::Mutex::new(vec![
                0x05, 0x00, // 方法协商：无认证
                0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0, // CONNECT 成功
            ]),
            received: std::sync::Mutex::new(Vec::new()),
            connected_to: std::sync::Mutex::new(Vec::new()),
        };
        let fake_ip = fakeip_lock().assign("example.com").unwrap();
        let target = SocketAddr::from((Ipv4Addr::from(fake_ip), 443));
        let ok = proxy_connect(&io, 42, &target).unwrap();
        assert!(ok);

        // 连到了核心
        assert_eq!(*io.connected_to.lock().unwrap(), vec![core]);

        // 请求内容：greeting(3) + CONNECT 请求
        // CONNECT: [0]=VER [1]=CMD [2]=RSV [3]=ATYP [4]=域名长度 [5..16]=域名 [16..18]=端口
        let received = io.received.lock().unwrap();
        assert_eq!(&received[..3], &[0x05, 0x01, 0x00]); // greeting
        assert_eq!(received[3], 0x05); // CONNECT VER
        assert_eq!(received[4], 0x01); // CMD
        assert_eq!(received[5], 0x00); // RSV
        assert_eq!(received[6], 0x03); // ATYP_DOMAIN
        assert_eq!(received[7], 11); // "example.com" 长度
        assert_eq!(&received[8..19], b"example.com");
        assert_eq!(&received[19..21], &443u16.to_be_bytes());
    }

    /// 私网目标直连放行。
    #[test]
    fn proxy_connect_bypasses_private() {
        install(&ShimConfig::default());
        let io = MockIo {
            core: "127.0.0.1:1080".parse().unwrap(),
            script: std::sync::Mutex::new(Vec::new()),
            received: std::sync::Mutex::new(Vec::new()),
            connected_to: std::sync::Mutex::new(Vec::new()),
        };
        let ok = proxy_connect(&io, 7, &"192.168.1.5:443".parse().unwrap()).unwrap();
        assert!(!ok, "私网目标应直连");
        assert!(io.connected_to.lock().unwrap().is_empty());
    }

    // ---- UDP 策略（P0.1） ----

    #[test]
    fn udp_public_blocked_by_default() {
        install(&ShimConfig::default());
        // 默认 udp_mode=block：公网 UDP 阻断（WebRTC STUN/QUIC/DNS 外泄面）
        let outcome = proxy_udp(&"8.8.8.8:53".parse().unwrap());
        assert!(
            matches!(outcome, UdpOutcome::Blocked(ref e) if e.raw_os_error() == Some(ERR_NET_UNREACH)),
            "公网 UDP 默认应阻断: {outcome:?}"
        );
        let outcome = proxy_udp(&"1.1.1.1:443".parse().unwrap());
        assert!(matches!(outcome, UdpOutcome::Blocked(_)));
    }

    #[test]
    fn udp_local_direct() {
        install(&ShimConfig::default());
        // 回环/私网/mDNS 语义地址：直连放行（不误伤正常应用）
        for addr in [
            "127.0.0.1:53",
            "192.168.1.5:5353",
            "10.0.0.1:9999",
            "172.16.0.9:3478",
        ] {
            let outcome = proxy_udp(&addr.parse().unwrap());
            assert!(
                matches!(outcome, UdpOutcome::Direct),
                "{addr} 应直连放行: {outcome:?}"
            );
        }
    }

    // ---- IPv6 策略（P0.2） ----

    #[test]
    fn ipv6_policy_default_proxy() {
        install(&ShimConfig::default());
        // 默认 ipv6_mode=proxy：公网 v6 走代理
        assert!(check_ipv6_policy(&"[2001:db8::1]:443".parse().unwrap()).unwrap());
        // 内网/回环 v6 直连
        assert!(!check_ipv6_policy(&"[fd00::1]:443".parse().unwrap()).unwrap());
        assert!(!check_ipv6_policy(&"[::1]:443".parse().unwrap()).unwrap());
        assert!(!check_ipv6_policy(&"[fe80::1]:443".parse().unwrap()).unwrap());
        // v4 不受 v6 策略影响
        assert!(check_ipv6_policy(&"8.8.8.8:443".parse().unwrap()).unwrap());
    }

    #[test]
    fn errno_values() {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            assert_eq!(ERR_NET_UNREACH, 51);
            assert_eq!(ERR_AF_NOT_SUPPORTED, 97);
        }
        assert_eq!(udp_blocked_error().raw_os_error(), Some(ERR_NET_UNREACH));
        assert_eq!(
            ipv6_blocked_error().raw_os_error(),
            Some(ERR_AF_NOT_SUPPORTED)
        );
    }
}
