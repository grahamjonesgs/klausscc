//! GDB remote-protocol server for the real board (`--gdbserver PORT --board`).
//!
//! Same register layout, target description and stop reporting as the
//! emulator server (`emu_target`), but the target is core 1 of a Nexys A7
//! running a `DEBUG_EN=1` bitstream, reached over the on-board USB-JTAG
//! (`jtag/`) through the debug module (`klaussCPU/debug_module.sv`).
//!
//! * Hardware breakpoints: the debug module's PC comparators (`Z1`), as
//!   many as DMSTATUS reports (4 on the current bitstream).
//! * Software breakpoints (`Z0`): the word at the address is replaced by
//!   `TRAP`; memory reads show the original word.  Resume invalidates the
//!   I-cache, so a patched word is always fetched.
//! * Watchpoints: the debug module's doubleword comparators (2 on the
//!   current bitstream).  The halt
//!   comes after the access, up to two instructions late (`DEBUG_PLAN.md`).
//! * A compiled-in `TRAP` (`__builtin_debugtrap`) reports SIGTRAP; resuming
//!   skips it.  `HALT` reports "exited" with the low byte of r12.
//! * Single steps run with interrupts masked; the timer, millisecond clock
//!   and cycle counter are frozen while the core is halted.
//! * MMIO (`0xF000_0000+`) is off limits unless `monitor mmio on`.
//!
//! On detach the server removes its breakpoints, clears "attached" and lets
//! the program run on.
//!
//! The debugger logic reaches the debug module only through the [`Dmi`]
//! seam, so it is also built for tests, which run it against a model of the
//! debug module; only opening the cable ([`Board::open`]) and serving a
//! session need the `board` feature.

mod dm;

use super::arch::{HW_BREAKPOINTS, HW_WATCHPOINTS, KlaussCpuArch, KlaussRegId, KlaussRegs, TRAP_WORD};
#[cfg(feature = "board")]
use super::{GdbError, accept, run_session, session_summary};
use super::{RunEvent, Session, access_last_addr, mmio_monitor_cmd};
#[cfg(feature = "board")]
use crate::jtag::Jtag;
use crate::jtag::{Dmi, DmiReq, JtagError};
use crate::layout::is_mmio;
use dm::{
    BP0, CTL_ATTACHED, CTL_HALT_ON_LOAD, CTL_HALTREQ, CTL_RESUMEREQ, CTL_STEP, Cause, DATA0, DMCONTROL, DMSTATUS, DmInfo, DmStatus, MAX_BREAKPOINTS,
    MAX_WATCHPOINTS, MEM_ADDR, MEM_BE_ALL, MEM_CMD, MEM_INC, MEM_WRITE, REG_CMD, REG_CMD_WRITE, SLOT_ENABLE, WATCH_ADDR, WP_LANES_SHIFT, WP_READ,
    WP_WRITE, WP0,
};
use gdbstub::common::Signal;
use gdbstub::stub::SingleThreadStopReason;
use gdbstub::target::ext::base::BaseOps;
use gdbstub::target::ext::base::single_register_access::{SingleRegisterAccess, SingleRegisterAccessOps};
use gdbstub::target::ext::base::singlethread::{
    SingleThreadBase, SingleThreadResume, SingleThreadResumeOps, SingleThreadSingleStep, SingleThreadSingleStepOps,
};
use gdbstub::target::ext::breakpoints::{
    Breakpoints, BreakpointsOps, HwBreakpoint, HwBreakpointOps, HwWatchpoint, HwWatchpointOps, SwBreakpoint, SwBreakpointOps, WatchKind,
};
use gdbstub::target::ext::host_info::HostInfoOps;
use gdbstub::target::ext::monitor_cmd::{ConsoleOutput, MonitorCmd, MonitorCmdOps, outputln};
use gdbstub::target::ext::process_info::ProcessInfoOps;
use gdbstub::target::{Target, TargetError, TargetResult};
use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, Instant};

/// How often a running core is polled for a halt.
const POLL: Duration = Duration::from_millis(5);

/// Why talking to the board's debug module failed.
#[derive(Debug)]
pub(crate) enum BoardError {
    /// JTAG or the DMI on top of it failed.
    Jtag(JtagError),
    /// The JTAG chain answered with this IDCODE: there is no FPGA on it.
    #[cfg_attr(not(feature = "board"), allow(dead_code, reason = "only opening the cable finds no FPGA"))]
    NoFpga {
        /// The IDCODE read (all zeros or all ones).
        idcode: u32,
    },
    /// The FPGA has no `KlaussCPU` debug module on USER3.
    #[cfg_attr(not(feature = "board"), allow(dead_code, reason = "only opening the cable finds no debug module"))]
    NoDebugModule {
        /// The FPGA's IDCODE.
        idcode: u32,
        /// What was found instead.
        detail: String,
    },
    /// The core did not halt in time.
    NotHalted {
        /// The last DMSTATUS read.
        status: DmStatus,
    },
    /// A halt request went unanswered (no program running, most likely).
    HaltRequest(Box<BoardError>),
    /// The debug module reported a failed memory command.
    MemCommand {
        /// The DMSTATUS that reported it.
        status: DmStatus,
    },
    /// A memory access runs past `0xFFFF_FFFF`.
    PastTop {
        /// The first address.
        addr: u32,
        /// The number of bytes.
        len: usize,
    },
    /// Loading the program over the UART before attaching failed.
    Load {
        /// The program being loaded.
        path: String,
        /// The serial port it was sent over.
        port: String,
        /// Why the load failed (already reported to the user).
        source: Box<crate::error::Error>,
    },
    /// After the load the core halted for this cause instead of at the entry.
    WrongCause {
        /// Why it halted.
        cause: Cause,
    },
    /// The debug module stopped answering while the program ran.
    Lost(Box<BoardError>),
    /// The debugger asked for something the board cannot do.
    Unsupported(&'static str),
}

impl fmt::Display for BoardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Jtag(e) => write!(f, "{e}"),
            Self::NoFpga { idcode } => write!(f, "JTAG IDCODE 0x{idcode:08X}: no FPGA on the chain (is the board powered?)"),
            Self::NoDebugModule { idcode, detail } => write!(
                f,
                "IDCODE 0x{idcode:08X} but no KlaussCPU debug module on USER3 ({detail}): \
                 program a bitstream built with DEBUG_EN=1"
            ),
            Self::NotHalted { status } => write!(f, "the core did not halt (DMSTATUS 0x{status:08X})"),
            Self::HaltRequest(e) => write!(f, "{e}: is a program running on the board? (load one with -s)"),
            Self::MemCommand { status } => write!(f, "debug module memory command failed (DMSTATUS 0x{status:08X})"),
            Self::PastTop { addr, len } => write!(f, "{len} bytes at 0x{addr:08X} run past the end of the address space"),
            Self::Load { path, port, source } => write!(f, "loading {path} over {port} failed ({source})"),
            Self::WrongCause { cause } => write!(f, "the core halted for cause {} instead of at the entry", cause.code()),
            Self::Lost(e) => write!(f, "lost the debug module ({e})"),
            Self::Unsupported(text) => f.write_str(text),
        }
    }
}

impl std::error::Error for BoardError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Jtag(e) => Some(e),
            Self::HaltRequest(e) | Self::Lost(e) => Some(e.as_ref()),
            Self::Load { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

impl From<JtagError> for BoardError {
    fn from(e: JtagError) -> Self {
        Self::Jtag(e)
    }
}

/// How a failed debugger request is reported to the debugger, by kind.
///
/// A memory access the debug module refused (or that runs past the top of
/// the address space) is `EFAULT`; a lost JTAG link ends the session; anything
/// else fails just that request.  A request that does not end the session is
/// also explained on stderr, since the debugger only sees an error number.
impl From<BoardError> for TargetError<BoardError> {
    fn from(e: BoardError) -> Self {
        match e {
            BoardError::Jtag(ref j) if j.is_link_lost() => Self::Fatal(e),
            BoardError::Lost(_) => Self::Fatal(e),
            BoardError::MemCommand { .. } | BoardError::PastTop { .. } => {
                eprintln!("klausscc gdbserver: {e}");
                Self::Errno(14) // EFAULT
            }
            _ => {
                eprintln!("klausscc gdbserver: {e}");
                Self::NonFatal
            }
        }
    }
}

/// Result of a debug module operation.
type BoardResult<T> = Result<T, BoardError>;

/// The debug module, over its DMI (JTAG on the real board).
pub(crate) struct Board<D> {
    dmi: D,
    /// What the debug module reported about itself when it was opened.
    info: Option<DmInfo>,
}

#[cfg(feature = "board")]
impl Board<Jtag> {
    /// Open the JTAG cable at `khz`, select the debug module and check it is
    /// there.  Returns the board and a one-line description.
    pub(crate) fn open(khz: u32) -> BoardResult<(Self, String)> {
        let mut jtag = Jtag::open(khz)?;
        let idcode = jtag.idcode()?;
        if idcode == 0 || idcode == u32::MAX {
            return Err(BoardError::NoFpga { idcode });
        }
        jtag.select_user3()?;
        let mut board = Self::new(jtag);
        let no_dm = |detail: String| BoardError::NoDebugModule { idcode, detail };
        let st = board.status().map_err(|e| no_dm(e.to_string()))?;
        let Some(info) = st.info() else {
            return Err(no_dm(format!("DMSTATUS 0x{st:08X}")));
        };
        board.info = Some(info);
        Ok((
            board,
            format!(
                "JTAG IDCODE 0x{idcode:08X}, debug module v{} ({} breakpoints, {} watchpoints)",
                info.version, info.breakpoints, info.watchpoints
            ),
        ))
    }
}

impl<D: Dmi> Board<D> {
    /// The debug module behind `dmi`, before it has reported its comparators.
    const fn new(dmi: D) -> Self {
        Self { dmi, info: None }
    }

    /// The hardware breakpoint comparators: as many as the debug module
    /// reports (at most [`MAX_BREAKPOINTS`]), or [`HW_BREAKPOINTS`] if it
    /// has not said.
    fn breakpoint_slots(&self) -> usize {
        self.info
            .map_or(HW_BREAKPOINTS, |info| usize::from(info.breakpoints).min(MAX_BREAKPOINTS))
    }

    /// The watchpoint comparators, as [`breakpoint_slots`](Self::breakpoint_slots).
    fn watchpoint_slots(&self) -> usize {
        self.info
            .map_or(HW_WATCHPOINTS, |info| usize::from(info.watchpoints).min(MAX_WATCHPOINTS))
    }

    fn batch(&mut self, reqs: &[DmiReq]) -> BoardResult<Vec<u64>> {
        self.dmi.dmi_batch(reqs).map_err(BoardError::Jtag)
    }

    /// DMSTATUS (reading clears the "halted since" bit).
    pub(crate) fn status(&mut self) -> BoardResult<DmStatus> {
        self.dmi.dmi_read(DMSTATUS).map(DmStatus::from_raw).map_err(BoardError::Jtag)
    }

    /// Write DMCONTROL.
    pub(crate) fn control(&mut self, v: u64) -> BoardResult<()> {
        self.dmi.dmi_write(DMCONTROL, v).map_err(BoardError::Jtag)
    }

    /// Wait up to `timeout` for the core to be halted.
    pub(crate) fn wait_halted(&mut self, timeout: Duration) -> BoardResult<DmStatus> {
        let t0 = Instant::now();
        loop {
            let st = self.status()?;
            if st.halted() {
                return Ok(st);
            }
            if t0.elapsed() > timeout {
                return Err(BoardError::NotHalted { status: st });
            }
            std::thread::sleep(POLL);
        }
    }

    fn read_regs(&mut self) -> BoardResult<Vec<u64>> {
        let reqs: Vec<DmiReq> = KlaussRegId::all()
            .flat_map(|reg| [DmiReq::Write(REG_CMD, reg.regnum() as u64), DmiReq::Read(DATA0)])
            .collect();
        let v = self.batch(&reqs)?;
        Ok(v.into_iter().skip(1).step_by(2).collect())
    }

    fn read_reg(&mut self, reg: KlaussRegId) -> BoardResult<u64> {
        Ok(self.batch(&[DmiReq::Write(REG_CMD, reg.regnum() as u64), DmiReq::Read(DATA0)])?[1])
    }

    fn write_reg(&mut self, reg: KlaussRegId, v: u64) -> BoardResult<()> {
        self.batch(&[DmiReq::Write(DATA0, v), DmiReq::Write(REG_CMD, reg.regnum() as u64 | REG_CMD_WRITE)])
            .map(|_| ())
    }

    /// Check (and clear) the DM's sticky error after a memory batch.
    fn check_mem(&mut self, st: DmStatus) -> BoardResult<()> {
        if st.error() || st.busy() {
            self.dmi.dmi_write(DMSTATUS, 0)?;
            return Err(BoardError::MemCommand { status: st });
        }
        Ok(())
    }

    /// Read whole doublewords starting at the doubleword `dw_addr`.
    fn read_dwords(&mut self, dw_addr: u32, n: usize) -> BoardResult<Vec<u64>> {
        let mut reqs = Vec::with_capacity(2 * n + 2);
        reqs.push(DmiReq::Write(MEM_ADDR, u64::from(dw_addr & !7)));
        for _ in 0..n {
            reqs.push(DmiReq::Write(MEM_CMD, MEM_BE_ALL | MEM_INC));
            reqs.push(DmiReq::Read(DATA0));
        }
        reqs.push(DmiReq::Read(DMSTATUS));
        let v = self.batch(&reqs)?;
        self.check_mem(DmStatus::from_raw(v[v.len() - 1]))?;
        Ok(v[1..v.len() - 1].iter().skip(1).step_by(2).copied().collect())
    }

    /// Read bytes (any alignment).
    pub(crate) fn read_mem(&mut self, addr: u32, buf: &mut [u8]) -> BoardResult<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let last_addr = access_last_addr(addr, buf.len()).ok_or_else(|| past_top(addr, buf.len()))?;
        let first = addr & !7;
        let last = last_addr & !7;
        let n = ((last - first) / 8 + 1) as usize;
        let mut done = 0;
        // bound one batch (~128 dwords = 1 KiB)
        for start in (0..n).step_by(128) {
            let cnt = (n - start).min(128);
            let dws = self.read_dwords(first + start as u32 * 8, cnt)?;
            for (i, dw) in dws.iter().enumerate() {
                let base = first + (start + i) as u32 * 8;
                for (lane, b) in dw.to_le_bytes().iter().enumerate() {
                    let a = base + lane as u32;
                    if a >= addr && ((a - addr) as usize) < buf.len() {
                        buf[(a - addr) as usize] = *b;
                        done += 1;
                    }
                }
            }
        }
        debug_assert_eq!(done, buf.len());
        Ok(())
    }

    /// Write bytes (any alignment): byte-enabled doubleword writes.
    pub(crate) fn write_mem(&mut self, addr: u32, data: &[u8]) -> BoardResult<()> {
        if data.is_empty() {
            return Ok(());
        }
        let last = access_last_addr(addr, data.len()).ok_or_else(|| past_top(addr, data.len()))?;
        let first = addr & !7;
        let mut reqs = vec![DmiReq::Write(MEM_ADDR, u64::from(first))];
        for base in (first..=(last & !7)).step_by(8) {
            let mut bytes = [0_u8; 8];
            let mut be = 0_u64;
            for (lane, b) in bytes.iter_mut().enumerate() {
                let a = base + lane as u32;
                if a >= addr && a <= last {
                    *b = data[(a - addr) as usize];
                    be |= 1 << lane;
                }
            }
            reqs.push(DmiReq::Write(DATA0, u64::from_le_bytes(bytes)));
            reqs.push(DmiReq::Write(MEM_CMD, be | MEM_WRITE | MEM_INC));
        }
        reqs.push(DmiReq::Read(DMSTATUS));
        let v = self.batch(&reqs)?;
        self.check_mem(DmStatus::from_raw(v[v.len() - 1]))
    }
}

/// The error for a memory access that runs past `0xFFFF_FFFF`.
const fn past_top(addr: u32, len: usize) -> BoardError {
    BoardError::PastTop { addr, len }
}

/// Disarm every hardware breakpoint and watchpoint slot and clear the debug
/// module's sticky error.
fn clear_slots<D: Dmi>(board: &mut Board<D>) -> BoardResult<()> {
    let clear: Vec<DmiReq> = (0..board.breakpoint_slots() as u8)
        .map(|i| DmiReq::Write(BP0 + i, 0))
        .chain((0..board.watchpoint_slots() as u8).map(|i| DmiReq::Write(WP0 + i, 0)))
        .chain(std::iter::once(DmiReq::Write(DMSTATUS, 0)))
        .collect();
    board.batch(&clear).map(drop)
}

// ---- target -----------------------------------------------------------------

/// A watchpoint slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Wp {
    addr: u32,
    len: u32,
    kind: WatchKind,
}

/// Where the debugger's run of the core stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunState {
    /// Halted, and any halt reported to the debugger.
    Halted,
    /// A resume or step was issued and its halt has not been reported yet.
    Running {
        /// DMSTATUS has shown the halt the run is waiting for.
        halt_seen: bool,
    },
    /// A step was consumed by skipping a compiled-in TRAP; it is reported
    /// as done without the core running.
    SkippedTrap,
    /// The program executed HALT: there is nothing more to run.
    Exited,
}

impl RunState {
    /// A resume or step is outstanding.
    const fn is_running(self) -> bool {
        matches!(self, Self::Running { .. } | Self::SkippedTrap)
    }
}

/// The board plus the debugger's view of it.
pub(crate) struct BoardTarget<D> {
    board: Board<D>,
    /// The hardware breakpoint comparators, one entry per slot.
    hw_breakpoints: Vec<Option<u32>>,
    /// The watchpoint comparators, one entry per slot.
    watches: Vec<Option<Wp>>,
    /// Software breakpoints: address -> the original word under the TRAP.
    sw_breakpoints: BTreeMap<u32, u32>,
    state: RunState,
    /// `monitor mmio on`: let the debugger access device registers.
    allow_mmio: bool,
}

impl<D: Dmi> BoardTarget<D> {
    fn new(board: Board<D>) -> Self {
        Self {
            hw_breakpoints: vec![None; board.breakpoint_slots()],
            watches: vec![None; board.watchpoint_slots()],
            board,
            sw_breakpoints: BTreeMap::new(),
            state: RunState::Halted,
            allow_mmio: false,
        }
    }

    /// Check a debugger memory access: EFAULT if it runs past `0xFFFF_FFFF`,
    /// or touches MMIO while that is off limits.
    fn check_access(&self, addr: u32, len: usize) -> TargetResult<(), Self> {
        let last = access_last_addr(addr, len).ok_or(TargetError::Errno(14))?; // EFAULT
        if self.allow_mmio || !(is_mmio(addr) || is_mmio(last)) {
            Ok(())
        } else {
            Err(TargetError::Errno(14))
        }
    }

    /// Resume (or step) from the halt.  A compiled-in TRAP at the PC is
    /// skipped (for a step, the skip is the step).
    fn start(&mut self, step: bool) -> BoardResult<()> {
        let pc = self.board.read_reg(KlaussRegId::Pc)? as u32;
        if !self.sw_breakpoints.contains_key(&pc) && self.read_word(pc)? == TRAP_WORD {
            self.board.write_reg(KlaussRegId::Pc, u64::from(pc.wrapping_add(4)))?;
            if step {
                self.state = RunState::SkippedTrap;
                return Ok(());
            }
        }
        self.board.status()?; // clear "halted since"
        self.board.control(CTL_ATTACHED | CTL_RESUMEREQ | if step { CTL_STEP } else { 0 })?;
        self.state = RunState::Running { halt_seen: false };
        Ok(())
    }

    fn read_word(&mut self, addr: u32) -> BoardResult<u32> {
        let mut b = [0_u8; 4];
        self.board.read_mem(addr, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    /// Why the core is halted, as a stop reply.
    fn stop_reason(&mut self, st: DmStatus) -> BoardResult<SingleThreadStopReason<u32>> {
        Ok(match st.cause() {
            Cause::Step => SingleThreadStopReason::DoneStep,
            Cause::Breakpoint => SingleThreadStopReason::HwBreak(()),
            Cause::Watchpoint => {
                let addr = self.board.batch(&[DmiReq::Read(WATCH_ADDR)])?[0] as u32;
                let kind = self
                    .watches
                    .iter()
                    .flatten()
                    .find(|w| addr >= w.addr && addr < w.addr + w.len.max(1))
                    .or_else(|| self.watches.iter().flatten().find(|w| addr & !7 == w.addr & !7))
                    .map_or(WatchKind::Write, |w| w.kind);
                SingleThreadStopReason::Watch { tid: (), kind, addr }
            }
            Cause::Trap => {
                let pc = self.board.read_reg(KlaussRegId::Pc)? as u32;
                if self.sw_breakpoints.contains_key(&pc) {
                    SingleThreadStopReason::SwBreak(())
                } else {
                    SingleThreadStopReason::Signal(Signal::SIGTRAP)
                }
            }
            Cause::Illegal => SingleThreadStopReason::Signal(Signal::SIGILL),
            Cause::Halt => {
                self.state = RunState::Exited;
                SingleThreadStopReason::Exited((self.board.read_reg(KlaussRegId::Gpr(12))? & 0xFF) as u8)
            }
            Cause::Request => SingleThreadStopReason::Signal(Signal::SIGINT),
            Cause::Unknown(_) => SingleThreadStopReason::Signal(Signal::SIGTRAP),
        })
    }

    /// Check once whether the run has stopped.  With no run outstanding,
    /// a halted core reports why it is halted.
    fn poll(&mut self) -> BoardResult<Option<SingleThreadStopReason<u32>>> {
        let halt_seen = match self.state {
            RunState::SkippedTrap => {
                self.state = RunState::Halted;
                return Ok(Some(SingleThreadStopReason::DoneStep));
            }
            RunState::Running { halt_seen } => halt_seen,
            RunState::Halted | RunState::Exited => true,
        };
        let st = self.board.status()?;
        let halt_seen = halt_seen || st.halted_seen();
        if st.halted() && halt_seen {
            self.state = RunState::Halted;
            return self.stop_reason(st).map(Some);
        }
        if let RunState::Running { .. } = self.state {
            self.state = RunState::Running { halt_seen };
        }
        Ok(None)
    }

    fn run(&mut self, mut incoming_data: impl FnMut() -> bool) -> BoardResult<RunEvent> {
        loop {
            if let Some(stop) = self.poll()? {
                return Ok(RunEvent::Stop(stop));
            }
            if incoming_data() {
                return Ok(RunEvent::IncomingData);
            }
            std::thread::sleep(POLL);
        }
    }

    fn interrupt(&mut self) -> BoardResult<SingleThreadStopReason<u32>> {
        self.board.control(CTL_ATTACHED | CTL_HALTREQ)?;
        let st = self.board.wait_halted(Duration::from_secs(2))?;
        self.state = RunState::Halted;
        // a breakpoint/step may have won the race against the request
        self.stop_reason(st)
    }

    fn write_bp(&mut self, slot: usize) -> BoardResult<()> {
        let v = self.hw_breakpoints[slot].map_or(0, |a| u64::from(a) | SLOT_ENABLE);
        self.board.batch(&[DmiReq::Write(BP0 + slot as u8, v)]).map(|_| ())
    }

    fn write_wp(&mut self, slot: usize) -> BoardResult<()> {
        let v = self.watches[slot].map_or(0, |w| {
            let mut lanes = 0_u64;
            for a in w.addr..w.addr + w.len.max(1) {
                lanes |= 1 << (a & 7);
            }
            let access = match w.kind {
                WatchKind::Write => WP_WRITE,
                WatchKind::Read => WP_READ,
                WatchKind::ReadWrite => WP_READ | WP_WRITE,
            };
            u64::from(w.addr & !7) | SLOT_ENABLE | access | (lanes << WP_LANES_SHIFT)
        });
        self.board.batch(&[DmiReq::Write(WP0 + slot as u8, v)]).map(|_| ())
    }

    /// Undo everything the session set and let the program run on.
    ///
    /// Best effort: every step is tried even when an earlier one fails (a
    /// `TRAP` left patched into program memory is worse than a failed
    /// halt), and the first error is returned.
    fn detach(&mut self) -> BoardResult<()> {
        let mut first_err: Option<BoardError> = None;
        let mut note = |result: BoardResult<()>| {
            if let Err(e) = result {
                first_err.get_or_insert(e);
            }
        };
        if self.state.is_running() {
            note(self.interrupt().map(drop));
        }
        // a program that executed HALT stays halted (resuming would run on
        // past its end); if the status cannot be read, only clear "attached".
        // Read the halt cause before clear_slots writes DMSTATUS.
        let resume = match self.board.status() {
            Ok(st) => self.state != RunState::Exited && st.cause() != Cause::Halt,
            Err(e) => {
                note(Err(e));
                false
            }
        };
        for (addr, orig) in std::mem::take(&mut self.sw_breakpoints) {
            note(self.board.write_mem(addr, &orig.to_le_bytes()));
        }
        self.hw_breakpoints.fill(None);
        self.watches.fill(None);
        note(clear_slots(&mut self.board));
        note(self.board.control(if resume { CTL_RESUMEREQ } else { 0 }));
        first_err.map_or(Ok(()), Err)
    }
}

impl<D: Dmi> Target for BoardTarget<D> {
    type Arch = KlaussCpuArch;
    type Error = BoardError;

    fn base_ops(&mut self) -> BaseOps<'_, Self::Arch, Self::Error> {
        BaseOps::SingleThread(self)
    }

    fn support_breakpoints(&mut self) -> Option<BreakpointsOps<'_, Self>> {
        Some(self)
    }

    fn support_host_info(&mut self) -> Option<HostInfoOps<'_, Self>> {
        Some(self)
    }

    fn support_process_info(&mut self) -> Option<ProcessInfoOps<'_, Self>> {
        Some(self)
    }

    fn support_monitor_cmd(&mut self) -> Option<MonitorCmdOps<'_, Self>> {
        Some(self)
    }
}

impl<D: Dmi> MonitorCmd for BoardTarget<D> {
    fn handle_monitor_cmd(&mut self, cmd: &[u8], mut out: ConsoleOutput<'_>) -> Result<(), Self::Error> {
        // Not UTF-8: no command, so the help.
        let cmd = std::str::from_utf8(cmd).map_or("", str::trim);
        match cmd {
            "status" => match self.board.status() {
                Ok(st) => outputln!(
                    out,
                    "DMSTATUS 0x{st:08X}: halted {} cause {} park_kind {}",
                    u8::from(st.halted()),
                    st.cause().code(),
                    st.park_kind()
                ),
                Err(e) => outputln!(out, "DMSTATUS read failed: {e}"),
            },
            _ if mmio_monitor_cmd(cmd, &mut self.allow_mmio, &mut out) => {}
            _ => {
                outputln!(out, "klausscc gdbserver (board) monitor commands:");
                outputln!(out, "  mmio on|off   allow debugger reads/writes of 0xF000_0000+ (default off)");
                outputln!(out, "  status        debug module status");
            }
        }
        Ok(())
    }
}

impl<D: Dmi> SingleThreadBase for BoardTarget<D> {
    fn read_registers(&mut self, regs: &mut KlaussRegs) -> TargetResult<(), Self> {
        let v = self.board.read_regs()?;
        regs.r.copy_from_slice(&v[..16]);
        regs.sp = v[16] as u32;
        regs.pc = v[17] as u32;
        regs.flags = v[18] as u32;
        regs.int_mask = v[19] as u32;
        Ok(())
    }

    fn write_registers(&mut self, regs: &KlaussRegs) -> TargetResult<(), Self> {
        for reg in KlaussRegId::all() {
            self.board.write_reg(reg, regs.get(reg))?;
        }
        Ok(())
    }

    fn support_single_register_access(&mut self) -> Option<SingleRegisterAccessOps<'_, (), Self>> {
        Some(self)
    }

    fn read_addrs(&mut self, start_addr: u32, data: &mut [u8]) -> TargetResult<usize, Self> {
        self.check_access(start_addr, data.len())?;
        self.board.read_mem(start_addr, data)?;
        // show the original words under software breakpoints
        let end = u64::from(start_addr) + data.len() as u64;
        for (&bp, &orig) in &self.sw_breakpoints {
            for (i, b) in orig.to_le_bytes().iter().enumerate() {
                let a = u64::from(bp) + i as u64;
                if a >= u64::from(start_addr) && a < end {
                    data[(a - u64::from(start_addr)) as usize] = *b;
                }
            }
        }
        Ok(data.len())
    }

    fn write_addrs(&mut self, start_addr: u32, data: &[u8]) -> TargetResult<(), Self> {
        self.check_access(start_addr, data.len())?;
        self.board.write_mem(start_addr, data).map_err(Into::into)
    }

    fn support_resume(&mut self) -> Option<SingleThreadResumeOps<'_, Self>> {
        Some(self)
    }
}

impl<D: Dmi> SingleRegisterAccess<()> for BoardTarget<D> {
    fn read_register(&mut self, _tid: (), reg_id: KlaussRegId, buf: &mut [u8]) -> TargetResult<usize, Self> {
        let v = self.board.read_reg(reg_id)?;
        Ok(reg_id.encode(v, buf))
    }

    fn write_register(&mut self, _tid: (), reg_id: KlaussRegId, val: &[u8]) -> TargetResult<(), Self> {
        let value = reg_id.decode(val).ok_or(TargetError::NonFatal)?;
        self.board.write_reg(reg_id, value).map_err(Into::into)
    }
}

impl<D: Dmi> SingleThreadResume for BoardTarget<D> {
    fn resume(&mut self, signal: Option<Signal>) -> Result<(), Self::Error> {
        if signal.is_some() {
            return Err(BoardError::Unsupported("no support for continuing with a signal"));
        }
        self.start(false)
    }

    fn support_single_step(&mut self) -> Option<SingleThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}

impl<D: Dmi> SingleThreadSingleStep for BoardTarget<D> {
    fn step(&mut self, signal: Option<Signal>) -> Result<(), Self::Error> {
        if signal.is_some() {
            return Err(BoardError::Unsupported("no support for stepping with a signal"));
        }
        self.start(true)
    }
}

impl<D: Dmi> Breakpoints for BoardTarget<D> {
    fn support_sw_breakpoint(&mut self) -> Option<SwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_breakpoint(&mut self) -> Option<HwBreakpointOps<'_, Self>> {
        Some(self)
    }

    fn support_hw_watchpoint(&mut self) -> Option<HwWatchpointOps<'_, Self>> {
        Some(self)
    }
}

impl<D: Dmi> SwBreakpoint for BoardTarget<D> {
    fn add_sw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        if addr & 3 != 0 || is_mmio(addr) {
            return Ok(false);
        }
        if self.sw_breakpoints.contains_key(&addr) {
            return Ok(true);
        }
        let orig = self.read_word(addr)?;
        self.board.write_mem(addr, &TRAP_WORD.to_le_bytes())?;
        self.sw_breakpoints.insert(addr, orig);
        Ok(true)
    }

    fn remove_sw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        let Some(orig) = self.sw_breakpoints.remove(&addr) else {
            return Ok(false);
        };
        self.board.write_mem(addr, &orig.to_le_bytes())?;
        Ok(true)
    }
}

impl<D: Dmi> HwBreakpoint for BoardTarget<D> {
    fn add_hw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        if self.hw_breakpoints.contains(&Some(addr)) {
            return Ok(true);
        }
        let Some(slot) = self.hw_breakpoints.iter().position(Option::is_none) else {
            return Ok(false);
        };
        self.hw_breakpoints[slot] = Some(addr);
        self.write_bp(slot)?;
        Ok(true)
    }

    fn remove_hw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        let Some(slot) = self.hw_breakpoints.iter().position(|b| *b == Some(addr)) else {
            return Ok(false);
        };
        self.hw_breakpoints[slot] = None;
        self.write_bp(slot)?;
        Ok(true)
    }
}

impl<D: Dmi> HwWatchpoint for BoardTarget<D> {
    fn add_hw_watchpoint(&mut self, addr: u32, len: u32, kind: WatchKind) -> TargetResult<bool, Self> {
        // one slot covers lanes of a single doubleword
        if len == 0 || len > 8 || (addr & 7) + len > 8 {
            return Ok(false);
        }
        let w = Wp { addr, len, kind };
        let Some(slot) = self.watches.iter().position(Option::is_none) else {
            return Ok(false);
        };
        self.watches[slot] = Some(w);
        self.write_wp(slot)?;
        Ok(true)
    }

    fn remove_hw_watchpoint(&mut self, addr: u32, len: u32, kind: WatchKind) -> TargetResult<bool, Self> {
        let w = Wp { addr, len, kind };
        let Some(slot) = self.watches.iter().position(|s| *s == Some(w)) else {
            return Ok(false);
        };
        self.watches[slot] = None;
        self.write_wp(slot)?;
        Ok(true)
    }
}

impl_host_process_info!([D: Dmi] BoardTarget<D>);

impl<D: Dmi> Session for BoardTarget<D> {
    /// A debug module that stops answering while the program runs ends the
    /// session ([`BoardError::Lost`]).
    fn run_until_event(&mut self, incoming_data: impl FnMut() -> bool) -> BoardResult<RunEvent> {
        self.run(incoming_data).map_err(|e| BoardError::Lost(Box::new(e)))
    }

    fn on_interrupt(&mut self) -> BoardResult<Option<SingleThreadStopReason<u32>>> {
        self.interrupt().map(Some)
    }
}

/// Bring the core to a debug halt for a new session.
///
/// With `load`, `load()` sends the program over the UART with halt-on-load
/// set, and the core halts on the program's first instruction; otherwise the
/// running program is halted where it is.
pub(crate) fn attach<D: Dmi>(board: &mut Board<D>, load: Option<&mut dyn FnMut() -> BoardResult<()>>) -> BoardResult<u32> {
    // start clean: a server that was killed leaves its slots armed
    clear_slots(board)?;
    if let Some(load) = load {
        board.control(CTL_ATTACHED | CTL_HALT_ON_LOAD)?;
        board.status()?;
        load()?;
        let st = board.wait_halted(Duration::from_secs(60))?;
        board.control(CTL_ATTACHED)?;
        if st.cause() != Cause::Request {
            return Err(BoardError::WrongCause { cause: st.cause() });
        }
    } else {
        if !board.status()?.halted() {
            board.control(CTL_ATTACHED | CTL_HALTREQ)?;
            board
                .wait_halted(Duration::from_secs(2))
                .map_err(|e| BoardError::HaltRequest(Box::new(e)))?;
        }
        board.control(CTL_ATTACHED)?;
    }
    Ok(board.read_reg(KlaussRegId::Pc)? as u32)
}

/// Serve one debugging session for the halted `board` on `127.0.0.1:port`.
#[cfg(feature = "board")]
pub(crate) fn serve<D: Dmi>(board: Board<D>, port: u16) -> Result<String, GdbError> {
    let conn = accept(port)?;
    let mut target = BoardTarget::new(board);
    let result = run_session(conn, &mut target);
    let summary = session_summary(result, "debugger detached; the program runs on", "debugger killed the session");
    if let Err(e) = target.detach() {
        eprintln!("klausscc gdbserver: detach: {e}");
    }
    summary
}

#[cfg(test)]
mod tests;
