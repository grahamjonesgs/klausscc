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
    pub PC: u32,
}

fn main() {
    let mut msg_list = Vec::new();
    let new_message = Message {
        name: String::from("Test Task"),
        number: 3,
        level: messages::MessageType::Error,
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
    //println!("The file passed is: {}", myfile);

    let mut oplist = parse_opcodes("/Users/graham/Documents/src/rust/opttest/src/opcode_select.vh");
    //println!("Finished, the 10th one is {:?}",oplist[10]);
    
    let input_file = read_file_to_vec(&mut msg_list,"/Users/graham/Documents/src/rust/opttest/src/jmptest.kla");
    
    let result2: Vec<Pass1> = input_file.iter()
                                
                                .map(|n| Pass1 {
                                    input: n.to_string(),
                                    PC: 0
                                })
                                .collect();
    //println!("Finished {:?}",result2);
    
    let mut pass1a: Vec<Pass1>= Vec::new();
    let input_file2=input_file.clone();

    let mut program_counter: u32=0;
    for mut code_line in input_file2 {
        pass1a.push( Pass1 {
                    input: code_line.clone(),
                    PC: program_counter});
        let num_args = num_operands(&mut oplist,&mut code_line);
        match num_args {
            Some(p) => {program_counter=program_counter+p+1}
            None => {}
        }
        //println!("Checking {}, opcode result is {:?}", code_line.clone(),is_opcode(&mut oplist,&mut code_line).unwrap_or("None".to_string()));
        println!("Checking {}, opcode result is {:?}", code_line.clone(),num_operands(&mut oplist,&mut code_line));
        
    }
  
    //println!("Finished {:?}",pass1a);

    // Test code to find labels
    let result3: Vec<String> = input_file.iter()
                                    .filter(|n| is_label(n.clone()))
                                    .map(|n| ("Label - ".to_string() + n).to_string())
                                    .collect();

    //println!("Finished {:?}",result3);
    //println!("Messages are: {:?}",msg_list);
    //println!("Number of errors is {}, number of warning is {}",
    //        number_errors(&mut msg_list),
    //        number_warnings(&mut msg_list));

}