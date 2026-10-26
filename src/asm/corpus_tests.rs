//! Every `.kla` file in the repository assembles.
//!
//! A program must build to a `.kbt` image with no errors.  A file that is not
//! a whole program by itself (a library with no `_start`, or `crt0.kla`,
//! which calls a `main` the program supplies) is listed in [`NON_PROGRAMS`]
//! and assembled inside a small driver instead.  A missing `_start` stays an
//! error for programs; the list only says how to check the files meant to be
//! included.

use super::source::read_file_to_vector;
use super::*;
use crate::asm::macros::{builtin_macros, expand_embedded_macros};
use std::fs;
use std::path::{Path, PathBuf};

/// The directories, relative to the crate root, whose `.kla` files are checked.
const DIRS: [&str; 2] = ["runtime", "tests/fixtures"];

/// A file that is not a whole program, and how to assemble it.
struct NonProgram {
    /// The file, relative to the crate root.
    file: &'static str,
    /// Source placed before the file: the `_start` or `main` it lacks.
    driver: &'static str,
    /// Files, relative to the crate root, placed after it: the routines it calls.
    companions: &'static [&'static str],
}

/// The files that are only meant to be `!include`d.
const NON_PROGRAMS: [NonProgram; 5] = [
    NonProgram {
        file: "runtime/crt0.kla",
        driver: "main:\nSETR M 0\nRET\n",
        companions: &[],
    },
    NonProgram {
        file: "runtime/libc.kla",
        driver: "_start\nHALT\n",
        companions: &["runtime/uart_stubs.kla"],
    },
    NonProgram {
        file: "runtime/uart_stubs.kla",
        driver: "_start\nHALT\n",
        companions: &[],
    },
    NonProgram {
        file: "tests/fixtures/string_print.kla",
        driver: "_start\nHALT\n",
        companions: &[],
    },
    NonProgram {
        file: "tests/fixtures/uart_stubs.kla",
        driver: "_start\nHALT\n",
        companions: &[],
    },
];

/// The crate root.
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every `.kla` file under `dir`, recursively.
fn kla_files(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            kla_files(&path, found);
        } else if path.extension().is_some_and(|e| e == "kla") {
            found.push(path);
        }
    }
}

/// The lines of `file` (relative to the crate root), with its `!include`s.
fn read(file: &str, msg_list: &mut Diagnostics) -> Vec<SourceLine> {
    let path = root().join(file);
    read_file_to_vector(&path.to_string_lossy(), msg_list).unwrap_or_else(|| panic!("cannot read {file}"))
}

/// Assemble `lines` and build its `.kbt` image; return the error messages.
fn build_errors(lines: Vec<SourceLine>, mut msg_list: Diagnostics) -> Vec<String> {
    let isa = Isa::v2();
    let macro_list = expand_embedded_macros(builtin_macros(), &mut msg_list);
    let assembly = assemble_lines(lines, &isa, &macro_list, &mut msg_list);
    if !msg_list.has_errors() {
        // Reports a missing or doubled _start, or a part-word image.
        let _ = assembly.to_kbt(&mut msg_list);
    }
    msg_list
        .messages()
        .iter()
        .filter(|m| m.level() == MessageType::Error)
        .map(|m| m.text().to_owned())
        .collect()
}

/// The lines of a non-program inside its driver, followed by its companions.
fn driven_lines(non_program: &NonProgram, msg_list: &mut Diagnostics) -> Vec<SourceLine> {
    let mut lines: Vec<SourceLine> = non_program
        .driver
        .lines()
        .zip(1..)
        .map(|(text, line_counter)| SourceLine {
            file_name: "<driver>".to_owned(),
            input_text_line: text.to_owned(),
            line_counter,
        })
        .collect();
    lines.extend(read(non_program.file, msg_list));
    for companion in non_program.companions {
        lines.extend(read(companion, msg_list));
    }
    lines
}

#[test]
fn every_kla_file_assembles() {
    let mut files = Vec::new();
    for dir in DIRS {
        kla_files(&root().join(dir), &mut files);
    }
    files.sort();
    assert!(files.len() >= 39, "only {} .kla files found", files.len());

    let mut failures = Vec::new();
    for path in &files {
        let file = path
            .strip_prefix(root())
            .expect("under the crate root")
            .to_string_lossy()
            .replace('\\', "/");
        let mut msg_list = Diagnostics::new();
        let lines = match NON_PROGRAMS.iter().find(|n| n.file == file) {
            Some(non_program) => driven_lines(non_program, &mut msg_list),
            None => read(&file, &mut msg_list),
        };
        let errors = build_errors(lines, msg_list);
        if !errors.is_empty() {
            failures.push(format!("{file}: {errors:?}"));
        }
    }
    assert!(failures.is_empty(), "files that do not assemble: {failures:#?}");

    // Every listed file exists, so the list cannot go stale.
    for non_program in &NON_PROGRAMS {
        assert!(files.contains(&root().join(non_program.file)), "{} not found", non_program.file);
    }
}

#[test]
fn non_programs_do_not_build_alone() {
    // A missing _start (or main) is still an error: the driver is what makes
    // these files build, so each listed file must need one.
    for non_program in &NON_PROGRAMS {
        let mut msg_list = Diagnostics::new();
        let lines = read(non_program.file, &mut msg_list);
        let errors = build_errors(lines, msg_list);
        assert!(!errors.is_empty(), "{} builds by itself; drop it from NON_PROGRAMS", non_program.file);
    }
    let mut msg_list = Diagnostics::new();
    let lines = read("tests/fixtures/uart_stubs.kla", &mut msg_list);
    assert_eq!(build_errors(lines, msg_list), ["No start address found"]);
}
