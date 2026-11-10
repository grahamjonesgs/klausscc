//! The SD card controller at MMIO `0xF000_0000` (device select `0x000`)
//! and the card behind it (`--sd-image`, `--sd-native`).
//!
//! Without either option the emulator has no SD controller at all: the
//! whole window reads 0 and ignores writes, as it always has, and that is
//! what software sees on a board with no card too (`SD_STATUS.CARD` = 0).
//!
//! **SPI mode** (`sd_spi.sv`, `MMIO_MAP.md` "SD card"): `SD_CTRL` (+0x00)
//! `[15:0]` clock divider, `[16]` CS high, `[17]` slot power; `SD_DATA`
//! (+0x08), whose write starts an 8-bit exchange and whose read returns the
//! last byte received; `SD_STATUS` (+0x10) `[0]` BUSY, `[1]` CARD; and the
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
/// `SD_STATUS[1]`: a card is in the slot.
const STATUS_CARD: u64 = 1 << 1;

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
/// `[31]` `ERR_ANY` (live): any of `[26:16]`.
const ST_ERR_ANY: u32 = 1 << 31;
/// The error bits `[26:16]`.
const ST_ERRORS: u32 = 0x07FF_0000;
/// The sticky, write-1-to-clear bits.
const ST_STICKY: u32 = ST_CMD_DONE | ST_DATA_DONE | ST_ERRORS;
/// The bits `SDM_IRQ_EN` can enable.
const IRQ_EN_MASK: u32 = ST_FIFO_THRESH | ST_STICKY;

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
/// `SDM_FIFO[31]` (write): reset the FIFO.
const FIFO_RESET: u64 = 1 << 31;
/// FIFO depth in 64-bit words (4 KiB, 8 blocks).
const FIFO_WORDS: usize = 512;
/// `SDM_CAPS`: version 1, 4-bit, no high speed, 8 blocks of FIFO.
const CAPS: u64 = (8 << 16) | (1 << 8) | 1;
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
struct Phase {
    /// Host to card.
    write: bool,
    /// Block size in bytes (`SDM_BLK[11:0]` when the command was written).
    size: usize,
    /// Block count, 0 = open-ended.
    count: u16,
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
}

impl SdController {
    /// A controller at reset with `card` in the slot; `native` adds the SD
    /// mode registers.
    pub(super) fn new(card: Option<SdCard>, native: bool) -> Self {
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
        }
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
                let card = if self.card.is_some() { STATUS_CARD } else { 0 };
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
            SDM_STATUS => u64::from(self.sdm_status()),
            SDM_IRQ_EN => u64::from(self.irq_en),
            SDM_TIMEOUT => self.timeout,
            SDM_CFG => self.cfg,
            SDM_FIFO => self.fifo.len() as u64,
            SDM_BLKDONE => u64::from(self.blkdone),
            SDM_DATA => {
                let word = self.fifo.pop_front().unwrap_or(0);
                self.pump();
                word
            }
            SDM_CAPS => CAPS,
            _ => 0,
        }
    }

    /// Write `data` to the register at `offset`; `bytes` is the store's
    /// width (1, 2, 4 or 8), which only `SD_BUF` looks at.
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
            SD_CTRL => self.write_ctrl(data),
            SD_DATA => self.exchange(data as u8, now),
            _ if !self.native => {}
            SDM_CMD => self.command(data),
            SDM_ARG => self.arg = data as u32,
            SDM_BLK => self.blk = data & BLK_MASK,
            SDM_STATUS => self.status &= !(data as u32 & ST_STICKY),
            SDM_IRQ_EN => self.irq_en = data as u32 & IRQ_EN_MASK,
            SDM_TIMEOUT => self.timeout = data,
            SDM_CFG => self.cfg = data & CFG_MASK,
            SDM_FIFO if data & FIFO_RESET != 0 => {
                self.fifo.clear();
                self.dir = None;
                self.pushed = 0;
            }
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

    /// `SD_CTRL` write: power the card up or down as `PWR_ON` changes.
    fn write_ctrl(&mut self, data: u64) {
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
            // Leaving SD mode abandons a native data phase.
            self.phase = None;
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
        if status & ST_ERRORS != 0 {
            status |= ST_ERR_ANY;
        }
        status
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
        let words = block_words(self.thresh_size);
        match (self.dir, self.phase) {
            (Some(false), _) => self.fifo.len() >= words,
            (Some(true), Some(phase)) => {
                FIFO_WORDS - self.fifo.len() >= words && (phase.count == 0 || self.pushed < u64::from(phase.count) * words as u64)
            }
            _ => false,
        }
    }

    /// `SDM_CMD` write: run a command, and start or stop a data phase.
    fn command(&mut self, cmd: u64) {
        if !self.sd_mode() {
            return;
        }
        let index = (cmd & CMD_INDEX) as u8;
        let rtype = (cmd >> CMD_RESP_SHIFT) & 7;
        let data = cmd & CMD_DATA != 0;
        if data {
            if self.phase.is_some() {
                // A data command while DATA_BUSY is ignored entirely.
                return;
            }
            self.blkdone = 0;
            self.dir = Some(cmd & CMD_WRITE != 0);
            self.thresh_size = self.blk_size();
        }
        let stopped = if cmd & CMD_STOP != 0 { self.phase.take() } else { None };
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
                self.phase = Some(Phase {
                    write: cmd & CMD_WRITE != 0,
                    size: self.blk_size(),
                    count: (self.blk >> 16) as u16,
                });
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
        let Some(phase) = self.phase else {
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
