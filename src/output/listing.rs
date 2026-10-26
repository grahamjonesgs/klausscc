//! The `.code` listing: every source line with its address and encoding.

use crate::asm::line::{LineType, Pass2};
use crate::diag::Diagnostics;
use crate::layout::{CODE_BASE, heap_start};
use itertools::Itertools as _;
use std::fs::File;
use std::io::{BufWriter, Error, Write};
use std::path::Path;

/// Format an instruction's hex as its 32-bit words separated by spaces.
///
/// The caller pads the result to the listing's column width.
fn format_opcodes(input: &str) -> String {
    input
        .as_bytes()
        .chunks(8)
        .map(|word| std::str::from_utf8(word).unwrap_or_default())
        .join(" ")
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
    // program_counter is already a byte address; opcode.len()/8 = words, *4 = bytes → /2 total
    let program_end: u32 = pass2
        .iter()
        .filter(|p| p.line_type == LineType::Opcode || p.line_type == LineType::Data)
        .map(|p| p.program_counter.saturating_add(p.opcode.len() as u32 / 2))
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
            LineType::Opcode => writeln!(out, "0x{:08X}: {:<17} -- {text}", pass.program_counter, format_opcodes(&pass.opcode))?,
            LineType::Data => {
                // One row per 64-bit word; a final part word gets a row of its own.
                for (chunk, n) in pass.opcode.as_bytes().chunks(16).zip(0_u32..) {
                    let word = std::str::from_utf8(chunk).unwrap_or_default();
                    writeln!(out, "0x{:08X}: {word:<32}  -- {text}", pass.program_counter + n * 8)?;
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
        assert_eq!(format_opcodes("0000000000000000"), "00000000 00000000");
        assert_eq!(format_opcodes("0000"), "0000");
        assert_eq!(format_opcodes("0123456789ABCDEF"), "01234567 89ABCDEF");
        assert_eq!(format_opcodes("12345678"), "12345678");
        assert_eq!(format_opcodes("123"), "123");
        assert_eq!(format_opcodes("8BD000000000004100000000"), "8BD00000 00000041 00000000");
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
                opcode: String::from("x"),
            },
            Pass2 {
                input_text_line: "DELAY 0x7".to_owned(),
                file_name: String::from("test"),
                line_counter: 1,
                program_counter: 1,
                line_type: LineType::Opcode,
                opcode: String::from("000F013"),
            },
            Pass2 {
                input_text_line: "PUSH A".to_owned(),
                file_name: String::from("test"),
                line_counter: 2,
                program_counter: 3,
                line_type: LineType::Opcode,
                opcode: String::from("0000F013"),
            },
            Pass2 {
                input_text_line: "RET".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 4,
                line_type: LineType::Opcode,
                opcode: String::from("0000F013"),
            },
            Pass2 {
                input_text_line: "RET".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Opcode,
                opcode: String::from("0000F013"),
            },
            Pass2 {
                input_text_line: ":ERIC".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Label,
                opcode: String::default(),
            },
            Pass2 {
                input_text_line: "// Comment".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Comment,
                opcode: String::from("test"),
            },
            Pass2 {
                input_text_line: "#DATA1 \"HELLO\"".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Data,
                opcode: String::from("12345678FFFFFFFFDDDDDDDD"),
            },
            Pass2 {
                input_text_line: "xxx".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Error,
                opcode: String::from("test"),
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
        // opcode "12345678FFFFFFFFDDDDDDDD" (24 chars) yields 2 data lines, the second a half word
        assert_eq!(buffer.lines().count(), 14);
        assert_eq!(
            buffer,
            "0x00000000: 0000000000000018                  -- heap_start  (set by assembler)\n0x00000008: 0000000000000000                  -- heap_end    (reserved)\n0x00000010: 0000000000000000                  -- (reserved)\n0x00000018: 0000000000000000                  -- (reserved)\n0x00000000: x                 -- MOV 0xEEEEEEEE 0xFFFFFFFF\n0x00000001: 000F013           -- DELAY 0x7\n0x00000003: 0000F013          -- PUSH A\n0x00000004: 0000F013          -- RET\n0x00000005: 0000F013          -- RET\n0x00000005:                   -- :ERIC\n                              -- // Comment\n0x00000005: 12345678FFFFFFFF                  -- #DATA1 \"HELLO\"\n0x0000000D: DDDDDDDD                          -- #DATA1 \"HELLO\"\nError                         -- xxx\n"
        );
    }
}
