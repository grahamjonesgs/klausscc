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
    pub line_counter: u32,
    pub program_counter: u32,
    pub line_type: LineType,
}

#[derive(Debug)]
pub struct Pass2 {
    pub input: String,
    pub line_counter: u32,
    pub program_counter: u32,
    pub line_type: LineType,
    pub opcode: String,
}

fn main() {
    let mut msg_list = Vec::new();

    msg_list.push(Message {
        name: String::from("Starting..."),
        line_number: 0,
        level: messages::MessageType::Info,
    });

    let _matches = App::new("Klauss Assembler")
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

    //let myfile = matches.value_of("file").unwrap_or("/Users/graham/Documents/src/rust/opttest/src/opcode_select.vh");
   
    // Parse the Opcode file
    let mut oplist = parse_opcodes("src/opcode_select.vh");
  
    
    let input_file = read_file_to_vec(&mut msg_list,"src/jmptest.kla");
    

    let mut pass1: Vec<Pass1>= Vec::new();
    let mut program_counter: u32=0;
    let mut input_line_count: u32=0;

    for mut code_line in input_file.clone() {
        pass1.push( Pass1 {
                    input: code_line.clone(),
                    line_counter: input_line_count,
                    program_counter: program_counter,
                    line_type: line_type(&mut oplist, &mut code_line)});
        input_line_count=input_line_count+1;
        if is_valid_line(&mut oplist, &mut code_line) == false {
            let msg_line = format!("Syntax error found on line {}",code_line);
            msg_list.push(Message {
                name: msg_line.clone(),
                line_number: input_line_count,
                level: messages::MessageType::Error,
            });
        }
        let num_args = num_arguments(&mut oplist,&mut code_line);
        match num_args {
            Some(p) => {program_counter=program_counter+p+1}
            None => {}
        }
        
    }
 
   let mut labels: Vec<Label> = pass1.iter()//input_file.iter()
                                    .filter(|n| return_label(&n.input.clone()).is_some())
                                    .map(|n| -> Label {Label { program_counter: n.program_counter, 
                                        code: return_label(&n.input).unwrap_or("".to_string()) }})
                                    .collect(); 


    let mut pass2: Vec<Pass2>= Vec::new();
    for line  in pass1 {
        pass2.push (Pass2 { 
            input: line.input.clone(),
            line_counter: (line.line_counter),
            program_counter: (line.program_counter),
            line_type: (line.line_type.clone()),
            opcode: (if line.line_type==LineType::Opcode {
                add_registers(&mut oplist, &mut line.input.to_string(),&mut msg_list,line.line_counter)
            + add_arguments(&mut oplist, &mut line.input.to_string(),&mut msg_list,line.line_counter,&mut labels).as_str()}
                else 
                {"".to_string()}) });
       
    }


    print_messages(&mut msg_list);
    println!("Number of errors is {}, number of warning is {}",
            number_errors(&mut msg_list),
            number_warnings(&mut msg_list));

    //for pass in pass2 {
    //    println!("{:04X} {} // {}",pass.program_counter,pass.opcode,pass.input);
    //}

}