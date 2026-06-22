//! Independent ISA emulator (golden-model trace generator) for the KlaussCPU.
//!
//! This is Phase-0 of the FPGA pipelining effort: an architectural reference
//! model that executes a flat DDR image and emits a per-retired-instruction
//! trace.  Semantics follow `EMULATOR_ISA_SEMANTICS.md` (RTL-verified corner
//! cases, which OVERRIDE `CPU_ARCHITECTURE.md`) and the per-opcode encodings in
//! the `--opcode` `opcode_select.vh` table.
//!
//! The model is intentionally cycle-agnostic: each instruction commits
//! atomically.  Cache, IFB, timing and the DDR multi-cycle pipeline are not
//! modelled (they have no architectural effect).  Interrupts / timer MMIO /
//! WAIT are stubbed (documented in the summary); the validation corpus does
//! not exercise them.
//!
//! Decoding is done directly from the 32-bit instruction word's opcode bit
//! fields (CPU_ARCHITECTURE.md §15), independent of the assembler's opcode
//! table — so the emulator is a genuine second implementation, not a re-run of
//! the assembler.

use std::fmt::Write as _;

/// Heap-header byte size (4 doublewords) — code starts here (0x20).
#[allow(dead_code, reason = "used by default_entry() / tests; documents the code base")]
const CODE_BASE: u32 = 32;
/// Default instruction-count cap to guard against runaway / infinite loops.
pub const DEFAULT_MAX_INSTRUCTIONS: u64 = 50_000_000;
/// Size of the modelled address space (128 MiB DDR2).
const MEM_SIZE: usize = 128 * 1024 * 1024;
/// Initial stack pointer — top of DDR2, grows down. The loader sets SP near the
/// top of memory; we use a generous value below the 128 MiB ceiling so PUSH
/// never wraps. Matches the board's full-descending stack convention.
const STACK_TOP: u32 = 0x0800_0000;

/// Reason the emulator stopped executing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// HALT instruction reached.
    Halt,
    /// Instruction-count cap hit (possible infinite loop).
    InstructionCap,
    /// TRAP instruction (software abort).
    Trap,
    /// Invalid / unimplemented opcode encountered.
    InvalidOpcode(u32),
    /// PC left the modelled address space.
    PcOutOfRange(u32),
}

/// Result of an emulation run.
pub struct EmulateResult {
    /// Captured UART output (TXR / TX* opcodes), exactly as the board would emit
    /// for the validation harness: low-32-bit value as 8 uppercase hex chars,
    /// one per line. See `tx_reg` for the format rationale.
    pub uart: String,
    /// Number of instructions retired.
    pub instructions: u64,
    /// Why execution stopped.
    pub stop: StopReason,
}

/// The architectural machine state.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is a distinct hardware condition flag (zero/sign/carry/overflow/equal/less/ult)"
)]
pub struct Cpu {
    /// General-purpose registers R0..R15 (64-bit).
    regs: [u64; 16],
    /// Stack pointer (32-bit hardware register, separate from R0..R15).
    sp: u32,
    /// Program counter (32-bit byte address).
    pc: u32,
    // Seven condition flags (sticky — only written by the documented producers).
    /// Zero flag.
    zero: bool,
    /// Sign flag (MSB of an arithmetic result).
    sign: bool,
    /// Carry / borrow out of bit 63.
    carry: bool,
    /// Signed overflow.
    overflow: bool,
    /// Equal flag (set only by CMPRR / CMPRV).
    equal: bool,
    /// Signed less-than flag (set only by CMPRR / CMPRV).
    less: bool,
    /// Unsigned less-than flag (set only by CMPRR / CMPRV).
    ult: bool,
    /// Flat little-endian memory image (128 MiB).
    mem: Vec<u8>,
    /// Captured UART output.
    uart: String,
    /// True once a HALT (or other terminator) is reached.
    halted: bool,
    /// Set when an unrecoverable stop condition occurs.
    stop: Option<StopReason>,
    /// Pending memory write for trace annotation (addr, `byte_enable`, data).
    last_write: Option<(u32, u8, u64)>,
}

/// Decoded operand fields of an instruction word.
struct Fields {
    /// Bits [11:8] — rd for the RRR ALU format.
    rd: usize,
    /// Bits [7:4] — rs1 / first operand.
    rs1: usize,
    /// Bits [3:0] — rs2 / second operand (and destination for many R/RV forms).
    rs2: usize,
}

impl Cpu {
    /// Build a CPU with a flat DDR image already laid out (heap header + code).
    ///
    /// `image` is the `build_ddr_image` output (header at 0x0, code at 0x20).
    /// `entry` is the byte address of the first instruction to execute.
    #[must_use]
    pub fn new(image: &[u8], entry: u32) -> Self {
        let mut mem = vec![0_u8; MEM_SIZE];
        let n = image.len().min(MEM_SIZE);
        mem[..n].copy_from_slice(&image[..n]);
        Self {
            regs: [0; 16],
            sp: STACK_TOP,
            pc: entry,
            zero: false,
            sign: false,
            carry: false,
            overflow: false,
            equal: false,
            less: false,
            ult: false,
            mem,
            uart: String::new(),
            halted: false,
            stop: None,
            last_write: None,
        }
    }

    // ---- memory helpers (little-endian) --------------------------------------

    /// Read a 32-bit little-endian word (used for instruction fetch and imm).
    fn read32(&self, addr: u32) -> u32 {
        let a = addr as usize;
        if a + 4 > self.mem.len() {
            return 0;
        }
        u32::from_le_bytes([self.mem[a], self.mem[a + 1], self.mem[a + 2], self.mem[a + 3]])
    }

    /// Read a 64-bit little-endian doubleword.
    fn read64(&self, addr: u32) -> u64 {
        let a = addr as usize;
        if a + 8 > self.mem.len() {
            return 0;
        }
        let mut b = [0_u8; 8];
        b.copy_from_slice(&self.mem[a..a + 8]);
        u64::from_le_bytes(b)
    }

    /// Read `n` bytes (1/2/4) zero-extended into a u64, little-endian.
    fn read_sub(&self, addr: u32, n: usize) -> u64 {
        let a = addr as usize;
        let mut v: u64 = 0;
        for i in 0..n {
            if a + i < self.mem.len() {
                v |= u64::from(self.mem[a + i]) << (8 * i);
            }
        }
        v
    }

    /// Write a 64-bit doubleword, record the write for the trace.
    fn write64(&mut self, addr: u32, val: u64) {
        let a = addr as usize;
        if a + 8 <= self.mem.len() {
            self.mem[a..a + 8].copy_from_slice(&val.to_le_bytes());
        }
        self.last_write = Some((addr, 0xFF, val));
    }

    /// Write the low `n` bytes (1/2/4) at `addr`, little-endian; record write.
    fn write_sub(&mut self, addr: u32, val: u64, n: usize) {
        let a = addr as usize;
        for i in 0..n {
            if a + i < self.mem.len() {
                self.mem[a + i] = (val >> (8 * i)) as u8;
            }
        }
        // Byte-enable mask placed at the byte-lane within the doubleword, matching
        // the RTL's byte-enable semantics for the trace `be` field.
        let lane = (addr & 7) as usize;
        let be_bits: u8 = ((1_u16 << n) - 1).rotate_left(lane as u32) as u8;
        self.last_write = Some((addr & !7, be_bits, self.read64(addr & !7)));
    }

    // ---- flag helpers --------------------------------------------------------

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

    // ---- UART output ---------------------------------------------------------

    /// Emit a register value over UART, byte-faithful to the RTL.
    ///
    /// The RTL `t_tx_reg` (`uart_tasks.vh:501`) transmits the FULL 64-bit value as
    /// 16 hex chars, most-significant nibble first, with NO trailing newline
    /// (NEWLINE is a separate opcode, `t_tx_newline`, emitting "\n\r"). We match
    /// that exactly so the emulator's UART byte stream cross-checks against the
    /// RTL self-trace. (The old klatest expected-values are stale low-32/8-hex
    /// and are superseded by the trace-based golden cross-check.)
    fn tx_reg(&mut self, val: u64) {
        let _ = write!(self.uart, "{val:016X}");
    }

    /// Emit a raw byte to the UART capture (TXCHARMEMR / TXSTRMEM*).
    fn tx_byte(&mut self, b: u8) {
        self.uart.push(b as char);
    }

    // ---- main execute loop ---------------------------------------------------

    /// Run until HALT / TRAP / cap / fault. Returns the result + trace (if any).
    ///
    /// When `trace` is `Some`, one line per retired instruction is appended in
    /// the `EMULATOR_ISA_SEMANTICS.md` "Trace format" layout.
    pub fn run(&mut self, max_instructions: u64, mut trace: Option<&mut String>) -> EmulateResult {
        let mut count: u64 = 0;
        while !self.halted && count < max_instructions {
            if self.stop.is_some() {
                break;
            }
            let pc = self.pc;
            if (pc as usize) + 4 > self.mem.len() {
                self.stop = Some(StopReason::PcOutOfRange(pc));
                break;
            }
            let word = self.read32(pc);
            self.last_write = None;
            self.step(word);
            count += 1;
            if let Some(t) = trace.as_deref_mut() {
                self.trace_line(t, count, pc, word);
            }
        }
        let stop = self
            .stop
            .clone()
            .unwrap_or(if self.halted { StopReason::Halt } else { StopReason::InstructionCap });
        EmulateResult {
            uart: std::mem::take(&mut self.uart),
            instructions: count,
            stop,
        }
    }

    /// Append one trace line for the just-retired instruction.
    fn trace_line(&self, out: &mut String, i: u64, pc: u32, word: u32) {
        let _ = write!(out, "i={i} pc={pc:08x} op={word:08x}");
        for (idx, r) in self.regs.iter().enumerate() {
            let _ = write!(out, " r{idx}={r:016x}");
        }
        let f = |b: bool| if b { '1' } else { '0' };
        let _ = write!(
            out,
            " sp={:08x} f={}{}{}{}{}{}{}",
            self.sp,
            f(self.zero),
            f(self.sign),
            f(self.carry),
            f(self.overflow),
            f(self.equal),
            f(self.less),
            f(self.ult),
        );
        if let Some((addr, be, data)) = self.last_write {
            let _ = write!(out, " wr={addr:08x}/{be:02x}/{data:016x}");
        }
        out.push('\n');
    }

    /// Decode + execute a single instruction word, advancing PC.
    fn step(&mut self, word: u32) {
        let fields = Fields {
            rd: ((word >> 8) & 0xF) as usize,
            rs1: ((word >> 4) & 0xF) as usize,
            rs2: (word & 0xF) as usize,
        };
        let imm = || self.read32(self.pc + 4);

        // ---- 3-register ALU format (upper 16 bits non-zero) ------------------
        let op_hi = word >> 16;
        if op_hi != 0 {
            self.exec_rrr(op_hi, &fields);
            self.pc = self.pc.wrapping_add(4);
            return;
        }

        // ---- legacy / non-ALU format (upper 16 bits == 0x0000) ---------------
        // [15:8] = opcode class, [11:8]=secondary nibble for some.
        let op = (word >> 8) & 0xFF; // [15:8]
        let op12 = (word >> 4) & 0xFFF; // [15:4], used for the 08?/09?/0A?/0F?... groups
        let full = word & 0xFFFF;

        match op {
            0x01 => {
                // COPY RR: reg[rs1] = reg[rs2]  (rs1=[7:4] dest, rs2=[3:0] src)
                self.regs[fields.rs1] = self.regs[fields.rs2];
                self.pc = self.pc.wrapping_add(4);
            }
            0x02 => {
                // ADDI RRV: rd=[7:4] = reg[rs2] + sign_ext(imm32); sets ADD flags.
                let a = self.regs[fields.rs2];
                let b = i64::from(imm() as i32) as u64;
                self.regs[fields.rs1] = self.add_flags(a, b, 0);
                self.pc = self.pc.wrapping_add(8);
            }
            0x05 => {
                // CMPRR RR: flags from rs1 - rs2; equal/less/ult/sign. No writeback, no zero.
                self.cmp(self.regs[fields.rs1], self.regs[fields.rs2]);
                self.pc = self.pc.wrapping_add(4);
            }
            0x10..=0x1C => self.exec_flow(full, &fields),
            0x20..=0x21 => self.exec_lcd(full, &fields),
            0x30 => self.exec_io(full, &fields),
            0x40 => self.exec_stack(full, &fields),
            0x50 => self.exec_uart(full, &fields),
            0x60 => self.exec_interrupt(full, &fields),
            0x70..=0x7B => self.exec_mem(op, full, &fields),
            0xC0..=0xC7 => self.exec_indexed_sub(op, &fields, imm()),
            0xFC..=0xFD => self.exec_indexed64a(op, &fields, imm()),
            0xF0..=0xF1 => {
                // misc Fxxx (DELAY/NOP/HALT/RESET/TRAP) live in 0xF00.. range
                self.exec_misc(full, &fields, imm());
            }
            0x08 | 0x09 | 0x0A | 0x0B | 0x0F => self.exec_rv_group(op12, full, &fields, imm()),
            0x0C..=0x0E => self.exec_indexed64(op, &fields, imm()),
            _ => {
                self.stop = Some(StopReason::InvalidOpcode(word));
            }
        }
    }

    /// CMPRR / CMPRV flag computation: equal/less/ult/sign from a - b.
    fn cmp(&mut self, a: u64, b: u64) {
        self.equal = a == b;
        self.less = (a as i64) < (b as i64);
        self.ult = a < b;
        let res = a.wrapping_sub(b);
        self.sign = (res >> 63) & 1 == 1;
    }

    /// 3-register ALU operations (upper 16 bits = op code).
    fn exec_rrr(&mut self, op_hi: u32, f: &Fields) {
        let a = self.regs[f.rs1];
        let b = self.regs[f.rs2];
        let sh = (b & 0x3F) as u32; // shift/rotate counts masked to 6 bits
        let res: Option<u64> = match op_hi {
            0x0001 => Some(self.add_flags(a, b, 0)), // ADDR
            0x0002 => Some(self.sub_flags(a, b, 0)), // SUBR
            0x0003 => {
                let r = a & b;
                self.zero = r == 0;
                Some(r)
            } // ANDR sets zero
            0x0004 => {
                let r = a | b;
                self.zero = r == 0;
                Some(r)
            } // ORR
            0x0005 => {
                let r = a ^ b;
                self.zero = r == 0;
                Some(r)
            } // XORR
            0x0006 => Some(self.add_flags(a, b, u64::from(self.carry))), // ADDC
            0x0007 => Some(self.sub_flags(a, b, u64::from(self.carry))), // SUBC
            0x0010 => Some((a as i64).wrapping_mul(b as i64) as u64), // MULR
            0x0011 => Some(a.wrapping_mul(b)),       // MULUR
            0x0012 => Some(((i128::from(a as i64) * i128::from(b as i64)) >> 64) as u64), // MULHR
            0x0013 => Some(((u128::from(a) * u128::from(b)) >> 64) as u64), // MULHUR
            0x0014 => Some(if b == 0 {
                0xFFFF_FFFF_FFFF_FFFF
            } else {
                (a as i64).wrapping_div(b as i64) as u64
            }), // DIVR
            // div-by-zero is non-trapping with an all-ones result (board semantics), not checked_div
            #[allow(clippy::manual_checked_ops, reason = "div-by-zero returns all-ones, not None")]
            0x0015 => Some(if b == 0 { 0xFFFF_FFFF_FFFF_FFFF } else { a / b }), // DIVUR
            0x0016 => Some(if b == 0 { a } else { (a as i64).wrapping_rem(b as i64) as u64 }), // MODR
            0x0017 => Some(if b == 0 { a } else { a % b }),                                    // MODUR
            0x0020 => Some(a << sh),                                                           // SHLR (zero set below)
            0x0021 => Some(a >> sh),                                                           // SHRR
            0x0022 => Some(((a as i64) >> sh) as u64),                                         // SARR
            0x0023 => Some(a.rotate_left(sh)),                                                 // ROLR
            0x0024 => Some(a.rotate_right(sh)),                                                // RORR
            0x0030 => Some(u64::from(a == b)),                                                 // CMPEQR (no flags)
            0x0031 => Some(u64::from(a != b)),
            0x0032 => Some(u64::from((a as i64) < (b as i64))),
            0x0033 => Some(u64::from((a as i64) <= (b as i64))),
            0x0034 => Some(u64::from((a as i64) > (b as i64))),
            0x0035 => Some(u64::from((a as i64) >= (b as i64))),
            0x0036 => Some(u64::from(a < b)),
            0x0037 => Some(u64::from(a <= b)),
            0x0038 => Some(u64::from(a > b)),
            0x0039 => Some(u64::from(a >= b)),
            0x0040 => Some((a as i64).min(b as i64) as u64), // MINR
            0x0041 => Some((a as i64).max(b as i64) as u64), // MAXR
            0x0042 => Some(a.min(b)),                        // MINUR
            0x0043 => Some(a.max(b)),                        // MAXUR
            0x0050 => Some(a | (1 << sh)),                   // BSETRR
            0x0051 => Some(a & !(1 << sh)),                  // BCLRRR
            0x0052 => Some(a ^ (1 << sh)),                   // BTGLRR
            0x0053 => Some((a >> sh) & 1),                   // BTSTRR -> 0/1
            _ => {
                self.stop = Some(StopReason::InvalidOpcode(op_hi << 16));
                None
            }
        };
        if let Some(r) = res {
            // ANDR/ORR/XORR/ADD/SUB already set their flags; shift forms set zero.
            if matches!(op_hi, 0x0020..=0x0024) {
                self.zero = r == 0;
            }
            self.regs[f.rd] = r;
        }
    }

    /// RV / R group: opcode classes 0x08?, 0x09?, 0x0A?, 0x0B?, 0x0F?
    /// (the register-immediate arithmetic/logic/bit/rotate/extend block).
    /// `op12` = word[15:4]; the destination register is rs2 = word[3:0].
    fn exec_rv_group(&mut self, op12: u32, _full: u32, f: &Fields, imm: u32) {
        let rd = f.rs2; // for these forms reg is in [3:0]
        let rs = self.regs[rd];
        let imm_s = i64::from(imm as i32) as u64; // sign-extended
        let imm_z = u64::from(imm); // zero-extended
        let mut pc_adv: u32 = 4;
        match op12 {
            0x080 => {
                self.regs[rd] = imm_s; // SETR sign-extend
                pc_adv = 8;
            }
            0x081 => {
                self.regs[rd] = self.add_flags(rs, imm_z, 0); // ADDV zero-extend, ADD flags
                pc_adv = 8;
            }
            0x082 => {
                self.regs[rd] = self.sub_flags(rs, imm_z, 0); // MINUSV
                pc_adv = 8;
            }
            0x083 => {
                self.cmp(rs, imm_s); // CMPRV sign-extend, flags only
                pc_adv = 8;
            }
            0x084 => {
                self.regs[rd] = self.add_flags(rs, 1, 0); // INCR
            }
            0x085 => {
                self.regs[rd] = self.sub_flags(rs, 1, 0); // DECR
            }
            0x086 => {
                let r = rs & imm_z; // ANDV zero-extend, sets zero only
                self.zero = r == 0;
                self.regs[rd] = r;
                pc_adv = 8;
            }
            0x087 => {
                let r = rs | imm_z; // ORV
                self.zero = r == 0;
                self.regs[rd] = r;
                pc_adv = 8;
            }
            0x088 => {
                let r = rs ^ imm_z; // XORV
                self.zero = r == 0;
                self.regs[rd] = r;
                pc_adv = 8;
            }
            0x089 => {
                // SETFR: rd = {zero,equal,carry,overflow, 60'b0} in TOP 4 bits.
                let mut v: u64 = 0;
                if self.zero {
                    v |= 1 << 63;
                }
                if self.equal {
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
            0x08A => {
                let r = rs.wrapping_neg(); // NEGR sets zero
                self.zero = r == 0;
                self.regs[rd] = r;
            }
            0x08B => {
                // ABSR: |rs| signed, sets zero AND overflow (ABSR is an overflow producer).
                // INT_MIN is NOT special-cased: abs wraps to INT_MIN, overflow=0.
                let v = rs as i64;
                let r = v.wrapping_abs() as u64;
                self.zero = r == 0;
                self.overflow = false;
                self.regs[rd] = r;
            }
            0x08C => {
                let r = i64::from(rs as i8) as u64; // SEXTB, sets zero/sign
                self.set_zs(r);
                self.regs[rd] = r;
            }
            0x08D => {
                self.regs[rd] = rs << 1; // SHLR1 (no flags per doc; logic)
            }
            0x08E => {
                self.regs[rd] = rs >> 1; // SHRR1
            }
            0x08F => {
                self.regs[rd] = rs << 1; // SHLAR (== logical left)
            }
            0x090 => {
                self.regs[rd] = ((rs as i64) >> 1) as u64; // SHRAR arithmetic
            }
            0x091 => {
                let r = rs << u64::from(imm & 0x3F); // SHLV sets zero
                self.zero = r == 0;
                self.regs[rd] = r;
                pc_adv = 8;
            }
            0x092 => {
                let r = rs >> u64::from(imm & 0x3F); // SHRV
                self.zero = r == 0;
                self.regs[rd] = r;
                pc_adv = 8;
            }
            0x093 => {
                let r = ((rs as i64) >> i64::from(imm & 0x3F)) as u64; // SHRAV
                self.zero = r == 0;
                self.regs[rd] = r;
                pc_adv = 8;
            }
            0x094 => {
                let r = i64::from(rs as i16) as u64; // SEXTH
                self.set_zs(r);
                self.regs[rd] = r;
            }
            0x095 => {
                let r = rs & 0xFF; // ZEXTB sets zero
                self.zero = r == 0;
                self.regs[rd] = r;
            }
            0x096 => {
                let r = rs & 0xFFFF; // ZEXTH
                self.zero = r == 0;
                self.regs[rd] = r;
            }
            0x097 => {
                self.regs[rd] = rs.swap_bytes(); // BSWAP
            }
            0x098 => {
                let r = !rs; // NOTR sets zero
                self.zero = r == 0;
                self.regs[rd] = r;
            }
            0x099 => {
                // LEAPC: rd = PC_of_this_insn + sign_ext(imm32), zero-extended.
                self.regs[rd] = u64::from(self.pc.wrapping_add(imm));
                pc_adv = 8;
            }
            0x0A0 => {
                self.regs[rd] = rs | (1 << (imm & 0x3F)); // BSET
                pc_adv = 8;
            }
            0x0A1 => {
                self.regs[rd] = rs & !(1 << (imm & 0x3F)); // BCLR
                pc_adv = 8;
            }
            0x0A2 => {
                self.regs[rd] = rs ^ (1 << (imm & 0x3F)); // BTGL
                pc_adv = 8;
            }
            0x0A3 => {
                let bit = (rs >> (imm & 0x3F)) & 1; // BTST: zero_flag = NOT(bit), no write
                self.zero = bit == 0;
                pc_adv = 8;
            }
            0x0A8 => {
                self.regs[rd] = u64::from(rs.count_ones()); // POPCNT
            }
            0x0A9 => {
                self.regs[rd] = u64::from(rs.leading_zeros()); // CLZ; CLZ(0)=64 (leading_zeros)
            }
            0x0AA => {
                self.regs[rd] = u64::from(rs.trailing_zeros()); // CTZ; CTZ(0)=64
            }
            0x0AB => {
                self.regs[rd] = rs.reverse_bits(); // BITREV
            }
            0x0AC => {
                // BEXTR: start=imm[4:0], len=imm[12:8]; low 32 bits only; zero-extend.
                let start = u64::from(imm & 0x1F);
                let len = u64::from((imm >> 8) & 0x1F);
                let src = rs & 0xFFFF_FFFF;
                let mask = if len >= 32 { 0xFFFF_FFFF } else { (1_u64 << len) - 1 };
                self.regs[rd] = (src >> start) & mask;
                pc_adv = 8;
            }
            0x0AD => {
                // BDEP: deposit len bits of rs at start into rs (low 32 bits).
                let start = u64::from(imm & 0x1F);
                let len = u64::from((imm >> 8) & 0x1F);
                let mask = if len >= 32 { 0xFFFF_FFFF } else { (1_u64 << len) - 1 };
                let field = (rs & mask) << start;
                let clear = !(mask << start) & 0xFFFF_FFFF;
                self.regs[rd] = (rs & clear) | (field & 0xFFFF_FFFF);
                pc_adv = 8;
            }
            0x0B8 => {
                self.regs[rd] = (rs as i64).wrapping_mul(imm_s as i64) as u64; // MULV signed imm
                pc_adv = 8;
            }
            0x0B9 => {
                // DIVV signed by sign_ext(imm); div-by-0 → all-ones, overflow=1, zero untouched.
                let d = imm_s as i64;
                if d == 0 {
                    self.regs[rd] = 0xFFFF_FFFF_FFFF_FFFF;
                    self.overflow = true;
                } else {
                    self.regs[rd] = (rs as i64).wrapping_div(d) as u64;
                }
                pc_adv = 8;
            }
            0x0BA => {
                // MODV signed; mod-by-0 → NO writeback at all, overflow=1, zero untouched.
                let d = imm_s as i64;
                if d == 0 {
                    self.overflow = true;
                } else {
                    self.regs[rd] = (rs as i64).wrapping_rem(d) as u64;
                }
                pc_adv = 8;
            }
            0x0F0 => {
                self.regs[rd] = i64::from(rs as i32) as u64; // SEXTW
            }
            0x0F1 => {
                self.regs[rd] = rs & 0xFFFF_FFFF; // ZEXTW
            }
            0x0F8 => {
                self.regs[rd] = rs.rotate_left(1); // ROLR1
            }
            0x0F9 => {
                self.regs[rd] = rs.rotate_right(1); // RORR1
            }
            0x0FA => {
                // ROLCR: rotate-left-through-carry (carry producer).
                let new_carry = (rs >> 63) & 1 == 1;
                self.regs[rd] = (rs << 1) | u64::from(self.carry);
                self.carry = new_carry;
            }
            0x0FB => {
                // RORCR: rotate-right-through-carry.
                let new_carry = rs & 1 == 1;
                self.regs[rd] = (rs >> 1) | (u64::from(self.carry) << 63);
                self.carry = new_carry;
            }
            0x0FC => {
                self.regs[rd] = rs.rotate_left(imm & 0x3F); // ROLV
                pc_adv = 8;
            }
            0x0FD => {
                self.regs[rd] = rs.rotate_right(imm & 0x3F); // RORV
                pc_adv = 8;
            }
            0x0FE => {
                // SETR64 (V64): rd = {hi32, lo32}; lo32 @ PC+4, hi32 @ PC+8.
                let lo = self.read32(self.pc + 4);
                let hi = self.read32(self.pc + 8);
                self.regs[rd] = (u64::from(hi) << 32) | u64::from(lo);
                pc_adv = 12;
            }
            _ => {
                self.stop = Some(StopReason::InvalidOpcode(op12 << 4));
            }
        }
        if self.stop.is_none() {
            self.pc = self.pc.wrapping_add(pc_adv);
        }
    }

    /// Flow control (0x1000..0x102F absolute + 0x1030..0x1041 PC-relative).
    fn exec_flow(&mut self, full: u32, f: &Fields) {
        // JMPR R (0x102?) — single word, target in rs2.
        if full & 0xFFF0 == 0x1020 {
            self.pc = self.regs[f.rs2] as u32;
            return;
        }
        // RET (0x1012)
        if full == 0x1012 {
            let ra = self.read64(self.sp);
            self.sp = self.sp.wrapping_add(8);
            self.pc = ra as u32;
            return;
        }
        let imm = self.read32(self.pc.wrapping_add(4));
        let cond = self.flow_cond(full);
        let is_call = matches!(full, 0x1009..=0x1011) || full == 0x1041;
        let is_rel = (0x1030..=0x1041).contains(&full);
        let next = self.pc.wrapping_add(8);
        if is_call {
            if cond {
                self.sp = self.sp.wrapping_sub(8);
                self.write64(self.sp, u64::from(next)); // push PC+8 zero-extended
                self.pc = if is_rel { self.pc.wrapping_add(imm) } else { imm };
            } else {
                self.pc = next;
            }
        } else if cond {
            self.pc = if is_rel { self.pc.wrapping_add(imm) } else { imm };
        } else {
            self.pc = next;
        }
    }

    /// Evaluate the branch/call condition for a flow opcode.
    fn flow_cond(&self, full: u32) -> bool {
        match full {
            0x1000 | 0x1009 | 0x1030 | 0x1041 => true, // JMP/CALL/JMPREL/CALLREL
            0x1001 | 0x100A | 0x1031 => self.zero,
            0x1002 | 0x100B | 0x1032 => !self.zero,
            0x1003 | 0x100C | 0x1033 => self.equal,
            0x1004 | 0x100D | 0x1034 => !self.equal,
            0x1005 | 0x100E | 0x1035 => self.carry,
            0x1006 | 0x100F | 0x1036 => !self.carry,
            0x1007 | 0x1010 => self.overflow,
            0x1008 | 0x1011 => !self.overflow,
            0x1013 | 0x1037 => self.sign,
            0x1014 | 0x1038 => !self.sign,
            0x1015 | 0x1039 => self.less,
            0x1016 | 0x103A => self.less || self.equal,
            0x1017 | 0x103B => !self.less && !self.equal,
            0x1018 | 0x103C => !self.less,
            0x1019 | 0x103D => self.ult,
            0x101A | 0x103E => self.ult || self.equal,
            0x101B | 0x103F => !self.ult && !self.equal,
            0x101C | 0x1040 => !self.ult,
            _ => false,
        }
    }

    /// Stack ops (0x40xx) — PUSH/POP/PUSHV/GETSP/SETSP/ADDSP/PUSHV64/CALLR.
    fn exec_stack(&mut self, full: u32, f: &Fields) {
        match full & 0xFFF0 {
            0x4000 => {
                // PUSH R
                self.sp = self.sp.wrapping_sub(8);
                self.write64(self.sp, self.regs[f.rs2]);
                self.pc = self.pc.wrapping_add(4);
            }
            0x4010 => {
                // POP R
                self.regs[f.rs2] = self.read64(self.sp);
                self.sp = self.sp.wrapping_add(8);
                self.pc = self.pc.wrapping_add(4);
            }
            0x4030 => {
                // GETSP R: rd = zero_ext(SP)
                self.regs[f.rs2] = u64::from(self.sp);
                self.pc = self.pc.wrapping_add(4);
            }
            0x4040 => {
                // SETSP R
                self.sp = self.regs[f.rs2] as u32;
                self.pc = self.pc.wrapping_add(4);
            }
            0x4070 => {
                // CALLR R: push PC+4, jump to rs2
                let next = self.pc.wrapping_add(4);
                self.sp = self.sp.wrapping_sub(8);
                self.write64(self.sp, u64::from(next));
                self.pc = self.regs[f.rs2] as u32;
            }
            _ => match full {
                0x4020 => {
                    // PUSHV V: push zero_ext(imm32)
                    let v = u64::from(self.read32(self.pc.wrapping_add(4)));
                    self.sp = self.sp.wrapping_sub(8);
                    self.write64(self.sp, v);
                    self.pc = self.pc.wrapping_add(8);
                }
                0x4050 => {
                    // ADDSP V: SP += sign_ext(imm32)
                    let off = self.read32(self.pc.wrapping_add(4));
                    self.sp = self.sp.wrapping_add(off);
                    self.pc = self.pc.wrapping_add(8);
                }
                0x4060 => {
                    // PUSHV64 V64: push {hi32,lo32}
                    let lo = self.read32(self.pc.wrapping_add(4));
                    let hi = self.read32(self.pc.wrapping_add(8));
                    let v = (u64::from(hi) << 32) | u64::from(lo);
                    self.sp = self.sp.wrapping_sub(8);
                    self.write64(self.sp, v);
                    self.pc = self.pc.wrapping_add(12);
                }
                _ => self.stop = Some(StopReason::InvalidOpcode(full)),
            },
        }
    }

    /// UART ops (0x50xx).
    fn exec_uart(&mut self, full: u32, f: &Fields) {
        match full & 0xFFF0 {
            0x5010 => {
                // TXR R: send rs2 as hex.
                let v = self.regs[f.rs2];
                self.tx_reg(v);
                self.pc = self.pc.wrapping_add(4);
                return;
            }
            0x5020 => {
                // TXMEMR R: send 64-bit value at mem[rs2] as hex (aligned doubleword).
                let v = self.read64((self.regs[f.rs2] as u32) & !7);
                self.tx_reg(v);
                self.pc = self.pc.wrapping_add(4);
                return;
            }
            0x5030 => {
                // TXCHARMEMR R: send byte at mem[rs2] (byte-lane select).
                let addr = self.regs[f.rs2] as u32;
                let b = self.read_sub(addr, 1) as u8;
                self.tx_byte(b);
                self.pc = self.pc.wrapping_add(4);
                return;
            }
            0x5040 => {
                // TXSTRMEMR R: null-terminated string from mem[rs2].
                let addr = self.regs[f.rs2] as u32;
                self.tx_string(addr);
                self.pc = self.pc.wrapping_add(4);
                return;
            }
            0x5050 => {
                // RXRB R: blocking receive — no input source modelled; return 0.
                self.regs[f.rs2] = 0;
                self.pc = self.pc.wrapping_add(4);
                return;
            }
            0x5060 => {
                // RXRNB R: non-blocking receive — FIFO empty: rd=0, zero_flag=1.
                self.regs[f.rs2] = 0;
                self.zero = true;
                self.pc = self.pc.wrapping_add(4);
                return;
            }
            _ => {}
        }
        match full {
            0x5000 => {
                // TESTMSG: fixed test string. Emit a stable marker (no expected value uses it).
                self.uart.push_str("TEST\r\n");
                self.pc = self.pc.wrapping_add(4);
            }
            0x5001 => {
                // NEWLINE: RTL `t_tx_newline` (uart_tasks.vh:484) emits LF then CR.
                self.uart.push_str("\n\r");
                self.pc = self.pc.wrapping_add(4);
            }
            0x5002 => {
                // TXMEM V: 64-bit value at mem[imm32] as hex (aligned doubleword).
                let addr = self.read32(self.pc.wrapping_add(4)) & !7;
                let v = self.read64(addr);
                self.tx_reg(v);
                self.pc = self.pc.wrapping_add(8);
            }
            0x5003 => {
                // TXSTRMEM V: null-terminated string at mem[imm32].
                let addr = self.read32(self.pc.wrapping_add(4));
                self.tx_string(addr);
                self.pc = self.pc.wrapping_add(8);
            }
            _ => self.stop = Some(StopReason::InvalidOpcode(full)),
        }
    }

    /// Transmit a null-terminated little-endian string from memory.
    fn tx_string(&mut self, start: u32) {
        let mut addr = start;
        loop {
            let b = self.read_sub(addr, 1) as u8;
            if b == 0 || (addr as usize) >= self.mem.len() {
                break;
            }
            self.tx_byte(b);
            addr = addr.wrapping_add(1);
        }
    }

    /// LCD ops (0x20xx) — modelled as no-ops (no architectural state).
    fn exec_lcd(&mut self, full: u32, _f: &Fields) {
        // 0x2021/0x2022/0x2023 are V (2-word); 0x200?/0x201? are R (1-word).
        let two_word = matches!(full, 0x2021..=0x2023);
        self.pc = self.pc.wrapping_add(if two_word { 8 } else { 4 });
    }

    /// Board I/O (0x30xx) — LEDs / 7-seg / switches. No architectural effect,
    /// except SWR reads switches (modelled as 0).
    fn exec_io(&mut self, full: u32, f: &Fields) {
        match full & 0xFFF0 {
            0x3010 => {
                // SWR R: read switch status (modelled as 0).
                self.regs[f.rs2] = 0;
                self.pc = self.pc.wrapping_add(4);
                return;
            }
            0x3000 | 0x3020 | 0x3030 | 0x3040 | 0x3050 | 0x3060 => {
                // LEDR / 7SEG*R / RGB*R — R form, no effect.
                self.pc = self.pc.wrapping_add(4);
                return;
            }
            _ => {}
        }
        // 0x3070..0x3075 are V (2-word); 0x3073 (7SEGBLANK) is 1-word.
        let two_word = matches!(full, 0x3070 | 0x3071 | 0x3072 | 0x3074 | 0x3075);
        self.pc = self.pc.wrapping_add(if two_word { 8 } else { 4 });
    }

    /// Interrupt ops (0x60xx) — INTSETRR / IRET. Stubbed: no interrupt model.
    fn exec_interrupt(&mut self, full: u32, _f: &Fields) {
        if full == 0x6011 {
            // IRET: pop saved context, restore PC[31:0] and flags. With no
            // interrupt dispatch modelled this still correctly unwinds an
            // explicitly-pushed context if one exists.
            let ctx = self.read64(self.sp);
            self.sp = self.sp.wrapping_add(8);
            self.pc = ctx as u32;
            self.zero = (ctx >> 38) & 1 == 1;
            self.equal = (ctx >> 37) & 1 == 1;
            self.carry = (ctx >> 36) & 1 == 1;
            self.overflow = (ctx >> 35) & 1 == 1;
            self.sign = (ctx >> 34) & 1 == 1;
            self.less = (ctx >> 33) & 1 == 1;
            self.ult = (ctx >> 32) & 1 == 1;
            return;
        }
        // INTSETRR RR: configure handler — no architectural register effect here.
        self.pc = self.pc.wrapping_add(4);
    }

    /// Misc Fxxx ops — DELAY / NOP / HALT / RESET / TRAP.
    fn exec_misc(&mut self, full: u32, _f: &Fields, _imm: u32) {
        match full {
            0xF010 => {
                self.pc = self.pc.wrapping_add(4); // NOP
            }
            0xF011 => {
                self.halted = true; // HALT
            }
            0xF012 => {
                self.pc = 0x4; // RESET → PC=0x4
            }
            0xF013 => {
                self.pc = self.pc.wrapping_add(8); // DELAYV V (spin → no-op)
            }
            0xF014 => {
                self.stop = Some(StopReason::Trap); // TRAP
            }
            _ => {
                if full & 0xFFF0 == 0xF000 {
                    self.pc = self.pc.wrapping_add(4); // DELAYR R (spin → no-op)
                } else {
                    self.stop = Some(StopReason::InvalidOpcode(full));
                }
            }
        }
    }

    /// 64-bit register-addressed and value-addressed memory (0x70xx..0x7Bxx).
    fn exec_mem(&mut self, op: u32, full: u32, f: &Fields) {
        match op {
            0x70 => {
                // MEMSET64RR RR: mem64[rs2] = rs1  (rs1=[7:4]=data, rs2=[3:0]=addr).
                // The 64-bit bus returns the aligned doubleword; the cache reads
                // addr & ~7 for any byte address. Align to match the hardware.
                let data = self.regs[f.rs1];
                let address = (self.regs[f.rs2] as u32) & !7;
                self.write64(address, data);
                self.pc = self.pc.wrapping_add(4);
            }
            0x71 => {
                // MEMREADRR RR: rd = mem64[rs2]  (rd=[7:4], addr=[3:0])
                let addr = (self.regs[f.rs2] as u32) & !7;
                self.regs[f.rs1] = self.read64(addr);
                self.pc = self.pc.wrapping_add(4);
            }
            0x72 => {
                // 0x720? MEMSETR RV: mem64[imm32]=rs ; 0x721? MEMREADR RV: rd=mem64[imm32]
                let imm = self.read32(self.pc.wrapping_add(4)) & !7;
                if (full >> 4).trailing_zeros() >= 4 {
                    self.write64(imm, self.regs[f.rs2]); // MEMSETR, reg in [3:0]
                } else {
                    self.regs[f.rs2] = self.read64(imm); // MEMREADR
                }
                self.pc = self.pc.wrapping_add(8);
            }
            0x73 => {
                // STIDX64R RRV: mem64[rs2 + reg[imm[3:0]]] = rs1
                let imm = self.read32(self.pc.wrapping_add(4));
                let off = self.regs[(imm & 0xF) as usize];
                let addr = ((self.regs[f.rs2].wrapping_add(off)) as u32) & !7;
                self.write64(addr, self.regs[f.rs1]);
                self.pc = self.pc.wrapping_add(8);
            }
            0x74 => {
                self.write_sub(self.regs[f.rs2] as u32, self.regs[f.rs1], 1); // MEMSET8
                self.pc = self.pc.wrapping_add(4);
            }
            0x75 => {
                self.regs[f.rs1] = self.read_sub(self.regs[f.rs2] as u32, 1); // MEMGET8
                self.pc = self.pc.wrapping_add(4);
            }
            0x76 => {
                self.write_sub((self.regs[f.rs2] as u32) & !1, self.regs[f.rs1], 2); // MEMSET16
                self.pc = self.pc.wrapping_add(4);
            }
            0x77 => {
                self.regs[f.rs1] = self.read_sub((self.regs[f.rs2] as u32) & !1, 2); // MEMGET16
                self.pc = self.pc.wrapping_add(4);
            }
            0x78 => {
                self.write_sub((self.regs[f.rs2] as u32) & !3, self.regs[f.rs1], 4); // MEMSET32
                self.pc = self.pc.wrapping_add(4);
            }
            0x79 => {
                // MEMGET32: ANY alignment (no & ~3) — reads 4 bytes from rs2 as-is.
                self.regs[f.rs1] = self.read_sub(self.regs[f.rs2] as u32, 4);
                self.pc = self.pc.wrapping_add(4);
            }
            0x7A => {
                self.write64((self.regs[f.rs2] as u32) & !7, self.regs[f.rs1]); // MEMSET64
                self.pc = self.pc.wrapping_add(4);
            }
            0x7B => {
                self.regs[f.rs1] = self.read64((self.regs[f.rs2] as u32) & !7); // MEMGET64
                self.pc = self.pc.wrapping_add(4);
            }
            _ => self.stop = Some(StopReason::InvalidOpcode(full)),
        }
    }

    /// 64-bit indexed memory (0x0C/0x0D/0x0E).
    fn exec_indexed64(&mut self, op: u32, f: &Fields, imm: u32) {
        let base = self.regs[f.rs2] as u32;
        match op {
            0x0C => {
                // LDIDX64: rd = mem64[rs2 + zero_ext(imm32)]
                let addr = base.wrapping_add(imm);
                self.regs[f.rs1] = self.read64(addr);
            }
            0x0D => {
                // STIDX64: mem64[rs2 + zero_ext(imm32)] = rs1
                let addr = base.wrapping_add(imm);
                self.write64(addr, self.regs[f.rs1]);
            }
            0x0E => {
                // LDIDX64R: rd = mem64[rs2 + reg[imm[3:0]]]
                let off = self.regs[(imm & 0xF) as usize] as u32;
                let addr = base.wrapping_add(off);
                self.regs[f.rs1] = self.read64(addr);
            }
            _ => self.stop = Some(StopReason::InvalidOpcode(op << 8)),
        }
        self.pc = self.pc.wrapping_add(8);
    }

    /// Indexed sub-word load/store (0xC0..0xC7).
    fn exec_indexed_sub(&mut self, op: u32, f: &Fields, imm: u32) {
        let ea = (self.regs[f.rs2] as u32).wrapping_add(imm);
        match op {
            0xC0 => self.regs[f.rs1] = self.read_sub(ea & !3, 4),                          // LDIDX32 (& ~3)
            0xC1 => self.write_sub(ea & !3, self.regs[f.rs1], 4),                          // STIDX32
            0xC2 => self.regs[f.rs1] = self.read_sub(ea & !1, 2),                          // LDIDX16
            0xC3 => self.write_sub(ea & !1, self.regs[f.rs1], 2),                          // STIDX16
            0xC4 => self.regs[f.rs1] = self.read_sub(ea, 1),                               // LDIDX8
            0xC5 => self.write_sub(ea, self.regs[f.rs1], 1),                               // STIDX8
            0xC6 => self.regs[f.rs1] = i64::from(self.read_sub(ea, 1) as i8) as u64,       // LDIDX8_S
            0xC7 => self.regs[f.rs1] = i64::from(self.read_sub(ea & !1, 2) as i16) as u64, // LDIDX16_S
            _ => self.stop = Some(StopReason::InvalidOpcode(op << 8)),
        }
        self.pc = self.pc.wrapping_add(8);
    }

    /// Forced-aligned 64-bit indexed memory (0xFC/0xFD) — address & ~7.
    fn exec_indexed64a(&mut self, op: u32, f: &Fields, imm: u32) {
        let ea = ((self.regs[f.rs2] as u32).wrapping_add(imm)) & !7;
        match op {
            0xFC => self.regs[f.rs1] = self.read64(ea), // LDIDX64A
            0xFD => self.write64(ea, self.regs[f.rs1]), // STIDX64A
            _ => self.stop = Some(StopReason::InvalidOpcode(op << 8)),
        }
        self.pc = self.pc.wrapping_add(8);
    }
}

/// Emulate a flat DDR image starting at `entry`. Convenience wrapper.
///
/// Returns the result and, if `want_trace`, the full trace text.
#[must_use]
pub fn emulate_image(image: &[u8], entry: u32, max_instructions: u64, want_trace: bool) -> (EmulateResult, Option<String>) {
    let mut cpu = Cpu::new(image, entry);
    let mut trace = want_trace.then(String::new);
    let result = cpu.run(max_instructions, trace.as_mut());
    (result, trace)
}

/// The default entry point for an assembled `.kla` program (code base 0x20).
#[must_use]
#[allow(dead_code, reason = "public golden-model API; used by tests and external callers")]
pub const fn default_entry() -> u32 {
    CODE_BASE
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests may unwrap/expect")]
    use super::*;
    use crate::helper::build_ddr_image;

    /// Assemble a tiny program from raw 32-bit words (already in board order) into
    /// a flat code byte vector (little-endian), then wrap in a DDR image.
    fn image_from_words(words: &[u32]) -> Vec<u8> {
        let mut code = Vec::new();
        for w in words {
            code.extend_from_slice(&w.to_le_bytes());
        }
        build_ddr_image(&code)
    }

    #[test]
    fn test_setr_and_add_flags() {
        // SETR A 0xFFFFFFFF ; INCR A  -> A wraps to 0, zero flag set.
        // SETR A: 0x0000_0800 (rd=A=0), imm 0xFFFFFFFF
        // INCR A: 0x0000_0840
        let words = [0x0000_0800, 0xFFFF_FFFF, 0x0000_0840, 0x0000_F011];
        let img = image_from_words(&words);
        let mut cpu = Cpu::new(&img, default_entry());
        let r = cpu.run(100, None);
        assert_eq!(r.stop, StopReason::Halt);
        assert_eq!(cpu.regs[0], 0);
        assert!(cpu.zero);
    }

    #[test]
    fn test_txr_output_format() {
        // SETR A 0xFF ; TXR A ; NEWLINE ; HALT
        // RTL-faithful UART: full 64-bit value, 16 hex chars MS-first, then "\n\r".
        let words = [0x0000_0800, 0x0000_00FF, 0x0000_5010, 0x0000_5001, 0x0000_F011];
        let img = image_from_words(&words);
        let (r, _) = emulate_image(&img, default_entry(), 100, false);
        assert_eq!(r.uart, "00000000000000FF\n\r");
    }

    #[test]
    fn test_div_by_zero() {
        // SETR A 5 ; DIVV A 0 -> A = all ones, overflow set, zero untouched.
        let words = [0x0000_0800, 0x0000_0005, 0x0000_0B90, 0x0000_0000, 0x0000_F011];
        let img = image_from_words(&words);
        let mut cpu = Cpu::new(&img, default_entry());
        cpu.run(100, None);
        assert_eq!(cpu.regs[0], 0xFFFF_FFFF_FFFF_FFFF);
        assert!(cpu.overflow);
    }

    /// Run words and return the final CPU for assertions.
    fn run_words(words: &[u32]) -> Cpu {
        let img = image_from_words(words);
        let mut cpu = Cpu::new(&img, default_entry());
        cpu.run(1000, None);
        cpu
    }

    /// Encode an RRR ALU op: `op_hi` in [31:16], rd[11:8], rs1[7:4], rs2[3:0].
    fn rrr(op_hi: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
        (op_hi << 16) | (rd << 8) | (rs1 << 4) | rs2
    }

    #[test]
    fn test_arithmetic_family() {
        // Mirrors test_arithmetic.kla intent with the REAL opcodes.
        // SETR A 0x10; SETR B 0x20; ADDR A A B  -> A = 0x30
        let mut w = vec![0x0000_0800, 0x10, 0x0000_0801, 0x20, rrr(0x0001, 0, 0, 1), 0x0000_F011];
        let cpu = run_words(&w);
        assert_eq!(cpu.regs[0] as u32, 0x30);
        // SUBR A A B -> A = 0x10
        w = vec![0x0000_0800, 0x30, 0x0000_0801, 0x20, rrr(0x0002, 0, 0, 1), 0x0000_F011];
        let cpu = run_words(&w);
        assert_eq!(cpu.regs[0] as u32, 0x10);
        // ADDV A 100 with A=0x0A -> 0x6E
        let w = vec![0x0000_0800, 0x0A, 0x0000_0810, 100, 0x0000_F011];
        let cpu = run_words(&w);
        assert_eq!(cpu.regs[0] as u32, 0x6E);
    }

    #[test]
    fn test_logic_family() {
        // AND: 0xFF & 0x12 = 0x12 ; OR: 0xFF00|0x00FF=0xFFFF ; XOR: 0xFFFF^0x00FF=0xFF00
        let cpu = run_words(&[0x0000_0800, 0xFF, 0x0000_0801, 0x12, rrr(0x0003, 0, 0, 1), 0x0000_F011]);
        assert_eq!(cpu.regs[0] as u32, 0x12);
        let cpu = run_words(&[0x0000_0800, 0xFF00, 0x0000_0801, 0x00FF, rrr(0x0004, 0, 0, 1), 0x0000_F011]);
        assert_eq!(cpu.regs[0] as u32, 0xFFFF);
        let cpu = run_words(&[0x0000_0800, 0xFFFF, 0x0000_0801, 0x00FF, rrr(0x0005, 0, 0, 1), 0x0000_F011]);
        assert_eq!(cpu.regs[0] as u32, 0xFF00);
        // BSWAP is 64-bit (matches RTL bit_reverse/byte-swap width): SETR sign-extends
        // 0x12345678 to 0x0000000012345678, and swap_bytes -> 0x7856341200000000.
        // (test_logic.kla's 0x78563412 expectation is a stale 32-bit assumption.)
        let cpu = run_words(&[0x0000_0800, 0x1234_5678, 0x0000_0970, 0x0000_F011]);
        assert_eq!(cpu.regs[0], 0x7856_3412_0000_0000);
    }

    #[test]
    fn test_muldiv_family() {
        // MULR 10*10=100; DIVR 100/10=10; MODR 10%7=3
        let cpu = run_words(&[0x0000_0800, 10, 0x0000_0801, 10, rrr(0x0010, 0, 0, 1), 0x0000_F011]);
        assert_eq!(cpu.regs[0] as u32, 100);
        let cpu = run_words(&[0x0000_0800, 100, 0x0000_0801, 10, rrr(0x0014, 0, 0, 1), 0x0000_F011]);
        assert_eq!(cpu.regs[0] as u32, 10);
        let cpu = run_words(&[0x0000_0800, 10, 0x0000_0801, 7, rrr(0x0016, 0, 0, 1), 0x0000_F011]);
        assert_eq!(cpu.regs[0] as u32, 3);
        // MULV signed: A=10, MULV A 5 -> 50
        let cpu = run_words(&[0x0000_0800, 10, 0x0000_0B80, 5, 0x0000_F011]);
        assert_eq!(cpu.regs[0] as u32, 50);
        // MODV by 0 -> NO writeback (A stays 17), overflow set.
        let cpu = run_words(&[0x0000_0800, 17, 0x0000_0BA0, 0, 0x0000_F011]);
        assert_eq!(cpu.regs[0] as u32, 17);
        assert!(cpu.overflow);
    }

    #[test]
    fn test_compare_and_branch() {
        // CMPRR with A<B sets less; JMPLT taken. Build:
        // SETR A 5; SETR B 0x10; CMPRR A B; JMPLT PASS; SETR P 0xFF(fail path) HALT; PASS: SETR P 1; HALT
        // Encode CMPRR A B = 0x0000_0501; JMPLT = 0x0000_1015.
        // Layout addresses (code base 0x20):
        // 0x20 SETR A 5      (8)
        // 0x28 SETR B 0x10   (8)
        // 0x30 CMPRR A B     (4)
        // 0x34 JMPLT 0x40    (8)
        // 0x3C HALT          (4)   <- fail lands here only if not taken
        // 0x40 SETR P 1 (rd=15) (8)
        // 0x48 HALT
        let words = [
            0x0000_0800,
            5, // SETR A 5
            0x0000_0801,
            0x10,        // SETR B 0x10
            0x0000_0501, // CMPRR A B
            0x0000_1015,
            0x40,        // JMPLT 0x40
            0x0000_F011, // HALT (fail)
            0x0000_080F,
            1,           // SETR P 1   (P = R15)
            0x0000_F011, // HALT
        ];
        let cpu = run_words(&words);
        assert_eq!(cpu.regs[15] as u32, 1, "JMPLT should be taken (A<B)");
        assert!(cpu.less);
    }

    #[test]
    fn test_memory_roundtrip() {
        // SETR A 0x200 (addr); SETR B 0xDEADBEEF; MEMSET64RR B A; MEMREADRR C A; HALT
        // MEMSET64RR = 0x70?? rs1=[7:4]=B(1), rs2=[3:0]=A(0) -> 0x7010
        // MEMREADRR = 0x71?? rd=[7:4]=C(2), addr=[3:0]=A(0) -> 0x7120
        let words = [
            0x0000_0800,
            0x200, // SETR A 0x200
            0x0000_0801,
            0xDEAD_BEEF, // SETR B 0xDEADBEEF
            0x0000_7010, // MEMSET64RR B A
            0x0000_7120, // MEMREADRR C A
            0x0000_F011, // HALT
        ];
        let cpu = run_words(&words);
        assert_eq!(cpu.regs[2] as u32, 0xDEAD_BEEF);
    }

    #[test]
    fn test_shift_family() {
        // SHLV A 4 with A=1 -> 0x10 ; SHRV A 2 with A=0x40 -> 0x10
        let cpu = run_words(&[0x0000_0800, 1, 0x0000_0910, 4, 0x0000_F011]);
        assert_eq!(cpu.regs[0] as u32, 0x10);
        let cpu = run_words(&[0x0000_0800, 0x40, 0x0000_0920, 2, 0x0000_F011]);
        assert_eq!(cpu.regs[0] as u32, 0x10);
    }

    #[test]
    fn test_trace_format() {
        // SETR A 0xFF ; HALT — first trace line should reflect A=0xFF after commit.
        let img = image_from_words(&[0x0000_0800, 0x0000_00FF, 0x0000_F011]);
        let (_r, trace) = emulate_image(&img, default_entry(), 100, true);
        let t = trace.expect("trace");
        let first = t.lines().next().expect("a line");
        assert!(first.starts_with("i=1 pc=00000020 op=00000800"), "got: {first}");
        assert!(first.contains(" r0=00000000000000ff"), "r0 not updated: {first}");
        assert!(first.contains(" sp=08000000 f="), "sp/flags missing: {first}");
        // f is exactly 7 chars after "f=".
        let fpos = first.find(" f=").unwrap() + 3;
        let fbits: String = first[fpos..].chars().take(7).collect();
        assert_eq!(fbits.len(), 7);
        assert!(fbits.chars().all(|c| c == '0' || c == '1'));
    }

    #[test]
    fn test_trace_memory_write_annotation() {
        // SETR A 0x200; SETR B 0xAA; MEMSET64RR B A; HALT — the store line has wr=.
        let img = image_from_words(&[0x0000_0800, 0x200, 0x0000_0801, 0xAA, 0x0000_7010, 0x0000_F011]);
        let (_r, trace) = emulate_image(&img, default_entry(), 100, true);
        let t = trace.expect("trace");
        // The MEMSET64RR line (3rd retired) should carry a wr= annotation at 0x200.
        let store_line = t.lines().nth(2).expect("store line");
        assert!(store_line.contains(" wr=00000200/ff/"), "missing wr: {store_line}");
    }

    #[test]
    fn test_clz_is_64bit() {
        // CLZ of 0x000000FF on a 64-bit register = 56 (matches the RTL, NOT 24).
        let cpu = run_words(&[0x0000_0800, 0x00FF, 0x0000_0A90, 0x0000_F011]);
        assert_eq!(cpu.regs[0], 56);
    }

    #[test]
    fn test_setfr_layout() {
        // Set zero flag via INCR of 0xFFFFFFFF, then SETFR B.
        // SETR A 0xFFFFFFFF; INCR A (zero=1); SETFR B (B=[3:0]=1)
        let words = [0x0000_0800, 0xFFFF_FFFF, 0x0000_0840, 0x0000_0891, 0x0000_F011];
        let img = image_from_words(&words);
        let mut cpu = Cpu::new(&img, default_entry());
        cpu.run(100, None);
        // SETFR B: rd=B=1; top bit (zero) set.
        assert_eq!(cpu.regs[1] >> 63, 1);
    }
}
