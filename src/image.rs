//! The board's program image, and the formats it is loaded in.
//!
//! Every loader sees the same memory: a heap header of [`HEAP_HEADER_WORDS`]
//! 64-bit words at address 0, then the program at [`CODE_BASE`].  Word 0 lo32
//! holds `heap_start`, the first free byte after the program rounded up to 8;
//! the rest of the header is zero.  [`Image`] holds the program and its entry
//! point once, and writes it as the serial `.kbt` wire format
//! ([`Image::to_kbt`]) or as raw DDR bytes ([`Image::to_ddr`]) for the network
//! boot, the boot ROM ([`readmemh`]) and the emulator.

use crate::asm::line::{LineType, Pass2};
use crate::layout::{heap_start, CODE_BASE, HEAP_HEADER_WORDS};
use crate::messages::{MessageType, MsgList};
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

    /// Lay out assembled lines in memory.
    ///
    /// Each line's machine code is placed at its `program_counter` (relative
    /// to [`CODE_BASE`]); see [`hex_to_bytes`] for how the hex becomes bytes.
    /// The entry point is the `_start` line's address.  Returns `None` when the
    /// program has no `_start`.
    #[must_use]
    pub(crate) fn from_lines(lines: &[Pass2]) -> Option<Self> {
        let mut code: Vec<u8> = Vec::new();
        let mut entry: Option<u32> = None;

        for line in lines {
            if line.line_type == LineType::Start {
                entry = Some(line.program_counter);
                continue;
            }
            if line.opcode.is_empty() {
                continue;
            }
            let offset = line.program_counter.saturating_sub(CODE_BASE) as usize;
            let bytes = hex_to_bytes(&line.opcode);
            if offset + bytes.len() > code.len() {
                code.resize(offset + bytes.len(), 0);
            }
            code[offset..offset + bytes.len()].copy_from_slice(&bytes);
        }

        entry.map(|entry| Self::new(code, entry))
    }

    /// The `.kbt` serial wire format: `S`, the heap header, the program, the
    /// entry point, `Z`, the checksum and `X`.
    ///
    /// Every byte is sent as two hex digits in memory order, so each 32-bit
    /// word goes least significant byte first, as the board's serial loader
    /// stores bytes in the order they arrive.  Returns `None` (reported) when
    /// the program is not a whole number of 32-bit words, which the board
    /// cannot load; see [`calc_checksum`].
    #[must_use]
    pub(crate) fn to_kbt(&self, msg_list: &mut MsgList) -> Option<String> {
        let mut out = String::with_capacity(1 + (CODE_BASE as usize + self.bytes.len() + 4) * 2 + 10);
        out.push('S');

        // Word 0: heap_start in lo32 (LE), hi32 zero.  Words 1-3 are reserved.
        out.push_str(&encode_word_kbt(self.heap_start()));
        out.push_str("00000000");
        for _ in 1..HEAP_HEADER_WORDS {
            out.push_str("0000000000000000");
        }

        for byte in &self.bytes {
            let _ = write!(out, "{byte:02X}");
        }

        // Entry point, LE-encoded like every other 32-bit word.
        out.push_str(&encode_word_kbt(self.entry));

        // The checksum covers everything before the Z delimiter.
        let checksum = calc_checksum(&out, msg_list)?;
        out.push('Z');
        out.push_str(&checksum);
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

/// The memory bytes of one assembled line's hex machine code.
///
/// Each whole 8-digit group is a 32-bit word, stored little-endian so the
/// CPU's 32-bit reads see its natural value; data is written as words the
/// same way.  A trailing group of fewer than 8 digits is taken as raw bytes,
/// in order.  Groups that are not valid hex are skipped.
fn hex_to_bytes(hex: &str) -> Vec<u8> {
    let (words, rest) = hex.as_bytes().as_chunks::<8>();
    let mut bytes: Vec<u8> = Vec::with_capacity(hex.len() / 2);
    for word in words {
        if let Some(w) = std::str::from_utf8(word).ok().and_then(|h| u32::from_str_radix(h, 16).ok()) {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
    }
    for pair in rest.as_chunks::<2>().0 {
        if let Some(b) = std::str::from_utf8(pair).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
            bytes.push(b);
        }
    }
    bytes
}

/// Encode a 32-bit word for the kbt wire format (little-endian byte order).
///
/// The board's serial loader stores bytes in the order they arrive, so a 32-bit
/// value must be transmitted LSByte-first so the CPU reads the correct value.
/// e.g. instruction 0x400F0000 → "00000F40", entry 0x20 → "20000000".
#[must_use]
fn encode_word_kbt(w: u32) -> String {
    format!("{:02X}{:02X}{:02X}{:02X}", w & 0xFF, (w >> 8) & 0xFF, (w >> 16) & 0xFF, (w >> 24) & 0xFF)
}

/// Compute the kbt checksum for a LE-encoded wire string.
///
/// For each 8-char (32-bit) chunk in the data stream the LE bytes are decoded back to
/// the natural 32-bit word V, and `V[31:16] + V[15:0]` is accumulated.  The checksum
/// formula is:
///
///   `chk = (running_sum + count) % 65536`.
///
/// where `count` is the number of 16-bit half-word groups accumulated (= 2 × number
/// of 32-bit words).  This matches the FPGA formula `chk = running_sum + last_addr×2`.
///
/// The result is a 32-bit word emitted in LE byte order (8 hex chars), exactly like
/// every other word in the stream.  The caller is responsible for writing the `Z`
/// delimiter; this function must be called **before** appending `Z` so that `Z` is
/// not included in the sum.
///
/// The board loads whole 32-bit words, so a stream that is not a whole number of
/// them cannot be loaded: that is reported as an error and `None` is returned,
/// rather than a checksum the board would reject.
#[must_use]
fn calc_checksum(input_string: &str, msg_list: &mut MsgList) -> Option<String> {
    // Strip S framing char only (Z/X are not present — caller hasn't appended them yet).
    let stripped: String = input_string.chars().filter(|&c| c != 'S').collect();

    if !stripped.len().is_multiple_of(8) {
        msg_list.push(
            format!(
                "Program image is not a whole number of 32-bit words, length is {} hex characters",
                stripped.len()
            ),
            None,
            None,
            MessageType::Error,
        );
        return None;
    }

    let mut sum: u32 = 0;
    let mut count: u32 = 0;

    // Process 8-char groups as LE-encoded 32-bit words.
    // Decode each word to its natural value V, accumulate V[31:16] + V[15:0].
    for chunk in stripped.as_bytes().as_chunks::<8>().0 {
        let le = std::str::from_utf8(chunk)
            .ok()
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .unwrap_or(0);
        let v = le.swap_bytes(); // natural V = b0 | b1<<8 | b2<<16 | b3<<24
        sum = (sum + (v >> 16) + (v & 0xFFFF)) % 0x1_0000;
        count += 2;
    }

    // chk = running_sum + last_addr×2 (FPGA formula, adj=0)
    let chk = (sum + count) % 0x1_0000;
    Some(encode_word_kbt(chk))
}

/// Return String of bit codes with start/stop bytes and CRC.
///
/// Based on the Pass2 vector, create the `.kbt` wire string: the lines' machine
/// code is laid out one after another (see [`hex_to_bytes`]) and framed by
/// [`Image::to_kbt`].  The program must have exactly one `_start`, which is
/// its entry point; otherwise that is reported and `None` returned.
pub(crate) fn create_bin_string(pass2: &[Pass2], msg_list: &mut MsgList) -> Option<String> {
    let mut starts = pass2.iter().filter(|x| x.line_type == LineType::Start);
    let entry = match (starts.next(), starts.next()) {
        (Some(start), None) => start.program_counter,
        (None, _) => {
            msg_list.push("No start address found".to_owned(), None, None, MessageType::Error);
            return None;
        }
        (Some(_), Some(_)) => {
            msg_list.push("Multiple start addresses found".to_owned(), None, None, MessageType::Error);
            return None;
        }
    };
    let bytes = pass2.iter().flat_map(|pass| hex_to_bytes(&pass.opcode)).collect();
    Image::new(bytes, entry).to_kbt(msg_list)
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
/// matching `boot_rom.v` (`DEPTH_DW` × 64-bit).  A DDR image is always a whole
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

    /// A `Pass2` line of `line_type` at `program_counter` with machine code `opcode`.
    fn line(line_type: LineType, program_counter: u32, opcode: &str) -> Pass2 {
        Pass2 {
            opcode: opcode.to_owned(),
            file_name: String::from("test"),
            input_text_line: String::default(),
            line_counter: 0,
            program_counter,
            line_type,
        }
    }

    #[test]
    fn hex_to_bytes_stores_words_little_endian() {
        assert_eq!(hex_to_bytes("400F0000"), vec![0x00, 0x00, 0x0F, 0x40]);
        assert_eq!(hex_to_bytes("1122334455667788"), vec![0x44, 0x33, 0x22, 0x11, 0x88, 0x77, 0x66, 0x55]);
        // A part word is kept as raw bytes, in order.
        assert_eq!(hex_to_bytes("11223344AABB"), vec![0x44, 0x33, 0x22, 0x11, 0xAA, 0xBB]);
        assert_eq!(hex_to_bytes(""), Vec::<u8>::new());
    }

    #[test]
    fn from_lines_places_code_by_address() {
        let lines = [
            line(LineType::Start, 0x20, ""),
            line(LineType::Opcode, 0x20, "8BD0000000000041"),
            line(LineType::Label, 0x28, ""),
            line(LineType::Opcode, 0x28, "6C010000"),
        ];
        let image = Image::from_lines(&lines).unwrap();
        assert_eq!(image.entry, 0x20);
        assert_eq!(image.bytes, vec![0x00, 0x00, 0xD0, 0x8B, 0x41, 0, 0, 0, 0x00, 0x00, 0x01, 0x6C]);
        // No _start: nothing to run.
        assert_eq!(Image::from_lines(&lines[1..]), None);
    }

    #[test]
    fn to_kbt_frames_header_program_entry_and_checksum() {
        let image = Image::new(vec![0x00, 0x00, 0x01, 0x6C], 0x20);
        let mut msg_list = MsgList::new();
        let kbt = image.to_kbt(&mut msg_list).unwrap();
        // heap_start = 0x20 + 4 = 0x24, rounded up to 8 = 0x28.
        let header = format!("28000000{}", "0".repeat(8 + 3 * 16));
        let body = "0000016C";
        let entry = "20000000";
        // sum: 0x28 + (0x6C01 + 0x0000) + 0x20 = 0x6C49; count = 2 × 10 words = 20.
        let checksum = encode_word_kbt(0x6C49 + 20);
        assert_eq!(kbt, format!("S{header}{body}{entry}Z{checksum}X"));
        assert!(msg_list.list.is_empty());
    }

    #[test]
    fn to_kbt_matches_the_assembled_kbt() {
        // The same program through create_bin_string and through an Image.
        let lines = [
            line(LineType::Start, 0x20, ""),
            line(LineType::Opcode, 0x20, "8BD0000000000041"),
            line(LineType::Opcode, 0x28, "6C010000"),
        ];
        let mut msg_list = MsgList::new();
        let image = Image::from_lines(&lines).unwrap();
        assert_eq!(image.to_kbt(&mut msg_list), create_bin_string(&lines, &mut msg_list));
        assert!(msg_list.list.is_empty());
    }

    #[test]
    fn to_kbt_rejects_a_part_word() {
        let mut msg_list = MsgList::new();
        assert_eq!(Image::new(vec![1, 2, 3], 0x20).to_kbt(&mut msg_list), None);
        assert_eq!(
            msg_list.list[0].text,
            "Program image is not a whole number of 32-bit words, length is 78 hex characters"
        );

        let mut image = Image::new(vec![1, 2, 3], 0x20);
        image.pad_to_word();
        assert_eq!(image.bytes, vec![1, 2, 3, 0]);
        assert!(image.to_kbt(&mut MsgList::new()).is_some());
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
    // Test that the bin_string is created correctly with start value

    fn test_create_bin_string1() {
        let pass2 = &mut Vec::<Pass2>::new();
        pass2.push(Pass2 {
            opcode: String::default(),
            file_name: String::from("test"),
            input_text_line: String::default(),
            line_counter: 0,
            program_counter: 1,
            line_type: LineType::Start,
        });
        pass2.push(Pass2 {
            opcode: String::from("1234"),
            file_name: String::from("test"),
            input_text_line: String::default(),
            line_counter: 0,
            program_counter: 3,
            line_type: LineType::Data,
        });
        pass2.push(Pass2 {
            opcode: String::from("4321"),
            input_text_line: String::default(),
            file_name: String::from("test"),
            line_counter: 0,
            program_counter: 5,
            line_type: LineType::Data,
        });
        let mut msg_list = MsgList::new();
        let bin_string = create_bin_string(pass2, &mut msg_list);
        // Word 0: heap_start=0x28 LE → "28000000"; hi32=0 → "00000000".
        // Entry pc=1 LE → "01000000". 4-char opcodes pass through unchanged.
        // No "Z0010" — checksum is a 32-bit LE word immediately after Z.
        // 10 × 8-char groups (8 header + "12344321" + entry); count=20.
        // sum: "28000000"→40 (0x28), "12344321"→21845, "01000000"→1 = 21886.
        // chk = (21886+20)%65536 = 21906 = 0x5592; LE → "92550000".
        assert_eq!(
            bin_string,
            Some("S28000000000000000000000000000000000000000000000000000000000000001234432101000000Z92550000X".to_owned())
        );
    }

    #[test]
    // A stream that is not whole 32-bit words gets no .kbt at all, rather than
    // one with a zero checksum
    fn test_create_bin_string_part_word() {
        let pass2 = vec![
            Pass2 {
                opcode: String::default(),
                file_name: String::from("test"),
                input_text_line: String::default(),
                line_counter: 0,
                program_counter: 0x20,
                line_type: LineType::Start,
            },
            Pass2 {
                opcode: String::from("000000"),
                file_name: String::from("test"),
                input_text_line: String::default(),
                line_counter: 0,
                program_counter: 0x20,
                line_type: LineType::Data,
            },
        ];
        let mut msg_list = MsgList::new();
        assert_eq!(create_bin_string(&pass2, &mut msg_list), None);
        assert_eq!(
            msg_list.list[0].text,
            "Program image is not a whole number of 32-bit words, length is 78 hex characters"
        );
        assert_eq!(calc_checksum("S0000", &mut MsgList::new()), None);
    }

    #[test]
    // Test that the bin_string is null if duplicate starts

    fn test_create_bin_string2() {
        let pass2 = &mut Vec::<Pass2>::new();
        pass2.push(Pass2 {
            opcode: String::default(),
            file_name: String::from("test"),
            input_text_line: String::default(),
            line_counter: 0,
            program_counter: 1,
            line_type: LineType::Start,
        });
        pass2.push(Pass2 {
            opcode: String::default(),
            file_name: String::from("test"),
            input_text_line: String::default(),
            line_counter: 0,
            program_counter: 3,
            line_type: LineType::Start,
        });
        pass2.push(Pass2 {
            opcode: String::from("4321"),
            input_text_line: String::default(),
            file_name: String::from("test"),
            line_counter: 0,
            program_counter: 5,
            line_type: LineType::Data,
        });
        let mut msg_list = MsgList::new();
        let bin_string = create_bin_string(pass2, &mut msg_list);
        assert_eq!(bin_string, None);
        assert_eq!(msg_list.list[0].text, "Multiple start addresses found");
    }

    #[test]
    // Test that the bin_string is null if no starts

    fn test_create_bin_string3() {
        let pass2 = &mut Vec::<Pass2>::new();
        pass2.push(Pass2 {
            opcode: String::default(),
            file_name: String::from("test"),
            input_text_line: String::default(),
            line_counter: 0,
            program_counter: 1,
            line_type: LineType::Comment,
        });
        pass2.push(Pass2 {
            opcode: String::from("1234"),
            file_name: String::from("test"),
            input_text_line: String::default(),
            line_counter: 0,
            program_counter: 3,
            line_type: LineType::Data,
        });
        pass2.push(Pass2 {
            opcode: String::from("4321"),
            input_text_line: String::default(),
            file_name: String::from("test"),
            line_counter: 0,
            program_counter: 5,
            line_type: LineType::Data,
        });
        let mut msg_list = MsgList::new();
        let bin_string = create_bin_string(pass2, &mut msg_list);
        assert_eq!(bin_string, None);
        assert_eq!(msg_list.list[0].text, "No start address found");
    }
}
