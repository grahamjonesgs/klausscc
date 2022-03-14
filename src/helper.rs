//use crate::messages;
use crate::files;

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
            if i==0 && word==opcode.name {return Some(word.to_string())}
        }
    }
    None
}
  
pub fn num_operands (opcodes: & mut Vec<files::Opcode>,line: &mut String) -> Option<u32> {
   
    for opcode in opcodes {
        let words=line.split_whitespace();
        for (i,word)  in words.enumerate() {
            if i==0 && word==opcode.name {return Some(opcode.variables)}
        }
    }
    None
}
 
pub fn is_valid_line (opcodes: & mut Vec<files::Opcode>,line: &mut String) -> bool {   
    if is_opcode(opcodes, line).is_some() {return true}
    if return_label(line).is_some() {return true}
    let words=line.split_whitespace();
        for (_i,word)  in words.enumerate() {
            if is_comment(&mut word.to_string()) == true {return true}
        } 
    false
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

   
