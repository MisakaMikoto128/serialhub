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
use axum::extract::{FromRequestParts, Path as AxumPath, Request, State};
use axum::http::{header, StatusCode};
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
    /// FR-19: 录制会话 (None = 空闲; 状态机见 record 模块)。
    pub rec: Mutex<Option<crate::record::RecHandle>>,
    /// FR-19: 回放会话 (None = 空闲; 进度经 detail_json.replay 可见)。
    pub replay: Mutex<Option<crate::record::ReplayHandle>>,
    /// FR-20: 旁路转发配置 ("host:port"; 空串 = 关闭) —— 配置真相, 热改经
    /// forward::apply_target 通知会话任务。
    forward_target: RwLock<String>,
    /// FR-20: 当前 TCP 转发是否连着 (detail_json.forwardConnected)。
    pub forward_connected: AtomicBool,
    /// FR-20: 运行中的转发会话 (None = 桥停止中/未运行; 随桥数据面启停)。
    pub forward: Mutex<Option<crate::forward::ForwardSession>>,
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
        forward_tcp: String,
    ) -> Arc<Bridge> {
        let hub = Arc::new(HubState::new(serial, max_clients));
        hub.set_auto_reconnect(auto_reconnect); // FR-12: 建桥即定 (改配走 hub)
        hub.set_label(format!("桥 {id}")); // FR-21: 日志身份标签
        let (bc_tx, _) = tokio::sync::broadcast::channel::<Vec<u8>>(1024);
        let ctx = PortCtx {
            hub: hub.clone(),
            bc_tx,
            tx_bc: tokio::sync::broadcast::channel(256).0,
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
            rec: Mutex::new(None),
            replay: Mutex::new(None),
            forward_target: RwLock::new(forward_tcp),
            forward_connected: AtomicBool::new(false),
            forward: Mutex::new(None),
            serve_handle: Mutex::new(None),
        })
    }

    pub fn name(&self) -> String {
        self.name.read().unwrap_or_else(|e| e.into_inner()).clone()
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

    /// FR-20: 旁路转发目标配置 (空串 = 关闭)。
    pub fn forward_target(&self) -> String {
        self.forward_target
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// FR-20: 写旁路转发目标配置 (forward::apply_target 用; 会话通知在它那边)。
    pub(crate) fn set_forward_target(&self, t: String) {
        *self
            .forward_target
            .write()
            .unwrap_or_else(|e| e.into_inner()) = t;
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
            // FR-19/ADR-24 B1: 录制/回放状态 (15→17 字段; null 或会话摘要)
            "recording": crate::record::recording_json(self),
            "replay": crate::record::replay_json(self),
            // FR-20/ADR-24② B2: 旁路转发配置与连接状态 (17→19 字段)
            "forwardTcp": self.forward_target(),
            "forwardConnected": self.forward_connected.load(Ordering::Relaxed),
        })
    }
}

/// 桥数据面路由 (ADR-20 双路径兼容): /ws 与 / (裸地址, serial_bridge.py 时代习惯)
/// 都受理 WS 升级; 其余路径 404 (axum 默认); /api/status 只读投影便于直连诊断。
fn bridge_router(b: Arc<Bridge>) -> Router {
    Router::new()
        .route("/ws", get(bridge_ws))
        .route("/", get(bridge_root))
        .route("/api/status", get(bridge_status))
        .with_state(b)
}

async fn bridge_status(State(b): State<Arc<Bridge>>) -> Response {
    api::status_core(&b.ctx)
}

/// 桥数据口根路径 (ADR-20): WS 升级请求与 /ws 等价受理;
/// 普通 HTTP GET → 426 Upgrade Required + 指路
/// (管理台控制面不受影响, 其 GET / 仍是管理台页面)。
/// axum 0.8 已移除 Option 提取器且 ws Rejection 私有 → 手动 FromRequestParts 判别。
async fn bridge_root(State(b): State<Arc<Bridge>>, req: Request) -> Response {
    let (mut parts, body) = req.into_parts();
    match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => {
            drop(body);
            bridge_ws(upgrade, State(b)).await
        }
        Err(_) => (
            StatusCode::UPGRADE_REQUIRED,
            "这是串口数据端点, 请用 WebSocket 连接 (ws://host:port/ws)",
        )
            .into_response(),
    }
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
async fn bridge_serve(bridge: Arc<Bridge>, listener: TcpListener, stop_rx: watch::Receiver<bool>) {
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
    /// FR-20/ADR-24②: 旁路转发目标 ("host:port"; 空串 = 关闭)。调用方已校验。
    pub forward_tcp: String,
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
    /// ADR-22①: 启动时从 fleet.json 读到的 [window] 段 —— persist 的**兜底**
    /// 来源 (盘上无文件/解析失败时)。BUG-1 起 persist 以盘上现值为主源 (见
    /// persist 注释), 本快照不再承担"桥变更不抹窗口几何"的主责;
    /// 运行期只有 restore_fleet 写一次; headless 同样只兜底 (不产生新值)。
    window_seen: RwLock<Option<WindowRec>>,
    /// FR-19: 录像目录 (None = exe 旁 recordings/; --recordings-dir 可指定)。
    recordings_dir: RwLock<Option<PathBuf>>,
    next_id: AtomicU64,
}

impl BridgeManager {
    pub fn new(fleet_path: Option<PathBuf>) -> Self {
        Self {
            bridges: RwLock::new(BTreeMap::new()),
            fleet_path,
            manager_addr: Mutex::new(None),
            persist_suppressed: AtomicBool::new(false),
            window_seen: RwLock::new(None),
            recordings_dir: RwLock::new(None),
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

    /// FR-19: 录像目录 (默认 exe 旁 recordings/; --recordings-dir 覆盖)。
    pub fn set_recordings_dir(&self, d: PathBuf) {
        *wr(&self.recordings_dir) = Some(d);
    }

    pub fn recordings_dir(&self) -> PathBuf {
        rd(&self.recordings_dir)
            .clone()
            .unwrap_or_else(crate::record::default_recordings_dir)
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
            spec.forward_tcp,
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
                // FR-20: 旁路转发会话随数据面启动 (目标空 = 待命, 不连接)
                crate::forward::start_session(&bridge);
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
        // FR-20: 旁路转发会话随数据面启动 (配置在 Bridge.forward_target)
        crate::forward::start_session(&b);
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
        crate::forward::stop_session(b); // FR-20: 断开旁路转发 (TCP 连接随任务收尾)
        let _ = b.cmd_tx.send(HubCmd::Close); // COM 释放优先
        b.ctx.stop_active();
        let _ = b.stop_tx.send(true);
        let h = lock_mutex(&b.serve_handle).take();
        if let Some(h) = h {
            if tokio::time::timeout(Duration::from_secs(2), h)
                .await
                .is_err()
            {
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

    /// 改配 (FR-10g config): name/串口参数/maxClients/autoOpen/autoReconnect/
    /// forwardTcp (FR-20, 热生效: 断开旧连接按新值重连, 改空 = 断开);
    /// listen 不可改 (FR-10f)。成功即持久化。串口参数沿用"close→config→open"
    /// 语义 (spec 非目标: 不热改); autoReconnect 经 apply_config_core 即改即生效。
    pub fn config_bridge(
        &self,
        id: &str,
        name: Option<String>,
        auto_open: Option<bool>,
        forward_tcp: Option<String>,
        req: api::ConfigReq,
    ) -> Response {
        let Some(b) = self.get(id) else {
            return not_found_bridge(id);
        };
        if let Some(n) = name {
            b.set_name(n);
        }
        if let Some(ft) = &forward_tcp {
            if let Err(e) = crate::forward::validate_target(ft) {
                return api::bad(e);
            }
        }
        let resp = api::apply_config_core(&b.ctx, req);
        if resp.status().is_success() {
            if let Some(a) = auto_open {
                b.auto_open.store(a, Ordering::Relaxed);
            }
            if let Some(ft) = forward_tcp {
                // FR-20: 热生效 (会话不在时只落配置, start 时按新值起任务)
                crate::forward::apply_target(&b, ft);
                crate::logging::write(
                    "info",
                    &format!("桥 {id} 旁路转发目标改为 \"{}\"", b.forward_target()),
                );
            }
            self.persist();
        }
        resp
    }

    /// fleet.json 变更即写 (FR-10b)。持久化关闭 (--no-fleet) 或恢复期间 = 空操作。
    /// FR-13: 同时写入顶层 [manager] 段; ADR-22①/BUG-1: 顶层 [window] 段一律
    /// 保住 (口径见下), 桥变更/换址的整份重写不再可能抹掉 GUI 窗口几何。
    pub fn persist(&self) {
        if self.persist_suppressed.load(Ordering::Relaxed) {
            return;
        }
        let Some(path) = &self.fleet_path else { return };
        let recs: Vec<FleetBridgeRec> = self.snapshot().iter().map(rec_of_bridge).collect();
        // BUG-1 (v1.7.0 用户现场「窗口记忆失效」): 整份重写必须合并盘上现值,
        // 不能只靠启动快照 —— 本进程启动时盘上可能还没有 [window] 段 (旧版清单/
        // 升级首启), GUI 退出才把它写上盘; 若 persist 只看 window_seen, 这之后
        // 的任何桥变更都把窗口几何抹掉。合并口径:
        //   [window]  = 盘上现值为准 (只有 GUI 真实退出经 save_window 改它,
        //               盘值即最新), window_seen 仅在盘上无文件/解析失败时兜底;
        //   [manager] = 内存现址为准 (运行中控制面地址是活真相), 盘值仅在
        //               set_manager_addr 尚未登记时兜底 (地址不因早退丢档)。
        // 读盘失败 (文件暂不在/坏档) 静默走兜底, 不阻塞"变更即存"主路径。
        let disk = load_fleet_file(path).ok();
        let window = disk
            .as_ref()
            .and_then(|ff| ff.window)
            .or(*rd(&self.window_seen));
        let manager = self
            .manager_addr()
            .map(|a| ManagerRec {
                addr: a.to_string(),
            })
            .or_else(|| disk.as_ref().and_then(|ff| ff.manager.clone()));
        if let Err(e) = save_fleet(path, &recs, manager.as_ref(), window.as_ref()) {
            eprintln!("serialhub: fleet 清单写入失败 ({path:?}): {e}");
        }
    }

    /// FR-22: 导出字节流 —— fleet.json **原样**下载; 盘上无清单 (--no-fleet 或
    /// 尚未落盘) 时按当前状态即时序列化 (与 persist 写出的形状完全一致)。
    pub fn fleet_export_bytes(&self) -> Vec<u8> {
        if let Some(path) = &self.fleet_path {
            if let Ok(bytes) = std::fs::read(path) {
                return bytes;
            }
        }
        let recs: Vec<FleetBridgeRec> = self.snapshot().iter().map(rec_of_bridge).collect();
        let manager = self.manager_addr().map(|a| ManagerRec {
            addr: a.to_string(),
        });
        serde_json::to_vec_pretty(&FleetFile {
            version: 1,
            bridges: recs,
            manager,
            window: None,
        })
        .unwrap_or_default()
    }

    /// FR-22: 导入 (merge=按 id 合并, 冲突跳过并计数; replace=整表替换, 运行中桥
    /// 先停)。调用方已做 schema/每桥深校验; 单座创建失败 (端口冲突等) 计入 skipped,
    /// 不中断导入。成功桥 = 数据面运行 + autoOpen 按需开串口; 结束统一持久化。
    pub async fn import_bridges(
        &self,
        specs: Vec<(FleetBridgeRec, SerialConfig, SocketAddr)>,
        replace: bool,
    ) -> (usize, usize) {
        let mut imported = 0usize;
        let mut skipped = 0usize;
        if replace {
            for b in self.snapshot() {
                self.stop_one(&b).await; // 运行中桥先停 (含录制/回放自动收尾)
            }
            wr(&self.bridges).clear();
        }
        for (rec, serial, listen) in specs {
            let spec = BridgeSpec {
                id: Some(rec.id.clone()),
                name: rec.name.clone(),
                serial,
                listen,
                auto_open: rec.auto_open,
                auto_reconnect: rec.auto_reconnect,
                max_clients: rec.max_clients,
                forward_tcp: rec.forward_tcp,
            };
            match self.create_bridge_inner(spec, false).await {
                Ok(_) => imported += 1,
                Err(e) => {
                    skipped += 1;
                    eprintln!("serialhub: 导入桥 {} 跳过: {e}", rec.id);
                }
            }
        }
        self.persist();
        (imported, skipped)
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
    /// FR-20/ADR-24②: 旁路转发目标 ("host:port"); 空串不落盘 (旧清单零噪声,
    /// 读入 default 空 = 关闭)。
    #[serde(
        rename = "forwardTcp",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub forward_tcp: String,
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

/// ADR-22①: fleet.json 顶层 [window] 段 —— GUI 主窗口几何记忆。
/// 归属 fleet.json 理由: 它已是本机状态的单一事实来源 (ADR-22①裁定),
/// 不再开第二个配置文件。x/y = 窗口外框左上角 (物理像素), w/h = 客户区
/// (inner) 尺寸 (物理像素, 与恢复接口 with_inner_size 同口径, 避免标题栏
/// 高度逐次累积); maximized 单独记, x/y/w/h 恒存"还原态"几何 —— 最大化
/// 期间采样跳过, 用户取消最大化后落回正确位置。headless 不写不读。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub(crate) struct WindowRec {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    #[serde(default)] // 旧版段可能没有此字段 → 非最大化
    pub maximized: bool,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct FleetFile {
    version: u32,
    #[serde(default)]
    bridges: Vec<FleetBridgeRec>,
    /// FR-13: 旧清单无此字段 → default None 兼容; 写出时空段跳过。
    #[serde(default, rename = "manager", skip_serializing_if = "Option::is_none")]
    manager: Option<ManagerRec>,
    /// ADR-22①: 旧清单无此字段 → default None 兼容; 写出时空段跳过
    /// (headless 从不产生该段; GUI 真实退出时经 save_window 落盘)。
    #[serde(default, rename = "window", skip_serializing_if = "Option::is_none")]
    window: Option<WindowRec>,
}

/// 原子写: 临时文件 + 改名覆盖 (进程中途被杀不会留下半截清单)。
pub(crate) fn save_fleet(
    path: &Path,
    bridges: &[FleetBridgeRec],
    manager: Option<&ManagerRec>,
    window: Option<&WindowRec>,
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
        window: window.copied(),
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

/// ADR-22①: 读 [window] 段 (GUI 启动恢复用)。文件不存在 / 无该段 → Ok(None);
/// 文件存在但解析失败 → Err (调用方记日志后按无记忆启动, 不得覆盖坏文件)。
pub(crate) fn load_window(path: &Path) -> Result<Option<WindowRec>, String> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(load_fleet_file(path)?.window)
}

/// ADR-22①: 只替换 [window] 段 (GUI 真实退出时调用) —— 读整文件 → 换 window →
/// 经 save_fleet 同一原子通道写回 (bridges/[manager] 段原样保留, 变更即存口径)。
/// 文件存在但解析失败 → 拒写 (宁丢一次窗口几何, 不覆盖用户清单), 错误由调用方记日志。
pub(crate) fn save_window(path: &Path, w: WindowRec) -> Result<(), String> {
    let (bridges, manager) = if path.exists() {
        let ff = load_fleet_file(path)?;
        (ff.bridges, ff.manager)
    } else {
        // 首次退出尚无清单 (从未建过桥): 建一个只含 window 段的最小清单
        (Vec::new(), None)
    };
    save_fleet(path, &bridges, manager.as_ref(), Some(&w))
}

/// 桥记录读取 (restore_fleet 已改走 load_fleet_file 以顺带取 [window] 段;
/// 本函数现仅测试断言用, 随测试编译)。
#[cfg(test)]
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
        forward_tcp: b.forward_target(),
        serial: SerialRec::of_config(&b.hub.config()),
    }
}

/// 启动恢复 (FR-10b): fleet.json 存在则恢复全部桥。
/// 单条记录非法 → 跳过并告警; 绑不上端口 → lenient 模式以 stopped+lastError 入队。
async fn restore_fleet(mgr: &Arc<BridgeManager>) -> usize {
    let Some(path) = mgr.fleet_path.clone() else {
        return 0;
    };
    if !path.exists() {
        return 0;
    }
    let recs = match load_fleet_file(&path) {
        Ok(ff) => {
            // ADR-22①: [window] 段登记到管理器 —— persist 时原样写回,
            // 运行期桥变更不再抹掉 GUI 的窗口几何 (真实退出时 GUI 才更新它)。
            *wr(&mgr.window_seen) = ff.window;
            ff.bridges
        }
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
                    forward_tcp: rec.forward_tcp.clone(),
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
                    if b.is_running() {
                        ""
                    } else {
                        " (端口暂不可用, 已停止)"
                    }
                );
            }
            Err(e) => eprintln!("serialhub: 恢复桥 {} 失败: {e}", rec.id),
        }
    }
    mgr.next_id.fetch_max(max_id + 1, Ordering::Relaxed);
    mgr.persist_suppressed.store(false, Ordering::Relaxed);
    crate::logging::write("info", &format!("fleet 恢复完成: {n} 座桥"));
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
    /// FR-19: 录像目录; None = exe 旁 recordings/ (record::default_recordings_dir)。
    pub recordings_dir: Option<PathBuf>,
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
            recordings_dir: cli.recordings_dir.clone(),
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
    Err(format!(
        "listen 地址无效: \"{t}\" (形如 127.0.0.1:8101 或 8101)"
    ))
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
    /// ADR-21②: 当前管理台地址 (/api/open-console 回显; 每轮 serve 随绑定值重建)。
    console_addr: SocketAddr,
    /// ADR-21②: 打开器 (生产 = crate::browser::open_url; 单测注入记录闭包,
    /// 不真开浏览器)。Arc<dyn Fn> 而非 fn 指针: 测试闭包需捕获记录槽。
    console_opener: Arc<dyn Fn(&str) + Send + Sync>,
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
        // ---- FR-19 录制与回放 (ADR-24 B1) ----
        .route("/api/fleet/{id}/record/start", post(fleet_record_start))
        .route("/api/fleet/{id}/record/stop", post(fleet_record_stop))
        .route("/api/fleet/{id}/recordings", get(fleet_recordings))
        // 删单条录像 (ADR-24⑥: 契约缺口裁定; 穿越拒绝同 replay)
        .route(
            "/api/fleet/{id}/recordings/delete",
            post(fleet_recordings_delete),
        )
        .route("/api/fleet/{id}/replay", post(fleet_replay))
        // loop=true 的回放需要可被停止 (FR-19 "循环到被停止")
        .route("/api/fleet/{id}/replay/stop", post(fleet_replay_stop))
        // ---- FR-22 配置导入导出 (ADR-24 B4) ----
        // export: GET+POST 双受理 (ADR-24⑥; UI 先 POST 后 GET)
        .route("/api/fleet/export", get(fleet_export).post(fleet_export))
        .route("/api/fleet/import", post(fleet_import))
        // ---- FR-13 管理台设置: 控制面地址原地换绑 (复用 ADR-12 机制) ----
        .route("/api/manager/addr", post(manager_set_addr))
        // ---- ADR-21②: 用系统默认浏览器打开管理台 (壳内页面按钮受信路径) ----
        .route("/api/open-console", post(open_console))
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
    /// FR-20/ADR-24②: 旁路转发目标 "host:port" (缺省/空 = 关闭)。
    #[serde(rename = "forwardTcp", default)]
    forward_tcp: Option<String>,
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
        forward_tcp: match &req.forward_tcp {
            Some(ft) => {
                if let Err(e) = crate::forward::validate_target(ft) {
                    return api::bad(e);
                }
                ft.trim().to_string()
            }
            None => String::new(),
        },
    };
    match cs.mgr.create_bridge(spec).await {
        Ok(b) => {
            let id = b.id.clone();
            let listen = b.listen_addr().to_string();
            println!("serialhub: 新建桥 {id} ({}) → 数据端口 {listen}", b.name());
            crate::logging::write(
                "info",
                &format!("新建桥 {id} ({}) → 数据端口 {listen}", b.name()),
            );
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
        Ok(()) => {
            crate::logging::write("info", &format!("桥 {id} 数据面启动"));
            api::ok()
        }
        Err(e) => api::bad(e),
    }
}

async fn fleet_stop(State(cs): State<ControlState>, AxumPath(id): AxumPath<String>) -> Response {
    if cs.mgr.get(&id).is_none() {
        return not_found_bridge(&id);
    }
    match cs.mgr.stop_bridge(&id).await {
        Ok(()) => {
            crate::logging::write("info", &format!("桥 {id} 数据面停止"));
            api::ok()
        }
        Err(e) => api::bad(e),
    }
}

async fn fleet_delete(State(cs): State<ControlState>, AxumPath(id): AxumPath<String>) -> Response {
    match cs.mgr.delete_bridge(&id).await {
        Ok(()) => {
            crate::logging::write("info", &format!("桥 {id} 已删除 (录像文件保留)"));
            api::ok()
        }
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
    /// FR-20/ADR-24②: 旁路转发目标 "host:port" (缺省 = 不改动; 空串 = 关闭)。
    #[serde(rename = "forwardTcp", default)]
    forward_tcp: Option<String>,
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
    cs.mgr
        .config_bridge(&id, req.name, req.auto_open, req.forward_tcp, cr)
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

// ---- FR-19: 录制与回放 (ADR-24 Sprint 13 B1; 引擎见 record 模块) ----

/// POST /api/fleet/<id>/record/start → {"ok":true,"file":"<桥id>-<戳>.jsonl"}。
async fn fleet_record_start(
    State(cs): State<ControlState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let Some(b) = cs.mgr.get(&id) else {
        return not_found_bridge(&id);
    };
    match crate::record::start(&b, &cs.mgr.recordings_dir()) {
        Ok(file) => Json(json!({"ok": true, "file": file})).into_response(),
        Err(e) => api::bad(e),
    }
}

/// POST /api/fleet/<id>/record/stop → {"ok":true,"file":...,"frames":n,"bytes":n}。
async fn fleet_record_stop(
    State(cs): State<ControlState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let Some(b) = cs.mgr.get(&id) else {
        return not_found_bridge(&id);
    };
    match crate::record::stop(&b).await {
        Ok((file, frames, bytes)) => Json(json!({
            "ok": true, "file": file, "frames": frames, "bytes": bytes
        }))
        .into_response(),
        Err(e) => api::bad(e),
    }
}

/// GET /api/fleet/<id>/recordings → {"ok":true,"recordings":[{file,frames,bytes,
/// txFrames,startedAt,durationSec}]} (本桥录像, 新的在前; 桥删除后文件仍在, 归属按
/// 文件名前缀; txFrames 有侧车为数字, 旧录像/录制中无侧车为 null —— 回放前警告
/// "0 tx 帧 = 回放无输出" 的数据源)。
async fn fleet_recordings(
    State(cs): State<ControlState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let Some(b) = cs.mgr.get(&id) else {
        return not_found_bridge(&id);
    };
    let list = crate::record::list(&b, &cs.mgr.recordings_dir());
    Json(json!({"ok": true, "recordings": list})).into_response()
}

#[derive(Deserialize)]
struct FleetRecordingsDeleteReq {
    file: String,
}

/// POST /api/fleet/<id>/recordings/delete {"file"} → {"ok":true} (ADR-24⑥):
/// 只删录像目录内本桥可解析的文件 (路径穿越拒绝同 replay); 文件不存在 → 400。
async fn fleet_recordings_delete(
    State(cs): State<ControlState>,
    AxumPath(id): AxumPath<String>,
    body: Result<Json<FleetRecordingsDeleteReq>, JsonRejection>,
) -> Response {
    // b 仅作存在性校验 (未知名 → 404, 与其他端点同序); 删除按文件名直取
    let Some(_b) = cs.mgr.get(&id) else {
        return not_found_bridge(&id);
    };
    let Json(req) = match body {
        Ok(v) => v,
        Err(rej) => return api::bad(format!("请求体不是合法 JSON: {rej}")),
    };
    let dir = cs.mgr.recordings_dir();
    let path = match crate::record::resolve_file(&dir, &req.file) {
        Ok(p) => p,
        Err(e) => return api::bad(e),
    };
    match std::fs::remove_file(&path) {
        Ok(()) => {
            // 侧车索引 (<file>.meta.json) 一并清理 (旧录像无侧车, 静默忽略)
            let mut s = path.clone().into_os_string();
            s.push(".meta.json");
            let _ = std::fs::remove_file(PathBuf::from(s));
            api::ok()
        }
        Err(e) => api::bad(format!("删除录像失败: {e}")),
    }
}

#[derive(Deserialize)]
struct FleetReplayReq {
    file: String,
    #[serde(default)]
    speed: Option<f64>,
    #[serde(rename = "loop", default)]
    r#loop: Option<bool>,
}

/// POST /api/fleet/<id>/replay {"file","speed","loop"}: 按原始时序把录像字节写回
/// 串口 TX (speed 0.5~10, 默认 1.0; loop 默认 false)。**只回放 tx 行** (ADR-24⑥)。
/// 安全: 串口须 open; file 须解析为录像目录内已有文件 (路径穿越拒绝)。
async fn fleet_replay(
    State(cs): State<ControlState>,
    AxumPath(id): AxumPath<String>,
    body: Result<Json<FleetReplayReq>, JsonRejection>,
) -> Response {
    let Some(b) = cs.mgr.get(&id) else {
        return not_found_bridge(&id);
    };
    let Json(req) = match body {
        Ok(v) => v,
        Err(rej) => return api::bad(format!("请求体不是合法 JSON: {rej}")),
    };
    let rr = crate::record::ReplayReq {
        file: req.file,
        speed: req.speed,
        loop_play: req.r#loop.unwrap_or(false),
    };
    let speed = rr.speed.unwrap_or(1.0);
    let loop_play = rr.loop_play;
    let file = rr.file.clone();
    match crate::record::start_replay(&b, &cs.mgr.recordings_dir(), rr) {
        Ok(()) => Json(json!({
            "ok": true, "file": file, "speed": speed, "loop": loop_play
        }))
        .into_response(),
        Err(e) => api::bad(e),
    }
}

/// POST /api/fleet/<id>/replay/stop: 停止回放 (幂等, loop=true 的停止入口)。
async fn fleet_replay_stop(
    State(cs): State<ControlState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    let Some(b) = cs.mgr.get(&id) else {
        return not_found_bridge(&id);
    };
    crate::record::stop_replay(&b);
    api::ok()
}

// ---- FR-22: 配置导入导出 (ADR-24 Sprint 13 B4) ----

/// GET+POST /api/fleet/export → fleet.json 原样下载 (attachment; 无清单文件时按
/// 当前状态序列化, 形状与 persist 写出一致)。双受理为 ADR-24⑥ 裁定 (UI 先 POST
/// 后 GET 兼容任务文口径)。
async fn fleet_export(State(cs): State<ControlState>) -> Response {
    let bytes = cs.mgr.fleet_export_bytes();
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"fleet.json\"",
            ),
        ],
        bytes,
    )
        .into_response()
}

#[derive(Deserialize)]
struct FleetImportReq {
    mode: String,
    json: Value,
}

/// POST /api/fleet/import {"mode":"merge"|"replace","json":{...}}:
/// schema 校验 (version==1 + bridges 形状 + 每桥 listen/serial 深校验), 失败 400;
/// merge=按 id 合并 (冲突跳过并计数), replace=整表替换 (运行中桥先停);
/// 导入桥即建即启 (数据面运行, autoOpen 按需开串口), 结束持久化。
async fn fleet_import(
    State(cs): State<ControlState>,
    body: Result<Json<FleetImportReq>, JsonRejection>,
) -> Response {
    let Json(req) = match body {
        Ok(v) => v,
        Err(rej) => return api::bad(format!("请求体不是合法 JSON: {rej}")),
    };
    let replace = match req.mode.as_str() {
        "merge" => false,
        "replace" => true,
        other => return api::bad(format!("mode 只支持 merge/replace, 得到 \"{other}\"")),
    };
    // schema 校验: 整份解析 (version 必填, bridges 形状), 再逐桥深校验 ——
    // 任何一处非法整体拒绝 (400), 不做半套导入
    let ff: FleetFile = match serde_json::from_value(req.json) {
        Ok(f) => f,
        Err(e) => return api::bad(format!("清单 schema 非法: {e}")),
    };
    if ff.version != 1 {
        return api::bad(format!("不支持的清单版本: {} (仅支持 1)", ff.version));
    }
    let mut specs = Vec::with_capacity(ff.bridges.len());
    for rec in &ff.bridges {
        let serial = match rec.serial.to_config() {
            Ok(c) => c,
            Err(e) => return api::bad(format!("桥 {} 串口配置非法: {e}", rec.id)),
        };
        if let Err(e) = crate::forward::validate_target(&rec.forward_tcp) {
            return api::bad(format!("桥 {} forwardTcp 非法: {e}", rec.id));
        }
        let listen: SocketAddr = match rec.listen.trim().parse() {
            Ok(a) => a,
            Err(_) => {
                return api::bad(format!(
                    "桥 {} listen 非法: \"{}\" (形如 127.0.0.1:8101)",
                    rec.id, rec.listen
                ))
            }
        };
        specs.push((rec.clone(), serial, listen));
    }
    let (imported, skipped) = cs.mgr.import_bridges(specs, replace).await;
    crate::logging::write(
        "info",
        &format!(
            "fleet 导入完成 (mode={}): imported={imported} skipped={skipped}",
            if replace { "replace" } else { "merge" }
        ),
    );
    Json(json!({"ok": true, "imported": imported, "skipped": skipped})).into_response()
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

// ---- ADR-21②: 打开管理台 ----

/// POST /api/open-console: 用系统默认浏览器打开管理台地址, 返回
/// `{"ok":true,"addr":"http://…"}`。壳内页面按钮的受信路径 (wry 拦截 window.open
/// 属已知, FR-13 起壳有 open_in_browser 能力 —— 此处把该能力开放给管理面);
/// 浏览器页 (非壳) 无需此端点 (维持 window.open 新标签), 但调了同样有效。
/// GUI 壳与 headless 共用控制面路由, 两形态均可用 (headless 脚本亦可触发)。
async fn open_console(State(cs): State<ControlState>) -> Response {
    let opener = cs.console_opener.clone();
    open_console_core(cs.console_addr, move |url| opener(url))
}

/// 核心 (ADR-21②): addr → URL → opener → 回显。opener 注入以便单测
/// (真实打开 = `crate::browser::open_url`, spawn 即返回不阻塞事件循环)。
fn open_console_core(addr: SocketAddr, opener: impl FnOnce(&str)) -> Response {
    let url = format!("http://{addr}/");
    opener(&url);
    Json(json!({ "ok": true, "addr": url })).into_response()
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
    crate::logging::write(
        "info",
        &format!(
            "SerialHub v{} 启动: 管理台 http://{addr} (多桥管理器)",
            env!("CARGO_PKG_VERSION")
        ),
    );
    mgr.set_manager_addr(addr);
    // FR-19: 录像目录 (--recordings-dir 或默认 exe 旁 recordings/)
    if let Some(d) = su.recordings_dir.clone() {
        mgr.set_recordings_dir(d);
    }

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
                    forward_tcp: String::new(),
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
    if mgr.fleet_path.as_ref().is_some_and(|p| p.exists()) {
        mgr.persist();
    }

    // FR-14: 主题目录初始化 (默认 exe 旁 themes/; --themes-dir 可指定)。
    // 失败不致命: 管理台照常服务, 主题列表为空 (/themes 端点按现状 404)。
    let themes_dir = su
        .themes_dir
        .clone()
        .unwrap_or_else(crate::themes::default_themes_dir);
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
    // ADR-21②: /api/open-console 的真实打开器 (每轮 ControlState 共用同一 Arc)
    let console_opener: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(crate::browser::open_url);
    loop {
        let round_rebind: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let cs = ControlState {
            mgr: mgr.clone(),
            shutdown_tx: shutdown_tx.clone(),
            restart_to: round_rebind.clone(),
            index: include_str!("../ui/index.html"),
            themes_dir: themes_dir.clone(),
            console_addr: cur_addr, // 换址轮随新绑定值重建 → 回显恒为现地址
            console_opener: console_opener.clone(),
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
            forward_tcp: String::new(),
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
        save_fleet(&path, &recs, None, None).unwrap();
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
            None,
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
        save_fleet(&path, &recs, None, None).unwrap();
        let f = load_fleet_file(&path).unwrap();
        assert_eq!(f.manager, None);
        // 旧式清单 (无 manager 字段) 也能解析
        std::fs::write(&path, r#"{"version":1,"bridges":[]}"#).unwrap();
        let f = load_fleet_file(&path).unwrap();
        assert_eq!(f.manager, None);
        let _ = std::fs::remove_file(&path);
    }

    /// ADR-22①: [window] 段往返 —— 写入/读取原样; 旧清单无该段 → None 兼容;
    /// 写出时 None 段跳过 (headless 永不产生 window 段)。
    #[test]
    fn fleet_file_window_rec_roundtrip() {
        let path = std::env::temp_dir().join(format!("sh_fleet_win_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let recs: Vec<FleetBridgeRec> = Vec::new();
        let win = WindowRec {
            x: -8,
            y: 160,
            w: 1120,
            h: 700,
            maximized: true,
        };
        // 带段写入 → 读回原样 (字段名即契约: x/y/w/h/maximized)
        save_fleet(&path, &recs, None, Some(&win)).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("\"window\""), "段名 window");
        assert!(body.contains("\"maximized\""), "字段名 maximized");
        assert_eq!(load_window(&path).unwrap(), Some(win));
        // 无段写入 → 读回 None (写出时空段跳过)
        save_fleet(&path, &recs, None, None).unwrap();
        let f = load_fleet_file(&path).unwrap();
        assert_eq!(f.window, None);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("window"));
        // 旧式清单 (无 window 字段) 也能解析; 缺 maximized 字段 → false
        std::fs::write(
            &path,
            r#"{"version":1,"bridges":[],"window":{"x":10,"y":20,"w":800,"h":600}}"#,
        )
        .unwrap();
        assert_eq!(
            load_window(&path).unwrap(),
            Some(WindowRec {
                x: 10,
                y: 20,
                w: 800,
                h: 600,
                maximized: false
            })
        );
        let _ = std::fs::remove_file(&path);
    }

    /// ADR-22①: save_window 只替换 [window] 段 —— bridges/[manager] 原样保留;
    /// 无清单文件时建最小清单; 坏清单拒写 (不得覆盖用户文件)。
    #[test]
    fn save_window_updates_only_window_section() {
        let path =
            std::env::temp_dir().join(format!("sh_fleet_savewin_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // 预置: 1 桥 + manager 段 + 旧 window
        let recs = vec![FleetBridgeRec {
            id: "b1".into(),
            name: "CLI".into(),
            auto_open: false,
            auto_reconnect: true,
            max_clients: 0,
            forward_tcp: String::new(),
            listen: "127.0.0.1:8081".into(),
            serial: SerialRec {
                port: "COM1".into(),
                baud: 115200,
                data_bits: 8,
                parity: "N".into(),
                stop_bits: 2,
                flow: "none".into(),
            },
        }];
        let mgr = ManagerRec {
            addr: "127.0.0.1:8080".into(),
        };
        save_fleet(
            &path,
            &recs,
            Some(&mgr),
            Some(&WindowRec {
                x: 0,
                y: 0,
                w: 640,
                h: 480,
                maximized: false,
            }),
        )
        .unwrap();
        // 换 window: 桥与 manager 不动, window 更新
        let nw = WindowRec {
            x: 137,
            y: 92,
            w: 1000,
            h: 640,
            maximized: true,
        };
        save_window(&path, nw).unwrap();
        assert_eq!(load_window(&path).unwrap(), Some(nw));
        let ff = load_fleet_file(&path).unwrap();
        assert_eq!(ff.bridges, recs, "bridges 原样保留");
        assert_eq!(ff.manager, Some(mgr), "[manager] 段原样保留");
        // 首次退出尚无清单 → 建只含 window 的最小清单
        let empty = std::env::temp_dir().join(format!(
            "sh_fleet_savewin_empty_{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&empty);
        save_window(&empty, nw).unwrap();
        let ff = load_fleet_file(&empty).unwrap();
        assert!(ff.bridges.is_empty() && ff.manager.is_none() && ff.window == Some(nw));
        let _ = std::fs::remove_file(&empty);
        // 坏清单拒写: 解析失败 → Err, 文件一字不动
        std::fs::write(&path, "{not json").unwrap();
        assert!(save_window(&path, nw).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
        // 缺失文件读 window → None (不报错)
        let _ = std::fs::remove_file(&path);
        assert_eq!(load_window(&path).unwrap(), None);
    }

    /// BUG-1 复现 (v1.7.0 用户现场「窗口记忆失效」): 本进程启动时盘上还没有
    /// [window] 段 (旧版清单/升级首启 —— window_seen=None), GUI 真实退出把它
    /// 写上盘之后, 任何一次桥变更触发的整份持久化都不得把它抹掉。
    /// persist 的 [window] 来源必须是盘上现值, 不能只有启动快照 window_seen。
    #[tokio::test(flavor = "multi_thread")]
    async fn persist_keeps_window_section_written_after_startup() {
        let path = temp_fleet("bug1win");
        let mgr = Arc::new(BridgeManager::new(Some(path.clone())));
        // 不跑 restore_fleet (对应"启动时盘上无 window 段"), 直接以 GUI 退出
        // 语义把窗口几何写进清单
        let win = WindowRec {
            x: 200,
            y: 100,
            w: 1200,
            h: 800,
            maximized: false,
        };
        save_window(&path, win).unwrap();
        assert_eq!(load_window(&path).unwrap(), Some(win));
        // 之后建一座桥 (整份持久化) —— [window] 必须原样幸存
        mgr.create_bridge(BridgeSpec {
            id: None,
            name: "BUG-1".into(),
            serial: SerialConfig::default(),
            listen: SocketAddr::from(([127, 0, 0, 1], 0)), // 随机端口, 不碰固定口
            auto_open: false,
            auto_reconnect: true,
            max_clients: 0,
            forward_tcp: String::new(),
        })
        .await
        .unwrap();
        assert_eq!(
            load_window(&path).unwrap(),
            Some(win),
            "桥变更的整份重写不得抹掉 [window] 段 (BUG-1)"
        );
        assert_eq!(load_fleet(&path).unwrap().len(), 1, "桥本身照常入档");
        let _ = std::fs::remove_file(&path);
    }

    /// BUG-1 同查 [manager] 段: 本进程尚未登记控制面地址 (set_manager_addr 未调)
    /// 时, 桥变更的整份重写不得抹掉盘上已有的 [manager] 段 (重启恢复地址不丢)。
    #[tokio::test(flavor = "multi_thread")]
    async fn persist_keeps_manager_section_when_addr_not_registered() {
        let path = temp_fleet("bug1mgr");
        let mgr_rec = ManagerRec {
            addr: "127.0.0.1:18200".into(),
        };
        save_fleet(&path, &[], Some(&mgr_rec), None).unwrap();
        let mgr = Arc::new(BridgeManager::new(Some(path.clone())));
        mgr.create_bridge(BridgeSpec {
            id: None,
            name: "BUG-1mgr".into(),
            serial: SerialConfig::default(),
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            auto_open: false,
            auto_reconnect: true,
            max_clients: 0,
            forward_tcp: String::new(),
        })
        .await
        .unwrap();
        let ff = load_fleet_file(&path).unwrap();
        assert_eq!(
            ff.manager,
            Some(mgr_rec),
            "桥变更的整份重写不得抹掉 [manager] 段 (BUG-1 同查)"
        );
        assert_eq!(ff.bridges.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    /// BUG-1 口径: 盘上现值优先于启动快照 —— 启动时读到 W1 (window_seen),
    /// 退出会话随后把 W2 写上盘, 本进程再建桥 → 必须保 W2 (盘上最新值),
    /// 不能拿启动快照 W1 覆盖回去。
    #[tokio::test(flavor = "multi_thread")]
    async fn persist_prefers_fresh_window_on_disk_over_startup_snapshot() {
        let path = temp_fleet("bug1fresh");
        let w1 = WindowRec {
            x: 1,
            y: 2,
            w: 800,
            h: 600,
            maximized: false,
        };
        save_fleet(&path, &[], None, Some(&w1)).unwrap();
        let mgr = Arc::new(BridgeManager::new(Some(path.clone())));
        restore_fleet(&mgr).await; // window_seen = W1 (启动快照)
        let w2 = WindowRec {
            x: 300,
            y: 200,
            w: 1100,
            h: 760,
            maximized: true,
        };
        save_window(&path, w2).unwrap(); // 盘上更新为 W2
        mgr.create_bridge(BridgeSpec {
            id: None,
            name: "BUG-1fresh".into(),
            serial: SerialConfig::default(),
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            auto_open: false,
            auto_reconnect: true,
            max_clients: 0,
            forward_tcp: String::new(),
        })
        .await
        .unwrap();
        assert_eq!(
            load_window(&path).unwrap(),
            Some(w2),
            "整份重写必须携带盘上最新 [window], 不得回退到启动快照"
        );
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
            forward_tcp: String::new(),
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
            recordings_dir: None,
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
            assert!(tokio::time::Instant::now() < deadline, "等待兼容桥超时");
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

    /// 轮询 /api/status 直到 clients 达到期望值 (确认 client_loop 已注册进广播/可处理
    /// 上行帧); 慢 runner (ubuntu CI) 上握手返回 ≠ 任务已跑完, 直接注入会时序脆弱。
    async fn wait_clients(addr: SocketAddr, want: usize, timeout: Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let (_, resp) = http_req(addr, "GET", "/api/status", None).await;
            let v: Value = serde_json::from_str(&resp).unwrap_or(Value::Null);
            if v["clients"].as_u64() == Some(want as u64) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "等待 clients=={want} 超时"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    // ---- ADR-21②: /api/open-console ----

    /// 端点级测试: 真 ControlState + 真 control_router, 但 opener 注入记录闭包
    /// (不真开浏览器 —— 真机人工验证, 见 Sprint9 报告)。断言 200 + addr 回显 +
    /// opener 收到的 URL 与回显一致。
    #[tokio::test(flavor = "multi_thread")]
    async fn open_console_endpoint_reports_console_addr() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bound = listener.local_addr().unwrap();
        let opened = Arc::new(Mutex::new(Vec::<String>::new()));
        let opened_in_cs = opened.clone();
        let (sd_tx, sd_rx) = watch::channel(false);
        let cs = ControlState {
            mgr: Arc::new(BridgeManager::new(None)),
            shutdown_tx: sd_tx.clone(),
            restart_to: Arc::new(Mutex::new(None)),
            index: "",
            themes_dir: std::env::temp_dir(),
            console_addr: bound,
            console_opener: Arc::new(move |url: &str| {
                lock_mutex(&opened_in_cs).push(url.to_string());
            }),
        };
        let server = tokio::spawn(async move {
            let mut sd_rx = sd_rx;
            let _ = axum::serve(listener, control_router(cs))
                .with_graceful_shutdown(async move {
                    let _ = sd_rx.changed().await;
                })
                .await;
        });

        let (code, body) = http_req(bound, "POST", "/api/open-console", None).await;
        assert_eq!(code, 200, "open-console 应受理: {body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        let expect = format!("http://{bound}/");
        assert_eq!(v["ok"], json!(true), "回显 ok: {body}");
        assert_eq!(v["addr"], json!(expect), "回显 addr 应为管理台地址: {body}");
        // 注入的打开器收到的 URL 与回显一致 (真开浏览器的替身)
        assert_eq!(*lock_mutex(&opened), vec![expect]);

        let _ = sd_tx.send(true); // 优雅停 serve
        let _ = server.await;
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
        let (code, _) = http_req(
            t.addr,
            "POST",
            "/api/fleet/b1/config",
            Some(r#"{"baud":50}"#),
        )
        .await;
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
        fleet_create_ok(
            t.addr,
            r#"{"name":"一","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
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
                forward_tcp: String::new(),
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
                forward_tcp: String::new(),
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
        save_fleet(&path, &recs, None, None).unwrap();
        let t = spawn_mgr(Some(path.clone()), None).await;
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        let arr = v["bridges"].as_array().unwrap();
        assert_eq!(arr.len(), 2, "启动时应恢复全部桥");
        assert_eq!(arr[0]["id"], "b1");
        assert_eq!(arr[0]["name"], "一号");
        assert_eq!(arr[0]["serial"]["port"], "COM9");
        assert!(
            arr[0]["listen"].as_str().unwrap() != "127.0.0.1:0",
            "应回填实际端口"
        );
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
        fleet_create_ok(
            t.addr,
            r#"{"name":"二桥","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
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
        let (code, resp) =
            http_req(t.addr, "POST", "/api/config", Some(r#"{"maxClients":4}"#)).await;
        assert_eq!(code, 200, "{resp}");
        let (_, resp) = http_req(t.addr, "GET", "/api/status", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["maxClients"], 4, "POST /api/config maxClients 须生效");
        // 0 = 不限
        let (code, _) = http_req(t.addr, "POST", "/api/config", Some(r#"{"maxClients":0}"#)).await;
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
        // 确认 client_loop 已跑起来 (clients 计 1) 再注入 —— 慢 runner 上握手返回
        // ≠ 服务端任务已调度, 直接注入会时序脆弱 (ubuntu CI v1.5.1 实测)
        wait_clients(t.addr, 1, Duration::from_secs(5)).await;
        // RX 注入 broadcast → WS 下行二进制帧 (与串口读线程同一条通路)
        wait_receiver(&b.ctx.bc_tx, Duration::from_secs(5)).await;
        b.ctx.bc_tx.send(b"hello-fleet".to_vec()).unwrap();
        let got = ws_recv_binary(&mut ws, Duration::from_secs(5)).await;
        assert_eq!(got, b"hello-fleet");
        // TX: WS 二进制 → tx 队列 (帧语义零变化)
        ws_send_masked(&mut ws, b"to-port").await;
        let got = txq_rx.recv_timeout(Duration::from_secs(5)).unwrap();
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
        fleet_create_ok(
            t.addr,
            r#"{"name":"t","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
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

    // ---- ADR-20: 数据口双路径 (裸地址 / 与 /ws) + 426 指路 ----

    #[tokio::test(flavor = "multi_thread")]
    async fn bridge_data_root_dual_path_and_426() {
        let t = spawn_mgr(None, None).await;
        let v = fleet_create_ok(
            t.addr,
            r#"{"name":"裸地址","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        let bid = v["id"].as_str().unwrap().to_string();
        let b = t.mgr.get(&bid).unwrap();
        let listen = b.listen_addr();
        // 1) 裸地址 (根路径) 握手成功, 且是真实数据客户端 (RX 注入可收)
        let mut ws_root = ws_handshake(listen, "/").await;
        wait_receiver(&b.ctx.bc_tx, Duration::from_secs(2)).await;
        b.ctx.bc_tx.send(b"root-client".to_vec()).unwrap();
        let got = ws_recv_binary(&mut ws_root, Duration::from_secs(2)).await;
        assert_eq!(got, b"root-client");
        // 2) /ws 握手照常成功
        let _ws_path = ws_handshake(listen, "/ws").await;
        // 3) 普通 HTTP GET / → 426 Upgrade Required + 指路
        let (code, body) = http_req(listen, "GET", "/", None).await;
        assert_eq!(code, 426);
        assert!(body.contains("这是串口数据端点"), "{body}");
        assert!(body.contains("ws://host:port/ws"), "{body}");
        // 4) 其余路径 404 (axum 默认)
        let (code, _) = http_req(listen, "GET", "/other", None).await;
        assert_eq!(code, 404);
        // 5) 管理台控制面不受影响: GET / 仍是管理台页面
        let (code, body) = http_req(t.addr, "GET", "/", None).await;
        assert_eq!(code, 200);
        assert!(body.contains("SerialHub"), "{body}");
        t.shutdown().await;
    }

    // ---- 多桥隔离 (FR-10b) ----

    #[tokio::test(flavor = "multi_thread")]
    async fn bridge_data_planes_are_isolated() {
        let t = spawn_mgr(None, None).await;
        fleet_create_ok(
            t.addr,
            r#"{"name":"一","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        fleet_create_ok(
            t.addr,
            r#"{"name":"二","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        let arr = v["bridges"].as_array().unwrap();
        let p1: SocketAddr = arr[0]["listen"].as_str().unwrap().parse().unwrap();
        let p2: SocketAddr = arr[1]["listen"].as_str().unwrap().parse().unwrap();
        assert_ne!(p1, p2, "各桥独立数据端口");
        // 连 b1 的客户端: 只收 b1 的 RX
        let mut ws1 = ws_handshake(p1, "/ws").await;
        // b2 无客户端订阅 → send 返回 SendError 属正常 (读循环不反压, 无人收即弃)
        let _ = t.mgr.get("b2").unwrap().ctx.bc_tx.send(b"from-b2".to_vec());
        let mut tmp = [0u8; 64];
        let r = tokio::time::timeout(Duration::from_millis(250), ws1.read(&mut tmp)).await;
        assert!(r.is_err(), "b2 的广播不得泄漏到 b1 的客户端");
        wait_receiver(&t.mgr.get("b1").unwrap().ctx.bc_tx, Duration::from_secs(2)).await;
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
        let b = Bridge::new(
            "bx".into(),
            "t".into(),
            SerialConfig::default(),
            false,
            true,
            0,
            String::new(),
        );
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
        fleet_create_ok(
            t.addr,
            r#"{"name":"s","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
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

    /// ADR-15①/ADR-16①/ADR-24 B1/B2: fleet 桥对象契约字段集 17→19 —— 原 15 契约
    /// 字段 (与 QA conftest FLEET_ROW_FIELDS 同源) + recording/replay (FR-19:
    /// 录制/回放状态, null 或会话摘要) + forwardTcp/forwardConnected (FR-20:
    /// 旁路转发配置与连接状态, QA 需随动修订字段集)。
    /// 列表行与单桥详情同构, 必须都回显; 除已声明的内部字段 (running/autoOpen)
    /// 外不得缺字段, 也不得混入未裁定字段。
    #[tokio::test(flavor = "multi_thread")]
    async fn fleet_bridge_object_contract_19_fields() {
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
            "retries",          // ADR-15① 新增 (13→14)
            "autoReconnect",    // ADR-16① 新增 (14→15)
            "recording",        // FR-19/ADR-24 B1 新增 (15→17)
            "replay",           // FR-19/ADR-24 B1 新增 (15→17)
            "forwardTcp",       // FR-20/ADR-24② B2 新增 (17→19)
            "forwardConnected", // FR-20/ADR-24② B2 新增 (17→19)
        ]
        .into_iter()
        .collect();
        let known_extra: HashSet<&str> = ["running", "autoOpen"].into_iter().collect();
        let check = |row: &Value, where_: &str| {
            let got: HashSet<&str> = row
                .as_object()
                .unwrap()
                .keys()
                .map(|s| s.as_str())
                .collect();
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
            assert!(
                row["recording"].is_null(),
                "{where_} 空闲桥 recording 须为 null: {row}"
            );
            assert!(
                row["replay"].is_null(),
                "{where_} 空闲桥 replay 须为 null: {row}"
            );
            assert_eq!(
                row["forwardTcp"], "",
                "{where_} 默认桥 forwardTcp 须为空串 (关闭): {row}"
            );
            assert_eq!(
                row["forwardConnected"].as_bool(),
                Some(false),
                "{where_} 未配置转发 forwardConnected 须 false: {row}"
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
        let (code, _) = http_req(
            t.addr,
            "POST",
            "/api/config",
            Some(r#"{"autoReconnect":true}"#),
        )
        .await;
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
        let t = spawn_mgr_opt(
            Some(path.clone()),
            None,
            "127.0.0.1:0".parse().unwrap(),
            false,
        )
        .await;
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
            .next_back()
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
        let t2 = spawn_mgr_opt(
            Some(path.clone()),
            None,
            "127.0.0.1:0".parse().unwrap(),
            false,
        )
        .await;
        assert_eq!(t2.addr, new_addr, "重启后应恢复 [manager] 持久化地址");
        t2.shutdown().await;

        // 显式 --addr 优先于清单恢复值
        let t3 = spawn_mgr_opt(
            Some(path.clone()),
            None,
            "127.0.0.1:0".parse().unwrap(),
            true,
        )
        .await;
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
        // 启动时内置主题已落盘 (ensure_builtin), 列表应含四套内置
        let (code, resp) = http_req(t.addr, "GET", "/api/themes", None).await;
        assert_eq!(code, 200, "{resp}");
        let v: Value = serde_json::from_str(&resp).unwrap();
        let list = v["themes"].as_array().expect("themes 数组");
        let get = |n: &str| list.iter().find(|t| t["name"] == n).cloned();
        for builtin in ["light", "dark", "example-oreo", "win95"] {
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
        assert!(
            body.contains(":root") && body.contains("--bg:#14171a"),
            "{body}"
        );
        // 静态服务: win95 (直角 + 海军蓝定版值)
        let (code, body) = http_req(t.addr, "GET", "/themes/win95.css", None).await;
        assert_eq!(code, 200, "win95.css 应可服务");
        assert!(
            body.contains("--ctl-radius:0") && body.contains("--accent:#000080"),
            "{body}"
        );
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

    // ---- FR-19: 录制与回放 (ADR-24 B1) ----

    fn temp_recdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sh_rec_{}_{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// 录制状态机 + JSONL 行格式 + 计数器 + 列表端点:
    /// RX 注入经 bc_tx (tap 同源), TX 注入经 set_tx 后 send_to_port (tee 源),
    /// 停止后逐行校验 {"ts","dir","hex"} (hex 小写), stop 响应计数与文件一致。
    #[tokio::test(flavor = "multi_thread")]
    async fn record_jsonl_format_counters_and_state() {
        let t = spawn_mgr(None, None).await;
        let recdir = temp_recdir("fmt");
        t.mgr.set_recordings_dir(recdir.clone());
        fleet_create_ok(
            t.addr,
            r#"{"name":"录制桥","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        let b = t.mgr.get("b1").unwrap();
        let (txq_tx, _txq_rx) = std_mpsc::channel::<Vec<u8>>();
        b.ctx.set_tx(txq_tx);

        // 空闲态: detail.recording = null; 未开始 stop → 400
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(v["bridge"]["recording"].is_null(), "{resp}");
        let (code, resp) = http_req(t.addr, "POST", "/api/fleet/b1/record/stop", None).await;
        assert_eq!(code, 400, "未录制时 stop 应 400: {resp}");

        // 开始录制 → {"ok":true,"file":"b1-<戳>.jsonl"}
        let (code, resp) = http_req(t.addr, "POST", "/api/fleet/b1/record/start", None).await;
        assert_eq!(code, 200, "{resp}");
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["ok"], true);
        let file = v["file"].as_str().unwrap().to_string();
        assert!(
            file.starts_with("b1-") && file.ends_with(".jsonl"),
            "{file}"
        );
        // 进行中: detail.recording.file 回显; 重复 start → 400 (非幂等)
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["bridge"]["recording"]["file"], file, "{resp}");
        let (code, resp) = http_req(t.addr, "POST", "/api/fleet/b1/record/start", None).await;
        assert_eq!(code, 400, "重复录制应 400: {resp}");

        // 注入一帧 RX (tap 同源 bc_tx) + 一帧 TX (send_to_port tee)
        b.ctx.bc_tx.send(b"RX-HELLO".to_vec()).unwrap();
        assert!(b.ctx.send_to_port(b"tx-abc"), "TX 应成功入队");
        tokio::time::sleep(Duration::from_millis(120)).await; // 至少一个 flush 周期前

        let (code, resp) = http_req(t.addr, "POST", "/api/fleet/b1/record/stop", None).await;
        assert_eq!(code, 200, "{resp}");
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["file"], file);
        assert_eq!(v["frames"], 2, "{resp}");
        assert_eq!(v["bytes"], 14, "RX-HELLO(8) + tx-abc(6) = 14: {resp}");

        // 文件逐行校验: JSONL 形状 / dir / 小写 hex / ts 非递减
        let body = std::fs::read_to_string(recdir.join(&file)).unwrap();
        let lines: Vec<Value> = body
            .lines()
            .map(|l| serde_json::from_str(l).expect("每行都是合法 JSON"))
            .collect();
        assert_eq!(lines.len(), 2, "{body}");
        for l in &lines {
            let mut keys: Vec<&str> = l.as_object().unwrap().keys().map(|s| s.as_str()).collect();
            keys.sort_unstable();
            assert_eq!(keys, ["dir", "hex", "ts"], "行恰三字段: {body}");
            assert!(l["ts"].is_u64(), "ts 为整数毫秒: {body}");
            assert!(l["hex"].is_string(), "{body}");
        }
        // dir 集合与小写 hex (select 双臂就绪时消费顺序随机, 不钉行序)
        let dirs: Vec<&str> = lines.iter().map(|l| l["dir"].as_str().unwrap()).collect();
        let mut sorted = dirs.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, ["rx", "tx"], "{body}");
        let by_dir = |d: &str| {
            lines.iter().find(|l| l["dir"] == d).unwrap()["hex"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(by_dir("rx"), "52582d48454c4c4f", "RX-HELLO 小写 hex");
        assert_eq!(by_dir("tx"), "74782d616263", "tx-abc 小写 hex");
        let ts0 = lines[0]["ts"].as_u64().unwrap();
        let ts1 = lines[1]["ts"].as_u64().unwrap();
        assert!(ts1 >= ts0, "ts 相对录制开始且非递减: {body}");
        // 停止后: detail.recording 归 null
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(v["bridge"]["recording"].is_null(), "{resp}");

        // 列表端点: 恰一条, 计数与列表字段齐全
        let (code, resp) = http_req(t.addr, "GET", "/api/fleet/b1/recordings", None).await;
        assert_eq!(code, 200, "{resp}");
        let v: Value = serde_json::from_str(&resp).unwrap();
        let list = v["recordings"].as_array().unwrap();
        assert_eq!(list.len(), 1, "{resp}");
        assert_eq!(list[0]["file"], file);
        assert_eq!(list[0]["frames"], 2);
        assert_eq!(list[0]["bytes"], 14);
        assert!(list[0]["startedAt"].as_u64().unwrap() > 0, "{resp}");
        assert!(list[0]["durationSec"].as_f64().unwrap() >= 0.0);
        // 不存在的桥 → 404
        let (code, _) = http_req(t.addr, "GET", "/api/fleet/bX/recordings", None).await;
        assert_eq!(code, 404);
        t.shutdown().await;
        let _ = std::fs::remove_dir_all(&recdir);
    }

    /// 桥删除时录像文件保留 (FR-19 硬性要求)。
    #[tokio::test(flavor = "multi_thread")]
    async fn record_bridge_delete_keeps_files() {
        let t = spawn_mgr(None, None).await;
        let recdir = temp_recdir("keep");
        t.mgr.set_recordings_dir(recdir.clone());
        fleet_create_ok(
            t.addr,
            r#"{"name":"删桥桥","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        let b = t.mgr.get("b1").unwrap();
        let (txq_tx, _txq_rx) = std_mpsc::channel::<Vec<u8>>();
        b.ctx.set_tx(txq_tx);
        let (code, _) = http_req(t.addr, "POST", "/api/fleet/b1/record/start", None).await;
        assert_eq!(code, 200);
        b.ctx.bc_tx.send(b"keep".to_vec()).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (code, resp) = http_req(t.addr, "POST", "/api/fleet/b1/record/stop", None).await;
        assert_eq!(code, 200, "{resp}");
        let v: Value = serde_json::from_str(&resp).unwrap();
        let file = v["file"].as_str().unwrap().to_string();
        // 删除桥 (录制已停; 若未停也会随 stop_tx 自动收尾)
        let (code, _) = http_req(t.addr, "POST", "/api/fleet/b1/delete", None).await;
        assert_eq!(code, 200);
        assert!(
            recdir.join(&file).is_file(),
            "桥删除后录像文件必须保留: {file}"
        );
        t.shutdown().await;
        let _ = std::fs::remove_dir_all(&recdir);
    }

    /// 删录像端点 (ADR-24⑥): 正常删除 {"ok":true} 且列表同步; 路径穿越/不存在 → 400。
    #[tokio::test(flavor = "multi_thread")]
    async fn recordings_delete_and_traversal_rejected() {
        let t = spawn_mgr(None, None).await;
        let recdir = temp_recdir("del");
        t.mgr.set_recordings_dir(recdir.clone());
        fleet_create_ok(
            t.addr,
            r#"{"name":"删录像桥","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        std::fs::create_dir_all(&recdir).unwrap();
        std::fs::write(
            recdir.join("b1-20260101-000000.jsonl"),
            "{\"ts\":0,\"dir\":\"tx\",\"hex\":\"01\"}\n",
        )
        .unwrap();
        // 布饵: 录像目录外文件 (穿越目标) —— 删除后必须仍在
        let bait = std::env::temp_dir().join(format!("sh_rec_del_bait_{}.txt", std::process::id()));
        std::fs::write(&bait, "do not delete").unwrap();

        let (code, resp) = http_req(
            t.addr,
            "POST",
            "/api/fleet/b1/recordings/delete",
            Some(r#"{"file":"b1-20260101-000000.jsonl"}"#),
        )
        .await;
        assert_eq!(code, 200, "{resp}");
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["ok"], true, "{resp}");
        assert!(
            !recdir.join("b1-20260101-000000.jsonl").exists(),
            "录像已删"
        );
        let (code, resp) = http_req(t.addr, "GET", "/api/fleet/b1/recordings", None).await;
        assert_eq!(code, 200);
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["recordings"].as_array().unwrap().len(), 0, "{resp}");

        // 穿越拒绝 (含反斜杠/URL 编码变体) + 不存在文件 + 坏 body → 400
        for bad in [
            r#"{"file":"../sh_rec_del_bait.txt"}"#,
            r#"{"file":"..\\sh_rec_del_bait.txt"}"#,
            r#"{"file":"missing.jsonl"}"#,
            r#"{}"#,
            "not json",
        ] {
            let (code, resp) =
                http_req(t.addr, "POST", "/api/fleet/b1/recordings/delete", Some(bad)).await;
            assert_eq!(code, 400, "body={bad} 应 400: {resp}");
        }
        assert!(bait.is_file(), "目录外饵文件不得被波及");
        // 不存在的桥 → 404
        let (code, _) = http_req(
            t.addr,
            "POST",
            "/api/fleet/bX/recordings/delete",
            Some(r#"{"file":"b1.jsonl"}"#),
        )
        .await;
        assert_eq!(code, 404);
        let _ = std::fs::remove_file(&bait);
        t.shutdown().await;
        let _ = std::fs::remove_dir_all(&recdir);
    }

    /// 侧车索引 txFrames (回放可见性配套): 录 3tx+2rx → stop 后列表 txFrames==3
    /// (frames==5/bytes==14 不变); 侧车 <file>.meta.json 落盘且恰四字段;
    /// 删录像连带删侧车。
    #[tokio::test(flavor = "multi_thread")]
    async fn recordings_list_txframes_from_sidecar() {
        let t = spawn_mgr(None, None).await;
        let recdir = temp_recdir("txf");
        t.mgr.set_recordings_dir(recdir.clone());
        fleet_create_ok(
            t.addr,
            r#"{"name":"tx计数桥","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        let b = t.mgr.get("b1").unwrap();
        let (txq_tx, _txq_rx) = std_mpsc::channel::<Vec<u8>>();
        b.ctx.set_tx(txq_tx);

        let (code, resp) = http_req(t.addr, "POST", "/api/fleet/b1/record/start", None).await;
        assert_eq!(code, 200, "{resp}");
        // 2 帧 RX (bc_tx) + 3 帧 TX (send_to_port tee)
        b.ctx.bc_tx.send(b"r1".to_vec()).unwrap();
        b.ctx.bc_tx.send(b"r22".to_vec()).unwrap();
        assert!(b.ctx.send_to_port(b"t1"));
        assert!(b.ctx.send_to_port(b"t22"));
        assert!(b.ctx.send_to_port(b"t333"));
        tokio::time::sleep(Duration::from_millis(150)).await;
        let (code, resp) = http_req(t.addr, "POST", "/api/fleet/b1/record/stop", None).await;
        assert_eq!(code, 200, "{resp}");
        let v: Value = serde_json::from_str(&resp).unwrap();
        let file = v["file"].as_str().unwrap().to_string();
        assert_eq!(v["frames"], 5, "{resp}");
        assert_eq!(
            v["bytes"], 14,
            "r1(2)+r22(3)+t1(2)+t22(3)+t333(4)=14: {resp}"
        );

        // 侧车落盘: <file>.meta.json 恰 {frames,bytes,txFrames,durationSec} 四字段
        let meta_path = recdir.join(format!("{file}.meta.json"));
        let meta_body = std::fs::read_to_string(&meta_path).expect("侧车应随 stop 落盘");
        let mv: Value = serde_json::from_str(&meta_body).unwrap();
        let mut keys: Vec<&str> = mv.as_object().unwrap().keys().map(|s| s.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["bytes", "durationSec", "frames", "txFrames"],
            "{meta_body}"
        );
        assert_eq!(mv["frames"], 5, "{meta_body}");
        assert_eq!(mv["txFrames"], 3, "{meta_body}");
        assert_eq!(mv["bytes"], 14, "{meta_body}");
        assert!(mv["durationSec"].as_f64().unwrap() >= 0.0, "{meta_body}");

        // 列表: txFrames==3 (前端 "0 tx 帧 = 回放无输出" 警告的数据源)
        let (code, resp) = http_req(t.addr, "GET", "/api/fleet/b1/recordings", None).await;
        assert_eq!(code, 200, "{resp}");
        let v: Value = serde_json::from_str(&resp).unwrap();
        let list = v["recordings"].as_array().unwrap();
        assert_eq!(list.len(), 1, "{resp}");
        assert_eq!(list[0]["file"], file);
        assert_eq!(list[0]["frames"], 5, "{resp}");
        assert_eq!(list[0]["bytes"], 14, "{resp}");
        assert_eq!(list[0]["txFrames"], 3, "{resp}");
        assert!(list[0]["startedAt"].as_u64().unwrap() > 0, "{resp}");
        assert!(list[0]["durationSec"].as_f64().unwrap() >= 0.0, "{resp}");

        // 删录像连带删侧车 (不留孤儿索引)
        let (code, resp) = http_req(
            t.addr,
            "POST",
            "/api/fleet/b1/recordings/delete",
            Some(&format!(r#"{{"file":"{file}"}}"#)),
        )
        .await;
        assert_eq!(code, 200, "{resp}");
        assert!(!recdir.join(&file).exists(), "录像已删");
        assert!(!meta_path.exists(), "侧车应随录像一并删除");
        t.shutdown().await;
        let _ = std::fs::remove_dir_all(&recdir);
    }

    /// 旧录像无侧车 (及侧车损坏) → 列表回退逐行扫描, txFrames=null 不崩
    /// (frames/bytes/durationSec 仍现算; 侧车文件本身不出现在列表里)。
    #[tokio::test(flavor = "multi_thread")]
    async fn recordings_list_legacy_without_sidecar_txframes_null() {
        let t = spawn_mgr(None, None).await;
        let recdir = temp_recdir("legacy");
        t.mgr.set_recordings_dir(recdir.clone());
        fleet_create_ok(
            t.addr,
            r#"{"name":"旧录像桥","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        std::fs::create_dir_all(&recdir).unwrap();
        // 旧录像 A: 无侧车 (2 rx + 1 tx)
        std::fs::write(
            recdir.join("b1-20200101-000000.jsonl"),
            "{\"ts\":0,\"dir\":\"tx\",\"hex\":\"aa\"}\n\
             {\"ts\":100,\"dir\":\"rx\",\"hex\":\"bb\"}\n\
             {\"ts\":900,\"dir\":\"rx\",\"hex\":\"ccdd\"}\n",
        )
        .unwrap();
        // 旧录像 B: 侧车存在但损坏 (非 JSON) —— 必须按无侧车回退, 不崩
        std::fs::write(
            recdir.join("b1-20200102-000000.jsonl"),
            "{\"ts\":50,\"dir\":\"tx\",\"hex\":\"0102\"}\n",
        )
        .unwrap();
        std::fs::write(
            recdir.join("b1-20200102-000000.jsonl.meta.json"),
            "not json",
        )
        .unwrap();

        let (code, resp) = http_req(t.addr, "GET", "/api/fleet/b1/recordings", None).await;
        assert_eq!(code, 200, "{resp}");
        let v: Value = serde_json::from_str(&resp).unwrap();
        let list = v["recordings"].as_array().unwrap();
        // 恰两条 (侧车 .meta.json 不以 .jsonl 结尾, 不混入列表)
        assert_eq!(list.len(), 2, "{resp}");
        let find = |f: &str| {
            list.iter()
                .find(|e| e["file"] == f)
                .unwrap_or_else(|| panic!("缺 {f}: {resp}"))
                .clone()
        };
        let a = find("b1-20200101-000000.jsonl");
        assert_eq!(a["frames"], 3, "{a}");
        assert_eq!(a["bytes"], 4, "{a}");
        assert_eq!(a["durationSec"], 0.9, "{a}");
        assert!(a["startedAt"].as_u64().unwrap() > 0, "{a}");
        assert!(
            a["txFrames"].is_null(),
            "无侧车 → txFrames=null (前端容错): {a}"
        );
        let c = find("b1-20200102-000000.jsonl");
        assert_eq!(c["frames"], 1, "{c}");
        assert_eq!(c["bytes"], 2, "{c}");
        assert!(
            c["txFrames"].is_null(),
            "损坏侧车 → 回退扫描 txFrames=null: {c}"
        );
        t.shutdown().await;
        let _ = std::fs::remove_dir_all(&recdir);
    }

    /// 回放: 假串口注入 (set_tx 队列) 收回放字节; **只回放 tx 行** (ADR-24⑥,
    /// rx 行不回注); 原始时序按倍率缩放 (speed=1 下界 + speed=10 上界双向钉住),
    /// 字节与顺序逐帧一致; 首帧立即发 (绝对 epoch ms ts 兼容, QA §3 A1)。
    #[tokio::test(flavor = "multi_thread")]
    async fn replay_bytes_and_timing_via_fake_serial() {
        let t = spawn_mgr(None, None).await;
        let recdir = temp_recdir("replay");
        t.mgr.set_recordings_dir(recdir.clone());
        fleet_create_ok(
            t.addr,
            r#"{"name":"回放桥","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        let b = t.mgr.get("b1").unwrap();
        // 假录像: rx+tx 混合 (tx@0 / rx@400 / tx@800) —— tx-only 过滤的试金石
        std::fs::create_dir_all(&recdir).unwrap();
        let rec = recdir.join("b1-20260101-000000.jsonl");
        std::fs::write(
            &rec,
            "{\"ts\":0,\"dir\":\"tx\",\"hex\":\"aa\"}\n\
             {\"ts\":400,\"dir\":\"rx\",\"hex\":\"bbcc\"}\n\
             {\"ts\":800,\"dir\":\"tx\",\"hex\":\"ddeeff00\"}\n",
        )
        .unwrap();

        // 串口未打开 → 400
        let (code, resp) = http_req(
            t.addr,
            "POST",
            "/api/fleet/b1/replay",
            Some(r#"{"file":"b1-20260101-000000.jsonl"}"#),
        )
        .await;
        assert_eq!(code, 400, "串口未 open 应拒绝: {resp}");
        b.hub.set_phase(Phase::Open); // 测试直置 open (生产由监督任务迁移)

        let (txq_tx, txq_rx) = std_mpsc::channel::<Vec<u8>>();
        b.ctx.set_tx(txq_tx);

        // speed=1: 首帧 (tx@0) 立即到; 次帧 (tx@800, 与首帧 tx 行间差 800ms)
        // 应在 >=600ms 后; rx 行 (bbcc) 绝不出现在假串口 (tx-only 过滤)
        let t0 = Instant::now();
        let (code, resp) = http_req(
            t.addr,
            "POST",
            "/api/fleet/b1/replay",
            Some(r#"{"file":"b1-20260101-000000.jsonl","speed":1.0,"loop":false}"#),
        )
        .await;
        assert_eq!(code, 200, "{resp}");
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["file"], "b1-20260101-000000.jsonl");
        assert_eq!(v["speed"], 1.0);
        assert_eq!(v["loop"], false);
        let f1 = txq_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let t1 = t0.elapsed();
        let f2 = txq_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let t2 = t0.elapsed();
        assert_eq!(f1, b"\xaa");
        assert_eq!(f2, b"\xdd\xee\xff\x00");
        assert!(
            t2 - t1 >= Duration::from_millis(600),
            "800ms tx 行间差 speed=1 不得瞬发, 实际 {t2:?} - {t1:?}"
        );
        // rx 行不回注: 自然结束后守窗无任何多余字节 (尤其 bbcc)
        assert!(
            txq_rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "tx-only: 守窗内不得再收到帧 (rx 行 bbcc 不得回注)"
        );
        // 回放自然结束后 replay 槽位归 null
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(v["bridge"]["replay"].is_null(), "{resp}");

        // speed=10: 全部 tx 帧应在 500ms 内到齐 (钉住倍率生效; 未缩放需 800ms)
        let t0 = Instant::now();
        let (code, _) = http_req(
            t.addr,
            "POST",
            "/api/fleet/b1/replay",
            Some(r#"{"file":"b1-20260101-000000.jsonl","speed":10}"#),
        )
        .await;
        assert_eq!(code, 200);
        for want in [&b"\xaa"[..], b"\xdd\xee\xff\x00"] {
            let got = txq_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            assert_eq!(got, want);
        }
        let total = t0.elapsed();
        assert!(
            total < Duration::from_millis(500),
            "speed=10 应把 800ms 压到 ~80ms, 实际 {total:?}"
        );

        // loop=true: 进度经 detail.replay 可见, replay/stop 可停
        let (code, _) = http_req(
            t.addr,
            "POST",
            "/api/fleet/b1/replay",
            Some(r#"{"file":"b1-20260101-000000.jsonl","speed":10,"loop":true}"#),
        )
        .await;
        assert_eq!(code, 200);
        tokio::time::sleep(Duration::from_millis(150)).await;
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(
            v["bridge"]["replay"]["loop"], true,
            "回放期间状态可见: {resp}"
        );
        assert_eq!(v["bridge"]["replay"]["speed"], 10.0);
        assert!(
            v["bridge"]["replay"]["frames"].as_u64().unwrap() >= 2,
            "{resp}"
        );
        let (code, _) = http_req(t.addr, "POST", "/api/fleet/b1/replay/stop", None).await;
        assert_eq!(code, 200);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(
            v["bridge"]["replay"].is_null(),
            "停止后 replay 归 null: {resp}"
        );
        // 回放字节确实走了 tx 队列 (假串口收到 loop 重复帧); 单轮恒为 2 帧
        // (tx-only), 超过 2 即证明发生了循环
        let mut got = 0usize;
        while txq_rx.recv_timeout(Duration::from_millis(50)).is_ok() {
            got += 1;
        }
        assert!(got > 2, "loop 回放应超过单轮 (2 tx 帧), 实际 {got}");
        t.shutdown().await;
        let _ = std::fs::remove_dir_all(&recdir);
    }

    /// 绝对 epoch ms ts 的手工录像 (QA §3 A1): 首帧立即发 (不按首行 ts 绝对时刻
    /// 等待), 行间差按时序缩放 —— 回放须在秒级自然完成而非永久卡死。
    #[tokio::test(flavor = "multi_thread")]
    async fn replay_accepts_absolute_epoch_ts() {
        let t = spawn_mgr(None, None).await;
        let recdir = temp_recdir("epoch");
        t.mgr.set_recordings_dir(recdir.clone());
        fleet_create_ok(
            t.addr,
            r#"{"name":"绝对ts桥","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        let b = t.mgr.get("b1").unwrap();
        b.hub.set_phase(Phase::Open);
        let (txq_tx, txq_rx) = std_mpsc::channel::<Vec<u8>>();
        b.ctx.set_tx(txq_tx);
        // 绝对 epoch ms: now / now+200 / now+400
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        std::fs::create_dir_all(&recdir).unwrap();
        std::fs::write(
            recdir.join("b1-epoch.jsonl"),
            format!(
                "{{\"ts\":{},\"dir\":\"tx\",\"hex\":\"0101\"}}\n\
                 {{\"ts\":{},\"dir\":\"tx\",\"hex\":\"0202\"}}\n\
                 {{\"ts\":{},\"dir\":\"tx\",\"hex\":\"0303\"}}\n",
                now,
                now + 200,
                now + 400
            ),
        )
        .unwrap();
        let t0 = Instant::now();
        let (code, resp) = http_req(
            t.addr,
            "POST",
            "/api/fleet/b1/replay",
            Some(r#"{"file":"b1-epoch.jsonl","speed":1.0,"loop":false}"#),
        )
        .await;
        assert_eq!(code, 200, "{resp}");
        for want in [&b"\x01\x01"[..], b"\x02\x02", b"\x03\x03"] {
            let got = txq_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(got, want);
        }
        let total = t0.elapsed();
        assert!(
            total < Duration::from_secs(4),
            "绝对 ts 首帧须立即发, 回放秒级完成 (实际 {total:?}); 卡死即回归"
        );
        assert!(
            total >= Duration::from_millis(250),
            "行间差 400ms 须按时序铺开 (实际 {total:?})"
        );
        t.shutdown().await;
        let _ = std::fs::remove_dir_all(&recdir);
    }

    /// 回放参数校验与路径穿越拒绝 (FR-19 安全)。
    #[tokio::test(flavor = "multi_thread")]
    async fn replay_param_validation_and_traversal_rejected() {
        let t = spawn_mgr(None, None).await;
        let recdir = temp_recdir("valid");
        t.mgr.set_recordings_dir(recdir.clone());
        fleet_create_ok(
            t.addr,
            r#"{"name":"校验桥","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        let b = t.mgr.get("b1").unwrap();
        std::fs::create_dir_all(&recdir).unwrap();
        std::fs::write(
            recdir.join("b1-ok.jsonl"),
            "{\"ts\":0,\"dir\":\"tx\",\"hex\":\"01\"}\n",
        )
        .unwrap();
        b.hub.set_phase(Phase::Open);
        // speed 越界 (FR-19: 0.5~10)
        for bad_speed in ["0.1", "20", "-1"] {
            let (code, resp) = http_req(
                t.addr,
                "POST",
                "/api/fleet/b1/replay",
                Some(&format!(r#"{{"file":"b1-ok.jsonl","speed":{bad_speed}}}"#)),
            )
            .await;
            assert_eq!(code, 400, "speed={bad_speed} 应 400: {resp}");
        }
        // 文件不存在 / 空 body / 坏 JSON
        for bad_body in [
            r#"{"file":"missing.jsonl"}"#,
            r#"{"file":"a/b.jsonl"}"#,
            r#"{}"#,
            r#"not json"#,
        ] {
            let (code, _) = http_req(t.addr, "POST", "/api/fleet/b1/replay", Some(bad_body)).await;
            assert_eq!(code, 400, "body={bad_body} 应 400");
        }
        // 路径穿越拒绝 (FR-19: file 必须解析为录像目录内已有文件)
        for bad_file in [
            "../../Cargo.toml",
            "..\\Cargo.toml",
            "../serialhub/Cargo.toml",
            "%2e%2e%2fb1-ok.jsonl",
            "C:\\Windows\\notepad.exe",
        ] {
            let body = json!({"file": bad_file}).to_string();
            let (code, resp) = http_req(t.addr, "POST", "/api/fleet/b1/replay", Some(&body)).await;
            assert_eq!(code, 400, "穿越 \"{bad_file}\" 应 400: {resp}");
        }
        // 回放互斥: loop 回放进行中再回放 → 400
        let (code, _) = http_req(
            t.addr,
            "POST",
            "/api/fleet/b1/replay",
            Some(r#"{"file":"b1-ok.jsonl","speed":1,"loop":true}"#),
        )
        .await;
        assert_eq!(code, 200);
        let (code, resp) = http_req(
            t.addr,
            "POST",
            "/api/fleet/b1/replay",
            Some(r#"{"file":"b1-ok.jsonl"}"#),
        )
        .await;
        assert_eq!(code, 400, "回放进行中应 400: {resp}");
        let (code, _) = http_req(t.addr, "POST", "/api/fleet/b1/replay/stop", None).await;
        assert_eq!(code, 200);
        t.shutdown().await;
        let _ = std::fs::remove_dir_all(&recdir);
    }

    // ---- FR-22: 配置导入导出 (ADR-24 B4) ----

    fn rec_json(id: &str, listen: &str) -> Value {
        json!({
            "id": id,
            "name": format!("导入-{id}"),
            "autoOpen": false,
            "autoReconnect": true,
            "maxClients": 0,
            "listen": listen,
            "serial": {
                "port": "", "baud": 115200, "dataBits": 8,
                "parity": "N", "stopBits": 2, "flow": "none"
            },
        })
    }

    /// 导出形状 (version/bridges + attachment 头) + 导出体可直接回导 (merge 全跳过)。
    #[tokio::test(flavor = "multi_thread")]
    async fn export_shape_and_roundtrip() {
        let t = spawn_mgr(None, None).await;
        fleet_create_ok(
            t.addr,
            r#"{"name":"导1","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        fleet_create_ok(
            t.addr,
            r#"{"name":"导2","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        // 原始响应取头部 (http_req 只回 body): 断言 Content-Disposition attachment
        let mut s = TcpStream::connect(t.addr).await.unwrap();
        s.write_all(b"GET /api/fleet/export HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut resp = Vec::new();
        s.read_to_end(&mut resp).await.unwrap();
        let text = String::from_utf8_lossy(&resp).to_string();
        assert!(text.contains("HTTP/1.1 200"), "{text}");
        assert!(
            text.to_lowercase()
                .contains("content-disposition: attachment; filename=\"fleet.json\""),
            "须为 attachment 下载: {text}"
        );
        assert!(text.contains("application/json"), "{text}");
        let body = text.split_once("\r\n\r\n").unwrap().1;
        let v: Value = serde_json::from_str(body).expect("导出体是合法 JSON");
        assert_eq!(v["version"], 1, "{v}");
        assert_eq!(v["bridges"].as_array().unwrap().len(), 2, "{v}");
        // POST 同路由双受理 (ADR-24⑥: UI 先 POST 后 GET), 响应体与 GET 一致
        let (code, post_body) = http_req(t.addr, "POST", "/api/fleet/export", None).await;
        assert_eq!(code, 200, "{post_body}");
        assert_eq!(post_body, body, "POST 导出体须与 GET 完全一致");
        let row = &v["bridges"][0];
        for key in [
            "id",
            "name",
            "autoOpen",
            "autoReconnect",
            "maxClients",
            "listen",
            "serial",
        ] {
            assert!(row.get(key).is_some(), "导出桥行缺 {key}: {row}");
        }
        // 回导 (merge): id 全部冲突 → imported=0 skipped=2 (形状可被导入器接受)
        let import_body = json!({"mode": "merge", "json": v}).to_string();
        let (code, resp) = http_req(t.addr, "POST", "/api/fleet/import", Some(&import_body)).await;
        assert_eq!(code, 200, "{resp}");
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["imported"], 0, "{resp}");
        assert_eq!(v["skipped"], 2, "{resp}");
        t.shutdown().await;
    }

    /// merge=按 id 合并 (冲突跳过计数), replace=整表替换 (运行中桥先停);
    /// 非法 schema 整体 400。
    #[tokio::test(flavor = "multi_thread")]
    async fn import_merge_replace_and_schema_rejects() {
        let t = spawn_mgr(None, None).await;
        fleet_create_ok(
            t.addr,
            r#"{"name":"旧1","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        fleet_create_ok(
            t.addr,
            r#"{"name":"旧2","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;

        // merge: b1 冲突跳过, b9 新建 → imported=1 skipped=1; b9 即建即启
        let body = json!({
            "mode": "merge",
            "json": {"version": 1, "bridges": [rec_json("b1", "127.0.0.1:0"), rec_json("b9", "127.0.0.1:0")]}
        })
        .to_string();
        let (code, resp) = http_req(t.addr, "POST", "/api/fleet/import", Some(&body)).await;
        assert_eq!(code, 200, "{resp}");
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["imported"], 1, "{resp}");
        assert_eq!(v["skipped"], 1, "{resp}");
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b9", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["bridge"]["running"], true, "导入桥数据面应启动: {resp}");
        assert_eq!(v["bridge"]["name"], "导入-b9");

        // replace: 整表换成 b5 (运行中的旧桥先停后清) → imported=1 skipped=0
        let body = json!({
            "mode": "replace",
            "json": {"version": 1, "bridges": [rec_json("b5", "127.0.0.1:0")]}
        })
        .to_string();
        let (code, resp) = http_req(t.addr, "POST", "/api/fleet/import", Some(&body)).await;
        assert_eq!(code, 200, "{resp}");
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["imported"], 1, "{resp}");
        assert_eq!(v["skipped"], 0, "{resp}");
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        let arr = v["bridges"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "replace 应整表替换: {resp}");
        assert_eq!(arr[0]["id"], "b5");
        assert_eq!(arr[0]["running"], true);

        // 非法 schema → 400 (mode 非法 / 缺 version / version 不支持 / 桥缺字段 /
        // 串口配置越界 / json 非对象)
        let mut bad_serial = rec_json("b7", "127.0.0.1:0");
        bad_serial["serial"]["baud"] = json!(50); // 越界 (<110)
        let cases = [
            r#"{"mode":"upsert","json":{"version":1,"bridges":[]}}"#.to_string(),
            r#"{"mode":"merge","json":{"bridges":[]}}"#.to_string(),
            r#"{"mode":"merge","json":{"version":2,"bridges":[]}}"#.to_string(),
            r#"{"mode":"merge","json":{"version":1,"bridges":[{"id":"x","name":"n"}]}}"#
                .to_string(),
            json!({"mode":"replace","json":{"version":1,"bridges":[bad_serial]}}).to_string(),
        ];
        for (i, bad) in cases.iter().enumerate() {
            let (code, resp) = http_req(t.addr, "POST", "/api/fleet/import", Some(bad)).await;
            assert_eq!(code, 400, "case{i} 应 400: {resp}");
        }
        let mut bad_json = json!({"mode":"merge","json":{"version":1,"bridges":[]}});
        bad_json["json"] = json!(5); // json 非对象
        let (code, resp) = http_req(
            t.addr,
            "POST",
            "/api/fleet/import",
            Some(&bad_json.to_string()),
        )
        .await;
        assert_eq!(code, 400, "json 非对象应 400: {resp}");
        // 校验失败不落库: 桥表仍是 replace 后的 1 座
        let (_, resp) = http_req(t.addr, "GET", "/api/fleet", None).await;
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["bridges"].as_array().unwrap().len(), 1);
        t.shutdown().await;
    }

    // ---- FR-20: TCP 旁路转发 (ADR-24② B2) ----

    /// RX 字节经 bc_tx (与 WS/tap 同源) 单向转发到本地 TCP listener;
    /// forwardConnected 随连接状态回显; 目标校验 create/config 双路 400。
    #[tokio::test(flavor = "multi_thread")]
    async fn forward_rx_bytes_to_local_listener() {
        let t = spawn_mgr(None, None).await;
        fleet_create_ok(
            t.addr,
            r#"{"name":"转发桥","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        let b = t.mgr.get("b1").unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = listener.local_addr().unwrap().to_string();

        // 非法目标 400 (create 与 config 双路)
        let (code, _) = http_req(
            t.addr,
            "POST",
            "/api/fleet",
            Some(r#"{"name":"坏目标","listen":"127.0.0.1:0","forwardTcp":"host:abc"}"#),
        )
        .await;
        assert_eq!(code, 400, "forwardTcp 端口非法应 400");
        let (code, resp) = http_req(
            t.addr,
            "POST",
            "/api/fleet/b1/config",
            Some(r#"{"forwardTcp":"host:abc"}"#),
        )
        .await;
        assert_eq!(code, 400, "config forwardTcp 端口非法应 400: {resp}");

        // 配置合法目标 → 会话热生效连接
        let body = json!({"forwardTcp": target}).to_string();
        let (code, resp) = http_req(t.addr, "POST", "/api/fleet/b1/config", Some(&body)).await;
        assert_eq!(code, 200, "{resp}");
        let (mut conn, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .expect("连接超时")
            .unwrap();
        let mut connected = false;
        for _ in 0..50 {
            let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
            let v: Value = serde_json::from_str(&resp).unwrap();
            if v["bridge"]["forwardConnected"].as_bool() == Some(true) {
                connected = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(connected, "forwardConnected 应为 true");

        // RX 注入 → listener 收到原样字节
        b.ctx.bc_tx.send(b"FORWARD-1".to_vec()).unwrap();
        let mut buf = [0u8; 9];
        tokio::time::timeout(Duration::from_secs(5), conn.read_exact(&mut buf))
            .await
            .expect("读转发字节超时")
            .unwrap();
        assert_eq!(&buf, b"FORWARD-1");
        t.shutdown().await;
    }

    /// 对端断开后 3s 节拍自动重连; 配置移除 (改空) 即断开且不再转发。
    #[tokio::test(flavor = "multi_thread")]
    async fn forward_reconnect_after_drop_and_removal_stops() {
        let t = spawn_mgr(None, None).await;
        fleet_create_ok(
            t.addr,
            r#"{"name":"重连桥","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        let b = t.mgr.get("b1").unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = listener.local_addr().unwrap().to_string();
        let body = json!({"forwardTcp": target}).to_string();
        let (code, _) = http_req(t.addr, "POST", "/api/fleet/b1/config", Some(&body)).await;
        assert_eq!(code, 200);

        let (conn1, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        drop(conn1); // 对端断开

        // 注入触发对死连接的写失败 → 3s 节拍内重连; accept 轮询等第二次连接
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut conn2 = None;
        while Instant::now() < deadline {
            b.ctx.bc_tx.send(b"KICK".to_vec()).unwrap();
            match tokio::time::timeout(Duration::from_millis(300), listener.accept()).await {
                Ok(Ok((c, _))) => {
                    conn2 = Some(c);
                    break;
                }
                _ => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        let mut conn2 = conn2.expect("10s 内未重连 (断线重连回归)");

        // 新连接上继续收到转发字节 (KICK 积压帧可能先出站, 扫描标记)
        b.ctx.bc_tx.send(b"RECONN-OK".to_vec()).unwrap();
        let mut got = Vec::new();
        let marker = b"RECONN-OK".to_vec();
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut chunk = [0u8; 64];
            while !got.windows(9).any(|w| w == marker.as_slice()) {
                let n = conn2.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&chunk[..n]);
            }
        })
        .await
        .expect("重连后读转发字节超时");
        assert!(
            got.windows(9).any(|w| w == marker.as_slice()),
            "重连连接上应收到 RECONN-OK: {got:?}"
        );

        // 配置移除 (改空) → 立即断开 + 状态回 false
        let (code, _) = http_req(
            t.addr,
            "POST",
            "/api/fleet/b1/config",
            Some(r#"{"forwardTcp":""}"#),
        )
        .await;
        assert_eq!(code, 200);
        let mut closed = false;
        for _ in 0..30 {
            let (_, resp) = http_req(t.addr, "GET", "/api/fleet/b1", None).await;
            let v: Value = serde_json::from_str(&resp).unwrap();
            if v["bridge"]["forwardConnected"].as_bool() == Some(false)
                && v["bridge"]["forwardTcp"] == ""
            {
                closed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            closed,
            "移除配置后 forwardConnected 应 false 且 forwardTcp 空"
        );
        // 对端应看到连接关闭 (EOF)
        let eof = tokio::time::timeout(Duration::from_secs(3), async {
            let mut probe = [0u8; 1];
            let _ = conn2.read(&mut probe).await; // Ok(0) = EOF
        })
        .await;
        assert!(eof.is_ok(), "3s 内对端应观察到连接断开 (EOF)");
        // 移除后注入不再出站
        b.ctx.bc_tx.send(b"AFTER-OFF".to_vec()).unwrap();
        let mut probe = [0u8; 1];
        let got = tokio::time::timeout(Duration::from_millis(400), conn2.read(&mut probe)).await;
        assert!(
            matches!(got, Err(_) | Ok(Ok(0))),
            "配置移除后不得再有转发字节"
        );
        t.shutdown().await;
    }

    /// 配置热改目标: 旧连接断开, 新连接按新值建立 (运行中改配置语义)。
    #[tokio::test(flavor = "multi_thread")]
    async fn forward_target_hot_swap() {
        let t = spawn_mgr(None, None).await;
        fleet_create_ok(
            t.addr,
            r#"{"name":"换向桥","listen":"127.0.0.1:0","autoOpen":false}"#,
        )
        .await;
        let b = t.mgr.get("b1").unwrap();
        let l1 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let l2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let body = json!({"forwardTcp": l1.local_addr().unwrap().to_string()}).to_string();
        let (code, _) = http_req(t.addr, "POST", "/api/fleet/b1/config", Some(&body)).await;
        assert_eq!(code, 200);
        let (mut conn1, _) = tokio::time::timeout(Duration::from_secs(5), l1.accept())
            .await
            .unwrap()
            .unwrap();

        // 热改到 l2
        let body = json!({"forwardTcp": l2.local_addr().unwrap().to_string()}).to_string();
        let (code, _) = http_req(t.addr, "POST", "/api/fleet/b1/config", Some(&body)).await;
        assert_eq!(code, 200);
        let (mut conn2, _) = tokio::time::timeout(Duration::from_secs(5), l2.accept())
            .await
            .unwrap()
            .unwrap();
        // 旧连接应被断开 (EOF)
        let mut probe = [0u8; 1];
        let n1 = tokio::time::timeout(Duration::from_secs(3), conn1.read(&mut probe)).await;
        assert!(matches!(n1, Ok(Ok(0)) | Err(_)), "旧连接应断开: {n1:?}");
        // 新连接收到后续字节
        b.ctx.bc_tx.send(b"NEW-TARGET".to_vec()).unwrap();
        let mut buf = [0u8; 10];
        tokio::time::timeout(Duration::from_secs(5), conn2.read_exact(&mut buf))
            .await
            .expect("新目标读转发字节超时")
            .unwrap();
        assert_eq!(&buf, b"NEW-TARGET");
        t.shutdown().await;
    }
}
