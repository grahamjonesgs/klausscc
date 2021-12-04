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
    let pos_opcode: u32;
    let pos_name: u32;
    let pos_comment: u32;
    let num_variables: u32;
    let num_registers: u32;

    
    match input_line.find("16'h") {
        None => { return None}
        Some(a) => {pos_opcode=(a as u32)+4}
    }
    match input_line.find("//") {
        None => { return None}
        Some(a) => {pos_name=(a as u32)+3}
    }



    
    match input_line.find("16'h") {
        None => { return None}
        Some(a) => {pos_opcode=a as u32}
    }

   

    let opcode = Opcode {
        opcode: "ccc".to_string(),
        registers: 0,
        variables: 0,
        comment: "ddd".to_string(),
        name: "hello".to_string(),
    };

    Some(opcode)
}

pub fn lines_from_file(filename: impl AsRef<Path>) -> Vec<String> {
    let file = File::open(filename).expect("no such file");
    let buf = BufReader::new(file);
    buf.lines()
        .map(|l| l.expect("Could not parse line"))
        .collect()
}

/*pub fn parse_opcodes(filename: impl AsRef<Path>) -> Vec<Opcode> {
    let file = File::open(filename).expect("no such file");
    let buf = BufReader::new(file);
    let mut opcodes: Vec<Opcode> = Vec::new();

    for line in buf.lines() {
        match line {
            Ok(v) =>
               match v.find("16'h") {
                Some(a) =>  { opcodes.push(Opcode {
                    name: v[a..].to_string(),
                    opcode: "Eric".to_string(),
                    registers: 0,
                    variables: 1,
                    comment: "Comment line".to_string(),
                });

            }
            None => ()
            }


            Err(e) => println!("error parsing header: {:?}", e),
    }
    }
    opcodes
} */
pub fn parse_opcodes(filename: impl AsRef<Path>) -> Vec<Opcode> {
    let file = File::open(filename).expect("no such file");
    let buf = BufReader::new(file);
    let mut opcodes: Vec<Opcode> = Vec::new();

    for line in buf.lines() {
        match line {
            Ok(v) => {
                match opcode_from_string(&v){
                    None => (),
                    Some(a) => {
                        opcodes.push(a)
                    }
                }
            }

            Err(e) => println!("error parsing header: {:?}", e),
        }
    }
    opcodes
}
