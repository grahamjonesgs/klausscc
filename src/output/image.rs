//! The board's program image, and the formats it is loaded in.
//!
//! Every loader sees the same memory: a heap header of [`HEAP_HEADER_WORDS`]
//! 64-bit words at address 0, then the program at [`CODE_BASE`].  Word 0 lo32
//! holds `heap_start`, the first free byte after the program rounded up to 8;
//! the rest of the header is zero.  [`Image`] holds the program and its entry
//! point once, and writes it as the serial `.kbt` wire format
//! ([`Image::to_kbt`]) or as raw DDR bytes ([`Image::to_ddr`]) for the network
//! boot, the boot ROM ([`readmemh`]) and the emulator.

use crate::asm::line::Pass2;
use crate::diag::Diagnostics;
use crate::layout::{CODE_BASE, HEAP_HEADER_WORDS, heap_start};
use std::fmt::Write as _;

/// A program ready to load: its bytes as they sit in memory from
/// [`CODE_BASE`], and the address execution starts at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Image {
    /// The program, in memory byte order, starting at [`CODE_BASE`].
    pub(crate) bytes: Vec<u8>,
    /// The entry point, as a board byte address.
    pub(crate) entry: u32,
}

impl Image {
    /// An image of `bytes` (loaded at [`CODE_BASE`]) that starts at `entry`.
    #[must_use]
    pub(crate) const fn new(bytes: Vec<u8>, entry: u32) -> Self {
        Self { bytes, entry }
    }

    /// Pad the program with zero bytes to a whole number of 32-bit words.
    pub(crate) fn pad_to_word(&mut self) {
        while !self.bytes.len().is_multiple_of(4) {
            self.bytes.push(0);
        }
    }

    /// The first free byte after the program, rounded up to 8: word 0 of the
    /// heap header.
    #[must_use]
    pub(crate) const fn heap_start(&self) -> u32 {
        heap_start(CODE_BASE + self.bytes.len() as u32)
    }

    /// Lay out assembled lines in memory, to start at `entry`.
    ///
    /// Each line's machine code is placed at its `program_counter` (relative
    /// to [`CODE_BASE`]), in memory byte order.  This one layout is what the
    /// `.kbt`, the DDR image and the emulator all load.
    #[must_use]
    pub(crate) fn from_lines(lines: &[Pass2], entry: u32) -> Self {
        let mut code: Vec<u8> = Vec::new();
        for line in lines {
            let len = line.emitted.byte_len();
            if len == 0 {
                continue;
            }
            let offset = line.program_counter.saturating_sub(CODE_BASE) as usize;
            if offset + len > code.len() {
                code.resize(offset + len, 0);
            }
            line.emitted.copy_to(&mut code[offset..offset + len]);
        }
        Self::new(code, entry)
    }

    /// The `.kbt` serial wire format: `S`, the heap header, the program, the
    /// entry point, `Z`, the checksum and `X`.
    ///
    /// Every 32-bit word is sent as 8 hex digits in memory order, least
    /// significant byte first, as the board's serial loader stores bytes in
    /// the order they arrive.  Returns `None` (reported) when the program is
    /// not a whole number of 32-bit words, which the board cannot load.
    #[must_use]
    pub(crate) fn to_kbt(&self, msg_list: &mut Diagnostics) -> Option<String> {
        let (words, rest) = self.bytes.as_chunks::<4>();
        if !rest.is_empty() {
            // The length of the hex stream between `S` and `Z`.
            let hex_len = (CODE_BASE as usize + self.bytes.len() + 4) * 2;
            msg_list.error(format!(
                "Program image is not a whole number of 32-bit words, length is {hex_len} hex characters"
            ));
            return None;
        }

        // Word 0: heap_start in lo32, hi32 zero.  Words 1-3 are reserved.
        let header = [self.heap_start()]
            .into_iter()
            .chain(std::iter::repeat_n(0, HEAP_HEADER_WORDS as usize * 2 - 1));
        let stream: Vec<u32> = header
            .chain(words.iter().map(|&word| u32::from_le_bytes(word)))
            .chain([self.entry])
            .collect();

        let mut out = String::with_capacity((stream.len() + 1) * 8 + 3);
        out.push('S');
        for &word in &stream {
            push_word_kbt(&mut out, word);
        }
        // The checksum covers every word before the Z delimiter.
        out.push('Z');
        push_word_kbt(&mut out, kbt_checksum(&stream));
        out.push('X');
        Some(out)
    }

    /// The raw DDR image: the heap header followed by the program, padded to
    /// `heap_start`.  See [`build_ddr_image`].
    #[must_use]
    pub(crate) fn to_ddr(&self) -> Vec<u8> {
        build_ddr_image(&self.bytes)
    }
}

/// Append a 32-bit word to a kbt wire string, as 8 hex digits in
/// little-endian byte order.
///
/// The board's serial loader stores bytes in the order they arrive, so a 32-bit
/// value must be transmitted LSByte-first so the CPU reads the correct value.
/// e.g. instruction 0x400F0000 → "00000F40", entry 0x20 → "20000000".
fn push_word_kbt(out: &mut String, word: u32) {
    let _ = write!(out, "{:08X}", word.swap_bytes());
}

/// The kbt checksum of the 32-bit words between `S` and `Z`.
///
/// For each word V, `V[31:16] + V[15:0]` is accumulated, and the checksum is
///
///   `chk = (running_sum + count) % 65536`.
///
/// where `count` is the number of 16-bit half-word groups accumulated (= 2 × number
/// of 32-bit words).  This matches the FPGA formula `chk = running_sum + last_addr×2`.
/// It is sent like every other word in the stream.
#[must_use]
fn kbt_checksum(words: &[u32]) -> u32 {
    let sum = words.iter().fold(0, |sum, &v| (sum + (v >> 16) + (v & 0xFFFF)) % 0x1_0000);
    // chk = running_sum + last_addr×2 (FPGA formula, adj=0)
    let count = (words.len() * 2 % 0x1_0000) as u32;
    (sum + count) % 0x1_0000
}

/// Build the raw DDR image bytes for network loading (TCP netboot).
///
/// Mirrors the `.kbt` memory layout ([`Image::to_kbt`]) but emits **raw
/// little-endian bytes** — no ASCII-hex, no `S`/`Z`/`X` framing: a 4×64-bit
/// heap header followed by the program, padded so the total length is the
/// 8-byte-aligned `heap_start`.  Word 0 lo32 holds `heap_start` (first free
/// byte after the program); the rest of the header is zero.  The entry point
/// is transmitted separately in the netboot protocol header, not embedded
/// here.
#[must_use]
pub(crate) fn build_ddr_image(binary_data: &[u8]) -> Vec<u8> {
    const HEADER_BYTES: usize = CODE_BASE as usize;
    let mut image = vec![0_u8; HEADER_BYTES];
    image.extend_from_slice(binary_data);
    // Pad to a 4-byte word boundary (matches the kbt path's word emission).
    while !image.len().is_multiple_of(4) {
        image.push(0);
    }
    let heap_start = heap_start(image.len() as u32);
    // Pad the image out to heap_start so it is itself 8-byte aligned.
    image.resize(heap_start as usize, 0);
    // Word 0 lo32 = heap_start (LE); hi32 stays zero.
    image[0..4].copy_from_slice(&heap_start.to_le_bytes());
    image
}

/// Format a DDR image as a `$readmemh` boot-ROM file.
///
/// One 64-bit little-endian doubleword per line, as 16 upper-case hex digits,
/// matching `boot_rom.sv` (`DEPTH_DW` × 64-bit).  A DDR image is always a whole
/// number of doublewords; a trailing part doubleword would be zero-filled.
#[must_use]
pub(crate) fn readmemh(ddr: &[u8]) -> String {
    let mut out = String::with_capacity(ddr.len().div_ceil(8) * 17);
    for chunk in ddr.chunks(8) {
        let mut dw = [0_u8; 8];
        dw[..chunk.len()].copy_from_slice(chunk);
        let _ = writeln!(out, "{:016X}", u64::from_le_bytes(dw));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::line::{Emitted, LineType};

    /// A `Pass2` line of `line_type` at `program_counter` emitting `emitted`.
    fn line(line_type: LineType, program_counter: u32, emitted: Emitted) -> Pass2 {
        Pass2 {
            emitted,
            file_name: String::from("test"),
            input_text_line: String::default(),
            line_counter: 0,
            program_counter,
            line_type,
        }
    }

    #[test]
    fn emitted_bytes_are_in_memory_order() {
        let code = Emitted::Code(vec![0x400F_0000, 0x1122_3344]);
        let mut bytes = vec![0; code.byte_len()];
        code.copy_to(&mut bytes);
        assert_eq!(bytes, [0x00, 0x00, 0x0F, 0x40, 0x44, 0x33, 0x22, 0x11]);
        // Data bytes are already in memory order.
        let data = Emitted::Data(vec![0xAA, 0xBB]);
        let mut bytes = vec![0; data.byte_len()];
        data.copy_to(&mut bytes);
        assert_eq!(bytes, [0xAA, 0xBB]);
        assert_eq!(Emitted::None.byte_len(), 0);
    }

    #[test]
    fn from_lines_places_code_by_address() {
        let lines = [
            line(LineType::Start, 0x20, Emitted::None),
            line(LineType::Opcode, 0x20, Emitted::Code(vec![0x8BD0_0000, 0x41])),
            line(LineType::Label, 0x28, Emitted::None),
            line(LineType::Opcode, 0x28, Emitted::Code(vec![0x6C01_0000])),
        ];
        let image = Image::from_lines(&lines, 0x20);
        assert_eq!(image.entry, 0x20);
        assert_eq!(image.bytes, vec![0x00, 0x00, 0xD0, 0x8B, 0x41, 0, 0, 0, 0x00, 0x00, 0x01, 0x6C]);
        // Out of source order: still placed by address.
        let mut reversed = lines.clone();
        reversed.reverse();
        assert_eq!(Image::from_lines(&reversed, 0x20), image);
        // Data is copied as it is.
        let data = [line(LineType::Data, 0x24, Emitted::Data(vec![1, 2, 3, 4]))];
        assert_eq!(Image::from_lines(&data, 0x20).bytes, [0, 0, 0, 0, 1, 2, 3, 4]);
    }

    #[test]
    fn to_kbt_frames_header_program_entry_and_checksum() {
        let image = Image::new(vec![0x00, 0x00, 0x01, 0x6C], 0x20);
        let mut msg_list = Diagnostics::new();
        let kbt = image.to_kbt(&mut msg_list).unwrap();
        // heap_start = 0x20 + 4 = 0x24, rounded up to 8 = 0x28.
        let header = format!("28000000{}", "0".repeat(8 + 3 * 16));
        let body = "0000016C";
        let entry = "20000000";
        // sum: 0x28 + (0x6C01 + 0x0000) + 0x20 = 0x6C49; count = 2 × 10 words = 20.
        let checksum = "5D6C0000"; // 0x6C49 + 20, little-endian
        assert_eq!(kbt, format!("S{header}{body}{entry}Z{checksum}X"));
        assert!(msg_list.messages().is_empty());
    }

    #[test]
    fn to_kbt_rejects_a_part_word() {
        let mut msg_list = Diagnostics::new();
        assert_eq!(Image::new(vec![1, 2, 3], 0x20).to_kbt(&mut msg_list), None);
        assert_eq!(
            msg_list.messages()[0].text(),
            "Program image is not a whole number of 32-bit words, length is 78 hex characters"
        );

        let mut image = Image::new(vec![1, 2, 3], 0x20);
        image.pad_to_word();
        assert_eq!(image.bytes, vec![1, 2, 3, 0]);
        assert!(image.to_kbt(&mut Diagnostics::new()).is_some());
    }

    #[test]
    fn ddr_image_is_header_then_program_padded_to_heap_start() {
        let ddr = Image::new(vec![0xAA; 5], 0x20).to_ddr();
        // 0x20 + 5 = 0x25 → words to 0x28 → heap_start 0x28.
        assert_eq!(ddr.len(), 0x28);
        assert_eq!(ddr[0..8], [0x28, 0, 0, 0, 0, 0, 0, 0]);
        assert!(ddr[8..0x20].iter().all(|&b| b == 0));
        assert_eq!(ddr[0x20..0x25], [0xAA; 5]);
        assert!(ddr[0x25..].iter().all(|&b| b == 0));
        // Already 8-aligned: no padding beyond the program.
        assert_eq!(build_ddr_image(&[1; 8]).len(), 0x28);
    }

    #[test]
    fn readmemh_writes_one_le_doubleword_per_line() {
        let ddr = [0x28, 0, 0, 0, 0, 0, 0, 0, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11];
        assert_eq!(readmemh(&ddr), "0000000000000028\n1122334455667788\n");
        assert_eq!(readmemh(&[]), "");
        // A part doubleword is zero-filled.
        assert_eq!(readmemh(&[0x01, 0x02]), "0000000000000201\n");
    }

    #[test]
    fn readmemh_of_a_ddr_image_has_heap_start_first() {
        let ddr = Image::new(vec![0x00, 0x00, 0x01, 0x6C], 0x20).to_ddr();
        let mem = readmemh(&ddr);
        let lines: Vec<&str> = mem.lines().collect();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0], "0000000000000028");
        assert_eq!(lines[4], "000000006C010000");
    }

    #[test]
    // The checksum sums each word's half words, plus two per word, mod 2^16
    fn kbt_checksum_sums_half_words_and_count() {
        assert_eq!(kbt_checksum(&[]), 0);
        assert_eq!(kbt_checksum(&[0x0001_0002]), 3 + 2);
        assert_eq!(kbt_checksum(&[0xFFFF_FFFF, 0xFFFF_FFFF]), (4 * 0xFFFF + 4) % 0x1_0000);
        let mut out = String::new();
        push_word_kbt(&mut out, 0x400F_0000);
        assert_eq!(out, "00000F40");
    }
}
