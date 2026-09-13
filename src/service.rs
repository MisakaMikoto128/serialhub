//! 服务装配 (FR-8): hub + 监督任务 + axum 组装成一个可运行单元。
//!
//! headless (进程内 tokio 主任务) 与 GUI (后台线程 tokio) 共用本函数;
//! 与 GUI 事件循环的整合方式 (为什么这样设计, 下一个接手的人先读):
//! - Win32 规定窗口/托盘/菜单只能在主线程操作, tao 的 EventLoop::run 占死主线程;
//! - 所以 tokio runtime 整个搬去后台线程, 主线程只跑事件循环;
//! - 服务 → 主线程: EventLoopProxy<UserEvent> (Send) 经 ServiceEvent 转发相位变化/停止;
//! - 主线程 → 服务: cmd_tx (tokio unbounded, 同步 send) 与 shutdown watch;
//! - 任何一侧都绝不做 block_on/await 跨线程等待, 否则冻结消息泵或服务线程。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::sync::{mpsc, watch};

use crate::config::{Flow, SerialConfig};
use crate::hub::{HubState, Phase};
use crate::serial::PortCtx;
use crate::supervisor::{self, HubCmd};

/// GUI/headless 都需要的启动参数 (由 cli::Cli 派生, 见 Cli::startup)。
#[derive(Debug, Clone)]
pub struct Startup {
    pub cfg: SerialConfig,
    pub auto_open: bool,
    pub addr: SocketAddr,
    /// FR-9b: 最大客户端数 (0 = 不限)。
    pub max_clients: u32,
    /// ADR-10: 流控 (CLI 与 /api/config 同一套校验; 自我重启携带)。
    pub flow: Flow,
    /// FR-9a: 是否 GUI 壳 —— /api/restart 只在 GUI 模式允许自我重启。
    pub gui: bool,
}

/// 服务向上层 (GUI) 发的通知。
#[derive(Debug, Clone)]
pub enum ServiceEvent {
    /// 绑定成功, 服务已就绪 (携带实际监听地址)。
    Ready(SocketAddr),
    /// 状态机相位或端口名变化 (GUI 托盘图标/tooltip 刷新)。
    Phase(Phase, String),
    /// 收到退出指令且清理完毕 (串口线程已停), 上层可以结束进程。
    Stopped,
}

pub type OnEvent = Arc<dyn Fn(ServiceEvent) + Send + Sync>;

/// cmd 通道由调用方创建: GUI 模式下托盘菜单 (主线程) 要直接往里发指令。
/// shutdown_tx 是唯一停机开关 (ADR-8): 托盘「退出」、`POST /api/shutdown` 都发它,
/// run_service 内部统一转成优雅停机序列 —— 只此一条路径, 不允许第二种停机写法。
pub async fn run_service(
    su: Startup,
    cmd_tx: mpsc::UnboundedSender<HubCmd>,
    cmd_rx: mpsc::UnboundedReceiver<HubCmd>,
    shutdown_tx: watch::Sender<bool>,
    on_event: Option<OnEvent>,
) -> Result<(), String> {
    let mut cfg = su.cfg;
    cfg.flow = su.flow; // ADR-10: CLI --flow 进入统一配置
    let hub = Arc::new(HubState::new(cfg, su.max_clients));
    let (bc_tx, _) = broadcast::channel::<Vec<u8>>(1024);
    let ctx = PortCtx {
        hub: hub.clone(),
        bc_tx: bc_tx.clone(),
        tx_slot: Arc::new(std::sync::Mutex::new(None)),
        active_stop: Arc::new(std::sync::Mutex::new(None)),
    };

    // watch 接收端必须在 Ready 事件之前创建 —— 否则 Ready 后立刻到达的停机指令
    // (测试/UI 竞态) 会因接收端不存在而 SendError; 且 watch 语义是"晚订阅的 changed()
    // 以订阅时值为基线", 晚订阅会漏看已发生的停机变更。
    let mut serve_sd = shutdown_tx.subscribe();
    let mut main_sd = shutdown_tx.subscribe();

    // 1) 绑定: FR-9a 交接需要 —— bind 失败按 250ms 重试至 <=2s (等旧实例优雅退出),
    //    仍失败按 FR-8 报"端口被占用" (不开串口、不进服务循环)。
    let listener = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            match TcpListener::bind(su.addr).await {
                Ok(l) => break l,
                Err(e) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(format!("端口被占用: 无法监听 {}: {e}", su.addr));
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        }
    };

    // 实际监听地址 (su.addr 端口可为 0, 由系统分配 —— GUI/测试都以 Ready 上报的为准)
    let addr = listener.local_addr().map_err(|e| format!("获取监听地址失败: {e}"))?;
    if let Some(cb) = &on_event {
        cb(ServiceEvent::Ready(addr));
    }
    println!("SerialHub 就绪: http://{addr}  (关闭窗口 = 退到托盘)");
    if !hub.config().port.is_empty() {
        println!("  串口: {} @ {} (自动打开: {})", hub.config().port, hub.config().config_str(), su.auto_open);
    }

    // 2) 监督任务 + 自动打开
    tokio::spawn(supervisor::run_supervisor(
        ctx.clone(),
        cmd_rx,
        Arc::new(crate::serial::RealOpener),
        Duration::from_secs(1),
    ));
    if su.auto_open {
        let _ = cmd_tx.send(HubCmd::Open);
    }

    // 3) 相位/端口变化 → 上层 (GUI 托盘刷新); 轮询 150ms, 解耦监督任务内部实现
    if let Some(cb) = &on_event {
        let hub = hub.clone();
        let cb = cb.clone();
        tokio::spawn(async move {
            let mut last = (String::new(), String::new());
            loop {
                tokio::time::sleep(Duration::from_millis(150)).await;
                let phase = hub.phase();
                let port = hub.config().port.clone();
                if (format!("{phase:?}"), port.clone()) != last {
                    last = (format!("{phase:?}"), port.clone());
                    cb(ServiceEvent::Phase(phase, port));
                }
            }
        });
    }

    // 4) HTTP+WS 服务 (优雅停机由 shutdown watch 触发)
    // FR-9a: 自我重启目标地址槽 —— /api/restart 只登记并触发停机;
    // 真正的 spawn 在 finalize_shutdown 里、串口释放之后 (先释放 COM 再交接,
    // 否则新实例 auto-open 会撞上旧实例未释放的串口, 触发 ELTIMA 失败打开锁)。
    let restart_to: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
    let app = crate::api::App {
        ctx: ctx.clone(),
        cmd_tx: cmd_tx.clone(),
        shutdown_tx: shutdown_tx.clone(),
        gui: su.gui,
        restart_to: restart_to.clone(),
        index: include_str!("../ui/index.html"),
    };
    let serve = tokio::spawn(async move {
        let shutdown = async move {
            tokio::select! {
                _ = serve_sd.changed() => {},          // 托盘退出 / POST /api/shutdown
                _ = tokio::signal::ctrl_c() => {},     // headless 控制台 Ctrl-C
            }
        };
        axum::serve(listener, crate::api::router(app))
            .with_graceful_shutdown(shutdown)
            .await
    });

    // 5) 主停机路径: watch 触发 -> COM 释放优先 -> axum 有界宽限 (qa-sprint2-fix:
    //    外部进程挂着连接可拖死优雅停机, 必须 1.5s 内兜底退出)。
    //    Ctrl-C 与 watch 等价 (GUI 无控制台焦点时不会触发; headless 二者皆可)。
    tokio::select! {
        _ = main_sd.changed() => {}          // 托盘退出 / POST /api/shutdown
        _ = tokio::signal::ctrl_c() => {},   // headless 控制台 Ctrl-C
    }
    finalize_shutdown(cmd_tx, ctx, serve, on_event, SHUTDOWN_GRACE, true, restart_to).await
}

/// 停机宽限 (qa-sprint2-fix): watch 触发后 250ms 保证串口线程退出 (COM 释放优先),
/// 再给 axum 1.25s —— 总计 1.5s; 超时且 hard_exit=true 时 std::process::exit(0)
/// (串口已停, COM 已释放, 进程立即结束)。
const SHUTDOWN_GRACE: Duration = Duration::from_millis(1250);
const SERIAL_THREAD_DRAIN: Duration = Duration::from_millis(250);

async fn finalize_shutdown(
    cmd_tx: mpsc::UnboundedSender<HubCmd>,
    ctx: PortCtx,
    serve: tokio::task::JoinHandle<Result<(), std::io::Error>>,
    on_event: Option<OnEvent>,
    grace: Duration,
    hard_exit: bool,
    restart_to: Arc<std::sync::Mutex<Option<String>>>,
) -> Result<(), String> {
    // COM 释放优先: 关串口 + 置会话 stop 标志 (读/写线程 <=150ms 自行退出)
    let _ = cmd_tx.send(HubCmd::Close);
    ctx.stop_active();
    tokio::time::sleep(SERIAL_THREAD_DRAIN).await;
    // FR-9a: COM 已释放, 此刻再拉起新实例 (新地址) —— 新实例 auto-open 不会撞锁
    if let Some(new_addr) = restart_to.lock().unwrap().take() {
        let cfg = ctx.hub.config();
        let max_clients = ctx.hub.max_clients();
        if let Err(e) = spawn_respawn(&new_addr, &cfg, max_clients) {
            eprintln!("serialhub: 自我重启拉起新实例失败: {e} (本进程仍将退出)");
        }
    }
    // axum 有界宽限
    let outcome = tokio::time::timeout(grace, serve).await;
    let done = matches!(&outcome, Ok(Ok(Ok(()))));
    let result = match outcome {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(e))) => Err(format!("HTTP 服务异常退出: {e}")),
        Ok(Err(e)) => Err(format!("HTTP 服务任务异常: {e:?}")),
        Err(_) => {
            if hard_exit {
                std::process::exit(0); // 兜底: 串口已停, 进程立即结束 (COM 已释放)
            }
            Err("优雅停机超时: 有界宽限耗尽仍有连接未退".into())
        }
    };
    if done {
        if let Some(cb) = &on_event {
            cb(ServiceEvent::Stopped);
        }
    }
    result
}

/// FR-9a: 以"当前配置 + 替换 addr"的等价 CLI 参数 DETACHED 拉起同 exe 新实例。
/// 新实例对 bind 做 <=2s 重试 (旧实例退出即交接成功)。
fn respawn_args(new_addr: &str, cfg: &SerialConfig, max_clients: u32) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--addr".into(),
        new_addr.to_string(),
        "--max-clients".into(),
        max_clients.to_string(),
        "--flow".into(), // ADR-10: 重启携带当前流控, 不再回 none
        cfg.flow.as_str().to_string(),
        "--gui".into(),
    ];
    if !cfg.port.is_empty() {
        args.push("--port".into());
        args.push(cfg.port.clone());
        args.push("--baud".into());
        args.push(cfg.baud.to_string());
        args.push("--config".into());
        args.push(cfg.config_str());
    }
    args
}

fn spawn_respawn(new_addr: &str, cfg: &SerialConfig, max_clients: u32) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("定位当前可执行文件失败: {e}"))?;
    let args = respawn_args(new_addr, cfg, max_clients);
    #[cfg(windows)]
    {
        const DETACHED: u32 = 0x0000_0008; // DETACHED_PROCESS
        const NEW_GROUP: u32 = 0x0000_0200; // CREATE_NEW_PROCESS_GROUP
        use std::os::windows::process::CommandExt;
        std::process::Command::new(&exe)
            .args(&args)
            .creation_flags(DETACHED | NEW_GROUP)
            .spawn()
            .map_err(|e| format!("spawn 失败: {e}"))?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new(&exe)
            .args(&args)
            .spawn()
            .map_err(|e| format!("spawn 失败: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SerialConfig;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    /// FIX-9 (FR-4⑤/ADR-8): POST /api/shutdown 优雅停机整进程。
    /// 核心回归点: **存在打开着的 WS 长连接**时, axum 优雅停机不能被它卡住
    /// (服务端必须主动断 WS)。用原始 TCP 手工握手 WebSocket, 全程不碰真实串口
    /// (SerialConfig::default 的 port 为空 + auto_open=false)。
    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_api_closes_gracefully_with_open_ws() {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<HubCmd>();
        let (shutdown_tx, _) = watch::channel(false);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<SocketAddr>();
        let ready_tx = Arc::new(std::sync::Mutex::new(Some(ready_tx)));
        let on_event: OnEvent = Arc::new(move |ev: ServiceEvent| {
            if let ServiceEvent::Ready(a) = ev {
                if let Some(tx) = ready_tx.lock().unwrap().take() {
                    let _ = tx.send(a);
                }
            }
        });
        let su = Startup {
            cfg: SerialConfig::default(),
            auto_open: false,
            addr: "127.0.0.1:0".parse().unwrap(), // 随机空闲端口
            max_clients: 0,
            flow: crate::config::Flow::None,
            gui: false,
        };
        let srv = tokio::spawn(run_service(
            su,
            cmd_tx,
            cmd_rx,
            shutdown_tx.clone(),
            Some(on_event),
        ));

        let addr = tokio::time::timeout(Duration::from_secs(5), ready_rx)
            .await
            .expect("服务 5s 内未就绪")
            .expect("Ready 事件缺失");

        // 1) 打开一条 WS 并保持 (HTTP/1.1 Upgrade 手工握手)
        let mut ws = TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "GET /ws HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        ws.write_all(req.as_bytes()).await.unwrap();
        let mut buf = [0u8; 1024];
        let n = ws.read(&mut buf).await.unwrap();
        let head = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(head.starts_with("HTTP/1.1 101"), "WS 握手失败: {head}");

        // 2) POST /api/shutdown → {"ok":true} (ADR-5 ③)
        let mut http = TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "POST /api/shutdown HTTP/1.1\r\nHost: {addr}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        http.write_all(req.as_bytes()).await.unwrap();
        let mut resp = Vec::new();
        http.read_to_end(&mut resp).await.unwrap();
        let body = String::from_utf8_lossy(&resp);
        assert!(body.contains("\"ok\":true"), "shutdown 响应异常: {body}");

        // 3) 核心断言: WS 存活的情况下 run_service 仍能在超时内优雅完成
        tokio::time::timeout(Duration::from_secs(3), srv)
            .await
            .expect("优雅停机被打开的 WS 阻塞 (>3s) —— 停机序列未关闭长连接")
            .unwrap()
            .expect("run_service 不应报错");

        // 4) 所有 watch 接收端已随服务析构
        assert!(shutdown_tx.is_closed());
    }

    /// FIX-9: watch 触发与 Ctrl-C (信号路径在测试环境不可注入, 这里只验证 watch 是
    /// 唯一可编程停机开关且可重复触发不 panic)。
    #[tokio::test]
    async fn shutdown_watch_can_be_sent_repeatedly() {
        let (tx, mut rx) = watch::channel(false);
        tx.send(true).unwrap();
        assert!(rx.changed().await.is_ok());
        tx.send(false).unwrap(); // 重复发送不 panic (幂等开关语义)
        assert_eq!(*rx.borrow(), false);
    }

    /// 手工 WS 握手 (HTTP/1.1 Upgrade), 返回升级后的 TcpStream。
    async fn ws_handshake(addr: SocketAddr) -> TcpStream {
        let mut s = TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "GET /ws HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = [0u8; 1024];
        let n = s.read(&mut buf).await.unwrap();
        let head = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(head.starts_with("HTTP/1.1 101"), "WS 握手失败: {head}");
        s
    }

    /// FR-9b: maxClients=1 时, 第 2 条 WS 握手后以 close code 1013 (reason "max clients")
    /// 拒绝且不计入 clients; 第 1 条不受影响; status 回显 maxClients=1。
    #[tokio::test(flavor = "multi_thread")]
    async fn ws_over_max_clients_rejected_with_1013() {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<HubCmd>();
        let (shutdown_tx, _) = watch::channel(false);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<SocketAddr>();
        let ready_tx = Arc::new(std::sync::Mutex::new(Some(ready_tx)));
        let on_event: OnEvent = Arc::new(move |ev: ServiceEvent| {
            if let ServiceEvent::Ready(a) = ev {
                if let Some(tx) = ready_tx.lock().unwrap().take() {
                    let _ = tx.send(a);
                }
            }
        });
        let su = Startup {
            cfg: SerialConfig::default(),
            auto_open: false,
            addr: "127.0.0.1:0".parse().unwrap(),
            max_clients: 1,
            flow: Flow::None,
            gui: false,
        };
        let srv = tokio::spawn(run_service(
            su,
            cmd_tx,
            cmd_rx,
            shutdown_tx.clone(),
            Some(on_event),
        ));
        let addr = tokio::time::timeout(Duration::from_secs(5), ready_rx)
            .await
            .expect("服务 5s 内未就绪")
            .expect("Ready 事件缺失");

        // 第 1 条 WS: 正常接受
        let mut ws1 = ws_handshake(addr).await;

        // 第 2 条 WS: 握手成功后收到 close 1013 "max clients"
        // (101 响应头与 close 帧可能合并或分片到达, 循环读取直到帧完整)
        let mut ws2 = TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "GET /ws HTTP/1.1\r\nHost: {addr}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        ws2.write_all(req.as_bytes()).await.unwrap();
        let mut all: Vec<u8> = Vec::new();
        let mut tmp = [0u8; 512];
        let mut saw_101 = false;
        loop {
            let n = tokio::time::timeout(Duration::from_secs(2), ws2.read(&mut tmp))
                .await
                .expect("等待 close 帧超时")
                .unwrap();
            if n == 0 {
                panic!("连接关闭但未收到 close 帧");
            }
            all.extend_from_slice(&tmp[..n]);
            let hdr_end = all
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|pos| pos + 4);
            if let Some(hs) = hdr_end {
                if !saw_101 {
                    assert!(all.starts_with(b"HTTP/1.1 101"), "第 2 条应先完成握手");
                    saw_101 = true;
                }
                let frames = &all[hs..];
                if frames.len() >= 4 {
                    assert_eq!(frames[0] & 0x0f, 8, "应为 Close 帧");
                    let code = ((frames[2] as u16) << 8) | frames[3] as u16;
                    assert_eq!(code, 1013, "close code 应为 1013, 实际 {code}");
                    let plen = (frames[1] & 0x7f) as usize;
                    let reason = String::from_utf8_lossy(&frames[4..2 + plen]).to_string();
                    assert_eq!(reason, "max clients");
                    break;
                }
            }
        }
        drop(ws2);

        // status: clients 恰 1 (拒连不计入) 且 maxClients=1
        let mut http = TcpStream::connect(addr).await.unwrap();
        let req = format!(
            "GET /api/status HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
        );
        http.write_all(req.as_bytes()).await.unwrap();
        let mut resp = Vec::new();
        http.read_to_end(&mut resp).await.unwrap();
        let body = String::from_utf8_lossy(&resp);
        assert!(body.contains("\"clients\":1"), "clients 应为 1: {body}");
        assert!(body.contains("\"maxClients\":1"), "maxClients 应为 1: {body}");

        shutdown_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(3), srv)
            .await
            .expect("停机 3s 内未完成")
            .unwrap()
            .unwrap();
        drop(ws1);
    }

    /// FR-9a: /api/restart 在 headless (gui=false) 下拒绝, 提示改地址需重启进程。
    #[tokio::test(flavor = "multi_thread")]
    async fn restart_rejected_in_headless() {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<HubCmd>();
        let (shutdown_tx, _) = watch::channel(false);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<SocketAddr>();
        let ready_tx = Arc::new(std::sync::Mutex::new(Some(ready_tx)));
        let on_event: OnEvent = Arc::new(move |ev: ServiceEvent| {
            if let ServiceEvent::Ready(a) = ev {
                if let Some(tx) = ready_tx.lock().unwrap().take() {
                    let _ = tx.send(a);
                }
            }
        });
        let su = Startup {
            cfg: SerialConfig::default(),
            auto_open: false,
            addr: "127.0.0.1:0".parse().unwrap(),
            max_clients: 0,
            flow: crate::config::Flow::None,
            gui: false,
        };
        let mut srv = tokio::spawn(run_service(
            su,
            cmd_tx,
            cmd_rx,
            shutdown_tx.clone(),
            Some(on_event),
        ));
        let addr = tokio::time::timeout(Duration::from_secs(5), ready_rx)
            .await
            .expect("服务 5s 内未就绪")
            .expect("Ready 事件缺失");

        let mut http = TcpStream::connect(addr).await.unwrap();
        let json_body = r#"{"addr":"127.0.0.1:8099"}"#;
        let body_req = format!(
            "POST /api/restart HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            json_body.len(),
            json_body
        );
        http.write_all(body_req.as_bytes()).await.unwrap();
        let mut resp = Vec::new();
        http.read_to_end(&mut resp).await.unwrap();
        let body = String::from_utf8_lossy(&resp);
        assert!(body.contains("400"), "应 400: {body}");
        assert!(body.contains("重启进程"), "应提示重启进程: {body}");

        // 守卫在停机之前: 服务不应因 restart 请求退出 (&mut 借用, srv 稍后仍需 join)
        tokio::time::timeout(Duration::from_millis(500), &mut srv)
            .await
            .expect_err("服务不应因 restart 请求退出");
        shutdown_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(3), srv)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    /// ADR-10: 自我重启参数组必须携带当前 flow (--flow), 重启后不再回 none。
    #[test]
    fn respawn_args_carry_flow() {
        let mut cfg = SerialConfig::default();
        cfg.port = "COM2".into();
        cfg.baud = 921_600;
        cfg.flow = Flow::RtsCts;
        let args = respawn_args("127.0.0.1:9000", &cfg, 4);
        let joined = args.join(" ");
        for expect in [
            "--addr 127.0.0.1:9000",
            "--port COM2",
            "--baud 921600",
            "--config 8N2",
            "--max-clients 4",
            "--flow rtscts",
            "--gui",
        ] {
            assert!(joined.contains(expect), "respawn 参数缺 {expect}: {joined}");
        }
        // flow=none 也应显式携带 (语义明确)
        cfg.flow = Flow::None;
        let args = respawn_args("127.0.0.1:9000", &cfg, 0);
        assert!(args.iter().any(|a| a == "--flow"));
        assert!(args.windows(2).any(|w| w[0] == "--flow" && w[1] == "none"));
    }

    /// 停机兜底 (qa-sprint2-fix): 挂着不退的客户端 (永不完成的 serve) 时,
    /// 停机仍**有界**完成 —— Close 先发 (COM 释放优先) → stop_active 置位 →
    /// 宽限耗尽返回 Err (hard_exit=false; 生产路径 hard_exit=true 在此 exit(0))。
    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_is_bounded_with_hung_client() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<HubCmd>();
        let (shutdown_tx, _) = watch::channel(false);
        let hub = Arc::new(HubState::new(SerialConfig::default(), 0));
        let ctx = PortCtx {
            hub: hub.clone(),
            bc_tx: broadcast::channel(16).0,
            tx_slot: Arc::new(std::sync::Mutex::new(None)),
            active_stop: Arc::new(std::sync::Mutex::new(None)),
        };
        let stop = Arc::new(AtomicBool::new(false));
        ctx.set_active_stop(stop.clone()); // 模拟在跑的会话
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<ServiceEvent>();
        let on_event: OnEvent = Arc::new(move |ev| {
            let _ = ev_tx.send(ev);
        });
        // 永不完成的 serve —— 模拟外部进程挂连接拖死优雅停机
        let never: tokio::task::JoinHandle<Result<(), std::io::Error>> =
            tokio::spawn(async { std::future::pending::<Result<(), std::io::Error>>().await });

        let restart_to: Arc<std::sync::Mutex<Option<String>>> =
            Arc::new(std::sync::Mutex::new(None));
        let res = finalize_shutdown(
            cmd_tx,
            ctx,
            never,
            Some(on_event),
            Duration::from_millis(200),
            false, // 测试不 exit 进程; 生产路径 true
            restart_to,
        )
        .await;

        assert!(res.is_err(), "宽限耗尽应走超时路径");
        assert!(
            matches!(cmd_rx.try_recv(), Ok(HubCmd::Close)),
            "COM 释放优先: Close 指令应已发出"
        );
        assert!(stop.load(Ordering::Relaxed), "stop_active 应已置位");
        assert!(
            ev_rx.try_recv().is_err(),
            "超时路径不应发 Stopped (进程由 exit(0) 结束)"
        );
        drop(shutdown_tx);
    }

    /// watch 触发路径端到端: send(true) → run_service 完成 → Stopped 事件 + Close 指令。
    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_watch_triggers_graceful_sequence() {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<HubCmd>();
        let (shutdown_tx, _) = watch::channel(false);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<SocketAddr>();
        let ready_tx = Arc::new(std::sync::Mutex::new(Some(ready_tx)));
        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<ServiceEvent>();
        let on_event: OnEvent = Arc::new(move |ev: ServiceEvent| {
            match ev {
                ServiceEvent::Ready(a) => {
                    if let Some(tx) = ready_tx.lock().unwrap().take() {
                        let _ = tx.send(a);
                    }
                }
                other => {
                    let _ = ev_tx.send(other);
                }
            }
        });
        let su = Startup {
            cfg: SerialConfig::default(),
            auto_open: false,
            addr: "127.0.0.1:0".parse().unwrap(),
            max_clients: 0,
            flow: crate::config::Flow::None,
            gui: false,
        };
        let srv = tokio::spawn(run_service(
            su,
            cmd_tx,
            cmd_rx,
            shutdown_tx.clone(),
            Some(on_event),
        ));
        let _addr = tokio::time::timeout(Duration::from_secs(5), ready_rx)
            .await
            .expect("服务 5s 内未就绪")
            .expect("Ready 事件缺失");

        shutdown_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(3), srv)
            .await
            .expect("watch 停机 3s 内未完成")
            .unwrap()
            .expect("run_service 不应报错");

        let mut saw_stopped = false;
        while let Ok(ev) = ev_rx.try_recv() {
            if matches!(ev, ServiceEvent::Stopped) {
                saw_stopped = true;
            }
        }
        assert!(saw_stopped, "停机完成后应发 Stopped 事件");
    }
}
