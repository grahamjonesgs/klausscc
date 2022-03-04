//use crate::messages;
use crate::files;

// Check if end of first word is colon
pub fn is_label (line: &String) -> bool {
    let words=line.split_whitespace();
    for (i,word)  in words.enumerate() {
        //println!("Word {} is {}",i,word);
        if i==0 && word.ends_with(":") {return true}
    }
    false
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
    

   
