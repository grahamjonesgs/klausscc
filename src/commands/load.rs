//! Putting a program on the board: net-load, mem-out (boot ROM image),
//! elf2serial and sending a pre-built `.kbt`.

use super::{SendOpts, send_image, write_binary_file};
use crate::diag::{Diagnostics, MessageType};
use crate::error::Error;
use crate::isa::disasm::disassemble_flat_to_pass2;
use crate::layout::CODE_BASE;
use crate::link::netload::net_load;
use crate::link::serial::AUTO_SERIAL;
use crate::loader::{BinaryInput, load_flat};
use crate::output::image::readmemh;
use crate::output::listing::write_code_output_file;
use std::fs;
use std::path::PathBuf;

/// Flatten an ELF (or take a flat binary) and stream it to the board over TCP.
///
/// Detects ELF magic automatically: ELF LOAD segments are extracted and the
/// entry point read from the header (overridable with `--entry`); a flat binary
/// is used verbatim with entry defaulting to `0x20`.  The flat image is wrapped
/// in the heap-header DDR layout (`build_ddr_image`) and sent via `net_load`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_netload(input: BinaryInput<'_>, board_ip: &str, board_port: u16, msg_list: &mut Diagnostics) -> Result<(), Error> {
    let loaded = load_flat(input, msg_list)?;
    msg_list.info(loaded.detected_message(true));

    if let Err(err) = net_load(board_ip, board_port, &loaded.image.to_ddr(), loaded.image.entry, msg_list) {
        msg_list.error(format!("netboot failed: \"{err}\""));
        return Err(Error::Netboot(err));
    }

    Ok(())
}

/// Flatten an ELF (or take a flat binary) and write a `$readmemh` boot-ROM image.
///
/// The image is the same `build_ddr_image` DDR layout used for net-load, emitted
/// as one 64-bit little-endian doubleword per line (16 hex chars) — matching
/// `boot_rom.v` (`DEPTH_DW` × 64-bit, `$readmemh`).  `boot_rom`'s copy FSM reads
/// word 0 (`heap_start` = image byte length) to know how much to copy to DDR.
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_mem_out(binary_path: &str, mem_file_name: &str, msg_list: &mut Diagnostics) -> Result<(), Error> {
    // mem-out writes a $readmemh image and does not need an entry point.
    let loaded = load_flat(
        BinaryInput {
            path: binary_path,
            entry: None,
        },
        msg_list,
    )?;
    msg_list.info(loaded.detected_message(false));

    let image = loaded.image.to_ddr();

    if let Err(e) = fs::write(mem_file_name, readmemh(&image)) {
        msg_list.error(format!("Failed to write boot ROM image {mem_file_name}: {e}"));
        return Err(Error::Write {
            path: PathBuf::from(mem_file_name),
            source: e,
        });
    }
    msg_list.info(format!(
        "mem-out: {binary_path} → {mem_file_name} ({} doublewords, {} bytes)",
        image.len() / 8,
        image.len()
    ));
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
pub(crate) fn run_elf2serial(input: BinaryInput<'_>, kbt_file_name: &str, send: SendOpts<'_>, msg_list: &mut Diagnostics) -> Result<(), Error> {
    let binary_path = input.path;
    // load_flat converts the ELF entry point to a board byte address: the
    // flat buffer starts at ELF VMA `elf_base` but is loaded by the board
    // immediately after the heap header, so board_entry = (elf_entry - elf_base)
    // + HEAP_HEADER_WORDS * 8.
    let loaded = load_flat(input, msg_list)?;
    msg_list.info(loaded.detected_message(true));

    // Pad to whole 32-bit words: the board loads words, and the .code
    // listing below shows the last part word as a whole one.
    let mut image = loaded.image;
    image.pad_to_word();
    let entry_addr = image.entry;
    let heap_start = image.heap_start();

    // The image was padded to whole words above, so this only fails on a bug,
    // but an image the board would reject is never written or sent.
    let Some(out) = image.to_kbt(msg_list) else {
        return Err(Error::Assembly(msg_list.count(MessageType::Error)));
    };

    msg_list.info(format!(
        "elf2serial: {binary_path} → {kbt_file_name} (entry 0x{entry_addr:08X}, heap_start 0x{heap_start:08X})"
    ));

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
        let opcodes = crate::isa::table::v2_opcodes();
        let code_file_name = {
            let stem = kbt_file_name.strip_suffix(".kbt").unwrap_or(kbt_file_name);
            format!("{stem}.code")
        };
        let pass2 = disassemble_flat_to_pass2(&image.bytes, CODE_BASE, &opcodes);
        if let Err(e) = write_code_output_file(&code_file_name, &pass2, msg_list) {
            msg_list.warning(format!("Failed to write disassembly file {code_file_name}: {e}"));
        }
    }

    // A bad image (e.g. a checksum error) is never sent to the board.
    let error_count = msg_list.count(MessageType::Error);
    if error_count > 0 {
        return outcome.and(Err(Error::Assembly(error_count)));
    }

    if !send.port.is_empty() {
        outcome = send_image(&out, send, msg_list);
    }
    outcome
}

/// Send a pre-built `.kbt` board wire-format image to the board and/or monitor it.
///
/// A `.kbt` file already holds the complete wire-format string (`S…Z…X`) produced
/// by an earlier assembly or elf2serial run, so it is sent verbatim — no opcode
/// file and no re-encoding.  With `-m` and no `-s`, the serial port is
/// auto-detected so a bare `-m` can both send and monitor.
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_kbt_send(kbt_path: &str, send: SendOpts<'_>, msg_list: &mut Diagnostics) -> Result<(), Error> {
    let wire = match fs::read_to_string(kbt_path) {
        Ok(contents) => contents,
        Err(e) => {
            msg_list.error(format!("Cannot read kbt file {kbt_path}: {e}"));
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
        msg_list.warning("Nothing to do: a .kbt input needs a serial port (-s) to send or -m to monitor");
        return Ok(());
    }

    msg_list.info(format!("Sending pre-built image {kbt_path} to board"));

    send_image(wire_trimmed, send, msg_list)
}
