//! The assembler: `.kla` source in, addressed machine code out.
//!
//! [`assemble`] runs the whole pipeline for one file: read it (with its
//! `!include`s), drop block comments, apply source and built-in macros, give
//! every line its address ([`pass1`]), collect the labels, and encode every
//! line ([`pass2`]).  Problems in the source are diagnostics in the
//! [`MsgList`]; assembly carries on past them so that one run reports them
//! all and the `.code` listing can show where they are.  Callers decide what
//! a build with errors may still be used for, by checking
//! [`MsgList::has_errors`].
//!
//! [`line`] holds the per-line records that flow between the passes and
//! [`lexer`] the pure text predicates used to classify a source line.  Both
//! are leaves: they import nothing else from the crate, so every other
//! assembler module can depend on them without forming a cycle.

pub(crate) mod lexer;
pub(crate) mod line;
mod pass1;
mod pass2;

use crate::files::{read_file_to_vector, remove_block_comments};
use crate::image::{create_bin_string, Image};
use crate::labels::{find_duplicate_label, get_labels};
use crate::macros::{apply_source_macros, expand_macros, Macro};
use crate::messages::{MessageType, MsgList};
use crate::opcodes::Opcode;
use line::{Pass2, SourceLine};
use pass1::get_pass1;
use pass2::get_pass2;

/// One assembled source file.
#[derive(Debug)]
pub(crate) struct Assembly {
    /// Every line after macro expansion, with its address and machine code.
    pub(crate) lines: Vec<Pass2>,
}

impl Assembly {
    /// The `.kbt` wire-format image of the program, or `None` (reported) when
    /// it does not have exactly one `_start` or is not whole 32-bit words.
    pub(crate) fn to_kbt(&self, msg_list: &mut MsgList) -> Option<String> {
        create_bin_string(&self.lines, msg_list)
    }

    /// The program laid out in memory, or `None` when it has no `_start`.
    pub(crate) fn image(&self) -> Option<Image> {
        Image::from_lines(&self.lines)
    }
}

/// Assemble the source file `input_file_name`.
///
/// Returns `None` only when the file (or one it includes) cannot be read,
/// which is reported.  Otherwise the assembly is returned even when it has
/// errors; see the [module documentation](self).
pub(crate) fn assemble(input_file_name: &str, oplist: &[Opcode], macro_list: &[Macro], msg_list: &mut MsgList) -> Option<Assembly> {
    msg_list.push(format!("Input file is {input_file_name}"), None, None, MessageType::Information);
    let input_list = read_file_to_vector(input_file_name, msg_list)?;
    Some(assemble_lines(input_list, oplist, macro_list, msg_list))
}

/// Assemble source lines that have already been read.
///
/// The lines' own `!macro` definitions are added to a copy of `macro_list`.
pub(crate) fn assemble_lines(input_list: Vec<SourceLine>, oplist: &[Opcode], macro_list: &[Macro], msg_list: &mut MsgList) -> Assembly {
    let input_list = remove_block_comments(input_list, msg_list);

    // Fold in any `!macro` / `!macros` definitions from the source, then expand.
    let mut macro_list = macro_list.to_vec();
    let input_list = apply_source_macros(input_list, &mut macro_list, msg_list);
    let pass0 = expand_macros(msg_list, input_list, &mut macro_list);

    // Pass 1 to get line numbers and labels
    let pass1 = get_pass1(msg_list, pass0, oplist);
    let labels = get_labels(&pass1, msg_list);
    find_duplicate_label(&labels, msg_list);

    // Pass 2 to create the output
    Assembly {
        lines: get_pass2(msg_list, pass1, oplist, &labels),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macros::expand_embedded_macros;
    use crate::opcodes::v2_opcodes_and_macros;

    /// Assemble `source` (as `test.kla`) with the built-in v2 table, without file I/O.
    fn assemble_text(source: &str) -> (Vec<Pass2>, MsgList) {
        let mut msg_list = MsgList::new();
        let (oplist, macros_raw) = v2_opcodes_and_macros(&mut msg_list);
        let macro_list = expand_embedded_macros(macros_raw, &mut msg_list);
        let input: Vec<SourceLine> = source
            .lines()
            .enumerate()
            .map(|(i, line)| SourceLine {
                file_name: "test.kla".to_owned(),
                input_text_line: line.to_owned(),
                line_counter: u32::try_from(i + 1).unwrap(),
            })
            .collect();
        let assembly = assemble_lines(input, &oplist, &macro_list, &mut msg_list);
        (assembly.lines, msg_list)
    }

    /// The `(line, text)` of every message of `level`.
    fn messages_of(msg_list: &MsgList, level: MessageType) -> Vec<(Option<u32>, String)> {
        msg_list
            .list
            .iter()
            .filter(|m| m.level == level)
            .map(|m| (m.line_number, m.text.clone()))
            .collect()
    }

    #[test]
    // Regression: a string was 4 + 4N bytes, so data after one in a .data section
    // sat at 4 mod 8, and a 64-bit store to it (masked down to 8 bytes) overwrote
    // the last word of the string.
    fn test_data_after_string_stays_8_byte_aligned() {
        let (pass2, msg_list) = assemble_text("_start\nHALT\n.data\n#S \"Hello\"\n#T \"ABCDEFGHIJKL\"\n#V 1\n.text\n");
        assert_eq!(msg_list.number_by_type(MessageType::Error), 0);
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
        let (pass2, mut msg_list) = assemble_text("_start\nHALT\n.zeropad 4\n");
        assert_eq!(msg_list.number_by_type(MessageType::Error), 0);
        assert!(create_bin_string(&pass2, &mut msg_list).is_some());
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
        assert!(setr.opcode.ends_with("FFFFFFFFFFFFFFFF"), "{}", setr.opcode);
    }

    #[test]
    fn assemble_reports_an_unreadable_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("missing.kla");
        let mut msg_list = MsgList::new();
        let (oplist, macros) = v2_opcodes_and_macros(&mut msg_list);
        assert!(assemble(&file.to_string_lossy(), &oplist, &macros, &mut msg_list).is_none());
        assert!(msg_list.has_errors());
    }

    #[test]
    // A build with errors is still returned in full, so the listing can show them.
    fn assemble_keeps_going_after_errors() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bad.kla");
        std::fs::write(&file, "_start\nNOTANOP A\nHALT\n").unwrap();
        let mut msg_list = MsgList::new();
        let (oplist, macros) = v2_opcodes_and_macros(&mut msg_list);
        let assembly = assemble(&file.to_string_lossy(), &oplist, &macros, &mut msg_list).unwrap();
        assert!(msg_list.has_errors());
        assert_eq!(assembly.lines.len(), 3);
        assert_eq!(msg_list.list[0].text, format!("Input file is {}", file.to_string_lossy()));
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
}
