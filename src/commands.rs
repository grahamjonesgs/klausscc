//! Subcommand handlers — each `run_*` function drives one CLI mode end to end
//! (net-load, mem-out, elf2serial, kbt send, emulate, and the test runners).

use crate::asm::{assemble, Assembly};
use crate::error::Error;
use crate::files::{filename_stem, write_binary_output_file, write_code_output_file};
use crate::helper::{disassemble_flat_to_pass2, human_bytes, parse_expected_uart_values};
use crate::image::{readmemh, Image};
use crate::layout::CODE_BASE;
use crate::loader::{load_flat, BinaryInput, Loaded};
use crate::messages::{print_messages, print_results, MessageType, MsgList};
use crate::netload::net_load;
use crate::opcodes::Opcode;
use crate::serial::{monitor_serial_port, run_test_monitor, write_to_board, write_to_board_keep_port, AUTO_SERIAL};
use crate::{emulate, gdbserver, macros};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Instant;

/// Result of a single test in a batch run.
struct BatchTestResult {
    /// File name of the test.
    file_name: String,
    /// Number of expected values that matched.
    passed: usize,
    /// Number of expected values that did not match.
    failed: usize,
    /// True if the test timed out.
    timed_out: bool,
    /// Total number of expected values.
    total: usize,
    /// Whether the test ran, and if not, why.
    outcome: Outcome,
}

/// Whether a test in a batch ran.
enum Outcome {
    /// The program ran on the board; the counts say how it went.
    Ran,
    /// Not run, for the given reason (no expected values, assembly or serial error).
    Skipped(String),
    /// Counted as a failure without counts: the file cannot be read, or the
    /// serial port failed while the test was reading UART output.
    Failed(String),
}

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

/// Flatten an ELF (or take a flat binary) and stream it to the board over TCP.
///
/// Detects ELF magic automatically: ELF LOAD segments are extracted and the
/// entry point read from the header (overridable with `--entry`); a flat binary
/// is used verbatim with entry defaulting to `0x20`.  The flat image is wrapped
/// in the heap-header DDR layout (`build_ddr_image`) and sent via `net_load`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_netload(input: BinaryInput<'_>, board_ip: &str, board_port: u16, msg_list: &mut MsgList, start_time: Instant) -> Result<(), Error> {
    if board_ip.is_empty() {
        msg_list.push("net-load requires --ip <board address>".to_owned(), None, None, MessageType::Error);
        print_results(msg_list, start_time);
        return Err(Error::Usage("net-load requires --ip <board address>"));
    }

    let loaded = load_flat(input, msg_list)?;
    msg_list.push(loaded.detected_message(true), None, None, MessageType::Information);

    if let Err(err) = net_load(board_ip, board_port, &loaded.image.to_ddr(), loaded.image.entry, msg_list) {
        msg_list.push(format!("netboot failed: \"{err}\""), None, None, MessageType::Error);
        print_results(msg_list, start_time);
        return Err(Error::Netboot(err));
    }

    print_results(msg_list, start_time);
    Ok(())
}

/// Flatten an ELF (or take a flat binary) and write a `$readmemh` boot-ROM image.
///
/// The image is the same `build_ddr_image` DDR layout used for net-load, emitted
/// as one 64-bit little-endian doubleword per line (16 hex chars) — matching
/// `boot_rom.v` (`DEPTH_DW` × 64-bit, `$readmemh`).  `boot_rom`'s copy FSM reads
/// word 0 (`heap_start` = image byte length) to know how much to copy to DDR.
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_mem_out(binary_path: &str, mem_file_name: &str, msg_list: &mut MsgList, start_time: Instant) -> Result<(), Error> {
    // mem-out writes a $readmemh image and does not need an entry point.
    let loaded = load_flat(
        BinaryInput {
            path: binary_path,
            entry: None,
        },
        msg_list,
    )?;
    msg_list.push(loaded.detected_message(false), None, None, MessageType::Information);

    let image = loaded.image.to_ddr();

    if let Err(e) = fs::write(mem_file_name, readmemh(&image)) {
        msg_list.push(
            format!("Failed to write boot ROM image {mem_file_name}: {e}"),
            None,
            None,
            MessageType::Error,
        );
        print_results(msg_list, start_time);
        return Err(Error::Write {
            path: PathBuf::from(mem_file_name),
            source: e,
        });
    }
    msg_list.push(
        format!(
            "mem-out: {binary_path} → {mem_file_name} ({} doublewords, {} bytes)",
            image.len() / 8,
            image.len()
        ),
        None,
        None,
        MessageType::Information,
    );
    print_results(msg_list, start_time);
    Ok(())
}

/// Convert an LLVM ELF or flat binary to the board wire format and optionally send it.
///
/// Detects ELF magic automatically:
/// - **ELF file**: LOAD segments are extracted; entry point is read from the ELF
///   header (overridden by `--entry` if given).
/// - **Flat binary**: bytes are used verbatim; entry point comes from `--entry`
///   (defaults to `0x20` if not given).
///
/// The resulting wire string is written to a `.kbt` file or, when `send` names a
/// serial port, sent to the board exactly like the normal assembly path.
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_elf2serial(
    input: BinaryInput<'_>,
    kbt_file_name: &str,
    send: SendOpts<'_>,
    msg_list: &mut MsgList,
    start_time: Instant,
) -> Result<(), Error> {
    let binary_path = input.path;
    // load_flat converts the ELF entry point to a board byte address: the
    // flat buffer starts at ELF VMA `elf_base` but is loaded by the board
    // immediately after the heap header, so board_entry = (elf_entry - elf_base)
    // + HEAP_HEADER_WORDS * 8.
    let loaded = load_flat(input, msg_list)?;
    msg_list.push(loaded.detected_message(true), None, None, MessageType::Information);

    // Pad to whole 32-bit words: the board loads words, and the .code
    // listing below shows the last part word as a whole one.
    let mut image = loaded.image;
    image.pad_to_word();
    let entry_addr = image.entry;
    let heap_start = image.heap_start();

    // The image was padded to whole words above, so this only fails on a bug,
    // but an image the board would reject is never written or sent.
    let Some(out) = image.to_kbt(msg_list) else {
        print_results(msg_list, start_time);
        return Err(Error::Assembly(msg_list.number_by_type(MessageType::Error)));
    };

    msg_list.push(
        format!("elf2serial: {binary_path} → {kbt_file_name} (entry 0x{entry_addr:08X}, heap_start 0x{heap_start:08X})"),
        None,
        None,
        MessageType::Information,
    );

    /* Skip the on-disk .kbt / .code when streaming to the board over serial:
     * the image is sent straight from memory (`out`), so writing these (large)
     * files just adds several seconds for no benefit. */
    let mut outcome: Result<(), Error> = Ok(());
    if send.port.is_empty() {
        outcome = write_binary_file(msg_list, kbt_file_name, &out);
    }

    // Optionally produce a .code disassembly listing alongside the .kbt, using
    // the built-in ISA-v2 opcode table.  (Skipped for a serial load — see above.)
    if send.port.is_empty() {
        let opcodes = crate::opcodes::v2_opcodes();
        let code_file_name = {
            let stem = kbt_file_name.strip_suffix(".kbt").unwrap_or(kbt_file_name);
            format!("{stem}.code")
        };
        let pass2 = disassemble_flat_to_pass2(&image.bytes, CODE_BASE, &opcodes);
        if let Err(e) = write_code_output_file(&code_file_name, &pass2, msg_list) {
            msg_list.push(
                format!("Failed to write disassembly file {code_file_name}: {e}"),
                None,
                None,
                MessageType::Warning,
            );
        }
    }

    // A bad image (e.g. a checksum error) is never sent to the board.
    let error_count = msg_list.number_by_type(MessageType::Error);
    if error_count > 0 {
        print_results(msg_list, start_time);
        return outcome.and(Err(Error::Assembly(error_count)));
    }

    if !send.port.is_empty() {
        outcome = send_image(&out, send, msg_list, start_time);
        if send.monitor {
            return outcome;
        }
    }

    print_results(msg_list, start_time);
    outcome
}

/// Send a pre-built `.kbt` board wire-format image to the board and/or monitor it.
///
/// A `.kbt` file already holds the complete wire-format string (`S…Z…X`) produced
/// by an earlier assembly or elf2serial run, so it is sent verbatim — no opcode
/// file and no re-encoding.  With `-m` and no `-s`, the serial port is
/// auto-detected so a bare `-m` can both send and monitor.
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_kbt_send(kbt_path: &str, send: SendOpts<'_>, msg_list: &mut MsgList, start_time: Instant) -> Result<(), Error> {
    let wire = match fs::read_to_string(kbt_path) {
        Ok(contents) => contents,
        Err(e) => {
            msg_list.push(format!("Cannot read kbt file {kbt_path}: {e}"), None, None, MessageType::Error);
            print_results(msg_list, start_time);
            return Err(Error::Read {
                path: PathBuf::from(kbt_path),
                source: e,
            });
        }
    };
    let wire_trimmed = wire.trim();

    // A bare `-m` with no `-s` should still send: auto-detect the serial port.
    let send = if send.port.is_empty() && send.monitor {
        SendOpts { port: AUTO_SERIAL, ..send }
    } else {
        send
    };

    if send.port.is_empty() {
        msg_list.push(
            "Nothing to do: a .kbt input needs a serial port (-s) to send or -m to monitor".to_owned(),
            None,
            None,
            MessageType::Warning,
        );
        print_results(msg_list, start_time);
        return Ok(());
    }

    msg_list.push(
        format!("Sending pre-built image {kbt_path} to board"),
        None,
        None,
        MessageType::Information,
    );

    let outcome = send_image(wire_trimmed, send, msg_list, start_time);
    if send.monitor {
        return outcome;
    }
    print_results(msg_list, start_time);
    outcome
}

/// Run test verification mode.
///
/// Sends program to board, reads UART output, and verifies against expected values
/// parsed from the source file comments.
#[cfg(not(tarpaulin_include))] // Cannot test serial hardware in tarpaulin
pub fn run_test_mode(
    msg_list: &mut MsgList,
    bin_string: &str,
    output_serial_port: &str,
    input_file_name: &str,
    test_timeout: u64,
    send_break: bool,
    start_time: Instant,
) -> Result<(), Error> {
    // Parse expected values from the raw source file
    let raw_lines: Vec<String> = match fs::read_to_string(input_file_name) {
        Ok(source) => source.lines().map(String::from).collect(),
        Err(e) => {
            msg_list.push(format!("Cannot read {input_file_name}: {e}"), None, None, MessageType::Error);
            print_results(msg_list, start_time);
            return Err(Error::Read {
                path: PathBuf::from(input_file_name),
                source: e,
            });
        }
    };
    let expected_values = parse_expected_uart_values(&raw_lines);

    if expected_values.is_empty() {
        msg_list.push(
            "No expected UART values found in source file comments".to_owned(),
            None,
            None,
            MessageType::Error,
        );
        print_results(msg_list, start_time);
        return Err(Error::NoTests(PathBuf::from(input_file_name)));
    }

    let expected_count = expected_values.len();
    msg_list.push(
        format!("Found {expected_count} expected UART values in source comments"),
        None,
        None,
        MessageType::Information,
    );

    // Send to board and keep port open
    let port = match write_to_board_keep_port(bin_string, output_serial_port, send_break, false, msg_list) {
        Ok(port) => {
            msg_list.push("Wrote to serial port".to_owned(), None, None, MessageType::Information);
            port
        }
        Err(err) => return Err(send_failed(output_serial_port, err, msg_list, start_time)),
    };

    print_results(msg_list, start_time);

    // Run test verification.  A serial read error is a failure of its own
    // (exit 1), not a timeout (exit 3); run_test_monitor has reported it.
    let result = run_test_monitor(port, &expected_values, test_timeout, msg_list).map_err(|err| Error::SerialRead {
        port: output_serial_port.to_owned(),
        source: err,
    })?;

    // Print summary
    println!(
        "\nTest result: {}/{} passed, {}/{} failed{}",
        result.passed,
        result.total,
        result.failed,
        result.total,
        if result.timed_out { " (TIMED OUT)" } else { "" },
    );

    if result.failed > 0 || result.timed_out {
        Err(Error::TestsFailed {
            failed: result.failed,
            total: result.total,
            timed_out: result.timed_out,
        })
    } else {
        Ok(())
    }
}

/// Open the destination for the emulator's per-instruction trace.
///
/// Follows the `--trace` convention: `None` → no trace, `Some("-")` → stdout,
/// `Some(path)` → that file.  Returns `Ok(None)` when no trace was requested,
/// `Ok(Some(writer))` otherwise, or an error if the file cannot be created (the
/// error is already reported to `msg_list`).  The writer is line-buffered so the
/// trace streams during the run instead of being held in memory.
#[cfg(not(tarpaulin_include))]
fn open_trace_writer(trace_file: Option<&str>, msg_list: &mut MsgList) -> Result<Option<Box<dyn std::io::Write>>, Error> {
    use std::io::BufWriter;
    match trace_file {
        None => Ok(None),
        Some("-") => Ok(Some(Box::new(BufWriter::new(std::io::stdout())))),
        Some(path) => match std::fs::File::create(path) {
            Ok(f) => Ok(Some(Box::new(BufWriter::new(f)))),
            Err(e) => {
                msg_list.push(format!("Failed to create trace file {path}: {e}"), None, None, MessageType::Error);
                Err(Error::Write {
                    path: PathBuf::from(path),
                    source: e,
                })
            }
        },
    }
}

/// Finish a trace writer: flush it and, for a file destination, report the count.
#[cfg(not(tarpaulin_include))]
fn finish_trace_writer(mut writer: Option<Box<dyn std::io::Write>>, trace_file: Option<&str>, instructions: u64, msg_list: &mut MsgList) {
    use std::io::Write as _;
    if let Some(w) = writer.as_mut() {
        let _ = w.flush();
    }
    if let Some(path) = trace_file {
        if path != "-" {
            msg_list.push(
                format!("Wrote {instructions} instruction trace lines to {path}"),
                None,
                None,
                MessageType::Information,
            );
        }
    }
}

/// Run `image` on the emulator and report the run (`--emulate`).
///
/// Streams the per-instruction trace to `trace_file` (or, for `"-"`, stdout),
/// prints the summary, then the instruction count, the stop reason and the
/// captured UART output on stdout.
#[cfg(not(tarpaulin_include))]
fn emulate_and_report(
    image: &Image,
    trace_file: Option<&str>,
    max_instructions: u64,
    msg_list: &mut MsgList,
    start_time: Instant,
) -> Result<(), Error> {
    let mut trace_writer = open_trace_writer(trace_file, msg_list).inspect_err(|_| {
        print_results(msg_list, start_time);
    })?;
    let result = emulate::emulate_image_to_writer(
        &image.to_ddr(),
        image.entry,
        max_instructions,
        trace_writer.as_mut().map(|w| w.as_mut() as &mut dyn std::io::Write),
    );
    finish_trace_writer(trace_writer, trace_file, result.instructions, msg_list);

    print_results(msg_list, start_time);
    println!(
        "--- Emulator finished: {} instructions, stop = {:?} ---",
        result.instructions, result.stop
    );
    println!("--- Captured UART output ---");
    {
        use std::io::Write as _;
        let mut out = std::io::stdout();
        let _ = out.write_all(&emulate::uart_bytes(&result.uart));
        let _ = out.flush();
    }
    println!("--- end UART ---");
    Ok(())
}

/// Run the emulator on a single assembled program (`--emulate`).
///
/// Lays the assembled program out in memory and runs it with
/// [`emulate_and_report`].  A build with errors, or without `_start`, is not run.
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_emulate(
    assembly: &Assembly,
    input_file_name: &str,
    trace_file: Option<&str>,
    max_instructions: u64,
    msg_list: &mut MsgList,
    start_time: Instant,
) -> Result<(), Error> {
    if msg_list.number_by_type(MessageType::Error) > 0 {
        msg_list.push("Not emulating due to assembly errors".to_owned(), None, None, MessageType::Error);
        print_results(msg_list, start_time);
        return Err(Error::Assembly(msg_list.number_by_type(MessageType::Error)));
    }
    let Some(image) = assembly.image() else {
        msg_list.push("No _start address found - cannot emulate".to_owned(), None, None, MessageType::Error);
        print_results(msg_list, start_time);
        return Err(Error::Assembly(1));
    };
    msg_list.push(
        format!(
            "Emulating {input_file_name}: {} code bytes, entry 0x{:08X}",
            image.bytes.len(),
            image.entry
        ),
        None,
        None,
        MessageType::Information,
    );
    emulate_and_report(&image, trace_file, max_instructions, msg_list, start_time)
}

/// Run the emulator on an ELF or flat binary input (`--emulate` with binary input).
///
/// Mirrors `run_elf2serial`'s ELF → flat → board-address conversion, then builds
/// the DDR image and runs the golden model instead of sending it to the board, so
/// prebuilt C ELFs (queens, `test_64bit`, …) can be cross-checked against the RTL.
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_emulate_elf(
    input: BinaryInput<'_>,
    trace_file: Option<&str>,
    max_instructions: u64,
    msg_list: &mut MsgList,
    start_time: Instant,
) -> Result<(), Error> {
    let binary_path = input.path;
    let Loaded { image, elf } = load_flat(input, msg_list)?;
    if let Some(elf) = elf {
        msg_list.push(
            format!(
                "Emulating ELF {binary_path}: {}, ELF base 0x{:08X}, board entry 0x{:08X}",
                human_bytes(image.bytes.len()),
                elf.base,
                image.entry
            ),
            None,
            None,
            MessageType::Information,
        );
    } else {
        msg_list.push(format!("Emulating flat binary {binary_path}"), None, None, MessageType::Information);
    }

    emulate_and_report(&image, trace_file, max_instructions, msg_list, start_time)
}

/// Debug an ELF (or flat binary) on the emulator over the GDB remote protocol.
///
/// The image is laid out exactly as `--emulate` does (heap header, then the
/// program at 0x20), so ELF addresses equal emulator addresses when the ELF is
/// linked at 0x20 — the normal `klausscpu.ld` layout.
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_gdbserver(input: BinaryInput<'_>, port: u16, msg_list: &mut MsgList, start_time: Instant) -> Result<(), Error> {
    let binary_path = input.path;
    let Loaded { image, elf } = load_flat(input, msg_list)?;
    let code_base = u64::from(CODE_BASE);
    if let Some(elf) = &elf {
        if elf.base != code_base {
            msg_list.push(
                format!(
                    "ELF {binary_path} is linked at 0x{:08X}, not 0x{code_base:08X}: debugger addresses will not match the emulator's",
                    elf.base
                ),
                None,
                None,
                MessageType::Warning,
            );
        }
    }
    print_results(msg_list, start_time);

    let cpu = emulate::Cpu::new(&image.to_ddr(), image.entry);
    match gdbserver::serve(cpu, port) {
        Ok(summary) => {
            eprintln!("klausscc gdbserver: {summary}");
            Ok(())
        }
        Err(e) => {
            eprintln!("klausscc gdbserver: {e}");
            Err(Error::Other(e))
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
    msg_list: &mut MsgList,
    start_time: Instant,
) -> Result<(), Error> {
    use crate::gdbserver_board;

    let fail = |e: String| {
        eprintln!("klausscc gdbserver: {e}");
        Error::Other(e)
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
            run_elf2serial(input, kbt_file_name, send, msg_list, start_time)
                .map_err(|err| format!("loading {} over {} failed ({err})", input.path, send.port))
        };
        gdbserver_board::attach(&mut board, Some(&mut load)).map_err(fail)?
    };
    eprintln!("klausscc gdbserver: core 1 halted at 0x{pc:08X}");

    match gdbserver_board::serve(board, port) {
        Ok(summary) => {
            eprintln!("klausscc gdbserver: {summary}");
            Ok(())
        }
        Err(e) => Err(fail(e)),
    }
}

/// `--board` without the `board` feature: explain how to get it.
#[cfg(all(not(feature = "board"), not(tarpaulin_include)))]
pub(crate) fn board_unsupported(msg_list: &mut MsgList) -> Error {
    msg_list.push(
        "--board needs a klausscc built with board support: cargo build --release --features board (needs libftdi1-dev)".to_owned(),
        None,
        None,
        MessageType::Error,
    );
    Error::Usage("--board needs a klausscc built with board support")
}

/// Batch-verify the emulator against `.kla` files with expected `// ` UART values.
///
/// `test_path` may be a single `.kla` file, a directory (all `*.kla` inside),
/// or a test-list file (one path per line).  For each file with expected
/// values, assemble + emulate and compare captured UART tokens in order.
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_emulate_test(
    oplist: &[Opcode],
    macro_list: &[macros::Macro],
    test_path: &str,
    max_instructions: u64,
    msg_list: &mut MsgList,
    start_time: Instant,
) -> Result<(), Error> {
    use std::path::Path;

    // Resolve the list of .kla files to test.
    let path = Path::new(test_path);
    let mut files: Vec<String> = Vec::new();
    if path.is_dir() {
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().is_some_and(|e| e == "kla") {
                    files.push(p.to_string_lossy().into_owned());
                }
            }
        }
        files.sort();
    } else if test_path.ends_with(".kla") {
        files.push(test_path.to_owned());
    } else {
        // treat as a test-list file
        files = read_test_list(test_path, msg_list);
    }

    if files.is_empty() {
        msg_list.push(
            format!("No .kla files found for emulate-test at {test_path}"),
            None,
            None,
            MessageType::Error,
        );
        print_results(msg_list, start_time);
        return Err(Error::NoTests(PathBuf::from(test_path)));
    }

    println!("Emulator verification over {} file(s):\n", files.len());
    let mut total_pass = 0_usize;
    let mut total_files = 0_usize;
    let mut failed_files: Vec<String> = Vec::new();

    for file in &files {
        match verify_emulated(file, oplist, macro_list, max_instructions) {
            Ok(Verdict::NotValidatable) => {} // skip non-validatable files silently
            Ok(Verdict::Pass { matched, instructions }) => {
                total_files += 1;
                total_pass += 1;
                println!("  PASS {}: {matched}/{matched} ({instructions} instrs)", filename_stem(file));
            }
            Ok(Verdict::Fail { matched, expected, diff }) => {
                total_files += 1;
                let stem = filename_stem(file);
                println!("  FAIL {stem}: {matched}/{expected} — {diff}");
                failed_files.push(format!("{stem}: {diff}"));
            }
            Err(reason) => {
                total_files += 1;
                println!("  FAIL {file}: {reason}");
                failed_files.push(format!("{file} ({reason})"));
            }
        }
    }

    println!("\nEmulator verification: {total_pass}/{total_files} files passed");
    if !failed_files.is_empty() {
        println!("Failures:");
        for f in &failed_files {
            println!("  {f}");
        }
    }
    if total_files == 0 {
        msg_list.push(
            format!("No files with expected UART values found for emulate-test at {test_path}"),
            None,
            None,
            MessageType::Error,
        );
        print_results(msg_list, start_time);
        return Err(Error::NoTests(PathBuf::from(test_path)));
    }
    print_results(msg_list, start_time);
    if total_pass == total_files {
        Ok(())
    } else {
        Err(Error::TestsFailed {
            failed: total_files - total_pass,
            total: total_files,
            timed_out: false,
        })
    }
}

/// Outcome of checking one `.kla` file against the emulator.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The file has no expected UART values, so there is nothing to check.
    NotValidatable,
    /// Every expected value was printed, in order.
    Pass {
        /// Number of expected values (all of them matched).
        matched: usize,
        /// Instructions the emulator executed.
        instructions: u64,
    },
    /// The UART output diverged from the expected values.
    Fail {
        /// Number of expected values that matched before the first difference.
        matched: usize,
        /// Number of expected values.
        expected: usize,
        /// Description of the first difference.
        diff: String,
    },
}

/// Assemble `file`, run it on the emulator and compare its UART output with the
/// expected values in its `// ` comments.
///
/// Each UART line contributes its leading 8 upper-case hex digits (mirroring the
/// serial test runner), and the values are compared in order.  Returns `Err`
/// with a short reason when the file cannot be read or does not assemble.
pub(crate) fn verify_emulated(file: &str, oplist: &[Opcode], macro_list: &[macros::Macro], max_instructions: u64) -> Result<Verdict, String> {
    // A file that cannot be read is a failure, not a silently skipped file.
    let source = fs::read_to_string(file).map_err(|e| format!("cannot read: {e}"))?;
    let raw_lines: Vec<String> = source.lines().map(String::from).collect();
    let expected = parse_expected_uart_values(&raw_lines);
    if expected.is_empty() {
        return Ok(Verdict::NotValidatable);
    }

    let mut test_msgs = MsgList::new();
    let assembly = assemble(file, oplist, macro_list, &mut test_msgs);
    let Some(image) = assembly.filter(|_| !test_msgs.has_errors()).and_then(|assembly| assembly.image()) else {
        return Err("assembly error".to_owned());
    };

    let (result, _) = emulate::emulate_image(&image.to_ddr(), image.entry, max_instructions, false);
    let got: Vec<&str> = result
        .uart
        .lines()
        .filter_map(|l| {
            let c = l.trim().get(..8)?;
            (c.chars().all(|ch| ch.is_ascii_hexdigit()) && c == c.to_ascii_uppercase()).then_some(c)
        })
        .collect();

    // Compare in order; report the first difference.
    for (idx, exp) in expected.iter().enumerate() {
        let diff = match got.get(idx) {
            Some(g) if g == exp => continue,
            Some(g) => format!("at #{}: expected {exp}, got {g}", idx + 1),
            None => format!("at #{}: expected {exp}, got <none> (stop={:?})", idx + 1, result.stop),
        };
        return Ok(Verdict::Fail {
            matched: idx,
            expected: expected.len(),
            diff,
        });
    }
    Ok(Verdict::Pass {
        matched: expected.len(),
        instructions: result.instructions,
    })
}

/// Assemble one file of a `-L` batch for the board.
///
/// Writes its `.code` listing next to it (also for a build with errors), and
/// returns the `.kbt` wire string, or `None` when the file cannot be read, the
/// listing cannot be written or the build has errors.
#[cfg(not(tarpaulin_include))]
fn assemble_to_kbt(input_file_name: &str, oplist: &[Opcode], macro_list: &[macros::Macro], msg_list: &mut MsgList) -> Option<String> {
    let assembly = assemble(input_file_name, oplist, macro_list, msg_list)?;

    let output_file_name = format!("{}.code", filename_stem(input_file_name));
    if let Err(result_err) = write_code_output_file(&output_file_name, &assembly.lines, msg_list) {
        msg_list.push(
            format!("Unable to write to code file {output_file_name}, error {result_err}"),
            None,
            None,
            MessageType::Error,
        );
        return None;
    }

    if msg_list.has_errors() {
        return None;
    }
    assembly.to_kbt(msg_list)
}

/// Read a test list file and return the list of test file paths.
///
/// Each line is a test file path. Blank lines and lines starting with `//` or `#` are ignored.
/// Paths are resolved relative to the directory containing the list file.
#[cfg(not(tarpaulin_include))]
fn read_test_list(list_file: &str, msg_list: &mut MsgList) -> Vec<String> {
    use std::path::Path;
    let list_dir = Path::new(list_file).parent().unwrap_or_else(|| Path::new("."));

    let contents = match fs::read_to_string(list_file) {
        Ok(c) => c,
        Err(err) => {
            msg_list.push(
                format!("Error reading test list file {list_file}: \"{err}\""),
                None,
                None,
                MessageType::Error,
            );
            return Vec::new();
        }
    };

    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//") && !line.starts_with('#'))
        .map(|line| {
            let path = Path::new(line);
            if path.is_absolute() {
                line.to_owned()
            } else {
                list_dir.join(line).to_string_lossy().into_owned()
            }
        })
        .collect()
}

/// Run a batch of tests from a test list file.
///
/// For each test file: assembles, sends to board, verifies UART output,
/// and prints per-test and aggregate results.
#[cfg(not(tarpaulin_include))]
pub fn run_test_list(
    oplist: &[Opcode],
    macro_list: &[macros::Macro],
    list_file: &str,
    output_serial_port: &str,
    test_timeout: u64,
    send_break: bool,
    msg_list: &mut MsgList,
) -> Result<(), Error> {
    if output_serial_port.is_empty() {
        msg_list.push("Test list (-L) requires a serial port (-s)".to_owned(), None, None, MessageType::Error);
        print_messages(msg_list);
        return Err(Error::Usage("test list (-L) requires a serial port (-s)"));
    }

    let test_files = read_test_list(list_file, msg_list);
    if test_files.is_empty() {
        msg_list.push("No test files found in test list".to_owned(), None, None, MessageType::Error);
        print_messages(msg_list);
        return Err(Error::NoTests(PathBuf::from(list_file)));
    }

    println!("Running {} tests from {list_file}...\n", test_files.len());

    let mut results: Vec<BatchTestResult> = Vec::new();

    for (index, test_file) in test_files.iter().enumerate() {
        println!("--- [{}/{}] {} ---", index + 1, test_files.len(), test_file);

        // Fresh message list for each test to avoid error accumulation
        let mut test_msg_list = MsgList::new();

        // A listed file that cannot be read is a failure, not a skip.
        let source = match fs::read_to_string(test_file) {
            Ok(source) => source,
            Err(err) => {
                println!("  FAIL: cannot read: {err}");
                results.push(BatchTestResult {
                    file_name: test_file.clone(),
                    passed: 0,
                    failed: 0,
                    timed_out: false,
                    total: 0,
                    outcome: Outcome::Failed(format!("cannot read: {err}")),
                });
                println!();
                continue;
            }
        };

        // Assemble the test file
        let Some(bin_string) = assemble_to_kbt(test_file, oplist, macro_list, &mut test_msg_list) else {
            println!("  SKIP: assembly failed");
            print_messages(&test_msg_list);
            results.push(BatchTestResult {
                file_name: test_file.clone(),
                passed: 0,
                failed: 0,
                timed_out: false,
                total: 0,
                outcome: Outcome::Skipped("assembly error".to_owned()),
            });
            println!();
            continue;
        };

        // Write binary file.  It is only a by-product here (the image is sent
        // from memory), so a failed write is shown but does not fail the test.
        let binary_file_name = format!("{}.kbt", filename_stem(test_file));
        if let Err(err) = write_binary_file(&mut test_msg_list, &binary_file_name, &bin_string) {
            println!("  WARNING: {err}");
        }

        // Parse expected values
        let raw_lines: Vec<String> = source.lines().map(String::from).collect();
        let expected_values = parse_expected_uart_values(&raw_lines);

        if expected_values.is_empty() {
            println!("  SKIP: no expected UART values in source comments");
            results.push(BatchTestResult {
                file_name: test_file.clone(),
                passed: 0,
                failed: 0,
                timed_out: false,
                total: 0,
                outcome: Outcome::Skipped("no expected values".to_owned()),
            });
            println!();
            continue;
        }

        // Send to board and keep port open
        let port = match write_to_board_keep_port(&bin_string, output_serial_port, send_break, false, &mut test_msg_list) {
            Ok(port) => port,
            Err(err) => {
                println!("  SKIP: serial port error \"{err}\"");
                results.push(BatchTestResult {
                    file_name: test_file.clone(),
                    passed: 0,
                    failed: 0,
                    timed_out: false,
                    total: 0,
                    outcome: Outcome::Skipped(format!("serial error: {err}")),
                });
                println!();
                continue;
            }
        };

        // Run test verification.  A serial read error fails the test (it is not a timeout).
        let result = match run_test_monitor(port, &expected_values, test_timeout, &mut test_msg_list) {
            Ok(result) => result,
            Err(err) => {
                println!("  FAIL: serial read error \"{err}\"");
                results.push(BatchTestResult {
                    file_name: test_file.clone(),
                    passed: 0,
                    failed: 0,
                    timed_out: false,
                    total: expected_values.len(),
                    outcome: Outcome::Failed(format!("serial read error: {err}")),
                });
                println!();
                continue;
            }
        };

        println!(
            "  Result: {}/{} passed{}",
            result.passed,
            result.total,
            if result.timed_out { " (TIMED OUT)" } else { "" },
        );

        results.push(BatchTestResult {
            file_name: test_file.clone(),
            passed: result.passed,
            failed: result.failed,
            timed_out: result.timed_out,
            total: result.total,
            outcome: Outcome::Ran,
        });

        println!();
    }

    report_batch(&results, list_file)
}

/// Print the summary of a `-L` batch run and decide its outcome.
///
/// Any failed, timed-out or unreadable test, or one whose serial read failed,
/// fails the batch (exit 2, timeouts included); a batch in which no test was
/// validated fails too (exit 1).
fn report_batch(results: &[BatchTestResult], list_file: &str) -> Result<(), Error> {
    println!("=== Test Suite Summary ===");
    let mut total_failed: usize = 0;
    let mut total_skipped: usize = 0;
    let mut total_timed_out: usize = 0;
    let mut files_all_pass: usize = 0;

    for r in results {
        if let Outcome::Skipped(reason) = &r.outcome {
            total_skipped += 1;
            println!("  SKIP  {} ({reason})", r.file_name);
        } else if let Outcome::Failed(reason) = &r.outcome {
            total_failed += 1;
            println!("  FAIL  {} ({reason})", r.file_name);
        } else if r.failed > 0 || r.timed_out {
            total_failed += 1;
            if r.timed_out {
                total_timed_out += 1;
            }
            println!(
                "  FAIL  {} ({}/{} passed{})",
                r.file_name,
                r.passed,
                r.total,
                if r.timed_out { ", timed out" } else { "" }
            );
        } else {
            files_all_pass += 1;
            println!("  PASS  {} ({}/{})", r.file_name, r.passed, r.total);
        }
    }

    let total_files = results.len();
    println!(
        "\n{files_all_pass}/{total_files} test files passed, {total_failed} failed, {total_skipped} skipped{}",
        if total_timed_out > 0 {
            format!(", {total_timed_out} timed out")
        } else {
            String::new()
        },
    );

    // A batch exits 2 for any failure, timeouts included (the summary lists them).
    if total_failed > 0 || total_timed_out > 0 {
        Err(Error::TestsFailed {
            failed: total_failed,
            total: total_files,
            timed_out: false,
        })
    } else if files_all_pass == 0 {
        println!("No test was validated");
        Err(Error::NoTests(PathBuf::from(list_file)))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A batch entry with the given counts and outcome.
    fn batch_result(passed: usize, failed: usize, timed_out: bool, outcome: Outcome) -> BatchTestResult {
        BatchTestResult {
            file_name: "t.kla".to_owned(),
            passed,
            failed,
            timed_out,
            total: passed + failed,
            outcome,
        }
    }

    // ---- report_batch --------------------------------------------------------

    #[test]
    fn batch_with_all_tests_passing_succeeds() {
        let results = [
            batch_result(3, 0, false, Outcome::Ran),
            batch_result(0, 0, false, Outcome::Skipped("x".to_owned())),
        ];
        assert!(report_batch(&results, "list.txt").is_ok());
    }

    #[test]
    fn batch_with_a_failed_or_timed_out_test_exits_2() {
        for failing in [batch_result(1, 1, false, Outcome::Ran), batch_result(1, 0, true, Outcome::Ran)] {
            let results = [batch_result(3, 0, false, Outcome::Ran), failing];
            let err = report_batch(&results, "list.txt").unwrap_err();
            assert!(matches!(err, Error::TestsFailed { failed: 1, total: 2, .. }), "{err:?}");
            assert_eq!(err.code(), 2);
        }
    }

    #[test]
    fn batch_with_an_unreadable_file_fails() {
        let results = [
            batch_result(3, 0, false, Outcome::Ran),
            batch_result(0, 0, false, Outcome::Failed("cannot read".to_owned())),
        ];
        let err = report_batch(&results, "list.txt").unwrap_err();
        assert_eq!(err.code(), 2);
    }

    #[test]
    fn batch_with_nothing_validated_fails() {
        let results = [batch_result(0, 0, false, Outcome::Skipped("no expected values".to_owned()))];
        let err = report_batch(&results, "list.txt").unwrap_err();
        assert!(matches!(err, Error::NoTests(_)), "{err:?}");
        assert_eq!(err.code(), 1);
    }

    // ---- verify_emulated -----------------------------------------------------

    /// Absolute path of the emulator fixture corpus.
    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    /// The built-in v2 opcode table and expanded macros, as the CLI loads them.
    fn v2_tables() -> (Vec<Opcode>, Vec<macros::Macro>) {
        let mut msg_list = MsgList::new();
        let (oplist, macros_raw) = crate::opcodes::v2_opcodes_and_macros(&mut msg_list);
        let macro_list = macros::expand_embedded_macros(macros_raw, &mut msg_list);
        (oplist, macro_list)
    }

    /// The corpus `--emulate-test tests/fixtures` runs: every file with expected
    /// UART values must assemble and print exactly those values on the emulator.
    #[test]
    fn emulator_corpus_passes() {
        let (oplist, macro_list) = v2_tables();
        let mut files: Vec<PathBuf> = fs::read_dir(fixture_dir())
            .expect("read fixture dir")
            .flatten()
            .map(|entry| entry.path())
            .filter(|p| p.extension().is_some_and(|e| e == "kla"))
            .collect();
        files.sort();

        let mut passed: Vec<String> = Vec::new();
        let mut failures: Vec<String> = Vec::new();
        for path in &files {
            let file = path.to_string_lossy();
            match verify_emulated(&file, &oplist, &macro_list, emulate::DEFAULT_MAX_INSTRUCTIONS) {
                Ok(Verdict::NotValidatable) => {}
                Ok(Verdict::Pass { .. }) => passed.push(file.into_owned()),
                other => failures.push(format!("{file}: {other:?}")),
            }
        }
        assert!(failures.is_empty(), "emulator corpus failures: {failures:#?}");
        // Guards against the corpus silently shrinking (e.g. a file that stops
        // declaring its expected values).
        assert!(passed.len() >= 22, "only {} validated files passed: {passed:#?}", passed.len());
    }

    #[test]
    fn verify_reports_the_first_wrong_value() {
        // test_regs with its first expected value changed: the program still
        // prints 000000FF, so the check must fail at #1.
        let dir = tempfile::tempdir().expect("tempdir");
        fs::copy(fixture_dir().join("uart_stubs.kla"), dir.path().join("uart_stubs.kla")).expect("copy stubs");
        let source = fs::read_to_string(fixture_dir().join("test_regs.kla")).expect("read test_regs");
        let source = source.replacen("//   000000FF  (SETR A 0xFF)", "//   000000FE  (SETR A 0xFF)", 1);
        let file = dir.path().join("wrong.kla");
        fs::write(&file, source).expect("write fixture");

        let (oplist, macro_list) = v2_tables();
        let verdict = verify_emulated(&file.to_string_lossy(), &oplist, &macro_list, emulate::DEFAULT_MAX_INSTRUCTIONS);
        assert_eq!(
            verdict,
            Ok(Verdict::Fail {
                matched: 0,
                expected: 6,
                diff: "at #1: expected 000000FE, got 000000FF".to_owned(),
            })
        );
    }

    #[test]
    fn verify_skips_a_file_without_expected_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("plain.kla");
        fs::write(&file, "// no expected values here\nHALT\n").expect("write fixture");
        let (oplist, macro_list) = v2_tables();
        let verdict = verify_emulated(&file.to_string_lossy(), &oplist, &macro_list, emulate::DEFAULT_MAX_INSTRUCTIONS);
        assert_eq!(verdict, Ok(Verdict::NotValidatable));
    }

    #[test]
    fn verify_fails_an_unreadable_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("missing.kla");
        let (oplist, macro_list) = v2_tables();
        let verdict = verify_emulated(&file.to_string_lossy(), &oplist, &macro_list, emulate::DEFAULT_MAX_INSTRUCTIONS);
        assert!(matches!(&verdict, Err(reason) if reason.starts_with("cannot read: ")), "{verdict:?}");
    }

    #[test]
    fn verify_fails_a_file_that_does_not_assemble() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("bad.kla");
        fs::write(&file, "//   00000001  (never printed)\nNOTANOPCODE A\nHALT\n").expect("write fixture");
        let (oplist, macro_list) = v2_tables();
        let verdict = verify_emulated(&file.to_string_lossy(), &oplist, &macro_list, emulate::DEFAULT_MAX_INSTRUCTIONS);
        assert_eq!(verdict, Err("assembly error".to_owned()));
    }
}
