//! The serial monitor: print what the board sends and forward keystrokes to it.

use super::port::return_port;
use crate::diag::Diagnostics;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serialport::SerialPort;
use std::borrow::Cow;
use std::fmt::Write as _;
use std::io::{self, Error, Write as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

/// How long each read of the monitored port waits, so the loop notices a stop.
const MONITOR_READ_TIMEOUT: Duration = Duration::from_millis(500);

/// How long the keystroke forwarder waits for a key before checking whether to stop.
const KEY_POLL: Duration = Duration::from_millis(100);

/// The text the monitor shows for bytes received from the board: the bytes
/// themselves or, with `debug`, each byte as hex (with the character beside it
/// when printable).
fn format_bytes(data: &[u8], debug: bool) -> Cow<'_, str> {
    if !debug {
        return String::from_utf8_lossy(data);
    }
    let mut text = String::with_capacity(data.len() * 8);
    for &b in data {
        if b.is_ascii_graphic() || b == b' ' {
            let _ = write!(text, "{b:02X}('{}') ", char::from(b));
        } else {
            let _ = write!(text, "{b:02X} ");
        }
    }
    Cow::Owned(text)
}

/// Print bytes received from the board to stderr in one write (see [`format_bytes`]).
fn print_bytes(data: &[u8], debug: bool) {
    let mut stderr = io::stderr().lock();
    let _ = stderr.write_all(format_bytes(data, debug).as_bytes());
    let _ = stderr.flush();
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
                if let Ok(true) = event::poll(KEY_POLL)
                    && let Ok(Event::Key(key)) = event::read()
                {
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
        use nix::sys::termios::{FlushArg, tcflush};
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
    port.set_timeout(MONITOR_READ_TIMEOUT)?;

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
                    halt_received = true;
                    break 'monitor;
                }
                print_bytes(data, debug);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn received_bytes_are_shown_as_text_or_hex() {
        assert_eq!(format_bytes(b"Hi\r\n", false), "Hi\r\n");
        assert_eq!(format_bytes(b"\xFF", false), "\u{FFFD}");
        assert_eq!(format_bytes(b"A \n\xFF", true), "41('A') 20(' ') 0A FF ");
        assert_eq!(format_bytes(b"", true), "");
    }

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
}
