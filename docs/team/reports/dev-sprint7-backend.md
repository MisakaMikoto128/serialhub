# Dev Sprint 7 后端报告 — 管理台换址 / 主题插件 / 图标集成 (FR-13/14/15 / ADR-18)

作者: 后端主程 (201) · 2026-09-13 · 依据: decisions.md ADR-18 / spec FR-13/14/15 /
backlog「Sprint 7 · dev-backend (波1)」。改动范围: `src/fleet.rs`、`src/themes.rs`(新)、
`src/icons.rs`(新)、`src/gui.rs`、`src/api.rs`、`src/cli.rs`、`src/main.rs`、
`build.rs`(新)、`assets/`(themes 三主题 + README 契约)、`Cargo.toml`
(build-dependencies: png 0.17 / winres 0.1 —— 仅构建期, 运行时零新增依赖)。
不 commit; 真机自验用 127.0.0.1:8100/8101/9201, 桥串口仅 COM1 配置未真开
(autoOpen=false), COM8 未触碰; 验证完 taskkill 清场, 无残留进程。

## 1. FR-13 管理台端口可改 (原地换绑复用 ADR-12)

- **端点**: `POST /api/manager/addr {"addr":"ip:port"}` (fleet.rs `manager_set_addr`)
  —— 校验/登记复用 `api::restart_core` 与 /api/restart 完全同一路径, 登记槽由
  run_manager_with 的 serve 循环消费 (ADR-12 第二出口), 原地 drop 旧 listener →
  重新 bind; hub/串口会话/各桥数据面/托盘全程不动; 失败 2s 重试后回退原地址
  (沿用现有语义, 真机演练: 8103 被占 → 8101 继续服务)。
- **持久化**: fleet.json 顶层新增 `"manager": {"addr": "127.0.0.1:8101"}` 段
  (`ManagerRec`; serde default + skip_serializing_if, 旧清单无段照常解析)。
  换绑成功即写; 启动时若清单已存在则同步一次现地址 (不新建文件)。
- **重启恢复**: `run_manager_with` 绑定前读 [manager] 段 —— **未显式给 --addr**
  时恢复持久化地址; 显式 `--addr` 优先 (`Cli.addr_explicit`, parse 时标记)。
  地址非法/清单损坏 → 回退 CLI 地址, 不致命。
- **壳侧跟随闭环 (gui.rs)**: `UserEvent::AddrChanged` —— 服务线程 on_event 里
  Ready 分流 (首次 = ready 通道握手; 之后每次 = 换绑成功 → proxy 发事件), 主循环
  收到后 `webview.load_url(新地址)` 并更新 `cur_addr` (托盘「在浏览器打开控制台」
  用现值)。浏览器页侧跟随属前端 (页面自己 location 跳)。
- 各桥数据端口不动有回归断言 (换绑后 9201 仍可连)。

## 2. FR-14 主题插件 (themes/ 目录即插件)

- **目录**: 默认 exe 旁 `themes/`, `--themes-dir <路径>` 可指定; 启动时不存在则
  创建并写入三套内置主题 (已存在文件**不覆盖** —— 用户自制内容优先)。
  内置 CSS 经 `include_str!` 编译进二进制, 源文件在 `assets/themes/`。
- **三套内置**: `light.css` (= ui/index.html 当前浅色令牌, 出厂基准);
  `dark.css` (定版: 暖灰底 #14171a / 面板 #1d2126 / 文字 #e8eaed / 边框 #2c313a,
  accent #0e6db8 不变; 文字对比 ink 13.1:1 / muted 7.4:1 / faint 4.8:1,
  徽章 ink 对各自底 ≥7:1, 过 WCAG AA —— 注: accent 作为 hover/选中**文字色**
  约 3:1, 为守住"accent 不变"与主按钮白字 5.4:1 的取舍, 详见 dark.css 头注);
  `example-oreo.css` (黑白高对比示范, 文件头即「照这个文件自制主题」四步指南)。
- **端点**: `GET /api/themes` → `{"themes":[{"name":"light","builtin":true},...]}`
  (扫目录 *.css 去后缀, 字典序; builtin = 三套内置之一)。`GET /themes/{file}`
  静态服务 (text/css; 缺失/非法 → 404 JSON)。
- **路径穿越防护** (themes::resolve, 双重防线): 白名单字符 (ASCII 字母数字._-)
  + 拒绝分隔符与 `..` + 必须 .css 结尾; 再 canonicalize 复核仍落在主题目录内。
  真机验证 `%2e%2e%2fCargo.toml`、`%2e%2e%5cCargo.toml`、`/themes/../Cargo.toml`
  全部 404。主题文件**每请求现读** —— 用户改 css 刷新即生效 (插件语义)。
- 主题选择器 UI (无刷新切换/浏览器本地记忆) 与 `<link>` 接线属 dev-ui。

## 3. FR-15 图标集成 (存在才嵌入, CI 不依赖美术文件)

- **契约**: `assets/README.md` —— master.svg / app.ico (须含 PNG 条目) /
  png/serialhub-{16..256}.png / tray-{open,retry,closed}.png (32×32 RGBA) /
  favicon.svg, 命名逐字匹配。icon-designer 波1 已到货 master.svg / icon.svg /
  favicon.svg / png 六尺寸; app.ico / tray-\*.png / serialhub-64.png 暂以本机
  占位图补位联调 (交付后直接覆盖即可, README 已注明)。
- **build.rs**: ① exe 图标 —— 仅 Windows **目标** (CARGO_CFG_TARGET_OS) 且
  assets/app.ico 存在时 winres 嵌入, 失败只 warning 不阻塞; ② 构建期把
  tray-\*.png / 窗口图标源 PNG 解码成原始 RGBA 写 OUT_DIR (运行时零解码依赖);
  窗口图标源优先级 = app.ico 内最大 PNG 条目 → png/serialhub-64.png;
  ③ 恒定生成 OUT_DIR/embedded_icons.rs (缺资产的常量为 None) 供 `src/icons.rs`
  include! —— 三平台 CI 无 assets 图标也照常编译。
- **gui.rs**: 窗口图标 = WINDOW_ICON_RGBA (回退程序画的圆点); 托盘三态 =
  tray-open/retry/closed.png, 相位切换**仅 set_icon 换图**, szTip/菜单条目身份
  终生不变 (FIX-12 三修口径); 资产缺失/尺寸不符 → 圆点回退。
  真机验证: `ExtractAssociatedIcon` 从 exe 解出图标 (winres 嵌入生效)。
- **favicon**: `GET /favicon.svg` 同时挂管理台 (fleet control_router) 与 legacy
  单桥 router; 内容 = 嵌入的 favicon.svg, 缺资产走 icons.rs 内置兜底 SVG,
  端点恒 200 (image/svg+xml)。页面 `<link>` 由前端加。

## 4. 测试 (cargo test 64→**73** 全绿) + 真机自验

新增 9 项: fleet 3 (`manager_addr_rebind_persists_and_restores` —— 换绑受理/旧址
关闭/新址就绪/桥数据面不动/清单 [manager] 写入/Ready(新址) 事件/重启恢复/显式
--addr 优先; `themes_endpoints_list_serve_and_traversal_guard`; `favicon_endpoint_
serves_svg`) + fleet 持久化 1 (`fleet_file_manager_rec_roundtrip` 含旧清单兼容) +
themes 单测 4 (内置三主题内容契约含 dark 定版色与奥利奥头注 / 落盘幂等不覆盖
用户改动 / 扫描排序与 builtin 标记 / 穿越防护矩阵) + CLI 1
(`themes_dir_and_addr_explicit_flags`)。既有 64 项零改动通过 (契约无变更:
status 13 字段 / fleet 桥对象 15 字段不动)。

真机 (headless + fleet.json): ① 8100→8101 换址 —— 旧地址拒绝连接、新地址
服务 fleet、桥数据面 9201 照常、[manager] 落盘; ② 换址失败回退 —— 8103 被占
2s 重试后保持 8101 继续服务 (日志留痕); ③ 重启恢复 —— 不带 --addr 重启, 管理台
直接回到 8101; ④ /api/themes 三内置 + /themes/dark.css 200 + 穿越三连 404 +
favicon 200 svg; ⑤ themes 目录自动落盘三文件。全程未开串口, 无进程残留。

## 5. 遗留与移交 (波2)

- 换绑成功后壳侧 webview 已 navigate; **浏览器页侧跟随** (轮询断线后 location
  跳) 与端口设置对话框 / 「打开面板」按钮 = dev-ui。
- 壳侧 Ready→navigate 的端到端人工复核 (自动化只到事件层: 单测断言 Ready(新址)
  事件已发) —— 建议 QA 真机双击启动 → 页内改端口 → 观察窗口页面自动跳转。
- icon-designer 交付 app.ico / tray-\*.png 后直接覆盖占位文件重跑 `cargo build`
  即可, 代码零改动; CI 无图标文件时回退圆点/兜底 favicon (已验证编译路径)。
- 版本号 v1.3.0 与 CHANGELOG 随发版轮 (ADR-18⑤ UX 复审后) 统一动。
- 已知取舍: dark 主题 accent 文字色 (hover/选中) 约 3:1 (UX 波2 复审可裁);
  显式 `--addr` 启动会以现地址刷新 [manager] 段 (现状即真相, 与"变更即写"一致)。

---

# Dev Sprint 7 波2 追加 — 真资产重建与真机核验 (FR-15)

作者: 后端主程 (201) · 2026-09-14 · 范围: assets/ 真资产替换 + 重建 + 真机核验;
代码零改动 (build.rs/gui.rs/api.rs 均未动)。cargo test 73/73 全绿不受影响。
不 commit; 我的进程已清场 (另一席位 release 实例 :8089 占 COM1, 非本席进程未动)。

## 1. 资产盘点 (重要发现)

icon-designer 交付中 **SVG/favicon/png 六档为真资产** (master.svg / icon.svg /
favicon.svg / png/serialhub-{16,24,32,48,128,256}.png, 均为桥+枢纽母题);
但 **app.ico、tray-{open,retry,closed}.png、png/serialhub-64.png 盘面仍是我波1
的占位点图** (时间戳/字节数/ico 条目数确证, 疑似被波1 占位脚本覆盖或交付未落盘)。
处置: app.ico 以**真 PNG 六档原像素重打包** (16/24/32/48/128/256, PNG 条目,
纯容器封装零美术改动); tray-\*.png 与 64.png 无法凭空恢复 → 列入视觉席清单。

## 2. 真机核验 (全部换真货)

- **exe 文件属性图标**: 重 builds 后 ExtractAssociatedIcon 解出 32px —— 真桥母题 ✓
  (winres 嵌入生效, 证据 output/shots7/exe_icon_check.png)。
- **窗口/任务栏图标**: 活动窗口 WM_GETICON 取到 256px 真母题 ✓; PrintWindow
  窗口截图中标题栏 16px 图标可辨 (output/shots7/window_icon.png / win_title_zoom.png)。
- **托盘三态 (HTTP 驱动相位)**: COM2 开→open(绿) / 配置 COM99 开→retry(琥珀,
  retries 计数) / 关→closed(灰), /api/status 三态全数驱动成功 ✓; 但托盘**角标
  截图**受采集环境限制 (本会话截屏不含任务栏/通知区), 且当前盘面 tray 图本就是
  占位点图 —— 换真资产后请 QA/人工补一次目视复核。
- **/favicon.svg**: 端点已服务真 SVG (品牌绿渐变反色版, 内容逐字核对) ✓。

## 3. 视觉席 (agent_13dff983) 待办清单

1. **tray-open/retry/closed.png 真资产缺失** (32×32 RGBA 三态: 运行中绿 /
   重连中琥珀 / 已停止灰) —— 现exe托盘为波1占位点图, 交付后覆盖 assets/ 同名
   文件重跑 cargo build 即生效, 代码零改动。
2. **png/serialhub-64.png 仍为占位** (六档真货缺 64 一档) —— 补交; 或确认删档,
   本席同步改 assets/README.md 契约 (现仅作 app.ico 缺失时的窗口图标回退源)。
3. **app.ico**: 本席以真 PNG 六档重打包为过渡版; 若设计师有原版七档 ico
   (含 64 档), 交付覆盖即可 (build.rs 契约不变)。
4. 16px 辨识度: png/serialhub-16.png 目测拱桥+端口可辨, 无阻塞项; favicon.svg
   真货对比度 OK, 无修改意见。

## 4. 其他

- 本机 COM1 被另一席位 release 实例 (:8089) 占用致"拒绝访问", 已换 COM2 完成
  相位驱动; 该进程非本席所有, 未触碰。我的全部实例经 /api/shutdown 或 taskkill
  清场, tasklist 复核无残留。
- 真资产六档 PNG/ICO 打包产物已嵌入 target/debug/serialhub.exe; CI 无资产时
  回退路径 (圆点/兜底 favicon) 波1 已验证, 本轮未回归 (73/73 含全部图标路径单测)。

---

# Dev Sprint 7 波3 追加 — 视觉席真资产全量替换后的最终核验 (FR-15)

作者: 后端主程 (201) · 2026-09-14 · 代码零改动; `cargo build --release` 重新嵌入;
不 commit; 我的实例经 /api/shutdown 优雅清场, tasklist 无 serialhub 残留。

## 1. 资产复核 (无占位残留)

视觉席重交真资产逐项核对: **app.ico 42,549B 七档全 PNG 条目** (16/24/32/48/64/128/256,
各条目与 png/ 同尺寸真资产逐字节同源)、**tray-open/retry/closed.png 真母题三态**
(桥形 + 右下绿/琥珀/灰状态圆点, 2.2KB 级, 非波1 占位点图的 564B 指纹)、
**png/serialhub-64.png 真货** (4,715B)。盘面已无任何占位图指纹。

## 2. 嵌入与真机核验 (release 构建全部真货)

- **嵌入零漂移**: build.rs 产出的 4 份 OUT_DIR RGBA 载荷 (TRAY_OPEN/RETRY/CLOSED
  + WINDOW_ICON 256px) 与真资产新鲜解码**逐字节一致 (4/4)** —— 二进制内无占位字节。
- **exe 属性图标**: ExtractAssociatedIcon 解出真桥母题 (output/shots7/exe_icon_rel.png)。
- **窗口图标**: 活动窗口 WM_GETICON = 256px 真母题 (win_icon_rel.png); 任务栏同源。
- **托盘三态 (HTTP 驱动)**: COM2 开→open(绿) / COM99→retry(琥珀, retries 计数) /
  关→closed(灰), 相位切换仅换 hIcon (ADR-12 三修口径不变); 托盘角标目视复核仍受
  本会话截屏环境限制 (无任务栏), 请 QA 人工补一眼 —— 图像源已是真资产 (逐字节实证)。
- **/favicon.svg**: 706B 真 SVG, 品牌绿渐变逐字核对 ✓。
- cargo test **73/73** 全绿 (零回归)。

## 3. FR-15 收尾

视觉席清单三项 (tray 三态 / serialhub-64 / 原版七档 app.ico) 全部交付完毕,
后端集成无遗留; FR-15 可关闭。
