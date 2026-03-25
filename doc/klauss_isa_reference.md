# Klauss ISA Assembly Language Reference

This document describes the Klauss Instruction Set Architecture (ISA) and its assembler, **klausscc**. The Klauss ISA is a custom 32-bit architecture implemented on an FPGA, with a rich instruction set covering arithmetic, logic, memory, I/O, and flow control.

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
  - [Register Operations](#register-operations)
  - [Arithmetic](#arithmetic)
  - [Logic and Bitwise](#logic-and-bitwise)
  - [Sign Extension and Type Conversion](#sign-extension-and-type-conversion)
  - [Min/Max Operations](#minmax-operations)
  - [Shift and Rotate](#shift-and-rotate)
  - [Bit Manipulation](#bit-manipulation)
  - [Multiply and Divide](#multiply-and-divide)
  - [Comparison](#comparison)
  - [Memory Access](#memory-access)
  - [Flow Control (Jumps and Calls)](#flow-control-jumps-and-calls)
  - [Stack Operations](#stack-operations)
  - [I/O and Peripherals](#io-and-peripherals)
  - [UART / Debug Output](#uart--debug-output)
  - [Interrupts](#interrupts)
  - [System](#system)
- [Serial Communication](#serial-communication)
  - [Monitoring UART Output](#monitoring-uart-output)
  - [Automated Test Mode](#automated-test-mode)
- [Writing Test Programs](#writing-test-programs)
- [Complete Examples](#complete-examples)

---

## Getting Started

A minimal Klauss assembly program:

```
_start
SETR A 0x00       // Load 0 into register A
BEGIN:
INCR A            // Increment A
7SEGR A           // Display A on seven-segment display
DELAYV 0xFFF      // Wait
JMP BEGIN:        // Loop forever
```

Save this as `myprogram.kla` and assemble it with:

```sh
klausscc -c opcode_select.vh -i myprogram.kla
```

The `opcode_select.vh` file comes directly from the Verilog source of the FPGA CPU design. It is the hardware's own definition of which instructions the CPU supports, so the assembler always stays in sync with the actual silicon -- if an instruction is added or changed in the Verilog, the assembler automatically picks it up.

---

## Assembler Usage

```
klausscc [OPTIONS] -c <opcode_file>
```

### Required Arguments

| Flag | Description |
|------|-------------|
| `-c`, `--opcode <file>` | Opcode definition file (`.vh`) from the Verilog CPU design. This file is part of the FPGA CPU hardware definition and serves as the single source of truth for available instructions. |
| `-i`, `--input <file>` | Assembly source file to assemble |

### Optional Arguments

| Flag | Description |
|------|-------------|
| `-o`, `--output <name>` | Output info file name (default: input stem + `.code`) |
| `-b`, `--bitcode <name>` | Output binary file name (default: input stem + `.kbt`) |
| `-s`, `--serial [port]` | Serial port to write binary output to the FPGA board. If given without a value, auto-detects the FTDI USB serial device. |
| `-m`, `--monitor` | After sending, stay connected and print all UART output from the board (Ctrl+C to stop) |
| `-T`, `--test` | Test mode: assemble, send to board, and verify UART output against expected values declared in source file comments. See [Automated Test Mode](#automated-test-mode). |
| `--test-timeout <secs>` | Timeout for test mode (default: 10 seconds) |
| `--opcodes` | Output opcode/macro documentation as HTML and JSON, then exit |
| `-t`, `--textmate` | Output opcode list for TextMate/VSCode syntax highlighting, then exit |

Note: `-m` and `-T` are mutually exclusive. Both require `-s`.

### Examples

Assemble a program:
```sh
klausscc -c opcode_select.vh -i count.kla
```

Assemble and upload to FPGA board via serial (auto-detect port):
```sh
klausscc -c opcode_select.vh -i count.kla -s
```

Assemble, upload, and monitor UART output:
```sh
klausscc -c opcode_select.vh -i count.kla -s -m
```

Assemble, upload, and run automated verification:
```sh
klausscc -c opcode_select.vh -i test_bits.kla -s --test
```

Generate ISA documentation:
```sh
klausscc -c opcode_select.vh --opcodes
```

---

## Source File Format

Assembly source files use the `.kla` extension. Each line contains one of:

- An instruction with optional operands
- A label definition
- A data directive
- A macro invocation
- A comment
- A file include directive
- The `_start` directive
- A blank line (ignored)

Instructions are **case-insensitive** (`NOP`, `nop`, and `Nop` are equivalent).

### The `_start` Directive

Every program must contain exactly one `_start` directive, which marks the program entry point:

```
_start
SETR A 0x1
```

---

## Registers

The Klauss ISA provides **16 general-purpose 32-bit registers**, named `A` through `P`:

| Register | Encoding | Register | Encoding |
|----------|----------|----------|----------|
| A | 0x0 | I | 0x8 |
| B | 0x1 | J | 0x9 |
| C | 0x2 | K | 0xA |
| D | 0x3 | L | 0xB |
| E | 0x4 | M | 0xC |
| F | 0x5 | N | 0xD |
| G | 0x6 | O | 0xE |
| H | 0x7 | P | 0xF |

Register names are **case-insensitive** (`A` and `a` are equivalent).

---

## Numeric Literals

Immediate values can be specified in two formats:

### Hexadecimal
Prefixed with `0x` or `0X`. Underscores are allowed for readability.

```
SETR A 0xFF
SETR B 0x1_0000
DELAYV 0xFFF
```

### Decimal
No prefix required.

```
SETR A 255
SETR B 12347867
```

All values are 32-bit (range: `0x00000000` to `0xFFFFFFFF` / `0` to `4294967295`). Out-of-range values produce an error.

---

## Labels

Labels mark locations in the program that can be referenced by jump and call instructions. A label is defined by placing a name followed by a colon (`:`) on its own line:

```
MY_LABEL:
    SETR A 0x1
    JMP MY_LABEL:
```

**Rules:**
- Label names are case-sensitive
- Labels are referenced with the colon included: `JMP MY_LABEL:`
- A label definition must be the only meaningful content on its line
- Duplicate label names produce an error

### Labels as Addresses

When a label is used as an argument to an instruction, the assembler substitutes the label's program counter address (as an 8-digit hex value):

```
SETR A 0x1
LOOP:
INCR A
JMP LOOP:        // Jumps back to LOOP address
```

---

## Comments

### Line Comments

Use `//` for single-line comments. Everything after `//` is ignored:

```
SETR A 0xB       // Value to store
// This entire line is a comment
```

### Block Comments

Use `/* ... */` for multi-line comments. Block comments can be nested:

```
/* This is a
   multi-line comment */

/* Outer /* inner comment */ still outer */
```

---

## Data Directives

Data directives allocate memory and optionally initialise it. They start with `#` followed by a name:

### Allocating Uninitialised Words

Specify a numeric count to allocate that many 32-bit words of zero:

```
#DATA1 20         // Allocate 20 words (80 bytes) of zeros
#BUFFER 0x100     // Allocate 256 words
```

### Storing String Data

Specify a string in double quotes to store character data with a length prefix:

```
#GREETING "Hello\n"
#MESSAGE "Test line"
```

- Strings are stored with a 4-byte length prefix followed by the character data
- `\n` is converted to a carriage return + newline (`\r\n`)
- Data is aligned to 4-byte (32-bit word) boundaries

### Referencing Data

Data labels can be used as addresses in instructions:

```
#MY_DATA 20
SETR A #MY_DATA   // Load the address of MY_DATA into register A
```

### Data Placement

Data sections are automatically moved to the end of the program by the assembler, after all executable code. This means you can declare data anywhere in your source file.

---

## File Includes

Use the `!include` directive to include another assembly file:

```
!include string_print.kla
```

- The included file is inserted at the point of the directive
- Recursive includes (file A includes file B which includes file A) are detected and produce an error
- Comments are allowed on the same line: `!include utils.kla // Helper functions`

---

## Macros

Macros are defined alongside the opcodes in the Verilog CPU definition file (`.vh`) inside a block comment, and expand to one or more instructions. Because they live in the same file as the hardware opcode definitions, macros and instructions are maintained together as a single source of truth.

### Defining Macros (in the `.vh` file)

```
/* Macro definition
$PUSHALL PUSH A / PUSH B / PUSH C
$POPALL POP A / POP B / POP C
$WAIT DELAYV %1 / DELAYV %2
*/
```

- Macro names start with `$`
- Instructions are separated by `/`
- Parameters are referenced as `%1`, `%2`, etc.
- Macros can reference other macros (nested expansion)

### Using Macros (in `.kla` files)

```
$PUSHALL              // Expands to: PUSH A, PUSH B, PUSH C
$WAIT 0xFFF 0x100     // Expands to: DELAYV 0xFFF, DELAYV 0x100
```

Arguments are substituted positionally into `%1`, `%2`, etc.

### Nested Macros

Macros can invoke other macros:

```
$IMBED3 $PUSHALL / $IMBED1
```

The assembler resolves nested macros through multiple expansion passes.

---

## Instruction Reference

Instructions follow the general format:

```
OPCODE [register1] [register2] [value1] [value2]
```

The number and type of operands depends on the instruction. In the tables below:
- **Reg** = a register name (A-P)
- **Val** = an immediate value (hex or decimal) or a label/data reference

### Register Operations

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `COPY` | Reg1, Reg2 | Copy Reg1 into Reg2 |
| `SETR` | Reg, Val | Set register to immediate value |
| `INCR` | Reg | Increment register by 1 |
| `DECR` | Reg | Decrement register by 1 |
| `NEGR` | Reg | Two's complement (negate register) |
| `ABSR` | Reg | Absolute value of register |
| `SETFR` | Reg | Set register to current flags value |

**Examples:**
```
COPY A B          // B = A
SETR A 0xFF       // A = 0xFF
INCR A            // A = A + 1
DECR B            // B = B - 1
NEGR C            // C = -C
ABSR D            // D = |D|
```

### Arithmetic

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `ADDRR` | Reg1, Reg2 | Reg1 = Reg1 + Reg2 |
| `MINUSRR` | Reg1, Reg2 | Reg1 = Reg1 - Reg2 |
| `ADDV` | Reg, Val | Reg = Reg + Val |
| `MINUSV` | Reg, Val | Reg = Reg - Val |

**Examples:**
```
ADDRR A B         // A = A + B
MINUSRR C D       // C = C - D
ADDV A 100        // A = A + 100
MINUSV B 0x10     // B = B - 16
```

### Logic and Bitwise

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `AND` | Reg1, Reg2 | Reg1 = Reg1 AND Reg2 |
| `OR` | Reg1, Reg2 | Reg1 = Reg1 OR Reg2 |
| `XOR` | Reg1, Reg2 | Reg1 = Reg1 XOR Reg2 |
| `ANDV` | Reg, Val | Reg = Reg AND Val |
| `ORV` | Reg, Val | Reg = Reg OR Val |
| `XORV` | Reg, Val | Reg = Reg XOR Val |
| `BSWAP` | Reg | Byte-swap (endian conversion) |

**Examples:**
```
AND A B           // A = A & B
OR B D            // B = B | D
XOR C E           // C = C ^ E
ANDV A 0xFF       // A = A & 0xFF  (mask lower byte)
BSWAP A           // Reverse byte order of A
```

### Sign Extension and Type Conversion

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `SEXTB` | Reg | Sign-extend byte (bits 0-7) to 32 bits |
| `SEXTH` | Reg | Sign-extend halfword (bits 0-15) to 32 bits |
| `ZEXTB` | Reg | Zero-extend byte to 32 bits |
| `ZEXTH` | Reg | Zero-extend halfword to 32 bits |

**Examples:**
```
SETR A 0x80            // A = 0x00000080
SEXTB A                // A = 0xFFFFFF80 (sign-extended from byte)
SETR B 0xFF
ZEXTB B                // B = 0x000000FF (zero-extended)
```

### Min/Max Operations

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `MINRR` | Reg | Signed minimum of register and second register |
| `MAXRR` | Reg | Signed maximum of register and second register |
| `MINURR` | Reg | Unsigned minimum |
| `MAXURR` | Reg | Unsigned maximum |

### Shift and Rotate

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `SHLR` | Reg | Logical left shift by 1 |
| `SHRR` | Reg | Logical right shift by 1 |
| `SHLAR` | Reg | Arithmetic left shift by 1 |
| `SHRAR` | Reg | Arithmetic right shift by 1 |
| `SHLV` | Reg, Val | Logical left shift by N bits |
| `SHRV` | Reg, Val | Logical right shift by N bits |
| `SHRAV` | Reg, Val | Arithmetic right shift by N bits |
| `ROLR` | Reg | Rotate left by 1 |
| `RORR` | Reg | Rotate right by 1 |
| `ROLCR` | Reg | Rotate left through carry |
| `RORCR` | Reg | Rotate right through carry |
| `ROLV` | Reg, Val | Rotate left by N bits |
| `RORV` | Reg, Val | Rotate right by N bits |
| `ROLRR` | Reg | Rotate left by amount in second register |
| `RORRR` | Reg | Rotate right by amount in second register |

**Examples:**
```
SHLR A            // A = A << 1
SHRV B 4          // B = B >> 4 (logical)
SHRAV C 0x8       // C = C >>> 8 (arithmetic, sign-preserving)
ROLV D 8          // Rotate D left by 8 bits
```

### Bit Manipulation

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `BSET` | Reg, Val | Set bit N in register |
| `BCLR` | Reg, Val | Clear bit N in register |
| `BTGL` | Reg, Val | Toggle bit N in register |
| `BTST` | Reg, Val | Test bit N, result in zero flag |
| `BSETRR` | Reg | Set bit (bit number in second register) |
| `BCLRRR` | Reg | Clear bit (bit number in second register) |
| `BTGLRR` | Reg | Toggle bit (bit number in second register) |
| `BTSTRR` | Reg | Test bit (bit number in second register) |
| `POPCNT` | Reg | Population count (count set bits) |
| `CLZ` | Reg | Count leading zeros |
| `CTZ` | Reg | Count trailing zeros |
| `BITREV` | Reg | Reverse all bits in register |
| `BEXTR` | Reg, Val | Extract bit field (position:8, length:8) |
| `BDEP` | Reg, Val | Deposit bit field |

**Examples:**
```
BSET A 7          // Set bit 7 of A
BCLR B 0          // Clear bit 0 of B
BTST C 15         // Test bit 15 of C, sets zero flag
POPCNT D          // D = number of 1-bits in D
CLZ A             // A = number of leading zeros in A
```

### Multiply and Divide

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `MULRR` | Reg | Signed multiply, result in first register (low word) |
| `MULURR` | Reg | Unsigned multiply |
| `MULHRR` | Reg | Signed multiply, high word |
| `MULHURR` | Reg | Unsigned multiply, high word |
| `DIVRR` | Reg | Signed divide |
| `DIVURR` | Reg | Unsigned divide |
| `MODRR` | Reg | Signed modulo |
| `MODURR` | Reg | Unsigned modulo |
| `MULV` | Reg, Val | Multiply register by immediate (signed) |
| `DIVV` | Reg, Val | Divide register by immediate (signed) |
| `MODV` | Reg, Val | Modulo register by immediate (signed) |

**Examples:**
```
MULRR A           // A = A * (second register), signed
DIVV B 10         // B = B / 10
MODV C 0x100      // C = C % 256
```

### Comparison

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `CMPRR` | Reg1, Reg2 | Compare registers, sets equal flag |
| `CMPRV` | Reg, Val | Compare register to value, sets equal flag |
| `CMPLTRR` | Reg | Signed less-than |
| `CMPLERR` | Reg | Signed less-or-equal |
| `CMPGTRR` | Reg | Signed greater-than |
| `CMPGERR` | Reg | Signed greater-or-equal |
| `CMPULTRR` | Reg | Unsigned less-than |
| `CMPULERR` | Reg | Unsigned less-or-equal |
| `CMPUGTRR` | Reg | Unsigned greater-than |
| `CMPUGERR` | Reg | Unsigned greater-or-equal |

Comparison instructions set CPU flags (zero, equal, carry, overflow, sign) which are then used by conditional jumps and calls.

**Examples:**
```
CMPRR A B         // Compare A and B, sets equal flag
CMPRV A 0x10      // Compare A to 16, sets equal flag
CMPLTRR A         // Sets flags if A < (second register), signed
```

### Memory Access

#### Direct Memory

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `MEMSETRR` | Reg1, Reg2 | Write Reg1 value to memory at address in Reg2 |
| `MEMREADRR` | Reg1, Reg2 | Read memory at address in Reg2 into Reg1 |
| `MEMSETR` | Reg, Val | Write register value to memory at immediate address |
| `MEMREADR` | Reg, Val | Read memory at immediate address into register |

#### Indexed Memory (2-word instructions)

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `LDIDX` | Reg1, Reg2, Val | Indexed load: Reg1 = mem[Reg2 + Val] |
| `STIDX` | Reg1, Reg2, Val | Indexed store: mem[Reg2 + Val] = Reg1 |
| `LDIDXR` | Reg1, Reg2 | Indexed load: Reg1 = mem[Reg2 + offset register] |
| `STIDXR` | Reg1, Reg2 | Indexed store: mem[Reg2 + offset register] = Reg1 |

`LDIDX` and `STIDX` take an immediate offset value as their third operand. `LDIDXR` and `STIDXR` use a register-based offset.

**Examples:**
```
// Store and load via register addresses
SETR A #DATA1        // A = address of DATA1
SETR B 0x42          // B = value to store
MEMSETRR B A         // mem[A] = B
MEMREADRR C A        // C = mem[A]  (C is now 0x42)

// Direct memory access
MEMSETR B 0x100      // mem[0x100] = B
MEMREADR C 0x200     // C = mem[0x200]

// Indexed memory with immediate offset
SETR A #ARRAY        // A = base address
SETR B 0xAA
STIDX B A 0x0        // mem[A + 0] = 0xAA
STIDX B A 0x2        // mem[A + 2] = 0xAA
LDIDX D A 0x2        // D = mem[A + 2]

// Indexed memory with register offset
SETR B 0xCC
STIDXR B A           // mem[A + offset_reg] = 0xCC
LDIDXR D A           // D = mem[A + offset_reg]
```

### Flow Control (Jumps and Calls)

#### Unconditional

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `JMP` | Label/Val | Unconditional jump |
| `JMPR` | Reg | Jump to address in register |
| `CALL` | Label/Val | Call subroutine (pushes return address) |
| `RET` | *(none)* | Return from subroutine |

#### Conditional Jumps

| Instruction | Condition | Description |
|-------------|-----------|-------------|
| `JMPZ` | Zero flag set | Jump if zero |
| `JMPNZ` | Zero flag clear | Jump if not zero |
| `JMPE` | Equal flag set | Jump if equal |
| `JMPNE` | Equal flag clear | Jump if not equal |
| `JMPC` | Carry flag set | Jump if carry |
| `JMPNC` | Carry flag clear | Jump if not carry |
| `JMPO` | Overflow flag set | Jump if overflow |
| `JMPNO` | Overflow flag clear | Jump if not overflow |
| `JMPS` | Sign flag set | Jump if negative |
| `JMPNS` | Sign flag clear | Jump if positive |
| `JMPLT` | Signed less-than | Jump if less-than |
| `JMPLE` | Signed less-or-equal | Jump if less-or-equal |
| `JMPGT` | Signed greater-than | Jump if greater-than |
| `JMPGE` | Signed greater-or-equal | Jump if greater-or-equal |

#### Conditional Calls

| Instruction | Condition |
|-------------|-----------|
| `CALLZ` | Call if zero |
| `CALLNZ` | Call if not zero |
| `CALLE` | Call if equal |
| `CALLNE` | Call if not equal |
| `CALLC` | Call if carry |
| `CALLNC` | Call if not carry |
| `CALLO` | Call if overflow |
| `CALLNO` | Call if not overflow |

**Examples:**
```
// Simple loop
LOOP:
DECR A
JMPNZ LOOP:       // Loop until A reaches zero

// Subroutine call
CALL MY_FUNCTION:
// ... execution continues here after RET

MY_FUNCTION:
PUSH A
SETR A 0x1
POP A
RET
```

### Stack Operations

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `PUSH` | Reg | Push register value onto stack |
| `POP` | Reg | Pop top of stack into register |
| `PUSHV` | Val | Push immediate value onto stack |

**Examples:**
```
PUSH A            // Save A on stack
PUSH B            // Save B on stack
// ... do work ...
POP B             // Restore B (LIFO order)
POP A             // Restore A
```

### I/O and Peripherals

#### LEDs

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `LEDR` | Reg | Set LEDs from register value |
| `LEDV` | Val | Set LEDs to immediate value |

#### Seven-Segment Display

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `7SEG1R` | Reg | Set 7-segment display 1 from register |
| `7SEG2R` | Reg | Set 7-segment display 2 from register |
| `7SEGR` | Reg | Set both 7-segment displays from register |
| `7SEG1V` | Val | Set 7-segment display 1 to immediate value |
| `7SEG2V` | Val | Set 7-segment display 2 to immediate value |
| `7SEGBLANK` | *(none)* | Blank (clear) both 7-segment displays |

#### RGB LEDs

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `RGB1R` | Reg | Set RGB LED 1 from register |
| `RGB2R` | Reg | Set RGB LED 2 from register |
| `RGB1V` | Val | Set RGB LED 1 to immediate value |
| `RGB2V` | Val | Set RGB LED 2 to immediate value |

#### LCD

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `CDCDMR` | Reg | Send LCD command from register |
| `LCDDATAR` | Reg | Send LCD data from register |
| `LCDCMDV` | Val | Send LCD command (immediate) |
| `LCDDATAV` | Val | Send LCD data (immediate) |
| `LCD` | Val | LCD reset line |

#### Switches

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `SWR` | Reg | Read switch status into register |

**Examples:**
```
LEDV 0xFF         // Turn on all LEDs
7SEG1V 0x1234     // Display 0x1234 on 7-seg 1
SWR A             // Read switches into A
7SEGR A           // Display A on both 7-seg displays
```

### UART / Debug Output

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `TESTMSG` | *(none)* | Send test message via UART |
| `NEWLINE` | *(none)* | Send newline via UART |
| `TXR` | Reg | Send register value (8 hex digits) via UART |
| `TXMEM` | Val | Send 8-byte value at memory address via UART |
| `TXSTRMEM` | Val | Send string at memory address via UART |
| `TXMEMR` | Reg | Send value at memory address (from register) via UART |
| `TXCHARMEMR` | Reg | Send character at memory address (from register) via UART |
| `TXSTRMEMR` | Reg | Send string at memory address (from register) via UART |

**Examples:**
```
SETR A 0x42
TXR A             // Transmit "00000042" via UART
NEWLINE           // Transmit newline

SETR A #MESSAGE
TXSTRMEMR A       // Transmit string stored at MESSAGE address
```

### Interrupts

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `INTSETRR` | Reg1, Reg2 | Set interrupt from registers |

### System

| Instruction | Operands | Description |
|-------------|----------|-------------|
| `NOP` | *(none)* | No operation |
| `HALT` | *(none)* | Halt CPU (freeze) |
| `RESET` | *(none)* | Reset CPU |
| `DELAYV` | Val | Delay by immediate value |
| `DELAYR` | Reg | Delay by register value |

**Examples:**
```
NOP               // Do nothing for one cycle
DELAYV 0xFFFF     // Long delay
HALT              // Stop execution
```

---

## Serial Communication

The assembler can upload compiled programs to the FPGA board via UART serial and interact with the board's output.

### Uploading Programs

Use `-s` to send the compiled binary to the board after assembly:

```sh
klausscc -c opcode_select.vh -i myprogram.kla -s /dev/ttyUSB0
```

If `-s` is given without a port name, the assembler auto-detects the first FTDI USB serial device (VID/PID 0403:6001, 6010, 6011, or 6014):

```sh
klausscc -c opcode_select.vh -i myprogram.kla -s
```

The upload protocol sends an `S` character to reset the board, waits for the board to respond, then transmits the binary byte-by-byte. The board responds with an acknowledgment message (e.g. "Load Complete OK") when the program is loaded.

### Monitoring UART Output

Use `-m` alongside `-s` to monitor the board's UART output after uploading. This is useful for programs that send debug output via `TXR`, `NEWLINE`, `TXSTRMEMR`, etc:

```sh
klausscc -c opcode_select.vh -i myprogram.kla -s -m
```

The monitor prints all received serial data to stdout in real time. Press **Ctrl+C** to stop monitoring -- the serial port is closed cleanly so it can be reused immediately.

### Automated Test Mode

Use `-T` (or `--test`) to automatically verify the board's UART output against expected values declared in the source file's comments:

```sh
klausscc -c opcode_select.vh -i test_bits.kla -s --test
```

The assembler:
1. Parses expected hex values from the `// Expected UART output:` comment block in the source file
2. Assembles and uploads the program
3. Captures UART output and compares each 8-digit hex value against the expected sequence
4. Reports per-line PASS/FAIL and an overall summary

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

Use `--test-timeout` to adjust the wait time (default 10 seconds):

```sh
klausscc -c opcode_select.vh -i test_bits.kla -s --test --test-timeout 20
```

**Exit codes:**
- `0` -- all tests passed
- `1` -- assembly error or no expected values found
- `2` -- one or more test values did not match
- `3` -- timed out before all expected values were received

---

## Writing Test Programs

Test programs follow a standard structure that enables both manual observation and automated verification. The test files in `src/klatest/` demonstrate this pattern.

### Test File Structure

```
// Test NN: Description of what is being tested
// Tests: INSTRUCTION1, INSTRUCTION2, ...
// Expected UART output:
//   XXXXXXXX  (description of first expected value)
//   XXXXXXXX  (description of second expected value)
// Expected 7SEG: 0xNN
// Expected LEDs: 0xFF = all pass

_start
DELAYV 0x3000          // Wait for UART to be ready after upload

7SEG1V 0xNN            // Display test number on 7-segment
NEWLINE                 // Initial newline for clean output

// --- Test case 1 ---
SETR A 0xFF
// ... perform operation ...
TXR A                   // Send result via UART
NEWLINE

// --- Test case 2 ---
// ... more tests ...

LEDV 0xFF               // All LEDs on = all pass
HALT
```

### Key Conventions

1. **Initial delay**: Start with `DELAYV 0x3000` to let the board's upload acknowledgment finish transmitting before your program sends UART output.

2. **Expected UART comments**: Declare expected hex values in a comment block matching this exact format:
   ```
   // Expected UART output:
   //   XXXXXXXX  (description)
   ```
   Each value must be exactly 8 uppercase hex characters. The automated test mode (`--test`) parses these comments and compares them against the actual UART output.

3. **TXR for verification**: Use `TXR` to send register values as 8-digit hex strings over UART. Follow each `TXR` with `NEWLINE` to separate values.

4. **Visual indicators**: Use `7SEG1V` with a unique test number so you can identify which test is running on the board. Use `LEDV 0xFF` at the end to indicate all tests passed.

5. **Data sections**: Declare any data arrays at the bottom of the file with `#NAME count`. The assembler automatically moves data to the end of the program.

### Example Test Program

This test verifies register operations:

```
// Test 01: Register Operations
// Tests: SETR, COPY, INCR, DECR, NEGR
// Expected UART output:
//   000000FF  (SETR A 0xFF)
//   000000FF  (COPY A->B, B should equal A)
//   00000100  (INCR A, 0xFF+1 = 0x100)
//   000000FE  (DECR B, 0xFF-1 = 0xFE)
//   FFFFFFFE  (NEGR C with C=2, twos complement)
// Expected 7SEG: 0x01
// Expected LEDs: 0xFF = all pass

_start
DELAYV 0x3000
7SEG1V 0x01
NEWLINE

SETR A 0xFF
TXR A                  // Expect: 000000FF
NEWLINE

COPY A B
TXR B                  // Expect: 000000FF
NEWLINE

INCR A
TXR A                  // Expect: 00000100
NEWLINE

DECR B
TXR B                  // Expect: 000000FE
NEWLINE

SETR C 0x2
NEGR C
TXR C                  // Expect: FFFFFFFE
NEWLINE

LEDV 0xFF
HALT
```

Run it with automated verification:
```sh
klausscc -c opcode_select.vh -i test_regs.kla -s --test
```

### Available Test Programs

The `src/klatest/` directory contains test programs covering the full ISA:

| File | Description |
|------|-------------|
| `test_regs.kla` | Register operations (SETR, COPY, INCR, DECR, NEGR, ABSR) |
| `test_arithmetic.kla` | Arithmetic (ADDRR, MINUSRR, ADDV, MINUSV) |
| `test_logic.kla` | Logic operations (AND, OR, XOR, ANDV, ORV, XORV) |
| `test_shifts.kla` | Shift and rotate operations |
| `test_bits.kla` | Bit manipulation (BSET, BCLR, BTGL, POPCNT, CLZ, CTZ, BITREV) |
| `test_muldiv.kla` | Multiply and divide |
| `test_compare_jump.kla` | Comparison and conditional jumps |
| `test_memory.kla` | Memory access (MEMSETRR, MEMREADRR, MEMSETR, MEMREADR) |
| `test_stack.kla` | Stack operations (PUSH, POP, PUSHV) |
| `test_call_ret.kla` | Subroutine calls and returns |
| `test_strings.kla` | String output via UART |
| `test_edge_cases.kla` | Boundary conditions and corner cases |
| `test_sign_extend.kla` | Sign/zero extension (SEXTB, SEXTH, ZEXTB, ZEXTH, BSWAP) |
| `test_indexed_mem.kla` | Indexed memory operations (LDIDX, STIDX, LDIDXR, STIDXR) |
| `test_io.kla` | I/O peripherals (LEDs, 7-segment, switches) |
| `test_loop_patterns.kla` | Loop patterns and control flow |

---

## Complete Examples

### Example 1: Counting Loop with Display

This program counts up from 0, displaying the value on the seven-segment display and LEDs:

```
_start

SETR A 0x0        // Initialise counter

LOOP:
INCR A            // Increment counter
7SEGR A           // Show on 7-segment display
LEDR A            // Show on LEDs
DELAYV 0xFFF      // Visible delay
JMP LOOP:         // Repeat forever
```

### Example 2: Memory Read/Write

This program writes a value to memory and reads it back for display:

```
_start

SETR A #DATA2     // A = address of data area
SETR B 0xB        // B = value to store
SETR C 0          // C = for display

START:
INCR B
MEMSETRR B A      // Store B at memory address A
MEMREADRR C A     // Read back from memory address A into C
7SEGR C           // Display the value
DELAYV 0xFFF
LEDV 0xFF
JMP START:

#DATA1 20         // 20 words of storage
#DATA2 20         // 20 words of storage
```

### Example 3: String Printing with Subroutine

A reusable string-printing subroutine and a program that calls it:

**string_print.kla:**
```
// Prints string starting at address in register A
// Register B holds the string length for counting down
STRING_PRINT:
PUSH A
PUSH B
MEMREADRR B A     // Read string length into B
INCR A            // Advance A past the length prefix

STRING_PRINT_LOOP:
TXSTRMEMR A       // Print characters at address A
INCR A            // Next character group
DECR B            // Count down
JMPNZ STRING_PRINT_LOOP:  // Loop until done

POP B             // Restore registers
POP A
RET
```

**main.kla:**
```
!include string_print.kla

_start
7SEG1V 0x1234
SETR A 5
NOP

BEGIN:
INCR A
7SEGR A
TXR A
NEWLINE

// Print a string
PUSH A
SETR A #TEST1
CALL STRING_PRINT:
NEWLINE
POP A

JMP BEGIN:

#TEST1 "Short\n"
#TEST2 "123456789\n"
```

### Example 4: Function Calls with Stack

This program demonstrates calling a function that manipulates values on the stack:

```
_start

BEGIN:
SETR A 12347867
SETR B 0x001A
SETR C 0x1110

7SEG1V 0x4321
DELAYV 0xFFFF

DISPLOOP:
DECR A
JMPZ BEGIN:            // Restart if A reached zero

// Save registers and call function
PUSH A
PUSH B
CALL F_DEC_STACK_TOP:
POP B
POP A

7SEGR A
DELAYV 0xFFFF
JMP DISPLOOP:

// Function: decrement the second value on the stack
F_DEC_STACK_TOP:
POP D                  // Save return address
POP C                  // Get the value
DECR C                 // Decrement it
PUSH C                 // Push it back
PUSH D                 // Restore return address
RET
```

### Example 5: Using Macros

With macros defined in the `.vh` file:

```
/* Macro definition
$PUSHALL PUSH A / PUSH B / PUSH C
$POPALL POP A / POP B / POP C
$WAIT DELAYV %1 / DELAYV %2
*/
```

Use them in your assembly:

```
_start

SETR A 0x1
SETR B 0x2
SETR C 0x3

$PUSHALL              // Expands to: PUSH A, PUSH B, PUSH C

SETR A 0xFF
SETR B 0xFE
SETR C 0xFD

$POPALL               // Expands to: POP A, POP B, POP C
                      // A=1, B=2, C=3 again

$WAIT 0xFFFF 0x1000  // Expands to: DELAYV 0xFFFF, DELAYV 0x1000

HALT
```

---

## Output Files

When assembling, klausscc produces:

| File | Description |
|------|-------------|
| `<name>.code` | Human-readable listing with addresses, opcodes, and source lines |
| `<name>.kbt` | Binary output file for uploading to the FPGA board |

When using `--opcodes`:

| File | Description |
|------|-------------|
| `<name>.html` | ISA documentation with opcode and macro tables |
| `<name>_opcodes.json` | Machine-readable opcode list |
| `<name>_macros.json` | Machine-readable macro list |

When using `--textmate`:

| File | Description |
|------|-------------|
| `<name>_textmate.txt` | Pipe-delimited opcode names for TextMate/VSCode syntax grammar |

---

## Assembly Process

The assembler processes source files in multiple passes:

1. **Preprocessing** -- Block comments are removed and `!include` directives are expanded
2. **Pass 0** -- Macros are expanded (nested macros resolved over multiple sub-passes)
3. **Pass 1** -- Line types are classified, labels are extracted, and program counter addresses are calculated. Data directives are moved to the end of the program.
4. **Pass 2** -- Opcodes are generated, register and immediate arguments are encoded, and label references are resolved to addresses. The final `.code` and `.kbt` files are written.

---

## CPU Flags

Several instructions set CPU flags that are used by conditional jumps and calls:

| Flag | Set By |
|------|--------|
| **Zero** | Arithmetic/logic results equal to zero, `DECR`, `INCR`, `BTST` |
| **Equal** | `CMPRR`, `CMPRV` when operands are equal |
| **Carry** | Arithmetic overflow (unsigned), shift-out bits |
| **Overflow** | Signed arithmetic overflow |
| **Sign** | Result is negative (MSB set) |

---

*This document was generated from the Klauss ISA opcode definitions and the klausscc assembler source code.*
