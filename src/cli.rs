//! Command-line interface definition (clap).

use crate::serial::AUTO_SERIAL;
use clap::{Arg, ArgAction, Command};

/// Builds the clap `Command` describing every CLI argument and subcommand mode.
#[must_use]
pub fn set_matches() -> Command {
    Command::new("Klauss Assembler")
        .version(env!("CARGO_PKG_VERSION"))
        .author("Graham Jones")
        .about("Assembler for FPGA_CPU")
        .arg_required_else_help(true)
        .override_usage(
            "klausscc [OPTIONS] \
             <--input <input> | --textmate | --opcodes | --test-list <test_list> \
             | --net-load <file> | --mem-out <file> | --monitor>",
        )
        .arg(
            Arg::new("opcode_file")
                .short('c')
                .long("opcode")
                .num_args(1)
                .help("Opcode source file from Verilog (required only when assembling a .kla file or emitting opcode/textmate JSON)"),
        )
        .arg(
            Arg::new("net_load")
                .short('N')
                .long("net-load")
                .num_args(1)
                .conflicts_with_all(["input", "test_list", "textmate", "opcodes", "mem_out"])
                .help("ELF or flat binary: flatten and stream to the board over TCP (network boot). Needs --ip."),
        )
        .arg(
            Arg::new("ip")
                .long("ip")
                .num_args(1)
                .help("Board IP address for --net-load (e.g. 192.168.68.50)"),
        )
        .arg(
            Arg::new("port")
                .long("port")
                .num_args(1)
                .help("Board TCP port for --net-load (default 5000)"),
        )
        .arg(
            Arg::new("mem_out")
                .long("mem-out")
                .num_args(1)
                .conflicts_with_all(["input", "test_list", "textmate", "opcodes", "net_load"])
                .help("ELF or flat binary: write a $readmemh boot-ROM image for boot_rom.v (resident netboot)."),
        )
        .arg(
            Arg::new("mem_file")
                .long("mem-file")
                .num_args(1)
                .help("Output path for --mem-out (default <input>.mem)"),
        )
        .arg(
            Arg::new("entry_point")
                .long("entry")
                .num_args(1)
                .help("Entry point address for ELF / flat-binary input or net-load (hex 0x... or decimal). Optional for ELF files \u{2014} read from ELF header. Required for flat binaries (default 0x20)."),
        )
        .arg(
            Arg::new("input")
                .short('i')
                .long("input")
                .required_unless_present_any(["textmate", "opcodes", "test_list", "net_load", "mem_out", "monitor", "emulate_test"])
                .conflicts_with("textmate")
                .conflicts_with("opcodes")
                .num_args(1)
                .help("Input file. Type is detected from the extension: .kla assembles (needs --opcode), .kbt sends a pre-built image, anything else (.elf or flat binary) converts to the board wire format"),
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
        .arg(
            Arg::new("no_break")
                .short('n')
                .long("no-break")
                .action(ArgAction::SetTrue)
                .help("Skip UART break signal and send 'S' instead (for testing CPU without break)"),
        )
        .arg(
            Arg::new("debug")
                .short('d')
                .long("debug")
                .action(ArgAction::SetTrue)
                .help("Print each received UART byte as hex alongside normal output"),
        )
        .arg(
            Arg::new("emulate")
                .long("emulate")
                .action(ArgAction::SetTrue)
                .help("Assemble the input and run it on the built-in ISA emulator (golden model); prints captured UART output"),
        )
        .arg(
            Arg::new("trace")
                .long("trace")
                .num_args(1)
                .help("With --emulate, write the per-instruction golden-model trace to this file (default: stdout)"),
        )
        .arg(
            Arg::new("emulate_test")
                .long("emulate-test")
                .num_args(1)
                .help("Run the emulator over a test list (or a directory of .kla files) and verify captured UART vs expected // values"),
        )
        .arg(
            Arg::new("max_instructions")
                .long("max-instructions")
                .num_args(1)
                .help("Instruction-count cap for the emulator (default 50000000)"),
        )
}
