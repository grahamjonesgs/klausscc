# Klauss ISA Assembly Language Reference

This document describes the assembly language of the Klauss CPU and its assembler, **klausscc**. The Klauss CPU is a custom 64-bit CPU on an FPGA (a Nexys A7 board with 128 MiB of DDR2): sixteen 64-bit registers, 32-bit byte addresses, little-endian memory, and instructions of one, two or three 32-bit words.

The instruction set is ISA encoding v2 plus the v3 additions (short one-word forms, fused compare-and-branch, `ENTER`/`LEAVE`, 32-bit `W` operations). This reference covers what you write in a `.kla` file and what each instruction does. For the bit-level encodings, see [ISA_ENCODING_V2.md](ISA_ENCODING_V2.md). For the emulator's corner cases and trace format, see [EMULATOR_ISA_SEMANTICS.md](EMULATOR_ISA_SEMANTICS.md).

Where the built-in emulator and the hardware disagree, this document describes the hardware and says what the emulator does instead. Those cases are collected in [Emulator and Hardware Differences](#emulator-and-hardware-differences).

## Table of Contents

- [Getting Started](#getting-started)
- [Assembler Usage](#assembler-usage)
- [Source File Format](#source-file-format)
- [Registers](#registers)
- [Numeric Literals](#numeric-literals)
- [Labels](#labels)
- [Comments](#comments)
- [Data Directives](#data-directives)
- [File Includes](#file-includes)
- [Macros](#macros)
- [Instruction Reference](#instruction-reference)
  - [ALU, Register-Register](#alu-register-register)
  - [ALU, Immediate](#alu-immediate)
  - [Compare](#compare)
  - [Fused Compare-and-Branch](#fused-compare-and-branch)
  - [Shift and Rotate](#shift-and-rotate)
  - [Bit Manipulation](#bit-manipulation)
  - [Unary](#unary)
  - [Loads](#loads)
  - [Stores](#stores)
  - [Jumps and Calls](#jumps-and-calls)
  - [Short Jumps and Calls](#short-jumps-and-calls)
  - [Stack](#stack)
  - [Multiply and Divide](#multiply-and-divide)
  - [System and LCD](#system-and-lcd)
  - [Separate Source Registers](#separate-source-registers)
  - [LLVM Names](#llvm-names)
- [CPU Flags](#cpu-flags)
- [Memory Layout](#memory-layout)
- [Memory-Mapped I/O](#memory-mapped-io)
- [Interrupts](#interrupts)
- [Emulator and Hardware Differences](#emulator-and-hardware-differences)
- [Serial Communication](#serial-communication)
  - [Monitoring UART Output](#monitoring-uart-output)
  - [Automated Test Mode](#automated-test-mode)
- [Writing Test Programs](#writing-test-programs)
- [C Programs and the Runtime](#c-programs-and-the-runtime)
- [Complete Examples](#complete-examples)
- [Output Files](#output-files)
- [Assembly Process](#assembly-process)

---

## Getting Started

A minimal Klauss assembly program that counts from 1 to 5 and prints each value on the UART:

```
_start
SETR A 0            // A = 0
LOOP:
INCR A              // A = A + 1
$TXR A              // print A as 8 hex digits on the UART
$NEWLINE
CMPRV A 5           // flags from A - 5
JMPNE LOOP:         // loop until A = 5
HALT

!include uart_stubs.kla   // the UART print routines $TXR / $NEWLINE call
```

`$TXR` and `$NEWLINE` are built-in macros. They call print routines in `uart_stubs.kla`, which is in `tests/fixtures/`. Copy it next to your program. Save the program as `myprogram.kla` and assemble it with:

```sh
klausscc -i myprogram.kla
```

This writes `myprogram.code` (a listing) and `myprogram.kbt` (the image to send to the board). Run it on the built-in emulator with:

```sh
klausscc -i myprogram.kla --emulate
```

The ISA opcode table and the standard macros are built into the assembler, so no opcode file is needed.

---

## Assembler Usage

```
klausscc [OPTIONS]
```

The input's extension picks what `-i` does: a `.kla` file is assembled, a `.kbt` file is a pre-built image to send, and anything else (an `.elf` or a flat binary) is converted to the board's wire format. `klausscc --help` lists every option.

### Assembling and sending

| Flag | Description |
|------|-------------|
| `-i`, `--input <FILE>` | Input file (`.kla`, `.kbt`, `.elf` or flat binary) |
| `-o`, `--output <FILE>` | Listing file name (default: input stem + `.code`) |
| `-b`, `--bitcode <FILE>` | Binary output file name (default: input stem + `.kbt`) |
| `-s`, `--serial [PORT]` | Send the program to the board over this serial port. Without a value, klausscc picks the board's FTDI USB-UART itself (see [Uploading Programs](#uploading-programs)). |
| `-m`, `--monitor` | After sending, print the board's UART output and forward keystrokes (Ctrl+C to stop). On its own, `-m` just opens the monitor. |
| `-n`, `--no-break` | Reset the board by sending `S` instead of a UART break |
| `-d`, `--debug` | Print each received UART byte as hex alongside normal output |
| `--entry <ADDRESS>` | Entry point for ELF / flat-binary input (read from the ELF header if not given; flat binaries default to `0x20`) |

### Testing

| Flag | Description |
|------|-------------|
| `-T`, `--test` | Test mode: send to the board and check its UART output against the expected values in the source's comments. See [Automated Test Mode](#automated-test-mode). Needs `-s`. |
| `--test-timeout <SECONDS>` | Timeout for test mode UART capture (default: 10) |
| `-L`, `--test-list <FILE>` | Assemble, send and check each `.kla` file listed in `FILE` in turn. Needs `-s`. |
| `--emulate-test <PATH>` | Check a test list (or a directory of `.kla` files) on the built-in emulator instead of the board. Each program starts with the UART idle, as on the board. A file that writes to the UART while it is busy fails, since it would lose output on the board |

### Emulator and debugger

| Flag | Description |
|------|-------------|
| `--emulate` | Assemble the input (or load the ELF or flat binary) and run it on the built-in ISA emulator, starting with the UART idle as on the board; prints the captured UART output, and any write to the UART made while it was busy (see [UART](#uart--0xf001_0000)) |
| `--trace [FILE]` | With `--emulate`, write the per-instruction trace to `FILE`, or to stdout with no value (or `-`) |
| `--max-instructions <COUNT>` | Instruction-count cap for the emulator (default: 50000000) |
| `--gdbserver <PORT>` | Debug an ELF on the emulator over the GDB remote protocol on `127.0.0.1:PORT` (for lldb) |
| `--board` | With `--gdbserver`, debug the real board over its USB-JTAG instead (needs a `DEBUG_EN=1` bitstream and a klausscc built with `--features board`). With `-s`, the ELF is first loaded over the UART. |
| `--jtag-khz <KHZ>` | JTAG clock for `--board` (default: 10000) |

### Network boot and boot ROM

| Flag | Description |
|------|-------------|
| `-N`, `--net-load <FILE>` | Flatten an ELF (or take a flat binary) and stream it to the board over TCP. Needs `--ip`. |
| `--ip <ADDRESS>` | Board IP address or host name for `--net-load` |
| `--port <PORT>` | Board TCP port for `--net-load` (default: 5000) |
| `--mem-out <FILE>` | Write a `$readmemh` boot-ROM image of an ELF or flat binary |
| `--mem-file <FILE>` | Output path for `--mem-out` (default: the input with a `.mem` extension) |

### Documentation

| Flag | Description |
|------|-------------|
| `--opcodes` | Write the opcode and macro tables as HTML and JSON to the current directory, then exit |
| `-t`, `--textmate` | Write the opcode list for TextMate/VSCode syntax highlighting, then exit |
| `-h`, `--help` / `-V`, `--version` | Print help or the version |

Note: `-m` and `-T` are mutually exclusive, and `-T` requires `-s`. Flag combinations that make no sense are rejected before any work is done.

### Examples

Assemble a program:
```sh
klausscc -i count.kla
```

Assemble and upload to the FPGA board over serial (auto-detect port):
```sh
klausscc -i count.kla -s
```

Assemble, upload, and monitor UART output:
```sh
klausscc -i count.kla -s -m
```

Assemble, upload, and run automated verification:
```sh
klausscc -i test_bits.kla -s --test
```

Run a program on the emulator, or check the test corpus there:
```sh
klausscc -i test_bits.kla --emulate
klausscc --emulate-test tests/fixtures
```

Generate ISA documentation:
```sh
klausscc --opcodes
```

---

## Source File Format

Assembly source files use the `.kla` extension. Each line holds one of:

- An instruction with its operands
- A label definition (`name:`)
- A data definition (`#name ...`) or directive (`.word`, `.data`, ...)
- A macro call (`$NAME ...`) or definition (`!macro`, `!macros`)
- A file include (`!include`)
- The `_start` marker
- A comment, or nothing

Operands are separated by **spaces, not commas**. `ADDR A, B, C` is an error ("Incorrect register definition"); write `ADDR A B C`.

Instruction mnemonics and register names are **case-insensitive** (`NOP`, `nop` and `Nop` are the same). Labels and data names are also case-insensitive. Macro names, directives (`.word`, `!include`, ...) are not: write them as shown.

### The `_start` Marker

A program must contain exactly one `_start` line. Execution begins at the instruction after it:

```
_start
SETR A 0x1
```

Code is always placed from address `0x20` in source order (see [Memory Layout](#memory-layout)). `_start` does not move code; it only sets the entry point stored in the `.kbt` image. If `_start` is missing, or appears twice, no `.kbt` is written.

---

## Registers

The CPU has **16 general-purpose 64-bit registers**, named `A` through `P` (R0 to R15):

| Register | Number | Register | Number |
|----------|--------|----------|--------|
| A | 0 | I | 8 |
| B | 1 | J | 9 |
| C | 2 | K | 10 |
| D | 3 | L | 11 |
| E | 4 | M | 12 |
| F | 5 | N | 13 |
| G | 6 | O | 14 |
| H | 7 | P | 15 |

- All sixteen are 64 bits wide, and none is hardwired to zero.
- `P` (R15) is the frame pointer by convention. `ENTER`, `LEAVE` and `LEAVERET` use it implicitly.
- **SP**, the stack pointer, is a separate 32-bit register, not one of `A`–`P`. It is read and written with `GETSP`, `SETSP` and `ADDSP`, and used by `PUSH`, `POP`, `CALL`, `RET` and interrupts. The stack grows down, and every slot is 8 bytes.
- **PC** is a 32-bit byte address. Instructions are 4-byte aligned.
- The **flags** are `Z S C V` (zero, sign, carry, overflow). See [CPU Flags](#cpu-flags).

Addresses are 32 bits. Where an instruction uses a register as an address, it uses the register's low 32 bits.

---

## Numeric Literals

Every number in the source (immediates, counts, `.word`, `.space`, `#name N`) uses the same forms:

| Form | Examples | Notes |
|------|----------|-------|
| Decimal | `42`, `-1`, `+7` | Optional `-` or `+` sign. No `_` separators. |
| Hexadecimal | `0xFF`, `0X1_0000`, `-0x10` | `0x` or `0X` prefix, digits in any case, `_` allowed between digits, optional sign. |

There are no binary, octal or character literals. The magnitude must fit in 64 bits.

How big a value may be depends on the operand:

| Operand | Accepts |
|---------|---------|
| 32-bit immediate (`imm32`, `off32`, `addr32`, `target`, ...) | `-2147483648` to `4294967295`, or a label. A hex literal is read as a 64-bit pattern, so `0xFFFFFFFFFFFFFFFF` is `-1` and fits. The CPU sign- or zero-extends the word to 64 bits, depending on the instruction (see the tables). |
| 64-bit immediate (`SETR64`, `PUSHV64`) | One literal from `-2^63` to `2^64 - 1`, or two 32-bit words (low, then high). |
| Shift / bit count `n` | `0` to `63` |
| Short immediate `imm8` | A literal only: `-128` to `127` for the sign-extending `.S` forms (`SETR.S`, `ADDI.S`, `CMPRV.S`), `0` to `255` for the zero-extending ones (`ADDV.S`, `MINUSV.S`, `ANDV.S`, `ORV.S`, `XORV.S`). Any other value is an error (see [ALU, Immediate](#alu-immediate)). |
| `simm4` (fused branches) | A literal from `-8` to `7` |
| `.word` | Any 64-bit value, negative or not; not a label |

`SETR` sign-extends its 32-bit immediate, so `SETR A 0xFFFFFFFF` sets A to `0xFFFF_FFFF_FFFF_FFFF`, and `SETR A 0x80000000` sets it to `0xFFFF_FFFF_8000_0000`. When a `SETR` literal does not fit the 32-bit range, the assembler emits `SETR64` instead:

```
SETR A 0x1_0000          // hex with separators
SETR B -1                // B = 0xFFFF_FFFF_FFFF_FFFF
SETR C 0x1_0000_0000     // needs 33 bits: assembled as SETR64
SETR64 D 0x11223344 0x55667788   // two words: D = 0x5566778811223344
```

---

## Labels

A label marks an address. Define it with a name and a colon as the first word of a line:

```
MY_LABEL:
    SETR A 0x1
    JMP MY_LABEL:
```

**Rules:**
- A label stands alone on its line. Any text after it is ignored, with a warning.
- Labels are case-insensitive, and the colon is optional when you use one: `JMP loop:`, `JMP LOOP:` and `JMP loop` all name the same label.
- Defining a name twice at different addresses is an error. Using a label that is not defined is an error.
- A label's value is its 32-bit byte address. Use it anywhere a 32-bit immediate or a branch target is expected, for example `SETR A LOOP:` to load an address, or `MEMREADR A COUNT:` to load the doubleword at a label. Short literal fields (`imm8`, `off`, `simm4`, `n`) take numbers only.

---

## Comments

### Line Comments

`//` starts a comment that runs to the end of the line. It may follow code:

```
SETR A 0xB       // Value to store
// This entire line is a comment
```

### Block Comments

`/* ... */` comments may span lines:

```
/* This is a
   multi-line comment */
```

Block comments do **not** nest: the first `*/` ends the comment. A block comment cannot continue into another file, and one that is never closed is an error. A `/*` inside a `//` comment or a string still opens a block comment.

---

## Data Directives

Data is placed in memory where it appears in the source, unless it is in a data section (see [Sections](#sections)). Execution does not skip data, so put data where the program never runs into it: after `HALT`, or in a `.data` section.

| Directive | Emits |
|-----------|-------|
| `#name N` | `N` zeroed 64-bit words (8 × N bytes), and defines `#name` as their address. `N` is 1 to 16777216 (128 MiB). |
| `#name "text"` | A NUL-terminated string (layout below), and defines `#name` as its address |
| `.word V` | One 64-bit word holding `V`, little-endian |
| `.space N` | `N` zero bytes, rounded up to a whole number of 8-byte words. `N` is 1 to 128 MiB. |
| `.zeropad N` | Exactly `N` zero bytes. `N` must be a multiple of 4. |
| `.comm name, size[, align]` / `.lcomm name size` | A label `name:` plus `.space size`, placed in the data section |
| `.global`, `.globl`, `.extern` | Accepted and ignored |
| `.text`, `.data`, `.rodata`, `.bss` | Section switches; see [Sections](#sections) |

Every data item except `.zeropad` is a whole number of 8-byte words. `.word` takes literals only; to store a label's address, load it into a register and store that.

### Referencing Data

`#name` stands for the address of the data, wherever an immediate is expected:

```
SETR A #TABLE            // A = address of TABLE
SETR B 0x2A
MEMSET64 B A             // TABLE[0] = 0x2A
SETR B -1
STIDX64A B A 8           // TABLE[1] = -1
LDIDX64A C A 8           // C = TABLE[1]
HALT
#TABLE 3                 // after HALT, so it is never executed
```

### Strings

`#name "text"` stores a C string:

- the characters in memory order, **first character at the lowest address** (`#name` is its address), then
- a NUL byte, then
- NUL padding to a whole number of 8-byte words.

`\n` in the text becomes carriage return + line feed (`\r\n`); there are no other escapes. A `//` comment after the closing quote is an error ("Error in data definition"); put the comment on its own line. A `//` inside the quotes is stored as written, but the assembler warns "has no string termination".

A byte load (`MEMGET8`, `LDIDX8`) reads the characters back in order, and `$TXSTR` prints the string as written (see `tests/fixtures/test_strings.kla` and `tests/fixtures/test_txstr.kla`). It is the same layout C uses for a string literal, so C code can take the address too. Memory is little-endian, so `MEMGET32` at `#S` for `#S "Hello"` reads `0x6C6C6548` ("lleH"), and the listing shows the string as `6C6C65480000006F`.

### Sections

`.data`, `.rodata` and `.bss` start a data section, and `.text` returns to code. Data and labels in a data section are moved after all the code. The moved data starts on an 8-byte boundary (the assembler adds a `.zeropad 4` when it needs to). Data outside a data section stays where it is written:

```
_start
SETR A #TABLE            // A = address of TABLE
SETR B 7
STIDX64A B A 16          // TABLE[2] = 7
MEMREADR E COUNT:        // E = 64-bit word at label COUNT: (100)
HALT

.data                    // everything up to .text goes after the code
#TABLE 3                 // 3 zeroed 64-bit words (24 bytes)...
COUNT:
.word 100                // ...then one 64-bit word holding 100
.text
```

Inline data starts on an 8-byte boundary too. Code is a whole number of 4-byte words, so after an odd number of code words the assembler adds a `.zeropad 4` before the data, and before the labels, comments and directives just above it, so those labels name the data. Every data item is a whole number of 8-byte words (except `.zeropad`, which is never aligned), so the data that follows stays aligned. The pad is zero, which is not a valid instruction, so do not let execution run into inline data.

---

## File Includes

`!include` inserts another source file in place of the line:

```
!include uart_stubs.kla
```

- The path is relative to the directory of the file that contains the `!include`.
- Included files may include others. A file that includes itself, directly or through other files, is an error ("Recursive include").
- A comment may follow the name: `!include utils.kla // Helper functions`.
- The included lines are assembled as if they were written there, so an included file of subroutines belongs after the main program's `HALT`.

---

## Macros

A macro is a name starting with `$` that expands to one or more lines. The standard macros below are built into the assembler; `klausscc --opcodes` also lists them.

### Defining Macros

Define a macro on one line with `!macro`, or read a file of definitions (one per line) with `!macros <file>`, resolved like `!include`:

```
!macro $SAVEABC PUSH A / PUSH B / PUSH C
!macro $SET2 SETR %1 %3 / SETR %2 %3     // $SET2 r1 r2 value
```

- Macro names start with `$` and are case-sensitive.
- The lines of the macro are separated by `/` (a word on its own, with spaces around it).
- `%1`, `%2`, ... stand for the arguments of the call. A missing argument is an error; extra arguments get a warning.
- A macro may call other macros, which are expanded in turn (up to 10 levels; macros that call each other for ever are an error).
- Defining a macro that already exists, built-in or not, is an error.

### Using Macros

Arguments follow the name, separated by spaces, and replace `%1`, `%2`, ... in order. A `//` comment after the call is not an argument.

```
$PUSHALL              // Expands to: PUSH A / PUSH B / PUSH C / PUSH D
$WAIT 0xFFF 0x100     // Expands to: DELAYV 0xFFF / DELAYV 0x100
```

The `.code` listing shows each expanded line with `// Macro expansion from $NAME`.

### Built-in Macros

| Macro | Expands to | Purpose |
|-------|-----------|---------|
| `$PUSH2 a b` | `PUSH a / PUSH b` | Save two registers |
| `$PUSH3 a b c` | `PUSH a / PUSH b / PUSH c` | Save three registers |
| `$PUSH4 a b c d` | `PUSH a` ... `PUSH d` | Save four registers |
| `$POP2 a b` | `POP b / POP a` | Restore what `$PUSH2 a b` saved |
| `$POP3 a b c` | `POP c / POP b / POP a` | Restore what `$PUSH3 a b c` saved |
| `$POP4 a b c d` | `POP d` ... `POP a` | Restore what `$PUSH4 a b c d` saved |
| `$PUSHALL` | `PUSH A / PUSH B / PUSH C / PUSH D` | Save A–D |
| `$POPALL` | `POP D / POP C / POP B / POP A` | Restore A–D |
| `$CLR r` | `SETR r 0` | Zero a register |
| `$TEST r` | `CMPRV r 0` | Set the flags from a register |
| `$CALLSAVE target` | `$PUSHALL`, `CALL target`, `$POPALL` | Call a subroutine, keeping A–D |
| `$MEMCPYW dst src tmp` | `MEMREADRR tmp src / MEMSET64RR tmp dst` | Copy one 64-bit word from address `src` to address `dst` |
| `$WAIT a b` | `DELAYV a / DELAYV b` | Two busy-wait delays (not the `WAIT` instruction) |
| `$TXR r` | `PUSH A / COPY A r / CALL TX_HEX32: / POP A` | Print the low 32 bits of `r` as 8 upper-case hex digits |
| `$NEWLINE` | `CALL TX_NL:` | Print CR LF |
| `$TXCHAR r` | `PUSH A / COPY A r / CALL TX_CHAR: / POP A` | Print the low byte of `r` |
| `$TXMEMCHAR r` | `PUSH A / COPY A r / MEMGET8 A A / CALL TX_CHAR: / POP A` | Print the byte at address `r` |
| `$TXSTR r` | `PUSH A / COPY A r / CALL TX_STR: / POP A` | Print the NUL-terminated bytes at address `r` |

The `$TX...` and `$NEWLINE` macros call the routines in `tests/fixtures/uart_stubs.kla`, so a program that uses them must `!include uart_stubs.kla` (after its `HALT`). The routines keep every register they do not take as an argument (they do change the flags), and wait for the transmitter to be idle before each byte. They replace the v1 print instructions (`TXR`, `NEWLINE`, `TXCHARMEMR`, `TXSTRMEMR`).

---

## Instruction Reference

Instructions follow the general format:

```
MNEMONIC operand operand ...
```

Operands are separated by spaces and written in the order shown. Register operands come first, then any immediate. The tables use these operand names:

| Name | Meaning |
|------|---------|
| `rd` | Destination register (for stores, the register whose value is stored) |
| `rs1`, `rs2` | Source registers. For loads and stores, `rs1` is the base address. |
| `rd [rs1]` | A destination register, then optionally a separate source. Written with one register it is both (`ADDV A 5` is A = A + 5); with two it is LLVM's 3-operand form (`ADDV B A 5` is B = A + 5, LLVM's `addv r1, r0, 5`). See [Separate Source Registers](#separate-source-registers) |
| `imm32`, `off32`, `addr32`, `target`, `offset`, `field` | A 32-bit immediate word after the instruction: a literal or a label |
| `imm64` | A 64-bit immediate (two words) |
| `n` | A literal count or bit number, 0 to 63 (for `ENTER`, a frame size in 8-byte words) |
| `imm8`, `off`, `simm4` | Short literals held in the instruction word itself |

Notation: `sext(x)` / `zext(x)` sign- / zero-extend `x` to 64 bits. `sext32(x)` sign-extends the low 32 bits of `x`. `memN[a]` is the N-bit value at byte address `a`. "Words" is the instruction's length in 32-bit words. The Flags column lists the flags the instruction writes; the others keep their values. "(v3)" marks the ISA v3 additions.

The register-register forms take three independent registers, so `ADDR C A B` (C = A + B) leaves A and B unchanged. The `rd [rs1]` forms work in place when given one register.

### ALU, Register-Register

| Instruction | Words | Operation | Flags |
|---|---|---|---|
| `ADDR rd rs1 rs2` | 1 | rd = rs1 + rs2 | Z S C V |
| `SUBR rd rs1 rs2` | 1 | rd = rs1 − rs2 | Z S C V |
| `ADDC rd rs1 rs2` | 1 | rd = rs1 + rs2 + C | Z S C V |
| `SUBC rd rs1 rs2` | 1 | rd = rs1 − rs2 − C | Z S C V |
| `ADDW rd rs1 rs2` | 1 | rd = sext32(rs1 + rs2): the low 32 bits of the sum, sign-extended (v3) | Z S C V ¹ |
| `SUBW rd rs1 rs2` | 1 | rd = sext32(rs1 − rs2) (v3) | Z S C V ¹ |
| `ANDR rd rs1 rs2` | 1 | rd = rs1 AND rs2 | — ² |
| `ORR rd rs1 rs2` | 1 | rd = rs1 OR rs2 | — ² |
| `XORR rd rs1 rs2` | 1 | rd = rs1 XOR rs2 | — ² |
| `MINR rd rs1 rs2` | 1 | rd = signed minimum of rs1, rs2 | — |
| `MAXR rd rs1 rs2` | 1 | rd = signed maximum of rs1, rs2 | — |
| `MINUR rd rs1 rs2` | 1 | rd = unsigned minimum of rs1, rs2 | — |
| `MAXUR rd rs1 rs2` | 1 | rd = unsigned maximum of rs1, rs2 | — |

¹ The `W` operations write a 32-bit result sign-extended to 64 bits. Z reflects the value written; S, C and V come from the full 64-bit operation.

² `ANDR`, `ORR` and `XORR` write no flags, so a branch after them tests the flags of an earlier instruction.

```
ADDR C A B        // C = A + B
SUBC D D E        // D = D - E - C (multi-word subtract)
MINUR A A B       // A = unsigned min(A, B)
```

### ALU, Immediate

| Instruction | Words | Operation | Flags |
|---|---|---|---|
| `ADDI rd rs1 imm32` | 2 | rd = rs1 + sext(imm32) | Z S C V |
| `ADDIW rd rs1 imm32` | 2 | rd = sext32(rs1 + sext(imm32)) (v3) | Z S C V ¹ |
| `ADDV rd [rs1] imm32` | 2 | rd = rs1 + zext(imm32) | Z S C V |
| `MINUSV rd [rs1] imm32` | 2 | rd = rs1 − zext(imm32) | Z S C V |
| `ANDV rd [rs1] imm32` | 2 | rd = rs1 AND zext(imm32) | — |
| `ORV rd [rs1] imm32` | 2 | rd = rs1 OR zext(imm32) | — |
| `XORV rd [rs1] imm32` | 2 | rd = rs1 XOR zext(imm32) | — |
| `ANDI rd rs1 imm32` | 2 | rd = rs1 AND sext(imm32) | — |
| `ORI rd rs1 imm32` | 2 | rd = rs1 OR sext(imm32) | — |
| `XORI rd rs1 imm32` | 2 | rd = rs1 XOR sext(imm32) | — |
| `SETR rd imm32` | 2 | rd = sext(imm32); a literal that needs more than 32 bits is assembled as `SETR64` | — |
| `SETRU rd imm32` | 2 | rd = zext(imm32) | — |
| `SETR64 rd imm64` | 3 | rd = imm64 (one 64-bit literal, or two words: low then high) | — |
| `LEAPC rd offset` | 2 | rd = address of this instruction + offset (a 32-bit result, zero-extended) | — |
| `SETR.S rd imm8` | 1 | rd = sext(imm8); imm8 −128..127 (v3) | — |
| `ADDI.S rd rs1 imm8` | 1 | rd = rs1 + sext(imm8); imm8 −128..127 (v3) | Z S C V |
| `ADDV.S rd [rs1] imm8` | 1 | rd = rs1 + zext(imm8); imm8 0..255 (v3) | Z S C V |
| `MINUSV.S rd [rs1] imm8` | 1 | rd = rs1 − zext(imm8); imm8 0..255 (v3) | Z S C V |
| `ANDV.S rd [rs1] imm8` | 1 | rd = rs1 AND zext(imm8); imm8 0..255 (v3) | — |
| `ORV.S rd [rs1] imm8` | 1 | rd = rs1 OR zext(imm8); imm8 0..255 (v3) | — |
| `XORV.S rd [rs1] imm8` | 1 | rd = rs1 XOR zext(imm8); imm8 0..255 (v3) | — |
| `SETRU.S rd imm8` | 1 | rd = zext(imm8); imm8 0..255 (v3) | — |
| `ANDI.S rd rs1 imm8` | 1 | rd = rs1 AND sext(imm8); imm8 −128..127 (v3) | — |
| `ORI.S rd rs1 imm8` | 1 | rd = rs1 OR sext(imm8); imm8 −128..127 (v3) | — |
| `XORI.S rd rs1 imm8` | 1 | rd = rs1 XOR sext(imm8); imm8 −128..127 (v3) | — |

¹ See [ALU, Register-Register](#alu-register-register).

`ADDI` and the in-place forms differ in the extension: `ADDI` sign-extends its immediate, while `ADDV`, `MINUSV`, `ANDV`, `ORV` and `XORV` zero-extend theirs. `ADDV A -1` therefore adds `0xFFFF_FFFF`, not -1; use `ADDI A A -1` or `DECR A`. `ANDI`, `ORI` and `XORI` are the sign-extending forms of `ANDV`, `ORV` and `XORV` (`ANDI B A -8` clears the low three bits of A into B), and `SETRU` the zero-extending form of `SETR`; these four have the names the LLVM compiler gives them.

The `.S` forms are one word long and take a literal only. Each takes the values its extension can give: -128 to 127 for `SETR.S`, `ADDI.S`, `ANDI.S`, `ORI.S`, `XORI.S` and `CMPRV.S`, which sign-extend, and 0 to 255 for the other `.S` forms, which zero-extend. Any other value is an assembly error, so `SETR.S A 200` and `ADDV.S A -1` do not assemble; use the 2-word form.

```
SETR A 0xFF       // A = 0xFF
ADDV A 0x10       // A = A + 16
ADDV B A 0x10     // B = A + 16 (A unchanged)
ADDI B A -1       // B = A - 1 (A unchanged)
ANDV A 0xF        // A = A AND 0xF
SETR.S C -5       // one word: C = -5
```

### Compare

| Instruction | Words | Operation | Flags |
|---|---|---|---|
| `CMPRR rs1 rs2` | 1 | flags from rs1 − rs2 (nothing written) | Z S C V |
| `CMPRV rs1 imm32` | 2 | flags from rs1 − sext(imm32) | Z S C V |
| `CMPRV.S rs1 imm8` | 1 | flags from rs1 − sext(imm8); imm8 −128..127 (v3) | Z S C V |
| `CMPRVU rs1 imm32` | 2 | flags from rs1 − zext(imm32) | Z S C V |
| `CMPRRW rs1 rs2` | 1 | flags from the 32-bit compare of the low halves of rs1 and rs2 (v3) | Z S C V |
| `CMPRVW rs1 imm32` | 2 | flags from the 32-bit compare of the low halves of rs1 and sext(imm32) (v3) | Z S C V |
| `CMPEQR rd rs1 rs2` | 1 | rd = 1 if rs1 = rs2, else 0 | — |
| `CMPNER rd rs1 rs2` | 1 | rd = 1 if rs1 ≠ rs2, else 0 | — |
| `CMPLTR rd rs1 rs2` | 1 | rd = 1 if rs1 < rs2 (signed), else 0 | — |
| `CMPLER rd rs1 rs2` | 1 | rd = 1 if rs1 ≤ rs2 (signed), else 0 | — |
| `CMPGTR rd rs1 rs2` | 1 | rd = 1 if rs1 > rs2 (signed), else 0 | — |
| `CMPGER rd rs1 rs2` | 1 | rd = 1 if rs1 ≥ rs2 (signed), else 0 | — |
| `CMPULTR rd rs1 rs2` | 1 | rd = 1 if rs1 < rs2 (unsigned), else 0 | — |
| `CMPULER rd rs1 rs2` | 1 | rd = 1 if rs1 ≤ rs2 (unsigned), else 0 | — |
| `CMPUGTR rd rs1 rs2` | 1 | rd = 1 if rs1 > rs2 (unsigned), else 0 | — |
| `CMPUGER rd rs1 rs2` | 1 | rd = 1 if rs1 ≥ rs2 (unsigned), else 0 | — |
| `CMPEQV rd rs1 imm32` … `CMPUGEV rd rs1 imm32` | 2 | as `CMPEQR` … `CMPUGER`, comparing rs1 with sext(imm32) | — |
| `CMPEQRW rd rs1 rs2` … `CMPUGERW rd rs1 rs2` | 1 | as `CMPEQR` … `CMPUGER`, comparing the low 32 bits (v3) | — |
| `CMPEQVW rd rs1 imm32` … `CMPUGEVW rd rs1 imm32` | 2 | as `CMPEQV` … `CMPUGEV`, comparing the low 32 bits (v3) | — |

`CMPRR`, `CMPRV` and `CMPRVU` set the flags exactly as a subtraction would, without writing a register. Follow them with any conditional jump: `JMPE`/`JMPNE` for equality, `JMPLT`/`JMPGE`/`JMPLE`/`JMPGT` for signed order, `JMPULT`/`JMPUGE`/`JMPULE`/`JMPUGT` for unsigned order. The `CMP..R` boolean forms write 0 or 1 to `rd` and leave the flags alone, and so do their immediate (`CMP..V`) and 32-bit (`CMP..RW`, `CMP..VW`) forms, for all ten conditions `EQ`, `NE`, `LT`, `LE`, `GT`, `GE`, `ULT`, `ULE`, `UGT` and `UGE`. The unsigned immediate forms compare with the sign-extended immediate as an unsigned number, so `CMPULTV C A -1` is 1 unless A is all ones.

```
CMPRV A 10        // flags from A - 10
JMPLT SMALL:      // jump if A < 10 (signed)
CMPLTR C A B      // C = 1 if A < B (signed), else 0
SMALL:
HALT
```

### Fused Compare-and-Branch

These v3 instructions compare two registers, or a register and a small literal, and branch in one word. They **do not change the flags**. The target is a label (or address) within ±16 KB of the instruction.

| Instruction | Words | Operation | Flags |
|---|---|---|---|
| `BEQ rs1 rs2 target` | 1 | branch if rs1 = rs2 | — |
| `BEQI rs1 simm4 target` | 1 | branch if rs1 = simm4; simm4 −8..7, sign-extended | — |
| `BNE rs1 rs2 target` | 1 | branch if rs1 ≠ rs2 | — |
| `BNEI rs1 simm4 target` | 1 | branch if rs1 ≠ simm4; simm4 −8..7, sign-extended | — |
| `BLT rs1 rs2 target` | 1 | branch if rs1 < rs2 (signed) | — |
| `BLTI rs1 simm4 target` | 1 | branch if rs1 < simm4 (signed); simm4 −8..7, sign-extended | — |
| `BGE rs1 rs2 target` | 1 | branch if rs1 ≥ rs2 (signed) | — |
| `BGEI rs1 simm4 target` | 1 | branch if rs1 ≥ simm4 (signed); simm4 −8..7, sign-extended | — |
| `BLE rs1 rs2 target` | 1 | branch if rs1 ≤ rs2 (signed) | — |
| `BLEI rs1 simm4 target` | 1 | branch if rs1 ≤ simm4 (signed); simm4 −8..7, sign-extended | — |
| `BGT rs1 rs2 target` | 1 | branch if rs1 > rs2 (signed) | — |
| `BGTI rs1 simm4 target` | 1 | branch if rs1 > simm4 (signed); simm4 −8..7, sign-extended | — |
| `BULT rs1 rs2 target` | 1 | branch if rs1 < rs2 (unsigned) | — |
| `BULTI rs1 simm4 target` | 1 | branch if rs1 < simm4 (unsigned); simm4 −8..7, sign-extended | — |
| `BUGE rs1 rs2 target` | 1 | branch if rs1 ≥ rs2 (unsigned) | — |
| `BUGEI rs1 simm4 target` | 1 | branch if rs1 ≥ simm4 (unsigned); simm4 −8..7, sign-extended | — |
| `BULE rs1 rs2 target` | 1 | branch if rs1 ≤ rs2 (unsigned) | — |
| `BULEI rs1 simm4 target` | 1 | branch if rs1 ≤ simm4 (unsigned); simm4 −8..7, sign-extended | — |
| `BUGT rs1 rs2 target` | 1 | branch if rs1 > rs2 (unsigned) | — |
| `BUGTI rs1 simm4 target` | 1 | branch if rs1 > simm4 (unsigned); simm4 −8..7, sign-extended | — |
| `FBR rs1 rs2 cc target` | 1 | LLVM's spelling of `BEQ` … `BUGT`: cc 0 EQ, 1 LT, 2 LE, 3 ULT, 4 ULE; plus 8 for the inverse (NE, GE, GT, UGE, UGT) | — |
| `FBRI rs1 simm4 cc target` | 1 | LLVM's spelling of `BEQI` … `BUGTI`, cc as for `FBR` | — |

`FBR` and `FBRI` are the names the LLVM compiler prints (`fbr r14, r12, 8, …` is `BNE O M …`). They assemble to the same words as the named branches, a cc of 5 to 7 or 13 to 15 is an error (the CPU traps on it), and a disassembly listing shows the named branch.

```
SETR B 10
LOOP:
ADDR A A B
DECR B
BNEI B 0 LOOP:    // loop while B != 0
```

### Shift and Rotate

| Instruction | Words | Operation | Flags |
|---|---|---|---|
| `SHLR rd rs1 rs2` | 1 | rd = rs1 << rs2[5:0] | Z |
| `SHRR rd rs1 rs2` | 1 | rd = rs1 >> rs2[5:0] (logical) | Z |
| `SARR rd rs1 rs2` | 1 | rd = rs1 >> rs2[5:0] (arithmetic) | Z |
| `ROLR rd rs1 rs2` | 1 | rd = rs1 rotated left by rs2[5:0] | Z ³ |
| `RORR rd rs1 rs2` | 1 | rd = rs1 rotated right by rs2[5:0] | Z ³ |
| `SHLV rd [rs1] n` | 1 | rd = rs1 << n; n 0..63 | Z |
| `SHRV rd [rs1] n` | 1 | rd = rs1 >> n (logical) | Z |
| `SHRAV rd [rs1] n` | 1 | rd = rs1 >> n (arithmetic) | Z |
| `ROLV rd [rs1] n` | 1 | rd = rs1 rotated left by n | Z; n = 1: Z C |
| `RORV rd [rs1] n` | 1 | rd = rs1 rotated right by n | Z; n = 1: Z C |
| `SHLR1 rd [rs1]` | 1 | rd = rs1 << 1 | — |
| `SHLAR rd [rs1]` | 1 | rd = rs1 << 1 (same encoding as `SHLR1`) | — |
| `SHRR1 rd [rs1]` | 1 | rd = rs1 >> 1 (logical) | — |
| `SHRAR rd [rs1]` | 1 | rd = rs1 >> 1 (arithmetic) | — |
| `ROLR1 rd [rs1]` | 1 | rd = rs1 rotated left by 1; C = the bit rotated out | Z C |
| `RORR1 rd [rs1]` | 1 | rd = rs1 rotated right by 1; C = the bit rotated out | Z C |
| `ROLCR rd [rs1]` | 1 | rd = rs1 rotated left by 1 through carry (C in at bit 0, bit 63 out to C) | Z C |
| `RORCR rd [rs1]` | 1 | rd = rs1 rotated right by 1 through carry (C in at bit 63, bit 0 out to C) | Z C |

Counts held in a register use its low 6 bits (`rs2[5:0]`), so a count of 64 shifts by 0.

³ With a register count, only Z is set, even for a count of 1. Only `ROLR1`/`RORR1` and `ROLV`/`RORV` with n = 1 set C.

```
SHLV A 4          // A = A << 4
SHLV C A 4        // C = A << 4 (A unchanged)
SHRAV B 8         // B = B >> 8, keeping the sign
SHRR C A B        // C = A >> B[5:0] (logical)
ROLR1 D           // rotate D left 1; C = old bit 63
```

### Bit Manipulation

| Instruction | Words | Operation | Flags |
|---|---|---|---|
| `BSET rd [rs1] n` | 1 | rd = rs1 with bit n set | — |
| `BCLR rd [rs1] n` | 1 | rd = rs1 with bit n cleared | — |
| `BTGL rd [rs1] n` | 1 | rd = rs1 with bit n inverted | — |
| `BTST rs1 n` | 1 | Z = 1 if bit n of rs1 is 0; no register is written | Z |
| `BSETRR rd rs1 rs2` | 1 | rd = rs1 with bit rs2[5:0] set | — |
| `BCLRRR rd rs1 rs2` | 1 | rd = rs1 with bit rs2[5:0] cleared | — |
| `BTGLRR rd rs1 rs2` | 1 | rd = rs1 with bit rs2[5:0] inverted | — |
| `BTSTRR rd rs1 rs2` | 1 | rd = bit rs2[5:0] of rs1 (0 or 1) | — |
| `BEXTR rd rs1 field` | 2 | rd = the `len`-bit field of rs1 that starts at bit `start`, zero-extended; field = len × 256 + start | Z |
| `BDEP rd rs1 rs2 field` | 2 | rd = rs1[31:0] with its `len`-bit field at bit `start` replaced by the low `len` bits of rs2, zero-extended; field as for BEXTR | — |

`BEXTR` and `BDEP` describe the field with one immediate: `start` in bits 4:0 and `len` in bits 12:8 (`field = len × 256 + start`). `len` is 0 to 31; a `len` of 0 is an empty field. Both results are zero-extended 32-bit values.

```
SETR A 0xABCD
BEXTR B A 0x0804         // B = 8 bits of A from bit 4 = 0xBC
SETR C 0x5
BDEP D A C 0x0400        // D = A with bits 3:0 replaced by C = 0xABC5
BTST A 0                 // bit 0 of A is 1, so Z = 0
BSET A 4                 // A = 0xABDD
```

### Unary

| Instruction | Words | Operation | Flags |
|---|---|---|---|
| `COPY rd rs1` | 1 | rd = rs1 | — |
| `NEGR rd [rs1]` | 1 | rd = −rs1 | Z |
| `NOTR rd [rs1]` | 1 | rd = NOT rs1 | Z |
| `ABSR rd [rs1]` | 1 | rd = \|rs1\| (signed; 0x8000…0000 stays as it is) | Z V ⁴ |
| `SEXTB rd [rs1]` | 1 | rd = rs1[7:0] sign-extended | Z S |
| `SEXTH rd [rs1]` | 1 | rd = rs1[15:0] sign-extended | Z S |
| `SEXTW rd [rs1]` | 1 | rd = rs1[31:0] sign-extended | — |
| `ZEXTB rd [rs1]` | 1 | rd = rs1[7:0] zero-extended | Z |
| `ZEXTH rd [rs1]` | 1 | rd = rs1[15:0] zero-extended | Z |
| `ZEXTW rd [rs1]` | 1 | rd = rs1[31:0] zero-extended | — |
| `BSWAP rd [rs1]` | 1 | rd = rs1 with its 8 bytes reversed | — |
| `BITREV rd [rs1]` | 1 | rd = rs1 with its 64 bits reversed | — |
| `POPCNT rd [rs1]` | 1 | rd = number of 1 bits in rs1 | Z |
| `CLZ rd [rs1]` | 1 | rd = number of leading zero bits of rs1 (64 for 0) | — |
| `CTZ rd [rs1]` | 1 | rd = number of trailing zero bits of rs1 (64 for 0) | — |
| `INCR rd [rs1]` | 1 | rd = rs1 + 1 | Z S C V |
| `DECR rd [rs1]` | 1 | rd = rs1 − 1 | Z S C V |
| `SETFR rd` | 1 | rd = the flags in bits 63:60 as {Z, Z, C, V}, bits 59:0 zero | — |

⁴ V is set when the operand was `0x8000_0000_0000_0000`, whose absolute value does not fit, and cleared otherwise.

All of these except `COPY` and `SETFR` work in place when given one register (`NEGR C` is C = -C), and leave the source unchanged when given two (`NEGR D C` is D = -C).

### Loads

| Instruction | Words | Operation | Flags |
|---|---|---|---|
| `MEMGET8 rd rs1` | 1 | rd = zext(mem8[rs1]) | — |
| `MEMGET16 rd rs1` | 1 | rd = zext(mem16[rs1 & ~1]) | — |
| `MEMGET32 rd rs1` | 1 | rd = zext(mem32[rs1]); any alignment, may cross a doubleword | — |
| `MEMREADRR rd rs1` | 1 | rd = mem64[rs1] | — |
| `MEMGET64 rd rs1` | 1 | rd = mem64[rs1 & ~7] | — |
| `LDIDX8 rd rs1 off32` | 2 | rd = zext(mem8[rs1 + off32]) | — |
| `LDIDX8_S rd rs1 off32` | 2 | rd = sext(mem8[rs1 + off32]) | — |
| `LDIDX16 rd rs1 off32` | 2 | rd = zext(mem16[(rs1 + off32) & ~1]) | — |
| `LDIDX16_S rd rs1 off32` | 2 | rd = sext(mem16[(rs1 + off32) & ~1]) | — |
| `LDIDX32 rd rs1 off32` | 2 | rd = zext(mem32[(rs1 + off32) & ~3]) | — |
| `LDIDX32_S rd rs1 off32` | 2 | rd = sext(mem32[(rs1 + off32) & ~3]) (v3) | — |
| `LDIDX64 rd rs1 off32` | 2 | rd = mem64[rs1 + off32]; also spelt `LDIDX64U` ⚠ | — |
| `LDIDX64A rd rs1 off32` | 2 | rd = mem64[(rs1 + off32) & ~7] | — |
| `LDIDX64R rd rs1 rs2` | 1 | rd = mem64[rs1 + rs2] | — |
| `LDIDX8R rd rs1 rs2` | 1 | rd = zext(mem8[rs1 + rs2]) | — |
| `LDIDX8SR rd rs1 rs2` | 1 | rd = sext(mem8[rs1 + rs2]) | — |
| `LDIDX16R rd rs1 rs2` | 1 | rd = zext(mem16[(rs1 + rs2) & ~1]) | — |
| `LDIDX16SR rd rs1 rs2` | 1 | rd = sext(mem16[(rs1 + rs2) & ~1]) | — |
| `LDIDX32R rd rs1 rs2` | 1 | rd = zext(mem32[(rs1 + rs2) & ~3]) | — |
| `LDIDX32SR rd rs1 rs2` | 1 | rd = sext(mem32[(rs1 + rs2) & ~3]) | — |
| `MEMREADR rd addr32` | 2 | rd = mem64[addr32]; addr32 is an address or label | — |
| `LDABS8 rd addr32` | 2 | rd = zext(mem8[addr32]) | — |
| `LDABS8S rd addr32` | 2 | rd = sext(mem8[addr32]) | — |
| `LDABS16 rd addr32` | 2 | rd = zext(mem16[addr32 & ~1]) | — |
| `LDABS16S rd addr32` | 2 | rd = sext(mem16[addr32 & ~1]) | — |
| `LDABS32 rd addr32` | 2 | rd = zext(mem32[addr32 & ~3]) | — |
| `LDABS32S rd addr32` | 2 | rd = sext(mem32[addr32 & ~3]) | — |
| `LDIDX8.S rd rs1 off` | 1 | as `LDIDX8`, off −128..127 (v3) | — |
| `LDIDX8_S.S rd rs1 off` | 1 | as `LDIDX8_S`, off −128..127 (v3) | — |
| `LDIDX16.S rd rs1 off` | 1 | as `LDIDX16`, off even, −256..254 (v3) | — |
| `LDIDX16_S.S rd rs1 off` | 1 | as `LDIDX16_S`, off even, −256..254 (v3) | — |
| `LDIDX32.S rd rs1 off` | 1 | as `LDIDX32`, off a multiple of 4, −512..508 (v3) | — |
| `LDIDX32_S.S rd rs1 off` | 1 | as `LDIDX32_S`, off a multiple of 4, −512..508 (v3) | — |
| `LDIDX64.S rd rs1 off` | 1 | as `LDIDX64`, off a multiple of 8, −1024..1016 (v3); also spelt `LDIDX64U.S` ⚠ | — |
| `LDIDX64A.S rd rs1 off` | 1 | as `LDIDX64A`, off a multiple of 8, −1024..1016 (v3) | — |

### Stores

| Instruction | Words | Operation | Flags |
|---|---|---|---|
| `MEMSET8 rd rs1` | 1 | mem8[rs1] = rd[7:0] | — |
| `MEMSET16 rd rs1` | 1 | mem16[rs1 & ~1] = rd[15:0] | — |
| `MEMSET32 rd rs1` | 1 | mem32[rs1 & ~3] = rd[31:0] | — |
| `MEMSET64RR rd rs1` | 1 | mem64[rs1] = rd | — |
| `MEMSET64 rd rs1` | 1 | mem64[rs1 & ~7] = rd | — |
| `STIDX8 rd rs1 off32` | 2 | mem8[rs1 + off32] = rd[7:0] | — |
| `STIDX16 rd rs1 off32` | 2 | mem16[(rs1 + off32) & ~1] = rd[15:0] | — |
| `STIDX32 rd rs1 off32` | 2 | mem32[(rs1 + off32) & ~3] = rd[31:0] | — |
| `STIDX64 rd rs1 off32` | 2 | mem64[rs1 + off32] = rd; also spelt `STIDX64U` ⚠ | — |
| `STIDX64A rd rs1 off32` | 2 | mem64[(rs1 + off32) & ~7] = rd | — |
| `STIDX64R rd rs1 rs2` | 1 | mem64[rs1 + rs2] = rd | — |
| `STIDX8R rd rs1 rs2` | 1 | mem8[rs1 + rs2] = rd[7:0] | — |
| `STIDX16R rd rs1 rs2` | 1 | mem16[(rs1 + rs2) & ~1] = rd[15:0] | — |
| `STIDX32R rd rs1 rs2` | 1 | mem32[(rs1 + rs2) & ~3] = rd[31:0] | — |
| `MEMSETR rd addr32` | 2 | mem64[addr32] = rd; addr32 is an address or label | — |
| `STABS8 rd addr32` | 2 | mem8[addr32] = rd[7:0] | — |
| `STABS16 rd addr32` | 2 | mem16[addr32 & ~1] = rd[15:0] | — |
| `STABS32 rd addr32` | 2 | mem32[addr32 & ~3] = rd[31:0] | — |
| `STIDX8.S rd rs1 off` | 1 | as `STIDX8`, off −128..127 (v3) | — |
| `STIDX16.S rd rs1 off` | 1 | as `STIDX16`, off even, −256..254 (v3) | — |
| `STIDX32.S rd rs1 off` | 1 | as `STIDX32`, off a multiple of 4, −512..508 (v3) | — |
| `STIDX64.S rd rs1 off` | 1 | as `STIDX64`, off a multiple of 8, −1024..1016 (v3); also spelt `STIDX64U.S` ⚠ | — |
| `STIDX64A.S rd rs1 off` | 1 | as `STIDX64A`, off a multiple of 8, −1024..1016 (v3) | — |

Stores take the value from `rd` and the address from `rs1`: `MEMSET8 D B` stores the low byte of D at the address in B.

Byte accesses use the address as it is. 16- and 32-bit accesses clear the address bits below their size, except `MEMGET32`, which reads four bytes at any address. The `A` forms (`MEMGET64`, `MEMSET64`, `LDIDX64A`, `STIDX64A` and their `.S` forms) clear the low three address bits. The other 64-bit forms pass the address on unchanged, but the data cache ignores address bits 2:0, so every 64-bit memory access reaches the aligned doubleword that holds the address. The same goes for `PUSH`, `POP`, calls and returns when SP is not a multiple of 8. A device (MMIO) access without `A` keeps the address as it is. Keep the address of every 64-bit access 8-byte aligned, or use an `A` form.

The `.S` forms are one word long and take a literal byte offset, which must be a multiple of the access size. Offsets in `off32` may be negative (`STIDX64 A P -8`). The `R` forms add two registers; the sign-extending ones are spelt `LDIDX8SR`, `LDIDX16SR` and `LDIDX32SR`, as the LLVM compiler names them, without the underscore of `LDIDX8_S`. The 2-word and `.S` sign-extending loads also take LLVM's spelling without the underscore (`LDIDX8S`, `LDIDX16S.S`, …). The `LDABS`/`STABS` forms take an absolute address at every width, as `MEMREADR`/`MEMSETR` do for 64 bits; the compiler uses them for device registers (`LDABS32S` is the sign-extending 32-bit one).

> **⚠ `LDIDX64` and `STIDX64` mean different instructions here and in LLVM.** klausscc's `LDIDX64`/`STIDX64` are the A=0 forms (address as computed), which LLVM calls `ldidx64u`/`stidx64u`. LLVM's `ldidx64`/`stidx64` are the A=1 forms (address `& ~7`), klausscc's `LDIDX64A`/`STIDX64A`. The two only differ for an address that is not 8-byte aligned, but a line copied from LLVM output with the name unchanged assembles to the other encoding. klausscc keeps its meaning so existing sources assemble as before. Write `LDIDX64U`/`STIDX64U` (accepted here as the same A=0 forms) or `LDIDX64A`/`STIDX64A` (LLVM accepts `ldidx64a`/`stidx64a`) to mean the same thing in both; a disassembly listing uses only those names.

Addresses with the top four bits set (`0xF000_0000` and up) reach the devices; see [Memory-Mapped I/O](#memory-mapped-io).

```
SETR A #BUF              // A = address of BUF (8-byte aligned)
SETR B 0x1122334455667788
MEMSET64 B A             // BUF[0..7] = B, little-endian
MEMGET8 C A              // C = 0x88 (the lowest byte comes first)
LDIDX8 D A 7             // D = 0x11
LDIDX16 E A 2            // E = 0x5566
MEMGET32 F A             // F = 0x55667788
SETR I 0xFF
STIDX8 I A 1             // byte 1 = 0xFF
MEMGET64 J A             // J = 0x112233445566FF88
LDIDX8_S K A 1           // K = 0xFFFF_FFFF_FFFF_FFFF (sign-extended)
HALT
#BUF 2
```

### Jumps and Calls

| Instruction | Words | Operation | Flags |
|---|---|---|---|
| `JMP target` | 2 | jump | — |
| `JMPZ target` | 2 | jump if Z | — |
| `JMPNZ target` | 2 | jump if not Z | — |
| `JMPC target` | 2 | jump if C | — |
| `JMPNC target` | 2 | jump if not C | — |
| `JMPO target` | 2 | jump if V | — |
| `JMPNO target` | 2 | jump if not V | — |
| `JMPS target` | 2 | jump if S | — |
| `JMPNS target` | 2 | jump if not S | — |
| `JMPLT target` | 2 | jump if less (signed) | — |
| `JMPGE target` | 2 | jump if greater or equal (signed) | — |
| `JMPLE target` | 2 | jump if less or equal (signed) | — |
| `JMPGT target` | 2 | jump if greater (signed) | — |
| `JMPULT target` | 2 | jump if lower (unsigned) | — |
| `JMPUGE target` | 2 | jump if higher or same (unsigned) | — |
| `JMPULE target` | 2 | jump if lower or same (unsigned) | — |
| `JMPUGT target` | 2 | jump if higher (unsigned) | — |
| `JMPE target` | 2 | jump if equal (= Z) | — |
| `JMPNE target` | 2 | jump if not equal (= not Z) | — |
| `CALL target` | 2 | call; pushes the address of the next instruction | — |
| `CALLZ target` | 2 | call if Z; pushes the address of the next instruction | — |
| `CALLNZ target` | 2 | call if not Z; pushes the address of the next instruction | — |
| `CALLC target` | 2 | call if C; pushes the address of the next instruction | — |
| `CALLNC target` | 2 | call if not C; pushes the address of the next instruction | — |
| `CALLO target` | 2 | call if V; pushes the address of the next instruction | — |
| `CALLNO target` | 2 | call if not V; pushes the address of the next instruction | — |
| `CALLE target` | 2 | call if equal (= Z); pushes the address of the next instruction | — |
| `CALLNE target` | 2 | call if not equal (= not Z); pushes the address of the next instruction | — |
| `JMPREL target` … `JMPNEREL target` | 2 | as `JMP` … `JMPNE`, PC-relative | — |
| `CALLREL target` | 2 | as `CALL`, PC-relative | — |
| `JMPR rs2` | 1 | jump to the address in rs2 | — |
| `CALLR rs2` | 1 | call the address in rs2; pushes the address of the next instruction | — |

`target` is a label or an absolute address. The `REL` forms (`JMPREL`, `JMPZREL`, … `JMPNEREL` for every condition but `O` and `NO`, and `CALLREL`) store the target's distance from the instruction instead of its address, so the code runs wherever it is loaded; they reach any address, and the `.code` listing of a disassembled program shows the distance (`CALLREL PC+288`). A call pushes the address of the instruction after it (8 bytes on the stack) and jumps; `RET` pops it. The condition codes read the flags as follows:

| Suffix | Taken when | Use after `CMPRR a b` |
|--------|-----------|------------------------|
| `Z` / `E` | Z = 1 | a = b |
| `NZ` / `NE` | Z = 0 | a ≠ b |
| `C` / `ULT` | C = 1 | a < b, unsigned |
| `NC` / `UGE` | C = 0 | a ≥ b, unsigned |
| `ULE` | C = 1 or Z = 1 | a ≤ b, unsigned |
| `UGT` | C = 0 and Z = 0 | a > b, unsigned |
| `LT` | S ≠ V | a < b, signed |
| `GE` | S = V | a ≥ b, signed |
| `LE` | Z = 1 or S ≠ V | a ≤ b, signed |
| `GT` | Z = 0 and S = V | a > b, signed |
| `S` / `NS` | S = 1 / S = 0 | result negative / not negative |
| `O` / `NO` | V = 1 / V = 0 | signed overflow / none |

```
CALL MY_FUNCTION:        // push the return address, jump
// ... execution continues here after RET
HALT

MY_FUNCTION:
PUSH A
SETR A 0x1
POP A
RET
```

### Short Jumps and Calls

One-word, PC-relative forms of the jumps (v3). The target is a label (or address) within ±512 KB. `CALL.S` pushes the address of the next instruction, which is 4 bytes on, not 8.

| Instruction | Words | Operation | Flags |
|---|---|---|---|
| `JMP.S target` | 1 | as `JMP`, PC-relative, within ±512 KB (v3) | — |
| `JMPZ.S target` | 1 | as `JMPZ`, PC-relative, within ±512 KB (v3) | — |
| `JMPNZ.S target` | 1 | as `JMPNZ`, PC-relative, within ±512 KB (v3) | — |
| `JMPC.S target` | 1 | as `JMPC`, PC-relative, within ±512 KB (v3) | — |
| `JMPNC.S target` | 1 | as `JMPNC`, PC-relative, within ±512 KB (v3) | — |
| `JMPO.S target` | 1 | as `JMPO`, PC-relative, within ±512 KB (v3) | — |
| `JMPNO.S target` | 1 | as `JMPNO`, PC-relative, within ±512 KB (v3) | — |
| `JMPS.S target` | 1 | as `JMPS`, PC-relative, within ±512 KB (v3) | — |
| `JMPNS.S target` | 1 | as `JMPNS`, PC-relative, within ±512 KB (v3) | — |
| `JMPLT.S target` | 1 | as `JMPLT`, PC-relative, within ±512 KB (v3) | — |
| `JMPGE.S target` | 1 | as `JMPGE`, PC-relative, within ±512 KB (v3) | — |
| `JMPLE.S target` | 1 | as `JMPLE`, PC-relative, within ±512 KB (v3) | — |
| `JMPGT.S target` | 1 | as `JMPGT`, PC-relative, within ±512 KB (v3) | — |
| `JMPULT.S target` | 1 | as `JMPULT`, PC-relative, within ±512 KB (v3) | — |
| `JMPUGE.S target` | 1 | as `JMPUGE`, PC-relative, within ±512 KB (v3) | — |
| `JMPULE.S target` | 1 | as `JMPULE`, PC-relative, within ±512 KB (v3) | — |
| `JMPUGT.S target` | 1 | as `JMPUGT`, PC-relative, within ±512 KB (v3) | — |
| `JMPE.S target` | 1 | as `JMPE`, PC-relative, within ±512 KB (v3) | — |
| `JMPNE.S target` | 1 | as `JMPNE`, PC-relative, within ±512 KB (v3) | — |
| `CALL.S target` | 1 | as `CALL`, PC-relative, within ±512 KB (v3) | — |

The LLVM compiler prints a short jump with the name of the 2-word PC-relative form (`jmprel`, `jmpzrel`, …; `jmpo`/`jmpno` for O and NO) and chooses the length itself. klausscc's `JMPREL`, `JMPZREL`, … are always 2 words; write `JMP.S`, `JMPZ.S`, … for the 1-word form. LLVM has no `CALL.S`.

### Stack

| Instruction | Words | Operation | Flags |
|---|---|---|---|
| `PUSH rs1` | 1 | SP −= 8; mem64[SP] = rs1 | — |
| `PUSHV imm32` | 2 | SP −= 8; mem64[SP] = zext(imm32) | — |
| `PUSHV64 imm64` | 3 | SP −= 8; mem64[SP] = imm64 | — |
| `POP rd` | 1 | rd = mem64[SP]; SP += 8 | — |
| `GETSP rd` | 1 | rd = SP (zero-extended) | — |
| `SETSP rs1` | 1 | SP = rs1[31:0] | — |
| `ADDSP imm32` | 2 | SP = SP + sext(imm32) | — |
| `RET` | 1 | pop the return address into PC | — |
| `IRET` | 1 | pop the interrupt context: PC, flags and `INT_MASK` | Z S C V |
| `ENTER n` | 1 | push P; P = SP; SP −= 8 × n (n 0..4194303) (v3) | — |
| `LEAVE` | 1 | SP = P; pop P (v3) | — |
| `LEAVERET` | 1 | SP = P; pop P; pop PC (= `LEAVE` then `RET`) (v3) | — |

The stack is full-descending: SP points at the last item pushed, and every push or pop moves 8 bytes. `PUSHV64` takes one 64-bit literal, or two words, low then high.

`ENTER n` builds a frame of `n` 8-byte locals below the saved `P`; address them as `P - 8`, `P - 16`, ... `LEAVERET` undoes `ENTER` and returns:

```
_start
SETR A 20
CALL DOUBLE:
HALT                     // A = 40

// DOUBLE: A = 2 * A, using a stack frame with one local doubleword.
DOUBLE:
ENTER 1                  // push P; P = SP; SP -= 8
STIDX64 A P -8           // local = A
LDIDX64 B P -8
ADDR A A B
LEAVERET                 // SP = P; pop P; return
```

### Multiply and Divide

| Instruction | Words | Operation | Flags |
|---|---|---|---|
| `MULR rd rs1 rs2` | 1 | rd = low 64 bits of rs1 × rs2 (signed) | Z S V ⁵ |
| `MULUR rd rs1 rs2` | 1 | rd = low 64 bits of rs1 × rs2 (unsigned) | Z S V ⁵ |
| `MULHR rd rs1 rs2` | 1 | rd = high 64 bits of rs1 × rs2 (signed) | Z S V ⁵ |
| `MULHUR rd rs1 rs2` | 1 | rd = high 64 bits of rs1 × rs2 (unsigned) | Z S V ⁵ |
| `MULW rd rs1 rs2` | 1 | rd = sext32(low 32 bits of rs1 × rs2) (v3) | Z S V ⁵ |
| `MULV rd [rs1] imm32` | 2 | rd = rs1 × sext(imm32) (signed) | Z S V ⁵ |
| `DIVR rd rs1 rs2` | 1 | rd = rs1 ÷ rs2 (signed) | Z V ⁵ |
| `DIVUR rd rs1 rs2` | 1 | rd = rs1 ÷ rs2 (unsigned) | Z V ⁵ |
| `DIVV rd [rs1] imm32` | 2 | rd = rs1 ÷ sext(imm32) (signed) | Z V ⁵ |
| `MODR rd rs1 rs2` | 1 | rd = rs1 mod rs2 (signed remainder) | Z V ⁵ |
| `MODUR rd rs1 rs2` | 1 | rd = rs1 mod rs2 (unsigned) | Z V ⁵ |
| `MODV rd [rs1] imm32` | 2 | rd = rs1 mod sext(imm32) (signed) | Z V ⁵ |

⁵ The multiplies set Z from the value written, S from bit 63 of the 64-bit half they produce (for `MULW`, of the full low half before sign-extension) and V = 1 when the low-half product does not fit in 64 bits (always 0 for `MULHR`/`MULHUR`). Divide and modulo set Z and clear V. Divide by zero does not trap: `DIV..` gives `0xFFFF_FFFF_FFFF_FFFF`, `MOD..` gives the dividend, V = 1 and Z is unchanged.

`MULV`, `DIVV` and `MODV` sign-extend their immediate.

### System and LCD

| Instruction | Words | Operation | Flags |
|---|---|---|---|
| `NOP` | 1 | no operation | — |
| `HALT` | 1 | stop the CPU | — |
| `WAIT` | 1 | sleep until an enabled interrupt is pending (see [Interrupts](#interrupts)) | — |
| `TRAP` | 1 | stop the CPU with a trap (software abort) | — |
| `RESET` | 1 | jump to address 0x4 | — |
| `DELAYR rs1` | 1 | busy-wait for rs1[31:0] × 8192 clock cycles | — |
| `DELAYV imm32` | 2 | busy-wait for imm32 × 8192 clock cycles | — |
| `LCDCMDR rs1` | 1 | send rs1 to the LCD as a command | — |
| `LCDDATAR rs1` | 1 | send rs1 to the LCD as data | — |
| `LCDCMDV imm32` | 2 | send imm32 to the LCD as a command | — |
| `LCDDATAV imm32` | 2 | send imm32 to the LCD as data | — |
| `LCDRST imm32` | 2 | drive the LCD reset line from imm32 bit 0; LLVM's name `LCD` is accepted too | — |

`HALT` stops the CPU for good (the emulator stops with `Halt`). `TRAP` stops it as a software abort (the emulator stops with `Trap`). `RESET` jumps to address `0x4`, which in an assembled image is inside the heap header, not code. `DELAYR`/`DELAYV` busy-wait in the CPU; the emulator does not wait, but advances its clock (timer, millisecond clock, cycle counter) by the same number of cycles. The LCD instructions drive the board's SPI LCD and are no-ops in the emulator.

---

### Separate Source Registers

The CPU encodes `rd` (bits 11:8) and `rs1` (bits 7:4) separately in every instruction listed above as `rd [rs1]`: `ADDV`, `MINUSV`, `ANDV`, `ORV`, `XORV` and their `.S` forms, `MULV`, `DIVV`, `MODV`, the shifts, rotates and bit operations by a count (`SHLV`, `BSET`, …, `ROLR1`, `ROLCR`, …) and the unary operations (`NEGR`, `SEXTB`, `INCR`, `CLZ`, …). The assembler accepts both forms:

- one register is both destination and source, as it always was: `ADDV A 5` is A = A + 5;
- two registers are the destination, then the source, in the order the LLVM compiler writes them: `ADDV B A 5` is B = A + 5 (`addv r1, r0, 5`), `NEGR D C` is D = -C (`negr r3, r2`), `SHLV C A 3` is C = A << 3 (`shlv r2, r0, 3`).

The second token is the source register only when it is a register name and the line has one more token than the one-register form takes. `ADDV A B` therefore still adds the value of the label `B`, so every program written for the one-register form assembles to the same words.

A disassembly listing (the `.code` file written for an ELF or flat binary) shows one register when `rd` = `rs1` and both otherwise. Compiled code often uses a separate source: `ADDV B C 0x1F4` is B = C + 500, and leaves C unchanged.

### LLVM Names

The LLVM compiler's assembler writes registers as `r0`–`r15` (`A`–`P` here) and separates operands with commas; otherwise most of its names and operand orders are the ones used here. These differ:

| LLVM | klausscc | Notes |
|---|---|---|
| `addv r1, r2, 5`, `negr r1, r2`, … | `ADDV B C 5`, `NEGR B C`, … | Both forms are accepted; see [Separate Source Registers](#separate-source-registers) |
| **`ldidx64`, `stidx64`** | **`LDIDX64A`, `STIDX64A`** | **⚠ Same name, different instruction.** klausscc's `LDIDX64`/`STIDX64` are LLVM's `ldidx64u`/`stidx64u`; see [Loads](#loads) |
| `ldidx64u`, `stidx64u` | `LDIDX64U`, `STIDX64U` (= `LDIDX64`, `STIDX64`) | Accepted, with their `.S` forms; the listing uses these names |
| `ldidx8s`, `ldidx16s`, `ldidx32s` | `LDIDX8_S`, … (or `LDIDX8S`, …) | Both spellings are accepted, and by LLVM too |
| `fbr rs1, rs2, cc, target`, `fbri rs1, simm4, cc, target` | `BEQ` … `BUGTI`, or `FBR`/`FBRI` | See [Fused Compare-and-Branch](#fused-compare-and-branch) |
| `lcd` | `LCDRST` (or `LCD`) | |
| `rolr1`, `rorr1` | `ROLR1`, `RORR1` (= `ROLV r 1`, `RORV r 1`) | One encoding; the listing shows `ROLR1`/`RORR1`, as LLVM does |
| `shlar` | `SHLAR` (= `SHLR1`) | One encoding; the listing shows `SHLR1`, as LLVM does |
| `setr`, `setru`, `addv`, `addi`, `andi`, `ldidx32`, `stidx64`, `cmprv`, … with a small literal | the `.S` form (`SETR.S`, `ADDV.S`, …) | **⚠ Same name, different length.** LLVM picks the 1-word form when the literal fits and prints it under the 2-word name; here the name chooses the length, so `SETR A 5` is always 2 words. LLVM's `setr rX, 128..255` is the 1-word `SETRU.S` |
| `jmprel`, `jmpzrel`, … `jmpo`, `jmpno` to a near target | `JMP.S`, `JMPZ.S`, … `JMPO.S`, `JMPNO.S` | **⚠ Same name, different length**, as above: `JMPREL` here is always 2 words |
| (none) | `CALL.S` | LLVM has no 1-word call |

The aliases change nothing an existing program assembles to: each is a name that klausscc did not have before. The names that mean something else in LLVM (`LDIDX64`, `STIDX64`, and the 2-word names LLVM also uses for 1-word forms) keep their klausscc meaning.

## CPU Flags

The CPU has four flags:

| Flag | Meaning |
|------|---------|
| **Z** (zero) | The result was zero (for `BTST`: the tested bit was 0) |
| **S** (sign) | Bit 63 of the result was 1 |
| **C** (carry) | Carry out of an add, or **borrow** out of a subtract or compare: after `CMPRR a b`, C = 1 exactly when a < b unsigned. Also the bit shifted out by the 1-bit rotates. |
| **V** (overflow) | Signed overflow (and the other cases in the tables) |

Flags are sticky: an instruction that does not write a flag leaves it unchanged. The Flags column of each table lists what each instruction writes. In short:

- Add, subtract, compare, `INCR`, `DECR`: Z S C V.
- Shifts and rotates: Z (the 1-bit rotates and `ROLCR`/`RORCR` also C). The fixed 1-bit shifts (`SHLR1`, `SHRR1`, `SHRAR`) write nothing.
- `NEGR`, `NOTR`, `ZEXTB`, `ZEXTH`, `POPCNT`, `BEXTR`, `BTST`: Z. `SEXTB`, `SEXTH`: Z S. `ABSR`: Z V.
- Multiply: Z S V. Divide and modulo: Z V.
- Logic, min/max, moves, loads, stores, boolean compares, fused branches, jumps and calls: none.

There are no separate "equal" or "less" flags: the condition codes derive them from `Z S C V` (equal = Z, signed less = S ≠ V, unsigned less = C). Signed and unsigned conditional jumps therefore work after any instruction that sets all four flags, not only after a compare.

`SETFR rd` reads the flags into the top of a register: bit 63 = Z, bit 62 = Z (the old equal flag), bit 61 = C, bit 60 = V. The interrupt context word saves and restores all four (see [Interrupts](#interrupts)).

---

## Memory Layout

| Address | Contents |
|---------|----------|
| `0x0000_0000` | Heap header word 0: `heap_start` (low 32 bits), written by the assembler |
| `0x0000_0008` | Heap header word 1: `heap_end` (reserved, 0) |
| `0x0000_0010`, `0x0000_0018` | Heap header words 2 and 3 (reserved, 0) |
| `0x0000_0020` | `CODE_BASE`: the first line of the program |
| ... | Code and inline data, in source order, then the data sections |
| `heap_start` | The first free byte after the program, rounded up to 8 |
| up to `0x07FF_FFFF` | Free memory; the stack grows down from the top of the 128 MiB DDR2 |
| `0xF000_0000`–`0xFFFF_FFFF` | Memory-mapped devices |

- The heap header is four 64-bit words. Do not place code or data there.
- `heap_start` is the end of the program rounded up to 8 bytes, so the heap starts 8-byte aligned. The `.code` listing, the `.kbt` image and the network-boot image all use this rule.
- The stack lives at the top of DDR2 and grows down. The CPU comes out of reset with SP = `0x0800_0000`, and so does the emulator, so the first push writes `0x07FF_FFF8`.

---

## Memory-Mapped I/O

The v1 I/O instructions (LEDs, 7-segment, RGB, switches and UART: `LEDV`, `7SEGR`, `SWR`, `TXR`, ...) are gone. Devices are memory-mapped: any load or store whose address has `0xF` in its top four bits goes to a device, not to memory. Bits 27:16 of the address pick the device and bits 15:0 the register. Registers are 8 bytes apart. Any load or store size works; bits beyond a register's width read as 0. Reads from undefined offsets return 0, and writes to them are dropped.

| Base | Device | In the emulator |
|------|--------|-----------------|
| `0xF001_0000` | UART | Yes |
| `0xF002_0000` | RGB LEDs | No (reads 0, writes ignored) |
| `0xF003_0000` | 7-segment display | No |
| `0xF004_0000` | LEDs and switches | No |
| `0xF00D_0000` | Performance counters | `PERF_CTRL`, `PERF_CYCLES`, `PERF_INSTR` only |
| `0xF00F_0000` | Interrupt controller and timer | Yes (timer source only) |

The board has more devices: SD card, cache controller, Ethernet, crypto, a 2D blitter and a second core's mailbox. The KlaussCPU repository's `MMIO_MAP.md` describes them.

### UART — `0xF001_0000`

| Offset | Register | Access | Behaviour |
|--------|----------|--------|-----------|
| `0x00` | `TX_DATA` | W | Writing transmits bits 7:0. Wait until `STATUS.TX_BUSY` is 0 first: a write while busy is dropped (the byte being sent goes out unchanged). |
| `0x08` | `RX_DATA` | R | The oldest received byte in bits 7:0; reading it removes it. Read only while `STATUS.RX_EMPTY` is 0. |
| `0x10` | `STATUS` | R | Bit 0 `TX_BUSY`, bit 1 `RX_EMPTY`, bit 2 `RX_FULL` |

There is no transmit FIFO. The UART runs at 3,000,000 baud (33 clock cycles a bit at 100 MHz), so each byte keeps `TX_BUSY` set for about 3.3 µs (334 cycles from the write: ten bits of 33 cycles plus four cycles of the transmitter's own). A program loaded over the UART (or by netboot) starts with the transmitter idle: the CPU waits for the loader's 18-byte `Load Complete OK` reply to finish before it starts the program (klaussCPU 04db80e, `KlaussCPU.sv` `START_WAIT`), so `TX_BUSY` reads 0 and the first write is taken. Every write to `TX_DATA` must still wait for `TX_BUSY` to clear, because a write made while the previous byte is going out is dropped, so back-to-back writes lose bytes. Poll before the first write too: on bitstreams built before klaussCPU 04db80e the program starts while the reply is still being sent, which keeps `TX_BUSY` set for about the first 60 µs, and a first write made then is lost as well.

The receive FIFO is emptied when a load completes (also klaussCPU 04db80e), so nothing typed or sent before the load ended reaches the program; a byte that arrives during the short start wait is kept. On older bitstreams a byte left in the FIFO is still there when the program starts.

The emulator models this. A write sets `TX_BUSY` for 334 cycles of its clock (which counts 2 cycles per instruction). A program starts with the transmitter idle, as on the board since klaussCPU 04db80e (the emulator does not model older bitstreams, where the first write can be dropped). A write while busy is dropped, as on the board. `--emulate` lists each such write ("UART TX write while busy at PC 0x...") and warns, and `--emulate-test` fails the file; under `--gdbserver` a write while busy is reported on stderr. The emulator collects the transmitted bytes as the program's UART output. Its receive FIFO starts empty, as on the board, which empties it when a load completes (klaussCPU 04db80e); under `--emulate` it stays empty, so `RX_EMPTY` reads 1, and under `--gdbserver` stdin feeds it.

Sending and receiving with plain loads and stores:

```
_start
SETR B 0xF0010000        // UART base: TX_DATA at +0x00
SETR A 0x4F              // 'O'
CALL PUTC:
SETR A 0x4B              // 'K'
CALL PUTC:
SETR A 0x0A              // newline
CALL PUTC:
// Read one byte if one is waiting (RX_EMPTY clear), else skip.
LDIDX8 C B 0x10          // C = STATUS
ANDV C 0x2               // RX_EMPTY (bit 1)
CMPRV C 0
JMPNE NO_RX:
LDIDX8 A B 0x08          // A = RX_DATA (reading removes the byte)
CALL PUTC:               // echo it
NO_RX:
HALT

// PUTC: send A[7:0]; B = UART base. Waits while STATUS.TX_BUSY (bit 0) is set.
PUTC:
PUSH C
PUTC_WAIT:
LDIDX8 C B 0x10          // C = STATUS
ANDV C 0x1               // TX_BUSY
CMPRV C 0
JMPNE PUTC_WAIT:
MEMSET8 A B              // TX_DATA = A[7:0]
POP C
RET
```

For printing values and text, use the built-in `$TXR`, `$NEWLINE`, `$TXCHAR`, `$TXMEMCHAR` and `$TXSTR` macros with `uart_stubs.kla` (see [Built-in Macros](#built-in-macros)).

### LEDs, Switches, 7-Segment and RGB LEDs

These exist only on the board.

| Address | Register | Access | Description |
|---------|----------|--------|-------------|
| `0xF004_0000` | `LEDS` | RW | The 16 LEDs (bits 15:0) |
| `0xF004_0008` | `SWITCHES` | R | The 16 slide switches (bits 15:0) |
| `0xF003_0000` | `SEG_LOW` | W | 4 hex digits (bits 15:0) for the lower 4-digit group |
| `0xF003_0008` | `SEG_HIGH` | W | 4 hex digits (bits 15:0) for the upper group |
| `0xF003_0010` | `SEG_ALL` | W | 8 hex digits (bits 31:0) across both groups |
| `0xF003_0018` | `SEG_BLANK` | W | Any write blanks both groups |
| `0xF002_0000` | `RGB1` | RW | RGB LED 1: bits 11:8 red, 7:4 green, 3:0 blue |
| `0xF002_0008` | `RGB2` | RW | RGB LED 2, same layout |

Reading `SEG_LOW`, `SEG_HIGH` and `SEG_ALL` returns the display's internal padded form (`0x0d` per digit), not the value written.

```
_start
SETR B 0xF0040000        // LEDs / switches
SETR A 0xFFFF
MEMSET32 A B             // LEDS = 0xFFFF: all 16 LEDs on
LDIDX32 C B 0x08         // C = SWITCHES
SETR B 0xF0030000        // 7-segment display
SETR A 0xDEADBEEF
STIDX32 A B 0x10         // SEG_ALL: "DEADBEEF" across all eight digits
SETR B 0xF0020000        // RGB LEDs
SETR A 0xF00
MEMSET32 A B             // RGB1 = full red
HALT
```

| v1 instruction | Now |
|----------------|-----|
| `LEDR r` / `LEDV v` | Store to `0xF004_0000` |
| `SWR r` | Load from `0xF004_0008` |
| `7SEG1R`, `7SEG2R`, `7SEGR`, `7SEG1V`, ... | Store to `SEG_LOW`, `SEG_HIGH` or `SEG_ALL` |
| `7SEGBLANK` | Store anything to `0xF003_0018` |
| `RGB1R`, `RGB2R`, `RGB1V`, `RGB2V` | Store to `0xF002_0000` / `0xF002_0008` |
| `TXR r` / `NEWLINE` | `$TXR r` / `$NEWLINE` |
| `TXCHARMEMR r` / `TXSTRMEMR r` | `$TXMEMCHAR r` / `$TXSTR r` |

### Performance Counters — `0xF00D_0000`

| Offset | Register | Description |
|--------|----------|-------------|
| `0x00` | `PERF_CTRL` | Writing bit 0 clears the counters |
| `0x08` | `PERF_CYCLES` | Clock cycles since the last clear (64-bit read) |
| `0x10` | `PERF_INSTR` | Instructions retired since the last clear |

The board has more counters at higher offsets (see `MMIO_MAP.md`). The emulator models these three, counting 2 cycles per instruction.

---

## Interrupts

There are four interrupt sources. Source 0 is a periodic timer. On the board, source 1 is the blitter and source 2 is Ethernet; source 3 is reserved. Everything is set up through the interrupt controller at `0xF00F_0000`. The only interrupt instruction is `IRET`.

| Offset | Register | Access | Description |
|--------|----------|--------|-------------|
| `0x00` | `INT_MASK` | RW | Bit N enables source N (bits 3:0). 0 at reset and after a program load. |
| `0x08` | `INT_PENDING` | R | Bit N: source N is pending |
| `0x10` + 8 × N | `INT_VECn` | RW | Handler address for source N. A vector of 0 disables the source. |
| `0x30` | `TIMER_PERIOD` | RW | Timer period in clock cycles (reset `0x000F_FFFF`, about 10.5 ms at 100 MHz). Writing it restarts the count. |
| `0x38` | `TIMER_COUNT` | R | The timer's current count |
| `0x40` | `CLOCK_MS` | R | Milliseconds since the FPGA was configured (read it with one 64-bit load) |

An interrupt is taken between instructions when its mask bit is set, it is pending and its vector is not 0. The timer has priority over the blitter, which has priority over Ethernet. The CPU then:

1. pushes one 64-bit context word: PC in bits 31:0, the flags in bits 38:32, and `INT_MASK` in bits 42:39;
2. clears the source's `INT_MASK` bit, so the handler is not re-entered for the same source;
3. clears the timer's pending bit (source 0 only), and jumps to the source's vector.

`IRET` pops the context word and restores PC, flags and `INT_MASK`. The hardware saves no registers, so a handler must save and restore any register it changes. The blitter's pending bit stays set until its handler acknowledges the blitter, so that handler must do so before `IRET`.

`WAIT` sleeps until an enabled interrupt is pending. The handler runs, and its `IRET` returns to the instruction after the `WAIT`.

The emulator models the timer (source 0), `WAIT` and `IRET`. It counts time as 2 clock cycles per instruction, so interrupts arrive at different points than on the board. A `WAIT` that nothing can wake stops the emulator.

```
// Timer interrupt: the handler counts ticks in K; the main loop sleeps
// in WAIT and prints the count after each tick, three times.
// Expected UART output:
//   00000001
//   00000002
//   00000003
_start
SETR K 0                 // tick counter
SETR B 0xF00F0000        // interrupt controller base
SETR A TICK:
STIDX64 A B 0x10         // INT_VEC0 = handler address
SETR A 10000
STIDX64 A B 0x30         // TIMER_PERIOD = 10000 clock cycles
SETR A 1
MEMSET64 A B             // INT_MASK = 1: timer enabled
LOOP:
WAIT                     // sleep until the timer interrupt has run
$TXR K
$NEWLINE
CMPRV K 3
JMPNE LOOP:
SETR A 0
MEMSET64 A B             // INT_MASK = 0: interrupts off
HALT

TICK:                    // changes only K (and the flags, which IRET restores)
INCR K
IRET

!include uart_stubs.kla
```

---

## Emulator and Hardware Differences

`--emulate` and `--emulate-test` run the built-in emulator. It was checked against the KlaussCPU `pipeline_core.sv`, and no difference in what an instruction does (its registers, flags and memory, and which encodings are illegal) is known. It differs in timing and devices, and one MMIO corner is unchecked:

| Area | Hardware | Emulator |
|------|----------|----------|
| `DELAYR`/`DELAYV` | Busy-wait (value × 8192 cycles) | No wait; its clock advances by value × 8192 cycles |
| LCD instructions | Drive the SPI LCD | No effect |
| UART transmit | 334 cycles a byte; a write while `TX_BUSY` is dropped; idle when a program starts (on bitstreams before klaussCPU 04db80e, the loader's reply is still being sent then) | The same, on the emulator's clock (2 cycles per instruction), so a polling loop spins a different number of times. Always idle at start. Writes while busy are reported, and fail `--emulate-test` |
| UART receive | Emptied when a load completes (klaussCPU 04db80e); a byte arriving during the start wait is kept | Starts empty; `--gdbserver`'s stdin feeds it |
| Time, timer interrupts | 100 MHz clock | 2 cycles per instruction |
| Devices other than UART, timer and three perf counters | Present | Read 0, writes ignored |
| MMIO access at an address that is not a multiple of 8 | Up to the device's address decoder (not checked) | Decodes the exact address; no register is there, so it reads 0 or is ignored |

---

## Serial Communication

The assembler can upload programs to the FPGA board over its USB-UART and show the board's output. The link runs at 3,000,000 baud, 8N1.

### Uploading Programs

Use `-s` to send the program to the board after assembly:

```sh
klausscc -i myprogram.kla -s /dev/ttyUSB0
```

If `-s` is given without a port name, the assembler looks for FTDI USB serial devices (VID 0403, PID 6001, 6010, 6011 or 6014) and uses the one whose port name sorts last. The board's FT2232H shows up as two ports, JTAG and then the UART, so this picks the UART:

```sh
klausscc -i myprogram.kla -s
```

To upload, klausscc resets the board into its loader with a UART break (or, with `-n`, by sending `S` to a board already in its loader). It then sends the `.kbt` image with a progress bar and reports the loader's reply (for example "Load Complete OK"). A program with assembly errors is not sent.

### Monitoring UART Output

Use `-m` alongside `-s` to watch the board's UART output after uploading, for example what the program prints with `$TXR`, `$NEWLINE` and `$TXSTR`:

```sh
klausscc -i myprogram.kla -s -m
```

The monitor prints all received serial data to stdout as it arrives, and sends your keystrokes to the board. Press **Ctrl+C** to stop; the serial port is closed cleanly so it can be reused immediately.

### Automated Test Mode

Use `-T` (or `--test`) to check the board's UART output against expected values declared in the source file's comments:

```sh
klausscc -i test_bits.kla -s --test
```

The assembler:
1. Collects the expected values from the source's comments (see [Writing Test Programs](#writing-test-programs))
2. Assembles and uploads the program
3. Reads the UART output line by line and compares each line's 8-digit hex value against the next expected value
4. Reports PASS/FAIL per value and an overall summary

Example output:
```
Test mode: expecting 8 UART values (timeout 10s)...
  PASS [1/8]: 00000080 == 00000080
  PASS [2/8]: 00000000 == 00000000
  PASS [3/8]: 00000001 == 00000001
  FAIL [4/8]: got 00000002, expected 00000000
  ...

Test result: 7/8 passed, 1/8 failed
```

Use `--test-timeout` to change the wait (default 10 seconds):

```sh
klausscc -i test_bits.kla -s --test --test-timeout 20
```

`-L list.txt` runs every test in a list. Each line of the list is a `.kla` path relative to the list file; blank lines and lines starting with `//` or `#` are skipped. `--emulate-test` takes a list or a directory and runs the tests on the emulator instead.

**Exit codes:**
- `0`: all tests passed
- `1`: assembly error, no expected values found, or a serial port error (including a read error while waiting for UART output)
- `2`: one or more values did not match (for `-L` and `--emulate-test`, one or more tests failed or timed out)
- `3`: timed out before all expected values were received (this wins over `2`)

---

## Writing Test Programs

Test programs print results over the UART so that they can be checked both by eye and automatically. The programs in `tests/fixtures/` follow this pattern.

### Expected Values

The expected values are **every whole-line `//` comment whose text starts with 8 upper-case hex digits**, in file order, wherever it is in the file. By convention they sit in a header block:

```
// Expected UART output:
//   000000FF  (what this value checks)
```

- The `// Expected UART output:` line is only a label; the hex lines are what count.
- A value after code on the same line (`$TXR A // 000000FF`) is **not** collected. Neither is a lower-case value.
- Any other whole-line comment that starts with 8 hex digits is collected too. Avoid comments that start with words like `DEADBEEF` or `FACEFEED`.

On the output side, each UART line is trimmed and its first 8 characters must be upper-case hex digits; other lines (blank lines, the loader's "... Complete" reply, text) are skipped. `$TXR` followed by `$NEWLINE` produces exactly one such line.

### Test File Structure

```
// Test: register operations
// Expected UART output:
//   000000FF  (SETR A 0xFF)
//   00000100  (INCR A)
//   FFFFFFFE  (NEGR with 2)
_start
DELAYV 0x500             // let the loader's reply finish before printing

SETR A 0xFF
$TXR A
$NEWLINE

INCR A
$TXR A
$NEWLINE

SETR C 2
NEGR C
$TXR C
$NEWLINE

HALT

!include uart_stubs.kla
```

Run it on the emulator or the board:
```sh
klausscc --emulate-test .            # every .kla in this directory
klausscc -i test_regs.kla -s --test  # on the board
```

### Key Conventions

1. **Initial delay**: start with a `DELAYV` so the loader's reply has finished before the program prints. On bitstreams from klaussCPU 04db80e on, the reply has always finished by the time the program starts. (The emulator does not wait.)
2. **One value per line**: print each result with `$TXR`, then `$NEWLINE`. `$TXR` prints the low 32 bits, so check 64-bit results in two halves (`SHRV` by 32 for the top half).
3. **Include the UART routines last**: `!include uart_stubs.kla` after `HALT`, so execution never runs into them.
4. **Data after the code**: put `#NAME` data after `HALT` (or in a `.data` section), so it is never executed.

### Available Test Programs

The `tests/fixtures/` directory holds the test corpus; `cargo test` runs every file that lists expected values on the emulator, the same check as `klausscc --emulate-test tests/fixtures`. `tests/fixtures/all_tests.txt` lists them for `-L`.

| File | Covers |
|------|--------|
| `test_regs.kla` | `SETR`, `COPY`, `INCR`, `DECR`, `NEGR`, `ABSR` |
| `test_arithmetic.kla` | Add and subtract, register and immediate forms |
| `test_logic.kla` | `ANDR`, `ORR`, `XORR`, `ANDV`, `ORV`, `XORV`, `BSWAP` |
| `test_shifts.kla` | Shifts and 1-bit rotates |
| `test_rotate.kla` | Rotates by register and by count |
| `test_bits.kla` | `BSET`, `BCLR`, `BTGL`, `POPCNT`, `CLZ`, `CTZ`, `BITREV` |
| `test_sign_extend.kla` | `SEXTB`, `SEXTH`, `ZEXTB`, `ZEXTH` |
| `test_muldiv.kla` | Multiply, divide and modulo |
| `test_three_operand.kla` | The in-place instructions with a separate source register, and the LLVM names `FBR`, `FBRI`, `LDIDX64U`, `STIDX64U`, `LDIDX8S` |
| `test_compare_jump.kla` | Compares and conditional jumps |
| `test_compare_ext.kla` | Signed and unsigned compares, min/max |
| `test_memory.kla` | 64-bit loads and stores |
| `test_indexed_mem.kla` | Base + offset and base + register addressing |
| `test_stack.kla` | `PUSH`, `POP`, `PUSHV` |
| `test_call_ret.kla` | `CALL`, `RET`, conditional calls |
| `test_strings.kla` | The `#name "text"` layout, read a byte at a time |
| `test_txstr.kla` | Printing `#name "text"` strings with `$TXSTR` |
| `test_edge_cases.kla`, `test_boundary.kla` | Boundary values and overflow |
| `test_loop_patterns.kla`, `test_algorithms.kla`, `test_benchmark.kla` | Loops and small algorithms |
| `test_directives.kla` | `.data`, `.text`, `.word`, `.comm`, `.global` |
| `test_io.kla` | A numbered step sequence, checked by eye (no expected values) |
| `hw_alignment.kla` | The emulator-vs-hardware check: flags, divide by zero, unaligned 64-bit access, `BEXTR`/`BDEP`, `.S` immediates, strings and inline data, each derived from the RTL |
| `test2.kla`, `test_64bit.kla` | Compiled C linked with the runtime (see [C Programs and the Runtime](#c-programs-and-the-runtime)); `test_64bit` runs 33 64-bit integer checks and expects `00000000`, the failure count |
| `uart_stubs.kla` | The UART print routines the tests include |

---

## C Programs and the Runtime

`runtime/` holds the C runtime for programs compiled with lcc `-target=klacpu`:

| File | What it is |
|------|------------|
| `crt0.kla` | Startup code: `_start` sets SP to `0x0800_0000`, calls `main`, prints `main`'s return value as 8 hex digits and a newline, then halts. It includes `libc.kla` and `uart_stubs.kla`. |
| `libc.kla` | The C library (`libc.c` compiled with lcc): character, string and number output, `strlen`/`strcmp`/`strcpy`, `memset`/`memcpy`, and a `malloc`/`free`/`calloc`/`realloc` heap. `libc.h` declares it. |
| `uart_stubs.kla` | The UART routines `libc.kla` calls: `_uart_tx_hex`, `_uart_newline`, `_uart_tx_char`, `_uart_rx_char` and `_uart_rx_char_nb`. |

`runtime/uart_stubs.kla` is not the same file as `tests/fixtures/uart_stubs.kla`: the runtime's routines have the C names above, and the fixtures' file holds the `TX_...` routines the `$TX...` macros call.

A compiled program has no `_start` of its own. Add one line to the compiled `.kla`, with the path relative to that file, and assemble it as usual:

```
!include path/to/runtime/crt0.kla
```

```sh
build/cpp prog.c | build/rcc -target=klacpu > prog.kla    # in the lcc tree
klausscc -i prog.kla --emulate
```

Do not include `libc.kla` or `uart_stubs.kla` as well: `crt0.kla` brings them in. Because `crt0.kla` prints `main`'s return value, a program can return a result or a failure count and list it as its expected value. `tests/fixtures/test2.kla` and `tests/fixtures/test_64bit.kla` are built this way.

The calling convention is lcc's: arguments in A, B, C, ..., the result in M, and P is the frame pointer. Assembly code can call the library too. Include both runtime files after `HALT`, and assume a call changes the flags and every register except SP and P:

```
_start
SETR A #MSG
CALL print_str:          // print the string at A
SETR A -1234
CALL print_int:          // signed decimal
CALL newline:
HALT

#MSG "libc says "
!include libc.kla        // copies of runtime/libc.kla and runtime/uart_stubs.kla
!include uart_stubs.kla
```

This prints `libc says -1234`.

---

## Complete Examples

Each example uses `uart_stubs.kla` where it prints; copy that file next to it.

### Example 1: Counting Loop

Counts down from 10, adding each value into a sum, then prints the sum (0x37 = 55):

```
_start
SETR A 0                 // sum
SETR B 10                // counter
LOOP:
ADDR A A B               // A = A + B
DECR B                   // B = B - 1; sets Z when B reaches 0
JMPNZ LOOP:
$TXR A                   // prints 00000037
$NEWLINE
HALT

!include uart_stubs.kla
```

### Example 2: Memory Read/Write

Fills an 8-entry table with the squares 0 to 49, then reads it back and prints the sum (0x8C = 140):

```
_start
SETR A #TABLE            // A = address of the table
SETR B 0                 // i = 0
FILL:
MULR C B B               // C = i * i
COPY D B
SHLV D 3                 // D = i * 8, the byte offset of entry i
STIDX64R C A D           // TABLE[i] = C
INCR B
CMPRV B 8
JMPNE FILL:              // for i = 0 .. 7

SETR E 0                 // sum
SETR D 0                 // byte offset
SUM:
LDIDX64R C A D           // C = TABLE[offset]
ADDR E E C
ADDV D 8
CMPRV D 64
JMPNE SUM:
$TXR E                   // prints 0000008C (140)
$NEWLINE
HALT

!include uart_stubs.kla

.data
#TABLE 8                 // 8 64-bit words, 8-byte aligned in the data section
```

### Example 3: Printing a String

`$TXSTR` prints bytes in memory order up to a NUL, which is how `#name "text"` stores a string:

```
_start
SETR A #HELLO
$TXSTR A                 // prints "Hello!" and a newline
HALT

// \n is stored as CR LF. No comment may follow the closing quote.
#HELLO "Hello!\n"
!include uart_stubs.kla
```

### Example 4: Subroutines and the Stack

```
_start
SETR A 5
CALL FACT:               // M = 5! = 120 = 0x78
$TXR M
$NEWLINE
HALT

// FACT: M = A! (A >= 1). Keeps A and B.
FACT:
$PUSH2 A B
SETR M 1
FACT_LOOP:
MULR M M A               // M = M * A
DECR A
BNEI A 0 FACT_LOOP:      // fused compare-and-branch
$POP2 A B
RET

!include uart_stubs.kla
```

### Example 5: Using Macros

```
!macro $SAVEABC PUSH A / PUSH B / PUSH C
!macro $RESTOREABC POP C / POP B / POP A
!macro $SET2 SETR %1 %3 / SETR %2 %3     // $SET2 r1 r2 value
!macro $SHOW $TXR %1 / $NEWLINE          // a macro may use other macros
_start
$SET2 A B 7              // A = B = 7
SETR C 3
$SAVEABC
$CLR A                   // built-in: SETR A 0
$RESTOREABC              // A = 7 again
$SHOW A                  // prints 00000007
$WAIT 0xFFFF 0x1000      // built-in: DELAYV 0xFFFF / DELAYV 0x1000
HALT

!include uart_stubs.kla
```

---

## Output Files

When assembling, klausscc writes:

| File | Description |
|------|-------------|
| `<name>.code` | Listing: every line with its address and machine code |
| `<name>.kbt` | The image to send to the board, in its serial wire format |

The listing is written even when there are errors, so you can see where they are. The `.kbt` is not written if there are errors.

### The `.code` Listing

The listing starts with the four heap-header words, then shows every source line after macro expansion. For this program:

```
_start
SETR A 5
LOOP:
DECR A
JMPNZ LOOP:
HALT
#S "Hello"
.word 7
```

the listing is:

```
0x00000000: 0000000000000048                  -- heap_start  (set by assembler)
0x00000008: 0000000000000000                  -- heap_end    (reserved)
0x00000010: 0000000000000000                  -- (reserved)
0x00000018: 0000000000000000                  -- (reserved)
                              -- _start
0x00000020: 8BD00000 00000005 -- SETR A 5
0x00000028:                   -- LOOP:
0x00000028: 57C80000          -- DECR A
0x0000002C: A00C0000 00000028 -- JMPNZ LOOP:
0x00000034: 6C010000          -- HALT
0x00000038: 6C6C65480000006F                  -- #S "Hello"
0x00000040: 0000000700000000                  -- .word 7
```

- An instruction line shows its address, then word 0 and any immediate words, as 8 hex digits each.
- A data line shows one row per 8 bytes: two 32-bit words in memory order (lower address first), each as its 32-bit value. A `.word 7` shows as `0000000700000000`.
- A label shows its address. Comments, blank lines, `_start` and directives show only their text.
- A line that could not be assembled shows `Error` in place of its address, for example `Error                         -- ADDR A, B, C`.

For an ELF or flat binary, the listing is a disassembly: each instruction is shown as the assembler would write it, with branch targets as distances (`BNE O M PC-8`). An instruction with a separate source register shows both registers (`ADDV B C 0x1F4`, see [Separate Source Registers](#separate-source-registers)), and the names follow LLVM's where the same name means the same instruction in both (see [LLVM Names](#llvm-names)). A word that is no instruction shows as `??? (0x...)`.

### The `.kbt` Image

The `.kbt` file is one line of text: `S`, then every 32-bit word as 8 hex digits, then `Z`, a checksum word and `X`. Each word is written least significant byte first (the instruction `0x6C010000` is `0000016C`), because the loader stores bytes in the order they arrive. The words are:

1. the 8 words of the heap header (`heap_start`, then 7 zeros);
2. the program, from `0x20`;
3. the entry point (the address after `_start`).

The checksum is the sum of the two 16-bit halves of every word between `S` and `Z`, plus twice the number of those words, modulo 65536.

### Documentation Files

With `--opcodes` (written to the current directory):

| File | Description |
|------|-------------|
| `klausscc_opcodes_v2.html` | The opcode and macro tables |
| `klausscc_opcodes_v2_opcodes.json` | The opcodes: mnemonic, word-0 template and operand layout |
| `klausscc_opcodes_v2_macros.json` | The built-in macros |

With `--textmate`:

| File | Description |
|------|-------------|
| `klausscc_opcodes_v2_textmate.txt` | Pipe-delimited opcode names for a TextMate/VSCode syntax grammar |

---

## Assembly Process

The assembler works through the source in these steps:

1. **Read**: the input file is read, with each `!include` replaced by the file it names.
2. **Block comments**: `/* ... */` comments are removed.
3. **Macros**: `!macro` and `!macros` definitions are collected and removed, then every macro call is expanded (nested macros included).
4. **Pass 1**: each line is classified (instruction, label, data, directive, ...) and given its address. A `SETR` whose literal needs more than 32 bits becomes `SETR64`. Data in `.data`/`.rodata`/`.bss` sections, with its labels, is moved after the code.
5. **Labels**: the symbol table is built from the labels and data names.
6. **Pass 2**: every instruction is encoded, with its labels resolved, and every data line is turned into bytes. The `.code` listing and, if there were no errors, the `.kbt` image are written.

An error does not stop the run: the assembler carries on and reports every problem it finds, each with its file and line.
