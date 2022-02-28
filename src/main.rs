
use clap::{Arg, App};
mod files;
mod helper;
mod messages;
use files::*;
use messages::*;
use helper::is_label;

#[derive(Debug)]
pub enum MessageType {
    Error,
    Warning,
    Info
}

#[derive(Debug)]
pub struct ResultCode {
    pub name: String,
    pub number: u32,
    pub level: MessageType,
}

fn main() {
    let mut msg_list = Vec::new();
    let new_message = Message {
        name: String::from("Test Task"),
        number: 3,
        level: messages::MessageType::Warning,
    };
    msg_list.push(new_message);
    println!("msg is now {:?}",msg_list);
    let matches = App::new("Klauss Assembler")
        .version("0.0.1")
        .author("Graham Jones")
        .about("Assembler for FPGA_CPU")
        .arg(Arg::with_name("file")
                 .short("f")
                 .long("file")
                 .takes_value(true)
                 .help("Opcode source file"))
        .arg(Arg::with_name("num")
                 .short("n")
                 .long("number")
                 .takes_value(true)
                 .help("Dummy number"))
        .get_matches();

    let myfile = matches.value_of("file").unwrap_or("input.txt");
    println!("The file passed is: {}", myfile);

    let oplist = parse_opcodes("/Users/graham/Documents/src/rust/opttest/src/opcode_select.vh");
    println!("Finished {:?}",oplist[10]);
    
    let input_file = read_file_to_vec(&mut msg_list,"/Users/graham/Documents/src/rust/opttest/src/jmptest.kla");
    // println!("Finished {:?}",input_file);

    let result3: Vec<String> = input_file.iter()
                                    .filter(|n| is_label(n))
                                    .map(|n| ("Label - ".to_string() + n).to_string())
                                    .collect();
    println!("Finished {:?}",result3);
    println!("Messages are: {:?}",msg_list);
}