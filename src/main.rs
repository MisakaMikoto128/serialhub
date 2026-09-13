//! SerialHub 入口 (FR-8): CLI 解析 → 分派 GUI / headless 两种形态。
//!
//! - GUI (默认): gui::run_gui —— 主线程 tao 事件循环 + wry WebView + 托盘,
//!   tokio 服务在后台线程 (整合方式与坑见 gui.rs 模块头注释);
//! - --headless: 纯 CLI 前台 (自动化测试与脚本场景, PLAT-4 无 GUI 依赖);
//! - Sprint 4 (FR-10/ADR-13): 两种形态都走 fleet::run_manager 多桥管理器;
//!   字节通路与并发模型见 serial.rs / supervisor.rs 顶部注释。

mod api;
mod cli;
mod config;
mod fleet;
mod gui;
mod hub;
mod icons;
mod serial;
mod service;
mod stats;
mod supervisor;
mod themes;

use cli::Cli;
use supervisor::HubCmd;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{}", cli::USAGE);
        return;
    }
    let cli = match cli::parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("serialhub: {e}\n\n{}", cli::USAGE);
            std::process::exit(2);
        }
    };

    if cli.list_ports {
        let ports = serial::list_ports();
        if ports.is_empty() {
            println!("(未发现串口)");
        }
        for (name, desc) in ports {
            if desc.is_empty() {
                println!("{name}");
            } else {
                println!("{name}\t{desc}");
            }
        }
        return;
    }

    if cli.gui {
        // GUI 模式: run_gui 只在"窗口创建之前"的失败路径返回 (如端口被占用);
        // 事件循环启动后进程由事件循环接管。
        if let Err(e) = gui::run_gui(cli) {
            eprintln!("serialhub: {e}");
            std::process::exit(1);
        }
        unreachable!("tao 事件循环不返回");
    }

    // headless 模式: 进程内 tokio 主任务 + Ctrl-C 优雅停机
    if let Err(e) = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(headless_service(cli))
    {
        eprintln!("serialhub: {e}");
        std::process::exit(1);
    }
}

async fn headless_service(cli: Cli) -> Result<(), String> {
    let startup = fleet::ManagerStartup::from_cli(&cli);
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<HubCmd>();
    let (shutdown_tx, _) = tokio::sync::watch::channel(false);
    // 控制面 (管理台) bind 失败时 run_manager 先失败, 兼容桥不会创建 (不开串口直接退出)
    fleet::run_manager(startup, cmd_tx, cmd_rx, shutdown_tx, None).await
}
