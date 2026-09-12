//! HTTP + WS 层 (FR-4/FR-6/FR-7)。
//!
//! 控制面与数据面彻底分离 (ADR-2):
//! - /api/* 是 JSON 控制面; /ws 是纯二进制数据面, 不做任何 JSON 包帧;
//! - 每个客户端一条 select 循环: 下行 = broadcast 订阅 (慢客户端 Lagged 丢旧帧),
//!   上行 = 丢进 tx 队列由单写者串行写串口;
//! - 任何客户端断开/乱帧/协议错误只 break 循环, 绝不 panic;
//!   clients 计数用 Drop 兜底, 即使任务被取消也不会漏减。

use axum::extract::rejection::JsonRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::StatusCode;
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

async fn client_loop(socket: WebSocket, app: App) {
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
        }
    }
}
