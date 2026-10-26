//! Per-line records that flow through the assembler passes.
//!
//! Source files are read into [`SourceLine`]s, which macro expansion turns
//! into more `SourceLine`s; pass 1 adds the [`LineType`] and program counter
//! ([`Pass1`]) and pass 2 the encoded opcode ([`Pass2`]).  This module has no
//! intra-crate dependencies.

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
    /// Opcode as string.
    pub opcode: String,
    /// Program counter.
    pub program_counter: u32,
}
