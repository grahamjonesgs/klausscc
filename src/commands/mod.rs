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

use crate::diag::{Diagnostics, MessageType};
use crate::error::Error;
use crate::link::serial::{monitor_serial, monitor_serial_port, write_to_board, write_to_board_keep_port};
use crate::output::write_binary_output_file;
use std::io;
use std::path::PathBuf;

/// Monitor the board's UART on `port` (and forward keystrokes) until the user quits.
///
/// A monitor that stops on an error is reported and returned as [`Error::Serial`].
#[cfg(not(tarpaulin_include))] // Cannot test serial hardware in tarpaulin
pub(crate) fn monitor(port: &str, debug: bool, msg_list: &mut Diagnostics) -> Result<(), Error> {
    monitor_serial(port, debug, msg_list).map_err(|err| monitor_failed(port, err, msg_list))
}

/// Report that the serial monitor stopped on an error, and make it the command's failure.
#[cfg(not(tarpaulin_include))]
fn monitor_failed(port: &str, err: io::Error, msg_list: &mut Diagnostics) -> Error {
    msg_list.warning(format!("Serial monitor stopped: \"{err}\""));
    Error::Serial {
        port: port.to_owned(),
        source: err,
    }
}

/// Report a failed send to the board's serial port, and make it the command's failure.
#[cfg(not(tarpaulin_include))]
fn send_failed(port: &str, err: io::Error, msg_list: &mut Diagnostics) -> Error {
    msg_list.error(format!("Failed to write to serial port, error \"{err}\""));
    Error::Serial {
        port: port.to_owned(),
        source: err,
    }
}

/// Where and how to send a `.kbt` wire image to the board.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SendOpts<'a> {
    /// Serial port name (`-s`) or [`AUTO_SERIAL`](crate::link::serial::AUTO_SERIAL); empty means do not send.
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
/// With `opts.monitor` the port stays open: once the image is sent the run
/// summary is printed and the board's UART is monitored until the user quits,
/// and the monitor's outcome is returned.  Otherwise this returns as soon as
/// the image is sent.  A failed send is reported and returned as
/// [`Error::Serial`].
#[cfg(not(tarpaulin_include))] // Cannot test device write in tarpaulin
pub(crate) fn send_image(wire: &str, opts: SendOpts<'_>, msg_list: &mut Diagnostics) -> Result<(), Error> {
    if opts.monitor {
        return match write_to_board_keep_port(wire, opts.port, opts.send_break, true, msg_list) {
            Ok(port) => {
                msg_list.info("Wrote to serial port");
                msg_list.print_summary();
                monitor_serial_port(port, opts.debug, msg_list).map_err(|err| monitor_failed(opts.port, err, msg_list))
            }
            Err(err) => Err(send_failed(opts.port, err, msg_list)),
        };
    }

    let error_count = msg_list.count(MessageType::Error);
    if error_count > 0 {
        msg_list.warning("Not writing to serial port due to assembly errors");
        return Err(Error::Assembly(error_count));
    }
    match write_to_board(wire, opts.port, opts.send_break, msg_list) {
        Ok(()) => {
            msg_list.info("Wrote to serial port");
            Ok(())
        }
        Err(err) => Err(send_failed(opts.port, err, msg_list)),
    }
}

/// Writes the binary file.
///
/// Writes the `.kbt` wire-format string to `binary_file_name`, reporting (and
/// returning) a failure.
#[cfg(not(tarpaulin_include))] // Cannot test device write in tarpaulin
pub(crate) fn write_binary_file(msg_list: &mut Diagnostics, binary_file_name: &str, bin_string: &str) -> Result<(), Error> {
    msg_list.info(format!("Writing binary file to {binary_file_name}"));
    write_binary_output_file(&binary_file_name, bin_string).map_err(|result_err| {
        msg_list.error(format!("Unable to write to binary code file {binary_file_name:?}, error {result_err}"));
        Error::Write {
            path: PathBuf::from(binary_file_name),
            source: result_err,
        }
    })
}
