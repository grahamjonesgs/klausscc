//! Unit tests for the SD controller and card, driven through the CPU's
//! MMIO as a program would drive them.

use super::card::{BLOCK, RCA, crc7, crc16};
use super::*;
use crate::emu::{Cpu, Hardware};
use crate::layout::CODE_BASE;

/// The SD window.
const SD: u32 = 0xF000_0000;
/// `CPU_FEATURES`.
const CPU_FEATURES: u32 = 0xF00F_0048;
/// `INT_MASK`.
const INT_MASK: u32 = 0xF00F_0000;
/// `INT_PENDING`.
const INT_PENDING: u32 = 0xF00F_0008;
/// `INT_VEC2`.
const INT_VEC2: u32 = 0xF00F_0020;
/// `INT2_EN`.
const INT2_EN: u32 = 0xF00F_0050;
/// `INT2_PENDING`.
const INT2_PENDING: u32 = 0xF00F_0058;
/// Blocks on the test card: 1 MiB.
const TEST_BLOCKS: usize = 2048;
/// SPI clock divider for the data phase (25 MHz), as the drivers use.
const CLK_FAST: u64 = 1;
/// SPI clock divider for initialisation (200 kHz).
const CLK_INIT: u64 = 249;
/// SD-mode clock divider for identification (400 kHz).
const CLK_IDENT: u64 = 124;

/// Response types for `SDM_CMD[10:8]`.
const R_NONE: u64 = 0;
const R1: u64 = 1;
const R1B: u64 = 2;
const R2: u64 = 3;
const R3: u64 = 4;

/// The test card's contents: block n holds bytes `(n * 7 + i) as u8`.
fn pattern(lba: usize) -> Vec<u8> {
    (0..BLOCK).map(|i| (lba * 7 + i) as u8).collect()
}

fn image() -> Vec<u8> {
    (0..TEST_BLOCKS).flat_map(pattern).collect()
}

/// A CPU with a small RAM and the given SD hardware.
fn cpu(card: bool, native: bool) -> Cpu {
    let mut cpu = Cpu::with_mem_size(&[0; 0x100], CODE_BASE, 1 << 16);
    cpu.install(Hardware {
        mpu: false,
        tlb_entries: None,
        no_tlb_walker: false,
        sd_card: card.then(|| SdCard::from_bytes(image()).unwrap()),
        sd_native: native,
        sd_dma: false,
        sd_no_hs: false,
        sd_hs_window: None,
        dcache_range_ops: false,
    });
    cpu
}

fn rd(cpu: &mut Cpu, offset: u32) -> u64 {
    cpu.mmio_read(SD + offset)
}

fn wr(cpu: &mut Cpu, offset: u32, value: u64) {
    cpu.mmio_write(SD + offset, value);
}

/// One instruction's worth of modelled time.
fn tick(cpu: &mut Cpu) {
    cpu.tick(crate::emu::CYCLES_PER_INSTRUCTION);
}

/// The image as the card holds it now.
fn card_bytes(cpu: &Cpu) -> Vec<u8> {
    cpu.sd.as_ref().unwrap().card.as_ref().unwrap().memory().to_vec()
}

fn card(cpu: &Cpu) -> &SdCard {
    cpu.sd.as_ref().unwrap().card.as_ref().unwrap()
}

// ---- SPI mode, as Zephyr's, Linux's and the bare-metal drivers drive it ----

/// `spi_xfer`: start the byte, poll BUSY, read it.
fn xfer(cpu: &mut Cpu, tx: u8) -> u8 {
    wr(cpu, SD_DATA, u64::from(tx));
    tick(cpu);
    while rd(cpu, SD_STATUS) & STATUS_BUSY != 0 {
        tick(cpu);
    }
    tick(cpu);
    rd(cpu, SD_DATA) as u8
}

fn cs(cpu: &mut Cpu, low: bool) {
    let ctrl = (rd(cpu, SD_CTRL) & 0xFFFF) | CTRL_PWR_ON | if low { 0 } else { CTRL_CS_HI };
    wr(cpu, SD_CTRL, ctrl);
}

/// `sd_cmd`: wait ready (except CMD0), the frame, then up to 8 bytes for R1.
fn spi_cmd(cpu: &mut Cpu, index: u8, arg: u32) -> u8 {
    if index == 0 {
        xfer(cpu, 0xFF);
    } else {
        let mut tries = 0;
        while xfer(cpu, 0xFF) != 0xFF {
            tries += 1;
            assert!(tries < 100, "card never ready before CMD{index}");
        }
    }
    let frame = [0x40 | index, (arg >> 24) as u8, (arg >> 16) as u8, (arg >> 8) as u8, arg as u8];
    for b in frame {
        xfer(cpu, b);
    }
    xfer(cpu, (crc7(&frame) << 1) | 1);
    for _ in 0..8 {
        let r = xfer(cpu, 0xFF);
        if r & 0x80 == 0 {
            return r;
        }
    }
    0xFF
}

/// `sd_hw_init`, checking each step; returns the OCR.
fn spi_init(cpu: &mut Cpu) -> u32 {
    wr(cpu, SD_CTRL, CLK_INIT | CTRL_CS_HI | CTRL_PWR_ON);
    for _ in 0..10 {
        xfer(cpu, 0xFF);
    }
    cs(cpu, true);
    assert_eq!(spi_cmd(cpu, 0, 0), 0x01, "CMD0");
    assert_eq!(spi_cmd(cpu, 8, 0x1AA), 0x01, "CMD8");
    let r7: Vec<u8> = (0..4).map(|_| xfer(cpu, 0xFF)).collect();
    assert_eq!(r7, [0x00, 0x00, 0x01, 0xAA], "R7 echoes the voltage and check pattern");
    let mut polls = 0;
    loop {
        assert_eq!(spi_cmd(cpu, 55, 0), 0x01, "CMD55 in idle");
        polls += 1;
        match spi_cmd(cpu, 41, 0x4000_0000) {
            0x00 => break,
            0x01 => assert!(polls < 10),
            r => panic!("ACMD41 R1 0x{r:02X}"),
        }
    }
    assert_eq!(polls, 2, "ready on the second ACMD41");
    assert_eq!(spi_cmd(cpu, 58, 0), 0x00, "CMD58");
    let ocr = u32::from_be_bytes(std::array::from_fn(|_| xfer(cpu, 0xFF)));
    cs(cpu, false);
    let ctrl = (rd(cpu, SD_CTRL) & !0xFFFF) | CLK_FAST;
    wr(cpu, SD_CTRL, ctrl);
    ocr
}

/// Wait for a data token: 0xFE, or the error token.
fn spi_token(cpu: &mut Cpu) -> u8 {
    for _ in 0..100 {
        let t = xfer(cpu, 0xFF);
        if t != 0xFF {
            return t;
        }
    }
    panic!("no data token");
}

/// Zephyr's `sd_rx_block`: the plain loop, or the pipelined one, which
/// returns `None` if it saw a late read.
fn rx_block(cpu: &mut Cpu, pipe: bool) -> Option<Vec<u8>> {
    if !pipe {
        return Some((0..BLOCK).map(|_| xfer(cpu, 0xFF)).collect());
    }
    let mut buf = vec![0; BLOCK];
    let mut late = false;
    wr(cpu, SD_DATA, 0xFF);
    tick(cpu);
    for b in buf.iter_mut().take(BLOCK - 1) {
        while rd(cpu, SD_STATUS) & STATUS_BUSY != 0 {
            tick(cpu);
        }
        tick(cpu);
        wr(cpu, SD_DATA, 0xFF); // start byte i+1
        tick(cpu);
        *b = rd(cpu, SD_DATA) as u8; // byte i
        tick(cpu);
        let st = rd(cpu, SD_STATUS); // i+1 still shifting?
        tick(cpu);
        late |= st & STATUS_BUSY == 0;
    }
    while rd(cpu, SD_STATUS) & STATUS_BUSY != 0 {
        tick(cpu);
    }
    buf[BLOCK - 1] = rd(cpu, SD_DATA) as u8;
    (!late).then_some(buf)
}

/// Zephyr's `sd_tx_block` (plain or pipelined).
fn tx_block(cpu: &mut Cpu, data: &[u8], pipe: bool) {
    for &b in data {
        if pipe {
            while rd(cpu, SD_STATUS) & STATUS_BUSY != 0 {
                tick(cpu);
            }
            wr(cpu, SD_DATA, u64::from(b));
            tick(cpu);
        } else {
            xfer(cpu, b);
        }
    }
    while rd(cpu, SD_STATUS) & STATUS_BUSY != 0 {
        tick(cpu);
    }
}

fn spi_wait_not_busy(cpu: &mut Cpu) -> usize {
    let mut busy = 0;
    while xfer(cpu, 0xFF) == 0x00 {
        busy += 1;
        assert!(busy < 100);
    }
    busy
}

/// CMD17 for `lba`.
fn spi_read(cpu: &mut Cpu, lba: u32, pipe: bool) -> Vec<u8> {
    cs(cpu, true);
    assert_eq!(spi_cmd(cpu, 17, lba), 0x00, "CMD17");
    assert_eq!(spi_token(cpu), 0xFE);
    let block = rx_block(cpu, pipe).expect("no late read");
    let crc = u16::from_be_bytes([xfer(cpu, 0xFF), xfer(cpu, 0xFF)]);
    assert_eq!(crc, crc16(&block), "the data block's CRC16");
    cs(cpu, false);
    block
}

/// CMD24 of `data` to `lba`.
fn spi_write(cpu: &mut Cpu, lba: u32, data: &[u8], pipe: bool) {
    cs(cpu, true);
    assert_eq!(spi_cmd(cpu, 24, lba), 0x00, "CMD24");
    xfer(cpu, 0xFF);
    xfer(cpu, 0xFE);
    tx_block(cpu, data, pipe);
    xfer(cpu, 0xFF);
    xfer(cpu, 0xFF);
    assert_eq!(xfer(cpu, 0xFF) & 0x1F, 0x05, "data accepted");
    assert!(spi_wait_not_busy(cpu) > 0, "the card is busy, then not");
    cs(cpu, false);
}

#[test]
fn crcs_match_known_values() {
    // CMD0 and CMD8(0x1AA) frames end in the well-known 0x95 and 0x87.
    assert_eq!((crc7(&[0x40, 0, 0, 0, 0]) << 1) | 1, 0x95);
    assert_eq!((crc7(&[0x48, 0, 0, 1, 0xAA]) << 1) | 1, 0x87);
    // A block of 0xFF has CRC16 0x7FA1.
    assert_eq!(crc16(&[0xFF; 512]), 0x7FA1);
}

#[test]
fn no_card_reads_zero_as_before() {
    // Without --sd-image and --sd-native there is no SD controller: the
    // window reads 0 and ignores writes, as it always has.
    let mut cpu = Cpu::with_mem_size(&[0; 0x100], CODE_BASE, 1 << 16);
    assert!(cpu.sd.is_none());
    for offset in [SD_CTRL, SD_DATA, SD_STATUS, SDM_CMD, SDM_STATUS, SDM_CAPS, 0x200, 0x3F8] {
        wr(&mut cpu, offset, u64::MAX);
        assert_eq!(rd(&mut cpu, offset), 0, "offset 0x{offset:03X}");
    }
    assert_eq!(cpu.mmio_read(CPU_FEATURES), 0x200D | (3 << 22) | (1 << 25));
    cpu.mmio_write(INT2_EN, 3);
    assert_eq!(cpu.mmio_read(INT2_EN), 0);
    assert_eq!(cpu.mmio_read(INT2_PENDING), 0);
    assert_eq!(cpu.mmio_read(INT_PENDING), 0);
}

#[test]
fn spi_controller_without_native_matches_release_bitstream() {
    let mut cpu = cpu(true, false);
    // sd_spi.sv's reset values and read-back.
    assert_eq!(rd(&mut cpu, SD_CTRL), CLK_INIT | CTRL_CS_HI);
    assert_eq!(rd(&mut cpu, SD_DATA), 0xFF);
    assert_eq!(rd(&mut cpu, SD_STATUS), 0, "card detect is active low: a card is in");
    assert_eq!(rd(&mut cpu, SDM_STATUS), 0, "no native controller");
    wr(&mut cpu, SD_CTRL, 0xFFFF_FFFF);
    assert_eq!(rd(&mut cpu, SD_CTRL), 0x3_FFFF, "no SD_MODE bit");
    // The native registers and the cascade are absent.
    for offset in [SDM_ARG, SDM_STATUS, SDM_TIMEOUT, SDM_CAPS, SDM_FIFO] {
        wr(&mut cpu, offset, 0x1234);
        assert_eq!(rd(&mut cpu, offset), 0, "offset 0x{offset:03X}");
    }
    assert_eq!(cpu.mmio_read(CPU_FEATURES), 0x200D | (3 << 22) | (1 << 25));
    cpu.mmio_write(INT2_EN, 3);
    assert_eq!(cpu.mmio_read(INT2_EN), 0);
    // SD_BUF is plain RAM, byte-addressable like RAM.
    wr(&mut cpu, 0x200, 0x1122_3344_5566_7788);
    wr(&mut cpu, 0x3F8, 0xAA);
    assert_eq!(rd(&mut cpu, 0x200), 0x1122_3344_5566_7788);
    assert_eq!(rd(&mut cpu, 0x3F8), 0xAA);
    cpu.mmio_store(SD + 0x201, 0xEE, 1);
    assert_eq!(rd(&mut cpu, 0x200), 0x1122_3344_5566_EE88);
    assert_eq!(rd(&mut cpu, 0x206) & 0xFF, 0x22);
    // An empty slot: no card, MISO high.
    let mut empty = cpu_without_card();
    assert_eq!(rd(&mut empty, SD_STATUS), STATUS_NO_CARD, "the raw pin is high with no card");
    assert_eq!(rd(&mut empty, SDM_STATUS) as u32 & ST_CARD_PRESENT, 0);
    wr(&mut empty, SD_CTRL, CTRL_PWR_ON);
    assert_eq!(xfer(&mut empty, 0x40), 0xFF);
}

fn cpu_without_card() -> Cpu {
    cpu(false, true)
}

#[test]
fn spi_init_reports_sdhc_and_csd_capacity() {
    let mut cpu = cpu(true, false);
    assert_eq!(spi_init(&mut cpu), 0xC0FF_8000, "OCR: ready, CCS, 2.7-3.6 V");
    assert!(card(&cpu).in_spi_mode());
    // CMD9: a CSD 2.0 whose C_SIZE gives the 1 MiB image.
    cs(&mut cpu, true);
    assert_eq!(spi_cmd(&mut cpu, 9, 0), 0x00);
    assert_eq!(spi_token(&mut cpu), 0xFE);
    let csd: Vec<u8> = (0..16).map(|_| xfer(&mut cpu, 0xFF)).collect();
    xfer(&mut cpu, 0xFF);
    xfer(&mut cpu, 0xFF);
    assert_eq!(csd[0] >> 6, 1, "CSD version 2.0");
    let c_size = (u32::from(csd[7] & 0x3F) << 16) | (u32::from(csd[8]) << 8) | u32::from(csd[9]);
    assert_eq!((c_size + 1) * 1024, TEST_BLOCKS as u32, "Zephyr's sector count");
    assert_eq!(csd[15], (crc7(&csd[..15]) << 1) | 1, "CRC7 and end bit");
    // CMD10: the CID.
    assert_eq!(spi_cmd(&mut cpu, 10, 0), 0x00);
    assert_eq!(spi_token(&mut cpu), 0xFE);
    let cid: Vec<u8> = (0..16).map(|_| xfer(&mut cpu, 0xFF)).collect();
    assert_eq!(&cid[3..8], b"KLEMU");
    xfer(&mut cpu, 0xFF);
    xfer(&mut cpu, 0xFF);
    // CMD13 (R2), CMD16, an unknown command, and an address past the end.
    assert_eq!(spi_cmd(&mut cpu, 13, 0), 0x00);
    assert_eq!(xfer(&mut cpu, 0xFF), 0x00);
    assert_eq!(spi_cmd(&mut cpu, 16, 512), 0x00);
    assert_eq!(spi_cmd(&mut cpu, 42, 0), 0x04, "illegal command");
    assert_eq!(spi_cmd(&mut cpu, 17, TEST_BLOCKS as u32), 0x40, "parameter error");
    assert_eq!(spi_cmd(&mut cpu, 24, TEST_BLOCKS as u32), 0x40, "parameter error");
    cs(&mut cpu, false);
}

#[test]
fn spi_commands_before_init_are_refused() {
    let mut cpu = cpu(true, false);
    wr(&mut cpu, SD_CTRL, CLK_INIT | CTRL_CS_HI | CTRL_PWR_ON);
    cs(&mut cpu, true);
    // Before CMD0 the card is in SD mode and ignores SPI frames.
    assert_eq!(spi_cmd(&mut cpu, 8, 0x1AA), 0xFF);
    assert_eq!(spi_cmd(&mut cpu, 0, 0), 0x01);
    assert_eq!(spi_cmd(&mut cpu, 17, 0), 0x05, "idle and illegal");
    // ACMD41 without HCS never finishes on an SDHC card.
    for _ in 0..5 {
        spi_cmd(&mut cpu, 55, 0);
        assert_eq!(spi_cmd(&mut cpu, 41, 0), 0x01);
    }
    // CS high: the card ignores the bus.
    cs(&mut cpu, false);
    assert_eq!(spi_cmd(&mut cpu, 0, 0), 0xFF);
}

#[test]
fn spi_single_block_read_and_write_plain_and_pipelined() {
    let mut cpu = cpu(true, false);
    spi_init(&mut cpu);
    for pipe in [false, true] {
        assert_eq!(spi_read(&mut cpu, 5, pipe), pattern(5));
        assert_eq!(spi_read(&mut cpu, TEST_BLOCKS as u32 - 1, pipe), pattern(TEST_BLOCKS - 1));
        let data: Vec<u8> = (0..BLOCK).map(|i| (i as u8) ^ if pipe { 0xA5 } else { 0x5A }).collect();
        spi_write(&mut cpu, 9, &data, pipe);
        assert_eq!(&card_bytes(&cpu)[9 * BLOCK..10 * BLOCK], &data[..]);
        assert_eq!(spi_read(&mut cpu, 9, pipe), data);
        assert_eq!(&card_bytes(&cpu)[8 * BLOCK..9 * BLOCK], &pattern(8)[..], "neighbours untouched");
        assert_eq!(&card_bytes(&cpu)[10 * BLOCK..11 * BLOCK], &pattern(10)[..]);
    }
    // ACMD22 in SPI mode: R1, then a 4-byte data block, big-endian.
    cs(&mut cpu, true);
    assert_eq!(spi_cmd(&mut cpu, 55, 0), 0x00);
    assert_eq!(spi_cmd(&mut cpu, 22, 0), 0x00);
    assert_eq!(spi_token(&mut cpu), 0xFE);
    let count: Vec<u8> = (0..4).map(|_| xfer(&mut cpu, 0xFF)).collect();
    assert_eq!(count, [0, 0, 0, 1], "the last CMD24 wrote one block");
    xfer(&mut cpu, 0xFF);
    xfer(&mut cpu, 0xFF);
    cs(&mut cpu, false);
}

#[test]
fn spi_multi_block_read_and_write() {
    let mut cpu = cpu(true, false);
    spi_init(&mut cpu);
    for pipe in [false, true] {
        // CMD25 + ACMD23, three blocks, the stop token (sd_write_multi).
        let blocks: Vec<Vec<u8>> = (0..3).map(|n| vec![0x30 + n as u8 + u8::from(pipe) * 8; BLOCK]).collect();
        cs(&mut cpu, true);
        assert_eq!(spi_cmd(&mut cpu, 55, 0), 0x00);
        assert_eq!(spi_cmd(&mut cpu, 23, 3), 0x00);
        assert_eq!(spi_cmd(&mut cpu, 25, 100), 0x00);
        for block in &blocks {
            xfer(&mut cpu, 0xFF);
            xfer(&mut cpu, 0xFC);
            tx_block(&mut cpu, block, pipe);
            xfer(&mut cpu, 0xFF);
            xfer(&mut cpu, 0xFF);
            assert_eq!(xfer(&mut cpu, 0xFF) & 0x1F, 0x05);
            spi_wait_not_busy(&mut cpu);
        }
        spi_wait_not_busy(&mut cpu);
        xfer(&mut cpu, 0xFD);
        xfer(&mut cpu, 0xFF);
        assert!(spi_wait_not_busy(&mut cpu) > 0);
        cs(&mut cpu, false);
        assert_eq!(card_bytes(&cpu)[100 * BLOCK..103 * BLOCK], blocks.concat()[..]);

        // CMD18, four blocks, then CMD12 mid-stream (sd_read_multi).
        cs(&mut cpu, true);
        assert_eq!(spi_cmd(&mut cpu, 18, 99), 0x00);
        let mut got = Vec::new();
        for _ in 0..4 {
            assert_eq!(spi_token(&mut cpu), 0xFE);
            got.extend(rx_block(&mut cpu, pipe).expect("no late read"));
            xfer(&mut cpu, 0xFF);
            xfer(&mut cpu, 0xFF);
        }
        for b in [0x4C, 0, 0, 0, 0, 0x61, 0xFF] {
            xfer(&mut cpu, b);
        }
        let r1 = (0..8).map(|_| xfer(&mut cpu, 0xFF)).find(|r| r & 0x80 == 0);
        assert_eq!(r1, Some(0x00), "CMD12's R1 after the stuff byte");
        spi_wait_not_busy(&mut cpu);
        cs(&mut cpu, false);
        assert!(got == [pattern(99), blocks.concat()].concat(), "CMD18 read back the blocks");
        // The card is back in tran: a plain read works.
        assert_eq!(spi_read(&mut cpu, 0, pipe), pattern(0));
    }
    // A CMD18 that runs off the end gets the out-of-range error token.
    cs(&mut cpu, true);
    assert_eq!(spi_cmd(&mut cpu, 18, TEST_BLOCKS as u32 - 1), 0x00);
    assert_eq!(spi_token(&mut cpu), 0xFE);
    rx_block(&mut cpu, false);
    xfer(&mut cpu, 0xFF);
    xfer(&mut cpu, 0xFF);
    assert_eq!(spi_token(&mut cpu), 0x08);
    cs(&mut cpu, false);
}

#[test]
fn spi_busy_follows_the_clock_when_nobody_polls() {
    let mut cpu = cpu(true, false);
    spi_init(&mut cpu);
    cs(&mut cpu, true);
    // At clkdiv 1 an exchange takes 33 cycles: a DATA write while it runs
    // is ignored (as in the RTL), and DATA keeps the previous byte.
    wr(&mut cpu, SD_DATA, 0xFF);
    let before = rd(&mut cpu, SD_DATA);
    wr(&mut cpu, SD_DATA, 0x40);
    cpu.tick(40);
    assert_eq!(rd(&mut cpu, SD_STATUS) & STATUS_BUSY, 0, "done after 16 * (div + 1) + 1 cycles");
    assert_eq!(rd(&mut cpu, SD_DATA), 0xFF);
    assert_eq!(before, 0x00, "the OCR's last byte until the exchange ends");
    // The ignored 0x40 did not start a command frame: CMD13 still works.
    assert_eq!(spi_cmd(&mut cpu, 13, 0), 0x00);
    // A DATA read right after the write still sees the previous byte; the
    // received one lands when the exchange ends.
    let mut fresh = cpu_with_init();
    cs(&mut fresh, true);
    let r1 = spi_cmd(&mut fresh, 13, 0);
    assert_eq!(r1, 0x00);
    wr(&mut fresh, SD_DATA, 0xFF); // the R2's second byte
    assert_eq!(rd(&mut fresh, SD_DATA), 0x00, "still R1");
    assert_eq!(rd(&mut fresh, SD_STATUS) & STATUS_BUSY, STATUS_BUSY);
    assert_eq!(rd(&mut fresh, SD_STATUS) & STATUS_BUSY, 0);
}

fn cpu_with_init() -> Cpu {
    let mut cpu = cpu(true, false);
    spi_init(&mut cpu);
    cpu
}

#[test]
fn spi_power_off_resets_the_card() {
    let mut cpu = cpu_with_init();
    wr(&mut cpu, SD_CTRL, CLK_INIT | CTRL_CS_HI);
    wr(&mut cpu, SD_CTRL, CLK_INIT | CTRL_CS_HI | CTRL_PWR_ON);
    assert!(!card(&cpu).in_spi_mode());
    spi_init(&mut cpu);
}

#[test]
fn spi_card_in_a_file() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    std::io::Write::write_all(&mut file, &image()).unwrap();
    let mut cpu = Cpu::with_mem_size(&[0; 0x100], CODE_BASE, 1 << 16);
    cpu.install(Hardware {
        sd_card: Some(SdCard::open(file.path()).unwrap()),
        ..Hardware::default()
    });
    spi_init(&mut cpu);
    assert_eq!(spi_read(&mut cpu, 3, true), pattern(3));
    spi_write(&mut cpu, 4, &[0xC3; BLOCK], true);
    assert_eq!(spi_read(&mut cpu, 4, false), [0xC3; BLOCK]);
    assert!(cpu.take_sd_error().is_none());
    let on_disk = std::fs::read(file.path()).unwrap();
    assert_eq!(on_disk[4 * BLOCK..5 * BLOCK], [0xC3; BLOCK]);
    assert_eq!(on_disk[5 * BLOCK..6 * BLOCK], pattern(5)[..]);

    // Sizes the card cannot present.
    for len in [0, 1000, 512 * 1023] {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(len).unwrap();
        assert!(SdCard::open(file.path()).is_err(), "{len} bytes");
    }
    let odd = SdCard::from_bytes(vec![0; 512 * 1025]).unwrap();
    assert!(odd.capacity_note().is_some());
    assert!(SdCard::from_bytes(image()).unwrap().capacity_note().is_none());
}

// ---- native SD mode ----

/// An `SDM_CMD` value: index, response type, flags.
const fn cmd_word(index: u64, rtype: u64, flags: u64) -> u64 {
    index | (rtype << 8) | flags
}

/// Clear the sticky bits, write `SDM_ARG` and `SDM_CMD`, return `SDM_STATUS`.
fn sdm(cpu: &mut Cpu, index: u64, arg: u32, rtype: u64, flags: u64) -> u32 {
    wr(cpu, SDM_STATUS, u64::from(ST_STICKY | ST_DMA_ERR));
    wr(cpu, SDM_ARG, u64::from(arg));
    wr(cpu, SDM_CMD, index | (rtype << 8) | flags);
    let status = rd(cpu, SDM_STATUS) as u32;
    assert_ne!(status & ST_CMD_DONE, 0, "CMD{index} done");
    status
}

/// A good command: `CMD_DONE` and no error; returns `SDM_RESP0`.
fn sdm_ok(cpu: &mut Cpu, index: u64, arg: u32, rtype: u64, flags: u64) -> u64 {
    let status = sdm(cpu, index, arg, rtype, flags);
    assert_eq!(status & (ST_ERRORS | ST_ERR_ANY), 0, "CMD{index}: status 0x{status:08X}");
    rd(cpu, SDM_RESP0)
}

/// The card status in an R1 response: `CURRENT_STATE`.
fn state_of(resp0: u64) -> u64 {
    (resp0 >> 9) & 0xF
}

/// Pop `n` words from the FIFO.
fn pop(cpu: &mut Cpu, n: usize) -> Vec<u8> {
    (0..n).flat_map(|_| rd(cpu, SDM_DATA).to_le_bytes()).collect()
}

/// Power-cycle into SD mode and identify the card, as `SD_NATIVE_PLAN` §4.1
/// and the Linux mmc core do, ending selected in tran with a 4-bit bus.
fn native_init(cpu: &mut Cpu) {
    assert_eq!(cpu.mmio_read(CPU_FEATURES) & !(1 << 27), 0x200D | (1 << 21) | (3 << 22) | (1 << 25));
    assert_eq!(rd(cpu, SDM_CAPS) & !CAPS_DMA & !CAPS_HS, 0x0008_0101);
    wr(cpu, SD_CTRL, CLK_IDENT | CTRL_SD_MODE);
    wr(cpu, SD_CTRL, CLK_IDENT | CTRL_SD_MODE | CTRL_PWR_ON);
    wr(cpu, SDM_CFG, 0b100); // CLK_ON: the 74 init clocks
    wr(cpu, SDM_CFG, 0);
    sdm_ok(cpu, 0, 0, R_NONE, 0);
    // CMD8: R7 echoes the check pattern; RESP0[37:32] is the index.
    assert_eq!(sdm_ok(cpu, 8, 0x1AA, R1, 0), (8 << 32) | 0x1AA);
    // An SDIO probe (CMD5) times out on a memory card.
    assert_eq!(sdm(cpu, 5, 0, R3, 0) & ST_CMD_TIMEOUT, ST_CMD_TIMEOUT);
    // ACMD41: an inquiry, then polls until the OCR says ready (R3: type 4).
    assert_eq!(sdm_ok(cpu, 55, 0, R1, 0) & u64::from(card::ST_APP_CMD), u64::from(card::ST_APP_CMD));
    let ocr = sdm_ok(cpu, 41, 0, R3, 0);
    assert_eq!(ocr, (0x3F << 32) | 0x00FF_8000, "inquiry: voltage window, not ready");
    let mut polls = 0;
    loop {
        sdm_ok(cpu, 55, 0, R1, 0);
        let ocr = sdm_ok(cpu, 41, 0x40FF_8000, R3, 0) as u32;
        polls += 1;
        if ocr & (1 << 31) != 0 {
            assert_eq!(ocr, 0xC0FF_8000, "ready, CCS");
            break;
        }
        assert!(polls < 10);
    }
    // CMD2: the CID as R2.
    sdm_ok(cpu, 2, 0, R2, 0);
    let cid = (u128::from(rd(cpu, SDM_RESP1)) << 64) | u128::from(rd(cpu, SDM_RESP0));
    assert_eq!(&cid.to_be_bytes()[3..8], b"KLEMU");
    // CMD3: R6 with the RCA, the card now in stby.
    let r6 = sdm_ok(cpu, 3, 0, R1, 0);
    assert_eq!(r6 >> 16 & 0xFFFF, u64::from(RCA));
    let rca = u32::from(RCA) << 16;
    assert_eq!(state_of(sdm_ok(cpu, 13, rca, R1, 0)), 3, "stby");
    // CMD9: the CSD as R2.
    sdm_ok(cpu, 9, rca, R2, 0);
    let csd = (u128::from(rd(cpu, SDM_RESP1)) << 64) | u128::from(rd(cpu, SDM_RESP0));
    assert_eq!(csd >> 126, 1, "CSD 2.0");
    assert_eq!(((csd >> 48) & 0x3F_FFFF) + 1, (TEST_BLOCKS / 1024) as u128, "C_SIZE");
    // CMD7 selects (R1b): tran.
    sdm_ok(cpu, 7, rca, R1B, 0);
    assert_eq!(state_of(sdm_ok(cpu, 13, rca, R1, 0)), 4, "tran");
    // ACMD51 reads the 8-byte SCR on the 1-bit bus.
    wr(cpu, SDM_FIFO, FIFO_RESET);
    wr(cpu, SDM_BLK, (1 << 16) | 8);
    sdm_ok(cpu, 55, rca, R1, 0);
    sdm_ok(cpu, 51, 0, R1, CMD_DATA);
    assert_ne!(rd(cpu, SDM_STATUS) as u32 & ST_DATA_DONE, 0);
    assert_eq!(rd(cpu, SDM_BLKDONE), 1);
    let scr = pop(cpu, 1);
    assert_eq!(scr[0] & 0xF, 2, "SD_SPEC 2");
    assert_eq!(scr[1] & 0xF, 0b0101, "1- and 4-bit buses");
    assert_eq!(scr[3] & 0x2, 0x2, "CMD23");
    // ACMD6: the 4-bit bus.
    sdm_ok(cpu, 55, rca, R1, 0);
    sdm_ok(cpu, 6, 2, R1, 0);
    wr(cpu, SDM_CFG, 1);
    wr(cpu, SD_CTRL, 1 | CTRL_SD_MODE | CTRL_PWR_ON);
}

#[test]
fn native_init_r2_layout_and_registers() {
    let mut cpu = cpu(true, true);
    // Reset values.
    assert_eq!(rd(&mut cpu, SDM_TIMEOUT), TIMEOUT_RESET);
    assert_eq!(rd(&mut cpu, SDM_BLK), 0x0001_0200, "512 bytes, 1 block");
    for offset in [SDM_ARG, SDM_RESP0, SDM_RESP1, SDM_IRQ_EN, SDM_CFG, SDM_FIFO, SDM_BLKDONE] {
        assert_eq!(rd(&mut cpu, offset), 0, "offset 0x{offset:03X}");
    }
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32, ST_DAT0 | ST_CARD_PRESENT | ST_IDELAY_RDY);
    assert_eq!(rd(&mut cpu, SD_STATUS) & STATUS_NO_CARD, 0, "the raw pin: low with a card in");
    assert_eq!(rd(&mut cpu, SDM_CMD), 0, "write-only");
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR_TEST), 0, "reserved");
    wr(&mut cpu, SDM_CFG, 0xFFFF_FFFF);
    assert_eq!(rd(&mut cpu, SDM_CFG), 0x1FF7, "4-bit, HS_SAMPLE, CLK_ON, the phase and the tap");
    wr(&mut cpu, SDM_BLK, 0xFFFF_FFFF);
    assert_eq!(rd(&mut cpu, SDM_BLK), 0xFFFF_0FFC, "size [1:0] reads 0");
    wr(&mut cpu, SDM_BLK, (1 << 16) | 3);
    assert_eq!(rd(&mut cpu, SDM_BLK), 1 << 16, "sizes 1-3 read 0");
    assert_eq!(cpu.sd.as_ref().unwrap().blk_size(), 8, "and act as 8");
    wr(&mut cpu, SDM_CFG, 0);
    // A command with SD_MODE clear is ignored.
    wr(&mut cpu, SDM_CMD, cmd_word(8, R1, 0));
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32 & ST_CMD_DONE, 0);

    native_init(&mut cpu);
    assert_eq!(card(&cpu).state(), card::State::Tran);

    // The R2 layout: RESP1:RESP0 = response bits [127:0], the CRC7 and end
    // bit in RESP0[7:0]; Linux's resp[0..3] = RESP1 hi, RESP1 lo, RESP0 hi,
    // RESP0 lo.  Deselect to read the CSD again.
    let rca = u32::from(RCA) << 16;
    assert_eq!(sdm(&mut cpu, 7, 0, R_NONE, 0) & ST_ERRORS, 0, "deselect: no response");
    sdm_ok(&mut cpu, 9, rca, R2, 0);
    let (r1, r0) = (rd(&mut cpu, SDM_RESP1), rd(&mut cpu, SDM_RESP0));
    let raw = (u128::from(r1) << 64) | u128::from(r0);
    let bytes = raw.to_be_bytes();
    assert_eq!(r0 & 0xFF, u64::from((crc7(&bytes[..15]) << 1) | 1));
    let linux = [(r1 >> 32) as u32, r1 as u32, (r0 >> 32) as u32, r0 as u32];
    assert_eq!(linux[0] >> 30, 1, "CSD_STRUCTURE in resp[0][31:30]");
    assert_eq!(linux[1] >> 20 & 0xFFF, 0x515, "CCC in resp[1][31:20]");
    // R2 asked for as R1: the first 48 bits have index 0x3F and a bad CRC.
    // SDM_RESP* hold the response as received.
    let status = sdm(&mut cpu, 9, rca, R1, 0);
    assert_ne!(status & ST_CMD_IDX_ERR, 0);
    assert_ne!(status & ST_ERR_ANY, 0);
    let head = (raw >> 96) as u64; // the 32 bits after the index field
    assert_eq!(rd(&mut cpu, SDM_RESP0), (0x3F << 32) | (head & 0xFFFF_FFFF));
    assert_eq!(rd(&mut cpu, SDM_RESP1), 0);
    // A timeout leaves SDM_RESP* unchanged.
    sdm(&mut cpu, 9, rca, R2, 0);
    assert_ne!(sdm(&mut cpu, 2, 0, R2, 0) & ST_CMD_TIMEOUT, 0, "CMD2 in stby: no answer");
    assert_eq!((rd(&mut cpu, SDM_RESP1), rd(&mut cpu, SDM_RESP0)), (r1, r0));
    // R1 asked for as R2: a bad CRC over the padding.
    let status = sdm(&mut cpu, 13, rca, R2, 0);
    assert_ne!(status & ST_CMD_CRC_ERR, 0);
    sdm_ok(&mut cpu, 7, rca, R1B, 0);
    // An illegal command times out, and the next R1 reports it.
    assert_ne!(sdm(&mut cpu, 2, 0, R2, 0) & ST_CMD_TIMEOUT, 0);
    let r1 = sdm_ok(&mut cpu, 13, rca, R1, 0);
    assert_ne!(r1 & u64::from(card::ST_ILLEGAL_COMMAND), 0);
    assert_eq!(sdm_ok(&mut cpu, 13, rca, R1, 0) & u64::from(card::ST_ILLEGAL_COMMAND), 0, "reported once");
    // W1C: writing a 1 clears a sticky bit, writing 0 leaves it.
    sdm(&mut cpu, 2, 0, R2, 0);
    wr(&mut cpu, SDM_STATUS, u64::from(ST_CMD_DONE));
    let status = rd(&mut cpu, SDM_STATUS) as u32;
    assert_eq!(status & (ST_CMD_DONE | ST_CMD_TIMEOUT | ST_ERR_ANY), ST_CMD_TIMEOUT | ST_ERR_ANY);
    wr(&mut cpu, SDM_STATUS, u64::from(ST_CMD_TIMEOUT));
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32 & (ST_ERRORS | ST_ERR_ANY), 0);
}

/// `SDM_DMA_ADDR`, reserved.
const SDM_DMA_ADDR_TEST: u32 = 0x150;

#[test]
fn native_status_registers_cmd6_and_acmd13() {
    let mut cpu = cpu(true, true);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    // ACMD13: 64 bytes of SD status on the 4-bit bus, which it reports.
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (1 << 16) | 64);
    sdm_ok(&mut cpu, 55, rca, R1, 0);
    sdm_ok(&mut cpu, 13, 0, R1, CMD_DATA);
    assert_eq!(rd(&mut cpu, SDM_FIFO), 8);
    let status = pop(&mut cpu, 8);
    assert_eq!(status[0] >> 6, 0b10, "DAT_BUS_WIDTH: 4 bits");
    // CMD6 check mode for high speed: group 1 supports functions 0 and 1,
    // and the selection says function 1 can be switched to; nothing
    // switches in mode 0.
    wr(&mut cpu, SDM_BLK, (1 << 16) | 64);
    sdm_ok(&mut cpu, 6, 0x00FF_FFF1, R1, CMD_DATA);
    let switch = pop(&mut cpu, 8);
    assert_eq!(switch[12..14], [0x80, 0x03], "group 1 supports default and high speed");
    assert_eq!(switch[2..12], [0x80, 0x01].repeat(5), "groups 6-2: function 0 only");
    assert_eq!(switch[16] & 0xF, 1, "group 1: high speed available");
    assert_eq!(switch[17], 1, "data structure version");
    assert_eq!(rd(&mut cpu, SDM_FIFO), 0);
    assert!(!card(&cpu).high_speed());
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "back in tran");
}

#[test]
fn native_multi_block_read_paces_the_fifo() {
    let mut cpu = cpu(true, true);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    // Ten blocks from 20 with CMD18: the FIFO takes eight, then the clock
    // stops until the CPU drains one.
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (10 << 16) | 512);
    sdm_ok(&mut cpu, 18, 20, R1, CMD_DATA);
    let status = rd(&mut cpu, SDM_STATUS) as u32;
    assert_eq!(status & (ST_DATA_BUSY | ST_FIFO_THRESH | ST_DATA_DONE), ST_DATA_BUSY | ST_FIFO_THRESH);
    assert_eq!(rd(&mut cpu, SDM_FIFO), 512);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 8);
    let mut got = Vec::new();
    let mut thresholds = 0;
    while rd(&mut cpu, SDM_STATUS) as u32 & ST_FIFO_THRESH != 0 {
        thresholds += 1;
        got.extend(pop(&mut cpu, 64));
    }
    assert_eq!(thresholds, 10, "FIFO_THRESH once per block, also after DATA_DONE");
    let status = rd(&mut cpu, SDM_STATUS) as u32;
    assert_eq!(status & (ST_DATA_BUSY | ST_DATA_DONE | ST_ERR_ANY), ST_DATA_DONE);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 10);
    assert_eq!(rd(&mut cpu, SDM_FIFO), 0);
    assert_eq!(rd(&mut cpu, SDM_DATA), 0, "an empty FIFO pops 0");
    assert_eq!(got, (20..30).flat_map(pattern).collect::<Vec<_>>());
    // The card still streams until CMD12 (R1b, stop): data -> tran.
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 5, "data");
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");

    // Open-ended (count 0): CMD12 with the stop flag ends the phase
    // cleanly; the blocks already in the FIFO stay to be drained.
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, 512);
    sdm_ok(&mut cpu, 18, 0, R1, CMD_DATA);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 8);
    got = pop(&mut cpu, 64 * 3);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 11);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    let status = rd(&mut cpu, SDM_STATUS) as u32;
    assert_eq!(status & (ST_DATA_BUSY | ST_DATA_DONE), ST_DATA_DONE);
    got.extend(pop(&mut cpu, 64 * 8));
    assert_eq!(got, (0..11).flat_map(pattern).collect::<Vec<_>>());
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32 & ST_FIFO_THRESH, 0);

    // CMD23 then CMD18: the card stops by itself, no CMD12.
    sdm_ok(&mut cpu, 23, 2, R1, 0);
    wr(&mut cpu, SDM_BLK, (2 << 16) | 512);
    sdm_ok(&mut cpu, 18, 7, R1, CMD_DATA);
    assert_eq!(pop(&mut cpu, 128), [pattern(7), pattern(8)].concat());
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");
}

#[test]
fn native_multi_block_write_with_prefill() {
    let mut cpu = cpu(true, true);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    let data: Vec<Vec<u8>> = (0..3).map(|n| (0..BLOCK).map(|i| (i as u8).wrapping_mul(3) ^ n).collect()).collect();
    let push = |cpu: &mut Cpu, block: &[u8]| {
        for w in block.as_chunks::<8>().0 {
            wr(cpu, SDM_DATA, u64::from_le_bytes(*w));
        }
    };
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32 & ST_FIFO_THRESH, 0, "DIR = NONE after a FIFO reset");
    // Pre-fill one block (no FIFO_THRESH: DIR is still NONE), then CMD25
    // for three: the block goes out at once.
    push(&mut cpu, &data[0]);
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32 & ST_FIFO_THRESH, 0, "a pre-fill does not raise it");
    wr(&mut cpu, SDM_BLK, (3 << 16) | 512);
    sdm_ok(&mut cpu, 25, 40, R1, CMD_DATA | CMD_WRITE);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 1);
    assert_eq!(rd(&mut cpu, SDM_FIFO), 0);
    let status = rd(&mut cpu, SDM_STATUS) as u32;
    assert_eq!(status & (ST_DATA_BUSY | ST_FIFO_THRESH), ST_DATA_BUSY | ST_FIFO_THRESH);
    push(&mut cpu, &data[1]);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 2);
    // Half a block is held until it is whole.
    push(&mut cpu, &data[2][..256]);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 2);
    assert_eq!(rd(&mut cpu, SDM_FIFO), 32);
    assert_ne!(rd(&mut cpu, SDM_STATUS) as u32 & ST_FIFO_THRESH, 0, "room for a block");
    push(&mut cpu, &data[2][256..]);
    let status = rd(&mut cpu, SDM_STATUS) as u32;
    assert_eq!(status & (ST_DATA_BUSY | ST_DATA_DONE | ST_FIFO_THRESH | ST_ERR_ANY), ST_DATA_DONE);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 3);
    assert_eq!(card_bytes(&cpu)[40 * BLOCK..43 * BLOCK], data.concat()[..]);
    assert_eq!(card_bytes(&cpu)[43 * BLOCK..44 * BLOCK], pattern(43)[..]);
    // CMD12 ends the card's receive: rcv -> prg -> tran.
    let r1 = sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(state_of(r1), 6, "the R1 shows rcv");
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");
    // ACMD22: the blocks the last write wrote, a 4-byte big-endian block
    // in [31:0] of one FIFO word ([63:32] = 0).
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (1 << 16) | 4);
    sdm_ok(&mut cpu, 55, rca, R1, 0);
    sdm_ok(&mut cpu, 22, 0, R1, CMD_DATA);
    let status = rd(&mut cpu, SDM_STATUS) as u32;
    assert_eq!(status & (ST_DATA_DONE | ST_FIFO_THRESH | ST_ERR_ANY), ST_DATA_DONE | ST_FIFO_THRESH);
    assert_eq!(rd(&mut cpu, SDM_FIFO), 1);
    let word = rd(&mut cpu, SDM_DATA);
    assert_eq!(word >> 32, 0);
    assert_eq!(u32::from_be_bytes((word as u32).to_le_bytes()), 3, "three blocks written");
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");
    // FIFO_THRESH on a counted write drops once every block is pushed
    // (counted in words since the FIFO reset).
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (1 << 16) | 512);
    sdm_ok(&mut cpu, 24, 50, R1, CMD_DATA | CMD_WRITE);
    assert_ne!(rd(&mut cpu, SDM_STATUS) as u32 & ST_FIFO_THRESH, 0);
    push(&mut cpu, &data[1]);
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32 & (ST_FIFO_THRESH | ST_DATA_DONE), ST_DATA_DONE);
    assert_eq!(card_bytes(&cpu)[50 * BLOCK..51 * BLOCK], data[1][..]);
    // Read it back with CMD17.
    wr(&mut cpu, SDM_BLK, (1 << 16) | 512);
    sdm_ok(&mut cpu, 17, 50, R1, CMD_DATA);
    assert_eq!(pop(&mut cpu, 64), data[1]);
}

#[test]
fn native_errors() {
    let mut cpu = cpu(true, true);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    // The host on the 1-bit bus while the card uses 4 bits: the block's
    // CRC fails and never reaches the FIFO.
    wr(&mut cpu, SDM_CFG, 0);
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (1 << 16) | 512);
    let status = sdm(&mut cpu, 17, 0, R1, CMD_DATA);
    assert_eq!(
        status & (ST_DATA_CRC_ERR | ST_DATA_BUSY | ST_DATA_DONE | ST_ERR_ANY),
        ST_DATA_CRC_ERR | ST_ERR_ANY
    );
    assert_eq!(rd(&mut cpu, SDM_FIFO), 0);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 0);
    // The same for a write: CRC status token 101.
    for w in 0..64 {
        wr(&mut cpu, SDM_DATA, w);
    }
    let status = sdm(&mut cpu, 24, 0, R1, CMD_DATA | CMD_WRITE);
    assert_eq!(status & (ST_WRITE_CRC_ERR | ST_DATA_BUSY), ST_WRITE_CRC_ERR);
    assert_eq!(card_bytes(&cpu)[..BLOCK], pattern(0)[..], "nothing written");
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    wr(&mut cpu, SDM_CFG, 1);
    // A read past the end: R1 says OUT_OF_RANGE and no data comes.
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    let r1 = {
        let status = sdm(&mut cpu, 17, TEST_BLOCKS as u32, R1, CMD_DATA);
        assert_eq!(status & (ST_DATA_TIMEOUT | ST_DATA_BUSY), ST_DATA_TIMEOUT);
        rd(&mut cpu, SDM_RESP0)
    };
    assert_ne!(r1 & u64::from(card::ST_OUT_OF_RANGE), 0);
    // A CMD18 that runs off the end: the good blocks stay in the FIFO.
    wr(&mut cpu, SDM_BLK, (4 << 16) | 512);
    let status = sdm(&mut cpu, 18, TEST_BLOCKS as u32 - 2, R1, CMD_DATA);
    assert_eq!(status & (ST_DATA_TIMEOUT | ST_DATA_BUSY), ST_DATA_TIMEOUT);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 2);
    assert_eq!(pop(&mut cpu, 128), [pattern(TEST_BLOCKS - 2), pattern(TEST_BLOCKS - 1)].concat());
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    // A write data command the card rejects (not in tran: CMD2 is illegal
    // there) times out: no data phase.
    let status = sdm(&mut cpu, 2, 0, R2, CMD_DATA | CMD_WRITE);
    assert_eq!(status & (ST_CMD_TIMEOUT | ST_DATA_BUSY), ST_CMD_TIMEOUT);
    // A write the card isn't receiving: no CRC token.
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (1 << 16) | 512);
    sdm_ok(&mut cpu, 13, rca, R1, 0);
    let status = sdm(&mut cpu, 16, 512, R1, CMD_DATA | CMD_WRITE);
    for w in 0..64 {
        wr(&mut cpu, SDM_DATA, w);
    }
    assert_eq!(status & ST_ERRORS, 0);
    assert_ne!(rd(&mut cpu, SDM_STATUS) as u32 & ST_WRITE_NO_TOKEN, 0);
    // A push into a full FIFO is dropped and sets FIFO_ERR.
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_STATUS, u64::from(ST_STICKY));
    for w in 0..=FIFO_WORDS as u64 {
        wr(&mut cpu, SDM_DATA, w);
    }
    assert_eq!(rd(&mut cpu, SDM_FIFO), FIFO_WORDS as u64);
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32 & (ST_FIFO_ERR | ST_ERR_ANY), ST_FIFO_ERR | ST_ERR_ANY);
    assert_eq!(rd(&mut cpu, SDM_DATA), 0, "the first word pushed");
    // No card at all: every command times out.
    let mut empty = cpu_without_card();
    wr(&mut empty, SD_CTRL, CTRL_SD_MODE | CTRL_PWR_ON);
    assert_eq!(rd(&mut empty, SDM_STATUS) as u32 & ST_CARD_PRESENT, 0);
    assert_ne!(sdm(&mut empty, 8, 0x1AA, R1, 0) & ST_CMD_TIMEOUT, 0);
}

#[test]
fn native_and_spi_modes_need_a_power_cycle() {
    let mut cpu = cpu(true, true);
    native_init(&mut cpu);
    // SPI exchanges do nothing in SD mode.
    wr(&mut cpu, SD_DATA, 0x40);
    assert_eq!(rd(&mut cpu, SD_STATUS) & STATUS_BUSY, 0);
    // Back to SPI mode without a power cycle: CMD0 with CS low still
    // switches the card (the plan's second line of defence).
    wr(&mut cpu, SD_CTRL, CLK_INIT | CTRL_PWR_ON);
    assert_eq!(spi_cmd(&mut cpu, 0, 0), 0x01);
    assert!(card(&cpu).in_spi_mode());
    // A card in SPI mode ignores the SD bus.
    wr(&mut cpu, SD_CTRL, CLK_IDENT | CTRL_SD_MODE | CTRL_PWR_ON);
    assert_ne!(sdm(&mut cpu, 0, 0, R_NONE, 0) & ST_CMD_DONE, 0);
    assert_ne!(sdm(&mut cpu, 8, 0x1AA, R1, 0) & ST_CMD_TIMEOUT, 0);
    // A power cycle brings it back in SD mode.
    wr(&mut cpu, SD_CTRL, CLK_IDENT | CTRL_SD_MODE);
    native_init(&mut cpu);
    // And the SPI drivers' rule (power off, then on, then CMD0) works too.
    wr(&mut cpu, SD_CTRL, CLK_INIT | CTRL_CS_HI);
    spi_init(&mut cpu);
    assert_eq!(spi_read(&mut cpu, 1, true), pattern(1));
}

#[test]
fn native_interrupt_cascades_onto_source_2() {
    let mut cpu = cpu(true, true);
    assert_eq!(cpu.mmio_read(INT2_EN), 0b01, "Ethernet only at reset");
    assert_eq!(cpu.mmio_read(INT2_PENDING), 0);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    // CMD_DONE is set but not enabled: nothing pending.
    sdm_ok(&mut cpu, 13, rca, R1, 0);
    assert_eq!(cpu.mmio_read(INT2_PENDING), 0);
    wr(&mut cpu, SDM_IRQ_EN, u64::from(ST_CMD_DONE) | 0xF000_0000);
    assert_eq!(rd(&mut cpu, SDM_IRQ_EN), u64::from(ST_CMD_DONE), "only the defined bits");
    assert_eq!(cpu.mmio_read(INT2_PENDING), 0b10, "raw, before INT2_EN");
    assert_eq!(cpu.mmio_read(INT_PENDING) & 0b100, 0, "INT2_EN[1] clear");
    cpu.mmio_write(INT2_EN, 0b11);
    assert_eq!(cpu.mmio_read(INT_PENDING) & 0b100, 0b100);
    assert_eq!(cpu.irq_state().pending & 0b100, 0b100);
    // Dispatched only when vectored and unmasked.
    assert_eq!(cpu.irq_ready(), None);
    cpu.mmio_write(INT_VEC2, 0x80);
    assert_eq!(cpu.irq_ready(), None);
    cpu.mmio_write(INT_MASK, 0b100);
    assert_eq!(cpu.irq_ready(), Some(2));
    // A level: clearing CMD_DONE (W1C) drops it.
    wr(&mut cpu, SDM_STATUS, u64::from(ST_CMD_DONE));
    assert_eq!(cpu.irq_ready(), None);
    assert_eq!(cpu.mmio_read(INT_PENDING) & 0b100, 0);
    // FIFO_THRESH as a source: a read paced block by block.
    wr(&mut cpu, SDM_IRQ_EN, u64::from(ST_FIFO_THRESH));
    wr(&mut cpu, SDM_BLK, (2 << 16) | 512);
    sdm_ok(&mut cpu, 18, 0, R1, CMD_DATA);
    assert_eq!(cpu.irq_ready(), Some(2));
    pop(&mut cpu, 128);
    assert_eq!(cpu.irq_ready(), None);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    // INT2_EN[1] off masks SD again.
    cpu.mmio_write(INT2_EN, 0b01);
    wr(&mut cpu, SDM_IRQ_EN, u64::from(ST_CMD_DONE));
    assert_eq!(cpu.mmio_read(INT2_PENDING), 0b10);
    assert_eq!(cpu.irq_ready(), None);
}

#[test]
fn native_interrupt_wakes_a_waiting_program() {
    // INT_VEC2 / INT_MASK / SDM_IRQ_EN set up and a CMD13 issued, a CPU
    // parked in WAIT wakes into the source 2 handler, which HALTs.
    let mut cpu = cpu(true, true);
    native_init(&mut cpu);
    cpu.mmio_write(INT2_EN, 0b11);
    cpu.mmio_write(INT_VEC2, 0x40);
    cpu.mmio_write(INT_MASK, 0b100);
    wr(&mut cpu, SDM_IRQ_EN, u64::from(ST_CMD_DONE));
    sdm_ok(&mut cpu, 13, u32::from(RCA) << 16, R1, 0);
    // HALT at the vector.
    assert!(cpu.debug_write(0x40, &0x6C01_0000_u32.to_le_bytes()));
    cpu.waiting = true;
    let result = cpu.run(10, None);
    assert_eq!(result.stop, crate::emu::StopReason::Halt, "the source 2 handler ran");
    assert_eq!(result.instructions, 1);
    assert_eq!(cpu.int_mask & 0b100, 0, "source 2 masked on entry");
}

#[test]
fn native_corner_cases_of_plan_4_8() {
    let mut cpu = cpu(true, true);
    // Before any data command DIR is NONE: pushed words raise nothing.
    wr(&mut cpu, SDM_BLK, (1 << 16) | 8);
    wr(&mut cpu, SDM_DATA, 1);
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32 & ST_FIFO_THRESH, 0);
    // Registers other than SDM_CMD work with SD_MODE = 0: pop, W1C, reset.
    assert_eq!(rd(&mut cpu, SDM_DATA), 1);
    wr(&mut cpu, SDM_DATA, 2);
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    assert_eq!(rd(&mut cpu, SDM_FIFO), 0);
    // SDM_ARG keeps 32 bits.
    wr(&mut cpu, SDM_ARG, 0xDEAD_BEEF_1234_5678);
    assert_eq!(rd(&mut cpu, SDM_ARG), 0x1234_5678);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;

    // A type-0 command leaves SDM_RESP* as they were.
    let resp = sdm_ok(&mut cpu, 13, rca, R1, 0);
    sdm_ok(&mut cpu, 13, rca, R_NONE, 0);
    assert_eq!(rd(&mut cpu, SDM_RESP0), resp);
    // A stop with no data phase is an ordinary R1b: DATA_* unchanged.
    wr(&mut cpu, SDM_STATUS, u64::from(ST_STICKY));
    wr(&mut cpu, SDM_ARG, 0);
    wr(&mut cpu, SDM_CMD, cmd_word(12, R1B, CMD_STOP));
    let status = rd(&mut cpu, SDM_STATUS) as u32;
    assert_eq!(
        status & (ST_CMD_DONE | ST_CMD_TIMEOUT | ST_DATA_DONE | ST_DATA_BUSY),
        ST_CMD_DONE | ST_CMD_TIMEOUT,
        "CMD12 in tran: no answer"
    );

    // A size field of 0 reads 0 and acts as 8: the 8-byte SCR.
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, 1 << 16);
    assert_eq!(rd(&mut cpu, SDM_BLK), 1 << 16);
    sdm_ok(&mut cpu, 55, rca, R1, 0);
    sdm_ok(&mut cpu, 51, 0, R1, CMD_DATA);
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32 & ST_DATA_DONE, ST_DATA_DONE);
    assert_eq!(rd(&mut cpu, SDM_FIFO), 1);
    assert_ne!(rd(&mut cpu, SDM_STATUS) as u32 & ST_FIFO_THRESH, 0, "READ: one 8-byte block in the FIFO");
    // FIFO_THRESH uses the block size latched by the data command, even
    // after the phase: a new SDM_BLK counts only from the next one.
    wr(&mut cpu, SDM_BLK, (1 << 16) | 512);
    assert_ne!(rd(&mut cpu, SDM_STATUS) as u32 & ST_FIFO_THRESH, 0, "still the latched 8 bytes");
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    sdm_ok(&mut cpu, 17, 0, R1, CMD_DATA);
    wr(&mut cpu, SDM_BLK, (1 << 16) | 8);
    assert_ne!(rd(&mut cpu, SDM_STATUS) as u32 & ST_FIFO_THRESH, 0, "a 512-byte block");
    pop(&mut cpu, 63);
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32 & ST_FIFO_THRESH, 0, "one word left: below 512 bytes");
    pop(&mut cpu, 1);

    // A data command while DATA_BUSY is ignored entirely; a non-data one
    // (CMD13) goes out and leaves the phase running.
    wr(&mut cpu, SDM_BLK, 512);
    sdm_ok(&mut cpu, 18, 0, R1, CMD_DATA);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 8);
    wr(&mut cpu, SDM_STATUS, u64::from(ST_STICKY));
    let before = (rd(&mut cpu, SDM_RESP0), rd(&mut cpu, SDM_STATUS));
    wr(&mut cpu, SDM_BLK, (1 << 16) | 8);
    wr(&mut cpu, SDM_ARG, 100);
    wr(&mut cpu, SDM_CMD, cmd_word(17, R1, CMD_DATA));
    wr(&mut cpu, SDM_CMD, cmd_word(24, R1, CMD_DATA | CMD_WRITE));
    assert_eq!((rd(&mut cpu, SDM_RESP0), rd(&mut cpu, SDM_STATUS)), before, "no bit changes");
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 8);
    assert_eq!(card(&cpu).state(), card::State::Data, "nothing sent");
    assert_eq!(cpu.sd.as_ref().unwrap().thresh_size, 512, "nor the latched size");
    wr(&mut cpu, SDM_BLK, 512);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 5, "CMD13 during the phase");
    assert_ne!(rd(&mut cpu, SDM_STATUS) as u32 & ST_DATA_BUSY, 0, "still running");
    // A stop that ends the open-ended phase sets DATA_DONE.
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32 & (ST_DATA_DONE | ST_DATA_BUSY), ST_DATA_DONE);

    // A stop that ends a counted phase early: no DATA_DONE.
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (20 << 16) | 512);
    sdm_ok(&mut cpu, 18, 0, R1, CMD_DATA);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32 & (ST_DATA_DONE | ST_DATA_BUSY), 0);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 8);

    // Clearing SD_MODE abandons the phase (no DONE) but keeps the FIFO,
    // DIR and the registers.
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    sdm_ok(&mut cpu, 18, 0, R1, CMD_DATA);
    wr(&mut cpu, SDM_STATUS, u64::from(ST_STICKY));
    wr(&mut cpu, SD_CTRL, 1 | CTRL_PWR_ON);
    let status = rd(&mut cpu, SDM_STATUS) as u32;
    assert_eq!(status & (ST_DATA_BUSY | ST_DATA_DONE | ST_FIFO_THRESH), ST_FIFO_THRESH, "READ still");
    assert_eq!(rd(&mut cpu, SDM_FIFO), 512);
    assert_eq!(rd(&mut cpu, SDM_BLK), (20 << 16) | 512);
    // SDM_CMD is ignored with SD_MODE clear.
    wr(&mut cpu, SDM_CMD, cmd_word(13, R1, 0));
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32 & ST_CMD_DONE, 0);
}

#[test]
fn native_blocks_of_4_mod_8_bytes() {
    // 12-byte blocks take two words each; the second carries bytes 8-11 in
    // [31:0] and 0 in [63:32], and each block starts in a new word.
    let blocks: Vec<Vec<u8>> = (0..3_u8).map(|b| (0..12).map(|i| 0x10 * (b + 1) + i).collect()).collect();
    let mut cpu = cpu(true, true);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    cpu.sd.as_mut().unwrap().card.as_mut().unwrap().queue_blocks(blocks.clone());
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (3 << 16) | 0xC);
    sdm_ok(&mut cpu, 13, rca, R1, CMD_DATA);
    let status = rd(&mut cpu, SDM_STATUS) as u32;
    assert_eq!(status & (ST_DATA_DONE | ST_ERR_ANY), ST_DATA_DONE);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 3);
    assert_eq!(rd(&mut cpu, SDM_FIFO), 6, "ceil(12 / 8) words a block");
    for block in &blocks {
        assert_ne!(rd(&mut cpu, SDM_STATUS) as u32 & ST_FIFO_THRESH, 0, "two words: a block");
        let lo = rd(&mut cpu, SDM_DATA);
        let hi = rd(&mut cpu, SDM_DATA);
        assert_eq!(lo.to_le_bytes(), block[..8]);
        assert_eq!(hi, u64::from(u32::from_le_bytes(block[8..].try_into().unwrap())));
    }
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32 & ST_FIFO_THRESH, 0);

    // The packing both ways: a write takes only [31:0] of a last half word.
    let words: Vec<u64> = block_to_words(&blocks[0]).collect();
    assert_eq!(words.len(), 2);
    let garbage = words[1] | 0xDEAD_BEEF_0000_0000;
    assert_eq!(words_to_block([words[0], garbage].into_iter(), 12), blocks[0]);
    assert_eq!(block_words(4), 1);
    assert_eq!(block_words(12), 2);
    assert_eq!(block_words(512), 64);
}

// ---- fault injection (`--sd-fault`) ----

/// Give the card these `--sd-fault` specs.
fn inject(cpu: &mut Cpu, specs: &[&str]) {
    let specs = specs.iter().map(|s| s.parse().unwrap()).collect();
    cpu.sd.as_mut().unwrap().card.as_mut().unwrap().set_faults(specs);
}

/// SPI ACMD22: the blocks the last write wrote.
fn spi_acmd22(cpu: &mut Cpu) -> u32 {
    cs(cpu, true);
    assert_eq!(spi_cmd(cpu, 55, 0), 0x00);
    assert_eq!(spi_cmd(cpu, 22, 0), 0x00);
    assert_eq!(spi_token(cpu), 0xFE);
    let n = u32::from_be_bytes(std::array::from_fn(|_| xfer(cpu, 0xFF)));
    xfer(cpu, 0xFF);
    xfer(cpu, 0xFF);
    cs(cpu, false);
    n
}

/// Native ACMD22 (by PIO: `DMA_EN` off).
fn native_acmd22(cpu: &mut Cpu) -> u32 {
    let cfg = rd(cpu, SDM_CFG);
    wr(cpu, SDM_CFG, cfg & !CFG_DMA_EN);
    wr(cpu, SDM_FIFO, FIFO_RESET);
    wr(cpu, SDM_BLK, (1 << 16) | 4);
    sdm_ok(cpu, 55, u32::from(RCA) << 16, R1, 0);
    sdm_ok(cpu, 22, 0, R1, CMD_DATA);
    u32::from_be_bytes((rd(cpu, SDM_DATA) as u32).to_le_bytes())
}

#[test]
fn fault_specs_parse_and_print() {
    for (spec, kind, cmd, at, count) in [
        ("wcrc@37", FaultKind::WriteCrc, None, 37, 1),
        ("wbusy@1x3", FaultKind::WriteBusy, None, 1, 3),
        ("rerr@5", FaultKind::ReadError, None, 5, 1),
        ("rcrc@5x2", FaultKind::ReadError, None, 5, 2),
        ("cmdto@100", FaultKind::CmdTimeout, None, 100, 1),
        ("cmdto:17@2x4", FaultKind::CmdTimeout, Some(17), 2, 4),
        ("dmaerr@3", FaultKind::DmaErr, None, 3, 1),
        ("dmastall@1x2", FaultKind::DmaStall, None, 1, 2),
        ("dmahang@7", FaultKind::DmaHang, None, 7, 1),
        ("dmastuck@9", FaultKind::DmaStuck, None, 9, 1),
    ] {
        let fault: SdFault = spec.parse().unwrap();
        assert_eq!((fault.kind, fault.cmd, fault.at, fault.count), (kind, cmd, at, count), "{spec}");
        assert_eq!(fault.to_string(), spec.replace("rcrc", "rerr"));
    }
    for bad in [
        "",
        "wcrc",
        "wcrc@0",
        "wcrc@x2",
        "wcrc@3x0",
        "wcrc@3x",
        "foo@1",
        "wcrc:17@1",
        "cmdto:64@1",
        "cmdto:x@1",
        "wcrc@-1",
        "dmaerr:17@1",
    ] {
        assert!(bad.parse::<SdFault>().is_err(), "{bad}");
    }
}

#[test]
fn spi_write_faults() {
    let mut cpu = cpu_with_init();
    // wcrc@2: the second block of a CMD25 gets data response 0x0B and is
    // not programmed; the driver stops, and ACMD22 says one block.
    inject(&mut cpu, &["wcrc@2", "wbusy@4"]);
    cs(&mut cpu, true);
    assert_eq!(spi_cmd(&mut cpu, 25, 200), 0x00);
    let mut responses = Vec::new();
    for b in 0..3_u8 {
        xfer(&mut cpu, 0xFF);
        xfer(&mut cpu, 0xFC);
        tx_block(&mut cpu, &[0x70 + b; BLOCK], true);
        xfer(&mut cpu, 0xFF);
        xfer(&mut cpu, 0xFF);
        let resp = xfer(&mut cpu, 0xFF) & 0x1F;
        responses.push(resp);
        if resp != 0x05 {
            break;
        }
        spi_wait_not_busy(&mut cpu);
    }
    assert_eq!(responses, [0x05, 0x0B]);
    spi_wait_not_busy(&mut cpu);
    xfer(&mut cpu, 0xFD);
    xfer(&mut cpu, 0xFF);
    spi_wait_not_busy(&mut cpu);
    cs(&mut cpu, false);
    assert_eq!(spi_acmd22(&mut cpu), 1);
    assert_eq!(card_bytes(&cpu)[200 * BLOCK..201 * BLOCK], [0x70; BLOCK]);
    assert_eq!(card_bytes(&cpu)[201 * BLOCK..202 * BLOCK], pattern(201)[..], "not programmed");
    // The retry (write event 3) succeeds: the fault was transient.
    spi_write(&mut cpu, 201, &[0x71; BLOCK], false);
    assert_eq!(card_bytes(&cpu)[201 * BLOCK..202 * BLOCK], [0x71; BLOCK]);
    // wbusy@4: accepted, then MISO stays low until CS goes high; the block
    // is not programmed.
    cs(&mut cpu, true);
    assert_eq!(spi_cmd(&mut cpu, 24, 202), 0x00);
    xfer(&mut cpu, 0xFF);
    xfer(&mut cpu, 0xFE);
    tx_block(&mut cpu, &[0x72; BLOCK], false);
    xfer(&mut cpu, 0xFF);
    xfer(&mut cpu, 0xFF);
    assert_eq!(xfer(&mut cpu, 0xFF) & 0x1F, 0x05);
    assert!((0..10_000).all(|_| xfer(&mut cpu, 0xFF) == 0x00), "busy for good");
    cs(&mut cpu, false);
    cs(&mut cpu, true);
    assert_eq!(xfer(&mut cpu, 0xFF), 0xFF, "CS high ended it");
    cs(&mut cpu, false);
    assert_eq!(card_bytes(&cpu)[202 * BLOCK..203 * BLOCK], pattern(202)[..]);
    assert_eq!(spi_acmd22(&mut cpu), 0);
    spi_write(&mut cpu, 202, &[0x72; BLOCK], true);
    assert_eq!(spi_acmd22(&mut cpu), 1);
    let summary = card(&cpu).fault_summary().unwrap();
    assert_eq!(summary, "SD faults injected: 2 (wcrc@2: 1, wbusy@4: 1)");
}

#[test]
fn spi_read_and_command_faults() {
    let mut cpu = cpu_with_init();
    // rerr@1: CMD17 gets the error token 0x04; the retry reads the block.
    // rerr@5: counted over CMD18 blocks the host actually receives (a
    // block cut short by CMD12 is no event).
    inject(&mut cpu, &["rerr@1", "rerr@5", "cmdto:17@3x2"]);
    cs(&mut cpu, true);
    assert_eq!(spi_cmd(&mut cpu, 17, 3), 0x00);
    assert_eq!(spi_token(&mut cpu), 0x04);
    cs(&mut cpu, false);
    assert_eq!(spi_read(&mut cpu, 3, true), pattern(3), "read event 2");
    let cmd18 = |cpu: &mut Cpu, lba: u32, n: usize| -> Result<Vec<u8>, u8> {
        cs(cpu, true);
        assert_eq!(spi_cmd(cpu, 18, lba), 0x00);
        let mut got = Vec::new();
        let mut result = Ok(());
        for _ in 0..n {
            let token = spi_token(cpu);
            if token != 0xFE {
                result = Err(token);
                break;
            }
            got.extend(rx_block(cpu, true).unwrap());
            xfer(cpu, 0xFF);
            xfer(cpu, 0xFF);
        }
        for b in [0x4C, 0, 0, 0, 0, 0x61, 0xFF] {
            xfer(cpu, b);
        }
        assert_eq!((0..8).map(|_| xfer(cpu, 0xFF)).find(|r| r & 0x80 == 0), Some(0x00));
        spi_wait_not_busy(cpu);
        cs(cpu, false);
        result.map(|()| got)
    };
    assert_eq!(cmd18(&mut cpu, 10, 2), Ok([pattern(10), pattern(11)].concat()), "read events 3-4");
    assert_eq!(cmd18(&mut cpu, 20, 3), Err(0x04), "read event 5 is the first block");
    assert_eq!(cmd18(&mut cpu, 20, 3), Ok((20..23).flat_map(pattern).collect()));
    // cmdto:17@3x2: the third and fourth CMD17 get no R1, the fifth works.
    cs(&mut cpu, true);
    assert_eq!(spi_cmd(&mut cpu, 17, 0), 0xFF);
    assert_eq!(spi_cmd(&mut cpu, 17, 0), 0xFF);
    assert_eq!(spi_cmd(&mut cpu, 13, 0), 0x00, "other commands answer");
    xfer(&mut cpu, 0xFF);
    cs(&mut cpu, false);
    assert_eq!(spi_read(&mut cpu, 0, false), pattern(0));
    // A dropped CMD55 is no APP_CMD: the next command is an ordinary one.
    inject(&mut cpu, &["cmdto:55@1"]);
    cs(&mut cpu, true);
    assert_eq!(spi_cmd(&mut cpu, 55, 0), 0xFF);
    assert_eq!(spi_cmd(&mut cpu, 22, 0), 0x04, "CMD22 is illegal");
    cs(&mut cpu, false);
}

#[test]
fn native_write_faults_and_acmd22() {
    let mut cpu = cpu(true, true);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    inject(&mut cpu, &["wcrc@3", "wbusy@5"]);
    let push_block = |cpu: &mut Cpu, byte: u8| {
        for _ in 0..64 {
            wr(cpu, SDM_DATA, u64::from_le_bytes([byte; 8]));
        }
    };
    // CMD25 of four blocks: the third gets CRC status 101.
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (4 << 16) | 512);
    sdm_ok(&mut cpu, 25, 300, R1, CMD_DATA | CMD_WRITE);
    for b in 0..4 {
        push_block(&mut cpu, 0x80 + b);
    }
    let status = rd(&mut cpu, SDM_STATUS) as u32;
    assert_eq!(status & (ST_WRITE_CRC_ERR | ST_DATA_BUSY | ST_DATA_DONE), ST_WRITE_CRC_ERR);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 2);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(native_acmd22(&mut cpu), 2, "ACMD22 counts the blocks before the failed one");
    assert_eq!(card_bytes(&cpu)[301 * BLOCK..302 * BLOCK], [0x81; BLOCK]);
    assert_eq!(card_bytes(&cpu)[302 * BLOCK..303 * BLOCK], pattern(302)[..], "not programmed");
    // Linux's recovery: rewrite from the first unwritten block.  Write
    // events 4 and 5: the second block's busy never ends.
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (2 << 16) | 512);
    sdm_ok(&mut cpu, 25, 302, R1, CMD_DATA | CMD_WRITE);
    push_block(&mut cpu, 0x82);
    push_block(&mut cpu, 0x83);
    let status = rd(&mut cpu, SDM_STATUS) as u32;
    assert_eq!(status & (ST_BUSY_TIMEOUT | ST_DATA_BUSY | ST_DATA_DONE), ST_BUSY_TIMEOUT);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 1);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 6, "still rcv");
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(native_acmd22(&mut cpu), 1);
    // The retry succeeds.
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (1 << 16) | 512);
    sdm_ok(&mut cpu, 24, 303, R1, CMD_DATA | CMD_WRITE);
    push_block(&mut cpu, 0x83);
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32 & (ST_DATA_DONE | ST_ERR_ANY), ST_DATA_DONE);
    assert_eq!(native_acmd22(&mut cpu), 1);
    let want: Vec<u8> = (0x80..0x84).flat_map(|b| [b; BLOCK]).collect();
    assert_eq!(card_bytes(&cpu)[300 * BLOCK..304 * BLOCK], want[..]);
}

#[test]
fn native_read_and_command_faults() {
    let mut cpu = cpu(true, true);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    inject(&mut cpu, &["rcrc@2", "cmdto:13@1"]);
    // CMD18 of four: the second block arrives with a bad CRC and never
    // reaches the FIFO; the first stays to be drained.
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (4 << 16) | 512);
    let status = sdm(&mut cpu, 18, 40, R1, CMD_DATA);
    assert_eq!(status & (ST_DATA_CRC_ERR | ST_DATA_BUSY | ST_DATA_DONE), ST_DATA_CRC_ERR);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 1);
    assert_eq!(rd(&mut cpu, SDM_FIFO), 64);
    assert_eq!(pop(&mut cpu, 64), pattern(40));
    // cmdto: the first CMD13 gets no response and leaves SDM_RESP* alone.
    let resp = (rd(&mut cpu, SDM_RESP0), rd(&mut cpu, SDM_RESP1));
    assert_ne!(sdm(&mut cpu, 13, rca, R1, 0) & ST_CMD_TIMEOUT, 0);
    assert_eq!((rd(&mut cpu, SDM_RESP0), rd(&mut cpu, SDM_RESP1)), resp);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 5, "the card still streams");
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    // The retry reads all four.
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    sdm_ok(&mut cpu, 18, 40, R1, CMD_DATA);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 4);
    assert_eq!(pop(&mut cpu, 256), (40..44).flat_map(pattern).collect::<Vec<_>>());
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(cpu.sd_fault_summary().unwrap(), "SD faults injected: 2 (rerr@2: 1, cmdto:13@1: 1)");
    // A CMD12 that gets no response leaves the card streaming; the phase
    // still ends at the host.
    inject(&mut cpu, &["cmdto:12@1"]);
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, 512);
    sdm_ok(&mut cpu, 18, 0, R1, CMD_DATA);
    assert_ne!(sdm(&mut cpu, 12, 0, R1B, CMD_STOP) & ST_CMD_TIMEOUT, 0);
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32 & (ST_DATA_BUSY | ST_DATA_DONE), 0);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 5);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4);
}

#[test]
fn no_faults_no_summary() {
    let cpu = cpu_with_init();
    assert_eq!(cpu.sd_fault_summary(), None);
}

#[test]
fn native_write_thresh_uses_the_latched_count() {
    // FIFO_THRESH's "pushed < count" uses the count latched by the data
    // command (§4.8, klaussCPU c1e540a): rewriting SDM_BLK mid-phase
    // changes neither it nor the block size.
    let mut cpu = cpu(true, true);
    native_init(&mut cpu);
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (2 << 16) | 512);
    sdm_ok(&mut cpu, 25, 60, R1, CMD_DATA | CMD_WRITE);
    wr(&mut cpu, SDM_BLK, (1 << 16) | 8);
    for w in 0..64 {
        wr(&mut cpu, SDM_DATA, w);
    }
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 1, "a whole 512-byte block went out");
    assert_ne!(rd(&mut cpu, SDM_STATUS) as u32 & ST_FIFO_THRESH, 0, "one of two blocks pushed");
    wr(&mut cpu, SDM_BLK, (9 << 16) | 512);
    for w in 0..64 {
        wr(&mut cpu, SDM_DATA, w);
    }
    let status = rd(&mut cpu, SDM_STATUS) as u32;
    assert_eq!(
        status & (ST_FIFO_THRESH | ST_DATA_DONE | ST_DATA_BUSY),
        ST_DATA_DONE,
        "done after the latched two"
    );
}

// ---- DMA (`--sd-dma`, klaussCPU SD DMA spec v1) and the range ops ----

/// RAM of the DMA test machine: 256 KiB.
const DMA_MEM: usize = 1 << 18;
/// The DMA buffer.
const BUF: u64 = 0x1_0000;
/// `CACHE_CTRL`, `CACHE_STATUS`, `CACHE_OP_ADDR`, `CACHE_OP_LEN`.
const CACHE_CTRL_REG: u32 = 0xF005_0000;
const CACHE_STATUS_REG: u32 = 0xF005_0010;
const CACHE_OP_ADDR_REG: u32 = 0xF005_0030;
const CACHE_OP_LEN_REG: u32 = 0xF005_0038;

/// A machine with the card, the native controller, its DMA engine and
/// (`range_ops`) the D-cache range ops.
fn cpu_dma(range_ops: bool) -> Cpu {
    let mut cpu = Cpu::with_mem_size(&[0; 0x100], CODE_BASE, DMA_MEM);
    cpu.install(Hardware {
        sd_card: Some(SdCard::from_bytes(image()).unwrap()),
        sd_native: true,
        sd_dma: true,
        dcache_range_ops: range_ops,
        ..Hardware::default()
    });
    cpu
}

/// RAM bytes.
fn ram(cpu: &Cpu, addr: u64, len: usize) -> Vec<u8> {
    cpu.mem[addr as usize..addr as usize + len].to_vec()
}

/// Fill RAM without the CPU (as the loader would): no coherence events.
fn fill(cpu: &mut Cpu, addr: u64, data: &[u8]) {
    cpu.mem[addr as usize..addr as usize + data.len()].copy_from_slice(data);
}

/// A CPU store or load of a doubleword, as the coherence checker sees it.
fn cpu_store(cpu: &mut Cpu, addr: u64, value: u64) {
    cpu.note_access(addr as u32, 8, crate::emu::Access::Store);
    cpu.mem[addr as usize..addr as usize + 8].copy_from_slice(&value.to_le_bytes());
}

fn cpu_load(cpu: &mut Cpu, addr: u64) -> u64 {
    cpu.note_access(addr as u32, 8, crate::emu::Access::Load);
    u64::from_le_bytes(cpu.mem[addr as usize..addr as usize + 8].try_into().unwrap())
}

fn problems(cpu: &Cpu) -> u64 {
    cpu.coherence.as_ref().unwrap().problems()
}

/// Let `cycles` pass, 64 at a time.
fn pass(cpu: &mut Cpu, cycles: u64) {
    for _ in 0..cycles.div_ceil(64) {
        cpu.tick(64);
    }
}

/// Let time pass until `done`, at most `limit` cycles; the cycles it took.
fn until(cpu: &mut Cpu, limit: u64, mut done: impl FnMut(&mut Cpu) -> bool) -> u64 {
    let start = cpu.cycles;
    while !done(cpu) {
        assert!(cpu.cycles - start < limit, "timed out");
        cpu.tick(16);
    }
    cpu.cycles - start
}

fn status(cpu: &mut Cpu) -> u32 {
    rd(cpu, SDM_STATUS) as u32
}

/// Start a data command by DMA: FIFO reset, `SDM_BLK`, `SDM_DMA_ADDR`,
/// `DMA_EN` on the 4-bit bus, then the command.
fn dma_cmd(cpu: &mut Cpu, index: u64, arg: u32, blocks: u64, size: u64, addr: u64, write: bool) {
    // SDM_DMA_ADDR is ignored while DMA_BUSY: wait out a drain first.
    until(cpu, 100_000, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    wr(cpu, SDM_FIFO, FIFO_RESET);
    wr(cpu, SDM_BLK, (blocks << 16) | size);
    wr(cpu, SDM_DMA_ADDR, addr);
    wr(cpu, SDM_CFG, CFG_4BIT | CFG_DMA_EN);
    sdm_ok(cpu, index, arg, R1, CMD_DATA | if write { CMD_WRITE } else { 0 });
}

#[test]
fn dma_registers_and_caps() {
    // Without --sd-dma: today's bitstream.  SDM_DMA_ADDR and SDM_CFG[1]
    // are reserved (read 0), SDM_CAPS[10] = 0, IRQ_EN[27] does not stick,
    // and without --dcache-range-ops CPU_FEATURES[27] = 0 and the range
    // registers read 0.
    let mut cpu = cpu(true, true);
    assert_eq!(rd(&mut cpu, SDM_CAPS), 0x0708_0B01);
    wr(&mut cpu, SDM_DMA_ADDR, 0x1000);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), 0);
    wr(&mut cpu, SDM_CFG, 0b111 | CFG_DMA_EN);
    assert_eq!(rd(&mut cpu, SDM_CFG), 0b111);
    wr(&mut cpu, SDM_IRQ_EN, u64::from(ST_DMA_ERR | ST_DATA_DONE));
    assert_eq!(rd(&mut cpu, SDM_IRQ_EN), u64::from(ST_DATA_DONE));
    assert_eq!(cpu.mmio_read(CPU_FEATURES) & (1 << 27), 0);
    cpu.mmio_write(CACHE_OP_ADDR_REG, 0x1000);
    cpu.mmio_write(CACHE_OP_LEN_REG, 0x40);
    assert_eq!(cpu.mmio_read(CACHE_OP_ADDR_REG), 0);
    assert_eq!(cpu.mmio_read(CACHE_OP_LEN_REG), 0);
    assert!(cpu.sd_dma_summary().is_none());

    assert_eq!(cpu.mmio_read(CPU_FEATURES), 0x02E0_200D, "--sd-native");
    // With it: SDM_CAPS[10], the version still 1.  CPU_FEATURES with
    // --sd-native --sd-dma --dcache-range-ops: the sddma bitstream's
    // 0x0FF0_200D less [20] TRNG, [24] ETH_RX_SLOTS and [26] L2, which
    // the emulator does not model (as without these options).
    assert_eq!(cpu_dma(true).mmio_read(CPU_FEATURES), 0x0AE0_200D);
    let mut cpu = cpu_dma(false);
    assert_eq!(cpu.mmio_read(CPU_FEATURES), 0x02E0_200D, "--sd-dma alone adds no CPU_FEATURES bit");
    assert_eq!(rd(&mut cpu, SDM_CAPS), 0x0708_0F01, "the sdhs bitstream's");
    assert_eq!(rd(&mut cpu, SDM_CAPS) & 0xFF, 1);
    assert_eq!(cpu.mmio_read(CPU_FEATURES) & (1 << 27), 0, "the range ops are a separate option");
    assert_eq!(cpu.mmio_read(CPU_FEATURES) & (1 << 21), 1 << 21);
    // SDM_DMA_ADDR: 64-bit, [2:0] read 0.
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), 0, "reset 0");
    wr(&mut cpu, SDM_DMA_ADDR, 0x1234_5677);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), 0x1234_5670);
    wr(&mut cpu, SDM_DMA_ADDR, 0xABCD_0001_0000_000F);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), 0xABCD_0001_0000_0008);
    // DMA_EN is SDM_CFG[16] (spec v1.1); [1] is not it.
    assert_eq!(CFG_DMA_EN, 1 << 16);
    wr(&mut cpu, SDM_CFG, 0b101 | CFG_DMA_EN);
    assert_eq!(rd(&mut cpu, SDM_CFG), 0b101 | CFG_DMA_EN);
    // IRQ_EN[27] enables DMA_ERR.
    wr(&mut cpu, SDM_IRQ_EN, u64::from(ST_DMA_ERR | ST_DATA_DONE));
    assert_eq!(rd(&mut cpu, SDM_IRQ_EN), u64::from(ST_DMA_ERR | ST_DATA_DONE));
    assert_eq!(status(&mut cpu) & (ST_DMA_BUSY | ST_DMA_ERR), 0);
    assert_eq!(
        cpu.sd_dma_summary().unwrap(),
        "SD DMA: 0 data commands, 0 blocks read, 0 written, 0 aborts, 0 DMA_ERR, 0 card errors; DMA coherence problems: 0"
    );
}

#[test]
fn dma_multi_block_read() {
    let mut cpu = cpu_dma(true);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    fill(&mut cpu, BUF, &[0xEE; 12 * BLOCK]);
    wr(&mut cpu, SDM_IRQ_EN, u64::from(ST_DATA_DONE | ST_DMA_ERR));
    cpu.mmio_write(INT2_EN, 0b10);
    dma_cmd(&mut cpu, 18, 20, 10, 512, BUF, false);
    // The blocks come at the card's pace: nothing yet.
    let st = status(&mut cpu);
    assert_eq!(
        st & (ST_DATA_BUSY | ST_DMA_BUSY | ST_DATA_DONE | ST_FIFO_THRESH),
        ST_DATA_BUSY | ST_DMA_BUSY
    );
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 0);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF);
    assert_eq!(cpu.mmio_read(INT2_PENDING), 0);
    // SDM_DATA is not used: it reads 0 and pops nothing.
    assert_eq!(rd(&mut cpu, SDM_DATA), 0);
    // Block by block: SDM_DMA_ADDR advances as a block's bursts land in
    // DDR, a little after BLKDONE counts it (it lags while DMA_BUSY), the
    // FIFO stays empty, and DATA_DONE (and the interrupt) only comes when
    // the last byte is in DDR.
    let mut seen = 0;
    let mut lagged = false;
    until(&mut cpu, 10_000_000, |cpu| {
        let done = status(cpu) & ST_DATA_DONE != 0;
        let blocks = rd(cpu, SDM_BLKDONE);
        let landed = (rd(cpu, SDM_DMA_ADDR) - BUF) / 512;
        assert!(landed == blocks || landed + 1 == blocks, "{landed} of {blocks}");
        lagged |= landed < blocks;
        assert_eq!(rd(cpu, SDM_FIFO), 0);
        let n = landed as usize;
        assert_eq!(ram(cpu, BUF, n * BLOCK), (20..20 + n).flat_map(pattern).collect::<Vec<_>>());
        assert_eq!(ram(cpu, BUF + n as u64 * 512, BLOCK), vec![0xEE; BLOCK], "nothing past the progress");
        assert_eq!(cpu.mmio_read(INT2_PENDING) == 2, done, "the interrupt is DATA_DONE");
        if done {
            assert_eq!(blocks, 10, "DATA_DONE after the last block");
        }
        seen = seen.max(blocks);
        done
    });
    assert_eq!(seen, 10);
    assert!(lagged, "a block counted and not yet in DDR");
    let st = status(&mut cpu);
    assert_eq!(st & (ST_DATA_BUSY | ST_DMA_BUSY | ST_DATA_DONE | ST_ERR_ANY), ST_DATA_DONE);
    assert_eq!(ram(&cpu, BUF, 10 * BLOCK), (20..30).flat_map(pattern).collect::<Vec<_>>());
    assert_eq!(ram(&cpu, BUF + 10 * 512, 2 * BLOCK), vec![0xEE; 2 * BLOCK]);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 10 * 512, "the next address");
    wr(&mut cpu, SDM_STATUS, u64::from(ST_DATA_DONE));
    assert_eq!(cpu.mmio_read(INT2_PENDING), 0);
    // The card streams on until CMD12, as without DMA.
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 5, "data");
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");
    // The pace: ten 512-byte blocks on the 4-bit bus at 25 MHz take about
    // 10 x 1050 clocks of 4 cycles.
    dma_cmd(&mut cpu, 18, 0, 10, 512, BUF, false);
    let cycles = until(&mut cpu, 10_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert!((40_000..50_000).contains(&cycles), "{cycles} cycles");
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    // CMD17 and CMD23 + CMD18 need no CMD12.
    dma_cmd(&mut cpu, 17, 5, 1, 512, BUF + 0x8000, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(ram(&cpu, BUF + 0x8000, BLOCK), pattern(5));
    sdm_ok(&mut cpu, 23, 3, R1, 0);
    dma_cmd(&mut cpu, 18, 100, 3, 512, BUF + 0x9000, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(ram(&cpu, BUF + 0x9000, 3 * BLOCK), (100..103).flat_map(pattern).collect::<Vec<_>>());
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");
    // On the 1-bit bus a block takes four times as long.
    sdm_ok(&mut cpu, 55, rca, R1, 0);
    sdm_ok(&mut cpu, 6, 0, R1, 0);
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (1 << 16) | 512);
    wr(&mut cpu, SDM_DMA_ADDR, BUF);
    wr(&mut cpu, SDM_CFG, CFG_DMA_EN);
    sdm_ok(&mut cpu, 17, 9, R1, CMD_DATA);
    let cycles = until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert!((16_000..18_000).contains(&cycles), "{cycles} cycles");
    assert_eq!(ram(&cpu, BUF, BLOCK), pattern(9));
    assert!(
        cpu.sd_dma_summary()
            .unwrap()
            .starts_with("SD DMA: 5 data commands, 25 blocks read, 0 written, 0 aborts")
    );
    assert_eq!(problems(&cpu), 0);
}

#[test]
fn dma_multi_block_write() {
    let mut cpu = cpu_dma(true);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    let data: Vec<u8> = (0..12 * BLOCK).map(|i| (i as u8).wrapping_mul(7) ^ (i >> 9) as u8).collect();
    fill(&mut cpu, BUF, &data);
    wr(&mut cpu, SDM_IRQ_EN, u64::from(ST_DATA_DONE));
    cpu.mmio_write(INT2_EN, 0b10);
    // Five blocks: the engine fetches them all into the FIFO at once
    // (SDM_DMA_ADDR is past them), then the card takes them at its pace;
    // DATA_DONE when the last one is programmed, and DMA_BUSY drops with
    // it (spec v1.1 item 7).
    dma_cmd(&mut cpu, 25, 40, 5, 512, BUF, true);
    tick(&mut cpu);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 5 * 512, "fetched ahead");
    assert_eq!(rd(&mut cpu, SDM_FIFO), 5 * 64);
    let st = status(&mut cpu);
    assert_eq!(
        st & (ST_DATA_BUSY | ST_DMA_BUSY | ST_DATA_DONE | ST_FIFO_THRESH),
        ST_DATA_BUSY | ST_DMA_BUSY
    );
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 0);
    until(&mut cpu, 10_000_000, |cpu| {
        let st = status(cpu);
        let done = st & ST_DATA_DONE != 0;
        assert_eq!(st & ST_DMA_BUSY == 0, done, "DMA_BUSY drops with DATA_DONE");
        let blocks = rd(cpu, SDM_BLKDONE) as usize;
        assert_eq!(card_bytes(cpu)[40 * BLOCK..(40 + blocks) * BLOCK], data[..blocks * BLOCK]);
        assert_eq!(cpu.mmio_read(INT2_PENDING) == 2, done);
        if done {
            assert_eq!(blocks, 5);
        }
        done
    });
    assert_eq!(card_bytes(&cpu)[45 * BLOCK..46 * BLOCK], pattern(45)[..]);
    let r1 = sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(state_of(r1), 6, "rcv");
    assert_eq!(native_acmd22(&mut cpu), 5);
    // Twelve blocks: the FIFO holds eight, so the engine stays busy and
    // fetches a block each time the card takes one.
    dma_cmd(&mut cpu, 25, 60, 12, 512, BUF, true);
    tick(&mut cpu);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 8 * 512);
    assert_eq!(rd(&mut cpu, SDM_FIFO), 512);
    assert_ne!(status(&mut cpu) & ST_DMA_BUSY, 0);
    until(&mut cpu, 10_000_000, |cpu| rd(cpu, SDM_BLKDONE) == 2);
    tick(&mut cpu);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 10 * 512);
    until(&mut cpu, 10_000_000, |cpu| rd(cpu, SDM_DMA_ADDR) == BUF + 12 * 512);
    assert_eq!(
        status(&mut cpu) & (ST_DATA_DONE | ST_DMA_BUSY),
        ST_DMA_BUSY,
        "all fetched, the card still has blocks to program"
    );
    until(&mut cpu, 10_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(status(&mut cpu) & ST_DMA_BUSY, 0);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 12 * 512, "never fetched past size x count");
    assert_eq!(card_bytes(&cpu)[60 * BLOCK..72 * BLOCK], data[..]);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");
    // A single block (CMD24), read back by DMA.
    dma_cmd(&mut cpu, 24, 80, 1, 512, BUF + 3 * 512, true);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    dma_cmd(&mut cpu, 17, 80, 1, 512, BUF + 0x2_0000, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(ram(&cpu, BUF + 0x2_0000, BLOCK), data[3 * BLOCK..4 * BLOCK]);
    assert_eq!(problems(&cpu), 0);
}

#[test]
fn dma_abort_by_dma_en_and_fifo_reset() {
    let mut cpu = cpu_dma(false);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    fill(&mut cpu, BUF, &[0xEE; 16 * BLOCK]);
    // A read of 16 blocks, aborted by DMA_EN = 0 after three: the address
    // holds the next one, DMA_BUSY drops once the block in progress has
    // landed, and nothing more reaches DDR.
    dma_cmd(&mut cpu, 18, 0, 16, 512, BUF, false);
    until(&mut cpu, 10_000_000, |cpu| rd(cpu, SDM_BLKDONE) == 3);
    wr(&mut cpu, SDM_CFG, CFG_4BIT);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 2 * 512, "lags while DMA_BUSY");
    assert_ne!(status(&mut cpu) & ST_DMA_BUSY, 0, "the burst in flight completes");
    let drained = until(&mut cpu, 1000, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    assert!(drained <= 300, "{drained}");
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 3 * 512, "final once DMA_BUSY dropped");
    pass(&mut cpu, 200_000);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 3 * 512);
    assert_eq!(ram(&cpu, BUF, 3 * BLOCK), (0..3).flat_map(pattern).collect::<Vec<_>>());
    assert_eq!(ram(&cpu, BUF + 3 * 512, 13 * BLOCK), vec![0xEE; 13 * BLOCK]);
    // The card side is recovered as without DMA: FIFO reset, CMD12.
    assert_ne!(status(&mut cpu) & ST_DATA_BUSY, 0);
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(status(&mut cpu) & (ST_DATA_BUSY | ST_DMA_BUSY | ST_ERR_ANY), 0);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");

    // A read aborted by SDM_FIFO[31] while its second block is on its way
    // to DDR: the engine stops after the 16-byte transaction in flight, so
    // part of the block lands (SDM_DMA_ADDR says how much) and no more.
    fill(&mut cpu, BUF, &[0xEE; 16 * BLOCK]);
    dma_cmd(&mut cpu, 18, 0, 16, 512, BUF, false);
    until(&mut cpu, 10_000_000, |cpu| rd(cpu, SDM_BLKDONE) == 2);
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    assert_ne!(status(&mut cpu) & ST_DMA_BUSY, 0);
    until(&mut cpu, 1000, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    pass(&mut cpu, 100_000);
    let moved = rd(&mut cpu, SDM_DMA_ADDR) - BUF - 512;
    assert!(moved > 0 && moved < 512 && moved.is_multiple_of(16), "{moved}");
    assert_eq!(
        ram(&cpu, BUF, 512 + moved as usize),
        [pattern(0), pattern(1)[..moved as usize].to_vec()].concat()
    );
    assert_eq!(ram(&cpu, BUF + 512 + moved, 512), vec![0xEE; 512], "the rest dropped");
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");

    // A write of 16 blocks aborted by SDM_FIFO[31] after two reached the
    // card: the address shows the fetches (ahead of the card), and the
    // card got only what it had.
    let data: Vec<u8> = (0..16 * BLOCK).map(|i| ((i * 13) >> 3) as u8).collect();
    fill(&mut cpu, BUF, &data);
    dma_cmd(&mut cpu, 25, 200, 16, 512, BUF, true);
    until(&mut cpu, 10_000_000, |cpu| rd(cpu, SDM_BLKDONE) == 2);
    tick(&mut cpu);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 10 * 512, "eight blocks ahead");
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    until(&mut cpu, 1000, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    pass(&mut cpu, 100_000);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 10 * 512);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 2);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");
    assert_eq!(native_acmd22(&mut cpu), 2);
    assert_eq!(card_bytes(&cpu)[200 * BLOCK..202 * BLOCK], data[..2 * BLOCK]);
    assert_eq!(card_bytes(&cpu)[202 * BLOCK..203 * BLOCK], pattern(202)[..]);

    // A write aborted by DMA_EN = 0.
    dma_cmd(&mut cpu, 25, 300, 16, 512, BUF, true);
    until(&mut cpu, 10_000_000, |cpu| rd(cpu, SDM_BLKDONE) == 1);
    wr(&mut cpu, SDM_CFG, CFG_4BIT);
    until(&mut cpu, 1000, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    let addr = rd(&mut cpu, SDM_DMA_ADDR);
    pass(&mut cpu, 100_000);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), addr, "the engine stays stopped");
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");
    assert!(cpu.sd_dma_summary().unwrap().contains(", 4 aborts,"));
}

/// Write the registers of a DMA data command and the command itself, as
/// `dma_cmd`, without expecting `CMD_DONE`; returns `SDM_STATUS`.
fn dma_cmd_raw(cpu: &mut Cpu, index: u64, blocks: u64, size: u64, addr: u64, write: bool) -> u32 {
    until(cpu, 100_000, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    wr(cpu, SDM_FIFO, FIFO_RESET);
    wr(cpu, SDM_BLK, (blocks << 16) | size);
    wr(cpu, SDM_DMA_ADDR, addr);
    wr(cpu, SDM_CFG, CFG_4BIT | CFG_DMA_EN);
    wr(cpu, SDM_STATUS, u64::from(ST_STICKY | ST_DMA_ERR));
    wr(cpu, SDM_ARG, 0);
    wr(cpu, SDM_CMD, cmd_word(index, R1, CMD_DATA | if write { CMD_WRITE } else { 0 }));
    status(cpu)
}

/// A data command refused up front (spec v1.1 items 2 and 5): `DMA_ERR`
/// at once, no `CMD_DONE`, no data phase, nothing moved, and the card got
/// nothing (it is still in tran).
fn assert_refused(cpu: &mut Cpu, st: u32, addr: u64) {
    assert_eq!(
        st & (ST_DMA_ERR | ST_ERR_ANY | ST_CMD_DONE | ST_DATA_BUSY | ST_DMA_BUSY | ST_DATA_DONE),
        ST_DMA_ERR | ST_ERR_ANY
    );
    assert_eq!(rd(cpu, SDM_DMA_ADDR), addr);
    wr(cpu, SDM_STATUS, u64::from(ST_DMA_ERR));
    assert_eq!(
        state_of(sdm_ok(cpu, 13, u32::from(RCA) << 16, R1, 0)),
        4,
        "tran: no command reached the card"
    );
}

#[test]
fn dma_err_outside_ddr() {
    let mut cpu = cpu_dma(false);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    let end = DMA_MEM as u64;
    wr(&mut cpu, SDM_IRQ_EN, u64::from(ST_DMA_ERR));
    cpu.mmio_write(INT2_EN, 0b10);
    fill(&mut cpu, end - 4 * 512, &[0xEE; 4 * BLOCK]);
    // With a block count the whole transfer is checked when SDM_CMD is
    // written: four blocks from two before DDR's end are refused.
    let st = dma_cmd_raw(&mut cpu, 18, 4, 512, end - 2 * 512, false);
    assert_eq!(cpu.mmio_read(INT2_PENDING), 2, "IRQ_EN[27]");
    assert_refused(&mut cpu, st, end - 2 * 512);
    assert_eq!(ram(&cpu, end - 2 * 512, 2 * BLOCK), vec![0xEE; 2 * BLOCK]);
    assert_eq!(cpu.mmio_read(INT2_PENDING), 0, "W1C");
    // Exactly to the end is fine.
    dma_cmd(&mut cpu, 18, 7, 2, 512, end - 2 * 512, false);
    until(&mut cpu, 10_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(ram(&cpu, end - 2 * 512, 2 * BLOCK), (7..9).flat_map(pattern).collect::<Vec<_>>());
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    // A counted write that would read past the end, and one from above
    // 4 GiB: refused.
    let st = dma_cmd_raw(&mut cpu, 25, 2, 512, end - 512, true);
    assert_refused(&mut cpu, st, end - 512);
    let st = dma_cmd_raw(&mut cpu, 25, 2, 512, 1 << 32, true);
    assert_refused(&mut cpu, st, 1 << 32);
    // A block size not a multiple of 8 (ACMD22's 4) is refused too.
    let st = dma_cmd_raw(&mut cpu, 17, 1, 516, BUF, false);
    assert_refused(&mut cpu, st, BUF);

    // Open-ended (count 0): each block is checked before it moves.  Two
    // land, the third gets DMA_ERR: nothing of it reaches DDR, the address
    // is the first byte not transferred, and the card side goes on into
    // the FIFO (the failed block first) until it is full.
    fill(&mut cpu, end - 2 * 512, &[0xEE; 2 * BLOCK]);
    dma_cmd(&mut cpu, 18, 0, 0, 512, end - 2 * 512, false);
    until(&mut cpu, 10_000_000, |cpu| status(cpu) & ST_ERR_ANY != 0);
    let st = status(&mut cpu);
    assert_eq!(st & (ST_DMA_ERR | ST_DATA_BUSY | ST_DATA_DONE), ST_DMA_ERR | ST_DATA_BUSY);
    assert_eq!(st & ST_ERRORS, 0, "only DMA_ERR");
    until(&mut cpu, 1000, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), end);
    assert_eq!(ram(&cpu, end - 2 * 512, 2 * BLOCK), (0..2).flat_map(pattern).collect::<Vec<_>>());
    assert_eq!(rd(&mut cpu, SDM_FIFO), FIFO_WORDS as u64, "the FIFO filled");
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 10, "good blocks received: 2 in DDR, 8 in the FIFO");
    assert_eq!(pop(&mut cpu, BLOCK / 8), pattern(2), "the failed block is the FIFO's first");
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");
    // A block that would cross the end: nothing of it is written.
    fill(&mut cpu, end - 256, &[0xEE; 256]);
    dma_cmd(&mut cpu, 18, 0, 0, 512, end - 256, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_ERR_ANY != 0);
    assert_eq!(status(&mut cpu) & (ST_DMA_ERR | ST_DATA_DONE), ST_DMA_ERR);
    assert_eq!(ram(&cpu, end - 256, 256), vec![0xEE; 256]);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), end - 256);
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    // An open-ended write from outside DDR: DMA_ERR at the first fetch;
    // the card side waits for data that never comes, until CMD12.
    dma_cmd(&mut cpu, 25, 0, 0, 512, 1 << 32, true);
    tick(&mut cpu);
    assert_eq!(status(&mut cpu) & (ST_DMA_ERR | ST_DATA_BUSY), ST_DMA_ERR | ST_DATA_BUSY);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), 1 << 32);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");
    assert_eq!(native_acmd22(&mut cpu), 0);
    assert!(
        cpu.sd_dma_summary().unwrap().contains(", 7 DMA_ERR,"),
        "{}",
        cpu.sd_dma_summary().unwrap()
    );
}

#[test]
fn dma_card_errors_and_odd_blocks() {
    let mut cpu = cpu_dma(false);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    // A bad CRC on the third block: two blocks in DDR, the address after
    // them, DATA_CRC_ERR and no DATA_DONE; CMD12 recovers.
    inject(&mut cpu, &["rcrc@3", "wcrc@2"]);
    fill(&mut cpu, BUF, &[0xEE; 4 * BLOCK]);
    dma_cmd(&mut cpu, 18, 10, 4, 512, BUF, false);
    until(&mut cpu, 10_000_000, |cpu| status(cpu) & ST_ERR_ANY != 0);
    assert_eq!(
        status(&mut cpu) & (ST_DATA_CRC_ERR | ST_DATA_DONE | ST_DATA_BUSY | ST_DMA_BUSY),
        ST_DATA_CRC_ERR
    );
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 2);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 2 * 512);
    assert_eq!(ram(&cpu, BUF, 2 * BLOCK), (10..12).flat_map(pattern).collect::<Vec<_>>());
    assert_eq!(ram(&cpu, BUF + 2 * 512, BLOCK), vec![0xEE; BLOCK], "the bad block does not land");
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    // A write whose second block gets a CRC error.
    dma_cmd(&mut cpu, 25, 500, 3, 512, BUF, true);
    until(&mut cpu, 10_000_000, |cpu| status(cpu) & ST_ERR_ANY != 0);
    assert_eq!(status(&mut cpu) & (ST_WRITE_CRC_ERR | ST_DATA_DONE | ST_DATA_BUSY), ST_WRITE_CRC_ERR);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 1);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");
    assert_eq!(native_acmd22(&mut cpu), 1);
    assert!(cpu.sd_dma_summary().unwrap().contains(", 2 card errors"));
    // ACMD22 by DMA: its 4-byte block is not a multiple of 8, so the
    // engine refuses it up front (spec v1.1 item 2); by PIO it works.
    fill(&mut cpu, BUF, &[0xEE; 16]);
    let st = dma_cmd_raw(&mut cpu, 22, 1, 4, BUF, false);
    assert_refused(&mut cpu, st, BUF);
    assert_eq!(ram(&cpu, BUF, 16), [0xEE; 16]);
    assert_eq!(native_acmd22(&mut cpu), 1);
    // ACMD51's 8-byte SCR.
    sdm_ok(&mut cpu, 55, rca, R1, 0);
    dma_cmd(&mut cpu, 51, 0, 1, 8, BUF + 64, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(ram(&cpu, BUF + 64, 1)[0] & 0xF, 2, "SD_SPEC 2");
}

#[test]
fn dcache_range_ops_and_mnt_busy() {
    // Without the option: CPU_FEATURES[27] = 0, the registers read 0,
    // CACHE_CTRL[5:4] do nothing; FLUSH still keeps MNT_BUSY up for its
    // walk, 4,097 cycles (once: the RTL's double walk is being fixed).
    let mut cpu = cpu(false, false);
    assert_eq!(cpu.mmio_read(CPU_FEATURES) & (1 << 27), 0);
    assert_eq!(cpu.mmio_read(CACHE_STATUS_REG), 0);
    cpu.mmio_write(CACHE_OP_LEN_REG, 0x1000);
    cpu.mmio_write(CACHE_CTRL_REG, 0b11_0000);
    assert_eq!(cpu.mmio_read(CACHE_STATUS_REG), 0, "no range ops");
    cpu.mmio_write(CACHE_CTRL_REG, 0b10);
    assert_eq!(cpu.mmio_read(CACHE_STATUS_REG), 1, "FLUSH walks");
    pass(&mut cpu, 4096);
    assert_eq!(cpu.mmio_read(CACHE_STATUS_REG), 1);
    cpu.tick(1);
    assert_eq!(cpu.mmio_read(CACHE_STATUS_REG), 0, "4,097 cycles");

    let mut cpu = cpu_dma(true);
    assert_eq!(cpu.mmio_read(CPU_FEATURES) & (1 << 27), 1 << 27);
    // The registers read back, ADDR's [4:0] as 0 and LEN's [31:0] only;
    // CACHE_CTRL's bits self-clear (it reads 0).
    cpu.mmio_write(CACHE_OP_ADDR_REG, 0x1_2345);
    cpu.mmio_write(CACHE_OP_LEN_REG, 0x7_0000_4000);
    assert_eq!(cpu.mmio_read(CACHE_OP_ADDR_REG), 0x1_2340);
    assert_eq!(cpu.mmio_read(CACHE_OP_LEN_REG), 0x4000);
    cpu.mmio_write(CACHE_CTRL_REG, 0b01_0000);
    assert_eq!(cpu.mmio_read(CACHE_CTRL_REG), 0);
    assert_eq!(cpu.mmio_read(CACHE_STATUS_REG), 1);
    let busy = until(&mut cpu, 100_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    // 0x4000 bytes = 512 lines, nothing dirty: 1 + 2 per set.
    assert!((1025..1025 + 16).contains(&busy), "{busy}");
    // LEN = 0 is a no-op.
    cpu.mmio_write(CACHE_OP_LEN_REG, 0);
    cpu.mmio_write(CACHE_CTRL_REG, 0b11_0000);
    assert_eq!(cpu.mmio_read(CACHE_STATUS_REG), 0);
    // FLUSH with range bits: the whole-cache op only.
    cpu.mmio_write(CACHE_OP_LEN_REG, 0x4000);
    cpu.mmio_write(CACHE_CTRL_REG, 0b11_0010);
    let busy = until(&mut cpu, 100_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    assert!((4097..4097 + 16).contains(&busy), "{busy}");
    // A whole-cache walk: + 3 per dirty line.
    for k in 0..20 {
        cpu_store(&mut cpu, BUF + 32 * k, k);
    }
    cpu.mmio_write(CACHE_CTRL_REG, 0b10);
    let busy = until(&mut cpu, 100_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    assert!((4157..4157 + 16).contains(&busy), "{busy}");
    // ADDR / LEN are copied when an op is accepted: rewriting them while
    // it is queued (legal) does not change it.  Lines A and B dirty; a
    // clean of A queued behind a FLUSH; then the registers point at B.
    let (a, b) = (BUF + 0x2000, BUF + 0x3000);
    cpu_store(&mut cpu, a, 1);
    cpu.mmio_write(CACHE_CTRL_REG, 0b10);
    cpu_store(&mut cpu, b, 1);
    cpu.mmio_write(CACHE_OP_ADDR_REG, a);
    cpu.mmio_write(CACHE_OP_LEN_REG, 32);
    cpu.mmio_write(CACHE_CTRL_REG, 0b01_0000);
    cpu.mmio_write(CACHE_OP_ADDR_REG, b);
    until(&mut cpu, 100_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    assert_eq!(problems(&cpu), 0, "writing the registers during a walk is fine");
    native_init(&mut cpu);
    dma_cmd(&mut cpu, 24, 1, 1, 512, a, true);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(problems(&cpu), 0, "A was cleaned");
    dma_cmd(&mut cpu, 24, 1, 1, 512, b, true);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(problems(&cpu), 1, "B was not");
    assert!(!cpu.sd_dma_summary().unwrap().contains("whole-cache"), "no conversion so far");
}

/// Wait for `MNT_BUSY` to drop, then time `ctrl` over `[addr, + len)`.
fn walk_cycles(cpu: &mut Cpu, addr: u64, len: u64, ctrl: u64) -> u64 {
    until(cpu, 1_000_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    cpu.mmio_write(CACHE_OP_ADDR_REG, addr);
    cpu.mmio_write(CACHE_OP_LEN_REG, len);
    cpu.mmio_write(CACHE_CTRL_REG, ctrl);
    let start = cpu.cycles;
    while cpu.mmio_read(CACHE_STATUS_REG) != 0 {
        cpu.tick(1);
    }
    cpu.cycles - start
}

#[test]
fn range_walk_timings() {
    // HW_PERF_PLAN.md §7 results (tb_soc): 1 + 2 per set for min(n, 1024)
    // sets, + 6 for a set with a dirty line of the range (+ 3 for a
    // second one); RANGE_INV costs the same as RANGE_CLEAN.
    let mut cpu = Cpu::with_mem_size(&[0; 0x100], CODE_BASE, 1 << 20);
    cpu.install(Hardware {
        sd_card: Some(SdCard::from_bytes(image()).unwrap()),
        sd_native: true,
        sd_dma: true,
        dcache_range_ops: true,
        ..Hardware::default()
    });
    let base = 0x1_0000;
    let dirty = |cpu: &mut Cpu, lines: u64| {
        for k in 0..lines {
            cpu_store(cpu, base + 32 * k, k);
        }
    };
    assert_eq!(walk_cycles(&mut cpu, base, 32 * 32, 0b01_0000), 65, "32 lines clean");
    dirty(&mut cpu, 32);
    assert_eq!(walk_cycles(&mut cpu, base, 32 * 32, 0b10_0000), 257, "32 lines dirty, RANGE_INV");
    assert_eq!(walk_cycles(&mut cpu, base, 32 * 1024, 0b01_0000), 2049, "1,024 lines clean");
    dirty(&mut cpu, 1024);
    assert_eq!(walk_cycles(&mut cpu, base, 32 * 1024, 0b01_0000), 8193, "1,024 lines dirty");
    // 1,536 lines: 1,024 sets, 512 of them with two range lines.
    dirty(&mut cpu, 1536);
    assert_eq!(walk_cycles(&mut cpu, base, 32 * 1536, 0b11_0000), 1 + 2048 + 6 * 1024 + 3 * 512);
    // A dirty line outside the range costs nothing; one line of a range
    // that starts mid-line ([4:0] read 0) is the line it starts in.
    cpu_store(&mut cpu, base + 0x8000, 1);
    assert_eq!(walk_cycles(&mut cpu, base, 64, 0b01_0000), 5);
    assert_eq!(walk_cycles(&mut cpu, base + 0x8000 + 31, 1, 0b01_0000), 9);
    // The whole cache: 4,097 + 3 per dirty line.
    dirty(&mut cpu, 100);
    assert_eq!(walk_cycles(&mut cpu, 0, 0, 0b100), 4097 + 300);
    // RANGE_IINV: min(n, 64) cycles; a range of the I-cache's size or more,
    // or one that meets a sweep running, sweeps everything (64).
    assert_eq!(walk_cycles(&mut cpu, base, 32, 0b100_0000), 1);
    assert_eq!(walk_cycles(&mut cpu, base, 32 * 32, 0b100_0000), 32);
    assert_eq!(walk_cycles(&mut cpu, base, 4096, 0b100_0000), 64, "4 KiB: 128 lines");
    assert_eq!(walk_cycles(&mut cpu, base, 1 << 20, 0b100_0000), 64);
    cpu.mmio_write(CACHE_OP_LEN_REG, 32 * 8);
    cpu.mmio_write(CACHE_CTRL_REG, 0b100_0000);
    cpu.tick(4);
    cpu.mmio_write(CACHE_CTRL_REG, 0b100_0000);
    let start = cpu.cycles;
    while cpu.mmio_read(CACHE_STATUS_REG) != 0 {
        cpu.tick(1);
    }
    assert_eq!(cpu.cycles - start, 64, "a second sweep during one sweeps everything");
    // RANGE_IINV with FLUSH: both happen (the sweep under the walk).
    cpu.mmio_write(CACHE_OP_LEN_REG, 32 * 8);
    cpu.mmio_write(CACHE_CTRL_REG, 0b100_0010);
    assert_eq!(cpu.cache_ctl.isweep_until, cpu.cycles + 8, "the sweep runs");
    while cpu.mmio_read(CACHE_STATUS_REG) != 0 {
        cpu.tick(1);
    }
    // ICACHE_INV is no walk: MNT_BUSY stays 0.
    cpu.mmio_write(CACHE_CTRL_REG, 0b1000);
    assert_eq!(cpu.mmio_read(CACHE_STATUS_REG), 0);
    // An I-cache sweep and a D-cache walk run side by side.
    cpu.mmio_write(CACHE_OP_LEN_REG, 32 * 8);
    cpu.mmio_write(CACHE_CTRL_REG, 0b101_0000);
    let start = cpu.cycles;
    while cpu.mmio_read(CACHE_STATUS_REG) != 0 {
        cpu.tick(1);
    }
    assert_eq!(cpu.cycles - start, 17, "the D walk (17) covers the sweep (8)");
}

#[test]
fn cache_op_addr_is_32_bits() {
    // CACHE_OP_ADDR is a 32-bit register (DDR is 128 MiB): the upper half
    // of a 64-bit store is dropped, a store to 0x34 does nothing, and the
    // range is the low 32 bits'.
    let mut cpu = cpu_dma(true);
    cpu.mmio_write(CACHE_OP_ADDR_REG, 0xABCD_0001_0001_2345);
    assert_eq!(cpu.mmio_read(CACHE_OP_ADDR_REG), 0x1_2340);
    cpu.mmio_store(CACHE_OP_ADDR_REG + 4, 0xFFFF_FFFF, 4);
    assert_eq!(cpu.mmio_read(CACHE_OP_ADDR_REG), 0x1_2340);
    cpu.mmio_write(CACHE_OP_ADDR_REG, 0xFFFF_FFFF_FFFF_FFFF);
    assert_eq!(cpu.mmio_read(CACHE_OP_ADDR_REG), 0xFFFF_FFE0);
    // A clean "above 4 GiB" cleans the line at the address's low 32 bits.
    cpu_store(&mut cpu, BUF, 1);
    cpu.mmio_write(CACHE_OP_ADDR_REG, (1 << 32) | BUF);
    cpu.mmio_write(CACHE_OP_LEN_REG, 32);
    cpu.mmio_write(CACHE_CTRL_REG, 0b01_0000);
    until(&mut cpu, 100_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    native_init(&mut cpu);
    dma_cmd(&mut cpu, 24, 1, 1, 512, BUF, true);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(problems(&cpu), 0, "the line at BUF was cleaned");
}

#[test]
fn range_op_queue_has_depth_one() {
    // MMIO_MAP "Range operations": one range op can wait behind a running
    // walk with its own range; one that finds that slot taken, a
    // whole-cache walk waiting, or that covers more than 64 KiB becomes a
    // whole-cache walk (an INVALIDATE if either op invalidates); a
    // whole-cache request absorbs a waiting range op.
    let mut cpu = cpu_dma(true);
    native_init(&mut cpu);
    let summary = |cpu: &Cpu| cpu.sd_dma_summary().unwrap();
    let wait = |cpu: &mut Cpu| until(cpu, 1_000_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    let range = |cpu: &mut Cpu, addr: u64, len: u64, ctrl: u64| {
        cpu.mmio_write(CACHE_OP_ADDR_REG, addr);
        cpu.mmio_write(CACHE_OP_LEN_REG, len);
        cpu.mmio_write(CACHE_CTRL_REG, ctrl);
    };
    let (a, b, c) = (BUF, BUF + 0x1000, BUF + 0x2000);
    // A range op behind a running range walk waits with its own range:
    // two walks, 1 + 2 x 16 cycles each, and nothing converted.
    range(&mut cpu, a, 512, 0b01_0000);
    range(&mut cpu, b, 512, 0b01_0000);
    let busy = wait(&mut cpu);
    assert!((66..66 + 16).contains(&busy), "{busy}");
    assert!(!summary(&cpu).contains("whole-cache"));
    // Behind a running whole walk too.
    cpu.mmio_write(CACHE_CTRL_REG, 0b10);
    range(&mut cpu, a, 512, 0b01_0000);
    let busy = wait(&mut cpu);
    assert!((4130..4130 + 16).contains(&busy), "{busy}");
    assert!(!summary(&cpu).contains("whole-cache"));
    // A third op while one waits: both become one whole-cache walk.
    range(&mut cpu, a, 512, 0b01_0000);
    range(&mut cpu, b, 512, 0b01_0000);
    range(&mut cpu, c, 512, 0b01_0000);
    let busy = wait(&mut cpu);
    assert!((33 + 4097..33 + 4097 + 16).contains(&busy), "{busy}");
    assert!(summary(&cpu).contains(", 1 range ops made whole-cache walks"), "{}", summary(&cpu));
    // ... an INVALIDATE when the waiting one invalidates: a line a DMA wrote
    // outside both ranges is invalidated too (no stale load after).
    dma_cmd(&mut cpu, 17, 3, 1, 512, BUF + 0x8000, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    range(&mut cpu, a, 512, 0b01_0000);
    range(&mut cpu, b, 512, 0b10_0000);
    range(&mut cpu, c, 512, 0b01_0000);
    wait(&mut cpu);
    cpu_load(&mut cpu, BUF + 0x8000);
    assert_eq!(problems(&cpu), 0, "the converted walk invalidated");
    // 2,048 lines (64 KiB from a line boundary) is still a range op; the
    // lines counted are those from ADDR & ~31 covering [ADDR, + LEN), so
    // 64 KiB from 8 bytes into a line (LEN = 64 KiB + 8, as a driver
    // writes it for an unaligned buffer) is 2,049 and becomes a whole-cache
    // walk at once.
    let busy = walk_cycles(&mut cpu, 0, 64 * 1024, 0b01_0000);
    assert_eq!(busy, 2049);
    assert!(summary(&cpu).contains(", 2 range ops made whole-cache walks"));
    range(&mut cpu, 8, 64 * 1024 + 8, 0b01_0000);
    let busy = wait(&mut cpu);
    assert!((4097..4097 + 16).contains(&busy), "{busy}");
    assert!(summary(&cpu).contains(", 3 range ops made whole-cache walks"), "{}", summary(&cpu));
    // A range op while a whole-cache request waits merges into it.
    cpu.mmio_write(CACHE_CTRL_REG, 0b10);
    cpu.mmio_write(CACHE_CTRL_REG, 0b10);
    range(&mut cpu, a, 512, 0b01_0000);
    let busy = wait(&mut cpu);
    assert!((2 * 4097..2 * 4097 + 16).contains(&busy), "{busy}");
    assert!(summary(&cpu).contains(", 4 range ops made whole-cache walks"));
    // A whole-cache request absorbs a waiting range op (no conversion
    // counted: that op's own request was taken).  A FLUSH absorbing a
    // RANGE_INV becomes an INVALIDATE.
    dma_cmd(&mut cpu, 17, 3, 1, 512, BUF + 0x8000, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    range(&mut cpu, a, 512, 0b01_0000);
    range(&mut cpu, b, 512, 0b10_0000);
    cpu.mmio_write(CACHE_CTRL_REG, 0b10);
    let busy = wait(&mut cpu);
    assert!((33 + 4097..33 + 4097 + 16).contains(&busy), "{busy}");
    cpu_load(&mut cpu, BUF + 0x8000);
    assert_eq!(problems(&cpu), 0, "the FLUSH became an INVALIDATE");
    assert!(summary(&cpu).contains(", 4 range ops made whole-cache walks"));
    // MNT_BUSY covers the waiting ops: it reads 1 until the last is done.
    range(&mut cpu, a, 512, 0b01_0000);
    range(&mut cpu, b, 512, 0b01_0000);
    pass(&mut cpu, 40);
    assert_eq!(cpu.mmio_read(CACHE_STATUS_REG), 1);
}

#[test]
fn range_inv_writes_dirty_lines_back() {
    // RANGE_INV alone writes a dirty line back before it drops it (spec
    // v1.2; the RTL): not a problem, and the line is clean after it.
    let mut cpu = cpu_dma(true);
    native_init(&mut cpu);
    let range = |cpu: &mut Cpu, addr: u64, len: u64, ctrl: u64| {
        until(cpu, 1_000_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
        cpu.mmio_write(CACHE_OP_ADDR_REG, addr);
        cpu.mmio_write(CACHE_OP_LEN_REG, len);
        cpu.mmio_write(CACHE_CTRL_REG, ctrl);
        until(cpu, 1_000_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    };
    let (clean, inv) = (0b01_0000, 0b10_0000);
    cpu_store(&mut cpu, BUF + 64, 1);
    range(&mut cpu, BUF, 512, inv);
    assert_eq!(problems(&cpu), 0, "a dirty line under RANGE_INV is written back");
    dma_cmd(&mut cpu, 24, 1, 1, 512, BUF, true);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(problems(&cpu), 0, "and clean after it");
    // The failure it can cause: a line dirtied in a device-bound buffer
    // after its clean (the driver's map) is written back over the DMA's
    // data by the unmap's RANGE_INV.  The DMA write is reported (it could
    // be clobbered), and the writeback that does clobber it.
    range(&mut cpu, BUF, 512, clean);
    cpu_store(&mut cpu, BUF + 128, 7);
    dma_cmd(&mut cpu, 17, 3, 1, 512, BUF, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(problems(&cpu), 1, "the DMA write over a dirty line");
    range(&mut cpu, BUF, 512, inv);
    assert_eq!(problems(&cpu), 2, "its writeback over the DMA's data");
    cpu_load(&mut cpu, BUF + 128);
    assert_eq!(problems(&cpu), 2, "invalidated: the load sees DDR");
    // Once per line: a second DMA into it before the clean is not
    // reported again, and a clean line DMA'd into is never reported.
    range(&mut cpu, BUF, 512, clean);
    cpu_store(&mut cpu, BUF + 256, 7);
    dma_cmd(&mut cpu, 17, 3, 1, 512, BUF, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    dma_cmd(&mut cpu, 17, 4, 1, 512, BUF, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(problems(&cpu), 3);
    range(&mut cpu, BUF, 512, clean | inv);
    assert_eq!(problems(&cpu), 4, "RANGE_CLEAN | RANGE_INV writes it back too");
    // A whole-cache walk catches it as well: a store into a line a DMA
    // wrote (stale: reported), then FLUSH writes it back over the rest of
    // the DMA's line.
    dma_cmd(&mut cpu, 17, 3, 1, 512, BUF, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    cpu_store(&mut cpu, BUF + 32, 1);
    assert_eq!(problems(&cpu), 5, "store to a stale line");
    cpu.mmio_write(CACHE_CTRL_REG, 0b10);
    until(&mut cpu, 1_000_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    assert_eq!(problems(&cpu), 6, "the FLUSH's writeback over the DMA's data");
    // The correct sequence never is: clean, DMA, invalidate, then the CPU
    // writes the buffer and cleans it again.
    range(&mut cpu, BUF, 512, clean);
    dma_cmd(&mut cpu, 17, 3, 1, 512, BUF, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    range(&mut cpu, BUF, 512, inv);
    cpu_store(&mut cpu, BUF + 32, 1);
    range(&mut cpu, BUF, 512, inv);
    cpu.mmio_write(CACHE_CTRL_REG, 0b100);
    until(&mut cpu, 1_000_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    assert_eq!(problems(&cpu), 6);
}

#[test]
fn dma_phase_rules() {
    // Spec v1.1 items 8 and 9.
    let mut cpu = cpu_dma(false);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    // During a DMA phase SDM_DATA loads read 0 and stores are dropped with
    // FIFO_ERR; FIFO_THRESH reads 0.
    dma_cmd(&mut cpu, 18, 0, 0, 512, BUF, false);
    until(&mut cpu, 1_000_000, |cpu| rd(cpu, SDM_BLKDONE) == 1);
    assert_eq!(rd(&mut cpu, SDM_DATA), 0);
    assert_eq!(status(&mut cpu) & (ST_FIFO_THRESH | ST_FIFO_ERR), 0);
    wr(&mut cpu, SDM_DATA, 5);
    assert_eq!(status(&mut cpu) & ST_FIFO_ERR, ST_FIFO_ERR);
    // A CMD12 ends the phase: DMA_BUSY drops after the burst in flight,
    // and the blocks received are in DDR.
    let blocks = rd(&mut cpu, SDM_BLKDONE);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(status(&mut cpu) & ST_DATA_BUSY, 0);
    until(&mut cpu, 100, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + blocks * 512);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");
    // A CMD12 during a DMA write ends the fetching.
    dma_cmd(&mut cpu, 25, 100, 0, 512, BUF, true);
    tick(&mut cpu);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 8 * 512, "4 KiB ahead at most");
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    until(&mut cpu, 100, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    pass(&mut cpu, 50_000);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 8 * 512);
    // DMA_EN is sampled when SDM_CMD is written: set during a PIO phase it
    // changes nothing (the FIFO still moves the data).
    wr(&mut cpu, SDM_CFG, CFG_4BIT);
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (2 << 16) | 512);
    sdm_ok(&mut cpu, 23, 2, R1, 0);
    sdm_ok(&mut cpu, 18, 40, R1, CMD_DATA);
    wr(&mut cpu, SDM_CFG, CFG_4BIT | CFG_DMA_EN);
    assert_eq!(status(&mut cpu) & ST_DMA_BUSY, 0);
    assert_eq!(pop(&mut cpu, 128), (40..42).flat_map(pattern).collect::<Vec<_>>());
    assert_ne!(status(&mut cpu) & ST_DATA_DONE, 0);
    // Clearing SD_MODE resets the engine: a DMA in flight stops once its
    // burst completes.
    dma_cmd(&mut cpu, 18, 0, 0, 512, BUF, false);
    until(&mut cpu, 1_000_000, |cpu| rd(cpu, SDM_BLKDONE) == 1);
    let ctrl = rd(&mut cpu, SD_CTRL);
    wr(&mut cpu, SD_CTRL, ctrl & !CTRL_SD_MODE);
    assert_eq!(status(&mut cpu) & ST_DATA_BUSY, 0);
    until(&mut cpu, 100, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    pass(&mut cpu, 50_000);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 512, "nothing more moved");
}

#[test]
fn dma_abort_leaves_committed_blocks_for_pio() {
    // SD_NATIVE_PLAN.md §8.4 / §8.5: DMA_EN = 0 finishes the block in
    // progress; blocks the card side committed that the engine had not
    // started stay in the FIFO, and PIO takes them (and the card side
    // goes on into the FIFO) once DMA_BUSY has dropped.
    let mut cpu = cpu_dma(false);
    native_init(&mut cpu);
    // A stalled engine moves nothing while the card side fills the FIFO:
    // three committed blocks, DATA_BUSY (the engine owes them), no
    // DATA_DONE.  DMA_EN = 0: DATA_DONE at once (the card side is done),
    // DMA_BUSY drops, and the blocks are PIO's.
    inject(&mut cpu, &["dmastall@1"]);
    fill(&mut cpu, BUF, &[0xEE; 3 * BLOCK]);
    dma_cmd(&mut cpu, 18, 50, 3, 512, BUF, false);
    pass(&mut cpu, 100_000);
    assert_eq!(rd(&mut cpu, SDM_FIFO), 3 * 64);
    assert_eq!(status(&mut cpu) & (ST_DATA_BUSY | ST_DMA_BUSY | ST_DATA_DONE), ST_DATA_BUSY | ST_DMA_BUSY);
    assert_eq!(rd(&mut cpu, SDM_DATA), 0, "the FIFO port is the engine's");
    assert_eq!(rd(&mut cpu, SDM_FIFO), 3 * 64, "nothing popped");
    wr(&mut cpu, SDM_CFG, CFG_4BIT);
    tick(&mut cpu);
    assert_eq!(status(&mut cpu) & (ST_DATA_BUSY | ST_DMA_BUSY | ST_DATA_DONE), ST_DATA_DONE);
    assert_eq!(pop(&mut cpu, 3 * 64), (50..53).flat_map(pattern).collect::<Vec<_>>());
    assert_eq!(ram(&cpu, BUF, 3 * BLOCK), vec![0xEE; 3 * BLOCK]);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);

    // DMA_EN = 0 with a block on its way to DDR: it lands; until then
    // DMA_BUSY holds the FIFO port (loads read 0 and pop nothing, stores
    // set FIFO_ERR, FIFO_THRESH reads 0) although the card side already
    // goes on into the FIFO as PIO.
    fill(&mut cpu, BUF, &[0xEE; 4 * BLOCK]);
    dma_cmd(&mut cpu, 18, 60, 0, 512, BUF, false);
    until(&mut cpu, 10_000_000, |cpu| rd(cpu, SDM_BLKDONE) == 2);
    wr(&mut cpu, SDM_STATUS, u64::from(ST_STICKY));
    wr(&mut cpu, SDM_CFG, CFG_4BIT);
    let level = rd(&mut cpu, SDM_FIFO);
    assert_eq!(level, 512, "the card side filled the FIFO as PIO");
    assert_ne!(status(&mut cpu) & ST_DMA_BUSY, 0, "the block in flight");
    assert_eq!(status(&mut cpu) & ST_FIFO_THRESH, 0);
    assert_eq!(rd(&mut cpu, SDM_DATA), 0);
    wr(&mut cpu, SDM_DATA, 1);
    assert_eq!(rd(&mut cpu, SDM_FIFO), level, "nothing popped or pushed");
    assert_ne!(status(&mut cpu) & ST_FIFO_ERR, 0);
    let drained = until(&mut cpu, 1000, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    assert!(drained <= 64, "{drained}");
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 2 * 512, "the block in progress finished");
    assert_eq!(ram(&cpu, BUF, 2 * BLOCK), (60..62).flat_map(pattern).collect::<Vec<_>>());
    assert_ne!(status(&mut cpu) & ST_FIFO_THRESH, 0, "PIO's now");
    assert_eq!(pop(&mut cpu, 64), pattern(62));
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
}

#[test]
fn dma_data_command_while_dma_busy_is_ignored() {
    // A data command written while DMA_BUSY (DATA_BUSY clear: here after a
    // CMD12, the engine still finishing its block) is ignored by the RTL;
    // the emulator ignores it too, says so on stderr and counts it.
    let mut cpu = cpu_dma(false);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    dma_cmd(&mut cpu, 18, 0, 0, 512, BUF, false);
    until(&mut cpu, 10_000_000, |cpu| rd(cpu, SDM_BLKDONE) == 2);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(status(&mut cpu) & (ST_DATA_BUSY | ST_DMA_BUSY), ST_DMA_BUSY);
    wr(&mut cpu, SDM_STATUS, u64::from(ST_STICKY));
    wr(&mut cpu, SDM_BLK, (1 << 16) | 512);
    wr(&mut cpu, SDM_CFG, CFG_4BIT);
    wr(&mut cpu, SDM_ARG, 7);
    wr(&mut cpu, SDM_CMD, cmd_word(17, R1, CMD_DATA));
    assert_eq!(status(&mut cpu) & (ST_CMD_DONE | ST_DATA_BUSY), 0, "ignored");
    assert!(
        cpu.sd_dma_summary().unwrap().contains(", 1 data commands ignored while DMA_BUSY"),
        "{}",
        cpu.sd_dma_summary().unwrap()
    );
    // A non-data command goes out; once DMA_BUSY has dropped the data
    // command does too.
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");
    until(&mut cpu, 1000, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    sdm_ok(&mut cpu, 17, 7, R1, CMD_DATA);
    assert_eq!(pop(&mut cpu, 64), pattern(7));
}

#[test]
fn dma_read_moves_whatever_is_in_the_fifo() {
    // A DMA read moves the FIFO's head: words left in it before the
    // command (here pushed through SDM_DATA) reach DDR ahead of the card's
    // data, and the block's tail stays behind.  Drivers reset the FIFO
    // first; the emulator says so on stderr and counts it.
    let mut cpu = cpu_dma(false);
    native_init(&mut cpu);
    let junk: Vec<u64> = (0..8).map(|k| 0x1111_1111_1111_1111 * (k + 1)).collect();
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    for &w in &junk {
        wr(&mut cpu, SDM_DATA, w);
    }
    fill(&mut cpu, BUF, &[0xEE; 2 * BLOCK]);
    wr(&mut cpu, SDM_BLK, (1 << 16) | 512);
    wr(&mut cpu, SDM_DMA_ADDR, BUF);
    wr(&mut cpu, SDM_CFG, CFG_4BIT | CFG_DMA_EN);
    sdm_ok(&mut cpu, 17, 9, R1, CMD_DATA);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    let head: Vec<u8> = junk.iter().flat_map(|w| w.to_le_bytes()).collect();
    assert_eq!(ram(&cpu, BUF, BLOCK), [head, pattern(9)[..448].to_vec()].concat());
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 512);
    assert_eq!(ram(&cpu, BUF + 512, BLOCK), vec![0xEE; BLOCK]);
    assert_eq!(status(&mut cpu) & ST_DMA_BUSY, 0);
    assert_eq!(rd(&mut cpu, SDM_FIFO), 8, "the block's tail");
    assert_eq!(pop(&mut cpu, 8), pattern(9)[448..]);
    assert!(
        cpu.sd_dma_summary()
            .unwrap()
            .contains(", 1 DMA data commands started with words in the FIFO")
    );
    // With the FIFO reset first (as the kernel does): the block alone.
    dma_cmd(&mut cpu, 17, 9, 1, 512, BUF, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(ram(&cpu, BUF, BLOCK), pattern(9));
    assert!(cpu.sd_dma_summary().unwrap().contains(", 1 DMA data commands started"));
}

#[test]
fn dma_faults() {
    let mut cpu = cpu_dma(false);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    inject(&mut cpu, &["dmaerr@1", "dmaerr@2", "dmastall@3", "dmahang@4", "dmastuck@5"]);
    // dmaerr on a counted command: refused up front.
    let st = dma_cmd_raw(&mut cpu, 18, 4, 512, BUF, false);
    assert_refused(&mut cpu, st, BUF);
    // On an open-ended one: DMA_ERR at the first block, which goes to the
    // FIFO.
    fill(&mut cpu, BUF, &[0xEE; BLOCK]);
    dma_cmd(&mut cpu, 18, 0, 0, 512, BUF, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DMA_ERR != 0);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF);
    assert_eq!(ram(&cpu, BUF, BLOCK), vec![0xEE; BLOCK]);
    assert_eq!(pop(&mut cpu, BLOCK / 8), pattern(0));
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    // dmastall: nothing moves, DMA_BUSY stays up, until DMA_EN = 0 stops
    // it; the card side then goes on into the FIFO, as for any abort.
    dma_cmd(&mut cpu, 18, 0, 2, 512, BUF, false);
    pass(&mut cpu, 100_000);
    assert_eq!(status(&mut cpu) & (ST_DMA_BUSY | ST_DATA_DONE), ST_DMA_BUSY);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF);
    wr(&mut cpu, SDM_CFG, CFG_4BIT);
    until(&mut cpu, 1000, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    // dmahang: DMA_EN = 0, SDM_FIFO[31] and CMD12 do not drop DMA_BUSY;
    // clearing SD_MODE does.
    dma_cmd(&mut cpu, 25, 0, 2, 512, BUF, true);
    wr(&mut cpu, SDM_CFG, CFG_4BIT);
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    pass(&mut cpu, 100_000);
    assert_ne!(status(&mut cpu) & ST_DMA_BUSY, 0);
    let ctrl = rd(&mut cpu, SD_CTRL);
    wr(&mut cpu, SD_CTRL, ctrl & !CTRL_SD_MODE);
    until(&mut cpu, 1000, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    wr(&mut cpu, SD_CTRL, ctrl);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");
    // dmastuck: not even that.
    dma_cmd(&mut cpu, 17, 0, 1, 512, BUF, false);
    wr(&mut cpu, SDM_CFG, CFG_4BIT);
    wr(&mut cpu, SD_CTRL, ctrl & !CTRL_SD_MODE);
    pass(&mut cpu, 100_000);
    assert_ne!(status(&mut cpu) & ST_DMA_BUSY, 0);
    assert_eq!(
        cpu.sd_fault_summary().unwrap(),
        "SD faults injected: 5 (dmaerr@1: 1, dmaerr@2: 1, dmastall@3: 1, dmahang@4: 1, dmastuck@5: 1)"
    );
}

/// A DMA write landing while a whole-cache FLUSH (clean only) is pending is
/// not a hazard; a whole-cache INVALIDATE is.
#[test]
fn dma_write_during_flush_is_no_problem() {
    let mut cpu = cpu_dma(true);
    native_init(&mut cpu);
    for _ in 0..3 {
        cpu.mmio_write(CACHE_CTRL_REG, 0b10);
    }
    assert_eq!(cpu.mmio_read(CACHE_STATUS_REG), 1, "FLUSH walks pending");
    dma_cmd(&mut cpu, 17, 3, 1, 512, BUF, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(problems(&cpu), 0, "DMA write during a FLUSH");
    until(&mut cpu, 1_000_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    for _ in 0..3 {
        cpu.mmio_write(CACHE_CTRL_REG, 0b100);
    }
    dma_cmd(&mut cpu, 17, 3, 1, 512, BUF, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(problems(&cpu), 1, "DMA write during an INVALIDATE");
}

#[test]
fn dma_coherence_checker() {
    let mut cpu = cpu_dma(true);
    native_init(&mut cpu);
    let range = |cpu: &mut Cpu, addr: u64, len: u64, bits: u64| {
        until(cpu, 1_000_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
        cpu.mmio_write(CACHE_OP_ADDR_REG, addr);
        cpu.mmio_write(CACHE_OP_LEN_REG, len);
        cpu.mmio_write(CACHE_CTRL_REG, bits);
        until(cpu, 1_000_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    };
    let clean = 0b01_0000;
    let inv = 0b10_0000;
    // CPU writes a buffer, a DMA sends it to the card without a clean.
    cpu_store(&mut cpu, BUF + 40, 0x55);
    dma_cmd(&mut cpu, 24, 1, 1, 512, BUF, true);
    tick(&mut cpu);
    assert_eq!(problems(&cpu), 1, "dirty line under a DMA read");
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    // With the clean (and waiting for it): fine.
    cpu_store(&mut cpu, BUF + 8, 0x55);
    range(&mut cpu, BUF, 512, clean);
    dma_cmd(&mut cpu, 24, 1, 1, 512, BUF, true);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(problems(&cpu), 1);
    // The clean not waited for: the DMA reads during the walk, and the
    // walk's effect counts only when it completes, so the line is still
    // dirty too.
    cpu_store(&mut cpu, BUF + 8, 0x56);
    cpu.mmio_write(CACHE_OP_ADDR_REG, BUF);
    cpu.mmio_write(CACHE_OP_LEN_REG, 512);
    cpu.mmio_write(CACHE_CTRL_REG, clean);
    dma_cmd(&mut cpu, 24, 1, 1, 512, BUF, true);
    tick(&mut cpu);
    assert_eq!(problems(&cpu), 3, "DMA while MNT_BUSY, of a dirty line");
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    // A DMA read into a buffer the CPU then reads without an invalidate.
    dma_cmd(&mut cpu, 17, 3, 1, 512, BUF, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(cpu_load(&mut cpu, BUF + 64), u64::from_le_bytes(pattern(3)[64..72].try_into().unwrap()));
    assert_eq!(problems(&cpu), 4, "stale line read");
    // With the invalidate: fine.
    dma_cmd(&mut cpu, 17, 3, 1, 512, BUF, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    range(&mut cpu, BUF, 512, inv);
    cpu_load(&mut cpu, BUF + 64);
    cpu_load(&mut cpu, BUF + 504);
    assert_eq!(problems(&cpu), 4);
    // A DMA into a line the CPU dirtied (no clean before): its writeback
    // could clobber the data.
    cpu_store(&mut cpu, BUF + 256, 1);
    dma_cmd(&mut cpu, 17, 3, 1, 512, BUF, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(problems(&cpu), 5);
    // A whole-cache INVALIDATE covers everything, once it completes: a
    // load during its walk is still a problem.
    cpu.mmio_write(CACHE_CTRL_REG, 0b100);
    cpu_load(&mut cpu, BUF);
    assert_eq!(problems(&cpu), 6, "load before the walk completed");
    // Its writeback of the line dirtied above lands over the DMA's data.
    until(&mut cpu, 1_000_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    assert_eq!(problems(&cpu), 7, "the dirty line written back over the DMA's data");
    cpu_load(&mut cpu, BUF + 32);
    assert_eq!(problems(&cpu), 7);
    // A range invalidate alone writes a dirty line back (spec v1.2): no
    // data lost.
    cpu_store(&mut cpu, BUF + 1024, 1);
    range(&mut cpu, BUF + 1024, 32, inv);
    assert_eq!(problems(&cpu), 7);
    // Nor with clean + invalidate; [4:0] of the address read 0, so the
    // length counts from the line's start.
    cpu_store(&mut cpu, BUF + 1024 + 0x20, 1);
    range(&mut cpu, BUF + 1024 + 0x3F, 1, clean | inv);
    dma_cmd(&mut cpu, 24, 1, 1, 512, BUF + 1024, true);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(problems(&cpu), 7);
    // SDM_DMA_ADDR during a DMA is ignored and reads back the live
    // address (spec v1.1 item 8): not a problem.
    dma_cmd(&mut cpu, 18, 0, 4, 512, BUF, false);
    wr(&mut cpu, SDM_DMA_ADDR, BUF + 0x8000);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF);
    assert_eq!(problems(&cpu), 7);
    // A DMA writing memory while a walk runs: its writebacks could land
    // over the device's data.
    until(&mut cpu, 1_000_000, |cpu| rd(cpu, SDM_BLKDONE) == 1);
    // Three queued INVALIDATEs outlast the next block.  (A pending
    // whole-cache FLUSH is no hazard for a DMA write: see
    // dma_write_during_flush_is_no_problem.)
    for _ in 0..3 {
        cpu.mmio_write(CACHE_CTRL_REG, 0b100);
    }
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(problems(&cpu), 9, "DMA writes (the first block's landing, the second) while MNT_BUSY");
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    // Range ops issued after the DMA cover it; a whole FLUSH cleans all.
    cpu.mmio_write(CACHE_CTRL_REG, 0b100);
    until(&mut cpu, 1_000_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    cpu_store(&mut cpu, BUF, 7);
    cpu.mmio_write(CACHE_CTRL_REG, 0b10);
    until(&mut cpu, 1_000_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    dma_cmd(&mut cpu, 24, 1, 1, 512, BUF, true);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(problems(&cpu), 9);
    // A driver that invalidates (unmaps) before DMA_BUSY drops: the burst
    // in flight lands after its invalidate, and the CPU's read is caught.
    dma_cmd(&mut cpu, 17, 9, 1, 512, BUF, false);
    until(&mut cpu, 1_000_000, |cpu| rd(cpu, SDM_BLKDONE) == 1);
    assert_ne!(status(&mut cpu) & ST_DMA_BUSY, 0);
    range(&mut cpu, BUF, 512, inv);
    until(&mut cpu, 1000, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    cpu_load(&mut cpu, BUF + 480);
    assert_eq!(problems(&cpu), 10, "invalidated before DMA_BUSY dropped");
}

#[test]
fn dma_icache_coherence() {
    // Code a DMA wrote must be invalidated in the I-cache before it runs:
    // ICACHE_INV, or RANGE_IINV over it; a CPU store to a line also does
    // (the I-cache snoops CPU stores).
    let mut cpu = cpu_dma(true);
    native_init(&mut cpu);
    let iinv = 0b100_0000;
    let fetch = |cpu: &mut Cpu, addr: u64| cpu.coherence.as_deref_mut().unwrap().cpu_fetch(addr as u32, 4);
    let dma_read = |cpu: &mut Cpu, addr: u64| {
        cpu.mmio_write(CACHE_CTRL_REG, 0b10);
        until(cpu, 1_000_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
        dma_cmd(cpu, 17, 3, 1, 512, addr, false);
        until(cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
        // The D-side invalidate, so only the I-side is in question.
        cpu.mmio_write(CACHE_CTRL_REG, 0b100);
        until(cpu, 1_000_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    };
    dma_read(&mut cpu, BUF);
    fetch(&mut cpu, BUF + 64);
    assert_eq!(problems(&cpu), 1, "executing DMA-written code without an I-cache invalidate");
    fetch(&mut cpu, BUF + 68);
    assert_eq!(problems(&cpu), 1, "reported once per line");
    // ICACHE_INV: at once.
    dma_read(&mut cpu, BUF);
    cpu.mmio_write(CACHE_CTRL_REG, 0b1000);
    fetch(&mut cpu, BUF + 64);
    assert_eq!(problems(&cpu), 1);
    // RANGE_IINV of the buffer: fetch is coherent with it at once (every
    // lookup misses during its sweep), which keeps MNT_BUSY up for min(n,
    // 64) cycles.
    dma_read(&mut cpu, BUF);
    cpu.mmio_write(CACHE_OP_ADDR_REG, BUF);
    cpu.mmio_write(CACHE_OP_LEN_REG, 256);
    cpu.mmio_write(CACHE_CTRL_REG, iinv);
    assert_eq!(cpu.mmio_read(CACHE_STATUS_REG), 1);
    fetch(&mut cpu, BUF + 64);
    assert_eq!(problems(&cpu), 1, "covered at once");
    let busy = until(&mut cpu, 1000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    assert!((8..=16).contains(&busy), "{busy}");
    fetch(&mut cpu, BUF + 64);
    assert_eq!(problems(&cpu), 1, "covered");
    fetch(&mut cpu, BUF + 256);
    assert_eq!(problems(&cpu), 2, "past the range");
    // A CPU store to a line counts as its invalidate.
    dma_read(&mut cpu, BUF);
    cpu_store(&mut cpu, BUF + 128, 0);
    fetch(&mut cpu, BUF + 132);
    assert_eq!(problems(&cpu), 2);
    // ICACHE_INV with the range bits: both happen.
    dma_read(&mut cpu, BUF);
    cpu_store(&mut cpu, BUF + 0x4000, 1);
    cpu.mmio_write(CACHE_OP_ADDR_REG, BUF + 0x4000);
    cpu.mmio_write(CACHE_OP_LEN_REG, 32);
    cpu.mmio_write(CACHE_CTRL_REG, 0b1000 | 0b01_0000);
    assert_eq!(cpu.mmio_read(CACHE_STATUS_REG), 1, "the range clean walks");
    fetch(&mut cpu, BUF + 64);
    assert_eq!(problems(&cpu), 2, "the I-cache dropped at once");
    until(&mut cpu, 1000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    dma_cmd(&mut cpu, 24, 1, 1, 512, BUF + 0x4000, true);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(problems(&cpu), 2, "and the line was cleaned");
    // RANGE_IINV with a whole-cache INVALIDATE: both happen.
    dma_read(&mut cpu, BUF);
    cpu.mmio_write(CACHE_OP_ADDR_REG, BUF);
    cpu.mmio_write(CACHE_OP_LEN_REG, 512);
    cpu.mmio_write(CACHE_CTRL_REG, iinv | 0b100);
    fetch(&mut cpu, BUF + 64);
    assert_eq!(problems(&cpu), 2, "the I-cache range dropped with the INVALIDATE");
    until(&mut cpu, 100_000, |cpu| cpu.mmio_read(CACHE_STATUS_REG) == 0);
    // I-cache fills come through the D-cache: a DMA'd line with the
    // I-cache dropped but no D-cache invalidate is still a problem.
    dma_cmd(&mut cpu, 17, 3, 1, 512, BUF + 0x8000, false);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    cpu.mmio_write(CACHE_CTRL_REG, 0b1000);
    fetch(&mut cpu, BUF + 0x8000);
    assert_eq!(problems(&cpu), 3, "fetch of a line stale on the D side");
    fetch(&mut cpu, BUF + 0x8004);
    assert_eq!(problems(&cpu), 3, "once per line");
}

#[test]
fn dma_completion_wakes_a_waiting_program() {
    // A CPU parked in WAIT with no timer sleeps to the DMA's blocks (not
    // "WAIT forever"), and DATA_DONE's interrupt wakes it into the source 2
    // handler, which HALTs, with the data in DDR.
    let mut cpu = cpu_dma(false);
    native_init(&mut cpu);
    cpu.mmio_write(INT2_EN, 0b11);
    cpu.mmio_write(INT_VEC2, 0x40);
    cpu.mmio_write(INT_MASK, 0b100);
    wr(&mut cpu, SDM_IRQ_EN, u64::from(ST_DATA_DONE));
    assert!(cpu.debug_write(0x40, &0x6C01_0000_u32.to_le_bytes()));
    dma_cmd(&mut cpu, 18, 30, 4, 512, BUF, false);
    let start = cpu.cycles;
    cpu.waiting = true;
    let result = cpu.run(10, None);
    assert_eq!(result.stop, crate::emu::StopReason::Halt, "the source 2 handler ran");
    assert_eq!(result.instructions, 1);
    assert!(cpu.cycles - start > 4 * 4000, "the blocks took their time");
    assert_eq!(ram(&cpu, BUF, 4 * BLOCK), (30..34).flat_map(pattern).collect::<Vec<_>>());
    // With the interrupt not enabled the CPU sleeps through the blocks and
    // then has nothing to wake it.
    let mut cpu = cpu_dma(false);
    native_init(&mut cpu);
    dma_cmd(&mut cpu, 17, 1, 1, 512, BUF, false);
    cpu.waiting = true;
    let result = cpu.run(1000, None);
    assert!(matches!(result.stop, crate::emu::StopReason::WaitForever(_)), "{:?}", result.stop);
    assert_ne!(status(&mut cpu) & ST_DATA_DONE, 0);
}

#[test]
fn dma_write_response_error_leaves_a_block_in_the_fifo() {
    // A DMA write's engine fetches from DDR at command accept: when the
    // response then fails, what it fetched stays in the FIFO (modelled at
    // the most: the count, or the FIFO's room), the engine ends, and DDR
    // is only read.  A read's moves nothing.
    // The driver resets the FIFO before it retries; one that doesn't sends
    // the stale block to the card first (SD_NATIVE_PLAN §8.4).
    let mut cpu = cpu_dma(false);
    native_init(&mut cpu);
    let data: Vec<u8> = (0..2 * BLOCK).map(|i| (i as u8).wrapping_mul(13) ^ 0x5A).collect();
    fill(&mut cpu, BUF, &data);
    fill(&mut cpu, BUF + 0x4000, &[0xEE; BLOCK]);
    inject(&mut cpu, &["cmdto:24@1"]);
    let setup = |cpu: &mut Cpu, addr: u64| {
        wr(cpu, SDM_FIFO, FIFO_RESET);
        wr(cpu, SDM_BLK, (1 << 16) | 512);
        wr(cpu, SDM_DMA_ADDR, addr);
        wr(cpu, SDM_CFG, CFG_4BIT | CFG_DMA_EN);
    };
    // A write: CMD_TIMEOUT, then DMA_BUSY drops with its block in the FIFO.
    setup(&mut cpu, BUF);
    let st = sdm(&mut cpu, 24, 50, R1, CMD_DATA | CMD_WRITE);
    assert_ne!(st & ST_CMD_TIMEOUT, 0);
    assert_eq!(st & ST_DATA_BUSY, 0, "no data phase");
    until(&mut cpu, 100_000, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    assert_eq!(rd(&mut cpu, SDM_FIFO), 64, "the block");
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 512);
    assert_eq!(status(&mut cpu) & (ST_DATA_DONE | ST_DATA_BUSY | ST_DMA_ERR), 0);
    // Three blocks (CMD25): all three; open-ended: the FIFO's eight.
    for (blocks, words) in [(3, 192), (0, 512)] {
        inject(&mut cpu, &["cmdto:25@1"]);
        wr(&mut cpu, SDM_FIFO, FIFO_RESET);
        wr(&mut cpu, SDM_BLK, (blocks << 16) | 512);
        wr(&mut cpu, SDM_DMA_ADDR, BUF);
        wr(&mut cpu, SDM_CFG, CFG_4BIT | CFG_DMA_EN);
        assert_ne!(sdm(&mut cpu, 25, 50, R1, CMD_DATA | CMD_WRITE) & ST_CMD_TIMEOUT, 0);
        until(&mut cpu, 100_000, |cpu| status(cpu) & ST_DMA_BUSY == 0);
        assert_eq!(rd(&mut cpu, SDM_FIFO), words);
        assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 8 * words);
    }
    inject(&mut cpu, &["cmdto:17@1", "cmdto:24@1"]);
    // A read: nothing moves.
    setup(&mut cpu, BUF + 0x2_0000);
    let st = sdm(&mut cpu, 17, 50, R1, CMD_DATA);
    assert_ne!(st & ST_CMD_TIMEOUT, 0);
    tick(&mut cpu);
    assert_eq!(status(&mut cpu) & ST_DMA_BUSY, 0);
    assert_eq!(rd(&mut cpu, SDM_FIFO), 0);
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR), BUF + 0x2_0000);
    // Retried after a FIFO reset: the card gets the buffer.
    setup(&mut cpu, BUF);
    let st = sdm(&mut cpu, 24, 50, R1, CMD_DATA | CMD_WRITE);
    assert_ne!(st & ST_CMD_TIMEOUT, 0, "the second injected timeout");
    until(&mut cpu, 100_000, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    dma_cmd(&mut cpu, 24, 50, 1, 512, BUF, true);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(card_bytes(&cpu)[50 * BLOCK..51 * BLOCK], data[..BLOCK]);
    // Retried without one: the stale block goes to the card first.
    let st = {
        wr(&mut cpu, SDM_FIFO, FIFO_RESET);
        wr(&mut cpu, SDM_BLK, (1 << 16) | 512);
        wr(&mut cpu, SDM_DMA_ADDR, BUF + 0x4000);
        wr(&mut cpu, SDM_CFG, CFG_4BIT | CFG_DMA_EN);
        inject(&mut cpu, &["cmdto:24@1"]);
        sdm(&mut cpu, 24, 51, R1, CMD_DATA | CMD_WRITE)
    };
    assert_ne!(st & ST_CMD_TIMEOUT, 0);
    until(&mut cpu, 100_000, |cpu| status(cpu) & ST_DMA_BUSY == 0);
    wr(&mut cpu, SDM_BLK, (1 << 16) | 512);
    wr(&mut cpu, SDM_DMA_ADDR, BUF + 512);
    sdm_ok(&mut cpu, 24, 51, R1, CMD_DATA | CMD_WRITE);
    until(&mut cpu, 1_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
    assert_eq!(
        card_bytes(&cpu)[51 * BLOCK..52 * BLOCK],
        [0xEE; BLOCK][..],
        "the failed attempt's block, not the buffer"
    );
    let summary = cpu.sd_dma_summary().unwrap();
    assert!(summary.contains("1 DMA data commands started with words in the FIFO"), "{summary}");
    assert!(summary.contains("5 DMA writes fetched blocks before a failed response"), "{summary}");
    assert_eq!(problems(&cpu), 0);
}

// ---- high speed (SD_NATIVE_PLAN §9) ----

/// A machine with the card and the native controller: `dma` its DMA
/// engine, `no_hs` a bitstream before high speed, `window` the data eye.
fn cpu_hs(dma: bool, no_hs: bool, window: Option<&str>) -> Cpu {
    let mut cpu = Cpu::with_mem_size(&[0; 0x100], CODE_BASE, DMA_MEM);
    cpu.install(Hardware {
        sd_card: Some(SdCard::from_bytes(image()).unwrap()),
        sd_native: true,
        sd_dma: dma,
        sd_no_hs: no_hs,
        sd_hs_window: window.map(|w| w.parse().unwrap()),
        ..Hardware::default()
    });
    cpu
}

/// `SDM_CFG` for the 4-bit bus sampling at `phase` and `tap`.
const fn cfg_hs(phase: u64, tap: u64) -> u64 {
    CFG_4BIT | CFG_HS_SAMPLE | (phase << 4) | (tap << 8)
}

/// CMD6 with `arg` and its 64-byte status, on the 4-bit bus.
fn cmd6(cpu: &mut Cpu, arg: u32) -> Vec<u8> {
    wr(cpu, SDM_FIFO, FIFO_RESET);
    wr(cpu, SDM_BLK, (1 << 16) | 64);
    sdm_ok(cpu, 6, arg, R1, CMD_DATA);
    assert_ne!(status(cpu) & ST_DATA_DONE, 0);
    pop(cpu, 8)
}

/// After `native_init`: CMD6 to high speed, then 50 MHz with the HS
/// sample point at `phase` / `tap`.
fn switch_hs(cpu: &mut Cpu, phase: u64, tap: u64) {
    assert_eq!(cmd6(cpu, 0x80FF_FFF1)[16] & 0xF, 1, "switched");
    wr(cpu, SDM_CFG, cfg_hs(phase, tap));
    wr(cpu, SD_CTRL, CTRL_SD_MODE | CTRL_PWR_ON);
}

#[test]
fn hs_registers_and_the_bitstreams_before_it() {
    // sdhs: SDM_CAPS 0x0708_0B01 (| [10] with DMA), the sampling bits
    // [1], [7:4], [12:8] kept as written (phases 7-15 too), IDELAY_RDY.
    let mut cpu = cpu_hs(false, false, None);
    assert_eq!(rd(&mut cpu, SDM_CAPS), 0x0708_0B01);
    assert_eq!(rd(&mut cpu, SDM_CAPS) >> 24 & 0xF, 7, "7 sample phases");
    assert_eq!(rd(&mut cpu, SDM_CFG), 0, "reset 0");
    assert_eq!(status(&mut cpu) & ST_IDELAY_RDY, ST_IDELAY_RDY);
    for cfg in [cfg_hs(2, 8), cfg_hs(6, 31), cfg_hs(15, 0), 0x823, CFG_HS_SAMPLE] {
        wr(&mut cpu, SDM_CFG, cfg);
        assert_eq!(rd(&mut cpu, SDM_CFG), cfg);
    }
    wr(&mut cpu, SDM_CFG, !0);
    assert_eq!(rd(&mut cpu, SDM_CFG), 0x1FF7, "[16] DMA_EN is not there without --sd-dma");
    // IDELAY_RDY is live: W1C does nothing to it, and it is no interrupt.
    wr(&mut cpu, SDM_STATUS, u64::from(ST_IDELAY_RDY));
    assert_eq!(status(&mut cpu) & ST_IDELAY_RDY, ST_IDELAY_RDY);
    wr(&mut cpu, SDM_IRQ_EN, !0);
    assert_eq!(rd(&mut cpu, SDM_IRQ_EN) as u32 & ST_IDELAY_RDY, 0);
    assert_eq!(rd(&mut cpu, SDM_CAPS) & CAPS_DMA, 0);
    assert_eq!(rd(&mut cpu_hs(true, false, None), SDM_CAPS), 0x0708_0F01, "the sdhs bitstream with DMA");

    // --sd-no-hs: the bitstreams before sdhs, as the model was.
    let mut cpu = cpu_hs(false, true, None);
    assert_eq!(rd(&mut cpu, SDM_CAPS), 0x0008_0101);
    assert_eq!(rd(&mut cpu_hs(true, true, None), SDM_CAPS), 0x0008_0501);
    wr(&mut cpu, SDM_CFG, !0);
    assert_eq!(rd(&mut cpu, SDM_CFG), 0b101, "only 4-bit and CLK_ON");
    assert_eq!(status(&mut cpu) & ST_IDELAY_RDY, 0);
    // The card still offers high speed: cards do.
    wr(&mut cpu, SDM_CFG, 0);
    native_init(&mut cpu);
    assert_eq!(cmd6(&mut cpu, 0x00FF_FFF1)[13] & 2, 2);
}

#[test]
fn hs_cmd6_switch_and_back() {
    let mut cpu = cpu_hs(false, false, None);
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    // Mode 0 (check) with no change: group 1 is at default speed.
    let st = cmd6(&mut cpu, 0x00FF_FFFF);
    assert_eq!((st[0], st[1]), (0, 100), "max current");
    assert_eq!(st[12..14], [0x80, 0x03]);
    assert_eq!(st[14..17], [0, 0, 0], "every group at function 0");
    assert_eq!(st[17], 1, "version 1");
    assert!(st[18..].iter().all(|&b| b == 0), "no busy status, reserved 0");
    // Unsupported functions report 0xF; in mode 1 they cancel the switch.
    assert_eq!(cmd6(&mut cpu, 0x00FF_FFF2)[16] & 0xF, 0xF, "group 1 function 2");
    let st = cmd6(&mut cpu, 0x80FF_FF11);
    assert_eq!(st[16], 0xF1, "group 2 function 1: not supported; group 1 would be 1");
    assert!(!card(&cpu).high_speed(), "cancelled: nothing switched");
    // Mode 1, group 1 function 1 (Linux's 0x80FF_FFF1): high speed, and
    // the card stays in tran.
    assert_eq!(cmd6(&mut cpu, 0x80FF_FFF1)[16], 0x01);
    assert!(card(&cpu).high_speed());
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4, "tran");
    // 0xF (no change) reports the current function.
    assert_eq!(cmd6(&mut cpu, 0x00FF_FFFF)[16] & 0xF, 1);
    assert_eq!(cmd6(&mut cpu, 0x80FF_FFFF)[16] & 0xF, 1);
    assert!(card(&cpu).high_speed());
    // Function 0 in mode 1 goes back to default speed.
    assert_eq!(cmd6(&mut cpu, 0x80FF_FFF0)[16] & 0xF, 0);
    assert!(!card(&cpu).high_speed());
    // CMD0 reverts.
    cmd6(&mut cpu, 0x80FF_FFF1);
    assert!(card(&cpu).high_speed());
    sdm(&mut cpu, 0, 0, R_NONE, 0);
    assert!(!card(&cpu).high_speed(), "CMD0");
    // So does a power cycle.
    native_init(&mut cpu);
    cmd6(&mut cpu, 0x80FF_FFF1);
    wr(&mut cpu, SD_CTRL, CLK_IDENT | CTRL_SD_MODE);
    assert!(!card(&cpu).high_speed(), "power off");
}

#[test]
fn hs_block_time_at_divider_0() {
    // A DMA read of ten blocks on the 4-bit bus: about 10 x 1050 clocks,
    // 4 cycles each at 25 MHz (divider 1) and 2 at 50 MHz (divider 0).
    let mut cpu = cpu_hs(true, false, None);
    native_init(&mut cpu);
    let time = |cpu: &mut Cpu, cfg: u64| {
        wr(cpu, SDM_FIFO, FIFO_RESET);
        wr(cpu, SDM_BLK, (10 << 16) | 512);
        wr(cpu, SDM_DMA_ADDR, BUF);
        wr(cpu, SDM_CFG, cfg | CFG_DMA_EN);
        sdm_ok(cpu, 18, 0, R1, CMD_DATA);
        let cycles = until(cpu, 10_000_000, |cpu| status(cpu) & ST_DATA_DONE != 0);
        assert_eq!(ram(cpu, BUF, 10 * BLOCK), (0..10).flat_map(pattern).collect::<Vec<_>>());
        sdm_ok(cpu, 12, 0, R1B, CMD_STOP);
        until(cpu, 100_000, |cpu| status(cpu) & ST_DMA_BUSY == 0);
        wr(cpu, SDM_CFG, cfg);
        cycles
    };
    let slow = time(&mut cpu, CFG_4BIT);
    assert!((40_000..50_000).contains(&slow), "{slow} cycles at 25 MHz");
    switch_hs(&mut cpu, 2, 8);
    let fast = time(&mut cpu, cfg_hs(2, 8));
    assert!((20_000..25_000).contains(&fast), "{fast} cycles at 50 MHz");
    assert!(fast * 2 <= slow + 200 && fast * 2 + 200 >= slow, "half: {fast} vs {slow}");
    // PIO at 50 MHz reads the same data (no window: every phase passes).
    for phase in 0..16 {
        wr(&mut cpu, SDM_CFG, cfg_hs(phase, 31));
        wr(&mut cpu, SDM_FIFO, FIFO_RESET);
        wr(&mut cpu, SDM_BLK, (1 << 16) | 512);
        sdm_ok(&mut cpu, 17, 3, R1, CMD_DATA);
        assert_eq!(pop(&mut cpu, 64), pattern(3), "phase {phase}");
    }
    assert_eq!(cpu.sd.as_ref().unwrap().hs_notes, [0, 0, 0], "used as the plan says");
}

#[test]
fn hs_window_parse_and_instants() {
    let w: HsWindow = "13..27.5".parse().unwrap();
    assert_eq!(w.to_string(), "13..27.5 ns");
    assert!("27..13".parse::<HsWindow>().is_err());
    assert!("13".parse::<HsWindow>().is_err());
    assert!("a..b".parse::<HsWindow>().is_err());
    // Phase p, tap t: 10 + 5p - 0.078t ns; phases 7-15 act as 6.
    assert_eq!(HsWindow::instant(cfg_hs(0, 0)), 10_000);
    assert_eq!(HsWindow::instant(cfg_hs(2, 8)), 19_376);
    assert_eq!(HsWindow::instant(cfg_hs(6, 0)), 40_000);
    assert_eq!(HsWindow::instant(cfg_hs(15, 31)), 40_000 - 78 * 31);
    // Modulo the 20 ns clock: phase p and p + 4 agree.
    for phase in 0..3 {
        for tap in [0, 8, 16, 24, 31] {
            let a = w.passes(HsWindow::instant(cfg_hs(phase, tap)));
            assert_eq!(a, w.passes(HsWindow::instant(cfg_hs(phase + 4, tap))), "phase {phase} tap {tap}");
        }
    }
    assert!(!w.passes(10_000) && w.passes(15_000) && w.passes(20_000) && w.passes(25_000) && !w.passes(30_000));
    assert!(w.passes(35_000 + 20_000 * 3) && !w.passes(-10_000));
    // The board's sweep (§9.7): everything but phases 0 and 4 at taps 0-16.
    let board: HsWindow = "10.1..28.6".parse().unwrap();
    for phase in 0..7 {
        for tap in [0, 8, 16, 24] {
            let pass = board.passes(HsWindow::instant(cfg_hs(phase, tap)));
            assert_eq!(pass, !(phase % 4 == 0 && tap <= 16), "phase {phase} tap {tap}");
        }
    }
    let all: HsWindow = "0..20".parse().unwrap();
    assert!((0..7).all(|p| all.passes(HsWindow::instant(cfg_hs(p, 0)))));
}

#[test]
fn hs_window_gives_crc_errors_outside_it() {
    let mut cpu = cpu_hs(false, false, Some("17..23"));
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    // At 25 MHz the window does not apply, HS_SAMPLE or not.
    wr(&mut cpu, SDM_CFG, cfg_hs(0, 0));
    sdm_ok(&mut cpu, 13, rca, R1, 0);
    switch_hs(&mut cpu, 0, 0);
    // Phase 0 (10 ns) is outside: CMD_CRC_ERR, the response held as
    // received; the card executed the command.
    let st = sdm(&mut cpu, 13, rca, R1, 0);
    assert_eq!(st & ST_ERRORS, ST_CMD_CRC_ERR);
    assert_eq!(state_of(rd(&mut cpu, SDM_RESP0)), 4);
    // Phase 2, tap 8 (19.4 ns) and phase 2 at tap 31 (17.6 ns) are inside.
    for (phase, tap) in [(2, 8), (2, 31), (6, 8)] {
        wr(&mut cpu, SDM_CFG, cfg_hs(phase, tap));
        sdm_ok(&mut cpu, 13, rca, R1, 0);
    }
    // A data command whose response fails: no data phase, and the card,
    // which sent its single block anyway, is back in tran.
    wr(&mut cpu, SDM_CFG, cfg_hs(3, 0));
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (1 << 16) | 64);
    let st = sdm(&mut cpu, 6, 0x00FF_FFFF, R1, CMD_DATA);
    assert_eq!(st & (ST_ERRORS | ST_DATA_BUSY | ST_DATA_DONE), ST_CMD_CRC_ERR);
    wr(&mut cpu, SDM_CFG, cfg_hs(2, 8));
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4);
    // Read blocks: a CMD18 inside the window fills the FIFO; moved outside
    // it (which a driver must not do while DATA_BUSY: noted on stderr),
    // the next block arrives bad and ends the phase; CMD12 recovers.
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (10 << 16) | 512);
    sdm_ok(&mut cpu, 18, 40, R1, CMD_DATA);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 8);
    wr(&mut cpu, SDM_CFG, cfg_hs(4, 0));
    assert_eq!(pop(&mut cpu, 64), pattern(40));
    let st = status(&mut cpu);
    assert_eq!(st & (ST_ERRORS | ST_DATA_BUSY), ST_DATA_CRC_ERR);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 8);
    assert_eq!(cpu.sd.as_ref().unwrap().hs_notes, [0, 0, 1]);
    wr(&mut cpu, SDM_CFG, cfg_hs(2, 8));
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4);
    // At 50 MHz without HS_SAMPLE the window does not apply either (the
    // model notes it).
    wr(&mut cpu, SDM_CFG, CFG_4BIT);
    sdm_ok(&mut cpu, 13, rca, R1, 0);
    assert_eq!(cpu.sd.as_ref().unwrap().hs_notes, [0, 1, 1]);
}

#[test]
fn hs_notes_50_mhz_at_default_speed() {
    // 50 MHz with the card never switched: noted (the model still works).
    let mut cpu = cpu_hs(false, false, None);
    native_init(&mut cpu);
    wr(&mut cpu, SDM_CFG, cfg_hs(2, 8));
    wr(&mut cpu, SD_CTRL, CTRL_SD_MODE | CTRL_PWR_ON);
    sdm_ok(&mut cpu, 13, u32::from(RCA) << 16, R1, 0);
    assert_eq!(cpu.sd.as_ref().unwrap().hs_notes, [1, 0, 0]);
}

#[test]
fn hs_window_bit_slots() {
    // The eye of the slot's bit is 21..34 ns: 30 ns (phase 4) takes it,
    // 10 ns (phase 0) the bit before (k = -1), 40 ns (phase 6) none.
    let w: HsWindow = "21..34".parse().unwrap();
    assert_eq!(w.slot(HsWindow::instant(cfg_hs(4, 0))), Some(0));
    assert_eq!(w.slot(HsWindow::instant(cfg_hs(0, 0))), Some(-1));
    assert_eq!(w.slot(HsWindow::instant(cfg_hs(0, 24))), Some(-1));
    assert_eq!(w.slot(HsWindow::instant(cfg_hs(3, 31))), Some(0), "25 - 2.4 = 22.6 ns");
    assert_eq!(w.slot(HsWindow::instant(cfg_hs(6, 0))), None);
    assert_eq!(w.slot(HsWindow::instant(cfg_hs(2, 0))), None);
    // An eye before 20 ns: 40 ns (phase 6) takes the next bit (k = 1).
    let w: HsWindow = "10.1..28.6".parse().unwrap();
    assert_eq!(w.slot(HsWindow::instant(cfg_hs(6, 8))), Some(1));
    assert_eq!(w.slot(HsWindow::instant(cfg_hs(2, 8))), Some(0));
    assert_eq!(w.slot(HsWindow::instant(cfg_hs(0, 24))), Some(-1), "8.1 ns: a clock before 28.1");
    assert_eq!("0..25".parse::<HsWindow>().unwrap().slot(5_000), Some(0), "wide: no offset");
}

#[test]
fn hs_early_sample_loses_a_block_after_a_park() {
    let mut cpu = cpu_hs(false, false, Some("21..34"));
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    switch_hs(&mut cpu, 0, 0);
    // Phase 0 (10 ns) takes the bit before: commands, single blocks and a
    // read that never parks pass.
    sdm_ok(&mut cpu, 13, rca, R1, 0);
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (1 << 16) | 512);
    sdm_ok(&mut cpu, 17, 5, R1, CMD_DATA);
    assert_eq!(pop(&mut cpu, 64), pattern(5));
    wr(&mut cpu, SDM_BLK, (8 << 16) | 512);
    sdm_ok(&mut cpu, 18, 0, R1, CMD_DATA);
    assert_eq!(status(&mut cpu) & (ST_ERRORS | ST_DATA_DONE), ST_DATA_DONE);
    assert_eq!(pop(&mut cpu, 8 * 64), (0..8).flat_map(pattern).collect::<Vec<_>>());
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    // Ten blocks: the FIFO fills at 8, CLK parks, and the block after the
    // park loses its start bit.
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (10 << 16) | 512);
    sdm_ok(&mut cpu, 18, 20, R1, CMD_DATA);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 8);
    assert_eq!(pop(&mut cpu, 64), pattern(20));
    assert_eq!(status(&mut cpu) & (ST_ERRORS | ST_DATA_BUSY), ST_DATA_CRC_ERR | ST_DATA_END_ERR);
    assert_eq!(rd(&mut cpu, SDM_BLKDONE), 8, "short");
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);
    // Phase 4 samples the same point a bit later: the park is harmless.
    wr(&mut cpu, SDM_CFG, cfg_hs(4, 0));
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (10 << 16) | 512);
    sdm_ok(&mut cpu, 18, 20, R1, CMD_DATA);
    let mut got = pop(&mut cpu, 64);
    got.extend(pop(&mut cpu, 9 * 64));
    assert_eq!(status(&mut cpu) & (ST_ERRORS | ST_DATA_DONE), ST_DATA_DONE);
    assert_eq!(got, (20..30).flat_map(pattern).collect::<Vec<_>>());
    sdm_ok(&mut cpu, 12, 0, R1B, CMD_STOP);

    // DMA: the engine moves each block to DDR before the next one ends,
    // so the FIFO never fills, CLK never parks, and phase 0 reads well.
    let mut cpu = cpu_hs(true, false, Some("21..34"));
    native_init(&mut cpu);
    switch_hs(&mut cpu, 0, 0);
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (12 << 16) | 512);
    wr(&mut cpu, SDM_DMA_ADDR, BUF);
    wr(&mut cpu, SDM_CFG, cfg_hs(0, 0) | CFG_DMA_EN);
    sdm_ok(&mut cpu, 18, 0, R1, CMD_DATA);
    until(&mut cpu, 10_000_000, |cpu| status(cpu) & (ST_DATA_DONE | ST_ERRORS) != 0);
    assert_eq!(status(&mut cpu) & ST_ERRORS, 0, "the engine keeps the FIFO from filling");
    assert_eq!(ram(&cpu, BUF, 12 * BLOCK), (0..12).flat_map(pattern).collect::<Vec<_>>());
}

#[test]
fn hs_window_from_the_nth_switch() {
    // 0.1..0.2@2: no phase or tap samples in the window, but it applies
    // only from the card's second switch to high speed.
    let mut cpu = cpu_hs(false, false, Some("0.1..0.2@2"));
    native_init(&mut cpu);
    let rca = u32::from(RCA) << 16;
    switch_hs(&mut cpu, 2, 8);
    for phase in 0..7 {
        wr(&mut cpu, SDM_CFG, cfg_hs(phase, 0));
        sdm_ok(&mut cpu, 13, rca, R1, 0);
    }
    assert_eq!(card(&cpu).hs_switches(), 1);
    // A power cycle and a second switch: now nothing works at 50 MHz...
    native_init(&mut cpu);
    switch_hs(&mut cpu, 2, 8);
    assert_eq!(card(&cpu).hs_switches(), 2);
    for phase in 0..7 {
        for tap in [0, 8, 16, 24, 31] {
            wr(&mut cpu, SDM_CFG, cfg_hs(phase, tap));
            assert_eq!(sdm(&mut cpu, 13, rca, R1, 0) & ST_ERRORS, ST_CMD_CRC_ERR, "phase {phase} tap {tap}");
        }
    }
    // ...but CMD6 back to default speed still reaches the card (its
    // response is unreadable), and 25 MHz works again.
    wr(&mut cpu, SDM_FIFO, FIFO_RESET);
    wr(&mut cpu, SDM_BLK, (1 << 16) | 64);
    assert_eq!(sdm(&mut cpu, 6, 0x80FF_FFF0, R1, CMD_DATA) & ST_ERRORS, ST_CMD_CRC_ERR);
    assert!(!card(&cpu).high_speed());
    wr(&mut cpu, SDM_CFG, CFG_4BIT);
    wr(&mut cpu, SD_CTRL, 1 | CTRL_SD_MODE | CTRL_PWR_ON);
    assert_eq!(state_of(sdm_ok(&mut cpu, 13, rca, R1, 0)), 4);
    assert_eq!(cpu.sd.as_ref().unwrap().hs_notes, [0, 0, 0]);
}
