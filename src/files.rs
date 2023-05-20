use crate::helper::{strip_comments, trim_newline};
use crate::macros::Macro;
use crate::messages::{MessageType, MsgList};
use crate::opcodes::{InputData, Opcode, Pass2};

use std::ffi::OsStr;
use std::{
    fs::File,
    io::{prelude::*, BufReader},
    path::{Path, MAIN_SEPARATOR_STR},
};

/// Enum for the type of line
#[derive(PartialEq, Eq, Debug)]
pub enum LineType {
    /// Comment line type
    Comment,
    /// Blank line type
    Blank,
    /// Label line type
    Label,
    /// Opcode line type
    Opcode,
    /// Data line type
    Data,
    /// Error line type
    Error,
}

/// Open text file and return as vector of strings
///
/// Reads any given file by filename, adding the fill line by line into vector and returns None or Some(String). Manages included files.
// #[cfg(not(tarpaulin_include))] // Cannot test reading file in tarpaulin
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
    #[allow(clippy::unwrap_used)]
    let file = file_result.unwrap();

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

    opened_files.push(filename.to_string());

    let buf = BufReader::new(file);
    let mut lines: Vec<InputData> = Vec::new();

    let mut line_number = 0;
    for line in buf.lines() {
        match line {
            Ok(v) => {
                line_number += 1;
                if is_include(&v) {
                    let include_file = get_include_filename(&v);
                    if include_file.clone().unwrap_or_default() == String::new() {
                        msg_list.push(
                            format!("Missing include file name in {filename}"),
                            Some(line_number),
                            Some(filename.to_string()),
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
                            Some(filename.to_string()),
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
                        input: v,
                        file_name: filename.to_string(),
                        line_counter: line_number,
                    });
                }
            }
            #[cfg(not(tarpaulin_include))] // Cannot test error reading file line in tarpaulin
            Err(e) => msg_list.push(
                format!("Error parsing opcode file: {e}"),
                Some(line_number),
                Some(filename.to_string()),
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
#[cfg(not(tarpaulin_include))] // Cannot test writing file in tarpaulin
#[allow(clippy::impl_trait_in_params)]
pub fn write_binary_output_file(filename: &impl AsRef<Path>, output_string: &str) -> bool {
    let result_file = File::create(filename);

    if result_file.is_err() {
        return false;
    }
    #[allow(clippy::unwrap_used)]
    let mut file = result_file.unwrap();

    if file.write(output_string.as_bytes()).is_err() {
        return false;
    };

    true
}

/// Output the code details file to given filename
///
/// Writes all data to the detailed code file
#[cfg(not(tarpaulin_include))] // Cannot test writing file in tarpaulin
#[allow(clippy::impl_trait_in_params)]
pub fn write_code_output_file(filename: impl AsRef<Path>, pass2: &mut Vec<Pass2>) -> bool {
    let result_file = File::create(filename);
    if result_file.is_err() {
        return false;
    }
    #[allow(clippy::unwrap_used)]
    let mut file = result_file.unwrap();
    let mut out_line: String;

    for pass in pass2 {
        if pass.line_type == LineType::Opcode {
            out_line = format!(
                "0x{:08X}: {:<16} -- {}\n",
                pass.program_counter,
                format_opcodes(&mut pass.opcode),
                pass.input
            );
        } else if pass.line_type == LineType::Data || pass.line_type == LineType::Label {
            out_line = format!(
                "0x{:08X}:                   -- {}\n",
                pass.program_counter, pass.input
            );
        } else if pass.line_type == LineType::Error {
            out_line = format!("Error                         -- {}\n", pass.input);
        } else {
            out_line = format!("                              -- {}\n", pass.input);
        }
        if file.write(out_line.as_bytes()).is_err() {
            return false;
        };
    }
    true
}

/// Outputs the opcodes and macros details file to given filename
///
/// Writes all data to the html ISA and macro file file
#[cfg(not(tarpaulin_include))] // Not needed except for setting up VScode and docs
#[allow(clippy::impl_trait_in_params)]
pub fn output_macros_opcodes(
    filename: impl AsRef<Path> + core::clone::Clone,
    opcodes: Vec<Opcode>,
    macros: Vec<Macro>,
    msg_list: &mut MsgList,
) {
    use chrono::Local;

    msg_list.push(
        format!(
            "Outputting macros and opcodes to {}",
            filename.as_ref().display()
        ),
        None,
        None,
        MessageType::Information,
    );

    let output_file = File::create(filename.clone());
    if output_file.is_err() {
        msg_list.push(
            format!("Error opening file {}", filename.as_ref().display()),
            None,
            None,
            MessageType::Information,
        );
        return;
    }
    #[allow(clippy::unwrap_used)]
    let mut file = output_file.unwrap();
    let _ = file.write(b"<!DOCTYPE html>\n");
    let _ = file.write(b"<html>\n<head>\n<style>\n");
    let _ = file.write(b"#opcodes { font-family: Arial, Helvetica, sans-serif; border-collapse: collapse; width: 100%;}\n");
    let _ = file.write(b"#opcodes td, #opcodes th { border: 1px solid #ddd; padding: 8px;}\n");
    //let _ = file.write(b"#opcodes tr:nth-child(even){background-color: #f2f2f2;}\n"); // Banded table
    let _ = file.write(b"#opcodes tr:hover {background-color: #ddd;}\n");
    let _ = file.write(b"#opcodes th { padding-top: 12px; padding-bottom: 12px; text-align: left; background-color: #04AA6D; color: white;}\n");
    let _ = file.write(b"#macros { font-family: Arial, Helvetica, sans-serif; border-collapse: collapse; width: 100%;}\n");
    let _ = file.write(b"#macros td, #macros th { border: 1px solid #ddd; padding: 8px;}\n");
    //let _ = file.write(b"#macros tr:nth-child(even){background-color: #f2f2f2;}\n");  // Banded table
    let _ = file.write(b"#macros tr:hover {background-color: #ddd;}\n");
    let _ = file.write(b"#macros th { padding-top: 12px; padding-bottom: 12px; text-align: left; background-color: #3004aa; color: white;}\n");
    let _ = file.write(b"</style>\n</head>\n<body>\n");
    let _ = file.write(b"<h1>Klauss ISA Instruction set and macros</h1>\n");
    let _ = file.write(format!("Created {}", Local::now().format("%d/%m/%Y %H:%M")).as_bytes());
    let _ = file.write(b"<h2>Opcode Table</h2>\n\n<table id=\"opcodes\">\n");
    let _ = file.write(b"<tr>\n    <th>Name</th>\n    <th>Opcode</th>\n    <th>Variables</th>\n    <th>Registers</th>\n    <th>Description</th>\n</tr>\n");

    let mut sorted_opcodes: Vec<Opcode> = opcodes;
    sorted_opcodes.sort_by(|a, b| a.hex_opcode.cmp(&b.hex_opcode));

    let mut old_section = String::new();
    for opcode in sorted_opcodes.clone() {
        if old_section != opcode.section {
            let _ = file.write(
                format!(
                    "<tr>\n    <td colspan=\"5\" style=\"background-color:#b0b0b0;\"><b>{}</b></td>\n</tr>\n",
                    opcode.section
                )
                .as_bytes(),
            );
            old_section = opcode.section.clone();
        }
        let _ = file.write(format!("<tr>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n</tr>\n",
            opcode.text_name,
            opcode.hex_opcode,
            opcode.variables,
            opcode.registers,
            opcode.comment).as_bytes());
    }

    let _ = file.write(b"\n</table><h2>Macro Table</h2>\n<table id=\"macros\">\n");
    let _ = file.write(b"<tr>\n    <th>Name</th>\n    <th>Variables</th>\n    <th>Description</th>\n    <th>Details</th>\n</tr>\n");

    let mut sorted_macros: Vec<Macro> = macros;
    sorted_macros.sort_by(|a, b| a.name.cmp(&b.name));

    for macro_item in sorted_macros {
        let _ = file.write(
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
        );
    }
    let _ = file.write(b"</table>\n");

    let _ = file.write(b"</body>\n</html>\n");
}

/// Format a given string, adding spaces between groups of 4
///
/// For string of 8 and 12 charters adds spaces between groups of 4 characters, otherwise returns original string
pub fn format_opcodes(input: &mut String) -> String {
    if input.len() == 4 {
        return (*input).clone() + "              ";
    }
    if input.len() == 8 {
        return input[0..4].to_string() + &input[4..8] + "         ";
    }
    if input.len() == 16 {
        return input[0..4].to_string() + &input[4..8] + " " + &input[8..12] + &input[12..16];
    }
    (*input).clone()
}

/// Output the code details file to given serial port
///
/// Will send the program to the serial port, and wait for the response
#[allow(clippy::cast_possible_wrap)]
#[cfg(not(tarpaulin_include))] // Cannot test writing to serial in tarpaulin
pub fn write_serial(binary_output: &str, port_name: &str, msg_list: &mut MsgList) -> bool {
    let mut buffer = [0; 1024];
    let port_result = serialport::new(port_name, 115_200)
        .timeout(core::time::Duration::from_millis(100))
        .open();

    if port_result.is_err() {
        let mut all_ports: String = String::new();
        let available_ports = serialport::available_ports();

        match available_ports {
            Err(_) => {
                msg_list.push(
                    "Error opening serial port, no ports found".to_string(),
                    None,
                    None,
                    MessageType::Error,
                );
                return false;
            }
            Ok(ports) => {
                let mut max_ports: i32 = -1;
                for (port_count, p) in (0_u32..).zip(ports.into_iter()) {
                    if port_count > 0 {
                        all_ports.push_str(" , ");
                    }
                    all_ports.push_str(&p.port_name);
                    max_ports = port_count as i32;
                }

                let ports_msg = match max_ports {
                    -1_i32 => "no ports were found".to_string(),
                    0_i32 => {
                        format!("only port {all_ports} was found")
                    }
                    _ => {
                        format!("the following ports were found {all_ports}")
                    }
                };

                msg_list.push(
                    format!("Error opening serial port {port_name}, {ports_msg}"),
                    None,
                    None,
                    MessageType::Error,
                );
                return false;
            }
        }
    }
    #[allow(clippy::unwrap_used)]
    let mut port = port_result.unwrap();

    if port.set_stop_bits(serialport::StopBits::One).is_err() {
        return false;
    }
    if port.set_data_bits(serialport::DataBits::Eight).is_err() {
        return false;
    }
    if port.set_parity(serialport::Parity::None).is_err() {
        return false;
    }

    if port.read(&mut buffer[..]).is_err() { //clear any old messages in buffer
    }

    if port.write(binary_output.as_bytes()).is_err() {
        return false;
    }

    if port.flush().is_err() {
        return false;
    }

    let ret_msg_size = port.read(&mut buffer[..]).unwrap_or(0);

    if ret_msg_size == 0 {
        msg_list.push(
            "No message received from board".to_string(),
            None,
            None,
            MessageType::Warning,
        );
        return true;
    }

    let ret_msg = String::from_utf8(buffer[..ret_msg_size].to_vec());

    if ret_msg.is_err() {
        msg_list.push(
            "Invalid message received from board".to_string(),
            None,
            None,
            MessageType::Warning,
        );
        return true;
    }

    let mut print_ret_msg = ret_msg.unwrap_or_else(|_| String::new());

    trim_newline(&mut print_ret_msg); //Board can send CR/LF messages

    msg_list.push(
        format!("Message received from board is \"{print_ret_msg}\""),
        None,
        None,
        MessageType::Information,
    );

    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod test {

    use super::*;
    use tempfile::TempDir;

    #[test]
    // Test remove of comments in single line
    fn test_remove_block_comments1() {
        let input = vec![InputData {
            input: "abc/* This is a comment */def".to_string(),
            file_name: "test.kla".to_string(),
            line_counter: 1,
        }];
        let output = remove_block_comments(input, &mut MsgList::new());
        assert_eq!(
            output,
            vec![InputData {
                input: "abcdef".to_string(),
                file_name: "test.kla".to_string(),
                line_counter: 1,
            }]
        );
    }

    #[test]
    // Test remove of comments in two lines
    fn test_remove_block_comments2() {
        let input = vec![
            InputData {
                input: "abc/* This is a comment */def".to_string(),
                file_name: "test.kla".to_string(),
                line_counter: 1,
            },
            InputData {
                input: "abc/* This is a comment */def".to_string(),
                file_name: "test.kla".to_string(),
                line_counter: 2,
            },
        ];
        let output = remove_block_comments(input, &mut MsgList::new());
        assert_eq!(
            output,
            vec![
                InputData {
                    input: "abcdef".to_string(),
                    file_name: "test.kla".to_string(),
                    line_counter: 1,
                },
                InputData {
                    input: "abcdef".to_string(),
                    file_name: "test.kla".to_string(),
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
                input: "abc/* This is a comment ".to_string(),
                file_name: "test.kla".to_string(),
                line_counter: 1,
            },
            InputData {
                input: "so is this */def".to_string(),
                file_name: "test.kla".to_string(),
                line_counter: 2,
            },
        ];
        let output = remove_block_comments(input, &mut MsgList::new());
        assert_eq!(
            output,
            vec![
                InputData {
                    input: "abc".to_string(),
                    file_name: "test.kla".to_string(),
                    line_counter: 1,
                },
                InputData {
                    input: "def".to_string(),
                    file_name: "test.kla".to_string(),
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
                input: "abc/* This is a comment ".to_string(),
                file_name: "test.kla".to_string(),
                line_counter: 1,
            },
            InputData {
                input: "so is this def".to_string(),
                file_name: "test.kla".to_string(),
                line_counter: 2,
            },
            InputData {
                input: "*/def".to_string(),
                file_name: "test.kla".to_string(),
                line_counter: 3,
            },
        ];
        let output = remove_block_comments(input, &mut MsgList::new());
        assert_eq!(
            output,
            vec![
                InputData {
                    input: "abc".to_string(),
                    file_name: "test.kla".to_string(),
                    line_counter: 1,
                },
                InputData {
                    input: String::new(),
                    file_name: "test.kla".to_string(),
                    line_counter: 2,
                },
                InputData {
                    input: "def".to_string(),
                    file_name: "test.kla".to_string(),
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
                input: "abc/* This is a /* /*comment ".to_string(),
                file_name: "test.kla".to_string(),
                line_counter: 1,
            },
            InputData {
                input: "so is this def".to_string(),
                file_name: "test.kla".to_string(),
                line_counter: 2,
            },
            InputData {
                input: "*/def".to_string(),
                file_name: "test.kla".to_string(),
                line_counter: 3,
            },
        ];
        let output = remove_block_comments(input, &mut MsgList::new());
        assert_eq!(
            output,
            vec![
                InputData {
                    input: "abc".to_string(),
                    file_name: "test.kla".to_string(),
                    line_counter: 1,
                },
                InputData {
                    input: String::new(),
                    file_name: "test.kla".to_string(),
                    line_counter: 2,
                },
                InputData {
                    input: "def".to_string(),
                    file_name: "test.kla".to_string(),
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
                input: "abc/* This is a /* /*comment ".to_string(),
                file_name: "test1.kla".to_string(),
                line_counter: 1,
            },
            InputData {
                input: "so is this def".to_string(),
                file_name: "test1.kla".to_string(),
                line_counter: 2,
            },
            InputData {
                input: "def".to_string(),
                file_name: "test2.kla".to_string(),
                line_counter: 3,
            },
        ];
        let output = remove_block_comments(input, msg_list);
        assert_eq!(
            output,
            vec![
                InputData {
                    input: "abc".to_string(),
                    file_name: "test1.kla".to_string(),
                    line_counter: 1,
                },
                InputData {
                    input: String::new(),
                    file_name: "test1.kla".to_string(),
                    line_counter: 2,
                },
                InputData {
                    input: "def".to_string(),
                    file_name: "test2.kla".to_string(),
                    line_counter: 3,
                },
            ]
        );
        assert_eq!(
            msg_list.list[0].name,
            "Comment not terminated in file test1.kla"
        );
    }

    #[test]
    // Tests for the is_include to check if there is a !include as first non white-space
    fn test_is_include() {
        assert!(is_include("!include file.type"));
        assert!(!is_include("include file.type"));
        assert!(is_include("!include"));
        assert!(!is_include("!!include"));
        assert!(is_include("    !include   123 234 456"));
    }

    #[test]
    // Check functions returns correct filename from !include
    fn test_get_include_filename() {
        assert_eq!(
            get_include_filename("!include myfile.name"),
            Some("myfile.name".to_string())
        );
        assert_eq!(get_include_filename("test_line"), None);
        assert_eq!(
            get_include_filename("!include myfile.name extra words"),
            Some("myfile.name".to_string())
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
        assert_eq!(filename_stem(&"file.type".to_string()), "file");
        assert_eq!(filename_stem(&"file".to_string()), "file");
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
            format_opcodes(&mut "0000000000000000".to_string()),
            "00000000 00000000"
        );
        assert_eq!(
            format_opcodes(&mut "0000".to_string()),
            "0000              "
        );
        assert_eq!(
            format_opcodes(&mut "0123456789ABCDEF".to_string()),
            "01234567 89ABCDEF"
        );
        assert_eq!(
            format_opcodes(&mut "12345678".to_string()),
            "12345678         "
        );
        assert_eq!(format_opcodes(&mut "123".to_string()), "123");
    }

    #[test]
    // Test for missing file
    fn test_read_file_to_vec1() {
        let mut msg_list = MsgList::new();
        let mut opened_files: Vec<String> = Vec::new();
        read_file_to_vector("////xxxxxxx", &mut msg_list, &mut opened_files);
        assert_eq!(msg_list.list[0].name, "Unable to open file ////xxxxxxx");
    }

    #[test]
    // Test for simple file added
    fn test_write_file_to_vec2() {
        let mut msg_list = MsgList::new();
        let mut opened_files: Vec<String> = Vec::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap_or_default();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        let _ = writeln!(tmp_file1, "Test line in file");
        let _ = writeln!(tmp_file1, "Test line in file 1");
        let _ = writeln!(tmp_file1, "Test line in file 2");
        let _ = writeln!(tmp_file1, "Test line in file 4");

        let lines = read_file_to_vector(file_name1, &mut msg_list, &mut opened_files);
        assert_eq!(lines.clone().unwrap()[0].input, "Test line in file");
        assert_eq!(lines.unwrap().len(), 4);

        drop(tmp_file1);
        let _ = tmp_dir.close();
    }

    #[test]
    // Test for included file
    fn test_write_file_to_vec3() {
        let mut msg_list = MsgList::new();
        let mut opened_files: Vec<String> = Vec::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        let _ = writeln!(tmp_file1, "Test line in file 1 line 0");
        let _ = writeln!(tmp_file1, "!include test2.kla");
        let _ = writeln!(tmp_file1, "Test line in file 1 line 1");
        let _ = writeln!(tmp_file1, "Test line in file 1 line 2");

        let file_path2 = tmp_dir.path().join("test2.kla");
        let mut tmp_file2: File = File::create(file_path2).unwrap();
        let _ = writeln!(tmp_file2, "Test line in file 2 line 0");
        let _ = writeln!(tmp_file2, "Test line in file 2 line 1");
        let _ = writeln!(tmp_file2, "Test line in file 2 line 2");
        let _ = writeln!(tmp_file2, "Test line in file 2 line 4");

        let lines = read_file_to_vector(file_name1, &mut msg_list, &mut opened_files);
        assert_eq!(
            lines.clone().unwrap_or_default()[0].input,
            "Test line in file 1 line 0"
        );
        assert_eq!(
            lines.clone().unwrap()[2].input,
            "Test line in file 2 line 1"
        );
        assert_eq!(
            lines.clone().unwrap()[6].input,
            "Test line in file 1 line 2"
        );
        assert_eq!(lines.unwrap().len(), 7);

        drop(tmp_file1);
        drop(tmp_file2);
        let _ = tmp_dir.close();
    }

    #[test]
    // Test for recursive file includes
    fn test_write_file_to_vec4() {
        let mut msg_list = MsgList::new();
        let mut opened_files: Vec<String> = Vec::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        let _ = writeln!(tmp_file1, "Test line in file 1 line 0");
        let _ = writeln!(tmp_file1, "!include test2.kla");
        let _ = writeln!(tmp_file1, "Test line in file 1 line 1");
        let _ = writeln!(tmp_file1, "Test line in file 1 line 2");

        let file_path2 = tmp_dir.path().join("test2.kla");
        let mut tmp_file2: File = File::create(file_path2).unwrap();
        let _ = writeln!(tmp_file2, "Test line in file 2 line 0");
        let _ = writeln!(tmp_file2, "Test line in file 2 line 1");
        let _ = writeln!(tmp_file2, "!include test1.kla");
        let _ = writeln!(tmp_file2, "Test line in file 2 line 4");

        let _lines = read_file_to_vector(file_name1, &mut msg_list, &mut opened_files);
        assert_eq!(
            msg_list.list[0].name,
            format!("Recursive include of file {file_name1}")
        );

        drop(tmp_file1);
        drop(tmp_file2);
        let _ = tmp_dir.close();
    }

    #[test]
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
        let _ = writeln!(tmp_file1, "!include test2.kla");
        let _ = writeln!(tmp_file1, "Test line in file 1 line 1");
        let _ = writeln!(tmp_file1, "Test line in file 1 line 2");

        let lines = read_file_to_vector(file_name1, &mut msg_list, &mut opened_files);
        assert_eq!(
            msg_list.list[0].name,
            format!("Unable to open file {file_name2}")
        );
        assert_eq!(
            msg_list.list[1].name,
            format!("Unable to open include file {file_name2} in {file_name1}")
        );
        assert_eq!(lines, Some(vec![]));

        drop(tmp_file1);
        let _ = tmp_dir.close();
    }
    #[test]
    // Test for missing included file name
    fn test_write_file_to_vec6() {
        let mut msg_list = MsgList::new();
        let mut opened_files: Vec<String> = Vec::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        let _ = writeln!(tmp_file1, "!include");
        let _ = writeln!(tmp_file1, "Test line in file 1 line 1");
        let _ = writeln!(tmp_file1, "Test line in file 1 line 2");

        let lines = read_file_to_vector(file_name1, &mut msg_list, &mut opened_files);
        assert_eq!(
            msg_list.list[0].name,
            format!("Missing include file name in {file_name1}")
        );
        assert_eq!(lines, None);

        drop(tmp_file1);
        let _ = tmp_dir.close();
    }

    #[test]
    // Test for double included file
    fn test_write_file_to_vec7() {
        let mut msg_list = MsgList::new();
        let mut opened_files: Vec<String> = Vec::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        let _ = writeln!(tmp_file1, "Test line in file 1 line 0");
        let _ = writeln!(tmp_file1, "!include test2.kla");
        let _ = writeln!(tmp_file1, "Test line in file 1 line 1");
        let _ = writeln!(tmp_file1, "Test line in file 1 line 2");

        let file_path2 = tmp_dir.path().join("test2.kla");
        let mut tmp_file2: File = File::create(file_path2.clone()).unwrap();
        let binding2 = file_path2;
        let file_name2: &str = binding2.to_str().unwrap();
        let _ = writeln!(tmp_file2, "Test line in file 2 line 0");
        let _ = writeln!(tmp_file2, "!include test3.kla");
        let _ = writeln!(tmp_file2, "Test line in file 2 line 2");
        let _ = writeln!(tmp_file2, "Test line in file 2 line 4");

        let file_path3 = tmp_dir.path().join("test3.kla");
        let binding3 = file_path3;
        let file_name3: &str = binding3.to_str().unwrap();

        let _lines = read_file_to_vector(file_name1, &mut msg_list, &mut opened_files);
        assert_eq!(
            msg_list.list[0].name,
            format!("Unable to open file {file_name3}")
        );
        assert_eq!(
            msg_list.list[1].name,
            format!("Unable to open include file {file_name3} in {file_name2}")
        );

        drop(tmp_file1);
        drop(tmp_file2);
        let _ = tmp_dir.close();
    }
}
