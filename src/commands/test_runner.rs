//! Running tests on the board and checking their UART output against the
//! values in their `// ` comments: one program (`-T`) or a test list (`-L`).

use super::{send_failed, write_binary_file};
use crate::asm::assemble;
use crate::asm::macros;
use crate::diag::{Diagnostics, print_messages};
use crate::error::Error;
use crate::isa::Isa;
use crate::link::serial::{AwaitAck, ResetMethod, SerialTarget, TestResult, parse_expected_uart_values, run_test_monitor, write_to_board};
use crate::output::filename_stem;
use crate::output::listing::write_code_output_file;
use std::fs;
use std::path::Path;
use std::time::Duration;

/// Result of a single test in a batch run.
struct BatchTestResult {
    /// File name of the test.
    file_name: String,
    /// Whether the test ran and how it went, and if not, why.
    outcome: Outcome,
}

/// Whether a test in a batch ran.
enum Outcome {
    /// The program ran on the board, with this result.
    Ran(TestResult),
    /// Not run, for the given reason (no expected values, assembly or serial error).
    Skipped(String),
    /// Counted as a failure without counts: the file cannot be read, or the
    /// serial port failed while the test was reading UART output.
    Failed(String),
}

/// Run test verification mode.
///
/// Sends program to board, reads UART output, and verifies against expected values
/// parsed from the source file comments.
#[cfg(not(tarpaulin_include))] // Cannot test serial hardware in tarpaulin
pub(crate) fn run_test_mode(
    msg_list: &mut Diagnostics,
    bin_string: &str,
    port: SerialTarget<'_>,
    input_file_name: &Path,
    test_timeout: Duration,
    reset: ResetMethod,
) -> Result<(), Error> {
    // Parse expected values from the raw source file
    let raw_lines: Vec<String> = match fs::read_to_string(input_file_name) {
        Ok(source) => source.lines().map(String::from).collect(),
        Err(e) => {
            msg_list.error(format!("Cannot read {}: {e}", input_file_name.display()));
            return Err(Error::Read {
                path: input_file_name.to_path_buf(),
                source: e,
            });
        }
    };
    let expected_values = parse_expected_uart_values(&raw_lines);

    if expected_values.is_empty() {
        msg_list.error("No expected UART values found in source file comments");
        return Err(Error::NoTests(input_file_name.to_path_buf()));
    }

    let expected_count = expected_values.len();
    msg_list.info(format!("Found {expected_count} expected UART values in source comments"));

    // Send to board and keep port open
    let open = match write_to_board(bin_string, port, reset, AwaitAck::Wait, msg_list) {
        Ok(open) => {
            msg_list.info("Wrote to serial port");
            open
        }
        Err(err) => return Err(send_failed(port, err, msg_list)),
    };

    msg_list.print_summary();

    // Run test verification.  A serial read error is a failure of its own
    // (exit 1), not a timeout (exit 3); run_test_monitor has reported it.
    let result = run_test_monitor(open, &expected_values, test_timeout, msg_list).map_err(|err| Error::SerialRead {
        port: port.to_string(),
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

/// Assemble one file of a `-L` batch for the board.
///
/// Writes its `.code` listing next to it (also for a build with errors), and
/// returns the `.kbt` wire string, or `None` when the file cannot be read, the
/// listing cannot be written or the build has errors.
#[cfg(not(tarpaulin_include))]
fn assemble_to_kbt(input_file_name: &str, isa: &Isa, macro_list: &[macros::Macro], msg_list: &mut Diagnostics) -> Option<String> {
    let assembly = assemble(input_file_name, isa, macro_list, msg_list).ok()?;

    let output_file_name = format!("{}.code", filename_stem(input_file_name));
    if let Err(result_err) = write_code_output_file(&output_file_name, &assembly.lines, msg_list) {
        msg_list.error(format!("Unable to write to code file {output_file_name}, error {result_err}"));
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
pub(super) fn read_test_list(list_file: &Path, msg_list: &mut Diagnostics) -> Vec<String> {
    let list_dir = list_file.parent().unwrap_or_else(|| Path::new("."));

    let contents = match fs::read_to_string(list_file) {
        Ok(c) => c,
        Err(err) => {
            msg_list.error(format!("Error reading test list file {}: \"{err}\"", list_file.display()));
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
pub(crate) fn run_test_list(
    isa: &Isa,
    macro_list: &[macros::Macro],
    list_file: &Path,
    port: SerialTarget<'_>,
    test_timeout: Duration,
    reset: ResetMethod,
    msg_list: &mut Diagnostics,
) -> Result<(), Error> {
    let test_files = read_test_list(list_file, msg_list);
    if test_files.is_empty() {
        msg_list.error("No test files found in test list");
        return Err(Error::NoTests(list_file.to_path_buf()));
    }

    println!("Running {} tests from {}...\n", test_files.len(), list_file.display());

    let mut results: Vec<BatchTestResult> = Vec::new();

    for (index, test_file) in test_files.iter().enumerate() {
        println!("--- [{}/{}] {} ---", index + 1, test_files.len(), test_file);

        // Fresh message list for each test to avoid error accumulation
        let mut test_msg_list = Diagnostics::new();

        // A listed file that cannot be read is a failure, not a skip.
        let source = match fs::read_to_string(test_file) {
            Ok(source) => source,
            Err(err) => {
                println!("  FAIL: cannot read: {err}");
                results.push(BatchTestResult {
                    file_name: test_file.clone(),
                    outcome: Outcome::Failed(format!("cannot read: {err}")),
                });
                println!();
                continue;
            }
        };

        // Assemble the test file
        let Some(bin_string) = assemble_to_kbt(test_file, isa, macro_list, &mut test_msg_list) else {
            println!("  SKIP: assembly failed");
            print_messages(&test_msg_list);
            results.push(BatchTestResult {
                file_name: test_file.clone(),
                outcome: Outcome::Skipped("assembly error".to_owned()),
            });
            println!();
            continue;
        };

        // Write binary file.  It is only a by-product here (the image is sent
        // from memory), so a failed write is shown but does not fail the test.
        let binary_file_name = format!("{}.kbt", filename_stem(test_file));
        if let Err(err) = write_binary_file(&mut test_msg_list, Path::new(&binary_file_name), &bin_string) {
            println!("  WARNING: {err}");
        }

        // Parse expected values
        let raw_lines: Vec<String> = source.lines().map(String::from).collect();
        let expected_values = parse_expected_uart_values(&raw_lines);

        if expected_values.is_empty() {
            println!("  SKIP: no expected UART values in source comments");
            results.push(BatchTestResult {
                file_name: test_file.clone(),
                outcome: Outcome::Skipped("no expected values".to_owned()),
            });
            println!();
            continue;
        }

        // Send to board and keep port open
        let open = match write_to_board(&bin_string, port, reset, AwaitAck::Wait, &mut test_msg_list) {
            Ok(open) => open,
            Err(err) => {
                println!("  SKIP: serial port error \"{err}\"");
                results.push(BatchTestResult {
                    file_name: test_file.clone(),
                    outcome: Outcome::Skipped(format!("serial error: {err}")),
                });
                println!();
                continue;
            }
        };

        // Run test verification.  A serial read error fails the test (it is not a timeout).
        let result = match run_test_monitor(open, &expected_values, test_timeout, &mut test_msg_list) {
            Ok(result) => result,
            Err(err) => {
                println!("  FAIL: serial read error \"{err}\"");
                results.push(BatchTestResult {
                    file_name: test_file.clone(),
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
            outcome: Outcome::Ran(result),
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
fn report_batch(results: &[BatchTestResult], list_file: &Path) -> Result<(), Error> {
    println!("=== Test Suite Summary ===");
    let mut total_failed: usize = 0;
    let mut total_skipped: usize = 0;
    let mut total_timed_out: usize = 0;
    let mut files_all_pass: usize = 0;

    for r in results {
        match &r.outcome {
            Outcome::Skipped(reason) => {
                total_skipped += 1;
                println!("  SKIP  {} ({reason})", r.file_name);
            }
            Outcome::Failed(reason) => {
                total_failed += 1;
                println!("  FAIL  {} ({reason})", r.file_name);
            }
            Outcome::Ran(result) if result.failed > 0 || result.timed_out => {
                total_failed += 1;
                if result.timed_out {
                    total_timed_out += 1;
                }
                println!(
                    "  FAIL  {} ({}/{} passed{})",
                    r.file_name,
                    result.passed,
                    result.total,
                    if result.timed_out { ", timed out" } else { "" }
                );
            }
            Outcome::Ran(result) => {
                files_all_pass += 1;
                println!("  PASS  {} ({}/{})", r.file_name, result.passed, result.total);
            }
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
        Err(Error::NoTests(list_file.to_path_buf()))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A batch entry for a test that ran, with the given counts.
    fn ran(passed: usize, failed: usize, timed_out: bool) -> BatchTestResult {
        batch_result(Outcome::Ran(TestResult {
            passed,
            failed,
            timed_out,
            total: passed + failed,
        }))
    }

    /// A batch entry with the given outcome.
    fn batch_result(outcome: Outcome) -> BatchTestResult {
        BatchTestResult {
            file_name: "t.kla".to_owned(),
            outcome,
        }
    }

    // ---- report_batch --------------------------------------------------------

    #[test]
    fn batch_with_all_tests_passing_succeeds() {
        let results = [ran(3, 0, false), batch_result(Outcome::Skipped("x".to_owned()))];
        assert!(report_batch(&results, Path::new("list.txt")).is_ok());
    }

    #[test]
    fn batch_with_a_failed_or_timed_out_test_exits_2() {
        for failing in [ran(1, 1, false), ran(1, 0, true)] {
            let results = [ran(3, 0, false), failing];
            let err = report_batch(&results, Path::new("list.txt")).unwrap_err();
            assert!(matches!(err, Error::TestsFailed { failed: 1, total: 2, .. }), "{err:?}");
            assert_eq!(err.code(), 2);
        }
    }

    #[test]
    fn batch_with_an_unreadable_file_fails() {
        let results = [ran(3, 0, false), batch_result(Outcome::Failed("cannot read".to_owned()))];
        let err = report_batch(&results, Path::new("list.txt")).unwrap_err();
        assert_eq!(err.code(), 2);
    }

    #[test]
    fn batch_with_nothing_validated_fails() {
        let results = [batch_result(Outcome::Skipped("no expected values".to_owned()))];
        let err = report_batch(&results, Path::new("list.txt")).unwrap_err();
        assert!(matches!(err, Error::NoTests(_)), "{err:?}");
        assert_eq!(err.code(), 1);
    }
}
