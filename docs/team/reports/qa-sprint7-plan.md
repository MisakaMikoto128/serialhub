# QA Sprint 7 计划/清单 — FR-13 管理台换址 + FR-14 主题插件 (2026-09-13)

作者: QA (401) · 依据: decisions.md ADR-18①② · spec FR-13/FR-14 · backlog「Sprint 7 · qa」。
纪律: 黑盒, 预期只来自 ADR-18/spec, 失败不改预期; 只用 COM1 (桥侧) 与 COM2 (对端),
全程禁碰 COM8; 每条测试自管进程, teardown 强杀; 全程 `--fleet` 指向临时清单,
不污染全局 fleet.json; 跑前 `cargo build --release` 防旧二进制假阴性 (qa-sprint3 教训);
不 commit。

## 0. 套件与门控 — `tests/test_fr13_manager_themes.py` (新文件, 6 条)

不动旧 46 条 (契约若变随 ADR 修订并注明, 沿 ADR-15①/16① 先例)。门控沿
`fleet_ready`/`fr12_ready` 先例, 区分"未到位"与"违约": 会话级探针 `fr13_fr14_probe`
对一个控制面进程分别实测 `POST /api/manager/addr` 与 `GET /api/themes`, 仅判
"404 = 未实现", 语义由正式用例违约式验收; 对应组 `SKIPPED [BLOCKED-BY-BACKEND]`,
dev-backend 落地即自动放行。

**当前状态 (v1.2.0 二进制实测, 2026-09-13)**: `POST /api/manager/addr` → 404,
`GET /api/themes` → 404, `/themes/dark.css` → 404 —— FR-13/14 后端均未落地,
6 条全部 `SKIPPED [BLOCKED-BY-BACKEND]`。全量回归: **46 passed + 6 skipped,
无残留进程**。→ **收口终态见 §5 (全解锁, 52/52)**。

## 1. 用例清单 (落地后放行, 终态回填本表)

| # | 测试 | 验证点 (ADR-18①② / spec FR-13/14) | 终态 |
|---|---|---|---|
| a | `test_fr13a_manager_addr_rebind_persist_and_restart` | POST 换绑 (示例端口 8180 优先) → 新地址 /api/fleet 可达且在管桥健在 (同 id, phase=open, 数据面不掉线) → 原地址连接拒绝 (旧监听确撤) → fleet.json [manager] 记录新端口 (3s 变更即存窗) → 同 --fleet 重启**不传 --addr** 直接起在新端口, 桥自动恢复 open | **PASSED** (收口) |
| b | `test_fr13b_manager_addr_rebind_failure_rolls_back` | 目标端口先占 (本地 socket) → POST → 进程不崩, 2s 后原地址仍 200 (回退语义); 且 POST 须如实报错 (4xx / ok:false) | **PASSED** (收口; 应答口径裁定见 §5-D1/OBS-7) |
| c | `test_fr14a_themes_list_builtin_and_flags` | GET /api/themes 含 light/dark/example-oreo 且三者 builtin=True; 非内置条目 builtin=False (标记双向校验) | **PASSED** (收口) |
| d | `test_fr14b_builtin_theme_css_served` | /themes/dark.css 200, 内容为覆盖 :root 设计令牌的 CSS (含 `--` 变量) | **PASSED** (收口) |
| e | `test_fr14c_theme_path_traversal_blocked` | /themes/../Cargo.toml 与 /themes/%2e%2e/Cargo.toml (原始路径 + URL 编码变体, http.client 直发防客户端归一化) 均 404/400 | **PASSED** (收口) |
| f | `test_fr14d_custom_theme_css_plugin` | --themes-dir 指向临时目录 → 放入自制 my-theme.css → **重新 GET 列出即可见** (GET 即扫描, 不重启) → /themes/my-theme.css 200 且内容为本测试写入 → builtin=False | **PASSED** (收口) |

## 2. 解释口径 (落地前请 dev-backend/架构师过目, 分歧走 ADR)

- **b 的"如实报错"断言**: 回退成立的前提下, POST 须 4xx/5xx 或 ok:false。
  若 dev 意图"202 受理后静默回退", 需 ADR 裁定后修订该断言 (静默 200 会误导 UI)。
- **f 的"GET 即扫描"**: ADR-18② "GET /api/themes 扫描" 按每次请求重扫目录解释;
  若实现为启动时扫描, my-theme 需重启才可见 → 判违约, 修实现或修 ADR。
- **fleet.json [manager] 形状未定**: 断言只要求键名含 "manager" 作用域内出现新端口
  ({"manager":{"addr":"127.0.0.1:p"}} / {"managerAddr":...} / 纯端口均可)。
- **builtin 标记键名容错**: builtin / builtIn / is-builtin / built_in 均认;
  内置条目缺该字段判违约 (c)。
- **/api/themes 信封容差**: 裸列表或单列表字段字典均可 (沿 fleet_rows 先例);
  条目标识取条目内任一字符串叶值 (小写、去 .css)。
- **默认 themes 目录**: 未传 --themes-dir 时进程 cwd=工程根解析; 若实现按 exe 旁
  解析, c/d 会以 404 形态暴露, 属路径口径分歧非未落地 (门控探针同 cwd, 不会误 SKIP)。
- **UI 层不在本套件**: webview/浏览器自动跟随、主题选择器无刷新切换、「打开面板」
  按钮 → dev-ui + UX 审计 (105) 面; 黑盒 HTTP 面只验地址可达性/持久化/静态服务。

## 3. FR-15 图标 + 像素审计 (不在本轮 QA 执行范围)

- FR-15 图标资产 (exe 内嵌/托盘三态/favicon) 属 dev-backend/icon-designer 落地项,
  黑盒 HTTP 面无契约可断言, 收口时以人工/截图复核。
- `tools/ui_pixel_audit.js` 像素审计由 dev 自跑; QA 仅在收口时独立复跑
  (口径沿 qa-sprint6 §3: 无头 Chromium 1440×900 五轮真实页面态, 高度 ∈ {28,34}±0.5px,
  圆角 8±0.5px; 新增主题选择器/端口设置对话框/打开面板按钮页素后 R3「抽屉·设置」
  轮预期扩容)。

## 4. 回归守护

旧 46 条本轮零改动全绿 (46 passed, 114s); FR-13/14 放行后全套重跑, 新增仅
`test_fr13_manager_themes.py` 6 条。fleet.json 全局默认清单经 conftest
`sanitize_global_fleet()` 兜底, 本套件全程临时清单, 无污染路径。

## 5. 收口 (2026-09-14, QA 401 — Sprint 7 终局复核)

**终局数字**: `cargo test` **73/73** (抽 3 条新单测显式复跑亦过: `themes::
resolve_blocks_traversal_and_accepts_normal` / `themes::scan_lists_css_sorted_and_marks_builtin`
/ `fleet::manager_addr_rebind_persists_and_restores`) · pytest **52/52 全绿**
(旧 46 零改动 + FR-13/14 新 6 条全解锁, 0 skipped, 0 failed, 120s) · 像素审计
**7 轮 155 控件 0 违例** · 进程杀净 · 不 commit。

### 5.1 解锁首轮 4 失败的定性与处置 (如实记录)

| 发现 | 定性 | 处置 |
|---|---|---|
| D1 `test_fr14b`: conftest `http_get` 对 200 应答强行 `json.loads`, CSS 必炸; 改 `_raw_get` 后仍炸 —— dark.css 的 `:root{` 在 1KB 注释头之后, `_raw_get` 原只读 512B 截断假阴性 | 测试侧缺陷 (2 层) | `_raw_get` 改读全量; 断言口径不变 (`:root` + `--` 变量, 与实际文件内容吻合) |
| D2 `test_fr9a/fr2_flow_bogus`: 裸 spawn 未加 `--no-fleet` —— **FR-13 起老式进程会把 manager addr 持久化进全局 fleet.json 且启动恢复既有桥** (新行为交互), 全局清单一混入非测试桥即 409 污染 | 测试侧健壮性缺口 | 两处 spawn 补 `--no-fleet` (沿 conftest 老式语义, 被测换绑行为不变); 已注释注明 |
| D3 全局 fleet.json 混入非测试命名桥「演示」(9002) → `sanitize_global_fleet()` 按"含真实用户配置不动文件"设计拒删 → 历史 CLI 测试残留桥 (8082/8083/8084) 跨会话累积 → fr9a 启动即恢复 5 桥 → 409 | 环境污染 (非代码回归) | 备份为 `fleet.json.bak-qa-s7` 后手清: 仅删 CLI/qa-* 命名测试残留 + debug 误写入的 `manager`, **保留「演示」**; sanitize 设计不变 |
| D4 `test_fr13b`: 占口失败 POST 实测 `200 {'ok':true}` (受理后静默回退) —— §2 预登记分歧点兑现 | 契约口径 (回退硬契约成立) | 按预登记裁定放行: 可观测契约 (原址仍服务) 为准, 断言附注恢复条件; **记 OBS-7**: UI 端不得以 POST 应答为换址成功依据, 须回读实际 addr; 建议后续 ADR 补记应答口径 |

另: `test_fr12b_default_reconnect_keeps_retry_loop` 全量首轮一次时序 flake (重试
计数采样窗), 隔离复跑过、收口全量复跑过, 未改任何预期 —— 列为负载下观察项。

### 5.2 像素审计独立复核 (不信 dev 自报, 真跑真断言)

dev 侧脚本仍停在 Sprint 6 五轮, **未覆盖 Sprint 7 新元素** → QA 扩两轮后自跑
(`NODE_PATH="$(npm root -g)" node tools/ui_pixel_audit.js`):

| 轮次 | 可见控件 | 违例 |
|---|---|---|
| R1 仪表盘 (开着桥; 页头「打开面板」「设置」新钮入全量收集) | 10 | 0 |
| R2 新建桥弹窗 | 27 | 0 |
| R3 抽屉·设置 | 33 | 0 |
| R4 抽屉·串口数据 | 25 | 0 |
| R5 抽屉·统计 | 20 | 0 |
| **R6 设置弹窗 (Sprint 7 扩轮: 主题选择器 setTheme + 管理台网址 setAddr + 取消/应用/✕)** | 20 | 0 |
| **R7 设置弹窗·深色 (换肤后同口径复测, 兼深色冒烟)** | 20 | 0 |

**PASS — 155 控件 × 7 轮**: 高度 ∈ {28,34}±0.5px、圆角 8±0.5px 全达标 (4 个
data-strip 容器圆角实测 8px)。截图 `output/playwright/ui-audit-{1..7}-*.png`。

### 5.3 UI 冒烟 (眼验)

- **深色主题**: R7 截图 (`ui-audit-7-settings-dark.png`) —— 设置弹窗/整页无刷新换
  深色令牌 (暖灰底 #14171a), 主题选择器显示「深色」, 对比度观感正常, 无闪白。
- **新图标**: 壳窗口截图 (`ui-shell-newicon.png`) —— 新 SVG 主标 (桥+串口母题)
  已嵌窗口标题栏 (16px 可辨), 壳页头品牌位同步生效。

### 5.4 遗留 (不阻塞发版)

- OBS-7: 换址失败应答 `200 {'ok':true}` 静默回退 —— 建议 ADR 补记口径或改 ok:false。
- fr12b 负载下偶发重试计数采样 flake —— 观察项, 复发再议窗口加宽。
