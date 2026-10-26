//! Per-line records that flow through the assembler passes.
//!
//! Source files are read into [`SourceLine`]s, which macro expansion turns
//! into more `SourceLine`s; pass 1 adds the [`LineType`] and program counter
//! ([`Pass1`]) and pass 2 the machine code the line emits ([`Pass2`], with
//! its [`Emitted`] words or bytes).  This module has no intra-crate
//! dependencies.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Defines the type of line.
pub(crate) enum LineType {
    /// Nothing but whitespace.
    Blank,
    /// Only a `//` comment.
    Comment,
    /// A `#name` definition or a `.word`/`.space`/`.zeropad` directive.
    Data,
    /// A directive that emits nothing, such as `.text` or `.global`.
    Directive,
    /// Not valid source, or an instruction that could not be encoded.
    Error,
    /// A `name:` label.
    Label,
    /// An instruction.
    Opcode,
    /// The `_start` marker: the program's entry point.
    Start,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// One line of assembler source: as read from a file, or as produced by
/// macro expansion.
pub(crate) struct SourceLine {
    /// File name of input file.
    pub file_name: String,
    /// Line text.
    pub input_text_line: String,
    /// Line number of input file.
    pub line_counter: u32,
}

#[derive(Debug)]
/// Struct for Pass1.
pub(crate) struct Pass1 {
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

#[derive(Debug, Clone)]
/// Struct for Pass2.
pub(crate) struct Pass2 {
    /// File name of input file.
    #[allow(dead_code, reason = "where the line came from; no output reads it yet, only tests")]
    pub file_name: String,
    /// Line text.
    pub input_text_line: String,
    /// Line number of input file.
    #[allow(dead_code, reason = "where the line came from; no output reads it yet, only tests")]
    pub line_counter: u32,
    /// Line type.
    pub line_type: LineType,
    /// The machine code the line emits.
    pub emitted: Emitted,
    /// Program counter.
    pub program_counter: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
/// The machine code one line emits, as numbers; hex is only written by the
/// `.code` listing and the `.kbt` image.
pub(crate) enum Emitted {
    /// Nothing: a line that holds no code or data, or one that could not be
    /// encoded.
    #[default]
    None,
    /// Instruction words: word 0, then its immediate words.
    Code(Vec<u32>),
    /// Data bytes, in memory order.
    Data(Vec<u8>),
}

impl Emitted {
    /// The number of bytes the line occupies in memory.
    #[must_use]
    pub(crate) const fn byte_len(&self) -> usize {
        match self {
            Self::None => 0,
            Self::Code(words) => words.len() * 4,
            Self::Data(bytes) => bytes.len(),
        }
    }

    /// Write the line's bytes, in memory order, to `dest`, which must be
    /// [`byte_len`](Self::byte_len) long.  Each instruction word is stored
    /// little-endian, as the CPU's 32-bit reads expect.
    pub(crate) fn copy_to(&self, dest: &mut [u8]) {
        match self {
            Self::None => {}
            Self::Code(words) => {
                for (slot, word) in dest.as_chunks_mut::<4>().0.iter_mut().zip(words) {
                    *slot = word.to_le_bytes();
                }
            }
            Self::Data(bytes) => dest.copy_from_slice(bytes),
        }
    }
}
