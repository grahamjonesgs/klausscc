use crate::helper::data_name_from_string;
use crate::messages::{MessageType, MsgList};
use crate::opcodes::Pass1;

#[derive(Clone, Debug, PartialEq, Eq)]
/// Label struct
pub struct Label {
    /// Program counter derived from Pass1
    pub program_counter: u32,
    /// Label name as text without colon
    pub name: String,
}

#[allow(clippy::missing_docs_in_private_items)]
impl Default for & Label {
    fn default() -> &'static Label {
        static VALUE: Label = Label {
            program_counter: 0,
            name: String::new(),   
        };
        &VALUE

    }
}

/// Extracts label from string
///
/// Checks if end of first word is colon if so return label as option string
pub fn label_name_from_string(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");
    if first_word.ends_with(':') {
        return Some(first_word.to_owned());
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
                None,
                None,
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
    filename: String,
    labels: &mut Vec<Label>,
) -> Option<String> {
    let argument_trim = argument.trim();
    if label_name_from_string(argument_trim).is_some() {
        if let Some(n) = return_label_value(argument_trim, labels) {
            return Some(format!("{n:08X}"));
        }

        msg_list.push(
            format!("Label {argument} not found - line {line_number}"),
            Some(line_number),
            Some(filename),
            MessageType::Warning,
        );
        return None;
    }

    if data_name_from_string(argument_trim).is_some() {
        if let Some(n) = return_label_value(argument_trim, labels) {
            return Some(format!("{n:08X}"));
        }
        msg_list.push(
            format!("Label {argument} not found"),
            Some(line_number),
            Some(filename),
            MessageType::Warning,
        );
        return None;
    }

    if argument_trim.len() >= 2
        && (argument_trim.get(0..2).unwrap_or("  ") == "0x"
            || argument_trim.get(0..2).unwrap_or("  ") == "0X")
    {
        let without_prefix1 = argument_trim.trim_start_matches("0x");
        let without_prefix2 = without_prefix1.trim_start_matches("0X");
        let int_value_result = i64::from_str_radix(&without_prefix2.replace('_', ""), 16);
        if int_value_result.is_err() {
            msg_list.push(
                format!("Hex value {argument} incorrect"),
                Some(line_number),
                Some(filename),
                MessageType::Warning,
            );
            return None;
        }
        let int_value = int_value_result.unwrap_or(0);

        if int_value <= 0xFFFF_FFFF {
            return Some(format!("{int_value:08X}"));
        }
        msg_list.push(
            format!("Hex value out 0x{int_value:08X} of bounds"),
            Some(line_number),
            Some(filename),
            MessageType::Warning,
        );
        return None;
    }

    match argument_trim.parse::<i64>() {
        Ok(n) => {
            if n <= 0xFFFF_FFFF {
                return Some(format!("{n:08X}"));
            }
            msg_list.push(
                format!("Decimal value out {n} of bounds"),
                Some(line_number),
                Some(filename),
                MessageType::Warning,
            );
        }
        Err(_e) => {
            msg_list.push(
                format!("Decimal value {argument} incorrect"),
                Some(line_number),
                Some(filename),
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
pub fn get_labels(pass1: &[Pass1], msg_list: &mut MsgList) -> Vec<Label> {
    let labels: Vec<Label> = pass1
        .iter()
        .filter(|n| {
            label_name_from_string(&n.input_text_line).is_some()
                || data_name_from_string(&n.input_text_line).is_some()
        })
        .map(|n| -> Label {
            Label {
                program_counter: n.program_counter,
                name: {
                    let this = label_name_from_string(&n.input_text_line);
                    this.map_or_else(
                        || data_name_from_string(&n.input_text_line).unwrap_or_default(),
                        |x| x,
                    )
                },
            }
        })
        .collect();
    for line in pass1.iter() {
        if label_name_from_string(&line.input_text_line).is_some() {
            let mut words = line.input_text_line.split_whitespace();
            let first_word = words.next().unwrap_or("");
            let second_word = words.next();
            if second_word.is_some() {
                msg_list.push(
                    format!(
                        "Label {first_word} has extra text {}",
                        second_word.unwrap_or_default()
                    ),
                    Some(line.line_counter),
                    Some(line.file_name.clone()),
                    MessageType::Warning,
                );
            }
        }
    }
    for line in pass1.iter() {
        if data_name_from_string(&line.input_text_line).is_some() {
            let mut words = line.input_text_line.split_whitespace();
            let first_word = words.next().unwrap_or("");
            let remaining_line = line.input_text_line.trim_start_matches(first_word).trim();
            let second_word = words.next();
            let third_word = words.next();
            if third_word.is_some() && !second_word.unwrap_or_default().starts_with('\"') {
                msg_list.push(
                    format!(
                        "Data {first_word} has extra text {}",
                        third_word.unwrap_or_default()
                    ),
                    Some(line.line_counter),
                    Some(line.file_name.clone()),
                    MessageType::Warning,
                );
            }
            if remaining_line.starts_with('\"') && !remaining_line.ends_with('\"') {
                msg_list.push(
                    format!("Data {first_word} has no string termination"),
                    Some(line.line_counter),
                    Some(line.file_name.clone()),
                    MessageType::Warning,
                );
            }
        }
    }

    labels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::LineType;

    #[test]
    // Check that labels are correctly extracted from strings test for label
    fn test_label_name_from_string1() {
        assert_eq!(label_name_from_string("label:"), Some("label:".to_owned()));
        assert_eq!(label_name_from_string("label: "), Some("label:".to_owned()));
        assert_eq!(label_name_from_string("label:"), Some("label:".to_owned()));
        assert_eq!(
            label_name_from_string("     label:"),
            Some("label:".to_owned())
        );
        assert_eq!(
            label_name_from_string("     label: dummy words"),
            Some("label:".to_owned())
        );
    }

    #[test]
    // Check that none is returned if not a label
    fn test_label_name_from_string2() {
        assert_eq!(label_name_from_string("label :"), None);
        assert_eq!(label_name_from_string("label : "), None);
        assert_eq!(label_name_from_string("label"), None);
        assert_eq!(label_name_from_string("label "), None);
        assert_eq!(label_name_from_string(" label"), None);
        assert_eq!(label_name_from_string(" label "), None);
        assert_eq!(label_name_from_string("lab:el"), None);
        assert_eq!(label_name_from_string("  xxxxxxx   label:"), None);
    }

    #[test]
    // Check that labels are correctly extracted from strings test for data
    fn test_return_label_value1() {
        let mut labels = vec![
            Label {
                program_counter: 0,
                name: "label1".to_owned(),
            },
            Label {
                program_counter: 1,
                name: "label2".to_owned(),
            },
        ];
        assert_eq!(return_label_value("label1", &mut labels), Some(0));
        assert_eq!(return_label_value("label2", &mut labels), Some(1));
    }

    #[test]
    // Check that none is returned if not a label
    fn test_return_label_value2() {
        let mut labels = vec![
            Label {
                program_counter: 0,
                name: "label1".to_owned(),
            },
            Label {
                program_counter: 1,
                name: "label2".to_owned(),
            },
        ];
        assert_eq!(return_label_value("label3", &mut labels), None);
    }

    #[test]
    // Test duplicate label names are identitiied
    fn test_find_duplicate_label() {
        let mut labels = vec![
            Label {
                program_counter: 0,
                name: "label1".to_owned(),
            },
            Label {
                program_counter: 1,
                name: "label2".to_owned(),
            },
            Label {
                program_counter: 2,
                name: "label1".to_owned(),
            },
        ];
        let mut msg_list = MsgList::new();
        find_duplicate_label(&mut labels, &mut msg_list);
        assert_eq!(msg_list.number_errors(), 1);
        assert_eq!(msg_list.number_warnings(), 0);

        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            "Duplicate label label1 found, with differing values"
        );
    }

    #[test]
    // Test convertion is correct for value argumanets
    fn test_convert_argument1() {
        let mut labels = vec![
            Label {
                program_counter: 1,
                name: "label1:".to_owned(),
            },
            Label {
                program_counter: 2,
                name: "label2:".to_owned(),
            },
            Label {
                program_counter: 30,
                name: "#data1".to_owned(),
            },
        ];
        let mut msg_list = MsgList::new();
        assert_eq!(
            convert_argument("0x1234", &mut msg_list, 3, "test".to_owned(), &mut labels),
            Some("00001234".to_owned())
        );
        assert_eq!(
            convert_argument("1234", &mut msg_list, 5, "test".to_owned(), &mut labels),
            Some("000004D2".to_owned())
        );
        assert_eq!(
            convert_argument(
                "123456789",
                &mut msg_list,
                6,
                "test".to_owned(),
                &mut labels
            ),
            Some("075BCD15".to_owned())
        );
        assert_eq!(
            convert_argument("label1:", &mut msg_list, 7, "test".to_owned(), &mut labels),
            Some("00000001".to_owned())
        );
        assert_eq!(
            convert_argument("label1: ", &mut msg_list, 8, "test".to_owned(), &mut labels),
            Some("00000001".to_owned())
        );

        assert_eq!(
            convert_argument("label2:", &mut msg_list, 14, "test".to_owned(), &mut labels),
            Some("00000002".to_owned())
        );

        assert_eq!(
            convert_argument("#data1", &mut msg_list, 14, "test".to_owned(), &mut labels),
            Some("0000001E".to_owned())
        );
    }

    #[test]
    // Test for convert_argument if the argument is invalid with ocrrect message
    fn test_convert_argument2() {
        let mut labels = vec![
            Label {
                program_counter: 1,
                name: "label1:".to_owned(),
            },
            Label {
                program_counter: 2,
                name: "label2:".to_owned(),
            },
            Label {
                program_counter: 30,
                name: "#data1".to_owned(),
            },
        ];
        let mut msg_list = MsgList::new();

        // Check for non label text
        assert_eq!(
            convert_argument("label1", &mut msg_list, 0, "test".to_owned(), &mut labels),
            None
        );
        assert_eq!(
            msg_list.list.last().unwrap_or_default().text,
            "Decimal value label1 incorrect".to_owned()
        );

        // Check for hex value out of bounds
        assert_eq!(
            convert_argument(
                "0x123456789",
                &mut msg_list,
                4,
                "test".to_owned(),
                &mut labels
            ),
            None
        );
        assert_eq!(
            msg_list.list.last().unwrap_or_default().text,
            "Hex value out 0x123456789 of bounds".to_owned()
        );

        // Check for label not defined
        assert_eq!(
            convert_argument("label3:", &mut msg_list, 14, "test".to_owned(), &mut labels),
            None
        );
        assert_eq!(
            msg_list.list.last().unwrap_or_default().text,
            "Label label3: not found - line 14".to_owned()
        );

        // Check for invalid decimal value
        assert_eq!(
            convert_argument(
                "4294967296",
                &mut msg_list,
                14,
                "test".to_owned(),
                &mut labels
            ),
            None
        );
        assert_eq!(
            msg_list.list.last().unwrap_or_default().text,
            "Decimal value out 4294967296 of bounds".to_owned()
        );

        // Check for data not defined
        assert_eq!(
            convert_argument("#data2", &mut msg_list, 15, "test".to_owned(), &mut labels),
            None
        );
        assert_eq!(
            msg_list.list.last().unwrap_or_default().text,
            "Label #data2 not found".to_owned()
        );

        // Check for invalid hex value
        assert_eq!(
            convert_argument("0xGGG", &mut msg_list, 14, "test".to_owned(), &mut labels),
            None
        );
        assert_eq!(
            msg_list.list.last().unwrap_or_default().text,
            "Hex value 0xGGG incorrect".to_owned()
        );
    }

    #[test]
    // Test that the labels are correctly extracted from the pass1 list
    #[allow(clippy::too_many_lines)]
    fn test_get_labels() {
        let msglist = &mut MsgList::new();
        let pass1 = vec![
            Pass1 {
                program_counter: 0,
                file_name: String::from("test"),
                line_counter: 0,
                input_text_line: "label1:".to_owned(),
                line_type: LineType::Label,
            },
            Pass1 {
                program_counter: 2,
                file_name: String::from("test"),
                line_counter: 0,
                input_text_line: "xxxx".to_owned(),
                line_type: LineType::Opcode,
            },
            Pass1 {
                program_counter: 4,
                file_name: String::from("test"),
                line_counter: 1,
                input_text_line: "label2:".to_owned(),
                line_type: LineType::Label,
            },
            Pass1 {
                program_counter: 6,
                file_name: String::from("test"),
                line_counter: 2,
                input_text_line: "label3:".to_owned(),
                line_type: LineType::Label,
            },
            Pass1 {
                program_counter: 7,
                file_name: String::from("test"),
                line_counter: 3,
                input_text_line: "#data321".to_owned(),
                line_type: LineType::Label,
            },
            Pass1 {
                program_counter: 7,
                file_name: String::from("test"),
                line_counter: 4,
                input_text_line: "test".to_owned(),
                line_type: LineType::Label,
            },
            Pass1 {
                program_counter: 8,
                file_name: String::from("test"),
                line_counter: 7,
                input_text_line: "label1: dummy".to_owned(),
                line_type: LineType::Label,
            },
            Pass1 {
                program_counter: 7,
                file_name: String::from("test"),
                line_counter: 3,
                input_text_line: "#data123 0xFFFF dummy2".to_owned(),
                line_type: LineType::Label,
            },
            Pass1 {
                program_counter: 8,
                file_name: String::from("test"),
                line_counter: 3,
                input_text_line: "#data2 \"TEST WITH SPACES\"".to_owned(),
                line_type: LineType::Label,
            },
            Pass1 {
                program_counter: 9,
                file_name: String::from("test"),
                line_counter: 3,
                input_text_line: "#data3 \"TEST WITH NO TERMINATION".to_owned(),
                line_type: LineType::Label,
            },
        ];
        let labels = get_labels(&pass1, msglist);
        assert_eq!(
            *labels.get(0).unwrap_or_default(),
            Label {
                program_counter: 0,
                name: "label1:".to_owned(),
            }
        );
        assert_eq!(
            *labels.get(1).unwrap_or_default(),
            Label {
                program_counter: 4,
                name: "label2:".to_owned(),
            }
        );
        assert_eq!(
            *labels.get(2).unwrap_or_default(),
            Label {
                program_counter: 6,
                name: "label3:".to_owned(),
            }
        );
        assert_eq!(
            *labels.get(3).unwrap_or_default(),
            Label {
                program_counter: 7,
                name: "#data321".to_owned(),
            }
        );
        assert_eq!(msglist.list.get(0).unwrap_or_default().text, "Label label1: has extra text dummy");
        assert_eq!(msglist.list.get(1).unwrap_or_default().text, "Data #data123 has extra text dummy2");
        assert_eq!(
            msglist.list.get(2).unwrap_or_default().text,
            "Data #data3 has no string termination"
        );
    }
}
