# QA 报告 — 配置/持久化交叉矩阵 (测试二组 · qa-xtest-config) · 2026-09-15

作者: 测试二组·配置与持久化方向 (新实例) · 依据: spec FR-13/16/17/19/22 + ADR-22① ·
补 qa-sprint11 (ADR-22 批次 A) 与 qa-sprint13 (FR-19/22) 单项验收之间的**交叉盲区**。
纪律: 黑盒 (预期只来自 spec/ADR 交互, 未读 src/ 调预期); 全程未碰 COM8; 未 commit;
未改 src/ 与 ui/; 进程按 PID 清理; 产物 `tests/test_xtest_config.py` + `tests/pw_theme_persist.cjs`。

## 0. 结论一览

| # | 交叉项 | 结果 | 一句话证据 |
|---|---|---|---|
| 1 | 导入 × 运行态 (FR-22 × FR-10g) | **PASS** | A open → replace 导入含 B 清单: A 行消失 + 串口实测 +0.11s 释放; B 按清单启动 (open); merge 导回 A (open) 且 B 身份/配置/运行态分毫不动 |
| 2 | 导入 × 窗口记忆 (FR-22 × ADR-22①) | **PASS** | 种子 [window] 段在 purge/建桥/replace/merge 导入后逐字段原样保留 |
| 3 | 导入 × 录像文件 (FR-22 × FR-19) | **PASS** | A 录像落盘 → replace 删 A → recordings/ 文件逐字节原样保留 |
| 4 | 单实例 × fleet (FR-16 × FR-10/ADR-22①) | **PASS** | 第二实例 headless 同端口+同清单 → exit 0; A 存活; fleet.json 逐字节不变 |
| 5 | 主题持久化 × 重启 (FR-14) | **PASS** | 真 Chromium 走 UI 切 win95 → 重启浏览器进程 → localStorage 记忆 + win95.css 自动挂回 |

**收口轮: 5/5 passed (16.5s, 单轮全绿)**; 基线不变: cargo test 115/115, pytest 72/72
(本席号新增 5 例, 全仓 collect 82 = 72 + 本组 5 + 姊妹组 xtest-data 5)。

## 1. 过程环境事件 (如实留档, 均非产品违约)

- **并行席位**: UI 走查组活跃 (`output/ui-xtest/` + walkthrough.cjs), 其实例
  `--addr 127.0.0.1:18500 --fleet output/ui-xtest/fleet.json` (桥 COM1-bridge/COM2-bridge,
  autoOpen, 3s 重试), 由 node 父进程守护、被杀自动重启, 且**周期性重启其实例**。
  会话早期曾按任务书杀过其一次孤儿实例 (守护随即重启, 无损害), 此后认定活跃席位不再干预。
- **串口占用竞争**: 席位桥周期性抢占 COM1/COM2 → 初版 xt1 用 conftest
  `wait_serial_free` (0.2s 轮询) 断言「replace 后串口释放」两次误判 FAIL。探针
  (`build/qa-f2/probe_xt*.py`) 50ms 高频监视实测 ground truth: **释放发生在导入返回后
  ≈0.11s**, 随即被席位 3s 重试夺回 —— 测量伪影非违约 (沿 qa-sprint13-plan §6.3 R4 纪律),
  已改为 `_watch_serial_released` (先起 watcher 再导入, 单次 free 采样即判; A 此刻必然
  持有该口, free ⇒ 释放已发生)。open 态断言遇抢占在用例尾部降级
  [BLOCKED-BY-ENVIRONMENT], 其余断言先行。
- **「桥进程提前退出 (code=1)」两次**: 均发生于席位走查运行期间。exit 1 = `taskkill /F`
  签名 (无 WER 崩溃记录, 日志止于「兼容桥 b1 → 数据端口」行) —— 席位工具链的
  `taskkill /IM` 全杀清场 (qa-sprint13 §12.4 已记录该隐患) 误伤本席位实例所致;
  15 连发受控 spawn 复现实验 0 失败, 排除产品启动缺陷。教训: 并行席位期间以
  PID 快照差集 + 席位静默窗口跑套件。

## 2. 各组实测记录

### ① 导入 × 运行态 — PASS (COM1/COM2 实跑)

A (COM1, autoOpen, open) → export → replace 导入 `{version:1, bridges:[B(COM2)]}`:

- A 行消失 (row_of → None) 且 **COM1 实测释放** (watcher 判定);
- B 按清单启动: 行在列、serial=COM2、listen=清单值、running=true、**phase=open**。

merge 导回含 A 的清单: A 以原 id/串口/listen 回归且 phase=open (autoOpen 生效);
B 身份/配置/运行态零变化 (merge 前后对账), 终态桥集合恰 {qa-xt-a, qa-xt-b}。

### ② 导入 × 窗口记忆 — PASS

种子 `fleet.json = {version:1, window:{x:120,y:80,w:1024,h:768,maximized:false}, bridges:[]}`,
headless 实例 (18410) 依次经历: 启动播种 CLI 桥 → purge 清场 → 建桥 (COM99) →
**replace 导入 (清单不含 window 键)** → **merge 导回**。每步后读盘断言:

- `[window]` 段每步后**逐字段原样保留** (含 purge 与建桥的 persist 重写路径 ——
  dev-sprint11 BUG-1「整份重写抹 window」的导入路径变体不复发);
- 防「假阳」对账: replace 后盘上 bridges 恰 `["qa-xt-d-id"]` (旧桥真被换), merge 后
  `{qa-xt-d-id, id_c}` (导回真生效) —— window 保留不是「导入没生效」。

**观察 O1 (待裁定, 不判违约)**: 探针实测导入清单**携带外来 window 段**时, 盘上 window
仍为本机值 —— 导入路径忽略/丢弃清单中的 window 键。与 ADR-22①「window 属本机状态」
方向一致, 建议架构师裁定后钉进 spec (防未来实现「忠实采纳他机 window」回退)。

### ③ 导入 × 录像文件 — PASS

双路径 (环境自适应, 契约同一条): COM1 空闲 → 真 open 桥 + WS TX 载荷 (收口轮即此路径,
录像非空); 被占 → COM99 retry 桥 (record/start|stop 受理, 文件仍由本桥录制产出)。
replace 导入不含 A 的清单后: A 行消失, **recordings/ 文件存在且逐字节不变** (前后
byte 对账), 测毕自清。

**观察 O3 (记录, 不判违约)**: 串口 closed (COM99 retry) 桥上 WS TX 不落盘
(录像文件创建但为空) —— 录制 tee 位于串口写之后的推断; FR-19「录制 = tap 通道 tee」
字面下 TX 行缺失是否算缺口, 供架构师知悉。

### ④ 单实例 × fleet — PASS

种子清单 (window 段 + 桥 b1=COM1/autoOpen, 管理台 18431) 启动实例 A → 恢复出桥 →
第二实例 `--headless --addr 同端口 --fleet 同清单` (写坏风险最大化场景):

- 第二实例 **exit 0** 快速退出 (FR-16 友好路径; 探针另实测 stderr 文案
  `SerialHub 已在运行: http://…`, 见观察 O2);
- A 存活, `/api/fleet` 仍 200;
- **fleet.json 逐字节不变** (before/after byte 对比), 且解析复核 window 段与 bridges 段
  (id/串口/listen) 均原样 —— 第二实例既没写坏也没重排。

**观察 O2 (记录, 不判违约)**: release headless 的友好退出文案在 stderr 可捕获, 与
qa-sprint8 FR-18 注记「release 无 stderr」的表述不符 (或 v2.0.0 headless 形态行为有变);
本套件只断 exit 0 不依赖文本, 供 dev/架构师核对 FR-18 口径。

### ⑤ 主题持久化 × 重启 — PASS

真 Chromium (Playwright `launchPersistentContext`, 持久化 profile), **UI 用户路径**:

- set: 点「设置」→ 等主题列表异步补齐出 win95 → selectOption 触发 change →
  UI 自写 `localStorage("sh_theme")="win95"` 并挂 `/themes/win95.css` (探针不直写 localStorage);
- **重启**: 关闭浏览器进程 → 同 profile **重新 launch** (全新进程 = 真实重启);
- verify: 页面 `<head>` 同步脚本按记忆挂回 `win95.css`, `sh_theme` 仍 `win95`;
  两阶段 pageerror 均为 0。
- **人工项 (沿 qa-sprint11 §3)**: 桌面壳 (wry webview) 的 localStorage 跨应用重启持久性
  无法黑盒自动化 —— 建议 30s 人工项: 壳内切 win95 → 退出重开 → 主题应保持。

## 3. 套件与产物

- `tests/test_xtest_config.py` (5 用例, pytest): xt1 导入×运行态 / xt2 导入×window /
  xt3 导入×录像 / xt4 单实例×fleet / xt5 主题×重启; 门控沿 conftest
  fr22_ready/fr19_ready/fleet_ready + 新增 serial_pair_free/_pair_free (环境让行/降级)
  与 [BLOCKED-BY-TOOLING] (Playwright 缺席)。
- `tests/pw_theme_persist.cjs`: 主题记忆探针 (set/verify 双模式, 沿 playwright_prefill.cjs
  NODE_PATH 先例)。
- 运行口径: `SERIALHUB_EXE=target-f2/release/serialhub.exe python -m pytest tests/test_xtest_config.py -v`
  (独立构建 `CARGO_TARGET_DIR=target-f2`, 与并行席位的共享 target/release 隔离;
  管理台/数据口 18400+, 录像落 target-f2/release/recordings, 席位文件零接触)。

## 4. 移交 / 建议

1. 观察项 O1 (导入忽略外来 window 段) 与 O2 (headless release stderr 可见) 请架构师
   裁定钉口径; O3 (closed 串口桥 TX 不落盘) 供知悉;
2. 壳 webview 主题记忆 = 30s 人工项 (§2⑤);
3. 席位工具 `taskkill /IM` 全杀清场 (qa-sprint13 §12.4) 在多席位并行时会互杀, 建议尽早
   改按 PID (本轮已被误伤两次, 已如实归因)。
