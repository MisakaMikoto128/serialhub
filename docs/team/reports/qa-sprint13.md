# QA Sprint 13 收口报告 — 批次 B 全量 (FR-19/20/21/22) · 2026-09-15

作者: QA (401, 新实例接手) · 前序: qa-sprint13-plan.md (§1-§5 计划与门控, §6 波1 解锁;
本报告为其收口续篇, 编号接续) · 依据: spec FR-19/20/21/22 + ADR-24⑤⑥ · 黑盒纪律不变
(COM1↔COM2 ELTIMA 虚拟对, 禁碰 COM8, 端口 18xxx/8091, 进程按 PID 清理, 未 commit)。

## 7. 收口轮构建与隔离 (Sprint 13 完整态 B1-B4)

- **构建**: 2026-09-15 `CARGO_TARGET_DIR=target-qa cargo build --release` → 11.1s (缓存复用),
  exe 副本 `build/qa401/serialhub.exe` (4,889,088 B; 较波1 快照 +140,288 B, forward/logging 入镜,
  = 任务书标的 B1-B4 完整态)。全部 pytest / 审计 / 黑盒以 `SERIALHUB_EXE` 指向该副本, 不抢共享 target。
- **cargo test: 115 passed / 0 failed** (3.99s; 任务书 115/115 ✅, 波2 报告口径一致)。
- 运行期无并行席位 serialhub/cargo 进程 (tasklist 实测), 未复现 plan §4 竞争性 flake。

## 8. 契约随动修订 (任务书「FR-13/14 契约断言按实际字段集修订」落点)

- **探针实测 fleet 桥对象 = 21 键**: 原 15 (conftest FLEET_ROW_FIELDS) + `autoOpen`/`running`
  (Sprint 8 起既有回显, 此前未入契约集) + `recording`/`replay` (B1, FR-19) +
  `forwardTcp`/`forwardConnected` (B2, FR-20; 默认 `""`/`false`)。
  dev 报告「17→19」计数未含 autoOpen/running 两键, 实测以 21 键为准 (plan §6.4 的 19 键实测 + 本轮新增 2 键)。
- **tests/conftest.py**: `FLEET_ROW_FIELDS` 15→21 并注明演进 (15→17→19→21, ADR-24);
  `assert_row_shape` 增类型契约: autoOpen/running/forwardConnected 布尔, forwardTcp 字符串,
  recording/replay null 或状态对象。
- **tests/test_fr10_fleet.py**: 文档串与行注「15 字段」→「21 字段」并指到 conftest 注。
- `/api/status` 13 字段不变 (实测 13/13, STATUS_FIELDS 全中); export 持久化条目 7 字段不变。
- **修订后全量验证**: pytest 全套 **72/72 passed, 0 failed, 0 skipped** (184.53s) ——
  旧 63 + FR-19 五条 + FR-22 四条 (解锁后全绿, plan §6.2 矩阵维持 9/9 ✅)。

## 9. 独立黑盒复验 (35/35 PASS, 与套件互不依赖)

独立脚本 `build/qa401/bb_sprint13.py` (预期只来自 spec+ADR-24, 自带进程/PID 清理;
管理台 18401, 桥数据面 18403, 转发收站 18410; `--recordings-dir`/`--log-file` 指向临时目录):

| # | 能力 | 断言组 | 结果 | 实测证据 |
|---|---|---|---|---|
| ① | FR-19 录制 | start 受理→起点边界→JSONL 行契约→双向字节对账→stop 语义→列表/状态回显 | 13/13 | 8 行录像; ts[0]=0 (相对 ms, ADR-24⑥), ts 单调; RX/TX 拼接逐字节=灌入字节; start 前 PRE-NOISE 不入镜; stop 后 row.recording=null |
| ② | FR-19 回放 | 手工 TX-only 录像 (ts 0/300/600) → replay(speed=1, loop=false) → COM2 按序收全 | 5/5 | 字节逐字节等; spread=0.546s (≥0.25×0.6s, 实测到达 0.09/0.42/0.64s ≈ 300ms 节拍); 自然结束无多余字节; replay/stop 幂等 |
| ③ | FR-20 转发 | 配置→单向转发→只出不进→防环路→断线重连→改空=关 | 8/8 | row.forwardTcp 回显 + forwardConnected=true; RX→TCP 按序完整; WS 上行与 TCP 入站双向零回注; 断开后探测→重连 (新 accept + forwardConnected=true) →续传; 清空后 false |
| ④ | FR-22 导入导出 | export 对账→merge→replace→非法 schema | 6/6 | export 与活表 name/listen 对账; merge imported=1 skipped=1 旧桥原样; replace 整表替换; `{"unrelated":true}`/mode 越域 → 400 ×2 且不动表; 收口 export 反映替换后表 |
| ⑤ | FR-21 日志 (附检) | --log-file 落盘事件 | 1/1 | 2,429B: 启动/state/录制/回放/转发 事件全中 |
| — | 卫生 | 无残留 | 2/2 | tasklist 零 serialhub.exe; 临时目录自清 |

黑盒过程记录 (前两轮 26/33 → 修正脚本测量伪影后 35/35, 后端无违约):
WS 上行的 COM2 正常回环污染防环路检查 (脚本未排空)、TCP 半开需写流量才触发断线检测
(spec FR-20「写失败/连接失败探测」的既定语义, 探针补一枚探测字节后按 forwardConnected 轮询)、
导入条目 `id` 为必填 (脚本误删, 套件 E2/E3 同口径保留 id)。

## 10. 像素审计复跑 (转发新控件入径) + 录制页签补测

- **主审计** (`tools/ui_pixel_audit.js`, SERIALHUB_EXE 指 qa401 副本, 8090):
  **PASS — 168 可见控件 × 7 轮**, 高度 ∈ {28,34}±0.5px、圆角 = 主题 --ctl-radius ±0.5px, 违例 0。
  转发设置新控件 `#cfForward` 在 R3 抽屉·设置 (37 控件, 较上轮 +2) 入径; 导出/导入钮在 R6 设置弹窗 (22 控件) 入径。
- **录制页签补测** (`build/qa401/rec_tab_pixel_probe.js`, 一次性探针): 主审计七轮未含新「录制」
  页签, 补测空闲态 + 录制中态 (列表含条目钮) 各 7 可见控件, 高度/圆角全部合规, 违例 0。
- 工具小改: `ui_pixel_audit.js` 的 EXE 支持 `SERIALHUB_EXE` 环境变量 (与 conftest 同名约定, 并行隔离)。

## 11. 文档同步 (任务 B, 同轮完成)

- **手册** `docs/manual/用户使用手册.md`: 适用版本 → v2.0.0; 桥抽屉 三→四页签 (+录制行);
  新四节: 录制与回放 (JSONL 格式 / 只回放 tx / 固件复现场景) · 旁路转发 (只出不进 / 3s 重连 / 防环路) ·
  配置导入导出 (换机器迁移, 合并 vs 替换); CLI 表增 `--recordings-dir`/`--log-file` + sc create 服务化包装;
  API 摘要补 6 行新端点 + 桥对象 21 字段; 安全章节「v1.x」措辞改版本中立。
- **README**: 特性表补 4 行 (录制回放/旁路转发/日志文件/导入导出); 开发计数 115/72 对齐; About 补一句。
- **CHANGELOG**: 新增 v2.0.0 (2026-09-15) 条目 (四能力 + 契约 15→21)。
- **backlog**: Sprint 13 QA 行状态列更新 (qa-plan ✔ / 集成 QA ◐ 注明 UX 走查未做)。

## 12. 观察与移交 (不阻塞收口)

1. **转发首连积压语义**: 新配 forwardTcp 首连会把桥启动以来的 RX (广播缓冲 1024 帧内) 一并送达
   —— 与 dev 报告「重连间隙不丢帧」设计一致, spec 未约束连接时点, 不判违约; 已留痕 (黑盒 §9①③)。
2. **转发断线探测**: TCP 半开且无写流量时不主动感知, 有数据才触发「写失败→3s 重连」—— 符合
   spec FR-20 字面, 无人值守低流量场景感知延迟上限 ≈ 3s+数据间隔, 供架构师知悉。
3. **import 缺 bridges 键**: `{"version":1}` (无 bridges) 实测受理为空集导入 (imported=0);
   套件 E4 的「缺 bridges」案形是 `{"unrelated":true}` → 400, 两者不矛盾, 是否收紧 schema 待裁定。
4. `tools/ui_pixel_audit.js` 的 killSelfSweep 仍为 `taskkill /IM` 全杀口径 (先于 ADR-22), 建议后续改按 PID;
   本轮未动。
5. **发版待办**: Cargo.toml version 仍 1.8.1, 随 v2.0.0 里程碑由 dev/架构师推进; UX 走查 (波3) 未做。

## 13. 卫生自证

全程未触碰 COM8; 会话末 tasklist 复核零 serialhub.exe 残留; 录像/清单/日志临时文件测毕即删;
共享 target 未被占用 (构建走 target-qa); 未 commit, 未碰 src/ ui/。
