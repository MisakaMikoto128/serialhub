//! CLI 解析 (FR-5)。
//!
//! 为什么手写而不用 clap: 一共 6 个参数, 手写省一个重依赖 (首编译快);
//! 校验规则全部复用 config.rs 的纯函数, 与 /api/config 完全同一套,
//! 不会出现"CLI 能配 Web 不能配"的分叉。禁止写死任何 COM 号 —— 一切经参数。

use std::net::SocketAddr;
use std::path::PathBuf;

use crate::config::{parse_config_str, validate_baud, Flow, Parity};

pub const USAGE: &str = "\
SerialHub — 串口 <-> WebSocket 桥接管理器 (内嵌 Web 控制台)

用法:
  serialhub [选项]

选项:
  --port <名称>      串口名, 如 COM1 / /dev/ttyUSB0 (缺省 = 启动为未打开态, 由控制台驱动)
  --baud <数值>      波特率, 110..=2000000 (默认 115200)
  --config <8N2>     数据位/校验/停止位, 如 8N2、7E1、7O1 (默认 8N2)
  --addr <ip:port>   管理台 (控制面) 监听地址, 永远可达 (默认 127.0.0.1:8080)
  --list-ports       列出本机串口后退出
  --no-open          给了 --port 也不自动打开 (仍作为控制台的默认参数)
  --max-clients <n>  最大 WS 客户端数/每桥, 0 = 不限 (默认 0; 超限新连接以 close 1013 拒绝, FR-9b)
  --flow <模式>      流控: none / rtscts / xonxoff (默认 none; 与 /api/config 同一套校验, ADR-10)
  --reconnect        串口断开后自动重连 (FR-12/ADR-16①, 默认开启)
  --no-reconnect     关闭自动重连: 掉线即停 (已停止), 手动打开仍可用
  --fleet <路径>     桥清单文件路径 (默认 %APPDATA%\\SerialHub\\fleet.json; FR-10b)
  --no-fleet         关闭桥清单持久化 (FR-10b)
  --themes-dir <路径> 主题目录 (FR-14, 默认 exe 旁 themes/; 不存在则启动时创建并写入内置主题)
  --headless         纯 CLI 前台模式: 无窗口无托盘 (自动化测试与脚本场景, FR-8)
  --gui              原生窗口 + 托盘模式 (默认; 与 --headless 互斥)
  -h, --help         显示本帮助

说明:
  旧单桥参数 (--port/--baud/...) 等价于自动建一座桥并启动 (FR-10h);
  每座桥的独立数据端口在管理台 (GET/POST /api/fleet) 里新建/启停/改配/删除。

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
    /// FR-10b: 桥清单路径 (None = 用默认路径)。
    pub fleet: Option<PathBuf>,
    /// FR-10b: --no-fleet 关闭持久化。
    pub no_fleet: bool,
    /// FR-12/ADR-16①: 串口断开后自动重连 (默认 true; --no-reconnect 关闭)。
    pub auto_reconnect: bool,
    /// FR-14: 主题目录 (None = exe 旁 themes/, 运行时由 themes::default_themes_dir 兜底)。
    pub themes_dir: Option<PathBuf>,
    /// FR-13: 本次启动是否显式给出 --addr (显式地址优先于 fleet.json [manager] 恢复)。
    pub addr_explicit: bool,
}

impl Cli {
    /// FR-5: 给了 --port 且未 --no-open 才自动打开。
    pub fn auto_open(&self) -> bool {
        self.port.is_some() && !self.no_open
    }
    // (原 Cli::startup —— 派生 legacy 单桥 Startup 的辅助, Sprint 4 FR-10 改走
    //  fleet::ManagerStartup::from_cli 后再无调用方, clippy 清债时删除。)
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
    let mut fleet: Option<PathBuf> = None;
    let mut no_fleet = false;
    let mut auto_reconnect = true; // FR-12: 默认开启自动重连
    let mut themes_dir: Option<PathBuf> = None; // FR-14: 默认 exe 旁 themes/
    let mut addr_explicit = false; // FR-13: --addr 是否显式给出

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
                addr_explicit = true;
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
            "--fleet" => {
                let v = val!();
                let t = v.trim().to_string();
                if t.is_empty() {
                    return Err("--fleet 不能为空字符串".into());
                }
                fleet = Some(PathBuf::from(t));
            }
            "--no-fleet" => no_fleet = true,
            "--themes-dir" => {
                let v = val!();
                let t = v.trim().to_string();
                if t.is_empty() {
                    return Err("--themes-dir 不能为空字符串".into());
                }
                themes_dir = Some(PathBuf::from(t));
            }
            "--reconnect" => auto_reconnect = true,
            "--no-reconnect" => auto_reconnect = false,
            "--headless" => headless = true,
            "--gui" => gui_flag = true,
            other => return Err(format!("未知参数 \"{other}\"")),
        }
        i += 1;
    }

    if headless && gui_flag {
        return Err("--headless 与 --gui 互斥, 只能二选一".into());
    }
    let addr: SocketAddr = addr_str
        .parse()
        .map_err(|_| format!("--addr 不是合法地址 (形如 127.0.0.1:8080): \"{addr_str}\""))?;

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
        fleet,
        no_fleet,
        auto_reconnect,
        themes_dir,
        addr_explicit,
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
        // FR-12: 默认开启自动重连
        assert!(c.auto_reconnect);
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

    #[test]
    fn fleet_flags() {
        // FR-10b: 默认持久化开 (路径留空 = 运行时取默认), --no-fleet 关闭
        let c = parse(&[]).unwrap();
        assert!(c.fleet.is_none());
        assert!(!c.no_fleet);
        let c = parse_str("--no-fleet").unwrap();
        assert!(c.no_fleet);
        let c = parse_str("--fleet %TEMP%\\a.json").unwrap();
        assert_eq!(c.fleet.unwrap().to_string_lossy(), "%TEMP%\\a.json");
        let c = parse_str("--fleet relative.json").unwrap();
        assert_eq!(c.fleet.unwrap().to_string_lossy(), "relative.json");
        assert!(parse_str("--fleet").is_err()); // 缺值
        assert!(parse_str("--fleet  ").is_err()); // 空值
                                                  // --no-fleet 与 --fleet 可并存, --no-fleet 优先 (语义在 from_cli 里收敛)
        let c = parse_str("--fleet a.json --no-fleet").unwrap();
        assert!(c.no_fleet);
    }

    #[test]
    fn reconnect_flags() {
        // FR-12/ADR-16①: 默认 true; --no-reconnect 关; --reconnect 显式开
        assert!(parse(&[]).unwrap().auto_reconnect);
        let c = parse_str("--no-reconnect").unwrap();
        assert!(!c.auto_reconnect);
        let c = parse_str("--reconnect").unwrap();
        assert!(c.auto_reconnect);
        // 与其他参数组合不受影响
        let c = parse_str("--port COM1 --no-reconnect --baud 9600").unwrap();
        assert!(!c.auto_reconnect && c.baud == 9600);
        // 重复给出时后者生效 (与 --flow 同口径, 无互斥裁定)
        let c = parse_str("--no-reconnect --reconnect").unwrap();
        assert!(c.auto_reconnect);
        let c = parse_str("--reconnect --no-reconnect").unwrap();
        assert!(!c.auto_reconnect);
    }

    #[test]
    fn themes_dir_and_addr_explicit_flags() {
        // FR-14: --themes-dir 解析; 默认 None (= exe 旁 themes/)
        let c = parse(&[]).unwrap();
        assert!(c.themes_dir.is_none());
        let c = parse_str("--themes-dir D:\\t").unwrap();
        assert_eq!(c.themes_dir.unwrap().to_string_lossy(), "D:\\t");
        let c = parse_str("--themes-dir themes").unwrap();
        assert_eq!(c.themes_dir.unwrap().to_string_lossy(), "themes");
        assert!(parse_str("--themes-dir").is_err()); // 缺值
        assert!(parse_str("--themes-dir  ").is_err()); // 空值

        // FR-13: --addr 显式标记 (恢复 fleet.json [manager] 地址时, 显式地址优先)
        assert!(!parse(&[]).unwrap().addr_explicit, "默认不显式");
        assert!(parse_str("--addr 127.0.0.1:9000").unwrap().addr_explicit);
        assert!(!parse_str("--port COM1").unwrap().addr_explicit);
    }
}
