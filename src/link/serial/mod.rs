//! The board's UART: send a program image over it, monitor what the board
//! prints, and check a test program's output against expected values.

mod monitor;
mod port;
mod uart_test;
mod upload;

pub(crate) use monitor::{monitor_serial, monitor_serial_port};
pub(crate) use uart_test::run_test_monitor;
pub(crate) use upload::{write_to_board, write_to_board_keep_port};

/// Used to define if a port request was not defined so is for auto.
pub(crate) const AUTO_SERIAL: &str = "auto_serial_requested";

/// The serial port `-s` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SerialTarget {
    /// `-s` with no port: use the first USB serial port found.
    Auto,
    /// The named port.
    Port(String),
}

impl SerialTarget {
    /// The port name the serial functions take: the port's name, or
    /// [`AUTO_SERIAL`] to auto-detect.
    pub(crate) fn port_name(&self) -> &str {
        match self {
            Self::Auto => AUTO_SERIAL,
            Self::Port(name) => name,
        }
    }

    /// Parse a `-s` value.  [`AUTO_SERIAL`], which a bare `-s` stands for,
    /// asks for auto-detection; an empty port name is an error.
    pub(crate) fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "" => Err("the serial port name is empty"),
            AUTO_SERIAL => Ok(Self::Auto),
            name => Ok(Self::Port(name.to_owned())),
        }
    }
}
