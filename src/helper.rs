use crate::files::LineType;
use crate::labels::label_name_from_string;
use crate::messages::{MessageType, MsgList};
use crate::opcodes::{return_opcode, Opcode, Pass2};

/// Number of reserved words at the start of memory for the heap header.
/// Byte 0x00: heap_start (written by assembler), Byte 0x04: heap_end (written by assembler),
/// Byte 0x08: reserved, Byte 0x0C: reserved. Code begins at byte 0x10.
pub const HEAP_HEADER_WORDS: u32 = 4;
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
        if (char != 'S') && (char != 'Z') && (char != 'X') {
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

    // Word 0: heap_start placeholder — patched below once total program size is known
    let heap_start_offset = output_string.len();
    output_string.push_str("00000000");

    // Words 1-3: reserved header words (heap_end, reserved, reserved)
    for _ in 1..HEAP_HEADER_WORDS {
        output_string.push_str("00000000");
    }

    for pass in pass2 {
        output_string.push_str(&pass.opcode);
    }

    // heap_start = first free byte after the program
    // output_string is 'S' + hex_chars; hex_chars/8 = words; words*4 = bytes
    #[allow(clippy::arithmetic_side_effects, reason = "Subtraction safe: string starts with 'S' so len >= 1")]
    #[allow(clippy::integer_division, reason = "Integer division intentional: hex chars → words → bytes")]
    let heap_start: u32 = (((output_string.len() - 1) / 8) * 4) as u32;
    #[allow(clippy::string_slice, reason = "Slice bounds are fixed and known safe")]
    output_string.replace_range(heap_start_offset..heap_start_offset + 8, &format!("{heap_start:08X}"));

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

    // Handle .word VALUE directive — emit a single 32-bit word
    if first_word == ".word" {
        let value_str = words.next().unwrap_or("");
        if value_str.is_empty() {
            return None;
        }
        let value: i64 = if value_str.len() >= 2
            && (value_str.get(0..2).unwrap_or("  ") == "0x"
                || value_str.get(0..2).unwrap_or("  ") == "0X")
        {
            let without_prefix = value_str.trim_start_matches("0x").trim_start_matches("0X");
            i64::from_str_radix(without_prefix, 16).unwrap_or(0)
        } else {
            value_str.parse::<i64>().unwrap_or(0)
        };
        #[allow(clippy::cast_sign_loss, reason = "Sign loss is intentional for u32 hex representation")]
        #[allow(clippy::cast_possible_truncation, reason = "Truncation to u32 is intentional for 32-bit hex output")]
        return Some(format!("{:08X}", value as u32));
    }

    // Handle .space N directive — N bytes of zero, rounded up to word boundary
    if first_word == ".space" {
        let count_str = words.next().unwrap_or("");
        if count_str.is_empty() {
            return None;
        }
        let byte_count: i64 = if count_str.len() >= 2
            && (count_str.get(0..2).unwrap_or("  ") == "0x"
                || count_str.get(0..2).unwrap_or("  ") == "0X")
        {
            let without_prefix = count_str.trim_start_matches("0x").trim_start_matches("0X");
            i64::from_str_radix(without_prefix, 16).unwrap_or(0)
        } else {
            count_str.parse::<i64>().unwrap_or(0)
        };
        if byte_count <= 0 {
            return None;
        }
        #[allow(clippy::integer_division, reason = "Integer division is intentional for word count calculation")]
        #[allow(clippy::arithmetic_side_effects, reason = "Arithmetic is intentional for word count rounding")]
        #[allow(clippy::integer_division_remainder_used, reason = "Division remainder is intentional for rounding")]
        let word_count = (byte_count + 3) / 4;
        let mut data = String::default();
        for _ in 0..word_count {
            data.push_str("00000000");
        }
        return Some(data);
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
    // Check for C compiler directives
    let first_word = line.split_whitespace().next().unwrap_or("");
    match first_word {
        ".word" | ".space" => return LineType::Data,
        ".text" | ".data" | ".rodata" | ".bss" | ".global" | ".globl" | ".extern" | ".comm" | ".lcomm" => return LineType::Comment,
        _ => {}
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


/// Parse expected UART hex values from source file comment headers.
///
/// Extracts 8-digit uppercase hex values from lines matching the pattern `//   XXXXXXXX  (`.
#[allow(clippy::arithmetic_side_effects, reason = "Slice indexing is safe after length check")]
#[allow(clippy::min_ident_chars, reason = "Single-char closure arg is idiomatic for simple predicates")]
pub fn parse_expected_uart_values(lines: &[String]) -> Vec<String> {
    let mut expected: Vec<String> = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if !trimmed.starts_with("//") {
            continue;
        }
        let comment_body = trimmed.get(2..).unwrap_or("").trim();
        if comment_body.len() >= 8 {
            let candidate = comment_body.get(..8).unwrap_or("");
            if candidate.len() == 8
                && candidate.chars().all(|c| c.is_ascii_hexdigit())
                && candidate == candidate.to_ascii_uppercase()
            {
                expected.push(candidate.to_owned());
            }
        }
    }
    expected
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
        // Word 0 is heap_start in bytes (5 words × 4 = 20 = 0x14),
        // words 1-3 are reserved zeros; checksum reflects the updated word 0.
        assert_eq!(bin_string, Some("S000000140000000000000000000000001234432100000001Z00105586X".to_owned()));
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

    #[test]
    // Test parsing expected UART values from typical test file header
    fn test_parse_expected_uart_values1() {
        let lines = vec![
            "// Test 05: Bit Manipulation".to_owned(),
            "// Expected UART output:".to_owned(),
            "//   00000080  (BSET: set bit 7)".to_owned(),
            "//   000000FF  (result)".to_owned(),
            "//   FF000000  (BITREV)".to_owned(),
            "// Expected 7SEG: 0x05".to_owned(),
            "_start".to_owned(),
            "SETR A 0x0".to_owned(),
        ];
        let result = parse_expected_uart_values(&lines);
        assert_eq!(result, vec!["00000080", "000000FF", "FF000000"]);
    }

    #[test]
    // Test parsing returns empty vec when no expected values
    fn test_parse_expected_uart_values2() {
        let lines = vec![
            "// This is a comment".to_owned(),
            "// No hex values here".to_owned(),
            "_start".to_owned(),
        ];
        let result = parse_expected_uart_values(&lines);
        assert!(result.is_empty());
    }

    #[test]
    // Test parsing ignores non-comment lines and short hex values
    fn test_parse_expected_uart_values3() {
        let lines = vec![
            "00000080".to_owned(),              // not a comment
            "// 0x05".to_owned(),               // too short
            "//   ZZZZZZZZ  (invalid)".to_owned(), // not hex
            "//   0000abcd  (lowercase)".to_owned(), // lowercase hex - should not match
            "//   0000ABCD  (uppercase)".to_owned(), // valid
        ];
        let result = parse_expected_uart_values(&lines);
        assert_eq!(result, vec!["0000ABCD"]);
    }

    #[test]
    // Test parsing with empty input
    fn test_parse_expected_uart_values4() {
        let lines: Vec<String> = Vec::new();
        let result = parse_expected_uart_values(&lines);
        assert!(result.is_empty());
    }

    #[test]
    fn test_data_as_bytes_word_hex() {
        let output = data_as_bytes(".word 0x2A");
        assert_eq!(output, Some("0000002A".to_owned()));
    }

    #[test]
    fn test_data_as_bytes_word_decimal() {
        let output = data_as_bytes(".word 42");
        assert_eq!(output, Some("0000002A".to_owned()));
    }

    #[test]
    fn test_data_as_bytes_word_zero() {
        let output = data_as_bytes(".word 0");
        assert_eq!(output, Some("00000000".to_owned()));
    }

    #[test]
    fn test_data_as_bytes_word_negative() {
        let output = data_as_bytes(".word -1");
        assert_eq!(output, Some("FFFFFFFF".to_owned()));
    }

    #[test]
    fn test_data_as_bytes_word_missing_value() {
        let output = data_as_bytes(".word");
        assert_eq!(output, None);
    }

    #[test]
    fn test_data_as_bytes_space_4_bytes() {
        let output = data_as_bytes(".space 4");
        assert_eq!(output, Some("00000000".to_owned()));
    }

    #[test]
    fn test_data_as_bytes_space_8_bytes() {
        let output = data_as_bytes(".space 8");
        assert_eq!(output, Some("0000000000000000".to_owned()));
    }

    #[test]
    fn test_data_as_bytes_space_5_bytes_rounds_up() {
        let output = data_as_bytes(".space 5");
        assert_eq!(output, Some("0000000000000000".to_owned()));
    }

    #[test]
    fn test_data_as_bytes_space_zero() {
        let output = data_as_bytes(".space 0");
        assert_eq!(output, None);
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
