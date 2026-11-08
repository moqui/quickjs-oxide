//! Number conversions: `Number::toString(10)`, `StringToNumber`, `ToInt32`.

use crate::value::Value;

/// A number result: an `Int32` when the double is one (not `-0`), so integer
/// loops stay on the smi path.
#[inline]
pub(crate) fn number(value: f64) -> Value {
    let int = value as i32;
    if f64::from(int) == value && (int != 0 || value.is_sign_positive()) {
        Value::int32(int)
    } else {
        Value::number(value)
    }
}

/// `ToInt32` of a number.
#[inline]
pub(crate) fn to_int32(value: f64) -> i32 {
    if value.is_finite() && value.abs() < 2_147_483_648.0 {
        return value as i32;
    }
    to_uint32(value) as i32
}

/// `ToUint32` of a number.
pub(crate) fn to_uint32(value: f64) -> u32 {
    if !value.is_finite() {
        return 0;
    }
    let truncated = value.trunc();
    let modulo = truncated.rem_euclid(4_294_967_296.0);
    modulo as u32
}

/// `Number::toString(x)` in radix 10: a safe integer prints its digits;
/// anything else takes the fewest digits that round-trip, rounding an
/// exact tie to even as QuickJS does (Rust's formatter does not).
pub(crate) fn to_string(value: f64) -> String {
    text(value).as_str().to_owned()
}

/// A number's `Number::toString(10)` text, composed on the stack: ASCII, at
/// most 25 bytes (`-0.0000012345678901234567`, `-1.7976931348623157e+308`).
#[derive(Clone, Copy)]
pub(crate) struct NumberText {
    bytes: [u8; 25],
    len: u8,
}

impl NumberText {
    fn of(text: &[u8]) -> Self {
        let mut bytes = [0; 25];
        bytes[..text.len()].copy_from_slice(text);
        NumberText {
            bytes,
            len: text.len() as u8,
        }
    }

    #[inline]
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    #[inline]
    pub(crate) fn as_str(&self) -> &str {
        std::str::from_utf8(self.as_bytes()).expect("ASCII")
    }
}

/// Up to 17 significant digits (no leading or trailing zeros) and the
/// decimal point's place: the value is `0.d1d2… × 10^point`.
struct Decimal {
    digits: [u8; 17],
    len: usize,
    point: i32,
}

impl Decimal {
    fn of_u64(mut value: u64) -> Self {
        let mut buffer = [0u8; 20];
        let mut at = buffer.len();
        loop {
            at -= 1;
            buffer[at] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        let mut decimal = Decimal {
            digits: [0; 17],
            len: 0,
            point: (buffer.len() - at) as i32,
        };
        let mut end = buffer.len();
        while end - at > 1 && buffer[end - 1] == b'0' {
            end -= 1;
        }
        decimal.len = end - at;
        decimal.digits[..decimal.len].copy_from_slice(&buffer[at..end]);
        decimal
    }
}

/// `Number::toString(value)` in radix 10, without a heap allocation on the
/// common paths: an integer below 2^53 from a `u64`, a short decimal from
/// one multiply ([`short_decimal`]), anything else from Rust's shortest
/// formatting into a stack buffer, its exact ties moved to even
/// ([`shortest`]). A power of two keeps the walk of
/// [`super::radix::decimal_sequence`], whose round-trip interval is
/// asymmetric.
pub(crate) fn text(value: f64) -> NumberText {
    if value.is_nan() {
        return NumberText::of(b"NaN");
    }
    if value.is_infinite() {
        return NumberText::of(if value > 0.0 {
            b"Infinity"
        } else {
            b"-Infinity"
        });
    }
    if value == 0.0 {
        return NumberText::of(b"0");
    }
    let magnitude = value.abs();
    let decimal = if magnitude < 9_007_199_254_740_992.0 && magnitude.fract() == 0.0 {
        Decimal::of_u64(magnitude as u64)
    } else if let Some(decimal) = short_decimal(magnitude) {
        decimal
    } else if magnitude.to_bits() & ((1 << 52) - 1) == 0 {
        let (digits, point) = super::radix::decimal_sequence(magnitude);
        let mut decimal = Decimal {
            digits: [0; 17],
            len: digits.len(),
            point,
        };
        decimal.digits[..digits.len()].copy_from_slice(digits.as_bytes());
        decimal
    } else {
        shortest(magnitude)
    };
    place(value < 0.0, &decimal)
}

/// The shortest decimal of a non-integral `magnitude` in `[1e-4, 1e15)` when
/// it has at most 16 significant digits, from one multiply; `None` when it
/// needs more (or lies outside the range).
///
/// At `d` fraction digits each integer `n` near `m = magnitude × 10^d` is a
/// candidate, and with `n < 2^53` and `10^d` exact, `n / 10^d` is the
/// correctly rounded quotient, which is what reading the decimal back gives.
/// While `m < 2^51`, `10^d` times an ulp of `magnitude` is under a half, so
/// at most one `n` round-trips, among the four integers around `m`. So one
/// try at the largest such `d` decides: the shortest round-tripping decimal,
/// scaled to `d` digits, is that candidate, and its trailing zeros come off.
/// Being the only one, it is also the one QuickJS's nearest-even walk finds.
/// (zipp 0d2445f3's `fmt_short_decimal_into`.)
fn short_decimal(magnitude: f64) -> Option<Decimal> {
    const POWERS: [f64; 16] = [
        1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15,
    ];
    const LIMIT: f64 = 2_251_799_813_685_248.0; // 2^51
    if !(1e-4..1e15).contains(&magnitude) {
        return None;
    }
    let mut places = 15usize;
    while places > 0 && magnitude * POWERS[places] >= LIMIT {
        places -= 1;
    }
    if places == 0 {
        return None;
    }
    let scale = POWERS[places];
    let scaled = magnitude * scale;
    let near = scaled.round() as u64;
    let base = (scaled.floor() as u64).saturating_sub(1);
    let n = std::iter::once(near)
        .chain((base..base + 4).filter(|&n| n != near))
        .find(|&n| n as f64 / scale == magnitude)?;
    let mut decimal = Decimal::of_u64(n);
    decimal.point -= places as i32;
    Some(decimal)
}

/// The fewest digits that round-trip, from Rust's shortest `{:e}` written
/// into a stack buffer. Where the value lies exactly halfway between two
/// such digit strings Rust keeps the upper one; QuickJS rounds to even.
fn shortest(magnitude: f64) -> Decimal {
    use std::fmt::Write;
    struct Buffer {
        bytes: [u8; 32],
        len: usize,
    }
    impl Write for Buffer {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            let end = self.len + text.len();
            let slot = self.bytes.get_mut(self.len..end).ok_or(std::fmt::Error)?;
            slot.copy_from_slice(text.as_bytes());
            self.len = end;
            Ok(())
        }
    }
    let mut buffer = Buffer {
        bytes: [0; 32],
        len: 0,
    };
    write!(buffer, "{magnitude:e}").expect("at most 24 bytes");
    let text = &buffer.bytes[..buffer.len];
    let at_e = text
        .iter()
        .position(|&byte| byte == b'e')
        .expect("an exponent");
    let mut decimal = Decimal {
        digits: [0; 17],
        len: 0,
        point: 1,
    };
    for &byte in &text[..at_e] {
        if byte != b'.' {
            decimal.digits[decimal.len] = byte;
            decimal.len += 1;
        }
    }
    let (negative, exponent) = match &text[at_e + 1..] {
        [b'-', rest @ ..] => (true, rest),
        rest => (false, rest),
    };
    let exponent = exponent
        .iter()
        .fold(0i32, |sum, &byte| sum * 10 + i32::from(byte - b'0'));
    decimal.point += if negative { -exponent } else { exponent };
    even_tie(magnitude, &mut decimal);
    decimal
}

/// Moves odd shortest digits to the even neighbor when `magnitude` is
/// exactly halfway between the two. The value `m × 2^q` (odd `m`) is such a
/// midpoint only when its last digit, a 5, sits just past the shortest
/// digits, at `10^q`; then `m × 5^-q` is ten times the digits plus or minus
/// five. The interval around a value that is not a power of two is
/// symmetric, and its edges are never at a midpoint, so both neighbors
/// round-trip and the even one is equally short.
fn even_tie(magnitude: f64, decimal: &mut Decimal) {
    let last = decimal.digits[decimal.len - 1];
    if (last - b'0') % 2 == 0 {
        return;
    }
    // The exponent of the digit past the last one.
    let past = decimal.point - decimal.len as i32 - 1;
    let bits = magnitude.to_bits();
    let biased = ((bits >> 52) & 0x7FF) as i32;
    let fraction = bits & ((1 << 52) - 1);
    let (mut mantissa, mut exponent) = if biased == 0 {
        (fraction, -1074)
    } else {
        (fraction | (1 << 52), biased - 1075)
    };
    let zeros = mantissa.trailing_zeros();
    mantissa >>= zeros;
    exponent += zeros as i32;
    // `m × 5^-q` stays below 2^128 for `-q <= 25`.
    if exponent != past || !(-25..0).contains(&past) {
        return;
    }
    let exact = u128::from(mantissa) * 5u128.pow((-past) as u32);
    let digits = decimal.digits[..decimal.len]
        .iter()
        .fold(0u128, |sum, &byte| sum * 10 + u128::from(byte - b'0'));
    let even = if exact + 5 == digits * 10 {
        digits - 1
    } else if exact == digits * 10 + 5 {
        digits + 1
    } else {
        return;
    };
    // An odd last digit moves by one without a carry or a borrow out of it,
    // except `9 + 1`, which a shortest string never needs.
    if even % 10 == 0 {
        return;
    }
    let mut rest = even;
    for at in (0..decimal.len).rev() {
        decimal.digits[at] = b'0' + (rest % 10) as u8;
        rest /= 10;
    }
}

/// Lays out `decimal` as QuickJS does: positional for a decimal point from
/// `10^-6` up to `10^21`, exponential outside.
fn place(negative: bool, decimal: &Decimal) -> NumberText {
    let digits = &decimal.digits[..decimal.len];
    let (len, point) = (decimal.len, decimal.point);
    let mut bytes = [b'0'; 25];
    let mut at = usize::from(negative);
    bytes[0] = if negative { b'-' } else { b'0' };
    if point <= -6 || point > 21 {
        bytes[at] = digits[0];
        at += 1;
        if len > 1 {
            bytes[at] = b'.';
            bytes[at + 1..at + len].copy_from_slice(&digits[1..]);
            at += len;
        }
        let exponent = point - 1;
        bytes[at] = b'e';
        bytes[at + 1] = if exponent < 0 { b'-' } else { b'+' };
        at += 2;
        let exponent = exponent.unsigned_abs();
        if exponent >= 100 {
            bytes[at] = b'0' + (exponent / 100) as u8;
            at += 1;
        }
        if exponent >= 10 {
            bytes[at] = b'0' + (exponent / 10 % 10) as u8;
            at += 1;
        }
        bytes[at] = b'0' + (exponent % 10) as u8;
        at += 1;
    } else if point <= 0 {
        // `0.`, then `-point` zeros (already in the buffer), then the digits.
        bytes[at + 1] = b'.';
        at += 2 + (-point) as usize;
        bytes[at..at + len].copy_from_slice(digits);
        at += len;
    } else if point as usize >= len {
        // The digits, then zeros up to the point.
        bytes[at..at + len].copy_from_slice(digits);
        at += point as usize;
    } else {
        let point = point as usize;
        bytes[at..at + point].copy_from_slice(&digits[..point]);
        bytes[at + point] = b'.';
        bytes[at + point + 1..at + len + 1].copy_from_slice(&digits[point..]);
        at += len + 1;
    }
    NumberText {
        bytes,
        len: at as u8,
    }
}

/// The JavaScript white space and line terminators `StringToNumber` trims.
pub(crate) fn is_space(unit: u16) -> bool {
    matches!(
        unit,
        0x09 | 0x0A | 0x0B | 0x0C | 0x0D | 0x20 | 0xA0 | 0x1680 | 0x2000
            ..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF
    )
}

/// `StringToNumber` over UTF-16 code units.
pub(crate) fn parse(units: &[u16]) -> f64 {
    let start = units.iter().position(|&unit| !is_space(unit));
    let Some(start) = start else {
        return 0.0;
    };
    let end = units.iter().rposition(|&unit| !is_space(unit)).unwrap() + 1;
    let Ok(text) = String::from_utf16(&units[start..end]) else {
        return f64::NAN;
    };
    parse_trimmed(&text)
}

fn parse_trimmed(text: &str) -> f64 {
    let bytes = text.as_bytes();
    if bytes.len() > 2 && bytes[0] == b'0' {
        let radix = match bytes[1] {
            b'x' | b'X' => 16,
            b'o' | b'O' => 8,
            b'b' | b'B' => 2,
            _ => 0,
        };
        if radix != 0 {
            let mut value = 0.0f64;
            for &byte in &bytes[2..] {
                let Some(digit) = (byte as char).to_digit(radix) else {
                    return f64::NAN;
                };
                value = value * f64::from(radix) + f64::from(digit);
            }
            return value;
        }
    }
    let (sign, unsigned) = match bytes.first() {
        Some(b'-') => (-1.0, &text[1..]),
        Some(b'+') => (1.0, &text[1..]),
        _ => (1.0, text),
    };
    if unsigned == "Infinity" {
        return sign * f64::INFINITY;
    }
    if !is_decimal_literal(unsigned.as_bytes()) {
        return f64::NAN;
    }
    sign * unsigned.parse::<f64>().unwrap_or(f64::NAN)
}

/// `StrUnsignedDecimalLiteral`: digits with an optional fraction and
/// exponent; Rust's parser alone would also take `inf` and `nan`.
fn is_decimal_literal(bytes: &[u8]) -> bool {
    let mut at = 0;
    let digits = |at: &mut usize| {
        let start = *at;
        while *at < bytes.len() && bytes[*at].is_ascii_digit() {
            *at += 1;
        }
        *at - start
    };
    let mut mantissa = digits(&mut at);
    if at < bytes.len() && bytes[at] == b'.' {
        at += 1;
        mantissa += digits(&mut at);
    }
    if mantissa == 0 {
        return false;
    }
    if at < bytes.len() && (bytes[at] == b'e' || bytes[at] == b'E') {
        at += 1;
        if at < bytes.len() && (bytes[at] == b'+' || bytes[at] == b'-') {
            at += 1;
        }
        if digits(&mut at) == 0 {
            return false;
        }
    }
    at == bytes.len()
}

/// `Number::exponentiate`: IEEE `pow` except where ECMA-262 differs.
pub(crate) fn pow(base: f64, exponent: f64) -> f64 {
    if exponent.is_nan() || (base.abs() == 1.0 && exponent.is_infinite()) {
        return f64::NAN;
    }
    base.powf(exponent)
}

/// Decodes an IEEE-754 binary16 (QuickJS `cutils.h::fromfp16`): the half
/// payload placed in a binary64 and scaled by 2^1008, which keeps signed
/// zero, the infinities and the NaN sign.
pub(crate) fn from_float16_bits(value: u16) -> f64 {
    let mut magnitude = u32::from(value & 0x7fff);
    if magnitude >= 0x7c00 {
        magnitude += 0x1f_8000;
    }
    let bits =
        (u64::from(value >> 15) << 63) | (u64::from(magnitude) << (f64::MANTISSA_DIGITS - 11));
    const SCALE_2_POW_1008: f64 = f64::from_bits(0x7ef0_0000_0000_0000);
    f64::from_bits(bits) * SCALE_2_POW_1008
}

/// Encodes a binary64 as binary16 (QuickJS `cutils.h::tofp16`): nearest,
/// ties to even; overflow is infinity; NaN keeps its sign with payload 1.
pub(crate) fn to_float16_bits(value: f64) -> u16 {
    let bits = value.to_bits();
    let sign = ((bits >> 63) << 15) as u16;
    let mut magnitude = bits & 0x7fff_ffff_ffff_ffff;
    let encoded = if magnitude > 0x7ff0_0000_0000_0000 {
        0x7c01
    } else if magnitude < 0x3f10_0000_0000_0000 {
        if magnitude <= 0x3e60_0000_0000_0000 {
            0
        } else {
            let shift = (1051 - (magnitude >> 52)) as u32;
            magnitude = (1_u64 << 52) | (magnitude & ((1_u64 << 52) - 1));
            let addend = ((magnitude >> shift) & 1) + ((1_u64 << (shift - 1)) - 1);
            (magnitude + addend) >> shift
        }
    } else {
        magnitude -= 0x3f00_0000_0000_0000;
        let addend = ((magnitude >> (52 - 10)) & 1) + ((1_u64 << (52 - 11)) - 1);
        let rounded = (magnitude + addend) >> (52 - 10);
        if rounded > 0x7c00 { 0x7c00 } else { rounded }
    };
    encoded as u16 | sign
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_to_string_matches_ecma262() {
        for (value, text) in [
            (1.0, "1"),
            (-1.5, "-1.5"),
            (0.1, "0.1"),
            (1e21, "1e+21"),
            (1e20, "100000000000000000000"),
            (123456789.125, "123456789.125"),
            (1.5e-7, "1.5e-7"),
            (0.000001, "0.000001"),
            (2f64.powi(53), "9007199254740992"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (-0.0, "0"),
            (f64::NAN, "NaN"),
            (f64::NEG_INFINITY, "-Infinity"),
            (0.1 + 0.2, "0.30000000000000004"),
        ] {
            assert_eq!(to_string(value), text, "{value}");
        }
    }

    #[test]
    fn string_to_number() {
        let parse = |text: &str| parse(&text.encode_utf16().collect::<Vec<_>>());
        assert_eq!(parse(""), 0.0);
        assert_eq!(parse("  42  "), 42.0);
        assert_eq!(parse("0x1F"), 31.0);
        assert_eq!(parse("-0x1F").to_bits(), f64::NAN.to_bits());
        assert_eq!(parse("1e3"), 1000.0);
        assert_eq!(parse(".5"), 0.5);
        assert_eq!(parse("5."), 5.0);
        assert_eq!(parse("-Infinity"), f64::NEG_INFINITY);
        assert!(parse("inf").is_nan());
        assert!(parse("1e").is_nan());
        assert!(parse("abc").is_nan());
        assert!(parse(".").is_nan());
    }

    #[test]
    fn int32_conversions() {
        assert_eq!(to_int32(4_294_967_296.0 + 5.0), 5);
        assert_eq!(to_int32(2_147_483_648.0), i32::MIN);
        assert_eq!(to_int32(-1.5), -1);
        assert_eq!(to_uint32(-1.0), u32::MAX);
        assert_eq!(to_int32(f64::NAN), 0);
        assert_eq!(number(-0.0), Value::number(-0.0));
        assert_eq!(number(3.0), Value::int32(3));
    }
}
