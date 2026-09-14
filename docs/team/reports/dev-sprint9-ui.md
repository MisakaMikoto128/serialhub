# Dev Sprint 9 报告 — 复制图标化+微型气泡 / 打开面板改调端点 (dev-ui 301)

- 日期: 2026-09-14 · 变更文件: **仅 `ui/index.html`** (CSS 4 处 / HTML 6 处 / JS 4 处; 未 commit)
- 依据: ADR-21①② · backlog「Sprint 9」dev-ui P0 · UI-0/UI-1
- 自验: mock 管理台 (127.0.0.1:8117, Playwright) **16/16 PASS** + 像素审计双跑全 PASS;
  全程未碰真实 8080 服务 (审计用真后端绑 8091)、未碰 COM8; mock/审计脚本在
  %TEMP%\sprint9-ui-test.cjs / sprint9-pixel-audit.cjs (不入库); 未 commit; 未碰 src/tests

## 1. 复制动作图标化 (ADR-21①)

1. **图标钮**: 全站 5 颗「复制」文字钮 (页头 `btnCopyUrl` / 桥卡片 mini / 抽屉
   `btnDrCopyUrl` / 抽屉设置页 `btnCfCopyUrl` / 命令行 `btnCfCopyCmd`) 全部改为
   **`.icon-btn` 图标钮**: 28×28px 紧凑档 (`--ctl-h-sm`) + 8px 圆角 (`--ctl-radius`),
   内联 SVG 两叠圆角矩形 (Feather「copy」同款, 24 viewBox → 15px, stroke=currentColor,
   不引外部库), `padding:0` + flex 居中; hover 沿全站钮规则提亮 (边框+图标转 `--accent`);
   `title="复制"` + `aria-label` 保留语义 (如「复制控制台网址」)。图标由 JS 单源
   (`COPY_ICON`) 注入 5 处, 模板卡内那颗开盖即盖, 改图标只动一处。
2. **文字大按钮清零**: `copy-btn` / `mini-copy` 类与「复制」字样全站移除
   (grep `copy-btn|mini-copy|>复制<` = 0); 原 34px 页头复制钮并入 28 档
   (`#btnCopyUrl` 从 `--ctl-h` 组摘除), 网址条 (urlchip) 随之变矮更紧凑。
3. **微型气泡「已复制」**: 全站共享一个 `#copyTip` (fixed, `role=status aria-live=polite`),
   点任意图标钮后在**该钮上方**弹出 (顶边放不下自动挪下方, 横向夹在视口内不越界),
   深浅取 `--ink`/`--bg` 配对反色随主题翻转, 失败显示红底「复制失败」;
   **~1.5s 自散, 重触发即重定位重计时** —— 同一时刻全站最多一个, 不堆叠 (实测连点:
   首点 1.1s 后点第二颗, 又过 1.4s 仍显示且已挪到第二颗旁, 再 1.3s 散)。
4. `copyText()` 签名简化为 `(t, btn)`: 剪贴板写法不变 (WebView2/非安全上下文
   execCommand 兜底), 文字回填逻辑删除, 结果统一走气泡。

## 2. 打开面板修复 (ADR-21②)

1. **桌面壳** (`window.__SERIALHUB_SHELL`): 点「打开面板」→ `POST /api/open-console`
   (302 并行新增的端点, 后端真开系统浏览器) → 成功仅轻提示 **「已在浏览器打开管理台」**;
   **端点失败才降级**为复制网址 + 原提示 (「没弹出浏览器 — 管理台网址已复制…」)。
   成功路径上不再出现误导文案。
2. **浏览器页 (非壳)**: 维持 `window.open` 新标签 (实测弹出新标签指向管理台, 无降级提示);
   弹不出时同样走复制降级兜底。
3. 端点尚未落地时 (404/500) 壳内自动降级复制, 不白屏不报错 —— 与 302 的并序兼容。

## 3. 像素审计 (UI-1, 28 档入径)

- **官方 `tools/ui_pixel_audit.js` 实跑 PASS**: 155 控件 × 7 轮, 高度 ∈ {28,34}±0.5px、
  圆角 8±0.5px 零违例 (真后端 8080 + COM1 桥 open)。
- **注意**: UI 经 `include_str!` 编译进 exe (`src/service.rs:172`), 上述官方跑的是
  **改动前内嵌 UI**; src 正被 302 开发中, 未重编 exe —— 故补跑一轮**同口径补充审计**
  (COLLECT_JS/STRIP_JS/断言逐字取自官方工具, 真后端 8091 + COM1 真桥, 仅文档请求改服务
  磁盘新版 index.html): **155 控件 × 7 轮 PASS, 新图标钮按 28 档入径实测** (R1 两颗 28px,
  R3 抽屉 6 颗, R6/R7 深色 3 颗, 全部 28±0/8±0)。截图 `pixel-audit-1..7-*.png` 在本目录;
  `output/playwright/` 现存 7 图为官方工具本次产物 (旧内嵌 UI), QA 集成后重跑即覆盖。
- 注: 审计桥名旁多出一座后端自种的「CLI」示例桥 (空 fleet 时自动建, 管理台端口+1),
  两轮审计均如此, 属后端既有行为非本次引入。

## 4. Playwright 自验 (mock, 16/16 PASS)

| 场景 | 结果 |
|---|---|
| S1 图标钮形态 (页头+卡片) | 28×28 / 圆角 8 / 内联 SVG / 水平垂直居中 / 无「复制」文字 |
| S2 页头复制 | 气泡「已复制」仅 1 个、贴钮出现; 剪贴板=控制台网址 |
| S3 自散 | ~1.5s 后气泡消失 |
| S4 不堆叠 (抽屉两颗连点) | 全站仅 1 个气泡、挪到新钮旁、计时重置、二次 1.5s 后散 |
| S5 卡片 mini 复制 | 气泡在钮上方居中; 剪贴板=ws://127.0.0.1:9001/ws |
| S6 抽屉三颗 | 28/8/SVG 全对; 复制命令行 → 剪贴板 `serialhub --port COM3 …` |
| S7 壳+端点成功 | 轻提示「已在浏览器打开管理台」, 不弹新标签, 端点恰被调 1 次 |
| S8 壳+端点失败(500) | 降级: 「没弹出浏览器 — 管理台网址已复制…」 |
| S9 非壳 | window.open 新标签指向管理台, 无降级提示 |
| S10 720px | 仪表盘/抽屉无横向溢出 (scrollWidth=720); 气泡不越界 |
| 全程 | 预期外 JS 错误 0 (仅 favicon 404 / mock 500, 属 mock 环境固有) |

截图 (本目录): `icon-header-bubble.png` (页头图标钮+气泡) · `icon-card-bubble.png` ·
`icon-drawer-bubble.png` (抽屉三颗) · `shell-open-ok.png` (壳成功轻提示) ·
`shell-open-fallback.png` (壳失败降级) · `narrow-720.png` / `narrow-720-bubble.png` ·
`pixel-audit-1..7-*.png` (新 UI 七轮)。

## 5. 留给 QA / 集成

- exe 重编后 (302 后端落地) 复跑官方 `tools/ui_pixel_audit.js` 与回归: 新图标钮口径
  本轮已用同码补充审计预验, 预期直接 PASS。
- 真壳内手验「打开面板」: 需 302 的 `/api/open-console` 联调 (本报告 S7/S8 为 mock 断言);
  深色主题下气泡为浅底深字 (ink/bg 反色), 已在 R7 深色轮审计口径内核过控件尺度。
- 气泡在顶栏触发时会自动落到按钮下方 (上方放不下), 属设计行为非 bug。
