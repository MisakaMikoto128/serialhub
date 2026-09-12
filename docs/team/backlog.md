# SerialHub 待办池 (单一事实来源 — 架构师维护, 角色只改「状态」列)

状态: ☐ 待做 · ◐ 进行中 · ◑ 待验收 · ✔ 完成 · ✖ 砍掉

## Sprint 1 — 核心桥 MVP (当前)

| 优先级 | 条目 | 对应 | 状态 |
|---|---|---|---|
| P0 | FR-1/2/3/4/5/7 后端骨架: CLI、axum HTTP+WS、串口配置/打开/自动重开、广播扇出、TX 队列、状态接口 | spec §2 | ✔ (QA 24/24 ×2 轮) |
| P0 | FR-6 控制台 v1: 连接面板 + 终端 (ASCII/HEX) + 发送区, 内嵌单文件 | spec §3 | ✔ (FIX-1~8 + NEW-1 点验 3/3) |
| P0 | Rust 单元测试: 配置解析、状态机、计数器 (21 项) | spec §8 | ✔ |
| P0 | QA 一致性套件 24 条, 连跑 2 遍全绿; PERF 基线超标 8 倍 | goal-qa | ✔ |
| P1 | UX 第一轮实用报告 (25 张截图, P0=0 P1=6) | goal-ux | ✔ |
| P1 | **修复轮 (反馈回流)**: FIX-1~8 全修, QA 复验/点验全过 | 回路④⑤ | ✔ 2026-09-12 |

### 修复轮工单 (来源: ux-sprint1.md P1 + qa-sprint1.md §5/§6)

| # | 条目 | 来源 |
|---|---|---|
| FIX-1 | 终端按 WS 帧边界断行 → 每行劈成两条 (1B+85B)、行计数翻倍: 入站帧做 ~20ms 合并缓冲再断行; 顺带去掉 "+/=" 前缀 (P2-2 并入) | UX P1-1 |
| FIX-2 | HEX 视图折行把一个字节劈成两半: 按字节边界分组折行 | UX P1-2 |
| FIX-3 | 非法 HEX 输入、越界波特率静默拒绝: 就地红色错误提示, 不发送 | UX P1-3 |
| FIX-4 | ASCII 视图 0x80+ 字节显示为乱码字形: 与 0x00-1F 统一受控占位 (·) | UX P1-4 |
| FIX-5 | 宽屏计数器数值截断 (RX `656,9…`): CSS 修复 | UX P1-5 |
| FIX-6 | spec UI-4 时间戳开关未实现: 终端行加可开关时间戳 | UX P1-6 |
| FIX-7 | README 注明 "WS 消息边界≠串口帧边界" (ADR-6⑤) 与 Lagged 丢旧帧策略 | QA OBS-1/5 |
| FIX-8 | NEW-1 无换行二进制流渲染竞态: 内容重复 ~2×、内嵌 0x0A 断行丢失、carry 乱序 (qa-sprint1-fix.md NEW-1) —— 渲染层单一 flush 路径, 杜绝定时器/消息双写 | QA 复验 NEW-1 |

**Sprint 1 验收**: 2026-09-12 通过 —— cargo test 21/21 · pytest 24/24 · UX P0/P1 清零 · NEW-1 点验 3/3。

## 反馈池 (UX/QA 报告 → 分诊后入 Sprint)

| 来源 | 条目 | 优先级 |
|---|---|---|
| QA 点验 | flush 边界短行 (<512B) 与 Dev "行长恰 512B" 表述出入, 令 Dev 知悉/对齐语义 | P2 |
| QA OBS-5 | Lagged 丢帧 dropped 计数上 status (契约变更) | P2 |

## Sprint 2 — 桌面客户端形态 (当前, 用户直接下达)

| 优先级 | 条目 | 对应 | 状态 |
|---|---|---|---|
| P0 | FR-8 GUI 壳: 原生窗口(WebView 内嵌现有控制台) + 托盘(四态图标/菜单/关窗即后台) + --headless | spec FR-8 | ◑ 待验收 (托盘菜单人工确认项见 dev-sprint2 §4) |
| P0 | QA: 套件夹具改 --headless, 全量回归; FR-8 的 CLI 面黑盒核对 | spec §8 | ☐ QA |
| P1 | UX: 窗口/托盘行为实用报告 (关窗→托盘→恢复→真退出, 浏览器控制台并存) | goal-ux | ☐ UX |

## Sprint 2 候选 (验收后圈定)

| 优先级 | 条目 | 对应 |
|---|---|---|
| P1 | PERF-1/2/3 正式达标与压测脚本固化 (基线已录: 吞吐 7327/10353 kbps, p95 0.61ms) | spec §5 |
| P1 | PLAT-1 GitHub Actions 三平台 CI (win 验证, linux/mac 编译) | spec PLAT-1 |
| P2 | status 增加 dropped 计数 (Lagged 丢帧可见, 契约变更需裁定) | QA OBS-5 |
| P2 | 终端导出收发日志 (csv/bin) | UX 反馈池 |
| P2 | 深色模式 | UI-2 |
| P2 | Windows 预编译发布包 (x86_64-pc-windows-msvc zip) | 调研结论 |
| P2 | /api/ports 防误导默认选中; flow 字段回显 | QA OBS-4 / Dev SQ-3 |
| P2 | README 英文版 + GIF 演示 | 传播 |
