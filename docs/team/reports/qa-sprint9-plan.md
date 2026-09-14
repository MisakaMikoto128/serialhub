# QA Sprint 9 计划/清单 — ADR-21 回归 + open-console 黑盒 + 人工清单 (2026-09-14)

作者: QA (401) · 依据: decisions.md ADR-21①②③④ · backlog「Sprint 9 · qa+docs (波1)」。
纪律: 黑盒, 预期只来自 ADR-21, 失败不改预期; 串口纪律同套件 (仅 COM1/COM2 虚拟对允许,
全程禁碰 COM8); open-console 黑盒用无 `--port` 的 headless 实例 (不占串口); 进程全生命周期
管理, 测毕 taskkill 杀净; 不 commit。

## 1. 自动化回归 (dev 波1 落地后全部重跑, release 二进制先重建)

| 项 | 结果 |
|---|---|
| `cargo test` | **83 passed / 0 failed** (基线 80 + 新 3: `tray_left_click_restores_window` / `tray_right_and_other_keys_never_touch_window` / `open_console_endpoint_reports_console_addr`) |
| `python -m pytest tests/ -q` | **63 passed / 0 failed / 0 skipped** (3m00s, 无行为回归) |
| `tools/ui_pixel_audit.js` 独立复跑 | **PASS — 155 可见控件 × 7 轮 0 违例** (高度 ∈{28,34}±0.5px, 圆角 8±0.5px; 新复制图标钮以 28px 紧凑档入径: R1 页头钮 + R3/R4 抽屉钮实测达标) |

基线对照 (落地前实测): cargo 80/80 · pytest 63/63 —— 唯一自动化增量为 3 条 Rust 单测,
集成套件与像素口径均无回归。

## 2. POST /api/open-console 黑盒 (ADR-21②)

环境: release 构建 headless, `--addr 127.0.0.1:18099`, 无 `--port`; 实例恢复了既有 fleet
兼容桥 b1 (COM1 虚拟对, 属允许口), 全程未触碰 COM8。

| # | 检查 | 实测 |
|---|---|---|
| a | POST `/api/open-console` | **HTTP 200** + `{"addr":"http://127.0.0.1:18099/","ok":true}` (契约: ok + addr 回显管理台地址) |
| b | GET 同路径 | **HTTP 405** (post-only 守卫成立) |
| c | 系统浏览器真开 | POST 后默认浏览器进程数 38→39, 真实弹出管理台标签并渲染 (127.0.0.1:18099) —— "浏览器真开一次"已执行, 留档本行 |
| d | 进程残留 | 测毕 `taskkill /F /IM serialhub.exe /T`, tasklist 复查 0 残留 |

## 3. 人工目视清单 (自动化不覆盖, 随 v1.5.0 发布页留档)

| # | 项目 | 预期 (ADR-21①③) |
|---|---|---|
| M1 | **托盘右键菜单稳定** | 右键弹菜单不再瞬间消失: 主窗口不被拉起、焦点不被抢, 菜单停留可正常点选 |
| M2 | **托盘左键恢复** | 左键单击/双击均恢复主窗口; 右键/中键/悬停不触碰窗口 (与 Rust 单测口径一致) |
| M3 | 打开面板 (桌面壳) | 壳内点「打开面板」→ 系统默认浏览器打开管理台 (改走 /api/open-console); 端点失败才降级为复制网址提示 |
| M4 | 复制图标钮 + 气泡 | 全站复制钮为 28px 图标钮; 点击后钮旁弹「已复制」约 1.5s 自散, 连点不堆叠 (dev 截图 `dev-sprint9-ui/icon-*.png` 佐证, 发布页人工再过一遍) |
| M5 | open-console 浏览器目视 | §2c 真实标签人工确认页面渲染正常 (截图或口述留发布页) |

M1/M2 即「托盘右键 bug 修复」验收口径: 修复前右键弹菜单瞬间主窗口被拉起抢走焦点致菜单即逝;
修复后右键专属菜单、左键专属恢复, 两键互不干扰。

## 4. 文档席 (604) 同波交付 (docs 侧无重复描述)

- 手册补缺口: 主题插件用法/自制 (新「设置弹窗」节) / 单实例友好提示 (FAQ 改写) /
  新建桥端口预填 (§三) / release 无控制台与 headless stdout 取舍 (§六表下注) /
  打开面板按钮与复制图标钮·气泡 (§五仪表盘) / 管理台网址原地修改 (§五设置弹窗) /
  数据口双路径兼容 (§七) / `--themes-dir` 入 CLI 表 / `POST /api/open-console` 入 API 表;
  适用版本 v1.2.0 → v1.5.0。
- 去重与统一: README 的 Python 示例代码与"两条语义"段并入手册「程序接入」后改链接;
  「控制台」→「管理台」统一 (README ×2、CHANGELOG ×1; 托盘菜单项「在浏览器打开控制台」
  为界面原生文案, 保留); 手册表格/空行的编辑器格式噪音回滚为 HEAD 口径。
- CHANGELOG 补 v1.5.0 条目 (§本文件回归数字)。
