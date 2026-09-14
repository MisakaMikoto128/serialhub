//! 运行日志文件 (FR-21 / ADR-24③, `--log-file <path>`)。
//!
//! 记录: 启动 / 桥状态机迁移 / 旁路转发连接事件 / 录制回放事件 / 错误。
//! 滚动: 当前文件 ≥5MB 时 → `path`→`path.1`→`path.2` 顺移 (共 3 份, 最旧删除),
//! 新当前文件从头写。默认**不开启** (未 init 时 write 为无操作, 零开销,
//! 向后兼容); GUI 与 headless 同样生效 (main 在分派前 init)。
//!
//! 刻意不引日志框架依赖: 单文件 + 手动滚动 ~80 行, 与本仓"零重量级依赖"口径一致。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 单文件滚动阈值 (FR-21: 5MB)。
pub const ROTATE_BYTES: u64 = 5 * 1024 * 1024;
/// 保留文件总数 (当前 + 2 份历史 = 3 份)。
pub const KEEP_FILES: usize = 3;

struct Inner {
    path: PathBuf,
    /// None = 滚动中已关闭 (Windows 上开着句柄 rename 会失败, 先关再移)。
    file: Option<File>,
    written: u64,
}

static LOGGER: OnceLock<Mutex<Option<Inner>>> = OnceLock::new();

/// 启用文件日志 (main 在分派 GUI/headless 前调用一次)。重复调用忽略。
/// 打不开文件 → 返回 Err (CLI 报错退出, 不静默吞配置错误)。
pub fn init(path: &Path) -> Result<(), String> {
    if let Some(p) = path.parent() {
        if !p.as_os_str().is_empty() {
            std::fs::create_dir_all(p).map_err(|e| format!("创建日志目录失败: {e}"))?;
        }
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("打开日志文件失败 ({path:?}): {e}"))?;
    let written = file.metadata().map(|m| m.len()).unwrap_or(0);
    let cell = LOGGER.get_or_init(|| Mutex::new(None));
    let mut g = cell.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_some() {
        return Ok(()); // 已启用 (首建者赢)
    }
    *g = Some(Inner {
        path: path.to_path_buf(),
        file: Some(file),
        written,
    });
    Ok(())
}

/// 写一行日志 (未启用 = 无操作)。行格式: `[<iso-8601 UTC 毫秒>] [<level>] <msg>`。
pub fn write(level: &str, msg: &str) {
    let line = format!("[{}] [{}] {}\n", timestamp_iso(), level, msg);
    let Some(cell) = LOGGER.get() else { return };
    let Ok(mut g) = cell.lock() else { return };
    if let Some(inner) = g.as_mut() {
        write_line(inner, &line, ROTATE_BYTES);
    }
}

fn write_line(inner: &mut Inner, line: &str, rotate_at: u64) {
    if inner.written.saturating_add(line.len() as u64) > rotate_at {
        rotate(inner);
    }
    if let Some(f) = inner.file.as_mut() {
        if f.write_all(line.as_bytes()).is_ok() {
            inner.written += line.len() as u64;
        }
    }
}

/// 滚动: 关当前句柄 (Windows 开句柄 rename 会失败) → 删最旧 → 历史顺移 →
/// 当前让位 → 新建空当前文件。
fn rotate(inner: &mut Inner) {
    inner.file = None; // 关闭句柄
    for i in (1..KEEP_FILES - 1).rev() {
        let from = sibling(&inner.path, i);
        let to = sibling(&inner.path, i + 1);
        if from.exists() {
            let _ = std::fs::rename(&from, &to);
        }
    }
    let _ = std::fs::rename(&inner.path, sibling(&inner.path, 1));
    inner.file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&inner.path)
        .ok();
    inner.written = 0;
}

fn sibling(path: &Path, n: usize) -> PathBuf {
    let mut os: std::ffi::OsString = path.as_os_str().to_owned();
    os.push(format!(".{n}"));
    PathBuf::from(os)
}

/// UTC 时间戳 "yyyy-mm-ddTHH:MM:SS.mmmZ" (毫秒精度; 天数→日期用
/// Howard Hinnant civil_from_days 公历算法, record::stamp_utc 同源)。
fn timestamp_iso() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    let ms = d.subsec_millis();
    let z = (secs / 86_400) as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    let rem = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_iso_shape() {
        let t = timestamp_iso();
        // 形如 2026-09-15T08:09:10.123Z
        let b = t.as_bytes();
        assert_eq!(t.len(), 24, "{t}");
        assert_eq!(b[4], b'-');
        assert_eq!(b[7], b'-');
        assert_eq!(b[10], b'T');
        assert_eq!(b[13], b':');
        assert_eq!(b[19], b'.');
        assert!(t.ends_with('Z'));
    }

    #[test]
    fn rotation_keeps_three_files() {
        let dir = std::env::temp_dir().join(format!("sh_log_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("run.log");
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        let mut inner = Inner {
            path: path.clone(),
            file: Some(file),
            written: 0,
        };
        let limit: u64 = 100; // 测试用小阈值
        let line = "x".repeat(40) + "\n"; // 41 字节
        for _ in 0..5 {
            write_line(&mut inner, &line, limit);
        }
        // 41×2>100 → 第 3 行触发滚动一次, 第 5 行再滚一次 → .1 .2 都该在
        assert!(sibling(&path, 1).is_file(), "path.1 应存在");
        assert!(sibling(&path, 2).is_file(), "path.2 应存在");
        assert!(path.is_file());
        let cur = std::fs::metadata(&path).unwrap().len();
        assert!(cur > 0 && cur <= limit, "当前文件应重新计数: {cur}");
        // 顺移语义: .2 是最旧的 (先写的那批)
        let old = std::fs::read_to_string(sibling(&path, 2)).unwrap();
        assert_eq!(old.lines().count(), 2, "最旧一份含滚出前的行: {old:?}");
        // 不超 3 份: 手动触发第三次滚动后 .3 不存在
        for _ in 0..3 {
            write_line(&mut inner, &line, limit);
        }
        assert!(!sibling(&path, 3).exists(), "最多保留 3 份");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_noop_before_init() {
        // LOGGER 未 init (或仅其他测试 init 过) —— write 绝不 panic
        write("info", "无文件日志时不落盘不报错");
    }
}
