//! The macros built into the assembler, always available to every program.

use super::{Macro, macro_from_string};
use crate::diag::Diagnostics;
use crate::isa::Opcode;
use crate::isa::table::V2_OPCODES;

/// Standard macro definitions bundled with the assembler (formerly the `.vh`
/// header's `/* Macro definition */` block).
///
/// They use the same `$NAME item / item / …` syntax the old `opcode_select.vh`
/// used (`%1`, `%2`, … are positional arguments).  They are always available;
/// programs can add their own with the `!macro` / `!macros` source directives
/// (see [`apply_source_macros`](super::apply_source_macros)).
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

/// The built-in v2 opcode table plus the standard macro definitions — the
/// in-code replacement for parsing an `opcode_select.vh` file.
#[must_use]
pub(crate) fn v2_opcodes_and_macros(msg_list: &mut Diagnostics) -> (&'static [Opcode], Vec<Macro>) {
    let macros = V2_MACROS.iter().filter_map(|line| macro_from_string(line, msg_list)).collect();
    (V2_OPCODES, macros)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Every built-in definition parses, and parsing them reports nothing.
    fn test_builtin_macros_all_parse() {
        let mut msg_list = Diagnostics::new();
        let (_, macros) = v2_opcodes_and_macros(&mut msg_list);
        assert_eq!(macros.len(), V2_MACROS.len());
        assert!(msg_list.messages().is_empty());
    }
}
