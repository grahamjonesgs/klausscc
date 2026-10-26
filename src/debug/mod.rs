//! GDB remote-protocol servers for source-level debugging (`--gdbserver PORT`).
//!
//! lldb (the KlaussCPU-aware build from `klausscpu-llvm`, via `gdb-remote` or
//! VS Code's lldb-dap) is served over TCP with the `gdbstub` crate.  See
//! `klaussCPU/DEBUG_PLAN.md` (Phase 2).
//!
//! There are two targets with the same register layout, target description
//! and stop reporting ([`arch`]): `emu_target` serves the built-in ISA
//! emulator and `board` (feature `board`) the real board over its USB-JTAG.
//! What they share — waiting for the debugger to connect, and the errors
//! that end a session — is here.

pub(crate) mod arch;
#[cfg(feature = "board")]
pub(crate) mod board;
pub(crate) mod emu_target;

use std::fmt;
use std::io;
use std::net::{TcpListener, TcpStream};

/// Why a debugging session could not be served.
#[derive(Debug)]
pub(crate) enum GdbError {
    /// The server could not listen on its port.
    Listen {
        /// The TCP port asked for.
        port: u16,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// Waiting for the debugger to connect failed.
    Accept(io::Error),
    /// The session ended on an error: the protocol, the connection or the target.
    Session(String),
}

impl fmt::Display for GdbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Listen { port, source } => write!(f, "cannot listen on port {port}: {source}"),
            Self::Accept(source) => write!(f, "accept failed: {source}"),
            Self::Session(text) => write!(f, "gdbstub error: {text}"),
        }
    }
}

impl std::error::Error for GdbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Listen { source, .. } | Self::Accept(source) => Some(source),
            Self::Session(_) => None,
        }
    }
}

/// Listen on `127.0.0.1:port` and wait for one debugger to connect.
pub(crate) fn accept(port: u16) -> Result<TcpStream, GdbError> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|source| GdbError::Listen { port, source })?;
    eprintln!("klausscc gdbserver: waiting for a debugger on 127.0.0.1:{port}");
    let (stream, peer) = listener.accept().map_err(GdbError::Accept)?;
    eprintln!("klausscc gdbserver: debugger connected from {peer}");
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

/// The last address a `len`-byte access at `addr` touches (`addr` itself for
/// an empty access), or `None` if the access runs past `0xFFFF_FFFF`.
#[cfg(any(test, feature = "board"))]
pub(crate) fn access_last_addr(addr: u32, len: usize) -> Option<u32> {
    let span = u32::try_from(len.saturating_sub(1)).ok()?;
    addr.checked_add(span)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gdb_errors_name_the_problem() {
        use std::error::Error as _;
        let listen = GdbError::Listen {
            port: 1234,
            source: io::Error::from(io::ErrorKind::AddrInUse),
        };
        assert!(listen.to_string().starts_with("cannot listen on port 1234: "), "{listen}");
        assert!(listen.source().is_some());
        assert_eq!(GdbError::Session("bad packet".to_owned()).to_string(), "gdbstub error: bad packet");
    }

    #[test]
    fn access_last_addr_rejects_ranges_past_the_top() {
        assert_eq!(access_last_addr(0x1000, 0), Some(0x1000));
        assert_eq!(access_last_addr(0x1000, 1), Some(0x1000));
        assert_eq!(access_last_addr(0x1000, 4), Some(0x1003));
        assert_eq!(access_last_addr(0xFFFF_FFFC, 4), Some(0xFFFF_FFFF));
        assert_eq!(access_last_addr(0xFFFF_FFFD, 4), None);
        assert_eq!(access_last_addr(0xFFFF_FFFF, 2), None);
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(access_last_addr(0, 0x1_0000_0000), Some(0xFFFF_FFFF));
            assert_eq!(access_last_addr(0, 0x1_0000_0001), None);
        }
    }
}
