# 贡献指南 (CONTRIBUTING)

谢谢关注 SerialHub。本项目有**两条贡献轨**, 按你的习惯选一条。

## 轨道 A: 传统 PR 轨

适合: 改代码、修 Bug、补文档的一次性贡献。

1. Fork 本仓库, 从 `main` 拉分支: `git checkout -b fix/xxx`。
2. 动手前扫一眼 [docs/product/spec.md](docs/product/spec.md) (需求编号 FR-x/UI-x 在这里)
   与 [docs/team/decisions.md](docs/team/decisions.md) (架构决策 ADR-x) —— 改需求先改 spec, 不写离经叛道的代码。
3. 开发环境:

   | 组件 | 版本/要求 |
   |---|---|
   | Rust | stable (见 Cargo.toml) |
   | Python 3 | pytest, pyserial (集成套件用) |
   | 串口资源 | 一对虚拟串口 (com0com 等) 供 `tests/` 使用; 本机无虚拟对时部分用例会跳过 |
4. 提交前自测全绿:

   ```bash
   cargo test                     # Rust 单元测试
   python -m pytest tests/ -v     # 集成一致性套件
   ```

   UI 改动请另附截图或前后对比说明。
5. 发 PR: 中文描述「改了什么 / 为什么 / 怎么验的」, 关联对应 Issue。

**测试硬纪律** (违反 = 直接驳回): 测试只许用虚拟对端口; 不得写死 COM 号/波特率;
`/ws` 数据通道只传原始二进制帧, 不加包帧。

## 轨道 B: 多智能体回路轨 (本仓库特色)

SerialHub 由一个**多智能体团队**开发迭代: 架构师编排 → Dev 实现 → QA 黑盒测试 →
UX 以真实用户姿态体验, 反馈回流待办池, 循环推进 (详见
[docs/team/charter.md](docs/team/charter.md) 与 [docs/team/org.md](docs/team/org.md) 编制)。

想以这种方式贡献:

1. **认领席位**: 读 [docs/team/org.md](docs/team/org.md) 选一个空席 (Dev/QA/UX 等);
2. **领任务**: 在 [docs/team/backlog.md](docs/team/backlog.md) 待办池里认领一个条目 (或提新条目经架构师排期);
3. **跑回路**: 按对应角色常驻指令执行 —— [goal-dev.md](docs/team/goal-dev.md) /
   [goal-qa.md](docs/team/goal-qa.md) / [goal-ux.md](docs/team/goal-ux.md);
4. **交报告**: 报告落 [docs/team/reports/](docs/team/reports/), 命名 `角色-sprintN.md`,
   内容包含: 测试清单 × 结果、发现的问题分级 (P0/P1/P2)、遗留与交接事项;
5. **等验收**: 架构师按 [charter.md](docs/team/charter.md) 的 DoD 验收后合入。

**角色不自行 commit 主干** —— 每轮由架构师验收后统一提交。

## 提交约定

- 中文约定式提交: `feat|fix|test|docs(scope): 一句话说明`, 如
  `feat(fleet): 桥对象新增 autoReconnect 字段`。
- AI 参与的提交带尾行: `Co-Authored-By: Claude`。
- 小步提交, 一次一件事; 测试与实现同提交。

## 行为准则

请读 [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)。安全漏洞请走 [SECURITY.md](SECURITY.md),
不要开公开 Issue。
