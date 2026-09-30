//! Independent ISA emulator (golden-model trace generator) for the KlaussCPU.
//!
//! This is Phase-0 of the FPGA pipelining effort: an architectural reference
//! model that executes a flat DDR image and emits a per-retired-instruction
//! trace.  Semantics follow `EMULATOR_ISA_SEMANTICS.md` (RTL-verified corner
//! cases, which OVERRIDE `CPU_ARCHITECTURE.md`); the per-opcode *encodings* are
//! the **ISA encoding v2** flag-day re-numbering documented in
//! `ISA_ENCODING_V2.md`.  Execution semantics are identical to the v1 CPU (the
//! "no benefit taken" checkpoint build) — only the instruction word layout has
//! changed.
//!
//! The model is intentionally cycle-agnostic: each instruction commits
//! atomically.  Cache, IFB, timing and the DDR multi-cycle pipeline are not
//! modelled (they have no architectural effect).  Interrupts / timer MMIO /
//! WAIT are stubbed (documented in the summary); the validation corpus does
//! not exercise them.
//!
//! ## v2 word-0 layout (`ISA_ENCODING_V2.md` §1)
//!
//! ```text
//!  31 30 29    26 25              16 15  12 11  8 7   4 3   0
//! ┌─────┬────────┬──────────────────┬──────┬─────┬─────┬─────┐
//! │ LEN │ CLASS  │ attributes + OP  │  x   │ rd  │ rs1 │ rs2 │
//! └─────┴────────┴──────────────────┴──────┴─────┴─────┴─────┘
//! ```
//!
//! - **LEN [31:30]**: `01`=1 word, `10`=2 words, `11`=3 words, `00`=illegal.
//! - **CLASS [29:26]**: major class (1=ALU-rr, 2=ALU-imm, 3=cmp, 4=shift/bit,
//!   5=unary, 6=load, 7=store, 8=branch, 9=stack, A=mul/div, B=system, C=I/O).
//! - **rd [11:8]**, **rs1 [7:4]**, **rs2 [3:0]**: register fields (stores put
//!   the data source in `rd`).  `imm32` at PC+4; `imm64` = lo32@PC+4, hi32@PC+8.
//!
//! Decoding is done directly from the 32-bit word's bit fields, independent of
//! the assembler's opcode table — so the emulator is a genuine second
//! implementation, not a re-run of the assembler.

use std::collections::VecDeque;

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

// ---- Memory-mapped I/O (bus_splitter address decode) ------------------------
// The RTL bus_splitter routes addr[31:28] == 0xF to the MMIO space; within that,
// addr[27:16] selects the device (0x001 == UART) and addr[15:0] is the register
// offset.  See KlaussCPU.sv:843 (default read 0) and 2796-2800 (TX fire).

/// UART device select value found in `addr[27:16]` of an MMIO access.
const UART_DEV_SELECT: u32 = 0x001;
/// UART `TX_DATA` register offset (`addr[15:0]`) — write transmits `data[7:0]`.
const UART_TX_DATA: u32 = 0x0000;
/// UART `RX_DATA` register offset — read returns the FIFO head and pops it once.
const UART_RX_DATA: u32 = 0x0008;
/// UART `STATUS` register offset — read returns {`RX_FULL`, `RX_EMPTY`, `TX_BUSY`}.
const UART_STATUS: u32 = 0x0010;
/// Modelled RX FIFO depth.  Only affects the `STATUS.RX_FULL` bit; the RTL depth is
/// a parameter and the emulator has no external byte source that would fill it.
const UART_RX_FIFO_DEPTH: usize = 16;

/// True if `addr` decodes to the MMIO region (`addr[31:28] == 0xF`).
const fn is_mmio(addr: u32) -> bool {
    (addr >> 28) == 0xF
}

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
    /// Captured UART output: the byte stream written to the UART `TX_DATA` MMIO
    /// register (`0xF001_0000`), one byte (`data[7:0]`) per store, in order.
    pub uart: String,
    /// Number of instructions retired.
    pub instructions: u64,
    /// Why execution stopped.
    pub stop: StopReason,
}

/// The architectural machine state.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is a distinct hardware condition flag (zero/sign/carry/overflow)"
)]
pub struct Cpu {
    /// General-purpose registers R0..R15 (64-bit).
    regs: [u64; 16],
    /// Stack pointer (32-bit hardware register, separate from R0..R15).
    sp: u32,
    /// Program counter (32-bit byte address).
    pc: u32,
    // The unified 4-bit flags register `Z S C V` (sticky — only written by the
    // documented producers).  The old compare flags `E`/`L`/`U` are RETIRED as
    // storage; they are DERIVED on read (`E = Z`, `L = S ^ V`, `U = C`) — see
    // [`Cpu::flag_e`] / [`Cpu::flag_l`] / [`Cpu::flag_u`].
    /// Zero flag.
    zero: bool,
    /// Sign flag (MSB of an arithmetic result).
    sign: bool,
    /// Carry / borrow out of bit 63 (x86 borrow convention: `C = 1` ⟺ `a < b`).
    carry: bool,
    /// Signed overflow.
    overflow: bool,
    /// Flat little-endian memory image (128 MiB).
    mem: Vec<u8>,
    /// Captured UART output (`TX_DATA` writes, low byte per store).
    uart: String,
    /// UART receive FIFO — head byte returned (and popped) by an `RX_DATA` read.
    /// Empty unless a caller feeds it via [`Cpu::feed_uart_rx`].
    uart_rx: VecDeque<u8>,
    /// True once a HALT (or other terminator) is reached.
    halted: bool,
    /// Set when an unrecoverable stop condition occurs.
    stop: Option<StopReason>,
    /// Pending memory write for trace annotation (addr, `byte_enable`, data).
    last_write: Option<(u32, u8, u64)>,
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
            mem,
            uart: String::new(),
            uart_rx: VecDeque::new(),
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
    ///
    /// Takes `&mut self` because an MMIO read (`RX_DATA`) is read-to-consume and
    /// mutates the UART FIFO.  Instruction/immediate fetch uses `read32` instead,
    /// so this never fires on a code fetch.
    fn read64(&mut self, addr: u32) -> u64 {
        if is_mmio(addr) {
            return self.mmio_read(addr);
        }
        let a = addr as usize;
        if a + 8 > self.mem.len() {
            return 0;
        }
        let mut b = [0_u8; 8];
        b.copy_from_slice(&self.mem[a..a + 8]);
        u64::from_le_bytes(b)
    }

    /// Read `n` bytes (1/2/4) zero-extended into a u64, little-endian.
    fn read_sub(&mut self, addr: u32, n: usize) -> u64 {
        if is_mmio(addr) {
            // MMIO presents its value in the low bits; a sub-word load takes the
            // low `n` bytes (addr[2]==0 lands the value in [31:0]).
            let mask = if n >= 8 { u64::MAX } else { (1_u64 << (8 * n)) - 1 };
            return self.mmio_read(addr) & mask;
        }
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
        if is_mmio(addr) {
            self.mmio_write(addr, val);
        } else {
            let a = addr as usize;
            if a + 8 <= self.mem.len() {
                self.mem[a..a + 8].copy_from_slice(&val.to_le_bytes());
            }
        }
        self.last_write = Some((addr, 0xFF, val));
    }

    /// Write the low `n` bytes (1/2/4) at `addr`, little-endian; record write.
    fn write_sub(&mut self, addr: u32, val: u64, n: usize) {
        // Byte-enable mask placed at the byte-lane within the doubleword, matching
        // the RTL's byte-enable semantics for the trace `be` field.
        let lane = (addr & 7) as usize;
        let be_bits: u8 = ((1_u16 << n) - 1).rotate_left(lane as u32) as u8;
        if is_mmio(addr) {
            self.mmio_write(addr, val);
            self.last_write = Some((addr & !7, be_bits, val));
            return;
        }
        let a = addr as usize;
        for i in 0..n {
            if a + i < self.mem.len() {
                self.mem[a + i] = (val >> (8 * i)) as u8;
            }
        }
        self.last_write = Some((addr & !7, be_bits, self.read64(addr & !7)));
    }

    // ---- memory-mapped I/O ---------------------------------------------------

    /// Service an MMIO read (`addr[31:28] == 0xF`), returning the value in the low
    /// bits of a 64-bit word.  `addr[27:16] == 0x001` selects the UART; every
    /// other device/offset reads 0 (RTL `default: 0`).
    fn mmio_read(&mut self, addr: u32) -> u64 {
        if (addr >> 16) & 0xFFF != UART_DEV_SELECT {
            return 0;
        }
        match addr & 0xFFFF {
            UART_RX_DATA => u64::from(self.uart_rx.pop_front().unwrap_or(0)), // read-to-consume
            UART_STATUS => {
                // bit0 TX_BUSY (instant-transmit model → always 0),
                // bit1 RX_EMPTY, bit2 RX_FULL.
                let rx_empty = u64::from(self.uart_rx.is_empty());
                let rx_full = u64::from(self.uart_rx.len() >= UART_RX_FIFO_DEPTH);
                (rx_full << 2) | (rx_empty << 1)
            }
            _ => 0, // TX_DATA read-back / unmapped UART offset
        }
    }

    /// Service an MMIO write.  A UART `TX_DATA` store transmits `data[7:0]`; every
    /// other device/offset is ignored (RTL `default: 0`).
    fn mmio_write(&mut self, addr: u32, data: u64) {
        if (addr >> 16) & 0xFFF == UART_DEV_SELECT && (addr & 0xFFFF) == UART_TX_DATA {
            self.tx_byte((data & 0xFF) as u8);
        }
    }

    /// Feed bytes into the UART RX FIFO (for tests / an external input source).
    #[allow(dead_code, reason = "public golden-model API; exercised by tests and external drivers")]
    pub fn feed_uart_rx(&mut self, bytes: &[u8]) {
        self.uart_rx.extend(bytes.iter().copied());
    }

    // ---- flag helpers --------------------------------------------------------

    // Derived compare flags (`E`/`L`/`U`).  The flag-unification model retires
    // these as storage; they are computed on read from the unified `Z/S/C/V`
    // register.  The derivations are bit-identical to the retired flags for every
    // operand pair (proven RTL-side by `tb_flags.sv`).

    /// Derived equal flag: `E = Z`.
    const fn flag_e(&self) -> bool {
        self.zero
    }

    /// Derived signed-less flag: `L = S ^ V`.
    const fn flag_l(&self) -> bool {
        self.sign ^ self.overflow
    }

    /// Derived unsigned-less flag: `U = C` (the x86 borrow convention, §1.3).
    const fn flag_u(&self) -> bool {
        self.carry
    }

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

    /// Emit a raw byte to the UART capture (a UART `TX_DATA` MMIO store).
    fn tx_byte(&mut self, b: u8) {
        self.uart.push(b as char);
    }

    // ---- main execute loop ---------------------------------------------------

    /// Run until HALT / TRAP / cap / fault. Returns the result + trace (if any).
    ///
    /// When `trace` is `Some`, one line per retired instruction is written to the
    /// sink in the `EMULATOR_ISA_SEMANTICS.md` "Trace format" layout.  The trace is
    /// streamed as the run proceeds rather than buffered, so a multi-million
    /// instruction run costs no extra memory.
    pub fn run(&mut self, max_instructions: u64, mut trace: Option<&mut dyn std::io::Write>) -> EmulateResult {
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
    ///
    /// Write errors (e.g. a broken pipe on stdout) are intentionally ignored — a
    /// failed trace write must not abort the golden-model run.
    fn trace_line(&self, out: &mut dyn std::io::Write, i: u64, pc: u32, word: u32) {
        let _ = write!(out, "i={i} pc={pc:08x} op={word:08x}");
        for (idx, r) in self.regs.iter().enumerate() {
            let _ = write!(out, " r{idx}={r:016x}");
        }
        let f = |b: bool| if b { '1' } else { '0' };
        // Fixed 7-char field `{zero,sign,carry,overflow,equal,less,ult}` — the last
        // three are DERIVED from Z/S/C/V (E/L/U retired as storage), matching the
        // RTL self-trace which now presents the same derived word.
        let _ = write!(
            out,
            " sp={:08x} f={}{}{}{}{}{}{}",
            self.sp,
            f(self.zero),
            f(self.sign),
            f(self.carry),
            f(self.overflow),
            f(self.flag_e()),
            f(self.flag_l()),
            f(self.flag_u()),
        );
        if let Some((addr, be, data)) = self.last_write {
            let _ = write!(out, " wr={addr:08x}/{be:02x}/{data:016x}");
        }
        let _ = out.write_all(b"\n");
    }

    // ---- v2 decode + execute -------------------------------------------------

    /// Decode + execute a single v2 instruction word, advancing PC.
    ///
    /// The word is decoded straight from its bit fields (`ISA_ENCODING_V2.md` §1):
    /// LEN gives the instruction length, CLASS selects the handler, and each
    /// handler pulls its own attribute bits from `[25:16]`.
    fn step(&mut self, word: u32) {
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

    /// Class 1 — ALU reg-reg: `rd = rs1 OP rs2` (`ISA_ENCODING_V2.md` §2).
    fn v2_alu_rr(&mut self, word: u32, rd: usize, rs1: usize, rs2: usize, next: u32) {
        let op = (word >> 22) & 0xF;
        let a = self.regs[rs1];
        let b = self.regs[rs2];
        let res = match op {
            0 => self.add_flags(a, b, 0),                    // ADDR
            1 => self.sub_flags(a, b, 0),                    // SUBR
            2 => self.add_flags(a, b, u64::from(self.carry)), // ADDC
            3 => self.sub_flags(a, b, u64::from(self.carry)), // SUBC
            4 => { let r = a & b; self.zero = r == 0; r }    // ANDR — RRR logic sets zero
            5 => { let r = a | b; self.zero = r == 0; r }    // ORR
            6 => { let r = a ^ b; self.zero = r == 0; r }    // XORR
            7 => (a as i64).min(b as i64) as u64,            // MINR (signed)
            8 => (a as i64).max(b as i64) as u64,            // MAXR
            9 => a.min(b),                                   // MINUR (unsigned)
            10 => a.max(b),                                  // MAXUR
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
        let (lhs, rhs) = if !short && (word >> 19) & 1 == 1 { (lhs << 32, rhs << 32) } else { (lhs, rhs) };
        if boolean {
            let base = match pred {
                0 => lhs == rhs,                     // EQ
                1 => (lhs as i64) < (rhs as i64),    // LT (signed)
                2 => (lhs as i64) <= (rhs as i64),   // LE (signed)
                3 => lhs < rhs,                      // ULT
                4 => lhs <= rhs,                     // ULE
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
        let rhs = if (word >> 21) & 1 == 1 { i64::from(((word << 28) as i32) >> 28) as u64 } else { self.regs[rs2] };
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
            0 => { let r = a << cnt; if f { self.zero = r == 0; } self.regs[rd] = r; } // SHL
            1 => { let r = a >> cnt; if f { self.zero = r == 0; } self.regs[rd] = r; } // SHR
            2 => { let r = ((a as i64) >> cnt) as u64; if f { self.zero = r == 0; } self.regs[rd] = r; } // SAR
            3 => {
                let r = a.rotate_left(cnt); // ROL
                if f {
                    self.zero = r == 0;
                    if cnt == 1 { self.carry = (a >> 63) & 1 == 1; }
                }
                self.regs[rd] = r;
            }
            4 => {
                let r = a.rotate_right(cnt); // ROR
                if f {
                    self.zero = r == 0;
                    if cnt == 1 { self.carry = a & 1 == 1; }
                }
                self.regs[rd] = r;
            }
            5 => {
                // RCL — rotate left through carry (ROLCR).
                let new_carry = (a >> 63) & 1 == 1;
                let r = (a << 1) | u64::from(self.carry);
                self.carry = new_carry;
                if f { self.zero = r == 0; }
                self.regs[rd] = r;
            }
            6 => {
                // RCR — rotate right through carry (RORCR).
                let new_carry = a & 1 == 1;
                let r = (a >> 1) | (u64::from(self.carry) << 63);
                self.carry = new_carry;
                if f { self.zero = r == 0; }
                self.regs[rd] = r;
            }
            8 => self.regs[rd] = a | (1 << cnt),   // BSET
            9 => self.regs[rd] = a & !(1 << cnt),  // BCLR
            10 => self.regs[rd] = a ^ (1 << cnt),  // BTGL
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
                let start = u64::from(imm32 & 0x1F);
                let l = u64::from((imm32 >> 8) & 0x1F);
                let mask = if l >= 32 { 0xFFFF_FFFF } else { (1_u64 << l) - 1 };
                self.regs[rd] = ((a & 0xFFFF_FFFF) >> start) & mask;
            }
            13 => {
                // BDEP — deposit rs2's low field into rs1(base) at start; low 32 bits.
                let start = u64::from(imm32 & 0x1F);
                let l = u64::from((imm32 >> 8) & 0x1F);
                let mask = if l >= 32 { 0xFFFF_FFFF } else { (1_u64 << l) - 1 };
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
            1 => { let r = a.wrapping_neg(); if f { self.zero = r == 0; } self.regs[rd] = r; } // NEG
            2 => { let r = !a; if f { self.zero = r == 0; } self.regs[rd] = r; } // NOT
            3 => {
                // ABS — INT_MIN wraps (result INT_MIN, overflow cleared); matches v1.
                let r = (a as i64).wrapping_abs() as u64;
                if f { self.zero = r == 0; self.overflow = false; }
                self.regs[rd] = r;
            }
            4 => {
                let r = match size { 0 => i64::from(a as i8), 1 => i64::from(a as i16), _ => i64::from(a as i32) } as u64;
                if f { self.set_zs(r); }
                self.regs[rd] = r;
            }
            5 => {
                let r = match size { 0 => a & 0xFF, 1 => a & 0xFFFF, _ => a & 0xFFFF_FFFF };
                if f { self.zero = r == 0; }
                self.regs[rd] = r;
            }
            6 => self.regs[rd] = a.swap_bytes(),   // BSWAP (64-bit)
            7 => self.regs[rd] = a.reverse_bits(), // BITREV
            8 => { let r = u64::from(a.count_ones()); if f { self.zero = r == 0; } self.regs[rd] = r; } // POPCNT
            9 => self.regs[rd] = u64::from(a.leading_zeros()),  // CLZ; CLZ(0)=64
            10 => self.regs[rd] = u64::from(a.trailing_zeros()), // CTZ; CTZ(0)=64
            12 => {
                // GETF / SETFR: rd = {zero,equal,carry,overflow} in the top nibble [63:60].
                // `equal` is DERIVED (E = Z) — retired as storage per §1.5.
                let mut v = 0_u64;
                if self.zero { v |= 1 << 63; }
                if self.flag_e() { v |= 1 << 62; }
                if self.carry { v |= 1 << 61; }
                if self.overflow { v |= 1 << 60; }
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
        let val = if size_bytes == 8 { self.read64(ea) } else { self.read_sub(ea, size_bytes) };
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
            0 => self.regs[rs1] as u32,                                    // [rs1]
            1 => (self.regs[rs1] as u32).wrapping_add(imm32),              // rs1 + imm32
            2 => imm32,                                                    // [imm32] absolute
            _ => (self.regs[rs1] as u32).wrapping_add(self.regs[rs2] as u32), // rs1 + rs2
        }
    }

    /// Class 8 — branch / call.
    fn v2_branch(&mut self, word: u32, rs2: usize, imm32: u32, next: u32) {
        let link = (word >> 25) & 1 == 1;  // call (push return address)
        let rel = (word >> 24) & 1 == 1;   // PC-relative displacement
        let rind = (word >> 23) & 1 == 1;  // target = rs2
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
    fn eval_cond(&self, cond: u32) -> Option<bool> {
        Some(match cond {
            0 => true,                          // always
            1 => self.zero,                     // Z
            2 => self.carry,                    // C (raw carry / borrow)
            3 => self.overflow,                 // V
            4 => self.sign,                     // S
            5 => self.flag_l(),                 // LT  = S ^ V
            6 => self.zero || self.flag_l(),    // LE  = Z | (S ^ V)
            7 => self.flag_u(),                 // ULT = C
            8 => self.flag_u() || self.zero,    // ULE = C | Z
            9 => self.flag_e(),                 // E   = Z (alias of COND 1)
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
            3 => { self.regs[rd] = u64::from(self.sp); self.pc = next; } // GETSP
            4 => { self.sp = self.regs[rs1] as u32; self.pc = next; }    // SETSP
            5 => { self.sp = self.sp.wrapping_add(imm32); self.pc = next; } // ADDSP (imm sign-ext, 32-bit add)
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

    /// IRET — pop the saved interrupt context and restore PC[31:0] + flags[38:32].
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
            0 | 2 | 5 => self.pc = next,             // NOP / WAIT / DELAY (all stubbed as fall-through)
            1 => self.halted = true,                 // HALT
            3 => self.pc = 0x4,                      // RESET → PC = 0x4
            4 => self.stop = Some(StopReason::Trap), // TRAP
            _ => self.stop = Some(StopReason::InvalidOpcode(word)),
        }
    }
}

/// Address-alignment mask for a class-6 **load** of `size_bytes` in addressing
/// `mode` with the `A` (force-align) bit. Mirrors the per-instruction masking of
/// the v1 CPU: `MEMGET32` is unaligned-tolerant while `LDIDX32` forces `&~3`;
/// the register/absolute 64-bit reads align while the indexed ones stay raw
/// unless `A=1`.
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

/// Emulate a flat DDR image starting at `entry`, buffering the trace into a `String`.
///
/// Returns the result and, if `want_trace`, the full trace text.  This buffers the
/// whole trace in memory — fine for tests and short programs, but the CLI streams
/// instead via [`emulate_image_to_writer`] to stay bounded on long runs.
#[must_use]
pub fn emulate_image(image: &[u8], entry: u32, max_instructions: u64, want_trace: bool) -> (EmulateResult, Option<String>) {
    let mut cpu = Cpu::new(image, entry);
    let mut buf: Option<Vec<u8>> = want_trace.then(Vec::new);
    let result = cpu.run(max_instructions, buf.as_mut().map(|b| b as &mut dyn std::io::Write));
    let trace = buf.map(|b| String::from_utf8_lossy(&b).into_owned());
    (result, trace)
}

/// Emulate a flat DDR image starting at `entry`, streaming the per-instruction
/// trace to `trace` (if `Some`) as the run proceeds.
///
/// Unlike [`emulate_image`], nothing is buffered: each retired instruction's line
/// is written straight to the sink, so a multi-million instruction run uses no
/// extra memory regardless of trace length.
pub fn emulate_image_to_writer(image: &[u8], entry: u32, max_instructions: u64, trace: Option<&mut dyn std::io::Write>) -> EmulateResult {
    let mut cpu = Cpu::new(image, entry);
    cpu.run(max_instructions, trace)
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

    // ---- v2 instruction encoders (word 0) ------------------------------------
    // Register letters A..P map to nibbles 0..15. `word0 = template | rd<<8 | rs1<<4 | rs2`.

    /// SETR rd, imm32 (sign-extended MOV) — 2 words.
    fn setr(rd: u32, imm: u32) -> [u32; 2] {
        [0x8BD0_0000 | (rd << 8), imm]
    }
    const HALT: u32 = 0x6C01_0000;

    /// Assemble raw 32-bit words (board order) into a flat DDR image.
    fn image_from_words(words: &[u32]) -> Vec<u8> {
        let mut code = Vec::new();
        for w in words {
            code.extend_from_slice(&w.to_le_bytes());
        }
        build_ddr_image(&code)
    }

    /// Run words and return the final CPU for assertions.
    fn run_words(words: &[u32]) -> Cpu {
        let img = image_from_words(words);
        let mut cpu = Cpu::new(&img, default_entry());
        cpu.run(1000, None);
        cpu
    }

    #[test]
    fn test_setr_and_inc_wraps_zero() {
        // SETR A 0xFFFFFFFF (sign-extends to all-ones) ; INCR A -> 0, zero+carry set.
        // INCR A: class-5 INC, rd=rs1=A -> 0x5788_0000.
        let words = [0x8BD0_0000, 0xFFFF_FFFF, 0x5788_0000, HALT];
        let cpu = run_words(&words);
        assert_eq!(cpu.regs[0], 0);
        assert!(cpu.zero);
        assert!(cpu.carry);
    }

    #[test]
    fn test_mmio_uart_tx() {
        // SETR B TX_DATA ; SETR A 'H' ; MEMSET8 [B]=A ; SETR A 'i' ; MEMSET8 [B]=A ; HALT
        // MEMSET8 data=rd=A(0), base=rs1=B(1): 0x5C00_0000 | 0<<8 | 1<<4 = 0x5C00_0010.
        let words = [
            0x8BD0_0100, 0xF001_0000, // SETR B, TX_DATA
            0x8BD0_0000, 0x0000_0048, // SETR A, 'H'
            0x5C00_0010, // MEMSET8 [B] = A
            0x8BD0_0000, 0x0000_0069, // SETR A, 'i'
            0x5C00_0010, // MEMSET8 [B] = A
            HALT,
        ];
        let img = image_from_words(&words);
        let (r, _) = emulate_image(&img, default_entry(), 100, false);
        assert_eq!(r.uart, "Hi");
    }

    #[test]
    fn test_mmio_uart_rx_read_to_consume_and_status() {
        // R1=STATUS, R2=RX_DATA; R3=[R1] before, R4=[R2] consume, R5=[R1] after.
        // MEMGET8 rd, base=rs1: 0x5800_0000 | rd<<8 | rs1<<4.
        let words = [
            0x8BD0_0100, 0xF001_0010, // SETR B(R1), STATUS
            0x8BD0_0200, 0xF001_0008, // SETR C(R2), RX_DATA
            0x5800_0310, // MEMGET8 D(R3) = [B]
            0x5800_0420, // MEMGET8 E(R4) = [C]
            0x5800_0510, // MEMGET8 F(R5) = [B]
            HALT,
        ];
        let img = image_from_words(&words);
        let mut cpu = Cpu::new(&img, default_entry());
        cpu.feed_uart_rx(&[0x41]); // 'A'
        let _ = cpu.run(100, None);
        assert_eq!(cpu.regs[3], 0b000, "RX_EMPTY clear while a byte is queued, TX never busy");
        assert_eq!(cpu.regs[4], 0x41, "RX_DATA returns and pops the FIFO head");
        assert_eq!(cpu.regs[5], 0b010, "RX_EMPTY set once the byte has been consumed");
    }

    #[test]
    fn test_div_by_zero() {
        // SETR A 5 ; DIVV A 0 -> A = all ones, overflow set.
        // DIVV rd=rs1=A: 0xA980_0000, imm 0.
        let words = [0x8BD0_0000, 5, 0xA980_0000, 0, HALT];
        let cpu = run_words(&words);
        assert_eq!(cpu.regs[0], 0xFFFF_FFFF_FFFF_FFFF);
        assert!(cpu.overflow);
    }

    #[test]
    fn test_arithmetic_family() {
        // SETR A 0x10 ; SETR B 0x20 ; ADDR A A B -> 0x30.
        let cpu = run_words(&[0x8BD0_0000, 0x10, 0x8BD0_0100, 0x20, 0x4420_0001, HALT]);
        assert_eq!(cpu.regs[0] as u32, 0x30);
        // SUBR A A B with A=0x30, B=0x20 -> 0x10.
        let cpu = run_words(&[0x8BD0_0000, 0x30, 0x8BD0_0100, 0x20, 0x4460_0001, HALT]);
        assert_eq!(cpu.regs[0] as u32, 0x10);
        // ADDV A 100 (rd=rs1=A) with A=0x0A -> 0x6E.
        let cpu = run_words(&[0x8BD0_0000, 0x0A, 0x8820_0000, 100, HALT]);
        assert_eq!(cpu.regs[0] as u32, 0x6E);
    }

    #[test]
    fn test_logic_family() {
        // ANDR: 0xFF & 0x12 = 0x12 ; ORR: 0xFF00|0x00FF=0xFFFF ; XORR: 0xFFFF^0x00FF=0xFF00
        let cpu = run_words(&[0x8BD0_0000, 0xFF, 0x8BD0_0100, 0x12, 0x4500_0001, HALT]);
        assert_eq!(cpu.regs[0] as u32, 0x12);
        let cpu = run_words(&[0x8BD0_0000, 0xFF00, 0x8BD0_0100, 0x00FF, 0x4540_0001, HALT]);
        assert_eq!(cpu.regs[0] as u32, 0xFFFF);
        let cpu = run_words(&[0x8BD0_0000, 0xFFFF, 0x8BD0_0100, 0x00FF, 0x4580_0001, HALT]);
        assert_eq!(cpu.regs[0] as u32, 0xFF00);
        // BSWAP A A is 64-bit: SETR sign-extends 0x12345678, swap_bytes -> 0x7856341200000000.
        let cpu = run_words(&[0x8BD0_0000, 0x1234_5678, 0x5580_0000, HALT]);
        assert_eq!(cpu.regs[0], 0x7856_3412_0000_0000);
    }

    #[test]
    fn test_muldiv_family() {
        // MULR 10*10=100 ; DIVR 100/10=10 ; MODR 10%7=3
        let cpu = run_words(&[0x8BD0_0000, 10, 0x8BD0_0100, 10, 0x6880_0001, HALT]);
        assert_eq!(cpu.regs[0] as u32, 100);
        let cpu = run_words(&[0x8BD0_0000, 100, 0x8BD0_0100, 10, 0x6980_0001, HALT]);
        assert_eq!(cpu.regs[0] as u32, 10);
        let cpu = run_words(&[0x8BD0_0000, 10, 0x8BD0_0100, 7, 0x6A80_0001, HALT]);
        assert_eq!(cpu.regs[0] as u32, 3);
        // MULV signed: A=10, MULV A 5 -> 50.
        let cpu = run_words(&[0x8BD0_0000, 10, 0xA880_0000, 5, HALT]);
        assert_eq!(cpu.regs[0] as u32, 50);
        // MODV by 0 -> NO writeback (A stays 17), overflow set.
        let cpu = run_words(&[0x8BD0_0000, 17, 0xAA80_0000, 0, HALT]);
        assert_eq!(cpu.regs[0] as u32, 17);
        assert!(cpu.overflow);
    }

    #[test]
    fn test_compare_and_branch() {
        // SETR A 5 ; SETR B 0x10 ; CMPRR A B ; JMPLT 0x40 ; HALT(fail) ; SETR P 1 ; HALT
        // Layout (code base 0x20): 0x20 SETR A, 0x28 SETR B, 0x30 CMPRR, 0x34 JMPLT->0x40,
        // 0x3C HALT(fail), 0x40 SETR P 1, 0x48 HALT.
        let words = [
            0x8BD0_0000, 5,          // SETR A 5
            0x8BD0_0100, 0x10,       // SETR B 0x10
            0x4C00_0001,             // CMPRR A B  (rs1=A, rs2=B)
            0xA028_0000, 0x40,       // JMPLT 0x40
            HALT,                    // fail path
            0x8BD0_0F00, 1,          // SETR P 1   (P = R15)
            HALT,
        ];
        let cpu = run_words(&words);
        assert_eq!(cpu.regs[15] as u32, 1, "JMPLT should be taken (A<B)");
        // Signed-less is now DERIVED (L = S ^ V); CMP sets Z/S/C/V like SUB.
        assert_eq!(cpu.eval_cond(5), Some(true), "signed-less (LT) must hold for A<B");
    }

    // ---- flag-unification model (FLAG_UNIFICATION_CHANGES) -------------------

    #[test]
    fn test_cmp_sets_full_zscv() {
        // CMP ≡ SUB without writeback: equal operands must set zero and clear the
        // borrow (previously CMP left Z/C/V stale — the whole point of §1.2).
        // SETR A 7 ; SETR B 7 ; CMPRR A B ; HALT.
        let cpu = run_words(&[0x8BD0_0000, 7, 0x8BD0_0100, 7, 0x4C00_0001, HALT]);
        assert!(cpu.zero, "CMP of equal operands sets zero");
        assert!(!cpu.carry, "no borrow when a == b");
        assert!(!cpu.sign, "a - b == 0 is non-negative");
        assert!(!cpu.overflow, "no signed overflow for 7 - 7");
    }

    #[test]
    fn test_jmpz_after_cmp_taken() {
        // The new-codegen win: JMPZ (COND 1 = Z) after a CMP of equal operands is
        // taken because CMP now sets Z.  Under the old model CMP left Z stale, so
        // this fell through to the HALT-fail path.
        // 0x20 SETR A 7, 0x28 SETR B 7, 0x30 CMPRR A B, 0x34 JMPZ 0x40,
        // 0x3C HALT(fail), 0x40 SETR P 1, 0x48 HALT.
        let words = [
            0x8BD0_0000, 7,          // SETR A 7
            0x8BD0_0100, 7,          // SETR B 7
            0x4C00_0001,             // CMPRR A B
            0xA008_0000, 0x40,       // JMPZ 0x40
            HALT,                    // fail path
            0x8BD0_0F00, 1,          // SETR P 1
            HALT,
        ];
        let cpu = run_words(&words);
        assert_eq!(cpu.regs[15] as u32, 1, "JMPZ must be taken after CMP of equal operands");
    }

    #[test]
    fn test_signed_branch_after_plain_sub() {
        // Derived-after-arith: JMPLT (COND 5 = S ^ V) is valid after a plain SUB,
        // not just after CMP.  3 - 5 = -2 (sign=1, overflow=0) ⇒ LT taken.  Under
        // the old model `less` was CMP-only and stale here, so this failed.
        // 0x20 SETR A 3, 0x28 SETR B 5, 0x30 SUBR A A B, 0x34 JMPLT 0x40, ...
        let words = [
            0x8BD0_0000, 3,          // SETR A 3
            0x8BD0_0100, 5,          // SETR B 5
            0x4460_0001,             // SUBR A A B  -> A = -2
            0xA028_0000, 0x40,       // JMPLT 0x40
            HALT,                    // fail path
            0x8BD0_0F00, 1,          // SETR P 1
            HALT,
        ];
        let cpu = run_words(&words);
        assert_eq!(cpu.regs[15] as u32, 1, "JMPLT must be taken after a plain SUB that goes negative");
    }

    #[test]
    fn test_borrow_polarity_ult_is_carry() {
        // §1.3 x86 borrow convention: after CMP, C = 1 ⟺ a < b (unsigned).
        // A=5, B=0x10 ⇒ a < b ⇒ carry (borrow) set, ULT/ULE derive true.
        let cpu = run_words(&[0x8BD0_0000, 5, 0x8BD0_0100, 0x10, 0x4C00_0001, HALT]);
        assert!(cpu.carry, "borrow set when a < b (unsigned)");
        assert_eq!(cpu.eval_cond(2), Some(true), "COND C reads raw carry");
        assert_eq!(cpu.eval_cond(7), Some(true), "ULT = C");
        assert_eq!(cpu.eval_cond(8), Some(true), "ULE = C | Z");
        assert!(cpu.flag_u(), "derived U = C");

        // Reverse: A=0x10, B=5 ⇒ a > b ⇒ no borrow ⇒ ULT false (so UGE, ¬C, holds).
        let cpu = run_words(&[0x8BD0_0000, 0x10, 0x8BD0_0100, 5, 0x4C00_0001, HALT]);
        assert!(!cpu.carry, "no borrow when a > b (unsigned)");
        assert_eq!(cpu.eval_cond(7), Some(false), "ULT false ⇒ UGE (¬C) taken");
    }

    #[test]
    fn test_getf_derives_equal_from_zero() {
        // GETF/SETFR nibble is {Z,E,C,V} at [63:60]; E is DERIVED (E = Z), so after
        // a CMP of equal operands both bit63 and bit62 are set and carry (bit61) is 0.
        // SETR A 7 ; SETR B 7 ; CMPRR A B ; SETFR C ; HALT.
        let cpu = run_words(&[0x8BD0_0000, 7, 0x8BD0_0100, 7, 0x4C00_0001, 0x5700_0200, HALT]);
        assert_eq!((cpu.regs[2] >> 63) & 1, 1, "zero bit set");
        assert_eq!((cpu.regs[2] >> 62) & 1, 1, "derived equal bit set (E = Z)");
        assert_eq!((cpu.regs[2] >> 61) & 1, 0, "carry bit clear (a == b, no borrow)");
    }

    #[test]
    fn test_memory_roundtrip() {
        // SETR A 0x200 ; SETR B 0xDEADBEEF ; MEMSET64RR B A ; MEMREADRR C A ; HALT
        // MEMSET64RR data=rd=B(1), addr=rs1=A(0): 0x5F00_0100.
        // MEMREADRR dest=rd=C(2), addr=rs1=A(0): 0x5B00_0200.
        let words = [
            0x8BD0_0000, 0x200,       // SETR A 0x200
            0x8BD0_0100, 0xDEAD_BEEF, // SETR B 0xDEADBEEF
            0x5F00_0100,              // MEMSET64RR B A
            0x5B00_0200,              // MEMREADRR C A
            HALT,
        ];
        let cpu = run_words(&words);
        assert_eq!(cpu.regs[2] as u32, 0xDEAD_BEEF);
    }

    #[test]
    fn test_shift_family() {
        // SHLV A #4 with A=1 -> 0x10 ; SHRV A #2 with A=0x40 -> 0x10.
        // SHLV template 0x5020_4000 | N<<15 (rd=rs1=A=0).
        let cpu = run_words(&[0x8BD0_0000, 1, 0x5020_4000 | (4 << 15), HALT]);
        assert_eq!(cpu.regs[0] as u32, 0x10);
        let cpu = run_words(&[0x8BD0_0000, 0x40, 0x5060_4000 | (2 << 15), HALT]);
        assert_eq!(cpu.regs[0] as u32, 0x10);
    }

    #[test]
    fn test_stack_push_pop() {
        // SETR B 0x1234 ; PUSH B ; SETR B 0 ; POP B ; HALT -> B restored to 0x1234.
        // PUSH rs1=B(1): 0x6400_0010 ; POP rd=B(1): 0x6480_0100.
        let words = [0x8BD0_0100, 0x1234, 0x6400_0010, 0x8BD0_0100, 0, 0x6480_0100, HALT];
        let cpu = run_words(&words);
        assert_eq!(cpu.regs[1] as u32, 0x1234);
    }

    #[test]
    fn test_call_and_ret() {
        // SETR A 0 ; CALL FUNC(0x38) ; HALT ; NOP ; FUNC: SETR A 0x42 ; RET
        // CALL: 0xA200_0000, target 0x38. RET: 0x6580_0000. NOP pads 0x34 so FUNC lands at 0x38.
        let words = [
            0x8BD0_0000, 0,          // 0x20 SETR A 0
            0xA200_0000, 0x38,       // 0x28 CALL 0x38
            HALT,                    // 0x30 (return lands here)
            0x6C00_0000,             // 0x34 NOP (padding)
            0x8BD0_0000, 0x42,       // 0x38 SETR A 0x42
            0x6580_0000,             // 0x40 RET
        ];
        let cpu = run_words(&words);
        assert_eq!(cpu.regs[0] as u32, 0x42);
        assert_eq!(cpu.sp, STACK_TOP, "RET must unwind the pushed return address");
    }

    // ---- ISA v3 short 1-word forms (ISA_V3_PROPOSAL.md §3) ----------------

    #[test]
    fn test_v3_short_alu_and_setr() {
        let cpu = run_words(&[
            0x4BDF_B000, // SETR  A, -5   (short, SGN=1: sign-extended)
            0x4BCF_B100, // MOV   B, 0xFB (short, SGN=0: zero-extended)
            0x483F_F210, // ADDI  C, B, -1 (short, sext)
            0x482F_F310, // ADDV  D, B, 0xFF (short, zext)
            HALT,
        ]);
        assert_eq!(cpu.regs[0], (-5_i64) as u64);
        assert_eq!(cpu.regs[1], 0xFB);
        assert_eq!(cpu.regs[2], 0xFA);
        assert_eq!(cpu.regs[3], 0x1FA);
    }

    #[test]
    fn test_v3_short_cmprv_negative() {
        // The short CMPRV immediate is sign-extended (Fix 6 regression guard).
        let cpu = run_words(&[0x4BDF_D000, 0x4C1F_D000, HALT]); // A=-3; CMPRV A,-3
        assert!(cpu.zero, "A == -3 must set Z");
        let cpu = run_words(&[0x4BDF_D000, 0x4C1F_C000, HALT]); // A=-3; CMPRV A,-4
        assert!(!cpu.zero && !cpu.sign, "-3 - (-4) = 1");
    }

    #[test]
    fn test_v3_short_load_store_scaled() {
        let mut w = Vec::new();
        w.extend_from_slice(&setr(1, 0x1000)); // B = 0x1000
        w.extend_from_slice(&setr(0, 0x8000_0001)); // A = sext -> 0xFFFF_FFFF_8000_0001
        w.extend_from_slice(&[
            0x5F30_2010, // STIDX64 [B + 2*8], A  (short, A=1 aligned)
            0x5B30_2210, // LDIDX64 C, [B + 2*8]
            0x5E2F_F010, // STIDX32 [B - 1*4], A
            0x5AAF_F310, // LDIDX32_S D, [B - 4]
            0x5A2F_F410, // LDIDX32   E, [B - 4]
            HALT,
        ]);
        let cpu = run_words(&w);
        assert_eq!(cpu.regs[2], 0xFFFF_FFFF_8000_0001);
        assert_eq!(cpu.regs[3], 0xFFFF_FFFF_8000_0001);
        assert_eq!(cpu.regs[4], 0x8000_0001);
    }

    #[test]
    fn test_v3_enter_leave() {
        // R15 = 0x77; ENTER 3 (0x6600_0003); SETR A,5; LEAVE (0x6640_0000).
        let cpu = run_words(&[0x4BD7_7F00, 0x6600_0003, 0x4BD0_5000, 0x6640_0000, HALT]);
        assert_eq!(cpu.regs[15], 0x77, "LEAVE restores the caller's R15");
        assert_eq!(cpu.sp, STACK_TOP, "LEAVE unwinds the whole frame");
        // Frame state inside: R15 = SP_entry-8, SP = R15 - 24, [R15] = old R15.
        let cpu = run_words(&[0x4BD7_7F00, 0x6600_0003, HALT]);
        assert_eq!(cpu.regs[15] as u32, STACK_TOP - 8);
        assert_eq!(cpu.sp, STACK_TOP - 8 - 24);
    }

    #[test]
    fn test_v3_leaveret() {
        // CALL f (short) ; HALT ; f: ENTER 1 ; SETR A,9 ; LEAVERET (0x6680_0000)
        let cpu = run_words(&[
            0x4BD7_7F00, // 0x20 SETR R15, 0x77
            0x6300_0002, // 0x24 CALLREL +2 -> 0x2C
            HALT,        // 0x28
            0x6600_0001, // 0x2C ENTER 1
            0x4BD0_9000, // 0x30 SETR A, 9
            0x6680_0000, // 0x34 LEAVERET
        ]);
        assert_eq!(cpu.regs[0], 9);
        assert_eq!(cpu.regs[15], 0x77);
        assert_eq!(cpu.sp, STACK_TOP);
        assert_eq!(cpu.pc, 0x28);
    }

    #[test]
    fn test_v3_fused_branch() {
        // A=3; loop: DECR A; BR.NE A, #0, -1 (imm form) -> A ends at 0.
        // 0x7400_0000 = LEN01 cls D PRED=EQ INV=1 (0x0040_0000) IMM (0x0020_0000);
        // disp13 = -1 -> 0x1FFF << 8.
        let bne = 0x7400_0000 | 0x0040_0000 | 0x0020_0000 | (0x1FFF << 8);
        let cpu = run_words(&[0x4BD0_3000, 0x57C8_0000, bne, HALT]);
        assert_eq!(cpu.regs[0], 0);
        // Signed LT on registers (not taken): A=-1, B=-2: BR.LT A, B, +2 falls through.
        let blt = 0x7400_0000 | (1 << 23) | (2 << 8) | 0x01;
        let cpu = run_words(&[0x4BDF_F000, 0x4BDF_E100, blt, 0x4BD0_7200, HALT]);
        assert_eq!(cpu.regs[2], 7, "not taken -> fall through executes SETR C,7");
        // ULT on registers (taken): A=1 <u B=-2 -> skip SETR.
        let bult = 0x7400_0000 | (3 << 23) | (2 << 8) | 0x01;
        let cpu = run_words(&[0x4BD0_1000, 0x4BDF_E100, bult, 0x4BD0_7200, HALT]);
        assert_eq!(cpu.regs[2], 0, "taken -> SETR C,7 skipped");
        assert!(!cpu.zero && !cpu.carry, "fused branch leaves the flags alone");
    }

    #[test]
    fn test_v3_d3_w_alu() {
        // A = 0x7FFF_FFFF, B = 1: ADDW -> 0xFFFF_FFFF_8000_0000 (sext of low 32).
        let mut w = Vec::new();
        w.extend_from_slice(&setr(0, 0x7FFF_FFFF));
        w.extend_from_slice(&[0x4BD0_1100]); // SETR.S B,1
        w.extend_from_slice(&[0x4428_0201]); // ADDW C, A, B
        w.extend_from_slice(&[0x4468_0310]); // SUBW D, B, A = 1 - 0x7FFFFFFF -> sext
        w.extend_from_slice(&[0x8838_0400, 0x8000_0000]); // ADDIW E, A, 0x80000000 -> 0xFFFF_FFFF (low 32) -> -1
        w.extend_from_slice(&[0x6888_0501]); // MULW F, A, B
        w.extend_from_slice(&[HALT]);
        let cpu = run_words(&w);
        assert_eq!(cpu.regs[2], 0xFFFF_FFFF_8000_0000);
        assert_eq!(cpu.regs[3], i64::from(1_i32.wrapping_sub(0x7FFF_FFFF)) as u64);
        assert_eq!(cpu.regs[4], u64::MAX);
        assert_eq!(cpu.regs[5], 0x7FFF_FFFF);
        // Z follows the W result: 0x1_0000_0000 + 0 -> W result 0 -> Z set.
        let cpu = run_words(&[0xCBC0_0000, 0, 1, 0x4BD0_0100, 0x4428_0201, HALT]);
        assert_eq!(cpu.regs[2], 0);
        assert!(cpu.zero);
    }

    #[test]
    fn test_v3_w_compare() {
        // A = 0x1_0000_0005 (upper half set), B = 5: 64-bit CMPRR differs,
        // 32-bit CMPRRW (0x4C08_0000 | rs1<<4 | rs2) is equal.
        let mut w = Vec::new();
        w.extend_from_slice(&[0xCBC0_0000, 5, 1]); // SETR64 A, 0x1_0000_0005
        w.extend_from_slice(&[0x4BD0_5100]); // SETR B, 5
        w.extend_from_slice(&[0x4C08_0001, HALT]); // CMPRRW A, B
        let cpu = run_words(&w);
        assert!(cpu.zero, "low halves equal");
        // Signed 32-bit order: A = 0x0000_0000_8000_0000 (negative as i32) < B = 1.
        let mut w = Vec::new();
        w.extend_from_slice(&setr(0, 0x8000_0000));
        w.extend_from_slice(&[0x5560_0000]); // ZEXTW A (clear sext upper half)
        w.extend_from_slice(&[0x4BD0_1100, 0x4C08_0001, HALT]); // SETR B,1; CMPRRW A,B
        let cpu = run_words(&w);
        assert!(cpu.flag_l(), "i32 0x8000_0000 < 1 (signed)");
        // CMPRVW (2-word, 0x8C18_0000): A=0xFFFF_FFFF_0000_0007 vs 7 -> equal.
        let cpu = run_words(&[0xCBC0_0000, 7, 0xFFFF_FFFF, 0x8C18_0000, 7, HALT]);
        assert!(cpu.zero);
    }

    #[test]
    fn test_v3_short_branch_and_call() {
        let cpu = run_words(&[
            0x4BD0_0000, // 0x20 SETR A, 0
            0x6100_0002, // 0x24 JMPREL +2 words -> 0x2C
            0x4BD0_1000, // 0x28 SETR A, 1 (skipped)
            0x6300_0002, // 0x2C CALLREL +2 words -> 0x34, pushes PC+4 = 0x30
            HALT,        // 0x30
            0x4BD0_7100, // 0x34 SETR B, 7
            0x6580_0000, // 0x38 RET
        ]);
        assert_eq!(cpu.regs[0], 0);
        assert_eq!(cpu.regs[1], 7);
        assert_eq!(cpu.sp, STACK_TOP);
        assert_eq!(cpu.pc, 0x30, "short CALL must return to PC+4");
        // Backward conditional: A=3; loop: DECR A; JMPNZ -1 word.
        let cpu = run_words(&[0x4BD0_3000, 0x57C8_0000, 0x610F_FFFF, HALT]);
        assert_eq!(cpu.regs[0], 0);
    }

    #[test]
    fn test_clz_is_64bit() {
        // CLZ A A with A=0xFF -> 56 (64-bit register). CLZ: 0x5640_0000.
        let cpu = run_words(&[0x8BD0_0000, 0x00FF, 0x5640_0000, HALT]);
        assert_eq!(cpu.regs[0], 56);
    }

    #[test]
    fn test_setfr_layout() {
        // SETR A 0xFFFFFFFF ; INCR A (zero=1) ; SETFR B -> top bit set.
        // SETFR rd=B(1): 0x5700_0100.
        let words = [0x8BD0_0000, 0xFFFF_FFFF, 0x5788_0000, 0x5700_0100, HALT];
        let cpu = run_words(&words);
        assert_eq!(cpu.regs[1] >> 63, 1);
    }

    #[test]
    fn test_trace_format() {
        // SETR A 0xFF ; HALT — first trace line reflects A=0xFF after commit.
        let img = image_from_words(&[setr(0, 0xFF)[0], setr(0, 0xFF)[1], HALT]);
        let (_r, trace) = emulate_image(&img, default_entry(), 100, true);
        let t = trace.expect("trace");
        let first = t.lines().next().expect("a line");
        assert!(first.starts_with("i=1 pc=00000020 op=8bd00000"), "got: {first}");
        assert!(first.contains(" r0=00000000000000ff"), "r0 not updated: {first}");
        assert!(first.contains(" sp=08000000 f="), "sp/flags missing: {first}");
        let fpos = first.find(" f=").unwrap() + 3;
        let fbits: String = first[fpos..].chars().take(7).collect();
        assert_eq!(fbits.len(), 7);
        assert!(fbits.chars().all(|c| c == '0' || c == '1'));
    }

    #[test]
    fn test_trace_memory_write_annotation() {
        // SETR A 0x200 ; SETR B 0xAA ; MEMSET64RR B A ; HALT — store line has wr=.
        let img = image_from_words(&[0x8BD0_0000, 0x200, 0x8BD0_0100, 0xAA, 0x5F00_0100, HALT]);
        let (_r, trace) = emulate_image(&img, default_entry(), 100, true);
        let t = trace.expect("trace");
        let store_line = t.lines().nth(2).expect("store line");
        assert!(store_line.contains(" wr=00000200/ff/"), "missing wr: {store_line}");
    }

    #[test]
    fn test_setr64_full_width() {
        // SETR64 A 0xDEADBEEF_CAFEBABE -> full 64-bit load. 3-word: 0xCBC0_0000, lo, hi.
        let words = [0xCBC0_0000, 0xCAFE_BABE, 0xDEAD_BEEF, HALT];
        let cpu = run_words(&words);
        assert_eq!(cpu.regs[0], 0xDEAD_BEEF_CAFE_BABE);
    }

    #[test]
    fn test_stale_v1_binary_traps() {
        // Any v1 word has bits [31:30] = 00 (LEN=00) -> ERR_INV_OPCODE on the first fetch.
        let img = image_from_words(&[0x0000_0800, 0xFF, 0x0000_F011]);
        let (r, _) = emulate_image(&img, default_entry(), 100, false);
        assert!(matches!(r.stop, StopReason::InvalidOpcode(0x0000_0800)));
    }
}
