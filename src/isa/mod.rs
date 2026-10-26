//! The Klauss instruction set: the opcode table, encoding source operands
//! into instruction words, and disassembling them again.
//!
//! [`Opcode`](crate::isa::Opcode) and [`Operand`](crate::isa::Operand)
//! describe one instruction and how its operands map onto the word, and
//! [`Isa`](crate::isa::Isa) looks opcodes up by name;
//! [`table`](crate::isa::table) holds the built-in ISA-encoding-v2 table,
//! [`encode`](crate::isa::encode) packs a source line's operands into words
//! and [`disasm`](crate::isa::disasm) turns words back into text.

pub(crate) mod disasm;
pub(crate) mod encode;
pub(crate) mod table;

use serde::Serialize;
use std::collections::HashMap;
use table::V2_OPCODES;

/// The register names, by register number (the value of a 4-bit register field).
pub(crate) const REG_NAMES: [&str; 16] = ["A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P"];

/// The number of the register named `name`, in any case, or `None` when
/// `name` is not a register.
pub(crate) fn reg_index(name: &str) -> Option<u32> {
    let name = name.to_uppercase();
    REG_NAMES.iter().zip(0..).find_map(|(&reg, index)| (reg == name).then_some(index))
}

/// One source-operand slot of a v2 instruction, describing how the operand
/// token maps onto the instruction word (`doc/ISA_ENCODING_V2.md` §1). The uniform
/// v2 layout is `word0 = template | rd<<8 | rs1<<4 | rs2` plus `N<<15` for the
/// embedded shift/bit count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) enum Operand {
    /// Destination register → word0[11:8].
    Rd,
    /// First source / base register → word0[7:4].
    Rs1,
    /// Second source / offset register → word0[3:0].
    Rs2,
    /// In-place register: one source token drives BOTH rd[11:8] and rs1[7:4].
    /// v1's in-place forms (`ADDV A 5`, `NEGR A`, `SHLV A 4`) become v2's
    /// 3-operand encodings with `rd == rs1`.
    RdRs1,
    /// Embedded 6-bit shift/bit count or position → word0[20:15].
    Count,
    /// 32-bit immediate word appended at PC+4.
    Imm,
    /// 64-bit immediate appended as lo32@PC+4, hi32@PC+8.
    Imm64,
    /// ISA v3 short form: literal 8-bit immediate → word0[19:12]
    /// (-128..=255; the instruction's SGN bit picks the extension).
    Imm8,
    /// ISA v3 short load/store: literal BYTE offset, a multiple of
    /// `1 << shift` whose scaled value fits simm8 → word0[19:12].
    Off8(u8),
    /// ISA v3 `ENTER`: literal frame size in 8-byte units → word0[21:0].
    Frame22,
    /// ISA v3 fused branch: literal simm4 compare immediate → word0[3:0].
    Simm4,
    /// ISA v3 short branch target (label or address) → simm18 word
    /// displacement from this instruction, word0[17:0]. Filled in pass 2.
    Rel18,
    /// ISA v3 fused branch target (label or address) → simm13 word
    /// displacement from this instruction, word0[20:8]. Filled in pass 2.
    Rel13,
}

impl Operand {
    /// The bits of word 0 this operand's field occupies; 0 for the immediates
    /// appended after it.
    pub(crate) const fn mask(self) -> u32 {
        match self {
            Self::Rd => 0xF << 8,
            Self::Rs1 => 0xF << 4,
            Self::Rs2 | Self::Simm4 => 0xF,
            Self::RdRs1 => 0xFF << 4,
            Self::Count => 0x3F << 15,
            Self::Imm8 | Self::Off8(_) => 0xFF << 12,
            Self::Frame22 => 0x3F_FFFF,
            Self::Rel18 => 0x3_FFFF,
            Self::Rel13 => 0x1FFF << 8,
            Self::Imm | Self::Imm64 => 0,
        }
    }

    /// Whether the operand is a field of word 0, taking one source token,
    /// rather than an immediate appended after it.
    pub(crate) const fn is_word0(self) -> bool {
        !matches!(self, Self::Imm | Self::Imm64)
    }

    /// The number of 32-bit words appended after word 0 for this operand.
    pub(crate) const fn imm_words(self) -> u32 {
        match self {
            Self::Imm => 1,
            Self::Imm64 => 2,
            _ => 0,
        }
    }
}

/// One instruction of the table: its mnemonic, its word-0 template and how
/// its source operands map onto the instruction words.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Opcode {
    /// Mnemonic, in upper case.
    pub name: &'static str,
    /// Instruction word 0 with every operand field clear.
    pub template: u32,
    /// Ordered source-operand layout (empty for an opcode without operands).
    pub ops: &'static [Operand],
    /// Heading the docs export groups the opcode under.
    pub section: &'static str,
    /// Description for the docs export.
    pub doc: &'static str,
}

impl Opcode {
    /// The number of source tokens that fill fields of word 0 (registers,
    /// counts, short literals and branch targets).
    pub(crate) fn reg_tokens(&self) -> u32 {
        self.ops.iter().filter(|op| op.is_word0()).count() as u32
    }

    /// The number of 32-bit immediate words appended after word 0
    /// (imm32 → 1, imm64 → 2).
    pub(crate) fn imm_words(&self) -> u32 {
        self.ops.iter().map(|op| op.imm_words()).sum()
    }
}

/// An instruction set: its opcode table, indexed by mnemonic.
pub(crate) struct Isa {
    /// The table, in its order (the disassembler matches the first fit).
    opcodes: &'static [Opcode],
    /// Each opcode by its upper-case name.
    by_name: HashMap<&'static str, &'static Opcode>,
}

impl Isa {
    /// Index `opcodes` by name; a repeated name keeps its first entry.
    pub(crate) fn new(opcodes: &'static [Opcode]) -> Self {
        let mut by_name = HashMap::with_capacity(opcodes.len());
        for opcode in opcodes {
            by_name.entry(opcode.name).or_insert(opcode);
        }
        Self { opcodes, by_name }
    }

    /// The built-in ISA-encoding-v2 instruction set ([`V2_OPCODES`]).
    pub(crate) fn v2() -> Self {
        Self::new(V2_OPCODES)
    }

    /// The opcode table, in table order.
    pub(crate) const fn opcodes(&self) -> &'static [Opcode] {
        self.opcodes
    }

    /// The opcode named `mnemonic`, in any case.
    pub(crate) fn get(&self, mnemonic: &str) -> Option<&'static Opcode> {
        self.by_name.get(mnemonic.to_uppercase().as_str()).copied()
    }

    /// The opcode named by the first word of `line`, if it is one.
    pub(crate) fn find(&self, line: &str) -> Option<&'static Opcode> {
        self.get(line.split_whitespace().next()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Each operand names the word-0 field it fills; immediates fill none
    fn test_operand_layout() {
        assert_eq!(Operand::Rd.mask(), 0x0000_0F00);
        assert_eq!(Operand::Rs1.mask(), 0x0000_00F0);
        assert_eq!(Operand::RdRs1.mask(), Operand::Rd.mask() | Operand::Rs1.mask());
        assert_eq!(Operand::Count.mask(), 0x001F_8000);
        assert_eq!(Operand::Off8(3).mask(), Operand::Imm8.mask());
        assert_eq!(Operand::Rel13.mask(), 0x001F_FF00);
        for op in [Operand::Imm, Operand::Imm64] {
            assert_eq!((op.mask(), op.is_word0()), (0, false), "{op:?}");
        }
        assert_eq!((Operand::Imm.imm_words(), Operand::Imm64.imm_words(), Operand::Rd.imm_words()), (1, 2, 0));
        assert!(Operand::Rel18.is_word0());
    }

    #[test]
    // Every opcode's operand fields are disjoint and clear in its template,
    // and its immediates come after all of its word-0 operands.
    fn test_table_operand_fields_are_disjoint() {
        for opcode in V2_OPCODES {
            let mut fields = 0;
            for op in opcode.ops {
                assert_eq!(fields & op.mask(), 0, "{} {op:?}", opcode.name);
                fields |= op.mask();
            }
            assert_eq!(opcode.template & fields, 0, "{}", opcode.name);
            let first_imm = opcode.ops.iter().position(|op| !op.is_word0()).unwrap_or(opcode.ops.len());
            assert!(opcode.ops[first_imm..].iter().all(|op| !op.is_word0()), "{}", opcode.name);
        }
    }

    #[test]
    // Register names map to their numbers in any case; anything else is None
    fn test_reg_index() {
        assert_eq!(reg_index("A"), Some(0));
        assert_eq!(reg_index("b"), Some(1));
        assert_eq!(reg_index("P"), Some(15));
        for bad in ["Z", "", "AB", "0"] {
            assert_eq!(reg_index(bad), None, "{bad}");
        }
        for (name, index) in REG_NAMES.iter().zip(0..) {
            assert_eq!(reg_index(name), Some(index));
        }
    }

    #[test]
    // Every mnemonic in the table is upper case and names one opcode
    fn test_table_names_are_unique() {
        let isa = Isa::v2();
        assert_eq!(isa.by_name.len(), V2_OPCODES.len());
        for opcode in V2_OPCODES {
            assert_eq!(opcode.name, opcode.name.to_uppercase());
            assert!(std::ptr::eq(isa.get(opcode.name).unwrap(), opcode), "{}", opcode.name);
        }
    }

    #[test]
    // The register/count operands of an opcode are counted; unknown names are None
    fn test_num_registers() {
        let isa = Isa::v2();
        let regs = |line| isa.find(line).map(Opcode::reg_tokens);
        assert_eq!(regs("PUSH"), Some(1));
        assert_eq!(regs("ADDR A B C"), Some(3));
        assert_eq!(regs("RET"), Some(0));
        assert_eq!(regs("PULL"), None);
        assert_eq!(regs(""), None);
    }

    /// The number of immediate words of the opcode `line` starts with.
    fn num_arguments(line: &str) -> Option<u32> {
        Isa::v2().find(line).map(Opcode::imm_words)
    }

    #[test]
    // An opcode without immediates has no argument words
    fn test_num_arguments1() {
        assert_eq!(num_arguments("PUSH A"), Some(0));
    }

    #[test]
    // A 32-bit immediate is one argument word, whatever the case of the mnemonic
    fn test_num_arguments2() {
        assert_eq!(num_arguments("SETR A 5"), Some(1));
        assert_eq!(num_arguments("setr A 5"), Some(1));
    }

    #[test]
    // A 64-bit immediate is two argument words
    fn test_num_arguments3() {
        assert_eq!(num_arguments("SETR64 A 0x1_0000_0000"), Some(2));
        assert_eq!(num_arguments("PUSHV64 1 2"), Some(2));
    }

    #[test]
    // None is returned if the opcode is not found
    fn test_num_arguments4() {
        assert_eq!(num_arguments("PUSH2 A B"), None);
    }

    #[test]
    // None is returned if the line is blank
    fn test_num_arguments5() {
        assert_eq!(num_arguments(""), None);
        assert_eq!(num_arguments("   "), None);
    }

    #[test]
    // An opcode is found by the first word of a line, whatever its case
    fn test_find_opcode1() {
        let isa = Isa::v2();
        assert_eq!(isa.find("PUSH A").map(|o| o.template), Some(0x6400_0000));
        assert_eq!(isa.find("halt // stop").map(|o| o.template), Some(0x6C01_0000));
        assert_eq!(isa.get("Leave.").map(|o| o.name), None);
        assert_eq!(isa.get("ldidx8.s").map(|o| o.name), Some("LDIDX8.S"));
    }

    #[test]
    // Test that None is returned if the opcode is not found
    fn test_find_opcode2() {
        assert_eq!(Isa::v2().find("PUSH2"), None);
        assert_eq!(Isa::new(&[]).find("PUSH"), None);
    }
}
