# QA Sprint4 修复复验报告 (603 复验员)

日期: 2026-09-13 · 复验人: qa-603 · 结论: **全部通过, 无阻塞缺陷**

## 1. 回归测试

| 套件 | 期望 | 实际 | 结果 |
|---|---|---|---|
| `python -m pytest tests/ -q` | 36/36 (含已修 test_fr9b) | 36 passed, 0 failed (62.7s) | PASS |
| `cargo test` | 53/53 | 53 passed; 0 failed (2.33s) | PASS |

## 2. 修复抽验 (target/release/serialhub.exe, cargo build --release 最新)

环境: `--headless --addr 127.0.0.1:8080 --port COM1`

- **DEF-1 (maxClients 回显)**: POST `/api/config` `{"maxClients":4}` → `{"ok":true}`; GET `/api/status` → `"maxClients":4` 回显正确。GUI 设置面板"同时连接上限"同步显示 4, 一致。**PASS**
- **DEF-2 (/api/fleet 字段)**: GET `/api/fleet` → 桥对象含 `"maxClients":4`, 速率字段 `"rxRate":0.0` / `"txRate":0.0` 均存在。**PASS**

## 3. 新手代言四问 (GUI 模式 COM2, 浏览器 127.0.0.1:8080, 零文档视角)

截图均在 `output/playwright/`。

| # | 问题 | 判定 | 一句话依据 | 截图 |
|---|---|---|---|---|
| ① | 3 秒看出几座桥/哪座在跑 | **能** | 顶部"我的桥 (2)"给总数, 每卡绿色"● 运行中"徽章一眼可辨 | q1-landing.png |
| ② | "新建桥"入口找得到 | **能** | 左上角唯一蓝色主按钮"＋ 新建桥", 表单预填"桥 3"、按钮"创建并启动" | q2-new-bridge.png |
| ③ | 流程图/徽章/速率不看词表能懂 | **能** | "串口 COM1 ⋯ 桥 ⋯ 网址"三段图自明; 速率写"收·串口→网页 / 发·网页→串口"方向明确; 统计页"连接数/运行时长/最近 60 秒"全白话 | q3-stats-tab.png |
| ④ | 有无卡壳的实现词 | **基本无** | 全站未见"重启/换绑/监听/客户端"等实现词 (上限叫"同时连接上限"); 唯一协议痕迹是可复制 URL 里的 `ws://…/ws`, 但旁有白话解释"网页/程序打开「数据网址」就能连到这个串口", 不构成卡壳 | q4-settings-dialog.png |

## 4. 顺带记录 (不修)

- 浏览器控制台唯一报错: `favicon.ico` 404, 纯外观小瑕疵。
- GUI 模式启动会自动恢复上次会话的桥 (本次恢复了 headless 留下的 COM1 桥), 属设计行为且页脚已说明"桥的配置自动保存, 关掉再打开会原样恢复", 非缺陷; 测试时需注意端口残留。

## 5. 环境清理

headless 与 GUI 两个 serialhub 进程均已 taskkill, 8080/8081/8082 无残留监听, `tasklist` 无 serialhub 进程。未 commit, 未改任何代码。
