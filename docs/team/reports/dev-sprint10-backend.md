# Dev Sprint 10 后端报告 — FR-14 内置主题 win95 (直角 Win95 复古)

作者: 后端主程 (201) · 2026-09-13 · 依据: decisions.md ADR-18② / backlog Sprint 10。
用户点名 "直角、Win95 复古"。改动范围: `assets/themes/win95.css` (新增, 78 行)、
`src/themes.rs` (+~65: 注册 + 单测)、`src/fleet.rs` (+7, 集成测试)、
`ui/index.html` (**+1 行, 跨席微改**, 见 §2)。零新增依赖。cargo test **83 → 86 全绿**。
真机自验 headless COM1 (127.0.0.1:18095), 全程未占用 COM8 (--list-ports 仅枚举),
验证完杀净 + 清 fleet.json, 无残留进程。不 commit。

## 1. win95.css —— 配色与令牌

配色语言参考 98.css 复原项目, 文件头注明 **"配色致敬 Windows 95 与 98.css 项目"**:

- 页面底 #008080 (经典桌面青) / 面板 #c0c0c0 银 / 面板内浅底 #d4d0c8;
- --accent:#000080 (经典标题栏海军蓝), hover #0000a8, accent-ink 白;
- **--radius:0 与 --ctl-radius:0** (用户明确要求, Win95 没有圆角);
- 徽章底一律回银 (#d4d0c8), 深色 ink 区分语义: ok #004a00 / warn #5c4400 /
  err #7a1010 / gray #303030 —— 复古系统没有彩色底徽章, 且对比全部过 AA;
- 终端 --term-bg:#000000, term-border #808080; 方向色取经典终端蓝/绿
  (#0000ff/#008000) 按银底可读性微调: --tx:#3333ff / --rx:#007700;
- --sans 首选 Tahoma (Win9x 时代 UI 字体, 缺失时逐级回退, 非 Windows 不受影响);
- 令牌全集与 dark.css 同键 (单测强制, 见 §3), 另附加一个 --term-ink (§2)。

文件头含各令牌 WCAG 对比度实测自检: ink 对 panel-2 13.7 / panel 11.5;
muted 6.7/5.7; accent-ink 对 accent **16.0:1** (hover 上 13.4); accent 作文字对
panel-2 10.4; 徽章 ink 对银底 6.9/6.0/7.2/8.6 (全 ≥4.5); 方向色按图形描边档
(≥3:1) 自检 tx 3.8 / rx 3.2 (UI 中 --tx/--rx 只作连线/火花线描边与圆点, 不作
正文); term-ink 17.1; 时间戳 faint 对黑 3.3。

## 2. 黑终端×黑正文冲突 → 附加令牌 --term-ink (跨席 1 行)

用户配色里 --ink 与 --term-bg 同为纯黑, 而 `#term` 数据行无显式 color、继承
正文色 —— 换上 win95 终端即黑上黑。主题铁律只许覆盖 :root 令牌, 不能写选择器
自救。处理: win95.css 定义附加令牌 `--term-ink:#e8e8e8`, ui/index.html 的
`#term` 增加 `color:var(--term-ink,var(--ink))` —— 未定义该令牌的主题 (现有
三套) 自动回退原表现, **零变化**; 已用 86 个测试 + 真机换肤路径确认无回归。
此行属 ui/ (202) 地盘, 属 P0 可见性修复的越席微改, 请 UI 席知悉复核。

## 3. 注册与单测 (83 → 86, 全绿)

- `themes::BUILTIN` 第 4 项 ("win95", include_str!...) 排 example-oreo 后;
  模块头注释三套→四套。落盘走既有 ensure_builtin (对 BUILTIN 泛化): 启动即写
  themes/win95.css, 已存在文件不覆盖 —— 真机确认 target/debug/themes/ 出现
  win95.css, 且幂等语义有专测。
- themes.rs: `builtin_contains_three_themes` 更名 `builtin_contains_four_themes`;
  新增 `win95_theme_pins_retro_tokens` (直角双 0 / --accent:#000080 / --bg
  #008080 / 银面板 / 四徽章底全银 / term-ink / 文件头致敬语与自检段落);
  新增 `win95_ensure_roundtrip_preserves_user_edits` (落盘往返逐字节一致 +
  resolve 命中 + 用户改动不被覆盖); 新增 `every_builtin_covers_the_full_token_set`
  (以 dark 为令牌键基准集, 任何内置主题缺项即编译期失败 —— ADR-18 "覆盖同一
  集合" 的回归防线); scan 字典序断言更新 (win95 介于 light 与 zeta)。
- fleet.rs `themes_endpoints_list_serve_and_traversal_guard`: /api/themes 四套
  内置全在且 builtin=true; /themes/win95.css 200 且含 --ctl-radius:0 与
  --accent:#000080。

## 4. 真机验证 (headless, COM1, 已杀净)

`target/debug/serialhub.exe --headless --port COM1 --baud 115200 --config 8N2
--addr 127.0.0.1:18095`:

- `/api/themes` → `{"themes":[{"builtin":true,"name":"dark"},
  {"builtin":true,"name":"example-oreo"},{"builtin":true,"name":"light"},
  {"builtin":true,"name":"win95"}]}` —— 四套齐全;
- `/themes/win95.css` → HTTP 200, text/css; charset=utf-8, 3772 字节, 内容正确;
- `/api/status` → phase:"open", COM1, 115200 8N2 —— 桥真机打开成功。

收尾: 进程按 PID 击杀, tasklist 无 serialhub 残留, 18095 关闭;
%APPDATA%\SerialHub\fleet.json 为本次测试所写, 已删 (仅余 qa-s7 历史备份)。
**COM8 未占用** (仅 --list-ports 枚举到, 从未打开)。

排查插曲 (供后续真机测试参考): 用 harness 后台任务方式起桥会随任务包装层被
回收 (表现为 "exit 1" 且无错误文案, 两次复现); 改为**单次前台调用内起桥→
curl→kill** 后一切正常, 桥本身无问题。期间观察到第三方 release 实例
(127.0.0.1:8095, --no-fleet) 出现又自行退出, 非本席启动/击杀, 未触碰。

## 5. 已知取舍与交接

- faint #606060 对银面板 3.5:1 (panel-2 4.1): 低于正文 AA, 仅用于装饰性小字/
  时间戳 —— 复古哑色的固有取舍, 文件头自检已注明; UX 复审 (105) 如嫌弱可微调。
- body 纯黑文字对桌面青底 4.4:1、黑底气泡提示 4.4:1: ≈AA 线, 经典配色的固值
  (正文实际均落在银面板上, 青底只承担留白)。
- ui/index.html 的 --term-ink 兜底行请 UI 席 (202) 过目; 建议后续 UI 席用
  Playwright 对 win95 做一次视觉审计 (直角控件/银徽章/黑终端观感), 本席只验了
  令牌层与服务链路。

- UI 席复核通过 (301): 三主题 --term-ink 未定义均落回 --ink, 与原继承值零视觉差; 写法/位置合规, 已并入 ui/index.html 结构约定。

## 6. 波2 · win95 主题「真·95 结构版」(主题席, 2026-09-14)

用户实测反馈「win95 不够正宗: 设置框/提示框还是圆角、颜色不对、文字像现代 UI」。
根因: 第一版只覆盖了颜色令牌, Win95 的魂在结构性样式。重写 `assets/themes/win95.css`,
在令牌之外追加结构规则 (参考 98.css 做法; 主题文件是整张样式表, 经 `<link>` 挂载):

- **直角到底**: `*{border-radius:0!important}` —— 弹窗/提示气泡/徽章 pill/输入框/
  按钮/抽屉一个圆角不剩; `.switch-track::before` (伪元素, `*` 打不到) 单独拍平。
- **凹凸立体**: 按钮/下拉/面板/对话框 `border-color:#dfdfdf #404040 #404040 #dfdfdf`
  (上左亮下右暗), `button:active` 翻转内陷; 输入/下拉内陷 + 白底; `.stat/.equiv/#term`
  内陷; 弹窗/抽屉 `box-shadow:none` (95 没有柔投影); 幽灵复制钮保持无框; 页签做立体小舌。
- **标题栏**: 页头与弹窗头 = 藏蓝 #000080 白粗体字, 弹窗关闭钮 = 银色立体小钮,
  网址块 = 内陷白窗格; 藏蓝底上焦点环改白。
- **颜色/字体深化**: 复制气泡 = 经典土黄 tooltip #ffffe1 + 1px 黑边; 桌面青上的零散
  文字 (footer/工具条计数) 改白 (银字对青仅 3.3:1, 白 5.1:1); 字体栈
  `Tahoma,"MS Sans Serif","Microsoft YaHei",sans-serif`; 控件字号 12px
  (**--ctl-h 34/28 未动**, 高度契约保持)。
- **诚实边界**: 文件头注明浏览器字体反锯齿关不掉, 文字分辨率无法 100% 复刻像素字;
  真·像素字需内嵌位图字体 (约 +80KB) 留作候选项。
- 对比度复核全过: 标题栏白字 17.4:1 / .sub 8.2:1 / 银钮黑字 10.4:1 / tooltip 17.9:1 /
  输入白底黑字 21:1; 徽章与方向色沿用第一版自检值。

### 验证

- `cargo test themes` 9/9 过 (含 `win95_theme_pins_retro_tokens` 致敬语 pin —— 波2
  首跑曾因重写丢了该注释句挂红, 已补回; 全部令牌 pin 一直绿);
- `AUDIT_THEME=win95 node tools/ui_pixel_audit.js` → **PASS**: 160 可见控件 × 7 轮,
  高度 ∈ {28,34}±0.5, 圆角 = 0±0.5, 违例 0; 全景存
  `docs/team/reports/qa-sprint10/admin-panorama-win95.png`;
- 真机截图 (COM1 桥 win95-demo open, headless 8093) 存
  `docs/team/reports/dev-sprint10-ui/`: win95-pano-dashboard.png (浅银/青底全景) +
  特写 ×5: dialog-settings / dialog-new (藏蓝标题栏+银钮) / tooltip (土黄气泡) /
  badge (方形徽章+银钮排) / drawer (立体页签+内陷白输入+方形开关)。
- 功能零变化, 只动 `assets/themes/win95.css` (+二进制重嵌); 未 commit。

### 插曲 (给后续真机测试避坑)

1. 起手时 8080 有常驻 serialhub.exe (PID 53296, 有活跃浏览器连接) 锁住二进制,
   按 audit 同款口径 `taskkill /IM` 清掉后才能重链; 波2 首次审计/截图全跑在旧皮肤上
   险些误判 —— 排查发现 `target/release/themes/win95.css` 是 `ensure_builtin` 早期
   落盘的**旧副本, 会遮蔽新内置主题** (磁盘命中优先, 用户编辑不被覆盖属设计行为)。
   **改内置主题后必须删 `target/release/themes/<name>.css` 再起后端**, 已删除并由新
   二进制重新落盘 (8094 字节)。收尾 tasklist 无 serialhub 残留, 8080/8092/8093 全释放,
   临时脚本已删; **COM8 全程未碰** (仅审计桥 COM1)。
