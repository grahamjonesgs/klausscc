//! Subcommand handlers — each `run_*` function drives one CLI mode end to end
//! (net-load, mem-out, elf2serial, kbt send, emulate, and the test runners).

use crate::files::{filename_stem, read_file_to_vector, write_code_output_file};
use crate::helper::{build_ddr_image, disassemble_flat_to_pass2, encode_word_kbt, human_bytes, parse_expected_uart_values, HEAP_HEADER_WORDS};
use crate::messages::{print_messages, MessageType, MsgList};
use crate::netload::net_load;
use crate::opcodes::{parse_vh_file, Opcode, Pass2};
use crate::serial::{monitor_serial_port, run_test_monitor, write_to_board_keep_port, AUTO_SERIAL};
use crate::{assemble_file, assemble_to_image, build_flat_code, print_results, write_binary_file, write_to_device, ELF_MAGIC};
use crate::{emulate, helper, macros};
use chrono::NaiveTime;
use std::fmt::Write as _;
use std::fs;

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
    /// True if the test could not be assembled or had no expected values.
    skipped: bool,
    /// Reason for skipping, if applicable.
    skip_reason: String,
}

/// Parse an ELF file and extract its LOAD segments as a contiguous flat buffer.
///
/// Returns `(flat_bytes, base_address, entry_address)` where `flat_bytes` covers
/// the range `[base_address, base_address + flat_bytes.len())`.  Gaps between
/// non-contiguous LOAD segments are zero-filled.  Returns `None` if the file
/// cannot be parsed or contains no LOAD segments with data.
fn parse_elf_to_flat(data: &[u8]) -> Option<(Vec<u8>, u64, u64)> {
    use object::{Object as _, ObjectSegment as _, ObjectSymbol as _};
    /// Max gap between contiguous LOAD segments before the rest is dropped (see below).
    const MAX_SEGMENT_GAP: u64 = 0x40_0000; // 4 MB
    let file = object::File::parse(data).ok()?;

    // Collect all LOAD segments that actually have bytes in the file.
    let mut segments: Vec<(u64, Vec<u8>)> = file
        .segments()
        .filter_map(|seg| {
            let addr = seg.address();
            let bytes = seg.data().ok()?.to_vec();
            if bytes.is_empty() {
                None
            } else {
                Some((addr, bytes))
            }
        })
        .collect();
    segments.sort_by_key(|(addr, _)| *addr);

    if segments.is_empty() {
        return None;
    }

    // Only keep the first contiguous cluster of segments.  Embedded ELFs often
    // place code/data at a low VMA and a completely separate region (e.g. XIP
    // flash, ITCM, peripheral windows) at a very high VMA.  Zero-filling the
    // gap between them would produce a multi-hundred-MB flat buffer that stalls
    // encoding and disassembly.  Segments separated by more than MAX_SEGMENT_GAP
    // bytes from the previous one are treated as a different physical memory
    // region and dropped from this cluster.
    let cluster: Vec<(u64, Vec<u8>)> = segments
        .into_iter()
        .scan(None::<u64>, |prev_end, (addr, bytes)| {
            let gap = prev_end.map_or(0, |end| addr.saturating_sub(end));
            *prev_end = Some(addr + bytes.len() as u64);
            if gap <= MAX_SEGMENT_GAP {
                Some(Some((addr, bytes)))
            } else {
                Some(None)
            }
        })
        .take_while(Option::is_some)
        .flatten()
        .collect();

    if cluster.is_empty() {
        return None;
    }

    let base = cluster[0].0;
    let end = cluster.iter().map(|(addr, bytes)| addr + bytes.len() as u64).max()?;
    let mut flat = vec![0_u8; (end - base) as usize];
    for (addr, bytes) in &cluster {
        let offset = (addr - base) as usize;
        flat[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    // Determine entry point:
    // 1. ELF e_entry header field (set by linker ENTRY() directive)
    // 2. Fall back to _start symbol address
    // 3. Final fallback: base address of first LOAD segment
    let entry = {
        let e = file.entry();
        if e != 0 {
            e
        } else {
            file.symbols().find(|sym| sym.name() == Ok("_start")).map_or(base, |sym| sym.address())
        }
    };

    Some((flat, base, entry))
}

/// ELF layout captured while flattening, kept for diagnostic logging.
struct ElfLayout {
    /// ELF virtual base address (VMA of the first LOAD segment).
    base: u64,
    /// ELF entry point (`e_entry` / `_start`) as an ELF virtual address.
    entry: u64,
}

/// Flatten a loaded input file into a board image payload plus entry address.
///
/// Shared core of every `--net-load` / `--mem-out` / `elf2serial` / `--emulate`
/// path.  If `file_data` is an ELF (detected via [`ELF_MAGIC`]) its LOAD segments
/// are extracted ([`parse_elf_to_flat`]) and the entry point is translated from
/// ELF VMA to a board byte address: `(elf_entry - elf_base) + HEAP_HEADER_WORDS*8`
/// (the board loads the image immediately after the heap header).  Otherwise the
/// bytes are used verbatim with a default board entry of `0x20`.  `entry_override`,
/// when set, wins in both cases.
///
/// Returns `None` only when ELF parsing fails.  The returned `Option<ElfLayout>`
/// is `Some` for ELF inputs (for the caller's log line) and `None` for flat
/// binaries.  The returned bytes are NOT padded — callers that need 4-byte
/// alignment pad afterwards.
fn flatten_input(file_data: Vec<u8>, entry_override: Option<u32>) -> Option<(Vec<u8>, u32, Option<ElfLayout>)> {
    if file_data.starts_with(ELF_MAGIC) {
        let (flat, elf_base, elf_entry) = parse_elf_to_flat(&file_data)?;
        let board_entry = elf_entry.saturating_sub(elf_base) + u64::from(HEAP_HEADER_WORDS) * 8;
        let entry = entry_override.unwrap_or(board_entry as u32);
        Some((
            flat,
            entry,
            Some(ElfLayout {
                base: elf_base,
                entry: elf_entry,
            }),
        ))
    } else {
        Some((file_data, entry_override.unwrap_or(0x20_u32), None))
    }
}

/// Flatten an ELF (or take a flat binary) and stream it to the board over TCP.
///
/// Detects ELF magic automatically: ELF LOAD segments are extracted and the
/// entry point read from the header (overridable with `--entry`); a flat binary
/// is used verbatim with entry defaulting to `0x20`.  The flat image is wrapped
/// in the heap-header DDR layout (`build_ddr_image`) and sent via `net_load`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_netload(
    binary_path: &str,
    entry_override: Option<u32>,
    board_ip: &str,
    board_port: u16,
    msg_list: &mut MsgList,
    start_time: NaiveTime,
) -> Result<(), i32> {
    if board_ip.is_empty() {
        msg_list.push("net-load requires --ip <board address>".to_owned(), None, None, MessageType::Error);
        print_results(msg_list, start_time);
        return Err(1);
    }

    let file_data = fs::read(binary_path).map_err(|e| {
        msg_list.push(format!("Cannot read binary file {binary_path}: {e}"), None, None, MessageType::Error);
        1
    })?;

    let (mut binary_data, entry_addr, elf) = flatten_input(file_data, entry_override).ok_or_else(|| {
        msg_list.push(
            format!("Failed to extract LOAD segments from ELF file {binary_path}"),
            None,
            None,
            MessageType::Error,
        );
        1
    })?;
    if let Some(elf) = elf {
        msg_list.push(
            format!(
                "Detected ELF file: {}, ELF base 0x{:08X}, ELF entry 0x{:08X}, board entry 0x{entry_addr:08X}",
                human_bytes(binary_data.len()),
                elf.base,
                elf.entry
            ),
            None,
            None,
            MessageType::Information,
        );
    } else {
        msg_list.push("Detected flat binary (no ELF header)".to_owned(), None, None, MessageType::Information);
    }

    // Pad to a 4-byte boundary so every word is complete.
    while binary_data.len() % 4 != 0 {
        binary_data.push(0);
    }

    let image = build_ddr_image(&binary_data);

    if let Err(err) = net_load(board_ip, board_port, &image, entry_addr, msg_list) {
        msg_list.push(format!("netboot failed: \"{err}\""), None, None, MessageType::Error);
        print_results(msg_list, start_time);
        return Err(1);
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
pub(crate) fn run_mem_out(binary_path: &str, mem_file_name: &str, msg_list: &mut MsgList, start_time: NaiveTime) -> Result<(), i32> {
    let file_data = fs::read(binary_path).map_err(|e| {
        msg_list.push(format!("Cannot read binary file {binary_path}: {e}"), None, None, MessageType::Error);
        1
    })?;

    // mem-out writes a $readmemh image and does not need an entry point.
    let (mut binary_data, _entry, elf) = flatten_input(file_data, None).ok_or_else(|| {
        msg_list.push(
            format!("Failed to extract LOAD segments from ELF file {binary_path}"),
            None,
            None,
            MessageType::Error,
        );
        1
    })?;
    if let Some(elf) = elf {
        msg_list.push(
            format!(
                "Detected ELF file: {}, ELF base 0x{:08X}, ELF entry 0x{:08X}",
                human_bytes(binary_data.len()),
                elf.base,
                elf.entry
            ),
            None,
            None,
            MessageType::Information,
        );
    } else {
        msg_list.push("Detected flat binary (no ELF header)".to_owned(), None, None, MessageType::Information);
    }

    while binary_data.len() % 4 != 0 {
        binary_data.push(0);
    }
    let image = build_ddr_image(&binary_data);

    // One 64-bit little-endian doubleword per line. image is 8-byte aligned.
    let mut out = String::with_capacity(image.len() / 8 * 17);
    for chunk in image.chunks(8) {
        let dw = u64::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7]]);
        let _ = writeln!(out, "{dw:016X}");
    }

    if let Err(e) = fs::write(mem_file_name, &out) {
        msg_list.push(
            format!("Failed to write boot ROM image {mem_file_name}: {e}"),
            None,
            None,
            MessageType::Error,
        );
        print_results(msg_list, start_time);
        return Err(1);
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
/// The resulting wire string is written to a `.kbt` file and, when a serial port
/// is given, sent to the board exactly like the normal assembly path.
#[cfg(not(tarpaulin_include))]
#[allow(clippy::too_many_arguments, reason = "mirrors the full assemble→send path; all parameters are required")]
pub(crate) fn run_elf2serial(
    binary_path: &str,
    entry_override: Option<u32>,
    opcode_file_name: &str,
    output_serial_port: &str,
    kbt_file_name: &str,
    monitor_flag: bool,
    debug_flag: bool,
    no_break_flag: bool,
    msg_list: &mut MsgList,
    start_time: NaiveTime,
) -> Result<(), i32> {
    use helper::calc_checksum;

    let file_data = fs::read(binary_path).map_err(|e| {
        msg_list.push(format!("Cannot read binary file {binary_path}: {e}"), None, None, MessageType::Error);
        1
    })?;

    // Convert ELF virtual address → board byte address inside flatten_input:
    // the flat buffer starts at ELF VMA `elf_base` but is loaded by the board
    // immediately after the heap header, so board_entry = (elf_entry - elf_base)
    // + HEAP_HEADER_WORDS * 8.
    let (mut binary_data, entry_addr, elf) = flatten_input(file_data, entry_override).ok_or_else(|| {
        msg_list.push(
            format!("Failed to extract LOAD segments from ELF file {binary_path}"),
            None,
            None,
            MessageType::Error,
        );
        1
    })?;
    if let Some(elf) = elf {
        msg_list.push(
            format!(
                "Detected ELF file: {}, ELF base 0x{:08X}, ELF entry 0x{:08X}, board entry 0x{entry_addr:08X}",
                human_bytes(binary_data.len()),
                elf.base,
                elf.entry
            ),
            None,
            None,
            MessageType::Information,
        );
    } else {
        msg_list.push("Detected flat binary (no ELF header)".to_owned(), None, None, MessageType::Information);
    }

    // Pad binary_data to 4-byte alignment so every word is complete.
    while binary_data.len() % 4 != 0 {
        binary_data.push(0);
    }

    let mut out = String::new();
    out.push('S');

    // Word 0: heap_start placeholder, patched below
    let heap_start_offset = out.len();
    out.push_str("0000000000000000");
    // Words 1-3: reserved header words
    for _ in 1..HEAP_HEADER_WORDS {
        out.push_str("0000000000000000");
    }

    // Encode program words for the kbt wire format.
    // The LLVM ELF stores instruction bytes in little-endian order, so read each
    // 4-byte chunk as LE to reconstruct the 32-bit value, then encode it with the
    // half-word byte-swap the board's serial loader expects.
    for chunk in binary_data.chunks(4) {
        let word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        out.push_str(&encode_word_kbt(word));
    }

    // Patch heap_start: lo32 = heap_start value (encoded for LE board), hi32 = 0.
    let heap_start_raw: u32 = ((out.len() - 1) / 2) as u32;
    let heap_start: u32 = (heap_start_raw + 7) & !7_u32; // align to 8-byte boundary
    out.replace_range(
        heap_start_offset..heap_start_offset + 16,
        &format!("{}00000000", encode_word_kbt(heap_start)),
    );

    // Entry point — LE-encoded like all other 32-bit words per FPGA team spec.
    out.push_str(&encode_word_kbt(entry_addr));

    // Checksum computed before Z delimiter; returns 8-char LE 32-bit word.
    let checksum = calc_checksum(&out, msg_list);
    out.push('Z');
    out.push_str(&checksum);
    out.push('X');

    msg_list.push(
        format!("elf2serial: {binary_path} → {kbt_file_name} (entry 0x{entry_addr:08X}, heap_start 0x{heap_start:08X})"),
        None,
        None,
        MessageType::Information,
    );

    /* Skip the on-disk .kbt / .code when streaming to the board over serial:
     * the image is sent straight from memory (`out`), so writing these (large)
     * files just adds several seconds for no benefit. */
    if output_serial_port.is_empty() {
        write_binary_file(msg_list, kbt_file_name, &out);
    }

    // Optionally produce a .code disassembly listing alongside the .kbt.
    // Try to load the opcode file; if it exists and parses, disassemble binary_data.
    // If the file is absent or fails to parse, skip silently.  (Also skipped for
    // a serial load — see above.)
    if output_serial_port.is_empty() && std::path::Path::new(opcode_file_name).exists() {
        let mut tmp_msgs = MsgList::new();
        let mut opened: Vec<String> = Vec::new();
        let opt_opcodes = read_file_to_vector(opcode_file_name, &mut tmp_msgs, &mut opened).and_then(|vh| parse_vh_file(vh, &mut tmp_msgs).0);
        if let Some(opcodes) = opt_opcodes {
            let code_file_name = {
                let stem = kbt_file_name.strip_suffix(".kbt").unwrap_or(kbt_file_name);
                format!("{stem}.code")
            };
            let mut pass2 = disassemble_flat_to_pass2(&binary_data, HEAP_HEADER_WORDS * 8, &opcodes);
            if let Err(e) = write_code_output_file(&code_file_name, &mut pass2, msg_list) {
                msg_list.push(
                    format!("Failed to write disassembly file {code_file_name}: {e}"),
                    None,
                    None,
                    MessageType::Warning,
                );
            }
        } else {
            msg_list.push(
                format!("Opcode file {opcode_file_name} found but could not be parsed — skipping .code output"),
                None,
                None,
                MessageType::Warning,
            );
        }
    }

    if !output_serial_port.is_empty() {
        if monitor_flag {
            match write_to_board_keep_port(&out, output_serial_port, !no_break_flag, true, msg_list) {
                Ok(port) => {
                    msg_list.push("Wrote to serial port".to_owned(), None, None, MessageType::Information);
                    print_results(msg_list, start_time);
                    if let Err(err) = monitor_serial_port(port, debug_flag, msg_list) {
                        msg_list.push(format!("Serial monitor stopped: \"{err}\""), None, None, MessageType::Warning);
                    }
                    return Ok(());
                }
                Err(err) => {
                    msg_list.push(format!("Failed to write to serial port, error \"{err}\""), None, None, MessageType::Error);
                    print_results(msg_list, start_time);
                    return Err(1);
                }
            }
        }
        write_to_device(msg_list, &out, output_serial_port, !no_break_flag);
    }

    print_results(msg_list, start_time);
    Ok(())
}

/// Send a pre-built `.kbt` board wire-format image to the board and/or monitor it.
///
/// A `.kbt` file already holds the complete wire-format string (`S…Z…X`) produced
/// by an earlier assembly or elf2serial run, so it is sent verbatim — no opcode
/// file and no re-encoding.  With `-m` and no `-s`, the serial port is
/// auto-detected so a bare `-m` can both send and monitor.
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_kbt_send(
    kbt_path: &str,
    output_serial_port: &str,
    monitor_flag: bool,
    debug_flag: bool,
    no_break_flag: bool,
    msg_list: &mut MsgList,
    start_time: NaiveTime,
) -> Result<(), i32> {
    let wire = match fs::read_to_string(kbt_path) {
        Ok(contents) => contents,
        Err(e) => {
            msg_list.push(format!("Cannot read kbt file {kbt_path}: {e}"), None, None, MessageType::Error);
            print_results(msg_list, start_time);
            return Err(1);
        }
    };
    let wire_trimmed = wire.trim();

    // A bare `-m` with no `-s` should still send: auto-detect the serial port.
    let serial_port = if output_serial_port.is_empty() && monitor_flag {
        AUTO_SERIAL
    } else {
        output_serial_port
    };

    if serial_port.is_empty() {
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

    if monitor_flag {
        match write_to_board_keep_port(wire_trimmed, serial_port, !no_break_flag, true, msg_list) {
            Ok(port) => {
                msg_list.push("Wrote to serial port".to_owned(), None, None, MessageType::Information);
                print_results(msg_list, start_time);
                if let Err(err) = monitor_serial_port(port, debug_flag, msg_list) {
                    msg_list.push(format!("Serial monitor stopped: \"{err}\""), None, None, MessageType::Warning);
                }
                return Ok(());
            }
            Err(err) => {
                msg_list.push(format!("Failed to write to serial port, error \"{err}\""), None, None, MessageType::Error);
                print_results(msg_list, start_time);
                return Err(1);
            }
        }
    }

    write_to_device(msg_list, wire_trimmed, serial_port, !no_break_flag);
    print_results(msg_list, start_time);
    Ok(())
}

/// Run test verification mode.
///
/// Sends program to board, reads UART output, and verifies against expected values
/// parsed from the source file comments.
#[inline]
#[cfg(not(tarpaulin_include))] // Cannot test serial hardware in tarpaulin
pub fn run_test_mode(
    msg_list: &mut MsgList,
    bin_string: &str,
    output_serial_port: &str,
    input_file_name: &str,
    test_timeout: u64,
    send_break: bool,
    start_time: NaiveTime,
) -> Result<(), i32> {
    // Parse expected values from the raw source file
    let raw_lines: Vec<String> = fs::read_to_string(input_file_name)
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect();
    let expected_values = parse_expected_uart_values(&raw_lines);

    if expected_values.is_empty() {
        msg_list.push(
            "No expected UART values found in source file comments".to_owned(),
            None,
            None,
            MessageType::Error,
        );
        print_results(msg_list, start_time);
        return Err(1);
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
        Err(err) => {
            msg_list.push(format!("Failed to write to serial port, error \"{err}\""), None, None, MessageType::Error);
            print_results(msg_list, start_time);
            return Err(1);
        }
    };

    print_results(msg_list, start_time);

    // Run test verification
    let result = run_test_monitor(port, &expected_values, test_timeout, msg_list);

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
        Err(if result.timed_out { 3 } else { 2 })
    } else {
        Ok(())
    }
}

/// Run the emulator on a single assembled program (`--emulate`).
///
/// Builds the flat DDR image, executes the golden model, prints captured UART
/// output, and writes the per-instruction trace to `trace_file` (or stdout).
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_emulate(
    pass2: &[Pass2],
    input_file_name: &str,
    trace_file: Option<&str>,
    max_instructions: u64,
    msg_list: &mut MsgList,
    start_time: NaiveTime,
) -> Result<(), i32> {
    if msg_list.number_by_type(&MessageType::Error) > 0 {
        msg_list.push("Not emulating due to assembly errors".to_owned(), None, None, MessageType::Error);
        print_results(msg_list, start_time);
        return Err(1);
    }
    let Some((code, entry)) = build_flat_code(pass2) else {
        msg_list.push("No _start address found - cannot emulate".to_owned(), None, None, MessageType::Error);
        print_results(msg_list, start_time);
        return Err(1);
    };
    let image = build_ddr_image(&code);
    msg_list.push(
        format!("Emulating {input_file_name}: {} code bytes, entry 0x{entry:08X}", code.len()),
        None,
        None,
        MessageType::Information,
    );

    let (result, trace) = emulate::emulate_image(&image, entry, max_instructions, trace_file.is_some());

    if let (Some(path), Some(text)) = (trace_file, trace.as_ref()) {
        if let Err(e) = fs::write(path, text) {
            msg_list.push(format!("Failed to write trace file {path}: {e}"), None, None, MessageType::Error);
        } else {
            msg_list.push(
                format!("Wrote {} instruction trace lines to {path}", result.instructions),
                None,
                None,
                MessageType::Information,
            );
        }
    }

    print_results(msg_list, start_time);
    println!(
        "--- Emulator finished: {} instructions, stop = {:?} ---",
        result.instructions, result.stop
    );
    if trace_file.is_none() {
        if let Some(text) = trace.as_ref() {
            print!("{text}");
        }
    }
    println!("--- Captured UART output ---");
    print!("{}", result.uart);
    println!("--- end UART ---");
    Ok(())
}

/// Run the emulator on an ELF or flat binary input (`--emulate` with binary input).
///
/// Mirrors `run_elf2serial`'s ELF → flat → board-address conversion, then builds
/// the DDR image and runs the golden model instead of sending it to the board, so
/// prebuilt C ELFs (queens, `test_64bit`, …) can be cross-checked against the RTL.
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_emulate_elf(
    binary_path: &str,
    entry_override: Option<u32>,
    trace_file: Option<&str>,
    max_instructions: u64,
    msg_list: &mut MsgList,
    start_time: NaiveTime,
) -> Result<(), i32> {
    let file_data = fs::read(binary_path).map_err(|e| {
        msg_list.push(format!("Cannot read binary file {binary_path}: {e}"), None, None, MessageType::Error);
        1
    })?;

    let (binary_data, entry_addr, elf) = flatten_input(file_data, entry_override).ok_or_else(|| {
        msg_list.push(
            format!("Failed to extract LOAD segments from ELF file {binary_path}"),
            None,
            None,
            MessageType::Error,
        );
        1
    })?;
    if let Some(elf) = elf {
        msg_list.push(
            format!(
                "Emulating ELF {binary_path}: {}, ELF base 0x{:08X}, board entry 0x{entry_addr:08X}",
                human_bytes(binary_data.len()),
                elf.base
            ),
            None,
            None,
            MessageType::Information,
        );
    } else {
        msg_list.push(format!("Emulating flat binary {binary_path}"), None, None, MessageType::Information);
    }

    let image = build_ddr_image(&binary_data);
    let (result, trace) = emulate::emulate_image(&image, entry_addr, max_instructions, trace_file.is_some());

    if let (Some(path), Some(text)) = (trace_file, trace.as_ref()) {
        if let Err(e) = fs::write(path, text) {
            msg_list.push(format!("Failed to write trace file {path}: {e}"), None, None, MessageType::Error);
        } else {
            msg_list.push(
                format!("Wrote {} instruction trace lines to {path}", result.instructions),
                None,
                None,
                MessageType::Information,
            );
        }
    }

    print_results(msg_list, start_time);
    println!(
        "--- Emulator finished: {} instructions, stop = {:?} ---",
        result.instructions, result.stop
    );
    if trace_file.is_none() {
        if let Some(text) = trace.as_ref() {
            print!("{text}");
        }
    }
    println!("--- Captured UART output ---");
    print!("{}", result.uart);
    println!("--- end UART ---");
    Ok(())
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
    start_time: NaiveTime,
) -> Result<(), i32> {
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
        return Err(1);
    }

    println!("Emulator verification over {} file(s):\n", files.len());
    let mut total_pass = 0_usize;
    let mut total_files = 0_usize;
    let mut failed_files: Vec<String> = Vec::new();

    for file in &files {
        // Expected values from source comments.
        let raw_lines: Vec<String> = fs::read_to_string(file).unwrap_or_default().lines().map(String::from).collect();
        let expected = parse_expected_uart_values(&raw_lines);
        if expected.is_empty() {
            continue; // skip non-validatable files silently
        }
        total_files += 1;

        let mut test_msgs = MsgList::new();
        let Some((image, entry)) = assemble_to_image(file, oplist, macro_list, &mut test_msgs) else {
            println!("  FAIL {file}: assembly error");
            failed_files.push(format!("{file} (assembly error)"));
            continue;
        };

        let (result, _) = emulate::emulate_image(&image, entry, max_instructions, false);
        // Extract 8-hex-digit tokens from each UART line (mirrors serial.rs).
        let got: Vec<String> = result
            .uart
            .lines()
            .filter_map(|l| {
                let t = l.trim();
                if t.len() >= 8 {
                    let c = t.get(..8).unwrap_or("");
                    if c.len() == 8 && c.chars().all(|ch| ch.is_ascii_hexdigit()) && c == c.to_ascii_uppercase() {
                        return Some(c.to_owned());
                    }
                }
                None
            })
            .collect();

        // Compare in order; report first diff.
        let mut matched = 0_usize;
        let mut first_diff: Option<String> = None;
        for (idx, exp) in expected.iter().enumerate() {
            match got.get(idx) {
                Some(g) if g == exp => matched += 1,
                Some(g) => {
                    first_diff = Some(format!("at #{}: expected {exp}, got {g}", idx + 1));
                    break;
                }
                None => {
                    first_diff = Some(format!("at #{}: expected {exp}, got <none> (stop={:?})", idx + 1, result.stop));
                    break;
                }
            }
        }

        let stem = filename_stem(&file.clone());
        if matched == expected.len() {
            total_pass += 1;
            println!("  PASS {stem}: {matched}/{} ({} instrs)", expected.len(), result.instructions);
        } else {
            let diff = first_diff.unwrap_or_else(|| "unknown".to_owned());
            println!("  FAIL {stem}: {matched}/{} — {diff}", expected.len());
            failed_files.push(format!("{stem}: {diff}"));
        }
    }

    println!("\nEmulator verification: {total_pass}/{total_files} files passed");
    if !failed_files.is_empty() {
        println!("Failures:");
        for f in &failed_files {
            println!("  {f}");
        }
    }
    print_results(msg_list, start_time);
    if total_pass == total_files {
        Ok(())
    } else {
        Err(2)
    }
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
) -> Result<(), i32> {
    if output_serial_port.is_empty() {
        msg_list.push("Test list (-L) requires a serial port (-s)".to_owned(), None, None, MessageType::Error);
        print_messages(msg_list);
        return Err(1);
    }

    let test_files = read_test_list(list_file, msg_list);
    if test_files.is_empty() {
        msg_list.push("No test files found in test list".to_owned(), None, None, MessageType::Error);
        print_messages(msg_list);
        return Err(1);
    }

    println!("Running {} tests from {list_file}...\n", test_files.len());

    let mut results: Vec<BatchTestResult> = Vec::new();

    for (index, test_file) in test_files.iter().enumerate() {
        println!("--- [{}/{}] {} ---", index + 1, test_files.len(), test_file);

        // Fresh message list for each test to avoid error accumulation
        let mut test_msg_list = MsgList::new();

        // Assemble the test file
        let Some(bin_string) = assemble_file(test_file, oplist, macro_list, &mut test_msg_list) else {
            println!("  SKIP: assembly failed");
            print_messages(&test_msg_list);
            results.push(BatchTestResult {
                file_name: test_file.clone(),
                passed: 0,
                failed: 0,
                timed_out: false,
                total: 0,
                skipped: true,
                skip_reason: "assembly error".to_owned(),
            });
            println!();
            continue;
        };

        // Write binary file
        let binary_file_name = format!("{}.kbt", filename_stem(test_file));
        write_binary_file(&mut test_msg_list, &binary_file_name, &bin_string);

        // Parse expected values
        let raw_lines: Vec<String> = fs::read_to_string(test_file).unwrap_or_default().lines().map(String::from).collect();
        let expected_values = parse_expected_uart_values(&raw_lines);

        if expected_values.is_empty() {
            println!("  SKIP: no expected UART values in source comments");
            results.push(BatchTestResult {
                file_name: test_file.clone(),
                passed: 0,
                failed: 0,
                timed_out: false,
                total: 0,
                skipped: true,
                skip_reason: "no expected values".to_owned(),
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
                    skipped: true,
                    skip_reason: format!("serial error: {err}"),
                });
                println!();
                continue;
            }
        };

        // Run test verification
        let result = run_test_monitor(port, &expected_values, test_timeout, &mut test_msg_list);

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
            skipped: false,
            skip_reason: String::new(),
        });

        println!();
    }

    // Print aggregate summary
    println!("=== Test Suite Summary ===");
    let mut total_failed: usize = 0;
    let mut total_skipped: usize = 0;
    let mut total_timed_out: usize = 0;
    let mut files_all_pass: usize = 0;

    for r in &results {
        if r.skipped {
            total_skipped += 1;
            println!("  SKIP  {} ({})", r.file_name, r.skip_reason);
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

    if total_failed > 0 || total_timed_out > 0 {
        Err(2)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests may unwrap/expect")]
    use super::*;

    /// Read a git-tracked ELF fixture from the klatest corpus (path is absolute
    /// via `CARGO_MANIFEST_DIR`, so the test is independent of the working dir).
    fn fixture(name: &str) -> Vec<u8> {
        let path = format!("{}/src/klatest/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(&path).unwrap_or_else(|e| panic!("read fixture {path}: {e}"))
    }

    // ---- parse_elf_to_flat ---------------------------------------------------

    #[test]
    fn parse_elf_extracts_hello() {
        let (flat, base, entry) = parse_elf_to_flat(&fixture("hello.elf")).expect("hello.elf should parse");
        assert_eq!(base, 0x20, "first LOAD segment VMA");
        assert_eq!(entry, 0x20, "entry point (e_entry)");
        assert_eq!(flat.len(), 3980, "flattened LOAD-segment image length");
        // The entry point must land inside the loaded image.
        assert!(entry >= base && (entry - base) < flat.len() as u64, "entry within loaded range");
    }

    #[test]
    fn parse_elf_extracts_test_64bit() {
        let (flat, base, entry) = parse_elf_to_flat(&fixture("test_64bit.elf")).expect("should parse");
        assert_eq!((base, entry, flat.len()), (0x20, 0x20, 8507));
    }

    #[test]
    fn parse_elf_is_deterministic() {
        let data = fixture("hello.elf");
        assert_eq!(parse_elf_to_flat(&data), parse_elf_to_flat(&data));
    }

    #[test]
    fn parse_elf_rejects_non_elf() {
        assert!(parse_elf_to_flat(b"not an ELF file at all").is_none());
        assert!(parse_elf_to_flat(&[]).is_none());
    }

    #[test]
    fn parse_elf_rejects_truncated_elf() {
        // ELF magic but an otherwise-invalid/empty header → no usable LOAD segments.
        let mut bad = ELF_MAGIC.to_vec();
        bad.extend_from_slice(&[0_u8; 64]);
        assert!(parse_elf_to_flat(&bad).is_none());
    }

    // ---- flatten_input -------------------------------------------------------

    #[test]
    fn flatten_flat_binary_uses_default_entry() {
        let raw = vec![0xDE_u8, 0xAD, 0xBE, 0xEF];
        let (data, entry, elf) = flatten_input(raw.clone(), None).expect("flat path is infallible");
        assert_eq!(data, raw, "flat bytes pass through verbatim");
        assert_eq!(entry, 0x20, "default board entry for a flat binary");
        assert!(elf.is_none(), "no ELF metadata for a flat binary");
    }

    #[test]
    fn flatten_flat_binary_entry_override_wins() {
        let (_, entry, _) = flatten_input(vec![1, 2, 3], Some(0x1000)).expect("flat path");
        assert_eq!(entry, 0x1000);
    }

    #[test]
    fn flatten_elf_translates_entry_to_board_address() {
        let data = fixture("hello.elf");
        let (flat, base, elf_entry) = parse_elf_to_flat(&data).unwrap();
        let (fdata, entry, meta) = flatten_input(data, None).expect("ELF should flatten");

        assert_eq!(fdata, flat, "ELF path returns the parse_elf_to_flat buffer");
        let meta = meta.expect("ELF metadata present for ELF input");
        assert_eq!((meta.base, meta.entry), (base, elf_entry), "metadata mirrors parse output");

        // board entry = (elf_entry - elf_base) + heap-header bytes
        let expected = (elf_entry.saturating_sub(base) + u64::from(HEAP_HEADER_WORDS) * 8) as u32;
        assert_eq!(entry, expected, "VMA-to-board entry translation");
        assert_eq!(entry, 0x20, "hello.elf loads at the start of the post-header image");
    }

    #[test]
    fn flatten_elf_entry_override_wins() {
        let (_, entry, meta) = flatten_input(fixture("hello.elf"), Some(0xABC)).expect("ELF");
        assert_eq!(entry, 0xABC, "explicit --entry overrides the computed board entry");
        assert!(meta.is_some(), "override does not suppress ELF metadata");
    }

    #[test]
    fn flatten_rejects_bad_elf() {
        // Starts with ELF magic (so the flat fallback is skipped) but is not a
        // valid ELF → flatten_input propagates parse_elf_to_flat's None.
        let mut bad = ELF_MAGIC.to_vec();
        bad.extend_from_slice(&[0_u8; 64]);
        assert!(flatten_input(bad, None).is_none());
    }
}
