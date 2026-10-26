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
/// The legacy `.vh` opcode-definition file format.
mod vh;

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
    /// Ordered source-operand layout (empty for legacy `.vh`-parsed opcodes).
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

    #[test]
    // Test that the correct number of registers is returned
    fn test_num_registers1() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 1,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, Some(1));
    }

    #[test]
    // Test that the None is returned if the opcode is not found
    fn test_num_registers2() {
        let input = String::from("PULL");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 1,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, None);
    }
    #[test]
    // Test that the correct number of arguments is returned
    fn test_num_arguments1() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 1,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, Some(1));
    }
    #[test]
    // Test that the correct number of arguments is returned
    fn test_num_arguments2() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 2,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, Some(2));
    }

    #[test]
    // Test that the correct number of arguments is returned 2 variable 2 registers
    fn test_num_arguments3() {
        let input = String::from("PUSH ddd yyy");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 2,
            registers: 2,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, Some(2));
    }

    #[test]
    // Test that None is returned if the opcode is not found
    fn test_num_arguments4() {
        let input = String::from("PUSH2");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 2,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, None);
    }

    #[test]
    // Test that None is returned if the opcode is blank
    fn test_num_arguments5() {
        let input = String::default();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 2,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, None);
    }

    #[test]
    // Test that the correct opcode is returned
    fn test_return_opcode1() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 2,
            section: String::default(),
        });
        let output = return_opcode(&input, opcodes);
        assert_eq!(output, Some(String::from("1234")));
    }

    #[test]
    // Test that None is returned if the opcode is not found
    fn test_return_opcode2() {
        let input = String::from("PUSH2");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 2,
            section: String::default(),
        });
        let output = return_opcode(&input, opcodes);
        assert_eq!(output, None);
    }

    // -------------------------------------------------------------------------
    // 32-bit format (32'hXXXX_XXXX) tests
    // -------------------------------------------------------------------------
}
