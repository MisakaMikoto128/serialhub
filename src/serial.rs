//! 数据面: 串口打开 + 读/写线程 (FR-1)。
//!
//! 设计取舍:
//! - serialport-rs 是同步阻塞 API, 因此读、写各占一条 OS 线程 (try_clone 双句柄),
//!   与 tokio 世界通过 channel 解耦 —— 串口读循环永不阻塞在任何客户端上 (硬约束 2);
//! - RX: 读线程 → broadcast 扇出给所有 WS 客户端 (ADR-3); 慢客户端只丢旧帧
//!   (Lagged), 绝不反压读线程;
//! - TX: 客户端帧 → tx_slot 里的 mpsc 队列 → 单写者线程 FIFO 串行写串口;
//!   同批到达的小帧合并成一次 write_all (仅省 syscall, 不破坏 FIFO 顺序);
//! - 生命周期: 每条会话一个 stop 标志 + events 通道; 任何线程退出前先置 stop
//!   再上报 PortEvent::Exited 一次 (读/写共两次), 监督任务据此迁移状态机;
//! - 崩溃面 (FR-7): 本模块所有锁访问走带毒兜底, 无 unwrap, 不 panic。

use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender as StdSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::broadcast;
use tokio::sync::mpsc;

use crate::config::{Flow, Parity, SerialConfig};
use crate::hub::{lock_mutex, HubState};

/// 串口会话生命周期信号: 读、写线程各上报一次 `Exited`;
/// reason = Some 表示异常退出 (监督任务据此进 Retry 并写 lastError)。
pub enum PortEvent {
    Exited(Option<String>),
}

/// 打开串口所需的环境句柄 (监督任务持有, 打开器使用)。
#[derive(Clone)]
pub struct PortCtx {
    pub hub: Arc<HubState>,
    pub bc_tx: broadcast::Sender<Vec<u8>>,
    /// 当前会话的 TX 队列发送端; None = 串口未打开, 客户端帧直接丢弃。
    pub tx_slot: Arc<Mutex<Option<StdSender<Vec<u8>>>>>,
}

impl PortCtx {
    pub fn set_tx(&self, tx: StdSender<Vec<u8>>) {
        *lock_mutex(&self.tx_slot) = Some(tx);
    }

    pub fn clear_tx(&self) {
        *lock_mutex(&self.tx_slot) = None;
    }

    /// 串口未打开或写线程已死 → 直接丢弃返回 false (绝不 panic)。
    pub fn send_to_port(&self, data: &[u8]) -> bool {
        match &*lock_mutex(&self.tx_slot) {
            Some(tx) => tx.send(data.to_vec()).is_ok(),
            None => false,
        }
    }
}

/// 一条已打开的串口会话: 监督任务用它停线程、收退出事件。
pub struct PortSession {
    pub stop: Arc<AtomicBool>,
    pub events: mpsc::UnboundedReceiver<PortEvent>,
}

pub trait PortOpener: Send + Sync {
    fn open(&self, cfg: &SerialConfig, ctx: &PortCtx) -> Result<PortSession, String>;
}

/// 生产打开器: serialport-rs, 读/写线程在这里拉起。
pub struct RealOpener;

impl PortOpener for RealOpener {
    fn open(&self, cfg: &SerialConfig, ctx: &PortCtx) -> Result<PortSession, String> {
        cfg.validate()?;
        if cfg.port.is_empty() {
            return Err("未配置串口名称".into());
        }

        let port = serialport::new(&cfg.port, cfg.baud)
            .data_bits(match cfg.data_bits {
                7 => serialport::DataBits::Seven,
                _ => serialport::DataBits::Eight,
            })
            .parity(match cfg.parity {
                Parity::N => serialport::Parity::None,
                Parity::E => serialport::Parity::Even,
                Parity::O => serialport::Parity::Odd,
            })
            .stop_bits(match cfg.stop_bits {
                2 => serialport::StopBits::Two,
                _ => serialport::StopBits::One,
            })
            .flow_control(match cfg.flow {
                Flow::None => serialport::FlowControl::None,
                Flow::RtsCts => serialport::FlowControl::Hardware,
                Flow::XonXoff => serialport::FlowControl::Software,
            })
            .timeout(Duration::from_millis(50)) // 读超时 = 读线程的轮询节拍
            .open()
            .map_err(|e| format!("打开 {} 失败: {e}", cfg.port))?;

        let stop = Arc::new(AtomicBool::new(false));
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let (tx, rx) = channel::<Vec<u8>>();
        ctx.set_tx(tx);

        let w_port = port
            .try_clone()
            .map_err(|e| format!("串口句柄克隆失败: {e}"))?;

        {
            let stop = stop.clone();
            let ev = ev_tx.clone();
            let hub = ctx.hub.clone();
            let bc = ctx.bc_tx.clone();
            std::thread::Builder::new()
                .name("serial-rx".into())
                .spawn(move || reader_loop(port, stop, ev, hub, bc))
                .map_err(|e| format!("启动串口读线程失败: {e}"))?;
        }
        {
            let stop = stop.clone();
            let ev = ev_tx;
            let hub = ctx.hub.clone();
            std::thread::Builder::new()
                .name("serial-tx".into())
                .spawn(move || writer_loop(w_port, stop, ev, hub, rx))
                .map_err(|e| format!("启动串口写线程失败: {e}"))?;
        }

        Ok(PortSession { stop, events: ev_rx })
    }
}

fn reader_loop(
    mut port: Box<dyn serialport::SerialPort>,
    stop: Arc<AtomicBool>,
    ev: mpsc::UnboundedSender<PortEvent>,
    hub: Arc<HubState>,
    bc: broadcast::Sender<Vec<u8>>,
) {
    let mut buf = [0u8; 1024];
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = ev.send(PortEvent::Exited(None));
            return;
        }
        match port.read(&mut buf) {
            Ok(0) => continue,
            Ok(n) => {
                hub.add_rx(n);
                // 无客户端时 send 返回 Err, 丢弃即可 (读循环绝不被反压)
                let _ = bc.send(buf[..n].to_vec());
            }
            Err(e) if e.kind() == ErrorKind::TimedOut => continue,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                stop.store(true, Ordering::Relaxed); // 让写线程也退出
                let _ = ev.send(PortEvent::Exited(Some(format!("串口读取错误: {e}"))));
                return;
            }
        }
    }
}

fn writer_loop(
    mut port: Box<dyn serialport::SerialPort>,
    stop: Arc<AtomicBool>,
    ev: mpsc::UnboundedSender<PortEvent>,
    hub: Arc<HubState>,
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
) {
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = ev.send(PortEvent::Exited(None));
            return;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(first) => {
                // 同批小帧合并 (FIFO 顺序不变); 上限防止单次 write 过大
                let mut batch = first;
                while batch.len() < 64 * 1024 {
                    match rx.try_recv() {
                        Ok(more) => batch.extend_from_slice(&more),
                        Err(_) => break,
                    }
                }
                match port.write_all(&batch).and_then(|_| port.flush()) {
                    Ok(()) => hub.add_tx(batch.len()),
                    Err(e) => {
                        stop.store(true, Ordering::Relaxed); // 让读线程也退出
                        let _ = ev.send(PortEvent::Exited(Some(format!("串口写入错误: {e}"))));
                        return;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                stop.store(true, Ordering::Relaxed);
                let _ = ev.send(PortEvent::Exited(None));
                return;
            }
        }
    }
}

/// 本机串口枚举 (FR-4 /api/ports 与 CLI --list-ports 共用)。
/// desc: USB 串口尽量给出产品/厂商名; 非 USB (含虚拟对) 可能为空串。
pub fn list_ports() -> Vec<(String, String)> {
    match serialport::available_ports() {
        Ok(list) => list
            .into_iter()
            .map(|p| {
                let desc = match &p.port_type {
                    serialport::SerialPortType::UsbPort(info) => info
                        .product
                        .clone()
                        .or_else(|| info.manufacturer.clone())
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                (p.port_name, desc)
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_ports_never_panics() {
        // 任何机器 (包括 CI 无串口) 都只应返回空表或列表
        let _ = list_ports();
    }

    #[test]
    fn tx_slot_roundtrip() {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let ctx = PortCtx {
            hub: Arc::new(HubState::new(SerialConfig::default())),
            bc_tx: broadcast::channel(16).0,
            tx_slot: Arc::new(Mutex::new(None)),
        };
        assert!(!ctx.send_to_port(b"x")); // 未安装 = 丢弃
        ctx.set_tx(tx);
        assert!(ctx.send_to_port(b"abc"));
        assert_eq!(rx.recv_timeout(Duration::from_millis(100)).unwrap(), b"abc");
        drop(rx); // 写侧消费者消失 → send 失败而非 panic
        assert!(!ctx.send_to_port(b"abc"));
        ctx.clear_tx();
        assert!(!ctx.send_to_port(b"abc"));
    }
}
