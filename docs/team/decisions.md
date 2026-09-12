# 架构决策记录 (ADR)

## ADR-1 语言 = Rust (2026-09-12, 架构师)

本机 cargo/rustc 1.97.1 可用 (已验证)。备选: Go (未装)、Node/TS (运行时依赖, 单二进制弱)。
选 Rust: 跨平台静态二进制、tokio 生态成熟、serialport-rs 支持全参数 (8N2/2M 已被 wterm 证明可行)。
代价: 编译期长一点, 换分发零依赖。

## ADR-2 数据面 = 纯原始二进制 WS 帧 (2026-09-12)

调研显示 json/base64 包帧型桥 (websocket-serial-server) 迫使页面侧改协议。
SerialHub 的定位是"管道", 管道里不做语义。控制/状态走 HTTP JSON (/api/*), 与数据面彻底分离,
页面重载即重连, 桥无会话概念。

## ADR-3 多客户端仲裁 = FIFO 单写者 (2026-09-12)

串口是单用户设备。多客户端 RX 广播 (broadcast channel) + TX 单写者 mpsc 队列,
先到先发, 文档明示"不做客户端优先级"。对比: wterm 每会话各自轮询同一端口, 会抢字节 —— 明确避开。

## ADR-4 自动重开 = 后台监督任务 (2026-09-12)

serial_bridge.py 的实战教训: CH340 抖动后旧句柄永久失效, "桥活着却不转发"极难查。
SerialHub 把重开做成一等状态机 (Closed/Opening/Open/Retry) 并暴露到 /api/status,
监督任务独立于数据面, 1s 间隔, 错误留痕。

## ADR-5 Sprint1 SPEC-QUESTION 裁定 (2026-09-12, 架构师)

① POST /api/config|open|close: 成功 `{"ok":true}`, 失败 400+`{"ok":false,"error"}` —— 采 Dev 现实现, 入规格 FR-4;
② uptimeSec = 进程运行时长 —— 采 Dev 实现;
③ status 维持 9 字段, 不加 flow (契约稳定优先; UX 若强需求入 Sprint2);
④ open-while-open = no-op —— 维持, 与"热改需 close→config→open"一致。

## ADR-6 Sprint1 QA SPEC-QUESTION 裁定 + 契约归档 (2026-09-12, 架构师)

① config 字符串格式 = `{数据位}{校验}{停止位}` ("8N2"/"7E1"), QA 断言依据入档;
② 计数器参照系: rxBytes = 桥从串口**读**到并转发给 WS 的字节; txBytes = 桥从 WS 收到并**写**入串口的字节;
③ lastError 无错时为 `null` (JSON null, 非空串);
④ POST /api/config 空 body `{}` = 200 no-op (部分更新语义的自然推论), API 文档注明;
⑤ WS 消息边界 ≠ 串口帧边界 (RX 按读块广播), 页面侧协议栈须容忍任意分块 —— README 注明。
