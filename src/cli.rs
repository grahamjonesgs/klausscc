//! Command-line interface definition (clap derive), and the mode it selects.
//!
//! [`Cli`] is the parsed command line, with every value already typed.
//! [`Cli::mode`] then decides, once and before any work is done, which of the
//! tool's modes the command line asks for.

use crate::emu::DEFAULT_MAX_INSTRUCTIONS;
use crate::error::Error;
use crate::link::netload::NETBOOT_DEFAULT_PORT;
use crate::link::serial::{ResetMethod, SerialTarget};
use crate::loader::BinaryInput;
use crate::num::parse_u32;
use crate::output::{filename_stem, output_file_name};
use clap::Parser;
use clap::builder::NonEmptyStringValueParser;
use std::path::{Path, PathBuf};

/// Default for `--test-timeout`, in seconds.
pub(crate) const DEFAULT_TEST_TIMEOUT: u64 = 10;

/// Default for `--jtag-khz`: the JTAG clock for `--board`, in kHz.
pub(crate) const DEFAULT_JTAG_KHZ: u32 = 10_000;

/// The `klausscc` command line.
///
/// Each field's doc comment is its `--help` text, so it is written for the
/// terminal rather than as Markdown.
#[derive(Debug, Parser)]
#[allow(
    clippy::doc_markdown,
    clippy::struct_excessive_bools,
    reason = "the field docs are the --help text; the bools are the command line's on/off flags"
)]
#[command(
    name = "Klauss Assembler",
    bin_name = "klausscc",
    version,
    author,
    about = "Assembler for FPGA_CPU",
    long_about = None,
    arg_required_else_help = true
)]
// At most one mode: an input file, a net-load, a boot-ROM image, a board or
// emulator test run, or the opcode lists (--opcodes and -t may go together).
//
// clap counts `requires = X` as met whenever X conflicts with an argument
// that is present, so `requires = "input"` would let `--emulate` through
// beside `--emulate-test`.  That a flag such as `--emulate` or
// `--gdbserver` needs `-i` is checked in `Cli::mode` instead.
pub(crate) struct Cli {
    /// ELF or flat binary: flatten and stream to the board over TCP (network boot). Needs --ip.
    #[arg(
        short = 'N',
        long,
        value_name = "FILE",
        requires = "ip",
        conflicts_with_all = ["input", "mem_out", "test_list", "emulate_test", "opcodes", "textmate"]
    )]
    pub(crate) net_load: Option<PathBuf>,

    /// Board IP address (or host name) for --net-load (e.g. 192.168.68.50)
    #[arg(long, value_name = "ADDRESS", value_parser = NonEmptyStringValueParser::new())]
    pub(crate) ip: Option<String>,

    /// Board TCP port for --net-load
    #[arg(long, value_name = "PORT", default_value_t = NETBOOT_DEFAULT_PORT)]
    pub(crate) port: u16,

    /// ELF or flat binary: write a $readmemh boot-ROM image for boot_rom.v (resident netboot).
    #[arg(long, value_name = "FILE", conflicts_with_all = ["input", "test_list", "emulate_test", "opcodes", "textmate"])]
    pub(crate) mem_out: Option<PathBuf>,

    /// Output path for --mem-out (default: the --mem-out file with a .mem extension)
    #[arg(long, value_name = "FILE")]
    pub(crate) mem_file: Option<PathBuf>,

    /// Entry point address for ELF / flat-binary input or net-load (hex 0x... or decimal). Optional for ELF files — read from ELF header. Required for flat binaries (default 0x20).
    #[arg(long = "entry", value_name = "ADDRESS", value_parser = parse_u32)]
    pub(crate) entry_point: Option<u32>,

    /// Input file. Type is detected from the extension: .kla assembles (ISA v2 opcode table is built in), .kbt sends a pre-built image, anything else (.elf or flat binary) converts to the board wire format
    #[arg(
        short = 'i',
        long,
        value_name = "FILE",
        required_unless_present_any = ["textmate", "opcodes", "test_list", "net_load", "mem_out", "monitor", "emulate_test"],
        conflicts_with_all = ["test_list", "emulate_test", "opcodes", "textmate"]
    )]
    pub(crate) input: Option<PathBuf>,

    /// Output info file for assembled code
    #[arg(short = 'o', long, value_name = "FILE")]
    pub(crate) output: Option<PathBuf>,

    /// Output bitcode file for assembled code
    #[arg(short = 'b', long, value_name = "FILE")]
    pub(crate) bitcode: Option<PathBuf>,

    /// Set if JSON output of opcode and macro list is required
    #[arg(long)]
    pub(crate) opcodes: bool,

    /// Set if JSON output of opcodes for use in Textmate or vscode language formatter is required
    #[arg(short = 't', long)]
    pub(crate) textmate: bool,

    /// Serial port for output. With no value, the first USB serial port found
    #[arg(
        short = 's',
        long,
        value_name = "PORT",
        num_args = 0..=1,
        value_parser = NonEmptyStringValueParser::new()
    )]
    #[allow(clippy::option_option, reason = "clap's shape for an optional-valued flag: no -s, a bare -s, or -s PORT")]
    pub(crate) serial: Option<Option<String>>,

    /// Monitor serial port for UART output after sending (Ctrl+C to stop)
    #[arg(short = 'm', long, conflicts_with_all = ["test", "mem_out", "opcodes", "textmate"])]
    pub(crate) monitor: bool,

    /// Test mode: verify UART output against expected values in source comments
    #[arg(short = 'T', long, conflicts_with = "monitor", requires = "serial")]
    pub(crate) test: bool,

    /// Timeout in seconds for test mode UART capture
    #[arg(long, value_name = "SECONDS", default_value_t = DEFAULT_TEST_TIMEOUT)]
    pub(crate) test_timeout: u64,

    /// File containing list of test .kla files to assemble and verify sequentially
    #[arg(
        short = 'L',
        long,
        value_name = "FILE",
        requires = "serial",
        conflicts_with_all = ["emulate_test", "opcodes", "textmate", "monitor"]
    )]
    pub(crate) test_list: Option<PathBuf>,

    /// Skip UART break signal and send 'S' instead (for testing CPU without break)
    #[arg(short = 'n', long)]
    pub(crate) no_break: bool,

    /// Print each received UART byte as hex alongside normal output
    #[arg(short = 'd', long)]
    pub(crate) debug: bool,

    /// Assemble the input and run it on the built-in ISA emulator (golden model); prints captured UART output
    #[arg(long)]
    pub(crate) emulate: bool,

    /// Debug an ELF on the built-in emulator: serve the GDB remote protocol on 127.0.0.1:PORT for lldb (gdb-remote / lldb-dap). UART output goes to stdout, stdin feeds the UART
    #[arg(long, value_name = "PORT")]
    pub(crate) gdbserver: Option<u16>,

    /// With --gdbserver, debug the real board over its USB-JTAG (needs a DEBUG_EN=1 bitstream and a klausscc built with --features board). With -s, the ELF is first loaded over the UART and the core halts at its entry; without -s, the running program is halted where it is
    #[arg(long, requires = "gdbserver")]
    pub(crate) board: bool,

    /// JTAG clock for --board, in kHz
    #[arg(
        long,
        value_name = "KHZ",
        requires = "board",
        value_parser = clap::value_parser!(u32).range(100..=30_000),
        default_value_t = DEFAULT_JTAG_KHZ
    )]
    pub(crate) jtag_khz: u32,

    /// With --emulate, write the per-instruction golden-model trace. Give a path to write a file, or pass --trace with no value (or '-') to stream it to stdout
    #[arg(long, value_name = "FILE", num_args = 0..=1, default_missing_value = "-", requires = "emulate")]
    pub(crate) trace: Option<PathBuf>,

    /// Run the emulator over a test list (or a directory of .kla files) and verify captured UART vs expected // values
    #[arg(long, value_name = "PATH", conflicts_with_all = ["opcodes", "textmate", "monitor"])]
    pub(crate) emulate_test: Option<PathBuf>,

    /// Instruction-count cap for the emulator
    #[arg(long, value_name = "COUNT", default_value_t = DEFAULT_MAX_INSTRUCTIONS)]
    pub(crate) max_instructions: u64,
}

/// What an `-i` input file holds, judged by its extension (case-insensitive).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InputKind {
    /// `.kla` assembly source.
    Source,
    /// A pre-built board wire-format image (`.kbt`), sent verbatim.
    Kbt,
    /// Anything else: an ELF or flat binary, converted to the wire format.
    Binary,
}

impl InputKind {
    /// Classify the input file `name` by its extension.
    pub(crate) fn of(name: &str) -> Self {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".kbt") {
            Self::Kbt
        } else if lower.ends_with(".kla") {
            Self::Source
        } else {
            Self::Binary
        }
    }
}

/// The one thing a command line asks for, with the files it names.
///
/// Options that only adjust a mode (`-n`, `-d`, `--max-instructions`, ...)
/// are read from the [`Cli`] by the mode's handler.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Mode<'a> {
    /// `-m` with no input: monitor the board's UART on this port.
    Monitor(SerialTarget<'a>),
    /// `-N FILE`: stream an ELF or flat binary to the board over TCP, then
    /// with `-m` monitor its UART.
    NetLoad {
        /// The ELF or flat binary.
        input: &'a str,
        /// The board's address (`--ip`).
        ip: &'a str,
        /// The port to monitor afterwards (`-m`): the `-s` port, else auto-detected.
        monitor: Option<SerialTarget<'a>>,
    },
    /// `--mem-out FILE`: write an ELF or flat binary as a `$readmemh` boot-ROM image.
    MemOut {
        /// The ELF or flat binary.
        input: &'a str,
        /// The image file to write (`--mem-file`, or the input's stem with `.mem`).
        mem_file: String,
    },
    /// `-i FILE --gdbserver PORT` with an ELF or flat binary: serve a debugger
    /// for it on the emulator or, with `--board`, on the board.
    Gdb {
        /// The ELF or flat binary.
        input: &'a str,
        /// The TCP port to serve the GDB remote protocol on.
        port: u16,
        /// Debug the board over JTAG (`--board`) rather than the emulator.
        board: bool,
        /// The `.kbt` file for a board load over `-s`.
        kbt: String,
    },
    /// `-i FILE --emulate` with an ELF or flat binary: run it on the emulator.
    EmulateBinary {
        /// The ELF or flat binary.
        input: &'a str,
        /// The `--trace` destination (`-` for stdout).
        trace: Option<&'a str>,
    },
    /// `-i FILE` with an ELF or flat binary: convert it to a `.kbt`, and with
    /// `-s` send it to the board.
    SendBinary {
        /// The ELF or flat binary.
        input: &'a str,
        /// The `.kbt` file to write.
        kbt: String,
    },
    /// `-i FILE.kbt`: send a pre-built image to the board.
    SendKbt(&'a str),
    /// `--opcodes` and/or `-t`: write the opcode and macro lists.
    Opcodes,
    /// `--emulate-test PATH`: run a test list or directory on the emulator.
    EmulateTest(&'a str),
    /// `-L FILE`: run a test list on the board.
    TestList {
        /// The test list.
        list: &'a str,
        /// The serial port to the board (`-s`).
        port: SerialTarget<'a>,
    },
    /// `-i FILE.kla --emulate`: assemble and run on the emulator.
    Emulate {
        /// The assembly source.
        input: &'a str,
        /// The `--trace` destination (`-` for stdout).
        trace: Option<&'a str>,
    },
    /// `-i FILE.kla`: assemble, write the `.code` listing and the `.kbt`, and
    /// with `-s` send it (and test it with `-T`, or monitor it with `-m`).
    Assemble {
        /// The assembly source.
        input: &'a str,
        /// The `.code` listing to write.
        code: String,
        /// The `.kbt` file to write.
        kbt: String,
    },
}

impl Cli {
    /// Decide which mode the command line asks for.
    ///
    /// clap has already rejected two modes at once and a flag without the
    /// one it qualifies; this checks what depends on the input file's type.
    /// A failure is an [`Error::Usage`].
    pub(crate) fn mode(&self) -> Result<Mode<'_>, Error> {
        let input = self.input.as_deref().map(utf8).transpose()?;
        if input.is_none() {
            if self.emulate {
                return Err(Error::Usage("--emulate needs an input file (-i)"));
            }
            if self.gdbserver.is_some() {
                return Err(Error::Usage("--gdbserver needs an input file (-i)"));
            }
        }

        if let Some(path) = &self.net_load {
            return Ok(Mode::NetLoad {
                input: utf8(path)?,
                ip: self.ip.as_deref().ok_or(Error::Usage("--net-load needs the board's address (--ip)"))?,
                monitor: self.monitor.then(|| self.monitor_port()),
            });
        }
        if let Some(path) = &self.mem_out {
            let input = utf8(path)?;
            let mem_file = match &self.mem_file {
                Some(mem_file) => utf8(mem_file)?.to_owned(),
                None => format!("{}.mem", filename_stem(input)),
            };
            return Ok(Mode::MemOut { input, mem_file });
        }

        let trace = self.trace.as_deref().map(utf8).transpose()?;
        if let Some(input) = input {
            let kind = InputKind::of(input);
            if self.gdbserver.is_some() && kind != InputKind::Binary {
                return Err(Error::Usage("--gdbserver debugs an ELF or flat binary, not a .kla or .kbt input"));
            }
            match kind {
                InputKind::Binary => {
                    let kbt = output_name(self.bitcode.as_deref(), input, "kbt")?;
                    return Ok(if let Some(port) = self.gdbserver {
                        Mode::Gdb {
                            input,
                            port,
                            board: self.board,
                            kbt,
                        }
                    } else if self.emulate {
                        Mode::EmulateBinary { input, trace }
                    } else {
                        Mode::SendBinary { input, kbt }
                    });
                }
                InputKind::Kbt if self.emulate => return Err(Error::Usage("--emulate runs a .kla, ELF or flat binary, not a .kbt image")),
                InputKind::Kbt => return Ok(Mode::SendKbt(input)),
                InputKind::Source => {}
            }
        } else if self.monitor {
            // `-m` (optionally with `-s`) and nothing to load: monitor only.
            return Ok(Mode::Monitor(self.monitor_port()));
        }

        if self.opcodes || self.textmate {
            return Ok(Mode::Opcodes);
        }
        if let Some(path) = &self.emulate_test {
            return Ok(Mode::EmulateTest(utf8(path)?));
        }
        if let Some(path) = &self.test_list {
            return Ok(Mode::TestList {
                list: utf8(path)?,
                port: self.serial().ok_or(Error::Usage("--test-list needs a serial port (-s)"))?,
            });
        }
        // clap requires -i whenever no other mode is given.
        let Some(input) = input else {
            return Err(Error::Usage("no input file (-i) given"));
        };
        if self.emulate {
            return Ok(Mode::Emulate { input, trace });
        }
        // A .kbt input auto-detects the port for -m, but an assembly only
        // monitors the port it sent to.
        if self.monitor && self.serial.is_none() {
            return Err(Error::Usage("monitor flag (-m) requires a serial port (-s) when assembling"));
        }
        Ok(Mode::Assemble {
            input,
            code: output_name(self.output.as_deref(), input, "code")?,
            kbt: output_name(self.bitcode.as_deref(), input, "kbt")?,
        })
    }

    /// The serial port to send to: the `-s` port, auto-detected for a bare
    /// `-s`, or `None` for no `-s` (do not send).
    pub(crate) fn serial(&self) -> Option<SerialTarget<'_>> {
        SerialTarget::from_option(self.serial.as_ref())
    }

    /// The serial port to monitor: as [`serial`](Self::serial), but
    /// auto-detected without `-s`.
    fn monitor_port(&self) -> SerialTarget<'_> {
        self.serial().unwrap_or(SerialTarget::Auto)
    }

    /// How to reset the board into its loader: a UART break, or with `-n`
    /// the loader's `S` command.
    pub(crate) const fn reset_method(&self) -> ResetMethod {
        if self.no_break { ResetMethod::Command } else { ResetMethod::Break }
    }

    /// The binary input file `path`, with the `--entry` override.
    pub(crate) const fn binary_input<'a>(&self, path: &'a str) -> BinaryInput<'a> {
        BinaryInput {
            path,
            entry: self.entry_point,
        }
    }
}

/// Name an output file for `input`: `explicit` (`-o` / `-b`) if given, else
/// the input with `extension` (see [`output_file_name`]).
fn output_name(explicit: Option<&Path>, input: &str, extension: &str) -> Result<String, Error> {
    Ok(output_file_name(explicit.map(utf8).transpose()?, input, extension))
}

/// `path` as a `&str`: the handlers take file names as UTF-8 strings.
fn utf8(path: &Path) -> Result<&str, Error> {
    path.to_str().ok_or(Error::Usage("file names must be valid UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    // clap's own consistency checks: duplicate ids or short flags, dangling
    // `requires`/`conflicts_with` names, bad defaults. These otherwise only
    // surface as a panic when the offending flag is used.
    fn cli_debug_assert() {
        Cli::command().debug_assert();
    }

    /// Parse `args` (without the program name).
    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("klausscc").chain(args.iter().copied()))
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
        let entry = |value: &str| parse(&["-i", "prog.elf", "--entry", value]).map(|cli| cli.entry_point);
        assert_eq!(entry("0x40").unwrap(), Some(0x40));
        assert_eq!(entry("64").unwrap(), Some(64));
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
    // Combinations the old early-return chain silently ignored part of.
    fn nonsensical_combinations_are_rejected() {
        assert_rejected(&[
            // Two modes at once.
            &["--emulate-test", "tests", "-i", "prog.kla"],
            &["--emulate-test", "tests", "-i", "prog.elf"],
            &["--emulate-test", "tests", "--opcodes"],
            &["--emulate-test", "tests", "-t"],
            &["--emulate-test", "tests", "-L", "list.txt", "-s", "COM3"],
            &["--emulate-test", "tests", "--mem-out", "prog.elf"],
            &["--emulate-test", "tests", "-N", "prog.elf", "--ip", "10.0.0.2"],
            &["-L", "list.txt", "-s", "COM3", "--opcodes"],
            &["-L", "list.txt", "-s", "COM3", "-t"],
            // -m with a mode that never monitors.
            &["-m", "--emulate-test", "tests"],
            &["-m", "--mem-out", "prog.elf"],
            &["-m", "--opcodes"],
            &["-m", "-t"],
            // A flag without the mode it qualifies.
            &["-i", "prog.kla", "--trace"],
            &["-i", "prog.elf", "--gdbserver", "1234", "--jtag-khz", "5000"],
            &["--opcodes", "--jtag-khz", "5000"],
            // What used to be reported only once the work was done.
            &["-N", "prog.elf"],
            &["-N", "prog.elf", "--ip", ""],
            &["-L", "list.txt"],
            &["-i", "prog.kla", "-T"],
            &["-i", "prog.kla", "-s", ""],
        ]);
    }

    #[test]
    // Rules that depend on the input file's type are checked by Cli::mode.
    fn input_type_rules_are_usage_errors() {
        for args in [
            &["-i", "prog.kla", "--gdbserver", "1234"][..],
            &["-i", "prog.kbt", "--gdbserver", "1234"],
            &["-i", "prog.kbt", "--emulate"],
            &["-i", "prog.kla", "-m"],
            // --emulate and --gdbserver qualify -i.
            &["--emulate-test", "tests", "--emulate"],
            &["-m", "--emulate"],
            &["--opcodes", "--gdbserver", "1234"],
            &["-N", "prog.elf", "--ip", "10.0.0.2", "--gdbserver", "1234"],
        ] {
            let cli = parse(args).unwrap();
            assert!(matches!(cli.mode(), Err(Error::Usage(_))), "{args:?}");
        }
    }

    #[test]
    // clap's PathBuf parser rejects an empty file name.  It used to reach the
    // handlers: an unopenable input, or a file named just .code or .kbt.
    fn empty_file_names_are_rejected() {
        assert_rejected(&[
            &["-i", ""],
            &["-i", "prog.kla", "-o", ""],
            &["-i", "prog.kla", "-b", ""],
            &["-N", "", "--ip", "10.0.0.2"],
            &["--mem-out", ""],
            &["--mem-out", "prog.elf", "--mem-file", ""],
            &["-L", "", "-s", "COM3"],
            &["--emulate-test", ""],
            &["-i", "prog.kla", "--emulate", "--trace="],
        ]);
    }

    #[test]
    // -s with no value asks for the board's USB serial port.
    fn serial_port_is_optional_valued() {
        let serial = |args: &[&str]| parse(args).unwrap().serial;
        assert_eq!(serial(&["-i", "prog.kla"]), None);
        assert_eq!(serial(&["-i", "prog.kla", "-s"]), Some(None));
        assert_eq!(serial(&["-i", "prog.kla", "-s", "-m"]), Some(None));
        assert_eq!(serial(&["-s", "/dev/ttyUSB1", "-i", "prog.kla"]), Some(Some("/dev/ttyUSB1".to_owned())));
        assert_eq!(serial(&["-i", "prog.kla", "--serial=COM3"]), Some(Some("COM3".to_owned())));
    }

    #[test]
    // No -s sends nothing, a bare -s auto-detects, and monitoring auto-detects without -s.
    fn serial_targets() {
        let cli = parse(&["-i", "prog.kla"]).unwrap();
        assert_eq!((cli.serial(), cli.monitor_port()), (None, SerialTarget::Auto));
        let cli = parse(&["-i", "prog.kla", "-s"]).unwrap();
        assert_eq!((cli.serial(), cli.monitor_port()), (Some(SerialTarget::Auto), SerialTarget::Auto));
        let cli = parse(&["-i", "prog.kla", "-s", "COM3"]).unwrap();
        assert_eq!(
            (cli.serial(), cli.monitor_port()),
            (Some(SerialTarget::Port("COM3")), SerialTarget::Port("COM3"))
        );
    }

    #[test]
    fn no_break_resets_with_the_loader_command() {
        assert_eq!(parse(&["-i", "prog.kla"]).unwrap().reset_method(), ResetMethod::Break);
        assert_eq!(parse(&["-i", "prog.kla", "-n"]).unwrap().reset_method(), ResetMethod::Command);
    }

    #[test]
    // --trace with no value streams the trace to stdout.
    fn trace_is_optional_valued() {
        let trace = |args: &[&str]| parse(args).unwrap().trace;
        assert_eq!(trace(&["-i", "prog.kla", "--emulate"]), None);
        assert_eq!(trace(&["-i", "prog.kla", "--emulate", "--trace"]), Some(PathBuf::from("-")));
        assert_eq!(trace(&["-i", "prog.kla", "--emulate", "--trace", "t.txt"]), Some(PathBuf::from("t.txt")));
    }

    #[test]
    fn values_reach_their_options() {
        let cli = parse(&["-i", "prog.kla", "-o", "o.code", "-b", "o.kbt", "-s", "COM3", "-T", "-n", "-d"]).unwrap();
        assert_eq!(cli.input, Some(PathBuf::from("prog.kla")));
        assert_eq!(cli.output, Some(PathBuf::from("o.code")));
        assert_eq!(cli.bitcode, Some(PathBuf::from("o.kbt")));
        assert!(cli.test && cli.no_break && cli.debug);
        assert!(!cli.monitor && !cli.emulate);

        let cli = parse(&["-i", "prog.elf", "--gdbserver", "1234", "--board", "--jtag-khz", "2500"]).unwrap();
        assert_eq!(cli.gdbserver, Some(1234));
        assert!(cli.board);
        assert_eq!(cli.jtag_khz, 2500);

        let cli = parse(&["-N", "prog.elf", "--ip", "10.0.0.2", "--port", "6000"]).unwrap();
        assert_eq!(cli.net_load, Some(PathBuf::from("prog.elf")));
        assert_eq!(cli.ip.as_deref(), Some("10.0.0.2"));
        assert_eq!(cli.port, 6000);

        let cli = parse(&["--mem-out", "prog.elf", "--mem-file", "boot.mem"]).unwrap();
        assert_eq!(cli.mem_out, Some(PathBuf::from("prog.elf")));
        assert_eq!(cli.mem_file, Some(PathBuf::from("boot.mem")));

        let cli = parse(&["-L", "list.txt", "-s", "COM3", "--test-timeout", "20"]).unwrap();
        assert_eq!(cli.test_list, Some(PathBuf::from("list.txt")));
        assert_eq!(cli.test_timeout, 20);

        let cli = parse(&["--emulate-test", "dir", "--max-instructions", "1000"]).unwrap();
        assert_eq!(cli.emulate_test, Some(PathBuf::from("dir")));
        assert_eq!(cli.max_instructions, 1000);
    }

    #[test]
    fn numeric_options_have_defaults() {
        let cli = parse(&["-i", "prog.kla"]).unwrap();
        assert_eq!(cli.test_timeout, DEFAULT_TEST_TIMEOUT);
        assert_eq!(cli.test_timeout, 10);
        assert_eq!(cli.max_instructions, DEFAULT_MAX_INSTRUCTIONS);
        assert_eq!(cli.port, NETBOOT_DEFAULT_PORT);
        assert_eq!(cli.jtag_khz, DEFAULT_JTAG_KHZ);
        assert_eq!(cli.entry_point, None);
    }

    #[test]
    // These used to fall back to their defaults without a word.
    fn bad_numbers_are_usage_errors() {
        assert_rejected(&[
            &["-i", "prog.kla", "-s", "COM3", "-T", "--test-timeout", "ten"],
            &["-i", "prog.kla", "-s", "COM3", "-T", "--test-timeout", "-1"],
            &["-i", "prog.kla", "--emulate", "--max-instructions", "lots"],
            &["--emulate-test", "dir", "--max-instructions", "1e6"],
            &["-N", "prog.elf", "--ip", "10.0.0.2", "--port", "http"],
            &["-N", "prog.elf", "--ip", "10.0.0.2", "--port", "65536"],
        ]);
    }

    /// Assert that `args` select `expected`.
    fn assert_mode(args: &[&str], expected: &Mode<'_>) {
        let cli = parse(args).unwrap();
        assert_eq!(&cli.mode().unwrap(), expected, "{args:?}");
    }

    #[test]
    fn input_kind_is_judged_by_extension() {
        assert_eq!(InputKind::of("prog.kla"), InputKind::Source);
        assert_eq!(InputKind::of("dir.v2/PROG.KLA"), InputKind::Source);
        assert_eq!(InputKind::of("prog.kbt"), InputKind::Kbt);
        assert_eq!(InputKind::of("Prog.KBT"), InputKind::Kbt);
        assert_eq!(InputKind::of("prog.elf"), InputKind::Binary);
        assert_eq!(InputKind::of("prog"), InputKind::Binary);
        assert_eq!(InputKind::of("prog.kla.bin"), InputKind::Binary);
    }

    #[test]
    fn each_mode_is_selected() {
        assert_mode(&["-m"], &Mode::Monitor(SerialTarget::Auto));
        assert_mode(&["-m", "-s"], &Mode::Monitor(SerialTarget::Auto));
        assert_mode(&["-m", "-s", "COM3", "-d"], &Mode::Monitor(SerialTarget::Port("COM3")));
        assert_mode(
            &["-N", "prog.elf", "--ip", "10.0.0.2", "-m"],
            &Mode::NetLoad {
                input: "prog.elf",
                ip: "10.0.0.2",
                monitor: Some(SerialTarget::Auto),
            },
        );
        assert_mode(
            &["-N", "prog.elf", "--ip", "10.0.0.2", "-m", "-s", "COM3"],
            &Mode::NetLoad {
                input: "prog.elf",
                ip: "10.0.0.2",
                monitor: Some(SerialTarget::Port("COM3")),
            },
        );
        assert_mode(
            &["-N", "prog.elf", "--ip", "10.0.0.2"],
            &Mode::NetLoad {
                input: "prog.elf",
                ip: "10.0.0.2",
                monitor: None,
            },
        );
        assert_mode(
            &["--mem-out", "dir/prog.elf"],
            &Mode::MemOut {
                input: "dir/prog.elf",
                mem_file: "dir/prog.mem".to_owned(),
            },
        );
        assert_mode(
            &["--mem-out", "prog.elf", "--mem-file", "boot.mem"],
            &Mode::MemOut {
                input: "prog.elf",
                mem_file: "boot.mem".to_owned(),
            },
        );
        assert_mode(
            &["-i", "prog.elf", "--gdbserver", "1234", "--board"],
            &Mode::Gdb {
                input: "prog.elf",
                port: 1234,
                board: true,
                kbt: "prog.kbt".to_owned(),
            },
        );
        assert_mode(
            &["-i", "prog.elf", "--emulate", "--trace"],
            &Mode::EmulateBinary {
                input: "prog.elf",
                trace: Some("-"),
            },
        );
        assert_mode(
            &["-i", "prog.bin", "-b", "out"],
            &Mode::SendBinary {
                input: "prog.bin",
                kbt: "out.kbt".to_owned(),
            },
        );
        assert_mode(&["-i", "prog.kbt", "-m"], &Mode::SendKbt("prog.kbt"));
        assert_mode(&["--opcodes"], &Mode::Opcodes);
        assert_mode(&["-t", "--opcodes"], &Mode::Opcodes);
        assert_mode(&["--emulate-test", "tests"], &Mode::EmulateTest("tests"));
        assert_mode(
            &["-L", "list.txt", "-s", "COM3"],
            &Mode::TestList {
                list: "list.txt",
                port: SerialTarget::Port("COM3"),
            },
        );
        assert_mode(
            &["-L", "list.txt", "-s"],
            &Mode::TestList {
                list: "list.txt",
                port: SerialTarget::Auto,
            },
        );
        assert_mode(
            &["-i", "prog.kla", "--emulate", "--trace", "t.txt"],
            &Mode::Emulate {
                input: "prog.kla",
                trace: Some("t.txt"),
            },
        );
        assert_mode(
            &["-i", "dir/prog.kla", "-s", "COM3", "-m"],
            &Mode::Assemble {
                input: "dir/prog.kla",
                code: "dir/prog.code".to_owned(),
                kbt: "dir/prog.kbt".to_owned(),
            },
        );
        assert_mode(
            &["-i", "prog.kla", "-o", "out.code", "-b", "out"],
            &Mode::Assemble {
                input: "prog.kla",
                code: "out.code".to_owned(),
                kbt: "out.kbt".to_owned(),
            },
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_file_name_is_a_usage_error() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt as _;
        let name = OsString::from_vec(b"bad\xFF.kla".to_vec());
        let cli = Cli::try_parse_from([OsString::from("klausscc"), OsString::from("-i"), name]).unwrap();
        assert!(matches!(cli.mode(), Err(Error::Usage(_))));
    }
}
