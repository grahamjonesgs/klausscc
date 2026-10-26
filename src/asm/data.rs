//! Data definitions: `#name` strings and reserved words, and the `.word`,
//! `.space` and `.zeropad` directives, turned into the bytes they emit.

use crate::diag::Diagnostics;
use crate::num::{LiteralError, parse_bits64, parse_int};
use std::fmt;
use std::fmt::Write as _;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::classify::line_type;
    use crate::asm::line::LineType;
    use crate::diag::MessageType;

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
}
