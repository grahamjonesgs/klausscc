//! GDB remote-protocol server for the emulator (`--gdbserver PORT`).
//!
//! Serves the built-in ISA emulator to lldb (the KlaussCPU-aware build from
//! `klausscpu-llvm`, via `gdb-remote` or VS Code's lldb-dap) using the
//! `gdbstub` crate.  See `klaussCPU/DEBUG_PLAN.md` (Phase 2).
//!
//! The register layout and target description are in [`arch`](super::arch).
//!
//! Breakpoints and watchpoints are emulated (code is never patched): before
//! each instruction the PC is checked against the breakpoint set, and every
//! data access is checked against the watchpoints, which report after the
//! accessing instruction.  Like the planned board debug module there are 4
//! hardware breakpoints and 2 watchpoints; software breakpoints are unlimited.
//!
//! Stops: HALT reports "exited" with the low byte of r12 (main's return value
//! once `crt0` halts); a TRAP (`__builtin_debugtrap`) reports SIGTRAP with the
//! PC on the TRAP, and resuming skips it; an invalid opcode reports SIGILL and
//! a PC outside RAM SIGSEGV, and a `WAIT` nothing can wake SIGSTOP.
//! Continuing takes interrupts as they become ready; a single-step never does
//! (like the planned board debug module), so stepping does not fall into a
//! handler — a pending interrupt is taken on the next continue, and stepping
//! a `WAIT` completes it as if woken.  UART output streams to stdout and stdin feeds the
//! UART receive FIFO.  The program starts with the UART transmitter idle, as
//! under `--emulate`.  A `TX_DATA` store made while the transmitter is busy is
//! dropped, as on the board, and reported on stderr.
//! The debugger cannot read or write MMIO (`0xF000_0000+`)
//! unless `monitor mmio on` is given: some device reads have side effects,
//! e.g. popping the UART RX FIFO, and debuggers read memory freely.
//!
//! MMU stage 1 (`MMU_PLAN.md` §4.4, §6.2): registers 20 (`sp_alt`) and 21
//! (`mode`: UM in bit 0, DI in bit 1, as SR `STATUS`) are served, and as on
//! the board an illegal instruction, TRAP or misaligned load or store stops
//! in the debugger even when the program has an exception vector, unless
//! `monitor exc_pass on` lets them through to it (a misaligned fetch never
//! stops there).  `monitor sr` shows the system registers.
//!
//! MMU stages 2 and 3 (`MMU_PLAN.md` §10-§11): `monitor mpu` shows the MPU
//! regions (on a CPU with the MPU, `--mpu`; the Nexys A7 has none) and
//! `monitor tlb` the TLB.  While translation is on
//! (`STATUS.TE` = 1) the debugger's memory addresses are virtual, as the
//! program's are: each is translated as a supervisor data access would be
//! (kseg direct, MMIO identity, useg and kmap through the TLB in the
//! current ASID, without the entry's R/W/X check), and an address that
//! does not translate is an error, not a physical access.  `monitor phys
//! on` makes them physical again.  The board's server is always physical,
//! because its debug module reads physical memory.  A double fault (an
//! entry whose frame push has no kseg address) reports SIGSEGV.

use super::arch::{HW_BREAKPOINTS, HW_WATCHPOINTS, KlaussCpuArch, KlaussRegId, KlaussRegs, MODE_DI, MODE_UM, TRAP_WORD};
use super::{GdbError, RunEvent, Session, accept, mmio_monitor_cmd, run_session, session_summary};
use crate::emu::{Access, CYCLES_PER_MS, Cpu, DebugRegs, StopReason, Watch, WatchKind as EmuWatchKind};
use crate::layout::is_mmio;
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
use std::io::{Read as _, Write as _};
use std::sync::mpsc;

/// Instructions run between checks for debugger input (Ctrl-C) and UART I/O.
const POLL_INTERVAL: u32 = 4096;

/// What the debugger last asked the emulator to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecMode {
    /// Run one instruction.
    Step {
        /// Resuming skipped a compiled-in TRAP, which was the step.
        skipped_trap: bool,
    },
    /// Run until something stops it.
    Continue,
}

/// The emulator plus the debugger's view of it.
pub(crate) struct EmuTarget {
    cpu: Cpu,
    exec: ExecMode,
    sw_breakpoints: Vec<u32>,
    hw_breakpoints: Vec<u32>,
    /// Set by resume/step: don't stop on a breakpoint at the resume PC.
    resuming: bool,
    /// `monitor mmio on`: let the debugger access device registers.
    allow_mmio: bool,
    /// `monitor phys on`: memory addresses are physical even with `TE` = 1.
    physical: bool,
    uart_rx: mpsc::Receiver<u8>,
}

impl EmuTarget {
    fn new(mut cpu: Cpu, uart_rx: mpsc::Receiver<u8>) -> Self {
        cpu.set_debugger_attached(true);
        Self {
            cpu,
            exec: ExecMode::Continue,
            sw_breakpoints: Vec::new(),
            hw_breakpoints: Vec::new(),
            resuming: false,
            allow_mmio: false,
            physical: false,
            uart_rx,
        }
    }

    /// Prepare to run from the current PC: step past a compiled-in TRAP the
    /// last stop reported, and let the emulator continue after a fault stop.
    /// Returns whether a TRAP was skipped.
    fn prepare_resume(&mut self) -> bool {
        let mut regs = self.cpu.debug_regs();
        let skipped_trap = self.cpu.fetch_word(regs.pc) == TRAP_WORD;
        if skipped_trap {
            regs.pc = regs.pc.wrapping_add(4);
            self.cpu.set_debug_regs(&regs);
        }
        self.cpu.clear_stop();
        self.resuming = true;
        skipped_trap
    }

    /// Stream UART output to stdout and feed stdin into the UART RX FIFO.
    fn pump_uart(&mut self) {
        let out = self.cpu.take_uart();
        if !out.is_empty() {
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(&out);
            let _ = stdout.flush();
        }
        let rx: Vec<u8> = self.uart_rx.try_iter().collect();
        if !rx.is_empty() {
            self.cpu.feed_uart_rx(&rx);
        }
        let busy = self.cpu.take_tx_busy_writes();
        if !busy.is_empty() {
            eprintln!("klausscc gdbserver: {busy}");
        }
    }

    /// Advance one step (an instruction, or an interrupt entry when
    /// `allow_irq`), checking breakpoints first and watchpoints after.
    /// Returns the stop reason, or `None` to keep going.
    fn step_instruction(&mut self, allow_irq: bool) -> Option<SingleThreadStopReason<u32>> {
        let pc = self.cpu.debug_regs().pc;
        if !self.resuming {
            if self.hw_breakpoints.contains(&pc) {
                return Some(SingleThreadStopReason::HwBreak(()));
            }
            if self.sw_breakpoints.contains(&pc) {
                return Some(SingleThreadStopReason::SwBreak(()));
            }
        }
        self.resuming = false;
        let stop = self.cpu.step_one(allow_irq);
        if let Some(hit) = self.cpu.take_watch_hit() {
            return Some(SingleThreadStopReason::Watch {
                tid: (),
                kind: match hit.access {
                    Access::Load => WatchKind::Read,
                    Access::Store => WatchKind::Write,
                },
                addr: hit.addr,
            });
        }
        stop.map(|s| match s {
            StopReason::Halt => SingleThreadStopReason::Exited((self.cpu.debug_regs().r[12] & 0xFF) as u8),
            StopReason::Trap => SingleThreadStopReason::Signal(Signal::SIGTRAP),
            StopReason::InvalidOpcode(_) => SingleThreadStopReason::Signal(Signal::SIGILL),
            // The PC stays on the access, as on the board.
            StopReason::Misaligned(_) => SingleThreadStopReason::Signal(Signal::SIGBUS),
            StopReason::PcOutOfRange(_) => SingleThreadStopReason::Signal(Signal::SIGSEGV),
            StopReason::DoubleFault(sp) => {
                eprintln!("klausscc gdbserver: double fault: the kernel SP 0x{sp:08X} is not in kseg (translation on)");
                SingleThreadStopReason::Signal(Signal::SIGSEGV)
            }
            StopReason::InstructionCap => SingleThreadStopReason::Signal(Signal::SIGXCPU),
            // Scripted input is an --emulate feature; never set under the debugger.
            StopReason::InputDone => SingleThreadStopReason::Signal(Signal::SIGSTOP),
            StopReason::WaitForever(pc) => {
                eprintln!("klausscc gdbserver: WAIT at 0x{pc:08X} can never be woken (no unmasked, vectored interrupt)");
                SingleThreadStopReason::Signal(Signal::SIGSTOP)
            }
        })
    }

    fn run(&mut self, mut incoming_data: impl FnMut() -> bool) -> RunEvent {
        if let ExecMode::Step { skipped_trap } = self.exec {
            let stop = if skipped_trap {
                None // skipping the TRAP was the step
            } else {
                self.step_instruction(false) // single-steps never take interrupts
            };
            self.pump_uart();
            return RunEvent::Stop(stop.unwrap_or(SingleThreadStopReason::DoneStep));
        }
        loop {
            for _ in 0..POLL_INTERVAL {
                if let Some(stop) = self.step_instruction(true) {
                    self.pump_uart();
                    return RunEvent::Stop(stop);
                }
            }
            self.pump_uart();
            if incoming_data() {
                return RunEvent::IncomingData;
            }
        }
    }
}

impl Target for EmuTarget {
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

impl MonitorCmd for EmuTarget {
    fn handle_monitor_cmd(&mut self, cmd: &[u8], mut out: ConsoleOutput<'_>) -> Result<(), Self::Error> {
        // Not UTF-8: no command, so the help.
        self.monitor(std::str::from_utf8(cmd).map_or("", str::trim), &mut out);
        Ok(())
    }
}

impl EmuTarget {
    /// Run the monitor command `cmd`, writing its reply to `out`.
    fn monitor(&mut self, cmd: &str, out: &mut impl std::fmt::Write) {
        match cmd {
            "irq" => {
                let st = self.cpu.irq_state();
                outputln!(
                    out,
                    "INT_MASK   0x{:X}   INT_PENDING 0x{:X}{}",
                    st.mask,
                    st.pending,
                    if st.waiting { "   (in WAIT)" } else { "" }
                );
                for (n, v) in st.vectors.iter().enumerate() {
                    outputln!(out, "INT_VEC{n}   0x{v:08X}");
                }
                outputln!(
                    out,
                    "TIMER      period {} count {}   cycles {} (~{} ms)",
                    st.timer_period,
                    st.timer_count,
                    st.cycles,
                    st.cycles / CYCLES_PER_MS
                );
            }
            "sr" => {
                let s = self.cpu.system_regs();
                // MPU_EN only on a CPU with the MPU (--mpu).
                let mpu_en = if self.cpu.has_mpu() {
                    format!("   MPU_EN {}", u8::from(s.mpu_en))
                } else {
                    String::new()
                };
                outputln!(
                    out,
                    "mode {}   DI {}{mpu_en}   TE {}   SP_ALT 0x{:08X}",
                    if s.um { "user" } else { "supervisor" },
                    u8::from(s.irq_disabled),
                    u8::from(s.te),
                    s.sp_alt
                );
                outputln!(
                    out,
                    "EVEC 0x{:08X}   ECAUSE {}{}   EADDR 0x{:08X}",
                    s.evec,
                    s.ecause & 0xFF,
                    if s.ecause & 0x100 != 0 { " GUARD" } else { "" },
                    s.eaddr
                );
                outputln!(out, "SCRATCH0 0x{:016X}   SCRATCH1 0x{:016X}", s.scratch[0], s.scratch[1]);
                outputln!(out, "TP 0x{:016X}   CYCLE {}", s.tp, s.cycle);
                if let Some(addr) = s.reservation {
                    outputln!(out, "LR/SC reservation 0x{addr:08X}");
                } else {
                    outputln!(out, "LR/SC reservation none");
                }
                outputln!(
                    out,
                    "TLB_HI 0x{:08X}   TLB_LO 0x{:08X}   TLB_INDEX 0x{:08X}   ASID {}   TLBVEC 0x{:08X}",
                    s.tlb_hi,
                    s.tlb_lo,
                    s.tlb_index,
                    s.asid,
                    s.tlbvec
                );
                if let Some((cause, addr)) = self.cpu.parked_exception() {
                    outputln!(out, "last stop on an exception: {cause:?} (ECAUSE {}) at 0x{addr:08X}", cause as u8);
                }
            }
            "mpu" if !self.cpu.has_mpu() => {
                outputln!(out, "no MPU (as on the Nexys A7; klausscc --mpu gives the emulator one)");
            }
            "mpu" => {
                let s = self.cpu.system_regs();
                outputln!(
                    out,
                    "MPU {} (STATUS.MPU_EN){}",
                    if s.mpu_en { "on" } else { "off" },
                    if s.te { "; ignored while TE = 1" } else { "" }
                );
                let mut any = false;
                for (n, r) in self.cpu.mpu_regions().iter().enumerate() {
                    if r.enabled() {
                        any = true;
                        outputln!(
                            out,
                            "  {n:2}  0x{:08X}-0x{:08X}  {}",
                            r.first(),
                            r.last(),
                            if r.guard() { "GUARD".to_owned() } else { r.permissions() }
                        );
                    } else if r.base != 0 || r.limit != 0 {
                        outputln!(out, "  {n:2}  (disabled: MPU_BASE 0x{:08X} MPU_LIMIT 0x{:08X})", r.base, r.limit);
                    }
                }
                if !any {
                    outputln!(out, "  no region enabled");
                }
            }
            "tlb" => {
                let s = self.cpu.system_regs();
                outputln!(
                    out,
                    "translation {} (STATUS.TE)   ASID {}   TLB_INDEX 0x{:08X}",
                    if s.te { "on" } else { "off" },
                    s.asid,
                    s.tlb_index
                );
                let mut any = false;
                for (n, e) in self.cpu.tlb_entries().iter().enumerate() {
                    if e.valid() {
                        any = true;
                        outputln!(
                            out,
                            "  {n:2}  VA 0x{:04X}xxxx -> PA 0x{:04X}xxxx  {}  {}",
                            e.vpn(),
                            e.pfn(),
                            e.permissions(),
                            if e.global() { "global".to_owned() } else { format!("ASID {}", e.asid()) }
                        );
                    }
                }
                if !any {
                    outputln!(out, "  no valid entry");
                }
            }
            "phys" => outputln!(
                out,
                "memory addresses are {}",
                if self.physical || !self.cpu.translation_on() {
                    "physical"
                } else {
                    "virtual (TE = 1)"
                }
            ),
            "phys on" => {
                self.physical = true;
                outputln!(out, "memory addresses are physical");
            }
            "phys off" => {
                self.physical = false;
                outputln!(out, "memory addresses are virtual while TE = 1");
            }
            "exc_pass" => outputln!(out, "EXC_PASS is {}", if self.cpu.exc_pass() { "on" } else { "off" }),
            "exc_pass on" | "exc_pass off" => {
                let on = cmd.ends_with("on");
                self.cpu.set_exc_pass(on);
                if on {
                    outputln!(
                        out,
                        "EXC_PASS on: illegal instructions, TRAP and misaligned accesses go to the program's EVEC"
                    );
                } else {
                    outputln!(
                        out,
                        "EXC_PASS off: illegal instructions, TRAP and misaligned accesses stop in the debugger"
                    );
                }
            }
            _ if mmio_monitor_cmd(cmd, &mut self.allow_mmio, out) => {}
            _ => {
                outputln!(out, "klausscc gdbserver monitor commands:");
                outputln!(out, "  mmio on|off       allow debugger reads/writes of 0xF000_0000+ (default off)");
                outputln!(out, "  irq               interrupt controller and timer state");
                outputln!(out, "  sr                MMU system registers and mode");
                outputln!(out, "  mpu               MPU regions (stage 2, with --mpu)");
                outputln!(out, "  tlb               TLB entries (stage 3)");
                outputln!(
                    out,
                    "  phys on|off       physical memory addresses even while TE = 1 (default off: virtual)"
                );
                outputln!(
                    out,
                    "  exc_pass on|off   let illegal/TRAP/misaligned go to EVEC instead of the debugger (default off)"
                );
            }
        }
    }
}

impl SingleThreadBase for EmuTarget {
    fn read_registers(&mut self, regs: &mut KlaussRegs) -> TargetResult<(), Self> {
        *regs = self.regs();
        Ok(())
    }

    fn write_registers(&mut self, regs: &KlaussRegs) -> TargetResult<(), Self> {
        self.set_regs(regs);
        Ok(())
    }

    fn support_single_register_access(&mut self) -> Option<SingleRegisterAccessOps<'_, (), Self>> {
        Some(self)
    }

    fn read_addrs(&mut self, start_addr: u32, data: &mut [u8]) -> TargetResult<usize, Self> {
        // MMIO is identity whether or not translation is on.
        let n = if self.allow_mmio && is_mmio(start_addr) {
            self.cpu.debug_read_mmio(start_addr, data)
        } else if self.virtual_addresses() {
            self.cpu.debug_read_virtual(start_addr, data)
        } else {
            self.cpu.debug_read(start_addr, data)
        };
        if n == 0 && !data.is_empty() {
            return Err(TargetError::NonFatal); // MMIO, outside RAM or not mapped
        }
        Ok(n)
    }

    fn write_addrs(&mut self, start_addr: u32, data: &[u8]) -> TargetResult<(), Self> {
        let ok = if self.allow_mmio && is_mmio(start_addr) {
            self.cpu.debug_write_mmio(start_addr, data)
        } else if self.virtual_addresses() {
            self.cpu.debug_write_virtual(start_addr, data)
        } else {
            self.cpu.debug_write(start_addr, data)
        };
        if ok { Ok(()) } else { Err(TargetError::NonFatal) }
    }

    fn support_resume(&mut self) -> Option<SingleThreadResumeOps<'_, Self>> {
        Some(self)
    }
}

impl SingleRegisterAccess<()> for EmuTarget {
    fn read_register(&mut self, _tid: (), reg_id: KlaussRegId, buf: &mut [u8]) -> TargetResult<usize, Self> {
        Ok(reg_id.encode(self.regs().get(reg_id), buf))
    }

    fn write_register(&mut self, _tid: (), reg_id: KlaussRegId, val: &[u8]) -> TargetResult<(), Self> {
        let value = reg_id.decode(val).ok_or(TargetError::NonFatal)?;
        let mut regs = self.regs();
        regs.set(reg_id, value);
        self.set_regs(&regs);
        Ok(())
    }
}

impl EmuTarget {
    /// Whether the debugger's memory addresses are virtual: translation is
    /// on and `monitor phys on` was not given.
    const fn virtual_addresses(&self) -> bool {
        self.cpu.translation_on() && !self.physical
    }

    /// Every register, the MMU stage 1 ones included.
    fn regs(&self) -> KlaussRegs {
        let d = self.cpu.debug_regs();
        let s = self.cpu.system_regs();
        KlaussRegs {
            r: d.r,
            sp: d.sp,
            pc: d.pc,
            flags: u32::from(d.flags),
            int_mask: u32::from(d.int_mask),
            sp_alt: s.sp_alt,
            mode: if s.um { MODE_UM } else { 0 } | if s.irq_disabled { MODE_DI } else { 0 },
            stage1: true,
        }
    }

    /// Set the registers; registers 20 and 21 only when `regs` has them.
    fn set_regs(&mut self, regs: &KlaussRegs) {
        self.cpu.set_debug_regs(&DebugRegs::from(regs));
        if regs.stage1 {
            self.cpu.set_mode_regs(regs.sp_alt, regs.mode & MODE_UM != 0, regs.mode & MODE_DI != 0);
        }
    }
}

/// The emulator keeps the low nibble of flags and `int_mask`.
impl From<&KlaussRegs> for DebugRegs {
    fn from(regs: &KlaussRegs) -> Self {
        Self {
            r: regs.r,
            sp: regs.sp,
            pc: regs.pc,
            flags: (regs.flags & 0xF) as u8,
            int_mask: (regs.int_mask & 0xF) as u8,
        }
    }
}

impl SingleThreadResume for EmuTarget {
    fn resume(&mut self, signal: Option<Signal>) -> Result<(), Self::Error> {
        if signal.is_some() {
            return Err("KlaussCPU cannot resume with a signal");
        }
        // a skipped TRAP just means the run starts after it
        self.prepare_resume();
        self.exec = ExecMode::Continue;
        Ok(())
    }

    fn support_single_step(&mut self) -> Option<SingleThreadSingleStepOps<'_, Self>> {
        Some(self)
    }
}

impl SingleThreadSingleStep for EmuTarget {
    fn step(&mut self, signal: Option<Signal>) -> Result<(), Self::Error> {
        if signal.is_some() {
            return Err("KlaussCPU cannot step with a signal");
        }
        let skipped_trap = self.prepare_resume();
        self.exec = ExecMode::Step { skipped_trap };
        Ok(())
    }
}

impl Breakpoints for EmuTarget {
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

impl SwBreakpoint for EmuTarget {
    fn add_sw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        if !self.sw_breakpoints.contains(&addr) {
            self.sw_breakpoints.push(addr);
        }
        Ok(true)
    }

    fn remove_sw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        let before = self.sw_breakpoints.len();
        self.sw_breakpoints.retain(|a| *a != addr);
        Ok(self.sw_breakpoints.len() != before)
    }
}

impl HwBreakpoint for EmuTarget {
    fn add_hw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        if self.hw_breakpoints.contains(&addr) {
            return Ok(true);
        }
        if self.hw_breakpoints.len() >= HW_BREAKPOINTS {
            return Ok(false); // out of comparators
        }
        self.hw_breakpoints.push(addr);
        Ok(true)
    }

    fn remove_hw_breakpoint(&mut self, addr: u32, _kind: usize) -> TargetResult<bool, Self> {
        let before = self.hw_breakpoints.len();
        self.hw_breakpoints.retain(|a| *a != addr);
        Ok(self.hw_breakpoints.len() != before)
    }
}

const fn emu_watch_kind(kind: WatchKind) -> EmuWatchKind {
    match kind {
        WatchKind::Read => EmuWatchKind::Read,
        WatchKind::Write => EmuWatchKind::Write,
        WatchKind::ReadWrite => EmuWatchKind::Access,
    }
}

impl HwWatchpoint for EmuTarget {
    fn add_hw_watchpoint(&mut self, addr: u32, len: u32, kind: WatchKind) -> TargetResult<bool, Self> {
        if self.cpu.watches().len() >= HW_WATCHPOINTS {
            return Ok(false);
        }
        self.cpu.add_watch(Watch {
            addr,
            len,
            kind: emu_watch_kind(kind),
        });
        Ok(true)
    }

    fn remove_hw_watchpoint(&mut self, addr: u32, len: u32, kind: WatchKind) -> TargetResult<bool, Self> {
        Ok(self.cpu.remove_watch(&Watch {
            addr,
            len,
            kind: emu_watch_kind(kind),
        }))
    }
}

impl_host_process_info!(EmuTarget);

impl Session for EmuTarget {
    fn run_until_event(&mut self, incoming_data: impl FnMut() -> bool) -> Result<RunEvent, Self::Error> {
        Ok(self.run(incoming_data))
    }

    fn on_interrupt(&mut self) -> Result<Option<SingleThreadStopReason<u32>>, Self::Error> {
        Ok(Some(SingleThreadStopReason::Signal(Signal::SIGINT)))
    }
}

/// Start a stdin reader that forwards bytes for the emulated UART RX FIFO.
pub(crate) fn spawn_stdin_reader() -> mpsc::Receiver<u8> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0_u8; 256];
        let mut stdin = std::io::stdin();
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 || buf[..n].iter().any(|b| tx.send(*b).is_err()) {
                break;
            }
        }
    });
    rx
}

/// Serve one debugging session for `cpu` on `127.0.0.1:port`.
///
/// Returns a one-line description of how the session ended.
pub(crate) fn serve(cpu: Cpu, port: u16) -> Result<String, GdbError> {
    let conn = accept(port)?;
    let mut target = EmuTarget::new(cpu, spawn_stdin_reader());
    let result = run_session(conn, &mut target);
    target.pump_uart();
    if let Some(e) = target.cpu.take_sd_error() {
        eprintln!("klausscc gdbserver: {e}");
    }
    if let Some(faults) = target.cpu.sd_fault_summary() {
        eprintln!("klausscc gdbserver: {faults}");
    }
    session_summary(result, "debugger disconnected", "debugger killed the program")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A target whose emulator has `word` at the entry, 0x20.
    fn target_with(word: u32) -> EmuTarget {
        let mut cpu = Cpu::new(&[0; 0x100], 0x20);
        assert!(cpu.debug_write(0x20, &word.to_le_bytes()));
        EmuTarget::new(cpu, mpsc::channel().1)
    }

    #[test]
    fn stepping_a_compiled_in_trap_skips_it() {
        let mut target = target_with(TRAP_WORD);
        assert!(target.step(None).is_ok());
        assert_eq!(target.exec, ExecMode::Step { skipped_trap: true });
        assert!(matches!(target.run(|| false), RunEvent::Stop(SingleThreadStopReason::DoneStep)));
        assert_eq!(target.cpu.debug_regs().pc, 0x24);
    }

    #[test]
    fn continuing_from_a_trap_starts_after_it() {
        let mut target = target_with(TRAP_WORD);
        assert!(target.resume(None).is_ok());
        assert_eq!(target.exec, ExecMode::Continue);
        assert_eq!(target.cpu.debug_regs().pc, 0x24);
        assert!(target.resume(Some(Signal::SIGINT)).is_err());
    }

    #[test]
    fn registers_convert_both_ways() {
        let mut target = target_with(0);
        let mut regs = KlaussRegs::default();
        assert!(target.read_registers(&mut regs).is_ok());
        assert_eq!(regs.pc, 0x20);
        assert!(regs.stage1, "the emulator has registers 20 and 21");
        regs.r[7] = 0x1234_5678_9ABC_DEF0;
        regs.flags = 0x1F;
        assert!(target.write_registers(&regs).is_ok());
        let d = target.cpu.debug_regs();
        assert_eq!((d.r[7], d.flags), (0x1234_5678_9ABC_DEF0, 0xF), "flags keep their nibble");
        let mut buf = [0_u8; 8];
        assert_eq!(target.read_register((), KlaussRegId::Gpr(7), &mut buf).ok(), Some(8));
        assert_eq!(u64::from_le_bytes(buf), 0x1234_5678_9ABC_DEF0);
        assert!(target.write_register((), KlaussRegId::Pc, &0x40_u32.to_le_bytes()).is_ok());
        assert_eq!(target.cpu.debug_regs().pc, 0x40);
        assert!(target.write_register((), KlaussRegId::Pc, &[1, 2]).is_err(), "wrong size");
        // MMU stage 1: SP_ALT and the mode bits (UM bit 0, DI bit 1).
        assert!(target.write_register((), KlaussRegId::SpAlt, &0x7000_u32.to_le_bytes()).is_ok());
        assert!(target.write_register((), KlaussRegId::Mode, &MODE_UM.to_le_bytes()).is_ok());
        let s = target.cpu.system_regs();
        assert_eq!((s.sp_alt, s.um, s.irq_disabled), (0x7000, true, false));
        assert_eq!(target.read_register((), KlaussRegId::Mode, &mut buf).ok(), Some(4));
        assert_eq!(u32::from_le_bytes(buf[..4].try_into().unwrap()), MODE_UM);
        // A 20-register G packet leaves them alone.
        regs.stage1 = false;
        assert!(target.write_registers(&regs).is_ok());
        assert_eq!(target.cpu.system_regs().sp_alt, 0x7000);
    }

    #[test]
    fn mpu_tlb_monitors_and_virtual_memory_while_translation_is_on() {
        // MTSR MPU_BASE0 D; MTSR MPU_LIMIT0 E; MTSR ECAUSE F; MTSR TLB_HI A;
        // MTSR TLB_LO B; TLBWI; MTSR STATUS C (MPU_EN and TE).
        let code: [u32; 7] = [0x6C61_0030, 0x6C61_1040, 0x6C60_2050, 0x6C63_0000, 0x6C63_1010, 0x6C09_0000, 0x6C60_0020];
        let mut target = target_with(code[0]);
        target.cpu.set_has_mpu(true);
        let bytes: Vec<u8> = code.iter().flat_map(|w| w.to_le_bytes()).collect();
        assert!(target.cpu.debug_write(0x20, &bytes));
        assert!(target.cpu.debug_write(0x3_0010, &0x1122_3344_u32.to_le_bytes()));
        let mut regs = target.regs();
        regs.r[0] = 0x0005_0100; // TLB_HI: VPN 5, global
        regs.r[1] = 0x0003_0003; // TLB_LO: PFN 3, R, V
        regs.r[2] = 0x300; // STATUS: MPU_EN, TE
        regs.r[3] = 0x2011; // MPU_BASE0: 0x2000, GUARD, EN
        regs.r[4] = 0x203F; // MPU_LIMIT0
        regs.r[5] = 0x107; // ECAUSE: ACCESS_LOAD, GUARD
        target.set_regs(&regs);
        for _ in 0..code.len() {
            assert_eq!(target.step_instruction(false), None);
        }
        let mut out = String::new();
        target.monitor("sr", &mut out);
        target.monitor("mpu", &mut out);
        target.monitor("tlb", &mut out);
        target.monitor("phys", &mut out);
        for want in [
            "MPU_EN 1   TE 1",
            "ECAUSE 7 GUARD",
            "TLB_HI 0x00050100   TLB_LO 0x00030003   TLB_INDEX 0x00000000   ASID 0   TLBVEC 0x00000000",
            "MPU on (STATUS.MPU_EN); ignored while TE = 1",
            "   0  0x00002000-0x0000203F  GUARD",
            "translation on (STATUS.TE)   ASID 0",
            "   0  VA 0x0005xxxx -> PA 0x0003xxxx  r--  global",
            "memory addresses are virtual (TE = 1)",
        ] {
            assert!(out.contains(want), "{want:?} not in:\n{out}");
        }
        // Virtual: useg through the TLB, kseg direct; an unmapped page and
        // the hole are errors, not physical reads.
        let mut buf = [0_u8; 4];
        assert_eq!(target.read_addrs(0x0005_0010, &mut buf).ok(), Some(4));
        assert_eq!(u32::from_le_bytes(buf), 0x1122_3344);
        assert_eq!(target.read_addrs(0x8000_0020, &mut buf).ok(), Some(4));
        assert_eq!(u32::from_le_bytes(buf), code[0]);
        assert!(target.read_addrs(0x0009_0000, &mut buf).is_err());
        assert!(target.read_addrs(0x9000_0000, &mut buf).is_err());
        assert!(target.write_addrs(0x0005_0014, &[0xAB]).is_ok());
        assert!(target.write_addrs(0x0009_0000, &[0xAB]).is_err());
        // Physical on request.
        let mut out = String::new();
        target.monitor("phys on", &mut out);
        assert_eq!(target.read_addrs(0x3_0010, &mut buf).ok(), Some(4));
        assert_eq!(u32::from_le_bytes(buf), 0x1122_3344);
        let mut eight = [0_u8; 8];
        assert_eq!(target.read_addrs(0x3_0010, &mut eight).ok(), Some(8));
        assert_eq!(eight[4], 0xAB, "the virtual write landed in frame 3");
        target.monitor("phys", &mut out);
        target.monitor("phys off", &mut out);
        assert!(out.contains("memory addresses are physical\n"), "{out}");
        assert!(target.read_addrs(0x0009_0000, &mut buf).is_err());
    }

    #[test]
    fn mpu_monitors_without_the_mpu() {
        // The default CPU, like the Nexys A7, has no MPU.
        let mut target = target_with(0);
        let mut out = String::new();
        target.monitor("sr", &mut out);
        target.monitor("mpu", &mut out);
        assert!(out.contains("mode supervisor   DI 0   TE 0   SP_ALT"), "{out}");
        assert!(!out.contains("MPU_EN"), "{out}");
        assert!(
            out.contains("no MPU (as on the Nexys A7; klausscc --mpu gives the emulator one)"),
            "{out}"
        );
    }

    #[test]
    fn exc_pass_lets_a_trap_reach_the_program_handler() {
        // MTSR EVEC A with A = 0x40 (set by the debugger), then TRAP at 0x24;
        // the handler at 0x40 is a NOP.
        let mut target = target_with(0x6C60_1000);
        assert!(target.cpu.debug_write(0x24, &TRAP_WORD.to_le_bytes()));
        assert!(target.cpu.debug_write(0x40, &0x6C00_0000_u32.to_le_bytes()));
        let mut regs = target.regs();
        regs.r[0] = 0x40;
        target.set_regs(&regs);
        assert!(target.step(None).is_ok());
        assert!(matches!(target.run(|| false), RunEvent::Stop(SingleThreadStopReason::DoneStep)));
        assert_eq!(target.cpu.system_regs().evec, 0x40);
        // Attached, EXC_PASS off: the TRAP stops in the debugger.
        assert!(matches!(
            target.step_instruction(false),
            Some(SingleThreadStopReason::Signal(Signal::SIGTRAP))
        ));
        assert_eq!(target.cpu.debug_regs().pc, 0x24);
        let mut out = String::new();
        target.monitor("exc_pass on", &mut out);
        assert!(target.cpu.exc_pass());
        target.monitor("exc_pass", &mut out);
        target.monitor("sr", &mut out);
        assert!(out.contains("EXC_PASS is on"), "{out}");
        assert!(out.contains("EVEC 0x00000040"), "{out}");
        target.cpu.clear_stop();
        assert_eq!(target.step_instruction(false), None);
        assert_eq!(target.cpu.debug_regs().pc, 0x40, "entered EVEC");
        assert_eq!(target.cpu.system_regs().ecause, 3, "TRAP");
    }
}
