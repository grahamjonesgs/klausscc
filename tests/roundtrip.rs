//! End-to-end ISA-encoding-v2 round trip through the `klausscc` binary.
//!
//! Each fixture is assembled with the built-in v2 opcode table and run on the
//! golden-model emulator (`--emulate`), and its captured UART output checked.
//! This proves the assembler and the emulator agree on the v2 encoding across
//! ALU, immediates, memory (MMIO UART), the stack, and conditional branches.
//! (The rest of the corpus, which prints through the `uart_stubs.kla` macros,
//! is checked by `commands::tests::emulator_corpus_passes`.)

use std::process::Command;

/// The marker `--emulate` prints before the captured UART output.
const UART_START: &str = "--- Captured UART output ---\n";
/// The marker `--emulate` prints after the captured UART output.
const UART_END: &str = "--- end UART ---";

/// The UART output `klausscc --emulate` printed between its markers.
fn captured_uart(stdout: &str) -> Option<&str> {
    let start = stdout.find(UART_START)? + UART_START.len();
    let end = stdout[start..].find(UART_END)? + start;
    Some(&stdout[start..end])
}

#[test]
fn v2_fixtures_print_their_expected_uart_output() {
    let cases = [("v2_hello.kla", "Hi\n"), ("v2_loop.kla", "AB"), ("v2_math.kla", "0")];
    for (name, expected) in cases {
        let file = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let out = Command::new(env!("CARGO_BIN_EXE_klausscc"))
            .args(["-i", &file, "--emulate"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{file}: stderr: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(captured_uart(&stdout), Some(expected), "UART mismatch for {file}: {stdout}");
    }
}
