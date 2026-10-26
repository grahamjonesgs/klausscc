//! Independent ISA emulator (golden-model trace generator) for the `KlaussCPU`.
//!
//! This is Phase-0 of the FPGA pipelining effort: an architectural reference
//! model that executes a flat DDR image and emits a per-retired-instruction
//! trace.  Semantics follow `doc/EMULATOR_ISA_SEMANTICS.md` (RTL-verified corner
//! cases, which OVERRIDE `CPU_ARCHITECTURE.md`); the per-opcode *encodings* are
//! the **ISA encoding v2** flag-day re-numbering documented in
//! `doc/ISA_ENCODING_V2.md`, plus the ISA v3 additions (short one-word forms,
//! fused compare-and-branch, `ENTER`/`LEAVE`/`LEAVERET`, the 32-bit `W`
//! operations).  Most instructions come from the v1 CPU; v2 changed the
//! instruction word layout and gave the register forms independent `rd`,
//! `rs1` and `rs2` fields.
//!
//! The model is intentionally cycle-agnostic: each instruction commits
//! atomically.  Cache, IFB, timing and the DDR multi-cycle pipeline are not
//! modelled (they have no architectural effect).
//!
//! Interrupts follow `CPU_ARCHITECTURE.md` §13.1 and the RTL (`KlaussCPU.sv`
//! IRQ controller, `pipeline_core.sv` entry sequencer): the controller at
//! `0xF00F_0000` (`INT_MASK`, `INT_PENDING`, `INT_VECn`, `TIMER_PERIOD`,
//! `TIMER_COUNT`, the millisecond clock) with source 0 = the periodic timer.
//! An unmasked, vectored pending source is taken between instructions: the
//! context word `{INT_MASK, flags, PC}` is pushed, the source's mask bit
//! cleared and the timer's pending bit acknowledged; `IRET` restores all of
//! it, and `WAIT` sleeps until a source is ready.  Time is modelled as
//! [`CYCLES_PER_INSTRUCTION`] cycles per retired instruction, so interrupt
//! *timing* differs from the RTL (no golden-trace use) while the
//! architectural effects match.  Blitter / Ethernet interrupts (sources 1-2)
//! are not modelled.
//!
//! The UART transmitter is timed on the same clock: a `TX_DATA` store keeps
//! `STATUS.TX_BUSY` set for one frame ([`UART_CYCLES_PER_BYTE`]), and a store
//! while it is set is dropped, as the RTL drops it, and recorded
//! ([`EmulateResult::tx_busy_writes`]).  [`emulate_loaded_image`] starts a
//! program as the board does after a UART load, with the loader's reply still
//! being sent for [`LOADER_MESSAGE_CYCLES`].
//!
//! ## v2 word-0 layout (`doc/ISA_ENCODING_V2.md` §1)
//!
//! ```text
//!  31 30 29    26 25              16 15  12 11  8 7   4 3   0
//! ┌─────┬────────┬──────────────────┬──────┬─────┬─────┬─────┐
//! │ LEN │ CLASS  │ attributes + OP  │  x   │ rd  │ rs1 │ rs2 │
//! └─────┴────────┴──────────────────┴──────┴─────┴─────┴─────┘
//! ```
//!
//! - **LEN `[31:30]`**: `01`=1 word, `10`=2 words, `11`=3 words, `00`=illegal.
//! - **CLASS `[29:26]`**: major class (1=ALU-rr, 2=ALU-imm, 3=cmp, 4=shift/bit,
//!   5=unary, 6=load, 7=store, 8=branch, 9=stack, A=mul/div, B=system, C=I/O).
//! - **rd `[11:8]`**, **rs1 `[7:4]`**, **rs2 `[3:0]`**: register fields (stores put
//!   the data source in `rd`).  `imm32` at PC+4; `imm64` = lo32@PC+4, hi32@PC+8.
//!
//! Decoding is done directly from the 32-bit word's bit fields, independent of
//! the assembler's opcode table — so the emulator is a genuine second
//! implementation, not a re-run of the assembler.
//!
//! [`Cpu`] and the run entry points live here, with the
//! memory helpers.  The rest of the machine is split by concern: `exec`
//! decodes and executes one instruction, `flags` is the `Z S C V` register
//! and its saved layouts, `mmio` models the UART, interrupt controller /
//! timer and performance counters, `debug` is the debugger's view (registers,
//! memory, watchpoints, stop reasons) and `trace` writes the per-instruction
//! trace.

mod debug;
mod exec;
mod flags;
mod mmio;
#[cfg(test)]
mod tests;
mod trace;

pub use debug::{Access, DebugRegs, StopReason, Watch, WatchHit, WatchKind};
pub use mmio::{IrqState, LOADER_MESSAGE_CYCLES, LOADER_MESSAGE_LEN, TxBusyWrite, TxBusyWrites, UART_CYCLES_PER_BYTE};

use crate::layout::is_mmio;
use flags::Flags;
use std::collections::VecDeque;

/// Modelled `i_Clk` cycles per retired instruction (timer, millisecond clock and
/// the cycle counter).  The emulator is not cycle-accurate; this only sets how
/// fast time passes relative to the instruction stream.
pub const CYCLES_PER_INSTRUCTION: u64 = 2;
/// `i_Clk` cycles per millisecond (100 MHz): the millisecond clock, and how
/// [`IrqState::cycles`] converts to time.
pub const CYCLES_PER_MS: u64 = 100_000;
/// `TIMER_PERIOD` reset value (~10.5 ms at 100 MHz).
const TIMER_PERIOD_RESET: u32 = 0x000F_FFFF;
/// Default instruction-count cap to guard against runaway / infinite loops.
pub const DEFAULT_MAX_INSTRUCTIONS: u64 = 50_000_000;
/// Size of the modelled address space (128 MiB DDR2).
const MEM_SIZE: usize = 128 * 1024 * 1024;
/// Interrupt context word `{INT_MASK[42:39], flags[38:32], PC[31:0]}`: where
/// the flags field (the [`Flags::to_ctx`] layout) starts.
const CTX_FLAGS_SHIFT: u32 = 32;
/// Interrupt context word: where the 4-bit `INT_MASK` starts.
const CTX_INT_MASK_SHIFT: u32 = 39;
/// Initial stack pointer — top of DDR2, grows down. The loader sets SP near the
/// top of memory; we use a generous value below the 128 MiB ceiling so PUSH
/// never wraps. Matches the board's full-descending stack convention.
const STACK_TOP: u32 = 0x0800_0000;

/// Result of an emulation run.
pub struct EmulateResult {
    /// Captured UART output: the byte stream written to the UART `TX_DATA` MMIO
    /// register (`0xF001_0000`), one byte (`data[7:0]`) per store, in order.
    pub uart: Vec<u8>,
    /// Number of instructions retired.
    pub instructions: u64,
    /// Why execution stopped.
    pub stop: StopReason,
    /// `TX_DATA` stores made while the transmitter was busy (their bytes are
    /// dropped, as on the board, and are not in [`EmulateResult::uart`]).
    pub tx_busy_writes: TxBusyWrites,
}

/// A memory write, as the trace annotates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MemWrite {
    /// The doubleword address (the byte address for a 64-bit store, which
    /// need not be aligned).
    addr: u32,
    /// Byte enables within the doubleword (bit `n` = byte lane `n`).
    byte_enable: u8,
    /// The doubleword after the write (for MMIO, the value stored).
    data: u64,
}

/// The performance-counter values at the last `PERF_CTRL` clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct PerfBase {
    /// Cycle counter.
    cycles: u64,
    /// Retired-instruction counter.
    instret: u64,
}

/// The architectural machine state.
pub struct Cpu {
    /// General-purpose registers R0..R15 (64-bit).
    regs: [u64; 16],
    /// Stack pointer (32-bit hardware register, separate from R0..R15).
    sp: u32,
    /// Program counter (32-bit byte address).
    pc: u32,
    /// The unified 4-bit flags register `Z S C V` (sticky — only written by the
    /// documented producers); `E`/`L`/`U` are derived on read.
    flags: Flags,
    /// Flat little-endian memory image (128 MiB).
    mem: Vec<u8>,
    /// Captured UART output (`TX_DATA` writes, low byte per store).
    uart: Vec<u8>,
    /// The cycle the byte being sent finishes (`STATUS.TX_BUSY` reads 1 before it).
    tx_busy_until: u64,
    /// `TX_DATA` stores made while busy.
    tx_busy_writes: TxBusyWrites,
    /// UART receive FIFO — head byte returned (and popped) by an `RX_DATA` read.
    /// Empty unless a caller feeds it via [`Cpu::feed_uart_rx`].
    uart_rx: VecDeque<u8>,
    /// Why execution stopped, once it has.  A HALT is final; the other
    /// reasons can be cleared by a debugger ([`Cpu::clear_stop`]).
    stop: Option<StopReason>,
    /// The current instruction's memory write, for the trace annotation.
    last_write: Option<MemWrite>,
    /// Debugger watchpoints checked on every data access.
    watches: Vec<Watch>,
    /// First watchpoint hit by the current instruction.
    watch_hit: Option<WatchHit>,
    /// `INT_MASK`: per-source interrupt enables (4 bits).
    int_mask: u8,
    /// `INT_VEC0..3`: handler addresses (0 disables a source).
    int_vec: [u32; 4],
    /// Source 0 pending (`r_timer_interrupt`), cleared when it is dispatched.
    timer_pending: bool,
    /// `TIMER_PERIOD`.
    timer_period: u32,
    /// Timer counter; rolls over (setting `timer_pending`) past the period.
    timer_count: u32,
    /// Modelled `i_Clk` cycles since reset.
    cycles: u64,
    /// Retired instructions since reset (the `PERF_INSTR` counter).
    instret: u64,
    /// Counter values at the last `PERF_CTRL` clear.
    perf_base: PerfBase,
    /// Parked in `WAIT` until an interrupt source is ready.
    waiting: bool,
}

impl Cpu {
    /// Build a CPU with a flat DDR image already laid out (heap header + code).
    ///
    /// `image` is the `build_ddr_image` output (header at 0x0, code at 0x20).
    /// `entry` is the byte address of the first instruction to execute.  An
    /// image larger than the 128 MiB address space is truncated to it (no
    /// toolchain image comes near that size).
    #[must_use]
    pub fn new(image: &[u8], entry: u32) -> Self {
        Self::with_memory(image, entry, MEM_SIZE, STACK_TOP)
    }

    /// A CPU with only `mem_size` bytes of RAM and the stack at its top, so a
    /// test does not allocate the full 128 MiB.
    #[cfg(test)]
    fn with_mem_size(image: &[u8], entry: u32, mem_size: usize) -> Self {
        Self::with_memory(image, entry, mem_size, u32::try_from(mem_size).unwrap_or(STACK_TOP))
    }

    /// A CPU at reset with `mem_size` bytes of RAM holding `image`.
    fn with_memory(image: &[u8], entry: u32, mem_size: usize, stack_top: u32) -> Self {
        let mut mem = vec![0_u8; mem_size];
        let n = image.len().min(mem_size);
        mem[..n].copy_from_slice(&image[..n]);
        Self {
            regs: [0; 16],
            sp: stack_top,
            pc: entry,
            flags: Flags::default(),
            mem,
            uart: Vec::new(),
            tx_busy_until: 0,
            tx_busy_writes: TxBusyWrites::default(),
            uart_rx: VecDeque::new(),
            stop: None,
            last_write: None,
            watches: Vec::new(),
            watch_hit: None,
            int_mask: 0,
            int_vec: [0; 4],
            timer_pending: false,
            timer_period: TIMER_PERIOD_RESET,
            timer_count: 0,
            cycles: 0,
            instret: 0,
            perf_base: PerfBase::default(),
            waiting: false,
        }
    }

    // ---- memory helpers (little-endian) --------------------------------------

    /// The `N` RAM bytes at `addr`, or `None` if any is past the end of RAM.
    fn bytes<const N: usize>(&self, addr: u32) -> Option<[u8; N]> {
        let a = addr as usize;
        self.mem.get(a..a + N)?.try_into().ok()
    }

    /// Read a 32-bit little-endian word (used for instruction fetch and imm);
    /// 0 past the end of RAM.
    fn read32(&self, addr: u32) -> u32 {
        self.bytes(addr).map_or(0, u32::from_le_bytes)
    }

    /// Read a 64-bit little-endian doubleword.
    ///
    /// A RAM access reaches the aligned doubleword that holds `addr`: the
    /// RTL's data cache picks the doubleword by `addr[4:3]` and ignores
    /// `addr[2:0]` (`mem_read_write.sv` :274-277, :569, :672-676, :818-822),
    /// which is what a 64-bit load, POP, RET or IRET with an unaligned address
    /// gets.  An MMIO read keeps the address as issued.
    ///
    /// Takes `&mut self` because an MMIO read (`RX_DATA`) is read-to-consume and
    /// mutates the UART FIFO.  Instruction/immediate fetch uses `read32` instead,
    /// so this never fires on a code fetch.
    fn read64(&mut self, addr: u32) -> u64 {
        if is_mmio(addr) {
            self.note_access(addr, 8, Access::Load);
            return self.mmio_read(addr);
        }
        let addr = addr & !7;
        self.note_access(addr, 8, Access::Load);
        self.bytes(addr).map_or(0, u64::from_le_bytes)
    }

    /// Read `n` bytes (1/2/4) zero-extended into a u64, little-endian.
    fn read_sub(&mut self, addr: u32, n: usize) -> u64 {
        self.note_access(addr, n as u32, Access::Load);
        if is_mmio(addr) {
            // MMIO presents its value in the low bits; a sub-word load takes the
            // low `n` bytes (addr[2]==0 lands the value in [31:0]).
            let mask = if n >= 8 { u64::MAX } else { (1_u64 << (8 * n)) - 1 };
            return self.mmio_read(addr) & mask;
        }
        let a = addr as usize;
        let mut v: u64 = 0;
        for i in 0..n {
            if a + i < self.mem.len() {
                v |= u64::from(self.mem[a + i]) << (8 * i);
            }
        }
        v
    }

    /// Write a 64-bit doubleword, record the write for the trace.
    ///
    /// As with [`Cpu::read64`], a RAM write lands on the aligned doubleword
    /// that holds `addr` (all eight byte enables at `addr[4:3]`,
    /// `mem_read_write.sv` :610-632, :725-745).  The trace records the address
    /// as issued, as the RTL's `ret_wr_addr` does (`pipeline_core.sv` :1895).
    fn write64(&mut self, addr: u32, val: u64) {
        if is_mmio(addr) {
            self.note_access(addr, 8, Access::Store);
            self.mmio_write(addr, val);
        } else {
            let at = (addr & !7) as usize;
            self.note_access(at as u32, 8, Access::Store);
            if let Some(dst) = self.mem.get_mut(at..at + 8) {
                dst.copy_from_slice(&val.to_le_bytes());
            }
        }
        self.last_write = Some(MemWrite {
            addr,
            byte_enable: 0xFF,
            data: val,
        });
    }

    /// Write the low `n` bytes (1/2/4) at `addr`, little-endian; record write.
    fn write_sub(&mut self, addr: u32, val: u64, n: usize) {
        // Byte-enable mask placed at the byte-lane within the doubleword, matching
        // the RTL's byte-enable semantics for the trace `be` field.
        self.note_access(addr, n as u32, Access::Store);
        let lane = (addr & 7) as usize;
        let be_bits: u8 = ((1_u16 << n) - 1).rotate_left(lane as u32) as u8;
        if is_mmio(addr) {
            self.mmio_write(addr, val);
            self.last_write = Some(MemWrite {
                addr: addr & !7,
                byte_enable: be_bits,
                data: val,
            });
            return;
        }
        let a = addr as usize;
        for i in 0..n {
            if a + i < self.mem.len() {
                self.mem[a + i] = (val >> (8 * i)) as u8;
            }
        }
        self.last_write = Some(MemWrite {
            addr: addr & !7,
            byte_enable: be_bits,
            data: self.peek64(addr & !7),
        });
    }

    /// Side-effect-free 64-bit RAM read (trace annotation): no MMIO, no watchpoints.
    fn peek64(&self, addr: u32) -> u64 {
        if is_mmio(addr) {
            return 0;
        }
        self.bytes(addr).map_or(0, u64::from_le_bytes)
    }

    /// One step of the machine: take a ready interrupt (entry is a step of
    /// its own), or wake from / finish a `WAIT`, or retire one instruction,
    /// returning its `(pc, word)` for the trace.  An illegal word stops the
    /// machine without retiring.  With `allow_irq` false (a
    /// debugger single-step) no interrupt is taken, and a `WAIT` completes as
    /// if woken, leaving the interrupt pending for the next continue.
    fn step_inner(&mut self, allow_irq: bool) -> Option<(u32, u32)> {
        if self.waiting {
            if !allow_irq {
                if self.timer_can_fire() {
                    self.skip_to_timer_expiry();
                }
                self.waiting = false;
                return None;
            }
            if self.irq_ready().is_none() {
                if !self.timer_can_fire() {
                    self.stop = Some(StopReason::WaitForever(self.pc));
                    return None;
                }
                self.skip_to_timer_expiry();
            }
        }
        if allow_irq && let Some(src) = self.irq_ready() {
            self.take_irq(src);
            return None;
        }
        let pc = self.pc;
        if (pc as usize) + 4 > self.mem.len() {
            self.stop = Some(StopReason::PcOutOfRange(pc));
            return None;
        }
        let word = self.read32(pc);
        self.last_write = None;
        self.execute(word);
        if matches!(self.stop, Some(StopReason::InvalidOpcode(_))) {
            // An illegal word parks without retiring: the RTL gives it no
            // trace line (`pipeline_core.sv` :1564-1566) and `PERF_INSTR`
            // counts only retirements (`KlaussCPU.sv` :2342-2343).
            return None;
        }
        self.instret += 1;
        self.tick(CYCLES_PER_INSTRUCTION);
        Some((pc, word))
    }

    // ---- main execute loop ---------------------------------------------------

    /// Run until HALT / TRAP / cap / fault. Returns the result + trace (if any).
    ///
    /// When `trace` is `Some`, one line per retired instruction is written to the
    /// sink in the `doc/EMULATOR_ISA_SEMANTICS.md` "Trace format" layout.  The trace is
    /// streamed as the run proceeds rather than buffered, so a multi-million
    /// instruction run costs no extra memory.  The first failed write (e.g. a
    /// broken pipe on stdout) stops the trace; the run carries on without it.
    pub fn run(&mut self, max_instructions: u64, mut trace: Option<&mut dyn std::io::Write>) -> EmulateResult {
        let mut count: u64 = 0;
        while self.stop.is_none() && count < max_instructions {
            if let Some((pc, word)) = self.step_inner(true) {
                count += 1;
                if let Some(t) = trace.as_deref_mut()
                    && self.trace_line(t, count, pc, word).is_err()
                {
                    // A failed write (e.g. a broken pipe) ends the trace, not the run.
                    trace = None;
                }
            }
        }
        let stop = self.stop.unwrap_or(StopReason::InstructionCap);
        EmulateResult {
            uart: std::mem::take(&mut self.uart),
            instructions: count,
            stop,
            tx_busy_writes: std::mem::take(&mut self.tx_busy_writes),
        }
    }
}

/// Emulate a flat DDR image starting at `entry` from reset (the UART idle),
/// streaming the per-instruction trace to `trace` (if `Some`) as the run
/// proceeds (see [`Cpu::run`]).
///
/// Nothing is buffered: each retired instruction's line is written straight to
/// the sink, so a multi-million instruction run uses no extra memory
/// regardless of trace length.
#[must_use]
pub fn emulate_image(image: &[u8], entry: u32, max_instructions: u64, trace: Option<&mut dyn std::io::Write>) -> EmulateResult {
    let mut cpu = Cpu::new(image, entry);
    cpu.run(max_instructions, trace)
}

/// As [`emulate_image`], but starting the way a program loaded onto the board
/// does: with the loader's reply still being sent, so the UART transmitter is
/// busy for [`LOADER_MESSAGE_CYCLES`] (see [`Cpu::start_after_uart_load`]).
/// `--emulate` and `--emulate-test` run programs this way.
#[must_use]
pub fn emulate_loaded_image(image: &[u8], entry: u32, max_instructions: u64, trace: Option<&mut dyn std::io::Write>) -> EmulateResult {
    let mut cpu = Cpu::new(image, entry);
    cpu.start_after_uart_load();
    cpu.run(max_instructions, trace)
}
