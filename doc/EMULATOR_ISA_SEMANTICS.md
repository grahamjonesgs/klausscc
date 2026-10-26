# KlaussCPU emulator — semantics and differences from the RTL

`klausscc --emulate` runs a program on a built-in ISA emulator, the golden
model for the CPU. It decodes each instruction word straight from its bit
fields rather than from the assembler's opcode table, so it is a second,
independent implementation of the ISA.

The reference it is checked against is the board core,
`KlaussCPU.srcs/sources_1/new/pipeline_core.sv` on `master` of the KlaussCPU
RTL repo: the 5-stage pipeline that executes every instruction. The older
multicycle core is no longer on `master`. The **RTL is the authority**: where the emulator and the RTL disagree, the
emulator is the one to fix, after checking.

The encodings are in [ISA_ENCODING_V2.md](ISA_ENCODING_V2.md), and the
flag model (`Z S C V`, who writes what, branch conditions) is in its §7.
This file records how the emulator executes them, the corner cases that
are easy to get wrong, and every known place where it differs from the
RTL.

## Where the code is

| File | What it holds |
|---|---|
| `src/emu/mod.rs` | `Cpu` state, memory helpers, the run loop, `emulate_image` |
| `src/emu/exec.rs` | decode and execute, one handler per CLASS; the v3 short-form immediates |
| `src/emu/flags.rs` | the `Z S C V` register and its saved layouts (GETF, interrupt frame, trace, debugger nibble) |
| `src/emu/mmio.rs` | UART, interrupt controller / timer, performance counters |
| `src/emu/debug.rs` | the debugger's view: registers, memory, watchpoints, stop reasons |
| `src/emu/trace.rs` | the per-instruction trace line |
| `src/emu/tests.rs` | unit tests, including the flag model |
| `src/layout.rs` | memory map: heap header, `CODE_BASE` = 0x20, the MMIO window `0xF000_0000`+ |
| `src/isa/table.rs` | the assembler's opcode table (not used by the emulator's decoder) |

## Behaviour that matches the RTL

These are the cases most worth knowing. Each was checked against
`pipeline_core.sv`.

### Flags

- One 4-bit register `Z S C V`; `E`, `L`, `U` are derived on read
  (`E = Z`, `L = S ^ V`, `U = C`). Flags are sticky.
- `CMPRR`/`CMPRV` (and `CMPRRW`/`CMPRVW`/`CMPRV.S`) are a SUB without
  writeback and set all of `Z S C V`.
- Carry uses the **borrow** convention: after SUB/CMP, `C = 1` exactly
  when `a < b` unsigned. `SUBC` computes `a - b - C`.
- Branch conditions are derived as in `ISA_ENCODING_V2.md` §7.3:
  `LT = S^V`, `LE = Z|(S^V)`, `ULT = C`, `ULE = C|Z`, and `E` is the same as
  `Z`.
- ADD/SUB family (including INCR/DECR and the W and `.S` forms) write
  `Z S C V`. The W forms take Z from the sign-extended 32-bit result and
  S/C/V from the 64-bit operation.
- Shifts and rotates write Z only when their F bit is set. Only an
  embedded rotate by N=1 with F (ROLR1/RORR1, `ROLV r 1`, `RORV r 1`) and
  ROLCR/RORCR write C.
- SEXTB/SEXTH write Z and S. NEGR, NOTR, ZEXTB/ZEXTH and POPCNT write Z.
  BTST (embedded N) writes `Z = NOT bit`.
- AND/OR/XOR **immediate** forms, MIN/MAX, MOV/LEA, the boolean compares,
  the fused branches, loads, stores and all control flow write no flags.

### Arithmetic

- **Divide by zero does not trap.** DIV gives `0xFFFF_FFFF_FFFF_FFFF`,
  MOD (register forms) gives the dividend, V is set and **Z is left as it
  was**.
- Signed division truncates toward zero. `INT_MIN / -1` gives INT_MIN with
  no trap; `INT_MIN % -1` gives 0.
- MULV/DIVV/MODV take a **signed** immediate (their SGN bit is set).
- `CMPRV` sign-extends its immediate before comparing, so a negative
  immediate is a large unsigned operand for the unsigned conditions.
- Shift and rotate counts from a register use `rs2[5:0]`, so a count of 64
  shifts by 0.
- `CLZ(0) = CTZ(0) = 64`. BSWAP and BITREV work on all 64 bits.

### BEXTR and BDEP

Both take `start = imm[4:0]` and `len = imm[12:8]`; a field is 0–31 bits
and `len = 0` is empty.

- `BEXTR` shifts the **full 64-bit rs1** right by `start` and keeps `len`
  bits, so a field with `start + len > 32` takes bits from `rs1[63:32]`.
  The result is zero-extended, and **Z is always written** from it,
  although F is clear in the template. (The emulator read only `rs1[31:0]`
  until commit 9800f3a.)
- `BDEP` deposits the low `len` bits of rs2 into the **low 32 bits** of
  rs1 at `start`, drops field bits above bit 31, and zero-extends the
  32-bit result. No flags.

### Memory

- Little-endian throughout: byte lane `n` of a doubleword is bits
  `[8n+7:8n]`.
- `MEMGET32` reads the 4 bytes at **any** address, including across a
  doubleword boundary. `LDIDX32`, `LDIDX32_S` and the other 32-bit loads
  align down to 4, 16-bit loads to 2. Sub-word stores align down to their
  size.
- 64-bit accesses with A=1 (`MEMGET64`, `LDIDX64A`, `MEMSET64`,
  `STIDX64A`) align down to 8. The other register, absolute and reg+reg
  64-bit forms (`MEMREADRR`, `MEMREADR`, `LDIDX64R`, `MEMSET64RR`,
  `MEMSETR`, `STIDX64R`) also access the aligned doubleword, which is what
  the RTL's data cache does with them. For `LDIDX64`/`STIDX64` see
  difference 6 below.
- Loads and stores to `addr[31:28] = 0xF` go to MMIO.

### Control flow, stack and system

- A call pushes the zero-extended return address (the next instruction:
  PC+8 for 2-word calls, PC+4 for CALLR and CALL.S), and only when taken.
  RET and IRET take the PC from bits `[31:0]` of the popped doubleword.
- The 2-word PC-relative jumps/calls (REL=1, LEN=10), which have no
  klausscc mnemonic, are executed: target = PC + imm32.
- v3 forms: the short immediates, short branches, fused compare-and-branch
  (no flags), W ops, `LDIDX32_S`, `ENTER`/`LEAVE`/`LEAVERET`.
- `RESET` jumps to PC = 0x4. In a klausscc image that address holds the
  upper half of the `heap_start` header word.
- SP starts at `0x0800_0000`, the top of the 128 MiB DDR (the RTL's
  `SP_RESET` for core 1). Execution starts at the entry address the
  emulator is given (`CODE_BASE` = 0x20 for an assembled program).

### GETF and interrupts

- `SETFR rd` (GETF): `rd = {Z, E, C, V}` in bits `[63:60]`, with E = Z.
- Interrupt entry pushes one doubleword: `[31:0]` = the PC to resume at,
  `[38:32]` = `{Z, E, C, V, S, L, U}` (bit 38 = Z … bit 32 = U, the last
  three derived), `[42:39]` = `INT_MASK`, `[63:43]` = 0. It then clears
  that source's mask bit and jumps to its vector. `IRET` restores PC,
  `INT_MASK` and **only Z, S, C, V** (bits 38, 34, 36, 35).
- The interrupt controller is at `0xF00F_0000` (`INT_MASK`,
  `INT_PENDING`, `INT_VEC0..3`, `TIMER_PERIOD`, `TIMER_COUNT`, the
  millisecond clock), with source 0 the periodic timer. `WAIT` sleeps
  until a source is ready.

## Differences from the RTL

Each item below was found by reading both `src/emu/` and
`pipeline_core.sv`. Items 1–6 can affect programs the assembler or LLVM
produces; items 7–9 need hand-encoded words.

1. **ANDR / ORR / XORR set Z in the emulator. The RTL writes no flags for
   them.** A branch on Z after a register AND/OR/XOR therefore tests the
   AND result in the emulator but an older flag value on the board.
   (`exec_alu_rr`: `with_z(a & b)`; RTL class 1 AND/OR/XOR decode has an
   empty flag mask.)
2. **MUL family: the RTL writes Z, S and V; the emulator writes no
   flags.** In the RTL, S is bit 63 of the 64-bit result (the high half
   for MULH*; for MULW, the low product before sign-extension). V is set when a
   low-half product does not fit in 64 bits (signed or unsigned per SGN)
   and cleared by the high-half forms. MULW's Z follows its W result.
3. **DIV / MOD with a non-zero divisor: the RTL writes Z from the result
   and clears V; the emulator leaves both unchanged.** The divide-by-zero
   case matches (V set, Z kept).
4. **ABSR of INT_MIN: the RTL sets V = 1; the emulator clears V.** For
   every other input both clear V. The result (INT_MIN) and Z match.
5. **ROLR / RORR with a register count of 1: the emulator also writes C
   (the bit rotated out); the RTL writes only Z.** In the RTL only the
   embedded N=1 form writes C. (`exec_shift_bit` tests `cnt == 1`
   whatever SRC is.)
6. **Unaligned 64-bit accesses with A=0 (`LDIDX64`, `STIDX64`,
   `LDIDX64.S`, `STIDX64.S`): the emulator reads or writes the 8 bytes at
   the exact address.** The RTL sends the unaligned address to the data
   cache, which picks the doubleword by address bits `[4:3]`, so the access
   goes to the aligned doubleword. Aligned addresses, the normal case,
   agree.
7. **2-word MOD by zero with rd ≠ rs1: the RTL writes the dividend to rd;
   the emulator writes nothing.** The assembler's `MODV` is in place
   (rd = rs1), so the visible result is the same.
8. **The F bit is ignored by the emulator on COPY, BSWAP, BITREV, CLZ, CTZ,
   GETF, BSET/BCLR/BTGL (both forms) and BTSTRR.** The RTL writes Z for
   them when F = 1. The assembler's templates have F = 0. The emulator's
   ROLCR/RORCR rotate one bit whatever N and F say; the RTL accepts them
   only with N = 1, F = 1.
9. **The emulator traps on fewer encodings than the RTL.** It decodes only
   the fields it needs, so many reserved encodings that the RTL traps
   execute in the emulator. Examples:
   - non-zero unused register fields;
   - ADD/SUB/ADC/SBC with F = 0, AND/OR/XOR with F = 1, SGN = 1 in class 1;
   - a flag CMP with PRED, INV or rd non-zero;
   - SEXT/ZEXT with SIZE = 11 (run as 32-bit), or SIZE ≠ 0 on other unary
     ops;
   - a shift with SRC = 0 and N ≠ 0;
   - BEXTR with rs2 ≠ 0;
   - `MEMGET32` with SGN = 1, a store with SGN = 1, A = 1 on a sub-word
     access;
   - COND 0 with INV (the emulator runs it as never taken);
   - any class-C (LCD) word, which the emulator runs as a no-op.

   The emulator does trap on LEN = 00, classes 0/E/F, class D with
   LEN ≠ 01, unassigned OP values, PRED 5–7 and COND 10–15.

### Not modelled

- **Timing.** Each instruction retires at once, with no pipeline, cache
  or memory latency. Time advances by `CYCLES_PER_INSTRUCTION` (2) cycles
  per instruction for the timer, millisecond clock and cycle counter, so
  interrupt timing differs from the board. Architectural effects match.
- `DELAYR`/`DELAYV` do not wait (the RTL spins for value × 8192 cycles),
  and LCD instructions do nothing.
- Interrupt sources 1–2 (blitter, Ethernet) are not modelled; only the
  timer (source 0) fires.
- MMIO is the UART (`0xF001_0000`: `TX_DATA`, `RX_DATA`, `STATUS`), the
  interrupt controller / timer (`0xF00F_0000`) and the performance
  counters (`0xF00D_0000`). Other MMIO reads return 0 and writes are
  ignored.
- Memory is 128 MiB. Reads past the end return 0 and writes past it are
  dropped.

### Flag unification: done

The cross-repo flag-unification spec (the RTL repo's
`FLAG_UNIFICATION_CHANGES.md`) asked klausscc for three things: CMP sets `Z S C V`, conditions are
derived with `ULT = C`, and `E/L/U` are derived on read in GETF, the
interrupt frame and the trace. All three are implemented
(`src/emu/flags.rs`, `src/emu/exec.rs`) and covered by unit tests. The
assembler needed no change: the encodings are the same and `JMPE`/`JMPNE`
still assemble.

## Trace format (shared with the RTL self-trace)

`--emulate --trace` writes one line per **retired** instruction, showing
the architectural state **after** the instruction. It matches the
RTL testbench's line, which `pipeline_core.sv` presents at its single
retire point (WB) through the `ret_*` outputs:

```
i=<n> pc=<8hex> op=<8hex> r0=<16hex> ... r15=<16hex> sp=<8hex> f=<7 bits> [wr=<addr8>/<be2>/<data16>]
```

- `op` is word 0 of the instruction.
- `f` is a fixed 7-character bit string `{Z, S, C, V, E, L, U}`. The last
  three are derived (`E = Z`, `L = S ^ V`, `U = C`).
- `wr` appears only when the instruction wrote memory or MMIO. For a
  sub-word store, `addr` is the doubleword address, `be` the byte enables
  within it and `data` the whole doubleword after the write. For a 64-bit
  store, `addr` is the store address, `be` is `ff` and `data` the stored
  value. For MMIO, `data` is the value written.
