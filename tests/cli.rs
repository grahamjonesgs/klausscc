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

#[test]
fn assembly_errors_exit_1_but_still_write_the_listing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bad.kla"), "_start\nNOTANOP A\nHALT\n").unwrap();

    let out = klausscc(dir.path(), &["-i", "bad.kla"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(dir.path().join("bad.code").is_file(), "the .code listing shows where the errors are");
    assert!(!dir.path().join("bad.kbt").exists(), "no binary for a failed build");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Completed with 1 error"), "stderr: {stderr}");
    assert!(!stderr.contains("Error: "), "no stray Debug print of the error: {stderr}");
}

#[test]
fn missing_input_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let out = klausscc(dir.path(), &["-i", "nothere.kla"]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn bad_command_line_exits_1_not_2() {
    // clap's own usage-error status is 2, which means "tests failed" here.
    let dir = tempfile::tempdir().unwrap();
    let out = klausscc(dir.path(), &["--no-such-flag"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--no-such-flag"));

    let out = klausscc(dir.path(), &[]);
    assert_eq!(out.status.code(), Some(1), "no arguments prints help and fails");
}

#[test]
fn help_and_version_exit_0() {
    let dir = tempfile::tempdir().unwrap();
    for flag in ["--help", "--version"] {
        let out = klausscc(dir.path(), &[flag]);
        assert!(out.status.success(), "{flag}");
        assert!(!out.stdout.is_empty(), "{flag} prints to stdout");
    }
}

#[test]
fn passing_emulator_test_exits_0() {
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/src/klatest/test_strings.kla");
    let dir = tempfile::tempdir().unwrap();
    let out = klausscc(dir.path(), &["--emulate-test", fixture]);
    assert!(out.status.success(), "stdout: {}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn failing_emulator_test_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    // Expects one UART value but prints nothing.
    std::fs::write(dir.path().join("silent.kla"), "// 00000001\n_start\nHALT\n").unwrap();

    let out = klausscc(dir.path(), &["--emulate-test", "silent.kla"]);
    assert_eq!(out.status.code(), Some(2), "stdout: {}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn emulator_test_list_with_unreadable_file_fails() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("list.txt"), "missing.kla\n").unwrap();

    let out = klausscc(dir.path(), &["--emulate-test", "list.txt"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(2), "stdout: {stdout}");
    assert!(stdout.contains("FAIL missing.kla: cannot read"), "stdout: {stdout}");
    assert!(stdout.contains("0/1 files passed"), "stdout: {stdout}");
}

#[test]
fn emulator_test_with_nothing_to_validate_fails() {
    let dir = tempfile::tempdir().unwrap();
    // Assembles and runs, but has no expected UART values to check.
    std::fs::write(dir.path().join("plain.kla"), GOOD_PROGRAM).unwrap();

    let out = klausscc(dir.path(), &["--emulate-test", "plain.kla"]);
    assert_eq!(out.status.code(), Some(1), "stdout: {}", String::from_utf8_lossy(&out.stdout));
}
