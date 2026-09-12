# QA 报告 — Sprint 1 (核心桥 MVP 一致性测试)

日期: 2026-09-12 · 角色: QA · 被测: `target/release/serialhub.exe` (Cargo.toml 0.1.0, 本工作树构建)
结论: **24 / 24 通过 (含 PERF 全部达标)** · 套件落盘 `tests/` · 连跑 2 遍均绿 · 无残留进程 · COM8 全程未占用

---

## 1. 环境说明

| 项 | 值 |
|---|---|
| OS | Windows 11 (10.0.26200 x64) |
| 工具链 | rustc 1.97.1 (8bab26f4f 2026-07-14) / cargo 1.97.1 · `cargo build --release` 21.5s |
| 测试栈 | Python 3.11.7 · pytest 9.0.3 · pyserial 3.5 · websockets 16.0 |
| 串口 | ELTIMA 虚拟对 **COM1(桥侧)↔COM2(pyserial 对端)**;COM8 (CH340 真板) 仅出现在桥的 `/api/ports` 枚举结果里,测试代码从未打开过它 |
| 桥进程管理 | 每条测试经 `start_bridge` 夹具拉起自己的桥进程 (随机空闲 TCP 端口, 不写死 8080): poll `/api/status` 等 HTTP 就绪 → `wait_phase` 等状态机到位 → 测试体 → teardown `terminate`→超时 `taskkill /F /T` → 会话级兜底再全量清扫一次。两轮全套件跑完后 `tasklist` 确认 **0 个 serialhub.exe 残留** |
| 对端 | pyserial 开 COM2, 参数随用例匹配 (7E1→7E1 等), 夹具统一关闭 |
| 复现 | 项目根执行 `python -m pytest tests/ -v` (全套 ≈40s; `-k perf` 单跑性能) |

套件文件 (tests/ 目录此前不存在, 无旧测试可删, 本 Sprint 全部新增, QA 所有):
`conftest.py`(夹具) · `test_fr1_pipeline.py`(3) · `test_fr2_config.py`(12) · `test_fr3_reopen.py`(1) · `test_fr4_status.py`(5) · `test_perf.py`(3, @perf 标记)

## 2. PASS/FAIL 矩阵

| 条目 | 结果 | 证据 (黑盒实测) |
|---|---|---|
| FR-1 `test_fr1_integrity` | **PASS** | 256B 随机图案×100 帧: 上行 WS→COM2 逐字节一致 (25600B/25600B, 首差异=无); 下行 COM2→WS 逐字节一致 (25600B) |
| FR-1 `test_fr1_broadcast` | **PASS** | 3 WS 客户端同时在线, COM2 发 40×64B, 三端各收满 2560B 且与源逐字节一致 |
| FR-1 `test_fr1_tx_arbitration` | **PASS** | 2 客户端并发各发 50 帧 (16B 定长帧带序号+魔数), 对端收满 1600B: 100/100 帧魔数完整 (无交叠/损坏), 每客户端 seq 严格 0..49 (FIFO 保序), 50+50 帧无丢失 |
| FR-2 `test_fr2_config_matrix` ×5 | **PASS** | 8N1/8N2/7E1@115200、8N1@921600、8N1@2000000 (CLI 入口): status 回显 `baud/config` 与 CLI 一致; 各配置下对端按同参数打开, 上行+回灌下行双向往返逐字节一致 (7E1 用 ≤0x7F 图案) |
| FR-2 `test_fr2_config_via_web_api` | **PASS** | Web 入口: `--no-open` 启动 → phase=="closed" → POST /api/config + /api/open → status `baud=115200, config=8N2` → 数据面 256B 往返一致 |
| FR-2 `test_fr2_config_api_rejects_out_of_range` ×5 | **PASS** | baud=3 / baud=2000001 / dataBits=5 / parity=X / stopBits=3 全部 HTTP 400 + `{"ok":false,"error":"波特率 3 超出允许范围 [110, 2000000]"}` 等, 形状符合 ADR-5 ③ |
| FR-2 `test_fr2_cli_rejects_invalid` | **PASS** | `--config 8X9`、`--baud 3`: 进程 5s 内退出且退出码非 0 (拒绝带坏参数运行) |
| FR-3 `test_fr3_reopen_state_machine` | **PASS** | a) `--port COM99` 启动 → phase=="retry" 且 lastError 非空; b) POST /api/close → phase=="closed", 静置 2.5s (>1s 重试间隔) 仍 closed (重试确被打断); c) config 换 COM1 + open → phase=="open", 上行 10×128B 逐字节恢复 |
| FR-4 `test_fr4_status_contract` | **PASS** | status 恰 9 字段 (多/少字段断言均空, 无 flow, ADR-5 ①); 初值 `phase=open, port=COM1, baud=115200, config=8N2, clients=0, rxBytes=0, txBytes=0, lastError=null`; uptimeSec 2.2s 内 +2; **close 之后仍继续增长** (证"进程运行时长"而非打开时长, ADR-5 ②) |
| FR-4 `test_fr4_status_clients_counter` | **PASS** | clients 0→1→2→(断开 1)→1→0, 全部 5s 内到位 |
| FR-4 `test_fr4_status_counters_grow` | **PASS** | 上行 1000B → 恰一个计数器 +1000; 下行 1000B → 恰另一个 +1000; 两向计入不同计数器, 记账精确无多计 |
| FR-4 `test_fr4_post_contract_shapes` | **PASS** | config/open/close 成功响应**精确等于** `{"ok":true}` (ADR-5 ③); open-while-open 返回 `{"ok":true}`、相位不变、紧接 128B 数据面仍通 (ADR-5 ④ no-op); 非法 config → 400+`{"ok":false,"error"}` |
| FR-4 `test_fr4_ports_lists_pair` | **PASS** | /api/ports 含 COM1/COM2 (也如实列出 COM8, 符合"列本机串口") |
| PERF-1 `test_perf_1_throughput_921600` | **PASS** | 921600 8N1, 每向 1MB: **上行 7327 kbps / 下行 10353 kbps** (阈值 900, 余量 8~11 倍) |
| PERF-2 `test_perf_2_16_clients_broadcast` | **PASS** | 16 WS 客户端同时在线, COM2 发 100×256B, 16/16 客户端收满 25600B 且逐字节一致 (零错) |
| PERF-3 `test_perf_3_rx_to_ws_latency_p95` | **PASS** | 300 样本: **p95 = 0.61 ms** (min 0.40 / p50 0.51 / max 1.51), 阈值 5 ms, 余量 8 倍 |

计数器方向语义 (黑盒探针实测, 供契约归档): `txBytes` = 桥**写入串口**字节数 (上行 +1000), `rxBytes` = 桥**读自串口**字节数 (下行 +1000), 以串口为参照系。

## 3. PERF 基线 (Sprint 2 优化对照用)

| 指标 | 实测 (115200 默认 / 921600 8N1 如标注) | 备注 |
|---|---|---|
| PERF-1 上行吞吐 (WS→串口) | 7327 kbps | 1MB, WS 64KB 消息, COM1→COM2 方向写侧自然背压, 数字即桥真实管道能力 |
| PERF-1 下行吞吐 (串口→WS) | 10353 kbps | 1MB, 台架 2KB@1ms 节流写入 (≈1.3MB/s 上限, 高于达标线 12 倍); 第 1 轮 10326、第 2 轮 10353, 稳定 |
| PERF-2 广播扇出 | 16 客户端 × 25600B 全量零错 | 100 帧逐帧广播, 5ms 间隔 |
| PERF-3 RX→WS 延迟 | p95 0.61 ms @115200 8N2 | 单帧在途法 300 样本, 含 pyserial 写入开销 (偏保守) |

## 4. 过程记录: 首轮 3 例失败的根因 (供 Dev 与后续测试者理解台架约束)

首轮 (台架未节流) 3 例失败: `test_fr1_integrity`(下行)、`test_perf_1`(下行)、`test_perf_3`。**预期未改**, 排查结论是 ELTIMA 虚拟驱动的机械特性, 修的是测试台架的写串口方式:

1. 现象: COM2 一次性写 25600B, 桥只转发 4097B (1B+4×1024B) 后流完全停住; `rxBytes` 冻结在已读量, phase 保持 open, lastError 为空, 不 panic。
2. **无桥对照实验** (pyserial 直读 COM1, 桥不参与): 同样 25600B 突发 → 8s 紧轮询 (129 次读) 只读到 4096B; 补 1B 写只解锁 1B。即: **ELTIMA 按"对端写事件"搬运数据、单次最多填满 RX 队列 (4KB), 积压部分滞留发送侧, 读方无论多勤快都拉不动**。任何实现 (包括完美桥) 在此环境下的"突发 >4KB"都同样表现。
3. 桥自身行为无瑕疵: 驱动交给它的 4097B 100% 转发到 WS (收发相等), 无丢帧、无 panic、无错误上报 —— FR-1 在驱动交付的每个字节上都成立。
4. 台架修正: 下行大流量改为 ≤2KB 小帧节流写入 (等效真实 UART 线速行为), perf_3 改为按 16B 定长帧缓冲重组 (桥按串口读块切 WS 消息, 一帧可拆多条消息, 属字节流语义, 测试原假设"一写=一消息"不成立)。修正后 3 例全绿, 且修正只涉及测试代码 `tests/`, 未动 `src/`、未动任何预期。

给 Dev/文档的一句话提醒: ELTIMA 虚拟对上做 >4KB 单写压测会得到"假性丢字节数据", 真机 CH340 不会; 若 Sprint 2 要做极限压测, 建议台架统一走节流注入。

## 5. 观察与建议 (非阻塞, 黑盒观察, 不构成 FAIL)

1. **RX→WS 消息粒度 = 串口读块**: 突发首条消息实测仅 1 字节, 随后 1KB 左右 (桥按读到的块直接 broadcast)。符合 FR-1"原始字节"语义, 但页面侧协议栈必须容忍任意分块 —— 建议 UI/文档注明"WS 消息边界≠串口帧边界"。
2. **POST /api/config 空 body `{}` → 200 `{"ok":true}`**: 空"部分更新"被接受为 no-op。spec 未禁止, 无害, 建议在 API 文档写明。
3. **lastError 无错时为 `null`** (非空串): ADR-5 未规定, 建议随契约归档。
4. **`/api/ports` 列出全部本机串口** (含 COM8): 符合"列本机串口"; UI 若默认选中第一项, 需注意别误导用户选到非测试设备。
5. **慢客户端 Lagged 丢旧帧策略**未被本套件触发到失败 (虚拟驱动 4KB/写事件上限天然限制了在途消息数)。真机高波特率 + 慢客户端场景下该策略会真实丢数据, 属 Dev 已知取舍, 建议在文档注明, Sprint 2 可考虑计数上报 (如 status 增加 dropped 计数, 属契约变更需裁定)。

## 6. SPEC-QUESTION (请架构师裁定)

1. `test_fr2_config_matrix` 断言 `status.config` 恰等于 CLI 令牌 (`"8N2"`/`"7E1"`) —— 当前实现一致, 建议把 config 字符串格式 (`{数据位}{校验}{停止位}`) 写进 ADR-5 防后续漂移。
2. 计数器参照系建议按本次实测语义归档: rxBytes=串口收、txBytes=串口发 (见 §2 末)。

## 7. 结论摘要

1. Sprint 1 核心桥 MVP 一致性套件建成并全绿: **24/24 PASS** (FR-1×3、FR-2×12、FR-3×1、FR-4×5、PERF×3), 连跑 2 遍结果一致, 全程未触碰 COM8, 测后零残留进程。
2. FR-1 数据管道: 25600B 双向逐字节一致; 3 客户端广播零差; 2×50 并发帧 FIFO 保序、无交叠无丢失。
3. FR-2: 5 组参数回环真实生效, CLI/Web 双入口均可配, 越界参数 API 400/CLI 拒启, 形状符合 ADR-5 ③。
4. FR-3: retry 相位与 lastError 可见、close 可打断重试、换回 COM1 后数据面完整恢复, 状态机契约成立。
5. FR-4: status 恰 9 字段, clients/计数器增减精确, uptimeSec 为进程时长, open-while-open no-op 验证通过。
6. PERF 基线: 921600 双向 7327/10353 kbps (阈值 900)、16 客户端广播零错、延迟 p95 0.61ms (阈值 5ms) —— 三项全部达标且余量 8 倍以上。
7. 首轮 3 例失败经无桥对照实验定界为 ELTIMA 驱动"写事件搬运/4KB 队列"特性, 桥自身无责; 已修测试台架 (未动预期/未动 src)。
8. 遗留观察 5 条 + SPEC-QUESTION 2 条见 §5/§6, 均不阻塞验收。
