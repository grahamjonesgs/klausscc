//! Memory-mapped devices: the UART, the interrupt controller / timer and
//! the performance counters, and the decode for the SD controller (`sd`).

use super::sd::DmaBus;
use super::{CYCLES_PER_MS, Cpu, PerfBase, StopReason, coherence};

/// Interrupt controller / timer device select (`addr[27:16]`).
const IRQ_DEV_SELECT: u32 = 0x00F;
/// Performance-counter device select (`addr[27:16]`).
const PERF_DEV_SELECT: u32 = 0x00D;
/// SD card controller device select (`addr[27:16]`).
const SD_DEV_SELECT: u32 = 0x000;
/// Cache controller device select (`addr[27:16]`; `MMIO_MAP.md`, "Cache controller").
const CACHE_DEV_SELECT: u32 = 0x005;

// Cache controller register offsets (`addr[15:0]`).  Only the read-only
// geometry and the DCBZ count are modelled; the hit/miss counters read 0
// and CACHE_CTRL writes (flush, invalidate, ICACHE_INV) do nothing, as the
// emulator has no caches.
/// `CACHE_INFO`: `[7:0]` ways, `[23:8]` sets, `[31:24]` line bytes,
/// `[63:32]` total bytes: the D-cache, 2 ways x 1024 sets x 32 B = 64 KiB.
const CACHE_INFO: u32 = 0x0008;
/// The D-cache's `CACHE_INFO` value.
const CACHE_INFO_VALUE: u64 = (64 << 10 << 32) | (32 << 24) | (1024 << 8) | 2;
/// `ICACHE_INFO` (`HW_PERF_PLAN.md` §2), `CACHE_INFO`'s format: the 16 KiB
/// direct-mapped I-cache, 1024 x 16 B.
const ICACHE_INFO: u32 = 0x0018;
/// The I-cache's `ICACHE_INFO` value.
const ICACHE_INFO_VALUE: u64 = (16 << 10 << 32) | (16 << 24) | (1024 << 8) | 1;
/// `CNT_ZERO_LINES` (`HW_PERF_PLAN.md` §1): DCBZ operations; `CACHE_CTRL[0]`
/// clears it with the other counters.
const CNT_ZERO_LINES: u32 = 0x0070;
/// `CACHE_CTRL`: `[0]` clears the counters, `[1]` FLUSH, `[2]` INVALIDATE,
/// `[3]` `ICACHE_INV`, `[4]` `RANGE_CLEAN`, `[5]` `RANGE_INV` and `[6]`
/// `RANGE_IINV` (`--dcache-range-ops`).
const CACHE_CTRL: u32 = 0x0000;
/// `CACHE_STATUS`: `[0]` `MNT_BUSY`, a maintenance walk pending or running.
const CACHE_STATUS: u32 = 0x0010;
/// `CACHE_OP_ADDR` (`--dcache-range-ops`): a range op's physical start;
/// `[4:0]` read 0 and are not stored.
const CACHE_OP_ADDR: u32 = 0x0030;
/// `CACHE_OP_LEN` (`--dcache-range-ops`): a range op's length in bytes,
/// `[31:0]` (`[63:32]` read 0); 0 is a no-op.
const CACHE_OP_LEN: u32 = 0x0038;
/// `CACHE_CTRL` bits.
const CTRL_CLEAR: u64 = 1;
const CTRL_FLUSH: u64 = 1 << 1;
const CTRL_INVALIDATE: u64 = 1 << 2;
const CTRL_ICACHE_INV: u64 = 1 << 3;
const CTRL_RANGE_CLEAN: u64 = 1 << 4;
const CTRL_RANGE_INV: u64 = 1 << 5;
const CTRL_RANGE_IINV: u64 = 1 << 6;
/// Cycles a whole-cache walk keeps `MNT_BUSY` up: 1024 sets x 2 ways, plus
/// [`WALK_CYCLES_PER_DIRTY_LINE`] for each line it writes back (spec v1.1).
const WALK_FULL_CYCLES: u64 = 2048;
/// A range walk: its fixed cost and cycles per line it touches (plus the
/// dirty lines' writebacks).
const WALK_RANGE_CYCLES: u64 = 8;
const WALK_RANGE_CYCLES_PER_LINE: u64 = 2;
/// A dirty line's writeback during a walk.
const WALK_CYCLES_PER_DIRTY_LINE: u64 = 10;
/// Lines in the D-cache (64 KiB): a whole walk writes back at most these.
const DCACHE_LINES: u64 = 2048;

/// What a walk does when it completes.
#[derive(Debug, Clone, Copy)]
enum WalkOp {
    /// FLUSH: write every dirty line back.
    Flush,
    /// INVALIDATE: write back, then drop every line.
    Invalidate,
    /// `RANGE_CLEAN` / `RANGE_INV` of the lines from `start` (a line
    /// address) for `len` bytes.
    Range { start: u64, len: u64, clean: bool, inv: bool },
    /// `RANGE_IINV`: the same lines dropped from the I-cache.
    IRange { start: u64, len: u64 },
}

/// The cache controller's maintenance state.  The emulator has no caches,
/// so a walk changes no data; but `MNT_BUSY` reads 1 for as long as the
/// walk would take, and the `--sd-dma` coherence checker sees each one
/// when it completes.
#[derive(Debug, Default)]
pub(crate) struct CacheCtl {
    /// The range ops are modelled (`--dcache-range-ops`).
    pub(super) range_ops: bool,
    /// `CACHE_OP_ADDR`.
    op_addr: u64,
    /// `CACHE_OP_LEN`.
    op_len: u64,
    /// `MNT_BUSY` until this cycle; a walk asked for during one queues
    /// behind it.
    busy_until: u64,
    /// The walks accepted and not yet complete, oldest first, with the
    /// cycle each completes and the coherence checker's sequence number.
    pending: std::collections::VecDeque<(u64, WalkOp, u64)>,
}

impl CacheCtl {
    /// Queue a walk of `cycles` at cycle `now`, after any in flight;
    /// `seq` is the coherence checker's number for it (0 without one).
    fn walk(&mut self, now: u64, cycles: u64, op: WalkOp, seq: u64) {
        self.busy_until = self.busy_until.max(now) + cycles;
        self.pending.push_back((self.busy_until, op, seq));
    }

    /// `MNT_BUSY` at cycle `now`.
    pub(super) const fn busy(&self, now: u64) -> bool {
        now < self.busy_until
    }

    /// A D-cache walk that covers `[addr, addr + len)` is pending or running
    /// at cycle `now` (a whole-cache one, or a range one that overlaps it).
    /// An I-cache walk writes nothing back, and a D-cache walk elsewhere
    /// touches none of these lines.  For a DMA write (`write`) a pending
    /// whole-cache FLUSH is no hazard: it only cleans, and a dirty line the
    /// DMA overwrites is reported separately.
    pub(crate) fn dwalk_covers(&self, now: u64, addr: u64, len: u64, write: bool) -> bool {
        self.pending.iter().any(|&(at, op, _)| {
            at > now
                && match op {
                    WalkOp::Flush => !write,
                    WalkOp::Invalidate => true,
                    WalkOp::Range { start, len: n, .. } => start < addr.saturating_add(len) && addr < start.saturating_add(n),
                    WalkOp::IRange { .. } => false,
                }
        })
    }
}

// Interrupt controller / timer register offsets (`addr[15:0]`).
/// `INT_MASK`: per-source enables (4 bits).
const INT_MASK: u32 = 0x0000;
/// `INT_PENDING` (read-only): bit 0 = timer, bit 2 = the source 2 cascade
/// with `--sd-native` (the blitter and Ethernet are not modelled).
const INT_PENDING: u32 = 0x0008;
/// `INT_VEC0`; `INT_VEC1..3` follow at 8-byte steps.
const INT_VEC0: u32 = 0x0010;
/// `INT_VEC3`, the last vector register.
const INT_VEC3: u32 = INT_VEC0 + 3 * 8;
/// `TIMER_PERIOD`; writing it restarts the count.
const TIMER_PERIOD: u32 = 0x0030;
/// `TIMER_COUNT` (read-only).
const TIMER_COUNT: u32 = 0x0038;
/// The free-running millisecond clock (read-only).
const MS_CLOCK: u32 = 0x0040;
/// `CPU_FEATURES` (read-only, `MMU_PLAN.md` §6.3): which of the MMU stages
/// and LR/SC the core has.  Older bitstreams read 0 here.
const CPU_FEATURES: u32 = 0x0048;
/// `INT2_EN` (`SD_NATIVE_PLAN.md` §4.5, `--sd-native` only): `[0]` Ethernet,
/// `[1]` SD, reset `0b01`.  Reads 0 without the native SD controller, as on
/// the release bitstream.
const INT2_EN: u32 = 0x0050;
/// `INT2_PENDING` (read-only, `--sd-native` only): `[0]` Ethernet (0 here),
/// `[1]` the SD interrupt, before `INT2_EN`.
const INT2_PENDING: u32 = 0x0058;

// Performance-counter register offsets (`addr[15:0]`).
/// `PERF_CTRL`: writing bit 0 clears the counters.
const PERF_CTRL: u32 = 0x0000;
/// `PERF_CYCLES`: cycles since the last clear.
const PERF_CYCLES: u32 = 0x0008;
/// `PERF_INSTR`: retired instructions since the last clear.
const PERF_INSTR: u32 = 0x0010;

// ---- Memory-mapped I/O (bus_splitter address decode) ------------------------
// The RTL bus_splitter routes addr[31:28] == 0xF to the MMIO space; within that,
// addr[27:16] selects the device (0x001 == UART) and addr[15:0] is the register
// offset.  See the MMIO read mux in `KlaussCPU.sv` (`default` reads 0) and its
// UART TX write.

/// UART device select value found in `addr[27:16]` of an MMIO access.
const UART_DEV_SELECT: u32 = 0x001;
/// UART `TX_DATA` register offset (`addr[15:0]`) — write transmits `data[7:0]`.
const UART_TX_DATA: u32 = 0x0000;
/// UART `RX_DATA` register offset — read returns the FIFO head and pops it once.
const UART_RX_DATA: u32 = 0x0008;
/// UART `STATUS` register offset — read returns {`RX_FULL`, `RX_EMPTY`, `TX_BUSY`}.
const UART_STATUS: u32 = 0x0010;
/// Modelled RX FIFO depth.  Only affects the `STATUS.RX_FULL` bit; the RTL depth is
/// a parameter and the emulator has no external byte source that would fill it.
///
/// The FIFO starts empty, as on the board: since klaussCPU 04db80e the RTL
/// empties it at `LOAD_COMPLETE` (`uart_rx_fifo1` `.i_Reset = w_reset_H |
/// (st.SM == LOAD_COMPLETE)`), so nothing sent before the load ended reaches
/// the program.  A byte that arrives during `START_WAIT` (about 6000 cycles,
/// before the first instruction) is kept; the emulator has no such source.
const UART_RX_FIFO_DEPTH: usize = 16;
/// Instructions between two scripted input bytes (`--uart-in`): about 20 µs
/// at 100 MHz, a fast typist.
pub const UART_SCRIPT_GAP: u64 = 2_000;

// ---- UART transmit timing ----------------------------------------------------
// There is no TX FIFO.  A `TX_DATA` store hands one byte to `uart_send_msg`
// (`KlaussCPU.sv` :2173-2181: `r_msg[7:0]`, length 1, a one-cycle
// `r_msg_send_DV`), which starts it only from its idle state
// (`uart_send_msg.sv` :47-58); while it is sending, `i_msg_send_DV` is not
// looked at (:61-76), so a store while busy is dropped and the byte in flight
// goes out unchanged.  `STATUS.TX_BUSY` is that sender's `o_sending_msg`
// (`KlaussCPU.sv` :911), which stays set until the cycle after the last
// frame's `s_CLEANUP` (`uart_send_msg.sv` :61-82).
//
// A program loaded over the UART (or by netboot) starts with the transmitter
// idle, so its first store is taken (`KlaussCPU.sv` `START_WAIT`, klaussCPU
// 04db80e): `LOAD_COMPLETE` clears `r_start_wait_counter` and
// `r_start_wait_ack` and starts the `"Load Complete OK\n\r"` reply, and
// `START_WAIT` waits for `uart_send_msg` to take it (`w_sending_msg` = 1, at
// most 16 cycles) and then for `w_sending_msg` = 0.  The last stop bit ends
// (`uart_tx.sv` :90-96), `uart_send_msg` goes `s_TX_MSG` -> `s_CLEANUP` on the
// next edge and `s_CLEANUP` -> `s_IDLE` (clearing `o_sending_msg`) on the one
// after, and `START_WAIT` then sees 0 and sets `r_pip_start`.  On bitstreams
// built before that change the pipeline starts while the 18-byte reply is
// still going out (about 60 µs), and a store made then is dropped too.

/// `uart_tx` `CLKS_PER_BIT` (`uart_tx.sv` :7, not overridden by the instance in
/// `uart_send_msg.sv` :29): `i_Clk` cycles per bit.  `i_Clk` is the 100 MHz MIG
/// `ui_clk` (`KlaussCPU.sv` :88-95), so the line runs at 100 MHz / 33 ≈ 3 Mbaud.
const UART_CLKS_PER_BIT: u64 = 33;
/// Bits per 8N1 frame: start, eight data, stop (`uart_tx.sv` :48-99).
const UART_BITS_PER_FRAME: u64 = 10;
/// The cycle `uart_tx` spends in `s_IDLE` accepting a byte and the one in
/// `s_CLEANUP` after the stop bit (`uart_tx.sv` :34-45, :103-106).
const UART_FRAME_OVERHEAD_CYCLES: u64 = 2;
/// `i_Clk` cycles between bytes of one message (332, about 3.3 µs):
/// `uart_send_msg` holds `r_UART_Tx_DV` high for the whole message, so `uart_tx`
/// goes from `s_CLEANUP` straight back through `s_IDLE` into the next start bit.
pub const UART_CYCLES_PER_BYTE: u64 = UART_BITS_PER_FRAME * UART_CLKS_PER_BIT + UART_FRAME_OVERHEAD_CYCLES;
/// The cycle `uart_send_msg` spends in `s_IDLE` taking a send request (before
/// `uart_tx` sees it) and the one in `s_CLEANUP` (`o_sending_msg` still 1) after
/// the last byte (`uart_send_msg.sv` :47-58, :78-82).
const UART_SEND_MSG_OVERHEAD_CYCLES: u64 = 2;
/// `i_Clk` cycles from a `TX_DATA` store (the cycle that sets `r_msg_send_DV`)
/// to the first cycle a second store is taken (334): a store `n` cycles after
/// the previous one is dropped while `n` < 334.  `o_sending_msg` reads 1 from 2
/// cycles after the store until then, and an MMIO read decodes from the
/// registered address a cycle after its strobe (`KlaussCPU.sv` :411-414), so
/// a `TX_BUSY` poll issued right after a store already sees 1.
pub const UART_CYCLES_PER_STORE: u64 = UART_CYCLES_PER_BYTE + UART_SEND_MSG_OVERHEAD_CYCLES;
/// How many write-while-busy events [`TxBusyWrites`] keeps (the total is
/// always counted), so a program that never polls cannot grow it without bound.
const TX_BUSY_WRITES_KEPT: usize = 16;

/// A `TX_DATA` store made while `STATUS.TX_BUSY` was set: on the board the
/// byte is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxBusyWrite {
    /// Address of the storing instruction.
    pub pc: u32,
    /// The byte that was dropped.
    pub byte: u8,
}

impl std::fmt::Display for TxBusyWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UART TX write while busy at PC 0x{:08X} (byte 0x{:02X} dropped)", self.pc, self.byte)
    }
}

/// The `TX_DATA` stores a run made while the transmitter was busy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TxBusyWrites {
    /// The first few, in order.
    pub first: Vec<TxBusyWrite>,
    /// How many there were in all.
    pub total: u64,
}

impl TxBusyWrites {
    /// True if no store was made while busy.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.total == 0
    }

    /// Count `write`, keeping it if it is one of the first few.
    fn record(&mut self, write: TxBusyWrite) {
        if self.first.len() < TX_BUSY_WRITES_KEPT {
            self.first.push(write);
        }
        self.total += 1;
    }
}

impl std::fmt::Display for TxBusyWrites {
    /// One line per kept write, then how many more there were.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, write) in self.first.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{write}")?;
        }
        let more = self.total - self.first.len() as u64;
        if more > 0 {
            write!(f, "\n... and {more} more")?;
        }
        Ok(())
    }
}

/// Interrupt controller and timer state, for a debugger's `monitor irq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IrqState {
    /// `INT_MASK`.
    pub mask: u8,
    /// `INT_PENDING` (bit 0 = timer, bit 2 = the source 2 cascade).
    pub pending: u8,
    /// `INT_VEC0..3`.
    pub vectors: [u32; 4],
    /// `TIMER_PERIOD`.
    pub timer_period: u32,
    /// `TIMER_COUNT`.
    pub timer_count: u32,
    /// Modelled cycles since reset.
    pub cycles: u64,
    /// True while parked in `WAIT`.
    pub waiting: bool,
}

impl Cpu {
    // ---- memory-mapped I/O ---------------------------------------------------

    /// Service an MMIO read (`addr[31:28] == 0xF`), returning the value in the low
    /// bits of a 64-bit word.  `addr[27:16] == 0x001` selects the UART; every
    /// other device/offset reads 0 (RTL `default: 0`).
    pub(super) fn mmio_read(&mut self, addr: u32) -> u64 {
        match (addr >> 16) & 0xFFF {
            IRQ_DEV_SELECT => return self.irq_mmio_read(addr & 0xFFFF),
            PERF_DEV_SELECT => {
                return match addr & 0xFFFF {
                    PERF_CYCLES => self.cycles - self.perf_base.cycles,
                    PERF_INSTR => self.instret - self.perf_base.instret,
                    _ => 0,
                };
            }
            SD_DEV_SELECT => {
                let now = self.cycles;
                return self.sd.as_mut().map_or(0, |sd| sd.read(addr & 0xFFFF, now));
            }
            CACHE_DEV_SELECT => {
                return match addr & 0xFFFF {
                    CACHE_STATUS => u64::from(self.cache_ctl.busy(self.cycles)),
                    CACHE_OP_ADDR if self.cache_ctl.range_ops => self.cache_ctl.op_addr,
                    CACHE_OP_LEN if self.cache_ctl.range_ops => self.cache_ctl.op_len,
                    CACHE_INFO => CACHE_INFO_VALUE,
                    ICACHE_INFO => ICACHE_INFO_VALUE,
                    CNT_ZERO_LINES => {
                        // The last hardware counter /proc/klausscpu_perf reads.
                        let instret = self.instret - self.perf_base.instret;
                        if let Some(sim) = self.tlbsim.get_mut() {
                            sim.report(instret, false);
                        }
                        if let Some(sim) = self.cachesim.as_mut() {
                            sim.report_window(instret, false);
                        }
                        self.zero_lines
                    }
                    _ => 0,
                };
            }
            UART_DEV_SELECT => {}
            _ => return 0,
        }
        match addr & 0xFFFF {
            UART_RX_DATA => u64::from(self.uart_rx.pop_front().unwrap_or(0)), // read-to-consume
            UART_STATUS => {
                // bit0 TX_BUSY, bit1 RX_EMPTY, bit2 RX_FULL (`KlaussCPU.sv` :911).
                let tx_busy = u64::from(self.tx_busy());
                let rx_empty = u64::from(self.uart_rx.is_empty());
                let rx_full = u64::from(self.uart_rx.len() >= UART_RX_FIFO_DEPTH);
                (rx_full << 2) | (rx_empty << 1) | tx_busy
            }
            _ => 0, // TX_DATA read-back / unmapped UART offset
        }
    }

    /// Service an MMIO write.  A UART `TX_DATA` store transmits `data[7:0]` (or,
    /// while the transmitter is busy, is dropped); every other device/offset
    /// is ignored (RTL `default: 0`).
    pub(super) fn mmio_write(&mut self, addr: u32, data: u64) {
        self.mmio_store(addr, data, 8);
    }

    /// Service an MMIO store of `bytes` (1, 2, 4 or 8) bytes: as
    /// [`Cpu::mmio_write`], with the width the SD scratch RAM needs.
    pub(super) fn mmio_store(&mut self, addr: u32, data: u64, bytes: u32) {
        match (addr >> 16) & 0xFFF {
            SD_DEV_SELECT => {
                let now = self.cycles;
                if let Some(sd) = self.sd.as_mut() {
                    sd.write(addr & 0xFFFF, data, bytes, now);
                }
            }
            UART_DEV_SELECT if (addr & 0xFFFF) == UART_TX_DATA => self.tx_byte((data & 0xFF) as u8),
            IRQ_DEV_SELECT => self.irq_mmio_write(addr & 0xFFFF, data),
            CACHE_DEV_SELECT => self.cache_mmio_write(addr & 0xFFFF, data),
            PERF_DEV_SELECT if addr & 0xFFFF == PERF_CTRL && data & 1 != 0 => {
                if let Some(sim) = self.tlbsim.get_mut() {
                    sim.clear();
                }
                if let Some(sim) = self.cachesim.as_mut() {
                    sim.clear();
                }
                self.perf_base = PerfBase {
                    cycles: self.cycles,
                    instret: self.instret,
                };
            }
            _ => {}
        }
    }

    // ---- cache controller (0xF005_0000) ---------------------------------------

    /// Write a cache-controller register.
    fn cache_mmio_write(&mut self, offset: u32, data: u64) {
        let now = self.cycles;
        let ctl = &mut self.cache_ctl;
        match offset {
            // Writable at any time: a range op copies them when it is
            // accepted (spec v1.1), so one queued behind a walk keeps the
            // values of its CACHE_CTRL write.
            CACHE_OP_ADDR if ctl.range_ops => ctl.op_addr = data & !(coherence::LINE - 1),
            CACHE_OP_LEN if ctl.range_ops => ctl.op_len = data & 0xFFFF_FFFF,
            CACHE_CTRL => {
                if data & CTRL_CLEAR != 0 {
                    self.zero_lines = 0;
                }
                if data & CTRL_ICACHE_INV != 0 {
                    // A 64-cycle sweep, not a walk: done at once.
                    if let Some(sim) = self.cachesim.as_mut() {
                        sim.icache_inv();
                    }
                    if let Some(coh) = self.coherence.as_deref_mut() {
                        coh.icache_all();
                    }
                }
                let coh = self.coherence.as_deref_mut();
                if data & (CTRL_FLUSH | CTRL_INVALIDATE) != 0 {
                    // The whole-cache op runs; range bits with it are ignored
                    // (RANGE_IINV included: it is a range op).
                    let dirty = coh.as_deref().map_or(0, |c| c.dirty_lines().min(DCACHE_LINES));
                    let op = if data & CTRL_INVALIDATE != 0 {
                        WalkOp::Invalidate
                    } else {
                        WalkOp::Flush
                    };
                    let seq = coh.map_or(0, coherence::Coherence::walk_accepted);
                    ctl.walk(now, WALK_FULL_CYCLES + dirty * WALK_CYCLES_PER_DIRTY_LINE, op, seq);
                    return;
                }
                if !ctl.range_ops || ctl.op_len == 0 {
                    return;
                }
                // The walk covers ceil(LEN / 32) lines from ADDR (whose
                // [4:0] are 0).
                let (start, len) = (ctl.op_addr, ctl.op_len);
                let lines = len.div_ceil(coherence::LINE);
                let clean = data & CTRL_RANGE_CLEAN != 0;
                let inv = data & CTRL_RANGE_INV != 0;
                if clean || inv {
                    let dirty = coh.as_deref().map_or(0, |c| c.dirty_lines_in(start, len));
                    let cycles = WALK_RANGE_CYCLES + lines * WALK_RANGE_CYCLES_PER_LINE + dirty * WALK_CYCLES_PER_DIRTY_LINE;
                    let seq = coh.map_or(0, coherence::Coherence::walk_accepted);
                    ctl.walk(now, cycles, WalkOp::Range { start, len, clean, inv }, seq);
                }
                if data & CTRL_RANGE_IINV != 0 {
                    ctl.walk(
                        now,
                        WALK_RANGE_CYCLES + lines * WALK_RANGE_CYCLES_PER_LINE,
                        WalkOp::IRange { start, len },
                        0,
                    );
                }
            }
            _ => {}
        }
    }

    /// Complete the cache walks whose time is up: their effect on the
    /// coherence checker (and a `RANGE_IINV`'s on the cache simulator,
    /// which drops its whole I-cache for it).
    fn cache_walks_complete(&mut self) {
        while let Some(&(at, op, seq)) = self.cache_ctl.pending.front() {
            if at > self.cycles {
                break;
            }
            self.cache_ctl.pending.pop_front();
            if let WalkOp::IRange { .. } = op
                && let Some(sim) = self.cachesim.as_mut()
            {
                sim.icache_inv();
            }
            let Some(coh) = self.coherence.as_deref_mut() else {
                continue;
            };
            match op {
                WalkOp::Flush => coh.flush_all(),
                WalkOp::Invalidate => coh.invalidate_all(),
                WalkOp::Range { start, len, clean, inv } => coh.range(start, len, clean, inv),
                WalkOp::IRange { start, len } => coh.icache_range(start, len),
            }
            if seq != 0 {
                coh.walk_completed(seq);
            }
        }
    }

    // ---- interrupt controller / timer (0xF00F_0000) ---------------------------

    /// Read an interrupt-controller register (RTL `KlaussCPU.sv` read mux).
    fn irq_mmio_read(&self, offset: u32) -> u64 {
        match offset {
            INT_MASK => u64::from(self.int_mask),
            INT_PENDING => u64::from(self.timer_pending) | (u64::from(self.int2_line()) << 2),
            INT_VEC0..=INT_VEC3 if offset.is_multiple_of(8) => u64::from(self.int_vec[((offset - INT_VEC0) / 8) as usize]),
            TIMER_PERIOD => u64::from(self.timer_period),
            TIMER_COUNT => u64::from(self.timer_count),
            MS_CLOCK => self.cycles / CYCLES_PER_MS,
            CPU_FEATURES => self.cpu_features(),
            INT2_EN => self.sd.as_ref().filter(|sd| sd.native()).map_or(0, |sd| u64::from(sd.int2_en())),
            INT2_PENDING => self.sd.as_ref().filter(|sd| sd.native()).map_or(0, |sd| u64::from(sd.int2_pending())),
            _ => 0,
        }
    }

    /// Write an interrupt-controller register; `INT_PENDING` / `TIMER_COUNT` are read-only.
    fn irq_mmio_write(&mut self, offset: u32, data: u64) {
        match offset {
            INT_MASK => self.int_mask = (data & 0xF) as u8,
            INT2_EN => {
                if let Some(sd) = self.sd.as_mut().filter(|sd| sd.native()) {
                    sd.set_int2_en(data);
                }
            }
            INT_VEC0..=INT_VEC3 if offset.is_multiple_of(8) => self.int_vec[((offset - INT_VEC0) / 8) as usize] = data as u32,
            TIMER_PERIOD => {
                self.timer_period = data as u32;
                self.timer_count = 0; // restart with the new period
            }
            _ => {}
        }
    }

    /// Advance modelled time: the cycle counter and the timer, which sets its
    /// pending bit when the count passes `TIMER_PERIOD` and restarts from 0.
    pub(super) fn tick(&mut self, cycles: u64) {
        self.cycles += cycles;
        if !self.cache_ctl.pending.is_empty() {
            self.cache_walks_complete();
        }
        if let Some(sd) = self.sd.as_deref_mut()
            && sd.dma_next_event().is_some_and(|at| at <= self.cycles)
        {
            let mut bus = DmaBus {
                mem: &mut self.mem,
                coherence: self.coherence.as_deref_mut(),
                cache: &self.cache_ctl,
                now: self.cycles,
            };
            sd.dma_advance(self.cycles, &mut bus);
        }
        let count = u64::from(self.timer_count) + cycles;
        if count > u64::from(self.timer_period) {
            self.timer_count = 0;
            self.timer_pending = true;
        } else {
            self.timer_count = count as u32;
        }
    }

    /// The source that would be dispatched now: pending, vectored and unmasked
    /// (priority timer > blitter > Ethernet).  The timer is source 0; with
    /// `--sd-native`, source 2 is the cascade, a level that stays pending
    /// until the SD status bits behind it are cleared.  The blitter is not
    /// modelled.
    pub(super) fn irq_ready(&self) -> Option<usize> {
        if self.timer_pending && self.timer_can_fire() {
            return Some(0);
        }
        (self.int_vec[2] != 0 && self.int_mask & 0b100 != 0 && self.int2_line()).then_some(2)
    }

    /// Interrupt source 2 with `--sd-native`: `|(INT2_PENDING & INT2_EN)`
    /// (`SD_NATIVE_PLAN.md` §4.5); without it, 0, as Ethernet is not modelled.
    fn int2_line(&self) -> bool {
        self.sd.as_ref().is_some_and(|sd| sd.native() && sd.int2_line())
    }

    /// True if the timer can raise a dispatchable interrupt.
    pub(super) const fn timer_can_fire(&self) -> bool {
        self.int_vec[0] != 0 && self.int_mask & 1 != 0
    }

    /// Jump modelled time to the timer's next expiry (sets it pending).
    pub(super) fn skip_to_timer_expiry(&mut self) {
        let remaining = u64::from(self.timer_period.saturating_sub(self.timer_count)) + 1;
        self.tick(remaining);
    }

    /// Interrupt entry (`pipeline_core.sv` IRQ sequencer): push the context
    /// word at SP-8, mask the source, acknowledge it, jump to its vector.
    /// From user mode the frame goes on the kernel stack, with PM set
    /// (`MMU_PLAN.md` §3.3, §4.5); DI and `ECAUSE` are left alone.
    /// With `TE` = 1 and a kernel SP outside kseg the entry is a double
    /// fault instead ([`Cpu::enter`]), and the interrupt stays pending.
    pub(super) fn take_irq(&mut self, src: usize) {
        if !self.enter(self.pc) {
            return;
        }
        self.int_mask &= !(1 << src);
        if src == 0 {
            self.timer_pending = false;
        }
        self.pc = self.int_vec[src];
    }

    /// Interrupt controller and timer state.
    #[must_use]
    pub fn irq_state(&self) -> IrqState {
        IrqState {
            mask: self.int_mask,
            pending: u8::from(self.timer_pending) | (u8::from(self.int2_line()) << 2),
            vectors: self.int_vec,
            timer_period: self.timer_period,
            timer_count: self.timer_count,
            cycles: self.cycles,
            waiting: self.waiting,
        }
    }

    /// Feed bytes into the UART RX FIFO (for tests / an external input source).
    pub fn feed_uart_rx(&mut self, bytes: &[u8]) {
        self.uart_rx.extend(bytes.iter().copied());
    }

    /// Type `bytes` into the UART receiver as [`Cpu::run`] goes (`--uart-in`):
    /// a byte moves into the RX FIFO when the FIFO isn't full and at least
    /// [`UART_SCRIPT_GAP`] instructions have retired since the last one, so
    /// a program that polls (or reads on a timer tick) sees typed input, and
    /// nothing is lost to a full FIFO.
    ///
    /// With a non-empty `prompt` (`--uart-prompt`), each line (the first one
    /// too) waits until the program has printed `prompt` since the previous
    /// line's newline, so the transcript reads as if typed at the prompt.
    pub fn set_uart_script(&mut self, bytes: &[u8], prompt: &[u8]) {
        self.uart_script = bytes.iter().copied().collect();
        self.uart_script_wait = UART_SCRIPT_GAP;
        self.uart_script_prompt = prompt.to_vec();
        self.uart_script_waiting_from = (!prompt.is_empty()).then_some(self.uart.len());
    }

    /// Move the next scripted byte into the RX FIFO if it is due.  With a
    /// prompt, the run stops ([`StopReason::InputDone`]) once the last line
    /// has been typed and the prompt is back.
    pub(super) fn pump_uart_script(&mut self) {
        if let Some(from) = self.uart_script_waiting_from {
            // Only output that arrived since the last look can complete the
            // prompt (keep the last prompt.len() - 1 bytes for an overlap).
            let prompt = &self.uart_script_prompt;
            let end = self.uart.len();
            if end == from {
                return;
            }
            let start = from.max(self.uart_script_checked.saturating_sub(prompt.len() - 1));
            self.uart_script_checked = end;
            if end < start + prompt.len() || !self.uart[start..].windows(prompt.len()).any(|w| w == prompt) {
                return;
            }
            self.uart_script_waiting_from = None;
            if self.uart_script.is_empty() {
                self.stop = Some(StopReason::InputDone);
                return;
            }
        }
        if self.uart_script.is_empty() {
            return;
        }
        if self.uart_script_wait > 0 {
            self.uart_script_wait -= 1;
        } else if self.uart_rx.len() < UART_RX_FIFO_DEPTH
            && let Some(b) = self.uart_script.pop_front()
        {
            self.uart_rx.push_back(b);
            self.uart_script_wait = UART_SCRIPT_GAP;
            if b == b'\n' && !self.uart_script_prompt.is_empty() {
                self.uart_script_waiting_from = Some(self.uart.len());
            }
        }
    }

    // ---- UART output ---------------------------------------------------------

    /// True while the transmitter is sending (`STATUS.TX_BUSY`).
    const fn tx_busy(&self) -> bool {
        self.cycles < self.tx_busy_until
    }

    /// A UART `TX_DATA` store: start sending `b`, or, if a byte is still being
    /// sent, drop it as the RTL does and record the store.
    fn tx_byte(&mut self, b: u8) {
        if self.tx_busy() {
            self.tx_busy_writes.record(TxBusyWrite { pc: self.pc, byte: b });
            return;
        }
        self.uart.push(b);
        self.tx_busy_until = self.cycles + UART_CYCLES_PER_STORE;
    }

    /// Take the `TX_DATA` stores made while busy since the last call.
    pub fn take_tx_busy_writes(&mut self) -> TxBusyWrites {
        std::mem::take(&mut self.tx_busy_writes)
    }

    /// Take the UART output produced since the last call.
    pub fn take_uart(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.uart)
    }
}
