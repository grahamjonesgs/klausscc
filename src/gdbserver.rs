//! GDB remote-protocol server for source-level debugging (`--gdbserver PORT`).
//!
//! Serves the built-in ISA emulator to lldb (the KlaussCPU-aware build from
//! `klausscpu-llvm`, via `gdb-remote` or VS Code's lldb-dap) using the
//! `gdbstub` crate.  See `klaussCPU/DEBUG_PLAN.md` (Phase 2).
//!
//! Register layout (`g` packet order, little-endian), matching the DWARF
//! numbers in `KlaussCPURegisterInfo.td`:
//!
//! | regnum | name     | bits | DWARF | generic |
//! |--------|----------|------|-------|---------|
//! | 0-15   | r0-r15   | 64   | 0-15  | r0-r3 = arg1-4, r15 = fp |
//! | 16     | sp       | 32   | 16    | sp      |
//! | 17     | pc       | 32   | 17    | pc (= the CFI return-address column) |
//! | 18     | flags    | 32   | -     | flags: bit3 Z, bit2 S, bit1 C, bit0 V |
//! | 19     | `int_mask` | 32 | -     | per-source interrupt enables (not modelled: 0) |
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
//! a PC outside RAM SIGSEGV.  UART output streams to stdout and stdin feeds the
//! UART receive FIFO.  The debugger cannot read or write MMIO (`0xF000_0000+`)
//! unless `monitor mmio on` is given: some device reads have side effects,
//! e.g. popping the UART RX FIFO, and debuggers read memory freely.

use crate::emulate::{Cpu, DebugRegs, StopReason, Watch, WatchKind as EmuWatchKind};
use gdbstub::arch::{Arch, RegId, Registers};
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
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::num::NonZeroUsize;
use std::sync::mpsc;

/// `TRAP` (v2 class B, OP 4): the debugger breakpoint instruction.
const TRAP_WORD: u32 = 0x6C04_0000;
/// Hardware breakpoint / watchpoint slots, matching the board's debug module.
const HW_BREAKPOINTS: usize = 4;
const HW_WATCHPOINTS: usize = 2;
/// Instructions run between checks for debugger input (Ctrl-C) and UART I/O.
const POLL_INTERVAL: u32 = 4096;
const TRIPLE: &str = "klausscpu-unknown-elf";

const TARGET_XML: &str = r#"<?xml version="1.0"?>
<!DOCTYPE target SYSTEM "gdb-target.dtd">
<target version="1.0">
  <architecture>klausscpu</architecture>
  <feature name="org.klausscpu.core">
    <flags id="klausscpu_flags" size="4">
      <field name="V" start="0" end="0"/>
      <field name="C" start="1" end="1"/>
      <field name="S" start="2" end="2"/>
      <field name="Z" start="3" end="3"/>
    </flags>
    <reg name="r0" altname="a" bitsize="64" regnum="0" type="int64" group="general" dwarf_regnum="0" generic="arg1"/>
    <reg name="r1" altname="b" bitsize="64" regnum="1" type="int64" group="general" dwarf_regnum="1" generic="arg2"/>
    <reg name="r2" altname="c" bitsize="64" regnum="2" type="int64" group="general" dwarf_regnum="2" generic="arg3"/>
    <reg name="r3" altname="d" bitsize="64" regnum="3" type="int64" group="general" dwarf_regnum="3" generic="arg4"/>
    <reg name="r4" altname="e" bitsize="64" regnum="4" type="int64" group="general" dwarf_regnum="4"/>
    <reg name="r5" altname="f" bitsize="64" regnum="5" type="int64" group="general" dwarf_regnum="5"/>
    <reg name="r6" altname="g" bitsize="64" regnum="6" type="int64" group="general" dwarf_regnum="6"/>
    <reg name="r7" altname="h" bitsize="64" regnum="7" type="int64" group="general" dwarf_regnum="7"/>
    <reg name="r8" altname="i" bitsize="64" regnum="8" type="int64" group="general" dwarf_regnum="8"/>
    <reg name="r9" altname="j" bitsize="64" regnum="9" type="int64" group="general" dwarf_regnum="9"/>
    <reg name="r10" altname="k" bitsize="64" regnum="10" type="int64" group="general" dwarf_regnum="10"/>
    <reg name="r11" altname="l" bitsize="64" regnum="11" type="int64" group="general" dwarf_regnum="11"/>
    <reg name="r12" altname="m" bitsize="64" regnum="12" type="int64" group="general" dwarf_regnum="12"/>
    <reg name="r13" altname="n" bitsize="64" regnum="13" type="int64" group="general" dwarf_regnum="13"/>
    <reg name="r14" altname="o" bitsize="64" regnum="14" type="int64" group="general" dwarf_regnum="14"/>
    <reg name="r15" altname="p" bitsize="64" regnum="15" type="int64" group="general" dwarf_regnum="15" generic="fp"/>
    <reg name="sp" bitsize="32" regnum="16" type="data_ptr" group="general" dwarf_regnum="16" generic="sp"/>
    <reg name="pc" bitsize="32" regnum="17" type="uint32" group="general" dwarf_regnum="17" generic="pc"/>
    <reg name="flags" bitsize="32" regnum="18" type="klausscpu_flags" group="general" generic="flags"/>
    <reg name="int_mask" bitsize="32" regnum="19" type="uint32" group="general"/>
  </feature>
</target>"#;

// ---- architecture definition -------------------------------------------------

/// The `KlaussCPU` register file in `g`-packet layout.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KlaussRegs {
    r: [u64; 16],
    sp: u32,
    pc: u32,
    flags: u32,
    int_mask: u32,
}

const REG_BYTES: usize = 16 * 8 + 4 * 4;

impl Registers for KlaussRegs {
    type ProgramCounter = u32;

    fn pc(&self) -> u32 {
        self.pc
    }

    fn gdb_serialize(&self, mut write_byte: impl FnMut(Option<u8>)) {
        for r in &self.r {
            r.to_le_bytes().iter().for_each(|b| write_byte(Some(*b)));
        }
        for v in [self.sp, self.pc, self.flags, self.int_mask] {
            v.to_le_bytes().iter().for_each(|b| write_byte(Some(*b)));
        }
    }

    fn gdb_deserialize(&mut self, bytes: &[u8]) -> Result<(), ()> {
        if bytes.len() != REG_BYTES {
            return Err(());
        }
        let (gprs, rest) = bytes.split_at(16 * 8);
        for (r, chunk) in self.r.iter_mut().zip(gprs.as_chunks::<8>().0) {
            *r = u64::from_le_bytes(*chunk);
        }
        let words: Vec<u32> = rest.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect();
        if let [sp, pc, flags, int_mask] = words[..] {
            self.sp = sp;
            self.pc = pc;
            self.flags = flags;
            self.int_mask = int_mask;
            Ok(())
        } else {
            Err(())
        }
    }
}

/// A register addressed by its `regnum` (single-register `p`/`P` packets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KlaussRegId {
    /// R0..R15.
    Gpr(u8),
    /// Stack pointer.
    Sp,
    /// Program counter.
    Pc,
    /// Z/S/C/V flags.
    Flags,
    /// Interrupt mask.
    IntMask,
}

impl RegId for KlaussRegId {
    fn from_raw_id(id: usize) -> Option<(Self, Option<NonZeroUsize>)> {
        let (reg, size) = match id {
            0..=15 => (Self::Gpr(id as u8), 8),
            16 => (Self::Sp, 4),
            17 => (Self::Pc, 4),
            18 => (Self::Flags, 4),
            19 => (Self::IntMask, 4),
            _ => return None,
        };
        Some((reg, NonZeroUsize::new(size)))
    }
}

/// `gdbstub` architecture description for `KlaussCPU`.
pub enum KlaussCpuArch {}

impl Arch for KlaussCpuArch {
    type Usize = u32;
    type Registers = KlaussRegs;
    type BreakpointKind = usize;
    type RegId = KlaussRegId;

    fn target_description_xml() -> Option<&'static str> {
        Some(TARGET_XML)
    }
}

// ---- target -----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecMode {
    Step,
    Continue,
}

/// What stopped a run, or debugger input waiting.
enum RunEvent {
    IncomingData,
    Stop(SingleThreadStopReason<u32>),
}

/// The emulator plus the debugger's view of it.
pub struct EmuTarget {
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
            // The emulator stores each transmitted byte as a char (Latin-1).
            let bytes: Vec<u8> = out.chars().map(|c| c as u32 as u8).collect();
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(&bytes);
            let _ = stdout.flush();
        }
        let rx: Vec<u8> = self.uart_rx.try_iter().collect();
        if !rx.is_empty() {
            self.cpu.feed_uart_rx(&rx);
        }
    }

    /// Execute one instruction, checking breakpoints first and watchpoints
    /// after.  Returns the stop reason, or `None` to keep going.
    fn step_instruction(&mut self) -> Option<SingleThreadStopReason<u32>> {
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
        let stop = self.cpu.step_one();
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
        })
    }

    fn run(&mut self, mut incoming_data: impl FnMut() -> bool) -> RunEvent {
        if self.exec == ExecMode::Step {
            let stop = if self.skipped_trap {
                None // skipping the TRAP was the step
            } else {
                self.step_instruction()
            };
            self.pump_uart();
            return RunEvent::Stop(stop.unwrap_or(SingleThreadStopReason::DoneStep));
        }
        loop {
            for _ in 0..POLL_INTERVAL {
                if let Some(stop) = self.step_instruction() {
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

/// True for the memory-mapped device window (`addr[31:28] == 0xF`).
const fn is_mmio(addr: u32) -> bool {
    addr >> 28 == 0xF
}

impl MonitorCmd for EmuTarget {
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
            _ => {
                outputln!(out, "klausscc gdbserver monitor commands:");
                outputln!(out, "  mmio on|off   allow debugger reads/writes of 0xF000_0000+ (default off)");
            }
        }
        Ok(())
    }
}

impl SingleThreadBase for EmuTarget {
    fn read_registers(&mut self, regs: &mut KlaussRegs) -> TargetResult<(), Self> {
        let d = self.cpu.debug_regs();
        regs.r = d.r;
        regs.sp = d.sp;
        regs.pc = d.pc;
        regs.flags = u32::from(d.flags);
        regs.int_mask = 0;
        Ok(())
    }

    fn write_registers(&mut self, regs: &KlaussRegs) -> TargetResult<(), Self> {
        self.cpu.set_debug_regs(&DebugRegs {
            r: regs.r,
            sp: regs.sp,
            pc: regs.pc,
            flags: (regs.flags & 0xF) as u8,
        });
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
        if ok {
            Ok(())
        } else {
            Err(TargetError::NonFatal)
        }
    }

    fn support_resume(&mut self) -> Option<SingleThreadResumeOps<'_, Self>> {
        Some(self)
    }
}

impl SingleRegisterAccess<()> for EmuTarget {
    fn read_register(&mut self, _tid: (), reg_id: KlaussRegId, buf: &mut [u8]) -> TargetResult<usize, Self> {
        let d = self.cpu.debug_regs();
        let bytes: Vec<u8> = match reg_id {
            KlaussRegId::Gpr(i) => d.r[usize::from(i)].to_le_bytes().to_vec(),
            KlaussRegId::Sp => d.sp.to_le_bytes().to_vec(),
            KlaussRegId::Pc => d.pc.to_le_bytes().to_vec(),
            KlaussRegId::Flags => u32::from(d.flags).to_le_bytes().to_vec(),
            KlaussRegId::IntMask => 0_u32.to_le_bytes().to_vec(),
        };
        let n = bytes.len().min(buf.len());
        buf[..n].copy_from_slice(&bytes[..n]);
        Ok(n)
    }

    fn write_register(&mut self, _tid: (), reg_id: KlaussRegId, val: &[u8]) -> TargetResult<(), Self> {
        let mut d = self.cpu.debug_regs();
        let word = |v: &[u8]| v.try_into().map(u32::from_le_bytes).map_err(|_| TargetError::NonFatal);
        match reg_id {
            KlaussRegId::Gpr(i) => {
                d.r[usize::from(i)] = val.try_into().map(u64::from_le_bytes).map_err(|_| TargetError::NonFatal)?;
            }
            KlaussRegId::Sp => d.sp = word(val)?,
            KlaussRegId::Pc => d.pc = word(val)?,
            KlaussRegId::Flags => d.flags = (word(val)? & 0xF) as u8,
            KlaussRegId::IntMask => return Ok(()), // not modelled
        }
        self.cpu.set_debug_regs(&d);
        Ok(())
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

impl HostInfo for EmuTarget {
    fn host_info(&self, write_item: &mut dyn FnMut(&HostInfoResponse<'_>)) -> Result<(), Self::Error> {
        write_item(&HostInfoResponse::Triple(TRIPLE));
        write_item(&HostInfoResponse::Endianness(Endianness::Little));
        write_item(&HostInfoResponse::PointerSize(4));
        Ok(())
    }
}

impl ProcessInfo for EmuTarget {
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

enum EmuEventLoop {}

impl run_blocking::BlockingEventLoop for EmuEventLoop {
    type Target = EmuTarget;
    type Connection = Box<dyn ConnectionExt<Error = std::io::Error>>;
    type StopReason = SingleThreadStopReason<u32>;

    fn wait_for_stop_reason(
        target: &mut EmuTarget,
        conn: &mut Self::Connection,
    ) -> Result<run_blocking::Event<Self::StopReason>, run_blocking::WaitForStopReasonError<&'static str, std::io::Error>> {
        match target.run(|| conn.peek().map_or(true, |b| b.is_some())) {
            RunEvent::IncomingData => {
                let byte = conn.read().map_err(run_blocking::WaitForStopReasonError::Connection)?;
                Ok(run_blocking::Event::IncomingData(byte))
            }
            RunEvent::Stop(reason) => Ok(run_blocking::Event::TargetStopped(reason)),
        }
    }

    fn on_interrupt(_target: &mut EmuTarget) -> Result<Option<Self::StopReason>, &'static str> {
        Ok(Some(SingleThreadStopReason::Signal(Signal::SIGINT)))
    }
}

/// Start a stdin reader that forwards bytes for the emulated UART RX FIFO.
fn spawn_stdin_reader() -> mpsc::Receiver<u8> {
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
pub fn serve(cpu: Cpu, port: u16) -> Result<String, String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("cannot listen on port {port}: {e}"))?;
    eprintln!("klausscc gdbserver: waiting for a debugger on 127.0.0.1:{port}");
    let (stream, peer) = listener.accept().map_err(|e| format!("accept failed: {e}"))?;
    eprintln!("klausscc gdbserver: debugger connected from {peer}");
    let _ = stream.set_nodelay(true);
    let conn: Box<dyn ConnectionExt<Error = std::io::Error>> = Box::new(stream);

    let mut target = EmuTarget::new(cpu, spawn_stdin_reader());
    let result = GdbStub::new(conn).run_blocking::<EmuEventLoop>(&mut target);
    target.pump_uart();
    match result {
        Ok(DisconnectReason::Disconnect) => Ok("debugger disconnected".to_owned()),
        Ok(DisconnectReason::TargetExited(code)) => Ok(format!("program halted (exit status {code})")),
        Ok(DisconnectReason::TargetTerminated(sig)) => Ok(format!("program terminated with {sig}")),
        Ok(DisconnectReason::Kill) => Ok("debugger killed the program".to_owned()),
        Err(e) => Err(format!("gdbstub error: {e}")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests may unwrap")]
    use super::*;

    #[test]
    fn register_packet_round_trips() {
        let mut regs = KlaussRegs::default();
        for (i, r) in regs.r.iter_mut().enumerate() {
            *r = 0x0101_0101_0101_0101 * i as u64;
        }
        regs.sp = 0x07FF_FF20;
        regs.pc = 0x0000_02C0;
        regs.flags = 0b0110;
        let mut bytes = Vec::new();
        regs.gdb_serialize(|b| bytes.push(b.unwrap()));
        assert_eq!(bytes.len(), REG_BYTES);
        assert_eq!(&bytes[16 * 8 + 4..16 * 8 + 8], &0x2C0_u32.to_le_bytes(), "pc follows sp");
        let mut back = KlaussRegs::default();
        back.gdb_deserialize(&bytes).unwrap();
        assert_eq!(back, regs);
        assert!(back.gdb_deserialize(&bytes[1..]).is_err());
    }

    #[test]
    fn register_ids_match_target_xml() {
        assert_eq!(KlaussRegId::from_raw_id(15).map(|r| r.0), Some(KlaussRegId::Gpr(15)));
        assert_eq!(KlaussRegId::from_raw_id(17).map(|r| r.0), Some(KlaussRegId::Pc));
        assert_eq!(KlaussRegId::from_raw_id(20), None);
        for n in 0..20 {
            assert!(TARGET_XML.contains(&format!("regnum=\"{n}\"")), "regnum {n} described");
        }
        assert!(TARGET_XML.contains(r#"name="pc" bitsize="32" regnum="17""#));
        assert!(TARGET_XML.contains(r#"dwarf_regnum="17" generic="pc""#));
    }
}
