//! The assembler: `.kla` source in, addressed machine code out.
//!
//! [`assemble`] runs the whole pipeline for one file: read it (with its
//! `!include`s), drop block comments, apply source and built-in macros, give
//! every line its address ([`pass1`]), collect the labels, and encode every
//! line ([`pass2`]).  Problems in the source are diagnostics in the
//! [`Diagnostics`]; assembly carries on past them so that one run reports them
//! all and the `.code` listing can show where they are.  Callers decide what
//! a build with errors may still be used for, by checking
//! [`Diagnostics::has_errors`].
//!
//! [`line`](mod@line) holds the per-line records that flow between the passes and
//! [`lexer`] the pure text predicates used to classify a source line.  Both
//! are leaves: they import nothing else from the crate, so every other
//! assembler module can depend on them without forming a cycle.
//!
//! The other stages each have a module: `source` reads the files, `macros`
//! expands macros, `classify` tells what a line holds, `data` encodes data
//! definitions and `symbols` holds the labels.

mod classify;
#[cfg(test)]
mod corpus_tests;
mod data;
pub(crate) mod lexer;
pub(crate) mod line;
pub(crate) mod macros;
mod pass1;
mod pass2;
mod source;
pub(crate) mod symbols;

use crate::diag::{Diagnostics, MessageType};
use crate::error::Error;
use crate::isa::Isa;
use crate::output::image::Image;
use line::{LineType, Pass2, SourceLine};
use macros::{Macro, apply_source_macros, expand_macros};
use pass1::get_pass1;
use pass2::get_pass2;
use source::{read_file_to_vector, remove_block_comments};
use symbols::get_labels;

/// One assembled source file.
#[derive(Debug)]
pub(crate) struct Assembly {
    /// Every line after macro expansion, with its address and machine code.
    pub(crate) lines: Vec<Pass2>,
}

impl Assembly {
    /// The addresses of the `_start` lines, in source order.
    fn starts(&self) -> impl Iterator<Item = u32> + '_ {
        self.lines
            .iter()
            .filter(|line| line.line_type == LineType::Start)
            .map(|line| line.program_counter)
    }

    /// The `.kbt` wire-format image of the program, or `None` (reported) when
    /// it does not have exactly one `_start` or is not whole 32-bit words.
    pub(crate) fn to_kbt(&self, msg_list: &mut Diagnostics) -> Option<String> {
        let mut starts = self.starts();
        let entry = match (starts.next(), starts.next()) {
            (Some(entry), None) => entry,
            (None, _) => {
                msg_list.error("No start address found");
                return None;
            }
            (Some(_), Some(_)) => {
                msg_list.error("Multiple start addresses found");
                return None;
            }
        };
        Image::from_lines(&self.lines, entry).to_kbt(msg_list)
    }

    /// The program laid out in memory, or `None` when it has no `_start`.
    ///
    /// It starts at the last `_start`; only [`to_kbt`](Self::to_kbt) insists
    /// on exactly one.
    pub(crate) fn image(&self) -> Option<Image> {
        self.starts().last().map(|entry| Image::from_lines(&self.lines, entry))
    }
}

/// Assemble the source file `input_file_name`.
///
/// Fails only when the file (or one it includes) cannot be read, which is
/// reported first, with [`Error::Assembly`] counting the errors so far.
/// Otherwise the assembly is returned even when it has errors; see the
/// [module documentation](self).
pub(crate) fn assemble(input_file_name: &str, isa: &Isa, macro_list: &[Macro], msg_list: &mut Diagnostics) -> Result<Assembly, Error> {
    msg_list.info(format!("Input file is {input_file_name}"));
    let Some(input_list) = read_file_to_vector(input_file_name, msg_list) else {
        return Err(Error::Assembly(msg_list.count(MessageType::Error).max(1)));
    };
    Ok(assemble_lines(input_list, isa, macro_list, msg_list))
}

/// Assemble source lines that have already been read.
///
/// The lines' own `!macro` definitions are added to a copy of `macro_list`.
pub(crate) fn assemble_lines(input_list: Vec<SourceLine>, isa: &Isa, macro_list: &[Macro], msg_list: &mut Diagnostics) -> Assembly {
    let input_list = remove_block_comments(input_list, msg_list);

    // Fold in any `!macro` / `!macros` definitions from the source, then expand.
    let mut macro_list = macro_list.to_vec();
    let input_list = apply_source_macros(input_list, &mut macro_list, msg_list);
    let pass0 = expand_macros(msg_list, input_list, &macro_list);

    // Pass 1 to get line numbers and labels
    let pass1 = get_pass1(msg_list, pass0, isa);
    let labels = get_labels(&pass1, msg_list);

    // Pass 2 to create the output
    Assembly {
        lines: get_pass2(msg_list, pass1, isa, &labels),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asm::line::Emitted;
    use crate::asm::macros::{builtin_macros, expand_embedded_macros};

    /// Assemble `source` (as `test.kla`) with the built-in v2 table, without file I/O.
    fn assemble_text(source: &str) -> (Vec<Pass2>, Diagnostics) {
        let mut msg_list = Diagnostics::new();
        let isa = Isa::v2();
        let macro_list = expand_embedded_macros(builtin_macros(), &mut msg_list);
        let input: Vec<SourceLine> = source
            .lines()
            .enumerate()
            .map(|(i, line)| SourceLine {
                file_name: "test.kla".to_owned(),
                input_text_line: line.to_owned(),
                line_counter: u32::try_from(i + 1).unwrap(),
            })
            .collect();
        let assembly = assemble_lines(input, &isa, &macro_list, &mut msg_list);
        (assembly.lines, msg_list)
    }

    /// The `(line, text)` of every message of `level`.
    fn messages_of(msg_list: &Diagnostics, level: MessageType) -> Vec<(Option<u32>, String)> {
        msg_list
            .messages()
            .iter()
            .filter(|m| m.level() == level)
            .map(|m| (m.line_number(), m.text().to_owned()))
            .collect()
    }

    #[test]
    // Regression: a string was 4 + 4N bytes, so data after one in a .data section
    // sat at 4 mod 8, and a 64-bit store to it (masked down to 8 bytes) overwrote
    // the last word of the string.
    fn test_data_after_string_stays_8_byte_aligned() {
        let (pass2, msg_list) = assemble_text("_start\nHALT\n.data\n#S \"Hello\"\n#T \"ABCDEFGHIJKL\"\n#V 1\n.text\n");
        assert_eq!(msg_list.count(MessageType::Error), 0);
        for name in ["#S", "#T", "#V"] {
            let line = pass2.iter().find(|p| p.input_text_line.starts_with(name)).unwrap();
            assert!(line.program_counter.is_multiple_of(8), "{name} at 0x{:X}", line.program_counter);
        }
    }

    #[test]
    // Regression: `.zeropad 3` only logged a checksum-length error and still
    // produced a .kbt with checksum 0; a part-word pad is now a data error.
    fn test_part_word_zeropad_is_error() {
        let (_, msg_list) = assemble_text("_start\nHALT\n.zeropad 3\n");
        assert_eq!(
            messages_of(&msg_list, MessageType::Error),
            vec![(
                Some(3),
                "Error in data definition for .zeropad 3: size \"3\" must be a multiple of 4 bytes".to_owned()
            )]
        );
        let (lines, mut msg_list) = assemble_text("_start\nHALT\n.zeropad 4\n");
        assert_eq!(msg_list.count(MessageType::Error), 0);
        assert!(Assembly { lines }.to_kbt(&mut msg_list).is_some());
    }

    #[test]
    // Regression: an undefined label was a warning, so "JMP nowhere:" assembled
    // to a jump to address 0 and the build succeeded.
    fn test_undefined_label_is_error() {
        let (_, msg_list) = assemble_text("_start\nJMP nowhere:\nHALT\n");
        assert_eq!(
            messages_of(&msg_list, MessageType::Error),
            vec![(Some(2), "Label NOWHERE: not found".to_owned())]
        );

        let (_, msg_list) = assemble_text("_start\nSETR A 12abc\nHALT\n");
        assert_eq!(
            messages_of(&msg_list, MessageType::Error),
            vec![(Some(2), "Decimal value 12ABC incorrect".to_owned())]
        );

        let (_, msg_list) = assemble_text("_start\nloop:\nJMP loop:\nJMP loop\n");
        assert_eq!(messages_of(&msg_list, MessageType::Error), vec![]);
    }

    #[test]
    // Regression: a bad .comm size was parsed with unwrap_or(0) and silently reserved nothing.
    fn test_comm_bad_size_is_error() {
        let (_, msg_list) = assemble_text("_start\nHALT\n.comm buf, 12abc, 4\n");
        assert_eq!(
            messages_of(&msg_list, MessageType::Error),
            vec![(Some(3), "Invalid size 12abc for .comm buf".to_owned())]
        );

        let (_, msg_list) = assemble_text("_start\nHALT\n.comm buf, 0x10, 4\n");
        assert_eq!(messages_of(&msg_list, MessageType::Error), vec![]);
    }

    #[test]
    // Regression: data values that do not parse are errors at their line.
    fn test_bad_data_values_are_errors() {
        let (_, msg_list) = assemble_text("_start\nHALT\n.word 12abc\n#BUF -5\n");
        let errors = messages_of(&msg_list, MessageType::Error);
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert_eq!(errors[0].0, Some(3));
        assert_eq!(errors[1].0, Some(4));
    }

    #[test]
    // Regression: a decimal SETR value that only fits 64 bits as a two's-complement
    // pattern is assembled as SETR64 with the whole 64-bit value.
    fn test_setr_upgrade_assembles_64bit_value() {
        let (pass2, msg_list) = assemble_text("_start\nSETR A 18446744073709551615\nHALT\n");
        assert_eq!(messages_of(&msg_list, MessageType::Error), vec![]);
        let setr = pass2.iter().find(|p| p.line_counter == 2).unwrap();
        let Emitted::Code(words) = &setr.emitted else {
            panic!("{:?}", setr.emitted)
        };
        assert!(words.ends_with(&[0xFFFF_FFFF, 0xFFFF_FFFF]), "{words:X?}");
    }

    #[test]
    fn assemble_reports_an_unreadable_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("missing.kla");
        let mut msg_list = Diagnostics::new();
        let (isa, macros) = (Isa::v2(), builtin_macros());
        assert!(matches!(
            assemble(&file.to_string_lossy(), &isa, &macros, &mut msg_list),
            Err(Error::Assembly(1))
        ));
        assert!(msg_list.has_errors());
    }

    #[test]
    // A build with errors is still returned in full, so the listing can show them.
    fn assemble_keeps_going_after_errors() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bad.kla");
        std::fs::write(&file, "_start\nNOTANOP A\nHALT\n").unwrap();
        let mut msg_list = Diagnostics::new();
        let (isa, macros) = (Isa::v2(), builtin_macros());
        let assembly = assemble(&file.to_string_lossy(), &isa, &macros, &mut msg_list).unwrap();
        assert!(msg_list.has_errors());
        assert_eq!(assembly.lines.len(), 3);
        assert_eq!(msg_list.messages()[0].text(), format!("Input file is {}", file.to_string_lossy()));
    }

    #[test]
    fn assembly_image_and_kbt_agree() {
        let (lines, mut msg_list) = assemble_text("_start\nSETR A 0x41\nHALT\n");
        let assembly = Assembly { lines };
        let image = assembly.image().unwrap();
        assert_eq!(image.entry, 0x20);
        assert_eq!(image.to_kbt(&mut msg_list), assembly.to_kbt(&mut msg_list));
        assert!(!msg_list.has_errors());
    }

    #[test]
    // Pass 1 gives every line the address just past the one before, so laying
    // the lines out by address is the same as laying them end to end: inline
    // data, the alignment pad and the deferred data section included.
    fn image_by_address_is_the_lines_end_to_end() {
        let source = "_start\nSETR A 0x41\n#MSG \"Hello\"\nHALT\n.data\nval:\n.word 0x1122334455667788\n.comm buf, 12\n";
        let (lines, msg_list) = assemble_text(source);
        assert_eq!(messages_of(&msg_list, MessageType::Error), vec![]);
        assert!(lines.iter().any(|line| line.file_name == "<align>"), "no alignment pad");
        let end_to_end: Vec<u8> = lines
            .iter()
            .flat_map(|line| {
                let mut bytes = vec![0; line.emitted.byte_len()];
                line.emitted.copy_to(&mut bytes);
                bytes
            })
            .collect();
        let image = Assembly { lines }.image().unwrap();
        assert_eq!(image.bytes, end_to_end);
    }

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
    // The .kbt frames the lines laid out by address, starting at _start
    fn test_to_kbt_with_start() {
        let assembly = Assembly {
            lines: vec![
                line(LineType::Start, 1, Emitted::None),
                line(LineType::Data, 0x20, Emitted::Data(vec![0x12, 0x34])),
                line(LineType::Data, 0x22, Emitted::Data(vec![0x43, 0x21])),
            ],
        };
        let mut msg_list = Diagnostics::new();
        // Word 0: heap_start=0x28 LE → "28000000"; hi32=0 → "00000000".
        // Entry pc=1 LE → "01000000".  The data bytes are sent in memory order.
        // No "Z0010" — checksum is a 32-bit LE word immediately after Z.
        // 10 × 8-char groups (8 header + "12344321" + entry); count=20.
        // sum: 0x28 → 40, bytes 12 34 43 21 → 0x21433412 → 0x2143 + 0x3412 = 21845, entry → 1: 21886.
        // chk = (21886+20)%65536 = 21906 = 0x5592; LE → "92550000".
        assert_eq!(
            assembly.to_kbt(&mut msg_list),
            Some("S28000000000000000000000000000000000000000000000000000000000000001234432101000000Z92550000X".to_owned())
        );
        assert!(msg_list.messages().is_empty());
    }

    #[test]
    // A stream that is not whole 32-bit words gets no .kbt at all, rather than
    // one with a zero checksum
    fn test_to_kbt_part_word() {
        let assembly = Assembly {
            lines: vec![
                line(LineType::Start, 0x20, Emitted::None),
                line(LineType::Data, 0x20, Emitted::Data(vec![0; 3])),
            ],
        };
        let mut msg_list = Diagnostics::new();
        assert_eq!(assembly.to_kbt(&mut msg_list), None);
        assert_eq!(
            msg_list.messages()[0].text(),
            "Program image is not a whole number of 32-bit words, length is 78 hex characters"
        );
    }

    #[test]
    // No .kbt with more than one _start; the image runs from the last
    fn test_to_kbt_multiple_starts() {
        let assembly = Assembly {
            lines: vec![
                line(LineType::Start, 0x20, Emitted::None),
                line(LineType::Start, 0x24, Emitted::None),
                line(LineType::Data, 0x20, Emitted::Data(vec![0x43, 0x21, 0, 0])),
            ],
        };
        let mut msg_list = Diagnostics::new();
        assert_eq!(assembly.to_kbt(&mut msg_list), None);
        assert_eq!(msg_list.messages()[0].text(), "Multiple start addresses found");
        assert_eq!(assembly.image().map(|image| image.entry), Some(0x24));
    }

    #[test]
    // No .kbt and no image without a _start
    fn test_to_kbt_no_start() {
        let assembly = Assembly {
            lines: vec![
                line(LineType::Comment, 0x20, Emitted::None),
                line(LineType::Data, 0x20, Emitted::Data(vec![0x12, 0x34, 0, 0])),
            ],
        };
        let mut msg_list = Diagnostics::new();
        assert_eq!(assembly.to_kbt(&mut msg_list), None);
        assert_eq!(msg_list.messages()[0].text(), "No start address found");
        assert_eq!(assembly.image(), None);
    }

    #[test]
    // The .kbt is the kbt of the same image the emulator and DDR load
    fn test_to_kbt_is_the_image_kbt() {
        let assembly = Assembly {
            lines: vec![
                line(LineType::Start, 0x20, Emitted::None),
                line(LineType::Opcode, 0x20, Emitted::Code(vec![0x8BD0_0000, 0x41])),
                line(LineType::Opcode, 0x28, Emitted::Code(vec![0x6C01_0000])),
            ],
        };
        let mut msg_list = Diagnostics::new();
        let image = assembly.image().unwrap();
        assert_eq!(image.bytes, [0x00, 0x00, 0xD0, 0x8B, 0x41, 0, 0, 0, 0x00, 0x00, 0x01, 0x6C]);
        assert_eq!(image.to_kbt(&mut msg_list), assembly.to_kbt(&mut msg_list));
        assert!(msg_list.messages().is_empty());
    }
}
