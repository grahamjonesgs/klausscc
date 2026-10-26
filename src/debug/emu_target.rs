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
//! UART receive FIFO.  The debugger cannot read or write MMIO (`0xF000_0000+`)
//! unless `monitor mmio on` is given: some device reads have side effects,
//! e.g. popping the UART RX FIFO, and debuggers read memory freely.

use super::arch::{HW_BREAKPOINTS, HW_WATCHPOINTS, KlaussCpuArch, KlaussRegId, KlaussRegs, TRAP_WORD};
use super::{GdbError, RunEvent, Session, accept, mmio_monitor_cmd, run_session, session_summary};
use crate::emu::{CYCLES_PER_MS, Cpu, DebugRegs, StopReason, Watch, WatchKind as EmuWatchKind};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecMode {
    Step,
    Continue,
}

/// The emulator plus the debugger's view of it.
pub(crate) struct EmuTarget {
    cpu: Cpu,
    exec: ExecMode,
    sw_breakpoints: Vec<u32>,
    hw_breakpoints: Vec<u32>,
    watches: Vec<Watch>,
    /// Set by resume/step: don't stop on a breakpoint at the resume PC.
    resuming: bool,
    /// Set when resuming skipped a compiled-in TRAP.
    skipped_trap: bool,
    /// `monitor mmio on`: let the debugger access device registers.
    allow_mmio: bool,
    uart_rx: mpsc::Receiver<u8>,
}

impl EmuTarget {
    fn new(cpu: Cpu, uart_rx: mpsc::Receiver<u8>) -> Self {
        Self {
            cpu,
            exec: ExecMode::Continue,
            sw_breakpoints: Vec::new(),
            hw_breakpoints: Vec::new(),
            watches: Vec::new(),
            resuming: false,
            skipped_trap: false,
            allow_mmio: false,
            uart_rx,
        }
    }

    /// Prepare to run from the current PC: step past a compiled-in TRAP the
    /// last stop reported, and let the emulator continue after a fault stop.
    fn prepare_resume(&mut self) {
        let mut regs = self.cpu.debug_regs();
        self.skipped_trap = self.cpu.fetch_word(regs.pc) == TRAP_WORD;
        if self.skipped_trap {
            regs.pc = regs.pc.wrapping_add(4);
            self.cpu.set_debug_regs(&regs);
        }
        self.cpu.clear_stop();
        self.resuming = true;
    }

    /// Stream UART output to stdout and feed stdin into the UART RX FIFO.
    fn pump_uart(&mut self) {
        let out = self.cpu.take_uart();
        if !out.is_empty() {
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(&crate::emu::uart_bytes(&out));
            let _ = stdout.flush();
        }
        let rx: Vec<u8> = self.uart_rx.try_iter().collect();
        if !rx.is_empty() {
            self.cpu.feed_uart_rx(&rx);
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
        if let Some((addr, is_store)) = self.cpu.take_watch_hit() {
            return Some(SingleThreadStopReason::Watch {
                tid: (),
                kind: if is_store { WatchKind::Write } else { WatchKind::Read },
                addr,
            });
        }
        stop.map(|s| match s {
            StopReason::Halt => SingleThreadStopReason::Exited((self.cpu.debug_regs().r[12] & 0xFF) as u8),
            StopReason::Trap => SingleThreadStopReason::Signal(Signal::SIGTRAP),
            StopReason::InvalidOpcode(_) => SingleThreadStopReason::Signal(Signal::SIGILL),
            StopReason::PcOutOfRange(_) => SingleThreadStopReason::Signal(Signal::SIGSEGV),
            StopReason::InstructionCap => SingleThreadStopReason::Signal(Signal::SIGXCPU),
            StopReason::WaitForever(pc) => {
                eprintln!("klausscc gdbserver: WAIT at 0x{pc:08X} can never be woken (no unmasked, vectored interrupt)");
                SingleThreadStopReason::Signal(Signal::SIGSTOP)
            }
        })
    }

    fn run(&mut self, mut incoming_data: impl FnMut() -> bool) -> RunEvent {
        if self.exec == ExecMode::Step {
            let stop = if self.skipped_trap {
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
        let cmd = std::str::from_utf8(cmd).map_or("", str::trim);
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
            _ if mmio_monitor_cmd(cmd, &mut self.allow_mmio, &mut out) => {}
            _ => {
                outputln!(out, "klausscc gdbserver monitor commands:");
                outputln!(out, "  mmio on|off   allow debugger reads/writes of 0xF000_0000+ (default off)");
                outputln!(out, "  irq           interrupt controller and timer state");
            }
        }
        Ok(())
    }
}

impl SingleThreadBase for EmuTarget {
    fn read_registers(&mut self, regs: &mut KlaussRegs) -> TargetResult<(), Self> {
        *regs = KlaussRegs::from(&self.cpu.debug_regs());
        Ok(())
    }

    fn write_registers(&mut self, regs: &KlaussRegs) -> TargetResult<(), Self> {
        self.cpu.set_debug_regs(&DebugRegs::from(regs));
        Ok(())
    }

    fn support_single_register_access(&mut self) -> Option<SingleRegisterAccessOps<'_, (), Self>> {
        Some(self)
    }

    fn read_addrs(&mut self, start_addr: u32, data: &mut [u8]) -> TargetResult<usize, Self> {
        let n = if self.allow_mmio && is_mmio(start_addr) {
            self.cpu.debug_read_mmio(start_addr, data)
        } else {
            self.cpu.debug_read(start_addr, data)
        };
        if n == 0 && !data.is_empty() {
            return Err(TargetError::NonFatal); // MMIO or outside RAM
        }
        Ok(n)
    }

    fn write_addrs(&mut self, start_addr: u32, data: &[u8]) -> TargetResult<(), Self> {
        let ok = if self.allow_mmio && is_mmio(start_addr) {
            self.cpu.debug_write_mmio(start_addr, data)
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
        let regs = KlaussRegs::from(&self.cpu.debug_regs());
        Ok(reg_id.encode(regs.get(reg_id), buf))
    }

    fn write_register(&mut self, _tid: (), reg_id: KlaussRegId, val: &[u8]) -> TargetResult<(), Self> {
        let value = reg_id.decode(val).ok_or(TargetError::NonFatal)?;
        let mut regs = KlaussRegs::from(&self.cpu.debug_regs());
        regs.set(reg_id, value);
        self.cpu.set_debug_regs(&DebugRegs::from(&regs));
        Ok(())
    }
}

impl From<&DebugRegs> for KlaussRegs {
    fn from(d: &DebugRegs) -> Self {
        Self {
            r: d.r,
            sp: d.sp,
            pc: d.pc,
            flags: u32::from(d.flags),
            int_mask: u32::from(d.int_mask),
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
        self.prepare_resume();
        self.exec = ExecMode::Step;
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
        if self.watches.len() >= HW_WATCHPOINTS {
            return Ok(false);
        }
        self.watches.push(Watch {
            addr,
            len,
            kind: emu_watch_kind(kind),
        });
        self.cpu.set_watches(self.watches.clone());
        Ok(true)
    }

    fn remove_hw_watchpoint(&mut self, addr: u32, len: u32, kind: WatchKind) -> TargetResult<bool, Self> {
        let target = Watch {
            addr,
            len,
            kind: emu_watch_kind(kind),
        };
        let Some(pos) = self.watches.iter().position(|w| *w == target) else {
            return Ok(false);
        };
        self.watches.remove(pos);
        self.cpu.set_watches(self.watches.clone());
        Ok(true)
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
    session_summary(result, "debugger disconnected", "debugger killed the program")
}
