# Dev Sprint 5 后端报告 — retries 重试计数 (ADR-15①)

作者: 后端主程 (201) · 2026-09-13 · 依据: decisions.md ADR-15① / backlog「Sprint 5 · dev-backend」。
改动范围: `src/hub.rs`、`src/supervisor.rs`、`src/fleet.rs` (契约投影一处), 其余零改动, `Cargo.toml`
零新依赖。不 commit; 自测只用缺席的 COM99 (ADR-15③ 演练口) + 127.0.0.1:8199, 进程已按 PID 清场
(tasklist 复核无残留)。

## 1. 语义 (ADR-15① 落地)

`retries: u32` = **当次会话内重试计数**, 存于 HubState (单一真相, 与 phase/lastError 同锁), 只有
监督任务写。写入点全部在 `supervisor.rs::run_supervisor` 的相位迁移处:

| 事件 | 迁移 | retries |
|---|---|---|
| 打开尝试失败 (`opener.open` 返回 Err) | → Retry | **+1** (`retry_inc`, 饱和不回绕) |
| 打开成功后会话异常掉线 (monitor 返回 !user_closed) | → Retry | **+1** (同上, 也是一次尝试失败) |
| 打开成功 (`→ Open`) | → Open | **归 0** (`clear_retries`) |
| 重试等待中被 Close 打断 (wait_retry = false) | → Closed | **归 0** (用户手动 close) |
| 已打开时用户 Close (user_closed break) | → Closed | 保持 0 (打开成功时已归 0) |

设计取舍: 计数在**监督任务**而非 HTTP 层维护 —— Open/Close 都只是异步指令, 只有监督任务知道
每次尝试的真实结果; 相位迁移点即权威计数点, 与 /api/status 的 phase 同帧更新, 不会出现
"phase=retry 但 retries 滞后"的撕裂窗口。

## 2. 契约变更 (QA 契约测试随动)

| 端点 | 变更 | 字段数 |
|---|---|---|
| GET /api/status (单桥兼容模式, `api::status_core`) | + `"retries": u32` (serde 原名, 无 rename 需求) | 11→**12** |
| GET /api/fleet 桥对象 (`fleet.rs::Bridge::detail_json`, 列表行) | + `"retries": u32` | 13→**14** |
| GET /api/fleet/<id> 详情 (同 detail_json, 列表项同构) | + `"retries": u32` | 13→**14** |

- 三处同源: status 走 `HubState::status_json()`, fleet 走同一 `status_json()` 投影再组装 —— 单一
  真相, 无双份计数。
- 语义示例 (真进程黑盒实测): `{"phase":"retry",...,"lastError":"打开 COM99 失败: 系统找不到指定的
  文件。","retries":17,...}` —— UI 侧 ADR-15② 可直接消费该值渲染「正在自动重连 (第 n 次)」。
- 已知内部字段 (QA 契约集之外): fleet 桥对象另有 `running`/`autoOpen`, 本变更未触碰。

## 3. 单测清单 (53→56, 全绿)

| # | 测试 (位置) | 断言 |
|---|---|---|
| 1 | `supervisor::tests::retries_increment_per_failed_attempt_and_reset_on_success` (新增) | 假打开器 `fail_first(3)` 连败 3 次 → 每次 Retry 迁移 +1, 第 3 次失败后稳定窗口内 `retries==3` 且 `/api/status` 投影同值; 第 4 次成功打开 → 归 0 (status 同验) |
| 2 | `supervisor::tests::retries_reset_on_user_close_during_retry` (新增) | 打开失败 1 次 → `retries==1`; 重试等待窗口内 Close → Closed 且 `retries==0` |
| 3 | `hub::tests::status_json_contract_exact_keys_and_defaults` (同步) | status 契约字段集 11→12 (HashSet 精确相等 + `retries` 初值 0) |
| 4 | `fleet::tests::fleet_bridge_object_contract_14_fields` (新增) | 列表行与单桥详情同构: 14 契约字段 (QA conftest FLEET_ROW_FIELDS 13 + retries) 全在, 且除已知内部字段 (running/autoOpen) 外无未裁定字段; `retries` 为非负整数初值 0 |

另: FakeOpener 增 `fail_times` 行为 (前 N 次失败之后成功), 原 `fail/stuck_ms/die` 语义不变
(stuck_ms 睡眠在失败判定前, Opening 相位可观察性测试不受影响)。时序型测试 #1 连跑 5 遍稳定。

## 4. 验证

- `cargo test` **56/56** (新增 3 条), 剩余 3 条警告均为本次之前已存在 (service.rs ×2 unused-mut,
  cli.rs startup dead-code), 未顺手清理 (非本工单所有权)。
- 真进程黑盒 (headless, `--port COM99 --no-fleet`, 管理台 127.0.0.1:8199 / 数据面 8200):
  /api/status 与 /api/fleet、/api/fleet/b1 的 retries 随 1s 重试递增 (3s→4, 16s→17, 一致);
  POST /api/close 后两处同步归 0。ADR-15④ 未动数据面: WS 客户端语义零变化 (本工单无数据面改动)。
- QA 侧随动提醒: conftest.FLEET_ROW_FIELDS 13→14 (加 retries, u32) + status 12 字段断言, 归 QA 波1。
