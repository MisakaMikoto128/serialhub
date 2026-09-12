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

use crate::config::SerialConfig;
use crate::hub::{HubState, Phase};
use crate::serial::PortCtx;
use crate::supervisor::{self, HubCmd};

/// GUI/headless 都需要的启动参数 (由 cli::Cli 派生, 见 Cli::startup)。
#[derive(Debug, Clone)]
pub struct Startup {
    pub cfg: SerialConfig,
    pub auto_open: bool,
    pub addr: SocketAddr,
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
    let hub = Arc::new(HubState::new(su.cfg));
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

    // 1) 先绑定: 端口被占用立即失败 —— 不开串口、不进服务循环 (FR-8)
    let listener = TcpListener::bind(su.addr)
        .await
        .map_err(|e| format!("端口被占用: 无法监听 {}: {e}", su.addr))?;

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
    let app = crate::api::App {
        ctx: ctx.clone(),
        cmd_tx: cmd_tx.clone(),
        shutdown_tx: shutdown_tx.clone(),
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
    finalize_shutdown(cmd_tx, ctx, serve, on_event, SHUTDOWN_GRACE, true).await
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
) -> Result<(), String> {
    // COM 释放优先: 关串口 + 置会话 stop 标志 (读/写线程 <=150ms 自行退出)
    let _ = cmd_tx.send(HubCmd::Close);
    ctx.stop_active();
    tokio::time::sleep(SERIAL_THREAD_DRAIN).await;
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

    /// 停机兜底 (qa-sprint2-fix): 挂着不退的客户端 (永不完成的 serve) 时,
    /// 停机仍**有界**完成 —— Close 先发 (COM 释放优先) → stop_active 置位 →
    /// 宽限耗尽返回 Err (hard_exit=false; 生产路径 hard_exit=true 在此 exit(0))。
    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_is_bounded_with_hung_client() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<HubCmd>();
        let (shutdown_tx, _) = watch::channel(false);
        let hub = Arc::new(HubState::new(SerialConfig::default()));
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

        let res = finalize_shutdown(
            cmd_tx,
            ctx,
            never,
            Some(on_event),
            Duration::from_millis(200),
            false, // 测试不 exit 进程; 生产路径 true
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
