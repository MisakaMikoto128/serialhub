# Dev Sprint 13 报告 — B1 录制/回放后端 + B4 导入导出 (dev-backend 201)

- 日期: 2026-09-15 · 变更文件: **仅 `src/`** (新增 `src/record.rs`; `fleet.rs`/`serial.rs`/
  `service.rs`/`supervisor.rs`/`cli.rs`/`main.rs` 配套) · Cargo.toml **零新增依赖** ·
  未 commit, 未碰 ui/ tests/ docs/(本报告除外)
- 依据: ADR-24①④ + **ADR-24⑥ 裁定** (实现契约) · spec FR-19/FR-22 ·
  dev-sprint13-ui.md §5 (前端 mock 契约) · qa-sprint13-plan.md §3
- 接手注: 工作区内已有前序后端实例未提交的半成品 (record.rs + fleet.rs 801 行)。
  本实例核实基线 (cargo test 106/0) 后**补齐三处契约缺口**、修订测试并完成真机自测。

## 1. 交付物 (FR-19)

1. **录制状态机 (每桥)**: `Bridge.rec` 槽位 (None=空闲)。tap 同源 tee —— RX 订阅数据面
   同一条 `bc_tx` 广播 (帧语义零变化, ADR-6⑤ 分块照录), TX 在 `PortCtx::send_to_port`
   成功入队时 tee 到新 `tx_bc` (凡进串口 TX 队列的帧必经此, 含 WS 上行与回放注入)。
   落盘 `recordings/<桥id>-<yyyymmdd-hhmmss>.jsonl`, 每行 `{"ts":相对毫秒,"dir":"rx|tx",
   "hex":小写}`, UTF-8; 带缓冲 + 500ms 定期 flush (崩溃最多丢 500ms)。重复 start 400;
   桥停止/删除/进程退出自动收尾且**录像文件保留** (FR-19 硬性, 有单测+黑盒)。
2. **API 五端点 + 删录像 (ADR-24⑥)**:
   - `POST /api/fleet/<id>/record/start` → `{"ok":true,"file":"<id>-<戳>.jsonl"}`
   - `POST /api/fleet/<id>/record/stop` → `{"ok":true,"file":...,"frames":n,"bytes":n}`
   - `GET  /api/fleet/<id>/recordings` → `{"ok":true,"recordings":[{file,frames,bytes,
     startedAt,durationSec}]}` (本桥前缀匹配, 新的在前; 逐行扫描现算, 无 sidecar)
   - `POST /api/fleet/<id>/recordings/delete {"file"}` → `{"ok":true}` (穿越拒绝同 replay;
     文件不存在 400; **UI §5 Q1 已闭合**)
   - `POST /api/fleet/<id>/replay {"file","speed":0.5~10,"loop":false}`: **只回放 tx 行**
     (ADR-24⑥: 网页→设备的原始命令重放给设备; rx 行不回注), 行间 ts 差/speed 时序写回
     串口 TX (走现有 tx 队列)。串口未 open 400; **首帧立即发** (不按首行 ts 绝对时刻等待
     —— 兼容手工构造的绝对 epoch ms 录像, QA §3 A1); loop=true 循环到
     `POST .../replay/stop` (幂等, **Q2 闭合**); 回放中再回放 400。
   - 状态回显 (Q3 兜底线): fleet 桥对象/单桥 detail 增 `recording`/`replay` 两字段
     (null 或 {file,startedAt} / {file,speed,loop,frames,bytes}), 契约 15→17 字段;
     UI「有则用」已防御, 旧契约测试为子集校验不受影响 (conftest FLEET_ROW_FIELDS 实查)。
   - `--recordings-dir <路径>` CLI (默认 exe 旁 recordings/); 录制中文件增长即时落盘,
     UI 轮询 recordings 列表即得实时帧数/字节 (Q3 正向满足, 超出裁定要求)。

## 2. 交付物 (FR-22)

- `GET+POST /api/fleet/export` → fleet.json **原样**下载 (attachment; 双受理为 ADR-24⑥,
  **Q4 闭合** —— UI 先 POST 后 GET 两路都通; 盘上无清单时按当前状态即时序列化, 形状与
  persist 写出一致)。
- `POST /api/fleet/import {"mode":"merge"|"replace","json":{...}}` (**Q5 闭合**):
  schema 全量校验 (version==1 + bridges 形状 + 每桥 serial/listen 深校验), 任一处非法
  整体 400 不动现有表; json 受理对象编码 (QA A4 主路径); merge 按 id 冲突跳过计数,
  replace 整表替换且运行中桥先停; 单桥建失败 (端口冲突等) 计入 skipped 不中断;
  响应 `{"ok":true,"imported":n,"skipped":n}`; 导入桥即建即启 (数据面 running, autoOpen
  按需开串口), 结束统一持久化 fleet.json。

## 3. 测试与自测

- **cargo test 108/0** (主分支基线 96 + 本 sprint 12 条): JSONL 三键格式/小写 hex/ts 单调、
  录制计数器与状态机 (重复 start 400/空闲 stop 400)、tx-only 过滤 (混合录像 rx 行绝不
  回注)、回放字节与时序双向钉住 (speed=1 行间差 ≥600ms; speed=10 压缩 <500ms)、假串口
  (set_tx 队列) 注入、loop 循环+replay/stop、绝对 epoch ms 首帧即发、replay 校验与穿越
  (5 变体含反斜杠/URL 编码)、recordings 列表、删录像正途+穿越+饵文件不波及、桥删除录像
  保留、export 形状/GET==POST/回导全跳过、merge/replace/schema 400×6、失败导入不动表。
  `cargo fmt --check` + `clippy --all-targets` 零警告 (ADR-22④ 门禁口径)。
- **黑盒真机自测 58/58 PASS** (`%TEMP%\sh13_backend_selftest.py`, release exe):
  管理台 127.0.0.1:18200 + 桥数据面 18201, COM1(桥,autoOpen)↔COM2(pyserial+WS 客户端)
  ELTIMA 虚拟对; COM8/COM4 全程未碰。实录→对端/网页双向对账 (RX 按读块拆并为 ADR-6⑤
  既定语义, 字节级对账硬契约全中)、录制文件回放给 COM2 (仅 tx 行)、手工绝对 ts 录像时序
  铺开 (0.15s≤dt≤6s)、导入导出全链路 (merge/replace/持久化/schema 400)、删录像与穿越、
  桥删除录像保留、fleet 行 recording/replay 投影。测毕按 PID 清理: 零 serialhub.exe 残留、
  零 182xx 监听、临时录像目录已删、exe 旁未产生 recordings/。

## 4. 契约符合性对照 (前端/QA 期待)

| UI/QA 期待 | 后端落地 | 状态 |
|---|---|---|
| ui §5 Q1 `POST .../recordings/delete {file}` | 已实现, `{"ok":true}`, 穿越拒绝 | ✅ |
| ui §5 Q2 回放停止 + 状态回显 | `replay/stop` + 桥对象 `replay` 字段 | ✅ |
| ui §5 Q3 录制中实时帧数 | 文件即写即 flush, recordings 列表回读即实时 | ✅ (超裁定) |
| ui §5 Q4 export POST/GET 两口径 | `get+.post` 双受理, 响应体逐字节一致 | ✅ |
| ui §5 Q5 import 响应字段 | `imported/skipped` (ui 已兼容别名, 无需改) | ✅ |
| qa A1 ts 语义 | 相对毫秒落盘; 回放按行间差, 绝对 epoch ms 首帧即发兼容 | ✅ |
| qa A2 回放方向 | **仅 tx 行** (ADR-24⑥); 手工 TX-only 录像直接受理, 无需先经 record | ✅ |
| qa A4/A5 json 对象编码 / 裸数组 vs 信封 | 受理对象编码; 导出为 `{version,bridges,...}` 信封, import 同形 (export 回显天然同形) | ✅ |

## 5. 留白与移交

- 16 进制/文件名/目录防护与 themes::resolve 同口径 (白名单字符 + canonicalize 复核)。
- QA 探针 `fr19_ready`/`fr22_ready` 现应放行 (record/start 于未运行桥返回 400 ≠ 404,
  探针判 404 逻辑不受影响); 回填指引见 qa-sprint13-plan.md §5。
- 回放写串口时若串口中途关闭, 帧暂弃不中断回放 (自动重连恢复后继续) —— 实现取舍,
  未见于契约, 供 QA/架构师知悉。
- 前端 UI 改动需随下次构建进壳 (include_str!, dev-sprint13-ui §5 出包提醒)。

---

# 波 2 — B2 TCP 旁路转发 (FR-20) + B3 日志文件 (FR-21)

- 日期: 2026-09-15 · 变更文件: **仅 `src/`** (新增 `src/forward.rs`/`src/logging.rs`;
  `fleet.rs`/`hub.rs`/`cli.rs`/`main.rs` 配套) · 零新增依赖 · 未 commit · 未碰 ui/tests/docs
- 依据: ADR-24②③ · spec FR-20/FR-21 · 波 1 同口径 (tap 同源 / 契约回显 / 校验拒绝)

## 6. B2 交付物 (FR-20 / ADR-24②)

1. **每桥 `forwardTcp:"host:port"`** (可空 = 关闭): create/config body + fleet.json
   持久化 (空串不落盘, 旧清单 `default` 空 = 兼容) + 桥对象回显。
   校验 (create/config/import 三路): 空=关; 非空须 `host:port`、host 非空、端口
   1~65535、无空白; **不做 DNS 预解析** (域名连接时才解析, 失败走 3s 重连节拍留痕)。
2. **TCP 客户端单向转发** (`forward.rs`): 会话随桥数据面启停 (create/start 起,
   stop/delete 断); 订阅与 WS/tap **同一条 `bc_tx`** (串口 RX) → TCP 写;
   **WS 上行 (tx_bc/send_to_port) 不在订阅源里, 天然防环路**; TCP 对端来的数据
   一律不读不回注。断线 (写失败/连接失败/3s 连接超时) → **3s 节拍自动重连**。
   会话级订阅: 重连间隙的帧在广播缓冲 (1024) 内不丢, 溢出 Lagged 跳过;
   配置改空 → 主动排空积压后待命 (关闭后旧帧不回灌)。
3. **运行中改配置热生效**: config `forwardTcp` → 断开旧连接, 立即按新值重连
   (改空 = 断开待命); 桥停止中改配只落配置, start 按新值起会话。
4. **状态回显**: 桥对象增 `forwardConnected` (bool, TCP 当前是否连着) +
   `forwardTcp` (配置回显) —— 列表行与单桥详情同构。

## 7. B3 交付物 (FR-21 / ADR-24③)

1. **`--log-file <path>`** (默认不开, 向后兼容; GUI/headless 同样生效, main 在
   分派前 init): 行格式 `[<UTC iso-8601 毫秒>] [<level>] <msg>`,
   level ∈ info/state/error。
2. **滚动 5MB×3**: 当前 ≥5MB → `path`→`path.1`→`path.2` 顺移 (共 3 份, 最旧删),
   Windows 开句柄不能 rename —— 滚动前先关句柄再顺移重建。
3. **事件接线**: 启动/日志启用、桥新建/启动/停止/删除、fleet 恢复与导入汇总、
   **桥状态机迁移** (hub.set_phase 内联, old→new)、最近错误 (set_last_error)、
   录制开始/结束、回放开始/停止、旁路转发 连接/断开/失败/超时/配置变更。
   未启用时 `write` 为无操作, 零开销。

## 8. 测试与自测 (波 2)

- **cargo test 115/0** (108 + 7): 转发字节到本地 listener、断线 3s 节拍重连、
  配置移除即断开 (EOF + 不再出站)、目标热改 (旧连断/新连收)、目标校验
  create/config 双路 400、滚动触发 (小阈值下 .1/.2 顺移、最多 3 份、当前重计数)、
  契约单测 17→19 字段随动 (默认 forwardTcp=""、forwardConnected=false)。
  `cargo fmt --check` + `clippy --all-targets` 零警告。
- **黑盒真机自测 23/23 PASS** (`%TEMP%\sh13_w2_selftest.py`, release exe):
  18300(管理台)/18301(桥)/18310(转发收站), COM1(桥)↔COM2(pyserial), COM8/COM4
  未碰 —— RX→TCP 逐字节对账、对端断开重连续传、移除后 EOF+状态归 false+
  fleet.json 无残留字段、日志含 启动/相位/转发/生命周期/录制事件、CLI 空串拒绝
  (exit 2)。测毕按 PID 清理: 零 serialhub.exe 残留、零监听、临时目录已删。

## 9. 契约变化清单 (供 QA/前端随动)

| 项 | 变化 | 影响面 |
|---|---|---|
| fleet 桥对象/单桥 detail | 17→**19** 字段: 新增 `forwardTcp`(string, 空串=关)、`forwardConnected`(bool) | QA `FLEET_ROW_FIELDS` 集合可加两键 (现有断言是子集校验, 不加也不挂); UI 卡片/抽屉可回显 |
| POST /api/fleet create body | 受理 `forwardTcp` (可选) | UI 建桥表单可选加项 |
| PATCH/POST `/api/fleet/<id>/config` | 受理 `forwardTcp` (可选, 缺省不改; 空串=关) | UI 设置抽屉 |
| fleet.json | 桥记录可选 `forwardTcp` (空不落盘); 旧清单零迁移 | 导入导出天然兼容 |
| CLI | 新增 `--log-file <path>` (默认不开) | 服务化文档可引用 |
| 单桥 `/api/status` 11 字段 | **不变** (转发是 fleet 桥层能力) | 无 |
| record/replay/recordings 等 | **不变** | 无 |

留白: MQTT/双向转发仍按 ADR-24② 入计划池; 转发断线期帧不补发 (旁路观测语义,
1024 缓冲内例外), 供架构师知悉。

