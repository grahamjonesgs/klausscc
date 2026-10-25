//! Subcommand handlers — each `run_*` function drives one CLI mode end to end.
//!
//! * [`load`]: put a program on the board — net-load, mem-out, elf2serial and
//!   sending a `.kbt`.
//! * [`emulate`]: run programs on the golden-model emulator (`--emulate`,
//!   `--emulate-test`).
//! * [`gdb`]: debug on the emulator or the board over the GDB remote protocol.
//! * [`test_runner`]: run tests on the board and check their UART output
//!   (`-T`, `-L`).
//!
//! This module holds what they share: sending an image to the board
//! ([`send_image`]) and writing the `.kbt` file.

mod emulate;
mod gdb;
mod load;
mod test_runner;

pub(crate) use emulate::{run_emulate, run_emulate_elf, run_emulate_test};
#[cfg(not(feature = "board"))]
pub(crate) use gdb::board_unsupported;
pub(crate) use gdb::run_gdbserver;
#[cfg(feature = "board")]
pub(crate) use gdb::run_gdbserver_board;
pub(crate) use load::{run_elf2serial, run_kbt_send, run_mem_out, run_netload};
pub(crate) use test_runner::{run_test_list, run_test_mode};

use crate::error::Error;
use crate::files::write_binary_output_file;
use crate::messages::{print_results, MessageType, MsgList};
use crate::serial::{monitor_serial_port, write_to_board, write_to_board_keep_port};
use std::io;
use std::path::PathBuf;
use std::time::Instant;

/// Report that the serial monitor stopped on an error, and make it the command's failure.
#[cfg(not(tarpaulin_include))]
pub(crate) fn monitor_failed(port: &str, err: io::Error, msg_list: &mut MsgList) -> Error {
    msg_list.push(format!("Serial monitor stopped: \"{err}\""), None, None, MessageType::Warning);
    Error::Serial {
        port: port.to_owned(),
        source: err,
    }
}

/// Report a failed send to the board's serial port, and make it the command's failure.
#[cfg(not(tarpaulin_include))]
fn send_failed(port: &str, err: io::Error, msg_list: &mut MsgList, start_time: Instant) -> Error {
    msg_list.push(format!("Failed to write to serial port, error \"{err}\""), None, None, MessageType::Error);
    print_results(msg_list, start_time);
    Error::Serial {
        port: port.to_owned(),
        source: err,
    }
}

/// Where and how to send a `.kbt` wire image to the board.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SendOpts<'a> {
    /// Serial port name (`-s`) or [`AUTO_SERIAL`]; empty means do not send.
    pub(crate) port: &'a str,
    /// Send a break first, so the board resets into its loader (no `-n`).
    pub(crate) send_break: bool,
    /// Keep the port open and monitor the board's UART afterwards (`-m`).
    pub(crate) monitor: bool,
    /// Show the bytes the monitor receives (`-d`).
    pub(crate) debug: bool,
}

/// Send the `.kbt` wire string `wire` to the board over `opts.port`.
///
/// With `opts.monitor` the port stays open: once the image is sent the summary
/// is printed and the board's UART is monitored until the user quits, and the
/// monitor's outcome is returned.  Otherwise this returns as soon as the image
/// is sent, and the caller prints the summary.  A failed send is reported and
/// returned as [`Error::Serial`].
#[cfg(not(tarpaulin_include))] // Cannot test device write in tarpaulin
pub(crate) fn send_image(wire: &str, opts: SendOpts<'_>, msg_list: &mut MsgList, start_time: Instant) -> Result<(), Error> {
    if opts.monitor {
        return match write_to_board_keep_port(wire, opts.port, opts.send_break, true, msg_list) {
            Ok(port) => {
                msg_list.push("Wrote to serial port".to_owned(), None, None, MessageType::Information);
                print_results(msg_list, start_time);
                monitor_serial_port(port, opts.debug, msg_list).map_err(|err| monitor_failed(opts.port, err, msg_list))
            }
            Err(err) => Err(send_failed(opts.port, err, msg_list, start_time)),
        };
    }

    let error_count = msg_list.number_by_type(MessageType::Error);
    if error_count > 0 {
        msg_list.push(
            "Not writing to serial port due to assembly errors".to_owned(),
            None,
            None,
            MessageType::Warning,
        );
        return Err(Error::Assembly(error_count));
    }
    match write_to_board(wire, opts.port, opts.send_break, msg_list) {
        Ok(()) => {
            msg_list.push("Wrote to serial port".to_owned(), None, None, MessageType::Information);
            Ok(())
        }
        Err(err) => {
            msg_list.push(format!("Failed to write to serial port, error \"{err}\""), None, None, MessageType::Error);
            Err(Error::Serial {
                port: opts.port.to_owned(),
                source: err,
            })
        }
    }
}

/// Writes the binary file.
///
/// Writes the `.kbt` wire-format string to `binary_file_name`, reporting (and
/// returning) a failure.
#[cfg(not(tarpaulin_include))] // Cannot test device write in tarpaulin
pub(crate) fn write_binary_file(msg_list: &mut MsgList, binary_file_name: &str, bin_string: &str) -> Result<(), Error> {
    msg_list.push(format!("Writing binary file to {binary_file_name}"), None, None, MessageType::Information);
    write_binary_output_file(&binary_file_name, bin_string).map_err(|result_err| {
        msg_list.push(
            format!("Unable to write to binary code file {binary_file_name:?}, error {result_err}"),
            None,
            None,
            MessageType::Error,
        );
        Error::Write {
            path: PathBuf::from(binary_file_name),
            source: result_err,
        }
    })
}
