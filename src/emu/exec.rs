//! Instruction decode and execute: one handler per v2 CLASS.
//!
//! [`Cpu::execute`] decodes the fields every class shares into [`Fields`],
//! dispatches on CLASS, and applies the handler's [`Flow`]: the handlers never
//! touch the PC themselves.  A reserved encoding is an [`Illegal`] error and
//! an MMU stage 1 exception (a privileged instruction in user mode, a
//! user-mode MMIO access, a misaligned access, TRAP, SYSCALL, ...) a
//! [`Fault::Exception`]; both reach [`Cpu::raise`] in one place.  A handler
//! makes every check that can fault before its first side effect, so a
//! faulting instruction changes nothing (`MMU_PLAN.md` §4.1-§4.2).
//!
//! Which encodings are reserved follows the board core's decoder,
//! `f_decode_raw` in `pipeline_core.sv` (`KlaussCPU` master, lines 388-715):
//! every reserved bit, unused register field and attribute combination it
//! traps on, the emulator traps on too.  The line numbers cited below are in
//! that file.

use super::flags::Flags;
use super::mmu::{Mapped, XlateFault};
use super::system::{Cause, sr_user_readable, sr_user_writable};
use super::{CTX_FLAGS_SHIFT, CTX_INT_MASK_SHIFT, CTX_IRQ_DISABLED_SHIFT, CTX_PREV_MODE_SHIFT, Cpu, StopReason};
use crate::layout::is_mmio;

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
    /// [`short_imm`]).  Unused by most 1-word forms.
    imm32: u32,
    /// Word 2, the high half of a 3-word form's 64-bit immediate.
    word2: u32,
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

    /// The 64-bit immediate of a 3-word form: lo32 in word 1, hi32 in word 2.
    const fn imm64(self) -> u64 {
        ((self.word2 as u64) << 32) | self.imm32 as u64
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

/// Why an instruction did not execute.  Nothing of it has happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// A reserved encoding (ECAUSE ILLEGAL).
    Illegal,
    /// Any other exception, with its `EADDR`.
    Exception(Cause, u32),
    /// A memory access that did not translate (the user-mode MMIO rule,
    /// the MPU, the TLB), with the detail `ECAUSE[8]` and `TLB_HI` take.
    Mmu(XlateFault),
}

impl From<XlateFault> for Fault {
    fn from(fault: XlateFault) -> Self {
        Self::Mmu(fault)
    }
}

impl From<Illegal> for Fault {
    fn from(_: Illegal) -> Self {
        Self::Illegal
    }
}

/// What a class handler returns.
type Exec = Result<Flow, Fault>;

/// The PRIVILEGED exception for the instruction `f` (`EADDR` = its word 0).
const fn privileged(f: Fields) -> Fault {
    Fault::Exception(Cause::Privileged, f.word)
}

/// `DELAYR` / `DELAYV` wait `value << DELAY_SHIFT` cycles (`pipeline_core.sv`
/// :1622: `dly_max = {value, 13'b0}`).
const DELAY_SHIFT: u32 = 13;

/// `Ok` when the RTL decoder accepts the encoding (`ok`), else [`Illegal`].
const fn legal(ok: bool) -> Result<(), Illegal> {
    if ok { Ok(()) } else { Err(Illegal) }
}

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
    /// Decode a class-3 PRED; 5..=7 are reserved there.
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

    /// Decode a fused-branch PRED (class D): 0..=4 as class 3, and 5..=7 the
    /// 32-bit EQW, LTW, ULTW (hw-proposals 6, `pipeline_core.sv`
    /// `f_fbr_pred`), returned with `true` for a compare of the low halves.
    const fn decode_fused(bits: u32) -> (Self, bool) {
        match bits {
            0 => (Self::Eq, false),
            1 => (Self::Lt, false),
            2 => (Self::Le, false),
            3 => (Self::Ult, false),
            4 => (Self::Ule, false),
            5 => (Self::Eq, true),
            6 => (Self::Lt, true),
            _ => (Self::Ult, true),
        }
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
        self.regs[rd] = if set_z { self.with_z(res) } else { res };
    }

    /// Check and translate the doubleword a push writes, at SP - 8.
    fn map_push(&self) -> Result<Mapped, XlateFault> {
        self.map_data(self.sp.wrapping_sub(8), 1, true)
    }

    /// Check and translate the doubleword a pop reads, at SP.
    fn map_pop(&self) -> Result<Mapped, XlateFault> {
        self.map_data(self.sp, 1, false)
    }

    /// Push a doubleword (SP grows down) to `slot`, from [`Cpu::map_push`].
    fn push(&mut self, slot: Mapped, value: u64) {
        self.write64(slot, value);
        self.sp = self.sp.wrapping_sub(8);
    }

    /// Pop a doubleword from `slot`, from [`Cpu::map_pop`].
    fn pop(&mut self, slot: Mapped) -> u64 {
        let value = self.read64(slot);
        self.sp = self.sp.wrapping_add(8);
        value
    }

    /// Decode + execute a single v2 instruction, then move the PC on.
    /// `words` are its words as [`Cpu::fetch`] translated and read them
    /// (word 0, then the immediate words, 0 past its length).
    ///
    /// The word is decoded straight from its bit fields (`doc/ISA_ENCODING_V2.md` §1):
    /// LEN gives the instruction length, CLASS selects the handler, and each
    /// handler pulls its own attribute bits from `[25:16]`.
    ///
    /// Returns whether the instruction retired; one that raised an
    /// exception did not ([`Cpu::raise`]).
    pub(super) fn execute(&mut self, words: [u32; 3]) -> bool {
        let word = words[0];
        let len = word >> 30;
        if len == 0 {
            // LEN=00 is illegal — this is how a stale v1 binary fails fast.
            return self.raise(Cause::Illegal, word, word, self.pc.wrapping_add(4));
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
            imm32: short_imm(word).unwrap_or(words[1]),
            word2: words[2],
            next: self.pc.wrapping_add(4 * len),
        };
        let flow = match f.field(26, 4) {
            0x1 => self.exec_alu_rr(f),
            0x2 => self.exec_alu_imm(f),
            0x3 => self.exec_compare(f),
            0x4 => self.exec_shift_bit(f),
            0x5 => self.exec_unary(f),
            0x6 => self.exec_load(f),
            0x7 => self.exec_store(f),
            0x8 => self.exec_branch(f),
            0x9 => self.exec_stack(f),
            0xA => self.exec_muldiv(f),
            0xB => self.exec_system(f),
            0xC => self.exec_lcd(f),
            0xD if len <= 2 => self.exec_fused_branch(f),
            _ => Err(Fault::Illegal),
        };
        match flow {
            Ok(Flow::Next) => self.pc = f.next,
            Ok(Flow::Jump(target)) => self.pc = target,
            Ok(Flow::Stop(reason)) => self.stop = Some(reason),
            // ECAUSE ILLEGAL: EADDR is the instruction word.
            Err(Fault::Illegal) => return self.raise(Cause::Illegal, word, word, f.next),
            Err(Fault::Exception(cause, eaddr)) => return self.raise(cause, eaddr, word, f.next),
            Err(Fault::Mmu(fault)) => return self.raise_fault(fault, word, f.next),
        }
        true
    }

    /// CMPRR / CMPRV flag computation.
    ///
    /// `CMP` is now `SUB` without writeback (`doc/ISA_ENCODING_V2.md` §7.2): it computes `a - b` and sets
    /// the **full** `Z/S/C/V` exactly like `SUB`, discarding the result.  The
    /// retired `E/L/U` regenerate from these on read.
    fn cmp(&mut self, a: u64, b: u64) {
        let _ = self.sub_flags(a, b, 0);
    }

    /// Class 1 — ALU reg-reg: `rd = rs1 OP rs2` (`doc/ISA_ENCODING_V2.md` §2).
    fn exec_alu_rr(&mut self, f: Fields) -> Exec {
        let op = f.field(22, 4);
        let w = f.bit(19);
        // One word, SGN = 0, [18:12] = 0, and W only on ADD/SUB (:402-403);
        // F set on ADD/SUB/ADC/SBC and clear on the rest (:408-418).
        legal(f.len == 1 && !f.bit(20) && f.field(12, 7) == 0 && (!w || op <= 1))?;
        legal(f.bit(21) == (op <= 3))?;
        let a = self.regs[f.rs1];
        let b = self.regs[f.rs2];
        let res = match op {
            0 => self.add_flags(a, b, 0),               // ADDR
            1 => self.sub_flags(a, b, 0),               // SUBR
            2 => self.add_flags(a, b, self.carry_in()), // ADDC
            3 => self.sub_flags(a, b, self.carry_in()), // SUBC
            // AND / OR / XOR write no flags: their decode sets no fmask
            // (pipeline_core.sv:413-415).
            4 => a & b,                           // ANDR
            5 => a | b,                           // ORR
            6 => a ^ b,                           // XORR
            7 => (a as i64).min(b as i64) as u64, // MINR (signed)
            8 => (a as i64).max(b as i64) as u64, // MAXR
            9 => a.min(b),                        // MINUR (unsigned)
            10 => a.max(b),                       // MAXUR
            _ => return Err(Fault::Illegal),
        };
        // v3 D3: W [19] on ADD/SUB — result = sext of the low 32 bits; Z
        // follows the W result (S/C/V are the 64-bit operation's).
        self.regs[f.rd] = if w { self.w32(res) } else { res };
        Ok(Flow::Next)
    }

    /// ISA v3 D3 W result: sign-extend the low 32 bits; Z from the W value.
    const fn w32(&mut self, res: u64) -> u64 {
        self.with_z(sign_extend(res, 4))
    }

    /// Class 2 — ALU immediate: `rd = rs1 OP ext(imm)` (MOV/LEA ignore rs1).
    fn exec_alu_imm(&mut self, f: Fields) -> Exec {
        let op = f.field(22, 4);
        if f.len == 3 {
            // SETR64: MOV with [21:12], rs1 and rs2 zero (:431-433).
            legal(op == 15 && f.field(12, 10) == 0 && f.field(0, 8) == 0)?;
            self.regs[f.rd] = f.imm64();
            return Ok(Flow::Next);
        }
        if f.len == 1 && op == 14 && f.bit(21) {
            // hw-proposals 9 LEAPC.S: rd = PC + sext(simm16), the offset in
            // {[19:12], [7:0]} (`short_imm`).  The RTL decodes it with SGN
            // [20] clear, and also with SGN set when rs2 [3:0] is zero (its
            // generic short ALU path), the same offset either way.
            legal(!f.bit(20) || f.rs2 == 0)?;
            self.regs[f.rd] = u64::from(self.pc.wrapping_add(f.imm32));
            return Ok(Flow::Next);
        }
        // v3 D3 ADDIW = the 2-word ADD with W [19] (the short form's [19:12]
        // is its immediate).
        let w = f.len == 2 && f.bit(19);
        // rs2 = 0; the 2-word form also needs [18:12] = 0 and W only on ADD
        // (:404-406); F as class 1, and MIN/MAX are reg-reg only (:408-428).
        legal(f.rs2 == 0 && (f.len == 1 || (f.field(12, 7) == 0 && (!w || op == 0))))?;
        legal(f.bit(21) == (op <= 3))?;
        let imm = ext(f.imm32, f.bit(20));
        let a = self.regs[f.rs1];
        self.regs[f.rd] = match op {
            0 => {
                // ADDI / ADDV / ADDIW
                let r = self.add_flags(a, imm, 0);
                if w { self.w32(r) } else { r }
            }
            1 => self.sub_flags(a, imm, 0),                 // MINUSV
            2 => self.add_flags(a, imm, self.carry_in()),   // ADC-imm
            3 => self.sub_flags(a, imm, self.carry_in()),   // SBC-imm
            4 => a & imm,                                   // ANDV — no flags
            5 => a | imm,                                   // ORV
            6 => a ^ imm,                                   // XORV
            14 => u64::from(self.pc.wrapping_add(f.imm32)), // LEAPC: rd = PC + imm32
            15 => imm,                                      // MOV: SETR (sign- or zero-extended per SGN)
            _ => return Err(Fault::Illegal),
        };
        Ok(Flow::Next)
    }

    /// Class 3 — compare: flag-setting CMP (B=0) or boolean `rd=0/1` (B=1).
    fn exec_compare(&mut self, f: Fields) -> Exec {
        let inv = f.bit(22);
        let boolean = f.bit(21);
        let sgn = f.bit(20);
        let short = f.len == 1 && sgn && !boolean;
        // The reg form (SGN = 0), the 2-word form (rs2 = 0) and the short
        // form (rs2 = 0), each with [18:12] = 0 outside the immediate (:440-442).
        legal(match f.len {
            1 => (!sgn && f.field(12, 7) == 0) || (short && f.rs2 == 0),
            2 => f.field(12, 7) == 0 && f.rs2 == 0,
            _ => false,
        })?;
        let lhs = self.regs[f.rs1];
        // 2-word forms (and the v3 short form, LEN=01 SGN=1 B=0) take a
        // sign/zero-extended immediate; other forms use rs2.
        let rhs = if f.len == 2 || short { ext(f.imm32, sgn) } else { self.regs[f.rs2] };
        // v3 D2: W [19] = 32-bit compare of the low halves (not the short
        // form, whose [19:12] is its immediate). Comparing {x[31:0], 0}
        // gives exactly the 32-bit Z/S/C/V and 32-bit signed/unsigned order —
        // the same trick the RTL uses at its dispatch mux.
        // hw-proposals 9: the short form with INV [22] set is CMPRVW.S, the
        // 32-bit compare against simm8.
        let w = if short { inv } else { f.bit(19) };
        let (lhs, rhs) = if w { (lhs << 32, rhs << 32) } else { (lhs, rhs) };
        if boolean {
            let pred = Pred::decode(f.field(23, 3))?;
            self.regs[f.rd] = u64::from(pred.holds(lhs, rhs) ^ inv);
        } else {
            // The flag-setting CMP has PRED = 0 and rd = 0, and INV only in
            // the short form (CMPRVW.S).
            legal(f.field(23, 3) == 0 && (!inv || short) && f.rd == 0)?;
            self.cmp(lhs, rhs);
        }
        Ok(Flow::Next)
    }

    /// Class D (ISA v3 B) — fused compare-and-branch, flags untouched:
    /// `if (rs1 PRED rhs) ^ INV { PC += 4*simm13 }`.  PRED `[25:23]` (EQ, LT,
    /// LE, ULT, ULE, and the 32-bit EQW, LTW, ULTW), INV `[22]`, IMM `[21]`
    /// (rhs = simm4 `[3:0]` instead of rs2), simm13 word displacement
    /// `[20:8]` from the branch.  The 2-word form (hw-proposals 6) has IMM
    /// set, `[3:0]` = 0 and rhs = sext(imm32) from word 1; not taken, it
    /// falls through to PC + 8.
    fn exec_fused_branch(&mut self, f: Fields) -> Exec {
        let (pred, w) = Pred::decode_fused(f.field(23, 3));
        let lhs = self.regs[f.rs1];
        let rhs = if f.len == 2 {
            legal(f.bit(21) && f.rs2 == 0)?;
            ext(f.imm32, true)
        } else if f.bit(21) {
            i64::from(((f.word << 28) as i32) >> 28) as u64 // sign-extend [3:0]
        } else {
            self.regs[f.rs2]
        };
        // The W predicates compare {x[31:0], 0}: the low halves, signed or
        // unsigned, as CMPRRW (the RTL's dispatch shift).
        let (lhs, rhs) = if w { (lhs << 32, rhs << 32) } else { (lhs, rhs) };
        Ok(if pred.holds(lhs, rhs) ^ f.bit(22) {
            let disp = ((f.word << 11) as i32) >> 19; // sign-extend [20:8]
            Flow::Jump(self.pc.wrapping_add((disp << 2) as u32))
        } else {
            Flow::Next
        })
    }

    /// Class 4 — shift / rotate / bit manipulation.
    fn exec_shift_bit(&mut self, f: Fields) -> Exec {
        let op = f.field(22, 4);
        if f.len == 2 {
            return self.exec_bit_field(f, op);
        }
        let embedded = f.bit(21); // 0 = count in rs2[5:0], 1 = embedded N [20:15]
        let n = f.field(15, 6);
        let set_z = f.bit(14); // F
        // One word with [13:12] = 0 (:458), and SRC = 0 needs N = 0 (:461).
        legal(f.len == 1 && f.field(12, 2) == 0 && (embedded || n == 0))?;
        let rd = f.rd;
        let a = self.regs[f.rs1];
        let cnt = if embedded { n } else { (self.regs[f.rs2] & 0x3F) as u32 };
        // U_ROT1: SRC=1, N=1, F=1 (:469, :478).
        let rot1 = embedded && n == 1 && set_z;
        match op {
            0 => self.write_z(rd, a << cnt, set_z),                   // SHL
            1 => self.write_z(rd, a >> cnt, set_z),                   // SHR
            2 => self.write_z(rd, ((a as i64) >> cnt) as u64, set_z), // SAR
            3 => {
                // ROL.  Only the embedded N=1 form with F (U_ROT1) also sets C
                // to the bit rotated out; a register count of 1 is a plain
                // U_SHIFT that writes Z alone (pipeline_core.sv:469-475, :1048).
                if rot1 {
                    self.flags.set_carry(a >> 63 == 1);
                }
                self.write_z(rd, a.rotate_left(cnt), set_z);
            }
            4 => {
                // ROR (pipeline_core.sv:1049).
                if rot1 {
                    self.flags.set_carry(a & 1 == 1);
                }
                self.write_z(rd, a.rotate_right(cnt), set_z);
            }
            5 => {
                // RCL — rotate left one bit through carry (ROLCR); only the
                // U_ROT1 encoding is legal (:477-482).
                legal(rot1)?;
                let r = (a << 1) | self.carry_in();
                self.flags.set_carry(a >> 63 == 1);
                self.write_z(rd, r, set_z);
            }
            6 => {
                // RCR — rotate right one bit through carry (RORCR).
                legal(rot1)?;
                let r = (a >> 1) | (self.carry_in() << 63);
                self.flags.set_carry(a & 1 == 1);
                self.write_z(rd, r, set_z);
            }
            // BSET / BCLR / BTGL: F writes Z from the result (pipeline_core.sv:483-485).
            8 => self.write_z(rd, a | (1 << cnt), set_z),
            9 => self.write_z(rd, a & !(1 << cnt), set_z),
            10 => self.write_z(rd, a ^ (1 << cnt), set_z),
            11 => {
                // BTST — BTSTRR (reg) writes rd = bit, and Z from it with F
                // (pipeline_core.sv:493-494); BTST-imm is flag-only (Z=~bit).
                let bit = (a >> cnt) & 1;
                if embedded {
                    // BTST-imm: rd = 0 and F = 0 (:489).
                    legal(f.rd == 0 && !set_z)?;
                    self.flags.set_zero(bit == 0);
                } else {
                    self.write_z(rd, bit, set_z);
                }
            }
            _ => return Err(Fault::Illegal),
        }
        Ok(Flow::Next)
    }

    /// Class 4, 2 words — BEXTR / BDEP, the field descriptor in word 1.
    fn exec_bit_field(&mut self, f: Fields, op: u32) -> Exec {
        // [21:12] = 0 (:499-500).
        legal(f.field(12, 10) == 0)?;
        let rd = f.rd;
        let a = self.regs[f.rs1];
        let (start, mask) = bit_field(f.imm32);
        match op {
            12 => {
                // BEXTR (rs2 = 0, :501) — extract the imm-described field of
                // rs1.  The RTL shifts the full 64-bit register (:1083-1088),
                // so a field with start+len > 32 takes bits from rs1[63:32];
                // the result is at most 31 bits, zero-extended.  Z is always
                // set from the result (fmask.zero, :503), although F is clear.
                legal(f.rs2 == 0)?;
                self.write_z(rd, (a >> start) & mask, true);
            }
            13 => {
                // BDEP — deposit rs2's low field into rs1 (the base) at start;
                // the result is the low 32 bits, zero-extended.  No flags.
                let field = (self.regs[f.rs2] & mask) << start;
                let clear = !(mask << start) & 0xFFFF_FFFF;
                self.regs[rd] = (a & clear) | (field & 0xFFFF_FFFF);
            }
            _ => return Err(Fault::Illegal),
        }
        Ok(Flow::Next)
    }

    /// Class 5 — unary: `rd = OP(rs1)`.
    fn exec_unary(&mut self, f: Fields) -> Exec {
        let op = f.field(22, 4);
        // SEXT/ZEXT SIZE [21:20]: 00=8 01=16 10=32; 11 is reserved, and the
        // other ops need SIZE = 0 (:514, :524, :528).
        let size = f.field(20, 2);
        // One word with [18:12] and rs2 zero (:512).
        legal(f.len == 1 && f.field(12, 7) == 0 && f.rs2 == 0)?;
        legal(if op == 4 || op == 5 { size != 3 } else { size == 0 })?;
        let size_bytes = 1_usize << size;
        let set_z = f.bit(19); // F
        let rd = f.rd;
        let a = self.regs[f.rs1];
        match op {
            // F writes Z from the result for every op but ABS / INC / DEC,
            // which require it, and SEXT, which adds S (pipeline_core.sv:516-518).
            0 => self.write_z(rd, a, set_z),                // COPY
            1 => self.write_z(rd, a.wrapping_neg(), set_z), // NEG
            2 => self.write_z(rd, !a, set_z),               // NOT
            3 => {
                // ABS, legal only with F (:520-522) — INT_MIN wraps to itself
                // and sets V, the one input whose absolute value does not fit
                // (:1060-1062).
                legal(set_z)?;
                self.flags.set_overflow(a == 1 << 63);
                self.write_z(rd, (a as i64).wrapping_abs() as u64, true);
            }
            4 => {
                // SEXT
                let r = sign_extend(a, size_bytes);
                if set_z {
                    self.flags.set_zs(r);
                }
                self.regs[rd] = r;
            }
            5 => self.write_z(rd, zero_extend(a, size_bytes), set_z),     // ZEXT
            6 => self.write_z(rd, a.swap_bytes(), set_z),                 // BSWAP (64-bit)
            7 => self.write_z(rd, a.reverse_bits(), set_z),               // BITREV
            8 => self.write_z(rd, u64::from(a.count_ones()), set_z),      // POPCNT
            9 => self.write_z(rd, u64::from(a.leading_zeros()), set_z),   // CLZ; CLZ(0)=64
            10 => self.write_z(rd, u64::from(a.trailing_zeros()), set_z), // CTZ; CTZ(0)=64
            // GETF / SETFR: rd = {zero,equal,carry,overflow} in the top nibble [63:60].
            // `equal` is DERIVED (E = Z) — retired as storage (doc/ISA_ENCODING_V2.md §7.1).
            // With F, Z is then written from that result (pipeline_core.sv:532-535).
            12 => self.write_z(rd, u64::from(self.flags.to_getf()) << 60, set_z),
            // INC / DEC, legal only with F (:537-540).
            14 => {
                legal(set_z)?;
                self.regs[rd] = self.add_flags(a, 1, 0);
            }
            15 => {
                legal(set_z)?;
                self.regs[rd] = self.sub_flags(a, 1, 0);
            }
            _ => return Err(Fault::Illegal),
        }
        Ok(Flow::Next)
    }

    /// Class 6 — loads: `rd = ext(mem[EA])`.
    fn exec_load(&mut self, f: Fields) -> Exec {
        if is_lr_sc(f) {
            return self.exec_lr(f);
        }
        mem_legal(f, false)?;
        // MEMGET32 ([rs1], 32-bit) has no sign-extending form (:559-560).
        legal(!(f.field(24, 2) == 2 && f.field(21, 2) == 0 && f.bit(23)))?;
        let size_bytes = 1_usize << f.field(24, 2); // 00=8 01=16 10=32 11=64
        let ea = self.ea(f);
        // MEMGET32 ([rs1], 32-bit) is unaligned-tolerant; every other 16/32-bit
        // load traps on a misaligned address.
        // The misaligned check comes first, as in the RTL (EX->MEM, before
        // the MEM-stage access check, MMU_PLAN §7.1).
        if !(size_bytes == 4 && f.field(21, 2) == 0) && misaligned(ea, size_bytes) {
            return Err(Fault::Exception(Cause::MisalignedLoad, ea));
        }
        // STATUS.MAL64: a misaligned 64-bit load traps (A = 0 or 1).
        if size_bytes == 8 && self.mal64 && ea & 7 != 0 {
            return Err(Fault::Exception(Cause::MisalignedLoad, ea));
        }
        // EADDR is the raw EA, before the A=1 alignment (MMU_PLAN §4.1).
        let at = self.map_data(ea, if size_bytes == 8 { 1 } else { size_bytes as u32 }, false)?;
        let at = at.masked(align_mask(size_bytes, f.bit(20)));
        let val = if size_bytes == 8 {
            self.read64(at)
        } else {
            self.read_sub(at, size_bytes)
        };
        self.regs[f.rd] = if f.bit(23) { sign_extend(val, size_bytes) } else { val };
        Ok(Flow::Next)
    }

    /// Class 7 — stores: `mem[EA] = reg[rd]` (rd field is the data source).
    fn exec_store(&mut self, f: Fields) -> Exec {
        if is_lr_sc(f) {
            return self.exec_sc(f);
        }
        if is_dcbz_space(f) {
            return self.exec_dcbz(f);
        }
        mem_legal(f, true)?;
        let size_bytes = 1_usize << f.field(24, 2);
        let ea = self.ea(f);
        if misaligned(ea, size_bytes) {
            return Err(Fault::Exception(Cause::MisalignedStore, ea));
        }
        // STATUS.MAL64: a misaligned 64-bit store traps (A = 0 or 1).
        if size_bytes == 8 && self.mal64 && ea & 7 != 0 {
            return Err(Fault::Exception(Cause::MisalignedStore, ea));
        }
        let at = self.map_data(ea, if size_bytes == 8 { 1 } else { size_bytes as u32 }, true)?;
        let at = at.masked(align_mask(size_bytes, f.bit(20)));
        let data = self.regs[f.rd];
        if size_bytes == 8 {
            self.write64(at, data);
        } else {
            self.write_sub(at, data, size_bytes);
        }
        Ok(Flow::Next)
    }

    /// `DCBZ [rs1]` (`HW_PERF_PLAN.md` §1, `0x5F00_2000 | rs1 << 4`): zero
    /// the 32-byte line holding EA = rs1 (aligned down to `CACHE_INFO`'s
    /// line size), checked as a store at EA (EADDR is the raw EA); MMIO is
    /// `ACCESS_STORE` in either mode; never `MISALIGNED`, not even with
    /// MAL64.  Any other encoding with `[13]` set is ILLEGAL.  The trace
    /// shows the line's first doubleword as the write.
    fn exec_dcbz(&mut self, f: Fields) -> Exec {
        legal(f.field(24, 2) == 3 && !f.bit(23) && !f.bit(20) && f.field(14, 6) == 0 && !f.bit(12) && f.field(8, 4) == 0 && f.field(0, 4) == 0)?;
        let ea = self.regs[f.rs1] as u32;
        if is_mmio(ea) {
            return Err(Fault::Exception(Cause::AccessStore, ea));
        }
        let at = self.map_data(ea, 1, true)?;
        let line = at.masked(!(DCBZ_LINE - 1));
        if let Some(sim) = self.cachesim.as_mut() {
            sim.zero_line(line.pa());
        }
        for dw in 0..DCBZ_LINE / 8 {
            self.write64(line.offset(dw * 8), 0);
        }
        self.last_write = Some(super::MemWrite {
            addr: at.va() & !(DCBZ_LINE - 1),
            byte_enable: 0xFF,
            data: 0,
        });
        self.zero_lines += 1;
        Ok(Flow::Next)
    }

    /// MMU stage 1 LR.D / LR.W / LR.W.S (`MMU_PLAN.md` §5.1): `rd =
    /// mem[rs1]` (LR.W zero-extended, LR.W.S sign-extended), and reserve its
    /// doubleword.  `[19:13]`, A `[20]` and rs2 are zero; SIZE 64 with SGN
    /// clear, or SIZE 32.
    fn exec_lr(&mut self, f: Fields) -> Exec {
        let size = f.field(24, 2);
        legal(f.field(13, 7) == 0 && !f.bit(20) && f.rs2 == 0 && (size == 2 || (size == 3 && !f.bit(23))))?;
        let bytes = if size == 3 { 8 } else { 4 };
        let ea = self.regs[f.rs1] as u32;
        atomic_check(ea, bytes, false)?;
        let at = self.map_data(ea, 1, false)?;
        let val = if bytes == 8 { self.read64(at) } else { self.read_sub(at, 4) };
        self.regs[f.rd] = if f.bit(23) { sign_extend(val, 4) } else { val };
        self.reservation = Some(ea >> 3);
        Ok(Flow::Next)
    }

    /// MMU stage 1 SC.D / SC.W (`MMU_PLAN.md` §5.1): if the reservation is
    /// valid and on this doubleword, store rs2 (SC.W its low 32 bits) at
    /// rs1 and set rd = 0; otherwise store nothing and set rd = 1.  Either
    /// way the reservation is cleared; a faulting SC leaves it, and rd, as
    /// they were.  `[19:13]`, A `[20]` and SGN `[23]` are zero; SIZE 64 or
    /// 32.
    fn exec_sc(&mut self, f: Fields) -> Exec {
        let size = f.field(24, 2);
        legal(f.field(13, 7) == 0 && !f.bit(20) && !f.bit(23) && size >= 2)?;
        let bytes = if size == 3 { 8 } else { 4 };
        let ea = self.regs[f.rs1] as u32;
        atomic_check(ea, bytes, true)?;
        // A failing SC stores nothing, but is checked as a store all the
        // same (MMU_PLAN §10.2: SC is a store).
        let at = self.map_data(ea, 1, true)?;
        let data = self.regs[f.rs2];
        let reserved = self.reservation == Some(ea >> 3);
        if reserved {
            if bytes == 8 {
                self.write64(at, data);
            } else {
                self.write_sub(at, data, 4);
            }
        }
        self.regs[f.rd] = u64::from(!reserved);
        self.reservation = None;
        Ok(Flow::Next)
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
        let inv = f.bit(18);
        // The 2-word absolute / PC-relative form with [17:0] = 0, the 1-word
        // register form with [17:4] = 0, or the 1-word short PC-relative
        // form; COND 0 (always) has no inverse (:572-575).
        legal(
            match f.len {
                2 => !rind && f.field(0, 18) == 0,
                1 if rel => !rind,
                1 => rind && f.field(4, 14) == 0,
                _ => false,
            } && !(f.field(19, 4) == 0 && inv),
        )?;
        let cond = self.eval_cond(f.field(19, 4)).ok_or(Illegal)?;
        if !(cond ^ inv) {
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
            let slot = self.map_push()?;
            self.push(slot, u64::from(f.next));
        }
        Ok(Flow::Jump(target))
    }

    /// Evaluate a class-8 COND code against the flags. `None` = reserved/illegal.
    ///
    /// Every relation is DERIVED from the unified `Z/S/C/V` register (`doc/ISA_ENCODING_V2.md` §7.3); the
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
        let op = f.field(22, 4);
        match (f.len, op) {
            (1, 8) => {
                // v3 C: ENTER N — push R15; R15 = SP; SP -= 8*N  (N = [21:0],
                // any value, :587).
                let slot = self.map_push()?;
                self.push(slot, self.regs[15]);
                self.regs[15] = u64::from(self.sp);
                self.sp = self.sp.wrapping_sub(f.field(0, 22) << 3);
            }
            (1, 9) => {
                // v3 C: LEAVE — SP = R15; pop R15 ([21:0] = 0, :591).
                legal(f.field(0, 22) == 0)?;
                let fp = self.regs[15] as u32;
                let at = self.map_data(fp, 1, false)?;
                self.regs[15] = self.read64(at);
                self.sp = fp.wrapping_add(8);
            }
            (1, 10) => {
                // v3 C: LEAVERET — SP = R15; pop R15; pop PC (= LEAVE; RET)
                // ([21:0] = 0, :595).
                legal(f.field(0, 22) == 0)?;
                let fp = self.regs[15] as u32;
                // Both loads are checked and translated before either
                // happens (MMU_PLAN §4.1, §10.2, §11.3).  EADDR is R15 when
                // the first faults; when only the second does, the start of
                // its doubleword.
                let first = self.map_data(fp, 1, false)?;
                let second = self.map_data(fp.wrapping_add(8) & !7, 1, false)?;
                self.regs[15] = self.read64(first);
                let ra = self.read64(second);
                self.sp = fp.wrapping_add(16);
                return Ok(Flow::Jump(ra as u32));
            }
            _ => return self.exec_stack_v2(f, op),
        }
        Ok(Flow::Next)
    }

    /// Class 9, the v2 forms: PUSH / POP / SP access / RET / IRET.  Each
    /// needs `[21:12]` = 0 and its unused register fields zero (:602-643).
    fn exec_stack_v2(&mut self, f: Fields, op: u32) -> Exec {
        legal(f.field(12, 10) == 0)?;
        // The register fields each form may use: (rd, rs1); rs2 is never used.
        let (rd, rs1) = match (f.len, op) {
            (1, 0 | 4) => (false, true),                        // PUSH, SETSP
            (1, 2 | 3) => (true, false),                        // POP, GETSP
            (1, 6 | 7) | (2, 1 | 5) | (3, 1) => (false, false), // RET, IRET, PUSHV, ADDSP, PUSHV64
            _ => return Err(Fault::Illegal),
        };
        legal(f.rs2 == 0 && (rd || f.rd == 0) && (rs1 || f.rs1 == 0))?;
        // MMU: IRET is privileged, checked first; then the stack access is
        // checked and translated (pushes are stores, pops loads).
        if op == 7 && self.um {
            return Err(privileged(f));
        }
        match op {
            0 | 1 => {
                let slot = self.map_push()?;
                // PUSH rs1, or PUSHI: PUSHV (2-word, zero-ext imm32) or
                // PUSHV64 (3-word, imm64).
                let value = match (op, f.len) {
                    (0, _) => self.regs[f.rs1],
                    (_, 3) => f.imm64(),
                    _ => u64::from(f.imm32),
                };
                self.push(slot, value);
            }
            2 => {
                // POP rd
                let slot = self.map_pop()?;
                self.regs[f.rd] = self.pop(slot);
            }
            3 => self.regs[f.rd] = u64::from(self.sp),    // GETSP
            4 => self.sp = self.regs[f.rs1] as u32,       // SETSP
            5 => self.sp = self.sp.wrapping_add(f.imm32), // ADDSP (imm sign-ext, 32-bit add)
            // RET — restore PC from [31:0] of the popped slot.
            6 => {
                let slot = self.map_pop()?;
                return Ok(Flow::Jump(self.pop(slot) as u32));
            }
            _ => {
                let slot = self.map_pop()?;
                return Ok(self.iret(slot));
            }
        }
        Ok(Flow::Next)
    }

    /// IRET — pop the saved interrupt context and restore PC`[31:0]`, flags`[38:32]`,
    /// `INT_MASK` `[42:39]` and the `DI` bit `[43]` (1 = disabled).
    ///
    /// The 7-bit saved word is `{Z,E,C,V,S,L,U}` (bit38→Z … bit32→U), but only the
    /// unified `Z/S/C/V` are consumed (`doc/ISA_ENCODING_V2.md` §7.4); the derived `E`(bit37)/`L`(bit33)/
    /// `U`(bit32) bits are ignored and regenerate from `Z/S/C/V` on read.
    ///
    /// MMU stage 1 (`MMU_PLAN.md` §3.4): `UM` = PM `[44]`, and with PM set
    /// the stacks swap after the pop, so the SP is the user SP and `SP_ALT`
    /// the kernel SP just above the popped frame.  The LR/SC reservation is
    /// cleared.
    fn iret(&mut self, slot: Mapped) -> Flow {
        let ctx = self.pop(slot);
        self.flags = Flags::from_ctx((ctx >> CTX_FLAGS_SHIFT) as u8 & 0x7F);
        self.int_mask = ((ctx >> CTX_INT_MASK_SHIFT) & 0xF) as u8;
        self.irq_disabled = (ctx >> CTX_IRQ_DISABLED_SHIFT) & 1 != 0;
        self.reservation = None;
        if (ctx >> CTX_PREV_MODE_SHIFT) & 1 != 0 {
            std::mem::swap(&mut self.sp, &mut self.sp_alt);
            self.um = true;
        }
        Flow::Jump(ctx as u32)
    }

    /// Class A — mul / div / mod.
    ///
    /// Flags follow `pipeline_core.sv`: MUL writes Z, S and V (:655-658,
    /// :1157-1163), DIV and MOD write Z and V (:659-666, :1164-1176), with Z
    /// from the value written to rd (:1549-1550).
    fn exec_muldiv(&mut self, f: Fields) -> Exec {
        let sgn = f.bit(23);
        let high = f.bit(22); // high half (MUL only)
        let kind = f.field(24, 2);
        if f.len == 1 && kind == 3 {
            // hw-proposals 9 MULV.S / MULUV.S: rd = rs1 * ext(imm8 [19:12]),
            // extended and multiplied signed or unsigned by SGN [23] as the
            // 2-word MULV / MULUV; the low half, Z/S/V as MUL.  [22:20] and
            // rs2 are zero.
            legal(f.field(20, 3) == 0 && f.rs2 == 0)?;
            self.mul_low(f.rd, self.regs[f.rs1], ext(f.imm32, sgn), sgn, false);
            return Ok(Flow::Next);
        }
        // [21:20] and [18:12] zero, W only on the low-half MUL, and rs2 = 0
        // in the 2-word form (:650-652); HIGH only on MUL (:659, :663).
        legal(
            f.field(20, 2) == 0
                && f.field(12, 7) == 0
                && (!f.bit(19) || (kind == 0 && !high))
                && (f.len == 1 || (f.len == 2 && f.rs2 == 0))
                && (kind == 0 || !high),
        )?;
        let is_imm = f.len == 2;
        let rd = f.rd;
        let a = self.regs[f.rs1];
        let b = if is_imm { ext(f.imm32, sgn) } else { self.regs[f.rs2] };
        match kind {
            0 if high => {
                // MULH: the high half of the 128-bit product, signed or
                // unsigned per SGN; S is its bit 63, V cleared (:1159-1162).
                let r = if sgn {
                    ((i128::from(a as i64) * i128::from(b as i64)) >> 64) as u64
                } else {
                    ((u128::from(a) * u128::from(b)) >> 64) as u64
                };
                self.flags.set_sign(r >> 63 == 1);
                self.flags.set_overflow(false);
                self.regs[rd] = self.with_z(r);
            }
            // v3 D3 MULW: W [19] on the low-half MUL (:649, :1333-1334).
            0 => self.mul_low(rd, a, b, sgn, f.bit(19)),
            kind @ (1 | 2) => {
                let is_mod = kind == 2;
                if b == 0 {
                    // Divide by zero does not trap: DIV gives all ones, MOD (every
                    // form, MODV too) the dividend; only V is written, set
                    // (:1165-1169).
                    self.regs[rd] = if is_mod { a } else { u64::MAX };
                    self.flags.set_overflow(true);
                } else {
                    // Signed division truncates toward zero and INT_MIN / -1
                    // gives INT_MIN (INT_MIN % -1 = 0), as the RTL's
                    // magnitude divider does (:923-924, :1170-1175).
                    let r = match (is_mod, sgn) {
                        (false, true) => (a as i64).wrapping_div(b as i64) as u64,
                        (false, false) => a / b,
                        (true, true) => (a as i64).wrapping_rem(b as i64) as u64,
                        (true, false) => a % b,
                    };
                    self.flags.set_overflow(false);
                    self.regs[rd] = self.with_z(r);
                }
            }
            _ => return Err(Fault::Illegal),
        }
        Ok(Flow::Next)
    }

    /// MUL's low half: the 128-bit product, signed or unsigned per `sgn`
    /// (:892-899).  S is bit 63 of the low half, before any W
    /// sign-extension; V is set when the low half does not hold the whole
    /// product.  With `w` (MULW) the result is the low 32 bits sign-extended,
    /// and Z follows it.
    fn mul_low(&mut self, rd: usize, a: u64, b: u64, sgn: bool, w: bool) {
        let (r, overflow) = if sgn {
            let (lo, v) = (a as i64).overflowing_mul(b as i64);
            (lo as u64, v)
        } else {
            a.overflowing_mul(b)
        };
        self.flags.set_sign(r >> 63 == 1);
        self.flags.set_overflow(overflow);
        let r = if w { sign_extend(r, 4) } else { r };
        self.regs[rd] = self.with_z(r);
    }

    /// Class B — system: NOP / HALT / WAIT / RESET / TRAP / DELAY, DI / EI,
    /// MMU stage 1's SYSCALL and MFSR / MTSR, and stage 3's TLBWI / TLBR /
    /// TLBP / TLBINV.  In user mode everything but NOP, TRAP and SYSCALL
    /// (and the user's SRs) is PRIVILEGED.
    fn exec_system(&mut self, f: Fields) -> Exec {
        if f.field(22, 4) == 1 {
            return self.exec_sr(f);
        }
        if f.len == 1 && f.field(17, 9) == 3 {
            // hw-proposals 7a, OP [21:16] 6 / 7 with [25:22], [15:12] and rs2
            // zero: DI rd (rs1 = 0) sets rd = the old enable bit and
            // disables; EI rs1 (rd = 0) sets enable = rs1[0].
            // The decode checks come first: an illegal word is ILLEGAL in
            // either mode, a legal one PRIVILEGED in user mode.
            let ei = f.bit(16);
            legal(f.field(12, 4) == 0 && f.rs2 == 0 && if ei { f.rd == 0 } else { f.rs1 == 0 })?;
            if self.um {
                return Err(privileged(f));
            }
            if ei {
                self.irq_disabled = self.regs[f.rs1] & 1 == 0;
            } else {
                self.regs[f.rd] = u64::from(!self.irq_disabled);
                self.irq_disabled = true;
            }
            return Ok(Flow::Next);
        }
        // [25:22], [15:8] and rs2 zero (:672).
        legal(f.field(22, 4) == 0 && f.field(8, 8) == 0 && f.rs2 == 0)?;
        let op = f.field(16, 6);
        // rs1 is used only by DELAYR (:675-684).
        legal(f.rs1 == 0 || (f.len == 1 && op == 5))?;
        // MMU stage 1 §3.5: HALT, WAIT, RESET and DELAY are privileged, and
        // so are stage 3's TLB instructions (§11.3).
        if self.um && matches!((f.len, op), (1, 1..=3 | 9..=12) | (1 | 2, 5)) {
            return Err(privileged(f));
        }
        Ok(match (f.len, op) {
            (1, 0) => Flow::Next, // NOP
            (1 | 2, 5) => {
                // DELAYR / DELAYV busy-wait in EX for {value, 13'b0} cycles,
                // the value being rs1[31:0] or imm32 (pipeline_core.sv:1619-1626);
                // modelled time (timer, millisecond clock, cycle counter)
                // advances by as much.
                let value = if f.len == 1 { self.regs[f.rs1] as u32 } else { f.imm32 };
                self.tick(u64::from(value) << DELAY_SHIFT);
                Flow::Next
            }
            (1, 1) => Flow::Stop(StopReason::Halt),
            (1, 2) => {
                // WAIT: sleep until an interrupt source is ready; the IRQ
                // pushes the following instruction as the resume address.
                self.waiting = true;
                Flow::Next
            }
            (1, 3) => Flow::Jump(0x4), // RESET → PC = 0x4
            // TRAP and SYSCALL raise their exception in either mode; with
            // EVEC = 0 a TRAP stops the CPU as before stage 1.
            (1, 4) => return Err(Fault::Exception(Cause::Trap, f.word)),
            (1, 8) => return Err(Fault::Exception(Cause::Syscall, 0)),
            // MMU stage 3 (§11.3).  They serialize on the board; the
            // emulator, one instruction at a time, needs nothing for that.
            (1, 9) => {
                self.tlbwi();
                Flow::Next
            }
            (1, 10) => {
                self.tlbr();
                Flow::Next
            }
            (1, 11) => {
                self.tlbp();
                Flow::Next
            }
            (1, 12) => {
                self.tlbinv();
                Flow::Next
            }
            _ => return Err(Fault::Illegal),
        })
    }

    /// MMU stage 1 `MFSR rd, sr` (`[21]` = 0) and `MTSR sr, rs1` (`[21]` =
    /// 1), `[25:22]` = 1, sr in `[19:12]` (`MMU_PLAN.md` §5, §6.1).  One
    /// word with `[20]` zero; MFSR has `[7:0]` zero, MTSR rd and rs2.  An
    /// SR that does not exist, and MTSR to `CYCLE`, are ILLEGAL; in user
    /// mode only `STATUS`, `TP` and `CYCLE` may be read and `TP` written,
    /// and the rest are PRIVILEGED.  MTSR serializes on the board, which
    /// the emulator, one instruction at a time, needs nothing for.
    fn exec_sr(&mut self, f: Fields) -> Exec {
        let n = f.field(12, 8);
        if f.bit(21) {
            legal(f.len == 1 && !f.bit(20) && f.rd == 0 && f.rs2 == 0)?;
            // Legality before privilege (MMU_PLAN §3.5): MTSR to CYCLE, or to
            // an SR that does not exist, is ILLEGAL in user mode too.
            legal(self.sr_read(n).is_some() && n != super::system::sr::CYCLE)?;
            if self.um && !sr_user_writable(n) {
                return Err(privileged(f));
            }
            let value = self.regs[f.rs1];
            let written = self.sr_write(n, value);
            debug_assert!(written, "SR {n} exists and is writable");
        } else {
            legal(f.len == 1 && !f.bit(20) && f.field(0, 8) == 0)?;
            let value = self.sr_read(n).ok_or(Illegal)?;
            if self.um && !sr_user_readable(n) {
                return Err(privileged(f));
            }
            self.regs[f.rd] = value;
        }
        Ok(Flow::Next)
    }

    /// Class C — the SPI LCD port: `LCDCMDR`/`LCDDATAR` (1 word, rs1),
    /// `LCDCMDV`/`LCDDATAV`/`LCDRST` (2 words, imm32).  There is no LCD model, so
    /// a legal word does nothing; the decode checks are the RTL's (:698-711).
    /// All are privileged in user mode (MMU stage 1 §3.5).
    fn exec_lcd(&self, f: Fields) -> Exec {
        let op = f.field(24, 2);
        legal(
            f.field(12, 12) == 0
                && f.rd == 0
                && f.rs2 == 0
                && match f.len {
                    1 => op <= 1,
                    2 => f.rs1 == 0 && op <= 2,
                    _ => false,
                },
        )?;
        if self.um {
            return Err(privileged(f));
        }
        Ok(Flow::Next)
    }
}

/// Whether `f` is an MMU stage 1 LR or SC: class 6 / 7, one word, MODE 00
/// and `[12]` set, which no other load or store has (`MMU_PLAN.md` §5.1).
const fn is_lr_sc(f: Fields) -> bool {
    f.len == 1 && f.field(21, 2) == 0 && f.bit(12)
}

/// Whether `f` is in the DCBZ space: class 7, one word, MODE 00, `[13]`
/// set and not LR/SC (`HW_PERF_PLAN.md` §1).
const fn is_dcbz_space(f: Fields) -> bool {
    f.len == 1 && f.field(21, 2) == 0 && f.bit(13) && !f.bit(12)
}

/// The D-cache line DCBZ zeroes (`CACHE_INFO[31:24]`).
const DCBZ_LINE: u32 = 32;

/// The LR/SC address checks (`MMU_PLAN.md` §5.1), in either mode: an
/// address not naturally aligned (4 or 8, 64-bit too) is
/// `MISALIGNED_LOAD/STORE`, and then one in MMIO is `ACCESS_LOAD/STORE`.
const fn atomic_check(ea: u32, bytes: u32, store: bool) -> Result<(), Fault> {
    if ea & (bytes - 1) != 0 {
        return Err(Fault::Exception(if store { Cause::MisalignedStore } else { Cause::MisalignedLoad }, ea));
    }
    if is_mmio(ea) {
        return Err(Fault::Exception(if store { Cause::AccessStore } else { Cause::AccessLoad }, ea));
    }
    Ok(())
}

/// The load / store decode checks shared by classes 6 and 7 (:547-554): the
/// short form (1 word, MODE = 01) keeps its offset in `[19:12]`, which is
/// otherwise zero; a store has no SGN; A only on a 64-bit access; MODE
/// 00/01/11 as 1 word and 01/10 as 2 words; and the register fields a MODE
/// does not use are zero.
fn mem_legal(f: Fields, store: bool) -> Result<(), Illegal> {
    let mode = f.field(21, 2);
    legal(
        (f.field(12, 8) == 0 || (f.len == 1 && mode == 1))
            && !(store && f.bit(23))
            && !(f.bit(20) && f.field(24, 2) != 3)
            && match f.len {
                1 => mode != 2,
                2 => mode == 1 || mode == 2,
                _ => false,
            }
            && match mode {
                0 | 1 => f.rs2 == 0,
                2 => f.field(0, 8) == 0,
                _ => true,
            },
    )
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
/// - hw-proposals 9 `LEAPC.S`, class 2 `OP=14` `[21]=1`: simm16
///   `{[19:12], [7:0]}`; `MULV.S`/`MULUV.S`, class A `OP=3`: imm8 `[19:12]`,
///   `SGN` `[23]` picks sign/zero extension.  (`CMPRVW.S` is A3's.)
const fn short_imm(word: u32) -> Option<u32> {
    if word >> 30 != 1 {
        return None;
    }
    let i8v = (word >> 12) & 0xFF;
    let s8 = ((i8v as u8) as i8) as i32 as u32;
    match (word >> 26) & 0xF {
        0x2 if (word >> 21) & 0x1F == 0b11101 => Some(((((word >> 4) & 0xFF00) | (word & 0xFF)) as u16 as i16) as i32 as u32),
        0x2 => Some(if (word >> 20) & 1 == 1 { s8 } else { i8v }),
        0xA if (word >> 24) & 3 == 3 => Some(if (word >> 23) & 1 == 1 { s8 } else { i8v }),
        0x3 if (word >> 20) & 1 == 1 && (word >> 21) & 1 == 0 => Some(s8),
        0x6 | 0x7 if (word >> 21) & 0x3 == 1 => Some(s8 << ((word >> 24) & 0x3)),
        0x8 if (word >> 23) & 1 == 0 => {
            let d = ((word << 14) as i32) >> 14; // sign-extend [17:0]
            Some((d << 2) as u32)
        }
        _ => None,
    }
}

/// Whether a `size_bytes` access at `ea` traps as misaligned: 16-bit at an
/// odd address, 32-bit not 4-byte aligned (hw-proposals 8, `pipeline_core.sv`
/// `ex_misal`).  Byte and 64-bit accesses never do; the caller exempts
/// `MEMGET32`.
const fn misaligned(ea: u32, size_bytes: usize) -> bool {
    match size_bytes {
        2 => ea & 1 != 0,
        4 => ea & 3 != 0,
        _ => false,
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
/// 0..=31 (BEXTR may read past bit 31 of rs1; BDEP stays within the low
/// word).  `len` 0 is an empty field: BEXTR yields 0 (and
/// sets Z), BDEP deposits nothing and just zero-extends the base's low 32 bits.
const fn bit_field(desc: u32) -> (u64, u64) {
    let start = (desc & 0x1F) as u64;
    let len = (desc >> 8) & 0x1F;
    (start, (1 << len) - 1)
}

/// Address-alignment mask for a class-6 load or class-7 store of
/// `size_bytes` with the `A` (force-align) bit, as the RTL forms the issue
/// address (`pipeline_core.sv` :1097-1116, :1180-1187): a 64-bit access
/// aligns to 8 only with `A=1`, and with `A=0` RAM itself serves the aligned
/// doubleword (see [`Cpu::read64`]).  Every other access is used as
/// computed: a byte or `MEMGET32` access takes any address, and a 16/32-bit
/// one reaches here only when aligned (a misaligned one has already stopped
/// with [`StopReason::Misaligned`]).
const fn align_mask(size_bytes: usize, force_align: bool) -> u32 {
    if size_bytes == 8 && force_align { !7 } else { !0 }
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
