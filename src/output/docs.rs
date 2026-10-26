//! Documentation of the opcode and macro tables: HTML, JSON and the
//! `TextMate` grammar's opcode list (`--opcodes`, `--textmate`).

use crate::asm::macros::Macro;
use crate::diag::Diagnostics;
use crate::isa::Opcode;
use itertools::Itertools as _;
use std::fs::File;
use std::io::{BufWriter, Error, Write};

/// The start of the HTML page, up to its creation date.
const HTML_HEAD: &str = "<!DOCTYPE html>
<html>
<head>
<style>
#opcodes { font-family: Arial, Helvetica, sans-serif; border-collapse: collapse; width: 100%;}
#opcodes td, #opcodes th { border: 1px solid #ddd; padding: 8px;}
#opcodes tr:hover {background-color: #ddd;}
#opcodes th { padding-top: 12px; padding-bottom: 12px; text-align: left; background-color: #04AA6D; color: white;}
#macros { font-family: Arial, Helvetica, sans-serif; border-collapse: collapse; width: 100%;}
#macros td, #macros th { border: 1px solid #ddd; padding: 8px;}
#macros tr:hover {background-color: #ddd;}
#macros th { padding-top: 12px; padding-bottom: 12px; text-align: left; background-color: #3004aa; color: white;}
</style>
</head>
<body>
<h1>Klauss ISA Instruction set and macros</h1>
";

/// The opcode table's heading and header row.
const HTML_OPCODE_TABLE: &str = "<h2>Opcode Table</h2>

<table id=\"opcodes\">
<tr>
    <th>Name</th>
    <th>Opcode</th>
    <th>Variables</th>
    <th>Registers</th>
    <th>Description</th>
</tr>
";

/// The end of the opcode table, and the macro table's heading and header row.
const HTML_MACRO_TABLE: &str = "
</table><h2>Macro Table</h2>
<table id=\"macros\">
<tr>
    <th>Name</th>
    <th>Variables</th>
    <th>Description</th>
    <th>Details</th>
</tr>
";

/// The end of the macro table and of the page.
const HTML_TAIL: &str = "</table>
</body>
</html>
";

/// Create the output file `filename`, reporting a warning when it cannot be.
fn create(filename: &str, msg_list: &mut Diagnostics) -> Result<BufWriter<File>, Error> {
    let file = File::create(filename).inspect_err(|_| msg_list.warning(format!("Error opening file {filename}")))?;
    Ok(BufWriter::new(file))
}

/// Outputs the opcodes and macros as html for documentation, and as JSON.
///
/// Writes all data of opcodes and macros to `<filename_stem>.html` as
/// tables, sorted by opcode and by macro name, and the same sorted lists to
/// JSON files (see [`output_macros_opcodes_json`]).
#[cfg(not(tarpaulin_include))] // Not needed except for setting up VScode and docs
pub(crate) fn output_macros_opcodes_html(filename_stem: &str, opcodes: &[Opcode], macros: &[Macro], msg_list: &mut Diagnostics) -> Result<(), Error> {
    use chrono::Local;

    let html_filename = format!("{filename_stem}.html");
    msg_list.info(format!("Outputting macros and opcodes to {html_filename}"));
    let mut html_file = create(&html_filename, msg_list)?;

    html_file.write_all(HTML_HEAD.as_bytes())?;
    write!(html_file, "Created {}", Local::now().format("%d/%m/%Y %H:%M"))?;
    html_file.write_all(HTML_OPCODE_TABLE.as_bytes())?;

    let mut sorted_opcodes: Vec<Opcode> = opcodes.to_vec();
    sorted_opcodes.sort_by(|first, second| first.hex_code.cmp(&second.hex_code));

    let mut old_section = "";
    for opcode in &sorted_opcodes {
        if old_section != opcode.section {
            writeln!(
                html_file,
                "<tr>\n    <td colspan=\"5\" style=\"background-color:#b0b0b0;\"><b>{}</b></td>\n</tr>",
                opcode.section
            )?;
            old_section = &opcode.section;
        }
        writeln!(
            html_file,
            "<tr>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n</tr>",
            opcode.text_name, opcode.hex_code, opcode.variables, opcode.registers, opcode.comment
        )?;
    }

    html_file.write_all(HTML_MACRO_TABLE.as_bytes())?;

    let mut sorted_macros: Vec<Macro> = macros.to_vec();
    sorted_macros.sort_by(|first, second| first.name.cmp(&second.name));

    for macro_item in &sorted_macros {
        let details: String = macro_item.items.iter().flat_map(|item| ["  ", item.as_str()]).collect();
        // Each row ends without a newline, as it always has.
        write!(
            html_file,
            "<tr>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n</tr>",
            macro_item.name, macro_item.variables, macro_item.comment, details
        )?;
    }
    html_file.write_all(HTML_TAIL.as_bytes())?;
    html_file.flush()?;

    output_macros_opcodes_json(filename_stem, &sorted_opcodes, &sorted_macros, msg_list)
}

/// Outputs the opcodes and macros as JSON files.
///
/// Writes all macros and opcodes to `<filename_stem>_opcodes.json` and
/// `<filename_stem>_macros.json`.
#[cfg(not(tarpaulin_include))] // Not needed except for setting up VScode and docs
fn output_macros_opcodes_json(filename_stem: &str, opcodes: &[Opcode], macros: &[Macro], msg_list: &mut Diagnostics) -> Result<(), Error> {
    let json_opcode_filename = format!("{filename_stem}_opcodes.json");
    let json_macro_filename = format!("{filename_stem}_macros.json");

    msg_list.info(format!("Writing JSON opcode file to {json_opcode_filename}"));
    msg_list.info(format!("Writing JSON macro file to {json_macro_filename}"));

    let mut json_opcode_file = create(&json_opcode_filename, msg_list)?;
    serde_json::to_writer_pretty(&mut json_opcode_file, opcodes)?;
    json_opcode_file.flush()?;

    let mut json_macro_file = create(&json_macro_filename, msg_list)?;
    serde_json::to_writer_pretty(&mut json_macro_file, macros)?;
    json_macro_file.flush()
}

/// Output the opcodes for textmate to given filename.
///
/// Writes the opcode names, separated by `|`, to `<filename_stem>_textmate.txt`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn output_opcodes_textmate(filename_stem: &str, opcodes: &[Opcode], msg_list: &mut Diagnostics) -> Result<(), Error> {
    let textmate_opcode_filename = format!("{filename_stem}_textmate.txt");
    msg_list.info(format!("Writing textmate opcode file to {textmate_opcode_filename}"));

    let mut textmate_file = create(&textmate_opcode_filename, msg_list)?;
    write!(textmate_file, "{}", opcodes.iter().map(|opcode| opcode.text_name.as_str()).join("|"))?;
    textmate_file.flush()
}
