use crate::files::{Label, LineType, Opcode, Pass2};
use crate::messages::{MessageType, MsgList};


/// Extracts label from string
///
/// Checks if end of first word is colon if so return label as option string
pub fn label_name_from_string(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");
    if first_word.ends_with(':') {
        return Some(first_word.to_string());
    }
    None
}

/// Extracts data name from string
///
/// Checks if start of first word is hash if so return data name as option string
pub fn data_name_from_string(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");
    if first_word.starts_with('#') {
        return Some(first_word.to_string());
    }
    None
}



/// Return program counter for label
///
/// Return option of program counter for label if it exists, or None
pub fn return_label_value(line: &str, labels: &mut Vec<Label>) -> Option<u32> {
    for label in labels {
        if label.name == line {
            return Some(label.program_counter);
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

/// Return number of bytes of data
///
/// From instruction name, option of number of bytes of data, or 0 is error
pub fn num_data_bytes(line: &str, msg_list: &mut MsgList, line_number: u32) -> u32 {
    match data_as_bytes(line) {
        Some(data) => data.len().try_into().unwrap(),
        None => {
            msg_list.push(
                format!("Error in data definition for {line}"),
                Some(line_number),
                MessageType::Error,
            );
            0
        }
    }
}

/// Returns bytes for data element
///
/// Parses data element and returns data as bytes, or None if error
pub fn data_as_bytes(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");
    if first_word.is_empty() {
        return None;
    }

    let second_word = words.next().unwrap_or("");
    if second_word.is_empty() {
        return None;
    }

    // Check if next word starts with quote
    if second_word.starts_with('\"') {
        let remaining_line = line.trim_start_matches(first_word).trim();

        if remaining_line.starts_with('\"') && remaining_line.ends_with('\"') {
            let output = remaining_line.trim_matches('\"').to_string();
            let mut output_hex = String::new();
            for c in output.as_bytes() {
                let hex = format!("{c:02X}");
                output_hex.push_str(&hex);
                output_hex.push_str("000000");
            }
            output_hex.push_str("00000000"); // Add null terminator

            Some(output_hex)
        } else {
            None
        }
    } else {
        // Check if next word is a number
        // let int_value: i64;
        let int_value = if second_word.len() >= 2
            && (second_word[0..2] == *"0x" || second_word[0..2] == *"0X")
        {
            let without_prefix = second_word.trim_start_matches("0x");
            let without_prefix = without_prefix.trim_start_matches("0X");
            let int_value_result = i64::from_str_radix(without_prefix, 16);
            int_value_result.unwrap_or(0)
        } else {
            let int_value_result = second_word.parse::<i64>();
            int_value_result.unwrap_or(0)
        };

        if int_value == 0 {
            None
        } else {
            let mut data = String::new();
            for _ in 0..int_value {
                data.push_str("00000000");
            }
            Some(data)
        }
    }
}

//// Returns number of registers for opcode
///
/// From opcode name, option of number of registers for opcode, or None
pub fn num_registers(opcodes: &mut Vec<Opcode>, line: &mut str) -> Option<u32> {
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

/// Returns enum of type of line
///
/// Given a code line, will returns if line is Label, Opcode, Blank, Comment or Error
pub fn line_type(opcodes: &mut Vec<Opcode>, line: &mut str) -> LineType {
    if label_name_from_string(line).is_some() {
        return LineType::Label;
    };
    if data_name_from_string(line).is_some() {
        return LineType::Data;
    };
    if return_opcode(line, opcodes).is_some() {
        return LineType::Opcode;
    }
    if is_blank(line) {
        return LineType::Blank;
    }
    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        if is_comment(&mut word.to_string()) && i == 0 {
            return LineType::Comment;
        }
    }
    LineType::Error
}

/// Check if line is valid
///  
/// Returns true if line is not error
pub fn is_valid_line(opcodes: &mut Vec<Opcode>, line: String) -> bool {
    let mut temp_line: String = line;
    if line_type(opcodes, &mut temp_line) == LineType::Error {
        return false;
    }
    true
}

/// Check if line is blank
///
/// Returns true if line if just whitespace
pub fn is_blank(line: &str) -> bool {
    let words = line.split_whitespace();

    for (_i, word) in words.enumerate() {
        if !word.is_empty() {
            return false;
        }
    }
    true
}

/// Check if line is comment
///
/// Returns true if line if just comment
pub fn is_comment(word: &mut String) -> bool {
    if word.len() < 2 {
        return false;
    }
    let bytes = word.as_bytes();
    let mut found_first = false;

    for (i, &item) in bytes.iter().enumerate() {
        if item == b'/' && i == 0 {
            found_first = true;
        }
        if item == b'/' && i == 1 && found_first {
            return true;
        }
    }
    false
}

/// Register name to hex
///
/// Map the register to the hex code for the opcode
pub fn map_reg_to_hex(input: &str) -> String {
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
        return "ERR ".to_string();
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
        if (i == num_registers as usize + 1) && (num_arguments == 1) {
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

/// Gets address from label or absolute values
///
/// Converts argument to label value or converts to Hex
pub fn convert_argument(
    argument: &str,
    msg_list: &mut MsgList,
    line_number: u32,
    labels: &mut Vec<Label>,
) -> Option<String> {
    if label_name_from_string(argument).is_some() {
        match return_label_value(argument, labels) {
            Some(n) => return Some(format!("{n:08X}")),
            None => {
                msg_list.push(
                    format!("Label {argument} not found - line {line_number}"),
                    Some(line_number),
                    MessageType::Warning,
                );
                return None;
            }
        };
    }

    if data_name_from_string(argument).is_some() {
        match return_label_value(argument, labels) {
            Some(n) => return Some(format!("{n:08X}")),
            None => {
                msg_list.push(
                    format!("Label {argument} not found - line {line_number}"),
                    Some(line_number),
                    MessageType::Warning,
                );
                return None;
            }
        };
    }

    if argument.len() >= 2 && (argument[0..2] == *"0x" || argument[0..2] == *"0X") {
        let without_prefix = argument.trim_start_matches("0x");
        let without_prefix = without_prefix.trim_start_matches("0X");
        let int_value_result = i64::from_str_radix(without_prefix, 16);
        if int_value_result.is_err() {
            return None;
        }
        let int_value = int_value_result.unwrap_or(0);

        if int_value <= 4_294_967_295 {
            return Some(format!("{int_value:08X}"));
        }
        msg_list.push(
            format!("Hex value out 0x{int_value:08X} of bounds"),
            Some(line_number),
            MessageType::Warning,
        );
        return None;
    }

    match argument.parse::<i64>() {
        Ok(n) => {
            if n <= 4_294_967_295 {
                return Some(format!("{n:08X}"));
            }
            msg_list.push(
                format!("Decimal value out {n} of bounds"),
                Some(line_number),
                MessageType::Warning,
            );
        }
        Err(_e) => {
            msg_list.push(
                format!("Decimal value {argument} incorrect"),
                Some(line_number),
                MessageType::Warning,
            );
        }
    };
    None
}

/// Strip trailing comments
///
///  Removes comments and starting and training whitespace
pub fn strip_comments(input: &mut str) -> String {
    match input.find("//") {
        None => return input.trim().to_string(),
        Some(a) => return input[0..a].trim().to_string(),
    }
}

/// Check if label is duplicate
///
/// Check if label is duplicate, and output message if duplicate is found
pub fn find_duplicate_label(labels: &mut Vec<Label>, msg_list: &mut MsgList) {
    let mut local_labels = labels.clone();
    for label in labels {
        let opt_found_line = return_label_value(&label.name, &mut local_labels);
        if opt_found_line.unwrap_or(0) != label.program_counter {
            msg_list.push(
                format!(
                    "Duplicate label {} found, with differing values",
                    label.name
                ),
                Some(label.line_counter),
                MessageType::Error,
            );
        }
    }
}

/// Find checksum
///
/// Calculates the checksum from the string of hex values, removing control characters
pub fn calc_checksum(input_string: &str, msg_list: &mut MsgList) -> String {
    let mut stripped_string: String = String::new();
    let mut checksum: u32 = 0;

    // Remove S, Z and X
    for char in input_string.chars() {
        if (char != 'S') && (char != 'Z') & (char != 'X') {
            stripped_string.push(char);
        }
    }

    // check if len is divisible by 4
    if stripped_string.len() % 4 != 0 {
        msg_list.push(
            {
                format!(
                    "Opcode list length not multiple of 4, length is {}",
                    stripped_string.len(),
                )
            },
            None,
            MessageType::Error,
        );
        return "00000000".to_string();
    }

    let mut position_index: u32 = 0;

    for (index, _) in stripped_string.chars().enumerate() {
        if index % 4 == 0 {
            let int_value = u32::from_str_radix(&stripped_string[index..index + 4], 16);
            if int_value.is_err() {
                msg_list.push(
                    {
                        format!(
                            "Error creating opcode for invalid value {}",
                            &stripped_string[index..index + 4],
                        )
                    },
                    None,
                    MessageType::Error,
                );
            } else {
                checksum = (checksum + int_value.unwrap_or(0)) % (0xFFFF + 1);
                position_index += 1;
            }
        }
    }
    checksum = (checksum + position_index - 1) % (0xFFFF + 1);
    format!("{checksum:04X}")
}

/// Return String of bit codes with start/stop bytes and CRC
///
/// Based on the Pass2 vector, create the bitcode, calculating the checksum, and adding control characters.
/// Currently only ever sets the stack to 16 bytes (Z0010)
pub fn create_bin_string(pass2: &mut Vec<Pass2>, msg_list: &mut MsgList) -> String {
    let mut output_string = String::new();

    output_string.push('S'); // Start character

    for pass in pass2 {
        output_string.push_str(&pass.opcode);
    }

    // Add writing Z0010 and then checksum.
    output_string.push_str("Z0010"); // Holding for stack of needed

    let checksum: String = calc_checksum(&output_string, msg_list);

    output_string.push_str(&checksum);

    output_string.push('X'); // Stop character

    output_string
}


pub fn trim_newline(s: &mut String) {
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    if s.ends_with('\r') {
        s.pop();
        if s.ends_with('\n') {
            s.pop();
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_calc_checksum() {
        let mut msg_list = MsgList::new();
        let checksum = calc_checksum("S0000Z0010", &mut msg_list);
        assert_eq!(checksum, "0011");
    }

    #[test]
    fn test_calc_checksum2() {
        let mut msg_list = MsgList::new();
        let checksum = calc_checksum("S00000000Z0010", &mut msg_list);
        assert_eq!(checksum, "0012");
    }

    #[test]
    fn test_calc_checksum3() {
        let mut msg_list = MsgList::new();
        let checksum = calc_checksum("S00009999Z0010", &mut msg_list);
        assert_eq!(checksum, "99AB");
    }

    #[test]
    fn test_trim_newline() {
        let mut s = String::from("Hello\n");
        trim_newline(&mut s);
        assert_eq!(s, "Hello");
    }
    #[test]

    fn test_create_bin_string() {
        let mut pass2 = Vec::new();
        pass2.push(Pass2 {
            opcode: String::from("1234"),
            input: String::new(),
            line_counter: 0,
            program_counter: 0,
            line_type: LineType::Data,
        });
        pass2.push(Pass2 {
            opcode: String::from("4321"),
            input: String::new(),
            line_counter: 0,
            program_counter: 0,
            line_type: LineType::Data,
        });
        pass2.push(Pass2 {
            opcode: String::from("9999"),
            input: String::new(),
            line_counter: 0,
            program_counter: 0,
            line_type: LineType::Data,
        });
        let mut msg_list = MsgList::new();
        let bin_string = create_bin_string(&mut pass2, &mut msg_list);
        assert_eq!(bin_string, "S123443219999Z0010EF01X");
    }

    #[test]
    fn test_strip_comments() {
        let mut input = String::from("Hello, world! //This is a comment");
        let output = strip_comments(&mut input);
        assert_eq!(output, "Hello, world!");
    }

    #[test]
    fn test_is_comment() {
        let mut input = String::from("//This is a comment");
        let output = is_comment(&mut input);
        assert!(output);
    }
    #[test]
    fn test_is_comment2() {
        let mut input = String::from("Hello //This is a comment");
        let output = is_comment(&mut input);
        assert!(!output);
    }

    #[test]
    fn test_is_comment3() {
        let mut input = String::from(" ");
        let output = is_comment(&mut input);
        assert!(!output);
    }

    #[test]
    fn test_is_blank1() {
        let input = String::from(" ");
        let output = is_blank(&input);
        assert!(output);
    }

    #[test]
    fn test_is_blank2() {
        let input = String::from("1234");
        let output = is_blank(&input);
        assert!(!output);
    }

    #[test]
    fn test_is_valid_line() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            name: String::from("PUSH"),
            opcode: String::from("1234"),
            comment: String::new(),
            variables: 0,
            registers: 0,
        });
        let output = is_valid_line(opcodes, input);
        assert!(output);
    }

    #[test]
    fn test_line_type1() {
        let mut input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            name: String::from("PUSH"),
            opcode: String::from("1234"),
            comment: String::new(),
            variables: 0,
            registers: 0,
        });
        let output = line_type(opcodes, &mut input);
        assert_eq!(output, LineType::Opcode);
    }
    #[test]
    fn test_line_type2() {
        let mut input = String::from("LOOP:");
        let opcodes = &mut Vec::<Opcode>::new();
        let output = line_type(opcodes, &mut input);
        assert_eq!(output, LineType::Label);
    }
    #[test]
    fn test_line_type3() {
        let mut input = String::from("#Dataname");
        let opcodes = &mut Vec::<Opcode>::new();
        let output = line_type(opcodes, &mut input);
        assert_eq!(output, LineType::Data);
    }

    #[test]
    fn test_line_type4() {
        let mut input = String::new();
        let opcodes = &mut Vec::<Opcode>::new();
        let output = line_type(opcodes, &mut input);
        assert_eq!(output, LineType::Blank);
    }

    #[test]
    fn test_line_type5() {
        let mut input = String::from("//This is a comment");
        let opcodes = &mut Vec::<Opcode>::new();
        let output = line_type(opcodes, &mut input);
        assert_eq!(output, LineType::Comment);
    }

    #[test]
    fn test_line_type6() {
        let mut input = String::from("1234");
        let opcodes = &mut Vec::<Opcode>::new();
        let output = line_type(opcodes, &mut input);
        assert_eq!(output, LineType::Error);
    }

    #[test]
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
        let output = num_registers(opcodes,&mut input);
        assert_eq!(output, Some(1));
    }

    #[test]
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
        let output = num_registers(opcodes,&mut input);
        assert_eq!(output, None);
    }
    #[test]
    fn test_data_as_bytes1() {
        let input = String::from("TEST 3");
        let output = data_as_bytes(&input);
        assert_eq!(output, Some("000000000000000000000000".to_string()));
    }

    #[test]
    fn test_data_as_bytes2() {
        let input = String::from("TEST");
        let output = data_as_bytes(&input);
        assert_eq!(output, None);
    }

    #[test]
    fn test_label_name_from_string1() {
        let input = String::from("LOOP:");
        let output = label_name_from_string(&input);
        assert_eq!(output, Some("LOOP:".to_string()));
    }

    #[test]
    fn test_label_name_from_string2() {
        let input = String::from("LOOP");
        let output = label_name_from_string(&input);
        assert_eq!(output, None);
    }

    #[test]
    fn test_data_name_from_string1() {
        let input = String::from("#TEST");
        let output = data_name_from_string(&input);
        assert_eq!(output, Some("#TEST".to_string()));
    }

    #[test]
    fn test_data_name_from_string2() {
        let input = String::from("TEST");
        let output = data_name_from_string(&input);
        assert_eq!(output, None);
    }

    

    #[test]
    fn test_return_label_value1() {
        let labels = &mut Vec::<Label>::new();
        labels.push(Label {
            program_counter: 42,
            line_counter: 0,
            name: String::from("LOOP:"),
        });
        let input = String::from("LOOP:");
        let output = return_label_value(&input, labels);
        assert_eq!(output, Some(42));
    }

    #[test]
    fn test_return_label_value2() {
        let labels = &mut Vec::<Label>::new();
        labels.push(Label {
            program_counter: 42,
            line_counter: 0,
            name: String::from("LOOP1:"),
        });
        let input = String::from("LOOP2:");
        let output = return_label_value(&input, labels);
        assert_eq!(output, None);
    }

    
    

}
