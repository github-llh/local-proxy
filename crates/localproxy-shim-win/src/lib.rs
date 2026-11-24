//! LocalProxy Windows 注入 shim。
//!
//! 经远程线程 `LoadLibrary` 注入目标进程，使用 vendored MinHook
//! hook ws2_32 网络函数：
//!
//! - `connect` / `WSAConnect`：TCP 连接 → 经本地核心 SOCKS5 就地协商
//! - `getaddrinfo` / `GetAddrInfoW`：域名解析 → 返回 FakeIP（198.18.x.x）
//!
//! 初始化分两步（规避 loader lock）：
//! 1. 注入方 `LoadLibrary` 加载本 DLL（DllMain 只做最少的标记）；
//! 2. 注入方再远程调用导出函数 `localproxy_shim_init` 完成 hook 安装。
//!
//! 注意：目标进程若是 PPL（受保护进程）或安全软件拦截注入会失败，
//! launcher 会给出明确错误。

// 整个 crate 仅在 Windows target 编译（macOS 上 `cargo check --workspace`
// 会构建本 crate 的 cdylib，需剔除全部 Windows 专属源码）。
#![cfg(windows)]
#![allow(clippy::missing_safety_doc)]
#![allow(non_camel_case_types)]

use std::ffi::c_void;
use std::net::{Ipv4Addr, SocketAddr};
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, Ordering};

use localproxy_shim::config::ShimConfig;
use localproxy_shim::engine;
use windows_sys::s;
use windows_sys::Win32::Foundation::{BOOL, FALSE, HANDLE, TRUE};
use windows_sys::Win32::Networking::WinSock::{
    connect as sys_connect, getaddrinfo as sys_getaddrinfo, recv as sys_recv, send as sys_send,
    GetAddrInfoW as sys_GetAddrInfoW, WSAGetLastError, WSAStartup, ADDRINFOW, INVALID_SOCKET,
    SEND_RECV_FLAGS, SOCKADDR, SOCKET,
};
use windows_sys::Win32::System::LibraryLoader::GetProcAddress;
use windows_sys::Win32::System::Threading::{
    CreateProcessA as sys_CreateProcessA, CreateProcessW as sys_CreateProcessW,
    GetCurrentProcessId, PROCESS_INFORMATION, STARTUPINFOW,
};

// ============= 原函数 trampoline =============

type ConnectFn = unsafe extern "system" fn(SOCKET, *const SOCKADDR, i32) -> i32;
type GetAddrInfoAFn =
    unsafe extern "system" fn(*const u8, *const u8, *const u8, *mut *mut u8) -> i32;
type GetAddrInfoWFn =
    unsafe extern "system" fn(*const u16, *const u16, *const ADDRINFOW, *mut *mut ADDRINFOW) -> i32;
type RecvFn = unsafe extern "system" fn(SOCKET, *const u8, i32, SEND_RECV_FLAGS) -> i32;
type SendFn = unsafe extern "system" fn(SOCKET, *const u8, i32, SEND_RECV_FLAGS) -> i32;

static mut ORIG_CONNECT: Option<ConnectFn> = None;
static mut ORIG_GETADDRINFO: Option<GetAddrInfoAFn> = None;
static mut ORIG_GETADDRINFOW: Option<GetAddrInfoWFn> = None;
static mut ORIG_RECV: Option<RecvFn> = None;
static mut ORIG_SEND: Option<SendFn> = None;

// ============= 平台 SockIo 实现 =============

/// Windows 平台 IO（ws2_32 connect/recv/send）。
struct WinSockIo;

impl engine::SockIo for WinSockIo {
    unsafe fn raw_connect(&self, fd: i64, addr: *const u8, len: u32) -> i32 {
        // SAFETY: 引擎约定 addr/len 构成有效 SOCKADDR
        unsafe { sys_connect(fd as SOCKET, addr as *const SOCKADDR, len as i32) }
    }

    unsafe fn raw_read(&self, fd: i64, buf: *mut u8, len: usize) -> isize {
        // SAFETY: 引擎约定 buf 可写 len 字节
        let n = unsafe { sys_recv(fd as SOCKET, buf, len as i32, 0) };
        if n == -1 {
            // WSAEWOULDBLOCK 等可重试错误：交给上层超时逻辑处理（返回 -1 即重试）
            let err = unsafe { WSAGetLastError() };
            if err == 10035 {
                // WSAEWOULDBLOCK：让出 CPU 后重试由上层 write/read 循环驱动
                std::thread::sleep(std::time::Duration::from_millis(1));
                return -2;
            }
        }
        n as isize
    }

    unsafe fn raw_write(&self, fd: i64, buf: *const u8, len: usize) -> isize {
        // SAFETY: 引擎约定 buf 可读 len 字节
        let n = unsafe { sys_send(fd as SOCKET, buf, len as i32, 0) };
        if n == -1 {
            let err = unsafe { WSAGetLastError() };
            if err == 10035 {
                std::thread::sleep(std::time::Duration::from_millis(1));
                return -2;
            }
        }
        n as isize
    }

    fn core_addr(&self) -> Option<SocketAddr> {
        // 核心地址必须是 IP 字面量（避免经 getaddrinfo hook 拿到 FakeIP 成环）
        engine::config().core_endpoint().parse().ok()
    }
}

static IO: WinSockIo = WinSockIo;

// ============= 安装状态 =============

static INSTALLED: AtomicBool = AtomicBool::new(false);

// ============= hook 函数 =============

/// 解析 SOCKADDR 为 Rust SocketAddr。
unsafe fn parse_sockaddr(addr: *const SOCKADDR) -> Option<SocketAddr> {
    if addr.is_null() {
        return None;
    }
    // SAFETY: 调用方保证 addr 有效
    let family = unsafe { (*addr).sa_family } as i32;
    if family == 2 {
        // AF_INET
        // SAFETY: family 已判定
        let sin =
            unsafe { &*(addr as *const windows_sys::Win32::Networking::WinSock::SOCKADDR_IN) };
        let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.S_un.S_addr as u32));
        let port = u16::from_be(sin.sin_port);
        Some(SocketAddr::from((ip, port)))
    } else if family == 23 {
        // AF_INET6
        // SAFETY: family 已判定
        let sin6 =
            unsafe { &*(addr as *const windows_sys::Win32::Networking::WinSock::SOCKADDR_IN6) };
        let ip = std::net::Ipv6Addr::from(unsafe { sin6.sin6_addr.u.Byte });
        let port = u16::from_be(sin6.sin6_port);
        Some(SocketAddr::from((ip, port)))
    } else {
        None
    }
}

/// `connect` 替换实现。
unsafe extern "system" fn hook_connect(fd: SOCKET, addr: *const SOCKADDR, len: i32) -> i32 {
    ensure_installed();
    let Some(_guard) = engine::HookGuard::enter() else {
        return call_orig_connect(fd, addr, len);
    };
    // SAFETY: 调用方保证 addr/len 有效
    let Some(target) = (unsafe { parse_sockaddr(addr) }) else {
        return call_orig_connect(fd, addr, len);
    };
    match engine::proxy_connect(&IO, fd as i64, &target) {
        Ok(true) => 0,
        Ok(false) => call_orig_connect(fd, addr, len),
        Err(_) => {
            unsafe { windows_sys::Win32::Networking::WinSock::WSASetLastError(10061) }; // WSAECONNREFUSED
            -1
        }
    }
}

unsafe fn call_orig_connect(fd: SOCKET, addr: *const SOCKADDR, len: i32) -> i32 {
    // SAFETY: install 成功时 ORIG_CONNECT 已填充
    match unsafe { ORIG_CONNECT } {
        Some(f) => unsafe { f(fd, addr, len) },
        None => unsafe { sys_connect(fd, addr, len) },
    }
}

/// `getaddrinfo`（ANSI）替换实现：非本地域名返回 FakeIP。
unsafe extern "system" fn hook_getaddrinfo(
    node: *const u8,
    service: *const u8,
    hints: *const u8,
    res: *mut *mut u8,
) -> i32 {
    ensure_installed();
    let Some(_guard) = engine::HookGuard::enter() else {
        return call_orig_getaddrinfo(node, service, hints, res);
    };
    if node.is_null() || res.is_null() {
        return call_orig_getaddrinfo(node, service, hints, res);
    }
    // SAFETY: node 为 C 字符串
    let Ok(host) = (unsafe { std::ffi::CStr::from_ptr(node as *const std::ffi::c_char) }).to_str()
    else {
        return call_orig_getaddrinfo(node, service, hints, res);
    };
    if !engine::should_fakeip(host) {
        return call_orig_getaddrinfo(node, service, hints, res);
    }
    // SAFETY: hints 有效
    if !hints.is_null() {
        let h = unsafe { &*(hints as *const AddrinfoACompat) };
        if h.ai_flags & 0x4 != 0 || h.ai_flags & 0x1 != 0 {
            // AI_NUMERICHOST / AI_PASSIVE 不代理
            return call_orig_getaddrinfo(node, service, hints, res);
        }
    }
    // SAFETY: service 为 C 字符串或 NULL
    let service_str = if service.is_null() {
        None
    } else {
        unsafe { std::ffi::CStr::from_ptr(service as *const std::ffi::c_char) }
            .to_str()
            .ok()
    };
    let port = engine::parse_service_port(service_str);
    let Some(ip) = engine::fakeip_lock().assign(host) else {
        return call_orig_getaddrinfo(node, service, hints, res);
    };
    let fake_ip = Ipv4Addr::from(ip);
    // SAFETY: res 非空
    match unsafe { build_fake_addrinfo_ansi(fake_ip, port) } {
        Some(ai) => {
            unsafe { *res = ai as *mut u8 };
            0
        }
        None => 8, // EAI_MEMORY 近似（Windows: EAI_MEMORY=8? 实际 11001 WSANO_DATA）
    }
}

unsafe fn call_orig_getaddrinfo(
    node: *const u8,
    service: *const u8,
    hints: *const u8,
    res: *mut *mut u8,
) -> i32 {
    // SAFETY: install 后 ORIG_GETADDRINFO 已填充
    match unsafe { ORIG_GETADDRINFO } {
        Some(f) => unsafe { f(node, service, hints, res) },
        None => unsafe { sys_getaddrinfo(node, service, hints, res) },
    }
}

/// `GetAddrInfoW`（宽字符）替换实现。
unsafe extern "system" fn hook_getaddrinfo_w(
    node: *const u16,
    service: *const u16,
    hints: *const ADDRINFOW,
    res: *mut *mut ADDRINFOW,
) -> i32 {
    ensure_installed();
    let Some(_guard) = engine::HookGuard::enter() else {
        return call_orig_getaddrinfo_w(node, service, hints, res);
    };
    if node.is_null() || res.is_null() {
        return call_orig_getaddrinfo_w(node, service, hints, res);
    }
    // SAFETY: node 为宽字符串
    let Ok(host) = (unsafe { wide_to_string(node) }) else {
        return call_orig_getaddrinfo_w(node, service, hints, res);
    };
    if !engine::should_fakeip(&host) {
        return call_orig_getaddrinfo_w(node, service, hints, res);
    }
    // SAFETY: hints 有效
    if !hints.is_null() {
        let h = unsafe { &*hints };
        if h.ai_flags & 0x4 != 0 || h.ai_flags & 0x1 != 0 {
            return call_orig_getaddrinfo_w(node, service, hints, res);
        }
    }
    // SAFETY: service 为宽字符串或 NULL
    let service_str = if service.is_null() {
        None
    } else {
        wide_to_string(service).ok()
    };
    let port = engine::parse_service_port(service_str.as_deref());
    let Some(ip) = engine::fakeip_lock().assign(&host) else {
        return call_orig_getaddrinfo_w(node, service, hints, res);
    };
    let fake_ip = Ipv4Addr::from(ip);
    // SAFETY: res 非空
    match unsafe { build_fake_addrinfo_w(fake_ip, port) } {
        Some(ai) => {
            unsafe { *res = ai };
            0
        }
        None => 11001, // WSANO_DATA 近似（内存不足）
    }
}

unsafe fn call_orig_getaddrinfo_w(
    node: *const u16,
    service: *const u16,
    hints: *const ADDRINFOW,
    res: *mut *mut ADDRINFOW,
) -> i32 {
    // SAFETY: install 后 ORIG_GETADDRINFOW 已填充
    match unsafe { ORIG_GETADDRINFOW } {
        Some(f) => unsafe { f(node, service, hints, res) },
        None => unsafe { sys_GetAddrInfoW(node, service, hints, res) },
    }
}

// ============= 辅助 =============

/// ANSI addrinfo 兼容布局（Windows `addrinfo` 内存布局稳定）。
#[repr(C)]
struct AddrinfoACompat {
    ai_flags: i32,
    ai_family: i32,
    ai_socktype: i32,
    ai_protocol: i32,
    ai_addrlen: usize,
    ai_canonname: *mut u8,
    ai_addr: *mut u8,
    ai_next: *mut u8,
}

/// 构造 ANSI FakeIP addrinfo（单节点，可被 freeaddrinfo 释放）。
unsafe fn build_fake_addrinfo_ansi(ip: Ipv4Addr, port: u16) -> Option<*mut AddrinfoACompat> {
    let ai = unsafe { std::alloc::alloc_zeroed(std::alloc::Layout::new::<AddrinfoACompat>()) }
        as *mut AddrinfoACompat;
    if ai.is_null() {
        return None;
    }
    // sockaddr_in { family(2) port(2) addr(4) zero(8) } = 16 字节
    let mut sa = [0u8; 16];
    sa[0] = 2; // AF_INET (little-endian u16)
    sa[1] = 0;
    sa[2..4].copy_from_slice(&port.to_be_bytes());
    sa[4..8].copy_from_slice(&ip.octets());
    let sa_ptr = unsafe { std::alloc::alloc(std::alloc::Layout::from_size_align(16, 4).ok()?) };
    if sa_ptr.is_null() {
        unsafe { std::alloc::dealloc(ai as *mut u8, std::alloc::Layout::new::<AddrinfoACompat>()) };
        return None;
    }
    unsafe { std::ptr::copy_nonoverlapping(sa.as_ptr(), sa_ptr, 16) };
    unsafe {
        (*ai).ai_family = 2;
        (*ai).ai_socktype = 1; // SOCK_STREAM
        (*ai).ai_protocol = 6; // IPPROTO_TCP
        (*ai).ai_addrlen = 16;
        (*ai).ai_addr = sa_ptr;
    }
    Some(ai)
}

/// 构造宽字符 FakeIP addrinfo（ADDRINFOW，可被 FreeAddrInfoW 释放）。
unsafe fn build_fake_addrinfo_w(ip: Ipv4Addr, port: u16) -> Option<*mut ADDRINFOW> {
    let ai = unsafe {
        std::alloc::alloc_zeroed(std::alloc::Layout::new::<ADDRINFOW>()) as *mut ADDRINFOW
    };
    if ai.is_null() {
        return None;
    }
    let sa = unsafe {
        std::alloc::alloc_zeroed(
            std::alloc::Layout::from_size_align(
                std::mem::size_of::<windows_sys::Win32::Networking::WinSock::SOCKADDR_IN>(),
                4,
            )
            .ok()?,
        ) as *mut windows_sys::Win32::Networking::WinSock::SOCKADDR_IN
    };
    if sa.is_null() {
        unsafe { std::alloc::dealloc(ai as *mut u8, std::alloc::Layout::new::<ADDRINFOW>()) };
        return None;
    }
    unsafe {
        (*sa).sin_family = 2;
        (*sa).sin_port = port.to_be();
        (*sa).sin_addr.S_un.S_addr = u32::from(ip).to_be();
        (*ai).ai_family = 2;
        (*ai).ai_socktype = 1;
        (*ai).ai_protocol = 6;
        (*ai).ai_addrlen =
            std::mem::size_of::<windows_sys::Win32::Networking::WinSock::SOCKADDR_IN>() as usize;
        (*ai).ai_addr = sa as *mut SOCKADDR;
    }
    Some(ai)
}

/// 宽字符串转 String（至多 NUL）。
unsafe fn wide_to_string(ptr: *const u16) -> Result<String, std::string::FromUtf16Error> {
    let mut len = 0usize;
    // SAFETY: ptr 为宽字符串
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: len 个 u16 可读
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf16(slice)
}

// ============= 安装 =============

/// 确保引擎已安装（幂等；由 hook 首次调用或显式 init 触发）。
fn ensure_installed() {
    if INSTALLED.load(Ordering::SeqCst) {
        return;
    }
    // hook 触发路径：只安装一次；安装动作委托给 init（此处不重复）
    let cfg = ShimConfig::from_env();
    let _ = engine::install(&cfg);
    INSTALLED.store(true, Ordering::SeqCst);
}

/// 本进程已成功注入的子进程数（防注入风暴上限）。
static CHILD_INJECT_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// 子进程注入策略（环境变量）。
struct ChildPolicy {
    /// `all`（默认）：全部子进程；`same-dir`：仅同目录二进制
    mode: String,
    /// 排除列表（分号分隔，子串匹配可执行路径）
    exclude: Vec<String>,
    /// 注入数量上限
    max: u32,
}

impl ChildPolicy {
    fn from_env() -> Self {
        let mode = std::env::var("LP_CHILD_MODE").unwrap_or_else(|_| "all".to_string());
        let exclude = std::env::var("LP_CHILD_EXCLUDE")
            .map(|s| {
                s.split(';')
                    .map(|x| x.trim().to_ascii_lowercase())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let max = std::env::var("LP_CHILD_MAX")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(64);
        Self { mode, exclude, max }
    }

    /// 目标命令行是否应注入。
    fn should_inject(&self, cmd_line_lower: &str) -> bool {
        if self.exclude.iter().any(|e| cmd_line_lower.contains(e)) {
            return false;
        }
        match self.mode.as_str() {
            "same-dir" => {
                // 与 shim DLL 同目录（同 bundle 的 Electron 多进程场景）
                match self_shim_dir() {
                    Some(dir) => {
                        // 命令行中任一路径片段位于同目录
                        cmd_line_lower.contains(&dir.to_ascii_lowercase())
                    }
                    None => true,
                }
            }
            _ => true, // all
        }
    }

    fn count_ok(&self) -> bool {
        CHILD_INJECT_COUNT.load(Ordering::Relaxed) < self.max
    }
}

/// shim 自身 DLL 所在目录（小写宽字符串）。
fn self_shim_dir() -> Option<String> {
    let mut buf = vec![0u16; 260];
    let len = unsafe {
        windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW(
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            buf.len() as u32,
        )
    };
    if len == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    std::path::Path::new(&path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
}

/// 读取宽字符命令行（至 NUL）。
unsafe fn read_wide(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    // SAFETY: ptr 为宽字符串
    while unsafe { *ptr.add(len) } != 0 && len < 4096 {
        len += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// 向新创建的子进程注入 shim（失败仅记录，不影响父进程行为）。
fn inject_child(pi: &PROCESS_INFORMATION) {
    let policy = ChildPolicy::from_env();
    if !policy.count_ok() {
        tracing::debug!("shim-win: 子进程注入达上限 {}，跳过", policy.max);
        return;
    }
    if pi.dwProcessId == unsafe { GetCurrentProcessId() } {
        return; // 不会发生，防御
    }
    let Some(dll_path) = self_shim_path() else {
        return;
    };
    if let Err(e) = localproxy_inject::inject_dll(pi.hProcess, &dll_path) {
        tracing::debug!("shim-win: 子进程注入失败 pid={}: {e}", pi.dwProcessId);
        return;
    }
    if let Err(e) =
        localproxy_inject::call_remote_export(pi.hProcess, &dll_path, "localproxy_shim_init")
    {
        tracing::debug!("shim-win: 子进程 init 失败 pid={}: {e}", pi.dwProcessId);
        return;
    }
    CHILD_INJECT_COUNT.fetch_add(1, Ordering::Relaxed);
    tracing::info!(
        "shim-win: 子进程注入成功 pid={}（累计 {}）",
        pi.dwProcessId,
        CHILD_INJECT_COUNT.load(Ordering::Relaxed)
    );
}

/// shim 自身 DLL 完整路径。
fn self_shim_path() -> Option<std::path::PathBuf> {
    let mut buf = vec![0u16; 260];
    let len = unsafe {
        windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW(
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            buf.len() as u32,
        )
    };
    if len == 0 {
        return None;
    }
    Some(std::path::PathBuf::from(String::from_utf16_lossy(
        &buf[..len as usize],
    )))
}

/// DLL 加载完成后的初始化入口（注入方二次远程调用）。
///
/// 返回 true 表示安装成功。
#[no_mangle]
pub unsafe extern "system" fn localproxy_shim_init() -> bool {
    // WSAStartup（目标进程可能尚未初始化 winsock；MinHook 安装也需要）
    let mut wsa_data =
        unsafe { std::mem::zeroed::<windows_sys::Win32::Networking::WinSock::WSADATA>() };
    unsafe { WSAStartup(0x0202, &mut wsa_data) };

    let cfg = ShimConfig::from_env();
    if !engine::install(&cfg) {
        // 已安装过也视为成功
        return engine::is_installed();
    }
    INSTALLED.store(true, Ordering::SeqCst);

    // MinHook 安装（ws2_32 内联 hook）
    let ok = unsafe { install_hooks() };
    tracing::info!("shim-win: hook 安装 {}", if ok { "成功" } else { "失败" });
    ok
}

/// MinHook 安装全部 hook。
unsafe fn install_hooks() -> bool {
    use minhook::MinHook;

    let ws2 = match unsafe {
        windows_sys::Win32::System::LibraryLoader::GetModuleHandleA(s!("ws2_32.dll"))
    } {
        h if !h.is_null() => h,
        _ => return false,
    };

    let connect_addr = unsafe { GetProcAddress(ws2, s!("connect")) } as *const c_void;
    let getaddrinfo_addr = unsafe { GetProcAddress(ws2, s!("getaddrinfo")) } as *const c_void;
    let getaddrinfow_addr = unsafe { GetProcAddress(ws2, s!("GetAddrInfoW")) } as *const c_void;

    macro_rules! hook {
        ($addr:expr, $hook:expr, $orig:ident) => {{
            let addr = $addr;
            if addr.is_null() {
                return false;
            }
            let orig = match unsafe { MinHook::create_hook(addr, $hook as *mut c_void) } {
                Ok(trampoline) => trampoline,
                Err(e) => {
                    tracing::error!("shim-win: MH_CreateHook 失败: {:?}", e);
                    return false;
                }
            };
            unsafe { $orig = Some(std::mem::transmute(orig)) };
        }};
    }

    hook!(connect_addr, hook_connect as *mut c_void, ORIG_CONNECT);
    hook!(
        getaddrinfo_addr,
        hook_getaddrinfo as *mut c_void,
        ORIG_GETADDRINFO
    );
    hook!(
        getaddrinfow_addr,
        hook_getaddrinfo_w as *mut c_void,
        ORIG_GETADDRINFOW
    );

    // kernel32: CreateProcessW/A（子进程注入）
    let kernel32 = match unsafe {
        windows_sys::Win32::System::LibraryLoader::GetModuleHandleA(s!("kernel32.dll"))
    } {
        h if !h.is_null() => h,
        _ => return false,
    };
    let cpw_addr = unsafe { GetProcAddress(kernel32, s!("CreateProcessW")) } as *const c_void;
    let cpa_addr = unsafe { GetProcAddress(kernel32, s!("CreateProcessA")) } as *const c_void;
    hook!(
        cpw_addr,
        hook_CreateProcessW as *mut c_void,
        ORIG_CREATE_PROCESS_W
    );
    hook!(
        cpa_addr,
        hook_CreateProcessA as *mut c_void,
        ORIG_CREATE_PROCESS_A
    );

    match unsafe { MinHook::enable_all_hooks() } {
        Ok(()) => true,
        Err(e) => {
            tracing::error!("shim-win: MH_EnableHook 失败: {:?}", e);
            false
        }
    }
}

// ============= hook: CreateProcess（子进程注入 P0.4） =============
//
// Chromium/Electron 的网络栈在独立子进程（network_service/utility/renderer）中，
// macOS 靠 DYLD_INSERT_LIBRARIES 天然继承，Windows 必须 hook CreateProcess
// 主动注入，否则这些子进程的网络全部直连泄漏。

const CREATE_SUSPENDED_FLAG: u32 = 0x0000_0004;

type CreateProcessWFn = unsafe extern "system" fn(
    *const u16,
    *mut u16,
    *const std::ffi::c_void,
    *const std::ffi::c_void,
    BOOL,
    u32,
    *const std::ffi::c_void,
    *const u16,
    *const STARTUPINFOW,
    *mut PROCESS_INFORMATION,
) -> BOOL;
type CreateProcessAFn = unsafe extern "system" fn(
    *const u8,
    *mut u8,
    *const std::ffi::c_void,
    *const std::ffi::c_void,
    BOOL,
    u32,
    *const std::ffi::c_void,
    *const u8,
    *const std::ffi::c_void,
    *mut PROCESS_INFORMATION,
) -> BOOL;

static mut ORIG_CREATE_PROCESS_W: Option<CreateProcessWFn> = None;
static mut ORIG_CREATE_PROCESS_A: Option<CreateProcessAFn> = None;

/// `CreateProcessW` 替换实现。
unsafe extern "system" fn hook_CreateProcessW(
    app_name: *const u16,
    cmd_line: *mut u16,
    process_attrs: *const std::ffi::c_void,
    thread_attrs: *const std::ffi::c_void,
    inherit: BOOL,
    flags: u32,
    env: *const std::ffi::c_void,
    cwd: *const u16,
    startup_info: *const STARTUPINFOW,
    process_info: *mut PROCESS_INFORMATION,
) -> BOOL {
    let Some(_guard) = engine::HookGuard::enter() else {
        return call_orig_cpw(
            app_name,
            cmd_line,
            process_attrs,
            thread_attrs,
            inherit,
            flags,
            env,
            cwd,
            startup_info,
            process_info,
        );
    };

    // 强制挂起（调用方未要求时），注入后恢复
    let caller_suspended = flags & CREATE_SUSPENDED_FLAG != 0;
    let ok = call_orig_cpw(
        app_name,
        cmd_line,
        process_attrs,
        thread_attrs,
        inherit,
        flags | CREATE_SUSPENDED_FLAG,
        env,
        cwd,
        startup_info,
        process_info,
    );
    if ok != 0 && !process_info.is_null() {
        // SAFETY: 原函数成功时 process_info 有效
        let pi = unsafe { &*process_info };
        let policy = ChildPolicy::from_env();
        let cmd_lower = unsafe { read_wide(cmd_line) }.to_ascii_lowercase();
        if policy.should_inject(&cmd_lower) {
            inject_child(pi);
        }
        if !caller_suspended {
            unsafe { windows_sys::Win32::System::Threading::ResumeThread(pi.hThread) };
        }
    }
    ok
}

unsafe fn call_orig_cpw(
    app_name: *const u16,
    cmd_line: *mut u16,
    process_attrs: *const std::ffi::c_void,
    thread_attrs: *const std::ffi::c_void,
    inherit: BOOL,
    flags: u32,
    env: *const std::ffi::c_void,
    cwd: *const u16,
    startup_info: *const STARTUPINFOW,
    process_info: *mut PROCESS_INFORMATION,
) -> BOOL {
    // SAFETY: install 后 ORIG_CREATE_PROCESS_W 已填充
    match unsafe { ORIG_CREATE_PROCESS_W } {
        Some(f) => unsafe {
            f(
                app_name,
                cmd_line,
                process_attrs,
                thread_attrs,
                inherit,
                flags,
                env,
                cwd,
                startup_info,
                process_info,
            )
        },
        None => unsafe {
            sys_CreateProcessW(
                app_name,
                cmd_line,
                process_attrs,
                thread_attrs,
                inherit,
                flags,
                env,
                cwd,
                startup_info,
                process_info,
            )
        },
    }
}

/// `CreateProcessA` 替换实现（ANSI；策略与 W 版一致）。
unsafe extern "system" fn hook_CreateProcessA(
    app_name: *const u8,
    cmd_line: *mut u8,
    process_attrs: *const std::ffi::c_void,
    thread_attrs: *const std::ffi::c_void,
    inherit: BOOL,
    flags: u32,
    env: *const std::ffi::c_void,
    cwd: *const u8,
    startup_info: *const std::ffi::c_void,
    process_info: *mut PROCESS_INFORMATION,
) -> BOOL {
    let Some(_guard) = engine::HookGuard::enter() else {
        return call_orig_cpa(
            app_name,
            cmd_line,
            process_attrs,
            thread_attrs,
            inherit,
            flags,
            env,
            cwd,
            startup_info,
            process_info,
        );
    };

    let caller_suspended = flags & CREATE_SUSPENDED_FLAG != 0;
    let ok = call_orig_cpa(
        app_name,
        cmd_line,
        process_attrs,
        thread_attrs,
        inherit,
        flags | CREATE_SUSPENDED_FLAG,
        env,
        cwd,
        startup_info,
        process_info,
    );
    if ok != 0 && !process_info.is_null() {
        // SAFETY: 原函数成功时 process_info 有效
        let pi = unsafe { &*process_info };
        let policy = ChildPolicy::from_env();
        let cmd_lower = unsafe { read_ansi(cmd_line) }.to_ascii_lowercase();
        if policy.should_inject(&cmd_lower) {
            inject_child(pi);
        }
        if !caller_suspended {
            unsafe { windows_sys::Win32::System::Threading::ResumeThread(pi.hThread) };
        }
    }
    ok
}

unsafe fn call_orig_cpa(
    app_name: *const u8,
    cmd_line: *mut u8,
    process_attrs: *const std::ffi::c_void,
    thread_attrs: *const std::ffi::c_void,
    inherit: BOOL,
    flags: u32,
    env: *const std::ffi::c_void,
    cwd: *const u8,
    startup_info: *const std::ffi::c_void,
    process_info: *mut PROCESS_INFORMATION,
) -> BOOL {
    // SAFETY: install 后 ORIG_CREATE_PROCESS_A 已填充
    match unsafe { ORIG_CREATE_PROCESS_A } {
        Some(f) => unsafe {
            f(
                app_name,
                cmd_line,
                process_attrs,
                thread_attrs,
                inherit,
                flags,
                env,
                cwd,
                startup_info,
                process_info,
            )
        },
        None => unsafe {
            sys_CreateProcessA(
                app_name,
                cmd_line,
                process_attrs,
                thread_attrs,
                inherit,
                flags,
                env,
                cwd,
                startup_info,
                process_info,
            )
        },
    }
}

/// 读取宽字符命令行（至 NUL，上限 4096）。
unsafe fn read_wide(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    // SAFETY: ptr 为宽字符串
    while unsafe { *ptr.add(len) } != 0 && len < 4096 {
        len += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// 读取 ANSI 命令行（至 NUL，上限 4096）。
unsafe fn read_ansi(ptr: *const u8) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    // SAFETY: ptr 为 ANSI 字符串
    while unsafe { *ptr.add(len) } != 0 && len < 4096 {
        len += 1;
    }
    String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(ptr, len) }).to_string()
}

// ============= DllMain =============

/// DLL 入口：只做最少标记（hook 安装由注入方二次调用 init 完成，
/// 规避 loader lock 死锁风险）。
#[no_mangle]
#[allow(non_snake_case)]
pub unsafe extern "system" fn DllMain(
    _hinst: *mut c_void,
    reason: u32,
    _reserved: *mut c_void,
) -> BOOL {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if reason == DLL_PROCESS_ATTACH {
        let pid = unsafe { GetCurrentProcessId() };
        tracing::info!("shim-win: 已注入进程 pid={pid}（等待 init 调用安装 hook）");
    }
    TRUE
}

// 抑制未使用告警（INVALID_SOCKET 仅文档用途）
#[allow(dead_code)]
const _INVALID_SOCKET: SOCKET = INVALID_SOCKET;
const _FALSE: BOOL = FALSE;
