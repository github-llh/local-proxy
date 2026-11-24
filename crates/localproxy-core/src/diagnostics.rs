//! 连接诊断日志（环形缓冲）。
//!
//! 记录每个被核心处理的连接的走向：目标、路由决策（proxy/direct）、
//! 命中规则、结果。用于：
//! - popover「最近连接」展示（流量走向可见）；
//! - 泄漏投诉/排障时举证 fail-closed 生效；
//! - 控制面 `/v1/connections` 暴露。
//!
//! 不记录载荷内容；仅元数据。

use std::collections::VecDeque;
use std::sync::Mutex;

/// 单条连接记录。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConnectionRecord {
    /// Unix 时间戳（毫秒）
    pub ts_ms: u64,
    /// 协议（tcp/udp）
    pub protocol: String,
    /// 目标主机（域名或 IP）
    pub host: String,
    /// 目标端口
    pub port: u16,
    /// 路由动作：proxy / direct / blocked
    pub action: String,
    /// 命中的规则名（默认动作为空）
    pub rule: String,
    /// 结果：ok / failed
    pub result: String,
    /// 失败原因（成功为空）
    pub error: String,
}

/// 连接日志（固定容量环形缓冲，满则淘汰最旧）。
#[derive(Debug)]
pub struct ConnectionLog {
    capacity: usize,
    records: Mutex<VecDeque<ConnectionRecord>>,
}

impl ConnectionLog {
    /// 默认容量 1000 条。
    pub const DEFAULT_CAPACITY: usize = 1000;

    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            records: Mutex::new(VecDeque::with_capacity(capacity.min(1024))),
        }
    }

    /// 默认容量构造。
    pub fn with_default_capacity() -> Self {
        Self::new(Self::DEFAULT_CAPACITY)
    }

    /// 记录一条连接。
    pub fn record(&self, record: ConnectionRecord) {
        let mut records = match self.records.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if records.len() >= self.capacity {
            records.pop_front();
        }
        records.push_back(record);
    }

    /// 最近 N 条（新的在前）。
    pub fn recent(&self, limit: usize) -> Vec<ConnectionRecord> {
        let records = match self.records.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        records.iter().rev().take(limit).cloned().collect()
    }

    /// 当前记录数。
    pub fn len(&self) -> usize {
        match self.records.lock() {
            Ok(g) => g.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for ConnectionLog {
    fn default() -> Self {
        Self::with_default_capacity()
    }
}

/// 当前 Unix 时间戳（毫秒）。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(host: &str, port: u16) -> ConnectionRecord {
        ConnectionRecord {
            ts_ms: now_ms(),
            protocol: "tcp".to_string(),
            host: host.to_string(),
            port,
            action: "proxy".to_string(),
            rule: "r1".to_string(),
            result: "ok".to_string(),
            error: String::new(),
        }
    }

    #[test]
    fn ring_buffer_evicts_oldest() {
        let log = ConnectionLog::new(3);
        for i in 0..5 {
            log.record(record(&format!("h{i}"), 443));
        }
        assert_eq!(log.len(), 3);
        let recent = log.recent(10);
        // 最新在前：h4, h3, h2（h0/h1 被淘汰）
        assert_eq!(recent[0].host, "h4");
        assert_eq!(recent[1].host, "h3");
        assert_eq!(recent[2].host, "h2");
    }

    #[test]
    fn recent_limit() {
        let log = ConnectionLog::new(100);
        for i in 0..10 {
            log.record(record(&format!("h{i}"), 443));
        }
        assert_eq!(log.recent(3).len(), 3);
        assert_eq!(log.recent(0).len(), 0);
        assert_eq!(log.recent(100).len(), 10);
    }

    #[test]
    fn serialize_shape() {
        let log = ConnectionLog::new(4);
        log.record(record("example.com", 443));
        let json = serde_json::to_string(&log.recent(1)).unwrap();
        assert!(json.contains("example.com"));
        assert!(json.contains("\"action\":\"proxy\""));
        assert!(json.contains("\"protocol\":\"tcp\""));
    }
}
