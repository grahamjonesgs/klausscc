//! GDB remote-protocol servers for source-level debugging (`--gdbserver PORT`).
//!
//! lldb (the KlaussCPU-aware build from `klausscpu-llvm`, via `gdb-remote` or
//! VS Code's lldb-dap) is served over TCP with the `gdbstub` crate.  See
//! `klaussCPU/DEBUG_PLAN.md` (Phase 2).
//!
//! There are two targets with the same register layout, target description
//! and stop reporting ([`arch`]): `emu_target` serves the built-in ISA
//! emulator and `board` (feature `board`) the real board over its USB-JTAG.
//! What they share — waiting for the debugger to connect, the event loop
//! that runs a `Session` target, the host and process info, the `mmio`
//! monitor commands and the errors that end a session — is here.

/// Implement `HostInfo` and `ProcessInfo` for a target, identically for
/// both: a little-endian [`TRIPLE`](arch::TRIPLE) with 4-byte pointers,
/// running as pid 1.
///
/// A generic target lists its generics in brackets first:
/// `impl_host_process_info!([D: Dmi] BoardTarget<D>)`.
macro_rules! impl_host_process_info {
    ([$($generics:tt)*] $target:ty) => {
        impl<$($generics)*> gdbstub::target::ext::host_info::HostInfo for $target {
            fn host_info(&self, write_item: &mut dyn FnMut(&gdbstub::target::ext::host_info::HostInfoResponse<'_>)) -> Result<(), Self::Error> {
                use gdbstub::common::Endianness;
                use gdbstub::target::ext::host_info::HostInfoResponse;
                write_item(&HostInfoResponse::Triple($crate::debug::arch::TRIPLE));
                write_item(&HostInfoResponse::Endianness(Endianness::Little));
                write_item(&HostInfoResponse::PointerSize(4));
                Ok(())
            }
        }

        impl<$($generics)*> gdbstub::target::ext::process_info::ProcessInfo for $target {
            fn process_info(
                &self,
                write_item: &mut dyn FnMut(&gdbstub::target::ext::process_info::ProcessInfoResponse<'_>),
            ) -> Result<(), Self::Error> {
                use gdbstub::common::{Endianness, Pid};
                use gdbstub::target::ext::process_info::ProcessInfoResponse;
                if let Some(pid) = Pid::new(1) {
                    write_item(&ProcessInfoResponse::Pid(pid));
                }
                write_item(&ProcessInfoResponse::Triple($crate::debug::arch::TRIPLE));
                write_item(&ProcessInfoResponse::Endianness(Endianness::Little));
                write_item(&ProcessInfoResponse::PointerSize(4));
                Ok(())
            }
        }
    };
    ($target:ty) => {
        impl_host_process_info!([] $target);
    };
}

pub(crate) mod arch;
#[cfg(any(test, feature = "board"))]
pub(crate) mod board;
pub(crate) mod emu_target;

use arch::KlaussCpuArch;
use gdbstub::conn::ConnectionExt;
use gdbstub::stub::{DisconnectReason, GdbStub, GdbStubError, SingleThreadStopReason, run_blocking};
use gdbstub::target::Target;
use gdbstub::target::ext::monitor_cmd::outputln;
use std::fmt;
use std::io;
use std::marker::PhantomData;
use std::net::{TcpListener, TcpStream};

/// What stopped a run, or debugger input waiting.
enum RunEvent {
    /// The debugger sent something (e.g. Ctrl-C) while the target ran.
    IncomingData,
    /// The target stopped.
    Stop(SingleThreadStopReason<u32>),
}

/// A target the shared [`EventLoop`] can run.
trait Session: Target<Arch = KlaussCpuArch> {
    /// Run the target until it stops, or until `incoming_data` reports that
    /// the debugger sent something.
    fn run_until_event(&mut self, incoming_data: impl FnMut() -> bool) -> Result<RunEvent, Self::Error>;

    /// Stop the target for a debugger interrupt (Ctrl-C), returning the stop
    /// to report.
    fn on_interrupt(&mut self) -> Result<Option<SingleThreadStopReason<u32>>, Self::Error>;
}

/// The blocking `gdbstub` event loop for a [`Session`] target over TCP.
struct EventLoop<T>(PhantomData<T>);

impl<T: Session> run_blocking::BlockingEventLoop for EventLoop<T> {
    type Target = T;
    type Connection = TcpStream;
    type StopReason = SingleThreadStopReason<u32>;

    fn wait_for_stop_reason(
        target: &mut T,
        conn: &mut TcpStream,
    ) -> Result<run_blocking::Event<Self::StopReason>, run_blocking::WaitForStopReasonError<T::Error, io::Error>> {
        match target.run_until_event(|| ConnectionExt::peek(conn).map_or(true, |b| b.is_some())) {
            Ok(RunEvent::IncomingData) => {
                let byte = ConnectionExt::read(conn).map_err(run_blocking::WaitForStopReasonError::Connection)?;
                Ok(run_blocking::Event::IncomingData(byte))
            }
            Ok(RunEvent::Stop(reason)) => Ok(run_blocking::Event::TargetStopped(reason)),
            Err(e) => Err(run_blocking::WaitForStopReasonError::Target(e)),
        }
    }

    fn on_interrupt(target: &mut T) -> Result<Option<Self::StopReason>, T::Error> {
        target.on_interrupt()
    }
}

/// Run one debugging session for `target` over the debugger connection `conn`.
fn run_session<T: Session>(conn: TcpStream, target: &mut T) -> Result<DisconnectReason, GdbStubError<T::Error, io::Error>> {
    GdbStub::new(conn).run_blocking::<EventLoop<T>>(target)
}

/// Describe how a session ended in one line.  `disconnected` and `killed`
/// are the target's own wording for the debugger detaching and killing it.
fn session_summary<E: fmt::Display>(result: Result<DisconnectReason, E>, disconnected: &str, killed: &str) -> Result<String, GdbError> {
    match result {
        Ok(DisconnectReason::Disconnect) => Ok(disconnected.to_owned()),
        Ok(DisconnectReason::TargetExited(code)) => Ok(format!("program halted (exit status {code})")),
        Ok(DisconnectReason::TargetTerminated(sig)) => Ok(format!("program terminated with {sig}")),
        Ok(DisconnectReason::Kill) => Ok(killed.to_owned()),
        Err(e) => Err(GdbError::Session(e.to_string())),
    }
}

/// Handle the `mmio` monitor commands both targets share, which turn
/// debugger access to device registers on and off.  Returns false if `cmd`
/// is not one of them.
fn mmio_monitor_cmd(cmd: &str, allow_mmio: &mut bool, out: &mut impl fmt::Write) -> bool {
    match cmd {
        "mmio on" => {
            *allow_mmio = true;
            outputln!(out, "MMIO access enabled: device reads have side effects (e.g. UART RX pops)");
        }
        "mmio off" => {
            *allow_mmio = false;
            outputln!(out, "MMIO access disabled");
        }
        "mmio" => outputln!(out, "MMIO access is {}", if *allow_mmio { "on" } else { "off" }),
        _ => return false,
    }
    true
}

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
    use gdbstub::common::Signal;

    #[test]
    fn session_summary_keeps_each_targets_wording() {
        let summary = |r: Result<DisconnectReason, &str>| session_summary(r, "detached", "killed").map_err(|e| e.to_string());
        assert_eq!(summary(Ok(DisconnectReason::Disconnect)), Ok("detached".to_owned()));
        assert_eq!(summary(Ok(DisconnectReason::Kill)), Ok("killed".to_owned()));
        assert_eq!(
            summary(Ok(DisconnectReason::TargetExited(3))),
            Ok("program halted (exit status 3)".to_owned())
        );
        assert_eq!(
            summary(Ok(DisconnectReason::TargetTerminated(Signal::SIGILL))),
            Ok(format!("program terminated with {}", Signal::SIGILL))
        );
        assert_eq!(summary(Err("bad packet")), Err("gdbstub error: bad packet".to_owned()));
    }

    #[test]
    fn mmio_monitor_commands_toggle_access() {
        let mut allow = false;
        let mut out = String::new();
        assert!(mmio_monitor_cmd("mmio", &mut allow, &mut out));
        assert!(mmio_monitor_cmd("mmio on", &mut allow, &mut out));
        assert!(allow);
        assert!(mmio_monitor_cmd("mmio", &mut allow, &mut out));
        assert!(mmio_monitor_cmd("mmio off", &mut allow, &mut out));
        assert!(!allow);
        assert_eq!(
            out,
            "MMIO access is off\nMMIO access enabled: device reads have side effects (e.g. UART RX pops)\n\
             MMIO access is on\nMMIO access disabled\n"
        );
        assert!(!mmio_monitor_cmd("irq", &mut allow, &mut out));
        assert!(!mmio_monitor_cmd("mmio maybe", &mut allow, &mut out));
    }

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
