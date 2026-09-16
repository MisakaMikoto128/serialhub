# Dev Sprint 14 报告 — ADR-25 单实例改版: /api/show 唤起主窗口 (dev-backend 201)

- 日期: 2026-09-16 · 变更文件: `src/fleet.rs` / `src/gui.rs` / `src/main.rs` +
  `tests/test_fr16_single_instance.py` + `docs/product/spec.md` (FR-16 随动) +
  `docs/team/backlog.md` (状态行) · Cargo.toml **零新增依赖** · 未 commit
- 依据: **ADR-25 用户裁定** (重复点图标 = 把软件叫回来, 无提示框不开浏览器) ·
  spec FR-16 (本报告随动改版) · backlog Sprint 14 P0 行
- 基线核实: 接手时 cargo test 117/117; 本实例落地后 **122/122** (117 + 新增 5);
  fmt / clippy -D warnings 全过 (CI 门禁口径)。

## 1. 交付物

1. **POST /api/show 新端点** (控制面路由, fleet.rs):
   - GUI 实例: handler 经 `ControlState.show_window` 钩子把 `UserEvent::ShowMainWindow`
     打进 tao 主循环 (钩子 = `EventLoopProxy` 克隆包成的 `Arc<dyn Fn + Send + Sync>`
     闭包, tao proxy 三平台均 Send+Sync —— win 有显式 unsafe impl, linux/mac 底层
     crossbeam Sender 天然 Sync), 主循环新分支执行
     `window.set_visible(true) + set_minimized(false) + set_focus()`;
     应答 `{"ok":true,"shown":true}`。
   - headless 实例: 钩子 None → `{"ok":true,"shown":false}`, 不 panic 不弹窗。
   - 事件循环启动前发送的事件天然排队 (gui.rs 模块头既定机制), 启动竞窗零处理成本。
2. **第二实例流程改版** (ADR-25):
   - GUI (gui.rs bind 失败路径): 探测判明 SerialHub → `wake_running_instance()`
     POST /api/show → **静默 exit 0**。原「信息框 + 开浏览器」整段删除
     (`already_running_notice` 两平台变体一并移除); stderr 一行结果 (release GUI
     无控制台不可见, debug 可见, 不构成弹窗)。
   - headless (main.rs): 同样 POST /api/show → stderr 一行 + exit 0。
   - 非 SerialHub 占用 → 维持既有错误框 (GUI) / exit 1 (headless), FR-8 语义不变。
3. **接线方式 (任务③的评估结论)**: 没有把裸 `EventLoopProxy` 传进 run_manager ——
   fleet.rs 定义 `pub type ShowMainWindowHook = Arc<dyn Fn() + Send + Sync>`,
   `run_manager / run_manager_with` 各加尾参 `show_window: Option<ShowMainWindowHook>`
   (gui.rs 传 Some, main.rs headless 传 None, 测试 spawn 传 None), 经每轮
   ControlState 重建注入控制面 handler。选闭包而非 proxy 的理由: ① fleet.rs 不必
   依赖 gui::UserEvent 类型; ② None 即 headless 的显式建模, handler 侧无需 cfg;
   ③ 任务建议的"App 状态"实为 api.rs 的数据面结构 (App), /api/show 属控制面
   (第二实例探测的是管理台地址), 落在 ControlState 才是对的注入点。
4. **唤起结果三态** (fleet.rs `WakeResult`, 供 stderr 文案):
   `Shown` (对端 GUI, 窗口已前置) / `NoWindow` (对端 headless, 无窗可前置 ——
   同样算成功) / `Failed` (请求失败, 对端可能恰在退出; 探测已判明是 SerialHub,
   仍 0 退出 —— 用户再点一次即正常启动, 端口已释放)。
   POST 实现口径同探测 GET: 手写 std TcpStream, 连接/读写各 1s, 状态行非 2xx 判失败。

## 2. 测试

1. **cargo 单测 117 → 122** (+5, 全绿):
   - `show_outcome_classifies_ok_shown_variants` / `show_outcome_rejects_bad_or_missing_body`
     (响应判别纯函数); `api_show_core_none_hook_reports_unshown` /
     `api_show_core_invokes_hook_and_reports_shown` (端点核心, 钩子注入记录调用);
     `show_endpoint_reports_shown_per_hook` (真实 control_router HTTP 端到端)。
2. **fr16 套件随动改版** (dev 先例: Sprint 12 fr19 修): 8/8 通过 (release+debug 双构建)。
   - fr16a (headless 第二实例): 断言不变 (exit 0 + stderr 含 "已在运行") —— 新
     stderr 三态文案均含该核心子串, 文本断言只锁宽松核心不过度约束。
   - **fr16c 重写** (`test_fr16c_gui_second_instance_wakes_first_window`):
     A=GUI 实例 → 主窗口 (按 pid+标题 "SerialHub" 枚举, 排除 #32770) 经 WM_CLOSE
     关窗到托盘 → B=GUI 第二实例 → 断言 **B exit 0 + B 存活期轮询无 #32770 弹框**
     (弹框会阻塞退出, 检出可靠) + **A 窗口被唤回** (IsWindowVisible=true 且
     IsIconic=false, 可见性 隐藏→可见 分支) + A 仍活 /api/status 仍 200。
   - fr16b/fr16d (非 SerialHub 占用): 不设门原样保留 —— 旧错误路径未被误伤。
   - 模块头契约说明与人工清单项同步 ADR-25 (无浏览器标签/焦点抢到前台=目视项)。
3. **真机自测** (全过, 端口 18500+, 按 PID 清理, 未 commit):
   - headless 双实例 (18500): B exit 0, stderr
     `SerialHub 已在运行, 对端无窗口可前置: http://127.0.0.1:18500`;
     直 POST /api/show 得 `{"ok":true,"shown":false}`。
   - GUI 双实例 ×2 态 (18502 最小化到任务栏 / 18503 关窗到托盘): B exit 0 无弹框,
     A 窗口两态下均被唤回 (visible=1, iconic=0), A 进程存活。
   - COM 未触碰 (COM8 禁碰), 自测进程全部按 PID 回收。

## 3. 环境注记 (非本变更问题)

- `test_fr13a_manager_addr_rebind_persist_and_restart` 在本席复跑失败: 根因
  **COM1 打开被拒 (拒绝访问)** —— 昨日 18:26 启动的旧 serialhub 实例 (PID 46828,
  监听 8090/7088, 非本实例所起, 未代杀) 仍占 COM1。fr13 其余 5 条全过; 与本变更
  无关路径 (COM open / 桥状态机零改动), QA 全量回归时如仍复现请先清旧实例。
- 版本号未动 (仍 2.0.2): v2.1.0 发版随 Sprint 14 集成复验 (backlog P1) 一并做。

## 4. 留给 QA / 集成的人工清单 (GUI 自动化受限项)

1. GUI 双实例: B 退出全程**无浏览器标签被打开** (自动化只断无弹框)。
2. A 窗口唤回后**焦点抢到前台** (GetForegroundWindow 为 A) —— Windows 前台锁
   (foreground lock) 语义下自动化易假阴/假阳, 留目视。
3. 托盘图标仍在 (唤起不打断托盘), tooltip/图标三态不受影响。
4. release 安装包形态 (GUI 子系统, 无控制台) 双击场景 = fr16c release 参数已覆盖,
   建议出包后再点一遍真图标。
