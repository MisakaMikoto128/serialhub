# QA 报告 — Sprint 2 修复轮复验 (回路⑥, FIX-9~FIX-15)

日期: 2026-09-12 · 角色: QA · 被测: 重建产物 (ui/index.html 19:13 重嵌入, cargo build 0 警告, 5.6s)
方法: 黑盒 —— cargo/pytest 回归 + 进程级核对 (子进程退出码/stderr/Win32 窗口与对话框枚举/UIA 托盘 Name/Playwright 页内按钮) + 文档 grep
结论: **回归 24/24 零回归; FIX-9/11/13/14/15 PASS; FIX-12 FAIL (UIA 托盘 Name 拼接仍在, 证据确凿)**

---

## 1. 回归 (零回归确认)

| 项 | 结果 | 证据 |
|---|---|---|
| Rust 单元测试 | PASS | `cargo test`: **24 passed / 0 failed** (+2 shutdown 用例: `shutdown_api_closes_gracefully_with_open_ws`、`shutdown_watch_can_be_sent_repeatedly`, 与 Dev 报告一致) |
| 一致性套件 | **PASS** | `python -m pytest tests/ -v`: **24 passed / 0 failed** (39.9s, conftest 带 `--headless`) —— 数据面/ADR-5 契约零回归 |
| 进程卫生 | PASS | 全程结束 `tasklist` 无 serialhub.exe; **COM8 未碰** |

## 2. FIX 逐条核对

### FIX-9 页内「退出程序」+ POST /api/shutdown — **PASS**

| 子项 | 证据 |
|---|---|
| ① headless + WS 长连接 (关键回归点) | `--headless --port COM2` (8081) → websockets 客户端保持连接 (status `clients=1`) → `POST /api/shutdown` → **HTTP 200 `{"ok":true}`** (ADR-5 ③ 形状) → 进程 **328ms 自退, exit_code=0** (≤5s ✓); 服务端**主动断开** WS (客户端收到 `ConnectionClosedError`, 旧实现被长连接卡死的路径已修); 进程退出后 **COM2 立即可重开** ✓ |
| ② GUI 页内按钮端到端 (Playwright, 8084 隔离) | 页头「退出程序」按钮存在 (`#btnQuit`); 第一击 → 按钮武装 (**armed="1", 文案"确认退出?"**), 进程存活; **3s 无操作自动回退** (armed="0", 文案复原, 防误触 ✓); 双击 (150ms 间隔) → 文案"正在退出…" + 网络日志捕获**恰 1 次 POST /api/shutdown** → 进程 **553ms 自退, exit_code=0** → COM1 立即可重开 ✓ |

观察 (非阻塞): 第一次复验跑在 8080 时按钮 POST 后退出 >5s —— 8084 隔离后 553ms。netstat 取证显示 8080 上存在**外部进程 ZCode.exe (本机宿主应用, PID 39564) 持有的连接**, 疑其拖慢 axum 优雅停机; 非应用缺陷, 但说明对外暴露端口上第三方长连接可延长停机耗时, 交 Dev 知悉。

### FIX-11 GUI clients 恒 2 — **PASS**

- GUI 实例起在 `127.0.0.1:8084 --port COM1` (隔离外部干扰, 见下): clients 序列 **t+0 到 t+90s 共 10 次采样全部 = 1**, 无增长 (WebView 控制台恰占 1 席, 无第二条并存 WS)。
- 定界证据: 同期起在 8080 的 GUI 实例 clients=2 (uptime 3s→97s 稳定), `Get-NetTCPConnection`/netstat 取证: 第二条连接属 **ZCode.exe (PID 39564, 本机宿主应用) 的外部连接** —— 环境干扰而非页面双连接; 8084 隔离后即 1, 证明页面 WS 状态机修复生效。

### FIX-12 托盘 tooltip UIA Name 拼接 — **FAIL**

- 手法: GUI 实例 (8084) 运行中, 每次以**全新 PowerShell UIAutomation 客户端**枚举桌面 Button 并过滤 Name 含 "SerialHub" (排除客户端缓存因素), HTTP 驱动 close/open 切换相位。
- 实测托盘图标元素 Name:
  - open 相位 (启动后): **`[SerialHub · COM1 · closed SerialHub · COM1 · open]`** ← 两段拼接
  - close 后: `[SerialHub · COM1 · closed]` (单段, 干净)
  - 再 open 后: **`[SerialHub · COM1 · closed SerialHub · COM1 · open]`** ← 拼接再现
- 判定: 按工单验收口径 "相位切换后 UIA 读托盘图标 Name 不再拼接两段" —— **拼接仍在 (open 相位必现)**。`set_tooltip(None)→Some` 未刷新 shell 暴露给 UIA 的 Name (每次读取均为全新 UIA 客户端, 非读端缓存)。对照事实: close 相位单段干净 → 拼接特定于 open 转换路径。供 Dev: 疑点在 NIM_MODIFY 对 UIA Name 的刷新路径 (可能需删除/重建图标 NIM_DELETE+ADD, 或检查 muda 0.25 set_tooltip 的 tooltip 更新 vs UIA Name 来源差异)。
- 附注: 同次枚举中的 `[关闭 SerialHub 串口↔WebSocket桥接控制台]` 等为 Windows 任务栏项的常规可访问性命名, 非本缺陷。

### FIX-13 托盘圆点描边 — **PASS (取证级)**

- 任务要求截图+文档取证: 全屏截图已存 `docs/team/reports/qa-sprint2-fix/tray-taskbar-evidence.png`; 圆点描边观感判定留 UX (Dev 同样声明目视留 UX)。进程级无异常。

### FIX-14 bind 失败 MessageBox — **PASS**

- 占住 8081 起 GUI 实例: **~1s 内出现错误对话框**, Win32 类 `#32770`, 标题 **"SerialHub 启动失败"**, 对话框文本 = `端口被占用: 无法监听 127.0.0.1:8081: ... (os error 10048)` (完整); 主窗口 "SerialHub" **从未出现** (不抢焦点路径成立); WM_CLOSE 关闭对话框后进程 **exit=1**; stderr 输出保留 (`端口被占用: ...` 在日志)。

### FIX-15 README「桌面形态」文档 — **PASS (grep 取证)**

- README 含全部关键词: 「桌面形态」「托盘」「退出」「气泡」「--headless」「溢出」「关窗」 ✓ (任务要求的文档取证; 内容质量归 UX/文档审阅)。

## 3. 环境与清理

- Windows 11 · rustc 1.97.1 · Python 3.11.7 (pytest 9.0.3/pyserial 3.5/websockets 16.0) · Playwright Chromium (Node 全局) · UIA (PowerShell UIAutomationClient)
- 桥进程管理: 每项核对独立拉起/退出 (优先优雅路径, 超时才 taskkill), 全程结束 `tasklist` 确认 **0 残留**; COM1/COM2/8080-8084; **COM8 未碰**
- 8080 上发现外部进程 ZCode.exe (本机宿主) 持有连接 —— 对 FIX-9/11 的 8080 观测构成干扰, 已用 8084 隔离并以 netstat 取证

## 4. 结论摘要 (≤8 行)

1. 回归: cargo test **24/24** (+2 shutdown 用例), pytest **24/24** —— 数据面与契约零回归。
2. FIX-9 **PASS**: headless WS 保持下 /api/shutdown → 200 {"ok":true} → 328ms 自退 exit 0 → COM2 立即释放; GUI 页内按钮两段确认+3s 回退+553ms 自退 exit 0 → COM1 释放。
3. FIX-11 **PASS**: GUI clients 90s 内 10 次采样恒 1 (8080 的 clients=2 经 netstat 定界为 ZCode.exe 外部连接, 环境干扰)。
4. FIX-12 **FAIL**: 全新 UIA 客户端三次读取, open 相位托盘 Name 仍拼接两段 (`closed ... open`), close 相位单段干净 —— set_tooltip(None→Some) 未刷新 UIA Name, 需 Dev 二次修复。
5. FIX-13 **PASS (取证级)**: 全屏截图留档, 观感判定留 UX; FIX-14 **PASS**: #32770 对话框 "SerialHub 启动失败" + 完整占用文案 + 主窗口不出现 + 关框后 exit=1; FIX-15 **PASS**: README 桌面形态章节七关键词全中。
6. 8080 端口存在外部进程 (ZCode.exe) 连接干扰, 已隔离取证 —— 建议后续 GUI 观测统一避开 8080。
7. COM8 未碰, 0 残留。**Sprint2 修复轮: 6/7 通过, FIX-12 需二次修复。**

---

# 追加 — 二次修复点验 (回路⑧, 2026-09-12, FIX-12 二修 + 停机兜底)

方法: 重建产物 (src 20:07 重编译, 0 警告) → cargo test / pytest 回归 → ① FIX-12 二修: GUI 实例 (COM1, 8084 隔离) 多轮 open→closed→open 相位切换, **每轮全新 PowerShell UIAutomation 客户端进程**读托盘图标 Name (排除客户端缓存); ② 停机兜底: headless 实例 (COM2, 8085) + **不说话的 raw TCP 连接** (connect 后零字节) → POST /api/shutdown → 进程退出计时 + COM2 重开。台架注记: 初版扫描 0 命中为台架问题 (PS 5.1 按 ANSI 读 UTF-8 脚本致 `·` 字面量失效 + 重挂后 UIA 元素重建时序), 修正输出编码与重试后以诊断版扫描 (已证实可读) 取终版证据。

## 1. 回归 (本轮重跑)

`cargo test`: **26 passed / 0 failed** (+2 有界停机用例) —— shutdown 相关单测名字核对 (任务要求抽 2 条): `shutdown_is_bounded_with_hung_client` ✓、`shutdown_watch_triggers_graceful_sequence` ✓ (另有 `shutdown_api_closes_gracefully_with_open_ws`、`shutdown_watch_can_be_sent_repeatedly` 亦在)。`python -m pytest tests/ -v`: **24 passed / 0 failed** (40.7s) —— 零回归。

## 2. FIX-12 二修 (NIM_DELETE + NIM_ADD 重挂) — **FAIL**

5 次读取 / 2 轮完整切换, 每次均为全新 UIA 客户端进程:

| 相位 | UIA 读到的托盘图标 Name | 单段? |
|---|---|---|
| open (t+12s, 启动后) | `SerialHub · COM1 · closed SerialHub · COM1 · open` | **否 (2 段)** |
| closed (第 1 轮) | `SerialHub · COM1 · closed` **×2 条元素** | 各 1 段, 但**出现重复托盘条目** |
| open (第 1 轮) | `SerialHub · COM1 · closed SerialHub · COM1 · open` | **否 (2 段)** |
| closed (第 2 轮) | `SerialHub · COM1 · closed` ×2 | 各 1 段, 重复条目仍在 |
| open (第 2 轮) | `SerialHub · COM1 · closed SerialHub · COM1 · open` | **否 (2 段)** |

判定:
- **拼接未消除**: open 相位 3/3 次读取均为 "closed … open" 两段拼接, 与一修前完全一致 —— "ADD 为全新 shell 条目, Name = ADD 时 szTip" 的机制假设被实测否定 (NIM_DELETE+ADD 后 shell 暴露的 UIA Name 仍累积旧 tooltip)。
- **新迹象**: closed 相位 UIA 树中出现 **2 个托盘图标元素** (均显示 closed) —— 重挂可能产生重复条目 (open 相位合并回 1 个拼接元素)。此现象一修 (NIM_MODIFY) 时不存在 (恒 1 个元素)。
- 顺带确认: 多轮重挂期间进程无异常、/api/close、/api/open 与 /api/shutdown 全部正常 ({"ok":true}), 图标在 UIA 始终可达 (未消失), 未观察到崩溃/卡死; 托盘菜单与左键恢复为真实鼠标交互, 自动化不可达, 维持留 UX。
- 注册表侧证: `HKCU\Control Panel\NotifyIconSettings` 下 serialhub.exe 存在 **4 个条目** (release/debug × UID 1/2, InitialTooltip 各为不同相位的快照), 其中 release-UID2 条目 `IsPromoted=1` (即本轮可见可读的图标) —— 多条目现象与 UIA 双元素互相印证, 建议 Dev 从"Shell 按 hwnd+uid 视角的重挂身份"入手 (每次重挂后 Shell 侧似乎演化出新身份, 旧条目残留)。

## 3. 停机兜底 — **PASS**

| 项 | 证据 |
|---|---|
| 挂死连接 | headless (COM2, 8085) 起桥后, raw TCP `connect` 后零字节挂住 1s+ (模拟外部不说话连接) |
| POST /api/shutdown | HTTP 200 `{"ok":true}` (ADR-5 ③ 形状) |
| 进程退出 | **264ms 自退, exit_code=0** (≤2s 判据 ✓; Dev 自验 408ms 同量级) |
| 僵死连接处置 | 服务端主动断开 (客户端 recv 返回 EOF) |
| COM 释放 | **COM2 立即可被 pyserial 重开** ✓ |

## 4. 二次点验结论 (≤6 行)

1. 回归: cargo test 26/26 (含 +2 有界停机用例, 名字核对通过), pytest 24/24 —— 零回归。
2. 停机兜底 **PASS**: 不说话 raw TCP 挂底下 shutdown → 264ms 自退 exit 0 → 僵死连接被服务端断开 → COM2 立即重开 (1.5s 有界宽限机制生效)。
3. FIX-12 二修 **FAIL**: NIM_DELETE+ADD 重挂后, open 相位 UIA Name 仍稳定拼接两段 (3/3), 且 closed 相位出现 2 个托盘元素 (重复条目, 新迹象)。
4. 注册表 4 个 NotifyIconSettings 条目与 UIA 双元素互相印证, 建议从 Shell 侧图标身份/条目生命周期入手。
5. 托盘菜单/左键恢复/图标变色自动化不可达 (真实鼠标交互), 维持留 UX; 重挂期间数据面与 API 无异常。
6. COM8 未碰, 测毕 0 残留。**两项点验: 停机兜底通过; FIX-12 仍未达标, 需第三次修复。**

---

# 追加 — FIX-12 三修点验 (回路⑧终验, 2026-09-12)

三修路线: 托盘图标**启动时 NIM_ADD 一次**, szTip **静态** `"SerialHub · <端口> · 状态见控制台"`, 运行期相位仅 NIM_MODIFY 换 hIcon, 绝不 DELETE/重加/改 szTip (README 同步)。方法: 重建 (gui.rs 20:32 重编译, 0 警告) → 注册表条目基线 → GUI 实例 (COM1, 8084 隔离) → **两轮 close→open→close→open**, 每次相位切换后以全新 PowerShell UIAutomation 客户端读托盘 Name → 注册表复查 → /api/shutdown 收尾。

## 1. 回归抽检 (按工单要求)

- cargo test 抽 1 条: `headless_gui_flags` → **1 passed** (全套 26/26 由 Dev 已验, 不重复)。
- pytest 冒烟 3 条: `test_fr1_integrity` + `test_fr4_status_contract` + `test_fr3_reopen_state_machine` → **3 passed** (11.6s) —— 数据面/契约/状态机零回归。

## 2. FIX-12 三修核对 — **PASS**

托盘图标 Name 实测 (5 次读取, 每次均为全新 UIA 客户端进程):

| 读取时机 | UIA Name (托盘形态条目, 过滤后) | 单段静态? |
|---|---|---|
| 启动后基线 | `SerialHub · COM1 · 状态见控制台` | ✓ |
| 切换 1: close 后 (API phase=closed) | `SerialHub · COM1 · 状态见控制台` | ✓ |
| 切换 2: open 后 (API phase=open) | `SerialHub · COM1 · 状态见控制台` | ✓ |
| 切换 3: close 后 (API phase=closed) | `SerialHub · COM1 · 状态见控制台` | ✓ |
| 切换 4: open 后 (API phase=open) | `SerialHub · COM1 · 状态见控制台` | ✓ |

- **5/5 次读取恒为单段静态文案**, 零拼接、零变化、零重复元素 (每次读取托盘形态条目恰 1 个; 二修时的双元素/拼接形态完全消失)。
- **注册表**: `NotifyIconSettings` 下 serialhub.exe 条目切换前 **4** → 切换后 **4** (release 2 + debug 2, 均为历史遗留), **无新增** ✓; 三修单次 ADD 未制造新条目。
- 相位切换 API 全部 200 `{"ok":true}`, /api/status 相位同步正确 (closed/open 交替); 收尾 /api/shutdown → **exit 0**, COM1 立即释放。
- 台架注记: ① 判定以"托盘形态条目" (`SerialHub ·` 前缀) 过滤为准 —— 脚本初版用全 Serial 列表直比较产生假 false, 已用同批数据复核修正 (每轮恰 1 条托盘形态且等于期望文案); ② 首扫即读到静态文案, 图标可见 (历史条目 IsPromoted=1 沿用); 脚本误触发的 promote+重启引导分支发生在任何相位切换之前, 不影响三修判定数据; ③ 图标颜色随相位变化的观感判定留 UX (进程级仅见 NIM_MODIFY 无错、UIA 无消失)。

## 5. 三修点验结论 (≤5 行)

1. UIA Name 5/5 恒为静态单段 `SerialHub · COM1 · 状态见控制台`, 两轮 close/open 切换零拼接零变化 —— 拼接缺陷根治。
2. 注册表 NotifyIconSettings serialhub 条目 4→4 无新增 (历史遗留 4 条不计, 与工单口径一致)。
3. 回归抽检: cargo 1/1 + pytest 冒烟 3/3 —— 零回归。
4. 收尾 /api/shutdown exit 0、COM1 立即释放; COM8 未碰, 0 残留。
5. **FIX-12 三修点验 PASS, Sprint2 修复轮至此全部收口。**
