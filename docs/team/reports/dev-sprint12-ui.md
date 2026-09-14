# Dev Sprint 12 报告 — BUG-2 建桥参数记忆加固 + BUG-3 波特率自绘下拉 (dev-ui 301)

- 日期: 2026-09-14 · 变更文件: **仅 `ui/index.html`** (openNew 回填/两个波特率输入框/CSS/增强脚本/取消监听五处; 未 commit)
- 依据: 用户真机壳内实测反馈 (附截图): BUG-2 新建桥波特率框为空; BUG-3 波特率框右下角
  number spinner 与 datalist 箭头挤在一处。UI-0 全程遵循; 未碰 src/tests
- 自验: mock 管理台 (127.0.0.2:8123, 桥 8081 + COM1/COM9; UI 从磁盘直读当前工作区代码) +
  Playwright **63 断言全过**, 全程预期外 JS 错误 0; 脚本在 %TEMP%\s12-mock.mjs / s12-test.mjs (不入库)

## 0. 根因取证 (BUG-2 "空波特率")

- 壳的 UI 是**编译期嵌入**: `src/service.rs:179` 与 `src/fleet.rs` 均 `include_str!("../ui/index.html")`
  —— 真机壳跑的是**构建当时**打进去的 UI 快照。当前 HEAD (f8f94ee) 的回填逻辑已带逐字段校验,
  用户实测到的空框来自更早的构建产物。**修复后需重新出包, 壳内才会生效。**
- 本次仍按任务规格把现网代码彻底加固 (脏数据/未来回归双保险), 见下。

## 1. BUG-2 修复: 回填字段级防御

1. **默认钉底**: 新增 `applyBridgeDefaults()` (115200/8/N/2/none), 每次打开表单先重钉
   (dialog 重开不重置表单)。
2. **字段级回填** `restoreLastBridge(last)`: 波特率只认 `[110,2000000]` 内的**有限数字**
   (字符串一律不收, 否则保留默认 115200); 数据位/校验/停止位/流控只收选项集内的字符串/数字;
   **一个字段坏不连坐其他字段**。
3. **整段 try**: 回填 + 串口预填包 try, 任何异常按无记忆处理再钉一次默认。
4. **终审兜底**: 打开表单最后一步检查 `npBaud` 非空, 空则强制 115200 —— 任何情况不允许空。
5. **串口**: 只在扫描列表里仍存在才选回 (fillPortSelect 的 cur 裁决, 设备拔了落回列表默认);
   回填提示 (`#npRestoreHint`) 仅在确有字段回填时出现。
6. 设置抽屉 (cf*) 不参与记忆, 保持不动。

## 2. BUG-3 修复: 波特率输入换自绘下拉

1. **藏 spinner**: `.baud-in` 上 `::-webkit-outer/inner-spin-button` display:none +
   `appearance:textfield` (Chromium≥111 靠后者抑制; 伪元素 computed style 是遗留怪癖, 以视觉为准)。
2. **去 datalist**: 删除 `<datalist id="baudList">` 与两处 `list` 属性。
3. **自绘下拉**: 输入框右缘 28px 幽灵 chevron (`aria-haspopup="listbox"` + `aria-expanded`,
   与 .copy-ghost 同族: 无框无底、muted → hover/展开 accent); 点击/聚焦弹出常用波特率菜单
   (9600~2000000 十项, 打开时全量显示); 输入数字即过滤 (无匹配显示提示行, 关着时不弹空菜单);
   ↑↓ 移动高亮 (首项默认高亮) + 回车选中, Esc 只关菜单不关弹窗 (keydown 拦截 + cancel 事件
   `baudMenuEscAt` 250ms 守卫双保险), 失焦/点外关闭; 选中后程序性派发 input 事件
   (去红框/收错误行/刷新抽屉等价命令照常跑)。
4. **设计令牌**: 菜单用 --panel/--border-strong/--ctl-radius/--ctl-fs/--accent 等, 深/浅主题自动翻转。
5. **同族推广**: 抽屉 cfBaud 复用同一 `enhanceBaudInput()` (同一痛点, 不留半拉子); 无外部依赖。

## 3. Playwright 自验 (mock, 63 断言全过)

| 场景 | 结果 |
|---|---|
| BUG-2 无历史 | 六字段=默认 115200/8/N/2/none, 无提示; FR-17 预填照常 (127.0.0.1:8124) |
| BUG-2 正常回填 | COM1/9600/7/E/1/xonxoff 全回填 + 提示可见 |
| BUG-2 串口拔了 | 不选回 COM99 落回 COM1, 其余照常回填 |
| BUG-2 脏数据不连坐 | 全脏→全默认不写空; 部分脏→好字段回填坏字段落默认; 垃圾 JSON/越界/baud=0→默认 |
| BUG-2 任何情况非空 | 每分支断言波特率非空且在 [110,2000000] |
| BUG-2 建桥写入回归 | 建桥成功写入六元组 + 再开回填, 新代码未破坏 saveLastBridge 路径 |
| BUG-3 结构 | datalist 已删 / 无 list 属性 / spinner 藏掉 (含裁剪截图) / chevron+aria 就位 |
| BUG-3 交互 | 点击开 10 项+首项高亮; 输 92→[19200,921600]+回车选中; ↓↓→38400+回车; Esc 关菜单弹窗仍在; 失焦关; 点项选中 |
| BUG-3 抽屉 | cfBaud 同组件, 选 57600 后等价命令跟随 `--baud 57600` |
| 布局 | 720px 无横向滚动, 菜单宽度贴合输入框 |
| 主题 | 浅色默认全程 + 深色 (sh_theme=dark) 菜单随令牌翻转, 断言非白底 |
| console | 预期外 JS 错误 0 |

截图 (`docs/team/reports/dev-sprint12-ui/`):
`bug2-default-115200.png` · `bug2-restore-9600-7e1.png` · `bug2-stale-port-not-restored.png` ·
`bug2-dirty-no-cascade.png` · `bug3-spinner-hidden-crop.png` · `bug3-menu-open.png` ·
`bug3-filter-92.png` · `bug3-keyboard-38400.png` · `bug3-drawer-cfbaud-57600.png` ·
`layout-720-dialog.png` · `theme-dark-dialog-menu.png`。

## 4. 边界与留给 QA

- **必须重新出包**才能在真机壳验证 BUG-2/BUG-3 (UI 编译期嵌入, 见 §0); 本轮自验基于磁盘直读。
- 自绘菜单固定向下弹出, 抽屉滚动容器内极端贴底时可能被裁剪 (弹窗场景无此问题); 按需再做翻转。
- 菜单过滤为"包含数字子串" (如 92 → 19200/921600); 手输任意值仍走既有 validateBaud 校验。
- 并行说明: 工作区里 `src/fleet.rs`、`dev-sprint11-backend.md` 的未提交改动为后端同事所改,
  本任务未触碰。

---

# Sprint 12 第二轮 — 用户实测回归修复 (F1 串口没记住 / F2 波特率不自由 / F3 设置抽屉流程)

- 日期: 2026-09-14 · 基于 v1.7.1 (f2e95a9) · 变更文件: **仅 `ui/index.html`** (+本节与截图); 未 commit
- 依据: 用户对 v1.7.1 真机实测三条反馈; UI-0 文案 / UI-1 尺寸令牌 (新钮走 28 紧凑档) 遵循,
  数据面契约零变化, 未碰 src/tests
- 自验: **真程序** (debug 构建, `--headless --no-fleet --addr 127.0.0.1:8099`, 桥建在 COM2) +
  Playwright **25 断言全过**, 页面 JS 错误 0, 720px 三屏 (主屏/抽屉/弹窗) scrollWidth≤720 无破版;
  脚本 `output/sprint12-ui-test.js` (output/ 不入库)

## 5. F1 修复: 建桥表单串口回填 (根因 = 回填时机早于扫描列表)

- **根因取证**: openNew 把记忆值 `$("npPort").value = last.port` 写在扫描列表填好之前 ——
  options 里没有 COM2, select 赋值静默落空; 随后 scanInto 抓走的 `cur` 已是空串,
  fillPortSelect 无值可选回。波特率是同步赋值所以记住了, 串口是异步列表所以丢了。
- **修法**: `scanInto(sel, curHint)` 增加显式回填参数, openNew 把 `last.port` 直递进去;
  选回裁决仍在 fillPortSelect (扫描列表里还在才选回, 设备拔了自然落回默认)。删除两处无效赋值。
- 验收: 建桥 COM2/9600 → 成功 → 重开表单 → 串口=COM2 且波特率=9600 + 回填提示可见
  ✓ (`f1-backfill.png`)

## 6. F2 修复: 波特率自由键入 + 下拉并存 (列表只是快捷方式)

- 现状痛点: v1.7.1 菜单"首项默认高亮 + 回车即选"会**偷换键入值** (键入 1152 回车 → 变 115200);
  预设外值 (1500000) 回车则菜单僵在"无匹配"不动 —— 即用户说的"过滤列表式"。
- **修法**: 新增 `navigated` 状态 —— 只有 ↑↓ 显式移动过高亮, 回车才替入该项; 否则回车 = 确认
  键入值, 仅收起菜单。键入过滤辅助/点选填入/Esc/失焦行为不变; 绝无白名单拦截,
  [110,2000000] 内任意数字直接就是最终值 (提交仍走既有 validateBaud 终检)。
- 验收: 键入 1500000 → 回车/失焦均保持 1500000 ✓; 键入 1152 回车保持 1152 (不被高亮项偷换) ✓;
  下拉点选 230400 = 填入 ✓; ↑↓ 后回车选定高亮项 19200 ✓ (`f2-typing-nomatch.png` /
  `f2-dropdown-open.png`)

## 7. F3 实现: 设置抽屉的停止/启动 + 「保存并启动」一次完成

- 抽屉头部 (标题条) 新增 `#drToggle` (紧凑档 28px, UI-1), 文案/悬浮提示与卡片同源 (UI-0),
  随既有 500ms 轮询实时同步 (头部钮 / 徽章 / 卡片钮三处一致)。
- 打开抽屉时记 `drawerRunAtOpen`: 运行中 → 字段照旧锁定 + 头部显示「停止」, 点击即停
  (等价卡片停止, 复用 toggleBridge) 并解锁编辑; 保存钮文案「保存并启动」→ 保存 =
  **stop → config → start 一次完成** (全部走既有公开 API, UI 只是代跑, 数据面契约零变化)。
  原本停止 → 现行为 (保存, 手动启动, 文案「保 存」)。
- `cfgSaving` 在途标记: 停→存→启期间冻结抽屉锁定态 (防半程轮询解锁被编辑/重复提交), 保存钮在途禁用。
- 顺带修掉同一保存路径上的两个存量缺陷:
  1. **抽屉「保存」自 Sprint 4 起必然失败**: saveCfg 一直携带 `listen`, 而后端对 config 里的
     listen 一律拒收 (FR-10f; curl 实证 400 "桥数据端口生命周期内不变")。改为不发送该字段;
     用户真改了网址就就地说明 "这座桥的数据网址建好就不能改; 要换端口请删除这座桥再新建", 不静默吞。
  2. **toggleBridge 发完 poll 不等待**: 停止后立刻开抽屉会读到旧相位 (实测复现: 卡片已停,
     抽屉仍按"运行中"给出「保存并启动」)。改为 `await poll()` 落地再返回; saveCfg 同样处理。
- 验收三态: 运行态 (字段锁 + 头部「停止」+ 「保存并启动」) ✓ → 抽屉点「停止」解锁, 卡片同步变「启动」 ✓
  → 改 19200 保存: 服务端恰好转一次 stop/config/start, baud=19200 且恢复运行 ✓ → 关抽屉、卡片停止、
  重开抽屉: 「保 存」保存后仍停 ✓; 改数据网址被就地拦下并说明 ✓ (`f3-drawer-running.png` /
  `f3-saved-restarted.png` / `f3-stopped-save.png`)

## 9. 边界与留给 QA (第二轮)

- **重新出包**后壳内才生效 (UI 编译期嵌入, 同 §0)。
- 自验时 COM1/COM2 曾被用户在跑的 v1.7.1 实例 (8090) 占用, 前几轮桥走 retry (控制面断言不受影响);
  末轮该实例让出 COM2, 测试桥真实 open (phase=open), 全序列含数据面打开验证。用户实例全程未动。
- F3 全程只调既有 POST /api/fleet/<id>/stop|config|start, 后端零改动; 侦听/统计/等价命令等
  抽屉其余能力未动。
- F2 菜单贴底裁剪问题沿用 §4 未回归; 抽屉内 cfBaud 与弹窗 npBaud 同组件同行为。

---

# Sprint 12 第三轮 — 视觉修复: 抽屉头部按钮成对 + 全站按钮档位审计

- 日期: 2026-09-14 · 变更文件: **仅 `ui/index.html`** (一行: `#drClose` 加入紧凑档规则) (+本节与截图); 未 commit
- 依据: 用户实测「设置抽屉里的「停止」钮和「关闭」✕ 尺寸不一致, 看着突兀」
- 自验: 真程序 debug 构建 (8099) + Playwright 实测全站可见按钮 46 颗 × 高/圆角/字号/内边距;
  官方像素审计 `tools/ui_pixel_audit.js` **PASS (160 控件 × 7 轮全绿, 含深色)**

## 10. 修复: #drClose 34px → 28px (与 #drToggle 成对)

- **实测取证 (修复前)**: `#drToggle`=28px、`#drClose`=34px 同排 —— drClose 当初没进紧凑档
  规则 (无 class, 吃了 button 基础 34 档), 同排不同高即用户所说的突兀。
- **修法**: 紧凑档选择器组追加 `#drClose` (28px 档: 同高/同 12px 内边距/同 8px 圆角/同 13px 字号)。
  修复前后特写: `drhead-before.png` / `drhead-after.png` (720px 复验: `drhead-after-720.png`,
  scrollWidth=720 无破版)。

## 11. 全站按钮档位归档表 (修复后实测, 仅列唯一元素; 幽灵族=无框透明图标钮)

| 场景 | 元素 → 文案 | 档位 | 实测高 | 圆角 | 字号 |
|---|---|---|---|---|---|
| 页头 | #btnOpenPanel 打开面板 / #btnSettings 设置 / #btnQuit 退出程序 | 标准 | 34 | 8px | 13px |
| 页头 | #btnCopyUrl 复制网址 (幽灵族) | 紧凑 | 28 | 8px | 13px |
| 主区 | #btnNew / #btnNew2 新建桥 | 标准 | 34 | 8px | 13px |
| 卡片 | .act-toggle 启停 / .act-cfg 设置 / .act-del 删除 | 标准 | 34 | 8px | 13px |
| 卡片 | .bname 桥名 (无框链接样式) | 标准 | 34 | 8px | 13px |
| 卡片 | .copy-ghost 复制网址 (幽灵族) | 紧凑 | 28 | 8px | 13px |
| 抽屉头部 | **#drToggle 停止/启动 = #drClose ✕关闭 (本轮成对)** | 紧凑 | **28/28** | 8px | 13px |
| 抽屉 | #tb-cfg/tap/stats 页签 (data-strip, 圆角由容器承载) | 标准 | 34 | 容器 8px | 13px |
| 抽屉 | #btnCfScan 扫描 / #btnCfSave 保存(并启动) | 标准 | 34 | 8px | 13px |
| 抽屉 | #btnDrCopyUrl / #btnCfCopyUrl / #btnCfCopyCmd / .baud-ghost (幽灵族) | 紧凑 | 28 | 8px | 13px |
| 侦听工具条 | #btnPause 暂停滚动 / #btnTs 时间戳 / #btnClear 清空 | 紧凑 | 28 | 8px | 13px |
| 侦听工具条 | #viewAscii / #viewHex (.seg data-strip, 圆角由容器承载) | 紧凑 | 28 | 容器 8px | 13px |
| 新建桥弹窗 | #btnNpScan 扫描 / #btnCancelNew 取消 / #btnCreate 创建并启动 | 标准 | 34 | 8px | 13px |
| 新建桥弹窗 | #btnDlgX ✕ (幽灵族) / .baud-ghost (幽灵族) | 紧凑 | 28 | 8px | 13px |
| 设置弹窗 | #btnSetCancel 取消 / #btnSetApply 应用 | 标准 | 34 | 8px | 13px |
| 设置弹窗 | #btnSetX ✕ (幽灵族) | 紧凑 | 28 | 8px | 13px |

- 断言结果: 全站可见按钮高度**只出现 28/34 两档** (0 违例); 同排实心按钮同档 ——
  页头三钮全 34 (btnQuit 有显式 34 覆盖, 与邻居同档)、卡片三钮全 34、抽屉头部两钮全 28 (本轮修复点)。
  幽灵族 (复制/✕/波特率 chevron) 一律 28 无框, 是刻意的轻量家族, 不与实心钮比重量。
- 官方审计: `tools/ui_pixel_audit.js` PASS —— 160 控件 × 7 轮 (R1 仪表盘…R6 设置弹窗,
  R7 深色主题) 高度 ∈ {28,34}±0.5px、圆角=主题 --ctl-radius±0.5px 全绿; 全站截图在
  `output/playwright/ui-audit-*.png`。
