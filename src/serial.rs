use crate::diag::Diagnostics;
use crate::helper::{human_bytes, trim_newline};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serialport::{SerialPort, SerialPortType, UsbPortInfo};
use std::fmt::Write as _;
use std::io::{self, Error, Read as _, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

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
pub(crate) fn find_possible_port() -> Option<String> {
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
fn return_port(port_name: &str, msg_list: &mut Diagnostics) -> Result<Box<dyn SerialPort>, Error> {
    let mut local_port_name = port_name.to_owned();
    if port_name == AUTO_SERIAL {
        if let Some(suggested_port) = find_possible_port() {
            msg_list.info(format!("No port name given, using automatic port {suggested_port}"));
            local_port_name = suggested_port;
        }
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

/// Output the code details file to given serial port, keeping the port open.
///
/// Will send the program to the serial port, wait for the response, and return the open port.
#[cfg(not(tarpaulin_include))] // Cannot test writing to serial in tarpaulin
pub(crate) fn write_to_board_keep_port(
    binary_output: &str,
    port_name: &str,
    send_break: bool,
    skip_load_response: bool,
    msg_list: &mut Diagnostics,
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

    /* Send the image with a progress bar that tracks ACTUAL transmission.
     *
     * write_all() only queues bytes into the (large) USB-serial driver buffer
     * and returns almost immediately, so a naive bar jumps straight to 100%.
     * The real multi-second transmission then used to happen inside a single
     * blocking port.flush() (tcdrain) — 23 s of dead silence, after which every
     * later message (and the board's boot log) clumped out at once.
     *
     * Instead we poll bytes_to_write() — the count still pending transmission —
     * to drive the bar, and never call the blocking tcdrain.  The driver keeps
     * transmitting in the background; the loop just watches it drain. */
    {
        let bytes = binary_output.as_bytes();
        let total = bytes.len();
        let mut written = 0_usize;
        let mut last_pct = usize::MAX;

        for window in bytes.chunks(0x10000) {
            port.write_all(window)?;
            written = written.saturating_add(window.len());
            let pending = usize::try_from(port.bytes_to_write().unwrap_or(0)).unwrap_or(0);
            let on_wire = written.saturating_sub(pending);
            let pct = on_wire.saturating_mul(100).checked_div(total).unwrap_or(100);
            if pct != last_pct {
                let line = format!("Sending to board: {pct}% ({} / {})", human_bytes(on_wire), human_bytes(total));
                eprint!("\r{line:<50}");
                last_pct = pct;
            }
        }

        /* Everything is queued; watch the driver buffer drain to the wire. */
        loop {
            let pending = usize::try_from(port.bytes_to_write().unwrap_or(0)).unwrap_or(0);
            let on_wire = total.saturating_sub(pending);
            let pct = on_wire.saturating_mul(100).checked_div(total).unwrap_or(100);
            if pct != last_pct {
                let line = format!("Sending to board: {pct}% ({} / {})", human_bytes(on_wire), human_bytes(total));
                eprint!("\r{line:<50}");
                last_pct = pct;
            }
            if pending == 0 {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        eprintln!();
    }

    if skip_load_response {
        return Ok(port);
    }

    let ret_msg_size = port.read(&mut read_buffer[..]).unwrap_or(0);

    if ret_msg_size == 0 {
        msg_list.warning("No message received from board");
        return Ok(port);
    }

    let ret_msg = String::from_utf8(read_buffer.get(..ret_msg_size).unwrap_or(b"").to_vec());

    if let Err(err) = ret_msg {
        msg_list.warning(format!("Invalid message received from board, error \"{err}\""));
        return Ok(port);
    }

    let mut print_ret_msg = ret_msg.unwrap_or_else(|_| String::default());

    trim_newline(&mut print_ret_msg); //Board can send CR/LF messages

    msg_list.info(format!("Message received from board is \"{print_ret_msg}\""));

    Ok(port)
}

/// Output the code details file to given serial port.
///
/// Will send the program to the serial port, and wait for the response.
#[cfg(not(tarpaulin_include))] // Cannot test writing to serial in tarpaulin
pub(crate) fn write_to_board(binary_output: &str, port_name: &str, send_break: bool, msg_list: &mut Diagnostics) -> Result<(), Error> {
    let _port = write_to_board_keep_port(binary_output, port_name, send_break, false, msg_list)?;
    Ok(())
}

/// Print bytes to stderr (unbuffered), optionally showing each byte as hex alongside the character.
fn print_bytes(data: &[u8], debug: bool) {
    if debug {
        for &b in data {
            if b.is_ascii_graphic() || b == b' ' {
                eprint!("{:02X}('{}') ", b, char::from(b));
            } else {
                eprint!("{b:02X} ");
            }
        }
    } else {
        eprint!("{}", String::from_utf8_lossy(data));
    }
}

/// Monitor serial port for incoming UART data from the FPGA board.
///
/// Continuously reads from the serial port and prints received data to stdout.
/// Runs until the user presses Ctrl+C, then closes the port cleanly.
#[cfg(not(tarpaulin_include))] // Cannot test serial monitoring in tarpaulin
pub(crate) fn monitor_serial(port_name: &str, debug: bool, msg_list: &mut Diagnostics) -> Result<(), Error> {
    let port = return_port(port_name, msg_list)?;
    monitor_serial_port(port, debug, msg_list)
}

/// What the serial monitor does with one terminal key event.
#[derive(Debug, PartialEq, Eq)]
enum KeyAction {
    /// Stop the monitor (Ctrl+C).
    Stop,
    /// Send these bytes to the board.
    Send(Vec<u8>),
    /// Nothing to send.
    Ignore,
}

/// Map a terminal key event to what the serial monitor sends to the board.
///
/// Only presses and auto-repeats count: Windows also reports key releases,
/// which would otherwise send every keystroke twice.  A character goes out
/// as its full UTF-8 encoding; Ctrl+<ASCII char> sends its control code.
fn key_action(event: &KeyEvent) -> KeyAction {
    if !matches!(event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
        return KeyAction::Ignore;
    }
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    match event.code {
        KeyCode::Char('c') if ctrl => KeyAction::Stop,
        // Ctrl+<letter> becomes its control code (e.g. Ctrl+Z -> 0x1A).
        KeyCode::Char(c) if ctrl && c.is_ascii() => KeyAction::Send(vec![(c.to_ascii_lowercase() as u8) & 0x1F]),
        KeyCode::Char(_) if ctrl => KeyAction::Ignore,
        KeyCode::Char(c) => KeyAction::Send(c.to_string().into_bytes()),
        KeyCode::Enter => KeyAction::Send(vec![b'\r']),
        KeyCode::Backspace => KeyAction::Send(vec![0x08]),
        _ => KeyAction::Ignore,
    }
}

/// The terminal in raw mode for as long as the guard lives.
///
/// Raw mode sends each keystroke immediately without buffering and without
/// the OS intercepting Ctrl+C as a signal; dropping the guard restores the
/// terminal on every exit path.
struct RawMode;

impl RawMode {
    /// Turn raw mode on, or warn and return `None` if the terminal refuses.
    #[cfg(not(tarpaulin_include))] // Needs a real terminal
    fn enable(msg_list: &mut Diagnostics) -> Option<Self> {
        match crossterm::terminal::enable_raw_mode() {
            Ok(()) => Some(Self),
            Err(err) => {
                msg_list.warning(format!("Failed to enable raw terminal mode: \"{err}\""));
                None
            }
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        drop(crossterm::terminal::disable_raw_mode());
    }
}

/// A thread forwarding keystrokes to the board, stopped and joined on drop.
///
/// Ctrl+C on the keyboard clears `running`, which also ends the monitor loop.
struct StdinForwarder {
    /// Shared with the monitor loop; cleared to stop both.
    running: Arc<AtomicBool>,
    /// The forwarding thread, until it is joined.
    handle: Option<thread::JoinHandle<()>>,
}

impl StdinForwarder {
    /// Start forwarding keystrokes to `write_port` while `running` is set.
    #[cfg(not(tarpaulin_include))] // Needs a real terminal and serial port
    fn spawn(mut write_port: Box<dyn SerialPort>, running: Arc<AtomicBool>) -> Self {
        use crossterm::event::{self, Event};
        let thread_running = Arc::clone(&running);
        let handle = thread::spawn(move || {
            while thread_running.load(Ordering::Relaxed) {
                // Poll with a short timeout so we can check `running` regularly.
                if let Ok(true) = event::poll(Duration::from_millis(100)) {
                    if let Ok(Event::Key(key)) = event::read() {
                        match key_action(&key) {
                            KeyAction::Stop => {
                                thread_running.store(false, Ordering::Relaxed);
                                break;
                            }
                            KeyAction::Send(bytes) => drop(write_port.write_all(&bytes)),
                            KeyAction::Ignore => {}
                        }
                    }
                }
            }
        });
        Self {
            running,
            handle: Some(handle),
        }
    }
}

impl Drop for StdinForwarder {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            drop(handle.join());
        }
    }
}

/// Discard keyboard input typed before the monitor started.
///
/// Both steps are needed: tcflush clears bytes already in the kernel buffer
/// (e.g. a Ctrl+Z from a previous run), the poll loop clears anything
/// crossterm has already read from that buffer into its own queue.
/// tcflush is Unix-only; on Windows the crossterm drain alone is used.
#[cfg(not(tarpaulin_include))] // Needs a real terminal
fn flush_stdin() {
    #[cfg(unix)]
    {
        use nix::sys::termios::{tcflush, FlushArg};
        use std::os::fd::AsFd as _;
        let _ = tcflush(io::stdin().as_fd(), FlushArg::TCIFLUSH);
    }
    while crossterm::event::poll(Duration::ZERO).unwrap_or(false) {
        drop(crossterm::event::read());
    }
}

/// Monitor an already-open serial port for incoming UART data.
///
/// Used after `write_to_board_keep_port` to continue reading from the same port
/// that was used to upload the program, ensuring no UART output is missed.
#[cfg(not(tarpaulin_include))] // Cannot test serial monitoring in tarpaulin
pub(crate) fn monitor_serial_port(mut port: Box<dyn SerialPort>, debug: bool, msg_list: &mut Diagnostics) -> Result<(), Error> {
    port.set_timeout(Duration::from_millis(500))?;

    let running = Arc::new(AtomicBool::new(true));

    // Clone the port so the stdin thread can write while the main loop reads.
    let write_port = match port.try_clone() {
        Ok(write_port) => Some(write_port),
        Err(err) => {
            msg_list.warning(format!("Failed to clone serial port for input forwarding: \"{err}\""));
            None
        }
    };

    // Raw mode only once keystrokes are forwarded: without the forwarder
    // nothing would see Ctrl+C, so the terminal keeps its normal mode and
    // Ctrl+C (SIGINT) still stops the monitor.  Declared before the forwarder
    // so the forwarder is dropped (stopped) first.
    let raw_mode = if write_port.is_some() { RawMode::enable(msg_list) } else { None };
    flush_stdin();
    let forwarder = write_port.map(|write_port| StdinForwarder::spawn(write_port, Arc::clone(&running)));

    eprintln!("Monitoring serial port (Ctrl+C to stop)...\r");

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
                    io::stderr().flush().unwrap_or(());
                    halt_received = true;
                    break 'monitor;
                }
                print_bytes(data, debug);
                io::stderr().flush().unwrap_or(());
            }
            Err(err) if err.kind() == io::ErrorKind::TimedOut => {}
            Err(err) => {
                msg_list.error(format!("Serial monitor error: \"{err}\""));
                return Err(err);
            }
        }
    }

    // Stop the forwarder, then leave raw mode, before the closing message.
    drop(forwarder);
    drop(raw_mode);
    if halt_received {
        eprintln!("\r\nCPU halted.");
    } else {
        eprintln!("\r\nSerial monitor stopped.");
    }
    drop(port);
    Ok(())
}

/// Result of a test verification run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TestResult {
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
        if candidate.len() == 8 && candidate.chars().all(|ch| ch.is_ascii_hexdigit()) && candidate == candidate.to_ascii_uppercase() {
            return Some(candidate.to_owned());
        }
    }
    None
}

/// Run test verification by monitoring serial output against expected values.
///
/// Reads UART output from the FPGA board and compares each hex line against the
/// expected values in order. Stops when all expected values are matched or timeout.
/// A serial read error (other than a read timeout) is reported and returned:
/// it is not a test timeout.
#[cfg(not(tarpaulin_include))] // Cannot test serial hardware in tarpaulin
pub(crate) fn run_test_monitor(
    mut port: Box<dyn SerialPort>,
    expected_values: &[String],
    timeout_secs: u64,
    msg_list: &mut Diagnostics,
) -> io::Result<TestResult> {
    port.set_timeout(Duration::from_millis(500)).unwrap_or(());

    println!(
        "Test mode: expecting {} UART values (timeout {}s)...",
        expected_values.len(),
        timeout_secs
    );

    let result = match_uart_values(&mut port, expected_values, Duration::from_secs(timeout_secs));
    match &result {
        Ok(result) if result.timed_out => {
            let remaining = result.total - result.passed - result.failed;
            msg_list.warning(format!("Test timed out after {timeout_secs}s: {remaining} expected values not received"));
        }
        Ok(_) => {}
        Err(err) => {
            msg_list.error(format!("Serial read error during test: \"{err}\""));
        }
    }
    result
}

/// Compare the hex lines read from `reader` against `expected_values`, in order.
///
/// Prints a PASS/FAIL line per value.  Stops when every expected value has
/// been checked or `timeout` has passed (`timed_out` is then set); a read
/// error other than a read timeout stops it and is returned.
fn match_uart_values<R: io::Read + ?Sized>(reader: &mut R, expected_values: &[String], timeout: Duration) -> io::Result<TestResult> {
    let start = Instant::now();
    let total = expected_values.len();
    let mut buffer = [0_u8; 1024];
    let mut line_buffer = String::new();
    let mut expected_index: usize = 0;
    let mut passed: usize = 0;
    let mut failed: usize = 0;

    while expected_index < total && start.elapsed() < timeout {
        match reader.read(&mut buffer[..]) {
            Ok(bytes_read) => {
                line_buffer.push_str(&String::from_utf8_lossy(buffer.get(..bytes_read).unwrap_or(&[])));
            }
            Err(err) if err.kind() == io::ErrorKind::TimedOut => continue,
            Err(err) => return Err(err),
        }

        // Process complete lines from the buffer
        while let Some(newline_pos) = line_buffer.find('\n') {
            let line: String = line_buffer.drain(..=newline_pos).collect();
            let trimmed = line.trim();

            // Skip blank lines and board acknowledgment messages
            if trimmed.is_empty() || trimmed.contains("Complete") {
                continue;
            }

            let Some(hex_value) = extract_hex_value(trimmed) else {
                continue;
            };
            let Some(expected) = expected_values.get(expected_index) else {
                continue;
            };
            if hex_value == *expected {
                println!("  PASS [{}/{total}]: {hex_value} == {expected}", expected_index + 1);
                passed += 1;
            } else {
                println!("  FAIL [{}/{total}]: got {hex_value}, expected {expected}", expected_index + 1);
                failed += 1;
            }
            expected_index += 1;
        }
    }

    Ok(TestResult {
        passed,
        failed,
        timed_out: expected_index < total,
        total,
    })
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

    // ---- key_action ----------------------------------------------------------

    fn key(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> KeyEvent {
        KeyEvent::new_with_kind(code, modifiers, kind)
    }

    fn press(code: KeyCode) -> KeyEvent {
        key(code, KeyModifiers::NONE, KeyEventKind::Press)
    }

    #[test]
    fn key_releases_send_nothing() {
        // Windows reports a release after every press; it must not send the key again.
        let release = key(KeyCode::Char('a'), KeyModifiers::NONE, KeyEventKind::Release);
        assert_eq!(key_action(&release), KeyAction::Ignore);
        let ctrl_c_release = key(KeyCode::Char('c'), KeyModifiers::CONTROL, KeyEventKind::Release);
        assert_eq!(key_action(&ctrl_c_release), KeyAction::Ignore);
        let repeat = key(KeyCode::Char('a'), KeyModifiers::NONE, KeyEventKind::Repeat);
        assert_eq!(key_action(&repeat), KeyAction::Send(b"a".to_vec()));
    }

    #[test]
    fn non_ascii_keys_send_their_full_utf8_encoding() {
        assert_eq!(key_action(&press(KeyCode::Char('a'))), KeyAction::Send(b"a".to_vec()));
        assert_eq!(key_action(&press(KeyCode::Char('é'))), KeyAction::Send(vec![0xC3, 0xA9]));
        assert_eq!(key_action(&press(KeyCode::Char('€'))), KeyAction::Send(vec![0xE2, 0x82, 0xAC]));
    }

    #[test]
    fn control_keys_map_to_control_codes() {
        let ctrl = |c| key(KeyCode::Char(c), KeyModifiers::CONTROL, KeyEventKind::Press);
        assert_eq!(key_action(&ctrl('c')), KeyAction::Stop);
        assert_eq!(key_action(&ctrl('z')), KeyAction::Send(vec![0x1A]));
        assert_eq!(key_action(&ctrl('A')), KeyAction::Send(vec![0x01]));
        assert_eq!(key_action(&ctrl('é')), KeyAction::Ignore);
        assert_eq!(key_action(&press(KeyCode::Enter)), KeyAction::Send(vec![b'\r']));
        assert_eq!(key_action(&press(KeyCode::Backspace)), KeyAction::Send(vec![0x08]));
        assert_eq!(key_action(&press(KeyCode::Left)), KeyAction::Ignore);
    }

    // ---- ports_found_message -------------------------------------------------

    #[test]
    fn ports_found_message_counts_every_port() {
        assert_eq!(ports_found_message(0, ""), "no ports were found");
        assert_eq!(ports_found_message(1, "A"), "only port A was found");
        assert_eq!(ports_found_message(2, "A,\nB"), "the following 2 ports were found:\nA,\nB");
        assert_eq!(ports_found_message(3, "A,\nB,\nC"), "the following 3 ports were found:\nA,\nB,\nC");
    }

    // ---- match_uart_values ---------------------------------------------------

    /// A reader that returns scripted chunks, then read timeouts.
    struct Script(std::collections::VecDeque<io::Result<Vec<u8>>>);

    impl Script {
        fn new(chunks: Vec<io::Result<&[u8]>>) -> Self {
            Self(chunks.into_iter().map(|c| c.map(<[u8]>::to_vec)).collect())
        }
    }

    impl io::Read for Script {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.0.pop_front() {
                Some(Ok(chunk)) => {
                    buf[..chunk.len()].copy_from_slice(&chunk);
                    Ok(chunk.len())
                }
                Some(Err(err)) => Err(err),
                None => {
                    thread::sleep(Duration::from_millis(1));
                    Err(io::Error::from(io::ErrorKind::TimedOut))
                }
            }
        }
    }

    fn expected(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn uart_values_are_matched_across_chunks() {
        let mut reader = Script::new(vec![
            Ok(b"Load Complete\r\n0000"),
            Err(io::Error::from(io::ErrorKind::TimedOut)),
            Ok(b"0001\r\n\r\n"),
            Ok(b"0000000B\n"),
        ]);
        let result = match_uart_values(&mut reader, &expected(&["00000001", "0000000A"]), Duration::from_secs(5)).unwrap();
        assert_eq!(
            result,
            TestResult {
                passed: 1,
                failed: 1,
                timed_out: false,
                total: 2,
            }
        );
    }

    #[test]
    fn uart_values_missing_at_the_deadline_time_out() {
        let mut reader = Script::new(vec![Ok(b"00000001\n")]);
        let result = match_uart_values(&mut reader, &expected(&["00000001", "00000002"]), Duration::from_millis(20)).unwrap();
        assert_eq!(
            result,
            TestResult {
                passed: 1,
                failed: 0,
                timed_out: true,
                total: 2,
            }
        );
    }

    #[test]
    fn a_serial_read_error_is_returned_not_reported_as_a_timeout() {
        let mut reader = Script::new(vec![Ok(b"00000001\n"), Err(io::Error::from(io::ErrorKind::BrokenPipe))]);
        let err = match_uart_values(&mut reader, &expected(&["00000001", "00000002"]), Duration::from_secs(5)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }
}
