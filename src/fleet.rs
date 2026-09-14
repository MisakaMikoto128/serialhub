//! 多桥管理器 (FR-10 / ADR-13, Sprint 4) —— 产品主体。
//!
//! 架构 (文字稿):
//!
//! ```text
//!                  控制面 (管理台, --addr / fleet [manager] addr, 永远可达, FR-10a/13)
//!    GET/POST /api/fleet ...  ──────────────┐
//!    旧 /api/status /api/config /ws (兼容分发)│
//!                  ┌────────────────────────┘
//!           BridgeManager (本模块)
//!            │  bridges: BTreeMap<id, Arc<Bridge>>   ←→ fleet.json (变更即写)
//!            │
//!            ├── Bridge b1 ── 数据面 listener 127.0.0.1:8101 ── /ws (纯二进制)
//!            │      └─ hub(HubState) + supervisor(自动重开) + tx 队列 + broadcast
//!            │           —— 全部复用单桥内部件, 帧语义零变化 (硬约束 1/2/3)
//!            ├── Bridge b2 ── 数据面 listener 127.0.0.1:8102 ── /ws
//!            └── Bridge bN ── ...
//! ```
//!
//! - 每座桥 = 独立 listen 端口 + 独立 HubState/监督任务/tx 队列/broadcast;
//!   数据面只有 ws://<listen>/ws 一条二进制通道 (FR-10g), 控制面不碰串口字节。
//! - 端口稳定 (FR-10f): 桥 listen 端口生命周期内不变; serve 异常自动按 1s 重绑重建;
//!   串口掉线沿用监督任务自动重开。stop 释放 COM, start 重绑同一端口。
//! - 兼容分发 (FR-10h): 旧 CLI 参数等价于自动建一座"兼容桥"; 恰好一座桥时,
//!   管理台上的旧单桥端点 (/api/status、/api/config、/api/open|close、/ws 等)
//!   直接分发到那座桥的内部件 —— 不是 HTTP 代理, 是同进程 Arc 直调, 帧语义零变化。
//! - 持久化 (FR-10b): fleet.json 变更即写 (临时文件+改名原子替换); 启动时存在则
//!   恢复全部桥 (绑不上端口的桥以 stopped+lastError 状态入队, 可手动 start)。
//! - 统计 (FR-10c): stats::RateWindow 每桥 1s 滑窗, 管理器任务 200ms 采样累计值。

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use axum::extract::rejection::JsonRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::api::{self, App};
use crate::config::{Flow, Parity, SerialConfig};
use crate::hub::{lock_mutex, HubState, Phase};
use crate::serial::{PortCtx, RealOpener};
use crate::service::{OnEvent, ServiceEvent};
use crate::stats::RateWindow;
use crate::supervisor::{self, HubCmd};

// ============================================================ Bridge (单桥组件)

/// 一座桥: 复用单桥内部件 (hub/监督任务/tx 队列/broadcast) + 独立数据面 listener。
/// 相位/配置/计数/最近错误唯一存放处仍是 HubState (单一真相, 见 hub.rs 模块头)。
pub struct Bridge {
    pub id: String,
    name: RwLock<String>,
    /// 数据端口 (FR-10f 生命周期内不变; 建桥时端口 0 → 绑定后回填实际端口)。
    pub listen: Mutex<SocketAddr>,
    pub hub: Arc<HubState>,
    pub ctx: PortCtx,
    pub cmd_tx: UnboundedSender<HubCmd>,
    /// 建桥/start 时是否自动打开串口 (持久化字段)。
    pub auto_open: AtomicBool,
    /// 数据面是否在跑 (start/stop/delete 状态)。
    running: AtomicBool,
    /// 桥的创建时刻 (uptimeSec 口径 = 桥龄, 与 legacy 服务 uptime 同语义)。
    pub created: Instant,
    /// 本桥数据面停机开关 (stop/删除/进程退出 → serve 优雅退出 + 断开本桥全部 WS)。
    pub stop_tx: watch::Sender<bool>,
    /// 统计引擎: 1s 滑窗 (采样由管理器任务驱动)。
    pub rates: Mutex<RateWindow>,
    serve_handle: Mutex<Option<JoinHandle<()>>>,
}

impl Bridge {
    /// 装配一座桥: hub + broadcast + tx 槽 + 监督任务 (FR-3 自动重开, 1s 重试)。
    /// 需要 tokio 运行时上下文 (监督任务在此 spawn)。
    fn new(
        id: String,
        name: String,
        serial: SerialConfig,
        auto_open: bool,
        auto_reconnect: bool,
        max_clients: u32,
    ) -> Arc<Bridge> {
        let hub = Arc::new(HubState::new(serial, max_clients));
        hub.set_auto_reconnect(auto_reconnect); // FR-12: 建桥即定 (改配走 hub)
        let (bc_tx, _) = tokio::sync::broadcast::channel::<Vec<u8>>(1024);
        let ctx = PortCtx {
            hub: hub.clone(),
            bc_tx,
            tx_slot: Arc::new(Mutex::new(None)),
            active_stop: Arc::new(Mutex::new(None)),
        };
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<HubCmd>();
        let (stop_tx, _) = watch::channel(false);
        tokio::spawn(supervisor::run_supervisor(
            ctx.clone(),
            cmd_rx,
            Arc::new(RealOpener),
            Duration::from_secs(1),
        ));
        Arc::new(Bridge {
            id,
            name: RwLock::new(name),
            listen: Mutex::new(SocketAddr::from(([127, 0, 0, 1], 0))),
            hub,
            ctx,
            cmd_tx,
            auto_open: AtomicBool::new(auto_open),
            running: AtomicBool::new(false),
            created: Instant::now(),
            stop_tx,
            rates: Mutex::new(RateWindow::new()),
            serve_handle: Mutex::new(None),
        })
    }

    pub fn name(&self) -> String {
        self.name
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_name(&self, n: String) {
        *self.name.write().unwrap_or_else(|e| e.into_inner()) = n;
    }

    pub fn listen_addr(&self) -> SocketAddr {
        *lock_mutex(&self.listen)
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// FR-10g 单桥详情 (列表项同构)。phase 恒取 hub 四态 (closed/opening/open/retry,
    /// 与 UI-3 徽章和 QA 契约一致: stop 后 = closed); 停止态由额外 "running" 字段区分。
    pub fn detail_json(&self) -> Value {
        let s = self.hub.status_json();
        let (rx_rate, tx_rate) = lock_mutex(&self.rates).rates(Instant::now());
        let cfg = self.hub.config();
        let running = self.is_running();
        // f64 保留 1 位小数, 避免 JSON 里无穷小数
        let r1 = |v: f64| (v * 10.0).round() / 10.0;
        json!({
            "id": self.id,
            "name": self.name(),
            "serial": {
                "port": cfg.port,
                "baud": cfg.baud,
                "dataBits": cfg.data_bits,
                "parity": cfg.parity.as_char().to_string(),
                "stopBits": cfg.stop_bits,
                "flow": cfg.flow.as_str(),
            },
            "listen": self.listen_addr().to_string(),
            "phase": s.phase,
            "running": running,
            "clients": s.clients,
            "maxClients": s.max_clients,
            "rxBytes": s.rx_bytes,
            "txBytes": s.tx_bytes,
            "rxRate": r1(rx_rate),
            "txRate": r1(tx_rate),
            "lastError": s.last_error,
            "retries": s.retries, // ADR-15①: 当次会话内重试计数 (契约 13→14 字段)
            "autoReconnect": s.auto_reconnect, // FR-12/ADR-16① (契约 14→15 字段)
            "uptimeSec": self.created.elapsed().as_secs(),
            "autoOpen": self.auto_open.load(Ordering::Relaxed),
        })
    }
}

/// 桥数据面路由: 只有 /ws (纯二进制数据面) 与 /api/status (只读投影, 便于直连诊断)。
fn bridge_router(b: Arc<Bridge>) -> Router {
    Router::new()
        .route("/ws", get(bridge_ws))
        .route("/api/status", get(bridge_status))
        .with_state(b)
}

async fn bridge_status(State(b): State<Arc<Bridge>>) -> Response {
    api::status_core(&b.ctx)
}

/// 桥数据面 /ws: 与 legacy 单桥完全同一个 client_loop (帧语义零变化, 硬约束 1/2/3)。
/// 停机 watch 传本桥的 stop_tx —— 桥 stop/删除时主动断开本桥全部客户端。
async fn bridge_ws(ws: WebSocketUpgrade, State(b): State<Arc<Bridge>>) -> Response {
    ws.on_upgrade(move |socket| {
        api::client_loop(
            socket,
            App {
                ctx: b.ctx.clone(),
                cmd_tx: b.cmd_tx.clone(),
                shutdown_tx: b.stop_tx.clone(),
                restart_to: Arc::new(Mutex::new(None)),
                index: "",
            },
        )
    })
}

/// 桥数据面 serve 主循环 (FR-10f 端点稳定):
/// - 优雅停机 (stop/删除/进程退出) → 退出循环 → 释放串口;
/// - serve 异常 (listener 死亡等) → 按 1s 间隔重绑**同一端口**, hub/监督任务全程不动。
async fn bridge_serve(
    bridge: Arc<Bridge>,
    listener: TcpListener,
    stop_rx: watch::Receiver<bool>,
) {
    let id = bridge.id.clone();
    let mut pending = Some(listener);
    loop {
        let listener = match pending.take() {
            Some(l) => l,
            None => {
                let addr = bridge.listen_addr();
                let mut rebuilt = None;
                while !*stop_rx.borrow() {
                    match TcpListener::bind(addr).await {
                        Ok(l) => {
                            rebuilt = Some(l);
                            break;
                        }
                        Err(e) => {
                            eprintln!(
                                "serialhub: 桥 {id} 重建数据端口 {addr} 失败: {e} (1s 后重试, FR-10f)"
                            );
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
                match rebuilt {
                    Some(l) => l,
                    None => break, // 重试期间收到停机
                }
            }
        };
        let b = bridge.clone();
        let mut serve_sd = stop_rx.clone();
        let res = tokio::spawn(async move {
            let shutdown = async move {
                let _ = serve_sd.changed().await;
            };
            axum::serve(listener, bridge_router(b))
                .with_graceful_shutdown(shutdown)
                .await
        })
        .await;
        match res {
            Ok(Ok(())) => break, // 优雅停机
            Ok(Err(e)) => {
                eprintln!("serialhub: 桥 {id} 数据面异常: {e} —— 自动重建 listener (FR-10f)")
            }
            Err(e) => {
                eprintln!("serialhub: 桥 {id} 数据面任务异常: {e:?} —— 自动重建 listener (FR-10f)")
            }
        }
        if *stop_rx.borrow() {
            break;
        }
    }
    // 数据面停止: COM 释放优先 (与 legacy finalize 同序), 相位归位由监督任务处理
    bridge.running.store(false, Ordering::Relaxed);
    let _ = bridge.cmd_tx.send(HubCmd::Close);
    bridge.ctx.stop_active();
}

// ============================================================ BridgeManager

fn rd<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|e| e.into_inner())
}

fn wr<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|e| e.into_inner())
}

/// 建桥参数 (内部)。
pub struct BridgeSpec {
    /// 显式 id (恢复 fleet 时用); None = 自动分配 b1,b2,...
    pub id: Option<String>,
    pub name: String,
    pub serial: SerialConfig,
    pub listen: SocketAddr,
    pub auto_open: bool,
    /// FR-12/ADR-16①: 自动重连开关 (默认 true)。
    pub auto_reconnect: bool,
    pub max_clients: u32,
}

/// 同进程多桥管理器: 桥表 + 持久化 + 兼容分发解析。
pub struct BridgeManager {
    bridges: RwLock<BTreeMap<String, Arc<Bridge>>>,
    /// fleet.json 路径; None = 持久化关闭 (--no-fleet)。
    pub fleet_path: Option<PathBuf>,
    /// FR-13: 控制面 (管理台) 当前地址 —— 随启动绑定与每次换址更新,
    /// persist 时写入 fleet.json 顶层 [manager] 段 (重启恢复)。
    manager_addr: Mutex<Option<SocketAddr>>,
    /// 恢复 fleet 期间抑制 persist (避免把"暂时绑不上"的桥从清单里抹掉)。
    persist_suppressed: AtomicBool,
    next_id: AtomicU64,
}

impl BridgeManager {
    pub fn new(fleet_path: Option<PathBuf>) -> Self {
        Self {
            bridges: RwLock::new(BTreeMap::new()),
            fleet_path,
            manager_addr: Mutex::new(None),
            persist_suppressed: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
        }
    }

    /// FR-13: 登记控制面当前地址 (启动绑定后与每次换址成功后调用)。
    pub fn set_manager_addr(&self, a: SocketAddr) {
        *lock_mutex(&self.manager_addr) = Some(a);
    }

    pub fn manager_addr(&self) -> Option<SocketAddr> {
        *lock_mutex(&self.manager_addr)
    }

    pub fn snapshot(&self) -> Vec<Arc<Bridge>> {
        rd(&self.bridges).values().cloned().collect()
    }

    pub fn get(&self, id: &str) -> Option<Arc<Bridge>> {
        rd(&self.bridges).get(id).cloned()
    }

    /// 兼容分发的目标 (FR-10h): 恰好一座桥时返回它。
    pub fn single_bridge(&self) -> Option<Arc<Bridge>> {
        let m = rd(&self.bridges);
        if m.len() == 1 {
            m.values().next().cloned()
        } else {
            None
        }
    }

    /// 建桥 (API 用, 严格模式): 端口冲突/绑定失败 → Err, 桥不入队。
    pub async fn create_bridge(&self, spec: BridgeSpec) -> Result<Arc<Bridge>, String> {
        self.create_bridge_inner(spec, false).await
    }

    async fn create_bridge_inner(
        &self,
        spec: BridgeSpec,
        lenient: bool,
    ) -> Result<Arc<Bridge>, String> {
        if spec.listen.port() != 0 {
            self.check_port_conflict(spec.listen, spec.id.as_deref())?;
        }
        if let Some(id) = &spec.id {
            if self.get(id).is_some() {
                return Err(format!("桥 id 已存在: {id}"));
            }
        }
        let id = match spec.id {
            Some(id) => id,
            None => loop {
                let cand = format!("b{}", self.next_id.fetch_add(1, Ordering::Relaxed));
                if self.get(&cand).is_none() {
                    break cand;
                }
            },
        };
        let name = if spec.name.trim().is_empty() {
            format!("桥 {id}")
        } else {
            spec.name.trim().to_string()
        };
        let bridge = Bridge::new(
            id,
            name,
            spec.serial.clone(),
            spec.auto_open,
            spec.auto_reconnect,
            spec.max_clients,
        );
        match bind_with_retry(spec.listen, Duration::from_secs(2)).await {
            Ok(listener) => {
                let bound = listener
                    .local_addr()
                    .map_err(|e| format!("获取监听地址失败: {e}"))?;
                *lock_mutex(&bridge.listen) = bound;
                bridge.running.store(true, Ordering::Relaxed);
                let h = tokio::spawn(bridge_serve(
                    bridge.clone(),
                    listener,
                    bridge.stop_tx.subscribe(),
                ));
                *lock_mutex(&bridge.serve_handle) = Some(h);
                if spec.auto_open && !spec.serial.port.is_empty() {
                    let _ = bridge.cmd_tx.send(HubCmd::Open);
                }
            }
            Err(e) => {
                if !lenient {
                    return Err(e);
                }
                // 恢复场景: 暂时绑不上 (端口被外部占用) → 以 stopped 入队并留痕,
                // 用户释放端口后可 start 重试 (FR-10f 端口生命周期内不变)。
                bridge.hub.set_last_error(e);
            }
        }
        wr(&self.bridges).insert(bridge.id.clone(), bridge.clone());
        self.persist();
        Ok(bridge)
    }

    /// 端口冲突校验 (FR-10b 建桥时): 同端口且 IP 相容 (或任一方为通配) 即冲突。
    fn check_port_conflict(&self, listen: SocketAddr, exclude: Option<&str>) -> Result<(), String> {
        for (id, b) in rd(&self.bridges).iter() {
            if Some(id.as_str()) == exclude {
                continue;
            }
            let other = b.listen_addr();
            if other.port() == listen.port()
                && (other.ip() == listen.ip()
                    || other.ip().is_unspecified()
                    || listen.ip().is_unspecified())
            {
                return Err(format!(
                    "listen 端口冲突: {listen} 与桥 {id} ({other}) 使用同一端口"
                ));
            }
        }
        Ok(())
    }

    /// 启动一座停止中的桥: 重绑**同一端口** (FR-10f), autoOpen 则自动开串口。幂等。
    pub async fn start_bridge(&self, id: &str) -> Result<(), String> {
        let Some(b) = self.get(id) else {
            return Err(format!("桥不存在: {id}"));
        };
        if b.is_running() {
            return Ok(());
        }
        let addr = b.listen_addr();
        self.check_port_conflict(addr, Some(id))?;
        let listener = bind_with_retry(addr, Duration::from_secs(2)).await?;
        let _ = b.stop_tx.send(false); // 复位停机开关 (随后 spawn 的订阅者以此为基线)
        b.running.store(true, Ordering::Relaxed);
        let h = tokio::spawn(bridge_serve(b.clone(), listener, b.stop_tx.subscribe()));
        *lock_mutex(&b.serve_handle) = Some(h);
        if b.auto_open.load(Ordering::Relaxed) && !b.hub.config().port.is_empty() {
            let _ = b.cmd_tx.send(HubCmd::Open);
        }
        Ok(())
    }

    /// 停止一座桥: COM 释放优先 → 断开本桥全部 WS → 关数据端口 (幂等)。
    pub async fn stop_bridge(&self, id: &str) -> Result<(), String> {
        let Some(b) = self.get(id) else {
            return Err(format!("桥不存在: {id}"));
        };
        self.stop_one(&b).await;
        Ok(())
    }

    async fn stop_one(&self, b: &Arc<Bridge>) {
        if !b.running.swap(false, Ordering::Relaxed) {
            return; // 幂等
        }
        let _ = b.cmd_tx.send(HubCmd::Close); // COM 释放优先
        b.ctx.stop_active();
        let _ = b.stop_tx.send(true);
        let h = lock_mutex(&b.serve_handle).take();
        if let Some(h) = h {
            if tokio::time::timeout(Duration::from_secs(2), h).await.is_err() {
                eprintln!("serialhub: 桥 {} 数据面停机超时 (仍有客户端未退)", b.id);
            }
        }
    }

    /// 删除一座桥: 停止 + 移出桥表 + 持久化。
    pub async fn delete_bridge(&self, id: &str) -> Result<(), String> {
        let Some(b) = wr(&self.bridges).remove(id) else {
            return Err(format!("桥不存在: {id}"));
        };
        self.stop_one(&b).await;
        self.persist();
        Ok(())
    }

    /// 改配 (FR-10g config): name/串口参数/maxClients/autoOpen/autoReconnect;
    /// listen 不可改 (FR-10f)。成功即持久化。串口参数沿用"close→config→open"
    /// 语义 (spec 非目标: 不热改); autoReconnect 经 apply_config_core 即改即生效。
    pub fn config_bridge(
        &self,
        id: &str,
        name: Option<String>,
        auto_open: Option<bool>,
        req: api::ConfigReq,
    ) -> Response {
        let Some(b) = self.get(id) else {
            return not_found_bridge(id);
        };
        if let Some(n) = name {
            b.set_name(n);
        }
        let resp = api::apply_config_core(&b.ctx, req);
        if resp.status().is_success() {
            if let Some(a) = auto_open {
                b.auto_open.store(a, Ordering::Relaxed);
            }
            self.persist();
        }
        resp
    }

    /// fleet.json 变更即写 (FR-10b)。持久化关闭 (--no-fleet) 或恢复期间 = 空操作。
    /// FR-13: 同时写入顶层 [manager] 段 (控制面地址, set_manager_addr 登记的现值)。
    pub fn persist(&self) {
        if self.persist_suppressed.load(Ordering::Relaxed) {
            return;
        }
        let Some(path) = &self.fleet_path else { return };
        let recs: Vec<FleetBridgeRec> = self.snapshot().iter().map(rec_of_bridge).collect();
        let manager = self.manager_addr().map(|a| ManagerRec { addr: a.to_string() });
        if let Err(e) = save_fleet(path, &recs, manager.as_ref()) {
            eprintln!("serialhub: fleet 清单写入失败 ({path:?}): {e}");
        }
    }
}

// ============================================================ 持久化 (fleet.json)

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct FleetBridgeRec {
    pub id: String,
    pub name: String,
    #[serde(rename = "autoOpen", default)]
    pub auto_open: bool,
    #[serde(rename = "autoReconnect", default = "default_auto_reconnect")]
    pub auto_reconnect: bool,
    #[serde(rename = "maxClients", default)]
    pub max_clients: u32,
    pub listen: String,
    pub serial: SerialRec,
}

fn default_auto_reconnect() -> bool {
    true // FR-12: 旧清单无此字段 → 按默认 true 恢复 (与建桥缺省一致)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct SerialRec {
    #[serde(default)]
    pub port: String,
    #[serde(default = "default_baud")]
    pub baud: u32,
    #[serde(rename = "dataBits", default = "default_data_bits")]
    pub data_bits: u8,
    #[serde(default = "default_parity")]
    pub parity: String,
    #[serde(rename = "stopBits", default = "default_stop_bits")]
    pub stop_bits: u8,
    #[serde(default = "default_flow")]
    pub flow: String,
}

fn default_baud() -> u32 {
    115200
}
fn default_data_bits() -> u8 {
    8
}
fn default_parity() -> String {
    "N".into()
}
fn default_stop_bits() -> u8 {
    2
}
fn default_flow() -> String {
    "none".into()
}

impl SerialRec {
    fn of_config(cfg: &SerialConfig) -> Self {
        Self {
            port: cfg.port.clone(),
            baud: cfg.baud,
            data_bits: cfg.data_bits,
            parity: cfg.parity.as_char().to_string(),
            stop_bits: cfg.stop_bits,
            flow: cfg.flow.as_str().to_string(),
        }
    }

    fn to_config(&self) -> Result<SerialConfig, String> {
        let cfg = SerialConfig {
            port: self.port.trim().to_string(),
            baud: self.baud,
            data_bits: self.data_bits,
            parity: Parity::parse(&self.parity)?,
            stop_bits: self.stop_bits,
            flow: Flow::parse(&self.flow)?,
        };
        cfg.validate()?;
        Ok(cfg)
    }
}

/// FR-13: fleet.json 顶层 [manager] 段 —— 控制面 (管理台) 地址持久化,
/// 重启恢复 (显式 --addr 优先, 见 run_manager_with)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct ManagerRec {
    #[serde(default)]
    pub addr: String,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct FleetFile {
    version: u32,
    #[serde(default)]
    bridges: Vec<FleetBridgeRec>,
    /// FR-13: 旧清单无此字段 → default None 兼容; 写出时空段跳过。
    #[serde(default, rename = "manager", skip_serializing_if = "Option::is_none")]
    manager: Option<ManagerRec>,
}

/// 原子写: 临时文件 + 改名覆盖 (进程中途被杀不会留下半截清单)。
pub(crate) fn save_fleet(
    path: &Path,
    bridges: &[FleetBridgeRec],
    manager: Option<&ManagerRec>,
) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {e}"))?;
        }
    }
    let body = serde_json::to_string_pretty(&FleetFile {
        version: 1,
        bridges: bridges.to_vec(),
        manager: manager.cloned(),
    })
    .map_err(|e| format!("序列化失败: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).map_err(|e| format!("写入临时文件失败: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("替换清单失败: {e}"))
}

/// 整文件读取 (FR-13: 含 [manager] 段)。
pub(crate) fn load_fleet_file(path: &Path) -> Result<FleetFile, String> {
    let body = std::fs::read_to_string(path).map_err(|e| format!("读取失败: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("解析失败: {e}"))
}

/// 桥记录读取 (旧接口; [manager] 段请走 load_fleet_file)。
pub(crate) fn load_fleet(path: &Path) -> Result<Vec<FleetBridgeRec>, String> {
    Ok(load_fleet_file(path)?.bridges)
}

fn rec_of_bridge(b: &Arc<Bridge>) -> FleetBridgeRec {
    FleetBridgeRec {
        id: b.id.clone(),
        name: b.name(),
        auto_open: b.auto_open.load(Ordering::Relaxed),
        auto_reconnect: b.hub.auto_reconnect(),
        max_clients: b.hub.max_clients(),
        listen: b.listen_addr().to_string(),
        serial: SerialRec::of_config(&b.hub.config()),
    }
}

/// 启动恢复 (FR-10b): fleet.json 存在则恢复全部桥。
/// 单条记录非法 → 跳过并告警; 绑不上端口 → lenient 模式以 stopped+lastError 入队。
async fn restore_fleet(mgr: &Arc<BridgeManager>) -> usize {
    let Some(path) = mgr.fleet_path.clone() else { return 0 };
    if !path.exists() {
        return 0;
    }
    let recs = match load_fleet(&path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("serialhub: fleet 清单无法读取 ({path:?}): {e} —— 以空桥队启动");
            return 0;
        }
    };
    mgr.persist_suppressed.store(true, Ordering::Relaxed);
    let mut n = 0usize;
    let mut max_id = 0u64;
    for rec in &recs {
        if let Ok(num) = rec.id.trim_start_matches('b').parse::<u64>() {
            max_id = max_id.max(num);
        }
        let listen = match parse_listen(&rec.listen) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("serialhub: 跳过桥 {}: {e}", rec.id);
                continue;
            }
        };
        let serial = match rec.serial.to_config() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("serialhub: 跳过桥 {}: 串口配置无效: {e}", rec.id);
                continue;
            }
        };
        match mgr
            .create_bridge_inner(
                BridgeSpec {
                    id: Some(rec.id.clone()),
                    name: rec.name.clone(),
                    serial,
                    listen,
                    auto_open: rec.auto_open,
                    auto_reconnect: rec.auto_reconnect,
                    max_clients: rec.max_clients,
                },
                true,
            )
            .await
        {
            Ok(b) => {
                n += 1;
                println!(
                    "serialhub: 已恢复桥 {} ({}) → 数据端口 {}{}",
                    b.id,
                    b.name(),
                    b.listen_addr(),
                    if b.is_running() { "" } else { " (端口暂不可用, 已停止)" }
                );
            }
            Err(e) => eprintln!("serialhub: 恢复桥 {} 失败: {e}", rec.id),
        }
    }
    mgr.next_id.fetch_max(max_id + 1, Ordering::Relaxed);
    mgr.persist_suppressed.store(false, Ordering::Relaxed);
    n
}

// ============================================================ 启动参数

/// 管理器启动参数 (由 Cli 派生, 见 ManagerStartup::from_cli)。
#[derive(Debug, Clone)]
pub struct ManagerStartup {
    /// 控制面 (管理台) 地址 (FR-10a)。FR-13: 非显式给出时可被
    /// fleet.json [manager] addr 覆盖 (重启恢复, 见 addr_explicit)。
    pub control_addr: SocketAddr,
    /// FR-13: CLI 是否显式给出 --addr (显式 → 优先于清单恢复值)。
    pub addr_explicit: bool,
    /// FR-14: 主题目录; None = exe 旁 themes/ (themes::default_themes_dir)。
    pub themes_dir: Option<PathBuf>,
    /// CLI 兼容桥参数 (FR-10h); None = 不建 (纯 fleet 模式, 测试用)。
    pub cli_bridge: Option<CliBridgeSpec>,
    /// fleet.json 路径; None = 持久化关闭 (--no-fleet)。
    pub fleet_path: Option<PathBuf>,
}

/// CLI 兼容桥 (FR-10h): 旧单桥参数等价于"建一座桥并启动"。
#[derive(Debug, Clone)]
pub struct CliBridgeSpec {
    pub name: String,
    pub serial: SerialConfig,
    pub auto_open: bool,
    /// FR-12/ADR-16①: 自动重连开关 (CLI --reconnect/--no-reconnect, 默认 true)。
    pub auto_reconnect: bool,
    pub max_clients: u32,
}

impl ManagerStartup {
    pub fn from_cli(cli: &crate::cli::Cli) -> Self {
        let fleet_path = if cli.no_fleet {
            None
        } else {
            Some(cli.fleet.clone().unwrap_or_else(default_fleet_path))
        };
        let serial = SerialConfig {
            port: cli.port.clone().unwrap_or_default(),
            baud: cli.baud,
            data_bits: cli.data_bits,
            parity: cli.parity,
            stop_bits: cli.stop_bits,
            flow: cli.flow,
        };
        Self {
            control_addr: cli.addr,
            addr_explicit: cli.addr_explicit,
            themes_dir: cli.themes_dir.clone(),
            fleet_path,
            cli_bridge: Some(CliBridgeSpec {
                name: "CLI".into(),
                serial,
                auto_open: cli.auto_open(),
                auto_reconnect: cli.auto_reconnect,
                max_clients: cli.max_clients,
            }),
        }
    }
}

/// 默认清单路径: %APPDATA%\SerialHub\fleet.json (Windows) /
/// ~/.config/serialhub/fleet.json (其他平台)。
pub fn default_fleet_path() -> PathBuf {
    #[cfg(windows)]
    if let Some(base) = std::env::var_os("APPDATA") {
        return PathBuf::from(base).join("SerialHub").join("fleet.json");
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        return PathBuf::from(home)
            .join(".config")
            .join("serialhub")
            .join("fleet.json");
    }
    PathBuf::from("fleet.json")
}

/// listen 解析: 完整地址 / 裸端口 (→127.0.0.1:<p>) / 空 (→随机端口)。
pub(crate) fn parse_listen(s: &str) -> Result<SocketAddr, String> {
    let t = s.trim();
    if t.is_empty() {
        return Ok(SocketAddr::from(([127, 0, 0, 1], 0)));
    }
    if let Ok(a) = t.parse::<SocketAddr>() {
        return Ok(a);
    }
    if let Ok(p) = t.parse::<u16>() {
        return Ok(SocketAddr::from(([127, 0, 0, 1], p)));
    }
    Err(format!("listen 地址无效: \"{t}\" (形如 127.0.0.1:8101 或 8101)"))
}

/// 兼容桥数据端口选址: 从管理台端口 +1 起向上探测首个空闲端口 (最多 20 个),
/// 全忙 → 交由系统分配。先探测后重绑存在微小竞窗, create_bridge 的重试可吸收。
async fn pick_listen_near(control: SocketAddr) -> SocketAddr {
    let ip = if control.ip().is_unspecified() {
        IpAddr::from([127, 0, 0, 1])
    } else {
        control.ip()
    };
    let base = control.port();
    if base == 0 {
        return SocketAddr::new(ip, 0);
    }
    for delta in 1..=20u16 {
        if let Some(p) = base.checked_add(delta) {
            let cand = SocketAddr::new(ip, p);
            if TcpListener::bind(cand).await.is_ok() {
                return cand;
            }
        }
    }
    SocketAddr::new(ip, 0)
}

/// 绑定重试 (与 legacy run_service 同策略: 250ms 步进至 deadline)。
async fn bind_with_retry(addr: SocketAddr, within: Duration) -> Result<TcpListener, String> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match TcpListener::bind(addr).await {
            Ok(l) => return Ok(l),
            Err(e) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(format!("无法监听 {addr}: {e}"));
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

// ============================================================ FR-16 单实例探测

/// 生效的管理台地址 (FR-13): 显式 --addr 优先, 否则 fleet.json [manager] 恢复值,
/// 都没有/不合法 → CLI 地址。run_manager_with 的 bind 与 FR-16 单实例探测
/// (bind 失败后判别占用者) 共用本函数, 保证探测目标 == 尝试绑定的目标。
pub fn effective_control_addr(
    fleet_path: Option<&Path>,
    cli_addr: SocketAddr,
    addr_explicit: bool,
) -> SocketAddr {
    let persisted = fleet_path
        .and_then(|p| load_fleet_file(p).ok())
        .and_then(|f| f.manager)
        .and_then(|m| m.addr.trim().parse::<SocketAddr>().ok());
    match (addr_explicit, persisted) {
        (false, Some(a)) => a,
        _ => cli_addr,
    }
}

/// FR-16 探测结论 (bind 失败时目标端口上跑的是谁)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceProbe {
    /// 另一个 SerialHub 在跑 → 引导用户去既有管理台 (0 退出, 友好分支)
    AlreadyRunning,
    /// 其他程序占用 / 不可达 → 维持既有错误语义 (错误框 / exit 1)
    NotOurs,
}

/// FR-16 纯判别: 另一 SerialHub 的 /api/status 响应形状。
/// 主判据 = 响应体含 `"phase"` 字段 (spec FR-16; 恰好一桥时 status_json 必有);
/// 辅判据 = 无桥/多桥时控制面的 409 文案「当前不是单桥模式」(legacy_unavailable)
/// —— 同为本程序签名, 缺了它建过多座桥的实例会被误判成"其他程序占用"。
/// 带引号的字段名 + 中文专句, 其他软件极难撞上; 误判代价也只是换一种提示框。
fn status_body_is_serialhub(body: &str) -> bool {
    body.contains("\"phase\"") || body.contains("当前不是单桥模式")
}

/// FR-16 纯判别入口: HTTP 探测结果 (响应体; None = 连接失败/超时/读失败)
/// 收敛到两态。三分支 (SerialHub 响应/非 SerialHub 响应/超时) 单测见 probe_tests。
fn classify_instance_probe(resp: Option<&str>) -> InstanceProbe {
    match resp {
        Some(body) if status_body_is_serialhub(body) => InstanceProbe::AlreadyRunning,
        _ => InstanceProbe::NotOurs,
    }
}

/// FR-16: 对 `<addr>/api/status` 发一次最小 HTTP GET, 成功返回响应体。
/// 手写而不引 HTTP 客户端依赖: 一个端点一次 GET, std TcpStream 足矣;
/// 连接与读各限 1s (调用点在 bind 失败路径上, 不能久等)。
/// 必须用 std 同步栈: 调用点是 GUI 主线程 / headless 收尾, 不在 tokio 上下文里。
fn http_get_status_body(addr: SocketAddr) -> Option<String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(1)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
    s.set_write_timeout(Some(Duration::from_secs(1))).ok()?;
    let req = format!(
        "GET /api/status HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nUser-Agent: serialhub-probe\r\n\r\n"
    );
    s.write_all(req.as_bytes()).ok()?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).ok()?;
    let text = String::from_utf8_lossy(&raw);
    // 响应体 = 空行之后 (头/体分隔); 无分隔符 (响应不完整) 时按全文判别
    Some(match text.find("\r\n\r\n") {
        Some(i) => text[i + 4..].to_owned(),
        None => text.into_owned(),
    })
}

/// FR-16: 目标地址上是否跑着另一个 SerialHub。gui.rs (信息框) 与
/// main.rs (headless stderr) 的 bind 失败路径共用。
pub fn another_serialhub_running(addr: SocketAddr) -> bool {
    matches!(
        classify_instance_probe(http_get_status_body(addr).as_deref()),
        InstanceProbe::AlreadyRunning
    )
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    #[test]
    fn serialhub_status_body_is_detected() {
        // 恰好一桥时的 /api/status 形状 (hub.status_json 序列化, 字段名见 hub.rs 契约)
        let body = r#"{"phase":"closed","port":"","baud":115200,"config":"8N2","flow":"none","clients":0,"maxClients":0,"rxBytes":0,"txBytes":0,"lastError":null,"retries":0,"autoReconnect":true,"uptimeSec":1}"#;
        assert_eq!(
            classify_instance_probe(Some(body)),
            InstanceProbe::AlreadyRunning
        );
    }

    #[test]
    fn multi_bridge_409_body_is_detected() {
        // 无桥/多桥时控制面 409 (legacy_unavailable) —— 同为本程序签名 (FR-16 辅判据)
        let body = r#"{"ok":false,"error":"当前不是单桥模式, 旧单桥接口仅在恰好一座桥时可用 (见 GET /api/fleet)"}"#;
        assert_eq!(
            classify_instance_probe(Some(body)),
            InstanceProbe::AlreadyRunning
        );
    }

    #[test]
    fn other_program_body_is_not_detected() {
        // 非 SerialHub 的 JSON / HTML / 空体
        assert_eq!(
            classify_instance_probe(Some(r#"{"status":"ok","service":"nginx"}"#)),
            InstanceProbe::NotOurs
        );
        // 裸词 "phase" 不算 —— 必须是带引号的字段名
        assert_eq!(
            classify_instance_probe(Some("<html>phase</html>")),
            InstanceProbe::NotOurs
        );
        assert_eq!(classify_instance_probe(Some("")), InstanceProbe::NotOurs);
    }

    #[test]
    fn timeout_or_unreachable_is_not_detected() {
        // None = 连接失败/超时/读失败 → 维持既有错误语义, 不做友好引导
        assert_eq!(classify_instance_probe(None), InstanceProbe::NotOurs);
    }

    #[test]
    fn effective_addr_explicit_wins_then_fallback() {
        // FR-13/FR-16 共用: 显式 --addr 恒优先; 无清单/清单不可读 → CLI 地址。
        // (清单恢复值路径由 run_manager_with 生产路径覆盖; 此处锁定两条回退分支。)
        let cli: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        assert_eq!(effective_control_addr(None, cli, true), cli);
        assert_eq!(effective_control_addr(None, cli, false), cli);
        let missing = std::path::Path::new("Z:/definitely/not/exist/fleet.json");
        assert_eq!(effective_control_addr(Some(missing), cli, false), cli);
    }

    #[test]
    fn effective_addr_persisted_manager_wins_when_not_explicit() {
        // FR-13: 未显式给出 --addr 且清单 [manager] 有合法地址 → 用恢复值
        // (这也是 FR-16 探测目标必须走本函数的原因: bind 目标随清单漂移)
        let dir = std::env::temp_dir().join(format!("serialhub-probe-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fleet.json");
        std::fs::write(
            &path,
            r#"{"version":1,"manager":{"addr":"127.0.0.1:9155"},"bridges":[]}"#,
        )
        .unwrap();
        let cli: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let got = effective_control_addr(Some(&path), cli, false);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(got, "127.0.0.1:9155".parse::<SocketAddr>().unwrap());
        // 显式 --addr 时清单值让位
        assert_eq!(effective_control_addr(Some(&path), cli, true), cli);
    }
}

// ============================================================ 控制面 (FR-10g)

#[derive(Clone)]
struct ControlState {
    mgr: Arc<BridgeManager>,
    shutdown_tx: watch::Sender<bool>,
    restart_to: Arc<Mutex<Option<String>>>,
    index: &'static str,
    /// FR-14: 主题目录 (每轮 serve 重建 ControlState 时随 mgr 携带)。
    themes_dir: PathBuf,
}

fn control_router(cs: ControlState) -> Router {
    Router::new()
        .route("/", get(control_index))
        // ---- FR-10g fleet API ----
        .route("/api/fleet", get(fleet_list).post(fleet_create))
        .route("/api/fleet/{id}", get(fleet_detail))
        .route("/api/fleet/{id}/start", post(fleet_start))
        .route("/api/fleet/{id}/stop", post(fleet_stop))
        .route("/api/fleet/{id}/delete", post(fleet_delete))
        .route(
            "/api/fleet/{id}/config",
            patch(fleet_config).post(fleet_config),
        )
        .route("/api/fleet/{id}/tap", get(fleet_tap))
        // ---- FR-13 管理台设置: 控制面地址原地换绑 (复用 ADR-12 机制) ----
        .route("/api/manager/addr", post(manager_set_addr))
        // ---- FR-14 主题插件 ----
        .route("/api/themes", get(themes_list))
        .route("/themes/{file}", get(themes_file))
        // ---- FR-15 网页 favicon ----
        .route("/favicon.svg", get(control_favicon))
        // ---- 旧单桥端点 (恰一座桥时兼容分发, FR-10h) ----
        .route("/api/status", get(legacy_status))
        .route("/api/ports", get(legacy_ports))
        .route("/api/config", post(legacy_config))
        .route("/api/open", post(legacy_open))
        .route("/api/close", post(legacy_close))
        .route("/api/shutdown", post(legacy_shutdown))
        .route("/api/restart", post(legacy_restart))
        .route("/ws", get(legacy_ws))
        .with_state(cs)
}

async fn control_index(State(cs): State<ControlState>) -> Html<&'static str> {
    Html(cs.index)
}

fn not_found_bridge(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"ok": false, "error": format!("桥不存在: {id}")})),
    )
        .into_response()
}

/// 旧单桥端点在非"恰一座桥"时的统一拒绝 (409)。
fn legacy_unavailable() -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({"ok": false, "error": "当前不是单桥模式, 旧单桥接口仅在恰好一座桥时可用 (见 GET /api/fleet)"})),
    )
        .into_response()
}

// ---- FR-10g: fleet CRUD ----

async fn fleet_list(State(cs): State<ControlState>) -> Response {
    let mut bridges = cs.mgr.snapshot();
    bridges.sort_by_key(|b| b.id.trim_start_matches('b').parse::<u64>().unwrap_or(0));
    Json(json!({
        "ok": true,
        "bridges": bridges.iter().map(|b| b.detail_json()).collect::<Vec<_>>(),
    }))
    .into_response()
}

async fn fleet_detail(State(cs): State<ControlState>, AxumPath(id): AxumPath<String>) -> Response {
    match cs.mgr.get(&id) {
        Some(b) => Json(json!({"ok": true, "bridge": b.detail_json()})).into_response(),
        None => not_found_bridge(&id),
    }
}

#[derive(Deserialize)]
struct SerialReq {
    #[serde(default)]
    port: Option<String>,
    #[serde(default)]
    baud: Option<u32>,
    #[serde(rename = "dataBits", default)]
    data_bits: Option<u8>,
    #[serde(default)]
    parity: Option<String>,
    #[serde(rename = "stopBits", default)]
    stop_bits: Option<u8>,
    #[serde(default)]
    flow: Option<String>,
}

#[derive(Deserialize)]
struct FleetCreateReq {
    #[serde(default)]
    name: String,
    #[serde(default)]
    serial: Option<SerialReq>,
    #[serde(default)]
    listen: Option<String>,
    #[serde(rename = "autoOpen", default)]
    auto_open: Option<bool>,
    /// FR-12/ADR-16①: 自动重连开关 (缺省 true)。
    #[serde(rename = "autoReconnect", default)]
    auto_reconnect: Option<bool>,
    #[serde(rename = "maxClients", default)]
    max_clients: Option<u32>,
}

async fn fleet_create(
    State(cs): State<ControlState>,
    body: Result<Json<FleetCreateReq>, JsonRejection>,
) -> Response {
    let Json(req) = match body {
        Ok(b) => b,
        Err(rej) => return api::bad(format!("请求体不是合法 JSON: {rej}")),
    };
    let mut serial = SerialConfig::default();
    if let Some(s) = &req.serial {
        if let Some(p) = &s.port {
            serial.port = p.trim().to_string();
        }
        if let Some(b) = s.baud {
            serial.baud = b;
        }
        if let Some(d) = s.data_bits {
            serial.data_bits = d;
        }
        if let Some(p) = &s.parity {
            match Parity::parse(p) {
                Ok(v) => serial.parity = v,
                Err(e) => return api::bad(e),
            }
        }
        if let Some(sb) = s.stop_bits {
            serial.stop_bits = sb;
        }
        if let Some(f) = &s.flow {
            match Flow::parse(f) {
                Ok(v) => serial.flow = v,
                Err(e) => return api::bad(e),
            }
        }
    }
    if let Err(e) = serial.validate() {
        return api::bad(e);
    }
    let listen = match &req.listen {
        Some(l) => match parse_listen(l) {
            Ok(a) => a,
            Err(e) => return api::bad(e),
        },
        None => SocketAddr::from(([127, 0, 0, 1], 0)),
    };
    let spec = BridgeSpec {
        id: None,
        name: req.name,
        serial,
        listen,
        auto_open: req.auto_open.unwrap_or(true),
        auto_reconnect: req.auto_reconnect.unwrap_or(true), // FR-12: 缺省 true
        max_clients: req.max_clients.unwrap_or(0),
    };
    match cs.mgr.create_bridge(spec).await {
        Ok(b) => {
            let id = b.id.clone();
            let listen = b.listen_addr().to_string();
            println!("serialhub: 新建桥 {id} ({}) → 数据端口 {listen}", b.name());
            Json(json!({"ok": true, "id": id, "listen": listen})).into_response()
        }
        Err(e) => api::bad(e),
    }
}

async fn fleet_start(State(cs): State<ControlState>, AxumPath(id): AxumPath<String>) -> Response {
    if cs.mgr.get(&id).is_none() {
        return not_found_bridge(&id);
    }
    match cs.mgr.start_bridge(&id).await {
        Ok(()) => api::ok(),
        Err(e) => api::bad(e),
    }
}

async fn fleet_stop(State(cs): State<ControlState>, AxumPath(id): AxumPath<String>) -> Response {
    if cs.mgr.get(&id).is_none() {
        return not_found_bridge(&id);
    }
    match cs.mgr.stop_bridge(&id).await {
        Ok(()) => api::ok(),
        Err(e) => api::bad(e),
    }
}

async fn fleet_delete(State(cs): State<ControlState>, AxumPath(id): AxumPath<String>) -> Response {
    match cs.mgr.delete_bridge(&id).await {
        Ok(()) => api::ok(),
        Err(e) if e.starts_with("桥不存在") => not_found_bridge(&id),
        Err(e) => api::bad(e),
    }
}

#[derive(Deserialize)]
struct FleetConfigReq {
    #[serde(default)]
    name: Option<String>,
    /// 仅用于显式拒绝: 桥数据端口生命周期内不变 (FR-10f)。
    #[serde(default)]
    listen: Option<String>,
    #[serde(default)]
    serial: Option<SerialReq>,
    // 扁平字段 (与 legacy /api/config 同构; 与嵌套 serial{} 二者皆可, 嵌套优先)
    #[serde(default)]
    port: Option<String>,
    #[serde(default)]
    baud: Option<u32>,
    #[serde(rename = "dataBits", default)]
    data_bits: Option<u8>,
    #[serde(default)]
    parity: Option<String>,
    #[serde(rename = "stopBits", default)]
    stop_bits: Option<u8>,
    #[serde(default)]
    flow: Option<String>,
    #[serde(rename = "maxClients", default)]
    max_clients: Option<u32>,
    #[serde(rename = "autoOpen", default)]
    auto_open: Option<bool>,
    /// FR-12/ADR-16①: 自动重连开关 (缺省 = 不改动)。
    #[serde(rename = "autoReconnect", default)]
    auto_reconnect: Option<bool>,
}

async fn fleet_config(
    State(cs): State<ControlState>,
    AxumPath(id): AxumPath<String>,
    body: Result<Json<FleetConfigReq>, JsonRejection>,
) -> Response {
    if cs.mgr.get(&id).is_none() {
        return not_found_bridge(&id);
    }
    let Json(req) = match body {
        Ok(v) => v,
        Err(rej) => return api::bad(format!("请求体不是合法 JSON: {rej}")),
    };
    if req.listen.is_some() {
        return api::bad("桥数据端口生命周期内不变 (FR-10f): 不支持修改 listen".into());
    }
    let mut cr = api::ConfigReq {
        port: req.port,
        baud: req.baud,
        data_bits: req.data_bits,
        parity: req.parity,
        stop_bits: req.stop_bits,
        flow: req.flow,
        max_clients: req.max_clients,
        auto_reconnect: req.auto_reconnect, // FR-12: 经 apply_config_core 即改即生效
    };
    if let Some(s) = req.serial {
        cr.port = s.port.or(cr.port);
        cr.baud = s.baud.or(cr.baud);
        cr.data_bits = s.data_bits.or(cr.data_bits);
        cr.parity = s.parity.or(cr.parity);
        cr.stop_bits = s.stop_bits.or(cr.stop_bits);
        cr.flow = s.flow.or(cr.flow);
    }
    cs.mgr.config_bridge(&id, req.name, req.auto_open, cr)
}

// ---- FR-10g: tap (旁看串口 RX 原始字节, 只读, 不计 clients) ----

async fn fleet_tap(
    ws: WebSocketUpgrade,
    State(cs): State<ControlState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let Some(b) = cs.mgr.get(&id) else {
        return not_found_bridge(&id);
    };
    ws.on_upgrade(move |socket| tap_loop(socket, b))
}

async fn tap_loop(socket: WebSocket, b: Arc<Bridge>) {
    let (mut sink, mut stream) = socket.split();
    let mut bsub = b.ctx.bc_tx.subscribe();
    let mut sd = b.stop_tx.subscribe();
    loop {
        tokio::select! {
            // 下行: 串口 RX 原始字节 (broadcast 与数据面同源)
            frame = bsub.recv() => match frame {
                Ok(bytes) => {
                    if sink.send(Message::Binary(bytes.into())).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(_)) => continue, // 慢侦听者丢旧帧
                Err(RecvError::Closed) => break,
            },
            // 桥停/删/进程退出 → 断开
            _ = sd.changed() => break,
            // 只读: 入站帧一律忽略 (不进 tx 队列, 不影响数据面)
            msg = stream.next() => match msg {
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
        }
    }
}

// ---- FR-13: 管理台设置 —— 控制面地址原地换绑 ----

/// POST /api/manager/addr {"addr":"ip:port"}: 校验并登记新地址, serve 循环
/// 检测到登记后退出当前 TCP 面 → 原地重新 bind (失败 2s 重试后回退原地址)。
/// 校验/登记复用 api::restart_core 同一路径 (ADR-12); 成功受理后:
/// 持久化 [manager] 段 (重启恢复) + Ready(新地址) 事件 (壳层 webview 跟随)。
/// 各桥数据面全程不动 (换绑只影响管理台)。
async fn manager_set_addr(
    State(cs): State<ControlState>,
    body: Result<Json<api::RestartReq>, JsonRejection>,
) -> Response {
    api::restart_core(&cs.restart_to, body)
}

// ---- FR-14: 主题插件 ----

/// GET /api/themes: {"themes":[{"name":"light","builtin":true},...]}
/// (扫主题目录, 文件名去后缀, 字典序)。
async fn themes_list(State(cs): State<ControlState>) -> Response {
    Json(json!({ "themes": crate::themes::scan(&cs.themes_dir) })).into_response()
}

/// GET /themes/<file>.css: 主题静态服务 (路径穿越防护见 themes::resolve)。
async fn themes_file(State(cs): State<ControlState>, AxumPath(file): AxumPath<String>) -> Response {
    crate::themes::serve(&cs.themes_dir, &file)
}

// ---- FR-15: 网页 favicon ----

async fn control_favicon(State(_cs): State<ControlState>) -> Response {
    api::favicon_core()
}

// ---- 旧单桥端点兼容分发 (FR-10h) ----

async fn legacy_status(State(cs): State<ControlState>) -> Response {
    match cs.mgr.single_bridge() {
        Some(b) => api::status_core(&b.ctx),
        None => legacy_unavailable(),
    }
}

async fn legacy_ports(State(_cs): State<ControlState>) -> Response {
    api::ports_core() // 全局端点, 不依赖桥
}

async fn legacy_config(
    State(cs): State<ControlState>,
    body: Result<Json<api::ConfigReq>, JsonRejection>,
) -> Response {
    let Some(b) = cs.mgr.single_bridge() else {
        return legacy_unavailable();
    };
    let resp = match body {
        Ok(Json(req)) => api::apply_config_core(&b.ctx, req),
        Err(rej) => api::bad(format!("请求体不是合法 JSON: {rej}")),
    };
    if resp.status().is_success() {
        cs.mgr.persist(); // 兼容桥的串口配置变更同样进 fleet
    }
    resp
}

async fn legacy_open(State(cs): State<ControlState>) -> Response {
    match cs.mgr.single_bridge() {
        Some(b) => api::open_core(&b.ctx, &b.cmd_tx),
        None => legacy_unavailable(),
    }
}

async fn legacy_close(State(cs): State<ControlState>) -> Response {
    match cs.mgr.single_bridge() {
        Some(b) => api::close_core(&b.cmd_tx),
        None => legacy_unavailable(),
    }
}

async fn legacy_shutdown(State(cs): State<ControlState>) -> Response {
    api::shutdown_core(&cs.shutdown_tx) // 停整进程: 管理台 + 全部桥
}

async fn legacy_restart(
    State(cs): State<ControlState>,
    body: Result<Json<api::RestartReq>, JsonRejection>,
) -> Response {
    // ADR-12 语义延续: 管理台原地换绑 (串口面/各桥数据面不动)
    api::restart_core(&cs.restart_to, body)
}

async fn legacy_ws(ws: WebSocketUpgrade, State(cs): State<ControlState>) -> Response {
    match cs.mgr.single_bridge() {
        Some(b) => ws.on_upgrade(move |socket| {
            api::client_loop(
                socket,
                App {
                    ctx: b.ctx.clone(),
                    cmd_tx: b.cmd_tx.clone(),
                    shutdown_tx: b.stop_tx.clone(), // 桥停 → 本 WS 断 (优雅停机不被长连接卡住)
                    restart_to: Arc::new(Mutex::new(None)),
                    index: "",
                },
            )
        }),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ok": false, "error": "当前不是单桥模式, 数据面请连各桥的 ws://<listen>/ws"})),
        )
            .into_response(),
    }
}

// ============================================================ 运行循环

/// headless/GUI 共用入口: 构造管理器后进入运行循环。
pub async fn run_manager(
    su: ManagerStartup,
    _cmd_tx: UnboundedSender<HubCmd>,
    cmd_rx: UnboundedReceiver<HubCmd>,
    shutdown_tx: watch::Sender<bool>,
    on_event: Option<OnEvent>,
) -> Result<(), String> {
    let mgr = Arc::new(BridgeManager::new(su.fleet_path.clone()));
    run_manager_with(mgr, su, cmd_rx, shutdown_tx, on_event).await
}

/// 可注入已构造好的管理器 (测试持有 Arc 以直查桥内部件)。
/// su.fleet_path 在此路径下被忽略 (以 mgr.fleet_path 为准)。
pub async fn run_manager_with(
    mgr: Arc<BridgeManager>,
    su: ManagerStartup,
    cmd_rx: UnboundedReceiver<HubCmd>,
    shutdown_tx: watch::Sender<bool>,
    on_event: Option<OnEvent>,
) -> Result<(), String> {
    // watch 接收端必须在 Ready 事件之前创建 (与 run_service 同理: 防晚订阅漏停机;
    // fleet 恢复/兼容桥建桥可能耗时, 竞窗更大) —— 控制面首轮 serve 共用此接收端。
    let mut main_sd = shutdown_tx.subscribe();
    let mut first_serve_sd: Option<watch::Receiver<bool>> = Some(shutdown_tx.subscribe());

    // 1) 控制面绑定 (FR-10a: 管理台固定地址, 永远可达; bind 失败重试 ≤2s → 致命)。
    //    FR-13: fleet.json [manager] addr 持久化恢复 —— 显式 --addr 优先;
    //    未显式给出且清单里有合法地址 → 用恢复值 (改过端口的用户重启后回到原地址)。
    //    解析逻辑抽成 effective_control_addr: FR-16 单实例探测与 bind 共用同一目标。
    let control_addr =
        effective_control_addr(mgr.fleet_path.as_deref(), su.control_addr, su.addr_explicit);
    let mut listener = bind_with_retry(control_addr, Duration::from_secs(2))
        .await
        .map_err(|e| format!("管理台端口被占用: {e}"))?;
    let addr = listener
        .local_addr()
        .map_err(|e| format!("获取监听地址失败: {e}"))?;
    if let Some(cb) = &on_event {
        cb(ServiceEvent::Ready(addr));
    }
    println!("SerialHub 管理台就绪: http://{addr} (多桥管理器)");
    mgr.set_manager_addr(addr);

    // 2) fleet 恢复 (存在则恢复全部桥, FR-10b)
    let restored = restore_fleet(&mgr).await;

    // 3) CLI 兼容桥 (FR-10h): 显式 --port 必建; 无 --port 且无恢复桥时也建一座
    //    (空串口, 与旧"无参启动 = 控制台驱动"行为等价)
    if let Some(spec) = &su.cli_bridge {
        let explicit_port = !spec.serial.port.is_empty();
        if explicit_port || restored == 0 {
            let listen = pick_listen_near(addr).await;
            match mgr
                .create_bridge(BridgeSpec {
                    id: None,
                    name: spec.name.clone(),
                    serial: spec.serial.clone(),
                    listen,
                    auto_open: spec.auto_open && explicit_port,
                    auto_reconnect: spec.auto_reconnect,
                    max_clients: spec.max_clients,
                })
                .await
            {
                Ok(b) => println!(
                    "serialhub: 兼容桥 {} ({}) → 数据端口 {}",
                    b.id,
                    b.name(),
                    b.listen_addr()
                ),
                Err(e) => eprintln!("serialhub: 兼容桥创建失败: {e} (管理台继续可用)"),
            }
        }
    }

    // FR-13: 清单已存在时同步一次 [manager] 段 (现地址入档; 不新建文件 ——
    // 全新安装首次落盘仍以首次桥变更为准, 避免无谓写文件)
    if mgr.fleet_path.as_ref().map_or(false, |p| p.exists()) {
        mgr.persist();
    }

    // FR-14: 主题目录初始化 (默认 exe 旁 themes/; --themes-dir 可指定)。
    // 失败不致命: 管理台照常服务, 主题列表为空 (/themes 端点按现状 404)。
    let themes_dir = su.themes_dir.clone().unwrap_or_else(crate::themes::default_themes_dir);
    if let Err(e) = crate::themes::ensure_builtin(&themes_dir) {
        eprintln!("serialhub: 主题目录初始化失败 ({themes_dir:?}): {e} (FR-14)");
    }

    // 4) 后台任务: legacy 指令泵 / 统计采样 / Ctrl-C / 相位聚合
    {
        // legacy 指令泵: GUI 托盘的 打开/关闭串口 → 当前唯一桥 (兼容分发语义)
        let mgr = mgr.clone();
        tokio::spawn(async move {
            let mut rx = cmd_rx;
            while let Some(cmd) = rx.recv().await {
                if let Some(b) = mgr.single_bridge() {
                    let _ = b.cmd_tx.send(cmd);
                }
            }
        });
    }
    {
        // 统计引擎 (202): 200ms 采样各桥累计字节 → RateWindow 1s 滑窗
        let mgr = mgr.clone();
        let mut sd = shutdown_tx.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = sd.changed() => break,
                    _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                }
                let now = Instant::now();
                for b in mgr.snapshot() {
                    let s = b.hub.status_json();
                    lock_mutex(&b.rates).push(now, s.rx_bytes, s.tx_bytes);
                }
            }
        });
    }
    {
        let sd = shutdown_tx.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                let _ = sd.send(true);
            }
        });
    }
    if let Some(cb) = &on_event {
        // 相位聚合 → GUI 托盘 (150ms 轮询, 与 run_service 同节拍)
        let mgr = mgr.clone();
        let cb = cb.clone();
        tokio::spawn(async move {
            let mut last = (String::new(), String::new());
            loop {
                tokio::time::sleep(Duration::from_millis(150)).await;
                let snaps = mgr.snapshot();
                let mut phase = Phase::Closed;
                for b in &snaps {
                    let p = if b.is_running() {
                        b.hub.phase()
                    } else {
                        Phase::Closed
                    };
                    match p {
                        Phase::Open => {
                            phase = Phase::Open;
                            break;
                        }
                        Phase::Opening | Phase::Retry => {
                            if phase == Phase::Closed {
                                phase = p;
                            }
                        }
                        Phase::Closed => {}
                    }
                }
                let port = snaps
                    .first()
                    .map(|b| b.hub.config().port)
                    .unwrap_or_default();
                if (format!("{phase:?}"), port.clone()) != last {
                    last = (format!("{phase:?}"), port.clone());
                    cb(ServiceEvent::Phase(phase, port));
                }
            }
        });
    }

    // 5) 控制面 serve 循环 (watch 停机 / 原地换绑, 与 run_service 同构;
    //    换绑只影响管理台, 各桥数据面不动)。
    //    退出路径必须区分, 防止 JoinHandle 双重 poll (qa-sprint4-regression):
    //    - main_sd 先到: handle 未被 select 消费 → 交 finalize 宽限等待一次;
    //    - serve 臂先到: 结果已在此消费 (换址轮继续 / 自行结束退出),
    //      finalize 不得再重复 await 同一 handle, 否则 tokio panic
    //      "JoinHandle polled after completion"。
    let mut cur_addr = addr;
    let mut done_handle: Option<JoinHandle<Result<Option<()>, std::io::Error>>> = None;
    loop {
        let round_rebind: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let cs = ControlState {
            mgr: mgr.clone(),
            shutdown_tx: shutdown_tx.clone(),
            restart_to: round_rebind.clone(),
            index: include_str!("../ui/index.html"),
            themes_dir: themes_dir.clone(),
        };
        // 首轮用 Ready 前订阅好的接收端 (无晚订阅竞态); 换址轮重新订阅
        let mut serve_sd = first_serve_sd
            .take()
            .unwrap_or_else(|| shutdown_tx.subscribe());
        let rebind_watcher = round_rebind.clone();
        let mut handle = tokio::spawn(async move {
            let shutdown = async move {
                let _ = serve_sd.changed().await;
            };
            let rebinding = async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    if lock_mutex(&rebind_watcher).is_some() {
                        break;
                    }
                }
            };
            tokio::select! {
                r = axum::serve(listener, control_router(cs))
                    .with_graceful_shutdown(shutdown) => { r.map(|_| None) }
                _ = rebinding => { Ok(Some(())) } // 换址请求: 正常退出 serve
            }
        });

        let mut serve_out: Option<
            Result<Result<Option<()>, std::io::Error>, tokio::task::JoinError>,
        > = None;
        let mut shutdown_seen = false;
        tokio::select! {
            _ = main_sd.changed() => shutdown_seen = true, // 托盘退出 / POST /api/shutdown / Ctrl-C
            r = &mut handle => serve_out = Some(r),
        }
        if shutdown_seen {
            done_handle = Some(handle);
            break; // 真停机 (handle 由 finalize 宽限等待)
        }
        match serve_out.expect("serve 臂获胜时必有结果") {
            Ok(Ok(Some(()))) => {
                // 换址请求: handle 已消费, 原地换绑后继续下一轮
                // 先取值再离开锁: MutexGuard 不得跨 await (Send 约束)
                let new_addr = lock_mutex(&round_rebind).take();
                let Some(new_addr) = new_addr else { break };
                // restart_core 登记前已校验过 SocketAddr, parse 失败纯属防御 → 原地不动
                let parsed: SocketAddr = new_addr.parse().unwrap_or(cur_addr);
                let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
                let bound = loop {
                    match TcpListener::bind(parsed).await {
                        Ok(l) => break l,
                        Err(e) => {
                            if tokio::time::Instant::now() >= deadline {
                                eprintln!(
                                    "serialhub: 管理台换址失败 ({parsed}): {e} —— 保持原地址 {cur_addr} 继续服务"
                                );
                                break TcpListener::bind(cur_addr)
                                    .await
                                    .map_err(|e| format!("换址失败且原地址也绑定失败: {e}"))?;
                            }
                            tokio::time::sleep(Duration::from_millis(250)).await;
                        }
                    }
                };
                let bound_addr = bound
                    .local_addr()
                    .map_err(|e| format!("获取监听地址失败: {e}"))?;
                println!(
                    "serialhub: 管理台地址已切换 {cur_addr} -> {bound_addr} (各桥数据面不受影响)"
                );
                cur_addr = bound_addr;
                listener = bound;
                // FR-13: 新地址持久化 ([manager] 段, 重启恢复)。
                // 回退原地址时 bound_addr == cur_addr, 写入无害 (现状即真相)。
                mgr.set_manager_addr(cur_addr);
                mgr.persist();
                if let Some(cb) = &on_event {
                    cb(ServiceEvent::Ready(cur_addr)); // 壳层跟随新地址 (gui.rs → webview navigate)
                }
                continue;
            }
            Ok(Ok(None)) => break, // serve 自行结束 (异常), 按停机处理
            Ok(Err(e)) => return Err(format!("管理台服务错误: {e}")),
            Err(e) => return Err(format!("管理台服务任务异常: {e}")),
        }
    }
    finalize_manager(&mgr, done_handle, on_event).await
}

/// 停机宽限 (与 legacy finalize 同值): COM 释放 250ms + axum 1.25s。
const SHUTDOWN_GRACE: Duration = Duration::from_millis(1250);

async fn finalize_manager(
    mgr: &Arc<BridgeManager>,
    serve: Option<JoinHandle<Result<Option<()>, std::io::Error>>>,
    on_event: Option<OnEvent>,
) -> Result<(), String> {
    // COM 释放优先: 每桥停串口 + 断数据面 (watch 同时断开 legacy /ws 与各桥 /ws)
    for b in mgr.snapshot() {
        let _ = b.cmd_tx.send(HubCmd::Close);
        b.ctx.stop_active();
        let _ = b.stop_tx.send(true);
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    // 控制面有界宽限 (serve 已在循环内自行结束 → 无需等待)
    let (done, result) = match serve {
        Some(serve) => {
            // inner: Ok(Some(())) = 并发换址请求; Ok(None) = 优雅停机完成
            let outcome = tokio::time::timeout(SHUTDOWN_GRACE, serve).await;
            // inner: Ok(Some(())) = 并发换址请求 (停机优先); Ok(None) = 优雅停机完成
            let done = matches!(&outcome, Ok(Ok(Ok(_))));
            let result = match outcome {
                Ok(Ok(Ok(_))) => Ok(()),
                Ok(Ok(Err(e))) => Err(format!("管理台服务异常退出: {e}")),
                Ok(Err(e)) => Err(format!("管理台服务任务异常: {e:?}")),
                Err(_) => Err("优雅停机超时: 有界宽限耗尽仍有连接未退".into()),
            };
            (done, result)
        }
        None => (true, Ok(())),
    };
    // 数据面 listener 收尾 (COM 已释放, 此处只等 TCP 面退出)
    for b in mgr.snapshot() {
        let h = lock_mutex(&b.serve_handle).take();
        if let Some(h) = h {
            let _ = tokio::time::timeout(Duration::from_millis(500), h).await;
        }
    }
    if done {
        if let Some(cb) = &on_event {
            cb(ServiceEvent::Stopped);
        }
    }
    result
}

// ============================================================ 单元测试

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc as std_mpsc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    // ---------- 纯函数 ----------

    #[test]
    fn parse_listen_variants() {
        assert_eq!(
            parse_listen("8101").unwrap(),
            SocketAddr::from(([127, 0, 0, 1], 8101))
        );
        assert_eq!(
            parse_listen("127.0.0.1:8101").unwrap(),
            SocketAddr::from(([127, 0, 0, 1], 8101))
        );
        assert_eq!(
            parse_listen("[::1]:9000").unwrap(),
            "[::1]:9000".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(parse_listen("").unwrap().port(), 0);
        assert_eq!(parse_listen("  8102  ").unwrap().port(), 8102);
        assert!(parse_listen("bad").is_err());
        assert!(parse_listen("99999").is_err());
    }

    #[test]
    fn fleet_file_roundtrip() {
        let path = std::env::temp_dir().join(format!("sh_fleet_rt_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let recs = vec![FleetBridgeRec {
            id: "b1".into(),
            name: "主桥".into(),
            auto_open: true,
            auto_reconnect: false,
            max_clients: 4,
            listen: "127.0.0.1:8101".into(),
            serial: SerialRec {
                port: "COM1".into(),
                baud: 921_600,
                data_bits: 8,
                parity: "N".into(),
                stop_bits: 2,
                flow: "rtscts".into(),
            },
        }];
        save_fleet(&path, &recs, None).unwrap();
        let back = load_fleet(&path).unwrap();
        assert_eq!(back, recs);
        assert!(!back[0].auto_reconnect, "autoReconnect 原样往返 (FR-12)");
        let cfg = back[0].serial.to_config().unwrap();
        assert_eq!(cfg.port, "COM1");
        assert_eq!(cfg.baud, 921_600);
        assert_eq!(cfg.flow, Flow::RtsCts);
        assert_eq!(cfg.config_str(), "8N2");
        let _ = std::fs::remove_file(&path);
    }

    /// FR-13: [manager] 段往返 —— 写入/读取原样, 旧清单无该段 → None 兼容。
    #[test]
    fn fleet_file_manager_rec_roundtrip() {
        let path = std::env::temp_dir().join(format!("sh_fleet_mgr_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let recs: Vec<FleetBridgeRec> = Vec::new();
        // 带段写入
        save_fleet(
            &path,
            &recs,
            Some(&ManagerRec {
                addr: "127.0.0.1:8100".into(),
            }),
        )
        .unwrap();
        let f = load_fleet_file(&path).unwrap();
        assert_eq!(
            f.manager,
            Some(ManagerRec {
                addr: "127.0.0.1:8100".into()
            })
        );
        // 无段写入 → 读回 None (写出时空段跳过)
        save_fleet(&path, &recs, None).unwrap();
        let f = load_fleet_file(&path).unwrap();
        assert_eq!(f.manager, None);
        // 旧式清单 (无 manager 字段) 也能解析
        std::fs::write(&path, r#"{"version":1,"bridges":[]}"#).unwrap();
        let f = load_fleet_file(&path).unwrap();
        assert_eq!(f.manager, None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fleet_file_rejects_garbage() {
        let path = std::env::temp_dir().join(format!("sh_fleet_bad_{}.json", std::process::id()));
        std::fs::write(&path, "not json").unwrap();
        assert!(load_fleet(&path).is_err());
        let _ = std::fs::remove_file(&path);
        // 非法串口配置在 to_config 被拒
        let rec = FleetBridgeRec {
            id: "b1".into(),
            name: "x".into(),
            auto_open: false,
            auto_reconnect: true,
            max_clients: 0,
            listen: "127.0.0.1:8101".into(),
            serial: SerialRec {
                port: "COM1".into(),
                baud: 50, // 越界
                data_bits: 8,
                parity: "N".into(),
                stop_bits: 2,
                flow: "none".into(),
            },
        };
        assert!(rec.serial.to_config().is_err());
    }

    #[test]
    fn from_cli_maps_flags() {
        let cli = crate::cli::parse(&[]).unwrap();
        let ms = ManagerStartup::from_cli(&cli);
        assert_eq!(ms.control_addr.to_string(), "127.0.0.1:8080");
        assert!(ms.fleet_path.unwrap().ends_with("fleet.json"));
        let spec = ms.cli_bridge.unwrap();
        assert!(spec.serial.port.is_empty());
        assert!(!spec.auto_open); // 未给 --port 不自动打开 (FR-5)

        let args: Vec<String> = "--port COM1 --no-open --no-fleet"
            .split_whitespace()
            .map(String::from)
            .collect();
        let cli = crate::cli::parse(&args).unwrap();
        let ms = ManagerStartup::from_cli(&cli);
        assert!(ms.fleet_path.is_none(), "--no-fleet 关闭持久化");
        assert!(!ms.cli_bridge.unwrap().auto_open);

        let args: Vec<String> = "--port COM1 --fleet D:\\x\\a.json"
            .split_whitespace()
            .map(String::from)
            .collect();
        let cli = crate::cli::parse(&args).unwrap();
        let ms = ManagerStartup::from_cli(&cli);
        assert_eq!(ms.fleet_path.unwrap().to_string_lossy(), "D:\\x\\a.json");
        assert!(ms.cli_bridge.unwrap().auto_open);
    }

    // ---------- 运行中的管理器 (HTTP 端到端) ----------

    struct TestMgr {
        mgr: Arc<BridgeManager>,
        addr: SocketAddr,
        shutdown_tx: watch::Sender<bool>,
        handle: JoinHandle<Result<(), String>>,
        /// FR-14: 本实例的主题目录 (spawn_mgr 分配的唯一临时目录)。
        themes_dir: PathBuf,
        /// 全部 ServiceEvent 的时间线 (Ready 换址跟随/Phase/Stopped 断言用)。
        events: Arc<Mutex<Vec<ServiceEvent>>>,
    }

    impl TestMgr {
        async fn shutdown(self) {
            let _ = self.shutdown_tx.send(true);
            tokio::time::timeout(Duration::from_secs(5), self.handle)
                .await
                .expect("停机 5s 内未完成")
                .expect("run_manager_with 不应报错")
                .expect("停机结果应为 Ok");
        }
    }

    async fn spawn_mgr(fleet: Option<PathBuf>, cli: Option<CliBridgeSpec>) -> TestMgr {
        spawn_mgr_opt(fleet, cli, "127.0.0.1:0".parse().unwrap(), false).await
    }

    /// 可控启动: 控制面初值 + 是否显式 --addr (FR-13 恢复优先级测试用)。
    async fn spawn_mgr_opt(
        fleet: Option<PathBuf>,
        cli: Option<CliBridgeSpec>,
        control_addr: SocketAddr,
        addr_explicit: bool,
    ) -> TestMgr {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        // 每实例唯一临时主题目录 (FR-14), 避免并行测试互写同一落盘目录
        let themes_dir = std::env::temp_dir().join(format!(
            "sh_themes_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let mgr = Arc::new(BridgeManager::new(fleet));
        let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<HubCmd>();
        let (shutdown_tx, _) = watch::channel(false);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<SocketAddr>();
        let ready_tx = Arc::new(Mutex::new(Some(ready_tx)));
        let events: Arc<Mutex<Vec<ServiceEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let ev_log = events.clone();
        let on_event: OnEvent = Arc::new(move |ev: ServiceEvent| {
            ev_log.lock().unwrap().push(ev.clone());
            if let ServiceEvent::Ready(a) = ev {
                if let Some(tx) = ready_tx.lock().unwrap().take() {
                    let _ = tx.send(a);
                }
            }
        });
        let su = ManagerStartup {
            control_addr,
            addr_explicit,
            themes_dir: Some(themes_dir.clone()),
            cli_bridge: cli,
            fleet_path: None, // 直接注入 mgr 时以 mgr.fleet_path 为准
        };
        let handle = tokio::spawn(run_manager_with(
            mgr.clone(),
            su,
            cmd_rx,
            shutdown_tx.clone(),
            Some(on_event),
        ));
        let addr = tokio::time::timeout(Duration::from_secs(5), ready_rx)
            .await
            .expect("管理台 5s 内未就绪")
            .expect("Ready 事件缺失");
        TestMgr {
            mgr,
            addr,
            shutdown_tx,
            handle,
            themes_dir,
            events,
        }
    }

    async fn http_req(
        addr: SocketAddr,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> (u16, String) {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let req = match body {
            Some(b) => format!(
                "{method} {path} HTTP/1.1\r\nHost: t\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
                b.len()
            ),
            None => format!("{method} {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n"),
        };
        s.write_all(req.as_bytes()).await.unwrap();
        let mut resp = Vec::new();
        s.read_to_end(&mut resp).await.unwrap();
        let text = String::from_utf8_lossy(&resp).to_string();
        let code: u16 = text
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let body = text
            .split_once("\r\n\r\n")
            .map(|(_, b)| b.to_string())
            .unwrap_or_default();
        (code, body)
    }

    async fn ws_handshake(addr: SocketAddr, path: &str) -> TcpStream {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: t\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = [0u8; 1024];
        let n = s.read(&mut buf).await.unwrap();
        let head = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(
            head.starts_with("HTTP/1.1 101"),
            "WS 握手失败 ({path}): {head}"
        );
        s
    }

    /// 客户端 → 服务端帧必须带掩码 (RFC 6455)
    async fn ws_send_masked(s: &mut TcpStream, payload: &[u8]) {
        let mut frame = vec![0x82u8]; // FIN + binary
        let n = payload.len();
        if n < 126 {
            frame.push(0x80 | n as u8);
        } else {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(n as u16).to_be_bytes());
        }
        let mask = [0x11u8, 0x22, 0x33, 0x44];
        frame.extend_from_slice(&mask);
        for (i, b) in payload.iter().enumerate() {
            frame.push(b ^ mask[i % 4]);
        }
        s.write_all(&frame).await.unwrap();
    }

    /// 读一个服务端二进制帧 (服务端帧不带掩码)
    async fn ws_recv_binary(s: &mut TcpStream, timeout: Duration) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            if buf.len() >= 2 {
                let op = buf[0] & 0x0f;
                assert_ne!(op, 8, "意外收到 Close 帧: {buf:?}");
                let len7 = (buf[1] & 0x7f) as usize;
                let (hdr, plen) = if len7 < 126 {
                    (2usize, len7)
                } else if len7 == 126 {
                    (4usize, u16::from_be_bytes([buf[2], buf[3]]) as usize)
                } else {
                    (10usize, 0usize) // 64KB+ 帧, 测试用不到
                };
                if plen > 0 && buf.len() >= hdr + plen && op == 2 {
                    return buf[hdr..hdr + plen].to_vec();
                }
            }
            let n = tokio::time::timeout(timeout, s.read(&mut tmp))
                .await
                .expect("等待 WS 帧超时")
                .unwrap();
            assert!(n > 0, "连接被对端关闭");
            buf.extend_from_slice(&tmp[..n]);
        }
    }

    async fn wait_connectable(addr: SocketAddr, want: bool, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let ok = TcpStream::connect(addr).await.is_ok();
            if ok == want {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn temp_fleet(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("sh_fleet_{}_{}.json", std::process::id(), tag));
        let _ = std::fs::remove_file(&p);
        p
    }

    async fn fleet_create_ok(addr: SocketAddr, body: &str) -> Value {
        let (code, resp) = http_req(addr, "POST", "/api/fleet", Some(body)).await;
        assert_eq!(code, 200, "建桥应成功: {resp}");
        serde_json::from_str(&resp).unwrap()
    }

    /// 兼容桥在 Ready 事件之后才创建, 测试需轮询等待它入队。
    async fn wait_single_bridge(mgr: &BridgeManager, timeout: Duration) -> Arc<Bridge> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(b) = mgr.single_bridge() {
                return b;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "等待兼容桥超时"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// WS 握手返回时, 服务端的 broadcast 订阅可能尚未建立 (升级任务尚未跑完);
    /// 注入 RX 前等至少一个订阅者, 消除 SendError 竞态。
    async fn wait_receiver(bc: &tokio::sync::broadcast::Sender<Vec<u8>>, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        while bc.receiver_count() == 0 {
            assert!(tokio::time::Instant::now() < deadline, "等待 WS 订阅超时");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    // ---- CRUD / 端口冲突 / 端点稳定 (FR-10b/f/g) ----

    #[tokio::test(flavor = "multi_thread")]
    async fn fleet_crud_and_port_conflict() {
        let t = spawn_mgr(None, None).await;
        // 新建 b1 (随机端口) → 回填实际端口
        let v = fleet_create_ok(
            t.addr,
            r#"{"name":"一桥","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        assert_eq!(v["id"], "b1");
        let (_, body) = http_req(t.addr, "GET", "/api/fleet", None).await;
        let v: Value = serde_json::from_str(&body).unwrap();
        let listen1 = v["bridges"][0]["listen"].as_str().unwrap().to_string();
        assert!(!listen1.ends_with(":0"), "应回填实际端口: {listen1}");
        // 端口冲突: 同端口再建 → 400
        let (code, resp) = http_req(
            t.addr,
            "POST",
            "/api/fleet",
            Some(&format!(r#"{{"name":"二桥","listen":"{listen1}"}}"#)),
        )
        .await;
        assert_eq!(code, 400, "同端口建桥应被拒: {resp}");
        assert!(resp.contains("冲突"), "错误应说明端口冲突: {resp}");
        // 详情 404
        let (code, _) = http_req(t.addr, "GET", "/api/fleet/b9", None).await;
        assert_eq!(code, 404);
        // stop → 端口关闭; start → 同一端口恢复 (FR-10f)
        let p1: SocketAddr = listen1.parse().unwrap();
        let (code, _) = http_req(t.addr, "POST", "/api/fleet/b1/stop", None).await;
        assert_eq!(code, 200);
        assert!(
            wait_connectable(p1, false, Duration::from_secs(3)).await,
            "stop 后数据端口应关闭"
        );
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["bridge"]["phase"], "closed"); // 停止态 = hub 四态 (QA 契约 stop→closed)
        let (code, _) = http_req(t.addr, "POST", "/api/fleet/b1/start", None).await;
        assert_eq!(code, 200);
        assert!(
            wait_connectable(p1, true, Duration::from_secs(3)).await,
            "start 后数据端口应恢复"
        );
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(
            v["bridge"]["listen"].as_str().unwrap(),
            listen1,
            "端口生命周期内不变 (FR-10f)"
        );
        // start 幂等
        let (code, _) = http_req(t.addr, "POST", "/api/fleet/b1/start", None).await;
        assert_eq!(code, 200);
        // delete → 列表空 + 详情 404 + stop 404
        let (code, _) = http_req(t.addr, "POST", "/api/fleet/b1/delete", None).await;
        assert_eq!(code, 200);
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["bridges"].as_array().unwrap().len(), 0);
        let (code, _) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        assert_eq!(code, 404);
        let (code, _) = http_req(t.addr, "POST", "/api/fleet/b1/stop", None).await;
        assert_eq!(code, 404);
        t.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fleet_config_updates_and_persists() {
        let path = temp_fleet("cfg");
        let t = spawn_mgr(Some(path.clone()), None).await;
        let v = fleet_create_ok(
            t.addr,
            r#"{"name":"一桥","serial":{"port":"COM1","baud":115200},"listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        assert_eq!(v["id"], "b1");
        // 建桥即持久化
        let recs = load_fleet(&path).unwrap();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].serial.baud, 115200);
        // PATCH 改配 (嵌套 serial{})
        let (code, resp) = http_req(
            t.addr,
            "PATCH",
            "/api/fleet/b1/config",
            Some(r#"{"serial":{"baud":921600,"parity":"E","stopBits":1}}"#),
        )
        .await;
        assert_eq!(code, 200, "{resp}");
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["bridge"]["serial"]["baud"], 921600);
        assert_eq!(v["bridge"]["serial"]["parity"], "E");
        assert_eq!(v["bridge"]["serial"]["stopBits"], 1);
        let recs = load_fleet(&path).unwrap();
        assert_eq!(recs[0].serial.baud, 921_600, "改配即持久化");
        // 扁平字段等价 (POST) + name/maxClients
        let (code, _) = http_req(
            t.addr,
            "POST",
            "/api/fleet/b1/config",
            Some(r#"{"baud":115200,"maxClients":3,"name":"改名"}"#),
        )
        .await;
        assert_eq!(code, 200);
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["bridge"]["serial"]["baud"], 115200);
        assert_eq!(v["bridge"]["name"], "改名");
        assert_eq!(v["bridge"]["maxClients"], 3);
        // 非法配置整体拒绝, 文件不变
        let (code, _) = http_req(t.addr, "POST", "/api/fleet/b1/config", Some(r#"{"baud":50}"#)).await;
        assert_eq!(code, 400);
        let recs = load_fleet(&path).unwrap();
        assert_eq!(recs[0].serial.baud, 115_200);
        // listen 不可改 (FR-10f)
        let (code, resp) = http_req(
            t.addr,
            "POST",
            "/api/fleet/b1/config",
            Some(r#"{"listen":"127.0.0.1:9999"}"#),
        )
        .await;
        assert_eq!(code, 400, "{resp}");
        // 非法校验位
        let (code, _) = http_req(
            t.addr,
            "POST",
            "/api/fleet/b1/config",
            Some(r#"{"parity":"M"}"#),
        )
        .await;
        assert_eq!(code, 400);
        t.shutdown().await;
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fleet_delete_updates_persisted_file() {
        let path = temp_fleet("del");
        let t = spawn_mgr(Some(path.clone()), None).await;
        fleet_create_ok(t.addr, r#"{"name":"一","listen":"127.0.0.1:0","autoOpen":false}"#).await;
        assert_eq!(load_fleet(&path).unwrap().len(), 1);
        let (code, _) = http_req(t.addr, "POST", "/api/fleet/b1/delete", None).await;
        assert_eq!(code, 200);
        let recs = load_fleet(&path).unwrap();
        assert!(recs.is_empty(), "删除即持久化空清单");
        t.shutdown().await;
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fleet_restore_from_file_and_continue_ids() {
        let path = temp_fleet("restore");
        let recs = vec![
            FleetBridgeRec {
                id: "b1".into(),
                name: "一号".into(),
                auto_open: false,
                auto_reconnect: false,
                max_clients: 0,
                listen: "127.0.0.1:0".into(),
                serial: SerialRec {
                    port: "COM9".into(),
                    baud: 115200,
                    data_bits: 8,
                    parity: "N".into(),
                    stop_bits: 2,
                    flow: "none".into(),
                },
            },
            FleetBridgeRec {
                id: "b2".into(),
                name: "二号".into(),
                auto_open: false,
                auto_reconnect: true,
                max_clients: 2,
                listen: "127.0.0.1:0".into(),
                serial: SerialRec {
                    port: "COM8".into(), // 仅配置, autoOpen=false 不真开
                    baud: 9600,
                    data_bits: 7,
                    parity: "E".into(),
                    stop_bits: 1,
                    flow: "none".into(),
                },
            },
        ];
        save_fleet(&path, &recs, None).unwrap();
        let t = spawn_mgr(Some(path.clone()), None).await;
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        let arr = v["bridges"].as_array().unwrap();
        assert_eq!(arr.len(), 2, "启动时应恢复全部桥");
        assert_eq!(arr[0]["id"], "b1");
        assert_eq!(arr[0]["name"], "一号");
        assert_eq!(arr[0]["serial"]["port"], "COM9");
        assert!(arr[0]["listen"].as_str().unwrap() != "127.0.0.1:0", "应回填实际端口");
        assert_eq!(arr[1]["serial"]["baud"], 9600);
        assert_eq!(arr[1]["maxClients"], 2);
        // FR-12: autoReconnect 随清单恢复, 不丢
        assert_eq!(arr[0]["autoReconnect"], false, "恢复后回显 false");
        assert_eq!(arr[1]["autoReconnect"], true, "恢复后回显 true");
        // 新建桥 id 续号 (b3)
        let v = fleet_create_ok(
            t.addr,
            r#"{"name":"三号","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        assert_eq!(v["id"], "b3");
        t.shutdown().await;
        let _ = std::fs::remove_file(&path);
    }

    // ---- 兼容分发 (FR-10h) ----

    fn compat_spec() -> CliBridgeSpec {
        CliBridgeSpec {
            name: "CLI".into(),
            serial: SerialConfig::default(),
            auto_open: false,
            auto_reconnect: true,
            max_clients: 0,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn compat_single_bridge_dispatch() {
        let t = spawn_mgr(None, Some(compat_spec())).await;
        // 恰一座桥: 旧端点直调那座桥
        let (code, resp) = http_req(t.addr, "GET", "/api/status", None).await;
        assert_eq!(code, 200, "{resp}");
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["phase"], "closed");
        // open (空串口) → open_core 语义原样透传 (400 + 提示)
        let (code, resp) = http_req(t.addr, "POST", "/api/open", None).await;
        assert_eq!(code, 400, "{resp}");
        assert!(resp.contains("未配置串口"), "{resp}");
        let (code, _) = http_req(t.addr, "POST", "/api/close", None).await;
        assert_eq!(code, 200);
        // config → 改到兼容桥上 (fleet 可见)
        let (code, _) = http_req(t.addr, "POST", "/api/config", Some(r#"{"port":"COM1"}"#)).await;
        assert_eq!(code, 200);
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["bridge"]["serial"]["port"], "COM1");
        // ports 全局可用
        let (code, _) = http_req(t.addr, "GET", "/api/ports", None).await;
        assert_eq!(code, 200);
        // 第二座桥出现 → 旧端点转入 409
        fleet_create_ok(t.addr, r#"{"name":"二桥","listen":"127.0.0.1:0","autoOpen":false}"#).await;
        let (code, _) = http_req(t.addr, "GET", "/api/status", None).await;
        assert_eq!(code, 409, "多桥时旧单桥端点应拒绝");
        let (code, resp) = http_req(t.addr, "POST", "/api/open", None).await;
        assert_eq!(code, 409, "{resp}");
        // /ws 拒绝升级 (503)
        {
            let mut s = TcpStream::connect(t.addr).await.unwrap();
            let req = "GET /ws HTTP/1.1\r\nHost: t\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
            s.write_all(req.as_bytes()).await.unwrap();
            let mut buf = [0u8; 512];
            let n = s.read(&mut buf).await.unwrap();
            assert!(
                String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 503"),
                "多桥时 /ws 应 503"
            );
        }
        // 删除第二座桥 → 旧端点恢复
        let (code, _) = http_req(t.addr, "POST", "/api/fleet/b2/delete", None).await;
        assert_eq!(code, 200);
        let (code, _) = http_req(t.addr, "GET", "/api/status", None).await;
        assert_eq!(code, 200);
        t.shutdown().await;
    }

    /// qa-sprint4 DEF-1 回归 (FR-9b/ADR-9b①): 兼容模式下旧端点 POST /api/config
    /// 的 "maxClients" 必须生效并回显 (0=不限 / 非 0 均可), 与 fleet config 同一路径。
    /// (根因: ConfigReq.max_clients 丢 serde rename → "maxClients" 被当未知字段静默忽略。)
    #[tokio::test(flavor = "multi_thread")]
    async fn compat_legacy_config_max_clients_takes_effect() {
        let cli = CliBridgeSpec {
            name: "CLI".into(),
            serial: SerialConfig::default(),
            auto_open: false,
            auto_reconnect: true,
            max_clients: 1, // 模拟 --max-clients 1 启动 (QA 复现条件)
        };
        let t = spawn_mgr(None, Some(cli)).await;
        let (_, resp) = http_req(t.addr, "GET", "/api/status", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["maxClients"], 1, "CLI 初值");
        // 非 0 生效
        let (code, resp) = http_req(
            t.addr,
            "POST",
            "/api/config",
            Some(r#"{"maxClients":4}"#),
        )
        .await;
        assert_eq!(code, 200, "{resp}");
        let (_, resp) = http_req(t.addr, "GET", "/api/status", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["maxClients"], 4, "POST /api/config maxClients 须生效");
        // 0 = 不限
        let (code, _) = http_req(
            t.addr,
            "POST",
            "/api/config",
            Some(r#"{"maxClients":0}"#),
        )
        .await;
        assert_eq!(code, 200);
        let (_, resp) = http_req(t.addr, "GET", "/api/status", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["maxClients"], 0, "0 = 不限 须生效");
        t.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn compat_ws_data_path_zero_semantic_change() {
        let t = spawn_mgr(None, Some(compat_spec())).await;
        let b = wait_single_bridge(&t.mgr, Duration::from_secs(3)).await;
        // 注入 TX 队列: WS 上行字节应原样到达 (FIFO 单写者前的一站)
        let (txq_tx, txq_rx) = std_mpsc::channel::<Vec<u8>>();
        b.ctx.set_tx(txq_tx);
        let mut ws = ws_handshake(t.addr, "/ws").await;
        // RX 注入 broadcast → WS 下行二进制帧 (与串口读线程同一条通路)
        wait_receiver(&b.ctx.bc_tx, Duration::from_secs(2)).await;
        b.ctx.bc_tx.send(b"hello-fleet".to_vec()).unwrap();
        let got = ws_recv_binary(&mut ws, Duration::from_secs(2)).await;
        assert_eq!(got, b"hello-fleet");
        // TX: WS 二进制 → tx 队列 (帧语义零变化)
        ws_send_masked(&mut ws, b"to-port").await;
        let got = txq_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(got, b"to-port");
        // clients 计 1 (Drop 兜底计数不变)
        let (_, resp) = http_req(t.addr, "GET", "/api/status", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["clients"], 1);
        drop(ws);
        t.shutdown().await;
    }

    // ---- tap (FR-10g: 只读旁看) ----

    #[tokio::test(flavor = "multi_thread")]
    async fn fleet_tap_is_readonly_side_channel() {
        let t = spawn_mgr(None, None).await;
        fleet_create_ok(t.addr, r#"{"name":"t","listen":"127.0.0.1:0","autoOpen":false}"#).await;
        let b = t.mgr.get("b1").unwrap();
        let (txq_tx, txq_rx) = std_mpsc::channel::<Vec<u8>>();
        b.ctx.set_tx(txq_tx);
        let mut ws = ws_handshake(t.addr, "/api/fleet/b1/tap").await;
        // RX 旁看: 与数据面同一条 broadcast 源
        wait_receiver(&b.ctx.bc_tx, Duration::from_secs(2)).await;
        b.ctx.bc_tx.send(b"tap-bytes".to_vec()).unwrap();
        let got = ws_recv_binary(&mut ws, Duration::from_secs(2)).await;
        assert_eq!(got, b"tap-bytes");
        // 只读: tap 发送的字节绝不进 tx 队列
        ws_send_masked(&mut ws, b"evil").await;
        assert!(
            txq_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "tap 不得写入串口方向"
        );
        // tap 不计入 clients (连接数口径 = 数据面客户端)
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["bridge"]["clients"], 0);
        // 不存在的桥 → 404 (非 101)
        {
            let mut s = TcpStream::connect(t.addr).await.unwrap();
            let req = "GET /api/fleet/bX/tap HTTP/1.1\r\nHost: t\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
            s.write_all(req.as_bytes()).await.unwrap();
            let mut buf = [0u8; 512];
            let n = s.read(&mut buf).await.unwrap();
            assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 404"));
        }
        drop(ws);
        t.shutdown().await;
    }

    // ---- 多桥隔离 (FR-10b) ----

    #[tokio::test(flavor = "multi_thread")]
    async fn bridge_data_planes_are_isolated() {
        let t = spawn_mgr(None, None).await;
        fleet_create_ok(t.addr, r#"{"name":"一","listen":"127.0.0.1:0","autoOpen":false}"#).await;
        fleet_create_ok(t.addr, r#"{"name":"二","listen":"127.0.0.1:0","autoOpen":false}"#).await;
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        let arr = v["bridges"].as_array().unwrap();
        let p1: SocketAddr = arr[0]["listen"].as_str().unwrap().parse().unwrap();
        let p2: SocketAddr = arr[1]["listen"].as_str().unwrap().parse().unwrap();
        assert_ne!(p1, p2, "各桥独立数据端口");
        // 连 b1 的客户端: 只收 b1 的 RX
        let mut ws1 = ws_handshake(p1, "/ws").await;
        // b2 无客户端订阅 → send 返回 SendError 属正常 (读循环不反压, 无人收即弃)
        let _ = t
            .mgr
            .get("b2")
            .unwrap()
            .ctx
            .bc_tx
            .send(b"from-b2".to_vec());
        let mut tmp = [0u8; 64];
        let r = tokio::time::timeout(Duration::from_millis(250), ws1.read(&mut tmp)).await;
        assert!(r.is_err(), "b2 的广播不得泄漏到 b1 的客户端");
        wait_receiver(
            &t.mgr.get("b1").unwrap().ctx.bc_tx,
            Duration::from_secs(2),
        )
        .await;
        t.mgr
            .get("b1")
            .unwrap()
            .ctx
            .bc_tx
            .send(b"from-b1".to_vec())
            .unwrap();
        let got = ws_recv_binary(&mut ws1, Duration::from_secs(2)).await;
        assert_eq!(got, b"from-b1");
        drop(ws1);
        t.shutdown().await;
    }

    // ---- 统计引擎挂接 (FR-10c) ----

    #[tokio::test]
    async fn bridge_rate_shows_in_detail() {
        // 独立 Bridge (无管理器采样任务干扰): push 可控时间戳
        let b = Bridge::new("bx".into(), "t".into(), SerialConfig::default(), false, true, 0);
        let t0 = Instant::now();
        lock_mutex(&b.rates).push(t0, 0, 0);
        b.hub.add_rx(500);
        lock_mutex(&b.rates).push(t0 + Duration::from_millis(500), 500, 0);
        let v = b.detail_json();
        assert_eq!(v["rxBytes"], 500);
        assert!(
            (v["rxRate"].as_f64().unwrap() - 1000.0).abs() < 1.0,
            "500B/0.5s = 1000B/s, 得 {}",
            v["rxRate"]
        );
        assert_eq!(v["phase"], "closed"); // 未 start 的桥 (running:false 区分)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn manager_sampler_feeds_rate_window() {
        let t = spawn_mgr(None, None).await;
        fleet_create_ok(t.addr, r#"{"name":"s","listen":"127.0.0.1:0","autoOpen":false}"#).await;
        let b = t.mgr.get("b1").unwrap();
        b.hub.add_rx(1234);
        // 等采样任务至少推一个点 (200ms 节拍; 900ms ≈ 3+ 周期, 抗并行调度抖动),
        // 验证窗口已有样本且读取不 panic
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert!(lock_mutex(&b.rates).rates(Instant::now()).0 >= 0.0);
        t.shutdown().await;
    }

    /// SQ-UI-1/ADR-14②③: 桥对象必须回显 maxClients (FIX-17 同类风险: 无回显则 UI
    /// 保存「同时连接上限」即静默清零); config 受理 create 形状 body 含 maxClients。
    #[tokio::test(flavor = "multi_thread")]
    async fn fleet_maxclients_echo_and_config_roundtrip() {
        let t = spawn_mgr(None, None).await;
        // create 带上限 → 列表与详情都回显 (SQ-UI-1 钉住)
        let v = fleet_create_ok(
            t.addr,
            r#"{"name":"上限桥","listen":"127.0.0.1:0","autoOpen":false,"maxClients":4}"#,
        )
        .await;
        assert_eq!(v["id"], "b1");
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["bridges"][0]["maxClients"], 4, "列表行须回显 maxClients");
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["bridge"]["maxClients"], 4, "详情须回显 maxClients");
        // PATCH 路径 (SQ-UI-2): create 形状 body 含 maxClients → 往返
        let (code, resp) = http_req(
            t.addr,
            "PATCH",
            "/api/fleet/b1/config",
            Some(r#"{"maxClients":2}"#),
        )
        .await;
        assert_eq!(code, 200, "{resp}");
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["bridge"]["maxClients"], 2);
        // 0 = 不限 回显 (回显什么 UI 就能存回什么, 不静默清零)
        let (code, _) = http_req(
            t.addr,
            "POST",
            "/api/fleet/b1/config",
            Some(r#"{"maxClients":0}"#),
        )
        .await;
        assert_eq!(code, 200);
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["bridge"]["maxClients"], 0);
        t.shutdown().await;
    }

    /// ADR-15①/ADR-16①: fleet 桥对象契约字段集 14→15 —— 原 14 契约字段
    /// (与 QA conftest FLEET_ROW_FIELDS 同源) + autoReconnect (bool, FR-12)。
    /// 列表行与单桥详情同构, 必须都回显; 除已声明的内部字段 (running/autoOpen)
    /// 外不得缺字段, 也不得混入未裁定字段。
    #[tokio::test(flavor = "multi_thread")]
    async fn fleet_bridge_object_contract_15_fields() {
        use std::collections::HashSet;
        let t = spawn_mgr(None, None).await;
        fleet_create_ok(
            t.addr,
            r#"{"name":"契约桥","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        let want: HashSet<&str> = [
            "id",
            "name",
            "serial",
            "listen",
            "phase",
            "clients",
            "rxBytes",
            "txBytes",
            "rxRate",
            "txRate",
            "lastError",
            "uptimeSec",
            "maxClients",
            "retries",        // ADR-15① 新增 (13→14)
            "autoReconnect",  // ADR-16① 新增 (14→15)
        ]
        .into_iter()
        .collect();
        let known_extra: HashSet<&str> = ["running", "autoOpen"].into_iter().collect();
        let check = |row: &Value, where_: &str| {
            let got: HashSet<&str> =
                row.as_object().unwrap().keys().map(|s| s.as_str()).collect();
            let missing: Vec<_> = want.difference(&got).collect();
            assert!(missing.is_empty(), "{where_} 缺契约字段 {missing:?}: {row}");
            let unknown: Vec<_> = got
                .difference(&want)
                .filter(|k| !known_extra.contains(**k))
                .collect();
            assert!(
                unknown.is_empty(),
                "{where_} 混入未裁定字段 {unknown:?} (契约膨胀需走 ADR): {row}"
            );
            assert_eq!(
                row["retries"].as_u64(),
                Some(0),
                "{where_} retries 须为非负整数 (初值 0): {row}"
            );
            assert_eq!(
                row["autoReconnect"].as_bool(),
                Some(true),
                "{where_} autoReconnect 须为 bool (缺省 true): {row}"
            );
        };
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        check(&v["bridges"][0], "列表行");
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        check(&v["bridge"], "单桥详情");
        t.shutdown().await;
    }

    /// FR-12/ADR-16①: autoReconnect 全链路 —— 建桥缺省 true / 显式 false 受理 /
    /// PATCH+POST 改配往返 / fleet.json 变更即写 / 恢复不丢。
    #[tokio::test(flavor = "multi_thread")]
    async fn fleet_auto_reconnect_default_create_config_and_persist() {
        let path = temp_fleet("reconn");
        let t = spawn_mgr(Some(path.clone()), None).await;
        // 建桥不带 autoReconnect → 缺省 true
        fleet_create_ok(
            t.addr,
            r#"{"name":"默认桥","serial":{"port":"COM1"},"listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        // 建桥显式 autoReconnect=false → 受理
        fleet_create_ok(
            t.addr,
            r#"{"name":"停桥","serial":{"port":"COM2"},"listen":"127.0.0.1:0","autoOpen":false,"autoReconnect":false}"#,
        )
        .await;
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        let arr = v["bridges"].as_array().unwrap();
        assert_eq!(arr[0]["autoReconnect"], true, "建桥缺省 true");
        assert_eq!(arr[1]["autoReconnect"], false, "建桥显式 false 受理");
        // 建桥即持久化, 字段不丢
        let recs = load_fleet(&path).unwrap();
        assert!(recs[0].auto_reconnect);
        assert!(!recs[1].auto_reconnect);
        // PATCH 改配 true→false→true 往返
        let (code, resp) = http_req(
            t.addr,
            "PATCH",
            "/api/fleet/b1/config",
            Some(r#"{"autoReconnect":false}"#),
        )
        .await;
        assert_eq!(code, 200, "{resp}");
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["bridge"]["autoReconnect"], false, "PATCH 关闭须生效");
        let (code, _) = http_req(
            t.addr,
            "POST",
            "/api/fleet/b1/config",
            Some(r#"{"autoReconnect":true}"#),
        )
        .await;
        assert_eq!(code, 200);
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["bridge"]["autoReconnect"], true, "POST 重开须生效");
        // 改配即持久化
        let recs = load_fleet(&path).unwrap();
        assert!(recs[0].auto_reconnect, "改配 true 须写进清单");
        assert!(!recs[1].auto_reconnect, "b2 须保持 false 不被静默清改");
        t.shutdown().await;
        let _ = std::fs::remove_file(&path);
    }

    /// FR-12: 单桥兼容 POST /api/config 也受理 autoReconnect (FR-10h 分发语义);
    /// /api/status 回显 13 字段契约中的 autoReconnect。
    #[tokio::test(flavor = "multi_thread")]
    async fn compat_legacy_config_auto_reconnect_accepted() {
        let t = spawn_mgr(None, Some(compat_spec())).await;
        let (_, resp) = http_req(t.addr, "GET", "/api/status", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["autoReconnect"], true, "单桥 status 默认 true");
        // 兼容端点关闭
        let (code, resp) = http_req(
            t.addr,
            "POST",
            "/api/config",
            Some(r#"{"autoReconnect":false}"#),
        )
        .await;
        assert_eq!(code, 200, "{resp}");
        let (_, resp) = http_req(t.addr, "GET", "/api/status", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["autoReconnect"], false, "POST /api/config 关闭须生效");
        // fleet 详情同源回显 (同一 HubState, 单一真相)
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["bridge"]["autoReconnect"], false);
        // 兼容端点重开
        let (code, _) = http_req(t.addr, "POST", "/api/config", Some(r#"{"autoReconnect":true}"#)).await;
        assert_eq!(code, 200);
        let (_, resp) = http_req(t.addr, "GET", "/api/status", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["autoReconnect"], true);
        t.shutdown().await;
    }

    // ---- 无桥运行: 旧端点不可用但不 panic ----

    #[tokio::test(flavor = "multi_thread")]
    async fn zero_bridges_legacy_endpoints_unavailable() {
        let t = spawn_mgr(None, None).await;
        for (method, path) in [
            ("GET", "/api/status"),
            ("POST", "/api/open"),
            ("POST", "/api/close"),
        ] {
            let (code, resp) = http_req(t.addr, method, path, None).await;
            assert_eq!(code, 409, "{method} {path} → {resp}");
        }
        // fleet 列表仍可用 (空)
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["bridges"].as_array().unwrap().len(), 0);
        // ports 仍可用
        let (code, _) = http_req(t.addr, "GET", "/api/ports", None).await;
        assert_eq!(code, 200);
        t.shutdown().await;
    }

    // ---- FR-13: 管理台地址换绑 + [manager] 持久化 + 重启恢复 ----

    /// 挑一个当前空闲的 127.0.0.1 端口 (绑定后立即释放)。
    async fn free_port() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn manager_addr_rebind_persists_and_restores() {
        let path = temp_fleet("mgraddr");
        let t = spawn_mgr_opt(Some(path.clone()), None, "127.0.0.1:0".parse().unwrap(), false).await;
        // 先建一座桥: 换绑管理台不得影响桥数据面 (FR-13 硬边界)
        let v = fleet_create_ok(
            t.addr,
            r#"{"name":"稳定桥","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        let bridge_listen: SocketAddr = v["listen"].as_str().unwrap().parse().unwrap();
        // 选一个空闲目标端口
        let new_port = free_port().await;
        let new_addr = SocketAddr::from(([127, 0, 0, 1], new_port));
        // POST /api/manager/addr → 受理
        let (code, resp) = http_req(
            t.addr,
            "POST",
            "/api/manager/addr",
            Some(&format!(r#"{{"addr":"{new_addr}"}}"#)),
        )
        .await;
        assert_eq!(code, 200, "换址应受理: {resp}");
        // 原地换绑: 旧地址关闭, 新地址就绪 (<1s 量级, 给 3s 余量)
        assert!(
            wait_connectable(new_addr, true, Duration::from_secs(3)).await,
            "新地址应就绪"
        );
        assert!(
            wait_connectable(t.addr, false, Duration::from_secs(3)).await,
            "旧地址应关闭"
        );
        // 桥数据面不动
        assert!(
            wait_connectable(bridge_listen, true, Duration::from_secs(3)).await,
            "桥数据端口必须不受管理台换址影响"
        );
        // [manager] 段持久化 (重启恢复的依据)
        let f = load_fleet_file(&path).unwrap();
        assert_eq!(
            f.manager.map(|m| m.addr),
            Some(new_addr.to_string()),
            "换址成功后 [manager] 段须写入清单"
        );
        // Ready(新地址) 事件 (壳层 webview 跟随的依据)
        let evs = t.events.lock().unwrap().clone();
        let last_ready = evs
            .iter()
            .filter_map(|e| match e {
                ServiceEvent::Ready(a) => Some(*a),
                _ => None,
            })
            .last()
            .expect("至少一次 Ready");
        assert_eq!(last_ready, new_addr, "换绑成功后须发 Ready(新地址)");
        // 非法地址 → 400 (原地址继续服务)
        let (code, _) = http_req(
            new_addr,
            "POST",
            "/api/manager/addr",
            Some(r#"{"addr":"not-an-addr"}"#),
        )
        .await;
        assert_eq!(code, 400);
        t.shutdown().await;

        // 重启恢复: 未显式 --addr → 控制面回到持久化地址
        let t2 = spawn_mgr_opt(Some(path.clone()), None, "127.0.0.1:0".parse().unwrap(), false).await;
        assert_eq!(t2.addr, new_addr, "重启后应恢复 [manager] 持久化地址");
        t2.shutdown().await;

        // 显式 --addr 优先于清单恢复值
        let t3 = spawn_mgr_opt(Some(path.clone()), None, "127.0.0.1:0".parse().unwrap(), true).await;
        assert_ne!(
            t3.addr, new_addr,
            "显式 --addr (:0 → 随机) 应优先于 [manager] 恢复值"
        );
        t3.shutdown().await;
        let _ = std::fs::remove_file(&path);
    }

    // ---- FR-14: 主题插件端点 ----

    #[tokio::test(flavor = "multi_thread")]
    async fn themes_endpoints_list_serve_and_traversal_guard() {
        let t = spawn_mgr(None, None).await;
        // 启动时内置主题已落盘 (ensure_builtin), 列表应含三套内置
        let (code, resp) = http_req(t.addr, "GET", "/api/themes", None).await;
        assert_eq!(code, 200, "{resp}");
        let v: Value = serde_json::from_str(&resp).unwrap();
        let list = v["themes"].as_array().expect("themes 数组");
        let get = |n: &str| list.iter().find(|t| t["name"] == n).cloned();
        for builtin in ["light", "dark", "example-oreo"] {
            let e = get(builtin).unwrap_or_else(|| panic!("内置主题 {builtin} 应在列: {resp}"));
            assert_eq!(e["builtin"], true, "{builtin} builtin 标记");
        }
        assert!(
            list.iter().all(|e| e["builtin"] == true),
            "此刻目录里只有内置主题"
        );
        // 插件语义: 丢一个自制 css 进目录 → 扫描即见 (builtin=false)
        std::fs::write(t.themes_dir.join("my-plugin.css"), ":root{--bg:#000}").unwrap();
        let (_, resp) = http_req(t.addr, "GET", "/api/themes", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        let e = v["themes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "my-plugin")
            .cloned()
            .expect("插件主题应被扫描到");
        assert_eq!(e["builtin"], false);
        // 静态服务: 命中内置 (200 + :root 令牌)
        let (code, body) = http_req(t.addr, "GET", "/themes/dark.css", None).await;
        assert_eq!(code, 200);
        assert!(body.contains(":root") && body.contains("--bg:#14171a"), "{body}");
        // 静态服务: 插件文件
        let (code, body) = http_req(t.addr, "GET", "/themes/my-plugin.css", None).await;
        assert_eq!(code, 200);
        assert!(body.contains("--bg:#000"));
        // 静态服务: 未命中
        let (code, _) = http_req(t.addr, "GET", "/themes/missing.css", None).await;
        assert_eq!(code, 404);
        // 路径穿越: 多段 (`..` 路径段) 路由不匹配
        let (code, _) = http_req(t.addr, "GET", "/themes/../Cargo.toml", None).await;
        assert_eq!(code, 404, "目录上跳不得命中");
        // 路径穿越: 百分号编码挤进单段 (../Cargo.toml / ..\Cargo.toml) → resolve 拒绝
        let (code, _) = http_req(t.addr, "GET", "/themes/%2e%2e%2fCargo.toml", None).await;
        assert_eq!(code, 404, "编码穿越 (../) 不得命中");
        let (code, _) = http_req(t.addr, "GET", "/themes/%2e%2e%5cCargo.toml", None).await;
        assert_eq!(code, 404, "编码穿越 (..\\) 不得命中");
        // 非法字符名
        let (code, _) = http_req(t.addr, "GET", "/themes/%E4%B8%AD%E6%96%87.css", None).await;
        assert_eq!(code, 404, "白名单外文件名不得命中");
        // 非 css
        let (code, _) = http_req(t.addr, "GET", "/themes/fake.css.css", None).await;
        assert_eq!(code, 404, "不存在的文件不得命中");
        let dir = t.themes_dir.clone();
        t.shutdown().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- FR-15: 网页 favicon ----

    #[tokio::test(flavor = "multi_thread")]
    async fn favicon_endpoint_serves_svg() {
        let t = spawn_mgr(None, None).await;
        let (code, body) = http_req(t.addr, "GET", "/favicon.svg", None).await;
        assert_eq!(code, 200, "favicon 端点恒可用 (资产缺失走兜底)");
        assert!(body.contains("<svg"), "应为 SVG: {body}");
        t.shutdown().await;
    }
}
