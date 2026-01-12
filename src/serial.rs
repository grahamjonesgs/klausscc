use crate::helper::trim_newline;
use crate::messages::{MessageType, MsgList};
use core::time::Duration;
use serialport::{SerialPort, SerialPortType, UsbPortInfo};
use std::io::Error;
use std::thread;

/// Used to define if a port request was not defined so is for auto.
pub const AUTO_SERIAL: &str = "auto_serial_requested";

/// Return bool if port is possibly correct FDTI port.
///
/// Checks the info to check the VID and PID of known boards.
#[cfg(not(tarpaulin_include))] // Cannot test writing to serial in tarpaulin
fn check_usb_serial_possible(info: &UsbPortInfo) -> bool {
    let vid_pi = format!("{:04x}:{:04x}", info.vid, info.pid);

    if vid_pi == "0403:6001"
        || vid_pi == "0403:6010"
        || vid_pi == "0403:6011"
        || vid_pi == "0403:6014"
    {
        return true;
    }
    false
}

/// Formats the USB Port information into a human readable form.
///
/// Gives more USB details.
#[allow(clippy::ref_patterns, reason = "Pattern matching on references is intentional for clarity")]
#[allow(clippy::arithmetic_side_effects, reason = "Arithmetic side effects are intentional and safe in this context")]
#[cfg(not(tarpaulin_include))] // Cannot test writing to serial in tarpaulin
fn extra_usb_info(info: &UsbPortInfo) -> String {
    let mut output = String::default();
    #[allow(clippy::format_push_string, reason = "Using format! with push_str for clarity and explicitness")]
    output.push_str(&format!(" {:04x}:{:04x}", info.vid, info.pid));

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
pub fn find_possible_port() -> Option<String> {
    let mut suggested_port: Option<String> = None;
    let available_ports = serialport::available_ports();
    match available_ports {
        Err(_) => {
            return None;
        }
        Ok(ports) => {
            for port in ports {
                if let SerialPortType::UsbPort(info) = port.port_type {
                    if check_usb_serial_possible(&info) {
                        suggested_port = Some(port.port_name.clone());
                    }
                }
            }
        }
    }
    suggested_port
}

/// Return port from port name.
///
/// If port name is `AUTO_SERIAL` then return the first USB serial port found.
#[cfg(not(tarpaulin_include))] // Cannot test writing to serial in tarpaulin
fn return_port(port_name: &str, msg_list: &mut MsgList) -> Result<Box<dyn SerialPort>, Error> {
    let mut local_port_name = port_name.to_owned();
    if port_name == AUTO_SERIAL {
        if let Some(suggested_port) = find_possible_port() {
            msg_list.push(
                format!("No port name given, using automatic port {suggested_port}"),
                None,
                None,
                MessageType::Information,
            );
            local_port_name = suggested_port;
        }
    }

    let port_result = serialport::new(local_port_name.clone(), 1_000_000)
        .timeout(Duration::from_millis(100))
        .open();
    if let Err(err) = port_result {
        if local_port_name != AUTO_SERIAL {
            msg_list.push(
                format!("Error opening serial port {local_port_name} error \"{err}\""),
                None,
                None,
                MessageType::Error,
            );
        }
        let mut all_ports = String::default();
        let available_ports = serialport::available_ports();
        let mut suggested_port: Option<String> = None;

        return match available_ports {
            Err(_) => {
                msg_list.push(
                    "Error opening serial port, no ports found".to_owned(),
                    None,
                    None,
                    MessageType::Error,
                );
                Err(Error::other("No ports found"))
            }
            Ok(ports) => {
                let mut max_ports: i32 = -1;
                for (port_count, port) in (0_u32..).zip(ports) {
                    if port_count > 0 {
                        all_ports.push_str(",\n");
                    }
                    #[allow(clippy::format_push_string, reason = "Using format! with push_str for clarity and explicitness")]
                    if let SerialPortType::UsbPort(info) = port.port_type {
                        all_ports.push_str(&format!(
                            "USB Serial Device{} {}",
                            extra_usb_info(&info),
                            port.port_name
                        ));
                        if check_usb_serial_possible(&info) {
                            suggested_port = Some(port.port_name.clone());
                        }
                    } else {
                        all_ports.push_str(&format!(" Non USB Serial Device {}", port.port_name));
                    }

                    max_ports = port_count.try_into().unwrap_or_default();
                }

                let ports_msg = match max_ports {
                    -1_i32 => "no ports were found".to_owned(),
                    0_i32 => {
                        format!("only port{all_ports} was found")
                    }
                    _ => {
                        format!("the following {max_ports} ports were found:\n{all_ports}")
                    }
                };

                msg_list.push(
                    format!("Error opening serial port, {ports_msg}"),
                    None,
                    None,
                    MessageType::Error,
                );

                if suggested_port.is_some() {
                    msg_list.push(
                        format!("Suggested port {}", suggested_port.unwrap_or_default()),
                        None,
                        None,
                        MessageType::Information,
                    );
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

/// Output the code details file to given serial port.
///
/// Will send the program to the serial port, and wait for the response.
#[allow(clippy::question_mark_used, reason = "Using the question mark operator for error handling is intentional and improves readability in this context")]
#[cfg(not(tarpaulin_include))] // Cannot test writing to serial in tarpaulin
pub fn write_to_board(
    binary_output: &str,
    port_name: &str,
    msg_list: &mut MsgList,
) -> Result<(), Error> {
    use serialport::{DataBits, FlowControl, Parity, StopBits};

    let mut read_buffer = [0; 1024];
    let mut port = return_port(port_name, msg_list)?;

    port.set_stop_bits(StopBits::One)?;
    port.set_data_bits(DataBits::Eight)?;
    port.set_parity(Parity::None)?;
    port.set_flow_control(FlowControl::None)?;
    port.write_all(b"S")?;

    thread::sleep(Duration::from_millis(500)); //Wait for board to reset

    port.flush()?;

    if port.read(&mut read_buffer[..]).is_err() { //clear any old messages in buffer
    }

    for byte in binary_output.as_bytes() {
        let char_delay = Duration::from_micros(100);
        thread::sleep(char_delay);
        port.write_all(&[*byte])?;
    }

    port.flush()?;
    let ret_msg_size = port.read(&mut read_buffer[..]).unwrap_or(0);

    if ret_msg_size == 0 {
        msg_list.push(
            "No message received from board".to_owned(),
            None,
            None,
            MessageType::Warning,
        );
        return Ok(());
    }

    let ret_msg = String::from_utf8(read_buffer.get(..ret_msg_size).unwrap_or(b"").to_vec());

    if let Err(err) = ret_msg {
        msg_list.push(
            format!("Invalid message received from board, error \"{err}\""),
            None,
            None,
            MessageType::Warning,
        );
        return Ok(());
    }

    let mut print_ret_msg = ret_msg.unwrap_or_else(|_| String::default());

    trim_newline(&mut print_ret_msg); //Board can send CR/LF messages

    msg_list.push(
        format!("Message received from board is \"{print_ret_msg}\""),
        None,
        None,
        MessageType::Information,
    );

    Ok(())
}
