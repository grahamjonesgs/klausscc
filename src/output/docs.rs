//! Documentation of the opcode and macro tables: HTML, JSON and the
//! `TextMate` grammar's opcode list (`--opcodes`, `--textmate`).

use crate::asm::macros::Macro;
use crate::diag::Diagnostics;
use crate::isa::Opcode;
use std::fs::File;
use std::io::{Error, Write as _};

/// Outputs the opcodes and macros as html for documentation.
///
/// Writes all data of opcodes and macros to html as tables.
#[cfg(not(tarpaulin_include))] // Not needed except for setting up VScode and docs
pub(crate) fn output_macros_opcodes_html(
    filename_stem: String,
    opcodes: &[Opcode],
    macros: &[Macro],
    msg_list: &mut Diagnostics,
    opcodes_flag: bool,
    textmate_flag: bool,
) -> Result<(), Error> {
    use chrono::Local;

    let mut html_filename = filename_stem.clone();
    html_filename.push_str(".html");

    if opcodes_flag {
        msg_list.info(format!("Outputting macros and opcodes to {html_filename}"));

        // Open the html file
        let html_output_file = File::create(html_filename.clone());
        if html_output_file.is_err() {
            msg_list.warning(format!("Error opening file {html_filename}"));
            return Err(html_output_file.err().unwrap_or_else(|| Error::other("Unknown error")));
        }
        let Ok(mut html_file) = html_output_file else {
            return Err(Error::other("Unknown error"));
        };
        html_file.write_all(b"<!DOCTYPE html>\n")?;
        html_file.write_all(b"<html>\n<head>\n<style>\n")?;
        html_file.write_all(b"#opcodes { font-family: Arial, Helvetica, sans-serif; border-collapse: collapse; width: 100%;}\n")?;
        html_file.write_all(b"#opcodes td, #opcodes th { border: 1px solid #ddd; padding: 8px;}\n")?;
        //file.write_all(b"#opcodes tr:nth-child(even){background-color: #f2f2f2;}\n")?; // Banded table
        html_file.write_all(b"#opcodes tr:hover {background-color: #ddd;}\n")?;
        html_file
            .write_all(b"#opcodes th { padding-top: 12px; padding-bottom: 12px; text-align: left; background-color: #04AA6D; color: white;}\n")?;
        html_file.write_all(b"#macros { font-family: Arial, Helvetica, sans-serif; border-collapse: collapse; width: 100%;}\n")?;
        html_file.write_all(b"#macros td, #macros th { border: 1px solid #ddd; padding: 8px;}\n")?;
        //file.write_all(b"#macros tr:nth-child(even){background-color: #f2f2f2;}\n");  // Banded table
        html_file.write_all(b"#macros tr:hover {background-color: #ddd;}\n")?;
        html_file
            .write_all(b"#macros th { padding-top: 12px; padding-bottom: 12px; text-align: left; background-color: #3004aa; color: white;}\n")?;
        html_file.write_all(b"</style>\n</head>\n<body>\n")?;
        html_file.write_all(b"<h1>Klauss ISA Instruction set and macros</h1>\n")?;
        html_file.write_all(format!("Created {}", Local::now().format("%d/%m/%Y %H:%M")).as_bytes())?;
        html_file.write_all(b"<h2>Opcode Table</h2>\n\n<table id=\"opcodes\">\n")?;
        html_file.write_all(
            b"<tr>\n    <th>Name</th>\n    <th>Opcode</th>\n    <th>Variables</th>\n    <th>Registers</th>\n    <th>Description</th>\n</tr>\n",
        )?;

        let mut sorted_opcodes: Vec<Opcode> = opcodes.to_vec();
        sorted_opcodes.sort_by(|first: &Opcode, second: &Opcode| first.hex_code.cmp(&second.hex_code));

        let mut old_section = String::default();
        for opcode in &sorted_opcodes {
            if old_section != opcode.section {
                html_file.write_all(
                    format!(
                        "<tr>\n    <td colspan=\"5\" style=\"background-color:#b0b0b0;\"><b>{}</b></td>\n</tr>\n",
                        opcode.section
                    )
                    .as_bytes(),
                )?;
                old_section.clone_from(&opcode.section);
            }
            html_file.write_all(
                format!(
                    "<tr>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n</tr>\n",
                    opcode.text_name, opcode.hex_code, opcode.variables, opcode.registers, opcode.comment
                )
                .as_bytes(),
            )?;
        }

        html_file.write_all(b"\n</table><h2>Macro Table</h2>\n<table id=\"macros\">\n")?;
        html_file.write_all(b"<tr>\n    <th>Name</th>\n    <th>Variables</th>\n    <th>Description</th>\n    <th>Details</th>\n</tr>\n")?;

        let mut sorted_macros: Vec<Macro> = macros.to_vec();
        sorted_macros.sort_by(|first, second| first.name.cmp(&second.name));

        for macro_item in &sorted_macros {
            html_file.write_all(
                format!(
                    "<tr>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n    <td>{}</td>\n</tr>\n",
                    macro_item.name,
                    macro_item.variables,
                    macro_item.comment,
                    macro_item.items.iter().fold(String::default(), |mut cur, nxt| {
                        cur.push_str("  ");
                        cur.push_str(nxt);
                        cur
                    })
                )
                .trim()
                .as_bytes(),
            )?;
        }
        html_file.write_all(b"</table>\n")?;
        html_file.write_all(b"</body>\n</html>\n")?;

        output_macros_opcodes_json(filename_stem.clone(), &sorted_opcodes, &sorted_macros, msg_list)?;
    }
    // Write out the Textmate
    if textmate_flag {
        output_opcodes_textmate(filename_stem, opcodes, msg_list)?;
    }
    Ok(())
}

/// Outputs the opcodes and macros as JSON files.
///
/// Writes all macros and opcodes to JSON files.
#[cfg(not(tarpaulin_include))] // Not needed except for setting up VScode and docs
pub(crate) fn output_macros_opcodes_json(
    filename_stem: String,
    opcodes: &[Opcode],
    macros: &[Macro],
    msg_list: &mut Diagnostics,
) -> Result<(), Error> {
    let mut json_opcode_filename = filename_stem.clone();
    json_opcode_filename.push_str("_opcodes.json");
    let mut json_macro_filename = filename_stem;
    json_macro_filename.push_str("_macros.json");

    msg_list.info(format!("Writing JSON opcode file to {json_opcode_filename}"));

    msg_list.info(format!("Writing JSON macro file to {json_macro_filename}"));

    // Write out the JSON opcode file
    let json_opcode_output_file = File::create(json_opcode_filename.clone());
    if json_opcode_output_file.is_err() {
        msg_list.warning(format!("Error opening file {json_opcode_filename}"));
        return Err(json_opcode_output_file.err().unwrap_or_else(|| Error::other("Unknown error")));
    }
    let Ok(mut json_opcode_file) = json_opcode_output_file else {
        return Err(Error::other("Unknown error"));
    };
    json_opcode_file.write_all(serde_json::to_string_pretty(&opcodes).unwrap_or_default().as_bytes())?;

    // Write out the JSON macro file
    let json_macro_output_file = File::create(json_macro_filename.clone());
    if json_macro_output_file.is_err() {
        msg_list.warning(format!("Error opening file {json_macro_filename}"));
        return Err(json_macro_output_file.err().unwrap_or_else(|| Error::other("Unknown error")));
    }
    let Ok(mut json_macro_file) = json_macro_output_file else {
        return Err(Error::other("Unknown error"));
    };
    json_macro_file.write_all(serde_json::to_string_pretty(&macros).unwrap_or_default().as_bytes())?;
    Ok(())
}

/// Output the opcodes for textmate to given filename.
///
/// Writes all data of opcodes to textmate file.
#[cfg(not(tarpaulin_include))]
fn output_opcodes_textmate(filename_stem: String, opcodes: &[Opcode], msg_list: &mut Diagnostics) -> Result<(), Error> {
    let mut textmate_opcode_filename = filename_stem;
    textmate_opcode_filename.push_str("_textmate.txt");
    msg_list.info(format!("Writing textmate opcode file to {textmate_opcode_filename}"));

    let textmate_opcode_output_file = File::create(textmate_opcode_filename.clone());
    if textmate_opcode_output_file.is_err() {
        msg_list.warning(format!("Error opening file {textmate_opcode_filename}"));
        return Err(textmate_opcode_output_file.err().unwrap_or_else(|| Error::other("Unknown error")));
    }
    let Ok(mut json_opcode_file) = textmate_opcode_output_file else {
        return Err(Error::other("Unknown error"));
    };
    match json_opcode_file.write_all(
        opcodes
            .iter()
            .fold(String::default(), |mut cur, nxt| {
                cur.push('|');
                cur.push_str(&nxt.text_name);
                cur
            })
            .strip_prefix('|')
            .unwrap_or("")
            .as_bytes(),
    ) {
        Ok(()) => {}
        Err(err) => return Err(err),
    }

    Ok(())
}
