# ISA Encoding v2 — Opcode Number Map (flag-day renumbering)

Companion to `ISA_ENCODING_PLAN`. This is the **authoritative symbol → number
map** for the v2 re-encode, used by the CPU RTL (branch `isa-encoding-v2`),
LLVM, the assembler and any other tool. The CPU implements exactly the
instructions listed here, with **semantics identical to the v1 CPU** (this is
the "no benefit taken" checkpoint build — one dispatch arm per instruction,
same next-state functions, same flags, same cycle counts except where noted).
Every encoding not listed here traps with ERR_INV_OPCODE.

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
- Unused register fields and reserved bits must be **0** — the CPU matches
  them strictly and traps otherwise.

The table's "template" column is word 0 with all register fields = 0. To
assemble: `word0 = template | rd<<8 | rs1<<4 | rs2` (plus `N<<15` for
class-4 embedded-count forms).

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
| 0x9 | stack / SP |
| 0xA | mul / div |
| 0xB | system |
| 0xC | I/O (LCD) |
| 0x0, 0xD, 0xE, 0xF | illegal / reserved (trap) |

## 2. Per-class attribute bits

- **Class 1/2 (ALU)**: `[25:22]` OP (0=ADD 1=SUB 2=ADC 3=SBC 4=AND 5=OR 6=XOR
  7=MIN 8=MAX 9=MINU 10=MAXU 14=LEA 15=MOV), `[21]` F (flags), `[20]` SGN
  (class 2: sign- vs zero-extend imm32).
- **Class 3 (compare)**: `[25:23]` PRED (0=EQ 1=LT 2=LE 3=ULT 4=ULE),
  `[22]` INV, `[21]` B (0=flag-setting CMP: writes E/L/U; 1=boolean rd=0/1),
  `[20]` SGN (imm form).
- **Class 4 (shift/bit)**: `[25:22]` OP (0=SHL 1=SHR 2=SAR 3=ROL 4=ROR 5=RCL
  6=RCR 8=BSET 9=BCLR 10=BTGL 11=BTST 12=BEXTR 13=BDEP), `[21]` SRC
  (0=count in rs2[5:0], 1=embedded), `[20:15]` N (embedded count/position),
  `[14]` F.
- **Class 5 (unary)**: `[25:22]` OP (0=COPY 1=NEG 2=NOT 3=ABS 4=SEXT 5=ZEXT
  6=BSWAP 7=BITREV 8=POPCNT 9=CLZ 10=CTZ 12=GETF 14=INC 15=DEC),
  `[21:20]` SIZE (SEXT/ZEXT: 00=8 01=16 10=32), `[19]` F.
- **Class 6/7 (load/store)**: `[25:24]` SIZE (00=8 01=16 10=32 11=64),
  `[23]` SGN (loads only), `[22:21]` MODE (00=[rs1] 01=rs1+imm32 10=[imm32]
  11=rs1+rs2), `[20]` A (64-bit: 1 = force 8-byte alignment of the address).
- **Class 8 (branch)**: `[25]` LINK (call), `[24]` REL (PC-relative),
  `[23]` RIND (target = rs2, LEN=01), `[22:19]` COND (0=always 1=Z 2=C 3=V
  4=S 5=LT 6=LE 7=ULT 8=ULE 9=E), `[18]` INV.
- **Class 9 (stack)**: `[25:22]` OP (0=PUSH 1=PUSHI 2=POP 3=GETSP 4=SETSP
  5=ADDSP 6=RET 7=IRET).
- **Class A (mul/div)**: `[25:24]` OP (0=MUL 1=DIV 2=MOD), `[23]` SGN,
  `[22]` H (high half, MUL only).
- **Class B (system)**: `[21:16]` OP (0=NOP 1=HALT 2=WAIT 3=RESET 4=TRAP
  5=DELAY).
- **Class C (I/O)**: `[25:24]` OP (0=LCDCMD 1=LCDDATA 2=LCDRESET); reg vs imm
  source selected by LEN.

## 3. Complete symbol → number table

"Regs" lists the fields the instruction actually uses (all others must be 0).
Flag column = flags written (unchanged from v1 unless noted).

### ALU reg-reg (class 1) — `rd = rs1 OP rs2`, 1 word

| Symbol | Template | Regs | Flags | v1 opcode |
|---|---|---|---|---|
| ADDR  | `0x4420_0000` | rd,rs1,rs2 | Z,C,V | 0x0001_0xxx |
| SUBR  | `0x4460_0000` | rd,rs1,rs2 | Z,C,V | 0x0002_0xxx |
| ADDC  | `0x44A0_0000` | rd,rs1,rs2 | Z,C,V | 0x0006_0xxx |
| SUBC  | `0x44E0_0000` | rd,rs1,rs2 | Z,C,V | 0x0007_0xxx |
| ANDR  | `0x4500_0000` | rd,rs1,rs2 | — | 0x0003_0xxx |
| ORR   | `0x4540_0000` | rd,rs1,rs2 | — | 0x0004_0xxx |
| XORR  | `0x4580_0000` | rd,rs1,rs2 | — | 0x0005_0xxx |
| MINR  | `0x45C0_0000` | rd,rs1,rs2 | — | 0x0040_0xxx |
| MAXR  | `0x4600_0000` | rd,rs1,rs2 | — | 0x0041_0xxx |
| MINUR | `0x4640_0000` | rd,rs1,rs2 | — | 0x0042_0xxx |
| MAXUR | `0x4680_0000` | rd,rs1,rs2 | — | 0x0043_0xxx |

### ALU immediate (class 2) — `rd = rs1 OP ext(imm32)`, 2 words

rd and rs1 are now independent fields; the old in-place forms (ADDV, ANDV, …)
are emitted as `rd == rs1`.

| Symbol | Template | Regs | Flags | v1 opcode / note |
|---|---|---|---|---|
| ADDI   | `0x8830_0000` | rd,rs1,imm32 sext | Z,C,V | 0x0000_02xx (rd was [7:4], rs [3:0]) |
| ADDV   | `0x8820_0000` | rd,rs1,imm32 zext | Z,C,V | 0x0000_081x (emit rd=rs1) |
| MINUSV | `0x8860_0000` | rd,rs1,imm32 zext | Z,C,V | 0x0000_082x (emit rd=rs1) |
| ANDV   | `0x8900_0000` | rd,rs1,imm32 zext | — | 0x0000_086x (emit rd=rs1) |
| ORV    | `0x8940_0000` | rd,rs1,imm32 zext | — | 0x0000_087x (emit rd=rs1) |
| XORV   | `0x8980_0000` | rd,rs1,imm32 zext | — | 0x0000_088x (emit rd=rs1) |
| LEAPC  | `0x8B80_0000` | rd,imm32 | — | 0x0000_099x — `rd = PC + imm32` |
| SETR   | `0x8BD0_0000` | rd,imm32 sext | — | 0x0000_080x (MOV, SGN=1) |
| SETR64 | `0xCBC0_0000` | rd,imm64 | — | 0x0000_0FEx (MOV, LEN=11, 3 words) |

### Compare (class 3)

| Symbol | Template | Regs | Flags/result | v1 opcode |
|---|---|---|---|---|
| CMPRR  | `0x4C00_0000` | rs1,rs2 | E,L,U | 0x0000_05xx |
| CMPRV  | `0x8C10_0000` | rs1,imm32 sext | E,L,U | 0x0000_083x (reg was [3:0]) |
| CMPEQR | `0x4C20_0000` | rd,rs1,rs2 | rd=0/1 | 0x0030_0xxx |
| CMPNER | `0x4C60_0000` | rd,rs1,rs2 | rd=0/1 | 0x0031_0xxx |
| CMPLTR | `0x4CA0_0000` | rd,rs1,rs2 | rd=0/1 | 0x0032_0xxx |
| CMPGER | `0x4CE0_0000` | rd,rs1,rs2 | rd=0/1 | 0x0035_0xxx (LT+INV) |
| CMPLER | `0x4D20_0000` | rd,rs1,rs2 | rd=0/1 | 0x0033_0xxx |
| CMPGTR | `0x4D60_0000` | rd,rs1,rs2 | rd=0/1 | 0x0034_0xxx (LE+INV) |
| CMPULTR | `0x4DA0_0000` | rd,rs1,rs2 | rd=0/1 | 0x0036_0xxx |
| CMPUGER | `0x4DE0_0000` | rd,rs1,rs2 | rd=0/1 | 0x0039_0xxx (ULT+INV) |
| CMPULER | `0x4E20_0000` | rd,rs1,rs2 | rd=0/1 | 0x0037_0xxx |
| CMPUGTR | `0x4E60_0000` | rd,rs1,rs2 | rd=0/1 | 0x0038_0xxx (ULE+INV) |

### Shift / rotate / bit (class 4)

Register-count forms: count/position = `rs2[5:0]`. Embedded forms: SRC=1,
count/position N at `[20:15]` — **these are now 1 word** (v1 SHLV/BSET/… were
2 words). Template shown with N=0; OR in `N<<15`.

| Symbol | Template | Regs | Flags | v1 opcode / note |
|---|---|---|---|---|
| SHLR  | `0x5000_4000` | rd,rs1,rs2 | Z | 0x0020_0xxx |
| SHRR  | `0x5040_4000` | rd,rs1,rs2 | Z | 0x0021_0xxx |
| SARR  | `0x5080_4000` | rd,rs1,rs2 | Z | 0x0022_0xxx |
| ROLR  | `0x50C0_4000` | rd,rs1,rs2 | Z | 0x0023_0xxx |
| RORR  | `0x5100_4000` | rd,rs1,rs2 | Z | 0x0024_0xxx |
| SHLV #N  | `0x5020_4000\|N<<15` | rd,rs1 | Z | 0x0000_091x — now 1 word, rd=rs1 for v1 |
| SHRV #N  | `0x5060_4000\|N<<15` | rd,rs1 | Z | 0x0000_092x — now 1 word |
| SHRAV #N | `0x50A0_4000\|N<<15` | rd,rs1 | Z | 0x0000_093x — now 1 word |
| ROLV #N  | `0x50E0_4000\|N<<15` | rd,rs1 | Z (N=1: Z,C) | 0x0000_0FCx — now 1 word; N=1 ≡ ROLR1 |
| RORV #N  | `0x5120_4000\|N<<15` | rd,rs1 | Z (N=1: Z,C) | 0x0000_0FDx — now 1 word; N=1 ≡ RORR1 |
| SHLR1 | `0x5020_8000` | rd,rs1 | — | 0x0000_08Dx (SHL SRC=1 N=1 F=0) |
| SHLAR | `0x5020_8000` | rd,rs1 | — | 0x0000_08Fx — **alias of SHLR1** |
| SHRR1 | `0x5060_8000` | rd,rs1 | — | 0x0000_08Ex |
| SHRAR | `0x50A0_8000` | rd,rs1 | — | 0x0000_090x |
| ROLR1 | `0x50E0_C000` | rd,rs1 | Z,C | 0x0000_0F8x (ROL SRC=1 N=1 F=1) |
| RORR1 | `0x5120_C000` | rd,rs1 | Z,C | 0x0000_0F9x |
| ROLCR | `0x5160_C000` | rd,rs1 | Z,C | 0x0000_0FAx (RCL N=1) |
| RORCR | `0x51A0_C000` | rd,rs1 | Z,C | 0x0000_0FBx (RCR N=1) |
| BSETRR | `0x5200_0000` | rd,rs1,rs2 | — | 0x0050_0xxx |
| BCLRRR | `0x5240_0000` | rd,rs1,rs2 | — | 0x0051_0xxx |
| BTGLRR | `0x5280_0000` | rd,rs1,rs2 | — | 0x0052_0xxx |
| BTSTRR | `0x52C0_0000` | rd,rs1,rs2 | rd=bit | 0x0053_0xxx |
| BSET #N | `0x5220_0000\|N<<15` | rd,rs1 | — | 0x0000_0A0x — now 1 word |
| BCLR #N | `0x5260_0000\|N<<15` | rd,rs1 | — | 0x0000_0A1x — now 1 word |
| BTGL #N | `0x52A0_0000\|N<<15` | rd,rs1 | — | 0x0000_0A2x — now 1 word |
| BTST #N | `0x52E0_0000\|N<<15` | rs1 | Z=~bit (no rd) | 0x0000_0A3x — now 1 word; keeps v1 flag-only semantics |
| BEXTR | `0x9300_0000` | rd,rs1,imm32 | Z | 0x0000_0ACx — params in imm32 (start[4:0], len[12:8]) |
| BDEP  | `0x9340_0000` | rd,rs1(base),rs2(src),imm32 | — | 0x0000_0ADx — v1 base/dest was reg_2, src reg_1 |

### Unary (class 5) — `rd = OP(rs1)`, 1 word

| Symbol | Template | Regs | Flags | v1 opcode |
|---|---|---|---|---|
| COPY   | `0x5400_0000` | rd,rs1 | — | 0x0000_01xx |
| NEGR   | `0x5448_0000` | rd,rs1 | Z | 0x0000_08Ax |
| NOTR   | `0x5488_0000` | rd,rs1 | Z | 0x0000_098x |
| ABSR   | `0x54C8_0000` | rd,rs1 | Z,V | 0x0000_08Bx |
| SEXTB  | `0x5508_0000` | rd,rs1 | Z,S | 0x0000_08Cx |
| SEXTH  | `0x5518_0000` | rd,rs1 | Z,S | 0x0000_094x |
| SEXTW  | `0x5520_0000` | rd,rs1 | — | 0x0000_0F0x |
| ZEXTB  | `0x5548_0000` | rd,rs1 | Z | 0x0000_095x |
| ZEXTH  | `0x5558_0000` | rd,rs1 | Z | 0x0000_096x |
| ZEXTW  | `0x5560_0000` | rd,rs1 | — | 0x0000_0F1x |
| BSWAP  | `0x5580_0000` | rd,rs1 | — | 0x0000_097x |
| BITREV | `0x55C0_0000` | rd,rs1 | — | 0x0000_0ABx |
| POPCNT | `0x5608_0000` | rd,rs1 | Z | 0x0000_0A8x |
| CLZ    | `0x5640_0000` | rd,rs1 | — | 0x0000_0A9x |
| CTZ    | `0x5680_0000` | rd,rs1 | — | 0x0000_0AAx |
| SETFR  | `0x5700_0000` | rd | — | 0x0000_089x — rd={Z,E,C,V,60'b0}; GETF slot, v1 semantics kept |
| INCR   | `0x5788_0000` | rd,rs1 | Z,C,V | 0x0000_084x — `rd = rs1 + 1` (emit rd=rs1) |
| DECR   | `0x57C8_0000` | rd,rs1 | Z,C,V | 0x0000_085x — `rd = rs1 - 1` (emit rd=rs1) |

### Loads (class 6) — `rd = ext(mem[EA])`

Base address is **rs1** (v1 used the [3:0] field). Dest is **rd** (v1 used
[7:4] or [3:0] depending on form).

| Symbol | Template | Words | EA | v1 opcode |
|---|---|---|---|---|
| MEMGET8   | `0x5800_0000` | 1 | rs1 | 0x0000_75xx |
| MEMGET16  | `0x5900_0000` | 1 | rs1 | 0x0000_77xx |
| MEMGET32  | `0x5A00_0000` | 1 | rs1 (unaligned-tolerant) | 0x0000_79xx |
| MEMREADRR | `0x5B00_0000` | 1 | rs1 (raw 64) | 0x0000_71xx |
| MEMGET64  | `0x5B10_0000` | 1 | rs1 (8-byte aligned) | 0x0000_7Bxx |
| LDIDX8    | `0x9820_0000` | 2 | rs1+imm32 | 0x0000_C4xx |
| LDIDX8_S  | `0x98A0_0000` | 2 | rs1+imm32, sext | 0x0000_C6xx |
| LDIDX16   | `0x9920_0000` | 2 | rs1+imm32 | 0x0000_C2xx |
| LDIDX16_S | `0x99A0_0000` | 2 | rs1+imm32, sext | 0x0000_C7xx |
| LDIDX32   | `0x9A20_0000` | 2 | rs1+imm32 | 0x0000_C0xx |
| LDIDX64   | `0x9B20_0000` | 2 | rs1+imm32 (raw) | 0x0000_0Cxx |
| LDIDX64A  | `0x9B30_0000` | 2 | rs1+imm32 (aligned) | 0x0000_FCxx |
| MEMREADR  | `0x9B40_0000` | 2 | imm32 absolute | 0x0000_721x |
| LDIDX64R  | `0x5B60_0000` | **1** | rs1+rs2 | 0x0000_0Exx — offset reg now rs2 (was in imm word); 1 word and faster |

### Stores (class 7) — `mem[EA] = reg[rd]` (rd field = data source)

| Symbol | Template | Words | EA | v1 opcode |
|---|---|---|---|---|
| MEMSET8    | `0x5C00_0000` | 1 | rs1 | 0x0000_74xx |
| MEMSET16   | `0x5D00_0000` | 1 | rs1 | 0x0000_76xx |
| MEMSET32   | `0x5E00_0000` | 1 | rs1 | 0x0000_78xx |
| MEMSET64RR | `0x5F00_0000` | 1 | rs1 (raw 64) | 0x0000_70xx |
| MEMSET64   | `0x5F10_0000` | 1 | rs1 (8-byte aligned) | 0x0000_7Axx |
| STIDX8     | `0x9C20_0000` | 2 | rs1+imm32 | 0x0000_C5xx |
| STIDX16    | `0x9D20_0000` | 2 | rs1+imm32 | 0x0000_C3xx |
| STIDX32    | `0x9E20_0000` | 2 | rs1+imm32 | 0x0000_C1xx |
| STIDX64    | `0x9F20_0000` | 2 | rs1+imm32 (raw) | 0x0000_0Dxx |
| STIDX64A   | `0x9F30_0000` | 2 | rs1+imm32 (aligned) | 0x0000_FDxx |
| MEMSETR    | `0x9F40_0000` | 2 | imm32 absolute | 0x0000_720x |
| STIDX64R   | `0x5F60_0000` | **1** | rs1+rs2 | 0x0000_73xx — offset reg now rs2; 1 word and faster |

### Branch / call (class 8)

Immediate-target forms are 2 words (target/displacement = imm32); register
forms are 1 word (target = rs2). No register fields otherwise.

| Symbol | Template | Symbol | Template |
|---|---|---|---|
| JMP     | `0xA000_0000` | JMPREL    | `0xA100_0000` |
| JMPZ    | `0xA008_0000` | JMPZREL   | `0xA108_0000` |
| JMPNZ   | `0xA00C_0000` | JMPNZREL  | `0xA10C_0000` |
| JMPC    | `0xA010_0000` | JMPCREL   | `0xA110_0000` |
| JMPNC   | `0xA014_0000` | JMPNCREL  | `0xA114_0000` |
| JMPO    | `0xA018_0000` | JMPSREL   | `0xA120_0000` |
| JMPNO   | `0xA01C_0000` | JMPNSREL  | `0xA124_0000` |
| JMPS    | `0xA020_0000` | JMPLTREL  | `0xA128_0000` |
| JMPNS   | `0xA024_0000` | JMPGEREL  | `0xA12C_0000` |
| JMPLT   | `0xA028_0000` | JMPLEREL  | `0xA130_0000` |
| JMPGE   | `0xA02C_0000` | JMPGTREL  | `0xA134_0000` |
| JMPLE   | `0xA030_0000` | JMPULTREL | `0xA138_0000` |
| JMPGT   | `0xA034_0000` | JMPUGEREL | `0xA13C_0000` |
| JMPULT  | `0xA038_0000` | JMPULEREL | `0xA140_0000` |
| JMPUGE  | `0xA03C_0000` | JMPUGTREL | `0xA144_0000` |
| JMPULE  | `0xA040_0000` | JMPEREL   | `0xA148_0000` |
| JMPUGT  | `0xA044_0000` | JMPNEREL  | `0xA14C_0000` |
| JMPE    | `0xA048_0000` | | |
| JMPNE   | `0xA04C_0000` | | |
| CALL    | `0xA200_0000` | CALLO     | `0xA218_0000` |
| CALLZ   | `0xA208_0000` | CALLNO    | `0xA21C_0000` |
| CALLNZ  | `0xA20C_0000` | CALLE     | `0xA248_0000` |
| CALLC   | `0xA210_0000` | CALLNE    | `0xA24C_0000` |
| CALLNC  | `0xA214_0000` | CALLREL   | `0xA300_0000` |
| JMPR    | `0x6080_0000` (rs2) | CALLR | `0x6280_0000` (rs2) |

(v1: JMP family 0x0000_1000-101C, REL 0x0000_1030-1040, JMPR 0x0000_102x,
CALLR 0x0000_407x. Rel O/NO variants remain unassigned, as in v1.)

### Stack / SP (class 9)

| Symbol | Template | Words | Regs | v1 opcode |
|---|---|---|---|---|
| PUSH    | `0x6400_0000` | 1 | rs1 | 0x0000_400x |
| PUSHV   | `0xA440_0000` | 2 | imm32 zext | 0x0000_4020 |
| PUSHV64 | `0xE440_0000` | 3 | imm64 | 0x0000_4060 |
| POP     | `0x6480_0000` | 1 | rd | 0x0000_401x |
| GETSP   | `0x64C0_0000` | 1 | rd | 0x0000_403x |
| SETSP   | `0x6500_0000` | 1 | rs1 | 0x0000_404x |
| ADDSP   | `0xA540_0000` | 2 | imm32 sext | 0x0000_4050 |
| RET     | `0x6580_0000` | 1 | — | 0x0000_1012 |
| IRET    | `0x65C0_0000` | 1 | — | 0x0000_6011 |

### Mul / div (class A)

| Symbol | Template | Words | Regs | v1 opcode |
|---|---|---|---|---|
| MULUR  | `0x6800_0000` | 1 | rd,rs1,rs2 | 0x0011_0xxx |
| MULHUR | `0x6840_0000` | 1 | rd,rs1,rs2 | 0x0013_0xxx |
| MULR   | `0x6880_0000` | 1 | rd,rs1,rs2 | 0x0010_0xxx |
| MULHR  | `0x68C0_0000` | 1 | rd,rs1,rs2 | 0x0012_0xxx |
| MULV   | `0xA880_0000` | 2 | rd,rs1,imm32 sext | 0x0000_0B8x |
| DIVUR  | `0x6900_0000` | 1 | rd,rs1,rs2 | 0x0015_0xxx |
| DIVR   | `0x6980_0000` | 1 | rd,rs1,rs2 | 0x0014_0xxx |
| DIVV   | `0xA980_0000` | 2 | rd,rs1,imm32 sext | 0x0000_0B9x |
| MODUR  | `0x6A00_0000` | 1 | rd,rs1,rs2 | 0x0017_0xxx |
| MODR   | `0x6A80_0000` | 1 | rd,rs1,rs2 | 0x0016_0xxx |
| MODV   | `0xAA80_0000` | 2 | rd,rs1,imm32 sext | 0x0000_0BAx |

### System (class B) / I/O (class C)

| Symbol | Template | Words | Regs | v1 opcode |
|---|---|---|---|---|
| NOP     | `0x6C00_0000` | 1 | — | 0x0000_F010 |
| HALT    | `0x6C01_0000` | 1 | — | 0x0000_F011 |
| WAIT    | `0x6C02_0000` | 1 | — | 0x0000_6012 |
| RESET   | `0x6C03_0000` | 1 | — | 0x0000_F012 |
| TRAP    | `0x6C04_0000` | 1 | — | 0x0000_F014 |
| DELAYR  | `0x6C05_0000` | 1 | rs1 | 0x0000_F00x |
| DELAYV  | `0xAC05_0000` | 2 | imm32 | 0x0000_F013 |
| LCDCMDR  | `0x7000_0000` | 1 | rs1 | 0x0000_200x |
| LCDDATAR | `0x7100_0000` | 1 | rs1 | 0x0000_201x |
| LCDCMDV  | `0xB000_0000` | 2 | imm32 | 0x0000_2021 |
| LCDDATAV | `0xB100_0000` | 2 | imm32 | 0x0000_2022 |
| LCDRST   | `0xB200_0000` | 2 | imm32[0] | 0x0000_2023 |

## 4. Migration notes for LLVM / assembler

1. **Register fields moved.** Everything is now rd=[11:8], rs1=[7:4],
   rs2=[3:0]. v1's legacy forms packed operands differently per opcode
   (see the "v1 opcode" notes). Stores read their **data from the rd field**
   (the CPU grew a third register read port for this).
2. **In-place forms are gone as encodings.** ADDV/ANDV/ORV/XORV/MINUSV/
   INCR/DECR/NEGR/shift-by-imm/etc. are 3-operand; emit rd=rs1 to reproduce
   v1 behaviour. rd≠rs1 also works (new capability, same hardware path).
3. **Length changes.** Shift/rotate/bit-manipulate with embedded constant
   (SHLV/SHRV/SHRAV/ROLV/RORV/BSET/BCLR/BTGL/BTST) are now **1 word**
   (count/position at word0[20:15]). LDIDX64R/STIDX64R are now **1 word**
   (offset register in rs2). Everything else keeps its v1 length.
4. **Aliases.** SHLAR ≡ SHLR1 (one encoding). ROLV #1 ≡ ROLR1 and
   RORV #1 ≡ RORR1 (these set Z and C; ROLV/RORV with N≠1 set Z only,
   exactly as v1).
5. **Immediate extension** is explicit per-encoding (SGN bit): ADDI/CMPRV/
   SETR/MULV/DIVV/MODV/ADDSP sign-extend; ADDV/MINUSV/ANDV/ORV/XORV/PUSHV
   zero-extend. Unchanged from v1 behaviour.
6. **Old binaries fail fast**: any v1 word 0 has bits [31:30] = 00 → LEN=00
   → ERR_INV_OPCODE crash dump on the first instruction.
7. **Unassigned = trap.** Reserved classes, unassigned OP/attribute
   combinations, and non-zero unused register/x fields all trap. The free
   combinations the plan mentions (LDIDX32_S, MULHV, boolean compare-imm,
   conditional indirect branches, SETF, …) are NOT implemented in this
   checkpoint build.
8. **Deferred semantic cleanups** (kept at v1 behaviour for this build,
   revisit in the simplification pass): CMP does not drive Z (E flag and
   JMPE/JMPNE stay); BTST-imm is flag-only while BTSTRR writes rd; SETFR
   still exposes only Z/E/C/V in the top nibble; rotates by N≠1 do not
   update carry.
9. **Retired v1 opcodes** (already MMIO, not re-encoded): LED/7-seg/RGB
   (0x3000/0x306x/0x307x), TXMEM/TXSTRMEM (0x5002/0x5003), UART ops.

## 5. Field-driven decode — newly legal encodings

Since the decode-simplification build (dispatch = one arm per CLASS×LEN,
attributes parameterize the shared next-state functions), these previously
trapping combinations are **architecturally legal** and available to LLVM.
Everything not listed here and not in §3 still traps.

- **ALU-imm (class 2)**: ADC/SBC with imm32; both SGN values on every OP
  (e.g. sign-extended SUB-imm, zero-extended MOV). LEA/MOV ignore rs1 rather
  than trapping on it.
- **Compare (class 3)**: boolean compare-immediate (B=1, LEN=10, rd=0/1);
  both SGN values on both forms.
- **Shift/bit (class 4)**: the F bit is honored (Z-update is per-instruction
  selectable) for SHL/SHR/SAR/ROL/ROR/BSET/BCLR/BTGL/BTSTRR, with any
  embedded N. ROL/ROR with SRC=1, N=1, F=1 keep the rotate-by-1 Z+C
  semantics; RCL/RCR still exist only in that shape.
- **Unary (class 5)**: F honored where the datapath parameterizes it
  (COPY/NEG/NOT/ZEXT/BSWAP/BITREV/POPCNT/CLZ/CTZ/GETF); SEXT with F=1 now
  exists at all three sizes (Z+S). ABS/INC/DEC still require F=1.
- **Loads (class 6)**: the full SIZE × SGN × MODE × A space (A: 64-bit only;
  SIZE=32 MODE=00 remains the unaligned-tolerant MEMGET32 with SGN=A=0):
  LDIDX8_S/16_S/32_S, MEMGET8_S/16_S/32_S(via mode 01)/64, sub-word reg+reg
  (mode 11) and absolute (mode 10) forms — all 1 word in modes 00/11.
- **Stores (class 7)**: full SIZE × MODE space (SGN reserved-0): sub-word
  reg+reg and absolute forms.
- **Mul/div (class A)**: MULHV/MULUV/MULHUV, DIVUV/MODUV — imm forms extend
  per the SGN bit.
- **Branches (class 8)**: conditional PC-relative calls (LINK=1, REL=1,
  any COND) and conditional register-indirect jumps/calls (RIND=1, any
  COND, including E). COND values 10-15 and INV-of-always trap.

## 6. Machine-readable table

```csv
symbol,template,words,class,regs,imm,flags,notes
ADDR,0x44200000,1,1,"rd,rs1,rs2",,ZCV,
SUBR,0x44600000,1,1,"rd,rs1,rs2",,ZCV,
ADDC,0x44A00000,1,1,"rd,rs1,rs2",,ZCV,
SUBC,0x44E00000,1,1,"rd,rs1,rs2",,ZCV,
ANDR,0x45000000,1,1,"rd,rs1,rs2",,,
ORR,0x45400000,1,1,"rd,rs1,rs2",,,
XORR,0x45800000,1,1,"rd,rs1,rs2",,,
MINR,0x45C00000,1,1,"rd,rs1,rs2",,,
MAXR,0x46000000,1,1,"rd,rs1,rs2",,,
MINUR,0x46400000,1,1,"rd,rs1,rs2",,,
MAXUR,0x46800000,1,1,"rd,rs1,rs2",,,
ADDI,0x88300000,2,2,"rd,rs1",sext32,ZCV,
ADDV,0x88200000,2,2,"rd,rs1",zext32,ZCV,emit rd=rs1 for v1
MINUSV,0x88600000,2,2,"rd,rs1",zext32,ZCV,emit rd=rs1 for v1
ANDV,0x89000000,2,2,"rd,rs1",zext32,,emit rd=rs1 for v1
ORV,0x89400000,2,2,"rd,rs1",zext32,,emit rd=rs1 for v1
XORV,0x89800000,2,2,"rd,rs1",zext32,,emit rd=rs1 for v1
LEAPC,0x8B800000,2,2,rd,pc32,,rd=PC+imm32
SETR,0x8BD00000,2,2,rd,sext32,,
SETR64,0xCBC00000,3,2,rd,imm64,,
CMPRR,0x4C000000,1,3,"rs1,rs2",,ELU,
CMPRV,0x8C100000,2,3,rs1,sext32,ELU,
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
ROLR,0x50C04000,1,4,"rd,rs1,rs2",,Z,count=rs2[5:0]
RORR,0x51004000,1,4,"rd,rs1,rs2",,Z,count=rs2[5:0]
SHLV,0x50204000,1,4,"rd,rs1",N<<15,Z,was 2 words
SHRV,0x50604000,1,4,"rd,rs1",N<<15,Z,was 2 words
SHRAV,0x50A04000,1,4,"rd,rs1",N<<15,Z,was 2 words
ROLV,0x50E04000,1,4,"rd,rs1",N<<15,Z,N=1 sets Z+C (=ROLR1); was 2 words
RORV,0x51204000,1,4,"rd,rs1",N<<15,Z,N=1 sets Z+C (=RORR1); was 2 words
SHLR1,0x50208000,1,4,"rd,rs1",,,N=1 fixed
SHLAR,0x50208000,1,4,"rd,rs1",,,alias of SHLR1
SHRR1,0x50608000,1,4,"rd,rs1",,,N=1 fixed
SHRAR,0x50A08000,1,4,"rd,rs1",,,N=1 fixed
ROLR1,0x50E0C000,1,4,"rd,rs1",,ZC,N=1 fixed
RORR1,0x5120C000,1,4,"rd,rs1",,ZC,N=1 fixed
ROLCR,0x5160C000,1,4,"rd,rs1",,ZC,rotate through carry
RORCR,0x51A0C000,1,4,"rd,rs1",,ZC,rotate through carry
BSETRR,0x52000000,1,4,"rd,rs1,rs2",,,pos=rs2[5:0]
BCLRRR,0x52400000,1,4,"rd,rs1,rs2",,,pos=rs2[5:0]
BTGLRR,0x52800000,1,4,"rd,rs1,rs2",,,pos=rs2[5:0]
BTSTRR,0x52C00000,1,4,"rd,rs1,rs2",,,rd=bit value
BSET,0x52200000,1,4,"rd,rs1",N<<15,,was 2 words
BCLR,0x52600000,1,4,"rd,rs1",N<<15,,was 2 words
BTGL,0x52A00000,1,4,"rd,rs1",N<<15,,was 2 words
BTST,0x52E00000,1,4,rs1,N<<15,Z,flag-only; was 2 words
BEXTR,0x93000000,2,4,"rd,rs1",params32,Z,start[4:0] len[12:8]
BDEP,0x93400000,2,4,"rd,rs1,rs2",params32,,base=rs1 src=rs2
COPY,0x54000000,1,5,"rd,rs1",,,
NEGR,0x54480000,1,5,"rd,rs1",,Z,
NOTR,0x54880000,1,5,"rd,rs1",,Z,
ABSR,0x54C80000,1,5,"rd,rs1",,ZV,
SEXTB,0x55080000,1,5,"rd,rs1",,ZS,
SEXTH,0x55180000,1,5,"rd,rs1",,ZS,
SEXTW,0x55200000,1,5,"rd,rs1",,,
ZEXTB,0x55480000,1,5,"rd,rs1",,Z,
ZEXTH,0x55580000,1,5,"rd,rs1",,Z,
ZEXTW,0x55600000,1,5,"rd,rs1",,,
BSWAP,0x55800000,1,5,"rd,rs1",,,
BITREV,0x55C00000,1,5,"rd,rs1",,,
POPCNT,0x56080000,1,5,"rd,rs1",,Z,
CLZ,0x56400000,1,5,"rd,rs1",,,
CTZ,0x56800000,1,5,"rd,rs1",,,
SETFR,0x57000000,1,5,rd,,,rd={Z E C V}<<60
INCR,0x57880000,1,5,"rd,rs1",,ZCV,rd=rs1+1
DECR,0x57C80000,1,5,"rd,rs1",,ZCV,rd=rs1-1
MEMGET8,0x58000000,1,6,"rd,rs1",,,
MEMGET16,0x59000000,1,6,"rd,rs1",,,
MEMGET32,0x5A000000,1,6,"rd,rs1",,,unaligned-tolerant
MEMREADRR,0x5B000000,1,6,"rd,rs1",,,raw 64-bit
MEMGET64,0x5B100000,1,6,"rd,rs1",,,8-byte aligned
LDIDX8,0x98200000,2,6,"rd,rs1",off32,,
LDIDX8_S,0x98A00000,2,6,"rd,rs1",off32,,sext
LDIDX16,0x99200000,2,6,"rd,rs1",off32,,
LDIDX16_S,0x99A00000,2,6,"rd,rs1",off32,,sext
LDIDX32,0x9A200000,2,6,"rd,rs1",off32,,
LDIDX64,0x9B200000,2,6,"rd,rs1",off32,,raw
LDIDX64A,0x9B300000,2,6,"rd,rs1",off32,,aligned
MEMREADR,0x9B400000,2,6,rd,addr32,,
LDIDX64R,0x5B600000,1,6,"rd,rs1,rs2",,,EA=rs1+rs2; was 2 words
MEMSET8,0x5C000000,1,7,"rd,rs1",,,data=reg[rd]
MEMSET16,0x5D000000,1,7,"rd,rs1",,,data=reg[rd]
MEMSET32,0x5E000000,1,7,"rd,rs1",,,data=reg[rd]
MEMSET64RR,0x5F000000,1,7,"rd,rs1",,,data=reg[rd] raw
MEMSET64,0x5F100000,1,7,"rd,rs1",,,data=reg[rd] aligned
STIDX8,0x9C200000,2,7,"rd,rs1",off32,,data=reg[rd]
STIDX16,0x9D200000,2,7,"rd,rs1",off32,,data=reg[rd]
STIDX32,0x9E200000,2,7,"rd,rs1",off32,,data=reg[rd]
STIDX64,0x9F200000,2,7,"rd,rs1",off32,,data=reg[rd] raw
STIDX64A,0x9F300000,2,7,"rd,rs1",off32,,data=reg[rd] aligned
MEMSETR,0x9F400000,2,7,rd,addr32,,data=reg[rd]
STIDX64R,0x5F600000,1,7,"rd,rs1,rs2",,,EA=rs1+rs2; was 2 words
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
JMPE,0xA0480000,2,8,,target32,,
JMPNE,0xA04C0000,2,8,,target32,,
JMPREL,0xA1000000,2,8,,disp32,,
JMPZREL,0xA1080000,2,8,,disp32,,
JMPNZREL,0xA10C0000,2,8,,disp32,,
JMPCREL,0xA1100000,2,8,,disp32,,
JMPNCREL,0xA1140000,2,8,,disp32,,
JMPSREL,0xA1200000,2,8,,disp32,,
JMPNSREL,0xA1240000,2,8,,disp32,,
JMPLTREL,0xA1280000,2,8,,disp32,,
JMPGEREL,0xA12C0000,2,8,,disp32,,
JMPLEREL,0xA1300000,2,8,,disp32,,
JMPGTREL,0xA1340000,2,8,,disp32,,
JMPULTREL,0xA1380000,2,8,,disp32,,
JMPUGEREL,0xA13C0000,2,8,,disp32,,
JMPULEREL,0xA1400000,2,8,,disp32,,
JMPUGTREL,0xA1440000,2,8,,disp32,,
JMPEREL,0xA1480000,2,8,,disp32,,
JMPNEREL,0xA14C0000,2,8,,disp32,,
CALL,0xA2000000,2,8,,target32,,
CALLZ,0xA2080000,2,8,,target32,,
CALLNZ,0xA20C0000,2,8,,target32,,
CALLC,0xA2100000,2,8,,target32,,
CALLNC,0xA2140000,2,8,,target32,,
CALLO,0xA2180000,2,8,,target32,,
CALLNO,0xA21C0000,2,8,,target32,,
CALLE,0xA2480000,2,8,,target32,,
CALLNE,0xA24C0000,2,8,,target32,,
CALLREL,0xA3000000,2,8,,disp32,,
JMPR,0x60800000,1,8,rs2,,,target=rs2
CALLR,0x62800000,1,8,rs2,,,target=rs2
PUSH,0x64000000,1,9,rs1,,,
PUSHV,0xA4400000,2,9,,zext32,,
PUSHV64,0xE4400000,3,9,,imm64,,
POP,0x64800000,1,9,rd,,,
GETSP,0x64C00000,1,9,rd,,,
SETSP,0x65000000,1,9,rs1,,,
ADDSP,0xA5400000,2,9,,sext32,,
RET,0x65800000,1,9,,,,
IRET,0x65C00000,1,9,,,,
MULUR,0x68000000,1,10,"rd,rs1,rs2",,,
MULHUR,0x68400000,1,10,"rd,rs1,rs2",,,
MULR,0x68800000,1,10,"rd,rs1,rs2",,,
MULHR,0x68C00000,1,10,"rd,rs1,rs2",,,
MULV,0xA8800000,2,10,"rd,rs1",sext32,,
DIVUR,0x69000000,1,10,"rd,rs1,rs2",,V on /0,
DIVR,0x69800000,1,10,"rd,rs1,rs2",,V on /0,
DIVV,0xA9800000,2,10,"rd,rs1",sext32,V on /0,
MODUR,0x6A000000,1,10,"rd,rs1,rs2",,V on /0,
MODR,0x6A800000,1,10,"rd,rs1,rs2",,V on /0,
MODV,0xAA800000,2,10,"rd,rs1",sext32,V on /0,
NOP,0x6C000000,1,11,,,,
HALT,0x6C010000,1,11,,,,
WAIT,0x6C020000,1,11,,,,
RESET,0x6C030000,1,11,,,,
TRAP,0x6C040000,1,11,,,,
DELAYR,0x6C050000,1,11,rs1,,,
DELAYV,0xAC050000,2,11,,imm32,,
LCDCMDR,0x70000000,1,12,rs1,,,
LCDDATAR,0x71000000,1,12,rs1,,,
LCDCMDV,0xB0000000,2,12,,imm32,,
LCDDATAV,0xB1000000,2,12,,imm32,,
LCDRST,0xB2000000,2,12,,imm32,,
```
