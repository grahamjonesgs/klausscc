//! The Klauss memory map, shared by the assembler, the image builders, the
//! emulator and the debug servers.
//!
//! A program image starts with a [`HEAP_HEADER_WORDS`]-doubleword heap header,
//! code begins at [`CODE_BASE`], and the program is followed by the heap from
//! [`heap_start`].  Addresses with `addr[31:28] == 0xF` are device registers
//! ([`is_mmio`]).  This module has no intra-crate dependencies.

/// Number of 64-bit words reserved at the start of memory for the heap header.
/// Word 0 (byte 0x00): `heap_start` (written by the assembler), word 1 (0x08):
/// `heap_end` (reserved), words 2-3 (0x10, 0x18): reserved.  Code begins at
/// byte 0x20 ([`CODE_BASE`]).
pub const HEAP_HEADER_WORDS: u32 = 4;

/// Heap-header byte size (4 doublewords): code starts here (0x20).
pub const CODE_BASE: u32 = HEAP_HEADER_WORDS * 8;

/// The `heap_start` value for a program image whose last byte ends at `end`.
///
/// `heap_start` is the first free byte after the program, rounded up to an
/// 8-byte boundary.  All heap block headers are 3×8=24 bytes, so every data
/// area is 8-byte aligned when `heap_start` is — required for MEMSET8→MEMGET64
/// coherency.  The `.kbt` stream, the DDR image and the `.code` listing header
/// all use this one rule.
#[must_use]
pub const fn heap_start(end: u32) -> u32 {
    end.next_multiple_of(8)
}

/// True if `addr` decodes to the memory-mapped device window (`addr[31:28] == 0xF`).
///
/// The RTL `bus_splitter` routes this range to the MMIO space instead of DDR.
#[must_use]
pub const fn is_mmio(addr: u32) -> bool {
    addr >> 28 == 0xF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Code starts after the four 8-byte heap header words
    fn test_code_base() {
        assert_eq!(CODE_BASE, 0x20);
    }

    #[test]
    // heap_start is the end of the program rounded up to 8 bytes
    fn test_heap_start_rounds_up_to_8() {
        assert_eq!(heap_start(0x20), 0x20);
        assert_eq!(heap_start(0x21), 0x28);
        assert_eq!(heap_start(0x24), 0x28);
        assert_eq!(heap_start(0x28), 0x28);
    }

    #[test]
    // Only the top nibble 0xF selects the device window
    fn test_is_mmio() {
        assert!(is_mmio(0xF000_0000));
        assert!(is_mmio(0xFFFF_FFFF));
        assert!(!is_mmio(0xEFFF_FFFF));
        assert!(!is_mmio(0));
    }
}
