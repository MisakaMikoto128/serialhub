# QA Sprint 4 计划/清单 — FR-10 桥接管理器黑盒套件 (2026-09-13)

作者: QA (401) · 依据: spec FR-10 / decisions.md ADR-13 · backlog「Sprint 4 · qa-plan」·
任务契约 (控制面固定 --addr; GET/POST /api/fleet, /api/fleet/<id>/start|stop|delete,
GET /api/fleet/<id>, WS /api/fleet/<id>/tap; 数据面 ws://<listen>/ws; --fleet 持久化自动恢复)。
纪律: 黑盒, 预期只来自 spec, 失败不改预期; 只用 COM1/COM2 (禁 COM8); 每条测试自管进程
(起停/taskkill 清理, 沿 conftest 模式); 不 commit。

## 1. 套件构成 (新增 3 文件 + conftest 扩展区, 旧 29 条零改动)

| 文件 | 内容 |
|---|---|
| `tests/conftest.py` | 追加「FR-10 扩展区」: `start_fleet`(控制面进程工厂)/`fleet_ready`(就绪门控)/建桥与列表行工具/`ws_collect_exact`+`ws_push`/`hard_kill`/`take_port`/`wait_port_free`; 另给旧 `start_bridge` 加了 `cwd` 参数 (仅兼容测试隔离默认 fleet.json 落盘用, 旧路径行为不变) |
| `tests/test_fr10_fleet.py` | 任务 1/2/3/5/6 共 5 条 |
| `tests/test_fr10_fleet_persist.py` | 任务 4 持久化 1 条 |
| `tests/test_fr10_fleet_compat.py` | 任务 7 兼容 1 条 |

**就绪门控**: 会话级探针起一次控制面进程并 GET /api/fleet; 端点不存在 → 7 条全部
`SKIPPED [BLOCKED-BY-BACKEND]`。这不是改预期, 而是把"实现未到位"与"实现违约"区分开;
后端落地后探针自动放行, 即书即跑。

## 2. 套件清单 × 验证点 × 当前状态

| # | 测试 | 验证点 (spec 依据) | 状态 |
|---|---|---|---|
| 1 | `test_fr10a_two_bridges_coexist_and_isolate` | 双桥 (COM1/COM2 各占一端, 优先 8101/8102) 同时 open; id 唯一; 跨对回环逐字节 (8101⇄COM1⇄COM2⇄8102 链路, 见注①); 静默守窗无多余字节 (无串扰/回显); 逐桥 rx/tx 计数对账 (A: tx=p1/rx=p2, B: 对称); 断开后 clients 归零 | **BLOCKED-BY-BACKEND** |
| 2 | `test_fr10g_fleet_crud_and_list_contract` | 建桥成功; 列表恰一行且 12 字段齐全 (id/name/serial/listen/phase/clients/rx/tx/rxRate/txRate/lastError/uptimeSec) + 类型/相位合法 (FR-10c/g); GET /api/fleet/<id> 详情 200; start→open; stop→closed; delete→列表移除+详情 ≥400+端口可重绑 (释放) | **BLOCKED-BY-BACKEND** |
| 3 | `test_fr10_create_rejects_occupied_listen` | 同 listen 建第二桥被拒 (400 含"占用"或 ok:false); 被拒不污染列表, 原桥仍 open | **BLOCKED-BY-BACKEND** |
| 4 | `test_fr10b_fleet_json_survives_hard_kill` | --fleet 落盘 (变更即存, 非退出才存); taskkill /F 断电式强杀; 同 --fleet 重启自动恢复两桥, listen 不变 (FR-10f), phase=open; 停 B 释放 COM2 后经 A 回环验证数据面可用 | **BLOCKED-BY-BACKEND** |
| 5 | `test_fr10f_listen_stable_and_peer_reconnect` | 桥 listen 跨 create/open/对端断接/stop/start 五次读取不变; 对端 (COM2 peer) 断开→重接, 数据面自愈字节精确、桥相位不扰动 (FR-3/FR-10f); 缺席串口 (COM99) 桥 start 后 phase=retry 且 lastError 非空 (FR-3 状态机每桥可见), retry 态可直接 delete | **BLOCKED-BY-BACKEND** |
| 6 | `test_fr10g_tap_receives_serial_rx_alongside_data_plane` | WS /api/fleet/<id>/tap 收到串口 RX 字节 (512B 逐字节); 同期数据面 ws://listen/ws 同流不缺; tap 挂接期间数据面 TX 不受扰 | **BLOCKED-BY-BACKEND** |
| 7 | `test_fr10h_legacy_single_bridge_maps_to_one_fleet_row` | 旧单桥 CLI 进程: /api/fleet 恰见一座桥 (serial=COM1, phase=open, listen=旧地址端口); 旧 /api/status 恰 11 字段不变 (ADR-11); 旧 /ws 双向回环不回归; 兼容桥 tap 同样可旁看 | **BLOCKED-BY-BACKEND** |
| — | 既有 29 条回归 (任务 7 后半) | `python -m pytest tests/` | **29 passed** (54.6s), 0 失败, 无进程/端口残留 —— 现有套件零改动即全绿 |

注① (拓扑): 本机仅 COM1↔COM2 一对虚拟串口, 双桥同时 open 时每桥各占一端, 唯一可行回环
= 两桥互为对端组成链路 8101 ⇄ COM1 ⇄ (虚拟对) ⇄ COM2 ⇄ 8102; "互不串扰"由静默守窗
(多 1 字节即 FAIL) + 逐桥计数对账双保险断言。真实拔插无法用 ELTIMA 程序化仿真,
任务 5 的 FR-3 面以「缺席端口 retry 态可见 + 对端重接自愈」两切片覆盖可观测行为 (与 test_fr3 同口径)。

## 3. 契约假设 (spec 未定死, 实现若有出入以裁定为准, 不属"改预期")

1. **POST /api/fleet 请求体**: `{name, serial, baud, config:"8N1", dataBits, parity, stopBits, listen:"127.0.0.1:<port>"}` —— config 串与展开字段同时携带 (沿 /api/config 与 CLI 两种风格, serde 默认忽略未知字段, 低风险); listen 传完整 host:port (与 --addr 同风格)。
2. **/api/fleet 信封**: 裸列表或单列表字段字典均可解包 (conftest `fleet_rows`); 行内以 `id` 字段定位, id 类型不限 (str/int)。
3. **tap 帧格式**: 任务契约只要求"收到串口 RX 字节", 收集器容忍混入文本帧; 数据面收集器保持 FR-1 严格二进制。
4. **恢复相位语义**: 重启后恢复为重启前的运行态 (open→open), 即"自动恢复全部桥"= 连运行态一并恢复 (任务契约明示 phase=open)。
5. **兼容桥 listen**: 旧参数进程的数据面必须仍是旧地址同端口 (旧 /ws 兼容的前提), 故 fleet 行 listen 应含该端口。

## 4. 结论 / 下一步

- 后端 (dev-backend 波 1) 尚未落地: `--fleet` 为未知参数 (进程退出码 2), GET /api/fleet 不存在 →
  7 条新测试全部 [BLOCKED-BY-BACKEND] 跳过, 套件本身就绪。
- 实现到位后: 直接 `python -m pytest tests/ -v` 全量跑; 若上述假设与实现出入, 回架构师裁定契约,
  再改测试 (注明依据), 不许反向凑绿。
- 旧 29 条不受 Sprint 4 影响, 本轮实跑全绿; conftest 改动仅为追加 + `cwd` 可选参数, 旧路径行为不变。
