use crate::helper::trim_newline;
use crate::messages::{MessageType, MsgList};
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;
use std::time::Instant;
use serialport::{SerialPort, SerialPortType, UsbPortInfo};
use std::io::{self, Error, Read as _, Write as _};
use std::sync::Arc;
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
    let available_ports = serialport::available_ports();
    match available_ports {
        Err(_) => None,
        Ok(ports) => {
            let mut matching: Vec<String> = ports
                .into_iter()
                .filter_map(|port| {
                    if let SerialPortType::UsbPort(info) = port.port_type {
                        if check_usb_serial_possible(&info) {
                            return Some(port.port_name);
                        }
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

/// Output the code details file to given serial port, keeping the port open.
///
/// Will send the program to the serial port, wait for the response, and return the open port.
#[allow(clippy::question_mark_used, reason = "Using the question mark operator for error handling is intentional and improves readability in this context")]
#[cfg(not(tarpaulin_include))] // Cannot test writing to serial in tarpaulin
pub fn write_to_board_keep_port(
    binary_output: &str,
    port_name: &str,
    send_break: bool,
    msg_list: &mut MsgList,
) -> Result<Box<dyn SerialPort>, Error> {
    use serialport::{DataBits, FlowControl, Parity, StopBits};

    let mut read_buffer = [0; 1024];
    let mut port = return_port(port_name, msg_list)?;

    port.set_stop_bits(StopBits::One)?;
    port.set_data_bits(DataBits::Eight)?;
    port.set_parity(Parity::None)?;
    port.set_flow_control(FlowControl::None)?;

    if send_break {
        port.set_break()?;
        thread::sleep(Duration::from_millis(100));
        port.clear_break()?;
    } else {
        port.write_all(b"S")?;
    }

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
        return Ok(port);
    }

    let ret_msg = String::from_utf8(read_buffer.get(..ret_msg_size).unwrap_or(b"").to_vec());

    if let Err(err) = ret_msg {
        msg_list.push(
            format!("Invalid message received from board, error \"{err}\""),
            None,
            None,
            MessageType::Warning,
        );
        return Ok(port);
    }

    let mut print_ret_msg = ret_msg.unwrap_or_else(|_| String::default());

    trim_newline(&mut print_ret_msg); //Board can send CR/LF messages

    msg_list.push(
        format!("Message received from board is \"{print_ret_msg}\""),
        None,
        None,
        MessageType::Information,
    );

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
    send_break: bool,
    msg_list: &mut MsgList,
) -> Result<(), Error> {
    let _port = write_to_board_keep_port(binary_output, port_name, send_break, msg_list)?;
    Ok(())
}

/// Print bytes to stdout, optionally showing each byte as hex alongside the character.
#[allow(clippy::print_stdout, reason = "Printing to stdout is required for serial monitor output")]
fn print_bytes(data: &[u8], debug: bool) {
    if debug {
        for &b in data {
            if b.is_ascii_graphic() || b == b' ' {
                print!("{:02X}('{}') ", b, char::from(b));
            } else {
                print!("{:02X} ", b);
            }
        }
    } else {
        print!("{}", String::from_utf8_lossy(data));
    }
}

/// Monitor serial port for incoming UART data from the FPGA board.
///
/// Continuously reads from the serial port and prints received data to stdout.
/// Runs until the user presses Ctrl+C, then closes the port cleanly.
#[allow(clippy::print_stdout, reason = "Printing to stdout is required for serial monitor output")]
#[allow(clippy::question_mark_used, reason = "Using the question mark operator for error handling")]
#[cfg(not(tarpaulin_include))] // Cannot test serial monitoring in tarpaulin
pub fn monitor_serial(port_name: &str, debug: bool, msg_list: &mut MsgList) -> Result<(), Error> {
    let port = return_port(port_name, msg_list)?;
    monitor_serial_port(port, debug, msg_list)
}

/// Monitor an already-open serial port for incoming UART data.
///
/// Used after `write_to_board_keep_port` to continue reading from the same port
/// that was used to upload the program, ensuring no UART output is missed.
#[allow(clippy::print_stdout, reason = "Printing to stdout is required for serial monitor output")]
#[allow(clippy::question_mark_used, reason = "? operator is idiomatic for propagating errors")]
#[cfg(not(tarpaulin_include))] // Cannot test serial monitoring in tarpaulin
pub fn monitor_serial_port(mut port: Box<dyn SerialPort>, debug: bool, msg_list: &mut MsgList) -> Result<(), Error> {
    port.set_timeout(Duration::from_millis(500))?;

    let running = Arc::new(AtomicBool::new(true));
    let running_stdin = Arc::clone(&running);

    // Raw mode sends each keystroke immediately without buffering and without
    // the OS intercepting Ctrl+C as a signal, so we handle 0x03 manually below.
    if let Err(err) = crossterm::terminal::enable_raw_mode() {
        msg_list.push(
            format!("Failed to enable raw terminal mode: \"{err}\""),
            None,
            None,
            MessageType::Warning,
        );
    }

    // Flush the OS-level stdin buffer, then drain the crossterm event queue.
    // Both are needed: tcflush clears bytes already in the kernel buffer
    // (e.g. a Ctrl+Z from a previous run), the poll loop clears anything
    // crossterm has already read from that buffer into its own queue.
    {
        use nix::sys::termios::{FlushArg, tcflush};
        let stdin_fd = unsafe { std::os::unix::io::BorrowedFd::borrow_raw(0) };
        let _ = tcflush(stdin_fd, FlushArg::TCIFLUSH);
    }
    while crossterm::event::poll(Duration::ZERO).unwrap_or(false) {
        let _ = crossterm::event::read();
    }

    // Clone the port so the stdin thread can write while the main loop reads.
    match port.try_clone().map_err(|e| Error::other(e.to_string())) {
        Ok(mut write_port) => {
            thread::spawn(move || {
                use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
                while running_stdin.load(Ordering::Relaxed) {
                    // Poll with a short timeout so we can check `running` regularly.
                    match event::poll(Duration::from_millis(100)) {
                        Ok(true) => {
                            if let Ok(Event::Key(KeyEvent { code, modifiers, .. })) = event::read() {
                                let byte: Option<u8> = match code {
                                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                                        running_stdin.store(false, Ordering::Relaxed);
                                        break;
                                    }
                                    KeyCode::Char(c) if modifiers.contains(KeyModifiers::CONTROL) => {
                                        // Convert Ctrl+<letter> to its control code (e.g. Ctrl+Z -> 0x1A)
                                        c.to_ascii_lowercase().try_into().ok().map(|b: u8| b & 0x1F)
                                    }
                                    KeyCode::Char(c) => c.encode_utf8(&mut [0u8; 4]).bytes().next(),
                                    KeyCode::Enter => Some(b'\r'),
                                    KeyCode::Backspace => Some(0x08),
                                    _ => None,
                                };
                                if let Some(b) = byte {
                                    let _ = write_port.write_all(&[b]);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            });
        }
        Err(err) => {
            msg_list.push(
                format!("Failed to clone serial port for input forwarding: \"{err}\""),
                None,
                None,
                MessageType::Warning,
            );
        }
    }

    println!("Monitoring serial port (Ctrl+C to stop)...\r");

    let mut read_buf = [0_u8; 1024];
    let mut halt_received = false;

    'monitor: while running.load(Ordering::Relaxed) {
        match port.read(&mut read_buf[..]) {
            Ok(bytes_read) => {
                let data = read_buf.get(..bytes_read).unwrap_or(&[]);
                // A UART break arrives as 0x00 (NUL).  Since the CPU only ever
                // sends ASCII text, 0x00 cannot appear in normal output.
                if let Some(pos) = data.iter().position(|&b| b == 0x00) {
                    print_bytes(&data[..pos], debug);
                    io::stdout().flush().unwrap_or(());
                    halt_received = true;
                    break 'monitor;
                }
                print_bytes(data, debug);
                io::stdout().flush().unwrap_or(());
            }
            Err(err) if err.kind() == io::ErrorKind::TimedOut => {}
            Err(err) => {
                msg_list.push(
                    format!("Serial monitor error: \"{err}\""),
                    None,
                    None,
                    MessageType::Error,
                );
                let _ = crossterm::terminal::disable_raw_mode();
                return Err(err);
            }
        }
    }

    let _ = crossterm::terminal::disable_raw_mode();
    if halt_received {
        println!("\r\nCPU halted.");
    } else {
        println!("\r\nSerial monitor stopped.");
    }
    drop(port);
    Ok(())
}

/// Result of a test verification run.
#[allow(clippy::arbitrary_source_item_ordering, reason = "Fields ordered by logical flow, not alphabetically")]
pub struct TestResult {
    /// Number of expected values that matched.
    pub passed: usize,
    /// Number of expected values that did not match.
    pub failed: usize,
    /// True if the test timed out before all expected values were received.
    pub timed_out: bool,
    /// Total number of expected values.
    pub total: usize,
}

/// Extract an 8-digit uppercase hex value from a UART line.
///
/// Returns Some if the trimmed line starts with exactly 8 uppercase hex digits.
fn extract_hex_value(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.len() >= 8 {
        let candidate = trimmed.get(..8).unwrap_or("");
        if candidate.len() == 8
            && candidate.chars().all(|ch| ch.is_ascii_hexdigit())
            && candidate == candidate.to_ascii_uppercase()
        {
            return Some(candidate.to_owned());
        }
    }
    None
}

/// Run test verification by monitoring serial output against expected values.
///
/// Reads UART output from the FPGA board and compares each hex line against the
/// expected values in order. Stops when all expected values are matched or timeout.
#[allow(clippy::print_stdout, reason = "Printing to stdout is required for test result output")]
#[allow(clippy::question_mark_used, reason = "Using the question mark operator for error handling")]
#[allow(clippy::arithmetic_side_effects, reason = "Index arithmetic is safe within bounds checks")]
#[cfg(not(tarpaulin_include))] // Cannot test serial hardware in tarpaulin
pub fn run_test_monitor(
    mut port: Box<dyn SerialPort>,
    expected_values: &[String],
    timeout_secs: u64,
    msg_list: &mut MsgList,
) -> TestResult {
    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);

    port.set_timeout(Duration::from_millis(500)).unwrap_or(());

    let mut buffer = [0_u8; 1024];
    let mut line_buffer = String::new();
    let mut expected_index: usize = 0;
    let mut passed: usize = 0;
    let mut failed: usize = 0;

    println!("Test mode: expecting {} UART values (timeout {}s)...", expected_values.len(), timeout_secs);

    while expected_index < expected_values.len() && start.elapsed() < timeout {
        match port.read(&mut buffer[..]) {
            Ok(bytes_read) => {
                if bytes_read > 0 {
                    let text = String::from_utf8_lossy(buffer.get(..bytes_read).unwrap_or(&[]));
                    line_buffer.push_str(&text);
                }
            }
            Err(err) if err.kind() == io::ErrorKind::TimedOut => {
                continue;
            }
            Err(err) => {
                msg_list.push(
                    format!("Serial read error during test: \"{err}\""),
                    None,
                    None,
                    MessageType::Error,
                );
                break;
            }
        }

        // Process complete lines from the buffer
        while let Some(newline_pos) = line_buffer.find('\n') {
            let line: String = line_buffer.get(..newline_pos).unwrap_or("").to_owned();
            line_buffer = line_buffer.get(newline_pos + 1..).unwrap_or("").to_owned();

            let trimmed = line.trim();

            // Skip blank lines and board acknowledgment messages
            if trimmed.is_empty() || trimmed.contains("Complete") {
                continue;
            }

            if let Some(hex_value) = extract_hex_value(trimmed) {
                if expected_index < expected_values.len() {
                    let empty = String::new();
                    let expected = expected_values.get(expected_index).unwrap_or(&empty).as_str();
                    if hex_value == expected {
                        println!("  PASS [{}/{}]: {} == {}", expected_index + 1, expected_values.len(), hex_value, expected);
                        passed += 1;
                    } else {
                        println!("  FAIL [{}/{}]: got {}, expected {}", expected_index + 1, expected_values.len(), hex_value, expected);
                        failed += 1;
                    }
                    expected_index += 1;
                }
            }
        }
    }

    let timed_out = expected_index < expected_values.len();

    if timed_out {
        let remaining = expected_values.len() - expected_index;
        msg_list.push(
            format!("Test timed out after {timeout_secs}s: {remaining} expected values not received"),
            None,
            None,
            MessageType::Warning,
        );
    }

    drop(port);

    TestResult {
        passed,
        failed,
        timed_out,
        total: expected_values.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_hex_value_valid() {
        assert_eq!(extract_hex_value("00000080"), Some("00000080".to_owned()));
        assert_eq!(extract_hex_value("FF000000"), Some("FF000000".to_owned()));
        assert_eq!(extract_hex_value("  0000ABCD  "), Some("0000ABCD".to_owned()));
    }

    #[test]
    fn test_extract_hex_value_invalid() {
        assert_eq!(extract_hex_value(""), None);
        assert_eq!(extract_hex_value("0x05"), None);
        assert_eq!(extract_hex_value("ZZZZZZZZ"), None);
        assert_eq!(extract_hex_value("0000abc"), None); // too short
    }

    #[test]
    fn test_extract_hex_value_lowercase_rejected() {
        assert_eq!(extract_hex_value("0000abcd"), None);
    }
}
