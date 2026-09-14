# 竞品对照调研 — SerialHub 还值得附加哪些"非常实用"的功能 (2026-09-13)

作者: 竞品对照员/研究员 (505) · 任务: 只调"实用", 不实用的明确说不 · 只交报告不改代码。

调研方法: 通读 README / spec (FR-1~18) / 用户手册 / CHANGELOG v1.3.0 → 逐一翻同类工具的
issue 区 (ser2net、wterm、serial2tcp、ESP8266-SerialWebSocket、WebSerial、omnihub、
vsp-router、FUXA、pyserial tcp_serial_redirect、SerialTool、Serial Studio), 以 issue 原文
为证据, 反推真实场景。商业官网 (Eltima VSPD / Moxa NPort) 反爬 403/404, 该维度以开源生态
替代取证 (omnihub 已集成 com0com+hub4com, 功能面与商业组合一致)。

## 0. 总体判断

用户抱怨集中在四类, 与 SerialHub 现状对照:

| 抱怨主题 | 证据 | SerialHub 现状 |
|---|---|---|
| 串口断开要能自动重试 | ser2net #67 "Need a flag to force ser2net to retry to open serial devices" | ✅ 已有 (FR-3/FR-12, 且更强) |
| 数据要能**存下来/再放一遍** | omnihub 自带 Recording/Replay (JSONL); wterm 请求图数据导出 #6 | ❌ 无, tap 数据看一眼就丢 |
| 数据要能**分发给不讲 WS 的既有工具** | omnihub 做 UDP/WS/MQTT 三桥; FUXA PR #2200 "COM port sharing"; espSuite MQTT+WS+Telnet | ❌ 只出不进, 只有 WS 一种口 |
| 远端机房要**当服务跑, 出错有据可查** | ser2net #77 (service 方式跑, core dump); ser2net #48 段错误求日志 | ❌ headless 有, 服务化/日志文件无 |

结论: SerialHub 的"可靠管道+多桥管理"骨架已对 (自动重连甚至领先同类), 缺的是
**数据的后半程** (存、回放、转发) 和 **服务的前半程** (无人值守)。以下候选按此排列。

## 1. Top 3 推荐 (下一两个版本最该做)

### T1. 每桥数据录制与回放 (P1, M)

- **谁在什么场景用**: ① 固件开发者 —— 现场/客户机器上的异常串口流录下来, 回工位重放给
  页面上位机复现 bug (omnihub 把 Recording/Replay 当核心卖点: "trafic JSONL kayıt,
  replay"); ② 自动化测试 —— 从真机抓一段 Modbus/自定义协议流, 回放成 CI 里的虚拟设备,
  SerialHub 自己的 pytest 套件直接受益; ③ 现场支持 —— 用户把 .jsonl 发回来, 远程看流。
- **证据**: omnihub `recording.rs`/`replay.rs` (Tauri+Rust 同栈, 格式可直接对标:
  JSON Lines, 每行 {ts, dir, hex}); wterm #5/#6 请求保留/下载曲线数据 (同类用户对
  "数据要能带走"的需求旁证); 商业 Electronic Team Serial Port Monitor 的付费点就是日志。
- **为什么 SerialHub 做有优势**: tap 通道 (FR-10g) 已存在, 录制=在桥内加一个 tee 任务
  落盘; 广播架构天然支持"边录边放"。管理台一个录制按钮+文件列表, UI 成本极低。
- **工作量**: M (录制 S; 回放含倍速/单发约 M; 契约 +2 字段 recording/replaying)。
- **注意**: 回放目标 = 桥的 WS 端 (模拟设备), 不是写串口 (避免与真实设备互踩), 文档讲清。

### T2. 每桥旁路转发到 TCP / MQTT (P1, M)

- **谁在什么场景用**: ① 工控集成商 —— 手里现成的串口助手/Modbus 轮询工具/Home Assistant
  /Node-RED 全讲 TCP 或 MQTT, 不讲 WS; 有了旁路出口, 既有工具零改造接入; ② "一个 COM
  只能被一个程序打开"是 Windows 生态第一抱怨类 (hub4com/mux_serial 因之而生; FUXA
  PR #2200 标题就是 "implement COM port sharing"; Z2M/ZHA 双工具抢适配器同因),
  SerialHub 变成"串口分发中枢"后此痛点被顺带解决。
- **证据**: omnihub "pure-Rust UDP/WS/MQTT bridges"; espSuite "supports MQTT,
  WebSockets, Telnet"; Serial Studio (3.3k★) 官方描述 "Supports UART, BLE, MQTT,
  Modbus" —— 汇聚类工具标配多协议出口, WS-only 是少数派。
- **为什么 SerialHub 做有优势**: 广播扇出已就绪, TCP/MQTT 出口就是两个特殊"常驻客户端"
  (sink), 实现薄; TCP server 模式即 ser2net 的 raw 模式, 可与其生态互通 (RFC2217 可后置)。
- **工作量**: M (TCP sink 小, MQTT 用 rumqttc; 每桥配置 +sinks 数组, UI 一行一个出口)。

### T3. 服务化 (Windows 服务) + 运行日志文件 (P1, M+S)

- **谁在什么场景用**: 工控远程运维 —— 桥宿主机在远端机柜/机房, 无人在旁双击 exe;
  需要开机自动起桥、崩溃自动拉起、出事后有日志可查。ser2net #77/#48 的抱怨全是
  "当服务跑时不稳且无日志", 反证这两件事是运维刚需。
- **证据**: ser2net 以 systemd 服务发行 (其 issue 区大量服务态故障报告); omnihub
  "Auto-start / Auto-restart: route bazında"; 手册 FAQ 已支持 0.0.0.0 远程访问, 但
  "无人值守"缺最后一块。
- **为什么 SerialHub 做有优势**: --headless 已存在, 服务化=包一层 (windows-service crate
  + `serialhub service install/uninstall` 子命令); fleet.json 自动恢复 (FR-10b) 恰是
  服务态最需要的语义, 骨架现成。
- **工作量**: M (服务安装/卸载/事件写入 Windows 事件日志) + S (桥/管理台运行日志按天
  滚动落盘 + 管理台一键导出诊断包)。

## 2. 其余候选清单 (P2 为主)

| # | 功能 | 谁在什么场景用 (证据) | SerialHub 优势 | 工作量 | 优先级 |
|---|---|---|---|---|---|
| C1 | **DTR/RTS 控制线手动控制+打开时初始电平可配** | 嵌入式开发者: ESP32/Arduino 进 bootloader 靠 DTR/RTS 电平; ser2net #46 (CTS/DTR) #128 (RTS 高不释放) 两案都是控制线误伤 | serialport-rs 已暴露 API; 每桥加两个开关+初始电平下拉即可; 解决"桥一打开设备就进了怪状态" | S | P2 |
| C2 | **旁看抽屉行模式分帧显示 (按分隔符/超时分行)** | 看流的可读性; wterm #3 "Specify what consider to be a new line"; SerialTool 默认按行 | tap 已有, 加"分帧: none/\n\n/超时"下拉; 对帧式协议 (NMEA/AT) 可读性质变 | S | P2 |
| C3 | **mDNS/Zeroconf 广播管理台** | 多机/手机/平板接入先要找 IP; ser2net #55 mdns 配置困难遭抱怨 (反向证明需求真实存在) | axum 服务已有, 加 `_serialhub._tcp` 公告; 管理台页脚显示二维码扫码直达 | M | P2 |
| C4 | **最小访问令牌 (单一 --token, WS query + /api header)** | 手册 FAQ 已教 0.0.0.0 暴露, 但无鉴权全裸; WebSerial #126 "HTTPS/WSS Autodetection" 是浏览器场景对安全的头号 issue | 单文件工具做最小令牌即可解锁内网远程用; 完整 TLS 仍不做 (见"不做"清单) | M | P2 |
| C5 | **桥分组/标签 + 批量启停** | 10+ 桥的机柜场景 (T3 的伴生需求); omnihub route 分组 auto-start | fleet.json 加 tag 字段, 列表按组折叠 | S | P2 |
| C6 | **fleet 配置导入导出/单桥复制** | 部署第二台同款网关; 支持时让用户"把桥配置发我" | fleet.json 已是文件, 导出=下载; "复制桥"按钮省重填 | S | P2 |
| C7 | **链路嗅探预设 (串口⇄串口, 桥在中间记录)** | 协议逆向: 在设备与原上位机之间窃听 (Eltima Serial Port Monitor 收费在做; 开源 LemonSerialMonitor 专做只读嗅探) | 两座桥 + 内部中继 + T1 录制, 一次勾选自动建全链路 | L | P2 |
| C8 | **安装器 (Windows MSI/NSIS, 可选装服务)** | zip 解压+手动建快捷方式是采用摩擦; com0com 全靠安装器分发 | CI 已产三平台包, 加打包含步骤 | M | P2 |
| C9 | **桥抽屉允许发送 (HEX/文本小按钮, 标注"调试用")** | 现场让继电器吸合/发一条 AT 试探, 免写脚本; wterm/串口助手用户均预期 | 发送走既有 TX FIFO, 实现极小; **但与 FR-10e"只看不发"的既定产品决策冲突, 需团队裁定** (可做成设置里的开关, 默认关) | S | P2 (需裁定) |
| C10 | **诊断日志导出** (并入 T3, 单列备查) | 远程支持的第一句永远是"把日志给我" | ser2net #48 段错误无日志之痛 | S | P1 (随 T3) |

## 3. 明确"不做的"清单 (听上去炫, 实际不实用)

| 不做 | 为什么 |
|---|---|
| **内置协议解析/Modbus 解码器** | 违背立身之本 (spec §0: node-modbus-ws 暴露 JSON 被否, 原始字节是产品差异点); 协议长尾无尽头 (Modbus 变体/私有协议), 维护是深坑; SerialTool/Serial Studio/omnihub 已在做, 不缺。回放文件用开放格式 (JSONL), 想解析的人喂给现成工具 |
| **内置波形绘图/仪表盘** | wterm #5/#6 有此请求但它是终端的错位补偿; 可视化上位机/Serial Studio 已成熟, 桥做绘图=第二个半吊子仪表盘, 挤占 T1~T3 的价值 |
| **串口终端仿真 (VT100/交互输入行)** | 手册"不是什么"已明示; 生态有 xterm.js 等成熟方案; 管理台职责是管理与旁看, 不是又一个串口助手 |
| **内置虚拟串口驱动 (com0com 式)** | 驱动签名/Windows 更新报废/兼容性地狱 (com0com 维护史即证据), 维护成本远超收益; 用户已有 com0com/ELTIMa 时给文档组合食谱即可 |
| **脚本钩子 (Rhai/Lua on_rx/on_tx, omnihub 已做)** | 听着强, 实际把工具变成编程平台, 每个用户脚本都是你的支持负担; 数据变换应由客户端程序/Node-RED 承担; SerialHub 的差异化是"零配置可靠管道" |
| **Wi-Fi/蓝牙串口接入 (BLE UART)** | spec 非目标不变; BLE UART 各厂商协议不统一, 故障率与修复成本高; 需求频次远低于 USB 串口 |
| **完整 TLS/用户体系/证书管理** | 本机+内网定位, 单文件工具背不动证书生命周期管理; C4 最小令牌已够内网用, 要公网就走 SSH 隧道 (手册已写) |
| **数据加密脱敏/字段级统计/AI 日志分析** | 炫技词, 无一条真实 issue 支撑; 前面 T1 的录制文件已覆盖分析素材需求 |

## 4. 给排期的取舍建议

- T1 与 T3 不冲突可并行; T2 若团队担心"WS 定位被稀释", 可收窄为"每桥一个 TCP sink",
  MQTT 放下一版 —— 仅 TCP 已能接住 FUXA 式 COM-sharing 场景的大头。
- C9 (抽屉发送) 动的是既有产品决策, 动手前先过一次 ADR, 避免 UX-0"自明性"红线
  (发送区必须显式标注影响真实设备)。
- 所有新契约字段 (recording/sinks/tag) 记得沿用 ADR 惯例: status/fleet 字段数变更
  走 ADR 修订, pytest 契约测试同步。
