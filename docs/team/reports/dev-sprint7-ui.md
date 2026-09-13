# Dev Sprint 7 报告 — 打开面板 / 管理台网址设置 / 主题选择器 / favicon (dev-ui 301)

- 日期: 2026-09-13 · 变更文件: **仅 `ui/index.html`** (head/页头/弹窗/脚本四处; 未 commit)
- 依据: ADR-18 (①FR-13 ②FR-14) · spec FR-13/14 · backlog「Sprint 7」dev-ui 条目 · UI-0/UI-1
- 自验: mock 控制面 (按指挥部契约, 后端波1端点尚未落 src) + Playwright **43 断言全过**,
  预期外 console 错误 0; **真壳实测** (release exe + CDP); 720px 无横向滚动;
  全程纯 HTTP + `--no-fleet` (壳实测不加载清单), **未碰任何串口, COM8 禁碰未碰**;
  测完 mock/壳进程已清, 临时脚本在 %TEMP%\sh7test (不入库); 未 commit; 未碰 src/tests

## 1. 任务 A — 「打开面板」按钮 (页头, FR-13)

1. **行为**: 浏览器页 `window.open(url,"_blank")` 开新标签 (实测 popup 网址=控制台网址);
   `window.open` 返回空 = 没弹出来 → 降级: 复制网址进剪贴板 + 通知「没弹出浏览器 — 管理台网址已复制, 请粘贴到浏览器地址栏打开」(copyText 补返回值, 原调用方不受影响)。
2. **壳内实测结论 (任务钦定"实测为准", 已做真实验)**: `target/release/serialhub.exe`
   (wry 0.57.0) + `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port` 挂 CDP:
   - 壳内 `window.open(url,"_blank")` **返回 null**、无异常、无新 target —— 与源码一致
     (gui.rs 未设 `new_window_req_handler`, wry webview2/mod.rs 对 NewWindowRequested
     直接 `SetHandled(true)` 吞掉);
   - 注入与发行版同逻辑的处理函数 + **真实鼠标点击** (有用户手势): opened=false →
     `navigator.clipboard.writeText` **成功**, 剪贴板内容=管理台网址。
   - **结论: 发行版按钮代码无需改动 —— 壳内自动落入降级分支 (返回 null 判定可靠), 复制+提示路径在真壳验证通过。**
3. **尺寸裁定**: 任务钦定 34px 档 → `#btnOpenPanel`/`#btnSettings` 归标准档 34 (实测 34/13px/8px)。
   注意现有页头钮 (复制/退出) 是 28 紧凑档 → 页头 34/28 混排, 属任务钦定裁定; QA 像素审计
   两档皆合规 (34∈标准档)。若 UX 想全页头统一 28, 改两行档位即可。

## 2. 任务 B — 设置对话框 (页头「设置」→ `#dlgSettings`, FR-13)

1. **「管理台网址」输入** (UI-0: 不叫端口/监听): 回显当前 `location.host`, 占位 127.0.0.1:8080;
   校验复用 `validateListen` (IP:端口), 错误走 `.ferr` 就地红框红字+聚焦 (P3 同款)。
   值未变化时「应用」禁用。
2. **两段确认** (沿用 退出/删除 同款): 首点「应用」→ 按钮变「确认修改?」+ 通知预告
   「管理台网址将从 A 改为 B; 串口不受影响, 本页将自动跳到新网址」, 3s 不确认自动复原;
   编辑输入即解除确认态。再点 → `POST /api/manager/addr {addr}`。
3. **自动跟随**: 200 ok → 优先采用响应 `j.addr` (防御: 缺省用输入值) → **先探后跳**:
   no-cors fetch 探新址, 2s AbortController 超时兜底; 探通才 `location.replace("http://"+新址+"/")`;
   探不通明示「新网址 2 秒内没应答, 本页留在原网址没跳 — 桥和串口不受影响; 请稍后手动打开 <新址>」
   (契约的 2s 兜底文案要求; 不先探后跳的话, replace 到不可达地址会整页死在错误页, 兜底提示根本来不及发)。
4. **失败回退明示**: 非 2xx/契约错误 → `.ferr`「没改成功: <原因> — 管理台仍在原网址 <A>」+
   通知 err; 按钮复原、回显原址 (实测 500 路径)。

## 3. 任务 C — 主题选择器 (FR-14, 无刷新换肤)

1. **数据**: `GET /api/themes` → 下拉 (值=name, 显示名映射 light/dark/oreo→浅色/深色/示例·奥利奥,
   未知名显示原名); 获取失败 → `.ferr` 明示并保持当前主题可选 (不白屏)。
2. **切换**: 动态 `<link id="themeCss">` 换 href=/themes/<name>.css, **无刷新** (实测 JS 状态存活);
   选择存 `localStorage("sh_theme")`。
3. **防闪白**: `<head>` 内同步内联脚本在 body 渲染前读 localStorage 预挂 link —— 实测重载后
   domcontentloaded 早期 link 已就位、首帧即深色 (bg 实测 rgb(20,24,29), 无浅色闪白)。
4. **火花线方向色随主题**: C_RX/C_TX 原为启动时一次性快照, 深色下会沿用浅色色值 —— 已改
   `refreshSparkColors()` (换肤 link onload + 600ms 兜底 + 启动时各刷一次, 重绘卡片+抽屉全部火花线),
   QA 观测口 `window.__sh.sparkColors` (实测深色下 rx/tx 换成主题值)。
5. **onerror 明示**: 主题文件缺失 → 通知「主题「X」没加载出来 — 请确认程序 themes 目录里有这个文件」。
6. **深色肉眼审** (自备代表深色令牌覆盖, 截图 03/04): 卡片/徽章四态/流程图/火花线/终端/抽屉全可读,
   **未发现不可读令牌**。基础 CSS 三处**非令牌硬编码色** (`.node.bridge .cap` 白、`.quit-btn.armed`
   /`button.danger.armed` 白字) → **波2 已修, 见 §9**。
7. **⚠ 交 UX/后端知晓**: localStorage 按 origin (含端口) 隔离 → 改管理台网址跳新址后, 主题记忆
   **不会跟过去** (浏览器本质, UI 层无解), 新址回落内置浅色。

## 4. 任务 D — favicon

`<link rel="icon" type="image/svg+xml" href="/favicon.svg" />` 入 head (后端端点并行; mock 实测 link 在位)。

## 5. 新增文案 (UI-0 过审: 0 实现词, 无"监听/端口/换绑")

| 文案 | 位置 |
|---|---|
| 打开面板 / 设置 (悬浮题各配大白话) | 页头两钮 |
| 主题 / 浅色 / 深色 / 示例·奥利奥 | 设置弹窗下拉 |
| 换主题立刻生效, 记在这台电脑的浏览器里; 想自制主题, 把样式文件放进程序的 themes 目录就行。 | 主题下方 hint |
| 管理台网址 | 设置弹窗字段 (悬浮题「管理台自己的网址, 形如 127.0.0.1:8080…」) |
| 改的是管理台自己的网址: 串口不受影响, 本页将自动跳到新网址, 重启后仍用新网址。 | 字段下方 hint |
| 管理台网址将从 A 改为 B; 串口不受影响, 本页将自动跳到新网址。再点一次「确认修改?」。 | 两段确认通知 |
| 已复制网址类降级提示 / 主题缺失提示 / 改址失败红字 / 2s 兜底提示 | 见 §1-§3 各条 |

## 6. 高度归档表 (Playwright 实测, 全部 fs=13px / radius=8px)

**标准档 34px**: `#btnOpenPanel`(打开面板) · `#btnSettings`(设置) · `#btnSetApply`(应用, 含 armed 态) ·
`#btnSetCancel`(取消) · `#setTheme`(select) · `#setAddr`(input) —— 任务钦定页头 34 档, 见 §1.3
**紧凑档 28px**: `#btnSetX`(弹窗✕, 沿用 .x-btn 档位)

## 7. 自验明细 (mock 契约: /api/themes, /themes/*.css, POST /api/manager/addr, /api/fleet)

13 组 43 断言: 页头按钮/favicon · 打开面板新标签 · 主题列表+中文名 · 回显+应用钮联动 ·
深色无刷新切换 (href/localStorage/JS 状态存活/令牌生效/火花线重读) · 重载防闪白 ·
深色抽屉截图 · 缺主题文件 onerror 明示 · 改址两段确认→POST body {addr}→自动跳新址 (控制台网址更新) ·
后端 500 失败红字不跳转 · ok 但新址不可达 2s 兜底留原页 · 壳模拟 (open 返回 null→剪贴板+提示, 退出钮回归) ·
720px 主视图+弹窗无横向滚动 · 新控件 34/28 实测。截图 `dev-sprint7-ui/01..05` (浅色仪表盘/浅色设置/
深色仪表盘/深色抽屉/720 设置)。

## 8. 交接 / 待办

1. **后端波1合入后需回归**: 端点按指挥部契约 mock (src 现无 /api/manager/addr、/api/themes);
   真端点字段若与契约有差 (如响应无 addr 字段) 代码已防御 (回落输入值)。
2. 真壳只实测了 window.open/剪贴板/降级; **换绑后的壳内跳转** (wry 跟随 location.replace 到新端口)
   需后端端点就位后在真壳回归一轮。
3. §3.7 主题记忆不跨端口, 请 UX/后端裁定 (本轮 UI 无动作); §3.6 三处硬编码色 → **波2 已修, 见 §9**。

## 9. 波2 小修 — 硬编码色令牌化 (2026-09-13, 仍仅 `ui/index.html`)

1. **三处全改, 零新增令牌** —— 全部用**现有配对令牌派生**, 任何成对定义 `--err/--err-bg`、
   `--accent/--accent-ink` 的主题 (含第三方自制) 自动成立, **后端三套主题文件无需补任何令牌**:
   - `.quit-btn.armed{color:#fff}` → `color:var(--err-bg)` (armed = --err 实底, 文字取其配对反色);
   - `button.danger.armed{color:#fff}` → `color:var(--err-bg)` (同上);
   - `.node.bridge .cap{color:rgba(255,255,255,.75)}` → `color:inherit;opacity:.75`
     (继承 .node.bridge 的 `--accent-ink`, 75% 层级弱化不变)。
   实测对比度: 浅色 #fae9e7 字/#bb3a30 底 ≈ 4.6:1; 深色 #3a1d1a 字/#e06a60 底 ≈ 4.6:1 —— 两主题均可读。
2. **双主题回归**: 测试脚本加 `SH_THEME` 预置 (浅色默认 / dark 预置 localStorage 启动) +
   T14 断言 (armed 文字 computed color ≡ --err-bg 解析值; 桥 cap ≡ accent-ink @0.75)。
   **浅色 46/46 全过, 深色 47/47 全过** (深色多 1 条: 预置后首帧即深色无闪白)。
   截图双主题各 5 张 (`*-light` / `*-dark`)。未 commit; 未碰 src/tests; 无串口接触。

## 10. 波3 修复 — UX 复审 P1×2 + P2×4 (2026-09-13, 仍仅 `ui/index.html`)

依据 `ux-sprint7-review.md` (105); 全部六条本轮修完, 未碰 COM8 (mock 用 COM1/COM2/COM3 文案)。

| 条目 | 修法 |
|---|---|
| P1-1 主题下拉默认态骗人 | `fillThemeSelect` 末尾以 `cur = themeNow \|\| "light"` 参与匹配, 无命中回落首项 —— 下拉恒等于真实生效主题 (无记忆=内置浅色, 真后端字典序 dark 在前也不再看错) |
| P1-2 第三主题露英文 ID | 后端契约仅 name/builtin (无显示名字段) → 选实现简单者: `THEME_LABEL` 补 `"example-oreo":"示例·奥利奥"` (保留 oreo 键兼容) |
| P2-1 页头 34/28 混排 + 720 拆钮 | `#btnCopyUrl,#btnQuit{height:var(--ctl-h)}` 覆盖紧凑档 (id 特异性) → 页头四钮统一 34; 新增 `.hdr-actions` 包裹 打开面板/设置/退出程序 (flex+nowrap) → 720 换行整组走, 断言实测两钮同排 |
| P2-2 统计摘要裸「--」 | 校验位有记录 → `校验 <b>8N1</b>`; 缺记录 → `<b>校验 未记录</b>` (带标签大白话, 不再裸杠) |
| P2-3 📋 复制 emoji | `#btnCfCopyCmd` 及其 copyText idle 参数去掉 emoji, 回归全站纯文字 |
| P2-4 串口下拉重复端口名 | `fillPortSelect` 剥掉描述尾部与端口名重复的 `(COMxx)` (正则转义后缀匹配), 悬浮题保留原始描述 |

**回归**: mock 对齐真后端 (主题字典序 dark/example-oreo/light、内置名 example-oreo、端口描述带重复、
桥对象含无 config 字段的停机桥) + 断言扩充 (P1-1 默认选中、P2-1 页头四钮 34 实测+720 同排、
P2-2 占位话术、P2-3 纯文字、P2-4 去重) → **浅色 53/53 全过, 深色 54/54 全过** (深色多 1 条预置首帧);
截图双主题刷新 (`*-light` / `*-dark` 各 5 张)。未 commit; 未碰 src/tests; 无串口接触。
