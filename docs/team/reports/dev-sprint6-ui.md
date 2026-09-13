# Dev Sprint 6 报告 — 控件尺度统一 (UI-1) + 重连开关 (FR-12) (dev-ui 301)

- 日期: 2026-09-13 · 变更文件: **仅 `ui/index.html`** (样式/模板/脚本三处; include_str 内嵌不变, 未 commit)
- 依据: ADR-16 (①FR-12 autoReconnect / ②UI-1 令牌) · spec UI-1+FR-12 · backlog「Sprint 6」dev-ui 条目 · UI-0 自明性
- 自验: mock 控制面 (按 fleet.rs 契约 15 字段喂桥对象, 未入库) + Playwright, **33 项断言全过**, 0 console 错误; 720px 无横向滚动; 测完 mock 进程已停、临时脚本目录已删; 未 commit; 未碰 src/tests; COM8 禁碰未碰

## 1. 任务 A — UI-1 控件尺度归档

1. **令牌入 :root**: `--ctl-h:34px` (标准档) / `--ctl-h-sm:28px` (紧凑档, 仅工具条/标题条/侦听工具条内小按钮) / `--ctl-radius:8px` / `--ctl-px:12px` / `--ctl-fs:13px`。全局 `button` 改定高 + flex 居中 + nowrap, `select/input[text|number]` 改定高; 高度由 height 精确控制 (box-sizing 已全局 border-box), 不再靠 padding 凑。
2. **全站归档** (方法: Playwright 遍历可见 `button,input,select` 逐个取 `getBoundingClientRect().height` + computed font-size/border-radius, 5 个视口场景实测, 见 §4 表): 全部 34 或 28, 字号全部 13px, 圆角全部 8px。允许例外照任务: 流程图 SVG、火花线、终端区、徽章 pill (非交互) 不动。
3. **两处特意裁定**:
   - `.bname` (卡片名) 语义是 button 且可点开抽屉, 不属豁免 —— 从「15px 无定高」归入**标准档 34/13px/600 加粗** (可点区域随之变大, 层级靠字重维持);
   - 流程图三节点改 `min-height:50px` 等高 (网址节点的 28px 复制小按钮不再撑高单节点), `seg`/`dr-tabs` 属组合条带: 自身 radius 0, 8px 由容器外廓承载 (实测表注明)。
4. **视觉层级 + 间距**: 主操作实底 (新建桥×2 / 创建并启动 / 保存 / 启动中按钮), 其余描边; 消息盒/统计卡/等价块等容器圆角同步 8px; 间距归 4 的倍数 (`--gap` 10→12, 卡片/行 gap 10→12, 6→8, 5→4, 弹窗底 14→16 等); 不加新颜色不加阴影, 开关仅用现有 ok 绿/灰底。顺手补上 `#btnPause/#btnTs` 按下态无视觉的旧账 (`term-toolbar>button.active` 描边高亮, FIX-22 同类小刺)。

## 2. 任务 B — FR-12 重连开关

1. **开关本体**: 新建桥弹窗 + 每桥设置抽屉各一枚「自动重连」开关 (`input[type=checkbox].switch`, role=switch, input 本体即 34px 轨道 —— 归 UI-1 审计不豁免), 默认开; 小白 hint 用任务钦定原文: 「开着: 串口断了会自动连回来; 关掉: 串口断了就直接停止, 需要手动重新打开」。
2. **契约往返**: 建桥 body 带 `autoReconnect` (mock 实测 false); 设置保存走 `POST /api/fleet/<id>/config` body.autoReconnect (每次保存显式携带, 缺省 true); 抽屉回显 `b.autoReconnect` (字段缺失按 true 显示, 防契约回退); 等价命令照 FIX-16 零失真惯例**始终显式携带** `--reconnect`/`--no-reconnect`。
3. **锁定语义**: 开关随抽屉表单同一时机锁定 (运行中不可改, 与串口参数一致; 后端 hub 其实支持即改即生效, 如 UX 要求运行中可关, 波2 解锁一处即可)。fillCfg/syncDrawerLive/删除清理路径均覆盖。
4. **卡片大白话** (契约: 桥对象有 autoReconnect): closed + lastError=「串口已断开 (自动重连已关闭)」(supervisor.rs 原话) 时, 错误行显示「**串口已断开, 自动重连已关闭; 点「启动」可重新打开**」, 后端原话以次级灰字保留; 防御: 若出现 retry+autoReconnect=false (契约上不应发生), 按停机显示 (徽章「已停止」/按钮「启动」/无重连闪动), 不谎报「正在自动重连」。

## 3. 新增文案 (UI-0 过审: 0 实现词)

| 文案 | 位置 | 说明 |
|---|---|---|
| 自动重连 | 开关可见标签 (弹窗+抽屉) | 悬浮题为「串口断开自动重连」 |
| 开着: 串口断了会自动连回来; 关掉: 串口断了就直接停止, 需要手动重新打开 | 开关下方 hint | ◆任务钦定原文, 弹窗/抽屉同文 |
| 串口已断开, 自动重连已关闭; 点「启动」可重新打开 | 卡片错误行 (autoOff 停机) | ◆任务「已停止 (自动重连已关闭)」类大白话, 加可执行动作 |
| (lastError 原话, 如「串口已断开 (自动重连已关闭)」) | 同上次级灰字 | 展示后端原话不翻译 |

Sprint5 文案零改动 (重连中/第 n 次/串口已恢复等回归实测不变)。

## 4. 高度归档表 [元素 → 档位 → 实测像素] (Playwright 实测, 全部 fs=13px)

**标准档 34px**: `#btnNew`(＋新建桥) · `#btnNew2` · `.bname`(卡片名) · `.act-toggle`(启动/停止, 含 primary 态) · `.act-cfg`(设置) · `.act-del`(删除) · `#btnNpScan`/`#btnCfScan`(扫描) · `#npName` `#npBaud` `#npListen` `#cfName` `#cfBaud` `#cfListen` `#cfMax` (input) · `#npPort` `#npData` `#npParity` `#npStop` `#npFlow` `#cfPort` `#cfData` `#cfParity` `#cfStop` `#cfFlow` (select) · **`#npAuto`/`#cfAuto` (重连开关, 轨道 34)** · `#btnCancelNew` · `#btnCreate`(primary) · `#btnCfSave`(primary) · `#drClose`(✕ 关闭) · `#tb-cfg`/`#tb-tap`/`#tb-stats` (页签, 组合条带自身 radius 0)

**紧凑档 28px**: `#btnCopyUrl`(urlchip 复制) · `#btnQuit`(退出程序) · `#btnDlgX`(弹窗✕) · `#btnDrCopyUrl` · `#btnCfCopyUrl` · `#btnCfCopyCmd`(复制小按钮) · `.mini-copy`(卡片流程图网址复制) · `#viewAscii`/`#viewHex` (seg, 组合条带自身 radius 0) · `#btnPause` `#btnTs` `#btnClear` (侦听工具条)

**实测核验**: 5 场景 (主视图/弹窗/抽屉设置/抽屉侦听/720px 主视图+抽屉) 共 13~32 个可见控件逐个断言 `height∈{28,34}` 且 `font-size=13px` 且 `border-radius=8px` (组合条带白名单除外), 0 异常。豁免未测: 流程图 SVG、火花线、终端区、徽章 pill (非交互)。

## 5. 自验 (mock + Playwright, 33/33 PASS)

mock 按 fleet.rs 实际契约喂 15 字段桥对象 (含 autoReconnect/retries), 最小 WS 握手受理 tap; 断言覆盖:
- **UI-1**: 上述 5 场景高度/字号/圆角审计全过; 720px 主视图/抽屉/弹窗 `scrollWidth≤clientWidth` 无横向滚动。
- **FR-12 往返**: 弹窗默认开 → 关掉建桥 → `POST /api/fleet` body.autoReconnect=false; 新卡回显; 抽屉 false→勾选→等价命令 --reconnect→保存 `POST /config` autoReconnect=true→重开回显 true; tap WS 正常 (无连接报错噪音)。
- **卡片大白话**: autoOff 停机文案+原话灰字+徽章「已停止」+按钮「启动」; 防御态 retry+autoOff 不出现重连字样; 回归 retry+autoOn「正在自动重连 (第 3 次)…」+闪动原样。
- **卫生**: 0 console/page 错误。

截图 (docs/team/reports/dev-sprint6-ui/): 01-dashboard (两桥+autoOff 大白话) · 02-new-dialog (开关默认开) · 03-drawer-cfg (开关+hint+等价命令) · 04-720-dashboard · 05-720-drawer · 06-retry-autoon (Sprint5 回归)。

## 6. 契约使用与注记

- `autoReconnect` 按 ADR-16①/FR-12 消费: 缺失按 true (默认开, 防旧契约回退); 显示层不解释后端语义, 只透传开关与回显。
- 错误行 autoOff 判定依赖 lastError 含「自动重连已关闭」(supervisor.rs 既有原话); 若后端日后改文案或加结构化字段, 只动 `stoppedByAutoOff` 一处。
- 弹窗/抽屉 hint 与卡片文案均为页内一次性呈现, 不新增弹窗。
- dev-backend 波1 已落地 (src 内 --reconnect/--no-reconnect + 15 字段回显均在), 本轮为 mock 契约自验, 真机复验留集成验收 (COM8 禁碰未碰)。

## 7. 修复轮 D2 (qa-sprint6-plan.md 审计 13/107 违例, 2026-09-13)

两处违例修复, **审计实测 PASS — 0 违例** (`tools/ui_pixel_audit.js` 真后端 COM1 开桥五轮:
105 个可见控件 × 5 轮高度∈{28,34}±0.5 全过 + 4 个条带容器圆角实测 8px; 107→105 因两枚开关
input 改视觉隐藏, 按 QA 口径 "opacity:0 记 skipped"):

1. **D2①开关重造**: `input.switch` 保留语义 (role=switch/checked→aria-checked/键盘可达) 但视觉隐藏
   (1×1, opacity:0, pointer-events:none); 轨道由相邻 `label.switch-track` 承载 —— 苹果式
   **44×28 (紧凑档高) 胶囊轨道 + 22px 滑块动画**, 勾选=ok 绿、锁定=0.45 透明度、焦点环
   (`.switch:focus-visible + .switch-track`)。建桥 body/config 保存/回显/等价命令往返 mock
   27/27 复验不变。
2. **D2②条带圆角**: 页签 `.dr-tabs` 补 `border-radius:8px + overflow:hidden`, 与 `.seg` 一并加
   `data-strip` 标记; 审计脚本同步识别 (条带内子元素免圆角断言、高度断言不豁免; 容器自身
   四角必须 8±0.5 —— 严格性不降, 脚本头部注明口径)。

截图 02/03/05 已刷新 (新开关形态); 01/04/06 未涉改动沿用。未 commit; COM1 审计桥由工具自管,
COM8 未碰。

## 8. 下一步

- QA 像素审计套件 (backlog Sprint6 qa 条目) 可直接复用 §4 白名单 (组合条带 radius 0) 与豁免清单。
- 若 UX 走查要求「运行中也可关自动重连」: 解锁 syncDrawerLive 锁名单里的 cfAuto + 保存按钮门控, 两处小改。
- 34/28 两档在 WebView2 的实际观感 (桌面壳) 建议人工过一眼 01/03 截图。
