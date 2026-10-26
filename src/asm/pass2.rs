//! Pass 2: encode every line's machine code, with the labels resolved.

use crate::asm::data::data_as_bytes;
use crate::asm::lexer::strip_comments;
use crate::asm::line::{LineType, Pass1, Pass2};
use crate::asm::symbols::SymbolTable;
use crate::diag::Diagnostics;
use crate::isa::Opcode;
use crate::isa::encode::{add_arguments, add_registers, pc_relative_bits};

/// Returns pass2 from pass1.
///
/// Pass1 with program counters and returns vector of pass2, with final values.
pub(super) fn get_pass2(msg_list: &mut Diagnostics, pass1: Vec<Pass1>, oplist: &[Opcode], labels: &SymbolTable) -> Vec<Pass2> {
    let mut pass2: Vec<Pass2> = Vec::new();
    for line in pass1 {
        let new_opcode = if line.line_type == LineType::Opcode {
            let code = strip_comments(&line.input_text_line);
            let mut opcode = add_registers(oplist, code, &line.file_name, msg_list, line.line_counter);
            // ISA v3 PC-relative targets need this line's address.
            let rel = pc_relative_bits(oplist, code, line.program_counter, labels, msg_list, line.line_counter, &line.file_name);
            if rel != 0
                && let Some(w0) = opcode.get(..8).and_then(|h| u32::from_str_radix(h, 16).ok())
            {
                opcode = format!("{:08X}{}", w0 | rel, &opcode[8..]);
            }
            opcode.push_str(&add_arguments(oplist, code, msg_list, line.line_counter, &line.file_name, labels));
            opcode
        } else if line.line_type == LineType::Data {
            data_as_bytes(line.input_text_line.as_str()).unwrap_or_default()
        } else {
            String::new()
        };

        pass2.push(Pass2 {
            input_text_line: line.input_text_line,
            file_name: line.file_name,
            line_counter: line.line_counter,
            program_counter: line.program_counter,
            line_type: if new_opcode.contains("ERR") { LineType::Error } else { line.line_type },
            opcode: new_opcode,
        });
    }
    pass2
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isa::table::v2_opcodes;

    #[test]
    // Test get_pass2 for correct vector returned, with correct opcodes, registers and variables
    fn test_get_pass2_1() {
        let mut msg_list = Diagnostics::new();
        let labels = SymbolTable::default();
        let opcodes = &v2_opcodes();
        let pass2 = get_pass2(
            &mut msg_list,
            vec![
                Pass1 {
                    input_text_line: "PUSHV64 0xEEEEEEEE 0xFFFFFFFF".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 1,
                    program_counter: 0,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input_text_line: "DELAYV 0x7".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 1,
                    program_counter: 1,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input_text_line: "PUSH B".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 2,
                    program_counter: 3,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input_text_line: "RET".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 4,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input_text_line: "RET".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 5,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input_text_line: "SETR C 0xAAAA".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 5,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input_text_line: "SETR64 D 0xA 0xB".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 5,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input_text_line: "#DATA1 \"HELLO\"".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 5,
                    line_type: LineType::Data,
                },
                Pass1 {
                    input_text_line: "xxx".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 5,
                    line_type: LineType::Error,
                },
            ],
            opcodes,
            &labels,
        );
        assert_eq!(pass2[0].opcode, "E4400000EEEEEEEEFFFFFFFF");
        assert_eq!(pass2[1].opcode, "AC05000000000007");
        assert_eq!(pass2[2].opcode, "64000010");
        assert_eq!(pass2[3].opcode, "65800000");
        assert_eq!(pass2[4].opcode, "65800000");
        assert_eq!(pass2[5].opcode, "8BD002000000AAAA");
        assert_eq!(pass2[6].opcode, "CBC003000000000A0000000B");
        assert_eq!(pass2[7].opcode, "0000000248454C4C4F00000000000000"); // padded to 2 × 64-bit words
        assert_eq!(pass2[8].opcode, "");
    }

    #[test]
    // Test get_pass2 for invalid opcode
    fn test_get_pass2_2() {
        let mut msg_list = Diagnostics::new();
        let labels = SymbolTable::default();
        let opcodes = &v2_opcodes();
        let pass2 = get_pass2(
            &mut msg_list,
            vec![Pass1 {
                input_text_line: "TEST".to_owned(),
                file_name: String::from("test"),
                line_counter: 1,
                program_counter: 0,
                line_type: LineType::Opcode,
            }],
            opcodes,
            &labels,
        );
        assert_eq!(pass2[0].opcode, "ERR     ");
        assert_eq!(pass2[0].line_type, LineType::Error);
        assert_eq!(msg_list.messages()[0].text(), "Opcode not found - \"TEST\"");
    }
}
