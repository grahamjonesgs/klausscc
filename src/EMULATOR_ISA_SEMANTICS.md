# KlaussCPU emulator — RTL semantics & corner cases

Authoritative behaviours for the independent ISA emulator (golden-model, Phase 0
of the FPGA repo's PIPELINE_PLAN.md). Extracted by reading the RTL task files
(`KlaussCPU.srcs/sources_1/new/*.vh` + `KlaussCPU.v`) — the **silicon is the
authority**, not `CPU_ARCHITECTURE.md`. The per-opcode *encodings* come from the
assembler's own tables (`opcodes.rs` / the `--opcode` `opcode_select.vh`); this
file records only the EXECUTION semantics and the places the RTL **diverges from
the architecture doc** — the high-value, easy-to-get-wrong cases.

> Every item below must be re-confirmed by the emulator-vs-RTL self-trace
> cross-check (the "authority" half of the golden model). Where emulator and RTL
> disagree, investigate — do not auto-trust either.

## RTL-vs-doc divergences (implement the RTL behaviour)

1. **Reset vector is PC = 0x4, not 0x20.** Doc §12 says 0x20; that's a linker
   convention. Cold-start PC = 0x0. (The assembler still places code at 0x20 via
   the heap header.)
2. **Undocumented opcodes that exist in silicon** — must be emulated:
   - `ADDI` (0x02??, RRV, rd=[7:4], **sign-extended** imm)
   - `LEAPC` (0x099?)
   - `LDIDX8_S` / `LDIDX16_S` (0xC6 / 0xC7) — **sign-extending** sub-word loads
   - `LDIDX64A` / `STIDX64A` (0xFC / 0xFD) — force address `& ~7`
   - **PC-relative branch/call family 0x1030–0x1041** — doc §9 wrongly claims
     "all jumps are absolute". These compute target = PC + signed offset.
3. **Divide/modulo by zero does NOT trap:**
   - `DIV*` by 0 → result `0xFFFF_FFFF_FFFF_FFFF`, `overflow_flag = 1`.
   - `MOD*` by 0 → result = dividend, `overflow_flag = 1`.
   - `MODV` by 0 → **no writeback at all**.
   - In **all** zero-divisor paths `zero_flag` is left **untouched** (only overflow set).
   - Do NOT raise SIGFPE / exception.
4. **Signed `INT_MIN / −1` is NOT special-cased.** `abs(INT_MIN)` wraps; result
   = INT_MIN, `overflow = 0`. No trap.
5. **`MULV` / `DIVV` / `MODV` immediates are SIGNED**, even though they sit in the
   zero-extend RV opcode block.

## Flag rules (the biggest correctness trap)

Flags are **sticky** — an instruction that doesn't write a flag leaves the prior
value. Conditional jumps read whatever the last flag-writer left.

- **`overflow_flag`**: written ONLY by ADD/SUB-family, MUL, DIV/MOD, ABSR.
- **`carry_flag`**: written ONLY by ADD/SUB-family and the carry-rotates.
- **`sign_flag`**: written by the arithmetic result MSB (ADD/SUB-family etc.);
  left stale by logic/shift/move/load/etc.
- **`zero_flag`**: set from the 64-bit result by arithmetic AND by the **RRR**
  logic forms `ANDR`/`ORR`/`XORR`. **Asymmetry:** the **RV** immediate forms
  `ANDV`/`ORV`/`XORV` do **NOT** set `zero_flag`.
- **`equal_flag` / `less_flag` / `ult_flag`**: set **only** by `CMPRR` / `CMPRV`.
- Logic ops, shifts, rotates, bit ops, sign/zero extends, min/max, the boolean
  `CMP*R` ops, ALL loads/stores, and all flow-control leave carry/overflow/sign
  at their **stale** values.

## Other corner cases

- **`MEMGET32` (0x79) reads ANY alignment** — assembles a little-endian 32-bit
  word that may straddle the 8-byte doubleword boundary (no `& ~3`). But
  **`LDIDX32` (0xC0) masks `& ~3`**. They disagree on unaligned addresses —
  emulate each as written.
- **`CMPRV` unsigned compare sign-extends the immediate first**, so a negative
  imm becomes a large unsigned operand for the `ult` comparison.
- **`SETFR` bit layout:** `rd = {zero, equal, carry, overflow, 60'b0}` — the four
  flags occupy the **top** 4 bits [63:60].
- **Interrupt saved-context slot** (pushed on dispatch, restored by `IRET`):
  `[31:0] = PC`, `[38:32] = {zero, equal, carry, overflow, sign, less, ult}`
  (bit38→zero … bit32→ult), `[42:39] = INT_MASK`, `[63:43] = 0`.
- **CALL/CALLR push a zero-extended return PC** (`PC+8` for V-format CALL,
  `PC+4` for CALLR). `RET`/`IRET` restore PC from `[31:0]` only.
- **Shift/rotate counts masked to 6 bits** — count 64 aliases to 0 (no shift).
- **`CLZ(0) = CTZ(0) = 64`.**
- The big-endian→little-endian "fixes" listed in `CPU_ARCHITECTURE.md` §14 are
  **already applied** in the current RTL — emulate the fixed (little-endian)
  behaviour, e.g. byte lane `n` ↔ bits `[8n+7:8n]`.

## Trace format (shared with the RTL self-trace)

One line per **retired** instruction, capturing architectural state **after**
the instruction commits (so it diffs against the Vivado RTL self-trace hooked at
the `OPCODE_FETCH2` commit gate / writeback):

```
i=<n> pc=<8hex> op=<8hex> r0=<16hex> ... r15=<16hex> sp=<8hex> f=<zscoelu bits> [wr=<addr8>/<be2>/<data16>]
```

`f` = a fixed-order 7-char bitstring `{zero,sign,carry,overflow,equal,less,ult}`.
`wr` present only when the instruction performed a memory write.
