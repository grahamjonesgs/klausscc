//! JTAG over an FTDI FT2232H in MPSSE mode, and the KlaussCPU debug module's
//! DMI on top of it (`klaussCPU` `debug_module.sv`, `DEBUG_PLAN.md` §4).
//!
//! The Nexys A7's on-board USB-JTAG is a Digilent-programmed FT2232H: channel
//! A is JTAG (MPSSE), channel B the console UART (`/dev/ttyUSB1`).  Opening
//! channel A detaches only its kernel driver, so the UART keeps working.
//! Vivado's `hw_server` (hardware manager) holds the same channel while it
//! runs; close it first.
//!
//! Built with the `jtag` cargo feature (links the system libftdi1).

use std::ffi::{c_char, c_int, c_uchar, c_void, CStr};
use std::fmt;

// ---- libftdi1 (just what MPSSE JTAG needs) ----------------------------------

#[allow(non_camel_case_types)]
type ftdi_context = c_void;

/// libftdi1 `enum ftdi_interface`: `INTERFACE_A`.
const INTERFACE_A: c_int = 1;
/// libftdi1 `enum ftdi_mpsse_mode`.
const BITMODE_RESET: c_uchar = 0x00;
const BITMODE_MPSSE: c_uchar = 0x02;

#[link(name = "ftdi1")]
unsafe extern "C" {
    fn ftdi_new() -> *mut ftdi_context;
    fn ftdi_free(ftdi: *mut ftdi_context);
    fn ftdi_set_interface(ftdi: *mut ftdi_context, interface: c_int) -> c_int;
    fn ftdi_usb_open(ftdi: *mut ftdi_context, vendor: c_int, product: c_int) -> c_int;
    fn ftdi_usb_close(ftdi: *mut ftdi_context) -> c_int;
    fn ftdi_usb_reset(ftdi: *mut ftdi_context) -> c_int;
    fn ftdi_tcioflush(ftdi: *mut ftdi_context) -> c_int;
    fn ftdi_set_latency_timer(ftdi: *mut ftdi_context, latency: c_uchar) -> c_int;
    fn ftdi_set_bitmode(ftdi: *mut ftdi_context, bitmask: c_uchar, mode: c_uchar) -> c_int;
    fn ftdi_write_data(ftdi: *mut ftdi_context, buf: *const c_uchar, size: c_int) -> c_int;
    fn ftdi_read_data(ftdi: *mut ftdi_context, buf: *mut c_uchar, size: c_int) -> c_int;
    fn ftdi_get_error_string(ftdi: *mut ftdi_context) -> *const c_char;
}

/// A JTAG / DMI error.
#[derive(Debug)]
pub struct JtagError(pub String);

impl fmt::Display for JtagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// JTAG result.
pub type Result<T> = std::result::Result<T, JtagError>;

// ---- MPSSE commands --------------------------------------------------------
const MPSSE_SET_LOW: u8 = 0x80; // value, direction (ADBUS)
const MPSSE_SET_HIGH: u8 = 0x82; // value, direction (ACBUS)
const MPSSE_LOOPBACK_OFF: u8 = 0x85;
const MPSSE_CLK_DIV: u8 = 0x86; // divisor lo, hi
const MPSSE_SEND_IMMEDIATE: u8 = 0x87;
const MPSSE_DIV5_OFF: u8 = 0x8A; // 60 MHz base clock
const MPSSE_3PHASE_OFF: u8 = 0x8D;
const MPSSE_ADAPTIVE_OFF: u8 = 0x97;
/// Bytes out on -ve edge, in on +ve edge, LSB first.
const MPSSE_BYTES_IO: u8 = 0x39;
/// Bits out on -ve edge, in on +ve edge, LSB first.
const MPSSE_BITS_IO: u8 = 0x3B;
/// TMS bits out on -ve edge (bit 7 of the data byte = TDI), TDO in on +ve.
const MPSSE_TMS_IO: u8 = 0x6B;
/// TMS bits out, no read.
const MPSSE_TMS: u8 = 0x4B;

/// Digilent JTAG pin setup on channel A (from openFPGALoader's "digilent"
/// cable: ADBUS value / direction, ACBUS value / direction).
const DIGILENT_LOW: (u8, u8) = (0xE8, 0xEB);
const DIGILENT_HIGH: (u8, u8) = (0x00, 0x60);

/// Xilinx 7-series JTAG instructions (IR length 6).
const IR_LEN: u32 = 6;
const IR_IDCODE: u64 = 0x09;
const IR_USER3: u64 = 0x22;

/// MPSSE JTAG master on FT2232H channel A.
pub struct Jtag {
    ctx: *mut ftdi_context,
    /// Run-Test/Idle clocks after every scan (lets the DMI request cross).
    idle_clocks: u32,
}

// SAFETY: the context is only ever used from the owning thread.
unsafe impl Send for Jtag {}

impl Jtag {
    /// Open the first FT2232H (0403:6010) channel A and configure MPSSE JTAG
    /// at `khz` kHz (60 MHz / (2 * (divisor + 1))).
    pub fn open(khz: u32) -> Result<Self> {
        // SAFETY: plain libftdi1 calls on a context we own.
        let ctx = unsafe { ftdi_new() };
        if ctx.is_null() {
            return Err(JtagError("ftdi_new failed".into()));
        }
        let mut jtag = Self { ctx, idle_clocks: 16 };
        jtag.check(unsafe { ftdi_set_interface(ctx, INTERFACE_A) }, "set interface A")?;
        let rc = unsafe { ftdi_usb_open(ctx, 0x0403, 0x6010) };
        if rc < 0 {
            return Err(JtagError(format!(
                "cannot open the board's USB-JTAG (FT2232H 0403:6010 channel A): {} \
                 (is the board connected, and Vivado's hardware manager / hw_server closed?)",
                jtag.last_error()
            )));
        }
        jtag.check(unsafe { ftdi_usb_reset(ctx) }, "usb reset")?;
        jtag.check(unsafe { ftdi_set_latency_timer(ctx, 1) }, "latency timer")?;
        jtag.check(unsafe { ftdi_set_bitmode(ctx, 0, BITMODE_RESET) }, "bitmode reset")?;
        jtag.check(unsafe { ftdi_set_bitmode(ctx, 0, BITMODE_MPSSE) }, "bitmode MPSSE")?;
        jtag.check(unsafe { ftdi_tcioflush(ctx) }, "flush")?;
        let div = (30_000 / khz.max(1)).saturating_sub(1).min(0xFFFF);
        jtag.write(&[
            MPSSE_DIV5_OFF,
            MPSSE_ADAPTIVE_OFF,
            MPSSE_3PHASE_OFF,
            MPSSE_LOOPBACK_OFF,
            MPSSE_CLK_DIV,
            (div & 0xFF) as u8,
            (div >> 8) as u8,
            MPSSE_SET_LOW,
            DIGILENT_LOW.0,
            DIGILENT_LOW.1,
            MPSSE_SET_HIGH,
            DIGILENT_HIGH.0,
            DIGILENT_HIGH.1,
        ])?;
        jtag.reset()?;
        Ok(jtag)
    }

    fn last_error(&self) -> String {
        // SAFETY: libftdi returns a static, NUL-terminated string.
        unsafe { CStr::from_ptr(ftdi_get_error_string(self.ctx)) }.to_string_lossy().into_owned()
    }

    fn check(&self, rc: c_int, what: &str) -> Result<()> {
        if rc < 0 {
            Err(JtagError(format!("ftdi {what}: {}", self.last_error())))
        } else {
            Ok(())
        }
    }

    fn write(&mut self, buf: &[u8]) -> Result<()> {
        // SAFETY: buf is valid for buf.len() bytes.
        let n = unsafe { ftdi_write_data(self.ctx, buf.as_ptr(), buf.len() as c_int) };
        if n as usize == buf.len() {
            Ok(())
        } else {
            Err(JtagError(format!("ftdi write: {}", self.last_error())))
        }
    }

    fn read_exact(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut out = vec![0_u8; n];
        let mut got = 0;
        let mut tries = 0;
        while got < n {
            // SAFETY: out[got..] is valid for n - got bytes.
            let r = unsafe { ftdi_read_data(self.ctx, out[got..].as_mut_ptr(), (n - got) as c_int) };
            if r < 0 {
                return Err(JtagError(format!("ftdi read: {}", self.last_error())));
            }
            got += r as usize;
            tries += 1;
            if tries > 10_000 {
                return Err(JtagError("ftdi read timeout".into()));
            }
        }
        Ok(out)
    }

    /// Test-Logic-Reset, then Run-Test/Idle.
    pub fn reset(&mut self) -> Result<()> {
        // 5 x TMS=1, then TMS=0
        self.write(&[MPSSE_TMS, 5, 0x1F, MPSSE_TMS, 0, 0x00])
    }

    /// Append the commands for one scan from Run-Test/Idle: shift `len` bits
    /// (LSB first) through IR (`ir`) or DR, end in Run-Test/Idle after
    /// `idle_clocks` idle clocks.  Returns how many response bytes it makes.
    fn push_scan(idle_clocks: u32, cmd: &mut Vec<u8>, ir: bool, data: &[u8], len: u32) -> usize {
        assert!(len >= 1 && data.len() * 8 >= len as usize);
        // RTI -> Shift-IR (1,1,0,0) or Shift-DR (1,0,0)
        if ir {
            cmd.extend_from_slice(&[MPSSE_TMS, 3, 0x03]);
        } else {
            cmd.extend_from_slice(&[MPSSE_TMS, 2, 0x01]);
        }
        let body = len - 1; // the last bit goes out with TMS=1
        let full = (body / 8) as usize;
        let rem = body % 8;
        if full > 0 {
            let n = full - 1;
            cmd.extend_from_slice(&[MPSSE_BYTES_IO, (n & 0xFF) as u8, (n >> 8) as u8]);
            cmd.extend_from_slice(&data[..full]);
        }
        if rem > 0 {
            cmd.extend_from_slice(&[MPSSE_BITS_IO, (rem - 1) as u8, data[full]]);
        }
        let last_bit = (data[(body / 8) as usize] >> (body % 8)) & 1;
        // last bit with TMS=1 (-> Exit1), read its TDO; then Update (1), RTI (0)
        cmd.extend_from_slice(&[MPSSE_TMS_IO, 0, (last_bit << 7) | 0x01]);
        cmd.extend_from_slice(&[MPSSE_TMS, 1, 0x01]);
        let mut idle = idle_clocks.max(1);
        while idle > 0 {
            let n = idle.min(7);
            cmd.extend_from_slice(&[MPSSE_TMS, (n - 1) as u8, 0x00]);
            idle -= n;
        }
        full + usize::from(rem > 0) + 1
    }

    /// Unpack one scan's response bytes (see `push_scan`) into `len` bits.
    fn parse_scan(raw: &[u8], len: u32) -> Vec<u8> {
        let body = len - 1;
        let full = (body / 8) as usize;
        let rem = body % 8;
        let mut out = vec![0_u8; len.div_ceil(8) as usize];
        out[..full].copy_from_slice(&raw[..full]);
        let mut bit = full * 8;
        if rem > 0 {
            // bits shift in at the top of the byte: MSB-aligned
            let b = raw[full] >> (8 - rem);
            for i in 0..rem as usize {
                if (b >> i) & 1 != 0 {
                    out[(bit + i) / 8] |= 1 << ((bit + i) % 8);
                }
            }
            bit += rem as usize;
        }
        if (raw[raw.len() - 1] >> 7) & 1 != 0 {
            out[bit / 8] |= 1 << (bit % 8);
        }
        out
    }

    fn scan(&mut self, ir: bool, data: &[u8], len: u32) -> Result<Vec<u8>> {
        let mut cmd = Vec::with_capacity(48 + data.len());
        let n = Self::push_scan(self.idle_clocks, &mut cmd, ir, data, len);
        cmd.push(MPSSE_SEND_IMMEDIATE);
        self.write(&cmd)?;
        let raw = self.read_exact(n)?;
        Ok(Self::parse_scan(&raw, len))
    }

    fn ir(&mut self, value: u64) -> Result<()> {
        self.scan(true, &value.to_le_bytes(), IR_LEN).map(|_| ())
    }

    /// Read the device's 32-bit IDCODE.
    pub fn idcode(&mut self) -> Result<u32> {
        self.ir(IR_IDCODE)?;
        let out = self.scan(false, &[0; 4], 32)?;
        Ok(u32::from_le_bytes([out[0], out[1], out[2], out[3]]))
    }

    /// Select the debug module's DR (USER3).
    pub fn select_user3(&mut self) -> Result<()> {
        self.ir(IR_USER3)
    }

    /// Run DMI requests back to back: each 74-bit DR scan
    /// (`{op[1:0], addr[7:0], data[63:0]}` in, `{status, addr, data}` out)
    /// returns the answer to the PREVIOUS request, so `reqs` plus one no-op
    /// scan give every answer in one USB round trip.  Returns each request's
    /// data (reads; writes give 0).  The idle clocks after each scan give
    /// the DM time to answer, memory commands included; a busy status means
    /// they did not, and fails the batch (the request after it was dropped).
    pub fn dmi_batch(&mut self, reqs: &[DmiReq]) -> Result<Vec<u64>> {
        let mut answers = Vec::with_capacity(reqs.len());
        // keep each transfer's response well inside the FT2232H's 4 KiB buffer
        for chunk in reqs.chunks(200) {
            let mut cmd = Vec::with_capacity((chunk.len() + 1) * 48);
            let mut sizes = Vec::with_capacity(chunk.len() + 1);
            for r in chunk.iter().copied().chain(std::iter::once(DmiReq::Nop)) {
                let (op, addr, data) = r.encode();
                let mut bits = [0_u8; 10];
                bits[..8].copy_from_slice(&data.to_le_bytes());
                bits[8] = addr;
                bits[9] = op;
                sizes.push(Self::push_scan(self.idle_clocks, &mut cmd, false, &bits, DMI_BITS));
            }
            cmd.push(MPSSE_SEND_IMMEDIATE);
            self.write(&cmd)?;
            let raw = self.read_exact(sizes.iter().sum())?;
            let mut at = 0;
            for (i, n) in sizes.iter().enumerate() {
                let out = Self::parse_scan(&raw[at..at + n], DMI_BITS);
                at += n;
                if i == 0 {
                    continue; // answers whatever came before the batch
                }
                let status = out[9] & 3;
                let (_, want, _) = chunk[i - 1].encode();
                if status != 0 {
                    return Err(JtagError(format!(
                        "DMI busy answering 0x{want:02X} (raise the idle clocks, or lower --jtag-khz)"
                    )));
                }
                if out[8] != want {
                    return Err(JtagError(format!(
                        "DMI answer for 0x{:02X}, asked 0x{want:02X} (is the bitstream built with DEBUG_EN=1?)",
                        out[8]
                    )));
                }
                answers.push(u64::from_le_bytes([out[0], out[1], out[2], out[3], out[4], out[5], out[6], out[7]]));
            }
        }
        Ok(answers)
    }

    /// DMI register read.
    pub fn dmi_read(&mut self, addr: u8) -> Result<u64> {
        Ok(self.dmi_batch(&[DmiReq::Read(addr)])?[0])
    }

    /// DMI register write.
    pub fn dmi_write(&mut self, addr: u8, data: u64) -> Result<()> {
        self.dmi_batch(&[DmiReq::Write(addr, data)]).map(|_| ())
    }
}

/// DR length of the DMI scan.
const DMI_BITS: u32 = 74;

/// One DMI request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmiReq {
    /// Read a DMI register.
    Read(u8),
    /// Write a DMI register.
    Write(u8, u64),
    /// No request (collects the previous answer).
    Nop,
}

impl DmiReq {
    const fn encode(self) -> (u8, u8, u64) {
        match self {
            Self::Read(a) => (1, a, 0),
            Self::Write(a, d) => (2, a, d),
            Self::Nop => (0, 0, 0),
        }
    }
}

impl Drop for Jtag {
    fn drop(&mut self) {
        // SAFETY: the context is valid until freed here.
        unsafe {
            ftdi_set_bitmode(self.ctx, 0, BITMODE_RESET);
            ftdi_usb_close(self.ctx);
            ftdi_free(self.ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unnested_or_patterns, reason = "the TAP table reads better flat")]
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Tap {
        Reset,
        Idle,
        SelDr,
        SelIr,
        Capture(bool),
        Shift(bool),
        Exit1(bool),
        Update(bool),
    }

    /// A TAP with one IR and one DR, driven by an MPSSE command stream the
    /// way the FT2232H does (TDI/TMS out on -ve, TDO sampled on +ve, reads
    /// returned per command as the chip packs them).
    struct Model {
        state: Tap,
        ir_len: u32,
        dr_len: u32,
        ir: u64,
        sr: u128,
        /// Value loaded at Capture-DR.
        capture: u128,
        /// Values seen at Update-IR / Update-DR.
        ir_updates: Vec<u64>,
        dr_updates: Vec<u128>,
        idle_clocks: u32,
    }

    impl Model {
        fn new(dr_len: u32, capture: u128) -> Self {
            Self {
                state: Tap::Reset,
                ir_len: IR_LEN,
                dr_len,
                ir: 0,
                sr: 0,
                capture,
                ir_updates: vec![],
                dr_updates: vec![],
                idle_clocks: 0,
            }
        }

        /// One TCK; returns TDO.
        fn clock(&mut self, tms: bool, tdi: bool) -> bool {
            let mut tdo = false;
            if let Tap::Shift(ir) = self.state {
                let len = if ir { self.ir_len } else { self.dr_len };
                tdo = self.sr & 1 != 0;
                self.sr = (self.sr >> 1) | (u128::from(tdi) << (len - 1));
            }
            if self.state == Tap::Idle && !tms {
                self.idle_clocks += 1;
            }
            self.state = match (self.state, tms) {
                (Tap::Reset, false) | (Tap::Idle, false) | (Tap::Update(_), false) => Tap::Idle,
                (Tap::Reset, true) | (Tap::SelIr, true) => Tap::Reset,
                (Tap::Idle, true) | (Tap::Update(_), true) => Tap::SelDr,
                (Tap::SelDr, true) => Tap::SelIr,
                (Tap::SelDr, false) => Tap::Capture(false),
                (Tap::SelIr, false) => Tap::Capture(true),
                (Tap::Capture(ir), false) | (Tap::Shift(ir), false) => Tap::Shift(ir),
                (Tap::Capture(ir), true) | (Tap::Shift(ir), true) => Tap::Exit1(ir),
                (Tap::Exit1(ir), true) => Tap::Update(ir),
                (Tap::Exit1(_), false) => panic!("pause states are not used"),
            };
            match self.state {
                Tap::Capture(true) => self.sr = 0b01, // 7-series IR capture
                Tap::Capture(false) => self.sr = self.capture,
                Tap::Update(true) => {
                    self.ir = self.sr as u64;
                    self.ir_updates.push(self.ir);
                }
                Tap::Update(false) => self.dr_updates.push(self.sr),
                _ => {}
            }
            tdo
        }

        /// Run an MPSSE command stream; returns the bytes the chip sends back.
        fn run(&mut self, cmd: &[u8]) -> Vec<u8> {
            let mut out = vec![];
            let mut i = 0;
            while i < cmd.len() {
                match cmd[i] {
                    MPSSE_TMS | MPSSE_TMS_IO => {
                        let n = u32::from(cmd[i + 1]) + 1;
                        let d = cmd[i + 2];
                        let tdi = d & 0x80 != 0;
                        let mut r = 0_u8;
                        for b in 0..n {
                            let tdo = self.clock((d >> b) & 1 != 0, tdi);
                            r = (r >> 1) | (u8::from(tdo) << 7);
                        }
                        if cmd[i] == MPSSE_TMS_IO {
                            out.push(r);
                        }
                        i += 3;
                    }
                    MPSSE_BYTES_IO => {
                        let n = usize::from(cmd[i + 1]) + (usize::from(cmd[i + 2]) << 8) + 1;
                        for k in 0..n {
                            let d = cmd[i + 3 + k];
                            let mut r = 0_u8;
                            for b in 0..8 {
                                r |= u8::from(self.clock(false, (d >> b) & 1 != 0)) << b;
                            }
                            out.push(r);
                        }
                        i += 3 + n;
                    }
                    MPSSE_BITS_IO => {
                        let n = cmd[i + 1] + 1;
                        let d = cmd[i + 2];
                        let mut r = 0_u8;
                        for b in 0..n {
                            let tdo = self.clock(false, (d >> b) & 1 != 0);
                            r = (r >> 1) | (u8::from(tdo) << 7);
                        }
                        out.push(r);
                        i += 3;
                    }
                    MPSSE_SEND_IMMEDIATE => i += 1,
                    c => panic!("unexpected MPSSE command 0x{c:02X}"),
                }
            }
            out
        }
    }

    fn to_bytes(v: u128, len: u32) -> Vec<u8> {
        v.to_le_bytes()[..len.div_ceil(8) as usize].to_vec()
    }

    #[test]
    fn dr_scans_shift_and_capture_every_length() {
        for len in [1_u32, 5, 7, 8, 9, 15, 16, 17, 32, 63, 64, 73, 74, 100] {
            let mask = if len == 128 { u128::MAX } else { (1_u128 << len) - 1 };
            let sent = 0x0123_4567_89AB_CDEF_FEDC_BA98_7654_3210_u128 & mask;
            let captured = 0xA5A5_5A5A_F00F_0FF0_1234_5678_9ABC_DEF0_u128 & mask;
            let mut m = Model::new(len, captured);
            m.state = Tap::Idle;
            let mut cmd = vec![];
            let n = Jtag::push_scan(5, &mut cmd, false, &to_bytes(sent, len), len);
            let raw = m.run(&cmd);
            assert_eq!(raw.len(), n, "len {len}: response size");
            let got = Jtag::parse_scan(&raw, len);
            assert_eq!(got, to_bytes(captured, len), "len {len}: TDO");
            assert_eq!(m.dr_updates, vec![sent], "len {len}: TDI");
            assert_eq!(m.state, Tap::Idle, "len {len}: ends in Run-Test/Idle");
            assert_eq!(m.idle_clocks, 5, "len {len}: idle clocks");
        }
    }

    #[test]
    fn ir_scan_loads_user3() {
        let mut m = Model::new(74, 0);
        m.state = Tap::Idle;
        let mut cmd = vec![];
        let n = Jtag::push_scan(16, &mut cmd, true, &IR_USER3.to_le_bytes(), IR_LEN);
        let raw = m.run(&cmd);
        assert_eq!(raw.len(), n);
        assert_eq!(Jtag::parse_scan(&raw, IR_LEN), vec![0b01], "IR capture pattern");
        assert_eq!(m.ir_updates, vec![IR_USER3]);
        assert_eq!(m.idle_clocks, 16);
    }

    #[test]
    fn back_to_back_scans_stay_in_step() {
        // the batched DMI layout: several scans in one command stream
        let mut m = Model::new(DMI_BITS, 0x2AB_CDEF_0123_4567_89AB_u128);
        m.state = Tap::Idle;
        let reqs = [DmiReq::Write(0x04, 0x20), DmiReq::Write(0x05, 0x2FF), DmiReq::Read(0x03), DmiReq::Nop];
        let mut cmd = vec![];
        let mut sizes = vec![];
        for r in reqs {
            let (op, addr, data) = r.encode();
            let mut bits = [0_u8; 10];
            bits[..8].copy_from_slice(&data.to_le_bytes());
            bits[8] = addr;
            bits[9] = op;
            sizes.push(Jtag::push_scan(16, &mut cmd, false, &bits, DMI_BITS));
        }
        let raw = m.run(&cmd);
        assert_eq!(raw.len(), sizes.iter().sum::<usize>());
        let mut at = 0;
        for n in sizes {
            let out = Jtag::parse_scan(&raw[at..at + n], DMI_BITS);
            at += n;
            assert_eq!(out, to_bytes(0x2AB_CDEF_0123_4567_89AB_u128, DMI_BITS));
        }
        let want: Vec<u128> = reqs
            .iter()
            .map(|r| {
                let (op, addr, data) = r.encode();
                (u128::from(op) << 72) | (u128::from(addr) << 64) | u128::from(data)
            })
            .collect();
        assert_eq!(m.dr_updates, want);
    }
}
