use clap::{App, Arg};
mod files;
mod helper;
mod messages;
use files::*;
use helper::*;
use messages::*;

#[derive(Debug)]
pub struct Pass1 {
    pub input: String,
    pub line_counter: u32,
    pub program_counter: u32,
    pub line_type: LineType,
}

#[derive(Debug, Clone)]
pub struct Pass2 {
    pub input: String,
    pub line_counter: u32,
    pub program_counter: u32,
    pub line_type: LineType,
    pub opcode: String,
}

fn main() {
    let raw = "0xf";
    let without_prefix = raw.trim_start_matches("0x");
    let z = i64::from_str_radix(without_prefix, 16);
    println!("{:?}", z);

    let mut msg_list = Vec::new();

    msg_list.push(Message {
        name: String::from("Starting..."),
        line_number: 0,
        level: messages::MessageType::Info,
    });

    let matches = App::new("Klauss Assembler")
        .version("0.0.1")
        .author("Graham Jones")
        .about("Assembler for FPGA_CPU")
        .arg(
            Arg::with_name("opcode_file")
                .short("c")
                .long("opcode")
                .takes_value(true)
                .required(true)
                .help("Opcode source file from VHDL"),
        )
        .arg(
            Arg::with_name("input")
                .short("i")
                .long("input")
                .required(true)
                .takes_value(true)
                .help("Input file to be assembled"),
        )
        .arg(
            Arg::with_name("output")
                .short("o")
                .long("output")
                .takes_value(true)
                .help("Output info file fomr assembled code"),
        )
        .arg(
            Arg::with_name("bitcode")
                .short("b")
                .long("bitcode")
                .takes_value(true)
                .help("Output bitcode file fomr assembled code"),
        )
        .arg(
            Arg::with_name("verbose")
                .short("v")
                .long("verbose")
                .takes_value(false)
                .help("Set if verbose"),
        )
        .get_matches();

    let opcode_file_name = matches
        .value_of("opcode_file")
        .unwrap_or("opcode_select.vh")
        .replace(" ", "");
    let input_file_name = matches.value_of("input").unwrap_or("").replace(" ", "");
    let binary_file_name = matches
        .value_of("bitcode")
        .unwrap_or(&filename_stem(input_file_name.clone()))
        .replace(" ", "")
        + ".kbt";
    let output_file_name = matches
        .value_of("output")
        .unwrap_or(&filename_stem(input_file_name.clone()))
        .replace(" ", "")
        + ".code";

    // Parse the Opcode file
    let opt_oplist = parse_opcodes(opcode_file_name.clone());
    if opt_oplist.is_none() {
        println!("Unable to open opcode file {:?}", opcode_file_name);
        std::process::exit(1);
    }
    let mut oplist = opt_oplist.unwrap();

    // Parse the input file
    let input_list = read_file_to_vec(&mut msg_list, input_file_name.clone());
    if input_list.is_none() {
        println!("Unable to open input file {:?}", input_file_name);
        std::process::exit(1);
    }

    let mut pass1: Vec<Pass1> = Vec::new();
    let mut program_counter: u32 = 0;
    let mut input_line_count: u32 = 0;

    for mut code_line in input_list.unwrap() {
        pass1.push(Pass1 {
            input: code_line.clone(),
            line_counter: input_line_count,
            program_counter: program_counter,
            line_type: line_type(&mut oplist, &mut code_line),
        });
        input_line_count = input_line_count + 1;
        if is_valid_line(&mut oplist, &mut code_line) == false {
            let msg_line = format!("Syntax error found on line {}", code_line);
            msg_list.push(Message {
                name: msg_line.clone(),
                line_number: input_line_count,
                level: messages::MessageType::Error,
            });
        }
        let num_args = num_arguments(&mut oplist, &mut code_line);
        match num_args {
            Some(p) => program_counter = program_counter + p + 1,
            None => {}
        }
    }

    let mut labels: Vec<Label> = pass1
        .iter()
        .filter(|n| return_label(&n.input.clone()).is_some())
        .map(|n| -> Label {
            Label {
                program_counter: n.program_counter,
                code: return_label(&n.input).unwrap_or("".to_string()),
            }
        })
        .collect();

    let mut pass2: Vec<Pass2> = Vec::new();
    for line in pass1 {
        pass2.push(Pass2 {
            input: line.input.clone(),
            line_counter: (line.line_counter),
            program_counter: (line.program_counter),
            line_type: (line.line_type.clone()),
            opcode: (if line.line_type == LineType::Opcode {
                add_registers(
                    &mut oplist,
                    &mut line.input.to_string(),
                    &mut msg_list,
                    line.line_counter,
                ) + add_arguments(
                    &mut oplist,
                    &mut line.input.to_string(),
                    &mut msg_list,
                    line.line_counter,
                    &mut labels,
                )
                .as_str()
            } else {
                "".to_string()
            }),
        });
    }

    print_messages(&mut msg_list);
    println!(
        "Number of errors is {}, number of warning is {}",
        number_errors(&mut msg_list),
        number_warnings(&mut msg_list)
    );

    if !output_code(output_file_name.clone(), &mut pass2) {
        println!("Unable to write to code file {:?}", output_file_name);
        std::process::exit(1);
    }

    if !output_binary(binary_file_name.clone(), &mut pass2) {
        println!("Unable to write to bincode file {:?}", binary_file_name);
        std::process::exit(1);
    }
}
