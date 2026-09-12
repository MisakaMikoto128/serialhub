# 团队章程 — 开发/测试/使用反馈 回路

## 角色

| 角色 | 职责 | 文件所有权 | 常驻指令 |
|---|---|---|---|
| **架构师 (编排会话)** | 定规格、派工、验收、更新待办池、commit | docs/** 、git | — |
| **开发 Dev** | 实现/修复, 跑 cargo test | src/ ui/ tests/rust/ Cargo.toml | goal-dev.md |
| **测试 QA** | 一致性套件 (COM1↔COM2), 出 PASS/FAIL 矩阵报告 | tests/ (python) reports/qa-*.md | goal-qa.md |
| **体验官 UX** | 以真实用户姿态用 Web 控制台, 出反馈报告 | reports/ux-*.md + 截图 | goal-ux.md |

## 回路 (每个 Sprint 必须完整走一遍)

```
Sprint 开始: 架构师从 backlog 圈定本 Sprint 范围 (P0 → P1 顺延)
  ① Dev    : 按 goal-dev 实现, 自跑 cargo test, 写 reports/dev-sprintN.md
  ② QA     : 按 goal-qa 跑/扩一致性套件, 写 reports/qa-sprintN.md (PASS/FAIL 矩阵)
  ③ UX     : 按 goal-ux 实用一轮, 写 reports/ux-sprintN.md (P0/P1/P2 反馈条目)
  ④ 架构师 : 三份报告合并分诊 → 回写 backlog (新条目/修复项/升降级)
  ⑤ Dev    : 修复轮 (只动反馈条目), QA 复验失败项
  ⑥ 架构师 : 验收口径 (spec §8) 达标 → commit → 下一 Sprint
```

## 派工方式

智能体经编排会话的 Agent 工具生成; **每次派工 = 一段自包含 prompt**:
先贴对应 goal-*.md 全文, 再附本 Sprint 范围、当前仓库路径、验收口径。
智能体无记忆 —— 一切上下文以仓库文件为准, 汇报一律写成 reports/ 文件 (而非只写在回话里)。

## 冲突规则

- 文件所有权表以外不许写; QA 与 Dev 分离是为了让"失败"不被写代码的人自己消化掉。
- UX 反馈**不得**直接改代码, 只提报告; 由架构师分诊后才成为 Dev 任务。
- 任何角色发现规格问题 → 报告里写 `SPEC-QUESTION:`, 由架构师裁决后改 spec.md。

## 完成的定义 (DoD)

1. `cargo test` 零失败; 2. `python -m pytest tests/` 全绿; 3. UX 报告无未关闭的 P0/P1;
4. backlog 状态列与实际一致; 5. 报告三件套齐全 (dev/qa/ux)。
