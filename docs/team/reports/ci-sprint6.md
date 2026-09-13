# CI / Release Sprint 6 报告 (角色 205: CI/发布工程师)

日期: 2026-09-13 · 状态: v1.2.0 · 交付物: `.github/workflows/ci.yml`、`.github/workflows/release.yml`

## 一、ci.yml (push(main) / PR 触发)

三平台矩阵 windows-latest / ubuntu-latest / macos-latest, 流程:
checkout → dtolnay/rust-toolchain@stable → Swatinem/rust-cache@v2 → (Linux: apt 系统依赖) → `cargo build --locked` → `cargo test --locked`。

- **Linux 系统依赖** (`pkg-config libudev-dev libgtk-3-dev libwebkit2gtk-4.1-dev libxdo-dev`):
  - `libudev-dev`: serialport 4 → libudev-sys 经 pkg-config (PLAT-4);
  - `libgtk-3-dev` / `libwebkit2gtk-4.1-dev`: tao / wry —— GUI 模块无 feature 门, headless 也要编译过;
  - `libxdo-dev`: tray-icon 默认 feature `muda-libxdo`, 链接期需要 libxdo.so。
  (tray-icon → libappindicator-sys 0.9 无 build.rs、运行时 dlopen, 构建期不需要 appindicator 系统包。)
- **pytest 不进 CI**: `tests/` 需要 COM1/COM2 串口对, CI 无串口硬件 (yml 顶部有注释说明), 由 QA 在带 ELTIMA 虚拟串口的机器执行。
- **fmt / clippy 门禁暂缓**: 当前仓库不过 (见下), 两步以注释形式保留在 yml 中, 清债后取消注释即启用。
- 单元测试无需串口: 监督测试用假打开器注入; `serial::list_ports_never_panics` 无串口机器安全。

## 二、release.yml (tag `v*` 触发)

同一三平台矩阵, `cargo build --release --locked` → 打包 → `softprops/action-gh-release@v2` 上传。`permissions: contents: write`。

| 平台 | 产物 | 内容 |
|---|---|---|
| Windows | `serialhub-<tag>-windows-x86_64.zip` | serialhub.exe + README.md + LICENSE + NOTICE |
| Linux | `serialhub-<tag>-linux-x86_64.tar.gz` | serialhub + README.md + LICENSE + NOTICE |
| macOS | `serialhub-<tag>-macos-aarch64-unsigned.tar.gz` | serialhub + README.md + LICENSE + NOTICE (未签名/未公证, 首次打开需绕过 Gatekeeper) |

- Windows 打包用 PowerShell `Compress-Archive` (runner 无 zip.exe); macos-latest 是 Apple Silicon, 产物名 aarch64。
- 发布方式: `git tag v1.2.1 && git push origin v1.2.1` 即触发。

## 三、本地预检结果 (Windows 本机, clippy 1.97.0)

| 检查 | 结果 | 明细 |
|---|---|---|
| `cargo test` | 通过 (已知 64/64) | 单元测试, 无需串口 |
| `cargo fmt --check` | **不过**: 64 处 diff / 9 个文件 | fleet.rs 22 · service.rs 11 · supervisor.rs 10 · gui.rs 7 · api.rs 6 · cli.rs 3 · config.rs 3 · serial.rs 1 · stats.rs 1 (多为 import 排序与长函数签名换行) |
| `cargo clippy --all-targets` | **不过**: 12 条警告 | `assert_eq!` 与 bool 字面量比较 ×4; `field_reassign_with_default` ×3; 不必要的 `mut` ×2; 死代码 (`startup` / `retries` 未使用) ×2; `map_or` 可简化 ×1 |

`clippy -- -D warnings` 会直接编译失败, `fmt --check` 退出码 1 —— 故 CI 首版只保留 build + test 门禁。按分工我只记录债务、**不改 src/**; 建议由 Dev 一轮 `cargo fmt` + 6 处 `--fix` 可自动项清掉后, 再放开 yml 中注释的两步。

## 四、风险与备注

- ubuntu-latest 已是 24.04, `libwebkit2gtk-4.1-dev` 可用; webkit2gtk-4.1 是 wry 0.57 的 pkg-config 目标 (非 4.0)。
- 工作流未在真实 runner 上验证过 (本地无法跑 Actions), 首次 push / tag 后需人工盯一次 run 日志。
- 遵守约束: 未 commit、未触碰 `.github/` 以外文件、未占用 COM8。
