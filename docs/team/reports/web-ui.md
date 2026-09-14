# web-ui.md — 发布官网视觉规范 (704 UI 组, 2026-09-14)

> 上游: docs/product/website-spec.md (WEB v1) · docs/team/goal-web.md「704 UI 组」· 产品设计系统
> (ui/index.html `:root` 令牌段 + assets/themes/light.css/dark.css) · spec.md §3 UI-0/UI-1。
> 读者: 706 功能开发组 (照此直接落 website/) · 702 美术组 (素材规格) · 707 测试组 (走查口径)。
> 本席只出规范不写页面; website/index.html 归 706。视觉基调: **浅色工业风, 与产品同族但更轻盈**。

---

## 0. 总原则 (四条, 先于一切细节)

1. **同族不同密度**: 官网沿用产品的令牌思路与色板基因 (同一套蓝、同一套灰阶、同一字体栈),
   但作为宣传页放宽呼吸感 —— 页面底更亮、区块留白更大、正文字号比管理台 (14px) 大一档。
2. **令牌集中 `:root`**: 全站样式只允许消费 `--wh-*` 变量, 不许出现裸色值/裸尺寸
   (对齐 UI-2 的做法); 唯一例外是主题色板注释。
3. **零依赖**: 无框架/无构建/无外部字体/无图标 CDN (WEB-1/WEB-5)。图标一律内联 SVG
   (currentColor 描边), 字体走系统栈, 与产品 `--sans/--mono` 完全同栈。
4. **克制**: 无渐变彩带、无大圆角卡片、无动效堆砌。装饰手段限定为: 细边框、留白、
   等宽字眉标、虚线"导线"母题 (源自产品流程图 `.wire` 的 `stroke-dasharray:5 4`)。

---

## 1. 设计令牌表 (全部 `--wh-*` 前缀, 共 46 条)

### 1.1 色板 (18 条) —— 从产品令牌派生, 官网更亮

| 令牌 | 值 | 用途 | 派生关系 |
|---|---|---|---|
| `--wh-bg` | `#f7f9fb` | 页面底 | 产品 `--bg #eef1f4` 提亮一档, 更通透 |
| `--wh-panel` | `#ffffff` | 面板/卡片底 | 同产品 `--panel` |
| `--wh-panel-2` | `#f6f8fa` | 徽章/标签/输入类浅底 | = 产品 `--panel-2` (官网复用作次级底) |
| `--wh-ink` | `#1c2630` | 标题/正文主色 | 同产品 `--ink` |
| `--wh-muted` | `#5f6b76` | 次级文字 (副标/说明) | 同产品 `--muted` |
| `--wh-faint` | `#8b96a1` | 眉标/小注/占位 | 同产品 `--faint` (仅限装饰性小字, 见 1.5) |
| `--wh-accent` | `#0e6db8` | 主色 (按钮/链接/图标) | 同产品 `--accent`, 品牌蓝不动 |
| `--wh-accent-hover` | `#0b5c9c` | 主色悬停 | 同产品 `--accent-hover` |
| `--wh-accent-ink` | `#ffffff` | 主色上的文字 | 同产品 `--accent-ink` |
| `--wh-border` | `#d3dae1` | 常规边框 | 同产品 `--border` |
| `--wh-border-strong` | `#b7c1cb` | 强边框 (页脚顶/卡悬停) | 同产品 `--border-strong` |
| `--wh-code-bg` | `#f6f8fa` | 行内代码底 | = 产品 `--panel-2` |
| `--wh-code-ink` | `#1c2630` | 行内代码字色 | = `--wh-ink` |
| `--wh-term-bg` | `#14171a` | 深色命令/SHA 条底 | = 产品深色主题 `--bg` (终端血统) |
| `--wh-term-ink` | `#e8eaed` | 深色条字色 | = 产品深色主题 `--ink` |
| `--wh-term-border` | `#2c313a` | 深色条边框 | = 产品深色主题 `--border` |
| `--wh-term-faint` | `#828c99` | 深色条内小注 | = 产品深色主题 `--faint` |
| `--wh-ok` | `#1e8e4e` | "最新版本"圆点/成功点缀 | 同产品 `--ok` |

> 不引入 `warn/err/gray/tx/rx`: 官网无错误与双向流量语义, 需要时再扩 (保持令牌面最小)。
> 深色命令条是全页唯一的深色面 —— 给 SHA256/命令行"终端感", 也为浅色页面提供视觉锚点。

### 1.2 字体与字阶 (8 条)

| 令牌 | 值 | 用途 |
|---|---|---|
| `--wh-sans` | `"Segoe UI","Microsoft YaHei","PingFang SC",system-ui,sans-serif` | 全站正文 (同产品) |
| `--wh-mono` | `Consolas,"Cascadia Mono","SFMono-Regular",Menlo,monospace` | 版本号/SHA/命令/眉标 (同产品) |
| `--wh-fs-hero` | `44px` | Hero 主标语 (700, 行高 1.2, 字距 0.5px) |
| `--wh-fs-h2` | `28px` | 区块标题 (700, 行高 1.25) |
| `--wh-fs-h3` | `18px` | 卡片/步骤标题 (600) |
| `--wh-fs-body` | `16px` | 正文 (400, 行高 1.7) |
| `--wh-fs-small` | `13px` | 小注/说明 (行高 1.6) |
| `--wh-fs-mono` | `14px` | 等宽内容 (SHA/命令/版本 chip) |

> 字阶按"1440 基准值"定义, 720/360 档在各媒体查询里**覆写同名令牌** (见 §3), 不新增字号令牌。
> 断行规则: 中文标题 `word-break:keep-all` + 手动 `<br>` 控点交给 703 文案标注, CSS 不做两端对齐。

### 1.3 间距 / 布局 (11 条)

| 令牌 | 值 | 用途 |
|---|---|---|
| `--wh-sp-1` … `--wh-sp-9` | `4 / 8 / 12 / 16 / 24 / 32 / 48 / 64 / 96 px` | 9 档间距 (4px 基数; 卡内小间距→区块留白) |
| `--wh-container` | `1100px` | 内容容器最大宽, 水平居中 |
| `--wh-nav-h` | `56px` | 粘性导航条高度 (锚点偏移量同源) |

> 容器左右内边距: 24px (≥720) / 16px (<720), 直接写死在容器规则, 不设令牌。
> 区块纵向节奏: 相邻区块之间 = `--wh-sp-9` (1440 档) / `--wh-sp-8` (720 档) / `--wh-sp-7` (360 档)。

### 1.4 圆角 / 阴影 / 控件 (10 条)

| 令牌 | 值 | 用途 |
|---|---|---|
| `--wh-radius` | `8px` | 卡片/命令条/按钮圆角 (同产品 `--ctl-radius`) |
| `--wh-radius-sm` | `4px` | 行内代码/小 chip (同产品 `--radius` 家族) |
| `--wh-pill` | `999px` | 平台标签/版本胶囊 |
| `--wh-shadow-1` | `0 1px 2px rgba(28,38,48,.06)` | 卡片静息态 |
| `--wh-shadow-2` | `0 6px 24px rgba(28,38,48,.10)` | 卡片悬停 / 弹层 |
| `--wh-shadow-3` | `0 10px 40px rgba(28,38,48,.16)` | 截图放大层 |
| `--wh-btn-h` | `40px` | 常规按钮高 (对齐产品 34 档的营销页放大版) |
| `--wh-btn-h-lg` | `48px` | Hero 主按钮高 |
| `--wh-btn-px` | `20px` | 按钮水平内边距 (产品 12px 的宣传页放宽) |

> 阴影色相用产品 `--ink #1c2630` 调 rgba, 不用纯黑 —— 同族且更柔和。
> 按钮/输入圆角统一 `--wh-radius`, 无第三种圆角 (对齐 UI-1 "禁第三档"精神);
> 控件字号 `--wh-fs-body` (按钮 16px) / 小按钮 14px 两档, 不再细分。

### 1.5 可达性注记 (707 走查用)

- `--wh-muted` 对 `--wh-panel` ≈ 5.4:1 (AA 过); `--wh-faint` 对白 ≈ 3.0:1 —— **faint 只许用于
  眉标/装饰小注, 不许承载必读信息** (必读说明一律 muted)。
- 主按钮白字对 `--wh-accent` 5.4:1 (产品 dark.css 已核算, 直接引用)。
- 深色命令条 `--wh-term-ink` 对 `--wh-term-bg` 13.1:1 (产品 dark.css 已核算)。
- `:focus-visible` 统一 `outline:2px solid var(--wh-accent); outline-offset:1px` (同产品)。

---

## 2. 区块布局 (对照 website-spec §2 八区块)

通用区块头模式 (除导航/Hero/页脚外所有区块):
居中或左对齐的 **眉标 + H2 + 一句 lead**:
眉标 = `--wh-mono` 12px、`--wh-faint`、全大写英文标签、`letter-spacing:2px`
(延续产品 `.dr-sec>h3` 与 `.node .cap` 的字距手法); H2 = `--wh-fs-h2`; lead = muted 16px 一行。

### 2.1 导航 (粘性)

- 高 `--wh-nav-h`(56px), `--wh-panel` 底, 底边 `2px solid var(--wh-border-strong)`
  (产品页头同款 2px 强边框, 全站唯一 2px 边框)。
- 左: 品牌块 = 10px `--wh-accent` 方块 (复用产品 `.brand-mark` 母题) + "SerialHub" 16px/700。
- 右: 锚点链接 特性 / 截图 / 下载 / 快速开始 / 更新日志, 14px, muted → hover 变 accent。
- 置顶方式: `position:sticky; top:0`, 无阴影, 仅底边框 (工业克制)。
- <720: 链接行 `overflow-x:auto` 横向滑动, 不换行不折叠汉堡 (零 JS, 五个词放得下)。

### 2.2 Hero (居中构图, 详见 §6)

- 垂直结构五层, 全部水平居中: 眉标 → 主标语 → 副标 → 双按钮 → 平台标签。
- 区块纵向内边距: 上 `--wh-sp-8` 下 `--wh-sp-9` (1440 档), 各档递减见 §3。
- 副标限宽 640px 居中; Hero 不放截图 (保 LCP 快 + 留白感), 首屏底线 = 下邻区块顶部 1px `--wh-border` 分隔线。

### 2.3 特性网格 (WEB-3)

- 栅格: ≥1440 三列 / 720–1439 两列 / <720 单列; 列距 `--wh-sp-5`。
- 卡片: `--wh-panel` 底 + 1px `--wh-border` + `--wh-radius` + `--wh-shadow-1`;
  hover 只做 `border-color:var(--wh-border-strong)` (产品 `.bcard` 同款, 无位移无抬升)。
- 卡内结构: 24px 内联 SVG 图标槽 (stroke 2px、currentColor、优先复用 FR-15 桥/端口母题资产)
  → 标题 `--wh-fs-h3` → 一句白话 `--wh-fs-small` muted。内边距 `--wh-sp-5`。
- 卡片数以 703 文案表为准 (4~6 条); 6 条时 1440 档 3×2, 4 条时建议末行居中或 2×2。

### 2.4 截图区 (WEB-4)

- 布局 = **一大图 + 说明 + 两列副图网格**: 主图 (dashboard.png) 占满容器宽,
  `figure > img + figcaption`; 副图 2 列网格 (`--wh-sp-4` 列距), <720 转单列。
- 图容器: 1px `--wh-border` + `--wh-radius` + `--wh-shadow-1`; 图圆角同容器 (overflow:hidden)。
- 必须写 `width/height` 属性 + `aspect-ratio` 占位 (防 CLS), 首屏外图 `loading="lazy"`。
- figcaption: 13px, `--wh-faint`, 图下方 `--wh-sp-2`; 主图 caption 可稍长 (一句功能指认)。
- 素材来自 docs/images 精修 (702 裁切统一); 本席建议入选: dashboard (主) +
  flow-stats / bridge-drawer / create-bridge (副); cli.png 可选用于快速开始, tray.png 不上首页
  (见 SPEC-QUESTION ③)。

### 2.5 下载区 (WEB-2, 页面核心)

- 区块头下方一行版本摘要: 版本 chip (mono, `--wh-panel-2` 底 + `--wh-pill` 胶囊) +
  发布日期 (13px faint) + 系统要求小注。
- 三平台卡片 **等宽三列** (≥1024px; 其余单列堆叠): 每卡结构自上而下:
  1. 平台名 16px/600 + 系统要求 12px faint (如 "Windows 10/11 x64");
  2. 主通道按钮「GitHub Releases 下载」= primary (`--wh-accent` 底白字), 通栏;
  3. 副通道按钮「国内镜像直传」= 次级 (panel 底 + `--wh-border-strong` 边, 同产品次级钮);
     两按钮下各挂一行 11px faint 适用网络注 ("国际网络"/"国内网络", 措辞对齐 709/710 口径);
  4. 分隔线后 SHA256 行: 标签 "SHA256" 11px 字距 1px faint + 等宽值
     (`--wh-term-bg` 深色条, 12–13px mono, `word-break:break-all`) + 右缘幽灵复制钮 (§4.3)。
- 卡片底色 `--wh-panel`, 边框同特性卡; 高度不强求对齐 (内容天然不等长)。
- **数据块纪律**: 版本/日期/三包链接/三个 SHA 集中写在 HTML 里一个带注释标记的数据块
  (如 `<!-- RELEASE-DATA v1.7.0 -->`), 纯静态、可全文搜索; 复制脚本读 DOM 取值。
  不用 JS 渲染数据 (保 SEO 与无 JS 可下载) —— 这是对 WEB-2 "单一数据块"的实现口径。

### 2.6 快速开始

- 步骤条: ≥720 横向三列, 列间用**虚线导线**连接 (`border-top:2px dashed var(--wh-border-strong)`,
  致敬产品 `.wire` 母题); <720 纵向时间线 (左侧虚线竖线 + 节点)。
- 每步: 序号方块 (28px, `--wh-radius-sm`, mono 数字, `--wh-panel-2` 底 + 1px 强边框) +
  步骤标题 `--wh-fs-h3` + 说明 14px muted。
- 步骤内命令一律进深色命令条 (`--wh-term-bg`, ghost 复制钮), 如
  `serialhub --port COM1 --baud 115200 --config 8N2`; 第三步给管理台网址
  `http://127.0.0.1:8080` 同款命令条。命令示例须与 README/手册一致 (WEB-3)。

### 2.7 更新日志

- 单列列表 (非卡片): 每条 = 版本 chip (mono 胶囊, 最新版加 `--wh-ok` 圆点) + 日期 (13px faint)
  + 2~4 条摘要 bullet (14px, muted); 条目间 1px `--wh-border` 分隔线, 不加底色 (轻)。
- 最新一条在上; 末尾右对齐"查看全部版本 →"链 GitHub Releases (外链, `rel="noopener"`)。
- 摘要由 703 自 CHANGELOG.md 提炼, 本规范只约束形态。

### 2.8 页脚

- 顶边 `2px solid var(--wh-border-strong)` (与导航呼应成框), 底色 `--wh-panel`, 三列:
  ① 品牌 + 一句定位 + "Apache-2.0" (链 LICENSE);
  ② 链接列: 仓库 / 使用手册 / 更新日志 / 安全建议 (链 SECURITY.md) —— WEB-8 清单全落此;
  ③ 安全提示段 (对齐 710 口径: v1.x 无 TLS, 请只在可信网络使用) 13px muted。
- 底行: 版权 + "本站无 Cookie、无统计脚本"一行 (710 隐私声明的站内落点), 12px faint。
- <720 三列纵向堆叠, 顺序 ①→②→③。

---

## 3. 断点规范 (三档: 360 / 720 / 1440)

实现方式: mobile-first, 两档媒体查询 `@media (min-width:720px)` 与 `@media (min-width:1440px)`
中**覆写令牌**, 组件规则不写死尺寸。示意 (非交付代码):

```css
:root{ --wh-fs-hero:28px; --wh-fs-h2:21px; /* …360 基准 */ }
@media (min-width:720px){ :root{ --wh-fs-hero:36px; --wh-fs-h2:24px; /* … */ } }
@media (min-width:1440px){ :root{ --wh-fs-hero:44px; --wh-fs-h2:28px; /* … */ } }
```

| 项目 | 360–719 (基准) | 720–1439 | ≥1440 |
|---|---|---|---|
| 容器 | 100% − 32px | 100% − 48px | 1100px 居中 |
| 字号 | hero 28 / h2 21 / h3 17 / 正文 15 / 小注 12 / mono 13 | hero 36 / h2 24 / h3 18 / 正文 16 / 小注 13 / mono 14 | hero 44 / h2 28 / h3 18 / 正文 16 / 小注 13 / mono 14 |
| 区块纵距 | `--wh-sp-7`(48) | `--wh-sp-8`(64) | `--wh-sp-9`(96) |
| 特性网格 | 1 列 | 2 列 | 3 列 |
| 截图区 | 全部单列 | 副图 2 列, 主图通栏 | 同左 |
| 下载卡 | 1 列 | 1 列 (≥1024 可升 3 列, 过渡规则) | 3 列 |
| 快速开始 | 纵向时间线 | 横向三列 + 虚线导线 | 同左 |
| 导航 | 链接横滑 | 完整展开 | 同左 |
| Hero 按钮 | 双钮纵向堆叠通栏 (主钮在上) | 双钮横排 | 同左 |

- 验收线: 360px **无横向滚动** (WEB-5); 深色命令条内的长 SHA 是唯一允许 `break-all` 的地方。
- 720–1023 为过渡带: 栅格按 720 档, 仅在宽度自然放得下处 (下载卡 ≥1024) 放宽。

---

## 4. 交互细节

### 4.1 锚点平滑滚动
`html{scroll-behavior:smooth}` + `html{scroll-padding-top:calc(var(--wh-nav-h) + 12px)}`
(粘性导航不遮锚点头); 配 `@media (prefers-reduced-motion:reduce){html{scroll-behavior:auto}}`。
零 JS。

### 4.2 截图点击放大
轻量方案: **原生 `<dialog>`**(约 15 行内联 JS): 缩略图包在 `<a href="大图" target="_blank">` 里,
JS 激活时 `preventDefault()` 改开 `dialog.showModal()` 放大层 (同 src 大图 + 关闭钮);
Esc / 点击背板关闭; `dialog::backdrop{background:rgba(28,38,48,.4)}` (与产品弹窗背板同值)。
无 JS 时退化为新标签原图 —— 不引库、不造 lightbox。放大层图宽上限 `90vw/90vh` 内自适应。

### 4.3 SHA 复制按钮反馈
- 形态: 复用产品 `.copy-ghost` 的"单一表面"模式 —— 幽灵图标钮 absolute 内嵌深色命令条右缘,
  静默态 `--wh-term-faint`, hover 转 `--wh-accent` (深底上 hover 用产品深色主题的提亮蓝思路)。
- 行为: `navigator.clipboard.writeText` (非安全 context 回退 `execCommand`) 成功后
  图标切换对勾 + 旁挂 `aria-live="polite"` 的"已复制" 1.5s 自散 (对齐产品 `#copyTip` 节奏);
  失败显示"复制失败"并保留手动选中可能。复制目标是 SHA 全串, 不带截断。

### 4.4 回到顶部 —— **裁定: 首版不做**
理由: ① 粘性导航常驻, 任意区块回跳一次点击, 浮钮无增量价值; ② 首版页面约 6 屏, 滚动负担小;
③ 360px 上浮钮遮挡正文且与下载 CTA 抢注意力, 违背克制原则。
重启条件 (P2): 页面扩充超 ~8 屏或新增长内容区时再议。

### 4.5 其他
- 导航"当前区块高亮" (scrollspy) 首版不做: 需常驻 IntersectionObserver, 视觉收益低;
  hover/focus 反馈足够。
- 外链一律 `target="_blank" rel="noopener"`; 全站零死链由 707 断链检查兜底 (WEB-5)。
- Hero 双按钮键盘顺序 = 视觉顺序 (下载 → GitHub), 天然符合焦点流。

---

## 5. 明确裁定 (三条, 均为"首版不做")

1. **深色模式: 首版不做, 列 P2。** 站点是 30 秒任务页 (看懂→下载), 非长时间驻留界面;
   深色版会把 707 的三档截图矩阵翻倍, 且 WEB-1 零依赖约束下无现成切换方案。
   产品侧 FR-14 主题体系是产品能力, 不构成官网义务 (UI-2 深色列 P2 的精神平移)。
2. **win95 彩蛋: 首版不用。** WEB-4 定性为"可选…不喧宾夺主", 首版选择不用, 理由:
   ① 官网首屏是首次访客对工具的**信任判断**现场, 复古梗对工控/嵌入式受众有"不严肃"误读风险;
   ② 彩蛋 = 一整套第二主题的素材 + 三断点 QA, 投入产出不成立; ③ 产品内该梗成立是因为用户
   已主动选择产品, 官网没有这层前置关系。留作后续版本里程碑彩蛋 (规格"可选"字样仍兑现)。
3. **站点不做主题切换** (无论深浅或 win95): 无 localStorage、无切换控件、无防闪白脚本;
   官网只有一套浅色令牌。理由: 下载页停留时长短, 切换价值趋零; 砍掉后 706/707 各减一类
   状态空间, 原创性核对 (验收⑤) 更简单。

---

## 6. Hero 构图建议 (纯文字)

自上而下五层, 全部水平居中, 纵向节奏 sp-2 → sp-4 → sp-6 → sp-5:

1. **眉标** (可选但建议): mono 12px 全大写、字距 2px、faint, 如 `SERIAL ⇄ WEB BRIDGE`
   —— 用等宽字 + 字距建立工业气质, 是产品界面小标题语言的原样平移; 也可换成
   "v1.7.0 · 开源 Apache-2.0"版本胶囊 (panel-2 底 pill), 二选一, 不叠用。
2. **主标语**: `--wh-fs-hero` 44px/700, ink, 一句定位 ≤20 字 (703 定稿), 一行放完,
   最多两行; 不加引号不加书名号。
3. **副标**: 17px muted, 1–2 句 (703 定稿), 限宽 640px 居中; 只说用户可感知的事
   (串口/网页/网址/下载/桥, WEB-3 词表)。
4. **双按钮**横排居中 (360 档纵向堆叠): **左 = 「下载安装包」** primary 48px 高
   (accent 底白字, 点击平滑滚到 #下载区 —— 让访客看到双通道与 SHA, 而不是直抛外链,
   见 SPEC-QUESTION ①); **右 = 「GitHub 仓库」** 次级 48px (panel 底 + 强边框 + ink 字,
   前置 16px 内联 GitHub 图标)。主行为居左占先 (LTR 首焦点)。
5. **平台标签**: 一行三枚纯文字 pill (Windows / Linux / macOS), 13px, panel-2 底 +
   1px `--wh-border` + `--wh-pill` 圆角, 居中 8px 间距; **不画 OS logo** (省素材且避商标绘制
   走样)。建议做成锚链接, 点 pill 直达下载区对应平台卡 (标签即导航, 功能密度+1)。
   pill 行下可再挂一行 12px faint 小注 "当前版本 v1.7.0 · 三平台安装包" (可并入数据块)。

Hero 整体不加背景图形/渐变; 唯一允许的装饰是极淡的顶部到 `--wh-panel` 的过渡或什么都不加,
由留白与字阶完成层级。

---

## SPEC-QUESTION (交架构师裁决)

1. **Hero「下载」按钮行为未定义**: website-spec §2 只写"双按钮 (下载 / GitHub 仓库)"。
   本规范按"站内锚点滚到下载区"实现 (访客可看到双通道 + SHA, 国内访客不被直抛 GitHub);
   若架构师裁定直链 Releases 直链, 请改 §6 第 4 层。影响 706。
2. **国内通道未上线期的下载区呈现未定义**: WEB-2 写"并列按钮并注明适用网络", 但国内
   Cloudflare 直链上线前, 副按钮是隐藏、禁用灰置还是链向 Releases? 本规范默认
   **禁用态灰置 + 文案「国内直传 · 即将上线」**(保留版位, 上线只改数据块), 请裁定。影响 706/709。
3. **截图素材清单需确认**: §2 写"docs/images 现有素材", 实有 6 张, 其中 tray.png (托盘)
   与 cli.png (命令行) 不属"管理台截图"。本规范建议首页用 dashboard/flow-stats/
   bridge-drawer/create-bridge 四张, cli.png 备选快速开始, tray.png 不上首页;
   请与 702 确认裁切定稿。
4. **平台口径**: 产品 spec PLAT-1 注明 Linux/macOS 仅"编译通过、未实机验证", 而 Hero
   平台标签三平台并列无差别。是否需在下载卡 Linux/macOS 卡内加一行"CI 构建, 未实机验证"
   类小注 (710 合规口径)? 本规范倾向加 (诚实且不碍下载), 请裁定。

—— 704 UI 组 · 完
