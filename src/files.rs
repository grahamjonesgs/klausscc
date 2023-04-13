use crate::helper::{trim_newline, strip_comments};
use crate::{
    messages::{MessageType, MsgList},
    opcodes::Pass2,
};


use std::{
    fs::File,
    io::{prelude::*, BufReader},
    path::Path,
};

#[derive(PartialEq, Debug)]
pub enum LineType {
    Comment,
    Blank,
    Label,
    Opcode,
    Data,
    Error,
}



/// Open text file and return as vector of strings
///
/// Reads any given file by filename, adding the fill line by line into vector and returns None or Some(String). Manages included files.
pub fn read_file_to_vec(
    filename: &str,
    msg_list: &mut MsgList,
    opened_files: &mut Vec<String>,
) -> Option<Vec<String>> {
    let file = File::open(filename);
    if file.is_err() {
        msg_list.push(
            format!("Unable to open file {filename}"),
            None,
            MessageType::Error,
        );
        return None;
    }

    for file in opened_files.clone() {
        if file == filename {
            msg_list.push(
                format!("Recursive include of file {filename}"),
                None,
                MessageType::Error,
            );
            return None;
        }
    }

    opened_files.push(filename.to_string());

    let buf = BufReader::new(file.unwrap());
    let mut lines: Vec<String> = Vec::new();

    let mut line_number=0;
    for line in buf.lines() {
        match line {
            Ok(v) => {
                line_number+=1;
                if is_include(&v) {
                    let include_file = get_include_filename(&v);
                    if include_file.is_none() {
                        msg_list.push(
                            format!("Unable to parse include file {v} in {filename}"),
                            Some(line_number),
                            MessageType::Error,
                        );
                        return None;
                    }
                    if include_file.clone().unwrap_or(String::new())==String::new() {
                        msg_list.push(
                            format!("Missing include file name in {filename}"),
                            Some(line_number),
                            MessageType::Error,
                        );
                        return None;
                    }
                    let include_file = include_file.unwrap();
                    let include_lines = read_file_to_vec(&include_file, msg_list, opened_files);
                    if include_lines.is_none() {
                        msg_list.push(
                            format!("Unable to open include file {include_file} in {filename}"),
                            Some(line_number),
                            MessageType::Error,
                        );
                        return None;
                    }
                    let include_lines = include_lines.unwrap();
                    for line in include_lines {
                        lines.push(line);
                    }
                } else {
                    lines.push(v);
                }
            }
            Err(e) => println!("Error parsing opcode file: {e:?}"),
        }
    }
    opened_files.pop();
    Some(lines)
}

/// Return true if string is !include
///
/// Checks if the string is include and returns true if it is
pub fn is_include(line: &str) -> bool {
    let line = line.trim();
    if line.starts_with("!include") {
        return true;
    }
    false
}

/// Return the filename from include string
///
/// Returns the filename from include string
pub fn get_include_filename(line: &str) -> Option<String> {
    let line = line.trim();
    if !line.starts_with("!include") {
        return None;
    }
    let mut line = line.replace("!include", "");
    let line=strip_comments(&mut line);
    let mut words = line.split_whitespace();
    Some(words.next().unwrap_or("").to_owned())
}

/// Remove comments from vector of strings
///
/// Checks for /* */ and removes them from the vector of strings
pub fn remove_block_comments(lines: Vec<String>) -> Vec<String> {
    let mut in_comment = false;
    let mut new_lines: Vec<String> = Vec::new();
    for line in lines {
        let mut new_line = String::new();
        let mut in_char = false; // If in normal last was / or if in comment last was *
        for c in line.chars() {
            if in_comment {
                if c == '/' && in_char {
                    in_comment = false;
                } else if c == '*' {
                    in_char = true;
                } else {
                    in_char = false;
                }
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
        new_lines.push(new_line);
    }
    new_lines
}

/// Return the stem of given filename
///
/// Looks for first dot in the string, and returns the slice before the dot
pub fn filename_stem(full_name: &String) -> String {
    let dot_pos = full_name.find('.');
    if dot_pos.is_none() {
        return full_name.to_string();
    }
    full_name[..dot_pos.unwrap_or(0)].to_string()
}

/// Output the bitcode to given file
///
/// Based on the bitcode string outputs to file
pub fn output_binary(filename: &impl AsRef<Path>, output_string: &str) -> bool {
    let result_file = File::create(filename);

    if result_file.is_err() {
        return false;
    }

    let mut file = result_file.unwrap();

    if file.write(output_string.as_bytes()).is_err() {
        return false;
    };

    true
}

/// Output the code details file to given filename
///
/// Writes all data to the detailed code file
pub fn output_code(filename: impl AsRef<Path>, pass2: &mut Vec<Pass2>) -> bool {
    let result_file = File::create(filename);
    if result_file.is_err() {
        return false;
    }
    let mut out_line: String;
    let mut file = result_file.unwrap();

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

/// Format a given string, adding spaces between groups of 4
///
/// For string of 8 and 12 charters adds spaces between groups of 4 characters, otherwise returns original string
pub fn format_opcodes(input: &mut String) -> String {
    if input.len() == 4 {
        return (*input).to_string() + "              ";
    }
    if input.len() == 8 {
        return input[0..4].to_string() + &input[4..8] + "         ";
    }
    if input.len() == 16 {
        return input[0..4].to_string() + &input[4..8] + " " + &input[8..12] + &input[12..16];
    }
    (*input).to_string()
}

/// Output the code details file to given serial port
///
/// Will send the program to the serial port, and wait for the response
#[allow(clippy::cast_possible_wrap)]
pub fn write_serial(binary_output: &str, port_name: &str, msg_list: &mut MsgList) -> bool {
    let mut buffer = [0; 1024];
    let port_result = serialport::new(port_name, 115_200)
        .timeout(std::time::Duration::from_millis(100))
        .open();

    if port_result.is_err() {
        let mut all_ports: String = String::new();
        let ports = serialport::available_ports();

        match ports {
            Err(_) => {
                msg_list.push(
                    "Error opening serial port, no ports found".to_string(),
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
                    -1 => "no ports were found".to_string(),
                    0 => {
                        format!("only port {all_ports} was found")
                    }
                    _ => {
                        format!("the following ports were found {all_ports}")
                    }
                };

                msg_list.push(
                    format!("Error opening serial port {port_name}, {ports_msg}"),
                    None,
                    MessageType::Error,
                );
                return false;
            }
        }
    }

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
            MessageType::Warning,
        );
        return true;
    }

    let ret_msg = String::from_utf8(buffer[..ret_msg_size].to_vec());

    if ret_msg.is_err() {
        msg_list.push(
            "Invalid message received from board".to_string(),
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
        MessageType::Info,
    );

    true
}

#[cfg(test)]
mod test {
    use crate::{
        files::{remove_block_comments, is_include, get_include_filename, filename_stem, format_opcodes},
    };

    #[test]
    // Test remove of commnets in single line
    fn test_remove_block_comments1() {
        let input = vec!["abc/* This is a comment */def".to_string()];
        let output = remove_block_comments(input);
        assert_eq!(output, vec!["abcdef"]);
    }

    #[test]
    // Test remove of commnets in two lines
    fn test_remove_block_comments2() {
        let input = vec![
            "abc/* This is a comment */def".to_string(),
            "abc/* This is a comment */defg".to_string(),
        ];
        let output = remove_block_comments(input);
        assert_eq!(output, vec!["abcdef", "abcdefg"]);
    }

    #[test]
    // Test remove of commnets in across two lines
    fn test_remove_block_comments3() {
        let input = vec![
            "abc/* This is a comment ".to_string(),
            "so is this */defg".to_string(),
        ];
        let output = remove_block_comments(input);
        assert_eq!(output, vec!["abc", "defg"]);
    }

    #[test]
    // Test remove of commnets in across three line with blank line left
    fn test_remove_block_comments4() {
        let input = vec![
            "abc/* This is a comment ".to_string(),
            "so is this defg".to_string(),
            "*/def".to_string(),
        ];
        let output = remove_block_comments(input);
        assert_eq!(output, vec!["abc", "", "def"]);
    }

    #[test]
    // Test restart comments
    fn test_remove_block_comments5() {
        let input = vec![
            "abc/* This is a /* /*comment ".to_string(),
            "so is this defg".to_string(),
            "*/def".to_string(),
        ];
        let output = remove_block_comments(input);
        assert_eq!(output, vec!["abc", "", "def"]);
    }

   #[test]
   // Tests for the is_include to check if there is a !include as fiorst non white-space
   fn test_is_include () {
    assert!(is_include("!include file.type"));
    assert!(!is_include("include file.type"));
    assert!(is_include("!include"));
    assert!(!is_include("!!include"));
    assert!(is_include("    !include   123 234 456"));
   }

   #[test]
   // Checkl funtions returns correct filename from !include
   fn test_get_include_filename() {
    assert_eq!(get_include_filename("!include myfile.name"),Some("myfile.name".to_string()));
    assert_eq!(get_include_filename("testline"),None);
    assert_eq!(get_include_filename("!include myfile.name extra words"),Some("myfile.name".to_string()));
    assert_eq!(get_include_filename("!include"),Some(String::new()));
    assert_eq!(get_include_filename("!include //test comment"),Some(String::new()));
   }

   #[test]
   // Check correct filename stem is returned
   fn test_filename_stem () {
    assert_eq!(filename_stem(&"file.type".to_string()),"file");
    assert_eq!(filename_stem(&"file".to_string()),"file"); 
   }

   #[test]
   // Check for formatting of codes used for debug file
   fn test_format_opcodes () {
    assert_eq!(format_opcodes(&mut "0000000000000000".to_string()),"00000000 00000000");
    assert_eq!(format_opcodes(&mut "0000".to_string()),"0000              ");
    assert_eq!(format_opcodes(&mut "0123456789ABCDEF".to_string()),"01234567 89ABCDEF");
   }

   
    
}
