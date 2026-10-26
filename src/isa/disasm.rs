//! Disassembling instruction words back into assembler text.

use super::{Opcode, Operand};
use crate::asm::line::{LineType, Pass2};
use std::fmt::Write as _;

/// Register nibble value to register name (inverse of `map_reg_to_hex`).
fn hex_nibble_to_reg(nibble: u32) -> &'static str {
    match nibble & 0xF {
        0x0 => "A",
        0x1 => "B",
        0x2 => "C",
        0x3 => "D",
        0x4 => "E",
        0x5 => "F",
        0x6 => "G",
        0x7 => "H",
        0x8 => "I",
        0x9 => "J",
        0xA => "K",
        0xB => "L",
        0xC => "M",
        0xD => "N",
        0xE => "O",
        _ => "P",
    }
}

/// Disassemble a 32-bit instruction word.
///
/// Searches `opcodes` for the first entry whose template matches `word` in
/// every bit its operands do not occupy.
/// Returns `(mnemonic_with_operands, variables_count)` or `None` if no match.
/// Operands are shown in source order.
pub(crate) fn disassemble_word(word: u32, opcodes: &[Opcode]) -> Option<(String, u32)> {
    for opcode in opcodes {
        // Build the fixed-bit mask from the operand layout, then match.
        let Ok(template) = u32::from_str_radix(&opcode.hex_code, 16) else {
            continue;
        };
        let mut mask = 0xFFFF_FFFF_u32;
        for op in &opcode.ops {
            match op {
                Operand::Rd => mask &= !(0xF << 8),
                Operand::Rs1 => mask &= !(0xF << 4),
                Operand::Rs2 | Operand::Simm4 => mask &= !0xF,
                Operand::RdRs1 => mask &= !((0xF << 8) | (0xF << 4)),
                Operand::Count => mask &= !(0x3F << 15),
                Operand::Imm8 | Operand::Off8(_) => mask &= !(0xFF << 12),
                Operand::Frame22 => mask &= !0x3F_FFFF,
                Operand::Rel18 => mask &= !0x3_FFFF,
                Operand::Rel13 => mask &= !(0x1FFF << 8),
                Operand::Imm | Operand::Imm64 => {}
            }
        }
        if (word & mask) == (template & mask) {
            let mut text = opcode.text_name.clone();
            for op in &opcode.ops {
                match op {
                    Operand::Rd | Operand::RdRs1 => {
                        text.push(' ');
                        text.push_str(hex_nibble_to_reg((word >> 8) & 0xF));
                    }
                    Operand::Rs1 => {
                        text.push(' ');
                        text.push_str(hex_nibble_to_reg((word >> 4) & 0xF));
                    }
                    Operand::Rs2 => {
                        text.push(' ');
                        text.push_str(hex_nibble_to_reg(word & 0xF));
                    }
                    Operand::Count => {
                        text.push(' ');
                        text.push_str(&((word >> 15) & 0x3F).to_string());
                    }
                    Operand::Imm8 => {
                        text.push(' ');
                        text.push_str(&((word >> 12) & 0xFF).to_string());
                    }
                    Operand::Off8(shift) => {
                        let v = i64::from((((word >> 12) & 0xFF) as u8) as i8) << shift;
                        text.push(' ');
                        text.push_str(&v.to_string());
                    }
                    Operand::Frame22 => {
                        text.push(' ');
                        text.push_str(&(word & 0x3F_FFFF).to_string());
                    }
                    Operand::Simm4 => {
                        text.push(' ');
                        text.push_str(&((((word & 0xF) << 28) as i32) >> 28).to_string());
                    }
                    Operand::Rel18 => {
                        let _ = write!(text, " PC{:+}", (((word << 14) as i32) >> 14) * 4);
                    }
                    Operand::Rel13 => {
                        let _ = write!(text, " PC{:+}", (((word << 11) as i32) >> 19) * 4);
                    }
                    Operand::Imm | Operand::Imm64 => {}
                }
            }
            return Some((text, opcode.variables));
        }
    }
    None
}

/// Disassemble a flat byte slice into a `Vec<Pass2>` for use with `write_code_output_file`.
///
/// Each 4-byte chunk is read as a little-endian 32-bit word (matching the `KlaussCPU` LLVM ELF
/// byte order) and matched against `opcodes`.  When a match is found, register operands are
/// extracted and any argument words that follow (determined by `opcode.variables`) are consumed
/// and appended to the same entry's `opcode` hex string so `write_code_output_file` formats them
/// correctly.  Unrecognised words produce a `???` fallback line.  When the binary ends before
/// all of an instruction's argument words, the words that are there are shown once, on that
/// instruction's line, marked `(truncated)`.
///
/// `base_addr` is the board byte address of the first byte in `binary` (normally
/// [`CODE_BASE`](crate::layout::CODE_BASE) = 0x20).
pub(crate) fn disassemble_flat_to_pass2(binary: &[u8], base_addr: u32, opcodes: &[Opcode]) -> Vec<Pass2> {
    let mut result: Vec<Pass2> = Vec::new();
    let mut i: usize = 0;

    while i + 4 <= binary.len() {
        let word = u32::from_le_bytes([binary[i], binary[i + 1], binary[i + 2], binary[i + 3]]);
        let pc = base_addr.saturating_add(i as u32);

        let (base_text, vars) = disassemble_word(word, opcodes).unwrap_or_else(|| (format!("??? (0x{word:08X})"), 0_u32));

        let mut opcode_hex = format!("{word:08X}");
        let mut display_text = base_text;

        // Consume argument words that immediately follow the instruction word.
        // vars == 1 → one 32-bit immediate at PC+4.
        // vars == 2 → two 32-bit words at PC+4 (lo32) and PC+8 (hi32), forming a 64-bit value.
        let arg_word_count = vars.min(2) as usize;
        let args: Vec<u32> = (0..arg_word_count)
            .map_while(|n| {
                let off = i + 4 + n * 4;
                binary.get(off..off + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes)
            })
            .collect();
        for av in &args {
            let _ = write!(opcode_hex, "{av:08X}");
        }
        match *args.as_slice() {
            [av] if arg_word_count == 1 => _ = write!(display_text, " 0x{av:X}"),
            [lo, hi] => _ = write!(display_text, " 0x{:X}", (u64::from(hi) << 32) | u64::from(lo)),
            [lo] => _ = write!(display_text, " 0x{lo:08X}"),
            _ => {}
        }
        if args.len() < arg_word_count {
            display_text.push_str(" (truncated)");
        }

        // Consume exactly the words shown, so none is shown twice.
        let words_consumed = 1 + args.len();

        result.push(Pass2 {
            file_name: String::new(),
            input_text_line: display_text,
            line_counter: 0,
            line_type: LineType::Opcode,
            opcode: opcode_hex,
            program_counter: pc,
        });

        i += words_consumed * 4;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isa::table::v2_opcodes;

    /// Little-endian bytes of `words`, as an ELF or flat binary holds them.
    fn le_bytes(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    // Words are read little-endian; a 64-bit argument is shown as one value
    fn test_disassemble_flat_to_pass2_le_and_64bit() {
        let opcodes = v2_opcodes();
        let pass2 = disassemble_flat_to_pass2(&le_bytes(&[0x8BD0_0000, 5, 0xCBC0_0000, 0x5566_7788, 0x1122_3344]), 0x20, &opcodes);
        assert_eq!(pass2.len(), 2);
        assert_eq!(pass2[0].program_counter, 0x20);
        assert_eq!(pass2[0].opcode, "8BD0000000000005");
        assert!(pass2[0].input_text_line.starts_with("SETR "), "{}", pass2[0].input_text_line);
        assert!(pass2[0].input_text_line.ends_with(" 0x5"), "{}", pass2[0].input_text_line);
        assert_eq!(pass2[1].program_counter, 0x28);
        assert_eq!(pass2[1].opcode, "CBC000005566778811223344");
        assert!(pass2[1].input_text_line.ends_with(" 0x1122334455667788"), "{}", pass2[1].input_text_line);
    }

    #[test]
    // Regression: a 64-bit argument cut off by the end of the binary showed its
    // lo32 word on the instruction's line and then again as the next line.
    fn test_disassemble_flat_to_pass2_truncated_argument() {
        let opcodes = v2_opcodes();
        let pass2 = disassemble_flat_to_pass2(&le_bytes(&[0xCBC0_0000, 0x5566_7788]), 0x20, &opcodes);
        assert_eq!(pass2.len(), 1);
        assert_eq!(pass2[0].opcode, "CBC0000055667788");
        assert!(
            pass2[0].input_text_line.ends_with(" 0x55667788 (truncated)"),
            "{}",
            pass2[0].input_text_line
        );

        let pass2 = disassemble_flat_to_pass2(&le_bytes(&[0x8BD0_0000]), 0x20, &opcodes);
        assert_eq!(pass2.len(), 1);
        assert!(pass2[0].input_text_line.ends_with(" (truncated)"), "{}", pass2[0].input_text_line);
    }
}
