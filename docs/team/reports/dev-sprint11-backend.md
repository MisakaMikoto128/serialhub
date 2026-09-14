# Dev Sprint 11 后端报告 — 批次 A (A1 窗口记忆 / A3 win95 页脚对比度 / A4 债清偿 + 双门禁)

作者: 后端主程 (201) · 2026-09-14 · 依据: decisions.md ADR-22 / backlog 批次 A。
改动范围: `src/gui.rs` (+~140)、`src/fleet.rs` (+~150 含单测)、`src/themes.rs` (+17 单测)、
`assets/themes/win95.css` (仅注释实测数字 + 规则处注释, **选择器/令牌零变化**)、
`src/cli.rs` (-31)、`src/hub.rs` / `src/service.rs` / `build.rs` (clippy 修复)、
`.github/workflows/ci.yml` (双门禁上岗)。全仓 `cargo fmt` 清偿。
cargo test **86 → 93 全绿** (+7: fleet window 段 ×2、gui 夹紧 ×4、win95 页脚钉 ×1)。
真机自验: 独立 `--fleet/--themes-dir` 临时目录 + `--addr 127.0.0.1:18471`,
全程未占用 COM8 (测试 fleet 无串口, 兼容桥空口不打开); 测试实例按 PID 终止,
另一并行席位实例未受影响。不 commit。

## 1. A1 窗口记忆 (ADR-22①) —— fleet.json 顶层 "window" 段

**数据契约** (`src/fleet.rs::WindowRec`): `{x, y, w, h, maximized}`。x/y = 窗口
外框左上角 (物理像素), w/h = **客户区 (inner) 尺寸** (物理像素) —— 与恢复接口
`with_inner_size` 同口径, 避免标题栏高度在 save/restore 间逐次累积; maximized
单独记, x/y/w/h 恒存**还原态**几何 (最大化期间不采样, 用户取消最大化落回原位)。

**写路径** (只走 fleet 现有写入通道, 不开第二配置文件):

- `save_window(path, rec)`: 读整清单 → 只替换 window 段 → 经 `save_fleet`
  同一原子写 (tmp+rename) 落盘; bridges/[manager] 段原样保留。
- 写入时机 = **真实退出**: GUI 所有真实退出路径 (托盘退出 / 网页 /api/shutdown /
  Ctrl-C) 都汇入 `UserEvent::Stopped`, 在此落盘一次; 3s 停机兜底路径同样写。
  关窗到托盘 (FR-8, 桥继续跑) **不是退出, 不写**; headless 不写; `--no-fleet` 不写。
- 首次退出尚无 fleet.json → 建只含 window 段的最小清单; **清单存在但解析失败 →
  拒写** (宁丢一次窗口几何, 不覆盖用户文件)。
- `BridgeManager` 新增 `window_seen` (启动时读到的段值), `persist()` 原样写回 ——
  运行期桥变更不会抹掉 GUI 的窗口几何。

**读路径**: `run_gui` 在建窗前 `load_window(fleet_path)`; 旧清单无该段 / 文件
缺失 → 内置默认 (1120×780); **解析失败 → stderr 一行 + 按默认启动** (不覆盖坏文件)。

**夹紧** (`gui.rs::clamp_window_rect`, 纯函数 + 4 条单测): 与任一当前显示器交叠
≥ 120×60 物理 px (标题条够得着) → 原样保留 (多显示器布局不拉回主屏); 否则整体
夹进主屏 (w/h 超屏缩到屏内、下限对齐 640×480 最小尺寸, x/y 贴边)。拔显示器后
记忆点在屏外 → 拉回主屏, 不会丢在屏外。显示器信息取 `available_monitors()` /
`primary_monitor()` (拿不到则不越权, 原样返回)。

**真机踩坑与解法 (值得留档)**: 最大化时 Win32 的几何事件先带最大化矩形、
`WS_MAXIMIZE` 样式位后到 —— 事件当口查 `is_maximized()` 仍是 false, 即时采样
两次实测都把最大化矩形污染进还原态几何。解法 = **采样只进 pending, 静默 300ms
(`GEOM_DEBOUNCE`) 且届时窗口非最大化非最小化才落账**; 退出路径额外用掉未提交的
新鲜候选 (此刻状态已稳定, 过渡态候选天然被跳过)。`WaitUntil` 计时器在每次进
事件闭包时重新武装 (其他事件会把 Wait 复位), 不与 quitting 的 Poll 轮询冲突。

**单测 +7** 中 A1 占 6: `fleet_file_window_rec_roundtrip` (段往返 / 旧清单兼容 /
缺 maximized → false)、`save_window_updates_only_window_section` (只换 window 段,
bridges/manager 保留 / 建最小清单 / 坏清单拒写)、`clamp_*` ×4 (副屏保留 / 拔屏
拉回 / 超屏缩放 + 阈值边界 / 无显示器原样)。

**真机验证** (200% DPI, PowerShell Win32 GetWindowRect/SetWindowRect):
① 改窗口至 137,92 1000×640 → POST /api/shutdown → fleet.json 落
`{x:274, y:184, w:1974, h:1209}` (物理, 自洽) → 重启 → 实测窗口恢复 137,92
1000×640 **逐像素一致**; ② 还原态 200,150 900×600 → 最大化 → 退出 → 落
`{x:400, y:300, w:1774, h:1129, maximized:true}` (存还原态而非最大化矩形) →
重启 → 恢复为最大化 (IsZoomed=True) → 手动还原 → 精确落回 200,150 900×600。

## 2. A3 win95 页脚对比度 —— 实测定版: 已达标, 纠正注释误算

QA-sprint10 §5.2 提出 "页脚说明文字在青底上对比度偏低"。全量 WCAG 2.x 重测
(脚本算 SAR/相对亮度) 结论:

- **win95.css 现行规则 `footer,.toolbar .count{color:#fff}` 已达标**: 白 #ffffff
  对青 #008080 = **4.77:1 ≥ 4.5 (AA)**。且青底 (亮度 ≈0.170) 上的理论上限就是
  纯白的 4.77:1 —— 不改经典桌面青的前提下**无更高解**, 故不改色、不动 `--muted`
  (它在银面板上 6.7:1, 改亮反而砸银底对比)。
- **选择器与令牌零变化** (对比度不足的真凶见 §2.1); 变化只有两处注释:
  文件头对比度自检块整体**用实测数字重写校正** (旧注释多处误算: "白对青 5.1"
  实为 4.77、"银对青 3.3" 实为 2.62/3.10、tooltip "17.9" 实为 20.6、".sub 对
  藏蓝 8.2" 实为 10.4、银钮黑字 "10.4" 实为 11.5、经典蓝/绿对银 "5.6/3.3"
  实为 4.7/2.8、copyTip.err "4.4" 实为 7.2), 并在青底条款注明其余色禁躺青底
  (银 3.10 / #c0c0c0 2.62 / muted 2.17 / faint 1.32)。
- 新单测 `themes.rs::win95_footer_on_teal_pins_aa_contrast` 钉死规则与实测数字,
  任何把青底文字改回弱化令牌的改动会在 CI 红掉。

**2.1 给 QA 的重要备注**: `themes.rs::ensure_builtin` 对已落盘的主题文件**永不
覆盖** (插件语义 "文件为准")。若 QA 机器的 `themes/win95.css` 是 Sprint 10 波1
的旧拷贝, 则不含 footer/count 改白规则, 实测仍会看到 `--faint` 灰字对青
(1.32:1)。**复验前请删掉运行目录 `themes/win95.css` 让内置新版重新落盘**
(或对比文件与 `assets/themes/win95.css` 是否一致)。本报告截图为临时目录
`--themes-dir` 全新落盘后实拍: footer 与 .toolbar .count 计算样式均
rgb(255,255,255) on rgb(0,128,128), 见 `dev-sprint11-backend-win95-footer.png`
(页脚特写) 与 `dev-sprint11-backend-win95-panorama.png` (全景)。

## 3. A4 债清偿 + CI 双门禁

- **cargo fmt 全仓**: `cargo fmt --all` 执行, `cargo fmt --all --check` 本地绿
  (历史欠 ~1075 行 diff 一次清偿; 波及 src 全部 + build.rs, 纯格式无语义)。
- **clippy --all-targets 19 条全修, 零 `#[allow]`** (真实修复, `-- -D warnings`
  本地零告警):
  - bool_assert_comparison ×5 (hub.rs ×3 / service.rs ×1): 改 `assert!` / `assert!`;
  - field_reassign_with_default ×3 (hub.rs ×2 / service.rs ×1): 改结构体初始化
    (`..Default::default()`);
  - unused_mut ×2 (service.rs): 删 mut (ws1 / cmd_rx);
  - dead_code ×2: `cli.rs::Cli::startup()` 零调用方 → 整体删除 (含 cfg(test)
    imports, 留一行注释考古); `hub.rs::HubState::retries()` 仅监督测试用
    (生产读 /api/status 投影) → `#[cfg(test)]` 随测试编译; `fleet.rs::load_fleet`
    因 restore 改走 load_fleet_file 后同样转 `#[cfg(test)]`;
  - unnecessary_map_or ×3 (gui.rs / fleet.rs / build.rs): 改 `is_none_or` /
    `is_some_and`;
  - DoubleEndedIterator `.last()` → `.next_back()` (fleet.rs 测试);
  - redundant_closure (build.rs): `and_then(decode_png_bytes)`。
- **ci.yml**: 原注释位放开为正式步骤, build 之前依次
  `cargo fmt --all --check` + `cargo clippy --all-targets -- -D warnings`
  (dtolnay/rust-toolchain@stable 自带两组件, 无需额外安装); 文件头技术债注释
  同步更新 (指向本报告)。release.yml **有意不加** (打包不被风格卡, ADR-22④)。

## 4. 验证汇总

- `cargo fmt --all --check` 绿; `cargo clippy --all-targets -- -D warnings` 零告警
  (含 build.rs 构建脚本); `cargo test` **93 passed / 0 failed** (86 → 93, +7)。
- 真机 A1: 见 §1 末段, 两轮重启循环全部逐像素/逐字段一致。
- 真机 A3: 见 §2.1 截图 ×2; 浏览器实测 computed color 白 on 青。
- COM8 全程未占用 (测试实例 fleet 无串口配置, 兼容桥空口不自动打开);
  测试实例按 `netstat` 定位 PID 后 taskkill /PID 精确终止, 并行席位实例无恙。

## 5. 遗留 / 交接

- A2 (localStorage 建桥表单记忆) 归 UI 席; A5 托盘目视确认属用户 30s 人工项。
- P1 集成复验: pytest 套件在带虚拟串口机器重跑 (63 条, 不受本次影响;
  fleet.json 新增 window 段为可选字段, 旧断言按 skip-if-absent 语义天然兼容)。
- 顺手发现未修 (知情即可): win95 "我的桥 (n)" 计数在 QA-sprint10 全景截图里
  呈深色, 疑为当时落盘主题旧拷贝所致, 现行文件已钉白并单测守护。

## 6. BUG-1 修复 — 桥变更整份持久化可抹掉 [window] 段 (201, 2026-09-14)

用户现场: v1.7.0, %APPDATA%\SerialHub\fleet.json window 段消失, 桥 b2/b3/b4, 实例仍在跑
(8080/8081/COM1 全程未触碰, 复现与验证一律 tempdir)。

- **根因**: `BridgeManager::persist()` 的 [window] 来源只有启动快照 `window_seen`
  (仅 `restore_fleet` 启动时写一次)。凡"本进程启动时盘上还没有 [window] 段"的会话
  (旧版清单/升级首启, window_seen=None), GUI 真实退出把窗口几何写上盘之后, 该会话
  (或同处境的后续会话) 任何一次桥 CRUD/换址触发的整份重写都把 [window] 抹掉;
  且启动快照还可能比盘上现值旧 (会覆盖退出会话新写入的几何)。FleetFile 序列化
  结构本身有 window 字段 (round-trip 无缺), 缺的是 persist 的**读盘合并**。
- **修复** (src/fleet.rs persist): 整份重写前读盘合并 —— [window] 以盘上现值为主源
  (只有 GUI 退出经 save_window 改它), `window_seen` 降为盘上无文件/解析失败时的
  兜底; [manager] 维持内存现址为准 (控制面活地址), 新增盘值兜底
  (set_manager_addr 未登记时不再丢档, 同类风险同堵)。读盘失败静默走兜底,
  不阻塞"变更即存"。save_window 拒写坏档语义不变。
- **测试** (+3, 全 tempdir): `persist_keeps_window_section_written_after_startup`
  (修复前红: window 被抹成 None)、`persist_keeps_manager_section_when_addr_not_registered`
  (修复前红: manager 被抹)、`persist_prefers_fresh_window_on_disk_over_startup_snapshot`
  (修复前红: 快照 W1 覆盖盘上 W2)。
- **回归**: `cargo test` **96 passed / 0 failed** (93 旧 + 3 新); fmt --check /
  clippy -D warnings 双门禁绿。未 commit; ui/index.html 的未提交改动属并行席位, 未触碰。
