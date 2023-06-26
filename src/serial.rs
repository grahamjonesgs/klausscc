

use std::{thread, time};
use serialport::{SerialPortType, UsbPortInfo};
use std::io::{Error, ErrorKind};
use crate::helper::{trim_newline};
use crate::messages::{MsgList,MessageType};



/// Output the code details file to given serial port
///
/// Will send the program to the serial port, and wait for the response
#[allow(clippy::cast_possible_wrap)]
#[allow(clippy::too_many_lines)]
#[allow(clippy::question_mark_used)]
#[allow(clippy::pattern_type_mismatch)]
#[allow(clippy::format_push_string)]
#[cfg(not(tarpaulin_include))] // Cannot test writing to serial in tarpaulin
pub fn write_to_board(
    binary_output: &str,
    port_name: &str,
    msg_list: &mut MsgList,
) -> Result<(), std::io::Error> {

    let mut local_port_name = port_name.to_owned();

    if port_name=="auto" {
        if let Some(suggested_port) = find_possible_port() {
            msg_list.push(
                format!("No port name given, using suggested port {suggested_port}"),
                None,
                None,
                MessageType::Warning,
            );
            local_port_name=suggested_port;
            
        } 
    }
 
    let mut buffer = [0; 1024];
    let port_result = serialport::new(local_port_name.clone(), 1_000_000)
        .timeout(core::time::Duration::from_millis(100))
        .open();

    if let Err(e) = port_result {
        msg_list.push(
            format!("Error opening serial port {local_port_name} error \"{e}\""),
            None,
            None,
            MessageType::Error,
        );
        let mut all_ports: String = String::new();
        let available_ports = serialport::available_ports();
        let mut suggested_port: Option<String> = None;

        match available_ports {
            Err(_) => {
                msg_list.push(
                    "Error opening serial port, no ports found".to_owned(),
                    None,
                    None,
                    MessageType::Error,
                );
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "No ports found",
                ));
            }
            Ok(ports) => {
                let mut max_ports: i32 = -1;
                for (port_count, p) in (0_u32..).zip(ports.into_iter()) {
                    if port_count > 0 {
                        all_ports.push_str(",\n");
                    }

                    if let SerialPortType::UsbPort(info) = &p.port_type {
                        all_ports.push_str(&format!(
                            "USB Serial Device{} {}",
                            extra_usb_info(info),
                            p.port_name
                        ));
                        if check_usb_serial_possible(info) {
                            suggested_port = Some(p.port_name.clone());
                        }
                    } else {
                        all_ports.push_str(&format!("Non USB Serial Device {}", p.port_name));
                    }

                    max_ports = port_count.try_into().unwrap_or_default();
                }

                let ports_msg = match max_ports {
                    -1_i32 => "no ports were found".to_owned(),
                    0_i32 => {
                        format!("only port {all_ports} was found")
                    }
                    _ => {
                        format!("the following {max_ports} ports were found:\n{all_ports}")
                    }
                };

                msg_list.push(
                    format!("Error opening serial port {port_name}, {ports_msg}"),
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

                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Failed to open port",
                ));
            }
        }
    }

    let  Ok(mut port) = port_result else { return Err(Error::new(ErrorKind::Other, "Unknown error")) };

    port.set_stop_bits(serialport::StopBits::One)?;
    port.set_data_bits(serialport::DataBits::Eight)?;
    port.set_parity(serialport::Parity::None)?;
    port.set_flow_control(serialport::FlowControl::None)?;
    port.write_all(b"S")?;

    thread::sleep(time::Duration::from_millis(500)); //Wait for board to reset

    if port.flush().is_err() {};

    if port.read(&mut buffer[..]).is_err() { //clear any old messages in buffer
    }

    for byte in binary_output.as_bytes() {
        let char_delay = time::Duration::from_micros(100);
        thread::sleep(char_delay);
        port.write_all(&[*byte])?;
    }

    port.flush()?;

    let ret_msg_size = port.read(&mut buffer[..]).unwrap_or(0);

    if ret_msg_size == 0 {
        msg_list.push(
            "No message received from board".to_owned(),
            None,
            None,
            MessageType::Warning,
        );
        return Ok(());
    }

    let ret_msg = String::from_utf8(buffer.get(..ret_msg_size).unwrap_or(b"").to_vec());

    if let Err(e) = ret_msg {
        msg_list.push(
            format!("Invalid message received from board, error \"{e}\""),
            None,
            None,
            MessageType::Warning,
        );
        return Ok(());
    }

    let mut print_ret_msg = ret_msg.unwrap_or_else(|_| String::new());

    trim_newline(&mut print_ret_msg); //Board can send CR/LF messages

    msg_list.push(
        format!("Message received from board is \"{print_ret_msg}\""),
        None,
        None,
        MessageType::Information,
    );

    Ok(())
}

/// Formats the USB Port information into a human readable form.
///
/// Gives more USB detals
#[allow(clippy::format_push_string)]
#[allow(clippy::pattern_type_mismatch)]
fn extra_usb_info(info: &UsbPortInfo) -> String {
    let mut output = String::new();
    output.push_str(&format!(" {:04x}:{:04x}", info.vid, info.pid));

    let mut extra_items = Vec::new();

    if let Some(manufacturer) = &info.manufacturer {
        extra_items.push(format!("manufacturer '{manufacturer}'"));
    }
    if let Some(serial) = &info.serial_number {
        extra_items.push(format!("serial '{serial}'"));
    }
    if let Some(product) = &info.product {
        extra_items.push(format!("product '{product}'"));
    }
    if !extra_items.is_empty() {
        output += " with ";
        output += &extra_items.join(" ");
    }
    output
}

/// Return bool if port is possibly correct FDTI port
///
/// Checks the info to check the VID and PID of known boards
fn check_usb_serial_possible(info: &UsbPortInfo) -> bool {
    let vid_pi = format!("{:04x}:{:04x}", info.vid, info.pid);

    if vid_pi == "0403:6010" || vid_pi == "0403:6014" || vid_pi == "0403:6015" {
        return true;
    }
    false
}

/// Checks all ports and returns option of last possible one
///
/// Lists all ports, checlks if ISB and possible and returns some last one or none
#[allow(clippy::pattern_type_mismatch)]
pub fn find_possible_port() -> Option<String> {

    let mut suggested_port: Option<String> = None;
    let available_ports = serialport::available_ports();
    match available_ports {
        Err(_) => {
            return None;
        }
        Ok(ports) => {
            for port in ports {
                if let SerialPortType::UsbPort(info) = &port.port_type {
                    if check_usb_serial_possible(info) {
                        suggested_port = Some(port.port_name.clone());
                    }
                }
            }
        }
    }
    suggested_port
}