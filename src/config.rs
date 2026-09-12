//! 串口参数模型与解析 (FR-2)。
//!
//! 为什么独立成纯函数模块: CLI (`--config 8N2`) 与 Web (`/api/config`) 两个入口
//! 共用同一套校验与映射逻辑, 保证两边行为一字不差; 且不依赖 serialport 类型,
//! 可以在没有串口的环境里跑纯函数单测。serialport 类型的映射放 serial.rs。

/// FR-2 规定的波特率合法区间。
pub const BAUD_MIN: u32 = 110;
pub const BAUD_MAX: u32 = 2_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Parity {
    N,
    E,
    O,
}

impl Parity {
    pub fn as_char(self) -> char {
        match self {
            Parity::N => 'N',
            Parity::E => 'E',
            Parity::O => 'O',
        }
    }

    #[allow(dead_code)] // 单元测试与调试输出使用
    pub fn as_str(self) -> &'static str {
        match self {
            Parity::N => "N",
            Parity::E => "E",
            Parity::O => "O",
        }
    }

    pub fn parse(s: &str) -> Result<Parity, String> {
        match s.trim().to_ascii_uppercase().as_str() {
            "N" => Ok(Parity::N),
            "E" => Ok(Parity::E),
            "O" => Ok(Parity::O),
            _ => Err(format!("校验位只支持 N/E/O, 得到 \"{s}\"")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    None,
    RtsCts,
    XonXoff,
}

impl Flow {
    #[allow(dead_code)] // 单元测试与调试输出使用
    pub fn as_str(self) -> &'static str {
        match self {
            Flow::None => "none",
            Flow::RtsCts => "rtscts",
            Flow::XonXoff => "xonxoff",
        }
    }

    pub fn parse(s: &str) -> Result<Flow, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Flow::None),
            "rtscts" => Ok(Flow::RtsCts),
            "xonxoff" => Ok(Flow::XonXoff),
            _ => Err(format!("流控只支持 none/rtscts/xonxoff, 得到 \"{s}\"")),
        }
    }
}

/// 串口完整配置。port 为空表示"未配置" (FR-5: 未给 --port 时启动为未打开态)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialConfig {
    pub port: String,
    pub baud: u32,
    pub data_bits: u8,
    pub parity: Parity,
    pub stop_bits: u8,
    pub flow: Flow,
}

impl Default for SerialConfig {
    fn default() -> Self {
        // 项目签名的实战默认: 8N2 (见 spec §0 调研结论)。
        Self {
            port: String::new(),
            baud: 115200,
            data_bits: 8,
            parity: Parity::N,
            stop_bits: 2,
            flow: Flow::None,
        }
    }
}

impl SerialConfig {
    /// "8N2" 形式的紧凑串 (与 CLI --config / 状态接口 config 字段同构)。
    pub fn config_str(&self) -> String {
        format!("{}{}{}", self.data_bits, self.parity.as_char(), self.stop_bits)
    }

    /// 打开串口前的完整校验。port 允许为空 (是否为空由调用方决定语义)。
    pub fn validate(&self) -> Result<(), String> {
        validate_baud(self.baud)?;
        if !matches!(self.data_bits, 7 | 8) {
            return Err(format!("数据位只支持 7/8, 得到 {}", self.data_bits));
        }
        if !matches!(self.stop_bits, 1 | 2) {
            return Err(format!("停止位只支持 1/2, 得到 {}", self.stop_bits));
        }
        Ok(())
    }
}

pub fn validate_baud(b: u32) -> Result<(), String> {
    if (BAUD_MIN..=BAUD_MAX).contains(&b) {
        Ok(())
    } else {
        Err(format!(
            "波特率 {b} 超出允许范围 [{BAUD_MIN}, {BAUD_MAX}]"
        ))
    }
}

/// 解析 "8N2"/"7E1" 形式的参数串 → (数据位, 校验, 停止位)。大小写不敏感。
pub fn parse_config_str(s: &str) -> Result<(u8, Parity, u8), String> {
    let t = s.trim().to_ascii_uppercase();
    let ch: Vec<char> = t.chars().collect();
    if ch.len() != 3 {
        return Err(format!(
            "串口参数 \"{s}\" 格式非法, 应为 <数据位><校验><停止位>, 如 8N2 / 7E1 / 7O1"
        ));
    }
    let data_bits = match ch[0] {
        '7' => 7u8,
        '8' => 8u8,
        _ => return Err(format!("数据位只支持 7/8, 得到 \"{}\"", ch[0])),
    };
    let parity = match ch[1] {
        'N' => Parity::N,
        'E' => Parity::E,
        'O' => Parity::O,
        _ => return Err(format!("校验位只支持 N/E/O, 得到 \"{}\"", ch[1])),
    };
    let stop_bits = match ch[2] {
        '1' => 1u8,
        '2' => 2u8,
        _ => return Err(format!("停止位只支持 1/2, 得到 \"{}\"", ch[2])),
    };
    Ok((data_bits, parity, stop_bits))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_str_ok() {
        assert_eq!(parse_config_str("8N2"), Ok((8, Parity::N, 2)));
        assert_eq!(parse_config_str("7E1"), Ok((7, Parity::E, 1)));
        assert_eq!(parse_config_str("7O1"), Ok((7, Parity::O, 1)));
        assert_eq!(parse_config_str("8O2"), Ok((8, Parity::O, 2)));
        // 大小写不敏感 + 容忍首尾空白
        assert_eq!(parse_config_str("8n2 "), Ok((8, Parity::N, 2)));
        assert_eq!(parse_config_str(" 7e1"), Ok((7, Parity::E, 1)));
    }

    #[test]
    fn parse_config_str_rejects_garbage() {
        for bad in ["", "8N", "8N21", "9N2", "8X2", "8N3", "8 E 1", "82", "8,n,2"] {
            assert!(parse_config_str(bad).is_err(), "应拒绝 {bad:?}");
        }
        // 错误信息要能定位到是哪一位非法
        assert!(parse_config_str("9N2").unwrap_err().contains('9'));
        assert!(parse_config_str("8X2").unwrap_err().contains('X'));
    }

    #[test]
    fn baud_range_enforced() {
        assert!(validate_baud(110).is_ok());
        assert!(validate_baud(115_200).is_ok());
        assert!(validate_baud(2_000_000).is_ok());
        assert!(validate_baud(109).is_err());
        assert!(validate_baud(2_000_001).is_err());
    }

    #[test]
    fn parity_and_flow_parse() {
        assert_eq!(Parity::parse("n"), Ok(Parity::N));
        assert_eq!(Parity::parse("E"), Ok(Parity::E));
        assert_eq!(Parity::parse("o "), Ok(Parity::O));
        assert!(Parity::parse("M").is_err());
        assert_eq!(Flow::parse("none"), Ok(Flow::None));
        assert_eq!(Flow::parse("RTSCTS"), Ok(Flow::RtsCts));
        assert_eq!(Flow::parse("xonxoff"), Ok(Flow::XonXoff));
        assert!(Flow::parse("hardware").is_err());
        assert!(Flow::parse("").is_err());
    }

    #[test]
    fn config_str_and_validate() {
        let mut cfg = SerialConfig::default();
        assert_eq!(cfg.config_str(), "8N2");
        assert!(cfg.validate().is_ok());
        cfg.baud = 100; // 低于 110
        assert!(cfg.validate().is_err());
        cfg.baud = 115_200;
        cfg.data_bits = 6;
        assert!(cfg.validate().is_err());
        cfg.data_bits = 7;
        cfg.stop_bits = 2;
        assert!(cfg.validate().is_ok());
        assert_eq!(cfg.config_str(), "7N2");
        // port 为空不参与 validate (语义由 /api/open 决定)
        assert!(SerialConfig::default().validate().is_ok());
    }
}
