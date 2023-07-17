#![warn(
    clippy::all,
    clippy::restriction,
    clippy::pedantic,
    clippy::nursery,
    clippy::cargo
)]
#![allow(clippy::arithmetic_side_effects)]
#![allow(clippy::implicit_return)]
#![allow(clippy::string_add)]
#![allow(clippy::as_conversions)]
#![allow(clippy::separated_literal_suffix)]
#![allow(clippy::blanket_clippy_restriction_lints)]
#![allow(clippy::multiple_crate_versions)]
#![allow(clippy::pattern_type_mismatch)]
#![allow(clippy::ref_patterns)]
#![allow(clippy::single_call_fn)]

//! Top level file for Klausscc

/// Module to manage file read and write
mod files;
/// Module of helper functions
mod helper;
/// Module to manage labels
mod labels;
/// Module to manage macros
mod macros;
/// Module to manage messages
mod messages;
/// Module to manage opcodes
mod opcodes;
/// Module to write to serial and read response
mod serial;
use chrono::{Local, NaiveTime};
use clap::{Arg, Command};
use files::{
    filename_stem, read_file_to_vector, remove_block_comments, write_binary_output_file,
    write_code_output_file, LineType,
};
use helper::{
    create_bin_string, data_as_bytes, is_valid_line, line_type, num_data_bytes, strip_comments,
};
use labels::{find_duplicate_label, get_labels, Label};
use macros::{expand_embedded_macros, expand_macros};
use messages::{print_messages, MessageType, MsgList};
use opcodes::{
    add_arguments, add_registers, num_arguments, parse_vh_file, Opcode, Pass0, Pass1, Pass2,
};
use serial::{write_to_board, AUTO_SERIAL};

/// Main function for Klausscc
///
/// Main function to read CLI and call other functions
#[cfg(not(tarpaulin_include))] // Cannot test main in tarpaulin
#[allow(clippy::too_many_lines)]
fn main() -> Result<(), i32> {
    use files::output_macros_opcodes_html;

    let mut msg_list = MsgList::new();
    let start_time: NaiveTime = Local::now().time();

    let matches = set_matches().get_matches();
    let opcode_file_name: String = matches
        .get_one::<String>("opcode_file")
        .unwrap_or(&"opcode_select.vh".to_owned())
        .replace(' ', "");
    let input_file_name: String = matches
        .get_one::<String>("input")
        .unwrap_or(&String::new())
        .replace(' ', "");
    let binary_file_name: String = matches
        .get_one::<String>("bitcode")
        .unwrap_or(&filename_stem(&input_file_name))
        .replace(' ', "")
        + ".kbt";
    let output_file_name: String = matches
        .get_one::<String>("output")
        .unwrap_or(&filename_stem(&input_file_name))
        .replace(' ', "")
        + ".code";
    let output_serial_port: String = matches
        .get_one::<String>("serial")
        .unwrap_or(&String::new())
        .replace(' ', "");
    let opcodes_flag = matches.get_flag("opcodes");
    let textmate_flag = matches.get_flag("textmate");

    // Parse the opcode file
    let mut opened_files: Vec<String> = Vec::new(); // Used for recursive includes check
    let vh_list = read_file_to_vector(&opcode_file_name, &mut msg_list, &mut opened_files);
    let (opt_oplist, opt_macro_list) = parse_vh_file(vh_list.unwrap_or_default(), &mut msg_list);

    if opt_macro_list.is_none() || opt_oplist.is_none() {
        msg_list.push(
            format!("Error parsing opcode file {opcode_file_name} to macro and opcode lists"),
            None,
            None,
            MessageType::Error,
        );
        print_messages(&mut msg_list);
        return Err(1_i32);
    }
    let oplist = opt_oplist.unwrap_or_else(|| [].to_vec());
    let mut macro_list =
        expand_embedded_macros(opt_macro_list.unwrap_or_else(|| [].to_vec()), &mut msg_list);

    if let Err(result_err) = output_macros_opcodes_html(
        filename_stem(&opcode_file_name),
        &oplist,
        macro_list.clone(),
        &mut msg_list,
        opcodes_flag,
        textmate_flag,
    ) {
        msg_list.push(
            format!("Error {result_err} writing opcode file {opcode_file_name} to HTML"),
            None,
            None,
            MessageType::Error,
        );
    }

    if textmate_flag || opcodes_flag {
        print_messages(&mut msg_list);
        return Ok(());
    }

    // Parse the input file
    msg_list.push(
        format!("Input file is {input_file_name}"),
        None,
        None,
        MessageType::Information,
    );
    let mut opened_input_files: Vec<String> = Vec::new(); // Used for recursive includes check
    let input_list_option =
        read_file_to_vector(&input_file_name, &mut msg_list, &mut opened_input_files);
    if input_list_option.is_none() {
        print_messages(&mut msg_list);
        return Err(1_i32);
    }

    let input_list = remove_block_comments(
        input_list_option.unwrap_or_else(|| [].to_vec()),
        &mut msg_list,
    );

    // Pass 0 to add macros
    let pass0 = expand_macros(
        &mut msg_list,
        input_list,
        &mut macro_list,
    );

    // Pass 1 to get line numbers and labels
    let pass1: Vec<Pass1> = get_pass1(&mut msg_list, pass0, oplist.clone());
    let mut labels = get_labels(&pass1, &mut msg_list);
    find_duplicate_label(&mut labels, &mut msg_list);

    // Pass 2 to get create output
    let mut pass2 = get_pass2(&mut msg_list, pass1, oplist, labels);

    if let Err(result_err) = write_code_output_file(&output_file_name, &mut pass2, &mut msg_list) {
        msg_list.push(
            format!(
                "Unable to write to code file {}, error {}",
                &output_file_name, result_err
            ),
            None,
            None,
            MessageType::Error,
        );
        print_messages(&mut msg_list);
        return Err(1_i32);
    }

    if msg_list.number_errors() == 0 {
        // let bin_string = create_bin_string(&mut pass2, &mut msg_list);
        if let Some(bin_string) = create_bin_string(&mut pass2, &mut msg_list) {
            write_binary_file(&mut msg_list, &binary_file_name, &bin_string);
            if !output_serial_port.is_empty() {
                write_to_device(&mut msg_list, &bin_string, &output_serial_port);
            }
        } else {
            if std::fs::remove_file(&binary_file_name).is_ok() {
                msg_list.push(
                    "Removed old binary file".to_owned(),
                    None,
                    None,
                    MessageType::Warning,
                );
            }
            msg_list.push(
                "Not writing binary file due to assembly errors creating binary file".to_owned(),
                None,
                None,
                MessageType::Warning,
            );
        }
    } else {
        if std::fs::remove_file(&binary_file_name).is_ok() {
            msg_list.push(
                "Removed old binary file".to_owned(),
                None,
                None,
                MessageType::Warning,
            );
        }
        msg_list.push(
            "Not writing new binary file due to assembly errors".to_owned(),
            None,
            None,
            MessageType::Warning,
        );
    }

    print_results(&mut msg_list, start_time);
    Ok(())
}

/// Manages the CLI
///
/// Uses the Command from Clap to expand the CLI
#[cfg(not(tarpaulin_include))] // Can not test CLI in tarpaulin
#[must_use]
pub fn set_matches() -> Command {
    use clap::ArgAction;

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
                .conflicts_with("textmate")
                .conflicts_with("opcodes")
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
            Arg::new("opcodes")
                .long("opcodes")
                .action(ArgAction::SetTrue)
                .help("Set if output of opcode.macro list is required"),
        )
        .arg(
            Arg::new("textmate")
                .short('t')
                .long("textmate")
                .action(ArgAction::SetTrue)
                .help(
                    "Prints list of all opcodes for use in Textmate of vscode language formatter",
                ),
        )
        .arg(
            Arg::new("serial")
                .short('s')
                .long("serial")
                .num_args(0..=1)
                .default_missing_value(AUTO_SERIAL)
                .help("Serial port for output"),
        )
}
/// Prints results of assembly
///
/// Takes the message list and start time and prints the results to the users
#[allow(clippy::cast_precision_loss)]
#[allow(clippy::float_arithmetic)]
#[allow(clippy::print_stdout)]
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
/// Takes the macro expanded pass0 and returns vector of pass1, with the program counters
pub fn get_pass1(msg_list: &mut MsgList, pass0: Vec<Pass0>, mut oplist: Vec<Opcode>) -> Vec<Pass1> {
    let mut pass1: Vec<Pass1> = Vec::new();
    let mut program_counter: u32 = 0;
    let mut data_pass0: Vec<Pass0> = Vec::new();

    for mut pass in pass0 {
        pass1.push(Pass1 {
            input_text_line: pass.input_text_line.clone(),
            file_name: pass.file_name.clone(),
            line_counter: pass.line_counter,
            program_counter,
            line_type: line_type(&mut oplist, &mut pass.input_text_line),
        });
        if !is_valid_line(&mut oplist, strip_comments(&mut pass.input_text_line)) {
            msg_list.push(
                format!("Error {}", pass.input_text_line),
                Some(pass.line_counter),
                Some(pass.file_name.clone()),
                MessageType::Error,
            );
        }
        if line_type(&mut oplist, &mut pass.input_text_line) == LineType::Opcode {
            let num_args =
                num_arguments(&mut oplist, &mut strip_comments(&mut pass.input_text_line));
            if let Some(arguments) = num_args {
                program_counter = program_counter + arguments + 1;
            }
        }

        if line_type(&mut oplist, &mut pass.input_text_line) == LineType::Data {
            data_pass0.push(pass);
            pass1.pop();
        }
    }
    #[allow(clippy::integer_division)]
    for mut data_pass in data_pass0 {
        pass1.push(Pass1 {
            input_text_line: data_pass.input_text_line.clone(),
            file_name: data_pass.file_name.clone(),
            line_counter: data_pass.line_counter,
            program_counter,
            line_type: line_type(&mut oplist, &mut data_pass.input_text_line),
        });
        program_counter += num_data_bytes(
            &data_pass.input_text_line,
            msg_list,
            data_pass.line_counter,
            data_pass.file_name,
        ) / 8;
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
                &mut strip_comments(&mut line.input_text_line.clone()),
                line.file_name.clone(),
                msg_list,
                line.line_counter,
            ) + add_arguments(
                &mut oplist,
                &mut strip_comments(&mut line.input_text_line.clone()),
                msg_list,
                line.line_counter,
                &line.file_name,
                &mut labels,
            )
            .as_str()
        } else if line.line_type == LineType::Data {
            data_as_bytes(line.input_text_line.as_str()).unwrap_or_default()
        } else {
            String::new()
        };

        pass2.push(Pass2 {
            input_text_line: line.input_text_line,
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
#[cfg(not(tarpaulin_include))] // Cannot test device write in tarpaulin
pub fn write_to_device(msg_list: &mut MsgList, bin_string: &str, output_serial_port: &str) {
    if msg_list.number_errors() == 0 {
        let write_result = write_to_board(bin_string, output_serial_port, msg_list);
        match write_result {
            Ok(_) => {
                msg_list.push(
                    "Wrote to serial port".to_owned(),
                    None,
                    None,
                    MessageType::Information,
                );
            }
            Err(err) => {
                msg_list.push(
                    format!("Failed to write to serial port, error \"{err}\""),
                    None,
                    None,
                    MessageType::Error,
                );
            }
        }
    } else {
        msg_list.push(
            "Not writing to serial port due to assembly errors".to_owned(),
            None,
            None,
            MessageType::Warning,
        );
    }
}

/// Writes the binary file
///
/// If not errors are found, write the binary output file
#[cfg(not(tarpaulin_include))] // Cannot test device write in tarpaulin
pub fn write_binary_file(msg_list: &mut MsgList, binary_file_name: &str, bin_string: &str) {
    msg_list.push(
        format!("Writing binary file to {binary_file_name}"),
        None,
        None,
        MessageType::Information,
    );
    if let Err(result_err) = write_binary_output_file(&binary_file_name, bin_string) {
        msg_list.push(
            format!(
                "Unable to write to binary code file {:?}, error {}",
                &binary_file_name, result_err
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
    // Test get_pass1 for correct vector returned, with correct program counters
    fn test_get_pass1_1() {
        let mut msg_list = MsgList::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_opcode: String::from("0000001X"),
            comment: String::new(),
            variables: 0,
            registers: 1,
            section: String::new(),
        });
        opcodes.push(Opcode {
            text_name: String::from("MOV"),
            hex_opcode: String::from("00000020"),
            comment: String::new(),
            variables: 2,
            registers: 0,
            section: String::new(),
        });
        opcodes.push(Opcode {
            text_name: String::from("RET"),
            hex_opcode: String::from("00000030"),
            comment: String::new(),
            variables: 0,
            registers: 0,
            section: String::new(),
        });

        let pass0 = vec![
            Pass0 {
                input_text_line: "MOV A B".to_owned(),
                file_name: String::new(),
                line_counter: 1,
            },
            Pass0 {
                input_text_line: "PUSH A".to_owned(),
                file_name: String::new(),
                line_counter: 2,
            },
            Pass0 {
                input_text_line: "RET".to_owned(),
                file_name: String::new(),
                line_counter: 3,
            },
            Pass0 {
                input_text_line: "#DATA1 0x2".to_owned(), // Should be moved to end
                file_name: String::new(),
                line_counter: 4,
            },
            Pass0 {
                input_text_line: "RET".to_owned(),
                file_name: String::new(),
                line_counter: 5,
            },
            Pass0 {
                input_text_line: "#DATA1 \"HELLO\"".to_owned(), // Should be moved to end
                file_name: String::new(),
                line_counter: 6,
            },
            Pass0 {
                input_text_line: "RET".to_owned(),
                file_name: String::new(),
                line_counter: 7,
            },
        ];
        let pass1 = get_pass1(&mut msg_list, pass0, opcodes.clone());
        assert_eq!(pass1.get(0).unwrap_or_default().program_counter, 0);
        assert_eq!(pass1.get(1).unwrap_or_default().program_counter, 3);
        assert_eq!(pass1.get(2).unwrap_or_default().program_counter, 4);
        assert_eq!(pass1.get(3).unwrap_or_default().program_counter, 5);
        assert_eq!(pass1.get(4).unwrap_or_default().program_counter, 6);
        assert_eq!(pass1.get(5).unwrap_or_default().program_counter, 7);
        assert_eq!(pass1.get(6).unwrap_or_default().program_counter, 9);
    }

    #[test]
    // Test get_pass1 for correct vector returned, with correct program counters
    fn test_get_pass1_2() {
        let mut msg_list = MsgList::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_opcode: String::from("0000001X"),
            comment: String::new(),
            variables: 0,
            registers: 1,
            section: String::new(),
        });

        let pass0 = vec![Pass0 {
            input_text_line: "Test_not_code_line".to_owned(),
            file_name: String::new(),
            line_counter: 1,
        }];
        let _pass1 = get_pass1(&mut msg_list, pass0, opcodes.clone());
        assert_eq!(
            msg_list.list.get(0).unwrap_or_default().text,
            "Error Test_not_code_line"
        );
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
            section: String::new(),
        });
        opcodes.push(Opcode {
            text_name: String::from("MOVR"),
            hex_opcode: String::from("0000007X"),
            comment: String::new(),
            variables: 1,
            registers: 1,
            section: String::new(),
        });
        opcodes.push(Opcode {
            text_name: String::from("MOV"),
            hex_opcode: String::from("00000020"),
            comment: String::new(),
            variables: 2,
            registers: 0,
            section: String::new(),
        });
        opcodes.push(Opcode {
            text_name: String::from("RET"),
            hex_opcode: String::from("00000030"),
            comment: String::new(),
            variables: 0,
            registers: 0,
            section: String::new(),
        });
        opcodes.push(Opcode {
            text_name: String::from("DELAY"),
            hex_opcode: String::from("00000040"),
            comment: String::new(),
            variables: 1,
            registers: 0,
            section: String::new(),
        });

        opcodes.push(Opcode {
            text_name: String::from("DMOV"),
            hex_opcode: String::from("00000AXX"),
            comment: String::new(),
            variables: 2,
            registers: 2,
            section: String::new(),
        });

        let pass2 = get_pass2(
            &mut msg_list,
            vec![
                Pass1 {
                    input_text_line: "MOV 0xEEEEEEEE 0xFFFFFFFF".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 1,
                    program_counter: 0,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input_text_line: "DELAY 0x7".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 1,
                    program_counter: 1,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input_text_line: "PUSH A".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 2,
                    program_counter: 3,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input_text_line: "RET".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 4,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input_text_line: "RET".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 5,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input_text_line: "MOVR C 0xAAAA".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 5,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input_text_line: "DMOV D E 0xA 0xB".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 5,
                    line_type: LineType::Opcode,
                },
                Pass1 {
                    input_text_line: "#DATA1 \"HELLO\"".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 5,
                    line_type: LineType::Data,
                },
                Pass1 {
                    input_text_line: "xxx".to_owned(),
                    file_name: String::from("test"),
                    line_counter: 3,
                    program_counter: 5,
                    line_type: LineType::Error,
                },
            ],
            opcodes.clone(),
            labels,
        );
        assert_eq!(
            pass2.get(0).unwrap_or_default().opcode,
            "00000020EEEEEEEEFFFFFFFF"
        );
        assert_eq!(pass2.get(1).unwrap_or_default().opcode, "0000004000000007");
        assert_eq!(pass2.get(2).unwrap_or_default().opcode, "00000010");
        assert_eq!(pass2.get(3).unwrap_or_default().opcode, "00000030");
        assert_eq!(pass2.get(4).unwrap_or_default().opcode, "00000030");
        assert_eq!(pass2.get(5).unwrap_or_default().opcode, "000000720000AAAA");
        assert_eq!(
            pass2.get(6).unwrap_or_default().opcode,
            "00000A340000000A0000000B"
        );
        assert_eq!(
            pass2.get(7).unwrap_or_default().opcode,
            "0000000248454C4C4F000000"
        );
        assert_eq!(pass2.get(8).unwrap_or_default().opcode, "");
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
            section: String::new(),
        });
        let pass2 = get_pass2(
            &mut msg_list,
            vec![Pass1 {
                input_text_line: "TEST".to_owned(),
                file_name: String::from("test"),
                line_counter: 1,
                program_counter: 0,
                line_type: LineType::Opcode,
            }],
            opcodes.clone(),
            labels,
        );
        assert_eq!(pass2.get(0).unwrap_or_default().opcode, "ERR     ");
    }
}
