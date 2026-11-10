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
        sd_card: card.then(|| SdCard::from_bytes(image()).unwrap()),
        sd_native: native,
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
    assert_eq!(cpu.mmio_read(CPU_FEATURES), 0x100D);
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
    assert_eq!(rd(&mut cpu, SD_STATUS), STATUS_CARD);
    wr(&mut cpu, SD_CTRL, 0xFFFF_FFFF);
    assert_eq!(rd(&mut cpu, SD_CTRL), 0x3_FFFF, "no SD_MODE bit");
    // The native registers and the cascade are absent.
    for offset in [SDM_ARG, SDM_STATUS, SDM_TIMEOUT, SDM_CAPS, SDM_FIFO] {
        wr(&mut cpu, offset, 0x1234);
        assert_eq!(rd(&mut cpu, offset), 0, "offset 0x{offset:03X}");
    }
    assert_eq!(cpu.mmio_read(CPU_FEATURES), 0x100D);
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
    assert_eq!(rd(&mut empty, SD_STATUS), 0);
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

/// Clear the sticky bits, write `SDM_ARG` and `SDM_CMD`, return `SDM_STATUS`.
fn sdm(cpu: &mut Cpu, index: u64, arg: u32, rtype: u64, flags: u64) -> u32 {
    wr(cpu, SDM_STATUS, u64::from(ST_STICKY));
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
    assert_eq!(cpu.mmio_read(CPU_FEATURES), 0x100D | (1 << 21));
    assert_eq!(rd(cpu, SDM_CAPS), 0x0008_0101);
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
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32, ST_DAT0 | ST_CARD_PRESENT);
    assert_eq!(rd(&mut cpu, SDM_CMD), 0, "write-only");
    assert_eq!(rd(&mut cpu, SDM_DMA_ADDR_TEST), 0, "reserved");
    wr(&mut cpu, SDM_CFG, 0xFFFF_FFFF);
    assert_eq!(rd(&mut cpu, SDM_CFG), 0b101, "only 4-bit and CLK_ON");
    wr(&mut cpu, SDM_BLK, 0xFFFF_FFFF);
    assert_eq!(rd(&mut cpu, SDM_BLK), 0xFFFF_0FF8, "size [2:0] reads 0");
    wr(&mut cpu, SDM_CFG, 0);
    // A command with SD_MODE clear is ignored.
    wr(&mut cpu, SDM_CMD, 8 | (R1 << 8));
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
    let status = sdm(&mut cpu, 9, rca, R1, 0);
    assert_ne!(status & ST_CMD_IDX_ERR, 0);
    assert_ne!(status & ST_ERR_ANY, 0);
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
    // CMD6 check mode for high speed: group 1 supports only function 0, and
    // the selection says function 1 cannot be switched to.
    wr(&mut cpu, SDM_BLK, (1 << 16) | 64);
    sdm_ok(&mut cpu, 6, 0x00FF_FFF1, R1, CMD_DATA);
    let switch = pop(&mut cpu, 8);
    assert_eq!(switch[13] & 0x02, 0, "no high speed in group 1's support");
    assert_eq!(switch[16] & 0xF, 0xF, "group 1: cannot switch");
    assert_eq!(switch[17], 1, "data structure version");
    assert_eq!(rd(&mut cpu, SDM_FIFO), 0);
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
    assert_eq!(rd(&mut cpu, SDM_STATUS) as u32 & ST_FIFO_THRESH, 0, "never while idle");
    // Pre-fill one block, then CMD25 for three: it goes out at once.
    push(&mut cpu, &data[0]);
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
    // FIFO_THRESH on a counted write drops once every block is pushed.
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
