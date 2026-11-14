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
//! `STATUS.TX_BUSY` set for one frame and the sender's own two cycles
//! ([`UART_CYCLES_PER_STORE`]), and a store while it is set is dropped, as
//! the RTL drops it, and recorded ([`EmulateResult::tx_busy_writes`]).  A
//! program starts with the transmitter idle, as one loaded over the UART does
//! since klaussCPU 04db80e (`KlaussCPU.sv` `START_WAIT`: the pipeline
//! waits for the loader's reply to finish), so its first store is taken.
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
//! trace, `system` is MMU stage 1: user mode, exceptions, the system
//! registers and the LR/SC reservation, and `mmu` is MMU stages 2 and 3:
//! the MPU, the TLB and the one translation every memory access an
//! instruction makes goes through.

mod cachesim;
mod tlbsim;
mod debug;
mod exec;
mod flags;
mod mmio;
mod mmu;
mod sd;
mod system;
#[cfg(test)]
mod tests;
mod trace;

pub use debug::{Access, DebugRegs, StopReason, Watch, WatchHit, WatchKind};
pub use mmio::{IrqState, TxBusyWrite, TxBusyWrites, UART_CYCLES_PER_BYTE, UART_CYCLES_PER_STORE, UART_SCRIPT_GAP};
pub use mmu::{MpuRegion, TlbEntry};
pub use sd::{FaultKind, SdCard, SdFault};
pub use system::{Cause, SystemRegs};

use crate::layout::is_mmio;
use flags::Flags;
use mmu::{Fetch, MPU_REGIONS, Mapped, TLB_MAX};
pub use mmu::TLB_ENTRIES_DEFAULT;
use sd::SdController;
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
/// Interrupt context word: the `DI`/`EI` bit, in disable sense (1 =
/// disabled), so a software-built frame with it clear resumes enabled
/// (`pipeline_core.sv` IRQ frame push, `IRET` restore).
const CTX_IRQ_DISABLED_SHIFT: u32 = 43;
/// Interrupt / exception frame: PM, the mode the entry came from (1 = user;
/// `MMU_PLAN.md` §3.2), which `IRET` restores.
const CTX_PREV_MODE_SHIFT: u32 = 44;
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
    /// The first I/O error on the `--sd-image` file, if any (the card read
    /// zeros or dropped the write).
    pub sd_error: Option<String>,
    /// The faults `--sd-fault` injected, if any were asked for.
    pub sd_faults: Option<String>,
}

/// The optional hardware of an emulated machine.  The default is the
/// Nexys A7 release bitstream with no SD card: no MPU, no native SD
/// controller, 32 TLB entries and the hardware TLB walker.
#[derive(Debug, Default)]
pub struct Hardware {
    /// The stage 2 MPU (`--mpu`, [`Cpu::set_has_mpu`]).
    pub mpu: bool,
    /// The TLB entry count (`--tlb-entries`, [`Cpu::set_tlb_entries`]);
    /// `None` is [`TLB_ENTRIES_DEFAULT`].
    pub tlb_entries: Option<u32>,
    /// No hardware TLB walker (`--no-tlb-walker`, [`Cpu::set_has_tlb_walker`]):
    /// the bitstreams before it.  The default has one, as the release
    /// bitstream does.
    pub no_tlb_walker: bool,
    /// The SD card in the slot (`--sd-image`).
    pub sd_card: Option<SdCard>,
    /// The native 4-bit SD controller (`--sd-native`): `SD_CTRL[18]`, the
    /// `SDM_*` registers, the source 2 cascade and `CPU_FEATURES[21]`.
    pub sd_native: bool,
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent architectural bits (WAIT, DI, UM, the timer's pending bit) and debugger switches"
)]
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
    /// Scripted UART input (`--uart-in`): bytes not yet in `uart_rx`.  One
    /// moves into the FIFO, when it has room, every [`UART_SCRIPT_GAP`]
    /// instructions, so a polled reader sees them arrive like typed input.
    uart_script: VecDeque<u8>,
    /// Instructions left before the next scripted byte may move.
    uart_script_wait: u64,
    /// `--uart-prompt`: the next scripted line waits until the program
    /// prints this (after the previous line's newline, or from the start).
    uart_script_prompt: Vec<u8>,
    /// Where in `uart` to look for the prompt, while a line is waiting.
    uart_script_waiting_from: Option<usize>,
    /// How much of `uart` has been searched for the prompt already.
    uart_script_checked: usize,
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
    /// The core's interrupt-disable bit (`DI` sets it, `EI` clears it from
    /// rs1[0], `IRET` restores it from the context word; clear at reset).
    /// While set no interrupt is taken, but `WAIT` still wakes.
    irq_disabled: bool,
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
    // ---- MMU stage 1 (`system`) ----
    /// User mode (`UM`).
    um: bool,
    /// The inactive stack pointer: the user SP in supervisor mode, the
    /// kernel SP in user mode (SR `USP` from supervisor mode).
    sp_alt: u32,
    /// `EVEC`: the exception vector; 0 parks on every exception.
    evec: u32,
    /// `ECAUSE[7:0]`, the cause code.
    ecause: u8,
    /// `ECAUSE[8]` GUARD: the last `ACCESS_*` was an MPU GUARD denial.
    ecause_guard: bool,
    /// `EADDR`.
    eaddr: u32,
    /// `SCRATCH0`, `SCRATCH1`.
    scratch: [u64; 2],
    /// `TP`, the thread pointer.
    tp: u64,
    /// The LR/SC reservation: `EA[31:3]` of the reserved doubleword.
    reservation: Option<u32>,
    /// A debugger is attached (ILLEGAL, TRAP and `MISALIGNED_LOAD/STORE` stop the CPU
    /// unless `exc_pass`).
    debugger_attached: bool,
    /// `EXC_PASS`: with a debugger attached, those causes go to `EVEC`.
    exc_pass: bool,
    /// Exceptions entered since reset (they retire no instruction, but
    /// count against a run's instruction cap).
    exc_entries: u64,
    /// The exception the CPU last stopped on instead of entering a handler.
    parked: Option<(Cause, u32)>,
    // ---- MMU stages 2 and 3 (`mmu`) ----
    /// This CPU has the stage 2 MPU ([`Cpu::set_has_mpu`]).  The Nexys A7
    /// bitstream does not, so by default neither does the emulator: its SRs
    /// are ILLEGAL, `STATUS.MPU_EN` reads 0 and `CPU_FEATURES` says so.
    has_mpu: bool,
    /// The MPU regions (`MPU_BASEn`, `MPU_LIMITn`).
    mpu: [MpuRegion; MPU_REGIONS],
    /// `STATUS.MPU_EN`.
    mpu_en: bool,
    /// `STATUS.TE`: translation on.
    te: bool,
    /// `STATUS[10]` MAL64 (`HW_PERF_PLAN.md` §4): misaligned 64-bit loads
    /// and stores trap instead of using the aligned-down doubleword.
    mal64: bool,
    /// `CNT_ZERO_LINES`: DCBZ operations since the last `CACHE_CTRL` clear.
    zero_lines: u64,
    /// The TLB.
    tlb: [TlbEntry; TLB_MAX],
    /// The modelled entry count (`CPU_FEATURES[15:8]`): the first `tlb_n`
    /// of `tlb` ([`Cpu::set_tlb_entries`]).
    tlb_n: u32,
    /// `TLB_HI`.
    tlb_hi: u32,
    /// `TLB_LO`.
    tlb_lo: u32,
    /// `TLB_INDEX`.
    tlb_index: u32,
    /// `ASID`, the current address space.
    asid: u8,
    /// `TLBVEC`: the TLB-miss vector; 0 = use `EVEC`.
    tlbvec: u32,
    /// This CPU has the hardware TLB walker (`CPU_FEATURES[25]`,
    /// [`Cpu::set_has_tlb_walker`]); the release bitstream does.  Without
    /// it `TLB_PTB`, `TLB_WALK` and `TLB_WCNT` are ILLEGAL.
    has_tlb_walker: bool,
    /// `TLB_PTB` `[26:3]`: the physical address of the current useg pgd.
    tlb_ptb: u32,
    /// `TLB_WALK[0]` EN: a useg TLB miss under `TE` = 1 walks.
    tlb_walk_en: bool,
    /// `TLB_WALK[12:8]` RR: the entry the next walker refill writes (only
    /// the bits that address an entry).
    tlb_walk_rr: u32,
    /// `TLB_WCNT`: successful walker refills, 48 bits.
    tlb_wcnt: u64,
    /// Cache what-if counts (`KLAUSSCC_CACHESIM`, [`cachesim`]).
    cachesim: Option<Box<cachesim::CacheSim>>,
    /// TLB what-if counts (`KLAUSSCC_TLBSIM`, [`tlbsim`]).
    tlbsim: std::cell::RefCell<Option<Box<tlbsim::TlbSim>>>,
    // ---- devices ----
    /// The SD controller, if it is modelled (`--sd-image`, `--sd-native`);
    /// without it the SD window reads 0, as it always has.
    sd: Option<Box<SdController>>,
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
            uart_script: VecDeque::new(),
            uart_script_wait: 0,
            uart_script_prompt: Vec::new(),
            uart_script_waiting_from: None,
            uart_script_checked: 0,
            stop: None,
            last_write: None,
            watches: Vec::new(),
            watch_hit: None,
            int_mask: 0,
            irq_disabled: false,
            int_vec: [0; 4],
            timer_pending: false,
            timer_period: TIMER_PERIOD_RESET,
            timer_count: 0,
            cycles: 0,
            instret: 0,
            perf_base: PerfBase::default(),
            waiting: false,
            um: false,
            sp_alt: 0,
            evec: 0,
            ecause: 0,
            ecause_guard: false,
            eaddr: 0,
            scratch: [0; 2],
            tp: 0,
            reservation: None,
            debugger_attached: false,
            exc_pass: false,
            exc_entries: 0,
            parked: None,
            has_mpu: false,
            mpu: [MpuRegion::default(); MPU_REGIONS],
            mpu_en: false,
            te: false,
            mal64: false,
            zero_lines: 0,
            tlb: [TlbEntry::default(); TLB_MAX],
            tlb_n: TLB_ENTRIES_DEFAULT,
            tlb_hi: 0,
            tlb_lo: 0,
            tlb_index: 0,
            asid: 0,
            tlbvec: 0,
            has_tlb_walker: true,
            tlb_ptb: 0,
            tlb_walk_en: false,
            tlb_walk_rr: 0,
            tlb_wcnt: 0,
            cachesim: cachesim::CacheSim::from_env(),
            tlbsim: std::cell::RefCell::new(tlbsim::TlbSim::from_env()),
            sd: None,
        }
    }

    /// Fit the optional hardware: the MPU, and an SD controller when there
    /// is a card or the native controller.  Without a card and
    /// `sd_native`, the SD window stays unmodelled (reads 0).
    pub fn install(&mut self, hw: Hardware) {
        self.set_has_mpu(hw.mpu);
        self.set_tlb_entries(hw.tlb_entries.unwrap_or(TLB_ENTRIES_DEFAULT));
        self.set_has_tlb_walker(!hw.no_tlb_walker);
        self.sd = (hw.sd_card.is_some() || hw.sd_native).then(|| Box::new(SdController::new(hw.sd_card, hw.sd_native)));
    }

    /// The native SD controller is modelled (`--sd-native`).
    pub(super) fn has_sd_native(&self) -> bool {
        self.sd.as_ref().is_some_and(|sd| sd.native())
    }

    /// The faults `--sd-fault` injected so far, if any were asked for.
    #[must_use]
    pub fn sd_fault_summary(&self) -> Option<String> {
        self.sd.as_ref()?.card()?.fault_summary()
    }

    /// Take the first I/O error on the SD image, if any.
    pub fn take_sd_error(&mut self) -> Option<String> {
        self.sd.as_mut()?.card_mut()?.take_io_error()
    }

    // ---- memory helpers (little-endian) --------------------------------------

    /// The `N` RAM bytes at `addr`, or `None` if any is past the end of RAM.
    fn bytes<const N: usize>(&self, addr: u32) -> Option<[u8; N]> {
        let a = addr as usize;
        self.mem.get(a..a + N)?.try_into().ok()
    }

    /// Read the 32-bit little-endian word at physical address `addr` (an
    /// instruction word, once [`Cpu::fetch`] has translated it); 0 past the
    /// end of RAM.
    fn read32(&self, addr: u32) -> u32 {
        self.bytes(addr).map_or(0, u32::from_le_bytes)
    }

    /// Read a 64-bit little-endian doubleword at the checked and
    /// translated address `at` ([`Cpu::map_data`]).
    ///
    /// A RAM access reaches the aligned doubleword that holds the address:
    /// the RTL's data cache picks the doubleword by `addr[4:3]` and ignores
    /// `addr[2:0]` (`mem_read_write.sv` :274-277, :569, :672-676, :818-822),
    /// which is what a 64-bit load, POP, RET or IRET with an unaligned address
    /// gets.  An MMIO read keeps the address as issued.  Watchpoints see the
    /// physical address, as the RTL's do (klaussCPU mmu 5e9338d); with
    /// `TE` = 0 that is the address as issued.
    ///
    /// Takes `&mut self` because an MMIO read (`RX_DATA`) is read-to-consume and
    /// mutates the UART FIFO.  Instruction fetch uses `read32` instead, so
    /// this never fires on a code fetch.
    fn read64(&mut self, at: Mapped) -> u64 {
        let addr = at.pa();
        debug_assert!(!(self.um && is_mmio(addr)), "unchecked user-mode MMIO load at 0x{addr:08X}");
        if is_mmio(addr) {
            self.note_access(at.pa(), 8, Access::Load);
            return self.mmio_read(addr);
        }
        self.note_access(at.pa() & !7, 8, Access::Load);
        self.bytes(addr & !7).map_or(0, u64::from_le_bytes)
    }

    /// Read `n` bytes (1/2/4) at `at`, zero-extended into a u64,
    /// little-endian.  A `MEMGET32` that runs into the next doubleword
    /// reads its bytes there through that doubleword's own translation.
    fn read_sub(&mut self, at: Mapped, n: usize) -> u64 {
        let addr = at.pa();
        debug_assert!(!(self.um && is_mmio(addr)), "unchecked user-mode MMIO load at 0x{addr:08X}");
        self.note_access(at.pa(), n as u32, Access::Load);
        if is_mmio(addr) {
            // MMIO presents its value in the low bits; a sub-word load takes the
            // low `n` bytes (addr[2]==0 lands the value in [31:0]).
            let mask = if n >= 8 { u64::MAX } else { (1_u64 << (8 * n)) - 1 };
            return self.mmio_read(addr) & mask;
        }
        let mut v: u64 = 0;
        for i in 0..n {
            if let Some(&byte) = self.mem.get(at.pa_at(i as u32) as usize) {
                v |= u64::from(byte) << (8 * i);
            }
        }
        v
    }

    /// Write a 64-bit doubleword at `at`, record the write for the trace.
    ///
    /// As with [`Cpu::read64`], a RAM write lands on the aligned doubleword
    /// that holds the address (all eight byte enables at `addr[4:3]`,
    /// `mem_read_write.sv` :610-632, :725-745).  The trace records the
    /// physical address, as the RTL's `ret_wr_addr` does: it is the
    /// translated port address (`pipeline_core.sv` :2997, :3659, :3758);
    /// with translation off that is the address as issued.
    fn write64(&mut self, at: Mapped, val: u64) {
        let addr = at.pa();
        debug_assert!(!(self.um && is_mmio(addr)), "unchecked user-mode MMIO store at 0x{addr:08X}");
        if is_mmio(addr) {
            self.note_access(at.pa(), 8, Access::Store);
            self.mmio_write(addr, val);
        } else {
            self.note_access(at.pa() & !7, 8, Access::Store);
            let pa = (addr & !7) as usize;
            if let Some(dst) = self.mem.get_mut(pa..pa + 8) {
                dst.copy_from_slice(&val.to_le_bytes());
            }
        }
        self.last_write = Some(MemWrite {
            addr: at.pa(),
            byte_enable: 0xFF,
            data: val,
        });
    }

    /// Write the low `n` bytes (1/2/4) at `at`, little-endian; record write.
    fn write_sub(&mut self, at: Mapped, val: u64, n: usize) {
        let addr = at.pa();
        debug_assert!(!(self.um && is_mmio(addr)), "unchecked user-mode MMIO store at 0x{addr:08X}");
        // Byte-enable mask placed at the byte-lane within the doubleword, matching
        // the RTL's byte-enable semantics for the trace `be` field.
        self.note_access(at.pa(), n as u32, Access::Store);
        let lane = (addr & 7) as usize;
        let be_bits: u8 = ((1_u16 << n) - 1).rotate_left(lane as u32) as u8;
        if is_mmio(addr) {
            self.mmio_store(addr, val, n as u32);
            self.last_write = Some(MemWrite {
                addr: at.pa() & !7,
                byte_enable: be_bits,
                data: val,
            });
            return;
        }
        for i in 0..n {
            if let Some(byte) = self.mem.get_mut(at.pa_at(i as u32) as usize) {
                *byte = (val >> (8 * i)) as u8;
            }
        }
        self.last_write = Some(MemWrite {
            addr: at.pa() & !7,
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
    /// machine without retiring, and an instruction that raises an
    /// exception retires nothing either (`MMU_PLAN.md` §7.2).  With
    /// `allow_irq` false (a
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
            if !self.irq_disabled {
                self.take_irq(src);
                return None;
            }
            // DI: no interrupt is taken, but a WAIT wakes (WFI) and the
            // instruction after it runs (`pipeline_core.sv` irq_take vs
            // irq_ok).
            self.waiting = false;
        }
        let pc = self.pc;
        self.last_write = None;
        let (words, fetch_pa) = match self.fetch(pc) {
            Fetch::Words(words, pa) => (words, pa),
            Fetch::Fault(fault, word) => {
                // A fetch fault (MMU stage 1 §3.6, the MPU, the TLB):
                // nothing of the instruction runs.
                self.raise_fault(fault, word, pc);
                return None;
            }
            Fetch::OutOfRange => {
                self.stop = Some(StopReason::PcOutOfRange(pc));
                return None;
            }
        };
        let word = words[0];
        if let Some(sim) = self.cachesim.as_mut() {
            sim.fetch(pc, self.um, fetch_pa, (word >> 30).max(1));
        }
        if !self.execute(words) {
            // An illegal word or a misaligned access parks without retiring:
            // the RTL gives it no trace line (`pipeline_core.sv` WB park,
            // kinds 2 and 5) and `PERF_INSTR` counts only retirements
            // (`KlaussCPU.sv` :2342-2343).  An exception taken to `EVEC`
            // retires nothing either (`MMU_PLAN.md` §7.2).
            return None;
        }
        self.instret += 1;
        if let Some(sim) = self.cachesim.as_mut() {
            sim.retire();
        }
        self.tick(CYCLES_PER_INSTRUCTION);
        Some((pc, word))
    }

    // ---- main execute loop ---------------------------------------------------

    /// Run until HALT / TRAP / cap / fault. Returns the result + trace (if any).
    /// The cap counts retired instructions and exceptions entered, so a
    /// handler that faults on its own first instruction still ends.
    ///
    /// When `trace` is `Some`, one line per retired instruction is written to the
    /// sink in the `doc/EMULATOR_ISA_SEMANTICS.md` "Trace format" layout.  The trace is
    /// streamed as the run proceeds rather than buffered, so a multi-million
    /// instruction run costs no extra memory.  The first failed write (e.g. a
    /// broken pipe on stdout) stops the trace; the run carries on without it.
    pub fn run(&mut self, max_instructions: u64, mut trace: Option<&mut dyn std::io::Write>) -> EmulateResult {
        let mut count: u64 = 0;
        let entries_before = self.exc_entries;
        while self.stop.is_none() && count + (self.exc_entries - entries_before) < max_instructions {
            if let Some((pc, word)) = self.step_inner(true) {
                count += 1;
                self.pump_uart_script();
                if let Some(t) = trace.as_deref_mut()
                    && self.trace_line(t, count, pc, word).is_err()
                {
                    // A failed write (e.g. a broken pipe) ends the trace, not the run.
                    trace = None;
                }
            }
        }
        if let Some(sim) = self.cachesim.as_mut() {
            sim.report_window(self.instret - self.perf_base.instret, true);
            sim.report();
        }
        if let Some(sim) = self.tlbsim.get_mut() {
            sim.report(self.instret - self.perf_base.instret, true);
        }
        let stop = self.stop.unwrap_or(StopReason::InstructionCap);
        EmulateResult {
            uart: std::mem::take(&mut self.uart),
            instructions: count,
            stop,
            tx_busy_writes: std::mem::take(&mut self.tx_busy_writes),
            sd_error: self.take_sd_error(),
            sd_faults: self.sd_fault_summary(),
        }
    }
}

/// Emulate a flat DDR image starting at `entry` from reset, streaming the
/// per-instruction trace to `trace` (if `Some`) as the run proceeds (see
/// [`Cpu::run`]).  The UART transmitter starts idle, as it does when a program
/// loaded onto the board starts; `--emulate` and `--emulate-test` run
/// programs this way.
///
/// Nothing is buffered: each retired instruction's line is written straight to
/// the sink, so a multi-million instruction run uses no extra memory
/// regardless of trace length.
///
/// `mpu` gives the CPU the stage 2 MPU ([`Cpu::set_has_mpu`], `--mpu`).
#[must_use]
pub fn emulate_image(image: &[u8], entry: u32, max_instructions: u64, mpu: bool, trace: Option<&mut dyn std::io::Write>) -> EmulateResult {
    let hw = Hardware { mpu, ..Hardware::default() };
    emulate_image_input(image, entry, max_instructions, hw, &[], &[], trace)
}

/// [`emulate_image`] with scripted UART input (`--uart-in`): `uart_in` is
/// typed into the UART receiver a byte at a time, each line after the
/// program prints `uart_prompt` if that isn't empty ([`Cpu::set_uart_script`]),
/// on a machine with the optional hardware `hw` ([`Cpu::install`]).
#[must_use]
pub fn emulate_image_input(
    image: &[u8],
    entry: u32,
    max_instructions: u64,
    hw: Hardware,
    uart_in: &[u8],
    uart_prompt: &[u8],
    trace: Option<&mut dyn std::io::Write>,
) -> EmulateResult {
    let mut cpu = Cpu::new(image, entry);
    cpu.install(hw);
    cpu.set_uart_script(uart_in, uart_prompt);
    cpu.run(max_instructions, trace)
}
