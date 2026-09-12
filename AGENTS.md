# AGENTS.md — 接手必读 (任何智能体/开发者落在此目录先读这里)

## 项目现状速览

- **是什么**: SerialHub —— 串口↔WebSocket 桥接工具 (跨平台单二进制 Rust + 内嵌 Web 控制台)。
- **阶段**: Sprint 1 (核心桥 MVP)。进度与分止单一事实来源 = `docs/team/backlog.md`。
- **本机测试资源**: COM1↔COM2 为 ELTIMA 虚拟串口对 (已验证 8N2 / 2Mbaud 回环);
  COM8 是真板 CH340, **测试禁止占用 COM8**。Web UI 测试用 Playwright
  (`NODE_PATH="$(npm root -g)" node <script>`, 浏览器已缓存)。

## 文件地图

```
src/            Rust 实现 (Dev 所有)
ui/             内嵌 Web 控制台 (include_str! 进二进制; Dev 所有)
tests/          Python 集成一致性套件, pytest (QA 所有)
docs/product/   产品规格 spec.md —— 需求编号 FR-x/UI-x/PERF-x, 改需求先改这里
docs/team/      charter.md 团队章程 · goal-dev/qa/ux.md 各角色常驻指令
                backlog.md 待办池(单一事实来源) · decisions.md 架构决策记录
                reports/   每轮 QA/UX/Dev 报告落这里
```

## 硬约束 (违反 = 验收不通过)

1. `/ws` 数据通道**只传原始二进制帧**, 不做任何 JSON 包帧 —— 页面侧自带协议栈。
2. 串口读循环**永不阻塞**在单客户端上; 掉线必须自动重开并向状态接口上报。
3. 多客户端: RX 广播给**所有**客户端; TX 经队列串行化 (FIFO 仲裁), 不得 panic。
4. 不得写死 COM 号/波特率; 一切经 CLI 参数与运行时配置。
5. 测试只许用 COM1/COM2; **禁止碰 COM8**。

## 常用命令

```bash
cargo run -- --list-ports                    # 列串口
cargo run -- --port COM1 --baud 115200 --config 8N2 --addr 127.0.0.1:8080
cargo test                                   # Rust 单元测试
python -m pytest tests/ -v                   # 集成一致性套件 (QA 所有)
python -m pytest tests/ -v -k broadcast      # 单跑某类
```

## 提交约定

中文约定式提交 `feat|fix|test|docs(scope): ...`, AI 提交带 `Co-Authored-By: Claude` 尾。
每个 Sprint 由架构师 (编排会话) 验收后提交, 角色不自行 commit 主干。
