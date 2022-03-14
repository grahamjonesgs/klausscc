use clap::{Arg, App};
mod files;
mod helper;
mod messages;
use files::*;
use messages::*;
use helper::*;

#[derive(Debug)]
pub struct Pass1 {
    pub input: String,
    pub program_counter: u32,
}

fn main() {
    let mut msg_list = Vec::new();

    msg_list.push(Message {
        name: String::from("Starting..."),
        number: 1,
        level: messages::MessageType::Info,
    });


    //println!("msg is now {:?}",msg_list);
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

    let myfile = matches.value_of("file").unwrap_or("/Users/graham/Documents/src/rust/opttest/src/opcode_select.vh");
   
    // Parse the Opcode file
    let mut oplist = parse_opcodes("/Users/graham/Documents/src/rust/opttest/src/opcode_select.vh");
  
    
    let input_file = read_file_to_vec(&mut msg_list,"/Users/graham/Documents/src/rust/opttest/src/jmptest.kla");
    

    let mut pass1a: Vec<Pass1>= Vec::new();
    let mut program_counter: u32=0;

    for mut code_line in input_file {
        pass1a.push( Pass1 {
                    input: code_line.clone(),
                    program_counter: program_counter});
        let num_args = num_operands(&mut oplist,&mut code_line);
        match num_args {
            Some(p) => {program_counter=program_counter+p+1}
            None => {}
        }
        
    }
    println!("Pass1a is {:?}",pass1a);
  

    // Test code to find labels
   let labels: Vec<Label> = pass1a.iter()//input_file.iter()
                                    .filter(|n| is_label(&n.input.clone()))
                                    .map(|n| -> Label {Label { program_counter: n.program_counter, code: return_label(&n.input) }})
                                    .collect(); 

    println!("Labels are {:?}",labels);



    //println!("Messages are: {:?}",msg_list);
    //println!("Number of errors is {}, number of warning is {}",
    //        number_errors(&mut msg_list),
    //        number_warnings(&mut msg_list));

}