//! Command-line interface definition (clap).

use crate::num::parse_u32;
use crate::serial::AUTO_SERIAL;
use clap::{Arg, ArgAction, Command};

/// Builds the clap `Command` describing every CLI argument and subcommand mode.
#[must_use]
pub(crate) fn set_matches() -> Command {
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
                .value_parser(parse_u32)
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
                .help("Input file. Type is detected from the extension: .kla assembles (ISA v2 opcode table is built in), .kbt sends a pre-built image, anything else (.elf or flat binary) converts to the board wire format"),
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
                .help("Set if JSON output of opcodes for use in Textmate or vscode language formatter is required"),
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
            Arg::new("gdbserver")
                .long("gdbserver")
                .num_args(1)
                .value_parser(clap::value_parser!(u16))
                .value_name("PORT")
                .help("Debug an ELF on the built-in emulator: serve the GDB remote protocol on 127.0.0.1:PORT for lldb (gdb-remote / lldb-dap). UART output goes to stdout, stdin feeds the UART"),
        )
        .arg(
            Arg::new("board")
                .long("board")
                .action(ArgAction::SetTrue)
                .requires("gdbserver")
                .help("With --gdbserver, debug the real board over its USB-JTAG (needs a DEBUG_EN=1 bitstream and a klausscc built with --features board). With -s, the ELF is first loaded over the UART and the core halts at its entry; without -s, the running program is halted where it is"),
        )
        .arg(
            Arg::new("jtag_khz")
                .long("jtag-khz")
                .num_args(1)
                .value_parser(clap::value_parser!(u32).range(100..=30_000))
                .value_name("KHZ")
                .help("JTAG clock for --board, in kHz (default 10000)"),
        )
        .arg(
            Arg::new("trace")
                .long("trace")
                .num_args(0..=1)
                .default_missing_value("-")
                .help("With --emulate, write the per-instruction golden-model trace. Give a path to write a file, or pass --trace with no value (or '-') to stream it to stdout"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // clap's own consistency checks: duplicate ids or short flags, dangling
    // `requires`/`conflicts_with` names, bad defaults. These otherwise only
    // surface as a panic when the offending flag is used.
    fn command_definition_is_valid() {
        set_matches().debug_assert();
    }

    /// Parse `args` (without the program name).
    fn parse(args: &[&str]) -> Result<clap::ArgMatches, clap::Error> {
        set_matches().try_get_matches_from(std::iter::once("klausscc").chain(args.iter().copied()))
    }

    /// Assert that every command line in `cases` parses.
    fn assert_accepted(cases: &[&[&str]]) {
        for args in cases {
            if let Err(err) = parse(args) {
                panic!("{args:?} should parse: {err}");
            }
        }
    }

    /// Assert that every command line in `cases` is a usage error.
    fn assert_rejected(cases: &[&[&str]]) {
        for args in cases {
            assert!(parse(args).is_err(), "{args:?} should be rejected");
        }
    }

    #[test]
    // --entry takes hex or decimal and rejects junk instead of falling back to 0x20.
    fn entry_point_is_parsed() {
        let entry = |value: &str| parse(&["-i", "prog.elf", "--entry", value]);
        assert_eq!(entry("0x40").unwrap().get_one::<u32>("entry_point").copied(), Some(0x40));
        assert_eq!(entry("64").unwrap().get_one::<u32>("entry_point").copied(), Some(64));
        assert!(entry("0xZZ").is_err());
        assert!(entry("0x1_0000_0000").is_err());
    }

    #[test]
    // Every mode's usual flag combinations, short and long.
    fn each_mode_accepts_its_flags() {
        assert_accepted(&[
            // Monitor only.
            &["-m"],
            &["--monitor", "--debug"],
            &["-m", "-s"],
            &["-m", "-s", "/dev/ttyUSB1", "-d"],
            // Net-load, optionally monitoring afterwards.
            &["-N", "prog.elf", "--ip", "192.168.68.50"],
            &["--net-load", "prog.bin", "--ip", "10.0.0.2", "--port", "6000", "--entry", "0x40"],
            &["-N", "prog.elf", "--ip", "10.0.0.2", "-m"],
            &["-N", "prog.elf", "--ip", "10.0.0.2", "-m", "-s", "/dev/ttyUSB1", "-d"],
            // Boot-ROM image.
            &["--mem-out", "prog.elf"],
            &["--mem-out", "prog.elf", "--mem-file", "boot.mem"],
            // ELF / flat binary to the board.
            &["-i", "prog.elf"],
            &["--input", "prog.bin", "--entry", "64", "-b", "out.kbt"],
            &["-i", "prog.elf", "-s", "/dev/ttyUSB1", "-n"],
            &["-i", "prog.elf", "--serial", "-m", "-d"],
            // Debugging.
            &["-i", "prog.elf", "--gdbserver", "1234"],
            &["-i", "prog.elf", "--gdbserver", "1234", "--board"],
            &["-i", "prog.elf", "--gdbserver", "1234", "--board", "--jtag-khz", "5000", "-s", "COM3"],
            // The emulator.
            &["-i", "prog.elf", "--emulate"],
            &["-i", "prog.elf", "--emulate", "--trace"],
            &["-i", "prog.kla", "--emulate", "--trace", "trace.txt", "--max-instructions", "100"],
            &["--emulate-test", "tests"],
            &["--emulate-test", "list.txt", "--max-instructions", "1000"],
            // A pre-built image.
            &["-i", "prog.kbt", "-s", "/dev/ttyUSB1"],
            &["-i", "prog.kbt", "-m"],
            // Opcode lists.
            &["--opcodes"],
            &["-t"],
            &["--textmate", "--opcodes"],
            // Board tests.
            &["-L", "list.txt", "-s", "/dev/ttyUSB1"],
            &["--test-list", "list.txt", "-s", "/dev/ttyUSB1", "--test-timeout", "20", "--no-break"],
            // Assembly, optionally sending, testing or monitoring.
            &["-i", "prog.kla"],
            &["-i", "prog.kla", "-o", "prog.code", "--bitcode", "prog.kbt"],
            &["-i", "prog.kla", "--output", "out", "-b", "out"],
            &["-i", "prog.kla", "-s", "/dev/ttyUSB1"],
            &["-i", "prog.kla", "-s", "/dev/ttyUSB1", "-m", "-d"],
            &["-i", "prog.kla", "-s", "/dev/ttyUSB1", "-T", "--test-timeout", "5", "-n"],
            &["-i", "prog.kla", "-s", "/dev/ttyUSB1", "--test"],
        ]);
    }

    #[test]
    // Mixing modes, or a flag without what it qualifies, is a usage error.
    fn conflicting_flags_are_rejected() {
        assert_rejected(&[
            // Nothing to do.
            &[],
            &["-s", "/dev/ttyUSB1"],
            // Two modes.
            &["-i", "prog.kla", "-t"],
            &["-i", "prog.kla", "--opcodes"],
            &["-L", "list.txt", "-i", "prog.kla"],
            &["-N", "prog.elf", "--ip", "10.0.0.2", "-i", "prog.kla"],
            &["-N", "prog.elf", "--ip", "10.0.0.2", "--mem-out", "prog.elf"],
            &["--mem-out", "prog.elf", "-i", "prog.kla"],
            // Monitoring and testing at once.
            &["-i", "prog.kla", "-s", "/dev/ttyUSB1", "-m", "-T"],
            &["-L", "list.txt", "-s", "/dev/ttyUSB1", "-m"],
            // --board needs --gdbserver.
            &["-i", "prog.elf", "--board"],
            // Bad values.
            &["-i", "prog.elf", "--gdbserver", "port"],
            &["-i", "prog.elf", "--gdbserver", "70000"],
            &["-i", "prog.elf", "--gdbserver", "1234", "--board", "--jtag-khz", "50"],
            &["-i", "prog.elf", "--gdbserver", "1234", "--board", "--jtag-khz", "30001"],
            // Unknown flags and missing values.
            &["--no-such-flag"],
            &["-i"],
            &["-i", "prog.kla", "-o"],
        ]);
    }

    #[test]
    // -s with no value asks for the first USB serial port.
    fn serial_port_is_optional_valued() {
        let serial = |args: &[&str]| parse(args).unwrap().get_one::<String>("serial").cloned();
        assert_eq!(serial(&["-i", "prog.kla"]), None);
        assert_eq!(serial(&["-i", "prog.kla", "-s"]), Some(AUTO_SERIAL.to_owned()));
        assert_eq!(serial(&["-i", "prog.kla", "-s", "-m"]), Some(AUTO_SERIAL.to_owned()));
        assert_eq!(serial(&["-s", "/dev/ttyUSB1", "-i", "prog.kla"]), Some("/dev/ttyUSB1".to_owned()));
        assert_eq!(serial(&["-i", "prog.kla", "--serial=COM3"]), Some("COM3".to_owned()));
    }

    #[test]
    // --trace with no value streams the trace to stdout.
    fn trace_is_optional_valued() {
        let trace = |args: &[&str]| parse(args).unwrap().get_one::<String>("trace").cloned();
        assert_eq!(trace(&["-i", "prog.kla", "--emulate"]), None);
        assert_eq!(trace(&["-i", "prog.kla", "--emulate", "--trace"]), Some("-".to_owned()));
        assert_eq!(trace(&["-i", "prog.kla", "--emulate", "--trace", "t.txt"]), Some("t.txt".to_owned()));
    }

    #[test]
    fn values_reach_their_options() {
        let matches = parse(&["-i", "prog.kla", "-o", "o.code", "-b", "o.kbt", "-s", "COM3", "-T", "-n", "-d"]).unwrap();
        assert_eq!(matches.get_one::<String>("input").map(String::as_str), Some("prog.kla"));
        assert_eq!(matches.get_one::<String>("output").map(String::as_str), Some("o.code"));
        assert_eq!(matches.get_one::<String>("bitcode").map(String::as_str), Some("o.kbt"));
        assert!(matches.get_flag("test") && matches.get_flag("no_break") && matches.get_flag("debug"));
        assert!(!matches.get_flag("monitor") && !matches.get_flag("emulate"));

        let matches = parse(&["-i", "prog.elf", "--gdbserver", "1234", "--board", "--jtag-khz", "2500"]).unwrap();
        assert_eq!(matches.get_one::<u16>("gdbserver").copied(), Some(1234));
        assert!(matches.get_flag("board"));
        assert_eq!(matches.get_one::<u32>("jtag_khz").copied(), Some(2500));

        let matches = parse(&["-N", "prog.elf", "--ip", "10.0.0.2", "--port", "6000"]).unwrap();
        assert_eq!(matches.get_one::<String>("net_load").map(String::as_str), Some("prog.elf"));
        assert_eq!(matches.get_one::<String>("ip").map(String::as_str), Some("10.0.0.2"));
        assert_eq!(matches.get_one::<String>("port").map(String::as_str), Some("6000"));

        let matches = parse(&["--mem-out", "prog.elf", "--mem-file", "boot.mem"]).unwrap();
        assert_eq!(matches.get_one::<String>("mem_out").map(String::as_str), Some("prog.elf"));
        assert_eq!(matches.get_one::<String>("mem_file").map(String::as_str), Some("boot.mem"));

        let matches = parse(&["-L", "list.txt", "-s", "COM3", "--test-timeout", "20"]).unwrap();
        assert_eq!(matches.get_one::<String>("test_list").map(String::as_str), Some("list.txt"));
        assert_eq!(matches.get_one::<String>("test_timeout").map(String::as_str), Some("20"));

        let matches = parse(&["--emulate-test", "dir", "--max-instructions", "1000"]).unwrap();
        assert_eq!(matches.get_one::<String>("emulate_test").map(String::as_str), Some("dir"));
        assert_eq!(matches.get_one::<String>("max_instructions").map(String::as_str), Some("1000"));
    }

    #[test]
    fn test_timeout_defaults_to_10() {
        let matches = parse(&["-i", "prog.kla"]).unwrap();
        assert_eq!(matches.get_one::<String>("test_timeout").map(String::as_str), Some("10"));
    }
}
