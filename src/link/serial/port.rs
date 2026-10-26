//! Finding and opening the board's serial port: auto-detection of the
//! FTDI USB-UART, and the list of ports when the one asked for will not open.

use super::SerialTarget;
use crate::diag::Diagnostics;
use serialport::{DataBits, FlowControl, Parity, SerialPort, SerialPortInfo, SerialPortType, StopBits, UsbPortInfo};
use std::fmt::{self, Write as _};
use std::io;
use std::time::Duration;

/// The board UART's baud rate (8N1, no flow control): fixed by the bitstream.
pub(super) const BAUD_RATE: u32 = 3_000_000;

/// How long a read on a freshly opened port waits for data.
const READ_TIMEOUT: Duration = Duration::from_millis(100);

/// FTDI's USB vendor id.
const FTDI_VID: u16 = 0x0403;
/// FTDI USB-UART product ids the board's loader port can show up as
/// (FT232R, FT2232H, FT4232H and FT232H).
const FTDI_PIDS: [u16; 4] = [0x6001, 0x6010, 0x6011, 0x6014];

/// Why the board's serial port could not be used.
#[derive(Debug)]
pub(crate) enum SerialError {
    /// The port would not open.
    Open {
        /// The port's name, as given or auto-detected.
        name: String,
        /// Why it would not open.
        source: serialport::Error,
        /// The serial ports there are, to suggest another.
        available: Vec<SerialPortInfo>,
    },
    /// A bare `-s` (or `-m` alone), and no FTDI USB-UART was found.
    NotFound {
        /// The serial ports there are.
        available: Vec<SerialPortInfo>,
    },
    /// Talking to the open port failed.
    Io(io::Error),
}

impl SerialError {
    /// Report why the port would not open, with the ports there are and the
    /// one to try instead.  A failure on an open port ([`Self::Io`]) is left
    /// to the caller, which knows what it was doing.
    pub(crate) fn report_open_failure(&self, msg_list: &mut Diagnostics) {
        match self {
            Self::Open { name, source, available } => {
                msg_list.error(format!(
                    "Error opening serial port {name}, error \"{source}\", {}",
                    ports_found_message(available)
                ));
                if let Some(suggested) = find_possible_port(available) {
                    msg_list.info(format!("Suggested port {suggested}"));
                }
            }
            Self::NotFound { available } => msg_list.error(format!(
                "Error opening serial port, no FTDI USB serial port found and {}",
                ports_found_message(available)
            )),
            Self::Io(_) => {}
        }
    }
}

impl fmt::Display for SerialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open { name, source, .. } => write!(f, "cannot open serial port {name}: {source}"),
            Self::NotFound { .. } => f.write_str("no FTDI USB serial port found"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for SerialError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Open { source, .. } => Some(source),
            Self::NotFound { .. } => None,
            Self::Io(err) => Some(err),
        }
    }
}

impl From<io::Error> for SerialError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serialport::Error> for SerialError {
    fn from(err: serialport::Error) -> Self {
        Self::Io(err.into())
    }
}

/// Whether a USB device with this vendor and product id may be the board's
/// FTDI USB-UART.
fn is_ftdi(vid: u16, pid: u16) -> bool {
    vid == FTDI_VID && FTDI_PIDS.contains(&pid)
}

/// Whether `port` is an FTDI USB-UART.
fn is_ftdi_port(port: &SerialPortInfo) -> bool {
    matches!(&port.port_type, SerialPortType::UsbPort(info) if is_ftdi(info.vid, info.pid))
}

/// The FTDI USB-UART port to use when none is named.
///
/// The board's FT2232H shows up as two ports, JTAG on interface A and the
/// UART on interface B, and the UART's is the one whose name sorts last
/// (`/dev/cu.usbserial-XXXXB`, or the higher `/dev/ttyUSBn`), so the highest
/// name of the FTDI ports wins.
fn find_possible_port(ports: &[SerialPortInfo]) -> Option<&str> {
    ports.iter().filter(|port| is_ftdi_port(port)).map(|port| port.port_name.as_str()).max()
}

/// Formats the USB Port information into a human readable form.
///
/// Gives more USB details.
fn extra_usb_info(info: &UsbPortInfo) -> String {
    let mut output = String::default();
    let _ = write!(output, " {:04x}:{:04x}", info.vid, info.pid);

    let mut extra_items = Vec::new();

    if let Some(ref manufacturer) = info.manufacturer {
        extra_items.push(format!("manufacturer '{manufacturer}'"));
    }
    if let Some(ref serial) = info.serial_number {
        extra_items.push(format!("serial '{serial}'"));
    }
    if let Some(ref product) = info.product {
        extra_items.push(format!("product '{product}'"));
    }
    if !extra_items.is_empty() {
        output += " with ";
        output += &extra_items.join(" ");
    }
    output
}

/// Describe one serial port for the list of ports found.
fn describe_port(port: &SerialPortInfo) -> String {
    match &port.port_type {
        SerialPortType::UsbPort(info) => format!("USB Serial Device{} {}", extra_usb_info(info), port.port_name),
        _ => format!("Non-USB Serial Device {}", port.port_name),
    }
}

/// Describe the serial ports found, for the error when a port will not open.
fn ports_found_message(ports: &[SerialPortInfo]) -> String {
    let all_ports = ports.iter().map(describe_port).collect::<Vec<_>>().join(",\n");
    match ports.len() {
        0 => "no ports were found".to_owned(),
        1 => format!("only port {all_ports} was found"),
        count => format!("the following {count} ports were found:\n{all_ports}"),
    }
}

/// Open the serial port `target` names, set up for the board's UART (8N1, no
/// flow control).
///
/// [`SerialTarget::Auto`] picks the FTDI USB-UART (see
/// [`find_possible_port`]) and says which it chose.  A port that will not
/// open comes back as [`SerialError::Open`] with the ports there are, for
/// [`SerialError::report_open_failure`]; nothing is reported here.
#[cfg(not(tarpaulin_include))] // Cannot test opening a serial port in tarpaulin
pub(super) fn open_port(target: SerialTarget<'_>, msg_list: &mut Diagnostics) -> Result<Box<dyn SerialPort>, SerialError> {
    let mut listed = None;
    let name = match target {
        SerialTarget::Port(name) => name.to_owned(),
        SerialTarget::Auto => {
            let available = serialport::available_ports().unwrap_or_default();
            let Some(name) = find_possible_port(&available).map(str::to_owned) else {
                return Err(SerialError::NotFound { available });
            };
            msg_list.info(format!("No port name given, using automatic port {name}"));
            listed = Some(available);
            name
        }
    };

    serialport::new(&name, BAUD_RATE)
        .data_bits(DataBits::Eight)
        .parity(Parity::None)
        .stop_bits(StopBits::One)
        .flow_control(FlowControl::None)
        .timeout(READ_TIMEOUT)
        .open()
        .map_err(|source| SerialError::Open {
            available: listed.unwrap_or_else(|| serialport::available_ports().unwrap_or_default()),
            name,
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A USB serial port with this name and USB id.
    fn usb(name: &str, vid: u16, pid: u16) -> SerialPortInfo {
        SerialPortInfo {
            port_name: name.to_owned(),
            port_type: SerialPortType::UsbPort(UsbPortInfo {
                vid,
                pid,
                serial_number: None,
                manufacturer: None,
                product: None,
            }),
        }
    }

    /// A serial port that is not on USB.
    fn other(name: &str) -> SerialPortInfo {
        SerialPortInfo {
            port_name: name.to_owned(),
            port_type: SerialPortType::Unknown,
        }
    }

    #[test]
    fn ports_found_message_counts_every_port() {
        assert_eq!(ports_found_message(&[]), "no ports were found");
        assert_eq!(ports_found_message(&[other("A")]), "only port Non-USB Serial Device A was found");
        assert_eq!(
            ports_found_message(&[other("A"), usb("B", 0x0403, 0x6010)]),
            "the following 2 ports were found:\nNon-USB Serial Device A,\nUSB Serial Device 0403:6010 B"
        );
    }

    #[test]
    fn usb_ports_are_described_with_their_details() {
        let mut port = usb("/dev/ttyUSB0", 0x0403, 0x6001);
        if let SerialPortType::UsbPort(info) = &mut port.port_type {
            info.manufacturer = Some("FTDI".to_owned());
            info.serial_number = Some("A1".to_owned());
            info.product = Some("UART".to_owned());
        }
        assert_eq!(
            describe_port(&port),
            "USB Serial Device 0403:6001 with manufacturer 'FTDI' serial 'A1' product 'UART' /dev/ttyUSB0"
        );
    }

    #[test]
    fn is_ftdi_matches_the_known_uart_chips() {
        for pid in [0x6001, 0x6010, 0x6011, 0x6014] {
            assert!(is_ftdi(0x0403, pid), "{pid:04x}");
        }
        assert!(!is_ftdi(0x0403, 0x6015));
        assert!(!is_ftdi(0x10C4, 0x6001), "another vendor's chip with the same pid");
    }

    #[test]
    fn the_highest_named_ftdi_port_is_chosen() {
        let ports = [
            usb("/dev/cu.usbserial-1234B", 0x0403, 0x6010),
            other("/dev/cu.zzz"),
            usb("/dev/cu.usbserial-1234A", 0x0403, 0x6010),
            usb("/dev/cu.usbmodem-9", 0x10C4, 0xEA60),
        ];
        assert_eq!(find_possible_port(&ports), Some("/dev/cu.usbserial-1234B"));
        assert_eq!(find_possible_port(&ports[1..]), Some("/dev/cu.usbserial-1234A"));
        assert_eq!(find_possible_port(&[other("/dev/ttyS0")]), None);
        assert_eq!(find_possible_port(&[]), None);
    }

    #[test]
    fn an_open_failure_lists_the_ports_and_suggests_one() {
        let err = SerialError::Open {
            name: "COM9".to_owned(),
            source: serialport::Error::new(serialport::ErrorKind::NoDevice, "not found"),
            available: vec![usb("COM4", 0x0403, 0x6001)],
        };
        assert_eq!(err.to_string(), "cannot open serial port COM9: not found");
        let mut msgs = Diagnostics::new();
        err.report_open_failure(&mut msgs);
        let text: Vec<String> = msgs.messages().iter().map(|m| m.text().to_owned()).collect();
        assert_eq!(
            text,
            [
                "Error opening serial port COM9, error \"not found\", only port USB Serial Device 0403:6001 COM4 was found",
                "Suggested port COM4"
            ]
        );
    }

    #[test]
    fn no_ftdi_port_found_is_one_error() {
        let err = SerialError::NotFound { available: vec![] };
        let mut msgs = Diagnostics::new();
        err.report_open_failure(&mut msgs);
        let text: Vec<String> = msgs.messages().iter().map(|m| m.text().to_owned()).collect();
        assert_eq!(text, ["Error opening serial port, no FTDI USB serial port found and no ports were found"]);
    }
}
