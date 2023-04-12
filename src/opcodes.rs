use crate::files::LineType;
use crate::labels::convert_argument;
use crate::labels::Label;
use crate::messages::{MessageType, MsgList};
use core::fmt;

#[derive(Clone, Debug, PartialEq)]
pub struct Opcode {
    pub name: String,
    pub opcode: String,
    pub registers: u32,
    pub variables: u32,
    pub comment: String,
}

impl fmt::Display for Opcode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{} {}, registers {}, variables {} - {}",
            self.name, self.opcode, self.registers, self.variables, self.comment
        )
    }
}


#[derive(Debug)]
pub struct Pass0 {
    pub input: String,
    pub line_counter: u32,
}

#[derive(Debug)]
pub struct Pass1 {
    pub input: String,
    pub line_counter: u32,
    pub program_counter: u32,
    pub line_type: LineType,
}

#[derive(Debug)]
pub struct Pass2 {
    pub input: String,
    pub line_counter: u32,
    pub program_counter: u32,
    pub line_type: LineType,
    pub opcode: String,
}
/// Parse opcode definition line to opcode
///
/// Receive a line from the opcode definition file and if possible parse of Some(Opcode), or None
pub fn opcode_from_string(input_line: &str) -> Option<Opcode> {
    let pos_comment: usize;
    let pos_end_comment: usize;
    let line_pos_opcode: usize;

    // Find the opcode if it exists
    let pos_opcode: usize = match input_line.find("16'h") {
        None => return None,
        Some(a) => {
            line_pos_opcode = a;
            a + 4
        }
    };

    // check if the line was commented out
    match input_line.find("//") {
        None => {}
        Some(a) => {
            if a < line_pos_opcode {
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
    if &input_line[pos_opcode + 3..pos_opcode + 4] == "?" {
        num_registers = 1;
    }
    if &input_line[pos_opcode + 2..pos_opcode + 4] == "??" {
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
    let pos_name: usize = match input_line.find("//") {
        None => return None,
        Some(a) => a + 3,
    };

    // Find end of first word after comment as end of opcode name
    let pos_end_name: usize = match input_line[pos_name..].find(' ') {
        None => return None,
        Some(a) => a + pos_name,
    };

    // Set comments filed, or none if missing
    if input_line.len() > pos_end_name + 1 {
        pos_comment = pos_end_name + 1;
        pos_end_comment = input_line.len();
    } else {
        pos_comment = 0;
        pos_end_comment = 0;
    }

    Some(Opcode {
        opcode: format!(
            "0000{}",
            &input_line[pos_opcode..pos_opcode + 4].to_string()
        ),
        registers: num_registers,
        variables: num_variables,
        comment: input_line[pos_comment..pos_end_comment].to_string(),
        name: input_line[pos_name..pos_end_name].to_string(),
    })
}

/// Returns number of args for opcode
///
/// From opcode name, option of number of arguments for opcode, or None
pub fn num_arguments(opcodes: &mut Vec<Opcode>, line: &mut str) -> Option<u32> {
    for opcode in opcodes {
        let mut words = line.split_whitespace();
        let first_word = words.next().unwrap_or("");
        if first_word.is_empty() {
            return None;
        }
        if first_word.to_uppercase() == opcode.name {
            return Some(opcode.variables);
        }
    }
    None
}

/// Returns hex opcode from name
///
/// Checks if first word is opcode and if so returns opcode hex value
pub fn return_opcode(line: &str, opcodes: &mut Vec<Opcode>) -> Option<String> {
    for opcode in opcodes {
        let mut words = line.split_whitespace();
        let first_word = words.next().unwrap_or("");
        if first_word.to_uppercase() == opcode.name {
            return Some(opcode.opcode.to_string().to_uppercase());
        }
    }
    None
}

//// Returns number of registers for opcode
///
/// From opcode name, option of number of registers for opcode, or None
fn num_registers(opcodes: &mut Vec<Opcode>, line: &mut str) -> Option<u32> {
    for opcode in opcodes {
        let mut words = line.split_whitespace();
        let first_word = words.next().unwrap_or("");
        if first_word.is_empty() {
            return None;
        }
        if first_word == opcode.name {
            return Some(opcode.registers);
        }
    }
    None
}
/// Register name to hex
///
/// Map the register to the hex code for the opcode
fn map_reg_to_hex(input: &str) -> String {
    match input.to_uppercase().as_str() {
        "A" => "0".to_string(),
        "B" => "1".to_string(),
        "C" => "2".to_string(),
        "D" => "3".to_string(),
        "E" => "4".to_string(),
        "F" => "5".to_string(),
        "G" => "6".to_string(),
        "H" => "7".to_string(),
        "I" => "8".to_string(),
        "J" => "9".to_string(),
        "K" => "A".to_string(),
        "L" => "B".to_string(),
        "M" => "C".to_string(),
        "N" => "D".to_string(),
        "O" => "E".to_string(),
        "P" => "F".to_string(),
        _ => "X".to_string(),
    }
}

/// Updates opcode with register
///
/// Returns the hex code operand from the line, adding register values
pub fn add_registers(
    opcodes: &mut Vec<Opcode>,
    line: &mut String,
    msg_list: &mut MsgList,
    line_number: u32,
) -> String {
    let num_registers =
        num_registers(opcodes, &mut (*line).to_string().to_uppercase()).unwrap_or(0);

    let mut opcode_found = {
        let this = return_opcode(&line.to_uppercase(), opcodes);
        let default = String::new();
        match this {
            Some(x) => x,
            None => default,
        }
    };

    if opcode_found.len() != 8  {
        msg_list.push(
            format!("Incorrect register definition - \"{line}\""),
            Some(line_number),
            MessageType::Warning,
        );
        return "ERR     ".to_string();
    }

    opcode_found = opcode_found[..(8 - num_registers) as usize].to_string();
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
            MessageType::Warning,
        );
        return "ERR     ".to_string();
    }
    opcode_found
}

/// Return opcode with formatted arguments
///
/// Returns the hex code argument from the line, converting arguments from decimal to 8 digit hex values
/// Converts label names to hex addresses
pub fn add_arguments(
    opcodes: &mut Vec<Opcode>,
    line: &mut String,
    msg_list: &mut MsgList,
    line_number: u32,
    labels: &mut Vec<Label>,
) -> String {
    let num_registers = num_registers(opcodes, &mut line.to_uppercase()).unwrap_or(0);
    let num_arguments = num_arguments(opcodes, &mut line.to_uppercase()).unwrap_or(0);
    let mut arguments = String::new();

    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        if (i == num_registers as usize + 1) && ((num_arguments == 1) || (num_arguments == 2)) {
            arguments.push_str(&{
                let this = convert_argument(
                    &word.to_string().to_uppercase(),
                    msg_list,
                    line_number,
                    labels,
                );
                let default = "00000000".to_string();
                match this {
                    Some(x) => x,
                    None => default,
                }
            });
        }
        if i == num_registers as usize + 2 && num_arguments == 2 {
            arguments.push_str(&{
                let this = convert_argument(
                    &word.to_string().to_uppercase(),
                    msg_list,
                    line_number,
                    labels,
                );
                let default = "00000000".to_string();
                match this {
                    Some(x) => x,
                    None => default,
                }
            });
        }
        if i > num_registers as usize + num_arguments as usize {
            msg_list.push(
                format!("Too many arguments found - \"{line}\""),
                Some(line_number),
                MessageType::Warning,
            );
        }
    }

    if arguments.len() != 8 * num_arguments as usize {
        msg_list.push(
            format!("Incorrect argument definition - \"{line}\""),
            Some(line_number),
            MessageType::Error,
        );
    }
    arguments
}

#[cfg(test)]
mod tests {
    use crate::{opcodes::{Opcode, num_registers, return_opcode, add_registers, add_arguments, opcode_from_string}, labels, messages::print_messages};

    #[test]
    // Test that the correct number of registers is returned
    fn test_num_registers1() {
        let mut input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            name: String::from("PUSH"),
            opcode: String::from("1234"),
            comment: String::new(),
            variables: 0,
            registers: 1,
        });
        let output = num_registers(opcodes, &mut input);
        assert_eq!(output, Some(1));
    }

    #[test]
    // Test that the None is returned if the opcode is not found
    fn test_num_registers2() {
        let mut input = String::from("PULL");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            name: String::from("PUSH"),
            opcode: String::from("1234"),
            comment: String::new(),
            variables: 0,
            registers: 1,
        });
        let output = num_registers(opcodes, &mut input);
        assert_eq!(output, None);
    }
    #[test]
    // Test that the correct number of arguments is returned
    fn test_num_arguments1() {
        let mut input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            name: String::from("PUSH"),
            opcode: String::from("1234"),
            comment: String::new(),
            variables: 0,
            registers: 1,
        });
        let output = num_registers(opcodes, &mut input);
        assert_eq!(output, Some(1));
    }
    #[test]
    // Test that the correct number of arguments is returned
    fn test_num_arguments2() {
        let mut input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            name: String::from("PUSH"),
            opcode: String::from("1234"),
            comment: String::new(),
            variables: 0,
            registers: 2,
        });
        let output = num_registers(opcodes, &mut input);
        assert_eq!(output, Some(2));
    }

    #[test]
    // Test that None is returned if the opcode is not found
    fn test_num_arguments3() {
        let mut input = String::from("PUSH2");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            name: String::from("PUSH"),
            opcode: String::from("1234"),
            comment: String::new(),
            variables: 0,
            registers: 2,
        });
        let output = num_registers(opcodes, &mut input);
        assert_eq!(output, None);
    }

    #[test]
    // Test that the correct opcode is returned
    fn test_return_opcode1() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            name: String::from("PUSH"),
            opcode: String::from("1234"),
            comment: String::new(),
            variables: 0,
            registers: 2,
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
            name: String::from("PUSH"),
            opcode: String::from("1234"),
            comment: String::new(),
            variables: 0,
            registers: 2,
        });
        let output = return_opcode(&input, opcodes);
        assert_eq!(output, None);
    }

    #[test]
    // This test is to check that the function will return correct output if the number of registers is correct
    fn test_add_registers1() {
        let mut msg_list = crate::messages::MsgList::new();
        let mut input = String::from("PUSH A B");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            name: String::from("PUSH"),
            opcode: String::from("000056XX"),
            comment: String::new(),
            variables: 0,
            registers: 2,
        });
        let output = add_registers(opcodes, &mut input, &mut msg_list,1);
        assert_eq!(output, String::from("00005601"));
    }
    
    #[test]
    // This test is to check that the function will return an error if the number of registers is incorrect
    fn test_add_registers2() {
        let mut msg_list = crate::messages::MsgList::new();
        let mut input = String::from("PUSH A B");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            name: String::from("PUSH"),
            opcode: String::from("000056XX"),
            comment: String::new(),
            variables: 0,
            registers: 1,
        });
        let output = add_registers(opcodes, &mut input, &mut msg_list,1);
        assert_eq!(output, String::from("ERR     "));
    }
    #[test]
    // This test is to check that the function will return an error if the length of the opcode is not correct
    fn test_add_registers3() {
        let mut msg_list = crate::messages::MsgList::new();
        let mut input = String::from("PUSH A B");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            name: String::from("PUSH"),
            opcode: String::from("000056X"),
            comment: String::new(),
            variables: 0,
            registers: 1,
        });
        let output = add_registers(opcodes, &mut input, &mut msg_list,1);
        assert_eq!(output, String::from("ERR     "));
    }
    #[test]
    // Test single hex argument
    fn test_add_arguments1() {
        let mut msg_list = crate::messages::MsgList::new();
        let mut input = String::from("PUSH 0xFFFF");
        let mut labels = Vec::<labels::Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            name: String::from("PUSH"),
            opcode: String::from("00000000"),
            comment: String::new(),
            variables: 1,
            registers: 0,
        });
        let output = add_arguments(opcodes, &mut input, &mut msg_list,1,&mut labels);
        print_messages(&mut msg_list);
        assert_eq!(output, String::from("0000FFFF"));
    }

    #[test]
    // Test single decimal argument
    fn test_add_arguments2() {
        let mut msg_list = crate::messages::MsgList::new();
        let mut input = String::from("PUSH 1234");
        let mut labels = Vec::<labels::Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            name: String::from("PUSH"),
            opcode: String::from("00000000"),
            comment: String::new(),
            variables: 1,
            registers: 0,
        });
        let output = add_arguments(opcodes, &mut input, &mut msg_list,1,&mut labels);
        print_messages(&mut msg_list);
        assert_eq!(output, String::from("000004D2"));
    }

    #[test]
    // Test invald argument
    fn test_add_arguments3() {
        let mut msg_list = crate::messages::MsgList::new();
        let mut input = String::from("PUSH HELLO");
        let mut labels = Vec::<labels::Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            name: String::from("PUSH"),
            opcode: String::from("00000000"),
            comment: String::new(),
            variables: 1,
            registers: 0,
        });
        let output = add_arguments(opcodes, &mut input, &mut msg_list,1,&mut labels);
        print_messages(&mut msg_list);
        assert_eq!(output, String::from("00000000"));
    }

    #[test]
    // Test two arguments
    fn test_add_arguments4() {
        let mut msg_list = crate::messages::MsgList::new();
        let mut input = String::from("PUSH 1 0xF");
        let mut labels = Vec::<labels::Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            name: String::from("PUSH"),
            opcode: String::from("00000000"),
            comment: String::new(),
            variables: 2,
            registers: 0,
        });
        let output = add_arguments(opcodes, &mut input, &mut msg_list,1,&mut labels);
        print_messages(&mut msg_list);
        assert_eq!(output, String::from("000000010000000F"));
    }

    #[test]
    // Test import with two registers
    fn test_opcode_from_string1() {
        let input = "16'h01??: t_copy_regs;                              // COPY Copy register";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                name: "COPY".to_string(),
                opcode: "000001??".to_string(),
                registers: 2,
                variables: 0,
                comment: "Copy register".to_string()
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
                name: "ANDV".to_string(),
                opcode: "0000086?".to_string(),
                registers: 1,
                variables: 1,
                comment: "AND register with value".to_string()
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
                name: "MOV".to_string(),
                opcode: "00000864".to_string(),
                registers: 0,
                variables: 2,
                comment: "Move from addr to addr".to_string()
            })
        );
    }

    #[test]
    // Test import if failed
    fn test_opcode_from_string4() {
        let input = "xxxxx";
        let output = opcode_from_string(input);
        assert_eq!(output, None);
    }


}
