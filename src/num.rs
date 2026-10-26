//! Integer literal parsing shared by the assembler and the command line,
//! and [`human_bytes`](crate::num::human_bytes) for the sizes in progress messages.
//!
//! Every numeric operand in the assembler (`.word`, `.space`, `#name N`,
//! immediates, counts) and the `--entry` address go through [`parse_int`], so
//! they all accept the same forms:
//!
//! * decimal, with an optional leading `-` or `+` (`42`, `-1`, `+7`);
//! * hex with a `0x`/`0X` prefix, case-insensitive digits and `_` separators
//!   (`0xFF`, `0X1_0000`, `-0x10`).
//!
//! Decimal literals do not take `_` separators.  The magnitude must fit in a
//! `u64`; callers then check the range they need.

use std::fmt;

/// Why a literal could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LiteralError {
    /// The literal is empty (or only a sign / `0x` prefix).
    Empty,
    /// The literal contains a character that is not a digit of its base.
    InvalidDigit,
    /// The magnitude does not fit in 64 bits.
    Overflow,
}

impl fmt::Display for LiteralError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("missing number"),
            Self::InvalidDigit => f.write_str("invalid digit"),
            Self::Overflow => f.write_str("number too large for 64 bits"),
        }
    }
}

impl std::error::Error for LiteralError {}

/// Returns true when `token` is written as a hex literal (`0x…`, `-0x…`, `+0x…`).
pub(crate) fn is_hex_literal(token: &str) -> bool {
    let unsigned = token.strip_prefix(['-', '+']).unwrap_or(token);
    unsigned.get(..2).is_some_and(|prefix| prefix.eq_ignore_ascii_case("0x"))
}

/// Parse an integer literal (see the module docs for the accepted forms).
///
/// Returns the mathematical value, so a hex literal is never negative unless
/// it carries a `-` sign: `0xFFFF_FFFF_FFFF_FFFF` is `u64::MAX`, not `-1`.
pub(crate) fn parse_int(token: &str) -> Result<i128, LiteralError> {
    let (negative, unsigned) = match token.as_bytes().first() {
        Some(b'-') => (true, &token[1..]),
        Some(b'+') => (false, &token[1..]),
        _ => (false, token),
    };
    // Only one sign is allowed, so look for the prefix directly rather than
    // through `is_hex_literal`, which would skip a second sign.
    let magnitude = match unsigned.get(..2) {
        Some(prefix) if prefix.eq_ignore_ascii_case("0x") => digits_value(unsigned[2..].chars().filter(|&c| c != '_'), 16)?,
        _ => digits_value(unsigned.chars(), 10)?,
    };
    let value = i128::from(magnitude);
    Ok(if negative { -value } else { value })
}

/// Parse a literal as a 64-bit pattern.
///
/// Accepts any value in `i64::MIN ..= u64::MAX`; negative values are stored in
/// two's complement, so `-1` and `0xFFFF_FFFF_FFFF_FFFF` give the same bits.
pub(crate) fn parse_bits64(token: &str) -> Result<u64, LiteralError> {
    let value = parse_int(token)?;
    if value < i128::from(i64::MIN) {
        return Err(LiteralError::Overflow);
    }
    // In range [i64::MIN, u64::MAX]: truncating to 64 bits is the two's complement pattern.
    Ok(value as u64)
}

/// Parse an instruction operand literal under the assembler's immediate rule.
///
/// A hex literal is read as a 64-bit pattern and reinterpreted as signed, so
/// `0xFFFF_FFFF_FFFF_FFFF` is `-1`; a decimal literal keeps its value, so
/// `18446744073709551615` stays `u64::MAX`.
pub(crate) fn parse_operand(token: &str) -> Result<i128, LiteralError> {
    if is_hex_literal(token) {
        parse_bits64(token).map(|bits| i128::from(bits as i64))
    } else {
        parse_int(token)
    }
}

/// Returns true when an operand value fits a 32-bit immediate word.
///
/// The CPU sign-extends the word, so `i32::MIN ..= 0xFFFF_FFFF` is accepted.
pub(crate) fn fits_imm32(value: i128) -> bool {
    (i128::from(i32::MIN)..=0xFFFF_FFFF).contains(&value)
}

/// Parse a literal that must fit in a `u32` (an address such as `--entry`).
pub(crate) fn parse_u32(token: &str) -> Result<u32, String> {
    let value = parse_int(token).map_err(|err| format!("{err} in \"{token}\""))?;
    u32::try_from(value).map_err(|_| format!("\"{token}\" is outside 0..=0xFFFFFFFF"))
}

/// Accumulate `digits` in `radix`, rejecting empty input and 64-bit overflow.
fn digits_value(digits: impl Iterator<Item = char>, radix: u32) -> Result<u64, LiteralError> {
    let mut value: u64 = 0;
    let mut any = false;
    for c in digits {
        let digit = c.to_digit(radix).ok_or(LiteralError::InvalidDigit)?;
        value = value
            .checked_mul(u64::from(radix))
            .and_then(|v| v.checked_add(u64::from(digit)))
            .ok_or(LiteralError::Overflow)?;
        any = true;
    }
    if any { Ok(value) } else { Err(LiteralError::Empty) }
}

/// Format a byte count as a human-readable size (B / KiB / MiB / GiB), e.g.
/// `9223315` -> `"8.80 MiB"`.  Used for progress/size display.
#[must_use]
pub(crate) fn human_bytes(n: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1_048_576.0;
    const GIB: f64 = 1_073_741_824.0;
    let f = n as f64;
    if f >= GIB {
        format!("{:.2} GiB", f / GIB)
    } else if f >= MIB {
        format!("{:.2} MiB", f / MIB)
    } else if f >= KIB {
        format!("{:.1} KiB", f / KIB)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_forms() {
        assert_eq!(parse_int("0"), Ok(0));
        assert_eq!(parse_int("42"), Ok(42));
        assert_eq!(parse_int("-1"), Ok(-1));
        assert_eq!(parse_int("+7"), Ok(7));
        assert_eq!(parse_int("007"), Ok(7));
        assert_eq!(parse_int("18446744073709551615"), Ok(i128::from(u64::MAX)));
        assert_eq!(parse_int("-18446744073709551615"), Ok(-i128::from(u64::MAX)));
    }

    #[test]
    fn hex_forms() {
        assert_eq!(parse_int("0x2A"), Ok(42));
        assert_eq!(parse_int("0X2a"), Ok(42));
        assert_eq!(parse_int("0x1_000"), Ok(0x1000));
        assert_eq!(parse_int("0x_10"), Ok(0x10));
        assert_eq!(parse_int("-0x10"), Ok(-16));
        assert_eq!(parse_int("0xFFFF_FFFF_FFFF_FFFF"), Ok(i128::from(u64::MAX)));
        assert_eq!(parse_int("0x8000000000000000"), Ok(1_i128 << 63));
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(parse_int(""), Err(LiteralError::Empty));
        assert_eq!(parse_int("-"), Err(LiteralError::Empty));
        assert_eq!(parse_int("0x"), Err(LiteralError::Empty));
        assert_eq!(parse_int("0x__"), Err(LiteralError::Empty));
        assert_eq!(parse_int("12abc"), Err(LiteralError::InvalidDigit));
        assert_eq!(parse_int("1_000"), Err(LiteralError::InvalidDigit)); // no separators in decimal
        assert_eq!(parse_int("0xGG"), Err(LiteralError::InvalidDigit));
        assert_eq!(parse_int("--5"), Err(LiteralError::InvalidDigit));
        assert_eq!(parse_int("0x-5"), Err(LiteralError::InvalidDigit));
        assert_eq!(parse_int("--0x5"), Err(LiteralError::InvalidDigit));
        assert_eq!(parse_int("-+0x5"), Err(LiteralError::InvalidDigit));
        assert_eq!(parse_int("LOOP"), Err(LiteralError::InvalidDigit));
        assert_eq!(parse_int("loop:"), Err(LiteralError::InvalidDigit));
        assert_eq!(parse_int(" 5"), Err(LiteralError::InvalidDigit)); // callers pass whole tokens
    }

    #[test]
    fn rejects_overflow() {
        assert_eq!(parse_int("18446744073709551616"), Err(LiteralError::Overflow));
        assert_eq!(parse_int("0x1_0000_0000_0000_0000"), Err(LiteralError::Overflow));
    }

    #[test]
    fn hex_detection() {
        assert!(is_hex_literal("0x1"));
        assert!(is_hex_literal("0X"));
        assert!(is_hex_literal("-0x1"));
        assert!(!is_hex_literal("10"));
        assert!(!is_hex_literal("x10"));
        assert!(!is_hex_literal(""));
    }

    #[test]
    fn bits64_reinterprets() {
        assert_eq!(parse_bits64("-1"), Ok(u64::MAX));
        assert_eq!(parse_bits64("0xFFFFFFFFFFFFFFFF"), Ok(u64::MAX));
        assert_eq!(parse_bits64("0x8000000000000000"), Ok(1_u64 << 63));
        assert_eq!(parse_bits64("-9223372036854775808"), Ok(1_u64 << 63));
        assert_eq!(parse_bits64("-9223372036854775809"), Err(LiteralError::Overflow));
        assert_eq!(parse_bits64("12abc"), Err(LiteralError::InvalidDigit));
    }

    #[test]
    fn operand_values() {
        assert_eq!(parse_operand("0xFFFFFFFFFFFFFFFF"), Ok(-1));
        assert_eq!(parse_operand("0x8000000000000000"), Ok(i128::from(i64::MIN)));
        assert_eq!(parse_operand("-0x10"), Ok(-16));
        assert_eq!(parse_operand("18446744073709551615"), Ok(i128::from(u64::MAX)));
        assert_eq!(parse_operand("-1"), Ok(-1));
        assert_eq!(parse_operand("0xGG"), Err(LiteralError::InvalidDigit));
    }

    #[test]
    fn imm32_bounds() {
        assert!(fits_imm32(0));
        assert!(fits_imm32(-1));
        assert!(fits_imm32(i128::from(i32::MIN)));
        assert!(fits_imm32(0xFFFF_FFFF));
        assert!(!fits_imm32(0x1_0000_0000));
        assert!(!fits_imm32(i128::from(i32::MIN) - 1));
        assert!(!fits_imm32(i128::from(u64::MAX)));
    }

    #[test]
    fn u32_range() {
        assert_eq!(parse_u32("0x20"), Ok(0x20));
        assert_eq!(parse_u32("4294967295"), Ok(u32::MAX));
        assert!(parse_u32("0x1_0000_0000").is_err());
        assert!(parse_u32("-1").is_err());
        assert_eq!(parse_u32("0xZZ"), Err("invalid digit in \"0xZZ\"".to_owned()));
    }

    #[test]
    fn human_bytes_picks_the_unit() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(9_223_315), "8.80 MiB");
        assert_eq!(human_bytes(1 << 30), "1.00 GiB");
    }
}
