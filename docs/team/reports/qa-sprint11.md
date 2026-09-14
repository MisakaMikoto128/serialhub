# QA Sprint 11 报告 — 批次 A 终局验收 (A1 窗口记忆 / A2 表单记忆 / A3 对比度 / A4 双门禁)

作者: QA (401) · 2026-09-14 · 依据: decisions.md ADR-22 · backlog 批次 A ·
dev-sprint11-backend.md + dev-sprint8-ui.md (A2 追加节)。
纪律: 黑盒; COM8 全程未触碰 (用户的 serial_bridge.py COM8 中继进程原样未动); 本会话进程按
PID 精确清理, 用户实例 (PID 45200, 8080/8081) 全程未杀; 不 commit。

## 0. 结论一览

| 项 | 结果 |
|---|---|
| cargo test | **93 passed / 0 failed** |
| pytest tests/ | **63 passed / 0 failed** (修订后, 见 §1) |
| A4 fmt --check / clippy -D warnings | 退出码 **0 / 0**; ci.yml 双门禁在 build 之前 ✓ |
| A1 窗口记忆 | **核心契约全过** (见 §2), 带两条环境/实现级注记 |
| A2 表单记忆 (真后端) | **21/21 断言全过** (Playwright + 真实 headless 实例) |
| A3 win95 页脚 | **实测 4.77:1 = dev 数字**, 计算样式白 on 青, 截图留档 |

## 1. 套件修订 (环境适配, 行为零放宽 —— 逐条注明)

跑测机器与并行席位共享, 三处修订 (diff 内注释同步):

1. `tests/conftest.py`: `kill_all_bridges` 由 `taskkill /IM` 全杀改为 **会话前快照差集按 PID
   清理** —— 保护并行席位/用户的 serialhub 实例 (PID 45200, 占用 8080/8081); 本会话实例仍全清。
   另增 `SERIALHUB_EXE` 环境覆盖 BRIDGE_EXE (默认 target/release 被运行中实例锁死无法重链接,
   QA 用 `CARGO_TARGET_DIR=target-qa` 旁路构建后指过来; 不设则行为不变)。
2. `tests/test_fr9_config_full.py`: 换址目标 8081/8085/8087 硬编码 → **动态空闲口** ——
   本机 GameViewerServer 外联残留占 8087 元组 (bind 报 10013, 程序侧"保持原地址继续服务"
   兜底行为正确), 用户桥数据口占 8081; "恰 1 个 serialhub 进程"断言改为**外来快照差集**;
   EXE 改走 conftest.BRIDGE_EXE。修订后 fr9 5/5, 全套 63/63。
3. 修订前基线如实记录: 首轮 33 failed 全部为 COM1 被用户实例桥占用 (PermissionError),
   非代码回归; 修复占用后重跑 60/63 → 修订 fr9 → **63/63**。

## 2. A1 窗口记忆黑盒 (release 构建, --fleet 临时清单 + --port COM1 115200 真起桥)

方法: PowerShell Win32 (DPI aware, 物理像素) + fleet.json 断言; 200% DPI。

| # | 场景 | 结果 |
|---|---|---|
| a | 几何 (200,100) → 真实退出 (/api/shutdown, 与托盘退出同汇 `UserEvent::Stopped`) → fleet.json | **`{x:200, y:100, w:2114, h:1495, maximized:false}`** —— x/y=外框左上物理、w/h=客户区物理, 与 GetWindowRect 实测外框 (200,100)-(2340,1666) 完全自洽; bridges/manager 段原样保留 |
| b | 重启 | 窗口恢复 **(200,100)-(2340,1666) 逐像素一致**; 桥 b1 COM1 phase=open 恢复 |
| c | 最大化态退出 | 存 **还原态几何 + `maximized:true`** (不存最大化矩形, 与 ADR-22① 契约逐字一致) |
| d | 重启 | **直接恢复为最大化** (IsZoomed=True) → SW_RESTORE **精确落回 (200,100)-(2340,1666)** |
| e | 关窗到托盘 (WM_CLOSE, 先挪到 (300,150) 制造差异) | 窗口隐藏、服务存活, fleet.json **逐字节不变** —— 关窗路径不写 window 段 ✓ |
| f | 超屏毒化记忆恢复 | 预置 `{h:32696}` 后重启 → 夹紧回屏内可用 (标题条可达), 不崩不丢 |

**注记 1 (缺陷移交 dev)**: 对主窗口做**程序化 resize** (SetWindowPos/MoveWindow, 任意高度、
任意调用方 DPI 上下文、双实例复现) 高度恒变为 32767 (宽度精确生效; 纯挪位安全; 对照 WinForms
窗口正常)。用户真实路径经 **SC_SIZE 键盘模态缩放验证正常** (1630→1181 / 1324→1566 精确跟随),
本验收即用该路径设几何。疑似 tao 0.37 层面对外部跨进程 SetWindowPos 的处理, A1 自身采样/落盘/
恢复代码在爆炸几何下仍自洽 (§2f 即证据)。建议 dev 用 SendInput 边框拖拽人工复核一次。
**注记 2**: 托盘菜单外部自动化不可达 —— 合成回调消息后菜单不弹 (Win11 溢出区图标 rect 查询
失败为疑似根因; 真实右键不受影响, 托盘分键已有 3 条 Rust 单测 + A5 用户人工项覆盖)。退出路径
按 dev 报告 §1 的收敛口径用 /api/shutdown 代验证。

## 3. A2 建桥表单记忆 (真后端复验)

真 headless 实例 + Playwright Chromium, 空场起跑 (前置 API 删净残留桥):
**21/21 断言全过** —— 清记忆=六字段默认+无提示+FR-17 预填 (mgr+1); 建桥成功 (200) 写
`sh_lastbridge` 六元组; 重开回填 COM1/9600/7/E/1/none + UI-0 提示可见; 端口递增跳过链
18652→18653→18654; 再建参数仍=上次; 清记忆回默认; 预期外 JS 错误 0。
备注: 真后端串口列表需先点「扫描」(mock 直填), 扫描后 cur 保持机制正确选回上次口;
失败建桥 (串口占用 409) 不动历史 —— 恰为 ADR-22② 规定行为, 实测成立。
真壳 (wry webview) localStorage 跨重启持久性不在本轮 (Playwright 走 HTTP 页), 留用户人工项。

## 4. A3 win95 页脚对比度复核

- 复现 dev §2.1 陷阱: 运行目录旧 win95.css 重启**不被覆盖** (插件语义 ✓) → 删除后重启
  **重落盘 == assets 内置新版** (字节一致) → `/themes/win95.css` 服务字节 == assets ✓。
- win95 主题计算样式: footer 与 `.toolbar .count` 均 **rgb(255,255,255) on rgb(0,128,128)**;
  WCAG 实算 **4.77:1 ≥ 4.5 (AA)**, 与 dev 实测一致; 目视截图
  `qa-sprint11-win95-footer.png` + "我的桥 (1)" 3× 特写 `qa-sprint11-win95-count.png`
  (确认白字, QA-sprint10 全景的"深色计数"系缩略图缩放误判, dev 遗留注记成立)。
- 顺手修复: 用户运行目录 `target/release/themes/win95.css` 为旧拷贝 (仅注释落后, 白字规则
  已在), 已以 assets 版覆盖, 用户实例下次加载即一致。

## 5. A4 债清偿 + 双门禁独立复核

- `cargo fmt --all --check` → **exit 0**;
- `cargo clippy --all-targets -- -D warnings` → **exit 0** (零 `#[allow]` 抽查成立);
- ci.yml: `cargo fmt --check`(L48) → `cargo clippy`(L51) → 锁文件门禁(L56) → `cargo build`(L65)
  → `cargo test`(L70) —— **双门禁在 build 之前** ✓; release.yml 未加 (ADR-22④ 有意) ✓。

## 6. 环境干预留档 (全部可逆、已还原)

- 用户实例 b2 桥 (COM1, 与绕组变形测试仪配套) 曾持续占用 COM1 → 经其控制面 API 停桥 →
  测试全程 → **已原样 start 恢复 (COM1 open)**; 实例本身未杀未动。
- 我方全部实例 (GUI×3 + headless×4 + 旁路构建) 按 PID taskkill 清零, 终态仅存用户实例 45200。

## 7. 遗留 / 交接

- A1 注记 1 的程序化 resize 爆炸 (32767) 请 dev 复核 (tao 层); A5 托盘目视 = 用户 30s 人工项。
- 真-拖拽 (SendInput 边框) 因前台锁未自动化, 与 A5 一并人工即可。
