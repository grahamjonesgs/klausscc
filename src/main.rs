#![warn(
     clippy::all,
    //clippy::restriction,
      clippy::pedantic,
    //clippy::nursery,
    //clippy::cargo,
)]
#![allow(clippy::single_match_else)]

mod files;
mod helper;
mod labels;
mod macros;
mod messages;
mod opcodes;
use chrono::{Local, NaiveTime};
use clap::{Arg, Command};
use files::{filename_stem, output_binary, output_code, read_file_to_vec, write_serial, LineType};
use helper::{
    create_bin_string, data_as_bytes, is_valid_line, line_type, num_data_bytes, strip_comments,
};
use labels::{find_duplicate_label, get_labels, Label};
use macros::{expand_macros, expand_macros_multi};
use messages::{print_messages, MessageType, MsgList};
use opcodes::{
    add_arguments, add_registers, num_arguments, parse_vh_file, Opcode, Pass0, Pass1, Pass2,
};

use crate::files::remove_block_comments;

/// Main function for Klausscc
///
/// Main funation to read CLI and call other functions
#[cfg(not(tarpaulin_include))] // Cannot test main in tarpaulin
fn main() {
    let mut msg_list: MsgList = MsgList::new();
    let start_time: NaiveTime = Local::now().time();

    let matches = set_matches().get_matches();
    let opcode_file_name = matches
        .get_one::<String>("opcode_file")
        .unwrap_or(&"opcode_select.vh".to_string())
        .replace(' ', "");
    let input_file_name = matches
        .get_one::<String>("input")
        .unwrap_or(&String::new())
        .replace(' ', "");
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
    let output_serial_port = matches
        .get_one::<String>("serial")
        .unwrap_or(&String::new())
        .replace(' ', "");

    // Parse the Opcode file
    let (opt_oplist, opt_macro_list) = parse_vh_file(&opcode_file_name, &mut msg_list);
    if opt_oplist.is_none() {
        println!("Unable to open opcode file {opcode_file_name:?}");
        std::process::exit(1);
    }

    if opt_macro_list.is_none() || opt_oplist.is_none() {
        println!("Error parsing opcode file {opcode_file_name} to marco and opcode lists");
        std::process::exit(1);
    }
    let oplist = opt_oplist.unwrap_or([].to_vec());
    let mut macro_list = expand_macros_multi(opt_macro_list.unwrap(), &mut msg_list);

    // Parse the input file
    msg_list.push(
        format!("Input file is {input_file_name}"),
        None,
        None,
        MessageType::Info,
    );
    let mut opened_files: Vec<String> = Vec::new(); // Used for recursive includes check
    let input_list = read_file_to_vec(&input_file_name, &mut msg_list, &mut opened_files);
    if input_list.is_none() {
        print_messages(&mut msg_list);
        std::process::exit(1);
    }

    // Pass 0 to add macros
    let pass0 = expand_macros(
        &mut msg_list,
        input_list.unwrap(),
        &mut macro_list,
    );

    // Pass 1 to get line numbers and labels
    //msg_list.push("Pass 1".to_string(), None, MessageType::Info);
    let pass1 = get_pass1(&mut msg_list, pass0, oplist.clone());
    let mut labels = get_labels(&pass1);
    find_duplicate_label(&mut labels, &mut msg_list);

    // Pass 2 to get create output
    //msg_list.push("Pass 2".to_string(), None, MessageType::Info);
    let mut pass2 = get_pass2(&mut msg_list, pass1, oplist, labels);

    msg_list.push(
        format!("Writing code file to {output_file_name}"),
        None,
        None,
        MessageType::Info,
    );
    if !output_code(&output_file_name, &mut pass2) {
        println!("Unable to write to code file {:?}", &output_file_name);
        std::process::exit(1);
    }

    let bin_string = create_bin_string(&mut pass2, &mut msg_list);

    if msg_list.number_errors() == 0 {
        write_binary_file(&mut msg_list, &binary_file_name, &bin_string);
    } else if let Err(e) = std::fs::remove_file(&binary_file_name) {
        match e.kind() {
            std::io::ErrorKind::NotFound => (),
            _ => msg_list.push(
                format!("Removing binary file {}, error {}", &binary_file_name, e),
                None,
                None,
                MessageType::Info,
            ),
        };
    }

    if !output_serial_port.is_empty() {
        write_to_device(&mut msg_list, &bin_string, &output_serial_port);
    }

    print_results(&mut msg_list, start_time);
}

/// Manages the CLI
///
/// Uses the Command from Clap to expand the CLI
#[cfg(not(tarpaulin_include))] // Can not test CLI in tarpaulin
#[allow(clippy::must_use_candidate)]
pub fn set_matches() -> Command {
    Command::new("Klauss Assembler")
        .version("0.0.1")
        .author("Graham Jones")
        .about("Assembler for FPGA_CPU")
        .arg(
            Arg::new("opcode_file")
                .short('c')
                .long("opcode")
                .num_args(1)
                .required(true)
                .help("Opcode source file from Verilog"),
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
                .help("Output info file for assembled code"),
        )
        .arg(
            Arg::new("bitcode")
                .short('b')
                .long("bitcode")
                .num_args(1)
                .help("Output bitcode file for assembled code"),
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
}
/// Prints results of assembly
///
/// Takes the message list and start time and prints the results to the users
#[allow(clippy::cast_precision_loss)]
#[cfg(not(tarpaulin_include))] // Cannot test printing in tarpaulin
pub fn print_results(msg_list: &mut MsgList, start_time: NaiveTime) {
    print_messages(msg_list);
    let duration = Local::now().time() - start_time;
    let time_taken: f64 =
        duration.num_milliseconds() as f64 / 1000.0 + duration.num_seconds() as f64;
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

/// Returns pass1 from pass0
///
/// Takes the macro expanded pass0 and returns vector of pass1, with the program counter
pub fn get_pass1(msg_list: &mut MsgList, pass0: Vec<Pass0>, mut oplist: Vec<Opcode>) -> Vec<Pass1> {
    let mut pass1: Vec<Pass1> = Vec::new();
    let mut program_counter: u32 = 0;

    for mut pass in pass0 {
        pass1.push(Pass1 {
            input: pass.input.to_string(),
            file_name: pass.file_name.to_string(),
            line_counter: pass.line_counter,
            program_counter,
            line_type: line_type(&mut oplist, &mut pass.input),
        });
        if !is_valid_line(&mut oplist, strip_comments(&mut pass.input)) {
            msg_list.push(
                format!("Opcode error {}", pass.input),
                Some(pass.line_counter),
                Some(pass.file_name.to_string()),
                MessageType::Error,
            );
        }
        if line_type(&mut oplist, &mut pass.input) == LineType::Opcode {
            let num_args = num_arguments(&mut oplist, &mut strip_comments(&mut pass.input));
            if let Some(p) = num_args {
                program_counter = program_counter + p + 1;
            }
        }

        if line_type(&mut oplist, &mut pass.input) == LineType::Data {
            program_counter += num_data_bytes(&pass.input, msg_list, pass.line_counter, pass.file_name) / 8;
        }
    }
    pass1
}

/// Returns pass2 from pass1
///
/// Pass1 with program counters and returns vector of pass2, with final values
pub fn get_pass2(
    msg_list: &mut MsgList,
    pass1: Vec<Pass1>,
    mut oplist: Vec<Opcode>,
    mut labels: Vec<Label>,
) -> Vec<Pass2> {
    let mut pass2: Vec<Pass2> = Vec::new();
    for line in pass1 {
        let new_opcode = if line.line_type == LineType::Opcode {
            add_registers(
                &mut oplist,
                &mut strip_comments(&mut line.input.clone()),
                line.file_name.clone(),
                msg_list,
                line.line_counter,
            ) + add_arguments(
                &mut oplist,
                &mut strip_comments(&mut line.input.clone()),
                msg_list,
                line.line_counter,
                &line.file_name,
                &mut labels,
            )
            .as_str()
        } else if line.line_type == LineType::Data {
            data_as_bytes(line.input.as_str()).unwrap_or_default()
        } else {
            String::new()
        };

        pass2.push(Pass2 {
            input: line.input,
            file_name: line.file_name.clone(),
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
    pass2
}

/// Send machine code to device
///
/// Sends the resultant code on the serial device defined if no errors were found
#[cfg(not(tarpaulin_include))] // Cannot test device write in trarpaulin
pub fn write_to_device(msg_list: &mut MsgList, bin_string: &str, output_serial_port: &str) {
    if msg_list.number_errors() == 0 {
        if write_serial(bin_string, output_serial_port, msg_list) {
            msg_list.push(
                format!("Wrote to serial port {output_serial_port}"),
                None,
                None,
                MessageType::Info,
            );
        } else {
            msg_list.push(
                format!("Failed to write to serial port {output_serial_port}"),
                None,
                None,
                MessageType::Error,
            );
        }
    } else {
        msg_list.push(
            "Not writing to serial port due to assembly errors".to_string(),
            None,
            None,
            MessageType::Warning,
        );
    }
}

/// Writes the binary file
///
/// If not errors are found, write the binary output file
#[cfg(not(tarpaulin_include))] // Cannnot test device write in tarpaulin
pub fn write_binary_file(msg_list: &mut MsgList, binary_file_name: &str, bin_string: &str) {
    msg_list.push(
        format!("Writing binary file to {binary_file_name}"),
        None,
        None,
        MessageType::Info,
    );
    if !output_binary(&binary_file_name, bin_string) {
        msg_list.push(
            format!(
                "Unable to write to binary code file {:?}",
                &binary_file_name
            ),
            None,
            None,
            MessageType::Error,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Test get_pass1 for correct vector returned, woth correct program counters
    fn test_get_pass1_1() {
        let mut msg_list = MsgList::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_opcode: String::from("0000001X"),
            comment: String::new(),
            variables: 0,
            registers: 1,
        });
        opcodes.push(Opcode {
            text_name: String::from("MOV"),
            hex_opcode: String::from("00000020"),
            comment: String::new(),
            variables: 2,
            registers: 0,
        });
        opcodes.push(Opcode {
            text_name: String::from("RET"),
            hex_opcode: String::from("00000030"),
            comment: String::new(),
            variables: 0,
            registers: 0,
        });

        let pass0 = vec![
            Pass0 {
                input: "MOV A B".to_string(),
                file_name: String::new(),
                line_counter: 1,
            },
            Pass0 {
                input: "PUSH A".to_string(),
                file_name: String::new(),
                line_counter: 2,
            },
            Pass0 {
                input: "RET".to_string(),
                file_name: String::new(),
                line_counter: 3,
            },
            Pass0 {
                input: "#DATA1 0x2".to_string(),
                file_name: String::new(),
                line_counter: 3,
            },
            Pass0 {
                input: "RET".to_string(),
                file_name: String::new(),
                line_counter: 3,
            },
            Pass0 {
                input: "#DATA1 \"HELLO\"".to_string(),
                file_name: String::new(),
                line_counter: 3,
            },
            Pass0 {
                input: "RET".to_string(),
                file_name: String::new(),
                line_counter: 3,
            },
        ];
        let pass1 = get_pass1(&mut msg_list, pass0, opcodes.clone());
        assert_eq!(pass1[0].program_counter, 0);
        assert_eq!(pass1[1].program_counter, 3);
        assert_eq!(pass1[2].program_counter, 4);
        assert_eq!(pass1[3].program_counter, 5);
        assert_eq!(pass1[4].program_counter, 7);
        assert_eq!(pass1[5].program_counter, 8);
        assert_eq!(pass1[6].program_counter, 14);
    }

    #[test]
    // Test get_pass1 for correct vector returned, woth correct program counters
    fn test_get_pass1_2() {
        let mut msg_list = MsgList::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_opcode: String::from("0000001X"),
            comment: String::new(),
            variables: 0,
            registers: 1,
        });

        let pass0 = vec![Pass0 {
            input: "Test_not_code_line".to_string(),
            file_name: String::new(),
            line_counter: 1,
        }];
        let _pass1 = get_pass1(&mut msg_list, pass0, opcodes.clone());
        assert_eq!(msg_list.list[0].name, "Opcode error Test_not_code_line");
    }

    #[allow(clippy::too_many_lines)]
    #[test]
    // Test get_pass2 for correct vector returned, with correct opcodes, registers and variables
    fn test_get_pass2_1() {
        let mut msg_list = MsgList::new();
        let labels = Vec::<Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_opcode: String::from("0000001X"),
            comment: String::new(),
            variables: 0,
            registers: 1,
        });
        opcodes.push(Opcode {
            text_name: String::from("MOVR"),
            hex_opcode: String::from("0000007X"),
            comment: String::new(),
            variables: 1,
            registers: 1,
        });
        opcodes.push(Opcode {
            text_name: String::from("MOV"),
            hex_opcode: String::from("00000020"),
            comment: String::new(),
            variables: 2,
            registers: 0,
        });
        opcodes.push(Opcode {
            text_name: String::from("RET"),
            hex_opcode: String::from("00000030"),
            comment: String::new(),
            variables: 0,
            registers: 0,
        });
        opcodes.push(Opcode {
            text_name: String::from("DELAY"),
            hex_opcode: String::from("00000040"),
            comment: String::new(),
            variables: 1,
            registers: 0,
        });

        opcodes.push(Opcode {
            text_name: String::from("DMOV"),
            hex_opcode: String::from("00000AXX"),
            comment: String::new(),
            variables: 2,
            registers: 2,
        });

        let pass2 = get_pass2(
            &mut msg_list,
            vec![
                Pass1 {
                    input: "MOV 0xEEEEEEEE 0xFFFFFFFF".to_string(),
                    file_name: String::from("test"),
                    line_counter: 1,
                    program_counter: 0,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input: "DELAY 0x7".to_string(),
                    file_name: String::from("test"),
                    line_counter: 1,
                    program_counter: 1,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input: "PUSH A".to_string(),
                    file_name: String::from("test"),
                    line_counter: 2,
                    program_counter: 3,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input: "RET".to_string(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 4,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input: "RET".to_string(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 5,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input: "MOVR C 0xAAAA".to_string(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 5,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input: "DMOV D E 0xA 0xB".to_string(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 5,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input: "#DATA1 \"HELLO\"".to_string(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 5,
                    line_type: LineType::Data,
                },
                Pass1 {
                    input: "xxx".to_string(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 5,
                    line_type: LineType::Error,
                },
            ],
            opcodes.clone(),
            labels,
        );
        assert_eq!(pass2[0].opcode, "00000020EEEEEEEEFFFFFFFF");
        assert_eq!(pass2[1].opcode, "0000004000000007");
        assert_eq!(pass2[2].opcode, "00000010");
        assert_eq!(pass2[3].opcode, "00000030");
        assert_eq!(pass2[4].opcode, "00000030");
        assert_eq!(pass2[5].opcode, "000000720000AAAA");
        assert_eq!(pass2[6].opcode, "00000A340000000A0000000B");
        assert_eq!(
            pass2[7].opcode,
            "48000000450000004C0000004C0000004F00000000000000"
        );
        assert_eq!(pass2[8].opcode, "");
    }

    #[test]
    // Test get_pass2 for invalid opcode
    fn test_get_pass2_2() {
        let mut msg_list = MsgList::new();
        let labels = Vec::<Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_opcode: String::from("0000001X"),
            comment: String::new(),
            variables: 0,
            registers: 1,
        });
        let pass2 = get_pass2(
            &mut msg_list,
            vec![Pass1 {
                input: "TEST".to_string(),
                file_name: String::from("test"),
                line_counter: 1,
                program_counter: 0,
                line_type: LineType::Opcode,
            }],
            opcodes.clone(),
            labels,
        );
        assert_eq!(pass2[0].opcode, "ERR     ");
    }
}
