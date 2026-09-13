# Dev Sprint 6 后端报告 — autoReconnect 每桥可选 (FR-12 / ADR-16①)

作者: 后端主程 (201) · 2026-09-13 · 依据: decisions.md ADR-16① / spec FR-12 /
backlog「Sprint 6 · dev-backend」。改动范围: `src/hub.rs`、`src/supervisor.rs`、
`src/cli.rs`、`src/api.rs`、`src/fleet.rs`; `Cargo.toml` 零新依赖。不 commit;
自测只用 COM2 (真开成功) / COM99 (缺席, 确定性打开失败) + 127.0.0.1:18080,
COM8 未触碰; 进程按 /api/shutdown 优雅清场 (tasklist 复核无残留)。

## 1. 配置链路 (全打通)

CLI `--reconnect` / `--no-reconnect` (**默认 true**, 重复给出时后者生效) →
`Cli.auto_reconnect` → `ManagerStartup::from_cli` → `CliBridgeSpec.auto_reconnect`
→ `BridgeSpec.auto_reconnect` → `Bridge::new` → `HubState.set_auto_reconnect`。
HTTP 侧:

| 入口 | 字段 | 缺省语义 |
|---|---|---|
| POST /api/fleet (建桥) | `autoReconnect` (bool) | 缺省 **true** |
| POST/PATCH /api/fleet/<id>/config | `autoReconnect` (bool) | 缺省 = 不改动 |
| POST /api/config (单桥兼容, FR-10h 分发) | `autoReconnect` (bool) | 缺省 = 不改动 (走 `apply_config_core`, 与 fleet config 同一路径) |

持久化: fleet.json 每桥记录新增 `"autoReconnect"` (serde 缺省 true —— 旧清单无此
字段按默认恢复, 不丢不炸); 变更即写, 恢复路径 (restore_fleet) 原样回灌。

## 2. 行为语义 (supervisor.rs)

autoReconnect=true 维持 FR-3/ADR-15 现状 (掉线→Retry→1s 重开, retries 计数不变)。
false 时:

| 事件 | 行为 | lastError |
|---|---|---|
| 已打开会话异常掉线 | **直接 Closed, 不进 Retry** | `"串口已断开 (自动重连已关闭)"` (固定话术) |
| 打开失败 (含手动 open) | **直接 Closed, 不进 Retry** | 真实失败原因 + `" (自动重连已关闭)"` 后缀 (修复轮 D1: 如 `"打开 COM99 失败: 系统找不到指定的文件。 (自动重连已关闭)"`) |

裁定说明: ADR-16① 要求 closed 态 lastError 一律注明「自动重连已关闭」(QA 计划
a/f 断言, 修复轮 D1 落实); 掉线用固定话术, 打开失败保留真实原因并追加同款后缀
—— 两者共同点是**都不循环**。
手动 open 在 false 下仍可单次尝试 (成功→Open; 失败→Closed+lastError)。
**运行中改 false 即时生效**: `wait_retry` 每 25ms 轮询开关, Retry 等待中翻 false
立即转 Closed (清 retries), 不等满间隔; 已 Open 的活会话不受改配影响 (下个决策点
才读新值)。实现上 `wait_retry` 以 deadline+分段 sleep 重写, Close/Open「催一下」
语义原样保留。

## 3. 契约变更 (QA 契约测试随动)

| 端点 | 变更 | 字段数 |
|---|---|---|
| GET /api/status (单桥, `api::status_core`) | + `"autoReconnect": bool` | 12→**13** |
| GET /api/fleet 列表行 / GET /api/fleet/<id> (`detail_json`) | + `"autoReconnect": bool` | 14→**15** |

`autoReconnect` 存于 HubState (单一真相), fleet 桥对象与单桥 status 同源回显,
无撕裂窗口。回显即持久化即真相, UI 可直接存回 (FIX-17 同类风险已闭环)。

## 4. 测试 (cargo test 56→**64** 全绿)

新增 8 项: supervisor 4 项 (`reconnect_false_open_fail_goes_closed_no_retry` /
`reconnect_false_drop_goes_closed_with_fixed_message` /
`flip_false_during_retry_stops_immediately` / `reconnect_true_keeps_retrying_after_drop`)
+ hub 契约 13 字段与翻转回显 + CLI 两态解析 (`reconnect_flags`) + fleet 端到端
(`fleet_auto_reconnect_default_create_config_and_persist` /
`compat_legacy_config_auto_reconnect_accepted`); 原桥对象契约测试 14→15 字段随动,
fleet.json 往返/恢复用例补字段断言。备注: `HubState::retries()` 的 dead_code 警告
在 HEAD 即存在 (仅测试引用), 与本波无关未动。

## 5. 真机自测 (127.0.0.1:18080, debug 包)

- **Run A** `--port COM99 --no-reconnect`: 单桥 /api/status 13 字段、
  `autoReconnect:false`、phase=closed、retries=0 恒 0 (打开失败一次即停, 不循环),
  lastError=真实原因; fleet 桥对象同样回显 false; fleet.json 落盘 `"autoReconnect": false`。
- **Run A2** 建桥缺省: 不带 autoReconnect 建 COM2 桥 → 回显 true 且**真开成功
  (phase=open, 真硬件)**; PATCH `{"autoReconnect":false}` 受理, 详情回显 false,
  活会话不受影响 (语义正确); 落盘 [b1=false(COM99), b2=false(COM2)]。
- **Run B** 缺省重连 + 运行中翻转: 不带 flag → phase=retry、retries 3s 内爬到 4
  (true 维持重试循环); legacy POST /api/config `{"autoReconnect":false}` 受理 →
  2s 内 phase=**closed** (即时生效, 不等满间隔); 手动 /api/open 单次失败 → 仍 closed
  不循环; /api/shutdown 优雅退出, tasklist 无 serialhub 残留。

## 6. 对接备注

- QA: 单桥 status 13 字段 / fleet 桥对象 15 字段已随单测钉住 (conftest
  FLEET_ROW_FIELDS 若有镜像需同步 +1); false 语义黑盒可用 COM99 (缺席) 复现
  Run B 的路径, 真拔插演练口不变 (ADR-15③)。
- UI: 重连开关建议直连 POST/PATCH config 的 `autoReconnect`, 回显读 status/fleet
  同名字段, 语义即改即生效无需重启。
- 后端遗留: 无; Sprint 6 波 1 后端部分完成, 等待 UI (UI-1 控件尺度) 与 QA 黑盒。
