//! HTTP + WS 层 (FR-4/FR-6/FR-7)。
//!
//! 控制面与数据面彻底分离 (ADR-2):
//! - /api/* 是 JSON 控制面; /ws 是纯二进制数据面, 不做任何 JSON 包帧;
//! - 每个客户端一条 select 循环: 下行 = broadcast 订阅 (慢客户端 Lagged 丢旧帧),
//!   上行 = 丢进 tx 队列由单写者串行写串口;
//! - 任何客户端断开/乱帧/协议错误只 break 循环, 绝不 panic;
//!   clients 计数用 Drop 兜底, 即使任务被取消也不会漏减。
//!
//! Sprint 4 (FR-10): handler 逻辑抽成 `*_core` 纯函数 (入参 = 桥的 PortCtx/cmd_tx,
//! 不依赖 axum State), 供两处复用 —— 本模块 legacy 单桥路由 + fleet.rs 的
//! 管理台 (兼容分发与每桥数据面)。核心逻辑一字不改, 语义零变化。

use axum::extract::rejection::JsonRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
use std::net::SocketAddr;
use std::sync::Arc;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc::UnboundedSender;

use crate::config;
use crate::serial::{list_ports, PortCtx};
use crate::supervisor::HubCmd;

#[derive(Clone)]
pub struct App {
    pub ctx: PortCtx,
    pub cmd_tx: UnboundedSender<HubCmd>,
    /// 停机开关 (ADR-8): /api/shutdown 与托盘「退出」共用同一 watch。
    pub shutdown_tx: tokio::sync::watch::Sender<bool>,
    /// FR-9a: 是否 GUI 壳 (决定 /api/restart 是否可用)。
    /// FR-9a: 自我重启目标地址槽 (/api/restart 登记, finalize_shutdown 消费)。
    pub restart_to: Arc<std::sync::Mutex<Option<String>>>,
    pub index: &'static str,
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/status", get(status))
        .route("/api/ports", get(ports))
        .route("/api/config", post(set_config))
        .route("/api/open", post(open))
        .route("/api/close", post(close))
        .route("/api/shutdown", post(shutdown))
        .route("/api/restart", post(restart))
        .route("/ws", get(ws_upgrade))
        .with_state(app)
}

// ---------- 控制面 /api/* ----------

async fn index(State(app): State<App>) -> Html<&'static str> {
    Html(app.index)
}

/// GET /api/status 核心 (管理台兼容分发复用)。
pub(crate) fn status_core(ctx: &PortCtx) -> Response {
    Json(ctx.hub.status_json()).into_response()
}

async fn status(State(app): State<App>) -> Response {
    status_core(&app.ctx)
}

#[derive(serde::Serialize)]
struct PortsResp {
    ports: Vec<PortEntry>,
}

#[derive(serde::Serialize)]
struct PortEntry {
    name: String,
    desc: String,
}

/// 契约: {"ports":[{"name":"COM1","desc":"..."}]}, 每项恰好两个字段。
pub(crate) fn ports_core() -> Response {
    Json(PortsResp {
        ports: list_ports()
            .into_iter()
            .map(|(name, desc)| PortEntry { name, desc })
            .collect(),
    })
    .into_response()
}

async fn ports() -> Response {
    ports_core()
}

#[derive(Deserialize)]
pub(crate) struct ConfigReq {
    #[allow(dead_code)] // fleet.rs 直接构造本结构 (字段全 Option)
    pub(crate) port: Option<String>,
    pub(crate) baud: Option<u32>,
    #[serde(rename = "dataBits")]
    pub(crate) data_bits: Option<u8>,
    pub(crate) parity: Option<String>,
    #[serde(rename = "stopBits")]
    pub(crate) stop_bits: Option<u8>,
    pub(crate) flow: Option<String>,
    /// FR-9b: 最大客户端数, 0 = 不限。
    /// (qa-sprint4 DEF-1 回归: rename 不可缺 —— 缺了 "maxClients" 会被当未知字段
    /// 静默忽略, 兼容端点 POST /api/config 即失效, FR-9b/ADR-9b① 违约。)
    #[serde(rename = "maxClients")]
    pub(crate) max_clients: Option<u32>,
    /// FR-12/ADR-16①: 每桥自动重连开关 (缺省 = 不改动, 保持现值)。
    #[serde(rename = "autoReconnect")]
    pub(crate) auto_reconnect: Option<bool>,
}

/// 全字段可省: 只更新给出的字段 (方便部分修改); 校验失败整体拒绝, 不做半套更新。
pub(crate) fn apply_config_core(ctx: &PortCtx, req: ConfigReq) -> Response {
    let mut cfg = ctx.hub.config();
    if let Some(p) = req.port {
        cfg.port = p.trim().to_string();
    }
    if let Some(b) = req.baud {
        if let Err(e) = config::validate_baud(b) {
            return bad(e);
        }
        cfg.baud = b;
    }
    if let Some(d) = req.data_bits {
        if !matches!(d, 7 | 8) {
            return bad(format!("数据位只支持 7/8, 得到 {d}"));
        }
        cfg.data_bits = d;
    }
    if let Some(p) = &req.parity {
        match config::Parity::parse(p) {
            Ok(v) => cfg.parity = v,
            Err(e) => return bad(e),
        }
    }
    if let Some(s) = req.stop_bits {
        if !matches!(s, 1 | 2) {
            return bad(format!("停止位只支持 1/2, 得到 {s}"));
        }
        cfg.stop_bits = s;
    }
    if let Some(f) = &req.flow {
        match config::Flow::parse(f) {
            Ok(v) => cfg.flow = v,
            Err(e) => return bad(e),
        }
    }
    if let Some(m) = req.max_clients {
        ctx.hub.set_max_clients(m); // 0 = 不限, 无上限校验 (u32)
    }
    // FR-12/ADR-16①: autoReconnect 即改即生效 (监督任务在下个决策点读新值)
    if let Some(a) = req.auto_reconnect {
        ctx.hub.set_auto_reconnect(a);
    }
    ctx.hub.update_config(cfg);
    ok()
}

async fn set_config(State(app): State<App>, body: Result<Json<ConfigReq>, JsonRejection>) -> Response {
    match body {
        Ok(Json(req)) => apply_config_core(&app.ctx, req),
        Err(rej) => bad(format!("请求体不是合法 JSON: {rej}")),
    }
}

/// 打开是异步的: 这里只受理指令, 相位经 /api/status 观察 (opening→open/retry)。
pub(crate) fn open_core(ctx: &PortCtx, cmd_tx: &UnboundedSender<HubCmd>) -> Response {
    if ctx.hub.config().port.is_empty() {
        return bad("未配置串口: 先在控制台选择串口, 或用 --port 指定".into());
    }
    let _ = cmd_tx.send(HubCmd::Open);
    ok()
}

async fn open(State(app): State<App>) -> Response {
    open_core(&app.ctx, &app.cmd_tx)
}

pub(crate) fn close_core(cmd_tx: &UnboundedSender<HubCmd>) -> Response {
    let _ = cmd_tx.send(HubCmd::Close);
    ok()
}

async fn close(State(app): State<App>) -> Response {
    close_core(&app.cmd_tx)
}

/// FR-9a/ADR-12: 改监听地址 = **原地换绑** (headless 与 GUI 一视同仁, 均不重启进程) ——
/// 校验并登记新地址, serve 循环退出当前 TCP 面后重 bind; 串口会话/状态/托盘全程不动。
#[derive(Deserialize)]
pub(crate) struct RestartReq {
    addr: String,
}

/// FR-9a/ADR-12 核心: 校验并登记换址目标 (登记槽由服务循环消费, 原地换绑)。
pub(crate) fn restart_core(
    restart_to: &Arc<std::sync::Mutex<Option<String>>>,
    body: Result<Json<RestartReq>, JsonRejection>,
) -> Response {
    let Json(req) = match body {
        Ok(b) => b,
        Err(rej) => return bad(format!("请求体不是合法 JSON: {rej}")),
    };
    let new_addr: SocketAddr = match req.addr.trim().parse() {
        Ok(a) => a,
        Err(_) => {
            return bad(format!(
                "地址无效: \"{}\" (形如 127.0.0.1:8080)",
                req.addr.trim()
            ))
        }
    };
    *crate::hub::lock_mutex(restart_to) = Some(new_addr.to_string());
    ok()
}

async fn restart(State(app): State<App>, body: Result<Json<RestartReq>, JsonRejection>) -> Response {
    restart_core(&app.restart_to, body)
}

/// FR-4⑤/ADR-8: 优雅停机整进程 —— 与托盘「退出」完全同一序列
/// (watch → axum 优雅退出 → Close → stop_active → Stopped → 进程退出)。
/// 退出路径必须有冗余, 不能只依赖托盘菜单 (Sprint2 UX P1-1)。
/// FR-4⑤/ADR-8 核心: 优雅停机整进程 —— 与托盘「退出」完全同一序列。
pub(crate) fn shutdown_core(shutdown_tx: &tokio::sync::watch::Sender<bool>) -> Response {
    let _ = shutdown_tx.send(true);
    ok()
}

async fn shutdown(State(app): State<App>) -> Response {
    shutdown_core(&app.shutdown_tx)
}

pub(crate) fn ok() -> Response {
    Json(json!({"ok": true})).into_response()
}

pub(crate) fn bad(msg: String) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": msg}))).into_response()
}

// ---------- 数据面 /ws (纯二进制) ----------

async fn ws_upgrade(ws: WebSocketUpgrade, State(app): State<App>) -> Response {
    ws.on_upgrade(move |socket| client_loop(socket, app))
}

/// 数据面客户端主循环 (FR-10 起供三处复用: legacy 单桥 /ws、每桥数据面、管理台兼容分发)。
/// app.shutdown_tx 应传**该桥的停机 watch** —— 桥停/删/进程退出都会主动断开 WS,
/// 优雅停机才不会被长连接卡住。
pub(crate) async fn client_loop(mut socket: WebSocket, app: App) {
    // FR-9b: 握手后若客户端已满, 以 close 1013 (Try Again Later) 拒绝新连接,
    // 且不计入 clients (未 client_inc); 老客户端不受影响。
    let max = app.ctx.hub.max_clients();
    if max > 0 && app.ctx.hub.client_count() >= max as usize {
        let _ = socket.send(Message::Close(Some(CloseFrame {
            code: 1013,
            reason: "max clients".into(),
        })))
        .await;
        return;
    }
    app.ctx.hub.client_inc();
    struct DecOnDrop(PortCtx);
    impl Drop for DecOnDrop {
        fn drop(&mut self) {
            self.0.hub.client_dec();
        }
    }
    let _guard = DecOnDrop(app.ctx.clone());

    let (mut sink, mut stream) = socket.split();
    let mut bsub = app.ctx.bc_tx.subscribe();
    let mut sd = app.shutdown_tx.subscribe(); // 停机时主动断开 WS, 优雅停机才不会被长连接卡住
    loop {
        tokio::select! {
            // 上行: 客户端帧 → TX 队列 (单写者 FIFO 串行写串口)
            msg = stream.next() => match msg {
                Some(Ok(Message::Binary(b))) => {
                    if !b.is_empty() {
                        app.ctx.send_to_port(&b[..]);
                    }
                }
                // 保险起见把文本帧的字节也当原始数据转发 (乱帧不 panic, FR-7)
                Some(Ok(Message::Text(t))) => {
                    let b = t.as_str().as_bytes();
                    if !b.is_empty() {
                        app.ctx.send_to_port(b);
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {} // Ping/Pong 由底层自动处理
                Some(Err(_)) => break, // 断开/协议错误: 只退出本客户端循环
            },
            // 下行: 串口 RX 广播
            frame = bsub.recv() => match frame {
                Ok(bytes) => {
                    if sink.send(Message::Binary(bytes.into())).await.is_err() {
                        break; // 客户端已断开
                    }
                }
                Err(RecvError::Lagged(_)) => continue, // 慢客户端丢旧帧, 保持连接
                Err(RecvError::Closed) => break,       // 广播源消失 (进程退出)
            },
            // 停机: 服务端主动断开本 WS (否则长连接会卡死 axum 优雅停机)
            _ = sd.changed() => break,
        }
    }
}
