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
