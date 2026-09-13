//! 自动重开监督任务 (FR-3 / ADR-4)。
//!
//! 独立于数据面: 本任务只做「打开 → 盯会话 → 掉线/失败重试」的状态机,
//! 一个字节都不碰。相位迁移全程写 HubState, /api/status 实时可见:
//!
//!   Closed --Open--> Opening --ok--> Open --异常--> Retry --1s--> Opening ...
//!                      |                |
//!                      +---- Close ---->+---- Close ---> Closed
//!
//! 重试间隔默认 1s; 测试通过 retry_delay 参数注入更短的值。
//! ADR-15①: 每次尝试失败进入 Retry 迁移时 hub.retries +1; 成功打开或用户
//! 手动 Close 归 0 —— UI 据此显示「正在自动重连 (第 n 次)」。
//! opener 做成 trait 是为了单测可以注入假串口 (真实串口状态机无法确定性复现)。

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;

use crate::hub::Phase;
use crate::serial::{PortCtx, PortEvent, PortOpener, PortSession};

/// 控制面 → 监督任务 的指令。/api/open 与 /api/close 各映射一条。
#[derive(Debug, Clone, Copy)]
pub enum HubCmd {
    Open,
    Close,
}

pub async fn run_supervisor(
    ctx: PortCtx,
    mut cmd_rx: UnboundedReceiver<HubCmd>,
    opener: Arc<dyn PortOpener>,
    retry_delay: Duration,
) {
    let hub = ctx.hub.clone();
    loop {
        hub.set_phase(Phase::Closed);
        ctx.clear_tx();

        // —— 等待打开指令 ——
        loop {
            match cmd_rx.recv().await {
                Some(HubCmd::Open) => break,
                Some(HubCmd::Close) => {} // 已是关闭态, 忽略
                None => return,           // 所有指令发送端都析构 (进程退出), 收尾
            }
        }

        // —— 打开 / 重试循环 ——
        loop {
            let cfg = hub.config();
            hub.set_phase(Phase::Opening);
            match opener.open(&cfg, &ctx) {
                Ok(sess) => {
                    hub.set_phase(Phase::Open);
                    hub.clear_last_error();
                    hub.clear_retries(); // ADR-15①: 成功打开归 0
                    let (user_closed, err) = monitor(sess, &mut cmd_rx).await;
                    ctx.clear_tx(); // 会话已结束, 后续客户端帧直接丢弃
                    ctx.clear_active_stop(); // 会话 stop 标志注销
                    if let Some(e) = err {
                        hub.set_last_error(e);
                    }
                    if user_closed {
                        break; // → Closed (retries 已在成功打开时归 0), 回到等待指令
                    }
                    // 异常断开 → Retry, 1s 后重开 (FR-3); 本次尝试失败计入重试次数 (ADR-15①)
                    hub.retry_inc();
                    hub.set_phase(Phase::Retry);
                    if !wait_retry(&mut cmd_rx, retry_delay).await {
                        hub.clear_retries(); // Close 打断重试等待 = 用户手动关闭, 归 0
                        break;
                    }
                }
                Err(e) => {
                    hub.set_last_error(e);
                    hub.retry_inc(); // ADR-15①: 打开失败 → Retry 迁移, 计数 +1
                    hub.set_phase(Phase::Retry);
                    if !wait_retry(&mut cmd_rx, retry_delay).await {
                        hub.clear_retries(); // Close 打断重试等待 = 用户手动关闭, 归 0
                        break;
                    }
                }
            }
        }
    }
}

/// 重试等待: 到点返回 true (继续重试); 收到 Close/通道关闭返回 false (回 Closed)。
/// 重试期间到达的 Open 指令视为"催一下", 立即结束等待去重开。
async fn wait_retry(cmd_rx: &mut UnboundedReceiver<HubCmd>, d: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(d) => true,
        cmd = cmd_rx.recv() => matches!(cmd, Some(HubCmd::Open)),
    }
}

/// 盯住一条已打开的会话, 直到读/写线程都退出。
/// 返回 (是否用户主动关闭, 最近一次异常原因)。
/// 用户主动关闭优先: 即使同时发生读写错误也不再重试 (尊重用户意图, 错误仍留痕)。
async fn monitor(
    mut sess: PortSession,
    cmd_rx: &mut UnboundedReceiver<HubCmd>,
) -> (bool, Option<String>) {
    let mut remaining = 2usize; // 读线程 + 写线程各一次 Exited
    let mut user_closed = false;
    let mut err: Option<String> = None;
    loop {
        tokio::select! {
            ev = sess.events.recv() => match ev {
                Some(PortEvent::Exited(reason)) => {
                    if let Some(r) = reason {
                        if err.is_none() {
                            err = Some(r);
                        }
                    }
                    remaining = remaining.saturating_sub(1);
                    if remaining == 0 {
                        return (user_closed, err);
                    }
                }
                None => {
                    return (user_closed, err.or_else(|| Some("串口会话事件通道意外关闭".into())));
                }
            },
            cmd = cmd_rx.recv() => match cmd {
                Some(HubCmd::Close) => {
                    user_closed = true;
                    sess.stop.store(true, Ordering::Relaxed); // 读/写线程轮询到后自行退出
                }
                Some(HubCmd::Open) => {} // 已打开, 忽略重复打开
                None => {
                    sess.stop.store(true, Ordering::Relaxed);
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SerialConfig;
    use crate::hub::{HubState, Phase};
    use crate::serial::PortCtx;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex as StdMutex;
    use tokio::sync::broadcast;
    use tokio::sync::mpsc;

    /// 假串口打开器, 四种行为可组合:
    /// - fail: 直接打开失败 (对应设备被占用/不存在)
    /// - fail_times: 前 N 次调用失败, 之后成功 (ADR-15① 连败→恢复的确定性注入)
    /// - stuck_ms: 打开动作阻塞指定毫秒 (用于确定性地观察 Opening 相位)
    /// - die: 打开"成功"但会话立刻以错误退出 (对应设备刚打开就掉线)
    /// - 否则: 会话存活, 直到收到 stop 才干净退出
    struct FakeOpener {
        fail: bool,
        fail_times: usize,
        stuck_ms: u64,
        die: bool,
        calls: AtomicUsize,
    }

    impl FakeOpener {
        fn new(fail: bool, stuck_ms: u64, die: bool) -> Self {
            Self { fail, fail_times: 0, stuck_ms, die, calls: AtomicUsize::new(0) }
        }

        /// 前 fail_times 次打开失败, 之后成功且会话存活 (连败 n 次后恢复)。
        fn fail_first(fail_times: usize) -> Self {
            Self { fail: false, fail_times, stuck_ms: 0, die: false, calls: AtomicUsize::new(0) }
        }
    }

    impl PortOpener for FakeOpener {
        fn open(&self, _cfg: &SerialConfig, _ctx: &PortCtx) -> Result<PortSession, String> {
            let n = self.calls.fetch_add(1, Ordering::Relaxed);
            if self.stuck_ms > 0 {
                std::thread::sleep(Duration::from_millis(self.stuck_ms));
            }
            if self.fail || n < self.fail_times {
                return Err("模拟: 打开失败 (设备被占用)".into());
            }
            let stop = Arc::new(AtomicBool::new(false));
            let (ev_tx, ev_rx) = mpsc::unbounded_channel();
            if self.die {
                // 延迟掉线: 让 Open 相位有可观察窗口 (真机也是"先活一小会再断")
                let ev = ev_tx.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(50));
                    let _ = ev.send(PortEvent::Exited(Some("模拟: 打开后立刻掉线".into())));
                    let _ = ev.send(PortEvent::Exited(None));
                });
            } else {
                for _ in 0..2 {
                    let stop = stop.clone();
                    let ev = ev_tx.clone();
                    std::thread::spawn(move || loop {
                        if stop.load(Ordering::Relaxed) {
                            let _ = ev.send(PortEvent::Exited(None));
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    });
                }
            }
            Ok(PortSession { stop, events: ev_rx })
        }
    }

    fn test_ctx() -> (Arc<HubState>, PortCtx) {
        let hub = Arc::new(HubState::new(SerialConfig::default(), 0));
        let ctx = PortCtx {
            hub: hub.clone(),
            bc_tx: broadcast::channel(16).0,
            tx_slot: Arc::new(StdMutex::new(None)),
            active_stop: Arc::new(StdMutex::new(None)),
        };
        (hub, ctx)
    }

    async fn wait_phase(hub: &HubState, want: Phase, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if hub.phase() == want {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return hub.phase() == want;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn closed_opening_retry_when_open_fails_then_close() {
        let (hub, ctx) = test_ctx();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let opener = Arc::new(FakeOpener::new(true, 0, false));
        tokio::spawn(run_supervisor(
            ctx,
            cmd_rx,
            opener.clone(),
            Duration::from_millis(20),
        ));

        assert_eq!(hub.phase(), Phase::Closed);
        cmd_tx.send(HubCmd::Open).unwrap();
        // Closed → Opening → (失败) → Retry, 且错误留痕
        assert!(wait_phase(&hub, Phase::Retry, Duration::from_secs(2)).await);
        assert!(hub.status_json().last_error.is_some());
        // 重试间隔内应至少尝试了两次
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(opener.calls.load(Ordering::Relaxed) >= 2);

        cmd_tx.send(HubCmd::Close).unwrap();
        assert!(wait_phase(&hub, Phase::Closed, Duration::from_secs(2)).await);
        // 关闭后不再重试
        let calls = opener.calls.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(opener.calls.load(Ordering::Relaxed), calls);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn opening_phase_observable_while_open_blocks() {
        let (hub, ctx) = test_ctx();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        tokio::spawn(run_supervisor(
            ctx,
            cmd_rx,
            Arc::new(FakeOpener::new(true, 150, false)),
            Duration::from_millis(20),
        ));
        cmd_tx.send(HubCmd::Open).unwrap();
        // 打开动作被卡住 → Opening 相位必须可见 (FR-3)
        assert!(wait_phase(&hub, Phase::Opening, Duration::from_secs(2)).await);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_ok_then_error_enters_retry_then_close() {
        let (hub, ctx) = test_ctx();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        tokio::spawn(run_supervisor(
            ctx,
            cmd_rx,
            Arc::new(FakeOpener::new(false, 0, true)),
            Duration::from_millis(20),
        ));

        cmd_tx.send(HubCmd::Open).unwrap();
        // 打开成功 → Open, 且清掉历史错误
        assert!(wait_phase(&hub, Phase::Open, Duration::from_secs(2)).await);
        // 会话立刻异常退出 → Retry + lastError
        assert!(wait_phase(&hub, Phase::Retry, Duration::from_secs(2)).await);
        assert!(hub
            .status_json()
            .last_error
            .unwrap_or_default()
            .contains("模拟"));
        cmd_tx.send(HubCmd::Close).unwrap();
        assert!(wait_phase(&hub, Phase::Closed, Duration::from_secs(2)).await);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn clean_user_close_yields_closed_without_error() {
        let (hub, ctx) = test_ctx();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        tokio::spawn(run_supervisor(
            ctx,
            cmd_rx,
            Arc::new(FakeOpener::new(false, 0, false)),
            Duration::from_millis(20),
        ));

        cmd_tx.send(HubCmd::Open).unwrap();
        assert!(wait_phase(&hub, Phase::Open, Duration::from_secs(2)).await);
        cmd_tx.send(HubCmd::Close).unwrap();
        assert!(wait_phase(&hub, Phase::Closed, Duration::from_secs(2)).await);
        assert!(hub.status_json().last_error.is_none());

        // 关闭后重复 open → 能再次打开 ( Closed → Open 往返)
        cmd_tx.send(HubCmd::Open).unwrap();
        assert!(wait_phase(&hub, Phase::Open, Duration::from_secs(2)).await);
        cmd_tx.send(HubCmd::Close).unwrap();
        assert!(wait_phase(&hub, Phase::Closed, Duration::from_secs(2)).await);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn close_while_retry_sleeping_takes_effect_promptly() {
        let (hub, ctx) = test_ctx();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        tokio::spawn(run_supervisor(
            ctx,
            cmd_rx,
            Arc::new(FakeOpener::new(true, 0, false)),
            Duration::from_secs(5), // 故意拉长: Close 必须能打断重试等待
        ));
        cmd_tx.send(HubCmd::Open).unwrap();
        assert!(wait_phase(&hub, Phase::Retry, Duration::from_secs(2)).await);
        cmd_tx.send(HubCmd::Close).unwrap();
        // 不应等满 5s
        assert!(wait_phase(&hub, Phase::Closed, Duration::from_millis(500)).await);
    }

    /// ADR-15①: 每次 retry 迁移 retries +1 —— 假打开器连败 3 次 → retries==3;
    /// 第 4 次尝试成功打开 → 归 0; /api/status 投影与计数同源回显。
    #[tokio::test(flavor = "multi_thread")]
    async fn retries_increment_per_failed_attempt_and_reset_on_success() {
        let (hub, ctx) = test_ctx();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        tokio::spawn(run_supervisor(
            ctx,
            cmd_rx,
            Arc::new(FakeOpener::fail_first(3)),
            // 重试等待拉长: 第 3 次失败后有 400ms 稳定窗口观察 retries==3
            Duration::from_millis(400),
        ));
        cmd_tx.send(HubCmd::Open).unwrap();
        // 连败 3 次: 每次打开失败 → Retry 迁移 → 计数 +1
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while hub.retries() < 3 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "3 次连败内 retries 未递增到 3 (现 {})",
                hub.retries()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(hub.retries(), 3, "连败恰 3 次, 计数应为 3");
        assert_eq!(hub.status_json().retries, 3, "/api/status 投影须回显同一计数");
        assert_eq!(hub.phase(), Phase::Retry);
        // 第 4 次尝试成功打开 → 归 0
        assert!(wait_phase(&hub, Phase::Open, Duration::from_secs(3)).await);
        assert_eq!(hub.retries(), 0, "成功打开归 0");
        assert_eq!(hub.status_json().retries, 0);
        cmd_tx.send(HubCmd::Close).unwrap();
        assert!(wait_phase(&hub, Phase::Closed, Duration::from_secs(2)).await);
    }

    /// ADR-15①: 用户手动 close 归 0 —— 重试等待窗口内 Close 打断, 计数清零回 Closed。
    #[tokio::test(flavor = "multi_thread")]
    async fn retries_reset_on_user_close_during_retry() {
        let (hub, ctx) = test_ctx();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        tokio::spawn(run_supervisor(
            ctx,
            cmd_rx,
            Arc::new(FakeOpener::new(true, 0, false)),
            Duration::from_secs(5), // 重试等待拉长, Close 一定落在等待窗口内
        ));
        cmd_tx.send(HubCmd::Open).unwrap();
        assert!(wait_phase(&hub, Phase::Retry, Duration::from_secs(2)).await);
        assert_eq!(hub.retries(), 1, "打开失败 1 次 → 计数 1 (5s 内不会有第 2 次尝试)");
        cmd_tx.send(HubCmd::Close).unwrap();
        assert!(wait_phase(&hub, Phase::Closed, Duration::from_millis(500)).await);
        assert_eq!(hub.retries(), 0, "用户手动 close 归 0");
    }
}
