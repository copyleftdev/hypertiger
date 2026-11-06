//! Exact decimal numbers. A price never passes through a float.
//!
//! The official SDKs round through `f64`, then guard the round trip with an epsilon. Here the wire
//! string parses straight into an integer and a value that cannot be held exactly is an error, so
//! the trailing-zero and rounding classes of signature bug cannot be written.
//!
//! Three types, because three ranges were measured on mainnet (2026-11-05):
//!
//! - `Px` is `u64` at 8 decimals. Spot prices carry at most 8 decimals; the range reaches 1.8e11.
//! - `Sz` is `u128` at 8 decimals. A token's supply is a `u64` of wei, and `totalSupply` was seen
//!   at 1.8e14, past what `u64` holds at this scale.
//! - `Decimal` is `i128` at 10 decimals, for funding and aggregates, seen at 10 decimals.

use core::fmt;

/// Decimal places held by `Px` and `Sz`. Hyperliquid's wire format stops at 8.
pub const WIRE_DECIMALS: u32 = 8;
/// One whole unit of `Px` or `Sz`.
pub const WIRE_ONE: u64 = 100_000_000;
/// Decimal places held by `Decimal`.
pub const DECIMAL_DECIMALS: u32 = 10;
/// One whole unit of `Decimal`.
pub const DECIMAL_ONE: i128 = 10_000_000_000;
/// The longest number accepted on the wire. `u128::MAX` is 39 digits; a sign and a point make 41,
/// and the rest allows for zeros past the scale, which are value-preserving.
pub const WIRE_SIZE_MAX: usize = 64;

const _: () = assert!(10u64.pow(WIRE_DECIMALS) == WIRE_ONE);
const _: () = assert!(10i128.pow(DECIMAL_DECIMALS) == DECIMAL_ONE);
const _: () = assert!(WIRE_DECIMALS <= DECIMAL_DECIMALS);
const _: () = assert!(u128::MAX.ilog10() + 1 + 2 <= WIRE_SIZE_MAX as u32);

/// Why a string on the wire is not a number this crate can hold exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecimalError {
    Empty,
    TooLong,
    Malformed,
    /// Negative where the type is unsigned.
    Negative,
    /// A nonzero digit past the scale. Rounding it away would change the value.
    Precision,
    Overflow,
}

/// A price at 8 decimals.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Px(u64);

/// A size in units of the base coin at 8 decimals.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sz(u128);

/// A signed size at 8 decimals: positive is long.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Szi(i128);

/// A signed amount at 10 decimals, for funding, PnL and account values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Decimal(i128);

impl Px {
    pub const ZERO: Px = Px(0);

    #[must_use]
    pub const fn from_raw(raw: u64) -> Px {
        Px(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }

    pub fn parse(text: &[u8]) -> Result<Px, DecimalError> {
        let parsed = parse(text, WIRE_DECIMALS)?;
        if parsed.negative && parsed.magnitude != 0 {
            return Err(DecimalError::Negative);
        }
        let raw = u64::try_from(parsed.magnitude).map_err(|_| DecimalError::Overflow)?;
        Ok(Px(raw))
    }

    #[must_use]
    pub fn to_wire(self) -> Wire {
        Wire::format(false, u128::from(self.0), WIRE_DECIMALS)
    }
}

impl Sz {
    pub const ZERO: Sz = Sz(0);

    #[must_use]
    pub const fn from_raw(raw: u128) -> Sz {
        Sz(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u128 {
        self.0
    }

    pub fn parse(text: &[u8]) -> Result<Sz, DecimalError> {
        let parsed = parse(text, WIRE_DECIMALS)?;
        if parsed.negative && parsed.magnitude != 0 {
            return Err(DecimalError::Negative);
        }
        Ok(Sz(parsed.magnitude))
    }

    #[must_use]
    pub fn to_wire(self) -> Wire {
        Wire::format(false, self.0, WIRE_DECIMALS)
    }

    /// The smaller of two sizes.
    #[must_use]
    pub fn min(self, other: Sz) -> Sz {
        if self.0 <= other.0 { self } else { other }
    }

    #[must_use]
    pub fn checked_add(self, other: Sz) -> Option<Sz> {
        self.0.checked_add(other.0).map(Sz)
    }

    #[must_use]
    pub fn checked_sub(self, other: Sz) -> Option<Sz> {
        self.0.checked_sub(other.0).map(Sz)
    }

    /// `self - other`, stopping at zero, for a remainder that a late report may overshoot.
    #[must_use]
    pub fn saturating_sub(self, other: Sz) -> Sz {
        Sz(self.0.saturating_sub(other.0))
    }

    /// Price times size in whole quote units, rounded toward zero, or `None` past the range.
    ///
    /// Rounds down because its use is the exchange's minimum-notional check, which must not pass
    /// an order the exchange will refuse.
    #[must_use]
    pub fn notional_floor(self, px: Px) -> Option<Decimal> {
        // Two 8-decimal factors make a 16-decimal product; `Decimal` holds 10.
        const _: () = assert!(2 * WIRE_DECIMALS - DECIMAL_DECIMALS == 6);
        let product = self.0.checked_mul(u128::from(px.0))?;
        let magnitude = i128::try_from(product.div_euclid(1_000_000)).ok()?;
        Some(Decimal(magnitude))
    }
}

impl Szi {
    pub const ZERO: Szi = Szi(0);

    #[must_use]
    pub const fn from_raw(raw: i128) -> Szi {
        Szi(raw)
    }

    #[must_use]
    pub const fn raw(self) -> i128 {
        self.0
    }

    pub fn parse(text: &[u8]) -> Result<Szi, DecimalError> {
        let parsed = parse(text, WIRE_DECIMALS)?;
        let magnitude = i128::try_from(parsed.magnitude).map_err(|_| DecimalError::Overflow)?;
        Ok(Szi(if parsed.negative { -magnitude } else { magnitude }))
    }

    #[must_use]
    pub fn to_wire(self) -> Wire {
        Wire::format(self.0 < 0, self.0.unsigned_abs(), WIRE_DECIMALS)
    }

    #[must_use]
    pub fn checked_add_sz(self, size: Sz) -> Option<Szi> {
        let magnitude = i128::try_from(size.0).ok()?;
        self.0.checked_add(magnitude).map(Szi)
    }

    #[must_use]
    pub fn checked_sub_sz(self, size: Sz) -> Option<Szi> {
        let magnitude = i128::try_from(size.0).ok()?;
        self.0.checked_sub(magnitude).map(Szi)
    }
}

impl Decimal {
    pub const ZERO: Decimal = Decimal(0);

    #[must_use]
    pub const fn from_raw(raw: i128) -> Decimal {
        Decimal(raw)
    }

    #[must_use]
    pub const fn raw(self) -> i128 {
        self.0
    }

    pub fn parse(text: &[u8]) -> Result<Decimal, DecimalError> {
        let parsed = parse(text, DECIMAL_DECIMALS)?;
        let magnitude = i128::try_from(parsed.magnitude).map_err(|_| DecimalError::Overflow)?;
        Ok(Decimal(if parsed.negative { -magnitude } else { magnitude }))
    }

    #[must_use]
    pub fn to_wire(self) -> Wire {
        Wire::format(self.0 < 0, self.0.unsigned_abs(), DECIMAL_DECIMALS)
    }
}

impl fmt::Display for Px {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.to_wire().as_str())
    }
}

impl fmt::Display for Sz {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.to_wire().as_str())
    }
}

impl fmt::Display for Szi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.to_wire().as_str())
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.to_wire().as_str())
    }
}

/// A number's canonical wire form, held inline: no trailing zeros, no trailing point, no `-0`.
///
/// This is the one place that form is produced, which is what closes the docs' "trailing zeroes"
/// signing trap: a non-canonical number cannot reach the hash.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Wire {
    bytes: [u8; WIRE_SIZE_MAX],
    size: u8,
}

const _: () = assert!(WIRE_SIZE_MAX <= u8::MAX as usize);

impl Wire {
    fn format(negative: bool, magnitude: u128, decimals: u32) -> Wire {
        assert!(decimals <= DECIMAL_DECIMALS);
        let mut digits = [0u8; WIRE_SIZE_MAX];
        let mut digits_count = 0usize;
        let mut remaining = magnitude;
        while remaining > 0 {
            digits[digits_count] = b'0' + (remaining % 10) as u8;
            remaining /= 10;
            digits_count += 1;
        }
        assert!(digits_count <= 39);

        // `digits` is little-endian, so the trailing zeros to strip come first.
        let decimals_count = decimals as usize;
        let mut skip = 0usize;
        while skip < decimals_count && skip < digits_count && digits[skip] == b'0' {
            skip += 1;
        }
        if digits_count < decimals_count && skip == digits_count {
            // Only reachable for zero, whose digit string is empty.
            assert_eq!(magnitude, 0);
            skip = decimals_count;
        }

        let mut wire = Wire { bytes: [0; WIRE_SIZE_MAX], size: 0 };
        if negative && magnitude != 0 {
            wire.push(b'-');
        }
        wire.push_integer_part(&digits, digits_count, decimals_count);
        wire.push_fraction(&digits, digits_count, decimals_count, skip);
        assert!(wire.size > 0);
        wire
    }

    fn push_integer_part(&mut self, digits: &[u8], digits_count: usize, decimals_count: usize) {
        if digits_count <= decimals_count {
            self.push(b'0');
        } else {
            let mut position = digits_count;
            while position > decimals_count {
                position -= 1;
                self.push(digits[position]);
            }
        }
    }

    fn push_fraction(
        &mut self,
        digits: &[u8],
        digits_count: usize,
        decimals_count: usize,
        skip: usize,
    ) {
        if skip < decimals_count {
            self.push(b'.');
            let mut position = decimals_count;
            while position > skip {
                position -= 1;
                self.push(if position < digits_count { digits[position] } else { b'0' });
            }
            assert!(self.bytes[self.size as usize - 1] != b'0');
        }
    }

    fn push(&mut self, byte: u8) {
        assert!((self.size as usize) < WIRE_SIZE_MAX);
        self.bytes[self.size as usize] = byte;
        self.size += 1;
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        assert!(self.size > 0);
        &self.bytes[..self.size as usize]
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        let Ok(text) = core::str::from_utf8(self.as_bytes()) else {
            panic!("Wire holds only ASCII digits, a sign and a point");
        };
        text
    }
}

impl fmt::Debug for Wire {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

struct Parsed {
    negative: bool,
    magnitude: u128,
}

/// Parses `-?digits(.digits)?` strictly: no plus sign, no exponent, no bare point, no whitespace.
/// Leading zeros are accepted, since the exchange's own aggregate fields can carry them.
fn parse(text: &[u8], decimals: u32) -> Result<Parsed, DecimalError> {
    assert!(decimals <= DECIMAL_DECIMALS);
    if text.is_empty() {
        return Err(DecimalError::Empty);
    }
    if text.len() > WIRE_SIZE_MAX {
        return Err(DecimalError::TooLong);
    }

    let negative = text[0] == b'-';
    let body = if negative { &text[1..] } else { text };
    let (integer, fraction) = split_point(body);
    if integer.is_empty() {
        return Err(DecimalError::Malformed);
    }

    let mut magnitude = 0u128;
    for &byte in integer {
        magnitude = push_digit(magnitude, byte)?;
    }
    match fraction {
        None => scale_up(magnitude, decimals).map(|magnitude| Parsed { negative, magnitude }),
        Some(digits) => {
            if digits.is_empty() {
                return Err(DecimalError::Malformed);
            }
            let magnitude = parse_fraction(magnitude, digits, decimals)?;
            Ok(Parsed { negative, magnitude })
        }
    }
}

fn parse_fraction(integer: u128, digits: &[u8], decimals: u32) -> Result<u128, DecimalError> {
    let mut magnitude = integer;
    let mut digits_used = 0u32;
    for &byte in digits {
        if digits_used < decimals {
            magnitude = push_digit(magnitude, byte)?;
            digits_used += 1;
        } else if byte == b'0' {
            // Value-preserving: the aggregates pad, and "0.10000000000" is still a tenth.
        } else if byte.is_ascii_digit() {
            return Err(DecimalError::Precision);
        } else {
            return Err(DecimalError::Malformed);
        }
    }
    assert!(digits_used <= decimals);
    scale_up(magnitude, decimals - digits_used)
}

fn split_point(body: &[u8]) -> (&[u8], Option<&[u8]>) {
    let mut index = 0usize;
    while index < body.len() {
        if body[index] == b'.' {
            return (&body[..index], Some(&body[index + 1..]));
        }
        index += 1;
    }
    (body, None)
}

fn push_digit(magnitude: u128, byte: u8) -> Result<u128, DecimalError> {
    if !byte.is_ascii_digit() {
        return Err(DecimalError::Malformed);
    }
    let shifted = magnitude.checked_mul(10).ok_or(DecimalError::Overflow)?;
    shifted.checked_add(u128::from(byte - b'0')).ok_or(DecimalError::Overflow)
}

fn scale_up(magnitude: u128, decimals: u32) -> Result<u128, DecimalError> {
    assert!(decimals <= DECIMAL_DECIMALS);
    magnitude.checked_mul(10u128.pow(decimals)).ok_or(DecimalError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_is_canonical() {
        for (text, wire) in [
            ("1670.1", "1670.1"),
            ("0.0147", "0.0147"),
            ("100", "100"),
            ("100.0", "100"),
            ("100.00000000", "100"),
            ("0", "0"),
            ("0.0", "0"),
            ("-0", "0"),
            ("0.00000001", "0.00000001"),
            ("0.1", "0.1"),
            ("1000000", "1000000"),
            ("76029.0", "76029"),
            ("10.10", "10.1"),
            ("000123.4500", "123.45"),
        ] {
            assert_eq!(Sz::parse(text.as_bytes()).unwrap().to_wire().as_str(), wire, "{text}");
        }
    }

    #[test]
    fn parse_refuses_what_it_cannot_hold_exactly() {
        for (text, error) in [
            ("", DecimalError::Empty),
            ("-", DecimalError::Malformed),
            (".", DecimalError::Malformed),
            (".5", DecimalError::Malformed),
            ("5.", DecimalError::Malformed),
            ("+5", DecimalError::Malformed),
            ("1e5", DecimalError::Malformed),
            (" 1", DecimalError::Malformed),
            ("1 ", DecimalError::Malformed),
            ("1.2.3", DecimalError::Malformed),
            ("--1", DecimalError::Malformed),
            ("1.00000000x", DecimalError::Malformed),
            ("0.000000001", DecimalError::Precision),
            ("1.123456789", DecimalError::Precision),
            ("-1", DecimalError::Negative),
        ] {
            assert_eq!(Sz::parse(text.as_bytes()), Err(error), "{text:?}");
        }
        assert_eq!(Px::parse(b"184467440737.09551616"), Err(DecimalError::Overflow));
        assert_eq!(Px::parse(b"184467440737.09551615").unwrap().raw(), u64::MAX);
        let too_long = [b'1'; WIRE_SIZE_MAX + 1];
        assert_eq!(Sz::parse(&too_long), Err(DecimalError::TooLong));
    }

    #[test]
    fn measured_mainnet_ranges_parse() {
        assert_eq!(
            Sz::parse(b"184467440737095.53125").unwrap().to_wire().as_str(),
            "184467440737095.53125"
        );
        assert_eq!(Sz::parse(b"21312908264.0").unwrap().to_wire().as_str(), "21312908264");
        let funding = Decimal::parse(b"-0.0001998885").unwrap();
        assert_eq!(funding.raw(), -1_998_885);
        assert_eq!(funding.to_wire().as_str(), "-0.0001998885");
        let volume = Decimal::parse(b"4787999938.1588039398").unwrap();
        assert_eq!(volume.to_wire().as_str(), "4787999938.1588039398");
        assert_eq!(Decimal::parse(b"0.00000000001"), Err(DecimalError::Precision));
        assert_eq!(Decimal::parse(b"0.10000000000").unwrap().raw(), 1_000_000_000);
    }

    #[test]
    fn signed_size_round_trips_and_has_no_negative_zero() {
        assert_eq!(Szi::parse(b"-1.5").unwrap().raw(), -150_000_000);
        assert_eq!(Szi::parse(b"-1.5").unwrap().to_wire().as_str(), "-1.5");
        assert_eq!(Szi::parse(b"-0.0").unwrap().to_wire().as_str(), "0");
        assert_eq!(Szi::ZERO.checked_add_sz(Sz::from_raw(5)).unwrap().raw(), 5);
        assert_eq!(Szi::ZERO.checked_sub_sz(Sz::from_raw(5)).unwrap().raw(), -5);
    }

    #[test]
    fn notional_rounds_toward_zero() {
        let px = Px::parse(b"0.00000003").unwrap();
        let sz = Sz::parse(b"0.5").unwrap();
        assert_eq!(sz.notional_floor(px).unwrap().to_wire().as_str(), "0.000000015");
        let smallest = Sz::from_raw(1).notional_floor(Px::from_raw(1)).unwrap();
        assert_eq!(smallest, Decimal::ZERO);
        let px = Px::parse(b"10").unwrap();
        let sz = Sz::parse(b"1.5").unwrap();
        assert_eq!(sz.notional_floor(px).unwrap().to_wire().as_str(), "15");
        assert_eq!(Sz::from_raw(u128::MAX).notional_floor(px), None);
    }

    /// Every raw value in a dense band and at each power of ten survives a text round trip.
    #[test]
    fn round_trip_exhaustive_band() {
        let mut raw = 0u64;
        while raw < 200_000 {
            let px = Px::from_raw(raw);
            assert_eq!(Px::parse(px.to_wire().as_bytes()), Ok(px));
            raw += 1;
        }
        let mut power = 1u64;
        while power < u64::MAX / 10 {
            for candidate in [power - 1, power, power + 1] {
                let px = Px::from_raw(candidate);
                assert_eq!(Px::parse(px.to_wire().as_bytes()), Ok(px));
                assert_eq!(
                    Sz::parse(Sz::from_raw(candidate.into()).to_wire().as_bytes()).unwrap().raw(),
                    candidate.into()
                );
            }
            power *= 10;
        }
        assert_eq!(
            Sz::from_raw(u128::MAX).to_wire().as_str(),
            "3402823669209384634633746074317.68211455"
        );
        assert_eq!(
            Sz::parse(b"3402823669209384634633746074317.68211455").unwrap().raw(),
            u128::MAX
        );
        assert_eq!(
            Sz::parse(b"3402823669209384634633746074317.68211456"),
            Err(DecimalError::Overflow)
        );
    }
}
