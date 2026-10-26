//! Sending a program image to the board over the UART.

use super::SerialTarget;
use super::port::{BAUD_RATE, SerialError, open_port};
use crate::diag::Diagnostics;
use crate::num::human_bytes;
use serialport::{ClearBuffer, SerialPort};
use std::io::{Read as _, Write as _};
use std::str::Utf8Error;
use std::thread;
use std::time::{Duration, Instant};

/// How long the UART break that resets the board into its loader lasts.
const BREAK_DURATION: Duration = Duration::from_millis(100);

/// How long the board takes to reset into its loader.
const RESET_DELAY: Duration = Duration::from_millis(500);

/// How much of the image is queued on the port at a time, between progress updates.
const SEND_CHUNK: usize = 0x10000;

/// How often the progress bar checks the driver's transmit buffer once
/// everything is queued.
const DRAIN_POLL: Duration = Duration::from_millis(50);

/// Slack on top of the wire time before the drain is given up on.
const DRAIN_SLACK: Duration = Duration::from_secs(5);

/// Bits on the wire per byte: start, eight data, stop (8N1).
const BITS_PER_BYTE: u64 = 10;

/// How the board is reset into its loader before the image is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResetMethod {
    /// A UART break, which resets the CPU whatever it is running.
    Break,
    /// The loader's `S` command, for a CPU already in its loader (`-n`).
    Command,
}

/// Whether to wait for the loader's reply once the image is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AwaitAck {
    /// Read and report the reply.
    Wait,
    /// Leave the reply for whatever reads the port next (the monitor).
    Skip,
}

/// Send the `.kbt` wire string `wire` to the board over `target`, and
/// return the still-open port.
///
/// Resets the board with `reset`, sends the image with a progress bar and,
/// with [`AwaitAck::Wait`], reports the loader's reply.
#[cfg(not(tarpaulin_include))] // Cannot test writing to serial in tarpaulin
pub(crate) fn write_to_board(
    wire: &str,
    target: SerialTarget<'_>,
    reset: ResetMethod,
    ack: AwaitAck,
    msg_list: &mut Diagnostics,
) -> Result<Box<dyn SerialPort>, SerialError> {
    let mut port = open_port(target, msg_list)?;

    match reset {
        ResetMethod::Break => {
            port.set_break()?;
            thread::sleep(BREAK_DURATION);
            port.clear_break()?;
        }
        ResetMethod::Command => port.write_all(b"S")?,
    }
    thread::sleep(RESET_DELAY);
    port.flush()?;
    // Drop anything the board printed before its loader started.
    port.clear(ClearBuffer::Input)?;

    send_with_progress(port.as_mut(), wire.as_bytes(), msg_list)?;

    if ack == AwaitAck::Wait {
        let mut read_buffer = [0; 1024];
        let received = port.read(&mut read_buffer[..]).unwrap_or(0);
        report_reply(read_buffer.get(..received).unwrap_or_default(), msg_list);
    }
    Ok(port)
}

/// Send `bytes` with a progress bar that tracks what is actually on the wire.
///
/// `write_all` only queues bytes into the (large) USB-serial driver buffer
/// and returns almost immediately, so a bar driven by it jumps straight to
/// 100%, and the real multi-second transmission would then happen inside a
/// blocking `flush` (tcdrain): seconds of silence, after which every later
/// message (and the board's boot log) clumps out at once.  Instead the bar
/// follows `bytes_to_write`, the count still pending transmission, and the
/// loop watches the driver drain rather than calling tcdrain.  A drain that
/// takes well past the wire time is warned about and left to finish.
#[cfg(not(tarpaulin_include))] // Cannot test writing to serial in tarpaulin
fn send_with_progress(port: &mut dyn SerialPort, bytes: &[u8], msg_list: &mut Diagnostics) -> Result<(), SerialError> {
    let total = bytes.len();
    let mut progress = Progress::new(total);
    let pending = |port: &dyn SerialPort| usize::try_from(port.bytes_to_write().unwrap_or(0)).unwrap_or(0);

    let mut written = 0_usize;
    for window in bytes.chunks(SEND_CHUNK) {
        port.write_all(window)?;
        written = written.saturating_add(window.len());
        progress.show(written.saturating_sub(pending(port)));
    }

    // Everything is queued; watch the driver buffer drain to the wire.
    let deadline = Instant::now() + wire_time(total) + DRAIN_SLACK;
    loop {
        let left = pending(port);
        progress.show(total.saturating_sub(left));
        if left == 0 {
            break;
        }
        if Instant::now() >= deadline {
            eprintln!();
            msg_list.warning(format!("Serial port still has {} to send", human_bytes(left)));
            return Ok(());
        }
        thread::sleep(DRAIN_POLL);
    }
    eprintln!();
    Ok(())
}

/// How long `len` bytes take on the wire at the board's baud rate.
fn wire_time(len: usize) -> Duration {
    let bits = u64::try_from(len).unwrap_or(u64::MAX).saturating_mul(BITS_PER_BYTE);
    Duration::from_millis(bits.saturating_mul(1000) / u64::from(BAUD_RATE))
}

/// The `Sending to board` progress line on stderr.
struct Progress {
    /// Bytes to send.
    total: usize,
    /// The percentage last shown, so the line is only redrawn when it changes.
    last_pct: Option<usize>,
}

impl Progress {
    /// A progress line for sending `total` bytes; nothing is shown yet.
    const fn new(total: usize) -> Self {
        Self { total, last_pct: None }
    }

    /// Show that `sent` bytes are on the wire, if the percentage changed.
    fn show(&mut self, sent: usize) {
        let pct = percent(sent, self.total);
        if self.last_pct != Some(pct) {
            let line = format!("Sending to board: {pct}% ({} / {})", human_bytes(sent), human_bytes(self.total));
            eprint!("\r{line:<50}");
            self.last_pct = Some(pct);
        }
    }
}

/// `sent` as a percentage of `total`; an empty image is 100% sent.
fn percent(sent: usize, total: usize) -> usize {
    sent.saturating_mul(100).checked_div(total).unwrap_or(100)
}

/// Report the loader's reply to the image, `received`.
fn report_reply(received: &[u8], msg_list: &mut Diagnostics) {
    if received.is_empty() {
        msg_list.warning("No message received from board");
        return;
    }
    match board_reply(received) {
        Ok(reply) => msg_list.info(format!("Message received from board is \"{reply}\"")),
        Err(err) => msg_list.warning(format!("Invalid message received from board, error \"{err}\"")),
    }
}

/// The loader's reply as text, without the line ending (the board can send CR/LF).
fn board_reply(received: &[u8]) -> Result<&str, Utf8Error> {
    Ok(std::str::from_utf8(received)?.trim_end_matches(['\r', '\n']))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // The reply is trimmed of its line ending, whichever order CR and LF come in.
    fn board_reply_drops_the_line_ending() {
        assert_eq!(board_reply(b"Hello\n"), Ok("Hello"));
        assert_eq!(board_reply(b"Hello\r\n"), Ok("Hello"));
        assert_eq!(board_reply(b"Hello\n\r"), Ok("Hello"));
        assert_eq!(board_reply(b"Load Complete"), Ok("Load Complete"));
        assert!(board_reply(b"\xFF\n").is_err());
    }

    #[test]
    fn report_reply_reports_once() {
        let texts = |received: &[u8]| {
            let mut msgs = Diagnostics::new();
            report_reply(received, &mut msgs);
            msgs.messages().iter().map(|m| m.text().to_owned()).collect::<Vec<_>>()
        };
        assert_eq!(texts(b"OK\r\n"), ["Message received from board is \"OK\""]);
        assert_eq!(texts(b""), ["No message received from board"]);
        assert_eq!(
            texts(b"\xFF"),
            ["Invalid message received from board, error \"invalid utf-8 sequence of 1 bytes from index 0\""]
        );
    }

    #[test]
    fn percent_of_an_empty_image_is_100() {
        assert_eq!(percent(0, 0), 100);
        assert_eq!(percent(0, 200), 0);
        assert_eq!(percent(50, 200), 25);
        assert_eq!(percent(200, 200), 100);
    }

    #[test]
    fn wire_time_is_ten_bits_a_byte() {
        assert_eq!(wire_time(0), Duration::ZERO);
        assert_eq!(wire_time(300_000), Duration::from_secs(1));
        assert_eq!(wire_time(3_000_000), Duration::from_secs(10));
    }
}
