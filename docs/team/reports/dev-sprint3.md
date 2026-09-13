# Dev 报告 — Sprint 3 (FR-9 配置全量双入口)

日期: 2026-09-12 · 角色: Dev · 状态: 待验收
范围: 仅 FR-9 四条 (9a addr 自我重启 / 9b --max-clients / 9c 等价命令 / 9d 对照表)。
契约变更: `/api/status` 9→**10 字段** (新增 `maxClients`, ADR-9 修订 ADR-5①); 新增端点
`POST /api/restart` (FR-9a, gui 专用); `POST /api/config` 增可选字段 `maxClients`。
数据面 `/ws` 帧语义零改动。

## 1. FR-9b --max-clients (契约先行) — 完成

- **CLI**: `--max-clients <n>` (默认 0=不限; u32; 负数/非数字/缺值均报错)。
- **API**: `POST /api/config` 增可选字段 `maxClients` (部分更新语义同现有字段);
  `/api/status` 第 10 字段 `maxClients`。
- **执行点**: WS 握手完成**之后**、计入 clients **之前** 检查 —— 已满则发送
  `Close(code=1013, reason="max clients")` 并直接断开 (不计入 clients, 老客户端不受影响)。
- **单测**: status 契约字段集断言 9→10 (含 maxClients=0 默认); hub maxClients 往返;
  `ws_over_max_clients_rejected_with_1013` (真实握手: 第 1 条 accepted、第 2 条收到
  1013/"max clients"、status clients=1 且 maxClients=1)。
- **UI**: 连接面板新增「最大客户端」数字输入 (0=不限), 锁定规则同波特率 (非 closed 态锁定);
  经 `/api/config` 的 maxClients 字段下发。
- **自验**: headless 实例 `--max-clients 2` → status 10 字段齐 + maxClients=2; 单测 1013 拒连全过。

## 2. FR-9a addr 自我重启 — 完成 (含一处实现顺序的关键偏离, 已验证)

- **UI**: 连接面板顶部「地址」输入 + 「应用并重启」按钮 (两段确认)。**仅桌面壳可用**:
  wry `with_initialization_script("window.__SERIALHUB_SHELL = true;")` 注入壳标记,
  页面无标记 (浏览器打开 / headless) 时控件置灰 + 提示"仅桌面壳窗口支持改地址重启;
  headless 模式请重启进程"。
- **端点**: `POST /api/restart {addr}` —— 校验地址; headless (gui=false) 拒绝 400 并提示;
  登记 `restart_to` 槽 + 触发与托盘退出**同一** watch 停机序列。
- **实现顺序偏离 (与工单"spawn→退旧"不同, 原因如下)**: 工单原序是"先 spawn 新实例 → 旧实例
  优雅退出"。实测该顺序在 ELTIMA 虚拟串口对上**死锁**: 新实例 auto-open 撞上旧实例尚未
  释放的 COM2 → 打开失败; 且**打开失败的进程自身会持有驱动级锁** (pyserial 同样打不开,
  持续 拒绝访问; 杀掉该进程才释放, 已两次复现)。故改为: 旧实例先 `Close`+`stop_active`
  +250ms (COM 已释放) → **再 spawn 新实例** (DETACHED|NEW_GROUP, current_exe + 等价参数
  仅替换 addr) → 新实例 bind 重试 ≤2s (本场景秒绑) → 旧实例按有界宽限退出。对外行为与
  spec 一致 (新实例 ≤2s 内接管), 仅内部顺序对调; 已在代码注释与报告双处说明。
- **bind 重试**: run_service 绑定循环 250ms 间隔 / ≤2s 总限, 仍失败按 FR-8 报"端口被占用"
  (GUI 弹 MessageBox, headless stderr+exit)。
- **单测**: `restart_rejected_in_headless` (400 + "重启进程" 提示 + 服务不退出)。
- **自验 (E2E)**: GUI 实例 A (8081+COM2+max-clients 3, phase=open) → `POST /api/restart
  {addr:8085}` → `{"ok":true}` → A 退出 → 实例 B 在 8085 `phase=open, port=COM2,
  maxClients=3` —— **COM 零撞锁、无 retry 间隙** (对比工单原序实测 retry 卡死 110s+)。

## 3. FR-9c 等价命令一键复制 — 完成

- 连接面板新增「等价 CLI 命令 (FR-9c)」块: 实时渲染
  `serialhub --port X --baud Y --config Z --max-clients N --addr A` (与**表单当前值**一致,
  任意配置项 input/change/状态轮询即时刷新); 📋 复制按钮 (clipboard API + 选中文本兜底);
  提示行注明 "headless 场景请追加 --headless" (页面无法感知进程模式, 契约未加字段, 以提示代替)。
- **自验**: Playwright 读取 `#equivCmd` = `serialhub --port COM2 --baud 115200 --config 8N2
  --max-clients 3 --addr 127.0.0.1:8085` (与实例实际参数一致); 修改 maxClients 输入 → 命令
  即时变为 `--max-clients 7` ✓。

## 4. FR-9d README 配置对照表 — 完成

README 新增「配置对照表 (CLI ↔ 控制台 UI, FR-9d)」: `--port`/`--baud`/`--config`/
`--max-clients`/`--addr`/`--gui|--headless`/`--no-open`/`--list-ports` 逐项对应 UI 位置与说明。

## 5. 契约变更清单 (ADR-9)

| 项 | 变更 |
|---|---|
| `/api/status` | 9→10 字段: 新增 `maxClients` (u32, 0=不限) —— hub 契约单测字段集断言同步 (ADR-9①) |
| `/api/config` | 新增可选字段 `maxClients` (u32; 省略=不变; 0=不限) |
| 新端点 | `POST /api/restart {addr}`: 仅 GUI (headless 400 "headless 模式改地址请重启进程"); 成功 `{"ok":true}` 后旧进程 ≤2s 优雅退出、新进程接管 |
| `/ws` | 超限新连接: 升级后立即 `Close(1013, "max clients")`, 不计入 clients (ADR-9③) |
| `--max-clients` | CLI 新参数 (u32, 默认 0) |
| 未变更 | `/ws` 路径固定 (ADR-9④); 停机/shutdown/托盘语义; 其余 9 字段含义 |

## 6. 自验证据汇总 (COM2 + 8081/8083/8085; 未碰用户实例 8080+COM1, COM8 未碰)

| 项 | 结果 |
|---|---|
| cargo build / cargo test | 0 警告; **29 passed / 0 failed** (含 5 个新 FR-9 用例) |
| status 10 字段 | headless `--max-clients 2` 实测 10 字段齐、maxClients=2 |
| 1013 拒连 | 单测: 第 2 条 WS 收 1013/"max clients", clients=1 (真实握手) |
| /api/restart headless 守卫 | 400 + "headless 模式改地址请重启进程", 服务不退出 (单测+实测) |
| 自我重启 E2E | 8081→8085: 旧退新起、phase=open 无间隙、maxClients=3 保留、COM2 零撞锁 |
| 等价命令 | 渲染/实时刷新/与实例参数一致 (Playwright 读取断言) |
| 进程卫生 | 收工无 serialhub/python 残留, COM2 空闲 |

## 7. 遗留 / 建议

1. `--flow` 不在 CLI 契约 (FR-2 CLI 从未包含), 自我重启参数组亦未带 flow —— 若 flow 需要
   跨重启保留, 需契约裁定 (现状: 重启后 flow 回到 none)。
2. 重启交接瞬间 (~1.5s) 数据面短暂中断属设计内 (旧退新起); WS 客户端自动重连已验证。
3. 自我重启仅桌面壳可用; "浏览器页触发壳重启"可作为 P2 (需壳标记之外的第二信任层)。
4. pytest 既有 9 字段断言将由 QA 同步修订 (ADR-9①, QA 文件所有权)。

## 8. 启动命令

```bash
cargo run --release -- --addr 127.0.0.1:8080 --port COM1 --max-clients 0   # GUI (默认)
cargo run --release -- --headless --addr 127.0.0.1:8080 --port COM1        # headless
```

## ADR-10 --flow 入 CLI + 重启携带 (2026-09-12 追加)

架构师裁定: "flow 不在 CLI" 恰是 FR-9 要消灭的不对等 —— 补齐三项:

1. **CLI `--flow <none|rtscts|xonxoff>`** (默认 none): 校验复用 `config::Flow::parse`
   (与 POST /api/config 的 flow 完全同一套); Startup/HubState 全链路携带
   (run_service 将 su.flow 写入统一配置)。
2. **自我重启携带**: `respawn_args` 参数组新增 `--flow <当前值>` (显式携带, 含 none),
   重启后流控不再回 none。抽出纯函数 `respawn_args` 便于单测。
3. **README 对照表** 补 `--flow none` 行。

单测 +2: cli `--flow` 合法 (rtscts/XONXOFF 大小写/none) 与非法 (hardware/缺值);
`respawn_args_carry_flow` (rtscts 携带 + none 显式携带)。

自验 (COM2↔COM1 对端, 本轮 COM1 已获准使用): GUI `--flow xonxoff --port COM2 --max-clients 4`
→ WS 发 `FLOW1` → 对端 COM1 (pyserial xonxoff) 收到 `FLOW1` (flow 配置到达串口) ✓;
`POST /api/restart {8086}` → 新实例起后 WS 发 `FLOW2` → COM1 收到 `FLOW2` ——
**重启后 flow 仍生效** ✓。cargo build 0 警告; cargo test **30/30 全绿**。
(修复过程中曾因补丁双跑造成 cli.rs `--max-clients` 臂重复 —— 已去重并清理一处别扭测试写法。)

## 修复轮 FIX-16~22 (回路⑤; Dev 中断后由架构师验收收尾, 2026-09-12)

代码经核验全部落地 (ui/index.html + src/api.rs), 契约 10→11 字段 (ADR-11, flow 回显):
- FIX-16: 等价命令始终显式携带 --flow (ui/index.html:444, 含 none) ✔
- FIX-17: /api/status 增 flow (hub.rs status 契约单测含 "flow":"none" 断言); UI 流控以
  服务端回显为准, 打开动作显式携带表单 flow ✔
- FIX-18: 发送区使能只由相位决定, "徽章 open ⇒ 发送可用"恒成立 (自重启死锁根治) ✔
- FIX-19: WS 被 1013 拒绝时页面横幅/提示行明示"已达最大客户端数" ✔
- FIX-20: 退出/重启按钮按 __SERIALHUB_SHELL 壳标记门控, 浏览器页置灰 ✔
- FIX-21: 自重启确认文案明示"终端历史将清空" ✔
- FIX-22: label/按钮换行 CSS 修正 ✔
收尾修正: Dev 中断前 api.rs 残留 1013 对照实验残码 (不发 close + [dbg] 打印),
架构师恢复正确实现 (Close(1013,"max clients") + CloseFrame 导入 + mut)。
cargo test 31/31 全绿 (30 旧 + ws_over_max_clients_rejected_with_1013 恢复)。
