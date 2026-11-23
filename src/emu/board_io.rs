//! The board's simple devices (klaussCPU `MMIO_MAP.md`; the RTL is the MMIO
//! read mux and write handler in `KlaussCPU.sv`): the two RGB LEDs at
//! `0xF002_0000`, the 7-segment display at `0xF003_0000`, and the LED bar
//! and slide switches at `0xF004_0000`.
//!
//! Reads are the RTL's: each register masked to its width, the display in
//! its padded `{4'h0, digit}` per byte form, and every other offset 0.  A
//! program starts as one loaded onto the board does: `LOAD_COMPLETE` clears
//! the LEDs and the RGB LEDs, and `START_WAIT` blanks both displays as it
//! starts the pipeline (`0x22` per byte, the display controller's " ").
//! The switches read the value given with `--switches` (0 by default).
//!
//! The board's devices ignore the byte enables, and the core replicates a
//! sub-word store's data across the doubleword (`pipeline_core.sv`
//! `exo_wdata`), so a byte store of `0x5A` to `LEDS` lights `0x5A5A`; the
//! emulator does the same.
//!
//! `SEG_RAW` (`0xF003_0020`) and `SEG_MODE` (`0x0028`, `CPU_FEATURES[30]`,
//! klaussCPU `seg7raw`) are the exception: `SEG_RAW` honours the byte
//! enables, so a byte or 32-bit store sets only its own digits, and any load
//! returns its own lanes.  `SEG_RAW` is the same 64 bits `SEG_ALL` reads; a
//! store to it sets `SEG_MODE.RAW` (the display shows segment patterns), and
//! `SEG_LOW`, `SEG_HIGH`, `SEG_ALL`, `SEG_BLANK` and a program start clear
//! it, `SEG_LOW` / `SEG_HIGH` blanking the other four digits as they do.
//! Without them (`--no-seg7raw`, the bitstreams before) both read 0 and
//! drop writes.
//!
//! With `--board-io-log FILE` each write to an LED or display register is
//! logged, one line per store with the value it latched (`LEDS 0x5a5a`,
//! `RGB1 0xf00`, `SEG_LOW 1234`, `SEG_HIGH abcd`, `SEG_ALL deadbeef`,
//! `SEG_BLANK`, `SEG_RAW 807f3f065b4f666d`).  A `SEG_RAW` store narrower
//! than 64 bits logs the digits it set, numbered from the right (byte *k*
//! is digit *k*), and their bytes, leftmost first: `SEG_RAW[3] ab`,
//! `SEG_RAW[1:0] 5566`, `SEG_RAW[7:4] 11223344`.

use std::io::Write;

use super::Cpu;

/// RGB LED device select (`addr[27:16]`).
pub(super) const RGB_DEV_SELECT: u32 = 0x002;
/// 7-segment display device select (`addr[27:16]`).
pub(super) const SEG_DEV_SELECT: u32 = 0x003;
/// LED bar / slide switch device select (`addr[27:16]`).
pub(super) const IO_DEV_SELECT: u32 = 0x004;

// Register offsets (`addr[15:0]`).
/// `RGB1`: `[11:8]` R, `[7:4]` G, `[3:0]` B.
const RGB1: u32 = 0x0000;
/// `RGB2`, as `RGB1`.
const RGB2: u32 = 0x0008;
/// `SEG_LOW`: a write takes 4 hex digits `[15:0]` for the lower display; a
/// read returns its padded value, 32 bits (`st.seven_seg_value2`).
const SEG_LOW: u32 = 0x0000;
/// `SEG_HIGH`: as `SEG_LOW`, for the upper display (`st.seven_seg_value1`).
const SEG_HIGH: u32 = 0x0008;
/// `SEG_ALL`: a write takes 8 hex digits `[31:0]`, `[31:16]` to the upper
/// display; a read returns `{upper_padded, lower_padded}`, 64 bits.
const SEG_ALL: u32 = 0x0010;
/// `SEG_BLANK` (write only): any write blanks both displays.
const SEG_BLANK: u32 = 0x0018;
/// `SEG_RAW` (`CPU_FEATURES[30]`): one segment pattern per digit, byte *k*
/// digit *k* from the right; the same 64 bits as `SEG_ALL`'s read.  Decoded
/// on `addr[15:3]`, so a narrow access keeps its lanes.
const SEG_RAW: u32 = 0x0020;
/// `SEG_MODE` (read only, `CPU_FEATURES[30]`): `[0]` RAW.
const SEG_MODE: u32 = 0x0028;
/// `LEDS`: the 16-bit LED bar.
const LEDS: u32 = 0x0000;
/// `SWITCHES` (read only): the 16 slide switches; writes are dropped.
const SWITCHES: u32 = 0x0008;

/// An RGB LED register's bits.
const RGB_MASK: u64 = 0xFFF;
/// `CPU_FEATURES[30]`: `SEG_RAW` and `SEG_MODE`.
pub(super) const CPU_FEATURES_SEG_RAW: u64 = 1 << 30;
/// A blank display: `0x22`, the controller's " " glyph, in each digit
/// (`KlaussCPU.sv` `SEG_BLANK`, and `START_WAIT` as a program starts).
pub(super) const SEG_BLANK_VALUE: u32 = 0x2222_2222;

/// Four hex digits (`[15:0]`) in the display's padded form, `{4'h0, digit}`
/// per byte.
const fn pad(digits: u64) -> u32 {
    let d = digits as u32;
    (d & 0xF) | (((d >> 4) & 0xF) << 8) | (((d >> 8) & 0xF) << 16) | (((d >> 12) & 0xF) << 24)
}

/// A store's data as the device sees it: the core replicates a sub-word
/// store's `bytes` low bytes across the doubleword.
const fn replicate(data: u64, bytes: u32) -> u64 {
    match bytes {
        1 => (data & 0xFF) * 0x0101_0101_0101_0101,
        2 => (data & 0xFFFF) * 0x0001_0001_0001_0001,
        4 => (data & 0xFFFF_FFFF) * 0x0000_0001_0000_0001,
        _ => data,
    }
}

/// Where `--board-io-log` writes: one line per LED or display write.  A
/// failed write ends the log (with one note on stderr), not the run.
pub struct BoardIoLog {
    /// The file, or stderr.
    sink: Box<dyn Write + Send>,
    /// Put before each line: `klausscc io: ` on stderr, as the emulator's
    /// other stderr notes are (`klausscc sd: `), and nothing in a file.
    prefix: &'static str,
}

impl BoardIoLog {
    /// Log to `sink`.  Each line is one `write_all`, so an unbuffered file
    /// holds every line up to a killed run.
    pub fn new(sink: impl Write + Send + 'static) -> Self {
        Self {
            sink: Box::new(sink),
            prefix: "",
        }
    }

    /// Log to stderr, each line after `klausscc io: `.
    #[must_use]
    pub fn stderr() -> Self {
        Self {
            sink: Box::new(std::io::stderr()),
            prefix: "klausscc io: ",
        }
    }
}

impl std::fmt::Debug for BoardIoLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoardIoLog").field("prefix", &self.prefix).finish_non_exhaustive()
    }
}

/// The LEDs, the RGB LEDs, the display and the switches.
#[derive(Debug)]
pub(crate) struct BoardIo {
    /// `RGB1`, `RGB2` (`st.RGB_LED_1/2`).
    rgb: [u16; 2],
    /// The upper display's padded value (`st.seven_seg_value1`).
    seg_upper: u32,
    /// The lower display's padded value (`st.seven_seg_value2`).
    seg_lower: u32,
    /// `SEG_MODE.RAW` (`r_seg_raw`): the display bytes are segment patterns.
    seg_raw: bool,
    /// The bitstream has `SEG_RAW` / `SEG_MODE` (not `--no-seg7raw`).
    pub(super) has_seg_raw: bool,
    /// `LEDS` (`st.led`).
    leds: u16,
    /// What `SWITCHES` reads (`--switches`).
    pub(super) switches: u16,
    /// `--board-io-log`.
    pub(super) log: Option<BoardIoLog>,
}

impl Default for BoardIo {
    /// The state a program starts in: LEDs off, displays blank.
    fn default() -> Self {
        Self {
            rgb: [0; 2],
            seg_upper: SEG_BLANK_VALUE,
            seg_lower: SEG_BLANK_VALUE,
            seg_raw: false,
            has_seg_raw: true,
            leds: 0,
            switches: 0,
            log: None,
        }
    }
}

impl BoardIo {
    /// Both displays as `SEG_ALL` reads them, `{upper, lower}`.
    fn seg_all(&self) -> u64 {
        (u64::from(self.seg_upper) << 32) | u64::from(self.seg_lower)
    }

    /// Read register `offset` of device `dev` (one of the three selects).
    /// `SEG_RAW` and `SEG_MODE` return the value shifted down to the
    /// access's lane, as the core takes a load's bytes from its lanes.
    pub(super) fn read(&self, dev: u32, offset: u32) -> u64 {
        let lane = 8 * (offset & 7);
        match (dev, offset) {
            (SEG_DEV_SELECT, o) if self.has_seg_raw && o & !7 == SEG_RAW => self.seg_all() >> lane,
            (SEG_DEV_SELECT, o) if self.has_seg_raw && o & !7 == SEG_MODE => u64::from(self.seg_raw) >> lane,
            (RGB_DEV_SELECT, RGB1) => u64::from(self.rgb[0]),
            (RGB_DEV_SELECT, RGB2) => u64::from(self.rgb[1]),
            (SEG_DEV_SELECT, SEG_LOW) => u64::from(self.seg_lower),
            (SEG_DEV_SELECT, SEG_HIGH) => u64::from(self.seg_upper),
            (SEG_DEV_SELECT, SEG_ALL) => self.seg_all(),
            (IO_DEV_SELECT, LEDS) => u64::from(self.leds),
            (IO_DEV_SELECT, SWITCHES) => u64::from(self.switches),
            _ => 0,
        }
    }

    /// A store of `bytes` bytes of `data` to register `offset` of device
    /// `dev`.
    pub(super) fn write(&mut self, dev: u32, offset: u32, data: u64, bytes: u32) {
        let data = replicate(data, bytes);
        let line = match (dev, offset) {
            (RGB_DEV_SELECT, RGB1 | RGB2) => {
                let i = (offset / 8) as usize;
                self.rgb[i] = (data & RGB_MASK) as u16;
                format!("RGB{} 0x{:03x}", i + 1, self.rgb[i])
            }
            // SEG_LOW / SEG_HIGH leave raw mode; the other four digits held
            // segment patterns, so they are blanked.
            (SEG_DEV_SELECT, SEG_LOW) => {
                self.seg_lower = pad(data);
                if std::mem::take(&mut self.seg_raw) {
                    self.seg_upper = SEG_BLANK_VALUE;
                }
                format!("SEG_LOW {:04x}", data & 0xFFFF)
            }
            (SEG_DEV_SELECT, SEG_HIGH) => {
                self.seg_upper = pad(data);
                if std::mem::take(&mut self.seg_raw) {
                    self.seg_lower = SEG_BLANK_VALUE;
                }
                format!("SEG_HIGH {:04x}", data & 0xFFFF)
            }
            (SEG_DEV_SELECT, SEG_ALL) => {
                self.seg_upper = pad(data >> 16);
                self.seg_lower = pad(data);
                self.seg_raw = false;
                format!("SEG_ALL {:08x}", data & 0xFFFF_FFFF)
            }
            (SEG_DEV_SELECT, SEG_BLANK) => {
                self.seg_upper = SEG_BLANK_VALUE;
                self.seg_lower = SEG_BLANK_VALUE;
                self.seg_raw = false;
                "SEG_BLANK".to_owned()
            }
            (SEG_DEV_SELECT, o) if self.has_seg_raw && o & !7 == SEG_RAW => self.write_seg_raw(o & 7, data, bytes),
            (IO_DEV_SELECT, LEDS) => {
                self.leds = data as u16;
                format!("LEDS 0x{:04x}", self.leds)
            }
            _ => return,
        };
        self.log_line(&line);
    }

    /// A `SEG_RAW` store of `bytes` bytes at lane `lane` (the address's
    /// `[2:0]`) with the lane-replicated `data`: the byte enables are
    /// honoured, so only its own digits change.  Returns its log line.
    fn write_seg_raw(&mut self, lane: u32, data: u64, bytes: u32) -> String {
        let (lo, hi) = (lane, (lane + bytes.min(8)).min(8) - 1);
        let mut all = self.seg_all();
        let mask = (u64::MAX >> (8 * (7 - (hi - lo)))) << (8 * lo);
        all = (all & !mask) | (data & mask);
        self.seg_upper = (all >> 32) as u32;
        self.seg_lower = all as u32;
        self.seg_raw = true;
        let width = 2 * (hi - lo + 1) as usize;
        let value = (all & mask) >> (8 * lo);
        match (lo, hi) {
            (0, 7) => format!("SEG_RAW {value:016x}"),
            _ if lo == hi => format!("SEG_RAW[{lo}] {value:0width$x}"),
            _ => format!("SEG_RAW[{hi}:{lo}] {value:0width$x}"),
        }
    }

    /// Append `line` to the `--board-io-log`, if there is one.
    fn log_line(&mut self, line: &str) {
        let Some(log) = self.log.as_mut() else {
            return;
        };
        if let Err(e) = log.sink.write_all(format!("{}{line}\n", log.prefix).as_bytes()) {
            eprintln!("klausscc io: --board-io-log write failed ({e}); no more board I/O is logged");
            self.log = None;
        }
    }
}

impl Cpu {
    /// Give the display `SEG_RAW` and `SEG_MODE` (klaussCPU `seg7raw`,
    /// `CPU_FEATURES[30]`; the default), or take them away (`--no-seg7raw`,
    /// the bitstreams before them): then both read 0, writes to them are
    /// dropped and `CPU_FEATURES[30]` reads 0.  Taking them away leaves raw
    /// mode.
    pub fn set_has_seg_raw(&mut self, present: bool) {
        self.board_io.has_seg_raw = present;
        self.board_io.seg_raw &= present;
    }

    /// Whether the display has `SEG_RAW` and `SEG_MODE`
    /// ([`Cpu::set_has_seg_raw`]).
    #[must_use]
    pub const fn has_seg_raw(&self) -> bool {
        self.board_io.has_seg_raw
    }
}
