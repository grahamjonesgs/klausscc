//! The assembler's shared building blocks.
//!
//! [`line`] holds the per-line records that flow between the passes and
//! [`lexer`] the pure text predicates used to classify a source line.  Both
//! are leaves: they import nothing else from the crate, so every other
//! assembler module can depend on them without forming a cycle.

pub mod lexer;
pub mod line;
