# Dev Sprint 13 报告 — B1 录制/回放页签 + B4 配置导入导出 (dev-ui 301)

- 日期: 2026-09-14 · 变更文件: **仅 `ui/index.html`** (抽屉第 4 页签「录制」/ 设置弹窗导入导出块 /
  配套 CSS 与脚本; 未 commit, 未碰 src/tests)
- 依据: ADR-24 (Sprint 13 B1/B4) · spec FR-19/FR-22 · backlog Sprint 13 波 1 · UI-0/UI-1 全程遵循
- 自验: **mock 契约控制面** (127.0.0.1:18500, 按下发契约仿真; 桥 COM2/9001 运行态) +
  Playwright **34 断言全过**, 预期外页面 JS 异常 0。mock+测试脚本在 `output/sprint13-ui-mock/`
  (gitignored, 不入库); 真后端 18200 未起 —— 已核实 src 尚无 record 路由 (dev-backend 波 1 在途)。

## 1. 抽屉「录制」页签 (FR-19)

1. **页签**: `cfg/tap/rec/stats` 四页签; 进页签即拉 `GET .../recordings` 渲染列表。
2. **录制钮**: 空闲 =「● 开始录制」(红点录制母题); 录制中 = danger 态「停止录制」+
   红底状态行 `录制中 · 已录 0:03 · 6 帧 · 180 字节` (1s 节拍走表, 不整列重绘)。
   帧数/字节**从 recordings 列表回读**: 后端若对录制中文件即时回填即实时显示,
   不回填则显示「帧数字节在停止后统计」—— 不谎报 0 (DEF-2 同哲学)。
3. **录像列表**: 行 = 文件名 (等宽截断) + `时长 · 字节数` + [回放][删除]; 空列表/取列表失败
   均给可行动话术。删除两段确认 (3s 自动复原), `POST .../recordings/delete {file}`
   (**契约缺口 → §5 Q1**)。
4. **回放**: 工具条 `回放速度 0.5/1/2/5 倍` + `循环` 开关 (FR-12 同款 44×28 胶囊), 对整列表生效;
   点击行内「回放」→ `POST .../replay {file,speed,loop}`, 成功后蓝边状态行
   `正在回放 <file> · N 倍速 · 数据发给这座桥的串口 (· 循环开着, 回放会一直重复)`;
   非循环按 `时长/倍速+1.5s` 自动收点, 循环保留到关抽屉 (Q2)。请求体经 mock 实测
   `{speed:2,loop:true/false}` 正确上送。
5. **状态生命周期**: 录制是后端长活的, 本地跟踪 (startTs 计时) 关抽屉不丢; 回放状态关抽屉即收;
   桥被删除时两 Map 连带清理。`window.__sh.rec` 增为 QA 观测口。

## 2. 设置弹窗「配置备份」(FR-22)

1. **导出配置**: 先 `POST /api/fleet/export` (契约口径), 失败自动退 `GET` (兼容任务文口径);
   blob → `a.download="fleet.json"` 触发浏览器下载, Playwright 实收下载事件且文件名正确。
2. **导入配置**: 「导入配置」→ 文件选择 → 前端先校验 (JSON 解析失败 / 无 bridges 数组 →
   红字给下一步动作) → 面板回显 `已选 <file> · 里面有 N 座桥` → **合并导入 / 替换导入**
   两枚钮各带两段确认 (3s 自动复原); 替换首点额外亮红色警告框
   「…现有桥配置将被全部替换, 已连的网页会断开」→ `POST /api/fleet/import {mode,json}`。
3. **结果播报**: 响应按 `imported/skipped` (兼容 added/created 等别名, Q5) 解析 →
   「导入完成: 新增 N 座桥, 跳过 N 座 (名字或网址和现有桥相同)」/「现有桥已全部替换, 现在共 N 座桥」;
   导入成功自动关弹窗 + 立即 poll。开关弹窗均重置导入面板。

## 3. UI-0 / UI-1 落点

- **UI-0**: 全部新文案说用户可感知的事 (「把这座桥收发的数据原样存成录像文件」「回放时按几倍
  速度把录像发给串口」「换电脑迁移用」); 报错带下一步动作 (「确认这座桥在运行, 再点一次
  『开始录制』」「请选之前用『导出配置』下载的 fleet.json 文件」); 无实现词 (JSONL/tee/WS 不上脸)。
- **UI-1**: 新控件全部走令牌 —— 页签钮/主录钮/导出导入钮/速度下拉 34 档, 行内回放/删除与
  工具条行 28 档, 圆角一律 `--ctl-radius`(8px), 字号 `--ctl-fs`; Playwright 实测三场景
  (录制页签/设置弹窗/720px) 全部可见控件高度 ∈{28,34}±0.5、新控件圆角=8±0.5, 违例 0。

## 4. Playwright 自验 (mock 契约, 34/34 PASS)

| 场景 | 结果 |
|---|---|
| 录制·空闲 | 文案「开始录制」+红点; 无状态行; 空列表可行动话术 (截图 01) |
| 录制·进行 | 「停止录制」danger 态; 已录 0:03 走表; 帧/字节实时回读 (截图 02) |
| 录制·停止 | 通知「录制完成: file (N 帧 / N 字节)」; 列表刷新 1 行含名/时长/字节; 计数更新 (截图 03) |
| 回放 | 状态行含倍速/循环; 请求体 `{file,speed:2,loop:true/false}` 实测正确 (截图 04) |
| 删录像 | 两段确认 → 列表回 0, mock 侧确认已删 |
| 导出 | 触发下载 fleet.json (文件名实收); 通知大白话; 端点调 1 次 (截图 05) |
| 导入·合并 | 文件选择器→面板回显 2 座桥; 首点确认态; 结果「新增 2 座桥, 跳过 0 座」; mode=merge |
| 导入·替换 | 首点亮红色警告含「现有桥配置将被全部替换」; 确认态; 结果「已全部替换」; mode=replace (截图 06) |
| 720px | scrollWidth=720 无横向溢出; 抽屉全宽; 设置弹窗不破版 (截图 08/09) |
| UI-1 像素 | 三场景高度 28/34 + 新控件圆角 8, 违例 0 |
| console | 预期外 JS 异常 0 |

截图 (`docs/team/reports/dev-sprint13-ui/`): 01-rec-idle / 02-recording / 03-rec-list /
04-replaying / 05-settings / 06-import-replace-armed / 07-drawer-rec-final / 08-720-rec / 09-720-settings

## 5. 契约缺口与后端对齐点 (请 dev-backend/架构师裁决)

- **Q1 删录像端点契约未列**: UI 按 `POST /api/fleet/<id>/recordings/delete {file}` 预实现
  (与 `/api/fleet/<id>/delete` 同风格); 后端定稿若有出入只需改 `armDelRec()` 一处。
- **Q2 回放无停止端点、无状态回显**: 循环回放在 UI 侧没有已知终点 (状态保持到关抽屉)。
  建议后端在 `/api/fleet` 桥对象加 `record`/`replay` 状态字段 (UI 按「有则用」已防御),
  并考虑 `replay/stop`; 否则循环回放只能靠重启桥停。
- **Q3 录制中实时帧数无契约来源**: UI 从 recordings 列表回读兜住 (mock 已验证回填路径);
  后端不回填则显示「停止后统计」, 功能不破但不「实时」。
- **Q4 export 方法两处口径不一** (契约块 POST / 任务文 GET): UI 先 POST 后 GET, 两者都兼容。
- **Q5 import 响应字段未定**: UI 按 `imported/skipped` 解析并兼容别名, 缺失时降级为
  「导入完成」不带计数, 不会报错。
- **出包提醒**: 壳内 UI 是编译期 `include_str!` 嵌入 (dev-sprint12-ui §0 同款), 本次改动
  需随下次构建进壳才会出现在桌面壳里; 浏览器页/`--ui-dir` 直读磁盘即时生效。

---

# 波 2 — FR-20 旁路转发 (TCP) 设置项 + 状态显示 (dev-ui 301, 2026-09-15)

- 变更文件: **仅 `ui/index.html`** (设置页签 1 输入项 + 卡片/抽屉/统计页状态回显 + 配套 CSS/JS;
  未 commit, 未碰 src/tests) · 依据 ADR-24② / spec FR-20 / dev-sprint13-backend.md §6 §9 契约
- 自验: mock 契约控制面 (127.0.0.1:18501; 桥对象 19 字段含 forwardTcp/forwardConnected;
  config 受理 forwardTcp/缺省不改; stop→forwardConnected=false, start 按新值连上) +
  Playwright **36 断言全过**, 预期外页面 JS 异常 0。脚本 `output/sprint13-ui-mock/sprint13_ui_w2_test.cjs`
  (gitignored 不入库); 真后端未起, mock 按已交付后端契约仿真。

## 6. 抽屉「旁路转发」设置项 (FR-20)

1. **输入项**: 设置页签「数据网址」块之下新增「旁路转发」输入 (placeholder
   `留空 = 不转发; 例: 192.168.1.20:9000`), hint 按任务口径大白话:
   「把串口收到的数据同步转发给这个 TCP 地址 (只出不进, 自动重连); 留空 = 关闭转发」。
   fillCfg 回显 `forwardTcp` (非 string 视为空, 旧后端兼容); 可见 label 取 4 字「旁路转发」
   (72px 标签列放不下「(TCP)」, TCP 字样进 title 与 hint, UI-1 列宽不破)。
2. **保存路径**: 完全沿用现有 saveCfg —— 每次保存显式携带 `body.forwardTcp`
   (后端口径「缺省不改」, 空串必须显式送才能关); 校验不过不发请求。
   锁定口径与 F3 同拍: cfForward 加入运行中锁定列表 (与串口参数同一时机) ——
   改转发 = 抽屉「停止」解锁 → 编辑 →「保存并启动」(stop→config→start 一次完成);
   停止态保存只发 config, 启动时后端按新值起会话。
3. **格式校验** (`validateForward`): 空=关; 非空须 host:port (lastIndexOf(':') 切分)、
   host 非空、端口 1~65535、无空白 —— 与后端校验同口径; 就地红字沿用 .ferr 模式
   (cfErrForward + input.invalid + focus), 键入即撤红。

## 7. 转发状态回显 (forwardConnected)

1. **桥卡片**: 速率行新增「转发 已连接/未连接」微标 (已配置才出现, 未配置不显示);
   已连接=绿 / 未连接=琥珀, 颜色走状态令牌; title 带目标地址与可行动话术。
2. **抽屉设置页签**: 输入项下方状态行: 已连接=「转发已连接 — 正在把串口收到的数据发给 <目标>」;
   运行中未连接=「转发未连接 — 会自动重连, 请检查 <目标> 那头的服务开没开」;
   桥停止=「转发已配置 — 这座桥启动后开始转发」(停止态不谎报"会自动重连")。
3. **抽屉统计页**: stParam 参数行尾追 `· 旁路转发 <目标> (已连接/未连接)`。
   三处均随 500ms 轮询原位联动, mock 实测 已连接↔未连接 切换即时跟随。

## 8. 决策注: cfForward 锁定口径 (回应任务项 1)

- 「运行中桥保存时自动 停止→应用→启动」按 Sprint12 F3 既有语义落地: 运行态字段
  (含转发)照旧锁 + 抽屉「停止」解锁 → 编辑 →「保存并启动」; Playwright 实测
  序列 stop→config→start、config.forwardTcp 正确上送。
- 若要「运行中直改转发不停车」需把保存钮从 F3 锁定表摘出 (动 QA 已验收的运行态行为),
  本次未取 —— 后端运行中热生效不受影响 (CLI 等直发 config 场景仍可用);
  架构师若裁定 UI 也要热改捷径, 只需动锁定表一处 + 抽屉 hint 文案。

## 9. Playwright 自验 (mock 契约, 36/36 PASS)

| 场景 | 结果 |
|---|---|
| 未配置基线 | 卡片微标/抽屉状态行均不显示; 输入框空 (截图 10) |
| 校验红字 | "abc"→「主机:端口」红字红框; 70000→端口范围红字; 键入即撤; 校验不过 0 请求 (截图 11) |
| 停止态保存 | config 请求体 forwardTcp=192.168.1.20:9000; 只发 config 无 stop/start; 卡片+抽屉=未连接琥珀「启动后开始转发」(截图 12) |
| 清空=关 | forwardTcp="" 上送; 状态回隐 (未配置不显示) |
| 运行中已连接 | 卡片「转发 已连接」绿; 抽屉状态行+目标地址回显; 输入框照旧锁 (F3) (截图 13/14) |
| 运行中改目标 | 「停止」解锁→「保存并启动」→ stop→config→start; config.forwardTcp=10.0.0.5:7000; 状态联动新目标 |
| 断开态 | forwardConnected=false → 抽屉「转发未连接 — 会自动重连」+ 卡片琥珀未连接 (截图 15) |
| 统计页 | 参数行含「旁路转发 10.0.0.5:7000 (未连接)」 |
| UI-1 像素 | cfForward 34 档/圆角 8; 设置页签全部控件高度 ∈{28,34} 违例 0 |
| 720px | scrollWidth=720 无横向溢出 (截图 16) |
| console | 预期外 JS 异常 0 |

截图 (`docs/team/reports/dev-sprint13-ui/`, 编号接波 1): 10-fwd-field / 11-fwd-err /
12-fwd-configured-stopped / 13-fwd-card-connected / 14-fwd-drawer-connected /
15-fwd-disconnected / 16-720-fwd

- **出包提醒** (同波 1): 壳内 UI 是 include_str! 嵌入, 本次改动需随下次构建进壳才会出现在
  桌面壳里; 浏览器页/`--ui-dir` 直读磁盘即时生效。

---

# 对齐修复轮 — 「旁路转发」组间距/缩进归位 (dev-ui 301, 2026-09-15)

- 变更文件: **仅 `ui/index.html`** (3 处 CSS, 未碰结构/文案/JS/src/tests) · 未 commit
- 依据: 用户截图反馈「旁路转发一行与同表单其他字段行不齐」· 基准 = 同抽屉 cf 表单既有栅格

## 10. 根因 (Playwright 实测, 非目测)

对 cf 表单逐行实测 `getBoundingClientRect` (1120×780 = 桌面壳默认窗, 桥已配转发运行态):

- **字段行与 hint 本来就在栅格上**: 旁路转发 label 左缘/宽度/控件左缘 (634/72/714) 与
  波特率、网址端口等完全同列; 行距 field→hint→下行 = 8/0/8px, 与数据网址块同拍。
- **出格的是组内状态行 `.fwd-stat`** (「转发未连接 — …」): 全表单唯一 80px 缩进 + `-2px`
  负上边距的文本行 —— 夹在 hint (label 左缘) 与 lock-hint (label 左缘) 之间, 左缘 634→714→634
  三行连着三个基准; 474px 宽被 80px 缩进挤到 394px, 目标地址 `192.168.1.20:9000` 被断成
  「…900/0」两截、状态文字拖成三行 —— 即用户看到的"明显是后加的"。
- `.ferr` (校验红字框) 的 80px 缩进**有意保留**: 输入框随身报错语义, 与新建桥弹窗
  (npErrName 等) 同模式且 QA 已验收, 不属本组常态视图。

## 11. 修复 (同栅格、同 gap、同缩进)

1. `.fwd-stat` margin `-2px 0 8px 80px` → `8px 0`: 全宽落 label 左缘, 行距走表单 8px 节奏
   (hint/lock-hint/rec-live 等同抽屉状态行同款全宽基准)。
2. `.fwd-stat b` 补 `word-break:normal;overflow-wrap:anywhere`: 转发目标地址整段走,
   放不下才让位, 不再从 IP 中间拆断。
3. 删 720px 媒体查询里 `.fwd-stat{margin-left:68px}` 孤儿覆盖 (基准改全宽后不再需要)。

修复后实测 (1120 运行态): hint / fwd-stat / lock-hint 左缘 634/634/634, 行距 8/8px,
状态行单行 19px (前: 36px 三行); 720px 窄屏同拍 (label 60px 档, 左缘 14/14/14)。

## 12. 自验

- **tools/ui_pixel_audit.js 复跑全绿**: PASS — 168 可见控件 × 7 轮, 高度 ∈{28,34}±0.5、
  圆角 = 主题 --ctl-radius ±0.5, 违例 0 (R3 抽屉·设置 37 控件含 cfForward)。
- 截图 (`docs/team/reports/dev-sprint13-ui/`, 1120 = 桌面壳默认窗尺寸):
  `forward-align-before-1120` (修复前运行态) / `forward-align-after-1120` /
  `forward-align-after-1440` (审计标准宽) / `forward-align-after-720` (窄屏) /
  `forward-align-compare` (前|后并排)。
- 出包提醒: 已 `cargo build --release` 让改动进壳验证过; 桌面壳需随下次发版重打。
