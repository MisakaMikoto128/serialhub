//! CLI 解析 (FR-5)。
//!
//! 为什么手写而不用 clap: 一共 6 个参数, 手写省一个重依赖 (首编译快);
//! 校验规则全部复用 config.rs 的纯函数, 与 /api/config 完全同一套,
//! 不会出现"CLI 能配 Web 不能配"的分叉。禁止写死任何 COM 号 —— 一切经参数。

use std::net::SocketAddr;

use crate::config::{parse_config_str, validate_baud, Flow, Parity, SerialConfig};
use crate::service::Startup;

pub const USAGE: &str = "\
SerialHub — 串口 <-> WebSocket 桥接 (内嵌 Web 控制台)

用法:
  serialhub [选项]

选项:
  --port <名称>      串口名, 如 COM1 / /dev/ttyUSB0 (缺省 = 启动为未打开态, 由控制台驱动)
  --baud <数值>      波特率, 110..=2000000 (默认 115200)
  --config <8N2>     数据位/校验/停止位, 如 8N2、7E1、7O1 (默认 8N2)
  --addr <ip:port>   HTTP/WS 监听地址 (默认 127.0.0.1:8080)
  --list-ports       列出本机串口后退出
  --no-open          给了 --port 也不自动打开 (仍作为控制台的默认参数)
  --max-clients <n>  最大 WS 客户端数, 0 = 不限 (默认 0; 超限新连接以 close 1013 拒绝, FR-9b)
  --flow <模式>      流控: none / rtscts / xonxoff (默认 none; 与 /api/config 同一套校验, ADR-10)
  --headless         纯 CLI 前台模式: 无窗口无托盘 (自动化测试与脚本场景, FR-8)
  --gui              原生窗口 + 托盘模式 (默认; 与 --headless 互斥)
  -h, --help         显示本帮助

示例:
  serialhub --port COM1 --baud 115200 --config 8N2 --addr 127.0.0.1:8080
";

#[derive(Debug, Clone)]
pub struct Cli {
    pub port: Option<String>,
    pub baud: u32,
    pub data_bits: u8,
    pub parity: Parity,
    pub stop_bits: u8,
    pub addr: SocketAddr,
    pub list_ports: bool,
    pub no_open: bool,
    /// FR-8: 默认 GUI 模式 (原生窗口 + 托盘); --headless 退回纯 CLI 前台。
    pub gui: bool,
    /// FR-9b: 最大 WS 客户端数, 0 = 不限 (默认)。
    pub max_clients: u32,
    /// ADR-10: 流控 (CLI 与 /api/config 同一套校验)。
    pub flow: Flow,
}

impl Cli {
    /// FR-5: 给了 --port 且未 --no-open 才自动打开。
    pub fn auto_open(&self) -> bool {
        self.port.is_some() && !self.no_open
    }

    /// 派生服务启动参数 (GUI / headless 共用, FR-8)。
    pub fn startup(&self) -> Startup {
        Startup {
            cfg: SerialConfig {
                port: self.port.clone().unwrap_or_default(),
                baud: self.baud,
                data_bits: self.data_bits,
                parity: self.parity,
                stop_bits: self.stop_bits,
                flow: Flow::None,
            },
            auto_open: self.auto_open(),
            addr: self.addr,
            max_clients: self.max_clients,
            flow: self.flow,
            gui: self.gui,
        }
    }
}

pub fn parse(args: &[String]) -> Result<Cli, String> {
    let mut port: Option<String> = None;
    let mut baud: u32 = 115200;
    let mut data_bits: u8 = 8;
    let mut parity = Parity::N;
    let mut stop_bits: u8 = 2;
    let mut addr_str = "127.0.0.1:8080".to_string();
    let mut list_ports = false;
    let mut no_open = false;
    let mut headless = false;
    let mut gui_flag = false;
    let mut max_clients: u32 = 0;
    let mut flow = Flow::None;

    let mut i = 0usize;
    while i < args.len() {
        let a = args[i].clone();
        macro_rules! val {
            () => {{
                i += 1;
                if i >= args.len() {
                    return Err(format!("参数 {a} 缺少值"));
                }
                args[i].clone()
            }};
        }
        match a.as_str() {
            "--port" => {
                let v = val!();
                let t = v.trim().to_string();
                if t.is_empty() {
                    return Err("--port 不能为空字符串".into());
                }
                port = Some(t);
            }
            "--baud" => {
                let v = val!();
                baud = v
                    .parse()
                    .map_err(|_| format!("--baud 不是合法数字: \"{v}\""))?;
                validate_baud(baud)?;
            }
            "--config" => {
                let v = val!();
                (data_bits, parity, stop_bits) = parse_config_str(&v)?;
            }
            "--addr" => {
                addr_str = val!();
            }
            "--list-ports" => list_ports = true,
            "--no-open" => no_open = true,
            "--max-clients" => {
                let v = val!();
                max_clients = v
                    .parse()
                    .map_err(|_| format!("--max-clients 不是合法非负整数: \"{v}\""))?;
            }
            "--flow" => {
                let v = val!();
                flow = Flow::parse(&v).map_err(|e| format!("--flow {e}"))?;
            }
            "--headless" => headless = true,
            "--gui" => gui_flag = true,
            other => return Err(format!("未知参数 \"{other}\"")),
        }
        i += 1;
    }

    if headless && gui_flag {
        return Err("--headless 与 --gui 互斥, 只能二选一".into());
    }
    let addr: SocketAddr = addr_str.parse().map_err(|_| {
        format!("--addr 不是合法地址 (形如 127.0.0.1:8080): \"{addr_str}\"")
    })?;

    Ok(Cli {
        port,
        baud,
        data_bits,
        parity,
        stop_bits,
        addr,
        list_ports,
        no_open,
        gui: !headless,
        max_clients,
        flow,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(s: &str) -> Result<Cli, String> {
        let args: Vec<String> = s.split_whitespace().map(|p| p.to_string()).collect();
        parse(&args)
    }

    #[test]
    fn defaults() {
        let c = parse(&[]).unwrap();
        assert_eq!(c.port, None);
        assert_eq!(c.baud, 115200);
        assert_eq!(c.data_bits, 8);
        assert_eq!(c.parity, Parity::N);
        assert_eq!(c.stop_bits, 2);
        assert_eq!(c.addr.to_string(), "127.0.0.1:8080");
        assert!(!c.list_ports);
        assert!(!c.no_open);
        // FR-8: 默认 GUI
        assert!(c.gui);
        // 未给 --port → 不自动打开 (FR-5)
        assert!(!c.auto_open());
    }

    #[test]
    fn full_args_and_auto_open() {
        let c = parse_str("--port COM1 --baud 115200 --config 8N2 --addr 127.0.0.1:8080").unwrap();
        assert_eq!(c.port.as_deref(), Some("COM1"));
        assert_eq!(c.baud, 115200);
        assert_eq!((c.data_bits, c.stop_bits), (8, 2));
        assert_eq!(c.parity, Parity::N);
        assert!(c.auto_open());

        let c2 = parse_str("--port COM1 --no-open").unwrap();
        assert!(!c2.auto_open());

        let c3 = parse_str("--config 7E1").unwrap();
        assert_eq!((c3.data_bits, c3.parity, c3.stop_bits), (7, Parity::E, 1));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_str("--baud").is_err()); // 缺值
        assert!(parse_str("--baud abc").is_err());
        assert!(parse_str("--baud 100").is_err()); // 越界
        assert!(parse_str("--baud 2000001").is_err());
        assert!(parse_str("--config 9N2").is_err());
        assert!(parse_str("--config 8N").is_err());
        assert!(parse_str("--addr not-an-addr").is_err());
        assert!(parse_str("--port").is_err());
        assert!(parse(&["--port".into(), "".into()]).is_err()); // shell 剥引号后的空值
        assert!(parse_str("--wat").is_err());
        assert!(parse_str("COM1").is_err()); // 裸参数不允许
    }

    #[test]
    fn headless_gui_flags() {
        let c = parse_str("--headless").unwrap();
        assert!(!c.gui);
        // FR-9b: 默认 0 = 不限; 显式值透传
        assert_eq!(parse(&[]).unwrap().max_clients, 0);
        let c = parse_str("--max-clients 5").unwrap();
        assert_eq!(c.max_clients, 5);
        let c = parse_str("--max-clients 0").unwrap();
        assert_eq!(c.max_clients, 0);
        assert!(parse_str("--max-clients -1").is_err());
        assert!(parse_str("--max-clients abc").is_err());
        assert!(parse_str("--max-clients").is_err());
        // ADR-10: --flow 解析 (合法/非法; 校验复用 config::Flow::parse)
        let c = parse_str("--flow rtscts").unwrap();
        assert_eq!(c.flow, Flow::RtsCts);
        let c = parse_str("--flow XONXOFF").unwrap();
        assert_eq!(c.flow, Flow::XonXoff);
        let c = parse_str("--port COM1 --flow none").unwrap();
        assert_eq!(c.flow, Flow::None);
        assert!(parse_str("--flow hardware").is_err());
        assert!(parse_str("--flow").is_err());
        let c = parse_str("--gui").unwrap();
        assert!(c.gui);
        let c = parse_str("--port COM1 --headless --baud 115200").unwrap();
        assert!(!c.gui && c.auto_open());
        // 互斥
        assert!(parse_str("--headless --gui").is_err());
        assert!(parse_str("--gui --headless").is_err());
    }

    #[test]
    fn addr_accepts_ipv6() {
        let c = parse_str("--addr [::1]:9000").unwrap();
        assert_eq!(c.addr.to_string(), "[::1]:9000");
    }
}
