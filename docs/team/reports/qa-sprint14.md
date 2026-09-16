# QA Sprint 14 报告 — ADR-25 单实例改版 (FR-16) 回归 + 独立黑盒收口 (qa 401)

- 日期: 2026-09-16 · 对象: 工作区未提交改动 (v2.0.2 + ADR-25 改版, src/fleet.rs / gui.rs /
  main.rs + tests/test_fr16_single_instance.py + spec/backlog/decisions 随动) · 未 commit
- 依据: ADR-25 (decisions.md) · spec FR-16 (改版) · dev-sprint14-backend.md ·
  tests/test_fr16_single_instance.py (dev 先例, 本席独立复核)
- 结论: **回归全绿 (cargo 122/122, pytest 82/82) + 独立黑盒 25/25 PASS, 建议验收**。

## 1. fr16 套件断言语义独立复核 (读码, 非复跑口径)

- fr16a (headless 第二实例): 断 exit 0 + stderr 含宽松核心 "已在运行" (仅 debug 断文本,
  release 按 FR-18 只断退出码) + A 存活 /api/status 仍 200 —— 与 ADR-25①② 相符,
  宽松核心不锁三态文案前后缀, 无过度约束。
- fr16c (GUI 双实例): A 经 **WM_CLOSE** (与用户点 X 同路径) 关窗到托盘 → 断
  IsWindowVisible=false; B 存活期**轮询 #32770 弹框, 一现即违约** (弹框阻塞退出,
  检出可靠) + B 25s 内 exit 0 + A 窗口唤回 (**可见→隐藏→重新可见** 分支:
  IsWindowVisible=true 且 IsIconic=false) + A 存活。语义正确; 会话级 `fr16_gate`
  行为探针区分"未落地"(SKIP BLOCKED-BY-BACKEND) 与"违约"(FAIL), 门控设计合理。
- fr16b/fr16d (非 SerialHub 占用): **不设门**原样保留, 守护 FR-8 旧错误路径不被误伤 ——
  覆盖面正确。
- 已知边界 (与 dev 人工清单一致): "不开浏览器标签 / 焦点抢到前台 / 托盘图标" 属目视项,
  自动化不覆盖 (Windows 前台锁语义下 GetForegroundWindow 易假阴/假阳, 不硬断)。

## 2. 全量回归

| 项 | 结果 | 备注 |
|---|---|---|
| cargo test | **122/122 passed** (3.99s) | 与 dev 自报一致 (117+5 新增) |
| pytest 全量 | **82/82 passed** (237s) | 含 fr16 8/8 (release+debug 双构建), perf 3 条 |
| fr13a | **PASS** | 见 §4 环境注记: 清旧实例后复验即绿, 佐证 dev 的根因诊断 |
| fmt/clippy | 未另跑 | CI 门禁口径 dev 已自报, 本席未改动任何源码 |

## 3. 独立黑盒 (release 构建 target/release/serialhub.exe, 端口 18600+, COM1, 自写 Win32 断言)

| # | 场景 | 断言 | 结果 | 证据 |
|---|---|---|---|---|
| BB1 | GUI 双实例: A 起桥 (POST /api/fleet 建 COM1@115200/8N1 → listen 18602, phase=open) → WM_CLOSE 关窗隐藏 → B 第二实例 | B exit 0 静默 + 无 #32770 + A 唤回 + 桥不受扰 | **13/13 PASS** | B rc=0, B 存活期 0 弹框; A visible=1 iconic=0; 桥仍 open (`[('CLI','closed'),('qa-bb1','open')]`); B stderr 为空 (GUI 子系统, FR-18 既定) |
| BB2 | 最小化态: A `ShowWindow(SW_MINIMIZE)` (IsIconic=true) → B | A 恢复可见且非最小化 | **6/6 PASS** | B rc=0 无弹框; A visible=1 iconic=0; A 存活 + status 200 |
| BB3 | headless 双实例 (A headless 就绪 → B headless 同地址) | B exit 0 + stderr 一行 | **4/4 PASS** | B rc=0, stderr 恰一行 `SerialHub 已在运行, 对端无窗口可前置: http://127.0.0.1:18620` (WakeResult NoWindow 文案, 含核心 "已在运行"); A 存活 |
| BB4 | 非 SerialHub 占用 (裸 socket 占 18630, GUI 第二实例) | 错误框维持 (FR-8) + exit 非 0 | **2/2 PASS** | #32770 错误框弹出 (进程级), WM_CLOSE 关框后 rc=1 |

- 合计 **25/25 PASS**。脚本: 会话临时件 (Temp/qa_sprint14_bb.py), 独立实现不复用 dev 辅助代码。
- 进程回收: 全部 taskkill **/PID** 精确回收, 收口后 tasklist 无 serialhub、无 186xx 残留监听。

## 4. 环境注记 (非缺陷)

1. **旧实例清场**: 昨日残留 serialhub (PID 46828, 监听 8090/7088, 占 COM1) 经
   `POST /api/shutdown` 优雅关闭 (响应 200), COM1 释放 —— 此后 fr13a 复跑即绿,
   与 dev 报告 §3 的根因诊断 (COM1 被旧实例占用) 相互印证, **非本变更问题**。
2. BB1 首跑建桥撞 18601: A 启动时自动播种的 CLI 兼容桥预占 manager+1 (FR-17 语义),
   属既定行为, 测试脚本改用 18602 后通过; 与 FR-16 无关。
3. 本机 18681/18682 的监听者为用户自用 mini-rtt-viewer (非测试产物), 未触碰。

## 5. 留给人工目视清单 (自动化受限, 沿 dev 清单收窄)

1. B 退出全程无浏览器标签被打开 (BB1/BB2/BB3 自动化只断无 #32770 + exit 0)。
2. A 唤回后焦点抢到前台 (GetForegroundWindow=A)。
3. 托盘图标在唤回后仍在, tooltip/三态不受影响。
4. release 安装包双击真图标场景 (建议出包后点一遍)。
