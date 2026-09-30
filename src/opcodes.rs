use crate::files::LineType;
use crate::labels::{convert_argument, Label};
use crate::macros::{macro_from_string, return_macro, Macro};
use crate::messages::{MessageType, MsgList};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq, Eq)]
/// Struct for opcode argument.
pub struct InputData {
    /// File name of input file.
    pub file_name: String,
    /// Text name of opcode.
    pub input: String,
    /// Line number of input file.
    pub line_counter: u32,
}

/// One source-operand slot of a v2 instruction, describing how the operand
/// token maps onto the instruction word (`ISA_ENCODING_V2.md` §1). The uniform
/// v2 layout is `word0 = template | rd<<8 | rs1<<4 | rs2` plus `N<<15` for the
/// embedded shift/bit count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operand {
    /// Destination register → word0[11:8].
    Rd,
    /// First source / base register → word0[7:4].
    Rs1,
    /// Second source / offset register → word0[3:0].
    Rs2,
    /// In-place register: one source token drives BOTH rd[11:8] and rs1[7:4].
    /// v1's in-place forms (`ADDV A 5`, `NEGR A`, `SHLV A 4`) become v2's
    /// 3-operand encodings with `rd == rs1`.
    RdRs1,
    /// Embedded 6-bit shift/bit count or position → word0[20:15].
    Count,
    /// 32-bit immediate word appended at PC+4.
    Imm,
    /// 64-bit immediate appended as lo32@PC+4, hi32@PC+8.
    Imm64,
    /// ISA v3 short form: literal 8-bit immediate → word0[19:12]
    /// (-128..=255; the instruction's SGN bit picks the extension).
    Imm8,
    /// ISA v3 short load/store: literal BYTE offset, a multiple of
    /// `1 << shift` whose scaled value fits simm8 → word0[19:12].
    Off8(u8),
    /// ISA v3 `ENTER`: literal frame size in 8-byte units → word0[21:0].
    Frame22,
    /// ISA v3 fused branch: literal simm4 compare immediate → word0[3:0].
    Simm4,
    /// ISA v3 short branch target (label or address) → simm18 word
    /// displacement from this instruction, word0[17:0]. Filled in pass 2.
    Rel18,
    /// ISA v3 fused branch target (label or address) → simm13 word
    /// displacement from this instruction, word0[20:8]. Filled in pass 2.
    Rel13,
}

/// Parse a signed literal: decimal (optionally negative) or `0x` hex.
fn parse_simm(token: &str) -> Option<i64> {
    let (neg, t) = token.strip_prefix('-').map_or((false, token), |r| (true, r));
    let v = if t.len() >= 2 && t.get(..2).is_some_and(|s| s.eq_ignore_ascii_case("0x")) {
        i64::from_str_radix(&t[2..].replace('_', ""), 16).ok()?
    } else {
        t.parse::<i64>().ok()?
    };
    Some(if neg { -v } else { v })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Struct for opcode.
pub struct Opcode {
    /// Comment from opcode definition file.
    pub comment: String,
    /// Hexadecimal opcode template (word 0, register/count fields = 0).
    pub hex_code: String,
    /// Number of register/count source operands (tokens consumed into word 0).
    pub registers: u32,
    /// Section name from opcode definition file.
    pub section: String,
    /// Text name of opcode.
    pub text_name: String,
    /// Number of appended immediate words (imm32 → 1, imm64 → 2).
    pub variables: u32,
    /// Ordered source-operand layout (empty for legacy `.vh`-parsed opcodes).
    #[serde(default)]
    pub ops: Vec<Operand>,
}

#[cfg(not(tarpaulin_include))]
impl Default for &InputData {
    #[inline]
    fn default() -> &'static InputData {
        static VALUE: InputData = InputData {
            input: String::new(),
            file_name: String::new(),
            line_counter: 0,
        };
        &VALUE
    }
}

#[derive(Debug)]
/// Struct for Pass0.
pub struct Pass0 {
    /// File name of input file.
    pub file_name: String,
    /// Line text.
    pub input_text_line: String,
    /// Line number of input file.
    pub line_counter: u32,
}

#[cfg(not(tarpaulin_include))]
impl Default for &Pass0 {
    #[inline]
    fn default() -> &'static Pass0 {
        static VALUE: Pass0 = Pass0 {
            input_text_line: String::new(),
            file_name: String::new(),
            line_counter: 0,
        };
        &VALUE
    }
}

#[derive(Debug)]
/// Struct for Pass1.
pub struct Pass1 {
    /// File name of input file.
    pub file_name: String,
    /// Line text.
    pub input_text_line: String,
    /// Line number of input file.
    pub line_counter: u32,
    /// Line type.
    pub line_type: LineType,
    /// Program counter.
    pub program_counter: u32,
}

#[cfg(not(tarpaulin_include))]
impl Default for &Pass1 {
    #[inline]
    fn default() -> &'static Pass1 {
        static VALUE: Pass1 = Pass1 {
            input_text_line: String::new(),
            file_name: String::new(),
            line_counter: 0,
            program_counter: 0,
            line_type: LineType::Blank,
        };
        &VALUE
    }
}

#[derive(Debug, Clone)]
/// Struct for Pass2.
pub struct Pass2 {
    /// File name of input file.
    pub file_name: String,
    /// Line text.
    pub input_text_line: String,
    /// Line number of input file.
    pub line_counter: u32,
    /// Line type.
    pub line_type: LineType,
    /// Opcode as string.
    pub opcode: String,
    /// Program counter.
    pub program_counter: u32,
}

#[cfg(not(tarpaulin_include))]
impl Default for &Pass2 {
    #[inline]
    fn default() -> &'static Pass2 {
        static VALUE: Pass2 = Pass2 {
            input_text_line: String::new(),
            file_name: String::new(),
            line_counter: 0,
            program_counter: 0,
            line_type: LineType::Blank,
            opcode: String::new(),
        };
        &VALUE
    }
}

/// Parse a single token as a u64 immediate value.
///
/// Handles `0x`/`0X` hex prefix (full 64-bit range) and signed decimal.
/// Returns `None` if the token cannot be parsed (e.g. a label name).
fn parse_imm64(token: &str) -> Option<u64> {
    if token.len() >= 2 && token.get(..2).is_some_and(|s| s.eq_ignore_ascii_case("0x")) {
        u64::from_str_radix(&token[2..].replace('_', ""), 16).ok()
    } else {
        token.parse::<i64>().ok().map(|v| v as u64)
    }
}

/// Return opcode with formatted arguments.
///
/// Returns the hex code argument from the line, converting arguments from decimal to 8 digit hex values.
/// Converts label names to hex addresses.
pub fn add_arguments(
    opcodes: &mut Vec<Opcode>,
    line: &String,
    msg_list: &mut MsgList,
    line_number: u32,
    filename: &str,
    labels: &mut Vec<Label>,
) -> String {
    let num_registers = num_registers(opcodes, &line.to_uppercase()).unwrap_or(0);
    let num_arguments = num_arguments(opcodes, &line.to_uppercase()).unwrap_or(0);
    let mut arguments = String::default();

    // Special case: 2-variable instruction with a single 64-bit literal (e.g. SETR64).
    // When exactly one value token follows the register(s), interpret it as a 64-bit immediate
    // and split into lo32 (var1 at PC+4) and hi32 (var2 at PC+8).
    if num_arguments == 2 {
        let words_vec: Vec<&str> = line.split_whitespace().collect();
        let val_idx = num_registers as usize + 1;
        if words_vec.len() == val_idx + 1 {
            if let Some(val64) = parse_imm64(words_vec[val_idx]) {
                let lo32 = (val64 & 0xFFFF_FFFF) as u32;
                let hi32 = ((val64 >> 32) & 0xFFFF_FFFF) as u32;
                return format!("{lo32:08X}{hi32:08X}");
            }
        }
    }

    let words = line.split_whitespace();
    for (i, word) in words.enumerate() {
        if (i == num_registers as usize + 1) && ((num_arguments == 1) || (num_arguments == 2)) {
            arguments.push_str(&{
                let this = convert_argument(&word.to_owned().to_uppercase(), msg_list, line_number, filename.to_owned(), labels);
                //let default = "00000000".to_owned();
                this.unwrap_or_else(|| "00000000".to_owned())
            });
        }
        if i == num_registers as usize + 2 && num_arguments == 2 {
            arguments.push_str(&{
                let this = convert_argument(&word.to_owned().to_uppercase(), msg_list, line_number, filename.to_owned(), labels);
                this.unwrap_or_else(|| "00000000".to_owned())
            });
        }
        if i > num_registers as usize + num_arguments as usize {
            msg_list.push(
                format!("Too many arguments found - \"{line}\""),
                Some(line_number),
                Some((filename).to_owned()),
                MessageType::Warning,
            );
        }
    }

    // Can't be in tarpaulin as we can't test the error by passing wrong size
    if arguments.len() != 8 * num_arguments as usize {
        #[cfg(not(tarpaulin_include))]
        msg_list.push(
            format!("Incorrect argument definition - \"{line}\""),
            Some(line_number),
            Some(filename.to_owned()),
            MessageType::Error,
        );
    }
    arguments
}

/// Parse a shift/bit count token (embedded `N` field): decimal or `0x` hex.
fn parse_count(token: &str) -> Option<u32> {
    if token.len() >= 2 && token.get(..2).is_some_and(|s| s.eq_ignore_ascii_case("0x")) {
        u32::from_str_radix(&token[2..].replace('_', ""), 16).ok()
    } else {
        token.parse::<u32>().ok()
    }
}

/// Find the opcode whose `text_name` matches the first word of `line`.
fn return_opcode_struct<'a>(line: &str, opcodes: &'a [Opcode]) -> Option<&'a Opcode> {
    let first = line.split_whitespace().next().unwrap_or("").to_uppercase();
    opcodes.iter().find(|o| o.text_name == first)
}

/// Updates opcode with register.
///
/// Builds instruction word 0 (as an 8-hex string) from the opcode template and
/// the register/count operands on the line.  For v2 opcodes (those carrying an
/// [`Operand`] layout) the register fields are packed numerically per
/// `ISA_ENCODING_V2.md` (`rd<<8 | rs1<<4 | rs2`, in-place `rd==rs1`, embedded
/// `N<<15`).  Opcodes without a layout fall back to the legacy trailing-nibble
/// scheme (used by `.vh`-parsed tables).
#[allow(clippy::ptr_arg, reason = "shares the &mut Vec<Opcode> signature convention with add_arguments / num_registers")]
pub fn add_registers(opcodes: &mut Vec<Opcode>, line: &String, filename: String, msg_list: &mut MsgList, line_number: u32) -> String {
    let Some(opcode) = return_opcode_struct(&line.to_uppercase(), opcodes).cloned() else {
        msg_list.push(
            format!("Opcode not found - \"{line}\""),
            Some(line_number),
            Some(filename),
            MessageType::Error,
        );
        return "ERR     ".to_owned();
    };

    // Legacy path: opcodes with no v2 operand layout keep the trailing-nibble fill.
    if opcode.ops.is_empty() {
        return legacy_add_registers(&opcode, line, &filename, msg_list, line_number);
    }

    let Ok(mut word0) = u32::from_str_radix(&opcode.hex_code, 16) else {
        msg_list.push(
            format!("Incorrect opcode template - \"{line}\""),
            Some(line_number),
            Some(filename),
            MessageType::Error,
        );
        return "ERR     ".to_owned();
    };

    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut token_idx = 1_usize; // first operand token (after the mnemonic)
    let mut ok = true;
    for op in &opcode.ops {
        match op {
            Operand::Imm | Operand::Imm64 => {} // appended words — handled by add_arguments
            Operand::Imm8 => {
                match tokens.get(token_idx).and_then(|t| parse_simm(t)) {
                    Some(v) if (-128..=255).contains(&v) => word0 |= ((v as u32) & 0xFF) << 12,
                    _ => ok = false,
                }
                token_idx += 1;
            }
            Operand::Off8(shift) => {
                let scale = 1_i64 << shift;
                match tokens.get(token_idx).and_then(|t| parse_simm(t)) {
                    Some(v) if v % scale == 0 && (-128..=127).contains(&(v / scale)) => {
                        word0 |= (((v / scale) as u32) & 0xFF) << 12;
                    }
                    _ => ok = false,
                }
                token_idx += 1;
            }
            Operand::Frame22 => {
                match tokens.get(token_idx).and_then(|t| parse_simm(t)) {
                    Some(v) if (0..(1 << 22)).contains(&v) => word0 |= v as u32,
                    _ => ok = false,
                }
                token_idx += 1;
            }
            Operand::Simm4 => {
                match tokens.get(token_idx).and_then(|t| parse_simm(t)) {
                    Some(v) if (-8..=7).contains(&v) => word0 |= (v as u32) & 0xF,
                    _ => ok = false,
                }
                token_idx += 1;
            }
            // PC-relative targets need this instruction's address: pass 2
            // ORs them in via `pc_relative_bits`.
            Operand::Rel18 | Operand::Rel13 => token_idx += 1,
            Operand::Count => {
                match tokens.get(token_idx).and_then(|t| parse_count(t)) {
                    Some(count) => word0 |= (count & 0x3F) << 15,
                    None => ok = false,
                }
                token_idx += 1;
            }
            Operand::Rd | Operand::Rs1 | Operand::Rs2 | Operand::RdRs1 => {
                let nib = tokens.get(token_idx).map_or_else(|| "X".to_owned(), |t| map_reg_to_hex(t));
                match u32::from_str_radix(&nib, 16) {
                    Ok(n) => match op {
                        Operand::Rd => word0 |= n << 8,
                        Operand::Rs1 => word0 |= n << 4,
                        Operand::Rs2 => word0 |= n,
                        Operand::RdRs1 => word0 |= (n << 8) | (n << 4),
                        _ => {}
                    },
                    Err(_) => ok = false,
                }
                token_idx += 1;
            }
        }
    }

    if !ok {
        msg_list.push(
            format!("Incorrect register definition - \"{line}\""),
            Some(line_number),
            Some(filename),
            MessageType::Error,
        );
        return "ERR     ".to_owned();
    }
    format!("{word0:08X}")
}

/// Legacy trailing-nibble register packing for `.vh`-parsed opcodes (`hex_code`
/// carries `?` wildcards in its trailing nibbles).
fn legacy_add_registers(opcode: &Opcode, line: &str, filename: &str, msg_list: &mut MsgList, line_number: u32) -> String {
    let num_registers = opcode.registers;
    let mut opcode_found = opcode.hex_code.clone();

    if opcode_found.len() != 8 {
        msg_list.push(
            format!("Incorrect register definition - \"{line}\""),
            Some(line_number),
            Some(filename.to_owned()),
            MessageType::Error,
        );
        return "ERR     ".to_owned();
    }

    let cloned_opcode_found = opcode_found.get(..(8 - num_registers) as usize).unwrap_or("").to_owned();
    opcode_found.clear();
    opcode_found.push_str(&cloned_opcode_found);

    for (i, word) in line.split_whitespace().enumerate() {
        if i >= 1 && i <= num_registers as usize {
            opcode_found.push_str(&map_reg_to_hex(word));
        }
    }

    if opcode_found.len() != 8 || opcode_found.contains('X') {
        msg_list.push(
            format!("Incorrect register definition - \"{line}\""),
            Some(line_number),
            Some(filename.to_owned()),
            MessageType::Error,
        );
        return "ERR     ".to_owned();
    }
    opcode_found
}

/// Register name to hex.
///
/// Map the register to the hex code for the opcode.
fn map_reg_to_hex(input: &str) -> String {
    match input.to_uppercase().as_str() {
        "A" => "0".to_owned(),
        "B" => "1".to_owned(),
        "C" => "2".to_owned(),
        "D" => "3".to_owned(),
        "E" => "4".to_owned(),
        "F" => "5".to_owned(),
        "G" => "6".to_owned(),
        "H" => "7".to_owned(),
        "I" => "8".to_owned(),
        "J" => "9".to_owned(),
        "K" => "A".to_owned(),
        "L" => "B".to_owned(),
        "M" => "C".to_owned(),
        "N" => "D".to_owned(),
        "O" => "E".to_owned(),
        "P" => "F".to_owned(),
        _ => "X".to_owned(),
    }
}

/// Register nibble value to register name (inverse of `map_reg_to_hex`).
fn hex_nibble_to_reg(nibble: u32) -> &'static str {
    match nibble & 0xF {
        0x0 => "A",
        0x1 => "B",
        0x2 => "C",
        0x3 => "D",
        0x4 => "E",
        0x5 => "F",
        0x6 => "G",
        0x7 => "H",
        0x8 => "I",
        0x9 => "J",
        0xA => "K",
        0xB => "L",
        0xC => "M",
        0xD => "N",
        0xE => "O",
        _ => "P",
    }
}

/// Disassemble a 32-bit instruction word.
///
/// Searches `opcodes` for the first entry whose `hex_code` pattern matches `word`.
/// `'?'` characters in `hex_code` act as nibble wildcards.
/// Returns `(mnemonic_with_registers, variables_count)` or `None` if no match.
/// Register operands are extracted from the low nibbles of `word` in the same
/// order they appear in source (reg1 at the highest of the occupied nibbles).
pub fn disassemble_word(word: u32, opcodes: &[Opcode]) -> Option<(String, u32)> {
    for opcode in opcodes {
        if opcode.ops.is_empty() {
            // Legacy path: '?' in hex_code marks a wildcard nibble.
            let mut mask: u32 = 0;
            let mut pattern: u32 = 0;
            for ch in opcode.hex_code.chars() {
                mask <<= 4;
                pattern <<= 4;
                if ch != '?' {
                    mask |= 0xF_u32;
                    pattern |= ch.to_digit(16).unwrap_or(0);
                }
            }
            if (word & mask) == pattern {
                let n = opcode.registers;
                let mut text = opcode.text_name.clone();
                for i in 1..=n {
                    let shift = (n - i) * 4;
                    let nibble = (word >> shift) & 0xF;
                    text.push(' ');
                    text.push_str(hex_nibble_to_reg(nibble));
                }
                return Some((text, opcode.variables));
            }
            continue;
        }

        // v2 path: build the fixed-bit mask from the operand layout, then match.
        let Ok(template) = u32::from_str_radix(&opcode.hex_code, 16) else { continue };
        let mut mask = 0xFFFF_FFFF_u32;
        for op in &opcode.ops {
            match op {
                Operand::Rd => mask &= !(0xF << 8),
                Operand::Rs1 => mask &= !(0xF << 4),
                Operand::Rs2 | Operand::Simm4 => mask &= !0xF,
                Operand::RdRs1 => mask &= !((0xF << 8) | (0xF << 4)),
                Operand::Count => mask &= !(0x3F << 15),
                Operand::Imm8 | Operand::Off8(_) => mask &= !(0xFF << 12),
                Operand::Frame22 => mask &= !0x3F_FFFF,
                Operand::Rel18 => mask &= !0x3_FFFF,
                Operand::Rel13 => mask &= !(0x1FFF << 8),
                Operand::Imm | Operand::Imm64 => {}
            }
        }
        if (word & mask) == (template & mask) {
            let mut text = opcode.text_name.clone();
            for op in &opcode.ops {
                match op {
                    Operand::Rd | Operand::RdRs1 => {
                        text.push(' ');
                        text.push_str(hex_nibble_to_reg((word >> 8) & 0xF));
                    }
                    Operand::Rs1 => {
                        text.push(' ');
                        text.push_str(hex_nibble_to_reg((word >> 4) & 0xF));
                    }
                    Operand::Rs2 => {
                        text.push(' ');
                        text.push_str(hex_nibble_to_reg(word & 0xF));
                    }
                    Operand::Count => {
                        text.push(' ');
                        text.push_str(&((word >> 15) & 0x3F).to_string());
                    }
                    Operand::Imm8 => {
                        text.push(' ');
                        text.push_str(&((word >> 12) & 0xFF).to_string());
                    }
                    Operand::Off8(shift) => {
                        let v = i64::from((((word >> 12) & 0xFF) as u8) as i8) << shift;
                        text.push(' ');
                        text.push_str(&v.to_string());
                    }
                    Operand::Frame22 => {
                        text.push(' ');
                        text.push_str(&(word & 0x3F_FFFF).to_string());
                    }
                    Operand::Simm4 => {
                        text.push(' ');
                        text.push_str(&((((word & 0xF) << 28) as i32) >> 28).to_string());
                    }
                    Operand::Rel18 => {
                        let _ = write!(text, " PC{:+}", (((word << 14) as i32) >> 14) * 4);
                    }
                    Operand::Rel13 => {
                        let _ = write!(text, " PC{:+}", (((word << 11) as i32) >> 19) * 4);
                    }
                    Operand::Imm | Operand::Imm64 => {}
                }
            }
            return Some((text, opcode.variables));
        }
    }
    None
}

/// ISA v3 PC-relative targets (`Rel18` short branches, `Rel13` fused
/// branches): the word-0 bits for this line, given its byte address `pc`.
/// The target token is a label (`name:`) or an absolute address. Out-of-range
/// or misaligned targets are reported as errors and yield 0.
#[allow(clippy::too_many_arguments, reason = "mirrors add_arguments' reporting context")]
pub fn pc_relative_bits(
    opcodes: &[Opcode],
    line: &str,
    pc: u32,
    labels: &mut Vec<Label>,
    msg_list: &mut MsgList,
    line_number: u32,
    filename: &str,
) -> u32 {
    let Some(opcode) = return_opcode_struct(&line.to_uppercase(), opcodes) else { return 0 };
    let tokens: Vec<&str> = line.split_whitespace().collect();
    for (i, op) in opcode.ops.iter().enumerate() {
        let (bits, shift, what) = match op {
            Operand::Rel18 => (18_u32, 0_u32, "short branch"),
            Operand::Rel13 => (13, 8, "fused branch"),
            _ => continue,
        };
        let Some(tok) = tokens.get(i + 1) else { return 0 };
        let Some(hex) = convert_argument(tok, msg_list, line_number, filename.to_owned(), labels) else { return 0 };
        let Ok(target) = u32::from_str_radix(&hex, 16) else { return 0 };
        let delta = i64::from(target) - i64::from(pc);
        let words = delta / 4;
        let lim = 1_i64 << (bits - 1);
        if delta % 4 != 0 || words < -lim || words >= lim {
            msg_list.push(
                format!("{what} target out of range ({delta:+} bytes) - \"{line}\""),
                Some(line_number),
                Some(filename.to_owned()),
                MessageType::Error,
            );
            return 0;
        }
        return ((words as u32) & ((1 << bits) - 1)) << shift;
    }
    0
}

/// Returns number of args for opcode.
///
/// From opcode name, option of number of arguments for opcode, or None.
pub fn num_arguments(opcodes: &mut Vec<Opcode>, line: &str) -> Option<u32> {
    for opcode in opcodes {
        let mut words = line.split_whitespace();
        let first_word = words.next().unwrap_or("");
        if first_word.to_uppercase() == opcode.text_name {
            return Some(opcode.variables);
        }
    }
    None
}

/// Returns number of registers for opcode.
///
/// From opcode name, option of number of registers for opcode, or None.
fn num_registers(opcodes: &mut Vec<Opcode>, line: &str) -> Option<u32> {
    for opcode in opcodes {
        let mut words = line.split_whitespace();
        let first_word = words.next().unwrap_or("");
        if first_word.is_empty() {
            return None;
        }
        if first_word == opcode.text_name {
            return Some(opcode.registers);
        }
    }
    None
}

/// Parse opcode definition line to opcode.
///
/// Receive a line from the opcode definition file and if possible parse of Some(Opcode), or None.
/// Supports both 32-bit format (`32'hXXXX_XXXX`) and legacy 16-bit format (`16'hXXXX`).
#[allow(dead_code, reason = "legacy `.vh` opcode-format parser; the v2 table is built in code but this is kept for backward compatibility and is exercised by unit tests")]
pub fn opcode_from_string(input_line: &str) -> Option<Opcode> {
    let pos_comment: usize;
    let pos_end_comment: usize;

    // Find the opcode — try 32'h (new 32-bit format) first, then 16'h (legacy format).
    // For 32'h: read XXXX_XXXX (9 chars including underscore separator), strip '_' → 8 hex digits.
    // For 16'h: read XXXX (4 chars), prepend '0000' → 8 hex digits.
    let hex_code: String = if let Some(location) = input_line.find("32'h") {
        // Check if the 32'h marker is preceded by a line comment (i.e. it is commented out)
        if let Some(comment_loc) = input_line.find("//") {
            if comment_loc < location {
                return None;
            }
        }
        let pos = location + 4;
        // Need XXXX_XXXX (9 chars) after "32'h"
        if input_line.len() < pos + 9 {
            return None;
        }
        // Build 8-char hex code by removing the underscore separator at offset 4
        let raw = input_line.get(pos..pos + 9)?;
        format!("{}{}", raw.get(..4)?, raw.get(5..9)?)
    } else if let Some(location) = input_line.find("16'h") {
        // Legacy format: check if commented out
        if let Some(comment_loc) = input_line.find("//") {
            if comment_loc < location {
                return None;
            }
        }
        let pos = location + 4;
        // Need XXXX (4 chars) after "16'h"
        if input_line.len() < pos + 4 {
            return None;
        }
        format!("0000{}", input_line.get(pos..pos + 4)?)
    } else {
        return None;
    };

    // Define number of registers from trailing '?' nibbles in the 8-char hex code.
    // Each '?' represents one register field (nibble). Checked longest-first so that
    // three '?'s correctly sets 3 registers (not just 2 or 1).
    let mut num_registers: u32 = 0;
    if hex_code.get(7..8) == Some("?") {
        num_registers = 1;
    }
    if hex_code.get(6..8) == Some("??") {
        num_registers = 2;
    }
    if hex_code.get(5..8) == Some("???") {
        num_registers = 3;
    }

    // Look for variable, and set flag
    let mut num_variables: u32 = 0;
    if input_line.contains("w_var1") {
        if input_line.contains("w_var2") {
            num_variables = 2;
        } else {
            num_variables = 1;
        }
    }

    // Look for comment as first word is opcode name
    let pos_name: usize = match input_line.find("// ") {
        None => match input_line.find("//") {
            None => return None,
            Some(location) => location + 2,
        },
        Some(location) => location + 3, // Assumes one space after the // before the name of the opcode
    };

    // Find end of first word after comment as end of opcode name
    let pos_end_name: usize = input_line
        .get(pos_name..)
        .unwrap_or("")
        .find(' ')
        .map_or(input_line.len(), |location| location + pos_name);

    // Set comments field, or none if missing
    if input_line.len() > pos_end_name + 1 {
        pos_comment = pos_end_name + 1;
        pos_end_comment = input_line.len();
    } else {
        pos_comment = 0;
        pos_end_comment = 0;
    }

    Some(Opcode {
        hex_code,
        ops: Vec::new(),
        registers: num_registers,
        variables: num_variables,
        comment: input_line.get(pos_comment..pos_end_comment).unwrap_or("").to_owned(),
        text_name: input_line.get(pos_name..pos_end_name).unwrap_or("").to_owned(),
        section: String::default(),
    })
}

/// Standard macro definitions bundled with the assembler (formerly the `.vh`
/// header's `/* Macro definition */` block).
/// Built-in macro definitions, in the same `$NAME item / item / …` syntax the
/// old `opcode_select.vh` used (`%1`, `%2`, … are positional arguments). These
/// are always available; programs can add their own with the `!macro` /
/// `!macros` source directives (see [`crate::macros::apply_source_macros`]).
const V2_MACROS: &[&str] = &[
    // --- Multi-register save / restore (hardware stack; POP mirrors PUSH) ---
    "$PUSH2 PUSH %1 / PUSH %2",
    "$PUSH3 PUSH %1 / PUSH %2 / PUSH %3",
    "$PUSH4 PUSH %1 / PUSH %2 / PUSH %3 / PUSH %4",
    "$POP2 POP %2 / POP %1",
    "$POP3 POP %3 / POP %2 / POP %1",
    "$POP4 POP %4 / POP %3 / POP %2 / POP %1",
    // Save / restore the A–D scratch registers around a region of code.
    "$PUSHALL PUSH A / PUSH B / PUSH C / PUSH D",
    "$POPALL POP D / POP C / POP B / POP A",

    // --- Common register idioms ---
    "$CLR SETR %1 0",   // zero a register
    "$TEST CMPRV %1 0", // set flags from a register (compare against 0)

    // Call a subroutine while preserving the A–D scratch registers.
    "$CALLSAVE PUSH A / PUSH B / PUSH C / PUSH D / CALL %1 / POP D / POP C / POP B / POP A",

    // Copy one 64-bit word from address %2 to address %1, using %3 as scratch.
    "$MEMCPYW MEMREADRR %3 %2 / MEMSET64RR %3 %1",

    // Two back-to-back programmable delays (longer busy-wait).
    "$WAIT DELAYV %1 / DELAYV %2",

    // --- MMIO UART print helpers ---
    // Each expands to a call into uart_stubs.kla (which the program must
    // `!include`) and preserves every register the printed value does not
    // occupy, so they drop in for the retired v1 print opcodes.
    "$TXR PUSH A / COPY A %1 / CALL TX_HEX32: / POP A",
    "$NEWLINE CALL TX_NL:",
    "$TXCHAR PUSH A / COPY A %1 / CALL TX_CHAR: / POP A",
    "$TXMEMCHAR PUSH A / COPY A %1 / MEMGET8 A A / CALL TX_CHAR: / POP A",
    "$TXSTR PUSH A / COPY A %1 / CALL TX_STR: / POP A",
];

/// Build an [`Opcode`] from a v2 template word and its operand layout, deriving
/// the register/variable counts.
fn op(name: &str, template: u32, ops: &[Operand]) -> Opcode {
    let registers = ops
        .iter()
        .filter(|o| {
            matches!(
                o,
                Operand::Rd | Operand::Rs1 | Operand::Rs2 | Operand::RdRs1 | Operand::Count
                    | Operand::Imm8 | Operand::Off8(_) | Operand::Frame22 | Operand::Simm4
                    | Operand::Rel18 | Operand::Rel13
            )
        })
        .count() as u32;
    let variables = ops
        .iter()
        .map(|o| match o {
            Operand::Imm => 1,
            Operand::Imm64 => 2,
            _ => 0,
        })
        .sum();
    Opcode {
        text_name: name.to_owned(),
        hex_code: format!("{template:08X}"),
        registers,
        variables,
        comment: String::new(),
        section: String::new(),
        ops: ops.to_vec(),
    }
}

/// The built-in ISA-encoding-v2 opcode table (`ISA_ENCODING_V2.md` §3/§6).
///
/// This is the authoritative symbol → number map the assembler emits; it
/// replaces the external `opcode_select.vh` file the toolchain used to read on
/// the command line.  Register fields follow the uniform v2 layout
/// (`rd[11:8]`, `rs1[7:4]`, `rs2[3:0]`); [`Operand::RdRs1`] reproduces v1's
/// in-place forms and [`Operand::Count`] carries the embedded shift/bit count.
#[must_use]
pub fn v2_opcodes() -> Vec<Opcode> {
    vec![
        op("ADDR", 0x4420_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("SUBR", 0x4460_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("ADDC", 0x44A0_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("SUBC", 0x44E0_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("ANDR", 0x4500_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("ORR", 0x4540_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("XORR", 0x4580_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("MINR", 0x45C0_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("MAXR", 0x4600_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("MINUR", 0x4640_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("MAXUR", 0x4680_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("ADDI", 0x8830_0000, &[Operand::Rd, Operand::Rs1, Operand::Imm]),
        op("ADDV", 0x8820_0000, &[Operand::RdRs1, Operand::Imm]),
        op("MINUSV", 0x8860_0000, &[Operand::RdRs1, Operand::Imm]),
        op("ANDV", 0x8900_0000, &[Operand::RdRs1, Operand::Imm]),
        op("ORV", 0x8940_0000, &[Operand::RdRs1, Operand::Imm]),
        op("XORV", 0x8980_0000, &[Operand::RdRs1, Operand::Imm]),
        op("LEAPC", 0x8B80_0000, &[Operand::Rd, Operand::Imm]),
        op("SETR", 0x8BD0_0000, &[Operand::Rd, Operand::Imm]),
        op("SETR64", 0xCBC0_0000, &[Operand::Rd, Operand::Imm64]),
        op("CMPRR", 0x4C00_0000, &[Operand::Rs1, Operand::Rs2]),
        op("CMPRV", 0x8C10_0000, &[Operand::Rs1, Operand::Imm]),
        op("CMPEQR", 0x4C20_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("CMPNER", 0x4C60_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("CMPLTR", 0x4CA0_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("CMPGER", 0x4CE0_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("CMPLER", 0x4D20_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("CMPGTR", 0x4D60_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("CMPULTR", 0x4DA0_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("CMPUGER", 0x4DE0_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("CMPULER", 0x4E20_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("CMPUGTR", 0x4E60_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("SHLR", 0x5000_4000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("SHRR", 0x5040_4000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("SARR", 0x5080_4000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("ROLR", 0x50C0_4000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("RORR", 0x5100_4000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("SHLV", 0x5020_4000, &[Operand::RdRs1, Operand::Count]),
        op("SHRV", 0x5060_4000, &[Operand::RdRs1, Operand::Count]),
        op("SHRAV", 0x50A0_4000, &[Operand::RdRs1, Operand::Count]),
        op("ROLV", 0x50E0_4000, &[Operand::RdRs1, Operand::Count]),
        op("RORV", 0x5120_4000, &[Operand::RdRs1, Operand::Count]),
        op("SHLR1", 0x5020_8000, &[Operand::RdRs1]),
        op("SHLAR", 0x5020_8000, &[Operand::RdRs1]),
        op("SHRR1", 0x5060_8000, &[Operand::RdRs1]),
        op("SHRAR", 0x50A0_8000, &[Operand::RdRs1]),
        op("ROLR1", 0x50E0_C000, &[Operand::RdRs1]),
        op("RORR1", 0x5120_C000, &[Operand::RdRs1]),
        op("ROLCR", 0x5160_C000, &[Operand::RdRs1]),
        op("RORCR", 0x51A0_C000, &[Operand::RdRs1]),
        op("BSETRR", 0x5200_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("BCLRRR", 0x5240_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("BTGLRR", 0x5280_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("BTSTRR", 0x52C0_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("BSET", 0x5220_0000, &[Operand::RdRs1, Operand::Count]),
        op("BCLR", 0x5260_0000, &[Operand::RdRs1, Operand::Count]),
        op("BTGL", 0x52A0_0000, &[Operand::RdRs1, Operand::Count]),
        op("BTST", 0x52E0_0000, &[Operand::Rs1, Operand::Count]),
        op("BEXTR", 0x9300_0000, &[Operand::Rd, Operand::Rs1, Operand::Imm]),
        op("BDEP", 0x9340_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2, Operand::Imm]),
        op("COPY", 0x5400_0000, &[Operand::Rd, Operand::Rs1]),
        op("NEGR", 0x5448_0000, &[Operand::RdRs1]),
        op("NOTR", 0x5488_0000, &[Operand::RdRs1]),
        op("ABSR", 0x54C8_0000, &[Operand::RdRs1]),
        op("SEXTB", 0x5508_0000, &[Operand::RdRs1]),
        op("SEXTH", 0x5518_0000, &[Operand::RdRs1]),
        op("SEXTW", 0x5520_0000, &[Operand::RdRs1]),
        op("ZEXTB", 0x5548_0000, &[Operand::RdRs1]),
        op("ZEXTH", 0x5558_0000, &[Operand::RdRs1]),
        op("ZEXTW", 0x5560_0000, &[Operand::RdRs1]),
        op("BSWAP", 0x5580_0000, &[Operand::RdRs1]),
        op("BITREV", 0x55C0_0000, &[Operand::RdRs1]),
        op("POPCNT", 0x5608_0000, &[Operand::RdRs1]),
        op("CLZ", 0x5640_0000, &[Operand::RdRs1]),
        op("CTZ", 0x5680_0000, &[Operand::RdRs1]),
        op("SETFR", 0x5700_0000, &[Operand::Rd]),
        op("INCR", 0x5788_0000, &[Operand::RdRs1]),
        op("DECR", 0x57C8_0000, &[Operand::RdRs1]),
        op("MEMGET8", 0x5800_0000, &[Operand::Rd, Operand::Rs1]),
        op("MEMGET16", 0x5900_0000, &[Operand::Rd, Operand::Rs1]),
        op("MEMGET32", 0x5A00_0000, &[Operand::Rd, Operand::Rs1]),
        op("MEMREADRR", 0x5B00_0000, &[Operand::Rd, Operand::Rs1]),
        op("MEMGET64", 0x5B10_0000, &[Operand::Rd, Operand::Rs1]),
        op("LDIDX8", 0x9820_0000, &[Operand::Rd, Operand::Rs1, Operand::Imm]),
        op("LDIDX8_S", 0x98A0_0000, &[Operand::Rd, Operand::Rs1, Operand::Imm]),
        op("LDIDX16", 0x9920_0000, &[Operand::Rd, Operand::Rs1, Operand::Imm]),
        op("LDIDX16_S", 0x99A0_0000, &[Operand::Rd, Operand::Rs1, Operand::Imm]),
        op("LDIDX32", 0x9A20_0000, &[Operand::Rd, Operand::Rs1, Operand::Imm]),
        op("LDIDX64", 0x9B20_0000, &[Operand::Rd, Operand::Rs1, Operand::Imm]),
        op("LDIDX64A", 0x9B30_0000, &[Operand::Rd, Operand::Rs1, Operand::Imm]),
        op("MEMREADR", 0x9B40_0000, &[Operand::Rd, Operand::Imm]),
        op("LDIDX64R", 0x5B60_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("MEMSET8", 0x5C00_0000, &[Operand::Rd, Operand::Rs1]),
        op("MEMSET16", 0x5D00_0000, &[Operand::Rd, Operand::Rs1]),
        op("MEMSET32", 0x5E00_0000, &[Operand::Rd, Operand::Rs1]),
        op("MEMSET64RR", 0x5F00_0000, &[Operand::Rd, Operand::Rs1]),
        op("MEMSET64", 0x5F10_0000, &[Operand::Rd, Operand::Rs1]),
        op("STIDX8", 0x9C20_0000, &[Operand::Rd, Operand::Rs1, Operand::Imm]),
        op("STIDX16", 0x9D20_0000, &[Operand::Rd, Operand::Rs1, Operand::Imm]),
        op("STIDX32", 0x9E20_0000, &[Operand::Rd, Operand::Rs1, Operand::Imm]),
        op("STIDX64", 0x9F20_0000, &[Operand::Rd, Operand::Rs1, Operand::Imm]),
        op("STIDX64A", 0x9F30_0000, &[Operand::Rd, Operand::Rs1, Operand::Imm]),
        op("MEMSETR", 0x9F40_0000, &[Operand::Rd, Operand::Imm]),
        op("STIDX64R", 0x5F60_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        // ---- ISA v3 (ISA_V3_PROPOSAL.md): D1/D2 long forms, short 1-word
        //      immediates (".S", literal immediates only), ENTER/LEAVE ----
        op("LDIDX32_S", 0x9AA0_0000, &[Operand::Rd, Operand::Rs1, Operand::Imm]),
        // v3 D3: 32-bit W ops (result = sext of the low 32 bits)
        op("ADDW", 0x4428_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("SUBW", 0x4468_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("MULW", 0x6888_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("ADDIW", 0x8838_0000, &[Operand::Rd, Operand::Rs1, Operand::Imm]),
        op("CMPRRW", 0x4C08_0000, &[Operand::Rs1, Operand::Rs2]),
        op("CMPRVW", 0x8C18_0000, &[Operand::Rs1, Operand::Imm]),
        op("SETR.S", 0x4BD0_0000, &[Operand::Rd, Operand::Imm8]),
        op("ADDI.S", 0x4830_0000, &[Operand::Rd, Operand::Rs1, Operand::Imm8]),
        op("ADDV.S", 0x4820_0000, &[Operand::RdRs1, Operand::Imm8]),
        op("MINUSV.S", 0x4860_0000, &[Operand::RdRs1, Operand::Imm8]),
        op("ANDV.S", 0x4900_0000, &[Operand::RdRs1, Operand::Imm8]),
        op("ORV.S", 0x4940_0000, &[Operand::RdRs1, Operand::Imm8]),
        op("XORV.S", 0x4980_0000, &[Operand::RdRs1, Operand::Imm8]),
        op("CMPRV.S", 0x4C10_0000, &[Operand::Rs1, Operand::Imm8]),
        op("LDIDX8.S", 0x5820_0000, &[Operand::Rd, Operand::Rs1, Operand::Off8(0)]),
        op("LDIDX8_S.S", 0x58A0_0000, &[Operand::Rd, Operand::Rs1, Operand::Off8(0)]),
        op("LDIDX16.S", 0x5920_0000, &[Operand::Rd, Operand::Rs1, Operand::Off8(1)]),
        op("LDIDX16_S.S", 0x59A0_0000, &[Operand::Rd, Operand::Rs1, Operand::Off8(1)]),
        op("LDIDX32.S", 0x5A20_0000, &[Operand::Rd, Operand::Rs1, Operand::Off8(2)]),
        op("LDIDX32_S.S", 0x5AA0_0000, &[Operand::Rd, Operand::Rs1, Operand::Off8(2)]),
        op("LDIDX64.S", 0x5B20_0000, &[Operand::Rd, Operand::Rs1, Operand::Off8(3)]),
        op("LDIDX64A.S", 0x5B30_0000, &[Operand::Rd, Operand::Rs1, Operand::Off8(3)]),
        op("STIDX8.S", 0x5C20_0000, &[Operand::Rd, Operand::Rs1, Operand::Off8(0)]),
        op("STIDX16.S", 0x5D20_0000, &[Operand::Rd, Operand::Rs1, Operand::Off8(1)]),
        op("STIDX32.S", 0x5E20_0000, &[Operand::Rd, Operand::Rs1, Operand::Off8(2)]),
        op("STIDX64.S", 0x5F20_0000, &[Operand::Rd, Operand::Rs1, Operand::Off8(3)]),
        op("STIDX64A.S", 0x5F30_0000, &[Operand::Rd, Operand::Rs1, Operand::Off8(3)]),
        op("ENTER", 0x6600_0000, &[Operand::Frame22]),
        op("LEAVE", 0x6640_0000, &[]),
        op("LEAVERET", 0x6680_0000, &[]),
        // ISA v3 A1 short PC-relative branches/calls (target within +-512 KB)
        // and B fused compare-and-branch (target within +-16 KB, flags untouched).
        op("JMP.S", 0x6100_0000, &[Operand::Rel18]),
        op("JMPZ.S", 0x6108_0000, &[Operand::Rel18]),
        op("JMPNZ.S", 0x610C_0000, &[Operand::Rel18]),
        op("JMPC.S", 0x6110_0000, &[Operand::Rel18]),
        op("JMPNC.S", 0x6114_0000, &[Operand::Rel18]),
        op("JMPO.S", 0x6118_0000, &[Operand::Rel18]),
        op("JMPNO.S", 0x611C_0000, &[Operand::Rel18]),
        op("JMPS.S", 0x6120_0000, &[Operand::Rel18]),
        op("JMPNS.S", 0x6124_0000, &[Operand::Rel18]),
        op("JMPLT.S", 0x6128_0000, &[Operand::Rel18]),
        op("JMPGE.S", 0x612C_0000, &[Operand::Rel18]),
        op("JMPLE.S", 0x6130_0000, &[Operand::Rel18]),
        op("JMPGT.S", 0x6134_0000, &[Operand::Rel18]),
        op("JMPULT.S", 0x6138_0000, &[Operand::Rel18]),
        op("JMPUGE.S", 0x613C_0000, &[Operand::Rel18]),
        op("JMPULE.S", 0x6140_0000, &[Operand::Rel18]),
        op("JMPUGT.S", 0x6144_0000, &[Operand::Rel18]),
        op("JMPE.S", 0x6148_0000, &[Operand::Rel18]),
        op("JMPNE.S", 0x614C_0000, &[Operand::Rel18]),
        op("CALL.S", 0x6300_0000, &[Operand::Rel18]),
        op("BEQ", 0x7400_0000, &[Operand::Rs1, Operand::Rs2, Operand::Rel13]),
        op("BEQI", 0x7420_0000, &[Operand::Rs1, Operand::Simm4, Operand::Rel13]),
        op("BNE", 0x7440_0000, &[Operand::Rs1, Operand::Rs2, Operand::Rel13]),
        op("BNEI", 0x7460_0000, &[Operand::Rs1, Operand::Simm4, Operand::Rel13]),
        op("BLT", 0x7480_0000, &[Operand::Rs1, Operand::Rs2, Operand::Rel13]),
        op("BLTI", 0x74A0_0000, &[Operand::Rs1, Operand::Simm4, Operand::Rel13]),
        op("BGE", 0x74C0_0000, &[Operand::Rs1, Operand::Rs2, Operand::Rel13]),
        op("BGEI", 0x74E0_0000, &[Operand::Rs1, Operand::Simm4, Operand::Rel13]),
        op("BLE", 0x7500_0000, &[Operand::Rs1, Operand::Rs2, Operand::Rel13]),
        op("BLEI", 0x7520_0000, &[Operand::Rs1, Operand::Simm4, Operand::Rel13]),
        op("BGT", 0x7540_0000, &[Operand::Rs1, Operand::Rs2, Operand::Rel13]),
        op("BGTI", 0x7560_0000, &[Operand::Rs1, Operand::Simm4, Operand::Rel13]),
        op("BULT", 0x7580_0000, &[Operand::Rs1, Operand::Rs2, Operand::Rel13]),
        op("BULTI", 0x75A0_0000, &[Operand::Rs1, Operand::Simm4, Operand::Rel13]),
        op("BUGE", 0x75C0_0000, &[Operand::Rs1, Operand::Rs2, Operand::Rel13]),
        op("BUGEI", 0x75E0_0000, &[Operand::Rs1, Operand::Simm4, Operand::Rel13]),
        op("BULE", 0x7600_0000, &[Operand::Rs1, Operand::Rs2, Operand::Rel13]),
        op("BULEI", 0x7620_0000, &[Operand::Rs1, Operand::Simm4, Operand::Rel13]),
        op("BUGT", 0x7640_0000, &[Operand::Rs1, Operand::Rs2, Operand::Rel13]),
        op("BUGTI", 0x7660_0000, &[Operand::Rs1, Operand::Simm4, Operand::Rel13]),
        op("JMP", 0xA000_0000, &[Operand::Imm]),
        op("JMPZ", 0xA008_0000, &[Operand::Imm]),
        op("JMPNZ", 0xA00C_0000, &[Operand::Imm]),
        op("JMPC", 0xA010_0000, &[Operand::Imm]),
        op("JMPNC", 0xA014_0000, &[Operand::Imm]),
        op("JMPO", 0xA018_0000, &[Operand::Imm]),
        op("JMPNO", 0xA01C_0000, &[Operand::Imm]),
        op("JMPS", 0xA020_0000, &[Operand::Imm]),
        op("JMPNS", 0xA024_0000, &[Operand::Imm]),
        op("JMPLT", 0xA028_0000, &[Operand::Imm]),
        op("JMPGE", 0xA02C_0000, &[Operand::Imm]),
        op("JMPLE", 0xA030_0000, &[Operand::Imm]),
        op("JMPGT", 0xA034_0000, &[Operand::Imm]),
        op("JMPULT", 0xA038_0000, &[Operand::Imm]),
        op("JMPUGE", 0xA03C_0000, &[Operand::Imm]),
        op("JMPULE", 0xA040_0000, &[Operand::Imm]),
        op("JMPUGT", 0xA044_0000, &[Operand::Imm]),
        op("JMPE", 0xA048_0000, &[Operand::Imm]),
        op("JMPNE", 0xA04C_0000, &[Operand::Imm]),
        op("CALL", 0xA200_0000, &[Operand::Imm]),
        op("CALLZ", 0xA208_0000, &[Operand::Imm]),
        op("CALLNZ", 0xA20C_0000, &[Operand::Imm]),
        op("CALLC", 0xA210_0000, &[Operand::Imm]),
        op("CALLNC", 0xA214_0000, &[Operand::Imm]),
        op("CALLO", 0xA218_0000, &[Operand::Imm]),
        op("CALLNO", 0xA21C_0000, &[Operand::Imm]),
        op("CALLE", 0xA248_0000, &[Operand::Imm]),
        op("CALLNE", 0xA24C_0000, &[Operand::Imm]),
        op("JMPR", 0x6080_0000, &[Operand::Rs2]),
        op("CALLR", 0x6280_0000, &[Operand::Rs2]),
        op("PUSH", 0x6400_0000, &[Operand::Rs1]),
        op("PUSHV", 0xA440_0000, &[Operand::Imm]),
        op("PUSHV64", 0xE440_0000, &[Operand::Imm64]),
        op("POP", 0x6480_0000, &[Operand::Rd]),
        op("GETSP", 0x64C0_0000, &[Operand::Rd]),
        op("SETSP", 0x6500_0000, &[Operand::Rs1]),
        op("ADDSP", 0xA540_0000, &[Operand::Imm]),
        op("RET", 0x6580_0000, &[]),
        op("IRET", 0x65C0_0000, &[]),
        op("MULUR", 0x6800_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("MULHUR", 0x6840_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("MULR", 0x6880_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("MULHR", 0x68C0_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("MULV", 0xA880_0000, &[Operand::RdRs1, Operand::Imm]),
        op("DIVUR", 0x6900_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("DIVR", 0x6980_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("DIVV", 0xA980_0000, &[Operand::RdRs1, Operand::Imm]),
        op("MODUR", 0x6A00_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("MODR", 0x6A80_0000, &[Operand::Rd, Operand::Rs1, Operand::Rs2]),
        op("MODV", 0xAA80_0000, &[Operand::RdRs1, Operand::Imm]),
        op("NOP", 0x6C00_0000, &[]),
        op("HALT", 0x6C01_0000, &[]),
        op("WAIT", 0x6C02_0000, &[]),
        op("RESET", 0x6C03_0000, &[]),
        op("TRAP", 0x6C04_0000, &[]),
        op("DELAYR", 0x6C05_0000, &[Operand::Rs1]),
        op("DELAYV", 0xAC05_0000, &[Operand::Imm]),
        op("LCDCMDR", 0x7000_0000, &[Operand::Rs1]),
        op("LCDDATAR", 0x7100_0000, &[Operand::Rs1]),
        op("LCDCMDV", 0xB000_0000, &[Operand::Imm]),
        op("LCDDATAV", 0xB100_0000, &[Operand::Imm]),
        op("LCDRST", 0xB200_0000, &[Operand::Imm]),
    ]
}

/// The built-in v2 opcode table plus the standard macro definitions — the
/// in-code replacement for parsing an `opcode_select.vh` file.
#[must_use]
pub fn v2_opcodes_and_macros(msg_list: &mut MsgList) -> (Vec<Opcode>, Vec<Macro>) {
    let macros = V2_MACROS.iter().filter_map(|line| macro_from_string(line, msg_list)).collect();
    (v2_opcodes(), macros)
}

/// Parse file to opcode and macro vectors.
///
/// Parses the .vh verilog file, creates two vectors of macro and opcode, returning None, None or Some(Opcode), Some(Macro).
#[allow(dead_code, reason = "legacy `.vh` opcode-file parser; superseded by the built-in v2 table but retained for backward compatibility and unit tests")]
pub fn parse_vh_file(input_list: Vec<InputData>, msg_list: &mut MsgList) -> (Option<Vec<Opcode>>, Option<Vec<Macro>>) {
    if input_list.is_empty() {
        return (None, None);
    }

    let mut opcodes: Vec<Opcode> = Vec::new();
    let mut macros: Vec<Macro> = Vec::new();
    let mut section_name = String::default();

    for line in input_list {
        if let Some(section) = line.input.trim().strip_prefix("///") {
            section.to_owned().trim().clone_into(&mut section_name);
        }

        match opcode_from_string(&line.input) {
            None => (),
            Some(opcode) => {
                if return_opcode(&opcode.text_name, &mut opcodes).is_some() {
                    msg_list.push(
                        format!("Duplicate Opcode {} found", opcode.text_name),
                        Some(line.line_counter),
                        Some(line.file_name.clone()),
                        MessageType::Error,
                    );
                }
                //opcodes.push(a);
                opcodes.push(Opcode {
                    text_name: opcode.text_name,
                    hex_code: opcode.hex_code,
                    ops: Vec::new(),
                    registers: opcode.registers,
                    variables: opcode.variables,
                    comment: opcode.comment,
                    section: section_name.clone(),
                });
            }
        }
        match macro_from_string(&line.input, msg_list) {
            None => (),
            Some(found_macro) => {
                if return_macro(&found_macro.name, &mut macros).is_some() {
                    msg_list.push(
                        format!("Duplicate Macro definition {} found", found_macro.name),
                        Some(line.line_counter),
                        Some(line.file_name),
                        MessageType::Error,
                    );
                }
                macros.push(found_macro);
            }
        }
    }
    (Some(opcodes), Some(macros))
}

/// Returns hex opcode from name.
///
/// Checks if first word is opcode and if so returns opcode hex value.
pub fn return_opcode(line: &str, opcodes: &mut Vec<Opcode>) -> Option<String> {
    for opcode in opcodes {
        let mut words = line.split_whitespace();
        let first_word = words.next().unwrap_or("");
        if first_word.to_uppercase() == opcode.text_name {
            return Some(opcode.hex_code.to_uppercase());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests may unwrap/expect")]
    use super::*;
    use crate::labels;

    #[test]
    // Test that the correct number of registers is returned
    fn test_num_registers1() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 1,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, Some(1));
    }

    #[test]
    // Test that the None is returned if the opcode is not found
    fn test_num_registers2() {
        let input = String::from("PULL");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 1,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, None);
    }
    #[test]
    // Test that the correct number of arguments is returned
    fn test_num_arguments1() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 1,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, Some(1));
    }
    #[test]
    // Test that the correct number of arguments is returned
    fn test_num_arguments2() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 2,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, Some(2));
    }

    #[test]
    // Test that the correct number of arguments is returned 2 variable 2 registers
    fn test_num_arguments3() {
        let input = String::from("PUSH ddd yyy");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 2,
            registers: 2,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, Some(2));
    }

    #[test]
    // Test that None is returned if the opcode is not found
    fn test_num_arguments4() {
        let input = String::from("PUSH2");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 2,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, None);
    }

    #[test]
    // Test that None is returned if the opcode is blank
    fn test_num_arguments5() {
        let input = String::default();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 2,
            section: String::default(),
        });
        let output = num_registers(opcodes, &input);
        assert_eq!(output, None);
    }

    #[test]
    // Test that the correct opcode is returned
    fn test_return_opcode1() {
        let input = String::from("PUSH");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 2,
            section: String::default(),
        });
        let output = return_opcode(&input, opcodes);
        assert_eq!(output, Some(String::from("1234")));
    }

    #[test]
    // Test that None is returned if the opcode is not found
    fn test_return_opcode2() {
        let input = String::from("PUSH2");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("1234"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 2,
            section: String::default(),
        });
        let output = return_opcode(&input, opcodes);
        assert_eq!(output, None);
    }

    #[test]
    // This test is to check that the function will return correct output if the number of registers is correct
    fn test_add_registers1() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH A B");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("000056XX"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 2,
            section: String::default(),
        });
        let output = add_registers(opcodes, &input, "test".to_owned(), &mut msg_list, 1);
        assert_eq!(output, String::from("00005601"));
    }

    #[test]
    // This test is to check that the function will return an error if the number of registers is incorrect
    fn test_add_registers2() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH A B");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("000056XX"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 1,
            section: String::default(),
        });
        let output = add_registers(opcodes, &input, "test".to_owned(), &mut msg_list, 1);
        assert_eq!(output, String::from("ERR     "));
    }
    #[test]
    // This test is to check that the function will return an error if the length of the opcode is not correct
    fn test_add_registers3() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH A B");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("000056X"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 1,
            section: String::default(),
        });
        let output = add_registers(opcodes, &input, "test".to_owned(), &mut msg_list, 1);
        assert_eq!(output, String::from("ERR     "));
    }
    #[test]
    // Test single hex argument
    fn test_add_arguments1() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH 0xFFFF");
        let mut labels = Vec::<labels::Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("00000000"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 1,
            registers: 0,
            section: String::default(),
        });
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &mut labels);
        assert_eq!(output, String::from("0000FFFF"));
    }

    #[test]
    // Test single decimal argument
    fn test_add_arguments2() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH 1234");
        let mut labels = Vec::<labels::Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("00000000"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 1,
            registers: 0,
            section: String::default(),
        });
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &mut labels);
        assert_eq!(output, String::from("000004D2"));
    }

    #[test]
    // Test invalid argument
    fn test_add_arguments3() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH HELLO");
        let mut labels = Vec::<labels::Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("00000000"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 1,
            registers: 0,
            section: String::default(),
        });
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &mut labels);
        assert_eq!(output, String::from("00000000"));
    }

    #[test]
    // Test invalid second argument
    fn test_add_arguments4() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH 0xF RRR");
        let mut labels = Vec::<labels::Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("00000000"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 2,
            registers: 0,
            section: String::default(),
        });
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &mut labels);
        assert_eq!(output, String::from("0000000F00000000"));
    }

    #[test]
    // Test two arguments
    fn test_add_arguments5() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH 1 0xF");
        let mut labels = Vec::<labels::Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("00000000"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 2,
            registers: 0,
            section: String::default(),
        });
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &mut labels);
        assert_eq!(output, String::from("000000010000000F"));
    }

    #[test]
    // Test too many arguments
    fn test_add_arguments6() {
        let mut msg_list = MsgList::new();
        let input = String::from("PUSH 1 0xF");
        let mut labels = Vec::<labels::Label>::new();
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("PUSH"),
            hex_code: String::from("00000000"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 1,
            registers: 0,
            section: String::default(),
        });
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &mut labels);
        assert_eq!(output, String::from("00000001"));
        assert_eq!(
            msg_list.list.first().unwrap_or_default().text,
            "Too many arguments found - \"PUSH 1 0xF\""
        );
    }

    #[test]
    // Test import with two registers
    fn test_opcode_from_string1() {
        let input = "16'h01??: t_copy_regs;                              // COPY Copy register";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "COPY".to_owned(),
                hex_code: "000001??".to_owned(),
                ops: Vec::new(),
                registers: 2,
                variables: 0,
                comment: "Copy register".to_owned(),
                section: String::default(),
            })
        );
    }
    #[test]
    // Test import with one argument and one register
    fn test_opcode_from_string2() {
        let input = "16'h086?: t_and_reg_value(w_var1);                  // ANDV AND register with value";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "ANDV".to_owned(),
                hex_code: "0000086?".to_owned(),
                ops: Vec::new(),
                registers: 1,
                variables: 1,
                comment: "AND register with value".to_owned(),
                section: String::default(),
            })
        );
    }

    #[test]
    // Test import with two arguments
    fn test_opcode_from_string3() {
        let input = "16'h0864: t_and_reg_value(w_var1,w_var2);        // MOV Move from addr to addr";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "MOV".to_owned(),
                hex_code: "00000864".to_owned(),
                ops: Vec::new(),
                registers: 0,
                variables: 2,
                comment: "Move from addr to addr".to_owned(),
                section: String::default(),
            })
        );
    }

    #[test]
    // Test no comments
    fn test_opcode_from_string4() {
        let input = "dummy 16'h0864: t_and_reg_value(w_var1,w_var2);";
        let output = opcode_from_string(input);
        assert_eq!(output, None);
    }

    #[test]
    // Test commented out
    fn test_opcode_from_string5() {
        let input = "// 16'h0864: t_and_reg_value(w_var1,w_var2);";
        let output = opcode_from_string(input);
        assert_eq!(output, None);
    }

    #[test]
    // Test import if failed
    fn test_opcode_from_string6() {
        let input = "xxxxx";
        let output = opcode_from_string(input);
        assert_eq!(output, None);
    }

    #[test]
    // Test import if too short
    fn test_opcode_from_string7() {
        let input = "16'h0";
        let output = opcode_from_string(input);
        assert_eq!(output, None);
    }

    #[test]
    // Test import if no space for definition
    fn test_opcode_from_string8() {
        let input = "16'h1234 //abcd";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "abcd".to_owned(),
                hex_code: "00001234".to_owned(),
                ops: Vec::new(),
                registers: 0,
                variables: 0,
                comment: String::default(),
                section: String::default(),
            })
        );
    }

    #[test]
    // Test import with for no comment after the opcode name
    fn test_opcode_from_string9() {
        let input = "16'h0864: t_and_reg_value(w_var1,w_var2);        // MOV";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "MOV".to_owned(),
                hex_code: "00000864".to_owned(),
                ops: Vec::new(),
                registers: 0,
                variables: 2,
                comment: String::default(),
                section: String::default(),
            })
        );
    }

    #[test]
    fn test_map_reg_to_hex() {
        assert_eq!(map_reg_to_hex("B"), "1");
        assert_eq!(map_reg_to_hex("P"), "F");
        assert_eq!(map_reg_to_hex("Z"), "X");
    }

    #[test]
    // Test no macro or opcodes
    fn test_parse_vh_file1() {
        let mut msg_list = MsgList::new();
        let vh_list = vec![
            InputData {
                input: "abc/* This is a comment */def".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "abc/* This is a comment */def".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 2,
            },
        ];

        let (opt_oplist, opt_macro_list) = parse_vh_file(vh_list, &mut msg_list);

        assert!(opt_oplist.unwrap_or_default().is_empty());
        assert!(opt_macro_list.unwrap_or_default().is_empty());
    }

    #[test]
    // Test normal macro and opcode
    fn test_parse_vh_file2() {
        let mut msg_list = MsgList::new();
        let vh_list = vec![
            InputData {
                input: "$WAIT DELAYV %1 / DELAYV %2 ".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "16'h05??: t_compare_regs;    // CMPRR Compare registers".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 2,
            },
        ];

        let (opt_oplist, opt_macro_list) = parse_vh_file(vh_list, &mut msg_list);

        assert_eq!(
            opt_oplist.unwrap_or_default(),
            vec![Opcode {
                text_name: "CMPRR".to_owned(),
                hex_code: "000005??".to_owned(),
                ops: Vec::new(),
                registers: 2,
                variables: 0,
                comment: "Compare registers".to_owned(),
                section: String::default(),
            }]
        );
        assert_eq!(
            opt_macro_list.unwrap_or_default(),
            vec![Macro {
                name: "$WAIT".to_owned(),
                variables: 2,
                items: ["DELAYV %1".to_owned(), "DELAYV %2".to_owned()].to_vec(),
                comment: String::default()
            }]
        );
    }

    #[test]
    // Test duplicate macro
    fn test_parse_vh_file3() {
        let mut msg_list = MsgList::new();
        let vh_list = vec![
            InputData {
                input: "$WAIT DELAYV %1 / DELAYV %2 ".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "16'h05??: t_compare_regs;    // CMPRR Compare registers".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 2,
            },
            InputData {
                input: "$WAIT DELAYV %1 / DELAYV %2 ".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 3,
            },
            InputData {
                input: "16'h05??: t_compare_regs;    // CMPRR Compare registers".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 4,
            },
        ];

        let (_opt_oplist, _opt_macro_list) = parse_vh_file(vh_list, &mut msg_list);

        assert_eq!(msg_list.list.first().unwrap_or_default().text, "Duplicate Macro definition $WAIT found");
        assert_eq!(msg_list.list.first().unwrap_or_default().line_number, Some(3));
        assert_eq!(msg_list.list.get(1).unwrap_or_default().text, "Duplicate Opcode CMPRR found");
        assert_eq!(msg_list.list.get(1).unwrap_or_default().line_number, Some(4));
    }
    #[test]
    // Test empty list
    fn test_parse_vh_file4() {
        let mut msg_list = MsgList::new();
        let vh_list = vec![];

        let (opt_oplist, opt_macro_list) = parse_vh_file(vh_list, &mut msg_list);

        assert_eq!(opt_oplist, None);
        assert_eq!(opt_macro_list, None);
    }

    #[test]
    // Test normal opcode with sections
    fn test_parse_vh_file5() {
        let mut msg_list = MsgList::new();
        let vh_list = vec![
            InputData {
                input: "/// Section 1".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 1,
            },
            InputData {
                input: "16'h06??: t_push_addr;    // PUSH push value to reg".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 2,
            },
            InputData {
                input: "16'h05??: t_compare_regs;    // CMPRR Compare registers".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 3,
            },
            InputData {
                input: "/// Section 2".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 4,
            },
            InputData {
                input: "16'h16??: t_pop_addr;    // POP push value to reg".to_owned(),
                file_name: "opcode_select.vh".to_owned(),
                line_counter: 5,
            },
        ];

        let (opt_oplist, _opt_macro_list) = parse_vh_file(vh_list, &mut msg_list);

        assert_eq!(
            opt_oplist.unwrap_or_default(),
            vec![
                Opcode {
                    text_name: "PUSH".to_owned(),
                    hex_code: "000006??".to_owned(),
                    ops: Vec::new(),
                    registers: 2,
                    variables: 0,
                    comment: "push value to reg".to_owned(),
                    section: "Section 1".to_owned(),
                },
                Opcode {
                    text_name: "CMPRR".to_owned(),
                    hex_code: "000005??".to_owned(),
                    ops: Vec::new(),
                    registers: 2,
                    variables: 0,
                    comment: "Compare registers".to_owned(),
                    section: "Section 1".to_owned(),
                },
                Opcode {
                    text_name: "POP".to_owned(),
                    hex_code: "000016??".to_owned(),
                    ops: Vec::new(),
                    registers: 2,
                    variables: 0,
                    comment: "push value to reg".to_owned(),
                    section: "Section 2".to_owned(),
                }
            ]
        );
    }

    // -------------------------------------------------------------------------
    // 32-bit format (32'hXXXX_XXXX) tests
    // -------------------------------------------------------------------------

    #[test]
    // Parse 32'h RRR format (three registers, no variable)
    fn test_opcode_from_string_32bit_rrr() {
        let input = "32'h0001_0???: t_addr3;                               // ADDR RRR rd=rs1+rs2";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "ADDR".to_owned(),
                hex_code: "00010???".to_owned(),
                ops: Vec::new(),
                registers: 3,
                variables: 0,
                comment: "RRR rd=rs1+rs2".to_owned(),
                section: String::default(),
            })
        );
    }

    #[test]
    // Parse 32'h RV format (one register, one variable)
    fn test_opcode_from_string_32bit_rv() {
        let input = "32'h0000_080?: t_set_reg(w_var1);                     // SETR RV Set register to a value";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "SETR".to_owned(),
                hex_code: "0000080?".to_owned(),
                ops: Vec::new(),
                registers: 1,
                variables: 1,
                comment: "RV Set register to a value".to_owned(),
                section: String::default(),
            })
        );
    }

    #[test]
    // Parse 32'h V format (no registers, one variable)
    fn test_opcode_from_string_32bit_v() {
        let input = "32'h0000_1000: t_cond_jump(w_var1, 1'b1);             // JMP V Jump";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "JMP".to_owned(),
                hex_code: "00001000".to_owned(),
                ops: Vec::new(),
                registers: 0,
                variables: 1,
                comment: "V Jump".to_owned(),
                section: String::default(),
            })
        );
    }

    #[test]
    // Parse 32'h RR format (two registers, no variable)
    fn test_opcode_from_string_32bit_rr() {
        let input = "32'h0000_0C??: t_load_indexed(w_var1);                // LDIDX RRV first=mem[second+var1]";
        let output = opcode_from_string(input);
        assert_eq!(
            output,
            Some(Opcode {
                text_name: "LDIDX".to_owned(),
                hex_code: "00000C??".to_owned(),
                ops: Vec::new(),
                registers: 2,
                variables: 1,
                comment: "RRV first=mem[second+var1]".to_owned(),
                section: String::default(),
            })
        );
    }

    #[test]
    // 32'h line that is commented out returns None
    fn test_opcode_from_string_32bit_commented() {
        let input = "// 32'h0001_0???: t_addr3;                            // ADDR RRR rd=rs1+rs2";
        let output = opcode_from_string(input);
        assert_eq!(output, None);
    }

    #[test]
    // 32'h line that is too short returns None
    fn test_opcode_from_string_32bit_too_short() {
        let input = "32'h0001_0";
        let output = opcode_from_string(input);
        assert_eq!(output, None);
    }

    #[test]
    // add_registers handles three-register (RRR) instructions correctly
    fn test_add_registers_three_regs() {
        let mut msg_list = MsgList::new();
        let input = String::from("ADDR A B C");
        let opcodes = &mut Vec::<Opcode>::new();
        opcodes.push(Opcode {
            text_name: String::from("ADDR"),
            hex_code: String::from("00010???"),
            ops: Vec::new(),
            comment: String::default(),
            variables: 0,
            registers: 3,
            section: String::default(),
        });
        let output = add_registers(opcodes, &input, "test".to_owned(), &mut msg_list, 1);
        // A=0, B=1, C=2 → "00010" + "0" + "1" + "2"
        assert_eq!(output, String::from("00010012"));
    }

    #[test]
    // ISA v3 mnemonics encode to the same words the emulator/RTL tests use.
    fn test_v3_short_forms_assemble() {
        let mut msg_list = MsgList::new();
        let opcodes = &mut v2_opcodes();
        let cases = [
            ("SETR.S A -5", "4BDFB000"),
            ("ADDI.S C B -1", "483FF210"),
            ("CMPRV.S A -3", "4C1FD000"),
            ("STIDX64A.S A B 16", "5F302010"),
            ("LDIDX64A.S C B 16", "5B302210"),
            ("LDIDX32_S.S D B -4", "5AAFF310"),
            ("CMPRRW A B", "4C080001"),
            ("ADDW C A B", "44280201"),
            ("MULW F A B", "68880501"),
            ("ENTER 3", "66000003"),
            ("LEAVE", "66400000"),
            ("LEAVERET", "66800000"),
        ];
        for (src, want) in cases {
            let got = add_registers(opcodes, &src.to_owned(), "test".to_owned(), &mut msg_list, 1);
            assert_eq!(got, want, "{src}");
        }
        assert_eq!(msg_list.number_by_type(&MessageType::Error), 0);
        // Out-of-range / misaligned short immediates are rejected.
        for bad in ["SETR.S A 300", "LDIDX64A.S A B 12", "LDIDX64A.S A B 2048"] {
            let got = add_registers(opcodes, &bad.to_owned(), "test".to_owned(), &mut msg_list, 1);
            assert_eq!(got, "ERR     ", "{bad}");
        }
        // Disassembly round-trips the scaled offset.
        let (text, vars) = disassemble_word(0x5AAF_F310, opcodes).unwrap();
        assert_eq!((text.as_str(), vars), ("LDIDX32_S.S D B -4", 0));
    }

    #[test]
    // ISA v3 PC-relative targets: short (simm18) and fused (simm13) branches.
    fn test_v3_pc_relative_branches() {
        let mut msg_list = MsgList::new();
        let opcodes = &mut v2_opcodes();
        let mut labels = vec![
            Label { name: "LOOP:".to_owned(), program_counter: 0x28 },
            Label { name: "FAR:".to_owned(), program_counter: 0x0004_0000 },
        ];
        // BNEI A 0 LOOP: at 0x30 -> -2 words in [20:8]; register fields in word0.
        let line = "BNEI A 0 LOOP:".to_owned();
        let w0 = u32::from_str_radix(&add_registers(opcodes, &line, "t".to_owned(), &mut msg_list, 1), 16).unwrap();
        let rel = pc_relative_bits(opcodes, &line, 0x30, &mut labels, &mut msg_list, 1, "t");
        assert_eq!(w0 | rel, 0x747F_FE00);
        // JMP.S LOOP: at 0x40 -> -6 words in [17:0].
        let rel = pc_relative_bits(opcodes, "JMP.S LOOP:", 0x40, &mut labels, &mut msg_list, 1, "t");
        assert_eq!(0x6100_0000 | rel, 0x6103_FFFA);
        assert_eq!(msg_list.number_by_type(&MessageType::Error), 0);
        // A fused branch 256 KB away is out of simm13 range: reported, no bits.
        let rel = pc_relative_bits(opcodes, "BEQ A B FAR:", 0x30, &mut labels, &mut msg_list, 1, "t");
        assert_eq!(rel, 0);
        assert_eq!(msg_list.number_by_type(&MessageType::Error), 1);
        // ...but in range for a short branch.
        let rel = pc_relative_bits(opcodes, "CALL.S FAR:", 0x30, &mut labels, &mut msg_list, 1, "t");
        assert_eq!(rel, (0x0004_0000 - 0x30) / 4);
        assert_eq!(disassemble_word(0x747F_FE00, opcodes).unwrap().0, "BNEI A 0 PC-8");
    }
}
