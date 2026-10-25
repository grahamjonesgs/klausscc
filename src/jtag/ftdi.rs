//! The libftdi1 binding and the MPSSE JTAG master on FT2232H channel A.
//!
//! Built with the `board` cargo feature (links the system libftdi1).  The
//! command framing it sends lives in `mpsse`.

use super::mpsse::{
    dmi_answer, parse_scan, push_dmi, push_scan, DmiReq, IR_IDCODE, IR_LEN, IR_USER3, MPSSE_3PHASE_OFF, MPSSE_ADAPTIVE_OFF, MPSSE_CLK_DIV,
    MPSSE_DIV5_OFF, MPSSE_LOOPBACK_OFF, MPSSE_SEND_IMMEDIATE, MPSSE_SET_HIGH, MPSSE_SET_LOW, MPSSE_TMS,
};
use super::{JtagError, Result};
use std::ffi::{c_char, c_int, c_uchar, c_void, CStr};

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

/// Digilent JTAG pin setup on channel A (from openFPGALoader's "digilent"
/// cable: ADBUS value / direction, ACBUS value / direction).
const DIGILENT_LOW: (u8, u8) = (0xE8, 0xEB);
const DIGILENT_HIGH: (u8, u8) = (0x00, 0x60);

/// MPSSE JTAG master on FT2232H channel A.
///
/// Holds a raw libftdi context, so it is neither `Send` nor `Sync`: the
/// gdbserver drives it from the thread that opened it.
pub(crate) struct Jtag {
    /// A live context from `ftdi_new`, freed in `Drop`; never null.
    ctx: *mut ftdi_context,
    /// Run-Test/Idle clocks after every scan (lets the DMI request cross).
    idle_clocks: u32,
}

impl Jtag {
    /// Open the first FT2232H (0403:6010) channel A and configure MPSSE JTAG
    /// at `khz` kHz (60 MHz / (2 * (divisor + 1))).
    pub(crate) fn open(khz: u32) -> Result<Self> {
        // SAFETY: ftdi_new has no preconditions; a null result is checked below.
        let ctx = unsafe { ftdi_new() };
        if ctx.is_null() {
            return Err(JtagError("ftdi_new failed".into()));
        }
        // From here `jtag` owns `ctx`: its Drop frees it on every early return.
        let mut jtag = Self { ctx, idle_clocks: 16 };
        // SAFETY: `ctx` is the live, non-null context from ftdi_new above, used
        // only through `jtag`; the same holds for each "as above" call below.
        jtag.check(unsafe { ftdi_set_interface(ctx, INTERFACE_A) }, "set interface A")?;
        // SAFETY: as above.
        let rc = unsafe { ftdi_usb_open(ctx, 0x0403, 0x6010) };
        if rc < 0 {
            return Err(JtagError(format!(
                "cannot open the board's USB-JTAG (FT2232H 0403:6010 channel A): {} \
                 (is the board connected, and Vivado's hardware manager / hw_server closed?)",
                jtag.last_error()
            )));
        }
        // SAFETY: as above.
        jtag.check(unsafe { ftdi_usb_reset(ctx) }, "usb reset")?;
        // SAFETY: as above.
        jtag.check(unsafe { ftdi_set_latency_timer(ctx, 1) }, "latency timer")?;
        // SAFETY: as above.
        jtag.check(unsafe { ftdi_set_bitmode(ctx, 0, BITMODE_RESET) }, "bitmode reset")?;
        // SAFETY: as above.
        jtag.check(unsafe { ftdi_set_bitmode(ctx, 0, BITMODE_MPSSE) }, "bitmode MPSSE")?;
        // SAFETY: as above.
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

    /// libftdi's description of the last error on this context.
    fn last_error(&self) -> String {
        // SAFETY: `self.ctx` is a live context (see the field).
        let msg = unsafe { ftdi_get_error_string(self.ctx) };
        if msg.is_null() {
            return "unknown libftdi error".to_owned();
        }
        // SAFETY: a non-null result is a static, NUL-terminated C string.
        unsafe { CStr::from_ptr(msg) }.to_string_lossy().into_owned()
    }

    fn check(&self, rc: c_int, what: &str) -> Result<()> {
        if rc < 0 {
            Err(JtagError(format!("ftdi {what}: {}", self.last_error())))
        } else {
            Ok(())
        }
    }

    fn write(&mut self, buf: &[u8]) -> Result<()> {
        // SAFETY: `self.ctx` is live, and libftdi only reads `buf.len()` bytes of `buf`.
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
            // SAFETY: `self.ctx` is live, and `out[got..]` is valid for `n - got` bytes.
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
    pub(crate) fn reset(&mut self) -> Result<()> {
        // 5 x TMS=1, then TMS=0
        self.write(&[MPSSE_TMS, 5, 0x1F, MPSSE_TMS, 0, 0x00])
    }

    fn scan(&mut self, ir: bool, data: &[u8], len: u32) -> Result<Vec<u8>> {
        let mut cmd = Vec::with_capacity(48 + data.len());
        let n = push_scan(self.idle_clocks, &mut cmd, ir, data, len);
        cmd.push(MPSSE_SEND_IMMEDIATE);
        self.write(&cmd)?;
        let raw = self.read_exact(n)?;
        Ok(parse_scan(&raw, len))
    }

    fn ir(&mut self, value: u64) -> Result<()> {
        self.scan(true, &value.to_le_bytes(), IR_LEN).map(|_| ())
    }

    /// Read the device's 32-bit IDCODE.
    pub(crate) fn idcode(&mut self) -> Result<u32> {
        self.ir(IR_IDCODE)?;
        let out = self.scan(false, &[0; 4], 32)?;
        Ok(u32::from_le_bytes([out[0], out[1], out[2], out[3]]))
    }

    /// Select the debug module's DR (USER3).
    pub(crate) fn select_user3(&mut self) -> Result<()> {
        self.ir(IR_USER3)
    }

    /// Run DMI requests back to back: each 74-bit DR scan
    /// (`{op[1:0], addr[7:0], data[63:0]}` in, `{status, addr, data}` out)
    /// returns the answer to the PREVIOUS request, so `reqs` plus one no-op
    /// scan give every answer in one USB round trip.  Returns each request's
    /// data (reads; writes give 0).  The idle clocks after each scan give
    /// the DM time to answer, memory commands included; a busy status means
    /// they did not, and fails the batch (the request after it was dropped).
    pub(crate) fn dmi_batch(&mut self, reqs: &[DmiReq]) -> Result<Vec<u64>> {
        let mut answers = Vec::with_capacity(reqs.len());
        // keep each transfer's response well inside the FT2232H's 4 KiB buffer
        for chunk in reqs.chunks(200) {
            let mut cmd = Vec::with_capacity((chunk.len() + 1) * 48);
            let mut sizes = Vec::with_capacity(chunk.len() + 1);
            for r in chunk.iter().copied().chain(std::iter::once(DmiReq::Nop)) {
                sizes.push(push_dmi(self.idle_clocks, &mut cmd, r));
            }
            cmd.push(MPSSE_SEND_IMMEDIATE);
            self.write(&cmd)?;
            let raw = self.read_exact(sizes.iter().sum())?;
            let mut at = 0;
            for (i, n) in sizes.iter().enumerate() {
                let scan = &raw[at..at + n];
                at += n;
                if i == 0 {
                    continue; // answers whatever came before the batch
                }
                answers.push(dmi_answer(scan, chunk[i - 1])?);
            }
        }
        Ok(answers)
    }

    /// DMI register read.
    pub(crate) fn dmi_read(&mut self, addr: u8) -> Result<u64> {
        Ok(self.dmi_batch(&[DmiReq::Read(addr)])?[0])
    }

    /// DMI register write.
    pub(crate) fn dmi_write(&mut self, addr: u8, data: u64) -> Result<()> {
        self.dmi_batch(&[DmiReq::Write(addr, data)]).map(|_| ())
    }
}

impl Drop for Jtag {
    fn drop(&mut self) {
        // SAFETY: `self.ctx` is live (see the field) and is never used after ftdi_free.
        unsafe {
            ftdi_set_bitmode(self.ctx, 0, BITMODE_RESET);
            ftdi_usb_close(self.ctx);
            ftdi_free(self.ctx);
        }
    }
}
