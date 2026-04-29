use crate::files::LineType;
use crate::labels::label_name_from_string;
use crate::messages::{MessageType, MsgList};
use crate::opcodes::{disassemble_word, return_opcode, Opcode, Pass2};

/// Number of reserved words at the start of memory for the heap header.
/// Byte 0x00: heap_start (written by assembler), Byte 0x04: heap_end (written by assembler),
/// Byte 0x08: reserved, Byte 0x0C: reserved. Code begins at byte 0x10.
pub const HEAP_HEADER_WORDS: u32 = 4;
/// Find checksum.
///
/// Calculates the checksum from the string of hex values, removing control characters.
#[allow(clippy::modulo_arithmetic, reason = "Modulo arithmetic is intentional for checksum calculation")]
#[allow(clippy::arithmetic_side_effects, reason = "Arithmetic side effects are intentional in this checksum context")]
#[allow(clippy::integer_division_remainder_used, reason = "Integer division remainder is intentional for this calculation")]
pub fn calc_checksum(input_string: &str, msg_list: &mut MsgList) -> String {
    let mut stripped_string = String::default();
    let mut checksum: i32 = 0;

    // Remove S, Z and X
    for char in input_string.chars() {
        if (char != 'S') && (char != 'Z') && (char != 'X') {
            stripped_string.push(char);
        }
    }

    // check if len is divisible by 4
    if !stripped_string.len().is_multiple_of(4) {
        msg_list.push(
            {
                format!(
                    "Opcode list length not multiple of 4, length is {}",
                    stripped_string.len(),
                )
            },
            None,
            None,
            MessageType::Error,
        );
        return "0000".to_owned();
    }

    let mut position_index: u32 = 0;
    #[allow(clippy::char_indices_as_byte_indices, reason = "Using char indices as byte indices is intentional in this context de to nature of characters")]
    for (index, _) in stripped_string.chars().enumerate() {
        #[allow(clippy::integer_division_remainder_used, reason = "Integer division remainder is intentional for this calculation")]
        if index.is_multiple_of(4) {
            let int_value =
                i32::from_str_radix(stripped_string.get(index..index + 4).unwrap_or("    "), 16);
            if int_value.is_err() {
                msg_list.push(
                    {
                        format!(
                            "Error creating opcode for invalid value {}",
                            stripped_string.get(index..index + 4).unwrap_or("    "),
                        )
                    },
                    None,
                    None,
                    MessageType::Error,
                );
            } else {
                checksum = (checksum + int_value.unwrap_or(0_i32)) % (0xFFFF_i32 + 1_i32);
                position_index += 1;
            }
        }
    }
    
    checksum =
        (checksum + position_index.try_into().unwrap_or(0_i32) - 1).abs() % (0xFFFF_i32 + 1_i32);
    format!("{checksum:04X}")
}

/// Encode a 32-bit word for the kbt wire format (little-endian byte order).
///
/// The board's serial loader stores bytes in the order they arrive, so a 32-bit
/// value must be transmitted LSByte-first so the CPU reads the correct value.
/// e.g. instruction 0x400F0000 → "00000F40", entry 0x20 → "20000000".
#[must_use]
#[allow(clippy::arithmetic_side_effects, reason = "bit shifts on u32 are always safe")]
pub fn encode_word_kbt(w: u32) -> String {
    format!(
        "{:02X}{:02X}{:02X}{:02X}",
        w & 0xFF,
        (w >> 8) & 0xFF,
        (w >> 16) & 0xFF,
        (w >> 24) & 0xFF,
    )
}

/// Encode a hex opcode/data string for the kbt wire format.
///
/// Applies `encode_word_kbt` to each 8-character (32-bit word) chunk.
/// Chunks shorter than 8 characters are passed through unchanged.
#[must_use]
pub fn encode_hex_kbt(hex: &str) -> String {
    let bytes = hex.as_bytes();
    let mut result = String::with_capacity(hex.len());
    let mut i = 0;
    while i + 8 <= bytes.len() {
        #[allow(clippy::string_slice, reason = "bounds guaranteed by while condition")]
        let chunk = &hex[i..i + 8];
        let w = u32::from_str_radix(chunk, 16).unwrap_or(0);
        result.push_str(&encode_word_kbt(w));
        i += 8;
    }
    // Pass through any trailing chars that don't form a full 32-bit word
    #[allow(clippy::string_slice, reason = "i <= hex.len() by construction")]
    result.push_str(&hex[i..]);
    result
}

/// Compute the kbt checksum for a LE-encoded wire string.
///
/// For each 8-char (32-bit) chunk in the data stream the LE bytes are decoded back to
/// the natural 32-bit word V, and `V[31:16] + V[15:0]` is accumulated.  Any trailing
/// 4-char groups (test artefacts — real programs only have 32-bit words) are summed as
/// natural 16-bit values.  The checksum formula is:
///
///   `chk = (running_sum + count) % 65536`
///
/// where `count` is the number of 16-bit half-word groups accumulated (= 2 × number
/// of 32-bit words).  This matches the FPGA formula `chk = running_sum + last_addr×2`.
///
/// The result is a 32-bit word emitted in LE byte order (8 hex chars), exactly like
/// every other word in the stream.  The caller is responsible for writing the `Z`
/// delimiter; this function must be called **before** appending `Z` so that `Z` is
/// not included in the sum.
#[must_use]
#[allow(clippy::arithmetic_side_effects, reason = "sum arithmetic is safe for realistic program sizes")]
#[allow(clippy::integer_division_remainder_used, reason = "modulo is intentional for checksum wrapping")]
#[allow(clippy::modulo_arithmetic, reason = "modulo is intentional for checksum calculation")]
pub fn calc_checksum_le(input_string: &str, msg_list: &mut MsgList) -> String {
    // Strip S framing char only (Z/X are not present — caller hasn't appended them yet).
    let stripped: String = input_string.chars().filter(|&c| c != 'S').collect();

    if !stripped.len().is_multiple_of(4) {
        msg_list.push(
            format!("LE checksum string length not multiple of 4, length is {}", stripped.len()),
            None, None, MessageType::Error,
        );
        return encode_word_kbt(0);
    }

    let mut sum: i32 = 0;
    let mut count: u32 = 0;
    let mut i: usize = 0;

    // Process 8-char groups as LE-encoded 32-bit words.
    // Decode each word to its natural value V, accumulate V[31:16] + V[15:0].
    while i + 8 <= stripped.len() {
        #[allow(clippy::string_slice, reason = "bounds guaranteed by while condition")]
        let chunk = &stripped[i..i + 8];
        let b0 = i32::from_str_radix(&chunk[0..2], 16).unwrap_or(0);
        let b1 = i32::from_str_radix(&chunk[2..4], 16).unwrap_or(0);
        let b2 = i32::from_str_radix(&chunk[4..6], 16).unwrap_or(0);
        let b3 = i32::from_str_radix(&chunk[6..8], 16).unwrap_or(0);
        // natural V = b0 | b1<<8 | b2<<16 | b3<<24
        // V[31:16] = b3*256 + b2,   V[15:0] = b1*256 + b0
        sum = (sum + b3 * 256 + b2 + b1 * 256 + b0) % (0xFFFF_i32 + 1);
        count += 2;
        i += 8;
    }
    // Any trailing 4-char groups (test-only artefacts) summed as natural 16-bit.
    while i + 4 <= stripped.len() {
        #[allow(clippy::string_slice, reason = "bounds guaranteed by while condition")]
        let chunk = &stripped[i..i + 4];
        let val = i32::from_str_radix(chunk, 16).unwrap_or(0);
        sum = (sum + val) % (0xFFFF_i32 + 1);
        count += 1;
        i += 4;
    }

    // chk = running_sum + last_addr×2 (FPGA formula, adj=0)
    let chk = ((sum + count as i32) % (0xFFFF_i32 + 1)) as u32;
    encode_word_kbt(chk)
}

/// Return String of bit codes with start/stop bytes and CRC.
///
/// Based on the Pass2 vector, create the bitcode, calculating the checksum, and adding control characters.
/// Currently only ever sets the stack to 16 bytes (Z0010).
#[allow(clippy::or_fun_call, reason = "Needed for simplicity of setting up default start address")]
pub fn create_bin_string(pass2: &[Pass2], msg_list: &mut MsgList) -> Option<String> {
    let mut output_string = String::default();

    output_string.push('S'); // Start character

    // Word 0: heap_start placeholder — patched below once total program size is known
    let heap_start_offset = output_string.len();
    output_string.push_str("0000000000000000");

    // Words 1-3: reserved header words (heap_end, reserved, reserved)
    for _ in 1..HEAP_HEADER_WORDS {
        output_string.push_str("0000000000000000");
    }

    for pass in pass2 {
        output_string.push_str(&encode_hex_kbt(&pass.opcode));
    }

    // heap_start = first free byte after the program, rounded up to 8-byte alignment.
    // All heap block headers are 3×8=24 bytes, so every data area is 8-byte aligned
    // when heap_start itself is 8-byte aligned — required for MEMSET8→MEMGET64 coherency.
    // output_string is 'S' + hex_chars; hex_chars/2 = bytes (valid for both 32-bit opcodes
    // and 64-bit data words since both maintain a 2:1 hex-char to byte ratio)
    #[allow(clippy::arithmetic_side_effects, reason = "Subtraction safe: string starts with 'S' so len >= 1")]
    #[allow(clippy::integer_division, reason = "Integer division intentional: hex chars → bytes")]
    let heap_start_raw: u32 = ((output_string.len() - 1) / 2) as u32;
    #[allow(clippy::arithmetic_side_effects, reason = "Addition safe: heap_start_raw + 7 cannot overflow for realistic program sizes")]
    let heap_start: u32 = (heap_start_raw + 7) & !7u32; // align to 8-byte boundary
    // Emit heap_start as lo32 then hi32, each word encoded for LE board loading.
    // hi32 is always zero; encode_word_kbt(0) = "00000000" so only lo32 needs encoding.
    #[allow(clippy::string_slice, reason = "Slice bounds are fixed and known safe")]
    output_string.replace_range(
        heap_start_offset..heap_start_offset + 16,
        &format!("{}00000000", encode_word_kbt(heap_start)),
    );

    if pass2
        .iter()
        .filter(|x| x.line_type == LineType::Start)
        .count()
        == 1
    {
        let entry_pc = pass2
            .iter()
            .find(|x| x.line_type == LineType::Start)
            .unwrap_or(&Pass2 {
                line_type: LineType::Start,
                opcode: String::default(),
                program_counter: 0,
                line_counter: 0,
                input_text_line: String::default(),
                file_name: "None".to_owned(),
            })
            .program_counter;
        output_string.push_str(&encode_word_kbt(entry_pc));
    } else if pass2
        .iter()
        .filter(|x| x.line_type == LineType::Start)
        .count()
        == 0
    {
        msg_list.push(
            "No start address found".to_owned(),
            None,
            None,
            MessageType::Error,
        );
        return None;
    } else {
        msg_list.push(
            "Multiple start addresses found".to_owned(),
            None,
            None,
            MessageType::Error,
        );
        return None;
    }

    // Checksum must be computed before the Z delimiter is appended.
    // Returns an 8-char LE-encoded 32-bit word.
    let checksum = calc_checksum_le(&output_string, msg_list);

    output_string.push('Z');
    output_string.push_str(&checksum);
    output_string.push('X'); // Stop character

    Some(output_string)
}

/// Disassemble a flat byte slice into a `Vec<Pass2>` for use with `write_code_output_file`.
///
/// Each 4-byte chunk is read as a big-endian 32-bit word (matching the KlaussCPU LLVM ELF
/// byte order) and matched against `opcodes`.  When a match is found, register operands are
/// extracted and any argument words that follow (determined by `opcode.variables`) are consumed
/// and appended to the same entry's `opcode` hex string so `write_code_output_file` formats them
/// correctly.  Unrecognised words produce a `???` fallback line.
///
/// `base_addr` is the board byte address of the first byte in `binary` (normally
/// `HEAP_HEADER_WORDS * 8 = 0x20`).
#[allow(clippy::arithmetic_side_effects, reason = "index arithmetic is bounds-checked by the while condition")]
#[allow(clippy::cast_possible_truncation, reason = "binary length fits in u32 for any realistic program")]
pub fn disassemble_flat_to_pass2(binary: &[u8], base_addr: u32, opcodes: &[Opcode]) -> Vec<Pass2> {
    let mut result: Vec<Pass2> = Vec::new();
    let mut i: usize = 0;

    while i + 4 <= binary.len() {
        let word = u32::from_le_bytes([binary[i], binary[i + 1], binary[i + 2], binary[i + 3]]);
        let pc = base_addr.saturating_add(i as u32);

        let (base_text, vars) = disassemble_word(word, opcodes)
            .map_or_else(|| (format!("??? (0x{word:08X})"), 0_u32), |(t, v)| (t, v));

        let mut opcode_hex = format!("{word:08X}");
        let mut display_text = base_text;

        // Consume argument words that immediately follow the instruction word.
        // vars == 1 → one 32-bit immediate at PC+4.
        // vars == 2 → two 32-bit words at PC+4 (lo32) and PC+8 (hi32), forming a 64-bit value.
        let arg_word_count = vars.min(2) as usize;
        let mut arg_ok = true;
        for arg_idx in 0..arg_word_count {
            let off = i + 4 + arg_idx * 4;
            if off + 4 <= binary.len() {
                let av = u32::from_le_bytes([binary[off], binary[off + 1], binary[off + 2], binary[off + 3]]);
                opcode_hex.push_str(&format!("{av:08X}"));
                if arg_idx == 0 && arg_word_count == 1 {
                    display_text.push_str(&format!(" 0x{av:X}"));
                } else if arg_idx == 0 {
                    // 64-bit: lo32 stored first — stash it, combine after reading hi32
                    display_text.push_str(&format!(" 0x{av:08X}"));
                } else {
                    // arg_idx == 1: hi32 — replace the lo32 placeholder with combined value
                    // The display_text already ends with " 0x{lo32:08X}"; replace with 64-bit
                    let lo_str_len = " 0x".len() + 8; // " 0x" + 8 hex chars
                    let lo = u64::from_str_radix(
                        display_text.get(display_text.len() - 8..).unwrap_or("0"),
                        16,
                    ).unwrap_or(0);
                    display_text.truncate(display_text.len() - lo_str_len);
                    let val64 = (u64::from(av) << 32) | lo;
                    display_text.push_str(&format!(" 0x{val64:X}"));
                }
            } else {
                arg_ok = false;
                break;
            }
        }

        let words_consumed = if arg_ok { 1 + arg_word_count } else { 1 };

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

/// Returns bytes for data element.
///
/// Parses data element and returns data as bytes, or None if error.
pub fn data_as_bytes(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");
    if first_word.is_empty() {
        return None;
    }

    // Handle .word VALUE directive — emit a single 64-bit word
    if first_word == ".word" {
        let value_str = words.next().unwrap_or("");
        if value_str.is_empty() {
            return None;
        }
        let value: i64 = if value_str.len() >= 2
            && (value_str.get(0..2).unwrap_or("  ") == "0x"
                || value_str.get(0..2).unwrap_or("  ") == "0X")
        {
            let without_prefix = value_str.trim_start_matches("0x").trim_start_matches("0X");
            i64::from_str_radix(without_prefix, 16).unwrap_or(0)
        } else {
            value_str.parse::<i64>().unwrap_or(0)
        };
        #[allow(clippy::cast_sign_loss, reason = "Sign loss is intentional for u64 hex representation")]
        let v64 = value as u64;
        let lo32 = (v64 & 0xFFFF_FFFF) as u32;
        let hi32 = ((v64 >> 32) & 0xFFFF_FFFF) as u32;
        return Some(format!("{lo32:08X}{hi32:08X}"));
    }

    // Handle .space N directive — N bytes of zero, rounded up to 64-bit word boundary
    if first_word == ".space" {
        let count_str = words.next().unwrap_or("");
        if count_str.is_empty() {
            return None;
        }
        let byte_count: i64 = if count_str.len() >= 2
            && (count_str.get(0..2).unwrap_or("  ") == "0x"
                || count_str.get(0..2).unwrap_or("  ") == "0X")
        {
            let without_prefix = count_str.trim_start_matches("0x").trim_start_matches("0X");
            i64::from_str_radix(without_prefix, 16).unwrap_or(0)
        } else {
            count_str.parse::<i64>().unwrap_or(0)
        };
        if byte_count <= 0 {
            return None;
        }
        #[allow(clippy::integer_division, reason = "Integer division is intentional for word count calculation")]
        #[allow(clippy::arithmetic_side_effects, reason = "Arithmetic is intentional for word count rounding")]
        #[allow(clippy::integer_division_remainder_used, reason = "Division remainder is intentional for rounding")]
        let word_count = (byte_count + 7) / 8;
        let mut data = String::default();
        for _ in 0..word_count {
            data.push_str("0000000000000000");
        }
        return Some(data);
    }

    let second_word = words.next().unwrap_or("");
    if second_word.is_empty() {
        return None;
    }

    // Check if next word starts with quote
    if second_word.starts_with('\"') {
        let remaining_line = line.trim_start_matches(first_word).trim();

        if remaining_line.starts_with('\"') && remaining_line.ends_with('\"') {
            let input_string = remaining_line.trim_matches('\"').replace("\\n", "\r\n");
            let mut output_hex = String::default();
            // Length is based on multiples of 4
            #[allow(clippy::integer_division, reason = "Integer division is intentional for string length calculation")]
            #[allow(clippy::arithmetic_side_effects, reason = "Arithmetic side effects are intentional for string length calculation")]
            #[allow(clippy::integer_division_remainder_used, reason = "Integer division remainder is intentional for string length calculation")]  
            output_hex.push_str(
                format!(
                    "{:08X}",
                    (input_string.len() + 4 - input_string.len() % 4) / 4
                )
                .as_str(),
            ); // Add length of string to start
            for char in input_string.as_bytes() {
                let hex = format!("{char:02X}");
                output_hex.push_str(&hex);
            }
            #[allow(clippy::arithmetic_side_effects, reason = "Arithmetic side effects are intentional for string length calculation")]
            #[allow(clippy::integer_division_remainder_used, reason = "Integer division remainder is intentional for string length calculation")]
            let needed_bytes = 8 - (output_hex.len() % 8);
            for _n in 0..needed_bytes {
                output_hex.push('0');
            }
            return Some(output_hex);
        }
        None
    } else {
        // Check if next word is a number
        // let int_value: i64;
        let int_value = if second_word.len() >= 2
            && (second_word.get(0..2).unwrap_or("  ") == "0x"
                || second_word.get(0..2).unwrap_or("  ") == "0X")
        {
            let without_prefix1 = second_word.trim_start_matches("0x");
            let without_prefix2 = without_prefix1.trim_start_matches("0X");
            let int_value_result = i64::from_str_radix(&without_prefix2.replace('_', ""), 16);
            int_value_result.unwrap_or(0)
        } else {
            let int_value_result = second_word.parse::<i64>();
            int_value_result.unwrap_or(0)
        };

        if int_value == 0 {
            None
        } else {
            let mut data = String::default();
            for _ in 0..int_value {
                data.push_str("0000000000000000");
            }
            Some(data)
        }
    }
}

/// Extracts data name from string.
///
/// Checks if start of first word is hash if so return data name as option string.
pub fn data_name_from_string(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let first_word = words.next().unwrap_or("");
    if first_word.starts_with('#') {
        return Some(first_word.to_owned());
    }
    None
}

/// Check if line is blank.
///
/// Returns true if line if just whitespace.
pub fn is_blank(line: &str) -> bool {
    let words = line.split_whitespace();

    for word in words {
        if !word.is_empty() {
            return false;
        }
    }
    true
}

/// Check if line is comment.
///
/// Returns true if line if just comment.
pub fn is_comment(line: &str) -> bool {
    let word = line.trim();
    if word.len() < 2 {
        return false;
    }

    let bytes = word.as_bytes();
    let mut found_first = false;

    for (i, &item) in bytes.iter().enumerate() {
        if item == b'/' && i == 0 {
            found_first = true;
        }
        if item == b'/' && i == 1 && found_first {
            return true;
        }
    }
    false
}

/// Check if line is start.
///
/// Returns true if line is start.
pub fn is_start(line: &str) -> bool {
    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        if i == 0 && word.to_uppercase() == "_START" {
            return true;
        }
    }
    false
}

/// Check if line is valid.
///
/// Returns true if line is not error.
pub fn is_valid_line(opcodes: &mut Vec<Opcode>, line: String) -> bool {
    let temp_line: String = line;
    if line_type(opcodes, &temp_line) == LineType::Error {
        return false;
    }
    true
}

/// Returns enum of type of line.
///
/// Given a code line, will returns if line is Label, Opcode, Blank, Comment or Error.
pub fn line_type(opcodes: &mut Vec<Opcode>, line: &str) -> LineType {
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
        ".word" | ".space" => return LineType::Data,
        ".text" | ".data" | ".rodata" | ".bss" | ".global" | ".globl" | ".extern" | ".comm" | ".lcomm" => return LineType::Comment,
        _ => {}
    }
    LineType::Error
}

/// Return number of bytes of data.
///
/// From instruction name, option of number of bytes of data, or 0 is error.
pub fn num_data_bytes(
    line: &str,
    msg_list: &mut MsgList,
    line_number: u32,
    filename: String,
) -> u32 {
    data_as_bytes(line).map_or_else(
        || {
            msg_list.push(
                format!("Error in data definition for {line}"),
                Some(line_number),
                Some(filename),
                MessageType::Error,
            );
            0
        },
        |data| data.len().try_into().unwrap_or_default(),
    )
}

/// Returns trailing comments.
///
/// Removes comments and starting and training whitespace.
#[allow(clippy::arithmetic_side_effects, reason = "Arithmetic side effects are intentional for comment extraction")]
pub fn return_comments(input: &str) -> String {
    input.find("//").map_or_else(String::default, |location| input.get(location + 2..).unwrap_or("").trim().to_owned())
}

/// Strip trailing comments.
///
/// Removes comments and starting and training whitespace.
pub fn strip_comments(input: &str) -> String {
    input.find("//").map_or_else(
        || input.trim().to_owned(),
        |location| input.get(0..location).unwrap_or("").trim().to_owned(),
    )
}


/// Parse expected UART hex values from source file comment headers.
///
/// Extracts 8-digit uppercase hex values from lines matching the pattern `//   XXXXXXXX  (`.
#[allow(clippy::arithmetic_side_effects, reason = "Slice indexing is safe after length check")]
#[allow(clippy::min_ident_chars, reason = "Single-char closure arg is idiomatic for simple predicates")]
pub fn parse_expected_uart_values(lines: &[String]) -> Vec<String> {
    let mut expected: Vec<String> = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if !trimmed.starts_with("//") {
            continue;
        }
        let comment_body = trimmed.get(2..).unwrap_or("").trim();
        if comment_body.len() >= 8 {
            let candidate = comment_body.get(..8).unwrap_or("");
            if candidate.len() == 8
                && candidate.chars().all(|c| c.is_ascii_hexdigit())
                && candidate == candidate.to_ascii_uppercase()
            {
                expected.push(candidate.to_owned());
            }
        }
    }
    expected
}

/// Trim newline from string.
///
/// Removes newline from end of string.
pub fn trim_newline(input: &mut String) {
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
#[allow(clippy::arbitrary_source_item_ordering, reason = "Test functions can be in any order")]
mod tests {
    use super::*;
    use crate::labels::{return_label_value, Label};

    #[test]
    // Test that correct checksum is calculated
    fn test_calc_checksum1() {
        let mut msg_list = MsgList::new();
        let checksum = calc_checksum("S0000Z0010", &mut msg_list);
        assert_eq!(checksum, "0011");
    }
    #[test]
    // Test for invalid length
    fn test_calc_checksum2() {
        let mut msg_list = MsgList::new();
        let checksum = calc_checksum("S00001Z0010", &mut msg_list);
        assert_eq!(checksum, "0000");
        assert_eq!(
            msg_list.list.first().unwrap_or_default().text,
            "Opcode list length not multiple of 4, length is 9"
        );
    }

    #[test]
    // Test that correct checksum is calculated
    fn test_calc_checksum3() {
        let mut msg_list = MsgList::new();
        let checksum = calc_checksum("S00000000Z0010", &mut msg_list);
        assert_eq!(checksum, "0012");
    }

    #[test]
    // Test that correct checksum is calculated
    fn test_calc_checksum4() {
        let mut msg_list = MsgList::new();
        let checksum = calc_checksum("S00009999Z0010", &mut msg_list);
        assert_eq!(checksum, "99AB");
    }

    #[test]
    // Test that correct checksum is calculated
    fn test_calc_checksum5() {
        let mut msg_list = MsgList::new();
        let checksum = calc_checksum("____", &mut msg_list);
        assert_eq!(checksum, "0001");
        assert_eq!(
            msg_list.list.first().unwrap_or_default().text,
            "Error creating opcode for invalid value ____"
        );
    }

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
        assert_eq!(bin_string, Some("S28000000000000000000000000000000000000000000000000000000000000001234432101000000Z92550000X".to_owned()));
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
        assert_eq!(
            msg_list.list.first().unwrap_or_default().text,
            "Multiple start addresses found"
        );
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
        assert_eq!(
            msg_list.list.first().unwrap_or_default().text,
            "No start address found"
        );
    }

    #[test]
    // Test that comment is stripped
    fn test_strip_comments() {
        assert_eq!(
            strip_comments("Hello, world! //This is a comment"),
            "Hello, world!"
        );
        assert_eq!(strip_comments("Hello, world! //"), "Hello, world!");
        assert_eq!(strip_comments(""), "");
    }

    #[test]
    // Test that comment is returned
    fn test_return_comments() {
        assert_eq!(
            return_comments("Hello, world! //This is a comment"),
            "This is a comment"
        );
        assert_eq!(return_comments("Hello, world! //"), "");
        assert_eq!(return_comments("Hello, world!"), "");
    }

    #[test]
    // Test true is returned for comment
    fn test_is_comment1() {
        assert!(is_comment("//This is a comment"));
        assert!(is_comment("      //This is a comment"));
    }

    #[test]
    // Test false is returned for non-comment
    fn test_is_comment2() {
        assert!(!is_comment("Hello //This is a comment"));
        assert!(!is_comment(" "));
    }

    #[test]
    // Test for blank line returns true
    fn test_is_blank1() {
        assert!(is_blank(" "));
        assert!(is_blank(""));
    }

    #[test]
    // Test for non blank line returns false
    fn test_is_blank2() {
        assert!(!is_blank("1234"));
        assert!(!is_blank("    1234"));
    }

    #[test]
    // Test for valid line returns true is opcode is found
    fn test_is_valid_line1() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            comment: String::default(),
            variables: 0,
            registers: 0,
            section: String::default(),
        });
        let output = is_valid_line(opcodes, input);
        assert!(output);
    }

    #[test]
    fn test_is_valid_line2() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PULL"),
            hex_code: String::from("1234"),
            comment: String::default(),
            variables: 0,
            registers: 0,
            section: String::default(),
        });
        let output = is_valid_line(opcodes, input);
        assert!(!output);
    }

    #[test]
    // Test for opcode line type
    fn test_line_type1() {
        let input = String::from("PUSH");
        let mut opcodes = Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            comment: String::default(),
            variables: 0,
            registers: 0,
            section: String::default(),
        });
        let output = line_type(&mut opcodes, &input);
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
        let mut msg_list = MsgList::new();
        let input = String::from("#TEST 3");
        let output = num_data_bytes(&input, &mut msg_list, 0, "test".to_owned());
        assert_eq!(output, 48); // 3 words × 16 hex chars per 64-bit word = 48
    }

    #[test]
    fn test_num_data_bytes2() {
        let mut msg_list = MsgList::new();
        let input = String::from("#TEST");
        let output = num_data_bytes(&input, &mut msg_list, 0, "test".to_owned());
        assert_eq!(output, 0);
        assert_eq!(
            msg_list.list.first().unwrap_or_default().text,
            "Error in data definition for #TEST"
        );
    }

    #[test]
    // Test for correct output from data line
    fn test_data_as_bytes1() {
        let input = String::from("#TEST 3");
        let output = data_as_bytes(&input);
        assert_eq!(output, Some("000000000000000000000000000000000000000000000000".to_owned())); // 3 × 16 hex chars per 64-bit word
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
        assert_eq!(output, Some("0000000248656C6C6F000000".to_owned()),);
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
    // Test for correct label name
    fn test_label_name_from_string1() {
        let input = String::from("LOOP:");
        let output = label_name_from_string(&input);
        assert_eq!(output, Some("LOOP:".to_owned()));
    }

    #[test]
    // Test for invalid label name
    fn test_label_name_from_string2() {
        let input = String::from("LOOP");
        let output = label_name_from_string(&input);
        assert_eq!(output, None);
    }

    #[test]
    // Test for correct data name
    fn test_data_name_from_string1() {
        let input = String::from("#TEST");
        let output = data_name_from_string(&input);
        assert_eq!(output, Some("#TEST".to_owned()));
    }

    #[test]
    // Test for invalid data name
    fn test_data_name_from_string2() {
        let input = String::from("TEST");
        let output = data_name_from_string(&input);
        assert_eq!(output, None);
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
        let lines = vec![
            "// This is a comment".to_owned(),
            "// No hex values here".to_owned(),
            "_start".to_owned(),
        ];
        let result = parse_expected_uart_values(&lines);
        assert!(result.is_empty());
    }

    #[test]
    // Test parsing ignores non-comment lines and short hex values
    fn test_parse_expected_uart_values3() {
        let lines = vec![
            "00000080".to_owned(),              // not a comment
            "// 0x05".to_owned(),               // too short
            "//   ZZZZZZZZ  (invalid)".to_owned(), // not hex
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
        assert!(result.is_empty());
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
