# Dev 报告 — Sprint 1 (核心桥 MVP)

日期: 2026-09-12 · 角色: Dev · 状态: 待验收

## 1. 完成条目对照 (backlog P0 ◐ 三项)

### ① 后端骨架 (FR-1/2/3/4/5/7) — 已完成

| 需求 | 实现 | 自验 |
|---|---|---|
| FR-1 数据管道 | `/ws` 纯二进制双向; 串口读线程 → `broadcast`(容量 1024) 扇出; 客户端帧 → `mpsc` 队列 → 单写者线程 FIFO 串行写串口 (同批小帧合并 write_all, 只省 syscall 不乱序) | 冒烟 [2][3][4]: 上行/下行/双客户端广播全过 |
| FR-2 串口配置 | CLI `--port/--baud/--config/--addr` + `POST /api/config` (port/baud/dataBits/parity/stopBits/flow, 部分更新, 全字段校验); 波特率 110~2,000,000; 流控 none/rtscts/xonxoff | 单测 config 5 项 + 冒烟 [10] |
| FR-3 自动重开 | 监督任务独立于数据面, 状态机 Closed→Opening→Open→Retry; 打开失败/会话异常均 1s 重试; Open 指令可催单, Close 可随时打断重试等待 | 单测 supervisor 5 项 (假打开器注入) + 冒烟 [7][8][9] |
| FR-4 状态接口 | `/api/status` 恰好 9 字段 (phase/port/baud/config/clients/rxBytes/txBytes/lastError/uptimeSec); `/api/ports`; `/api/config|open|close` | 单测锁定字段集合; 冒烟 [1] |
| FR-5 CLI | `--port/--baud/--config/--addr/--list-ports/--no-open/-h`; 未给 `--port` 启动为未打开态 | 单测 cli 4 项 |
| FR-7 崩溃面 | 无 unwrap 于热路径; 带毒锁走 `into_inner`; 客户端硬断开 (transport.abort) 只 break 循环; clients 计数 Drop 兜底; 慢客户端 Lagged 丢旧帧不反压读线程 | 冒烟 [6] |

### ② FR-6 控制台 v1 — 已完成 (`ui/index.html`, 23.7 KB 单文件, include_str! 内嵌)

- 连接面板: 扫描串口 (name+desc)、波特率 (带常用值 datalist)、数据位/校验/停止位/流控、打开/关闭、
  四态徽章 (open=绿 / opening=绿闪 / retry=琥珀 / closed=灰, UI-3)、RX/TX/客户端/运行时长、最近错误条。
- 终端区: ASCII/HEX 双视图切换 (基于环形缓冲重放, 切视图不丢历史)、暂停滚动/清空、
  上限 5000 行、收发方向箭头前缀 (← RX / → TX)。
- 发送区: 文本/HEX 切换 (HEX 容忍空格/逗号/0x, 非法输入提示不发送)、追加 `\n` 可选、回车发送、
  未打开或 WS 掉线时禁用并明示原因 (UI-5)。
- 浅色工业风, 设计令牌全集中 `:root` (UI-2); 状态轮询 500ms + WS 断线 1s 自动重连;
  ≤900px 单列, ≤720px 统计两列, 无横向滚动 (UI-1)。
- 打开动作 = `POST /api/config` (表单当前值) + `POST /api/open`, 保证"所见即所开";
  phase≠closed 时配置控件锁定, 与"热改需 close→config→open"一致。

### ③ Rust 单元测试 — 已完成

`cargo test`: **21 通过 / 0 失败**。覆盖: config 解析 (合法/非法/边界波特率/流控)、CLI 解析
(默认值/全参/IPv6/非法输入)、状态机迁移 (失败重试→close 打断、打开阻塞时 Opening 可见、
打开成功后异常→Retry、干净关闭、重试等待可被打断)、计数器 (累加/不下溢/归零)、
status 契约 (字段集合精确锁定)、tx 队列 slot 语义 (未安装/消费者消失→丢弃不 panic)。

## 2. 关键取舍

1. **读写各占一条 OS 线程** (serialport 是阻塞 API, `try_clone` 双句柄), 与 tokio 经 channel 解耦
   —— 保证读循环永不 await 在客户端上 (硬约束 2)。写线程 `recv_timeout(100ms)` + stop 标志轮询,
   换来零运行时嵌套的简单性; 停机延迟 ≤150ms, 对串口场景无感。
2. **监督任务可测性**: 打开器做成 `PortOpener` trait, 单测注入假串口 (fail/stuck/die/alive 四种
   行为), 状态机迁移得以确定性断言, 不依赖真实串口。
3. **TX 队列 slot 换代**: 每次成功打开创建新 channel, 发送端存 `PortCtx.tx_slot`; 会话结束置 None。
   客户端在未打开/换代瞬间发的帧直接丢弃 (写线程死后旧 channel send 失败同理) —— 简单且绝不 panic。
4. **慢客户端策略**: broadcast Lagged = 丢旧帧保持连接 (串口是流, 回放无意义), 不踢人、不反压。
5. **CLI 手写解析** (6 个参数, 省一个 clap 依赖), 校验复用 config.rs 纯函数, 与 API 入口同一套规则。
6. **status 恰好 9 字段**, 不加 flow 等额外字段 —— 防止 QA 做严格字段集合断言时翻车。

## 3. 本机冒烟 (COM1↔COM2, 手动脚本已清理)

`cargo run -- --port COM1 --baud 115200 --config 8N2 --addr 127.0.0.1:8080` + pyserial(COM2) + websockets×N:
端口列表 / status 契约 / 上行 / 下行 / 双客户端广播 / 计数与客户端增减 / 硬断开不炸 /
指向不存在端口 → retry+lastError / close 打断重试 / 恢复 COM1 / config 非法参数 400 —— 全过。
测试进程已杀干净 (COM8 全程未碰)。

## 4. 遗留问题 / 风险

1. **PERF 未测** (P1, Sprint 2 条目): broadcast 每帧一次 Vec 分配、合并写上限 64KB, 预计 921600
   无压力但未实测; QA 套件会记录 perf 基线。
2. **desc 字段在 Windows 上**: 仅 USB 串口有产品/厂商名 (CH340 显示正常), ELTIMA 虚拟对为空串;
   serialport-rs 不暴露注册表友好名, 如 UX 强需求再加 winreg。
3. **终端"行"按协议帧计**: 一帧含 `\n` 仍算一行 (pre-wrap 展示); 与"5000 行"的字面语义有出入,
   内存上限目的已达成, 请 UX 判断观感。
4. **`/api/open` 在已打开态是 no-op** (不按新 config 重开) —— 符合 spec"热改需 close→config→open",
   但若有工具直接调 API 改 config 后立刻 open, 会以为切换生效了; UI 侧已用控件锁定规避。
5. Linux/macOS 仅保证代码无平台 API (CI 由 Sprint 2 承接); serialport 在 Linux 需系统 libudev。

## 5. SPEC-QUESTION (请架构师裁定)

1. **POST /api/config、/api/open、/api/close 的响应体 spec 未定义**: 现为成功 `{"ok":true}`、
   失败 400 + `{"ok":false,"error":"..."}`。若 QA 要断言别的形状 (如回带 status), 请在 spec 补充。
2. **uptimeSec 语义**: 实现为**进程运行时长** (非"本次打开时长")。spec 未言明, 若是后者请指出。
3. **status 无 flow 字段** (契约固定 9 字段): 页面无法回显服务端当前流控。若需要, 属契约变更。
4. **`/api/open` 已打开时**: 现为 no-op。备选语义"按当前 config 先关再开"会更宽容, 但与
   "热改需 close→config→open"的 spec 表述冲突, 维持 no-op, 请确认。

## 6. 启动命令

```bash
cargo run --release -- --port COM1 --baud 115200 --config 8N2 --addr 127.0.0.1:8080
# 浏览器打开 http://127.0.0.1:8080; 未给 --port 则启动为未打开态, 由控制台驱动
cargo test          # 21 项单测
cargo run -- --list-ports
```

文件清单: `Cargo.toml` · `src/{main,cli,config,hub,serial,supervisor,api}.rs` · `ui/index.html`
