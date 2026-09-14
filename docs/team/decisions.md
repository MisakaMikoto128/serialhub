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

## ADR-7 GUI 壳 = wry + tao + tray-icon, headless 分叉 (2026-09-12, 架构师)

用户下令"客户端要有界面且可退到后台"。选型: wry (WebView2/WKWebView/webkit2gtk) + tao 窗口 +
tray-icon 托盘, 内嵌**同一个** ui/index.html —— 不养第二套界面。tokio 服务在后台线程,
事件循环在主线程, 经 channel 打通。默认 GUI, `--headless` 保住自动化测试与脚本场景
(QA 套件随之改夹具, 属 QA 文件所有权)。关窗=隐藏到托盘, 真退出只在托盘菜单 —— 桥是常驻服务语义。

## ADR-8 退出路径冗余 + /api/shutdown (2026-09-12, 架构师)

UX Sprint2 发现托盘菜单在自动化注入下不可达 (真人是否可达待人工复核)。裁定: 优雅退出**不允许只有一条路** ——
① 托盘菜单「退出」; ② 控制台页内「退出程序」按钮 → 新契约端点 `POST /api/shutdown`
(与托盘退出同一停机序列); ③ 兜底任务管理器。GUI bind 失败改为 MessageBox 明示 (双击用户可见)。

## ADR-9 配置对等与契约修订 (2026-09-12, 架构师)

① status 契约 9→10 字段: 新增 `maxClients` (FR-9b), 修订 ADR-5① —— QA 字段集断言同步;
② GUI 改 addr = 自我重启 (spawn 同 exe + 优雅退旧, 新实例 bind 重试 ≤2s 平滑交接);
   headless 无壳不自起, UI 明示需手动重启;
③ WS 超限拒连用 close code 1013 (Try Again Later);
④ /ws 路径维持固定 /ws 不做配置项 (契约稳定, 现实无人要改); i18n 英文界面入 Sprint 候选池。

## ADR-11 flow 回显入契约 (2026-09-12, 架构师)

Sprint1 曾裁"status 不加 flow" (契约稳定优先); Sprint3 双入口走查实证: 无回显时 UI 一次普通
开关就把 CLI 配的 xonxoff 静默降级为 none —— 对等性破缺比契约膨胀更伤。裁定: status 契约
10→11 字段 (新增 flow), UI 流控以服务端回显为准, 打开动作显式携带表单 flow。本条同时修正
ADR-5① 的"恰好 9 字段"表述 (现行为 11 字段: 原始 9 + maxClients + flow)。

## ADR-12 (推翻 ADR-9② 的自我重启路线) 无重启换址 (2026-09-12, 架构师)

用户质询: "改个桥接还要重启, 监测部门没发现这么严重的问题吗?" 复盘: ADR-9② 把"实现麻烦
(axum 不能运行时换绑端口)"当成了"产品合理"。改串口参数从来不需要重启; 改监听地址也**不该要**。

裁定: FR-9a 改为**原地换绑** —— axum Listener 关闭 → 串口数据面/状态/托盘**全程不动** →
新地址 bind → TCP 面换新。现连客户端按 TCP 语义断开 (客户端自动重连到新地址), 但:
串口配置、终端历史、托盘、进程、状态机全部保留, 断档 <1s, 无窗口重建。
POST /api/restart 保留兼容 (内部转调原地换绑, 不再 spawn 新进程); UI 文案从"应用并重启"
改为"应用地址" (两段确认保留, 明示"现有连接将断开, 重连新地址")。
技术注: tokio 可 drop 旧 Server/Listener 后重新 bind + serve; 串口线程与广播通道不在
axum 任务树内, 天然不受影响 —— 之前选择 spawn 重启是偷懒, 不是约束。

### ADR-12 实现记录 (架构师亲修, 2026-09-12)
service.rs serve 循环化: rebind 槽 (每轮新建) 触发 serve 任务第二出口 → 主 select 判定换址 →
重新 bind (失败 2s 重试后回退原地址继续服务) → continue 换下一轮; hub/串口会话/广播/托盘
全程不动。Ctrl-C 独立任务汇入 watch (与 /api/shutdown 同路), serve 闭包只听 watch。
/api/restart 门控取消 (headless 同样受理); App.gui/Startup.gui 字段随之删除。
黑盒实证: 8097→8098 换址, 旧地址收 18 帧→新地址 0.51s 接管续收 18 帧, phase/port 保持,
0 残留进程。UI 文案: 「应用并重启」→「应用地址」, 预告改为"串口不断, 现有连接需重连"。

## ADR-13 产品转向: 串口助手 → 桥接管理器 (2026-09-12, 架构师; 用户定性)

用户: "这软件怎么看都像串口助手而不是桥接器, 完全无法管理桥接, 没有可视化状态/统计"。
裁定全面转向 **多桥管理器**:
① 控制面/数据面分离 —— 管理台固定一个地址永可达; 每座桥独立数据端口 (串口⇄端口)。
② 多桥并存: 列表新建/启停/改配/删除, 配置持久化 fleet.json, 进程重启自动恢复全部桥。
③ 每桥可视化: 流程图 (串口⇄桥⇄客户端) + 状态徽章 + RX/TX 速率火花线 + 总量/错误/时长。
④ 终端降级为每桥「侦听」抽屉 (旁看串口原始流), 不再占据主界面。
⑤ 端点稳定: 桥的 ws 端口在桥生命周期内不变; 串口掉线自动重开 (已有) + listener 异常自动重建。
⑥ CLI 兼容: 单桥参数等价于"建一座桥"; --fleet fleet.json 批量恢复。
⑦ UI-0 自明性原则入规格: 文案只说用户可感知的事 (串口/网址/连接/网页), 禁实现词
   (重启/换绑/监听/WS/TCP/契约); 换地址页面自动跟随, 零文档上手。

## ADR-14 Sprint4 波1 跨队裁决 (2026-09-12, 架构师)

① 文案五分歧全采 UX 版: 管理台网址 / 拒连长版(含上限值+两个动作) / 统计卡「已连接」/
   徽章四态词表(运行中/连接中/重连中/已停止) / 页签「串口数据」。
② SQ-UI-1: /api/fleet 与 /api/fleet/<id> 桥对象**必须回显 maxClients** (FIX-17 同类风险:
   无回显则 UI 保存即静默清零) —— 后端补齐 + 单测。
③ SQ-UI-2: POST/PATCH /api/fleet/<id>/config 受理 create 形状 body (+maxClients), 与 UI 对齐。
④ FR-10e 只读裁定: 侦听页签**只看不发**维持 v1 —— 人为发数据属客户端程序职责,
   管理台定位是"管理桥"不是"操作串口"; spec §1 场景措辞同步修订; 如未来需要再立 FR。
⑤ 波1 五处文案分歧词已裁, UI 端一词切换即可, 波2 复验时核对。

## ADR-15 串口热拔插自动重连产品化 (2026-09-12, 架构师; Sprint 5)

用户定版需求: "用户不小心串口断掉, 需要自动重连"。监督任务 1s 重试骨架已在 (FR-3),
产品化补三块:
① 契约 +`retries` (u32): 当次会话内重试计数 —— retry 态每次失败 +1, 成功打开归 0。
   单桥 /api/status 与 /api/fleet 桥对象同步 (11→12 / 13→14 字段, QA 契约测试随动)。
② UI 大白话: retry 态卡片明示「串口已断开，正在自动重连 (第 n 次)…」; 恢复瞬间页内
   横幅「串口已恢复」约 3s (不弹窗); 火花线断档自然可见, 不做伪装。
③ 演练背书: COM99 缺席→retries 递增 (黑盒); 恢复路径单测注入假打开器 (已有);
   QA 调查 ELTIMA 是否有 CLI 可程序化"拔插"虚拟对 —— 有则补金测试, 无则出人工演练指南。
④ 客户端零动作: 串口断/恢复期间 WS 客户端连接保持, 恢复后数据自动续传 —— 必须有测试断言。

## ADR-16 自动重连可选项 + 控件尺度统一 (2026-09-12, 架构师; Sprint 6 / v1.2.0)

用户两点: ①自动重连应可自由选择 (有时不需要); ②按钮/输入框高度参差, 缺高级感 —— 按苹果
设计哲学统一。

① FR-12 每桥 `autoReconnect` (默认 true): CLI --reconnect/--no-reconnect + 建桥/改配 body
   + fleet 桥对象与单桥 status 回显。false 时串口断开 → 直接 phase=closed (lastError 注明
   "自动重连已关闭"), 不进重试循环; 手动打开仍可用。契约字段数 12→13 / 14→15。
② UI-1 控件尺度令牌 (苹果式): 控件只允许两档高 —— 标准 34px / 紧凑 28px
   (--ctl-h/--ctl-h-sm), 圆角 8px 统一 (--ctl-radius), 控件字号 13px, 水平内边距 12px;
   全站 button/input/select 逐一归入两档, QA 以 Playwright 实测像素断言 (不许"差不多")。

## ADR-17 开源发布 (2026-09-12, 架构师)

① 许可证 = **Apache-2.0**: 商业友好 (允许商用/闭源分发) + 显式专利授予 + NOTICE 机制,
   比 MIT 多一层专利保护, 是"商业开源"的标准选择; 版权人 = GitHub 账号主体。
② 公开范围: 全仓库公开 (含 docs/team 多智能体开发记录 —— 它是本项目方法论的一部分);
   密钥/凭据不存在的先决条件已核对 (无 .env/token 入库)。
③ 社区文件: CONTRIBUTING (多智能体回路+传统贡献双轨) / CODE_OF_CONDUCT (Contributor
   Covenant) / SECURITY (如实注明 v1.x 无 TLS, 勿暴露不可信网络) / CI 三平台。
④ 用户手册 docs/manual/ 与 README 双语 (中文为主, 英文节选), 截图统一 docs/images/。

## ADR-18 Sprint 7 (2026-09-12, 架构师; 用户四需求)

① FR-13 管理台设置: 控制面端口运行时可改 (原地换绑复用 ADR-12 机制, fleet.json [manager]
   持久化, 壳 webview 与浏览器页自动跟随); 页头「打开面板」按钮 (壳→默认浏览器, 页→新标签)。
② FR-14 主题=文件夹插件: themes/*.css 只覆盖 :root 设计令牌; GET /api/themes 扫描 +
   /themes/<file> 静态服务; UI 动态切换无刷新, 选择存浏览器本地; 内置 浅色/深色/示例第三方
   主题 (示范插件格式); --themes-dir 可指定。
③ FR-15 图标: 新 SVG 主标 (桥+串口母题) 全尺寸资产 (ico/png/favicon), exe 内嵌 (winres) +
   窗口/托盘三态变体 + 网页 favicon。
④ 最小接入 demo: examples/web-client.html (零依赖) + examples/python-client.py —— 原始字节
   读写示范, 值语义属客户端协议 (手册教程节引用)。
⑤ UX 审计 (105): 改动落定后全站美感复审, P1 当轮修。

## ADR-19 Sprint 8 (2026-09-14, 架构师; 用户四点)

① FR-16 单实例友好处理: GUI 第二实例 bind 失败时, 先探测目标端口是否为另一 SerialHub
   (GET /api/status 响应形状判别) —— 是 → 信息提示框 (非错误): "SerialHub 已在运行,
   管理台: <网址>", 确定后自动在浏览器打开该管理台, exit 0; 否则维持现有错误框。
② FR-17 新建桥端口自动递增: 管理台新建桥表单预填下一个空闲数据端口
   (管理台端口+1 起向上探测, 跳过已用), 用户仍可改 —— 少一次输入。
③ 发布版 GUI 无控制台窗口: main.rs `#![cfg_attr(all(windows, not(debug_assertions)),
   windows_subsystem = "windows")]` —— release GUI 无黑窗 (调试版保留控制台);
   headless release 的 stdout 随之不可见, 属既定取舍 (文档注明)。
④ 实用功能调研: 竞品对照席 (505) 专项报告, 只列真实用的, 不堆功能。
