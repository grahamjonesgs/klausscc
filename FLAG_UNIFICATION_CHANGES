# Flag-model unification — cross-repo change spec (LLVM / assembler / emulator / runtime)

**Status:** RTL side DONE on branch `feat/core-pipeline` (commit `fbb77d7`).
**Owner of this doc:** CPU/RTL. **Action owner for everything below:** toolchain
(LLVM fork + `klausscc` emulator + assembler) — the user. This is the "document
the LLVM/assembler changes, I'll handle them separately" deliverable.

Phase 1 of `PIPELINE_MASTER_PLAN.md`. Read that for the why (pipeline-friendly
single flags register). This doc is the *what changed* and *what you must mirror*.

---

## 1. What the RTL now does (the new architectural contract)

### 1.1 One 4-bit flags register: `Z S C V`
The architectural flags register is now **`Z` (zero), `S` (sign), `C` (carry/
borrow), `V` (overflow)**. The old separate compare flags **`E` (equal), `L`
(signed less), `U` (unsigned less) are RETIRED** — they no longer exist as
storage.

### 1.2 `CMP` is now `SUB` without writeback
`CMPRR/CMPRV` (the flag-setting compare, `ALU_CMP`) computes `a - b` and sets the
**full `Z/S/C/V`** exactly like `SUB`, with **no register writeback**. Previously
it set only `E/L/U` (+S) and left `Z/C/V` stale.

### 1.3 Carry is the x86 BORROW convention — **`ULT = C`, not `¬C`**
Verified from the subtractor (`klauss_pkg.sv` `f_alu`): `sum = {1'b0,a} -
{1'b0,b}`, and `C = sum[64]`. So after `SUB`/`CMP`:

> **`C = 1` ⟺ `a < b` (unsigned)** — a borrow occurred.

This is the **x86 convention**, the *opposite* of ARM. Get this right in the
backend and emulator or every unsigned branch inverts. `SBC` matches:
`cin = C; result = a - b - cin`.

### 1.4 Branch conditions are DERIVED from `Z/S/C/V`
The COND field encoding is **unchanged**; `f_cond_eval` derives each relation:

| COND | mnemonic sense | derived expression |
|---|---|---|
| 0 | always | 1 |
| 1 | Z / EQ | `Z` |
| 2 | C | `C` (raw carry / borrow) |
| 3 | V | `V` |
| 4 | S | `S` |
| 5 | LT (signed <) | `S ^ V` |
| 6 | LE (signed ≤) | `Z | (S ^ V)` |
| 7 | ULT (unsigned <) | `C` |
| 8 | ULE (unsigned ≤) | `C | Z` |
| 9 | E (equal) | `Z` (alias of COND 1) |

`INV` inverts the taken bit, giving the negations: `NE=¬Z`, `GE=¬(S^V)`,
`GT=¬(Z|(S^V))`, `UGE=¬C`, `UGT=¬(C|Z)`. Derivations are **bit-identical to the
retired `E/L/U`** for every operand pair — proven by `tb_flags.sv`
(256,192 checks, 0 errors, incl. signed-overflow corners + the borrow polarity).

### 1.5 ISA-visible flag word: 7-bit layout KEPT, `E/L/U` derived on read
The interrupt-save frame, `GETF`, and the crash dump still present the old 7-bit
`{Z,E,C,V,S,L,U}` word (bits `[38:32]` in the IRQ frame) — but `E/L/U` are
**derived on read** (`E=Z`, `L=S^V`, `U=C`). `IRET` and `SETFR`/`GETF`-write
consume **only `Z/S/C/V`**; the derived bits regenerate. So the interrupt
stack-frame ABI and `GETF` values are **unchanged** — no runtime frame edits
required.

### 1.6 Encoding is STABLE — old binaries still run
No opcode was renumbered or retired at the RTL level. `JMPE` (COND 9) still
decodes and now evaluates `Z` (= equality after `CMP`, since `CMP` now sets `Z`).
**Current ELFs run correctly on the new RTL** (being board-confirmed now). This
is deliberate: it decouples the RTL landing from the toolchain landing.

---

## 2. What the toolchain must change

### 2.1 LLVM backend — condition lowering (`KlaussCPUISelDAGToDAG.cpp`, CC tables)
- Equality now has **one** source: `Z`. Lower `EQ/NE` after *either* a `CMP` or
  an arithmetic op to `JMPZ/JMPNZ` (the `JMPE/JMPNE` alias reads the same bit).
- Signed `LT/LE/GT/GE` → COND 5/6 (+INV); unsigned `ULT/ULE/UGT/UGE` → COND 7/8
  (+INV). All are now valid after arithmetic too (since `CMP`≡`SUB` for flags),
  which is what enables flag-reuse.
- **Borrow polarity:** unsigned `<` is `C`, unsigned `≥` is `¬C` (§1.3). Pin it.

### 2.2 A3a flag-reuse peephole (`KlaussCPUFlagReuse.cpp`) — direct payoff
Its only risk was "changes which HW flag a branch reads (zero vs equal)." That
risk is **gone** (equal ≡ zero now). **Promote `cl::init(false)` → `cl::init(true)`**,
or lower the fused `arith; JMPZ/NZ` directly and drop the peephole. This is the
concrete compile-quality win the unification unlocks.

### 2.3 Carry patterns — `ADC/SBC`, rotate-through-carry
Carry is now uniformly defined (borrow convention). Add `ADC/SBC` and
`ROLC/RORC` isel patterns for i128 add/sub chains. Match the RTL semantics:
`ADC = a+b+C`, `SBC = a-b-C`, `C` after `SUB` = borrow (§1.3).

### 2.4 Assembler / encoder (`KlaussCPUMCCodeEmitter.cpp`, AsmParser)
- Encoding is stable, so you may keep every current mnemonic. Optional cleanup:
  alias `JMPE→JMPZ` / `JMPNE→JMPNZ` and stop emitting the redundant equality
  form. If you retire any mnemonic, update the emitter + mnemonic table together.

### 2.5 Emulator / golden model (`klausscc --emulate`) — REQUIRED to match
The emulator models flags too. Update it to the new contract: `CMP` sets
`Z/S/C/V`; conditions derived per §1.4 with **`ULT=C`**; `E/L/U` retired (derive
on `GETF`/frame read). Until updated, the emulator still matches the board on
*current* ELFs (bit-identical either way), but will diverge the moment you emit
new codegen (A3a on, `JMPZ`-after-`CMP`, derived-after-arith). **Update the
emulator in lockstep with the backend.**

### 2.6 Boot ROM / netboot stored program
Encoding-stable ⇒ the existing boot-ROM image and netboot payload still run. But
**any recompile must use the updated toolchain**, and you flagged the netboot
stored program specifically: regenerate it (`klausscc --mem-out` for the boot
ROM; rebuild the netboot resident image) once the backend/emulator land.

### 2.7 Regression (LLVM)
- Add `cmp-branch.ll`: pin the condition→branch mapping for every CC after both a
  `CMP` and an arithmetic producer (mirrors `tb_flags.sv` on the codegen side).
- Extend `flag-reuse.ll` for the now-default-on A3a.
- `klausscpu-runtime`: re-validate any inline asm / crt / context-switch that
  hand-reads flags or assumed `JMPE ≠ JMPZ` (should be none; the frame is
  unchanged).

---

## 3. Validation / rollout order
1. **RTL alone (now):** current ELFs on new RTL = board PASS + UART bit-identical
   to `perf/golden/` + `perf_baseline` no-HCF + CPI unchanged. (Encoding-stable,
   so this must hold with zero toolchain change.)
2. **Toolchain:** update backend + emulator + assembler together; add the LLVM
   regressions; A3a on.
3. **Recompile ELFs + boot ROM + netboot payload** with the new toolchain.
4. **Re-baseline:** re-run `--emulate` (new golden) + board; confirm bit-identical
   program UART (instruction *counts* will drop where A3a/flag-reuse fires — that's
   the win). Re-capture `perf/` with the new ELF set + record the new SHA
   (stale-ELF trap — see `perf/METHOD.md`).

## 4. RTL anchors (for reference)
`klauss_pkg.sv`: `flags_t` (~85), `f_alu` `ALU_CMP` (~295), `f_cond_eval` (~1004),
`f_iret` (~875). `KlaussCPU.sv`: `ALU_FINISH` CMP commit (~2944), IRQ save concat
(~1874), `GETF` (~2214). `uart_tasks.vh`: crash-dump flag display (~335/363/367).
Unit test: `KlaussCPU.srcs/sim_1/new/tb_flags.sv`.
