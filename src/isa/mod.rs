//! The Klauss instruction set: the opcode table, encoding source operands
//! into instruction words, and disassembling them again.
//!
//! [`Opcode`](crate::isa::Opcode) and [`Operand`](crate::isa::Operand)
//! describe one instruction and how its operands map onto the word;
//! [`table`](crate::isa::table) holds the built-in ISA-encoding-v2 table,
//! [`encode`](crate::isa::encode) packs a source line's operands into words
//! and [`disasm`](crate::isa::disasm) turns words back into text.

pub(crate) mod disasm;
pub(crate) mod encode;
pub(crate) mod table;

use serde::{Deserialize, Serialize};

/// One source-operand slot of a v2 instruction, describing how the operand
/// token maps onto the instruction word (`doc/ISA_ENCODING_V2.md` §1). The uniform
/// v2 layout is `word0 = template | rd<<8 | rs1<<4 | rs2` plus `N<<15` for the
/// embedded shift/bit count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Struct for opcode.
pub(crate) struct Opcode {
    /// Comment from opcode definition file.
    pub comment: String,
    /// Hexadecimal opcode template (word 0, register/count fields = 0).
    pub hex_code: String,
    /// Number of register/count source operands (tokens consumed into word 0).
    pub registers: u32,
    /// Section name from opcode definition file.
    pub section: String,
    /// Text name of opcode.
    pub text_name: String,
    /// Number of appended immediate words (imm32 → 1, imm64 → 2).
    pub variables: u32,
    /// Ordered source-operand layout (empty for an opcode without operands).
    #[serde(default)]
    pub ops: Vec<Operand>,
}

/// Find the opcode whose `text_name` matches the first word of `line`.
fn return_opcode_struct<'a>(line: &str, opcodes: &'a [Opcode]) -> Option<&'a Opcode> {
    let first = line.split_whitespace().next().unwrap_or("").to_uppercase();
    opcodes.iter().find(|o| o.text_name == first)
}

/// Returns number of args for opcode.
///
/// From opcode name, option of number of arguments for opcode, or None.
pub(crate) fn num_arguments(opcodes: &[Opcode], line: &str) -> Option<u32> {
    for opcode in opcodes {
        let mut words = line.split_whitespace();
        let first_word = words.next().unwrap_or("");
        if first_word.to_uppercase() == opcode.text_name {
            return Some(opcode.variables);
        }
    }
    None
}

/// Returns number of registers for opcode.
///
/// From opcode name, option of number of registers for opcode, or None.
fn num_registers(opcodes: &[Opcode], line: &str) -> Option<u32> {
    for opcode in opcodes {
        let mut words = line.split_whitespace();
        let first_word = words.next().unwrap_or("");
        if first_word.is_empty() {
            return None;
        }
        if first_word == opcode.text_name {
            return Some(opcode.registers);
        }
    }
    None
}

/// Returns hex opcode from name.
///
/// Checks if first word is opcode and if so returns opcode hex value.
pub(crate) fn return_opcode(line: &str, opcodes: &[Opcode]) -> Option<String> {
    for opcode in opcodes {
        let mut words = line.split_whitespace();
        let first_word = words.next().unwrap_or("");
        if first_word.to_uppercase() == opcode.text_name {
            return Some(opcode.hex_code.to_uppercase());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isa::table::v2_opcodes;

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
        for opcode in &v2_opcodes() {
            let template = u32::from_str_radix(&opcode.hex_code, 16).unwrap();
            let mut fields = 0;
            for op in &opcode.ops {
                assert_eq!(fields & op.mask(), 0, "{} {op:?}", opcode.text_name);
                fields |= op.mask();
            }
            assert_eq!(template & fields, 0, "{}", opcode.text_name);
            let first_imm = opcode.ops.iter().position(|op| !op.is_word0()).unwrap_or(opcode.ops.len());
            assert!(opcode.ops[first_imm..].iter().all(|op| !op.is_word0()), "{}", opcode.text_name);
        }
    }

    #[test]
    // The register/count operands of an opcode are counted; unknown names are None
    fn test_num_registers() {
        let opcodes = v2_opcodes();
        assert_eq!(num_registers(&opcodes, "PUSH"), Some(1));
        assert_eq!(num_registers(&opcodes, "ADDR A B C"), Some(3));
        assert_eq!(num_registers(&opcodes, "RET"), Some(0));
        assert_eq!(num_registers(&opcodes, "PULL"), None);
        assert_eq!(num_registers(&opcodes, ""), None);
    }

    #[test]
    // An opcode without immediates has no argument words
    fn test_num_arguments1() {
        assert_eq!(num_arguments(&v2_opcodes(), "PUSH A"), Some(0));
    }

    #[test]
    // A 32-bit immediate is one argument word, whatever the case of the mnemonic
    fn test_num_arguments2() {
        let opcodes = v2_opcodes();
        assert_eq!(num_arguments(&opcodes, "SETR A 5"), Some(1));
        assert_eq!(num_arguments(&opcodes, "setr A 5"), Some(1));
    }

    #[test]
    // A 64-bit immediate is two argument words
    fn test_num_arguments3() {
        let opcodes = v2_opcodes();
        assert_eq!(num_arguments(&opcodes, "SETR64 A 0x1_0000_0000"), Some(2));
        assert_eq!(num_arguments(&opcodes, "PUSHV64 1 2"), Some(2));
    }

    #[test]
    // None is returned if the opcode is not found
    fn test_num_arguments4() {
        assert_eq!(num_arguments(&v2_opcodes(), "PUSH2 A B"), None);
    }

    #[test]
    // None is returned if the line is blank
    fn test_num_arguments5() {
        let opcodes = v2_opcodes();
        assert_eq!(num_arguments(&opcodes, ""), None);
        assert_eq!(num_arguments(&opcodes, "   "), None);
    }

    #[test]
    // The opcode's template is returned as 8 hex digits, whatever the case of the mnemonic
    fn test_return_opcode1() {
        let opcodes = v2_opcodes();
        assert_eq!(return_opcode("PUSH A", &opcodes), Some(String::from("64000000")));
        assert_eq!(return_opcode("halt", &opcodes), Some(String::from("6C010000")));
    }

    #[test]
    // Test that None is returned if the opcode is not found
    fn test_return_opcode2() {
        assert_eq!(return_opcode("PUSH2", &v2_opcodes()), None);
    }
}
