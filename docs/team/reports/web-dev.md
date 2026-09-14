# web-dev.md — 706 功能开发组产出报告 (2026-09-14)

- 任务: 实现 website/index.html 发布官网单页 (WEB-1/2/5/7), 纯静态零依赖零构建。
- 依据: docs/product/website-spec.md · goal-web.md「706」· reports/web-copy.md (逐字) ·
  reports/web-ui.md (令牌/布局/断点照办) · reports/web-assets.md · 架构师裁决 6 条。
- 红线遵守: 未访问 qingjian.app; 未改 src/ ui/ tests/ 与他人 reports; 未碰根 assets/;
  未 commit。新增文件仅 website/index.html 与本报告 (+自检截图 shots-webdev/)。

## 1. 交付物

- `website/index.html` — 单文件 (HTML + 内联 `<style>` + 内联 `<script>`), 零外部依赖,
  无 CDN/外链字体/统计脚本; 站内引用全相对 (`./assets/...`), 仅 og:image / og:url 用
  `https://misakamikoto128.github.io/serialhub/` 绝对地址 (子路径部署防 404)。
- `docs/team/reports/shots-webdev/` — 自检截图 (v360/v720/v1440 全页 + lightbox), 供 707 对照。

## 2. 实现要点

1. **八区块齐备**: 导航(粘性,56px) → Hero(眉标/H1/副标/双按钮/平台 pill/版本小注) →
   特性 6 卡 → 截图 6 图 → 下载 3 卡 → 快速开始 3 步 → 更新日志 5 条 → 页脚三列。
   锚点: #features/#screenshots/#download/#quickstart/#changelog; 平台 pill 直达对应下载卡。
2. **RELEASE 数据块 (WEB-2)**: `<head>` 内带注释标记的 `const RELEASE = {...}`
   (版本/日期/三平台包名/规格行/SHA256/链接/releases/changelog)。页面下载数据由它渲染;
   正文同名静态文本仅作无 JS/搜索引擎回退; `<noscript>` 提供 Releases 兜底链接。
   **逐版发布只改这一个对象**。复制按钮读取渲染后的 DOM 值。
3. **视觉**: 46 条 `--wh-*` 令牌全量落地 + 1 条扩展 (`--wh-term-accent`, 深色条幽灵钮
   hover 提亮蓝, 对应 ui 规范 §4.3 "提亮蓝思路", 原令牌面无此色)。mobile-first 三断点
   360/720/1440 令牌覆写; 快速开始 <720 纵向时间线 / ≥720 横向虚线导线 (.wire 母题);
   `scroll-behavior:smooth` + `scroll-padding-top` + `prefers-reduced-motion` 降级。
4. **交互 (原生实现)**: SHA/命令深色条右缘复制钮 (clipboard API, 非安全上下文回退
   execCommand; 成功「已复制」/失败「复制失败」1.5s 自散, aria-live=polite);
   截图点击放大 = 原生 `<dialog>` (无 JS 退化为新标签原图, Esc/背板/关闭钮均可关)。
5. **文案**: web-copy.md 逐字落地; 按裁决: 版本统一 v1.7.1; 「串口服务器」仅在
   title/meta description; Hero 主钮=站内 #download、GitHub 钮=仓库外链; 国内镜像 =
   一行筹备中小字 (未做禁用按钮); macOS 卡标「未签名」+ 区块下方放行提示框;
   全站无「已验证」类措辞。
6. **截图区布局**: 主图 dashboard 通栏 → 2 列 (create-bridge / drawer) → flow-stats
   全宽横幅 (依 702 建议, 1256×234 超宽图不作网格等高项) → 2 列 (cli / tray)。
   6 张全部带 alt/figcaption (文案表逐字) + width/height 属性防 CLS + loading=lazy。
   素材已自带圆角/边框/阴影 (702 烤入), 故容器不再叠 CSS 边框, 避免"框上加框"。

## 3. 交付自检 (全过)

| 项 | 结果 |
|---|---|
| 本地资源探测 (http.server 8018, urllib 逐个) | **8/8 相对资源 200** (favicon.svg / icon.png / 6 张 shot-*.png), 断链 **0** |
| 首页 + 子资源加载 (playwright networkidle) | 失败请求 **0** |
| 外链可达 (urllib HEAD, GitHub 实测) | **9/9 = 200**: 仓库 / releases / CHANGELOG / LICENSE / SECURITY / 手册 (中文路径已百分号编码) / 三个 v1.7.1 安装包直链 |
| console 报错 / pageerror | **0 / 0** |
| 360/720/1440 横向滚动 | 三档 scrollWidth == clientWidth, **无溢出** (截图留档) |
| 数据块渲染 | version=v1.7.1 / SHA 前缀 788fec…/ Windows 链接正确 / hero 小注同步 |
| 复制按钮 | 点击后剪贴板收到 64 位完整 SHA, 「已复制」1.5s 后复位 |
| 截图放大 | dialog 打开 ✓, Esc 关闭 ✓ (背板/关闭钮同测) |

## 4. 实现取舍备案 (与规范条文的三处微差)

1. **figcaption 用 muted 非 faint**: ui 规范 §2.4 写 faint, 与 §1.5 "必读说明一律 muted"
   冲突; 图注承载文案组正字, 按 §1.5 取 muted。请 707 按此口径走查。
2. **截图容器去边框**: §2.4 要求 1px 边框+阴影, 但 702 素材已烤入同款视觉
   (圆角/描边/投影+透明外扩), 叠加会双框, 故仅保留圆角裁切。
3. **JS 渲染下载数据**: 派工技术要求 `const RELEASE` 渲染 vs ui 规范 §2.5 "不用 JS 渲染
   (保 SEO/无 JS)" 存在张力; 按派工实现, 以静态回退 + noscript 兜底两者兼顾 (见 SQ-3)。

## SPEC-QUESTION (交架构师裁决)

1. **SQ-1 (已按裁决执行, 备案)**: 文案表 macOS 卡规格行「约 1.9 MB」与权威数据 1.8 MB
   冲突, 按「权威下载数据逐字采用」取 **1.8 MB**。
2. **SQ-2**: ui 规范 §2.4 图注 faint vs §1.5 必读 muted 冲突 (见取舍 1), 已取 muted;
   若裁定改回 faint 仅需改一处 CSS。
3. **SQ-3**: RELEASE 对象渲染后, 正文静态回退文本 (现为 v1.7.1 正确值) 未来逐版是否要求
   709 同步刷新? 不刷新不影响页面显示 (JS 覆写) 与访客, 但 HTML 源码会残留旧版本号,
   可能干扰 707 源码级内容核对。建议: 裁定「渲染以 RELEASE 为准, 707 只核渲染后 DOM」。
4. **SQ-4 (备案)**: Hero 版本小注「当前版本 v1.7.1 · 三平台安装包」出自 ui 规范 §6 第 5 层
   建议, 文案表无此句; 属数据性小字非营销文案, 已落地并由 RELEASE 渲染。
5. **SQ-5 (沿用文案组 SQ-3)**: SECURITY.md「支持版本」表仍写 1.2.x 维护中, 页脚「安全说明」
   会把访客带去过时表格, 建议另派工修正 (本席未动 docs/)。

—— 706 功能开发组 · 完
