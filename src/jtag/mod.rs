//! JTAG over an FTDI FT2232H in MPSSE mode, and the KlaussCPU debug module's
//! DMI on top of it (`klaussCPU` `debug_module.sv`, `DEBUG_PLAN.md` §4).
//!
//! The Nexys A7's on-board USB-JTAG is a Digilent-programmed FT2232H: channel
//! A is JTAG (MPSSE), channel B the console UART (`/dev/ttyUSB1`).  Opening
//! channel A detaches only its kernel driver, so the UART keeps working.
//! Vivado's `hw_server` (hardware manager) holds the same channel while it
//! runs; close it first.
//!
//! * `mpsse`: the pure MPSSE command framing for scans and DMI requests.  It
//!   needs no hardware, so its tests (against a TAP model) run in every
//!   `cargo test`.
//! * `ftdi`: the libftdi1 binding and the `Jtag` master that drives it.  Built
//!   with the `board` cargo feature (links the system libftdi1).

use std::fmt;

#[cfg(feature = "board")]
mod ftdi;
mod mpsse;

#[cfg(feature = "board")]
pub use ftdi::Jtag;
#[cfg(feature = "board")]
pub use mpsse::DmiReq;

/// A JTAG / DMI error.
#[derive(Debug)]
pub struct JtagError(pub String);

impl fmt::Display for JtagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// JTAG result.
pub type Result<T> = std::result::Result<T, JtagError>;
