# Dev Sprint 4 后端报告 — 多桥管理器 (FR-10 / ADR-13)

作者: 后端主程 (201, 兼统计引擎 202) · 2026-09-13 · 依据: spec FR-10a-h / decisions.md ADR-13 /
backlog「Sprint 4 · dev-backend」· 任务契约 (多桥核心/独立 listener/fleet.json/FR-10g API/统计引擎)。
改动范围: `src/` 独占 (`fleet.rs`、`stats.rs` 新增; `api.rs`、`cli.rs`、`main.rs`、`gui.rs`、`service.rs` 修改),
`Cargo.toml` 零新依赖。不 commit; 真机自测只用 COM1/COM2 + 127.0.0.1:8100-8199, 进程已按 PID 清场。

## 1. 架构 (文字稿)

```text
                控制面 (管理台, 固定 --addr, 永远可达, FR-10a)
  GET/POST /api/fleet(/<id>...)…  ────────────────┐
  旧 /api/status、/api/config、/api/open|close、/ws│  ← 兼容分发: 恰一座桥时同进程
                  ┌───────────────────────────────┘     Arc 直调 (非 HTTP 代理)
           BridgeManager (fleet.rs)
            │  bridges: BTreeMap<id, Arc<Bridge>>  ←→ fleet.json (变更即写, 原子替换)
            │  ├─ 统计引擎任务 (200ms 采样各桥累计字节)
            │  ├─ legacy 指令泵 (GUI 托盘 打开/关闭串口 → 唯一桥)
            │  └─ 相位聚合任务 (150ms 轮询 → GUI 托盘四态图标)
            ├── Bridge b1 ── 数据面 listener 127.0.0.1:8101 ── /ws (纯二进制, api::client_loop)
            │      └─ HubState + supervisor(1s 自动重开) + tx 队列 + broadcast —— 全部复用单桥内部件
            ├── Bridge b2 ── 数据面 listener 127.0.0.1:8102 ── /ws
            └── Bridge bN ── …
```

- **复用而非复制**: 桥 = `HubState`(单一真相) + `supervisor::run_supervisor`(FR-3 自动重开) +
  `PortCtx`(tx 队列/广播) + `api::client_loop`(数据面主循环) 原样组装; ws 帧语义零变化 (硬约束 1/2/3)。
- **api.rs 重构**: handler 逻辑抽成 `*_core` 纯函数 (入参 = 桥的 ctx/cmd_tx), legacy 路由与管理台共用,
  核心逻辑一字未改。
- **FR-10f 端点稳定**: 桥 listen 端口建桥时绑定后终生不变 (端口 0 → 回填实际端口并持久化);
  桥数据面 serve 异常 → 按 1s 重绑同一端口重建 (hub/监督任务不动); 串口掉线沿用监督任务自动重开。
  stop = Close(串口优先) + 断本桥 WS + 关数据端口; start = 重绑同端口 + autoOpen 则自动开串口。
- **统计引擎 (202)**: `stats::RateWindow` 每桥 1s 滑窗; 管理器任务 200ms 推累计字节, 读取时窗口内
  首尾差分 → 字节/秒 (保留一条过期样本作基线锚点, 滑动时平滑衰减)。时间戳全由调用方注入, 单测确定性。

## 2. FR-10g API 契约

控制面 (默认 127.0.0.1:8080, `--addr` 可改):

| 端点 | 方法 | 说明 |
|---|---|---|
| `/api/fleet` | GET | `{"ok":true,"bridges":[…]}`, 按 id 数值序; 每项含 12 契约字段 + `running/maxClients/autoOpen` 扩展 |
| `/api/fleet` | POST | 建+启动; body `{name, serial{port,baud,dataBits,parity,stopBits,flow}, listen, autoOpen?, maxClients?}`, serial/listen 可省 (空串口/随机端口); 成功 `{"ok":true,"id":"bN","listen":…}`; 端口冲突 400 (含"冲突") |
| `/api/fleet/{id}` | GET | 单桥详情 (同列表项结构); 缺失 404 `{"ok":false,"error":"桥不存在: …"}` |
| `/api/fleet/{id}/start` `/stop` `/delete` | POST | 幂等; stop 释放 COM; delete = stop+移除+持久化 |
| `/api/fleet/{id}/config` | PATCH 或 POST | 扁平字段 (port/baud/dataBits/parity/stopBits/flow/maxClients) 或嵌套 `serial{}` (嵌套优先), 另支持 `name/autoOpen`; 带 `listen` 一律 400 (FR-10f); 成功即持久化 |
| `/api/fleet/{id}/tap` | WS | 旁看该桥串口 RX 原始字节 (与数据面同一 broadcast 源), **只读**: 入站帧忽略、不进 tx 队列、不计 clients |

单桥详情/列表项字段: `id, name, serial{port,baud,dataBits,parity,stopBits,flow}, listen, phase,
running, clients, maxClients, rxBytes, txBytes, rxRate, txRate, lastError, uptimeSec, autoOpen`。
`phase` 恒取四态 `closed/opening/open/retry` (对齐 UI-3 徽章与 QA 契约: stop 后 = closed);
停止态由 `running:false` 区分。`rxRate/txRate` 保留 1 位小数 (字节/秒)。

**兼容分发 (FR-10h)**: 恰好一座桥时, 旧单桥端点在控制面上直调该桥 —— `/api/status` `/api/config`
`/api/open` `/api/close` 直调对应 `*_core`; `/ws` 同一个 `client_loop` (帧语义零变化, 停机随桥断开);
`/api/ports` 全局; `/api/shutdown` 停整进程; `/api/restart` = 管理台原地换绑 (ADR-12, 各桥不动)。
非"恰一座桥"时: 旧端点 409 (json `ok:false`), `/ws` 503 拒绝升级。

## 3. 持久化 (fleet.json, FR-10b)

- 路径: `%APPDATA%\SerialHub\fleet.json`; `--fleet <path>` 指定; `--no-fleet` 关闭 (GUI/headless 同规则)。
- 变更即写: 建/删/改配 (含旧 `/api/config` 落在兼容桥上) 立即落盘; 临时文件 + rename 原子替换,
  断电只丢最后一次变更不留半截文件。启动时存在则恢复全部桥 (id/名称/串口/端口/autoOpen/maxClients),
  新桥 id 续号 (fetch_max)。
- 恢复时单条记录非法 → 跳过并告警; 端口暂被占用 → 以 `running:false + lastError` 入队 (不丢配置),
  用户释放端口后可 start; 恢复期间抑制 persist, 避免把暂时绑不上的桥从清单抹掉。

## 4. CLI 兼容 (FR-10h) 行为矩阵

| 启动方式 | 结果 |
|---|---|
| `--port COM1 …` | 建兼容桥 (自动开串口, 除非 `--no-open`), 数据端口 = 管理台端口+1 起向上探测 (≤20 个, 全忙→随机) |
| 无 `--port`, 无 fleet 桥 | 建空串口兼容桥 (旧"无参启动 = 控制台驱动"行为) |
| 无 `--port`, fleet 已有桥 | 只恢复 fleet, 不建兼容桥 |
| fleet 存在且 `--port` 显式给出 | 兼容桥 + 恢复桥并存 |

GUI (`gui.rs`) 同步切到 `fleet::run_manager`: 管理台就绪 → WebView; 托盘四态 = 聚合相位
(任一 open→绿; 否则 opening/retry→闪/琥珀; 全停→灰); 托盘打开/关闭串口经 legacy 泵转发到唯一桥。

## 5. 单测 (31 → 53, 全绿; `cargo test` 多轮压测稳定)

新增 22 项:
- `stats` (4): 不足两样本归零 / 字节每秒差分 / 窗口滑动衰减与清理 / 计数器回退防御。
- `fleet` (15): listen 解析变体; fleet.json 往返; 垃圾清单与非法串口拒绝; from_cli 映射
  (`--fleet/--no-fleet`); **fleet CRUD + 端口冲突** (建/删/详情 404/stop 关端口/start 同端口恢复);
  **改配 + 持久化** (PATCH/POST、嵌套与扁平、非法拒绝不落盘、listen 不可改); 删除即持久化;
  **恢复** (从文件恢复两桥 + id 续号 b3); **兼容分发** (恰一座桥直调/两桥 409//ws 503/删除后恢复);
  **兼容 ws 数据面** (RX 注入→帧、TX→tx 队列、clients 计数); **tap 只读** (旁看 RX、不进 tx、不计 clients、404);
  **多桥隔离** (b2 广播不漏进 b1 客户端); 速率入详情; 采样任务挂接; 零桥时旧端点 409。
- `fleet` (补): maxClients 回显与 config 往返 (SQ-UI-1/ADR-14②③: 列表+详情回显、PATCH/POST 往返、0=不限);
  修复轮 DEF-1 守护: 兼容模式旧端点 POST /api/config 的 maxClients 生效回显 (根因 = ConfigReq 丢 serde rename)。
- `cli` (1): `--fleet/--no-fleet` 解析 (含缺值/空值/并存优先级)。

既有 31 项**零语义改动**全绿 (`Cli::startup` 与 `service::run_service` 标注 cfg_attr 保留为 legacy 路径)。

## 6. 真机自测 (release 构建, COM1⇄COM2 ELTIMA 对, 8100-8106)

- 兼容模式 (8100 管理台 + b1@8101 绑 COM1): `/api/status` open; ws `ws://…:8100/ws` RX 18B 逐字节、
  TX 14B 到对端; rxBytes/txBytes/clients 对账。
- fleet 全流程: 建 b2@8102 绑 COM2 自动开串口 (open) → 双桥后旧端点 409 → **跨桥环路** (b2 ws TX 17B →
  COM2→COM1→b1 RX 36→53 精确) → tap 旁看同字节且 b1.clients=0 → 同端口建桥 400"冲突" → stop 后端口
  关闭 → start 同端口恢复 → PATCH baud/name 回显 → delete 404。
- 速率: 持续灌 50KB → rxRate≈16.4KB/s (1s 滑窗); 静默 1.5s → 归零。
- 持久化: `--fleet` 实例 (8103) 建两桥落盘 → taskkill /F 断电式强杀 (按 PID) → 同清单重启 →
  三桥全部恢复同端口 (COM1 桥因两实例并存呈 retry, FR-3 自动重开正确)。自测进程已全部按 PID 清场,
  临时清单已删, **默认 %APPDATA% 未留 fleet.json**。

## 7. 顺带修复的两个遗留竞态 (基线即存在, 本轮压测暴露)

1. **watch 晚订阅漏停机 → 优雅停机超时 → `exit(0)` 杀死测试进程** (service.rs 头注释自己警告过的坑,
   但首轮 serve 接收端仍在 Ready 之后才订阅): 现首轮订阅提前到 Ready 之前 (`first_serve_sd`),
   fleet 控制面同构修复。基线 31 项套件在本机已可复现该截断 (test result 行缺失)。
2. **JoinHandle 双重 poll panic**: 停机时 main_sd 与 serve 任务同时完成, select 随机选中 serve 臂后
   finalize 再包装任务重复 await 同一 handle → tokio panic。重构为按退出路径区分: serve 臂获胜则结果
   已消费 (不再 await), main_sd 获胜才交 finalize 宽限等待。两个循环 (legacy + 管理台) 同修。

## 8. 遗留 / 交接

- **GUI 换址跟随**: 管理台原地换绑后会重发 `ServiceEvent::Ready(新地址)`, GUI 壳目前仅在启动时消费
  Ready —— 页面跟随新地址需壳层配合 (UI 波 2)。
- **fleet 恢复的 COM 竞争**: 清单里的桥与显式 `--port` 兼容桥可能指向同一 COM (监督任务自动重试,
  不崩溃); UI 提示策略待 UX 定。
- **`stopped` 相位**: 未采用, `phase` 恒四态; UI 若需"已停止"徽章, 用列表项 `running` 字段。
- clippy 仅剩 gui.rs 原有 1 条 map_or 提示 (非本轮代码, 未动)。
