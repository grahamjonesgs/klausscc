use crate::{messages::{MessageType, MsgList}};
use crate::macros::{Macro, return_macro};
use crate::helper::trim_newline;
use crate::opcodes::{Opcode, return_opcode};

use core::fmt::Write as _;
use core::fmt;
use std::{
    fs::File,
    io::{prelude::*, BufReader},
    path::Path,
};
use itertools::Itertools;


pub struct Pass0 {
    pub input: String,
    pub line_counter: u32,
}

pub struct Pass1 {
    pub input: String,
    pub line_counter: u32,
    pub program_counter: u32,
    pub line_type: LineType,
}

pub struct Pass2 {
    pub input: String,
    pub line_counter: u32,
    pub program_counter: u32,
    pub line_type: LineType,
    pub opcode: String,
}

#[derive(PartialEq,Debug)]
pub enum LineType {
    Comment,
    Blank,
    Label,
    Opcode,
    Data,
    Error,
}

impl fmt::Display for Opcode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{} {}, registers {}, variables {} - {}",
            self.name, self.opcode, self.registers, self.variables, self.comment
        )
    }
}

/// Parse opcode definition line to opcode
///
/// Receive a line from the opcode definition file and if possible parse of Some(Opcode), or None
pub fn opcode_from_string(input_line: &str) -> Option<Opcode> {
    let pos_comment: usize;
    let pos_end_comment: usize;
    let line_pos_opcode: usize;

    // Find the opcode if it exists
    let pos_opcode: usize = match input_line.find("16'h") {
        None => return None,
        Some(a) => {
            line_pos_opcode = a;
            a + 4
        }
    };

    // check if the line was commented out
    match input_line.find("//") {
        None => {}
        Some(a) => {
            if a < line_pos_opcode {
                return None;
            }
        }
    }

    // Check for length of opcode
    if input_line.len() < (pos_opcode + 4) {
        return None;
    }

    // Define number of registers from opcode definition




    
    let mut num_registers: u32 = 0;
    if &input_line[pos_opcode + 3..pos_opcode + 4] == "?" {
        num_registers = 1;
    }
    if &input_line[pos_opcode + 2..pos_opcode + 4] == "??" {
        num_registers = 2;
    }

    // Look for variable, and set flag
    let num_variables: u32 = u32::from(input_line.contains("w_var1"));

    // Look for comment as first word is opcode name
    let pos_name: usize = match input_line.find("//") {
        None => return None,
        Some(a) => a + 3,
    };

    // Find end of first word after comment as end of opcode name
    let pos_end_name: usize = match input_line[pos_name..].find(' ') {
        None => return None,
        Some(a) => a + pos_name,
    };

    // Set comments filed, or none if missing
    if input_line.len() > pos_end_name + 1 {
        pos_comment = pos_end_name + 1;
        pos_end_comment = input_line.len();
    } else {
        pos_comment = 0;
        pos_end_comment = 0;
    }

    Some(Opcode {
        opcode: format!("0000{}", &input_line[pos_opcode..pos_opcode + 4].to_string()),
        registers: num_registers,
        variables: num_variables,
        comment: input_line[pos_comment..pos_end_comment].to_string(),
        name: input_line[pos_name..pos_end_name].to_string(),
    })
}

/// Parse opcode definition line to macro
///
/// Receive a line from the opcode definition file and if possible parse to instance of Some(Macro), or None
pub fn macro_from_string(input_line: &str, msg_list: &mut MsgList) -> Option<Macro> {
    // Find the macro if it exists
    if input_line.find('$').unwrap_or(usize::MAX) != 0 {
        return None;
    }
    let mut name: String = String::new();
    let mut item: String = String::new();
    let mut items: Vec<String> = Vec::new();
    let mut max_variable: u32 = 0;
    let mut all_found_variables: Vec<i64> = Vec::new();
    let mut all_variables: Vec<i64> = Vec::new();

    let words = input_line.split_whitespace();
    for (i, word) in words.enumerate() {
        if i == 0 {
            name = word.to_string();
        } else if word == "/" {
            items.push(item.to_string());
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
        items.push(item.to_string());
    }

    if max_variable as usize != all_found_variables.clone().into_iter().unique().count() {
        for i in 1..max_variable {
            all_variables.push(i.into());
        }

        // Find the missing variables and create string
        let difference_all_variables: Vec<_> = all_variables
            .into_iter()
            .filter(|item| !all_found_variables.contains(item))
            
            .collect();
        let mut missing: String = String::new();
        for i in difference_all_variables {
            if !missing.is_empty() {
                missing.push(' ');
            }
            //missing.push_str(&format!("%{}", i));
            write!(missing, "%{i}").ok();
        }

        msg_list.push(
            format!(
                "Error in macro variable definition for macro {name}, missing {missing:?}",
            ),
            None,
            MessageType::Warning,
        );
    }

    Some(Macro {
        name,
        variables: max_variable,
        items,
    })
}

/// Parse file to opcode and macro vectors
///
/// Parses the .vh verilog file, creates two vectors of macro and opcode, returning None, None or Some(Opcode), Some(Macro)
pub fn parse_vh_file(
    filename: &impl AsRef<Path>,
    msg_list: &mut MsgList,
) -> (Option<Vec<Opcode>>, Option<Vec<Macro>>) {
    let file = File::open(filename);
    if file.is_err() {
        return (None, None);
    }

    let buf = BufReader::new(file.unwrap());
    let mut opcodes: Vec<Opcode> = Vec::new();
    let mut macros: Vec<Macro> = Vec::new();

    for line in buf.lines() {
        match line {
            Ok(v) => {
                match opcode_from_string(&v) {
                    None => (),
                    Some(a) => {
                        if return_opcode(&a.name, &mut opcodes).is_some() {
                            msg_list.push(
                                format!("Duplicate Opcode {} found", a.name),
                                None,
                                MessageType::Error,
                            );
                        }
                        opcodes.push(a);
                    }
                }
                match macro_from_string(&v, msg_list) {
                    None => (),
                    Some(a) => {
                        if return_macro(&a.name, &mut macros).is_some() {
                            msg_list.push(
                                format!("Duplicate Macro definition {} found", a.name),
                                None,
                                MessageType::Error,
                            );
                        }
                        macros.push(a);
                    }
                }
            }

            Err(e) => println!("Failed parsing opcode file: {e:?}"),
        }
    }
    (Some(opcodes), Some(macros))
}

/// Open text file and return as vector of strings
///
/// Reads any given file by filename, adding the fill line by line into vector and returns None or Some(String)
pub fn read_file_to_vec(filename: &str) -> Option<Vec<String>> {
    let file = File::open(filename);
    if file.is_err() {
        return None;
    }

    let buf = BufReader::new(file.unwrap());
    let mut lines: Vec<String> = Vec::new();

    for line in buf.lines() {
        match line {
            Ok(v) => lines.push(v),

            Err(e) => println!("Error parsing opcode file: {e:?}"),
        }
    }
    Some(lines)
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
                pass.program_counter,
                pass.input
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
        return input[0..4].to_string()
            + &input[4..8]
            + " "
            + &input[8..12]
            + &input[12..16];
    }
    (*input).to_string()
}

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
