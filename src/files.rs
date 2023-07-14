use crate::helper::strip_comments;
use crate::macros::Macro;
use crate::messages::{MessageType, MsgList};
use crate::opcodes::{InputData, Opcode, Pass2};
use std::io::{Error, ErrorKind};

use std::ffi::OsStr;
use std::{
    fs::File,
    io::{prelude::*, BufReader},
    path::{Path, MAIN_SEPARATOR_STR},
};

#[derive(PartialEq, Eq, Debug, Clone)]
#[allow(clippy::missing_docs_in_private_items)]
/// Defines the type of line
pub enum LineType {
    Comment,
    Blank,
    Label,
    Opcode,
    Data,
    Start,
    Error,
}

/// Open text file and return as vector of strings
///
/// Reads any given file by filename, adding the fill line by line into vector and returns None or Some(String). Manages included files.
pub fn read_file_to_vector(
    filename: &str,
    msg_list: &mut MsgList,
    opened_files: &mut Vec<String>,
) -> Option<Vec<InputData>> {
    let file_result = File::open(filename);
    if file_result.is_err() {
        msg_list.push(
            format!("Unable to open file {filename}"),
            None,
            None,
            MessageType::Error,
        );
        return None;
    }

    let Ok(file) = file_result else { return None };

    for file_found in opened_files.clone() {
        if file_found == filename {
            msg_list.push(
                format!("Recursive include of file {filename}"),
                None,
                None,
                MessageType::Error,
            );
            return None;
        }
    }

    opened_files.push(filename.to_owned());

    let buf = BufReader::new(file);
    let mut lines: Vec<InputData> = Vec::new();

    let mut line_number = 0;
    for line in buf.lines() {
        match line {
            Ok(line_contents) => {
                line_number += 1;
                if is_include(&line_contents) {
                    let include_file = get_include_filename(&line_contents);
                    if include_file.clone().unwrap_or_default() == String::new() {
                        msg_list.push(
                            format!("Missing include file name in {filename}"),
                            Some(line_number),
                            Some(filename.to_owned()),
                            MessageType::Error,
                        );
                        return None;
                    }

                    // Get the include file from the same directory as the previous file
                    let new_include_file = format!(
                        "{}{}{}",
                        Path::new(filename)
                            .parent()
                            .unwrap_or_else(|| Path::new(""))
                            .to_str()
                            .unwrap_or_default(),
                        MAIN_SEPARATOR_STR,
                        include_file.unwrap_or_default()
                    );

                    let include_lines =
                        read_file_to_vector(&new_include_file, msg_list, opened_files);
                    if include_lines.is_none() {
                        msg_list.push(
                            format!("Unable to open include file {new_include_file} in {filename}"),
                            Some(line_number),
                            Some(filename.to_owned()),
                            MessageType::Error,
                        );
                        //return None;
                        return Some(lines);
                    }
                    let unwrapped_include_lines = include_lines.unwrap_or_default();
                    for included_line in unwrapped_include_lines {
                        lines.push(included_line);
                    }
                } else {
                    lines.push(InputData {
                        input: line_contents,
                        file_name: filename.to_owned(),
                        line_counter: line_number,
                    });
                }
            }
            #[cfg(not(tarpaulin_include))] // Cannot test error reading file line in tarpaulin
            Err(err) => msg_list.push(
                format!("Error parsing opcode file: {err}"),
                Some(line_number),
                Some(filename.to_owned()),
                MessageType::Error,
            ),
        }
    }
    opened_files.pop();
    Some(lines)
}

/// Return true if string is !include
///
/// Checks if the string is include and returns true if it is
pub fn is_include(line: &str) -> bool {
    if line.trim().starts_with("!include") {
        return true;
    }
    false
}

/// Return the filename from include string
///
/// Returns the filename from include string
pub fn get_include_filename(input_line: &str) -> Option<String> {
    if !input_line.trim().starts_with("!include") {
        return None;
    }
    let mut line = input_line.replace("!include", "");
    let stripped_line = strip_comments(&mut line);
    let mut words = stripped_line.split_whitespace();
    Some(words.next().unwrap_or("").to_owned())
}

/// Remove comments from vector of strings
///
/// Checks for /* */ and removes them from the vector of strings
pub fn remove_block_comments(lines: Vec<InputData>, msg_list: &mut MsgList) -> Vec<InputData> {
    let mut in_comment = false;
    let mut new_lines: Vec<InputData> = Vec::new();
    let mut old_file_name = String::new();
    for line in lines {
        if old_file_name != line.file_name {
            if in_comment {
                msg_list.push(
                    format!("Comment not terminated in file {old_file_name}"),
                    Some(line.line_counter),
                    Some(line.file_name.clone()),
                    MessageType::Error,
                );
            }
            in_comment = false;
        }

        old_file_name = line.file_name.clone();
        let mut new_line = String::new();
        let mut in_char = false; // If in normal last was / or if in comment last was *
        for c in line.input.chars() {
            if in_comment {
                if c == '/' && in_char {
                    in_comment = false;
                } else {
                    in_char = c == '*';
                }; // Sets to true if c == '*'
            } else if c == '*' && in_char {
                in_comment = true;
                new_line.pop();
            } else if c == '/' {
                in_char = true;
                new_line.push(c);
            } else {
                in_char = false;
                new_line.push(c);
            }
        }
        new_lines.push(InputData {
            input: new_line,
            file_name: line.file_name,
            line_counter: line.line_counter,
        });
    }
    new_lines
}

/// Return the stem of given filename
///
/// Looks for first dot in the string, and returns the slice before the dot
pub fn filename_stem(full_name: &String) -> String {
    let path = Path::new(full_name);
    let stem = path.file_stem();
    let parent = path
        .parent()
        .unwrap_or_else(|| return Path::new(""))
        .join(stem.unwrap_or_else(|| return OsStr::new("")));

    return parent.to_str().unwrap_or_default().to_owned();
}

/// Output the bitcode to given file
///
/// Based on the bitcode string outputs to file
#[allow(clippy::impl_trait_in_params)]
#[allow(clippy::question_mark_used)]
pub fn write_binary_output_file(
    filename: &impl AsRef<Path>,
    output_string: &str,
) -> Result<(), std::io::Error> {
    let mut file = File::create(filename)?;

    file.write_all(output_string.as_bytes())?;

    Ok(())
}

/// Output the code details file to given filename
///
/// Writes all data to the detailed code file
#[allow(clippy::impl_trait_in_params)]
#[allow(clippy::cast_possible_truncation)]
#[allow(clippy::integer_division)]
#[allow(clippy::question_mark_used)]
pub fn write_code_output_file(
    filename: impl AsRef<Path> + core::marker::Copy,
    pass2: &mut Vec<Pass2>,
    msg_list: &mut MsgList,
) -> Result<(), std::io::Error> {
    let mut file = File::create(filename)?;

    let mut out_line = String::new();
    msg_list.push(
        format!("Writing code file to {}", filename.as_ref().display()),
        None,
        None,
        MessageType::Information,
    );
    for pass in pass2 {
        out_line.clear();
        if pass.line_type == LineType::Opcode {
            out_line = format!(
                "0x{:08X}: {:<17} -- {}\n",
                pass.program_counter,
                format_opcodes(&mut pass.opcode),
                pass.input_text_line
            );
        } else if pass.line_type == LineType::Data {
            for n in 0..pass.opcode.len() / 8 {
                out_line.push_str(
                    format!(
                        "0x{:08X}: {:<16}  -- {}\n",
                        pass.program_counter + n as u32,
                        &mut pass.opcode.get(n * 8..n * 8 + 8).unwrap_or("        "),
                        pass.input_text_line
                    )
                    .as_str(),
                );
            }
        } else if pass.line_type == LineType::Label {
            out_line = format!(
                "0x{:08X}:                   -- {}\n",
                pass.program_counter, pass.input_text_line
            );
        } else if pass.line_type == LineType::Error {
            out_line = format!(
                "Error                         -- {}\n",
                pass.input_text_line
            );
        } else {
            out_line = format!(
                "                              -- {}\n",
                pass.input_text_line
            );
        }
        file.write_all(out_line.as_bytes())?;
    }
    Ok(())
}

/// Output the opcodes for textmate to given filename
///
/// Writes all data of opcodes to textmate file
#[cfg(not(tarpaulin_include))]
#[allow(clippy::question_mark_used)]
fn output_opcodes_textmate(
    filename_stem: String,
    opcodes: &[Opcode],
    msg_list: &mut MsgList,
) -> Result<(), std::io::Error> {
    let textmate_opcode_filename = filename_stem + "_textmate.json";
    msg_list.push(
        format!("Writing textmate opcode file to {textmate_opcode_filename}"),
        None,
        None,
        MessageType::Information,
    );

    let textmate_opcode_output_file = File::create(textmate_opcode_filename.clone());
    if textmate_opcode_output_file.is_err() {
        msg_list.push(
            format!("Error opening file {textmate_opcode_filename}"),
            None,
            None,
            MessageType::Warning,
        );
        return Err(textmate_opcode_output_file
            .err()
            .unwrap_or_else(|| Error::new(ErrorKind::Other, "Unknown error")));
    }
    let  Ok(mut json_opcode_file) = textmate_opcode_output_file else { return Err(Error::new(ErrorKind::Other, "Unknown error")) };
    json_opcode_file.write_all(
        opcodes
            .iter()
            .fold(String::new(), |cur, nxt| cur + "|" + &nxt.text_name)
            .strip_prefix('|')
            .unwrap_or("")
            .as_bytes(),
    )?;

    Ok(())
}

/// Outputs the opcodes and macros as html for documentation
///
/// Writes all data of opcodes and macros to html as tables
#[cfg(not(tarpaulin_include))] // Not needed except for setting up VScode and docs
#[allow(clippy::question_mark_used)]
pub fn output_macros_opcodes_html(
    filename_stem: String,
    opcodes: &[Opcode],
    macros: Vec<Macro>,
    msg_list: &mut MsgList,
    opcodes_flag: bool,
    textmate_flag: bool,
) -> Result<(), std::io::Error> {
    use chrono::Local;

    let html_filename = filename_stem.clone() + ".html";

    if opcodes_flag {
        msg_list.push(
            format!("Outputting macros and opcodes to {html_filename}",),
            None,
            None,
            MessageType::Information,
        );

        // Open the html file
        let html_output_file = File::create(html_filename.clone());
        if html_output_file.is_err() {
            msg_list.push(
                format!("Error opening file {html_filename}"),
                None,
                None,
                MessageType::Warning,
            );
            return Err(html_output_file
                .err()
                .unwrap_or_else(|| Error::new(ErrorKind::Other, "Unknown error")));
        }
        let  Ok(mut html_file) = html_output_file else { return Err(Error::new(ErrorKind::Other, "Unknown error")) };
        html_file.write_all(b"<!DOCTYPE html>\n")?;
        html_file.write_all(b"<html>\n<head>\n<style>\n")?;
        html_file.write_all(b"#opcodes { font-family: Arial, Helvetica, sans-serif; border-collapse: collapse; width: 100%;}\n")?;
        html_file
            .write_all(b"#opcodes td, #opcodes th { border: 1px solid #ddd; padding: 8px;}\n")?;
        //file.write_all(b"#opcodes tr:nth-child(even){background-color: #f2f2f2;}\n")?; // Banded table
        html_file.write_all(b"#opcodes tr:hover {background-color: #ddd;}\n")?;
        html_file.write_all(b"#opcodes th { padding-top: 12px; padding-bottom: 12px; text-align: left; background-color: #04AA6D; color: white;}\n")?;
        html_file.write_all(b"#macros { font-family: Arial, Helvetica, sans-serif; border-collapse: collapse; width: 100%;}\n")?;
        html_file
            .write_all(b"#macros td, #macros th { border: 1px solid #ddd; padding: 8px;}\n")?;
        //file.write_all(b"#macros tr:nth-child(even){background-color: #f2f2f2;}\n");  // Banded table
        html_file.write_all(b"#macros tr:hover {background-color: #ddd;}\n")?;
        html_file.write_all(b"#macros th { padding-top: 12px; padding-bottom: 12px; text-align: left; background-color: #3004aa; color: white;}\n")?;
        html_file.write_all(b"</style>\n</head>\n<body>\n")?;
        html_file.write_all(b"<h1>Klauss ISA Instruction set and macros</h1>\n")?;
        html_file
            .write_all(format!("Created {}", Local::now().format("%d/%m/%Y %H:%M")).as_bytes())?;
        html_file.write_all(b"<h2>Opcode Table</h2>\n\n<table id=\"opcodes\">\n")?;
        html_file.write_all(b"<tr>\n    <th>Name</th>\n    <th>Opcode</th>\n    <th>Variables</th>\n    <th>Registers</th>\n    <th>Description</th>\n</tr>\n")?;

        let mut sorted_opcodes: Vec<Opcode> = opcodes.to_vec();
        sorted_opcodes.sort_by(|a, b| a.hex_opcode.cmp(&b.hex_opcode));

        let mut old_section = String::new();
        for opcode in sorted_opcodes.clone() {
            if old_section != opcode.section {
                html_file.write_all(
                format!(
                    "<tr>\n    <td colspan=\"5\" style=\"background-color:#b0b0b0;\"><b>{}</b></td>\n</tr>\n",
                    opcode.section
                )
                .as_bytes(),
            )?;
                old_section = opcode.section.clone();
            }
            html_file.write_all(format!("<tr>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n</tr>\n",
            opcode.text_name,
            opcode.hex_opcode,
            opcode.variables,
            opcode.registers,
            opcode.comment).as_bytes())?;
        }

        html_file.write_all(b"\n</table><h2>Macro Table</h2>\n<table id=\"macros\">\n")?;
        html_file.write_all(b"<tr>\n    <th>Name</th>\n    <th>Variables</th>\n    <th>Description</th>\n    <th>Details</th>\n</tr>\n")?;

        let mut sorted_macros: Vec<Macro> = macros;
        sorted_macros.sort_by(|a, b| a.name.cmp(&b.name));

        for macro_item in sorted_macros.clone() {
            html_file.write_all(
                format!(
                "<tr>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n</tr>\n",
                macro_item.name,
                macro_item.variables,
                macro_item.comment,
                macro_item
                    .items
                    .iter()
                    .fold(String::new(), |cur, nxt| cur + "  " + nxt)
            )
                .trim()
                .as_bytes(),
            )?;
        }
        html_file.write_all(b"</table>\n")?;
        html_file.write_all(b"</body>\n</html>\n")?;

        output_macros_opcodes_json(
            filename_stem.clone(),
            &sorted_opcodes,
            &sorted_macros,
            msg_list,
        )?;
    }
    // Write out the Textmate
    #[allow(clippy::print_stdout)]
    if textmate_flag {
        output_opcodes_textmate(filename_stem, opcodes, msg_list)?;
    }
    Ok(())
}

/// Outputs the opcodes and macros as JSON files
///
/// Writes all macros and opcodes to JSON files
#[cfg(not(tarpaulin_include))] // Not needed except for setting up VScode and docs
#[allow(clippy::question_mark_used)]
pub fn output_macros_opcodes_json(
    filename_stem: String,
    opcodes: &[Opcode],
    macros: &[Macro],
    msg_list: &mut MsgList,
) -> Result<(), std::io::Error> {
    let json_opcode_filename = filename_stem.clone() + "_opcodes.json";
    let json_macro_filename = filename_stem + "_macro.json";

    msg_list.push(
        format!("Writing JSON opcode file {json_opcode_filename}"),
        None,
        None,
        MessageType::Information,
    );

    msg_list.push(
        format!("Writing JSON macro file {json_macro_filename}"),
        None,
        None,
        MessageType::Information,
    );

    // Write out the JSON opcode file
    let json_opcode_output_file = File::create(json_opcode_filename.clone());
    if json_opcode_output_file.is_err() {
        msg_list.push(
            format!("Error opening file {json_opcode_filename}"),
            None,
            None,
            MessageType::Warning,
        );
        return Err(json_opcode_output_file
            .err()
            .unwrap_or_else(|| Error::new(ErrorKind::Other, "Unknown error")));
    }
    let  Ok(mut json_opcode_file) = json_opcode_output_file else { return Err(Error::new(ErrorKind::Other, "Unknown error")) };
    json_opcode_file.write_all(
        serde_json::to_string_pretty(&opcodes)
            .unwrap_or_default()
            .as_bytes(),
    )?;

    // Write out the JSON macro file
    let json_macro_output_file = File::create(json_macro_filename.clone());
    if json_macro_output_file.is_err() {
        msg_list.push(
            format!("Error opening file {json_macro_filename}"),
            None,
            None,
            MessageType::Warning,
        );
        return Err(json_macro_output_file
            .err()
            .unwrap_or_else(|| Error::new(ErrorKind::Other, "Unknown error")));
    }
    let  Ok(mut json_macro_file) = json_macro_output_file else { return Err(Error::new(ErrorKind::Other, "Unknown error")) };
    json_macro_file.write_all(
        serde_json::to_string_pretty(&macros)
            .unwrap_or_default()
            .as_bytes(),
    )?;
    Ok(())
}

/// Format a given string, adding spaces between groups of 4
///
/// For string of 8 and 12 charters adds spaces between groups of 4 characters, otherwise returns original string
pub fn format_opcodes(input: &mut String) -> String {
    if input.len() == 4 {
        return (*input).clone() + "              ";
    }
    if input.len() == 8 {
        return input.get(0..4).unwrap_or("    ").to_owned()
            + input.get(4..8).unwrap_or("    ")
            + "         ";
    }
    if input.len() == 16 {
        return input.get(0..4).unwrap_or("    ").to_owned()
            + input.get(4..8).unwrap_or("    ")
            + " "
            + input.get(8..12).unwrap_or("    ")
            + input.get(12..16).unwrap_or("    ");
    }
    (*input).clone()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod test {
    use std::fs;

    use super::*;
    use tempfile::TempDir;

    #[test]
    // Test remove of comments in single line
    fn test_remove_block_comments1() {
        let input = vec![InputData {
            input: "abc/* This is a comment */def".to_owned(),
            file_name: "test.kla".to_owned(),
            line_counter: 1,
        }];
        let output = remove_block_comments(input, &mut MsgList::new());
        assert_eq!(
            output,
            vec![InputData {
                input: "abcdef".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 1,
            }]
        );
    }

    #[test]
    // Test remove of comments in two lines
    fn test_remove_block_comments2() {
        let input = vec![
            InputData {
                input: "abc/* This is a comment */def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "abc/* This is a comment */def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 2,
            },
        ];
        let output = remove_block_comments(input, &mut MsgList::new());
        assert_eq!(
            output,
            vec![
                InputData {
                    input: "abcdef".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 1,
                },
                InputData {
                    input: "abcdef".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 2
                },
            ]
        );
    }

    #[test]
    // Test remove of comments in across two lines
    fn test_remove_block_comments3() {
        let input = vec![
            InputData {
                input: "abc/* This is a comment ".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "so is this */def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 2,
            },
        ];
        let output = remove_block_comments(input, &mut MsgList::new());
        assert_eq!(
            output,
            vec![
                InputData {
                    input: "abc".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 1,
                },
                InputData {
                    input: "def".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 2,
                },
            ]
        );
    }

    #[test]
    // Test remove of comments in across three line with blank line left
    fn test_remove_block_comments4() {
        let input = vec![
            InputData {
                input: "abc/* This is a comment ".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "so is this def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 2,
            },
            InputData {
                input: "*/def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 3,
            },
        ];
        let output = remove_block_comments(input, &mut MsgList::new());
        assert_eq!(
            output,
            vec![
                InputData {
                    input: "abc".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 1,
                },
                InputData {
                    input: String::new(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 2,
                },
                InputData {
                    input: "def".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 3,
                },
            ]
        );
    }

    #[test]
    // Test restart comments
    fn test_remove_block_comments5() {
        let input = vec![
            InputData {
                input: "abc/* This is a /* /*comment ".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "so is this def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 2,
            },
            InputData {
                input: "*/def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 3,
            },
        ];
        let output = remove_block_comments(input, &mut MsgList::new());
        assert_eq!(
            output,
            vec![
                InputData {
                    input: "abc".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 1,
                },
                InputData {
                    input: String::new(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 2,
                },
                InputData {
                    input: "def".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 3,
                },
            ]
        );
    }

    #[test]
    // Test comments are closed at end of file
    fn test_remove_block_comments6() {
        let msg_list = &mut MsgList::new();
        let input = vec![
            InputData {
                input: "abc/* This is a /* /*comment ".to_owned(),
                file_name: "test1.kla".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "so is this def".to_owned(),
                file_name: "test1.kla".to_owned(),
                line_counter: 2,
            },
            InputData {
                input: "def".to_owned(),
                file_name: "test2.kla".to_owned(),
                line_counter: 3,
            },
        ];
        let output = remove_block_comments(input, msg_list);
        assert_eq!(
            output,
            vec![
                InputData {
                    input: "abc".to_owned(),
                    file_name: "test1.kla".to_owned(),
                    line_counter: 1,
                },
                InputData {
                    input: String::new(),
                    file_name: "test1.kla".to_owned(),
                    line_counter: 2,
                },
                InputData {
                    input: "def".to_owned(),
                    file_name: "test2.kla".to_owned(),
                    line_counter: 3,
                },
            ]
        );
        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            "Comment not terminated in file test1.kla"
        );
    }

    #[test]
    // Tests for the is_include to check if there is a !include as first non white-space
    fn test_is_include1() {
        assert!(is_include("!include file.type"));
        assert!(is_include("!include"));
        assert!(is_include("    !include   123 234 456"));
    }

    #[test]
    // Tests for the not is_include
    fn test_is_include2() {
        assert!(!is_include("include file.type"));
        assert!(!is_include("!!include"));
    }

    #[test]
    // Check functions returns correct filename from !include
    fn test_get_include_filename() {
        assert_eq!(
            get_include_filename("!include myfile.name"),
            Some("myfile.name".to_owned())
        );
        assert_eq!(get_include_filename("test_line"), None);
        assert_eq!(
            get_include_filename("!include myfile.name extra words"),
            Some("myfile.name".to_owned())
        );
        assert_eq!(get_include_filename("!include"), Some(String::new()));
        assert_eq!(
            get_include_filename("!include //test comment"),
            Some(String::new())
        );
    }

    #[test]
    // Check correct filename stem is returned
    fn test_filename_stem() {
        assert_eq!(filename_stem(&"file.type".to_owned()), "file");
        assert_eq!(filename_stem(&"file".to_owned()), "file");
        assert_eq!(
            filename_stem(&format!(
                "{MAIN_SEPARATOR_STR}my_path{MAIN_SEPARATOR_STR}file.kla"
            )),
            format!("{MAIN_SEPARATOR_STR}my_path{MAIN_SEPARATOR_STR}file")
        );
        assert_eq!(
            filename_stem(&format!("relative_path{MAIN_SEPARATOR_STR}file.kla")),
            format!("relative_path{MAIN_SEPARATOR_STR}file")
        );
    }

    #[test]
    // Check for formatting of codes used for debug file
    fn test_format_opcodes() {
        assert_eq!(
            format_opcodes(&mut "0000000000000000".to_owned()),
            "00000000 00000000"
        );
        assert_eq!(format_opcodes(&mut "0000".to_owned()), "0000              ");
        assert_eq!(
            format_opcodes(&mut "0123456789ABCDEF".to_owned()),
            "01234567 89ABCDEF"
        );
        assert_eq!(
            format_opcodes(&mut "12345678".to_owned()),
            "12345678         "
        );
        assert_eq!(format_opcodes(&mut "123".to_owned()), "123");
    }

    #[test]
    // Test for missing file
    fn test_read_file_to_vec1() {
        let mut msg_list = MsgList::new();
        let mut opened_files: Vec<String> = Vec::new();
        read_file_to_vector("////xxxxxxx", &mut msg_list, &mut opened_files);
        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            "Unable to open file ////xxxxxxx"
        );
    }

    #[test]
    #[allow(clippy::let_underscore_must_use)]
    // Test for simple file added
    fn test_write_file_to_vec2() {
        let mut msg_list = MsgList::new();
        let mut opened_files: Vec<String> = Vec::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap_or_default();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        _ = writeln!(tmp_file1, "Test line in file");
        _ = writeln!(tmp_file1, "Test line in file 1");
        _ = writeln!(tmp_file1, "Test line in file 2");
        _ = writeln!(tmp_file1, "Test line in file 4");

        let lines = read_file_to_vector(file_name1, &mut msg_list, &mut opened_files);
        assert_eq!(
            lines.clone().unwrap().get(0).unwrap_or_default().input,
            "Test line in file"
        );
        assert_eq!(lines.unwrap().len(), 4);

        drop(tmp_file1);
        _ = tmp_dir.close();
    }

    #[test]
    #[allow(clippy::let_underscore_must_use)]
    // Test for included file
    fn test_write_file_to_vec3() {
        let mut msg_list = MsgList::new();
        let mut opened_files: Vec<String> = Vec::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        _ = writeln!(tmp_file1, "Test line in file 1 line 0");
        _ = writeln!(tmp_file1, "!include test2.kla");
        _ = writeln!(tmp_file1, "Test line in file 1 line 1");
        _ = writeln!(tmp_file1, "Test line in file 1 line 2");

        let file_path2 = tmp_dir.path().join("test2.kla");
        let mut tmp_file2: File = File::create(file_path2).unwrap();
        _ = writeln!(tmp_file2, "Test line in file 2 line 0");
        _ = writeln!(tmp_file2, "Test line in file 2 line 1");
        _ = writeln!(tmp_file2, "Test line in file 2 line 2");
        _ = writeln!(tmp_file2, "Test line in file 2 line 4");

        let lines = read_file_to_vector(file_name1, &mut msg_list, &mut opened_files);
        assert_eq!(
            lines
                .clone()
                .unwrap_or_default()
                .get(0)
                .unwrap_or_default()
                .input,
            "Test line in file 1 line 0"
        );
        assert_eq!(
            lines
                .clone()
                .unwrap_or_default()
                .get(2)
                .unwrap_or_default()
                .input,
            "Test line in file 2 line 1"
        );
        assert_eq!(
            lines
                .clone()
                .unwrap_or_default()
                .get(6)
                .unwrap_or_default()
                .input,
            "Test line in file 1 line 2"
        );
        assert_eq!(lines.unwrap().len(), 7);

        drop(tmp_file1);
        drop(tmp_file2);
        _ = tmp_dir.close();
    }

    #[test]
    #[allow(clippy::let_underscore_must_use)]
    // Test for recursive file includes
    fn test_write_file_to_vec4() {
        let mut msg_list = MsgList::new();
        let mut opened_files: Vec<String> = Vec::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        _ = writeln!(tmp_file1, "Test line in file 1 line 0");
        _ = writeln!(tmp_file1, "!include test2.kla");
        _ = writeln!(tmp_file1, "Test line in file 1 line 1");
        _ = writeln!(tmp_file1, "Test line in file 1 line 2");

        let file_path2 = tmp_dir.path().join("test2.kla");
        let mut tmp_file2: File = File::create(file_path2).unwrap();
        _ = writeln!(tmp_file2, "Test line in file 2 line 0");
        _ = writeln!(tmp_file2, "Test line in file 2 line 1");
        _ = writeln!(tmp_file2, "!include test1.kla");
        _ = writeln!(tmp_file2, "Test line in file 2 line 4");

        _ = read_file_to_vector(file_name1, &mut msg_list, &mut opened_files);
        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            format!("Recursive include of file {file_name1}")
        );

        drop(tmp_file1);
        drop(tmp_file2);
        _ = tmp_dir.close();
    }

    #[test]
    #[allow(clippy::let_underscore_must_use)]
    // Test for missing included file
    fn test_write_file_to_vec5() {
        let mut msg_list = MsgList::new();
        let mut opened_files: Vec<String> = Vec::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap();
        let file_path2 = tmp_dir.path().join("test2.kla");
        let binding2 = file_path2;
        let file_name2: &str = binding2.to_str().unwrap();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        _ = writeln!(tmp_file1, "!include test2.kla");
        _ = writeln!(tmp_file1, "Test line in file 1 line 1");
        _ = writeln!(tmp_file1, "Test line in file 1 line 2");

        let lines = read_file_to_vector(file_name1, &mut msg_list, &mut opened_files);
        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            format!("Unable to open file {file_name2}")
        );
        assert_eq!(
            msg_list.list.get(1).unwrap_or_default().text,
            format!("Unable to open include file {file_name2} in {file_name1}")
        );
        assert_eq!(lines, Some(vec![]));

        drop(tmp_file1);
        _ = tmp_dir.close();
    }
    #[test]
    #[allow(clippy::let_underscore_must_use)]
    // Test for missing included file name
    fn test_write_file_to_vec6() {
        let mut msg_list = MsgList::new();
        let mut opened_files: Vec<String> = Vec::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        _ = writeln!(tmp_file1, "!include");
        _ = writeln!(tmp_file1, "Test line in file 1 line 1");
        _ = writeln!(tmp_file1, "Test line in file 1 line 2");

        let lines = read_file_to_vector(file_name1, &mut msg_list, &mut opened_files);
        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            format!("Missing include file name in {file_name1}")
        );
        assert_eq!(lines, None);

        drop(tmp_file1);
        _ = tmp_dir.close();
    }

    #[test]
    #[allow(clippy::let_underscore_must_use)]
    // Test for double included file
    fn test_write_file_to_vec7() {
        let mut msg_list = MsgList::new();
        let mut opened_files: Vec<String> = Vec::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        _ = writeln!(tmp_file1, "Test line in file 1 line 0");
        _ = writeln!(tmp_file1, "!include test2.kla");
        _ = writeln!(tmp_file1, "Test line in file 1 line 1");
        _ = writeln!(tmp_file1, "Test line in file 1 line 2");

        let file_path2 = tmp_dir.path().join("test2.kla");
        let mut tmp_file2: File = File::create(file_path2.clone()).unwrap();
        let binding2 = file_path2;
        let file_name2: &str = binding2.to_str().unwrap();
        _ = writeln!(tmp_file2, "Test line in file 2 line 0");
        _ = writeln!(tmp_file2, "!include test3.kla");
        _ = writeln!(tmp_file2, "Test line in file 2 line 2");
        _ = writeln!(tmp_file2, "Test line in file 2 line 4");

        let file_path3 = tmp_dir.path().join("test3.kla");
        let binding3 = file_path3;
        let file_name3: &str = binding3.to_str().unwrap();

        _ = read_file_to_vector(file_name1, &mut msg_list, &mut opened_files);
        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            format!("Unable to open file {file_name3}")
        );
        assert_eq!(
            msg_list.list.get(1).unwrap_or_default().text,
            format!("Unable to open include file {file_name3} in {file_name2}")
        );

        drop(tmp_file1);
        drop(tmp_file2);
        _ = tmp_dir.close();
    }

    #[test]
    fn test_write_binary_output_file() {
        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.klb");
        let binding = file_path1;
        let file_name1: &str = binding.to_str().unwrap();

        let result_write = write_binary_output_file(&file_name1, "12345678");
        result_write.unwrap();

        let bytes = fs::read(file_name1).unwrap();
        assert_eq!(bytes, b"12345678");
    }

    #[test]
    fn test_write_code_output_file() {
        let tmp_dir = TempDir::new().unwrap();
        let mut msg_list = MsgList::new();
        let mut pass2: Vec<Pass2> = vec![
            Pass2 {
                input_text_line: "MOV 0xEEEEEEEE 0xFFFFFFFF".to_owned(),
                file_name: String::from("test"),
                line_counter: 1,
                program_counter: 0,
                line_type: LineType::Opcode,
                opcode: String::from("x"),
            },
            Pass2 {
                input_text_line: "DELAY 0x7".to_owned(),
                file_name: String::from("test"),
                line_counter: 1,
                program_counter: 1,
                line_type: LineType::Opcode,
                opcode: String::from("000F013"),
            },
            Pass2 {
                input_text_line: "PUSH A".to_owned(),
                file_name: String::from("test"),
                line_counter: 2,
                program_counter: 3,
                line_type: LineType::Opcode,
                opcode: String::from("0000F013"),
            },
            Pass2 {
                input_text_line: "RET".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 4,
                line_type: LineType::Opcode,
                opcode: String::from("0000F013"),
            },
            Pass2 {
                input_text_line: "RET".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Opcode,
                opcode: String::from("0000F013"),
            },
            Pass2 {
                input_text_line: ":ERIC".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Label,
                opcode: String::new(),
            },
            Pass2 {
                input_text_line: "// Comment".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Comment,
                opcode: String::from("test"),
            },
            Pass2 {
                input_text_line: "#DATA1 \"HELLO\"".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Data,
                opcode: String::from("12345678FFFFFFFFDDDDDDDD"),
            },
            Pass2 {
                input_text_line: "xxx".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Error,
                opcode: String::from("test"),
            },
        ];

        let file_path1 = tmp_dir.path().join("test1.klc");
        let binding = file_path1;
        let file_name1: &str = binding.to_str().unwrap();

        let result_write = write_code_output_file(file_name1, &mut pass2, &mut msg_list);
        result_write.unwrap();

        let buffer = fs::read_to_string(file_name1).unwrap();
        assert_eq!(buffer.lines().count(), 11);
        assert_eq!(buffer, "0x00000000: x                 -- MOV 0xEEEEEEEE 0xFFFFFFFF\n0x00000001: 000F013           -- DELAY 0x7\n0x00000003: 0000F013          -- PUSH A\n0x00000004: 0000F013          -- RET\n0x00000005: 0000F013          -- RET\n0x00000005:                   -- :ERIC\n                              -- // Comment\n0x00000005: 12345678          -- #DATA1 \"HELLO\"\n0x00000006: FFFFFFFF          -- #DATA1 \"HELLO\"\n0x00000007: DDDDDDDD          -- #DATA1 \"HELLO\"\nError                         -- xxx\n");
    }
}
