use crate::files::LineType;
use crate::labels::label_name_from_string;
use crate::messages::{MessageType, MsgList};
use crate::opcodes::{return_opcode, Opcode, Pass2};
/// Find checksum.
///
/// Calculates the checksum from the string of hex values, removing control characters.
#[allow(clippy::modulo_arithmetic, reason = "Modulo arithmetic is intentional for checksum calculation")]
#[allow(clippy::arithmetic_side_effects, reason = "Arithmetic side effects are intentional in this checksum context")]
#[allow(clippy::integer_division_remainder_used, reason = "Integer division remainder is intentional for this calculation")]
pub fn calc_checksum(input_string: &str, msg_list: &mut MsgList) -> String {
    let mut stripped_string = String::default();
    let mut checksum: i32 = 0;

    // Remove S, Z and X
    for char in input_string.chars() {
        if (char != 'S') && (char != 'Z') & (char != 'X') {
            stripped_string.push(char);
        }
    }

    // check if len is divisible by 4
    if !stripped_string.len().is_multiple_of(4) {
        msg_list.push(
            {
                format!(
                    "Opcode list length not multiple of 4, length is {}",
                    stripped_string.len(),
                )
            },
            None,
            None,
            MessageType::Error,
        );
        return "0000".to_owned();
    }

    let mut position_index: u32 = 0;
    #[allow(clippy::char_indices_as_byte_indices, reason = "Using char indices as byte indices is intentional in this context de to nature of characters")]
    for (index, _) in stripped_string.chars().enumerate() {
        #[allow(clippy::integer_division_remainder_used, reason = "Integer division remainder is intentional for this calculation")]
        if index.is_multiple_of(4) {
            let int_value =
                i32::from_str_radix(stripped_string.get(index..index + 4).unwrap_or("    "), 16);
            if int_value.is_err() {
                msg_list.push(
                    {
                        format!(
                            "Error creating opcode for invalid value {}",
                            stripped_string.get(index..index + 4).unwrap_or("    "),
                        )
                    },
                    None,
                    None,
                    MessageType::Error,
                );
            } else {
                checksum = (checksum + int_value.unwrap_or(0_i32)) % (0xFFFF_i32 + 1_i32);
                position_index += 1;
            }
        }
    }
    
    checksum =
        (checksum + position_index.try_into().unwrap_or(0_i32) - 1).abs() % (0xFFFF_i32 + 1_i32);
    format!("{checksum:04X}")
}

/// Return String of bit codes with start/stop bytes and CRC.
///
/// Based on the Pass2 vector, create the bitcode, calculating the checksum, and adding control characters.
/// Currently only ever sets the stack to 16 bytes (Z0010).
#[allow(clippy::or_fun_call, reason = "Needed for simplicity of setting up default start address")]
pub fn create_bin_string(pass2: &[Pass2], msg_list: &mut MsgList) -> Option<String> {
    let mut output_string = String::default();

    output_string.push('S'); // Start character

    for pass in pass2 {
        output_string.push_str(&pass.opcode);
    }

    if pass2
        .iter()
        .filter(|x| x.line_type == LineType::Start)
        .count()
        == 1
    {
        output_string.push_str(
            format!(
                "{:08X}",
                pass2
                    .iter()
                    .find(|x| x.line_type == LineType::Start)
                    .unwrap_or(&Pass2 {
                        line_type: LineType::Start,
                        opcode: String::default(),
                        program_counter: 0,
                        line_counter: 0,
                        input_text_line: String::default(),
                        file_name: "None".to_owned(),
                    })
                    .program_counter
            )
            .as_str(),
        );
    } else if pass2
        .iter()
        .filter(|x| x.line_type == LineType::Start)
        .count()
        == 0
    {
        msg_list.push(
            "No start address found".to_owned(),
            None,
            None,
            MessageType::Error,
        );
        return None;
    } else {
        msg_list.push(
            "Multiple start addresses found".to_owned(),
            None,
            None,
            MessageType::Error,
        );
        return None;
    }

    // Add writing Z0010 and then checksum.
    output_string.push_str("Z0010"); // Holding for stack if needed

    let checksum = calc_checksum(&output_string, msg_list);

    output_string.push_str(&checksum);

    output_string.push('X'); // Stop character

    Some(output_string)
}

/// Returns bytes for data element.
///
/// Parses data element and returns data as bytes, or None if error.
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
            let input_string = remaining_line.trim_matches('\"').replace("\\n", "\r\n");
            let mut output_hex = String::default();
            // Length is based on multiples of 4
            #[allow(clippy::integer_division, reason = "Integer division is intentional for string length calculation")]
            #[allow(clippy::arithmetic_side_effects, reason = "Arithmetic side effects are intentional for string length calculation")]
            #[allow(clippy::integer_division_remainder_used, reason = "Integer division remainder is intentional for string length calculation")]  
            output_hex.push_str(
                format!(
                    "{:08X}",
                    (input_string.len() + 4 - input_string.len() % 4) / 4
                )
                .as_str(),
            ); // Add length of string to start
            for char in input_string.as_bytes() {
                let hex = format!("{char:02X}");
                output_hex.push_str(&hex);
            }
            #[allow(clippy::arithmetic_side_effects, reason = "Arithmetic side effects are intentional for string length calculation")]
            #[allow(clippy::integer_division_remainder_used, reason = "Integer division remainder is intentional for string length calculation")]
            let needed_bytes = 8 - (output_hex.len() % 8);
            for _n in 0..needed_bytes {
                output_hex.push('0');
            }
            return Some(output_hex);
        }
        None
    } else {
        // Check if next word is a number
        // let int_value: i64;
        let int_value = if second_word.len() >= 2
            && (second_word.get(0..2).unwrap_or("  ") == "0x"
                || second_word.get(0..2).unwrap_or("  ") == "0X")
        {
            let without_prefix1 = second_word.trim_start_matches("0x");
            let without_prefix2 = without_prefix1.trim_start_matches("0X");
            let int_value_result = i64::from_str_radix(&without_prefix2.replace('_', ""), 16);
            int_value_result.unwrap_or(0)
        } else {
            let int_value_result = second_word.parse::<i64>();
            int_value_result.unwrap_or(0)
        };

        if int_value == 0 {
            None
        } else {
            let mut data = String::default();
            for _ in 0..int_value {
                data.push_str("00000000");
            }
            Some(data)
        }
    }
}

/// Extracts data name from string.
///
/// Checks if start of first word is hash if so return data name as option string.
pub fn data_name_from_string(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");
    if first_word.starts_with('#') {
        return Some(first_word.to_owned());
    }
    None
}

/// Check if line is blank.
///
/// Returns true if line if just whitespace.
pub fn is_blank(line: &str) -> bool {
    let words = line.split_whitespace();

    for word in words {
        if !word.is_empty() {
            return false;
        }
    }
    true
}

/// Check if line is comment.
///
/// Returns true if line if just comment.
pub fn is_comment(line: &str) -> bool {
    let word = line.trim();
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

/// Check if line is start.
///
/// Returns true if line is start.
pub fn is_start(line: &str) -> bool {
    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        if i == 0 && word.to_uppercase() == "_START" {
            return true;
        }
    }
    false
}

/// Check if line is valid.
///
/// Returns true if line is not error.
pub fn is_valid_line(opcodes: &mut Vec<Opcode>, line: String) -> bool {
    let temp_line: String = line;
    if line_type(opcodes, &temp_line) == LineType::Error {
        return false;
    }
    true
}

/// Returns enum of type of line.
///
/// Given a code line, will returns if line is Label, Opcode, Blank, Comment or Error.
pub fn line_type(opcodes: &mut Vec<Opcode>, line: &str) -> LineType {
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
    LineType::Error
}

/// Return number of bytes of data.
///
/// From instruction name, option of number of bytes of data, or 0 is error.
pub fn num_data_bytes(
    line: &str,
    msg_list: &mut MsgList,
    line_number: u32,
    filename: String,
) -> u32 {
    data_as_bytes(line).map_or_else(
        || {
            msg_list.push(
                format!("Error in data definition for {line}"),
                Some(line_number),
                Some(filename),
                MessageType::Error,
            );
            0
        },
        |data| data.len().try_into().unwrap_or_default(),
    )
}

/// Returns trailing comments.
///
/// Removes comments and starting and training whitespace.
#[allow(clippy::arithmetic_side_effects, reason = "Arithmetic side effects are intentional for comment extraction")]
pub fn return_comments(input: &str) -> String {
    input.find("//").map_or_else(String::default, |location| input.get(location + 2..).unwrap_or("").trim().to_owned())
}

/// Strip trailing comments.
///
/// Removes comments and starting and training whitespace.
pub fn strip_comments(input: &str) -> String {
    input.find("//").map_or_else(
        || input.trim().to_owned(),
        |location| input.get(0..location).unwrap_or("").trim().to_owned(),
    )
}


/// Trim newline from string.
///
/// Removes newline from end of string.
pub fn trim_newline(input: &mut String) {
    if input.ends_with('\n') {
        input.pop();
        if input.ends_with('\r') {
            input.pop();
        }
    }
    if input.ends_with('\r') {
        input.pop();
        if input.ends_with('\n') {
            input.pop();
        }
    }
}

#[cfg(test)]
#[allow(clippy::arbitrary_source_item_ordering, reason = "Test functions can be in any order")]
mod tests {
    use super::*;
    use crate::labels::{return_label_value, Label};

    #[test]
    // Test that correct checksum is calculated
    fn test_calc_checksum1() {
        let mut msg_list = MsgList::new();
        let checksum = calc_checksum("S0000Z0010", &mut msg_list);
        assert_eq!(checksum, "0011");
    }
    #[test]
    // Test for invalid length
    fn test_calc_checksum2() {
        let mut msg_list = MsgList::new();
        let checksum = calc_checksum("S00001Z0010", &mut msg_list);
        assert_eq!(checksum, "0000");
        assert_eq!(
            msg_list.list.first().unwrap_or_default().text,
            "Opcode list length not multiple of 4, length is 9"
        );
    }

    #[test]
    // Test that correct checksum is calculated
    fn test_calc_checksum3() {
        let mut msg_list = MsgList::new();
        let checksum = calc_checksum("S00000000Z0010", &mut msg_list);
        assert_eq!(checksum, "0012");
    }

    #[test]
    // Test that correct checksum is calculated
    fn test_calc_checksum4() {
        let mut msg_list = MsgList::new();
        let checksum = calc_checksum("S00009999Z0010", &mut msg_list);
        assert_eq!(checksum, "99AB");
    }

    #[test]
    // Test that correct checksum is calculated
    fn test_calc_checksum5() {
        let mut msg_list = MsgList::new();
        let checksum = calc_checksum("____", &mut msg_list);
        assert_eq!(checksum, "0001");
        assert_eq!(
            msg_list.list.first().unwrap_or_default().text,
            "Error creating opcode for invalid value ____"
        );
    }

    #[test]
    // Test that line is trimmed of newline
    fn test_trim_newline1() {
        let mut test_string: String = String::from("Hello\n");
        trim_newline(&mut test_string);
        assert_eq!(test_string, "Hello");
    }
    #[test]
    // Test that line is trimmed of newline
    fn test_trim_newline2() {
        let mut test_string: String = String::from("Hello\r\n");
        trim_newline(&mut test_string);
        assert_eq!(test_string, "Hello");
    }
    #[test]
    // Test that line is trimmed of newline
    fn test_trim_newline3() {
        let mut test_string: String = String::from("Hello\n\r");
        trim_newline(&mut test_string);
        assert_eq!(test_string, "Hello");
    }
    #[test]
    // Test that the bin_string is created correctly with start value

    fn test_create_bin_string1() {
        let pass2 = &mut Vec::<Pass2>::new();
        pass2.push(Pass2 {
            opcode: String::default(),
            file_name: String::from("test"),
            input_text_line: String::default(),
            line_counter: 0,
            program_counter: 1,
            line_type: LineType::Start,
        });
        pass2.push(Pass2 {
            opcode: String::from("1234"),
            file_name: String::from("test"),
            input_text_line: String::default(),
            line_counter: 0,
            program_counter: 3,
            line_type: LineType::Data,
        });
        pass2.push(Pass2 {
            opcode: String::from("4321"),
            input_text_line: String::default(),
            file_name: String::from("test"),
            line_counter: 0,
            program_counter: 5,
            line_type: LineType::Data,
        });
        let mut msg_list = MsgList::new();
        let bin_string = create_bin_string(pass2, &mut msg_list);
        assert_eq!(bin_string, Some("S1234432100000001Z0010556AX".to_owned()));
    }

    #[test]
    // Test that the bin_string is null if duplicate starts

    fn test_create_bin_string2() {
        let pass2 = &mut Vec::<Pass2>::new();
        pass2.push(Pass2 {
            opcode: String::default(),
            file_name: String::from("test"),
            input_text_line: String::default(),
            line_counter: 0,
            program_counter: 1,
            line_type: LineType::Start,
        });
        pass2.push(Pass2 {
            opcode: String::default(),
            file_name: String::from("test"),
            input_text_line: String::default(),
            line_counter: 0,
            program_counter: 3,
            line_type: LineType::Start,
        });
        pass2.push(Pass2 {
            opcode: String::from("4321"),
            input_text_line: String::default(),
            file_name: String::from("test"),
            line_counter: 0,
            program_counter: 5,
            line_type: LineType::Data,
        });
        let mut msg_list = MsgList::new();
        let bin_string = create_bin_string(pass2, &mut msg_list);
        assert_eq!(bin_string, None);
        assert_eq!(
            msg_list.list.first().unwrap_or_default().text,
            "Multiple start addresses found"
        );
    }

    #[test]
    // Test that the bin_string is null if no starts

    fn test_create_bin_string3() {
        let pass2 = &mut Vec::<Pass2>::new();
        pass2.push(Pass2 {
            opcode: String::default(),
            file_name: String::from("test"),
            input_text_line: String::default(),
            line_counter: 0,
            program_counter: 1,
            line_type: LineType::Comment,
        });
        pass2.push(Pass2 {
            opcode: String::from("1234"),
            file_name: String::from("test"),
            input_text_line: String::default(),
            line_counter: 0,
            program_counter: 3,
            line_type: LineType::Data,
        });
        pass2.push(Pass2 {
            opcode: String::from("4321"),
            input_text_line: String::default(),
            file_name: String::from("test"),
            line_counter: 0,
            program_counter: 5,
            line_type: LineType::Data,
        });
        let mut msg_list = MsgList::new();
        let bin_string = create_bin_string(pass2, &mut msg_list);
        assert_eq!(bin_string, None);
        assert_eq!(
            msg_list.list.first().unwrap_or_default().text,
            "No start address found"
        );
    }

    #[test]
    // Test that comment is stripped
    fn test_strip_comments() {
        assert_eq!(
            strip_comments("Hello, world! //This is a comment"),
            "Hello, world!"
        );
        assert_eq!(strip_comments("Hello, world! //"), "Hello, world!");
        assert_eq!(strip_comments(""), "");
    }

    #[test]
    // Test that comment is returned
    fn test_return_comments() {
        assert_eq!(
            return_comments("Hello, world! //This is a comment"),
            "This is a comment"
        );
        assert_eq!(return_comments("Hello, world! //"), "");
        assert_eq!(return_comments("Hello, world!"), "");
    }

    #[test]
    // Test true is returned for comment
    fn test_is_comment1() {
        assert!(is_comment("//This is a comment"));
        assert!(is_comment("      //This is a comment"));
    }

    #[test]
    // Test false is returned for non-comment
    fn test_is_comment2() {
        assert!(!is_comment("Hello //This is a comment"));
        assert!(!is_comment(" "));
    }

    #[test]
    // Test for blank line returns true
    fn test_is_blank1() {
        assert!(is_blank(" "));
        assert!(is_blank(""));
    }

    #[test]
    // Test for non blank line returns false
    fn test_is_blank2() {
        assert!(!is_blank("1234"));
        assert!(!is_blank("    1234"));
    }

    #[test]
    // Test for valid line returns true is opcode is found
    fn test_is_valid_line1() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            comment: String::default(),
            variables: 0,
            registers: 0,
            section: String::default(),
        });
        let output = is_valid_line(opcodes, input);
        assert!(output);
    }

    #[test]
    fn test_is_valid_line2() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PULL"),
            hex_code: String::from("1234"),
            comment: String::default(),
            variables: 0,
            registers: 0,
            section: String::default(),
        });
        let output = is_valid_line(opcodes, input);
        assert!(!output);
    }

    #[test]
    // Test for opcode line type
    fn test_line_type1() {
        let input = String::from("PUSH");
        let mut opcodes = Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            comment: String::default(),
            variables: 0,
            registers: 0,
            section: String::default(),
        });
        let output = line_type(&mut opcodes, &input);
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
    fn test_num_data_bytes1() {
        let mut msg_list = MsgList::new();
        let input = String::from("#TEST 3");
        let output = num_data_bytes(&input, &mut msg_list, 0, "test".to_owned());
        assert_eq!(output, 24);
    }

    #[test]
    fn test_num_data_bytes2() {
        let mut msg_list = MsgList::new();
        let input = String::from("#TEST");
        let output = num_data_bytes(&input, &mut msg_list, 0, "test".to_owned());
        assert_eq!(output, 0);
        assert_eq!(
            msg_list.list.first().unwrap_or_default().text,
            "Error in data definition for #TEST"
        );
    }

    #[test]
    // Test for correct output from data line
    fn test_data_as_bytes1() {
        let input = String::from("#TEST 3");
        let output = data_as_bytes(&input);
        assert_eq!(output, Some("000000000000000000000000".to_owned()));
    }

    #[test]
    // Test for correct output from invalid data line
    fn test_data_as_bytes2() {
        let input = String::from("#TEST");
        let output = data_as_bytes(&input);
        assert_eq!(output, None);
    }

    #[test]
    // Test for correct output from invalid data line
    fn test_data_as_bytes3() {
        let input = String::default();
        let output = data_as_bytes(&input);
        assert_eq!(output, None);
    }

    #[test]
    fn test_data_as_bytes4() {
        let input = String::from("#TEST \"Hello\"");
        let output = data_as_bytes(&input);
        assert_eq!(output, Some("0000000248656C6C6F000000".to_owned()),);
    }

    #[test]
    fn test_data_as_bytes5() {
        let input = String::from("#TEST 0x1");
        let output = data_as_bytes(&input);
        assert_eq!(output, Some("00000000".to_owned()),);
    }

    #[test]
    fn test_data_as_bytes6() {
        let input = String::from("#TEST \"Hello");
        let output = data_as_bytes(&input);
        assert_eq!(output, None);
    }

    #[test]
    fn test_data_as_bytes7() {
        let input = String::from("#TEST FFFF");
        let output = data_as_bytes(&input);
        assert_eq!(output, None);
    }

    #[test]
    fn test_data_as_bytes8() {
        let input = String::from("#TEST FFFF DUMMY");
        let output = data_as_bytes(&input);
        assert_eq!(output, None);
    }

    #[test]
    // Test for correct label name
    fn test_label_name_from_string1() {
        let input = String::from("LOOP:");
        let output = label_name_from_string(&input);
        assert_eq!(output, Some("LOOP:".to_owned()));
    }

    #[test]
    // Test for invalid label name
    fn test_label_name_from_string2() {
        let input = String::from("LOOP");
        let output = label_name_from_string(&input);
        assert_eq!(output, None);
    }

    #[test]
    // Test for correct data name
    fn test_data_name_from_string1() {
        let input = String::from("#TEST");
        let output = data_name_from_string(&input);
        assert_eq!(output, Some("#TEST".to_owned()));
    }

    #[test]
    // Test for invalid data name
    fn test_data_name_from_string2() {
        let input = String::from("TEST");
        let output = data_name_from_string(&input);
        assert_eq!(output, None);
    }

    #[test]
    // Test for correct label returned
    fn test_return_label_value1() {
        let labels = &mut Vec::<Label>::new();
        labels.push(Label {
            program_counter: 42,
            name: String::from("LOOP:"),
        });
        let input = String::from("LOOP:");
        let output = return_label_value(&input, labels);
        assert_eq!(output, Some(42));
    }

    #[test]
    // Test for no label returned
    fn test_return_label_value2() {
        let labels = &mut Vec::<Label>::new();
        labels.push(Label {
            program_counter: 42,
            name: String::from("LOOP1:"),
        });
        let input = String::from("LOOP2:");
        let output = return_label_value(&input, labels);
        assert_eq!(output, None);
    }
}
