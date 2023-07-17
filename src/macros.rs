use crate::helper::{return_comments, strip_comments};
use crate::messages::{MessageType, MsgList};
use crate::opcodes::{InputData, Pass0};
use core::fmt::Write as _;
use itertools::Itertools;
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
/// Holds instance of macro from opcode definition file
pub struct Macro {
    /// Name of macro
    pub name: String,
    /// Number of variables
    pub variables: u32,
    /// Items in macro as vector of strings
    pub items: Vec<String>,
    /// Comment from definition
    pub comment: String,
}

#[cfg(not(tarpaulin_include))]
/// Default macro
impl Default for &Macro {
    fn default() -> &'static Macro {
        /// Default macro as blank
        static VALUE: Macro = Macro {
            name: String::new(),
            variables: 0,
            items: Vec::new(),
            comment: String::new(),
        };
        &VALUE
    }
}

/// Parse opcode definition line to macro
///
/// Receive a line from the opcode definition file and if possible parse to instance of Some(Macro), or None
pub fn macro_from_string(input_line_full: &str, msg_list: &mut MsgList) -> Option<Macro> {
    // Find the macro if it exists
    if input_line_full.trim().find('$').unwrap_or(usize::MAX) != 0 {
        return None;
    }
    let mut name = String::new();
    let mut item = String::new();
    let mut items: Vec<String> = Vec::new();
    let mut max_variable: u32 = 0;
    let mut all_found_variables: Vec<i64> = Vec::new();
    let mut all_variables: Vec<i64> = Vec::new();
    let comment = return_comments(&mut input_line_full.clone().to_owned());
    let input_line = &strip_comments(&mut input_line_full.trim().clone().to_owned());

    let words = input_line.split_whitespace();
    for (i, word) in words.enumerate() {
        if i == 0 {
            name = word.to_owned();
        } else if word == "/" {
            items.push(item);
            item = String::new();
        } else {
            if word.contains('%') {
                let without_prefix = word.trim_start_matches('%');
                let int_value = without_prefix.parse::<u32>();
                if int_value.clone().is_err() || int_value.clone().unwrap_or(0) < 1 {
                } else {
                    all_found_variables.push(int_value.clone().unwrap_or(0).into());
                    if int_value.clone().unwrap_or(0) > max_variable {
                        max_variable = int_value.unwrap_or(0);
                    }
                }
            }

            if item.is_empty() {
                item += word;
            } else {
                item = item + " " + word;
            }
        }
    }

    if !item.is_empty() {
        items.push(item);
    }

    if max_variable
        != core::convert::TryInto::<u32>::try_into(
            all_found_variables.clone().into_iter().unique().count(),
        )
        .unwrap_or_default()
    {
        for i in 1..max_variable {
            all_variables.push(i.into());
        }

        // Find the missing variables and create string
        let difference_all_variables: Vec<_> = all_variables
            .into_iter()
            .filter(|variable| !all_found_variables.contains(variable))
            .collect();
        let mut missing = String::new();

        for i in difference_all_variables {
            if !missing.is_empty() {
                missing.push(' ');
            }
            write!(missing, "%{i}").ok();
        }

        msg_list.push(
            format!("Error in macro variable definition for macro {name}, missing {missing:?}",),
            None,
            None,
            MessageType::Warning,
        );
    }

    Some(Macro {
        name,
        variables: max_variable,
        items,
        comment,
    })
}

/// Extracts macro from string
///
/// Checks if end of first word is colon if so return macro name as option string
pub fn macro_name_from_string(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");
    if first_word.starts_with('$') {
        return Some(first_word.to_owned());
    }
    None
}

/// Returns Macro from name
///
/// Return option macro if it exists, or none
pub fn return_macro(line: &str, macros: &mut [Macro]) -> Option<Macro> {
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
    filename: &str,
    msg_list: &mut MsgList,
) -> Option<Vec<String>> {
    let mut words = line.split_whitespace();
    let mut return_items: Vec<String> = Vec::new();
    let mut found = false;

    let input_line_array: Vec<_> = words.clone().collect();

    let first_word = words.next().unwrap_or("");

    for macro_line in macros {
        if macro_line.name == first_word {
            found = true;

            if input_line_array.len()
                > (macro_line.variables + 1_u32)
                    .try_into()
                    .unwrap_or_default()
            {
                msg_list.push(
                    format!("Too many variables for macro {}", macro_line.name),
                    Some(input_line_number),
                    Some(filename.to_owned()),
                    MessageType::Warning,
                );
            }

            for item in &macro_line.items {
                let item_words = item.split_whitespace();
                let mut build_line = String::new();
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
                                Some(filename.to_owned()),
                                MessageType::Error,
                            );
                        } else if int_value.clone().unwrap_or(0)
                            > (input_line_array.len() - 1).try_into().unwrap_or_default()
                        {
                            msg_list.push(
                                format!(
                                    "Missing argument {} for macro {}",
                                    int_value.clone().unwrap_or(0),
                                    macro_line.name
                                ),
                                Some(input_line_number),
                                Some(filename.to_owned()),
                                MessageType::Error,
                            );
                        } else {
                            build_line = build_line
                                + " "
                                + input_line_array
                                    .get(int_value.clone().unwrap_or(0) as usize)
                                    .unwrap_or(&"");
                        }
                    } else {
                        build_line = build_line + " " + item_word;
                    }
                }
                return_items.push(build_line.trim_start().to_owned());
            }
        }
    }
    if found {
        return Some(return_items);
    }
    None
}

/// Multi pass to resolve embedded macros
///
/// Takes Vector of macros, and embeds macros recursively, up to 10 passes
/// Will create errors message for more than 10 passes
#[allow(clippy::too_many_lines)]
#[allow(clippy::module_name_repetitions)]
pub fn expand_embedded_macros(macros: Vec<Macro>, msg_list: &mut MsgList) -> Vec<Macro> {
    let mut pass: u32 = 0;
    let mut changed = true;
    let mut last_macro = String::new();

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
                        item_line_array.push(item_word.to_owned());
                    }
                    #[allow(clippy::unwrap_used)]
                    if (return_macro(&item, &mut input_macros).unwrap().variables as usize)
                        < item_line_array.len() - 1
                    {
                        msg_list.push(
                            format!(
                                "Too many variables in imbedded macro \"{}\" in macro {}",
                                item, input_macro_line.name,
                            ),
                            None,
                            None,
                            MessageType::Warning,
                        );
                    }
                    #[allow(clippy::unwrap_used)]
                    for new_item in return_macro(&item, &mut input_macros).unwrap().items {
                        if new_item.contains('%') {
                            // Replace %n in new items with the nth value in item

                            let new_item_words = new_item.split_whitespace();
                            let mut build_line = String::new();
                            for item_word in new_item_words {
                                if item_word.contains('%') {
                                    let without_prefix = item_word.trim_start_matches('%');
                                    let int_value = without_prefix.parse::<u32>();
                                    if int_value.is_err() || int_value.clone().unwrap_or(0) < 1 {
                                        msg_list.push(
                                            format!(
                                            "Invalid macro argument number {}, in imbedded macro \"{}\" in {}",
                                            without_prefix,
                                            item,
                                            input_macro_line.name,
                                        ),
                                            None,
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
                                            None,
                                            MessageType::Error,
                                        );
                                    } else {
                                        build_line = build_line
                                            + " "
                                            + item_line_array
                                                .get(int_value.clone().unwrap_or(0) as usize)
                                                .unwrap_or(&String::new());
                                    }
                                } else {
                                    build_line = build_line + " " + item_word;
                                }
                            }
                            output_items.push(build_line.strip_prefix(' ').unwrap().to_owned());
                        } else {
                            output_items.push(new_item);
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
                comment: input_macro_line.comment,
            });
        }
        pass += 1;
        input_macros = output_macros.clone();
    }
    if changed {
        msg_list.push(
            format!("Too many macro passes, check {last_macro}"),
            None,
            None,
            MessageType::Error,
        );
    }
    input_macros
}

/// Expands the input lines by expanding all macros
///
/// Takes the input list of all lines and macro vector and expands
#[allow(clippy::module_name_repetitions)]
pub fn expand_macros(
    msg_list: &mut MsgList,
    input_list: Vec<InputData>,
    macro_list: &mut [Macro],
) -> Vec<Pass0> {
    let mut pass0: Vec<Pass0> = Vec::new();
    for code_line in input_list {
        if macro_name_from_string(&code_line.input).is_some() {
            let items = return_macro_items_replace(
                code_line.input.trim(),
                macro_list,
                code_line.line_counter,
                &code_line.file_name,
                msg_list,
            );
            if items.is_some() {
                for item in Option::unwrap(items) {
                    pass0.push(Pass0 {
                        input_text_line: item
                            + " // Macro expansion from "
                            + &macro_name_from_string(&code_line.input).unwrap_or_default(),
                        file_name: code_line.file_name.clone(),
                        line_counter: code_line.line_counter,
                    });
                }
            } else {
                msg_list.push(
                    format!("Macro not found {}", code_line.input),
                    None,
                    None,
                    MessageType::Error,
                );
                pass0.push(Pass0 {
                    file_name: code_line.file_name,
                    input_text_line: code_line.input,
                    line_counter: code_line.line_counter,
                });
            }
        } else {
            pass0.push(Pass0 {
                file_name: code_line.file_name,
                input_text_line: code_line.input,
                line_counter: code_line.line_counter,
            });
        }
    }
    pass0
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::messages::MsgList;

    #[test]
    // Test macro is returns if macro is found
    fn test_macro_name_from_string1() {
        let input = String::from("$TEST");
        let output = macro_name_from_string(&input);
        assert_eq!(output, Some("$TEST".to_owned()));
    }

    #[test]
    // Test for no macro
    fn test_macro_name_from_string2() {
        let input = String::from("TEST");
        let output = macro_name_from_string(&input);
        assert_eq!(output, None);
    }

    #[test]
    // Test for dollar sign in middle of string
    fn test_macro_name_from_string3() {
        let input = String::from("TE$ST");
        let output = macro_name_from_string(&input);
        assert_eq!(output, None);
    }

    #[test]
    fn test_return_macro_value1() {
        let macros = &mut Vec::<Macro>::new();
        macros.push(Macro {
            name: String::from("$TEST"),
            variables: 0,
            items: Vec::new(),
            comment: String::new(),
        });
        let input = String::from("$TEST");
        let output = return_macro(&input, macros);
        assert_eq!(
            output,
            Some(Macro {
                name: String::from("$TEST"),
                variables: 0,
                items: Vec::new(),
                comment: String::new()
            })
        );
    }
    #[test]
    fn test_return_macro_value2() {
        let macros = &mut Vec::<Macro>::new();
        macros.push(Macro {
            name: String::from("$TEST1"),
            variables: 0,
            items: Vec::new(),
            comment: String::new(),
        });
        let input = String::from("$TEST2");
        let output = return_macro(&input, macros);
        assert_eq!(output, None);
    }

    #[test]
    // Test for variable replacement
    fn test_return_macro_items_replace1() {
        let macros = &mut Vec::<Macro>::new();
        let msg_list = &mut MsgList::new();
        macros.push(Macro {
            name: String::from("$DELAY"),
            variables: 2,
            items: vec![
                String::from("DELAYV %1"),
                String::from("DELAYV %2"),
                String::from("PUSH %1"),
            ],
            comment: String::new(),
        });
        let input = String::from("$DELAY ARG_A  ARG_B");
        let output = return_macro_items_replace(&input, macros, 0, "test", msg_list);
        assert_eq!(
            output,
            Some(vec![
                String::from("DELAYV ARG_A"),
                String::from("DELAYV ARG_B"),
                String::from("PUSH ARG_A")
            ])
        );
    }

    #[test]
    // Test for variable replacement macro as variable
    fn test_return_macro_items_replace2() {
        let macros = &mut Vec::<Macro>::new();
        let msg_list = &mut MsgList::new();
        macros.push(Macro {
            name: String::from("$DELAY"),
            variables: 3,
            items: vec![
                String::from("DELAYV %1"),
                String::from("DELAYV %2"),
                String::from("PUSH %3"),
            ],
            comment: String::new(),
        });
        let input = String::from("   $DELAY %MACRO1  ARG_B ARG_C");
        let output = return_macro_items_replace(&input, macros, 0, "test", msg_list);
        assert_eq!(
            output,
            Some(vec![
                String::from("DELAYV %MACRO1"),
                String::from("DELAYV ARG_B"),
                String::from("PUSH ARG_C")
            ])
        );
    }

    #[test]
    // Test if macro does not exist
    fn test_return_macro_items_replace3() {
        let macros = &mut Vec::<Macro>::new();
        let msg_list = &mut MsgList::new();
        macros.push(Macro {
            name: String::from("$DELAY1"),
            variables: 3,
            items: vec![
                String::from("DELAYV %1"),
                String::from("DELAYV %2"),
                String::from("PUSH %3"),
            ],
            comment: String::new(),
        });
        let input = String::from("$DELAY2 %MACRO1  ARG_B ARG_C");
        let output = return_macro_items_replace(&input, macros, 0, "test", msg_list);
        assert_eq!(output, None);
    }

    #[test]
    // Test if too many variables
    fn test_return_macro_items_replace4() {
        let macros = &mut Vec::<Macro>::new();
        let msg_list = &mut MsgList::new();
        macros.push(Macro {
            name: String::from("$DELAY1"),
            variables: 3,
            items: vec![
                String::from("DELAYV %1"),
                String::from("DELAYV %2"),
                String::from("PUSH %3"),
            ],
            comment: String::new(),
        });
        let input = String::from("$DELAY1 %MACRO1  ARG_B ARG_C ARG_D ARG_E");
        let _output = return_macro_items_replace(&input, macros, 0, "test", msg_list);
        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            "Too many variables for macro $DELAY1".to_owned()
        );
    }

    #[test]
    // Test if invalid variable
    fn test_return_macro_items_replace5() {
        let macros = &mut Vec::<Macro>::new();
        let msg_list = &mut MsgList::new();
        macros.push(Macro {
            name: String::from("$DELAY1"),
            variables: 3,
            items: vec![
                String::from("DELAYV %xyz"),
                String::from("DELAYV %2"),
                String::from("PUSH %3"),
            ],
            comment: String::new(),
        });
        let input = String::from("$DELAY1 %MACRO1  ARG_B ARG_C");
        let _output = return_macro_items_replace(&input, macros, 0, "test", msg_list);
        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            "Invalid macro argument number xyz, in macro $DELAY1".to_owned()
        );
    }

    #[test]
    // Test if invalid variable not set
    fn test_return_macro_items_replace6() {
        let macros = &mut Vec::<Macro>::new();
        let msg_list = &mut MsgList::new();
        macros.push(Macro {
            name: String::from("$DELAY1"),
            variables: 3,
            items: vec![
                String::from("DELAYV %1"),
                String::from("DELAYV %2"),
                String::from("PUSH %3"),
            ],
            comment: String::new(),
        });
        let input = String::from("$DELAY1  ARG_A");
        let _output = return_macro_items_replace(&input, macros, 0, "test", msg_list);
        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            "Missing argument 2 for macro $DELAY1".to_owned()
        );
    }

    #[test]
    fn test_expand_embedded_macros1() {
        let macros = &mut Vec::<Macro>::new();

        let msg_list = &mut MsgList::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("OPCODE1 %1"), String::from("OPCODE2 %2")],
            comment: String::new(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("$MACRO1 %2 %1"), String::from("OPCODE3")],
            comment: String::new(),
        });

        let output = expand_embedded_macros(macros.clone(), msg_list);
        let macros_result = &mut Vec::<Macro>::new();
        macros_result.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("OPCODE1 %1"), String::from("OPCODE2 %2")],
            comment: String::new(),
        });
        macros_result.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![
                String::from("OPCODE1 %2"),
                String::from("OPCODE2 %1"),
                String::from("OPCODE3"),
            ],
            comment: String::new(),
        });

        assert_eq!(output, *macros_result);
    }

    #[test]
    fn test_expand_embedded_macros2() {
        let macros = &mut Vec::<Macro>::new();

        let msg_list = &mut MsgList::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("$MACRO1 %2 %1"), String::from("OPCODE2 %2")],
            comment: String::new(),
        });

        let _output = expand_embedded_macros(macros.clone(), msg_list);

        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            "Too many macro passes, check $MACRO1"
        );
    }

    #[test]
    // Test expand macros, to look for too many variables in imbedded macro
    fn test_expand_embedded_macros3() {
        let macros = &mut Vec::<Macro>::new();

        let msg_list = &mut MsgList::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 1,
            items: vec![String::from("OPCODE1 %1"), String::from("OPCODE2 %2")],
            comment: String::new(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 1,
            items: vec![String::from("$MACRO1 %2 %1"), String::from("OPCODE3")],
            comment: String::new(),
        });

        let _output = expand_embedded_macros(macros.clone(), msg_list);
        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            "Too many variables in imbedded macro \"$MACRO1 %2 %1\" in macro $MACRO2"
        );
    }

    #[test]
    // Test expand macros, to look for too invalid variable in imbedded macro
    fn test_expand_embedded_macros4() {
        let macros = &mut Vec::<Macro>::new();

        let msg_list = &mut MsgList::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("OPCODE1 %y"), String::from("OPCODE2 %2")],
            comment: String::new(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("$MACRO1 %2 %1"), String::from("OPCODE3")],
            comment: String::new(),
        });

        let _output = expand_embedded_macros(macros.clone(), msg_list);
        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            "Invalid macro argument number y, in imbedded macro \"$MACRO1 %2 %1\" in $MACRO2"
        );
    }

    #[test]
    // Test expand macros, to look for missing variable in imbedded macro
    fn test_expand_embedded_macros5() {
        let macros = &mut Vec::<Macro>::new();

        let msg_list = &mut MsgList::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("OPCODE1 %1"), String::from("OPCODE2 %2")],
            comment: String::new(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("$MACRO1 %2"), String::from("OPCODE3")],
            comment: String::new(),
        });

        let _output = expand_embedded_macros(macros.clone(), msg_list);
        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            "Missing argument 2 for imbedded macro \"$MACRO1 %2\" in $MACRO2"
        );
    }

    #[test]
    // Test to embed with no variables
    fn test_expand_embedded_macros6() {
        let macros = &mut Vec::<Macro>::new();

        let msg_list = &mut MsgList::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("OPCODE1"), String::from("OPCODE2")],
            comment: String::new(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("$MACRO1"), String::from("OPCODE3")],
            comment: String::new(),
        });

        let output = expand_embedded_macros(macros.clone(), msg_list);
        let macros_result = &mut Vec::<Macro>::new();
        macros_result.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("OPCODE1"), String::from("OPCODE2")],
            comment: String::new(),
        });
        macros_result.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![
                String::from("OPCODE1"),
                String::from("OPCODE2"),
                String::from("OPCODE3"),
            ],
            comment: String::new(),
        });

        assert_eq!(output, *macros_result);
    }

    #[test]
    // Test expand macros, to make sure it expands the macros correctly
    fn test_expand_macros1() {
        use super::*;
        let mut msg_list = MsgList::new();
        let macros = &mut Vec::<Macro>::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("MOV %1"), String::from("RET %2")],
            comment: String::new(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("PUSH %2"), String::from("POP %1")],
            comment: String::new(),
        });
        //  let mut input: Vec<InputData> = Vec::<InputData>::new();

        let input: Vec<InputData> = vec![
            InputData {
                input: String::from("$MACRO1 A B"),
                file_name: "File1".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: String::from("$MACRO2 C D"),
                file_name: "File2".to_owned(),
                line_counter: 2,
            },
        ];
        let pass0 = expand_macros(&mut msg_list, input, macros);
        assert_eq!(
            strip_comments(&mut pass0.get(0).unwrap_or_default().input_text_line.clone()),
            "MOV A"
        );
        assert_eq!(
            strip_comments(&mut pass0.get(0).unwrap_or_default().file_name.clone()),
            "File1"
        );
        assert_eq!(
            strip_comments(&mut pass0.get(1).unwrap_or_default().input_text_line.clone()),
            "RET B"
        );
        assert_eq!(
            strip_comments(&mut pass0.get(2).unwrap_or_default().input_text_line.clone()),
            "PUSH D"
        );
        assert_eq!(
            strip_comments(&mut pass0.get(3).unwrap_or_default().input_text_line.clone()),
            "POP C"
        );
        assert_eq!(
            strip_comments(&mut pass0.get(3).unwrap_or_default().file_name.clone()),
            "File2"
        );
        //  assert_eq!(&mut pass0[3].line_counter, 1);
    }

    #[test]
    // Test expand macros, with too few variables, gives error
    fn test_expand_macros2() {
        use super::*;
        let mut msg_list = MsgList::new();
        let macros = &mut Vec::<Macro>::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("MOV %1"), String::from("RET %2")],
            comment: String::new(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("PUSH %2"), String::from("POP %1")],
            comment: String::new(),
        });
        // let input = vec![String::from("$MACRO1 A B"), String::from("$MACRO2 C")];
        let input: Vec<InputData> = vec![
            InputData {
                input: String::from("$MACRO1 A B"),
                file_name: "File1".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: String::from("$MACRO2 C"),
                file_name: "File1".to_owned(),
                line_counter: 2,
            },
        ];

        let pass0 = expand_macros(&mut msg_list, input, macros);
        assert_eq!(
            strip_comments(&mut pass0.get(0).unwrap_or_default().input_text_line.clone()),
            "MOV A"
        );
        assert_eq!(
            strip_comments(&mut pass0.get(1).unwrap_or_default().input_text_line.clone()),
            "RET B"
        );
        assert_eq!(
            strip_comments(&mut pass0.get(2).unwrap_or_default().input_text_line.clone()),
            "PUSH"
        );
        assert_eq!(
            strip_comments(&mut pass0.get(3).unwrap_or_default().input_text_line.clone()),
            "POP C"
        );
        assert_eq!(msg_list.number_errors(), 1);
        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            "Missing argument 2 for macro $MACRO2"
        );
    }

    #[test]
    // Test expand macros, passing too many variables, gives warning
    fn test_expand_macros3() {
        use super::*;
        let mut msg_list = MsgList::new();
        let macros = &mut Vec::<Macro>::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("MOV %1"), String::from("RET %2")],
            comment: String::new(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 1,
            items: vec![String::from("PUSH %2")],
            comment: String::new(),
        });
        // let input = vec![String::from("$MACRO1 A B"), String::from("$MACRO2 C D")];
        let input: Vec<InputData> = vec![
            InputData {
                input: String::from("$MACRO1 A B"),
                file_name: "File1".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: String::from("$MACRO2 C D"),
                file_name: "File1".to_owned(),
                line_counter: 2,
            },
        ];

        let pass0 = expand_macros(&mut msg_list, input, macros);
        assert_eq!(
            strip_comments(&mut pass0.get(0).unwrap_or_default().input_text_line.clone()),
            "MOV A"
        );
        assert_eq!(
            strip_comments(&mut pass0.get(1).unwrap_or_default().input_text_line.clone()),
            "RET B"
        );
        assert_eq!(
            strip_comments(&mut pass0.get(2).unwrap_or_default().input_text_line.clone()),
            "PUSH D"
        );
        assert_eq!(msg_list.number_warnings(), 1);
        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            "Too many variables for macro $MACRO2"
        );
    }

    #[test]
    // Test expand macros, missing macro, gives error
    fn test_expand_macros4() {
        use super::*;
        let mut msg_list = MsgList::new();
        let macros = &mut Vec::<Macro>::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("MOV %1"), String::from("RET %2")],
            comment: String::new(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 1,
            items: vec![String::from("PUSH %2")],
            comment: String::new(),
        });
        // let input = vec![String::from("$MACRO7 A B"), String::from("$MACRO2 C D")];
        let input: Vec<InputData> = vec![
            InputData {
                input: String::from("$MACRO7 A B"),
                file_name: "File1".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: String::from("$MACRO2 C D"),
                file_name: "File1".to_owned(),
                line_counter: 1,
            },
        ];

        let _pass0 = expand_macros(&mut msg_list, input, macros);
        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            "Macro not found $MACRO7 A B"
        );
    }

    #[test]
    // Test expand macros if no macro
    fn test_expand_macros5() {
        use super::*;
        let mut msg_list = MsgList::new();
        let macros = &mut Vec::<Macro>::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("MOV %1"), String::from("RET %2")],
            comment: String::new(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 1,
            items: vec![String::from("PUSH %2")],
            comment: String::new(),
        });

        let input: Vec<InputData> = vec![InputData {
            input: String::from("OPCODE1 A B"),
            file_name: "File1".to_owned(),
            line_counter: 1,
        }];

        let pass0 = expand_macros(&mut msg_list, input, macros);
        assert_eq!(
            strip_comments(&mut pass0.get(0).unwrap_or_default().input_text_line.clone()),
            "OPCODE1 A B"
        );
    }

    #[test]
    // Convert string to macro with no variables
    fn test_macro_from_string1() {
        let mut msglist = MsgList::new();
        let input_line = String::from("$POPALL POP A / POP B");
        let macro_result = macro_from_string(&input_line, &mut msglist);
        assert_eq!(
            macro_result,
            Some(Macro {
                name: String::from("$POPALL"),
                variables: 0,
                items: vec!["POP A".to_owned(), "POP B".to_owned()],
                comment: String::new()
            })
        );
    }

    #[test]
    // Convert string to macro with variables
    fn test_macro_from_string2() {
        let mut msglist = MsgList::new();
        let input_line = String::from("$POPALL POP %1 / POP %2");
        let macro_result = macro_from_string(&input_line, &mut msglist);
        assert_eq!(
            macro_result,
            Some(Macro {
                name: String::from("$POPALL"),
                variables: 2,
                items: vec!["POP %1".to_owned(), "POP %2".to_owned()],
                comment: String::new()
            })
        );
    }

    #[test]
    // Convert string to macro with comments and spaces before name
    fn test_macro_from_string3() {
        let mut msglist = MsgList::new();
        let input_line = String::from("   $POPALL POP %1 / POP %2 //Test Macro");
        let macro_result = macro_from_string(&input_line, &mut msglist);
        assert_eq!(
            macro_result,
            Some(Macro {
                name: String::from("$POPALL"),
                variables: 2,
                items: vec!["POP %1".to_owned(), "POP %2".to_owned()],
                comment: String::from("Test Macro")
            })
        );
    }

    #[test]
    // Convert string to macro with missing variables
    fn test_macro_from_string4() {
        let mut msglist = MsgList::new();
        let input_line = String::from("$POPALL POP %3 / POP %2");
        let macro_result = macro_from_string(&input_line, &mut msglist);
        assert_eq!(
            macro_result,
            Some(Macro {
                name: String::from("$POPALL"),
                variables: 3,
                items: vec!["POP %3".to_owned(), "POP %2".to_owned()],
                comment: String::new()
            })
        );
        assert_eq!(
            msglist.list.get(0).unwrap_or_default().text,
            "Error in macro variable definition for macro $POPALL, missing \"%1\""
        );
    }

    #[test]
    // Convert string to macro with variables not a macro
    fn test_macro_from_string5() {
        let mut msglist = MsgList::new();
        let input_line = String::from("POPALL POP %1 / POP %2");
        let macro_result = macro_from_string(&input_line, &mut msglist);
        assert_eq!(macro_result, None);
    }

    #[test]
    // Convert string to macro with variables not a macro with dollar in middle
    fn test_macro_from_string6() {
        let mut msglist = MsgList::new();
        let input_line = String::from("POP$ALL POP %1 / POP %2");
        let macro_result = macro_from_string(&input_line, &mut msglist);
        assert_eq!(macro_result, None);
    }

    #[test]
    // Convert string to macro with missing variables
    fn test_macro_from_string7() {
        let mut msglist = MsgList::new();
        let input_line = String::from("$POPALL POP %5 / POP %3");
        let _macro_result = macro_from_string(&input_line, &mut msglist);
        //assert_eq!(macro_result, None);
        assert_eq!(
            msglist.list.get(0).unwrap_or_default().text,
            "Error in macro variable definition for macro $POPALL, missing \"%1 %2 %4\"",
        );
    }
}
