//! SerialHub 入口: CLI → HubState → 监督任务 → axum (HTTP+WS)。
//!
//! 线程/任务模型一览:
//! - 数据面: 2 条 OS 线程 (串口读/写, 见 serial.rs) + N 个 WS 客户端任务;
//!   RX 经 broadcast 扇出, TX 经 mpsc 队列单写者串行;
//! - 控制面: 监督任务 (状态机/自动重开) + axum 任务组 (/api/*, /ws);
//! - 所有共享状态集中在 HubState + PortCtx, 无第二份真相。

mod api;
mod cli;
mod config;
mod hub;
mod serial;
mod supervisor;

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

use config::SerialConfig;
use serial::PortCtx;
use supervisor::HubCmd;

/// broadcast 环形缓冲条数。慢客户端超过此量会被判 Lagged 丢旧帧 (不阻塞读线程)。
const BROADCAST_CAP: usize = 1024;

#[tokio::main]
async fn main() {
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

    let cfg = SerialConfig {
        port: cli.port.clone().unwrap_or_default(),
        baud: cli.baud,
        data_bits: cli.data_bits,
        parity: cli.parity,
        stop_bits: cli.stop_bits,
        flow: config::Flow::None,
    };
    let hub = Arc::new(hub::HubState::new(cfg));
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<HubCmd>();
    let (bc_tx, _) = broadcast::channel::<Vec<u8>>(BROADCAST_CAP);
    let ctx = PortCtx {
        hub: hub.clone(),
        bc_tx: bc_tx.clone(),
        tx_slot: Arc::new(std::sync::Mutex::new(None)),
    };

    // 监督任务: 1s 重试间隔 (FR-3), 独立于数据面
    tokio::spawn(supervisor::run_supervisor(
        ctx.clone(),
        cmd_rx,
        Arc::new(serial::RealOpener),
        Duration::from_secs(1),
    ));

    if cli.auto_open() {
        let _ = cmd_tx.send(HubCmd::Open);
    }

    let app = api::App {
        ctx,
        cmd_tx,
        index: include_str!("../ui/index.html"),
    };

    let listener = match TcpListener::bind(cli.addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("serialhub: 监听 {} 失败: {e}", cli.addr);
            std::process::exit(1);
        }
    };
    println!("SerialHub 就绪: http://{}  (Ctrl+C 退出)", cli.addr);
    if let Some(p) = &cli.port {
        println!(
            "  串口: {p} @ {} {}{}{} (自动打开: {})",
            cli.baud,
            cli.data_bits,
            cli.parity.as_char(),
            cli.stop_bits,
            if cli.no_open { "否" } else { "是" }
        );
    } else {
        println!("  未指定 --port, 启动为未打开态, 请在 Web 控制台选择串口");
    }

    let server = axum::serve(listener, api::router(app))
        .with_graceful_shutdown(shutdown_signal());
    if let Err(e) = server.await {
        eprintln!("serialhub: 服务异常退出: {e}");
        std::process::exit(1);
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
