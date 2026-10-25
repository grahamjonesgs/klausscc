//! End-to-end checks of the `klausscc` binary: file naming and exit status.
//!
//! Only offline modes are run here (assembly and the emulator); nothing opens
//! a serial port or a network connection.

use std::path::Path;
use std::process::{Command, Output};

/// A tiny program that assembles cleanly.
const GOOD_PROGRAM: &str = "_start\nSETR A 0x41\nHALT\n";

/// Run the assembler with `args` in `dir`.
#[allow(clippy::expect_used, reason = "test helper: no test can go on without the binary")]
fn klausscc(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_klausscc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run klausscc")
}

#[test]
fn input_path_with_spaces_assembles() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("my dir")).unwrap();
    std::fs::write(dir.path().join("my dir/my prog.kla"), GOOD_PROGRAM).unwrap();

    let out = klausscc(dir.path(), &["-i", "my dir/my prog.kla"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(dir.path().join("my dir/my prog.kbt").is_file());
    assert!(dir.path().join("my dir/my prog.code").is_file());
}

#[test]
fn explicit_output_names_keep_their_extension() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("prog.kla"), GOOD_PROGRAM).unwrap();

    let out = klausscc(dir.path(), &["-i", "prog.kla", "-b", "out.kbt", "-o", "out.code"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(dir.path().join("out.kbt").is_file());
    assert!(dir.path().join("out.code").is_file());
    assert!(!dir.path().join("out.kbt.kbt").exists());
    assert!(!dir.path().join("out.code.code").exists());
}
