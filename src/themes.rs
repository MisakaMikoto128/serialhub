//! FR-14 主题插件 (ADR-18②): themes/ 目录下每个 *.css = 一套主题 (只覆盖 :root 设计令牌)。
//!
//! - 内置四套 (light / dark / example-oreo / win95) 经 include_str! 编译进二进制,
//!   首次启动落盘到主题目录 (不存在则连目录一起创建) —— 用户可照 example-oreo
//!   的文件头注释自制主题, 放进同目录即为插件 (扫目录即见, 无需注册)。
//! - 静态服务带路径穿越防护: 只接受单段文件名 (无分隔符/无 `..`/白名单字符),
//!   再 canonicalize 复核仍落在主题目录内 (双重防线, 见 resolve)。
//! - 主题目录: exe 旁 themes/ (默认) 或 --themes-dir 指定 (由 cli/fleet 传入)。

use std::path::{Path, PathBuf};

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// 内置主题 (名, css 全文)。扫描列表里的 builtin 标记以此为准。
pub const BUILTIN: &[(&str, &str)] = &[
    ("light", include_str!("../assets/themes/light.css")),
    ("dark", include_str!("../assets/themes/dark.css")),
    (
        "example-oreo",
        include_str!("../assets/themes/example-oreo.css"),
    ),
    ("win95", include_str!("../assets/themes/win95.css")),
];

/// 主题文件名长度上限 (防滥用; 正常主题名远小于此)。
const MAX_NAME: usize = 64;

/// GET /api/themes 的列表项 (serde 字段名即契约: name / builtin)。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ThemeEntry {
    pub name: String,
    pub builtin: bool,
}

/// 默认主题目录: exe 旁 themes/ (exe 定位失败 → 相对路径 themes/)。
pub fn default_themes_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("themes")))
        .unwrap_or_else(|| PathBuf::from("themes"))
}

/// 首次启动落盘: 创建目录 + 写入尚不存在的内置主题 (已存在的文件不动 ——
/// 用户可能已改过, 插件语义是"文件为准", 覆盖会毁掉用户自制内容)。
pub fn ensure_builtin(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建主题目录失败: {e}"))?;
    for (name, css) in BUILTIN {
        let f = dir.join(format!("{name}.css"));
        if !f.exists() {
            std::fs::write(&f, css).map_err(|e| format!("写入内置主题 {name} 失败: {e}"))?;
        }
    }
    Ok(())
}

pub fn is_builtin(name: &str) -> bool {
    BUILTIN.iter().any(|(n, _)| *n == name)
}

/// 扫描主题目录: *.css 文件名去后缀 = 主题名, 字典序; 目录不存在 = 空列表。
pub fn scan(dir: &Path) -> Vec<ThemeEntry> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|f| {
            f.len() <= MAX_NAME
                && f.ends_with(".css")
                && valid_name_chars(f)
        })
        .map(|f| f[..f.len() - 4].to_string())
        .collect();
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|name| ThemeEntry {
            builtin: is_builtin(&name),
            name,
        })
        .collect()
}

/// 文件名白名单: ASCII 字母/数字/点/短横线/下划线 (排除分隔符、控制字符、
/// 全角/Unicode 混淆名)。
fn valid_name_chars(file: &str) -> bool {
    file.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// 静态服务: 命中 → 200 text/css; 未命中/穿越 → 404。
pub fn serve(dir: &Path, file: &str) -> Response {
    match resolve(dir, file) {
        Some(path) => match std::fs::read(&path) {
            Ok(bytes) => (
                [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                bytes,
            )
                .into_response(),
            Err(_) => not_found(),
        },
        None => not_found(),
    }
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        axum::Json(serde_json::json!({"ok": false, "error": "主题不存在"})),
    )
        .into_response()
}

/// 路径穿越防护 (FR-14, 双重防线):
/// ① 白名单字符 + 拒绝分隔符与 `..` + 必须 .css 结尾;
/// ② canonicalize 后复核仍在主题目录内 (防符号链接/8.3 短名等文件系统花样)。
pub fn resolve(dir: &Path, file: &str) -> Option<PathBuf> {
    if file.len() > MAX_NAME
        || !file.ends_with(".css")
        || file.contains('/')
        || file.contains('\\')
        || file.contains("..")
        || !valid_name_chars(file)
    {
        return None;
    }
    let p = dir.join(file);
    let cp = p.canonicalize().ok()?;
    let cd = dir.canonicalize().ok()?;
    if cp.starts_with(&cd) && cp.is_file() {
        Some(cp)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "sh_themes_{}_{}",
            std::process::id(),
            tag
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn builtin_contains_four_themes() {
        let names: Vec<_> = BUILTIN.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, vec!["light", "dark", "example-oreo", "win95"]);
        for (n, css) in BUILTIN {
            assert!(css.contains(":root"), "内置主题 {n} 必须只覆盖 :root 令牌");
        }
        // 深色令牌定版值 (ADR-18② 用户指定)
        assert!(BUILTIN[1].1.contains("--bg:#14171a"));
        assert!(BUILTIN[1].1.contains("--panel:#1d2126"));
        assert!(BUILTIN[1].1.contains("--ink:#e8eaed"));
        assert!(BUILTIN[1].1.contains("--border:#2c313a"));
        // 示例主题文件头 = 自制指南
        assert!(BUILTIN[2].1.contains("照这个文件自制主题"));
    }

    #[test]
    fn win95_theme_pins_retro_tokens() {
        let css = BUILTIN[3].1; // win95 (Sprint 10, 用户点名: 直角 + Win95 复古)
        // 直角是本主题灵魂
        assert!(css.contains("--radius:0"));
        assert!(css.contains("--ctl-radius:0"));
        // 经典 Win95 配色定版值
        assert!(css.contains("--bg:#008080"), "经典桌面青");
        assert!(css.contains("--panel:#c0c0c0"), "银灰面板");
        assert!(css.contains("--panel-2:#d4d0c8"));
        assert!(css.contains("--accent:#000080"), "标题栏海军蓝");
        assert!(css.contains("--accent-hover:#0000a8"));
        assert!(css.contains("--accent-ink:#ffffff"));
        assert!(css.contains("--ink:#000000"));
        // 徽章底一律回银 + 深色 ink
        for ink in ["--ok-ink:#004a00", "--warn-ink:#5c4400", "--err-ink:#7a1010"] {
            assert!(css.contains(ink), "{ink}");
        }
        // 四种徽章底一律回银
        for bg in [
            "--ok-bg:#d4d0c8",
            "--warn-bg:#d4d0c8",
            "--err-bg:#d4d0c8",
            "--gray-bg:#d4d0c8",
        ] {
            assert!(css.contains(bg), "{bg}");
        }
        // 终端黑底 + 独立前景令牌 (黑终端不能继承黑正文)
        assert!(css.contains("--term-bg:#000000"));
        assert!(css.contains("--term-ink:#e8e8e8"));
        // 文件头注释: 致敬语 + 对比度自检
        assert!(css.contains("配色致敬 Windows 95 与 98.css 项目"));
        assert!(css.contains("对比度自检"));
    }

    #[test]
    fn every_builtin_covers_the_full_token_set() {
        // 以 dark 为令牌基准集: 任何内置主题缺项 = 换肤后 UI 元素裸奔, 编译期挡下
        let tokens = |css: &str| -> Vec<String> {
            css.lines()
                .filter_map(|l| l.trim().split(':').next())
                .filter(|t| t.starts_with("--"))
                .map(|t| t.trim().to_string())
                .collect()
        };
        let base = tokens(BUILTIN[1].1); // dark
        assert!(base.len() >= 25, "dark 基准集应完整 ({:?})", base.len());
        for (n, css) in BUILTIN {
            let set = tokens(css);
            for t in &base {
                assert!(set.contains(t), "内置主题 {n} 缺令牌 {t}");
            }
        }
        // win95 附加 --term-ink (黑终端前景; UI 以 var(--term-ink,var(--ink)) 兜底)
        assert!(tokens(BUILTIN[3].1).iter().any(|t| t == "--term-ink"));
    }

    #[test]
    fn win95_ensure_roundtrip_preserves_user_edits() {
        let d = temp_dir("win95");
        ensure_builtin(&d).unwrap();
        let p = d.join("win95.css");
        // 落盘往返: 磁盘内容与编译进二进制的完全一致
        assert!(p.exists(), "win95.css 应落盘");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), BUILTIN[3].1);
        // 可被静态服务解析命中 (路径规则对下划线名友好)
        assert!(resolve(&d, "win95.css").is_some());
        // 幂等: 用户改动不被覆盖 (插件语义 = 文件为准)
        std::fs::write(&p, "/* user tuned win95 */").unwrap();
        ensure_builtin(&d).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "/* user tuned win95 */");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ensure_builtin_writes_files_once() {
        let d = temp_dir("ensure");
        ensure_builtin(&d).unwrap();
        for (name, css) in BUILTIN {
            let p = d.join(format!("{name}.css"));
            assert!(p.exists(), "内置主题 {name} 应落盘");
            assert_eq!(std::fs::read_to_string(&p).unwrap(), *css);
        }
        // 幂等: 已存在不覆盖 (用户改动保留)
        let p = d.join("light.css");
        std::fs::write(&p, "/* user edited */").unwrap();
        ensure_builtin(&d).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "/* user edited */");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn scan_lists_css_sorted_and_marks_builtin() {
        let d = temp_dir("scan");
        std::fs::write(d.join("zeta.css"), ":root{}").unwrap();
        std::fs::write(d.join("alpha.css"), ":root{}").unwrap();
        std::fs::write(d.join("notes.txt"), "not a theme").unwrap();
        std::fs::create_dir_all(d.join("fake.css")).unwrap(); // 目录不算 (is_file 过滤)
        ensure_builtin(&d).unwrap();
        let got = scan(&d);
        let names: Vec<_> = got.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["alpha", "dark", "example-oreo", "light", "win95", "zeta"],
            "字典序 + 只收 .css 文件"
        );
        let flag = |n: &str| got.iter().find(|t| t.name == n).unwrap().builtin;
        assert!(flag("light") && flag("dark") && flag("example-oreo") && flag("win95"));
        assert!(!flag("zeta") && !flag("alpha"), "非内置主题 builtin=false");
        // 目录不存在 = 空
        assert!(scan(&d.join("nope")).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn resolve_blocks_traversal_and_accepts_normal() {
        let d = temp_dir("guard");
        std::fs::write(d.join("ok.css"), ":root{}").unwrap();
        std::fs::write(d.join("plain.txt"), "secret").unwrap();
        // 正常命中
        assert!(resolve(&d, "ok.css").is_some());
        // 穿越 / 分隔符 / 非法字符 / 非 css
        for bad in [
            "../Cargo.toml.css",
            "..\\Cargo.toml.css",
            "..%2Fok.css",
            "a/b.css",
            "a\\b.css",
            "..css",
            "ok.txt",
            "ok.css.txt",
            "",
            ".css",
            "带中文.css",
            "ok .css",
        ] {
            assert!(resolve(&d, bad).is_none(), "\"{bad}\" 应被拒绝");
        }
        // canonicalize 复核: 目录外真实存在的文件 (同盘) 借符号链接难造,
        // 直接造一个目录外的 .css, 用 8.3 不可行 → 用绝对路径形式必含分隔符已挡;
        // 再验证"目录内不存在的文件"返回 None 即可 (canonicalize 失败)。
        assert!(resolve(&d, "missing.css").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
