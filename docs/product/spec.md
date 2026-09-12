# SerialHub 产品规格 v1 (Sprint 1 范围 = P0)

## 0. 调研结论 (2026-09-12, 为什么做这个)

市面串口↔WS 工具全部缺至少一条硬需求:
wterm (Rust, 内嵌终端) —— 串口固定 8N1 无停止位/校验位参数, 单客户端语义, 无 Windows 预编译;
node-modbus-ws —— 在桥里做 Modbus, 暴露 JSON API 而非原始字节, 采用即推翻页面侧协议栈;
ws-serial-gateway —— 明确单客户端, 无广播;
websocket-serial-server —— 2021 年停更, JSON+base64 包帧。
结论: 自研, 把 serial_bridge.py 的实战语义 (广播/自动重开/8N2) 产品化。

## 1. 用户与场景

- 嵌入式/工控开发者: 让浏览器上位机 (自带协议栈) 通过 WS 摸到串口设备;
- 自动化测试: 无头浏览器无法授权 Web Serial, 必须经 WS 字节管道;
- 日常调试: 打开内嵌 Web 控制台, 当一个好看的串口终端用。

## 2. 功能需求 (FR)

- **FR-1 数据管道**: WS `/ws` 只传原始二进制帧, 双向。串口 RX 广播给**所有**已连客户端;
  任意客户端的帧经队列 (FIFO) 串行写入串口。多客户端并发 TX 不 panic、不丢帧序。
- **FR-2 串口配置**: 端口名、波特率 (110~2,000,000)、数据位 7/8、校验 N/E/O、停止位 1/2、
  流控 none/RTSCTS/XONXOFF。CLI 与 Web 双入口可配。
- **FR-3 自动重开**: 设备拔出/枚举抖动/读写出错时, 后台监督任务按 1s 间隔自动重开,
  永不阻塞数据面; 状态机 `Closed → Opening → Open → Retry(reason)` 必须在状态接口可见。
- **FR-4 状态接口**: `GET /api/status` 返回 JSON: 状态机相位、当前参数、客户端数、
  RX/TX 字节计数、最近错误、运行时长。`GET /api/ports` 列本机串口。
  `POST /api/config`+`/api/open`+`/api/close` 供 UI 驱动。
  契约裁定 (ADR-5): ① status 恰 9 字段 phase/port/baud/config/clients/rxBytes/txBytes/
  lastError/uptimeSec, **不含 flow** (P2 再议); ② uptimeSec = **进程运行时长**;
  ③ POST 成功 `{"ok":true}`, 失败 400 + `{"ok":false,"error":"..."}`;
  ④ `/api/open` 在已打开态为 **no-op** (热改必须 close→config→open)。
- **FR-5 CLI**: `serialhub --port COM1 --baud 115200 --config 8N2 --addr 127.0.0.1:8080
  [--list-ports] [--no-open]`。未给 `--port` 时启动为"未打开"态, 由 UI 驱动。
- **FR-6 内嵌控制台**: 单页 (内嵌进二进制, include_str!), 零外部依赖:
  连接面板 (扫口/参数/打开关闭/状态机徽章/计数器)、终端区 (HEX 与 ASCII 双视图、
  暂停/清空、自动滚动)、发送区 (HEX 或文本, 回车发送)。
- **FR-7 崩溃面**: 任何客户端断开/乱码帧/串口错误都不得 panic; 错误进状态接口。

## 3. UI 需求 (UI)

- UI-1 中文界面, 信息密度高但不挤; 响应式: 1440 桌面到 720 窄屏无横向滚动。
- UI-2 浅色工业风 + 设计令牌集中 (`:root`), 深色模式列为 P2。
- UI-3 状态机徽章四态配色: Open=绿 / Opening=绿闪 / Retry=琥珀 / Closed=灰。
- UI-4 终端区默认 ASCII, 一键切 HEX; 时间戳可选; 最大保留 5000 行 (防内存膨胀)。
- UI-5 所有按钮可键盘触达; 禁用态明确 (未打开串口时发送区禁用)。

## 4. 兼容与平台 (PLAT)

- PLAT-1 Windows 10/11 (本机验证), Linux/macOS 编译通过 (CI 配置文件齐, 本机无法验证的注明)。
- PLAT-2 现代浏览器 (Chromium/Firefox/Safari) 的 WebSocket 二进制帧。
- PLAT-3 虚拟串口对 (ELTIMA/com0com) 与真实 USB-UART (CH340) 均须工作。

## 5. 性能 (PERF, P1)

- PERF-1 921600 baud 回环持续吞吐 ≥ 900 kbps (管道不成为瓶颈)。
- PERF-2 ≥16 WS 客户端同时广播无错。
- PERF-3 本机 RX→WS 延迟 p95 < 5 ms。

## 6. 非目标 (v1 明确不做)

TLS/鉴权 (文档注明"勿暴露到不可信网络")、多串口同时桥接、串口参数热改 (需 close→config→open)、
Windows 驱动安装、Wi-Fi/蓝牙串口。

## 7. 架构决策 (见 decisions.md 完整记录)

- Rust 1.97 + tokio + serialport-rs + axum (HTTP+WS 一体); 前端零构建单文件内嵌。
- 串口读循环 → `tokio::sync::broadcast` 扇出; 客户端 TX → `mpsc` 单写者任务 (FIFO 仲裁)。
- 状态机与计数器集中在 `HubState` (Arc<RwLock>), `/api/status` 直接投影。

## 8. 验收口径

P0 全绿 = `cargo test` 通过 + `tests/` 一致性套件通过 (COM1↔COM2) + UX 报告无 P0/P1 阻塞项。
