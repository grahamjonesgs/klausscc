//! The debug module's DMI register map (`klaussCPU/debug_module.sv`) and
//! the decoding of DMSTATUS.

use std::fmt;

// ---- DMI register addresses -------------------------------------------------
/// Run control (`CTL_*`).
pub(super) const DMCONTROL: u8 = 0x00;
/// Status ([`DmStatus`]); writing it clears the sticky error.
pub(super) const DMSTATUS: u8 = 0x01;
/// Register access: the `REG_CMD` index of a register (its GDB `regnum`),
/// plus [`REG_CMD_WRITE`] to write it from DATA0 rather than read it into DATA0.
pub(super) const REG_CMD: u8 = 0x02;
/// The data register of register and memory commands.
pub(super) const DATA0: u8 = 0x03;
/// The doubleword address of the next memory command.
pub(super) const MEM_ADDR: u8 = 0x04;
/// Memory command: byte enables (`MEM_BE_*`), [`MEM_WRITE`], [`MEM_INC`].
pub(super) const MEM_CMD: u8 = 0x05;
/// The first hardware breakpoint comparator.
pub(super) const BP0: u8 = 0x10;
/// The first watchpoint comparator.
pub(super) const WP0: u8 = 0x18;
/// The address of the access that hit a watchpoint.
pub(super) const WATCH_ADDR: u8 = 0x1F;
/// The breakpoint comparators the DMI addresses have room for.
pub(super) const MAX_BREAKPOINTS: usize = (WP0 - BP0) as usize;
/// The watchpoint comparators the DMI addresses have room for.
pub(super) const MAX_WATCHPOINTS: usize = (WATCH_ADDR - WP0) as usize;

// ---- DMCONTROL bits ---------------------------------------------------------
pub(super) const CTL_HALTREQ: u64 = 1 << 0;
pub(super) const CTL_RESUMEREQ: u64 = 1 << 1;
pub(super) const CTL_STEP: u64 = 1 << 2;
pub(super) const CTL_ATTACHED: u64 = 1 << 3;
pub(super) const CTL_HALT_ON_LOAD: u64 = 1 << 4;

// ---- REG_CMD and MEM_CMD bits -----------------------------------------------
/// `REG_CMD`: write the register from DATA0.
pub(super) const REG_CMD_WRITE: u64 = 1 << 8;
/// `MEM_CMD`: every byte lane of the doubleword.
pub(super) const MEM_BE_ALL: u64 = 0xFF;
/// `MEM_CMD`: write DATA0's enabled lanes (otherwise read into DATA0).
pub(super) const MEM_WRITE: u64 = 1 << 8;
/// `MEM_CMD`: step `MEM_ADDR` to the next doubleword afterwards.
pub(super) const MEM_INC: u64 = 1 << 9;

// ---- comparator slots -------------------------------------------------------
/// `BP*` / `WP*`: the comparator is armed (the address is in the low bits).
pub(super) const SLOT_ENABLE: u64 = 1 << 32;
/// `WP*`: halt on reads.
pub(super) const WP_READ: u64 = 1 << 33;
/// `WP*`: halt on writes.
pub(super) const WP_WRITE: u64 = 1 << 34;
/// `WP*`: where the byte lanes watched start.
pub(super) const WP_LANES_SHIFT: u32 = 40;

/// The DMSTATUS signature of a `KlaussCPU` debug module (`'K'`).
const DM_MAGIC: u64 = 0x4B;

/// Why the core is halted (`klauss_pkg::DBG_CAUSE_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cause {
    /// A halt request (also the halt-on-load at a program's entry).
    Request,
    /// A single step completed.
    Step,
    /// A hardware breakpoint.
    Breakpoint,
    /// A watchpoint.
    Watchpoint,
    /// A `TRAP` instruction.
    Trap,
    /// An illegal instruction.
    Illegal,
    /// A `HALT` instruction: the program ended.
    Halt,
    /// A cause this debugger does not know.
    Unknown(u8),
}

impl Cause {
    /// The cause with `DBG_CAUSE_*` value `code`.
    pub(crate) const fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Request,
            1 => Self::Step,
            2 => Self::Breakpoint,
            3 => Self::Watchpoint,
            4 => Self::Trap,
            5 => Self::Illegal,
            6 => Self::Halt,
            n => Self::Unknown(n),
        }
    }

    /// The cause's `DBG_CAUSE_*` value.
    pub(crate) const fn code(self) -> u8 {
        match self {
            Self::Request => 0,
            Self::Step => 1,
            Self::Breakpoint => 2,
            Self::Watchpoint => 3,
            Self::Trap => 4,
            Self::Illegal => 5,
            Self::Halt => 6,
            Self::Unknown(n) => n,
        }
    }
}

/// What a debug module reports about itself in DMSTATUS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DmInfo {
    /// The debug module's version.
    pub(crate) version: u8,
    /// Hardware breakpoint comparators.
    pub(crate) breakpoints: u8,
    /// Watchpoint comparators.
    pub(crate) watchpoints: u8,
}

/// A DMSTATUS value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DmStatus(u64);

impl DmStatus {
    /// The status with register value `raw`.
    pub(crate) const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The `width`-bit field at bit `shift`.
    const fn field(self, shift: u32, width: u32) -> u8 {
        ((self.0 >> shift) & ((1 << width) - 1)) as u8
    }

    /// The core is halted.
    pub(crate) const fn halted(self) -> bool {
        self.0 & 1 != 0
    }

    /// A memory command is still running.
    pub(crate) const fn busy(self) -> bool {
        self.0 & (1 << 2) != 0
    }

    /// A memory command failed (sticky until DMSTATUS is written).
    pub(crate) const fn error(self) -> bool {
        self.0 & (1 << 3) != 0
    }

    /// Why the core is halted.
    pub(crate) const fn cause(self) -> Cause {
        Cause::from_code(self.field(4, 3))
    }

    /// Where the halted core is parked.
    pub(crate) const fn park_kind(self) -> u8 {
        self.field(8, 3)
    }

    /// The core is parked (any park, debug halts included).
    pub(crate) const fn parked(self) -> bool {
        self.0 & (1 << 7) != 0
    }

    /// The core stopped on its own (HALT, TRAP or an illegal instruction,
    /// park kinds 0-2) with no debugger attached: a halt request cannot
    /// reach it.  A WAIT (kind 3) still takes one.
    pub(crate) const fn stopped(self) -> bool {
        self.parked() && !self.halted() && self.park_kind() <= 2
    }

    /// The core has halted since DMSTATUS was last read.
    pub(crate) const fn halted_seen(self) -> bool {
        self.0 & (1 << 11) != 0
    }

    /// The module's version and comparator counts, or `None` if this is not
    /// a `KlaussCPU` debug module's DMSTATUS.
    pub(crate) const fn info(self) -> Option<DmInfo> {
        if (self.0 >> 24) & 0xFF != DM_MAGIC {
            return None;
        }
        Some(DmInfo {
            version: self.field(12, 4),
            breakpoints: self.field(16, 4),
            watchpoints: self.field(20, 4),
        })
    }
}

/// The register value in hex, so messages read `DMSTATUS 0x{status:08X}`.
impl fmt::UpperHex for DmStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.0, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dmstatus_fields_decode() {
        let st = DmStatus::from_raw(0x4B24_1A49);
        assert!(st.halted());
        assert!(!st.busy());
        assert!(st.error());
        assert_eq!(st.cause(), Cause::Trap);
        assert_eq!(st.park_kind(), 2);
        assert!(st.halted_seen());
        assert_eq!(
            st.info(),
            Some(DmInfo {
                version: 1,
                breakpoints: 4,
                watchpoints: 2
            })
        );
        assert_eq!(format!("0x{st:08X}"), "0x4B241A49");
        assert_eq!(DmStatus::from_raw(0x0024_1000).info(), None, "no signature");
        assert!(DmStatus::from_raw(4).busy());
        assert_eq!(DmStatus::from_raw(0x70).cause(), Cause::Unknown(7));
    }

    #[test]
    fn causes_round_trip_their_codes() {
        for code in 0..8 {
            assert_eq!(Cause::from_code(code).code(), code);
        }
        assert_eq!(Cause::from_code(6), Cause::Halt);
        assert_eq!(Cause::from_code(0), Cause::Request);
    }
}
