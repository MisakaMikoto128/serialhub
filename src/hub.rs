//! HubState — 相位状态机、RX/TX 计数、客户端数、最近错误与当前配置的唯一存放处
//! (FR-3/FR-4)。`/api/status` 是它的只读投影。
//!
//! 为什么集中 + Arc<RwLock> (spec §7): 写入方有三个 —— 数据面线程 (计数)、
//! 监督任务 (相位/错误)、HTTP 层 (客户端数); 集中存放才不会有"多份真相"。
//! 锁用 std RwLock: 所有临界区内都不 await, 纳秒级持有, 不需要 tokio 锁;
//! 带毒锁走 into_inner 恢复而不是 panic (FR-7 崩溃面要求)。

use std::sync::RwLock;
use std::sync::{Mutex, MutexGuard, RwLockReadGuard, RwLockWriteGuard};
use std::time::Instant;

use serde::Serialize;

use crate::config::SerialConfig;

/// 带毒锁的兜底读/写: 只有持锁线程 panic 过才会带毒, into_inner 拿到的数据仍可用。
pub fn lock_mutex<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn w<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|e| e.into_inner())
}

fn r<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|e| e.into_inner())
}

/// 状态机相位 (FR-3)。serde 输出必须一字不差: closed / opening / open / retry。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Closed,
    Opening,
    Open,
    Retry,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Closed => "closed",
            Phase::Opening => "opening",
            Phase::Open => "open",
            Phase::Retry => "retry",
        }
    }
}

struct Inner {
    phase: Phase,
    cfg: SerialConfig,
    clients: usize,
    rx_bytes: u64,
    tx_bytes: u64,
    last_error: Option<String>,
}

pub struct HubState {
    started: Instant,
    inner: RwLock<Inner>,
}

impl HubState {
    pub fn new(cfg: SerialConfig) -> Self {
        Self {
            started: Instant::now(),
            inner: RwLock::new(Inner {
                phase: Phase::Closed,
                cfg,
                clients: 0,
                rx_bytes: 0,
                tx_bytes: 0,
                last_error: None,
            }),
        }
    }

    // ---- 相位 (只有监督任务写) ----

    #[allow(dead_code)] // 单元测试使用
    pub fn phase(&self) -> Phase {
        r(&self.inner).phase
    }

    pub fn set_phase(&self, p: Phase) {
        w(&self.inner).phase = p;
    }

    // ---- 配置 (监督任务读, /api/config 与 CLI 启动时写) ----

    pub fn config(&self) -> SerialConfig {
        r(&self.inner).cfg.clone()
    }

    pub fn update_config(&self, cfg: SerialConfig) {
        w(&self.inner).cfg = cfg;
    }

    // ---- 计数器 ----

    pub fn client_inc(&self) {
        w(&self.inner).clients += 1;
    }

    pub fn client_dec(&self) {
        let mut g = w(&self.inner);
        g.clients = g.clients.saturating_sub(1);
    }

    pub fn add_rx(&self, n: usize) {
        if n == 0 {
            return;
        }
        w(&self.inner).rx_bytes += n as u64;
    }

    pub fn add_tx(&self, n: usize) {
        if n == 0 {
            return;
        }
        w(&self.inner).tx_bytes += n as u64;
    }

    // ---- 最近错误 ----

    pub fn set_last_error(&self, e: String) {
        w(&self.inner).last_error = Some(e);
    }

    pub fn clear_last_error(&self) {
        w(&self.inner).last_error = None;
    }

    // ---- /api/status 投影 ----

    /// 字段名与字段集合是 QA 契约: 恰好 9 个字段, 一个不多一个不少。
    pub fn status_json(&self) -> StatusJson {
        let g = r(&self.inner);
        StatusJson {
            phase: g.phase.as_str(),
            port: g.cfg.port.clone(),
            baud: g.cfg.baud,
            config: g.cfg.config_str(),
            clients: g.clients,
            rx_bytes: g.rx_bytes,
            tx_bytes: g.tx_bytes,
            last_error: g.last_error.clone(),
            uptime_sec: self.started.elapsed().as_secs(),
        }
    }
}

/// GET /api/status 的响应体 (FR-4)。字段顺序即 spec 示例顺序 (仅影响可读性)。
#[derive(Debug, Serialize)]
pub struct StatusJson {
    pub phase: &'static str,
    pub port: String,
    pub baud: u32,
    pub config: String,
    pub clients: usize,
    #[serde(rename = "rxBytes")]
    pub rx_bytes: u64,
    #[serde(rename = "txBytes")]
    pub tx_bytes: u64,
    #[serde(rename = "lastError")]
    pub last_error: Option<String>,
    #[serde(rename = "uptimeSec")]
    pub uptime_sec: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Parity;
    use std::collections::HashSet;

    fn hub_with_com1() -> HubState {
        let mut cfg = SerialConfig::default();
        cfg.port = "COM1".into();
        HubState::new(cfg)
    }

    #[test]
    fn status_json_contract_exact_keys_and_defaults() {
        let hub = hub_with_com1();
        let v = serde_json::to_value(hub.status_json()).unwrap();
        let obj = v.as_object().unwrap();
        // 契约: 恰好这 9 个字段 (QA 按字段名断言)
        let want: HashSet<&str> = [
            "phase",
            "port",
            "baud",
            "config",
            "clients",
            "rxBytes",
            "txBytes",
            "lastError",
            "uptimeSec",
        ]
        .into_iter()
        .collect();
        let got: HashSet<&str> = obj.keys().map(|s| s.as_str()).collect();
        assert_eq!(got, want);
        assert_eq!(v["phase"], "closed");
        assert_eq!(v["port"], "COM1");
        assert_eq!(v["baud"], 115200);
        assert_eq!(v["config"], "8N2");
        assert_eq!(v["clients"], 0);
        assert_eq!(v["rxBytes"], 0);
        assert_eq!(v["txBytes"], 0);
        assert!(v["lastError"].is_null());
        assert_eq!(v["uptimeSec"], 0);
    }

    #[test]
    fn counters_accumulate() {
        let hub = hub_with_com1();
        hub.add_rx(1);
        for _ in 0..9 {
            hub.add_rx(3);
        }
        hub.add_tx(256);
        hub.add_tx(0); // 0 不应清零也不应加锁副作用
        hub.client_inc();
        hub.client_inc();
        hub.client_inc();
        hub.client_dec();
        let s = hub.status_json();
        assert_eq!(s.rx_bytes, 28);
        assert_eq!(s.tx_bytes, 256);
        assert_eq!(s.clients, 2);
        // client_dec 不会下溢
        hub.client_dec();
        hub.client_dec();
        hub.client_dec();
        assert_eq!(hub.status_json().clients, 0);
    }

    #[test]
    fn phase_transitions_round_trip() {
        let hub = hub_with_com1();
        // 合法迁移路径全走一遍, as_str 必须与契约一字不差
        let seq = [
            (Phase::Closed, "closed"),
            (Phase::Opening, "opening"),
            (Phase::Open, "open"),
            (Phase::Retry, "retry"),
            (Phase::Opening, "opening"),
            (Phase::Open, "open"),
            (Phase::Closed, "closed"),
        ];
        for (p, s) in seq {
            hub.set_phase(p);
            assert_eq!(hub.phase(), p);
            assert_eq!(hub.status_json().phase, s);
        }
    }

    #[test]
    fn last_error_set_and_clear() {
        let hub = hub_with_com1();
        assert!(hub.status_json().last_error.is_none());
        hub.set_last_error("打开 COM1 失败: 拒绝访问。 (os error 5)".into());
        assert_eq!(
            hub.status_json().last_error.as_deref(),
            Some("打开 COM1 失败: 拒绝访问。 (os error 5)")
        );
        hub.clear_last_error();
        assert!(hub.status_json().last_error.is_none());
    }

    #[test]
    fn config_update_reflected_in_status() {
        let hub = hub_with_com1();
        let mut cfg = SerialConfig::default();
        cfg.port = "COM2".into();
        cfg.baud = 921_600;
        cfg.data_bits = 7;
        cfg.parity = Parity::E;
        cfg.stop_bits = 1;
        hub.update_config(cfg);
        let s = hub.status_json();
        assert_eq!(s.port, "COM2");
        assert_eq!(s.baud, 921_600);
        assert_eq!(s.config, "7E1");
    }
}
