# QA Sprint 5 计划/清单 — FR-11 串口热拔插自动重连 (2026-09-13)

作者: QA (401) · 依据: decisions.md ADR-15①③④ · backlog「Sprint 5 · qa」·
任务契约 (每桥对象/status 新增 `retries`: retry 态递增、打开成功归 0; COM99 缺席递增;
恢复零动作; 客户端保持)。纪律: 黑盒, 预期只来自 ADR-15, 失败不改预期; 只用 COM1/COM2
虚拟对 (方向沿用 conftest 约定 桥侧=COM1/对端=COM2), COM99 为幽灵端口, 全程禁碰 COM8;
每条测试自管进程, 不 commit。

## 1. 套件构成 (新增 1 文件, conftest 零改动, 旧 36 条零改动)

| 文件 | 内容 |
|---|---|
| `tests/test_fr11_reconnect.py` | ADR-15③④ 共 4 条: retries 递增 ×2 (fleet / 单桥 status, 门控)、retry 态客户端保持+零动作恢复 ×1、串口断/恢复客户端零动作续传 ×1 (降级) |

门控: `retries` 字段未落地 → 递增/归零用例 `SKIPPED [BLOCKED-BY-BACKEND]`, 后端波1
落地即自动放行 (沿用 Sprint 4 fleet_ready 同一原则: 区分"未到位"与"违约")。

## 2. 套件清单 × 验证点 × 当前状态

| # | 测试 | 验证点 (ADR 依据) | 状态 |
|---|---|---|---|
| 1 | `test_fr11_fleet_retries_increments_on_missing_port` | COM99 桥 start → phase=retry + lastError 非空 (FR-3); /api/fleet 桥对象 `retries` 为 u32, 7s 采样 (0.5s 粒度) 严格递增 ≥3 次 (监督 1s 间隔, ADR-4/15①), 单调不减、phase 恒 retry; retry 态可 delete | **BLOCKED-BY-BACKEND** (retries 未落地) |
| 2 | `test_fr11_status_retries_increments_and_reset_on_open` | 单桥 /api/status `retries` 同步递增 ≥3 次; 换回 COM1 → open 成功 → `retries == 0` (ADR-15① "成功打开归 0") | **BLOCKED-BY-BACKEND** (retries 未落地) |
| 3 | `test_fr11_retry_phase_client_holds_and_zero_action_recovery` | **真实 retry→open 相变**: 先占住桥侧 COM1 (pyserial) → 桥 start 后 open 必败 → phase=retry; 数据面 WS 在 retry 态可接入 (ADR-13⑤ 端点稳定); 保持期 ≥3s (≥3 个监督周期) 周期 ping/pong 存活 + clients 恒 1; **释放 COM1 → 零 API/客户端动作自动回 open** (实测 ~1.1s); 对端灌 768B 原客户端 (未重连) 逐字节续收; 恢复后 retries==0 (落地时) | **PASSED** (现行二进制) |
| 4 | `test_fr11_serial_cycle_client_zero_action_continuity` | 任务降级路径 (b): open 于 COM1 → 对端灌 512B 收满 → 控制面 close 模拟串口断 → closed 期间 clients==1 + ping/pong 存活 ≥2 次 → open 恢复 → 对端再灌 512B, 同一连接零动作续收, 逐字节一致 | **PASSED** (现行二进制) |
| — | 既有 36 条回归 | `python -m pytest tests/` | **38 passed, 2 skipped (73.7s)**, 0 失败, 无进程残留, COM1/2/8 端口表无扰动 |

## 3. ELTIMA CLI 调查结论 (ADR-15③ 指定调查项)

**结论: 无可用程序化删/建虚拟对的 CLI, 真拔插无法自动化 → 走任务降级路径。**

调查过程 (2026-09-13, 本机):

| 线索 | 位置 | 结论 |
|---|---|---|
| VSPD Pro 9.0 (当前 COM1/COM2 对的管理者, 注册表 Uninstall 确认 Build 9.0.270) | `C:\Program Files\Eltima Software\Virtual Serial Port Driver Pro 9.0` | 仅 `vspdpro.exe`(GUI) + `vspdpro_service.exe`(服务) + 安装器; **无 CLI 工具**。二进制 strings 仅有 bundle 启用/禁用日志文案 (GUI 动作写日志), 无 install/remove 命令动词 |
| VSPD 7.2 (旧版共存) | `C:\Program Files\Eltima Software\Virtual Serial Port Driver 7.2` | `vspdconfig.exe` 为 MFC GUI 应用 (CCommandLineInfo): 无参数运行即开 GUI、控制台零输出; 以 `remove COM30 COM31` (不存在的擦边对) 试探 → 挂起无输出无效果; strings 无 usage/命令动词。其控件 `vspdctl.dll` 面向 v7 驱动 (`\\.\VSPD_Ctrl_7`), 管不了 Pro 9 的 bundle |
| 其他常见路径 | `C:\Program Files\VComManage` | 空目录 |
| 权限 | 当前 shell | 非管理员 (net session 验证); 即便有 CLI, 增删虚拟对属驱动级操作需提权, pytest 常规运行不满足 |

注: vspdpro.txt 日志显示 Pro 9 支持 bundle「禁用/启用」= 等效拔插, 但只能 GUI 手动操作;
bundle 4 = COM7↔COM8 (真实使用中的对, 测试纪律禁碰)。

## 4. 覆盖边界 (如实标注) + 人工演练指南

自动化已覆盖: retry 态可观测 (phase/lastError)、retries 递增/归零契约 (待后端落地即跑)、
**真实 retry→open 相变的零动作恢复与数据续传** (#3, 借"占用-释放"制造相变, 优于纯降级)、
数据面 WS 与串口状态解耦 (closed/retry 期间连接保持)。未覆盖: ELTIMA 虚拟对被物理删除
后重建的真拔插路径 (无 CLI/权限)。

**人工演练指南 (用户执行, 真机 COM8 拔插 10 次)**:

1. 准备: COM8 接真实 USB 串口设备 (或对端终端), 桥配置指向 COM8 (GUI 或 `serialhub --port COM8`), 数据面客户端 (浏览器页或脚本) 保持连接并观察。
2. 拔出 USB → 预期: 徽章变「重连中」, 卡片显示「串口已断开，正在自动重连 (第 n 次)…」(ADR-15②), n 随时间递增; 客户端页面不掉线、无报错弹窗; 火花线断档。
3. 插回 USB → 预期: 数秒内自动回「运行中」, 页内横幅「串口已恢复」约 3s, 客户端零动作恢复收发; 对端续灌数据客户端续收。
4. 重复拔插 10 次 (含快速连拔 <1s 间隔 2 次): 10/10 次均自动恢复, 客户端全程零重连动作, retries 计数与横幅行为符合 2/3 描述。
5. 记录: 每次恢复耗时、retries 峰值、任何一次需人工干预/重启进程的异常 (有则判 FAIL)。

## 5. 交接 / 下一步

- 后端 (dev-backend 波 1) 落地 `retries` 后: 用例 #1/#2 自动放行, 无需改动; 契约 11→12 /
  13→14 字段, **旧守护 `test_fr4_status.py::test_fr4_status_contract` 断言恰 11 字段将按
  ADR-15① "契约测试随动" 失效, 需下一轮 QA 修订 STATUS_FIELDS (本轮按"旧 36 条守护不动"
  纪律未触碰, 特此预告)**。
- 集成复验时建议把 #3 的"占用-释放"手法纳入常规回归 (真实相变, 零成本模拟监督自愈)。

## 6. 波2 收口 (2026-09-13, 后端 retries 落地)

**最终数字: pytest 40/40 全绿 (0 skip) · cargo test 56/56 · FR-11 4/4 · UI 冒烟通过。**

1. **解锁**: #1/#2 去除 `[BLOCKED-BY-BACKEND]` 门控 (conftest 探针保留为 FR-10 就绪门控);
   按实证语义加严 —— 进入 retry 态即 `retries ≥ 1` (首次失败计入, 黑盒 trail 1→8 逐秒递增);
   #3 的 retries 断言 (保持期累计 + 成功打开归 0) 同步无条件化。
   注: 收口时 release 二进制滞后于源码 (无 retries 字段), `cargo build --release` 重建后放行
   (有一个 dev 会话残留 serialhub.exe 占锁, 已 taskkill 后重建)。
2. **契约随动修订** (ADR-15①, 均注明): conftest `STATUS_FIELDS` 11→12、
   `FLEET_ROW_FIELDS` 13→14 (+`retries` 入非负数值校验); `test_fr4_status_contract`
   恰字段断言 12 字段 + 初始 `retries == 0`; `test_fr10h_compat` 旧端点契约同步 12 字段
   (+ open 态 `retries == 0`); test_fr10 注释 12→14 字段口径。
3. **全量回归**: pytest 40/40 (87.9s, 0 失败 0 跳过) · cargo test 56/56 (2.32s)。
4. **UI 冒烟** (headless 进程 + playwright, 截图存本目录):
   - COM99 桥 → 卡片徽章「重连中」+ 横幅「串口已断开，正在自动重连 (第 n 次)…」,
     n 随 1s 监督节奏递增 (实测 20→45→50, 与 /api/fleet retries 同步);
     见 `ui-smoke-retry-increment.png` (第 45 次时刻, 含 lastError 明示)。
   - 「停止」→ 徽章「已停止」、横幅消失, /api/fleet `retries` 归 0 (API 复核);
     见 `ui-smoke-after-stop-reset.png`。ADR-15② 大白话文案与递增呈现符合规格。
5. 演练纪律照旧: COM8 未碰, 进程杀净 (终检 0 残留), 未 commit。
