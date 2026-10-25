use crate::asm::lexer::{data_name_from_string, is_blank, is_comment, is_start, label_name_from_string};
use crate::asm::line::{LineType, Pass2};
use crate::diag::Diagnostics;
use crate::num::{parse_bits64, parse_int, LiteralError};
use crate::opcodes::{disassemble_word, return_opcode, Opcode};
use std::fmt;
use std::fmt::Write as _;

/// Format a byte count as a human-readable size (B / KiB / MiB / GiB), e.g.
/// `9223315` -> `"8.80 MiB"`.  Used for progress/size display.
#[must_use]
pub(crate) fn human_bytes(n: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1_048_576.0;
    const GIB: f64 = 1_073_741_824.0;
    let f = n as f64;
    if f >= GIB {
        format!("{:.2} GiB", f / GIB)
    } else if f >= MIB {
        format!("{:.2} MiB", f / MIB)
    } else if f >= KIB {
        format!("{:.1} KiB", f / KIB)
    } else {
        format!("{n} B")
    }
}

/// Disassemble a flat byte slice into a `Vec<Pass2>` for use with `write_code_output_file`.
///
/// Each 4-byte chunk is read as a little-endian 32-bit word (matching the `KlaussCPU` LLVM ELF
/// byte order) and matched against `opcodes`.  When a match is found, register operands are
/// extracted and any argument words that follow (determined by `opcode.variables`) are consumed
/// and appended to the same entry's `opcode` hex string so `write_code_output_file` formats them
/// correctly.  Unrecognised words produce a `???` fallback line.  When the binary ends before
/// all of an instruction's argument words, the words that are there are shown once, on that
/// instruction's line, marked `(truncated)`.
///
/// `base_addr` is the board byte address of the first byte in `binary` (normally
/// [`CODE_BASE`] = 0x20).
pub(crate) fn disassemble_flat_to_pass2(binary: &[u8], base_addr: u32, opcodes: &[Opcode]) -> Vec<Pass2> {
    let mut result: Vec<Pass2> = Vec::new();
    let mut i: usize = 0;

    while i + 4 <= binary.len() {
        let word = u32::from_le_bytes([binary[i], binary[i + 1], binary[i + 2], binary[i + 3]]);
        let pc = base_addr.saturating_add(i as u32);

        let (base_text, vars) = disassemble_word(word, opcodes).unwrap_or_else(|| (format!("??? (0x{word:08X})"), 0_u32));

        let mut opcode_hex = format!("{word:08X}");
        let mut display_text = base_text;

        // Consume argument words that immediately follow the instruction word.
        // vars == 1 → one 32-bit immediate at PC+4.
        // vars == 2 → two 32-bit words at PC+4 (lo32) and PC+8 (hi32), forming a 64-bit value.
        let arg_word_count = vars.min(2) as usize;
        let args: Vec<u32> = (0..arg_word_count)
            .map_while(|n| {
                let off = i + 4 + n * 4;
                binary.get(off..off + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes)
            })
            .collect();
        for av in &args {
            let _ = write!(opcode_hex, "{av:08X}");
        }
        match *args.as_slice() {
            [av] if arg_word_count == 1 => _ = write!(display_text, " 0x{av:X}"),
            [lo, hi] => _ = write!(display_text, " 0x{:X}", (u64::from(hi) << 32) | u64::from(lo)),
            [lo] => _ = write!(display_text, " 0x{lo:08X}"),
            _ => {}
        }
        if args.len() < arg_word_count {
            display_text.push_str(" (truncated)");
        }

        // Consume exactly the words shown, so none is shown twice.
        let words_consumed = 1 + args.len();

        result.push(Pass2 {
            file_name: String::new(),
            input_text_line: display_text,
            line_counter: 0,
            line_type: LineType::Opcode,
            opcode: opcode_hex,
            program_counter: pc,
        });

        i += words_consumed * 4;
    }

    result
}

/// Largest single data item, in bytes: the whole of the board's 128 MiB memory.
const MAX_DATA_BYTES: i128 = 128 * 1024 * 1024;

/// Why a data line could not be turned into bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DataError {
    /// The line is not a complete data definition (missing value, bad string, ...).
    Malformed,
    /// A numeric value is not a valid literal.
    BadValue(String, LiteralError),
    /// A size is zero or negative.
    NotPositive(String),
    /// A size is larger than the board's memory.
    TooLarge(String),
    /// A byte count is not a whole number of 32-bit words.
    NotWordMultiple(String),
}

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => f.write_str("malformed data definition"),
            Self::BadValue(token, err) => write!(f, "invalid value \"{token}\" ({err})"),
            Self::NotPositive(token) => write!(f, "size \"{token}\" must be greater than zero"),
            Self::TooLarge(token) => write!(f, "size \"{token}\" is larger than memory"),
            Self::NotWordMultiple(token) => write!(f, "size \"{token}\" must be a multiple of 4 bytes"),
        }
    }
}

/// Parse a data size (bytes or words) that must be positive and at most `max`.
fn data_size(token: &str, max: i128) -> Result<usize, DataError> {
    let value = parse_int(token).map_err(|err| DataError::BadValue(token.to_owned(), err))?;
    if value <= 0 {
        return Err(DataError::NotPositive(token.to_owned()));
    }
    if value > max {
        return Err(DataError::TooLarge(token.to_owned()));
    }
    // 1..=max, and max fits in usize on every supported target.
    Ok(value as usize)
}

/// Returns bytes for data element.
///
/// Parses data element and returns data as bytes, or None if error.
pub(crate) fn data_as_bytes(line: &str) -> Option<String> {
    data_bytes(line).ok()
}

/// Returns bytes for data element, or why the line is not valid data.
///
/// The bytes are returned as a hex string (two characters per byte).
pub(crate) fn data_bytes(line: &str) -> Result<String, DataError> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");
    if first_word.is_empty() {
        return Err(DataError::Malformed);
    }

    // Handle .word VALUE directive — emit a single 64-bit word
    if first_word == ".word" {
        let value_str = words.next().unwrap_or("");
        if value_str.is_empty() {
            return Err(DataError::Malformed);
        }
        let v64 = parse_bits64(value_str).map_err(|err| DataError::BadValue(value_str.to_owned(), err))?;
        let lo32 = (v64 & 0xFFFF_FFFF) as u32;
        let hi32 = ((v64 >> 32) & 0xFFFF_FFFF) as u32;
        return Ok(format!("{lo32:08X}{hi32:08X}"));
    }

    // Handle .zeropad N directive — EXACTLY N bytes of zero (no rounding to 8).
    // Assembler-internal: emitted by get_pass1 to 8-byte-align the start of a data
    // section so 64-bit loads (MEMREADR / MEMGET64), which mask the address down to
    // an 8-byte boundary, read .word data cleanly instead of the preceding word.
    // N must be a whole number of 32-bit words: the board loads the program a word
    // at a time, so a part word would shift everything after it, entry included.
    if first_word == ".zeropad" {
        let count_str = words.next().unwrap_or("");
        if count_str.is_empty() {
            return Err(DataError::Malformed);
        }
        let byte_count = data_size(count_str, MAX_DATA_BYTES)?;
        if !byte_count.is_multiple_of(4) {
            return Err(DataError::NotWordMultiple(count_str.to_owned()));
        }
        return Ok("00".repeat(byte_count));
    }

    // Handle .space N directive — N bytes of zero, rounded up to 64-bit word boundary
    if first_word == ".space" {
        let count_str = words.next().unwrap_or("");
        if count_str.is_empty() {
            return Err(DataError::Malformed);
        }
        let byte_count = data_size(count_str, MAX_DATA_BYTES)?;
        let word_count = byte_count.div_ceil(8);
        return Ok("0000000000000000".repeat(word_count));
    }

    let second_word = words.next().unwrap_or("");
    if second_word.is_empty() {
        return Err(DataError::Malformed);
    }

    // Check if next word starts with quote
    if second_word.starts_with('\"') {
        let remaining_line = line.trim_start_matches(first_word).trim();

        if remaining_line.starts_with('\"') && remaining_line.ends_with('\"') {
            let input_string = remaining_line.trim_matches('\"').replace("\\n", "\r\n");
            // Length prefix: the number of 32-bit words holding the characters,
            // which always include at least one NUL byte.
            let char_words = input_string.len() / 4 + 1;
            let mut output_hex = format!("{char_words:08X}");
            for char in input_string.as_bytes() {
                let _ = write!(output_hex, "{char:02X}");
            }
            // NUL-pad the characters out to `char_words`, then the whole item out to
            // a 64-bit word, like every other data item, so data after a string stays
            // 8-byte aligned for MEMGET64 / MEMSET64.
            let item_bytes = (4 + char_words * 4).next_multiple_of(8);
            while output_hex.len() < item_bytes * 2 {
                output_hex.push('0');
            }
            return Ok(output_hex);
        }
        Err(DataError::Malformed)
    } else {
        // `#name N` reserves N zeroed 64-bit words.
        let word_count = data_size(second_word, MAX_DATA_BYTES / 8)?;
        Ok("0000000000000000".repeat(word_count))
    }
}

/// Check if line is valid.
///
/// Returns true if line is not error.
pub(crate) fn is_valid_line(opcodes: &[Opcode], line: &str) -> bool {
    if line_type(opcodes, line) == LineType::Error {
        return false;
    }
    true
}

/// Returns enum of type of line.
///
/// Given a code line, will returns if line is Label, Opcode, Blank, Comment or Error.
pub(crate) fn line_type(opcodes: &[Opcode], line: &str) -> LineType {
    if label_name_from_string(line).is_some() {
        return LineType::Label;
    }
    if data_name_from_string(line).is_some() {
        return LineType::Data;
    }
    if return_opcode(line, opcodes).is_some() {
        return LineType::Opcode;
    }
    if is_blank(line) {
        return LineType::Blank;
    }
    if is_start(line) {
        return LineType::Start;
    }
    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        if is_comment(word) && i == 0 {
            return LineType::Comment;
        }
    }
    // Check for C compiler directives
    let first_word = line.split_whitespace().next().unwrap_or("");
    match first_word {
        ".word" | ".space" | ".zeropad" => return LineType::Data,
        ".text" | ".data" | ".rodata" | ".bss" | ".global" | ".globl" | ".extern" | ".comm" | ".lcomm" => return LineType::Comment,
        _ => {}
    }
    LineType::Error
}

/// Return number of bytes of data.
///
/// From instruction name, option of number of bytes of data, or 0 is error.
pub(crate) fn num_data_bytes(line: &str, msg_list: &mut Diagnostics, line_number: u32, filename: &str) -> u32 {
    match data_bytes(line) {
        Ok(data) => data.len().try_into().unwrap_or_default(),
        Err(err) => {
            let text = if err == DataError::Malformed {
                format!("Error in data definition for {line}")
            } else {
                format!("Error in data definition for {line}: {err}")
            };
            msg_list.error_at(filename, line_number, text);
            0
        }
    }
}

/// Parse expected UART hex values from source file comment headers.
///
/// Extracts 8-digit uppercase hex values from lines matching the pattern `//   XXXXXXXX  (`.
pub(crate) fn parse_expected_uart_values(lines: &[String]) -> Vec<String> {
    let mut expected: Vec<String> = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if !trimmed.starts_with("//") {
            continue;
        }
        let comment_body = trimmed.get(2..).unwrap_or("").trim();
        if comment_body.len() >= 8 {
            let candidate = comment_body.get(..8).unwrap_or("");
            if candidate.len() == 8 && candidate.chars().all(|c| c.is_ascii_hexdigit()) && candidate == candidate.to_ascii_uppercase() {
                expected.push(candidate.to_owned());
            }
        }
    }
    expected
}

/// Trim newline from string.
///
/// Removes newline from end of string.
pub(crate) fn trim_newline(input: &mut String) {
    if input.ends_with('\n') {
        input.pop();
        if input.ends_with('\r') {
            input.pop();
        }
    }
    if input.ends_with('\r') {
        input.pop();
        if input.ends_with('\n') {
            input.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::MessageType;
    use crate::labels::{return_label_value, Label};

    #[test]
    // Test that line is trimmed of newline
    fn test_trim_newline1() {
        let mut test_string: String = String::from("Hello\n");
        trim_newline(&mut test_string);
        assert_eq!(test_string, "Hello");
    }
    #[test]
    // Test that line is trimmed of newline
    fn test_trim_newline2() {
        let mut test_string: String = String::from("Hello\r\n");
        trim_newline(&mut test_string);
        assert_eq!(test_string, "Hello");
    }
    #[test]
    // Test that line is trimmed of newline
    fn test_trim_newline3() {
        let mut test_string: String = String::from("Hello\n\r");
        trim_newline(&mut test_string);
        assert_eq!(test_string, "Hello");
    }

    /// Little-endian bytes of `words`, as an ELF or flat binary holds them.
    fn le_bytes(words: &[u32]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    // Words are read little-endian; a 64-bit argument is shown as one value
    fn test_disassemble_flat_to_pass2_le_and_64bit() {
        let opcodes = crate::opcodes::v2_opcodes();
        let pass2 = disassemble_flat_to_pass2(&le_bytes(&[0x8BD0_0000, 5, 0xCBC0_0000, 0x5566_7788, 0x1122_3344]), 0x20, &opcodes);
        assert_eq!(pass2.len(), 2);
        assert_eq!(pass2[0].program_counter, 0x20);
        assert_eq!(pass2[0].opcode, "8BD0000000000005");
        assert!(pass2[0].input_text_line.starts_with("SETR "), "{}", pass2[0].input_text_line);
        assert!(pass2[0].input_text_line.ends_with(" 0x5"), "{}", pass2[0].input_text_line);
        assert_eq!(pass2[1].program_counter, 0x28);
        assert_eq!(pass2[1].opcode, "CBC000005566778811223344");
        assert!(pass2[1].input_text_line.ends_with(" 0x1122334455667788"), "{}", pass2[1].input_text_line);
    }

    #[test]
    // Regression: a 64-bit argument cut off by the end of the binary showed its
    // lo32 word on the instruction's line and then again as the next line.
    fn test_disassemble_flat_to_pass2_truncated_argument() {
        let opcodes = crate::opcodes::v2_opcodes();
        let pass2 = disassemble_flat_to_pass2(&le_bytes(&[0xCBC0_0000, 0x5566_7788]), 0x20, &opcodes);
        assert_eq!(pass2.len(), 1);
        assert_eq!(pass2[0].opcode, "CBC0000055667788");
        assert!(
            pass2[0].input_text_line.ends_with(" 0x55667788 (truncated)"),
            "{}",
            pass2[0].input_text_line
        );

        let pass2 = disassemble_flat_to_pass2(&le_bytes(&[0x8BD0_0000]), 0x20, &opcodes);
        assert_eq!(pass2.len(), 1);
        assert!(pass2[0].input_text_line.ends_with(" (truncated)"), "{}", pass2[0].input_text_line);
    }

    #[test]
    // Test for valid line returns true is opcode is found
    fn test_is_valid_line1() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 0,
            section: String::default(),
        });
        let output = is_valid_line(opcodes, &input);
        assert!(output);
    }

    #[test]
    fn test_is_valid_line2() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PULL"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 0,
            section: String::default(),
        });
        let output = is_valid_line(opcodes, &input);
        assert!(!output);
    }

    #[test]
    // Test for opcode line type
    fn test_line_type1() {
        let input = String::from("PUSH");
        let opcodes = vec![Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 0,
            section: String::default(),
        }];
        let output = line_type(&opcodes, &input);
        assert_eq!(output, LineType::Opcode);
    }
    #[test]
    // Test for label line type
    fn test_line_type2() {
        let input = String::from("LOOP:");
        let opcodes = &mut Vec::<Opcode>::new();
        let output = line_type(opcodes, &input);
        assert_eq!(output, LineType::Label);
    }
    #[test]
    // Test for data line type
    fn test_line_type3() {
        let input = String::from("#Data_name");
        let opcodes = &mut Vec::<Opcode>::new();
        let output = line_type(opcodes, &input);
        assert_eq!(output, LineType::Data);
    }

    #[test]
    // Test for blank line type
    fn test_line_type4() {
        let input = String::default();
        let opcodes = &mut Vec::<Opcode>::new();
        let output = line_type(opcodes, &input);
        assert_eq!(output, LineType::Blank);
    }

    #[test]
    // Test for comment line type
    fn test_line_type5() {
        let input = String::from("//This is a comment");
        let opcodes = &mut Vec::<Opcode>::new();
        let output = line_type(opcodes, &input);
        assert_eq!(output, LineType::Comment);
    }

    #[test]
    // Test for start line type
    fn test_line_type6() {
        let input = String::from("_start");
        let opcodes = &mut Vec::<Opcode>::new();
        let output = line_type(opcodes, &input);
        assert_eq!(output, LineType::Start);
    }

    #[test]
    // Test for error line type
    fn test_line_type7() {
        let input = String::from("1234");
        let opcodes = &mut Vec::<Opcode>::new();
        let output = line_type(opcodes, &input);
        assert_eq!(output, LineType::Error);
    }

    #[test]
    fn test_num_data_bytes1() {
        let mut msg_list = Diagnostics::new();
        let input = String::from("#TEST 3");
        let output = num_data_bytes(&input, &mut msg_list, 0, "test");
        assert_eq!(output, 48); // 3 words × 16 hex chars per 64-bit word = 48
    }

    #[test]
    fn test_num_data_bytes2() {
        let mut msg_list = Diagnostics::new();
        let input = String::from("#TEST");
        let output = num_data_bytes(&input, &mut msg_list, 0, "test");
        assert_eq!(output, 0);
        assert_eq!(msg_list.messages()[0].text(), "Error in data definition for #TEST");
    }

    #[test]
    // Test for correct output from data line
    fn test_data_as_bytes1() {
        let input = String::from("#TEST 3");
        let output = data_as_bytes(&input);
        assert_eq!(output, Some("000000000000000000000000000000000000000000000000".to_owned()));
        // 3 × 16 hex chars per 64-bit word
    }

    #[test]
    // Test for correct output from invalid data line
    fn test_data_as_bytes2() {
        let input = String::from("#TEST");
        let output = data_as_bytes(&input);
        assert_eq!(output, None);
    }

    #[test]
    // Test for correct output from invalid data line
    fn test_data_as_bytes3() {
        let input = String::default();
        let output = data_as_bytes(&input);
        assert_eq!(output, None);
    }

    #[test]
    fn test_data_as_bytes4() {
        let input = String::from("#TEST \"Hello\"");
        let output = data_as_bytes(&input);
        // Prefix (2 character words), "Hello\0\0\0", then a NUL word so the
        // item is a whole number of 64-bit words.
        assert_eq!(output, Some("0000000248656C6C6F00000000000000".to_owned()),);
    }

    #[test]
    // Every string is a whole number of 64-bit words, whatever its length
    fn test_data_as_bytes_string_word_aligned() {
        for (text, prefix) in [("", 1), ("abc", 1), ("abcd", 2), ("Hello", 2), ("abcdefg", 2), ("abcdefgh", 3)] {
            let output = data_as_bytes(&format!("#S \"{text}\"")).unwrap();
            assert!(output.len().is_multiple_of(16), "{text:?} -> {output}");
            assert_eq!(output.get(..8), Some(format!("{prefix:08X}").as_str()), "{text:?}");
            // Characters are always followed by at least one NUL byte.
            assert!(output.len() / 2 > 4 + text.len(), "{text:?}");
        }
        // A 3-word string: prefix + 3 words = 16 bytes, no extra padding needed.
        assert_eq!(data_as_bytes("#S \"abcdefgh\"").unwrap().len(), 32);
    }

    #[test]
    fn test_data_as_bytes5() {
        let input = String::from("#TEST 0x1");
        let output = data_as_bytes(&input);
        assert_eq!(output, Some("0000000000000000".to_owned()),); // 1 × 16 hex chars per 64-bit word
    }

    #[test]
    fn test_data_as_bytes6() {
        let input = String::from("#TEST \"Hello");
        let output = data_as_bytes(&input);
        assert_eq!(output, None);
    }

    #[test]
    fn test_data_as_bytes7() {
        let input = String::from("#TEST FFFF");
        let output = data_as_bytes(&input);
        assert_eq!(output, None);
    }

    #[test]
    fn test_data_as_bytes8() {
        let input = String::from("#TEST FFFF DUMMY");
        let output = data_as_bytes(&input);
        assert_eq!(output, None);
    }

    #[test]
    // .zeropad N emits EXACTLY N bytes of zero (no 8-byte rounding), unlike .space,
    // and N must be a whole number of 32-bit words.
    fn test_data_as_bytes_zeropad() {
        assert_eq!(data_as_bytes(".zeropad 4"), Some("00000000".to_owned())); // 4 bytes
        assert_eq!(data_as_bytes(".zeropad 12"), Some("0".repeat(24))); // 12 bytes, no rounding to 8
                                                                        // A part word would misalign the board's word-at-a-time load.
        assert_eq!(data_bytes(".zeropad 2"), Err(DataError::NotWordMultiple("2".to_owned())));
        assert_eq!(data_bytes(".zeropad 3"), Err(DataError::NotWordMultiple("3".to_owned())));
        assert_eq!(data_as_bytes(".zeropad 0"), None);
        assert_eq!(data_as_bytes(".zeropad"), None);
        // Contrast with .space, which rounds up to a whole 64-bit word.
        assert_eq!(data_as_bytes(".space 4"), Some("0000000000000000".to_owned())); // 8 bytes
        assert_eq!(line_type(&Vec::new(), ".zeropad 4"), LineType::Data);
    }

    #[test]
    // Test for correct label returned
    fn test_return_label_value1() {
        let labels = &mut Vec::<Label>::new();
        labels.push(Label {
            program_counter: 42,
            name: String::from("LOOP:"),
        });
        let input = String::from("LOOP:");
        let output = return_label_value(&input, labels);
        assert_eq!(output, Some(42));
    }

    #[test]
    // Test for no label returned
    fn test_return_label_value2() {
        let labels = &mut Vec::<Label>::new();
        labels.push(Label {
            program_counter: 42,
            name: String::from("LOOP1:"),
        });
        let input = String::from("LOOP2:");
        let output = return_label_value(&input, labels);
        assert_eq!(output, None);
    }

    #[test]
    // Test parsing expected UART values from typical test file header
    fn test_parse_expected_uart_values1() {
        let lines = vec![
            "// Test 05: Bit Manipulation".to_owned(),
            "// Expected UART output:".to_owned(),
            "//   00000080  (BSET: set bit 7)".to_owned(),
            "//   000000FF  (result)".to_owned(),
            "//   FF000000  (BITREV)".to_owned(),
            "// Expected 7SEG: 0x05".to_owned(),
            "_start".to_owned(),
            "SETR A 0x0".to_owned(),
        ];
        let result = parse_expected_uart_values(&lines);
        assert_eq!(result, vec!["00000080", "000000FF", "FF000000"]);
    }

    #[test]
    // Test parsing returns empty vec when no expected values
    fn test_parse_expected_uart_values2() {
        let lines = vec!["// This is a comment".to_owned(), "// No hex values here".to_owned(), "_start".to_owned()];
        let result = parse_expected_uart_values(&lines);
        assert_eq!(result, Vec::<String>::new());
    }

    #[test]
    // Test parsing ignores non-comment lines and short hex values
    fn test_parse_expected_uart_values3() {
        let lines = vec![
            "00000080".to_owned(),                   // not a comment
            "// 0x05".to_owned(),                    // too short
            "//   ZZZZZZZZ  (invalid)".to_owned(),   // not hex
            "//   0000abcd  (lowercase)".to_owned(), // lowercase hex - should not match
            "//   0000ABCD  (uppercase)".to_owned(), // valid
        ];
        let result = parse_expected_uart_values(&lines);
        assert_eq!(result, vec!["0000ABCD"]);
    }

    #[test]
    // Test parsing with empty input
    fn test_parse_expected_uart_values4() {
        let lines: Vec<String> = Vec::new();
        let result = parse_expected_uart_values(&lines);
        assert_eq!(result, Vec::<String>::new());
    }

    #[test]
    fn test_data_as_bytes_word_hex() {
        let output = data_as_bytes(".word 0x2A");
        assert_eq!(output, Some("0000002A00000000".to_owned()));
    }

    #[test]
    fn test_data_as_bytes_word_decimal() {
        let output = data_as_bytes(".word 42");
        assert_eq!(output, Some("0000002A00000000".to_owned()));
    }

    #[test]
    fn test_data_as_bytes_word_zero() {
        let output = data_as_bytes(".word 0");
        assert_eq!(output, Some("0000000000000000".to_owned()));
    }

    #[test]
    fn test_data_as_bytes_word_negative() {
        let output = data_as_bytes(".word -1");
        assert_eq!(output, Some("FFFFFFFFFFFFFFFF".to_owned()));
    }

    #[test]
    fn test_data_as_bytes_word_missing_value() {
        let output = data_as_bytes(".word");
        assert_eq!(output, None);
    }

    #[test]
    fn test_data_as_bytes_space_4_bytes() {
        let output = data_as_bytes(".space 4");
        assert_eq!(output, Some("0000000000000000".to_owned()));
    }

    #[test]
    fn test_data_as_bytes_space_8_bytes() {
        let output = data_as_bytes(".space 8");
        assert_eq!(output, Some("0000000000000000".to_owned()));
    }

    #[test]
    fn test_data_as_bytes_space_5_bytes_rounds_up() {
        let output = data_as_bytes(".space 5");
        assert_eq!(output, Some("0000000000000000".to_owned()));
    }

    #[test]
    fn test_data_as_bytes_space_zero() {
        let output = data_as_bytes(".space 0");
        assert_eq!(output, None);
    }

    #[test]
    // Regression: .word used i64 parsing with unwrap_or(0), so these all emitted 0.
    fn test_data_bytes_word_full_range() {
        assert_eq!(data_bytes(".word 0x8000000000000000"), Ok("0000000080000000".to_owned()));
        assert_eq!(data_bytes(".word 0xFFFFFFFFFFFFFFFF"), Ok("FFFFFFFFFFFFFFFF".to_owned()));
        assert_eq!(data_bytes(".word 0x1_000"), Ok("0000100000000000".to_owned()));
        assert_eq!(data_bytes(".word -9223372036854775808"), Ok("0000000080000000".to_owned()));
    }

    #[test]
    // Regression: a malformed .word value is an error, not a silent 0.
    fn test_data_bytes_word_bad_value() {
        assert_eq!(
            data_bytes(".word 12abc"),
            Err(DataError::BadValue("12abc".to_owned(), LiteralError::InvalidDigit))
        );
        assert_eq!(
            data_bytes(".word 0x1_0000_0000_0000_0000"),
            Err(DataError::BadValue("0x1_0000_0000_0000_0000".to_owned(), LiteralError::Overflow))
        );
        assert_eq!(
            data_bytes(".word label:"),
            Err(DataError::BadValue("label:".to_owned(), LiteralError::InvalidDigit))
        );
    }

    #[test]
    // Regression: .space sizes were parsed with unwrap_or(0); negative and junk sizes are errors.
    fn test_data_bytes_space_sizes() {
        assert_eq!(data_bytes(".space 0x10"), Ok("0".repeat(32)));
        assert_eq!(data_bytes(".space 0x1_0"), Ok("0".repeat(32)));
        assert_eq!(data_bytes(".space -8"), Err(DataError::NotPositive("-8".to_owned())));
        assert_eq!(data_bytes(".space 0"), Err(DataError::NotPositive("0".to_owned())));
        assert_eq!(
            data_bytes(".space 12abc"),
            Err(DataError::BadValue("12abc".to_owned(), LiteralError::InvalidDigit))
        );
        assert_eq!(data_bytes(".space 0x100000000"), Err(DataError::TooLarge("0x100000000".to_owned())));
    }

    #[test]
    // Regression: "#name -5" produced a zero-size item; it is now an error.
    fn test_data_bytes_named_sizes() {
        assert_eq!(data_bytes("#BUF 0x1_0"), Ok("0".repeat(16 * 16)));
        assert_eq!(data_bytes("#BUF -5"), Err(DataError::NotPositive("-5".to_owned())));
        assert_eq!(
            data_bytes("#BUF 99999999999999999999"),
            Err(DataError::BadValue("99999999999999999999".to_owned(), LiteralError::Overflow))
        );
        assert_eq!(data_bytes("#BUF 0x10000000"), Err(DataError::TooLarge("0x10000000".to_owned())));
        assert_eq!(data_bytes("#BUF"), Err(DataError::Malformed));
    }

    #[test]
    // A bad data value is reported as an error at its line, with the reason.
    fn test_num_data_bytes_reports_bad_value() {
        let mut msg_list = Diagnostics::new();
        assert_eq!(num_data_bytes(".word 12abc", &mut msg_list, 7, "prog.kla"), 0);
        let msg = msg_list.messages().first().unwrap();
        assert_eq!(msg.level(), MessageType::Error);
        assert_eq!(msg.line_number(), Some(7));
        assert_eq!(
            msg.text(),
            "Error in data definition for .word 12abc: invalid value \"12abc\" (invalid digit)"
        );
    }

    #[test]
    fn test_line_type_directive_word() {
        let opcodes = &mut Vec::<Opcode>::new();
        assert_eq!(line_type(opcodes, ".word 42"), LineType::Data);
    }

    #[test]
    fn test_line_type_directive_space() {
        let opcodes = &mut Vec::<Opcode>::new();
        assert_eq!(line_type(opcodes, ".space 16"), LineType::Data);
    }

    #[test]
    fn test_line_type_directive_text() {
        let opcodes = &mut Vec::<Opcode>::new();
        assert_eq!(line_type(opcodes, ".text"), LineType::Comment);
    }

    #[test]
    fn test_line_type_directive_data() {
        let opcodes = &mut Vec::<Opcode>::new();
        assert_eq!(line_type(opcodes, ".data"), LineType::Comment);
    }

    #[test]
    fn test_line_type_directive_global() {
        let opcodes = &mut Vec::<Opcode>::new();
        assert_eq!(line_type(opcodes, ".global main"), LineType::Comment);
    }

    #[test]
    fn test_line_type_directive_comm() {
        let opcodes = &mut Vec::<Opcode>::new();
        assert_eq!(line_type(opcodes, ".comm buffer, 256, 4"), LineType::Comment);
    }

    #[test]
    fn test_line_type_directive_lcomm() {
        let opcodes = &mut Vec::<Opcode>::new();
        assert_eq!(line_type(opcodes, ".lcomm temp 8"), LineType::Comment);
    }
}
