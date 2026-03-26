#![warn(clippy::all, clippy::restriction, clippy::pedantic, clippy::nursery, clippy::cargo)]

#![allow(clippy::allow_attributes, reason = "Needed to allow use of clippy restrictions")]
#![allow(clippy::implicit_return, reason = "Needed for compatibility with code using implicit returns")]
#![allow(clippy::as_conversions, reason = "Needed for data manipulation")]
#![allow(clippy::separated_literal_suffix, reason = "Needed for readability of numeric literals")]
#![allow(clippy::blanket_clippy_restriction_lints, reason = "Needed to enable clippy restrictions")]
#![allow(clippy::multiple_crate_versions, reason = "Needed for compatibility with crates using multiple versions")]
#![allow(clippy::single_call_fn, reason = "Needed for code clarity")]
#![allow(clippy::redundant_test_prefix, reason = "Needed for compatibility with test naming")]

//! Top level file for Klausscc.

/// Module to manage file read and write.
mod files;
/// Module of helper functions.
mod helper;
/// Module to manage labels.
mod labels;
/// Module to manage macros.
mod macros;
/// Module to manage messages.
mod messages;
/// Module to manage opcodes.
mod opcodes;
/// Module to write to serial and read response.
mod serial;
use chrono::{Local, NaiveTime};
use clap::{Arg, Command};
use files::{filename_stem, read_file_to_vector, remove_block_comments, write_binary_output_file, write_code_output_file, LineType};
use helper::{create_bin_string, data_as_bytes, is_valid_line, line_type, num_data_bytes, parse_expected_uart_values, strip_comments};
use labels::{find_duplicate_label, get_labels, Label};
use macros::{expand_embedded_macros, expand_macros};
use messages::{print_messages, MessageType, MsgList};
use opcodes::{add_arguments, add_registers, num_arguments, parse_vh_file, Opcode, Pass0, Pass1, Pass2};
use serial::{monitor_serial, monitor_serial_port, run_test_monitor, write_to_board, write_to_board_keep_port, AUTO_SERIAL};
use std::fs::{self, read_to_string};

/// Main function for Klausscc.
///
/// Main function to read CLI and call other functions.
#[cfg(not(tarpaulin_include))] // Cannot test main in tarpaulin
#[allow(
    clippy::too_many_lines,
    reason = "Main function requires many lines to handle CLI and file processing logic"
)]
#[allow(clippy::or_fun_call, reason = "Needed for simplicity of setting up strings and file names")]
fn main() -> Result<(), i32> {
    use std::fs::remove_file;

    use files::output_macros_opcodes_html;

    let mut msg_list = MsgList::new();
    let start_time: NaiveTime = Local::now().time();

    let matches = set_matches().get_matches();
    let opcode_file_name: String = matches
        .get_one::<String>("opcode_file")
        .unwrap_or(&"opcode_select.vh".to_owned())
        .replace(' ', "");
    let input_file_name: String = matches.get_one::<String>("input").unwrap_or(&String::default()).replace(' ', "");
    let mut binary_file_name: String = matches
        .get_one::<String>("bitcode")
        .unwrap_or(&filename_stem(&input_file_name))
        .replace(' ', "");
    binary_file_name.push_str(".kbt");
    let mut output_file_name: String = matches
        .get_one::<String>("output")
        .unwrap_or(&filename_stem(&input_file_name))
        .replace(' ', "");
    output_file_name.push_str(".code");
    let output_serial_port: String = matches.get_one::<String>("serial").unwrap_or(&String::default()).replace(' ', "");
    let opcodes_flag = matches.get_flag("opcodes");
    let textmate_flag = matches.get_flag("textmate");
    let monitor_flag = matches.get_flag("monitor");
    let test_flag = matches.get_flag("test");
    let test_timeout: u64 = matches
        .get_one::<String>("test_timeout")
        .and_then(|timeout_str| timeout_str.parse().ok())
        .unwrap_or(10);
    let test_list_file: String = matches.get_one::<String>("test_list").unwrap_or(&String::default()).replace(' ', "");

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
        print_messages(&msg_list);
        return Err(1_i32);
    }
    let oplist = opt_oplist.unwrap_or_else(|| [].to_vec());
    let mut macro_list = expand_embedded_macros(opt_macro_list.unwrap_or_else(|| [].to_vec()), &mut msg_list);

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
        print_messages(&msg_list);
        return Ok(());
    }

    // Batch test list mode
    if !test_list_file.is_empty() {
        return run_test_list(&oplist, &macro_list, &test_list_file, &output_serial_port, test_timeout, &mut msg_list);
    }

    // Parse the input file
    msg_list.push(format!("Input file is {input_file_name}"), None, None, MessageType::Information);
    let mut opened_input_files: Vec<String> = Vec::new(); // Used for recursive includes check
    let input_list_option = read_file_to_vector(&input_file_name, &mut msg_list, &mut opened_input_files);
    if input_list_option.is_none() {
        print_messages(&msg_list);
        return Err(1_i32);
    }

    let input_list = remove_block_comments(input_list_option.unwrap_or_else(|| [].to_vec()), &mut msg_list);

    // Pass 0 to add macros
    let pass0 = expand_macros(&mut msg_list, input_list, &mut macro_list);

    // Pass 1 to get line numbers and labels
    let pass1: Vec<Pass1> = get_pass1(&mut msg_list, pass0, oplist.clone());
    let mut labels = get_labels(&pass1, &mut msg_list);
    find_duplicate_label(&mut labels, &mut msg_list);

    // Pass 2 to get create output
    let mut pass2 = get_pass2(&mut msg_list, pass1, oplist, labels);

    if let Err(result_err) = write_code_output_file(&output_file_name, &mut pass2, &mut msg_list) {
        msg_list.push(
            format!("Unable to write to code file {}, error {}", &output_file_name, result_err),
            None,
            None,
            MessageType::Error,
        );
        print_messages(&msg_list);
        return Err(1_i32);
    }

    if msg_list.number_by_type(&MessageType::Error) == 0 {
        if let Some(bin_string) = create_bin_string(&pass2, &mut msg_list) {
            write_binary_file(&mut msg_list, &binary_file_name, &bin_string);
            if !output_serial_port.is_empty() {
                if test_flag {
                    // Test mode: send to board, keep port open, then verify UART output
                    return run_test_mode(&mut msg_list, &bin_string, &output_serial_port, &input_file_name, test_timeout, start_time);
                }
                if monitor_flag {
                    // Monitor mode: send to board, keep port open, then monitor UART output
                    match write_to_board_keep_port(&bin_string, &output_serial_port, &mut msg_list) {
                        Ok(port) => {
                            msg_list.push("Wrote to serial port".to_owned(), None, None, MessageType::Information);
                            print_results(&msg_list, start_time);
                            if let Err(err) = monitor_serial_port(port, &mut msg_list) {
                                msg_list.push(
                                    format!("Serial monitor stopped: \"{err}\""),
                                    None,
                                    None,
                                    MessageType::Warning,
                                );
                                print_messages(&msg_list);
                            }
                            return Ok(());
                        }
                        Err(err) => {
                            msg_list.push(format!("Failed to write to serial port, error \"{err}\""), None, None, MessageType::Error);
                        }
                    }
                } else {
                    write_to_device(&mut msg_list, &bin_string, &output_serial_port);
                }
            }
        } else {
            if remove_file(&binary_file_name).is_ok() {
                msg_list.push("Removed old binary file".to_owned(), None, None, MessageType::Warning);
            }
            msg_list.push(
                "Not writing binary file due to assembly errors creating binary file".to_owned(),
                None,
                None,
                MessageType::Warning,
            );
        }
    } else {
        if remove_file(&binary_file_name).is_ok() {
            msg_list.push("Removed old binary file".to_owned(), None, None, MessageType::Warning);
        }
        msg_list.push(
            "Not writing new binary file due to assembly errors".to_owned(),
            None,
            None,
            MessageType::Warning,
        );
    }

    print_results(&msg_list, start_time);

    // Monitor serial port after assembly and send if requested
    if monitor_flag {
        if output_serial_port.is_empty() {
            msg_list.push(
                "Monitor flag (-m) requires a serial port (-s)".to_owned(),
                None,
                None,
                MessageType::Error,
            );
            print_messages(&msg_list);
            return Err(1_i32);
        }
        if let Err(err) = monitor_serial(&output_serial_port, &mut msg_list) {
            msg_list.push(
                format!("Serial monitor stopped: \"{err}\""),
                None,
                None,
                MessageType::Warning,
            );
            print_messages(&msg_list);
        }
    }

    // Check test flag requires serial port
    if test_flag && output_serial_port.is_empty() {
        msg_list.push(
            "Test flag (-T) requires a serial port (-s)".to_owned(),
            None,
            None,
            MessageType::Error,
        );
        print_messages(&msg_list);
        return Err(1_i32);
    }

    Ok(())
}

/// Returns pass1 from pass0.
///
/// Takes the macro expanded pass0 and returns vector of pass1, with the program counters.
#[inline]
pub fn get_pass1(msg_list: &mut MsgList, pass0: Vec<Pass0>, mut oplist: Vec<Opcode>) -> Vec<Pass1> {
    let mut pass1: Vec<Pass1> = Vec::new();
    let mut program_counter: u32 = 0;
    let mut data_pass0: Vec<Pass0> = Vec::new();
    let mut in_data_section = false;

    for pass in pass0 {
        let stripped = strip_comments(&pass.input_text_line);
        let first_word = stripped.split_whitespace().next().unwrap_or("").to_owned();

        // Track section context for C compiler directives
        match first_word.as_str() {
            ".text" => { in_data_section = false; }
            ".data" | ".rodata" | ".bss" => { in_data_section = true; }
            _ => {}
        }

        // Expand .comm/.lcomm NAME SIZE into label + .space, defer to data section
        if first_word == ".comm" || first_word == ".lcomm" {
            let parts: Vec<&str> = stripped.split(|c: char| c.is_whitespace() || c == ',')
                .filter(|s| !s.is_empty())
                .collect();
            if parts.len() >= 3 {
                let name = parts[1];
                let size_str = parts[2];
                let size: u32 = size_str.parse().unwrap_or(0);
                data_pass0.push(Pass0 {
                    input_text_line: format!("{name}:"),
                    file_name: pass.file_name.clone(),
                    line_counter: pass.line_counter,
                });
                if size > 0 {
                    data_pass0.push(Pass0 {
                        input_text_line: format!(".space {size}"),
                        file_name: pass.file_name.clone(),
                        line_counter: pass.line_counter,
                    });
                }
            }
            continue;
        }

        let lt = line_type(&mut oplist, &pass.input_text_line);

        // Defer labels in data section to end of program with data
        if in_data_section && lt == LineType::Label {
            data_pass0.push(pass);
            continue;
        }

        pass1.push(Pass1 {
            input_text_line: pass.input_text_line.clone(),
            file_name: pass.file_name.clone(),
            line_counter: pass.line_counter,
            program_counter,
            line_type: lt.clone(),
        });
        if !is_valid_line(&mut oplist, strip_comments(&pass.input_text_line)) {
            msg_list.push(
                format!("Error {}", pass.input_text_line),
                Some(pass.line_counter),
                Some(pass.file_name.clone()),
                MessageType::Error,
            );
        }
        if lt == LineType::Opcode {
            let num_args = num_arguments(&mut oplist, &strip_comments(&pass.input_text_line));
            #[allow(clippy::arithmetic_side_effects, reason = "Needed for correct program counter calculation")]
            if let Some(arguments) = num_args {
                program_counter = program_counter + arguments + 1;
            }
        }

        if lt == LineType::Data {
            data_pass0.push(pass);
            pass1.pop();
        }
    }
    #[allow(clippy::integer_division, reason = "Needed for correct data size calculation")]
    #[allow(clippy::arithmetic_side_effects, reason = "Needed for correct data size calculation")]
    #[allow(clippy::integer_division_remainder_used, reason = "Needed for correct data size calculation")]
    for data_pass in data_pass0 {
        let lt = line_type(&mut oplist, &data_pass.input_text_line);
        pass1.push(Pass1 {
            input_text_line: data_pass.input_text_line.clone(),
            file_name: data_pass.file_name.clone(),
            line_counter: data_pass.line_counter,
            program_counter,
            line_type: lt.clone(),
        });

        if lt == LineType::Data {
            program_counter += num_data_bytes(&data_pass.input_text_line, msg_list, data_pass.line_counter, data_pass.file_name) / 8;
        }
    }
    pass1
}

/// Returns pass2 from pass1.
///
/// Pass1 with program counters and returns vector of pass2, with final values.
#[inline]
pub fn get_pass2(msg_list: &mut MsgList, pass1: Vec<Pass1>, mut oplist: Vec<Opcode>, mut labels: Vec<Label>) -> Vec<Pass2> {
    let mut pass2: Vec<Pass2> = Vec::new();
    for line in pass1 {
        let new_opcode = if line.line_type == LineType::Opcode {
            let mut opcode = add_registers(
                &mut oplist,
                &strip_comments(&line.input_text_line.clone()),
                line.file_name.clone(),
                msg_list,
                line.line_counter,
            );
            opcode.push_str(
                &add_arguments(
                    &mut oplist,
                    &strip_comments(&line.input_text_line.clone()),
                    msg_list,
                    line.line_counter,
                    &line.file_name,
                    &mut labels,
                )
            );
            opcode
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
            line_type: if new_opcode.contains("ERR") { LineType::Error } else { line.line_type },
            opcode: new_opcode,
        });
    }
    pass2
}

/// Prints results of assembly.
///
/// Takes the message list and start time and prints the results to the users.
#[inline]
#[allow(clippy::print_stdout, reason = "Printing to stdout is required for user feedback in this function")]
#[cfg(not(tarpaulin_include))] // Cannot test printing in tarpaulin
pub fn print_results(msg_list: &MsgList, start_time: NaiveTime) {
    print_messages(msg_list);
    #[allow(clippy::arithmetic_side_effects, reason = "Needed for correct duration calculation")]
    let duration = Local::now().time() - start_time;
    #[allow(clippy::float_arithmetic, reason = "Needed for correct duration calculation")]
    #[allow(clippy::cast_precision_loss, reason = "Needed for correct duration calculation")]
    let time_taken: f64 = duration.num_milliseconds() as f64 / 1000.0;
    println!(
        "Completed with {} error{} and {} warning{} in {:.3} seconds",
        msg_list.number_by_type(&MessageType::Error),
        if msg_list.number_by_type(&MessageType::Error) == 1 { "" } else { "s" },
        msg_list.number_by_type(&MessageType::Warning),
        if msg_list.number_by_type(&MessageType::Warning) == 1 { "" } else { "s" },
        time_taken,
    );
}

/// Manages the CLI.
///
/// Uses the Command from Clap to expand the CLI.
#[inline]
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
                .required_unless_present_any(["textmate", "opcodes", "test_list"])
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
                .help("Set if JSON output of opcode and macro list is required"),
        )
        .arg(
            Arg::new("textmate")
                .short('t')
                .long("textmate")
                .action(ArgAction::SetTrue)
                .help("Set if JSON output of opcodes for use in Textmate of vscode language formatter is required"),
        )
        .arg(
            Arg::new("serial")
                .short('s')
                .long("serial")
                .num_args(0..=1)
                .default_missing_value(AUTO_SERIAL)
                .help("Serial port for output"),
        )
        .arg(
            Arg::new("monitor")
                .short('m')
                .long("monitor")
                .action(ArgAction::SetTrue)
                .conflicts_with("test")
                .help("Monitor serial port for UART output after sending (Ctrl+C to stop)"),
        )
        .arg(
            Arg::new("test")
                .short('T')
                .long("test")
                .action(ArgAction::SetTrue)
                .conflicts_with("monitor")
                .help("Test mode: verify UART output against expected values in source comments"),
        )
        .arg(
            Arg::new("test_timeout")
                .long("test-timeout")
                .num_args(1)
                .default_value("10")
                .help("Timeout in seconds for test mode UART capture (default: 10)"),
        )
        .arg(
            Arg::new("test_list")
                .short('L')
                .long("test-list")
                .num_args(1)
                .conflicts_with("input")
                .conflicts_with("monitor")
                .help("File containing list of test .kla files to assemble and verify sequentially"),
        )
}

/// Writes the binary file.
///
/// If not errors are found, write the binary output file.
#[inline]
#[cfg(not(tarpaulin_include))] // Cannot test device write in tarpaulin
pub fn write_binary_file(msg_list: &mut MsgList, binary_file_name: &str, bin_string: &str) {
    msg_list.push(format!("Writing binary file to {binary_file_name}"), None, None, MessageType::Information);
    if let Err(result_err) = write_binary_output_file(&binary_file_name, bin_string) {
        msg_list.push(
            format!("Unable to write to binary code file {:?}, error {}", &binary_file_name, result_err),
            None,
            None,
            MessageType::Error,
        );
    }
}

/// Send machine code to device.
///
/// Sends the resultant code on the serial device defined if no errors were found.
#[inline]
#[cfg(not(tarpaulin_include))] // Cannot test device write in tarpaulin
pub fn write_to_device(msg_list: &mut MsgList, bin_string: &str, output_serial_port: &str) {
    if msg_list.number_by_type(&MessageType::Error) == 0 {
        let write_result = write_to_board(bin_string, output_serial_port, msg_list);
        match write_result {
            Ok(()) => {
                msg_list.push("Wrote to serial port".to_owned(), None, None, MessageType::Information);
            }
            Err(err) => {
                msg_list.push(format!("Failed to write to serial port, error \"{err}\""), None, None, MessageType::Error);
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

/// Run test verification mode.
///
/// Sends program to board, reads UART output, and verifies against expected values
/// parsed from the source file comments.
#[inline]
#[allow(clippy::print_stdout, reason = "Printing to stdout is required for test result output")]
#[allow(clippy::missing_errors_doc, reason = "Only test function to stdout is required for test result output")]
#[cfg(not(tarpaulin_include))] // Cannot test serial hardware in tarpaulin
pub fn run_test_mode(
    msg_list: &mut MsgList,
    bin_string: &str,
    output_serial_port: &str,
    input_file_name: &str,
    test_timeout: u64,
    start_time: NaiveTime,
) -> Result<(), i32> {
    // Parse expected values from the raw source file
    let raw_lines: Vec<String> = fs::read_to_string(input_file_name)
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect();
    let expected_values = parse_expected_uart_values(&raw_lines);

    if expected_values.is_empty() {
        msg_list.push(
            "No expected UART values found in source file comments".to_owned(),
            None,
            None,
            MessageType::Error,
        );
        print_results(msg_list, start_time);
        return Err(1_i32);
    }

    msg_list.push(
        format!("Found {} expected UART values in source comments", expected_values.len()),
        None,
        None,
        MessageType::Information,
    );

    // Send to board and keep port open
    let port = match write_to_board_keep_port(bin_string, output_serial_port, msg_list) {
        Ok(port) => {
            msg_list.push("Wrote to serial port".to_owned(), None, None, MessageType::Information);
            port
        }
        Err(err) => {
            msg_list.push(
                format!("Failed to write to serial port, error \"{err}\""),
                None,
                None,
                MessageType::Error,
            );
            print_results(msg_list, start_time);
            return Err(1_i32);
        }
    };

    print_results(msg_list, start_time);

    // Run test verification
    let result = run_test_monitor(port, &expected_values, test_timeout, msg_list);

    // Print summary
    println!(
        "\nTest result: {}/{} passed, {}/{} failed{}",
        result.passed,
        result.total,
        result.failed,
        result.total,
        if result.timed_out { " (TIMED OUT)" } else { "" },
    );

    if result.failed > 0 || result.timed_out {
        Err(if result.timed_out { 3_i32 } else { 2_i32 })
    } else {
        Ok(())
    }
}

/// Assemble a single input file and return the binary string.
///
/// Runs the full assembly pipeline (pass0 → pass1 → pass2 → binary) for one file.
/// Returns `Some(binary_string)` on success, `None` on assembly error.
#[inline]
#[cfg(not(tarpaulin_include))]
pub fn assemble_file(
    input_file_name: &str,
    oplist: &[Opcode],
    macro_list: &[macros::Macro],
    msg_list: &mut MsgList,
) -> Option<String> {
    msg_list.push(format!("Input file is {input_file_name}"), None, None, MessageType::Information);
    let mut opened_input_files: Vec<String> = Vec::new();
    let input_list_option = read_file_to_vector(input_file_name, msg_list, &mut opened_input_files);
    if input_list_option.is_none() {
        return None;
    }

    let input_list = remove_block_comments(input_list_option.unwrap_or_else(|| [].to_vec()), msg_list);

    let mut macro_list_clone = macro_list.to_vec();
    let pass0 = expand_macros(msg_list, input_list, &mut macro_list_clone);
    let pass1: Vec<Pass1> = get_pass1(msg_list, pass0, oplist.to_vec());
    let mut labels = get_labels(&pass1, msg_list);
    find_duplicate_label(&mut labels, msg_list);
    let mut pass2 = get_pass2(msg_list, pass1, oplist.to_vec(), labels);

    let output_file_name = format!("{}.code", filename_stem(&input_file_name.to_owned()));
    if let Err(result_err) = write_code_output_file(&output_file_name, &mut pass2, msg_list) {
        msg_list.push(
            format!("Unable to write to code file {output_file_name}, error {result_err}"),
            None,
            None,
            MessageType::Error,
        );
        return None;
    }

    if msg_list.number_by_type(&MessageType::Error) > 0 {
        return None;
    }

    create_bin_string(&pass2, msg_list)
}

/// Result of a single test in a batch run.
struct BatchTestResult {
    /// File name of the test.
    file_name: String,
    /// Number of expected values that matched.
    passed: usize,
    /// Number of expected values that did not match.
    failed: usize,
    /// True if the test timed out.
    timed_out: bool,
    /// Total number of expected values.
    total: usize,
    /// True if the test could not be assembled or had no expected values.
    skipped: bool,
    /// Reason for skipping, if applicable.
    skip_reason: String,
}

/// Read a test list file and return the list of test file paths.
///
/// Each line is a test file path. Blank lines and lines starting with `//` or `#` are ignored.
/// Paths are resolved relative to the directory containing the list file.
#[cfg(not(tarpaulin_include))]
fn read_test_list(list_file: &str, msg_list: &mut MsgList) -> Vec<String> {
    let list_dir = std::path::Path::new(list_file)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));

    let contents = match std::fs::read_to_string(list_file) {
        Ok(c) => c,
        Err(err) => {
            msg_list.push(
                format!("Error reading test list file {list_file}: \"{err}\""),
                None,
                None,
                MessageType::Error,
            );
            return Vec::new();
        }
    };

    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//") && !line.starts_with('#'))
        .map(|line| {
            let path = std::path::Path::new(line);
            if path.is_absolute() {
                line.to_owned()
            } else {
                list_dir.join(line).to_string_lossy().into_owned()
            }
        })
        .collect()
}

/// Run a batch of tests from a test list file.
///
/// For each test file: assembles, sends to board, verifies UART output,
/// and prints per-test and aggregate results.
#[allow(clippy::print_stdout, reason = "Printing to stdout is required for batch test output")]
#[allow(clippy::arithmetic_side_effects, reason = "Counter arithmetic is safe")]
#[allow(clippy::missing_inline_in_public_items, reason = "Only used in main batch test function, which is not performance critical")]
#[allow(clippy::too_many_lines, reason = "Only for test batch management, which requires many lines to handle all the logic")]
#[cfg(not(tarpaulin_include))]
pub fn run_test_list(
    oplist: &[Opcode],
    macro_list: &[macros::Macro],
    list_file: &str,
    output_serial_port: &str,
    test_timeout: u64,
    msg_list: &mut MsgList,
) -> Result<(), i32> {
    if output_serial_port.is_empty() {
        msg_list.push(
            "Test list (-L) requires a serial port (-s)".to_owned(),
            None,
            None,
            MessageType::Error,
        );
        print_messages(msg_list);
        return Err(1_i32);
    }

    let test_files = read_test_list(list_file, msg_list);
    if test_files.is_empty() {
        msg_list.push(
            "No test files found in test list".to_owned(),
            None,
            None,
            MessageType::Error,
        );
        print_messages(msg_list);
        return Err(1_i32);
    }

    println!("Running {} tests from {list_file}...\n", test_files.len());

    let mut results: Vec<BatchTestResult> = Vec::new();

    for (index, test_file) in test_files.iter().enumerate() {
        println!("--- [{}/{}] {} ---", index + 1, test_files.len(), test_file);

        // Fresh message list for each test to avoid error accumulation
        let mut test_msg_list = MsgList::new();

        // Assemble the test file
        let bin_string = match assemble_file(test_file, oplist, macro_list, &mut test_msg_list) {
            Some(bin) => bin,
            None => {
                println!("  SKIP: assembly failed");
                print_messages(&test_msg_list);
                results.push(BatchTestResult {
                    file_name: test_file.clone(),
                    passed: 0,
                    failed: 0,
                    timed_out: false,
                    total: 0,
                    skipped: true,
                    skip_reason: "assembly error".to_owned(),
                });
                println!();
                continue;
            }
        };

        // Write binary file
        let binary_file_name = format!("{}.kbt", filename_stem(test_file));
        write_binary_file(&mut test_msg_list, &binary_file_name, &bin_string);

        // Parse expected values
        let raw_lines: Vec<String> = std::fs::read_to_string(test_file)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect();
        let expected_values = parse_expected_uart_values(&raw_lines);

        if expected_values.is_empty() {
            println!("  SKIP: no expected UART values in source comments");
            results.push(BatchTestResult {
                file_name: test_file.clone(),
                passed: 0,
                failed: 0,
                timed_out: false,
                total: 0,
                skipped: true,
                skip_reason: "no expected values".to_owned(),
            });
            println!();
            continue;
        }

        // Send to board and keep port open
        let port = match write_to_board_keep_port(&bin_string, output_serial_port, &mut test_msg_list) {
            Ok(port) => port,
            Err(err) => {
                println!("  SKIP: serial port error \"{err}\"");
                results.push(BatchTestResult {
                    file_name: test_file.clone(),
                    passed: 0,
                    failed: 0,
                    timed_out: false,
                    total: 0,
                    skipped: true,
                    skip_reason: format!("serial error: {err}"),
                });
                println!();
                continue;
            }
        };

        // Run test verification
        let result = run_test_monitor(port, &expected_values, test_timeout, &mut test_msg_list);

        println!(
            "  Result: {}/{} passed{}",
            result.passed,
            result.total,
            if result.timed_out { " (TIMED OUT)" } else { "" },
        );

        results.push(BatchTestResult {
            file_name: test_file.clone(),
            passed: result.passed,
            failed: result.failed,
            timed_out: result.timed_out,
            total: result.total,
            skipped: false,
            skip_reason: String::new(),
        });

        println!();
    }

    // Print aggregate summary
    println!("=== Test Suite Summary ===");
    let mut total_failed: usize = 0;
    let mut total_skipped: usize = 0;
    let mut total_timed_out: usize = 0;
    let mut files_all_pass: usize = 0;

    for r in &results {
        if r.skipped {
            total_skipped += 1;
            println!("  SKIP  {} ({})", r.file_name, r.skip_reason);
        } else if r.failed > 0 || r.timed_out {
            total_failed += 1;
            if r.timed_out {
                total_timed_out += 1;
            }
            println!("  FAIL  {} ({}/{} passed{})", r.file_name, r.passed, r.total,
                if r.timed_out { ", timed out" } else { "" });
        } else {
            files_all_pass += 1;
            println!("  PASS  {} ({}/{})", r.file_name, r.passed, r.total);
        }
    }

    let total_files = results.len();
    println!(
        "\n{files_all_pass}/{total_files} test files passed, {total_failed} failed, {total_skipped} skipped{}",
        if total_timed_out > 0 { format!(", {total_timed_out} timed out") } else { String::new() },
    );

    if total_failed > 0 || total_timed_out > 0 {
        Err(2_i32)
    } else {
        Ok(())
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
            hex_code: String::from("0000001X"),
            comment: String::new(),
            variables: 0,
            registers: 1,
            section: String::new(),
        });
        opcodes.push(Opcode {
            text_name: String::from("MOV"),
            hex_code: String::from("00000020"),
            comment: String::new(),
            variables: 2,
            registers: 0,
            section: String::new(),
        });
        opcodes.push(Opcode {
            text_name: String::from("RET"),
            hex_code: String::from("00000030"),
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
        assert_eq!(pass1.first().unwrap_or_default().program_counter, 0);
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
            hex_code: String::from("0000001X"),
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
        assert_eq!(msg_list.list.first().unwrap_or_default().text, "Error Test_not_code_line");
    }

    #[allow(clippy::too_many_lines, reason = "Test function requires many lines to cover all test cases")]
    #[test]
    // Test get_pass2 for correct vector returned, with correct opcodes, registers and variables
    fn test_get_pass2_1() {
        let mut msg_list = MsgList::new();
        let labels = Vec::<Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("0000001X"),
            comment: String::new(),
            variables: 0,
            registers: 1,
            section: String::new(),
        });
        opcodes.push(Opcode {
            text_name: String::from("MOVR"),
            hex_code: String::from("0000007X"),
            comment: String::new(),
            variables: 1,
            registers: 1,
            section: String::new(),
        });
        opcodes.push(Opcode {
            text_name: String::from("MOV"),
            hex_code: String::from("00000020"),
            comment: String::new(),
            variables: 2,
            registers: 0,
            section: String::new(),
        });
        opcodes.push(Opcode {
            text_name: String::from("RET"),
            hex_code: String::from("00000030"),
            comment: String::new(),
            variables: 0,
            registers: 0,
            section: String::new(),
        });
        opcodes.push(Opcode {
            text_name: String::from("DELAY"),
            hex_code: String::from("00000040"),
            comment: String::new(),
            variables: 1,
            registers: 0,
            section: String::new(),
        });

        opcodes.push(Opcode {
            text_name: String::from("DMOV"),
            hex_code: String::from("00000AXX"),
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
        assert_eq!(pass2.first().unwrap_or_default().opcode, "00000020EEEEEEEEFFFFFFFF");
        assert_eq!(pass2.get(1).unwrap_or_default().opcode, "0000004000000007");
        assert_eq!(pass2.get(2).unwrap_or_default().opcode, "00000010");
        assert_eq!(pass2.get(3).unwrap_or_default().opcode, "00000030");
        assert_eq!(pass2.get(4).unwrap_or_default().opcode, "00000030");
        assert_eq!(pass2.get(5).unwrap_or_default().opcode, "000000720000AAAA");
        assert_eq!(pass2.get(6).unwrap_or_default().opcode, "00000A340000000A0000000B");
        assert_eq!(pass2.get(7).unwrap_or_default().opcode, "0000000248454C4C4F000000");
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
            hex_code: String::from("0000001X"),
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
        assert_eq!(pass2.first().unwrap_or_default().opcode, "ERR     ");
    }
}
