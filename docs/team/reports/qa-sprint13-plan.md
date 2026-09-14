# QA Sprint 13 计划/清单 — ADR-24⑤ 批次 B: FR-19 录制回放 + FR-22 导入导出 (2026-09-14)

作者: QA (401) · 依据: decisions.md ADR-24 (Sprint 13 批次 B) · spec FR-19/FR-22 ·
backlog「Sprint 13 — 批次 B (v2.0.0)」· 任务契约 (API 路径 / JSONL 行格式 / 回放参数)。
纪律: 黑盒, 预期只来自 spec + 任务契约, 不读 src/ 调预期; 串口仅 COM1(桥)↔COM2(对端)
ELTIMA 虚拟对, 全程禁碰 COM8; 测试端口 18200+; 进程按外来 PID 快照差集清理 (conftest
ADR-22 口径); 录像临时文件测毕即删; 不 commit。

## 1. 交付物与门控机制

| 项 | 说明 | 状态 |
|---|---|---|
| `tests/test_fr19_record_replay.py` | 5 用例: 录制形状对账 / 停止语义 / 回放前置 / 回放正途 / 路径穿越 | ✅ 已落地, `fr19_ready` 探针门控 |
| `tests/test_fr22_import_export.py` | 4 用例: export 对账 / merge / replace / 非法 schema | ✅ 已落地, `fr22_ready` 探针门控 |
| `tests/conftest.py` 增量 | `recordings_dir()` / `list_recordings()` / `delete_recordings()` / `post_accepted()` / `_fleet_probe()` + `fr19_ready` / `fr22_ready` 会话探针 (纯新增, 未动任何旧夹具) | ✅ |

门控 (BLOCKED-BY-BACKEND, 沿 `fleet_ready` 先例): 会话级探针起临时 fleet 进程 —
- `fr19_ready`: 建 closed 桥 (不 start, **零串口依赖**) → POST `/api/fleet/<id>/record/start`
  → 404 ⇒ 整组 skip (未实现时路由不存在, 与桥状态无关);
- `fr22_ready`: GET `/api/fleet/export` → 404 ⇒ 整组 skip。
dev-backend 落地后探针自动放行, 套件即书即跑, 测试代码零改动。

## 2. 用例清单 × 验证点 × 状态

### FR-19 录制与回放 (test_fr19_record_replay.py)

| # | 用例 | 验证点 (预期出处: spec FR-19 + 任务契约) | 状态 |
|---|---|---|---|
| R1 | `test_fr19_record_jsonl_shape_and_accounting` | record/start→stop 受理 (2xx); 起点前流量不入镜 (start 边界); JSONL 每行契约三键 ts/dir/hex 齐备 (多键 warning 记偏差); ts 正整数 ms 单调不减且近当前时刻 (±2min); dir∈{rx,tx}; hex 非空合法; RX/TX 按方向拼接与发送字节逐字节对账 (硬); 每方向帧数 ≥ 发送次数 (软下限, 允许按读写块拆并); GET recordings 列表含新文件; stop 后桥不受损 | ✅ PASS (2026-09-15 解锁, 见 §6) |
| R2 | `test_fr19_record_stop_halts_file_growth` | stop 后继续灌 RX: 无新文件、原文件大小不变 (停止语义) | ✅ PASS (2026-09-15 解锁, 见 §6) |
| R3 | `test_fr19_replay_rejected_when_serial_not_open` | COM99 缺席桥 (phase=retry, 永不 open) replay → 400/409 | ✅ PASS (2026-09-15 解锁, 见 §6) |
| R4 | `test_fr19_replay_delivers_in_order_paced_until_natural_end` | 手工构造 TX-only 录像 (契约行格式, 绝对 epoch ms) → replay(speed=1, loop=false) 受理; COM2 对端按序收全字节; 时序宽容断言: 实测跨度 ≥ 0.25×预期 (0.6s), 总耗时 ≤ 预期+8s (CI 慢 runner 容差); 自然结束静默守窗无多余字节 | ✅ PASS (2026-09-15 解锁, 见 §6) |
| R5 | `test_fr19_replay_rejects_path_traversal` | file="../Cargo.toml" 与反斜杠变体 → 400; 不存在文件 → 4xx | ✅ PASS (2026-09-15 解锁, 见 §6) |

### FR-22 导入导出 (test_fr22_import_export.py)

| # | 用例 | 验证点 | 状态 |
|---|---|---|---|
| E1 | `test_fr22_export_contains_existing_bridges` | export 200; body 合法 JSON; 桥集合与 GET /api/fleet 对账 (名称/串口/listen/桥数) | ✅ PASS (2026-09-15 解锁, 见 §6) |
| E2 | `test_fr22_import_merge_keeps_existing_and_adds_new` | 现有桥 id/name/串口/listen 原样不动 + 新增桥入列 (names 恰=旧+新) | ✅ PASS (2026-09-15 解锁, 见 §6) |
| E3 | `test_fr22_import_replace_swaps_whole_table` | 整表替换: 旧桥全消失 (id 定位为 None), 新桥恰在列 | ✅ PASS (2026-09-15 解锁, 见 §6) |
| E4 | `test_fr22_import_rejects_bad_schema_and_keeps_table` | 类型错 / 集合形状错 / 缺 bridges → 400; mode 越域 → 400; 失败导入不动现有表 | ✅ PASS (2026-09-15 解锁, 见 §6) |

状态图例: ☐ BLOCKED-BY-BACKEND (探针跳过, 落地后回填) → ✅ PASS / ❌ FAIL (附 bridge 日志)。
实测门控: 新 9 条在 6 轮全量运行中稳定 9 skipped (3s 内完成, 不拖累回归时长)。

## 3. 契约假设与留白 (落地后若有出入, 如实记录失败, 不改预期凑绿)

| # | 假设/留白 | 套件处理 |
|---|---|---|
| A1 | 回放源 ts 语义 (行间差值 vs 绝对时刻) spec 未定 | 源文件用绝对 epoch ms 构造, 两种实现下均应正确 |
| A2 | 回放对 rx 行的处理未定 (全量回放 vs 仅 tx) | 源文件仅含 tx 行, 两种语义均应正确; 且假设 replay 接受 recordings/ 内契约格式的文件 —— 若实现要求录像先经 record 登记, R4 会失败, 届时改 record→replay 回环并回填本条 |
| A3 | record/replay/recordings/import 响应体形状未定 | 只断状态码; JSONL 行恰三键 (缺键 FAIL, 契约外键 warning 记偏差) |
| A4 | import "json" 值编码 (对象 vs JSON 字符串) 未定 | 对象编码先试, 被拒再试字符串编码; 仅字符串受理时 warning 记偏差待裁定 |
| A5 | fleet 文档信封 (裸数组 vs {"bridges":[...]}) | 两者都认; merge/replace 源取自 export 回显 (天然同形), 新桥条目由现有条目派生 |
| A6 | 帧数对账口径 | 字节级对账为硬契约; 帧数 ≥ 发送次数为软下限 (实现可按读写块拆并, 字节不丢即可) |

## 4. 回归守护 (旧 63 条) — 本机为多席位共享机, 竞争性 flake 如实记录

并行席位活跃 (实测 26 个 python 会话, 且 COM1 曾被外部进程占用 PermissionError),
共享 COM1/COM2 对 + CPU 负载导致时序敏感用例在全量运行下随机 flake:

| 轮 | 结果 | 中招用例 (各轮均不同) |
|---|---|---|
| 1 | 59 passed / 2 failed / 2 error | fr12 重连环路, fr16a[debug], fr17a/c |
| 2 | 56 passed / 7 failed / 1 error | fr2_config, fr9a, fr17b 等 7 条 |
| 3 | 61 passed / 2 failed | fr11 retries, fr13b |
| 4 | 60 passed / 3 failed | fr1 integrity/tx_arbitration 等 |
| 5 | 59 passed / 4 failed | fr1 broadcast/tx_arbitration 等 |
| 6 | 61 passed / 2 failed | fr10g tap, fr10h compat |

**判定: 零确定性回归。** 证据: ① 各轮失败集互不相同、无任何用例两轮以上稳定失败;
② 每一条中招用例单跑/空闲窗口复验全部 PASS (fr12 ✓ fr16 ✓ fr17×3 ✓ fr1×3 ✓
fr11+fr13 10/10 ✓); ③ 新增代码纯增量, 且各轮失败用例按字母序多在 test_fr19 之前
执行 (早于任何新代码路径)。基线 63/63 需在独占机窗口复核; 建议后续引入串口互斥
(如跨席位文件锁) 或错峰跑全量。新功能字段影响旧契约: 预期不变 (FR-19/22 为纯新增
端点, 无既有字段变更), 6 轮中 fr13/fr14 类契约断言无一因新功能字段失败。

## 5. 落地后回填指引 (dev-backend 合入后)

1. 项目根 `cargo build --release` (或 `SERIALHUB_EXE` 旁路构建后指给夹具);
2. `python -m pytest tests/test_fr19_record_replay.py tests/test_fr22_import_export.py -v`
   —— 探针自动放行, 逐条回填 §2 状态列; 失败条目附 bridge 日志 (conftest `_LOG_DIR`) 交架构师裁定;
3. `python -m pytest tests/ -q` 全量回归 (建议独占机窗口), 回填 §4 并核对 §3 假设表;
4. 卫生自证: 录像临时文件由套件自清 (`delete_recordings`), 全程未触碰 COM8,
   会话结束无 serialhub.exe 残留 (外来快照户除外)。

## 6. 解锁跑结果 (2026-09-15, QA 401 接手; B1/B4 后端已落地, 探针放行)

### 6.1 隔离与构建证据 (并行纪律)

- **构建快照**: 2026-09-15 00:41 `CARGO_TARGET_DIR=target-qa cargo build --release`
  (1m07s, 复用前任缓存) → exe 副本 `build/qa401/serialhub.exe` (4,748,800 B)。
  该快照 = B1/B4 完整态 —— 波2 的 `src/forward.rs`(00:41:55)/`src/logging.rs`(00:43:40)
  在构建完成后才出现, **未入镜**, 被测对象恰为任务书标的 B1/B4。
- **隔离手段**: 全部 pytest 以 `SERIALHUB_EXE=build/qa401/serialhub.exe` 指向自己的副本;
  recordings/ 随 exe 副本落 `build/qa401/recordings/`, 与 target/release 席位互不干扰;
  进程清理沿 conftest ADR-22 外来 PID 快照差集, 会话末零残留 (tasklist 复核 ✓);
  COM8 全程未触碰; 测试端口 18200+; 未 commit。
- **门控**: `fr19_ready` / `fr22_ready` 探针均放行, 全程 0 skip。

### 6.2 用例矩阵 (9/9 PASS)

| # | 用例 | 结果 | 实测注记 |
|---|---|---|---|
| R1 | record_jsonl_shape_and_accounting | ✅ | ts=相对毫秒 ✓ 起点边界 ✓ 双向字节逐字节对账 ✓ 三键齐备无契约外键 |
| R2 | record_stop_halts_file_growth | ✅ | stop 后文件大小冻结 ✓ |
| R3 | replay_rejected_when_serial_not_open | ✅ | COM99 retry 桥 replay → 400/409 ✓ |
| R4 | replay_delivers_in_order_paced_until_natural_end | ✅* | *经 6.3-③ 测量伪影修复后过; pacing 实证 0.109/0.422/0.625s (≈300ms 间隔) |
| R5 | replay_rejects_path_traversal | ✅ | `../Cargo.toml`/反斜杠 → 400, 不存在 → 4xx ✓ |
| E1 | export_contains_existing_bridges | ✅ | export 200, 与 GET /api/fleet 对账 ✓ |
| E2 | import_merge_keeps_existing_and_adds_new | ✅ | 对象编码直接受理 (无 warning 偏差) |
| E3 | import_replace_swaps_whole_table | ✅ | 旧桥消失新桥恰在列 ✓ |
| E4 | import_rejects_bad_schema_and_keeps_table | ✅ | 3 非法 schema + mode 越域 → 400, 失败不动表 ✓ |

首轮 8/9 (R4 FAIL: spread=0.000s) → 判定为测试测量伪影而非后端违约 (见 6.3-③),
修复后复跑 9/9 (16.56s)。

### 6.3 断言随动修订 (以 ADR-24⑥ 为准; 均在 tests/test_fr19_record_replay.py 内注明)

1. **R1 ts 契约** (qa-plan §3 A1 收敛): 原按 A1 双语义宽容断言「ts 近当前时刻 ±2min」,
   ADR-24⑥ 已裁定 ts=录制起点起相对毫秒 → 改断言: ts 非负整数 (首帧可 0)、
   `0 ≤ ts[0] < 120s`、跨度 < 2min; 绝对 epoch 即违约。实测后端录制侧合规 ✓。
2. **R4 回放源构造**: 原按 A1 用绝对 epoch ms 构造 → 改 0 基相对 ms (0/300/600);
   并注明 ADR-24⑥「只回放 tx 行」(源文件本就仅含 tx 行, 断言不变)。
3. **R4 测量伪影修复 (非预期放宽)**: 原 peer read timeout=1.5s > 回放全程 0.6s,
   单次 `read()` 吞掉整个 pacing 窗 → `t_first==t_last`、spread 恒测 0 —— 任何合规
   实现都无法通过该断言 (首轮 FAIL 即此, 字节本身完整按序)。独立探针 (短超时逐块
   计时) 实证后端 pacing 正常后, 改 timeout=0.1s 按读块记录到达时刻。
   时序断言本身 (spread ≥ 0.25×预期 / 总耗时 ≤ 预期+8s) 未动。

### 6.4 契约字段实测集 (17→19 演进核对, 探针直采)

- **fleet 行 (运行时桥对象): 19 字段** = 原 15 (conftest FLEET_ROW_FIELDS) +
  `autoOpen` + `running` + **`recording` + `replay`** (后两者为 B1 新增回放/录制状态)。
  **未见 `forwardTcp`/`forwardConnected`** —— 波2 未落至本快照, 收口后需按 §6.1 同法
  重建快照复验字段集。
- export 条目 (fleet.json 持久化形状): 7 字段 = autoOpen/autoReconnect/id/listen/
  maxClients/name/serial。
- /api/status: 13 字段不变 (STATUS_FIELDS 全中)。
- replay 受理响应体 (实测, A3 留白可收敛): `{"ok":true,"file","speed":1.0,"loop":false}`。

### 6.5 全量回归

- **pytest 全套: 72/72 passed, 0 failed, 0 skipped, 184.28s** (旧 63 + 新 9;
  本轮窗口干净, 未复现前任 §4 的竞争性 flake)。
- **cargo test: 受阻未验** —— 并行席位波2 (B2/B3) 正在改写 src/ (探针实测 record.rs
  在本轮运行期间仍在变动), 测试档编译 10 errors (forward_tcp/logging 相关, E0063/
  E0433/E0507 等)。属并发中间态, 非 B1/B4 回归; 任务书「cargo test 108/108」为
  其构建时点状态。待 dev 波2 收口后由 dev 席复跑; 黑盒回归以 pytest 72/72 为准。
- 卫生: build/qa401/recordings/ 测毕无残留 (套件自清 ✓), 无 serialhub.exe 残留。
