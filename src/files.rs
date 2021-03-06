use crate::{helper::return_macro, messages::*, return_opcode, Pass2};

use std::fmt::Write as _;
use std::{
    fmt,
    fs::File,
    io::{prelude::*, BufReader},
    path::Path,
};

use itertools::Itertools;

#[derive(Debug)]
pub struct Opcode {
    pub name: String,
    pub opcode: String,
    pub registers: u32,
    pub variables: u32,
    pub comment: String,
}

#[derive(Debug)]
pub struct CodeLine {
    pub program_counter: u32,
    pub code: String,
}
#[derive(Debug, Clone)]
pub struct Label {
    pub program_counter: u32,
    pub name: String,
    pub line_counter: u32,
}

#[derive(Debug, Clone)]
pub struct Macro {
    pub name: String,
    pub variables: u32,
    pub items: Vec<String>,
}

#[derive(Debug, PartialEq)]
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
            "{} {}, regs {}, vars {} - {}",
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
        num_registers = 1
    }
    if &input_line[pos_opcode + 2..pos_opcode + 4] == "??" {
        num_registers = 2
    }

    // Look for variable, and set flag
    let num_variables: u32 = if input_line.find("w_var1") == None {
        0
    } else {
        1
    };

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
    let mut name: String = "".to_string();
    let mut item: String = "".to_string();
    let mut items: Vec<String> = Vec::new();
    let mut max_variable: i64 = 0;
    let mut all_found_variables: Vec<i64> = Vec::new();
    let mut all_variables: Vec<i64> = Vec::new();

    let words = input_line.split_whitespace();
    for (i, word) in words.enumerate() {
        if i == 0 {
            name = word.to_string();
        } else if word == "/" {
            items.push(item.to_string());
            item = "".to_string();
        } else {
            if word.contains('%') {
                let without_prefix = word.trim_start_matches('%');
                let int_value = without_prefix.parse::<i64>();
                if int_value.clone().is_err() || int_value.clone().unwrap_or(0) < 1 {
                } else {
                    all_found_variables.push(int_value.clone().unwrap_or(0));
                    if int_value.clone().unwrap_or(0) > max_variable as i64 {
                        max_variable = int_value.clone().unwrap_or(0);
                    }
                }
            }

            if !item.is_empty() {
                item = item + " " + word;
            } else {
                item += word;
            }
        }
    }

    if !item.is_empty() {
        items.push(item.to_string());
    }

    if max_variable != all_found_variables.clone().into_iter().unique().count() as i64 {
        for i in 1..max_variable {
            all_variables.push(i);
        }

        // Find the missing variables and create string
        let difference_all_variables: Vec<_> = all_variables
            .into_iter()
            .filter(|item| !all_found_variables.contains(item))
            .clone()
            .collect();
        let mut missing: String = "".to_string();
        for i in difference_all_variables {
            if !missing.is_empty() {
                missing.push(' ');
            }
            //missing.push_str(&format!("%{}", i));
            write!(missing, "%{}", i).ok();
        }

        msg_list.push(
            format!(
                "Error in macro variable definition for macro {}, missing {:?}",
                name, missing,
            ),
            None,
            MessageType::Warning,
        );
    }

    Some(Macro {
        name,
        variables: max_variable as u32,
        items,
    })
}

/// Parse file to opcode and macro vectors
///
/// Parses the .vh verilog file, creates two vectors of macro and opcode, returning None, None or Some<Opcode>, Some<Macro>
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
                        opcodes.push(a)
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
                        macros.push(a)
                    }
                }
            }

            Err(e) => println!("Failed parsing opcode file: {:?}", e),
        }
    }
    (Some(opcodes), Some(macros))
}

/// Open text file and return as vector of strings
///
/// Reads any given file by filename, adding the fill line by line into vector and returns None or Some<String>
pub fn read_file_to_vec(msg_list: &mut MsgList, filename: &str) -> Option<Vec<String>> {
    let file = File::open(filename);
    if file.is_err() {
        return None;
    }

    let buf = BufReader::new(file.unwrap());
    let mut lines: Vec<String> = Vec::new();

    msg_list.push(
        format!("Evaluating opcode file {}", filename),
        None,
        MessageType::Info,
    );

    for line in buf.lines() {
        match line {
            Ok(v) => lines.push(v),

            Err(e) => println!("Error parsing opcode file: {:?}", e),
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
    let rfile = File::create(filename);

    if rfile.is_err() {
        return false;
    }

    let mut file = rfile.unwrap();

    if file.write(output_string.as_bytes()).is_err() {
        return false;
    };

    true
}

/// Output the code details file to given filename
///
/// Writes all data to the detailed code file
pub fn output_code(filename: impl AsRef<Path>, pass2: &mut Vec<Pass2>) -> bool {
    let rfile = File::create(filename);
    if rfile.is_err() {
        return false;
    }
    let mut out_line: String;
    let mut file = rfile.unwrap();

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
/// For string of 8 and 12 charters addes spaces between grouops of 4 characters, otherwise returns original string
pub fn format_opcodes(input: &mut String) -> String {
    if input.len() == 4 {
        return input.to_string() + "              ";
    }
    if input.len() == 8 {
        return input[0..4].to_string() + &input[4..8].to_string() + "         ";
    }
    if input.len() == 16 {
        return input[0..4].to_string()
            + &input[4..8].to_string()
            + " "
            + &input[8..12].to_string()
            + &input[12..16].to_string();
    }
    input.to_string()
}
