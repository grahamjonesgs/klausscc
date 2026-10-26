//! Classifying a source line by what it holds, which for an instruction
//! needs the opcode table.

use crate::asm::lexer::{data_name_from_string, is_blank, is_comment, is_start, label_name_from_string};
use crate::asm::line::LineType;
use crate::isa::{Opcode, return_opcode};

/// Check if line is valid.
///
/// Returns true if line is not error.
pub(crate) fn is_valid_line(opcodes: &[Opcode], line: &str) -> bool {
    if line_type(opcodes, line) == LineType::Error {
        return false;
    }
    true
}

/// Returns enum of type of line.
///
/// Given a code line, will returns if line is Label, Opcode, Blank, Comment or Error.
pub(crate) fn line_type(opcodes: &[Opcode], line: &str) -> LineType {
    if label_name_from_string(line).is_some() {
        return LineType::Label;
    }
    if data_name_from_string(line).is_some() {
        return LineType::Data;
    }
    if return_opcode(line, opcodes).is_some() {
        return LineType::Opcode;
    }
    if is_blank(line) {
        return LineType::Blank;
    }
    if is_start(line) {
        return LineType::Start;
    }
    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        if is_comment(word) && i == 0 {
            return LineType::Comment;
        }
    }
    // Check for C compiler directives
    let first_word = line.split_whitespace().next().unwrap_or("");
    match first_word {
        ".word" | ".space" | ".zeropad" => return LineType::Data,
        ".text" | ".data" | ".rodata" | ".bss" | ".global" | ".globl" | ".extern" | ".comm" | ".lcomm" => return LineType::Comment,
        _ => {}
    }
    LineType::Error
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Test for valid line returns true is opcode is found
    fn test_is_valid_line1() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 0,
            section: String::default(),
        });
        let output = is_valid_line(opcodes, &input);
        assert!(output);
    }

    #[test]
    fn test_is_valid_line2() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PULL"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 0,
            section: String::default(),
        });
        let output = is_valid_line(opcodes, &input);
        assert!(!output);
    }

    #[test]
    // Test for opcode line type
    fn test_line_type1() {
        let input = String::from("PUSH");
        let opcodes = vec![Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 0,
            section: String::default(),
        }];
        let output = line_type(&opcodes, &input);
        assert_eq!(output, LineType::Opcode);
    }

    #[test]
    // Test for label line type
    fn test_line_type2() {
        let input = String::from("LOOP:");
        let opcodes = &mut Vec::<Opcode>::new();
        let output = line_type(opcodes, &input);
        assert_eq!(output, LineType::Label);
    }

    #[test]
    // Test for data line type
    fn test_line_type3() {
        let input = String::from("#Data_name");
        let opcodes = &mut Vec::<Opcode>::new();
        let output = line_type(opcodes, &input);
        assert_eq!(output, LineType::Data);
    }

    #[test]
    // Test for blank line type
    fn test_line_type4() {
        let input = String::default();
        let opcodes = &mut Vec::<Opcode>::new();
        let output = line_type(opcodes, &input);
        assert_eq!(output, LineType::Blank);
    }

    #[test]
    // Test for comment line type
    fn test_line_type5() {
        let input = String::from("//This is a comment");
        let opcodes = &mut Vec::<Opcode>::new();
        let output = line_type(opcodes, &input);
        assert_eq!(output, LineType::Comment);
    }

    #[test]
    // Test for start line type
    fn test_line_type6() {
        let input = String::from("_start");
        let opcodes = &mut Vec::<Opcode>::new();
        let output = line_type(opcodes, &input);
        assert_eq!(output, LineType::Start);
    }

    #[test]
    // Test for error line type
    fn test_line_type7() {
        let input = String::from("1234");
        let opcodes = &mut Vec::<Opcode>::new();
        let output = line_type(opcodes, &input);
        assert_eq!(output, LineType::Error);
    }

    #[test]
    fn test_line_type_directive_word() {
        let opcodes = &mut Vec::<Opcode>::new();
        assert_eq!(line_type(opcodes, ".word 42"), LineType::Data);
    }

    #[test]
    fn test_line_type_directive_space() {
        let opcodes = &mut Vec::<Opcode>::new();
        assert_eq!(line_type(opcodes, ".space 16"), LineType::Data);
    }

    #[test]
    fn test_line_type_directive_text() {
        let opcodes = &mut Vec::<Opcode>::new();
        assert_eq!(line_type(opcodes, ".text"), LineType::Comment);
    }

    #[test]
    fn test_line_type_directive_data() {
        let opcodes = &mut Vec::<Opcode>::new();
        assert_eq!(line_type(opcodes, ".data"), LineType::Comment);
    }

    #[test]
    fn test_line_type_directive_global() {
        let opcodes = &mut Vec::<Opcode>::new();
        assert_eq!(line_type(opcodes, ".global main"), LineType::Comment);
    }

    #[test]
    fn test_line_type_directive_comm() {
        let opcodes = &mut Vec::<Opcode>::new();
        assert_eq!(line_type(opcodes, ".comm buffer, 256, 4"), LineType::Comment);
    }

    #[test]
    fn test_line_type_directive_lcomm() {
        let opcodes = &mut Vec::<Opcode>::new();
        assert_eq!(line_type(opcodes, ".lcomm temp 8"), LineType::Comment);
    }
}
