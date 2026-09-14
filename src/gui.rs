//! GUI 壳 (FR-8 / ADR-7): tao 事件循环 (主线程) + wry WebView (加载内嵌控制台,
//! 不做第二套界面) + tray-icon 托盘 (四态图标 / 菜单 / 关窗即后台)。
//!
//! ⚠ 整合要点 (下一个接手的人先读, 这是本项目最容易踩的坑):
//! - Win32 规定窗口/托盘/菜单必须在主线程; tao 的 `EventLoop::run` 占死主线程且永不返回;
//! - 因此 tokio 服务整个搬去后台线程 (service.rs), 主线程只跑事件循环;
//! - 服务 → 主线程: `EventLoopProxy<UserEvent>` (Send+Clone) —— 服务侧经
//!   `proxy.send_event` 把相位变化/停止通知打进主循环, 事件循环未启动前发送的事件会排队;
//! - 主线程 → 服务: ①托盘菜单指令直接 `cmd_tx.send(...)` (tokio unbounded 的同步 send);
//!   ②退出经 `watch::Sender` 触发 service 的优雅停机, service 清理完 (串口线程释放 COM)
//!   再回发 `Stopped`, 主循环收到后才 `ControlFlow::Exit` 结束进程;
//! - 事件循环闭包里绝不能 await / block_on —— 会冻结 Win32 消息泵, 窗口假死;
//! - 托盘气泡: tray-icon 没有气泡 API, 借其公开的 `window_handle()` + uID=1
//!   (tray-icon 全局计数器从 1 起, 本进程只建一个托盘) 直接 Shell_NotifyIconW(NIF_INFO)。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::ControlFlow;
use tao::window::{Icon as TaoIcon, WindowBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, TrayIconBuilder, TrayIconEvent};

use crate::cli::Cli;
use crate::fleet::{run_manager, ManagerStartup};
use crate::hub::Phase;
use crate::service::ServiceEvent;
use crate::supervisor::HubCmd;

/// 服务 → 事件循环 的用户事件 (经 EventLoopProxy 从后台线程打回主线程)。
#[derive(Debug, Clone)]
pub enum UserEvent {
    /// 相位或端口变化 (托盘图标/tooltip 刷新)。
    PhaseChanged {
        phase: Phase,
        port: String,
    },
    /// FR-13: 管理台地址原地换绑成功 —— webview 导航到新地址 (页面跟随闭环壳侧)。
    AddrChanged {
        addr: SocketAddr,
    },
    ShowWindow,
    OpenPort,
    ClosePort,
    OpenBrowser,
    /// 托盘菜单「退出」: 触发服务优雅停机。
    Quit,
    /// 服务清理完毕 (串口已释放), 可以退出进程。
    Stopped,
}

// 菜单项 id (muda MenuId)
const ID_SHOW: &str = "show";
const ID_BROWSER: &str = "browser";
const ID_OPEN: &str = "open";
const ID_CLOSE: &str = "close";
const ID_QUIT: &str = "quit";

pub fn run_gui(cli: Cli) -> Result<(), String> {
    // 事件循环必须建在主线程 (Win32); build 需要 &mut
    let mut loop_builder = tao::event_loop::EventLoopBuilder::<UserEvent>::with_user_event();
    let event_loop = loop_builder.build();
    let proxy = event_loop.create_proxy();

    // —— 服务线程: tokio runtime 整体在后台线程跑 (见模块头注释) ——
    // cmd 通道在这里创建: 主线程留一个 clone (托盘菜单的 打开/关闭串口 直接发),
    // 另一个 clone 交给 service (auto_open 与退出时的 Close 都从这里发)。
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<SocketAddr, String>>();
    let (shutdown_tx, _) = tokio::sync::watch::channel(false);
    let service_shutdown_tx = shutdown_tx.clone(); // 托盘留 shutdown_tx, 服务线程拿 clone
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<HubCmd>();
    let service_cmd_tx = cmd_tx.clone();
    // Sprint 4 (FR-10): GUI 同样走多桥管理器 (管理台 = 控制面, 兼容桥承载旧 CLI 参数)
    let startup: ManagerStartup = ManagerStartup::from_cli(&cli);
    // ADR-22①: GUI 侧自留一份 fleet 路径 —— 启动时读 [window] 段恢复窗口几何,
    // 真实退出时写回; headless 形态不读不写, --no-fleet 两者皆跳过。
    let fleet_path_gui = startup.fleet_path.clone();
    // FR-16: bind 失败时的单实例探测目标 (= run_manager 将尝试绑定的生效地址,
    // 随 fleet.json [manager] 恢复值漂移; 与 bind 共用 fleet::effective_control_addr)
    let probe_addr = crate::fleet::effective_control_addr(
        startup.fleet_path.as_deref(),
        startup.control_addr,
        startup.addr_explicit,
    );
    let port0 = cli.port.clone().unwrap_or_default(); // 托盘 tooltip 初值
    let proxy_for_service = proxy.clone();
    let ready_tx2 = ready_tx.clone(); // 给 on_event (首次 Ready)
    let ready_tx3 = ready_tx.clone(); // 给 block_on 的 Err 回传
                                      // FR-13: Ready 事件分流 —— 首次 = 服务就绪 (ready 通道握手);
                                      // 之后每次 = 管理台原地换绑成功 → 主线程 webview 导航跟随新地址。
    let first_ready = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let first_ready_for_service = first_ready.clone();
    let service_thread = std::thread::Builder::new()
        .name("serialhub-service".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx2.send(Err(format!("tokio runtime 启动失败: {e}")));
                    return;
                }
            };
            let on_event: crate::service::OnEvent = Arc::new(move |ev: ServiceEvent| {
                let ue = match ev {
                    ServiceEvent::Ready(addr) => {
                        if first_ready_for_service.swap(false, std::sync::atomic::Ordering::Relaxed)
                        {
                            let _ = ready_tx2.send(Ok(addr));
                        } else {
                            let _ = proxy_for_service.send_event(UserEvent::AddrChanged { addr });
                        }
                        return;
                    }
                    ServiceEvent::Phase(phase, port) => UserEvent::PhaseChanged { phase, port },
                    ServiceEvent::Stopped => UserEvent::Stopped,
                };
                let _ = proxy_for_service.send_event(ue);
            });
            if let Err(e) = rt.block_on(run_manager(
                startup,
                service_cmd_tx,
                cmd_rx,
                service_shutdown_tx,
                Some(on_event),
            )) {
                // 就绪前失败 (如端口被占用) 必须立即回传主线程; 就绪后失败时 ready 端已关, 发送失败无妨
                let _ = ready_tx3.send(Err(e));
            }
            // block_on 返回 = 服务清理完毕 (Stopped 事件已发), runtime 随之析构
        });
    service_thread.map_err(|e| format!("服务线程启动失败: {e}"))?;

    // —— 等服务就绪 (端口被占用在此处报错; MessageBox 让双击用户可见, ADR-8) ——
    let addr = match ready_rx.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok(addr)) => addr,
        Ok(Err(e)) => {
            // FR-16: 占用者可能是另一个 SerialHub —— 先按 /api/status 响应形状探测;
            // 是 → 信息框 (非错误样式) + 确定后自动开既有管理台 + 0 退出 (双击友好);
            // 否则维持既有错误框语义 (FIX-14/ADR-8)。
            if crate::fleet::another_serialhub_running(probe_addr) {
                already_running_notice(probe_addr);
                std::process::exit(0);
            }
            fatal_msgbox(&e);
            return Err(e);
        }
        Err(_) => {
            fatal_msgbox("服务线程 15s 内未就绪");
            return Err("服务线程 15s 内未就绪".into());
        }
    };

    // —— 主窗口: WebView 内嵌现有控制台 ——
    // FR-15: 窗口图标 = app.ico 内最大 PNG (构建期解码); 资产缺失 → 程序画的圆点
    // ADR-22①: 窗口几何恢复 —— fleet.json [window] 段 (上次真实退出时写入)。
    // 恢复前夹紧到本次实际接驳的显示器 (拔显示器/改主屏防丢, 见 clamp_window_rect);
    // --no-fleet / 无段 / 段坏 / 尺寸防呆不过 → 内置默认 (1120×780, 系统摆位)。
    let saved_win = match fleet_path_gui.as_deref().map(crate::fleet::load_window) {
        Some(Ok(w)) => w,
        Some(Err(e)) => {
            eprintln!("serialhub: [window] 段读取失败 (忽略, 按默认窗口启动): {e}");
            None
        }
        None => None,
    };
    let restored = saved_win.filter(|r| r.w >= 320 && r.h >= 240); // 防呆: 坏段整段弃用
    let monitors: Vec<(i32, i32, i32, i32)> = event_loop
        .available_monitors()
        .map(|m| {
            let p = m.position();
            let s = m.size();
            (p.x, p.y, s.width as i32, s.height as i32)
        })
        .collect();
    let primary_rect = event_loop
        .primary_monitor()
        .or_else(|| event_loop.available_monitors().next())
        .map(|m| {
            let p = m.position();
            let s = m.size();
            (p.x, p.y, s.width as i32, s.height as i32)
        });
    let mut wb = WindowBuilder::new()
        .with_title("SerialHub")
        .with_inner_size(tao::dpi::LogicalSize::new(1120.0, 780.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(640.0, 480.0))
        .with_window_icon(Some(tao_icon_any(
            crate::icons::WINDOW_ICON_RGBA,
            DOT_CLOSED,
        )));
    if let Some(rec) = restored {
        let (x, y, w, h) = clamp_window_rect(
            rec.x,
            rec.y,
            rec.w as i32,
            rec.h as i32,
            &monitors,
            primary_rect,
        );
        wb = wb
            .with_position(tao::dpi::PhysicalPosition::new(x, y))
            .with_inner_size(tao::dpi::PhysicalSize::new(w as u32, h as u32));
        if rec.maximized {
            // 最大化单独记: 先摆好还原态几何, 再按段值直接最大化创建
            // (用户取消最大化时落回正确位置, 不会顶满全屏尺寸)
            wb = wb.with_maximized(true);
        }
    }
    let window = wb
        .build(&event_loop)
        .map_err(|e| format!("创建窗口失败: {e}"))?;

    let webview = match wry::WebViewBuilder::new()
        .with_url(format!("http://{addr}/"))
        // FR-9a: 壳标记 —— 页面据此判断"运行在桌面壳内" (可自我重启);
        // 浏览器打开同一页面无此标记, /api/restart 控件自动禁用。
        .with_initialization_script("window.__SERIALHUB_SHELL = true;")
        .build(&window)
    {
        Ok(w) => w,
        Err(e) => {
            let msg = format!("创建 WebView 失败 (缺 WebView2 运行时?): {e}");
            fatal_msgbox(&msg);
            return Err(msg);
        }
    };

    // —— 托盘 ——
    let menu = Menu::new();
    let mi_show = MenuItem::with_id(ID_SHOW, "显示主窗口", true, None);
    let mi_browser = MenuItem::with_id(ID_BROWSER, "在浏览器打开控制台", true, None);
    let mi_open = MenuItem::with_id(ID_OPEN, "打开串口", true, None);
    let mi_close = MenuItem::with_id(ID_CLOSE, "关闭串口", true, None);
    let mi_quit = MenuItem::with_id(ID_QUIT, "退出", true, None);
    menu.append(&mi_show).map_err(mstr)?;
    menu.append(&mi_browser).map_err(mstr)?;
    menu.append(&PredefinedMenuItem::separator())
        .map_err(mstr)?;
    menu.append(&mi_open).map_err(mstr)?;
    menu.append(&mi_close).map_err(mstr)?;
    menu.append(&PredefinedMenuItem::separator())
        .map_err(mstr)?;
    menu.append(&mi_quit).map_err(mstr)?;

    // FIX-12 三修: szTip 用静态文案且运行期绝不修改 —— UIA Name 拼接只发生在
    // "字符串可变"上 (NIM_MODIFY 换 szTip 会拼出 "旧 新"), 字符串恒定则无从发生。
    // 相位只用图标图像表达 (NIM_MODIFY 仅换 hIcon, UX 已验证该路径三色切换可靠)。
    // FR-15: 图像源 = tray-closed.png 等交付资产 (构建期解码), 缺失 → 程序画的圆点。
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false) // 左键单击 = 显示主窗口 (右键 = 菜单)
        .with_tooltip(format!("SerialHub · {} · 状态见控制台", port0))
        .with_icon(tray_icon_any(crate::icons::TRAY_CLOSED_RGBA, DOT_CLOSED))
        .build()
        .map_err(mstr)?;

    // 菜单事件 → 事件循环 (muda 全局 handler, 回调在主线程触发)
    let p_menu = proxy.clone();
    MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
        let ue = match ev.id.0.as_str() {
            ID_SHOW => UserEvent::ShowWindow,
            ID_BROWSER => UserEvent::OpenBrowser,
            ID_OPEN => UserEvent::OpenPort,
            ID_CLOSE => UserEvent::ClosePort,
            ID_QUIT => UserEvent::Quit,
            _ => return,
        };
        let _ = p_menu.send_event(ue);
    }));
    // 托盘图标事件 → 是否恢复主窗口 (ADR-21③: 仅左键; 判定抽成纯函数以便单测)
    let p_click = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |ev: TrayIconEvent| {
        if tray_event_restores_window(&ev) {
            let _ = p_click.send_event(UserEvent::ShowWindow);
        }
    }));

    // —— 事件循环 (主线程; 绝不 await/block_on, 见模块头) ——
    let mut hidden_notified = false; // 首次关窗气泡只提示一次 (FR-8)
    let mut cur_addr = addr; // FR-13: 随 AddrChanged 更新 (OpenBrowser 用现值)
    let mut quitting = false;
    let mut quit_deadline: Option<Instant> = None;
    // ADR-22①: 运行期几何跟踪 (还原态几何 + 最大化旗标) + 去抖提交。
    // 真机实测: 最大化时几何事件先于 WS_MAXIMIZE 样式位到达, 事件当口查询
    // is_maximized() 仍是 false, 即时采样会把最大化矩形误记为还原态。
    // 因此采样只进 pending, 需静默 300ms (无新几何事件) 且当前既非最大化
    // 也非最小化才落账 win_geom —— 过渡态候选必被丢弃。
    // 初值: 有恢复段用段值 —— 按最大化创建时实测到的是最大化矩形, 不是还原态。
    let (mut win_geom, mut win_maximized) = match restored {
        Some(rec) => ((rec.x, rec.y, rec.w, rec.h), rec.maximized),
        None => {
            let p = window
                .outer_position()
                .unwrap_or(tao::dpi::PhysicalPosition::new(0, 0));
            let s = window.inner_size();
            ((p.x, p.y, s.width, s.height), window.is_maximized())
        }
    };
    let mut pending_geom: Option<(i32, i32, u32, u32)> = None;
    let mut geom_commit_at: Option<Instant> = None;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        // ADR-22①: 有待提交几何时保证去抖计时器活着 —— 每次进闭包 Wait 都被
        // 复位, 必须重新抬成 WaitUntil; 已到点 (过去时刻) 则立即唤醒。
        // (下一行的 Quit/Poll 臂在 quitting 时会再改写回 Poll, 不受影响)
        if !quitting {
            if let Some(t) = geom_commit_at {
                *control_flow = ControlFlow::WaitUntil(t);
            }
        }
        match event {
            Event::UserEvent(ue) => match ue {
                UserEvent::PhaseChanged { phase, port } => {
                    // FIX-12 三修: 相位只经图标图像表达 —— NIM_MODIFY 仅换 hIcon
                    // (三色切换路径 UX 已实测可靠), szTip/条目身份终生不变。
                    // port 供日志/未来使用; UIA Name 恒为静态文案, 拼接无从发生。
                    // FR-15: 托盘三态 = tray-open/retry/closed.png, 仅换图 (口径同上)。
                    let _ = port;
                    let (src, fallback) = match phase {
                        Phase::Open => (crate::icons::TRAY_OPEN_RGBA, DOT_OPEN),
                        Phase::Opening | Phase::Retry => (crate::icons::TRAY_RETRY_RGBA, DOT_RETRY),
                        Phase::Closed => (crate::icons::TRAY_CLOSED_RGBA, DOT_CLOSED),
                    };
                    let _ = tray.set_icon(Some(tray_icon_any(src, fallback)));
                }
                UserEvent::AddrChanged { addr } => {
                    // FR-13 壳侧跟随闭环: 管理台原地换绑成功 → webview 导航新地址
                    // (浏览器页侧跟随是前端的事; 串口会话/托盘/窗口全程不动)
                    cur_addr = addr;
                    if let Err(e) = webview.load_url(&format!("http://{addr}/")) {
                        eprintln!("serialhub: webview 跟随新地址失败: {e}");
                    }
                }
                UserEvent::ShowWindow => {
                    window.set_visible(true);
                    window.set_focus();
                }
                UserEvent::OpenPort => {
                    let _ = cmd_tx.send(HubCmd::Open);
                }
                UserEvent::ClosePort => {
                    let _ = cmd_tx.send(HubCmd::Close);
                }
                UserEvent::OpenBrowser => {
                    crate::browser::open_url(&format!("http://{cur_addr}/"));
                }
                UserEvent::Stopped => {
                    // 服务清理完毕 (串口线程已退出), 真正结束进程。
                    // ADR-22①: 所有真实退出 (托盘退出/网页停机/Ctrl-C) 都汇入本事件,
                    // 窗口几何在此落盘一次; 关窗到托盘不是退出, 不写。
                    // 落盘前用掉未提交的新鲜候选 (去抖没来得及落账的最后一次
                    // 移动/改尺寸): 此刻窗口状态已稳定, 若读数非还原态则仍用旧值,
                    // 过渡态候选天然被跳过。
                    let geom = if !window.is_minimized() && !window.is_maximized() {
                        pending_geom.take().unwrap_or(win_geom)
                    } else {
                        win_geom
                    };
                    save_window_on_exit(&fleet_path_gui, geom, win_maximized);
                    let _ = tray.set_visible(false);
                    *control_flow = ControlFlow::Exit;
                }
                UserEvent::Quit => {
                    let _ = shutdown_tx.send(true);
                    quitting = true;
                    quit_deadline = Some(Instant::now() + Duration::from_secs(3));
                    *control_flow = ControlFlow::Poll; // 保持轮询等 Stopped / 超时兜底
                }
            },
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                // 关窗 = 退到托盘, 桥继续跑 (FR-8); 首次气泡提示一次
                // (气泡 = Win32 Shell_NotifyIconW 特性; tray.window_handle() 是
                //  tray-icon 的 Windows 专属扩展方法, CI 三平台要求非 Windows
                //  仅可编译 —— 整个调用随 balloon 一起平台门控, PLAT-4)
                window.set_visible(false);
                if !hidden_notified {
                    hidden_notified = true;
                    #[cfg(windows)]
                    balloon(
                        tray.window_handle(),
                        "SerialHub",
                        "已退到系统托盘继续运行, 右键托盘图标可退出。",
                    );
                }
            }
            Event::WindowEvent {
                event: WindowEvent::Resized(_) | WindowEvent::Moved(_),
                ..
            } => {
                // wry 0.57 build(&window) 已随窗口自适应, 此处仅保底触发一次重排
                let _ = webview.bounds();
                // ADR-22①: 几何采样只进 pending (去抖 300ms 后由唤醒臂裁决提交,
                // 见 GEOM_DEBOUNCE 与 NewEvents(Poll|ResumeTimeLost) 臂注释);
                // 事件当口不查 is_maximized —— 过渡期它滞后, 查了也不可信。
                if let Ok(p) = window.outer_position() {
                    let s = window.inner_size();
                    pending_geom = Some((p.x, p.y, s.width, s.height));
                }
                let commit_at = Instant::now() + GEOM_DEBOUNCE;
                geom_commit_at = Some(commit_at);
                if !quitting {
                    *control_flow = ControlFlow::WaitUntil(commit_at);
                }
            }
            Event::NewEvents(
                StartCause::Poll
                | StartCause::ResumeTimeReached { .. }
                | StartCause::WaitCancelled { .. },
            ) => {
                // ADR-22①: 去抖到点 —— 窗口状态自最后一个几何事件起已静默
                // GEOM_DEBOUNCE, 此时读 is_maximized/is_minimized 可信 (过渡最多
                // 毫秒级): 还原态 → 候选落账; 最大化/最小化 → 候选丢弃
                // (win_geom 保持上次正常值), 旗标按现值翻。
                if let Some(t) = geom_commit_at {
                    if Instant::now() >= t {
                        geom_commit_at = None;
                        let (minimized, maximized) = (window.is_minimized(), window.is_maximized());
                        if minimized || maximized {
                            pending_geom = None;
                            if !minimized {
                                win_maximized = maximized;
                            }
                        } else if let Some(g) = pending_geom.take() {
                            win_geom = g;
                            win_maximized = false;
                        }
                    } else if !quitting {
                        // 极端: 被更早的其他事件唤醒, 计时未到 → 重新武装
                        *control_flow = ControlFlow::WaitUntil(t);
                    }
                    // quitting 且未到点: 保持 Poll 高频轮询, 下次到点再裁决
                }
                // 退出兜底: Stopped 事件 3s 内未到 (异常) 也强制退出
                if quitting && quit_deadline.is_none_or(|d| Instant::now() >= d) {
                    // ADR-22①: 兜底退出同样是真实退出, 几何照写
                    save_window_on_exit(&fleet_path_gui, win_geom, win_maximized);
                    let _ = tray.set_visible(false);
                    *control_flow = ControlFlow::Exit;
                } else if quitting {
                    *control_flow = ControlFlow::Poll;
                }
            }
            _ => {}
        }
    });
}

// ---------------- 小工具 ----------------

fn mstr(e: impl std::fmt::Display) -> String {
    format!("托盘/菜单初始化失败: {e}")
}

type Rgba = (u8, u8, u8);
const DOT_OPEN: Rgba = (30, 142, 78); // 绿
const DOT_RETRY: Rgba = (192, 120, 0); // 琥珀
const DOT_CLOSED: Rgba = (124, 135, 145); // 灰

/// 程序内生成的 32×32 圆点图标: 实心彩色核心 + 深色描边环 (FIX-13, 浅色任务栏上
/// 可辨识) + 抗锯齿外缘, 不依赖美术资源 (FR-8)。
fn dot_rgba((r, g, b): Rgba) -> Vec<u8> {
    const OUTLINE: (u8, u8, u8) = (28, 38, 48); // 与 --ink 同源的深描边
    let mut rgba = Vec::with_capacity(32 * 32 * 4);
    for y in 0..32u32 {
        for x in 0..32u32 {
            let dx = x as f32 - 15.5;
            let dy = y as f32 - 15.5;
            let d = (dx * dx + dy * dy).sqrt();
            let a = ((15.5 - d) * 40.0).clamp(0.0, 255.0) as u8; // 抗锯齿外缘
            let (cr, cg, cb) = if d < 12.0 { (r, g, b) } else { OUTLINE };
            rgba.extend_from_slice(&[cr, cg, cb, a]);
        }
    }
    rgba
}

fn tray_icon(c: Rgba) -> tray_icon::Icon {
    tray_icon::Icon::from_rgba(dot_rgba(c), 32, 32).expect("固定 32×32 RGBA 不会失败")
}

fn tao_icon(c: Rgba) -> TaoIcon {
    TaoIcon::from_rgba(dot_rgba(c), 32, 32).expect("固定 32×32 RGBA 不会失败")
}

/// FR-15: 优先用构建期解码的交付图标 (原始 RGBA); 尺寸/长度不符或资产缺失 →
/// 程序画的圆点回退。托盘与窗口共用该装配口径。
fn tray_icon_any(src: Option<crate::icons::RgbaIcon>, fallback: Rgba) -> tray_icon::Icon {
    if let Some((bytes, w, h)) = src {
        if bytes.len() == (w as usize) * (h as usize) * 4 {
            if let Ok(icon) = tray_icon::Icon::from_rgba(bytes.to_vec(), w, h) {
                return icon;
            }
        }
    }
    tray_icon(fallback)
}

fn tao_icon_any(src: Option<crate::icons::RgbaIcon>, fallback: Rgba) -> TaoIcon {
    if let Some((bytes, w, h)) = src {
        if bytes.len() == (w as usize) * (h as usize) * 4 {
            if let Ok(icon) = TaoIcon::from_rgba(bytes.to_vec(), w, h) {
                return icon;
            }
        }
    }
    tao_icon(fallback)
}

/// 首次关窗气泡 (FR-8): tray-icon 无气泡 API, 直接对托盘图标 NIM_MODIFY + NIF_INFO。
/// uID=1 依据: tray-icon 的内部 id 计数器从 1 起, 本进程只创建一个托盘图标。
#[cfg(windows)]
fn balloon(hwnd: windows_sys::Win32::Foundation::HWND, title: &str, text: &str) {
    use windows_sys::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_INFO, NIIF_INFO, NIM_MODIFY, NOTIFYICONDATAW,
    };
    fn put_wide(dst: &mut [u16], s: &str) {
        let mut w: Vec<u16> = s.encode_utf16().take(dst.len().saturating_sub(1)).collect();
        w.push(0);
        dst[..w.len()].copy_from_slice(&w);
    }
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_INFO;
    put_wide(&mut nid.szInfoTitle, title);
    put_wide(&mut nid.szInfo, text);
    nid.dwInfoFlags = NIIF_INFO;
    unsafe {
        Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

// (balloon 无非 Windows 空壳: 唯一调用点已随 tray.window_handle() 一起平台门控)

/// FIX-14 (ADR-8): GUI 早期失败 (端口被占用/初始化失败) 用系统 MessageBox 明示 ——
/// 双击启动无控制台, stderr 不可见; 这是错误对话框, 允许抢占注意力 (与主窗口不同)。
#[cfg(windows)]
fn fatal_msgbox(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND,
    };
    fn wide(s: &str) -> Vec<u16> {
        let mut w: Vec<u16> = s.encode_utf16().collect();
        w.push(0);
        w
    }
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(text).as_ptr(),
            wide("SerialHub 启动失败").as_ptr(),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

#[cfg(not(windows))]
fn fatal_msgbox(_text: &str) {}

/// FR-16: 第二实例探测到已有 SerialHub 在跑 —— 信息框 (非错误样式: 信息图标,
/// 标题不带「启动失败」), 用户点确定后浏览器打开既有管理台, 调用方随后 0 退出。
/// 非 Windows 没有本项目的对话框路径, 直接开浏览器 (0 退出语义不变)。
#[cfg(windows)]
fn already_running_notice(addr: SocketAddr) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND,
    };
    fn wide(s: &str) -> Vec<u16> {
        let mut w: Vec<u16> = s.encode_utf16().collect();
        w.push(0);
        w
    }
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(&format!("SerialHub 已在运行\n管理台: http://{addr}")).as_ptr(),
            wide("SerialHub").as_ptr(),
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
        );
    }
    crate::browser::open_url(&format!("http://{addr}/"));
}

#[cfg(not(windows))]
fn already_running_notice(addr: SocketAddr) {
    crate::browser::open_url(&format!("http://{addr}/"));
}

/// ADR-21③: 托盘事件是否应恢复主窗口 —— 仅**左键**的单击/双击 (DoubleClick 同属
/// 左键恢复语义)。右键/中键不触碰窗口: 右键菜单是 tray-icon 内建弹出
/// (with_menu_on_left_click(false)), 判定不在这里做。
///
/// 病根 (用户实测): 此前 Click 不分键, 而 Windows 下 WM_RBUTTONDOWN/UP 都会发
/// `Click { button: Right }` —— 右键弹菜单的瞬间主窗口被拉起抢走焦点, 菜单即逝。
/// 抽成纯函数以便单测 (handler 本体在主线程 UI 事件回调里, 无法集成测试)。
fn tray_event_restores_window(ev: &TrayIconEvent) -> bool {
    matches!(
        ev,
        TrayIconEvent::Click {
            button: MouseButton::Left,
            ..
        } | TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        }
    )
}

// ---------------- 窗口几何记忆 (ADR-22①) ----------------

/// 真实退出时把跟踪到的窗口几何写入 fleet.json [window] 段。
/// 调用点只有两个: UserEvent::Stopped (托盘退出/网页停机/Ctrl-C 的共同汇点)
/// 与 3s 停机兜底 —— 关窗到托盘 (FR-8, 桥继续跑) 不是退出, 不经此处;
/// headless 形态不进本模块; --no-fleet (路径 None) 为空操作。
fn save_window_on_exit(
    fleet_path: &Option<std::path::PathBuf>,
    geom: (i32, i32, u32, u32),
    maximized: bool,
) {
    let Some(path) = fleet_path else { return };
    let rec = crate::fleet::WindowRec {
        x: geom.0,
        y: geom.1,
        w: geom.2,
        h: geom.3,
        maximized,
    };
    if let Err(e) = crate::fleet::save_window(path, rec) {
        eprintln!("serialhub: 窗口几何写入 fleet.json 失败: {e}");
    }
}

/// 夹紧判定里"还算可见"的最小交叠 (物理像素): 宽 ≥ 标题条按钮区、高 ≥ 标题条,
/// 用户能抓着拖回来就算没丢。
const MIN_VISIBLE_W: i32 = 120;
const MIN_VISIBLE_H: i32 = 60;

/// ADR-22①: 窗口几何去抖窗口 —— 最后一个几何事件后静默此时长才裁决提交。
/// 静默期内读 is_maximized/is_minimized 已可信 (最大化样式位过渡是毫秒级),
/// 过渡态采样 (矩形先到、旗标后到) 在到点裁决时必被判为最大化/最小化而丢弃。
const GEOM_DEBOUNCE: Duration = Duration::from_millis(300);

/// ADR-22①: 记忆几何夹紧 —— 防拔显示器/改主屏后窗口丢在屏外拉不回来。
/// 规则: 矩形与任一当前显示器交叠 ≥ (MIN_VISIBLE_W, MIN_VISIBLE_H) → 原样保留
/// (多显示器布局不被强行拉回主屏); 否则整体夹进主屏 (w/h 超屏缩到屏内,
/// x/y 贴边内移)。拿不到显示器信息 (monitors 空 / primary None) → 原样返回
/// (无法判断就不越权)。纯函数, 单测见 tests::clamp_window_rect_*。
fn clamp_window_rect(
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    monitors: &[(i32, i32, i32, i32)], // 每项 (x, y, w, h), 物理像素
    primary: Option<(i32, i32, i32, i32)>,
) -> (i32, i32, i32, i32) {
    let visible = monitors.iter().any(|&(mx, my, mw, mh)| {
        let ow = (x + w).min(mx + mw) - x.max(mx);
        let oh = (y + h).min(my + mh) - y.max(my);
        ow >= MIN_VISIBLE_W && oh >= MIN_VISIBLE_H
    });
    if visible {
        return (x, y, w, h);
    }
    let Some((px, py, pw, ph)) = primary else {
        return (x, y, w, h);
    };
    // 尺寸下限与 with_min_inner_size (640×480 逻辑) 同量级; 高 DPI 下 tao 会再
    // 放大到物理最小值, 极端"小屏+高缩放"组合可能仍溢出边缘, 属可接受近似。
    let cw = w.clamp(640.min(pw), pw);
    let ch = h.clamp(480.min(ph), ph);
    (x.clamp(px, px + pw - cw), y.clamp(py, py + ph - ch), cw, ch)
}

// ---------------- 单测 (ADR-21③: 托盘分键 / ADR-22①: 几何夹紧) ----------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_keeps_window_on_visible_secondary_monitor() {
        // 主屏 1920×1080 @(0,0) + 副屏 2560×1440 @(1920,0): 窗口整体在副屏上
        // → 原样保留 (多显示器布局不得被拉回主屏)
        let monitors = vec![(0, 0, 1920, 1080), (1920, 0, 2560, 1440)];
        assert_eq!(
            clamp_window_rect(2400, 300, 1000, 640, &monitors, Some((0, 0, 1920, 1080))),
            (2400, 300, 1000, 640)
        );
        // 跨主/副屏但主体可见 (交叠远超阈值) → 同样保留
        assert_eq!(
            clamp_window_rect(1800, 100, 600, 400, &monitors, Some((0, 0, 1920, 1080))),
            (1800, 100, 600, 400)
        );
    }

    #[test]
    fn clamp_pulls_unplugged_monitor_window_back_to_primary() {
        // 记忆位置在已拔掉的右侧副屏 (1920,0,2560,1440) 上 → 整体夹回主屏内
        let monitors = vec![(0, 0, 1920, 1080)];
        let got = clamp_window_rect(2400, 300, 1000, 640, &monitors, Some((0, 0, 1920, 1080)));
        let (x, y, w, h) = got;
        assert_eq!((w, h), (1000, 640), "尺寸不超屏则不动");
        assert!(x >= 0 && x + w <= 1920, "x 夹进主屏: {got:?}");
        assert!(y >= 0 && y + h <= 1080, "y 夹进主屏: {got:?}");
        // 右下角负方向记忆 (副屏在左/上方的布局) 同样拉回
        let got = clamp_window_rect(-1500, -200, 800, 500, &monitors, Some((0, 0, 1920, 1080)));
        assert_eq!(got, (0, 0, 800, 500));
    }

    #[test]
    fn clamp_oversized_window_fits_screen_and_borderline_visibility() {
        let m = vec![(0, 0, 1920, 1080)];
        // 巨窗 + 记忆点在屏外 (换小屏后完全不可见) → 缩到屏内, 位置归零
        assert_eq!(
            clamp_window_rect(3000, 2000, 5000, 3000, &m, Some((0, 0, 1920, 1080))),
            (0, 0, 1920, 1080)
        );
        // 只露出 50px 窄条 (< MIN_VISIBLE_W=120) → 算丢, 拉回 (尺寸过小时抬到
        // 窗口最小尺寸 640 宽, 与 with_min_inner_size 同量级)
        assert_eq!(
            clamp_window_rect(1870, 0, 400, 500, &m, Some((0, 0, 1920, 1080))),
            (1280, 0, 640, 500)
        );
        // 恰好露出阈值宽度 (120px) → 保留 (用户还抓得住)
        assert_eq!(
            clamp_window_rect(1800, 0, 400, 500, &m, Some((0, 0, 1920, 1080))),
            (1800, 0, 400, 500)
        );
        // 高度不够 (只露 30px < MIN_VISIBLE_H=60) → 拉回
        assert_eq!(
            clamp_window_rect(100, 1050, 400, 500, &m, Some((0, 0, 1920, 1080))),
            (100, 580, 640, 500)
        );
    }

    #[test]
    fn clamp_without_monitor_info_is_identity() {
        // 拿不到显示器列表/主屏 (异常平台) → 不越权, 原样返回
        assert_eq!(
            clamp_window_rect(99, -5, 800, 600, &[], None),
            (99, -5, 800, 600)
        );
        assert_eq!(
            clamp_window_rect(99, -5, 800, 600, &[(0, 0, 1920, 1080)], None),
            (99, -5, 800, 600)
        );
    }

    use tray_icon::dpi::PhysicalPosition;
    use tray_icon::{MouseButtonState, Rect, TrayIconId};

    fn click(button: MouseButton, state: MouseButtonState) -> TrayIconEvent {
        TrayIconEvent::Click {
            id: TrayIconId::new("1"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect::default(),
            button,
            button_state: state,
        }
    }

    fn dblclick(button: MouseButton) -> TrayIconEvent {
        TrayIconEvent::DoubleClick {
            id: TrayIconId::new("1"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect::default(),
            button,
        }
    }

    #[test]
    fn tray_left_click_restores_window() {
        // 左键按下/抬起都算恢复语义 (Windows 两个消息都发 Click)
        assert!(tray_event_restores_window(&click(
            MouseButton::Left,
            MouseButtonState::Down
        )));
        assert!(tray_event_restores_window(&click(
            MouseButton::Left,
            MouseButtonState::Up
        )));
        assert!(tray_event_restores_window(&dblclick(MouseButton::Left)));
    }

    #[test]
    fn tray_right_and_other_keys_never_touch_window() {
        // 右键 (按下/抬起) 只属于内建菜单, 不得拉起主窗口 (ADR-21③ 病根回归)
        assert!(!tray_event_restores_window(&click(
            MouseButton::Right,
            MouseButtonState::Down
        )));
        assert!(!tray_event_restores_window(&click(
            MouseButton::Right,
            MouseButtonState::Up
        )));
        assert!(!tray_event_restores_window(&dblclick(MouseButton::Right)));
        // 中键 / 悬停类事件同样不触碰窗口
        assert!(!tray_event_restores_window(&click(
            MouseButton::Middle,
            MouseButtonState::Up
        )));
        assert!(!tray_event_restores_window(&TrayIconEvent::Enter {
            id: TrayIconId::new("1"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect::default(),
        }));
        assert!(!tray_event_restores_window(&TrayIconEvent::Leave {
            id: TrayIconId::new("1"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect::default(),
        }));
    }
}
