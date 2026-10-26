//! The symbol table: the program's labels and their addresses, and turning
//! an operand (a label or a literal) into its value.

use crate::asm::lexer::{data_name_from_string, label_name_from_string, strip_comments};
use crate::asm::line::Pass1;
use crate::diag::Diagnostics;
use crate::num::{LiteralError, fits_imm32, is_hex_literal, parse_bits64, parse_int};
use std::collections::HashMap;
use std::collections::hash_map::Entry;

/// Where a label or data name was defined, and its address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Symbol {
    /// The address the name stands for.
    pub(crate) address: u32,
    /// The file the name was defined in.
    pub(crate) file_name: String,
    /// The line of `file_name` the name was defined on.
    pub(crate) line_counter: u32,
}

/// The program's labels (`name:`) and data names (`#name`).
///
/// Names are looked up without regard to case, and a label's trailing `:`
/// is optional, so `loop:`, `LOOP:` and `loop` are one symbol.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SymbolTable(HashMap<String, Symbol>);

impl SymbolTable {
    /// The key `name` is stored under: upper case, without a trailing `:`.
    fn key(name: &str) -> String {
        name.strip_suffix(':').unwrap_or(name).to_uppercase()
    }

    /// Define `name` as `symbol`.
    ///
    /// When the name is already defined the table keeps the first
    /// definition, which is returned as the error.
    pub(crate) fn insert(&mut self, name: &str, symbol: Symbol) -> Result<(), &Symbol> {
        match self.0.entry(Self::key(name)) {
            Entry::Vacant(entry) => {
                entry.insert(symbol);
                Ok(())
            }
            Entry::Occupied(entry) => Err(entry.into_mut()),
        }
    }

    /// The address of `name`, if it is defined.
    pub(crate) fn address(&self, name: &str) -> Option<u32> {
        self.0.get(&Self::key(name)).map(|symbol| symbol.address)
    }

    /// A table of `(name, address)` pairs, defined at line 0 of `test`.
    #[cfg(test)]
    pub(crate) fn from_pairs(pairs: &[(&str, u32)]) -> Self {
        let mut table = Self::default();
        for &(name, address) in pairs {
            let symbol = Symbol {
                address,
                file_name: "test".to_owned(),
                line_counter: 0,
            };
            assert!(table.insert(name, symbol).is_ok(), "{name} defined twice");
        }
        table
    }
}

/// Gets address from label or absolute values.
///
/// Converts argument to its label's address or its literal value, as the
/// 32-bit word the instruction holds. An undefined label or a malformed or
/// out-of-range literal is reported as an error at the line and returns `None`.
pub(crate) fn convert_argument(argument: &str, msg_list: &mut Diagnostics, line_number: u32, filename: &str, symbols: &SymbolTable) -> Option<u32> {
    let argument_trim = argument.trim();
    let mut error = |text: String| msg_list.error_at(filename, line_number, text);

    if label_name_from_string(argument_trim).is_some() || data_name_from_string(argument_trim).is_some() {
        if let Some(n) = symbols.address(argument_trim) {
            return Some(n);
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
                    return Some(value as u32);
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
                return Some(n as u32);
            }
            error(format!("Decimal value {n} out of bounds"));
        }
        Err(LiteralError::Overflow) => error(format!("Decimal value {argument} out of bounds")),
        Err(_) => {
            // Try resolving as a label name without colon suffix
            if let Some(n) = symbols.address(argument_trim) {
                return Some(n);
            }
            error(format!("Decimal value {argument} incorrect"));
        }
    }
    None
}

/// Build the symbol table from pass 1.
///
/// Every label and data name is defined at its line's program counter.  A
/// name defined again at a different address is an error at the second
/// definition (the first one stands), and text after a label or a data
/// definition that will be ignored is a warning.
pub(crate) fn get_labels(pass1: &[Pass1], msg_list: &mut Diagnostics) -> SymbolTable {
    let mut symbols = SymbolTable::default();
    for line in pass1 {
        let label = label_name_from_string(&line.input_text_line);
        let data = data_name_from_string(&line.input_text_line);
        let Some(name) = label.or(data) else { continue };

        let symbol = Symbol {
            address: line.program_counter,
            file_name: line.file_name.clone(),
            line_counter: line.line_counter,
        };
        if let Err(first) = symbols.insert(name, symbol)
            && first.address != line.program_counter
        {
            msg_list.error_at(
                &line.file_name,
                line.line_counter,
                format!(
                    "Duplicate label {name} found, with differing values, first defined at line {} in file {}",
                    first.line_counter, first.file_name
                ),
            );
        }

        let stripped = strip_comments(&line.input_text_line);
        let mut words = stripped.split_whitespace();
        let first_word = words.next().unwrap_or("");
        let second_word = words.next();
        if label.is_some()
            && let Some(extra) = second_word
        {
            msg_list.warning_at(&line.file_name, line.line_counter, format!("Label {first_word} has extra text {extra}"));
        }
        if data.is_some() {
            let remaining_line = stripped.trim_start_matches(first_word).trim();
            if let Some(extra) = words.next()
                && !second_word.unwrap_or_default().starts_with('\"')
            {
                msg_list.warning_at(&line.file_name, line.line_counter, format!("Data {first_word} has extra text {extra}"));
            }
            if remaining_line.starts_with('\"') && !remaining_line.ends_with('\"') {
                msg_list.warning_at(&line.file_name, line.line_counter, format!("Data {first_word} has no string termination"));
            }
        }
    }
    symbols
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::line::LineType;
    use crate::diag::MessageType;

    #[test]
    // Names are looked up without regard to case or a trailing colon.
    fn test_symbol_table_address() {
        let symbols = SymbolTable::from_pairs(&[("label1", 0), ("label2:", 1), ("#data1", 2)]);
        assert_eq!(symbols.address("label1"), Some(0));
        assert_eq!(symbols.address("LABEL1:"), Some(0));
        assert_eq!(symbols.address("Label2"), Some(1));
        assert_eq!(symbols.address("label2:"), Some(1));
        assert_eq!(symbols.address("#DATA1"), Some(2));
        assert_eq!(symbols.address("label3"), None);
        assert_eq!(symbols.address("data1"), None);
    }

    #[test]
    // A second definition is refused and the first one is kept.
    fn test_symbol_table_insert_duplicate() {
        let mut symbols = SymbolTable::default();
        let at = |address, line_counter| Symbol {
            address,
            file_name: "a.kla".to_owned(),
            line_counter,
        };
        assert_eq!(symbols.insert("loop:", at(8, 3)), Ok(()));
        assert_eq!(symbols.insert("LOOP:", at(16, 9)), Err(&at(8, 3)));
        assert_eq!(symbols.address("loop:"), Some(8));
    }

    /// The pass-1 line `text` at `program_counter`, on `line_counter` of `test`.
    fn pass1_line(text: &str, program_counter: u32, line_counter: u32) -> Pass1 {
        Pass1 {
            program_counter,
            file_name: String::from("test"),
            line_counter,
            input_text_line: text.to_owned(),
            line_type: LineType::Label,
        }
    }

    #[test]
    // Test duplicate label names are identified, with both locations
    fn test_duplicate_label() {
        let pass1 = vec![
            pass1_line("label1:", 0, 1),
            pass1_line("label2:", 1, 2),
            pass1_line("LABEL1:", 2, 3),
            // The same name at the same address is harmless.
            pass1_line("label2:", 1, 4),
        ];
        let mut msg_list = Diagnostics::new();
        let symbols = get_labels(&pass1, &mut msg_list);
        assert_eq!(msg_list.count(MessageType::Error), 1);
        assert_eq!(msg_list.count(MessageType::Warning), 0);
        let msg = &msg_list.messages()[0];
        assert_eq!(
            msg.text(),
            "Duplicate label LABEL1: found, with differing values, first defined at line 1 in file test"
        );
        assert_eq!((msg.line_number(), msg.file_name()), (Some(3), Some("test")));
        assert_eq!(symbols.address("label1:"), Some(0));
    }

    #[test]
    // Test conversion is correct for value arguments
    fn test_convert_argument1() {
        let labels = SymbolTable::from_pairs(&[("label1:", 1), ("label2:", 2), ("#data1", 30)]);
        let mut msg_list = Diagnostics::new();
        assert_eq!(convert_argument("0x1234", &mut msg_list, 3, "test", &labels), Some(0x1234));
        assert_eq!(convert_argument("1234", &mut msg_list, 5, "test", &labels), Some(0x4D2));
        assert_eq!(convert_argument("123456789", &mut msg_list, 6, "test", &labels), Some(0x075B_CD15));
        assert_eq!(convert_argument("label1:", &mut msg_list, 7, "test", &labels), Some(0x1));
        assert_eq!(convert_argument("label1: ", &mut msg_list, 8, "test", &labels), Some(0x1));

        assert_eq!(convert_argument("label2:", &mut msg_list, 14, "test", &labels), Some(0x2));

        assert_eq!(convert_argument("#data1", &mut msg_list, 14, "test", &labels), Some(0x1E));
    }

    #[test]
    // Test for convert_argument if the argument is invalid with correct message
    fn test_convert_argument2() {
        let labels = SymbolTable::from_pairs(&[("label1:", 1), ("label2:", 2), ("#data1", 30)]);
        let mut msg_list = Diagnostics::new();

        // Check for label reference without colon suffix
        assert_eq!(convert_argument("label1", &mut msg_list, 0, "test", &labels), Some(0x1));

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
        let labels = SymbolTable::from_pairs(&[("loop:", 8)]);
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
        let labels = SymbolTable::default();
        let mut msg_list = Diagnostics::new();
        let mut convert = |arg: &str| convert_argument(arg, &mut msg_list, 1, "t", &labels);
        assert_eq!(convert("0x1_0"), Some(0x10));
        assert_eq!(convert("0XFF"), Some(0xFF));
        assert_eq!(convert("-1"), Some(0xFFFF_FFFF));
        assert_eq!(convert("-0x10"), Some(0xFFFF_FFF0));
        assert_eq!(convert("0xFFFFFFFFFFFFFFFF"), Some(0xFFFF_FFFF));
        assert_eq!(convert("-2147483648"), Some(0x8000_0000));
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
        assert_eq!(labels.address("label1:"), Some(0));
        assert_eq!(labels.address("label2:"), Some(4));
        assert_eq!(labels.address("label3:"), Some(6));
        assert_eq!(labels.address("#data321"), Some(7));
        assert_eq!(labels.address("test"), None);
        // "label1: dummy" redefines label1 at another address.
        assert_eq!(msglist.count(MessageType::Error), 1);
        assert_eq!(
            msglist.messages()[0].text(),
            "Duplicate label label1: found, with differing values, first defined at line 0 in file test"
        );
        assert_eq!(msglist.messages()[1].text(), "Label label1: has extra text dummy");
        assert_eq!(msglist.messages()[2].text(), "Data #data123 has extra text dummy2");
        assert_eq!(msglist.messages()[3].text(), "Data #data3 has no string termination");
    }
}
