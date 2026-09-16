# 官网合规席报告 —— WEB-5 安全合规 + 原创性核对 (席位 710, 2026-09-15)

## 结论先行

1. **安全合规**: 总体 PASS。无追踪脚本、无 Cookie、无任何浏览器存储、无外部字体/CDN,
   许可/免责/SEO meta 齐全。唯一例外为 **1 项 P2**: 页面加载即 `fetch` GitHub API,
   与页脚"无外部请求"自述矛盾 (详见 C-06, 不属 WEB-5 字面禁止项, 不阻塞验收)。
2. **原创性 (对照 qingjian.app)**: 逐区块对照**无文案雷同项**; 信息架构相似属规格明示
   允许范围 (website-spec.md L11-13 "只借鉴信息架构")。视觉/代码级因外站样式与源码
   无法经 WebFetch 程序化取得, **无相似证据**, 留 1 条人工目视复核建议 (非阻塞)。

- 被测对象: https://misakamikoto128.github.io/serialhub/ (线上) + `website/index.html`
  (本地源码, 754 行, 经核对线上 RELEASE 数据块与本地一致: v2.0.2 / 2026-09-15)。
- 核验方式: 全文通读 + grep 证据 + curl 线上 head 核验 + WebFetch qingjian.app 踏勘。
- 许可基准: 仓库根 `LICENSE` 为 **Apache License 2.0** (附 `NOTICE`, 第三方依赖自带许可)。

---

## 一、安全合规清单

| # | 项 | 结论 | 证据 |
|---|---|---|---|
| C-01 | 无追踪/统计脚本 | **PASS** | `grep -inE "analytics|gtag|beacon|umami|plausible|matomo|piwik|hotjar|sentry|posthog|clarity|baidu|hm\.baidu|cnzz|googletagmanager|tracking"` 零命中 (exit=1)。全文仅两处 `<script>` (L21-49 RELEASE 数据块, L654-752 本站交互脚本: 渲染数据块/版本新鲜度自检/复制按钮/lightbox), 均为站内自有代码, 无任何第三方脚本标签。符合 WEB-8 "无访问统计埋点"。 |
| C-02 | 无 Cookie 写入 | **PASS** | `grep -iE "document\.cookie|setCookie"` 零命中。无任何 Set-Cookie 场景 (纯静态站, 页面自身无后端)。 |
| C-03 | 无 localStorage 滥用 | **PASS** | `grep -iE "localStorage|sessionStorage|indexedDB|ServiceWorker"` 零命中 —— 浏览器存储 API **完全未使用**, 不存在存储内容与隐私面。用户偏好均不入站内存储 (产品主题等记忆功能在 serialhub.exe 侧, 与官网无关)。 |
| C-04 | 无外部字体/CDN 资源依赖 | **PASS** | 字体为纯系统字体栈 (L74-75: `"Segoe UI","Microsoft YaHei","PingFang SC"` / `Consolas,"Cascadia Mono",...`), 无 `@font-face`、无 `url()`、无 `@import`、无 `srcset` (grep 全部零命中)。图标全部内联 SVG (L378, L401-426)。全部资源走相对路径 `./assets/*` (favicon.svg/icon.png/og-image.png/6 张截图/demo.mp4/demo-poster.jpg, 均在 `website/assets/` 站内自持)。 |
| C-05 | 外部 URL 全量清点 | **PASS** | 页面全部绝对 URL 共 4 类: ① github.com 仓库/Releases/CHANGELOG/LICENSE/SECURITY.md/手册 = 纯跳转与下载外链 (L25-27, L33/39/45, L505/518/531, L540/549/617, L629/633-635), 非资源依赖; ② `127.0.0.1:8080` (L578, L580) = 展示文本非请求; ③ 本站自身 og:url/og:image (L14-15); ④ **api.github.com (L679) —— 见 C-06, 唯一例外**。无其他第三方域名。 |
| C-06 | 页脚"无外部请求"自述 vs GitHub API fetch | **P2** | L679: `fetch("https://api.github.com/repos/MisakaMikoto128/serialhub/releases/latest")` 页面加载即触发 (版本新鲜度自检, WEB-2 配套); L644 页脚却写 "本站无 Cookie、无统计脚本、**无外部请求**"。矛盾点: ① 自述不实 (实浏览器下确有一次站外请求); ② 该请求使 GitHub 侧可见访客 IP/UA, "隐私零负担" (WEB-5) 打折。缓解: 非追踪用途、无 Cookie、失败静默 (L695 `.catch`)、无 JS 则不触发。建议二选一: (a) 页脚改措辞如 "无 Cookie、无统计脚本、无第三方追踪"; (b) fetch 改为用户点击"检查更新"才触发, 或直接移除 (静态回退文本已足够)。**归 Dev/发布工程组决断, 本席只测不改。** |
| C-07 | 开源许可展示 | **PASS** | 页脚 L629: "SerialHub 基于 [Apache License 2.0] 开源发布", 链接指向仓库 LICENSE 文件, 与仓库实际许可 (根 `LICENSE` = Apache-2.0 全文, `NOTICE` 声明版权与第三方依赖条款) 一致。 |
| C-08 | 免责/风险提示 | **PASS** | ① macOS 包未签名: 下载卡 spec 注明 "未签名" (L528, L543), 独立提示条给出放行步骤 (L544 "右键点文件选「打开」…即可放行"); ② 安全风险: 页脚 L639 "管理台与数据通信未加密、无鉴权，请仅在本机或可信内网使用，勿暴露到公网" 且链接 SECURITY.md (L635); ③ SHA256 校验指引含篡改提示 (L546-547)。 |
| C-09 | 隐私声明 | **PASS (受 C-06 牵连)** | L644 "本站无 Cookie、无统计脚本、无外部请求" 构成对访客的隐私声明, 方向正确; 但因 C-06 所述 fetch 存在, "无外部请求" 一句在实浏览器下不成立, 建议随 C-06 一并修正措辞, 使隐私声明可被逐字兑现。 |
| C-10 | WEB-7 SEO 基础齐全 | **PASS** | title (L7) / description (L8) / og:type·og:title·og:description·og:url·og:image (L11-15, og:image 为绝对 URL 指向站内 og-image.png) / favicon 双份 SVG+PNG 回退 (L9-10)。**线上实测** (curl): 七项 meta 全部在部署页 head 中, `assets/og-image.png` 与 `assets/favicon.svg` 均 HTTP 200。sitemap 按规格列 P2 未做, 不扣分 (website-spec.md L46)。可选增强 (不计缺陷): og:image:width/height、twitter:card、canonical。 |
| C-11 | 附加安全卫生 | **PASS** | 所有 `target="_blank"` 链接均带 `rel="noopener"` (L377/448/455/461/468/475/481/505… 共 15 处, 无遗漏); 无内联事件处理器; 无 `eval`/`innerHTML` 动态拼 HTML (L685-692 用 createTextNode); `<noscript>` 降级指引 (L540); 复制按钮回退 `execCommand` 仅作用于本站文本 (L698-710)。 |

---

## 二、原创性核对 (对照 qingjian.app)

qingjian.app 本次 WebFetch **抓取成功** (2026-09-15, 渲染文本级踏勘), 与本项目官网逐区块对照:

| 区块 | qingjian.app (踏勘实录) | SerialHub 官网 (源码行号) | 判定 |
|---|---|---|---|
| 导航 | 站名"青简 Qingjian"; 菜单: 首页/文档/下载/隐私/关于 | L356-367: 站名 SerialHub; 菜单: 特性/截图/下载/快速开始/更新日志 | 仅"站名+锚点菜单"框架同, 菜单项无一相同。信息架构层, 规格明示允许。 |
| Hero | 标语"输入的不只是文字。"; 标签行"全在本机 · 一次一种语言 · …"; 按钮"下载体验/查看文档" | L370-389: 标语"让网页和脚本直接读写串口"; 平台 pill 为 Windows/Linux/macOS 锚点跳转; 按钮"下载 SerialHub/GitHub 仓库" | 文案零重合。"双按钮+平台标签"为 WEB-1 结构表自定要求 (spec L20), 属允许借鉴的 IA。 |
| 特性区 | 两组共 8 卡: "全在本机/候选旁的译词/生词标橙/越用越顺/先是一个好用的输入法/…" | L399-430: 六卡 "多桥并存管理台/串口热拔插自动重连/实时速率统计与流程图/主题插件/桌面客户端与命令行双形态/单文件免安装零依赖" | 领域 (输入法 vs 串口桥接) 与全部卡片标题、说明文案零重合。 |
| 中段区块 | 理念区 + 交互演示 + 反面清单"不打算做什么" (五条否定项) | 截图区 (L435-488) / 下载区 (L491-554) / 快速开始 (L557-586) / 更新日志 (L589-619) | 区块集合实质不同 (本站为下载型官网, 对方为理念型产品页), 无对应可比较项。 |
| 页脚 | "© 2026 青简 Qingjian"; 无许可声明; 附诗句"不积跬步…" | L623-646: Apache-2.0 声明 + 仓库/手册/安全链接 + 安全提示 + "© 2026 SerialHub · 本站无 Cookie…" | 仅 "© 2026 + 站名" 格式通用雷同 (不构成相似项); 对方无许可声明而我方有, 内容实质不同。 |
| 视觉 | WebFetch 仅返回渲染文本, 无法取得其配色/字体/卡片样式 | 本站为自有 `--wh-*` 设计令牌体系 (L52-104, 注释标注来源 704 web-ui.md) | **无相似证据**; 但非双向程序化比对, 留人工复核 (见下)。 |
| 代码级 | 未能取得对方源码 (无 class 命名/构建器特征可提取) | 本站零依赖手写 (无框架/无构建), `grep` 无第三方注入痕迹 | **无相似证据**; 同上留人工复核。 |

**结论**: ① 文案雷同 —— 无 (逐区块、逐标题、逐按钮比对零重合); ② 视觉抄袭 —— 无证据;
③ 代码级相似 —— 无证据。信息架构相似 (单页/导航/hero 双按钮/特性网格/页脚) 为规格
L11-13 "只借鉴信息架构, 其文案/视觉/代码一律不抄 (ADR-23④)" 的执行结果, 不属违规。

---

## 三、需人工跟进项

1. **C-06 (P2, 建议本轮处理)**: L679 `fetch(api.github.com)` 与页脚 L644 "无外部请求"
   矛盾。改页脚措辞或把 fetch 降为显式交互触发 —— 归 Dev/发布工程组决断。
2. **原创性视觉/代码级终核 (非阻塞)**: WebFetch 只能取 qingjian.app 渲染文本, 其 CSS
   与源码未程序化取得。建议人工用浏览器并排打开两站目视比对一眼 (配色/卡片风格/圆角
   阴影语言), 作为验收第 5 条的最终闭环留档。

*(本报告只测不改, 未动任何产品文件; 未启动 serialhub.exe, 未触碰 COM 口。)*
