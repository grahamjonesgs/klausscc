use std::{
    fs::File,
    io::{prelude::*, BufReader},
    path::Path,
};

pub struct Opcode {
    pub name: String,
    pub opcode: String,
    pub registers: u32,
    pub variables: u32,
    pub comment: String,
}

pub fn opcode_from_string(input_line: &str) -> Option<Opcode> {
    let pos_opcode: usize;
    let pos_name: usize;
    let pos_end_name: usize;
    let pos_comment: usize;
    let pos_end_comment: usize;
    let num_variables: u32;
    let mut num_registers: u32;

    match input_line.find("16'h") {
        None => return None,
        Some(a) => pos_opcode = a + 4,
    }

    if input_line.len() < (pos_opcode + 4) {
        return None;
    }

    num_registers = 0;
    if &input_line[pos_opcode + 3..pos_opcode + 4] == "?" {
        num_registers = 1
    }
    if &input_line[pos_opcode + 2..pos_opcode + 4] == "??" {
        num_registers = 2
    }

    if input_line.find("w_var1") == None {
        num_variables = 0;
    } else {
        num_variables = 1;
    }

    match input_line.find("//") {
        None => return None,
        Some(a) => pos_name = a + 3,
    }

    match input_line[pos_name..].find(" ") {
        None => return None,
        Some(a) => pos_end_name = a + pos_name,
    }
    if input_line.len() > pos_end_name + 1 {
        pos_comment = pos_end_name + 1;
        pos_end_comment = input_line.len();
    } else {
        pos_comment = 0;
        pos_end_comment = 0;
    }

    let opcode = Opcode {
        opcode: input_line[pos_opcode..pos_opcode + 4].to_string(),
        registers: num_registers,
        variables: num_variables,
        comment: input_line[pos_comment..pos_end_comment].to_string(),
        name: input_line[pos_name..pos_end_name].to_string(),
    };

    Some(opcode)
}

pub fn parse_opcodes(filename: impl AsRef<Path>) -> Vec<Opcode> {
    let file = File::open(filename).expect("no such file");
    let buf = BufReader::new(file);
    let mut opcodes: Vec<Opcode> = Vec::new();

    for line in buf.lines() {
        match line {
            Ok(v) => match opcode_from_string(&v) {
                None => (),
                Some(a) => opcodes.push(a),
            },

            Err(e) => println!("error parsing opcode file: {:?}", e),
        }
    }
    opcodes
}
