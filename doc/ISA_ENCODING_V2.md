# Klauss ISA encoding — v2 opcode map with the v3 additions

This is the **symbol → number map** of the current Klauss instruction set:
the ISA encoding v2 layout plus the ISA v3 additions (short 1-word forms,
fused compare-and-branch, ENTER/LEAVE, the 32-bit W ops). The assembler's
built-in opcode table (`src/isa/table.rs`) is this map, and it matches the
board core's decoder (`pipeline_core.sv` in the KlaussCPU RTL repo, whose own
copy of this map is `ISA_ENCODING_V2_MAP.md`). Every encoding not listed here
or in §5 traps as an illegal instruction.

`klausscc --opcodes` writes the same table (with the built-in macros) to
HTML and JSON in the current directory.

The v3 additions all reuse encodings that trapped in v2, so every v2 binary
runs unchanged on a v3 core. The flag model (`Z S C V`, which instructions
write which flags, how branch conditions are derived) is in §7.

The "v1 opcode" columns are **history**: the pre-v2 opcode each symbol
replaced. No current tool or CPU accepts v1 words (their LEN field is `00`,
which is illegal).

## 1. Word-0 layout (all instructions)

```
 31 30 29    26 25                 16 15    12 11    8 7     4 3     0
┌─────┬────────┬─────────────────────┬────────┬───────┬───────┬───────┐
│ LEN │ CLASS  │ attributes + OP     │   x    │  rd   │  rs1  │  rs2  │
└─────┴────────┴─────────────────────┴────────┴───────┴───────┴───────┘
```

- **LEN [31:30]**: `01`=1 word, `10`=2 words, `11`=3 words. `00` = illegal.
- **CLASS [29:26]**: major class, table below.
- **rd [11:8]**: destination register (loads: dest; **stores: data source**).
- **rs1 [7:4]**: first source / base address.
- **rs2 [3:0]**: second source / shift count / indirect branch target.
- **imm32**: at PC+4 (2-word); **imm64**: lo32 at PC+4, hi32 at PC+8 (3-word).
- Unused register fields and reserved bits must be **0**. The CPU checks
  them and traps otherwise. (The v3 short forms put their immediate in
  `[19:12]` or a larger field; see §3.)

The "Template" column is word 0 with all operand fields 0. To assemble:
`word0 = template | rd<<8 | rs1<<4 | rs2`, plus `N<<15` for class-4
embedded counts and the short-form fields in §3.

| CLASS | Meaning |
|---|---|
| 0x1 | ALU reg-reg |
| 0x2 | ALU immediate (incl. MOV/LEA) |
| 0x3 | compare (flag CMP + boolean) |
| 0x4 | shift / rotate / bit |
| 0x5 | unary (incl. INC/DEC/GETF) |
| 0x6 | load |
| 0x7 | store |
| 0x8 | branch / call |
| 0x9 | stack / SP / frame |
| 0xA | mul / div |
| 0xB | system |
| 0xC | I/O (LCD) |
| 0xD | fused compare-and-branch (v3) |
| 0x0, 0xE, 0xF | illegal / reserved (trap) |

## 2. Per-class attribute bits

- **Class 1/2 (ALU)**: `[25:22]` OP (0=ADD 1=SUB 2=ADC 3=SBC 4=AND 5=OR 6=XOR
  7=MIN 8=MAX 9=MINU 10=MAXU 14=LEA 15=MOV), `[21]` F (flags; must be 1 for
  ADD/SUB/ADC/SBC and 0 for the others), `[20]` SGN (class 2: sign- vs
  zero-extend the immediate; 0 in class 1). MIN/MAX are class 1 only,
  LEA/MOV class 2 only. v3: `[19]` W on class-1 ADD/SUB and the 2-word
  class-2 ADD (§3.13); class 2 with `LEN=01` is the short form (§3.10).
- **Class 3 (compare)**: `[25:23]` PRED (0=EQ 1=LT 2=LE 3=ULT 4=ULE),
  `[22]` INV, `[21]` B (0=flag-setting CMP, writes Z/S/C/V; 1=boolean
  rd=0/1), `[20]` SGN (imm form). The flag CMP must have PRED=0, INV=0,
  rd=0. v3: `[19]` W (32-bit compare; reg form and 2-word form only);
  `LEN=01 SGN=1 B=0` is the short CMPRV (§3.10).
- **Class 4 (shift/bit)**: `[25:22]` OP (0=SHL 1=SHR 2=SAR 3=ROL 4=ROR 5=RCL
  6=RCR 8=BSET 9=BCLR 10=BTGL 11=BTST 12=BEXTR 13=BDEP), `[21]` SRC
  (0=count in rs2[5:0], 1=embedded), `[20:15]` N (embedded count/position;
  must be 0 when SRC=0), `[14]` F (write Z).
- **Class 5 (unary)**: `[25:22]` OP (0=COPY 1=NEG 2=NOT 3=ABS 4=SEXT 5=ZEXT
  6=BSWAP 7=BITREV 8=POPCNT 9=CLZ 10=CTZ 12=GETF 14=INC 15=DEC),
  `[21:20]` SIZE (SEXT/ZEXT only: 00=8 01=16 10=32; 11 traps), `[19]` F.
- **Class 6/7 (load/store)**: `[25:24]` SIZE (00=8 01=16 10=32 11=64),
  `[23]` SGN (loads only), `[22:21]` MODE (00=[rs1] 01=rs1+imm32 10=[imm32]
  11=rs1+rs2), `[20]` A (64-bit only: 1 = force 8-byte alignment). Modes
  00/11 are 1 word, 01/10 are 2 words. v3: `LEN=01 MODE=01` is the short
  scaled-offset form (§3.10).
- **Class 8 (branch)**: `[25]` LINK (call), `[24]` REL (PC-relative),
  `[23]` RIND (target = rs2, LEN=01), `[22:19]` COND (0=always 1=Z 2=C 3=V
  4=S 5=LT 6=LE 7=ULT 8=ULE 9=E), `[18]` INV. COND 10-15 and INV with
  COND=0 trap. v3: `LEN=01 RIND=0 REL=1` is the short PC-relative
  branch/call (simm18 `[17:0]`, §3.11).
- **Class 9 (stack)**: `[25:22]` OP (0=PUSH 1=PUSHI 2=POP 3=GETSP 4=SETSP
  5=ADDSP 6=RET 7=IRET; v3: 8=ENTER 9=LEAVE 10=LEAVERET, §3.12).
- **Class A (mul/div)**: `[25:24]` OP (0=MUL 1=DIV 2=MOD), `[23]` SGN,
  `[22]` H (high half, MUL only). v3: `[19]` W on the low-half MUL (§3.13).
- **Class B (system)**: `[21:16]` OP (0=NOP 1=HALT 2=WAIT 3=RESET 4=TRAP
  5=DELAY).
- **Class C (I/O)**: `[25:24]` OP (0=LCDCMD 1=LCDDATA 2=LCDRESET); reg vs imm
  source selected by LEN.
- **Class D (fused branch, v3)**: `[25:23]` PRED (as class 3), `[22]` INV,
  `[21]` IMM, `[20:8]` simm13 (§3.14).

## 3. Complete symbol → number table

**Operands** is what you write after the mnemonic in klausscc source,
separated by spaces:

- `rd`, `rs1`, `rs2`: a register `A`–`P` (R0–R15), placed in that field.
- `r`: one register written **in place**. The assembler puts it in both
  `rd` and `rs1` (`ADDV A 5` is `A = A + 5`). The CPU itself has separate
  fields, so `rd ≠ rs1` is a valid encoding, but the assembler has no
  syntax for it.
- `imm32`: a number or a label (`LOOP:`), stored in the word after word 0.
  "sext"/"zext" is how the CPU extends it to 64 bits.
- `imm64`: one 64-bit literal, or two 32-bit tokens (lo then hi).
- `N`: a literal count or bit position 0–63, in `[20:15]`, written as a
  plain number (`SHLV A 5`). A `#` prefix is not accepted (`#name` is the
  data directive).
- The v3 short-form operands are described in §3.10–3.14.

**Flags** lists the flags the instruction writes (§7). "—" means none: the
flags keep their previous values.

### 3.1 ALU reg-reg (class 1) — `rd = rs1 OP rs2`, 1 word

| Symbol | Template | Operands | Flags | v1 opcode (history) |
|---|---|---|---|---|
| ADDR  | `0x4420_0000` | rd rs1 rs2 | Z,S,C,V | 0x0001_0xxx |
| SUBR  | `0x4460_0000` | rd rs1 rs2 | Z,S,C,V | 0x0002_0xxx |
| ADDC  | `0x44A0_0000` | rd rs1 rs2 | Z,S,C,V (reads C) | 0x0006_0xxx |
| SUBC  | `0x44E0_0000` | rd rs1 rs2 | Z,S,C,V (reads C) | 0x0007_0xxx |
| ANDR  | `0x4500_0000` | rd rs1 rs2 | — | 0x0003_0xxx |
| ORR   | `0x4540_0000` | rd rs1 rs2 | — | 0x0004_0xxx |
| XORR  | `0x4580_0000` | rd rs1 rs2 | — | 0x0005_0xxx |
| MINR  | `0x45C0_0000` | rd rs1 rs2 | — | 0x0040_0xxx |
| MAXR  | `0x4600_0000` | rd rs1 rs2 | — | 0x0041_0xxx |
| MINUR | `0x4640_0000` | rd rs1 rs2 | — | 0x0042_0xxx |
| MAXUR | `0x4680_0000` | rd rs1 rs2 | — | 0x0043_0xxx |

ADDC is `rs1 + rs2 + C`; SUBC is `rs1 - rs2 - C` (C is the borrow, §7).
MIN/MAX compare signed, MINU/MAXU unsigned.

### 3.2 ALU immediate (class 2), 2 words (SETR64: 3)

| Symbol | Template | Operands | Flags | v1 opcode (history) |
|---|---|---|---|---|
| ADDI   | `0x8830_0000` | rd rs1 imm32 (sext) | Z,S,C,V | 0x0000_02xx |
| ADDV   | `0x8820_0000` | r imm32 (zext) | Z,S,C,V | 0x0000_081x |
| MINUSV | `0x8860_0000` | r imm32 (zext) | Z,S,C,V | 0x0000_082x |
| ANDV   | `0x8900_0000` | r imm32 (zext) | — | 0x0000_086x |
| ORV    | `0x8940_0000` | r imm32 (zext) | — | 0x0000_087x |
| XORV   | `0x8980_0000` | r imm32 (zext) | — | 0x0000_088x |
| LEAPC  | `0x8B80_0000` | rd imm32 | — | 0x0000_099x — `rd = PC + imm32` |
| SETR   | `0x8BD0_0000` | rd imm32 (sext) | — | 0x0000_080x (MOV, SGN=1) |
| SETR64 | `0xCBC0_0000` | rd imm64 | — | 0x0000_0FEx (MOV, LEN=11) |

LEA/MOV do not read rs1 (the field is ignored).

LEAPC adds imm32 to its own address. The assembler stores the operand
unchanged, so `LEAPC A LABEL:` gives A = PC + (address of LABEL), **not**
the label's address. Give LEAPC a literal displacement; to load a label's
address, use `SETR A LABEL:`.

### 3.3 Compare (class 3)

| Symbol | Template | Operands | Flags / result | v1 opcode (history) |
|---|---|---|---|---|
| CMPRR  | `0x4C00_0000` | rs1 rs2 | Z,S,C,V of `rs1 - rs2` | 0x0000_05xx |
| CMPRV  | `0x8C10_0000` | rs1 imm32 (sext) | Z,S,C,V of `rs1 - imm` | 0x0000_083x |
| CMPEQR | `0x4C20_0000` | rd rs1 rs2 | rd=0/1, no flags | 0x0030_0xxx |
| CMPNER | `0x4C60_0000` | rd rs1 rs2 | rd=0/1, no flags | 0x0031_0xxx |
| CMPLTR | `0x4CA0_0000` | rd rs1 rs2 | rd=0/1, no flags | 0x0032_0xxx |
| CMPGER | `0x4CE0_0000` | rd rs1 rs2 | rd=0/1, no flags | 0x0035_0xxx (LT+INV) |
| CMPLER | `0x4D20_0000` | rd rs1 rs2 | rd=0/1, no flags | 0x0033_0xxx |
| CMPGTR | `0x4D60_0000` | rd rs1 rs2 | rd=0/1, no flags | 0x0034_0xxx (LE+INV) |
| CMPULTR | `0x4DA0_0000` | rd rs1 rs2 | rd=0/1, no flags | 0x0036_0xxx |
| CMPUGER | `0x4DE0_0000` | rd rs1 rs2 | rd=0/1, no flags | 0x0039_0xxx (ULT+INV) |
| CMPULER | `0x4E20_0000` | rd rs1 rs2 | rd=0/1, no flags | 0x0037_0xxx |
| CMPUGTR | `0x4E60_0000` | rd rs1 rs2 | rd=0/1, no flags | 0x0038_0xxx (ULE+INV) |

CMPRR/CMPRV are a SUB that discards its result (§7). The W (32-bit)
compares are in §3.13.

### 3.4 Shift / rotate / bit (class 4)

Register-count forms take the count or position from `rs2[5:0]`; counts
64 and up wrap modulo 64. The embedded forms (SRC=1) carry N in `[20:15]`
and are 1 word; the template is shown with N=0.

| Symbol | Template | Operands | Flags | v1 opcode (history) |
|---|---|---|---|---|
| SHLR  | `0x5000_4000` | rd rs1 rs2 | Z | 0x0020_0xxx |
| SHRR  | `0x5040_4000` | rd rs1 rs2 | Z | 0x0021_0xxx |
| SARR  | `0x5080_4000` | rd rs1 rs2 | Z | 0x0022_0xxx |
| ROLR  | `0x50C0_4000` | rd rs1 rs2 | Z | 0x0023_0xxx |
| RORR  | `0x5100_4000` | rd rs1 rs2 | Z | 0x0024_0xxx |
| SHLV  | `0x5020_4000` | r N | Z | 0x0000_091x (was 2 words) |
| SHRV  | `0x5060_4000` | r N | Z | 0x0000_092x (was 2 words) |
| SHRAV | `0x50A0_4000` | r N | Z | 0x0000_093x (was 2 words) |
| ROLV  | `0x50E0_4000` | r N | Z (N=1: Z,C) | 0x0000_0FCx; `ROLV r 1` = ROLR1 |
| RORV  | `0x5120_4000` | r N | Z (N=1: Z,C) | 0x0000_0FDx; `RORV r 1` = RORR1 |
| SHLR1 | `0x5020_8000` | r | — | 0x0000_08Dx (SHL N=1 F=0) |
| SHLAR | `0x5020_8000` | r | — | 0x0000_08Fx — **same encoding as SHLR1** |
| SHRR1 | `0x5060_8000` | r | — | 0x0000_08Ex |
| SHRAR | `0x50A0_8000` | r | — | 0x0000_090x |
| ROLR1 | `0x50E0_C000` | r | Z,C | 0x0000_0F8x (ROL N=1 F=1) |
| RORR1 | `0x5120_C000` | r | Z,C | 0x0000_0F9x |
| ROLCR | `0x5160_C000` | r | Z,C (reads C) | 0x0000_0FAx (RCL N=1) |
| RORCR | `0x51A0_C000` | r | Z,C (reads C) | 0x0000_0FBx (RCR N=1) |
| BSETRR | `0x5200_0000` | rd rs1 rs2 | — | 0x0050_0xxx |
| BCLRRR | `0x5240_0000` | rd rs1 rs2 | — | 0x0051_0xxx |
| BTGLRR | `0x5280_0000` | rd rs1 rs2 | — | 0x0052_0xxx |
| BTSTRR | `0x52C0_0000` | rd rs1 rs2 | — (rd = the bit, 0/1) | 0x0053_0xxx |
| BSET  | `0x5220_0000` | r N | — | 0x0000_0A0x (was 2 words) |
| BCLR  | `0x5260_0000` | r N | — | 0x0000_0A1x (was 2 words) |
| BTGL  | `0x52A0_0000` | r N | — | 0x0000_0A2x (was 2 words) |
| BTST  | `0x52E0_0000` | rs1 N | Z = NOT bit N (no rd) | 0x0000_0A3x (was 2 words) |
| BEXTR | `0x9300_0000` | rd rs1 imm32 | Z | 0x0000_0ACx |
| BDEP  | `0x9340_0000` | rd rs1 rs2 imm32 | — | 0x0000_0ADx |

Only a rotate by an embedded N=1 with F=1 (ROLR1/RORR1, `ROLV r 1`,
`RORV r 1`) writes C, which gets the bit rotated out. Rotating by a
register count never writes C, even when the count is 1. ROLCR/RORCR
rotate one bit through C.

**BEXTR / BDEP** take a field descriptor in imm32: `start = imm[4:0]`,
`len = imm[12:8]`. Other imm bits are ignored. A field is 0–31 bits wide,
and `len = 0` is an empty field.

- `BEXTR rd rs1 desc`: `rd = (rs1 >> start) & ((1 << len) - 1)`. The shift
  uses the **full 64-bit rs1**, so a field with `start + len > 32` takes
  bits from `rs1[63:32]`. The result is at most 31 bits, zero-extended. Z
  is **always** written from the result, even though the template's F bit
  is clear. rs2 must be 0.
- `BDEP rd rs1 rs2 desc`: deposit the low `len` bits of rs2 into the
  **low 32 bits** of rs1 (the base) at `start`. Field bits above bit 31
  are dropped. The result is those 32 bits, zero-extended (rs1[63:32] is
  not kept). No flags.

```
_start
SETR A 0x12345678
BEXTR B A 0x0804      // B = (A >> 4) & 0xFF = 0x67, Z = 0
SETR C 0xAB
BDEP D A C 0x0810     // D = A with bits [23:16] = 0xAB -> 0x12AB5678
HALT
```

### 3.5 Unary (class 5) — `rd = OP(rs1)`, 1 word

| Symbol | Template | Operands | Flags | v1 opcode (history) |
|---|---|---|---|---|
| COPY   | `0x5400_0000` | rd rs1 | — | 0x0000_01xx |
| NEGR   | `0x5448_0000` | r | Z | 0x0000_08Ax |
| NOTR   | `0x5488_0000` | r | Z | 0x0000_098x |
| ABSR   | `0x54C8_0000` | r | Z,V (V=1 only for INT_MIN) | 0x0000_08Bx |
| SEXTB  | `0x5508_0000` | r | Z,S | 0x0000_08Cx |
| SEXTH  | `0x5518_0000` | r | Z,S | 0x0000_094x |
| SEXTW  | `0x5520_0000` | r | — | 0x0000_0F0x |
| ZEXTB  | `0x5548_0000` | r | Z | 0x0000_095x |
| ZEXTH  | `0x5558_0000` | r | Z | 0x0000_096x |
| ZEXTW  | `0x5560_0000` | r | — | 0x0000_0F1x |
| BSWAP  | `0x5580_0000` | r | — | 0x0000_097x (64-bit byte swap) |
| BITREV | `0x55C0_0000` | r | — | 0x0000_0ABx (64-bit) |
| POPCNT | `0x5608_0000` | r | Z | 0x0000_0A8x |
| CLZ    | `0x5640_0000` | r | — | 0x0000_0A9x (CLZ(0) = 64) |
| CTZ    | `0x5680_0000` | r | — | 0x0000_0AAx (CTZ(0) = 64) |
| SETFR  | `0x5700_0000` | rd | — | 0x0000_089x — GETF: `rd = {Z,Z,C,V} << 60` (§7) |
| INCR   | `0x5788_0000` | r | Z,S,C,V | 0x0000_084x — `r = r + 1` |
| DECR   | `0x57C8_0000` | r | Z,S,C,V | 0x0000_085x — `r = r - 1` |

ABS of INT_MIN (`0x8000_0000_0000_0000`) returns INT_MIN and sets V.

### 3.6 Loads (class 6) — `rd = ext(mem[EA])`

| Symbol | Template | Words | Operands | EA | v1 opcode (history) |
|---|---|---|---|---|---|
| MEMGET8   | `0x5800_0000` | 1 | rd rs1 | rs1 | 0x0000_75xx |
| MEMGET16  | `0x5900_0000` | 1 | rd rs1 | rs1 | 0x0000_77xx |
| MEMGET32  | `0x5A00_0000` | 1 | rd rs1 | rs1 (any alignment) | 0x0000_79xx |
| MEMREADRR | `0x5B00_0000` | 1 | rd rs1 | rs1 (A=0) | 0x0000_71xx |
| MEMGET64  | `0x5B10_0000` | 1 | rd rs1 | rs1 (A=1, `& ~7`) | 0x0000_7Bxx |
| LDIDX8    | `0x9820_0000` | 2 | rd rs1 imm32 | rs1+imm32 | 0x0000_C4xx |
| LDIDX8_S  | `0x98A0_0000` | 2 | rd rs1 imm32 | rs1+imm32, sext | 0x0000_C6xx |
| LDIDX16   | `0x9920_0000` | 2 | rd rs1 imm32 | rs1+imm32 | 0x0000_C2xx |
| LDIDX16_S | `0x99A0_0000` | 2 | rd rs1 imm32 | rs1+imm32, sext | 0x0000_C7xx |
| LDIDX32   | `0x9A20_0000` | 2 | rd rs1 imm32 | rs1+imm32 | 0x0000_C0xx |
| LDIDX32_S | `0x9AA0_0000` | 2 | rd rs1 imm32 | rs1+imm32, sext | — (v3 D1) |
| LDIDX64   | `0x9B20_0000` | 2 | rd rs1 imm32 | rs1+imm32 (A=0) | 0x0000_0Cxx |
| LDIDX64A  | `0x9B30_0000` | 2 | rd rs1 imm32 | rs1+imm32 (A=1) | 0x0000_FCxx |
| MEMREADR  | `0x9B40_0000` | 2 | rd imm32 | imm32 absolute | 0x0000_721x |
| LDIDX64R  | `0x5B60_0000` | **1** | rd rs1 rs2 | rs1+rs2 | 0x0000_0Exx (was 2 words) |

Loads write no flags. 16- and 32-bit loads align their address down to
the access size (`& ~1`, `& ~3`), except **MEMGET32**, which reads the 4
bytes at any address, even across a doubleword boundary. A 64-bit access
with A=1 is forced to `& ~7`. With A=0 the core sends the address as is,
but the data cache picks the doubleword from address bits `[4:3]`, so in
practice every 64-bit RAM access reads or writes the aligned doubleword
(the emulator does the same; see `EMULATOR_ISA_SEMANTICS.md`).

### 3.7 Stores (class 7) — `mem[EA] = rd` (the rd field is the data source)

| Symbol | Template | Words | Operands | EA | v1 opcode (history) |
|---|---|---|---|---|---|
| MEMSET8    | `0x5C00_0000` | 1 | rd rs1 | rs1 | 0x0000_74xx |
| MEMSET16   | `0x5D00_0000` | 1 | rd rs1 | rs1 | 0x0000_76xx |
| MEMSET32   | `0x5E00_0000` | 1 | rd rs1 | rs1 | 0x0000_78xx |
| MEMSET64RR | `0x5F00_0000` | 1 | rd rs1 | rs1 (A=0) | 0x0000_70xx |
| MEMSET64   | `0x5F10_0000` | 1 | rd rs1 | rs1 (A=1) | 0x0000_7Axx |
| STIDX8     | `0x9C20_0000` | 2 | rd rs1 imm32 | rs1+imm32 | 0x0000_C5xx |
| STIDX16    | `0x9D20_0000` | 2 | rd rs1 imm32 | rs1+imm32 | 0x0000_C3xx |
| STIDX32    | `0x9E20_0000` | 2 | rd rs1 imm32 | rs1+imm32 | 0x0000_C1xx |
| STIDX64    | `0x9F20_0000` | 2 | rd rs1 imm32 | rs1+imm32 (A=0) | 0x0000_0Dxx |
| STIDX64A   | `0x9F30_0000` | 2 | rd rs1 imm32 | rs1+imm32 (A=1) | 0x0000_FDxx |
| MEMSETR    | `0x9F40_0000` | 2 | rd imm32 | imm32 absolute | 0x0000_720x |
| STIDX64R   | `0x5F60_0000` | **1** | rd rs1 rs2 | rs1+rs2 | 0x0000_73xx (was 2 words) |

The first operand is the **value**, the second the **address**:
`MEMSET8 C B` stores the low byte of C at `[B]`. Sub-word stores align
down to their size. Stores write no flags.

### 3.8 Branch / call (class 8)

The immediate forms are 2 words, with the absolute target in imm32
(usually a label). The register forms are 1 word, with the target in rs2.
No flags are written; the condition is evaluated as in §7.

| Symbol | Template | Symbol | Template |
|---|---|---|---|
| JMP     | `0xA000_0000` | CALL    | `0xA200_0000` |
| JMPZ    | `0xA008_0000` | CALLZ   | `0xA208_0000` |
| JMPNZ   | `0xA00C_0000` | CALLNZ  | `0xA20C_0000` |
| JMPC    | `0xA010_0000` | CALLC   | `0xA210_0000` |
| JMPNC   | `0xA014_0000` | CALLNC  | `0xA214_0000` |
| JMPO    | `0xA018_0000` | CALLO   | `0xA218_0000` |
| JMPNO   | `0xA01C_0000` | CALLNO  | `0xA21C_0000` |
| JMPS    | `0xA020_0000` | CALLE   | `0xA248_0000` |
| JMPNS   | `0xA024_0000` | CALLNE  | `0xA24C_0000` |
| JMPLT   | `0xA028_0000` | JMPR    | `0x6080_0000` (rs2) |
| JMPGE   | `0xA02C_0000` | CALLR   | `0x6280_0000` (rs2) |
| JMPLE   | `0xA030_0000` | | |
| JMPGT   | `0xA034_0000` | | |
| JMPULT  | `0xA038_0000` | | |
| JMPUGE  | `0xA03C_0000` | | |
| JMPULE  | `0xA040_0000` | | |
| JMPUGT  | `0xA044_0000` | | |
| JMPE    | `0xA048_0000` | | |
| JMPNE   | `0xA04C_0000` | | |

A call pushes the zero-extended return address (the next instruction:
PC+8 for the 2-word forms, PC+4 for CALLR) and only when it is taken.
JMPE/JMPNE test the same Z flag as JMPZ/JMPNZ.

**2-word PC-relative forms (CPU only).** REL=1 with LEN=10 gives
`target = PC + imm32`, for example `0xA100_0000` (JMP rel) and
`0xA300_0000` (CALL rel), with any COND. The CPU and the emulator execute
them, but **klausscc has no mnemonics** for them: the old JMPREL/CALLREL
family was not carried into the assembler's v2 table. For PC-relative code,
use the v3 short forms in §3.11.

(v1 history: JMP family 0x0000_1000-101C, REL 0x0000_1030-1040, JMPR
0x0000_102x, CALLR 0x0000_407x.)

### 3.9 Mul / div (class A), stack / SP (class 9), system (class B), I/O (class C)

| Symbol | Template | Words | Operands | Flags | v1 opcode (history) |
|---|---|---|---|---|---|
| MULUR  | `0x6800_0000` | 1 | rd rs1 rs2 | Z,S,V | 0x0011_0xxx |
| MULHUR | `0x6840_0000` | 1 | rd rs1 rs2 | Z,S,V (V=0) | 0x0013_0xxx |
| MULR   | `0x6880_0000` | 1 | rd rs1 rs2 | Z,S,V | 0x0010_0xxx |
| MULHR  | `0x68C0_0000` | 1 | rd rs1 rs2 | Z,S,V (V=0) | 0x0012_0xxx |
| MULV   | `0xA880_0000` | 2 | r imm32 (sext) | Z,S,V | 0x0000_0B8x |
| DIVUR  | `0x6900_0000` | 1 | rd rs1 rs2 | Z,V | 0x0015_0xxx |
| DIVR   | `0x6980_0000` | 1 | rd rs1 rs2 | Z,V | 0x0014_0xxx |
| DIVV   | `0xA980_0000` | 2 | r imm32 (sext) | Z,V | 0x0000_0B9x |
| MODUR  | `0x6A00_0000` | 1 | rd rs1 rs2 | Z,V | 0x0017_0xxx |
| MODR   | `0x6A80_0000` | 1 | rd rs1 rs2 | Z,V | 0x0016_0xxx |
| MODV   | `0xAA80_0000` | 2 | r imm32 (sext) | Z,V | 0x0000_0BAx |

The `…U…` forms are unsigned; the others are signed, and their immediates
are sign-extended. MULH* return the high 64 bits of the 128-bit product.
Divide by zero does not trap (§7.2).

| Symbol | Template | Words | Operands | v1 opcode (history) |
|---|---|---|---|---|
| PUSH    | `0x6400_0000` | 1 | rs1 | 0x0000_400x |
| PUSHV   | `0xA440_0000` | 2 | imm32 (zext) | 0x0000_4020 |
| PUSHV64 | `0xE440_0000` | 3 | imm64 | 0x0000_4060 |
| POP     | `0x6480_0000` | 1 | rd | 0x0000_401x |
| GETSP   | `0x64C0_0000` | 1 | rd | 0x0000_403x |
| SETSP   | `0x6500_0000` | 1 | rs1 | 0x0000_404x |
| ADDSP   | `0xA540_0000` | 2 | imm32 (sext) | 0x0000_4050 |
| RET     | `0x6580_0000` | 1 | — | 0x0000_1012 |
| IRET    | `0x65C0_0000` | 1 | — (restores Z,S,C,V and INT_MASK) | 0x0000_6011 |
| NOP     | `0x6C00_0000` | 1 | — | 0x0000_F010 |
| HALT    | `0x6C01_0000` | 1 | — | 0x0000_F011 |
| WAIT    | `0x6C02_0000` | 1 | — | 0x0000_6012 |
| RESET   | `0x6C03_0000` | 1 | — (PC = 0x4) | 0x0000_F012 |
| TRAP    | `0x6C04_0000` | 1 | — | 0x0000_F014 |
| DELAYR  | `0x6C05_0000` | 1 | rs1 | 0x0000_F00x |
| DELAYV  | `0xAC05_0000` | 2 | imm32 | 0x0000_F013 |
| LCDCMDR  | `0x7000_0000` | 1 | rs1 | 0x0000_200x |
| LCDDATAR | `0x7100_0000` | 1 | rs1 | 0x0000_201x |
| LCDCMDV  | `0xB000_0000` | 2 | imm32 | 0x0000_2021 |
| LCDDATAV | `0xB100_0000` | 2 | imm32 | 0x0000_2022 |
| LCDRST   | `0xB200_0000` | 2 | imm32 (bit 0) | 0x0000_2023 |

SP is a separate 32-bit register, not one of R0–R15. Every stack slot is
8 bytes, and the stack grows down. RET and IRET take the PC from bits
`[31:0]` of the popped slot.

### 3.10 v3 short 1-word immediates

A short form is its 2-word form with `LEN=01` and the immediate moved into
word 0 `[19:12]`. It means exactly the same as the 2-word form with that
immediate. The assembler accepts **literal numbers only** (no labels) and
reports an error if the value does not fit.

| Symbol | Template | Operands | Immediate |
|---|---|---|---|
| SETR.S   | `0x4BD0_0000` | rd imm8 | sign-extended |
| ADDI.S   | `0x4830_0000` | rd rs1 imm8 | sign-extended |
| ADDV.S   | `0x4820_0000` | r imm8 | zero-extended |
| MINUSV.S | `0x4860_0000` | r imm8 | zero-extended |
| ANDV.S   | `0x4900_0000` | r imm8 | zero-extended |
| ORV.S    | `0x4940_0000` | r imm8 | zero-extended |
| XORV.S   | `0x4980_0000` | r imm8 | zero-extended |
| CMPRV.S  | `0x4C10_0000` | rs1 imm8 | sign-extended (always) |

For every imm8 operand the assembler accepts −128..255 and stores the low
8 bits. The instruction's SGN bit then decides how the CPU extends them, so
**on a sign-extending form 128..255 become −128..−1** (`SETR.S A 200` sets
A to −56), and on a zero-extending form −1 becomes 255. Use the 2-word form
for values outside the form's own range. Flags are as for the 2-word form.

The short loads and stores take a **byte offset**. The offset must be a
multiple of the access size, and offset / size must fit in a signed 8-bit
value. EA = `rs1 + (simm8 << SIZE)`. For example, 64-bit offsets reach
−1024..1016.

| Symbol | Template | Operands | Offset range (bytes) |
|---|---|---|---|
| LDIDX8.S    | `0x5820_0000` | rd rs1 off | −128..127 |
| LDIDX8_S.S  | `0x58A0_0000` | rd rs1 off | −128..127 (sext load) |
| LDIDX16.S   | `0x5920_0000` | rd rs1 off | −256..254, even |
| LDIDX16_S.S | `0x59A0_0000` | rd rs1 off | −256..254, even (sext load) |
| LDIDX32.S   | `0x5A20_0000` | rd rs1 off | −512..508, ×4 |
| LDIDX32_S.S | `0x5AA0_0000` | rd rs1 off | −512..508, ×4 (sext load) |
| LDIDX64.S   | `0x5B20_0000` | rd rs1 off | −1024..1016, ×8 |
| LDIDX64A.S  | `0x5B30_0000` | rd rs1 off | −1024..1016, ×8 (A=1) |
| STIDX8.S    | `0x5C20_0000` | rd rs1 off | −128..127 |
| STIDX16.S   | `0x5D20_0000` | rd rs1 off | −256..254, even |
| STIDX32.S   | `0x5E20_0000` | rd rs1 off | −512..508, ×4 |
| STIDX64.S   | `0x5F20_0000` | rd rs1 off | −1024..1016, ×8 |
| STIDX64A.S  | `0x5F30_0000` | rd rs1 off | −1024..1016, ×8 (A=1) |

```
_start
SETR.S A -5            // 0x4BDFB000
CMPRV.S A -3           // 0x4C1FD000
SETR B 0x1000
LDIDX64A.S C B 16      // 0x5B302210: C = mem64[B + 16]
STIDX32.S A B -4       // mem32[B - 4] = A
HALT
```

### 3.11 v3 short PC-relative branch / call (class 8, LEN=01, REL=1, RIND=0)

`word0 = 0x6100_0000 | LINK<<25 | COND<<19 | INV<<18 | simm18`, and the
target is `PC + 4·simm18` (±512 KB). The operand is a label or an address.
The assembler works out the displacement and reports an error if the
target is out of range or not word aligned. CALL.S pushes PC+4. No flags
are written.

| Symbol | Template | Symbol | Template |
|---|---|---|---|
| JMP.S    | `0x6100_0000` | JMPLE.S  | `0x6130_0000` |
| JMPZ.S   | `0x6108_0000` | JMPGT.S  | `0x6134_0000` |
| JMPNZ.S  | `0x610C_0000` | JMPULT.S | `0x6138_0000` |
| JMPC.S   | `0x6110_0000` | JMPUGE.S | `0x613C_0000` |
| JMPNC.S  | `0x6114_0000` | JMPULE.S | `0x6140_0000` |
| JMPO.S   | `0x6118_0000` | JMPUGT.S | `0x6144_0000` |
| JMPNO.S  | `0x611C_0000` | JMPE.S   | `0x6148_0000` |
| JMPS.S   | `0x6120_0000` | JMPNE.S  | `0x614C_0000` |
| JMPNS.S  | `0x6124_0000` | CALL.S   | `0x6300_0000` |
| JMPLT.S  | `0x6128_0000` | JMPGE.S  | `0x612C_0000` |

The CPU also accepts conditional short calls (LINK=1 with any COND), but
the assembler only has the unconditional CALL.S.

### 3.12 v3 frame instructions (class 9, LEN=01)

R15 is the frame pointer. It is named implicitly; there are no register
operands.

| Symbol | Template | Operands | Semantics |
|---|---|---|---|
| ENTER    | `0x6600_0000` | N (0..2²²−1, doublewords) | push R15; R15 = SP; SP −= 8·N |
| LEAVE    | `0x6640_0000` | — | SP = R15; pop R15 |
| LEAVERET | `0x6680_0000` | — | SP = R15; pop R15; pop PC (= LEAVE; RET) |

```
_start
CALL FN:
HALT
FN:
ENTER 2                // frame with 16 bytes of locals
STIDX64.S A P -8       // a local at [R15 - 8]
LEAVERET
```

### 3.13 v3 32-bit (W) operations

W `[19]` makes an operation 32-bit:

- The ALU ops (ADDW, SUBW, ADDIW, MULW) compute the 64-bit result and keep
  its low 32 bits, sign-extended. Z follows that W result; S, C and V are
  those of the 64-bit operation.
- The compares (CMPRRW, CMPRVW) compare the low 32 bits of both operands
  and set Z/S/C/V exactly as a 32-bit subtract.

| Symbol | Template | Words | Operands | Flags |
|---|---|---|---|---|
| ADDW   | `0x4428_0000` | 1 | rd rs1 rs2 | Z,S,C,V |
| SUBW   | `0x4468_0000` | 1 | rd rs1 rs2 | Z,S,C,V |
| ADDIW  | `0x8838_0000` | 2 | rd rs1 imm32 (sext) | Z,S,C,V |
| MULW   | `0x6888_0000` | 1 | rd rs1 rs2 | Z,S,V |
| CMPRRW | `0x4C08_0000` | 1 | rs1 rs2 | Z,S,C,V |
| CMPRVW | `0x8C18_0000` | 2 | rs1 imm32 (sext) | Z,S,C,V |

W is legal only on these (and on the boolean compares, which have no
klausscc mnemonic). The short forms use `[19:12]` for their immediate, so
they have no W.

### 3.14 v3 fused compare-and-branch (class D, LEN=01)

`word0 = 0x7400_0000 | PRED<<23 | INV<<22 | IMM<<21 | simm13<<8 | rs1<<4 | rs2/simm4`

If `(rs1 PRED rhs) XOR INV`, the branch goes to `PC + 4·simm13` (±16 KB).
`rhs` is rs2, or for the `…I` forms a literal −8..7 (sign-extended). The
compare is on the full 64-bit values. **Flags are not read or written.**
The target is a label or address, and the assembler reports an error if it
is out of range.

| Symbol | Template | Symbol | Template | Branch if |
|---|---|---|---|---|
| BEQ  | `0x7400_0000` | BEQI  | `0x7420_0000` | rs1 == rhs |
| BNE  | `0x7440_0000` | BNEI  | `0x7460_0000` | rs1 != rhs |
| BLT  | `0x7480_0000` | BLTI  | `0x74A0_0000` | signed < |
| BGE  | `0x74C0_0000` | BGEI  | `0x74E0_0000` | signed >= |
| BLE  | `0x7500_0000` | BLEI  | `0x7520_0000` | signed <= |
| BGT  | `0x7540_0000` | BGTI  | `0x7560_0000` | signed > |
| BULT | `0x7580_0000` | BULTI | `0x75A0_0000` | unsigned < |
| BUGE | `0x75C0_0000` | BUGEI | `0x75E0_0000` | unsigned >= |
| BULE | `0x7600_0000` | BULEI | `0x7620_0000` | unsigned <= |
| BUGT | `0x7640_0000` | BUGTI | `0x7660_0000` | unsigned > |

```
_start
SETR.S A 0
SETR.S B 10
LOOP:
INCR A
BNE A B LOOP:          // loop until A == 10
BNEI A -2 DONE:        // A != -2, so this is taken
NOP
DONE:
HALT
```

Class D can be left out of the RTL with its `EN_FBR` switch, which is on
in the current core. A core built without it traps these instructions.

## 4. History: v1 → v2 migration notes

These notes describe the v2 flag day. They are kept for reading old code
and old discussions; the current rules are in §1–§3 and §7.

1. **Register fields moved.** Everything is now rd=[11:8], rs1=[7:4],
   rs2=[3:0]. v1's legacy forms packed operands differently per opcode
   (see the "v1 opcode" columns). Stores read their **data from the rd
   field**.
2. **In-place forms are 3-operand encodings.** ADDV/ANDV/ORV/XORV/MINUSV/
   INCR/DECR/NEGR/shift-by-imm/etc. are encoded with rd=rs1, which is what
   the assembler emits.
3. **Length changes.** Shift/rotate/bit with an embedded constant
   (SHLV/SHRV/SHRAV/ROLV/RORV/BSET/BCLR/BTGL/BTST) became **1 word**
   (count/position at word0[20:15]). LDIDX64R/STIDX64R became **1 word**
   (offset register in rs2). Everything else kept its v1 length.
4. **Aliases.** SHLAR ≡ SHLR1 (one encoding). `ROLV r 1` ≡ ROLR1 and
   `RORV r 1` ≡ RORR1.
5. **Immediate extension** is explicit per encoding (the SGN bit):
   ADDI/CMPRV/SETR/MULV/DIVV/MODV/ADDSP sign-extend; ADDV/MINUSV/ANDV/
   ORV/XORV/PUSHV zero-extend. This kept v1 behaviour.
6. **Old binaries fail fast**: any v1 word 0 has bits [31:30] = 00, so
   LEN=00 traps on the first instruction.
7. **Retired v1 opcodes** that became MMIO and were not re-encoded:
   LED/7-seg/RGB (0x3000/0x306x/0x307x), TXMEM/TXSTRMEM (0x5002/0x5003),
   the UART ops.
8. **What changed after the v2 checkpoint.** The first v2 build kept v1
   semantics exactly. Since then the flags were unified to `Z S C V`, so
   CMP now drives Z and JMPE ≡ JMPZ (§7); the field-driven decode made the
   combinations in §5 legal; and the v3 forms in §3.10–3.14 were added.
   These v1 quirks remain: BTST-imm writes only Z while BTSTRR writes rd;
   GETF shows only Z/E/C/V; a rotate by any count other than an embedded
   N=1 does not write C.

## 5. Field-driven decode — legal encodings without a klausscc mnemonic

The decoder is field-driven (each attribute bit selects behaviour in a
shared datapath), so these combinations are **architecturally legal**.
LLVM can emit them. klausscc has no mnemonics for them. The emulator
executes them as the RTL does (see `EMULATOR_ISA_SEMANTICS.md`).
Everything not listed here and not in §3 traps, in the RTL and the
emulator alike.

- **ALU-imm (class 2)**: ADC/SBC with imm32; both SGN values on every OP
  (e.g. sign-extended SUB-imm, zero-extended MOV).
- **Compare (class 3)**: boolean compare-immediate (B=1, LEN=10, rd=0/1),
  either SGN; W on the boolean forms.
- **Shift/bit (class 4)**: the F bit is honoured (Z is written when F=1)
  for SHL/SHR/SAR/ROL/ROR/BSET/BCLR/BTGL/BTSTRR, with any embedded N.
  ROL/ROR with SRC=1, N=1, F=1 is the rotate-by-1 that also writes C;
  RCL/RCR exist only in that shape.
- **Unary (class 5)**: F honoured for COPY/NEG/NOT/ZEXT/BSWAP/BITREV/
  POPCNT/CLZ/CTZ/GETF; SEXT with F=1 at all three sizes (Z+S). ABS/INC/DEC
  require F=1.
- **Loads (class 6)**: the full SIZE × SGN × MODE × A space (A: 64-bit
  only), e.g. MEMGET8_S/16_S, sub-word reg+reg (mode 11) and absolute
  (mode 10) forms. SIZE=32 with MODE=00 is MEMGET32 and needs SGN=0.
- **Stores (class 7)**: the full SIZE × MODE space (SGN must be 0):
  sub-word reg+reg and absolute forms.
- **Mul/div (class A)**: MULHV/MULUV/MULHUV, DIVUV/MODUV; the immediate
  forms extend per the SGN bit.
- **Branches (class 8)**: the 2-word PC-relative jumps and calls (§3.8)
  with any COND; conditional register-indirect jumps/calls (RIND=1, any
  COND); conditional short calls (§3.11). COND 10–15 and INV with COND=0
  trap.

## 6. Machine-readable table

Every mnemonic the assembler accepts, in its table order. `regs` lists the
word-0 register fields an instruction encodes; for an in-place operand
(`r` in §3) that is `rd,rs1` with one source token. `flags` uses Z S C V.

```csv
symbol,template,words,class,regs,imm,flags,notes
ADDR,0x44200000,1,1,"rd,rs1,rs2",,ZSCV,
SUBR,0x44600000,1,1,"rd,rs1,rs2",,ZSCV,
ADDC,0x44A00000,1,1,"rd,rs1,rs2",,ZSCV,reads C
SUBC,0x44E00000,1,1,"rd,rs1,rs2",,ZSCV,reads C (borrow)
ANDR,0x45000000,1,1,"rd,rs1,rs2",,,
ORR,0x45400000,1,1,"rd,rs1,rs2",,,
XORR,0x45800000,1,1,"rd,rs1,rs2",,,
MINR,0x45C00000,1,1,"rd,rs1,rs2",,,signed
MAXR,0x46000000,1,1,"rd,rs1,rs2",,,signed
MINUR,0x46400000,1,1,"rd,rs1,rs2",,,unsigned
MAXUR,0x46800000,1,1,"rd,rs1,rs2",,,unsigned
ADDI,0x88300000,2,2,"rd,rs1",sext32,ZSCV,
ADDV,0x88200000,2,2,"rd,rs1",zext32,ZSCV,in place (rd=rs1)
MINUSV,0x88600000,2,2,"rd,rs1",zext32,ZSCV,in place (rd=rs1)
ANDV,0x89000000,2,2,"rd,rs1",zext32,,in place (rd=rs1)
ORV,0x89400000,2,2,"rd,rs1",zext32,,in place (rd=rs1)
XORV,0x89800000,2,2,"rd,rs1",zext32,,in place (rd=rs1)
LEAPC,0x8B800000,2,2,rd,pc32,,rd=PC+imm32
SETR,0x8BD00000,2,2,rd,sext32,,
SETR64,0xCBC00000,3,2,rd,imm64,,
CMPRR,0x4C000000,1,3,"rs1,rs2",,ZSCV,SUB without writeback
CMPRV,0x8C100000,2,3,rs1,sext32,ZSCV,SUB without writeback
CMPEQR,0x4C200000,1,3,"rd,rs1,rs2",,,rd=0/1
CMPNER,0x4C600000,1,3,"rd,rs1,rs2",,,rd=0/1
CMPLTR,0x4CA00000,1,3,"rd,rs1,rs2",,,rd=0/1
CMPGER,0x4CE00000,1,3,"rd,rs1,rs2",,,rd=0/1
CMPLER,0x4D200000,1,3,"rd,rs1,rs2",,,rd=0/1
CMPGTR,0x4D600000,1,3,"rd,rs1,rs2",,,rd=0/1
CMPULTR,0x4DA00000,1,3,"rd,rs1,rs2",,,rd=0/1
CMPUGER,0x4DE00000,1,3,"rd,rs1,rs2",,,rd=0/1
CMPULER,0x4E200000,1,3,"rd,rs1,rs2",,,rd=0/1
CMPUGTR,0x4E600000,1,3,"rd,rs1,rs2",,,rd=0/1
SHLR,0x50004000,1,4,"rd,rs1,rs2",,Z,count=rs2[5:0]
SHRR,0x50404000,1,4,"rd,rs1,rs2",,Z,count=rs2[5:0]
SARR,0x50804000,1,4,"rd,rs1,rs2",,Z,count=rs2[5:0]
ROLR,0x50C04000,1,4,"rd,rs1,rs2",,Z,count=rs2[5:0]; never writes C
RORR,0x51004000,1,4,"rd,rs1,rs2",,Z,count=rs2[5:0]; never writes C
SHLV,0x50204000,1,4,"rd,rs1",N<<15,Z,in place
SHRV,0x50604000,1,4,"rd,rs1",N<<15,Z,in place
SHRAV,0x50A04000,1,4,"rd,rs1",N<<15,Z,in place
ROLV,0x50E04000,1,4,"rd,rs1",N<<15,Z,in place; N=1 also writes C (=ROLR1)
RORV,0x51204000,1,4,"rd,rs1",N<<15,Z,in place; N=1 also writes C (=RORR1)
SHLR1,0x50208000,1,4,"rd,rs1",,,in place; N=1 fixed
SHLAR,0x50208000,1,4,"rd,rs1",,,same encoding as SHLR1
SHRR1,0x50608000,1,4,"rd,rs1",,,in place; N=1 fixed
SHRAR,0x50A08000,1,4,"rd,rs1",,,in place; N=1 fixed
ROLR1,0x50E0C000,1,4,"rd,rs1",,ZC,in place; N=1 fixed
RORR1,0x5120C000,1,4,"rd,rs1",,ZC,in place; N=1 fixed
ROLCR,0x5160C000,1,4,"rd,rs1",,ZC,in place; rotate left through C
RORCR,0x51A0C000,1,4,"rd,rs1",,ZC,in place; rotate right through C
BSETRR,0x52000000,1,4,"rd,rs1,rs2",,,pos=rs2[5:0]
BCLRRR,0x52400000,1,4,"rd,rs1,rs2",,,pos=rs2[5:0]
BTGLRR,0x52800000,1,4,"rd,rs1,rs2",,,pos=rs2[5:0]
BTSTRR,0x52C00000,1,4,"rd,rs1,rs2",,,rd=bit value
BSET,0x52200000,1,4,"rd,rs1",N<<15,,in place
BCLR,0x52600000,1,4,"rd,rs1",N<<15,,in place
BTGL,0x52A00000,1,4,"rd,rs1",N<<15,,in place
BTST,0x52E00000,1,4,rs1,N<<15,Z,Z=NOT bit; no rd
BEXTR,0x93000000,2,4,"rd,rs1",params32,Z,start[4:0] len[12:8]; full 64-bit rs1; Z always
BDEP,0x93400000,2,4,"rd,rs1,rs2",params32,,base=rs1 src=rs2; low 32 bits zero-extended
COPY,0x54000000,1,5,"rd,rs1",,,
NEGR,0x54480000,1,5,"rd,rs1",,Z,in place
NOTR,0x54880000,1,5,"rd,rs1",,Z,in place
ABSR,0x54C80000,1,5,"rd,rs1",,ZV,in place; V=1 for INT_MIN
SEXTB,0x55080000,1,5,"rd,rs1",,ZS,in place
SEXTH,0x55180000,1,5,"rd,rs1",,ZS,in place
SEXTW,0x55200000,1,5,"rd,rs1",,,in place
ZEXTB,0x55480000,1,5,"rd,rs1",,Z,in place
ZEXTH,0x55580000,1,5,"rd,rs1",,Z,in place
ZEXTW,0x55600000,1,5,"rd,rs1",,,in place
BSWAP,0x55800000,1,5,"rd,rs1",,,in place
BITREV,0x55C00000,1,5,"rd,rs1",,,in place
POPCNT,0x56080000,1,5,"rd,rs1",,Z,in place
CLZ,0x56400000,1,5,"rd,rs1",,,in place
CTZ,0x56800000,1,5,"rd,rs1",,,in place
SETFR,0x57000000,1,5,rd,,,GETF: rd={Z Z C V}<<60
INCR,0x57880000,1,5,"rd,rs1",,ZSCV,in place; rd=rs1+1
DECR,0x57C80000,1,5,"rd,rs1",,ZSCV,in place; rd=rs1-1
MEMGET8,0x58000000,1,6,"rd,rs1",,,
MEMGET16,0x59000000,1,6,"rd,rs1",,,
MEMGET32,0x5A000000,1,6,"rd,rs1",,,any alignment
MEMREADRR,0x5B000000,1,6,"rd,rs1",,,64-bit A=0
MEMGET64,0x5B100000,1,6,"rd,rs1",,,64-bit A=1
LDIDX8,0x98200000,2,6,"rd,rs1",off32,,
LDIDX8_S,0x98A00000,2,6,"rd,rs1",off32,,sext
LDIDX16,0x99200000,2,6,"rd,rs1",off32,,
LDIDX16_S,0x99A00000,2,6,"rd,rs1",off32,,sext
LDIDX32,0x9A200000,2,6,"rd,rs1",off32,,
LDIDX64,0x9B200000,2,6,"rd,rs1",off32,,A=0
LDIDX64A,0x9B300000,2,6,"rd,rs1",off32,,A=1
MEMREADR,0x9B400000,2,6,rd,addr32,,
LDIDX64R,0x5B600000,1,6,"rd,rs1,rs2",,,EA=rs1+rs2
MEMSET8,0x5C000000,1,7,"rd,rs1",,,data=rd
MEMSET16,0x5D000000,1,7,"rd,rs1",,,data=rd
MEMSET32,0x5E000000,1,7,"rd,rs1",,,data=rd
MEMSET64RR,0x5F000000,1,7,"rd,rs1",,,data=rd; A=0
MEMSET64,0x5F100000,1,7,"rd,rs1",,,data=rd; A=1
STIDX8,0x9C200000,2,7,"rd,rs1",off32,,data=rd
STIDX16,0x9D200000,2,7,"rd,rs1",off32,,data=rd
STIDX32,0x9E200000,2,7,"rd,rs1",off32,,data=rd
STIDX64,0x9F200000,2,7,"rd,rs1",off32,,data=rd; A=0
STIDX64A,0x9F300000,2,7,"rd,rs1",off32,,data=rd; A=1
MEMSETR,0x9F400000,2,7,rd,addr32,,data=rd
STIDX64R,0x5F600000,1,7,"rd,rs1,rs2",,,data=rd; EA=rs1+rs2
LDIDX32_S,0x9AA00000,2,6,"rd,rs1",off32,,sext (v3 D1)
ADDW,0x44280000,1,1,"rd,rs1,rs2",,ZSCV,sext32 result (v3 D3)
SUBW,0x44680000,1,1,"rd,rs1,rs2",,ZSCV,sext32 result (v3 D3)
MULW,0x68880000,1,10,"rd,rs1,rs2",,ZSV,low half sext32 (v3 D3)
ADDIW,0x88380000,2,2,"rd,rs1",sext32,ZSCV,sext32 result (v3 D3)
CMPRRW,0x4C080000,1,3,"rs1,rs2",,ZSCV,32-bit compare (v3 D2)
CMPRVW,0x8C180000,2,3,rs1,sext32,ZSCV,32-bit compare (v3 D2)
SETR.S,0x4BD00000,1,2,rd,sext8[19:12],,v3 A5
ADDI.S,0x48300000,1,2,"rd,rs1",sext8[19:12],ZSCV,v3 A5
ADDV.S,0x48200000,1,2,"rd,rs1",zext8[19:12],ZSCV,v3 A5; in place
MINUSV.S,0x48600000,1,2,"rd,rs1",zext8[19:12],ZSCV,v3 A5; in place
ANDV.S,0x49000000,1,2,"rd,rs1",zext8[19:12],,v3 A5; in place
ORV.S,0x49400000,1,2,"rd,rs1",zext8[19:12],,v3 A5; in place
XORV.S,0x49800000,1,2,"rd,rs1",zext8[19:12],,v3 A5; in place
CMPRV.S,0x4C100000,1,3,rs1,sext8[19:12],ZSCV,v3 A3
LDIDX8.S,0x58200000,1,6,"rd,rs1",simm8<<0,,v3 A4
LDIDX8_S.S,0x58A00000,1,6,"rd,rs1",simm8<<0,,v3 A4; sext
LDIDX16.S,0x59200000,1,6,"rd,rs1",simm8<<1,,v3 A4
LDIDX16_S.S,0x59A00000,1,6,"rd,rs1",simm8<<1,,v3 A4; sext
LDIDX32.S,0x5A200000,1,6,"rd,rs1",simm8<<2,,v3 A4
LDIDX32_S.S,0x5AA00000,1,6,"rd,rs1",simm8<<2,,v3 A4; sext
LDIDX64.S,0x5B200000,1,6,"rd,rs1",simm8<<3,,v3 A4; A=0
LDIDX64A.S,0x5B300000,1,6,"rd,rs1",simm8<<3,,v3 A4; A=1
STIDX8.S,0x5C200000,1,7,"rd,rs1",simm8<<0,,v3 A4; data=rd
STIDX16.S,0x5D200000,1,7,"rd,rs1",simm8<<1,,v3 A4; data=rd
STIDX32.S,0x5E200000,1,7,"rd,rs1",simm8<<2,,v3 A4; data=rd
STIDX64.S,0x5F200000,1,7,"rd,rs1",simm8<<3,,v3 A4; data=rd; A=0
STIDX64A.S,0x5F300000,1,7,"rd,rs1",simm8<<3,,v3 A4; data=rd; A=1
ENTER,0x66000000,1,9,,N[21:0],,v3 C; push R15; R15=SP; SP-=8N
LEAVE,0x66400000,1,9,,,,v3 C; SP=R15; pop R15
LEAVERET,0x66800000,1,9,,,,v3 C; LEAVE; RET
JMP.S,0x61000000,1,8,,simm18,,v3 A1; PC+4*disp
JMPZ.S,0x61080000,1,8,,simm18,,v3 A1
JMPNZ.S,0x610C0000,1,8,,simm18,,v3 A1
JMPC.S,0x61100000,1,8,,simm18,,v3 A1
JMPNC.S,0x61140000,1,8,,simm18,,v3 A1
JMPO.S,0x61180000,1,8,,simm18,,v3 A1
JMPNO.S,0x611C0000,1,8,,simm18,,v3 A1
JMPS.S,0x61200000,1,8,,simm18,,v3 A1
JMPNS.S,0x61240000,1,8,,simm18,,v3 A1
JMPLT.S,0x61280000,1,8,,simm18,,v3 A1
JMPGE.S,0x612C0000,1,8,,simm18,,v3 A1
JMPLE.S,0x61300000,1,8,,simm18,,v3 A1
JMPGT.S,0x61340000,1,8,,simm18,,v3 A1
JMPULT.S,0x61380000,1,8,,simm18,,v3 A1
JMPUGE.S,0x613C0000,1,8,,simm18,,v3 A1
JMPULE.S,0x61400000,1,8,,simm18,,v3 A1
JMPUGT.S,0x61440000,1,8,,simm18,,v3 A1
JMPE.S,0x61480000,1,8,,simm18,,v3 A1
JMPNE.S,0x614C0000,1,8,,simm18,,v3 A1
CALL.S,0x63000000,1,8,,simm18,,v3 A1; pushes PC+4
BEQ,0x74000000,1,13,"rs1,rs2",simm13[20:8],,v3 B; flags untouched
BEQI,0x74200000,1,13,rs1,"simm4[3:0],simm13[20:8]",,v3 B
BNE,0x74400000,1,13,"rs1,rs2",simm13[20:8],,v3 B
BNEI,0x74600000,1,13,rs1,"simm4[3:0],simm13[20:8]",,v3 B
BLT,0x74800000,1,13,"rs1,rs2",simm13[20:8],,v3 B
BLTI,0x74A00000,1,13,rs1,"simm4[3:0],simm13[20:8]",,v3 B
BGE,0x74C00000,1,13,"rs1,rs2",simm13[20:8],,v3 B
BGEI,0x74E00000,1,13,rs1,"simm4[3:0],simm13[20:8]",,v3 B
BLE,0x75000000,1,13,"rs1,rs2",simm13[20:8],,v3 B
BLEI,0x75200000,1,13,rs1,"simm4[3:0],simm13[20:8]",,v3 B
BGT,0x75400000,1,13,"rs1,rs2",simm13[20:8],,v3 B
BGTI,0x75600000,1,13,rs1,"simm4[3:0],simm13[20:8]",,v3 B
BULT,0x75800000,1,13,"rs1,rs2",simm13[20:8],,v3 B
BULTI,0x75A00000,1,13,rs1,"simm4[3:0],simm13[20:8]",,v3 B
BUGE,0x75C00000,1,13,"rs1,rs2",simm13[20:8],,v3 B
BUGEI,0x75E00000,1,13,rs1,"simm4[3:0],simm13[20:8]",,v3 B
BULE,0x76000000,1,13,"rs1,rs2",simm13[20:8],,v3 B
BULEI,0x76200000,1,13,rs1,"simm4[3:0],simm13[20:8]",,v3 B
BUGT,0x76400000,1,13,"rs1,rs2",simm13[20:8],,v3 B
BUGTI,0x76600000,1,13,rs1,"simm4[3:0],simm13[20:8]",,v3 B
JMP,0xA0000000,2,8,,target32,,
JMPZ,0xA0080000,2,8,,target32,,
JMPNZ,0xA00C0000,2,8,,target32,,
JMPC,0xA0100000,2,8,,target32,,
JMPNC,0xA0140000,2,8,,target32,,
JMPO,0xA0180000,2,8,,target32,,
JMPNO,0xA01C0000,2,8,,target32,,
JMPS,0xA0200000,2,8,,target32,,
JMPNS,0xA0240000,2,8,,target32,,
JMPLT,0xA0280000,2,8,,target32,,
JMPGE,0xA02C0000,2,8,,target32,,
JMPLE,0xA0300000,2,8,,target32,,
JMPGT,0xA0340000,2,8,,target32,,
JMPULT,0xA0380000,2,8,,target32,,
JMPUGE,0xA03C0000,2,8,,target32,,
JMPULE,0xA0400000,2,8,,target32,,
JMPUGT,0xA0440000,2,8,,target32,,
JMPE,0xA0480000,2,8,,target32,,tests Z (= JMPZ)
JMPNE,0xA04C0000,2,8,,target32,,tests Z (= JMPNZ)
CALL,0xA2000000,2,8,,target32,,pushes PC+8
CALLZ,0xA2080000,2,8,,target32,,
CALLNZ,0xA20C0000,2,8,,target32,,
CALLC,0xA2100000,2,8,,target32,,
CALLNC,0xA2140000,2,8,,target32,,
CALLO,0xA2180000,2,8,,target32,,
CALLNO,0xA21C0000,2,8,,target32,,
CALLE,0xA2480000,2,8,,target32,,
CALLNE,0xA24C0000,2,8,,target32,,
JMPR,0x60800000,1,8,rs2,,,target=rs2
CALLR,0x62800000,1,8,rs2,,,target=rs2; pushes PC+4
PUSH,0x64000000,1,9,rs1,,,
PUSHV,0xA4400000,2,9,,zext32,,
PUSHV64,0xE4400000,3,9,,imm64,,
POP,0x64800000,1,9,rd,,,
GETSP,0x64C00000,1,9,rd,,,
SETSP,0x65000000,1,9,rs1,,,
ADDSP,0xA5400000,2,9,,sext32,,
RET,0x65800000,1,9,,,,
IRET,0x65C00000,1,9,,,ZSCV,restored from the frame
MULUR,0x68000000,1,10,"rd,rs1,rs2",,ZSV,
MULHUR,0x68400000,1,10,"rd,rs1,rs2",,ZSV,V=0
MULR,0x68800000,1,10,"rd,rs1,rs2",,ZSV,
MULHR,0x68C00000,1,10,"rd,rs1,rs2",,ZSV,V=0
MULV,0xA8800000,2,10,"rd,rs1",sext32,ZSV,in place
DIVUR,0x69000000,1,10,"rd,rs1,rs2",,ZV,V=1 and Z kept on /0
DIVR,0x69800000,1,10,"rd,rs1,rs2",,ZV,V=1 and Z kept on /0
DIVV,0xA9800000,2,10,"rd,rs1",sext32,ZV,in place; V=1 and Z kept on /0
MODUR,0x6A000000,1,10,"rd,rs1,rs2",,ZV,V=1 and Z kept on /0
MODR,0x6A800000,1,10,"rd,rs1,rs2",,ZV,V=1 and Z kept on /0
MODV,0xAA800000,2,10,"rd,rs1",sext32,ZV,in place; V=1 and Z kept on /0
NOP,0x6C000000,1,11,,,,
HALT,0x6C010000,1,11,,,,
WAIT,0x6C020000,1,11,,,,
RESET,0x6C030000,1,11,,,,PC=0x4
TRAP,0x6C040000,1,11,,,,
DELAYR,0x6C050000,1,11,rs1,,,
DELAYV,0xAC050000,2,11,,imm32,,
LCDCMDR,0x70000000,1,12,rs1,,,
LCDDATAR,0x71000000,1,12,rs1,,,
LCDCMDV,0xB0000000,2,12,,imm32,,
LCDDATAV,0xB1000000,2,12,,imm32,,
LCDRST,0xB2000000,2,12,,imm32,,
```

## 7. Flags and conditions

### 7.1 One flags register: `Z S C V`

| Flag | Meaning |
|---|---|
| Z | zero: the result (or the 32-bit W result) is 0 |
| S | sign: bit 63 of the result |
| C | carry out of an add; **borrow** out of a subtract |
| V | signed overflow |

There are no separate compare flags. The pre-unification `E` (equal),
`L` (signed less) and `U` (unsigned less) are **derived on read**:
`E = Z`, `L = S XOR V`, `U = C`. After a compare, these derivations give
the same answer as the old stored flags for every operand pair.

Flags are **sticky**: an instruction writes only the flags in its Flags
column, and the others keep their values. A conditional branch reads
whatever the last writer left.

**Carry is the x86 borrow convention.** After `SUB`/`CMP`/`DECR`,
`C = 1` exactly when `a < b` unsigned. That is the opposite of ARM.
SUBC computes `a - b - C`.

### 7.2 Who writes what

| Instructions | Z | S | C | V |
|---|---|---|---|---|
| ADD/SUB family: ADDR SUBR ADDC SUBC ADDI ADDV MINUSV INCR DECR, ADDW SUBW ADDIW, ADDI.S ADDV.S MINUSV.S | ✓ | ✓ | ✓ | ✓ |
| CMPRR CMPRV CMPRV.S CMPRRW CMPRVW (SUB, no writeback) | ✓ | ✓ | ✓ | ✓ |
| MUL family (MULUR MULR MULHUR MULHR MULV MULW) | ✓ | ✓ | | ✓ |
| DIV/MOD family | ✓ | | | ✓ |
| ABSR | ✓ | | | ✓ |
| SEXTB SEXTH | ✓ | ✓ | | |
| ROLR1 RORR1 ROLCR RORCR, `ROLV r 1`, `RORV r 1` | ✓ | | ✓ | |
| Shifts and rotates with F (SHLR…RORR, SHLV SHRV SHRAV ROLV RORV) | ✓ | | | |
| NEGR NOTR ZEXTB ZEXTH POPCNT BEXTR | ✓ | | | |
| BTST (embedded N; Z = NOT bit) | ✓ | | | |
| IRET (restored from the saved frame) | ✓ | ✓ | ✓ | ✓ |
| Everything else | | | | |

"Everything else" includes AND/OR/XOR (register and immediate), MIN/MAX,
MOV/LEA/SETR, the boolean compares, the fused branches, COPY, SEXTW,
ZEXTW, BSWAP, BITREV, CLZ, CTZ, GETF, the bit set/clear/toggle ops,
BTSTRR, BDEP, the shift-by-1 forms without F, all loads and stores, all
branches and calls, the stack ops and the system ops.

Details:

- **MUL**: S = bit 63 of the 64-bit result (the high half for MULH*; for
  MULW, the 64-bit low product before sign-extension). For the low-half forms, V = 1
  when the full product does not fit in 64 bits (signed or unsigned per
  SGN); the high-half forms clear V. MULW's Z follows its W result.
- **DIV/MOD**: a non-zero divisor writes Z from the result and clears V.
  A **zero divisor does not trap**: DIV returns all ones, MOD returns the
  dividend, V is set, and **Z is left as it was**. Signed division
  truncates toward zero, and `INT_MIN / -1` returns INT_MIN with V = 0.
- **ABSR** sets V only for INT_MIN, whose result is INT_MIN.
- **F bit**: the table follows the assembler's templates. With F set in a
  hand-encoded word, COPY, BSWAP, BITREV, CLZ, CTZ, GETF, BSET/BCLR/BTGL
  and BTSTRR also write Z from their result (for GETF, from the 64-bit
  word it writes).

### 7.3 Branch conditions

The COND field of a branch picks a relation over `Z S C V`; INV negates
it.

| COND | Mnemonic | Taken when | With INV |
|---|---|---|---|
| 0 | JMP | always | (reserved) |
| 1 | JMPZ | `Z` | JMPNZ: `!Z` |
| 2 | JMPC | `C` | JMPNC: `!C` |
| 3 | JMPO | `V` | JMPNO: `!V` |
| 4 | JMPS | `S` | JMPNS: `!S` |
| 5 | JMPLT | `S ^ V` | JMPGE: `!(S ^ V)` |
| 6 | JMPLE | `Z \| (S ^ V)` | JMPGT: `!(Z \| (S ^ V))` |
| 7 | JMPULT | `C` | JMPUGE: `!C` |
| 8 | JMPULE | `C \| Z` | JMPUGT: `!(C \| Z)` |
| 9 | JMPE | `Z` (same as COND 1) | JMPNE: `!Z` |

Because CMP is a SUB for flag purposes, the signed and unsigned branches
also work straight after a 64-bit subtract (SUBR, MINUSV, DECR): they
compare its two operands exactly as a CMP would. After an add they test
the add's own carry, sign and overflow, which is not a comparison.

```
_start
SETR A 3
SETR B 5
CMPRR A B             // 3 - 5: Z=0, S=1, C=1 (borrow), V=0
JMPULT LESS:          // taken: C=1 means A < B unsigned
HALT
LESS:
JMPLT BOTH:           // taken: S^V = 1 means A < B signed
HALT
BOTH:
HALT
```

### 7.4 Where the flags are visible

- **GETF** (`SETFR rd`): `rd = {Z, E, C, V}` in bits `[63:60]`, the rest
  0. E is derived, so bit 62 always equals bit 63.
- **Interrupt frame**: one doubleword pushed on interrupt entry,
  `[31:0] = PC`, `[38:32] = {Z, E, C, V, S, L, U}` (bit 38 = Z … bit 32 =
  U), `[42:39] = INT_MASK`, `[63:43] = 0`. IRET restores the PC, INT_MASK
  and **only Z, S, C, V** (bits 38, 34, 36, 35). The derived bits are
  ignored and regenerate.
- **Debugger** (gdbserver / RTL debug module): the flags register is the
  nibble `{Z, S, C, V}` in bits `[3:0]`.
