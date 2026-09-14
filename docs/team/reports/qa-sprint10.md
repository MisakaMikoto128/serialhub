# QA Sprint 10 报告 — UI-1 像素审计主题感知化 + win95 前置验证 (2026-09-13)

作者: QA (401) · 二进制: `cargo build --release` (src 无改动, 增量重建) · 审计目标 127.0.0.1:8080, 桥 qa-ui-audit@COM1 open
· 纪律: 全程只用 COM1 (审计桥), COM8 未碰; 审计/pytest 测毕杀净 (serialhub.exe 残留 0); 本轮未 commit。

## 1. 审计脚本主题感知改造 (tools/ui_pixel_audit.js)

**口径变化 (已写进脚本头注释)**:

| 项 | 旧口径 | 新口径 (Sprint 10) |
|---|---|---|
| 控件圆角 | 四角 = 8±0.5px 硬断言 | 每轮先读页面 `#themeCss` 得当前主题 → `GET /themes/<主题>.css` 解析 `--ctl-radius` (px 可省) 作期望值 (按主题缓存) → 实测四角 = 期望 ±0.5px。内置 light/dark/example-oreo 期望 8 不变; **win95 期望 0 不再被 8px 误杀** |
| 期望值来源 | 脚本常量 `WANT_RADIUS=8` | 主题 CSS 实时解析; CSS 取不到 (404) 或缺 `--ctl-radius` 令牌 → 审计硬失败 (期望值不许猜, 不许"差不多") |
| data-strip 容器 (D2) | 容器四角 = 8±0.5 | 容器四角 = 当前主题期望 ±0.5 (严格性不降, 随主题) |
| 高度两档 | ∈ {28,34}±0.5 | **不变** (--ctl-h/-ctl-h-sm 各主题同值, 含 win95 34/28) |
| 主题指定 | 无 (浅色起跑, R7 固定切深色) | 新增 `AUDIT_THEME=<name>` 单主题审计: 落 `localStorage(sh_theme)` 后重载锁定主题, 七轮全在该主题下 (R7 不再切深色); 截图带 `ui-audit-<theme>-` 前缀, 顺带存管理台 fullPage 全景到 `docs/team/reports/qa-sprint10/` |

缺省 (不带 `AUDIT_THEME`) 行为与旧流程完全兼容: 浅色 6 轮 + R7 深色复测, 本轮全过 (155 控件 × 7 轮)。

## 2. 真机审计结果 (桥 open 态, Playwright 无头 Chromium)

| 运行 | 期望 --ctl-radius | 轮次/控件 | 结论 |
|---|---|---|---|
| 缺省流程 (light×6 + R7 dark) | 8px (light.css/dark.css 实时解析) | 7 轮 / 155 | **PASS** (违例 0, 条带容器 4) |
| `AUDIT_THEME=light` | 8px | 7 轮 / 160 | **PASS**; 全景 `admin-panorama-light.png` |
| `AUDIT_THEME=dark` | 8px | 7 轮 / 160 | **PASS**; 全景 `admin-panorama-dark.png` |
| `AUDIT_THEME=example-oreo` | 8px | 7 轮 / 160 | **PASS**; 全景 `admin-panorama-example-oreo.png` |
| `AUDIT_THEME=win95` | — | — | **[BLOCKED-BY-BACKEND]**: `src/themes.rs::BUILTIN` 仅 light/dark/example-oreo, `GET /themes/win95.css → HTTP 404`, 审计按设计硬失败中止。win95 "圆角 0 全过" 与管理台全景 (浅银+青底) 均未验证 |

审计截图: `output/playwright/ui-audit-*.png` (缺省 7 张) + `ui-audit-{light,dark,example-oreo}-*.png` (各 7 张); 全景 3 张在 `docs/team/reports/qa-sprint10/`。

**后端落地后一键复验 win95** (无需再改审计脚本):

```bash
cargo build --release
AUDIT_THEME=win95 NODE_PATH="$(npm root -g)" node tools/ui_pixel_audit.js
```

预期: 每轮打印 "主题 win95: --ctl-radius 期望 0px", 7 轮违例 0, 并落 `docs/team/reports/qa-sprint10/admin-panorama-win95.png`。

## 3. 回归

- **旧 46 条守护** (fr1/fr2/fr3/fr4/fr9/fr10×3/fr11/fr12/perf, Sprint 7 前老套件): **46 passed** (122s)。
- **全套 pytest**: **63 passed** (178s) —— 与基线 63/63 一致 (dev 并行 win95 单测落地后预计 86+, 届时重跑)。
- **win95 /api/themes 契约**: dev 单测**未覆盖** —— `src/themes.rs` 单测 `builtin_contains_three_themes` 仍断言内置恰三套, `tests/test_fr13_manager_themes.py::REQUIRED_BUILTIN` 亦无 win95 (live 404 与之一致, 非契约破损)。

## 4. 后端落地交接清单 (win95)

1. `assets/themes/win95.css`: 直角 `--ctl-radius:0` (审计解析允许省 px), 银灰面板/青底/海军蓝 accent; `--ctl-h:34px` / `--ctl-h-sm:28px` 沿两档。
2. `src/themes.rs`: BUILTIN 追加 win95 + `builtin_contains_three_themes` 单测同步。
3. `tests/test_fr13_manager_themes.py`: `REQUIRED_BUILTIN` 追加 win95 (builtin 标记双向断言随之覆盖)。
4. `AUDIT_THEME=win95` 复跑像素审计 (期望圆角 0 全过) + 全景入 `docs/team/reports/qa-sprint10/`。
5. 全套 pytest 重跑 (预期 86+)。

## 5. 收口 (win95 后端落地复验, 2026-09-13)

后端已落 BUILTIN 四套 (dev 重建二进制)。`cargo build --release` 重建后全部复验; 测毕 serialhub.exe 残留 0, COM8 未碰, 未 commit。

### 5.1 win95 像素审计 — PASS

`AUDIT_THEME=win95 node tools/ui_pixel_audit.js`: 每轮实时解析 `/themes/win95.css` 得 `--ctl-radius` 期望 **0px**, 七轮 (R1 仪表盘→R7 设置弹窗) **160 个可见控件违例 0**, 高度两档 {28,34}±0.5 不变, 4 个 data-strip 条带容器直角实测=期望。首次实证: 直角主题不再被旧 8px 硬断言误杀, 且 8px 三主题 (light/dark/oreo) 回归无恙 (§2 各 PASS)。

- 全景 (发布页卖点图): `docs/team/reports/qa-sprint10/admin-panorama-win95.png`
- 各轮截图: `output/playwright/ui-audit-win95-*.png` (7 张)

### 5.2 眼验 win95 观感 — 味道对

青底 (teal 桌面色) / 银灰面板 (含经典凹凸边) / 全站直角 (控件+条带+徽章) / 海军蓝主按钮 (新建桥·启动·桥名章, 白字清晰)。可读性: 徽章「运行中/已停止/已连接」银底深字均可读; 终端区黑底浅字 (空态提示「还没有数据…」清楚), ASCII/HEX 段控选中态海军蓝白字。小观察 (不阻塞): 仪表盘页脚说明文字在青底上对比度偏低 (装饰性文案, 浅色主题同款弱化, 留 UX 定夺是否 win95 单调)。

### 5.3 回归

- **全量 pytest**: 首跑 62 passed + 1 failed (`test_fr14a_themes_list_builtin_and_flags`) —— 即 §4-3 契约缺口: `REQUIRED_BUILTIN` 未含 win95, 反向 builtin 标记断言命中。按已落契约修订 (非凑绿; dev 单测 `themes.rs` 已钉四套) 追加 win95 后复跑: **63 passed** (179s)。dev 的 win95 单测在 Rust 侧, pytest 总数不变 63。
- **cargo test --release**: **86 passed / 0 failed** (3.4s) —— 架构师预报的 "86+" 实为此处 (含 `win95_theme_pins_retro_tokens` 等新单测)。
- win95 /api/themes 契约: pytest `REQUIRED_BUILTIN` 与 Rust 单测双侧已覆盖并全绿 (§4-3、§4-2 均已闭环)。
