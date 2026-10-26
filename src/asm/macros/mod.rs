//! Assembler macros: `$NAME item / item` definitions, from the built-in
//! table or the source's `!macro` directives, and their expansion into
//! source lines.

mod builtin;

use crate::asm::lexer::{return_comments, strip_comments};
use crate::asm::line::SourceLine;
use crate::diag::Diagnostics;
pub(crate) use builtin::v2_opcodes_and_macros;
use itertools::Itertools as _;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::Path;

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
/// Holds instance of macro from opcode definition file.
pub(crate) struct Macro {
    /// Comment from definition.
    pub comment: String,
    /// Items in macro as vector of strings.
    pub items: Vec<String>,
    /// Name of macro.
    pub name: String,
    /// Number of variables.
    pub variables: u32,
}

/// A `%n` placeholder in a macro item that could not be substituted.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SubstError {
    /// The placeholder is not `%` followed by a number of at least 1.
    BadIndex(String),
    /// The call has no argument `n`.
    Missing(usize),
}

/// Replace each `%n` word in `item` with argument `n` of `call`.
///
/// `call` is the macro call split into words, so `call[0]` is the macro name
/// and `call[n]` is argument `n`. Words that cannot be substituted are left
/// out of the result and returned as errors, so the caller can report every
/// one of them.
fn substitute_args(item: &str, call: &[&str]) -> (String, Vec<SubstError>) {
    let mut errors = Vec::new();
    let mut words: Vec<&str> = Vec::new();
    for word in item.split_whitespace() {
        if !word.contains('%') {
            words.push(word);
            continue;
        }
        let number = word.trim_start_matches('%');
        match number.parse::<usize>() {
            Ok(index @ 1..) => match call.get(index) {
                Some(argument) => words.push(argument),
                None => errors.push(SubstError::Missing(index)),
            },
            _ => errors.push(SubstError::BadIndex(number.to_owned())),
        }
    }
    (words.join(" "), errors)
}

/// Multi pass to resolve embedded macros.
///
/// Takes Vector of macros, and embeds macros recursively, up to 10 passes.
/// Will create errors message for more than 10 passes.
pub(crate) fn expand_embedded_macros(macros: Vec<Macro>, msg_list: &mut Diagnostics) -> Vec<Macro> {
    let mut pass: u32 = 0;
    let mut changed = true;
    let mut last_macro = String::default();

    let mut input_macros = macros;

    while pass < 10 && changed {
        changed = false;
        let mut output_macros: Vec<Macro> = Vec::new();
        for input_macro_line in input_macros.clone() {
            let mut output_items: Vec<String> = Vec::new();
            for item in input_macro_line.items {
                let Some(embedded) = return_macro(&item, &input_macros) else {
                    output_items.push(item);
                    continue;
                };
                let call: Vec<&str> = item.split_whitespace().collect();
                if usize::try_from(embedded.variables).is_ok_and(|variables| variables < call.len() - 1) {
                    msg_list.warning(format!(
                        "Too many variables in embedded macro \"{}\" in macro {}",
                        item, input_macro_line.name
                    ));
                }
                for new_item in embedded.items {
                    if !new_item.contains('%') {
                        output_items.push(new_item);
                        continue;
                    }
                    // Replace %n in new items with the nth value in item
                    let (substituted, errors) = substitute_args(&new_item, &call);
                    for error in errors {
                        let text = match error {
                            SubstError::BadIndex(number) => {
                                format!(
                                    "Invalid macro argument number {number}, in embedded macro \"{item}\" in {}",
                                    input_macro_line.name
                                )
                            }
                            SubstError::Missing(index) => {
                                format!("Missing argument {index} for embedded macro \"{item}\" in {}", input_macro_line.name)
                            }
                        };
                        msg_list.error(text);
                    }
                    output_items.push(substituted);
                }
                last_macro.clone_from(&input_macro_line.name);
                changed = true;
            }
            output_macros.push(Macro {
                name: input_macro_line.name,
                variables: input_macro_line.variables,
                items: output_items,
                comment: input_macro_line.comment,
            });
        }

        pass += 1;
        input_macros.clone_from(&output_macros);
    }
    if changed {
        msg_list.error(format!("Too many macro passes, check {last_macro}"));
    }
    input_macros
}

/// Expands the input lines by expanding all macros.
///
/// Takes the input list of all lines and macro vector and expands. A trailing
/// `//` comment on a macro call is not part of its arguments.
pub(crate) fn expand_macros(msg_list: &mut Diagnostics, input_list: Vec<SourceLine>, macro_list: &mut [Macro]) -> Vec<SourceLine> {
    let mut pass0: Vec<SourceLine> = Vec::new();
    for code_line in input_list {
        if let Some(macro_name) = macro_name_from_string(&code_line.input_text_line) {
            let call = strip_comments(&code_line.input_text_line);
            let items = return_macro_items_replace(call, macro_list, code_line.line_counter, &code_line.file_name, msg_list);
            if let Some(items_vec) = items {
                for item in items_vec {
                    pass0.push(SourceLine {
                        input_text_line: format!("{item} // Macro expansion from {macro_name}"),
                        file_name: code_line.file_name.clone(),
                        line_counter: code_line.line_counter,
                    });
                }
            } else {
                msg_list.error_at(&code_line.file_name, code_line.line_counter, format!("Macro not found {call}"));
                pass0.push(code_line);
            }
        } else {
            pass0.push(code_line);
        }
    }
    pass0
}

/// Merge user macro definitions declared in the source into `macro_list`.
///
/// Scans `input_list` for the `!macro` (inline, single definition) and
/// `!macros <file>` (external file of definitions) directives, adds each
/// definition to `macro_list`, and returns the input with the directive lines
/// removed. Redefining a macro that already exists (built-in or user) is an
/// error. When any macro is added the embedded references are re-resolved so a
/// user macro can build on the built-ins or on another user macro.
///
/// `!macros` files are resolved relative to the directory of the file that
/// contains the directive, matching `!include`.
pub(crate) fn apply_source_macros(input_list: Vec<SourceLine>, macro_list: &mut Vec<Macro>, msg_list: &mut Diagnostics) -> Vec<SourceLine> {
    let mut output: Vec<SourceLine> = Vec::new();
    let mut added = false;

    for line in input_list {
        let stripped = strip_comments(&line.input_text_line);
        let first_word = stripped.split_whitespace().next().unwrap_or("");
        match first_word {
            "!macro" => {
                // Inline definition: the remainder is "$NAME item / item / …".
                let definition = stripped.trim().strip_prefix("!macro").unwrap_or("").trim();
                added |= add_macro_definition(definition, macro_list, line.line_counter, &line.file_name, msg_list);
            }
            "!macros" => {
                // External file of one macro definition per line.
                let file_arg = stripped.split_whitespace().nth(1).unwrap_or("");
                if file_arg.is_empty() {
                    msg_list.error_at(
                        &line.file_name,
                        line.line_counter,
                        format!("Missing file name for !macros in {}", line.file_name),
                    );
                    continue;
                }
                let parent = Path::new(&line.file_name).parent().unwrap_or_else(|| Path::new(""));
                let macros_path = parent.join(file_arg).to_string_lossy().into_owned();
                match std::fs::read_to_string(&macros_path) {
                    Ok(contents) => {
                        for (index, definition) in contents.lines().enumerate() {
                            let file_line = u32::try_from(index + 1).unwrap_or(0);
                            added |= add_macro_definition(definition, macro_list, file_line, &macros_path, msg_list);
                        }
                    }
                    Err(err) => msg_list.error_at(
                        &line.file_name,
                        line.line_counter,
                        format!("Unable to open macros file {macros_path} in {}: {err}", line.file_name),
                    ),
                }
            }
            _ => output.push(line),
        }
    }

    if added {
        let expanded = expand_embedded_macros(macro_list.clone(), msg_list);
        *macro_list = expanded;
    }
    output
}

/// Parse one macro-definition line and add it to `macro_list`.
///
/// Blank or comment lines (anything [`macro_from_string`] rejects) are skipped.
/// A definition whose name already exists is an error and is not added. Returns
/// true only when a macro was actually appended.
fn add_macro_definition(definition: &str, macro_list: &mut Vec<Macro>, line_number: u32, filename: &str, msg_list: &mut Diagnostics) -> bool {
    let Some(new_macro) = macro_from_string(definition, msg_list) else {
        return false;
    };
    if return_macro(&new_macro.name, macro_list).is_some() {
        msg_list.error_at(filename, line_number, format!("Duplicate macro definition {} found", new_macro.name));
        return false;
    }
    macro_list.push(new_macro);
    true
}

/// Parse opcode definition line to macro.
///
/// Receive a line from the opcode definition file and if possible parse to instance of Some(Macro), or None.
pub(crate) fn macro_from_string(input_line_full: &str, msg_list: &mut Diagnostics) -> Option<Macro> {
    // Find the macro if it exists
    if input_line_full.trim().find('$').unwrap_or(usize::MAX) != 0 {
        return None;
    }
    let mut name = String::default();
    let mut item = String::default();
    let mut items: Vec<String> = Vec::new();
    let mut max_variable: u32 = 0;
    let mut all_found_variables: Vec<i64> = Vec::new();
    let mut all_variables: Vec<i64> = Vec::new();
    let comment = return_comments(input_line_full).to_owned();
    let input_line = strip_comments(input_line_full);

    let words = input_line.split_whitespace();
    for (i, word) in words.enumerate() {
        if i == 0 {
            word.clone_into(&mut name);
        } else if word == "/" {
            items.push(item);
            item = String::default();
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

            if !item.is_empty() {
                item.push(' ');
            }
            item.push_str(word);
        }
    }

    if !item.is_empty() {
        items.push(item);
    }

    if max_variable != TryInto::<u32>::try_into(all_found_variables.clone().into_iter().unique().count()).unwrap_or_default() {
        for i in 1..max_variable {
            all_variables.push(i.into());
        }

        // Find the missing variables and create string
        let difference_all_variables: Vec<_> = all_variables
            .into_iter()
            .filter(|variable| !all_found_variables.contains(variable))
            .collect();
        let mut missing = String::default();
        for i in difference_all_variables {
            if !missing.is_empty() {
                missing.push(' ');
            }
            write!(missing, "%{i}").ok();
        }

        msg_list.warning(format!("Error in macro variable definition for macro {name}, missing {missing:?}"));
    }

    Some(Macro {
        name,
        variables: max_variable,
        items,
        comment,
    })
}

/// Extracts macro from string.
///
/// Checks if end of first word is colon if so return macro name as option string.
fn macro_name_from_string(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");
    if first_word.starts_with('$') {
        return Some(first_word.to_owned());
    }
    None
}

/// Returns Macro from name.
///
/// Return option macro if it exists, or none.
pub(crate) fn return_macro(line: &str, macros: &[Macro]) -> Option<Macro> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");

    for macro_line in macros {
        if macro_line.name == first_word {
            return Some(macro_line.clone());
        }
    }
    None
}

/// Update variables in a macro.
///
/// Return option all vec string replacing %x with correct value.
fn return_macro_items_replace(
    line: &str,
    macros: &mut [Macro],
    input_line_number: u32,
    filename: &str,
    msg_list: &mut Diagnostics,
) -> Option<Vec<String>> {
    let mut words = line.split_whitespace();
    let mut return_items: Vec<String> = Vec::new();
    let mut found = false;

    let input_line_array: Vec<_> = words.clone().collect();

    let first_word = words.next().unwrap_or("");

    for macro_line in macros {
        if macro_line.name == first_word {
            found = true;

            if input_line_array.len() > (macro_line.variables + 1_u32).try_into().unwrap_or_default() {
                msg_list.warning_at(filename, input_line_number, format!("Too many variables for macro {}", macro_line.name));
            }

            for item in &macro_line.items {
                let (substituted, errors) = substitute_args(item, &input_line_array);
                for error in errors {
                    let text = match error {
                        SubstError::BadIndex(number) => format!("Invalid macro argument number {number}, in macro {}", macro_line.name),
                        SubstError::Missing(index) => format!("Missing argument {index} for macro {}", macro_line.name),
                    };
                    msg_list.error_at(filename, input_line_number, text);
                }
                return_items.push(substituted);
            }
        }
    }
    if found {
        return Some(return_items);
    }
    None
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::diag::MessageType;

    /// One source line of `test.kla`.
    fn source_line(input: &str, line_counter: u32) -> SourceLine {
        SourceLine {
            input_text_line: input.to_owned(),
            file_name: "test.kla".to_owned(),
            line_counter,
        }
    }

    #[test]
    // %n words are replaced by the matching call argument; bad ones are reported.
    fn test_substitute_args() {
        let call = ["$M", "A", "0x10"];
        assert_eq!(substitute_args("SETR %1 %2", &call), ("SETR A 0x10".to_owned(), vec![]));
        assert_eq!(substitute_args("NOP", &call), ("NOP".to_owned(), vec![]));
        assert_eq!(substitute_args("COPY %2  %1", &call), ("COPY 0x10 A".to_owned(), vec![]));
        assert_eq!(substitute_args("PUSH %3", &call), ("PUSH".to_owned(), vec![SubstError::Missing(3)]));
        assert_eq!(
            substitute_args("PUSH %0", &call),
            ("PUSH".to_owned(), vec![SubstError::BadIndex("0".to_owned())])
        );
        assert_eq!(
            substitute_args("PUSH %x", &call),
            ("PUSH".to_owned(), vec![SubstError::BadIndex("x".to_owned())])
        );
        // Every word dropped: no panic on the empty result.
        assert_eq!(substitute_args("%1", &["$A"]), (String::new(), vec![SubstError::Missing(1)]));
    }

    #[test]
    // Regression: "!macro $A %1" then "!macro $B $A" panicked (strip_prefix(' ').unwrap()
    // on an empty expansion). It is now an error.
    fn test_embedded_macro_missing_argument_does_not_panic() {
        let mut msg_list = Diagnostics::new();
        let mut macro_list = Vec::new();
        let input = vec![source_line("!macro $A %1", 1), source_line("!macro $B $A", 2)];
        let output = apply_source_macros(input, &mut macro_list, &mut msg_list);
        assert_eq!(output, vec![]);
        assert_eq!(msg_list.count(MessageType::Error), 1);
        assert_eq!(
            msg_list.messages().first().unwrap().text(),
            "Missing argument 1 for embedded macro \"$A\" in $B"
        );
        // The embedded call can still pass its own argument through.
        let mut msg_list = Diagnostics::new();
        let mut macro_list = Vec::new();
        let input = vec![source_line("!macro $A PUSH %1", 1), source_line("!macro $B $A %1", 2)];
        apply_source_macros(input, &mut macro_list, &mut msg_list);
        assert_eq!(msg_list.messages().len(), 0);
        assert_eq!(return_macro("$B", &macro_list).unwrap().items, vec!["PUSH %1".to_owned()]);
    }

    #[test]
    // Regression: a trailing comment on a macro call became extra arguments
    // ("Too many variables" warning, and "//" substituted for %2).
    fn test_macro_call_trailing_comment_is_not_arguments() {
        let mut msg_list = Diagnostics::new();
        let macros = &mut [Macro {
            name: String::from("$WAIT"),
            variables: 1,
            items: vec![String::from("DELAYV %1")],
            comment: String::default(),
        }];
        let pass0 = expand_macros(&mut msg_list, vec![source_line("$WAIT 5 // x", 3)], macros);
        assert_eq!(
            msg_list.messages().len(),
            0,
            "{:?}",
            msg_list.messages().first().map(crate::diag::Message::text)
        );
        assert_eq!(pass0.first().unwrap().input_text_line, "DELAYV 5 // Macro expansion from $WAIT");

        // A comment cannot supply a missing argument either.
        let macros = &mut [Macro {
            name: String::from("$TWO"),
            variables: 2,
            items: vec![String::from("COPY %1 %2")],
            comment: String::default(),
        }];
        let pass0 = expand_macros(&mut msg_list, vec![source_line("$TWO A // B", 4)], macros);
        assert_eq!(msg_list.messages().first().unwrap().text(), "Missing argument 2 for macro $TWO");
        assert_eq!(pass0.first().unwrap().input_text_line, "COPY A // Macro expansion from $TWO");
    }

    #[test]
    // "Macro not found" carries the line and file of the call.
    fn test_macro_not_found_has_location() {
        let mut msg_list = Diagnostics::new();
        expand_macros(&mut msg_list, vec![source_line("$NOPE 1 // why", 7)], &mut []);
        let msg = msg_list.messages().first().unwrap();
        assert_eq!(msg.level(), MessageType::Error);
        assert_eq!(msg.text(), "Macro not found $NOPE 1");
        assert_eq!((msg.line_number(), msg.file_name()), (Some(7), Some("test.kla")));
    }

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
            comment: String::default(),
        });
        let input = String::from("$TEST");
        let output = return_macro(&input, macros);
        assert_eq!(
            output,
            Some(Macro {
                name: String::from("$TEST"),
                variables: 0,
                items: Vec::new(),
                comment: String::default()
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
            comment: String::default(),
        });
        let input = String::from("$TEST2");
        let output = return_macro(&input, macros);
        assert_eq!(output, None);
    }

    #[test]
    // Test for variable replacement
    fn test_return_macro_items_replace1() {
        let macros = &mut Vec::<Macro>::new();
        let msg_list = &mut Diagnostics::new();
        macros.push(Macro {
            name: String::from("$DELAY"),
            variables: 2,
            items: vec![String::from("DELAYV %1"), String::from("DELAYV %2"), String::from("PUSH %1")],
            comment: String::default(),
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
        let msg_list = &mut Diagnostics::new();
        macros.push(Macro {
            name: String::from("$DELAY"),
            variables: 3,
            items: vec![String::from("DELAYV %1"), String::from("DELAYV %2"), String::from("PUSH %3")],
            comment: String::default(),
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
        let msg_list = &mut Diagnostics::new();
        macros.push(Macro {
            name: String::from("$DELAY1"),
            variables: 3,
            items: vec![String::from("DELAYV %1"), String::from("DELAYV %2"), String::from("PUSH %3")],
            comment: String::default(),
        });
        let input = String::from("$DELAY2 %MACRO1  ARG_B ARG_C");
        let output = return_macro_items_replace(&input, macros, 0, "test", msg_list);
        assert_eq!(output, None);
    }

    #[test]
    // Test if too many variables
    fn test_return_macro_items_replace4() {
        let macros = &mut Vec::<Macro>::new();
        let msg_list = &mut Diagnostics::new();
        macros.push(Macro {
            name: String::from("$DELAY1"),
            variables: 3,
            items: vec![String::from("DELAYV %1"), String::from("DELAYV %2"), String::from("PUSH %3")],
            comment: String::default(),
        });
        let input = String::from("$DELAY1 %MACRO1  ARG_B ARG_C ARG_D ARG_E");
        let _output = return_macro_items_replace(&input, macros, 0, "test", msg_list);
        assert_eq!(msg_list.messages()[0].text(), "Too many variables for macro $DELAY1".to_owned());
    }

    #[test]
    // Test if invalid variable
    fn test_return_macro_items_replace5() {
        let macros = &mut Vec::<Macro>::new();
        let msg_list = &mut Diagnostics::new();
        macros.push(Macro {
            name: String::from("$DELAY1"),
            variables: 3,
            items: vec![String::from("DELAYV %xyz"), String::from("DELAYV %2"), String::from("PUSH %3")],
            comment: String::default(),
        });
        let input = String::from("$DELAY1 %MACRO1  ARG_B ARG_C");
        let _output = return_macro_items_replace(&input, macros, 0, "test", msg_list);
        assert_eq!(
            msg_list.messages()[0].text(),
            "Invalid macro argument number xyz, in macro $DELAY1".to_owned()
        );
    }

    #[test]
    // Test if invalid variable not set
    fn test_return_macro_items_replace6() {
        let macros = &mut Vec::<Macro>::new();
        let msg_list = &mut Diagnostics::new();
        macros.push(Macro {
            name: String::from("$DELAY1"),
            variables: 3,
            items: vec![String::from("DELAYV %1"), String::from("DELAYV %2"), String::from("PUSH %3")],
            comment: String::default(),
        });
        let input = String::from("$DELAY1  ARG_A");
        let _output = return_macro_items_replace(&input, macros, 0, "test", msg_list);
        assert_eq!(msg_list.messages()[0].text(), "Missing argument 2 for macro $DELAY1".to_owned());
    }

    #[test]
    fn test_expand_embedded_macros1() {
        let macros = &mut Vec::<Macro>::new();

        let msg_list = &mut Diagnostics::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("OPCODE1 %1"), String::from("OPCODE2 %2")],
            comment: String::default(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("$MACRO1 %2 %1"), String::from("OPCODE3")],
            comment: String::default(),
        });

        let output = expand_embedded_macros(macros.clone(), msg_list);
        let macros_result = &mut Vec::<Macro>::new();
        macros_result.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("OPCODE1 %1"), String::from("OPCODE2 %2")],
            comment: String::default(),
        });
        macros_result.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("OPCODE1 %2"), String::from("OPCODE2 %1"), String::from("OPCODE3")],
            comment: String::default(),
        });

        assert_eq!(output, *macros_result);
    }

    #[test]
    fn test_expand_embedded_macros2() {
        let macros = &mut Vec::<Macro>::new();

        let msg_list = &mut Diagnostics::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("$MACRO1 %2 %1"), String::from("OPCODE2 %2")],
            comment: String::default(),
        });

        let _output = expand_embedded_macros(macros.clone(), msg_list);

        assert_eq!(msg_list.messages()[0].text(), "Too many macro passes, check $MACRO1");
    }

    #[test]
    // Test expand macros, to look for too many variables in embedded macro
    fn test_expand_embedded_macros3() {
        let macros = &mut Vec::<Macro>::new();

        let msg_list = &mut Diagnostics::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 1,
            items: vec![String::from("OPCODE1 %1"), String::from("OPCODE2 %2")],
            comment: String::default(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 1,
            items: vec![String::from("$MACRO1 %2 %1"), String::from("OPCODE3")],
            comment: String::default(),
        });

        let _output = expand_embedded_macros(macros.clone(), msg_list);
        assert_eq!(
            msg_list.messages()[0].text(),
            "Too many variables in embedded macro \"$MACRO1 %2 %1\" in macro $MACRO2"
        );
    }

    #[test]
    // Test expand macros, to look for too invalid variable in embedded macro
    fn test_expand_embedded_macros4() {
        let macros = &mut Vec::<Macro>::new();

        let msg_list = &mut Diagnostics::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("OPCODE1 %y"), String::from("OPCODE2 %2")],
            comment: String::default(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("$MACRO1 %2 %1"), String::from("OPCODE3")],
            comment: String::default(),
        });

        let _output = expand_embedded_macros(macros.clone(), msg_list);
        assert_eq!(
            msg_list.messages()[0].text(),
            "Invalid macro argument number y, in embedded macro \"$MACRO1 %2 %1\" in $MACRO2"
        );
    }

    #[test]
    // Test expand macros, to look for missing variable in embedded macro
    fn test_expand_embedded_macros5() {
        let macros = &mut Vec::<Macro>::new();

        let msg_list = &mut Diagnostics::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("OPCODE1 %1"), String::from("OPCODE2 %2")],
            comment: String::default(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("$MACRO1 %2"), String::from("OPCODE3")],
            comment: String::default(),
        });

        let _output = expand_embedded_macros(macros.clone(), msg_list);
        assert_eq!(
            msg_list.messages()[0].text(),
            "Missing argument 2 for embedded macro \"$MACRO1 %2\" in $MACRO2"
        );
    }

    #[test]
    // Test to embed with no variables
    fn test_expand_embedded_macros6() {
        let macros = &mut Vec::<Macro>::new();

        let msg_list = &mut Diagnostics::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("OPCODE1"), String::from("OPCODE2")],
            comment: String::default(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("$MACRO1"), String::from("OPCODE3")],
            comment: String::default(),
        });

        let output = expand_embedded_macros(macros.clone(), msg_list);
        let macros_result = &mut Vec::<Macro>::new();
        macros_result.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("OPCODE1"), String::from("OPCODE2")],
            comment: String::default(),
        });
        macros_result.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("OPCODE1"), String::from("OPCODE2"), String::from("OPCODE3")],
            comment: String::default(),
        });

        assert_eq!(output, *macros_result);
    }

    #[test]
    // Test expand macros, to make sure it expands the macros correctly
    fn test_expand_macros1() {
        use super::*;
        let mut msg_list = Diagnostics::new();
        let macros = &mut Vec::<Macro>::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("MOV %1"), String::from("RET %2")],
            comment: String::default(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("PUSH %2"), String::from("POP %1")],
            comment: String::default(),
        });
        //  let mut input: Vec<SourceLine> = Vec::<SourceLine>::new();

        let input: Vec<SourceLine> = vec![
            SourceLine {
                input_text_line: String::from("$MACRO1 A B"),
                file_name: "File1".to_owned(),
                line_counter: 1,
            },
            SourceLine {
                input_text_line: String::from("$MACRO2 C D"),
                file_name: "File2".to_owned(),
                line_counter: 2,
            },
        ];
        let pass0 = expand_macros(&mut msg_list, input, macros);
        assert_eq!(strip_comments(&pass0[0].input_text_line.clone()), "MOV A");
        assert_eq!(strip_comments(&pass0[0].file_name.clone()), "File1");
        assert_eq!(strip_comments(&pass0[1].input_text_line.clone()), "RET B");
        assert_eq!(strip_comments(&pass0[2].input_text_line.clone()), "PUSH D");
        assert_eq!(strip_comments(&pass0[3].input_text_line.clone()), "POP C");
        assert_eq!(strip_comments(&pass0[3].file_name.clone()), "File2");
    }

    #[test]
    // Test expand macros, with too few variables, gives error
    fn test_expand_macros2() {
        use super::*;
        let mut msg_list = Diagnostics::new();
        let macros = &mut Vec::<Macro>::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("MOV %1"), String::from("RET %2")],
            comment: String::default(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 2,
            items: vec![String::from("PUSH %2"), String::from("POP %1")],
            comment: String::default(),
        });
        // let input = vec![String::from("$MACRO1 A B"), String::from("$MACRO2 C")];
        let input: Vec<SourceLine> = vec![
            SourceLine {
                input_text_line: String::from("$MACRO1 A B"),
                file_name: "File1".to_owned(),
                line_counter: 1,
            },
            SourceLine {
                input_text_line: String::from("$MACRO2 C"),
                file_name: "File1".to_owned(),
                line_counter: 2,
            },
        ];

        let pass0 = expand_macros(&mut msg_list, input, macros);
        assert_eq!(strip_comments(&pass0[0].input_text_line.clone()), "MOV A");
        assert_eq!(strip_comments(&pass0[1].input_text_line.clone()), "RET B");
        assert_eq!(strip_comments(&pass0[2].input_text_line.clone()), "PUSH");
        assert_eq!(strip_comments(&pass0[3].input_text_line.clone()), "POP C");
        assert_eq!(msg_list.count(MessageType::Error), 1);
        assert_eq!(msg_list.messages()[0].text(), "Missing argument 2 for macro $MACRO2");
    }

    #[test]
    // Test expand macros, passing too many variables, gives warning
    fn test_expand_macros3() {
        use super::*;
        let mut msg_list = Diagnostics::new();
        let macros = &mut Vec::<Macro>::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("MOV %1"), String::from("RET %2")],
            comment: String::default(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 1,
            items: vec![String::from("PUSH %2")],
            comment: String::default(),
        });
        // let input = vec![String::from("$MACRO1 A B"), String::from("$MACRO2 C D")];
        let input: Vec<SourceLine> = vec![
            SourceLine {
                input_text_line: String::from("$MACRO1 A B"),
                file_name: "File1".to_owned(),
                line_counter: 1,
            },
            SourceLine {
                input_text_line: String::from("$MACRO2 C D"),
                file_name: "File1".to_owned(),
                line_counter: 2,
            },
        ];

        let pass0 = expand_macros(&mut msg_list, input, macros);
        assert_eq!(strip_comments(&pass0[0].input_text_line.clone()), "MOV A");
        assert_eq!(strip_comments(&pass0[1].input_text_line.clone()), "RET B");
        assert_eq!(strip_comments(&pass0[2].input_text_line.clone()), "PUSH D");
        assert_eq!(msg_list.count(MessageType::Warning), 1);
        assert_eq!(msg_list.messages()[0].text(), "Too many variables for macro $MACRO2");
    }

    #[test]
    // Test expand macros, missing macro, gives error
    fn test_expand_macros4() {
        use super::*;
        let mut msg_list = Diagnostics::new();
        let macros = &mut Vec::<Macro>::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("MOV %1"), String::from("RET %2")],
            comment: String::default(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 1,
            items: vec![String::from("PUSH %2")],
            comment: String::default(),
        });
        // let input = vec![String::from("$MACRO7 A B"), String::from("$MACRO2 C D")];
        let input: Vec<SourceLine> = vec![
            SourceLine {
                input_text_line: String::from("$MACRO7 A B"),
                file_name: "File1".to_owned(),
                line_counter: 1,
            },
            SourceLine {
                input_text_line: String::from("$MACRO2 C D"),
                file_name: "File1".to_owned(),
                line_counter: 1,
            },
        ];

        let _pass0 = expand_macros(&mut msg_list, input, macros);
        assert_eq!(msg_list.messages()[0].text(), "Macro not found $MACRO7 A B");
    }

    #[test]
    // Test expand macros if no macro
    fn test_expand_macros5() {
        use super::*;
        let mut msg_list = Diagnostics::new();
        let macros = &mut Vec::<Macro>::new();
        macros.push(Macro {
            name: String::from("$MACRO1"),
            variables: 2,
            items: vec![String::from("MOV %1"), String::from("RET %2")],
            comment: String::default(),
        });
        macros.push(Macro {
            name: String::from("$MACRO2"),
            variables: 1,
            items: vec![String::from("PUSH %2")],
            comment: String::default(),
        });

        let input: Vec<SourceLine> = vec![SourceLine {
            input_text_line: String::from("OPCODE1 A B"),
            file_name: "File1".to_owned(),
            line_counter: 1,
        }];

        let pass0 = expand_macros(&mut msg_list, input, macros);
        assert_eq!(strip_comments(&pass0[0].input_text_line.clone()), "OPCODE1 A B");
    }

    #[test]
    // Inline !macro is registered, its directive line removed, the rest passes through
    fn test_apply_source_macros_inline() {
        let mut msg_list = Diagnostics::new();
        let macros = &mut Vec::<Macro>::new();
        let input: Vec<SourceLine> = vec![
            SourceLine {
                input_text_line: String::from("!macro $INC3 ADDV %1 3"),
                file_name: "F".to_owned(),
                line_counter: 1,
            },
            SourceLine {
                input_text_line: String::from("$INC3 A"),
                file_name: "F".to_owned(),
                line_counter: 2,
            },
            SourceLine {
                input_text_line: String::from("NOP"),
                file_name: "F".to_owned(),
                line_counter: 3,
            },
        ];
        let output = apply_source_macros(input, macros, &mut msg_list);
        assert_eq!(output.len(), 2); // the !macro directive line is stripped
        assert_eq!(output.first().unwrap().input_text_line, "$INC3 A");
        assert_eq!(macros.len(), 1);
        assert_eq!(macros.first().unwrap().name, "$INC3");
        assert_eq!(macros.first().unwrap().items, vec![String::from("ADDV %1 3")]);
        assert_eq!(msg_list.count(MessageType::Error), 0);
    }

    #[test]
    // Redefining an existing macro is an error and does not add a duplicate
    fn test_apply_source_macros_redefine() {
        let mut msg_list = Diagnostics::new();
        let macros = &mut vec![Macro {
            name: String::from("$FOO"),
            variables: 0,
            items: vec![String::from("NOP")],
            comment: String::default(),
        }];
        let input: Vec<SourceLine> = vec![SourceLine {
            input_text_line: String::from("!macro $FOO HALT"),
            file_name: "F".to_owned(),
            line_counter: 1,
        }];
        let output = apply_source_macros(input, macros, &mut msg_list);
        assert_eq!(output, Vec::<SourceLine>::new()); // directive consumed
        assert_eq!(macros.len(), 1); // no duplicate added
        assert_eq!(msg_list.count(MessageType::Error), 1);
        assert_eq!(msg_list.messages().first().unwrap().text(), "Duplicate macro definition $FOO found");
    }

    #[test]
    // A user macro that references another user macro is embedded-expanded
    fn test_apply_source_macros_embedded() {
        let mut msg_list = Diagnostics::new();
        let macros = &mut Vec::<Macro>::new();
        let input: Vec<SourceLine> = vec![
            SourceLine {
                input_text_line: String::from("!macro $BASE NOP / NOP"),
                file_name: "F".to_owned(),
                line_counter: 1,
            },
            SourceLine {
                input_text_line: String::from("!macro $WRAP $BASE / HALT"),
                file_name: "F".to_owned(),
                line_counter: 2,
            },
        ];
        let _output = apply_source_macros(input, macros, &mut msg_list);
        let wrap = macros.iter().find(|m| m.name == "$WRAP").unwrap();
        assert_eq!(wrap.items, vec![String::from("NOP"), String::from("NOP"), String::from("HALT")]);
        assert_eq!(msg_list.count(MessageType::Error), 0);
    }

    #[test]
    // A missing file name after !macros is reported as an error
    fn test_apply_source_macros_missing_name() {
        let mut msg_list = Diagnostics::new();
        let macros = &mut Vec::<Macro>::new();
        let input: Vec<SourceLine> = vec![SourceLine {
            input_text_line: String::from("!macros"),
            file_name: "F".to_owned(),
            line_counter: 1,
        }];
        let output = apply_source_macros(input, macros, &mut msg_list);
        assert_eq!(output, Vec::<SourceLine>::new());
        assert_eq!(msg_list.count(MessageType::Error), 1);
        assert_eq!(msg_list.messages().first().unwrap().text(), "Missing file name for !macros in F");
    }

    #[test]
    // Convert string to macro with no variables
    fn test_macro_from_string1() {
        let mut msglist = Diagnostics::new();
        let input_line = String::from("$POPALL POP A / POP B");
        let macro_result = macro_from_string(&input_line, &mut msglist);
        assert_eq!(
            macro_result,
            Some(Macro {
                name: String::from("$POPALL"),
                variables: 0,
                items: vec!["POP A".to_owned(), "POP B".to_owned()],
                comment: String::default()
            })
        );
    }

    #[test]
    // Convert string to macro with variables
    fn test_macro_from_string2() {
        let mut msglist = Diagnostics::new();
        let input_line = String::from("$POPALL POP %1 / POP %2");
        let macro_result = macro_from_string(&input_line, &mut msglist);
        assert_eq!(
            macro_result,
            Some(Macro {
                name: String::from("$POPALL"),
                variables: 2,
                items: vec!["POP %1".to_owned(), "POP %2".to_owned()],
                comment: String::default()
            })
        );
    }

    #[test]
    // Convert string to macro with comments and spaces before name
    fn test_macro_from_string3() {
        let mut msglist = Diagnostics::new();
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
        let mut msglist = Diagnostics::new();
        let input_line = String::from("$POPALL POP %3 / POP %2");
        let macro_result = macro_from_string(&input_line, &mut msglist);
        assert_eq!(
            macro_result,
            Some(Macro {
                name: String::from("$POPALL"),
                variables: 3,
                items: vec!["POP %3".to_owned(), "POP %2".to_owned()],
                comment: String::default()
            })
        );
        assert_eq!(
            msglist.messages()[0].text(),
            "Error in macro variable definition for macro $POPALL, missing \"%1\""
        );
    }

    #[test]
    // Convert string to macro with variables not a macro
    fn test_macro_from_string5() {
        let mut msglist = Diagnostics::new();
        let input_line = String::from("POPALL POP %1 / POP %2");
        let macro_result = macro_from_string(&input_line, &mut msglist);
        assert_eq!(macro_result, None);
    }

    #[test]
    // Convert string to macro with variables not a macro with dollar in middle
    fn test_macro_from_string6() {
        let mut msglist = Diagnostics::new();
        let input_line = String::from("POP$ALL POP %1 / POP %2");
        let macro_result = macro_from_string(&input_line, &mut msglist);
        assert_eq!(macro_result, None);
    }

    #[test]
    // Convert string to macro with missing variables
    fn test_macro_from_string7() {
        let mut msglist = Diagnostics::new();
        let input_line = String::from("$POPALL POP %5 / POP %3");
        let _macro_result = macro_from_string(&input_line, &mut msglist);
        //assert_eq!(macro_result, None);
        assert_eq!(
            msglist.messages()[0].text(),
            "Error in macro variable definition for macro $POPALL, missing \"%1 %2 %4\"",
        );
    }
}
