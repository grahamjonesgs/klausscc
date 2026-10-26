//! Unit tests for the emulator, driven through hand-encoded v2 words.

use super::*;
use crate::output::image::build_ddr_image;

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

/// The trace of running `image` (at most 100 instructions).
fn trace_of(image: &[u8]) -> String {
    let mut buf = Vec::new();
    let _ = emulate_image(image, default_entry(), 100, Some(&mut buf));
    String::from_utf8(buf).expect("the trace is ASCII")
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
    assert!(cpu.flags.zero());
    assert!(cpu.flags.carry());
}

#[test]
fn test_mmio_uart_tx() {
    // SETR B TX_DATA ; SETR A 'H' ; MEMSET8 [B]=A ; SETR A 'i' ; MEMSET8 [B]=A ; HALT
    // MEMSET8 data=rd=A(0), base=rs1=B(1): 0x5C00_0000 | 0<<8 | 1<<4 = 0x5C00_0010.
    let words = [
        0x8BD0_0100,
        0xF001_0000, // SETR B, TX_DATA
        0x8BD0_0000,
        0x0000_0048, // SETR A, 'H'
        0x5C00_0010, // MEMSET8 [B] = A
        0x8BD0_0000,
        0x0000_0069, // SETR A, 'i'
        0x5C00_0010, // MEMSET8 [B] = A
        HALT,
    ];
    let img = image_from_words(&words);
    let r = emulate_image(&img, default_entry(), 100, None);
    assert_eq!(r.uart, b"Hi");
}

#[test]
fn test_uart_keeps_raw_bytes() {
    let mut cpu = Cpu::new(&image_from_words(&[HALT]), default_entry());
    for b in [0xC3_u8, 0xA9, 0xFF, b'\n'] {
        cpu.mmio_write(0xF001_0000, 0x1200 | u64::from(b));
    }
    assert_eq!(cpu.take_uart(), [0xC3, 0xA9, 0xFF, b'\n'], "the low byte of each store, verbatim");
    assert_eq!(cpu.take_uart(), b"", "taken once");
}

#[test]
fn test_mmio_uart_rx_read_to_consume_and_status() {
    // R1=STATUS, R2=RX_DATA; R3=[R1] before, R4=[R2] consume, R5=[R1] after.
    // MEMGET8 rd, base=rs1: 0x5800_0000 | rd<<8 | rs1<<4.
    let words = [
        0x8BD0_0100,
        0xF001_0010, // SETR B(R1), STATUS
        0x8BD0_0200,
        0xF001_0008, // SETR C(R2), RX_DATA
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
    assert!(cpu.flags.overflow());
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
    assert!(cpu.flags.overflow());
}

#[test]
fn test_compare_and_branch() {
    // SETR A 5 ; SETR B 0x10 ; CMPRR A B ; JMPLT 0x40 ; HALT(fail) ; SETR P 1 ; HALT
    // Layout (code base 0x20): 0x20 SETR A, 0x28 SETR B, 0x30 CMPRR, 0x34 JMPLT->0x40,
    // 0x3C HALT(fail), 0x40 SETR P 1, 0x48 HALT.
    let words = [
        0x8BD0_0000,
        5, // SETR A 5
        0x8BD0_0100,
        0x10,        // SETR B 0x10
        0x4C00_0001, // CMPRR A B  (rs1=A, rs2=B)
        0xA028_0000,
        0x40, // JMPLT 0x40
        HALT, // fail path
        0x8BD0_0F00,
        1, // SETR P 1   (P = R15)
        HALT,
    ];
    let cpu = run_words(&words);
    assert_eq!(cpu.regs[15] as u32, 1, "JMPLT should be taken (A<B)");
    // Signed-less is now DERIVED (L = S ^ V); CMP sets Z/S/C/V like SUB.
    assert_eq!(cpu.eval_cond(5), Some(true), "signed-less (LT) must hold for A<B");
}

// ---- flag-unification model (doc/FLAG_UNIFICATION_CHANGES) ---------------

#[test]
fn test_cmp_sets_full_zscv() {
    // CMP ≡ SUB without writeback: equal operands must set zero and clear the
    // borrow (previously CMP left Z/C/V stale — the whole point of §1.2).
    // SETR A 7 ; SETR B 7 ; CMPRR A B ; HALT.
    let cpu = run_words(&[0x8BD0_0000, 7, 0x8BD0_0100, 7, 0x4C00_0001, HALT]);
    assert!(cpu.flags.zero(), "CMP of equal operands sets zero");
    assert!(!cpu.flags.carry(), "no borrow when a == b");
    assert!(!cpu.flags.sign(), "a - b == 0 is non-negative");
    assert!(!cpu.flags.overflow(), "no signed overflow for 7 - 7");
}

#[test]
fn test_jmpz_after_cmp_taken() {
    // The new-codegen win: JMPZ (COND 1 = Z) after a CMP of equal operands is
    // taken because CMP now sets Z.  Under the old model CMP left Z stale, so
    // this fell through to the HALT-fail path.
    // 0x20 SETR A 7, 0x28 SETR B 7, 0x30 CMPRR A B, 0x34 JMPZ 0x40,
    // 0x3C HALT(fail), 0x40 SETR P 1, 0x48 HALT.
    let words = [
        0x8BD0_0000,
        7, // SETR A 7
        0x8BD0_0100,
        7,           // SETR B 7
        0x4C00_0001, // CMPRR A B
        0xA008_0000,
        0x40, // JMPZ 0x40
        HALT, // fail path
        0x8BD0_0F00,
        1, // SETR P 1
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
        0x8BD0_0000,
        3, // SETR A 3
        0x8BD0_0100,
        5,           // SETR B 5
        0x4460_0001, // SUBR A A B  -> A = -2
        0xA028_0000,
        0x40, // JMPLT 0x40
        HALT, // fail path
        0x8BD0_0F00,
        1, // SETR P 1
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
    assert!(cpu.flags.carry(), "borrow set when a < b (unsigned)");
    assert_eq!(cpu.eval_cond(2), Some(true), "COND C reads raw carry");
    assert_eq!(cpu.eval_cond(7), Some(true), "ULT = C");
    assert_eq!(cpu.eval_cond(8), Some(true), "ULE = C | Z");
    assert!(cpu.flags.unsigned_less(), "derived U = C");

    // Reverse: A=0x10, B=5 ⇒ a > b ⇒ no borrow ⇒ ULT false (so UGE, ¬C, holds).
    let cpu = run_words(&[0x8BD0_0000, 0x10, 0x8BD0_0100, 5, 0x4C00_0001, HALT]);
    assert!(!cpu.flags.carry(), "no borrow when a > b (unsigned)");
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
        0x8BD0_0000,
        0x200, // SETR A 0x200
        0x8BD0_0100,
        0xDEAD_BEEF, // SETR B 0xDEADBEEF
        0x5F00_0100, // MEMSET64RR B A
        0x5B00_0200, // MEMREADRR C A
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
        0x8BD0_0000,
        0, // 0x20 SETR A 0
        0xA200_0000,
        0x38,        // 0x28 CALL 0x38
        HALT,        // 0x30 (return lands here)
        0x6C00_0000, // 0x34 NOP (padding)
        0x8BD0_0000,
        0x42,        // 0x38 SETR A 0x42
        0x6580_0000, // 0x40 RET
    ];
    let cpu = run_words(&words);
    assert_eq!(cpu.regs[0] as u32, 0x42);
    assert_eq!(cpu.sp, STACK_TOP, "RET must unwind the pushed return address");
}

// ---- debugger interface (--gdbserver) ------------------------------------

/// SETR A 0x200 ; SETR B 0x55 ; MEMSET64RR B A ; MEMREADRR C A ; HALT
const STORE_THEN_LOAD: [u32; 7] = [0x8BD0_0000, 0x200, 0x8BD0_0100, 0x55, 0x5F00_0100, 0x5B00_0200, HALT];

#[test]
fn test_debug_step_one_and_regs() {
    let mut cpu = Cpu::new(&image_from_words(&STORE_THEN_LOAD), default_entry());
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(cpu.debug_regs().r[0], 0x200);
    assert_eq!(cpu.debug_regs().pc, 0x28, "SETR is 2 words");
    let mut regs = cpu.debug_regs();
    regs.r[3] = 0x1234;
    regs.flags = 0b1010; // Z and C
    cpu.set_debug_regs(&regs);
    assert_eq!(cpu.debug_regs(), regs);
    while cpu.step_one(true).is_none() {}
    assert_eq!(cpu.step_one(true), Some(StopReason::Halt), "HALT stays final");
    let pc = cpu.debug_regs().pc;
    cpu.clear_stop();
    assert_eq!(cpu.step_one(true), Some(StopReason::Halt), "clear_stop keeps a HALT");
    assert_eq!(cpu.debug_regs().pc, pc, "nothing ran past the HALT");
}

#[test]
fn test_debug_watchpoints_report_after_access() {
    let mut cpu = Cpu::new(&image_from_words(&STORE_THEN_LOAD), default_entry());
    cpu.add_watch(Watch {
        addr: 0x204,
        len: 4,
        kind: WatchKind::Write,
    });
    let mut hits = Vec::new();
    while cpu.step_one(true).is_none() {
        if let Some(hit) = cpu.take_watch_hit() {
            hits.push((hit, cpu.debug_regs().pc));
        }
    }
    // Only the 8-byte store overlaps [0x204, 0x208); the load does not
    // trigger a write watch, and the store's trace read-back is silent.
    let hit = WatchHit {
        addr: 0x204,
        access: Access::Store,
    };
    assert_eq!(hits, vec![(hit, 0x34)]);

    let mut cpu = Cpu::new(&image_from_words(&STORE_THEN_LOAD), default_entry());
    cpu.add_watch(Watch {
        addr: 0x200,
        len: 1,
        kind: WatchKind::Read,
    });
    let mut reads = 0;
    while cpu.step_one(true).is_none() {
        reads += usize::from(cpu.take_watch_hit().is_some());
    }
    assert_eq!(reads, 1, "only MEMREADRR reads 0x200");
}

#[test]
fn test_debug_add_and_remove_watch() {
    let mut cpu = Cpu::new(&image_from_words(&STORE_THEN_LOAD), default_entry());
    let w = Watch {
        addr: 0x200,
        len: 8,
        kind: WatchKind::Access,
    };
    cpu.add_watch(w);
    assert_eq!(cpu.watches(), [w]);
    assert!(!cpu.remove_watch(&Watch { len: 4, ..w }), "only an equal watch is removed");
    assert!(cpu.remove_watch(&w));
    assert_eq!(cpu.watches(), []);
    while cpu.step_one(true).is_none() {
        assert_eq!(cpu.take_watch_hit(), None, "a removed watch never fires");
    }
}

#[test]
fn test_debug_memory_access_skips_mmio_side_effects() {
    let mut cpu = Cpu::new(&image_from_words(&[HALT]), default_entry());
    cpu.feed_uart_rx(b"xy");
    let mut buf = [0_u8; 8];
    assert_eq!(cpu.debug_read(0xF001_0008, &mut buf), 0, "plain debug reads refuse MMIO");
    assert!(!cpu.debug_write(0xF001_0000, b"A"));
    assert_eq!(cpu.debug_read_mmio(0xF001_0008, &mut buf), 8);
    assert_eq!(buf[0], b'x', "an explicit MMIO read pops the RX FIFO");
    assert!(cpu.debug_write(0x100, &[1, 2, 3]));
    let mut back = [0_u8; 3];
    assert_eq!(cpu.debug_read(0x100, &mut back), 3);
    assert_eq!(back, [1, 2, 3]);
    assert!(cpu.debug_write_mmio(0xF001_0000, b"Z"));
    assert_eq!(cpu.take_uart(), b"Z");
}

#[test]
fn test_debug_trap_stops_on_the_trap_and_clear_stop_resumes() {
    // TRAP ; SETR A 7 ; HALT
    let mut cpu = Cpu::new(&image_from_words(&[0x6C04_0000, 0x8BD0_0000, 7, HALT]), default_entry());
    assert_eq!(cpu.step_one(true), Some(StopReason::Trap));
    assert_eq!(cpu.debug_regs().pc, 0x20, "PC stays on the TRAP");
    assert_eq!(cpu.fetch_word(0x20), 0x6C04_0000);
    let mut regs = cpu.debug_regs();
    regs.pc += 4;
    cpu.set_debug_regs(&regs);
    cpu.clear_stop();
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(cpu.debug_regs().r[0], 7);
}

// ---- interrupts / timer -----------------------------------------------------

/// MEMSET32 data=rd, addr=rs1.
const fn memset32(rd: u32, rs1: u32) -> u32 {
    0x5E00_0000 | (rd << 8) | (rs1 << 4)
}
const WAIT: u32 = 0x6C02_0000;
const IRET: u32 = 0x65C0_0000;

/// Install a timer handler at 0x70, period 100 cycles, unmask, WAIT; the
/// handler sets C = 0x77 and IRETs to 0x60, which sets D = 0x55 and halts.
fn timer_program() -> Vec<u32> {
    let mut w = Vec::new();
    w.extend(setr(0, 0x70)); // 0x20
    w.extend(setr(1, 0xF00F_0010)); // 0x28  INT_VEC0
    w.push(memset32(0, 1)); // 0x30
    w.extend(setr(0, 100)); // 0x34
    w.extend(setr(1, 0xF00F_0030)); // 0x3C  TIMER_PERIOD
    w.push(memset32(0, 1)); // 0x44
    w.extend(setr(0, 1)); // 0x48
    w.extend(setr(1, 0xF00F_0000)); // 0x50  INT_MASK
    w.push(memset32(0, 1)); // 0x58
    w.push(WAIT); // 0x5C
    w.extend(setr(3, 0x55)); // 0x60  resume point
    w.push(HALT); // 0x68
    w.push(0x6C00_0000); // 0x6C  NOP
    w.extend(setr(2, 0x77)); // 0x70  handler
    w.push(IRET); // 0x78
    w
}

#[test]
fn test_timer_irq_wakes_wait_and_iret_restores() {
    let mut cpu = Cpu::new(&image_from_words(&timer_program()), default_entry());
    let result = cpu.run(1000, None);
    assert_eq!(result.stop, StopReason::Halt);
    assert_eq!(cpu.regs[2], 0x77, "handler ran");
    assert_eq!(cpu.regs[3], 0x55, "IRET resumed after the WAIT");
    assert_eq!(cpu.int_mask, 1, "IRET restored the mask the entry cleared");
    assert_eq!(cpu.sp, STACK_TOP, "IRET popped the context word");
    let ctx = cpu.peek64(STACK_TOP - 8);
    assert_eq!(ctx as u32, 0x60, "pushed resume PC");
    assert_eq!((ctx >> 39) & 0xF, 1, "pushed INT_MASK");
}

#[test]
fn test_irq_entry_is_its_own_step_and_masks_the_source() {
    let mut cpu = Cpu::new(&image_from_words(&timer_program()), default_entry());
    while cpu.debug_regs().pc != 0x70 {
        assert_eq!(cpu.step_one(true), None);
    }
    assert_eq!(cpu.regs[2], 0, "entry executes no handler instruction");
    assert_eq!(cpu.irq_state().mask, 0, "source masked while the handler runs");
    assert_eq!(cpu.irq_state().pending, 0, "dispatch acknowledged the timer");
}

#[test]
fn test_single_step_never_takes_an_interrupt() {
    let mut cpu = Cpu::new(&image_from_words(&timer_program()), default_entry());
    while cpu.step_one(false).is_none() {}
    assert_eq!(cpu.step_one(false), Some(StopReason::Halt));
    assert_eq!(cpu.regs[2], 0, "handler never ran");
    assert_eq!(cpu.regs[3], 0x55, "WAIT completed as if woken");
    assert_eq!(cpu.irq_state().pending, 1, "the interrupt is left pending");
}

#[test]
fn test_wait_with_nothing_to_wake_it_stops() {
    let mut cpu = Cpu::new(&image_from_words(&[WAIT, HALT]), default_entry());
    assert_eq!(cpu.run(100, None).stop, StopReason::WaitForever(0x24));
}

#[test]
fn test_irq_controller_registers() {
    let mut cpu = Cpu::new(&image_from_words(&[HALT]), default_entry());
    cpu.mmio_write(0xF00F_0018, 0x1234);
    cpu.mmio_write(0xF00F_0030, 500);
    cpu.mmio_write(0xF00F_0000, 0xFF);
    assert_eq!(cpu.mmio_read(0xF00F_0018), 0x1234);
    assert_eq!(cpu.mmio_read(0xF00F_0030), 500);
    assert_eq!(cpu.mmio_read(0xF00F_0000), 0xF, "4-bit mask");
    cpu.tick(250_000);
    assert_eq!(cpu.mmio_read(0xF00F_0040), 2, "millisecond clock");
    assert_eq!(cpu.mmio_read(0xF00F_0008), 1, "timer pending after its period");
}

#[test]
fn test_perf_counters_count_from_the_last_clear() {
    const NOP: u32 = 0x6C00_0000;
    let mut cpu = Cpu::new(&image_from_words(&[NOP, NOP, NOP, HALT]), default_entry());
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(cpu.mmio_read(0xF00D_0010), 2, "PERF_INSTR");
    assert_eq!(cpu.mmio_read(0xF00D_0008), 2 * CYCLES_PER_INSTRUCTION, "PERF_CYCLES");
    cpu.mmio_write(0xF00D_0000, 0b10); // PERF_CTRL without bit 0
    cpu.mmio_write(0xF00D_0008, 1); // not PERF_CTRL
    assert_eq!(cpu.mmio_read(0xF00D_0010), 2, "neither write clears");
    cpu.mmio_write(0xF00D_0000, 1);
    assert_eq!(cpu.mmio_read(0xF00D_0010), 0, "PERF_CTRL bit 0 clears");
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(cpu.mmio_read(0xF00D_0010), 1);
    assert_eq!(cpu.mmio_read(0xF00D_0008), CYCLES_PER_INSTRUCTION);
    assert_eq!(cpu.mmio_read(0xF00D_0018), 0, "unmapped offset");
}

#[test]
fn test_irq_vector_registers_are_doubleword_aligned() {
    let mut cpu = Cpu::new(&image_from_words(&[HALT]), default_entry());
    for (i, offset) in [0x10_u32, 0x18, 0x20, 0x28].into_iter().enumerate() {
        cpu.mmio_write(0xF00F_0000 | offset, 0x100 + i as u64);
    }
    assert_eq!(cpu.irq_state().vectors, [0x100, 0x101, 0x102, 0x103]);
    cpu.mmio_write(0xF00F_0014, 0xDEAD);
    assert_eq!(cpu.mmio_read(0xF00F_0014), 0, "between vector registers");
    assert_eq!(cpu.irq_state().vectors, [0x100, 0x101, 0x102, 0x103]);
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
    assert!(cpu.flags.zero(), "A == -3 must set Z");
    let cpu = run_words(&[0x4BDF_D000, 0x4C1F_C000, HALT]); // A=-3; CMPRV A,-4
    assert!(!cpu.flags.zero() && !cpu.flags.sign(), "-3 - (-4) = 1");
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
    let signed_lt = 0x7400_0000 | (1 << 23) | (2 << 8) | 0x01;
    let cpu = run_words(&[0x4BDF_F000, 0x4BDF_E100, signed_lt, 0x4BD0_7200, HALT]);
    assert_eq!(cpu.regs[2], 7, "not taken -> fall through executes SETR C,7");
    // ULT on registers (taken): A=1 <u B=-2 -> skip SETR.
    let unsigned_lt = 0x7400_0000 | (3 << 23) | (2 << 8) | 0x01;
    let cpu = run_words(&[0x4BD0_1000, 0x4BDF_E100, unsigned_lt, 0x4BD0_7200, HALT]);
    assert_eq!(cpu.regs[2], 0, "taken -> SETR C,7 skipped");
    assert!(!cpu.flags.zero() && !cpu.flags.carry(), "fused branch leaves the flags alone");
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
    assert!(cpu.flags.zero());
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
    assert!(cpu.flags.zero(), "low halves equal");
    // Signed 32-bit order: A = 0x0000_0000_8000_0000 (negative as i32) < B = 1.
    let mut w = Vec::new();
    w.extend_from_slice(&setr(0, 0x8000_0000));
    w.extend_from_slice(&[0x5560_0000]); // ZEXTW A (clear sext upper half)
    w.extend_from_slice(&[0x4BD0_1100, 0x4C08_0001, HALT]); // SETR B,1; CMPRRW A,B
    let cpu = run_words(&w);
    assert!(cpu.flags.less(), "i32 0x8000_0000 < 1 (signed)");
    // CMPRVW (2-word, 0x8C18_0000): A=0xFFFF_FFFF_0000_0007 vs 7 -> equal.
    let cpu = run_words(&[0xCBC0_0000, 7, 0xFFFF_FFFF, 0x8C18_0000, 7, HALT]);
    assert!(cpu.flags.zero());
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
    let t = trace_of(&img);
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
    let t = trace_of(&img);
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

/// A trace sink that fails every write, like stdout after its reader quits.
#[derive(Default)]
struct BrokenPipe {
    /// Write calls made.
    writes: usize,
}

impl std::io::Write for BrokenPipe {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        self.writes += 1;
        Err(std::io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn test_trace_stops_at_the_first_write_error_and_the_run_goes_on() {
    let img = image_from_words(&[setr(0, 1)[0], 1, setr(1, 2)[0], 2, HALT]);
    let mut sink = BrokenPipe::default();
    let r = emulate_image(&img, default_entry(), 100, Some(&mut sink));
    assert_eq!(sink.writes, 1, "nothing is written after the first error");
    assert_eq!((r.instructions, r.stop), (3, StopReason::Halt));
}

#[test]
fn test_stale_v1_binary_traps() {
    // Any v1 word has bits [31:30] = 00 (LEN=00) -> ERR_INV_OPCODE on the first fetch.
    let img = image_from_words(&[0x0000_0800, 0xFF, 0x0000_F011]);
    let r = emulate_image(&img, default_entry(), 100, None);
    assert!(matches!(r.stop, StopReason::InvalidOpcode(0x0000_0800)));
}

// ---- BEXTR / BDEP --------------------------------------------------------
// BEXTR rd, rs1, imm32: 0x9300_0000 | rd<<8 | rs1<<4.  BDEP rd, rs1(base),
// rs2(src), imm32: 0x9340_0000 | rd<<8 | rs1<<4 | rs2.  imm32 = start | len<<8.

#[test]
fn bextr_extracts_the_field_and_always_sets_z() {
    // SETR A 0x12345678 ; BEXTR B A start 8 len 8 -> 0x56, Z clear.
    let cpu = run_words(&[0x8BD0_0000, 0x1234_5678, 0x9300_0100, 0x0808, HALT]);
    assert_eq!(cpu.regs[1], 0x56);
    assert!(!cpu.flags.zero());
    // SETR A 0xFFFFFF0F ; BEXTR B A start 4 len 4 -> 0: Z set although F is clear.
    let cpu = run_words(&[0x8BD0_0000, 0xFFFF_FF0F, 0x9300_0100, 0x0404, HALT]);
    assert_eq!(cpu.regs[1], 0);
    assert!(cpu.flags.zero(), "a zero field sets Z");
    // INCR A from all ones sets Z ; SETR B 0x30 ; BEXTR C B start 4 len 2 -> 3 clears it.
    let cpu = run_words(&[0x8BD0_0000, 0xFFFF_FFFF, 0x5788_0000, 0x8BD0_0100, 0x30, 0x9300_0210, 0x0204, HALT]);
    assert_eq!(cpu.regs[2], 3);
    assert!(!cpu.flags.zero(), "a non-zero field clears Z");
}

#[test]
fn bextr_reads_the_low_32_bits_and_zero_extends() {
    // SETR A 0x80000000 sign-extends to 0xFFFFFFFF_80000000 ; BEXTR B A start 28 len 4 -> 8.
    let cpu = run_words(&[0x8BD0_0000, 0x8000_0000, 0x9300_0100, 0x041C, HALT]);
    assert_eq!(cpu.regs[1], 8);
    // len 31 from start 0 keeps bits [30:0] only.
    let cpu = run_words(&[0x8BD0_0000, 0xFFFF_FFFF, 0x9300_0100, 0x1F00, HALT]);
    assert_eq!(cpu.regs[1], 0x7FFF_FFFF);
    // len 0 extracts nothing.
    let cpu = run_words(&[0x8BD0_0000, 0xFFFF_FFFF, 0x9300_0100, 0x0003, HALT]);
    assert_eq!(cpu.regs[1], 0);
    assert!(cpu.flags.zero());
}

#[test]
fn bdep_deposits_the_low_field_of_rs2_and_leaves_flags() {
    // INCR A from all ones sets Z ; SETR A all ones ; SETR B 0xA5 ;
    // BDEP C A B start 4 len 4 -> bits [7:4] of the base become 0x5.
    let cpu = run_words(&[
        0x8BD0_0000,
        0xFFFF_FFFF,
        0x5788_0000, // INCR A -> 0, Z set
        0x8BD0_0000,
        0xFFFF_FFFF, // SETR A all ones (base)
        0x8BD0_0100,
        0xA5, // SETR B 0xA5 (src: only its low 4 bits go in)
        0x9340_0201,
        0x0404, // BDEP C A B start 4 len 4
        HALT,
    ]);
    assert_eq!(cpu.regs[2], 0x0000_0000_FFFF_FF5F, "low 32 bits only, zero-extended");
    assert!(cpu.flags.zero(), "BDEP leaves the flags alone");
    // len 0 deposits nothing: C = low half of the base.
    let cpu = run_words(&[0x8BD0_0000, 0xFFFF_FFFF, 0x8BD0_0100, 0xA5, 0x9340_0201, 0x0004, HALT]);
    assert_eq!(cpu.regs[2], 0xFFFF_FFFF);
}
