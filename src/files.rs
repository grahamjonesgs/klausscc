use std::{
    fs::File,
    io::{prelude::*, BufReader},
    path::Path,
};

struct Opcode {
    name: String,
    opcode: String,
    registers: u32,
    variables: u32,
    comment: String
}

pub fn lines_from_file(filename: impl AsRef<Path>) -> Vec<String> {
    let file = File::open(filename).expect("no such file");
    let buf = BufReader::new(file);
    buf.lines()
        .map(|l| l.expect("Could not parse line"))
        .collect()
}

pub fn parse_opcodes(filename: impl AsRef<Path>) -> Vec<Opcode> {
    let file = File::open(filename).expect("no such file");
    let buf = BufReader::new(file);
    let mut opcodes : Vec<Opcode>  = Vec::new();
    let mut opcode : Opcode;
    opcode.registers=3;
    opcode.registers=4;

    for line in buf.lines() {
        match line {
            Ok(v) => opcode.registers = 7,
            Err(e) => println!("error parsing header: {:?}", e),
        }
        opcodes.push(opcode);
    }
    opcodes
    
}