# Dev Sprint 8 后端报告 — FR-16 单实例友好处理 / FR-18 发布版无控制台 (ADR-19①③)

作者: 后端主程 (201) · 2026-09-13 · 依据: decisions.md ADR-19 / spec FR-16/FR-18 /
backlog「Sprint 8 · dev-backend (波1)」。改动范围: `src/fleet.rs`(+169)、
`src/gui.rs`(+43)、`src/main.rs`(+25);**零新增依赖** (探测用 std TcpStream 手写
最小 HTTP GET;Cargo.toml 未动, 未引入第二二进制)。不 commit;真机自验用
127.0.0.1:17890/17895/17896/17897 + --no-fleet, 全程未触碰任何 COM 口 (含 COM8),
验证完杀净, 无残留进程。

## 1. FR-16 单实例友好处理 (bind 失败 → 探测 → 分流)

- **探测目标 = 生效地址**: bind 失败后要探测的是"这次想绑却没绑上的地址"。
  FR-13 后管理台地址会随 fleet.json [manager] 漂移, 故把 run_manager_with 里
  的地址解析抽成 `fleet::effective_control_addr(fleet_path, cli_addr,
  addr_explicit)` (显式 --addr 优先 → 清单恢复值 → CLI 默认), **bind 与探测共用
  同一函数**, 保证两边目标恒一致;含 [manager] 恢复值路径的单测。
- **纯函数判别 (可测)**: `classify_instance_probe(Option<&str>) -> InstanceProbe`
  —— Some(响应体) 且形状命中 → `AlreadyRunning`;其他响应 / None(超时·连接拒绝·
  读失败) → `NotOurs`。形状判别 `status_body_is_serialhub`:
  - 主判据 = 响应体含 `"phase"` 字段 (spec FR-16 口径;恰好一桥时 hub.status_json
    必有此字段);
  - **辅判据 = 409 文案「当前不是单桥模式」** —— 无桥/多桥实例的 /api/status 返回
    legacy_unavailable (409, 无 phase 字段), 缺了它建过多座桥的实例会被误判为
    "其他程序占用" 而退回错误框。两签名 (带引号字段名 / 中文专句) 他软件极难撞上,
    误判代价上限也只是换一种提示框。
- **HTTP 探测**: `http_get_status_body` —— std::net::TcpStream 同步栈
  (调用点在 GUI 主线程 / headless 收尾, 不在 tokio 上下文), connect/read/write
  各限 1s, `GET /api/status` + Connection: close, 取空行之后的响应体。
- **GUI 路径 (gui.rs)**: 就绪握手 `Ok(Err(e))` 分支 (bind 失败唯一入口) 先探测;
  命中 → `already_running_notice`: MessageBoxW **信息样式** (MB_ICONINFORMATION,
  标题 "SerialHub", 不带「启动失败」), 文本 `SerialHub 已在运行\n管理台:
  http://<addr>`, 用户点「确定」后 `open_in_browser` 打开既有管理台, 进程
  `exit(0)`。未命中 → 原样 fatal_msgbox (FIX-14/ADR-8 错误语义零变化)。
  非 Windows 无对话框路径: 直接开浏览器 + exit 0 (语义一致)。
- **headless 路径 (main.rs)**: run_manager 失败后同样探测;命中 → stderr 一行
  `SerialHub 已在运行: http://<addr>` + exit 0 (脚本可据此判定"服务已在");
  未命中 → 原 `serialhub: <e>` + exit 1 不变。

## 2. FR-18 发布版无控制台 (windows_subsystem)

- main.rs **第一行** (一切 use/文档注释之前):
  `#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]`
  —— release 双击无黑窗;debug 保留控制台。
- **println 不 panic 实证** (先行验证, 决定是否需要兜底): 写 4 行最小 rust 程序
  (println/eprintln/print 后 exit 42), rustc -O 编译后经 PowerShell Start-Process
  以**无控制台句柄**方式启动 (模拟双击) → 输出静默丢弃、exit=42 —— _print 对
  不存在的 stdout 不 panic。故 run_manager 就绪 println 等 7 处 print 无需
  任何兜底, release GUI 照常运行 (T5 佐证)。
- **PE 头断言**: target/debug/serialhub.exe subsystem=**3** (console, 保留),
  target/release/serialhub.exe subsystem=**2** (GUI, 无黑窗)。
- headless release 的 stdout 随之不可见 = spec 既定取舍;**手册需加一句注明**
  (属文档席动作, 见「交接」)。

## 3. 单测 (73 → 79, 全绿)

`fleet::probe_tests` 新增 6 个:
`serialhub_status_body_is_detected` (真 /api/status 形状) /
`multi_bridge_409_body_is_detected` (辅判据) /
`other_program_body_is_not_detected` (非 SerialHub JSON、裸词 phase 的 HTML、空体) /
`timeout_or_unreachable_is_not_detected` (None 分支) /
`effective_addr_explicit_wins_then_fallback` (显式优先 + 无清单/清单不可读回退) /
`effective_addr_persisted_manager_wins_when_not_explicit` (临时 fleet.json
[manager] 恢复值生效, 用后清理)。
`cargo test` → **79 passed; 0 failed**。新增代码 cargo fmt --check 零 diff
(仓库其余 80+ 处 fmt 漂移为既有, 未动);`retries` dead_code 警告亦为既有
(我方改动只增不减用法)。

## 4. 真机自验 (release/debug 五场景, 退出码 + 日志断言为主)

- **T1 headless 友好路径 (release)**: A 实例 (--headless --no-fleet --addr
  127.0.0.1:17890) 就绪后,curl /api/status 确认 `"phase":"closed"` (探针数据源);
  B 实例同地址 → stderr 恰一行 `SerialHub 已在运行: http://127.0.0.1:17890`,
  **exit=0** ✓
- **T2 非 SerialHub 占用 (release)**: python 假占用者 (accept 后立刻 close,
  不回响应) 占 17895 → C 实例报 `serialhub: 管理台端口被占用: 无法监听
  127.0.0.1:17895: ... (os error 10048)`,**exit=1** —— 既有错误语义零变化 ✓
- **T3 GUI 弹窗路径 (release, 自动化)**: A 在 17890 存活时 Start-Process 启动
  release GUI 第二实例 (--addr 127.0.0.1:17890 --no-fleet, 无控制台) → Win32
  枚举到该 PID 的 #32770 对话框 (标题 "SerialHub"), 子控件 Static 文本**逐字**
  = `SerialHub 已在运行\n管理台: http://127.0.0.1:17890`,「确定」Button 存在 →
  BM_CLICK 后 **GUI-exit-code=0** ✓ (exit 0 只能发生在 already_running_notice
  返回之后, 即 open_in_browser 已同步执行 —— 浏览器打开为执行顺序所证明;
  弹窗为信息样式由代码 MB_ICONINFORMATION 保证, 图标目视留 QA)。
  实现备注: 该框确定按钮控件 id=2 而非 IDOK=1, GetDlgItem(h,1) 取不到,
  自动化需枚举子控件;UIA 子树与 SetForegroundWindow+SendKeys 在本会话均不可靠。
- **T4 debug 双实例**: debug 构建同场景 → 同样友好分支 (exit 0 + 精确 stderr),
  且控制台输出照常可见 ("管理台就绪" 打印在案) ✓
- **T5 release GUI 第一实例**: Start-Process 模拟双击 (--addr 127.0.0.1:17896
  --no-fleet) → 5s 后进程存活且主窗口 'SerialHub' 可见 (bind→就绪→窗口/webview/
  托盘→事件循环全链路无 panic) → 按 PID 杀净 ✓
- **环境干扰备注 (给 QA/后续席位)**: 自验期间检测到并行席位同时在跑
  `pytest tests/` 与 serialhub.exe (--addr 8080/8096), 且发生了
  `taskkill /IM serialhub.exe` 式**全量清扫**, 两次误杀我方实例 (表现为 exit 1
  无输出)。QA 跑 FR-16 套件请: ① 用专属端口段并 / ② 只按 PID 清自己的进程,
  勿按映像名全杀。

## 5. 交接 / 遗留

- 弹窗信息图标 (MB_ICONINFORMATION) 与双击 UX 细节: 留 QA 目视一眼 (T3 已证文本
  与退出码)。
- 手册 (docs/manual/) 需按 ADR-19③ 注明 "headless + release 构建下 stdout 不可见"
  —— 文档席一句话动作。
- dev-ui 的 FR-17 预填与 QA 的 tests/test_fr16_single_instance.py 为并行席位
  交付, 本报告未涉及。
