use crate::{messages::{MessageType, MsgList}, helper::data_name_from_string};

#[derive(Clone)]
pub struct Label {
    pub program_counter: u32,
    pub name: String,
    pub line_counter: u32,
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

