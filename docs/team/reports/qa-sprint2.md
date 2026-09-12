# QA 报告 — Sprint 2 复测 (FR-8 桌面客户端形态)

日期: 2026-09-12 · 角色: QA · 被测: 重建产物 `target/release/serialhub.exe` (含 wry/tao/tray-icon FR-8 代码, cargo build 0 警告, 51.1s)
方法: 黑盒 —— 套件回归 (tests/ 加 `--headless` 适配) + FR-8 CLI 面进程级核对 (Win32 窗口枚举 / HTTP / pyserial / 子进程退出码与 stderr)
结论: **回归 24/24 零回归; FR-8 CLI 核对 4/4 PASS; 0 残留进程; COM8 未碰**

---

## 1. 套件适配与全量回归

适配 (tests/ 正当修改, 未动预期): `tests/conftest.py` 的 `start_bridge` 统一加 `--headless`(Sprint 2 起二进制默认 GUI 模式, 套件需要旧行为 —— 否则 24 条测试每条弹一个窗口)。

| 项 | 结果 | 证据 |
|---|---|---|
| Rust 单元测试 | PASS | `cargo test`: **22 passed / 0 failed** (较 Sprint1 +1: `headless_gui_flags` 互斥用例) |
| 一致性套件 (`--headless` 适配后) | **PASS** | `python -m pytest tests/ -v`: **24 passed / 0 failed** (40.5s) —— FR-1×3 (完整性/广播/仲裁)、FR-2×12 (五参数矩阵/API 入口/越界拒绝/CLI 拒绝)、FR-3 (重开状态机)、FR-4×5 (九字段/clients/计数器/POST 形状/ports)、PERF×3 全绿 |
| PERF 基线保持 | PASS | PERF-1 双向达标、PERF-2 16 客户端、PERF-3 p95 达标 (与 Sprint1 基线一致, 数字不赘) |
| 进程卫生 | PASS | 会话前后 `tasklist` 均无 serialhub.exe; **COM8 未碰** (--list-ports 输出中出现 COM8 属桥自身枚举本机串口, 非占用) |

结论: `--headless` 旧行为完全保真, 数据面与 ADR-5 契约零回归 —— 与 Dev "桥逻辑 100% 复用" 的声明一致。

## 2. FR-8 CLI 面核对 (进程级断言, 不测观感) — 4/4 PASS

| 条目 | 结果 | 证据 (实测数字) |
|---|---|---|
| `--headless` | **PASS** | `--headless --port COM1 --addr 127.0.0.1:8081`: /api/status 200 + `phase=open, clients=0` (无 WebView 自连, 与无窗口互证); 运行中 Win32 `EnumWindows` 可见窗口枚举 **0 个 "SerialHub"**; 发 `CTRL_BREAK`(独立进程组) 后 ≤6s 退出, **COM1 立即可被 pyserial 重开** (释放确认, 失败=无) |
| 默认 GUI (`--port COM1`, addr 默认 8080) | **PASS** | 进程存活; Win32 枚举到可见窗口标题恰 **"SerialHub"**; /api/status 200 + `phase=open, baud=115200, clients=1` (WebView 内控制台 WS 自连, 与 Dev 冒烟一致); 授权 `taskkill /F /T` 后进程消失、**COM1 立即可重开** |
| 端口占用报错退出 | **PASS** | 先占 8081 起第二实例 (GUI 默认): **exit=1**, stderr = `serialhub: 端口被占用: 无法监听 127.0.0.1:8081: ... (os error 10048)`; headless 变体 (8082) 同样 exit=1 + 同文案; 两种模式均不弹窗、不残留 |
| `--list-ports` | **PASS** | exit=0, stdout 含 **COM1 与 COM2** (亦如实列出 COM8) |

最终态: `tasklist` 无 serialhub.exe —— 无任何残留。

## 3. 过程观察 (如实记录, 非阻塞)

1. **headless 优雅停机退出码**: `CTRL_BREAK` 后 exit code = 3221225786 (0xC000013A, STATUS_CONTROL_C_EXIT) —— Windows 控制台事件的默认处置, 非零退出码。判据 "干净退出" 满足 (进程消失、无残留、**COM1 立即释放**已验证), 但若希望脚本化场景拿到 0 退出码, 需 tokio ctrl-c handler 覆盖 CTRL_BREAK; 交 Dev 知悉, 不判 FAIL。
2. GUI 默认实例 `clients=1` (WebView 控制台自连) —— 进程级可观测, 与 Dev 冒烟一致; 该 WS 客户端计入 clients 契约属 FR-8 新语义, QA 后续套件如需精确 clients 断言须用 `--headless`(已适配)。
3. GUI 观感 (托盘菜单五项/气泡/变色/关窗退托盘/关窗行为) 未测, 留 UX —— 与任务分工一致; Dev 自列 "托盘菜单点击自动化未覆盖" 同此。
4. 测试全部以 COM1/8080-8082 操作; --list-ports 输出中的 COM8 为本机枚举事实, 测试从未打开它。

## 4. 结论摘要 (≤8 行)

1. 套件适配: conftest 加 `--headless`(唯一正当修改), 预期零改动。
2. 回归: cargo test 22/22, pytest **24/24**, PERF 基线保持 —— FR-8 对数据面/契约零影响。
3. `--headless`: 无窗口 + status open + 优雅信号退出 + COM1 立即释放 —— PASS。
4. 默认 GUI: 进程活 + 可见窗口 "SerialHub" + status open + clients=1(WebView 自连) —— PASS。
5. 端口占用: GUI/headless 双模式 exit=1 + stderr "端口被占用 … (os error 10048)" —— PASS。
6. `--list-ports`: exit=0 且列出 COM1/COM2 —— PASS。
7. 观察 2 条 (headless CTRL_BREAK 退出码 0xC000013A; GUI clients=1 语义) 如实交 Dev, 不阻塞。
8. **Sprint 2 QA 复测通过**; 托盘/气泡/关窗观感留 UX。
