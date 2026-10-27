//! Disassembling instruction words back into assembler text.

use super::{Opcode, Operand, REG_NAMES};
use crate::asm::line::{Emitted, LineType, Pass2};
use std::fmt::Write as _;

/// The name of the register in the low nibble of `field`.
fn reg_name(field: u32) -> &'static str {
    REG_NAMES[(field & 0xF) as usize]
}

/// A disassembler for an opcode table, with each opcode's fixed-bit mask
/// worked out once.
pub(crate) struct Disassembler {
    /// Each opcode in table order with the mask of the bits its operands do
    /// not occupy; the first opcode whose fixed bits match a word wins.
    entries: Vec<(u32, &'static Opcode)>,
}

impl Disassembler {
    /// A disassembler for `opcodes`.
    pub(crate) fn new(opcodes: &'static [Opcode]) -> Self {
        let entries = opcodes
            .iter()
            .map(|opcode| (!opcode.ops.iter().fold(0, |fields, op| fields | op.mask()), opcode))
            .collect();
        Self { entries }
    }

    /// The opcode `word` is an instance of: the first in table order whose
    /// template matches `word` in every bit its operands do not occupy.
    pub(crate) fn opcode(&self, word: u32) -> Option<&'static Opcode> {
        self.entries
            .iter()
            .find(|&&(mask, opcode)| word & mask == opcode.template & mask)
            .map(|&(_, opcode)| opcode)
    }

    /// Disassemble a 32-bit instruction word.
    ///
    /// Returns `(mnemonic_with_operands, variables_count)`, the operands in
    /// source order, or `None` if no opcode matches.
    pub(crate) fn word(&self, word: u32) -> Option<(String, u32)> {
        let opcode = self.opcode(word)?;
        let mut text = opcode.name.to_owned();
        for op in opcode.ops {
            let _ = match op {
                Operand::Rd | Operand::RdRs1 => write!(text, " {}", reg_name(word >> 8)),
                Operand::Rs1 => write!(text, " {}", reg_name(word >> 4)),
                Operand::Rs2 => write!(text, " {}", reg_name(word)),
                Operand::Count => write!(text, " {}", (word >> 15) & 0x3F),
                // As the source would write it, so the line reassembles.
                Operand::Imm8 if opcode.imm8_signed() => write!(text, " {}", ((word >> 12) & 0xFF) as u8 as i8),
                Operand::Imm8 => write!(text, " {}", (word >> 12) & 0xFF),
                Operand::Off8(shift) => write!(text, " {}", i64::from((((word >> 12) & 0xFF) as u8) as i8) << shift),
                Operand::Frame22 => write!(text, " {}", word & 0x3F_FFFF),
                Operand::Simm4 => write!(text, " {}", (((word & 0xF) << 28) as i32) >> 28),
                Operand::Rel18 => write!(text, " PC{:+}", (((word << 14) as i32) >> 14) * 4),
                Operand::Rel13 => write!(text, " PC{:+}", (((word << 11) as i32) >> 19) * 4),
                Operand::Imm | Operand::Imm64 | Operand::Rel32 => Ok(()),
            };
        }
        Some((text, opcode.imm_words()))
    }

    /// Disassemble a flat byte slice into a `Vec<Pass2>` for use with `write_code_output_file`.
    ///
    /// Each 4-byte chunk is read as a little-endian 32-bit word (matching the `KlaussCPU` LLVM ELF
    /// byte order) and matched against the table.  When a match is found, register operands are
    /// extracted and any argument words that follow (determined by [`Opcode::imm_words`]) are consumed
    /// and emitted on the same entry after the instruction word, so `write_code_output_file` shows
    /// them together.  Unrecognised words produce a `???` fallback line.  When the binary ends before
    /// all of an instruction's argument words, the words that are there are shown once, on that
    /// instruction's line, marked `(truncated)`.
    ///
    /// `base_addr` is the board byte address of the first byte in `binary` (normally
    /// [`CODE_BASE`](crate::layout::CODE_BASE) = 0x20).
    pub(crate) fn flat_to_pass2(&self, binary: &[u8], base_addr: u32) -> Vec<Pass2> {
        let mut result: Vec<Pass2> = Vec::new();
        let mut i: usize = 0;

        while i + 4 <= binary.len() {
            let word = u32::from_le_bytes([binary[i], binary[i + 1], binary[i + 2], binary[i + 3]]);
            let pc = base_addr.saturating_add(i as u32);

            let (base_text, vars) = self.word(word).unwrap_or_else(|| (format!("??? (0x{word:08X})"), 0_u32));
            // A 2-word PC-relative target shows as its displacement, as the
            // short branches' does.
            let rel32 = self.opcode(word).is_some_and(|opcode| opcode.ops.contains(&Operand::Rel32));

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
            match *args.as_slice() {
                [av] if rel32 => _ = write!(display_text, " PC{:+}", av as i32),
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
                emitted: Emitted::Code(std::iter::once(word).chain(args).collect()),
                program_counter: pc,
            });

            i += words_consumed * 4;
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::symbols::SymbolTable;
    use crate::isa::encode::encode;
    use crate::isa::table::V2_OPCODES;

    /// Opcodes whose encodings overlap an earlier opcode's, as `(shown,
    /// hidden)`: a word both match disassembles as `shown`, the earlier.
    /// `ROLR1`/`RORR1` are `ROLV`/`RORV` by 1, and `SHLAR` is `SHLR1`.
    const ALIASES: &[(&str, &str)] = &[("ROLV", "ROLR1"), ("RORV", "RORR1"), ("SHLR1", "SHLAR")];

    /// A source token for each operand of `opcode`, as an instruction at
    /// 0x20 would be written, and the text the disassembler shows for it.
    fn sample_operands(opcode: &Opcode) -> (Vec<String>, Vec<String>) {
        let mut source = Vec::new();
        let mut shown = Vec::new();
        for (i, op) in opcode.ops.iter().enumerate() {
            let (token, text) = match op {
                // Distinct registers, so a field swap would show.
                Operand::Rd | Operand::Rs1 | Operand::Rs2 | Operand::RdRs1 => {
                    let reg = REG_NAMES[i + 1].to_owned();
                    (reg.clone(), reg)
                }
                Operand::Count => ("37".to_owned(), "37".to_owned()),
                // Outside the other extension's range, so a wrong one shows.
                Operand::Imm8 if opcode.imm8_signed() => ("-100".to_owned(), "-100".to_owned()),
                Operand::Imm8 => ("200".to_owned(), "200".to_owned()),
                Operand::Off8(shift) => {
                    let offset = (-5_i64 << shift).to_string();
                    (offset.clone(), offset)
                }
                Operand::Frame22 => ("12345".to_owned(), "12345".to_owned()),
                Operand::Simm4 => ("-3".to_owned(), "-3".to_owned()),
                Operand::Rel18 | Operand::Rel13 | Operand::Rel32 => ("0x10".to_owned(), "PC-16".to_owned()),
                Operand::Imm => ("0x1234".to_owned(), "0x1234".to_owned()),
                Operand::Imm64 => ("0x500000006".to_owned(), "0x500000006".to_owned()),
            };
            source.push(token);
            shown.push(text);
        }
        (source, shown)
    }

    #[test]
    // The only opcodes whose encodings overlap are the listed aliases, and in
    // each the opcode shown comes first in the table.
    fn test_table_overlaps_are_the_aliases() {
        let disassembler = Disassembler::new(V2_OPCODES);
        let mut overlaps = Vec::new();
        for (i, &(mask_a, a)) in disassembler.entries.iter().enumerate() {
            for &(mask_b, b) in &disassembler.entries[i + 1..] {
                if (a.template ^ b.template) & mask_a & mask_b == 0 {
                    overlaps.push((a.name, b.name));
                }
            }
        }
        assert_eq!(overlaps, ALIASES);
        // Every encoding of the hidden opcode shows as the alias.
        for &(shown, hidden) in ALIASES {
            let hidden = V2_OPCODES.iter().find(|o| o.name == hidden).unwrap();
            assert_eq!(disassembler.opcode(hidden.template).map(|o| o.name), Some(shown));
        }
    }

    #[test]
    // Every opcode in the table encodes and disassembles back to the line it
    // came from (an alias shows as the opcode it is an alias of).
    fn test_encode_disassemble_round_trip() {
        let disassembler = Disassembler::new(V2_OPCODES);
        let symbols = SymbolTable::default();
        for opcode in V2_OPCODES {
            let (source, shown) = sample_operands(opcode);
            let line = [opcode.name.to_owned()].into_iter().chain(source).collect::<Vec<_>>().join(" ");
            let encoded = encode(opcode, &line, 0x20, &symbols).unwrap_or_else(|e| panic!("{line}: {e:?}"));
            assert!(!encoded.extra_tokens, "{line}");
            let words = encoded.into_words().unwrap();
            let pass2 = disassembler.flat_to_pass2(&le_bytes(&words), 0x20);
            assert_eq!(pass2.len(), 1, "{line}");
            assert_eq!(pass2[0].emitted, Emitted::Code(words.clone()), "{line}");

            let name = ALIASES
                .iter()
                .find(|&&(_, hidden)| hidden == opcode.name)
                .map_or(opcode.name, |&(alias, _)| alias);
            let want = if name == opcode.name {
                [name.to_owned()].into_iter().chain(shown).collect::<Vec<_>>().join(" ")
            } else {
                // Shown as its alias: the same words encode back from that text.
                let text = &pass2[0].input_text_line;
                let alias = V2_OPCODES.iter().find(|o| o.name == name).unwrap();
                let again = encode(alias, text, 0x20, &symbols).unwrap();
                assert_eq!(again.word0, Some(words[0]), "{line} shown as {text}");
                text.clone()
            };
            assert!(pass2[0].input_text_line.starts_with(name), "{line}");
            assert_eq!(pass2[0].input_text_line, want, "{line}");
        }
    }

    /// Little-endian bytes of `words`, as an ELF or flat binary holds them.
    fn le_bytes(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    // Words are read little-endian; a 64-bit argument is shown as one value
    fn test_flat_to_pass2_le_and_64bit() {
        let pass2 = Disassembler::new(V2_OPCODES).flat_to_pass2(&le_bytes(&[0x8BD0_0000, 5, 0xCBC0_0000, 0x5566_7788, 0x1122_3344]), 0x20);
        assert_eq!(pass2.len(), 2);
        assert_eq!(pass2[0].program_counter, 0x20);
        assert_eq!(pass2[0].emitted, Emitted::Code(vec![0x8BD0_0000, 5]));
        assert!(pass2[0].input_text_line.starts_with("SETR "), "{}", pass2[0].input_text_line);
        assert!(pass2[0].input_text_line.ends_with(" 0x5"), "{}", pass2[0].input_text_line);
        assert_eq!(pass2[1].program_counter, 0x28);
        assert_eq!(pass2[1].emitted, Emitted::Code(vec![0xCBC0_0000, 0x5566_7788, 0x1122_3344]));
        assert!(pass2[1].input_text_line.ends_with(" 0x1122334455667788"), "{}", pass2[1].input_text_line);
    }

    #[test]
    // Regression: a 64-bit argument cut off by the end of the binary showed its
    // lo32 word on the instruction's line and then again as the next line.
    fn test_flat_to_pass2_truncated_argument() {
        let disassembler = Disassembler::new(V2_OPCODES);
        let pass2 = disassembler.flat_to_pass2(&le_bytes(&[0xCBC0_0000, 0x5566_7788]), 0x20);
        assert_eq!(pass2.len(), 1);
        assert_eq!(pass2[0].emitted, Emitted::Code(vec![0xCBC0_0000, 0x5566_7788]));
        assert!(
            pass2[0].input_text_line.ends_with(" 0x55667788 (truncated)"),
            "{}",
            pass2[0].input_text_line
        );

        let pass2 = disassembler.flat_to_pass2(&le_bytes(&[0x8BD0_0000]), 0x20);
        assert_eq!(pass2.len(), 1);
        assert!(pass2[0].input_text_line.ends_with(" (truncated)"), "{}", pass2[0].input_text_line);
    }
}
