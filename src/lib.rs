//! Klausscc: assembler, ISA emulator, GDB servers and loaders for the Klauss
//! FPGA CPU.
//!
//! The library holds every part of the tool; the `klausscc` binary is
//! [`run`], which reads the command line and runs the selected mode.  The
//! golden-model [`emulate`] module is public so it can be driven directly.

/// The assembler: `.kla` source in, addressed machine code out.
mod asm;
/// Module defining the command-line interface (clap).
mod cli;
/// Module of subcommand handlers (the `run_*` entry points).
mod commands;
/// Diagnostics: the errors, warnings and notes reported to the user.
mod diag;
/// Module: independent ISA emulator (golden-model trace generator).
pub mod emulate;
/// Module defining the crate error type and exit codes.
mod error;
/// Module to manage file read and write.
mod files;
/// GDB remote-protocol server for the emulator (`--gdbserver`).
mod gdbserver;
/// GDB remote-protocol server for the board (`--gdbserver --board`).
#[cfg(feature = "board")]
mod gdbserver_board;
/// Module of helper functions.
mod helper;
/// The program image and its `.kbt`, DDR and `$readmemh` formats.
mod image;
/// JTAG over the board's FT2232H, and the debug module's DMI.  The pure MPSSE
/// framing is also built for tests, so they run without libftdi.
#[cfg(any(test, feature = "board"))]
mod jtag;
/// Module to manage labels.
mod labels;
/// The memory map: heap header, code base, heap start and the MMIO window.
mod layout;
/// Loading an ELF or flat binary as a board program.
mod loader;
/// Module to manage macros.
mod macros;
/// Module to stream a flat DDR image to the board over TCP (network boot).
mod netload;
/// Module for integer literal parsing.
mod num;
/// Module to manage opcodes.
mod opcodes;
/// Module to write to serial and read response.
mod serial;
use asm::Assembly;
use clap::{CommandFactory as _, Parser as _};
use cli::{Cli, Mode};
use commands::{
    monitor, run_elf2serial, run_emulate, run_emulate_elf, run_emulate_test, run_gdbserver, run_kbt_send, run_mem_out, run_netload, run_test_list,
    run_test_mode, send_image, write_binary_file, SendOpts,
};
use diag::{Diagnostics, MessageType};
use error::Error;
use files::write_code_output_file;
use macros::{expand_embedded_macros, Macro};
use opcodes::{v2_opcodes_and_macros, Opcode};
use std::path::PathBuf;
use std::process::ExitCode;

/// Run the `klausscc` command with the process's command line.
///
/// Parses the command line and resolves its [`Mode`] before doing any work,
/// runs the mode, prints the run summary (the error and warning counts) once
/// and turns the outcome into the process exit status: `0` on success, `2`
/// when tests failed, `3` when a test timed out, `1` otherwise.  Failures are
/// reported to the user where they happen, so nothing more is printed here.
#[cfg(not(tarpaulin_include))] // Cannot test main in tarpaulin
#[must_use]
pub fn run() -> ExitCode {
    /* try_parse() rather than parse(): clap exits with status 2 on a bad
     * command line, which would read as "tests failed". */
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            // --help and --version go to stdout and succeed; anything else is a usage error.
            let _ = err.print();
            return if err.use_stderr() {
                Error::Usage("invalid command line").exit_code()
            } else {
                ExitCode::SUCCESS
            };
        }
    };
    let mode = match cli.mode() {
        Ok(mode) => mode,
        Err(err) => {
            // Reported like clap's own usage errors.
            let _ = Cli::command().error(clap::error::ErrorKind::ArgumentConflict, &err).print();
            return err.exit_code();
        }
    };

    /* Stream messages as they happen rather than dumping them all at the end:
     * during a board load the transfer is otherwise silent and every status
     * line (plus the board's boot log behind it) appears in one burst once the
     * monitor starts.  The summary is then just the final counts line. */
    let mut msg_list = Diagnostics::streaming();
    let outcome = run_mode(&cli, &mode, &mut msg_list);
    // A no-op if the command printed it before handing over the terminal.
    msg_list.print_summary();
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => err.exit_code(),
    }
}

/// Call the handler for `mode`; the other options come from `cli`.
#[cfg(not(tarpaulin_include))] // Cannot test main in tarpaulin
fn run_mode(cli: &Cli, mode: &Mode<'_>, msg_list: &mut Diagnostics) -> Result<(), Error> {
    let send = SendOpts {
        port: cli.serial_port(),
        send_break: !cli.no_break,
        monitor: cli.monitor,
        debug: cli.debug,
    };
    match *mode {
        // Monitor-only mode: `-m` on its own (optionally with `-s`) just opens
        // the serial monitor.  With no `-s` the first USB serial port is
        // auto-detected.
        Mode::Monitor => monitor(cli.monitor_port(), cli.debug, msg_list),
        // net-load mode: flatten an ELF (or take a flat binary) and stream it
        // to the board over TCP — no kbt, no UART. See NETBOOT_PLAN.md / netboot.c.
        Mode::NetLoad { input, ip } => {
            run_netload(cli.binary_input(input), ip, cli.port, msg_list)?;

            /* After a successful net-load, optionally monitor the board's UART
             * (and forward keystrokes), exactly like -s -m.  The load itself is
             * over TCP, so the monitor needs its own serial port: honour -s if
             * given, otherwise auto-detect the first USB serial port (same as
             * -s with no value).  The load's summary comes first, as the
             * monitor runs until the user quits. */
            if cli.monitor {
                msg_list.print_summary();
                monitor(cli.monitor_port(), cli.debug, msg_list)?;
            }
            Ok(())
        }
        // mem-out mode: flatten an ELF (or take a flat binary) and write a
        // $readmemh image for the resident boot ROM (boot_rom.v). See
        // NETBOOT_PLAN.md Phase 2.
        Mode::MemOut { input, ref mem_file } => run_mem_out(input, mem_file, msg_list),
        // ELF / flat binary input: debug, emulate, or convert directly to the
        // board wire format and optionally send it.  No opcode file is
        // required — the built-in table is only used for the .code
        // disassembly listing written alongside the .kbt.
        Mode::Gdb {
            input,
            port,
            board: true,
            ref kbt,
        } => {
            #[cfg(feature = "board")]
            return commands::run_gdbserver_board(cli.binary_input(input), port, cli.jtag_khz, kbt, send, msg_list);
            #[cfg(not(feature = "board"))]
            {
                let _ = (input, port, kbt);
                Err(commands::board_unsupported(msg_list))
            }
        }
        Mode::Gdb {
            input, port, board: false, ..
        } => run_gdbserver(cli.binary_input(input), port, msg_list),
        Mode::EmulateBinary { input, trace } => run_emulate_elf(cli.binary_input(input), trace, cli.max_instructions, msg_list),
        Mode::SendBinary { input, ref kbt } => run_elf2serial(cli.binary_input(input), kbt, send, msg_list),
        // .kbt input: already in board wire format — send and/or monitor verbatim.
        Mode::SendKbt(input) => run_kbt_send(input, send, msg_list),
        Mode::Opcodes => {
            use files::output_macros_opcodes_html;

            let (oplist, macro_list) = opcode_tables(msg_list);
            let html_result = output_macros_opcodes_html(
                "klausscc_opcodes_v2".to_owned(),
                &oplist,
                &macro_list,
                msg_list,
                cli.opcodes,
                cli.textmate,
            );
            if let Err(result_err) = &html_result {
                msg_list.error(format!("Error {result_err} writing opcode list to HTML"));
            }
            html_result.map_err(Error::OpcodeList)
        }
        // Emulator batch-verify mode: assemble + emulate each .kla and check UART.
        Mode::EmulateTest(path) => {
            let (oplist, macro_list) = opcode_tables(msg_list);
            run_emulate_test(&oplist, &macro_list, path, cli.max_instructions, msg_list)
        }
        // Batch test list mode.
        Mode::TestList(path) => {
            let (oplist, macro_list) = opcode_tables(msg_list);
            run_test_list(&oplist, &macro_list, path, cli.serial_port(), cli.test_timeout, !cli.no_break, msg_list)
        }
        // Emulator mode: build the flat DDR image from the assembled program
        // and run the golden-model.
        Mode::Emulate { input, trace } => {
            let assembly = assemble(input, msg_list)?;
            run_emulate(&assembly, input, trace, cli.max_instructions, msg_list)
        }
        Mode::Assemble { input, ref code, ref kbt } => run_assemble(cli, input, code, kbt, send, msg_list),
    }
}

/// The built-in opcode table (ISA encoding v2) and its macros, expanded.
///
/// The old external `opcode_select.vh` file is gone; the lists are built in code.
#[cfg(not(tarpaulin_include))] // Cannot test main in tarpaulin
fn opcode_tables(msg_list: &mut Diagnostics) -> (Vec<Opcode>, Vec<Macro>) {
    let (oplist, macro_list_raw) = v2_opcodes_and_macros(msg_list);
    let macro_list = expand_embedded_macros(macro_list_raw, msg_list);
    (oplist, macro_list)
}

/// Assemble `input` with the built-in opcode table.
///
/// A build with errors still comes back, so the `.code` listing can show
/// where they are; only an unreadable source is an `Err`.
#[cfg(not(tarpaulin_include))] // Cannot test main in tarpaulin
fn assemble(input: &str, msg_list: &mut Diagnostics) -> Result<Assembly, Error> {
    let (oplist, macro_list) = opcode_tables(msg_list);
    asm::assemble(input, &oplist, &macro_list, msg_list).ok_or_else(|| Error::Assembly(msg_list.count(MessageType::Error).max(1)))
}

/// Assemble `input`, write its `.code` listing and `.kbt`, and send, test or
/// monitor it as the command line asks.
#[cfg(not(tarpaulin_include))] // Cannot test main in tarpaulin
fn run_assemble(
    cli: &Cli,
    input: &str,
    code_file_name: &str,
    binary_file_name: &str,
    send: SendOpts<'_>,
    msg_list: &mut Diagnostics,
) -> Result<(), Error> {
    use std::fs::remove_file;

    let assembly = assemble(input, msg_list)?;
    let output_serial_port = cli.serial_port();

    // The .code listing is written even when assembly failed: it shows where.
    if let Err(result_err) = write_code_output_file(code_file_name, &assembly.lines, msg_list) {
        msg_list.error(format!("Unable to write to code file {code_file_name}, error {result_err}"));
        return Err(Error::Write {
            path: PathBuf::from(code_file_name),
            source: result_err,
        });
    }

    // Failure from writing the .kbt or sending it; assembly errors are counted below.
    let mut outcome: Result<(), Error> = Ok(());
    if msg_list.count(MessageType::Error) == 0 {
        if let Some(bin_string) = assembly.to_kbt(msg_list) {
            outcome = write_binary_file(msg_list, binary_file_name, &bin_string);
            if outcome.is_ok() && !output_serial_port.is_empty() {
                if cli.test {
                    // Test mode: send to board, keep port open, then verify UART output
                    return run_test_mode(msg_list, &bin_string, output_serial_port, input, cli.test_timeout, !cli.no_break);
                }
                outcome = send_image(&bin_string, send, msg_list);
                if cli.monitor {
                    // Monitor mode: the board's UART was monitored after the send.
                    return outcome;
                }
            }
        } else {
            if remove_file(binary_file_name).is_ok() {
                msg_list.warning("Removed old binary file");
            }
            msg_list.warning("Not writing binary file due to assembly errors");
        }
    } else {
        if remove_file(binary_file_name).is_ok() {
            msg_list.warning("Removed old binary file");
        }
        msg_list.warning("Not writing new binary file due to assembly errors");
    }

    // A failed build is a failure.  (`-m` and `-T` need `-s`, so they were
    // handled with the send above.)
    outcome?;
    let error_count = msg_list.count(MessageType::Error);
    if error_count > 0 {
        return Err(Error::Assembly(error_count));
    }
    Ok(())
}
