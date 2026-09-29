//! Serial ports (RS-232/RS-485, USB-serial adapters).
//!
//! Many laboratory analyzers still talk to their host over a serial line.
//! [`PortSettings`] describes a port and opens it as an asynchronous byte
//! stream; the ASTM connectors run the same link logic over it as over TCP.
//! Serial support uses the `tokio-serial` and `serialport` crates;
//! `serialport` is MPL-2.0 licensed and used unmodified.

use serde::Deserialize;
use tokio_serial::{SerialPortBuilderExt, SerialStream};

/// Parity checking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Parity {
    /// No parity bit.
    #[default]
    None,
    /// Odd parity.
    Odd,
    /// Even parity.
    Even,
}

/// Flow control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowControl {
    /// No flow control.
    #[default]
    None,
    /// XON/XOFF.
    Software,
    /// RTS/CTS.
    Hardware,
}

fn baud_rate_default() -> u32 {
    9600
}

fn data_bits_default() -> u8 {
    8
}

fn stop_bits_default() -> u8 {
    1
}

/// A serial port and its line settings. The defaults are 9600 baud, 8 data
/// bits, no parity, 1 stop bit and no flow control (9600 8N1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortSettings {
    /// Port name, for example `COM3` or `/dev/ttyUSB0`.
    pub port: String,
    /// Baud rate.
    #[serde(default = "baud_rate_default")]
    pub baud_rate: u32,
    /// Data bits: 5, 6, 7 or 8.
    #[serde(default = "data_bits_default")]
    pub data_bits: u8,
    /// Parity.
    #[serde(default)]
    pub parity: Parity,
    /// Stop bits: 1 or 2.
    #[serde(default = "stop_bits_default")]
    pub stop_bits: u8,
    /// Flow control.
    #[serde(default)]
    pub flow_control: FlowControl,
}

impl PortSettings {
    /// Settings for `port` at 9600 8N1.
    pub fn new(port: impl Into<String>) -> Self {
        Self {
            port: port.into(),
            baud_rate: baud_rate_default(),
            data_bits: data_bits_default(),
            parity: Parity::None,
            stop_bits: stop_bits_default(),
            flow_control: FlowControl::None,
        }
    }

    /// Checks the values that the port driver would reject.
    pub fn validate(&self) -> Result<(), String> {
        if self.port.trim().is_empty() {
            return Err("serial port name is empty".into());
        }
        if self.baud_rate == 0 {
            return Err("baud_rate must be positive".into());
        }
        if !(5..=8).contains(&self.data_bits) {
            return Err(format!("data_bits must be 5 to 8, not {}", self.data_bits));
        }
        if !(1..=2).contains(&self.stop_bits) {
            return Err(format!("stop_bits must be 1 or 2, not {}", self.stop_bits));
        }
        Ok(())
    }

    /// Opens the port. Must be called inside a Tokio runtime.
    pub fn open(&self) -> std::io::Result<SerialStream> {
        self.validate().map_err(std::io::Error::other)?;
        let data_bits = match self.data_bits {
            5 => tokio_serial::DataBits::Five,
            6 => tokio_serial::DataBits::Six,
            7 => tokio_serial::DataBits::Seven,
            _ => tokio_serial::DataBits::Eight,
        };
        let stop_bits = match self.stop_bits {
            2 => tokio_serial::StopBits::Two,
            _ => tokio_serial::StopBits::One,
        };
        let parity = match self.parity {
            Parity::None => tokio_serial::Parity::None,
            Parity::Odd => tokio_serial::Parity::Odd,
            Parity::Even => tokio_serial::Parity::Even,
        };
        let flow_control = match self.flow_control {
            FlowControl::None => tokio_serial::FlowControl::None,
            FlowControl::Software => tokio_serial::FlowControl::Software,
            FlowControl::Hardware => tokio_serial::FlowControl::Hardware,
        };
        tokio_serial::new(&self.port, self.baud_rate)
            .data_bits(data_bits)
            .stop_bits(stop_bits)
            .parity(parity)
            .flow_control(flow_control)
            .open_native_async()
            .map_err(std::io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_line_settings() {
        assert!(PortSettings::new("COM3").validate().is_ok());
        let mut settings = PortSettings::new("COM3");
        settings.data_bits = 9;
        assert!(settings.validate().is_err());
        let mut settings = PortSettings::new("COM3");
        settings.stop_bits = 3;
        assert!(settings.validate().is_err());
        assert!(PortSettings::new(" ").validate().is_err());
    }

    #[test]
    fn reads_settings_with_defaults() {
        let settings: PortSettings = serde_json::from_value(serde_json::json!({
            "port": "/dev/ttyUSB0",
            "parity": "even",
            "flow_control": "hardware"
        }))
        .unwrap();
        assert_eq!(settings.baud_rate, 9600);
        assert_eq!(settings.data_bits, 8);
        assert_eq!(settings.parity, Parity::Even);
        assert_eq!(settings.flow_control, FlowControl::Hardware);
    }
}
