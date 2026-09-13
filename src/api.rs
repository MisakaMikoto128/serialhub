//! HTTP + WS 层 (FR-4/FR-6/FR-7)。
//!
//! 控制面与数据面彻底分离 (ADR-2):
//! - /api/* 是 JSON 控制面; /ws 是纯二进制数据面, 不做任何 JSON 包帧;
//! - 每个客户端一条 select 循环: 下行 = broadcast 订阅 (慢客户端 Lagged 丢旧帧),
//!   上行 = 丢进 tx 队列由单写者串行写串口;
//! - 任何客户端断开/乱帧/协议错误只 break 循环, 绝不 panic;
//!   clients 计数用 Drop 兜底, 即使任务被取消也不会漏减。

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
use crate::hub::StatusJson;
use crate::serial::{list_ports, PortCtx};
use crate::supervisor::HubCmd;

#[derive(Clone)]
pub struct App {
    pub ctx: PortCtx,
    pub cmd_tx: UnboundedSender<HubCmd>,
    /// 停机开关 (ADR-8): /api/shutdown 与托盘「退出」共用同一 watch。
    pub shutdown_tx: tokio::sync::watch::Sender<bool>,
    /// FR-9a: 是否 GUI 壳 (决定 /api/restart 是否可用)。
    pub gui: bool,
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

async fn status(State(app): State<App>) -> Json<StatusJson> {
    Json(app.ctx.hub.status_json())
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
async fn ports() -> Json<PortsResp> {
    Json(PortsResp {
        ports: list_ports()
            .into_iter()
            .map(|(name, desc)| PortEntry { name, desc })
            .collect(),
    })
}

#[derive(Deserialize)]
struct ConfigReq {
    port: Option<String>,
    baud: Option<u32>,
    #[serde(rename = "dataBits")]
    data_bits: Option<u8>,
    parity: Option<String>,
    #[serde(rename = "stopBits")]
    stop_bits: Option<u8>,
    flow: Option<String>,
    /// FR-9b: 最大客户端数, 0 = 不限。
    #[serde(rename = "maxClients")]
    max_clients: Option<u32>,
}

/// 全字段可省: 只更新给出的字段 (方便部分修改); 校验失败整体拒绝, 不做半套更新。
async fn set_config(State(app): State<App>, body: Result<Json<ConfigReq>, JsonRejection>) -> Response {
    let Json(req) = match body {
        Ok(b) => b,
        Err(rej) => return bad(format!("请求体不是合法 JSON: {rej}")),
    };
    let mut cfg = app.ctx.hub.config();
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
        app.ctx.hub.set_max_clients(m); // 0 = 不限, 无上限校验 (u32)
    }
    app.ctx.hub.update_config(cfg);
    ok()
}

/// 打开是异步的: 这里只受理指令, 相位经 /api/status 观察 (opening→open/retry)。
async fn open(State(app): State<App>) -> Response {
    if app.ctx.hub.config().port.is_empty() {
        return bad("未配置串口: 先在控制台选择串口, 或用 --port 指定".into());
    }
    let _ = app.cmd_tx.send(HubCmd::Open);
    ok()
}

async fn close(State(app): State<App>) -> Response {
    let _ = app.cmd_tx.send(HubCmd::Close);
    ok()
}

/// FR-9a/ADR-9②: GUI 模式改 addr —— 校验并登记新地址后触发与托盘退出相同的
/// 优雅停机; 真正的 spawn 在 finalize_shutdown 里、串口释放之后执行 (顺序原因见
/// service.rs)。headless 模式拒绝: 无壳不自起, 提示"改地址请重启进程"。
#[derive(Deserialize)]
struct RestartReq {
    addr: String,
}

async fn restart(State(app): State<App>, body: Result<Json<RestartReq>, JsonRejection>) -> Response {
    if !app.gui {
        return bad("headless 模式改地址请重启进程".into());
    }
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
    *crate::hub::lock_mutex(&app.restart_to) = Some(new_addr.to_string());
    let _ = app.shutdown_tx.send(true);
    ok()
}

/// FR-4⑤/ADR-8: 优雅停机整进程 —— 与托盘「退出」完全同一序列
/// (watch → axum 优雅退出 → Close → stop_active → Stopped → 进程退出)。
/// 退出路径必须有冗余, 不能只依赖托盘菜单 (Sprint2 UX P1-1)。
async fn shutdown(State(app): State<App>) -> Response {
    let _ = app.shutdown_tx.send(true);
    ok()
}

fn ok() -> Response {
    Json(json!({"ok": true})).into_response()
}

fn bad(msg: String) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": msg}))).into_response()
}

// ---------- 数据面 /ws (纯二进制) ----------

async fn ws_upgrade(ws: WebSocketUpgrade, State(app): State<App>) -> Response {
    ws.on_upgrade(move |socket| client_loop(socket, app))
}

async fn client_loop(mut socket: WebSocket, app: App) {
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
