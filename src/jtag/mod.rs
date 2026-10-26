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
pub(crate) use ftdi::Jtag;
pub(crate) use mpsse::DmiReq;

/// A link to the debug module's DMI: what the board debugger needs from the
/// JTAG master ([`Jtag`](ftdi::Jtag) with the `board` feature), so it can
/// also run against a model of the debug module in tests.
pub(crate) trait Dmi {
    /// Run DMI requests back to back and return each request's data (reads;
    /// writes give 0).  Fails the whole batch if any request fails.
    fn dmi_batch(&mut self, reqs: &[DmiReq]) -> Result<Vec<u64>>;

    /// DMI register read.
    fn dmi_read(&mut self, addr: u8) -> Result<u64> {
        Ok(self.dmi_batch(&[DmiReq::Read(addr)])?[0])
    }

    /// DMI register write.
    fn dmi_write(&mut self, addr: u8, data: u64) -> Result<()> {
        self.dmi_batch(&[DmiReq::Write(addr, data)]).map(drop)
    }
}

/// A JTAG / DMI error.
#[derive(Debug)]
#[cfg_attr(
    not(feature = "board"),
    allow(dead_code, reason = "without the board feature only the MPSSE tests build these")
)]
pub(crate) enum JtagError {
    /// libftdi could not allocate a context.
    NoContext,
    /// The board's USB-JTAG could not be opened; libftdi's description.
    Open(String),
    /// A libftdi call failed.
    Ftdi {
        /// What was being done (`"write"`, `"usb reset"`, ...).
        op: &'static str,
        /// libftdi's description of the error.
        message: String,
    },
    /// The cable did not send back the bytes a scan should produce.
    ReadTimeout,
    /// The debug module had not answered the request for `addr` within the
    /// idle clocks (and dropped the request after it).
    DmiBusy {
        /// The DMI register asked for.
        addr: u8,
    },
    /// The answer was for another register: the scan did not reach a debug module.
    DmiMismatch {
        /// The DMI register asked for.
        asked: u8,
        /// The register the answer is for.
        got: u8,
    },
}

impl fmt::Display for JtagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoContext => f.write_str("ftdi_new failed"),
            Self::Open(message) => write!(
                f,
                "cannot open the board's USB-JTAG (FT2232H 0403:6010 channel A): {message} \
                 (is the board connected, and Vivado's hardware manager / hw_server closed?)"
            ),
            Self::Ftdi { op, message } => write!(f, "ftdi {op}: {message}"),
            Self::ReadTimeout => f.write_str("ftdi read timeout"),
            Self::DmiBusy { addr } => write!(f, "DMI busy answering 0x{addr:02X} (raise the idle clocks, or lower --jtag-khz)"),
            Self::DmiMismatch { asked, got } => {
                write!(
                    f,
                    "DMI answer for 0x{got:02X}, asked 0x{asked:02X} (is the bitstream built with DEBUG_EN=1?)"
                )
            }
        }
    }
}

impl std::error::Error for JtagError {}

impl JtagError {
    /// True if the link to the debug module is gone or never worked (the
    /// cable failed, or the scans reach no debug module), as opposed to a
    /// busy debug module, which may answer a retry.
    pub(crate) const fn is_link_lost(&self) -> bool {
        !matches!(self, Self::DmiBusy { .. })
    }
}

/// JTAG result.
pub(crate) type Result<T> = std::result::Result<T, JtagError>;
