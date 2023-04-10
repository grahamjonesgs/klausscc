use crate::{messages::{MessageType, MsgList}, opcodes::Pass0};

#[derive(Clone,PartialEq,Debug)]
pub struct Macro {
    pub name: String,
    pub variables: u32,
    pub items: Vec<String>,
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
                            output_items.push(build_line.strip_prefix(' ').unwrap().to_string());
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

/// Expands the input lines by expanding all macros
///
/// Takes the input list of all lines and macro vector and expands
#[allow(clippy::module_name_repetitions)]
pub fn expand_macros(
    msg_list: &mut MsgList,
    input_list: Vec<String>,
    macro_list: &mut [Macro],
) -> Vec<Pass0> {
    let mut pass0: Vec<Pass0> = Vec::new();
    let mut input_line_count: u32 = 1;
    for code_line in input_list {
        if macro_name_from_string(&code_line).is_some() {
            let items = return_macro_items_replace(
                code_line.trim(),
                macro_list,
                input_line_count,
                msg_list,
            );
            if items.is_some() {
                for item in Option::unwrap(items) {
                    pass0.push(Pass0 {
                        input: item + " // Macro expansion from "
                             + &{
                                let this = macro_name_from_string(&code_line);
                                let default = String::new();
                                match this {
                                    Some(x) => x,
                                    None => default,
                                }
                            }
                            .to_string(),
                        line_counter: input_line_count,
                    });
                }
            } else {
                msg_list.push(
                    format!("Macro not found {code_line}"),
                    None,
                    MessageType::Error,
                );
                pass0.push(Pass0 {
                    input: code_line,
                    line_counter: input_line_count,
                });
            }
        } else {
            pass0.push(Pass0 {
                input: code_line,
                line_counter: input_line_count,
            });
        }
        input_line_count += 1;
    }
    pass0
}

#[cfg(test)]
mod tests {
    
    #[allow(unused_imports)]
    use crate::helper::strip_comments;
    use crate::{macros::{macro_name_from_string, return_macro, Macro, return_macro_items_replace, expand_macros_multi}, messages::MsgList};

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
        let macros_result = &mut Vec::<Macro>::new();
        macros_result.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("OPCODE1 %1"), String::from("OPCODE2 %2")],
        });
        macros_result.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("OPCODE1 %2"), String::from("OPCODE2 %1"),String::from("OPCODE3")],
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
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("PUSH %2"), String::from("POP %1")],
        });
        let input = vec![String::from("$MACRO1 A B"),String::from("$MACRO2 C D")];
        let mut pass0= expand_macros(&mut msg_list,input , macros);
        assert_eq!(strip_comments(&mut pass0[0].input), "MOV A");
        assert_eq!(strip_comments(&mut pass0[1].input), "RET B");
        assert_eq!(strip_comments(&mut pass0[2].input), "PUSH D");
        assert_eq!(strip_comments(&mut pass0[3].input), "POP C");
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
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("PUSH %2"), String::from("POP %1")],
        });
        let input = vec![String::from("$MACRO1 A B"),String::from("$MACRO2 C")];
        let mut pass0= expand_macros(&mut msg_list,input , macros);
        assert_eq!(strip_comments(&mut pass0[0].input), "MOV A");
        assert_eq!(strip_comments(&mut pass0[1].input), "RET B");
        assert_eq!(strip_comments(&mut pass0[2].input), "PUSH");
        assert_eq!(strip_comments(&mut pass0[3].input), "POP C");
        assert_eq!(msg_list.number_errors(), 1);
        assert_eq!(msg_list.list[0].name, "Missing argument 2 for macro $MACRO2");
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
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 1,
            items: vec![String::from("PUSH %2")],
        });
        let input = vec![String::from("$MACRO1 A B"),String::from("$MACRO2 C D")];
        let mut pass0= expand_macros(&mut msg_list,input , macros);
        assert_eq!(strip_comments(&mut pass0[0].input), "MOV A");
        assert_eq!(strip_comments(&mut pass0[1].input), "RET B");
        assert_eq!(strip_comments(&mut pass0[2].input), "PUSH D");
        assert_eq!(msg_list.number_warnings(), 1);
        assert_eq!(msg_list.list[0].name, "Too many variables for macro $MACRO2");
    }
}