//! GDB remote-protocol server for the real board (`--gdbserver PORT --board`).
//!
//! Same register layout, target description and stop reporting as the
//! emulator server (`gdbserver.rs`), but the target is core 1 of a Nexys A7
//! running a `DEBUG_EN=1` bitstream, reached over the on-board USB-JTAG
//! (`jtag.rs`) through the debug module (`klaussCPU/debug_module.sv`).
//!
//! * Hardware breakpoints: the debug module's 4 PC comparators (`Z1`).
//! * Software breakpoints (`Z0`): the word at the address is replaced by
//!   `TRAP`; memory reads show the original word.  Resume invalidates the
//!   I-cache, so a patched word is always fetched.
//! * Watchpoints: the debug module's 2 doubleword comparators.  The halt
//!   comes after the access, up to two instructions late (DEBUG_PLAN.md).
//! * A compiled-in `TRAP` (`__builtin_debugtrap`) reports SIGTRAP; resuming
//!   skips it.  `HALT` reports "exited" with the low byte of r12.
//! * Single steps run with interrupts masked; the timer, millisecond clock
//!   and cycle counter are frozen while the core is halted.
//! * MMIO (`0xF000_0000+`) is off limits unless `monitor mmio on`.
//!
//! On detach the server removes its breakpoints, clears "attached" and lets
//! the program run on.

use crate::gdbserver::{is_mmio, KlaussCpuArch, KlaussRegId, KlaussRegs, HW_BREAKPOINTS, HW_WATCHPOINTS, TRAP_WORD, TRIPLE};
use crate::jtag::{DmiReq, Jtag};
use gdbstub::common::{Endianness, Pid, Signal};
use gdbstub::conn::ConnectionExt;
use gdbstub::stub::{run_blocking, DisconnectReason, GdbStub, SingleThreadStopReason};
use gdbstub::target::ext::base::single_register_access::{SingleRegisterAccess, SingleRegisterAccessOps};
use gdbstub::target::ext::base::singlethread::{
    SingleThreadBase, SingleThreadResume, SingleThreadResumeOps, SingleThreadSingleStep, SingleThreadSingleStepOps,
};
use gdbstub::target::ext::base::BaseOps;
use gdbstub::target::ext::breakpoints::{
    Breakpoints, BreakpointsOps, HwBreakpoint, HwBreakpointOps, HwWatchpoint, HwWatchpointOps, SwBreakpoint, SwBreakpointOps, WatchKind,
};
use gdbstub::target::ext::host_info::{HostInfo, HostInfoOps, HostInfoResponse};
use gdbstub::target::ext::monitor_cmd::{outputln, ConsoleOutput, MonitorCmd, MonitorCmdOps};
use gdbstub::target::ext::process_info::{ProcessInfo, ProcessInfoOps, ProcessInfoResponse};
use gdbstub::target::{Target, TargetError, TargetResult};
use std::collections::BTreeMap;
use std::net::TcpListener;
use std::time::{Duration, Instant};

// ---- debug module registers (debug_module.sv) ------------------------------
const DMCONTROL: u8 = 0x00;
const DMSTATUS: u8 = 0x01;
const REG_CMD: u8 = 0x02;
const DATA0: u8 = 0x03;
const MEM_ADDR: u8 = 0x04;
const MEM_CMD: u8 = 0x05;
const BP0: u8 = 0x10;
const WP0: u8 = 0x18;
const WATCH_ADDR: u8 = 0x1F;

const CTL_HALTREQ: u64 = 1 << 0;
const CTL_RESUMEREQ: u64 = 1 << 1;
const CTL_STEP: u64 = 1 << 2;
const CTL_ATTACHED: u64 = 1 << 3;
const CTL_HALT_ON_LOAD: u64 = 1 << 4;

const ST_HALTED: u64 = 1 << 0;
const ST_BUSY: u64 = 1 << 2;
const ST_ERROR: u64 = 1 << 3;
const ST_HALTED_SEEN: u64 = 1 << 11;

const MEM_WRITE: u64 = 1 << 8;
const MEM_INC: u64 = 1 << 9;

/// `klauss_pkg::DBG_CAUSE_*`.
const CAUSE_REQ: u64 = 0;
const CAUSE_STEP: u64 = 1;
const CAUSE_BP: u64 = 2;
const CAUSE_WATCH: u64 = 3;
const CAUSE_TRAP: u64 = 4;
const CAUSE_ILL: u64 = 5;
const CAUSE_HALT: u64 = 6;

/// `REG_CMD` index of the PC (16 = SP, 18 = flags, 19 = `INT_MASK`).
const REG_PC: u64 = 17;
const NUM_REGS: u64 = 20;

/// How often a running core is polled for a halt.
const POLL: Duration = Duration::from_millis(5);

type JResult<T> = Result<T, String>;

fn jerr(e: crate::jtag::JtagError) -> String {
    e.0
}

/// The debug module, over JTAG.
pub struct Board {
    jtag: Jtag,
}

impl Board {
    /// Open the JTAG cable at `khz`, select the debug module and check it is
    /// there.  Returns the board and a one-line description.
    pub fn open(khz: u32) -> JResult<(Self, String)> {
        let mut jtag = Jtag::open(khz).map_err(jerr)?;
        let idcode = jtag.idcode().map_err(jerr)?;
        if idcode == 0 || idcode == u32::MAX {
            return Err(format!("JTAG IDCODE 0x{idcode:08X}: no FPGA on the chain (is the board powered?)"));
        }
        jtag.select_user3().map_err(jerr)?;
        let mut board = Self { jtag };
        let no_dm = |what: String| {
            format!(
                "IDCODE 0x{idcode:08X} but no KlaussCPU debug module on USER3 ({what}): \
                 program a bitstream built with DEBUG_EN=1"
            )
        };
        let st = board.status().map_err(no_dm)?;
        if (st >> 24) & 0xFF != 0x4B {
            return Err(no_dm(format!("DMSTATUS 0x{st:08X}")));
        }
        Ok((
            board,
            format!(
                "JTAG IDCODE 0x{idcode:08X}, debug module v{} ({} breakpoints, {} watchpoints)",
                (st >> 12) & 0xF,
                (st >> 16) & 0xF,
                (st >> 20) & 0xF
            ),
        ))
    }

    fn batch(&mut self, reqs: &[DmiReq]) -> JResult<Vec<u64>> {
        self.jtag.dmi_batch(reqs).map_err(jerr)
    }

    /// DMSTATUS (reading clears the "halted since" bit).
    pub fn status(&mut self) -> JResult<u64> {
        self.jtag.dmi_read(DMSTATUS).map_err(jerr)
    }

    /// Write DMCONTROL.
    pub fn control(&mut self, v: u64) -> JResult<()> {
        self.jtag.dmi_write(DMCONTROL, v).map_err(jerr)
    }

    /// Wait up to `timeout` for the core to be halted.
    pub fn wait_halted(&mut self, timeout: Duration) -> JResult<u64> {
        let t0 = Instant::now();
        loop {
            let st = self.status()?;
            if st & ST_HALTED != 0 {
                return Ok(st);
            }
            if t0.elapsed() > timeout {
                return Err(format!("the core did not halt (DMSTATUS 0x{st:08X})"));
            }
            std::thread::sleep(POLL);
        }
    }

    fn read_regs(&mut self) -> JResult<Vec<u64>> {
        let reqs: Vec<DmiReq> = (0..NUM_REGS).flat_map(|i| [DmiReq::Write(REG_CMD, i), DmiReq::Read(DATA0)]).collect();
        let v = self.batch(&reqs)?;
        Ok(v.into_iter().skip(1).step_by(2).collect())
    }

    fn read_reg(&mut self, idx: u64) -> JResult<u64> {
        Ok(self.batch(&[DmiReq::Write(REG_CMD, idx), DmiReq::Read(DATA0)])?[1])
    }

    fn write_reg(&mut self, idx: u64, v: u64) -> JResult<()> {
        self.batch(&[DmiReq::Write(DATA0, v), DmiReq::Write(REG_CMD, idx | (1 << 8))]).map(|_| ())
    }

    /// Check (and clear) the DM's sticky error after a memory batch.
    fn check_mem(&mut self, st: u64) -> JResult<()> {
        if st & (ST_ERROR | ST_BUSY) != 0 {
            self.jtag.dmi_write(DMSTATUS, 0).map_err(jerr)?;
            return Err(format!("debug module memory command failed (DMSTATUS 0x{st:08X})"));
        }
        Ok(())
    }

    /// Read whole doublewords starting at the doubleword `dw_addr`.
    fn read_dwords(&mut self, dw_addr: u32, n: usize) -> JResult<Vec<u64>> {
        let mut reqs = Vec::with_capacity(2 * n + 2);
        reqs.push(DmiReq::Write(MEM_ADDR, u64::from(dw_addr & !7)));
        for _ in 0..n {
            reqs.push(DmiReq::Write(MEM_CMD, 0xFF | MEM_INC));
            reqs.push(DmiReq::Read(DATA0));
        }
        reqs.push(DmiReq::Read(DMSTATUS));
        let v = self.batch(&reqs)?;
        self.check_mem(v[v.len() - 1])?;
        Ok(v[1..v.len() - 1].iter().skip(1).step_by(2).copied().collect())
    }

    /// Read bytes (any alignment).
    pub fn read_mem(&mut self, addr: u32, buf: &mut [u8]) -> JResult<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let first = addr & !7;
        let last = (addr + buf.len() as u32 - 1) & !7;
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
    pub fn write_mem(&mut self, addr: u32, data: &[u8]) -> JResult<()> {
        if data.is_empty() {
            return Ok(());
        }
        let first = addr & !7;
        let end = addr + data.len() as u32;
        let mut reqs = vec![DmiReq::Write(MEM_ADDR, u64::from(first))];
        let mut base = first;
        while base < end {
            let mut bytes = [0_u8; 8];
            let mut be = 0_u64;
            for (lane, b) in bytes.iter_mut().enumerate() {
                let a = base + lane as u32;
                if a >= addr && a < end {
                    *b = data[(a - addr) as usize];
                    be |= 1 << lane;
                }
            }
            reqs.push(DmiReq::Write(DATA0, u64::from_le_bytes(bytes)));
            reqs.push(DmiReq::Write(MEM_CMD, be | MEM_WRITE | MEM_INC));
            base += 8;
        }
        reqs.push(DmiReq::Read(DMSTATUS));
        let v = self.batch(&reqs)?;
        self.check_mem(v[v.len() - 1])
    }
}

// ---- target -----------------------------------------------------------------

/// What stopped a run, or debugger input waiting.
enum RunEvent {
    IncomingData,
    Stop(SingleThreadStopReason<u32>),
}

/// A watchpoint slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Wp {
    addr: u32,
    len: u32,
    kind: WatchKind,
}

/// The board plus the debugger's view of it.
#[allow(clippy::struct_excessive_bools, reason = "independent session flags")]
pub struct BoardTarget {
    board: Board,
    hw_breakpoints: [Option<u32>; HW_BREAKPOINTS],
    watches: [Option<Wp>; HW_WATCHPOINTS],
    /// Software breakpoints: address -> the original word under the TRAP.
    sw_breakpoints: BTreeMap<u32, u32>,
    /// A resume/step was issued and its halt has not been reported yet.
    running: bool,
    /// The halt the current run is waiting for has been seen.
    halt_seen: bool,
    /// A step consumed by skipping a compiled-in TRAP.
    skipped_trap_step: bool,
    allow_mmio: bool,
    /// The program has halted (HALT): nothing more to run.
    exited: bool,
}

impl BoardTarget {
    const fn new(board: Board) -> Self {
        Self {
            board,
            hw_breakpoints: [None; HW_BREAKPOINTS],
            watches: [None; HW_WATCHPOINTS],
            sw_breakpoints: BTreeMap::new(),
            running: false,
            halt_seen: false,
            skipped_trap_step: false,
            allow_mmio: false,
            exited: false,
        }
    }

    fn mem_allowed(&self, addr: u32, len: usize) -> bool {
        self.allow_mmio || !(is_mmio(addr) || is_mmio(addr.wrapping_add(len.saturating_sub(1) as u32)))
    }

    /// Resume (or step) from the halt.  A compiled-in TRAP at the PC is
    /// skipped (for a step, the skip is the step).
    fn start(&mut self, step: bool) -> JResult<()> {
        let pc = self.board.read_reg(REG_PC)? as u32;
        if !self.sw_breakpoints.contains_key(&pc) && self.read_word(pc)? == TRAP_WORD {
            self.board.write_reg(REG_PC, u64::from(pc.wrapping_add(4)))?;
            if step {
                self.skipped_trap_step = true;
                self.running = true;
                return Ok(());
            }
        }
        self.board.status()?; // clear "halted since"
        self.board.control(CTL_ATTACHED | CTL_RESUMEREQ | if step { CTL_STEP } else { 0 })?;
        self.running = true;
        self.halt_seen = false;
        Ok(())
    }

    fn read_word(&mut self, addr: u32) -> JResult<u32> {
        let mut b = [0_u8; 4];
        self.board.read_mem(addr, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    /// Why the core is halted, as a stop reply.
    fn stop_reason(&mut self, st: u64) -> JResult<SingleThreadStopReason<u32>> {
        let cause = (st >> 4) & 7;
        Ok(match cause {
            CAUSE_STEP => SingleThreadStopReason::DoneStep,
            CAUSE_BP => SingleThreadStopReason::HwBreak(()),
            CAUSE_WATCH => {
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
            CAUSE_TRAP => {
                let pc = self.board.read_reg(REG_PC)? as u32;
                if self.sw_breakpoints.contains_key(&pc) {
                    SingleThreadStopReason::SwBreak(())
                } else {
                    SingleThreadStopReason::Signal(Signal::SIGTRAP)
                }
            }
            CAUSE_ILL => SingleThreadStopReason::Signal(Signal::SIGILL),
            CAUSE_HALT => {
                self.exited = true;
                SingleThreadStopReason::Exited((self.board.read_reg(12)? & 0xFF) as u8)
            }
            CAUSE_REQ => SingleThreadStopReason::Signal(Signal::SIGINT),
            _ => SingleThreadStopReason::Signal(Signal::SIGTRAP),
        })
    }

    fn poll(&mut self) -> JResult<Option<SingleThreadStopReason<u32>>> {
        if self.skipped_trap_step {
            self.skipped_trap_step = false;
            self.running = false;
            return Ok(Some(SingleThreadStopReason::DoneStep));
        }
        let st = self.board.status()?;
        self.halt_seen |= st & ST_HALTED_SEEN != 0;
        if st & ST_HALTED != 0 && self.halt_seen {
            self.running = false;
            return self.stop_reason(st).map(Some);
        }
        Ok(None)
    }

    fn run(&mut self, mut incoming_data: impl FnMut() -> bool) -> JResult<RunEvent> {
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

    fn interrupt(&mut self) -> JResult<SingleThreadStopReason<u32>> {
        self.board.control(CTL_ATTACHED | CTL_HALTREQ)?;
        let st = self.board.wait_halted(Duration::from_secs(2))?;
        self.running = false;
        // a breakpoint/step may have won the race against the request
        self.stop_reason(st)
    }

    fn write_bp(&mut self, slot: usize) -> JResult<()> {
        let v = self.hw_breakpoints[slot].map_or(0, |a| u64::from(a) | (1 << 32));
        self.board.batch(&[DmiReq::Write(BP0 + slot as u8, v)]).map(|_| ())
    }

    fn write_wp(&mut self, slot: usize) -> JResult<()> {
        let v = self.watches[slot].map_or(0, |w| {
            let mut lanes = 0_u64;
            for a in w.addr..w.addr + w.len.max(1) {
                lanes |= 1 << (a & 7);
            }
            let (rd, wr) = match w.kind {
                WatchKind::Write => (0, 1),
                WatchKind::Read => (1, 0),
                WatchKind::ReadWrite => (1, 1),
            };
            u64::from(w.addr & !7) | (1 << 32) | (rd << 33) | (wr << 34) | (lanes << 40)
        });
        self.board.batch(&[DmiReq::Write(WP0 + slot as u8, v)]).map(|_| ())
    }

    /// Undo everything the session set and let the program run on.
    fn detach(&mut self) -> JResult<()> {
        if self.running {
            self.interrupt()?;
        }
        let bps: Vec<(u32, u32)> = self.sw_breakpoints.iter().map(|(a, w)| (*a, *w)).collect();
        for (addr, orig) in bps {
            self.board.write_mem(addr, &orig.to_le_bytes())?;
        }
        self.sw_breakpoints.clear();
        for slot in 0..HW_BREAKPOINTS {
            self.hw_breakpoints[slot] = None;
            self.write_bp(slot)?;
        }
        for slot in 0..HW_WATCHPOINTS {
            self.watches[slot] = None;
            self.write_wp(slot)?;
        }
        // a program that executed HALT stays halted (resuming would run on
        // past its end)
        let at_halt = self.exited || (self.board.status()? >> 4) & 7 == CAUSE_HALT;
        self.board.control(if at_halt { 0 } else { CTL_RESUMEREQ })
    }
}

fn terr(e: &str) -> TargetError<&'static str> {
    eprintln!("klausscc gdbserver: {e}");
    TargetError::NonFatal
}

impl Target for BoardTarget {
    type Arch = KlaussCpuArch;
    type Error = &'static str;

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

impl MonitorCmd for BoardTarget {
    fn handle_monitor_cmd(&mut self, cmd: &[u8], mut out: ConsoleOutput<'_>) -> Result<(), Self::Error> {
        match std::str::from_utf8(cmd).map(str::trim) {
            Ok("mmio on") => {
                self.allow_mmio = true;
                outputln!(out, "MMIO access enabled: device reads have side effects (e.g. UART RX pops)");
            }
            Ok("mmio off") => {
                self.allow_mmio = false;
                outputln!(out, "MMIO access disabled");
            }
            Ok("mmio") => outputln!(out, "MMIO access is {}", if self.allow_mmio { "on" } else { "off" }),
            Ok("status") => match self.board.status() {
                Ok(st) => outputln!(
                    out,
                    "DMSTATUS 0x{st:08X}: halted {} cause {} park_kind {}",
                    st & 1,
                    (st >> 4) & 7,
                    (st >> 8) & 7
                ),
                Err(e) => outputln!(out, "DMSTATUS read failed: {e}"),
            },
            _ => {
                outputln!(out, "klausscc gdbserver (board) monitor commands:");
                outputln!(out, "  mmio on|off   allow debugger reads/writes of 0xF000_0000+ (default off)");
                outputln!(out, "  status        debug module status");
            }
        }
        Ok(())
    }
}

impl SingleThreadBase for BoardTarget {
    fn read_registers(&mut self, regs: &mut KlaussRegs) -> TargetResult<(), Self> {
        let v = self.board.read_regs().map_err(|e| terr(&e))?;
        regs.r.copy_from_slice(&v[..16]);
        regs.sp = v[16] as u32;
        regs.pc = v[17] as u32;
        regs.flags = v[18] as u32;
        regs.int_mask = v[19] as u32;
        Ok(())
    }

    fn write_registers(&mut self, regs: &KlaussRegs) -> TargetResult<(), Self> {
        let mut vals: Vec<u64> = regs.r.to_vec();
        vals.extend([regs.sp, regs.pc, regs.flags, regs.int_mask].map(u64::from));
        for (i, v) in vals.into_iter().enumerate() {
            self.board.write_reg(i as u64, v).map_err(|e| terr(&e))?;
        }
        Ok(())
    }

    fn support_single_register_access(&mut self) -> Option<SingleRegisterAccessOps<'_, (), Self>> {
        Some(self)
    }

    fn read_addrs(&mut self, start_addr: u32, data: &mut [u8]) -> TargetResult<usize, Self> {
        if !self.mem_allowed(start_addr, data.len()) {
            return Err(TargetError::Errno(14)); // EFAULT
        }
        self.board.read_mem(start_addr, data).map_err(|e| terr(&e))?;
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
        if !self.mem_allowed(start_addr, data.len()) {
            return Err(TargetError::Errno(14));
        }
        self.board.write_mem(start_addr, data).map_err(|e| terr(&e))
    }

    fn support_resume(&mut self) -> Option<SingleThreadResumeOps<'_, Self>> {
        Some(self)
    }
}

impl SingleRegisterAccess<()> for BoardTarget {
    fn read_register(&mut self, _tid: (), reg_id: KlaussRegId, buf: &mut [u8]) -> TargetResult<usize, Self> {
        let (idx, size) = reg_index(reg_id);
        let v = self.board.read_reg(idx).map_err(|e| terr(&e))?;
        let bytes = v.to_le_bytes();
        let n = size.min(buf.len());
        buf[..n].copy_from_slice(&bytes[..n]);
        Ok(n)
    }

    fn write_register(&mut self, _tid: (), reg_id: KlaussRegId, val: &[u8]) -> TargetResult<(), Self> {
        let (idx, size) = reg_index(reg_id);
        if val.len() != size {
            return Err(TargetError::NonFatal);
        }
        let mut b = [0_u8; 8];
        b[..size].copy_from_slice(val);
        self.board.write_reg(idx, u64::from_le_bytes(b)).map_err(|e| terr(&e))
    }
}

/// `REG_CMD` index and size in bytes of a register.
const fn reg_index(reg: KlaussRegId) -> (u64, usize) {
    match reg {
        KlaussRegId::Gpr(n) => (n as u64, 8),
        KlaussRegId::Sp => (16, 4),
        KlaussRegId::Pc => (17, 4),
        KlaussRegId::Flags => (18, 4),
        KlaussRegId::IntMask => (19, 4),
    }
}

impl SingleThreadResume for BoardTarget {
    fn resume(&mut self, signal: Option<Signal>) -> Result<(), Self::Error> {
        if signal.is_some() {
            return Err("no support for continuing with a signal");
        }
        self.start(false).map_err(|e| {
            eprintln!("klausscc gdbserver: resume: {e}");
            "resume failed"
        })
    }

    fn support_single_step(&mut self) -> Option<SingleThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}

impl SingleThreadSingleStep for BoardTarget {
    fn step(&mut self, signal: Option<Signal>) -> Result<(), Self::Error> {
        if signal.is_some() {
            return Err("no support for stepping with a signal");
        }
        self.start(true).map_err(|e| {
            eprintln!("klausscc gdbserver: step: {e}");
            "step failed"
        })
    }
}

impl Breakpoints for BoardTarget {
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

impl SwBreakpoint for BoardTarget {
    fn add_sw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        if addr & 3 != 0 || is_mmio(addr) {
            return Ok(false);
        }
        if self.sw_breakpoints.contains_key(&addr) {
            return Ok(true);
        }
        let orig = self.read_word(addr).map_err(|e| terr(&e))?;
        self.board.write_mem(addr, &TRAP_WORD.to_le_bytes()).map_err(|e| terr(&e))?;
        self.sw_breakpoints.insert(addr, orig);
        Ok(true)
    }

    fn remove_sw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        let Some(orig) = self.sw_breakpoints.remove(&addr) else {
            return Ok(false);
        };
        self.board.write_mem(addr, &orig.to_le_bytes()).map_err(|e| terr(&e))?;
        Ok(true)
    }
}

impl HwBreakpoint for BoardTarget {
    fn add_hw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        if self.hw_breakpoints.contains(&Some(addr)) {
            return Ok(true);
        }
        let Some(slot) = self.hw_breakpoints.iter().position(Option::is_none) else {
            return Ok(false);
        };
        self.hw_breakpoints[slot] = Some(addr);
        self.write_bp(slot).map_err(|e| terr(&e))?;
        Ok(true)
    }

    fn remove_hw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        let Some(slot) = self.hw_breakpoints.iter().position(|b| *b == Some(addr)) else {
            return Ok(false);
        };
        self.hw_breakpoints[slot] = None;
        self.write_bp(slot).map_err(|e| terr(&e))?;
        Ok(true)
    }
}

impl HwWatchpoint for BoardTarget {
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
        self.write_wp(slot).map_err(|e| terr(&e))?;
        Ok(true)
    }

    fn remove_hw_watchpoint(&mut self, addr: u32, len: u32, kind: WatchKind) -> TargetResult<bool, Self> {
        let w = Wp { addr, len, kind };
        let Some(slot) = self.watches.iter().position(|s| *s == Some(w)) else {
            return Ok(false);
        };
        self.watches[slot] = None;
        self.write_wp(slot).map_err(|e| terr(&e))?;
        Ok(true)
    }
}

impl HostInfo for BoardTarget {
    fn host_info(&self, write_item: &mut dyn FnMut(&HostInfoResponse<'_>)) -> Result<(), Self::Error> {
        write_item(&HostInfoResponse::Triple(TRIPLE));
        write_item(&HostInfoResponse::Endianness(Endianness::Little));
        write_item(&HostInfoResponse::PointerSize(4));
        Ok(())
    }
}

impl ProcessInfo for BoardTarget {
    fn process_info(&self, write_item: &mut dyn FnMut(&ProcessInfoResponse<'_>)) -> Result<(), Self::Error> {
        if let Some(pid) = Pid::new(1) {
            write_item(&ProcessInfoResponse::Pid(pid));
        }
        write_item(&ProcessInfoResponse::Triple(TRIPLE));
        write_item(&ProcessInfoResponse::Endianness(Endianness::Little));
        write_item(&ProcessInfoResponse::PointerSize(4));
        Ok(())
    }
}

// ---- event loop -------------------------------------------------------------

enum BoardEventLoop {}

impl run_blocking::BlockingEventLoop for BoardEventLoop {
    type Target = BoardTarget;
    type Connection = Box<dyn ConnectionExt<Error = std::io::Error>>;
    type StopReason = SingleThreadStopReason<u32>;

    fn wait_for_stop_reason(
        target: &mut BoardTarget,
        conn: &mut Self::Connection,
    ) -> Result<run_blocking::Event<Self::StopReason>, run_blocking::WaitForStopReasonError<&'static str, std::io::Error>> {
        match target.run(|| conn.peek().map_or(true, |b| b.is_some())) {
            Ok(RunEvent::IncomingData) => {
                let byte = conn.read().map_err(run_blocking::WaitForStopReasonError::Connection)?;
                Ok(run_blocking::Event::IncomingData(byte))
            }
            Ok(RunEvent::Stop(reason)) => Ok(run_blocking::Event::TargetStopped(reason)),
            Err(e) => {
                eprintln!("klausscc gdbserver: {e}");
                Err(run_blocking::WaitForStopReasonError::Target("lost the debug module"))
            }
        }
    }

    fn on_interrupt(target: &mut BoardTarget) -> Result<Option<Self::StopReason>, &'static str> {
        target.interrupt().map(Some).map_err(|e| {
            eprintln!("klausscc gdbserver: interrupt: {e}");
            "halt request failed"
        })
    }
}

/// Bring the core to a debug halt for a new session.
///
/// With `load`, `load()` sends the program over the UART with halt-on-load
/// set, and the core halts on the program's first instruction; otherwise the
/// running program is halted where it is.
pub fn attach(board: &mut Board, load: Option<&mut dyn FnMut() -> Result<(), String>>) -> JResult<u32> {
    // start clean: a server that was killed leaves its slots armed
    let clear: Vec<DmiReq> = (0..HW_BREAKPOINTS as u8)
        .map(|i| DmiReq::Write(BP0 + i, 0))
        .chain((0..HW_WATCHPOINTS as u8).map(|i| DmiReq::Write(WP0 + i, 0)))
        .chain(std::iter::once(DmiReq::Write(DMSTATUS, 0)))
        .collect();
    board.batch(&clear)?;
    if let Some(load) = load {
        board.control(CTL_ATTACHED | CTL_HALT_ON_LOAD)?;
        board.status()?;
        load()?;
        let st = board.wait_halted(Duration::from_secs(60))?;
        board.control(CTL_ATTACHED)?;
        if (st >> 4) & 7 != CAUSE_REQ {
            return Err(format!("the core halted for cause {} instead of at the entry", (st >> 4) & 7));
        }
    } else {
        let st = board.status()?;
        if st & ST_HALTED == 0 {
            board.control(CTL_ATTACHED | CTL_HALTREQ)?;
            board
                .wait_halted(Duration::from_secs(2))
                .map_err(|e| format!("{e}: is a program running on the board? (load one with -s)"))?;
        }
        board.control(CTL_ATTACHED)?;
    }
    Ok(board.read_reg(REG_PC)? as u32)
}

/// Serve one debugging session for the halted `board` on `127.0.0.1:port`.
pub fn serve(board: Board, port: u16) -> Result<String, String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("cannot listen on port {port}: {e}"))?;
    eprintln!("klausscc gdbserver: waiting for a debugger on 127.0.0.1:{port}");
    let (stream, peer) = listener.accept().map_err(|e| format!("accept failed: {e}"))?;
    eprintln!("klausscc gdbserver: debugger connected from {peer}");
    let _ = stream.set_nodelay(true);
    let conn: Box<dyn ConnectionExt<Error = std::io::Error>> = Box::new(stream);

    let mut target = BoardTarget::new(board);
    let result = GdbStub::new(conn).run_blocking::<BoardEventLoop>(&mut target);
    let summary = match result {
        Ok(DisconnectReason::Disconnect) => Ok("debugger detached; the program runs on".to_owned()),
        Ok(DisconnectReason::TargetExited(code)) => Ok(format!("program halted (exit status {code})")),
        Ok(DisconnectReason::TargetTerminated(sig)) => Ok(format!("program terminated with {sig}")),
        Ok(DisconnectReason::Kill) => Ok("debugger killed the session".to_owned()),
        Err(e) => Err(format!("gdbstub error: {e}")),
    };
    if let Err(e) = target.detach() {
        eprintln!("klausscc gdbserver: detach: {e}");
    }
    summary
}
