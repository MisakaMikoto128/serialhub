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
