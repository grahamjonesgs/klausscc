//use crate::messages;
use crate::files::*;
use crate::messages::*;

// Check if end of first word is colon if so return label
pub fn return_label (line: &String) -> Option<String> {
    let words=line.split_whitespace();
    for (i,word)  in words.enumerate() {
        //println!("Word {} is {}",i,word);
        if i==0 && word.ends_with(":") {return Some(word.to_string())}
    }
    None
}

// Return option of progam counter for label if it exists. 
pub fn return_label_value(line: &String,labels: &mut Vec<Label>) -> Option<u32> {
    for label in labels {
        if label.code==line.as_str() {return Some(label.program_counter)}
    }
    None
}

// Checks if first word is opcode and if so returns opcode hex value
pub fn is_opcode (opcodes: &mut Vec<Opcode>,line: &mut String) -> Option<String> {  
    for opcode in opcodes {
        let words=line.split_whitespace();
        for (i,word)  in words.enumerate() {
            if i==0 && word.to_uppercase()==opcode.name {return Some(opcode.opcode.to_string().to_uppercase())}
        }
    }
    None
}
  
// Returns option of number of arguments for opcode
pub fn num_arguments (opcodes: & mut Vec<Opcode>,line: &mut String) -> Option<u32> { 
    for opcode in opcodes {
        let words=line.split_whitespace();
        for (i,word)  in words.enumerate() {
            if i==0 && word==opcode.name {return Some(opcode.variables)}
        }
    }
    None
}

// Returns option of number of registers for opcode
pub fn num_registers (opcodes: & mut Vec<Opcode>,line: &mut String) -> Option<u32> {
    for opcode in opcodes {
        let words=line.split_whitespace();
        for (i,word)  in words.enumerate() {
            if i==0 && word==opcode.name {return Some(opcode.registers)}
        }
    }
    None
}
 
// Returns emum of type of line
pub fn line_type (opcodes: & mut Vec<Opcode>,line: &mut String) -> LineType {  
    if return_label(line).is_some() {return LineType::Label};
    if is_opcode(opcodes, line).is_some() {return LineType::Opcode}
    if is_blank(line) {return LineType::Blank}
    let words=line.split_whitespace();
        for (i,word)  in words.enumerate() {
            if is_comment(&mut word.to_string()) == true && i==0 {return LineType::Comment}
        } 
    LineType::Error
} 

//Returns true if line is not error
pub fn is_valid_line (opcodes: & mut Vec<Opcode>,line: &mut String) -> bool {   
    if line_type(opcodes, line) == LineType::Error {return false}
    true
}

// Returns true if line if just whitespace
pub fn is_blank (line: &mut String) -> bool {
    let words=line.split_whitespace();

    for (_i,word)  in words.enumerate() {
        if word.len()>0 {return false}
        } 
    true
}

// Returns true is line is just comment
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

// map the reigter to the hex code for the opcode
pub fn map_reg_to_hex(input: String) -> String {
    match input.to_uppercase().as_str() {
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

pub fn is_valid_hex(input: &mut String) -> bool {
    for char in input.to_uppercase().chars(){
        if char<'0' {return false};
        if char>'F' {return false};
        if char>'9' && char <'A' {return false};
    }
    true
}

// Returns the hex code operand from the line, adding regiter values
pub fn add_registers (opcodes: & mut Vec<Opcode>,line: &mut String,msg_list: &mut Vec<Message>,line_number: u32) -> String {
    let num_registers=num_registers(opcodes, line).unwrap_or(0);
    //println!("Num reg {}",num_registers);
     
    let mut opcode_found=is_opcode(opcodes, &mut line.to_uppercase()).unwrap_or("xxxx".to_string());
    opcode_found=opcode_found[..(4-num_registers) as usize].to_string();
    let words=line.split_whitespace();
    for (i,word)  in words.enumerate() {
        if (i==2 && num_registers==2) || (i==1 && (num_registers==2||num_registers==1))
            {opcode_found=opcode_found+&map_reg_to_hex(word.to_string())}
    } 

    if opcode_found.len()!=4 || opcode_found.find("X").is_some(){
        let msg_line = format!("Incorrect register defintion - line {}, \"{}\"",line_number,line);
        msg_list.push(Message {
            name: msg_line.clone(),
            line_number: line_number,
            level: MessageType::Warning,
        });
        
    }
    opcode_found
}
// Returns the hex code argument from the line
pub fn add_arguments (opcodes: & mut Vec<Opcode>,line: &mut String,msg_list: &mut Vec<Message>,line_number: u32,labels: &mut Vec<Label>) -> String {
    let num_registers=num_registers(opcodes, line).unwrap_or(0);
    let num_arguments=num_arguments(opcodes, line).unwrap_or(0);
    let mut arguments="".to_string();

    let words=line.split_whitespace();
    for (i,word)  in words.enumerate() {
        if i==num_registers as usize + 1 && num_arguments==1
            {arguments=arguments+&convert_argument(word.to_string().to_uppercase(),msg_list,line_number,labels).unwrap_or("".to_string())}
        if i==num_registers as usize + 2 && num_arguments==2
            {arguments=arguments+&convert_argument(word.to_string().to_uppercase(),msg_list,line_number,labels).unwrap_or("".to_string())}
    } 

    if arguments.len()!=4*num_arguments as usize {
        let msg_line = format!("Incorrect argument defintion - line {}, \"{}\"",line_number,line);
        msg_list.push(Message {
            name: msg_line.clone(),
            line_number: line_number,
            level: MessageType::Error,
        });
    }
    arguments
}

// Converts argument to label value or converts to Hex
pub fn convert_argument(argument: String,msg_list: &mut Vec<Message>,line_number: u32,labels: &mut Vec<Label>) -> Option<String> {
    
    if return_label(&argument).is_some() {
        match return_label_value(&argument, labels) {
            Some(n) => return Some(format!("{:04X}",n)),
            None => {
                let msg_line = format!("Label {} not found - line {}",argument,line_number);
                msg_list.push(Message {
                    name: msg_line.clone(),
                    line_number: line_number,
                    level: MessageType::Warning,
                });
                return None},
        };
    }
    
    if argument.len()==6 {
        let _temp=argument[0..2].to_string();
        if &argument[0..2]=="0x" || &argument[0..2]=="0X"{
            if is_valid_hex(&mut argument[2..].to_string()) {
                return Some(argument[2..].to_string().to_uppercase()) }  // was hex so return
            else {
                return None
            }
        }
    }

    match argument.parse::<i32>() {
        Ok(n) => if n<=65535 
                    {return Some(format!("{:04X}",n).to_string())}
                    else
                    {let msg_line = format!("Decimal value out {} of bounds",n);
                    msg_list.push(Message {
                        name: msg_line.clone(),
                        line_number: line_number,
                        level: MessageType::Warning,
                    });
                    return None},
        Err(_e) => return None,
      };
}



   
