//! Encoding a source line's operands into instruction words: the register
//! and literal fields of word 0, the PC-relative branch fields that need the
//! instruction's address, and the appended immediate words.

use super::{Opcode, Operand, reg_index};
use crate::asm::symbols::{OperandError, SymbolTable, resolve_operand};
use crate::num::{parse_bits64, parse_int};
use std::ops::RangeInclusive;

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
    /// The instruction words, word 0 first, or `None` when word 0 could not
    /// be encoded.
    pub(crate) fn into_words(self) -> Option<Vec<u32>> {
        let word0 = self.word0?;
        Some(std::iter::once(word0).chain(self.imm).collect())
    }

    /// The words as a listing would show them, for tests: word 0 as 8 hex
    /// digits (`ERR` padded to 8 when it failed), then each immediate word.
    #[cfg(test)]
    pub(crate) fn hex(&self) -> String {
        use std::fmt::Write as _;
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
    /// A short-form 8-bit immediate (the token as written) outside the
    /// values its extension can give.
    Imm8Range(String, RangeInclusive<i128>),
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
            Self::Imm8Range(token, range) => {
                format!("Immediate {token} out of range {}..={} - \"{line}\"", range.start(), range.end())
            }
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
/// `rd==rs1` or a separate `rs1` ([`Opcode::layout`]), embedded `N<<15`,
/// the ISA v3 short literals and PC-relative targets); an opcode without
/// operands is its template.  The immediate
/// words follow.  Tokens beyond the operands are ignored and flagged in
/// [`Encoded::extra_tokens`].
pub(crate) fn encode(opcode: &Opcode, line: &str, pc: u32, symbols: &SymbolTable) -> Result<Encoded, EncodeError> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let ops = opcode.layout(&tokens);
    let mut faults = Vec::new();
    let word0 = word0_fields(opcode, &ops, &tokens).map_err(|fault| faults.push(fault)).ok();
    let rel = pc_relative_bits(&ops, &tokens, pc, symbols).unwrap_or_else(|fault| {
        faults.push(fault);
        0
    });
    let (imm, extra_tokens) = immediates(&ops, &tokens, pc, symbols, &mut faults);
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
/// operands of the line's layout `ops` packed in.  PC-relative targets are
/// [`pc_relative_bits`].
fn word0_fields(opcode: &Opcode, ops: &[Operand], tokens: &[&str]) -> Result<u32, EncodeFault> {
    let mut word0 = opcode.template;
    let mut token_idx = 1_usize; // first operand token (after the mnemonic)
    let mut ok = true;
    for op in ops {
        if !op.is_word0() {
            continue; // appended words, not word 0
        }
        let token = tokens.get(token_idx).copied();
        token_idx += 1;
        let value = token.and_then(parse_simm);
        match op {
            // Report a value the byte's extension cannot give rather than
            // letting it wrap (SETR.S A 200 would set -56).
            Operand::Imm8 => match (token, value) {
                (_, Some(v)) if opcode.imm8_range().contains(&v) => word0 |= ((v as u32) & 0xFF) << 12,
                (Some(token), Some(_)) => return Err(EncodeFault::Imm8Range(token.to_owned(), opcode.imm8_range())),
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
            Operand::Cond => match value.and_then(|v| u32::try_from(v).ok()).and_then(Operand::cond_bits) {
                Some(bits) => word0 |= bits,
                None => ok = false,
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
            Operand::Rel18 | Operand::Rel13 | Operand::Rel32 | Operand::Imm | Operand::Imm64 => {}
        }
    }
    if ok { Ok(word0) } else { Err(EncodeFault::Registers) }
}

/// ISA v3 PC-relative targets (`Rel18` short branches, `Rel13` fused
/// branches): the word-0 bits for the target token, given the instruction's
/// byte address `pc`.  The target is a label (`name:`) or an absolute
/// address; a missing target leaves the field clear.
fn pc_relative_bits(ops: &[Operand], tokens: &[&str], pc: u32, symbols: &SymbolTable) -> Result<u32, EncodeFault> {
    for (i, op) in ops.iter().enumerate() {
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
/// and encodes as 0; fewer tokens than words is a fault too.  A
/// [`Operand::Rel32`] target becomes its byte displacement from `pc`, which
/// reaches any 32-bit address.
fn immediates(ops: &[Operand], tokens: &[&str], pc: u32, symbols: &SymbolTable, faults: &mut Vec<EncodeFault>) -> (Vec<u32>, bool) {
    let first = ops.iter().filter(|op| op.is_word0()).count() + 1;
    let count = ops.iter().map(|op| op.imm_words()).sum::<u32>() as usize;

    // Two words written as one 64-bit literal (e.g. SETR64): lo32 goes at
    // PC+4 and hi32 at PC+8.
    if count == 2
        && tokens.len() == first + 1
        && let Ok(value) = parse_bits64(tokens[first])
    {
        return (vec![value as u32, (value >> 32) as u32], false);
    }

    let base = if ops.contains(&Operand::Rel32) { pc } else { 0 };
    let imm: Vec<u32> = tokens
        .iter()
        .skip(first)
        .take(count)
        .map(|token| {
            resolve_operand(&token.to_uppercase(), symbols).map_or_else(
                |error| {
                    faults.push(EncodeFault::Operand(error));
                    0
                },
                |value| value.wrapping_sub(base),
            )
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
    use crate::isa::disasm::Disassembler;
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
            assert_eq!(Disassembler::new(V2_OPCODES).word(opcode.template), Some((line.to_owned(), 0)), "{line}");
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
        let (text, vars) = Disassembler::new(V2_OPCODES).word(0x5AAF_F310).unwrap();
        assert_eq!((text.as_str(), vars), ("LDIDX32_S.S D B -4", 0));
    }

    #[test]
    // The reg+reg (MODE=11) loads and stores encode to the words LLVM emits
    // (`llvm-mc -show-encoding`: `ldidx8r r1, r2, r3` is 0x58600123), are one
    // word with the value register in rd, base in rs1 and offset in rs2, and
    // disassemble back to their source line.
    fn test_reg_offset_loads_and_stores_assemble() {
        // (source, word, SIZE [25:24], SGN [23], store)
        let cases = [
            ("LDIDX8R B C D", 0x5860_0123, 0, false, false),
            ("LDIDX16R B C D", 0x5960_0123, 1, false, false),
            ("LDIDX32R B C D", 0x5A60_0123, 2, false, false),
            ("LDIDX64R B C D", 0x5B60_0123, 3, false, false),
            ("LDIDX8SR B C D", 0x58E0_0123, 0, true, false),
            ("LDIDX16SR B C D", 0x59E0_0123, 1, true, false),
            ("LDIDX32SR B C D", 0x5AE0_0123, 2, true, false),
            ("STIDX8R B C D", 0x5C60_0123, 0, false, true),
            ("STIDX16R B C D", 0x5D60_0123, 1, false, true),
            ("STIDX32R B C D", 0x5E60_0123, 2, false, true),
            ("STIDX64R B C D", 0x5F60_0123, 3, false, true),
        ];
        let disassembler = Disassembler::new(V2_OPCODES);
        for (src, word, size, sgn, store) in cases {
            let encoded = enc(src).unwrap();
            assert_eq!((encoded.word0, encoded.imm.len()), (Some(word), 0), "{src}");
            // LEN 01, class 6 (load) or 7 (store), MODE 11, A 0 and [19:12]
            // zero: the shape pipeline_core.sv decodes as a reg+reg access.
            assert_eq!(word >> 30, 1, "{src}");
            assert_eq!((word >> 26) & 0xF, if store { 7 } else { 6 }, "{src}");
            assert_eq!(((word >> 24) & 3, (word >> 23) & 1 == 1), (size, sgn), "{src}");
            assert_eq!(((word >> 21) & 3, (word >> 20) & 1, (word >> 12) & 0xFF), (3, 0, 0), "{src}");
            assert_eq!(disassembler.word(word), Some((src.to_owned(), 0)), "{src}");
        }
        // Lower case, as LLVM prints them, assembles too.
        assert_eq!(hex("ldidx16sr p a o"), "59E00F0E");
    }

    #[test]
    // The absolute (MODE=10) loads and stores encode to the words LLVM emits
    // (`llvm-mc -show-encoding`: `ldabs8 r1, 4026597377` is 0x98400100,
    // 0xF0010001), are two words with the value register in rd, rs1 and rs2
    // zero and the address in word 1, and disassemble back to their name.
    fn test_absolute_loads_and_stores_assemble() {
        // (source, word 0, SIZE [25:24], SGN [23], store)
        let cases = [
            ("LDABS8 B 0xF0010001", 0x9840_0100, 0, false, false),
            ("LDABS16 B 0xF0010002", 0x9940_0100, 1, false, false),
            ("LDABS32 B 0xF0010004", 0x9A40_0100, 2, false, false),
            ("LDABS8S B 0xF0010001", 0x98C0_0100, 0, true, false),
            ("LDABS16S B 0xF0010002", 0x99C0_0100, 1, true, false),
            ("LDABS32S B 0xF0010004", 0x9AC0_0100, 2, true, false),
            ("STABS8 B 0xF0010001", 0x9C40_0100, 0, false, true),
            ("STABS16 B 0xF0010002", 0x9D40_0100, 1, false, true),
            ("STABS32 B 0xF0010004", 0x9E40_0100, 2, false, true),
        ];
        let disassembler = Disassembler::new(V2_OPCODES);
        for (src, word, size, sgn, store) in cases {
            let encoded = enc(src).unwrap();
            let addr = u32::from_str_radix(&src[src.len() - 8..], 16).unwrap();
            assert_eq!((encoded.word0, encoded.imm), (Some(word), vec![addr]), "{src}");
            // LEN 10, class 6 (load) or 7 (store), MODE 10, A 0, and every
            // field but SIZE/SGN and rd zero: the shape pipeline_core.sv
            // decodes as an absolute access.
            assert_eq!(word >> 30, 2, "{src}");
            assert_eq!((word >> 26) & 0xF, if store { 7 } else { 6 }, "{src}");
            assert_eq!(((word >> 24) & 3, (word >> 23) & 1 == 1), (size, sgn), "{src}");
            assert_eq!(((word >> 21) & 3, (word >> 20) & 1, word & 0xF_F0FF), (2, 0, 0), "{src}");
            let name = src.split(' ').take(2).collect::<Vec<_>>().join(" ");
            assert_eq!(disassembler.word(word), Some((name, 1)), "{src}");
        }
        // Lower case, as LLVM prints them, with a decimal address.
        assert_eq!(hex("ldabs16s d 4026597378"), "99C00300F0010002");
    }

    #[test]
    // The other forms LLVM emits that klausscc once had no name for encode to
    // the words `llvm-mc -show-encoding` gives for the same operands
    // (llvm/test/MC/KlaussCPU/encoding-coverage.s, with r1/r2/r3 as B/C/D).
    fn test_llvm_forms_assemble_to_llvm_words() {
        let cases = [
            ("SETRU B 0xF0010000", "8BC00100F0010000"),
            ("SETRU.S B 200", "4BCC8100"),
            ("ANDI B C -4096", "89100120FFFFF000"),
            ("ANDI.S B C -8", "491F8120"),
            ("ORI B C -129", "89500120FFFFFF7F"),
            ("ORI.S B C -128", "49580120"),
            ("XORI B C -100000", "89900120FFFE7960"),
            ("XORI.S B C 127", "4997F120"),
            ("CMPRVU C 0xFFFFFFFF", "8C000020FFFFFFFF"),
            ("CMPEQV B C 5", "8C30012000000005"),
            ("CMPNEV B C -1", "8C700120FFFFFFFF"),
            ("CMPLTV B C 100000", "8CB00120000186A0"),
            ("CMPGEV B C -100000", "8CF00120FFFE7960"),
            ("CMPLEV B C 127", "8D3001200000007F"),
            ("CMPGTV B C -128", "8D700120FFFFFF80"),
            ("CMPULTV B C 2147483647", "8DB001207FFFFFFF"),
            ("CMPUGEV B C -2147483648", "8DF0012080000000"),
            ("CMPULEV B C 0", "8E30012000000000"),
            ("CMPUGTV B C 65535", "8E7001200000FFFF"),
            ("CMPEQRW B C D", "4C280123"),
            ("CMPNERW B C D", "4C680123"),
            ("CMPLTRW B C D", "4CA80123"),
            ("CMPGERW B C D", "4CE80123"),
            ("CMPLERW B C D", "4D280123"),
            ("CMPGTRW B C D", "4D680123"),
            ("CMPULTRW B C D", "4DA80123"),
            ("CMPUGERW B C D", "4DE80123"),
            ("CMPULERW B C D", "4E280123"),
            ("CMPUGTRW B C D", "4E680123"),
            ("CMPEQVW B C -3", "8C380120FFFFFFFD"),
            ("CMPNEVW B C 7", "8C78012000000007"),
            ("CMPLTVW B C 1000", "8CB80120000003E8"),
            ("CMPGEVW B C -1000", "8CF80120FFFFFC18"),
            ("CMPLEVW B C 1", "8D38012000000001"),
            ("CMPGTVW B C -2147483648", "8D78012080000000"),
            ("CMPULTVW B C 2147483647", "8DB801207FFFFFFF"),
            ("CMPUGEVW B C 255", "8DF80120000000FF"),
            ("CMPULEVW B C -1", "8E380120FFFFFFFF"),
            ("CMPUGTVW B C 12345", "8E78012000003039"),
        ];
        for (src, want) in cases {
            assert_eq!(hex(src), want, "{src}");
        }
        // `jmprel 64` .. `callrel -64` at 0x20: the target is an address or
        // label, and word 1 its displacement from the instruction.
        let jumps = [
            ("JMPREL", "A1000000"),
            ("JMPZREL", "A1080000"),
            ("JMPNZREL", "A10C0000"),
            ("JMPCREL", "A1100000"),
            ("JMPNCREL", "A1140000"),
            ("JMPSREL", "A1200000"),
            ("JMPNSREL", "A1240000"),
            ("JMPLTREL", "A1280000"),
            ("JMPGEREL", "A12C0000"),
            ("JMPLEREL", "A1300000"),
            ("JMPGTREL", "A1340000"),
            ("JMPULTREL", "A1380000"),
            ("JMPUGEREL", "A13C0000"),
            ("JMPULEREL", "A1400000"),
            ("JMPUGTREL", "A1440000"),
            ("JMPEREL", "A1480000"),
            ("JMPNEREL", "A14C0000"),
        ];
        for (name, word0) in jumps {
            assert_eq!(hex(&format!("{name} 0x60")), format!("{word0}00000040"), "{name}");
        }
        assert_eq!(hex("CALLREL 0x0"), "A3000000FFFFFFE0");
    }

    #[test]
    // The in-place opcodes take LLVM's 3-operand form, rd then a separate
    // rs1, and encode to the words `llvm-mc -show-encoding` gives for it
    // (r1/r2 as B/C); the 2-operand form is rd == rs1, as it always was.
    fn test_three_operand_in_place_forms() {
        let cases = [
            // addv r1, r2, 500 / addv r1, r2, 5 (short form)
            ("ADDV B C 500", "88200120000001F4"),
            ("ADDV.S B C 5", "48205120"),
            ("ADDV B 500", "88200110000001F4"),
            ("ADDV B B 500", "88200110000001F4"),
            ("ADDV.S B 5", "48205110"),
            ("MINUSV C B 7", "8860021000000007"),
            ("ANDV.S C B 0xFF", "490FF210"),
            ("mulv b c -5", "A8800120FFFFFFFB"),
            ("DIVV B C 3", "A980012000000003"),
            ("MODV B C 3", "AA80012000000003"),
            // shlv r1, r2, 3 / bset r1, r2, 3 / rorv r1, r2, 1 / shrav r1, r2, 1
            ("SHLV B C 3", "5021C120"),
            ("SHLV B 3", "5021C110"),
            ("BSET B C 3", "52218120"),
            ("RORV B C 1", "5120C120"),
            ("SHRAV B C 1", "50A0C120"),
            // negr r1, r2 / shrar r1, r2 / rolr1 r1, r2 / shlar r1, r2
            ("NEGR B C", "54480120"),
            ("NEGR B", "54480110"),
            ("SHRAR B C", "50A08120"),
            ("ROLR1 B C", "50E0C120"),
            ("SHLAR B C", "50208120"),
            ("CLZ P A", "56400F00"),
        ];
        for (src, want) in cases {
            let encoded = enc(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
            assert_eq!((encoded.hex().as_str(), encoded.extra_tokens), (want, false), "{src}");
        }
        // A register name is rs1 only on a line longer than the 2-operand
        // form: `ADDV A B` still adds the label B, as it always did.
        let labels = SymbolTable::from_pairs(&[("B:", 0x40)]);
        assert_eq!(enc_at("ADDV A B", 0x20, &labels).map(|e| e.hex()), Ok("8820000000000040".to_owned()));
        assert_eq!(enc_at("ADDV A B 5", 0x20, &labels).map(|e| e.hex()), Ok("8820001000000005".to_owned()));
        // A literal after rd is the 2-operand form with an extra token.
        let encoded = enc("ADDV A 5 6").unwrap();
        assert_eq!((encoded.hex().as_str(), encoded.extra_tokens), ("8820000000000005", true));
        let encoded = enc("NEGR A B C").unwrap();
        assert_eq!((encoded.hex().as_str(), encoded.extra_tokens), ("54480010", true));
        // A 3-operand line missing its immediate or count is an error.
        assert_eq!(faults("SHLV A B C").1, ["Incorrect register definition - \"SHLV A B C\""]);
        assert_eq!(faults("ADDV A B C").0.hex(), "8820001000000000");
    }

    #[test]
    // LLVM's names for klausscc opcodes assemble to the same words
    // (`llvm-mc -show-encoding`), and LLVM's `fbr`/`fbri` to the fused branch
    // their condition names.
    fn test_llvm_names_assemble() {
        let cases = [
            ("LCD 5", "B200000000000005"),
            ("LDIDX64U B C 8000", "9B20012000001F40"),
            ("LDIDX64U.S B C 8", "5B201120"),
            ("STIDX64U B C 8000", "9F20012000001F40"),
            ("STIDX64U.S B C 8", "5F201120"),
            ("LDIDX8S B C 1", "98A0012000000001"),
            ("LDIDX16S.S B C 2", "59A01120"),
            ("LDIDX32S.S B C 4", "5AA01120"),
        ];
        for (src, want) in cases {
            assert_eq!(hex(src), want, "{src}");
            // ... the same words as klausscc's own name for the opcode.
            let own = src
                .replace("LCD", "LCDRST")
                .replace("64U", "64")
                .replace("8S", "8_S")
                .replace("16S", "16_S")
                .replace("32S", "32_S");
            assert_eq!(hex(&own), want, "{own}");
        }
        // fbr r1, r2, 12, . / fbri r1, -3, 4, . (target the instruction itself)
        assert_eq!(hex("FBR B C 12 0x20"), "76400012");
        assert_eq!(hex("FBRI B -3 4 0x20"), "7620001D");
        assert_eq!(hex("FBR B C 12 0x20"), hex("BUGT B C 0x20"));
        assert_eq!(faults("FBR B C 5 0x20").1, ["Incorrect register definition - \"FBR B C 5 0x20\""]);
    }

    #[test]
    // A 2-word PC-relative target is resolved against the instruction's
    // address, reaches the whole 32-bit space, and disassembles as PC±n.
    fn test_rel32_targets() {
        let labels = SymbolTable::from_pairs(&[("LOOP:", 0x28), ("FAR:", 0xF000_0000)]);
        let words = |line: &str, pc| enc_at(line, pc, &labels).map(|e| e.hex());
        assert_eq!(words("JMPNEREL LOOP:", 0x40), Ok("A14C0000FFFFFFE8".to_owned()));
        assert_eq!(words("CALLREL FAR:", 0x20), Ok("A3000000EFFFFFE0".to_owned()));
        let error = enc_at("JMPREL NOWHERE:", 0x30, &labels).unwrap_err();
        assert_eq!(error.faults, [EncodeFault::Operand(OperandError::UnknownLabel("NOWHERE:".to_owned()))]);
        let disassembler = Disassembler::new(V2_OPCODES);
        let bytes: Vec<u8> = [0xA14C_0000_u32, 0xFFFF_FFE8, 0xA300_0000, 0x0000_0120]
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        let shown: Vec<String> = disassembler
            .flat_to_pass2(&bytes, 0x40)
            .into_iter()
            .map(|line| line.input_text_line)
            .collect();
        assert_eq!(shown, ["JMPNEREL PC-24", "CALLREL PC+288"]);
    }

    #[test]
    // Regression: every imm8 .S form took -128..=255, so SETR.S A 200 set -56
    // and ADDV.S A -1 added 255. Each now takes only the values its
    // extension can give (pipeline_core.sv:761-762).
    fn test_v3_imm8_range_follows_extension() {
        // (form with its register operands, sign-extended, word 0 with those
        // registers: A is 0, B is 1)
        let forms = [
            ("SETR.S A", true, 0x4BD0_0000_u32),
            ("ADDI.S A B", true, 0x4830_0010),
            ("ADDV.S A", false, 0x4820_0000),
            ("MINUSV.S A", false, 0x4860_0000),
            ("ANDV.S A", false, 0x4900_0000),
            ("ORV.S A", false, 0x4940_0000),
            ("XORV.S A", false, 0x4980_0000),
            ("SETRU.S A", false, 0x4BC0_0000),
            ("ANDI.S A B", true, 0x4910_0010),
            ("ORI.S A B", true, 0x4950_0010),
            ("XORI.S A B", true, 0x4990_0010),
            ("CMPRV.S B", true, 0x4C10_0010),
        ];
        let imm8_forms: Vec<&str> = V2_OPCODES.iter().filter(|o| o.ops.contains(&Operand::Imm8)).map(|o| o.name).collect();
        let listed: Vec<&str> = forms.iter().map(|(form, _, _)| form.split(' ').next().unwrap()).collect();
        assert_eq!(imm8_forms, listed, "every imm8 form is checked");
        for (form, signed, template) in forms {
            let (lo, hi, below, above) = if signed { (-128, 127, -129, 128) } else { (0, 255, -1, 256) };
            for (value, byte) in [(lo, lo & 0xFF), (hi, hi & 0xFF)] {
                let line = format!("{form} {value}");
                let want = template | ((byte as u32) << 12);
                assert_eq!(hex(&line), format!("{want:08X}"), "{line}");
                // And it disassembles to the line it came from.
                let (text, _) = Disassembler::new(V2_OPCODES).word(want).unwrap();
                assert_eq!(text, line);
            }
            for value in [below, above] {
                let line = format!("{form} {value}");
                let (partial, messages) = faults(&line);
                assert_eq!(partial.hex(), "ERR     ", "{line}");
                assert_eq!(messages, [format!("Immediate {value} out of range {lo}..={hi} - \"{line}\"")]);
            }
        }
        // The two cases that used to wrap.
        assert_eq!(faults("SETR.S A 200").1, ["Immediate 200 out of range -128..=127 - \"SETR.S A 200\""]);
        assert_eq!(faults("ADDV.S A -1").1, ["Immediate -1 out of range 0..=255 - \"ADDV.S A -1\""]);
        assert_eq!(hex("ADDV.S A 0xFF"), "482FF000");
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
        assert_eq!(Disassembler::new(V2_OPCODES).word(0x747F_FE00).unwrap().0, "BNEI A 0 PC-8");
    }
}
