//! Checking a test program's UART output against its expected values.

use crate::diag::Diagnostics;
use serialport::SerialPort;
use std::io;
use std::time::{Duration, Instant};

/// Result of a test verification run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TestResult {
    /// Number of expected values that matched.
    pub passed: usize,
    /// Number of expected values that did not match.
    pub failed: usize,
    /// True if the test timed out before all expected values were received.
    pub timed_out: bool,
    /// Total number of expected values.
    pub total: usize,
}

/// Extract an 8-digit uppercase hex value from a UART line.
///
/// Returns Some if the trimmed line starts with exactly 8 uppercase hex digits.
fn extract_hex_value(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.len() >= 8 {
        let candidate = trimmed.get(..8).unwrap_or("");
        if candidate.len() == 8 && candidate.chars().all(|ch| ch.is_ascii_hexdigit()) && candidate == candidate.to_ascii_uppercase() {
            return Some(candidate.to_owned());
        }
    }
    None
}

/// Run test verification by monitoring serial output against expected values.
///
/// Reads UART output from the FPGA board and compares each hex line against the
/// expected values in order. Stops when all expected values are matched or timeout.
/// A serial read error (other than a read timeout) is reported and returned:
/// it is not a test timeout.
#[cfg(not(tarpaulin_include))] // Cannot test serial hardware in tarpaulin
pub(crate) fn run_test_monitor(
    mut port: Box<dyn SerialPort>,
    expected_values: &[String],
    timeout_secs: u64,
    msg_list: &mut Diagnostics,
) -> io::Result<TestResult> {
    port.set_timeout(Duration::from_millis(500)).unwrap_or(());

    println!(
        "Test mode: expecting {} UART values (timeout {}s)...",
        expected_values.len(),
        timeout_secs
    );

    let result = match_uart_values(&mut port, expected_values, Duration::from_secs(timeout_secs));
    match &result {
        Ok(result) if result.timed_out => {
            let remaining = result.total - result.passed - result.failed;
            msg_list.warning(format!("Test timed out after {timeout_secs}s: {remaining} expected values not received"));
        }
        Ok(_) => {}
        Err(err) => {
            msg_list.error(format!("Serial read error during test: \"{err}\""));
        }
    }
    result
}

/// Compare the hex lines read from `reader` against `expected_values`, in order.
///
/// Prints a PASS/FAIL line per value.  Stops when every expected value has
/// been checked or `timeout` has passed (`timed_out` is then set); a read
/// error other than a read timeout stops it and is returned.
fn match_uart_values<R: io::Read + ?Sized>(reader: &mut R, expected_values: &[String], timeout: Duration) -> io::Result<TestResult> {
    let start = Instant::now();
    let total = expected_values.len();
    let mut buffer = [0_u8; 1024];
    let mut line_buffer = String::new();
    let mut expected_index: usize = 0;
    let mut passed: usize = 0;
    let mut failed: usize = 0;

    while expected_index < total && start.elapsed() < timeout {
        match reader.read(&mut buffer[..]) {
            Ok(bytes_read) => {
                line_buffer.push_str(&String::from_utf8_lossy(buffer.get(..bytes_read).unwrap_or(&[])));
            }
            Err(err) if err.kind() == io::ErrorKind::TimedOut => continue,
            Err(err) => return Err(err),
        }

        // Process complete lines from the buffer
        while let Some(newline_pos) = line_buffer.find('\n') {
            let line: String = line_buffer.drain(..=newline_pos).collect();
            let trimmed = line.trim();

            // Skip blank lines and board acknowledgment messages
            if trimmed.is_empty() || trimmed.contains("Complete") {
                continue;
            }

            let Some(hex_value) = extract_hex_value(trimmed) else {
                continue;
            };
            let Some(expected) = expected_values.get(expected_index) else {
                continue;
            };
            if hex_value == *expected {
                println!("  PASS [{}/{total}]: {hex_value} == {expected}", expected_index + 1);
                passed += 1;
            } else {
                println!("  FAIL [{}/{total}]: got {hex_value}, expected {expected}", expected_index + 1);
                failed += 1;
            }
            expected_index += 1;
        }
    }

    Ok(TestResult {
        passed,
        failed,
        timed_out: expected_index < total,
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_extract_hex_value_valid() {
        assert_eq!(extract_hex_value("00000080"), Some("00000080".to_owned()));
        assert_eq!(extract_hex_value("FF000000"), Some("FF000000".to_owned()));
        assert_eq!(extract_hex_value("  0000ABCD  "), Some("0000ABCD".to_owned()));
    }

    #[test]
    fn test_extract_hex_value_invalid() {
        assert_eq!(extract_hex_value(""), None);
        assert_eq!(extract_hex_value("0x05"), None);
        assert_eq!(extract_hex_value("ZZZZZZZZ"), None);
        assert_eq!(extract_hex_value("0000abc"), None); // too short
    }

    #[test]
    fn test_extract_hex_value_lowercase_rejected() {
        assert_eq!(extract_hex_value("0000abcd"), None);
    }

    // ---- match_uart_values ---------------------------------------------------

    /// A reader that returns scripted chunks, then read timeouts.
    struct Script(std::collections::VecDeque<io::Result<Vec<u8>>>);

    impl Script {
        fn new(chunks: Vec<io::Result<&[u8]>>) -> Self {
            Self(chunks.into_iter().map(|c| c.map(<[u8]>::to_vec)).collect())
        }
    }

    impl io::Read for Script {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.0.pop_front() {
                Some(Ok(chunk)) => {
                    buf[..chunk.len()].copy_from_slice(&chunk);
                    Ok(chunk.len())
                }
                Some(Err(err)) => Err(err),
                None => {
                    thread::sleep(Duration::from_millis(1));
                    Err(io::Error::from(io::ErrorKind::TimedOut))
                }
            }
        }
    }

    fn expected(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn uart_values_are_matched_across_chunks() {
        let mut reader = Script::new(vec![
            Ok(b"Load Complete\r\n0000"),
            Err(io::Error::from(io::ErrorKind::TimedOut)),
            Ok(b"0001\r\n\r\n"),
            Ok(b"0000000B\n"),
        ]);
        let result = match_uart_values(&mut reader, &expected(&["00000001", "0000000A"]), Duration::from_secs(5)).unwrap();
        assert_eq!(
            result,
            TestResult {
                passed: 1,
                failed: 1,
                timed_out: false,
                total: 2,
            }
        );
    }

    #[test]
    fn uart_values_missing_at_the_deadline_time_out() {
        let mut reader = Script::new(vec![Ok(b"00000001\n")]);
        let result = match_uart_values(&mut reader, &expected(&["00000001", "00000002"]), Duration::from_millis(20)).unwrap();
        assert_eq!(
            result,
            TestResult {
                passed: 1,
                failed: 0,
                timed_out: true,
                total: 2,
            }
        );
    }

    #[test]
    fn a_serial_read_error_is_returned_not_reported_as_a_timeout() {
        let mut reader = Script::new(vec![Ok(b"00000001\n"), Err(io::Error::from(io::ErrorKind::BrokenPipe))]);
        let err = match_uart_values(&mut reader, &expected(&["00000001", "00000002"]), Duration::from_secs(5)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }
}
