//! The `.code` listing: every source line with its address and encoding.

use crate::asm::line::{Emitted, LineType, Pass2};
use crate::diag::Diagnostics;
use crate::layout::{CODE_BASE, heap_start};
use itertools::Itertools as _;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Error, Write};
use std::path::Path;

/// Format an instruction's words as 8 hex digits each, separated by spaces.
///
/// The caller pads the result to the listing's column width.
fn format_opcodes(words: &[u32]) -> String {
    words.iter().map(|word| format!("{word:08X}")).join(" ")
}

/// Format data bytes (memory order) as their 32-bit words' values, 8 hex
/// digits each with no separator.  Data is always whole words; a trailing
/// part word would be shown zero-filled.
fn format_data_words(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for chunk in bytes.chunks(4) {
        let mut word = [0_u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        let _ = write!(hex, "{:08X}", u32::from_le_bytes(word));
    }
    hex
}

/// Output the code details file to given filename.
///
/// Writes all data to the detailed code file.
pub(crate) fn write_code_output_file(filename: impl AsRef<Path>, pass2: &[Pass2], msg_list: &mut Diagnostics) -> Result<(), Error> {
    let filename = filename.as_ref();
    let mut file = BufWriter::new(File::create(filename)?);
    msg_list.info(format!("Writing code file to {}", filename.display()));
    write_listing(&mut file, pass2)?;
    file.flush()
}

/// Write the listing of `pass2` to `out`: the heap header, then every line
/// with its address and encoding.
fn write_listing(out: &mut impl Write, pass2: &[Pass2]) -> Result<(), Error> {
    // Compute heap_start from the first byte after the program, as the .kbt does.
    let program_end: u32 = pass2
        .iter()
        .filter(|p| p.line_type == LineType::Opcode || p.line_type == LineType::Data)
        .map(|p| p.program_counter.saturating_add(p.emitted.byte_len() as u32))
        .max()
        .unwrap_or(CODE_BASE);
    let heap_start = heap_start(program_end);

    // Emit the 4 reserved heap header words before the assembled code
    let heap_header = [
        (format!("{heap_start:016X}"), "heap_start  (set by assembler)"),
        ("0000000000000000".to_owned(), "heap_end    (reserved)"),
        ("0000000000000000".to_owned(), "(reserved)"),
        ("0000000000000000".to_owned(), "(reserved)"),
    ];
    for (i, (value, comment)) in heap_header.iter().enumerate() {
        writeln!(out, "0x{:08X}: {value:<33} -- {comment}", i * 8)?;
    }

    for pass in pass2 {
        let text = &pass.input_text_line;
        match pass.line_type {
            LineType::Opcode => {
                let words = match &pass.emitted {
                    Emitted::Code(words) => words.as_slice(),
                    _ => &[],
                };
                writeln!(out, "0x{:08X}: {:<17} -- {text}", pass.program_counter, format_opcodes(words))?;
            }
            LineType::Data => {
                // One row per 64-bit word; a final part word gets a row of its own.
                let bytes = match &pass.emitted {
                    Emitted::Data(bytes) => bytes.as_slice(),
                    _ => &[],
                };
                for (chunk, n) in bytes.chunks(8).zip(0_u32..) {
                    writeln!(out, "0x{:08X}: {:<32}  -- {text}", pass.program_counter + n * 8, format_data_words(chunk))?;
                }
            }
            LineType::Label => writeln!(out, "0x{:08X}:                   -- {text}", pass.program_counter)?,
            LineType::Error => writeln!(out, "Error                         -- {text}")?,
            LineType::Blank | LineType::Comment | LineType::Directive | LineType::Start => writeln!(out, "                              -- {text}")?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    // Check for formatting of codes used for debug file
    fn test_format_opcodes() {
        assert_eq!(format_opcodes(&[0, 0]), "00000000 00000000");
        assert_eq!(format_opcodes(&[0x0123_4567, 0x89AB_CDEF]), "01234567 89ABCDEF");
        assert_eq!(format_opcodes(&[0x1234_5678]), "12345678");
        assert_eq!(format_opcodes(&[0x123]), "00000123");
        assert_eq!(format_opcodes(&[]), "");
        assert_eq!(format_opcodes(&[0x8BD0_0000, 0x41, 0]), "8BD00000 00000041 00000000");
    }

    #[test]
    // Data bytes are shown as the values of their little-endian 32-bit words
    fn test_format_data_words() {
        assert_eq!(format_data_words(&[0x78, 0x56, 0x34, 0x12, 0xFF, 0xFF, 0xFF, 0xFF]), "12345678FFFFFFFF");
        assert_eq!(format_data_words(&[2, 0, 0, 0]), "00000002");
        assert_eq!(format_data_words(&[]), "");
    }

    #[test]
    // The listing of a real program with a string, as the reference shows it
    // (doc/klauss_isa_reference.md, "The .code Listing"): the string is its
    // bytes in memory order, NUL-padded to one 64-bit row.
    fn test_listing_of_string_program() {
        use crate::asm::assemble_lines;
        use crate::asm::line::SourceLine;
        use crate::asm::macros::{builtin_macros, expand_embedded_macros};
        use crate::isa::Isa;

        let source = "_start\nSETR A 5\nLOOP:\nDECR A\nJMPNZ LOOP:\nHALT\n#S \"Hello\"\n.word 7";
        let mut msg_list = Diagnostics::new();
        let macro_list = expand_embedded_macros(builtin_macros(), &mut msg_list);
        let lines = source
            .lines()
            .zip(1..)
            .map(|(line, line_counter)| SourceLine {
                file_name: "test.kla".to_owned(),
                input_text_line: line.to_owned(),
                line_counter,
            })
            .collect();
        let assembly = assemble_lines(lines, &Isa::v2(), &macro_list, &mut msg_list);
        assert!(!msg_list.has_errors());
        let mut listing = Vec::new();
        write_listing(&mut listing, &assembly.lines).unwrap();
        assert_eq!(
            String::from_utf8(listing).unwrap(),
            "\
0x00000000: 0000000000000048                  -- heap_start  (set by assembler)
0x00000008: 0000000000000000                  -- heap_end    (reserved)
0x00000010: 0000000000000000                  -- (reserved)
0x00000018: 0000000000000000                  -- (reserved)
                              -- _start
0x00000020: 8BD00000 00000005 -- SETR A 5
0x00000028:                   -- LOOP:
0x00000028: 57C80000          -- DECR A
0x0000002C: A00C0000 00000028 -- JMPNZ LOOP:
0x00000034: 6C010000          -- HALT
0x00000038: 6C6C65480000006F                  -- #S \"Hello\"
0x00000040: 0000000700000000                  -- .word 7
"
        );
    }

    #[test]
    fn test_write_code_output_file() {
        let tmp_dir = TempDir::new().unwrap();
        let mut msg_list = Diagnostics::new();
        let pass2: Vec<Pass2> = vec![
            Pass2 {
                input_text_line: "MOV 0xEEEEEEEE 0xFFFFFFFF".to_owned(),
                file_name: String::from("test"),
                line_counter: 1,
                program_counter: 0,
                line_type: LineType::Opcode,
                emitted: Emitted::Code(vec![0xEEEE_EEEE, 0xFFFF_FFFF]),
            },
            Pass2 {
                input_text_line: "DELAY 0x7".to_owned(),
                file_name: String::from("test"),
                line_counter: 1,
                program_counter: 1,
                line_type: LineType::Opcode,
                emitted: Emitted::Code(vec![0x000F_0013]),
            },
            Pass2 {
                input_text_line: "PUSH A".to_owned(),
                file_name: String::from("test"),
                line_counter: 2,
                program_counter: 3,
                line_type: LineType::Opcode,
                emitted: Emitted::Code(vec![0x0000_F013]),
            },
            Pass2 {
                input_text_line: "RET".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 4,
                line_type: LineType::Opcode,
                emitted: Emitted::Code(vec![0x0000_F013]),
            },
            Pass2 {
                input_text_line: "RET".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Opcode,
                emitted: Emitted::Code(vec![0x0000_F013]),
            },
            Pass2 {
                input_text_line: ":ERIC".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Label,
                emitted: Emitted::None,
            },
            Pass2 {
                input_text_line: "// Comment".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Comment,
                emitted: Emitted::None,
            },
            Pass2 {
                input_text_line: "#DATA1 \"HELLO\"".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Data,
                emitted: Emitted::Data([0x1234_5678_u32, 0xFFFF_FFFF, 0xDDDD_DDDD].iter().flat_map(|w| w.to_le_bytes()).collect()),
            },
            Pass2 {
                input_text_line: "xxx".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Error,
                emitted: Emitted::None,
            },
        ];

        let file_path1 = tmp_dir.path().join("test1.klc");
        let binding = file_path1;
        let file_name1: &str = binding.to_str().unwrap();

        let result_write = write_code_output_file(file_name1, &pass2, &mut msg_list);
        result_write.unwrap();

        let buffer = fs::read_to_string(file_name1).unwrap();
        // 4 header lines prepended; word 0 = heap_start in bytes (program ends at 0x11,
        // rounded up to 8 like the .kbt → 0x18)
        // 64-bit mode: header offsets are i*8, values are 16-char hex; data words are 16 hex chars each
        // 12 data bytes (3 words) yield 2 data lines, the second a half word
        assert_eq!(buffer.lines().count(), 14);
        assert_eq!(
            buffer,
            "0x00000000: 0000000000000018                  -- heap_start  (set by assembler)\n0x00000008: 0000000000000000                  -- heap_end    (reserved)\n0x00000010: 0000000000000000                  -- (reserved)\n0x00000018: 0000000000000000                  -- (reserved)\n0x00000000: EEEEEEEE FFFFFFFF -- MOV 0xEEEEEEEE 0xFFFFFFFF\n0x00000001: 000F0013          -- DELAY 0x7\n0x00000003: 0000F013          -- PUSH A\n0x00000004: 0000F013          -- RET\n0x00000005: 0000F013          -- RET\n0x00000005:                   -- :ERIC\n                              -- // Comment\n0x00000005: 12345678FFFFFFFF                  -- #DATA1 \"HELLO\"\n0x0000000D: DDDDDDDD                          -- #DATA1 \"HELLO\"\nError                         -- xxx\n"
        );
    }
}
