//! The `KlaussCPU` architecture as the GDB servers describe it: the register
//! file and its `g`-packet layout, the target description XML, and the
//! constants both targets share.
//!
//! Register layout (`g` packet order, little-endian), matching the DWARF
//! numbers in `KlaussCPURegisterInfo.td`:
//!
//! | regnum | name     | bits | DWARF | generic |
//! |--------|----------|------|-------|---------|
//! | 0-15   | r0-r15   | 64   | 0-15  | r0-r3 = arg1-4, r15 = fp |
//! | 16     | sp       | 32   | 16    | sp      |
//! | 17     | pc       | 32   | 17    | pc (= the CFI return-address column) |
//! | 18     | flags    | 32   | -     | flags: bit3 Z, bit2 S, bit1 C, bit0 V |
//! | 19     | `int_mask` | 32 | -     | per-source interrupt enables (`INT_MASK`) |
//! | 20     | `sp_alt` | 32   | -     | MMU stage 1: the inactive stack pointer (`SP_ALT`) |
//! | 21     | `mode`   | 32   | -     | MMU stage 1: bit 0 UM (user mode), bit 1 DI, as SR `STATUS` |
//!
//! Registers 20 and 21 (`MMU_PLAN.md` §6.2) exist on the emulator and on a
//! board whose debug module reports version 2 or later.  Without them the
//! target description stops at 19 and the `g` packet has 20 registers.

use gdbstub::arch::{Arch, RegId, Registers};
use std::num::NonZeroUsize;

/// `TRAP` (v2 class B, OP 4): the debugger breakpoint instruction.
pub(crate) const TRAP_WORD: u32 = 0x6C04_0000;
/// Hardware breakpoint slots, matching the board's debug module.
pub(crate) const HW_BREAKPOINTS: usize = 4;
/// Hardware watchpoint slots, matching the board's debug module.
pub(crate) const HW_WATCHPOINTS: usize = 2;
/// The target triple reported to the debugger.
pub(crate) const TRIPLE: &str = "klausscpu-unknown-elf";

/// The `mode` register (21): user mode, bit 0 (`MMU_PLAN` §6.2: the same
/// layout as SR `STATUS`).
pub(crate) const MODE_UM: u32 = 1;
/// The `mode` register (21): interrupts disabled (`DI`), bit 1.
pub(crate) const MODE_DI: u32 = 1 << 1;

/// The target description up to register 19, without its closing tags
/// (a macro, so `concat!` can build both descriptions from it).
macro_rules! target_xml_base {
    () => {
        r#"<?xml version="1.0"?>
<!DOCTYPE target SYSTEM "gdb-target.dtd">
<target version="1.0">
  <architecture>klausscpu</architecture>
  <feature name="org.klausscpu.core">
    <flags id="klausscpu_flags" size="4">
      <field name="V" start="0" end="0"/>
      <field name="C" start="1" end="1"/>
      <field name="S" start="2" end="2"/>
      <field name="Z" start="3" end="3"/>
    </flags>
    <reg name="r0" altname="a" bitsize="64" regnum="0" type="int64" group="general" dwarf_regnum="0" generic="arg1"/>
    <reg name="r1" altname="b" bitsize="64" regnum="1" type="int64" group="general" dwarf_regnum="1" generic="arg2"/>
    <reg name="r2" altname="c" bitsize="64" regnum="2" type="int64" group="general" dwarf_regnum="2" generic="arg3"/>
    <reg name="r3" altname="d" bitsize="64" regnum="3" type="int64" group="general" dwarf_regnum="3" generic="arg4"/>
    <reg name="r4" altname="e" bitsize="64" regnum="4" type="int64" group="general" dwarf_regnum="4"/>
    <reg name="r5" altname="f" bitsize="64" regnum="5" type="int64" group="general" dwarf_regnum="5"/>
    <reg name="r6" altname="g" bitsize="64" regnum="6" type="int64" group="general" dwarf_regnum="6"/>
    <reg name="r7" altname="h" bitsize="64" regnum="7" type="int64" group="general" dwarf_regnum="7"/>
    <reg name="r8" altname="i" bitsize="64" regnum="8" type="int64" group="general" dwarf_regnum="8"/>
    <reg name="r9" altname="j" bitsize="64" regnum="9" type="int64" group="general" dwarf_regnum="9"/>
    <reg name="r10" altname="k" bitsize="64" regnum="10" type="int64" group="general" dwarf_regnum="10"/>
    <reg name="r11" altname="l" bitsize="64" regnum="11" type="int64" group="general" dwarf_regnum="11"/>
    <reg name="r12" altname="m" bitsize="64" regnum="12" type="int64" group="general" dwarf_regnum="12"/>
    <reg name="r13" altname="n" bitsize="64" regnum="13" type="int64" group="general" dwarf_regnum="13"/>
    <reg name="r14" altname="o" bitsize="64" regnum="14" type="int64" group="general" dwarf_regnum="14"/>
    <reg name="r15" altname="p" bitsize="64" regnum="15" type="int64" group="general" dwarf_regnum="15" generic="fp"/>
    <reg name="sp" bitsize="32" regnum="16" type="data_ptr" group="general" dwarf_regnum="16" generic="sp"/>
    <reg name="pc" bitsize="32" regnum="17" type="uint32" group="general" dwarf_regnum="17" generic="pc"/>
    <reg name="flags" bitsize="32" regnum="18" type="klausscpu_flags" group="general" generic="flags"/>
    <reg name="int_mask" bitsize="32" regnum="19" type="uint32" group="general"/>
"#
    };
}

/// The closing tags of the target description.
macro_rules! target_xml_end {
    () => {
        "  </feature>\n</target>"
    };
}

/// The target description: the register layout of the module docs, with
/// the MMU stage 1 registers 20 and 21.
const TARGET_XML: &str = concat!(
    target_xml_base!(),
    r#"    <flags id="klausscpu_mode" size="4">
      <field name="UM" start="0" end="0"/>
      <field name="DI" start="1" end="1"/>
    </flags>
    <reg name="sp_alt" bitsize="32" regnum="20" type="data_ptr" group="system"/>
    <reg name="mode" bitsize="32" regnum="21" type="klausscpu_mode" group="system"/>
"#,
    target_xml_end!()
);

/// The target description of a core without MMU stage 1 (registers 0-19),
/// for a board whose debug module is version 1.
#[cfg_attr(
    not(any(test, feature = "board")),
    expect(dead_code, reason = "only the board target has cores without stage 1")
)]
pub(crate) const TARGET_XML_V1: &str = concat!(target_xml_base!(), target_xml_end!());

/// The bytes of the target description for a `qXfer:features:read` of
/// `offset`/`length`, copied into `buf`: the full one when `stage1`, else
/// [`TARGET_XML_V1`].  Returns the number of bytes copied.
#[cfg_attr(
    not(any(test, feature = "board")),
    expect(dead_code, reason = "only the board target has cores without stage 1")
)]
pub(crate) fn target_xml_range(stage1: bool, offset: u64, length: usize, buf: &mut [u8]) -> usize {
    let xml = if stage1 { TARGET_XML } else { TARGET_XML_V1 }.as_bytes();
    let start = usize::try_from(offset).unwrap_or(usize::MAX).min(xml.len());
    let n = length.min(buf.len()).min(xml.len() - start);
    buf[..n].copy_from_slice(&xml[start..start + n]);
    n
}

/// The `KlaussCPU` register file in `g`-packet layout.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct KlaussRegs {
    pub(crate) r: [u64; 16],
    pub(crate) sp: u32,
    pub(crate) pc: u32,
    pub(crate) flags: u32,
    pub(crate) int_mask: u32,
    /// MMU stage 1 register 20, `SP_ALT`.
    pub(crate) sp_alt: u32,
    /// MMU stage 1 register 21, [`MODE_UM`] | [`MODE_DI`].
    pub(crate) mode: u32,
    /// Registers 20 and 21 exist (and are in the `g` packet).
    pub(crate) stage1: bool,
}

/// Bytes in a `g` packet: 16 64-bit GPRs, then sp, pc, flags and `int_mask`.
const REG_BYTES: usize = 16 * 8 + 4 * 4;
/// Bytes in a `g` packet with the MMU stage 1 registers `sp_alt` and `mode`.
const REG_BYTES_STAGE1: usize = REG_BYTES + 2 * 4;

impl Registers for KlaussRegs {
    type ProgramCounter = u32;

    fn pc(&self) -> u32 {
        self.pc
    }

    fn gdb_serialize(&self, mut write_byte: impl FnMut(Option<u8>)) {
        for r in &self.r {
            r.to_le_bytes().iter().for_each(|b| write_byte(Some(*b)));
        }
        let stage1 = [self.sp_alt, self.mode];
        let extra: &[u32] = if self.stage1 { &stage1 } else { &[] };
        for v in [self.sp, self.pc, self.flags, self.int_mask].iter().chain(extra) {
            v.to_le_bytes().iter().for_each(|b| write_byte(Some(*b)));
        }
    }

    fn gdb_deserialize(&mut self, bytes: &[u8]) -> Result<(), ()> {
        if bytes.len() != REG_BYTES && bytes.len() != REG_BYTES_STAGE1 {
            return Err(());
        }
        let (gprs, rest) = bytes.split_at(16 * 8);
        for (r, chunk) in self.r.iter_mut().zip(gprs.as_chunks::<8>().0) {
            *r = u64::from_le_bytes(*chunk);
        }
        let (words, []) = rest.as_chunks::<4>() else {
            return Err(());
        };
        let words: Vec<u32> = words.iter().map(|w| u32::from_le_bytes(*w)).collect();
        let (&[sp, pc, flags, int_mask], stage1) = words.split_at(4) else {
            return Err(());
        };
        (self.sp, self.pc, self.flags, self.int_mask) = (sp, pc, flags, int_mask);
        self.stage1 = !stage1.is_empty();
        if let &[sp_alt, mode] = stage1 {
            (self.sp_alt, self.mode) = (sp_alt, mode);
        }
        Ok(())
    }
}

impl KlaussRegs {
    /// The value of `reg` (a 32-bit register zero-extended).
    pub(crate) const fn get(&self, reg: KlaussRegId) -> u64 {
        match reg {
            KlaussRegId::Gpr(n) => self.r[n as usize],
            KlaussRegId::Sp => self.sp as u64,
            KlaussRegId::Pc => self.pc as u64,
            KlaussRegId::Flags => self.flags as u64,
            KlaussRegId::IntMask => self.int_mask as u64,
            KlaussRegId::SpAlt => self.sp_alt as u64,
            KlaussRegId::Mode => self.mode as u64,
        }
    }

    /// Set `reg` to `value` (a 32-bit register takes the low 32 bits).
    pub(crate) const fn set(&mut self, reg: KlaussRegId, value: u64) {
        match reg {
            KlaussRegId::Gpr(n) => self.r[n as usize] = value,
            KlaussRegId::Sp => self.sp = value as u32,
            KlaussRegId::Pc => self.pc = value as u32,
            KlaussRegId::Flags => self.flags = value as u32,
            KlaussRegId::IntMask => self.int_mask = value as u32,
            KlaussRegId::SpAlt => self.sp_alt = value as u32,
            KlaussRegId::Mode => self.mode = value as u32,
        }
    }
}

/// A register addressed by its `regnum` (single-register `p`/`P` packets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KlaussRegId {
    /// R0..R15.
    Gpr(u8),
    /// Stack pointer.
    Sp,
    /// Program counter.
    Pc,
    /// Z/S/C/V flags.
    Flags,
    /// Interrupt mask.
    IntMask,
    /// MMU stage 1: the inactive stack pointer (`SP_ALT`).
    SpAlt,
    /// MMU stage 1: [`MODE_UM`] | [`MODE_DI`].
    Mode,
}

impl KlaussRegId {
    /// The number of registers (`regnum` 0 to 21).
    #[cfg_attr(
        not(any(test, feature = "board")),
        expect(dead_code, reason = "only the board target lists every register")
    )]
    pub(crate) const COUNT: usize = 22;

    /// The register with GDB `regnum` `n`, which is also its index in the
    /// board debug module's `REG_CMD`.
    pub(crate) const fn from_regnum(n: usize) -> Option<Self> {
        Some(match n {
            0..=15 => Self::Gpr(n as u8),
            16 => Self::Sp,
            17 => Self::Pc,
            18 => Self::Flags,
            19 => Self::IntMask,
            20 => Self::SpAlt,
            21 => Self::Mode,
            _ => return None,
        })
    }

    /// Every register, in `regnum` order.
    #[cfg_attr(
        not(any(test, feature = "board")),
        expect(dead_code, reason = "only the board target lists every register")
    )]
    pub(crate) fn all() -> impl Iterator<Item = Self> {
        (0..Self::COUNT).filter_map(Self::from_regnum)
    }

    /// The register's GDB `regnum` (and debug module `REG_CMD` index).
    #[cfg_attr(
        not(any(test, feature = "board")),
        expect(dead_code, reason = "only the board target addresses registers by number")
    )]
    pub(crate) const fn regnum(self) -> usize {
        match self {
            Self::Gpr(n) => n as usize,
            Self::Sp => 16,
            Self::Pc => 17,
            Self::Flags => 18,
            Self::IntMask => 19,
            Self::SpAlt => 20,
            Self::Mode => 21,
        }
    }

    /// Whether this is an MMU stage 1 register (20 or 21), which a version 1
    /// debug module does not have.
    #[cfg_attr(
        not(any(test, feature = "board")),
        expect(dead_code, reason = "only the board target has cores without them")
    )]
    pub(crate) const fn is_stage1(self) -> bool {
        matches!(self, Self::SpAlt | Self::Mode)
    }

    /// The register's size in bytes.
    pub(crate) const fn size(self) -> usize {
        match self {
            Self::Gpr(_) => 8,
            Self::Sp | Self::Pc | Self::Flags | Self::IntMask | Self::SpAlt | Self::Mode => 4,
        }
    }

    /// Write `value` into `buf` as this register's little-endian bytes (a
    /// `p` packet's reply), truncated to `buf`.  Returns the bytes written.
    pub(crate) fn encode(self, value: u64, buf: &mut [u8]) -> usize {
        let n = self.size().min(buf.len());
        buf[..n].copy_from_slice(&value.to_le_bytes()[..n]);
        n
    }

    /// The value of a `P` packet's bytes for this register, or `None` unless
    /// there are exactly [`size`](Self::size) of them.
    pub(crate) fn decode(self, bytes: &[u8]) -> Option<u64> {
        if bytes.len() != self.size() {
            return None;
        }
        let mut b = [0_u8; 8];
        b[..bytes.len()].copy_from_slice(bytes);
        Some(u64::from_le_bytes(b))
    }
}

impl RegId for KlaussRegId {
    fn from_raw_id(id: usize) -> Option<(Self, Option<NonZeroUsize>)> {
        let reg = Self::from_regnum(id)?;
        Some((reg, NonZeroUsize::new(reg.size())))
    }
}

/// `gdbstub` architecture description for `KlaussCPU`.
pub(crate) enum KlaussCpuArch {}

impl Arch for KlaussCpuArch {
    type Usize = u32;
    type Registers = KlaussRegs;
    type BreakpointKind = usize;
    type RegId = KlaussRegId;

    fn target_description_xml() -> Option<&'static str> {
        Some(TARGET_XML)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_packet_round_trips() {
        let mut regs = KlaussRegs::default();
        for (i, r) in regs.r.iter_mut().enumerate() {
            *r = 0x0101_0101_0101_0101 * i as u64;
        }
        regs.sp = 0x07FF_FF20;
        regs.pc = 0x0000_02C0;
        regs.flags = 0b0110;
        let mut bytes = Vec::new();
        regs.gdb_serialize(|b| bytes.push(b.unwrap()));
        assert_eq!(bytes.len(), REG_BYTES);
        assert_eq!(&bytes[16 * 8 + 4..16 * 8 + 8], &0x2C0_u32.to_le_bytes(), "pc follows sp");
        let mut back = KlaussRegs::default();
        back.gdb_deserialize(&bytes).unwrap();
        assert_eq!(back, regs);
        assert!(back.gdb_deserialize(&bytes[1..]).is_err());
        // With the MMU stage 1 registers: two more words, after int_mask.
        regs.stage1 = true;
        regs.sp_alt = 0x0700_0000;
        regs.mode = MODE_UM | MODE_DI;
        let mut bytes = Vec::new();
        regs.gdb_serialize(|b| bytes.push(b.unwrap()));
        assert_eq!(bytes.len(), REG_BYTES_STAGE1);
        assert_eq!(&bytes[REG_BYTES..], &[0, 0, 0, 7, 3, 0, 0, 0]);
        let mut back = KlaussRegs::default();
        back.gdb_deserialize(&bytes).unwrap();
        assert_eq!(back, regs);
    }

    #[test]
    fn target_xml_reads_in_pieces() {
        let mut buf = [0_u8; 4096];
        let n = target_xml_range(true, 0, 4096, &mut buf);
        assert_eq!(&buf[..n], TARGET_XML.as_bytes());
        let n = target_xml_range(false, 10, 4096, &mut buf);
        assert_eq!(&buf[..n], &TARGET_XML_V1.as_bytes()[10..]);
        assert_eq!(target_xml_range(true, 0, 5, &mut buf), 5);
        assert_eq!(target_xml_range(true, 1 << 40, 5, &mut buf), 0, "past the end");
    }

    #[test]
    fn register_ids_number_and_size_every_register() {
        let all: Vec<KlaussRegId> = KlaussRegId::all().collect();
        assert_eq!(all.len(), KlaussRegId::COUNT);
        for (n, reg) in all.iter().enumerate() {
            assert_eq!(reg.regnum(), n);
            assert_eq!(
                KlaussRegId::from_raw_id(n).map(|r| (r.0, r.1.map(NonZeroUsize::get))),
                Some((*reg, Some(reg.size())))
            );
        }
        assert_eq!(KlaussRegId::Gpr(3).size(), 8);
        assert_eq!(KlaussRegId::IntMask.size(), 4);
        assert_eq!(KlaussRegId::from_regnum(KlaussRegId::COUNT), None);
    }

    #[test]
    fn registers_get_and_set_by_id() {
        let mut regs = KlaussRegs::default();
        for reg in KlaussRegId::all() {
            regs.set(reg, 0x1_0000_0000 + reg.regnum() as u64);
        }
        assert_eq!(regs.r[5], 0x1_0000_0005, "GPRs keep 64 bits");
        assert_eq!((regs.sp, regs.pc, regs.flags, regs.int_mask), (16, 17, 18, 19), "the rest keep 32");
        for reg in KlaussRegId::all() {
            let want = if reg.size() == 8 { 0x1_0000_0000 } else { 0 } + reg.regnum() as u64;
            assert_eq!(regs.get(reg), want, "{reg:?}");
        }
    }

    #[test]
    fn single_register_bytes_round_trip() {
        let mut buf = [0xEE_u8; 8];
        assert_eq!(KlaussRegId::Pc.encode(0x1122_3344_5566_7788, &mut buf), 4);
        assert_eq!(buf, [0x88, 0x77, 0x66, 0x55, 0xEE, 0xEE, 0xEE, 0xEE]);
        let mut short = [0_u8; 2];
        assert_eq!(KlaussRegId::Gpr(0).encode(0x0102_0304, &mut short), 2);
        assert_eq!(short, [0x04, 0x03]);
        assert_eq!(KlaussRegId::Pc.decode(&[1, 2, 3, 4]), Some(0x0403_0201));
        assert_eq!(KlaussRegId::Gpr(1).decode(&[1, 0, 0, 0, 0, 0, 0, 2]), Some(0x0200_0000_0000_0001));
        assert_eq!(KlaussRegId::Pc.decode(&[1, 2, 3, 4, 5, 6, 7, 8]), None, "wrong size");
        assert_eq!(KlaussRegId::Gpr(1).decode(&[1, 2, 3, 4]), None, "wrong size");
    }

    #[test]
    fn register_ids_match_target_xml() {
        assert_eq!(KlaussRegId::from_raw_id(15).map(|r| r.0), Some(KlaussRegId::Gpr(15)));
        assert_eq!(KlaussRegId::from_raw_id(17).map(|r| r.0), Some(KlaussRegId::Pc));
        assert_eq!(KlaussRegId::from_raw_id(21).map(|r| r.0), Some(KlaussRegId::Mode));
        assert_eq!(KlaussRegId::from_raw_id(22), None);
        for n in 0..22 {
            assert!(TARGET_XML.contains(&format!("regnum=\"{n}\"")), "regnum {n} described");
            assert_eq!(TARGET_XML_V1.contains(&format!("regnum=\"{n}\"")), n < 20, "regnum {n} in v1");
        }
        assert!(TARGET_XML.ends_with("</feature>\n</target>") && TARGET_XML_V1.ends_with("</feature>\n</target>"));
        assert!(TARGET_XML.contains(r#"<field name="UM" start="0" end="0"/>"#));
        assert!(TARGET_XML.contains(r#"<field name="DI" start="1" end="1"/>"#));
        assert!(TARGET_XML.contains(r#"name="pc" bitsize="32" regnum="17""#));
        assert!(TARGET_XML.contains(r#"dwarf_regnum="17" generic="pc""#));
    }
}
