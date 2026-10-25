//! Per-line records that flow through the assembler passes.
//!
//! Source files are read into [`SourceLine`]s, which macro expansion turns
//! into more `SourceLine`s; pass 1 adds the [`LineType`] and program counter
//! ([`Pass1`]) and pass 2 the encoded opcode ([`Pass2`]).  This module has no
//! intra-crate dependencies.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Defines the type of line.
pub enum LineType {
    Blank,
    Comment,
    Data,
    Error,
    Label,
    Opcode,
    Start,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// One line of assembler source: as read from a file, or as produced by
/// macro expansion.
pub struct SourceLine {
    /// File name of input file.
    pub file_name: String,
    /// Line text.
    pub input_text_line: String,
    /// Line number of input file.
    pub line_counter: u32,
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
