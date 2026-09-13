# QA Sprint 6 计划/清单 — FR-12 自动重连可选项 + UI-1 像素审计 (2026-09-13)

作者: QA (401) · 依据: decisions.md ADR-16①② · spec FR-12/UI-1 · backlog「Sprint 6 · qa」。
纪律: 黑盒, 预期只来自 ADR-16/spec, 失败不改预期; 只用 COM1 (桥侧/审计桥) 与 COM2
(仅 FR-12e 手动单次打开验证, 架构师指令指定真实在位端点), COM99 为幽灵端口,
全程禁碰 COM8; 每条测试自管进程; 跑前 `cargo build --release` 防旧二进制假阴性
(qa-sprint3 教训; 本次两次重建 —— dev-backend/dev-ui 并行落地后重测); 不 commit。

## 1. 契约随动 (任务 §2, ADR-16①) — 已实测生效

| 项 | 修订 | 落点 |
|---|---|---|
| 单桥 /api/status | 12→13 字段 (+`autoReconnect`, 默认 true) | `tests/conftest.py` STATUS_FIELDS + `status_fields_expected()` |
| fleet 行/详情 | 14→15 字段 (+`autoReconnect`) | `tests/conftest.py` FLEET_ROW_FIELDS + `assert_row_shape()` (加布尔校验) |
| 旧断言修订注明 | "恰 13 字段 (ADR-11 + ADR-15①/ADR-16① 随动修订)" | `test_fr4_status.py` / `test_fr10_fleet_compat.py` / `test_fr10_fleet.py` 注释与断言消息 |

渐进放行口径 (沿 fleet_ready 先例, 区分"未到位"与"违约"):
`status_fields_expected()` 在 `autoReconnect` 未落地时按旧 12/14 字段契约**严格守护**,
仅容忍该一字段缺席 —— 其它任何多/少字段仍当场违约; 落地即自动升为 13/15 全量。
实测: dev-backend 落地后旧 40 条零改动全绿 (守护未破), FR-12 门控组自动放行。

## 2. 语义黑盒套件 `tests/test_fr12_reconnect_opt.py` (a-f, 6 条)

门控: 会话级探针 `fr12_ready` (/api/status 无 `autoReconnect` → 整组
`SKIPPED [BLOCKED-BY-BACKEND]`)。dev-backend 波1 落地后已自动放行, 实测终态如下。

| # | 测试 | 验证点 (ADR-16①) | 终态 |
|---|---|---|---|
| a | `test_a_no_reconnect_flag_failed_open_goes_closed_not_retry` | `--no-reconnect` 起桥 (COM99) → 回显 false; 落定 **closed 非 retry** + 注明 + 6s 采样 retries 恒 0 | **FAIL → 相位/retries 语义全过; 仅 lastError 未注明 (见发现 D1)** |
| b | `test_b_default_reconnect_keeps_retry_loop` | 默认 true 同场景守护: COM99 → retry 且 retries 递增 (ADR-15 行为未破) | PASSED |
| c | `test_c_fleet_create_echoes_auto_reconnect` | 建桥 body `{"autoReconnect": false}` → fleet 行 + 详情回显 false (详情 {ok,bridge} 信封容差); 默认建桥回显 true | PASSED |
| d | `test_d_patch_config_disables_reconnect_live` | 占 COM1 → retry; 运行中 `PATCH /api/fleet/<id>/config {"autoReconnect": false}` → **即时生效** retry→closed, 回显更新, 3s 窗不再重试 | PASSED |
| e | `test_e_manual_open_still_works_when_disabled` | false 下手动 open: 幽灵 COM99 失败 → closed 不 retry; 真实在位 COM2 (架构师指令) 手动 open → **open** 且开关保持 false | PASSED |
| f | `test_f_closed_last_error_notes_reconnect_disabled` | 专项: closed 态 lastError 注明「自动重连已关闭」(启动失败/手动失败两路径) | **FAIL (发现 D1)** |

**发现 D1 (dev-backend 违约项, 如实 FAIL 不改预期)**: ADR-16① 明文 "lastError 注明
「自动重连已关闭」"; 实测 closed 态 lastError 仅回 OS 文案 (如 "打开 COM99 失败:
系统找不到指定的文件。" / "打开 COM1 失败: 拒绝访问。"), 未含注明。补齐后 a/f 即绿,
预期零改动。

## 3. UI-1 像素审计 — `tools/ui_pixel_audit.js` (真跑真断言, 非目测)

Playwright (真后端 8080 + 管理台, 自拉进程 + 临时 fleet 清单, 不污染全局配置):
建桥 qa-ui-audit@COM1 至 **open** (开着桥), 无头 Chromium 1440×900 五轮真实页面态,
各截图至 `output/playwright/ui-audit-{1..5}-*.png`: R1 仪表盘 / R2 新建桥弹窗 /
R3 抽屉·设置 / R4 抽屉·串口数据 / R5 抽屉·统计。每轮收集全部可见
button/input/select 的 `getBoundingClientRect().height` + 四角 border-radius,
断言 高度 ∈ {28,34}±0.5px、圆角 8±0.5px; 隐藏/零尺寸/opacity:0 记 skipped。
违例输出表格 (轮次/区域/元素/实测值), 全过 → PASS (exit 0)。
运行: `NODE_PATH="$(npm root -g)" node tools/ui_pixel_audit.js`

**两轮实测对照 (同脚本, UI-1 落地前后)**:

| 轮次 | 落地前 (v1.1.0 UI) | 落地后 (dev-ui 波1) |
|---|---|---|
| R1 仪表盘 | 8/8 违例 | **0/8 违例** |
| R2 新建桥弹窗 | 25/25 | **1/26** (npAuto 开关 2px) |
| R3 抽屉·设置 | 31/31 | **4/32** (页签圆角 0 ×3, cfAuto 开关 2px) |
| R4 抽屉·串口数据 | 23/23 | **5/23** (页签圆角 0 ×3, ASCII/HEX 圆角 0) |
| R5 抽屉·统计 | 18/18 | **3/18** (页签圆角 0 ×3) |
| **合计** | **105/105** | **13/107** |

**发现 D2 (dev-ui 残项, FAIL→PASS 即 UI-1 验收)**: ① 新增重连开关
`input#npAuto/#cfAuto.switch` 原生 input 高 2px (视觉为伪元素开关) —— 按 UI-1
"input 全部归档两档" 字面违约, 或改输入件本体达标或与 QA 裁定开关豁免标记;
② 抽屉页签 `#tb-cfg/#tb-tap/#tb-stats` 与 ASCII/HEX 段控 `#viewAscii/#viewHex`
高度已达标 (34/28) 但圆角 0px ≠ 8px。脚本与断言不变, 重跑全绿即收口。

## 4. 回归 (任务 §4: 旧 40 条守护不动)

终态 (落地后 `python -m pytest tests/ -q`): **43 passed, 3 failed** =
2 × 发现 D1 (a/f) + 1 × fr9a_restart_e2e 偶发; fr9 全组单测复跑 5/5 绿
(进程/换址计时敏感用例在本机偶发, 与契约无关, 见 §4 注 —— 本 Sprint 全量共 3 次
不同 fr9a 子用例偶发后均单跑复绿, 建议后续固化串行化或放宽进程计数采样)。
契约断言随 §1 修订后旧用例零逻辑改动, 未落二进制假阴性未发生 (两次 build 后实测)。

## 5. 执行命令 (项目根)

```bash
cargo build --release                                    # 防旧二进制假阴性
python -m pytest tests/ -q                               # 全套件
python -m pytest tests/test_fr12_reconnect_opt.py -v     # FR-12 语义组
NODE_PATH="$(npm root -g)" node tools/ui_pixel_audit.js  # UI-1 像素审计
```

## 6. 收口 (2026-09-13 复验, D1/D2 修复后)

| 项 | 结果 |
|---|---|
| pytest 全量 | **46 passed, 0 failed, 0 skipped** —— a/f 复跑过 (D1: closed lastError 固定后缀 " (自动重连已关闭)" 实测到位), 契约 13/15 字段全量生效 |
| cargo test | **64 passed** (56→64, +8); 抽验 3 条新单测名在源: `supervisor::tests::reconnect_false_open_fail_goes_closed_no_retry` / `supervisor::tests::reconnect_false_drop_goes_closed_with_fixed_message` / `hub::tests::auto_reconnect_echo_and_roundtrip` |
| 像素审计 (QA 独立复跑, 不采信 dev 自报) | **PASS — 105 可见控件 × 5 轮 0 违例**, 高度 ∈ {28,34}±0.5, 圆角 8±0.5; 4 个 data-strip 条带容器圆角实测 8px (子元素仅免 radius、高度不豁免 —— 口径核对未放水) |
| 开关 skipped 合理性 | `input.switch` opacity:0 / 1×1px / absolute (sr-only 惯用法), role="switch"+aria-label 无障碍完备; 可见轨道 `label.switch-track` 实测高 28px (紧凑档)。轨道为胶囊圆角 999px, 在 UI-1 button/input/select 字面范围外且属苹果式开关惯例, 如实注记 |
| 卫生 | 全程未碰 COM8; serialhub 进程杀净; 未 commit |
