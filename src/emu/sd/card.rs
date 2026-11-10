//! The emulated SD card behind `--sd-image`: an SDHC card (block
//! addressing, OCR CCS = 1, CSD version 2.0) whose blocks are a raw disk
//! image file.
//!
//! The card speaks both bus protocols, as a real one does:
//!
//! - **SPI mode**, byte by byte through the SPI controller's `SD_DATA`
//!   exchanges ([`SdCard::spi_exchange`]). A card enters it on a CMD0 sent
//!   with CS low, and leaves it only by a power cycle.
//! - **SD mode**, command by command through the native controller's
//!   command and data engines ([`SdCard::sd_command`],
//!   [`SdCard::sd_read_block`], [`SdCard::sd_write_block`]), with the
//!   SD-mode states idle / ready / ident / stby / tran / data / rcv / prg.
//!
//! Nothing takes time: a block is read or programmed at once, and a busy
//! period is a couple of zero bytes in SPI mode and nothing in SD mode.
//! CRCs sent by the host are not checked (SPI mode with CRC off; the native
//! engine's frames are built by the emulator and always good), but the
//! card's own CRCs (CRC7 on responses and registers, CRC16 on SPI data
//! blocks) are real.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;

/// Bytes per block.
pub(super) const BLOCK: usize = 512;
/// Bytes per CSD capacity unit: `(C_SIZE + 1) * 512 KiB`.
const CSD_UNIT: u64 = 512 * 1024;
/// The largest card the 32-bit block address reaches (2 TiB).
const MAX_BYTES: u64 = (1 << 32) * BLOCK as u64;
/// The RCA the card publishes with CMD3.
pub(super) const RCA: u16 = 0x1234;
/// ACMD41 polls before the card reports power-up done (the first one
/// reports busy, as cards do for a while).
const ACMD41_POLLS: u8 = 2;

// ---- the OCR (ACMD41 / CMD58) ----
/// OCR `[31]`: power-up done.
const OCR_READY: u32 = 1 << 31;
/// OCR `[30]` CCS: a high-capacity card (block addressing).
const OCR_CCS: u32 = 1 << 30;
/// OCR `[23:15]`: 2.7-3.6 V.
const OCR_VOLTAGE: u32 = 0x00FF_8000;
/// ACMD41 argument `[30]` HCS: the host supports high-capacity cards.
const ACMD41_HCS: u32 = 1 << 30;

// ---- card status (SD-mode R1, SD Physical Layer §4.10.1) ----
/// `[31]` `OUT_OF_RANGE`.
pub(super) const ST_OUT_OF_RANGE: u32 = 1 << 31;
/// `[22]` `ILLEGAL_COMMAND` (reported in the response after the illegal one).
pub(super) const ST_ILLEGAL_COMMAND: u32 = 1 << 22;
/// `[8]` `READY_FOR_DATA`.
pub(super) const ST_READY_FOR_DATA: u32 = 1 << 8;
/// `[5]` `APP_CMD`.
pub(super) const ST_APP_CMD: u32 = 1 << 5;

// ---- SPI-mode R1 bits and tokens ----
/// R1 `[0]`: in idle state.
const R1_IDLE: u8 = 0x01;
/// R1 `[2]`: illegal command.
const R1_ILLEGAL: u8 = 0x04;
/// R1 `[6]`: parameter error (an address past the end of the card).
const R1_PARAM: u8 = 0x40;
/// Start-block token for a read, CMD24 and the registers.
const TOKEN_START: u8 = 0xFE;
/// Start-block token for each CMD25 block.
const TOKEN_MULTI: u8 = 0xFC;
/// CMD25 stop-transmission token.
const TOKEN_STOP: u8 = 0xFD;
/// Data error token: out of range.
const TOKEN_OUT_OF_RANGE: u8 = 0x08;
/// Data response: accepted (`xxx0_0101`).
const DATA_ACCEPTED: u8 = 0xE5;
/// Data response: write error (`xxx0_1101`).
const DATA_WRITE_ERROR: u8 = 0xED;
/// SPI busy bytes after an R1b response, a write or a stop token.
const SPI_BUSY: [u8; 2] = [0x00, 0x00];

/// `CSD` `CCC`: classes 0 (basic), 2 (block read), 4 (block write), 8
/// (application) and 10 (switch). No erase (5) or lock (7) commands.
const CSD_CCC: u128 = 0x515;

/// SD-mode card states, numbered as `CURRENT_STATE` in the card status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum State {
    /// After power-up or CMD0.
    Idle = 0,
    /// ACMD41 done.
    Ready = 1,
    /// CMD2 sent the CID.
    Ident = 2,
    /// Has an RCA, not selected.
    Stby = 3,
    /// Selected, no transfer.
    Tran = 4,
    /// Sending data.
    Data = 5,
    /// Receiving data.
    Rcv = 6,
    /// Inactive (CMD15): only a power cycle leaves it.
    Ina = 15,
}

/// A response the card puts on the CMD line in SD mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Response {
    /// No response (no such command in this state, or CMD0).
    None,
    /// A 48-bit response with this index and argument field (R1, R1b, R6,
    /// R7), and a real CRC7.
    Short(u8, u32),
    /// R3: the OCR, with the index field all ones and no CRC (`0x7F`).
    Ocr(u32),
    /// R2: the CID or CSD with its CRC7 and end bit.
    Long(u128),
}

/// A block the native data engine reads from the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ReadBlock {
    /// The card sends nothing (not in the data state, or past its end):
    /// the host sees no start bit.
    None,
    /// The card sent a block the host could not receive: another block
    /// length or bus width (a CRC or end-bit error on the host's side).
    Bad,
    /// A good block.
    Good(Vec<u8>),
}

/// What the card does with a block the native data engine writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WriteBlock {
    /// No CRC status token: the card is not receiving.
    NoToken,
    /// CRC status token 101: the card got another block length or bus width.
    CrcError,
    /// CRC status token 010; the block is programmed and the busy is over.
    Ok,
}

/// Where the card's blocks are kept.
enum Storage {
    /// The `--sd-image` file.
    File(File),
    /// A buffer (tests).
    #[cfg_attr(not(test), expect(dead_code, reason = "used by the unit tests"))]
    Memory(Vec<u8>),
}

/// Data an SD-mode command left to send or receive.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pending {
    /// Nothing.
    None,
    /// One block of register data (SD status, SCR, switch status).
    Bytes(Vec<u8>),
    /// Memory blocks from this block address, `left` of them (`None`:
    /// until CMD12).
    Read { lba: u64, left: Option<u32> },
    /// Memory blocks to this block address.
    Write { lba: u64, left: Option<u32> },
}

/// SPI mode: what the card expects next from MOSI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum SpiRx {
    /// A command frame (bytes before one starts are ignored).
    #[default]
    Command,
    /// After CMD24/CMD25: a start token (or, for CMD25, a stop token).
    Token { multi: bool, lba: u64 },
    /// A data block and its CRC16.
    Data { multi: bool, lba: u64, buf: Vec<u8> },
}

/// SPI mode: a CMD18 multi-block read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Stream {
    /// No read running.
    #[default]
    Off,
    /// The next block to send.
    Next(u64),
    /// Past the end of the card: the error token went out, nothing more
    /// until CMD12.
    End,
}

/// SPI-mode byte-level state.
#[derive(Debug, Default)]
struct Spi {
    /// Bytes queued for MISO (`0xFF` when empty).
    out: VecDeque<u8>,
    /// The command frame being received.
    frame: [u8; 6],
    /// Its length so far.
    frame_len: usize,
    /// What MOSI carries next.
    rx: SpiRx,
    /// A running CMD18.
    stream: Stream,
}

/// The emulated SDHC card.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent card state bits: power, bus mode, APP_CMD, bus width"
)]
pub struct SdCard {
    /// The blocks.
    storage: Storage,
    /// Whole blocks in the image.
    blocks: u64,
    /// The first I/O error on the image, for the run's report.
    io_error: Option<String>,
    /// The slot is powered (`SD_CTRL.PWR_ON`).
    powered: bool,
    /// The card is in SPI mode (CMD0 with CS low since power-up).
    spi: bool,
    /// The SD-mode state (in SPI mode: Idle until ACMD41 is done, then Tran).
    state: State,
    /// ACMD41 polls with HCS since CMD0.
    acmd41_polls: u8,
    /// The previous command was CMD55.
    app_cmd: bool,
    /// The published RCA (0 before CMD3).
    rca: u16,
    /// ACMD6 chose the 4-bit bus.
    bus4: bool,
    /// Card status error bits for the next SD-mode response.
    status_err: u32,
    /// The block count CMD23 set for the next CMD18/CMD25.
    block_count: Option<u32>,
    /// Data an SD-mode command left to transfer.
    pending: Pending,
    /// SPI-mode state.
    spi_io: Spi,
    /// The CID, with its CRC7 and end bit.
    cid: u128,
    /// The CSD, with its CRC7 and end bit.
    csd: u128,
}

impl std::fmt::Debug for SdCard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SdCard")
            .field("blocks", &self.blocks)
            .field("powered", &self.powered)
            .field("spi", &self.spi)
            .field("state", &self.state)
            .field("rca", &self.rca)
            .field("bus4", &self.bus4)
            .finish_non_exhaustive()
    }
}

/// The CRC7 of the SD command and response frames and the CID/CSD
/// (x^7 + x^3 + 1), MSB first.
pub(super) fn crc7(bytes: &[u8]) -> u8 {
    let mut crc = 0_u8;
    for &byte in bytes {
        for bit in (0..8).rev() {
            let feedback = ((byte >> bit) & 1) ^ ((crc >> 6) & 1);
            crc = (crc << 1) & 0x7F;
            if feedback != 0 {
                crc ^= 0x09;
            }
        }
    }
    crc
}

/// The CRC16 of an SPI data block (CRC-CCITT, x^16 + x^12 + x^5 + 1, from 0).
pub(super) fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0_u16;
    for &byte in bytes {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

/// Put `value` in bits `[hi:lo]` of a 128-bit register.
const fn field(hi: u32, lo: u32, value: u128) -> u128 {
    (value & ((1 << (hi - lo + 1)) - 1)) << lo
}

/// A CID or CSD with its CRC7 in `[7:1]` and the end bit `[0]`.
fn with_crc(register: u128) -> u128 {
    let bytes = register.to_be_bytes();
    (register & !0xFF) | (u128::from(crc7(&bytes[..15])) << 1) | 1
}

impl SdCard {
    /// Open the raw disk image `path` (read and written in place).
    ///
    /// The image must be a non-empty whole number of 512-byte blocks, at
    /// least 512 KiB (the CSD's capacity unit) and at most 2 TiB.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let len = file.metadata()?.len();
        Self::check_len(len)?;
        Ok(Self::with_storage(Storage::File(file), len))
    }

    /// A card holding `bytes` in memory (tests).
    #[cfg(test)]
    pub(super) fn from_bytes(bytes: Vec<u8>) -> io::Result<Self> {
        let len = bytes.len() as u64;
        Self::check_len(len)?;
        Ok(Self::with_storage(Storage::Memory(bytes), len))
    }

    /// Refuse an image the card cannot present.
    fn check_len(len: u64) -> io::Result<()> {
        let bad = |why: String| Err(io::Error::new(io::ErrorKind::InvalidInput, why));
        if !len.is_multiple_of(BLOCK as u64) {
            return bad(format!("{len} bytes is not a whole number of 512-byte blocks"));
        }
        if len < CSD_UNIT {
            return bad(format!("{len} bytes is smaller than an SDHC card's 512 KiB capacity unit"));
        }
        if len > MAX_BYTES {
            return bad(format!("{len} bytes is larger than 2 TiB"));
        }
        Ok(())
    }

    /// A powered-off card with `len` bytes of storage.
    fn with_storage(storage: Storage, len: u64) -> Self {
        let blocks = len / BLOCK as u64;
        Self {
            storage,
            blocks,
            io_error: None,
            powered: false,
            spi: false,
            state: State::Idle,
            acmd41_polls: 0,
            app_cmd: false,
            rca: 0,
            bus4: false,
            status_err: 0,
            block_count: None,
            pending: Pending::None,
            spi_io: Spi::default(),
            cid: Self::make_cid(),
            csd: Self::make_csd(len),
        }
    }

    /// The CID: manufacturer 0x4B, OEM "KC", product "KLEMU", revision 1.0,
    /// serial 0x12345678, made 2026-11.
    fn make_cid() -> u128 {
        let pnm = u128::from_be_bytes(*b"\0\0\0\0\0\0\0\0\0\0\0KLEMU");
        with_crc(
            field(127, 120, 0x4B)
                | field(119, 104, u128::from(u16::from_be_bytes(*b"KC")))
                | field(103, 64, pnm)
                | field(63, 56, 0x10)
                | field(55, 24, 0x1234_5678)
                | field(19, 8, (26 << 4) | 0xB), // MDT: 2026-11
        )
    }

    /// The CSD (version 2.0): `C_SIZE` = the image's whole 512 KiB units - 1.
    fn make_csd(len: u64) -> u128 {
        let c_size = u128::from(len / CSD_UNIT - 1);
        with_crc(
            field(127, 126, 1)
                | field(119, 112, 0x0E) // TAAC: 1 ms
                | field(103, 96, 0x32) // TRAN_SPEED: 25 MHz
                | field(95, 84, CSD_CCC)
                | field(83, 80, 9) // READ_BL_LEN: 512
                | field(69, 48, c_size)
                | field(46, 46, 1) // ERASE_BLK_EN
                | field(45, 39, 0x7F) // SECTOR_SIZE
                | field(28, 26, 0b010) // R2W_FACTOR
                | field(25, 22, 9), // WRITE_BL_LEN: 512
        )
    }

    /// The SCR: SD 3.0, SDHC security, 1- and 4-bit buses, CMD23.
    fn scr() -> Vec<u8> {
        let scr: u64 = (2 << 56) // SD_SPEC 2
            | (3 << 52) // SD_SECURITY: SDHC
            | (0b0101 << 48) // SD_BUS_WIDTHS: 1 and 4 bits
            | (1 << 47) // SD_SPEC3
            | (0b0010 << 32); // CMD_SUPPORT: CMD23
        scr.to_be_bytes().to_vec()
    }

    /// The 64-byte SD status (ACMD13): the bus width, speed class 10, a
    /// 4 MiB allocation unit.
    fn sd_status(&self) -> Vec<u8> {
        let mut status = vec![0_u8; 64];
        // Byte n holds bits [511 - 8n : 504 - 8n].
        status[0] = if self.bus4 { 0x80 } else { 0x00 }; // DAT_BUS_WIDTH [511:510]
        status[8] = 0x04; // SPEED_CLASS [447:440]: class 10
        status[10] = 0x90; // AU_SIZE [431:428]: 4 MiB
        status
    }

    /// The 64-byte CMD6 switch status for `arg`: every group supports only
    /// function 0 (so high speed is not offered), and a request for any
    /// other function reports 0xF (cannot switch).
    fn switch_status(arg: u32) -> Vec<u8> {
        let mut status = vec![0_u8; 64];
        // [511:496] maximum current: 100 mA.
        status[1] = 100;
        // [495:400] support of groups 6..1: 0x8001 each.
        for group in 0..6 {
            status[2 + 2 * group] = 0x80;
            status[3 + 2 * group] = 0x01;
        }
        // [399:376] function selection of groups 6..1, a nibble each.
        let mut selection = 0_u32;
        for group in 0..6 {
            let want = (arg >> (4 * group)) & 0xF;
            let got = if want == 0 || want == 0xF { 0 } else { 0xF };
            selection |= got << (4 * group);
        }
        status[14] = (selection >> 16) as u8;
        status[15] = (selection >> 8) as u8;
        status[16] = selection as u8;
        // [375:368] data structure version 1 (busy status follows, all 0).
        status[17] = 1;
        status
    }

    /// How the image's size maps onto the card, if not exactly: the CSD
    /// counts whole 512 KiB units, so a tail beyond the last one is outside
    /// the capacity the host reads (though still addressable).
    #[must_use]
    pub fn capacity_note(&self) -> Option<String> {
        let len = self.blocks * BLOCK as u64;
        let unit = len - len % CSD_UNIT;
        (unit != len).then(|| format!("the SD image is {len} bytes, not a multiple of 512 KiB: the card's CSD reports {unit} bytes"))
    }

    /// Take the first I/O error on the image, if any.
    pub fn take_io_error(&mut self) -> Option<String> {
        self.io_error.take()
    }

    // ---- storage ----

    /// Read block `lba` (zeros, with the error kept, if the image fails).
    fn read_block(&mut self, lba: u64) -> Vec<u8> {
        let mut buf = vec![0_u8; BLOCK];
        let at = lba * BLOCK as u64;
        let result = match &mut self.storage {
            Storage::File(file) => file.seek(SeekFrom::Start(at)).and_then(|_| file.read_exact(&mut buf)),
            Storage::Memory(mem) => {
                buf.copy_from_slice(&mem[at as usize..at as usize + BLOCK]);
                Ok(())
            }
        };
        if let Err(e) = result {
            self.note_io_error("read", lba, &e);
        }
        buf
    }

    /// Write block `lba` (the first `BLOCK` bytes of `data`).
    fn write_block(&mut self, lba: u64, data: &[u8]) {
        let at = lba * BLOCK as u64;
        let data = &data[..BLOCK];
        let result = match &mut self.storage {
            Storage::File(file) => file.seek(SeekFrom::Start(at)).and_then(|_| file.write_all(data)),
            Storage::Memory(mem) => {
                mem[at as usize..at as usize + BLOCK].copy_from_slice(data);
                Ok(())
            }
        };
        if let Err(e) = result {
            self.note_io_error("write", lba, &e);
        }
    }

    /// Keep the first I/O error for the report.
    fn note_io_error(&mut self, what: &str, lba: u64, e: &io::Error) {
        if self.io_error.is_none() {
            self.io_error = Some(format!("SD image {what} of block {lba} failed: {e}"));
        }
    }

    /// The image's bytes (tests).
    #[cfg(test)]
    pub(super) fn memory(&self) -> &[u8] {
        match &self.storage {
            Storage::Memory(mem) => mem,
            Storage::File(_) => &[],
        }
    }

    // ---- power and reset ----

    /// Switch the slot's power. Turning it off loses every state; the card
    /// comes up in SD mode.
    pub(super) fn set_power(&mut self, on: bool) {
        if on == self.powered {
            return;
        }
        self.powered = on;
        self.spi = false;
        self.reset();
        self.status_err = 0;
        self.spi_io = Spi::default();
    }

    /// CMD0: back to idle, keeping the bus mode.
    fn reset(&mut self) {
        self.state = State::Idle;
        self.acmd41_polls = 0;
        self.app_cmd = false;
        self.rca = 0;
        self.bus4 = false;
        self.block_count = None;
        self.pending = Pending::None;
    }

    /// The OCR as ACMD41 and CMD58 report it.
    fn ocr(&self) -> u32 {
        if matches!(self.state, State::Idle) {
            OCR_VOLTAGE
        } else {
            OCR_VOLTAGE | OCR_READY | OCR_CCS
        }
    }

    /// One ACMD41 (or SPI CMD1) poll: the card is ready after
    /// [`ACMD41_POLLS`] of them with HCS set. An SDHC card asked without
    /// HCS never finishes, as the spec says.
    fn acmd41_poll(&mut self, arg: u32) {
        if self.state == State::Idle && arg & ACMD41_HCS != 0 {
            self.acmd41_polls = self.acmd41_polls.saturating_add(1);
            if self.acmd41_polls >= ACMD41_POLLS {
                self.state = if self.spi { State::Tran } else { State::Ready };
            }
        }
    }

    // ---- SD mode ----

    /// The card status for an R1 response: the pending error bits (cleared
    /// by this report), `CURRENT_STATE` = the state the command found,
    /// `READY_FOR_DATA` and `APP_CMD`.
    fn status(&mut self, state: State, app: bool) -> u32 {
        let status = std::mem::take(&mut self.status_err) | ((state as u32 & 0xF) << 9) | ST_READY_FOR_DATA;
        if app { status | ST_APP_CMD } else { status }
    }

    /// An R1 (or R1b) response to command `index`.
    fn r1(&mut self, index: u8, state: State, app: bool) -> Response {
        Response::Short(index, self.status(state, app))
    }

    /// An illegal command: no response, `ILLEGAL_COMMAND` in the next one.
    fn illegal(&mut self) -> Response {
        self.status_err |= ST_ILLEGAL_COMMAND;
        Response::None
    }

    /// Whether `arg[31:16]` addresses this card.
    const fn addressed(&self, arg: u32) -> bool {
        (arg >> 16) as u16 == self.rca
    }

    /// Execute an SD-mode command from the native controller. A card that
    /// is unpowered or in SPI mode does not answer.
    pub(super) fn sd_command(&mut self, index: u8, arg: u32) -> Response {
        if !self.powered || self.spi || self.state == State::Ina {
            return Response::None;
        }
        let app = std::mem::take(&mut self.app_cmd);
        let state = self.state;
        if app && let Some(response) = self.sd_app_command(index, arg, state) {
            return response;
        }
        match (index, state) {
            (0, _) => {
                self.reset();
                Response::None
            }
            (2, State::Ready) => {
                self.state = State::Ident;
                Response::Long(self.cid)
            }
            (3, State::Ident | State::Stby) => {
                self.state = State::Stby;
                self.rca = RCA;
                let status = self.status(state, false);
                // R6: RCA, then status bits 23, 22, 19 and 12:0.
                let bits = ((status >> 8) & 0xC000) | ((status >> 6) & 0x2000) | (status & 0x1FFF);
                Response::Short(3, (u32::from(self.rca) << 16) | bits)
            }
            (7, State::Stby | State::Tran | State::Data | State::Rcv) => {
                if self.addressed(arg) && self.rca != 0 {
                    if state == State::Stby {
                        self.state = State::Tran;
                    }
                    self.r1(7, state, false)
                } else {
                    // Deselected: no response.
                    self.state = State::Stby;
                    self.pending = Pending::None;
                    Response::None
                }
            }
            (8, State::Idle) if (arg >> 8) & 0xF == 1 => Response::Short(8, arg & 0xFFF),
            (9, State::Stby) if self.addressed(arg) => Response::Long(self.csd),
            (10, State::Stby) if self.addressed(arg) => Response::Long(self.cid),
            (12, State::Data | State::Rcv) => {
                self.state = State::Tran;
                self.pending = Pending::None;
                self.r1(12, state, false)
            }
            (13, State::Stby | State::Tran | State::Data | State::Rcv) if self.addressed(arg) => self.r1(13, state, false),
            (15, _) => {
                self.state = State::Ina;
                Response::None
            }
            (16, State::Tran) => self.r1(16, state, false),
            (6, State::Tran) => {
                self.pending = Pending::Bytes(Self::switch_status(arg));
                self.state = State::Data;
                self.r1(6, state, false)
            }
            (17 | 18 | 24 | 25, State::Tran) => {
                let lba = u64::from(arg);
                if lba >= self.blocks {
                    self.status_err |= ST_OUT_OF_RANGE;
                    return self.r1(index, state, false);
                }
                let left = if matches!(index, 17 | 24) { Some(1) } else { self.block_count.take() };
                if matches!(index, 17 | 18) {
                    self.pending = Pending::Read { lba, left };
                    self.state = State::Data;
                } else {
                    self.pending = Pending::Write { lba, left };
                    self.state = State::Rcv;
                }
                self.r1(index, state, false)
            }
            (23, State::Tran) => {
                self.block_count = Some(arg).filter(|&n| n != 0);
                self.r1(23, state, false)
            }
            (55, _) if !matches!(state, State::Stby | State::Tran | State::Data | State::Rcv) || self.addressed(arg) => {
                self.app_cmd = true;
                self.r1(55, state, true)
            }
            _ => self.illegal(),
        }
    }

    /// An application command after CMD55, or `None` to run `index` as an
    /// ordinary command.
    fn sd_app_command(&mut self, index: u8, arg: u32, state: State) -> Option<Response> {
        Some(match (index, state) {
            (6, State::Tran) => {
                self.bus4 = arg & 3 == 2;
                self.r1(6, state, true)
            }
            (13, State::Tran) => {
                self.pending = Pending::Bytes(self.sd_status());
                self.state = State::Data;
                self.r1(13, state, true)
            }
            (23 | 42, State::Tran) => self.r1(index, state, true),
            (41, State::Idle) => {
                if arg & OCR_VOLTAGE != 0 {
                    self.acmd41_poll(arg);
                }
                Response::Ocr(self.ocr())
            }
            (41, _) => self.illegal(),
            (51, State::Tran) => {
                self.pending = Pending::Bytes(Self::scr());
                self.state = State::Data;
                self.r1(51, state, true)
            }
            _ => return None,
        })
    }

    /// The native read engine asks for the next block: `len` bytes on a
    /// 1-bit or 4-bit (`bus4`) bus.
    pub(super) fn sd_read_block(&mut self, len: usize, bus4: bool) -> ReadBlock {
        if self.state != State::Data || !self.powered {
            return ReadBlock::None;
        }
        let data = match std::mem::replace(&mut self.pending, Pending::None) {
            Pending::Bytes(bytes) => {
                self.state = State::Tran;
                bytes
            }
            Pending::Read { lba, left } => {
                if lba >= self.blocks {
                    // Past the end: the card stops sending until CMD12.
                    self.status_err |= ST_OUT_OF_RANGE;
                    self.pending = Pending::Read { lba, left };
                    return ReadBlock::None;
                }
                let block = self.read_block(lba);
                match left.map(|n| n - 1) {
                    Some(0) => self.state = State::Tran,
                    left => self.pending = Pending::Read { lba: lba + 1, left },
                }
                block
            }
            other => {
                self.pending = other;
                return ReadBlock::None;
            }
        };
        if data.len() == len && bus4 == self.bus4 {
            ReadBlock::Good(data)
        } else {
            ReadBlock::Bad
        }
    }

    /// The native write engine sends a block on a 1-bit or 4-bit bus.
    pub(super) fn sd_write_block(&mut self, data: &[u8], bus4: bool) -> WriteBlock {
        let Pending::Write { lba, left } = self.pending else {
            return WriteBlock::NoToken;
        };
        if self.state != State::Rcv || !self.powered {
            return WriteBlock::NoToken;
        }
        if data.len() != BLOCK || bus4 != self.bus4 {
            return WriteBlock::CrcError;
        }
        if lba >= self.blocks {
            self.status_err |= ST_OUT_OF_RANGE;
            return WriteBlock::NoToken;
        }
        self.write_block(lba, data);
        match left.map(|n| n - 1) {
            Some(0) => {
                self.state = State::Tran;
                self.pending = Pending::None;
            }
            left => self.pending = Pending::Write { lba: lba + 1, left },
        }
        WriteBlock::Ok
    }

    /// The host gave up on a single-block transfer whose command response
    /// it rejected: the card sends that block anyway and returns to tran.
    pub(super) fn abandon_single(&mut self) {
        let single = match &self.pending {
            Pending::Bytes(_) => true,
            Pending::Read { left, .. } => *left == Some(1),
            _ => false,
        };
        if single && self.state == State::Data {
            self.state = State::Tran;
            self.pending = Pending::None;
        }
    }

    // ---- SPI mode ----

    /// One SPI byte exchange: the card shifts out a byte while it shifts
    /// `mosi` in. With CS high, or the slot unpowered, MISO floats high and
    /// the card ignores the byte.
    pub(super) fn spi_exchange(&mut self, mosi: u8, cs_low: bool) -> u8 {
        if !self.powered || !cs_low {
            return 0xFF;
        }
        if self.spi_io.out.is_empty()
            && self.spi
            && let Stream::Next(lba) = self.spi_io.stream
        {
            self.queue_stream_block(lba);
        }
        let miso = self.spi_io.out.pop_front().unwrap_or(0xFF);
        self.spi_take(mosi);
        miso
    }

    /// Queue the next CMD18 block (or, past the end, the error token).
    fn queue_stream_block(&mut self, lba: u64) {
        if lba >= self.blocks {
            self.spi_io.out.extend([0xFF, TOKEN_OUT_OF_RANGE]);
            self.spi_io.stream = Stream::End;
            return;
        }
        let block = self.read_block(lba);
        self.spi_io.out.push_back(0xFF);
        self.spi_io.out.extend(data_block(&block));
        self.spi_io.stream = Stream::Next(lba + 1);
    }

    /// Take one MOSI byte.
    fn spi_take(&mut self, mosi: u8) {
        match &mut self.spi_io.rx {
            SpiRx::Data { buf, .. } => {
                buf.push(mosi);
                if buf.len() == BLOCK + 2 {
                    self.spi_finish_write();
                }
                return;
            }
            SpiRx::Token { multi, lba } => {
                let (multi, lba) = (*multi, *lba);
                match mosi {
                    TOKEN_START if !multi => {
                        self.spi_io.rx = SpiRx::Data { multi, lba, buf: Vec::new() };
                        return;
                    }
                    TOKEN_MULTI if multi => {
                        self.spi_io.rx = SpiRx::Data { multi, lba, buf: Vec::new() };
                        return;
                    }
                    TOKEN_STOP if multi => {
                        self.spi_io.rx = SpiRx::Command;
                        self.state = State::Tran;
                        self.spi_io.out.clear();
                        self.spi_io.out.push_back(0xFF);
                        self.spi_io.out.extend(SPI_BUSY);
                        return;
                    }
                    // A command abandons the write.
                    b if b & 0xC0 == 0x40 => {
                        self.spi_io.rx = SpiRx::Command;
                        self.state = State::Tran;
                    }
                    _ => return,
                }
            }
            SpiRx::Command => {}
        }
        let io = &mut self.spi_io;
        if io.frame_len == 0 && mosi & 0xC0 != 0x40 {
            return;
        }
        io.frame[io.frame_len] = mosi;
        io.frame_len += 1;
        if io.frame_len == io.frame.len() {
            io.frame_len = 0;
            let index = io.frame[0] & 0x3F;
            let arg = u32::from_be_bytes([io.frame[1], io.frame[2], io.frame[3], io.frame[4]]);
            self.spi_command(index, arg);
        }
    }

    /// A CMD24/CMD25 data block is in: program it and queue the data
    /// response and the busy.
    fn spi_finish_write(&mut self) {
        let SpiRx::Data { multi, lba, buf } = std::mem::take(&mut self.spi_io.rx) else {
            return;
        };
        let ok = lba < self.blocks;
        if ok {
            self.write_block(lba, &buf);
        }
        self.spi_io.out.clear();
        self.spi_io.out.push_back(if ok { DATA_ACCEPTED } else { DATA_WRITE_ERROR });
        self.spi_io.out.extend(SPI_BUSY);
        if multi {
            self.spi_io.rx = SpiRx::Token { multi, lba: lba + 1 };
        } else {
            self.state = State::Tran;
        }
    }

    /// Execute an SPI-mode command frame and queue its response, after one
    /// byte of `N_CR`.
    fn spi_command(&mut self, index: u8, arg: u32) {
        if !self.spi {
            // SD mode: only CMD0 with CS low (this exchange) is seen; it
            // puts the card in SPI mode.
            if index == 0 {
                self.spi = true;
                self.reset();
                self.spi_io.out.extend([0xFF, R1_IDLE]);
            }
            return;
        }
        let io = &mut self.spi_io;
        io.out.clear();
        if index == 12 && io.stream != Stream::Off {
            // Stop a CMD18: a stuff byte, R1, then busy.
            io.stream = Stream::Off;
            self.state = State::Tran;
            io.out.extend([0xFF, 0x00]);
            io.out.extend(SPI_BUSY);
            return;
        }
        io.stream = Stream::Off;
        let idle = self.state == State::Idle;
        let r1 = u8::from(idle);
        let app = std::mem::take(&mut self.app_cmd);
        let mut out = vec![0xFF];
        let allowed_idle = matches!(index, 0 | 1 | 8 | 55 | 58 | 59) || (app && index == 41);
        if idle && !allowed_idle {
            out.push(R1_IDLE | R1_ILLEGAL);
            self.spi_io.out.extend(out);
            return;
        }
        match (app, index) {
            (_, 0) => {
                self.reset();
                out.push(R1_IDLE);
            }
            (true, 41) | (false, 1) => {
                self.acmd41_poll(if index == 1 { ACMD41_HCS } else { arg });
                out.push(u8::from(self.state == State::Idle));
            }
            (true, 13) => {
                out.extend([r1, 0x00, 0xFF]);
                out.extend(data_block(&self.sd_status()));
            }
            (true, 51) => {
                out.extend([r1, 0xFF]);
                out.extend(data_block(&Self::scr()));
            }
            (true, 23) | (_, 16 | 59) => out.push(r1),
            (_, 6) => {
                out.extend([r1, 0xFF]);
                out.extend(data_block(&Self::switch_status(arg)));
            }
            (_, 8) => out.extend([r1, 0x00, 0x00, ((arg >> 8) & 0xF) as u8, arg as u8]),
            (_, 9 | 10) => {
                let register = if index == 9 { self.csd } else { self.cid };
                out.extend([r1, 0xFF]);
                out.extend(data_block(&register.to_be_bytes()));
            }
            (_, 13) => out.extend([r1, 0x00]),
            (_, 17 | 18 | 24 | 25) => {
                let lba = u64::from(arg);
                if lba >= self.blocks {
                    out.push(r1 | R1_PARAM);
                } else {
                    out.push(r1);
                    match index {
                        17 => {
                            let block = self.read_block(lba);
                            out.push(0xFF);
                            out.extend(data_block(&block));
                        }
                        18 => self.spi_io.stream = Stream::Next(lba),
                        _ => {
                            self.spi_io.rx = SpiRx::Token { multi: index == 25, lba };
                            self.state = State::Rcv;
                        }
                    }
                }
            }
            (_, 55) => {
                self.app_cmd = true;
                out.push(r1);
            }
            (_, 58) => {
                out.push(r1);
                out.extend(self.ocr().to_be_bytes());
            }
            _ => out.push(r1 | R1_ILLEGAL),
        }
        self.spi_io.out.extend(out);
    }

    /// The card is in SPI mode (tests).
    #[cfg(test)]
    pub(super) const fn in_spi_mode(&self) -> bool {
        self.spi
    }

    /// The SD-mode state (tests).
    #[cfg(test)]
    pub(super) const fn state(&self) -> State {
        self.state
    }
}

/// An SPI data block: the start token, the data and its CRC16.
fn data_block(data: &[u8]) -> Vec<u8> {
    let mut block = Vec::with_capacity(data.len() + 3);
    block.push(TOKEN_START);
    block.extend_from_slice(data);
    block.extend(crc16(data).to_be_bytes());
    block
}
