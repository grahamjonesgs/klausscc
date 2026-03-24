# Klauss ISA CPU Test Suite

Test programs for verifying correct operation of the Klauss FPGA CPU.

## Verification Method

Each test uses **UART serial output** (`TXR`) as the primary verification method. Every `TXR` outputs the register value as 8 hex digits, giving exact values to compare against expected results.

Additionally:
- **7-segment display** shows the test number so you can visually track which test is running
- **LEDs** set to `0xFF` (all on) at the end of each test to indicate completion
- **HALT** stops the CPU after the test finishes

### Reading Results

Connect a serial terminal (e.g. `minicom`, `screen`, `picocom`) to the FPGA's UART output. Each test prints its expected values one per line. Compare against the expected output listed in the test file header comments.

```sh
# Example: monitor serial output
picocom /dev/ttyUSB0 -b 115200
```

## Running Tests

Assemble and upload each test individually:

```sh
klausscc -c ../klacode/opcode_select.vh -i test_regs.kla -s /dev/ttyUSB0
```

Or assemble without uploading to check for assembly errors:

```sh
klausscc -c ../klacode/opcode_select.vh -i test_regs.kla
```

## Test List

| # | File | Category | What it tests |
|---|------|----------|---------------|
| 01 | `test_regs.kla` | Register ops | SETR, COPY, INCR, DECR, NEGR, ABSR |
| 02 | `test_arithmetic.kla` | Arithmetic | ADDRR, MINUSRR, ADDV, MINUSV, overflow wrap |
| 03 | `test_logic.kla` | Logic/Bitwise | AND, OR, XOR (register and value variants), BSWAP |
| 04 | `test_shifts.kla` | Shifts/Rotates | SHLR, SHRR, SHLAR, SHRAR, SHLV, SHRV, SHRAV, ROLR, RORR |
| 05 | `test_bits.kla` | Bit manipulation | BSET, BCLR, BTGL, POPCNT, CLZ, CTZ, BITREV |
| 06 | `test_muldiv.kla` | Multiply/Divide | MULRR, MULURR, DIVRR, DIVURR, MODRR, MODURR, MULV, DIVV, MODV |
| 07 | `test_compare_jump.kla` | Comparison/Jumps | CMPRR, JMPZ, JMPNZ, JMPE, JMPNE, JMPS, JMPNS, JMPLT, JMPGT |
| 08 | `test_memory.kla` | Memory access | MEMSETRR, MEMREADRR, MEMSETR, MEMREADR, sequential access |
| 09 | `test_stack.kla` | Stack | PUSH, POP, PUSHV, LIFO ordering |
| 10 | `test_call_ret.kla` | Subroutines | CALL, RET, nested calls, conditional CALLE/CALLNE |
| 11 | `test_io.kla` | I/O peripherals | LEDV, LEDR, 7SEG, 7SEGBLANK, SWR, RGB (visual inspection) |
| 12 | `test_strings.kla` | String/Data | #DATA strings, TXSTRMEMR, string_print subroutine |
| 13 | `test_edge_cases.kla` | Edge cases | Zero wrap, max values, sign boundary, all 16 registers |
| 14 | `test_sign_extend.kla` | Type conversion | SEXTB, SEXTH, ZEXTB, ZEXTH |
| 15 | `test_indexed_mem.kla` | Indexed memory | LDIDX, STIDX, LDIDXR, STIDXR |
| 16 | `test_loop_patterns.kla` | Algorithms | Countdown, sum, factorial, find-max (integration test) |

## Expected Output Quick Reference

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
78563412
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
80000000
```

### test_bits (05)
```
00000080
00000000
00000001
00000000
00000008
00000018
00000008
FF000000
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
Any value of `FFFFFFFF` indicates a failed branch test.

### test_memory (08)
```
000000AA
000000BB
DEADBEEF
00000003
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

### test_sign_extend (14)
```
FFFFFF80
0000007F
FFFF8000
00007FFF
000000AB
0000CDEF
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

### test_loop_patterns (16)
```
0000000A
00000037
00000018
00000007
```

## Notes

- Tests 11 (I/O) and 12 (strings) require visual/terminal inspection rather than fixed hex comparison
- Test 12 requires `string_print.kla` from `../klacode/` -- copy it to this directory or adjust the include path
- Test 15 (indexed memory) may need adjustment depending on how `LDIDX`/`STIDX` encode their register/offset operands in the hardware
- Some expected values depend on exact CPU flag behaviour -- if a test fails, check whether the flag semantics match your Verilog implementation
