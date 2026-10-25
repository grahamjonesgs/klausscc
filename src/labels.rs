use crate::asm::lexer::{data_name_from_string, label_name_from_string, strip_comments};
use crate::asm::line::Pass1;
use crate::diag::Diagnostics;
use crate::num::{fits_imm32, is_hex_literal, parse_bits64, parse_int, LiteralError};

#[derive(Clone, Debug, PartialEq, Eq)]
/// Label struct.
pub(crate) struct Label {
    /// Label name as text without colon.
    pub name: String,
    /// Program counter derived from Pass1.
    pub program_counter: u32,
}

/// Gets address from label or absolute values.
///
/// Converts argument to label value or converts to Hex. An undefined label or
/// a malformed or out-of-range literal is reported as an error at the line and
/// returns `None`.
pub(crate) fn convert_argument(argument: &str, msg_list: &mut Diagnostics, line_number: u32, filename: &str, labels: &[Label]) -> Option<String> {
    let argument_trim = argument.trim();
    let mut error = |text: String| msg_list.error_at(filename, line_number, text);

    if label_name_from_string(argument_trim).is_some() || data_name_from_string(argument_trim).is_some() {
        if let Some(n) = return_label_value(argument_trim, labels) {
            return Some(format!("{n:08X}"));
        }
        error(format!("Label {argument} not found"));
        return None;
    }

    if is_hex_literal(argument_trim) {
        // Read as a 64-bit pattern so full-width literals (e.g. 0xFFFFFFFFFFFFFFFF)
        // don't overflow, then reinterpret as i64 for the same bounds check as the
        // decimal path. Values like 0xFFFFFFFFFFFFFFFF (-1) are in [i32::MIN, 0xFFFF_FFFF]
        // and emit FFFFFFFF; the CPU sign-extends the 32-bit immediate to 64 bits,
        // giving the correct 64-bit value.
        match parse_bits64(argument_trim) {
            Ok(bits) => {
                let value = bits as i64;
                if fits_imm32(i128::from(value)) {
                    return Some(format!("{:08X}", value as u32));
                }
                error(format!("Hex value 0x{bits:016X} out of bounds"));
            }
            Err(LiteralError::Overflow) => error(format!("Hex value {argument} out of bounds")),
            Err(_) => error(format!("Hex value {argument} incorrect")),
        }
        return None;
    }

    match parse_int(argument_trim) {
        Ok(n) => {
            if fits_imm32(n) {
                return Some(format!("{:08X}", n as u32));
            }
            error(format!("Decimal value {n} out of bounds"));
        }
        Err(LiteralError::Overflow) => error(format!("Decimal value {argument} out of bounds")),
        Err(_) => {
            // Try resolving as a label name without colon suffix
            let label_with_colon = format!("{argument_trim}:");
            if let Some(n) = return_label_value(&label_with_colon, labels) {
                return Some(format!("{n:08X}"));
            }
            error(format!("Decimal value {argument} incorrect"));
        }
    }
    None
}

/// Check if label is duplicate.
///
/// Check if label is duplicate, and output message if duplicate is found.
pub(crate) fn find_duplicate_label(labels: &[Label], msg_list: &mut Diagnostics) {
    for label in labels {
        let opt_found_line = return_label_value(&label.name, labels);
        if opt_found_line.unwrap_or(0) != label.program_counter {
            msg_list.error(format!("Duplicate label {} found, with differing values", label.name));
        }
    }
}

/// Create the vector of labels.
///
/// Takes the vector of pass 1 with the line numbers in it, and return a vector of all labels.
pub(crate) fn get_labels(pass1: &[Pass1], msg_list: &mut Diagnostics) -> Vec<Label> {
    let labels: Vec<Label> = pass1
        .iter()
        .filter(|n| label_name_from_string(&n.input_text_line).is_some() || data_name_from_string(&n.input_text_line).is_some())
        .map(|n| -> Label {
            Label {
                program_counter: n.program_counter,
                name: {
                    let this = label_name_from_string(&n.input_text_line);
                    this.unwrap_or_else(|| data_name_from_string(&n.input_text_line).unwrap_or_default())
                },
            }
        })
        .collect();
    for line in pass1 {
        if label_name_from_string(&line.input_text_line).is_some() {
            let stripped = strip_comments(&line.input_text_line);
            let mut words = stripped.split_whitespace();
            let first_word = words.next().unwrap_or("");
            let second_word = words.next();
            if second_word.is_some() {
                msg_list.warning_at(
                    &line.file_name,
                    line.line_counter,
                    format!("Label {first_word} has extra text {}", second_word.unwrap_or_default()),
                );
            }
        }
    }
    for line in pass1 {
        if data_name_from_string(&line.input_text_line).is_some() {
            let stripped = strip_comments(&line.input_text_line);
            let mut words = stripped.split_whitespace();
            let first_word = words.next().unwrap_or("");
            let remaining_line = stripped.trim_start_matches(first_word).trim().to_owned();
            let second_word = words.next();
            let third_word = words.next();
            if third_word.is_some() && !second_word.unwrap_or_default().starts_with('\"') {
                msg_list.warning_at(
                    &line.file_name,
                    line.line_counter,
                    format!("Data {first_word} has extra text {}", third_word.unwrap_or_default()),
                );
            }
            if remaining_line.starts_with('\"') && !remaining_line.ends_with('\"') {
                msg_list.warning_at(&line.file_name, line.line_counter, format!("Data {first_word} has no string termination"));
            }
        }
    }

    labels
}

/// Return program counter for label.
///
/// Return option of program counter for label if it exists, or None.
pub(crate) fn return_label_value(line: &str, labels: &[Label]) -> Option<u32> {
    let line_upper = line.to_uppercase();
    for label in labels {
        if label.name.to_uppercase() == line_upper {
            return Some(label.program_counter);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::line::LineType;
    use crate::diag::MessageType;

    #[test]
    // Check that labels are correctly extracted from strings test for data
    fn test_return_label_value1() {
        let labels = vec![
            Label {
                program_counter: 0,
                name: "label1".to_owned(),
            },
            Label {
                program_counter: 1,
                name: "label2".to_owned(),
            },
        ];
        assert_eq!(return_label_value("label1", &labels), Some(0));
        assert_eq!(return_label_value("label2", &labels), Some(1));
    }

    #[test]
    // Check that none is returned if not a label
    fn test_return_label_value2() {
        let labels = vec![
            Label {
                program_counter: 0,
                name: "label1".to_owned(),
            },
            Label {
                program_counter: 1,
                name: "label2".to_owned(),
            },
        ];
        assert_eq!(return_label_value("label3", &labels), None);
    }

    #[test]
    // Test duplicate label names are identified
    fn test_find_duplicate_label() {
        let labels = vec![
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
        let mut msg_list = Diagnostics::new();
        find_duplicate_label(&labels, &mut msg_list);
        assert_eq!(msg_list.count(MessageType::Error), 1);
        assert_eq!(msg_list.count(MessageType::Warning), 0);

        assert_eq!(msg_list.messages()[0].text(), "Duplicate label label1 found, with differing values");
    }

    #[test]
    // Test conversion is correct for value arguments
    fn test_convert_argument1() {
        let labels = vec![
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
        let mut msg_list = Diagnostics::new();
        assert_eq!(convert_argument("0x1234", &mut msg_list, 3, "test", &labels), Some("00001234".to_owned()));
        assert_eq!(convert_argument("1234", &mut msg_list, 5, "test", &labels), Some("000004D2".to_owned()));
        assert_eq!(
            convert_argument("123456789", &mut msg_list, 6, "test", &labels),
            Some("075BCD15".to_owned())
        );
        assert_eq!(
            convert_argument("label1:", &mut msg_list, 7, "test", &labels),
            Some("00000001".to_owned())
        );
        assert_eq!(
            convert_argument("label1: ", &mut msg_list, 8, "test", &labels),
            Some("00000001".to_owned())
        );

        assert_eq!(
            convert_argument("label2:", &mut msg_list, 14, "test", &labels),
            Some("00000002".to_owned())
        );

        assert_eq!(
            convert_argument("#data1", &mut msg_list, 14, "test", &labels),
            Some("0000001E".to_owned())
        );
    }

    #[test]
    // Test for convert_argument if the argument is invalid with correct message
    fn test_convert_argument2() {
        let labels = vec![
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
        let mut msg_list = Diagnostics::new();

        // Check for label reference without colon suffix
        assert_eq!(convert_argument("label1", &mut msg_list, 0, "test", &labels), Some("00000001".to_owned()));

        // Check for unknown label text
        assert_eq!(convert_argument("unknown", &mut msg_list, 0, "test", &labels), None);
        assert_eq!(msg_list.messages().last().unwrap().text(), "Decimal value unknown incorrect".to_owned());

        // Check for hex value out of bounds
        assert_eq!(convert_argument("0x123456789", &mut msg_list, 4, "test", &labels), None);
        assert_eq!(
            msg_list.messages().last().unwrap().text(),
            "Hex value 0x0000000123456789 out of bounds".to_owned()
        );

        // Check for label not defined
        assert_eq!(convert_argument("label3:", &mut msg_list, 14, "test", &labels), None);
        assert_eq!(msg_list.messages().last().unwrap().text(), "Label label3: not found".to_owned());

        // Check for invalid decimal value
        assert_eq!(convert_argument("4294967296", &mut msg_list, 14, "test", &labels), None);
        assert_eq!(
            msg_list.messages().last().unwrap().text(),
            "Decimal value 4294967296 out of bounds".to_owned()
        );

        // Check for data not defined
        assert_eq!(convert_argument("#data2", &mut msg_list, 15, "test", &labels), None);
        assert_eq!(msg_list.messages().last().unwrap().text(), "Label #data2 not found".to_owned());

        // Check for invalid hex value
        assert_eq!(convert_argument("0xGGG", &mut msg_list, 14, "test", &labels), None);
        assert_eq!(msg_list.messages().last().unwrap().text(), "Hex value 0xGGG incorrect".to_owned());
    }

    #[test]
    // Regression: undefined labels and bad literals were warnings, so the line
    // assembled as 00000000 and the build succeeded. They are now errors at the line.
    fn test_convert_argument_failures_are_errors() {
        let labels = vec![Label {
            program_counter: 8,
            name: "loop:".to_owned(),
        }];
        for bad in [
            "nowhere:",
            "#nodata",
            "nowhere",
            "0xGG",
            "12abc",
            "0x123456789",
            "4294967296",
            "99999999999999999999",
        ] {
            let mut msg_list = Diagnostics::new();
            assert_eq!(convert_argument(bad, &mut msg_list, 9, "prog.kla", &labels), None, "{bad}");
            assert_eq!(msg_list.count(MessageType::Error), 1, "{bad}");
            assert_eq!(msg_list.count(MessageType::Warning), 0, "{bad}");
            let msg = msg_list.messages().first().unwrap();
            assert_eq!((msg.line_number(), msg.file_name()), (Some(9), Some("prog.kla")), "{bad}");
            assert!(
                !msg.text().contains("- line"),
                "location is attached, not repeated in the text: {}",
                msg.text()
            );
        }
    }

    #[test]
    // Literal forms shared with the rest of the assembler (num::parse_int).
    fn test_convert_argument_literal_forms() {
        let labels = Vec::new();
        let mut msg_list = Diagnostics::new();
        let mut convert = |arg: &str| convert_argument(arg, &mut msg_list, 1, "t", &labels);
        assert_eq!(convert("0x1_0"), Some("00000010".to_owned()));
        assert_eq!(convert("0XFF"), Some("000000FF".to_owned()));
        assert_eq!(convert("-1"), Some("FFFFFFFF".to_owned()));
        assert_eq!(convert("-0x10"), Some("FFFFFFF0".to_owned()));
        assert_eq!(convert("0xFFFFFFFFFFFFFFFF"), Some("FFFFFFFF".to_owned()));
        assert_eq!(convert("-2147483648"), Some("80000000".to_owned()));
        assert_eq!(convert("-2147483649"), None);
        assert_eq!(msg_list.messages().last().unwrap().text(), "Decimal value -2147483649 out of bounds");
    }

    #[test]
    // Test that the labels are correctly extracted from the pass1 list
    fn test_get_labels() {
        let msglist = &mut Diagnostics::new();
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
            labels[0],
            Label {
                program_counter: 0,
                name: "label1:".to_owned(),
            }
        );
        assert_eq!(
            labels[1],
            Label {
                program_counter: 4,
                name: "label2:".to_owned(),
            }
        );
        assert_eq!(
            labels[2],
            Label {
                program_counter: 6,
                name: "label3:".to_owned(),
            }
        );
        assert_eq!(
            labels[3],
            Label {
                program_counter: 7,
                name: "#data321".to_owned(),
            }
        );
        assert_eq!(msglist.messages()[0].text(), "Label label1: has extra text dummy");
        assert_eq!(msglist.messages()[1].text(), "Data #data123 has extra text dummy2");
        assert_eq!(msglist.messages()[2].text(), "Data #data3 has no string termination");
    }
}
