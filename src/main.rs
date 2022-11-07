mod files;
mod helper;
mod messages;
use chrono::{Local, NaiveTime};
use clap::{Arg, Command};
use files::*;
use helper::*;
use messages::*;
use std::fs;

fn main() { 
    let mut msg_list: MsgList = MsgList::new();
    let start_time: NaiveTime = Local::now().time();

    let matches = Command::new("Klauss Assembler")
        .version("0.0.1")
        .author("Graham Jones")
        .about("Assembler for FPGA_CPU")
        .arg(
            Arg::new("opcode_file")
                .short('c')
                .long("opcode")
                .num_args(1)
                .required(true)
                .help("Opcode source file from VHDL"),
        )
        .arg(
            Arg::new("input")
                .short('i')
                .long("input")
                .required(true)
                .num_args(1)
                .help("Input file to be assembled"),
        )
        .arg(
            Arg::new("output")
                .short('o')
                .long("output")
                .num_args(1)
                .help("Output info file fomr assembled code"),
        )
        .arg(
            Arg::new("bitcode")
                .short('b')
                .long("bitcode")
                .num_args(1)
                .help("Output bitcode file fomr assembled code"),
        )
        .arg(
            Arg::new("verbose")
                .short('v')
                .long("verbose")
                .num_args(0)
                .help("Set if verbose"),
        )
        .arg(
            Arg::new("serial")
                .short('s')
                .long("serial")
                .num_args(1)
                .help("Serial port for output"),
        )
        .get_matches();

    let opcode_file_name = matches
        .get_one::<String>("opcode_file")
        .unwrap_or(&"opcode_select.vh".to_string())
        .replace(' ', "");
    let input_file_name = matches.get_one::<String>("input").unwrap_or(&"".to_string()).replace(' ', "");
    let binary_file_name = matches
        .get_one::<String>("bitcode")
        .unwrap_or(&filename_stem(&input_file_name))
        .replace(' ', "")
        + ".kbt";
    let output_file_name = matches
        .get_one::<String>("output")
        .unwrap_or(&filename_stem(&input_file_name))
        .replace(' ', "")
        + ".code";
    let ouput_serial_port = matches.get_one::<String>("serial").unwrap_or(&"".to_string()).replace(' ', "");

    // Parse the Opcode file
    let (opt_oplist, opt_macro_list) = parse_vh_file(&opcode_file_name, &mut msg_list);
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
    msg_list.push(
        format!("Input file is {}", input_file_name),
        None,
        MessageType::Info,
    );
    let input_list = read_file_to_vec(&mut msg_list, &input_file_name);
    if input_list.is_none() {
        println!("Unable to open input file {:?}", input_file_name);
        std::process::exit(1);
    }

    msg_list.push("Evaluating macros".to_string(), None, MessageType::Info);

    // Pass 0 to add macros
    let mut pass0: Vec<Pass0> = Vec::new();
    let mut input_line_count: u32 = 1;
    for code_line in input_list.unwrap() {
        if macro_name_from_string(&code_line).is_some() {
            let items = return_macro_items_replace(
                code_line.trim(),
                &mut macro_list,
                input_line_count,
                &mut msg_list,
            );
            if items.is_some() {
                for item in Option::unwrap(items) {
                    pass0.push(Pass0 {
                        input: item
                            + &{
                                let this = macro_name_from_string(&code_line);
                                let default = r#""#.to_string();
                                match this {
                                    Some(x) => x,
                                    None => default,
                                }
                            }
                            .to_string(),
                        line_counter: input_line_count,
                    });
                }
            } else {
                msg_list.push(
                    format!("Macro not found {}", code_line),
                    None,
                    MessageType::Error,
                );
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
        input_line_count += 1;
    }

    let mut pass1: Vec<Pass1> = Vec::new();
    let mut program_counter: u32 = 0;

    msg_list.push("Pass 1".to_string(), None, MessageType::Info);

    for mut pass in pass0 {
        pass1.push(Pass1 {
            input: pass.input.to_string(),
            line_counter: pass.line_counter,
            program_counter,
            line_type: line_type(&mut oplist, &mut pass.input),
        });
        if !is_valid_line(&mut oplist, strip_comments(&mut pass.input)) {
            msg_list.push(
                format!("Opcode error {}", pass.input),
                Some(pass.line_counter),
                MessageType::Error,
            );
        }
        if line_type(&mut oplist, &mut pass.input) == LineType::Opcode {
            let num_args = num_arguments(&mut oplist, &mut strip_comments(&mut pass.input));
            if let Some(p) = num_args { program_counter = program_counter + p + 1 }
            
            //match num_args {
            //    Some(p) => program_counter = program_counter + p + 1,
            //    None => {}
            //}
        }

        if line_type(&mut oplist, &mut pass.input) == LineType::Data {
            program_counter += num_data_bytes(&pass.input,&mut msg_list,input_line_count)/8;
        }
    }

    let mut labels: Vec<Label> = pass1
        .iter()
        .filter(|n| {
            label_name_from_string(&n.input).is_some() || data_name_from_string(&n.input).is_some()
        })
        .map(|n| -> Label {
            Label {
                program_counter: n.program_counter,
                name: {
                    let this = label_name_from_string(&n.input);
                    match this {
                        Some(x) => x,
                        None => {
                            let this = data_name_from_string(&n.input);
                            let default = "".to_string();
                            match this {
                                Some(x) => x,
                                None => default,
                            }
                        }
                    }
                },
                line_counter: n.line_counter,
            }
        })
        .collect();

    find_duplicate_label(&mut labels, &mut msg_list);

    msg_list.push("Pass 2".to_string(), None, MessageType::Info);
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
        } else if line.line_type == LineType::Data {
            data_as_bytes(line.input.as_str()).unwrap_or_default()
        }
        else {
            "".to_string()
        };



        pass2.push(Pass2 {
            input: line.input,
            line_counter: line.line_counter,
            program_counter: line.program_counter,
            line_type: if new_opcode.contains("ERR") {
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
        MessageType::Info,
    );
    if !output_code(&output_file_name, &mut pass2) {
        println!("Unable to write to code file {:?}", &output_file_name);
        std::process::exit(1);
    }

    let bin_string = create_bin_string(&mut pass2, &mut msg_list);

    if msg_list.number_errors() == 0 {
        msg_list.push(
            format!("Writing binary file to {}", binary_file_name),
            None,
            MessageType::Info,
        );
        if !output_binary(&binary_file_name, &bin_string) {
            msg_list.push(
                format!("Unable to write to bincode file {:?}", &binary_file_name),
                None,
                MessageType::Error,
            );
        }
    } else if let Err(e) = fs::remove_file(&binary_file_name) {
        match e.kind() {
            std::io::ErrorKind::NotFound => (),
            _ => msg_list.push(
                format!("Removing binary file {}, error {}", &binary_file_name, e),
                None,
                MessageType::Info,
            ),
        };
    }

    if !ouput_serial_port.is_empty() {
        if msg_list.number_errors() == 0 {
            if write_serial(bin_string, &ouput_serial_port, &mut msg_list) {
                msg_list.push(
                    format!("Wrote to serial port {}", ouput_serial_port),
                    None,
                    MessageType::Info,
                );
            } else {
                msg_list.push(
                    format!("Failed to write to serial port {}", ouput_serial_port),
                    None,
                    MessageType::Error,
                );
            }
        } else {
            msg_list.push(
                "Not writing to serial port due to assembly errors".to_string(),
                None,
                MessageType::Warning,
            );
        }
    }

    print_messages(&mut msg_list);
    let duration = Local::now().time() - start_time;
    let time_taken: f32 =
        duration.num_milliseconds() as f32 / 1000.0 + duration.num_seconds() as f32;

    println!(
        "Completed with {} error{} and {} warning{} in {} seconds",
        msg_list.number_errors(),
        if msg_list.number_errors() == 1 {
            ""
        } else {
            "s"
        },
        msg_list.number_warnings(),
        if msg_list.number_warnings() == 1 {
            ""
        } else {
            "s"
        },
        time_taken,
    );
}
