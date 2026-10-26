//! Encoding a source line's operands into instruction words: the register
//! and literal fields of word 0, the appended immediate words, and the
//! PC-relative branch fields that need the instruction's address.

use super::{Opcode, Operand, num_arguments, num_registers, return_opcode_struct};
use crate::asm::symbols::{Label, convert_argument};
use crate::diag::Diagnostics;
use crate::num::{parse_bits64, parse_int};

/// Parse a signed literal: decimal or `0x` hex, optionally negative.
fn parse_simm(token: &str) -> Option<i128> {
    parse_int(token).ok()
}

/// Parse a single token as a u64 immediate value.
///
/// Accepts any [`parse_bits64`] literal: hex (full 64-bit range) or signed decimal.
/// Returns `None` if the token cannot be parsed (e.g. a label name).
fn parse_imm64(token: &str) -> Option<u64> {
    parse_bits64(token).ok()
}

/// Return opcode with formatted arguments.
///
/// Returns the hex code argument from the line, converting arguments from decimal to 8 digit hex values.
/// Converts label names to hex addresses.
pub(crate) fn add_arguments(
    opcodes: &[Opcode],
    line: &str,
    msg_list: &mut Diagnostics,
    line_number: u32,
    filename: &str,
    labels: &[Label],
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
        if words_vec.len() == val_idx + 1
            && let Some(val64) = parse_imm64(words_vec[val_idx])
        {
            let lo32 = (val64 & 0xFFFF_FFFF) as u32;
            let hi32 = ((val64 >> 32) & 0xFFFF_FFFF) as u32;
            return format!("{lo32:08X}{hi32:08X}");
        }
    }

    let mut words = 0_usize;
    for (i, word) in line.split_whitespace().enumerate() {
        words += 1;
        if (i == num_registers as usize + 1) && ((num_arguments == 1) || (num_arguments == 2)) {
            arguments.push_str(&{
                let this = convert_argument(&word.to_uppercase(), msg_list, line_number, filename, labels);
                //let default = "00000000".to_owned();
                this.unwrap_or_else(|| "00000000".to_owned())
            });
        }
        if i == num_registers as usize + 2 && num_arguments == 2 {
            arguments.push_str(&{
                let this = convert_argument(&word.to_uppercase(), msg_list, line_number, filename, labels);
                this.unwrap_or_else(|| "00000000".to_owned())
            });
        }
    }
    // One warning for the line, however many extra tokens it has.
    if words > 1 + num_registers as usize + num_arguments as usize {
        msg_list.warning_at(filename, line_number, format!("Too many arguments found - \"{line}\""));
    }

    // Can't be in tarpaulin as we can't test the error by passing wrong size
    if arguments.len() != 8 * num_arguments as usize {
        #[cfg(not(tarpaulin_include))]
        msg_list.error_at(filename, line_number, format!("Incorrect argument definition - \"{line}\""));
    }
    arguments
}

/// Parse a shift/bit count token (embedded `N` field): decimal or `0x` hex.
///
/// Returns `None` for anything that is not a literal; the caller checks that
/// the count fits the 6-bit field.
fn parse_count(token: &str) -> Option<i128> {
    parse_int(token).ok()
}

/// Updates opcode with register.
///
/// Builds instruction word 0 (as an 8-hex string) from the opcode template and
/// the register/count operands on the line.  For v2 opcodes (those carrying an
/// [`Operand`] layout) the register fields are packed numerically per
/// `doc/ISA_ENCODING_V2.md` (`rd<<8 | rs1<<4 | rs2`, in-place `rd==rs1`, embedded
/// `N<<15`).  Opcodes without a layout fall back to the legacy trailing-nibble
/// scheme (used by `.vh`-parsed tables).
pub(crate) fn add_registers(opcodes: &[Opcode], line: &str, filename: &str, msg_list: &mut Diagnostics, line_number: u32) -> String {
    let Some(opcode) = return_opcode_struct(&line.to_uppercase(), opcodes) else {
        msg_list.error_at(filename, line_number, format!("Opcode not found - \"{line}\""));
        return "ERR     ".to_owned();
    };

    // Legacy path: opcodes with no v2 operand layout keep the trailing-nibble fill.
    if opcode.ops.is_empty() {
        return legacy_add_registers(opcode, line, filename, msg_list, line_number);
    }

    let Ok(mut word0) = u32::from_str_radix(&opcode.hex_code, 16) else {
        msg_list.error_at(filename, line_number, format!("Incorrect opcode template - \"{line}\""));
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
                let scale = 1_i128 << shift;
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
                // The N field is 6 bits: report a bigger count rather than masking it.
                match tokens.get(token_idx).map(|t| (t, parse_count(t))) {
                    Some((_, Some(count @ 0..=63))) => word0 |= (count as u32) << 15,
                    Some((token, Some(_))) => {
                        msg_list.error_at(filename, line_number, format!("Count {token} out of range 0..=63 - \"{line}\""));
                        return "ERR     ".to_owned();
                    }
                    _ => ok = false,
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
        msg_list.error_at(filename, line_number, format!("Incorrect register definition - \"{line}\""));
        return "ERR     ".to_owned();
    }
    format!("{word0:08X}")
}

/// Legacy trailing-nibble register packing for `.vh`-parsed opcodes (`hex_code`
/// carries `?` wildcards in its trailing nibbles).
fn legacy_add_registers(opcode: &Opcode, line: &str, filename: &str, msg_list: &mut Diagnostics, line_number: u32) -> String {
    let num_registers = opcode.registers;
    let mut opcode_found = opcode.hex_code.clone();

    if opcode_found.len() != 8 {
        msg_list.error_at(filename, line_number, format!("Incorrect register definition - \"{line}\""));
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
        msg_list.error_at(filename, line_number, format!("Incorrect register definition - \"{line}\""));
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

/// ISA v3 PC-relative targets (`Rel18` short branches, `Rel13` fused
/// branches): the word-0 bits for this line, given its byte address `pc`.
/// The target token is a label (`name:`) or an absolute address. Out-of-range
/// or misaligned targets are reported as errors and yield 0.
#[allow(clippy::too_many_arguments, reason = "mirrors add_arguments' reporting context")]
pub(crate) fn pc_relative_bits(
    opcodes: &[Opcode],
    line: &str,
    pc: u32,
    labels: &[Label],
    msg_list: &mut Diagnostics,
    line_number: u32,
    filename: &str,
) -> u32 {
    let Some(opcode) = return_opcode_struct(&line.to_uppercase(), opcodes) else {
        return 0;
    };
    let tokens: Vec<&str> = line.split_whitespace().collect();
    for (i, op) in opcode.ops.iter().enumerate() {
        let (bits, shift, what) = match op {
            Operand::Rel18 => (18_u32, 0_u32, "short branch"),
            Operand::Rel13 => (13, 8, "fused branch"),
            _ => continue,
        };
        let Some(tok) = tokens.get(i + 1) else { return 0 };
        let Some(hex) = convert_argument(tok, msg_list, line_number, filename, labels) else {
            return 0;
        };
        let Ok(target) = u32::from_str_radix(&hex, 16) else { return 0 };
        let delta = i64::from(target) - i64::from(pc);
        let words = delta / 4;
        let lim = 1_i64 << (bits - 1);
        if delta % 4 != 0 || words < -lim || words >= lim {
            msg_list.error_at(
                filename,
                line_number,
                format!("{what} target out of range ({delta:+} bytes) - \"{line}\""),
            );
            return 0;
        }
        return ((words as u32) & ((1 << bits) - 1)) << shift;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::MessageType;
    use crate::isa::disasm::disassemble_word;
    use crate::isa::table::v2_opcodes;

    #[test]
    // This test is to check that the function will return correct output if the number of registers is correct
    fn test_add_registers1() {
        let mut msg_list = Diagnostics::new();
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
        let output = add_registers(opcodes, &input, "test", &mut msg_list, 1);
        assert_eq!(output, String::from("00005601"));
    }

    #[test]
    // This test is to check that the function will return an error if the number of registers is incorrect
    fn test_add_registers2() {
        let mut msg_list = Diagnostics::new();
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
        let output = add_registers(opcodes, &input, "test", &mut msg_list, 1);
        assert_eq!(output, String::from("ERR     "));
    }

    #[test]
    // This test is to check that the function will return an error if the length of the opcode is not correct
    fn test_add_registers3() {
        let mut msg_list = Diagnostics::new();
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
        let output = add_registers(opcodes, &input, "test", &mut msg_list, 1);
        assert_eq!(output, String::from("ERR     "));
    }

    #[test]
    // Test single hex argument
    fn test_add_arguments1() {
        let mut msg_list = Diagnostics::new();
        let input = String::from("PUSH 0xFFFF");
        let labels = Vec::<Label>::new();
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
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &labels);
        assert_eq!(output, String::from("0000FFFF"));
    }

    #[test]
    // Test single decimal argument
    fn test_add_arguments2() {
        let mut msg_list = Diagnostics::new();
        let input = String::from("PUSH 1234");
        let labels = Vec::<Label>::new();
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
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &labels);
        assert_eq!(output, String::from("000004D2"));
    }

    #[test]
    // Test invalid argument
    fn test_add_arguments3() {
        let mut msg_list = Diagnostics::new();
        let input = String::from("PUSH HELLO");
        let labels = Vec::<Label>::new();
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
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &labels);
        assert_eq!(output, String::from("00000000"));
    }

    #[test]
    // Test invalid second argument
    fn test_add_arguments4() {
        let mut msg_list = Diagnostics::new();
        let input = String::from("PUSH 0xF RRR");
        let labels = Vec::<Label>::new();
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
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &labels);
        assert_eq!(output, String::from("0000000F00000000"));
    }

    #[test]
    // Test two arguments
    fn test_add_arguments5() {
        let mut msg_list = Diagnostics::new();
        let input = String::from("PUSH 1 0xF");
        let labels = Vec::<Label>::new();
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
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &labels);
        assert_eq!(output, String::from("000000010000000F"));
    }

    #[test]
    // Test too many arguments
    fn test_add_arguments6() {
        let mut msg_list = Diagnostics::new();
        let input = String::from("PUSH 1 0xF");
        let labels = Vec::<Label>::new();
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
        let output = add_arguments(opcodes, &input, &mut msg_list, 1, "test", &labels);
        assert_eq!(output, String::from("00000001"));
        assert_eq!(msg_list.messages()[0].text(), "Too many arguments found - \"PUSH 1 0xF\"");
    }

    #[test]
    // Regression: one "Too many arguments" warning was pushed per extra token.
    fn test_add_arguments_extra_tokens_warn_once() {
        let mut msg_list = Diagnostics::new();
        let labels = Vec::<Label>::new();
        let opcodes = &mut v2_opcodes();
        let output = add_arguments(opcodes, "SETR A 1 2 3 4", &mut msg_list, 1, "test", &labels);
        assert_eq!(output, String::from("00000001"));
        assert_eq!(msg_list.count(MessageType::Warning), 1);
        assert_eq!(msg_list.messages().len(), 1);

        let mut msg_list = Diagnostics::new();
        add_arguments(opcodes, "SETR A 1", &mut msg_list, 1, "test", &labels);
        assert_eq!(msg_list.messages().len(), 0);
    }

    #[test]
    fn test_map_reg_to_hex() {
        assert_eq!(map_reg_to_hex("B"), "1");
        assert_eq!(map_reg_to_hex("P"), "F");
        assert_eq!(map_reg_to_hex("Z"), "X");
    }

    #[test]
    // add_registers handles three-register (RRR) instructions correctly
    fn test_add_registers_three_regs() {
        let mut msg_list = Diagnostics::new();
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
        let output = add_registers(opcodes, &input, "test", &mut msg_list, 1);
        // A=0, B=1, C=2 → "00010" + "0" + "1" + "2"
        assert_eq!(output, String::from("00010012"));
    }

    #[test]
    // Regression: a count was masked to 6 bits, so "SHLV A 64" encoded as a shift of 0.
    fn test_count_operand_range() {
        let opcodes = &mut v2_opcodes();
        let mut msg_list = Diagnostics::new();
        assert_eq!(add_registers(opcodes, "SHLV A 0", "t", &mut msg_list, 1), "50204000");
        assert_eq!(add_registers(opcodes, "SHLV A 63", "t", &mut msg_list, 1), "503FC000");
        assert_eq!(add_registers(opcodes, "SHLV A 0x3F", "t", &mut msg_list, 1), "503FC000");
        assert_eq!(msg_list.count(MessageType::Error), 0);

        assert_eq!(add_registers(opcodes, "SHLV A 64", "t", &mut msg_list, 5), "ERR     ");
        let msg = msg_list.messages().last().unwrap();
        assert_eq!(msg.text(), "Count 64 out of range 0..=63 - \"SHLV A 64\"");
        assert_eq!((msg.level() == MessageType::Error, msg.line_number()), (true, Some(5)));

        // Negative counts are out of range too, not a register error.
        assert_eq!(add_registers(opcodes, "SHLV A -1", "t", &mut msg_list, 6), "ERR     ");
        assert_eq!(msg_list.messages().last().unwrap().text(), "Count -1 out of range 0..=63 - \"SHLV A -1\"");
        assert_eq!(msg_list.count(MessageType::Error), 2);
    }

    #[test]
    // ISA v3 mnemonics encode to the same words the emulator/RTL tests use.
    fn test_v3_short_forms_assemble() {
        let mut msg_list = Diagnostics::new();
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
            let got = add_registers(opcodes, src, "test", &mut msg_list, 1);
            assert_eq!(got, want, "{src}");
        }
        assert_eq!(msg_list.count(MessageType::Error), 0);
        // Out-of-range / misaligned short immediates are rejected.
        for bad in ["SETR.S A 300", "LDIDX64A.S A B 12", "LDIDX64A.S A B 2048"] {
            let got = add_registers(opcodes, bad, "test", &mut msg_list, 1);
            assert_eq!(got, "ERR     ", "{bad}");
        }
        // Disassembly round-trips the scaled offset.
        let (text, vars) = disassemble_word(0x5AAF_F310, opcodes).unwrap();
        assert_eq!((text.as_str(), vars), ("LDIDX32_S.S D B -4", 0));
    }

    #[test]
    // ISA v3 PC-relative targets: short (simm18) and fused (simm13) branches.
    fn test_v3_pc_relative_branches() {
        let mut msg_list = Diagnostics::new();
        let opcodes = &mut v2_opcodes();
        let labels = vec![
            Label {
                name: "LOOP:".to_owned(),
                program_counter: 0x28,
            },
            Label {
                name: "FAR:".to_owned(),
                program_counter: 0x0004_0000,
            },
        ];
        // BNEI A 0 LOOP: at 0x30 -> -2 words in [20:8]; register fields in word0.
        let line = "BNEI A 0 LOOP:".to_owned();
        let w0 = u32::from_str_radix(&add_registers(opcodes, &line, "t", &mut msg_list, 1), 16).unwrap();
        let rel = pc_relative_bits(opcodes, &line, 0x30, &labels, &mut msg_list, 1, "t");
        assert_eq!(w0 | rel, 0x747F_FE00);
        // JMP.S LOOP: at 0x40 -> -6 words in [17:0].
        let rel = pc_relative_bits(opcodes, "JMP.S LOOP:", 0x40, &labels, &mut msg_list, 1, "t");
        assert_eq!(0x6100_0000 | rel, 0x6103_FFFA);
        assert_eq!(msg_list.count(MessageType::Error), 0);
        // A fused branch 256 KB away is out of simm13 range: reported, no bits.
        let rel = pc_relative_bits(opcodes, "BEQ A B FAR:", 0x30, &labels, &mut msg_list, 1, "t");
        assert_eq!(rel, 0);
        assert_eq!(msg_list.count(MessageType::Error), 1);
        // ...but in range for a short branch.
        let rel = pc_relative_bits(opcodes, "CALL.S FAR:", 0x30, &labels, &mut msg_list, 1, "t");
        assert_eq!(rel, (0x0004_0000 - 0x30) / 4);
        assert_eq!(disassemble_word(0x747F_FE00, opcodes).unwrap().0, "BNEI A 0 PC-8");
    }
}
