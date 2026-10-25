use crate::helper::{heap_start, HEAP_HEADER_WORDS};
use crate::macros::Macro;
use crate::messages::{MessageType, MsgList};
use crate::opcodes::{InputData, Opcode, Pass2};
use std::io::Error;

use std::collections::HashSet;
use std::ffi::OsStr;
use std::{
    fs::{self, File},
    io::{prelude::*, BufReader},
    path::{Path, PathBuf},
};

#[derive(PartialEq, Eq, Debug, Clone)]
/// Defines the type of line.
pub enum LineType {
    Blank,
    Comment,
    Data,
    Error,
    Label,
    Opcode,
    Start,
}

/// Return the stem of given filename.
///
/// Looks for first dot in the string, and returns the slice before the dot.
pub fn filename_stem(full_name: &String) -> String {
    let path = Path::new(full_name);
    let stem = path.file_stem();
    let parent = path
        .parent()
        .unwrap_or_else(|| Path::new(""))
        .join(stem.unwrap_or_else(|| OsStr::new("")));

    parent.to_str().unwrap_or_default().to_owned()
}

/// Name an output file with the given extension (without the dot).
///
/// An explicit name (from `-o` / `-b`) is used as given, with the extension
/// appended unless it already ends with it (so `-b out.kbt` stays `out.kbt`).
/// Without one, the extension replaces the input file's.
pub fn output_file_name(explicit: Option<&str>, input_file_name: &str, extension: &str) -> String {
    let suffix = format!(".{extension}");
    match explicit {
        Some(name) if name.to_ascii_lowercase().ends_with(&suffix.to_ascii_lowercase()) => name.to_owned(),
        Some(name) => format!("{name}{suffix}"),
        None => format!("{}{suffix}", filename_stem(&input_file_name.to_owned())),
    }
}

/// Format a given string, adding spaces between groups of 4.
///
/// For string of 8 and 12 charters adds spaces between groups of 4 characters, otherwise returns original string.
pub fn format_opcodes(input: &String) -> String {
    if input.len() == 4 {
        return format!("{input}              ");
    }
    if input.len() == 8 {
        let mut result = input.get(0..4).unwrap_or("    ").to_owned();
        result.push_str(input.get(4..8).unwrap_or("    "));
        result.push_str("         ");
        return result;
    }
    if input.len() == 16 {
        let mut result = input.get(0..4).unwrap_or("    ").to_owned();
        result.push_str(input.get(4..8).unwrap_or("    "));
        result.push(' ');
        result.push_str(input.get(8..12).unwrap_or("    "));
        result.push_str(input.get(12..16).unwrap_or("    "));
        return result;
    }
    // 12-byte instruction: 32-bit opcode + 64-bit immediate (lo32 + hi32), e.g. SETR64
    if input.len() == 24 {
        let mut result = input.get(0..4).unwrap_or("    ").to_owned();
        result.push_str(input.get(4..8).unwrap_or("    "));
        result.push(' ');
        result.push_str(input.get(8..12).unwrap_or("    "));
        result.push_str(input.get(12..16).unwrap_or("    "));
        result.push(' ');
        result.push_str(input.get(16..20).unwrap_or("    "));
        result.push_str(input.get(20..24).unwrap_or("    "));
        return result;
    }
    (*input).clone()
}

/// Return the file named by an `!include` line.
///
/// `None` if the line is not an `!include` (the first non-blank text must be
/// `!include`); otherwise the first word after it, before any `//` comment,
/// which is empty when the name is missing.
pub fn include_target(input_line: &str) -> Option<&str> {
    let rest = input_line.trim_start().strip_prefix("!include")?;
    let rest = rest.find("//").map_or(rest, |comment| &rest[..comment]);
    Some(rest.split_whitespace().next().unwrap_or(""))
}

/// Outputs the opcodes and macros as html for documentation.
///
/// Writes all data of opcodes and macros to html as tables.
#[cfg(not(tarpaulin_include))] // Not needed except for setting up VScode and docs
pub fn output_macros_opcodes_html(
    filename_stem: String,
    opcodes: &[Opcode],
    macros: Vec<Macro>,
    msg_list: &mut MsgList,
    opcodes_flag: bool,
    textmate_flag: bool,
) -> Result<(), Error> {
    use chrono::Local;

    let mut html_filename = filename_stem.clone();
    html_filename.push_str(".html");

    if opcodes_flag {
        msg_list.push(
            format!("Outputting macros and opcodes to {html_filename}"),
            None,
            None,
            MessageType::Information,
        );

        // Open the html file
        let html_output_file = File::create(html_filename.clone());
        if html_output_file.is_err() {
            msg_list.push(format!("Error opening file {html_filename}"), None, None, MessageType::Warning);
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
        for opcode in sorted_opcodes.clone() {
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

        let mut sorted_macros: Vec<Macro> = macros;
        sorted_macros.sort_by(|first, second| first.name.cmp(&second.name));

        for macro_item in sorted_macros.clone() {
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
pub fn output_macros_opcodes_json(filename_stem: String, opcodes: &[Opcode], macros: &[Macro], msg_list: &mut MsgList) -> Result<(), Error> {
    let mut json_opcode_filename = filename_stem.clone();
    json_opcode_filename.push_str("_opcodes.json");
    let mut json_macro_filename = filename_stem;
    json_macro_filename.push_str("_macros.json");

    msg_list.push(
        format!("Writing JSON opcode file to {json_opcode_filename}"),
        None,
        None,
        MessageType::Information,
    );

    msg_list.push(
        format!("Writing JSON macro file to {json_macro_filename}"),
        None,
        None,
        MessageType::Information,
    );

    // Write out the JSON opcode file
    let json_opcode_output_file = File::create(json_opcode_filename.clone());
    if json_opcode_output_file.is_err() {
        msg_list.push(format!("Error opening file {json_opcode_filename}"), None, None, MessageType::Warning);
        return Err(json_opcode_output_file.err().unwrap_or_else(|| Error::other("Unknown error")));
    }
    let Ok(mut json_opcode_file) = json_opcode_output_file else {
        return Err(Error::other("Unknown error"));
    };
    json_opcode_file.write_all(serde_json::to_string_pretty(&opcodes).unwrap_or_default().as_bytes())?;

    // Write out the JSON macro file
    let json_macro_output_file = File::create(json_macro_filename.clone());
    if json_macro_output_file.is_err() {
        msg_list.push(format!("Error opening file {json_macro_filename}"), None, None, MessageType::Warning);
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
fn output_opcodes_textmate(filename_stem: String, opcodes: &[Opcode], msg_list: &mut MsgList) -> Result<(), Error> {
    let mut textmate_opcode_filename = filename_stem;
    textmate_opcode_filename.push_str("_textmate.txt");
    msg_list.push(
        format!("Writing textmate opcode file to {textmate_opcode_filename}"),
        None,
        None,
        MessageType::Information,
    );

    let textmate_opcode_output_file = File::create(textmate_opcode_filename.clone());
    if textmate_opcode_output_file.is_err() {
        msg_list.push(format!("Error opening file {textmate_opcode_filename}"), None, None, MessageType::Warning);
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

/// Open text file and return as vector of strings.
///
/// Reads any given file by filename, adding the fill line by line into vector and returns None or Some(String). Manages included files.
pub fn read_file_to_vector(filename: &str, msg_list: &mut MsgList) -> Option<Vec<InputData>> {
    read_source_file(filename, msg_list, &mut HashSet::new())
}

/// Read one source file, expanding its `!include` lines.
///
/// `open_files` holds the canonical paths of the files currently being read
/// (this one's includers); a file already in it is a recursive include. The
/// path is compared before the file is opened, and always removed again
/// afterwards, however reading the file ends.
fn read_source_file(filename: &str, msg_list: &mut MsgList, open_files: &mut HashSet<PathBuf>) -> Option<Vec<InputData>> {
    let Ok(canonical) = fs::canonicalize(filename) else {
        msg_list.push(format!("Unable to open file {filename}"), None, None, MessageType::Error);
        return None;
    };
    if open_files.contains(&canonical) {
        msg_list.push(format!("Recursive include of file {filename}"), None, None, MessageType::Error);
        return None;
    }
    let Ok(file) = File::open(&canonical) else {
        msg_list.push(format!("Unable to open file {filename}"), None, None, MessageType::Error);
        return None;
    };

    open_files.insert(canonical.clone());
    let lines = read_source_lines(file, filename, msg_list, open_files);
    open_files.remove(&canonical);
    lines
}

/// Read the lines of an open source file, expanding its `!include` lines.
fn read_source_lines(file: File, filename: &str, msg_list: &mut MsgList, open_files: &mut HashSet<PathBuf>) -> Option<Vec<InputData>> {
    let buf = BufReader::new(file);
    let mut lines: Vec<InputData> = Vec::new();

    let mut line_number = 0;
    for line in buf.lines() {
        match line {
            Ok(line_contents) => {
                line_number += 1;
                if let Some(include_file) = include_target(&line_contents) {
                    if include_file.is_empty() {
                        msg_list.push(
                            format!("Missing include file name in {filename}"),
                            Some(line_number),
                            Some(filename.to_owned()),
                            MessageType::Error,
                        );
                        return None;
                    }

                    // Get the include file from the same directory as the previous file
                    let parent = Path::new(filename).parent().unwrap_or_else(|| Path::new(""));
                    let new_include_file = parent.join(include_file).to_string_lossy().into_owned();

                    let Some(include_lines) = read_source_file(&new_include_file, msg_list, open_files) else {
                        msg_list.push(
                            format!("Unable to open include file {new_include_file} in {filename}"),
                            Some(line_number),
                            Some(filename.to_owned()),
                            MessageType::Error,
                        );
                        return Some(lines);
                    };
                    lines.extend(include_lines);
                } else {
                    lines.push(InputData {
                        input: line_contents,
                        file_name: filename.to_owned(),
                        line_counter: line_number,
                    });
                }
            }
            #[cfg(not(tarpaulin_include))] // Cannot test error reading file line in tarpaulin
            Err(err) => msg_list.push(
                format!("Error reading source file {filename}: {err}"),
                Some(line_number),
                Some(filename.to_owned()),
                MessageType::Error,
            ),
        }
    }
    Some(lines)
}

/// Scanner state for `remove_block_comments`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CommentState {
    /// Ordinary source text.
    Code,
    /// Source text, and the last character was a `/` that has not been output yet.
    Slash,
    /// Inside a `/* */` comment.
    Comment,
    /// Inside a comment, and the last character was a `*`.
    Star,
}

/// Report a block comment that was still open at the end of a file.
fn unterminated_comment(file_name: &str, line_number: u32, msg_list: &mut MsgList) {
    msg_list.push(
        format!("Comment not terminated in file {file_name}"),
        Some(line_number),
        Some(file_name.to_owned()),
        MessageType::Error,
    );
}

/// Remove comments from vector of strings.
///
/// Checks for /* */ and removes them from the vector of strings. Comments do
/// not nest, may span lines, but never span files: a comment still open at the
/// end of a file is reported at the line where its `/*` was.
pub fn remove_block_comments(lines: Vec<InputData>, msg_list: &mut MsgList) -> Vec<InputData> {
    let mut state = CommentState::Code;
    // File and line of the `/*` that opened the current comment.
    let mut opened_at: (String, u32) = (String::new(), 0);
    let mut new_lines: Vec<InputData> = Vec::new();
    for line in lines {
        let in_comment = matches!(state, CommentState::Comment | CommentState::Star);
        if in_comment && opened_at.0 != line.file_name {
            unterminated_comment(&opened_at.0, opened_at.1, msg_list);
            state = CommentState::Code;
        }

        let mut new_line = String::default();
        for char in line.input.chars() {
            state = match (state, char) {
                (CommentState::Code, '/') => CommentState::Slash,
                (CommentState::Code, _) => {
                    new_line.push(char);
                    CommentState::Code
                }
                (CommentState::Slash, '*') => {
                    opened_at = (line.file_name.clone(), line.line_counter);
                    CommentState::Comment
                }
                (CommentState::Slash, '/') => {
                    new_line.push('/');
                    CommentState::Slash
                }
                (CommentState::Slash, _) => {
                    new_line.push('/');
                    new_line.push(char);
                    CommentState::Code
                }
                (CommentState::Comment | CommentState::Star, '*') => CommentState::Star,
                (CommentState::Star, '/') => CommentState::Code,
                (CommentState::Comment | CommentState::Star, _) => CommentState::Comment,
            };
        }
        // A pending `/` at the end of a line is ordinary text, and a `*` that
        // ends a line cannot pair with a `/` on the next one.
        state = match state {
            CommentState::Slash => {
                new_line.push('/');
                CommentState::Code
            }
            CommentState::Star => CommentState::Comment,
            other => other,
        };
        new_lines.push(InputData {
            input: new_line,
            file_name: line.file_name,
            line_counter: line.line_counter,
        });
    }
    if state == CommentState::Comment {
        unterminated_comment(&opened_at.0, opened_at.1, msg_list);
    }
    new_lines
}

/// Output the bitcode to given file.
///
/// Based on the bitcode string outputs to file.
pub fn write_binary_output_file(filename: &impl AsRef<Path>, output_string: &str) -> Result<(), Error> {
    match File::create(filename) {
        Ok(mut file) => {
            match file.write_all(output_string.as_bytes()) {
                Ok(()) => {}
                #[cfg(not(tarpaulin_include))] // Can't test error writing to files
                Err(err) => return Err(err),
            }
        }
        #[cfg(not(tarpaulin_include))] // Can't test error creating
        Err(err) => {
            // code to execute if File::create returned an error
            return Err(err);
        }
    }

    Ok(())
}

/// Output the code details file to given filename.
///
/// Writes all data to the detailed code file.
pub fn write_code_output_file(filename: impl AsRef<Path> + Copy, pass2: &mut Vec<Pass2>, msg_list: &mut MsgList) -> Result<(), Error> {
    let mut file = match File::create(filename) {
        Ok(file) => file,
        #[cfg(not(tarpaulin_include))] // Can't test error creating file
        Err(err) => return Err(err),
    };

    let mut out_line = String::default();
    msg_list.push(
        format!("Writing code file to {}", filename.as_ref().display()),
        None,
        None,
        MessageType::Information,
    );
    // Compute heap_start from the first byte after the program, as the .kbt does.
    // program_counter is already a byte address; opcode.len()/8 = words, *4 = bytes → /2 total
    let program_end: u32 = pass2
        .iter()
        .filter(|p| p.line_type == LineType::Opcode || p.line_type == LineType::Data)
        .map(|p| p.program_counter.saturating_add(p.opcode.len() as u32 / 2))
        .max()
        .unwrap_or(HEAP_HEADER_WORDS * 8);
    let heap_start = heap_start(program_end);

    // Emit the 4 reserved heap header words before the assembled code
    let heap_header = [
        (format!("{heap_start:016X}"), "heap_start  (set by assembler)"),
        ("0000000000000000".to_owned(), "heap_end    (reserved)"),
        ("0000000000000000".to_owned(), "(reserved)"),
        ("0000000000000000".to_owned(), "(reserved)"),
    ];
    for (i, (value, comment)) in heap_header.iter().enumerate() {
        match file.write_all(format!("0x{:08X}: {value:<33} -- {comment}\n", i * 8).as_bytes()) {
            Ok(()) => {}
            #[cfg(not(tarpaulin_include))]
            Err(err) => return Err(err),
        }
    }

    for pass in pass2 {
        out_line.clear();
        if pass.line_type == LineType::Opcode {
            out_line = format!(
                "0x{:08X}: {:<17} -- {}\n",
                pass.program_counter,
                format_opcodes(&pass.opcode),
                pass.input_text_line
            );
        } else if pass.line_type == LineType::Data {
            // One row per 64-bit word; a final part word gets a row of its own.
            for (chunk, n) in pass.opcode.as_bytes().chunks(16).zip(0_u32..) {
                out_line.push_str(
                    format!(
                        "0x{:08X}: {:<32}  -- {}\n",
                        pass.program_counter + n * 8,
                        std::str::from_utf8(chunk).unwrap_or_default(),
                        pass.input_text_line
                    )
                    .as_str(),
                );
            }
        } else if pass.line_type == LineType::Label {
            out_line = format!("0x{:08X}:                   -- {}\n", pass.program_counter, pass.input_text_line);
        } else if pass.line_type == LineType::Error {
            out_line = format!("Error                         -- {}\n", pass.input_text_line);
        } else {
            out_line = format!("                              -- {}\n", pass.input_text_line);
        }
        match file.write_all(out_line.as_bytes()) {
            Ok(()) => {}
            #[cfg(not(tarpaulin_include))] // Can't test error writing to file
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use std::fs;
    use std::path::MAIN_SEPARATOR_STR;

    use super::*;
    use tempfile::TempDir;

    #[test]
    // Test remove of comments in single line
    fn test_remove_block_comments1() {
        let input = vec![InputData {
            input: "abc/* This is a comment */def".to_owned(),
            file_name: "test.kla".to_owned(),
            line_counter: 1,
        }];
        let output = remove_block_comments(input, &mut MsgList::new());
        assert_eq!(
            output,
            vec![InputData {
                input: "abcdef".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 1,
            }]
        );
    }

    #[test]
    // Test remove of comments in two lines
    fn test_remove_block_comments2() {
        let input = vec![
            InputData {
                input: "abc/* This is a comment */def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "abc/* This is a comment */def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 2,
            },
        ];
        let output = remove_block_comments(input, &mut MsgList::new());
        assert_eq!(
            output,
            vec![
                InputData {
                    input: "abcdef".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 1,
                },
                InputData {
                    input: "abcdef".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 2
                },
            ]
        );
    }

    #[test]
    // Test remove of comments in across two lines
    fn test_remove_block_comments3() {
        let input = vec![
            InputData {
                input: "abc/* This is a comment ".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "so is this */def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 2,
            },
        ];
        let output = remove_block_comments(input, &mut MsgList::new());
        assert_eq!(
            output,
            vec![
                InputData {
                    input: "abc".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 1,
                },
                InputData {
                    input: "def".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 2,
                },
            ]
        );
    }

    #[test]
    // Test remove of comments in across three line with blank line left
    fn test_remove_block_comments4() {
        let input = vec![
            InputData {
                input: "abc/* This is a comment ".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "so is this def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 2,
            },
            InputData {
                input: "*/def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 3,
            },
        ];
        let output = remove_block_comments(input, &mut MsgList::new());
        assert_eq!(
            output,
            vec![
                InputData {
                    input: "abc".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 1,
                },
                InputData {
                    input: String::default(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 2,
                },
                InputData {
                    input: "def".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 3,
                },
            ]
        );
    }

    #[test]
    // Test restart comments
    fn test_remove_block_comments5() {
        let input = vec![
            InputData {
                input: "abc/* This is a /* /*comment ".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "so is this def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 2,
            },
            InputData {
                input: "*/def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 3,
            },
        ];
        let output = remove_block_comments(input, &mut MsgList::new());
        assert_eq!(
            output,
            vec![
                InputData {
                    input: "abc".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 1,
                },
                InputData {
                    input: String::default(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 2,
                },
                InputData {
                    input: "def".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 3,
                },
            ]
        );
    }

    #[test]
    // Test comments are closed at end of file
    fn test_remove_block_comments6() {
        let msg_list = &mut MsgList::new();
        let input = vec![
            InputData {
                input: "abc/* This is a /* /*comment ".to_owned(),
                file_name: "test1.kla".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "so is this def".to_owned(),
                file_name: "test1.kla".to_owned(),
                line_counter: 2,
            },
            InputData {
                input: "def".to_owned(),
                file_name: "test2.kla".to_owned(),
                line_counter: 3,
            },
        ];
        let output = remove_block_comments(input, msg_list);
        assert_eq!(
            output,
            vec![
                InputData {
                    input: "abc".to_owned(),
                    file_name: "test1.kla".to_owned(),
                    line_counter: 1,
                },
                InputData {
                    input: String::default(),
                    file_name: "test1.kla".to_owned(),
                    line_counter: 2,
                },
                InputData {
                    input: "def".to_owned(),
                    file_name: "test2.kla".to_owned(),
                    line_counter: 3,
                },
            ]
        );
        assert_eq!(msg_list.list.first().unwrap_or_default().text, "Comment not terminated in file test1.kla");
        // Reported where the comment opened, not at the first line of the next file.
        assert_eq!(msg_list.list.first().unwrap_or_default().line_number, Some(1));
        assert_eq!(msg_list.list.first().unwrap_or_default().file_name, Some("test1.kla".to_owned()));
    }

    /// Run `remove_block_comments` over `lines` (all in `test.kla`).
    fn strip_block(lines: &[&str]) -> (Vec<String>, MsgList) {
        let mut msg_list = MsgList::new();
        let input = lines
            .iter()
            .zip(1..)
            .map(|(line, n)| InputData {
                input: (*line).to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: n,
            })
            .collect();
        let output = remove_block_comments(input, &mut msg_list).into_iter().map(|l| l.input).collect();
        (output, msg_list)
    }

    #[test]
    // `/*/` opens a comment; the `/` after the `*` does not also close it
    fn test_remove_block_comments_slash_star_slash() {
        let (output, msg_list) = strip_block(&["abc/*/def"]);
        assert_eq!(output, vec!["abc"]);
        assert_eq!(msg_list.number_by_type(&MessageType::Error), 1);
    }

    #[test]
    // A `*` after a comment closes is ordinary text
    fn test_remove_block_comments_star_after_close() {
        let (output, msg_list) = strip_block(&["a/* x */*b", "c"]);
        assert_eq!(output, vec!["a*b", "c"]);
        assert_eq!(msg_list.number_by_type(&MessageType::Error), 0);
    }

    #[test]
    // Comments do not nest, and `**/` still closes one
    fn test_remove_block_comments_nested_looking() {
        let (output, msg_list) = strip_block(&["abc/* /* x */def", "g/* x */ */h", "i/* x **/j"]);
        assert_eq!(output, vec!["abcdef", "g */h", "ij"]);
        assert_eq!(msg_list.number_by_type(&MessageType::Error), 0);
    }

    #[test]
    // Slashes that do not open a comment are kept
    fn test_remove_block_comments_plain_slashes() {
        let (output, msg_list) = strip_block(&["a/b", "c//d", "e/", "*/f"]);
        assert_eq!(output, vec!["a/b", "c//d", "e/", "*/f"]);
        assert_eq!(msg_list.number_by_type(&MessageType::Error), 0);
    }

    #[test]
    // A `*` ending a line does not pair with a `/` starting the next
    fn test_remove_block_comments_star_at_line_end() {
        let (output, msg_list) = strip_block(&["a/* x *", "/b */c"]);
        assert_eq!(output, vec!["a", "c"]);
        assert_eq!(msg_list.number_by_type(&MessageType::Error), 0);
    }

    #[test]
    // A comment still open at the end of the last file is reported where it opened
    fn test_remove_block_comments_unterminated_last_file() {
        let (output, msg_list) = strip_block(&["abc", "def /* open", "ghi"]);
        assert_eq!(output, vec!["abc", "def ", ""]);
        assert_eq!(msg_list.number_by_type(&MessageType::Error), 1);
        let msg = msg_list.list.first().unwrap_or_default();
        assert_eq!(msg.text, "Comment not terminated in file test.kla");
        assert_eq!(msg.line_number, Some(2));
        assert_eq!(msg.file_name, Some("test.kla".to_owned()));
    }

    #[test]
    // Tests for include_target recognising !include as first non white-space
    fn test_include_target_is_include() {
        assert_eq!(include_target("!include file.type"), Some("file.type"));
        assert_eq!(include_target("!include"), Some(""));
        assert_eq!(include_target("    !include   123 234 456"), Some("123"));
    }

    #[test]
    // Tests for lines that are not !include
    fn test_include_target_not_include() {
        assert_eq!(include_target("include file.type"), None);
        assert_eq!(include_target("!!include"), None);
        assert_eq!(include_target("test_line"), None);
    }

    #[test]
    // Check include_target returns correct filename from !include
    fn test_include_target_filename() {
        assert_eq!(include_target("!include myfile.name"), Some("myfile.name"));
        assert_eq!(include_target("!include myfile.name extra words"), Some("myfile.name"));
        assert_eq!(include_target("!include //test comment"), Some(""));
        assert_eq!(include_target("!include lib.kla // helpers"), Some("lib.kla"));
        // Only the leading directive is removed, not every occurrence of it.
        assert_eq!(include_target("!include my!include.kla"), Some("my!include.kla"));
    }

    #[test]
    // Check correct filename stem is returned
    fn test_filename_stem() {
        assert_eq!(filename_stem(&"file.type".to_owned()), "file");
        assert_eq!(filename_stem(&"file".to_owned()), "file");
        assert_eq!(
            filename_stem(&format!("{MAIN_SEPARATOR_STR}my_path{MAIN_SEPARATOR_STR}file.kla")),
            format!("{MAIN_SEPARATOR_STR}my_path{MAIN_SEPARATOR_STR}file")
        );
        assert_eq!(
            filename_stem(&format!("relative_path{MAIN_SEPARATOR_STR}file.kla")),
            format!("relative_path{MAIN_SEPARATOR_STR}file")
        );
    }

    #[test]
    fn output_file_name_appends_extension_once() {
        assert_eq!(output_file_name(Some("out"), "prog.kla", "kbt"), "out.kbt");
        assert_eq!(output_file_name(Some("out.kbt"), "prog.kla", "kbt"), "out.kbt");
        assert_eq!(output_file_name(Some("OUT.KBT"), "prog.kla", "kbt"), "OUT.KBT");
        assert_eq!(output_file_name(Some("out.code"), "prog.kla", "kbt"), "out.code.kbt");
        assert_eq!(output_file_name(Some("prog.kla"), "prog.kla", "code"), "prog.kla.code");
    }

    #[test]
    fn output_file_name_defaults_to_input_stem_and_keeps_spaces() {
        assert_eq!(output_file_name(None, "prog.kla", "kbt"), "prog.kbt");
        let input = format!("my dir{MAIN_SEPARATOR_STR}my prog.kla");
        assert_eq!(output_file_name(None, &input, "code"), format!("my dir{MAIN_SEPARATOR_STR}my prog.code"));
    }

    #[test]
    // Check for formatting of codes used for debug file
    fn test_format_opcodes() {
        assert_eq!(format_opcodes(&"0000000000000000".to_owned()), "00000000 00000000");
        assert_eq!(format_opcodes(&"0000".to_owned()), "0000              ");
        assert_eq!(format_opcodes(&"0123456789ABCDEF".to_owned()), "01234567 89ABCDEF");
        assert_eq!(format_opcodes(&"12345678".to_owned()), "12345678         ");
        assert_eq!(format_opcodes(&"123".to_owned()), "123");
    }

    #[test]
    // Test for missing file
    fn test_read_file_to_vec1() {
        let mut msg_list = MsgList::new();
        read_file_to_vector("////xxxxxxx", &mut msg_list);
        assert_eq!(msg_list.list.first().unwrap_or_default().text, "Unable to open file ////xxxxxxx");
    }

    #[test]
    // Test for simple file added
    fn test_write_file_to_vec2() {
        let mut msg_list = MsgList::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap_or_default();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        _ = writeln!(tmp_file1, "Test line in file");
        _ = writeln!(tmp_file1, "Test line in file 1");
        _ = writeln!(tmp_file1, "Test line in file 2");
        _ = writeln!(tmp_file1, "Test line in file 4");

        let lines = read_file_to_vector(file_name1, &mut msg_list);
        assert_eq!(lines.clone().unwrap().first().unwrap_or_default().input, "Test line in file");
        assert_eq!(lines.unwrap().len(), 4);

        drop(tmp_file1);
        _ = tmp_dir.close();
    }

    #[test]
    // Test for included file
    fn test_write_file_to_vec3() {
        let mut msg_list = MsgList::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        _ = writeln!(tmp_file1, "Test line in file 1 line 0");
        _ = writeln!(tmp_file1, "!include test2.kla");
        _ = writeln!(tmp_file1, "Test line in file 1 line 1");
        _ = writeln!(tmp_file1, "Test line in file 1 line 2");

        let file_path2 = tmp_dir.path().join("test2.kla");
        let mut tmp_file2: File = File::create(file_path2).unwrap();
        _ = writeln!(tmp_file2, "Test line in file 2 line 0");
        _ = writeln!(tmp_file2, "Test line in file 2 line 1");
        _ = writeln!(tmp_file2, "Test line in file 2 line 2");
        _ = writeln!(tmp_file2, "Test line in file 2 line 4");

        let lines = read_file_to_vector(file_name1, &mut msg_list);
        assert_eq!(
            lines.clone().unwrap_or_default().first().unwrap_or_default().input,
            "Test line in file 1 line 0"
        );
        assert_eq!(
            lines.clone().unwrap_or_default().get(2).unwrap_or_default().input,
            "Test line in file 2 line 1"
        );
        assert_eq!(
            lines.clone().unwrap_or_default().get(6).unwrap_or_default().input,
            "Test line in file 1 line 2"
        );
        assert_eq!(lines.unwrap().len(), 7);

        drop(tmp_file1);
        drop(tmp_file2);
        _ = tmp_dir.close();
    }

    #[test]
    // Test for recursive file includes
    fn test_write_file_to_vec4() {
        let mut msg_list = MsgList::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        _ = writeln!(tmp_file1, "Test line in file 1 line 0");
        _ = writeln!(tmp_file1, "!include test2.kla");
        _ = writeln!(tmp_file1, "Test line in file 1 line 1");
        _ = writeln!(tmp_file1, "Test line in file 1 line 2");

        let file_path2 = tmp_dir.path().join("test2.kla");
        let mut tmp_file2: File = File::create(file_path2).unwrap();
        _ = writeln!(tmp_file2, "Test line in file 2 line 0");
        _ = writeln!(tmp_file2, "Test line in file 2 line 1");
        _ = writeln!(tmp_file2, "!include test1.kla");
        _ = writeln!(tmp_file2, "Test line in file 2 line 4");

        _ = read_file_to_vector(file_name1, &mut msg_list);
        assert_eq!(
            msg_list.list.first().unwrap_or_default().text,
            format!("Recursive include of file {file_name1}")
        );

        drop(tmp_file1);
        drop(tmp_file2);
        _ = tmp_dir.close();
    }

    #[test]
    // Test for missing included file
    fn test_write_file_to_vec5() {
        let mut msg_list = MsgList::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap();
        let file_path2 = tmp_dir.path().join("test2.kla");
        let binding2 = file_path2;
        let file_name2: &str = binding2.to_str().unwrap();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        _ = writeln!(tmp_file1, "!include test2.kla");
        _ = writeln!(tmp_file1, "Test line in file 1 line 1");
        _ = writeln!(tmp_file1, "Test line in file 1 line 2");

        let lines = read_file_to_vector(file_name1, &mut msg_list);
        assert_eq!(
            msg_list.list.first().unwrap_or_default().text,
            format!("Unable to open file {file_name2}")
        );
        assert_eq!(
            msg_list.list.get(1).unwrap_or_default().text,
            format!("Unable to open include file {file_name2} in {file_name1}")
        );
        assert_eq!(lines, Some(vec![]));

        drop(tmp_file1);
        _ = tmp_dir.close();
    }
    #[test]
    // Test for missing included file name
    fn test_write_file_to_vec6() {
        let mut msg_list = MsgList::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        _ = writeln!(tmp_file1, "!include");
        _ = writeln!(tmp_file1, "Test line in file 1 line 1");
        _ = writeln!(tmp_file1, "Test line in file 1 line 2");

        let lines = read_file_to_vector(file_name1, &mut msg_list);
        assert_eq!(
            msg_list.list.first().unwrap_or_default().text,
            format!("Missing include file name in {file_name1}")
        );
        assert_eq!(lines, None);

        drop(tmp_file1);
        _ = tmp_dir.close();
    }

    #[test]
    // Test for double included file
    fn test_write_file_to_vec7() {
        let mut msg_list = MsgList::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        _ = writeln!(tmp_file1, "Test line in file 1 line 0");
        _ = writeln!(tmp_file1, "!include test2.kla");
        _ = writeln!(tmp_file1, "Test line in file 1 line 1");
        _ = writeln!(tmp_file1, "Test line in file 1 line 2");

        let file_path2 = tmp_dir.path().join("test2.kla");
        let mut tmp_file2: File = File::create(file_path2.clone()).unwrap();
        let binding2 = file_path2;
        let file_name2: &str = binding2.to_str().unwrap();
        _ = writeln!(tmp_file2, "Test line in file 2 line 0");
        _ = writeln!(tmp_file2, "!include test3.kla");
        _ = writeln!(tmp_file2, "Test line in file 2 line 2");
        _ = writeln!(tmp_file2, "Test line in file 2 line 4");

        let file_path3 = tmp_dir.path().join("test3.kla");
        let binding3 = file_path3;
        let file_name3: &str = binding3.to_str().unwrap();

        _ = read_file_to_vector(file_name1, &mut msg_list);
        assert_eq!(
            msg_list.list.first().unwrap_or_default().text,
            format!("Unable to open file {file_name3}")
        );
        assert_eq!(
            msg_list.list.get(1).unwrap_or_default().text,
            format!("Unable to open include file {file_name3} in {file_name2}")
        );

        drop(tmp_file1);
        drop(tmp_file2);
        _ = tmp_dir.close();
    }

    #[test]
    // A file including itself through a different spelling of its path is
    // caught before it is opened again, not after running out of descriptors.
    fn test_read_file_self_include_other_spelling() {
        let mut msg_list = MsgList::new();
        let tmp_dir = TempDir::new().unwrap();
        fs::create_dir(tmp_dir.path().join("sub")).unwrap();
        let file_path1 = tmp_dir.path().join("sub").join("a.kla");
        fs::write(&file_path1, "line 1\n!include ./a.kla\n!include ../sub/a.kla\nline 4\n").unwrap();
        let file_name1 = file_path1.to_str().unwrap();

        let lines = read_file_to_vector(file_name1, &mut msg_list).unwrap();
        let recursive: Vec<&str> = msg_list
            .list
            .iter()
            .filter(|m| m.text.starts_with("Recursive include"))
            .map(|m| m.text.as_str())
            .collect();
        assert_eq!(recursive.len(), 1);
        assert!(recursive[0].ends_with("a.kla"), "{recursive:?}");
        // Reading stops at the failed include, as for any include error.
        assert_eq!(lines.len(), 1);
    }

    #[test]
    // A failed include still pops the include stack, so including the same
    // file again later is not mistaken for recursion.
    fn test_read_file_failed_include_pops_stack() {
        let mut msg_list = MsgList::new();
        let tmp_dir = TempDir::new().unwrap();
        let file_path1 = tmp_dir.path().join("a.kla");
        fs::write(&file_path1, "!include b.kla\n").unwrap();
        fs::write(tmp_dir.path().join("b.kla"), "b line\n!include missing.kla\n").unwrap();
        fs::write(tmp_dir.path().join("c.kla"), "!include b.kla\n!include b.kla\n").unwrap();
        let file_name_c = tmp_dir.path().join("c.kla");

        _ = read_file_to_vector(file_path1.to_str().unwrap(), &mut msg_list);
        _ = read_file_to_vector(file_name_c.to_str().unwrap(), &mut msg_list);
        assert!(msg_list.list.iter().all(|m| !m.text.starts_with("Recursive include")));
    }

    #[test]
    // The same file may be included twice when it is not its own includer
    fn test_read_file_diamond_include() {
        let mut msg_list = MsgList::new();
        let tmp_dir = TempDir::new().unwrap();
        let file_path1 = tmp_dir.path().join("a.kla");
        fs::write(&file_path1, "!include b.kla\n!include c.kla\n").unwrap();
        fs::write(tmp_dir.path().join("b.kla"), "b\n!include d.kla\n").unwrap();
        fs::write(tmp_dir.path().join("c.kla"), "c\n!include d.kla\n").unwrap();
        fs::write(tmp_dir.path().join("d.kla"), "d\n").unwrap();

        let lines = read_file_to_vector(file_path1.to_str().unwrap(), &mut msg_list).unwrap();
        let text: Vec<&str> = lines.iter().map(|l| l.input.as_str()).collect();
        assert_eq!(text, vec!["b", "d", "c", "d"]);
        assert_eq!(msg_list.number_by_type(&MessageType::Error), 0);
    }

    #[test]
    fn test_write_binary_output_file() {
        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.klb");
        let binding = file_path1;
        let file_name1: &str = binding.to_str().unwrap();

        let result_write = write_binary_output_file(&file_name1, "12345678");
        result_write.unwrap();

        let bytes = fs::read(file_name1).unwrap();
        assert_eq!(bytes, b"12345678");
    }

    #[test]
    fn test_write_code_output_file() {
        let tmp_dir = TempDir::new().unwrap();
        let mut msg_list = MsgList::new();
        let mut pass2: Vec<Pass2> = vec![
            Pass2 {
                input_text_line: "MOV 0xEEEEEEEE 0xFFFFFFFF".to_owned(),
                file_name: String::from("test"),
                line_counter: 1,
                program_counter: 0,
                line_type: LineType::Opcode,
                opcode: String::from("x"),
            },
            Pass2 {
                input_text_line: "DELAY 0x7".to_owned(),
                file_name: String::from("test"),
                line_counter: 1,
                program_counter: 1,
                line_type: LineType::Opcode,
                opcode: String::from("000F013"),
            },
            Pass2 {
                input_text_line: "PUSH A".to_owned(),
                file_name: String::from("test"),
                line_counter: 2,
                program_counter: 3,
                line_type: LineType::Opcode,
                opcode: String::from("0000F013"),
            },
            Pass2 {
                input_text_line: "RET".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 4,
                line_type: LineType::Opcode,
                opcode: String::from("0000F013"),
            },
            Pass2 {
                input_text_line: "RET".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Opcode,
                opcode: String::from("0000F013"),
            },
            Pass2 {
                input_text_line: ":ERIC".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Label,
                opcode: String::default(),
            },
            Pass2 {
                input_text_line: "// Comment".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Comment,
                opcode: String::from("test"),
            },
            Pass2 {
                input_text_line: "#DATA1 \"HELLO\"".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Data,
                opcode: String::from("12345678FFFFFFFFDDDDDDDD"),
            },
            Pass2 {
                input_text_line: "xxx".to_owned(),
                file_name: String::from("test"),
                line_counter: 3,
                program_counter: 5,
                line_type: LineType::Error,
                opcode: String::from("test"),
            },
        ];

        let file_path1 = tmp_dir.path().join("test1.klc");
        let binding = file_path1;
        let file_name1: &str = binding.to_str().unwrap();

        let result_write = write_code_output_file(file_name1, &mut pass2, &mut msg_list);
        result_write.unwrap();

        let buffer = fs::read_to_string(file_name1).unwrap();
        // 4 header lines prepended; word 0 = heap_start in bytes (program ends at 0x11,
        // rounded up to 8 like the .kbt → 0x18)
        // 64-bit mode: header offsets are i*8, values are 16-char hex; data words are 16 hex chars each
        // opcode "12345678FFFFFFFFDDDDDDDD" (24 chars) yields 2 data lines, the second a half word
        assert_eq!(buffer.lines().count(), 14);
        assert_eq!(buffer, "0x00000000: 0000000000000018                  -- heap_start  (set by assembler)\n0x00000008: 0000000000000000                  -- heap_end    (reserved)\n0x00000010: 0000000000000000                  -- (reserved)\n0x00000018: 0000000000000000                  -- (reserved)\n0x00000000: x                 -- MOV 0xEEEEEEEE 0xFFFFFFFF\n0x00000001: 000F013           -- DELAY 0x7\n0x00000003: 0000F013          -- PUSH A\n0x00000004: 0000F013          -- RET\n0x00000005: 0000F013          -- RET\n0x00000005:                   -- :ERIC\n                              -- // Comment\n0x00000005: 12345678FFFFFFFF                  -- #DATA1 \"HELLO\"\n0x0000000D: DDDDDDDD                          -- #DATA1 \"HELLO\"\nError                         -- xxx\n");
    }
}
