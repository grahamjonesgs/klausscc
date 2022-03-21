//use crate::messages;
use crate::files::*;
use crate::messages::*;

// Check if end of first word is colon if so return label
pub fn return_label(line: &String) -> Option<String> {
    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        //println!("Word {} is {}",i,word);
        if i == 0 && word.ends_with(":") {
            return Some(word.to_string());
        }
    }
    None
}

pub fn return_macro(line: &String) -> Option<String> {
    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        //println!("Word {} is {}",i,word);
        if i == 0 && word.starts_with("$") {
            return Some(word.to_string());
        }
    }
    None
}

// Return option of progam counter for label if it exists.
pub fn return_label_value(line: &String, labels: &mut Vec<Label>) -> Option<u32> {
    for label in labels {
        if label.code == line.as_str() {
            return Some(label.program_counter);
        }
    }
    None
}

// Return option of progam counter for label if it exists.
pub fn return_macro_items(line: &String, macros: &mut Vec<Macro>) -> Option<Vec<String>> {
    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        if i == 0 {
            for macro_line in macros.clone() {
                if macro_line.name == word {
                    return Some(macro_line.items.clone());
                }
            }
        }
    }
    None
}

// One pass to resolve embedded macros
pub fn expand_macros_multi(macros: Vec<Macro>, msg_list:&mut Vec<Message>) -> Vec<Macro> {
    let mut pass: u32 = 0;
    let mut changed:bool=true;
    let mut last_macro:String="".to_string();
    
    let mut input_macros=macros;

    while pass < 10 && changed {
        changed=false;
        let mut output_macros: Vec<Macro> = Vec::new();
        for input_macro_line in input_macros.clone() {
            let mut output_items: Vec<String> = Vec::new();
            for item in input_macro_line.items.clone() {
                if return_macro_items(&item, &mut input_macros.clone()).is_some() {
                    for new_item in return_macro_items(&item, &mut input_macros.clone()).unwrap() {
                        output_items.push(new_item);
                    }
                    last_macro=input_macro_line.name.clone();
                    changed=true;
                } else {
                    output_items.push(item);
                }
            }
            output_macros.push(Macro {
                name: input_macro_line.name,
                variables: 2,
                items: output_items,
            })
        }
        pass=pass+1;
        input_macros=output_macros.clone();
    }
    if changed==true {
        add_message(
            format!("Too many macro passes, check {}", last_macro),
            None,
            MessageType::Error,
            msg_list,
        );
    }
    input_macros.to_vec()
}

// Checks if first word is opcode and if so returns opcode hex value
pub fn is_opcode(opcodes: &mut Vec<Opcode>, line: &mut String) -> Option<String> {
    for opcode in opcodes {
        let words = line.split_whitespace();
        for (i, word) in words.enumerate() {
            if i == 0 && word.to_uppercase() == opcode.name {
                return Some(opcode.opcode.to_string().to_uppercase());
            }
        }
    }
    None
}

// Returns option of number of arguments for opcode
pub fn num_arguments(opcodes: &mut Vec<Opcode>, line: &mut String) -> Option<u32> {
    for opcode in opcodes {
        let words = line.split_whitespace();
        for (i, word) in words.enumerate() {
            if i == 0 && word == opcode.name {
                return Some(opcode.variables);
            }
        }
    }
    None
}

// Returns option of number of registers for opcode
pub fn num_registers(opcodes: &mut Vec<Opcode>, line: &mut String) -> Option<u32> {
    for opcode in opcodes {
        let words = line.split_whitespace();
        for (i, word) in words.enumerate() {
            if i == 0 && word == opcode.name {
                return Some(opcode.registers);
            }
        }
    }
    None
}

// Returns emum of type of line
pub fn line_type(opcodes: &mut Vec<Opcode>, line: &mut String) -> LineType {
    if return_label(line).is_some() {
        return LineType::Label;
    };
    if is_opcode(opcodes, line).is_some() {
        return LineType::Opcode;
    }
    if is_blank(line) {
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
pub fn is_valid_line(opcodes: &mut Vec<Opcode>, line: &mut String) -> bool {
    if line_type(opcodes, line) == LineType::Error {
        return false;
    }
    true
}

// Returns true if line if just whitespace
pub fn is_blank(line: &mut String) -> bool {
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
    msg_list: &mut Vec<Message>,
    line_number: u32,
) -> String {
    let num_registers = num_registers(opcodes, &mut line.to_string().to_uppercase()).unwrap_or(0);

    let mut opcode_found = is_opcode(opcodes, &mut line.to_uppercase()).unwrap_or("".to_string());
    opcode_found = opcode_found[..(4 - num_registers) as usize].to_string();
    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        if (i == 2 && num_registers == 2) || (i == 1 && (num_registers == 2 || num_registers == 1))
        {
            opcode_found = opcode_found + &map_reg_to_hex(word.to_string())
        }
    }

    if opcode_found.len() != 4 || opcode_found.find("X").is_some() {
        add_message(
            format!("Incorrect register defintion - \"{}\"", line),
            Some(line_number),
            MessageType::Warning,
            msg_list,
        );
        return "ERR ".to_string();
    }
    opcode_found
}
// Returns the hex code argument from the line
pub fn add_arguments(
    opcodes: &mut Vec<Opcode>,
    line: &mut String,
    msg_list: &mut Vec<Message>,
    line_number: u32,
    labels: &mut Vec<Label>,
) -> String {
    let num_registers = num_registers(opcodes, &mut line.to_uppercase().to_string()).unwrap_or(0);
    let num_arguments = num_arguments(opcodes, &mut line.to_uppercase().to_string()).unwrap_or(0);
    let mut arguments = "".to_string();

    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        if i == num_registers as usize + 1 && num_arguments == 1 {
            arguments = arguments
                + &convert_argument(
                    word.to_string().to_uppercase(),
                    msg_list,
                    line_number,
                    labels,
                )
                .unwrap_or(" ERROR    ".to_string())
        }
        if i == num_registers as usize + 2 && num_arguments == 2 {
            arguments = arguments
                + &convert_argument(
                    word.to_string().to_uppercase(),
                    msg_list,
                    line_number,
                    labels,
                )
                .unwrap_or(" ERROR    ".to_string())
        }
        if i > num_registers as usize + num_arguments as usize {
            //arguments = " ERROR    ".to_string()
            add_message(
                format!("Too many arguments found - \"{}\"", line),
                Some(line_number),
                MessageType::Warning,
                msg_list,
            );
        }
    }

    if arguments.len() != 8 * num_arguments as usize {
        add_message(
            format!("Incorrect argument defintion - \"{}\"", line),
            Some(line_number),
            MessageType::Error,
            msg_list,
        );
    }
    arguments
}

// Converts argument to label value or converts to Hex
pub fn convert_argument(
    argument: String,
    msg_list: &mut Vec<Message>,
    line_number: u32,
    labels: &mut Vec<Label>,
) -> Option<String> {
    if return_label(&argument).is_some() {
        match return_label_value(&argument, labels) {
            Some(n) => return Some(format!("{:08X}", n)),
            None => {
                add_message(
                    format!("Label {} not found - line {}", argument, line_number),
                    Some(line_number),
                    MessageType::Warning,
                    msg_list,
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
                add_message(
                    format!("Decimal value out {} of bounds", n),
                    Some(line_number),
                    MessageType::Warning,
                    msg_list,
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
