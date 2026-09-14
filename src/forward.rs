//! TCP 旁路转发 (FR-20 / ADR-24②, 每桥 v1 客户端模式)。
//!
//! 语义: 桥配置 `forwardTcp: "host:port"` (可空 = 关闭)。桥运行期间启动一个
//! TCP **客户端**任务连接该地址, 把串口 RX (与数据面/tap 同一条 `bc_tx` 广播源)
//! **单向**转发出去; TCP 对端发来的数据一律不读不回注 —— WS 上行走 `tx_bc`/
//! `send_to_port`, 转发任务根本不订阅它, 天然防环路 (ADR-24②)。
//!
//! 生命周期: 会话随桥数据面启停 (create/start 建, stop/delete 断), 目标地址经
//! watch 通道热更新 —— 运行中改配置 → 断开旧连接按新值重连 (改空 = 断开待命)。
//! 断线 (连接失败/写失败) 3s 自动重连, 期间串口 RX 照常分流给 WS/tap, 转发侧
//! 丢帧不补发 (v1 取舍: 旁路观测语义, 不做背压缓冲)。
//!
//! 状态可见: `forwardConnected` (bool, 当前 TCP 是否连着) 与 `forwardTcp`
//! (配置回显) 都在桥对象 detail_json 上 (契约 17→19 字段)。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::watch;

use crate::fleet::Bridge;
use crate::hub::lock_mutex;

/// 会话代号: 防旧会话任务抹掉新会话槽位 (record::GEN 同款防混淆口径)。
static GEN: AtomicU64 = AtomicU64::new(1);

/// 断线重连间隔 (FR-20: 断线 3s 自动重连)。
const RECONNECT_EVERY: Duration = Duration::from_secs(3);
/// 单次连接尝试超时 (失败也走 3s 节拍, 不卡死任务)。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// 运行中的转发会话 (存于 Bridge.forward 槽位)。
pub struct ForwardSession {
    pub gen: u64,
    /// 会话停机开关 (桥 stop/删除 → true)。
    pub stop_tx: watch::Sender<bool>,
    /// 目标地址热更新 ("host:port"; 空串 = 关闭待命)。
    pub ctl_tx: watch::Sender<String>,
}

/// 目标地址校验 (create/config/import 共用): 空/全空白 = 关闭 (合法);
/// 否则须形如 "host:port", host 非空且不含空白, port 1..=65535。
/// 宽松校验 (不做 DNS 解析): 域名到连接时才解析, 失败走 3s 重连节拍并留痕。
pub fn validate_target(s: &str) -> Result<(), String> {
    let t = s.trim();
    if t.is_empty() {
        return Ok(());
    }
    if t.contains(char::is_whitespace) {
        return Err(format!("forwardTcp 不能含空白字符: \"{s}\""));
    }
    let Some((host, port)) = t.rsplit_once(':') else {
        return Err(format!("forwardTcp 须形如 host:port (缺端口): \"{s}\""));
    };
    if host.is_empty() {
        return Err(format!("forwardTcp host 不能为空: \"{s}\""));
    }
    match port.parse::<u16>() {
        Ok(0) => Err(format!("forwardTcp 端口不能为 0: \"{s}\"")),
        Ok(_) => Ok(()),
        Err(_) => Err(format!("forwardTcp 端口非法 (1~65535): \"{s}\"")),
    }
}

/// 开启转发会话 (桥启动时调用; 幂等 —— 已有会话则不动)。
pub fn start_session(b: &Arc<Bridge>) {
    let mut slot = lock_mutex(&b.forward);
    if slot.is_some() {
        return;
    }
    let (stop_tx, stop_rx) = watch::channel(false);
    let (ctl_tx, ctl_rx) = watch::channel(b.forward_target());
    let gen = GEN.fetch_add(1, Ordering::Relaxed);
    *slot = Some(ForwardSession {
        gen,
        stop_tx,
        ctl_tx,
    });
    drop(slot);
    let b2 = b.clone();
    tokio::spawn(run(b2, gen, ctl_rx, stop_rx));
}

/// 结束转发会话 (桥停止/删除时调用; 幂等)。TCP 连接由任务自身收尾关闭。
pub fn stop_session(b: &Arc<Bridge>) {
    if let Some(s) = lock_mutex(&b.forward).take() {
        let _ = s.stop_tx.send(true);
    }
}

/// 配置变更 (运行中热生效): 更新 Bridge 配置真相 + 通知会话任务重连。
/// 会话不在 (桥停止中) 时只落配置, start_session 会以新值起任务。
pub fn apply_target(b: &Arc<Bridge>, target: String) {
    b.set_forward_target(target.clone());
    if let Some(s) = lock_mutex(&b.forward).as_ref() {
        let _ = s.ctl_tx.send(target);
    }
}

async fn run(
    b: Arc<Bridge>,
    gen: u64,
    mut ctl: watch::Receiver<String>,
    mut stop: watch::Receiver<bool>,
) {
    // 会话级订阅 (一次): 断线/重连间隙的帧不丢 (≤1024 缓冲, 溢出走 Lagged 跳过);
    // 关闭待命分支主动排空 —— 转发关了就不许有旧帧延迟出站。
    let mut bc_rx = b.ctx.bc_tx.subscribe();
    let mut target = ctl.borrow_and_update().clone();
    let mut connected_to: Option<String> = None;
    'outer: loop {
        if *stop.borrow() {
            break 'outer;
        }
        if target.trim().is_empty() {
            // 关闭待命: 排空积压 (转发关了不回灌), 等配置变更或停机
            b.forward_connected
                .store(false, std::sync::atomic::Ordering::Relaxed);
            while bc_rx.try_recv().is_ok() {}
            tokio::select! {
                _ = ctl.changed() => {
                    target = ctl.borrow_and_update().clone();
                }
                _ = stop.changed() => break 'outer,
            }
            continue;
        }
        // 连接 (单次尝试限时, 失败走 3s 节拍)
        let attempt = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(&target));
        match attempt.await {
            Ok(Ok(stream)) => {
                b.forward_connected
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                connected_to = Some(target.clone());
                crate::logging::write("info", &format!("桥 {} 旁路转发已连接 {target}", b.id));
                pump(&b, stream, &mut ctl, &mut stop, &mut bc_rx).await;
                b.forward_connected
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                crate::logging::write("info", &format!("桥 {} 旁路转发连接断开 ({target})", b.id));
            }
            Ok(Err(e)) => {
                b.forward_connected
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                crate::logging::write(
                    "error",
                    &format!("桥 {} 旁路转发连接失败 ({target}): {e}", b.id),
                );
            }
            Err(_) => {
                b.forward_connected
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                crate::logging::write("error", &format!("桥 {} 旁路转发连接超时 ({target})", b.id));
            }
        }
        if *stop.borrow() {
            break 'outer;
        }
        // 收尾后取最新目标: 配置变更 → 立即按新值重连 (不熬 3s);
        // 同目标断线 → 3s 后重连 (FR-20)。
        target = ctl.borrow_and_update().clone();
        if connected_to.as_deref() == Some(target.as_str()) && !target.trim().is_empty() {
            tokio::select! {
                _ = tokio::time::sleep(RECONNECT_EVERY) => {}
                _ = ctl.changed() => {
                    target = ctl.borrow_and_update().clone();
                }
                _ = stop.changed() => break 'outer,
            }
        }
        connected_to = None;
    }
    // 自清理: 仅当槽位仍属本会话 (新会话已起则不动)
    let mut slot = lock_mutex(&b.forward);
    if slot.as_ref().map(|s| s.gen) == Some(gen) {
        *slot = None;
    }
    b.forward_connected
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

/// 单连接泵: bc_tx (串口 RX, 与 WS/tap 同源) → TCP 单向写。
/// 返回条件: 会话停机 / 目标变更 / 写失败 (对端断开) / 源广播关闭。
/// bc_rx 为会话级订阅 (run 持有), 断线重连间隙的帧在通道缓冲内不丢。
async fn pump(
    b: &Arc<Bridge>,
    mut stream: TcpStream,
    ctl: &mut watch::Receiver<String>,
    stop: &mut watch::Receiver<bool>,
    bc_rx: &mut tokio::sync::broadcast::Receiver<Vec<u8>>,
) {
    loop {
        tokio::select! {
            f = bc_rx.recv() => match f {
                Ok(data) => {
                    if let Err(e) = stream.write_all(&data).await {
                        crate::logging::write(
                            "error",
                            &format!("桥 {} 旁路转发写失败: {e}", b.id),
                        );
                        return;
                    }
                    let _ = stream.flush().await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            },
            _ = ctl.changed() => return,   // 配置变更 → 断开旧连接 (外层按新值重连)
            _ = stop.changed() => return,  // 桥停/删 → 断开
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_target_accepts_and_rejects() {
        assert!(validate_target("").is_ok()); // 关闭
        assert!(validate_target("  ").is_ok());
        assert!(validate_target("127.0.0.1:9000").is_ok());
        assert!(validate_target("plc.example.com:1883").is_ok());
        assert!(validate_target("[::1]:9000").is_ok());
        for bad in [
            "127.0.0.1",   // 缺端口
            ":9000",       // 缺 host
            "host:0",      // 端口 0
            "host:99999",  // 越界
            "host:abc",    // 非数字
            "host :9000",  // 空白
            "host:9000 x", // 空白
        ] {
            assert!(validate_target(bad).is_err(), "\"{bad}\" 应被拒绝");
        }
    }
}
