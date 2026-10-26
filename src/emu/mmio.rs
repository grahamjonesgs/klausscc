//! Memory-mapped devices: the UART, the interrupt controller / timer and
//! the performance counters.

use super::{CTX_FLAGS_SHIFT, CTX_INT_MASK_SHIFT, CYCLES_PER_MS, Cpu, PerfBase};

/// Interrupt controller / timer device select (`addr[27:16]`).
const IRQ_DEV_SELECT: u32 = 0x00F;
/// Performance-counter device select (`addr[27:16]`).
const PERF_DEV_SELECT: u32 = 0x00D;

// Interrupt controller / timer register offsets (`addr[15:0]`).
/// `INT_MASK`: per-source enables (4 bits).
const INT_MASK: u32 = 0x0000;
/// `INT_PENDING` (read-only): bit 0 = timer (blitter / Ethernet not modelled).
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
/// The FIFO starts empty.  That matches the board only once the RX FIFO is
/// flushed at `LOAD_COMPLETE`, a change proposed but not yet in the RTL: until
/// then a byte left in the FIFO when the load completes is still there when
/// the program starts.
const UART_RX_FIFO_DEPTH: usize = 16;

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
// idle, so its first store is taken (`KlaussCPU.sv` `START_WAIT` change of
// 2026-10-26): `LOAD_COMPLETE` clears `r_start_wait_counter` and
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
    /// `INT_PENDING` (bit 0 = timer).
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
        match (addr >> 16) & 0xFFF {
            UART_DEV_SELECT if (addr & 0xFFFF) == UART_TX_DATA => self.tx_byte((data & 0xFF) as u8),
            IRQ_DEV_SELECT => self.irq_mmio_write(addr & 0xFFFF, data),
            PERF_DEV_SELECT if addr & 0xFFFF == PERF_CTRL && data & 1 != 0 => {
                self.perf_base = PerfBase {
                    cycles: self.cycles,
                    instret: self.instret,
                };
            }
            _ => {}
        }
    }

    // ---- interrupt controller / timer (0xF00F_0000) ---------------------------

    /// Read an interrupt-controller register (RTL `KlaussCPU.sv` read mux).
    fn irq_mmio_read(&self, offset: u32) -> u64 {
        match offset {
            INT_MASK => u64::from(self.int_mask),
            INT_PENDING => u64::from(self.timer_pending),
            INT_VEC0..=INT_VEC3 if offset.is_multiple_of(8) => u64::from(self.int_vec[((offset - INT_VEC0) / 8) as usize]),
            TIMER_PERIOD => u64::from(self.timer_period),
            TIMER_COUNT => u64::from(self.timer_count),
            MS_CLOCK => self.cycles / CYCLES_PER_MS,
            _ => 0,
        }
    }

    /// Write an interrupt-controller register; `INT_PENDING` / `TIMER_COUNT` are read-only.
    fn irq_mmio_write(&mut self, offset: u32, data: u64) {
        match offset {
            INT_MASK => self.int_mask = (data & 0xF) as u8,
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
        let count = u64::from(self.timer_count) + cycles;
        if count > u64::from(self.timer_period) {
            self.timer_count = 0;
            self.timer_pending = true;
        } else {
            self.timer_count = count as u32;
        }
    }

    /// The source that would be dispatched now: pending, vectored and unmasked
    /// (priority timer > blitter > Ethernet; only the timer is modelled).
    pub(super) fn irq_ready(&self) -> Option<usize> {
        (self.timer_pending && self.timer_can_fire()).then_some(0)
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
    pub(super) fn take_irq(&mut self, src: usize) {
        let ctx = (u64::from(self.int_mask) << CTX_INT_MASK_SHIFT) | (u64::from(self.flags.to_ctx()) << CTX_FLAGS_SHIFT) | u64::from(self.pc);
        self.push(ctx);
        self.int_mask &= !(1 << src);
        if src == 0 {
            self.timer_pending = false;
        }
        self.pc = self.int_vec[src];
        self.waiting = false;
    }

    /// Interrupt controller and timer state.
    #[must_use]
    pub const fn irq_state(&self) -> IrqState {
        IrqState {
            mask: self.int_mask,
            pending: self.timer_pending as u8,
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
