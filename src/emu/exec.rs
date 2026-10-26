//! Instruction decode and execute: one handler per v2 CLASS.
//!
//! [`Cpu::execute`] decodes the fields every class shares into [`Fields`],
//! dispatches on CLASS, and applies the handler's [`Flow`]: the handlers never
//! touch the PC themselves, and a reserved encoding is an [`Illegal`] error
//! that becomes [`StopReason::InvalidOpcode`] in one place.

use super::flags::Flags;
use super::{Cpu, StopReason};

/// The decoded instruction: word 0 plus the fields every class shares
/// (`doc/ISA_ENCODING_V2.md` §1).  Each handler pulls its own attribute bits
/// from `[25:16]` with [`Fields::bit`] and [`Fields::field`].
#[derive(Debug, Clone, Copy)]
struct Fields {
    /// Word 0.
    word: u32,
    /// LEN `[31:30]`: the length in words (1..=3).
    len: u32,
    /// rd `[11:8]` (stores put the data source here).
    rd: usize,
    /// rs1 `[7:4]`.
    rs1: usize,
    /// rs2 `[3:0]`.
    rs2: usize,
    /// The 32-bit immediate: word 1, or a short 1-word form's immediate (see
    /// [`short_imm`]).  Read unconditionally; unused by most 1-word forms.
    imm32: u32,
    /// The fall-through PC (the next instruction).
    next: u32,
}

impl Fields {
    /// Bit `n` of word 0.
    const fn bit(self, n: u32) -> bool {
        (self.word >> n) & 1 == 1
    }

    /// The `width`-bit field of word 0 whose lowest bit is `lo`.
    const fn field(self, lo: u32, width: u32) -> u32 {
        (self.word >> lo) & ((1 << width) - 1)
    }
}

/// Where execution goes after an instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Fall through to the next instruction.
    Next,
    /// Continue at this address.
    Jump(u32),
    /// Stop with the PC on this instruction.
    Stop(StopReason),
}

/// The instruction word is a reserved encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Illegal;

/// What a class handler returns.
type Exec = Result<Flow, Illegal>;

/// A class-3 / class-D compare predicate (PRED `[25:23]`).  `INV` gives the
/// negations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pred {
    /// Equal.
    Eq,
    /// Signed less-than.
    Lt,
    /// Signed less-or-equal.
    Le,
    /// Unsigned less-than.
    Ult,
    /// Unsigned less-or-equal.
    Ule,
}

impl Pred {
    /// Decode PRED; 5..=7 are reserved.
    const fn decode(bits: u32) -> Result<Self, Illegal> {
        Ok(match bits {
            0 => Self::Eq,
            1 => Self::Lt,
            2 => Self::Le,
            3 => Self::Ult,
            4 => Self::Ule,
            _ => return Err(Illegal),
        })
    }

    /// Whether `lhs PRED rhs` holds.
    const fn holds(self, lhs: u64, rhs: u64) -> bool {
        match self {
            Self::Eq => lhs == rhs,
            Self::Lt => (lhs as i64) < (rhs as i64),
            Self::Le => (lhs as i64) <= (rhs as i64),
            Self::Ult => lhs < rhs,
            Self::Ule => lhs <= rhs,
        }
    }
}

impl Cpu {
    /// Add with full ADD-family flag effects (zero/sign/carry/overflow).
    fn add_flags(&mut self, a: u64, b: u64, carry_in: u64) -> u64 {
        let (s1, c1) = a.overflowing_add(b);
        let (res, c2) = s1.overflowing_add(carry_in);
        self.flags.set_carry(c1 || c2);
        // signed overflow: operands same sign, result differs.
        let sa = (a >> 63) & 1;
        let sb = (b >> 63) & 1;
        let sr = (res >> 63) & 1;
        self.flags.set_overflow((sa == sb) && (sr != sa));
        self.flags.set_zs(res);
        res
    }

    /// Subtract with full SUB-family flag effects. `borrow_in` is SUBC's carry.
    fn sub_flags(&mut self, a: u64, b: u64, borrow_in: u64) -> u64 {
        // a - b - borrow_in.  carry_flag = borrow out (a < b + borrow_in).
        let (s1, b1) = a.overflowing_sub(b);
        let (res, b2) = s1.overflowing_sub(borrow_in);
        self.flags.set_carry(b1 || b2);
        let sa = (a >> 63) & 1;
        let sb = (b >> 63) & 1;
        let sr = (res >> 63) & 1;
        // signed overflow on subtraction: operands differ in sign and result sign != a.
        self.flags.set_overflow((sa != sb) && (sr != sa));
        self.flags.set_zs(res);
        res
    }

    /// The carry flag as an ADDC / SUBC carry-in.
    fn carry_in(&self) -> u64 {
        u64::from(self.flags.carry())
    }

    /// Set Z from `res` and pass it through (the logic and bit-field producers).
    const fn with_z(&mut self, res: u64) -> u64 {
        self.flags.set_zero(res == 0);
        res
    }

    /// Write `res` to `rd`, setting Z from it when `set_z` (the `F` bit).
    const fn write_z(&mut self, rd: usize, res: u64, set_z: bool) {
        if set_z {
            self.flags.set_zero(res == 0);
        }
        self.regs[rd] = res;
    }

    /// The 64-bit immediate of a 3-word form: lo32 at PC+4, hi32 at PC+8.
    fn imm64(&self) -> u64 {
        let lo = self.read32(self.pc.wrapping_add(4));
        let hi = self.read32(self.pc.wrapping_add(8));
        (u64::from(hi) << 32) | u64::from(lo)
    }

    /// Push a doubleword (SP grows down).
    pub(super) fn push(&mut self, value: u64) {
        self.sp = self.sp.wrapping_sub(8);
        self.write64(self.sp, value);
    }

    /// Pop a doubleword.
    fn pop(&mut self) -> u64 {
        let value = self.read64(self.sp);
        self.sp = self.sp.wrapping_add(8);
        value
    }

    /// Decode + execute a single v2 instruction word, then move the PC on.
    ///
    /// The word is decoded straight from its bit fields (`doc/ISA_ENCODING_V2.md` §1):
    /// LEN gives the instruction length, CLASS selects the handler, and each
    /// handler pulls its own attribute bits from `[25:16]`.
    pub(super) fn execute(&mut self, word: u32) {
        let len = word >> 30;
        if len == 0 {
            // LEN=00 is illegal — this is how a stale v1 binary fails fast.
            self.stop = Some(StopReason::InvalidOpcode(word));
            return;
        }
        // ISA v3 short 1-word forms carry their immediate in word 0 instead;
        // `short_imm` returns it already extended/scaled to the 32-bit value
        // the 2-word form would have held in word 1, so the class handlers
        // are shared between the two lengths.
        let f = Fields {
            word,
            len,
            rd: ((word >> 8) & 0xF) as usize,
            rs1: ((word >> 4) & 0xF) as usize,
            rs2: (word & 0xF) as usize,
            imm32: short_imm(word).unwrap_or_else(|| self.read32(self.pc.wrapping_add(4))),
            next: self.pc.wrapping_add(4 * len),
        };
        let flow = match f.field(26, 4) {
            0x1 => self.exec_alu_rr(f),
            0x2 => self.exec_alu_imm(f),
            0x3 => self.exec_compare(f),
            0x4 => self.exec_shift_bit(f),
            0x5 => self.exec_unary(f),
            0x6 => Ok(self.exec_load(f)),
            0x7 => Ok(self.exec_store(f)),
            0x8 => self.exec_branch(f),
            0x9 => self.exec_stack(f),
            0xA => self.exec_muldiv(f),
            0xB => self.exec_system(f),
            0xC => Ok(Flow::Next), // I/O (LCD) — modelled as a no-op (no architectural state)
            0xD if len == 1 => self.exec_fused_branch(f),
            _ => Err(Illegal),
        };
        match flow {
            Ok(Flow::Next) => self.pc = f.next,
            Ok(Flow::Jump(target)) => self.pc = target,
            Ok(Flow::Stop(StopReason::Halt)) => self.halted = true,
            Ok(Flow::Stop(reason)) => self.stop = Some(reason),
            Err(Illegal) => self.stop = Some(StopReason::InvalidOpcode(word)),
        }
    }

    /// CMPRR / CMPRV flag computation.
    ///
    /// `CMP` is now `SUB` without writeback (§1.2): it computes `a - b` and sets
    /// the **full** `Z/S/C/V` exactly like `SUB`, discarding the result.  The
    /// retired `E/L/U` regenerate from these on read.
    fn cmp(&mut self, a: u64, b: u64) {
        let _ = self.sub_flags(a, b, 0);
    }

    /// Class 1 — ALU reg-reg: `rd = rs1 OP rs2` (`doc/ISA_ENCODING_V2.md` §2).
    fn exec_alu_rr(&mut self, f: Fields) -> Exec {
        let op = f.field(22, 4);
        let a = self.regs[f.rs1];
        let b = self.regs[f.rs2];
        let res = match op {
            0 => self.add_flags(a, b, 0),               // ADDR
            1 => self.sub_flags(a, b, 0),               // SUBR
            2 => self.add_flags(a, b, self.carry_in()), // ADDC
            3 => self.sub_flags(a, b, self.carry_in()), // SUBC
            4 => self.with_z(a & b),                    // ANDR — RRR logic sets zero
            5 => self.with_z(a | b),                    // ORR
            6 => self.with_z(a ^ b),                    // XORR
            7 => (a as i64).min(b as i64) as u64,       // MINR (signed)
            8 => (a as i64).max(b as i64) as u64,       // MAXR
            9 => a.min(b),                              // MINUR (unsigned)
            10 => a.max(b),                             // MAXUR
            _ => return Err(Illegal),
        };
        // v3 D3: W [19] on ADD/SUB — result = sext of the low 32 bits; Z
        // follows the W result (S/C/V are the 64-bit operation's).
        self.regs[f.rd] = if f.bit(19) && op <= 1 { self.w32(res) } else { res };
        Ok(Flow::Next)
    }

    /// ISA v3 D3 W result: sign-extend the low 32 bits; Z from the W value.
    const fn w32(&mut self, res: u64) -> u64 {
        self.with_z(sign_extend(res, 4))
    }

    /// Class 2 — ALU immediate: `rd = rs1 OP ext(imm)` (MOV/LEA ignore rs1).
    fn exec_alu_imm(&mut self, f: Fields) -> Exec {
        let imm = ext(f.imm32, f.bit(20));
        let a = self.regs[f.rs1];
        self.regs[f.rd] = match f.field(22, 4) {
            0 => {
                // ADDI / ADDV; v3 D3 ADDIW = the 2-word form with W [19]
                // (the short form's [19:12] is its immediate).
                let r = self.add_flags(a, imm, 0);
                if f.len == 2 && f.bit(19) { self.w32(r) } else { r }
            }
            1 => self.sub_flags(a, imm, 0),                 // MINUSV
            2 => self.add_flags(a, imm, self.carry_in()),   // ADC-imm
            3 => self.sub_flags(a, imm, self.carry_in()),   // SBC-imm
            4 => a & imm,                                   // ANDV — no flags
            5 => a | imm,                                   // ORV
            6 => a ^ imm,                                   // XORV
            14 => u64::from(self.pc.wrapping_add(f.imm32)), // LEAPC: rd = PC + imm32
            // MOV: SETR (2-word, sign-extended) or SETR64 (3-word, full 64-bit).
            15 if f.len == 3 => self.imm64(),
            15 => imm,
            _ => return Err(Illegal),
        };
        Ok(Flow::Next)
    }

    /// Class 3 — compare: flag-setting CMP (B=0) or boolean `rd=0/1` (B=1).
    fn exec_compare(&mut self, f: Fields) -> Exec {
        let inv = f.bit(22);
        let boolean = f.bit(21);
        let sgn = f.bit(20);
        let short = f.len == 1 && sgn && !boolean;
        let lhs = self.regs[f.rs1];
        // 2-word forms (and the v3 short form, LEN=01 SGN=1 B=0) take a
        // sign/zero-extended immediate; other forms use rs2.
        let rhs = if f.len == 2 || short { ext(f.imm32, sgn) } else { self.regs[f.rs2] };
        // v3 D2: W [19] = 32-bit compare of the low halves (not the short
        // form, whose [19:12] is its immediate). Comparing {x[31:0], 0}
        // gives exactly the 32-bit Z/S/C/V and 32-bit signed/unsigned order —
        // the same trick the RTL uses at its dispatch mux.
        let (lhs, rhs) = if !short && f.bit(19) { (lhs << 32, rhs << 32) } else { (lhs, rhs) };
        if boolean {
            // PRED is only decoded (and only reserved) for the boolean form.
            let pred = Pred::decode(f.field(23, 3))?;
            self.regs[f.rd] = u64::from(pred.holds(lhs, rhs) ^ inv);
        } else {
            self.cmp(lhs, rhs);
        }
        Ok(Flow::Next)
    }

    /// Class D (ISA v3 B) — fused compare-and-branch, 1 word, flags untouched:
    /// `if (rs1 PRED rhs) ^ INV { PC += 4*simm13 }`.  PRED `[25:23]` as class 3
    /// (EQ, LT, LE, ULT, ULE), INV `[22]`, IMM `[21]` (rhs = simm4 `[3:0]`
    /// instead of rs2), simm13 word displacement `[20:8]`.
    fn exec_fused_branch(&mut self, f: Fields) -> Exec {
        let pred = Pred::decode(f.field(23, 3))?;
        let lhs = self.regs[f.rs1];
        let rhs = if f.bit(21) {
            i64::from(((f.word << 28) as i32) >> 28) as u64 // sign-extend [3:0]
        } else {
            self.regs[f.rs2]
        };
        Ok(if pred.holds(lhs, rhs) ^ f.bit(22) {
            let disp = ((f.word << 11) as i32) >> 19; // sign-extend [20:8]
            Flow::Jump(self.pc.wrapping_add((disp << 2) as u32))
        } else {
            Flow::Next
        })
    }

    /// Class 4 — shift / rotate / bit manipulation.
    fn exec_shift_bit(&mut self, f: Fields) -> Exec {
        let embedded = f.bit(21); // 0 = count in rs2[5:0], 1 = embedded N [20:15]
        let set_z = f.bit(14); // F
        let rd = f.rd;
        let a = self.regs[f.rs1];
        let cnt = if embedded { f.field(15, 6) } else { (self.regs[f.rs2] & 0x3F) as u32 };
        match f.field(22, 4) {
            0 => self.write_z(rd, a << cnt, set_z),                   // SHL
            1 => self.write_z(rd, a >> cnt, set_z),                   // SHR
            2 => self.write_z(rd, ((a as i64) >> cnt) as u64, set_z), // SAR
            3 => {
                // ROL; a 1-bit rotate with F also sets C to the bit rotated out.
                if set_z && cnt == 1 {
                    self.flags.set_carry(a >> 63 == 1);
                }
                self.write_z(rd, a.rotate_left(cnt), set_z);
            }
            4 => {
                // ROR
                if set_z && cnt == 1 {
                    self.flags.set_carry(a & 1 == 1);
                }
                self.write_z(rd, a.rotate_right(cnt), set_z);
            }
            5 => {
                // RCL — rotate left one bit through carry (ROLCR).
                let r = (a << 1) | self.carry_in();
                self.flags.set_carry(a >> 63 == 1);
                self.write_z(rd, r, set_z);
            }
            6 => {
                // RCR — rotate right one bit through carry (RORCR).
                let r = (a >> 1) | (self.carry_in() << 63);
                self.flags.set_carry(a & 1 == 1);
                self.write_z(rd, r, set_z);
            }
            8 => self.regs[rd] = a | (1 << cnt),  // BSET
            9 => self.regs[rd] = a & !(1 << cnt), // BCLR
            10 => self.regs[rd] = a ^ (1 << cnt), // BTGL
            11 => {
                // BTST — BTSTRR (reg) writes rd = bit; BTST-imm is flag-only (Z=~bit).
                let bit = (a >> cnt) & 1;
                if embedded {
                    self.flags.set_zero(bit == 0);
                } else {
                    self.regs[rd] = bit;
                }
            }
            12 => {
                // BEXTR — extract the imm-described field of the low 32 bits.
                // Z is always set from the result: the RTL writes it back with
                // set_zero (f_bextr) / fmask.zero (pipeline), although F is clear.
                let (start, mask) = bit_field(f.imm32);
                self.write_z(rd, ((a & 0xFFFF_FFFF) >> start) & mask, true);
            }
            13 => {
                // BDEP — deposit rs2's low field into rs1 (the base) at start;
                // the result is the low 32 bits, zero-extended.  No flags.
                let (start, mask) = bit_field(f.imm32);
                let field = (self.regs[f.rs2] & mask) << start;
                let clear = !(mask << start) & 0xFFFF_FFFF;
                self.regs[rd] = (a & clear) | (field & 0xFFFF_FFFF);
            }
            _ => return Err(Illegal),
        }
        Ok(Flow::Next)
    }

    /// Class 5 — unary: `rd = OP(rs1)`.
    fn exec_unary(&mut self, f: Fields) -> Exec {
        // SEXT/ZEXT SIZE [21:20]: 00=8 01=16 10=32 (11 acts as 32).
        let size_bytes = 1_usize << f.field(20, 2).min(2);
        let set_z = f.bit(19); // F
        let rd = f.rd;
        let a = self.regs[f.rs1];
        match f.field(22, 4) {
            0 => self.regs[rd] = a,                         // COPY
            1 => self.write_z(rd, a.wrapping_neg(), set_z), // NEG
            2 => self.write_z(rd, !a, set_z),               // NOT
            3 => {
                // ABS — INT_MIN wraps (result INT_MIN, overflow cleared); matches v1.
                if set_z {
                    self.flags.set_overflow(false);
                }
                self.write_z(rd, (a as i64).wrapping_abs() as u64, set_z);
            }
            4 => {
                // SEXT
                let r = sign_extend(a, size_bytes);
                if set_z {
                    self.flags.set_zs(r);
                }
                self.regs[rd] = r;
            }
            5 => self.write_z(rd, zero_extend(a, size_bytes), set_z), // ZEXT
            6 => self.regs[rd] = a.swap_bytes(),                      // BSWAP (64-bit)
            7 => self.regs[rd] = a.reverse_bits(),                    // BITREV
            8 => self.write_z(rd, u64::from(a.count_ones()), set_z),  // POPCNT
            9 => self.regs[rd] = u64::from(a.leading_zeros()),        // CLZ; CLZ(0)=64
            10 => self.regs[rd] = u64::from(a.trailing_zeros()),      // CTZ; CTZ(0)=64
            // GETF / SETFR: rd = {zero,equal,carry,overflow} in the top nibble [63:60].
            // `equal` is DERIVED (E = Z) — retired as storage per §1.5.
            12 => self.regs[rd] = u64::from(self.flags.to_getf()) << 60,
            14 => self.regs[rd] = self.add_flags(a, 1, 0), // INC
            15 => self.regs[rd] = self.sub_flags(a, 1, 0), // DEC
            _ => return Err(Illegal),
        }
        Ok(Flow::Next)
    }

    /// Class 6 — loads: `rd = ext(mem[EA])`.
    fn exec_load(&mut self, f: Fields) -> Flow {
        let size_bytes = 1_usize << f.field(24, 2); // 00=8 01=16 10=32 11=64
        let ea = self.ea(f) & load_align_mask(size_bytes, f.field(21, 2), f.bit(20));
        let val = if size_bytes == 8 {
            self.read64(ea)
        } else {
            self.read_sub(ea, size_bytes)
        };
        self.regs[f.rd] = if f.bit(23) { sign_extend(val, size_bytes) } else { val };
        Flow::Next
    }

    /// Class 7 — stores: `mem[EA] = reg[rd]` (rd field is the data source).
    fn exec_store(&mut self, f: Fields) -> Flow {
        let size_bytes = 1_usize << f.field(24, 2);
        let ea = self.ea(f) & store_align_mask(size_bytes, f.field(21, 2), f.bit(20));
        let data = self.regs[f.rd];
        if size_bytes == 8 {
            self.write64(ea, data);
        } else {
            self.write_sub(ea, data, size_bytes);
        }
        Flow::Next
    }

    /// Effective address of a load or store from its MODE field `[22:21]`.
    fn ea(&self, f: Fields) -> u32 {
        let base = self.regs[f.rs1] as u32;
        match f.field(21, 2) {
            0 => base,                                       // [rs1]
            1 => base.wrapping_add(f.imm32),                 // rs1 + imm32
            2 => f.imm32,                                    // [imm32] absolute
            _ => base.wrapping_add(self.regs[f.rs2] as u32), // rs1 + rs2
        }
    }

    /// Class 8 — branch / call.
    fn exec_branch(&mut self, f: Fields) -> Exec {
        let link = f.bit(25); // call (push return address)
        let rel = f.bit(24); // PC-relative displacement
        let rind = f.bit(23); // target = rs2
        let cond = self.eval_cond(f.field(19, 4)).ok_or(Illegal)?;
        if !(cond ^ f.bit(18)) {
            return Ok(Flow::Next);
        }
        let target = if rind {
            self.regs[f.rs2] as u32
        } else if rel {
            self.pc.wrapping_add(f.imm32)
        } else {
            f.imm32
        };
        if link {
            // Push the (zero-extended) return address = the fall-through PC.
            self.push(u64::from(f.next));
        }
        Ok(Flow::Jump(target))
    }

    /// Evaluate a class-8 COND code against the flags. `None` = reserved/illegal.
    ///
    /// Every relation is DERIVED from the unified `Z/S/C/V` register (§1.4); the
    /// retired `E/L/U` no longer exist as storage.  `INV` (applied by the caller)
    /// gives the negations (`NE`, `GE`, `GT`, `UGE`, `UGT`).
    pub(super) const fn eval_cond(&self, cond: u32) -> Option<bool> {
        let f = self.flags;
        Some(match cond {
            0 => true,                          // always
            1 => f.zero(),                      // Z
            2 => f.carry(),                     // C (raw carry / borrow)
            3 => f.overflow(),                  // V
            4 => f.sign(),                      // S
            5 => f.less(),                      // LT  = S ^ V
            6 => f.zero() || f.less(),          // LE  = Z | (S ^ V)
            7 => f.unsigned_less(),             // ULT = C
            8 => f.unsigned_less() || f.zero(), // ULE = C | Z
            9 => f.equal(),                     // E   = Z (alias of COND 1)
            _ => return None,
        })
    }

    /// Class 9 — stack / SP.
    fn exec_stack(&mut self, f: Fields) -> Exec {
        match f.field(22, 4) {
            0 => self.push(self.regs[f.rs1]), // PUSH rs1
            // PUSHI: PUSHV (2-word, zero-ext imm32) or PUSHV64 (3-word, imm64).
            1 if f.len == 3 => self.push(self.imm64()),
            1 => self.push(u64::from(f.imm32)),
            2 => self.regs[f.rd] = self.pop(),            // POP rd
            3 => self.regs[f.rd] = u64::from(self.sp),    // GETSP
            4 => self.sp = self.regs[f.rs1] as u32,       // SETSP
            5 => self.sp = self.sp.wrapping_add(f.imm32), // ADDSP (imm sign-ext, 32-bit add)
            // RET — restore PC from [31:0] of the popped slot.
            6 => return Ok(Flow::Jump(self.pop() as u32)),
            7 => return Ok(self.iret()),
            8 if f.len == 1 => {
                // v3 C: ENTER N — push R15; R15 = SP; SP -= 8*N  (N = [21:0]).
                self.push(self.regs[15]);
                self.regs[15] = u64::from(self.sp);
                self.sp = self.sp.wrapping_sub(f.field(0, 22) << 3);
            }
            9 if f.len == 1 => {
                // v3 C: LEAVE — SP = R15; pop R15.
                let fp = self.regs[15] as u32;
                self.regs[15] = self.read64(fp);
                self.sp = fp.wrapping_add(8);
            }
            10 if f.len == 1 => {
                // v3 C: LEAVERET — SP = R15; pop R15; pop PC (= LEAVE; RET).
                let fp = self.regs[15] as u32;
                self.regs[15] = self.read64(fp);
                let ra = self.read64(fp.wrapping_add(8));
                self.sp = fp.wrapping_add(16);
                return Ok(Flow::Jump(ra as u32));
            }
            _ => return Err(Illegal),
        }
        Ok(Flow::Next)
    }

    /// IRET — pop the saved interrupt context and restore PC`[31:0]`, flags`[38:32]`
    /// and `INT_MASK` `[42:39]`.
    ///
    /// The 7-bit saved word is `{Z,E,C,V,S,L,U}` (bit38→Z … bit32→U), but only the
    /// unified `Z/S/C/V` are consumed (§1.5); the derived `E`(bit37)/`L`(bit33)/
    /// `U`(bit32) bits are ignored and regenerate from `Z/S/C/V` on read.
    fn iret(&mut self) -> Flow {
        let ctx = self.pop();
        self.flags = Flags::from_ctx((ctx >> 32) as u8 & 0x7F);
        self.int_mask = ((ctx >> 39) & 0xF) as u8;
        Flow::Jump(ctx as u32)
    }

    /// Class A — mul / div / mod.
    fn exec_muldiv(&mut self, f: Fields) -> Exec {
        let sgn = f.bit(23);
        let high = f.bit(22); // high half (MUL only)
        let is_imm = f.len == 2;
        let rd = f.rd;
        let a = self.regs[f.rs1];
        let b = if is_imm { ext(f.imm32, sgn) } else { self.regs[f.rs2] };
        match f.field(24, 2) {
            0 => {
                // MUL
                let r = if high {
                    if sgn {
                        ((i128::from(a as i64) * i128::from(b as i64)) >> 64) as u64
                    } else {
                        ((u128::from(a) * u128::from(b)) >> 64) as u64
                    }
                } else if sgn {
                    (a as i64).wrapping_mul(b as i64) as u64
                } else {
                    a.wrapping_mul(b)
                };
                // v3 D3 MULW: W [19] on the low-half MUL.
                self.regs[rd] = if !high && f.bit(19) { sign_extend(r, 4) } else { r };
            }
            1 => {
                // DIV — divide-by-zero → all-ones result, overflow set, zero untouched.
                if b == 0 {
                    self.regs[rd] = u64::MAX;
                    self.flags.set_overflow(true);
                } else if sgn {
                    self.regs[rd] = (a as i64).wrapping_div(b as i64) as u64;
                } else {
                    self.regs[rd] = a / b;
                }
            }
            2 => {
                // MOD — mod-by-zero: reg forms write the dividend, MODV writes nothing;
                // overflow set either way, zero untouched.
                if b == 0 {
                    self.flags.set_overflow(true);
                    if !is_imm {
                        self.regs[rd] = a;
                    }
                } else if sgn {
                    self.regs[rd] = (a as i64).wrapping_rem(b as i64) as u64;
                } else {
                    self.regs[rd] = a % b;
                }
            }
            _ => return Err(Illegal),
        }
        Ok(Flow::Next)
    }

    /// Class B — system: NOP / HALT / WAIT / RESET / TRAP / DELAY.
    fn exec_system(&mut self, f: Fields) -> Exec {
        Ok(match f.field(16, 6) {
            0 | 5 => Flow::Next, // NOP / DELAY (DELAY's wait is not modelled)
            2 => {
                // WAIT: sleep until an interrupt source is ready; the IRQ
                // pushes the following instruction as the resume address.
                self.waiting = true;
                Flow::Next
            }
            1 => Flow::Stop(StopReason::Halt),
            3 => Flow::Jump(0x4), // RESET → PC = 0x4
            4 => Flow::Stop(StopReason::Trap),
            _ => return Err(Illegal),
        })
    }
}

/// ISA v3 short 1-word forms (`ISA_V3_PROPOSAL.md` §3): the immediate is in
/// word 0.  Returns the value the equivalent 2-word form carries in word 1
/// (extended and, for loads/stores and branches, scaled), or `None` when the
/// word is not a short form.
///
/// - A5 class 2, `LEN=01`: imm8 `[19:12]`, `SGN` `[20]` picks sign/zero extension.
/// - A3 class 3, `LEN=01`, `SGN=1`, `B=0`: simm8 `[19:12]`.
/// - A4 class 6/7, `LEN=01`, `MODE=01`: simm8 `[19:12]` << `SIZE` `[25:24]`.
/// - A1 class 8, `LEN=01`, `RIND=0`: simm18 `[17:0]` << 2 (PC-relative).
const fn short_imm(word: u32) -> Option<u32> {
    if word >> 30 != 1 {
        return None;
    }
    let i8v = (word >> 12) & 0xFF;
    let s8 = ((i8v as u8) as i8) as i32 as u32;
    match (word >> 26) & 0xF {
        0x2 => Some(if (word >> 20) & 1 == 1 { s8 } else { i8v }),
        0x3 if (word >> 20) & 1 == 1 && (word >> 21) & 1 == 0 => Some(s8),
        0x6 | 0x7 if (word >> 21) & 0x3 == 1 => Some(s8 << ((word >> 24) & 0x3)),
        0x8 if (word >> 23) & 1 == 0 => {
            let d = ((word << 14) as i32) >> 14; // sign-extend [17:0]
            Some((d << 2) as u32)
        }
        _ => None,
    }
}

/// A 32-bit immediate sign- (`signed`) or zero-extended to 64 bits.
const fn ext(imm: u32, signed: bool) -> u64 {
    if signed { imm as i32 as i64 as u64 } else { imm as u64 }
}

/// The BEXTR / BDEP field descriptor `{len[12:8], start[4:0]}` as
/// `(start, mask)`, the mask covering `len` low bits.
///
/// Both fields are 5 bits, so a field is 0..=31 bits wide and starts at bit
/// 0..=31 of the low word.  `len` 0 is an empty field: BEXTR yields 0 (and
/// sets Z), BDEP deposits nothing and just zero-extends the base's low 32 bits.
const fn bit_field(desc: u32) -> (u64, u64) {
    let start = (desc & 0x1F) as u64;
    let len = (desc >> 8) & 0x1F;
    (start, (1 << len) - 1)
}

/// Address-alignment mask for a class-6 **load** of `size_bytes` in addressing
/// `mode` with the `A` (force-align) bit. Mirrors the per-instruction masking of
/// the v1 CPU: `MEMGET32` is unaligned-tolerant while `LDIDX32` forces `&~3`;
/// the register/absolute 64-bit reads align while the indexed ones stay raw
/// unless `A=1`.
const fn load_align_mask(size_bytes: usize, mode: u32, force_align: bool) -> u32 {
    match size_bytes {
        2 => !1,
        4 => {
            if mode == 0 {
                !0 // MEMGET32 — unaligned-tolerant
            } else {
                !3 // LDIDX32
            }
        }
        8 => {
            if mode == 1 && !force_align {
                !0 // LDIDX64 — raw (A=0)
            } else {
                !7 // MEMREADRR / MEMGET64 / MEMREADR / LDIDX64A / LDIDX64R (aligned, like the store side)
            }
        }
        _ => !0, // byte access: no masking
    }
}

/// Address-alignment mask for a class-7 **store**. Sub-word stores always align
/// to their width; 64-bit stores align except the raw indexed `STIDX64` (A=0).
const fn store_align_mask(size_bytes: usize, mode: u32, force_align: bool) -> u32 {
    match size_bytes {
        2 => !1,
        4 => !3, // MEMSET32 / STIDX32
        8 => {
            if mode == 1 && !force_align {
                !0 // STIDX64 — raw
            } else {
                !7 // MEMSET64RR / MEMSET64 / STIDX64A / MEMSETR / STIDX64R
            }
        }
        _ => !0,
    }
}

/// Sign-extend the low `size_bytes` (1/2/4) of `val` to 64 bits; 8 keeps `val`.
const fn sign_extend(val: u64, size_bytes: usize) -> u64 {
    match size_bytes {
        1 => val as u8 as i8 as i64 as u64,
        2 => val as u16 as i16 as i64 as u64,
        4 => val as u32 as i32 as i64 as u64,
        _ => val,
    }
}

/// Zero-extend the low `size_bytes` (1/2/4) of `val` to 64 bits; 8 keeps `val`.
const fn zero_extend(val: u64, size_bytes: usize) -> u64 {
    match size_bytes {
        1 => val & 0xFF,
        2 => val & 0xFFFF,
        4 => val & 0xFFFF_FFFF,
        _ => val,
    }
}
