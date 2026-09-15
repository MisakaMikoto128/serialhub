# QA XTest 数据链路交叉场景报告 (席位 f1) · 2026-09-15

作者: QA f1 (数据链路方向, 新实例) · 前序: qa-sprint13.md (§9 单能力黑盒, 本报告补其**交叉场景**盲区)
被测: v2.0.0 工作区 · 独立副本 `build/f1/serialhub.exe` (4,889,088 B, 自 build/qa401 快照复制,
git HEAD 1a3be08 无 src 改动 = 同一构建) · `SERIALHUB_EXE` 指副本, 不抢共享 target。
套件: `tests/test_xtest_data.py` (新增 5 函数, 复用 tests/conftest.py 夹具; 未改 conftest/src/ui)。

## 1. 矩阵 × 结果 × 证据

**结果: 5/5 PASS** (`python -m pytest tests/test_xtest_data.py -v -s` → 5 passed in 31.64s;
先行一轮 `-v` 亦 5 passed in 31.83s)。COM1(桥)↔COM2(对端/第二桥), 端口 18300+, 录像/日志走
每测试独立 `--recordings-dir`/`--log-file` 临时目录。

| # | 交叉场景 | 断言要点 | 结果 | 实测证据 |
|---|---|---|---|---|
| X1 | **录制 + 转发同时** | 转发口收全 RX AND JSONL 双向逐字节对账 (互不丢帧); TX 不入转发口 | PASS | JSONL 9 行 (rx 6 帧/tx 3 帧), RX 46B + TX 42B 拼接与灌入逐字节全等; 转发流恰 46B 全等 (TX 0 入镜, 无积压/重复); record/stop 汇总 bytes=88 与两向合计一致 |
| X2 | **回放 × 转发** (方向语义) | 回放写 **TX**→对端收全; 转发搬 **RX**→转发口收真实 RX 且 0 回放字节 | PASS | 对端收回放 51B 逐字节全等 (3 帧 pacing); 转发流恰 28B = 真实 RX 全等, 回放字节 0 入镜; loop=false 自然结束无多余字节。方向裁定: 虚拟对 TX↔RX 对端落地, 回放不构成"串口 RX"故不转发 —— 断言按此语义 |
| X3 | **日志全事件** (FR-21) | 同一 --log-file 跑全链: 启动/相位迁移/录制开始结束/回放开始停止/转发断线重连, 行格式+时序 | PASS | 日志 16 行, 全部 `[iso-utc] [level] msg` 格式; 启动(日志文件启用+数据面启动)/相位→open(state)/录制开始+结束/回放开始+停止 全中; 转发 已连接×2、断开/写失败×2, 时序 conn[0]<drop[0]<conn[-1] 成立, 重连后续传字节全等 |
| X4 | **多桥干扰** | 两桥 (COM1/COM2 各占一个数据端口) 同时录制, WS 交替灌不同图案, 方向×桥归属对账 | PASS | A: tx 70B=PA / rx 70B=PB; B: tx=PB / rx=PA 四向逐字节全等; 反串扰断言 (A.rx 无 PA 等 4 项) 全过; record/stop 两桥汇总各 bytes=140 与 tx+rx 合计一致 |
| X5 | **资源: 5 轮录制开始/停止** | 每轮文件独立且逐字节完整; stop 后尺寸稳定; 串口/录制器/HTTP 全程可用; 工作集对比 | PASS | 5 轮文件名全独立、每轮 rx 恰=该轮图案 (0 混轮); stop 后 0.5s 尺寸零增长×5; 第 6 轮 start/stop 仍正常; WS 上行→对端照收 (串口句柄未劣化); 工作集 r1=12,520KB → r5=12,524KB (**+4KB**, 阈值 20,000KB) |

## 2. 过程记录 (如实)

1. **首次全红 = 环境争用, 非产品缺陷**: 首轮 5 失败全部 `打开 COM1 失败: 拒绝访问` ——
   ui-xtest 席位实例 (PID 8828, `--fleet output/ui-xtest/...`) 当时持有 COM1+COM2。
   按"外来快照户不碰"纪律未杀, 改轮询等待。
2. **席位间互相干扰留痕**: 等待期间本席一套件跑至 4/5 时 X4 的 WS 连接被外部 abrupt 掐断
   (`ConnectionClosedError: no close frame`, 疑并行席位 `taskkill /IM` 口径清扫波及,
   即 qa-sprint13 §12-4 已留痕的 killSelfSweep 隐患); 随后 solo 复跑又遇 COM1 被占。
   另发现席位 f2 (`target-f2`) 亦在轮询等 rig —— 多席同抢一对虚拟口, 建议排班或扩对。
3. **解决**: 端口探测门控重试 (COM1+COM2 实际可开连续 2 次×20s 才花尝试), 第 1 次尝试即全绿
   (09:59:34), 10:03 复跑带 `-s` 采证再全绿 —— 结果稳定可复现, 排除 flake。
4. 测试自身两处修正 (非产品): ① `/api/fleet` 信封解析改用 conftest `row_of` (裸列表/`{bridges:[...]}` 双兼容);
   ② probe 脚本排除 PowerShell 自匹配伪影。

## 3. 观察与移交 (不阻塞)

1. **X1 首连积压**: forwardTcp 首连会送达桥启动以来 RX 积压 (qa-sprint13 §12-1 已留痕);
   本套件以 "connected 后 clear 收站缓冲再对账" 规避, 语义仍建议在 spec 补一句。
2. **回放自然结束无日志行** (仅显式 replay/stop 有"回放停止"): X3 为取事件行改用 loop=true+显式停;
   若需"回放完成"审计留痕, 移交 dev 裁定 (FR-21 未钉)。
3. **X5 工作集口径**: tasklist CSV 末列 (KB), 5 轮 +4KB 属噪声级; 阈值 20MB 只防毛漏,
   长时 soak 另立专项。

## 4. 卫生自证

全程未触碰 COM8; 会话末 `tasklist` 零 serialhub.exe 残留; COM1/COM2 复测可开 (probe RIGFREE);
录像/日志/清单走临时目录测毕即删 (build/f1 下无 recordings 残留); 共享 target 未占用;
未 commit, 未碰 src/ ui/。证据文件: `build/f1/xtest_final.log` (全绿+证据行),
`build/f1/xtest_attempt_1.log` (先行全绿轮), `build/f1/retry_run2.log` (争用过程)。
