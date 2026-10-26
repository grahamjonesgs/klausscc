//! Classifying a source line by what it holds, which for an instruction
//! needs the opcode table.

use crate::asm::lexer::{data_name_from_string, is_blank, is_comment, is_start, label_name_from_string};
use crate::asm::line::LineType;
use crate::isa::{Opcode, return_opcode};

/// Directives that emit data, like a `#name` definition.
const DATA_DIRECTIVES: [&str; 3] = [".word", ".space", ".zeropad"];

/// Directives that emit nothing: section switches and symbol declarations
/// from C compiler output.  Pass 1 tracks the sections and turns
/// `.comm`/`.lcomm` into a label and `.space`.
const OTHER_DIRECTIVES: [&str; 9] = [".text", ".data", ".rodata", ".bss", ".global", ".globl", ".extern", ".comm", ".lcomm"];

impl LineType {
    /// What the source line `line` holds.
    ///
    /// Looks at the first word: a label (`name:`), a data definition
    /// (`#name` or a data directive), an opcode from `opcodes`, the `_start`
    /// marker, a `//` comment or another directive.  A blank line is
    /// [`Blank`](Self::Blank) and anything else is [`Error`](Self::Error).
    pub(crate) fn classify(line: &str, opcodes: &[Opcode]) -> Self {
        if label_name_from_string(line).is_some() {
            return Self::Label;
        }
        if data_name_from_string(line).is_some() {
            return Self::Data;
        }
        if return_opcode(line, opcodes).is_some() {
            return Self::Opcode;
        }
        if is_blank(line) {
            return Self::Blank;
        }
        if is_start(line) {
            return Self::Start;
        }
        if is_comment(line) {
            return Self::Comment;
        }
        match line.split_whitespace().next() {
            Some(word) if DATA_DIRECTIVES.contains(&word) => Self::Data,
            Some(word) if OTHER_DIRECTIVES.contains(&word) => Self::Directive,
            _ => Self::Error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isa::table::V2_OPCODES;

    #[test]
    fn test_classify_known_opcode_is_valid() {
        let input = String::from("PUSH");
        let opcodes = V2_OPCODES;
        assert_ne!(LineType::classify(&input, opcodes), LineType::Error);
    }

    #[test]
    fn test_classify_unknown_opcode_is_error() {
        let input = String::from("PULL");
        let opcodes = V2_OPCODES;
        assert_eq!(LineType::classify(&input, opcodes), LineType::Error);
    }

    #[test]
    // Test for opcode line type
    fn test_line_type1() {
        let input = String::from("PUSH");
        let opcodes = V2_OPCODES;
        let output = LineType::classify(&input, opcodes);
        assert_eq!(output, LineType::Opcode);
    }

    #[test]
    // Test for label line type
    fn test_line_type2() {
        let input = String::from("LOOP:");
        let opcodes = V2_OPCODES;
        let output = LineType::classify(&input, opcodes);
        assert_eq!(output, LineType::Label);
    }

    #[test]
    // Test for data line type
    fn test_line_type3() {
        let input = String::from("#Data_name");
        let opcodes = V2_OPCODES;
        let output = LineType::classify(&input, opcodes);
        assert_eq!(output, LineType::Data);
    }

    #[test]
    // Test for blank line type
    fn test_line_type4() {
        let input = String::default();
        let opcodes = V2_OPCODES;
        let output = LineType::classify(&input, opcodes);
        assert_eq!(output, LineType::Blank);
    }

    #[test]
    // Test for comment line type
    fn test_line_type5() {
        let input = String::from("//This is a comment");
        let opcodes = V2_OPCODES;
        let output = LineType::classify(&input, opcodes);
        assert_eq!(output, LineType::Comment);
    }

    #[test]
    // Test for start line type
    fn test_line_type6() {
        let input = String::from("_start");
        let opcodes = V2_OPCODES;
        let output = LineType::classify(&input, opcodes);
        assert_eq!(output, LineType::Start);
    }

    #[test]
    // Test for error line type
    fn test_line_type7() {
        let input = String::from("1234");
        let opcodes = V2_OPCODES;
        let output = LineType::classify(&input, opcodes);
        assert_eq!(output, LineType::Error);
    }

    #[test]
    fn test_line_type_directive_word() {
        let opcodes = V2_OPCODES;
        assert_eq!(LineType::classify(".word 42", opcodes), LineType::Data);
    }

    #[test]
    fn test_line_type_directive_space() {
        let opcodes = V2_OPCODES;
        assert_eq!(LineType::classify(".space 16", opcodes), LineType::Data);
    }

    #[test]
    fn test_line_type_directive_text() {
        let opcodes = V2_OPCODES;
        assert_eq!(LineType::classify(".text", opcodes), LineType::Directive);
    }

    #[test]
    fn test_line_type_directive_data() {
        let opcodes = V2_OPCODES;
        assert_eq!(LineType::classify(".data", opcodes), LineType::Directive);
    }

    #[test]
    fn test_line_type_directive_global() {
        let opcodes = V2_OPCODES;
        assert_eq!(LineType::classify(".global main", opcodes), LineType::Directive);
    }

    #[test]
    fn test_line_type_directive_comm() {
        let opcodes = V2_OPCODES;
        assert_eq!(LineType::classify(".comm buffer, 256, 4", opcodes), LineType::Directive);
    }

    #[test]
    fn test_line_type_directive_lcomm() {
        let opcodes = V2_OPCODES;
        assert_eq!(LineType::classify(".lcomm temp 8", opcodes), LineType::Directive);
    }
}
