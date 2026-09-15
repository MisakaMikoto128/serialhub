//! 数据录制与回放 (FR-19 / ADR-24 Sprint 13 B1)。
//!
//! 录制 = tap 同源 tee:
//! - RX 订阅 `ctx.bc_tx` (与数据面 WS / tap 同一条广播源, 帧语义零变化);
//! - TX 订阅 `ctx.tx_bc` (PortCtx::send_to_port 成功入队时的 tee —— 凡进
//!   串口 TX 队列的帧必经此, 含 WS 客户端上行与回放注入);
//!
//! 每帧一行 JSONL 落盘 `{"ts":<相对录制开始毫秒>,"dir":"rx|tx","hex":"小写hex"}`,
//! 带缓冲 + 500ms 定期 flush; 桥停止/删除/进程退出时自动收尾, **录像文件保留**。
//! 停止时写侧车索引 `<file>.meta.json` (`{frames,bytes,txFrames,durationSec}`) ——
//! 列表端点免逐行重扫, 且给出 txFrames (回放可见性: 0 tx 帧 = 回放无输出, 前端
//! 据此警告); 旧录像/进程硬退无侧车 → 列表回退逐行扫描, txFrames = null。
//!
//! 回放 = 把选中录像按行间原始时序 (ts 差 / speed) 写回**串口 TX** (走现有
//! tx 队列, PortCtx::send_to_port), 用于固件复现/测试注入:
//! - **只回放 tx 行** (ADR-24⑥: 网页→设备 的原始命令重放给设备; rx 行是设备
//!   说的话, 不回注 —— "固件复现"语义 = 把当初发给固件的流量重放给它);
//! - 仅当串口 open (hub.phase == Open) 才受理;
//! - speed 0.5~10 (FR-19 倍率范围); loop=true 循环到被停止;
//! - 行间时序 = 相邻 tx 行的 ts 差 / speed (首帧立即发, 不按首行 ts 绝对时刻
//!   等待 —— 兼容手工构造的绝对 epoch ms 录像, QA 计划 §3 A1);
//! - file 参数必须解析为录像目录内的已有文件 (白名单字符 + canonicalize
//!   复核, 与 themes::resolve 同口径的双重防穿越)。
//!
//! 状态可见: 桥对象 detail_json 带 `recording` / `replay` 两个字段
//! (null 或进行中的会话摘要), 供管理台渲染录制钮/回放进度。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::sync::oneshot;
use tokio::sync::watch;

use crate::fleet::Bridge;
use crate::hub::{lock_mutex, Phase};

/// 会话代号 (录制/回放共用): 槽位清理时与任务自清理比对, 防止旧任务抹掉新会话。
static GEN: AtomicU64 = AtomicU64::new(1);

/// 文件名长度上限 (防滥用)。
const MAX_NAME: usize = 128;
/// 录制缓冲 flush 周期。
const FLUSH_EVERY: Duration = Duration::from_millis(500);
/// FR-19: 回放速度倍率范围。
const SPEED_MIN: f64 = 0.5;
const SPEED_MAX: f64 = 10.0;

// ============================================================ 目录与文件名

/// 默认录像目录: exe 旁 recordings/ (exe 定位失败 → 相对路径 recordings/)。
pub fn default_recordings_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("recordings")))
        .unwrap_or_else(|| PathBuf::from("recordings"))
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 天数 → (年, 月, 日) (Howard Hinnant civil_from_days, 公历)。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// unix 毫秒 → "yyyymmdd-hhmmss" (UTC; 文件名戳, 排序友好)。
fn stamp_utc(ms: u64) -> String {
    let secs = ms / 1000;
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// 路径穿越防护 (双重防线, 与 themes::resolve 同口径):
/// ① 白名单字符 (ASCII 字母/数字/点/短横线/下划线) + 拒绝分隔符与 `..`;
/// ② canonicalize 后复核仍落在录像目录内 (防符号链接/8.3 短名等花样)。
pub fn resolve_file(dir: &Path, file: &str) -> Result<PathBuf, String> {
    let name = file.trim();
    if name.is_empty()
        || name.len() > MAX_NAME
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err(format!("非法文件名 (路径穿越拒绝): \"{file}\""));
    }
    let (Ok(cp), Ok(cd)) = (dir.join(name).canonicalize(), dir.canonicalize()) else {
        return Err(format!("录像文件不存在: {name}"));
    };
    if cp.starts_with(&cd) && cp.is_file() {
        Ok(cp)
    } else {
        Err(format!("录像文件不存在或不在录像目录内: {name}"))
    }
}

fn hex_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

// ============================================================ 录制

/// 进行中的录制会话 (存于 Bridge.rec 槽位)。
pub struct RecHandle {
    pub gen: u64,
    /// 文件名 (相对录像目录)。
    pub file_name: String,
    /// 开始时刻 (unix ms)。
    pub started_at_ms: u64,
    stop_tx: watch::Sender<bool>,
    done: oneshot::Receiver<RecSummary>,
}

struct RecSummary {
    pub frames: u64,
    pub bytes: u64,
}

/// 开始录制 (每桥一个状态机: 空闲/录制中)。成功返回文件名。
/// 桥未运行 (数据面已停) 不受理; 已在录制中不受理 (非幂等)。
pub fn start(b: &Arc<Bridge>, dir: &Path) -> Result<String, String> {
    let mut slot = lock_mutex(&b.rec);
    if slot.is_some() {
        return Err("已在录制中: 请先停止当前录制".into());
    }
    if !b.is_running() {
        return Err("桥未运行: 录制需先启动桥".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("创建录像目录失败: {e}"))?;
    // 文件名 <桥id>-<yyyymmdd-hhmmss>.jsonl; 同秒重开加序号 (不覆盖既有录像)
    let stamp = stamp_utc(now_unix_ms());
    let mut name = format!("{}-{}.jsonl", b.id, stamp);
    let mut k = 1u32;
    while dir.join(&name).exists() {
        name = format!("{}-{}-{}.jsonl", b.id, stamp, k);
        k += 1;
    }
    let path = dir.join(&name);
    let file = std::fs::File::create(&path).map_err(|e| format!("创建录像文件失败: {e}"))?;
    // 订阅必须在返回前建立 (同步): start 返回即 tee 生效, 不丢首批帧
    let rx_sub = b.ctx.bc_tx.subscribe();
    let tx_sub = b.ctx.tx_bc.subscribe();
    let bridge_stop = b.stop_tx.subscribe();
    let (stop_tx, stop_rx) = watch::channel(false);
    let (done_tx, done_rx) = oneshot::channel();
    let gen = GEN.fetch_add(1, Ordering::Relaxed);
    let started_at_ms = now_unix_ms();
    *slot = Some(RecHandle {
        gen,
        file_name: name.clone(),
        started_at_ms,
        stop_tx,
        done: done_rx,
    });
    drop(slot);
    let b2 = b.clone();
    let fname = name.clone();
    let meta_dir = dir.to_path_buf();
    tokio::spawn(async move {
        let mut stop_rx = stop_rx;
        let mut bridge_stop = bridge_stop;
        let mut rx_sub = rx_sub;
        let mut tx_sub = tx_sub;
        let mut w = std::io::BufWriter::new(file);
        let t0 = Instant::now();
        let mut frames = 0u64;
        let mut bytes = 0u64;
        let mut tx_frames = 0u64;
        let mut flush = tokio::time::interval(FLUSH_EVERY);
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = stop_rx.changed() => break,      // record/stop
                _ = bridge_stop.changed() => break,  // 桥停/删/进程退出 (文件保留)
                _ = flush.tick() => {
                    let _ = w.flush(); // 定期落盘 (带缓冲写, 崩溃最多丢 500ms)
                }
                f = rx_sub.recv() => match f {
                    Ok(data) => {
                        write_line(&mut w, t0.elapsed().as_millis(), "rx", &data);
                        frames += 1;
                        bytes += data.len() as u64;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
                f = tx_sub.recv() => match f {
                    Ok(data) => {
                        write_line(&mut w, t0.elapsed().as_millis(), "tx", &data);
                        frames += 1;
                        bytes += data.len() as u64;
                        tx_frames += 1;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
            }
        }
        let _ = w.flush();
        // 侧车索引: 停止时把 txFrames 与 frames/bytes/durationSec 一起落盘
        // (<file>.meta.json), 列表免重扫; 写完才回报 RecSummary —— stop 返回即侧车就绪。
        // 进程硬退/任务意外死亡无侧车 → 列表回退逐行扫描 (txFrames=null)。
        let duration_sec = (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0;
        let meta_path = {
            let mut s = meta_dir.join(&fname).into_os_string();
            s.push(".meta.json");
            std::path::PathBuf::from(s)
        };
        let _ = std::fs::write(
            &meta_path,
            json!({
                "frames": frames,
                "bytes": bytes,
                "txFrames": tx_frames,
                "durationSec": duration_sec,
            })
            .to_string(),
        );
        let _ = done_tx.send(RecSummary { frames, bytes });
        crate::logging::write(
            "info",
            &format!(
                "桥 {} 录制结束: {fname} ({frames} 帧 / tx {tx_frames} / {bytes} 字节)",
                b2.id
            ),
        );
        // 自清理: 仅当槽位仍属本会话 (record/stop 已取走则不动)
        let mut slot = lock_mutex(&b2.rec);
        if slot.as_ref().map(|h| h.gen) == Some(gen) {
            *slot = None;
        }
    });
    crate::logging::write(
        "info",
        &format!("桥 {} 录制开始 → {name} (录像目录 {})", b.id, dir.display()),
    );
    Ok(name)
}

/// 停止录制并等待文件收尾 (flush 完成)。返回 (文件名, 帧数, 字节数)。
pub async fn stop(b: &Arc<Bridge>) -> Result<(String, u64, u64), String> {
    let h = lock_mutex(&b.rec)
        .take()
        .ok_or_else(|| "当前没有进行中的录制".to_string())?;
    let _ = h.stop_tx.send(true);
    let name = h.file_name;
    let done = h.done;
    // 有界等待收尾 (任务正常 <1ms); 任务意外死亡也返回 ok (文件已带缓冲落盘)
    let sum = match tokio::time::timeout(Duration::from_secs(2), done).await {
        Ok(Ok(s)) => s,
        _ => RecSummary {
            frames: 0,
            bytes: 0,
        },
    };
    Ok((name, sum.frames, sum.bytes))
}

/// 录制状态投影 (detail_json.recording): null 或 {"file","startedAt"}。
pub fn recording_json(b: &Bridge) -> Value {
    let slot = lock_mutex(&b.rec);
    match slot.as_ref() {
        Some(h) => json!({"file": h.file_name, "startedAt": h.started_at_ms}),
        None => Value::Null,
    }
}

fn write_line<W: Write>(w: &mut W, ts_ms: u128, dir: &str, data: &[u8]) {
    let _ = writeln!(
        w,
        "{{\"ts\":{},\"dir\":\"{}\",\"hex\":\"{}\"}}",
        ts_ms,
        dir,
        hex_encode(data)
    );
}

// ============================================================ 回放

/// 回放请求 (POST /api/fleet/<id>/replay)。
pub struct ReplayReq {
    pub file: String,
    pub speed: Option<f64>,
    pub loop_play: bool,
}

/// 进行中的回放会话 (存于 Bridge.replay 槽位; 进度经 detail_json 可见)。
pub struct ReplayHandle {
    pub gen: u64,
    pub file_name: String,
    pub speed: f64,
    pub loop_play: bool,
    /// 已写回 TX 的帧数/字节数 (含 loop 重复)。
    pub frames: Arc<AtomicU64>,
    pub bytes: Arc<AtomicU64>,
    stop_tx: watch::Sender<bool>,
}

/// 开始回放: 校验 (串口 open / speed 范围 / 文件在录像目录内) → 按原始时序
/// 写回串口 TX。loop=true 循环到被停止; 桥停止/删除时自动结束。
pub fn start_replay(b: &Arc<Bridge>, dir: &Path, req: ReplayReq) -> Result<(), String> {
    if lock_mutex(&b.replay).is_some() {
        return Err("回放已在进行".into());
    }
    let speed = req.speed.unwrap_or(1.0);
    if !speed.is_finite() || !(SPEED_MIN..=SPEED_MAX).contains(&speed) {
        return Err(format!("speed 超出范围 ({SPEED_MIN}~{SPEED_MAX}): {speed}"));
    }
    if b.hub.phase() != Phase::Open {
        return Err("串口未打开: 回放前请先打开串口".into());
    }
    let path = resolve_file(dir, &req.file)?;
    // ADR-24⑥: 只回放 tx 行 (rx 行是设备说的话, 不回注串口)
    let frames = parse_recording(&path, Some("tx"))?;
    if frames.is_empty() {
        return Err("录像文件没有有效帧".into());
    }
    let (stop_tx, stop_rx) = watch::channel(false);
    let gen = GEN.fetch_add(1, Ordering::Relaxed);
    let handle = ReplayHandle {
        gen,
        file_name: path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string(),
        speed,
        loop_play: req.loop_play,
        frames: Arc::new(AtomicU64::new(0)),
        bytes: Arc::new(AtomicU64::new(0)),
        stop_tx,
    };
    {
        let mut slot = lock_mutex(&b.replay);
        if slot.is_some() {
            return Err("回放已在进行".into()); // 并发双保险
        }
        *slot = Some(handle);
    }
    let fh_frames = lock_mutex(&b.replay).as_ref().unwrap().frames.clone();
    let fh_bytes = lock_mutex(&b.replay).as_ref().unwrap().bytes.clone();
    let b2 = b.clone();
    let ffile = req.file.clone();
    let floop = req.loop_play;
    tokio::spawn(async move {
        let mut stop_rx = stop_rx;
        let mut bridge_stop = b2.stop_tx.subscribe();
        'outer: loop {
            let mut prev: Option<u64> = None;
            for (ts, data) in &frames {
                // 每帧前非阻塞看停止旗 —— 零间隔帧 (ts 连续/单帧 loop) 不会饿死调度
                if *stop_rx.borrow() || *bridge_stop.borrow() {
                    break 'outer;
                }
                if let Some(p) = prev {
                    if *ts > p {
                        // 行间原始时序 / 倍率; sleep 可被停止/桥停打断。
                        // 首帧不等待 (prev=None): 绝对 epoch ms 录像首行 ts 巨大,
                        // 按它睡会永久卡死 (QA 计划 §3 A1 兼容)。
                        let delay = ((*ts - p) as f64 / speed).max(0.0).round() as u64;
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_millis(delay)) => {}
                            _ = stop_rx.changed() => break 'outer,
                            _ = bridge_stop.changed() => break 'outer,
                        }
                    }
                }
                prev = Some(*ts);
                // 走现有 tx 队列 (串口关闭时 send_to_port 返回 false 帧即弃,
                // 不中断回放 —— 自动重连恢复后继续)
                if b2.ctx.send_to_port(data) {
                    fh_frames.fetch_add(1, Ordering::Relaxed);
                    fh_bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
                }
                // 零间隔连续帧时让出调度 (绝不自旋占死 worker 线程)
                tokio::task::yield_now().await;
            }
            if !req.loop_play {
                break;
            }
        }
        let mut slot = lock_mutex(&b2.replay);
        if slot.as_ref().map(|h| h.gen) == Some(gen) {
            *slot = None;
        }
    });
    crate::logging::write(
        "info",
        &format!(
            "桥 {} 回放开始 → {ffile} (speed={speed}, loop={floop}, 仅 tx 行)",
            b.id
        ),
    );
    Ok(())
}

/// 停止回放 (幂等): true = 确有进行中的回放被停。
pub fn stop_replay(b: &Arc<Bridge>) -> bool {
    let h = lock_mutex(&b.replay).take();
    match h {
        Some(h) => {
            let _ = h.stop_tx.send(true);
            crate::logging::write("info", &format!("桥 {} 回放停止 ({})", b.id, h.file_name));
            true
        }
        None => false,
    }
}

/// 回放状态投影 (detail_json.replay): null 或进行中的会话摘要 (进度可见)。
pub fn replay_json(b: &Bridge) -> Value {
    let slot = lock_mutex(&b.replay);
    match slot.as_ref() {
        Some(h) => json!({
            "file": h.file_name,
            "speed": h.speed,
            "loop": h.loop_play,
            "frames": h.frames.load(Ordering::Relaxed),
            "bytes": h.bytes.load(Ordering::Relaxed),
        }),
        None => Value::Null,
    }
}

/// 解析录像文件 → [(ts_ms, bytes)]; 跳过坏行 (崩溃截断的尾行容错)。
/// dir_filter = Some("tx") 时只保留 tx 行 (ADR-24⑥ 回放口径); None 全保留。
fn parse_recording(path: &Path, dir_filter: Option<&str>) -> Result<Vec<(u64, Vec<u8>)>, String> {
    let body = std::fs::read_to_string(path).map_err(|e| format!("读取录像失败: {e}"))?;
    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let dir = v["dir"].as_str();
        let (Some(ts), Some(hex)) = (v["ts"].as_u64(), v["hex"].as_str()) else {
            continue;
        };
        if !matches!(dir, Some("rx") | Some("tx")) {
            continue;
        }
        if let Some(want) = dir_filter {
            if dir != Some(want) {
                continue;
            }
        }
        let Some(data) = hex_decode(hex) else {
            continue;
        };
        if data.is_empty() {
            continue;
        }
        out.push((ts, data));
    }
    Ok(out)
}

// ============================================================ 录像列表

/// 扫描本桥的录像文件 (文件名前缀 "<桥id>-") → 列表项
/// [{"file","frames","bytes","txFrames","startedAt","durationSec"}], 新的在前。
/// - 有侧车 (`<file>.meta.json`, 录制停止时落盘): frames/bytes/txFrames/durationSec
///   直接读侧车, 免逐行重扫 (txFrames = 回放可见性, 前端以 0 tx 帧警告无输出);
/// - 无侧车 (旧录像 / 录制中文件增长 / 进程硬退): frames/bytes/duration 逐行扫描
///   现算 (录制中轮询即实时帧数, Q3 口径不变), **txFrames = null** (前端容错);
/// - startedAt = 文件 mtime - 时长 (有侧车用 durationSec, 无侧车用末行 ts)。
pub fn list(b: &Bridge, dir: &Path) -> Vec<Value> {
    let prefix = format!("{}-", b.id);
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let path = e.path();
        if !path.is_file() {
            continue;
        }
        let Ok(name) = e.file_name().into_string() else {
            continue;
        };
        if !(name.starts_with(&prefix) && name.ends_with(".jsonl")) {
            continue;
        }
        let mtime_ms = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let meta_path = {
            let mut s = path.clone().into_os_string();
            s.push(".meta.json");
            PathBuf::from(s)
        };
        // (frames, bytes, txFrames, durationSec, startedAt)
        let (frames, bytes, tx_frames, duration_sec, started_at) = match read_sidecar(&meta_path) {
            Some((f, by, tx, d)) => {
                let started_at = mtime_ms.saturating_sub((d * 1000.0).round() as u64);
                (f, by, json!(tx), d, started_at)
            }
            None => {
                let (f, by, last_ts) = scan_file(&path);
                let started_at = mtime_ms.saturating_sub(last_ts);
                let d = (last_ts as f64 / 1000.0 * 10.0).round() / 10.0;
                (f, by, Value::Null, d, started_at)
            }
        };
        out.push(json!({
            "file": name,
            "frames": frames,
            "bytes": bytes,
            "txFrames": tx_frames,
            "startedAt": started_at,
            "durationSec": duration_sec,
        }));
    }
    out.sort_by_key(|v| -(v["startedAt"].as_u64().unwrap_or(0) as i64));
    out
}

/// 读侧车 `<file>.meta.json` → (frames, bytes, txFrames, durationSec);
/// 缺失或任一字段损坏 → None (列表回退逐行扫描, txFrames=null)。
fn read_sidecar(meta_path: &Path) -> Option<(u64, u64, u64, f64)> {
    let body = std::fs::read_to_string(meta_path).ok()?;
    let v: Value = serde_json::from_str(&body).ok()?;
    Some((
        v["frames"].as_u64()?,
        v["bytes"].as_u64()?,
        v["txFrames"].as_u64()?,
        v["durationSec"].as_f64()?,
    ))
}

/// 逐行扫描: (帧数, 数据字节合计, 末行 ts)。
fn scan_file(path: &Path) -> (u64, u64, u64) {
    let Ok(body) = std::fs::read_to_string(path) else {
        return (0, 0, 0);
    };
    let mut frames = 0u64;
    let mut bytes = 0u64;
    let mut last = 0u64;
    for line in body.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        frames += 1;
        bytes += v["hex"].as_str().map(|h| h.len() as u64 / 2).unwrap_or(0);
        last = last.max(v["ts"].as_u64().unwrap_or(0));
    }
    (frames, bytes, last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_utc_shapes_and_epoch() {
        assert_eq!(stamp_utc(0), "19700101-000000");
        // 2026-09-14 08:09:10 UTC = 1789373350
        assert_eq!(stamp_utc(1_789_373_350_000), "20260914-080910");
        assert_eq!(stamp_utc(951_827_696_789), "20000229-123456"); // 闰日
    }

    #[test]
    fn hex_roundtrip_and_rejects() {
        assert_eq!(hex_encode(&[0xde, 0xad, 0x01]), "dead01");
        assert_eq!(hex_decode("dead01").unwrap(), vec![0xde, 0xad, 0x01]);
        assert_eq!(hex_decode("DEAD01").unwrap(), vec![0xde, 0xad, 0x01]);
        assert!(hex_decode("abc").is_none()); // 奇数长度
        assert!(hex_decode("zz").is_none()); // 非 hex
    }

    #[test]
    fn resolve_blocks_traversal_and_accepts_normal() {
        let d = std::env::temp_dir().join(format!("sh_rec_guard_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("b1-20260914-000000.jsonl"), "").unwrap();
        // 正常命中 (返回绝对路径)
        assert!(resolve_file(&d, "b1-20260914-000000.jsonl").is_ok());
        // 穿越 / 分隔符 / 非法字符 / 不存在
        for bad in [
            "../Cargo.toml",
            "..\\Cargo.toml",
            "a/b.jsonl",
            "a\\b.jsonl",
            "..jsonl",
            "b1.jsonl.txt",
            "",
            "missing.jsonl",
            "带中文.jsonl",
            ".jsonl",
        ] {
            assert!(resolve_file(&d, bad).is_err(), "\"{bad}\" 应被拒绝");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn parse_recording_skips_bad_lines() {
        let d = std::env::temp_dir().join(format!("sh_rec_parse_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("x.jsonl");
        std::fs::write(
            &p,
            "{\"ts\":0,\"dir\":\"tx\",\"hex\":\"6162\"}\n\
             {truncated\n\
             {\"ts\":150,\"dir\":\"rx\",\"hex\":\"ff00\"}\n\
             {\"ts\":200,\"dir\":\"weird\",\"hex\":\"01\"}\n\
             {\"ts\":250,\"dir\":\"tx\",\"hex\":\"zz\"}\n\
             {\"ts\":300,\"dir\":\"tx\",\"hex\":\"\"}\n",
        )
        .unwrap();
        let frames = parse_recording(&p, None).unwrap();
        assert_eq!(frames.len(), 2, "坏行/空帧跳过: {frames:?}");
        assert_eq!(frames[0], (0, vec![0x61, 0x62]));
        assert_eq!(frames[1], (150, vec![0xff, 0x00]));
        // ADR-24⑥: 回放只取 tx 行 (rx 过滤)
        let tx_only = parse_recording(&p, Some("tx")).unwrap();
        assert_eq!(tx_only, vec![(0, vec![0x61, 0x62])], "{tx_only:?}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
