//! The legacy `.vh` opcode-definition file (`opcode_select.vh`), which
//! held the opcode table and the standard macros.  The built-in
//! [`table`](crate::isa::table) replaced it; the parser is kept for backward
//! compatibility.

use super::{Opcode, return_opcode};
use crate::asm::line::SourceLine;
use crate::asm::macros::{Macro, find_macro, macro_from_string};
use crate::diag::Diagnostics;

/// Parse opcode definition line to opcode.
///
/// Receive a line from the opcode definition file and if possible parse of Some(Opcode), or None.
/// Supports both 32-bit format (`32'hXXXX_XXXX`) and legacy 16-bit format (`16'hXXXX`).
#[allow(
    dead_code,
    reason = "legacy `.vh` opcode-format parser; the v2 table is built in code but this is kept for backward compatibility and is exercised by unit tests"
)]
pub(crate) fn opcode_from_string(input_line: &str) -> Option<Opcode> {
    let pos_comment: usize;
    let pos_end_comment: usize;

    // Find the opcode — try 32'h (new 32-bit format) first, then 16'h (legacy format).
    // For 32'h: read XXXX_XXXX (9 chars including underscore separator), strip '_' → 8 hex digits.
    // For 16'h: read XXXX (4 chars), prepend '0000' → 8 hex digits.
    let hex_code: String = if let Some(location) = input_line.find("32'h") {
        // Check if the 32'h marker is preceded by a line comment (i.e. it is commented out)
        if let Some(comment_loc) = input_line.find("//")
            && comment_loc < location
        {
            return None;
        }
        let pos = location + 4;
        // Need XXXX_XXXX (9 chars) after "32'h"
        if input_line.len() < pos + 9 {
            return None;
        }
        // Build 8-char hex code by removing the underscore separator at offset 4
        let raw = input_line.get(pos..pos + 9)?;
        format!("{}{}", raw.get(..4)?, raw.get(5..9)?)
    } else {
        let location = input_line.find("16'h")?;
        // Legacy format: check if commented out
        if let Some(comment_loc) = input_line.find("//")
            && comment_loc < location
        {
            return None;
        }
        let pos = location + 4;
        // Need XXXX (4 chars) after "16'h"
        if input_line.len() < pos + 4 {
            return None;
        }
        format!("0000{}", input_line.get(pos..pos + 4)?)
    };

    // Define number of registers from trailing '?' nibbles in the 8-char hex code.
    // Each '?' represents one register field (nibble). Checked longest-first so that
    // three '?'s correctly sets 3 registers (not just 2 or 1).
    let mut num_registers: u32 = 0;
    if hex_code.get(7..8) == Some("?") {
        num_registers = 1;
    }
    if hex_code.get(6..8) == Some("??") {
        num_registers = 2;
    }
    if hex_code.get(5..8) == Some("???") {
        num_registers = 3;
    }

    // Look for variable, and set flag
    let mut num_variables: u32 = 0;
    if input_line.contains("w_var1") {
        if input_line.contains("w_var2") {
            num_variables = 2;
        } else {
            num_variables = 1;
        }
    }

    // Look for comment as first word is opcode name
    let pos_name: usize = match input_line.find("// ") {
        None => input_line.find("//")? + 2,
        Some(location) => location + 3, // Assumes one space after the // before the name of the opcode
    };

    // Find end of first word after comment as end of opcode name
    let pos_end_name: usize = input_line
        .get(pos_name..)
        .unwrap_or("")
        .find(' ')
        .map_or(input_line.len(), |location| location + pos_name);

    // Set comments field, or none if missing
    if input_line.len() > pos_end_name + 1 {
        pos_comment = pos_end_name + 1;
        pos_end_comment = input_line.len();
    } else {
        pos_comment = 0;
        pos_end_comment = 0;
    }

    Some(Opcode {
        hex_code,
        ops: Vec::new(),
        registers: num_registers,
        variables: num_variables,
        comment: input_line.get(pos_comment..pos_end_comment).unwrap_or("").to_owned(),
        text_name: input_line.get(pos_name..pos_end_name).unwrap_or("").to_owned(),
        section: String::default(),
    })
}

/// Parse file to opcode and macro vectors.
///
/// Parses the .vh verilog file, creates two vectors of macro and opcode, returning None, None or Some(Opcode), Some(Macro).
#[allow(
    dead_code,
    reason = "legacy `.vh` opcode-file parser; superseded by the built-in v2 table but retained for backward compatibility and unit tests"
)]
pub(crate) fn parse_vh_file(input_list: Vec<SourceLine>, msg_list: &mut Diagnostics) -> (Option<Vec<Opcode>>, Option<Vec<Macro>>) {
    if input_list.is_empty() {
        return (None, None);
    }

    let mut opcodes: Vec<Opcode> = Vec::new();
    let mut macros: Vec<Macro> = Vec::new();
    let mut section_name = String::default();

    for line in input_list {
        if let Some(section) = line.input_text_line.trim().strip_prefix("///") {
            section.to_owned().trim().clone_into(&mut section_name);
        }

        match opcode_from_string(&line.input_text_line) {
            None => (),
            Some(opcode) => {
                if return_opcode(&opcode.text_name, &opcodes).is_some() {
                    msg_list.error_at(&line.file_name, line.line_counter, format!("Duplicate Opcode {} found", opcode.text_name));
                }
                //opcodes.push(a);
                opcodes.push(Opcode {
                    text_name: opcode.text_name,
                    hex_code: opcode.hex_code,
                    ops: Vec::new(),
                    registers: opcode.registers,
                    variables: opcode.variables,
                    comment: opcode.comment,
                    section: section_name.clone(),
                });
            }
        }
        match macro_from_string(&line.input_text_line, msg_list) {
            None => (),
            Some(found_macro) => {
                if find_macro(&found_macro.name, &macros).is_some() {
                    msg_list.error_at(
                        line.file_name,
                        line.line_counter,
                        format!("Duplicate Macro definition {} found", found_macro.name),
                    );
                }
                macros.push(found_macro);
            }
        }
    }
    (Some(opcodes), Some(macros))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Test import with two registers
    fn test_opcode_from_string1() {
        let input = "16'h01??: t_copy_regs;                              // COPY Copy register";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "COPY".to_owned(),
                hex_code: "000001??".to_owned(),
                ops: Vec::new(),
                registers: 2,
                variables: 0,
                comment: "Copy register".to_owned(),
                section: String::default(),
            })
        );
    }

    #[test]
    // Test import with one argument and one register
    fn test_opcode_from_string2() {
        let input = "16'h086?: t_and_reg_value(w_var1);                  // ANDV AND register with value";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "ANDV".to_owned(),
                hex_code: "0000086?".to_owned(),
                ops: Vec::new(),
                registers: 1,
                variables: 1,
                comment: "AND register with value".to_owned(),
                section: String::default(),
            })
        );
    }

    #[test]
    // Test import with two arguments
    fn test_opcode_from_string3() {
        let input = "16'h0864: t_and_reg_value(w_var1,w_var2);        // MOV Move from addr to addr";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "MOV".to_owned(),
                hex_code: "00000864".to_owned(),
                ops: Vec::new(),
                registers: 0,
                variables: 2,
                comment: "Move from addr to addr".to_owned(),
                section: String::default(),
            })
        );
    }

    #[test]
    // Test no comments
    fn test_opcode_from_string4() {
        let input = "dummy 16'h0864: t_and_reg_value(w_var1,w_var2);";
        let output = opcode_from_string(input);
        assert_eq!(output, None);
    }

    #[test]
    // Test commented out
    fn test_opcode_from_string5() {
        let input = "// 16'h0864: t_and_reg_value(w_var1,w_var2);";
        let output = opcode_from_string(input);
        assert_eq!(output, None);
    }

    #[test]
    // Test import if failed
    fn test_opcode_from_string6() {
        let input = "xxxxx";
        let output = opcode_from_string(input);
        assert_eq!(output, None);
    }

    #[test]
    // Test import if too short
    fn test_opcode_from_string7() {
        let input = "16'h0";
        let output = opcode_from_string(input);
        assert_eq!(output, None);
    }

    #[test]
    // Test import if no space for definition
    fn test_opcode_from_string8() {
        let input = "16'h1234 //abcd";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "abcd".to_owned(),
                hex_code: "00001234".to_owned(),
                ops: Vec::new(),
                registers: 0,
                variables: 0,
                comment: String::default(),
                section: String::default(),
            })
        );
    }

    #[test]
    // Test import with for no comment after the opcode name
    fn test_opcode_from_string9() {
        let input = "16'h0864: t_and_reg_value(w_var1,w_var2);        // MOV";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "MOV".to_owned(),
                hex_code: "00000864".to_owned(),
                ops: Vec::new(),
                registers: 0,
                variables: 2,
                comment: String::default(),
                section: String::default(),
            })
        );
    }

    #[test]
    // Test no macro or opcodes
    fn test_parse_vh_file1() {
        let mut msg_list = Diagnostics::new();
        let vh_list = vec![
            SourceLine {
                input_text_line: "abc/* This is a comment */def".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 1,
            },
            SourceLine {
                input_text_line: "abc/* This is a comment */def".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 2,
            },
        ];

        let (opt_oplist, opt_macro_list) = parse_vh_file(vh_list, &mut msg_list);

        assert_eq!(opt_oplist.unwrap_or_default(), Vec::<Opcode>::new());
        assert_eq!(opt_macro_list.unwrap_or_default(), Vec::<Macro>::new());
    }

    #[test]
    // Test normal macro and opcode
    fn test_parse_vh_file2() {
        let mut msg_list = Diagnostics::new();
        let vh_list = vec![
            SourceLine {
                input_text_line: "$WAIT DELAYV %1 / DELAYV %2 ".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 1,
            },
            SourceLine {
                input_text_line: "16'h05??: t_compare_regs;    // CMPRR Compare registers".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 2,
            },
        ];

        let (opt_oplist, opt_macro_list) = parse_vh_file(vh_list, &mut msg_list);

        assert_eq!(
            opt_oplist.unwrap_or_default(),
            vec![Opcode {
                text_name: "CMPRR".to_owned(),
                hex_code: "000005??".to_owned(),
                ops: Vec::new(),
                registers: 2,
                variables: 0,
                comment: "Compare registers".to_owned(),
                section: String::default(),
            }]
        );
        assert_eq!(
            opt_macro_list.unwrap_or_default(),
            vec![Macro {
                name: "$WAIT".to_owned(),
                variables: 2,
                items: ["DELAYV %1".to_owned(), "DELAYV %2".to_owned()].to_vec(),
                comment: String::default(),
                defined_at: None,
            }]
        );
    }

    #[test]
    // Test duplicate macro
    fn test_parse_vh_file3() {
        let mut msg_list = Diagnostics::new();
        let vh_list = vec![
            SourceLine {
                input_text_line: "$WAIT DELAYV %1 / DELAYV %2 ".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 1,
            },
            SourceLine {
                input_text_line: "16'h05??: t_compare_regs;    // CMPRR Compare registers".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 2,
            },
            SourceLine {
                input_text_line: "$WAIT DELAYV %1 / DELAYV %2 ".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 3,
            },
            SourceLine {
                input_text_line: "16'h05??: t_compare_regs;    // CMPRR Compare registers".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 4,
            },
        ];

        let (_opt_oplist, _opt_macro_list) = parse_vh_file(vh_list, &mut msg_list);

        assert_eq!(msg_list.messages()[0].text(), "Duplicate Macro definition $WAIT found");
        assert_eq!(msg_list.messages()[0].line_number(), Some(3));
        assert_eq!(msg_list.messages()[1].text(), "Duplicate Opcode CMPRR found");
        assert_eq!(msg_list.messages()[1].line_number(), Some(4));
    }

    #[test]
    // Test empty list
    fn test_parse_vh_file4() {
        let mut msg_list = Diagnostics::new();
        let vh_list = vec![];

        let (opt_oplist, opt_macro_list) = parse_vh_file(vh_list, &mut msg_list);

        assert_eq!(opt_oplist, None);
        assert_eq!(opt_macro_list, None);
    }

    #[test]
    // Test normal opcode with sections
    fn test_parse_vh_file5() {
        let mut msg_list = Diagnostics::new();
        let vh_list = vec![
            SourceLine {
                input_text_line: "/// Section 1".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 1,
            },
            SourceLine {
                input_text_line: "16'h06??: t_push_addr;    // PUSH push value to reg".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 2,
            },
            SourceLine {
                input_text_line: "16'h05??: t_compare_regs;    // CMPRR Compare registers".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 3,
            },
            SourceLine {
                input_text_line: "/// Section 2".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 4,
            },
            SourceLine {
                input_text_line: "16'h16??: t_pop_addr;    // POP pop value from reg".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 5,
            },
        ];

        let (opt_oplist, _opt_macro_list) = parse_vh_file(vh_list, &mut msg_list);

        assert_eq!(
            opt_oplist.unwrap_or_default(),
            vec![
                Opcode {
                    text_name: "PUSH".to_owned(),
                    hex_code: "000006??".to_owned(),
                    ops: Vec::new(),
                    registers: 2,
                    variables: 0,
                    comment: "push value to reg".to_owned(),
                    section: "Section 1".to_owned(),
                },
                Opcode {
                    text_name: "CMPRR".to_owned(),
                    hex_code: "000005??".to_owned(),
                    ops: Vec::new(),
                    registers: 2,
                    variables: 0,
                    comment: "Compare registers".to_owned(),
                    section: "Section 1".to_owned(),
                },
                Opcode {
                    text_name: "POP".to_owned(),
                    hex_code: "000016??".to_owned(),
                    ops: Vec::new(),
                    registers: 2,
                    variables: 0,
                    comment: "pop value from reg".to_owned(),
                    section: "Section 2".to_owned(),
                }
            ]
        );
    }

    #[test]
    // Parse 32'h RRR format (three registers, no variable)
    fn test_opcode_from_string_32bit_rrr() {
        let input = "32'h0001_0???: t_addr3;                               // ADDR RRR rd=rs1+rs2";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "ADDR".to_owned(),
                hex_code: "00010???".to_owned(),
                ops: Vec::new(),
                registers: 3,
                variables: 0,
                comment: "RRR rd=rs1+rs2".to_owned(),
                section: String::default(),
            })
        );
    }

    #[test]
    // Parse 32'h RV format (one register, one variable)
    fn test_opcode_from_string_32bit_rv() {
        let input = "32'h0000_080?: t_set_reg(w_var1);                     // SETR RV Set register to a value";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "SETR".to_owned(),
                hex_code: "0000080?".to_owned(),
                ops: Vec::new(),
                registers: 1,
                variables: 1,
                comment: "RV Set register to a value".to_owned(),
                section: String::default(),
            })
        );
    }

    #[test]
    // Parse 32'h V format (no registers, one variable)
    fn test_opcode_from_string_32bit_v() {
        let input = "32'h0000_1000: t_cond_jump(w_var1, 1'b1);             // JMP V Jump";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "JMP".to_owned(),
                hex_code: "00001000".to_owned(),
                ops: Vec::new(),
                registers: 0,
                variables: 1,
                comment: "V Jump".to_owned(),
                section: String::default(),
            })
        );
    }

    #[test]
    // Parse 32'h RR format (two registers, no variable)
    fn test_opcode_from_string_32bit_rr() {
        let input = "32'h0000_0C??: t_load_indexed(w_var1);                // LDIDX RRV first=mem[second+var1]";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "LDIDX".to_owned(),
                hex_code: "00000C??".to_owned(),
                ops: Vec::new(),
                registers: 2,
                variables: 1,
                comment: "RRV first=mem[second+var1]".to_owned(),
                section: String::default(),
            })
        );
    }

    #[test]
    // 32'h line that is commented out returns None
    fn test_opcode_from_string_32bit_commented() {
        let input = "// 32'h0001_0???: t_addr3;                            // ADDR RRR rd=rs1+rs2";
        let output = opcode_from_string(input);
        assert_eq!(output, None);
    }

    #[test]
    // 32'h line that is too short returns None
    fn test_opcode_from_string_32bit_too_short() {
        let input = "32'h0001_0";
        let output = opcode_from_string(input);
        assert_eq!(output, None);
    }
}
