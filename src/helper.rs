use crate::files::{Label, LineType, Macro, Opcode};
use crate::messages::{MessageType, MsgList};

pub struct Pass0 {
    pub input: String,
    pub line_counter: u32,
}

pub struct Pass1 {
    pub input: String,
    pub line_counter: u32,
    pub program_counter: u32,
    pub line_type: LineType,
}

pub struct Pass2 {
    pub input: String,
    pub line_counter: u32,
    pub program_counter: u32,
    pub line_type: LineType,
    pub opcode: String,
}

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

/// Extracts macro from string
///
/// Checks if end of first word is colon if so return macro name as option string
pub fn macro_name_from_string(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");
    if first_word.starts_with('$') {
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

/// Returns Macro from name
///
/// Return option macro if it exists, or none
pub fn return_macro<'a>(line: &'a str, macros: &'a mut [Macro]) -> Option<Macro> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");

    for macro_line in macros {
        if macro_line.name == first_word {
            return Some(macro_line.clone());
        }
    }
    None
}

/// Update variables in a macro
///
/// Return option all vec string replacing %x with correct value.
pub fn return_macro_items_replace(
    line: &str,
    macros: &mut [Macro],
    input_line_number: u32,
    msg_list: &mut MsgList,
) -> Option<Vec<String>> {
    let mut words = line.split_whitespace();
    let mut return_items: Vec<String> = Vec::new();
    let mut found: bool = false;

    let input_line_array: Vec<_> = words.clone().collect();

    let first_word = words.next().unwrap_or("");
    macro_name_from_string(first_word)?;
    for macro_line in macros {
        if macro_line.name == first_word {
            found = true;

            if input_line_array.len() > macro_line.variables as usize + 1 {
                msg_list.push(
                    format!("Too many variables for macro {}", macro_line.name),
                    Some(input_line_number),
                    MessageType::Warning,
                );
            }

            for item in &macro_line.items {
                let item_words = item.split_whitespace();
                let mut build_line: String = String::new();
                for item_word in item_words {
                    if item_word.contains('%') {
                        let without_prefix = item_word.trim_start_matches('%');
                        let int_value = without_prefix.parse::<u32>();
                        if int_value.clone().is_err() || int_value.clone().unwrap_or(0) < 1 {
                            msg_list.push(
                                format!(
                                    "Invalid macro argument number {}, in macro {}",
                                    without_prefix, macro_line.name
                                ),
                                Some(input_line_number),
                                MessageType::Error,
                            );
                        } else if int_value.clone().unwrap_or(0)
                            > (input_line_array.len() - 1).try_into().unwrap()
                        {
                            msg_list.push(
                                format!(
                                    "Missing argument {} for macro {}",
                                    int_value.clone().unwrap_or(0),
                                    macro_line.name
                                ),
                                Some(input_line_number),
                                MessageType::Error,
                            );
                        } else {
                            build_line = build_line
                                + " "
                                + input_line_array[int_value.clone().unwrap_or(0) as usize];
                        }
                    } else {
                        build_line = build_line + " " + item_word;
                    }
                }
                return_items.push(build_line.trim_start().to_string());
            }
            
        }
    }
    if found {
        Some(return_items)
    } else {
        None
    }
}

/// Multi pass to resolve embedded macros
///
/// Takes Vector of macros, and embeds macros recursively, up to 10 passes
/// Will create errors message for more than 10 passes
#[allow(clippy::too_many_lines)]
pub fn expand_macros_multi(macros: Vec<Macro>, msg_list: &mut MsgList) -> Vec<Macro> {
    let mut pass: u32 = 0;
    let mut changed: bool = true;
    let mut last_macro: String = String::new();

    let mut input_macros = macros;

    while pass < 10 && changed {
        changed = false;
        let mut output_macros: Vec<Macro> = Vec::new();
        for input_macro_line in input_macros.clone() {
            let mut output_items: Vec<String> = Vec::new();
            for item in input_macro_line.items {
                if return_macro(&item, &mut input_macros).is_some() {
                    let mut item_line_array: Vec<String> = Vec::new();
                    let item_words = item.split_whitespace();
                    for item_word in item_words {
                        item_line_array.push(item_word.to_string());
                    }

                    if (return_macro(&item, &mut input_macros).unwrap().variables as usize)
                        < item_line_array.len() - 1
                    {
                        msg_list.push(
                            format!(
                                "Too many variables in imbedded macro \"{}\" in macro {}",
                                item, input_macro_line.name,
                            ),
                            None,
                            MessageType::Warning,
                        );
                    }

                    for new_item in return_macro(&item, &mut input_macros).unwrap().items {
                        if new_item.contains('%') {
                            // Replace %n in new items with the nth value in item

                            let new_item_words = new_item.split_whitespace();
                            let mut build_line: String = String::new();
                            for item_word in new_item_words {
                                if item_word.contains('%') {
                                    let without_prefix = item_word.trim_start_matches('%');
                                    let int_value = without_prefix.parse::<u32>();
                                    if int_value.clone().is_err()
                                        || int_value.clone().unwrap_or(0) < 1
                                    {
                                        msg_list.push(
                                            format!(
                                            "Invalid macro argument number {}, in imbedded macro \"{}\" in {}",
                                            without_prefix,
                                            item,
                                            input_macro_line.name,
                                        ),
                                            None,
                                            MessageType::Error,
                                        );
                                    } else if int_value.clone().unwrap_or(0) as usize
                                        > item_line_array.len() - 1
                                    {
                                        msg_list.push(
                                            format!(
                                                "Missing argument {} for imbedded macro \"{}\" in {}",
                                                int_value.unwrap_or(0),
                                                item,
                                                input_macro_line.name,
                                            ),
                                            None,
                                            MessageType::Error,
                                        );
                                    } else {
                                        build_line = build_line
                                            + " "
                                            + &item_line_array
                                                [int_value.clone().unwrap_or(0) as usize];
                                    }
                                } else {
                                    build_line = build_line + " " + item_word;
                                }
                            }
                            output_items.push(build_line);
                        }
                    }
                    last_macro = input_macro_line.name.clone();
                    changed = true;
                } else {
                    output_items.push(item);
                }
            }
            output_macros.push(Macro {
                name: input_macro_line.name,
                variables: input_macro_line.variables,
                items: output_items,
            });
        }
        pass += 1;
        input_macros = output_macros.clone();
    }
    if changed {
        msg_list.push(
            format!("Too many macro passes, check {last_macro}"),
            None,
            MessageType::Error,
        );
    }
    input_macros
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
#[allow(clippy::cast_possible_wrap)]
pub fn write_serial(binary_output: &str, port_name: &str, msg_list: &mut MsgList) -> bool {
    let mut buffer = [0; 1024];
    let port_result = serialport::new(port_name, 115_200)
        .timeout(std::time::Duration::from_millis(100))
        .open();

    if port_result.is_err() {
        let mut all_ports: String = String::new();
        let ports = serialport::available_ports();

        match ports {
            Err(_) => {
                msg_list.push(
                    "Error opening serial port, no ports found".to_string(),
                    None,
                    MessageType::Error,
                );
                return false;
            }
            Ok(ports) => {
                let mut max_ports: i32 = -1;
                for (port_count, p) in (0_u32..).zip(ports.into_iter()) {
                    if port_count > 0 {
                        all_ports.push_str(" , ");
                    }
                    all_ports.push_str(&p.port_name);
                    max_ports = port_count as i32;
                }

                let ports_msg = match max_ports {
                    -1 => "no ports were found".to_string(),
                    0 => {
                        format!("only port {all_ports} was found")
                    }
                    _ => {
                        format!("the following ports were found {all_ports}")
                    }
                };

                msg_list.push(
                    format!("Error opening serial port {port_name}, {ports_msg}"),
                    None,
                    MessageType::Error,
                );
                return false;
            }
        }
    }

    let mut port = port_result.unwrap();

    if port.set_stop_bits(serialport::StopBits::One).is_err() {
        return false;
    }
    if port.set_data_bits(serialport::DataBits::Eight).is_err() {
        return false;
    }
    if port.set_parity(serialport::Parity::None).is_err() {
        return false;
    }

    if port.read(&mut buffer[..]).is_err() { //clear any old messages in buffer
    }

    if port.write(binary_output.as_bytes()).is_err() {
        return false;
    }

    if port.flush().is_err() {
        return false;
    }

    let ret_msg_size = port.read(&mut buffer[..]).unwrap_or(0);

    if ret_msg_size == 0 {
        msg_list.push(
            "No message received from board".to_string(),
            None,
            MessageType::Warning,
        );
        return true;
    }

    let ret_msg = String::from_utf8(buffer[..ret_msg_size].to_vec());

    if ret_msg.is_err() {
        msg_list.push(
            "Invalid message received from board".to_string(),
            None,
            MessageType::Warning,
        );
        return true;
    }

    let mut print_ret_msg = ret_msg.unwrap_or_else(|_| String::new());

    trim_newline(&mut print_ret_msg); //Board can send CR/LF messages

    msg_list.push(
        format!("Message received from board is \"{print_ret_msg}\""),
        None,
        MessageType::Info,
    );

    true
}

fn trim_newline(s: &mut String) {
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
    use crate::messages::print_messages;

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
    fn test_macro_name_from_string1() {
        let input = String::from("$TEST");
        let output = macro_name_from_string(&input);
        assert_eq!(output, Some("$TEST".to_string()));
    }

    #[test]
    fn test_macro_name_from_string2() {
        let input = String::from("TEST");
        let output = macro_name_from_string(&input);
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

    #[test]
    fn test_return_macro_value1() {
        let macros = &mut Vec::<Macro>::new();
        macros.push(Macro {
            name: String::from("$TEST"),
            variables: 0,
            items: Vec::new(),
        });
        let input = String::from("$TEST");
        let output = return_macro(&input, macros);
        assert_eq!(output, Some(Macro {
            name: String::from("$TEST"),
            variables: 0,
            items: Vec::new(),
        }));
    }
        #[test]
    fn test_return_macro_value2() {
        let macros = &mut Vec::<Macro>::new();
        macros.push(Macro {
            name: String::from("$TEST1"),
            variables: 0,
            items: Vec::new(),
        });
        let input = String::from("$TEST2");
        let output = return_macro(&input, macros);
        assert_eq!(output, None);
    }

    #[test]
    fn test_return_macro_items_replace1() {
        let macros = &mut Vec::<Macro>::new();
        let msg_list = &mut MsgList::new();
        macros.push(Macro {
            name: String::from("$DELAY"),
            variables: 2,
            items: vec![String::from("DELAYV %1"), String::from("DELAYV %2"), String::from("PUSH %1")],
        });
        let input = String::from("$DELAY ARG_A  ARG_B");
        let output = return_macro_items_replace(&input, macros, 0,msg_list);
        print_messages(msg_list);
        assert_eq!(output, Some(vec![String::from("DELAYV ARG_A"),String::from("DELAYV ARG_B"), String::from("PUSH ARG_A")]));
    }

    #[test]
    fn test_return_macro_items_replace2() {
        let macros = &mut Vec::<Macro>::new();
        let msg_list = &mut MsgList::new();
        macros.push(Macro {
            name: String::from("$DELAY"),
            variables: 3,
            items: vec![String::from("DELAYV %1"), String::from("DELAYV %2"), String::from("PUSH %3")],
        });
        let input = String::from("$DELAY %MACRO1  ARG_B ARG_C");
        let output = return_macro_items_replace(&input, macros, 0,msg_list);
        assert_eq!(output, Some(vec![String::from("DELAYV %MACRO1"),String::from("DELAYV ARG_B"), String::from("PUSH ARG_C")]));
    }

    #[test]
    fn test_return_macro_items_replace3() {
        let macros = &mut Vec::<Macro>::new();
        let msg_list = &mut MsgList::new();
        macros.push(Macro {
            name: String::from("$DELAY1"),
            variables: 3,
            items: vec![String::from("DELAYV %1"), String::from("DELAYV %2"), String::from("PUSH %3")],
        });
        let input = String::from("$DELAY2 %MACRO1  ARG_B ARG_C");
        let output = return_macro_items_replace(&input, macros, 0,msg_list);
        assert_eq!(output, None);
    }

    #[test]
    fn test_expand_macros_multi1() {
        let macros = &mut Vec::<Macro>::new();
        
        let msg_list = &mut MsgList::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("OPCODE1 %1"), String::from("OPCODE2 %2")],
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("$MACRO1 %2 %1"), String::from("OPCODE3")],
        });
        
        let output = expand_macros_multi(macros.clone(),msg_list);
        print_messages(msg_list);
        let macros_result = &mut Vec::<Macro>::new();
        macros_result.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("OPCODE1 %1"), String::from("OPCODE2 %2")],
        });
        macros_result.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("OPCODE1 %2"), String::from("OPCODE2 %1"),String::from("OPCODE3")],
        });



        assert_eq!(output, *macros_result);
       
    }

}
