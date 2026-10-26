//! The board's UART: send a program image over it, monitor what the board
//! prints, and check a test program's output against expected values.

mod monitor;
mod port;
mod uart_test;
mod upload;

pub(crate) use monitor::{monitor_serial, monitor_serial_port};
pub(crate) use port::SerialError;
pub(crate) use uart_test::{TestResult, hex_token, parse_expected_uart_values, run_test_monitor};
pub(crate) use upload::{AwaitAck, ResetMethod, write_to_board};

use std::fmt;

/// The serial port `-s` names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SerialTarget<'a> {
    /// `-s` with no port: use the board's FTDI USB-UART, if one is found.
    Auto,
    /// The named port.
    Port(&'a str),
}

impl<'a> SerialTarget<'a> {
    /// The target a `-s` option asks for: `None` for no `-s`, `Some(None)`
    /// for a bare `-s` and `Some(Some(name))` for `-s name`.
    pub(crate) fn from_option(option: Option<&'a Option<String>>) -> Option<Self> {
        option.map(|name| name.as_deref().map_or(Self::Auto, Self::Port))
    }
}

impl fmt::Display for SerialTarget<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("(auto-detected)"),
            Self::Port(name) => f.write_str(name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_target_follows_the_s_option() {
        assert_eq!(SerialTarget::from_option(None), None);
        assert_eq!(SerialTarget::from_option(Some(&None)), Some(SerialTarget::Auto));
        let name = Some("COM3".to_owned());
        assert_eq!(SerialTarget::from_option(Some(&name)), Some(SerialTarget::Port("COM3")));
    }

    #[test]
    fn serial_target_displays_the_port() {
        assert_eq!(SerialTarget::Port("/dev/ttyUSB1").to_string(), "/dev/ttyUSB1");
        assert_eq!(SerialTarget::Auto.to_string(), "(auto-detected)");
    }
}
