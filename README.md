# klausscc

The toolchain for **Klauss**, a custom 64-bit CPU implemented on an FPGA
(64-bit registers; 32-bit instruction words and byte addresses).
One binary, `klausscc`, does everything needed to get a program onto the CPU:

- **Assembler**: `.kla` assembly source to a `.code` listing and a `.kbt`
  board image. The opcode table (ISA encoding v2, with the v3 additions:
  short one-word forms, fused compare-and-branch, `ENTER`/`LEAVE` and the
  32-bit `W` operations) and the standard macros are built in.
- **Loaders**: send a `.kbt`, an ELF or a flat binary to the board over the
  UART, stream one over TCP to the board's network boot server, or write a
  `$readmemh` boot-ROM image.
- **Emulator**: a golden-model ISA emulator that runs a program and prints
  its UART output, optionally with a per-instruction trace.
- **Debugger**: a GDB remote-protocol server for lldb, debugging a program on
  the emulator or on the real board over its USB-JTAG.
- **Test runner**: checks a program's UART output against the expected values
  in its comments, on the board or on the emulator.

## Building

Requires Rust 1.88 or later.

```sh
cargo build --release      # target/release/klausscc
```

### The `board` feature

Debugging the real board (`--gdbserver PORT --board`) talks to the FT2232H
JTAG adapter through the system **libftdi1**, so it is behind a cargo feature:

```sh
# Debian/Ubuntu: sudo apt-get install libftdi1-dev
# macOS:         brew install libftdi
cargo build --release --features board
```

`build.rs` finds libftdi1 with `pkg-config` when it is installed. Without the
feature everything else works, and `--board` reports that this build does not
support it.

## Usage

The input's extension picks what `-i` does: `.kla` is assembled, `.kbt` is a
pre-built image to send, and anything else (`.elf` or a flat binary) is
converted to the board's wire format. `klausscc --help` lists every option.

```sh
# Assemble: writes prog.code (listing) and prog.kbt (image)
klausscc -i prog.kla
klausscc -i prog.kla -o listing.code -b image.kbt

# Assemble and send to the board over the UART (no port: auto-detect the
# board's FTDI USB-UART), then monitor its output (Ctrl+C to stop)
klausscc -i prog.kla -s
klausscc -i prog.kla -s /dev/ttyUSB1 -m
klausscc -m                                # just open the serial monitor

# Send a pre-built image or an ELF
klausscc -i prog.kbt -s
klausscc -i prog.elf -s -m

# Network boot: stream an ELF to the board's netboot server over TCP
klausscc -N prog.elf --ip 192.168.68.50 [--port 5000] [-m]

# Boot ROM: write a $readmemh image (prog.mem) for boot_rom.sv
klausscc --mem-out prog.elf [--mem-file boot.mem]

# Run on the emulator, optionally tracing every instruction
klausscc -i prog.kla --emulate
klausscc -i prog.elf --emulate --trace trace.txt --max-instructions 1000000

# Debug with lldb (gdb-remote 127.0.0.1:3333) on the emulator, or on the
# board over JTAG (needs the board feature)
klausscc -i prog.elf --gdbserver 3333
klausscc -i prog.elf --gdbserver 3333 --board [-s]

# Test: check UART output against the values in the source's comments
klausscc -i test_regs.kla -s -T [--test-timeout 20]    # one program, on the board
klausscc -L tests/fixtures/all_tests.txt -s            # a test list, on the board
klausscc --emulate-test tests/fixtures                 # a list or directory, on the emulator

# Export the opcode and macro tables (HTML + JSON), or the TextMate opcode list
klausscc --opcodes
klausscc --textmate
```

Exit status: `0` success, `1` error (including assembly errors), `2` tests
failed, `3` a test timed out.

A failed build still writes the `.code` listing, so you can see where the
errors are; the `.kbt` is not written.

## Testing

```sh
cargo test
```

This runs the unit tests, the command-line tests in `tests/cli.rs`, the
assembler-to-emulator round trip in `tests/roundtrip.rs` and the fixture
corpus. [`tests/fixtures/`](tests/fixtures/README.md) holds the CPU test
programs. Most of them list the UART values they should print, and `cargo
test` assembles each of those and checks the values on the emulator (the same
check as `klausscc --emulate-test tests/fixtures`). The ELF fixtures are
v1-era builds, used only by the loader's ELF-parsing tests.

The board debugger's tests run against a model of the debug module, so they
need neither the cable nor libftdi.

## Repository layout

```
src/
  main.rs           the binary: calls klausscc::run()
  lib.rs            run(): parse the command line, run the mode, exit status
  cli.rs            clap command line and the Mode it selects
  commands/         one handler per mode: load, emulate, gdb, test_runner
  error.rs          the Error type and exit codes
  diag.rs           Diagnostics: errors, warnings and notes for the user
  layout.rs         the memory map (heap header, code base, MMIO window)
  num.rs            integer literal parsing
  asm/              the assembler: source, lexer, classify, macros, symbols,
                    data, pass1, pass2, line
  isa/              the opcode table (v2 + v3), encoder and disassembler
  output/           the .code listing, program image formats, opcode docs
  loader.rs         ELF / flat binary to a board program
  emu/              the golden-model emulator: exec, mmio, flags, debug, trace
  debug/            GDB servers: emulator target, board target, shared arch
  jtag/             FT2232H MPSSE JTAG and the debug module's DMI
  link/             serial (upload, monitor, UART tests) and netload
tests/              integration tests and the fixture corpus
runtime/            C runtime for compiled programs: crt0.kla (startup),
                    libc.kla and the UART routines it calls (uart_stubs.kla)
doc/                ISA and emulator documentation
build.rs            locates libftdi1 for the board feature
```

## Documentation

- [doc/ISA_ENCODING_V2.md](doc/ISA_ENCODING_V2.md): the current instruction
  set and its encodings.
- [doc/EMULATOR_ISA_SEMANTICS.md](doc/EMULATOR_ISA_SEMANTICS.md): the
  emulator's execution semantics (including the unified `Z S C V` flags) and
  trace format.
- [doc/klauss_isa_reference.md](doc/klauss_isa_reference.md): the assembly
  language and command-line reference.
- `cargo doc --no-deps --document-private-items --open`: the code's own docs.

## License

MIT
