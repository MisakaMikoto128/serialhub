# web-assets.md — 702 美术组产出报告 (2026-09-14)

- 任务: website/assets/ 素材加工 (截图精修 / og-image / favicon 迁移)。
- 依据: docs/product/website-spec.md §2 截图区 + WEB-4; goal-web.md 702 节。
- **Pillow 可用性: 可用, Pillow 10.4.0** (未走降级方案, 全部处理真实完成)。
- 红线遵守: 截图内容零改动 (无增删像素级 UI 元素, 仅缩放+观感处理);
  docs/images/ 与 assets/ 原件只读; 未触碰 website/index.html; 未 commit。

## 1. 截图精修 (6 张)

统一处理 (只做观感, 不改内容): LANCZOS 等比缩放 → 2× 超采样圆角
(1200 组半径 14px / 900 组 12px) → 1px 浅灰细边框 (#d2d7dc) → 轻阴影
(黑 α58, 高斯模糊 12px, 下移 6px); 外扩透明画布 (左右上 28px / 下 36px) 容纳阴影。

| 输出 (website/assets/) | 来源 (docs/images/, 只读) | 源尺寸 | 内容尺寸 | 画布尺寸 |
|---|---|---|---|---|
| shot-dashboard.png | dashboard.png | 2560×1600 | 1200×750 | 1256×814 |
| shot-drawer.png | bridge-drawer.png | 2560×1600 | 1200×750 | 1256×814 |
| shot-flow-stats.png | flow-stats.png | 2560×362 | 1200×170 | 1256×234 |
| shot-create-bridge.png | create-bridge.png | 2560×1600 | 900×563 | 956×627 |
| shot-cli.png | cli.png | 2576×560 | 900×196 | 956×260 |
| shot-tray.png | tray.png | 2560×1510 | 900×531 | 956×595 |

说明: 高度按 `round(源高×目标宽/源宽)` 取整 (与精确等比差 ≤1px, 视觉无失真)。

## 2. og-image.png (社交分享图)

- 尺寸 1200×630 (RGB, 无透明边), 约 110 KB。
- 构图: 产品底色 = 品牌绿垂直渐变 #2f9e5f→#1f7a47 (**取自 assets/favicon.svg
  自带渐变**, 延续产品自身设计语言, WEB-4; 未参考任何外部站点配色)。
- 左侧: icon-256 (84px, 既有资产) + 产品名「SerialHub」(微软雅黑粗体
  C:/Windows/Fonts/msyhbd.ttc, 92px, 白) + 副标「串口 ⇄ WebSocket 桥接管理器」
  (msyh.ttc 36px, 白 90%)。
- 右侧: dashboard.png **真实截图裁切** 源矩形 (0,140)–(1090,1023), 缩至
  500×405, 加浅灰边框 + 圆角 + 投影; 内容与 shot-dashboard 同源, 未虚构。

## 3. favicon 迁移 (字节级原样复制, 未改动)

| 输出 | 来源 |
|---|---|
| website/assets/favicon.svg (748 B) | assets/favicon.svg |
| website/assets/icon.png (256×256 RGBA) | assets/icon-256.png |

## 4. 交付物清单

`website/assets/` 共 9 个文件: shot-*.png ×6, og-image.png, favicon.svg, icon.png。
处理脚本为一次性内存执行 (未落盘脚本文件); 参数已如实记录于上文, 可复现。

## SPEC-QUESTION (交架构师裁决)

1. **副标「⇄」字形**: 微软雅黑 cmap 声称含 U+21C4 但无实际轮廓 (PIL getmask
   误报非空, 实渲染为空框)。og 副标已改为「串口 + 手绘双向箭头 + WebSocket…」
   (箭头呼应 UI 内「串口→网页/网页→串口」母题, 语义不变)。若 706 网页正文也用
   「⇄」, 访客端字体不可控, 建议页面用内联 SVG 箭头而非该字符。
2. **flow-stats 排版**: 该源图是 2560×362 超宽单卡片, 1200px 宽下仅 170px 高,
   与其他截图不等高。建议 706 截图区将其作全宽横幅而非网格等高项, 否则会破版。
3. **cli.png 源宽 2576** 与其余 2560 不一致 (非同一裁切基准), 已按 900px 等比
   缩放未额外裁切; 若要求统一 2560 基准需 QA/Dev 重截, 美术组不代改原件。
4. **截图内真实运行数据** (速率/时长/IP 等) 原样保留未修饰; 若发布前希望刷新
   数据观感, 请安排重截后再走一遍本流程。
