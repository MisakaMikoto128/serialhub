# QA Sprint 4 报告 — FR-10 解锁全量集成 (波2, 2026-09-13)

作者: QA (401) · 后端: dev-sprint4-backend (已落地) · 二进制: cargo build --release 重建后全轮使用
· 纪律: 黑盒, 预期只来自 spec/ADR; COM8 仅出现在系统串口列表中, 全程未打开未占用; 测毕按 PID 杀净 (残留 0)。

## 1. FR-10 黑盒套件矩阵 (tests/test_fr10_fleet*.py)

**7 / 7 PASSED** (11.9s)。门控 fleet_ready 自动放行, 无 SKIP。

| # | 测试 | 结果 | 备注 |
|---|---|---|---|
| 1 | fr10a 双桥并存+隔离 | PASS | 跨对回环 8101⇄COM1⇄COM2⇄8102 双向逐字节; 静默守窗无串扰; 逐桥 rxBytes/txBytes 对账精确; clients 归零 |
| 2 | fr10g fleet CRUD | PASS | 建/列表 13 字段/详情/start→open/stop→closed/delete→移除+端口释放 |
| 3 | fr10 listen 冲突 | PASS | 同 listen 建桥被拒 (400+ok:false+error), 原桥不受损 |
| 4 | fr10b 持久化 | PASS | taskkill /F 强杀 → 同 --fleet 重启 → 两桥自动恢复 open, listen 不变, 数据面回环可用 |
| 5 | fr10f 端点稳定+重接 | PASS | listen 五次读取不变; 对端断开重接数据面自愈; COM99 → retry+lastError 可见, retry 态可删 |
| 6 | fr10g tap | PASS | /api/fleet/<id>/tap 收串口 RX 512B 逐字节, 与数据面并存互不干扰 |
| 7 | fr10h CLI 兼容 | PASS | 旧参数→恰一座兼容桥 (open); /api/status 11 字段; 旧 /ws 双向回环; 兼容桥自身数据面 + tap 可用 |

### 套件修订注记 (解锁过程中按已落契约修订, 非"改预期凑绿")

1. **计数器命名**: 行字段为 `rxBytes/txBytes` (ADR-6② 全产品同名同义), 非波1任务速记 `rx/tx` —— FLEET_ROW_FIELDS 已改并加注释; `serial` 为嵌套对象 `{port,baud,dataBits,parity,stopBits,flow}`, POST /api/fleet 同形状 (传扁平串 400: "expected struct SerialReq")。
2. **maxClients 回显** (ADR-14②): 行字段集 12→13, 套件强制断言。
3. **播种桥**: 纯控制面进程 (无 --port 且无恢复桥) 自动播种一座空白兼容桥 (name=CLI, serial.port="", closed) —— FR-5"未打开态"的 fleet 化。新增 `fleet_purge` 清场后再做行数断言。
4. **兼容桥数据端口**: = 管理台端口 +1 起探测 (FR-10a 每桥独立数据端口), 不再与旧地址同端口; 旧端点兼容由 /api/status 200 + 旧 /ws 同端口回环承载 (fr10h 实证两者都通)。原 "listen==旧地址" 断言按 FR-10a 修订。
5. **单桥模式守卫** (黑盒实证, 实现+单测在位): 多桥时旧单桥接口按契约拒绝 —— GET /api/status→409 `{"error":"当前不是单桥模式, 旧单桥接口仅在恰好一座桥时可用 (见 GET /api/fleet)","ok":false}`; 控制面 /ws→503 `{"error":"当前不是单桥模式, 数据面请连各桥的 ws://<listen>/ws","ok":false}`。conftest wait_http_ready 相应接受 200/409。
6. **旧式测试进程注入 `--no-fleet`** (conftest start_bridge): 旧 CLI 进程默认持久化到全局 `%APPDATA%/SerialHub/fleet.json` 且下次启动恢复, 会跨用例污染整套件 (恢复桥+新建兼容桥 → 多桥 → 409)。`--no-fleet` = 旧二进制语义 (本无持久化), 单桥行为零变化; 另加会话级 `sanitize_global_fleet` (仅当清单内桥名全为 CLI/qa-* 测试产物时清除, 真实用户配置不动)。
7. 桥 stop 后串口句柄异步释放存在亚秒级延迟 → 新增 `wait_serial_free` 轮询判据 (8s 不释放才算违约, 实测瞬时)。

## 2. 全量回归

- **cargo test --release: 52/52 PASSED** (2.32s)。
- **pytest tests/: 35 passed, 1 FAILED** (62s) —— 旧 29 条守护中 28 绿, 1 红见 DEF-1; FR-10 新 7 条全绿。

### DEF-1 (P1, dev-backend) 旧端点 /api/config 的 maxClients 被静默忽略

- **契约**: FR-9b / ADR-9b① (maxClients, 0=不限) + FIX-17 回显; POST 成功必须生效。
- **现象**: `--max-clients 1` 启动后, POST /api/config `{"maxClients":0}` 与 `{"maxClients":4}` 均返回 200 `{"ok":true}`, 但 status.maxClients 恒为 1 (0 与非 0 全部无效); 守护用例 test_fr9b_max_clients 在"解除后 maxClients 应为 0"断言失败 (实测 1)。
- **对照**: POST /api/fleet/<id>/config `{"maxClients":4}` (ADR-14③) **生效** —— status.maxClients 变 4。故 /api/config 的 maxClients 字段在新 fleet 架构下未接到兼容桥上 (其余字段如 flow 正常, fr2_flow 用例绿)。
- **复现** (60s):
  1. `target/release/serialhub.exe --headless --port COM1 --baud 115200 --config 8N2 --max-clients 1 --addr 127.0.0.1:18090 --no-fleet`
  2. `curl -X POST -H "Content-Type: application/json" -d '{"maxClients":0}' http://127.0.0.1:18090/api/config` → `{"ok":true}`
  3. `curl http://127.0.0.1:18090/api/status` → `"maxClients":1` (期望 0); 换 `{"maxClients":4}` 依旧 1。
  4. 单测: `python -m pytest tests/test_fr9_config_full.py::test_fr9b_max_clients -v` → FAIL。
- **处置建议**: /api/config (兼容桥上下文) 将 maxClients 转调兼容桥 (同 /api/fleet/<id>/config 路径); 或架构师裁定旧端点废弃该字段并修订守护用例 (需裁决, QA 未动此断言)。

### DEF-2 (P1, dev-ui) 管理台卡片速率数字恒 0, 不随流量刷新

- **契约**: FR-10c "RX/TX 速率 (每秒刷新)"; FR-10d 有流量时点亮。
- **现象**: 持续流量 (ws://127.0.0.1:8081/ws 每 100ms 发 512B, 约 10s) 期间, Playwright 每秒读取 CLI 卡片 `.v-rx` 六次全部为 `0 B/s`; 同期 API `/api/fleet` 的 rxRate/txRate ≈ **4527.2** 正常跳动 (统计引擎无错)。流程图/徽章/连接数等其余渲染正常 (徽章随停/启切换已实证)。
- **复现**: 起桥 (任一) → 后台持续发流 → 打开管理台看速率数字, 或 `docs/team/reports/qa-sprint4/06-traffic-rates.png` (流量中拍摄, 恒 0)。
- **处置建议**: dev-ui 检查卡片速率绑定/轮询 (疑未接 rxRate/txRate 或刷新节流写死)。

## 3. 真后端管理台集成冒烟 (Playwright, 全程真后端无 mock)

后端: `--headless --port COM1 --baud 115200 --config 8N2 --addr 127.0.0.1:8080 --fleet $TEMP/qa_smoke/fleet.json` (兼容桥 CLI→数据端口 8081)。

| 步骤 | 验证点 | 结果 |
|---|---|---|
| ① 仪表盘渲染 | CLI 卡片: 徽章"运行中"; 流程图三节点 串口(COM1)⇄桥(CLI)⇄网址(127.0.0.1:8081); 速率字段/连接数/时长渲染 | PASS (速率数值恒 0 → DEF-2) |
| ② UI 新建第二座桥 | 表单 名称=qa-ui-8181, 串口扫描后选 COM2, 网址=127.0.0.1:8181 → "创建并启动" | PASS |
| ③ 列表两座 | CLI 与 qa-ui-8181 双卡片, 新桥徽章"运行中"、listen 127.0.0.1:8181 | PASS |
| ④ 停/启跟手 | 停止→徽章"已停止"+按钮变"启动"; 启动→"运行中"+"停止" | PASS |
| ⑤ WS 数据面按桥网址 | 跨桥回环双向: 8181/ws→COM2→COM1→8081/ws 与反向, 各 512B 逐字节一致; fr10h 已证单桥时旧路由 8080/ws 亦通 | PASS |
| ⑥ 流量点亮 | API 速率正常跳动; UI 速率恒 0 | **FAIL → DEF-2** |

冒烟观察 (非阻断, 交 UX/Dev 参考):
- 新建桥表单"网址端口"只填 `8181` 时**静默不提交** (不发 POST, 对话框停留, 无醒目错误提示, 仅提示行"网址要写成 IP:端口") —— UI-0 危险/失败反馈可再醒目些 (P3)。
- 串口下拉扫描会列出 COM8 — USB-SERIAL CH340 (真实板卡口); UI 无"排除占用"问题, 仅 QA 纪律未触碰。

## 4. 截图清单 (docs/team/reports/qa-sprint4/)

| 文件 | 内容 |
|---|---|
| 01-dashboard-initial.png | 初始仪表盘: CLI 桥卡片 (徽章运行中/流程图/速率区) |
| 02-create-dialog-filled.png | 新建桥对话框已填 (qa-ui-8181 / COM2 / 127.0.0.1:8181) |
| 02-create-dialog-after.png | 网址只填 8181 时点创建被静默拦截的现场 (UX P3 证据) |
| 03-two-bridges.png | 双桥并存 (两卡片均"运行中") |
| 04-second-stopped.png | qa-ui-8181 已停止 (徽章+按钮态) |
| 05-second-restarted.png | qa-ui-8181 重启回"运行中" |
| 06-traffic-rates.png | 持续流量中拍摄: API 速率 ~4500 B/s 而 UI 恒 0 B/s (DEF-2 证据) |

## 5. 卫生与影响面

- 测毕按 PID 强杀 serialhub.exe (4472), 复查残留 0; 全局 %APPDATA%/SerialHub/fleet.json 无测试残留; 未触碰 COM8 (仅出现在扫描列表, 未打开)。
- tests/ 改动: conftest FR-10 扩展区按第 1 节注记修订; 旧 29 条测试代码零改动 (fr9b 之败为实现违约, 未动断言, 见 DEF-1)。
- 波2 待办移交: DEF-1 → dev-backend; DEF-2 → dev-ui; 两单修复后 `python -m pytest tests/ -v` 应 36/36。
