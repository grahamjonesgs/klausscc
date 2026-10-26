//! The unified `Z S C V` flags register and the layouts it is saved in.
//!
//! The old compare flags `E`/`L`/`U` are retired as storage: they are derived
//! on read (`E = Z`, `L = S ^ V`, `U = C`), bit-identically to the retired
//! flags for every operand pair (proven RTL-side by `tb_flags.sv`).  The
//! layouts that still carry them (the interrupt context word, `GETF`, the
//! trace) present the derived values.

/// The flags register, stored as its debugger nibble (see [`Flags::to_nibble`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct Flags(u8);

impl Flags {
    /// Zero flag bit.
    const Z: u8 = 1 << 3;
    /// Sign flag bit (MSB of an arithmetic result).
    const S: u8 = 1 << 2;
    /// Carry / borrow out of bit 63 (x86 borrow convention: `C = 1` ⟺ `a < b`).
    const C: u8 = 1 << 1;
    /// Signed overflow bit.
    const V: u8 = 1;

    /// Zero flag.
    pub(super) const fn zero(self) -> bool {
        self.0 & Self::Z != 0
    }

    /// Sign flag.
    pub(super) const fn sign(self) -> bool {
        self.0 & Self::S != 0
    }

    /// Carry / borrow flag.
    pub(super) const fn carry(self) -> bool {
        self.0 & Self::C != 0
    }

    /// Signed-overflow flag.
    pub(super) const fn overflow(self) -> bool {
        self.0 & Self::V != 0
    }

    /// Derived equal flag: `E = Z`.
    pub(super) const fn equal(self) -> bool {
        self.zero()
    }

    /// Derived signed-less flag: `L = S ^ V`.
    pub(super) const fn less(self) -> bool {
        self.sign() != self.overflow()
    }

    /// Derived unsigned-less flag: `U = C` (the x86 borrow convention, §1.3).
    pub(super) const fn unsigned_less(self) -> bool {
        self.carry()
    }

    /// Set or clear the flag bits in `bit`.
    const fn put(&mut self, bit: u8, on: bool) {
        if on {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }

    /// Set the zero flag.
    pub(super) const fn set_zero(&mut self, on: bool) {
        self.put(Self::Z, on);
    }

    /// Set the sign flag.
    pub(super) const fn set_sign(&mut self, on: bool) {
        self.put(Self::S, on);
    }

    /// Set the carry flag.
    pub(super) const fn set_carry(&mut self, on: bool) {
        self.put(Self::C, on);
    }

    /// Set the overflow flag.
    pub(super) const fn set_overflow(&mut self, on: bool) {
        self.put(Self::V, on);
    }

    /// Set Z and S from a 64-bit result (the arithmetic producers).
    pub(super) const fn set_zs(&mut self, res: u64) {
        self.set_zero(res == 0);
        self.set_sign(res >> 63 == 1);
    }

    /// Debugger nibble: bit 3 = Z, bit 2 = S, bit 1 = C, bit 0 = V.
    pub(super) const fn to_nibble(self) -> u8 {
        self.0
    }

    /// From a debugger nibble ([`Flags::to_nibble`]); higher bits are ignored.
    pub(super) const fn from_nibble(nibble: u8) -> Self {
        Self(nibble & 0xF)
    }

    /// Interrupt-context field (bits `[38:32]` of the saved word):
    /// `{Z,E,C,V,S,L,U}` from bit 6 down to bit 0.
    pub(super) const fn to_ctx(self) -> u8 {
        bit(self.zero(), 6)
            | bit(self.equal(), 5)
            | bit(self.carry(), 4)
            | bit(self.overflow(), 3)
            | bit(self.sign(), 2)
            | bit(self.less(), 1)
            | bit(self.unsigned_less(), 0)
    }

    /// From an interrupt-context field ([`Flags::to_ctx`]).  Only `Z/S/C/V`
    /// are consumed (§1.5); the derived `E`/`L`/`U` bits are ignored.
    pub(super) const fn from_ctx(ctx: u8) -> Self {
        let mut f = Self(0);
        f.set_zero(ctx & (1 << 6) != 0);
        f.set_carry(ctx & (1 << 4) != 0);
        f.set_overflow(ctx & (1 << 3) != 0);
        f.set_sign(ctx & (1 << 2) != 0);
        f
    }

    /// `GETF` / `SETFR` nibble (bits `[63:60]` of the result): `{Z,E,C,V}`
    /// from bit 3 down to bit 0, with `E` derived.
    pub(super) const fn to_getf(self) -> u8 {
        bit(self.zero(), 3) | bit(self.equal(), 2) | bit(self.carry(), 1) | bit(self.overflow(), 0)
    }

    /// The trace's fixed 7-character field `{zero,sign,carry,overflow,equal,less,ult}`
    /// as ASCII `0`/`1`, matching the RTL self-trace's derived word.
    pub(super) const fn trace_field(self) -> [u8; 7] {
        [
            digit(self.zero()),
            digit(self.sign()),
            digit(self.carry()),
            digit(self.overflow()),
            digit(self.equal()),
            digit(self.less()),
            digit(self.unsigned_less()),
        ]
    }
}

/// `flag` as bit `n` of a byte.
const fn bit(flag: bool, n: u32) -> u8 {
    (flag as u8) << n
}

/// `flag` as an ASCII `0` / `1`.
const fn digit(flag: bool) -> u8 {
    if flag { b'1' } else { b'0' }
}

#[cfg(test)]
mod tests {
    use super::Flags;

    /// Every `Z S C V` combination, as a debugger nibble.
    fn all() -> impl Iterator<Item = Flags> {
        (0..16).map(Flags::from_nibble)
    }

    #[test]
    fn nibble_round_trips_and_ignores_high_bits() {
        for f in all() {
            assert_eq!(Flags::from_nibble(f.to_nibble()), f);
        }
        assert_eq!(Flags::from_nibble(0xF5).to_nibble(), 0x5);
    }

    #[test]
    fn nibble_bit_positions() {
        // (nibble, Z, S, C, V)
        let table = [
            (0b1000, true, false, false, false),
            (0b0100, false, true, false, false),
            (0b0010, false, false, true, false),
            (0b0001, false, false, false, true),
        ];
        for (nibble, z, s, c, v) in table {
            let f = Flags::from_nibble(nibble);
            assert_eq!((f.zero(), f.sign(), f.carry(), f.overflow()), (z, s, c, v), "{nibble:04b}");
        }
    }

    #[test]
    fn derived_flags() {
        for f in all() {
            assert_eq!(f.equal(), f.zero());
            assert_eq!(f.less(), f.sign() ^ f.overflow());
            assert_eq!(f.unsigned_less(), f.carry());
        }
    }

    #[test]
    fn ctx_round_trips_and_ignores_derived_bits() {
        for f in all() {
            assert_eq!(Flags::from_ctx(f.to_ctx()), f);
            // Flipping E, L and U changes nothing on restore.
            assert_eq!(Flags::from_ctx(f.to_ctx() ^ 0b010_0011), f);
        }
        // Z and S set, so E (derived) and L (S ^ V) too.
        assert_eq!(Flags::from_nibble(0b1100).to_ctx(), 0b110_0110);
    }

    #[test]
    fn setters_touch_only_their_bit() {
        let mut f = Flags::default();
        f.set_zero(true);
        f.set_carry(true);
        assert_eq!(f.to_nibble(), 0b1010);
        f.set_zero(false);
        f.set_sign(true);
        f.set_overflow(true);
        assert_eq!(f.to_nibble(), 0b0111);
    }

    #[test]
    fn getf_and_trace_layouts() {
        // (nibble ZSCV, GETF ZECV, trace ZSCVELU)
        let table = [
            (0b0000, 0b0000, *b"0000000"),
            (0b1000, 0b1100, *b"1000100"),
            (0b0100, 0b0000, *b"0100010"),
            (0b0010, 0b0010, *b"0010001"),
            (0b0001, 0b0001, *b"0001010"),
            (0b0101, 0b0001, *b"0101000"),
            (0b1111, 0b1111, *b"1111101"),
        ];
        for (nibble, getf, trace) in table {
            let f = Flags::from_nibble(nibble);
            assert_eq!(f.to_getf(), getf, "{nibble:04b}");
            assert_eq!(f.trace_field(), trace, "{nibble:04b}");
        }
    }
}
