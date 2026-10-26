//! MPSSE command framing for JTAG scans and DMI requests.
//!
//! Pure byte-level code: it builds the command stream the FT2232H runs and
//! unpacks the bytes it sends back, so it is tested against a TAP model with
//! no hardware attached.  The `ftdi` module sends the streams.

// Without the `board` feature only the tests below use this module.
#![cfg_attr(
    not(feature = "board"),
    allow(dead_code, reason = "only the tests use the framing without the board feature")
)]

use super::{JtagError, Result};

// ---- MPSSE commands --------------------------------------------------------
pub(super) const MPSSE_SET_LOW: u8 = 0x80; // value, direction (ADBUS)
pub(super) const MPSSE_SET_HIGH: u8 = 0x82; // value, direction (ACBUS)
pub(super) const MPSSE_LOOPBACK_OFF: u8 = 0x85;
pub(super) const MPSSE_CLK_DIV: u8 = 0x86; // divisor lo, hi
pub(super) const MPSSE_SEND_IMMEDIATE: u8 = 0x87;
pub(super) const MPSSE_DIV5_OFF: u8 = 0x8A; // 60 MHz base clock
pub(super) const MPSSE_3PHASE_OFF: u8 = 0x8D;
pub(super) const MPSSE_ADAPTIVE_OFF: u8 = 0x97;
/// Bytes out on -ve edge, in on +ve edge, LSB first.
const MPSSE_BYTES_IO: u8 = 0x39;
/// Bits out on -ve edge, in on +ve edge, LSB first.
const MPSSE_BITS_IO: u8 = 0x3B;
/// TMS bits out on -ve edge (bit 7 of the data byte = TDI), TDO in on +ve.
const MPSSE_TMS_IO: u8 = 0x6B;
/// TMS bits out, no read.
pub(super) const MPSSE_TMS: u8 = 0x4B;

/// Xilinx 7-series JTAG instructions (IR length 6).
pub(super) const IR_LEN: u32 = 6;
pub(super) const IR_IDCODE: u64 = 0x09;
pub(super) const IR_USER3: u64 = 0x22;

/// DR length of the DMI scan.
pub(super) const DMI_BITS: u32 = 74;

/// Append the commands for one scan from Run-Test/Idle: shift `len` bits
/// (LSB first) through IR (`ir`) or DR, end in Run-Test/Idle after
/// `idle_clocks` idle clocks.  Returns how many response bytes it makes.
pub(super) fn push_scan(idle_clocks: u32, cmd: &mut Vec<u8>, ir: bool, data: &[u8], len: u32) -> usize {
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
pub(super) fn parse_scan(raw: &[u8], len: u32) -> Vec<u8> {
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

/// One DMI request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DmiReq {
    /// Read a DMI register.
    Read(u8),
    /// Write a DMI register.
    Write(u8, u64),
    /// No request (collects the previous answer).
    Nop,
}

impl DmiReq {
    /// The request's `(op, addr, data)` fields.
    const fn encode(self) -> (u8, u8, u64) {
        match self {
            Self::Read(a) => (1, a, 0),
            Self::Write(a, d) => (2, a, d),
            Self::Nop => (0, 0, 0),
        }
    }

    /// The DMI register address the request names (0 for `Nop`).
    pub(super) const fn addr(self) -> u8 {
        self.encode().1
    }

    /// The request as the 74-bit DR value `{op[1:0], addr[7:0], data[63:0]}`,
    /// least significant byte first (bits 74..80 are zero).
    pub(super) fn frame(self) -> [u8; 10] {
        let (op, addr, data) = self.encode();
        let mut bits = [0_u8; 10];
        bits[..8].copy_from_slice(&data.to_le_bytes());
        bits[8] = addr;
        bits[9] = op;
        bits
    }
}

/// Append one DMI request's DR scan to `cmd`; returns how many response
/// bytes it makes.
pub(super) fn push_dmi(idle_clocks: u32, cmd: &mut Vec<u8>, req: DmiReq) -> usize {
    push_scan(idle_clocks, cmd, false, &req.frame(), DMI_BITS)
}

/// Decode one DMI scan's response bytes as the answer to `asked` (the
/// request scanned just before it) and return its data.
///
/// A busy status means the DM had not answered within the idle clocks (and
/// dropped the request after it); a different address means the scan did not
/// reach a debug module.
pub(super) fn dmi_answer(raw: &[u8], asked: DmiReq) -> Result<u64> {
    let out = parse_scan(raw, DMI_BITS);
    let want = asked.addr();
    if out[9] & 3 != 0 {
        return Err(JtagError::DmiBusy { addr: want });
    }
    if out[8] != want {
        return Err(JtagError::DmiMismatch { asked: want, got: out[8] });
    }
    Ok(u64::from_le_bytes([out[0], out[1], out[2], out[3], out[4], out[5], out[6], out[7]]))
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

    /// The DR value `{op, addr, data}` a DMI request should shift in.
    fn dmi_value(op: u8, addr: u8, data: u64) -> u128 {
        (u128::from(op) << 72) | (u128::from(addr) << 64) | u128::from(data)
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
            let n = push_scan(5, &mut cmd, false, &to_bytes(sent, len), len);
            let raw = m.run(&cmd);
            assert_eq!(raw.len(), n, "len {len}: response size");
            let got = parse_scan(&raw, len);
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
        let n = push_scan(16, &mut cmd, true, &IR_USER3.to_le_bytes(), IR_LEN);
        let raw = m.run(&cmd);
        assert_eq!(raw.len(), n);
        assert_eq!(parse_scan(&raw, IR_LEN), vec![0b01], "IR capture pattern");
        assert_eq!(m.ir_updates, vec![IR_USER3]);
        assert_eq!(m.idle_clocks, 16);
    }

    #[test]
    fn dmi_frame_packs_data_addr_op() {
        assert_eq!(
            DmiReq::Write(0x12, 0x0807_0605_0403_0201).frame(),
            [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x12, 2]
        );
        assert_eq!(DmiReq::Read(0x34).frame(), [0, 0, 0, 0, 0, 0, 0, 0, 0x34, 1]);
        assert_eq!(DmiReq::Nop.frame(), [0; 10]);
    }

    #[test]
    fn back_to_back_scans_stay_in_step() {
        // the batched DMI layout: several scans in one command stream
        let mut m = Model::new(DMI_BITS, 0x2AB_CDEF_0123_4567_89AB_u128);
        m.state = Tap::Idle;
        let reqs = [DmiReq::Write(0x04, 0x20), DmiReq::Write(0x05, 0x2FF), DmiReq::Read(0x03), DmiReq::Nop];
        let mut cmd = vec![];
        let sizes: Vec<usize> = reqs.iter().map(|&r| push_dmi(16, &mut cmd, r)).collect();
        let raw = m.run(&cmd);
        assert_eq!(raw.len(), sizes.iter().sum::<usize>());
        let mut at = 0;
        for n in sizes {
            let out = parse_scan(&raw[at..at + n], DMI_BITS);
            at += n;
            assert_eq!(out, to_bytes(0x2AB_CDEF_0123_4567_89AB_u128, DMI_BITS));
        }
        let want = vec![
            dmi_value(2, 0x04, 0x20),
            dmi_value(2, 0x05, 0x2FF),
            dmi_value(1, 0x03, 0),
            dmi_value(0, 0, 0),
        ];
        assert_eq!(m.dr_updates, want);
    }

    /// The response bytes of one DMI scan whose DR captured `value`.
    fn dmi_response(value: u128) -> Vec<u8> {
        let mut m = Model::new(DMI_BITS, value);
        m.state = Tap::Idle;
        let mut cmd = vec![];
        push_dmi(16, &mut cmd, DmiReq::Nop);
        m.run(&cmd)
    }

    #[test]
    fn dmi_answer_returns_the_data() {
        let raw = dmi_response(dmi_value(0, 0x11, 0xDEAD_BEEF_0BAD_F00D));
        assert_eq!(dmi_answer(&raw, DmiReq::Read(0x11)).ok(), Some(0xDEAD_BEEF_0BAD_F00D));
    }

    #[test]
    fn dmi_answer_rejects_a_busy_status() {
        let raw = dmi_response(dmi_value(3, 0x11, 0));
        let err = dmi_answer(&raw, DmiReq::Write(0x11, 5)).err();
        assert!(matches!(err, Some(JtagError::DmiBusy { addr: 0x11 })), "{err:?}");
        assert_eq!(
            err.map(|e| e.to_string()).as_deref(),
            Some("DMI busy answering 0x11 (raise the idle clocks, or lower --jtag-khz)")
        );
    }

    #[test]
    fn only_a_busy_debug_module_keeps_the_link() {
        assert!(!JtagError::DmiBusy { addr: 1 }.is_link_lost());
        assert!(JtagError::DmiMismatch { asked: 1, got: 2 }.is_link_lost());
        assert!(JtagError::ReadTimeout.is_link_lost());
        assert_eq!(
            JtagError::Ftdi {
                op: "write",
                message: "usb gone".to_owned()
            }
            .to_string(),
            "ftdi write: usb gone"
        );
    }

    #[test]
    fn dmi_answer_rejects_an_answer_for_another_register() {
        let raw = dmi_response(dmi_value(0, 0x22, 0));
        let err = dmi_answer(&raw, DmiReq::Read(0x11)).err();
        assert!(matches!(err, Some(JtagError::DmiMismatch { asked: 0x11, got: 0x22 })), "{err:?}");
        assert_eq!(
            err.map(|e| e.to_string()).as_deref(),
            Some("DMI answer for 0x22, asked 0x11 (is the bitstream built with DEBUG_EN=1?)")
        );
    }
}
