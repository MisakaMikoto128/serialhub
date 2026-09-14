# QA Sprint 8 计划/清单 — FR-16 单实例友好处理 + FR-17 新建桥端口预填 (2026-09-14)

作者: QA (401) · 依据: decisions.md ADR-19①②③ · spec FR-16/FR-17/FR-18 · backlog「Sprint 8 · qa」。
纪律: 黑盒, 预期只来自 ADR-19/spec, 失败不改预期; 本轮两模块**不占任何串口**
(FR-16 探测只依赖控制面 /api/status 含 phase; FR-17 建桥按 fleet_create 契约填 COM1),
全程禁碰 COM8; 进程全生命周期管理 + teardown 强杀 + kill_all_bridges 兜底; 不 commit。

## 0. 套件与门控

不动旧 52 条。新文件:

- `tests/test_fr16_single_instance.py` (8 条, debug/release 双构建参数化)
- `tests/test_fr17_prefill.py` (3 条) + `tests/playwright_prefill.cjs` (探针脚本,
  沿 tools/ui_pixel_audit.js 的 `NODE_PATH=$(npm root -g)` + require 先例, 无头
  Chromium 只开弹窗读 `#npListen`, **不提交表单**)
- `tests/conftest.py` 修订: 仅增 FR-18 影响注记 (见 §3), 无行为变更。

门控沿 `fleet_ready`/`fr12_ready` 先例, 区分"未落地"与"违约":

- FR-16: `fr16_gate` 会话级行为探针 —— 对每个构建实测"第二实例 headless 同地址
  是否 0 退出"。探针不过 → 对应组 `SKIPPED [BLOCKED-BY-BACKEND]`。
  **非 SerialHub 占用组不设门** (落地前后都必须成立, 守护旧错误路径不被误伤)。
- FR-17: 探针读一次预填, 值为空 → 整组 `SKIPPED [BLOCKED-BY-FRONTEND]`
  (预填属前端行为; 任务书统称 BLOCKED-BY-BACKEND, 此处按实际归属细分, 语义同)。
  Playwright 缺失 → `SKIPPED [BLOCKED-BY-TOOLING]`。

## 1. 用例清单 (ADR-19①② / spec FR-16/17)

| # | 测试 | 验证点 | 终态 |
|---|---|---|---|
| a | `fr16a_headless_second_instance_friendly` [release/debug] | 第二实例 headless 同地址: **exit 0**; debug 另断 stderr 含「已在运行」(release 因 FR-18 无 stderr 只断退出码); 第一实例 A 仍活且 /api/status 仍 200 | **PASSED ×2** |
| b | `fr16b_headless_non_serialhub_occupant` [release/debug] | 裸 socket 占端口: 第二实例 **exit 非 0** + debug stderr 含「占用」语义; 不设门 (v1.3.0 基线即如此, 落地后须维持) | **PASSED ×2** |
| c | `fr16c_gui_second_instance_info_box_exit0` [release/debug] | GUI 第二实例 (目标=SerialHub): 信息框弹出 → WM_CLOSE 关闭 (单键框等价「确定」) → **进程 0 退出**; A 仍活。弹窗文案/图标与自动开浏览器为目视项 (§4); 确定后浏览器会真实打开管理台标签 (契约副作用, 已在 docstring 注明; 无显示环境设 `SERIALHUB_QA_SKIP_GUI=1`) | **PASSED ×2** |
| d | `fr16d_gui_non_serialhub_occupant_keeps_error_box` [release/debug] | 非 SerialHub 占用: GUI 第二实例维持旧**错误框** (FR-8) → 关闭 → **exit 非 0**; 不设门 | **PASSED ×2** |
| e | `fr17a_empty_fleet_prefills_manager_plus_1` | 空清单: 预填 = 管理台端口+1 (Playwright 真读 #npListen) | **PASSED** |
| f | `fr17b_prefill_skips_used_ports` | 建两桥占 P+1/P+2: 预填向上跳过已用 = P+3 | **PASSED** |
| g | `fr17c_prefill_refills_hole_after_delete` | 删中间桥 (P+1) 且数据口确认释放: 预填回填空洞 = P+1 | **PASSED** |

解释口径: a/c 的 stderr 文案只锁「已在运行」核心子串 (前后缀不锁); FR-17 场景
每条测试自建自清 (purge 前置 + 后置), 预填判定不受上一场景残留影响。

## 2. 时间线备注 (并行开发实证)

- 套件编写时 (2026-09-13 夜) 后端/前端均未落地: v1.3.0 二进制实测第二实例
  exit 1 + stderr「管理台端口被占用…(os error 10048)」→ 门控设计即按此基线。
- 2026-09-14 晨 dev-backend/dev-ui 波1 落地 (main.rs:87 headless stderr 文案 /
  gui.rs:443 信息框 / main.rs:5 FR-18 windows_subsystem / ui prefillListenPort
  异步预填), 二进制重建后门控自动放行, 11 条全 PASSED —— 门控按设计工作
  ("未落地→SKIP" 与 "落地→实测" 两态均实证)。

## 3. FR-18 对既有套件的影响评估 (conftest 修订)

release 构建为 GUI 子系统后, headless release 的 stdout/stderr 不可见 (ADR-19③ 既定取舍)。
逐项核查 conftest: 就绪门控 `wait_http_ready` **只依赖 HTTP 轮询** (200/409), 不依赖
stdout; 日志文件仅作失败诊断 (`_tail`), 为空不影响判定。结论: **无功能性修订需要**,
已在 conftest 头部增注记固化规则: "需要断言 stderr 文本的用例自行改用 debug 构建,
release 只断退出码; 后续用例不得以 stdout 内容作判据"。FR-16 套件即按此双构建矩阵实现。

## 4. 人工目视清单 (自动化不覆盖, 收口时人工过一遍)

| # | 项目 | 预期 (ADR-19①) |
|---|---|---|
| M1 | GUI 第二实例信息框样式 | **信息图标** (非错误 ✗), 文案「SerialHub 已在运行」+ 管理台网址, 按钮「确定」 |
| M2 | 确定后浏览器行为 | 系统默认浏览器自动打开该管理台 (地址与信息框一致) |
| M3 | 非 SerialHub 占用时 GUI 错误框 | 维持旧错误框样式 (FR-8, 错误图标「SerialHub 启动失败」) |
| M4 | FR-18 release GUI 无控制台 | 双击 release exe: 无黑窗; --headless release 无 stdout (文档已注明) |
| M5 | FR-17 预填可改性 | 预填值手动改后不被异步回填覆盖 (实现有 npListenTouched 守卫, UI 面目视) |

## 5. 回归

全量 `python -m pytest tests/ -v`: 旧 52 条不动 + 新 11 条, 终态回填:
**63 passed, 0 failed, 0 skipped (11 条新套件全解锁), 无残留进程** (2026-09-14,
门控探针在双构建上实测放行)。
