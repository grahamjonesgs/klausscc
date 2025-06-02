use crate::files::LineType;
use crate::labels::{convert_argument, Label};
use crate::macros::{macro_from_string, return_macro, Macro};
use crate::messages::{MessageType, MsgList};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq)]
/// Struct for opcode argument
pub struct InputData {
     /// File name of input file
     pub file_name: String,
    /// Text name of opcode
    pub input: String,
    /// Line number of input file
    pub line_counter: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Struct for opcode
pub struct Opcode {
    /// Comment from opcode definition file
    pub comment: String,
    /// Hexadecimal opcode
    pub hex_code: String,
    /// Number of registers
    pub registers: u32,
    /// Section name from opcode definition file
    pub section: String,
    /// Text name of opcode
    pub text_name: String,
    /// Number of variables
    pub variables: u32,
}


#[cfg(not(tarpaulin_include))]
#[allow(clippy::missing_docs_in_private_items, reason = "Private items are self-explanatory in this context")]
impl Default for &InputData {
    fn default() -> &'static InputData {
        static VALUE: InputData = InputData {
            input: String::new(),
            file_name: String::new(),
            line_counter: 0,
        };
        &VALUE
    }
}

#[derive(Debug)]
/// Struct for Pass0
pub struct Pass0 {
    /// File name of input file
    pub file_name: String,
    /// Line text
    pub input_text_line: String,
    /// Line number of input file
    pub line_counter: u32,
}

#[cfg(not(tarpaulin_include))]
#[allow(clippy::missing_docs_in_private_items, reason = "Private items are self-explanatory in this context")]
impl Default for &Pass0 {
    fn default() -> &'static Pass0 {
        static VALUE: Pass0 = Pass0 {
            input_text_line: String::new(),
            file_name: String::new(),
            line_counter: 0,
        };
        &VALUE
    }
}

#[derive(Debug)]
/// Struct for Pass1
pub struct Pass1 {
     /// File name of input file
     pub file_name: String,
    /// Line text
    pub input_text_line: String,
    /// Line number of input file
    pub line_counter: u32,
    /// Line type
    pub line_type: LineType,
    /// Program counter
    pub program_counter: u32,
    
}

#[cfg(not(tarpaulin_include))]
#[allow(clippy::missing_docs_in_private_items, reason = "Private items are self-explanatory in this context")]
impl Default for &Pass1 {
    fn default() -> &'static Pass1 {
        static VALUE: Pass1 = Pass1 {
            input_text_line: String::new(),
            file_name: String::new(),
            line_counter: 0,
            program_counter: 0,
            line_type: LineType::Blank,
        };
        &VALUE
    }
}

#[derive(Debug, Clone)]
/// Struct for Pass2
pub struct Pass2 {
     /// File name of input file
     pub file_name: String,
    /// Line text
    pub input_text_line: String,
    /// Line number of input file
    pub line_counter: u32,
    /// Line type
    pub line_type: LineType,
     /// Opcode as string
     pub opcode: String,
    /// Program counter
    pub program_counter: u32,
}

#[cfg(not(tarpaulin_include))]
#[allow(clippy::missing_docs_in_private_items, reason = "Private items are self-explanatory in this context")]
impl Default for &Pass2 {
    fn default() -> &'static Pass2 {
        static VALUE: Pass2 = Pass2 {
            input_text_line: String::new(),
            file_name: String::new(),
            line_counter: 0,
            program_counter: 0,
            line_type: LineType::Blank,
            opcode: String::new(),
        };
        &VALUE
    }
}

/// Return opcode with formatted arguments
///
/// Returns the hex code argument from the line, converting arguments from decimal to 8 digit hex values
/// Converts label names to hex addresses
pub fn add_arguments(
    opcodes: &mut Vec<Opcode>,
    line: &String,
    msg_list: &mut MsgList,
    line_number: u32,
    filename: &str,
    labels: &mut Vec<Label>,
) -> String {
    let num_registers = num_registers(opcodes, &line.to_uppercase()).unwrap_or(0);
    let num_arguments = num_arguments(opcodes, &line.to_uppercase()).unwrap_or(0);
    let mut arguments = String::default();

    let words = line.split_whitespace();
    #[allow(clippy::arithmetic_side_effects, reason = "Arithmetic side effects are intentional and safe in this context")]
    for (i, word) in words.enumerate() {
        if (i == num_registers as usize + 1) && ((num_arguments == 1) || (num_arguments == 2)) {
            arguments.push_str(&{
                let this = convert_argument(
                    &word.to_owned().to_uppercase(),
                    msg_list,
                    line_number,
                    filename.to_owned(),
                    labels,
                );
                //let default = "00000000".to_owned();
                this.unwrap_or_else(|| "00000000".to_owned())
            });
        }
        if i == num_registers as usize + 2 && num_arguments == 2 {
            arguments.push_str(&{
                let this = convert_argument(
                    &word.to_owned().to_uppercase(),
                    msg_list,
                    line_number,
                    filename.to_owned(),
                    labels,
                );
                this.unwrap_or_else(|| "00000000".to_owned())
            });
        }
        if i > num_registers as usize + num_arguments as usize {
            msg_list.push(
                format!("Too many arguments found - \"{line}\""),
                Some(line_number),
                Some((filename).to_owned()),
                MessageType::Warning,
            );
        }
    }

    // Can't be in tarpaulin as we can't test the error by passing wrong size
    #[allow(clippy::arithmetic_side_effects, reason = "Arithmetic side effects are intentional and safe in this context")]
    if arguments.len() != 8 * num_arguments as usize {
        #[cfg(not(tarpaulin_include))]
        msg_list.push(
            format!("Incorrect argument definition - \"{line}\""),
            Some(line_number),
            Some(filename.to_owned()),
            MessageType::Error,
        );
    }
    arguments
}

/// Updates opcode with register
///
/// Returns the hex code operand from the line, adding register values
#[allow(clippy::arithmetic_side_effects, reason = "Arithmetic side effects are intentional and safe in this context")]
pub fn add_registers(
    opcodes: &mut Vec<Opcode>,
    line: &String,
    filename: String,
    msg_list: &mut MsgList,
    line_number: u32,
) -> String {
    let num_registers = num_registers(opcodes, &(*line).to_uppercase()).unwrap_or(0);

    let mut opcode_found = {
        let this = return_opcode(&line.to_uppercase(), opcodes);
        this.unwrap_or_default()
    };

    if opcode_found.len() != 8 {
        msg_list.push(
            format!("Incorrect register definition - \"{line}\""),
            Some(line_number),
            Some(filename),
            MessageType::Error,
        );
        return "ERR     ".to_owned();
    }

    let cloned_opcode_found = opcode_found
        .get(..(8 - num_registers) as usize)
        .unwrap_or("")
        .to_owned();
    opcode_found.clear();
    opcode_found.push_str(&cloned_opcode_found);

    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        if (i == 2 && num_registers == 2) || (i == 1 && (num_registers == 2 || num_registers == 1))
        {
            opcode_found.push_str(&map_reg_to_hex(word));
        }
    }

    if opcode_found.len() != 8 || opcode_found.contains('X') {
        msg_list.push(
            format!("Incorrect register definition - \"{line}\""),
            Some(line_number),
            Some(filename),
            MessageType::Error,
        );
        return "ERR     ".to_owned();
    }
    opcode_found
}

/// Register name to hex
///
/// Map the register to the hex code for the opcode
fn map_reg_to_hex(input: &str) -> String {
    match input.to_uppercase().as_str() {
        "A" => "0".to_owned(),
        "B" => "1".to_owned(),
        "C" => "2".to_owned(),
        "D" => "3".to_owned(),
        "E" => "4".to_owned(),
        "F" => "5".to_owned(),
        "G" => "6".to_owned(),
        "H" => "7".to_owned(),
        "I" => "8".to_owned(),
        "J" => "9".to_owned(),
        "K" => "A".to_owned(),
        "L" => "B".to_owned(),
        "M" => "C".to_owned(),
        "N" => "D".to_owned(),
        "O" => "E".to_owned(),
        "P" => "F".to_owned(),
        _ => "X".to_owned(),
    }
}

/// Returns number of args for opcode
///
/// From opcode name, option of number of arguments for opcode, or None
pub fn num_arguments(opcodes: &mut Vec<Opcode>, line: &str) -> Option<u32> {
    for opcode in opcodes {
        let mut words = line.split_whitespace();
        let first_word = words.next().unwrap_or("");
        if first_word.to_uppercase() == opcode.text_name {
            return Some(opcode.variables);
        }
    }
    None
}

/// Returns number of registers for opcode
///
/// From opcode name, option of number of registers for opcode, or None
fn num_registers(opcodes: &mut Vec<Opcode>, line: &str) -> Option<u32> {
    for opcode in opcodes {
        let mut words = line.split_whitespace();
        let first_word = words.next().unwrap_or("");
        if first_word.is_empty() {
            return None;
        }
        if first_word == opcode.text_name {
            return Some(opcode.registers);
        }
    }
    None
}

/// Parse opcode definition line to opcode
///
/// Receive a line from the opcode definition file and if possible parse of Some(Opcode), or None
#[allow(clippy::useless_let_if_seq, reason = "Needed for compatibility with generated code or macro expansion")]
#[allow(clippy::arithmetic_side_effects, reason = "Needed for compatibility with generated code or macro expansion")]
pub fn opcode_from_string(input_line: &str) -> Option<Opcode> {
    let pos_comment: usize;
    let pos_end_comment: usize;
    let line_pos_opcode: usize;

    // Find the opcode if it exists
    let pos_opcode: usize = match input_line.find("16'h") {
        None => return None,
        Some(location) => {
            line_pos_opcode = location;
            location + 4
        }
    };

    // check if the line was commented out
    match input_line.find("//") {
        None => {}
        Some(location) => {
            if location < line_pos_opcode {
                return None;
            }
        }
    }

    // Check for length of opcode
    if input_line.len() < (pos_opcode + 4) {
        return None;
    }

    // Define number of registers from opcode definition

    let mut num_registers: u32 = 0;
    if input_line.get(pos_opcode + 3..pos_opcode + 4) == Some("?") {
        num_registers = 1;
    }

    if input_line.get(pos_opcode + 2..pos_opcode + 4) == Some("??") {
        num_registers = 2;
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
        None => match input_line.find("//") {
            None => return None,
            Some(location) => location + 2,
        },
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
        hex_code: format!(
            "0000{}",
            &input_line.get(pos_opcode..pos_opcode + 4).unwrap_or("    ")
        ),
        registers: num_registers,
        variables: num_variables,
        comment: input_line
            .get(pos_comment..pos_end_comment)
            .unwrap_or("")
            .to_owned(),
        text_name: input_line
            .get(pos_name..pos_end_name)
            .unwrap_or("")
            .to_owned(),
        section: String::default(),
    })
}

/// Parse file to opcode and macro vectors
///
/// Parses the .vh verilog file, creates two vectors of macro and opcode, returning None, None or Some(Opcode), Some(Macro)
pub fn parse_vh_file(
    input_list: Vec<InputData>,
    msg_list: &mut MsgList,
) -> (Option<Vec<Opcode>>, Option<Vec<Macro>>) {
    if input_list.is_empty() {
        return (None, None);
    }

    let mut opcodes: Vec<Opcode> = Vec::new();
    let mut macros: Vec<Macro> = Vec::new();
    let mut section_name = String::default();

    for line in input_list {
        if let Some(section) = line.input.trim().strip_prefix("///") {
            section.to_owned().trim().clone_into(&mut section_name);
        }

        match opcode_from_string(&line.input) {
            None => (),
            Some(opcode) => {
                if return_opcode(&opcode.text_name, &mut opcodes).is_some() {
                    msg_list.push(
                        format!("Duplicate Opcode {} found", opcode.text_name),
                        Some(line.line_counter),
                        Some(line.file_name.clone()),
                        MessageType::Error,
                    );
                }
                //opcodes.push(a);
                opcodes.push(Opcode {
                    text_name: opcode.text_name,
                    hex_code: opcode.hex_code,
                    registers: opcode.registers,
                    variables: opcode.variables,
                    comment: opcode.comment,
                    section: section_name.clone(),
                });
            }
        }
        match macro_from_string(&line.input, msg_list) {
            None => (),
            Some(found_macro) => {
                if return_macro(&found_macro.name, &mut macros).is_some() {
                    msg_list.push(
                        format!("Duplicate Macro definition {} found", found_macro.name),
                        Some(line.line_counter),
                        Some(line.file_name),
                        MessageType::Error,
                    );
                }
                macros.push(found_macro);
            }
        }
    }
    (Some(opcodes), Some(macros))
}

/// Returns hex opcode from name
///
/// Checks if first word is opcode and if so returns opcode hex value
pub fn return_opcode(line: &str, opcodes: &mut Vec<Opcode>) -> Option<String> {
    for opcode in opcodes {
        let mut words = line.split_whitespace();
        let first_word = words.next().unwrap_or("");
        if first_word.to_uppercase() == opcode.text_name {
            return Some(opcode.hex_code.to_uppercase());
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::arbitrary_source_item_ordering, reason = "Test functions are intentionally not ordered")]
mod tests {
    use super::*;
    use crate::labels;

    #[test]
    // Test that the correct number of registers is returned
    fn test_num_registers1() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            comment: String::default(),
            variables: 0,
            registers: 1,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, Some(1));
    }

    #[test]
    // Test that the None is returned if the opcode is not found
    fn test_num_registers2() {
        let input = String::from("PULL");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            comment: String::default(),
            variables: 0,
            registers: 1,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, None);
    }
    #[test]
    // Test that the correct number of arguments is returned
    fn test_num_arguments1() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            comment: String::default(),
            variables: 0,
            registers: 1,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, Some(1));
    }
    #[test]
    // Test that the correct number of arguments is returned
    fn test_num_arguments2() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            comment: String::default(),
            variables: 0,
            registers: 2,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, Some(2));
    }

    #[test]
    // Test that the correct number of arguments is returned 2 variable 2 registers
    fn test_num_arguments3() {
        let input = String::from("PUSH ddd yyy");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            comment: String::default(),
            variables: 2,
            registers: 2,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, Some(2));
    }

    #[test]
    // Test that None is returned if the opcode is not found
    fn test_num_arguments4() {
        let input = String::from("PUSH2");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            comment: String::default(),
            variables: 0,
            registers: 2,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, None);
    }

    #[test]
    // Test that None is returned if the opcode is blank
    fn test_num_arguments5() {
        let input = String::default();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            comment: String::default(),
            variables: 0,
            registers: 2,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, None);
    }

    #[test]
    // Test that the correct opcode is returned
    fn test_return_opcode1() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            comment: String::default(),
            variables: 0,
            registers: 2,
            section: String::default(),
        });
        let output = return_opcode(&input, opcodes);
        assert_eq!(output, Some(String::from("1234")));
    }

    #[test]
    // Test that None is returned if the opcode is not found
    fn test_return_opcode2() {
        let input = String::from("PUSH2");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            comment: String::default(),
            variables: 0,
            registers: 2,
            section: String::default(),
        });
        let output = return_opcode(&input, opcodes);
        assert_eq!(output, None);
    }

    #[test]
    // This test is to check that the function will return correct output if the number of registers is correct
    fn test_add_registers1() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH A B");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("000056XX"),
            comment: String::default(),
            variables: 0,
            registers: 2,
            section: String::default(),
        });
        let output = add_registers(opcodes, &input, "test".to_owned(), &mut msg_list, 1);
        assert_eq!(output, String::from("00005601"));
    }

    #[test]
    // This test is to check that the function will return an error if the number of registers is incorrect
    fn test_add_registers2() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH A B");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("000056XX"),
            comment: String::default(),
            variables: 0,
            registers: 1,
            section: String::default(),
        });
        let output = add_registers(opcodes, &input, "test".to_owned(), &mut msg_list, 1);
        assert_eq!(output, String::from("ERR     "));
    }
    #[test]
    // This test is to check that the function will return an error if the length of the opcode is not correct
    fn test_add_registers3() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH A B");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("000056X"),
            comment: String::default(),
            variables: 0,
            registers: 1,
            section: String::default(),
        });
        let output = add_registers(opcodes, &input, "test".to_owned(), &mut msg_list, 1);
        assert_eq!(output, String::from("ERR     "));
    }
    #[test]
    // Test single hex argument
    fn test_add_arguments1() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH 0xFFFF");
        let mut labels = Vec::<labels::Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("00000000"),
            comment: String::default(),
            variables: 1,
            registers: 0,
            section: String::default(),
        });
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &mut labels);
        assert_eq!(output, String::from("0000FFFF"));
    }

    #[test]
    // Test single decimal argument
    fn test_add_arguments2() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH 1234");
        let mut labels = Vec::<labels::Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("00000000"),
            comment: String::default(),
            variables: 1,
            registers: 0,
            section: String::default(),
        });
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &mut labels);
        assert_eq!(output, String::from("000004D2"));
    }

    #[test]
    // Test invalid argument
    fn test_add_arguments3() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH HELLO");
        let mut labels = Vec::<labels::Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("00000000"),
            comment: String::default(),
            variables: 1,
            registers: 0,
            section: String::default(),
        });
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &mut labels);
        assert_eq!(output, String::from("00000000"));
    }

    #[test]
    // Test invalid second argument
    fn test_add_arguments4() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH 0xF RRR");
        let mut labels = Vec::<labels::Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("00000000"),
            comment: String::default(),
            variables: 2,
            registers: 0,
            section: String::default(),
        });
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &mut labels);
        assert_eq!(output, String::from("0000000F00000000"));
    }

    #[test]
    // Test two arguments
    fn test_add_arguments5() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH 1 0xF");
        let mut labels = Vec::<labels::Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("00000000"),
            comment: String::default(),
            variables: 2,
            registers: 0,
            section: String::default(),
        });
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &mut labels);
        assert_eq!(output, String::from("000000010000000F"));
    }

    #[test]
    // Test too many arguments
    fn test_add_arguments6() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH 1 0xF");
        let mut labels = Vec::<labels::Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("00000000"),
            comment: String::default(),
            variables: 1,
            registers: 0,
            section: String::default(),
        });
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &mut labels);
        assert_eq!(output, String::from("00000001"));
        assert_eq!(
            msg_list.list.first().unwrap_or_default().text,
            "Too many arguments found - \"PUSH 1 0xF\""
        );
    }

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
        let input =
            "16'h086?: t_and_reg_value(w_var1);                  // ANDV AND register with value";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "ANDV".to_owned(),
                hex_code: "0000086?".to_owned(),
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
        let input =
            "16'h0864: t_and_reg_value(w_var1,w_var2);        // MOV Move from addr to addr";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "MOV".to_owned(),
                hex_code: "00000864".to_owned(),
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
                registers: 0,
                variables: 2,
                comment: String::default(),
                section: String::default(),
            })
        );
    }

    #[test]
    fn test_map_reg_to_hex() {
        assert_eq!(map_reg_to_hex("B"), "1");
        assert_eq!(map_reg_to_hex("P"), "F");
        assert_eq!(map_reg_to_hex("Z"), "X");
    }

    #[test]
    // Test no macro or opcodes
    fn test_parse_vh_file1() {
        let mut msg_list = MsgList::new();
        let vh_list = vec![
            InputData {
                input: "abc/* This is a comment */def".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "abc/* This is a comment */def".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 2,
            },
        ];

        let (opt_oplist, opt_macro_list) = parse_vh_file(vh_list, &mut msg_list);

        assert!(opt_oplist.unwrap_or_default().is_empty());
        assert!(opt_macro_list.unwrap_or_default().is_empty());
    }

    #[test]
    // Test normal macro and opcode
    fn test_parse_vh_file2() {
        let mut msg_list = MsgList::new();
        let vh_list = vec![
            InputData {
                input: "$WAIT DELAYV %1 / DELAYV %2 ".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "16'h05??: t_compare_regs;    // CMPRR Compare registers".to_owned(),
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
                comment: String::default()
            }]
        );
    }

    #[test]
    // Test duplicate macro
    fn test_parse_vh_file3() {
        let mut msg_list = MsgList::new();
        let vh_list = vec![
            InputData {
                input: "$WAIT DELAYV %1 / DELAYV %2 ".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "16'h05??: t_compare_regs;    // CMPRR Compare registers".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 2,
            },
            InputData {
                input: "$WAIT DELAYV %1 / DELAYV %2 ".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 3,
            },
            InputData {
                input: "16'h05??: t_compare_regs;    // CMPRR Compare registers".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 4,
            },
        ];

        let (_opt_oplist, _opt_macro_list) = parse_vh_file(vh_list, &mut msg_list);

        assert_eq!(
            msg_list.list.first().unwrap_or_default().text,
            "Duplicate Macro definition $WAIT found"
        );
        assert_eq!(
            msg_list.list.first().unwrap_or_default().line_number,
            Some(3)
        );
        assert_eq!(
            msg_list.list.get(1).unwrap_or_default().text,
            "Duplicate Opcode CMPRR found"
        );
        assert_eq!(
            msg_list.list.get(1).unwrap_or_default().line_number,
            Some(4)
        );
    }
    #[test]
    // Test empty list
    fn test_parse_vh_file4() {
        let mut msg_list = MsgList::new();
        let vh_list = vec![];

        let (opt_oplist, opt_macro_list) = parse_vh_file(vh_list, &mut msg_list);

        assert_eq!(opt_oplist, None);
        assert_eq!(opt_macro_list, None);
    }

    #[test]
    // Test normal opcode with sections
    fn test_parse_vh_file5() {
        let mut msg_list = MsgList::new();
        let vh_list = vec![
            InputData {
                input: "/// Section 1".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "16'h06??: t_push_addr;    // PUSH push value to reg".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 2,
            },
            InputData {
                input: "16'h05??: t_compare_regs;    // CMPRR Compare registers".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 3,
            },
            InputData {
                input: "/// Section 2".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 4,
            },
            InputData {
                input: "16'h16??: t_pop_addr;    // POP push value to reg".to_owned(),
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
                    registers: 2,
                    variables: 0,
                    comment: "push value to reg".to_owned(),
                    section: "Section 1".to_owned(),
                },
                Opcode {
                    text_name: "CMPRR".to_owned(),
                    hex_code: "000005??".to_owned(),
                    registers: 2,
                    variables: 0,
                    comment: "Compare registers".to_owned(),
                    section: "Section 1".to_owned(),
                },
                Opcode {
                    text_name: "POP".to_owned(),
                    hex_code: "000016??".to_owned(),
                    registers: 2,
                    variables: 0,
                    comment: "push value to reg".to_owned(),
                    section: "Section 2".to_owned(),
                }
            ]
        );
    }
}
