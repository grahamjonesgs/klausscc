//! Running programs on the golden-model emulator: `--emulate` for one
//! program, `--emulate-test` to check a corpus against its expected UART
//! output.

use super::test_runner::read_test_list;
use crate::asm::{Assembly, assemble};
use crate::diag::{Diagnostics, MessageType};
use crate::error::Error;
use crate::files::filename_stem;
use crate::helper::{human_bytes, parse_expected_uart_values};
use crate::image::Image;
use crate::loader::{BinaryInput, Loaded, load_flat};
use crate::opcodes::Opcode;
use crate::{emulate, macros};
use std::fs;
use std::path::PathBuf;

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
/// Streams the per-instruction trace to `trace_file` (or, for `"-"`, stdout),
/// prints the summary, then the instruction count, the stop reason and the
/// captured UART output on stdout.
#[cfg(not(tarpaulin_include))]
fn emulate_and_report(image: &Image, trace_file: Option<&str>, max_instructions: u64, msg_list: &mut Diagnostics) -> Result<(), Error> {
    let mut trace_writer = open_trace_writer(trace_file, msg_list)?;
    let result = emulate::emulate_image_to_writer(
        &image.to_ddr(),
        image.entry,
        max_instructions,
        trace_writer.as_mut().map(|w| w.as_mut() as &mut dyn std::io::Write),
    );
    finish_trace_writer(trace_writer, trace_file, result.instructions, msg_list);

    msg_list.print_summary();
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
    oplist: &[Opcode],
    macro_list: &[macros::Macro],
    test_path: &str,
    max_instructions: u64,
    msg_list: &mut Diagnostics,
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
        msg_list.error(format!("No .kla files found for emulate-test at {test_path}"));
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
        msg_list.error(format!("No files with expected UART values found for emulate-test at {test_path}"));
        return Err(Error::NoTests(PathBuf::from(test_path)));
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
pub(crate) fn verify_emulated(file: &str, oplist: &[Opcode], macro_list: &[macros::Macro], max_instructions: u64) -> Result<Verdict, String> {
    // A file that cannot be read is a failure, not a silently skipped file.
    let source = fs::read_to_string(file).map_err(|e| format!("cannot read: {e}"))?;
    let raw_lines: Vec<String> = source.lines().map(String::from).collect();
    let expected = parse_expected_uart_values(&raw_lines);
    if expected.is_empty() {
        return Ok(Verdict::NotValidatable);
    }

    let mut test_msgs = Diagnostics::new();
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

#[cfg(test)]
mod tests {
    use super::*;

    // ---- verify_emulated -----------------------------------------------------

    /// Absolute path of the emulator fixture corpus.
    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    /// The built-in v2 opcode table and expanded macros, as the CLI loads them.
    fn v2_tables() -> (Vec<Opcode>, Vec<macros::Macro>) {
        let mut msg_list = Diagnostics::new();
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
