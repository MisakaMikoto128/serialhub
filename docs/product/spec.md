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
- 日常调试: 管理台随时看每座桥的状态与统计; 串口原始流可在桥抽屉「串口数据」只读旁看
  (FR-10e, v1 只看不发 —— 人为发数据属客户端程序职责)。

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
  ④ `/api/open` 在已打开态为 **no-op** (热改必须 close→config→open);
  ⑤ (ADR-8) `POST /api/shutdown` 优雅停机整进程 —— 退出不能只依赖托盘菜单 (Sprint2 UX P1-1);
  ⑥ (ADR-11) status 契约 10→**11 字段**: 新增 `flow` (none/rtscts/xonxoff) —— 配置回显是
  双入口对等 (FR-9) 的前提, 无回显则 UI 一次开关即静默降级流控 (Sprint3 UX P1-2 实证)。
- **FR-5 CLI**: `serialhub --port COM1 --baud 115200 --config 8N2 --addr 127.0.0.1:8080
  [--list-ports] [--no-open]`。未给 `--port` 时启动为"未打开"态, 由 UI 驱动。
- **FR-6 内嵌控制台**: 单页 (内嵌进二进制, include_str!), 零外部依赖:
  连接面板 (扫口/参数/打开关闭/状态机徽章/计数器)、终端区 (HEX 与 ASCII 双视图、
  暂停/清空、自动滚动)、发送区 (HEX 或文本, 回车发送)。
- **FR-7 崩溃面**: 任何客户端断开/乱码帧/串口错误都不得 panic; 错误进状态接口。

- **FR-8 桌面客户端形态 (GUI 壳)**:
  - 默认启动为**原生窗口** (WebView 内嵌现有控制台, 同一 ui/index.html, 不做第二套界面);
  - **关闭窗口 = 退到系统托盘**, 桥继续跑; 首次隐藏时托盘气泡提示一次;
  - 托盘菜单: 显示主窗口 / 在浏览器打开控制台 / 打开串口 / 关闭串口 / 退出 (唯一真退出入口);
  - 托盘图标随状态机变 (Open=绿 / Retry=琥珀 / Closed=灰), 悬停 tooltip 显示相位与端口;
  - `--headless` 走旧行为 (纯 CLI 前台, 无窗口无托盘) —— 自动化测试与脚本场景专用;
  - 地址被占用时启动报错退出 (错误含"端口被占用"字样), 不静默;
  - 已打开态下窗口刷新/重连不丢终端历史 (内嵌同一套前端逻辑, 天然满足)。

- **FR-9 配置全量双入口 (UI ⇄ CLI 完全对等, 面向全体开发者)**:
  - FR-9a **监听地址 UI 可改**: GUI 模式下修改 addr → 桥**自我重启** (spawn 同 exe 同参数替换 addr →
    优雅退出旧实例; 新实例启动时对 bind 做 ≤2s 重试以平滑交接); headless 模式 UI 明示"改地址请重启进程"。
  - FR-9b **最大客户端数**: `--max-clients <n>` (0=不限, 默认) + UI 可设; 超限的新客户端 WS 以
    close code 1013 拒绝; status 契约**增至 10 字段** (新增 `maxClients`, ADR-9 修订 ADR-5①)。
  - FR-9c **等价命令一键复制**: 控制台显示与当前全部配置等价的 CLI 启动命令 + 复制按钮 ——
    界面上每个配置项都能在 CLI 找到对应物, 反之亦然 (含 --headless 提示)。
  - FR-9d README 增「配置对照表」: 每个参数 CLI ↔ UI 位置一一对应。
  - 启动 bind 重试 ≤2s (上述交接需要), 仍失败按 FR-8 报"端口被占用"。

- **FR-10 桥接管理器 (ADR-13, 产品主体)**:
  - FR-10a 控制面/数据面分离: 管理台固定地址 (默认 127.0.0.1:8080) 永远可达;
    每座桥独立数据端口 (串口 ⇄ 该端口)。
  - FR-10b 多桥: 同时运行 N 座桥; 列表新建/启动/停止/改配/删除; 配置持久化
    fleet.json (变更即存), 进程重启自动恢复全部桥; --fleet 指定路径, --no-fleet 关闭。
  - FR-10c 每桥统计: RX/TX 速率 (每秒刷新) 与累计字节、连接数、最近错误、运行时长;
    管理台总览一行一座桥。
  - FR-10d 可视化: 每桥流程图 (串口 ⇄ 桥 ⇄ 客户端) + 四态徽章 + 速率火花线;
    有流量时线路点亮。
  - FR-10e 终端降级为每桥「侦听」抽屉 (只旁看串口流), 不再是主界面。
  - FR-10f 端点稳定: 桥端口生命周期内不变; 串口掉线自动重开 (FR-3); listener 异常
    自动重建; 客户端重连即恢复。
  - FR-10g API: GET /api/fleet (列表+统计) / POST /api/fleet (新建) /
    POST /api/fleet/<id>/start|stop|delete|config; GET /api/fleet/<id> 详情;
    WS /api/fleet/<id>/tap 旁看串口流; 桥数据面仍为 ws://<listen>/ws。
  - FR-10h CLI 兼容: 旧单桥参数等价于建一座桥并启动; --fleet <path> 指定清单。

- **FR-12 自动重连可选 (每桥)**: autoReconnect 默认 true; CLI --reconnect/--no-reconnect;
  建桥/改配均可设; false 时串口断开直接「已停止」(不重试), 手动打开不受影响;
  单桥 /api/status 与 fleet 桥对象回显 (契约 12→13 / 14→15 字段)。

- **FR-13 管理台设置**: 控制面端口可在管理台设置中修改 —— POST /api/manager/addr
  原地换绑 (复用 ADR-12 机制), fleet.json 持久化, 重启恢复; 壳 webview 与浏览器页
  自动跟随新地址 (断档 <1s); 页头「打开面板」按钮 (桌面壳→系统默认浏览器,
  浏览器页→新标签)。
- **FR-14 主题插件**: themes/ 目录下每个 *.css = 一套主题 (只覆盖 :root 设计令牌);
  GET /api/themes 列出, /themes/<file> 静态服务, --themes-dir 可指定; 管理台主题
  选择器**无刷新**动态切换, 选择记忆在浏览器本地; 内置: 浅色 (默认) / 深色 /
  "示例·奥利奥" 第三方主题 (示范插件格式, 供用户照抄自制)。
- **FR-15 应用图标**: SVG 主标 (桥+串口母题, 16px 可辨) 输出 全尺寸 ico/png/favicon;
  exe 内嵌 (winres), 桌面窗口/托盘 (运行中绿·重连中琥珀·已停止灰 三态变体) / 网页
  favicon 全部生效。

## 3. UI 需求 (UI)

- **UI-1 控件尺度统一 (苹果式)**: 全站控件只允许两档高 —— 标准 34px / 紧凑 28px
  (令牌 --ctl-h/--ctl-h-sm); 圆角统一 8px, 控件字号 13px, 水平内边距 12px;
  button/input/select 全部归档, QA 以像素实测断言, 禁止第三种高度。
- **UI-0 自明性 (最高优先, 用户定性)**: 任何控件不看文档即可猜对; 文案只说用户可感知的
  事情 (串口/网址/连接/网页), **禁用实现词** (重启/换绑/监听/WS/TCP/契约/帧);
  状态用徽章+颜色表达; 危险操作两段确认且预告后果用大白话。


- UI-1 中文界面, 信息密度高但不挤; 响应式: 1440 桌面到 720 窄屏无横向滚动。
- UI-2 浅色工业风 + 设计令牌集中 (`:root`), 深色模式列为 P2。
- UI-3 状态机徽章四态配色: Open=绿 / Opening=绿闪 / Retry=琥珀 / Closed=灰。
- UI-4 终端区默认 ASCII, 一键切 HEX; 时间戳可选; 最大保留 5000 行 (防内存膨胀)。
- UI-5 所有按钮可键盘触达; 禁用态明确 (未打开串口时发送区禁用)。

## 4. 兼容与平台 (PLAT)

- PLAT-1 Windows 10/11 (本机验证), Linux/macOS 编译通过 (CI 配置文件齐, 本机无法验证的注明)。
- PLAT-2 现代浏览器 (Chromium/Firefox/Safari) 的 WebSocket 二进制帧。
- PLAT-3 虚拟串口对 (ELTIMa/com0com) 与真实 USB-UART (CH340) 均须工作。
- PLAT-4 GUI 壳 (FR-8): Windows 用 WebView2 (Win11 自带); Linux 需 webkit2gtk、macOS 用
  WKWebView —— 非 Windows 平台仅要求可编译, CI 注明依赖; headless 模式无任何 GUI 依赖。

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
