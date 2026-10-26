//! Reading assembler source: a file and its `!include`s, as numbered
//! lines, and removing `/* ... */` block comments from them.

use crate::asm::line::SourceLine;
use crate::diag::Diagnostics;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufReader, prelude::*};
use std::path::{Path, PathBuf};

/// Return the file named by an `!include` line.
///
/// `None` if the line is not an `!include` (the first non-blank text must be
/// `!include`); otherwise the first word after it, before any `//` comment,
/// which is empty when the name is missing.
pub(crate) fn include_target(input_line: &str) -> Option<&str> {
    let rest = input_line.trim_start().strip_prefix("!include")?;
    let rest = rest.find("//").map_or(rest, |comment| &rest[..comment]);
    Some(rest.split_whitespace().next().unwrap_or(""))
}

/// Open text file and return as vector of strings.
///
/// Reads any given file by filename, adding the fill line by line into vector and returns None or Some(String). Manages included files.
pub(crate) fn read_file_to_vector(filename: &str, msg_list: &mut Diagnostics) -> Option<Vec<SourceLine>> {
    read_source_file(filename, msg_list, &mut HashSet::new())
}

/// Read one source file, expanding its `!include` lines.
///
/// `open_files` holds the canonical paths of the files currently being read
/// (this one's includers); a file already in it is a recursive include. The
/// path is compared before the file is opened, and always removed again
/// afterwards, however reading the file ends.
fn read_source_file(filename: &str, msg_list: &mut Diagnostics, open_files: &mut HashSet<PathBuf>) -> Option<Vec<SourceLine>> {
    let Ok(canonical) = fs::canonicalize(filename) else {
        msg_list.error(format!("Unable to open file {filename}"));
        return None;
    };
    if open_files.contains(&canonical) {
        msg_list.error(format!("Recursive include of file {filename}"));
        return None;
    }
    let Ok(file) = File::open(&canonical) else {
        msg_list.error(format!("Unable to open file {filename}"));
        return None;
    };

    open_files.insert(canonical.clone());
    let lines = read_source_lines(file, filename, msg_list, open_files);
    open_files.remove(&canonical);
    lines
}

/// Read the lines of an open source file, expanding its `!include` lines.
fn read_source_lines(file: File, filename: &str, msg_list: &mut Diagnostics, open_files: &mut HashSet<PathBuf>) -> Option<Vec<SourceLine>> {
    let buf = BufReader::new(file);
    let mut lines: Vec<SourceLine> = Vec::new();

    let mut line_number = 0;
    for line in buf.lines() {
        match line {
            Ok(line_contents) => {
                line_number += 1;
                if let Some(include_file) = include_target(&line_contents) {
                    if include_file.is_empty() {
                        msg_list.error_at(filename, line_number, format!("Missing include file name in {filename}"));
                        return None;
                    }

                    // Get the include file from the same directory as the previous file
                    let parent = Path::new(filename).parent().unwrap_or_else(|| Path::new(""));
                    let new_include_file = parent.join(include_file).to_string_lossy().into_owned();

                    let Some(include_lines) = read_source_file(&new_include_file, msg_list, open_files) else {
                        msg_list.error_at(
                            filename,
                            line_number,
                            format!("Unable to open include file {new_include_file} in {filename}"),
                        );
                        return Some(lines);
                    };
                    lines.extend(include_lines);
                } else {
                    lines.push(SourceLine {
                        input_text_line: line_contents,
                        file_name: filename.to_owned(),
                        line_counter: line_number,
                    });
                }
            }
            // A read error usually repeats on every later read (a directory, say),
            // so stop reading the file rather than reporting it forever.
            Err(err) => {
                msg_list.error_at(filename, line_number, format!("Error reading source file {filename}: {err}"));
                return None;
            }
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
fn unterminated_comment(file_name: &str, line_number: u32, msg_list: &mut Diagnostics) {
    msg_list.error_at(file_name, line_number, format!("Comment not terminated in file {file_name}"));
}

/// Remove comments from vector of strings.
///
/// Checks for /* */ and removes them from the vector of strings. Comments do
/// not nest, may span lines, but never span files: a comment still open at the
/// end of a file is reported at the line where its `/*` was.
pub(crate) fn remove_block_comments(lines: Vec<SourceLine>, msg_list: &mut Diagnostics) -> Vec<SourceLine> {
    let mut state = CommentState::Code;
    // File and line of the `/*` that opened the current comment.
    let mut opened_at: (String, u32) = (String::new(), 0);
    let mut new_lines: Vec<SourceLine> = Vec::new();
    for line in lines {
        let in_comment = matches!(state, CommentState::Comment | CommentState::Star);
        if in_comment && opened_at.0 != line.file_name {
            unterminated_comment(&opened_at.0, opened_at.1, msg_list);
            state = CommentState::Code;
        }

        let mut new_line = String::default();
        for char in line.input_text_line.chars() {
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
        new_lines.push(SourceLine {
            input_text_line: new_line,
            file_name: line.file_name,
            line_counter: line.line_counter,
        });
    }
    if state == CommentState::Comment {
        unterminated_comment(&opened_at.0, opened_at.1, msg_list);
    }
    new_lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::MessageType;
    use tempfile::TempDir;

    #[test]
    // Test remove of comments in single line
    fn test_remove_block_comments1() {
        let input = vec![SourceLine {
            input_text_line: "abc/* This is a comment */def".to_owned(),
            file_name: "test.kla".to_owned(),
            line_counter: 1,
        }];
        let output = remove_block_comments(input, &mut Diagnostics::new());
        assert_eq!(
            output,
            vec![SourceLine {
                input_text_line: "abcdef".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 1,
            }]
        );
    }

    #[test]
    // Test remove of comments in two lines
    fn test_remove_block_comments2() {
        let input = vec![
            SourceLine {
                input_text_line: "abc/* This is a comment */def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 1,
            },
            SourceLine {
                input_text_line: "abc/* This is a comment */def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 2,
            },
        ];
        let output = remove_block_comments(input, &mut Diagnostics::new());
        assert_eq!(
            output,
            vec![
                SourceLine {
                    input_text_line: "abcdef".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 1,
                },
                SourceLine {
                    input_text_line: "abcdef".to_owned(),
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
            SourceLine {
                input_text_line: "abc/* This is a comment ".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 1,
            },
            SourceLine {
                input_text_line: "so is this */def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 2,
            },
        ];
        let output = remove_block_comments(input, &mut Diagnostics::new());
        assert_eq!(
            output,
            vec![
                SourceLine {
                    input_text_line: "abc".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 1,
                },
                SourceLine {
                    input_text_line: "def".to_owned(),
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
            SourceLine {
                input_text_line: "abc/* This is a comment ".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 1,
            },
            SourceLine {
                input_text_line: "so is this def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 2,
            },
            SourceLine {
                input_text_line: "*/def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 3,
            },
        ];
        let output = remove_block_comments(input, &mut Diagnostics::new());
        assert_eq!(
            output,
            vec![
                SourceLine {
                    input_text_line: "abc".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 1,
                },
                SourceLine {
                    input_text_line: String::default(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 2,
                },
                SourceLine {
                    input_text_line: "def".to_owned(),
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
            SourceLine {
                input_text_line: "abc/* This is a /* /*comment ".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 1,
            },
            SourceLine {
                input_text_line: "so is this def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 2,
            },
            SourceLine {
                input_text_line: "*/def".to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: 3,
            },
        ];
        let output = remove_block_comments(input, &mut Diagnostics::new());
        assert_eq!(
            output,
            vec![
                SourceLine {
                    input_text_line: "abc".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 1,
                },
                SourceLine {
                    input_text_line: String::default(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 2,
                },
                SourceLine {
                    input_text_line: "def".to_owned(),
                    file_name: "test.kla".to_owned(),
                    line_counter: 3,
                },
            ]
        );
    }

    #[test]
    // Test comments are closed at end of file
    fn test_remove_block_comments6() {
        let msg_list = &mut Diagnostics::new();
        let input = vec![
            SourceLine {
                input_text_line: "abc/* This is a /* /*comment ".to_owned(),
                file_name: "test1.kla".to_owned(),
                line_counter: 1,
            },
            SourceLine {
                input_text_line: "so is this def".to_owned(),
                file_name: "test1.kla".to_owned(),
                line_counter: 2,
            },
            SourceLine {
                input_text_line: "def".to_owned(),
                file_name: "test2.kla".to_owned(),
                line_counter: 3,
            },
        ];
        let output = remove_block_comments(input, msg_list);
        assert_eq!(
            output,
            vec![
                SourceLine {
                    input_text_line: "abc".to_owned(),
                    file_name: "test1.kla".to_owned(),
                    line_counter: 1,
                },
                SourceLine {
                    input_text_line: String::default(),
                    file_name: "test1.kla".to_owned(),
                    line_counter: 2,
                },
                SourceLine {
                    input_text_line: "def".to_owned(),
                    file_name: "test2.kla".to_owned(),
                    line_counter: 3,
                },
            ]
        );
        assert_eq!(msg_list.messages()[0].text(), "Comment not terminated in file test1.kla");
        // Reported where the comment opened, not at the first line of the next file.
        assert_eq!(msg_list.messages()[0].line_number(), Some(1));
        assert_eq!(msg_list.messages()[0].file_name(), Some("test1.kla"));
    }

    /// Run `remove_block_comments` over `lines` (all in `test.kla`).
    fn strip_block(lines: &[&str]) -> (Vec<String>, Diagnostics) {
        let mut msg_list = Diagnostics::new();
        let input = lines
            .iter()
            .zip(1..)
            .map(|(line, n)| SourceLine {
                input_text_line: (*line).to_owned(),
                file_name: "test.kla".to_owned(),
                line_counter: n,
            })
            .collect();
        let output = remove_block_comments(input, &mut msg_list)
            .into_iter()
            .map(|l| l.input_text_line)
            .collect();
        (output, msg_list)
    }

    #[test]
    // `/*/` opens a comment; the `/` after the `*` does not also close it
    fn test_remove_block_comments_slash_star_slash() {
        let (output, msg_list) = strip_block(&["abc/*/def"]);
        assert_eq!(output, vec!["abc"]);
        assert_eq!(msg_list.count(MessageType::Error), 1);
    }

    #[test]
    // A `*` after a comment closes is ordinary text
    fn test_remove_block_comments_star_after_close() {
        let (output, msg_list) = strip_block(&["a/* x */*b", "c"]);
        assert_eq!(output, vec!["a*b", "c"]);
        assert_eq!(msg_list.count(MessageType::Error), 0);
    }

    #[test]
    // Comments do not nest, and `**/` still closes one
    fn test_remove_block_comments_nested_looking() {
        let (output, msg_list) = strip_block(&["abc/* /* x */def", "g/* x */ */h", "i/* x **/j"]);
        assert_eq!(output, vec!["abcdef", "g */h", "ij"]);
        assert_eq!(msg_list.count(MessageType::Error), 0);
    }

    #[test]
    // Slashes that do not open a comment are kept
    fn test_remove_block_comments_plain_slashes() {
        let (output, msg_list) = strip_block(&["a/b", "c//d", "e/", "*/f"]);
        assert_eq!(output, vec!["a/b", "c//d", "e/", "*/f"]);
        assert_eq!(msg_list.count(MessageType::Error), 0);
    }

    #[test]
    // A `*` ending a line does not pair with a `/` starting the next
    fn test_remove_block_comments_star_at_line_end() {
        let (output, msg_list) = strip_block(&["a/* x *", "/b */c"]);
        assert_eq!(output, vec!["a", "c"]);
        assert_eq!(msg_list.count(MessageType::Error), 0);
    }

    #[test]
    // A comment still open at the end of the last file is reported where it opened
    fn test_remove_block_comments_unterminated_last_file() {
        let (output, msg_list) = strip_block(&["abc", "def /* open", "ghi"]);
        assert_eq!(output, vec!["abc", "def ", ""]);
        assert_eq!(msg_list.count(MessageType::Error), 1);
        let msg = &msg_list.messages()[0];
        assert_eq!(msg.text(), "Comment not terminated in file test.kla");
        assert_eq!(msg.line_number(), Some(2));
        assert_eq!(msg.file_name(), Some("test.kla"));
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
    // Test for missing file
    fn test_read_file_to_vec1() {
        let mut msg_list = Diagnostics::new();
        read_file_to_vector("////xxxxxxx", &mut msg_list);
        assert_eq!(msg_list.messages()[0].text(), "Unable to open file ////xxxxxxx");
    }

    #[test]
    // Test for simple file added
    fn test_write_file_to_vec2() {
        let mut msg_list = Diagnostics::new();

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
        assert_eq!(lines.clone().unwrap()[0].input_text_line, "Test line in file");
        assert_eq!(lines.unwrap().len(), 4);

        drop(tmp_file1);
        _ = tmp_dir.close();
    }

    #[test]
    // Test for included file
    fn test_write_file_to_vec3() {
        let mut msg_list = Diagnostics::new();

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
        assert_eq!(lines.clone().unwrap_or_default()[0].input_text_line, "Test line in file 1 line 0");
        assert_eq!(lines.clone().unwrap_or_default()[2].input_text_line, "Test line in file 2 line 1");
        assert_eq!(lines.clone().unwrap_or_default()[6].input_text_line, "Test line in file 1 line 2");
        assert_eq!(lines.unwrap().len(), 7);

        drop(tmp_file1);
        drop(tmp_file2);
        _ = tmp_dir.close();
    }

    #[test]
    // Test for recursive file includes
    fn test_write_file_to_vec4() {
        let mut msg_list = Diagnostics::new();

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
        assert_eq!(msg_list.messages()[0].text(), format!("Recursive include of file {file_name1}"));

        drop(tmp_file1);
        drop(tmp_file2);
        _ = tmp_dir.close();
    }

    #[test]
    // Test for missing included file
    fn test_write_file_to_vec5() {
        let mut msg_list = Diagnostics::new();

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
        assert_eq!(msg_list.messages()[0].text(), format!("Unable to open file {file_name2}"));
        assert_eq!(
            msg_list.messages()[1].text(),
            format!("Unable to open include file {file_name2} in {file_name1}")
        );
        assert_eq!(lines, Some(vec![]));

        drop(tmp_file1);
        _ = tmp_dir.close();
    }
    #[test]
    // Test for missing included file name
    fn test_write_file_to_vec6() {
        let mut msg_list = Diagnostics::new();

        let tmp_dir = TempDir::new().unwrap();

        let file_path1 = tmp_dir.path().join("test1.kla");
        let binding = file_path1.clone();
        let file_name1: &str = binding.to_str().unwrap();
        let mut tmp_file1: File = File::create(file_path1).unwrap();
        _ = writeln!(tmp_file1, "!include");
        _ = writeln!(tmp_file1, "Test line in file 1 line 1");
        _ = writeln!(tmp_file1, "Test line in file 1 line 2");

        let lines = read_file_to_vector(file_name1, &mut msg_list);
        assert_eq!(msg_list.messages()[0].text(), format!("Missing include file name in {file_name1}"));
        assert_eq!(lines, None);

        drop(tmp_file1);
        _ = tmp_dir.close();
    }

    #[test]
    // Test for double included file
    fn test_write_file_to_vec7() {
        let mut msg_list = Diagnostics::new();

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
        assert_eq!(msg_list.messages()[0].text(), format!("Unable to open file {file_name3}"));
        assert_eq!(
            msg_list.messages()[1].text(),
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
        let mut msg_list = Diagnostics::new();
        let tmp_dir = TempDir::new().unwrap();
        fs::create_dir(tmp_dir.path().join("sub")).unwrap();
        let file_path1 = tmp_dir.path().join("sub").join("a.kla");
        fs::write(&file_path1, "line 1\n!include ./a.kla\n!include ../sub/a.kla\nline 4\n").unwrap();
        let file_name1 = file_path1.to_str().unwrap();

        let lines = read_file_to_vector(file_name1, &mut msg_list).unwrap();
        let recursive: Vec<&str> = msg_list
            .messages()
            .iter()
            .filter(|m| m.text().starts_with("Recursive include"))
            .map(crate::diag::Message::text)
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
        let mut msg_list = Diagnostics::new();
        let tmp_dir = TempDir::new().unwrap();
        let file_path1 = tmp_dir.path().join("a.kla");
        fs::write(&file_path1, "!include b.kla\n").unwrap();
        fs::write(tmp_dir.path().join("b.kla"), "b line\n!include missing.kla\n").unwrap();
        fs::write(tmp_dir.path().join("c.kla"), "!include b.kla\n!include b.kla\n").unwrap();
        let file_name_c = tmp_dir.path().join("c.kla");

        _ = read_file_to_vector(file_path1.to_str().unwrap(), &mut msg_list);
        _ = read_file_to_vector(file_name_c.to_str().unwrap(), &mut msg_list);
        assert!(msg_list.messages().iter().all(|m| !m.text().starts_with("Recursive include")));
    }

    #[test]
    // The same file may be included twice when it is not its own includer
    fn test_read_file_diamond_include() {
        let mut msg_list = Diagnostics::new();
        let tmp_dir = TempDir::new().unwrap();
        let file_path1 = tmp_dir.path().join("a.kla");
        fs::write(&file_path1, "!include b.kla\n!include c.kla\n").unwrap();
        fs::write(tmp_dir.path().join("b.kla"), "b\n!include d.kla\n").unwrap();
        fs::write(tmp_dir.path().join("c.kla"), "c\n!include d.kla\n").unwrap();
        fs::write(tmp_dir.path().join("d.kla"), "d\n").unwrap();

        let lines = read_file_to_vector(file_path1.to_str().unwrap(), &mut msg_list).unwrap();
        let text: Vec<&str> = lines.iter().map(|l| l.input_text_line.as_str()).collect();
        assert_eq!(text, vec!["b", "d", "c", "d"]);
        assert_eq!(msg_list.count(MessageType::Error), 0);
    }

    #[test]
    #[cfg(unix)] // Opening a directory only fails at the first read on Unix
    // Regression: a read error (here, including a directory) repeats on every
    // read, and the reader reported it forever instead of stopping.
    fn test_read_file_include_directory() {
        let mut msg_list = Diagnostics::new();
        let tmp_dir = TempDir::new().unwrap();
        fs::create_dir(tmp_dir.path().join("dir")).unwrap();
        let file_path1 = tmp_dir.path().join("a.kla");
        fs::write(&file_path1, "line 1\n!include dir\nline 3\n").unwrap();
        let dir_name = tmp_dir.path().join("dir");
        let dir_name = dir_name.to_str().unwrap();

        let lines = read_file_to_vector(file_path1.to_str().unwrap(), &mut msg_list).unwrap();
        assert_eq!(lines.len(), 1);
        let errors: Vec<&str> = msg_list.messages().iter().map(crate::diag::Message::text).collect();
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].starts_with(&format!("Error reading source file {dir_name}: ")), "{errors:?}");
        assert!(
            errors[1].starts_with(&format!("Unable to open include file {dir_name} in ")),
            "{errors:?}"
        );

        let mut msg_list = Diagnostics::new();
        assert!(read_file_to_vector(dir_name, &mut msg_list).is_none());
        assert_eq!(msg_list.count(MessageType::Error), 1);
    }
}
