//! The `klausscc` command line tool; everything it does is in the library.

use std::process::ExitCode;

/// Main function for Klausscc: run the command and exit with its status.
#[cfg(not(tarpaulin_include))] // Cannot test main in tarpaulin
fn main() -> ExitCode {
    klausscc::run()
}
