use crate::messages::{MessageType, MsgList};
use crate::helper::data_name_from_string;
use crate::opcodes::Pass1;

#[derive(Clone,Debug,PartialEq)]
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
    let argument_trim = argument.trim();
    if label_name_from_string(argument_trim).is_some() {
        match return_label_value(argument_trim, labels) {
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

    if data_name_from_string(argument_trim).is_some() {
        match return_label_value(argument_trim, labels) {
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

    if argument_trim.len() >= 2 && (argument_trim[0..2] == *"0x" || argument_trim[0..2] == *"0X") {
        let without_prefix = argument_trim.trim_start_matches("0x");
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

    match argument_trim.parse::<i64>() {
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

/// Create the vector of labels
///
/// Takes the vector of pass 1 with the line numbers in it, and return a vector of all labels
#[allow(clippy::module_name_repetitions)]
pub fn get_labels(pass1: &[Pass1]) -> Vec<Label> {
    let labels: Vec<Label> = pass1
        .iter()
        .filter(|n| {
            label_name_from_string(&n.input).is_some() || data_name_from_string(&n.input).is_some()
        })
        .map(|n| -> Label {
            Label {
                program_counter: n.program_counter,
                name: {
                    let this = label_name_from_string(&n.input);
                    match this {
                        Some(x) => x,
                        None => {
                            let this = data_name_from_string(&n.input);
                            let default = String::new();
                            match this {
                                Some(x) => x,
                                None => default,
                            }
                        }
                    }
                },
                line_counter: n.line_counter,
            }
        })
        .collect();
    labels
}

#[cfg(test)]
mod tests {
    use crate::files::LineType;

    use super::*;

    #[test]
    fn test_label_name_from_string() {
        assert_eq!(label_name_from_string("label:"), Some("label:".to_string()));
        assert_eq!(label_name_from_string("label: "), Some("label:".to_string()));
        assert_eq!(label_name_from_string("label :"), None);
        assert_eq!(label_name_from_string("label : "), None);
        assert_eq!(label_name_from_string("label"), None);
        assert_eq!(label_name_from_string("label "), None);
        assert_eq!(label_name_from_string(" label"), None);
        assert_eq!(label_name_from_string(" label "), None);
    }

    #[test]
    fn test_return_label_value() {
        let mut labels = vec![
            Label {
                program_counter: 0,
                name: "label1".to_string(),
                line_counter: 0,
            },
            Label {
                program_counter: 1,
                name: "label2".to_string(),
                line_counter: 0,
            },
        ];
        assert_eq!(return_label_value("label1", &mut labels), Some(0));
        assert_eq!(return_label_value("label2", &mut labels), Some(1));
        assert_eq!(return_label_value("label3", &mut labels), None);
    }

    #[test]
    fn test_find_duplicate_label() {
        let mut labels = vec![
            Label {
                program_counter: 0,
                name: "label1".to_string(),
                line_counter: 0,
            },
            Label {
                program_counter: 1,
                name: "label2".to_string(),
                line_counter: 0,
            },
            Label {
                program_counter: 2,
                name: "label1".to_string(),
                line_counter: 3,
            },
        ];
        let mut msg_list = MsgList::new();
        find_duplicate_label(&mut labels, &mut msg_list);
        assert_eq!(msg_list.number_errors()   , 1);
        assert_eq!(msg_list.number_warnings() , 0);
       
       assert_eq!(msg_list.list[0].name, "Duplicate label label1 found, with differing values");
        }

    #[test]
    fn test_convert_argument() {
        let mut labels = vec![
            Label {
                program_counter: 1,
                name: "label1:".to_string(),
                line_counter: 1,
            },
            Label {
                program_counter: 2,
                name: "label2:".to_string(),
                line_counter: 2,
            },
        ];
        let mut msg_list = MsgList::new();
        assert_eq!(convert_argument("label1", &mut msg_list, 0, &mut labels), None);
        assert_eq!(convert_argument("label2", &mut msg_list, 1, &mut labels), None);
        assert_eq!(convert_argument("label3", &mut msg_list, 2, &mut labels), None);
        assert_eq!(convert_argument("0x1234", &mut msg_list, 3, &mut labels), Some("00001234".to_string()));
        assert_eq!(convert_argument("0x123456789", &mut msg_list, 4, &mut labels), None);
        assert_eq!(convert_argument("1234", &mut msg_list, 5, &mut labels), Some("000004D2".to_string()));
        assert_eq!(convert_argument("123456789", &mut msg_list, 6, &mut labels), Some("075BCD15".to_string()));
        assert_eq!(convert_argument("label1:", &mut msg_list, 7, &mut labels), Some("00000001".to_string()));
        assert_eq!(convert_argument("label1: ", &mut msg_list, 8, &mut labels), Some("00000001".to_string()));
        assert_eq!(convert_argument("label1 :", &mut msg_list, 9, &mut labels), None);
        assert_eq!(convert_argument("label1 : ", &mut msg_list, 10, &mut labels), None);
        assert_eq!(convert_argument("label1", &mut msg_list, 11, &mut labels), None);
        assert_eq!(convert_argument("label1 ", &mut msg_list, 12, &mut labels), None);
        assert_eq!(convert_argument(" label1", &mut msg_list, 13, &mut labels), None);
        assert_eq!(convert_argument("label2:", &mut msg_list, 14, &mut labels), Some("00000002".to_string()));
        assert_eq!(convert_argument("label3:", &mut msg_list, 14, &mut labels), None);
        assert_eq!(msg_list.list[msg_list.list.len()-1].name, "Label label3: not found - line 14".to_string());
    }

    #[test]
    // Test that the labels are correctly extracted from the pass1 list
    fn test_get_labels() {
        let pass1 = vec![
            Pass1 {program_counter:0,line_counter:0,input:"label1:".to_string(), line_type:LineType::Label },
            Pass1 {program_counter:2,line_counter:0,input:"xxxx".to_string(), line_type:LineType::Opcode },
            Pass1 {program_counter:4,line_counter:1,input:"label2:".to_string(), line_type:LineType::Label },
            Pass1 {program_counter:6,line_counter:2,input:"label3:".to_string(), line_type:LineType::Label }];
        let labels = get_labels(&pass1);
        assert_eq!(labels[0], Label { program_counter: 0, name: "label1:".to_string(), line_counter: 0 });
        assert_eq!(labels[1], Label { program_counter: 4, name: "label2:".to_string(), line_counter: 1 });
        assert_eq!(labels[2], Label { program_counter: 6, name: "label3:".to_string(), line_counter: 2 });
    }
            
}