//! Encoding a source line's operands into instruction words: the register
//! and literal fields of word 0, the PC-relative branch fields that need the
//! instruction's address, and the appended immediate words.

use super::{Opcode, Operand, reg_index};
use crate::asm::symbols::{OperandError, SymbolTable, resolve_operand};
use crate::num::{parse_bits64, parse_int};
use std::fmt::Write as _;

/// A line's instruction words, as far as they could be encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Encoded {
    /// Instruction word 0, or `None` when its operand fields could not be
    /// encoded.
    pub word0: Option<u32>,
    /// The immediate words appended after word 0.
    pub imm: Vec<u32>,
    /// Whether the line has tokens beyond the opcode's operands, which are
    /// ignored.
    pub extra_tokens: bool,
}

impl Encoded {
    /// The words as the listing shows them: word 0 as 8 hex digits (`ERR`
    /// padded to 8 when it failed), then each immediate word.
    pub(crate) fn hex(&self) -> String {
        let mut hex = self.word0.map_or_else(|| "ERR     ".to_owned(), |word0| format!("{word0:08X}"));
        for word in &self.imm {
            let _ = write!(hex, "{word:08X}");
        }
        hex
    }
}

/// One thing wrong with a line's operands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EncodeFault {
    /// A shift/bit count (the token as written) outside the 6-bit field.
    CountRange(String),
    /// A register, count or short literal that is missing or malformed.
    Registers,
    /// An immediate or branch target with no value.
    Operand(OperandError),
    /// A branch target out of the field's reach, or not word aligned.
    BranchRange {
        /// The kind of branch.
        what: &'static str,
        /// Target minus the instruction's address, in bytes.
        delta: i64,
    },
    /// Fewer immediate tokens than the opcode has immediate words.
    Arguments,
}

impl EncodeFault {
    /// The error message for this fault on the source line `line`.
    pub(crate) fn message(&self, line: &str) -> String {
        match self {
            Self::CountRange(token) => format!("Count {token} out of range 0..=63 - \"{line}\""),
            Self::Registers => format!("Incorrect register definition - \"{line}\""),
            Self::Operand(error) => error.to_string(),
            Self::BranchRange { what, delta } => format!("{what} target out of range ({delta:+} bytes) - \"{line}\""),
            Self::Arguments => format!("Incorrect argument definition - \"{line}\""),
        }
    }
}

/// A line whose operands could not all be encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EncodeError {
    /// What is wrong, in the order the operands were encoded.
    pub faults: Vec<EncodeFault>,
    /// The words as far as they could be encoded, for the listing: an
    /// immediate with no value is 0 and a failed branch target leaves its
    /// field clear.
    pub partial: Encoded,
}

/// Parse a signed literal: decimal or `0x` hex, optionally negative.
fn parse_simm(token: &str) -> Option<i128> {
    parse_int(token).ok()
}

/// Encode the source line `line`, whose first word names `opcode`, as the
/// instruction at byte address `pc`, resolving labels in `symbols`.
///
/// Word 0 is the template with each word-0 operand packed into its field
/// per `doc/ISA_ENCODING_V2.md` (`rd<<8 | rs1<<4 | rs2`, in-place
/// `rd==rs1`, embedded `N<<15`, the ISA v3 short literals and PC-relative
/// targets); an opcode without operands is its template.  The immediate
/// words follow.  Tokens beyond the operands are ignored and flagged in
/// [`Encoded::extra_tokens`].
pub(crate) fn encode(opcode: &Opcode, line: &str, pc: u32, symbols: &SymbolTable) -> Result<Encoded, EncodeError> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut faults = Vec::new();
    let word0 = word0_fields(opcode, &tokens).map_err(|fault| faults.push(fault)).ok();
    let rel = pc_relative_bits(opcode, &tokens, pc, symbols).unwrap_or_else(|fault| {
        faults.push(fault);
        0
    });
    let (imm, extra_tokens) = immediates(opcode, &tokens, symbols, &mut faults);
    let encoded = Encoded {
        word0: word0.map(|word0| word0 | rel),
        imm,
        extra_tokens,
    };
    if faults.is_empty() {
        Ok(encoded)
    } else {
        Err(EncodeError { faults, partial: encoded })
    }
}

/// Word 0: the template with the register, count and short-literal
/// operands packed in.  PC-relative targets are [`pc_relative_bits`].
fn word0_fields(opcode: &Opcode, tokens: &[&str]) -> Result<u32, EncodeFault> {
    let mut word0 = opcode.template;
    let mut token_idx = 1_usize; // first operand token (after the mnemonic)
    let mut ok = true;
    for op in opcode.ops {
        if !op.is_word0() {
            continue; // appended words, not word 0
        }
        let token = tokens.get(token_idx).copied();
        token_idx += 1;
        let value = token.and_then(parse_simm);
        match op {
            Operand::Imm8 => match value {
                Some(v) if (-128..=255).contains(&v) => word0 |= ((v as u32) & 0xFF) << 12,
                _ => ok = false,
            },
            Operand::Off8(shift) => {
                let scale = 1_i128 << shift;
                match value {
                    Some(v) if v % scale == 0 && (-128..=127).contains(&(v / scale)) => {
                        word0 |= (((v / scale) as u32) & 0xFF) << 12;
                    }
                    _ => ok = false,
                }
            }
            Operand::Frame22 => match value {
                Some(v) if (0..(1 << 22)).contains(&v) => word0 |= v as u32,
                _ => ok = false,
            },
            Operand::Simm4 => match value {
                Some(v) if (-8..=7).contains(&v) => word0 |= (v as u32) & 0xF,
                _ => ok = false,
            },
            // The N field is 6 bits: report a bigger count rather than masking it.
            Operand::Count => match (token, value) {
                (_, Some(count @ 0..=63)) => word0 |= (count as u32) << 15,
                (Some(token), Some(_)) => return Err(EncodeFault::CountRange(token.to_owned())),
                _ => ok = false,
            },
            Operand::Rd | Operand::Rs1 | Operand::Rs2 | Operand::RdRs1 => match token.and_then(reg_index) {
                // Each register field is a nibble; RdRs1 is two of them.
                Some(n) => word0 |= (n * 0x111) & op.mask(),
                None => ok = false,
            },
            // PC-relative targets need this instruction's address.
            Operand::Rel18 | Operand::Rel13 | Operand::Imm | Operand::Imm64 => {}
        }
    }
    if ok { Ok(word0) } else { Err(EncodeFault::Registers) }
}

/// ISA v3 PC-relative targets (`Rel18` short branches, `Rel13` fused
/// branches): the word-0 bits for the target token, given the instruction's
/// byte address `pc`.  The target is a label (`name:`) or an absolute
/// address; a missing target leaves the field clear.
fn pc_relative_bits(opcode: &Opcode, tokens: &[&str], pc: u32, symbols: &SymbolTable) -> Result<u32, EncodeFault> {
    for (i, op) in opcode.ops.iter().enumerate() {
        let what = match op {
            Operand::Rel18 => "short branch",
            Operand::Rel13 => "fused branch",
            _ => continue,
        };
        let (bits, shift) = (op.mask().count_ones(), op.mask().trailing_zeros());
        let Some(token) = tokens.get(i + 1) else { return Ok(0) };
        let target = resolve_operand(token, symbols).map_err(EncodeFault::Operand)?;
        let delta = i64::from(target) - i64::from(pc);
        let words = delta / 4;
        let lim = 1_i64 << (bits - 1);
        if delta % 4 != 0 || words < -lim || words >= lim {
            return Err(EncodeFault::BranchRange { what, delta });
        }
        return Ok(((words as u32) << shift) & op.mask());
    }
    Ok(0)
}

/// The immediate words, from the tokens after the word-0 operands, and
/// whether there are tokens beyond them.  A token with no value is a fault
/// and encodes as 0; fewer tokens than words is a fault too.
fn immediates(opcode: &Opcode, tokens: &[&str], symbols: &SymbolTable, faults: &mut Vec<EncodeFault>) -> (Vec<u32>, bool) {
    let first = opcode.reg_tokens() as usize + 1;
    let count = opcode.imm_words() as usize;

    // Two words written as one 64-bit literal (e.g. SETR64): lo32 goes at
    // PC+4 and hi32 at PC+8.
    if count == 2
        && tokens.len() == first + 1
        && let Ok(value) = parse_bits64(tokens[first])
    {
        return (vec![value as u32, (value >> 32) as u32], false);
    }

    let imm: Vec<u32> = tokens
        .iter()
        .skip(first)
        .take(count)
        .map(|token| {
            resolve_operand(&token.to_uppercase(), symbols).unwrap_or_else(|error| {
                faults.push(EncodeFault::Operand(error));
                0
            })
        })
        .collect();
    if imm.len() != count {
        faults.push(EncodeFault::Arguments);
    }
    (imm, tokens.len() > first + count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::isa::Isa;
    use crate::isa::disasm::disassemble_word;
    use crate::isa::table::V2_OPCODES;

    /// Encode `line` with the v2 table at address 0x20, with no labels.
    fn enc(line: &str) -> Result<Encoded, EncodeError> {
        enc_at(line, 0x20, &SymbolTable::default())
    }

    /// Encode `line` with the v2 table at address `pc`.
    fn enc_at(line: &str, pc: u32, symbols: &SymbolTable) -> Result<Encoded, EncodeError> {
        encode(Isa::v2().find(line).unwrap(), line, pc, symbols)
    }

    /// The words of `line`, which must encode cleanly.
    fn hex(line: &str) -> String {
        enc(line).unwrap_or_else(|e| panic!("{line}: {e:?}")).hex()
    }

    /// The faults of `line`, which must fail, with their messages.
    fn faults(line: &str) -> (Encoded, Vec<String>) {
        let error = enc(line).unwrap_err();
        (error.partial, error.faults.iter().map(|fault| fault.message(line)).collect())
    }

    #[test]
    // Register operands are packed into their v2 fields (rd[11:8], rs1[7:4])
    fn test_encode_registers1() {
        let encoded = enc("COPY A B").unwrap();
        assert_eq!(
            encoded,
            Encoded {
                word0: Some(0x5400_0010),
                imm: vec![],
                extra_tokens: false
            }
        );
        assert_eq!(encoded.hex(), "54000010");
    }

    #[test]
    // A missing register operand is an error
    fn test_encode_registers2() {
        let (partial, messages) = faults("COPY A");
        assert_eq!(partial.hex(), "ERR     ");
        assert_eq!(messages, ["Incorrect register definition - \"COPY A\""]);
    }

    #[test]
    // A register name that is not A..P is an error
    fn test_encode_registers3() {
        let (partial, messages) = faults("COPY A Z");
        assert_eq!(partial.word0, None);
        assert_eq!(messages.len(), 1);
    }

    #[test]
    // Test single hex argument
    fn test_encode_arguments1() {
        assert_eq!(hex("PUSHV 0xFFFF"), "A44000000000FFFF");
    }

    #[test]
    // Test single decimal argument
    fn test_encode_arguments2() {
        assert_eq!(enc("PUSHV 1234").unwrap().imm, [0x4D2]);
    }

    #[test]
    // An argument with no value is an error and encodes as 0
    fn test_encode_arguments3() {
        let (partial, messages) = faults("PUSHV HELLO");
        assert_eq!(partial.hex(), "A440000000000000");
        assert_eq!(messages, ["Decimal value HELLO incorrect"]);
    }

    #[test]
    // Test invalid second argument
    fn test_encode_arguments4() {
        let (partial, messages) = faults("PUSHV64 0xF RRR");
        assert_eq!(partial.imm, [0xF, 0]);
        assert_eq!(messages, ["Decimal value RRR incorrect"]);
    }

    #[test]
    // Test two arguments, and one 64-bit literal split into lo32 then hi32
    fn test_encode_arguments5() {
        assert_eq!(enc("PUSHV64 1 0xF").unwrap().imm, [1, 0xF]);
        assert_eq!(hex("SETR64 C 0x1122334455667788"), "CBC002005566778811223344");
    }

    #[test]
    // Too many arguments are ignored, and flagged for a warning
    fn test_encode_arguments6() {
        let encoded = enc("PUSHV 1 0xF").unwrap();
        assert_eq!((encoded.imm.as_slice(), encoded.extra_tokens), (&[1][..], true));
    }

    #[test]
    // Too few arguments are an error, and the listing shows the words there are
    fn test_encode_arguments_missing() {
        let (partial, messages) = faults("SETR A");
        assert_eq!(partial.hex(), "8BD00000");
        assert_eq!(messages, ["Incorrect argument definition - \"SETR A\""]);
        let (partial, _) = faults("BDEP A B C");
        assert_eq!(partial.hex(), "93400012");
    }

    #[test]
    // Regression: one "Too many arguments" warning was pushed per extra token.
    fn test_encode_extra_tokens_flagged_once() {
        let encoded = enc("SETR A 1 2 3 4").unwrap();
        assert_eq!((encoded.hex().as_str(), encoded.extra_tokens), ("8BD0000000000001", true));
        assert!(!enc("SETR A 1").unwrap().extra_tokens);
    }

    #[test]
    // Each fault of a line is reported, in operand order
    fn test_encode_reports_every_fault() {
        let labels = SymbolTable::default();
        let line = "BDEP A Z C nowhere: extra";
        let error = enc_at(line, 0x20, &labels).unwrap_err();
        let messages: Vec<String> = error.faults.iter().map(|fault| fault.message(line)).collect();
        assert_eq!(
            messages,
            [
                "Incorrect register definition - \"BDEP A Z C nowhere: extra\"",
                "Label NOWHERE: not found"
            ]
        );
        assert_eq!(error.partial.hex(), "ERR     00000000");
        assert!(error.partial.extra_tokens);
    }

    #[test]
    // Every opcode without operands (NOP, HALT, RET, IRET, ...) encodes to its
    // template alone, and disassembles back to its name.
    fn test_zero_operand_opcodes_encode_to_their_template() {
        let zero_operand: Vec<&Opcode> = V2_OPCODES.iter().filter(|o| o.ops.is_empty()).collect();
        let names: Vec<&str> = zero_operand.iter().map(|o| o.name).collect();
        assert_eq!(names, ["LEAVE", "LEAVERET", "RET", "IRET", "NOP", "HALT", "WAIT", "RESET", "TRAP"]);
        for opcode in zero_operand {
            let line = opcode.name;
            let encoded = enc(line).unwrap();
            assert_eq!((encoded.word0, encoded.imm.len()), (Some(opcode.template), 0), "{line}");
            assert_eq!(disassemble_word(opcode.template, V2_OPCODES), Some((line.to_owned(), 0)), "{line}");
        }
        // The encodings themselves, as the listing and the emulator see them.
        let words: Vec<String> = ["NOP", "HALT", "WAIT", "RESET", "TRAP", "RET", "IRET", "LEAVE", "LEAVERET", "nop"]
            .into_iter()
            .map(hex)
            .collect();
        assert_eq!(
            words,
            [
                "6C000000", "6C010000", "6C020000", "6C030000", "6C040000", "65800000", "65C00000", "66400000", "66800000", "6C000000"
            ]
        );
    }

    #[test]
    // Three-register (RRR) instructions are encoded correctly
    fn test_encode_three_regs() {
        // rd=A(0)<<8 | rs1=B(1)<<4 | rs2=C(2)
        assert_eq!(hex("ADDR A B C"), "44200012");
    }

    #[test]
    // Regression: a count was masked to 6 bits, so "SHLV A 64" encoded as a shift of 0.
    fn test_count_operand_range() {
        assert_eq!(hex("SHLV A 0"), "50204000");
        assert_eq!(hex("SHLV A 63"), "503FC000");
        assert_eq!(hex("SHLV A 0x3F"), "503FC000");

        let (partial, messages) = faults("SHLV A 64");
        assert_eq!(partial.hex(), "ERR     ");
        assert_eq!(messages, ["Count 64 out of range 0..=63 - \"SHLV A 64\""]);

        // Negative counts are out of range too, not a register error.
        assert_eq!(faults("SHLV A -1").1, ["Count -1 out of range 0..=63 - \"SHLV A -1\""]);
        // A bad register before a bad count reports only the count.
        assert_eq!(faults("SHLV Z 64").1, ["Count 64 out of range 0..=63 - \"SHLV Z 64\""]);
    }

    #[test]
    // ISA v3 mnemonics encode to the same words the emulator/RTL tests use.
    fn test_v3_short_forms_assemble() {
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
            assert_eq!(hex(src), want, "{src}");
        }
        // Out-of-range / misaligned short immediates are rejected.
        for bad in ["SETR.S A 300", "LDIDX64A.S A B 12", "LDIDX64A.S A B 2048"] {
            assert_eq!(faults(bad).0.hex(), "ERR     ", "{bad}");
        }
        // Disassembly round-trips the scaled offset.
        let (text, vars) = disassemble_word(0x5AAF_F310, V2_OPCODES).unwrap();
        assert_eq!((text.as_str(), vars), ("LDIDX32_S.S D B -4", 0));
    }

    #[test]
    // ISA v3 PC-relative targets: short (simm18) and fused (simm13) branches.
    fn test_v3_pc_relative_branches() {
        let labels = SymbolTable::from_pairs(&[("LOOP:", 0x28), ("FAR:", 0x0004_0000)]);
        let word0 = |line: &str, pc| enc_at(line, pc, &labels).map(|e| e.word0);
        // BNEI A 0 LOOP: at 0x30 -> -2 words in [20:8]; register fields in word0.
        assert_eq!(word0("BNEI A 0 LOOP:", 0x30), Ok(Some(0x747F_FE00)));
        // JMP.S LOOP: at 0x40 -> -6 words in [17:0].
        assert_eq!(word0("JMP.S LOOP:", 0x40), Ok(Some(0x6103_FFFA)));
        // A fused branch 256 KB away is out of simm13 range: reported, no bits.
        let error = enc_at("BEQ A B FAR:", 0x30, &labels).unwrap_err();
        assert_eq!(error.partial.word0, Some(0x7400_0001));
        assert_eq!(
            error.faults.iter().map(|f| f.message("BEQ A B FAR:")).collect::<Vec<_>>(),
            ["fused branch target out of range (+262096 bytes) - \"BEQ A B FAR:\""]
        );
        // ...but in range for a short branch.
        assert_eq!(word0("CALL.S FAR:", 0x30), Ok(Some(0x6300_0000 | ((0x0004_0000 - 0x30) / 4))));
        // An undefined target is reported; a missing one leaves the field clear.
        let error = enc_at("JMP.S NOWHERE:", 0x30, &labels).unwrap_err();
        assert_eq!(error.faults, [EncodeFault::Operand(OperandError::UnknownLabel("NOWHERE:".to_owned()))]);
        assert_eq!(word0("JMP.S", 0x30), Ok(Some(0x6100_0000)));
        assert_eq!(disassemble_word(0x747F_FE00, V2_OPCODES).unwrap().0, "BNEI A 0 PC-8");
    }
}
