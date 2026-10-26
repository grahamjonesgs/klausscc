//! Memory-mapped devices: the UART, the interrupt controller / timer and
//! the performance counters.

use super::{CYCLES_PER_MS, Cpu, PerfBase};

/// Interrupt controller / timer device select (`addr[27:16]`).
const IRQ_DEV_SELECT: u32 = 0x00F;
/// Performance-counter device select (`addr[27:16]`).
const PERF_DEV_SELECT: u32 = 0x00D;

// ---- Memory-mapped I/O (bus_splitter address decode) ------------------------
// The RTL bus_splitter routes addr[31:28] == 0xF to the MMIO space; within that,
// addr[27:16] selects the device (0x001 == UART) and addr[15:0] is the register
// offset.  See KlaussCPU.sv:843 (default read 0) and 2796-2800 (TX fire).

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
const UART_RX_FIFO_DEPTH: usize = 16;

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
                    0x0008 => self.cycles - self.perf_base.cycles,   // PERF_CYCLES
                    0x0010 => self.instret - self.perf_base.instret, // PERF_INSTR
                    _ => 0,
                };
            }
            UART_DEV_SELECT => {}
            _ => return 0,
        }
        match addr & 0xFFFF {
            UART_RX_DATA => u64::from(self.uart_rx.pop_front().unwrap_or(0)), // read-to-consume
            UART_STATUS => {
                // bit0 TX_BUSY (instant-transmit model → always 0),
                // bit1 RX_EMPTY, bit2 RX_FULL.
                let rx_empty = u64::from(self.uart_rx.is_empty());
                let rx_full = u64::from(self.uart_rx.len() >= UART_RX_FIFO_DEPTH);
                (rx_full << 2) | (rx_empty << 1)
            }
            _ => 0, // TX_DATA read-back / unmapped UART offset
        }
    }

    /// Service an MMIO write.  A UART `TX_DATA` store transmits `data[7:0]`; every
    /// other device/offset is ignored (RTL `default: 0`).
    pub(super) fn mmio_write(&mut self, addr: u32, data: u64) {
        match (addr >> 16) & 0xFFF {
            UART_DEV_SELECT if (addr & 0xFFFF) == UART_TX_DATA => self.tx_byte((data & 0xFF) as u8),
            IRQ_DEV_SELECT => self.irq_mmio_write(addr & 0xFFFF, data),
            PERF_DEV_SELECT if addr as u16 == 0 && data & 1 != 0 => {
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
            0x0000 => u64::from(self.int_mask),
            0x0008 => u64::from(self.timer_pending), // INT_PENDING: [0]=timer (blitter/eth not modelled)
            0x0010 | 0x0018 | 0x0020 | 0x0028 => u64::from(self.int_vec[((offset - 0x10) / 8) as usize]),
            0x0030 => u64::from(self.timer_period),
            0x0038 => u64::from(self.timer_count),
            0x0040 => self.cycles / CYCLES_PER_MS, // free-running millisecond clock
            _ => 0,
        }
    }

    /// Write an interrupt-controller register; `INT_PENDING` / `TIMER_COUNT` are read-only.
    fn irq_mmio_write(&mut self, offset: u32, data: u64) {
        match offset {
            0x0000 => self.int_mask = (data & 0xF) as u8,
            0x0010 | 0x0018 | 0x0020 | 0x0028 => self.int_vec[((offset - 0x10) / 8) as usize] = data as u32,
            0x0030 => {
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
        let ctx = (u64::from(self.int_mask) << 39) | (u64::from(self.flags.to_ctx()) << 32) | u64::from(self.pc);
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

    /// Emit a raw byte to the UART capture (a UART `TX_DATA` MMIO store).
    fn tx_byte(&mut self, b: u8) {
        self.uart.push(b as char);
    }

    /// Take the UART output produced since the last call.
    pub fn take_uart(&mut self) -> String {
        std::mem::take(&mut self.uart)
    }
}
