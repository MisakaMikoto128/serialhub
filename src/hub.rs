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
    /// FR-9b: 最大客户端数, 0 = 不限。超限的新 WS 以 close 1013 拒绝 (ADR-9③)。
    max_clients: u32,
    clients: usize,
    rx_bytes: u64,
    tx_bytes: u64,
    last_error: Option<String>,
    /// ADR-15①: 当次会话内重试计数 —— 每次打开失败 (retry 迁移) +1,
    /// 成功打开归 0; 用户手动 close 归 0。只有监督任务写。
    retries: u32,
    /// FR-12/ADR-16①: 每桥自动重连开关 (默认 true)。false 时串口掉线/打开失败
    /// 直接 phase=closed, 不进重试循环; 手动 open 仍可单次尝试。写方: HTTP 层
    /// (建桥/改配), 读方: 监督任务 (掉线后的分支决策)。
    auto_reconnect: bool,
}

pub struct HubState {
    started: Instant,
    inner: RwLock<Inner>,
}

impl HubState {
    pub fn new(cfg: SerialConfig, max_clients: u32) -> Self {
        Self {
            started: Instant::now(),
            inner: RwLock::new(Inner {
                phase: Phase::Closed,
                cfg,
                max_clients,
                clients: 0,
                rx_bytes: 0,
                tx_bytes: 0,
                last_error: None,
                retries: 0,
                auto_reconnect: true, // FR-12: 默认开启自动重连
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

    // ---- FR-9b: 最大客户端数 ----

    pub fn max_clients(&self) -> u32 {
        r(&self.inner).max_clients
    }

    pub fn set_max_clients(&self, n: u32) {
        w(&self.inner).max_clients = n;
    }

    /// 当前客户端数 (WS 拒连判断用)。
    pub fn client_count(&self) -> usize {
        r(&self.inner).clients
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

    // ---- 重试计数 (ADR-15①, 只有监督任务调用) ----

    /// 打开尝试失败 → Retry 迁移时 +1 (饱和, u32 不回绕)。
    pub fn retry_inc(&self) {
        let mut g = w(&self.inner);
        g.retries = g.retries.saturating_add(1);
    }

    /// 直接读数目前仅监督测试断言用 (生产侧读 /api/status 投影 status_json.retries,
    /// 与本 getter 同源); 随测试编译 —— bin 构建不编入, 免 dead_code 告警。
    #[cfg(test)]
    pub fn retries(&self) -> u32 {
        r(&self.inner).retries
    }

    /// 成功打开归 0; 用户手动 close 归 0。
    pub fn clear_retries(&self) {
        w(&self.inner).retries = 0;
    }

    // ---- 自动重连开关 (FR-12/ADR-16①) ----

    pub fn auto_reconnect(&self) -> bool {
        r(&self.inner).auto_reconnect
    }

    /// 建桥/改配时写; 监督任务在每次掉线/打开失败分支与重试等待中读取,
    /// 因此运行中翻转"即时生效" (下个决策点就改道)。
    pub fn set_auto_reconnect(&self, on: bool) {
        w(&self.inner).auto_reconnect = on;
    }

    // ---- /api/status 投影 ----

    /// 字段名与字段集合是 QA 契约: 恰好 13 个字段, 一个不多一个不少
    /// (ADR-15① 修订 + ADR-16① autoReconnect 12→13)。
    pub fn status_json(&self) -> StatusJson {
        let g = r(&self.inner);
        StatusJson {
            phase: g.phase.as_str(),
            port: g.cfg.port.clone(),
            baud: g.cfg.baud,
            config: g.cfg.config_str(),
            flow: g.cfg.flow.as_str(),
            clients: g.clients,
            max_clients: g.max_clients,
            rx_bytes: g.rx_bytes,
            tx_bytes: g.tx_bytes,
            last_error: g.last_error.clone(),
            retries: g.retries,
            auto_reconnect: g.auto_reconnect,
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
    /// ADR-11: flow 回显 (CLI/UI 双入口对等, 根治 UI 重开静默降级)。
    pub flow: &'static str,
    pub clients: usize,
    #[serde(rename = "maxClients")]
    pub max_clients: u32,
    #[serde(rename = "rxBytes")]
    pub rx_bytes: u64,
    #[serde(rename = "txBytes")]
    pub tx_bytes: u64,
    #[serde(rename = "lastError")]
    pub last_error: Option<String>,
    /// ADR-15①: 当次会话内重试计数 (打开失败 +1, 成功打开/手动 close 归 0)。
    pub retries: u32,
    /// FR-12/ADR-16①: 每桥自动重连开关 (默认 true; 12→13 字段)。
    #[serde(rename = "autoReconnect")]
    pub auto_reconnect: bool,
    #[serde(rename = "uptimeSec")]
    pub uptime_sec: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Flow, Parity};
    use std::collections::HashSet;

    fn hub_with_com1() -> HubState {
        HubState::new(
            SerialConfig {
                port: "COM1".into(),
                ..SerialConfig::default()
            },
            0,
        )
    }

    #[test]
    fn status_json_contract_exact_keys_and_defaults() {
        let hub = hub_with_com1();
        let v = serde_json::to_value(hub.status_json()).unwrap();
        let obj = v.as_object().unwrap();
        // 契约 (ADR-15① 修订 ADR-11 + ADR-16①): 恰好这 13 个字段 (QA 按字段名断言)
        let want: HashSet<&str> = [
            "phase",
            "port",
            "baud",
            "config",
            "flow",
            "clients",
            "maxClients",
            "rxBytes",
            "txBytes",
            "lastError",
            "retries",
            "autoReconnect", // ADR-16① 新增 (12→13)
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
        assert_eq!(v["flow"], "none");
        assert_eq!(v["clients"], 0);
        assert_eq!(v["maxClients"], 0);
        assert_eq!(v["rxBytes"], 0);
        assert_eq!(v["txBytes"], 0);
        assert!(v["lastError"].is_null());
        assert_eq!(v["retries"], 0);
        assert_eq!(v["autoReconnect"], true, "FR-12: autoReconnect 默认 true");
        assert_eq!(v["uptimeSec"], 0);
    }

    /// FR-12/ADR-16①: autoReconnect 可翻转且 /api/status 如实回显
    /// (回显什么 UI 就能存回什么, 不静默清零 —— FIX-17 同类风险)。
    #[test]
    fn auto_reconnect_echo_and_roundtrip() {
        let hub = hub_with_com1();
        assert!(hub.auto_reconnect(), "默认 true");
        assert!(hub.status_json().auto_reconnect);
        hub.set_auto_reconnect(false);
        assert!(!hub.auto_reconnect());
        assert!(!hub.status_json().auto_reconnect);
        hub.set_auto_reconnect(true);
        assert!(hub.status_json().auto_reconnect);
    }

    #[test]
    fn flow_echo_in_status() {
        // ADR-11: flow 回显 —— CLI/UI 任一入口设置后, status 必须如实反映
        // (根治 "CLI 设 xonxoff → UI 重开静默降级为 none")。
        let hub = hub_with_com1();
        let mut cfg = hub.config();
        cfg.flow = Flow::XonXoff;
        hub.update_config(cfg);
        assert_eq!(hub.status_json().flow, "xonxoff");
        let mut cfg = hub.config();
        cfg.flow = Flow::RtsCts;
        hub.update_config(cfg);
        assert_eq!(hub.status_json().flow, "rtscts");
    }

    #[test]
    fn max_clients_roundtrip() {
        // FR-9b: 默认 0 = 不限; /api/config 设置后 status 回显, 供 WS 拒连判断
        let hub = hub_with_com1();
        assert_eq!(hub.max_clients(), 0);
        assert_eq!(hub.client_count(), 0);
        hub.set_max_clients(3);
        assert_eq!(hub.max_clients(), 3);
        assert_eq!(hub.status_json().max_clients, 3);
        hub.client_inc();
        hub.client_inc();
        hub.client_inc();
        assert_eq!(hub.client_count(), 3);
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
        let cfg = SerialConfig {
            port: "COM2".into(),
            baud: 921_600,
            data_bits: 7,
            parity: Parity::E,
            stop_bits: 1,
            ..SerialConfig::default()
        };
        hub.update_config(cfg);
        let s = hub.status_json();
        assert_eq!(s.port, "COM2");
        assert_eq!(s.baud, 921_600);
        assert_eq!(s.config, "7E1");
    }
}
