//! Unit tests for the emulator, driven through hand-encoded v2 words.

use super::*;
use crate::layout::CODE_BASE;
use crate::output::image::build_ddr_image;

/// RAM for a test CPU: plenty for the hand-written programs, and far less
/// to allocate than the real 128 MiB.
const TEST_MEM: usize = 1 << 20;
/// A test CPU's initial SP: the top of its RAM.
const TEST_STACK_TOP: u32 = TEST_MEM as u32;

/// A test CPU with `image` loaded and the PC at the code base.
fn cpu_with(image: &[u8]) -> Cpu {
    Cpu::with_mem_size(image, CODE_BASE, TEST_MEM)
}

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
    let _ = emulate_image(image, CODE_BASE, 100, Some(&mut buf));
    String::from_utf8(buf).expect("the trace is ASCII")
}

/// Run words and return the final CPU for assertions.
fn run_words(words: &[u32]) -> Cpu {
    let img = image_from_words(words);
    let mut cpu = cpu_with(&img);
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
fn test_mmio_uart_tx_drops_a_store_while_busy() {
    // SETR B TX_DATA ; SETR A 'H' ; MEMSET8 [B]=A ; SETR A 'i' ; MEMSET8 [B]=A ; HALT
    // MEMSET8 data=rd=A(0), base=rs1=B(1): 0x5C00_0000 | 0<<8 | 1<<4 = 0x5C00_0010.
    // A program starts with the UART idle (as one loaded over the UART does
    // since the KlaussCPU.sv START_WAIT change of 2026-10-26), so 'H', the
    // first store, goes out; 'i' is stored two instructions later, while 'H'
    // is still being sent, and is dropped (uart_send_msg.sv :61-76 ignores a
    // send request while busy).
    let words = [
        0x8BD0_0100,
        0xF001_0000, // SETR B, TX_DATA
        0x8BD0_0000,
        0x0000_0048, // SETR A, 'H'
        0x5C00_0010, // MEMSET8 [B] = A
        0x8BD0_0000,
        0x0000_0069, // SETR A, 'i'
        0x5C00_0010, // MEMSET8 [B] = A  (at CODE_BASE + 0x1C)
        HALT,
    ];
    let img = image_from_words(&words);
    let r = emulate_image(&img, CODE_BASE, 100, None);
    assert_eq!(r.uart, b"H");
    assert_eq!(
        r.tx_busy_writes,
        TxBusyWrites {
            first: vec![TxBusyWrite {
                pc: CODE_BASE + 0x1C,
                byte: b'i'
            }],
            total: 1,
        }
    );
    assert_eq!(
        r.tx_busy_writes.to_string(),
        "UART TX write while busy at PC 0x0000003C (byte 0x69 dropped)"
    );
}

#[test]
fn test_uart_tx_busy_lasts_one_frame() {
    // STATUS.TX_BUSY (bit 0) is set for UART_CYCLES_PER_STORE cycles after a
    // TX_DATA store: 10 bits of 33 clocks (uart_tx.sv :7), uart_tx's IDLE and
    // CLEANUP cycles, and uart_send_msg's IDLE and CLEANUP cycles.
    assert_eq!(UART_CYCLES_PER_BYTE, 332);
    assert_eq!(UART_CYCLES_PER_STORE, 334);
    let mut cpu = cpu_with(&image_from_words(&[HALT]));
    assert_eq!(cpu.mmio_read(0xF001_0010) & 1, 0, "idle at reset");
    cpu.mmio_write(0xF001_0000, u64::from(b'A'));
    assert_eq!(cpu.mmio_read(0xF001_0010) & 1, 1, "busy once a byte is stored");
    cpu.tick(UART_CYCLES_PER_STORE - 1);
    assert_eq!(cpu.mmio_read(0xF001_0010) & 1, 1, "still busy one cycle before the frame ends");
    cpu.mmio_write(0xF001_0000, u64::from(b'B'));
    cpu.tick(1);
    assert_eq!(cpu.mmio_read(0xF001_0010) & 1, 0, "idle when the frame ends");
    cpu.mmio_write(0xF001_0000, u64::from(b'C'));
    assert_eq!(cpu.take_uart(), b"AC", "B was stored while busy");
    let busy = cpu.take_tx_busy_writes();
    assert_eq!((busy.total, busy.first[0].byte), (1, b'B'));
    assert!(cpu.take_tx_busy_writes().is_empty(), "taken once");
}

#[test]
fn test_uart_first_store_at_program_start_is_taken() {
    // START_WAIT now waits for the loader's reply to finish before starting
    // the pipeline, so at the program's first cycle TX_BUSY reads 0 and a
    // store is taken at once; a second one fewer than UART_CYCLES_PER_STORE
    // cycles later is dropped and recorded.
    let mut cpu = cpu_with(&image_from_words(&[HALT]));
    assert_eq!(cpu.mmio_read(0xF001_0010) & 1, 0, "idle at program start");
    cpu.mmio_write(0xF001_0000, u64::from(b'A'));
    cpu.tick(UART_CYCLES_PER_STORE - 1);
    cpu.mmio_write(0xF001_0000, u64::from(b'B'));
    assert_eq!(cpu.take_uart(), b"A", "the first store is taken");
    let busy = cpu.take_tx_busy_writes();
    assert_eq!((busy.total, busy.first[0].byte), (1, b'B'), "the second, within the frame, is dropped");
}

#[test]
fn test_uart_busy_writes_keep_the_first_few() {
    // The first of 41 back-to-back stores goes out; the other 40 are dropped.
    let mut cpu = cpu_with(&image_from_words(&[HALT]));
    for b in 0..41 {
        cpu.mmio_write(0xF001_0000, b);
    }
    assert_eq!(cpu.take_uart(), [0]);
    let busy = cpu.take_tx_busy_writes();
    assert_eq!((busy.first.len(), busy.total), (16, 40));
    assert_eq!(busy.first[0].byte, 1);
    assert!(busy.to_string().ends_with("\n... and 24 more"), "{busy}");
}

#[test]
fn test_uart_keeps_raw_bytes() {
    let mut cpu = cpu_with(&image_from_words(&[HALT]));
    for b in [0xC3_u8, 0xA9, 0xFF, b'\n'] {
        cpu.mmio_write(0xF001_0000, 0x1200 | u64::from(b));
        cpu.tick(UART_CYCLES_PER_STORE);
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
    let mut cpu = cpu_with(&img);
    cpu.feed_uart_rx(&[0x41]); // 'A'
    let _ = cpu.run(100, None);
    assert_eq!(cpu.regs[3], 0b000, "RX_EMPTY clear while a byte is queued, TX idle");
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
    // MODV by 0 -> the dividend (A stays 17), overflow set.
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

// ---- unified Z S C V flag model (doc/ISA_ENCODING_V2.md §7) ----------------------------

#[test]
fn test_cmp_sets_full_zscv() {
    // CMP ≡ SUB without writeback: equal operands must set zero and clear the
    // borrow (previously CMP left Z/C/V stale — see doc/ISA_ENCODING_V2.md §7.2).
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
    // doc/ISA_ENCODING_V2.md §7.1, x86 borrow convention: after CMP, C = 1 ⟺ a < b (unsigned).
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
    assert_eq!(cpu.sp, TEST_STACK_TOP, "RET must unwind the pushed return address");
}

// ---- debugger interface (--gdbserver) ------------------------------------

/// SETR A 0x200 ; SETR B 0x55 ; MEMSET64RR B A ; MEMREADRR C A ; HALT
const STORE_THEN_LOAD: [u32; 7] = [0x8BD0_0000, 0x200, 0x8BD0_0100, 0x55, 0x5F00_0100, 0x5B00_0200, HALT];

#[test]
fn test_debug_step_one_and_regs() {
    let mut cpu = cpu_with(&image_from_words(&STORE_THEN_LOAD));
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
    let mut cpu = cpu_with(&image_from_words(&STORE_THEN_LOAD));
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

    let mut cpu = cpu_with(&image_from_words(&STORE_THEN_LOAD));
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
    let mut cpu = cpu_with(&image_from_words(&STORE_THEN_LOAD));
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
    let mut cpu = cpu_with(&image_from_words(&[HALT]));
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
    let mut cpu = cpu_with(&image_from_words(&[0x6C04_0000, 0x8BD0_0000, 7, HALT]));
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
    let mut cpu = cpu_with(&image_from_words(&timer_program()));
    let result = cpu.run(1000, None);
    assert_eq!(result.stop, StopReason::Halt);
    assert_eq!(cpu.regs[2], 0x77, "handler ran");
    assert_eq!(cpu.regs[3], 0x55, "IRET resumed after the WAIT");
    assert_eq!(cpu.int_mask, 1, "IRET restored the mask the entry cleared");
    assert_eq!(cpu.sp, TEST_STACK_TOP, "IRET popped the context word");
    let ctx = cpu.peek64(TEST_STACK_TOP - 8);
    assert_eq!(ctx as u32, 0x60, "pushed resume PC");
    assert_eq!((ctx >> 39) & 0xF, 1, "pushed INT_MASK");
}

#[test]
fn test_irq_entry_is_its_own_step_and_masks_the_source() {
    let mut cpu = cpu_with(&image_from_words(&timer_program()));
    while cpu.debug_regs().pc != 0x70 {
        assert_eq!(cpu.step_one(true), None);
    }
    assert_eq!(cpu.regs[2], 0, "entry executes no handler instruction");
    assert_eq!(cpu.irq_state().mask, 0, "source masked while the handler runs");
    assert_eq!(cpu.irq_state().pending, 0, "dispatch acknowledged the timer");
}

#[test]
fn test_single_step_never_takes_an_interrupt() {
    let mut cpu = cpu_with(&image_from_words(&timer_program()));
    while cpu.step_one(false).is_none() {}
    assert_eq!(cpu.step_one(false), Some(StopReason::Halt));
    assert_eq!(cpu.regs[2], 0, "handler never ran");
    assert_eq!(cpu.regs[3], 0x55, "WAIT completed as if woken");
    assert_eq!(cpu.irq_state().pending, 1, "the interrupt is left pending");
}

#[test]
fn test_wait_with_nothing_to_wake_it_stops() {
    let mut cpu = cpu_with(&image_from_words(&[WAIT, HALT]));
    assert_eq!(cpu.run(100, None).stop, StopReason::WaitForever(0x24));
}

#[test]
fn test_irq_controller_registers() {
    let mut cpu = cpu_with(&image_from_words(&[HALT]));
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
    let mut cpu = cpu_with(&image_from_words(&[NOP, NOP, NOP, HALT]));
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
    let mut cpu = cpu_with(&image_from_words(&[HALT]));
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
    assert_eq!(cpu.sp, TEST_STACK_TOP, "LEAVE unwinds the whole frame");
    // Frame state inside: R15 = SP_entry-8, SP = R15 - 24, [R15] = old R15.
    let cpu = run_words(&[0x4BD7_7F00, 0x6600_0003, HALT]);
    assert_eq!(cpu.regs[15] as u32, TEST_STACK_TOP - 8);
    assert_eq!(cpu.sp, TEST_STACK_TOP - 8 - 24);
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
    assert_eq!(cpu.sp, TEST_STACK_TOP);
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
    assert_eq!(cpu.sp, TEST_STACK_TOP);
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
    let r = emulate_image(&img, CODE_BASE, 100, Some(&mut sink));
    assert_eq!(sink.writes, 1, "nothing is written after the first error");
    assert_eq!((r.instructions, r.stop), (3, StopReason::Halt));
}

#[test]
fn test_stale_v1_binary_traps() {
    // Any v1 word has bits [31:30] = 00 (LEN=00) -> ERR_INV_OPCODE on the first fetch.
    let img = image_from_words(&[0x0000_0800, 0xFF, 0x0000_F011]);
    let r = emulate_image(&img, CODE_BASE, 100, None);
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
fn bextr_reads_the_full_register_and_zero_extends() {
    // SETR A 0x80000000 sign-extends to 0xFFFFFFFF_80000000 ; BEXTR B A start 28 len 4 -> 8.
    let cpu = run_words(&[0x8BD0_0000, 0x8000_0000, 0x9300_0100, 0x041C, HALT]);
    assert_eq!(cpu.regs[1], 8);
    // start 28 len 8 crosses bit 32: like the RTL's 64-bit shift, bits [35:32]
    // come from the upper word -> 0xF8 (not 0x08 from the low word alone).
    let cpu = run_words(&[0x8BD0_0000, 0x8000_0000, 0x9300_0100, 0x081C, HALT]);
    assert_eq!(cpu.regs[1], 0xF8);
    // start 31 len 31 takes rs1[61:31] -> 31 ones.
    let cpu = run_words(&[0x8BD0_0000, 0x8000_0000, 0x9300_0100, 0x1F1F, HALT]);
    assert_eq!(cpu.regs[1], 0x7FFF_FFFF);
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

// ---- table-driven single-instruction checks --------------------------------
// These encoders build word 0 from the documented fields, independent of the
// assembler's opcode table: LEN=01 [31:30], CLASS [29:26], OP [25:22],
// rd [11:8], rs1 [7:4], rs2 [3:0].

/// A 1-word class-1 ALU reg-reg word: `rd = rs1 OP rs2`, with F `[21]`
/// set for the flag-writing ops 0-3, which require it.
const fn alu_rr(op: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
    0x4400_0000 | (op << 22) | (((op <= 3) as u32) << 21) | (rd << 8) | (rs1 << 4) | rs2
}

/// A 1-word class-4 shift word with an embedded count and `F` (bit 14) set.
const fn shift_imm_f(op: u32, rd: u32, rs1: u32, count: u32) -> u32 {
    0x5000_0000 | (op << 22) | (1 << 21) | (count << 15) | (1 << 14) | (rd << 8) | (rs1 << 4)
}

/// Run the one instruction `word` with R0 = `a`, R1 = `b` and the flags
/// nibble `flags` (Z S C V), returning the CPU after it.
fn exec_one(word: u32, a: u64, b: u64, flags: u8) -> Cpu {
    let mut cpu = cpu_with(&image_from_words(&[word, HALT]));
    let mut regs = cpu.debug_regs();
    regs.r[0] = a;
    regs.r[1] = b;
    regs.flags = flags;
    cpu.set_debug_regs(&regs);
    assert_eq!(cpu.step_one(false), None, "{word:08x} executes");
    cpu
}

#[test]
fn add_and_sub_set_zscv() {
    const MIN: u64 = 1 << 63;
    // (op, a, b, ZSCV after)
    let table = [
        (0, 1, 1, 0b0000),        // ADDR 1 + 1
        (0, u64::MAX, 1, 0b1010), // wraps to 0: Z, C
        (0, MIN - 1, 1, 0b0101),  // i64::MAX + 1: S, V
        (0, MIN, MIN, 0b1011),    // 0: Z, C, V
        (1, 5, 3, 0b0000),        // SUBR 5 - 3
        (1, 3, 5, 0b0110),        // -2: S, C (borrow)
        (1, 7, 7, 0b1000),        // 0: Z
        (1, MIN, 1, 0b0001),      // i64::MIN - 1: V
        (1, 0, MIN, 0b0111),      // 0 - i64::MIN: S, C, V
    ];
    for (op, a, b, zscv) in table {
        let cpu = exec_one(alu_rr(op, 2, 0, 1), a, b, 0);
        assert_eq!(cpu.debug_regs().flags, zscv, "op {op}: {a:#x}, {b:#x}");
        let want = if op == 0 { a.wrapping_add(b) } else { a.wrapping_sub(b) };
        assert_eq!(cpu.regs[2], want);
    }
}

#[test]
fn min_max_compare_signed_or_unsigned_and_leave_flags() {
    let (neg5, three) = (-5_i64 as u64, 3);
    // (op, result of op(-5, 3))
    for (op, want) in [(7, neg5), (8, three), (9, three), (10, neg5)] {
        for flags in [0b0000, 0b1111] {
            let cpu = exec_one(alu_rr(op, 2, 0, 1), neg5, three, flags);
            assert_eq!(cpu.regs[2], want, "op {op}");
            assert_eq!(cpu.debug_regs().flags, flags, "op {op} leaves the flags");
        }
    }
}

#[test]
fn rcl_and_rcr_rotate_one_bit_through_carry() {
    const TOP: u64 = 1 << 63;
    // (op, a, carry in, result, ZSCV after); only N = 1 is legal.
    let table = [
        (5, TOP | 1, false, 0b10, 0b0010),            // RCL: bit 63 -> C
        (5, TOP | 1, true, 0b11, 0b0010),             // C -> bit 0
        (5, TOP, false, 0, 0b1010),                   // F sets Z
        (5, 1, true, 0b11, 0b0000),                   // C cleared
        (6, TOP | 1, false, TOP >> 1, 0b0010),        // RCR: bit 0 -> C
        (6, TOP | 1, true, TOP | (TOP >> 1), 0b0010), // C -> bit 63
        (6, 1, false, 0, 0b1010),
        (6, TOP, true, TOP | (TOP >> 1), 0b0000),
    ];
    for (op, a, carry, want, zscv) in table {
        let cpu = exec_one(shift_imm_f(op, 2, 0, 1), a, 0, if carry { 0b0010 } else { 0 });
        assert_eq!(cpu.regs[2], want, "op {op}: {a:#x} carry {carry}");
        assert_eq!(cpu.debug_regs().flags, zscv, "op {op}: {a:#x} carry {carry}");
    }
}

#[test]
fn reserved_encodings_stop_without_moving_the_pc() {
    // (word, why it is reserved)
    let table = [
        (alu_rr(11, 0, 0, 0), "ALU reg-reg op 11"),
        (0x4C00_0000 | (1 << 21) | (5 << 23), "boolean compare PRED 5"),
        (0x5000_0000 | (7 << 22), "shift op 7"),
        (0x5400_0000 | (11 << 22), "unary op 11"),
        (0x6000_0000 | (10 << 19), "branch COND 10"),
        (0x6800_0000 | (3 << 24), "mul/div op 3"),
        (0x6C00_0000 | (6 << 16), "system op 6"),
        (0x7400_0000 | (5 << 23), "fused branch PRED 5"),
        (0x7C00_0000, "class F"),
        (0xB400_0000, "fused branch is 1 word only"),
    ];
    for (word, why) in table {
        let mut cpu = cpu_with(&image_from_words(&[word, HALT]));
        assert_eq!(cpu.step_one(false), Some(StopReason::InvalidOpcode(word)), "{why}");
        assert_eq!(cpu.debug_regs().pc, CODE_BASE, "{why}");
    }
}

// ---- flag effects checked against pipeline_core.sv (KlaussCPU master) ------

/// A 1-word class-5 unary word `rd = OP(rs1)` with SIZE `[21:20]` and F `[19]`.
const fn unary(op: u32, size: u32, f: bool, rd: u32, rs1: u32) -> u32 {
    0x5400_0000 | (op << 22) | (size << 20) | ((f as u32) << 19) | (rd << 8) | (rs1 << 4)
}

/// A 1-word class-4 word with the count in rs2 (SRC=0, N=0) and F `[14]`.
const fn shift_reg(op: u32, f: bool, rd: u32, rs1: u32, rs2: u32) -> u32 {
    0x5000_0000 | (op << 22) | ((f as u32) << 14) | (rd << 8) | (rs1 << 4) | rs2
}

#[test]
fn and_or_xor_reg_write_no_flags() {
    // pipeline_core.sv:413-415: AND/OR/XOR decode with an empty fmask.
    for op in [4, 5, 6] {
        for (a, b) in [(0xF0, 0x0F), (0xFF, 0xFF), (0, 0)] {
            for flags in [0b0000, 0b1111, 0b0101] {
                let cpu = exec_one(alu_rr(op, 2, 0, 1), a, b, flags);
                let want = match op {
                    4 => a & b,
                    5 => a | b,
                    _ => a ^ b,
                };
                assert_eq!(cpu.regs[2], want, "op {op}");
                assert_eq!(cpu.debug_regs().flags, flags, "op {op}: {a:#x}, {b:#x} leaves the flags");
            }
        }
    }
}

#[test]
fn absr_sets_v_only_for_int_min() {
    // pipeline_core.sv:520-522 (Z and V), :1060-1062 (V = operand == INT_MIN).
    const MIN: u64 = 1 << 63;
    // (a, result, ZSCV after) from flags S C V all set, Z clear.
    let table = [(MIN, MIN, 0b0111), (-5_i64 as u64, 5, 0b0110), (5, 5, 0b0110), (0, 0, 0b1110)];
    for (a, want, zscv) in table {
        let cpu = exec_one(unary(3, 0, true, 2, 0), a, 0, 0b0111);
        assert_eq!(cpu.regs[2], want, "{a:#x}");
        assert_eq!(cpu.debug_regs().flags, zscv, "{a:#x}");
    }
}

#[test]
fn rotate_by_register_count_never_writes_carry() {
    // pipeline_core.sv:467-475: only SRC=1, N=1, F=1 is U_ROT1 (Z and C); a
    // register count is U_SHIFT, which writes Z alone with F.
    const TOP: u64 = 1 << 63;
    for (op, a, want) in [(3, TOP, 1), (4, 1, TOP)] {
        for carry in [0b0000, 0b0010] {
            let cpu = exec_one(shift_reg(op, true, 2, 0, 1), a, 1, carry);
            assert_eq!(cpu.regs[2], want, "op {op}");
            assert_eq!(cpu.debug_regs().flags, carry, "op {op}: C is left alone, Z clear");
        }
        // The embedded N=1 form with F writes C (and Z).
        let cpu = exec_one(shift_imm_f(op, 2, 0, 1), a, 0, 0);
        assert_eq!(cpu.regs[2], want, "op {op}");
        assert_eq!(cpu.debug_regs().flags, 0b0010, "op {op}: C = the bit rotated out");
    }
}

#[test]
fn f_bit_writes_z_on_unary_and_bit_ops() {
    // pipeline_core.sv:483-485, :493-494 (BSET/BCLR/BTGL/BTSTRR) and
    // :516-518, :532-535 (COPY, BSWAP, BITREV, CLZ, CTZ, GETF): F writes Z
    // from the result.  Each word is given with F clear; (word, a, b, result).
    let f14 = 1 << 14;
    let f19 = 1 << 19;
    let table = [
        (unary(0, 0, false, 2, 0), 0, 0, 0, f19),                                 // COPY 0
        (unary(0, 0, false, 2, 0), 7, 0, 7, f19),                                 // COPY 7
        (unary(6, 0, false, 2, 0), 0, 0, 0, f19),                                 // BSWAP 0
        (unary(7, 0, false, 2, 0), 1, 0, 1 << 63, f19),                           // BITREV
        (unary(9, 0, false, 2, 0), 1 << 63, 0, 0, f19),                           // CLZ = 0
        (unary(10, 0, false, 2, 0), 1, 0, 0, f19),                                // CTZ = 0
        (unary(10, 0, false, 2, 0), 0, 0, 64, f19),                               // CTZ(0) = 64
        (shift_reg(8, false, 2, 0, 1), 0, 3, 8, f14),                             // BSETRR
        (shift_reg(9, false, 2, 0, 1), 8, 3, 0, f14),                             // BCLRRR
        (shift_reg(10, false, 2, 0, 1), 8, 3, 0, f14),                            // BTGLRR
        (shift_reg(11, false, 2, 0, 1), 8, 3, 1, f14),                            // BTSTRR set
        (shift_reg(11, false, 2, 0, 1), 8, 2, 0, f14),                            // BTSTRR clear
        (0x5000_0000 | (8 << 22) | (1 << 21) | (5 << 15) | 0x200, 0, 0, 32, f14), // BSET N=5
    ];
    for (word, a, b, want, fbit) in table {
        for flags in [0b0000, 0b1111] {
            // F clear: the flags are left alone.
            let cpu = exec_one(word, a, b, flags);
            assert_eq!(cpu.regs[2], want, "{word:08x}");
            assert_eq!(cpu.debug_regs().flags, flags, "{word:08x} without F");
            // F set: Z = (result == 0), nothing else.
            let cpu = exec_one(word | fbit, a, b, flags);
            assert_eq!(cpu.regs[2], want, "{word:08x} with F");
            let z = if want == 0 { 0b1000 } else { 0 };
            assert_eq!(cpu.debug_regs().flags, (flags & 0b0111) | z, "{word:08x} with F");
        }
    }
}

#[test]
fn getf_with_f_sets_z_from_the_saved_word() {
    // GETF result {Z, Z, C, V, 60'b0} (pipeline_core.sv:1076); F then writes
    // Z = (result == 0) (:532-535), so Z = !(Z | C | V).
    // (flags before, rd, flags after)
    let table = [
        (0b0000, 0, 0b1000),
        (0b0100, 0, 0b1100), // S is not in the word
        (0b0010, 0x2 << 60, 0b0010),
        (0b1000, 0xC << 60, 0b0000),
        (0b0001, 0x1 << 60, 0b0001),
    ];
    for (before, want, after) in table {
        let cpu = exec_one(unary(12, 0, true, 2, 0), 0, 0, before);
        assert_eq!(cpu.regs[2], want, "{before:04b}");
        assert_eq!(cpu.debug_regs().flags, after, "{before:04b}");
        let cpu = exec_one(unary(12, 0, false, 2, 0), 0, 0, before);
        assert_eq!(cpu.debug_regs().flags, before, "{before:04b} without F");
    }
}

/// A 1-word class-A word: KIND `[25:24]` (0 MUL, 1 DIV, 2 MOD), SGN `[23]`,
/// HIGH `[22]` and W `[19]`.
const fn muldiv(kind: u32, sgn: bool, high: bool, w: bool, rd: u32, rs1: u32, rs2: u32) -> u32 {
    0x6800_0000 | (kind << 24) | ((sgn as u32) << 23) | ((high as u32) << 22) | ((w as u32) << 19) | (rd << 8) | (rs1 << 4) | rs2
}

#[test]
fn mul_family_writes_z_s_v() {
    // pipeline_core.sv:655-658 (fmask Z S V), :1157-1163: S = bit 63 of the
    // half written (before W), V = the low half lost product bits, cleared
    // by the high-half forms; Z from the value written (:1549-1550).
    const MIN: u64 = 1 << 63;
    let neg = |v: i64| v as u64;
    // (sgn, high, w, a, b, result, ZSCV after from C and V set)
    let table = [
        (false, false, false, 3, 5, 15, 0b0010),                   // MULUR
        (false, false, false, 0, 5, 0, 0b1010),                    // Z
        (false, false, false, MIN, 2, 0, 0b1011),                  // lost bit: V
        (false, false, false, 1 << 62, 2, MIN, 0b0110),            // fits unsigned: S, no V
        (true, false, false, neg(-3), 5, neg(-15), 0b0110),        // MULR
        (true, false, false, 1 << 62, 2, MIN, 0b0111),             // signed overflow
        (true, false, false, MIN, neg(-1), MIN, 0b0111),           // INT_MIN * -1
        (false, true, false, u64::MAX, u64::MAX, neg(-2), 0b0110), // MULHUR: V cleared
        (false, true, false, 2, 3, 0, 0b1010),
        (true, true, false, neg(-1), 1, u64::MAX, 0b0110),                  // MULHR
        (true, false, true, 0x8000_0000, 1, 0xFFFF_FFFF_8000_0000, 0b0010), // MULW: S from bit 63
        (true, false, true, 1 << 32, 1, 0, 0b1010),                         // Z from the W result
        (true, false, true, MIN, 1, 0, 0b1110),
        (true, false, true, 1 << 62, 4, 0, 0b1011), // V from the 64-bit product
    ];
    for (sgn, high, w, a, b, want, zscv) in table {
        let cpu = exec_one(muldiv(0, sgn, high, w, 2, 0, 1), a, b, 0b0011);
        assert_eq!(cpu.regs[2], want, "sgn {sgn} high {high} w {w}: {a:#x} * {b:#x}");
        assert_eq!(cpu.debug_regs().flags, zscv, "sgn {sgn} high {high} w {w}: {a:#x} * {b:#x}");
    }
}

#[test]
fn div_and_mod_write_z_and_v() {
    // pipeline_core.sv:659-666 (fmask Z V), :1164-1176: a non-zero divisor
    // clears V and writes Z; divide by zero writes only V = 1, with DIV
    // giving all ones and MOD the dividend.
    const MIN: u64 = 1 << 63;
    let neg = |v: i64| v as u64;
    // (kind, sgn, a, b, flags before, result, flags after)
    let table = [
        (1, false, 10, 3, 0b1111, 3, 0b0110),
        (1, false, 2, 3, 0b0001, 0, 0b1000),
        (1, true, neg(-7), 2, 0b1001, neg(-3), 0b0000),
        (1, true, MIN, neg(-1), 0b0000, MIN, 0b0000),
        (2, true, MIN, neg(-1), 0b0001, 0, 0b1000),
        (2, true, neg(-7), 2, 0b0000, neg(-1), 0b0000),
        (2, false, 9, 3, 0b0000, 0, 0b1000),
        (1, false, 5, 0, 0b1000, u64::MAX, 0b1001), // Z kept
        (1, true, 5, 0, 0b0110, u64::MAX, 0b0111),
        (2, false, 5, 0, 0b0000, 5, 0b0001),
        (2, true, 0, 0, 0b0000, 0, 0b0001), // Z not written from the dividend
    ];
    for (kind, sgn, a, b, before, want, after) in table {
        let cpu = exec_one(muldiv(kind, sgn, false, false, 2, 0, 1), a, b, before);
        assert_eq!(cpu.regs[2], want, "kind {kind} sgn {sgn}: {a:#x}, {b:#x}");
        assert_eq!(cpu.debug_regs().flags, after, "kind {kind} sgn {sgn}: {a:#x}, {b:#x}");
    }
}

#[test]
fn modv_by_zero_writes_the_dividend_to_rd() {
    // The 2-word MOD by zero also writes rs1 to rd (pipeline_core.sv:653,
    // :1166), so MODV with rd != rs1 copies the dividend.  SETR A 17 ;
    // SETR C 99 ; MODV C <- A % 0.
    let cpu = run_words(&[0x8BD0_0000, 17, 0x8BD0_0200, 99, 0xAA80_0200, 0, HALT]);
    assert_eq!(cpu.regs[2], 17);
    assert!(cpu.flags.overflow());
}

// ---- decode legality checked against pipeline_core.sv f_decode_raw --------

#[test]
fn every_assembler_form_executes() {
    // Every opcode the assembler can emit, with several operand values in
    // each field, decodes as legal: the RTL legality checks must never trap
    // assembled code.
    use crate::isa::Operand;
    use crate::isa::table::V2_OPCODES;
    /// Values for a word-0 field, before shifting into place.
    fn values(op: Operand) -> &'static [u32] {
        match op {
            Operand::Rd => &[2, 0, 15],
            Operand::Rs1 => &[3, 15, 0],
            Operand::Rs2 => &[4, 0, 15],
            Operand::RdRs1 => &[0x22, 0x00, 0xFF],
            Operand::Count => &[1, 0, 63, 5],
            Operand::Imm8 | Operand::Off8(_) => &[0x80, 0, 0xFF, 0x7F],
            Operand::Frame22 => &[0, 3, 0x3F_FFFF],
            Operand::Simm4 => &[0xF, 0, 7],
            Operand::Rel18 => &[1, 0, 0x3_FFFF],
            Operand::Rel13 => &[1, 0, 0x1FFF],
            Operand::Imm | Operand::Imm64 => &[0],
        }
    }
    for opcode in V2_OPCODES {
        for variant in 0..4 {
            let mut word = opcode.template;
            let mut imm = Vec::new();
            for &op in opcode.ops {
                let mask = op.mask();
                if mask != 0 {
                    let vals = values(op);
                    word |= (vals[variant % vals.len()] << mask.trailing_zeros()) & mask;
                }
                for k in 0..op.imm_words() {
                    imm.push([0x40, 0xFFFF_FFFF, 0x1234_5678, 0][(variant + k as usize) % 4]);
                }
            }
            let mut words = vec![word];
            words.extend(imm);
            words.push(HALT);
            let mut cpu = cpu_with(&image_from_words(&words));
            let mut regs = cpu.debug_regs();
            regs.r = [0x100; 16];
            cpu.set_debug_regs(&regs);
            let stop = cpu.step_one(false);
            assert_ne!(stop, Some(StopReason::InvalidOpcode(word)), "{} as {word:08x}", opcode.name);
        }
    }
}

#[test]
fn rtl_reserved_encodings_trap() {
    // (word, why it is reserved, pipeline_core.sv line)
    let table = [
        (0x4400_0201, "ADDR with F = 0", 408),
        (0x4520_0000, "ANDR with F = 1", 413),
        (0x4510_0000, "class 1 with SGN = 1", 402),
        (0x4500_1000, "class 1 with [18:12] set", 402),
        (0x4508_0000, "W on ANDR", 403),
        (0x8420_0000, "2-word class 1", 402),
        (0x8820_0001, "ADDV with rs2 set", 404),
        (0x8868_0000, "W on MINUSV", 405),
        (0x89C0_0000, "MIN with an immediate", 417),
        (0x8920_0000, "ANDV with F = 1", 413),
        (0xC820_0000, "3-word ADD", 431),
        (0xCBC0_0010, "SETR64 with rs1 set", 432),
        (0x4C80_0000, "flag CMP with PRED = 1", 450),
        (0x4C40_0000, "flag CMP with INV", 450),
        (0x4C00_0100, "flag CMP with rd set", 450),
        (0x4C30_0000, "short boolean compare", 442),
        (0x8C10_0001, "CMPRV with rs2 set", 441),
        (0xCC00_0000, "3-word compare", 440),
        (0x5000_8000, "SRC = 0 shift with N = 1", 461),
        (0x5000_1000, "shift with [13:12] set", 458),
        (0x5161_4000, "ROLCR with N = 2", 478),
        (0x5160_8000, "ROLCR without F", 478),
        (0x5140_4000, "ROLCR with a register count", 478),
        (0x52E0_0100, "BTST with rd set", 489),
        (0x52E0_4000, "BTST with F", 489),
        (0x5300_0000, "1-word BEXTR", 462),
        (0x9300_0001, "BEXTR with rs2 set", 501),
        (0x9300_4000, "BEXTR with F", 500),
        (0x9000_0000, "2-word SHL", 499),
        (0x5458_0000, "NEGR with SIZE = 01", 514),
        (0x5530_0000, "SEXT with SIZE = 11", 524),
        (0x5570_0000, "ZEXT with SIZE = 11", 528),
        (0x54C0_0000, "ABSR without F", 520),
        (0x5780_0000, "INCR without F", 537),
        (0x5400_0001, "COPY with rs2 set", 512),
        (0x9400_0000, "2-word unary", 512),
        (0x5A80_0000, "MEMGET32 with SGN", 560),
        (0x5C80_0000, "store with SGN", 548),
        (0x5810_0000, "A on a byte load", 549),
        (0x5800_0001, "[rs1] load with rs2 set", 552),
        (0x9B40_0010, "[imm] load with rs1 set", 553),
        (0x9820_0001, "rs1+imm load with rs2 set", 554),
        (0x5840_0000, "1-word [imm] load", 550),
        (0x9800_0000, "2-word [rs1] load", 551),
        (0x9860_0000, "2-word rs1+rs2 load", 551),
        (0x5800_1000, "[rs1] load with [19:12] set", 547),
        (0xD800_0000, "3-word load", 550),
        (0xA004_0000, "COND 0 with INV", 575),
        (0xA080_0000, "2-word JMPR", 572),
        (0xA000_0100, "2-word JMP with rd set", 572),
        (0x6180_0000, "1-word REL and RIND", 573),
        (0x6000_0000, "1-word absolute JMP", 573),
        (0x6080_0010, "JMPR with rs1 set", 573),
        (0x6400_0100, "PUSH with rd set", 605),
        (0x6410_0000, "PUSH with [21:20] set", 602),
        (0x6480_0010, "POP with rs1 set", 609),
        (0x6580_0010, "RET with rs1 set", 619),
        (0x6440_0000, "1-word PUSHV", 604),
        (0xA540_0100, "ADDSP with rd set", 629),
        (0x6640_0001, "LEAVE with [21:0] set", 591),
        (0x66C0_0000, "stack op 11", 627),
        (0xA600_0000, "2-word ENTER", 630),
        (0x6810_0000, "MUL with [21:20] set", 650),
        (0x68C8_0000, "W on MULHR", 651),
        (0x6940_0000, "DIV with HIGH", 659),
        (0x6908_0000, "W on DIVR", 651),
        (0xA880_0001, "MULV with rs2 set", 652),
        (0xE880_0000, "3-word MUL", 652),
        (0x6C00_0010, "NOP with rs1 set", 675),
        (0xAC01_0000, "2-word HALT", 683),
        (0xAC05_0010, "DELAYV with rs1 set", 683),
        (0x6C41_0000, "system with [25:22] set", 672),
        (0x7200_0000, "1-word LCDRST", 700),
        (0x7000_0100, "LCDCMDR with rd set", 700),
        (0xB000_0010, "LCDCMDV with rs1 set", 703),
        (0xB300_0000, "LCD op 3", 704),
        (0x7000_1000, "LCD with [23:12] set", 699),
        (0x4000_0000, "class 0", 712),
        (0x7800_0000, "class E", 712),
    ];
    for (word, why, line) in table {
        let mut cpu = cpu_with(&image_from_words(&[word, HALT]));
        assert_eq!(cpu.step_one(false), Some(StopReason::InvalidOpcode(word)), "{why} (:{line})");
        assert_eq!(cpu.debug_regs().pc, CODE_BASE, "{why}");
    }
}

#[test]
fn lcd_words_are_legal_no_ops() {
    // LCDCMDR B ; LCDDATAV 0x41 ; LCDRST 1 ; HALT — nothing architectural
    // changes (pipeline_core.sv:698-711; the port is not modelled).
    let cpu = run_words(&[0x7000_0010, 0xB100_0000, 0x41, 0xB200_0000, 1, HALT]);
    assert_eq!(cpu.stop, Some(StopReason::Halt));
    assert_eq!(cpu.regs, [0; 16]);
}

// ---- 64-bit accesses reach the aligned doubleword (mem_read_write.sv) -----

/// The 16 RAM bytes at `addr`.
fn ram16(cpu: &Cpu, addr: u32) -> [u8; 16] {
    let mut buf = [0; 16];
    assert_eq!(cpu.debug_read(addr, &mut buf), 16);
    buf
}

#[test]
fn unaligned_64bit_load_and_store_use_the_aligned_doubleword() {
    // The data cache selects the doubleword by addr[4:3] and ignores
    // addr[2:0] for reads and writes (mem_read_write.sv:274-277, :610-632,
    // :672-676), and LDIDX64/STIDX64 (A=0) issue the address unaligned
    // (pipeline_core.sv:1113, :1185).
    // SETR A 0x1000 ; SETR64 B v ; STIDX64 B A 3 ; LDIDX64 C A 5 ;
    // SETR A 0x1006 ; MEMREADRR D A ; MEMSET64RR B A ; HALT
    let v: u64 = 0x1122_3344_5566_7788;
    let words = [
        0x8BD0_0000,
        0x1000, // SETR A 0x1000
        0xCBC0_0100,
        v as u32,
        (v >> 32) as u32, // SETR64 B v
        0x9F20_0100,
        3, // STIDX64 B -> [A + 3]
        0x9B20_0200,
        5, // LDIDX64 C <- [A + 5]
        0x8BD0_0000,
        0x1006,      // SETR A 0x1006
        0x5B00_0300, // MEMREADRR D <- [A]
        0x5F00_0100, // MEMSET64RR B -> [A]
        HALT,
    ];
    let cpu = run_words(&words);
    assert_eq!(cpu.regs[2], v, "LDIDX64 at 0x1005 reads the doubleword at 0x1000");
    assert_eq!(cpu.regs[3], v, "MEMREADRR at 0x1006 too");
    let mut want = [0; 16];
    want[..8].copy_from_slice(&v.to_le_bytes());
    assert_eq!(ram16(&cpu, 0x1000), want, "the stores wrote 0x1000..0x1008 only");
    // The trace shows the address as issued (ret_wr_addr = mem_iaddr,
    // pipeline_core.sv:1895).
    let t = trace_of(&image_from_words(&words));
    let store_line = t.lines().nth(2).expect("store line");
    assert!(store_line.contains(" wr=00001003/ff/1122334455667788"), "{store_line}");
}

#[test]
fn unaligned_sp_pushes_and_pops_the_aligned_doubleword() {
    // PUSH/POP issue SP-8 / SP unaligned (pipeline_core.sv:1118-1128); RAM
    // serves the aligned doubleword.  SETR A 0x2004 ; SETSP A ; SETR B 0xAB ;
    // PUSH B ; POP C ; HALT
    let words = [0x8BD0_0000, 0x2004, 0x6500_0000, 0x8BD0_0100, 0xAB, 0x6400_0010, 0x6480_0200, HALT];
    let cpu = run_words(&words);
    assert_eq!(cpu.regs[2], 0xAB);
    assert_eq!(cpu.sp, 0x2004);
    let mut want = [0; 16];
    want[0] = 0xAB;
    assert_eq!(ram16(&cpu, 0x1FF8), want, "the push wrote 0x1FF8..0x2000");
}

#[test]
fn delay_advances_modelled_time() {
    // DELAYR / DELAYV spin for {value, 13'b0} cycles (pipeline_core.sv:1622),
    // value = rs1[31:0] (1 word) or imm32 (2 words).  SETR B 3 ; DELAYR B ;
    // DELAYV 2 ; HALT: 5 << 13 cycles plus 2 per instruction.
    let words = [0x8BD0_0100, 3, 0x6C05_0010, 0xAC05_0000, 2, HALT];
    let mut cpu = cpu_with(&image_from_words(&words));
    let _ = cpu.run(10, None);
    assert_eq!(cpu.irq_state().cycles, (5 << 13) + 4 * CYCLES_PER_INSTRUCTION);
    // Only rs1's low 32 bits count.
    let words = [0x8BD0_0100, 0, 0xCBC0_0200, 0, 1, 0x6C05_0020, HALT];
    let mut cpu = cpu_with(&image_from_words(&words));
    let _ = cpu.run(10, None);
    assert_eq!(cpu.irq_state().cycles, 4 * CYCLES_PER_INSTRUCTION);
}

#[test]
fn an_illegal_word_does_not_retire() {
    // pipeline_core.sv:1564-1566: an illegal op parks with no trace line,
    // and PERF_INSTR counts retirements only (KlaussCPU.sv:2342-2343).
    // SETR A 1 ; <reserved: ADDR with F = 0>
    let img = image_from_words(&[0x8BD0_0000, 1, 0x4400_0000, HALT]);
    let mut buf = Vec::new();
    let r = emulate_image(&img, CODE_BASE, 100, Some(&mut buf));
    assert_eq!(r.stop, StopReason::InvalidOpcode(0x4400_0000));
    assert_eq!(r.instructions, 1, "only the SETR retired");
    assert_eq!(String::from_utf8(buf).expect("ASCII").lines().count(), 1);
    let mut cpu = cpu_with(&img);
    let _ = cpu.run(100, None);
    assert_eq!(cpu.instret, 1);
    // HALT does retire (and is traced).
    let r = emulate_image(&image_from_words(&[HALT]), CODE_BASE, 100, None);
    assert_eq!((r.stop, r.instructions), (StopReason::Halt, 1));
}
