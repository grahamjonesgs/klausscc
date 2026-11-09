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
    let _ = emulate_image(image, CODE_BASE, 100, false, Some(&mut buf));
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
    // since klaussCPU 04db80e, KlaussCPU.sv START_WAIT), so 'H', the
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
    let r = emulate_image(&img, CODE_BASE, 100, false, None);
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
fn reg_offset_subword_loads_extend_and_stores_truncate() {
    // LDIDXnR / LDIDXnSR / STIDXnR (MODE=11, EA = rs1 + rs2): loads zero- or
    // sign-extend and stores write only the low bytes of rd (pipeline_core.sv
    // :1004, :1101-1113).  The 16/32-bit accesses are aligned (a misaligned
    // one traps, `misaligned_subword_accesses_stop`).  `rrr` is `template |
    // rd<<8 | rs1<<4 | rs2`.
    const fn rrr(template: u32, rd: u32, rs1: u32, rs2: u32) -> u32 {
        template | rd << 8 | rs1 << 4 | rs2
    }
    let (a, b, c) = (0, 1, 2);
    let mut w = Vec::new();
    // [0x1000] = bytes FF EE DD CC 55 66 77 88; A = store data.
    w.extend_from_slice(&setr(b, 0x1000));
    w.extend_from_slice(&[0xCBC0_0B00, 0xCCDD_EEFF, 0x8877_6655]); // SETR64 L
    w.push(0x5F10_0B10); // MEMSET64 L B
    w.extend_from_slice(&[0xCBC0_0000, 0x5566_7788, 0x1122_3344]); // SETR64 A
    w.extend_from_slice(&setr(c, 3));
    w.push(rrr(0x5860_0000, 3, b, c)); // LDIDX8R   D = [B+3]
    w.push(rrr(0x58E0_0000, 4, b, c)); // LDIDX8SR  E = [B+3]
    w.extend_from_slice(&setr(c, 2));
    w.push(rrr(0x5960_0000, 5, b, c)); // LDIDX16R  F = [B+2]
    w.push(rrr(0x59E0_0000, 6, b, c)); // LDIDX16SR G = [B+2]
    w.extend_from_slice(&setr(c, 4));
    w.push(rrr(0x5A60_0000, 7, b, c)); // LDIDX32R  H = [B+4]
    w.push(rrr(0x5AE0_0000, 8, b, c)); // LDIDX32SR I = [B+4]
    w.extend_from_slice(&setr(c, 4));
    w.push(rrr(0x58E0_0000, 12, b, c)); // LDIDX8SR  M = [B+4], a positive byte
    // [0x1008] = all ones, then a byte, a halfword and a word of A over it.
    w.extend_from_slice(&setr(9, 0xFFFF_FFFF)); // J = -1
    w.extend_from_slice(&setr(c, 8));
    w.push(rrr(0x5F60_0000, 9, b, c)); // STIDX64R J -> [B+8]
    w.extend_from_slice(&setr(c, 9));
    w.push(rrr(0x5C60_0000, a, b, c)); // STIDX8R  A -> [B+9]
    w.extend_from_slice(&setr(c, 10));
    w.push(rrr(0x5D60_0000, a, b, c)); // STIDX16R A -> [B+10]
    w.extend_from_slice(&setr(c, 12));
    w.push(rrr(0x5E60_0000, a, b, c)); // STIDX32R A -> [B+12]
    w.extend_from_slice(&setr(c, 8));
    w.push(rrr(0x5B60_0000, 10, b, c)); // LDIDX64R K = [B+8]
    w.push(HALT);
    let cpu = run_words(&w);
    assert_eq!(cpu.stop, Some(StopReason::Halt));
    assert_eq!(cpu.regs[3], 0xCC, "LDIDX8R zero-extends");
    assert_eq!(cpu.regs[4], 0xFFFF_FFFF_FFFF_FFCC, "LDIDX8SR sign-extends");
    assert_eq!(cpu.regs[5], 0xCCDD, "LDIDX16R zero-extends");
    assert_eq!(cpu.regs[6], 0xFFFF_FFFF_FFFF_CCDD, "LDIDX16SR sign-extends");
    assert_eq!(cpu.regs[7], 0x8877_6655, "LDIDX32R zero-extends");
    assert_eq!(cpu.regs[8], 0xFFFF_FFFF_8877_6655, "LDIDX32SR sign-extends");
    assert_eq!(cpu.regs[12], 0x55, "LDIDX8SR keeps a positive byte positive");
    assert_eq!(
        ram16(&cpu, 0x1000),
        [
            0xFF, 0xEE, 0xDD, 0xCC, 0x55, 0x66, 0x77, 0x88, 0xFF, 0x88, 0x88, 0x77, 0x88, 0x77, 0x66, 0x55
        ],
        "each store wrote only its width of A's low bytes"
    );
    assert_eq!(cpu.regs[10], 0x5566_7788_7788_88FF);
}

#[test]
fn absolute_subword_loads_extend_and_stores_truncate() {
    // LDABSn / LDABSnS / STABSn (MODE=10, EA = imm32 in word 1): loads zero-
    // or sign-extend and stores write only the low bytes of rd
    // (pipeline_core.sv:1003, :1101-1113); the 16/32-bit accesses are
    // aligned. `abs` is `[template | rd<<8, addr]`.
    const fn abs(template: u32, rd: u32, addr: u32) -> [u32; 2] {
        [template | rd << 8, addr]
    }
    let (a, b) = (0, 1);
    let mut w = Vec::new();
    // [0x1000] = bytes FF EE DD CC 55 66 77 88; A = store data.
    w.extend_from_slice(&setr(b, 0x1000));
    w.extend_from_slice(&[0xCBC0_0B00, 0xCCDD_EEFF, 0x8877_6655]); // SETR64 L
    w.push(0x5F10_0B10); // MEMSET64 L B
    w.extend_from_slice(&[0xCBC0_0000, 0x5566_7788, 0x1122_3344]); // SETR64 A
    w.extend_from_slice(&abs(0x9840_0000, 3, 0x1003)); // LDABS8   D = [0x1003]
    w.extend_from_slice(&abs(0x98C0_0000, 4, 0x1003)); // LDABS8S  E = [0x1003]
    w.extend_from_slice(&abs(0x9940_0000, 5, 0x1002)); // LDABS16  F = [0x1002]
    w.extend_from_slice(&abs(0x99C0_0000, 6, 0x1002)); // LDABS16S G = [0x1002]
    w.extend_from_slice(&abs(0x9A40_0000, 7, 0x1004)); // LDABS32  H = [0x1004]
    w.extend_from_slice(&abs(0x9AC0_0000, 8, 0x1004)); // LDABS32S I = [0x1004]
    w.extend_from_slice(&abs(0x98C0_0000, 12, 0x1004)); // LDABS8S M, a positive byte
    // [0x1008] = all ones, then a byte, a halfword and a word of A over it.
    w.extend_from_slice(&setr(9, 0xFFFF_FFFF)); // J = -1
    w.extend_from_slice(&abs(0x9F40_0000, 9, 0x1008)); // MEMSETR J -> [0x1008]
    w.extend_from_slice(&abs(0x9C40_0000, a, 0x1009)); // STABS8  A -> [0x1009]
    w.extend_from_slice(&abs(0x9D40_0000, a, 0x100A)); // STABS16 A -> [0x100A]
    w.extend_from_slice(&abs(0x9E40_0000, a, 0x100C)); // STABS32 A -> [0x100C]
    w.extend_from_slice(&abs(0x9B40_0000, 10, 0x1008)); // MEMREADR K = [0x1008]
    w.push(HALT);
    let cpu = run_words(&w);
    assert_eq!(cpu.stop, Some(StopReason::Halt));
    assert_eq!(cpu.regs[3], 0xCC, "LDABS8 zero-extends");
    assert_eq!(cpu.regs[4], 0xFFFF_FFFF_FFFF_FFCC, "LDABS8S sign-extends");
    assert_eq!(cpu.regs[5], 0xCCDD, "LDABS16 zero-extends");
    assert_eq!(cpu.regs[6], 0xFFFF_FFFF_FFFF_CCDD, "LDABS16S sign-extends");
    assert_eq!(cpu.regs[7], 0x8877_6655, "LDABS32 zero-extends");
    assert_eq!(cpu.regs[8], 0xFFFF_FFFF_8877_6655, "LDABS32S sign-extends");
    assert_eq!(cpu.regs[12], 0x55, "LDABS8S keeps a positive byte positive");
    assert_eq!(
        ram16(&cpu, 0x1000),
        [
            0xFF, 0xEE, 0xDD, 0xCC, 0x55, 0x66, 0x77, 0x88, 0xFF, 0x88, 0x88, 0x77, 0x88, 0x77, 0x66, 0x55
        ],
        "each store wrote only its width of A's low bytes"
    );
    assert_eq!(cpu.regs[10], 0x5566_7788_7788_88FF);
}

/// Assemble `source` with the built-in table and macros and run it to its
/// end (at most 10 000 instructions).
fn run_source(source: &str) -> Cpu {
    use crate::asm::assemble_lines;
    use crate::asm::line::SourceLine;
    use crate::asm::macros::{builtin_macros, expand_embedded_macros};
    use crate::diag::Diagnostics;
    use crate::isa::Isa;
    let mut msg_list = Diagnostics::new();
    let macro_list = expand_embedded_macros(builtin_macros(), &mut msg_list);
    let lines = (1..)
        .zip(source.lines())
        .map(|(line_counter, line)| SourceLine {
            file_name: "test.kla".to_owned(),
            input_text_line: line.to_owned(),
            line_counter,
        })
        .collect();
    let assembly = assemble_lines(lines, &Isa::v2(), &macro_list, &mut msg_list);
    assert!(!msg_list.has_errors(), "{:?}", msg_list.messages());
    let image = assembly.image().expect("the source has a _start");
    assert_eq!(image.entry, CODE_BASE);
    let mut cpu = cpu_with(&image.to_ddr());
    cpu.run(10_000, None);
    assert_eq!(cpu.stop, Some(StopReason::Halt));
    cpu
}

#[test]
fn llvm_immediate_forms_extend_as_named() {
    // The opposite-SGN class-2 forms and the boolean compare-immediates, as
    // klausscc assembles them: SETRU zero-extends, ANDI/ORI/XORI and the
    // CMPxxV immediates sign-extend (pipeline_core.sv:399, :761-762).
    let cpu = run_source(
        "_start
SETRU A 0xF0010000
SETRU.S B 200
SETR C 0xFFFFFFFF
ANDI D C -16
ANDI.S E C -8
SETR F 0
ORI F F -129
ORI.S G B -128
XORI H C 0x100
XORI.S I C 127
CMPLTV J C 0
CMPULTV K C 5
CMPUGEV L C -1
HALT",
    );
    let want: [u64; 12] = [
        0xF001_0000,
        200,
        u64::MAX,
        0xFFFF_FFFF_FFFF_FFF0,
        0xFFFF_FFFF_FFFF_FFF8,
        0xFFFF_FFFF_FFFF_FF7F,
        0xFFFF_FFFF_FFFF_FFC8,
        0xFFFF_FFFF_FFFF_FEFF,
        0xFFFF_FFFF_FFFF_FF80,
        1, // -1 < 0
        0, // 2^64-1 is not unsigned-below 5
        1, // the -1 immediate is 2^64-1, so C >= it unsigned
    ];
    assert_eq!(cpu.regs[..12], want);
}

#[test]
fn llvm_w_compares_and_pc_relative_branches() {
    // CMPxxRW / CMPxxVW compare the low 32 bits; CMPRVU zero-extends its
    // immediate; JMPxxREL / CALLREL jump to PC + imm32.
    let cpu = run_source(
        "_start
SETR64 A 0x100000005
SETR B 5
CMPEQRW C A B
CMPEQR D A B
CMPGTVW E A 4
CMPLEVW F A 4
SETRU G 0xFFFFFFFF
SETR H 0
CMPRVU G 0xFFFFFFFF
JMPZREL EQUAL:
SETR H 1
EQUAL:
CALLREL SUB:
JMPREL END:
SETR I 0x99
SUB:
SETR J 0x77
RET
END:
HALT",
    );
    assert_eq!(cpu.regs[2..10], [1, 0, 1, 0, 0xFFFF_FFFF, 0, 0, 0x77]);
    assert_eq!(cpu.sp, TEST_STACK_TOP, "CALLREL's return address was popped");
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
    let r = emulate_image(&img, CODE_BASE, 100, false, Some(&mut sink));
    assert_eq!(sink.writes, 1, "nothing is written after the first error");
    assert_eq!((r.instructions, r.stop), (3, StopReason::Halt));
}

#[test]
fn test_stale_v1_binary_traps() {
    // Any v1 word has bits [31:30] = 00 (LEN=00) -> ERR_INV_OPCODE on the first fetch.
    let img = image_from_words(&[0x0000_0800, 0xFF, 0x0000_F011]);
    let r = emulate_image(&img, CODE_BASE, 100, false, None);
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
        (0xA800_0000 | (3 << 24), "2-word mul/div op 3"),
        (0x6C00_0000 | (8 << 16), "system op 8"),
        (0x7C00_0000, "class F"),
        (0xB400_0000, "2-word fused branch without IMM"),
        (0xF420_0000, "3-word fused branch"),
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
            // PRED << 1 | INV: PRED 1 INV 1, PRED 0, PRED 4 INV 1.
            Operand::Cond => &[0b0011, 0, 0b1001],
            Operand::Rel18 => &[1, 0, 0x3_FFFF],
            Operand::Rel13 => &[1, 0, 0x1FFF],
            Operand::Rel16 => &[0x0_1004, 0, 0xF_F0FF],
            // SRs that exist and MTSR may write (not CYCLE).
            Operand::Sr => &[7, 0, 5],
            Operand::Imm | Operand::Imm64 | Operand::Rel32 => &[0],
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
            // With EVEC set, SYSCALL (and TRAP) enter the handler instead
            // of parking as illegal: only ECAUSE tells them apart.
            cpu.evec = 0x800;
            let stop = cpu.step_one(false);
            assert_ne!(stop, Some(StopReason::InvalidOpcode(word)), "{} as {word:08x}", opcode.name);
            assert_ne!(cpu.ecause, Cause::Illegal as u8, "{} as {word:08x}", opcode.name);
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
        // hw-proposals (klaussCPU master 3508f87 pipeline_core.sv)
        (0x6B90_0000, "MULV.S with [20] set", 730),
        (0x6BC0_0000, "MULV.S with H", 730),
        (0x6B80_0001, "MULV.S with rs2 set", 731),
        (0x4CD0_0000, "CMPRVW.S with PRED 1", 527),
        (0x4C50_0100, "CMPRVW.S with rd set", 527),
        (0x8C50_0000, "2-word flag CMP with INV", 528),
        (0x6C06_0010, "DI with rs1 set", 763),
        (0x6C07_0100, "EI with rd set", 765),
        (0x6C06_1000, "DI with [15:12] set", 759),
        (0x6C07_0001, "EI with rs2 set", 759),
        (0xB420_0001, "2-word fused branch with [3:0] set", 791),
        (0x4BB0_0001, "LEAPC.S with SGN and rs2 set", 481),
        (0xE880_0000, "3-word MUL", 652),
        (0x6C00_0010, "NOP with rs1 set", 675),
        (0xAC01_0000, "2-word HALT", 683),
        (0xAC05_0010, "DELAYV with rs1 set", 683),
        (0x6C80_0000, "system with [25:22] = 2", 672),
        (0x6C0D_0000, "system OP 13", 672),
        (0x6C09_0010, "TLBWI with rs1 set", 675),
        (0xAC0A_0000, "2-word TLBR", 683),
        (0x6C0B_0100, "TLBP with rd set", 672),
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
    let r = emulate_image(&img, CODE_BASE, 100, false, Some(&mut buf));
    assert_eq!(r.stop, StopReason::InvalidOpcode(0x4400_0000));
    assert_eq!(r.instructions, 1, "only the SETR retired");
    assert_eq!(String::from_utf8(buf).expect("ASCII").lines().count(), 1);
    let mut cpu = cpu_with(&img);
    let _ = cpu.run(100, None);
    assert_eq!(cpu.instret, 1);
    // HALT does retire (and is traced).
    let r = emulate_image(&image_from_words(&[HALT]), CODE_BASE, 100, false, None);
    assert_eq!((r.stop, r.instructions), (StopReason::Halt, 1));
}

// ---- hw-proposals (klaussCPU master 3508f87, ISA_ENCODING_V2_MAP §8) -------

/// The value `branch` leaves in D: 2 when it is taken (to SETR D 2), 1 when
/// it falls through (SETR D 1), with A = `a` and B = `b`.  `branch` is the
/// branch's words; its target is 3 words past its end (the taken arm).
fn branch_result(branch: &[u32], a: u64, b: u64) -> u64 {
    let mut cpu = cpu_with(&image_from_words(&[branch, &[0x4BD0_1300, 0x6100_0002, 0x4BD0_2300, HALT]].concat()));
    let mut regs = cpu.debug_regs();
    regs.r[0] = a;
    regs.r[1] = b;
    cpu.set_debug_regs(&regs);
    let _ = cpu.run(10, None);
    assert_eq!(cpu.stop, Some(StopReason::Halt));
    cpu.regs[3]
}

/// A predicate on a fused branch's two operands.
type Holds = fn(u64, u64) -> bool;

/// The low 32 bits of `v` as `i32`.
const fn s32(v: u64) -> i32 {
    v as u32 as i32
}

#[test]
fn fused_branch_w_predicates_compare_the_low_halves() {
    // PRED 5/6/7 = EQW/LTW/ULTW (pipeline_core.sv f_fbr_pred, the id_w
    // shift); INV gives NEW/GEW/UGEW; register and simm4 rhs.  The taken
    // target is 3 words on: disp13 = 3 -> 0x300.
    let vals = [
        0,
        1,
        5,
        0x7FFF_FFFF,
        0x8000_0000,
        0xFFFF_FFFF,
        0x1_0000_0005,
        0xFFFF_FFFF_0000_0005,
        0x8000_0000_0000_0000,
        u64::MAX,
        0x1234_5678_ABCD_EF01,
        (-100_000_i64) as u64,
    ];
    let preds: [(u32, Holds); 3] = [
        (5, |a, b| a as u32 == b as u32),
        (6, |a, b| s32(a) < s32(b)),
        (7, |a, b| (a as u32) < (b as u32)),
    ];
    for (pred, holds) in preds {
        for inv in [false, true] {
            let cond = pred << 23 | u32::from(inv) << 22;
            for (i, &a) in vals.iter().enumerate() {
                let b = vals[(i * 5 + 3) % vals.len()];
                let want = if holds(a, b) ^ inv { 2 } else { 1 };
                assert_eq!(branch_result(&[0x7400_0301 | cond], a, b), want, "PRED {pred} INV {inv} {a:x} {b:x}");
                // A simm4 rhs is compared as a 32-bit value.
                for imm in [-8_i64, -1, 0, 5, 7] {
                    let word = 0x7420_0300 | cond | (imm as u32 & 0xF);
                    let want = if holds(a, imm as u64) ^ inv { 2 } else { 1 };
                    assert_eq!(branch_result(&[word], a, 0), want, "PRED {pred} INV {inv} {a:x} simm4 {imm}");
                }
            }
        }
    }
}

#[test]
fn fused_branch_imm32_compares_with_sign_extended_word_1() {
    // LEN=10, IMM=1, [3:0]=0: rhs = sext(imm32) from word 1, any PRED; the
    // simm13 displacement is from the branch itself; not taken falls
    // through to PC + 8.  The taken target is 3 words past the branch's
    // two (disp13 = 4 -> 0x400).
    let preds: [(u32, Holds); 8] = [
        (0, |a, b| a == b),
        (1, |a, b| (a as i64) < (b as i64)),
        (2, |a, b| (a as i64) <= (b as i64)),
        (3, |a, b| a < b),
        (4, |a, b| a <= b),
        (5, |a, b| a as u32 == b as u32),
        (6, |a, b| s32(a) < s32(b)),
        (7, |a, b| (a as u32) < (b as u32)),
    ];
    for (pred, holds) in preds {
        for inv in [false, true] {
            for imm in [0_i32, 1000, -100_000, i32::MAX, -1] {
                for a in [
                    0,
                    999,
                    1000,
                    (-100_000_i64) as u64,
                    0x7FFF_FFFF,
                    0x8000_0000,
                    u64::MAX,
                    0x0000_0123_FFFF_FFFB,
                ] {
                    let word = 0xB420_0400 | pred << 23 | u32::from(inv) << 22;
                    let rhs = i64::from(imm) as u64;
                    let want = if holds(a, rhs) ^ inv { 2 } else { 1 };
                    assert_eq!(branch_result(&[word, imm as u32], a, 0), want, "PRED {pred} INV {inv} {a:x} imm {imm}");
                }
            }
        }
    }
    // Backwards, and the flags are left alone: A = 3; loop: DECR A;
    // BNEV A 0 -1 word (to the DECR).  The DECR wrote the flags last.
    let cpu = run_words(&[0x4BD0_3000, 0x57C8_0000, 0xB47F_FF00, 0, HALT]);
    assert_eq!(cpu.regs[0], 0);
    assert!(cpu.flags.zero(), "the last DECR's Z");
}

#[test]
fn cmprvw_short_is_the_32_bit_compare() {
    // CMPRVW.S A, simm8 (0x4C50_0000, class 3 short CMP with INV = 1): the
    // flags of CMPRVW (2-word, 0x8C18_0000), a 32-bit subtract (`id_w`).
    for a in [0x1_0000_0005_u64, 0xFFFF_FFFD, 0x7FFF_FFFF, 0x8000_0000, 5, 0xFFFF_FF80] {
        for imm in [-128_i32, -3, 0, 5, 127] {
            let short = exec_one(0x4C50_0000 | (imm as u32 & 0xFF) << 12, a, 0, 0).flags;
            let mut long = cpu_with(&image_from_words(&[0x8C18_0000, imm as u32, HALT]));
            let mut regs = long.debug_regs();
            regs.r[0] = a;
            long.set_debug_regs(&regs);
            let _ = long.run(10, None);
            assert_eq!(short.to_nibble(), long.flags.to_nibble(), "{a:x} {imm}");
            let (diff, borrow) = (a as u32).overflowing_sub(imm as u32);
            assert_eq!(short.zero(), diff == 0, "{a:x} {imm}");
            assert_eq!(short.carry(), borrow, "{a:x} {imm}");
            assert_eq!(short.less(), s32(a) < imm, "{a:x} {imm}");
        }
    }
}

#[test]
fn mulv_short_extends_its_imm8_by_sgn() {
    // MULV.S (SGN = 1) / MULUV.S (SGN = 0): rd = rs1 * ext(imm8), the low
    // half, flags as MUL.  rd = C, rs1 = A.
    let cases: [(u64, i64, bool); 8] = [
        (7, 6, true),
        (7, -6, true),
        ((-3_i64) as u64, 100, true),
        ((1 << 40) + 3, -128, true),
        (5, 0xF0, false),
        (1 << 63, 2, true),
        (0x1_2345_6789, 127, false),
        (u64::MAX, -1, true),
    ];
    for (a, imm, signed) in cases {
        let word = if signed { 0x6B80_0200 } else { 0x6B00_0200 } | (imm as u32 & 0xFF) << 12;
        let cpu = exec_one(word, a, 0, 0);
        let want = a.wrapping_mul(imm as u64);
        assert_eq!(cpu.regs[2], want, "{word:08x} {a:x}");
        assert_eq!(cpu.flags.zero(), want == 0);
        assert_eq!(cpu.flags.sign(), want >> 63 == 1);
        let overflow = if signed {
            (a as i64).checked_mul(imm).is_none()
        } else {
            a.checked_mul(imm as u64).is_none()
        };
        assert_eq!(cpu.flags.overflow(), overflow, "{word:08x} {a:x}");
    }
    // The 2-word unsigned twin, MULUV (0xA800_0000), zero-extends its imm32.
    let cpu = run_words(&[0x4BDF_F000, 0xA800_0200, 0xFFFF_FFFF, HALT]); // A = -1
    assert_eq!(cpu.regs[2], u64::MAX.wrapping_mul(0xFFFF_FFFF));
}

#[test]
fn leapc_short_adds_its_simm16_to_the_pc() {
    // LEAPC.S D, off (0x4BA0_0000): D = PC + sext({[19:12], [7:0]}), a
    // 32-bit address; the instruction is at 0x20.
    for off in [0_i32, 8, 0x7FF0, -0x8000, -4, 0x1234, 0x7FFF] {
        let d = off as u32 & 0xFFFF;
        let cpu = exec_one(0x4BA0_0300 | (d >> 8) << 12 | (d & 0xFF), 0, 0, 0);
        assert_eq!(cpu.regs[3], u64::from(0x20_u32.wrapping_add(off as u32)), "{off}");
    }
    // The RTL also decodes it with SGN [20] set when rs2 [3:0] is zero
    // (its generic short ALU path), with the same offset.
    let cpu = exec_one(0x4BB1_2330, 0, 0, 0);
    assert_eq!(cpu.regs[3], 0x20 + 0x1230);
}

#[test]
fn di_ei_return_and_set_the_enable_bit() {
    // DI D (0x6C06_0300): D = old enable (1 after reset), disable; DI E
    // again: E = 0; EI D (0x6C07_0030): enable = D[0] = 1; DI F: F = 1.
    // EI with an even register disables.
    let cpu = run_words(&[0x6C06_0300, 0x6C06_0400, 0x6C07_0030, 0x6C06_0500, 0x4BD0_2600, 0x6C07_0060, HALT]);
    assert_eq!((cpu.regs[3], cpu.regs[4], cpu.regs[5]), (1, 0, 1));
    assert!(cpu.irq_disabled, "EI with rs1[0] = 0 disables");
    assert!(
        !cpu.flags.zero() && !cpu.flags.carry() && !cpu.flags.sign() && !cpu.flags.overflow(),
        "no flags"
    );
}

/// `timer_program` with `pre` in front of its WAIT (at 0x5C), the rest of
/// the program moved on by its length; the handler sets C = 0x77 and IRETs.
fn timer_program_with(pre: &[u32]) -> Vec<u32> {
    let mut w = timer_program();
    let at = (0x5C - 0x20) / 4;
    let tail = w.split_off(at);
    w.extend_from_slice(pre);
    w.extend(tail);
    // INT_VEC0 (word 1 of the first SETR) follows the handler.
    w[1] += 4 * pre.len() as u32;
    w
}

#[test]
fn di_blocks_interrupts_but_wait_still_wakes() {
    // DI D ; WAIT: the timer wakes the WAIT (WFI) but its handler does not
    // run; the program halts with the interrupt still pending.
    let mut cpu = cpu_with(&image_from_words(&timer_program_with(&[0x6C06_0300])));
    let result = cpu.run(1000, None);
    assert_eq!(result.stop, StopReason::Halt);
    assert_eq!(cpu.regs[3], 0x55, "resumed after the WAIT");
    assert_eq!(cpu.regs[2], 0, "the handler never ran");
    assert_eq!(cpu.irq_state().pending, 1, "the interrupt is still pending");
    // DI D ; EI D ; WAIT: enabled again, the handler runs.
    let mut cpu = cpu_with(&image_from_words(&timer_program_with(&[0x6C06_0300, 0x6C07_0030])));
    let _ = cpu.run(1000, None);
    assert_eq!(cpu.regs[2], 0x77, "the handler ran");
}

#[test]
fn the_irq_frame_saves_and_iret_restores_the_disable_bit() {
    // Taken while enabled, the frame's bit 43 is 0 and IRET leaves the core
    // enabled.
    let mut cpu = cpu_with(&image_from_words(&timer_program()));
    let _ = cpu.run(1000, None);
    let ctx = cpu.peek64(TEST_STACK_TOP - 8);
    assert_eq!((ctx >> 43) & 1, 0, "pushed: enabled");
    assert!(!cpu.irq_disabled);
    // A software-built frame with bit 43 set resumes disabled, and with it
    // clear resumes enabled: PUSHV64 {bit 43, PC 0x38} ; DI D ; IRET to
    // DI E.
    for (bit, disabled) in [(1_u32 << 11, true), (0, false)] {
        let cpu = run_words(&[0xE440_0000, 0x38, bit, 0x6C06_0300, IRET, HALT, 0x6C06_0400, HALT]);
        assert_eq!(cpu.regs[4], u64::from(!disabled), "the enable bit IRET restored");
        assert_eq!(cpu.stop, Some(StopReason::Halt));
    }
}

#[test]
fn misaligned_subword_accesses_stop() {
    // A 16-bit access at an odd address or a 32-bit one not 4-aligned stops
    // with Misaligned(EA) before it happens: no retire, no trace line, the
    // PC on it (pipeline_core.sv ex_misal, park kind 5).  B = 0x1001,
    // C = 0x1002, D = 2.  Every MODE: [rs1], rs1 + imm (2-word and short),
    // [imm], rs1 + rs2.
    let traps: [(&[u32], u32); 16] = [
        (&[0x5900_0010], 0x1001),              // MEMGET16 A [B]
        (&[0x9920_0020, 1], 0x1003),           // LDIDX16 A C 1
        (&[0x99A0_0020, 0xFFFF_FFFF], 0x1001), // LDIDX16_S A C -1
        (&[0x5920_1020], 0x1004),              // LDIDX16.S A C 2: aligned
        (&[0x9A20_0020, 0], 0x1002),           // LDIDX32 A C 0
        (&[0x5A20_1010], 0x1005),              // LDIDX32.S A B 4
        (&[0x9AA0_0010, 3], 0x1004),           // LDIDX32_S A B 3: aligned
        (&[0x9940_0000, 0x2001], 0x2001),      // LDABS16 A 0x2001
        (&[0x9AC0_0000, 0x2006], 0x2006),      // LDABS32S A 0x2006
        (&[0x5960_0013], 0x1003),              // LDIDX16R A B D
        (&[0x5A60_0023], 0x1004),              // LDIDX32R A C D: aligned
        (&[0x5D00_0010], 0x1001),              // MEMSET16 A [B]
        (&[0x5E00_0020], 0x1002),              // MEMSET32 A [C]
        (&[0x9E20_0010, 1], 0x1002),           // STIDX32 A B 1
        (&[0x9D40_0000, 0x3003], 0x3003),      // STABS16 A 0x3003
        (&[0x5E60_0013], 0x1003),              // STIDX32R A B D
    ];
    for (access, ea) in traps {
        let words = [&setr(1, 0x1001)[..], &setr(2, 0x1002), &setr(3, 2), access, &[HALT]].concat();
        let img = image_from_words(&words);
        let mut buf = Vec::new();
        let r = emulate_image(&img, CODE_BASE, 100, false, Some(&mut buf));
        let misaligned = match access[0] >> 24 & 3 {
            1 => ea & 1 != 0,
            _ => ea & 3 != 0,
        };
        if !misaligned {
            assert_eq!(r.stop, StopReason::Halt, "{:08x} at {ea:x}", access[0]);
            continue;
        }
        assert_eq!(r.stop, StopReason::Misaligned(ea), "{:08x}", access[0]);
        assert_eq!(r.instructions, 3, "{:08x}: only the SETRs retired", access[0]);
        assert_eq!(String::from_utf8(buf).expect("ASCII").lines().count(), 3, "{:08x}", access[0]);
        let mut cpu = cpu_with(&img);
        let _ = cpu.run(100, None);
        assert_eq!(cpu.pc, CODE_BASE + 24, "{:08x}: the PC stays on the access", access[0]);
        assert_eq!(cpu.peek64(0x1000), 0, "{:08x}: nothing stored", access[0]);
        assert_eq!(cpu.peek64(0x3000), 0, "{:08x}: nothing stored", access[0]);
    }
}

#[test]
fn memget32_byte_64bit_and_stack_accesses_never_stop_misaligned() {
    // MEMGET32 ([rs1], 32-bit) reads the unaligned word; byte accesses,
    // 64-bit ones (A = 0 uses the aligned doubleword) and the stack ops at an
    // unaligned SP are exempt (hw-proposals 8).
    let mut w = Vec::new();
    w.extend_from_slice(&setr(1, 0x1001)); // B
    w.extend_from_slice(&[0xCBC0_0000, 0x4433_2211, 0x8877_6655]); // SETR64 A
    w.push(0x5F10_0010); // MEMSET64 A [B] -> the doubleword at 0x1000
    w.push(0x5A00_0310); // MEMGET32 D [B]
    w.push(0x5800_0410); // MEMGET8 E [B]
    w.push(0x5C00_0010); // MEMSET8 A [B]
    w.push(0x5B10_0510); // MEMGET64 F [B]
    w.extend_from_slice(&setr(6, 0x2003)); // G
    w.push(0x6500_0060); // SETSP G (unaligned)
    w.push(0x6400_0000); // PUSH A
    w.push(0x6480_0700); // POP H
    w.push(HALT);
    let cpu = run_words(&w);
    assert_eq!(cpu.stop, Some(StopReason::Halt));
    assert_eq!(cpu.regs[3], 0x5544_3322, "MEMGET32 at 0x1001");
    assert_eq!(cpu.regs[4], 0x22, "MEMGET8 at 0x1001");
    assert_eq!(cpu.regs[7], 0x8877_6655_4433_2211, "POP at an unaligned SP");
}

// ---- MMU stage 1 (klaussCPU MMU_PLAN.md §3-§6) ----------------------------

/// `EVEC` for the stage 1 tests: the handler's address.
const HANDLER: u32 = 0x400;
/// The user stack top in the stage 1 tests.
const USER_SP: u32 = 0x8000;
/// The kernel stack top in the stage 1 tests.
const KERNEL_SP: u32 = 0xF000;
/// `SYSCALL`.
const SYSCALL: u32 = 0x6C08_0000;
/// `TRAP`.
const TRAP: u32 = 0x6C04_0000;
/// `NOP`.
const NOP: u32 = 0x6C00_0000;
/// An illegal word (class F).
const ILLEGAL_WORD: u32 = 0x7C00_0000;
/// The SR numbers.
const SR_STATUS: u32 = 0;
const SR_EVEC: u32 = 1;
const SR_ECAUSE: u32 = 2;
const SR_EADDR: u32 = 3;
const SR_USP: u32 = 4;
const SR_SCRATCH0: u32 = 5;
const SR_TP: u32 = 7;
const SR_CYCLE: u32 = 8;

/// `MFSR rd, sr`.
const fn mfsr(rd: u32, sr: u32) -> u32 {
    0x6C40_0000 | (sr << 12) | (rd << 8)
}

/// `MTSR sr, rs1`.
const fn mtsr(sr: u32, rs1: u32) -> u32 {
    0x6C60_0000 | (sr << 12) | (rs1 << 4)
}

/// LR.D / LR.W / LR.W.S `rd, [rs1]` from its template.
const fn lr(template: u32, rd: u32, rs1: u32) -> u32 {
    template | (rd << 8) | (rs1 << 4)
}

/// SC.D / SC.W `rd, rs2, [rs1]` from its template.
const fn sc(template: u32, rd: u32, rs2: u32, rs1: u32) -> u32 {
    template | (rd << 8) | (rs1 << 4) | rs2
}
const LR_D: u32 = 0x5B00_1000;
const LR_W: u32 = 0x5A00_1000;
const LR_W_S: u32 = 0x5A80_1000;
const SC_D: u32 = 0x5F00_1000;
const SC_W: u32 = 0x5E00_1000;

/// A CPU with `code` at the code base and `handler` at [`HANDLER`], `EVEC`
/// set to it, in user mode (`user`) on [`USER_SP`] with the kernel SP in
/// `SP_ALT`, or in supervisor mode on [`KERNEL_SP`].
fn stage1_cpu(code: &[u32], handler: &[u32], user: bool) -> Cpu {
    let mut cpu = cpu_with(&image_from_words(code));
    let bytes: Vec<u8> = handler.iter().flat_map(|w| w.to_le_bytes()).collect();
    assert!(cpu.debug_write(HANDLER, &bytes));
    cpu.evec = HANDLER;
    if user {
        cpu.um = true;
        cpu.sp = USER_SP;
        cpu.sp_alt = KERNEL_SP;
    } else {
        cpu.sp = KERNEL_SP;
        cpu.sp_alt = USER_SP;
    }
    cpu
}

/// Assert `cpu` has just entered the handler for `cause` with `eaddr`, the
/// frame PC `frame_pc`, from user mode if `from_user`: the frame is on the
/// kernel stack, the user SP in `SP_ALT`, supervisor mode, DI set.
fn assert_entered(cpu: &Cpu, cause: Cause, eaddr: u32, frame_pc: u32, from_user: bool) {
    let what = format!("{cause:?} from {}", if from_user { "user" } else { "supervisor" });
    assert_eq!(cpu.stop, None, "{what}");
    assert_eq!(cpu.pc, HANDLER, "{what}: at EVEC");
    assert_eq!((cpu.ecause, cpu.eaddr), (cause as u8, eaddr), "{what}: ECAUSE/EADDR");
    assert!(!cpu.um, "{what}: supervisor mode");
    assert!(cpu.irq_disabled, "{what}: DI set");
    assert_eq!(cpu.sp, KERNEL_SP - 8, "{what}: the frame is on the kernel stack");
    assert_eq!(cpu.sp_alt, USER_SP, "{what}: SP_ALT keeps the user SP");
    let frame = cpu.peek64(KERNEL_SP - 8);
    assert_eq!(frame as u32, frame_pc, "{what}: frame PC");
    assert_eq!((frame >> 44) & 1, u64::from(from_user), "{what}: frame PM");
    assert_eq!((frame >> 43) & 1, 0, "{what}: frame DI (enabled before)");
    assert_eq!(frame >> 45, 0, "{what}: frame [63:45]");
}

#[test]
fn stage1_reset_state_and_cpu_features() {
    let mut cpu = cpu_with(&image_from_words(&[HALT]));
    let s = cpu.system_regs();
    assert_eq!(s, SystemRegs::default(), "supervisor, EVEC 0, no reservation at reset");
    // By default, as on the Nexys A7, there is no MPU.
    assert!(!cpu.has_mpu());
    assert_eq!(
        cpu.mmio_read(0xF00F_0048),
        0x100D,
        "CPU_FEATURES: stages 1 and 3 and LR/SC, 16 entries, no MPU"
    );
    assert_eq!(cpu.mmio_read(0xF00F_0050), 0, "the next offset reads 0");
    cpu.set_has_mpu(true);
    assert_eq!(
        cpu.mmio_read(0xF00F_0048),
        0x10FF,
        "CPU_FEATURES with the MPU: stages 1-3 and LR/SC, 16 regions, 16 entries"
    );
}

#[test]
fn stage1_each_cause_from_supervisor_and_user_mode() {
    // (code, cause, EADDR, frame PC; None = the cause does not happen in
    // that mode).  Registers: B = 0x1001 (odd), C = 0xF001_0000 (UART).
    let setup = [setr(1, 0x1001), setr(2, 0xF001_0000)].concat();
    let at = CODE_BASE + 16; // the instruction after the setup
    let cases: [(u32, Cause, u32, u32, bool); 9] = [
        (ILLEGAL_WORD, Cause::Illegal, ILLEGAL_WORD, at, true),
        (TRAP, Cause::Trap, TRAP, at, true),
        (SYSCALL, Cause::Syscall, 0, at + 4, true),
        (0x5900_0310, Cause::MisalignedLoad, 0x1001, at, true),  // MEMGET16 D [B]
        (0x5E00_0310, Cause::MisalignedStore, 0x1001, at, true), // MEMSET32 D [B]
        (lr(LR_W, 3, 1), Cause::MisalignedLoad, 0x1001, at, true),
        (sc(SC_W, 3, 4, 1), Cause::MisalignedStore, 0x1001, at, true),
        (lr(LR_D, 3, 2), Cause::AccessLoad, 0xF001_0000, at, true), // LR to MMIO: both modes
        (sc(SC_D, 3, 4, 2), Cause::AccessStore, 0xF001_0000, at, true),
    ];
    for (word, cause, eaddr, frame_pc, _) in cases {
        for user in [false, true] {
            let mut code = setup.clone();
            code.extend([word, HALT]);
            let mut cpu = stage1_cpu(&code, &[HALT], false);
            for _ in 0..2 {
                assert_eq!(cpu.step_one(true), None);
            }
            if user {
                // Into user mode with the setup's registers.
                cpu.um = true;
                cpu.sp = USER_SP;
                cpu.sp_alt = KERNEL_SP;
            }
            let regs = cpu.regs;
            assert_eq!(cpu.step_one(true), None, "{word:08x}");
            assert_entered(&cpu, cause, eaddr, frame_pc, user);
            assert_eq!(cpu.regs, regs, "{word:08x}: no register written");
            assert_eq!(cpu.instret, 2, "{word:08x}: the faulting instruction did not retire");
        }
    }
    // The user-mode-only causes: an MMIO load and store, a privileged
    // instruction, and the two fetch faults.
    for (word, cause, eaddr) in [
        (0x5B10_0320, Cause::AccessLoad, 0xF001_0000),  // MEMGET64 D [C]
        (0x5F10_0320, Cause::AccessStore, 0xF001_0000), // MEMSET64 D [C]
        (0x5800_0320, Cause::AccessLoad, 0xF001_0000),  // MEMGET8 D [C]
        (0x6C06_0300, Cause::Privileged, 0x6C06_0300),  // DI D
    ] {
        let mut code = setup.clone();
        code.extend([word, HALT]);
        let mut cpu = stage1_cpu(&code, &[HALT], true);
        for _ in 0..3 {
            assert_eq!(cpu.step_one(true), None);
        }
        assert_entered(&cpu, cause, eaddr, at, true);
        // The same word in supervisor mode just runs.
        let mut cpu = stage1_cpu(&code, &[HALT], false);
        for _ in 0..3 {
            assert_eq!(cpu.step_one(true), None, "{word:08x}");
        }
        assert_eq!(cpu.pc, at + 4, "{word:08x} runs in supervisor mode");
    }
    let mut cpu = stage1_cpu(&[NOP], &[HALT], true);
    cpu.pc = CODE_BASE + 2;
    assert_eq!(cpu.step_one(true), None);
    assert_entered(&cpu, Cause::MisalignedFetch, CODE_BASE + 2, CODE_BASE + 2, true);
    let mut cpu = stage1_cpu(&[NOP], &[HALT], true);
    cpu.pc = 0xF000_0000;
    assert_eq!(cpu.step_one(true), None);
    assert_entered(&cpu, Cause::AccessFetch, 0xF000_0000, 0xF000_0000, true);
    // In supervisor mode an MMIO PC is outside RAM, as before stage 1.
    let mut cpu = stage1_cpu(&[NOP], &[HALT], false);
    cpu.pc = 0xF000_0000;
    assert_eq!(cpu.step_one(true), Some(StopReason::PcOutOfRange(0xF000_0000)));
}

#[test]
fn stage1_every_privileged_instruction_traps_in_user_mode() {
    let words: [&[u32]; 13] = [
        &[0x6C06_0300],        // DI D
        &[0x6C07_0030],        // EI D
        &[IRET],               // IRET
        &[HALT],               // HALT
        &[WAIT],               // WAIT
        &[0x6C03_0000],        // RESET
        &[0x6C05_0010],        // DELAYR B
        &[0xAC05_0000, 1],     // DELAYV 1
        &[0x7000_0010],        // LCDCMDR B
        &[0xB100_0000, 0x41],  // LCDDATAV 'A'
        &[0xB200_0000, 1],     // LCDRST 1
        &[mfsr(3, SR_EVEC)],   // MFSR D EVEC
        &[mtsr(SR_STATUS, 3)], // MTSR STATUS D
    ];
    for code in words {
        let mut cpu = stage1_cpu(code, &[HALT], true);
        let before = cpu.cycles;
        assert_eq!(cpu.step_one(true), None, "{:08x}", code[0]);
        assert_entered(&cpu, Cause::Privileged, code[0], CODE_BASE, true);
        assert!(!cpu.waiting, "{:08x}", code[0]);
        assert_eq!(cpu.cycles, before + CYCLES_PER_INSTRUCTION, "{:08x}: no DELAY", code[0]);
    }
    // ILLEGAL wins over PRIVILEGED: a DI with rs1 set is illegal in either mode.
    let mut cpu = stage1_cpu(&[0x6C06_0310], &[HALT], true);
    assert_eq!(cpu.step_one(true), None);
    assert_entered(&cpu, Cause::Illegal, 0x6C06_0310, CODE_BASE, true);
}

#[test]
fn stage1_user_mode_sr_allow_list() {
    // MFSR STATUS / TP / CYCLE and MTSR TP run in user mode.
    let code = [
        setr(1, 0x1234).as_slice(),
        &[mtsr(SR_TP, 1), mfsr(2, SR_TP), mfsr(3, SR_STATUS), mfsr(4, SR_CYCLE), mfsr(5, SR_CYCLE)],
    ]
    .concat();
    let mut cpu = stage1_cpu(&code, &[HALT], true);
    for _ in 0..6 {
        assert_eq!(cpu.step_one(true), None);
    }
    assert!(cpu.um);
    assert_eq!((cpu.tp, cpu.regs[2]), (0x1234, 0x1234), "TP written and read back");
    assert_eq!(cpu.regs[3], 0b01, "STATUS: UM set, DI clear");
    assert_eq!(cpu.regs[5] - cpu.regs[4], CYCLES_PER_INSTRUCTION, "CYCLE counts");
    // The others are PRIVILEGED in user mode and run in supervisor mode.
    for sr in [
        SR_EVEC,
        SR_ECAUSE,
        SR_EADDR,
        SR_USP,
        SR_SCRATCH0,
        6,
        0x10,
        0x2F,
        0x30,
        0x31,
        0x32,
        0x33,
        0x34,
    ] {
        for word in [mfsr(3, sr), mtsr(sr, 1)] {
            // With the MPU, so that its SRs (0x10-0x2F) exist.
            let mut cpu = stage1_cpu(&[word], &[HALT], true);
            cpu.set_has_mpu(true);
            assert_eq!(cpu.step_one(true), None);
            assert_entered(&cpu, Cause::Privileged, word, CODE_BASE, true);
            let mut cpu = stage1_cpu(&[word], &[HALT], false);
            cpu.set_has_mpu(true);
            assert_eq!(cpu.step_one(true), None);
            assert_eq!(cpu.pc, CODE_BASE + 4, "{word:08x} in supervisor mode");
        }
    }
    // An SR that does not exist, and MTSR to CYCLE, are ILLEGAL in both
    // modes; so are the reserved fields.
    for word in [
        mfsr(3, 9),
        mtsr(9, 1),
        mfsr(3, 0x0F),
        mtsr(0x35, 1),
        mfsr(3, 0xFF),
        mtsr(SR_CYCLE, 1),
        mfsr(3, SR_TP) | 1,
        mtsr(SR_TP, 1) | 0x100,
        mfsr(3, 0) | 1 << 20,
    ] {
        for user in [false, true] {
            let mut cpu = stage1_cpu(&[word], &[HALT], user);
            assert_eq!(cpu.step_one(true), None, "{word:08x}");
            assert_eq!((cpu.ecause, cpu.eaddr), (Cause::Illegal as u8, word), "{word:08x}");
        }
    }
}

#[test]
fn stage1_supervisor_sr_reads_and_writes() {
    let mut cpu = stage1_cpu(&[HALT], &[HALT], false);
    for (sr, value, back) in [
        (SR_EVEC, 0x1237, 0x1234),
        (SR_ECAUSE, 0x3FF, 0x1FF),
        (SR_EADDR, 0x1_2345_6789, 0x2345_6789),
        (SR_USP, 0x1_0000_7000, 0x7000),
        (SR_SCRATCH0, u64::MAX, u64::MAX),
        (6, 0x55, 0x55),
        (SR_TP, 1 << 63, 1 << 63),
        (SR_STATUS, 3, 0),
    ] {
        assert!(cpu.sr_write(sr, value), "SR {sr}");
        assert_eq!(cpu.sr_read(sr), Some(back), "SR {sr}");
    }
    assert!(!cpu.sr_write(SR_CYCLE, 0));
    assert_eq!(cpu.sr_read(9), None);
    cpu.irq_disabled = true;
    assert_eq!(cpu.sr_read(SR_STATUS), Some(0b10), "STATUS DI");
}

#[test]
fn stage1_faults_on_multi_side_effect_instructions_change_nothing_and_retry() {
    // Each instruction faults in user mode on the UART (0xF001_0000:
    // TX_DATA, +8 RX_DATA); the handler fixes up the user state and IRETs,
    // and the retry runs.  (code, set up the fault, EADDR, cause, fix up)
    type Fix = fn(&mut Cpu);
    type Case = (&'static str, Vec<u32>, Fix, u32, Cause, Fix);
    let cases: [Case; 9] = [
        (
            "PUSH",
            vec![0x6400_0010],
            |c| c.sp = 0xF001_0008,
            0xF001_0000,
            Cause::AccessStore,
            |c| c.sp_alt = USER_SP,
        ),
        (
            "PUSHV",
            vec![0xA440_0000, 0x55],
            |c| c.sp = 0xF001_0008,
            0xF001_0000,
            Cause::AccessStore,
            |c| c.sp_alt = USER_SP,
        ),
        (
            "POP",
            vec![0x6480_0300],
            |c| c.sp = 0xF001_0008,
            0xF001_0008,
            Cause::AccessLoad,
            |c| c.sp_alt = USER_SP - 8,
        ),
        (
            "CALL",
            vec![0xA200_0000, 0x200],
            |c| c.sp = 0xF001_0008,
            0xF001_0000,
            Cause::AccessStore,
            |c| c.sp_alt = USER_SP,
        ),
        (
            "RET",
            vec![0x6580_0000],
            |c| c.sp = 0xF001_0008,
            0xF001_0008,
            Cause::AccessLoad,
            |c| c.sp_alt = USER_SP - 8,
        ),
        (
            "ENTER",
            vec![0x6600_0002],
            |c| c.sp = 0xF001_0008,
            0xF001_0000,
            Cause::AccessStore,
            |c| c.sp_alt = USER_SP,
        ),
        (
            "LEAVE",
            vec![0x6640_0000],
            |c| c.regs[15] = 0xF001_0008,
            0xF001_0008,
            Cause::AccessLoad,
            |c| c.regs[15] = 0x3000,
        ),
        // Both loads are checked first: the frame pointer is RAM-side and
        // the return address in MMIO.
        (
            "LEAVERET",
            vec![0x6680_0000],
            |c| c.regs[15] = 0xEFFF_FFF8,
            0xF000_0000,
            Cause::AccessLoad,
            |c| c.regs[15] = 0x3000,
        ),
        // MEMGET32 spanning RAM-side and MMIO doublewords: the second doubleword.
        (
            "MEMGET32",
            vec![0x5A00_0310],
            |c| c.regs[1] = 0xEFFF_FFFE,
            0xF000_0000,
            Cause::AccessLoad,
            |c| c.regs[1] = 0x3000,
        ),
    ];
    for (name, code, set_up, eaddr, cause, fix) in cases {
        let mut code = code;
        code.push(HALT);
        let mut cpu = stage1_cpu(&code, &[IRET], true);
        cpu.regs[1] = 0x1111;
        cpu.regs[3] = 0x3333;
        cpu.feed_uart_rx(b"x");
        // A return address and saved R15 for RET / LEAVE / LEAVERET's retry.
        assert!(cpu.debug_write(0x3000, &0x3100_u64.to_le_bytes()));
        assert!(cpu.debug_write(0x3008, &u64::from(CODE_BASE + 0x40).to_le_bytes()));
        assert!(cpu.debug_write(USER_SP - 8, &u64::from(CODE_BASE + 0x40).to_le_bytes()));
        set_up(&mut cpu);
        let (regs, user_sp, flags) = (cpu.regs, cpu.sp, cpu.flags);
        assert_eq!(cpu.step_one(true), None, "{name}");
        assert_eq!((cpu.ecause, cpu.eaddr, cpu.pc), (cause as u8, eaddr, HANDLER), "{name}");
        assert_eq!((cpu.regs, cpu.sp_alt, cpu.flags), (regs, user_sp, flags), "{name}: nothing changed");
        assert_eq!(cpu.sp, KERNEL_SP - 8, "{name}");
        assert!(cpu.take_uart().is_empty(), "{name}: no UART store");
        assert_eq!(cpu.uart_rx.len(), 1, "{name}: no UART load");
        // The handler fixes the user state up and returns to the instruction.
        fix(&mut cpu);
        assert_eq!(cpu.step_one(true), None, "{name}: IRET");
        assert!(cpu.um, "{name}: back in user mode");
        assert_eq!(
            (cpu.pc, cpu.sp_alt),
            (CODE_BASE, KERNEL_SP),
            "{name}: at the instruction, kernel SP restored"
        );
        assert_eq!(cpu.step_one(true), None, "{name}: retry");
        assert_ne!(cpu.pc, HANDLER, "{name}: the retry ran");
        assert_eq!(cpu.ecause, cause as u8, "{name}: no second fault");
    }
}

#[test]
fn stage1_syscall_round_trip_to_user_mode() {
    // The kernel enters user mode with a frame of PM = 1, the user SP in
    // USP, and IRET; the user SYSCALLs; the handler reads ECAUSE, adds 1 to
    // A and IRETs back to the instruction after the SYSCALL.
    let user = CODE_BASE + 0x100;
    let frame = (1_u64 << 44) | u64::from(user);
    let mut kernel = Vec::new();
    kernel.extend(setr(1, HANDLER)); // B = handler
    kernel.push(mtsr(SR_EVEC, 1));
    kernel.extend(setr(1, USER_SP));
    kernel.push(mtsr(SR_USP, 1));
    kernel.extend([0xE440_0000, frame as u32, (frame >> 32) as u32]); // PUSHV64 frame
    kernel.push(IRET);
    let mut code = kernel.clone();
    code.resize(0x40, NOP);
    code.extend(setr(0, 41)); // user: A = 41
    code.push(SYSCALL);
    code.extend(setr(2, 0x77)); // C = 0x77
    code.push(0x6100_0000); // JMP.S . (spin)
    let handler = [mfsr(3, SR_ECAUSE), 0x8820_0000, 1, IRET]; // D = ECAUSE; ADDV A 1
    let mut cpu = stage1_cpu(&code, &handler, false);
    cpu.evec = 0;
    for _ in 0..6 {
        assert_eq!(cpu.step_one(true), None);
    }
    assert!(cpu.um, "IRET with PM = 1 entered user mode");
    assert_eq!((cpu.pc, cpu.sp, cpu.sp_alt), (user, USER_SP, KERNEL_SP), "on the user stack");
    assert_eq!(cpu.step_one(true), None); // SETR A
    assert_eq!(cpu.step_one(true), None); // SYSCALL
    assert_entered(&cpu, Cause::Syscall, 0, user + 12, true);
    for _ in 0..3 {
        assert_eq!(cpu.step_one(true), None);
    }
    assert!(cpu.um);
    assert!(!cpu.irq_disabled, "IRET restored DI");
    assert_eq!((cpu.pc, cpu.sp, cpu.sp_alt), (user + 12, USER_SP, KERNEL_SP));
    assert_eq!((cpu.regs[0], cpu.regs[3]), (42, u64::from(Cause::Syscall as u8)));
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(cpu.regs[2], 0x77, "the instruction after the SYSCALL ran");
}

#[test]
fn stage1_interrupt_in_user_mode_switches_stacks() {
    // The timer fires while user code spins; the entry pushes the frame on
    // the kernel stack with PM = 1 and leaves DI and ECAUSE alone; IRET
    // returns to user mode on the user stack.
    let mut cpu = stage1_cpu(&[0x6100_0000], &[IRET], true); // JMP.S .
    cpu.int_vec[0] = HANDLER;
    cpu.int_mask = 1;
    cpu.timer_period = 10;
    cpu.ecause = 0x42;
    cpu.reservation = Some(0x100);
    while cpu.pc != HANDLER {
        assert_eq!(cpu.step_one(true), None);
    }
    assert!(!cpu.um);
    assert!(!cpu.irq_disabled, "an interrupt leaves DI alone");
    assert_eq!(cpu.ecause, 0x42, "and ECAUSE");
    assert_eq!(cpu.reservation, None, "entry clears the reservation");
    assert_eq!((cpu.sp, cpu.sp_alt), (KERNEL_SP - 8, USER_SP));
    let frame = cpu.peek64(KERNEL_SP - 8);
    assert_eq!((frame as u32, (frame >> 44) & 1, (frame >> 39) & 0xF), (CODE_BASE, 1, 1));
    assert_eq!(cpu.step_one(true), None); // IRET
    assert!(cpu.um);
    assert_eq!((cpu.pc, cpu.sp, cpu.sp_alt), (CODE_BASE, USER_SP, KERNEL_SP));
    // From supervisor mode nothing changes: same stack, PM = 0.
    let mut cpu = stage1_cpu(&[0x6100_0000], &[IRET], false);
    cpu.int_vec[0] = HANDLER;
    cpu.int_mask = 1;
    cpu.timer_period = 10;
    while cpu.pc != HANDLER {
        assert_eq!(cpu.step_one(true), None);
    }
    assert_eq!((cpu.sp, cpu.sp_alt), (KERNEL_SP - 8, USER_SP));
    assert_eq!((cpu.peek64(KERNEL_SP - 8) >> 44) & 1, 0);
    assert_eq!(cpu.step_one(true), None);
    assert!(!cpu.um);
    assert_eq!(cpu.sp, KERNEL_SP);
}

#[test]
fn stage1_evec_zero_parks_as_before() {
    // With EVEC = 0 every cause stops the CPU as before stage 1, with the
    // PC on the instruction and nothing entered: ILLEGAL and the new
    // causes as an illegal instruction, TRAP as a TRAP, a misaligned load
    // or store as misaligned.
    let setup = [setr(1, 0x1001), setr(2, 0xF001_0000)].concat();
    for (word, user, stop) in [
        (ILLEGAL_WORD, true, StopReason::InvalidOpcode(ILLEGAL_WORD)),
        (TRAP, true, StopReason::Trap),
        (SYSCALL, false, StopReason::InvalidOpcode(SYSCALL)),
        (SYSCALL, true, StopReason::InvalidOpcode(SYSCALL)),
        (0x5900_0310, true, StopReason::Misaligned(0x1001)),
        (0x6C06_0300, true, StopReason::InvalidOpcode(0x6C06_0300)), // DI: PRIVILEGED
        (0x5B10_0320, true, StopReason::InvalidOpcode(0x5B10_0320)), // MMIO load
        (lr(LR_D, 3, 2), false, StopReason::InvalidOpcode(lr(LR_D, 3, 2))),
    ] {
        let mut code = setup.clone();
        code.extend([word, HALT]);
        let mut cpu = stage1_cpu(&code, &[HALT], false);
        cpu.evec = 0;
        for _ in 0..2 {
            assert_eq!(cpu.step_one(true), None);
        }
        cpu.um = user;
        let sp = cpu.sp;
        assert_eq!(cpu.step_one(true), Some(stop), "{word:08x}");
        assert_eq!(
            (cpu.pc, cpu.sp, cpu.um, cpu.ecause),
            (CODE_BASE + 16, sp, user, 0),
            "{word:08x}: nothing entered"
        );
        assert!(cpu.parked_exception().is_some());
    }
    // A user-mode fetch fault too.
    let mut cpu = stage1_cpu(&[NOP], &[HALT], true);
    cpu.evec = 0;
    cpu.pc = CODE_BASE + 2;
    assert!(matches!(cpu.step_one(true), Some(StopReason::InvalidOpcode(_))));
    assert_eq!(cpu.parked_exception(), Some((Cause::MisalignedFetch, CODE_BASE + 2)));
}

#[test]
fn stage1_debugger_halts_on_illegal_trap_misaligned_unless_exc_pass() {
    let setup = [setr(1, 0x1001)].concat();
    for (word, stop, cause) in [
        (ILLEGAL_WORD, StopReason::InvalidOpcode(ILLEGAL_WORD), Cause::Illegal),
        (TRAP, StopReason::Trap, Cause::Trap),
        (0x5900_0310, StopReason::Misaligned(0x1001), Cause::MisalignedLoad),
    ] {
        let mut code = setup.clone();
        code.extend([word, HALT]);
        let mut cpu = stage1_cpu(&code, &[HALT], false);
        cpu.set_debugger_attached(true);
        assert_eq!(cpu.step_one(false), None);
        assert_eq!(cpu.step_one(false), Some(stop), "{word:08x}: debug halt with EVEC set");
        assert_eq!(cpu.pc, CODE_BASE + 8);
        // EXC_PASS: the program's handler takes it.
        cpu.clear_stop();
        cpu.set_exc_pass(true);
        assert!(cpu.exc_pass());
        assert_eq!(cpu.step_one(false), None);
        assert_eq!((cpu.pc, cpu.ecause), (HANDLER, cause as u8), "{word:08x}");
    }
    // SYSCALL and the other new causes are never debug halts.
    let mut cpu = stage1_cpu(&[SYSCALL], &[HALT], false);
    cpu.set_debugger_attached(true);
    assert_eq!(cpu.step_one(false), None);
    assert_eq!(cpu.pc, HANDLER);
}

#[test]
fn stage1_faulting_instruction_retires_no_trace_line() {
    // SETR B handler ; MTSR EVEC B ; SYSCALL -> handler: HALT.
    let mut code = setr(1, HANDLER).to_vec();
    code.extend([mtsr(SR_EVEC, 1), SYSCALL]);
    let mut img = image_from_words(&code);
    img.resize(HANDLER as usize + 4, 0);
    img[HANDLER as usize..].copy_from_slice(&HALT.to_le_bytes());
    let trace = trace_of(&img);
    let ops: Vec<&str> = trace.lines().map(|l| l.split(' ').nth(2).unwrap()).collect();
    assert_eq!(ops, ["op=8bd00100", "op=6c601010", "op=6c010000"], "no line for the SYSCALL");
    let r = emulate_image(&img, CODE_BASE, 100, false, None);
    assert_eq!((r.stop, r.instructions), (StopReason::Halt, 3));
}

#[test]
fn stage1_a_handler_that_faults_forever_hits_the_cap() {
    // EVEC points at an illegal word: each entry faults again.
    let mut cpu = stage1_cpu(&[ILLEGAL_WORD], &[ILLEGAL_WORD], false);
    let r = cpu.run(50, None);
    assert_eq!((r.stop, r.instructions), (StopReason::InstructionCap, 0));
    assert_eq!(cpu.exc_entries, 50);
}

/// Run the one instruction `word` in supervisor mode with `regs` and the
/// reservation `res`, returning the CPU after it.
fn lr_sc_one(word: u32, regs: &[(usize, u64)], reserved: Option<u32>) -> Cpu {
    let mut cpu = stage1_cpu(&[word, HALT], &[HALT], false);
    for &(r, v) in regs {
        cpu.regs[r] = v;
    }
    cpu.reservation = reserved;
    assert_eq!(cpu.step_one(true), None, "{word:08x}");
    cpu
}

#[test]
fn stage1_lr_sc() {
    // LR.D reserves; SC.D to the same doubleword stores rs2, rd = 0, and
    // clears the reservation; ordinary stores leave it.
    let code = [
        lr(LR_D, 3, 1),    // D = [B]
        0x5F10_0420,       // MEMSET64 E [C]: an ordinary store elsewhere
        sc(SC_D, 6, 5, 1), // G = SC.D F [B]
        sc(SC_D, 7, 5, 1), // H = SC.D F [B] again: fails
        HALT,
    ];
    let mut cpu = stage1_cpu(&code, &[HALT], true);
    cpu.regs[1] = 0x2000;
    cpu.regs[2] = 0x2100;
    cpu.regs[5] = 0xAAAA_BBBB_CCCC_DDDD;
    assert!(cpu.debug_write(0x2000, &0x1234_u64.to_le_bytes()));
    for _ in 0..4 {
        assert_eq!(cpu.step_one(true), None);
    }
    assert_eq!(cpu.regs[3], 0x1234, "LR.D loaded");
    assert_eq!(cpu.regs[6], 0, "SC.D succeeded after an ordinary store");
    assert_eq!(cpu.peek64(0x2000), 0xAAAA_BBBB_CCCC_DDDD);
    assert_eq!(cpu.regs[7], 1, "a second SC fails: the first cleared the reservation");
    assert_eq!(cpu.reservation, None);

    // LR.W zero-extends, LR.W.S sign-extends; both reserve the doubleword.
    let mut cpu = stage1_cpu(&[lr(LR_W, 3, 1), lr(LR_W_S, 4, 1)], &[HALT], false);
    cpu.regs[1] = 0x2004;
    assert!(cpu.debug_write(0x2004, &0x8000_0001_u32.to_le_bytes()));
    for _ in 0..2 {
        assert_eq!(cpu.step_one(true), None);
    }
    assert_eq!((cpu.regs[3], cpu.regs[4]), (0x8000_0001, 0xFFFF_FFFF_8000_0001));
    assert_eq!(cpu.reservation, Some(0x2000 >> 3));

    // SC.W stores the low word; to the other half of the reserved
    // doubleword it succeeds too.
    let cpu = lr_sc_one(sc(SC_W, 6, 5, 1), &[(1, 0x2004), (5, 0x1_2345_6789)], Some(0x2000 >> 3));
    assert_eq!((cpu.regs[6], cpu.peek64(0x2000)), (0, 0x2345_6789_0000_0000));
    // A different doubleword fails, stores nothing, and clears the reservation.
    let cpu = lr_sc_one(sc(SC_D, 6, 5, 1), &[(1, 0x2008), (5, 7)], Some(0x2000 >> 3));
    assert_eq!((cpu.regs[6], cpu.peek64(0x2008), cpu.reservation), (1, 0, None));
    // No reservation: fails.
    let cpu = lr_sc_one(sc(SC_D, 6, 5, 1), &[(1, 0x2000), (5, 7), (6, 9)], None);
    assert_eq!((cpu.regs[6], cpu.peek64(0x2000)), (1, 0));
    // rd may be rs2: the data is read first.
    let cpu = lr_sc_one(sc(SC_D, 5, 5, 1), &[(1, 0x2000), (5, 7)], Some(0x2000 >> 3));
    assert_eq!((cpu.regs[5], cpu.peek64(0x2000)), (0, 7));
}

#[test]
fn stage1_lr_sc_faults_change_nothing_and_entry_clears_the_reservation() {
    // Misaligned (64-bit too, unlike ordinary 64-bit accesses) and MMIO, in
    // supervisor mode: nothing stored, rd kept; the exception entry clears
    // the reservation, so the restarted SC fails (MMU_PLAN §4.2, e4aa44b).
    for (word, addr, cause) in [
        (lr(LR_D, 6, 1), 0x2004, Cause::MisalignedLoad),
        (lr(LR_W, 6, 1), 0x2002, Cause::MisalignedLoad),
        (sc(SC_D, 6, 5, 1), 0x2004, Cause::MisalignedStore),
        (sc(SC_W, 6, 5, 1), 0x2001, Cause::MisalignedStore),
        (lr(LR_W, 6, 1), 0xF001_0008, Cause::AccessLoad),
        (sc(SC_W, 6, 5, 1), 0xF001_0000, Cause::AccessStore),
    ] {
        let mut cpu = lr_sc_one(word, &[(1, u64::from(addr)), (5, 0x55), (6, 0x66)], Some(addr >> 3));
        assert_entered(&cpu, cause, addr, CODE_BASE, false);
        assert_eq!(cpu.reservation, None, "{word:08x}: the entry clears it");
        assert_eq!(cpu.regs[6], 0x66, "{word:08x}: rd kept");
        assert_eq!(cpu.take_uart(), [] as [u8; 0]);
    }
    // The fault itself leaves the reservation: with EVEC = 0 (no entry to
    // clear it) a faulting SC parks with it intact.
    let mut cpu = stage1_cpu(&[sc(SC_D, 6, 5, 1)], &[HALT], false);
    cpu.evec = 0;
    cpu.regs[1] = 0x2004;
    cpu.reservation = Some(0x2000 >> 3);
    assert_eq!(cpu.step_one(true), Some(StopReason::Misaligned(0x2004)));
    assert_eq!(cpu.reservation, Some(0x2000 >> 3));
    // Reserved encodings: SGN on LR.D / SC, A, [19:13], an 8/16-bit size,
    // rs2 on LR.
    for word in [
        LR_D | 1 << 23,
        SC_D | 1 << 23,
        LR_W | 1 << 20,
        LR_W | 1 << 13,
        0x5800_1000,
        0x5D00_1000,
        LR_D | 1,
    ] {
        let mut cpu = stage1_cpu(&[word], &[HALT], false);
        cpu.evec = 0;
        assert_eq!(cpu.step_one(true), Some(StopReason::InvalidOpcode(word)), "{word:08x}");
    }
}

#[test]
fn stage1_sc_fails_after_an_interrupt_or_iret() {
    // An interrupt between LR and SC clears the reservation.
    let mut cpu = stage1_cpu(&[lr(LR_D, 3, 1), sc(SC_D, 6, 5, 1), HALT], &[IRET], true);
    cpu.regs[1] = 0x2000;
    assert_eq!(cpu.step_one(true), None);
    assert!(cpu.reservation.is_some());
    cpu.int_vec[0] = HANDLER;
    cpu.int_mask = 1;
    cpu.timer_pending = true;
    assert_eq!(cpu.step_one(true), None); // entry
    assert_eq!(cpu.step_one(true), None); // IRET
    assert_eq!(cpu.step_one(true), None); // SC.D
    assert_eq!((cpu.regs[6], cpu.peek64(0x2000)), (1, 0), "SC failed");
    // IRET alone clears it too.
    let frame = u64::from(CODE_BASE + 8);
    let mut cpu = stage1_cpu(&[lr(LR_D, 3, 1), IRET, sc(SC_D, 6, 5, 1)], &[HALT], false);
    cpu.regs[1] = 0x2000;
    cpu.sp = KERNEL_SP - 8;
    assert!(cpu.debug_write(KERNEL_SP - 8, &frame.to_le_bytes()));
    for _ in 0..3 {
        assert_eq!(cpu.step_one(true), None);
    }
    assert_eq!(cpu.regs[6], 1, "SC after IRET failed");
}

#[test]
fn stage1_cause_priority() {
    // MMU_PLAN §4.1: MISALIGNED_FETCH > ACCESS_FETCH > ILLEGAL > PRIVILEGED >
    // TRAP/SYSCALL > MISALIGNED_LOAD/STORE > ACCESS_LOAD/STORE.
    // A misaligned PC in MMIO is MISALIGNED_FETCH.
    let mut cpu = stage1_cpu(&[NOP], &[HALT], true);
    cpu.pc = 0xF000_0002;
    assert_eq!(cpu.step_one(true), None);
    assert_entered(&cpu, Cause::MisalignedFetch, 0xF000_0002, 0xF000_0002, true);
    // Whatever word sits in MMIO (illegal, privileged, a TRAP), fetching it
    // in user mode is ACCESS_FETCH: the word is never decoded.
    let mut cpu = stage1_cpu(&[NOP], &[HALT], true);
    cpu.pc = 0xF001_0000;
    assert_eq!(cpu.step_one(true), None);
    assert_entered(&cpu, Cause::AccessFetch, 0xF001_0000, 0xF001_0000, true);
    // (word, cause, EADDR) in user mode with B = 0xF001_0001 (MMIO, odd)
    // and C = 0xF001_0000.
    for (word, cause, eaddr) in [
        // ILLEGAL > PRIVILEGED: an illegal DI / MTSR form.
        (0x6C06_0310, Cause::Illegal, 0x6C06_0310),
        (mtsr(SR_CYCLE, 1), Cause::Illegal, mtsr(SR_CYCLE, 1)),
        (mfsr(3, 9), Cause::Illegal, mfsr(3, 9)),
        // ILLEGAL > ACCESS: an illegal load (MEMGET32 with SGN) from MMIO.
        (0x5A80_0320, Cause::Illegal, 0x5A80_0320),
        // MISALIGNED > ACCESS: a misaligned user load / store to MMIO, and
        // LR/SC (alignment before MMIO).
        (0x5900_0310, Cause::MisalignedLoad, 0xF001_0001),
        (0x5D00_0310, Cause::MisalignedStore, 0xF001_0001),
        (lr(LR_W, 3, 1), Cause::MisalignedLoad, 0xF001_0001),
        (sc(SC_D, 3, 4, 1), Cause::MisalignedStore, 0xF001_0001),
    ] {
        let mut cpu = stage1_cpu(&[word], &[HALT], true);
        cpu.regs[1] = 0xF001_0001;
        cpu.regs[2] = 0xF001_0000;
        assert_eq!(cpu.step_one(true), None, "{word:08x}");
        assert_entered(&cpu, cause, eaddr, CODE_BASE, true);
    }
}

#[test]
fn stage1_eaddr_is_the_raw_effective_address() {
    // MMU_PLAN §4.1: before any alignment, for A=1 64-bit accesses and
    // sub-word stores too; a two-access instruction whose second doubleword
    // alone faults reports that doubleword's start.
    for (code, b, eaddr, cause) in [
        (vec![0x5B10_0310], 0xF001_0003, 0xF001_0003, Cause::AccessLoad), // MEMGET64 D [B] (A=1)
        (vec![0x5F10_0310], 0xF001_0005, 0xF001_0005, Cause::AccessStore), // MEMSET64 D [B] (A=1)
        (vec![0x5B00_0310], 0xF001_0006, 0xF001_0006, Cause::AccessLoad), // MEMREADRR (A=0)
        (vec![0x5C00_0310], 0xF001_0005, 0xF001_0005, Cause::AccessStore), // MEMSET8 D [B]
        (vec![0x9B30_0310, 3], 0xF001_0000, 0xF001_0003, Cause::AccessLoad), // LDIDX64A D B 3
        (vec![0x5A00_0310], 0xEFFF_FFFD, 0xF000_0000, Cause::AccessLoad), // MEMGET32 spanning
        (vec![0x5A00_0310], 0xF000_0001, 0xF000_0001, Cause::AccessLoad), // MEMGET32 in MMIO
    ] {
        let mut cpu = stage1_cpu(&code, &[HALT], true);
        cpu.regs[1] = b;
        assert_eq!(cpu.step_one(true), None, "{:08x}", code[0]);
        assert_entered(&cpu, cause, eaddr, CODE_BASE, true);
    }
    // LEAVERET: R15 when the first load faults (as computed), the second
    // doubleword's start when only it does.
    for (fp, eaddr) in [(0xF001_0003_u32, 0xF001_0003), (0xEFFF_FFFC, 0xF000_0000), (0xEFFF_FFF8, 0xF000_0000)] {
        let mut cpu = stage1_cpu(&[0x6680_0000], &[HALT], true);
        cpu.regs[15] = u64::from(fp);
        assert_eq!(cpu.step_one(true), None);
        assert_entered(&cpu, Cause::AccessLoad, eaddr, CODE_BASE, true);
    }
}

#[test]
fn stage1_misaligned_fetch_never_debug_halts() {
    // With a debugger attached and EXC_PASS off, MISALIGNED_FETCH still goes
    // to EVEC like ACCESS_*; with EVEC = 0 it parks as an illegal
    // instruction (which the debugger sees).
    let mut cpu = stage1_cpu(&[NOP], &[HALT], true);
    cpu.set_debugger_attached(true);
    cpu.pc = CODE_BASE + 2;
    assert_eq!(cpu.step_one(false), None);
    assert_entered(&cpu, Cause::MisalignedFetch, CODE_BASE + 2, CODE_BASE + 2, true);
    let mut cpu = stage1_cpu(&[NOP], &[HALT], true);
    cpu.set_debugger_attached(true);
    cpu.evec = 0;
    cpu.pc = CODE_BASE + 2;
    assert!(matches!(cpu.step_one(false), Some(StopReason::InvalidOpcode(_))));
    assert_eq!(cpu.ecause, 0, "a park writes no ECAUSE");
}

// ---- MMU stage 2: the MPU (klaussCPU MMU_PLAN.md §10, mmu 12b3783) ---------

/// `MPU_BASE0`; region n's base is `SR_MPU_BASE0 + 2n`, its limit the next.
const SR_MPU_BASE0: u32 = 0x10;
const SR_TLB_HI: u32 = 0x30;
const SR_TLB_LO: u32 = 0x31;
const SR_TLB_INDEX: u32 = 0x32;
const SR_ASID: u32 = 0x33;
const SR_TLBVEC: u32 = 0x34;
/// `STATUS[8]` `MPU_EN` and `[9]` TE.
const STATUS_MPU_EN: u64 = 1 << 8;
const STATUS_TE: u64 = 1 << 9;
/// `MPU_BASEn` permission and kind bits (EN is added by [`mpu_region`]).
const MPU_R: u32 = 1 << 1;
const MPU_W: u32 = 1 << 2;
const MPU_X: u32 = 1 << 3;
const MPU_GUARD: u32 = 1 << 4;
/// `MEMGET64 D [B]`, `MEMSET64 D [B]`, `MEMGET32 D [B]`, `MEMGET16 D [B]`,
/// `MEMGET8 D [B]`, `MEMSET8 D [B]`.
const LOAD64: u32 = 0x5B10_0310;
const STORE64: u32 = 0x5F10_0310;
const LOAD32: u32 = 0x5A00_0310;
const LOAD16: u32 = 0x5900_0310;
const LOAD8: u32 = 0x5800_0310;
const STORE8: u32 = 0x5C00_0310;
/// `PUSH B`, `POP D`, `LEAVERET`, `JMPR B`.
const PUSH_B: u32 = 0x6400_0010;
const POP_D: u32 = 0x6480_0300;
const LEAVERET: u32 = 0x6680_0000;
const JMPR_B: u32 = 0x6080_0001;
/// The TLB instructions.
const TLBWI: u32 = 0x6C09_0000;
const TLBR: u32 = 0x6C0A_0000;
const TLBP: u32 = 0x6C0B_0000;
const TLBINV: u32 = 0x6C0C_0000;

/// Enable MPU region `n` over the granules of `first`..=`last` with `flags`.
fn mpu_region(cpu: &mut Cpu, n: u32, first: u32, last: u32, flags: u32) {
    assert!(cpu.sr_write(SR_MPU_BASE0 + 2 * n, u64::from(first | flags | 1)));
    assert!(cpu.sr_write(SR_MPU_BASE0 + 2 * n + 1, u64::from(last)));
}

/// A [`stage1_cpu`] with the MPU (`--mpu`) and it on, region 15 letting user mode run the
/// code (`0x000-0x3FF`, X) and region 14 use the top 256 bytes of the user
/// stack (R/W).  Supervisor mode is default-allow.
fn mpu_cpu(code: &[u32], handler: &[u32], user: bool) -> Cpu {
    let mut cpu = stage1_cpu(code, handler, user);
    cpu.set_has_mpu(true);
    mpu_region(&mut cpu, 15, 0, 0x3FF, MPU_X);
    mpu_region(&mut cpu, 14, USER_SP - 0x100, USER_SP - 1, MPU_R | MPU_W);
    assert!(cpu.sr_write(SR_STATUS, STATUS_MPU_EN));
    cpu
}

/// Run one instruction with B = `b` and say whether it ran (else it
/// faulted; the caller checks how).
fn mpu_runs(cpu: &mut Cpu, b: u32) -> bool {
    cpu.regs[1] = u64::from(b);
    let pc = cpu.pc;
    assert_eq!(cpu.step_one(true), None);
    cpu.pc != HANDLER && cpu.pc != pc
}

/// [`assert_entered`], plus `ECAUSE[8]` GUARD = `guard` (in the field and
/// in what `MFSR ECAUSE` reads).
fn assert_mpu_fault(cpu: &Cpu, cause: Cause, eaddr: u32, frame_pc: u32, user: bool, guard: bool) {
    assert_entered(cpu, cause, eaddr, frame_pc, user);
    assert_eq!(cpu.ecause_guard, guard, "{cause:?} at 0x{eaddr:08X}: GUARD");
    assert_eq!(cpu.sr_read(SR_ECAUSE), Some(u64::from(cause as u8) | if guard { 0x100 } else { 0 }));
}

#[test]
fn mpu_sr_layout_and_status_bits() {
    let mut cpu = stage1_cpu(&[HALT], &[HALT], false);
    cpu.set_has_mpu(true);
    // A base keeps all 32 bits; a limit's [4:0] read 0.
    assert!(cpu.sr_write(0x10, 0x1_2345_67FF));
    assert!(cpu.sr_write(0x11, 0xFFFF_FFFF));
    assert_eq!((cpu.sr_read(0x10), cpu.sr_read(0x11)), (Some(0x2345_67FF), Some(0xFFFF_FFE0)));
    assert!(cpu.sr_write(0x2F, 0x3F));
    assert_eq!(cpu.sr_read(0x2F), Some(0x20), "MPU_LIMIT15");
    assert_eq!(cpu.mpu_regions()[15].limit, 0x20);
    let r = cpu.mpu_regions()[0];
    assert!(r.enabled() && r.guard());
    assert_eq!((r.first(), r.last(), r.permissions().as_str()), (0x2345_67E0, 0xFFFF_FFFF, "rwx"));
    // STATUS: MPU_EN and TE are read/write; UM and DI stay read-only.
    assert!(cpu.sr_write(SR_STATUS, STATUS_MPU_EN | STATUS_TE | 3));
    assert_eq!(cpu.sr_read(SR_STATUS), Some(STATUS_MPU_EN | STATUS_TE));
    assert!(cpu.sr_write(SR_STATUS, 0));
    assert_eq!(cpu.sr_read(SR_STATUS), Some(0));
    // ECAUSE has GUARD in [8].
    assert!(cpu.sr_write(SR_ECAUSE, 0x107));
    assert_eq!(cpu.sr_read(SR_ECAUSE), Some(0x107));
    assert_eq!(cpu.system_regs().ecause, 0x107);
    // Reset (every program load): every region off, MPU_EN and TE clear.
    let cpu = cpu_with(&image_from_words(&[HALT]));
    assert!(cpu.mpu_regions().iter().all(|r| *r == MpuRegion::default()));
    assert_eq!(cpu.sr_read(SR_STATUS), Some(0));
}

#[test]
fn no_mpu_by_default_its_srs_are_illegal() {
    // Without the MPU (the default, as on the Nexys A7), MFSR/MTSR of
    // MPU_BASE0 (0x10) and MPU_LIMIT15 (0x2F) are ILLEGAL in either mode,
    // exactly as an SR number with nothing behind it (0x0F, 0x35).
    for sr in [0x0F, 0x10, 0x2F, 0x35] {
        for word in [mfsr(3, sr), mtsr(sr, 1)] {
            for user in [false, true] {
                let mut cpu = stage1_cpu(&[word], &[HALT], user);
                cpu.regs[1] = 0x2011;
                assert_eq!(cpu.step_one(true), None, "{word:08x}");
                assert_entered(&cpu, Cause::Illegal, word, CODE_BASE, user);
                assert_eq!(cpu.regs[3], 0, "{word:08x}: nothing read");
                assert_eq!(cpu.mpu_regions()[0], MpuRegion::default(), "{word:08x}: nothing written");
            }
        }
    }
    let mut cpu = stage1_cpu(&[HALT], &[HALT], false);
    assert_eq!((cpu.sr_read(0x10), cpu.sr_read(0x2F)), (None, None));
    assert!(!cpu.sr_write(0x10, 0x2011) && !cpu.sr_write(0x2F, 0x3F));
    // With the MPU they are there.
    cpu.set_has_mpu(true);
    assert!(cpu.sr_write(0x10, 0x2011) && cpu.sr_write(0x2F, 0x3F));
    assert_eq!((cpu.sr_read(0x10), cpu.sr_read(0x2F)), (Some(0x2011), Some(0x20)));
}

#[test]
fn no_mpu_by_default_status_mpu_en_ignores_writes() {
    // MTSR STATUS with MPU_EN (and TE): TE is taken, MPU_EN reads 0.
    let mut cpu = stage1_cpu(&[mtsr(SR_STATUS, 1), mfsr(3, SR_STATUS), HALT], &[HALT], false);
    cpu.regs[1] = STATUS_MPU_EN | STATUS_TE;
    assert_eq!(cpu.step_one(true), None);
    assert!(!cpu.mpu_en && cpu.te);
    assert!(!cpu.system_regs().mpu_en);
    assert!(cpu.sr_write(SR_STATUS, STATUS_MPU_EN));
    assert_eq!(cpu.sr_read(SR_STATUS), Some(0));
    // So the MPU never checks anything: a supervisor fetch from MMIO is not
    // the MPU_EN fake fill (ILLEGAL), but outside RAM as with MPU_EN clear.
    let mut cpu = stage1_cpu(&[NOP], &[HALT], false);
    assert!(cpu.sr_write(SR_STATUS, STATUS_MPU_EN));
    cpu.pc = 0xF000_0000;
    assert_eq!(cpu.step_one(true), Some(StopReason::PcOutOfRange(0xF000_0000)));
    assert_eq!(cpu.ecause, 0, "no exception entered");
    // Taking the MPU away clears MPU_EN.
    let mut cpu = mpu_cpu(&[HALT], &[HALT], false);
    assert!(cpu.mpu_en);
    cpu.set_has_mpu(false);
    assert!(!cpu.mpu_en);
    assert_eq!(cpu.sr_read(SR_STATUS), Some(0));
}

#[test]
fn mpu_guard_denies_in_either_mode() {
    for user in [false, true] {
        for (word, cause) in [(LOAD64, Cause::AccessLoad), (STORE64, Cause::AccessStore)] {
            let mut cpu = mpu_cpu(&[word, HALT], &[HALT], user);
            // A guard granule inside a region user mode may otherwise use;
            // the guard's own R/W bits are ignored.
            mpu_region(&mut cpu, 0, 0x2000, 0x2000, MPU_GUARD | MPU_R | MPU_W);
            mpu_region(&mut cpu, 1, 0x1000, 0x3FFF, MPU_R | MPU_W);
            let regs = cpu.regs;
            assert!(!mpu_runs(&mut cpu, 0x2008), "{word:08x} user {user}");
            assert_mpu_fault(&cpu, cause, 0x2008, CODE_BASE, user, true);
            assert_eq!(cpu.regs[3], regs[3], "nothing loaded");
            // The next granule is not guarded.
            let mut cpu = mpu_cpu(&[word, HALT], &[HALT], user);
            mpu_region(&mut cpu, 0, 0x2000, 0x2000, MPU_GUARD);
            mpu_region(&mut cpu, 1, 0x1000, 0x3FFF, MPU_R | MPU_W);
            assert!(mpu_runs(&mut cpu, 0x2020), "{word:08x} user {user}");
        }
    }
    // A later fault that is not a guard denial clears GUARD.
    let mut cpu = mpu_cpu(&[LOAD64, HALT], &[IRET], true);
    mpu_region(&mut cpu, 0, 0x2000, 0x2000, MPU_GUARD);
    assert!(!mpu_runs(&mut cpu, 0x2000));
    assert!(cpu.ecause_guard);
    assert_eq!(cpu.step_one(true), None); // IRET: retry, now with B elsewhere
    assert!(!mpu_runs(&mut cpu, 0x3000));
    assert_eq!((cpu.ecause, cpu.eaddr, cpu.ecause_guard), (Cause::AccessLoad as u8, 0x3000, false));
}

#[test]
fn mpu_user_mode_is_a_union_of_regions_and_default_deny() {
    // Region 0 gives R over 0x2000-0x203F, region 1 W over 0x2020-0x205F.
    // (word, B, runs)
    for (word, b, runs) in [
        (LOAD64, 0x2000, true),
        (STORE64, 0x2000, false), // R only
        (STORE64, 0x2020, true),  // W from region 1
        (LOAD64, 0x2020, true),   // R from region 0
        (LOAD64, 0x2040, false),  // W only
        (STORE64, 0x2058, true),
        (LOAD8, 0x3000, false), // no region: default deny
        (STORE8, 0x3000, false),
    ] {
        let mut cpu = mpu_cpu(&[word, HALT], &[HALT], true);
        mpu_region(&mut cpu, 0, 0x2000, 0x2020, MPU_R);
        mpu_region(&mut cpu, 1, 0x2020, 0x2040, MPU_W);
        assert_eq!(mpu_runs(&mut cpu, b), runs, "{word:08x} at 0x{b:X}");
        if !runs {
            let cause = if word == LOAD64 || word == LOAD8 {
                Cause::AccessLoad
            } else {
                Cause::AccessStore
            };
            assert_mpu_fault(&cpu, cause, b, CODE_BASE, true, false);
        }
        // Supervisor mode is default-allow.
        let mut cpu = mpu_cpu(&[word, HALT], &[HALT], false);
        mpu_region(&mut cpu, 0, 0x2000, 0x2020, MPU_R);
        assert!(mpu_runs(&mut cpu, b), "{word:08x} at 0x{b:X} in supervisor mode");
    }
}

#[test]
fn mpu_r_w_x_are_separate() {
    for perm in [MPU_R, MPU_W, MPU_X] {
        // Data: region 0 over 0x2000-0x201F with one permission.
        for (word, needs) in [(LOAD64, MPU_R), (STORE64, MPU_W)] {
            let mut cpu = mpu_cpu(&[word, HALT], &[HALT], true);
            mpu_region(&mut cpu, 0, 0x2000, 0x2000, perm);
            assert_eq!(mpu_runs(&mut cpu, 0x2000), perm == needs, "{word:08x} with {perm:#x}");
        }
        // Fetch: the code region has just this permission.
        let mut cpu = mpu_cpu(&[NOP, HALT], &[HALT], true);
        mpu_region(&mut cpu, 15, 0, 0x3FF, perm);
        assert_eq!(cpu.step_one(true), None);
        if perm == MPU_X {
            assert_eq!(cpu.pc, CODE_BASE + 4);
        } else {
            assert_mpu_fault(&cpu, Cause::AccessFetch, CODE_BASE, CODE_BASE, true, false);
        }
    }
    // Stack instructions are checked by direction: a push needs W, a pop R.
    for (code, b_sp, cause) in [
        (vec![PUSH_B, HALT], USER_SP - 0x100, Cause::AccessStore),            // writes 0x7EF8
        (vec![0xA200_0000, 0x40, HALT], USER_SP - 0x100, Cause::AccessStore), // CALL 0x40
        (vec![POP_D, HALT], USER_SP - 0x200, Cause::AccessLoad),
    ] {
        let mut cpu = mpu_cpu(&code, &[HALT], true);
        cpu.sp = b_sp;
        let eaddr = if cause == Cause::AccessStore { b_sp - 8 } else { b_sp };
        assert_eq!(cpu.step_one(true), None);
        assert_eq!((cpu.pc, cpu.ecause, cpu.eaddr), (HANDLER, cause as u8, eaddr), "{:08x}", code[0]);
        assert_eq!(cpu.sp_alt, b_sp, "the user SP is unchanged");
    }
}

#[test]
fn mpu_two_access_instructions_check_both_doublewords_first() {
    // User R over 0x2000-0x203F only.  MEMGET32 at 0x203E needs 0x2038 and
    // 0x2040: the second is denied, EADDR its start, nothing loaded.
    let mut cpu = mpu_cpu(&[LOAD32, HALT], &[HALT], true);
    mpu_region(&mut cpu, 0, 0x2000, 0x2020, MPU_R);
    cpu.regs[3] = 0x5555;
    assert!(!mpu_runs(&mut cpu, 0x203E));
    assert_mpu_fault(&cpu, Cause::AccessLoad, 0x2040, CODE_BASE, true, false);
    assert_eq!(cpu.regs[3], 0x5555);
    // Within one doubleword it runs.
    let mut cpu = mpu_cpu(&[LOAD32, HALT], &[HALT], true);
    mpu_region(&mut cpu, 0, 0x2000, 0x2020, MPU_R);
    assert!(cpu.debug_write(0x203C, &0x1234_5678_u32.to_le_bytes()));
    assert!(mpu_runs(&mut cpu, 0x203C));
    assert_eq!(cpu.regs[3], 0x1234_5678);
    // LEAVERET: R15 = 0x2038 reads 0x2038 and 0x2040; the second is denied.
    for (fp, runs) in [(0x2038_u32, false), (0x2030, true)] {
        let mut cpu = mpu_cpu(&[LEAVERET, HALT], &[HALT], true);
        mpu_region(&mut cpu, 0, 0x2000, 0x2020, MPU_R);
        assert!(cpu.debug_write(0x2038, &u64::from(CODE_BASE + 4).to_le_bytes()));
        cpu.regs[15] = u64::from(fp);
        let sp = cpu.sp;
        assert_eq!(cpu.step_one(true), None);
        if runs {
            assert_eq!((cpu.pc, cpu.sp), (CODE_BASE + 4, fp + 16));
        } else {
            assert_mpu_fault(&cpu, Cause::AccessLoad, 0x2040, CODE_BASE, true, false);
            assert_eq!((cpu.regs[15], cpu.sp_alt), (u64::from(fp), sp), "R15 and SP unchanged");
        }
    }
    // A guard on the second doubleword denies it in supervisor mode too.
    let mut cpu = mpu_cpu(&[LOAD32, HALT], &[HALT], false);
    mpu_region(&mut cpu, 0, 0x2040, 0x2040, MPU_GUARD);
    assert!(!mpu_runs(&mut cpu, 0x203E));
    assert_mpu_fault(&cpu, Cause::AccessLoad, 0x2040, CODE_BASE, false, true);
}

#[test]
fn mpu_fetch_checks_every_word_of_an_instruction() {
    // SETR A 0x1234 (two words) at 0x3C, its second word in the next
    // granule, 0x40.  User mode with X over 0x20-0x3F only: ACCESS_FETCH at
    // the second word, the frame PC the instruction.
    let mut code = vec![NOP; 7];
    code.extend(setr(0, 0x1234));
    code.push(HALT);
    let mut cpu = mpu_cpu(&code, &[HALT], true);
    mpu_region(&mut cpu, 15, 0x20, 0x20, MPU_X);
    cpu.pc = 0x3C;
    assert_eq!(cpu.step_one(true), None);
    assert_mpu_fault(&cpu, Cause::AccessFetch, 0x40, 0x3C, true, false);
    assert_eq!(cpu.regs[0], 0, "not executed");
    // With X over both granules it runs.
    let mut cpu = mpu_cpu(&code, &[HALT], true);
    cpu.pc = 0x3C;
    assert_eq!(cpu.step_one(true), None);
    assert_eq!((cpu.pc, cpu.regs[0]), (0x44, 0x1234));
    // A guard on the second word stops supervisor mode as well.
    let mut cpu = mpu_cpu(&code, &[HALT], false);
    mpu_region(&mut cpu, 0, 0x40, 0x40, MPU_GUARD);
    cpu.pc = 0x3C;
    assert_eq!(cpu.step_one(true), None);
    assert_mpu_fault(&cpu, Cause::AccessFetch, 0x40, 0x3C, false, true);
}

#[test]
fn mpu_limit_is_the_inclusive_last_granule() {
    // Base 0x2000, limit written 0x203F: granules 0x2000 and 0x2020.
    for (b, runs) in [(0x1FFF, false), (0x2000, true), (0x203F, true), (0x2040, false)] {
        let mut cpu = mpu_cpu(&[LOAD8, HALT], &[HALT], true);
        mpu_region(&mut cpu, 3, 0x2000, 0x203F, MPU_R);
        assert_eq!(cpu.sr_read(SR_MPU_BASE0 + 7), Some(0x2020));
        assert_eq!(mpu_runs(&mut cpu, b), runs, "0x{b:X}");
    }
    // A disabled region covers nothing, and a limit below the base neither.
    let mut cpu = mpu_cpu(&[LOAD8, HALT], &[HALT], true);
    mpu_region(&mut cpu, 3, 0x2000, 0x2000, MPU_R);
    assert!(cpu.sr_write(SR_MPU_BASE0 + 6, u64::from(0x2000 | MPU_R)));
    assert!(!mpu_runs(&mut cpu, 0x2000));
    let mut cpu = mpu_cpu(&[LOAD8, HALT], &[HALT], true);
    mpu_region(&mut cpu, 3, 0x2020, 0x2000, MPU_R);
    assert!(!mpu_runs(&mut cpu, 0x2000));
}

#[test]
fn mpu_off_checks_nothing() {
    // MPU_EN = 0: a guard and user default deny do not apply.
    for user in [false, true] {
        let mut cpu = mpu_cpu(&[LOAD64, HALT], &[HALT], user);
        mpu_region(&mut cpu, 0, 0x2000, 0x2000, MPU_GUARD);
        assert!(cpu.sr_write(SR_STATUS, 0));
        assert!(mpu_runs(&mut cpu, 0x2000));
        let mut cpu = mpu_cpu(&[LOAD64, HALT], &[HALT], user);
        assert!(cpu.sr_write(SR_STATUS, 0));
        assert!(mpu_runs(&mut cpu, 0x3000));
    }
    // With TE = 1 the MPU is ignored too (the kseg alias of a guard).
    let mut cpu = te_cpu(&[LOAD64, HALT], &[HALT], false);
    cpu.set_has_mpu(true);
    mpu_region(&mut cpu, 0, 0x2000, 0x2000, MPU_GUARD);
    cpu.mpu_en = true;
    cpu.regs[1] = 0x8000_2000;
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(cpu.pc, KSEG + CODE_BASE + 4);
}

#[test]
fn mpu_keeps_the_stage1_user_mmio_rule_and_guards_mmio_in_supervisor_mode() {
    // A user region over MMIO does not open it: ACCESS_LOAD, not a guard.
    let mut cpu = mpu_cpu(&[LOAD64, HALT], &[HALT], true);
    mpu_region(&mut cpu, 0, 0xF000_0000, 0xFFFF_FFE0, MPU_R | MPU_W);
    cpu.feed_uart_rx(b"x");
    assert!(!mpu_runs(&mut cpu, 0xF001_0008));
    assert_mpu_fault(&cpu, Cause::AccessLoad, 0xF001_0008, CODE_BASE, true, false);
    assert_eq!(cpu.uart_rx.len(), 1);
    // ... and a user guard over MMIO is still the MMIO fault, not GUARD.
    let mut cpu = mpu_cpu(&[LOAD64, HALT], &[HALT], true);
    mpu_region(&mut cpu, 0, 0xF000_0000, 0xFFFF_FFE0, MPU_GUARD);
    assert!(!mpu_runs(&mut cpu, 0xF001_0008));
    assert_mpu_fault(&cpu, Cause::AccessLoad, 0xF001_0008, CODE_BASE, true, false);
    // A supervisor guard over a device denies it; the device is not touched.
    let mut cpu = mpu_cpu(&[LOAD64, HALT], &[HALT], false);
    mpu_region(&mut cpu, 0, 0xF001_0000, 0xF001_0000, MPU_GUARD);
    cpu.feed_uart_rx(b"x");
    assert!(!mpu_runs(&mut cpu, 0xF001_0008));
    assert_mpu_fault(&cpu, Cause::AccessLoad, 0xF001_0008, CODE_BASE, false, true);
    assert_eq!(cpu.uart_rx.len(), 1);
    // Without the guard supervisor mode reads it.
    let mut cpu = mpu_cpu(&[LOAD64, HALT], &[HALT], false);
    cpu.feed_uart_rx(b"x");
    assert!(mpu_runs(&mut cpu, 0xF001_0008));
    assert_eq!(cpu.regs[3], u64::from(b'x'));
}

#[test]
fn mpu_does_not_check_the_entry_frame_push() {
    // A supervisor PUSH into a guard faults with SP unchanged; the entry
    // then pushes its frame at SP - 8, inside the guard, unchecked (§10.2).
    // A vector that pushes faults again, one frame lower (the descent the
    // spec's guard-safe vector rule is about).
    let mut cpu = mpu_cpu(&[PUSH_B, HALT], &[PUSH_B], false);
    mpu_region(&mut cpu, 0, 0x2000, 0x2000, MPU_GUARD);
    cpu.sp = 0x2018;
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(
        (cpu.pc, cpu.ecause, cpu.eaddr, cpu.ecause_guard),
        (HANDLER, Cause::AccessStore as u8, 0x2010, true)
    );
    assert_eq!(cpu.sp, 0x2010, "the frame went into the guard");
    assert_eq!(cpu.peek64(0x2010) as u32, CODE_BASE);
    assert_eq!(cpu.step_one(true), None);
    assert_eq!((cpu.pc, cpu.eaddr, cpu.sp), (HANDLER, 0x2008, 0x2008));
    assert_eq!(cpu.peek64(0x2008) as u32, HANDLER);
}

// ---- MMU stage 3: the TLB (klaussCPU MMU_PLAN.md §11, mmu 12b3783) ---------

/// kseg: PA = VA - `KSEG`.
const KSEG: u32 = 0x8000_0000;
/// The exception vector and kernel stack top under TE = 1: the kseg
/// aliases of [`HANDLER`] and [`KERNEL_SP`].
const K_HANDLER: u32 = KSEG + HANDLER;
const K_SP: u32 = KSEG + KERNEL_SP;
/// User code under TE = 1: page 1, mapped (global, R/X) to frame 0, the image.
const U_CODE: u32 = 0x0001_0000;
/// The user stack top under TE = 1: page 2 is mapped (global, R/W) to frame 1.
const U_SP: u32 = 0x0003_0000;
/// A user data page, mapped by the tests that use it, and its frame.
const U_DATA: u32 = 0x0005_0000;
const DATA_FRAME: u32 = 0x3_0000;
/// TLB entry bits.
const TLB_V: u32 = 1;
const TLB_R: u32 = 1 << 1;
const TLB_W: u32 = 1 << 2;
const TLB_X: u32 = 1 << 3;

/// Set TLB entry `i` to map the page of `va` to the frame of `pa` with
/// `bits`, for address space `asid` or (`None`) every one.
fn tlb_map(cpu: &mut Cpu, i: usize, va: u32, pa: u32, bits: u32, asid: Option<u8>) {
    cpu.tlb[i] = TlbEntry {
        hi: (va & 0xFFFF_0000) | asid.map_or(0x100, u32::from),
        lo: (pa & 0x07FF_0000) | bits,
    };
}

/// A [`stage1_cpu`] with translation on: `EVEC` and the kernel stack in
/// kseg, user code at [`U_CODE`] and the user stack below [`U_SP`] (TLB
/// entries 0 and 1), supervisor code at the kseg alias of the code base.
fn te_cpu(code: &[u32], handler: &[u32], user: bool) -> Cpu {
    let mut cpu = stage1_cpu(code, handler, user);
    cpu.te = true;
    cpu.evec = K_HANDLER;
    tlb_map(&mut cpu, 0, U_CODE, 0, TLB_V | TLB_R | TLB_X, None);
    tlb_map(&mut cpu, 1, U_SP - 1, 0x1_0000, TLB_V | TLB_R | TLB_W, None);
    if user {
        (cpu.pc, cpu.sp, cpu.sp_alt) = (U_CODE + CODE_BASE, U_SP, K_SP);
    } else {
        (cpu.pc, cpu.sp, cpu.sp_alt) = (KSEG + CODE_BASE, K_SP, U_SP);
    }
    cpu
}

/// Assert `cpu` (from [`te_cpu`]) has just entered `vector` for `cause`
/// with `eaddr` from the instruction at `frame_pc`: the frame is on the
/// kernel stack (kseg), DI set, supervisor mode.
fn assert_entered_te(cpu: &Cpu, vector: u32, cause: Cause, eaddr: u32, frame_pc: u32, from_user: bool) {
    let what = format!("{cause:?} at 0x{eaddr:08X}");
    assert_eq!(cpu.stop, None, "{what}");
    assert_eq!(
        (cpu.pc, cpu.ecause, cpu.eaddr),
        (vector, cause as u8, eaddr),
        "{what}: vector, ECAUSE, EADDR"
    );
    assert!(!cpu.um && cpu.irq_disabled, "{what}");
    assert_eq!(cpu.sp, K_SP - 8, "{what}: the frame is on the kernel stack");
    assert_eq!(cpu.sp_alt, U_SP, "{what}: SP_ALT is the user SP");
    // The frame push is untranslated: PA = SP - 8 - 0x8000_0000.
    let frame = cpu.peek64(KERNEL_SP - 8);
    assert_eq!((frame as u32, (frame >> 44) & 1), (frame_pc, u64::from(from_user)), "{what}: frame");
}

/// Step once with B = `b` and the CPU as set up.
fn step_with_b(cpu: &mut Cpu, b: u32) {
    cpu.regs[1] = u64::from(b);
    assert_eq!(cpu.step_one(true), None);
}

#[test]
fn tlb_reset_state_and_srs() {
    let mut cpu = cpu_with(&image_from_words(&[HALT]));
    // Reset (and so every program load): TE, ASID and TLBVEC clear and
    // every entry invalid.
    let s = cpu.system_regs();
    assert!(!s.te && !s.mpu_en);
    assert_eq!((s.asid, s.tlbvec, s.tlb_hi, s.tlb_lo, s.tlb_index), (0, 0, 0, 0, 0));
    assert!(cpu.tlb_entries().iter().all(|e| !e.valid()));
    // The SRs keep only their bits.
    for (sr, value, back) in [
        (SR_TLB_HI, 0xFFFF_FFFF_u64, 0xFFFF_01FF_u64),
        (SR_TLB_LO, 0xFFFF_FFFF, 0x07FF_000F),
        (SR_TLB_INDEX, 0xFFFF_FFFF, u64::from(super::mmu::TLB_INDEX_BITS)),
        (SR_ASID, 0x1FF, 0xFF),
        (SR_TLBVEC, 0x8000_0503, 0x8000_0500), // [1:0] read 0 (424e236)
    ] {
        assert!(cpu.sr_write(sr, value), "SR {sr:#x}");
        assert_eq!(cpu.sr_read(sr), Some(back), "SR {sr:#x}");
    }
}

#[test]
fn tlb_kseg_is_direct_and_the_hole_faults() {
    // Supervisor code runs from kseg; kseg data is PA = VA - 0x8000_0000.
    let code = [LOAD64, 0x5F10_0420, HALT]; // D = [B]; [C] = E
    let mut cpu = te_cpu(&code, &[HALT], false);
    assert!(cpu.debug_write(0x2000, &0x1234_u64.to_le_bytes()));
    cpu.regs[2] = 0x8000_2008;
    cpu.regs[4] = 0x5678;
    step_with_b(&mut cpu, 0x8000_2000);
    assert_eq!(cpu.step_one(true), None);
    assert_eq!((cpu.regs[3], cpu.peek64(0x2008)), (0x1234, 0x5678));
    assert_eq!(cpu.pc, KSEG + CODE_BASE + 8);
    // 0x8800_0000-0xBFFF_FFFF has no DDR: ACCESS_*, and TLB_HI is not loaded.
    for (word, b, cause) in [
        (LOAD64, 0x8800_0000, Cause::AccessLoad),
        (LOAD8, 0xBFFF_FFFF, Cause::AccessLoad),
        (STORE64, 0x9000_0000, Cause::AccessStore),
    ] {
        let mut cpu = te_cpu(&[word, HALT], &[HALT], false);
        cpu.tlb_hi = 0xAAAA_0000;
        step_with_b(&mut cpu, b);
        assert_entered_te(&cpu, K_HANDLER, cause, b, KSEG + CODE_BASE, false);
        assert_eq!(cpu.tlb_hi, 0xAAAA_0000, "a segment fault does not load TLB_HI");
    }
    // A fetch from the hole.
    let mut cpu = te_cpu(&[HALT], &[HALT], false);
    cpu.pc = 0x9000_0000;
    assert_eq!(cpu.step_one(true), None);
    assert_entered_te(&cpu, K_HANDLER, Cause::AccessFetch, 0x9000_0000, 0x9000_0000, false);
    // MMIO is identity in supervisor mode.
    let mut cpu = te_cpu(&[LOAD64, HALT], &[HALT], false);
    step_with_b(&mut cpu, 0xF00F_0048);
    assert_eq!(cpu.regs[3], 0x100D, "CPU_FEATURES (no MPU)");
}

#[test]
fn tlb_kmap_and_everything_above_useg_are_supervisor_only() {
    // kmap at 0xC000_0000 (global entry to frame 2) works in supervisor mode.
    let mut cpu = te_cpu(&[LOAD64, HALT], &[HALT], false);
    tlb_map(&mut cpu, 2, 0xC000_0000, 0x2_0000, TLB_V | TLB_R, None);
    assert!(cpu.debug_write(0x2_0010, &0x77_u64.to_le_bytes()));
    step_with_b(&mut cpu, 0xC000_0010);
    assert_eq!(cpu.regs[3], 0x77);
    // Supervisor mode also reaches useg through the TLB.
    let mut cpu = te_cpu(&[LOAD64, HALT], &[HALT], false);
    tlb_map(&mut cpu, 2, U_DATA, DATA_FRAME, TLB_V | TLB_R, Some(0));
    assert!(cpu.debug_write(DATA_FRAME + 8, &0x99_u64.to_le_bytes()));
    step_with_b(&mut cpu, U_DATA + 8);
    assert_eq!(cpu.regs[3], 0x99);
    // In user mode any address at or above 0x8000_0000 is ACCESS_*, even
    // one a TLB entry maps; TLB_HI is not loaded.
    for (word, b, cause) in [
        (LOAD64, 0xC000_0010, Cause::AccessLoad),
        (LOAD64, 0x8000_2000, Cause::AccessLoad),
        (STORE64, 0x8000_2000, Cause::AccessStore),
        (LOAD64, 0x8800_0000, Cause::AccessLoad),
        (LOAD64, 0xF001_0008, Cause::AccessLoad),
    ] {
        let mut cpu = te_cpu(&[word, HALT], &[HALT], true);
        tlb_map(&mut cpu, 2, 0xC000_0000, 0x2_0000, TLB_V | TLB_R | TLB_W, None);
        cpu.tlb_hi = 0xAAAA_0000;
        step_with_b(&mut cpu, b);
        assert_entered_te(&cpu, K_HANDLER, cause, b, U_CODE + CODE_BASE, true);
        assert_eq!(cpu.tlb_hi, 0xAAAA_0000);
    }
    // A user fetch from kseg.
    let mut cpu = te_cpu(&[HALT], &[HALT], true);
    cpu.pc = KSEG + CODE_BASE;
    assert_eq!(cpu.step_one(true), None);
    assert_entered_te(&cpu, K_HANDLER, Cause::AccessFetch, KSEG + CODE_BASE, KSEG + CODE_BASE, true);
}

#[test]
fn tlb_misses_raise_tlb_miss_with_eaddr_and_load_tlb_hi() {
    // User mode, ASID 7: a load, a store and a jump to an unmapped page.
    for (word, b, cause) in [
        (LOAD64, U_DATA + 0x1234, Cause::TlbMissLoad),
        (STORE64, U_DATA + 0x1236, Cause::TlbMissStore),
        (LOAD16, U_DATA + 2, Cause::TlbMissLoad),
    ] {
        let mut cpu = te_cpu(&[word, HALT], &[HALT], true);
        cpu.asid = 7;
        cpu.tlb_hi = 0x100;
        step_with_b(&mut cpu, b);
        assert_entered_te(&cpu, K_HANDLER, cause, b, U_CODE + CODE_BASE, true);
        assert_eq!(cpu.tlb_hi, U_DATA | 7, "TLB_HI = {{VPN, G = 0, ASID}}");
        assert_eq!(cpu.sr_read(SR_ECAUSE), Some(u64::from(cause as u8)));
    }
    // JMPR to an unmapped page: the jump retires; the fetch there misses,
    // with the frame PC and EADDR the target.
    let mut cpu = te_cpu(&[JMPR_B, HALT], &[HALT], true);
    cpu.asid = 7;
    step_with_b(&mut cpu, 0x0006_0040);
    assert_eq!(cpu.pc, 0x0006_0040);
    assert_eq!(cpu.step_one(true), None);
    assert_entered_te(&cpu, K_HANDLER, Cause::TlbMissFetch, 0x0006_0040, 0x0006_0040, true);
    assert_eq!(cpu.tlb_hi, 0x0006_0007);
    // An instruction whose second word is on an unmapped page: page 7 maps
    // frame 0, so SETR A at 0x0007_FFFC has its word 0 at PA 0xFFFC.
    let mut cpu = te_cpu(&[HALT], &[HALT], true);
    tlb_map(&mut cpu, 2, 0x0007_0000, 0, TLB_V | TLB_X, None);
    assert!(cpu.debug_write(0xFFFC, &setr(0, 5)[0].to_le_bytes()));
    cpu.pc = 0x0007_FFFC;
    assert_eq!(cpu.step_one(true), None);
    assert_entered_te(&cpu, K_HANDLER, Cause::TlbMissFetch, 0x0008_0000, 0x0007_FFFC, true);
    assert_eq!(cpu.regs[0], 0);
}

#[test]
fn tlb_miss_vectors_to_tlbvec_when_set() {
    // (TLBVEC, a miss or a permission fault, where it goes)
    for (tlbvec, miss, vector) in [
        (K_HANDLER + 0x40, true, K_HANDLER + 0x40),
        (0, true, K_HANDLER),
        (K_HANDLER + 0x40, false, K_HANDLER), // ACCESS_* always EVEC
    ] {
        let mut cpu = te_cpu(&[LOAD64, HALT], &[HALT], true);
        cpu.tlbvec = tlbvec;
        if !miss {
            tlb_map(&mut cpu, 2, U_DATA, DATA_FRAME, TLB_V | TLB_W, None); // no R
        }
        step_with_b(&mut cpu, U_DATA);
        let cause = if miss { Cause::TlbMissLoad } else { Cause::AccessLoad };
        assert_entered_te(&cpu, vector, cause, U_DATA, U_CODE + CODE_BASE, true);
        assert_eq!(cpu.tlb_hi, U_DATA, "loaded on both");
    }
    // EVEC = 0 parks, TLBVEC or not, and writes no ECAUSE/EADDR/TLB_HI.
    let mut cpu = te_cpu(&[LOAD64, HALT], &[HALT], true);
    cpu.evec = 0;
    cpu.tlbvec = K_HANDLER;
    cpu.tlb_hi = 0x1234_0000;
    cpu.regs[1] = u64::from(U_DATA);
    assert_eq!(cpu.step_one(true), Some(StopReason::InvalidOpcode(LOAD64)));
    assert_eq!((cpu.ecause, cpu.eaddr, cpu.tlb_hi, cpu.pc), (0, 0, 0x1234_0000, U_CODE + CODE_BASE));
    assert_eq!(cpu.parked_exception(), Some((Cause::TlbMissLoad, U_DATA)));
}

#[test]
fn tlbvec_low_bits_read_zero() {
    // MMU_PLAN 424e236: TLBVEC[1:0] read 0, as EVEC's do.
    let mut cpu = te_cpu(&[HALT], &[HALT], true);
    assert!(cpu.sr_write(SR_TLBVEC, 0x8000_1003));
    assert_eq!(cpu.tlbvec, 0x8000_1000);
    assert!(cpu.sr_write(SR_EVEC, 0x8000_2003));
    assert_eq!(cpu.evec, 0x8000_2000);
}

#[test]
fn tlb_permission_faults_and_the_dirty_bit_flow() {
    // (entry bits, the instruction, its cause): a matching entry without
    // the permission is ACCESS_*, which loads TLB_HI too.
    for (bits, word, cause) in [
        (TLB_V | TLB_W | TLB_X, LOAD64, Cause::AccessLoad),
        (TLB_V | TLB_R | TLB_X, STORE64, Cause::AccessStore),
        (TLB_R | TLB_W, LOAD64, Cause::TlbMissLoad), // V = 0 never matches
    ] {
        let mut cpu = te_cpu(&[word, HALT], &[HALT], true);
        cpu.asid = 3;
        tlb_map(&mut cpu, 5, U_DATA, DATA_FRAME, bits, Some(3));
        step_with_b(&mut cpu, U_DATA + 0x10);
        assert_entered_te(&cpu, K_HANDLER, cause, U_DATA + 0x10, U_CODE + CODE_BASE, true);
        assert_eq!((cpu.tlb_hi, cpu.ecause_guard), (U_DATA | 3, false));
    }
    // A page without X cannot be fetched from.
    let mut cpu = te_cpu(&[HALT], &[HALT], true);
    tlb_map(&mut cpu, 0, U_CODE, 0, TLB_V | TLB_R, None);
    assert_eq!(cpu.step_one(true), None);
    assert_entered_te(&cpu, K_HANDLER, Cause::AccessFetch, U_CODE + CODE_BASE, U_CODE + CODE_BASE, true);
    // Dirty tracking: the page starts W = 0; the first store faults; the
    // handler finds the entry with TLBP (TLB_HI was loaded), sets W with
    // TLBR / TLBWI and returns; the store runs again and lands.
    let handler = [
        TLBP,
        TLBR,
        mfsr(12, SR_TLB_LO), // M = TLB_LO
        0x8940_0CC0,         // ORV M 4 (W)
        4,
        mtsr(SR_TLB_LO, 12),
        TLBWI,
        IRET,
    ];
    let mut cpu = te_cpu(&[STORE64, HALT], &handler, true);
    cpu.asid = 3;
    tlb_map(&mut cpu, 5, U_DATA, DATA_FRAME, TLB_V | TLB_R, Some(3));
    tlb_map(&mut cpu, 6, 0x0009_0000, 0x4_0000, TLB_V | TLB_R, Some(3));
    cpu.regs[3] = 0xD1D1;
    step_with_b(&mut cpu, U_DATA + 0x18);
    assert_entered_te(&cpu, K_HANDLER, Cause::AccessStore, U_DATA + 0x18, U_CODE + CODE_BASE, true);
    for _ in 0..7 {
        assert_eq!(cpu.step_one(true), None);
    }
    assert!(cpu.um);
    assert_eq!(cpu.pc, U_CODE + CODE_BASE, "back at the store");
    assert_eq!(cpu.tlb_index, 5, "TLBP found entry 5");
    assert_eq!(cpu.tlb[5].lo, DATA_FRAME | TLB_V | TLB_R | TLB_W);
    assert_eq!(cpu.tlb[6].lo, 0x4_0000 | TLB_V | TLB_R, "the other entry is untouched");
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(cpu.pc, U_CODE + CODE_BASE + 4, "the retried store ran");
    assert_eq!(cpu.peek64(DATA_FRAME + 0x18), 0xD1D1);
}

#[test]
fn tlb_global_and_asid_matching_and_asid_changes() {
    // Two non-global entries for the same page in ASIDs 1 and 2 (frames 3
    // and 4) and a global one for another page (frame 5).
    let code = [LOAD64, mtsr(SR_ASID, 2), 0x5B10_0410, 0x5B10_0520, HALT]; // D = [B]; ASID = C; E = [B]; F = [C]
    let mut cpu = te_cpu(&code, &[HALT], false);
    tlb_map(&mut cpu, 3, U_DATA, 0x3_0000, TLB_V | TLB_R, Some(1));
    tlb_map(&mut cpu, 4, U_DATA, 0x4_0000, TLB_V | TLB_R, Some(2));
    tlb_map(&mut cpu, 5, 0x0006_0000, 0x5_0000, TLB_V | TLB_R, None);
    for (pa, v) in [(0x3_0000_u32, 0x33_u64), (0x4_0000, 0x44), (0x5_0000, 0x55)] {
        assert!(cpu.debug_write(pa, &v.to_le_bytes()));
    }
    cpu.asid = 1;
    cpu.regs[2] = 2;
    cpu.reservation = Some(0x10);
    step_with_b(&mut cpu, U_DATA);
    assert_eq!(cpu.regs[3], 0x33, "ASID 1");
    assert_eq!(cpu.step_one(true), None);
    assert_eq!((cpu.asid, cpu.reservation), (2, None), "MTSR ASID clears the reservation");
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(cpu.regs[4], 0x44, "ASID 2: the other entry");
    cpu.regs[2] = 0x0006_0000;
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(cpu.regs[5], 0x55, "a global entry in ASID 2");
    // In ASID 9 the page misses, and the global page still hits.
    for (b, hits) in [(U_DATA, false), (0x0006_0000, true)] {
        let mut cpu = te_cpu(&[LOAD64, HALT], &[HALT], true);
        tlb_map(&mut cpu, 3, U_DATA, 0x3_0000, TLB_V | TLB_R, Some(1));
        tlb_map(&mut cpu, 5, 0x0006_0000, 0x5_0000, TLB_V | TLB_R, None);
        cpu.asid = 9;
        step_with_b(&mut cpu, b);
        assert_eq!(cpu.pc != K_HANDLER, hits, "0x{b:08X}");
        if !hits {
            assert_eq!((cpu.ecause, cpu.tlb_hi), (Cause::TlbMissLoad as u8, U_DATA | 9));
        }
    }
}

#[test]
fn tlbwi_tlbr_tlbp_tlbinv() {
    // Supervisor mode, TE off (the TLB instructions work either way).
    let mut cpu = stage1_cpu(&[TLBWI, TLBR, TLBP, TLBP, TLBINV, TLBP, HALT], &[HALT], false);
    assert!(cpu.sr_write(SR_TLB_HI, 0x1234_0105)); // VPN 0x1234, G, ASID 5
    assert!(cpu.sr_write(SR_TLB_LO, 0xFFFF_FFFF)); // PFN 0x7FF, X W R V
    assert!(cpu.sr_write(SR_TLB_INDEX, 0x8000_0007)); // P set: TLBWI uses [4:0]
    assert_eq!(cpu.step_one(true), None); // TLBWI
    assert_eq!(
        cpu.tlb[7],
        TlbEntry {
            hi: 0x1234_0105,
            lo: 0x07FF_000F
        }
    );
    let e = cpu.tlb_entries()[7];
    assert!(e.valid() && e.global());
    assert_eq!((e.vpn(), e.asid(), e.pfn(), e.permissions().as_str()), (0x1234, 5, 0x7FF, "rwx"));
    cpu.tlb_hi = 0;
    cpu.tlb_lo = 0;
    assert_eq!(cpu.step_one(true), None); // TLBR
    assert_eq!((cpu.tlb_hi, cpu.tlb_lo), (0x1234_0105, 0x07FF_000F));
    // TLBP hit (a global entry, another ASID): TLB_INDEX = index, P clear.
    cpu.tlb_hi = 0x1234_0009;
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(cpu.tlb_index, 7);
    // TLBP miss: P set, the index bits unchanged.
    cpu.tlb_hi = 0x9999_0005;
    cpu.tlb_index = 3;
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(cpu.tlb_index, 0x8000_0003);
    // TLBINV: every V cleared, the rest kept; TLBP then misses.
    cpu.tlb[0] = TlbEntry { hi: 0x1, lo: 0x3_000F };
    assert_eq!(cpu.step_one(true), None);
    assert!(cpu.tlb_entries().iter().all(|e| !e.valid()));
    assert_eq!(cpu.tlb[0].lo, 0x3_000E);
    cpu.tlb_hi = 0x1234_0005;
    cpu.tlb_index = 2;
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(cpu.tlb_index, 0x8000_0002);
    // A non-global entry matches TLBP only in its own ASID.
    let mut cpu = stage1_cpu(&[TLBP, TLBP, HALT], &[HALT], false);
    cpu.tlb[4] = TlbEntry { hi: 0x0042_0003, lo: TLB_V };
    cpu.tlb_hi = 0x0042_0004;
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(cpu.tlb_index, 0x8000_0000);
    cpu.tlb_hi = 0x0042_0003;
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(cpu.tlb_index, 4);
    // All four are privileged in user mode, and illegal with a field set.
    for word in [TLBWI, TLBR, TLBP, TLBINV] {
        let mut cpu = stage1_cpu(&[word], &[HALT], true);
        assert_eq!(cpu.step_one(true), None);
        assert_entered(&cpu, Cause::Privileged, word, CODE_BASE, true);
        let mut cpu = stage1_cpu(&[word | 0x100], &[HALT], true);
        assert_eq!(cpu.step_one(true), None);
        assert_entered(&cpu, Cause::Illegal, word | 0x100, CODE_BASE, true);
    }
}

#[test]
fn tlb_changes_clear_the_reservation() {
    // TLBWI, TLBINV, MTSR ASID and MTSR STATUS clear it (§11.3); TLBR,
    // TLBP and the other TLB SRs do not.
    for (word, clears) in [
        (TLBWI, true),
        (TLBINV, true),
        (mtsr(SR_ASID, 0), true),
        (mtsr(SR_STATUS, 0), true),
        (TLBR, false),
        (TLBP, false),
        (mtsr(SR_TLB_HI, 0), false),
        (mtsr(SR_TLB_LO, 0), false),
        (mtsr(SR_TLB_INDEX, 0), false),
        (mtsr(SR_TLBVEC, 0), false),
        (mtsr(SR_MPU_BASE0, 0), false),
    ] {
        let mut cpu = stage1_cpu(&[word, HALT], &[HALT], false);
        cpu.set_has_mpu(true);
        cpu.reservation = Some(0x400);
        assert_eq!(cpu.step_one(true), None, "{word:08x}");
        assert_eq!(cpu.reservation.is_none(), clears, "{word:08x}");
    }
    // So an SC after a TLB change fails.
    let code = [lr(LR_D, 3, 1), TLBWI, sc(SC_D, 6, 5, 1), HALT];
    let mut cpu = stage1_cpu(&code, &[HALT], false);
    cpu.regs[1] = 0x2000;
    for _ in 0..3 {
        assert_eq!(cpu.step_one(true), None);
    }
    assert_eq!(cpu.regs[6], 1, "SC failed");
}

#[test]
fn tlb_two_access_instructions_translate_both_pages_first() {
    // MEMGET32 at the last two bytes of page 5 (frame 3): page 6 has no
    // entry, so TLB_MISS_LOAD at its start and nothing loaded.
    let mut cpu = te_cpu(&[LOAD32, HALT], &[IRET], true);
    cpu.asid = 2;
    tlb_map(&mut cpu, 2, U_DATA, DATA_FRAME, TLB_V | TLB_R, Some(2));
    assert!(cpu.debug_write(DATA_FRAME + 0xFFFE, &[0x11, 0x22]));
    assert!(cpu.debug_write(0x9_0000, &[0x33, 0x44]));
    cpu.regs[3] = 0x5555;
    step_with_b(&mut cpu, U_DATA + 0xFFFE);
    assert_entered_te(&cpu, K_HANDLER, Cause::TlbMissLoad, 0x0006_0000, U_CODE + CODE_BASE, true);
    assert_eq!((cpu.regs[3], cpu.tlb_hi), (0x5555, 0x0006_0002));
    // Refill page 6 with frame 9 (not frame 3's neighbour) and retry: the
    // bytes come from both frames.
    tlb_map(&mut cpu, 3, 0x0006_0000, 0x9_0000, TLB_V | TLB_R, Some(2));
    assert_eq!(cpu.step_one(true), None); // IRET
    assert_eq!(cpu.step_one(true), None);
    assert_eq!((cpu.pc, cpu.regs[3]), (U_CODE + CODE_BASE + 4, 0x4433_2211));
    // LEAVERET reading the last doubleword of page 5 and the first of page 6.
    let mut cpu = te_cpu(&[LEAVERET, HALT], &[HALT], true);
    tlb_map(&mut cpu, 2, U_DATA, DATA_FRAME, TLB_V | TLB_R, None);
    cpu.regs[15] = u64::from(U_DATA + 0xFFF8);
    assert_eq!(cpu.step_one(true), None);
    assert_entered_te(&cpu, K_HANDLER, Cause::TlbMissLoad, 0x0006_0000, U_CODE + CODE_BASE, true);
    assert_eq!(cpu.regs[15], u64::from(U_DATA + 0xFFF8), "R15 unchanged");
}

#[test]
fn tlb_cause_priority() {
    // MISALIGNED > ACCESS (segment, MMIO) > TLB_MISS > ACCESS (permission),
    // and for two accesses the lowest faulting byte.  User mode, ASID 0.
    // (word, B, cause, EADDR, TLB_HI loaded)
    for (word, b, cause, eaddr, loads) in [
        (LOAD16, 0x8000_0001, Cause::MisalignedLoad, 0x8000_0001, false), // misaligned > segment
        (LOAD16, 0x0009_0001, Cause::MisalignedLoad, 0x0009_0001, false), // misaligned > miss
        (lr(LR_D, 3, 1), 0x0009_0004, Cause::MisalignedLoad, 0x0009_0004, false),
        (LOAD64, 0x9000_0000, Cause::AccessLoad, 0x9000_0000, false), // segment, no TLB look-up
        // Page 0x7FFF readable: the second doubleword is kseg, a segment fault.
        (LOAD32, 0x7FFF_FFFE, Cause::AccessLoad, 0x8000_0000, false),
        // Page 5 without R and page 6 missing: the first (lower) doubleword's
        // permission fault is reported.
        (LOAD32, U_DATA + 0xFFFE, Cause::AccessLoad, U_DATA + 0xFFFE, true),
        // Page 0xA missing and page 0xB mapped: the first doubleword's miss.
        (LOAD32, 0x000A_FFFE, Cause::TlbMissLoad, 0x000A_FFFE, true),
    ] {
        let mut cpu = te_cpu(&[word, HALT], &[HALT], true);
        tlb_map(&mut cpu, 2, 0x7FFF_0000, 0x4_0000, TLB_V | TLB_R, None);
        tlb_map(&mut cpu, 3, U_DATA, DATA_FRAME, TLB_V | TLB_W, None);
        tlb_map(&mut cpu, 4, 0x000B_0000, 0x5_0000, TLB_V | TLB_R, None);
        cpu.tlb_hi = 0xAAAA_0000;
        step_with_b(&mut cpu, b);
        assert_entered_te(&cpu, K_HANDLER, cause, eaddr, U_CODE + CODE_BASE, true);
        assert_eq!(cpu.tlb_hi == 0xAAAA_0000, !loads, "{word:08x} at 0x{b:08X}: TLB_HI");
    }
    // An LR or SC to MMIO is ACCESS in supervisor mode too, before any
    // translation.
    let mut cpu = te_cpu(&[lr(LR_D, 3, 1), HALT], &[HALT], false);
    step_with_b(&mut cpu, 0xF001_0000);
    assert_entered_te(&cpu, K_HANDLER, Cause::AccessLoad, 0xF001_0000, KSEG + CODE_BASE, false);
    // MISALIGNED_FETCH > a fetch miss.
    let mut cpu = te_cpu(&[HALT], &[HALT], true);
    cpu.pc = 0x0009_0002;
    assert_eq!(cpu.step_one(true), None);
    assert_entered_te(&cpu, K_HANDLER, Cause::MisalignedFetch, 0x0009_0002, 0x0009_0002, true);
}

#[test]
fn tlb_double_fault_on_an_entry_outside_kseg() {
    // Supervisor TRAP with TE = 1 and SP in useg: the frame push has no
    // kseg address, so the CPU stops (park_kind 6) with nothing entered.
    for (sp, entered) in [
        (U_SP, false),
        (KSEG, false), // the push would be at 0x7FFF_FFF8
        (0x8800_0008, false),
        (0x8800_0000, true), // the top of DDR: the push is the last doubleword
        (KSEG + 8, true),
    ] {
        let mut cpu = te_cpu(&[TRAP, HALT], &[HALT], false);
        cpu.sp = sp;
        let stop = cpu.step_one(true);
        if entered {
            assert_eq!((stop, cpu.pc, cpu.sp), (None, K_HANDLER, sp - 8), "SP 0x{sp:08X}");
        } else {
            assert_eq!(stop, Some(StopReason::DoubleFault(sp)), "SP 0x{sp:08X}");
            assert_eq!((cpu.pc, cpu.sp, cpu.ecause, cpu.eaddr), (KSEG + CODE_BASE, sp, 0, 0));
            assert_eq!(cpu.parked_exception(), Some((Cause::Trap, TRAP)));
        }
    }
    // From user mode the kernel SP is SP_ALT; the mode is unchanged.
    let mut cpu = te_cpu(&[SYSCALL, HALT], &[HALT], true);
    cpu.sp_alt = 0x7000;
    assert_eq!(cpu.step_one(true), Some(StopReason::DoubleFault(0x7000)));
    assert!(cpu.um);
    assert_eq!((cpu.sp, cpu.sp_alt, cpu.pc), (U_SP, 0x7000, U_CODE + CODE_BASE));
    // A debugger attached sees the same stop (the board debug-halts).
    let mut cpu = te_cpu(&[LOAD64, HALT], &[HALT], true);
    cpu.set_debugger_attached(true);
    cpu.sp_alt = 0x7000;
    cpu.regs[1] = u64::from(U_DATA);
    assert_eq!(cpu.step_one(true), Some(StopReason::DoubleFault(0x7000)));
    // An interrupt entry too; the interrupt stays pending.
    let mut cpu = te_cpu(&[0x6100_0000], &[IRET], false); // JMP.S .
    cpu.int_vec[0] = K_HANDLER;
    cpu.int_mask = 1;
    cpu.timer_pending = true;
    cpu.sp = 0x7000;
    assert_eq!(cpu.step_one(true), Some(StopReason::DoubleFault(0x7000)));
    assert!(cpu.timer_pending);
    assert_eq!(cpu.int_mask, 1);
    // With TE = 0 there is no such check.
    let mut cpu = stage1_cpu(&[TRAP, HALT], &[HALT], false);
    cpu.sp = 0x7000;
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(cpu.pc, HANDLER);
}

#[test]
fn tlb_turning_translation_on_and_off() {
    // MTSR STATUS with TE: the next fetch is translated.  Boot code at its
    // physical address needs an entry for its own page first (entry 15,
    // global, identity), then jumps to its kseg alias.
    let code = [
        mtsr(SR_STATUS, 0), // A = 0x200: TE on
        JMPR_B,             // B = the kseg alias of the HALT
        HALT,
    ];
    let mut cpu = stage1_cpu(&code, &[HALT], false);
    tlb_map(&mut cpu, 15, 0, 0, TLB_V | TLB_X, None);
    cpu.regs[0] = STATUS_TE;
    step_with_b(&mut cpu, KSEG + CODE_BASE + 8);
    assert!(cpu.te);
    assert_eq!(cpu.step_one(true), None);
    assert_eq!(cpu.pc, KSEG + CODE_BASE + 8);
    assert_eq!(cpu.step_one(true), Some(StopReason::Halt));
    // Without that entry, the fetch after the MTSR misses (with EVEC 0, a
    // park).
    let mut cpu = stage1_cpu(&code, &[HALT], false);
    cpu.evec = 0;
    cpu.regs[0] = STATUS_TE;
    assert_eq!(cpu.step_one(true), None);
    assert!(matches!(cpu.step_one(true), Some(StopReason::InvalidOpcode(_))));
    assert_eq!(cpu.parked_exception(), Some((Cause::TlbMissFetch, CODE_BASE + 4)));
}

#[test]
fn debugger_accesses_translate_with_te_and_are_unchecked() {
    let mut cpu = te_cpu(&[HALT], &[HALT], false);
    tlb_map(&mut cpu, 2, U_DATA, DATA_FRAME, TLB_V | TLB_X, None); // no R or W
    cpu.mpu_en = true;
    assert!(cpu.debug_write(DATA_FRAME + 0x10, &0x1122_3344_u32.to_le_bytes()));
    assert_eq!(cpu.debug_translate(U_DATA + 0x10), Some(DATA_FRAME + 0x10));
    assert_eq!(cpu.debug_translate(KSEG + 0x2000), Some(0x2000));
    assert_eq!(cpu.debug_translate(0xF001_0000), Some(0xF001_0000));
    assert_eq!(cpu.debug_translate(0x9000_0000), None, "the hole");
    assert_eq!(cpu.debug_translate(0x0009_0000), None, "no entry");
    let mut buf = [0_u8; 4];
    assert_eq!(cpu.debug_read_virtual(U_DATA + 0x10, &mut buf), 4);
    assert_eq!(u32::from_le_bytes(buf), 0x1122_3344);
    assert!(cpu.debug_write_virtual(U_DATA + 0x14, &[0xAB]));
    assert_eq!(cpu.peek64(DATA_FRAME + 0x10) >> 32, 0xAB);
    assert_eq!(cpu.fetch_word(KSEG + CODE_BASE), HALT);
    // A read across into an unmapped page stops there; a write that would
    // cross writes nothing.
    let mut buf = [0_u8; 8];
    assert_eq!(cpu.debug_read_virtual(U_DATA + 0xFFFC, &mut buf), 4);
    assert!(!cpu.debug_write_virtual(U_DATA + 0xFFFC, &[1; 8]));
    assert_eq!(cpu.peek64(DATA_FRAME + 0xFFF8) >> 32, 0, "nothing written");
    // With TE = 0 addresses are physical.
    cpu.te = false;
    assert_eq!(cpu.debug_translate(U_DATA), Some(U_DATA));
}

/// Assemble `.kla` source (no file I/O) and return its DDR image and entry.
fn assemble_kla(source: &str) -> (Vec<u8>, u32) {
    use crate::asm::assemble_lines;
    use crate::asm::line::SourceLine;
    use crate::asm::macros::{builtin_macros, expand_embedded_macros};
    let mut msg_list = crate::diag::Diagnostics::new();
    let macro_list = expand_embedded_macros(builtin_macros(), &mut msg_list);
    let input = source
        .lines()
        .zip(1..)
        .map(|(line, n)| SourceLine {
            file_name: "test.kla".to_owned(),
            input_text_line: line.to_owned(),
            line_counter: n,
        })
        .collect();
    let assembly = assemble_lines(input, &crate::isa::Isa::v2(), &macro_list, &mut msg_list);
    assert!(!msg_list.has_errors(), "{:?}", msg_list.messages());
    let image = assembly.image().expect("a _start");
    (image.to_ddr(), image.entry)
}

/// A kernel with a software TLB refill handler, and a user program that
/// runs across several pages (`MMU_PLAN.md` §11.2-§11.3).
///
/// The page table is a flat array of `TLB_LO` values indexed by VPN, at
/// PA `0x2_0000` (kseg `0x8002_0000`); 0 = unmapped.  User code is at VA
/// `0x0040_0000` + its offset in frame 0, data on pages 0x41-0x43 (frames
/// 3-5; page 0x43 starts read-only, for the dirty-bit flow), the stack on
/// page 0x50 (frame 6).  The refill handler at `TLBVEC` copies the PTE
/// into the next entry, round robin (the counter in `SCRATCH1`), and counts
/// refills at PA `0x2_0800`; the `EVEC` handler sets W on a store to a
/// read-only page that may be written (counted at `0x2_0808`) and halts on a
/// SYSCALL.  The kernel, its vectors, its stack and the page table are all
/// in kseg, so the refill path cannot miss.
const REFILL_KLA: &str = "
_start
// The page table (TE is still off: physical addresses).
SETR B 0x20000
SETR C 0x0000000B          // VPN 0x40, user code: frame 0, X R V
STIDX64 C B 0x200
SETR C 0x00030007          // VPN 0x41: frame 3, W R V
STIDX64 C B 0x208
SETR C 0x00040007          // VPN 0x42: frame 4
STIDX64 C B 0x210
SETR C 0x00050003          // VPN 0x43: frame 5, read-only for now
STIDX64 C B 0x218
SETR C 0x00060007          // VPN 0x50: the user stack, frame 6
STIDX64 C B 0x280
// The vectors, at their kseg aliases.
SETR B REFILL:
ORV B 0x80000000
MTSR TLBVEC B
SETR B HANDLER:
ORV B 0x80000000
MTSR evec B
// Map this page (global, identity) so the fetches after TE = 1 work, then
// turn translation on and jump to the kseg alias.
SETR A 0x00000100
MTSR TLB_HI A
SETR A 0x0000000B
MTSR TLB_LO A
SETR A 31
MTSR TLB_INDEX A
TLBWI
SETR B KMAIN:
ORV B 0x80000000
SETR A 0x200
MTSR STATUS A
JMPR B
KMAIN:
GETSP A                    // the kernel stack moves to kseg too
ORV A 0x80000000
SETSP A
SETR A 0                   // drop the boot entry
MTSR TLB_LO A
TLBWI
MTSR SCRATCH1 A            // the refill counter
SETR A 1
MTSR ASID A
SETR A 0x00510000          // the user SP: top of page 0x50
MTSR USP A
SETR A USER:               // enter user mode at USER's VA
ORV A 0x00400000
SETR64 C 0x100000000000    // frame PM = user
ORR A A C
PUSH A
IRET

// TLB refill (TLBVEC).  TLB_HI already holds {VPN, ASID}.
REFILL:
PUSH A
PUSH B
MFSR A TLB_HI
SHRV A 16
SHLV A 3
SETR B 0x80020000
ADDR A A B
MEMGET64 B A               // the PTE
BEQI B 0 FATAL:
MTSR TLB_LO B
MFSR B SCRATCH1
MTSR TLB_INDEX B
TLBWI
ADDV B 1
ANDV B 31
MTSR SCRATCH1 B
SETR A 0x80020800
MEMGET64 B A
ADDV B 1
MEMSET64 B A
POP B
POP A
IRET

// Other exceptions (EVEC).
HANDLER:
PUSH A
PUSH B
MFSR A ECAUSE
BEQI A 4 DONE:             // SYSCALL: the user program is finished
BNEV A 8 FATAL:            // only ACCESS_STORE is handled
MFSR A TLB_HI              // the dirty bit: set W in the PTE and the entry
SHRV A 16
SHLV A 3
SETR B 0x80020000
ADDR A A B
MEMGET64 B A
ORV B 4
MEMSET64 B A
MTSR TLB_LO B
TLBP
TLBWI
SETR A 0x80020808
MEMGET64 B A
ADDV B 1
MEMSET64 B A
POP B
POP A
IRET
DONE:
HALT
FATAL:
TRAP

// The user program (runs at VA 0x0040_0000 + its offset).
USER:
SETR A 0x00410000
SETR B 0x11
MEMSET64 B A               // page 0x41: miss, refill, store
SETR A 0x00420008
SETR B 0x22
MEMSET64 B A               // page 0x42
SETR A 0x0042FFFE
MEMGET32 C A               // spans pages 0x42 and 0x43: the second misses
SETR A 0x00430000
SETR B 0x33
MEMSET64 B A               // page 0x43 is read-only: ACCESS_STORE, then W
PUSH B                     // page 0x50: the stack
POP D
CALLREL USUB:
SETR A 0x00410000
MEMGET64 E A               // a TLB hit now
SETR A 0
SYSCALL
USUB:
ADDV D 1
RET
";

#[test]
fn tlb_software_refill_runs_a_user_program_across_pages() {
    let (image, entry) = assemble_kla(REFILL_KLA);
    let mut cpu = Cpu::with_mem_size(&image, entry, TEST_MEM);
    // The bytes the spanning MEMGET32 reads, at the end of frame 4 and the
    // start of frame 5.
    assert!(cpu.debug_write(0x4_FFFE, &[0xAA, 0xBB]));
    assert!(cpu.debug_write(0x5_0000, &[0xCC, 0xDD]));
    let r = cpu.run(10_000, None);
    assert_eq!(
        r.stop,
        StopReason::Halt,
        "pc 0x{:08X} ECAUSE {} EADDR 0x{:08X}",
        cpu.pc,
        cpu.ecause,
        cpu.eaddr
    );
    assert!(cpu.te && !cpu.um, "halted in the kernel, translation on");
    assert_eq!(cpu.ecause, Cause::Syscall as u8);
    // Refills: the code page, 0x41, 0x42, 0x43 (the MEMGET32's second
    // doubleword) and the stack; one dirty-bit fault.
    assert_eq!((cpu.peek64(0x2_0800), cpu.peek64(0x2_0808)), (5, 1));
    assert_eq!(cpu.peek64(0x2_0218), 0x0005_0007, "the PTE is now writable");
    assert_eq!(cpu.peek64(0x3_0000), 0x11);
    assert_eq!(cpu.peek64(0x4_0008), 0x22);
    assert_eq!(cpu.peek64(0x5_0000) & 0xFF, 0x33);
    assert_eq!(cpu.regs[2], 0xDDCC_BBAA, "C: the MEMGET32 across the pages");
    assert_eq!(cpu.regs[3], 0x34, "D: the stack page and CALLREL/RET");
    assert_eq!(cpu.regs[4], 0x11, "E: page 0x41 read back");
    assert_eq!(cpu.system_regs().asid, 1);
    // The entries the refills wrote (round robin from 0), in ASID 1.
    let vpns: Vec<u32> = cpu.tlb_entries()[..5].iter().map(|e| e.vpn()).collect();
    assert_eq!(vpns, [0x40, 0x41, 0x42, 0x43, 0x50]);
    assert!(cpu.tlb_entries()[..5].iter().all(|e| e.valid() && !e.global() && e.asid() == 1));
}

#[test]
fn mmio_fetch_with_mpu_or_te_is_a_fake_fill() {
    // klaussCPU mmu 64bacdf (§10.2): with MPU_EN or TE set, a fetch fill
    // from MMIO is zeros, so a supervisor PC there takes ILLEGAL (EADDR = the
    // word, 0) instead of reading a device.  With both off it stays outside
    // RAM (stage1_each_cause_from_supervisor_and_user_mode).
    let mut cpu = mpu_cpu(&[NOP], &[HALT], false);
    cpu.pc = 0xF000_0000;
    assert_eq!(cpu.step_one(true), None);
    assert_entered(&cpu, Cause::Illegal, 0, 0xF000_0000, false);
    let mut cpu = te_cpu(&[NOP], &[HALT], false);
    cpu.pc = 0xF00F_0048;
    assert_eq!(cpu.step_one(true), None);
    assert_entered_te(&cpu, K_HANDLER, Cause::Illegal, 0, 0xF00F_0048, false);
    // The fetch faults outrank ILLEGAL (§4.1): a GUARD region over MMIO,
    let mut cpu = mpu_cpu(&[NOP], &[HALT], false);
    mpu_region(&mut cpu, 0, 0xF000_0000, 0xF000_00FF, MPU_GUARD);
    cpu.pc = 0xF000_0000;
    assert_eq!(cpu.step_one(true), None);
    assert_mpu_fault(&cpu, Cause::AccessFetch, 0xF000_0000, 0xF000_0000, false, true);
    // a user PC there with the MPU on (the stage 1 MMIO rule),
    let mut cpu = mpu_cpu(&[NOP], &[HALT], true);
    cpu.pc = 0xF000_0000;
    assert_eq!(cpu.step_one(true), None);
    assert_mpu_fault(&cpu, Cause::AccessFetch, 0xF000_0000, 0xF000_0000, true, false);
    // and with TE = 1 (the segment rule).
    let mut cpu = te_cpu(&[NOP], &[HALT], true);
    cpu.pc = 0xF000_0000;
    assert_eq!(cpu.step_one(true), None);
    assert_entered_te(&cpu, K_HANDLER, Cause::AccessFetch, 0xF000_0000, 0xF000_0000, true);
}

#[test]
fn watchpoints_compare_the_physical_address() {
    // klaussCPU mmu 5e9338d: with TE = 1 a watchpoint matches the PA, like
    // the debugger's own accesses.  kseg 0x8000_2000 is PA 0x2000.
    for (watch, hit) in [(0x2000, true), (0x8000_2000, false)] {
        let mut cpu = te_cpu(&[LOAD64, HALT], &[HALT], false);
        cpu.add_watch(Watch {
            addr: watch,
            len: 8,
            kind: WatchKind::Read,
        });
        step_with_b(&mut cpu, 0x8000_2000);
        assert_eq!(cpu.take_watch_hit().is_some(), hit, "watch 0x{watch:08X}");
    }
}

#[test]
fn tlb_len00_word_at_a_page_end_is_illegal_not_a_next_page_fault() {
    // LEN = 00 is a one-word (ILLEGAL) instruction: in the last word of a
    // page it must not translate the next page (stage 3 review; the RTL had
    // it wrong).  The page after U_CODE's is left unmapped.
    let at = U_CODE + 0xFFFC;
    let mut cpu = te_cpu(&[NOP], &[HALT], true);
    cpu.tlb[1] = TlbEntry::default();
    cpu.pc = at;
    assert_eq!(cpu.step_one(true), None);
    assert_entered_te(&cpu, K_HANDLER, Cause::Illegal, 0, at, true);
    // A two-word instruction there does fetch the next page: TLB_MISS_FETCH
    // with EADDR = that page's first word.
    let mut cpu = te_cpu(&[NOP], &[HALT], true);
    cpu.tlb[1] = TlbEntry::default();
    assert!(cpu.debug_write(0xFFFC, &0x8000_0000_u32.to_le_bytes()));
    cpu.pc = at;
    assert_eq!(cpu.step_one(true), None);
    assert_entered_te(&cpu, K_HANDLER, Cause::TlbMissFetch, U_CODE + 0x1_0000, at, true);
}

#[test]
fn uart_script_types_one_byte_at_a_time_into_an_empty_fifo() {
    // JMP.S . forever; the script moves a byte into the RX FIFO only when it
    // is empty and UART_SCRIPT_GAP instructions have passed (--uart-in).
    let mut cpu = cpu_with(&image_from_words(&[0x6100_0000]));
    cpu.set_uart_script(b"ab");
    let gap = crate::emu::UART_SCRIPT_GAP;
    let _ = cpu.run(gap, None);
    assert!(cpu.uart_rx.is_empty(), "nothing before the first gap has passed");
    let _ = cpu.run(1, None);
    assert_eq!(cpu.uart_rx, [b'a'], "the first byte after the gap");
    let _ = cpu.run(3 * gap, None);
    assert_eq!(cpu.uart_rx, [b'a'], "the next waits until the FIFO is read");
    cpu.uart_rx.clear();
    let _ = cpu.run(gap + 1, None);
    assert_eq!(cpu.uart_rx, [b'b']);
    assert!(cpu.uart_script.is_empty());
}
