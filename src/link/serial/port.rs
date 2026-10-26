//! Finding and opening the board's serial port: auto-detection of the
//! FTDI USB-UART, and the list of ports when the one asked for will not open.

use super::AUTO_SERIAL;
use crate::diag::Diagnostics;
use serialport::{SerialPort, SerialPortType, UsbPortInfo};
use std::fmt::Write as _;
use std::io::Error;
use std::time::Duration;

/// Return bool if port is possibly correct FDTI port.
///
/// Checks the info to check the VID and PID of known boards.
#[cfg(not(tarpaulin_include))] // Cannot test writing to serial in tarpaulin
fn check_usb_serial_possible(info: &UsbPortInfo) -> bool {
    let vid_pi = format!("{:04x}:{:04x}", info.vid, info.pid);

    if vid_pi == "0403:6001" || vid_pi == "0403:6010" || vid_pi == "0403:6011" || vid_pi == "0403:6014" {
        return true;
    }
    false
}

/// Formats the USB Port information into a human readable form.
///
/// Gives more USB details.
#[cfg(not(tarpaulin_include))] // Cannot test writing to serial in tarpaulin
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

/// Checks all ports and returns option of last possible one.
///
/// Lists all ports, checks if ISB and possible and returns some last one or none.
#[cfg(not(tarpaulin_include))] // Cannot test writing to serial in tarpaulin
fn find_possible_port() -> Option<String> {
    let available_ports = serialport::available_ports();
    match available_ports {
        Err(_) => None,
        Ok(ports) => {
            let mut matching: Vec<String> = ports
                .into_iter()
                .filter_map(|port| {
                    if let SerialPortType::UsbPort(info) = port.port_type
                        && check_usb_serial_possible(&info)
                    {
                        return Some(port.port_name);
                    }
                    None
                })
                .collect();
            matching.sort();
            matching.into_iter().last()
        }
    }
}

/// Return port from port name.
///
/// If port name is `AUTO_SERIAL` then return the first USB serial port found.
#[cfg(not(tarpaulin_include))] // Cannot test writing to serial in tarpaulin
pub(super) fn return_port(port_name: &str, msg_list: &mut Diagnostics) -> Result<Box<dyn SerialPort>, Error> {
    let mut local_port_name = port_name.to_owned();
    if port_name == AUTO_SERIAL
        && let Some(suggested_port) = find_possible_port()
    {
        msg_list.info(format!("No port name given, using automatic port {suggested_port}"));
        local_port_name = suggested_port;
    }

    let port_result = serialport::new(local_port_name.clone(), 3_000_000)
        .timeout(Duration::from_millis(100))
        .open();
    if let Err(err) = port_result {
        if local_port_name != AUTO_SERIAL {
            msg_list.error(format!("Error opening serial port {local_port_name}, error \"{err}\""));
        }
        let mut all_ports = String::default();
        let available_ports = serialport::available_ports();
        let mut suggested_port: Option<String> = None;

        return match available_ports {
            Err(_) => {
                msg_list.error("Error opening serial port, no ports found");
                Err(Error::other("No ports found"))
            }
            Ok(ports) => {
                let port_count = ports.len();
                for (index, port) in ports.into_iter().enumerate() {
                    if index > 0 {
                        all_ports.push_str(",\n");
                    }
                    if let SerialPortType::UsbPort(info) = port.port_type {
                        let _ = write!(all_ports, "USB Serial Device{} {}", extra_usb_info(&info), port.port_name);
                        if check_usb_serial_possible(&info) {
                            suggested_port = Some(port.port_name.clone());
                        }
                    } else {
                        let _ = write!(all_ports, "Non-USB Serial Device {}", port.port_name);
                    }
                }

                let ports_msg = ports_found_message(port_count, &all_ports);
                msg_list.error(format!("Error opening serial port, {ports_msg}"));

                if suggested_port.is_some() {
                    msg_list.info(format!("Suggested port {}", suggested_port.unwrap_or_default()));
                }

                Err(Error::other("Failed to open port"))
            }
        };
    }

    let Ok(port) = port_result else {
        return Err(Error::other("Unknown error"));
    };
    Ok(port)
}

/// Describe the serial ports found, for the error when a port will not open.
///
/// `all_ports` is the list of `count` port descriptions, one per line.
fn ports_found_message(count: usize, all_ports: &str) -> String {
    match count {
        0 => "no ports were found".to_owned(),
        1 => format!("only port {all_ports} was found"),
        _ => format!("the following {count} ports were found:\n{all_ports}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_found_message_counts_every_port() {
        assert_eq!(ports_found_message(0, ""), "no ports were found");
        assert_eq!(ports_found_message(1, "A"), "only port A was found");
        assert_eq!(ports_found_message(2, "A,\nB"), "the following 2 ports were found:\nA,\nB");
        assert_eq!(ports_found_message(3, "A,\nB,\nC"), "the following 3 ports were found:\nA,\nB,\nC");
    }
}
