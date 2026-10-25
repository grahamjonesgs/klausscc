//! Crate error type for operational failures, and its mapping to the exit status.
//!
//! Problems in the user's source (bad operands, undefined labels, ...) are
//! diagnostics: they are collected in a [`Diagnostics`](crate::diag::Diagnostics)
//! so one run can report all of them.  [`Error`] covers the failures that stop
//! a command.  By convention a handler reports the failure to the user (to the
//! live `Diagnostics`, or on stderr) before returning the `Error`, so `main` only
//! has to turn it into an exit code.

use crate::netload::NetbootError;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

/// Exit status for any failure other than a failed or timed-out test.
pub(crate) const EXIT_FAILURE: u8 = 1;
/// Exit status when one or more tests produced the wrong UART output.
pub(crate) const EXIT_TESTS_FAILED: u8 = 2;
/// Exit status when a test timed out waiting for UART output.
pub(crate) const EXIT_TEST_TIMEOUT: u8 = 3;

/// Why a command failed.
#[derive(Debug)]
pub(crate) enum Error {
    /// Assembly (or building the board image) reported this many errors.
    Assembly(usize),
    /// A file could not be read.
    Read {
        /// The file that could not be read.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// A file could not be written.
    Write {
        /// The file that could not be written.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// An ELF file had no loadable segments.
    Elf(PathBuf),
    /// Sending to the board over its serial port failed.
    Serial {
        /// The serial port name as given (or auto-detected).
        port: String,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// Reading the board's UART output over its serial port failed during a test.
    SerialRead {
        /// The serial port name as given (or auto-detected).
        port: String,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// Loading the board over the network failed.
    Netboot(NetbootError),
    /// Tests ran, and some of them failed or timed out.
    TestsFailed {
        /// Number of failed tests (or failed expected values, for a single test).
        failed: usize,
        /// Number of tests (or expected values) checked.
        total: usize,
        /// True if a test gave up waiting for UART output.
        timed_out: bool,
    },
    /// A test run found nothing it could validate.
    NoTests(PathBuf),
    /// The command line asks for something that cannot be done.
    Usage(&'static str),
    /// Any other failure, already described in the message.
    Other(String),
}

impl Error {
    /// The process exit status for this error, as a plain number.
    ///
    /// `2` means tests failed and `3` means a test timed out; every other
    /// failure is `1`.
    #[must_use]
    pub(crate) const fn code(&self) -> u8 {
        match self {
            Self::TestsFailed { timed_out: true, .. } => EXIT_TEST_TIMEOUT,
            Self::TestsFailed { .. } => EXIT_TESTS_FAILED,
            _ => EXIT_FAILURE,
        }
    }

    /// The process exit status for this error.
    #[must_use]
    pub(crate) fn exit_code(&self) -> ExitCode {
        ExitCode::from(self.code())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Assembly(count) => write!(f, "completed with {count} error{}", if *count == 1 { "" } else { "s" }),
            Self::Read { path, source } => write!(f, "cannot read {}: {source}", path.display()),
            Self::Write { path, source } => write!(f, "cannot write {}: {source}", path.display()),
            Self::Elf(path) => write!(f, "failed to extract LOAD segments from ELF file {}", path.display()),
            Self::Serial { port, source } => write!(f, "failed to write to serial port {port}: {source}"),
            Self::SerialRead { port, source } => write!(f, "failed to read from serial port {port}: {source}"),
            Self::Netboot(err) => write!(f, "netboot failed: {err}"),
            Self::TestsFailed { failed, total, timed_out } => {
                write!(f, "{failed} of {total} tests failed{}", if *timed_out { " (timed out)" } else { "" })
            }
            Self::NoTests(path) => write!(f, "no tests with expected UART values found at {}", path.display()),
            Self::Usage(text) => f.write_str(text),
            Self::Other(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } | Self::Serial { source, .. } | Self::SerialRead { source, .. } => Some(source),
            Self::Netboot(err) => Some(err),
            _ => None,
        }
    }
}

impl From<NetbootError> for Error {
    fn from(err: NetbootError) -> Self {
        Self::Netboot(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_tests_exit_2() {
        let err = Error::TestsFailed {
            failed: 1,
            total: 3,
            timed_out: false,
        };
        assert_eq!(err.code(), 2);
        assert_eq!(err.exit_code(), ExitCode::from(2));
    }

    #[test]
    fn timed_out_tests_exit_3() {
        let err = Error::TestsFailed {
            failed: 0,
            total: 3,
            timed_out: true,
        };
        assert_eq!(err.code(), 3);
        assert_eq!(err.exit_code(), ExitCode::from(3));
    }

    #[test]
    fn other_errors_exit_1() {
        let errors = [
            Error::Assembly(4),
            Error::Read {
                path: PathBuf::from("a.kla"),
                source: io::Error::from(io::ErrorKind::NotFound),
            },
            Error::Write {
                path: PathBuf::from("a.kbt"),
                source: io::Error::from(io::ErrorKind::PermissionDenied),
            },
            Error::Elf(PathBuf::from("a.elf")),
            Error::Serial {
                port: "/dev/null".to_owned(),
                source: io::Error::from(io::ErrorKind::TimedOut),
            },
            Error::SerialRead {
                port: "/dev/null".to_owned(),
                source: io::Error::from(io::ErrorKind::BrokenPipe),
            },
            Error::Netboot(NetbootError::Rejected(1)),
            Error::NoTests(PathBuf::from("tests")),
            Error::Usage("needs -s"),
            Error::Other("gdbserver failed".to_owned()),
        ];
        for err in &errors {
            assert_eq!(err.code(), 1, "{err}");
            assert_eq!(err.exit_code(), ExitCode::FAILURE, "{err}");
        }
    }

    #[test]
    fn display_names_the_problem() {
        assert_eq!(Error::Assembly(1).to_string(), "completed with 1 error");
        assert_eq!(Error::Assembly(2).to_string(), "completed with 2 errors");
        let read = Error::Read {
            path: PathBuf::from("dir/my file.kla"),
            source: io::Error::other("gone"),
        };
        assert_eq!(read.to_string(), "cannot read dir/my file.kla: gone");
        assert_eq!(
            Error::Netboot(NetbootError::Rejected(7)).to_string(),
            "netboot failed: board rejected image (status 7)"
        );
    }

    #[test]
    fn io_errors_keep_their_source() {
        use std::error::Error as _;
        let err = Error::Write {
            path: PathBuf::from("x"),
            source: io::Error::other("disk full"),
        };
        assert_eq!(err.source().map(ToString::to_string).as_deref(), Some("disk full"));
        assert!(Error::Assembly(1).source().is_none());
    }
}
