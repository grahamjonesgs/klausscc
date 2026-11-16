//! The SD card controller at MMIO `0xF000_0000` (device select `0x000`)
//! and the card behind it (`--sd-image`, `--sd-native`).
//!
//! Without either option the emulator has no SD controller at all: the
//! whole window reads 0 and ignores writes, as it always has.  Note that
//! the card-detect pin is active low (`SD_STATUS[1]` = 0 with a card in,
//! measured on the board), so that all-zero window reads as "card present"
//! on the raw pin, while every command gets no answer (MISO reads 0).
//!
//! **SPI mode** (`sd_spi.sv`, `MMIO_MAP.md` "SD card"): `SD_CTRL` (+0x00)
//! `[15:0]` clock divider, `[16]` CS high, `[17]` slot power; `SD_DATA`
//! (+0x08), whose write starts an 8-bit exchange and whose read returns the
//! last byte received; `SD_STATUS` (+0x10) `[0]` BUSY, `[1]` the raw,
//! active-low card-detect pin (0 = a card is in); and the
//! 512-byte scratch RAM `SD_BUF` (+0x200..+0x3F8, `addr[9]` set). Writing
//! `SD_DATA` while BUSY is ignored, as in the RTL.
//!
//! An exchange takes `16 * (clkdiv + 1) + 1` cycles on the modelled clock,
//! but it also ends at the first `SD_STATUS` read that sees it running:
//! that read returns BUSY = 1, and the next one 0. So a polling loop spins
//! once per byte, however slow the clock, while the received byte lands in
//! `SD_DATA` only when the exchange ends. Zephyr's and Linux's pipelined
//! read loop (start byte i+1, read byte i, then read STATUS to check that
//! i+1 is still shifting) therefore sees BUSY right after its `SD_DATA`
//! write and never reports a late read, and the plain loop (write, poll,
//! read) gets each byte.
//!
//! **Native SD mode** (`--sd-native`; klaussCPU `SD_NATIVE_PLAN.md` v1,
//! §4.2-§4.7): `SD_CTRL[18]` `SD_MODE`, and the `SDM_*` registers at
//! +0x100..+0x1F8: a command engine, a data engine with a 4 KiB FIFO
//! (`SDM_DATA`, 64-bit, little-endian), sticky write-1-to-clear status
//! bits and an interrupt that reaches the CPU through the source 2 cascade
//! (`INT2_EN`, `INT2_PENDING`). Reset values and corner cases follow the
//! plan's §4.3 and §4.8 (klaussCPU mmu-sd 57a5a8c): blocks of 4 to
//! 2048 bytes in steps of 4, ceil(size / 8) FIFO words each. Everything happens at once: a command
//! completes during the `SDM_CMD` store (`CMD_BUSY` never reads 1), and the
//! data engine moves whole blocks between the card and the FIFO whenever
//! the FIFO has room for one (a read) or holds one (a write), so `DATA_BUSY`
//! lasts until the block count is exhausted, an error, or a CMD12. No
//! timeout fires for a present card that answers; one that does not answer
//! (an illegal command, a read past its end) times out at once.
//!
//! **DMA** (`--sd-dma`, with `--sd-native`; the klaussCPU SD DMA spec
//! v1.2): `SDM_DMA_ADDR`, `SDM_CFG[16]` `DMA_EN`, `SDM_STATUS[5]`
//! `DMA_BUSY` and `[27]` `DMA_ERR`, `SDM_CAPS[10]`.  A data command written
//! with `DMA_EN` moves its blocks between the card and DDR, and unlike PIO
//! it takes modelled time: the card side finishes a block every
//! [`SdController::block_cycles`], driven from [`Cpu::tick`](super::Cpu),
//! a read storing it into DDR as it completes and a write fetching blocks
//! into the FIFO ahead of the card.  See `doc/EMULATOR_ISA_SEMANTICS.md`,
//! "SD DMA".

mod card;
mod fault;
#[cfg(test)]
mod tests;

pub use card::SdCard;
use card::{ReadBlock, Response, WriteBlock, crc7};
pub use fault::{FaultKind, SdFault};
use std::collections::VecDeque;

// ---- register offsets (`addr[15:0]`) ----
/// `SD_CTRL`.
const SD_CTRL: u32 = 0x000;
/// `SD_DATA`.
const SD_DATA: u32 = 0x008;
/// `SD_STATUS`.
const SD_STATUS: u32 = 0x010;
/// `SDM_CMD` (write-only).
const SDM_CMD: u32 = 0x100;
/// `SDM_ARG`.
const SDM_ARG: u32 = 0x108;
/// `SDM_RESP0`.
const SDM_RESP0: u32 = 0x110;
/// `SDM_RESP1`.
const SDM_RESP1: u32 = 0x118;
/// `SDM_BLK`.
const SDM_BLK: u32 = 0x120;
/// `SDM_STATUS`.
const SDM_STATUS: u32 = 0x128;
/// `SDM_IRQ_EN`.
const SDM_IRQ_EN: u32 = 0x130;
/// `SDM_TIMEOUT`.
const SDM_TIMEOUT: u32 = 0x138;
/// `SDM_CFG`.
const SDM_CFG: u32 = 0x140;
/// `SDM_FIFO`.
const SDM_FIFO: u32 = 0x148;
/// `SDM_DMA_ADDR` (`--sd-dma`; reserved, reading 0, without it).
const SDM_DMA_ADDR: u32 = 0x150;
/// `SDM_BLKDONE`.
const SDM_BLKDONE: u32 = 0x158;
/// `SDM_DATA`, the FIFO port.
const SDM_DATA: u32 = 0x180;
/// `SDM_CAPS`.
const SDM_CAPS: u32 = 0x1F8;
/// `addr[9]` selects `SD_BUF`.
const SD_BUF_SELECT: u32 = 0x200;

// ---- SD_CTRL / SD_STATUS bits ----
/// `SD_CTRL[16]`: CS high (deasserted).
const CTRL_CS_HI: u64 = 1 << 16;
/// `SD_CTRL[17]`: slot power on.
const CTRL_PWR_ON: u64 = 1 << 17;
/// `SD_CTRL[18]`: native SD mode (`--sd-native` only).
const CTRL_SD_MODE: u64 = 1 << 18;
/// `SD_CTRL` reset value: divider 249 (200 kHz), CS high, power off.
const CTRL_RESET: u64 = CLK_DIV_INIT | CTRL_CS_HI;
/// `SD_CTRL[15:0]` reset value: 249, 200 kHz.
const CLK_DIV_INIT: u64 = 249;
/// `SD_STATUS[0]`: an exchange is running.
const STATUS_BUSY: u64 = 1;
/// `SD_STATUS[1]`: the raw card-detect pin, active low: 1 = the slot is
/// empty, 0 = a card is in (Nexys A7, measured on the board).
const STATUS_NO_CARD: u64 = 1 << 1;

// ---- SDM_CMD fields ----
/// `[5:0]` the command index.
const CMD_INDEX: u64 = 0x3F;
/// `[10:8]` the response type.
const CMD_RESP_SHIFT: u32 = 8;
/// `[11]` a data phase follows.
const CMD_DATA: u64 = 1 << 11;
/// `[12]` the data phase writes.
const CMD_WRITE: u64 = 1 << 12;
/// `[13]` stop: this CMD12 ends a running data phase.
const CMD_STOP: u64 = 1 << 13;

// ---- SDM_STATUS bits ----
/// `[1]` `DATA_BUSY` (live).
const ST_DATA_BUSY: u32 = 1 << 1;
/// `[2]` DAT0 level (live).
const ST_DAT0: u32 = 1 << 2;
/// `[3]` `CARD_PRESENT` (live).
const ST_CARD_PRESENT: u32 = 1 << 3;
/// `[4]` `FIFO_THRESH` (live, §4.5).
const ST_FIFO_THRESH: u32 = 1 << 4;
/// `[5]` `DMA_BUSY` (live, `--sd-dma`).
const ST_DMA_BUSY: u32 = 1 << 5;
/// `[8]` `CMD_DONE`.
const ST_CMD_DONE: u32 = 1 << 8;
/// `[9]` `DATA_DONE`.
const ST_DATA_DONE: u32 = 1 << 9;
/// `[16]` `CMD_TIMEOUT`.
const ST_CMD_TIMEOUT: u32 = 1 << 16;
/// `[17]` `CMD_CRC_ERR`.
const ST_CMD_CRC_ERR: u32 = 1 << 17;
/// `[18]` `CMD_END_ERR`.
const ST_CMD_END_ERR: u32 = 1 << 18;
/// `[19]` `CMD_IDX_ERR`.
const ST_CMD_IDX_ERR: u32 = 1 << 19;
/// `[20]` `DATA_TIMEOUT`.
const ST_DATA_TIMEOUT: u32 = 1 << 20;
/// `[21]` `DATA_CRC_ERR`.
const ST_DATA_CRC_ERR: u32 = 1 << 21;
/// `[23]` `WRITE_CRC_ERR`.
const ST_WRITE_CRC_ERR: u32 = 1 << 23;
/// `[24]` `WRITE_NO_TOKEN`.
const ST_WRITE_NO_TOKEN: u32 = 1 << 24;
/// `[25]` `BUSY_TIMEOUT`.
const ST_BUSY_TIMEOUT: u32 = 1 << 25;
/// `[26]` `FIFO_ERR`.
const ST_FIFO_ERR: u32 = 1 << 26;
/// `[27]` `DMA_ERR` (`--sd-dma`): the DMA address left DDR.
const ST_DMA_ERR: u32 = 1 << 27;
/// `[31]` `ERR_ANY` (live): any of `[27:16]`.
const ST_ERR_ANY: u32 = 1 << 31;
/// The error bits `[26:16]` (without DMA).
const ST_ERRORS: u32 = 0x07FF_0000;
/// The sticky, write-1-to-clear bits (without DMA).
const ST_STICKY: u32 = ST_CMD_DONE | ST_DATA_DONE | ST_ERRORS;

// ---- other native registers ----
/// `SDM_BLK`: `[11:2]` of the block size and `[31:16]` the count.
const BLK_MASK: u64 = 0xFFFF_0FFC;
/// `SDM_BLK` reset (§4.8): 512 bytes, 1 block.
const BLK_RESET: u64 = (1 << 16) | 0x200;
/// `SDM_TIMEOUT` reset: 2 500 000 data / 12 500 000 busy CLK cycles.
const TIMEOUT_RESET: u64 = (BUSY_TIMEOUT_RESET << 32) | DATA_TIMEOUT_RESET;
/// `SDM_TIMEOUT[31:0]` reset: 100 ms at 25 MHz.
const DATA_TIMEOUT_RESET: u64 = 2_500_000;
/// `SDM_TIMEOUT[63:32]` reset: 500 ms at 25 MHz.
const BUSY_TIMEOUT_RESET: u64 = 12_500_000;
/// `SDM_CFG`: `[0]` 4-bit and `[2]` `CLK_ON` (the others are reserved, read 0).
const CFG_MASK: u64 = 0b101;
/// `SDM_CFG[0]`: 4-bit bus.
const CFG_4BIT: u64 = 1;
/// `SDM_CFG[16]` `DMA_EN` (`--sd-dma`; reserved, reading 0, without it).
/// (`[1]` is the high-speed edge and `[7:4]` the sample phase, which this
/// model, with no high speed, leaves reserved.)
const CFG_DMA_EN: u64 = 1 << 16;
/// `SDM_FIFO[31]` (write): reset the FIFO.
const FIFO_RESET: u64 = 1 << 31;
/// FIFO depth in 64-bit words (4 KiB, 8 blocks).
const FIFO_WORDS: usize = 512;
/// `SDM_CAPS`: version 1, 4-bit, no high speed, 8 blocks of FIFO.
const CAPS: u64 = (8 << 16) | (1 << 8) | 1;
/// `SDM_CAPS[10]`: DMA present (`--sd-dma`).
const CAPS_DMA: u64 = 1 << 10;
/// `SDM_DMA_ADDR[2:0]` read 0.
const DMA_ADDR_MASK: u64 = !7;
/// Cycles `DMA_BUSY` stays up after an abort (the spec: "within a few
/// hundred cycles"; the burst in flight completes).
const DMA_ABORT_DRAIN: u64 = 200;
/// DDR latency of a DMA burst, in cycles (spec v1.1: ~44, pipelined).
const DMA_DDR_LATENCY: u64 = 44;
/// FIFO words per DDR burst (32 bytes).
const DMA_BURST_WORDS: usize = 4;
/// SD clocks from a data command's store to its first data block: the
/// command (48), Ncr and the response (56), and Nac / Nwr.
const DMA_FIRST_BLOCK_CLOCKS: u64 = 120;
/// SD clocks a block takes besides its data: start bit, CRC16, end bit and
/// the gap before the next (Nac); a written block adds the CRC status
/// token and a short busy.
const DMA_BLOCK_OVERHEAD_CLOCKS: u64 = 1 + 16 + 1 + 8;
/// The CRC status and busy of a written block, in SD clocks.
const DMA_WRITE_EXTRA_CLOCKS: u64 = 8 + 16;
/// `INT2_EN` reset value: Ethernet only.
const INT2_EN_RESET: u8 = 0b01;

/// An SPI exchange in flight.
#[derive(Debug, Clone, Copy)]
struct Shift {
    /// The byte the card sent.
    rx: u8,
    /// The cycle the exchange ends.
    done_at: u64,
}

/// A native data phase in progress (`SDM_STATUS.DATA_BUSY`).
#[derive(Debug, Clone, Copy)]
#[expect(clippy::struct_excessive_bools, reason = "independent flags of one data phase")]
struct Phase {
    /// Host to card.
    write: bool,
    /// Block size in bytes (`SDM_BLK[11:0]` when the command was written).
    size: usize,
    /// Block count, 0 = open-ended.
    count: u16,
    /// The blocks move by DMA (`SDM_CFG.DMA_EN` when the command was
    /// written); cleared by an abort, after which the phase goes on as PIO.
    dma: bool,
    /// DMA write: blocks fetched from DDR into the FIFO.
    fetched: u32,
    /// DMA: the cycle the card side finishes its current block.
    block_at: u64,
    /// DMA read: every block is in; `DATA_DONE` (and `DMA_BUSY` = 0) at
    /// this cycle, when the last block's bursts are in DDR.
    done_at: Option<u64>,
    /// DMA: the engine moves nothing (`--sd-fault dmastall` and worse).
    stalled: bool,
    /// DMA, open-ended: `DMA_ERR` at the first block (`--sd-fault dmaerr`).
    fail: bool,
}

impl Phase {
    /// Block size in FIFO words.
    const fn words(&self) -> usize {
        block_words(self.size)
    }
}

/// The SD controller and its card.
#[derive(Debug)]
pub(super) struct SdController {
    /// The native controller is modelled (`--sd-native`).
    native: bool,
    /// The card in the slot.
    card: Option<SdCard>,
    /// `SD_CTRL` (bit 18 only with `native`).
    ctrl: u64,
    /// The last byte received (`SD_DATA` read; reset 0xFF).
    rx_byte: u8,
    /// The exchange in flight.
    shift: Option<Shift>,
    /// `SD_BUF`.
    buf: [u64; 64],
    /// `SDM_ARG`.
    arg: u32,
    /// `SDM_RESP0`, `SDM_RESP1`.
    resp: [u64; 2],
    /// `SDM_BLK`.
    blk: u64,
    /// The sticky `SDM_STATUS` bits.
    status: u32,
    /// `SDM_IRQ_EN`.
    irq_en: u32,
    /// `SDM_TIMEOUT`.
    timeout: u64,
    /// `SDM_CFG`.
    cfg: u64,
    /// The FIFO.
    fifo: VecDeque<u64>,
    /// `SDM_BLKDONE`.
    blkdone: u16,
    /// The data phase running.
    phase: Option<Phase>,
    /// The direction latch `DIR` (§4.5): `None` after a reset or a FIFO
    /// reset, else whether the last data command wrote.
    dir: Option<bool>,
    /// Words pushed since the last FIFO reset (a pre-fill included).
    pushed: u64,
    /// The block size in bytes latched by the last data command, which
    /// `FIFO_THRESH` uses during the phase and after it.
    thresh_size: usize,
    /// `INT2_EN` (in the interrupt controller, but only with `native`).
    int2_en: u8,
    /// The DMA engine is modelled (`--sd-dma`).
    dma: bool,
    /// `SDM_DMA_ADDR`.
    dma_addr: u64,
    /// `DMA_BUSY` stays up until this cycle after an abort.
    dma_drain_until: u64,
    /// A read block on its way to DDR: the cycle its last burst lands, and
    /// its bytes, for `SDM_DMA_ADDR`.  Until then DDR has the old bytes
    /// and `SDM_DMA_ADDR` lags (it advances as the bursts land), so a
    /// driver that reads it, or unmaps the buffer, before `DMA_BUSY`
    /// drops sees a short count or gets a DMA write after its invalidate
    /// (which the coherence checker reports at the CPU's next access).
    dma_store: Option<(u64, Vec<u8>)>,
    /// `--sd-fault dmahang` / `dmastuck`: the engine is wedged, `DMA_BUSY`
    /// reads 1 until `SD_MODE` is cleared (hang) or for good (stuck).
    dma_wedged: Option<FaultKind>,
    /// DDR's size: `DMA_ERR` past it.
    ddr_size: u64,
    /// What the DMA engine did, for the run's summary.
    dma_stats: DmaStats,
}

/// What the DMA engine did in a run.
#[derive(Debug, Default, Clone, Copy)]
struct DmaStats {
    /// Data commands that moved their blocks by DMA.
    commands: u64,
    /// Blocks read from the card into DDR.
    read_blocks: u64,
    /// Blocks written from DDR to the card.
    write_blocks: u64,
    /// DMAs aborted by `DMA_EN` = 0 or `SDM_FIFO[31]`.
    aborts: u64,
    /// `DMA_ERR`s.
    errors: u64,
    /// Data phases a card error ended during a DMA.
    card_errors: u64,
}

/// The DMA master's view of DDR: the RAM, the coherence checker and the
/// cache walks still running.
pub(super) struct DmaBus<'a> {
    /// The RAM (its length is DDR's size).
    pub(super) mem: &'a mut [u8],
    /// The `--sd-dma` coherence checker.
    pub(super) coherence: Option<&'a mut crate::emu::coherence::Coherence>,
    /// The cache controller, for the D-cache walks still running at `now`.
    pub(super) cache: &'a crate::emu::mmio::CacheCtl,
    /// The current cycle.
    pub(super) now: u64,
}

impl DmaBus<'_> {
    /// `len` bytes at `addr`, if they are all in DDR.
    fn range(&self, addr: u64, len: usize) -> Option<std::ops::Range<usize>> {
        let start = usize::try_from(addr).ok()?;
        let end = start.checked_add(len)?;
        (end <= self.mem.len()).then_some(start..end)
    }

    /// Memory to card: `len` bytes at `addr`, or `None` outside DDR.
    fn read(&mut self, addr: u64, len: usize) -> Option<Vec<u8>> {
        let range = self.range(addr, len)?;
        if let Some(coh) = self.coherence.as_deref_mut() {
            coh.dma_read(addr, len as u64, self.cache.dwalk_covers(self.now, addr, len as u64));
        }
        Some(self.mem[range].to_vec())
    }

    /// Card to memory: `data` at `addr`; false outside DDR (nothing written).
    fn write(&mut self, addr: u64, data: &[u8]) -> bool {
        let Some(range) = self.range(addr, data.len()) else {
            return false;
        };
        if let Some(coh) = self.coherence.as_deref_mut() {
            coh.dma_write(addr, data.len() as u64, self.cache.dwalk_covers(self.now, addr, data.len() as u64));
        }
        self.mem[range].copy_from_slice(data);
        true
    }
}

impl SdController {
    /// A controller at reset with `card` in the slot; `native` adds the SD
    /// mode registers, and `dma` (with `native`) the DMA engine.
    pub(super) fn new(card: Option<SdCard>, native: bool, dma: bool) -> Self {
        Self {
            native,
            card,
            ctrl: CTRL_RESET,
            rx_byte: 0xFF,
            shift: None,
            buf: [0; 64],
            arg: 0,
            resp: [0; 2],
            blk: BLK_RESET,
            status: 0,
            irq_en: 0,
            timeout: TIMEOUT_RESET,
            cfg: 0,
            fifo: VecDeque::with_capacity(FIFO_WORDS),
            blkdone: 0,
            phase: None,
            dir: None,
            pushed: 0,
            thresh_size: BLK_RESET as usize & 0xFFF,
            int2_en: INT2_EN_RESET,
            dma: native && dma,
            dma_addr: 0,
            dma_drain_until: 0,
            dma_store: None,
            dma_wedged: None,
            ddr_size: u64::MAX,
            dma_stats: DmaStats::default(),
        }
    }

    /// DDR's size in bytes: a DMA that would touch a byte at or above it
    /// gets `DMA_ERR`.
    pub(super) const fn set_ddr_size(&mut self, size: u64) {
        self.ddr_size = size;
    }

    /// The DMA engine's summary for the run, with `--sd-dma`.
    pub(super) fn dma_summary(&self) -> Option<String> {
        let s = self.dma_stats;
        self.dma.then(|| {
            format!(
                "SD DMA: {} data commands, {} blocks read, {} written, {} aborts, {} DMA_ERR, {} card errors",
                s.commands, s.read_blocks, s.write_blocks, s.aborts, s.errors, s.card_errors
            )
        })
    }

    /// The native controller is modelled.
    pub(super) const fn native(&self) -> bool {
        self.native
    }

    /// The card, if any.
    pub(super) const fn card(&self) -> Option<&SdCard> {
        self.card.as_ref()
    }

    /// The card, if any, to change.
    pub(super) const fn card_mut(&mut self) -> Option<&mut SdCard> {
        self.card.as_mut()
    }

    /// `SD_MODE` is set.
    const fn sd_mode(&self) -> bool {
        self.ctrl & CTRL_SD_MODE != 0
    }

    // ---- MMIO ----

    /// Read the register at `offset` (`addr[15:0]`) at cycle `now`.
    pub(super) fn read(&mut self, offset: u32, now: u64) -> u64 {
        if offset & SD_BUF_SELECT != 0 {
            // A sub-word load takes its lanes from addr[2:0], as from RAM.
            return self.buf[((offset >> 3) & 63) as usize] >> (8 * (offset & 7));
        }
        match offset {
            SD_CTRL => self.ctrl,
            SD_DATA => {
                self.settle(now);
                u64::from(self.rx_byte)
            }
            SD_STATUS => {
                let card = if self.card.is_some() { 0 } else { STATUS_NO_CARD };
                let busy = match self.shift.take() {
                    // The first poll that finds the byte shifting sees BUSY,
                    // and ends the exchange.
                    Some(shift) => {
                        self.rx_byte = shift.rx;
                        if now < shift.done_at { STATUS_BUSY } else { 0 }
                    }
                    None => 0,
                };
                card | busy
            }
            _ if !self.native => 0,
            SDM_ARG => u64::from(self.arg),
            SDM_RESP0 => self.resp[0],
            SDM_RESP1 => self.resp[1],
            SDM_BLK => self.blk,
            SDM_STATUS => {
                let busy = if self.dma_busy(now) { ST_DMA_BUSY } else { 0 };
                u64::from(self.sdm_status() | busy)
            }
            SDM_IRQ_EN => u64::from(self.irq_en),
            SDM_TIMEOUT => self.timeout,
            SDM_CFG => self.cfg,
            SDM_FIFO => self.fifo.len() as u64,
            SDM_DMA_ADDR if self.dma => self.dma_addr,
            SDM_BLKDONE => u64::from(self.blkdone),
            // While a DMA runs the FIFO port is not used: reads 0, pops nothing.
            SDM_DATA if self.dma_running() => 0,
            SDM_DATA => {
                let word = self.fifo.pop_front().unwrap_or(0);
                self.pump();
                word
            }
            SDM_CAPS => CAPS | if self.dma { CAPS_DMA } else { 0 },
            _ => 0,
        }
    }

    /// Write `data` to the register at `offset` at cycle `now`; `bytes` is
    /// the store's width (1, 2, 4 or 8), which only `SD_BUF` looks at.
    pub(super) fn write(&mut self, offset: u32, data: u64, bytes: u32, now: u64) {
        if offset & SD_BUF_SELECT != 0 {
            let word = &mut self.buf[((offset >> 3) & 63) as usize];
            if bytes >= 8 {
                *word = data;
            } else {
                let shift = 8 * (offset & 7);
                let mask = ((1_u64 << (8 * bytes)) - 1) << shift;
                *word = (*word & !mask) | ((data << shift) & mask);
            }
            return;
        }
        match offset {
            SD_CTRL => self.write_ctrl(data, now),
            SD_DATA => self.exchange(data as u8, now),
            _ if !self.native => {}
            SDM_CMD => self.command(data, now),
            SDM_ARG => self.arg = data as u32,
            SDM_BLK => self.blk = data & BLK_MASK,
            SDM_STATUS => self.status &= !(data as u32 & self.sticky_mask()),
            SDM_IRQ_EN => self.irq_en = data as u32 & (ST_FIFO_THRESH | self.sticky_mask()),
            SDM_TIMEOUT => self.timeout = data,
            SDM_CFG => {
                let mask = if self.dma { CFG_MASK | CFG_DMA_EN } else { CFG_MASK };
                self.cfg = data & mask;
                if self.cfg & CFG_DMA_EN == 0 {
                    // DMA_EN = 0 aborts a DMA in progress (DMA_EN is
                    // sampled per data command: setting it during a PIO
                    // phase changes nothing until the next one).
                    self.dma_abort(now);
                    // The card side goes on into the FIFO.
                    self.pump();
                }
            }
            SDM_FIFO if data & FIFO_RESET != 0 => {
                self.dma_abort(now);
                self.fifo.clear();
                self.dir = None;
                self.pushed = 0;
            }
            // Ignored while DMA_BUSY (spec v1.1 item 8, no error bit): it
            // reads back the live address, so a driver can check it took.
            SDM_DMA_ADDR if self.dma && !self.dma_busy(now) => self.dma_addr = data & DMA_ADDR_MASK,
            // During a DMA phase a store to the FIFO port is dropped and
            // sets FIFO_ERR (item 8).
            SDM_DATA if self.dma_running() => self.status |= ST_FIFO_ERR,
            SDM_DATA => {
                if self.fifo.len() < FIFO_WORDS {
                    self.fifo.push_back(data);
                    self.pushed += 1;
                    self.pump();
                } else {
                    self.status |= ST_FIFO_ERR;
                }
            }
            _ => {}
        }
    }

    /// `INT2_EN` (`0xF00F_0050`).
    pub(super) const fn int2_en(&self) -> u8 {
        self.int2_en
    }

    /// Write `INT2_EN`.
    pub(super) const fn set_int2_en(&mut self, data: u64) {
        self.int2_en = (data & 0b11) as u8;
    }

    /// `INT2_PENDING` (`0xF00F_0058`): `[0]` Ethernet (not modelled, 0),
    /// `[1]` the SD interrupt.
    pub(super) fn int2_pending(&self) -> u8 {
        u8::from(self.sdm_status() & self.irq_en != 0) << 1
    }

    /// The source 2 line: `|(INT2_PENDING & INT2_EN)`.
    pub(super) fn int2_line(&self) -> bool {
        self.int2_pending() & self.int2_en != 0
    }

    // ---- SPI mode ----

    /// `SD_CTRL` write at cycle `now`: power the card up or down as
    /// `PWR_ON` changes.
    fn write_ctrl(&mut self, data: u64, now: u64) {
        let mask = if self.native { 0x7_FFFF } else { 0x3_FFFF };
        let was_sd_mode = self.sd_mode();
        self.ctrl = data & mask;
        let on = self.ctrl & CTRL_PWR_ON != 0;
        if let Some(card) = self.card.as_mut() {
            card.set_power(on);
            if self.ctrl & CTRL_CS_HI != 0 {
                card.cs_released();
            }
        }
        if was_sd_mode && !self.sd_mode() {
            // Leaving SD mode resets the native engine: it abandons a data
            // phase, and a DMA stops once its burst in flight completes
            // (spec v1.1 item 9) -- unless --sd-fault dmastuck wedged it.
            if self.phase.take().is_some_and(|p| p.dma) || self.dma_wedged == Some(FaultKind::DmaHang) {
                self.dma_drain_until = now + DMA_DDR_LATENCY;
            }
            if self.dma_wedged == Some(FaultKind::DmaHang) {
                self.dma_wedged = None;
            }
        }
    }

    /// End the exchange in flight if its time is up.
    fn settle(&mut self, now: u64) {
        if let Some(shift) = self.shift
            && now >= shift.done_at
        {
            self.rx_byte = shift.rx;
            self.shift = None;
        }
    }

    /// `SD_DATA` write: start an exchange (ignored while one runs, or in
    /// SD mode, where the shifter has no pins).
    fn exchange(&mut self, tx: u8, now: u64) {
        self.settle(now);
        if self.shift.is_some() || self.sd_mode() {
            return;
        }
        let cs_low = self.ctrl & CTRL_CS_HI == 0;
        let rx = self.card.as_mut().map_or(0xFF, |card| card.spi_exchange(tx, cs_low));
        let clk_div = self.ctrl & 0xFFFF;
        self.shift = Some(Shift {
            rx,
            done_at: now + 16 * (clk_div + 1) + 1,
        });
    }

    // ---- native SD mode ----

    /// `SDM_STATUS`: the live bits and the sticky ones.
    fn sdm_status(&self) -> u32 {
        let mut status = self.status | ST_DAT0;
        if self.phase.is_some() {
            status |= ST_DATA_BUSY;
        }
        if self.card.is_some() {
            status |= ST_CARD_PRESENT;
        }
        if self.fifo_thresh() {
            status |= ST_FIFO_THRESH;
        }
        if status & (ST_ERRORS | ST_DMA_ERR) != 0 {
            status |= ST_ERR_ANY;
        }
        status
    }

    /// The sticky bits: with DMA, `DMA_ERR` too.
    const fn sticky_mask(&self) -> u32 {
        if self.dma { ST_STICKY | ST_DMA_ERR } else { ST_STICKY }
    }

    /// A data phase is moving its blocks by DMA.
    fn dma_running(&self) -> bool {
        self.phase.is_some_and(|p| p.dma)
    }

    /// `DMA_BUSY` at cycle `now`: a DMA phase (until `DATA_DONE`, for a
    /// write too: spec v1.1 item 7), the drain after an abort or a
    /// `DMA_ERR`, or a wedged engine.
    fn dma_busy(&self, now: u64) -> bool {
        self.dma_running() || now < self.dma_drain_until || self.dma_store.is_some() || self.dma_wedged.is_some()
    }

    /// `DMA_EN` = 0 or a FIFO reset during a DMA: the engine stops (after
    /// the burst in flight: `DMA_BUSY` drops [`DMA_ABORT_DRAIN`] cycles
    /// later) and the data phase goes on as without DMA, through the FIFO,
    /// until the driver ends it (CMD12, FIFO reset).  A wedged engine
    /// (`dmahang`, `dmastuck`) keeps `DMA_BUSY` up regardless.
    fn dma_abort(&mut self, now: u64) {
        if let Some(phase) = self.phase.as_mut()
            && phase.dma
        {
            phase.dma = false;
            phase.done_at = None;
            self.dma_drain_until = now + DMA_ABORT_DRAIN;
            self.dma_stats.aborts += 1;
        }
    }

    /// Cycles from a read block's last word in the FIFO to its last burst
    /// accepted in DDR: a 32-byte burst per four words, pipelined behind
    /// the DDR latency.
    const fn dma_store_cycles(words: usize) -> u64 {
        DMA_DDR_LATENCY + words.div_ceil(DMA_BURST_WORDS) as u64
    }

    /// The SD clock period in CPU cycles: CLK = 100 MHz / (2 (div + 1)).
    const fn sd_clock_cycles(&self) -> u64 {
        2 * ((self.ctrl & 0xFFFF) + 1)
    }

    /// The cycles the card side takes for one block of `phase`.
    const fn block_cycles(&self, phase: &Phase) -> u64 {
        let bits_per_clock = if self.cfg & CFG_4BIT != 0 { 4 } else { 1 };
        let data = (phase.size as u64 * 8).div_ceil(bits_per_clock);
        let extra = if phase.write { DMA_WRITE_EXTRA_CLOCKS } else { 0 };
        (data + DMA_BLOCK_OVERHEAD_CLOCKS + extra) * self.sd_clock_cycles()
    }

    /// The cycle of the DMA engine's next event, if it has one: a fetch
    /// for a write (at once), the card side's next block, or a read's
    /// `DATA_DONE`.  A stalled engine has none.
    pub(super) fn dma_next_event(&self) -> Option<u64> {
        let store = self.dma_store.as_ref().map(|&(at, _)| at);
        let phase = self.phase.filter(|p| p.dma && !p.stalled).map(|phase| {
            if let Some(at) = phase.done_at {
                at
            } else if phase.write && self.dma_fetch_wanted(&phase) {
                0
            } else {
                phase.block_at
            }
        });
        match (store, phase) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Land the read block on its way to DDR if its time (`now`) has come,
    /// or at once with `None`; `SDM_DMA_ADDR` advances past it.  (It was
    /// checked against DDR's end when it was received; the burst in flight
    /// completes even after an abort.)
    fn dma_land(&mut self, now: Option<u64>, bus: &mut DmaBus<'_>) {
        if self.dma_store.as_ref().is_some_and(|&(at, _)| now.is_none_or(|now| at <= now))
            && let Some((_, bytes)) = self.dma_store.take()
        {
            bus.write(self.dma_addr, &bytes);
            self.dma_addr += bytes.len() as u64;
        }
    }

    /// A DMA write has blocks left to fetch and the FIFO has room for one.
    fn dma_fetch_wanted(&self, phase: &Phase) -> bool {
        (phase.count == 0 || phase.fetched < u32::from(phase.count)) && FIFO_WORDS - self.fifo.len() >= phase.words()
    }

    /// Run the DMA engine up to cycle `now`.
    pub(super) fn dma_advance(&mut self, now: u64, bus: &mut DmaBus<'_>) {
        self.dma_land(Some(now), bus);
        while let Some(mut phase) = self.phase.filter(|p| p.dma && !p.stalled) {
            self.dma_land(Some(now), bus);
            if let Some(at) = phase.done_at {
                if at <= now {
                    self.status |= ST_DATA_DONE;
                    self.phase = None;
                }
                return;
            }
            if phase.write && self.dma_fetch_wanted(&phase) {
                // Memory to FIFO, ahead of the card: a block at a time
                // (never past size x count; an open-ended write at most
                // the FIFO's 4 KiB ahead), each fetch checked against the
                // end of DDR.  Its DDR latency is hidden: the card needs
                // the block only after the command and its response.
                let words = phase.words();
                let fetched = if phase.fail || !self.dma_in_ddr(self.dma_addr, (words * 8) as u64) {
                    None
                } else {
                    bus.read(self.dma_addr, words * 8)
                };
                let Some(bytes) = fetched else {
                    self.dma_error(now);
                    return;
                };
                self.fifo
                    .extend(bytes.chunks(8).map(|w| u64::from_le_bytes(w.try_into().unwrap_or([0; 8]))));
                self.dma_addr += (words * 8) as u64;
                phase.fetched += 1;
                self.phase = Some(phase);
                continue;
            }
            if phase.block_at > now {
                return;
            }
            let bus4 = self.cfg & CFG_4BIT != 0;
            let words = phase.words();
            let mut error = 0;
            if phase.write {
                let block = words_to_block(self.fifo.drain(..words.min(self.fifo.len())), phase.size);
                match self.card.as_mut().map_or(WriteBlock::NoToken, |card| card.sd_write_block(&block, bus4)) {
                    WriteBlock::Ok => {
                        self.blkdone = self.blkdone.wrapping_add(1);
                        self.dma_stats.write_blocks += 1;
                    }
                    WriteBlock::CrcError => error = ST_WRITE_CRC_ERR,
                    WriteBlock::NoToken => error = ST_WRITE_NO_TOKEN,
                    WriteBlock::BusyTimeout => error = ST_BUSY_TIMEOUT,
                }
            } else {
                match self.card.as_mut().map_or(ReadBlock::None, |card| card.sd_read_block(phase.size, bus4)) {
                    ReadBlock::Good(block) => {
                        // FIFO to memory: the block's words, so a 4-mod-8
                        // block's last word lands whole ([63:32] = 0).  A
                        // block goes to DDR only after its CRC passed (a
                        // bad one never does), and only if all of it fits
                        // in DDR: else DMA_ERR, and the block stays in the
                        // FIFO, where the card side goes on (item 5).
                        // Its bursts land in DDR dma_store_cycles later.
                        let bytes: Vec<u8> = block_to_words(&block).flat_map(u64::to_le_bytes).collect();
                        self.dma_land(None, bus);
                        if phase.fail || !self.dma_in_ddr(self.dma_addr, bytes.len() as u64) {
                            self.fifo.extend(block_to_words(&block));
                            self.blkdone = self.blkdone.wrapping_add(1);
                            self.dma_error(now);
                            return;
                        }
                        self.dma_store = Some((phase.block_at + Self::dma_store_cycles(words), bytes));
                        self.blkdone = self.blkdone.wrapping_add(1);
                        self.dma_stats.read_blocks += 1;
                    }
                    ReadBlock::Bad => error = ST_DATA_CRC_ERR,
                    ReadBlock::None => error = ST_DATA_TIMEOUT,
                }
            }
            if error != 0 {
                // As without DMA: the data phase ends; the driver recovers
                // the card (CMD12).
                self.status |= error;
                self.phase = None;
                self.dma_stats.card_errors += 1;
                return;
            }
            if phase.count != 0 && self.blkdone >= phase.count {
                if phase.write {
                    // The last block is on the card and its busy is over:
                    // DATA_DONE, and DMA_BUSY drops with it.
                    self.status |= ST_DATA_DONE;
                    self.phase = None;
                } else {
                    // DATA_DONE once the last block's bursts are in DDR.
                    phase.done_at = Some(phase.block_at + Self::dma_store_cycles(words));
                    self.phase = Some(phase);
                }
                continue;
            }
            phase.block_at += self.block_cycles(&phase);
            self.phase = Some(phase);
        }
    }

    /// `[addr, addr + len)` is all in DDR.
    const fn dma_in_ddr(&self, addr: u64, len: u64) -> bool {
        match addr.checked_add(len) {
            Some(end) => end <= self.ddr_size,
            None => false,
        }
    }

    /// `DMA_ERR` during a phase: the DMA ends (nothing of the block moved;
    /// `SDM_DMA_ADDR` is the first byte not transferred), not the card's
    /// data phase, which goes on through the FIFO until the clock stops or
    /// a CMD12 (spec v1.1 item 5).
    fn dma_error(&mut self, now: u64) {
        self.status |= ST_DMA_ERR;
        self.dma_stats.errors += 1;
        if let Some(phase) = self.phase.as_mut() {
            phase.dma = false;
            phase.done_at = None;
        }
        self.dma_drain_until = now + DMA_DDR_LATENCY;
        self.pump();
    }

    /// A data command with `DMA_EN` that the engine refuses up front (spec
    /// v1.1 items 2 and 5): a block size not a multiple of 8, or a counted
    /// transfer that would leave DDR.
    fn dma_refused(&self) -> bool {
        let size = self.blk_size() as u64;
        let count = self.blk >> 16;
        !size.is_multiple_of(8) || (count != 0 && !self.dma_in_ddr(self.dma_addr, size * count))
    }

    /// The block size in bytes, `SDM_BLK[11:0]`; a size field of 0 acts as
    /// 8 (§4.8; the field's `[1:0]` read 0, so sizes 1-3 are 0 too).
    const fn blk_size(&self) -> usize {
        match (self.blk & 0xFFF) as usize {
            0 => 8,
            size => size,
        }
    }

    /// `FIFO_THRESH` (§4.5), by the direction latch: READ, a whole block
    /// of unread data is in the FIFO (until it is drained, also after the
    /// transfer); WRITE, a write phase runs, the FIFO has room for a block
    /// and fewer words were pushed since the FIFO reset than the count's
    /// blocks hold (or the count is open-ended); NONE, 0.
    ///
    /// The block size is the one latched by the last data command, during
    /// the phase and after it; writing `SDM_BLK` changes it only at the
    /// next data command.
    fn fifo_thresh(&self) -> bool {
        if self.dma_running() {
            // The engine, not the driver, moves the data (not in the spec).
            return false;
        }
        let words = block_words(self.thresh_size);
        match (self.dir, self.phase) {
            (Some(false), _) => self.fifo.len() >= words,
            (Some(true), Some(phase)) => {
                FIFO_WORDS - self.fifo.len() >= words && (phase.count == 0 || self.pushed < u64::from(phase.count) * words as u64)
            }
            _ => false,
        }
    }

    /// `SDM_CMD` write at cycle `now`: run a command, and start or stop a
    /// data phase.
    fn command(&mut self, cmd: u64, now: u64) {
        if !self.sd_mode() {
            return;
        }
        let index = (cmd & CMD_INDEX) as u8;
        let rtype = (cmd >> CMD_RESP_SHIFT) & 7;
        let data = cmd & CMD_DATA != 0;
        let dma = data && self.dma && self.cfg & CFG_DMA_EN != 0;
        let mut fault = None;
        if data {
            if self.phase.is_some() {
                // A data command while DATA_BUSY is ignored entirely.
                return;
            }
            if dma {
                fault = self.card.as_mut().and_then(|card| card.dma_fault(index));
                let count = self.blk >> 16;
                if self.dma_refused() || (fault == Some(FaultKind::DmaErr) && count != 0) {
                    // Refused up front: DMA_ERR, no card command sent,
                    // nothing moves.
                    self.status |= ST_DMA_ERR;
                    self.dma_stats.errors += 1;
                    return;
                }
            }
            self.blkdone = 0;
            self.dir = Some(cmd & CMD_WRITE != 0);
            self.thresh_size = self.blk_size();
        }
        let stopped = if cmd & CMD_STOP != 0 { self.phase.take() } else { None };
        if stopped.is_some_and(|p| p.dma) {
            // A CMD12 during a DMA ends the data phase: a read's blocks
            // already received are in DDR, a write's fetching ends, and
            // DMA_BUSY drops after the burst in flight (item 9).
            self.dma_drain_until = now + DMA_DDR_LATENCY;
        }
        let response = match self.card.as_mut() {
            Some(card) => card.sd_command(index, self.arg),
            None => Response::None,
        };
        let errors = self.take_response(index, rtype, response);
        if let Some(stopped) = stopped
            && stopped.count == 0
            && errors == 0
        {
            // A CMD12 ends an open-ended phase cleanly.
            self.status |= ST_DATA_DONE;
        }
        if data {
            if errors == 0 {
                let mut phase = Phase {
                    write: cmd & CMD_WRITE != 0,
                    size: self.blk_size(),
                    count: (self.blk >> 16) as u16,
                    dma,
                    fetched: 0,
                    block_at: 0,
                    done_at: None,
                    stalled: matches!(fault, Some(FaultKind::DmaStall | FaultKind::DmaHang | FaultKind::DmaStuck)),
                    fail: fault == Some(FaultKind::DmaErr),
                };
                if matches!(fault, Some(FaultKind::DmaHang | FaultKind::DmaStuck)) {
                    self.dma_wedged = fault;
                }
                if dma {
                    phase.block_at = now + DMA_FIRST_BLOCK_CLOCKS * self.sd_clock_cycles() + self.block_cycles(&phase);
                    self.dma_stats.commands += 1;
                }
                self.phase = Some(phase);
                self.pump();
            } else if let Some(card) = self.card.as_mut() {
                card.abandon_single();
            }
        }
        self.status |= ST_CMD_DONE | errors;
    }

    /// Capture the card's response as response type `rtype` sees it and
    /// return the error bits: no start bit (timeout), a bad CRC7, a bad
    /// end bit, or (types 1 and 2) an echoed index other than `index`.
    /// `SDM_RESP*` hold the response as received, errors or not; after a
    /// timeout, or for type 0, they are unchanged (§4.8).
    fn take_response(&mut self, index: u8, rtype: u64, response: Response) -> u32 {
        if !matches!(rtype, 1..=4) {
            return 0;
        }
        // The frame on the CMD line, from its start bit; past its end the
        // line idles high.
        let frame: Vec<u8> = match response {
            Response::None => return ST_CMD_TIMEOUT,
            Response::Short(index, arg) => {
                let mut frame = vec![index & 0x3F];
                frame.extend(arg.to_be_bytes());
                frame.push((crc7(&frame) << 1) | 1);
                frame
            }
            Response::Ocr(ocr) => {
                let mut frame = vec![0x3F];
                frame.extend(ocr.to_be_bytes());
                frame.push(0xFF);
                frame
            }
            Response::Long(register) => {
                let mut frame = vec![0x3F];
                frame.extend(register.to_be_bytes());
                frame
            }
        };
        let byte = |i: usize| frame.get(i).copied().unwrap_or(0xFF);
        let mut errors = 0;
        if rtype == 3 {
            let bits: [u8; 16] = std::array::from_fn(|i| byte(1 + i));
            let raw = u128::from_be_bytes(bits);
            self.resp = [raw as u64, (raw >> 64) as u64];
            if bits[15] & 1 == 0 {
                errors |= ST_CMD_END_ERR;
            }
            if bits[15] >> 1 != crc7(&bits[..15]) {
                errors |= ST_CMD_CRC_ERR;
            }
        } else {
            let bits: [u8; 6] = std::array::from_fn(byte);
            let arg = u32::from_be_bytes([bits[1], bits[2], bits[3], bits[4]]);
            self.resp = [(u64::from(bits[0] & 0x3F) << 32) | u64::from(arg), 0];
            if bits[5] & 1 == 0 {
                errors |= ST_CMD_END_ERR;
            }
            if rtype != 4 {
                if bits[5] >> 1 != crc7(&bits[..5]) {
                    errors |= ST_CMD_CRC_ERR;
                }
                if bits[0] & 0x3F != index {
                    errors |= ST_CMD_IDX_ERR;
                }
            }
        }
        errors
    }

    /// Move data between the card and the FIFO as far as it can go now.
    fn pump(&mut self) {
        if !self.sd_mode() {
            return;
        }
        let Some(phase) = self.phase.filter(|p| !p.dma) else {
            return;
        };
        let words = phase.words();
        let bus4 = self.cfg & CFG_4BIT != 0;
        let mut error = 0;
        while phase.count == 0 || self.blkdone < phase.count {
            if phase.write {
                if self.fifo.len() < words {
                    break;
                }
                let block = words_to_block(self.fifo.drain(..words), phase.size);
                let outcome = self.card.as_mut().map_or(WriteBlock::NoToken, |card| card.sd_write_block(&block, bus4));
                match outcome {
                    WriteBlock::Ok => self.blkdone = self.blkdone.wrapping_add(1),
                    WriteBlock::CrcError => error = ST_WRITE_CRC_ERR,
                    WriteBlock::NoToken => error = ST_WRITE_NO_TOKEN,
                    WriteBlock::BusyTimeout => error = ST_BUSY_TIMEOUT,
                }
            } else {
                if FIFO_WORDS - self.fifo.len() < words {
                    break;
                }
                let outcome = self.card.as_mut().map_or(ReadBlock::None, |card| card.sd_read_block(phase.size, bus4));
                match outcome {
                    ReadBlock::Good(block) => {
                        self.fifo.extend(block_to_words(&block));
                        self.blkdone = self.blkdone.wrapping_add(1);
                    }
                    ReadBlock::Bad => error = ST_DATA_CRC_ERR,
                    ReadBlock::None => error = ST_DATA_TIMEOUT,
                }
            }
            if error != 0 {
                break;
            }
        }
        if error != 0 {
            self.status |= error;
            self.phase = None;
        } else if phase.count != 0 && self.blkdone >= phase.count {
            self.status |= ST_DATA_DONE;
            self.phase = None;
        } else {
            self.phase = Some(phase);
        }
    }
}

/// FIFO words a block of `size` bytes takes: ceil(size / 8), each block
/// starting in a new word (§4.3).
const fn block_words(size: usize) -> usize {
    size.div_ceil(8)
}

/// A block as FIFO words, little-endian; when its size is 4 mod 8 the
/// last word carries the final 4 bytes in `[31:0]` and 0 in `[63:32]`.
fn block_to_words(block: &[u8]) -> impl Iterator<Item = u64> + '_ {
    block.chunks(8).map(|chunk| {
        let mut word = [0_u8; 8];
        word[..chunk.len()].copy_from_slice(chunk);
        u64::from_le_bytes(word)
    })
}

/// The `size` bytes of a block from its FIFO words: of a last word with
/// only 4 bytes left, `[31:0]` (`[63:32]` is ignored).
fn words_to_block(words: impl Iterator<Item = u64>, size: usize) -> Vec<u8> {
    let mut block: Vec<u8> = words.flat_map(u64::to_le_bytes).collect();
    block.truncate(size);
    block
}
