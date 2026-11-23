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
| `src/emu/mmio.rs` | UART, interrupt controller / timer, performance counters, `CPU_FEATURES` |
| `src/emu/system.rs` | MMU stage 1: user mode, exception entry, the system registers, the LR/SC reservation |
| `src/emu/sd/` | the SD controller at `0xF000_0000` (SPI mode, and the native controller with `--sd-native` and its DMA engine with `--sd-dma`) and the emulated card (`--sd-image`) |
| `src/emu/board_io.rs` | the RGB LEDs (`0xF002_0000`), the 7-segment display (`0xF003_0000`), the LED bar and switches (`0xF004_0000`); `--switches`, `--board-io-log` |
| `src/emu/coherence.rs` | the `--sd-dma` DMA coherence checker |
| `src/emu/mmu.rs` | MMU stages 2 and 3: the MPU, the TLB, the address map, and `translate`, which every memory access an instruction makes goes through |
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
  doubleword boundary. Every other 16-bit load or store at an odd
  address, and 32-bit one not at a multiple of 4, in any MODE, stops the
  machine with `Misaligned(EA)` before it happens (klaussCPU hw-proposals
  8: `ex_misal` in `pipeline_core.sv`, park kind 5, crash dump `ERR=0A`):
  no memory changes, the instruction does not retire (no trace line, not
  in `PERF_INSTR`) and the PC stays on it. Byte, 64-bit and stack accesses
  never stop. (Cores before 3508f87 aligned the address down instead.)
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
- The 2-word PC-relative jumps/calls (REL=1, LEN=10; `JMPREL` … `CALLREL`)
  are executed with any COND: target = PC + imm32.
- v3 forms: the short immediates, short branches, fused compare-and-branch
  (no flags), W ops, `LDIDX32_S`, `ENTER`/`LEAVE`/`LEAVERET`.
- hw-proposals forms (klaussCPU 3508f87, `ISA_ENCODING_V2.md` §3.15): the
  fused branch's PRED 5..7 compare `{x[31:0], 0}` (the low halves, signed
  or unsigned, as `CMPRRW`), its 2-word form compares with sext(imm32)
  from word 1 and falls through to PC + 8; `CMPRVW.S` is the short CMP with
  INV set, a 32-bit compare; `MULV.S`/`MULUV.S` multiply by the imm8
  extended by SGN `[23]`, signed or unsigned as `MULV`/`MULUV`; `LEAPC.S`
  adds sext({`[19:12]`, `[7:0]`}) to the PC. The RTL also decodes a
  `LEAPC.S` word with SGN `[20]` set when rs2 `[3:0]` is zero (its generic
  short ALU path), and so does the emulator.
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
only). HALT and TRAP do retire. With MMU stage 1 an illegal word raises
ILLEGAL, which stops the machine like this while `EVEC` is 0 (always,
unless the program sets it; see [MMU stage 1](#mmu-stage-1)).

### GETF and interrupts

- `SETFR rd` (GETF): `rd = {Z, E, C, V}` in bits `[63:60]`, with E = Z.
- Interrupt entry pushes one doubleword: `[31:0]` = the PC to resume at,
  `[38:32]` = `{Z, E, C, V, S, L, U}` (bit 38 = Z … bit 32 = U, the last
  three derived), `[42:39]` = `INT_MASK`, `[43]` = the `DI` bit (1 =
  disabled), `[44]` = PM, the mode the entry came from (1 = user; MMU
  stage 1), `[63:45]` = 0. It then clears that source's mask bit and
  jumps to its vector. `IRET` restores PC, `INT_MASK`, the `DI` bit, the
  mode and **only Z, S, C, V** (bits 38, 34, 36, 35).
- The interrupt controller is at `0xF00F_0000` (`INT_MASK`,
  `INT_PENDING`, `INT_VEC0..3`, `TIMER_PERIOD`, `TIMER_COUNT`, the
  millisecond clock), with source 0 the periodic timer. `WAIT` sleeps
  until a source is ready. With `--sd-native`, source 2 is the cascade
  of the native SD controller (see "SD card").
- `DI rd` sets rd to the old enable bit (1 = enabled, as at reset) and
  disables; `EI rs1` sets enable = rs1[0]. No interrupt is taken while
  disabled (`irq_take`), but a ready source still wakes a `WAIT`, and the
  instruction after it runs without entering the handler (`irq_ok`, WFI).

## MMU stage 1

The emulator implements stage 1 of the klaussCPU MMU plan
(`MMU_PLAN.md` §3-§6 at klaussCPU 507990d, branch `mmu`; encodings in
[ISA_ENCODING_V2.md](ISA_ENCODING_V2.md) §3.16). It follows the spec,
including its clarifications of 507990d (§10 item 10: the register 21
bit order, MISALIGNED_FETCH and the debugger, legality before privilege,
cause priority and `EADDR`, and parking), which the hardware designer
checked against the stage 1 RTL. The emulator itself has not yet been
compared with that RTL.

**Compatibility.** At reset (every run and every `--gdbserver` session
starts from reset, which is also the program load) the CPU is in
supervisor mode with `EVEC` = 0, and nothing changes for code that does
not use the new features: an illegal word, TRAP and a misaligned access
stop the machine as before, and so do the new causes, as an illegal
instruction (`InvalidOpcode` with the instruction word: the board parks
with `park_kind` 2, as an old bitstream does for these encodings).

**State.** `UM` (user mode), `SP_ALT` (the inactive stack pointer: the
user SP in supervisor mode, the kernel SP in user mode), and the system
registers `MFSR`/`MTSR` reach: 0 `STATUS` (`[0]` UM, `[1]` DI, both
read-only; `[8]` MPU_EN and `[9]` TE from stages 2 and 3), 1 `EVEC`
(`[1:0]` read 0), 2 `ECAUSE` (the cause in `[7:0]`, GUARD in `[8]`,
and the user debug bits `[12:9]` below; `MTSR` writes `[12:0]`, `[8:0]`
with `--no-ustep`), 3
`EADDR`, 4 `USP` (= `SP_ALT`), 5/6 `SCRATCH0/1`, 7 `TP`, 8 `CYCLE`
(read-only), the stage 2/3 registers 0x10-0x2F, 0x30-0x34 and the
TLB walker's 0x35-0x37 (next section), and the user watchpoints'
0x40-0x43 (below). Any other SR number, and `MTSR` to `CYCLE`, is
ILLEGAL.
`CPU_FEATURES` at `0xF00F_0048` reads `0x200D`: stage 1, the TLB with
32 entries, LR/SC, and no MPU, as on the Nexys A7 bitstream since
klaussCPU `tlb32`. `--tlb-entries 16` (`Cpu::set_tlb_entries`) models the
16-entry bitstreams before it (`0x100D`). With `--mpu`
(`Cpu::set_has_mpu`) the emulator has the stage 2 MPU too and reads
`0x20FF` (bit 1, and `[7:4]` = 15 for 16 regions). Bits 22-23 (DCBZ,
MAL64) are set, and bit 25, the hardware TLB walker, as on the release
bitstream; `--no-tlb-walker` (`Cpu::set_has_tlb_walker`) clears it and
makes SRs 0x35-0x37 ILLEGAL. Bits 28-29, the user single step and the
watchpoints, are set as on the release bitstream (klaussCPU
`ustep_release_60bcc36`); `--no-ustep` (`Cpu::set_has_udbg`) clears them,
makes SRs 0x40-0x43 ILLEGAL, `ECAUSE` 9 bits, and frame bit 45 ignored
(`0x02C0_200D` in all, against `0x32C0_200D`). Bit 30, the 7-segment
display's `SEG_RAW` / `SEG_MODE` (klaussCPU branch `seg7raw`, "Board
I/O" below), is set; `--no-seg7raw` (`Cpu::set_has_seg_raw`) clears it.
The default reads `0x72C0_200D`.

**CYCLE** is the emulator's modelled cycle count since reset: 2 cycles
per retired instruction (`CYCLES_PER_INSTRUCTION`), 2 per exception
entry, plus each `DELAY`'s wait. It is the count `PERF_CYCLES` shows
before any `PERF_CTRL` clear; a clear does not touch `CYCLE`. It is not
the board's count (see [Not modelled](#not-modelled)).

**Exceptions are precise.** Every check that can fault is made before
the instruction's first side effect, so a faulting instruction changes no
register, flag, SP, memory or device, and retires nothing (no trace
line, not in `PERF_INSTR`). The causes and their `EADDR`:

| ECAUSE | Cause | Raised by | Frame PC | `EADDR` |
|---|---|---|---|---|
| 1 | ILLEGAL | a reserved encoding, an SR that does not exist, `MTSR CYCLE` | the instruction | its word 0 |
| 2 | PRIVILEGED | in user mode: `DI`, `EI`, `IRET`, `HALT`, `WAIT`, `RESET`, `DELAYR`/`DELAYV`, the LCD instructions, `MFSR` of any SR but `STATUS`/`TP`/`CYCLE`, `MTSR` of any SR but `TP` | the instruction | its word 0 |
| 3 | TRAP | `TRAP` | the instruction | its word 0 |
| 4 | SYSCALL | `SYSCALL` | **the next instruction** | 0 |
| 5 / 6 | MISALIGNED_LOAD / _STORE | a misaligned 16/32-bit access (as before); an LR/SC not naturally aligned (64-bit too) | the instruction | the effective address |
| 7 / 8 | ACCESS_LOAD / _STORE | in user mode, any data or stack access touching `0xF000_0000`+; LR/SC to MMIO in either mode; an MPU denial or an address-map or TLB permission fault (stages 2/3) | the instruction | the effective address ¹ |
| 9 | ACCESS_FETCH | in user mode, an instruction with a word in MMIO; the same stage 2/3 faults for a fetch | the instruction | that word's address |
| 10 | MISALIGNED_FETCH | in user mode, a PC with `[1:0]` ≠ 0 | the PC | the PC |
| 11 / 12 / 13 | TLB_MISS_LOAD / _STORE / _FETCH | stage 3: a useg or kmap access no TLB entry matches | the instruction | the VA ¹ |

¹ The effective address as computed (base + offset), before any
alignment: an A=1 64-bit access and a sub-word store report the raw
address. A two-access instruction (LEAVERET's `[R15]` and `[R15+8]`, a
`MEMGET32` that spans two doublewords) whose first doubleword is RAM and
second MMIO reports the second doubleword's start, `0xF000_0000`.

PUSH, PUSHV, PUSHV64, CALL (taken) and ENTER are stores; POP, RET,
LEAVE and LEAVERET loads. LEAVERET's two loads and a spanning `MEMGET32`
are both checked before either happens. When several causes apply, the
first of MISALIGNED_FETCH, ACCESS_FETCH, ILLEGAL, PRIVILEGED,
TRAP/SYSCALL, MISALIGNED_LOAD/STORE, ACCESS_LOAD/STORE is raised (§4.1):
a word fetched from MMIO in user mode is ACCESS_FETCH whatever it holds,
an illegal encoding is ILLEGAL in either mode (legality is checked before
privilege, so `MTSR CYCLE` and SR numbers with no SR are ILLEGAL in user
mode too), and a misaligned user access to MMIO, or LR/SC, is
MISALIGNED. In supervisor mode a PC in MMIO is still `PcOutOfRange`.

**What happens to an exception** (`MMU_PLAN.md` §4.4), in order:

1. With a debugger attached (`--gdbserver`) and `EXC_PASS` off (the
   default; `monitor exc_pass on`), ILLEGAL, TRAP and MISALIGNED_LOAD/
   STORE stop in the debugger as before, whatever `EVEC` holds.
   MISALIGNED_FETCH does not: it goes on to step 2 or 3 like ACCESS_*.
2. With `EVEC` = 0 the machine stops as before stage 1: `Trap`,
   `Misaligned(EADDR)`, or `InvalidOpcode(word)` for ILLEGAL and every new
   cause (MISALIGNED_FETCH included). The PC stays on the instruction,
   and `ECAUSE`/`EADDR` are not written. Under `--gdbserver` the stop is
   reported to the debugger, as any park is.
3. Otherwise the exception is entered: from user mode the stacks swap
   (the SP becomes the kernel SP) and `UM` clears; the frame
   `{PM, DI, INT_MASK, flags, frame PC}` is pushed; `ECAUSE`/`EADDR` are
   written, DI is set and the PC goes to `EVEC`.

An interrupt taken in user mode does the same stack swap and PM, but
leaves DI and `ECAUSE` alone. `IRET` sets `UM` = PM, and when PM is 1
swaps the stacks after the pop (the SP becomes the user SP and `SP_ALT`
the kernel SP just above the frame). Every entry and every `IRET` clears
the LR/SC reservation.

**LR/SC.** One reservation, on the doubleword `EA[31:3]`. `LR64`/`LR32`/
`LR32S` (LR.D/LR.W/LR.W.S) load and set it. `SC64`/`SC32` store rs2 (SC32
its low 32 bits) and set rd = 0 when it is valid and on the same
doubleword (so an `SC32` to the other half of the reserved doubleword
succeeds), and otherwise store nothing and set rd = 1; either way the
reservation is cleared. A faulting SC stores nothing and leaves rd as it
was. The fault does not clear the reservation, but the exception entry
that follows does (as every entry does), so the SC that IRET restarts
fails and the loop retries from the LR (klaussCPU `mmu` e4aa44b fixed
MMU_PLAN §4.2 to say this). With EVEC = 0 the core parks and the
reservation stays as it was. Ordinary stores, and the debugger's memory writes, do not
touch it.

A run's instruction cap (`--max-instructions`) counts exception entries
and TLB walker restarts (below) as well as retired instructions, so a
handler that faults on its own first instruction, or an instruction whose
walks keep evicting its own pages, still ends.

## MMU stages 2 and 3

The emulator implements stage 2 (the MPU) and stage 3 (the TLB) of the
klaussCPU MMU plan (`MMU_PLAN.md` §10-§11 at klaussCPU 12b3783, branch
`mmu`; encodings in [ISA_ENCODING_V2.md](ISA_ENCODING_V2.md) §3.17). No
RTL exists for them yet. Both are off at reset (every program load), so
nothing changes for code that does not turn them on.

**One translation.** Every memory access an instruction makes (loads,
stores, stack operations, LR/SC, IRET's pop, and the fetch of every word
of every instruction) goes through `Cpu::translate` in `src/emu/mmu.rs`,
before the instruction's first side effect: the memory helpers take only
an address it produced. It applies, in order, the stage 1 user-mode MMIO
rule, then with `STATUS.TE` = 0 the MPU (when `MPU_EN` is set), or with
TE = 1 the address map and the TLB. Two accesses are not checked, as the
spec says: the frame push of an interrupt or exception entry, and the
debugger's.

**The MPU** (`--mpu` only). Without `--mpu`, the default, the CPU has
none, as the Nexys A7 bitstream has none: `MFSR`/`MTSR` of SRs
0x10-0x2F are ILLEGAL like any SR number with nothing behind it,
`STATUS[8]` MPU_EN ignores writes and reads 0, so nothing below is ever
checked (and a supervisor fetch from MMIO is a zero word only while TE
= 1), and the debugger's `monitor mpu` says there is none. With `--mpu`
(`STATUS[8]` MPU_EN, with TE = 0): sixteen regions, each an
`MPU_BASEn` (`[31:5]` first 32-byte granule, `[4]` GUARD, `[3]` X, `[2]`
W, `[1]` R, `[0]` EN) and an `MPU_LIMITn` (`[31:5]` the **last** granule,
inclusive; `[4:0]` read 0). A region covers an address when it is
enabled and the address's granule is between the two. For each access:

1. any enabled GUARD region covering it denies it, in either mode
   (ACCESS_*, with `ECAUSE[8]` GUARD set); a GUARD region's R/W/X are
   ignored;
2. otherwise supervisor mode is allowed;
3. otherwise user mode needs an enabled non-GUARD region covering it
   with the permission (R for loads and pops, W for stores and pushes, X
   for fetches); regions are a union with no priority, and none is a
   denial (ACCESS_*, GUARD clear).

The user-mode MMIO rule comes first (a user region over MMIO does not
open it, and a fault there is not a GUARD denial), and a supervisor GUARD
region over MMIO denies the device. Every doubleword is checked: both of
a two-access instruction's, and every word of an instruction, the first
denied word being `EADDR`. A supervisor push into a guard faults with SP
unchanged, and the entry then pushes its frame there unchecked; a vector
that pushes first would fault again one frame lower (the spec's
guard-safe vector rule is software's to keep).

**The address map** (`STATUS[9]` TE = 1; the MPU is then ignored):

| VA | Who | Translation |
|---|---|---|
| `0x0000_0000-0x7FFF_FFFF` (useg) | user and supervisor | TLB |
| `0x8000_0000-0x87FF_FFFF` (kseg) | supervisor | PA = VA - `0x8000_0000` |
| `0x8800_0000-0xBFFF_FFFF` | — | ACCESS_* |
| `0xC000_0000-0xEFFF_FFFF` (kmap) | supervisor | TLB |
| `0xF000_0000-0xFFFF_FFFF` (MMIO) | supervisor | identity |

Any user access at or above `0x8000_0000` is ACCESS_*. Those faults are
decided from the VA alone and do not load `TLB_HI`.

**The TLB.** 32 entries (16 with `--tlb-entries 16`), each `{VPN [15:0], ASID, G, PFN [10:0], V, R, W,
X}` in the `TLB_HI` / `TLB_LO` layout (`TLB_HI`: `[31:16]` VPN, `[8]` G,
`[7:0]` ASID; `TLB_LO`: `[26:16]` PFN, `[3:0]` X W R V). Pages are 64
KiB. An entry matches a VA when V is set, its VPN is VA`[31:16]`, and it
is global or its ASID is the `ASID` SR; if several match, the lowest
index wins (the spec leaves that to be a kernel bug). The PA is `{PFN,
VA[15:0]}`. No match is TLB_MISS_LOAD / _STORE / _FETCH (11-13); a match
without the permission (R, W or X; user and supervisor alike) is
ACCESS_*. On both, `EADDR` is the VA and `TLB_HI` is loaded with {the
VPN of `EADDR`, G = 0, `ASID`}. A TLB miss enters `TLBVEC` when it is
nonzero, else `EVEC`; every other cause enters `EVEC`. `TLBVEC[1:0]`
read 0, as `EVEC`'s do. The points the emulator once marked as open
were all confirmed in klaussCPU `mmu` 424e236 (MMU_PLAN §11.2–§11.3).

`TLBWI` writes entry `TLB_INDEX[4:0]` (32 entries; with 16, `[3:0]`, and `[4]` is ignored and reads 0) from `TLB_HI`/`TLB_LO`, `TLBR`
reads it back, `TLBP` looks `TLB_HI` up (with its own ASID, G-aware) and
sets `TLB_INDEX` to the index, or on a miss sets `TLB_INDEX[31]` and
leaves `[4:0]` alone, and `TLBINV` clears every V. All four are
privileged; any other field set is ILLEGAL. `TLBWI`, `TLBINV`, `MTSR
ASID` and `MTSR STATUS` clear the LR/SC reservation (every such `MTSR`,
even one that rewrites the same value). The spec also has them discard
the fetch side's virtual copies (the fetch window, the NLB and the
BTIC); the emulator keeps none, so that has no effect here.

**The TLB walker** (klaussCPU `MMU_PLAN.md` §11.8, spec v1; not with
`--no-tlb-walker`). SRs, supervisor only, all 0 at reset: 0x35 `TLB_PTB`
(`[26:3]` the PA of the useg pgd; other bits ignored, read 0), 0x36
`TLB_WALK` (`[0]` EN, `[12:8]` RR, the entry the next walker refill
writes: the entry index bits, as in `TLB_INDEX`, so `[11:8]` with 16
entries; `MTSR` writes both), 0x37 `TLB_WCNT` (48 bits: successful
walker refills). With EN and TE set, a useg access (VA`[31]` = 0) with no
matching entry walks before it is a TLB_MISS: VA is what `EADDR` would
be (the data VA, the PC, or the next page's start for an instruction
whose later word misses). D = the doubleword at PA `TLB_PTB` +
VA`[30:26]`·8; if D`[0]` is set, T = the doubleword at PA
D`[26:13]`·(8 KiB) + VA`[25:16]`·8; if T`[4]` (PRESENT) and T`[0]` (V)
are set, entry `RR` = {VPN = VA`[31:16]`, G = 0, the `ASID` SR, PFN =
T`[26:16]`, X/W/R/V = T`[3:0]`} (what `TLBWI` writes from `TLB_HI` = {VPN,
ASID} and `TLB_LO` = T), `RR` += 1 mod the entry count, `TLB_WCNT` += 1,
and the whole instruction restarts, with no exception, as the RTL does
(klaussCPU `tlbwalk` de0694d: the walk runs in the exception entry
sequencer, which then refetches the instruction at its PC): `TLB_HI`,
`TLB_LO`, `TLB_INDEX`, `ECAUSE`, `EADDR` are untouched and nothing is
entered. The aborted attempt has no effect: no register, flag, SP or
memory is written (every instruction translates all its accesses before
its first side effect), it retires nothing and has no trace line, and
`PERF_INSTR` counts the instruction once, when it retires. The PC stays
on it; the next step fetches it again and translates every access again,
so an access may miss and walk again. In particular, when `RR` points at
the entry holding the instruction's other page (word 0 of a fetch whose
word 1 missed, the first doubleword of a two-access instruction such as
`LEAVERET` or a spanning `MEMGET32`), the walk evicts it, the restart
misses on it and walks it again (`RR` and `TLB_WCNT` one higher than a
walk retried in place would leave them), or, when only a `TLBWI` mapped
it (no PTE), takes the TLB_MISS exception for it. With 2 entries an
instruction touching three or four pages can evict its own pages forever
(on the RTL too: the minimum is 2 entries); the run's instruction cap
counts restarts, so such a run ends at the cap. The restart is an
instruction boundary: a ready interrupt is taken there, with the frame
PC = the instruction, and a debugger single-step (which takes no
interrupt) stops there, at the same PC, with nothing retired. As with
`TLBWI`, the LR/SC reservation is cleared. In
every other case (EN = 0, a kmap VA, D`[0]` = 0, T without PRESENT or V,
a pgd or PTE doubleword outside RAM) the miss is the TLB_MISS exception
exactly as without the walker, with `RR` and `TLB_WCNT` unchanged. An
ACCESS fault (a matching entry without the permission) never walks; a
walked entry's R/W/X are checked as any entry's. The two reads are plain
physical RAM reads: no watchpoint sees them; with `KLAUSSCC_CACHESIM`
they are D-cache loads, and the walker's refills and fallbacks are
counted apart from the TLB refill vector entries. A walk (either way)
costs `CYCLES_PER_INSTRUCTION` modelled cycles, and the restarted
instruction its own cycles when it retires; no performance counter
counts the walk but `TLB_WCNT`.

**User single step and watchpoints** (klaussCPU `MMU_PLAN.md` §13, as
merged to master in c5da780; not with `--no-ustep`).

*Step.* An `IRET` whose frame has PM = 1 and bit 45 SS = 1 arms a step
if `EVEC` != 0 (an `IRET` to supervisor mode, or with `EVEC` = 0, ignores
SS). The next instruction to complete is the stepped one; then, before
the next instruction and before any interrupt, STEP (`ECAUSE` 14 |
SSTEP `[12]` = `0x100E`) is entered at `EVEC`: frame PC = the
architectural successor (fall-through, or the target of a taken branch,
CALL, RET, LEAVERET), `EADDR` = the stepped instruction's PC, DI set.
Every entry (an exception, an interrupt, STEP, WATCH) pushes the armed
state as frame bit 45 and disarms, and so does a DOUBLE_FAULT park in
its place; so a fault or SYSCALL of the stepped instruction is entered
as usual with SS = 1 in its frame and no STEP, an interrupt may be taken
before the stepped instruction (its ISR's `IRET` re-arms), never between
it and its STEP, and nothing is stepped in supervisor mode. A walker
refill is no entry: the restarted instruction is still the stepped one.
A TRAP or misaligned access that stops in an attached debugger leaves
the step armed. A debugger write of the mode register with UM = 0
(`Cpu::set_mode_regs`, the board's debug register 21) disarms and drops
a pending STEP.

*Watch.* SRs 0x40 `WP0_ADDR` and 0x42 `WP1_ADDR` (`[31:3]` a doubleword
VA), 0x41 `WP0_CTRL` and 0x43 `WP1_CTRL` (`[7:0]` byte mask, `[8]` R,
`[9]` W, `[10]` U = user accesses only); supervisor only (PRIVILEGED in
user mode), 0 at reset. A watchpoint is enabled with R or W set and a
nonzero mask. An instruction's data accesses that happen (not a faulting
instruction's, nor a failing SC's, nor a not-taken CALLcc's) are compared
by VA: a byte, 16- or 32-bit access, LR.W / SC.W, covers its bytes (a
spanning `MEMGET32` its bytes in both doublewords); a 64-bit access
(A = 0 or 1), LR.D / SC.D, a push (PUSH, PUSHV, CALL, ENTER) or pop (POP,
RET, LEAVE) all 8 bytes of the aligned-down doubleword; LEAVERET both
doublewords `[R15]` and `[R15 + 8]`; DCBZ all four doublewords of its
line, as a store. Not compared: `IRET`'s frame read, entry pushes,
walker reads, fetches, debugger accesses, DMA. A supervisor access hits
only with U = 0 and DI = 0; nothing hits while `EVEC` = 0 or a debugger
is attached. A hit lets the instruction complete; then, before the next
one and before any interrupt, WATCH (`ECAUSE` 15 | WP0 `[9]` | WP1
`[10]` | WRITE `[11]` | SSTEP `[12]`) is entered: frame PC = the
successor, `EADDR` = the raw EA (DCBZ: rs1), or the second doubleword's
start when only that one of a spanning `MEMGET32` or LEAVERET hit. A
step and a watch on one instruction are one WATCH with SSTEP, frame
SS = 1. The entry clears the LR/SC reservation like any other, so a hit
between an LR and its SC makes the SC fail.

The emulator commits an instruction at once, so its STEP or WATCH is
pending until the next step, which takes it before anything else; a
debugger that stops in between (a breakpoint on the successor, a single
step) sees the PC at the successor, and the entry comes at the first
step after it resumes, as with the RTL's debug module.

**Priority.** For one access: MISALIGNED > ACCESS (the address map,
MMIO) > TLB_MISS > ACCESS (TLB permission), within the §4.1 order (an
LR/SC to MMIO is ACCESS before any translation). A two-access
instruction translates both doublewords before either access and
reports the lowest faulting byte: when its first doubleword faults, that
fault is reported whatever the second's would have been.

**Turning translation on.** `MTSR STATUS` with TE takes effect at the
next fetch, which is then a virtual address. Boot code running at its
physical address (in useg) therefore needs a TLB entry for its own page
before it sets TE, and then jumps to its kseg alias (the unit test
`tlb_turning_translation_on_and_off` and `tests/fixtures/hw_tlb.kla` do
this with a global identity entry).

**Double fault.** With TE = 1 an entry's frame push is not translated: it
writes PA = SP - 8 - `0x8000_0000`, and needs SP - 8 in kseg. When it is
not, the machine stops with `DoubleFault(kernel SP)` (the board parks
with `park_kind` 6, or debug-halts with a debugger attached) and nothing
of the entry happens: no stack swap, no frame, no `ECAUSE`, `EADDR` or
`TLB_HI`, the PC on the instruction. An interrupt entry stops the same
way and stays pending. With TE = 0 there is no such check.

**Parking.** With `EVEC` = 0 every cause parks as in stage 1, the new
ones (TLB misses included, whatever `TLBVEC` holds) as an illegal
instruction, and a park writes neither `ECAUSE`, `EADDR` nor `TLB_HI`.

**The debugger** (`--gdbserver`). With TE = 1 the debugger's memory
addresses are virtual: each is translated as a supervisor data access
would be (kseg, MMIO, the TLB in the current `ASID`), but without the
entry's R/W/X check, so execute-only text can be read and a read-only
page patched; an address that does not translate is an error. `monitor
phys on` makes them physical. Watchpoints and breakpoints compare virtual
addresses. `monitor mpu` and `monitor tlb` list the regions and entries,
and `monitor sr` shows the new SRs. The board's server stays physical,
because its debug module reads physical memory.

## SD card

The SD window `0xF000_0000` (device `0x000`) is modelled only with
`--sd-image FILE` or `--sd-native`. Without either, it reads 0 and
ignores writes, exactly as before, and `CPU_FEATURES`, `INT_PENDING` and
`0xF00F_0050`/`0x58` are unchanged (traces of a program run without the
options are bit-identical to the emulator before the SD model). The
card-detect pin is active low on the Nexys A7 (`SD_STATUS[1]` = 0 with a
card in, measured on the board), so that all-zero window reads as "card
present" on the raw pin, though no command is ever answered.

**The card** (`--sd-image FILE`): an SDHC card whose blocks are the raw
disk image `FILE` (a whole number of 512-byte blocks, at least 512 KiB),
read and written in place. Block addressing, OCR `0xC0FF_8000` once
ready (CCS = 1; the second ACMD41 with HCS completes it, without HCS it
never does), CSD 2.0 with `C_SIZE` = the image's whole 512 KiB units − 1
(a tail beyond them is reported with a warning), CCC `0x515` (no erase or
lock commands), and an SCR offering 1- and 4-bit buses and CMD23. The
card comes up in SD mode when `SD_CTRL.PWR_ON` is set, loses all state
when it is cleared, and enters SPI mode on a CMD0 sent with CS low. Host
CRCs are not checked; the card's own CRC7s and CRC16s are real. Nothing
takes time: a busy period is two `0x00` bytes in SPI mode and nothing in
SD mode, and no timeout fires for a card that answers. An I/O error on
the file is reported at the end of the run.

**SPI mode** (`sd_spi.sv`): `SD_CTRL` (`[15:0]` divider, reset 249;
`[16]` CS high, reset 1; `[17]` power, reset 0; read back as written),
`SD_DATA`, `SD_STATUS` (`[0]` BUSY; `[1]` the raw card-detect pin,
active low: 0 with `--sd-image`, 1 with `--sd-native` and no card;
`SDM_STATUS[3]` `CARD_PRESENT` is the inverted, active-high form) and
`SD_BUF`
(`+0x200..+0x3F8`, `addr[9]`, plain RAM; sub-word accesses use the byte
lanes of `addr[2:0]`). Commands: CMD0, 1, 6, 8, 9, 10, 12 (stuff byte,
R1, busy), 13 (R2), 16, 17, 18, 24, 25 (`0xFC` tokens, `0xFD` stop), 55,
58 (R3), 59, ACMD13, 22, 23, 41, 51; R1 `0x40` for an address past the end,
the `0x08` error token when a CMD18 runs off it, data response `0xE5`.
An exchange runs `16 * (div + 1) + 1` modelled cycles, and a `SD_DATA`
write while one runs is ignored, as in the RTL; but the first
`SD_STATUS` read that finds it running returns BUSY = 1 and ends it, so
a poll loop spins once per byte at any clock, while a `SD_DATA` read
right after the write still returns the previous byte. Zephyr's and
Linux's pipelined read loop therefore sees the next byte still shifting
and never retries a block.

**Native SD mode** (`--sd-native`, klaussCPU `SD_NATIVE_PLAN.md` v1):
`SD_CTRL[18]` `SD_MODE`, the `SDM_*` registers (`+0x100..+0x1F8`),
`CPU_FEATURES[21]`, and the source 2 cascade: `INT2_EN` (`0xF00F_0050`,
reset `0b01`), `INT2_PENDING` (`0xF00F_0058`; `[0]` Ethernet is always
0, as Ethernet is not modelled), `INT_PENDING[2]` =
`|(INT2_PENDING & INT2_EN)`, a level that is dispatched like the timer
(vector `INT_VEC2`, `INT_MASK[2]`) and stays pending until the
`SDM_STATUS` bits behind it are cleared. A command completes during its
`SDM_CMD` store (`CMD_BUSY` never reads 1): the card's 48- or 136-bit
frame is built with its CRC7 and checked as the response type asks, so a
wrong type gives `CMD_CRC_ERR`/`CMD_END_ERR`/`CMD_IDX_ERR`, and a card
that does not answer (an illegal command, CMD5, a card in SPI mode) gives
`CMD_TIMEOUT` at once. The data engine moves whole blocks between the
card and the 512-word FIFO whenever the FIFO has room for one (read) or
holds one (write), so `DATA_BUSY` lasts until the count is exhausted
(`DATA_DONE`), an error, or a CMD12 with `SDM_CMD[13]`. Host and card
must agree on the block size and bus width (`SDM_CFG[0]` and ACMD6),
else `DATA_CRC_ERR` / `WRITE_CRC_ERR`; a read past the end gives
`DATA_TIMEOUT`, a write the card is not receiving `WRITE_NO_TOKEN`. The
card's states are idle/ready/ident/stby/tran/data/rcv/prg (prg passes at
once) and ina; CMD0, 2, 3, 6 (high speed: see below), 7, 8, 9, 10, 12,
13, 15, 16, 17, 18, 23, 24, 25, 55 and ACMD6, 13, 22, 23, 41, 42, 51
(ACMD22: the blocks the last CMD24/CMD25 wrote, 4 bytes big-endian). The
emulator does not reload programs: every `--emulate` run and every
`--gdbserver` session starts from reset (the gdbserver has no load or
restart command), so the plan's resets at `LOAD_COMPLETE`, `INT2_EN`
back to `0b01` on every load included, always hold. The write-side
`FIFO_THRESH` uses the block count and size latched by the data
command. Reset values and corner cases
follow the plan's §4.3 and §4.8 (klaussCPU mmu-sd 57a5a8c): `SDM_BLK` resets to
`0x0001_0200`; the block size is a multiple of 4 (`[1:0]` read 0), a
field of 0 acts as 8, and a block takes ceil(size / 8) FIFO words, each
block starting in a new word, with the last 4 bytes of a size that is
4 mod 8 in `[31:0]` (`[63:32]` reads 0, and is ignored on a write); `FIFO_THRESH` follows the
direction latch (NONE after a reset or a FIFO reset, so a write pre-fill
never raises it; READ: a block in the FIFO, of the size latched by the
last data command, during the phase and after it; WRITE: a write phase runs,
there is room for a block and fewer words were pushed since the FIFO
reset than the count's blocks hold); a data `SDM_CMD` while `DATA_BUSY`
is ignored entirely (DIR, `SDM_BLKDONE` and the latched size
included), a non-data one goes out (CMD12 with `[13]` stops
the phase, setting `DATA_DONE` only for an open-ended phase; with no
phase it is an ordinary R1b); clearing `SD_MODE` abandons the engines
(no DONE) and keeps the registers, the FIFO and DIR; only `SDM_CMD`
needs `SD_MODE`; a type-0 command or a `CMD_TIMEOUT` leaves `SDM_RESP*`
unchanged, a response with a CRC, end-bit or index error is held as
received, a
48-bit response leaves `SDM_RESP1` = 0 and `SDM_RESP0[63:38]` = 0;
response types 5-7 act as 0; `SDM_ARG` keeps 32 bits.

### SD high speed (`--sd-no-hs`, `--sd-hs-window`)

`--sd-native` has the high-speed sampling of the klaussCPU `sdhs`
bitstream (`SD_NATIVE_PLAN.md` §9): `SDM_CAPS[9]` (high speed), `[11]`
(the input delay line) and `[27:24]` = 7 (sample phases), so `SDM_CAPS` =
`0x0708_0B01`; `SDM_CFG[1]` `HS_SAMPLE`, `[7:4]` the sample phase and
`[12:8]` the IDELAY tap are kept and read back (phases 7-15 act as 6);
`SDM_STATUS[6]` `IDELAY_RDY` reads 1 (live, no interrupt). `--sd-no-hs`
models the bitstreams before it: those bits read 0 and `SDM_CAPS` is
`0x0008_0101`. The card's CMD6 (SD and SPI mode) supports functions 0 and
1 in function group 1 (support `0x8003`) and only function 0 in groups
2-6: each group reports the function asked for, the current one for 0xF,
or 0xF when unsupported, which in mode 1 cancels the whole switch; mode 1
then switches group 1 at once. CMD0 and a power cycle return the card to
default speed. `SD_CTRL` divider 0 runs CLK at 50 MHz, and the DMA's block
time follows the clock (half that of divider 1). Every sample instant is
good unless `--sd-hs-window LO..HI` gives the eye of the bit the card
launches on CLK's rising edge, in ns after that edge: phase p at tap t
samples at T = 10 + 5p − 0.078t ns, and T samples well if T − 20k is in
the eye for some k (phases p and p + 4 sample the same point); at
divider 0 with `HS_SAMPLE`, an instant outside the window gives
`CMD_CRC_ERR` on response types 1-3 (the card has executed the command)
and `DATA_CRC_ERR` on read blocks (the card has sent them). k = −1 (T a
clock before the eye: the strobe takes the bit before its slot's, a slow
card at phases 0-1) passes commands and single blocks but loses the next
start bit after a CLK park: the block after a read parked with the FIFO
full gets `DATA_CRC_ERR` and `DATA_END_ERR` (`SDM_BLKDONE` short), as the
klaussCPU sdhs simulation shows; phase p + 4 at the same tap does not. Misuse the
hardware would punish is only noted on stderr (`klausscc sd: ...`, the
first few of each kind): a command at 50 MHz to a card at default speed
or without `HS_SAMPLE`, and a change of `SDM_CFG[12:4]`/`[1]` while
`DATA_BUSY` or `DMA_BUSY`.

### SD DMA (`--sd-dma`)

`--sd-dma` (needs `--sd-native`) adds the DMA engine of the klaussCPU SD
DMA spec v1.2, modelled on its RTL (branch `sddma`, `SD_NATIVE_PLAN.md`
§8): `SDM_DMA_ADDR` (`+0x150`, 64-bit, `[2:0]` read 0), `SDM_CFG[16]`
`DMA_EN`, `SDM_STATUS[5]` `DMA_BUSY` (live) and `[27]` `DMA_ERR` (sticky,
W1C, in `ERR_ANY` and `SDM_IRQ_EN[27]`), and `SDM_CAPS[10]` (`SDM_CAPS` =
`0x0708_0F01` with high speed, `0x0008_0501` with `--sd-no-hs`). Without
it those read 0 as on the bitstreams before `sddma`.
`DMA_EN` is sampled when `SDM_CMD` is written: a data command written while
it is 1 moves its blocks between the card and DDR at `SDM_DMA_ADDR`
through the FIFO, and `SDM_DMA_ADDR` advances as they move (it is always
the first byte not transferred). Unlike PIO, the data phase takes modelled
time: each block takes its bits on the bus (1 or 4 per SD clock,
`SD_CTRL[15:0]`) plus start bit, CRC16, end bit and a gap (a write also its
CRC status and a short busy), the first one after the command and
response, so the interrupt arrives later and a CPU in `WAIT` sleeps to it.

The card side of a read puts each block in the FIFO once its CRC passed (a
bad block never gets there; `BLKDONE` counts good blocks received), and the
engine moves blocks from the FIFO's head to DDR one at a time, each landing
44 cycles of DDR latency plus a cycle per 32 bytes after it started. The
FIFO must be empty when the command is written: words left in it reach DDR
ahead of the card's data (the emulator notes this on stderr and counts it,
`N DMA reads started with words in the FIFO`). A counted read's
`DATA_DONE` (and the end of `DATA_BUSY`) comes when the engine has moved
its last block. A write fetches blocks into the FIFO ahead of the card
while it has room (never past size x count; open-ended at most the FIFO's
4 KiB ahead), and `DMA_BUSY` drops with `DATA_DONE` when the card has
programmed the last block.

`DMA_BUSY` is the engine's phase or a block still on its way to DDR (or a
wedged engine, below); 0 means every DMA write is in DDR and none will be
issued. While it is 1, `SDM_DATA` loads read 0 and pop nothing, stores are
dropped and set `FIFO_ERR`, `FIFO_THRESH` reads 0, writes to `SDM_DMA_ADDR`
are ignored (it reads back the live address), and a data command is
ignored entirely, also with `DATA_BUSY` clear (after a CMD12 or an abort):
the emulator says so on stderr and counts it (`N data commands ignored
while DMA_BUSY`), since a driver doing it waits for a completion that never
comes.

`DMA_ERR`: a block size not a multiple of 8, or a counted transfer
(`SDM_BLK` count != 0) whose `[ADDR, ADDR + size x count)` passes the end
of DDR, is refused when `SDM_CMD` is written: `DMA_ERR`, no card command
(no `CMD_DONE`), nothing moves. An open-ended DMA checks each block before
it moves; on failure the DMA ends (nothing of that block reaches DDR; a
read block stays in the FIFO) but the card's data phase goes on through
the FIFO until it is full or CMD12. Ends and aborts: `DMA_EN` = 0, or
clearing `SD_MODE`, lets the block on its way to DDR land and stops the
engine (`DMA_BUSY` drops then); blocks the card side committed that the
engine had not started stay in the FIFO for PIO, and the card side goes on
as PIO (clearing `SD_MODE` abandons it instead); a counted read whose card
side was done gets `DATA_DONE` at once. `SDM_FIFO[31]` stops the engine
after the 16-byte DDR transaction in flight (the rest of the block is
dropped; `SDM_DMA_ADDR` says how far it got). A CMD12 or a card error ends
the card side at once; a read's engine first moves the whole blocks still
in the FIFO, a write's stops.

With `--sd-dma` the emulator also checks DMA coherence, since it has no
caches: per 32-byte line it tracks CPU stores not yet cleaned (by any walk
that writes dirty lines back: FLUSH, INVALIDATE, `RANGE_CLEAN` or
`RANGE_INV`), DMA writes since the line's last clean, DMA writes not yet
invalidated in the D-cache, and DMA writes not yet invalidated in the
I-cache (`ICACHE_INV`, `RANGE_IINV`; a CPU store to the line also counts,
as the I-cache snoops CPU stores). A DMA reading or overwriting a line the
CPU dirtied, a walk writing a dirty line back over data a DMA wrote since
the line's last clean (the device's data is lost), a CPU load or store of a
line a DMA wrote, the CPU executing from a line a DMA wrote without an
I-cache invalidate or (I-cache fills come through the D-cache) a D-cache
invalidate since, and a DMA reading or writing memory while a D-cache walk
over its lines (whole-cache, or an overlapping range) is pending are each
reported on stderr (`klausscc dma: coherence: ...`, the first 16) and
counted in the run's summary (`SD DMA: ... DMA coherence problems: N`). A
D-cache walk's effect counts when it completes, and covers only the CPU
stores before its `CACHE_CTRL` write; an I-cache one counts at once.

### Cache maintenance

`CACHE_CTRL` (`0xF005_0000`) FLUSH `[1]` and INVALIDATE `[2]` change no
data (there are no caches) but keep `CACHE_STATUS[0]` `MNT_BUSY` up for
the walk's time, 4,097 cycles plus 3 per dirty line (dirty as the
`--sd-dma` checker sees it, at most 2048 lines; 0 without it; one walk: the
RTL's double walk is being fixed). `ICACHE_INV` `[3]` is a 64-cycle sweep,
coherent at once and not in `MNT_BUSY`. `--dcache-range-ops` adds the
range ops (spec v1.2, branch `sddma`; `CPU_FEATURES[27]`): `CACHE_OP_ADDR`
(`0x30`, a 32-bit register: the upper half of a store is dropped; `[4:0]`
read 0), `CACHE_OP_LEN` (`0x38`, `[31:0]`, 0 = no-op), `CACHE_CTRL[4]`
`RANGE_CLEAN` (write the range's dirty lines back), `[5]` `RANGE_INV`
(write them back, then drop the range's lines; with `[4]` the same) and
`[6]` `RANGE_IINV` (drop the range's lines from the I-cache), over
ceil(LEN / 32) lines from `ADDR`. A D-cache range walk keeps `MNT_BUSY` up
1 + 2 cycles a set for min(n, 1024) sets, + 6 for a set with a dirty line
of the range (+ 3 for a second). `RANGE_IINV` is coherent at once and
keeps `MNT_BUSY` up min(n, 64) cycles (64 if it meets a sweep running or
covers the 2048-line I-cache, when it sweeps everything; the cache
simulator drops the range's lines and empties the next-line buffer whole).
An op takes `ADDR` and `LEN` when it is accepted. The queue is the RTL's:
one D-cache walk runs, and one whole-cache request or one range op waits; a
range op that finds a range op or a whole-cache walk waiting, or that
covers more than 2048 lines, becomes a whole-cache walk (an INVALIDATE if
either op invalidates), and a whole-cache request absorbs a waiting range
op the same way (the summary counts conversions: `N range ops made
whole-cache walks`). FLUSH or INVALIDATE with `[5:4]` runs the whole-cache
op only; `RANGE_IINV` with them, or `ICACHE_INV` with `[5:4]`, does both.
It is independent of `--sd-dma`. `CPU_FEATURES` with these options gains
`[21]` (`--sd-native`) and `[27]` only: `0x0AE0_200D` with all three, the
sddma bitstream's `0x0FF0_200D` less the TRNG, `ETH_RX_SLOTS` and L2 bits,
as without them.

### Fault injection

`--sd-fault SPEC` (repeatable, needs `--sd-image`) makes the card fail,
so drivers' error recovery can be tested. `SPEC` is
`KIND[:CMD]@N[xCOUNT]`: the `N`th matching event since the start of the
run (1-based) and the `COUNT - 1` after it (default 1) fail; then the
card behaves again, so a retry, which is a new event, succeeds. Each
spec counts its own events.

| `KIND` | Event | SD mode (`--sd-native`) | SPI mode |
|---|---|---|---|
| `wcrc` | a memory block written (CMD24/25) | CRC status 101: `WRITE_CRC_ERR` | data response `0x0B` (`0xEB`), no busy |
| `wbusy` | the same | busy never ends: `BUSY_TIMEOUT` at once | data response `0x05`, then MISO `0x00` until CS goes high or power is cut |
| `rerr` (`rcrc`) | a memory block read (CMD17/18) | bad CRC: `DATA_CRC_ERR`, the block never reaches the FIFO | error token `0x04` instead of the block; a CMD18 then sends nothing until CMD12 |
| `cmdto` | a command the card receives; `cmdto:CMD` only index `CMD` (as sent, so `cmdto:41` is ACMD41) | `CMD_TIMEOUT` | R1 stays `0xFF` |
| `dmaerr` | a data command written with `DMA_EN` (`--sd-dma`) | `DMA_ERR` as if its range left DDR: a counted command is refused up front, an open-ended one stops before its first block | - |
| `dmastall` | the same | the engine moves nothing (`DMA_BUSY` = 1, no `DATA_DONE`) until `DMA_EN` = 0, `SDM_FIFO[31]`, CMD12 or `SD_MODE` = 0 | - |
| `dmahang` | the same | stalls, and only clearing `SD_MODE` drops `DMA_BUSY` | - |
| `dmastuck` | the same | stalls, and `DMA_BUSY` never drops again | - |

A failed or never-finished write is not programmed, so ACMD22 counts
only the blocks before it, and the card stays in rcv (SD mode) or waits
for the next token (SPI CMD25) until the host stops it. A command that
gets no response is not executed (as a frame with a bad CRC), and it
cancels a preceding CMD55. A CMD18 block cut short by CMD12 in SPI mode
is not a read event. Each injected fault is logged on stderr
(`klausscc sd: injected wcrc at write block 2 (block address 201)`), and
the run ends with the totals (`SD faults injected: ...`). Without
`--sd-fault` nothing changes.

## Board I/O: LEDs, RGB LEDs, 7-segment display, switches

The three simple devices of klaussCPU `MMIO_MAP.md` are modelled on the
RTL (the MMIO read mux and write handler in `KlaussCPU.sv`):

| Address | Register | Write | Read |
|---|---|---|---|
| `0xF002_0000` | `RGB1` | `[11:0]` (`[11:8]` R, `[7:4]` G, `[3:0]` B) | the last write, 12 bits |
| `0xF002_0008` | `RGB2` | the same | the same |
| `0xF003_0000` | `SEG_LOW` | 4 hex digits `[15:0]` to the lower display | its padded value, 32 bits |
| `0xF003_0008` | `SEG_HIGH` | 4 hex digits `[15:0]` to the upper display | its padded value, 32 bits |
| `0xF003_0010` | `SEG_ALL` | 8 hex digits `[31:0]`, `[31:16]` to the upper display | `{upper, lower}` padded, 64 bits |
| `0xF003_0018` | `SEG_BLANK` | any write blanks both displays | 0 |
| `0xF003_0020` | `SEG_RAW` | segment patterns, byte *k* = digit *k* from the right; byte enables honoured | the same 64 bits as `SEG_ALL`, the access's lanes |
| `0xF003_0028` | `SEG_MODE` | dropped | `[0]` RAW |
| `0xF004_0000` | `LEDS` | `[15:0]` | the last write, 16 bits |
| `0xF004_0008` | `SWITCHES` | dropped | `--switches VALUE` (default 0), 16 bits |

A display holds each digit as a byte `{4'h0, digit}`, so after a
`SEG_ALL` write of `0x12345678`, `SEG_ALL` reads `0x0102_0304_0506_0708`.
A blank display reads `0x2222_2222` (`0x22` is the display controller's
" " glyph). A program starts as one loaded onto the board does:
`LOAD_COMPLETE` clears the LEDs and RGB LEDs, and `START_WAIT` blanks both
displays as it starts the pipeline, so `LEDS`, `RGB1` and `RGB2` read 0
and `SEG_LOW` / `SEG_HIGH` read `0x2222_2222`. Every other offset in the
three windows reads 0 and ignores writes.

`SEG_RAW` and `SEG_MODE` (klaussCPU `MMIO_MAP.md`, branch `seg7raw`,
`CPU_FEATURES[30]`) drive the segments directly. In each byte of
`SEG_RAW`, bits 0-6 are segments a-g and bit 7 the decimal point, 1 =
lit; byte 0 is the rightmost digit. Unlike the other registers it honours
the byte enables: a 64-bit store sets all eight digits, a 32-bit store at
`0x20` / `0x24` the right / left four, a 16-bit or byte store only its
own. It is the storage `SEG_ALL` reads, so `SEG_RAW` reads the same as
`SEG_ALL`, and a load of any width returns its own lanes (a 32-bit load at
`0x24` the left four digits). Any `SEG_RAW` store sets `SEG_MODE[0]`
RAW; `SEG_ALL` and `SEG_BLANK` clear it as they set all eight digits, and
`SEG_LOW` / `SEG_HIGH` clear it and, if it was set, blank the other four
digits (`0x22` each), which held segment patterns. A program starts with
RAW clear; `HALT` leaves it. `--no-seg7raw` models the bitstreams before
them: both read 0 and drop writes, and `CPU_FEATURES[30]` reads 0.

These devices ignore the byte enables, and the core replicates a sub-word
store's data across the doubleword (`pipeline_core.sv` `exo_wdata`:
`{8{c[7:0]}}`, `{4{c[15:0]}}`, `{2{c[31:0]}}`), so the emulator does the
same: a `MEMSET8` of `0x5A` to `LEDS` sets `0x5A5A`, and a 16-bit store
of `0x1234` to `SEG_ALL` shows `12341234`. A sub-word load takes the low
bytes of the register, as for every device.

`--board-io-log FILE` writes one line for each store to an LED, RGB LED
or display register, with what the device latched: `LEDS 0x5a5a`,
`RGB1 0xf00`, `RGB2 0x00f`, `SEG_LOW 1234`, `SEG_HIGH abcd`, `SEG_ALL
deadbeef` (the digits written), `SEG_BLANK` and `SEG_RAW
807f3f065b4f666d` (a 64-bit store). A narrower `SEG_RAW` store logs the
digits it set, numbered from the right, and their bytes, leftmost first:
`SEG_RAW[3] ab`, `SEG_RAW[1:0] 5566`, `SEG_RAW[7:4] 11223344`. A
`SEG_LOW` / `SEG_HIGH` line after a `SEG_RAW` one also blanked the other
four digits. A store of the same value
twice is logged twice; stores to `SWITCHES` or unlisted offsets are not
logged, nor are `SEG_MODE` writes. Each line is written as it happens, unbuffered, so a run that is
killed keeps its log. With `-` the lines go to stderr after `klausscc io:
`, as the emulator's other stderr notes do. Before this model these
windows read 0 and dropped writes, which no bitstream ever did, so there is
no option for the old behaviour.

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
  One consequence: a pending interrupt is taken on the first step after
  `EI` retires. The core reads the committed enable bit (`irq_take`) and
  abandons an interrupt boundary when a younger `DI` is in flight, so on
  the board `EI` immediately followed by `DI` may let no interrupt in.
  Software must not depend on either behaviour.
- `DELAYR`/`DELAYV` do not wait, but advance modelled time by the
  RTL's spin, value × 8192 cycles (value = rs1[31:0] or imm32). LCD
  instructions do nothing once their encoding has been checked.
- Interrupt sources 1–2 (blitter, Ethernet) are not modelled; only the
  timer (source 0) fires, and with `--sd-native` the SD controller
  through the source 2 cascade.
- MMIO is the UART (`0xF001_0000`: `TX_DATA`, `RX_DATA`, `STATUS`), the
  RGB LEDs, 7-segment display, LEDs and switches (`0xF002_0000` to
  `0xF004_0000`, see "Board I/O"), the cache controller
  (`0xF005_0000`, see "Cache maintenance"), the interrupt controller /
  timer (`0xF00F_0000`), the performance counters (`0xF00D_0000`) and,
  with `--sd-image` or `--sd-native`, the SD controller (`0xF000_0000`,
  see "SD card"). Other MMIO reads return 0 and writes are ignored.
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
- MMU stage 1 timing: `MTSR` does not serialize (nothing to serialize),
  and `MFSR CYCLE` reads the modelled count, not the board's.
- MMU stages 2 and 3: the TLB instructions and the MTSRs do not
  serialize, for the same reason, and there is no fetch window, NLB or
  BTIC for a TLB change to discard. The TLB's base-register lookup with a
  MEM replay on a page crossing (MMU_PLAN §11.4) is a timing detail with
  no architectural effect; the emulator translates the EA.

### Flag unification: done

The cross-repo flag-unification spec (the RTL repo's
`FLAG_UNIFICATION_CHANGES.md`) asked klausscc for three things: CMP sets `Z S C V`, conditions are
derived with `ULT = C`, and `E/L/U` are derived on read in GETF, the
interrupt frame and the trace. All three are implemented
(`src/emu/flags.rs`, `src/emu/exec.rs`) and covered by unit tests. The
assembler needed no change: the encodings are the same and `JMPE`/`JMPNE`
still assemble.

## Trace format (shared with the RTL self-trace)

`--emulate --trace` writes one line per **retired** instruction (an
instruction that raises an exception retires nothing and gets no line,
as an illegal one; the exception entry has none either), showing
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
  value. For MMIO, `data` is the value written. With translation on,
  `addr` is the **physical** address: the RTL's `ret_wr_addr` is the
  translated port address (`pipeline_core.sv` :2997, :3659, :3758). (Up to
  klausscc 8463af5 the emulator recorded the virtual address here.) DCBZ
  shows one write: the line's first doubleword (`PA & ~31`), `be` `ff`,
  `data` 0 (`HW_PERF_PLAN.md` §1).
