//! Sending a program image to the board over the UART.

use super::port::return_port;
use crate::diag::Diagnostics;
use crate::num::human_bytes;
use serialport::SerialPort;
use std::io::{Error, Read as _, Write as _};
use std::thread;
use std::time::Duration;

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

/// Trim newline from string.
///
/// Removes newline from end of string.
fn trim_newline(input: &mut String) {
    if input.ends_with('\n') {
        input.pop();
        if input.ends_with('\r') {
            input.pop();
        }
    }
    if input.ends_with('\r') {
        input.pop();
        if input.ends_with('\n') {
            input.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Test that line is trimmed of newline
    fn test_trim_newline1() {
        let mut test_string: String = String::from("Hello\n");
        trim_newline(&mut test_string);
        assert_eq!(test_string, "Hello");
    }

    #[test]
    // Test that line is trimmed of newline
    fn test_trim_newline2() {
        let mut test_string: String = String::from("Hello\r\n");
        trim_newline(&mut test_string);
        assert_eq!(test_string, "Hello");
    }

    #[test]
    // Test that line is trimmed of newline
    fn test_trim_newline3() {
        let mut test_string: String = String::from("Hello\n\r");
        trim_newline(&mut test_string);
        assert_eq!(test_string, "Hello");
    }
}
