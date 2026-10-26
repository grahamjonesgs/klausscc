//! Debugging over the GDB remote protocol, on the emulator or the board.

#[cfg(feature = "board")]
use super::load::run_elf2serial;
#[cfg(feature = "board")]
use super::SendOpts;
use crate::diag::Diagnostics;
use crate::error::Error;
use crate::layout::CODE_BASE;
use crate::loader::{load_flat, BinaryInput, Loaded};
use crate::{emulate, gdbserver};

/// Debug an ELF (or flat binary) on the emulator over the GDB remote protocol.
///
/// The image is laid out exactly as `--emulate` does (heap header, then the
/// program at 0x20), so ELF addresses equal emulator addresses when the ELF is
/// linked at 0x20 — the normal `klausscpu.ld` layout.
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_gdbserver(input: BinaryInput<'_>, port: u16, msg_list: &mut Diagnostics) -> Result<(), Error> {
    let binary_path = input.path;
    let Loaded { image, elf } = load_flat(input, msg_list)?;
    let code_base = u64::from(CODE_BASE);
    if let Some(elf) = &elf {
        if elf.base != code_base {
            msg_list.warning(format!(
                "ELF {binary_path} is linked at 0x{:08X}, not 0x{code_base:08X}: debugger addresses will not match the emulator's",
                elf.base
            ));
        }
    }
    msg_list.print_summary();

    let cpu = emulate::Cpu::new(&image.to_ddr(), image.entry);
    match gdbserver::serve(cpu, port) {
        Ok(summary) => {
            eprintln!("klausscc gdbserver: {summary}");
            Ok(())
        }
        Err(e) => {
            eprintln!("klausscc gdbserver: {e}");
            Err(Error::Gdb(e))
        }
    }
}

/// Debug an ELF on the real board over JTAG (`--gdbserver PORT --board`).
///
/// With a serial port the ELF is loaded over the UART (as without
/// `--board`) with the debug module's halt-on-load set, so the core halts on
/// the program's first instruction; without one, the running program is
/// halted where it is.  Then lldb is served as for the emulator.
#[cfg(all(feature = "board", not(tarpaulin_include)))]
pub(crate) fn run_gdbserver_board(
    input: BinaryInput<'_>,
    port: u16,
    jtag_khz: u32,
    kbt_file_name: &str,
    send: SendOpts<'_>,
    msg_list: &mut Diagnostics,
) -> Result<(), Error> {
    use crate::gdbserver_board::{self, BoardError};

    let fail = |e: BoardError| {
        eprintln!("klausscc gdbserver: {e}");
        Error::Board(e)
    };
    let (mut board, desc) = gdbserver_board::Board::open(jtag_khz).map_err(fail)?;
    eprintln!("klausscc gdbserver: {desc}");

    let pc = if send.port.is_empty() {
        gdbserver_board::attach(&mut board, None).map_err(fail)?
    } else {
        // Load only: the debugger, not the serial monitor, takes over after.
        let send = SendOpts {
            monitor: false,
            debug: false,
            ..send
        };
        let mut load = || {
            let loaded = run_elf2serial(input, kbt_file_name, send, msg_list);
            msg_list.print_summary();
            loaded.map_err(|err| BoardError::Load(format!("loading {} over {} failed ({err})", input.path, send.port)))
        };
        gdbserver_board::attach(&mut board, Some(&mut load)).map_err(fail)?
    };
    eprintln!("klausscc gdbserver: core 1 halted at 0x{pc:08X}");
    msg_list.print_summary();

    match gdbserver_board::serve(board, port) {
        Ok(summary) => {
            eprintln!("klausscc gdbserver: {summary}");
            Ok(())
        }
        Err(e) => {
            eprintln!("klausscc gdbserver: {e}");
            Err(Error::Gdb(e))
        }
    }
}

/// `--board` without the `board` feature: explain how to get it.
#[cfg(all(not(feature = "board"), not(tarpaulin_include)))]
pub(crate) fn board_unsupported(msg_list: &mut Diagnostics) -> Error {
    msg_list.error("--board needs a klausscc built with board support: cargo build --release --features board (needs libftdi1-dev)");
    Error::Usage("--board needs a klausscc built with board support")
}
