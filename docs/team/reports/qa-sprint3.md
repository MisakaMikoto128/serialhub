# QA 报告 — Sprint 3 复测 (FR-9 配置全量双入口 + ADR-9/10)

日期: 2026-09-12 · 角色: QA · 被测: 重建产物 `target/release/serialhub.exe` (cargo build 0 警告, 6.0s)
方法: 黑盒 —— 契约断言修订 (QA 文件所有权, 按 ADR-9) + FR-9 增补测试 5 条 + 全量回归
结论: **回归 29/29 零回归 (cargo test 30/30 与 Dev 一致); FR-9b/9a/FR-2 对等全部 PASS**

---

## 1. 旧断言修订清单 (ADR-9①, QA 文件所有权)

| # | 文件 | 修订 | 内容 |
|---|---|---|---|
| 1 | `tests/conftest.py` | 契约常量 | `STATUS_FIELDS` 9→**10** 字段: 新增 `maxClients`; 注释 ADR-5① → ADR-9①(修订) |
| 2 | `tests/test_fr4_status.py` | 断言 | `test_fr4_status_contract`: 字段集断言引用修订后常量; **新增 `maxClients == 0` 默认值断言** (0=不限) |
| 3 | `tests/conftest.py` | 台架扩展 | `start_bridge` 新增 `flow=` / `max_clients=` 参数 (生成 `--flow` / `--max-clients` CLI); `make_peer` 新增 `port_name=` 参数 (FR-9a 桥在 COM2, 对端需用 COM1) |
| 4 | `tests/test_fr9_config_full.py` | 新增文件 | FR-9 增补 5 条 (见 §2) |

## 2. 回归 (修订后全量)

| 项 | 结果 | 证据 |
|---|---|---|
| Rust 单元测试 | PASS | `cargo test`: **30 passed / 0 failed** (+5 FR-9 用例, 与 Dev 报告一致) |
| 一致性套件 | **PASS** | `python -m pytest tests/ -v`: **29 passed / 0 failed** (50.3s) = 旧 24 条 (含修订后 status 契约) + 新增 5 条 |
| 进程卫生 | PASS | 结束后 `tasklist` 无 serialhub.exe; COM8 未碰 (仅 --list-ports 类枚举会见到, 未占用) |

## 3. FR-9 核对矩阵 — 5/5 PASS

| 条目 | 结果 | 证据 (实测) |
|---|---|---|
| FR-9b `--max-clients` 限制 | **PASS** | `--max-clients 1` 起桥: status `maxClients=1`; **第 1 条 WS accepted**; 第 2 条 WS 升级完成后收 **`Close(code=1013, reason="max clients")`** (websockets 异常逐字段核对); 被拒连接**不计入 clients** (`clients=1`) |
| FR-9b API 解除 | **PASS** | `POST /api/config {"maxClients":0}` → 200 `{"ok":true}` → status `maxClients=0` → 新 WS 连接保持 OPEN → `clients=2` (旧 1 + 新 1) |
| FR-9a 自我重启 E2E | **PASS** | GUI 实例 (`--port COM2 --addr 127.0.0.1:8081 --max-clients 3 --flow xonxoff`, phase=open, maxClients=3) → `POST /api/restart {"addr":"127.0.0.1:8085"}` → 200 `{"ok":true}` → **旧进程 ≤3s 退出** → 新实例 8085 就绪: `phase=open, port=COM2, baud=115200, config=8N2, maxClients=3` **全部保留**; 进程级断言: 恰 1 个 serialhub 进程且 PID ≠ 旧 PID; **WS→串口数据面恢复**: WS 发 256B 图案 → COM1 对端 (pyserial) 逐字节一致 —— flow (xonxoff) 经重启保留并到达串口 (flow 不在 status 10 字段, 以数据面验证, 与 Dev 自验同法) |
| FR-9a headless 守卫 | **PASS** | headless 实例 `POST /api/restart {"addr":"127.0.0.1:8087"}` → **400 + `{"ok":false,"error":"…重启进程…"}`**; 2s 后 status 仍 200 且相位不变 (服务不退出) |
| FR-2 对等 `--flow` 回环 | **PASS** | `--headless --flow xonxoff --port COM1`: WS 发 512B 可打印图案 (避开 XON 0x11/XOFF 0x13 字节) → COM2 对端 (pyserial) **逐字节一致** —— flow 参数真实生效到达串口 |
| FR-2 对等 `--flow bogus` 拒启 | **PASS** | `--flow bogus`: 进程 5s 内退出且退出码非 0 (拒绝带非法流控运行) |

## 4. 环境与台架

- Windows 11 · rustc 1.97.1 · pytest 9.0.3 / pyserial 3.5 / websockets 16.0 · 夹具统一 `--headless` (GUI 默认模式, Sprint2 适配沿用)
- 端口: 套件随机空闲端口 + FR-9a 固定 8081→8085、headless 守卫 8087; **8080 未用** (Sprint2 已取证其存在外部进程 ZCode.exe 连接干扰, 本轮全程避开)
- 串口: FR-9a 桥在 COM2 + 对端 COM1; flow 回环桥在 COM1 + 对端 COM2; **COM8 未碰**
- 清理: restart E2E 的新 GUI 实例以 `kill_all_bridges()` (taskkill /IM) 收口, 全部结束后 0 残留

## 5. 结论摘要 (≤8 行)

1. 契约修订完成: status 断言 9→10 字段 (maxClients, 默认 0), 旧用例按 ADR-9① 同步, 零回归。
2. 回归: cargo test 30/30, pytest **29/29** (旧 24 + 新增 FR-9 5 条) —— 数据面 /ws 帧语义零改动得证。
3. FR-9b PASS: --max-clients 1 → 第 2 条 WS 收 Close(1013,"max clients") 不计入 clients; API {"maxClients":0} 解除后新连接 accepted。
4. FR-9a PASS: GUI 自我重启 8081→8085, 旧 ≤3s 退、新实例 phase=open 且 baud/config/maxClients 全保留, WS→COM1 对端逐字节恢复 (flow 保留实证); headless 400 守卫 + 服务不退。
5. FR-2 对等 PASS: --flow xonxoff 回环逐字节生效; --flow bogus 拒启。
6. Dev 报告的"实现顺序偏离"(先释放 COM 再 spawn) 在本环境实测无 retry 间隙、无 COM 撞锁, 与 spec 对外行为一致。
7. COM8 未碰, 测毕 0 残留。**Sprint 3 复测通过。**

## 修复轮复验 (架构师代行, 2026-09-12 — QA agent 配额受限)
- cargo test 31/31 (含修复后的 1013 拒连用例) · pytest 29/29 全绿
- 契约黑盒 (release 重建后): /api/status 11 字段齐, `--flow xonxoff` → flow=="xonxoff" 回显,
  POST /api/config {"flow":"none"} → 即时生效; /api/shutdown 干净退出 0 残留
- 期间发现并修正验证方法学问题: 修改契约后必须 `cargo build --release` 再做 HTTP 黑盒
  (旧 exe 会给出假阴性)
