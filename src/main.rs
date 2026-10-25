//! Top level file for Klausscc.

/// The assembler's shared line records and lexer predicates.
mod asm;
/// Module defining the command-line interface (clap).
mod cli;
/// Module of subcommand handlers (the `run_*` entry points).
mod commands;
/// Module: independent ISA emulator (golden-model trace generator).
mod emulate;
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
/// Module to manage macros.
mod macros;
/// Module to manage messages.
mod messages;
/// Module to stream a flat DDR image to the board over TCP (network boot).
mod netload;
/// Module for integer literal parsing.
mod num;
/// Module to manage opcodes.
mod opcodes;
/// Module to write to serial and read response.
mod serial;
use cli::set_matches;
use commands::{
    monitor_failed, run_elf2serial, run_emulate, run_emulate_elf, run_emulate_test, run_gdbserver, run_kbt_send, run_mem_out, run_netload,
    run_test_list, run_test_mode,
};
use error::Error;
use files::{filename_stem, output_file_name, write_binary_output_file, write_code_output_file};
use macros::expand_embedded_macros;
use messages::{print_messages, MessageType, MsgList};
use netload::NETBOOT_DEFAULT_PORT;
use opcodes::v2_opcodes_and_macros;
use serial::{monitor_serial, monitor_serial_port, write_to_board, write_to_board_keep_port, AUTO_SERIAL};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

/// Magic bytes at the start of every ELF file (`0x7F` `E` `L` `F`).
pub(crate) const ELF_MAGIC: &[u8] = b"\x7fELF";

/// Main function for Klausscc.
///
/// Runs the command and turns its outcome into the process exit status: `0` on
/// success, `2` when tests failed, `3` when a test timed out, `1` otherwise.
/// Failures are reported to the user where they happen, so nothing more is
/// printed here.
#[cfg(not(tarpaulin_include))] // Cannot test main in tarpaulin
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => err.exit_code(),
    }
}

/// Read the CLI and call the handler for the selected mode.
#[cfg(not(tarpaulin_include))] // Cannot test main in tarpaulin
fn run() -> Result<(), Error> {
    use std::fs::remove_file;

    use files::output_macros_opcodes_html;

    let mut msg_list = MsgList::new();
    /* Stream messages as they happen rather than dumping them all at the end:
     * during a board load the transfer is otherwise silent and every status
     * line (plus the board's boot log behind it) appears in one burst once the
     * monitor starts.  print_messages() becomes a no-op; print_results() still
     * prints the final summary line. */
    msg_list.live = true;
    let start_time = Instant::now();

    /* try_get_matches() rather than get_matches(): clap exits with status 2 on
     * a bad command line, which would read as "tests failed". */
    let matches = match set_matches().try_get_matches() {
        Ok(matches) => matches,
        Err(err) => {
            // --help and --version go to stdout and succeed; anything else is a usage error.
            let _ = err.print();
            return if err.use_stderr() {
                Err(Error::Usage("invalid command line"))
            } else {
                Ok(())
            };
        }
    };
    let input_file_name: String = matches.get_one::<String>("input").cloned().unwrap_or_default();
    let binary_file_name = output_file_name(matches.get_one::<String>("bitcode").map(String::as_str), &input_file_name, "kbt");
    let output_file_name = output_file_name(matches.get_one::<String>("output").map(String::as_str), &input_file_name, "code");
    let output_serial_port: String = matches.get_one::<String>("serial").cloned().unwrap_or_default();
    let opcodes_flag = matches.get_flag("opcodes");
    let textmate_flag = matches.get_flag("textmate");
    let monitor_flag = matches.get_flag("monitor");
    let test_flag = matches.get_flag("test");
    let no_break_flag = matches.get_flag("no_break");
    let debug_flag = matches.get_flag("debug");
    let test_timeout: u64 = matches
        .get_one::<String>("test_timeout")
        .and_then(|timeout_str| timeout_str.parse().ok())
        .unwrap_or(10);
    let test_list_file: String = matches.get_one::<String>("test_list").cloned().unwrap_or_default();
    let emulate_flag = matches.get_flag("emulate");
    let gdbserver_port: Option<u16> = matches.get_one::<u16>("gdbserver").copied();
    let board_flag = matches.get_flag("board");
    let jtag_khz: u32 = matches.get_one::<u32>("jtag_khz").copied().unwrap_or(10_000);
    let trace_file: Option<String> = matches.get_one::<String>("trace").cloned();
    let emulate_test_file: Option<String> = matches.get_one::<String>("emulate_test").cloned();
    let max_instructions: u64 = matches
        .get_one::<String>("max_instructions")
        .and_then(|s| s.parse().ok())
        .unwrap_or(emulate::DEFAULT_MAX_INSTRUCTIONS);

    // Classify the input file by extension (case-insensitive).  The file type is
    // determined from the name rather than from a mode-specific flag:
    //   .kla  → assemble (this is the only mode that needs an opcode file)
    //   .kbt  → pre-built board wire-format image, sent/monitored verbatim
    //   else  → an ELF or flat binary, converted to the wire format (elf2serial)
    let input_lower = input_file_name.to_ascii_lowercase();
    let is_kbt_input = input_lower.ends_with(".kbt");
    let is_binary_input = !input_file_name.is_empty() && !input_lower.ends_with(".kla") && !is_kbt_input;

    // Monitor-only mode: `-m` on its own (optionally with `-s`) just opens the
    // serial monitor — no input file, no assembly, no opcode file needed.  With
    // no `-s` the first USB serial port is auto-detected.
    if monitor_flag
        && input_file_name.is_empty()
        && matches.get_one::<String>("net_load").is_none()
        && matches.get_one::<String>("mem_out").is_none()
        && test_list_file.is_empty()
        && !opcodes_flag
        && !textmate_flag
    {
        let monitor_port = if output_serial_port.is_empty() {
            AUTO_SERIAL
        } else {
            output_serial_port.as_str()
        };
        let monitored = monitor_serial(monitor_port, debug_flag, &mut msg_list).map_err(|err| monitor_failed(monitor_port, err, &mut msg_list));
        print_messages(&msg_list);
        return monitored;
    }

    // net-load mode: flatten an ELF (or take a flat binary) and stream it to the
    // board over TCP — no kbt, no UART. See NETBOOT_PLAN.md / netboot.c.
    if let Some(net_binary_path) = matches.get_one::<String>("net_load") {
        let entry_addr: Option<u32> = matches.get_one::<u32>("entry_point").copied();
        let board_ip = matches.get_one::<String>("ip").cloned().unwrap_or_default();
        let board_port: u16 = matches
            .get_one::<String>("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(NETBOOT_DEFAULT_PORT);
        run_netload(net_binary_path, entry_addr, &board_ip, board_port, &mut msg_list, start_time)?;

        /* After a successful net-load, optionally monitor the board's UART (and
         * forward keystrokes), exactly like -s -m.  The load itself is over TCP,
         * so the monitor needs its own serial port: honour -s if given, otherwise
         * auto-detect the first USB serial port (same as -s with no value). */
        if monitor_flag {
            let monitor_port = if output_serial_port.is_empty() {
                AUTO_SERIAL
            } else {
                output_serial_port.as_str()
            };
            monitor_serial(monitor_port, debug_flag, &mut msg_list).map_err(|err| monitor_failed(monitor_port, err, &mut msg_list))?;
        }
        return Ok(());
    }

    // mem-out mode: flatten an ELF (or take a flat binary) and write a $readmemh
    // image for the resident boot ROM (boot_rom.v). See NETBOOT_PLAN.md Phase 2.
    if let Some(mem_binary_path) = matches.get_one::<String>("mem_out") {
        let mem_stem = filename_stem(mem_binary_path);
        let mem_file_name = matches
            .get_one::<String>("mem_file")
            .cloned()
            .unwrap_or_else(|| format!("{mem_stem}.mem"));
        return run_mem_out(mem_binary_path, &mem_file_name, &mut msg_list, start_time);
    }

    // ELF / flat binary input: convert directly to the board wire format and
    // optionally send it.  No opcode file is required — it is only used, if it
    // happens to exist, to emit a .code disassembly listing alongside the .kbt.
    if is_binary_input {
        let entry_addr: Option<u32> = matches.get_one::<u32>("entry_point").copied();
        if let Some(port) = gdbserver_port {
            if board_flag {
                return commands::run_gdbserver_board(
                    &input_file_name,
                    entry_addr,
                    port,
                    &output_serial_port,
                    &binary_file_name,
                    no_break_flag,
                    jtag_khz,
                    &mut msg_list,
                    start_time,
                );
            }
            return run_gdbserver(&input_file_name, entry_addr, port, &mut msg_list, start_time);
        }
        if emulate_flag {
            return run_emulate_elf(
                &input_file_name,
                entry_addr,
                trace_file.as_deref(),
                max_instructions,
                &mut msg_list,
                start_time,
            );
        }
        return run_elf2serial(
            &input_file_name,
            entry_addr,
            &output_serial_port,
            &binary_file_name,
            monitor_flag,
            debug_flag,
            no_break_flag,
            &mut msg_list,
            start_time,
        );
    }

    // .kbt input: already in board wire format — send and/or monitor verbatim.
    if is_kbt_input {
        return run_kbt_send(
            &input_file_name,
            &output_serial_port,
            monitor_flag,
            debug_flag,
            no_break_flag,
            &mut msg_list,
            start_time,
        );
    }

    // The opcode table is built in to the assembler (ISA encoding v2) — the old
    // external opcode_select.vh file is gone. Build the opcode + macro lists in code.
    let (oplist, macro_list_raw) = v2_opcodes_and_macros(&mut msg_list);
    let macro_list = expand_embedded_macros(macro_list_raw, &mut msg_list);

    let html_result = output_macros_opcodes_html(
        "klausscc_opcodes_v2".to_owned(),
        &oplist,
        macro_list.clone(),
        &mut msg_list,
        opcodes_flag,
        textmate_flag,
    );
    if let Err(result_err) = &html_result {
        msg_list.push(format!("Error {result_err} writing opcode list to HTML"), None, None, MessageType::Error);
    }

    if textmate_flag || opcodes_flag {
        print_messages(&msg_list);
        return html_result.map_err(|err| Error::Other(format!("writing opcode list: {err}")));
    }

    // Emulator batch-verify mode: assemble + emulate each .kla and check UART.
    if let Some(test_path) = emulate_test_file {
        return run_emulate_test(&oplist, &macro_list, &test_path, max_instructions, &mut msg_list, start_time);
    }

    // Batch test list mode
    if !test_list_file.is_empty() {
        return run_test_list(
            &oplist,
            &macro_list,
            &test_list_file,
            &output_serial_port,
            test_timeout,
            !no_break_flag,
            &mut msg_list,
        );
    }

    // Assemble the input file.  A build with errors still comes back, so the
    // .code listing below can show where they are.
    let Some(assembly) = asm::assemble(&input_file_name, &oplist, &macro_list, &mut msg_list) else {
        print_results(&msg_list, start_time);
        return Err(Error::Assembly(msg_list.number_by_type(MessageType::Error).max(1)));
    };

    // Emulator mode: build the flat DDR image from the assembled program and run
    // the golden-model. Additive — returns early, leaving normal modes untouched.
    if emulate_flag {
        return run_emulate(
            &assembly,
            &input_file_name,
            trace_file.as_deref(),
            max_instructions,
            &mut msg_list,
            start_time,
        );
    }

    // The .code listing is written even when assembly failed: it shows where.
    if let Err(result_err) = write_code_output_file(&output_file_name, &assembly.lines, &mut msg_list) {
        msg_list.push(
            format!("Unable to write to code file {output_file_name}, error {result_err}"),
            None,
            None,
            MessageType::Error,
        );
        print_results(&msg_list, start_time);
        return Err(Error::Write {
            path: PathBuf::from(output_file_name),
            source: result_err,
        });
    }

    // Failure from writing the .kbt or sending it; assembly errors are counted below.
    let mut outcome: Result<(), Error> = Ok(());
    if msg_list.number_by_type(MessageType::Error) == 0 {
        if let Some(bin_string) = assembly.to_kbt(&mut msg_list) {
            outcome = write_binary_file(&mut msg_list, &binary_file_name, &bin_string);
            if outcome.is_ok() && !output_serial_port.is_empty() {
                if test_flag {
                    // Test mode: send to board, keep port open, then verify UART output
                    return run_test_mode(
                        &mut msg_list,
                        &bin_string,
                        &output_serial_port,
                        &input_file_name,
                        test_timeout,
                        !no_break_flag,
                        start_time,
                    );
                }
                if monitor_flag {
                    // Monitor mode: send to board, keep port open, then monitor UART output
                    match write_to_board_keep_port(&bin_string, &output_serial_port, !no_break_flag, true, &mut msg_list) {
                        Ok(port) => {
                            msg_list.push("Wrote to serial port".to_owned(), None, None, MessageType::Information);
                            print_results(&msg_list, start_time);
                            let monitored = monitor_serial_port(port, debug_flag, &mut msg_list)
                                .map_err(|err| monitor_failed(&output_serial_port, err, &mut msg_list));
                            print_messages(&msg_list);
                            return monitored;
                        }
                        Err(err) => {
                            msg_list.push(format!("Failed to write to serial port, error \"{err}\""), None, None, MessageType::Error);
                            print_results(&msg_list, start_time);
                            return Err(Error::Serial {
                                port: output_serial_port,
                                source: err,
                            });
                        }
                    }
                }
                outcome = write_to_device(&mut msg_list, &bin_string, &output_serial_port, !no_break_flag);
            }
        } else {
            if remove_file(&binary_file_name).is_ok() {
                msg_list.push("Removed old binary file".to_owned(), None, None, MessageType::Warning);
            }
            msg_list.push(
                "Not writing binary file due to assembly errors".to_owned(),
                None,
                None,
                MessageType::Warning,
            );
        }
    } else {
        if remove_file(&binary_file_name).is_ok() {
            msg_list.push("Removed old binary file".to_owned(), None, None, MessageType::Warning);
        }
        msg_list.push(
            "Not writing new binary file due to assembly errors".to_owned(),
            None,
            None,
            MessageType::Warning,
        );
    }

    print_results(&msg_list, start_time);

    // A failed build is a failure: don't go on to monitor a stale program.
    outcome?;
    let error_count = msg_list.number_by_type(MessageType::Error);
    if error_count > 0 {
        return Err(Error::Assembly(error_count));
    }

    // Monitor serial port after assembly and send if requested
    if monitor_flag {
        if output_serial_port.is_empty() {
            msg_list.push("Monitor flag (-m) requires a serial port (-s)".to_owned(), None, None, MessageType::Error);
            print_messages(&msg_list);
            return Err(Error::Usage("monitor flag (-m) requires a serial port (-s)"));
        }
        let monitored =
            monitor_serial(&output_serial_port, debug_flag, &mut msg_list).map_err(|err| monitor_failed(&output_serial_port, err, &mut msg_list));
        print_messages(&msg_list);
        monitored?;
    }

    // Check test flag requires serial port
    if test_flag && output_serial_port.is_empty() {
        msg_list.push("Test flag (-T) requires a serial port (-s)".to_owned(), None, None, MessageType::Error);
        print_messages(&msg_list);
        return Err(Error::Usage("test flag (-T) requires a serial port (-s)"));
    }

    Ok(())
}

/// Prints results of assembly.
///
/// Takes the message list and start time and prints the results to the users.
#[cfg(not(tarpaulin_include))] // Cannot test printing in tarpaulin
pub fn print_results(msg_list: &MsgList, start_time: Instant) {
    print_messages(msg_list);
    let time_taken: f64 = start_time.elapsed().as_secs_f64();
    eprintln!(
        "Completed with {} error{} and {} warning{} in {:.3} seconds",
        msg_list.number_by_type(MessageType::Error),
        if msg_list.number_by_type(MessageType::Error) == 1 { "" } else { "s" },
        msg_list.number_by_type(MessageType::Warning),
        if msg_list.number_by_type(MessageType::Warning) == 1 { "" } else { "s" },
        time_taken,
    );
}

/// Writes the binary file.
///
/// Writes the `.kbt` wire-format string to `binary_file_name`, reporting (and
/// returning) a failure.
#[cfg(not(tarpaulin_include))] // Cannot test device write in tarpaulin
pub fn write_binary_file(msg_list: &mut MsgList, binary_file_name: &str, bin_string: &str) -> Result<(), Error> {
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

/// Send machine code to device.
///
/// Sends the resultant code on the serial device defined if no errors were found.
/// Fails if errors were already reported or the send fails.
#[cfg(not(tarpaulin_include))] // Cannot test device write in tarpaulin
pub fn write_to_device(msg_list: &mut MsgList, bin_string: &str, output_serial_port: &str, send_break: bool) -> Result<(), Error> {
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
    match write_to_board(bin_string, output_serial_port, send_break, msg_list) {
        Ok(()) => {
            msg_list.push("Wrote to serial port".to_owned(), None, None, MessageType::Information);
            Ok(())
        }
        Err(err) => {
            msg_list.push(format!("Failed to write to serial port, error \"{err}\""), None, None, MessageType::Error);
            Err(Error::Serial {
                port: output_serial_port.to_owned(),
                source: err,
            })
        }
    }
}
