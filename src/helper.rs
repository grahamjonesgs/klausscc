use crate::files::{LineType};
use crate::labels::label_name_from_string;
use crate::messages::{MessageType, MsgList};
use crate::opcodes::{return_opcode, Opcode, Pass2};

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

/// Strip trailing comments
///
///  Removes comments and starting and training whitespace
pub fn strip_comments(input: &mut str) -> String {
    match input.find("//") {
        None => return input.trim().to_string(),
        Some(a) => return input[0..a].trim().to_string(),
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
    use crate::files::{LineType};
    use crate::helper::{
        calc_checksum, create_bin_string, data_as_bytes, data_name_from_string, is_blank,
        is_comment, is_valid_line, label_name_from_string, line_type, strip_comments, trim_newline,
        MsgList,
    };
    use crate::labels::{return_label_value, Label};
    use crate::opcodes::{Opcode, Pass2};

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
