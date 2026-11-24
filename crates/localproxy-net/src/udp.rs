//! UDP 代理策略与退避逻辑。
//!
//! 移植自旧项目：
//! - `src/core/UdpPolicy.hpp`：`udp_mode` × `proxy_type` × `udp_fallback` → 有效动作
//! - `src/network/UdpAssociateBackoff.hpp`：UDP ASSOCIATE 失败退避窗口
//!
//! 纯逻辑、不依赖 socket，便于单元测试；时间由调用方传入毫秒。

use std::fmt;

/// UDP 处理动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UdpAction {
    /// 阻断 UDP，强制应用回退 TCP
    Block,
    /// UDP 直连
    Direct,
    /// UDP 走代理（SOCKS5 UDP Associate）
    Proxy,
}

impl fmt::Display for UdpAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl UdpAction {
    pub fn name(self) -> &'static str {
        match self {
            UdpAction::Direct => "direct",
            UdpAction::Proxy => "proxy",
            UdpAction::Block => "block",
        }
    }

    /// 从配置字符串解析（非法值由调用方回退，这里返回 None）。
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "direct" => Some(UdpAction::Direct),
            "proxy" => Some(UdpAction::Proxy),
            "block" => Some(UdpAction::Block),
            _ => None,
        }
    }
}

/// 解析有效 UDP 动作（移植旧 `ResolveUdpAction`）。
///
/// - `udp_mode=direct/proxy` 直接采用
/// - `udp_mode=auto`：SOCKS5 上游用 Proxy；其它上游按 `udp_fallback` 决策
/// - 非法 `udp_mode`：Block（保守，避免静默直连泄露）
pub fn resolve_udp_action(udp_mode: &str, proxy_type: &str, udp_fallback: &str) -> UdpAction {
    match udp_mode.trim().to_ascii_lowercase().as_str() {
        "direct" => UdpAction::Direct,
        "proxy" => UdpAction::Proxy,
        "auto" => {
            if proxy_type.trim().eq_ignore_ascii_case("socks5") {
                UdpAction::Proxy
            } else if udp_fallback.trim().eq_ignore_ascii_case("direct") {
                UdpAction::Direct
            } else {
                UdpAction::Block
            }
        }
        _ => UdpAction::Block,
    }
}

/// UDP ASSOCIATE 失败退避（进程级，移植旧 `UdpAssociateBackoff`）。
///
/// 代理未开启 UDP 时，每个新 UDP socket 都重建 TCP 控制连接再失败，
/// 白白增加握手耗时与日志噪音。失败后在窗口期内直接按 fallback 处理。
#[derive(Debug, Clone)]
pub struct UdpAssociateBackoff {
    window_ms: u64,
    until_ms: u64,
}

impl UdpAssociateBackoff {
    /// 默认窗口 60 秒。
    pub const DEFAULT_WINDOW_MS: u64 = 60_000;

    pub fn new(window_ms: u64) -> Self {
        Self {
            window_ms: if window_ms == 0 {
                Self::DEFAULT_WINDOW_MS
            } else {
                window_ms
            },
            until_ms: 0,
        }
    }

    /// 默认窗口构造。
    pub fn with_default_window() -> Self {
        Self::new(Self::DEFAULT_WINDOW_MS)
    }

    /// 当前是否处于退避期：是则跳过 ASSOCIATE，直接按 fallback 处理。
    pub fn should_skip(&self, now_ms: u64) -> bool {
        now_ms < self.until_ms
    }

    /// 记录一次失败并开启（或延长）退避窗口。
    /// 返回 true 表示这是新窗口的首次失败（调用方据此只输出一次提示）。
    pub fn record_failure(&mut self, now_ms: u64) -> bool {
        let first_in_window = now_ms >= self.until_ms;
        self.until_ms = now_ms.saturating_add(self.window_ms);
        first_in_window
    }

    /// ASSOCIATE 成功：立即结束退避。
    pub fn record_success(&mut self) {
        self.until_ms = 0;
    }
}

impl Default for UdpAssociateBackoff {
    fn default() -> Self {
        Self::with_default_window()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 对照旧 tests/test_udp_policy.cpp 的语义 ----

    #[test]
    fn auto_with_socks5_upstream_uses_proxy() {
        assert_eq!(
            resolve_udp_action("auto", "socks5", "block"),
            UdpAction::Proxy
        );
        assert_eq!(
            resolve_udp_action("auto", "socks5", "direct"),
            UdpAction::Proxy
        );
    }

    #[test]
    fn auto_with_http_upstream_follows_fallback() {
        assert_eq!(
            resolve_udp_action("auto", "http", "block"),
            UdpAction::Block
        );
        assert_eq!(
            resolve_udp_action("auto", "http", "direct"),
            UdpAction::Direct
        );
        // 大小写不敏感
        assert_eq!(
            resolve_udp_action("AUTO", "SOCKS5", "BLOCK"),
            UdpAction::Proxy
        );
    }

    #[test]
    fn explicit_modes_win() {
        assert_eq!(
            resolve_udp_action("direct", "socks5", "block"),
            UdpAction::Direct
        );
        assert_eq!(
            resolve_udp_action("proxy", "http", "direct"),
            UdpAction::Proxy
        );
    }

    #[test]
    fn invalid_mode_falls_back_to_block() {
        assert_eq!(
            resolve_udp_action("bogus", "socks5", "direct"),
            UdpAction::Block
        );
        assert_eq!(resolve_udp_action("", "http", "direct"), UdpAction::Block);
    }

    #[test]
    fn action_names_roundtrip() {
        for action in [UdpAction::Direct, UdpAction::Proxy, UdpAction::Block] {
            assert_eq!(UdpAction::from_str_opt(action.name()), Some(action));
        }
        assert_eq!(UdpAction::from_str_opt("nope"), None);
    }

    // ---- 逐条对照旧 tests/test_udp_associate_backoff.cpp ----

    #[test]
    fn backoff_window_behavior() {
        let mut backoff = UdpAssociateBackoff::new(60000);
        // 初始不退避
        assert!(!backoff.should_skip(1000));
        // 首次失败：开启窗口，并要求输出一次提示
        assert!(backoff.record_failure(1000));
        assert!(backoff.should_skip(1000));
        assert!(backoff.should_skip(60999));
        // 窗口内再次失败（并发 socket 已在途）：不重复提示，但延长窗口
        assert!(!backoff.record_failure(30000));
        assert!(backoff.should_skip(89999));
        // 窗口到期后允许重试；再次失败视为新窗口，重新提示一次
        assert!(!backoff.should_skip(90000));
        assert!(backoff.record_failure(90000));
    }

    #[test]
    fn success_clears_backoff() {
        let mut backoff = UdpAssociateBackoff::new(1000);
        assert!(backoff.record_failure(100));
        assert!(backoff.should_skip(500));
        backoff.record_success();
        assert!(!backoff.should_skip(500));
    }

    #[test]
    fn zero_window_uses_default() {
        let backoff = UdpAssociateBackoff::new(0);
        assert_eq!(backoff.window_ms, UdpAssociateBackoff::DEFAULT_WINDOW_MS);
    }
}
