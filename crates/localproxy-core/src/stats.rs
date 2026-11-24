//! 流量统计（原子计数器，无锁热路径）。

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// 统计快照。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    /// 累计连接数
    pub connections_total: u64,
    /// 活跃连接数
    pub connections_active: u64,
    /// 上行字节（客户端 → 上游）
    pub bytes_up: u64,
    /// 下行字节（上游 → 客户端）
    pub bytes_down: u64,
    /// 被规则判定 direct 的连接数
    pub connections_direct: u64,
    /// 被规则判定 proxy 的连接数
    pub connections_proxy: u64,
    /// 失败连接数
    pub connections_failed: u64,
}

/// 统计收集器。
#[derive(Debug, Default)]
pub struct Stats {
    connections_total: AtomicU64,
    connections_active: AtomicUsize,
    bytes_up: AtomicU64,
    bytes_down: AtomicU64,
    connections_direct: AtomicU64,
    connections_proxy: AtomicU64,
    connections_failed: AtomicU64,
}

impl Stats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_connect_started(&self) {
        self.connections_total.fetch_add(1, Ordering::Relaxed);
        self.connections_active.fetch_add(1, Ordering::Relaxed);
    }

    pub fn on_connect_finished(&self) {
        self.connections_active.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn on_connect_failed(&self) {
        self.connections_failed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn on_routed_direct(&self) {
        self.connections_direct.fetch_add(1, Ordering::Relaxed);
    }

    pub fn on_routed_proxy(&self) {
        self.connections_proxy.fetch_add(1, Ordering::Relaxed);
    }

    pub fn add_up(&self, n: usize) {
        if n > 0 {
            self.bytes_up.fetch_add(n as u64, Ordering::Relaxed);
        }
    }

    pub fn add_down(&self, n: usize) {
        if n > 0 {
            self.bytes_down.fetch_add(n as u64, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            connections_total: self.connections_total.load(Ordering::Relaxed),
            connections_active: self.connections_active.load(Ordering::Relaxed) as u64,
            bytes_up: self.bytes_up.load(Ordering::Relaxed),
            bytes_down: self.bytes_down.load(Ordering::Relaxed),
            connections_direct: self.connections_direct.load(Ordering::Relaxed),
            connections_proxy: self.connections_proxy.load(Ordering::Relaxed),
            connections_failed: self.connections_failed.load(Ordering::Relaxed),
        }
    }
}
