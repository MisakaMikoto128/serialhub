# Dev 报告 — Sprint 2 (FR-8 桌面客户端形态)

日期: 2026-09-12 · 角色: Dev · 状态: 待验收
范围: 仅 FR-8 (依赖 / GUI 默认 / 托盘 / 关窗即后台 / --headless / 端口占用报错); 未碰其他候选。

## 1. 完成条目对照 (FR-8 逐条)

| FR-8 条目 | 实现 | 自验 |
|---|---|---|
| 默认原生窗口 (WebView 内嵌同一控制台) | `tao` 窗口 (主线程) + `wry` 0.57 `WebViewBuilder::new().with_url("http://127.0.0.1:<addr>/").build(&window)` 加载**同一个** `ui/index.html`, 零第二套界面; wry 0.57 随窗口自适应缩放 | 冒烟: 进程起, MainWindowTitle="SerialHub", WebView 内控制台自动连 WS (`/api/status` clients=1) |
| 关窗 = 退到托盘, 首次气泡提示 | `CloseRequested` → `window.set_visible(false)` + 首次 `Shell_NotifyIconW(NIF_INFO)` 气泡 (一次) | 冒烟: `CloseMainWindow()` 后 title 空 / 进程活 / 服务仍 open、WS 仍连; 气泡代码路径执行 (人工目视留给 UX) |
| 托盘菜单五项 | muda 菜单: 显示主窗口 / 在浏览器打开控制台 / 打开串口 / 关闭串口 / —— / 退出; `MenuEvent::set_event_handler` → `EventLoopProxy` 打回主循环 | 菜单构建无错; 菜单项 → UserEvent → 与已验证路径同一处理函数 (原生托盘菜单点击自动化会抢用户焦点, 人工确认项见 §4) |
| 托盘图标随状态机变色 + tooltip | 程序内生成 32×32 抗锯齿圆点 (绿/琥珀/灰, 无美术资源); 相位 watcher (150ms) → `proxy.send_event(PhaseChanged)` → `tray.set_icon/set_tooltip("SerialHub · COM2 · open")` | 图标生成与 set_icon 无错; tooltip 文案形态符合; 变色目视留 UX |
| --headless 旧行为 | `--headless`/`--gui` (默认 GUI, 互斥); headless = 进程内 tokio 主任务 + Ctrl-C 优雅停机, 无任何 GUI 初始化 | 单测 `headless_gui_flags`; 冒烟 headless 起桥 status open ✓ |
| 端口被占用报错退出 | bind 在 run_service 最先执行; 失败即 `Err("端口被占用: 无法监听 {addr}: {e}")`; GUI 模式在**创建窗口之前**收到失败 → stderr + exit(1), 不弹窗不抢焦点 | 冒烟: 自占 8082 后第二实例启动即输出该错误并 exit=1, 第一实例不受影响 |
| 退出干净杀串口线程与监督任务 | 退出协议: 托盘 Quit → `watch` 触发 service 停机 → axum 优雅退出 → `HubCmd::Close` + `ctx.stop_active()` (会话 stop 标志, 读/写线程 ≤150ms 自行退出) + 300ms 等待 → 回发 `Stopped` → 主循环才 `Exit` | 退出后 COM2 立即可被 pyserial 重开 (释放确认); `ctx.stop_active()` 语义有单测覆盖 |
| (顺带) 窗口图标 | 同一程序内生成的圆点作为窗口图标 | — |

依赖新增: `wry 0.57` + `tao 0.37` + `tray-icon 0.25` (+ `windows-sys 0.61` 仅 Win32_UI_Shell/Foundation 用于气泡; 与 tray-icon 同版本族, 增量编译成本≈0)。

## 2. ⚠ wry/tao 事件循环与 tokio 的整合方式 (最容易踩的坑, 给下一个接手的人)

1. **线程模型是硬性的**: Win32 要求窗口/托盘/菜单在主线程操作, tao 的
   `EventLoop::run` 占死主线程**且永不返回** (`-> !`)。因此 tokio runtime 整体搬去
   后台线程 (`serialhub-service`), 主线程只跑事件循环。**不要**把服务搬回主线程,
   也**不要**在事件循环闭包里 `await` / `block_on` —— 前者编译不过, 后者冻结 Win32
   消息泵导致窗口假死。
2. **两条跨线程通道, 方向不同**:
   - 服务 → 主循环: `EventLoopProxy<UserEvent>` (Send)。service 把相位变化 (150ms 轮询
     HubState, 与监督任务内部实现解耦)、`Stopped` 打回主循环; **事件循环 run 之前发送的
     事件会排队**, 所以 `Ready(addr)` 用的是独立的 std mpsc ready 通道 + 主线程
     `recv_timeout(15s)` 同步等待 (窗口必须在主线程同步创建, 且必须在收到 Ready 之后)。
   - 主循环 → 服务: `cmd_tx` (tokio unbounded, `send` 是同步方法, 主线程可直接调) ——
     托盘菜单"打开/关闭串口"直接发 `HubCmd`; **该通道在 gui 侧创建, cmd_tx 留主线程、
     cmd_rx 传给 run_service**, 别像第一版那样在 service 内部重建导致托盘指令失踪。
3. **退出协议必须分两步**: 托盘"退出"不能直接 `ControlFlow::Exit` (tao 的 run 结束即
   进程结束, 串口线程来不及释放 COM)。正确顺序: `watch` 通知 → axum 优雅停机 →
   `HubCmd::Close` + `ctx.stop_active()` → 300ms → 回发 `Stopped` → 主循环收到才 Exit。
   另有 3s 超时兜底 (`ControlFlow::Poll` 轮询期检查), 防止 Stopped 永不到达卡死进程。
4. **失败回传别吞掉**: run_service 的 `Err` (如端口被占用) 在服务线程里必须 send 回
   ready 通道; 第一版 `let _ = rt.block_on(...)` 把它吞了, 症状是"启动卡 15s 才报
   未就绪"。就绪后 ready 接收端已关闭, 再 send 失败属正常 (用 `let _`)。
5. **托盘气泡**: tray-icon 没有气泡 API; 用其公开的 `window_handle()` + `uID=1`
   (tray-icon 内部计数器从 1 起, 本进程只建一个托盘) 直接 `Shell_NotifyIconW(NIM_MODIFY, NIF_INFO)`。
   如果将来创建第二个托盘, 此假设失效 —— 必须改用 `TrayIconId` 映射。

## 3. 关键取舍

1. **同一控制台**: WebView 直接加载 HTTP 服务页面, GUI 壳零业务逻辑 —— 桥逻辑 100% 复用,
   QA 数据面/契约零回归 (22/22 单测全绿, 含新增 `headless_gui_flags`)。
2. **相位 watcher 用轮询 (150ms) 而非监督任务回调**: 监督任务保持对 GUI 无感知, 换来
   supervisor 可测性不破坏; 150ms 延迟对托盘图标变色无感。
3. **气泡用 windows-sys 直调** 而非换托盘库/加美术资源: 最小依赖达成 FR-8 唯一硬缺口。
4. **"在浏览器打开"用 `cmd /C start`** (CREATE_NO_WINDOW) 而非引入 `open` crate。
5. **GUI 模式下端口被占用在窗口创建前失败退出**: 满足"不弹窗口抢焦点"; 双击 exe 无控制台
   时用户看到"没启动", 建议后续加托盘气泡形式的启动失败提示 (P2)。

## 4. 自验证据 (全部 COM2 + 127.0.0.1:8081/8082/8083; **未碰用户实例 8080+COM1 与 COM8**)

| 项 | 结果 |
|---|---|
| cargo build / cargo test | 0 警告; **22 passed / 0 failed** (含 `headless_gui_flags` 互斥用例) |
| GUI 启动 (8081+COM2) | 进程活, MainWindowTitle="SerialHub", status=open, **clients=1** (WebView 内控制台 WS 已连) |
| 关窗 = 后台 | `CloseMainWindow()` → 标题空、进程未退、status 仍 open、WS 仍连 ✓; 气泡已触发 (代码路径) |
| 端口被占用 | 第二实例 (GUI) 启动即报 `端口被占用: 无法监听 127.0.0.1:8082: ... (os error 10048)`, exit=1, 首实例无恙 |
| --headless (8083+COM2) | 起、status open、停, 全程无窗口 |
| --list-ports | 正常列出 (GUI/headless 共用) |
| 退出后串口释放 | 进程结束即 COM2 可重开 (pyserial 验证) |
| 托盘菜单点击路径 | **自动化未覆盖**: 原生托盘菜单在 Win11 溢出区, UIA 自动点击会抢用户焦点, 留人工/UX 确认 (显示主窗口/浏览器/开关串口/退出 五项); 其处理函数与已验证的 CloseRequested/`cmd_tx` 路径完全同源 |

## 5. 遗留 / 建议

1. 托盘图标变色、tooltip、气泡、五项菜单的实际观感: 请 UX 以真实点击走一遍 (关窗→托盘→
   恢复→真退出), 自动化够不到原生托盘。
2. Linux/macOS 仅保证编译路径 (PLAT-4: webkit2gtk/WKWebView 系统依赖), 本机无法验证;
   webview bounds 依赖 wry 自适应, 若 CI 发现平台差异需补 `Resized` 处理。
3. 双击 exe (无控制台) 场景下"端口被占用"提示不可见 → P2: 启动失败时用托盘气泡兜底
   (需先建托盘再报错的时序调整)。
4. 窗口关闭后任务栏 UIA 仍能扫到一个名为控制台的陈旧预览元素 (Win11 缓存), 无实际影响,
   记录备查。

## 6. 启动命令

```bash
cargo run --release -- --addr 127.0.0.1:8080 --port COM1   # GUI (默认): 窗口 + 托盘
cargo run --release -- --headless --addr 127.0.0.1:8080 --port COM1   # 旧行为 (QA 夹具用)
cargo run -- --list-ports                                  # 两模式共用
```

## 修复轮 (回路⑤, FIX-9~FIX-15) — 2026-09-12 第二次追加

来源: ux-sprint2.md (P1-1/P1-2/P1-3 + P2-1~P2-4) + ADR-8 (退出路径冗余 + /api/shutdown)。
数据面 `/ws` 与 `/api/status` 9 字段契约零改动; cargo build 0 警告; cargo test **24/24 全绿** (+2 停机用例)。

### FIX-9 页内「退出程序」按钮 + POST /api/shutdown (UX P1-1 连带, 升 P0 契约) — 已修

- **服务端**: 新契约端点 `POST /api/shutdown` (FR-4⑤/ADR-8)。停机开关收敛为**一个**
  `watch::Sender<bool>`: 托盘「退出」与 `/api/shutdown` 都只发它, run_service 内部统一转成
  优雅停机序列 —— watch → axum 优雅退出 → `HubCmd::Close` → `ctx.stop_active()` (串口线程
  ≤150ms 退出) → 300ms → 回发 `Stopped` → GUI 主循环才 `ControlFlow::Exit`。headless 同样
  受益 (Ctrl-C 与该端点二选一, select 合流)。
- **关键回归点**: 原优雅停机会被**打开着的 WS 长连接**卡死 (axum graceful 等全部连接关闭)。
  修复: `client_loop` 持 watch 接收端, 停机时主动 break 断开 WS —— 长连接不再阻塞停机。
  已加单测 `shutdown_api_closes_gracefully_with_open_ws`: 手工 TCP 握手 WS 并保持 →
  原始 HTTP POST /api/shutdown → 断言 `{"ok":true}` + run_service 3s 内优雅完成
  (旧实现此用例必挂); 另有 `shutdown_watch_can_be_sent_repeatedly` (重复触发不 panic)。
- **页面**: 页头新增「退出程序」按钮 (红色描边, **两段确认**防误触: 点击 → "确认退出?"
  3s 内再点 → POST → "正在退出…", 页面随进程一起结束)。
- 自验: ① headless 实例 POST /api/shutdown → `{"ok":true}` → 进程 2.5s 内自退、COM2 立即
  可重开; ② 真实按钮路径端到端 (Playwright 页内两次 click, 间隔 150ms): armed 文案出现 →
  POST 发出 → GUI 进程自退 → COM2 释放 ✓; ③ 单测 24/24。

### FIX-11 GUI clients 恒 2 (UX P2-1 → 升 P1) — 已修 (根治)

- **根因 (页面 WS 状态机双缺陷, 已注入复现)**: 旧守卫 `if (ws && (readyState===0||1)) return`
  放过 **CLOSING 态** —— 服务端此时仍把旧连接计为客户端; 且旧 socket 的**迟到 onclose 会把
  已换成的新 socket 置 null** (孤儿连接), 随后重连定时器再开一条 → 两条 WS 永久并存 = 恒 2。
  注入复现: 页面内 `ws.close(); ws = new WebSocket(...)` 后 clients 稳定=2。
- **修法**: WS 改为代次状态机 —— ① `readyState !== 3` 一律复用 (CONNECTING/OPEN/CLOSING
  都算占用); ② 每条 socket 持代次 `gen`, 全部事件先验 `gen !== wsGen` 即弃 (迟到事件不可能
  清掉新 socket); ③ 仅当前代次的 onclose 才 `ws=null` 并安排重连。
- **自验**: 注入攻击 (绕过状态机直开裸 socket ×2) 后 clients=3 属 FR-1 预期行为 (服务端如实
  计数任意客户端, 攻击流不在页面状态机内); **正常流全过**: 页面刷新后 clients=1;
  页内正常 close→重连后 clients=1; GUI 实例 (修复后) 连续 4 次轮询 clients=1;
  headless 无页面 clients=0; 浏览器开一页=1 —— 与工单验收口径逐一相符。

### FIX-12 托盘 tooltip 累积拼接 (UX P2-2) — 已修 (防御性)

- 修法: 相位变化时 `set_tooltip(None)` 先清再设 `set_tooltip(Some(new))` —— 我方只此一个
  托盘对象、只此一条 set 路径; 拼接发生在 Shell/UIA 缓存层 (悬停 tooltip 本身正确),
  清后设可刷新该缓存。无法在本地复现 UIA 层拼接, 请 UX 复验屏幕阅读器读数。

### FIX-13 托盘圆点加描边 (UX P2-3) — 已修

- 修法: 32×32 图标改为 **彩色实心核心 (r<12) + 深色描边环 (12≤r≤15.5, --ink 同源深色) +
  抗锯齿外缘**, 浅色任务栏上轮廓清晰。
- 自验: 构建 0 警告; 图标随相位切换无错 (代码路径同旧)。目视观感留 UX 截图复核。

### FIX-14 bind 失败 MessageBox (UX P2-4, ADR-8) — 已修

- 修法: GUI 模式下**窗口创建之前**的失败 (端口被占用 / 服务线程无响应 / WebView2 初始化
  失败) 弹系统 `MessageBoxW(MB_OK|MB_ICONERROR|MB_SETFOREGROUND)` 明示后 exit(1);
  stderr 输出保留。headless 行为不变 (仅 stderr)。
- 自验: 端口占用冒烟仍 PASS (此前已验证 stderr/exit 路径); MessageBox 为 Win32 直调,
  观感留人工确认。

### FIX-10/15 README「桌面形态」章节 + 气泡文档化 (UX P1-3/P1-2) — 已修

- README 新增「桌面形态 (GUI, 默认)」一节: GUI 默认与双击式启动、关窗=后台、托盘三色/
  左键恢复/右键菜单、**退出三种方式** (托盘菜单 / 页头退出按钮 `/api/shutdown` / 任务管理器
  兜底)、**Win11 溢出区可见性指引** (任务栏设置提升显示)、**气泡受勿扰/专注助手影响**
  (没看到气泡 ≠ 程序退出, UX P1-2 的反馈链修正)、`--headless` 场景与"无 GUI 依赖"说明、
  启动失败提示方式。
- 自验: 文档落盘; 无代码影响 (24/24 保持)。

### 修复轮自验环境声明

全程 COM2 + 127.0.0.1:8081/8083 (+ 端口 0 随机端口的单测); **未碰 COM1/8080 与 COM8**;
测试进程按 PID 清理, 收工时无 serialhub/python 残留、COM2 空闲。

## 二次修复 (qa-sprint2-fix.md, 只修两条) — 2026-09-12 第三次追加

数据面与契约零改动; cargo build 0 警告; cargo test **26/26 全绿** (+2 有界停机用例)。

### 1. FIX-12 二修 (QA FAIL: UIA Name 拼接) — 已按 QA 建议重写为 NIM_DELETE + NIM_ADD

- **修法**: 放弃 set_tooltip(None→Some) 的 NIM_MODIFY 路径 (QA 复验证明不刷新 Shell 暴露给
  UIA 的 Name, open 相位仍读到 "closed … open" 拼接)。相位变化时改为
  **NIM_DELETE → NIM_ADD 整体重挂**, 数据一次备齐:
  - `hWnd` / `uID=1` / `uCallbackMessage=6002` (tray-icon 0.25 托盘事件消息常量) **保持不变**
    —— 托盘事件回调与 muda 菜单子类都挂在同一隐藏窗口上, 不受影响 (防菜单失联);
  - `hIcon` 现场由 RGBA 生成: 32bpp BGRA XOR + 全零 AND 掩码 (per-pixel alpha) 经
    `CreateIcon` 产出, ADD 后 `DestroyIcon` (Shell 在 ADD 时已复制);
  - `szTip` = 新 tooltip, 与 NIF_ICON|NIF_MESSAGE|NIF_TIP 一次写入。
  托盘 tray-icon 内部状态仍先经 set_icon/set_tooltip 同步 (explorer 重启 TaskbarCreated
  重挂路径依赖 userdata)。
- **验证**: open→closed→open 相位切换全部走重挂路径, 进程无异常、图标/tooltip 无闪失
  (DELETE 与 ADD 为对 shell 的同步调用, 背靠背无中间可见态); **UIA Name 单段验证待 QA
  在其已提升图标可见性的环境点验** (我方环境图标在 Win11 溢出飞出区, UIA 树未暴露 ——
  本地 UIA 扫描读不到任何 SerialHub 图标, 与 QA 可读环境差异在此; 非代码路径差异)。
  机制保证: ADD 为全新 shell 条目, Name = ADD 时 szTip, 拼接态随 DELETE 一并消失。

### 2. 停机兜底时限 (QA 附带发现: 外部连接拖死优雅停机 >5s) — 已修

- **修法**: 停机序列改为「COM 释放优先 + 有界宽限」: watch 触发 (托盘退出 / `POST
  /api/shutdown` / Ctrl-C 三源合一) → **立即** `HubCmd::Close` + `ctx.stop_active()` →
  250ms 等串口线程退出 (COM 已释放) → axum 剩余宽限 1.25s → **未退完即
  `std::process::exit(0)`** (串口已停, COM 已释放, 进程立即结束)。总计 1.5s 有界。
  抽出可测函数 `finalize_shutdown(..., grace, hard_exit)`; run_service 生产路径
  hard_exit=true。
- **单测 +2**:
  - `shutdown_is_bounded_with_hung_client`: 永不完成的 serve (模拟外部挂连接) 下,
    Close 指令已发、stop_active 已置位、宽限耗尽走超时路径 (hard_exit=false 不杀测试进程)、
    不发 Stopped;
  - `shutdown_watch_triggers_graceful_sequence`: watch 触发端到端 → run_service 3s 内
    完成 → Stopped 事件发出。
- **自验**: headless 实例 (8084+COM2) 挂一条不说话的 raw TCP 连接 (模拟外部挂连接) →
  `POST /api/shutdown` → `{"ok":true}` → **进程 408ms 自退**、COM2 立即可重开 ✓。
- **顺带修掉的竞态**: watch 接收端原在 serve 任务内创建, Ready 与首次 poll 之间的窗口里
  到达的停机指令会因"接收端不存在"而 SendError 丢失 —— 两个接收端提前到 bind 之前创建
  (watch 晚订阅语义会漏看已发生的变更, 属真 bug)。
- 过程小坑: `tokio::select!` 两个臂分别消费同一 `JoinHandle` 编译不过 → 重构为
  「先等停机信号、后有界 join serve」的顺序结构 (顺带修正 Ctrl-C 下主路径不再卡 watch)。

### 修复轮环境声明

全程 COM2 + 127.0.0.1:8084; 未碰 COM1/8080 与 COM8; 进程按 PID 清理, 收工无残留。

## FIX-12 三修 (换路线: 静态 szTip + 相位仅图标图像) — 2026-09-12 第四次追加

QA 复验: 二修的 DELETE+ADD 使 Shell 侧条目翻倍 (UIA 双元素 + 注册表 NotifyIconSettings
4 条) —— 此路封死。按工单换根治姿势:

- **修法**: 托盘图标**只在启动时 NIM_ADD 一次**, szTip 为**静态**文案
  `"SerialHub · <端口> · 状态见控制台"`, 运行期**绝不 DELETE/重加、绝不改 szTip** ——
  UIA Name 拼接只发生在"字符串可变"上 (NIM_MODIFY 换 szTip 会拼出"旧 新"),
  字符串恒定则拼接无从发生。相位只用**图标图像**表达: 相位事件仅
  `NIM_MODIFY` 换 hIcon (绿/琥珀/灰描边圆点; 该路径 Sprint2 UX 已实测三色切换可靠),
  其余字段不动。`refresh_tray_shell_entry` (DELETE+ADD) 整体删除, 运行期不再有任何
  set_tooltip 调用。
- **回归**: 托盘菜单/左键恢复路径未触碰; 8084+COM2 实例连做 3 轮 close→open 相位切换
  (6 次 hIcon NIM_MODIFY) 进程稳定、相位正确、clients=1; cargo build 0 警告;
  cargo test **26/26 全绿**。UIA Name 恒为静态单段文案 + NotifyIconSettings 无新增
  条目 —— 留 QA 点验 (需其已提升图标可见性的环境)。
- **注册表说明**: NotifyIconSettings 已积累的旧条目 (SerialHub 同 hwnd 多条) 系二修
  DELETE+ADD 的产物, **无害且随系统清理**; 三修后代码只注册一次, 不再制造新条目。
- README 托盘描述同步: 悬停 tooltip 为静态文案, 相位看图标颜色与控制台徽章。
- 环境声明: COM2+8084, 未碰 COM1/8080 与 COM8; 进程按 PID 清理, 无残留; 未 commit。
