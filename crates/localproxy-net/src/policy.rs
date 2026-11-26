//! 核心与注入层共享的运行期安全策略。

use crate::udp::UdpAction;

pub const POLICY_PATH_ENV: &str = "LP_POLICY_PATH";
pub const POLICY_LEASE_FILENAME: &str = "lease";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkPolicy {
    pub udp: UdpAction,
    pub ipv6: UdpAction,
    pub dns_direct: bool,
}

impl NetworkPolicy {
    /// 核心退出或策略文件损坏时，使用保守策略。
    pub const FAIL_CLOSED: Self = Self {
        udp: UdpAction::Block,
        ipv6: UdpAction::Block,
        dns_direct: false,
    };

    pub fn encode(self) -> String {
        let dns = if self.dns_direct { "direct" } else { "proxy" };
        format!("{} {} {dns}\n", self.udp, self.ipv6)
    }

    pub fn decode(value: &str) -> Option<Self> {
        let mut fields = value.split_whitespace();
        let udp = UdpAction::from_str_opt(fields.next()?)?;
        let ipv6 = UdpAction::from_str_opt(fields.next()?)?;
        let dns_direct = match fields.next()? {
            "direct" => true,
            "proxy" => false,
            _ => return None,
        };
        if fields.next().is_some() {
            return None;
        }
        Some(Self {
            udp,
            ipv6,
            dns_direct,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_policy_rejects_partial_or_unrecognized_updates() {
        let policy = NetworkPolicy {
            udp: UdpAction::Proxy,
            ipv6: UdpAction::Block,
            dns_direct: true,
        };
        assert_eq!(NetworkPolicy::decode(&policy.encode()), Some(policy));
        assert!(NetworkPolicy::decode("direct proxy").is_none());
        assert!(NetworkPolicy::decode("direct proxy future").is_none());
    }
}
