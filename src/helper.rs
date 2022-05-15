use crate::files::*;
use crate::messages::*;

/// Extracts label from string
/// 
/// Checks if end of first word is colon if so return label as option string
pub fn label_name_from_string(line: &String) -> Option<String> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");
    if first_word.ends_with(":") {
        return Some(first_word.to_string());
    }
    None
}

/// Extracts macro from string
/// 
/// Checks if end of first word is colon if so return macro name as option string
pub fn macro_name_from_string(line: &String) -> Option<String> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");
    if first_word.starts_with("$") {
        return Some(first_word.to_string());
    }
    None
}

/// Return program counter for label
/// 
/// Return option of progam counter for label if it exists, or None
pub fn return_label_value(line: &String, labels: &mut Vec<Label>) -> Option<u32> {
    for label in labels {
        if label.name == line.as_str() {
            return Some(label.program_counter);
        }
    }
    None
}

/// Returns Macro from name
/// 
/// Return option macro if it exists, or none
pub fn return_macro(line: &String, macros: &mut Vec<Macro>) -> Option<Macro> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");
    if macro_name_from_string(&first_word.to_string()).is_none() {
        return None;
    }
    for macro_line in macros.clone() {
        if macro_line.name == first_word {
            return Some(macro_line);
        }
    }
    None
}

// Return option all vec string replacing %x with correct value.
pub fn return_macro_items_replace(
    line: &String,
    macros: &mut Vec<Macro>,
    input_line_number: u32,
    msg_list: &mut MsgList,
) -> Option<Vec<String>> {
    let mut words = line.split_whitespace();
    let mut return_items: Vec<String> = Vec::new();
    let mut found: bool = false;

    let input_line_array: Vec<_> = words.clone().collect();

    let first_word = words.next().unwrap_or("");
    if macro_name_from_string(&first_word.to_string()).is_none() {
        return None;
    }
    for macro_line in macros.clone() {
        if macro_line.name == first_word {
            found = true;

            if input_line_array.len() as u32 > macro_line.variables + 1 {
                msg_list.push(
                    format!("Too many variables for macro {}", macro_line.name),
                    Some(input_line_number),
                    MessageType::Warning,
                );
            }

            for item in macro_line.items {
                let item_words = item.split_whitespace();
                let mut build_line: String = "".to_string();
                for item_word in item_words {
                    if item_word.find("%").is_some() {
                        let without_prefix = item_word.trim_start_matches("%");
                        let int_value = i64::from_str_radix(without_prefix, 10);
                        if int_value.clone().is_err() || int_value.clone().unwrap_or(0) < 1 {
                            msg_list.push(
                                format!(
                                    "Invalid macro argument number {}, in macro {}",
                                    without_prefix, macro_line.name
                                ),
                                Some(input_line_number),
                                MessageType::Error,
                            );
                        } else {
                            if int_value.clone().unwrap_or(0) > input_line_array.len() as i64 - 1 {
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
        return Some(return_items);
    } else {
        None
    }
}

// Multi pass to resolve embedded macros
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
                        if new_item.find("%").is_some() {
                            // Replace %n in new _tems with the nth value in item

                            let new_item_words = new_item.split_whitespace();
                            let mut build_line: String = "".to_string();
                            for item_word in new_item_words {
                                if item_word.find("%").is_some() {
                                    let without_prefix = item_word.trim_start_matches("%");
                                    let int_value = i64::from_str_radix(without_prefix, 10);
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
                                    } else {
                                        if int_value.clone().unwrap_or(0)
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
        pass = pass + 1;
        input_macros = output_macros.clone();
    }
    if changed == true {
        msg_list.push(
            format!("Too many macro passes, check {}", last_macro),
            None,
            MessageType::Error,
        );
    }
    input_macros.to_vec()
}

// Checks if first word is opcode and if so returns opcode hex value
pub fn is_opcode(opcodes: &mut Vec<Opcode>, line: String) -> Option<String> {
    for opcode in opcodes {
        let mut words = line.split_whitespace();
        let first_word = words.next().unwrap_or("");
        if first_word.to_uppercase() == opcode.name {
            return Some(opcode.opcode.to_string().to_uppercase());
        }
    }
    None
}

// Returns option of number of arguments for opcode
pub fn num_arguments(opcodes: &mut Vec<Opcode>, line: &mut String) -> Option<u32> {
    for opcode in opcodes {
        let mut words = line.split_whitespace();
        let first_word = words.next().unwrap_or("");
        if first_word == "" {
            return None;
        }
        if first_word.to_uppercase() == opcode.name {
            return Some(opcode.variables);
        }
    }
    None
}

// Returns option of number of registers for opcode
pub fn num_registers(opcodes: &mut Vec<Opcode>, line: &mut String) -> Option<u32> {
    for opcode in opcodes {
        let mut words = line.split_whitespace();
        let first_word = words.next().unwrap_or("");
        if first_word == "" {
            return None;
        }
        if first_word == opcode.name {
            return Some(opcode.registers);
        }
    }
    None
}

// Returns emum of type of line
pub fn line_type(opcodes: &mut Vec<Opcode>, line: &mut String) -> LineType {
    if label_name_from_string(&line).is_some() {
        return LineType::Label;
    };
    if is_opcode(opcodes, line.clone()).is_some() {
        return LineType::Opcode;
    }
    if is_blank(line.clone()) {
        return LineType::Blank;
    }
    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        if is_comment(&mut word.to_string()) == true && i == 0 {
            return LineType::Comment;
        }
    }
    LineType::Error
}

//Returns true if line is not error
pub fn is_valid_line(opcodes: &mut Vec<Opcode>, line: String) -> bool {
    let mut myline: String = line;
    if line_type(opcodes, &mut myline) == LineType::Error {
        return false;
    }
    true
}

// Returns true if line if just whitespace
pub fn is_blank(line: String) -> bool {
    let words = line.split_whitespace();

    for (_i, word) in words.enumerate() {
        if word.len() > 0 {
            return false;
        }
    }
    true
}

// Returns true is line is just comment
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
        if item == b'/' && i == 1 && found_first == true {
            return true;
        }
    }
    false
}

// map the reigter to the hex code for the opcode
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

// Returns the hex code operand from the line, adding regiter values
pub fn add_registers(
    opcodes: &mut Vec<Opcode>,
    line: &mut String,
    msg_list: &mut MsgList,
    line_number: u32,
) -> String {
    let num_registers = num_registers(opcodes, &mut line.to_string().to_uppercase()).unwrap_or(0);

    let mut opcode_found = is_opcode(opcodes, line.to_uppercase()).unwrap_or("".to_string());
    opcode_found = opcode_found[..(4 - num_registers) as usize].to_string();
    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        if (i == 2 && num_registers == 2) || (i == 1 && (num_registers == 2 || num_registers == 1))
        {
            opcode_found = opcode_found + &map_reg_to_hex(word.to_string())
        }
    }

    if opcode_found.len() != 4 || opcode_found.find("X").is_some() {
        msg_list.push(
            format!("Incorrect register defintion - \"{}\"", line),
            Some(line_number),
            MessageType::Warning,
        );
        return "ERR ".to_string();
    }
    opcode_found
}
// Returns the hex code argument from the line
pub fn add_arguments(
    opcodes: &mut Vec<Opcode>,
    line: &mut String,
    msg_list: &mut MsgList,
    line_number: u32,
    labels: &mut Vec<Label>,
) -> String {
    let num_registers = num_registers(opcodes, &mut line.to_uppercase().to_string()).unwrap_or(0);
    let num_arguments = num_arguments(opcodes, &mut line.to_uppercase().to_string()).unwrap_or(0);
    let mut arguments = "".to_string();

    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        if i as u32 == num_registers + 1 && num_arguments == 1 {
            arguments = arguments
                + &convert_argument(
                    word.to_string().to_uppercase(),
                    msg_list,
                    line_number,
                    labels,
                )
                .unwrap_or(" ERROR    ".to_string())
        }
        if i as u32 == num_registers + 2 && num_arguments == 2 {
            arguments = arguments
                + &convert_argument(
                    word.to_string().to_uppercase(),
                    msg_list,
                    line_number,
                    labels,
                )
                .unwrap_or(" ERROR    ".to_string())
        }
        if i as u32 > num_registers + num_arguments {
            //arguments = " ERROR    ".to_string()
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

// Converts argument to label value or converts to Hex
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

    if argument.len() >= 2 {
        if argument[0..2] == "0x".to_string() || argument[0..2] == "0X".to_string() {
            let without_prefix = argument.trim_start_matches("0x");
            let without_prefix = without_prefix.trim_start_matches("0X");
            let int_value = i64::from_str_radix(without_prefix, 16);
            if int_value.is_err() {
                return None;
            }
            let ret_hex = format!("{:08X}", int_value.unwrap());
            return Some(ret_hex);
        }
    }

    match argument.parse::<i64>() {
        Ok(n) => {
            if n <= 4294967295 {
                return Some(format!("{:08X}", n).to_string());
            } else {
                msg_list.push(
                    format!("Decimal value out {} of bounds", n),
                    Some(line_number),
                    MessageType::Warning,
                    //  });
                );
                return None;
            }
        }
        Err(_e) => return None,
    };
}

// Removes comments and starting and training whitespace
pub fn strip_comments(input: &mut String) -> String {
    match input.find("//") {
        None => return input.trim().to_string(),
        Some(a) => return input[0..a].trim().to_string(),
    }
}

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

pub fn calc_checksum(input_string: &String, msg_list: &mut MsgList) -> String {
    let mut stripped_string: String = "".to_string();
    let mut checksum:u32 = 0;

    // Remove S, Z and X
    for char in input_string.chars() {
        if (char != 'S') && (char != 'Z') & (char != 'X') {
            stripped_string.push(char);
        }
    }

    // check if len is divisable by 4
    if stripped_string.len() % 4 !=0 {
        msg_list.push(
            {
                format!(
                    "Opcode list length not multiple of 4, lenght is {}",
                    stripped_string.len(),
                )
            },
            None,
            MessageType::Error,
        );
        "0000".to_string();
    }

    let mut index: usize = 0;
    let mut possition_index: u32 = 0;

    for _ in stripped_string.chars() {
        if index % 4 == 0 {
            let int_value = i64::from_str_radix(&stripped_string[index..index +4], 16);
            if int_value.is_err() {
                msg_list.push(
                    {
                        format!(
                            "Error creating opcode for invalid value {}",
                            &stripped_string[index..index +4],
                        )
                    },
                    None,
                    MessageType::Error,
                );
            }
            else {
            checksum=(checksum + int_value.unwrap_or(0) as u32)%(0xFFFF+1);
            possition_index=possition_index+1;

            }
        }
        index = index +1;
    }
    checksum=(checksum + possition_index -1)%(0xFFFF+1);
    format!("{:04X}",checksum)
}
