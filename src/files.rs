use crate::{messages, Pass2};

use std::{
    fs::File,
    io::{prelude::*, BufReader},
    path::Path,
    fmt, 
};


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
#[derive(Debug)]
pub struct Label {
    pub program_counter: u32,
    pub code: String,
}

#[derive(Debug)]
#[derive(PartialEq)]
#[derive(Clone)]
pub enum LineType {
    Comment,
    Blank,
    Label,
    Opcode,
    Error,
}

impl fmt::Display for Opcode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} {}, regs {}, vars {} - {}", 
        self.name,self.opcode, self.registers,self.variables,self.comment)
    }
}

// Receive a line from the opcode definition file and if possible 
// parse to instance of Some(Opcode), or None
pub fn opcode_from_string(input_line: &str) -> Option<Opcode> {
    let pos_opcode: usize;
    let pos_name: usize;
    let pos_end_name: usize;
    let pos_comment: usize;
    let pos_end_comment: usize;
    let num_variables: u32;
    let mut num_registers: u32;

    // Find the opcode if it exists
    match input_line.find("16'h") {
        None => return None,
        Some(a) => pos_opcode = a + 4,
    }
    // Check for lenght of opcode
    if input_line.len() < (pos_opcode + 4) {
        return None;
    }

    // Define number of registers from opcode definition
    num_registers = 0;
    if &input_line[pos_opcode + 3..pos_opcode + 4] == "?" {
        num_registers = 1
    }
    if &input_line[pos_opcode + 2..pos_opcode + 4] == "??" {
        num_registers = 2
    }

    // Look for variable, and set flag
    if input_line.find("w_var1") == None {
        num_variables = 0;
    } else {
        num_variables = 1;
    }

    // Look for comment as first word is opcode name
    match input_line.find("//") {
        None => return None,
        Some(a) => pos_name = a + 3,
    }

    // Find end of first word after comment as end of opcode name
    match input_line[pos_name..].find(" ") {
        None => return None,
        Some(a) => pos_end_name = a + pos_name,
    }

    // Set comments filed, or none if missing
    if input_line.len() > pos_end_name + 1 {
        pos_comment = pos_end_name + 1;
        pos_end_comment = input_line.len();
    } else {
        pos_comment = 0;
        pos_end_comment = 0;
    }
 
    // Create opcode
    let opcode = Opcode {
        opcode: input_line[pos_opcode..pos_opcode + 4].to_string(),
        registers: num_registers,
        variables: num_variables,
        comment: input_line[pos_comment..pos_end_comment].to_string(),
        name: input_line[pos_name..pos_end_name].to_string(),
    };

    Some(opcode)
}

// Parse given filename to Vec of Opcode.
pub fn parse_opcodes(filename: impl AsRef<Path>) -> Option<Vec<Opcode>> {
    let file = File::open(filename);
    if file.is_err() {return None}

    let buf = BufReader::new(file.unwrap());
    let mut opcodes: Vec<Opcode> = Vec::new();

    for line in buf.lines() {
        match line {
            Ok(v) => match opcode_from_string(&v) {
                None => (),
                Some(a) => opcodes.push(a),
            },

            Err(e) => println!("Failed parsing opcode file: {:?}", e),
        }
    }
    Some(opcodes)
} 

pub fn read_file_to_vec(msgs: &mut Vec<messages::Message>,filename: impl AsRef<Path>) -> Option<Vec<String>> {
    //let file = File::open(filename).expect("No such input file");
    let file = File::open(filename);
    if file.is_err() {return None}
   
    let buf = BufReader::new(file.unwrap());
    let mut lines: Vec<String> = Vec::new();

    messages::add_message("Starting opcode import", messages::MessageType::Info,2,msgs);

    for line in buf.lines() {
        match line {
            Ok(v) => lines.push(v),

            Err(e) => println!("Error parsing opcode file: {:?}", e),
        }
    }
    Some(lines)
}

pub fn filename_stem (full_name: String) -> String {
    let dot_pos=full_name.find(".");
    if dot_pos.is_none() {return  full_name;}
    full_name[..dot_pos.unwrap_or(0)].to_string()
}

pub fn output_binary (filename: impl AsRef<Path>, pass2: &mut Vec<Pass2>) -> bool {
    let rfile = File::create(filename);
    if rfile.is_err() {return false}

    let mut file=rfile.unwrap();
    if file.write(b"S").is_err() {return false};
    for pass in pass2 {
        if file.write(pass.opcode.as_bytes()).is_err() {return false};
    }
    if file.write(b"X").is_err() {return false};

    true
}

pub fn output_code (filename: impl AsRef<Path>, pass2: &mut Vec<Pass2>) -> bool {
    let rfile = File::create(filename);
    if rfile.is_err() {return false}
    let mut out_line: String;
    let mut file=rfile.unwrap();
 
    for pass in pass2 {
        if pass.line_type==LineType::Opcode {
            out_line = format!("0x{:08X}: {:<8} -- {}\n",pass.program_counter,split_opcodes(&mut pass.opcode),pass.input);}
        else  if pass.line_type==LineType::Error  {
            out_line=format!("Error                      -- {}\n",pass.input);}
        else {
            out_line=format!("                           -- {}\n",pass.input);} 
        if file.write(&out_line.as_bytes()).is_err() {return false};
    }
    true
}

pub fn split_opcodes (input: &mut String) -> String {
    if input.len()==4 {return input.to_string()+"          ";}
    if input.len()==8 {return input.clone()[0..4].to_string()+" "+&input[4..8].to_string()+"     ";}
    if input.len()==12 {return input.clone()[0..4].to_string()+" "+&input[4..8].to_string()+" "+&input[8..12].to_string();}
    input.to_string()
}



