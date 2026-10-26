//! Pass 1: give every line its address and gather the data section.
//!
//! Lines keep their source order, except that data (and the labels on it)
//! inside a `.data`/`.rodata`/`.bss` section is moved after the code, starting
//! 8-byte aligned.  `.comm`/`.lcomm` become a label plus `.space` there too.

use crate::asm::classify::{is_valid_line, line_type};
use crate::asm::data::num_data_bytes;
use crate::asm::lexer::strip_comments;
use crate::asm::line::{LineType, Pass1, SourceLine};
use crate::diag::Diagnostics;
use crate::isa::{Opcode, num_arguments};
use crate::layout::CODE_BASE;
use crate::num::{fits_imm32, parse_bits64, parse_int, parse_operand};

/// Auto-upgrade `SETR R value` to `SETR64 R value` when the immediate doesn't fit in 32 bits.
///
/// Returns the (possibly rewritten) line string unchanged for all other mnemonics or
/// when the value fits in the 32-bit range accepted by `convert_argument`
/// (i.e. `i32::MIN ..= 0xFFFF_FFFF`).
fn upgrade_setr_to_setr64(line: &str) -> String {
    let stripped = strip_comments(line);
    let mut words = stripped.split_whitespace();
    if !words.next().unwrap_or("").eq_ignore_ascii_case("setr") {
        return line.to_owned();
    }
    let _reg = words.next(); // skip register name
    let Some(val_str) = words.next() else {
        return line.to_owned();
    };
    // Same rule as convert_argument: hex is a 64-bit pattern (0xFFFF_FFFF_FFFF_FFFF
    // is -1 and fits), decimal keeps its value (18446744073709551615 does not fit).
    // Labels, bad literals and values no 64-bit word can hold stay SETR, so
    // convert_argument reports them.
    let Ok(val) = parse_operand(val_str) else {
        return line.to_owned();
    };
    if fits_imm32(val) || parse_bits64(val_str).is_err() {
        return line.to_owned();
    }
    // Upgrade: replace the leading "SETR" (any case) with "SETR64".
    let trimmed = line.trim_start();
    let leading_ws = &line[..line.len() - trimmed.len()];
    // SAFETY: trimmed starts with "setr" (4 ASCII chars), confirmed by eq_ignore_ascii_case above.
    format!("{leading_ws}SETR64{}", &trimmed[4..])
}

/// Returns pass1 from pass0.
///
/// Takes the macro expanded pass0 and returns vector of pass1, with the program counters.
pub(super) fn get_pass1(msg_list: &mut Diagnostics, pass0: Vec<SourceLine>, oplist: &[Opcode]) -> Vec<Pass1> {
    let mut pass1: Vec<Pass1> = Vec::new();
    let mut program_counter: u32 = CODE_BASE; // Byte address: 4 header words × 8 bytes each (64-bit words)
    let mut data_pass0: Vec<SourceLine> = Vec::new();
    let mut in_data_section = false;

    for pass in pass0 {
        let stripped = strip_comments(&pass.input_text_line);
        let first_word = stripped.split_whitespace().next().unwrap_or("").to_owned();

        // Track section context for C compiler directives
        match first_word.as_str() {
            ".text" => {
                in_data_section = false;
            }
            ".data" | ".rodata" | ".bss" => {
                in_data_section = true;
            }
            _ => {}
        }

        // Expand .comm/.lcomm NAME SIZE into label + .space, defer to data section
        if first_word == ".comm" || first_word == ".lcomm" {
            let parts: Vec<&str> = stripped
                .split(|c: char| c.is_whitespace() || c == ',')
                .filter(|s| !s.is_empty())
                .collect();
            if parts.len() >= 3 {
                let name = parts[1];
                let size_str = parts[2];
                let size = match parse_int(size_str) {
                    Ok(size) if size >= 0 => size,
                    _ => {
                        msg_list.error_at(
                            &pass.file_name,
                            pass.line_counter,
                            format!("Invalid size {size_str} for {first_word} {name}"),
                        );
                        0
                    }
                };
                data_pass0.push(SourceLine {
                    input_text_line: format!("{name}:"),
                    file_name: pass.file_name.clone(),
                    line_counter: pass.line_counter,
                });
                if size > 0 {
                    data_pass0.push(SourceLine {
                        input_text_line: format!(".space {size}"),
                        file_name: pass.file_name.clone(),
                        line_counter: pass.line_counter,
                    });
                }
            }
            continue;
        }

        // Rewrite "SETR R val" → "SETR64 R val" before line_type/num_arguments when val > 32 bits.
        let upgraded_line = upgrade_setr_to_setr64(&pass.input_text_line);
        let lt = line_type(oplist, &upgraded_line);

        // Defer labels in data section to end of program with data
        if in_data_section && lt == LineType::Label {
            data_pass0.push(pass);
            continue;
        }

        pass1.push(Pass1 {
            input_text_line: upgraded_line.clone(),
            file_name: pass.file_name.clone(),
            line_counter: pass.line_counter,
            program_counter,
            line_type: lt,
        });
        if !is_valid_line(oplist, strip_comments(&upgraded_line)) {
            msg_list.error_at(&pass.file_name, pass.line_counter, format!("Error {upgraded_line}"));
        }
        if lt == LineType::Opcode {
            let num_args = num_arguments(oplist, strip_comments(&upgraded_line));
            if let Some(arguments) = num_args {
                program_counter += (arguments + 1) * 4; // Each word = 4 bytes
            }
        }

        if lt == LineType::Data {
            if in_data_section {
                data_pass0.push(pass);
                pass1.pop();
            } else {
                // Keep inline data when no explicit .data section is active.
                // This preserves label semantics for C compiler output data blocks.
                //
                program_counter += num_data_bytes(&pass.input_text_line, msg_list, pass.line_counter, &pass.file_name) / 2;
            }
        }
    }
    // 8-byte-align the start of the deferred data section. 64-bit loads
    // (MEMREADR / MEMGET64) mask the effective address down to an 8-byte boundary,
    // so a .word read from a 4-aligned address returns the preceding word instead.
    // Code is 4-byte-granular, data elements are 8-byte multiples, so aligning the
    // section start once keeps every label and .word in it aligned. The pad is a
    // real stream entry, so the sequential .kbt, the offset-based image, and label
    // addresses all stay consistent.
    if !data_pass0.is_empty() && !program_counter.is_multiple_of(8) {
        let pad = 8 - (program_counter % 8);
        pass1.push(Pass1 {
            input_text_line: format!(".zeropad {pad}"),
            file_name: String::from("<align>"),
            line_counter: 0,
            program_counter,
            line_type: LineType::Data,
        });
        program_counter += pad;
    }
    for data_pass in data_pass0 {
        let lt = line_type(oplist, &data_pass.input_text_line);
        pass1.push(Pass1 {
            input_text_line: data_pass.input_text_line.clone(),
            file_name: data_pass.file_name.clone(),
            line_counter: data_pass.line_counter,
            program_counter,
            line_type: lt,
        });

        if lt == LineType::Data {
            program_counter += num_data_bytes(&data_pass.input_text_line, msg_list, data_pass.line_counter, &data_pass.file_name) / 2;
        }
    }
    pass1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Regression: a decimal SETR value that only fits 64 bits as a two's-complement
    // pattern (e.g. u64::MAX) stayed SETR and failed the 32-bit bounds check.
    fn test_upgrade_setr_to_setr64() {
        assert_eq!(upgrade_setr_to_setr64("SETR A 18446744073709551615"), "SETR64 A 18446744073709551615");
        assert_eq!(upgrade_setr_to_setr64("  setr B 0x1_0000_0000 // big"), "  SETR64 B 0x1_0000_0000 // big");
        assert_eq!(upgrade_setr_to_setr64("SETR C 9223372036854775808"), "SETR64 C 9223372036854775808");
        assert_eq!(upgrade_setr_to_setr64("SETR D -2147483649"), "SETR64 D -2147483649");
        // Fits a sign-extended 32-bit word, or not a 64-bit literal at all: unchanged.
        for line in [
            "SETR A 0xFFFFFFFFFFFFFFFF",
            "SETR A 4294967295",
            "SETR A -1",
            "SETR A loop:",
            "SETR A 12abc",
            "SETR A -9223372036854775809",
            "COPY A B",
        ] {
            assert_eq!(upgrade_setr_to_setr64(line), line);
        }
    }

    #[test]
    // Test get_pass1 for correct vector returned, with correct program counters
    fn test_get_pass1_1() {
        let mut msg_list = Diagnostics::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("0000001X"),
            ops: Vec::new(),
            comment: String::new(),
            variables: 0,
            registers: 1,
            section: String::new(),
        });
        opcodes.push(Opcode {
            text_name: String::from("MOV"),
            hex_code: String::from("00000020"),
            ops: Vec::new(),
            comment: String::new(),
            variables: 2,
            registers: 0,
            section: String::new(),
        });
        opcodes.push(Opcode {
            text_name: String::from("RET"),
            hex_code: String::from("00000030"),
            ops: Vec::new(),
            comment: String::new(),
            variables: 0,
            registers: 0,
            section: String::new(),
        });

        let pass0 = vec![
            SourceLine {
                input_text_line: "MOV A B".to_owned(),
                file_name: String::new(),
                line_counter: 1,
            },
            SourceLine {
                input_text_line: "PUSH A".to_owned(),
                file_name: String::new(),
                line_counter: 2,
            },
            SourceLine {
                input_text_line: "RET".to_owned(),
                file_name: String::new(),
                line_counter: 3,
            },
            SourceLine {
                input_text_line: "#DATA1 0x2".to_owned(), // Should be moved to end
                file_name: String::new(),
                line_counter: 4,
            },
            SourceLine {
                input_text_line: "RET".to_owned(),
                file_name: String::new(),
                line_counter: 5,
            },
            SourceLine {
                input_text_line: "#DATA1 \"HELLO\"".to_owned(), // Should be moved to end
                file_name: String::new(),
                line_counter: 6,
            },
            SourceLine {
                input_text_line: "RET".to_owned(),
                file_name: String::new(),
                line_counter: 7,
            },
        ];
        let pass1 = get_pass1(&mut msg_list, pass0, opcodes);
        // Byte addressing: PC starts at 32 (4 header words × 8 bytes each in 64-bit).
        // Each instruction word = 4 bytes (opcode encoding unchanged).
        // Each 64-bit data word = 8 bytes (16 hex chars).
        assert_eq!(pass1[0].program_counter, 32); // MOV
        assert_eq!(pass1[1].program_counter, 44); // PUSH (MOV=3 words×4=12)
        assert_eq!(pass1[2].program_counter, 48); // RET  (+4)
        assert_eq!(pass1[3].program_counter, 52); // #DATA1 0x2 inline (+4)
        assert_eq!(pass1[4].program_counter, 68); // RET  (2 data words×8=16)
        assert_eq!(pass1[5].program_counter, 72); // #DATA1 "HELLO" inline (+4)
        assert_eq!(pass1[6].program_counter, 88); // RET  (string=16 bytes: 2 × 64-bit words)
    }

    #[test]
    // Test get_pass1 for correct vector returned, with correct program counters
    fn test_get_pass1_2() {
        let mut msg_list = Diagnostics::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("0000001X"),
            ops: Vec::new(),
            comment: String::new(),
            variables: 0,
            registers: 1,
            section: String::new(),
        });

        let pass0 = vec![SourceLine {
            input_text_line: "Test_not_code_line".to_owned(),
            file_name: String::new(),
            line_counter: 1,
        }];
        let _pass1 = get_pass1(&mut msg_list, pass0, opcodes);
        assert_eq!(msg_list.messages()[0].text(), "Error Test_not_code_line");
    }
}
