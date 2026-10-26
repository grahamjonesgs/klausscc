//! Pass 2: encode every line's machine code, with the labels resolved.

use crate::asm::data::data_as_bytes;
use crate::asm::lexer::strip_comments;
use crate::asm::line::{LineType, Pass1, Pass2};
use crate::asm::symbols::SymbolTable;
use crate::diag::Diagnostics;
use crate::isa::Isa;
use crate::isa::encode::{EncodeError, Encoded, encode};

/// Returns pass2 from pass1.
///
/// Pass1 with program counters and returns vector of pass2, with final values.
pub(super) fn get_pass2(msg_list: &mut Diagnostics, pass1: Vec<Pass1>, isa: &Isa, labels: &SymbolTable) -> Vec<Pass2> {
    let mut pass2: Vec<Pass2> = Vec::new();
    for line in pass1 {
        let (new_opcode, line_type) = match line.line_type {
            LineType::Opcode => {
                let encoded = encode_line(&line, isa, labels, msg_list);
                let line_type = if encoded.as_ref().is_some_and(|e| e.word0.is_some()) {
                    LineType::Opcode
                } else {
                    LineType::Error
                };
                (encoded.map_or_else(|| "ERR     ".to_owned(), |e| e.hex()), line_type)
            }
            LineType::Data => (data_as_bytes(line.input_text_line.as_str()).unwrap_or_default(), LineType::Data),
            line_type => (String::new(), line_type),
        };

        pass2.push(Pass2 {
            input_text_line: line.input_text_line,
            file_name: line.file_name,
            line_counter: line.line_counter,
            program_counter: line.program_counter,
            line_type,
            opcode: new_opcode,
        });
    }
    pass2
}

/// Encode the instruction on `line`, reporting what is wrong with it.
///
/// The words come back even when they are partly wrong, for the listing;
/// `None` means the line names no opcode.
fn encode_line(line: &Pass1, isa: &Isa, labels: &SymbolTable, msg_list: &mut Diagnostics) -> Option<Encoded> {
    let code = strip_comments(&line.input_text_line);
    let Some(opcode) = isa.find(code) else {
        msg_list.error_at(&line.file_name, line.line_counter, format!("Opcode not found - \"{code}\""));
        return None;
    };
    let encoded = encode(opcode, code, line.program_counter, labels).unwrap_or_else(|EncodeError { faults, partial }| {
        for fault in faults {
            msg_list.error_at(&line.file_name, line.line_counter, fault.message(code));
        }
        partial
    });
    // One warning for the line, however many extra tokens it has.
    if encoded.extra_tokens {
        msg_list.warning_at(&line.file_name, line.line_counter, format!("Too many arguments found - \"{code}\""));
    }
    Some(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::MessageType;

    #[test]
    // Test get_pass2 for correct vector returned, with correct opcodes, registers and variables
    fn test_get_pass2_1() {
        let mut msg_list = Diagnostics::new();
        let labels = SymbolTable::default();
        let opcodes = &Isa::v2();
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
        let opcodes = &Isa::v2();
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

    #[test]
    // Encode faults are errors and extra tokens one warning, each at the
    // line; only a failed word 0 makes the line an error line.
    fn test_get_pass2_reports_faults_at_the_line() {
        let mut msg_list = Diagnostics::new();
        let labels = SymbolTable::default();
        let line = |text: &str, line_counter| Pass1 {
            input_text_line: text.to_owned(),
            file_name: String::from("prog.kla"),
            line_counter,
            program_counter: 0x20,
            line_type: LineType::Opcode,
        };
        let pass2 = get_pass2(
            &mut msg_list,
            vec![line("SETR A 1 2 3 // too many", 4), line("SHLV A 64", 5), line("PUSHV nowhere:", 6)],
            &Isa::v2(),
            &labels,
        );
        let shown: Vec<(&str, LineType)> = pass2.iter().map(|p| (p.opcode.as_str(), p.line_type)).collect();
        assert_eq!(
            shown,
            [
                ("8BD0000000000001", LineType::Opcode),
                ("ERR     ", LineType::Error),
                ("A440000000000000", LineType::Opcode),
            ]
        );
        let messages: Vec<_> = msg_list
            .messages()
            .iter()
            .map(|m| (m.level(), m.file_name(), m.line_number(), m.text()))
            .collect();
        assert_eq!(
            messages,
            [
                (
                    MessageType::Warning,
                    Some("prog.kla"),
                    Some(4),
                    "Too many arguments found - \"SETR A 1 2 3\""
                ),
                (
                    MessageType::Error,
                    Some("prog.kla"),
                    Some(5),
                    "Count 64 out of range 0..=63 - \"SHLV A 64\""
                ),
                (MessageType::Error, Some("prog.kla"), Some(6), "Label NOWHERE: not found"),
            ]
        );
    }
}
