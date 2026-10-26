//! Instruction decode and execute: one handler per v2 CLASS.

use super::{Cpu, StopReason};

impl Cpu {
    /// Set zero/sign from a 64-bit result (the arithmetic producers).
    fn set_zs(&mut self, res: u64) {
        self.zero = res == 0;
        self.sign = (res >> 63) & 1 == 1;
    }

    /// Add with full ADD-family flag effects (zero/sign/carry/overflow).
    fn add_flags(&mut self, a: u64, b: u64, carry_in: u64) -> u64 {
        let (s1, c1) = a.overflowing_add(b);
        let (res, c2) = s1.overflowing_add(carry_in);
        self.carry = c1 || c2;
        // signed overflow: operands same sign, result differs.
        let sa = (a >> 63) & 1;
        let sb = (b >> 63) & 1;
        let sr = (res >> 63) & 1;
        self.overflow = (sa == sb) && (sr != sa);
        self.set_zs(res);
        res
    }

    /// Subtract with full SUB-family flag effects. `borrow_in` is SUBC's carry.
    fn sub_flags(&mut self, a: u64, b: u64, borrow_in: u64) -> u64 {
        // a - b - borrow_in.  carry_flag = borrow out (a < b + borrow_in).
        let (s1, b1) = a.overflowing_sub(b);
        let (res, b2) = s1.overflowing_sub(borrow_in);
        self.carry = b1 || b2;
        let sa = (a >> 63) & 1;
        let sb = (b >> 63) & 1;
        let sr = (res >> 63) & 1;
        // signed overflow on subtraction: operands differ in sign and result sign != a.
        self.overflow = (sa != sb) && (sr != sa);
        self.set_zs(res);
        res
    }

    /// Decode + execute a single v2 instruction word, advancing PC.
    ///
    /// The word is decoded straight from its bit fields (`doc/ISA_ENCODING_V2.md` §1):
    /// LEN gives the instruction length, CLASS selects the handler, and each
    /// handler pulls its own attribute bits from `[25:16]`.
    pub(super) fn step(&mut self, word: u32) {
        let len = word >> 30;
        let inst_len: u32 = match len {
            1 => 4,
            2 => 8,
            3 => 12,
            _ => {
                // LEN=00 is illegal — this is how a stale v1 binary fails fast.
                self.stop = Some(StopReason::InvalidOpcode(word));
                return;
            }
        };
        let class = (word >> 26) & 0xF;
        let rd = ((word >> 8) & 0xF) as usize;
        let rs1 = ((word >> 4) & 0xF) as usize;
        let rs2 = (word & 0xF) as usize;
        let next = self.pc.wrapping_add(inst_len);
        // imm32 (word at PC+4) — read unconditionally; unused for 1-word forms.
        // ISA v3 short 1-word forms carry their immediate in word 0 instead;
        // `short_imm` returns it already extended/scaled to the 32-bit value
        // the 2-word form would have held in word 1, so the class handlers
        // below are shared between the two lengths.
        let short = short_imm(word);
        let imm32 = short.unwrap_or_else(|| self.read32(self.pc.wrapping_add(4)));

        match class {
            0x1 => self.v2_alu_rr(word, rd, rs1, rs2, next),
            0x2 => self.v2_alu_imm(word, rd, rs1, imm32, len, next),
            0x3 => self.v2_compare(word, rd, rs1, rs2, imm32, len, next),
            0x4 => self.v2_shift_bit(word, rd, rs1, rs2, imm32, next),
            0x5 => self.v2_unary(word, rd, rs1, next),
            0x6 => self.v2_load(word, rd, rs1, rs2, imm32, next),
            0x7 => self.v2_store(word, rd, rs1, rs2, imm32, next),
            0x8 => self.v2_branch(word, rs2, imm32, next),
            0x9 => self.v2_stack(word, rd, rs1, imm32, len, next),
            0xA => self.v2_muldiv(word, rd, rs1, rs2, imm32, len, next),
            0xB => self.v2_system(word, next),
            0xC => self.pc = next, // I/O (LCD) — modelled as a no-op (no architectural state)
            0xD if len == 1 => self.v3_fused_branch(word, rs1, rs2, next),
            _ => self.stop = Some(StopReason::InvalidOpcode(word)),
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
    fn v2_alu_rr(&mut self, word: u32, rd: usize, rs1: usize, rs2: usize, next: u32) {
        let op = (word >> 22) & 0xF;
        let a = self.regs[rs1];
        let b = self.regs[rs2];
        let res = match op {
            0 => self.add_flags(a, b, 0),                     // ADDR
            1 => self.sub_flags(a, b, 0),                     // SUBR
            2 => self.add_flags(a, b, u64::from(self.carry)), // ADDC
            3 => self.sub_flags(a, b, u64::from(self.carry)), // SUBC
            4 => {
                // ANDR — RRR logic sets zero
                let r = a & b;
                self.zero = r == 0;
                r
            }
            5 => {
                // ORR
                let r = a | b;
                self.zero = r == 0;
                r
            }
            6 => {
                // XORR
                let r = a ^ b;
                self.zero = r == 0;
                r
            }
            7 => (a as i64).min(b as i64) as u64, // MINR (signed)
            8 => (a as i64).max(b as i64) as u64, // MAXR
            9 => a.min(b),                        // MINUR (unsigned)
            10 => a.max(b),                       // MAXUR
            _ => {
                self.stop = Some(StopReason::InvalidOpcode(word));
                return;
            }
        };
        // v3 D3: W [19] on ADD/SUB — result = sext of the low 32 bits; Z
        // follows the W result (S/C/V are the 64-bit operation's).
        let res = if (word >> 19) & 1 == 1 && op <= 1 { self.w32(res) } else { res };
        self.regs[rd] = res;
        self.pc = next;
    }

    /// ISA v3 D3 W result: sign-extend the low 32 bits; Z from the W value.
    fn w32(&mut self, res: u64) -> u64 {
        let w = i64::from(res as i32) as u64;
        self.zero = w == 0;
        w
    }

    /// Class 2 — ALU immediate: `rd = rs1 OP ext(imm)` (MOV/LEA ignore rs1).
    fn v2_alu_imm(&mut self, word: u32, rd: usize, rs1: usize, imm32: u32, len: u32, next: u32) {
        let op = (word >> 22) & 0xF;
        let sgn = (word >> 20) & 1 == 1;
        let ext = if sgn { i64::from(imm32 as i32) as u64 } else { u64::from(imm32) };
        let a = self.regs[rs1];
        match op {
            0 => {
                // ADDI / ADDV; v3 D3 ADDIW = the 2-word form with W [19]
                // (the short form's [19:12] is its immediate).
                let r = self.add_flags(a, ext, 0);
                self.regs[rd] = if len == 2 && (word >> 19) & 1 == 1 { self.w32(r) } else { r };
            }
            1 => self.regs[rd] = self.sub_flags(a, ext, 0),                     // MINUSV
            2 => self.regs[rd] = self.add_flags(a, ext, u64::from(self.carry)), // ADC-imm
            3 => self.regs[rd] = self.sub_flags(a, ext, u64::from(self.carry)), // SBC-imm
            4 => self.regs[rd] = a & ext,                                       // ANDV — no flags
            5 => self.regs[rd] = a | ext,                                       // ORV
            6 => self.regs[rd] = a ^ ext,                                       // XORV
            14 => self.regs[rd] = u64::from(self.pc.wrapping_add(imm32)),       // LEAPC: rd = PC + imm32
            15 => {
                // MOV: SETR (2-word, sign-extended) or SETR64 (3-word, full 64-bit).
                self.regs[rd] = if len == 3 {
                    let lo = self.read32(self.pc.wrapping_add(4));
                    let hi = self.read32(self.pc.wrapping_add(8));
                    (u64::from(hi) << 32) | u64::from(lo)
                } else {
                    ext
                };
            }
            _ => {
                self.stop = Some(StopReason::InvalidOpcode(word));
                return;
            }
        }
        self.pc = next;
    }

    /// Class 3 — compare: flag-setting CMP (B=0) or boolean `rd=0/1` (B=1).
    #[allow(clippy::too_many_arguments, reason = "decoded instruction fields are passed explicitly for clarity")]
    fn v2_compare(&mut self, word: u32, rd: usize, rs1: usize, rs2: usize, imm32: u32, len: u32, next: u32) {
        let pred = (word >> 23) & 0x7;
        let inv = (word >> 22) & 1 == 1;
        let boolean = (word >> 21) & 1 == 1;
        let sgn = (word >> 20) & 1 == 1;
        let short = len == 1 && sgn && !boolean;
        let lhs = self.regs[rs1];
        // 2-word forms (and the v3 short form, LEN=01 SGN=1 B=0) take a
        // sign/zero-extended immediate; other 1-word forms use rs2.
        let rhs = if len == 2 || short {
            if sgn { i64::from(imm32 as i32) as u64 } else { u64::from(imm32) }
        } else {
            self.regs[rs2]
        };
        // v3 D2: W [19] = 32-bit compare of the low halves (not the short
        // form, whose [19:12] is its immediate). Comparing {x[31:0], 0}
        // gives exactly the 32-bit Z/S/C/V and 32-bit signed/unsigned order —
        // the same trick the RTL uses at its dispatch mux.
        let (lhs, rhs) = if !short && (word >> 19) & 1 == 1 {
            (lhs << 32, rhs << 32)
        } else {
            (lhs, rhs)
        };
        if boolean {
            let base = match pred {
                0 => lhs == rhs,                   // EQ
                1 => (lhs as i64) < (rhs as i64),  // LT (signed)
                2 => (lhs as i64) <= (rhs as i64), // LE (signed)
                3 => lhs < rhs,                    // ULT
                4 => lhs <= rhs,                   // ULE
                _ => {
                    self.stop = Some(StopReason::InvalidOpcode(word));
                    return;
                }
            };
            self.regs[rd] = u64::from(base ^ inv);
        } else {
            self.cmp(lhs, rhs);
        }
        self.pc = next;
    }

    /// Class D (ISA v3 B) — fused compare-and-branch, 1 word, flags untouched:
    /// `if (rs1 PRED rhs) ^ INV { PC += 4*simm13 }`.  PRED `[25:23]` as class 3
    /// (EQ, LT, LE, ULT, ULE), INV `[22]`, IMM `[21]` (rhs = simm4 `[3:0]`
    /// instead of rs2), simm13 word displacement `[20:8]`.
    fn v3_fused_branch(&mut self, word: u32, rs1: usize, rs2: usize, next: u32) {
        let pred = (word >> 23) & 0x7;
        let inv = (word >> 22) & 1 == 1;
        let lhs = self.regs[rs1];
        let rhs = if (word >> 21) & 1 == 1 {
            i64::from(((word << 28) as i32) >> 28) as u64
        } else {
            self.regs[rs2]
        };
        let base = match pred {
            0 => lhs == rhs,
            1 => (lhs as i64) < (rhs as i64),
            2 => (lhs as i64) <= (rhs as i64),
            3 => lhs < rhs,
            4 => lhs <= rhs,
            _ => {
                self.stop = Some(StopReason::InvalidOpcode(word));
                return;
            }
        };
        if base ^ inv {
            let disp = ((word << 11) as i32) >> 19; // sign-extend [20:8]
            self.pc = self.pc.wrapping_add((disp << 2) as u32);
        } else {
            self.pc = next;
        }
    }

    /// Class 4 — shift / rotate / bit manipulation.
    fn v2_shift_bit(&mut self, word: u32, rd: usize, rs1: usize, rs2: usize, imm32: u32, next: u32) {
        let op = (word >> 22) & 0xF;
        let src = (word >> 21) & 1; // 0 = count in rs2[5:0], 1 = embedded N
        let n = (word >> 15) & 0x3F;
        let f = (word >> 14) & 1 == 1;
        let a = self.regs[rs1];
        let cnt = if src == 1 { n } else { (self.regs[rs2] & 0x3F) as u32 };
        match op {
            0 => {
                // SHL
                let r = a << cnt;
                if f {
                    self.zero = r == 0;
                }
                self.regs[rd] = r;
            }
            1 => {
                // SHR
                let r = a >> cnt;
                if f {
                    self.zero = r == 0;
                }
                self.regs[rd] = r;
            }
            2 => {
                // SAR
                let r = ((a as i64) >> cnt) as u64;
                if f {
                    self.zero = r == 0;
                }
                self.regs[rd] = r;
            }
            3 => {
                let r = a.rotate_left(cnt); // ROL
                if f {
                    self.zero = r == 0;
                    if cnt == 1 {
                        self.carry = (a >> 63) & 1 == 1;
                    }
                }
                self.regs[rd] = r;
            }
            4 => {
                let r = a.rotate_right(cnt); // ROR
                if f {
                    self.zero = r == 0;
                    if cnt == 1 {
                        self.carry = a & 1 == 1;
                    }
                }
                self.regs[rd] = r;
            }
            5 => {
                // RCL — rotate left through carry (ROLCR).
                let new_carry = (a >> 63) & 1 == 1;
                let r = (a << 1) | u64::from(self.carry);
                self.carry = new_carry;
                if f {
                    self.zero = r == 0;
                }
                self.regs[rd] = r;
            }
            6 => {
                // RCR — rotate right through carry (RORCR).
                let new_carry = a & 1 == 1;
                let r = (a >> 1) | (u64::from(self.carry) << 63);
                self.carry = new_carry;
                if f {
                    self.zero = r == 0;
                }
                self.regs[rd] = r;
            }
            8 => self.regs[rd] = a | (1 << cnt),  // BSET
            9 => self.regs[rd] = a & !(1 << cnt), // BCLR
            10 => self.regs[rd] = a ^ (1 << cnt), // BTGL
            11 => {
                // BTST — BTSTRR (reg) writes rd = bit; BTST-imm is flag-only (Z=~bit).
                let bit = (a >> cnt) & 1;
                if src == 0 {
                    self.regs[rd] = bit;
                } else {
                    self.zero = bit == 0;
                }
            }
            12 => {
                // BEXTR — extract imm-described field (start[4:0], len[12:8]); low 32 bits.
                // len is 5 bits, so a field is 0..=31 bits wide; len 0 extracts nothing (0).
                // Z is always set from the result: the RTL writes it back with
                // set_zero (f_bextr) / fmask.zero (pipeline), although F is clear.
                let start = u64::from(imm32 & 0x1F);
                let l = u64::from((imm32 >> 8) & 0x1F);
                let mask = (1_u64 << l) - 1;
                let field = ((a & 0xFFFF_FFFF) >> start) & mask;
                self.zero = field == 0;
                self.regs[rd] = field;
            }
            13 => {
                // BDEP — deposit rs2's low field into rs1(base) at start; low 32 bits.
                // Same descriptor as BEXTR (len 0 deposits nothing); no flags.
                let start = u64::from(imm32 & 0x1F);
                let l = u64::from((imm32 >> 8) & 0x1F);
                let mask = (1_u64 << l) - 1;
                let field = (self.regs[rs2] & mask) << start;
                let clear = !(mask << start) & 0xFFFF_FFFF;
                self.regs[rd] = (a & clear) | (field & 0xFFFF_FFFF);
            }
            _ => {
                self.stop = Some(StopReason::InvalidOpcode(word));
                return;
            }
        }
        self.pc = next;
    }

    /// Class 5 — unary: `rd = OP(rs1)`.
    fn v2_unary(&mut self, word: u32, rd: usize, rs1: usize, next: u32) {
        let op = (word >> 22) & 0xF;
        let size = (word >> 20) & 0x3; // SEXT/ZEXT: 00=8 01=16 10=32
        let f = (word >> 19) & 1 == 1;
        let a = self.regs[rs1];
        match op {
            0 => self.regs[rd] = a, // COPY
            1 => {
                // NEG
                let r = a.wrapping_neg();
                if f {
                    self.zero = r == 0;
                }
                self.regs[rd] = r;
            }
            2 => {
                // NOT
                let r = !a;
                if f {
                    self.zero = r == 0;
                }
                self.regs[rd] = r;
            }
            3 => {
                // ABS — INT_MIN wraps (result INT_MIN, overflow cleared); matches v1.
                let r = (a as i64).wrapping_abs() as u64;
                if f {
                    self.zero = r == 0;
                    self.overflow = false;
                }
                self.regs[rd] = r;
            }
            4 => {
                let r = match size {
                    0 => i64::from(a as i8),
                    1 => i64::from(a as i16),
                    _ => i64::from(a as i32),
                } as u64;
                if f {
                    self.set_zs(r);
                }
                self.regs[rd] = r;
            }
            5 => {
                let r = match size {
                    0 => a & 0xFF,
                    1 => a & 0xFFFF,
                    _ => a & 0xFFFF_FFFF,
                };
                if f {
                    self.zero = r == 0;
                }
                self.regs[rd] = r;
            }
            6 => self.regs[rd] = a.swap_bytes(),   // BSWAP (64-bit)
            7 => self.regs[rd] = a.reverse_bits(), // BITREV
            8 => {
                // POPCNT
                let r = u64::from(a.count_ones());
                if f {
                    self.zero = r == 0;
                }
                self.regs[rd] = r;
            }
            9 => self.regs[rd] = u64::from(a.leading_zeros()),   // CLZ; CLZ(0)=64
            10 => self.regs[rd] = u64::from(a.trailing_zeros()), // CTZ; CTZ(0)=64
            12 => {
                // GETF / SETFR: rd = {zero,equal,carry,overflow} in the top nibble [63:60].
                // `equal` is DERIVED (E = Z) — retired as storage per §1.5.
                let mut v = 0_u64;
                if self.zero {
                    v |= 1 << 63;
                }
                if self.flag_e() {
                    v |= 1 << 62;
                }
                if self.carry {
                    v |= 1 << 61;
                }
                if self.overflow {
                    v |= 1 << 60;
                }
                self.regs[rd] = v;
            }
            14 => self.regs[rd] = self.add_flags(a, 1, 0), // INC
            15 => self.regs[rd] = self.sub_flags(a, 1, 0), // DEC
            _ => {
                self.stop = Some(StopReason::InvalidOpcode(word));
                return;
            }
        }
        self.pc = next;
    }

    /// Class 6 — loads: `rd = ext(mem[EA])`.
    fn v2_load(&mut self, word: u32, rd: usize, rs1: usize, rs2: usize, imm32: u32, next: u32) {
        let size = (word >> 24) & 0x3; // 00=8 01=16 10=32 11=64
        let sgn = (word >> 23) & 1 == 1;
        let mode = (word >> 21) & 0x3; // 00=[rs1] 01=rs1+imm32 10=[imm32] 11=rs1+rs2
        let a = (word >> 20) & 1;
        let size_bytes = 1_usize << size;
        let ea_raw = self.ea(mode, rs1, rs2, imm32);
        let ea = ea_raw & load_align_mask(size_bytes, mode, a);
        let val = if size_bytes == 8 {
            self.read64(ea)
        } else {
            self.read_sub(ea, size_bytes)
        };
        self.regs[rd] = if sgn && size_bytes < 8 { sign_extend(val, size_bytes) } else { val };
        self.pc = next;
    }

    /// Class 7 — stores: `mem[EA] = reg[rd]` (rd field is the data source).
    fn v2_store(&mut self, word: u32, rd: usize, rs1: usize, rs2: usize, imm32: u32, next: u32) {
        let size = (word >> 24) & 0x3;
        let mode = (word >> 21) & 0x3;
        let a = (word >> 20) & 1;
        let size_bytes = 1_usize << size;
        let ea = self.ea(mode, rs1, rs2, imm32) & store_align_mask(size_bytes, mode, a);
        let data = self.regs[rd];
        if size_bytes == 8 {
            self.write64(ea, data);
        } else {
            self.write_sub(ea, data, size_bytes);
        }
        self.pc = next;
    }

    /// Effective-address computation shared by loads and stores (MODE field).
    fn ea(&self, mode: u32, rs1: usize, rs2: usize, imm32: u32) -> u32 {
        match mode {
            0 => self.regs[rs1] as u32,                                       // [rs1]
            1 => (self.regs[rs1] as u32).wrapping_add(imm32),                 // rs1 + imm32
            2 => imm32,                                                       // [imm32] absolute
            _ => (self.regs[rs1] as u32).wrapping_add(self.regs[rs2] as u32), // rs1 + rs2
        }
    }

    /// Class 8 — branch / call.
    fn v2_branch(&mut self, word: u32, rs2: usize, imm32: u32, next: u32) {
        let link = (word >> 25) & 1 == 1; // call (push return address)
        let rel = (word >> 24) & 1 == 1; // PC-relative displacement
        let rind = (word >> 23) & 1 == 1; // target = rs2
        let cond_code = (word >> 19) & 0xF;
        let inv = (word >> 18) & 1 == 1;
        let Some(cond) = self.eval_cond(cond_code) else {
            self.stop = Some(StopReason::InvalidOpcode(word));
            return;
        };
        let taken = cond ^ inv;
        let target = if rind {
            self.regs[rs2] as u32
        } else if rel {
            self.pc.wrapping_add(imm32)
        } else {
            imm32
        };
        if taken {
            if link {
                // Push the (zero-extended) return address = the fall-through PC.
                self.sp = self.sp.wrapping_sub(8);
                self.write64(self.sp, u64::from(next));
            }
            self.pc = target;
        } else {
            self.pc = next;
        }
    }

    /// Evaluate a class-8 COND code against the flags. `None` = reserved/illegal.
    ///
    /// Every relation is DERIVED from the unified `Z/S/C/V` register (§1.4); the
    /// retired `E/L/U` no longer exist as storage.  `INV` (applied by the caller)
    /// gives the negations (`NE`, `GE`, `GT`, `UGE`, `UGT`).
    pub(super) fn eval_cond(&self, cond: u32) -> Option<bool> {
        Some(match cond {
            0 => true,                       // always
            1 => self.zero,                  // Z
            2 => self.carry,                 // C (raw carry / borrow)
            3 => self.overflow,              // V
            4 => self.sign,                  // S
            5 => self.flag_l(),              // LT  = S ^ V
            6 => self.zero || self.flag_l(), // LE  = Z | (S ^ V)
            7 => self.flag_u(),              // ULT = C
            8 => self.flag_u() || self.zero, // ULE = C | Z
            9 => self.flag_e(),              // E   = Z (alias of COND 1)
            _ => return None,
        })
    }

    /// Class 9 — stack / SP.
    fn v2_stack(&mut self, word: u32, rd: usize, rs1: usize, imm32: u32, len: u32, next: u32) {
        let op = (word >> 22) & 0xF;
        match op {
            0 => {
                // PUSH rs1
                self.sp = self.sp.wrapping_sub(8);
                self.write64(self.sp, self.regs[rs1]);
                self.pc = next;
            }
            1 => {
                // PUSHI: PUSHV (2-word, zero-ext imm32) or PUSHV64 (3-word, imm64).
                let v = if len == 3 {
                    let lo = self.read32(self.pc.wrapping_add(4));
                    let hi = self.read32(self.pc.wrapping_add(8));
                    (u64::from(hi) << 32) | u64::from(lo)
                } else {
                    u64::from(imm32)
                };
                self.sp = self.sp.wrapping_sub(8);
                self.write64(self.sp, v);
                self.pc = next;
            }
            2 => {
                // POP rd
                self.regs[rd] = self.read64(self.sp);
                self.sp = self.sp.wrapping_add(8);
                self.pc = next;
            }
            3 => {
                // GETSP
                self.regs[rd] = u64::from(self.sp);
                self.pc = next;
            }
            4 => {
                // SETSP
                self.sp = self.regs[rs1] as u32;
                self.pc = next;
            }
            5 => {
                // ADDSP (imm sign-ext, 32-bit add)
                self.sp = self.sp.wrapping_add(imm32);
                self.pc = next;
            }
            6 => {
                // RET — restore PC from [31:0] of the popped slot.
                let ra = self.read64(self.sp);
                self.sp = self.sp.wrapping_add(8);
                self.pc = ra as u32;
            }
            7 => self.iret(), // IRET
            8 if len == 1 => {
                // v3 C: ENTER N — push R15; R15 = SP; SP -= 8*N  (N = [21:0]).
                self.sp = self.sp.wrapping_sub(8);
                self.write64(self.sp, self.regs[15]);
                self.regs[15] = u64::from(self.sp);
                self.sp = self.sp.wrapping_sub((word & 0x3F_FFFF) << 3);
                self.pc = next;
            }
            9 if len == 1 => {
                // v3 C: LEAVE — SP = R15; pop R15.
                let fp = self.regs[15] as u32;
                self.regs[15] = self.read64(fp);
                self.sp = fp.wrapping_add(8);
                self.pc = next;
            }
            10 if len == 1 => {
                // v3 C: LEAVERET — SP = R15; pop R15; pop PC (= LEAVE; RET).
                let fp = self.regs[15] as u32;
                self.regs[15] = self.read64(fp);
                let ra = self.read64(fp.wrapping_add(8));
                self.sp = fp.wrapping_add(16);
                self.pc = ra as u32;
            }
            _ => self.stop = Some(StopReason::InvalidOpcode(word)),
        }
    }

    /// IRET — pop the saved interrupt context and restore PC`[31:0]`, flags`[38:32]`
    /// and `INT_MASK` `[42:39]`.
    ///
    /// The 7-bit saved word is `{Z,E,C,V,S,L,U}` (bit38→Z … bit32→U), but only the
    /// unified `Z/S/C/V` are consumed (§1.5); the derived `E`(bit37)/`L`(bit33)/
    /// `U`(bit32) bits are ignored and regenerate from `Z/S/C/V` on read.
    fn iret(&mut self) {
        let ctx = self.read64(self.sp);
        self.sp = self.sp.wrapping_add(8);
        self.pc = ctx as u32;
        self.zero = (ctx >> 38) & 1 == 1;
        self.carry = (ctx >> 36) & 1 == 1;
        self.overflow = (ctx >> 35) & 1 == 1;
        self.sign = (ctx >> 34) & 1 == 1;
        self.int_mask = ((ctx >> 39) & 0xF) as u8;
    }

    /// Class A — mul / div / mod.
    #[allow(clippy::too_many_arguments, reason = "decoded instruction fields are passed explicitly for clarity")]
    fn v2_muldiv(&mut self, word: u32, rd: usize, rs1: usize, rs2: usize, imm32: u32, len: u32, next: u32) {
        let op = (word >> 24) & 0x3; // 00=MUL 01=DIV 10=MOD
        let sgn = (word >> 23) & 1 == 1;
        let h = (word >> 22) & 1 == 1; // high half (MUL only)
        let a = self.regs[rs1];
        let is_imm = len == 2;
        let b = if is_imm {
            if sgn { i64::from(imm32 as i32) as u64 } else { u64::from(imm32) }
        } else {
            self.regs[rs2]
        };
        match op {
            0 => {
                // MUL
                self.regs[rd] = if h {
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
                if !h && (word >> 19) & 1 == 1 {
                    self.regs[rd] = i64::from(self.regs[rd] as i32) as u64;
                }
            }
            1 => {
                // DIV — divide-by-zero → all-ones result, overflow set, zero untouched.
                if b == 0 {
                    self.regs[rd] = 0xFFFF_FFFF_FFFF_FFFF;
                    self.overflow = true;
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
                    self.overflow = true;
                    if !is_imm {
                        self.regs[rd] = a;
                    }
                } else if sgn {
                    self.regs[rd] = (a as i64).wrapping_rem(b as i64) as u64;
                } else {
                    self.regs[rd] = a % b;
                }
            }
            _ => {
                self.stop = Some(StopReason::InvalidOpcode(word));
                return;
            }
        }
        self.pc = next;
    }

    /// Class B — system: NOP / HALT / WAIT / RESET / TRAP / DELAY.
    fn v2_system(&mut self, word: u32, next: u32) {
        let op = (word >> 16) & 0x3F;
        match op {
            0 | 5 => self.pc = next, // NOP / DELAY (DELAY's wait is not modelled)
            2 => {
                // WAIT: sleep until an interrupt source is ready; the IRQ
                // pushes the following instruction as the resume address.
                self.pc = next;
                self.waiting = true;
            }
            1 => self.halted = true,                 // HALT
            3 => self.pc = 0x4,                      // RESET → PC = 0x4
            4 => self.stop = Some(StopReason::Trap), // TRAP
            _ => self.stop = Some(StopReason::InvalidOpcode(word)),
        }
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

/// Address-alignment mask for a class-6 **load** of `size_bytes` in addressing
/// `mode` with the `A` (force-align) bit. Mirrors the per-instruction masking of
/// the v1 CPU: `MEMGET32` is unaligned-tolerant while `LDIDX32` forces `&~3`;
/// the register/absolute 64-bit reads align while the indexed ones stay raw
/// unless `A=1`.
const fn load_align_mask(size_bytes: usize, mode: u32, a: u32) -> u32 {
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
            if mode == 1 && a == 0 {
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
const fn store_align_mask(size_bytes: usize, mode: u32, a: u32) -> u32 {
    match size_bytes {
        2 => !1,
        4 => !3, // MEMSET32 / STIDX32
        8 => {
            if mode == 1 && a == 0 {
                !0 // STIDX64 — raw
            } else {
                !7 // MEMSET64RR / MEMSET64 / STIDX64A / MEMSETR / STIDX64R
            }
        }
        _ => !0,
    }
}

/// Sign-extend the low `size_bytes` (1/2/4) of `val` to 64 bits.
const fn sign_extend(val: u64, size_bytes: usize) -> u64 {
    match size_bytes {
        1 => val as u8 as i8 as i64 as u64,
        2 => val as u16 as i16 as i64 as u64,
        4 => val as u32 as i32 as i64 as u64,
        _ => val,
    }
}
