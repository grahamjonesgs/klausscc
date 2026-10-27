# Klauss CPU Test Suite

Test programs for the Klauss FPGA CPU, written in the current assembly syntax
(ISA encoding v2; see [`doc/ISA_ENCODING_V2.md`](../../doc/ISA_ENCODING_V2.md)).
They run on the board, and they are also the corpus `cargo test` runs on the
built-in emulator.

## Verification Method

Each test prints its results over the **UART**. The built-in `$TXR` macro
prints the low 32 bits of a register as 8 upper-case hex digits, and
`$NEWLINE` ends the line. Both call routines in `uart_stubs.kla` (in this
directory), which every test `!include`s after its `HALT`.

The values a test should print are listed in its header comments, one per
line:

```
// Expected UART output:
//   0000002A  (SETR A 0x2A)
//   0000002B  (ADDV A 1)
```

The test runners (`-T`, `-L`, `--emulate-test` and `cargo test`) take every
comment line (a line that starts with `//`) whose text starts with 8
upper-case hex digits, anywhere in the file, as the next expected value. A
`//` comment after an instruction on the same line does not count. They then
match those values in order against each line of UART output that starts
with 8 upper-case hex digits; other lines, blank ones included, are skipped.
A file with no such comments has nothing to check: `-L` reports it as
`SKIP`, and `--emulate-test` leaves it out.

The registers are 64 bits wide, but `$TXR` prints only the low 32 bits.
Results that land in the upper half therefore print as `00000000`; for
example, `BSWAP` and `BITREV` reverse all 64 bits. The expected values allow
for this.

A minimal test (assemble it next to a copy of `uart_stubs.kla`):

```
// Test: my feature
// Expected UART output:
//   0000002A  (SETR A 0x2A)
//   0000002B  (ADDV A 1)

_start
SETR A 0x2A
$TXR A
$NEWLINE
ADDV A 1
$TXR A
$NEWLINE
HALT

!include uart_stubs.kla
```

## Running Tests

Run these from this directory. The opcode table and the macros are built
into `klausscc`, so no opcode file is needed.

```sh
# Assemble only (writes test_regs.code and test_regs.kbt), to check for errors
klausscc -i test_regs.kla

# Run on the emulator and print the captured UART output
klausscc -i test_regs.kla --emulate

# Check every file with expected values, or every file in a list, on the emulator
klausscc --emulate-test .
klausscc --emulate-test all_tests.txt

# On the board: send one test and check its output (-T), or run a whole list (-L)
klausscc -i test_regs.kla -s /dev/ttyUSB0 -T [--test-timeout 20]
klausscc -L all_tests.txt -s /dev/ttyUSB0
```

With a bare `-s` (no port), the board's FTDI USB-UART is found automatically.
`-T` waits up to `--test-timeout` seconds (default 10) for the expected
values. `-L` assembles each listed file, writing its `.code` listing and
`.kbt` image beside it, and then sends and checks it. Paths in a list are relative to the list
file. Lines that are blank or start with `//` or `#` are ignored.

Exit status: `0` all passed, `2` a test failed, `3` a `-T` test timed out,
and `1` any other error. In a `-L` run a timeout counts as a failure (`2`).
A `-L` or `--emulate-test` run in which no file could be checked exits `1`.

To watch the board's output by eye, send with `-m` (`klausscc -i
test_io.kla -s -m`), or connect any serial terminal at 3000000 baud (8N1).
That is the rate the bitstream fixes for the board UART.

### In `cargo test`

- `emulator_corpus_passes` (in `src/commands/emulate.rs`) assembles every
  `.kla` here that lists expected values and checks them on the emulator. It
  is the same check as `klausscc --emulate-test tests/fixtures`. It also fails
  if fewer than 25 files pass, so a test that stops declaring its values is
  noticed.
- `every_kla_file_assembles` (in `src/asm/corpus_tests.rs`) assembles every
  `.kla` here and in `runtime/` and fails on any error. A file that is only
  meant to be included (`uart_stubs.kla`, `string_print.kla` and the runtime
  files) is listed there with a small driver that supplies the `_start` or
  `main` it lacks; a new file of that kind needs an entry.
  `driven_libraries_print_what_their_drivers_expect` also runs the
  `string_print.kla` driver on the emulator and checks what it prints.
- `tests/roundtrip.rs` runs `v2_hello.kla`, `v2_loop.kla` and `v2_math.kla`
  with `--emulate` and checks the exact characters they print.
- `tests/cli.rs` runs `--emulate-test` on `test_strings.kla` and checks that
  it exits `0`.
- The loader tests in `src/loader.rs` parse `hello.elf` and `test_64bit.elf`.

## Test List

These are the tests in `all_tests.txt`. The number is the one in each file's
header; `test_indexed_mem.kla` has none and fills the gap at 15.

| # | File | Category | What it tests |
|---|------|----------|---------------|
| 01 | `test_regs.kla` | Register ops | SETR, COPY, INCR, DECR, NEGR, ABSR |
| 02 | `test_arithmetic.kla` | Arithmetic | ADDR, SUBR, ADDV, MINUSV, overflow wrap |
| 03 | `test_logic.kla` | Logic/Bitwise | ANDR, ORR, XORR, ANDV, ORV, XORV, BSWAP |
| 04 | `test_shifts.kla` | Shifts/Rotates | SHLR1, SHRR1, SHLAR, SHRAR, SHLV, SHRV, SHRAV, ROLR1, RORR1 |
| 05 | `test_bits.kla` | Bit manipulation | BSET, BCLR, BTGL, POPCNT, CLZ, CTZ, BITREV |
| 06 | `test_muldiv.kla` | Multiply/Divide | MULR, MULUR, DIVR, DIVUR, MODR, MODUR, MULV, DIVV, MODV |
| 07 | `test_compare_jump.kla` | Comparison/Jumps | CMPRR, JMPZ, JMPNZ, JMPE, JMPNE, JMPS, JMPNS, JMPLT, JMPGT |
| 08 | `test_memory.kla` | Memory access | MEMSET64RR, MEMREADRR, MEMSETR, MEMREADR; the sub-word absolute LDABS8/16/32, LDABS8S/16S/32S, STABS8/16/32 |
| 09 | `test_stack.kla` | Stack | PUSH, POP, PUSHV, LIFO ordering |
| 10 | `test_call_ret.kla` | Subroutines | CALL, RET, nested calls, CALLE, CALLNE |
| 11 | `test_io.kla` | Step sequence | Prints the step numbers 1 to 10, pausing after each; it does not use the LED, 7-segment, switch or RGB registers. No expected values, so it is skipped |
| 12 | `test_strings.kla` | String data | `#name "string"` data layout, read back a byte at a time with LDIDX8 |
| 13 | `test_edge_cases.kla` | Edge cases | Zero wrap, max values, sign boundary, all 16 registers |
| 14 | `test_sign_extend.kla` | Type conversion | SEXTB, SEXTH, ZEXTB, ZEXTH |
| 15 | `test_indexed_mem.kla` | Indexed memory | STIDX64, LDIDX64, STIDX64R, LDIDX64R; the sub-word register-offset LDIDX8R/16R/32R, LDIDX8SR/16SR/32SR, STIDX8R/16R/32R |
| 16 | `test_loop_patterns.kla` | Algorithms | Countdown, sum, factorial, find-max |
| 17 | `test_boundary.kla` | Stress | Boundary values, overflow, MULHR, MULHUR |
| 18 | `test_rotate.kla` | Shifts/Rotates | ROLR1, RORR1, ROLV, RORV, ROLR, RORR |
| 19 | `test_compare_ext.kla` | Comparison | Signed/unsigned jumps after CMPRR, MINR, MAXR, MINUR, MAXUR, CALLZ, CALLNZ, JMPR |
| 20 | `test_algorithms.kla` | Algorithms | Fibonacci, bubble sort, GCD, prime detection |
| 21 | `test_benchmark.kla` | Stress | Binary search, bit ops, high multiply, carry flags, indexed memory |
| 22 | `test_txstr.kla` | String data | `#name "string"` printed with `$TXSTR`: several words, `\n`, an empty string |
| 23 | `hw_alignment.kla` | Emulator vs hardware | Every behaviour the emulator was recently aligned with the RTL; see [The emulator-vs-hardware check](#the-emulator-vs-hardware-check) |
| — | `test_directives.kla` | Directives | `.global`, `.extern`, `.text`, `.word`, `.comm` |
| — | `hello_world.kla` | Compiled C | Arithmetic, factorial, GCD, Fibonacci, primes |
| — | `test_64bit.kla` | Compiled C | 33 64-bit checks; expects main's return value (the failure count) `00000000` |

The list holds every file here with expected values (plus `test_io`), so a
board run covers the whole emulator corpus; `all_tests_lists_every_file_with_expected_values`
(in `src/commands/emulate.rs`) checks this.

### Other fixtures

| File | What it is |
|------|------------|
| `hello_world.kla` | Compiled C: arithmetic, factorial, GCD, Fibonacci and primes. Has expected values, so it is in the emulator corpus and `all_tests.txt` |
| `test_directives.kla` | The directives C compilers emit (`.global`, `.extern`, `.text`, `.word`, `.comm`). In the emulator corpus and `all_tests.txt` |
| `v2_hello.kla`, `v2_loop.kla`, `v2_math.kla` | Minimal programs that store straight to the UART `TX_DATA` register (`0xF0010000`), without `uart_stubs.kla`, polling `STATUS.TX_BUSY` before each byte. Checked by `tests/roundtrip.rs`. They print characters, not hex values, so they list no expected values and are not in `all_tests.txt` |
| `uart_stubs.kla` | The UART print routines the tests include: `TX_HEX32`, `TX_NL`, `TX_CHAR`, `TX_STR` |
| `string_print.kla` | A `STRING_PRINT` subroutine for inclusion; not a program by itself |
| `demo.kla`, `hello_simple.kla` | Compiler output with its own startup code; they run but list no expected values |
| `test2.kla`, `test_64bit.kla` | Compiler output (lcc `-target=klacpu`) for `test2.c` and `test_64bit.c`, with one added `!include ../../runtime/crt0.kla` line that brings in the startup code, `libc.kla` and the runtime's `uart_stubs.kla`. `test2` prints `Hellow World` and main's return value; `test_64bit` runs 33 checks and is in the emulator corpus and `all_tests.txt`, expecting main's return value (the failure count) `00000000` |
| `memcoherency.kla` | Diagnostic: byte stores followed by a 64-bit read, printed as two 32-bit halves |
| `test_mem0.kla` | Diagnostic: prints the word at address 0 (the heap header's `heap_start`) |
| `speed.kla` | An endless `INCR` loop with no output (it never halts) |
| `all_tests.txt` | The test list for `-L` and `--emulate-test` |
| `*.c` | C sources of the compiled programs and other C samples, for the external C compilers |
| `*.elf` | Prebuilt ELF images. They are v1-encoded: the loader tests use them only to check ELF parsing, and they do not run on the v2 emulator or CPU |

## The emulator-vs-hardware check

`hw_alignment.kla` checks, in one board run, that the emulator and the CPU
agree on each behaviour the emulator was changed to match the RTL:

- a) `ANDR`/`ORR`/`XORR`/`XORV` write no flags;
- b) `MULR`/`MULUR`/`MULHR`/`MULHUR`/`MULV` write Z, S and V (V on a
  low-half overflow, cleared by the high-half forms) and keep C;
- c) `DIVR`/`MODR` write Z and clear V; divide by zero gives all ones (DIV)
  or the dividend (MOD), sets V and keeps Z; `INT_MIN / -1`, `INT_MIN % -1`;
- d) `ABSR` of `INT_MIN` (result and V);
- e) `ROLR`/`RORR` by a register count of 1 keep C, `ROLR1`/`RORR1` write it;
- f) the F-bit Z (and SEXT's S) writes of the unary ops, `COPY` and `CLZ`
  writing none, `INCR` writing all four;
- g) 64-bit loads and stores with A=0 at an address 4 past a doubleword
  reach the aligned doubleword (scratch buffer at `0x0020_0000`);
- h) `BEXTR` reading `rs1[63:32]` and writing Z; `BDEP` zero-extending;
- i) the `.S` imm8 forms at -128/127 (sign-extended) and 0/255
  (zero-extended);
- j) a `#name` string printed with `$TXSTR` and read with byte loads;
- k) code after inline data that the assembler padded to 8 bytes.

Flag lines print `0000ZSCV`, one hex digit per flag. Each check sets the
flags to a known state first, so a flag the instruction does not write
shows its old value. The comments by each check derive its values from
`pipeline_core.sv` (with line numbers), so they can be checked without the
emulator. If `--emulate-test` passes and the board run (`-T`) fails, the
first wrong line names the behaviour that differs.

## Expected Output Quick Reference

These are copied from the test headers, which are what the runners check.
`test_io` (11) has no expected values.

### test_regs (01)
```
000000FF
000000FF
00000100
000000FE
FFFFFFFE
00000002
```

### test_arithmetic (02)
```
00000030
00000010
0000006E
0000005E
00000000
```

### test_logic (03)
```
00000012
0000FFFF
0000FF00
000000F0
000000FF
000000F0
00000000
```

### test_shifts (04)
```
00000002
00000001
FFFFFFFE
FFFFFFFF
00000F00
0000000F
FFFFFF80
00000003
00000000
```

### test_bits (05)
```
00000080
00000000
00000001
00000000
00000008
00000038
00000008
00000000
```

### test_muldiv (06)
```
00000064
00000064
0000000A
0000000A
00000003
00000003
00000032
00000005
00000002
```

### test_compare_jump (07)
```
00000001
00000002
00000003
00000004
00000005
00000006
00000007
00000008
```
A wrong branch prints `FFFFFFFF` in place of its value.

### test_memory (08)
```
000000AA
000000BB
DEADBEEF
00000003
000000CC
FFFFFFCC
0000CCDD
FFFFCCDD
88776655
00000000
FFFFFFFF
778888FF
55667788
```

### test_stack (09)
```
00000011
00000022
00000011
000000FF
0000000C
0000000B
0000000A
```

### test_call_ret (10)
```
00000001
0000000A
00000003
00000042
00000099
```

### test_strings (12)
```
00000002
48454C4C
4F000000
00000002
41424344
45460000
00000002
31323334
35000000
```

### test_edge_cases (13)
```
00000000
FFFFFFFF
7FFFFFFF
80000000
00000001
00000010
00000079
```

### test_sign_extend (14)
```
FFFFFF80
0000007F
FFFF8000
00007FFF
000000AB
0000CDEF
```

### test_indexed_mem (15)
```
000000AA
000000BB
000000CC
000000BB
000000CC
FFFFFFCC
0000CCDD
FFFFCCDD
88776655
00000000
FFFFFFFF
778888FF
55667788
```

### test_loop_patterns (16)
```
0000000A
00000037
00000018
00000007
```

### test_boundary (17)
```
FFFFFFFF
00000000
80000000
7FFFFFFF
00000000
80000001
80000000
00000001
FFFFFFFE
00000000
00000000
```

### test_rotate (18)
```
00000002
00000000
00000100
00000000
00000010
00000000
FFFFFFFF
00000003
C0000000
```

### test_compare_ext (19)
```
00000001
00000002
00000003
00000004
00000005
00000003
00000007
00000005
FFFFFFFF
0000000A
0000000B
0000000C
```

### test_algorithms (20)
```
00000037
0000000A
00000001
00000009
00000006
00000015
00000008
00000013
```

### test_benchmark (21)
```
00000037
00000005
00000001
00000009
00000002
FFFFFFFF
0000ABCD
0000000B
00000055
00000000
00000008
0000000A
00000030
FFFFFFFE
00000001
00000002
00000006
00000008
00000013
```

### test_txstr (22)
```
11111111
2222BBBB
CAFEF00D
600DC0DE
```

### hw_alignment (23)
102 values; see the file's header, which labels each one with its check.

## Notes

- Register-register ALU instructions take three registers, `rd rs1 rs2`
  (`MULR C A B` sets `C = A * B`). The immediate and single-register forms
  (`ADDV A 1`, `NEGR A`, `SHLV A 4`) work in place. `klausscc --opcodes` lists
  every mnemonic and its operands.
- Some expected values depend on exact flag behaviour. If a test passes on the
  emulator but fails on the board, compare the flag rules in
  [`doc/EMULATOR_ISA_SEMANTICS.md`](../../doc/EMULATOR_ISA_SEMANTICS.md) with
  the RTL.
- The `.code` and `.kbt` files that assembling writes here are build outputs
  and are ignored by git.
