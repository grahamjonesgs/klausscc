use std::default;

//use crate::messages;
use crate::files::{self, LineType};
use crate::messages::*;

// Check if end of first word is colon
pub fn return_label (line: &String) -> Option<String> {
    let words=line.split_whitespace();
    for (i,word)  in words.enumerate() {
        //println!("Word {} is {}",i,word);
        if i==0 && word.ends_with(":") {return Some(word.to_string())}
    }
    None
}

pub fn is_opcode (opcodes: &mut Vec<files::Opcode>,line: &mut String) -> Option<String> {  
    for opcode in opcodes {
        let words=line.split_whitespace();
        for (i,word)  in words.enumerate() {
            if i==0 && word==opcode.name {return Some(opcode.opcode.to_string())}
        }
    }
    None
}
  
pub fn num_arguments (opcodes: & mut Vec<files::Opcode>,line: &mut String) -> Option<u32> { 
    for opcode in opcodes {
        let words=line.split_whitespace();
        for (i,word)  in words.enumerate() {
            if i==0 && word==opcode.name {return Some(opcode.variables)}
        }
    }
    None
}

pub fn num_registers (opcodes: & mut Vec<files::Opcode>,line: &mut String) -> Option<u32> {
    for opcode in opcodes {
        let words=line.split_whitespace();
        for (i,word)  in words.enumerate() {
            if i==0 && word==opcode.name {return Some(opcode.registers)}
        }
    }
    None
}
 
pub fn line_type (opcodes: & mut Vec<files::Opcode>,line: &mut String) -> LineType {  
    if return_label(line).is_some() {return LineType::Label};
    if is_opcode(opcodes, line).is_some() {return LineType::Opcode}
    if is_blank(line) {return LineType::Blank}
    let words=line.split_whitespace();
        for (i,word)  in words.enumerate() {
            if is_comment(&mut word.to_string()) == true && i==0 {return LineType::Comment}
        } 
    LineType::Error
} 

pub fn is_valid_line (opcodes: & mut Vec<files::Opcode>,line: &mut String) -> bool {   
    if line_type(opcodes, line) == LineType::Error {return false}
    true
}

pub fn is_blank (line: &mut String) -> bool {
    let words=line.split_whitespace();

    for (_i,word)  in words.enumerate() {
        if word.len()>0 {return false}
        } 
    true
}

pub fn is_comment(word: &mut String) -> bool {
    if word.len() < 2 {return false}
    let bytes = word.as_bytes();
    let mut found_first=false;

    for (i, &item) in bytes.iter().enumerate() {
        if item == b'/' && i==0 {found_first=true}
        if item == b'/' && i==1 && found_first==true {return true}
    }
    false
}

pub fn return_opcode(opcodes: & mut Vec<files::Opcode>,line: &mut String) -> String {
    let num_operands=num_arguments(opcodes, line).unwrap_or(0);

    "rrr".to_string()
}

// map the reigter to the hex code for the opcode
pub fn map_reg_to_hex(input: String) -> String {
    match input.as_str() {
        "A" => {"0".to_string()}
        "B" => {"1".to_string()}
        "C" => {"2".to_string()}
        "D" => {"3".to_string()}
        "E" => {"4".to_string()}
        "F" => {"5".to_string()}
        "G" => {"6".to_string()}
        "H" => {"7".to_string()}
        "I" => {"8".to_string()}
        "J" => {"9".to_string()}
        "K" => {"A".to_string()}
        "L" => {"B".to_string()}
        "M" => {"C".to_string()}
        "N" => {"D".to_string()}
        "O" => {"E".to_string()}
        "P" => {"F".to_string()}
        _ => {"X".to_string()}
    }
}

// Returns the hex code operand from the line, adding regiter values
pub fn add_registers (opcodes: & mut Vec<files::Opcode>,line: &mut String,msg_list: &mut Vec<Message>,line_number: u32) -> String {
    let num_registers=num_registers(opcodes, line).unwrap_or(0);
    //println!("Num reg {}",num_registers);
    
    let mut opcode_found=is_opcode(opcodes, line).unwrap_or("xxxx".to_string());
    //println!("Opcode is {:?}",opcode_found.clone());
    opcode_found=opcode_found[..(4-num_registers) as usize].to_string();
    //println!("Opcode is now *{}*, length {}",opcode_found,opcode_found.len());
    let words=line.split_whitespace();
    for (i,word)  in words.enumerate() {
        if (i==2 && num_registers==2) || (i==1 && (num_registers==2||num_registers==1))
            {opcode_found=opcode_found+&map_reg_to_hex(word.to_string())}
    } 
    //println!("Opcode is now *{}*, length {}",opcode_found,opcode_found.len());

    if opcode_found.len()!=4 || opcode_found.find("X").is_some(){
        let msg_line = format!("Warning incorrect register defintion - line {}, \"{}\"",line_number,line);
        println!("{}",msg_line);
        msg_list.push(Message {
            name: msg_line.clone(),
            number: 1,
            level: MessageType::Warning,
        });
        
    }
    opcode_found
}
// Returns the hex code argument from the line
pub fn add_arguments (opcodes: & mut Vec<files::Opcode>,line: &mut String,msg_list: &mut Vec<Message>,line_number: u32) -> String {
    let num_registers=num_registers(opcodes, line).unwrap_or(0);
    let num_arguments=num_arguments(opcodes, line).unwrap_or(0);
    let mut arguments="".to_string();

    let words=line.split_whitespace();
    for (i,word)  in words.enumerate() {
        if i==num_registers as usize + 1 && num_arguments==1
            {arguments=arguments+&word.to_string()}
        if i==num_registers as usize + 2 && num_arguments==2
            {arguments=arguments+&word.to_string()}
    } 

    if arguments.len()!=4*num_arguments as usize {
        let msg_line = format!("Warning incorrect argument defintion - line {}, \"{}\"",line_number,line);
        println!("{}",msg_line);
        msg_list.push(Message {
            name: msg_line.clone(),
            number: 1,
            level: MessageType::Warning,
        });
    }
    arguments
}



   
