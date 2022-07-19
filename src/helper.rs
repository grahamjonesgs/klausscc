use crate::files::*;
use crate::messages::*;
use serialport::*;
use std::time::Duration;

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

/// Extracts dataname from string
///
/// Checks if start of first word is hash if so return dataname as option string
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
/// Return option of progam counter for label if it exists, or None
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
pub fn return_macro(line: &str, macros: &mut [Macro]) -> Option<Macro> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");
    macro_name_from_string(first_word)?;
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

            if input_line_array.len() as u32 > macro_line.variables + 1 {
                msg_list.push(
                    format!("Too many variables for macro {}", macro_line.name),
                    Some(input_line_number),
                    MessageType::Warning,
                );
            }

            for item in &macro_line.items {
                let item_words = item.split_whitespace();
                let mut build_line: String = "".to_string();
                for item_word in item_words {
                    if item_word.contains('%') {
                        let without_prefix = item_word.trim_start_matches('%');
                        let int_value = without_prefix.parse::<i64>();
                        if int_value.clone().is_err() || int_value.clone().unwrap_or(0) < 1 {
                            msg_list.push(
                                format!(
                                    "Invalid macro argument number {}, in macro {}",
                                    without_prefix, macro_line.name
                                ),
                                Some(input_line_number),
                                MessageType::Error,
                            );
                        } else if int_value.clone().unwrap_or(0) > input_line_array.len() as i64 - 1
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
                        build_line = build_line + " " + item_word
                    }
                }
                return_items.push(build_line.trim().to_string())
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
/// Takes Vector of macros, and embeds macros recursivly, up to 10 passes
/// Will create errors message for more than 10 passes
pub fn expand_macros_multi(macros: Vec<Macro>, msg_list: &mut MsgList) -> Vec<Macro> {
    let mut pass: u32 = 0;
    let mut changed: bool = true;
    let mut last_macro: String = "".to_string();

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

                    if return_macro(&item, &mut input_macros).unwrap().variables
                        < item_line_array.len() as u32 - 1
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
                            // Replace %n in new _tems with the nth value in item

                            let new_item_words = new_item.split_whitespace();
                            let mut build_line: String = "".to_string();
                            for item_word in new_item_words {
                                if item_word.contains('%') {
                                    let without_prefix = item_word.trim_start_matches('%');
                                    let int_value = without_prefix.parse::<i64>();
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
                                    } else if int_value.clone().unwrap_or(0)
                                        > item_line_array.len() as i64 - 1
                                    {
                                        msg_list.push(
                                            format!(
                                                "Missing argument {} for imbedded macro \"{}\" in {}",
                                                int_value.clone().unwrap_or(0),
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
                                    build_line = build_line + " " + item_word
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
            })
        }
        pass += 1;
        input_macros = output_macros.clone();
    }
    if changed {
        msg_list.push(
            format!("Too many macro passes, check {}", last_macro),
            None,
            MessageType::Error,
        );
    }
    input_macros.to_vec()
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
        Some(data) => data.len() as u32,
        None => {
            msg_list.push(
                format!("Error in data definition for {}", line),
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
    if !second_word.starts_with('\"') {
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
    } else {
        let remaning_line = line.trim_start_matches(first_word).trim();

        if remaning_line.starts_with('\"') && remaning_line.ends_with('\"') {
            let output = remaning_line.trim_matches('\"').to_string();
            let mut output_hex = "".to_string();
            for c in output.as_bytes() {
                let hex = format!("{:02X}", c);
                output_hex.push_str(&hex);
                output_hex.push_str("000000");
            }
            output_hex.push_str("00000000"); // Add null terminator

            Some(output_hex)
        } else {
            None
        }
    }
}

//// Returns number of regs for opcode
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

/// Returns emum of type of line
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
    if is_blank(line.to_string()) {
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
    let mut myline: String = line;
    if line_type(opcodes, &mut myline) == LineType::Error {
        return false;
    }
    true
}

/// Check if line is blank
///
/// Returns true if line if just whitespace
pub fn is_blank(line: String) -> bool {
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
            found_first = true
        }
        if item == b'/' && i == 1 && found_first {
            return true;
        }
    }
    false
}

/// Register name to hex
///
/// Map the reigter to the hex code for the opcode
pub fn map_reg_to_hex(input: String) -> String {
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
/// Returns the hex code operand from the line, adding regiter values
pub fn add_registers(
    opcodes: &mut Vec<Opcode>,
    line: &mut String,
    msg_list: &mut MsgList,
    line_number: u32,
) -> String {
    let num_registers = num_registers(opcodes, &mut line.to_string().to_uppercase()).unwrap_or(0);

    let mut opcode_found = {
        let this = return_opcode(&line.to_uppercase(), opcodes);
        let default = "".to_string();
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
            opcode_found = opcode_found + &map_reg_to_hex(word.to_string())
        }
    }

    if opcode_found.len() != 8 || opcode_found.contains('X') {
        msg_list.push(
            format!("Incorrect register defintion - \"{}\"", line),
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
    let mut arguments = "".to_string();

    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        if (i as u32 == num_registers + 1) && (num_arguments == 1) {
            arguments = arguments
                + &{
                    let this = convert_argument(
                        word.to_string().to_uppercase(),
                        msg_list,
                        line_number,
                        labels,
                    );
                    let default = "00000000".to_string();
                    match this {
                        Some(x) => x,
                        None => default,
                    }
                }
        }
        if i as u32 == num_registers + 2 && num_arguments == 2 {
            arguments = arguments
                + &{
                    let this = convert_argument(
                        word.to_string().to_uppercase(),
                        msg_list,
                        line_number,
                        labels,
                    );
                    let default = "00000000".to_string();
                    match this {
                        Some(x) => x,
                        None => default,
                    }
                }
        }
        if i as u32 > num_registers + num_arguments {
            msg_list.push(
                format!("Too many arguments found - \"{}\"", line),
                Some(line_number),
                MessageType::Warning,
            );
        }
    }

    if arguments.len() as u32 != 8 * num_arguments {
        msg_list.push(
            format!("Incorrect argument defintion - \"{}\"", line),
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
    argument: String,
    msg_list: &mut MsgList,
    line_number: u32,
    labels: &mut Vec<Label>,
) -> Option<String> {
    if label_name_from_string(&argument).is_some() {
        match return_label_value(&argument, labels) {
            Some(n) => return Some(format!("{:08X}", n)),
            None => {
                msg_list.push(
                    format!("Label {} not found - line {}", argument, line_number),
                    Some(line_number),
                    MessageType::Warning,
                );
                return None;
            }
        };
    }

    if data_name_from_string(&argument).is_some() {
        match return_label_value(&argument, labels) {
            Some(n) => return Some(format!("{:08X}", n)),
            None => {
                msg_list.push(
                    format!("Label {} not found - line {}", argument, line_number),
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

        if int_value <= 4294967295 {
            return Some(format!("{:08X}", int_value));
        } else {
            msg_list.push(
                format!("Hex value out 0x{:08X} of bounds", int_value),
                Some(line_number),
                MessageType::Warning,
            );
            return None;
        }
    }

    match argument.parse::<i64>() {
        Ok(n) => {
            if n <= 4294967295 {
                return Some(format!("{:08X}", n));
            } else {
                msg_list.push(
                    format!("Decimal value out {} of bounds", n),
                    Some(line_number),
                    MessageType::Warning,
                );
            }
        }
        Err(_e) => {
            msg_list.push(
                format!("Decimal value {} incorrect", argument),
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
/// Calculates the checksum from the string of hex values, removing control charaters
pub fn calc_checksum(input_string: &str, msg_list: &mut MsgList) -> String {
    let mut stripped_string: String = "".to_string();
    let mut checksum: u32 = 0;

    // Remove S, Z and X
    for char in input_string.chars() {
        if (char != 'S') && (char != 'Z') & (char != 'X') {
            stripped_string.push(char);
        }
    }

    // check if len is divisable by 4
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

    let mut possition_index: u32 = 0;

    for (index, _) in stripped_string.chars().enumerate() {
        if index % 4 == 0 {
            let int_value = i64::from_str_radix(&stripped_string[index..index + 4], 16);
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
                checksum = (checksum + int_value.unwrap_or(0) as u32) % (0xFFFF + 1);
                possition_index += 1;
            }
        }
    }
    checksum = (checksum + possition_index - 1) % (0xFFFF + 1);
    format!("{:04X}", checksum)
}

/// Return String of bitcodes with start/stop bytes and CRC
///
/// Based on the Pass2 vector, create the bitcode, calculating the checksum, and adding control charaters.
/// Currently only ever sets the stack to 16 bytes (Z0010)
pub fn create_bin_string(pass2: &mut Vec<Pass2>, msg_list: &mut MsgList) -> String {
    let mut output_string = "".to_string();

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

pub fn write_serial(binout: String, port_name: &str, msg_list: &mut MsgList) -> bool {
    let mut buffer = [0; 1024];
    let port_result = serialport::new(port_name, 115200)
        .timeout(Duration::from_millis(100))
        .open();

    if port_result.is_err() {
        let mut all_ports: String = "".to_string();
        let ports = serialport::available_ports();

        match ports {
            Err(_) => {
                msg_list.push(
                    "Error openning serial port, no ports found".to_string(),
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
                        format!("only port {} was found", all_ports)
                    }
                    _ => {
                        format!("the following ports were found {}", all_ports)
                    }
                };

                msg_list.push(
                    format!("Error openning serial port {}, {}", port_name, ports_msg),
                    None,
                    MessageType::Error,
                );
                return false;
            }
        }
    }

    let mut port = port_result.unwrap();

    if port.set_stop_bits(StopBits::One).is_err() {
        return false;
    }
    if port.set_data_bits(DataBits::Eight).is_err() {
        return false;
    }
    if port.set_parity(Parity::None).is_err() {
        return false;
    }

    if port.read(&mut buffer[..]).is_err() { //clear any old messages in buffer
    }

    if port.write(binout.as_bytes()).is_err() {
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

    let mut print_ret_msg = ret_msg.unwrap_or_else(|_| "".to_string());

    trim_newline(&mut print_ret_msg); //Board can send CR/LF messages

    msg_list.push(
        format!("Message received from board is \"{}\"", print_ret_msg),
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
