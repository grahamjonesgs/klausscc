//! Checking a test program's UART output against its expected values.

use crate::diag::Diagnostics;
use serialport::SerialPort;
use std::fmt;
use std::io;
use std::time::{Duration, Instant};

/// How long each read of the board's UART waits, so the deadline is checked.
const TEST_READ_TIMEOUT: Duration = Duration::from_millis(500);

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

/// The value a line of UART output (or of an expected-value comment) holds:
/// its leading 8 upper-case hex digits, once trimmed.
///
/// This is the one rule for test values, shared by the board and emulator
/// test runners and the expected values in a test's comments.
pub(crate) fn hex_token(line: &str) -> Option<&str> {
    let candidate = line.trim().get(..8)?;
    candidate.bytes().all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'F')).then_some(candidate)
}

/// Parse expected UART hex values from source file comment headers.
///
/// Extracts the [`hex_token`] of each `//` comment that starts with one,
/// as in `//   XXXXXXXX  (what it checks)`.
pub(crate) fn parse_expected_uart_values(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| line.trim().strip_prefix("//"))
        .filter_map(hex_token)
        .map(str::to_owned)
        .collect()
}

/// One expected value checked against a line of UART output.
#[derive(Debug, PartialEq, Eq)]
struct Check<'a> {
    /// Which expected value, counting from 1.
    number: usize,
    /// How many values are expected.
    total: usize,
    /// The value the board sent.
    got: String,
    /// The value expected.
    expected: &'a str,
}

impl Check<'_> {
    /// Whether the board sent the expected value.
    fn passed(&self) -> bool {
        self.got == self.expected
    }
}

impl fmt::Display for Check<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            number,
            total,
            got,
            expected,
        } = self;
        if self.passed() {
            write!(f, "  PASS [{number}/{total}]: {got} == {expected}")
        } else {
            write!(f, "  FAIL [{number}/{total}]: got {got}, expected {expected}")
        }
    }
}

/// Matches the lines of a test's UART output against its expected values, in order.
///
/// Fed the bytes as they arrive; each complete line that holds a
/// [`hex_token`] is checked against the next expected value.  Blank lines
/// and the loader's acknowledgement (`... Complete`) are skipped.
struct UartMatcher<'a> {
    /// The values expected, in order.
    expected: &'a [String],
    /// Bytes received after the last complete line.
    partial: Vec<u8>,
    /// How many expected values have been checked.
    checked: usize,
    /// How many of them matched.
    passed: usize,
}

impl<'a> UartMatcher<'a> {
    /// A matcher for `expected`, with nothing received yet.
    const fn new(expected: &'a [String]) -> Self {
        Self {
            expected,
            partial: Vec::new(),
            checked: 0,
            passed: 0,
        }
    }

    /// Whether every expected value has been checked.
    fn is_done(&self) -> bool {
        self.checked >= self.expected.len()
    }

    /// Take `bytes` received from the board, and return the checks of the
    /// lines they complete.
    fn feed(&mut self, bytes: &[u8]) -> Vec<Check<'a>> {
        self.partial.extend_from_slice(bytes);
        let mut checks = Vec::new();
        let mut consumed = 0;
        while let Some(len) = self.partial.get(consumed..).and_then(|rest| rest.iter().position(|&b| b == b'\n')) {
            let line = String::from_utf8_lossy(&self.partial[consumed..consumed + len]).into_owned();
            consumed += len + 1;
            checks.extend(self.check_line(&line));
        }
        self.partial.drain(..consumed);
        checks
    }

    /// Check one complete line against the next expected value.
    fn check_line(&mut self, line: &str) -> Option<Check<'a>> {
        let line = line.trim();
        if line.is_empty() || line.contains("Complete") {
            return None;
        }
        let got = hex_token(line)?;
        let expected = self.expected.get(self.checked)?;
        self.checked += 1;
        let check = Check {
            number: self.checked,
            total: self.expected.len(),
            got: got.to_owned(),
            expected,
        };
        if check.passed() {
            self.passed += 1;
        }
        Some(check)
    }

    /// The result so far; values not yet checked count as timed out.
    fn result(&self) -> TestResult {
        TestResult {
            passed: self.passed,
            failed: self.checked - self.passed,
            timed_out: !self.is_done(),
            total: self.expected.len(),
        }
    }
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
    timeout: Duration,
    msg_list: &mut Diagnostics,
) -> io::Result<TestResult> {
    port.set_timeout(TEST_READ_TIMEOUT).unwrap_or(());
    let timeout_secs = timeout.as_secs();

    println!("Test mode: expecting {} UART values (timeout {timeout_secs}s)...", expected_values.len());

    let result = match_uart_values(&mut port, expected_values, timeout);
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
    let mut matcher = UartMatcher::new(expected_values);
    let mut buffer = [0_u8; 1024];

    while !matcher.is_done() && start.elapsed() < timeout {
        match reader.read(&mut buffer[..]) {
            Ok(bytes_read) => {
                for check in matcher.feed(buffer.get(..bytes_read).unwrap_or_default()) {
                    println!("{check}");
                }
            }
            Err(err) if err.kind() == io::ErrorKind::TimedOut => {}
            Err(err) => return Err(err),
        }
    }
    Ok(matcher.result())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_hex_token_valid() {
        assert_eq!(hex_token("00000080"), Some("00000080"));
        assert_eq!(hex_token("FF000000"), Some("FF000000"));
        assert_eq!(hex_token("  0000ABCD  "), Some("0000ABCD"));
        assert_eq!(hex_token("0000ABCD  (what it checks)"), Some("0000ABCD"));
    }

    #[test]
    fn test_hex_token_invalid() {
        assert_eq!(hex_token(""), None);
        assert_eq!(hex_token("0x05"), None);
        assert_eq!(hex_token("ZZZZZZZZ"), None);
        assert_eq!(hex_token("0000abc"), None); // too short
        assert_eq!(hex_token("0000ABC\u{e9}"), None); // not a char boundary at 8
        assert_eq!(hex_token("+0000001"), None);
    }

    #[test]
    fn test_hex_token_lowercase_rejected() {
        assert_eq!(hex_token("0000abcd"), None);
        assert_eq!(hex_token("0000ABCd"), None);
    }

    #[test]
    // Test parsing expected UART values from typical test file header
    fn test_parse_expected_uart_values1() {
        let lines = vec![
            "// Test 05: Bit Manipulation".to_owned(),
            "// Expected UART output:".to_owned(),
            "//   00000080  (BSET: set bit 7)".to_owned(),
            "//   000000FF  (result)".to_owned(),
            "//   FF000000  (BITREV)".to_owned(),
            "// Ends with HALT".to_owned(),
            "_start".to_owned(),
            "SETR A 0x0".to_owned(),
        ];
        let result = parse_expected_uart_values(&lines);
        assert_eq!(result, vec!["00000080", "000000FF", "FF000000"]);
    }

    #[test]
    // Test parsing returns empty vec when no expected values
    fn test_parse_expected_uart_values2() {
        let lines = vec!["// This is a comment".to_owned(), "// No hex values here".to_owned(), "_start".to_owned()];
        let result = parse_expected_uart_values(&lines);
        assert_eq!(result, Vec::<String>::new());
    }

    #[test]
    // Test parsing ignores non-comment lines and short hex values
    fn test_parse_expected_uart_values3() {
        let lines = vec![
            "00000080".to_owned(),                   // not a comment
            "// 0x05".to_owned(),                    // too short
            "//   ZZZZZZZZ  (invalid)".to_owned(),   // not hex
            "//   0000abcd  (lowercase)".to_owned(), // lowercase hex - should not match
            "//   0000ABCD  (uppercase)".to_owned(), // valid
            "  //00000001".to_owned(),               // indented, no space after //
        ];
        let result = parse_expected_uart_values(&lines);
        assert_eq!(result, vec!["0000ABCD", "00000001"]);
    }

    #[test]
    // Test parsing with empty input
    fn test_parse_expected_uart_values4() {
        let lines: Vec<String> = Vec::new();
        let result = parse_expected_uart_values(&lines);
        assert_eq!(result, Vec::<String>::new());
    }

    // ---- UartMatcher -----------------------------------------------------------

    #[test]
    fn matcher_checks_complete_lines_in_order() {
        let expected = expected(&["00000001", "0000000A", "00000003"]);
        let mut matcher = UartMatcher::new(&expected);
        assert_eq!(matcher.feed(b"Load Complete\r\n\r\n0000"), []);
        let checks = matcher.feed(b"0001\r\nnoise\n0000000B\n000");
        assert_eq!(checks.iter().map(Check::passed).collect::<Vec<_>>(), [true, false]);
        assert_eq!(checks[0].to_string(), "  PASS [1/3]: 00000001 == 00000001");
        assert_eq!(checks[1].to_string(), "  FAIL [2/3]: got 0000000B, expected 0000000A");
        assert!(!matcher.is_done());
        assert_eq!(
            matcher.result(),
            TestResult {
                passed: 1,
                failed: 1,
                timed_out: true,
                total: 3,
            }
        );
        let checks = matcher.feed(b"00003\n00000004\n");
        assert_eq!(checks.len(), 1, "values after the last expected one are ignored");
        assert!(matcher.is_done());
        assert_eq!(
            matcher.result(),
            TestResult {
                passed: 2,
                failed: 1,
                timed_out: false,
                total: 3,
            }
        );
    }

    #[test]
    fn matcher_with_nothing_expected_is_done() {
        let matcher = UartMatcher::new(&[]);
        assert!(matcher.is_done());
        assert_eq!(matcher.result(), TestResult::default());
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
