//! Running programs on the golden-model emulator: `--emulate` for one
//! program, `--emulate-test` to check a corpus against its expected UART
//! output.

use super::test_runner::read_test_list;
use crate::asm::macros;
use crate::asm::{Assembly, assemble};
use crate::diag::{Diagnostics, MessageType};
use crate::emu;
use crate::error::Error;
use crate::isa::Isa;
use crate::link::serial::{hex_token, parse_expected_uart_values};
use crate::loader::{BinaryInput, Loaded, load_flat};
use crate::num::human_bytes;
use crate::output::filename_stem;
use crate::output::image::Image;
use std::fs;
use std::path::{Path, PathBuf};

/// Open the destination for the emulator's per-instruction trace.
///
/// Follows the `--trace` convention: `None` → no trace, `Some("-")` → stdout,
/// `Some(path)` → that file.  Returns `Ok(None)` when no trace was requested,
/// `Ok(Some(writer))` otherwise, or an error if the file cannot be created (the
/// error is already reported to `msg_list`).  The writer is line-buffered so the
/// trace streams during the run instead of being held in memory.
#[cfg(not(tarpaulin_include))]
fn open_trace_writer(trace_file: Option<&str>, msg_list: &mut Diagnostics) -> Result<Option<Box<dyn std::io::Write>>, Error> {
    use std::io::BufWriter;
    match trace_file {
        None => Ok(None),
        Some("-") => Ok(Some(Box::new(BufWriter::new(std::io::stdout())))),
        Some(path) => match std::fs::File::create(path) {
            Ok(f) => Ok(Some(Box::new(BufWriter::new(f)))),
            Err(e) => {
                msg_list.error(format!("Failed to create trace file {path}: {e}"));
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
fn finish_trace_writer(mut writer: Option<Box<dyn std::io::Write>>, trace_file: Option<&str>, instructions: u64, msg_list: &mut Diagnostics) {
    use std::io::Write as _;
    if let Some(w) = writer.as_mut() {
        let _ = w.flush();
    }
    if let Some(path) = trace_file
        && path != "-"
    {
        msg_list.info(format!("Wrote {instructions} instruction trace lines to {path}"));
    }
}

/// Run `image` on the emulator and report the run (`--emulate`).
///
/// The program starts with the UART transmitter idle, as one loaded onto the
/// board does (see [`emu::emulate_image`]).  Streams the
/// per-instruction trace to `trace_file` (or, for `"-"`, stdout), prints the
/// summary, then the instruction count, the stop reason, any `TX_DATA` store
/// made while the transmitter was busy, and the captured UART output on stdout.
#[cfg(not(tarpaulin_include))]
fn emulate_and_report(image: &Image, trace_file: Option<&str>, max_instructions: u64, msg_list: &mut Diagnostics) -> Result<(), Error> {
    let mut trace_writer = open_trace_writer(trace_file, msg_list)?;
    let result = emu::emulate_image(
        &image.to_ddr(),
        image.entry,
        max_instructions,
        trace_writer.as_mut().map(|w| w.as_mut() as &mut dyn std::io::Write),
    );
    finish_trace_writer(trace_writer, trace_file, result.instructions, msg_list);
    if !result.tx_busy_writes.is_empty() {
        msg_list.warning(tx_busy_reason(&result.tx_busy_writes));
    }

    msg_list.print_summary();
    println!(
        "--- Emulator finished: {} instructions, stop = {:?} ---",
        result.instructions, result.stop
    );
    if !result.tx_busy_writes.is_empty() {
        println!("--- UART TX writes while busy (dropped, as on the board; poll STATUS.TX_BUSY first) ---");
        println!("{}", result.tx_busy_writes);
    }
    println!("--- Captured UART output ---");
    {
        use std::io::Write as _;
        let mut out = std::io::stdout();
        let _ = out.write_all(&result.uart);
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
    msg_list: &mut Diagnostics,
) -> Result<(), Error> {
    if msg_list.has_errors() {
        msg_list.error("Not emulating due to assembly errors");
        return Err(Error::Assembly(msg_list.count(MessageType::Error)));
    }
    let Some(image) = assembly.image() else {
        msg_list.error("No _start address found - cannot emulate");
        return Err(Error::Assembly(1));
    };
    msg_list.info(format!(
        "Emulating {input_file_name}: {} code bytes, entry 0x{:08X}",
        image.bytes.len(),
        image.entry
    ));
    emulate_and_report(&image, trace_file, max_instructions, msg_list)
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
    msg_list: &mut Diagnostics,
) -> Result<(), Error> {
    let binary_path = input.path;
    let Loaded { image, elf } = load_flat(input, msg_list)?;
    if let Some(elf) = elf {
        msg_list.info(format!(
            "Emulating ELF {binary_path}: {}, ELF base 0x{:08X}, board entry 0x{:08X}",
            human_bytes(image.bytes.len()),
            elf.base,
            image.entry
        ));
    } else {
        msg_list.info(format!("Emulating flat binary {binary_path}"));
    }

    emulate_and_report(&image, trace_file, max_instructions, msg_list)
}

/// Batch-verify the emulator against `.kla` files with expected `// ` UART values.
///
/// `test_path` may be a single `.kla` file, a directory (all `*.kla` inside),
/// or a test-list file (one path per line).  For each file with expected
/// values, assemble + emulate and compare captured UART tokens in order.
#[cfg(not(tarpaulin_include))]
pub(crate) fn run_emulate_test(
    isa: &Isa,
    macro_list: &[macros::Macro],
    test_path: &Path,
    max_instructions: u64,
    msg_list: &mut Diagnostics,
) -> Result<(), Error> {
    // Resolve the list of .kla files to test.
    let path = test_path;
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
    } else if let Some(file) = test_path.to_str().filter(|name| name.ends_with(".kla")) {
        files.push(file.to_owned());
    } else {
        // treat as a test-list file
        files = read_test_list(test_path, msg_list);
    }

    if files.is_empty() {
        msg_list.error(format!("No .kla files found for emulate-test at {}", test_path.display()));
        return Err(Error::NoTests(test_path.to_path_buf()));
    }

    println!("Emulator verification over {} file(s):\n", files.len());
    let mut total_pass = 0_usize;
    let mut total_files = 0_usize;
    let mut failed_files: Vec<String> = Vec::new();

    for file in &files {
        match verify_emulated(file, isa, macro_list, max_instructions) {
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
        msg_list.error(format!(
            "No files with expected UART values found for emulate-test at {}",
            test_path.display()
        ));
        return Err(Error::NoTests(test_path.to_path_buf()));
    }
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
pub(crate) fn verify_emulated(file: &str, isa: &Isa, macro_list: &[macros::Macro], max_instructions: u64) -> Result<Verdict, String> {
    // A file that cannot be read is a failure, not a silently skipped file.
    let source = fs::read_to_string(file).map_err(|e| format!("cannot read: {e}"))?;
    let raw_lines: Vec<String> = source.lines().map(String::from).collect();
    let expected = parse_expected_uart_values(&raw_lines);
    if expected.is_empty() {
        return Ok(Verdict::NotValidatable);
    }

    let mut test_msgs = Diagnostics::new();
    let assembly = assemble(file, isa, macro_list, &mut test_msgs);
    let Some(image) = assembly.ok().filter(|_| !test_msgs.has_errors()).and_then(|assembly| assembly.image()) else {
        return Err("assembly error".to_owned());
    };

    let result = emu::emulate_image(&image.to_ddr(), image.entry, max_instructions, None);
    // Each byte as one `char` (Latin-1): every byte survives into the lines.
    let uart: String = result.uart.iter().copied().map(char::from).collect();
    let got: Vec<&str> = uart.lines().filter_map(hex_token).collect();

    // A store while the transmitter is busy loses a byte on the board, even
    // where the values that survive happen to match.
    if !result.tx_busy_writes.is_empty() {
        return Ok(Verdict::Fail {
            matched: expected.iter().zip(&got).take_while(|(exp, got)| exp == got).count(),
            expected: expected.len(),
            diff: tx_busy_reason(&result.tx_busy_writes),
        });
    }

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

/// The one-line reason a run with `TX_DATA` stores while busy fails: the first
/// store, and how many there were.
fn tx_busy_reason(writes: &emu::TxBusyWrites) -> String {
    let first = writes.first.first().map_or_else(String::new, ToString::to_string);
    if writes.total == 1 {
        format!("{first}; would lose output on hardware")
    } else {
        format!("{first} (+{} more); would lose output on hardware", writes.total - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- verify_emulated -----------------------------------------------------

    /// Absolute path of the emulator fixture corpus.
    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    /// The built-in v2 opcode table and expanded macros, as the CLI loads them.
    fn v2_tables() -> (Isa, Vec<macros::Macro>) {
        let mut msg_list = Diagnostics::new();
        let macro_list = macros::expand_embedded_macros(macros::builtin_macros(), &mut msg_list);
        (Isa::v2(), macro_list)
    }

    /// The corpus `--emulate-test tests/fixtures` runs: every file with expected
    /// UART values must assemble and print exactly those values on the emulator.
    #[test]
    fn emulator_corpus_passes() {
        let (isa, macro_list) = v2_tables();
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
            match verify_emulated(&file, &isa, &macro_list, emu::DEFAULT_MAX_INSTRUCTIONS) {
                Ok(Verdict::NotValidatable) => {}
                Ok(Verdict::Pass { .. }) => passed.push(file.into_owned()),
                other => failures.push(format!("{file}: {other:?}")),
            }
        }
        assert!(failures.is_empty(), "emulator corpus failures: {failures:#?}");
        // Guards against the corpus silently shrinking (e.g. a file that stops
        // declaring its expected values).
        assert!(passed.len() >= 28, "only {} validated files passed: {passed:#?}", passed.len());
    }

    /// `all_tests.txt` is what a board run (`-L`) checks, so it must list every
    /// fixture the emulator corpus checks, and nothing that is not there.
    #[test]
    fn all_tests_lists_every_file_with_expected_values() {
        let mut with_values: Vec<String> = fs::read_dir(fixture_dir())
            .expect("read fixture dir")
            .flatten()
            .map(|entry| entry.path())
            .filter(|p| p.extension().is_some_and(|e| e == "kla"))
            .filter(|p| {
                let source = fs::read_to_string(p).expect("read fixture");
                let lines: Vec<String> = source.lines().map(String::from).collect();
                !parse_expected_uart_values(&lines).is_empty()
            })
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        with_values.sort();

        let mut msg_list = Diagnostics::new();
        let listed = read_test_list(&fixture_dir().join("all_tests.txt"), &mut msg_list);
        assert!(!msg_list.has_errors());
        for file in &listed {
            assert!(Path::new(file).is_file(), "{file} is listed but missing");
        }
        let missing: Vec<&String> = with_values.iter().filter(|f| !listed.contains(f)).collect();
        assert!(missing.is_empty(), "files with expected values missing from all_tests.txt: {missing:#?}");
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

        let (isa, macro_list) = v2_tables();
        let verdict = verify_emulated(&file.to_string_lossy(), &isa, &macro_list, emu::DEFAULT_MAX_INSTRUCTIONS);
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
    fn verify_passes_a_program_whose_first_store_does_not_poll() {
        // test_regs with its opening DELAYV and polled newline replaced by an
        // unpolled TX_DATA store: the program starts with the transmitter idle
        // (KlaussCPU.sv START_WAIT, klaussCPU 04db80e), so that first store
        // is taken, and the polled stubs then wait for it to go out.
        let dir = tempfile::tempdir().expect("tempdir");
        fs::copy(fixture_dir().join("uart_stubs.kla"), dir.path().join("uart_stubs.kla")).expect("copy stubs");
        let source = fs::read_to_string(fixture_dir().join("test_regs.kla")).expect("read test_regs");
        let opening = "_start\nDELAYV 0x0500\n$NEWLINE\n";
        assert!(source.contains(opening), "test_regs opens with a delay and a newline");
        let source = source.replacen(opening, "_start\nSETR B 0xF0010000\nSETR A 0x0A\nMEMSET8 A B\n", 1);
        let file = dir.path().join("test_regs.kla");
        fs::write(&file, source).expect("write test_regs");

        let (isa, macro_list) = v2_tables();
        let verdict = verify_emulated(&file.to_string_lossy(), &isa, &macro_list, emu::DEFAULT_MAX_INSTRUCTIONS);
        assert!(matches!(verdict, Ok(Verdict::Pass { matched: 6, .. })), "{verdict:?}");
    }

    #[test]
    fn verify_fails_a_program_that_stores_to_the_uart_while_busy() {
        // test_regs printing through uart_stubs.kla routines that no longer
        // poll TX_BUSY: the first byte goes out (the transmitter is idle at
        // program start), but on the board each byte stored while the
        // previous one is still going out is lost.
        let dir = tempfile::tempdir().expect("tempdir");
        let stubs = fs::read_to_string(fixture_dir().join("uart_stubs.kla")).expect("read stubs");
        let no_poll: Vec<&str> = stubs.lines().filter(|line| !line.starts_with("CALL TX_WAIT_READY:")).collect();
        let no_poll = no_poll.join("\n");
        assert_ne!(no_poll.len(), stubs.len(), "the stubs poll before each byte");
        fs::write(dir.path().join("uart_stubs.kla"), no_poll).expect("write stubs");
        let file = dir.path().join("test_regs.kla");
        fs::copy(fixture_dir().join("test_regs.kla"), &file).expect("copy test_regs");

        let (isa, macro_list) = v2_tables();
        let verdict = verify_emulated(&file.to_string_lossy(), &isa, &macro_list, emu::DEFAULT_MAX_INSTRUCTIONS);
        let Ok(Verdict::Fail { expected: 6, diff, .. }) = &verdict else {
            panic!("{verdict:?}");
        };
        assert!(diff.starts_with("UART TX write while busy at PC 0x"), "{diff}");
        assert!(diff.ends_with("more); would lose output on hardware"), "{diff}");
    }

    #[test]
    fn tx_busy_reason_names_the_first_store_and_counts_the_rest() {
        let write = emu::TxBusyWrite { pc: 0x30, byte: b'H' };
        let one = emu::TxBusyWrites {
            first: vec![write],
            total: 1,
        };
        assert_eq!(
            tx_busy_reason(&one),
            "UART TX write while busy at PC 0x00000030 (byte 0x48 dropped); would lose output on hardware"
        );
        let three = emu::TxBusyWrites {
            first: vec![write],
            total: 3,
        };
        assert!(tx_busy_reason(&three).contains("dropped) (+2 more);"));
    }

    #[test]
    fn verify_skips_a_file_without_expected_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("plain.kla");
        fs::write(&file, "// no expected values here\nHALT\n").expect("write fixture");
        let (isa, macro_list) = v2_tables();
        let verdict = verify_emulated(&file.to_string_lossy(), &isa, &macro_list, emu::DEFAULT_MAX_INSTRUCTIONS);
        assert_eq!(verdict, Ok(Verdict::NotValidatable));
    }

    #[test]
    fn verify_fails_an_unreadable_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("missing.kla");
        let (isa, macro_list) = v2_tables();
        let verdict = verify_emulated(&file.to_string_lossy(), &isa, &macro_list, emu::DEFAULT_MAX_INSTRUCTIONS);
        assert!(matches!(&verdict, Err(reason) if reason.starts_with("cannot read: ")), "{verdict:?}");
    }

    #[test]
    fn verify_fails_a_file_that_does_not_assemble() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("bad.kla");
        fs::write(&file, "//   00000001  (never printed)\nNOTANOPCODE A\nHALT\n").expect("write fixture");
        let (isa, macro_list) = v2_tables();
        let verdict = verify_emulated(&file.to_string_lossy(), &isa, &macro_list, emu::DEFAULT_MAX_INSTRUCTIONS);
        assert_eq!(verdict, Err("assembly error".to_owned()));
    }
}
