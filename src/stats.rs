//! 统计引擎 (FR-10c): 每桥 RX/TX 速率 = 1 秒滑动窗口。
//!
//! 采样模型: 管理器任务每 200ms 把各桥的**累计**字节数推入窗口;
//! 读取 (GET /api/fleet) 时取窗口内首尾两个样本做差分 → 字节/秒。
//! 只存累计值不存增量, 采样线程与读取方零共享可变状态竞争 (Mutex 短临界区)。
//!
//! 纯结构 + 时间戳由调用方注入 (push/rates 都收 Instant) → 可确定性单测,
//! 不依赖真实 sleep。

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// 滑窗宽度 (spec FR-10c: 速率每秒刷新, 口径 = 1s 滑窗)。
const WINDOW: Duration = Duration::from_secs(1);
/// 防御性样本上限 (200ms 采样 → 1s 窗口约需 6 个, 32 绰绰有余)。
const MAX_SAMPLES: usize = 32;
/// 时间差低于此值视为不可信 (除零/抖动), 返回 0。
const MIN_DT: Duration = Duration::from_millis(50);

#[derive(Debug, Default)]
pub struct RateWindow {
    /// (采样时刻, rx 累计, tx 累计), 按时间升序。
    samples: VecDeque<(Instant, u64, u64)>,
}

impl RateWindow {
    pub fn new() -> Self {
        Self::default()
    }

    /// 推入一个采样点 (累计字节数, 非增量)。
    pub fn push(&mut self, at: Instant, rx_total: u64, tx_total: u64) {
        self.samples.push_back((at, rx_total, tx_total));
        while self.samples.len() > MAX_SAMPLES {
            self.samples.pop_front();
        }
    }

    /// 计算窗口内的 (rxRate, txRate), 字节/秒。
    /// 样本不足 2 个 / 时间差过小 → (0, 0)。
    /// 过期样本 (时刻 < now-1s) 丢弃, 但保留**一条**作差分基线锚点,
    /// 使窗口滑动时速率平滑衰减而不是突跳为 0。
    pub fn rates(&mut self, now: Instant) -> (f64, f64) {
        if let Some(cut) = now.checked_sub(WINDOW) {
            // front 是当前基线锚点; 若第二条样本也过期, front 可安全丢弃
            while self.samples.len() > 2
                && self.samples.get(1).map(|(t, _, _)| *t <= cut).unwrap_or(false)
            {
                self.samples.pop_front();
            }
        }
        let (Some((t0, rx0, tx0)), Some((t1, rx1, tx1))) =
            (self.samples.front().copied(), self.samples.back().copied())
        else {
            return (0.0, 0.0);
        };
        let Some(dt) = t1.checked_duration_since(t0) else {
            return (0.0, 0.0); // 乱序样本 (不应发生), 防御
        };
        if dt < MIN_DT {
            return (0.0, 0.0);
        }
        let secs = dt.as_secs_f64();
        (
            rx1.saturating_sub(rx0) as f64 / secs,
            tx1.saturating_sub(tx0) as f64 / secs,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_with_fewer_than_two_samples() {
        let t = Instant::now();
        let mut w = RateWindow::new();
        assert_eq!(w.rates(t), (0.0, 0.0)); // 空
        w.push(t, 100, 200);
        assert_eq!(w.rates(t), (0.0, 0.0)); // 单样本无差分基准
    }

    #[test]
    fn computes_bytes_per_second() {
        let t0 = Instant::now();
        let mut w = RateWindow::new();
        w.push(t0, 0, 0);
        w.push(t0 + Duration::from_millis(500), 500, 100);
        let (rx, tx) = w.rates(t0 + Duration::from_millis(600));
        assert!((rx - 1000.0).abs() < 1e-9, "500B/0.5s = 1000B/s, 得 {rx}");
        assert!((tx - 200.0).abs() < 1e-9, "100B/0.5s = 200B/s, 得 {tx}");
    }

    #[test]
    fn window_slides_and_decays_without_new_traffic() {
        let t0 = Instant::now();
        let mut w = RateWindow::new();
        w.push(t0, 0, 0);
        w.push(t0 + Duration::from_millis(200), 1000, 0); // 5000 B/s
        let (rx, _) = w.rates(t0 + Duration::from_millis(300));
        assert!((rx - 5000.0).abs() < 1.0);
        // 1.4s 后: 早期样本滑出窗口 (仅留锚点), 无新增字节 → 速率归零
        w.push(t0 + Duration::from_millis(1400), 1000, 0);
        let (rx, _) = w.rates(t0 + Duration::from_millis(1500));
        assert!(rx.abs() < 1e-9, "窗口滑出后无新增流量应为 0, 得 {rx}");
        // 窗口内部的过期样本已被清理, 不无限堆积
        assert!(w.samples.len() <= 3);
    }

    #[test]
    fn saturates_on_counter_regression() {
        // 防御: 计数器回退 (理论不可能) 不得产生负速率/panic
        let t0 = Instant::now();
        let mut w = RateWindow::new();
        w.push(t0, 1000, 1000);
        w.push(t0 + Duration::from_millis(500), 100, 100);
        let (rx, tx) = w.rates(t0 + Duration::from_millis(600));
        assert_eq!((rx, tx), (0.0, 0.0));
    }
}
