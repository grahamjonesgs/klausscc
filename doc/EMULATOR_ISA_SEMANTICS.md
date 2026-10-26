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
  ROLCR/RORCR write C; a register count of 1 does not.
- With F set, every class-5 op writes Z from its result: COPY, NEG, NOT,
  ZEXT, BSWAP, BITREV, POPCNT, CLZ, CTZ and GETF (Z = the 64-bit result
  is 0, so GETF gives Z = NOT (Z | C | V)); SEXT also writes S. So do
  BSET/BCLR/BTGL and BTSTRR in class 4. BTST (embedded N) writes
  `Z = NOT bit`. Among these, the assembler's templates set F only on
  NEGR, NOTR, ABSR, SEXTB/SEXTH, ZEXTB/ZEXTH, POPCNT and INCR/DECR.
- ABSR writes Z and V, with V = 1 only for `0x8000_0000_0000_0000`.
- MUL writes Z, S and V: S is bit 63 of the half written (for MULW, of
  the low half before sign-extension), V is set when the low half does
  not hold the whole product (signed or unsigned per SGN) and cleared by
  MULHR/MULHUR, and Z follows the value written (MULW: the W result).
- DIV and MOD write Z from the result and clear V.
- AND/OR/XOR (register **and** immediate forms), MIN/MAX, MOV/LEA, the
  boolean compares, the fused branches, loads, stores and all control
  flow write no flags.

### Arithmetic

- **Divide by zero does not trap.** DIV gives `0xFFFF_FFFF_FFFF_FFFF`,
  MOD (every form, MODV included) gives the dividend, V is set and **Z is
  left as it was**.
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
  `STIDX64A`) align the address down to 8. The other 64-bit forms issue it
  unaligned, as do PUSH, POP, CALL, RET, IRET, ENTER and LEAVE with an
  unaligned SP or R15, but the data cache selects the doubleword by
  `addr[4:3]` and ignores `addr[2:0]` (`mem_read_write.sv`), so every
  64-bit RAM access reaches the aligned doubleword that holds the
  address. The emulator does the same in `read64`/`write64`. An MMIO
  access keeps the address as issued, and the trace's `wr` shows the
  issued address of a 64-bit store, as the RTL's `ret_wr_addr` does.
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

### Legality

The emulator traps (`InvalidOpcode`) exactly the words the RTL decoder
(`f_decode_raw` in `pipeline_core.sv`) marks illegal: reserved bits and
unused register fields that are not zero, F/SGN/W combinations a class
does not accept, SEXT/ZEXT with SIZE 11, a shift with SRC=0 and N≠0,
ROLCR/RORCR other than as SRC=1 N=1 F=1, BEXTR with rs2≠0, a flag CMP
with PRED, INV or rd non-zero, COND 0 with INV, `MEMGET32` with SGN, a
store with SGN, A on a sub-word access, and LCD words with reserved
fields. Unit tests check every opcode in the assembler's table with
several operand values, so this can never trap assembled code.

An illegal word stops the machine **without retiring**: like the RTL, it
gets no trace line and is not counted (`PERF_INSTR` counts retirements
only). HALT and TRAP do retire.

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

No difference in the architectural effect of an instruction is known.
The nine differences this section used to list (logic and MUL/DIV flags,
ABSR's V, ROLR/RORR's C, the F bit on the unary and bit ops, unaligned
64-bit accesses, the 2-word MOD by zero, and the encodings the emulator
did not trap) were fixed against `pipeline_core.sv` on KlaussCPU
`master`, each with a unit test that cites the RTL lines.

What is left is what the emulator does not model (next section), and
one corner that was not checked: a 64-bit or sub-word **MMIO** access at
an address that is not a multiple of 8. The emulator decodes the
register from the exact address and returns the value in the low bits.
On the board the device decoder sees the same address, a sub-word load
takes its byte lanes from the reply by `addr[2:0]` (a 64-bit load takes
the reply whole), and what a device returns
for an address that is not a multiple of 8 depends on its decoder. The
emulator's devices only have registers at multiples of 8, so such an
access reads 0 or is ignored.

### Not modelled

- **Timing.** Each instruction retires at once, with no pipeline, cache
  or memory latency. Time advances by `CYCLES_PER_INSTRUCTION` (2) cycles
  per instruction for the timer, millisecond clock and cycle counter, so
  interrupt timing differs from the board. Architectural effects match.
- `DELAYR`/`DELAYV` do not wait, but advance modelled time by the
  RTL's spin, value × 8192 cycles (value = rs1[31:0] or imm32). LCD
  instructions do nothing once their encoding has been checked.
- Interrupt sources 1–2 (blitter, Ethernet) are not modelled; only the
  timer (source 0) fires.
- MMIO is the UART (`0xF001_0000`: `TX_DATA`, `RX_DATA`, `STATUS`), the
  interrupt controller / timer (`0xF00F_0000`) and the performance
  counters (`0xF00D_0000`). Other MMIO reads return 0 and writes are
  ignored.
- The UART transmitter is timed on the modelled clock, not on the
  board's. A `TX_DATA` store sets `STATUS.TX_BUSY` for
  `UART_CYCLES_PER_STORE` = 334 cycles: a frame of
  `UART_CYCLES_PER_BYTE` = 332 (10 bits of `CLKS_PER_BIT` = 33,
  `uart_tx.sv` :7, 3 Mbaud at the 100 MHz `ui_clk`, plus `uart_tx`'s
  `s_IDLE` and `s_CLEANUP` cycles) and `uart_send_msg`'s own `s_IDLE`
  and `s_CLEANUP` cycles; on the RTL a second store is taken 334 cycles
  after the first at the earliest. There is no TX FIFO: a store while
  busy is ignored by `uart_send_msg` (:47-76), so the emulator drops it
  too and records its PC and byte (`EmulateResult::tx_busy_writes`).
  `--emulate` lists them; `--emulate-test` fails the file. The busy
  window is the RTL's in cycles, but polling loops spin a different
  number of times, because instructions take 2 modelled cycles each.
- A program starts with TX idle (`--emulate`, `--emulate-test` and
  `--gdbserver` alike), as a program loaded over the UART (or by
  netboot) does since klaussCPU 04db80e (`KlaussCPU.sv` `START_WAIT`).
  `LOAD_COMPLETE` clears `r_start_wait_counter` and
  `r_start_wait_ack` and starts the `"Load Complete OK\n\r"` reply;
  `START_WAIT` waits for `uart_send_msg` to take it (`w_sending_msg` =
  1, at most 16 cycles), then for `w_sending_msg` = 0. Edge by edge: E0
  the last byte's stop bit finishes (`uart_tx.sv` :90-96, `r_Tx_Done` =
  1, `s_CLEANUP`); E1 `uart_send_msg` sees `w_Tx_Done` and goes
  `s_TX_MSG` → `s_CLEANUP` (`o_sending_msg` still 1); E2 `s_CLEANUP`
  clears `o_sending_msg` and goes to `s_IDLE`; E3 `START_WAIT` samples
  `w_sending_msg` = 0 and sets `r_pip_start` and `PIPE_RUN`. So
  `STATUS.TX_BUSY` (`w_sending_msg`, `KlaussCPU.sv` :911) reads 0 at
  the program's first instruction and its first `TX_DATA` store is
  taken. Every store must still poll `TX_BUSY`, because back-to-back
  stores are dropped. On bitstreams built before that change the
  pipeline starts while the 18-byte reply is still going out (about
  6000 cycles, 60 µs), so the first store can be dropped too; the
  emulator does not model that.
- The UART RX FIFO starts empty, so `STATUS.RX_EMPTY` reads 1 until
  `--gdbserver`'s stdin feeds it. That matches the board: since
  klaussCPU 04db80e the RTL empties the RX FIFO at `LOAD_COMPLETE`
  (`uart_rx_fifo1` `.i_Reset = w_reset_H | (st.SM == LOAD_COMPLETE)`),
  so bytes sent before the load ended never reach the program; a byte
  that arrives during `START_WAIT` is kept.
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
