use clap::{App, Arg};
mod files;
mod helper;
mod messages;
use crate::MessageType::*;
use files::*;
use helper::*;
use messages::*;
use std::fs;

#[derive(Debug)]
pub struct Pass0 {
    pub input: String,
    pub line_counter: u32,
}

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
    let mut msg_list: MsgList = MsgList::new();

    //let mut msg_list = Vec::new();
    msg_list.push("Starting...".to_string(), None, Info);

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
        .unwrap_or(&filename_stem(&input_file_name))
        .replace(" ", "")
        + ".kbt";
    let output_file_name = matches
        .value_of("output")
        .unwrap_or(&filename_stem(&input_file_name))
        .replace(" ", "")
        + ".code";

    // Parse the Opcode file
    msg_list.push(format!("Opcode file is {}", opcode_file_name), None, Info);
    let (opt_oplist, opt_macro_list) = parse_vh_file(&opcode_file_name);
    if opt_oplist.is_none() {
        println!("Unable to open opcode file {:?}", opcode_file_name);
        std::process::exit(1);
    }

    if opt_macro_list.is_none() || opt_oplist.is_none() {
        println!(
            "Error parsing opcode file {} to marco and opcode lists",
            opcode_file_name
        );
        std::process::exit(1);
    }
    let mut oplist = opt_oplist.unwrap();
    let mut macro_list = expand_macros_multi(opt_macro_list.unwrap(), &mut msg_list);

    // Parse the input file
    msg_list.push(format!("Input file is {}", input_file_name), None, Info);
    let input_list = read_file_to_vec(&mut msg_list, &input_file_name);
    if input_list.is_none() {
        println!("Unable to open input file {:?}", input_file_name);
        std::process::exit(1);
    }

    msg_list.push(format!("Starting pass 0"), None, Info);

    // Pass 0 to add macros
    let mut pass0: Vec<Pass0> = Vec::new();
    let mut input_line_count: u32 = 1;
    for code_line in input_list.unwrap() {
        if return_macro(&code_line).is_some() {
            let items = return_macro_items_replace(
                &code_line.trim().to_string(),
                &mut macro_list,
                input_line_count,
                &mut msg_list,
            );
            if items.is_some() {
                for item in items.unwrap() {
                    pass0.push(Pass0 {
                        input: item
                            + " //-- From macro "
                            + &return_macro(&code_line)
                                .unwrap_or("".to_string())
                                .to_string(),
                        line_counter: input_line_count,
                    });
                }
            } else {
                msg_list.push(format!("Macro not found {}", code_line), None, Error);
                pass0.push(Pass0 {
                    input: code_line,
                    line_counter: input_line_count,
                })
            }
        } else {
            pass0.push(Pass0 {
                input: code_line,
                line_counter: input_line_count,
            });
        }
        input_line_count = input_line_count + 1;
    }

    let mut pass1: Vec<Pass1> = Vec::new();
    let mut program_counter: u32 = 0;

    msg_list.push(format!("Starting pass 1"), None, Info);

    for mut pass in pass0 {
        pass1.push(Pass1 {
            input: pass.input.to_string(),
            line_counter: pass.line_counter,
            program_counter: program_counter,
            line_type: line_type(&mut oplist, &mut pass.input),
        });
        if is_valid_line(&mut oplist, strip_comments(&mut pass.input)) == false {
            msg_list.push(
                format!("Opcode error {}", pass.input),
                Some(pass.line_counter),
                Error,
            );
        }
        let num_args = num_arguments(&mut oplist, &mut strip_comments(&mut pass.input));
        match num_args {
            Some(p) => program_counter = program_counter + p + 1,
            None => {}
        }
    }

    msg_list.push(format!("Finding labels"), None, messages::MessageType::Info);

    let mut labels: Vec<Label> = pass1
        .iter()
        .filter(|n| return_label(&n.input).is_some())
        .map(|n| -> Label {
            Label {
                program_counter: n.program_counter,
                code: return_label(&n.input).unwrap_or("".to_string()),
            }
        })
        .collect();

    msg_list.push(format!("Starting pass 2"), None, Info);
    let mut pass2: Vec<Pass2> = Vec::new();
    for line in pass1 {
        let new_opcode = if line.line_type == LineType::Opcode {
            add_registers(
                &mut oplist,
                &mut strip_comments(&mut line.input.clone()),
                &mut msg_list,
                line.line_counter,
            ) + add_arguments(
                &mut oplist,
                &mut strip_comments(&mut line.input.clone()),
                &mut msg_list,
                line.line_counter,
                &mut labels,
            )
            .as_str()
        } else {
            "".to_string()
        };

        pass2.push(Pass2 {
            input: line.input,
            line_counter: line.line_counter,
            program_counter: line.program_counter,
            line_type: if new_opcode.find("ERR").is_some() {
                LineType::Error
            } else {
                line.line_type
            },
            opcode: new_opcode,
        });
    }

    msg_list.push(
        format!("Writing code file to {}", output_file_name),
        None,
        Info,
    );
    if !output_code(&output_file_name, &mut pass2) {
        println!("Unable to write to code file {:?}", &output_file_name);
        std::process::exit(1);
    }

    if msg_list.number_errors() == 0 {
        msg_list.push(
            format!("Writing binary file to {}", binary_file_name),
            None,
            Info,
        );
        if !output_binary(&binary_file_name, &mut pass2) {
            msg_list.push(
                format!("Unable to write to bincode file {:?}", &binary_file_name),
                None,
                Error,
            );
        }
    } else {
        match fs::remove_file(&binary_file_name) {
            Err(e) => {
                match e.kind() {
                    std::io::ErrorKind::NotFound => (),
                    _ => msg_list.push(
                        format!("Removing binary file {}, error {}", &binary_file_name, e),
                        None,
                        Info,
                    ),
                };
            }

            _ => (),
        }
    }

    print_messages(&mut msg_list);
    println!(
        "Number of errors is {}, number of warning is {}",
        msg_list.number_errors(),
        msg_list.number_warnings()
    );
}
