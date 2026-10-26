//! Small helpers that reproduce Python semantics the original readAFP relies
//! on (banker's rounding, `str.isprintable`, `str.split()` whitespace, ...),
//! plus big-endian field readers.

/// Python's `round()` for floats: round half to even.
pub fn py_round(v: f64) -> i64 {
    v.round_ties_even() as i64
}

/// Python's floor division for integers (`a // b`). Panics, like integer
/// division, when `b == 0`.
pub fn floor_div(a: i64, b: i64) -> i64 {
    let q = a / b;
    if a % b != 0 && ((a < 0) != (b < 0)) { q - 1 } else { q }
}

/// Python `int(x)` on a float: truncate toward zero.
pub fn trunc(v: f64) -> i64 {
    v.trunc() as i64
}

/// Big-endian unsigned integer of `data[off..off+n]`; missing bytes read as
/// absent (like a short Python slice), i.e. only the bytes present count.
pub fn be_uint(data: &[u8], off: usize, n: usize) -> u64 {
    let end = (off + n).min(data.len());
    let mut v = 0u64;
    if off < end {
        for &b in &data[off..end] {
            v = (v << 8) | b as u64;
        }
    }
    v
}

pub fn u16_at(data: &[u8], off: usize) -> u16 {
    be_uint(data, off, 2) as u16
}

pub fn s16_at(data: &[u8], off: usize) -> i16 {
    be_uint(data, off, 2) as u16 as i16
}

pub fn u24_at(data: &[u8], off: usize) -> u32 {
    be_uint(data, off, 3) as u32
}

/// Signed 24-bit big-endian (two's complement).
pub fn s24_at(data: &[u8], off: usize) -> i32 {
    let v = u24_at(data, off) as i32;
    if v & 0x80_0000 != 0 { v - 0x100_0000 } else { v }
}

pub fn u32_at(data: &[u8], off: usize) -> u32 {
    be_uint(data, off, 4) as u32
}

/// Python-style slice `data[start:end]` that clamps instead of panicking.
pub fn slice(data: &[u8], start: usize, end: usize) -> &[u8] {
    let end = end.min(data.len());
    let start = start.min(end);
    &data[start..end]
}

/// Python-style `data[start:]`.
pub fn tail(data: &[u8], start: usize) -> &[u8] {
    &data[start.min(data.len())..]
}

/// Lower-case hex with a separator, like `bytes.hex(" ")`.
pub fn hex_sep(data: &[u8], sep: &str) -> String {
    let mut out = String::with_capacity(data.len() * 3);
    for (i, b) in data.iter().enumerate() {
        if i > 0 {
            out.push_str(sep);
        }
        out.push_str(&format!("{b:02x}"));
    }
    out
}

pub fn hex(data: &[u8]) -> String {
    hex_sep(data, "")
}

/// Python `str.isprintable()` for one character.
///
/// Non-printable are the Unicode categories Cc, Cf, Cs, Co, Cn, Zl, Zp and
/// Zs (except the ASCII space). Exact for the Latin-1 range our decoders
/// produce; a close approximation elsewhere.
pub fn is_printable(c: char) -> bool {
    let cp = c as u32;
    if c == ' ' {
        return true;
    }
    if c.is_control() {
        return false;
    }
    !matches!(
        cp,
        0xA0 | 0xAD
            | 0x1680
            | 0x2000..=0x200F
            | 0x2028..=0x202F
            | 0x205F..=0x2064
            | 0x2066..=0x206F
            | 0x3000
            | 0xFEFF
            | 0xFFF9..=0xFFFB
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x180E
            | 0xD800..=0xF8FF
            | 0xFDD0..=0xFDEF
            | 0xFFFE
            | 0xFFFF
    )
}

/// Python `str.isspace()` for one character.
pub fn is_py_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\x0b'
            | '\x0c'
            | '\r'
            | '\x1c'..='\x1f'
            | ' '
            | '\u{85}'
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

/// First position of `needle` (non-empty) in `hay`.
pub(crate) fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// First position of `needle` (non-empty) in `hay` at or after `from`.
pub(crate) fn find_bytes_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    find_bytes(hay.get(from..)?, needle).map(|p| p + from)
}

/// Python `str.strip()` (Unicode whitespace).
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// Python `" ".join(s.split())`.
pub fn collapse_ws(s: &str) -> String {
    s.split(is_py_space).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ")
}

/// Python `bool(s.strip())`.
pub fn has_non_space(s: &str) -> bool {
    s.chars().any(|c| !is_py_space(c))
}

/// Python `str.isdigit()` over a whole string (non-empty, all digits).
pub fn is_all_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// Python `repr()` of a str, for summaries that use `{x!r}`.
pub fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !is_printable(c) => {
                let cp = c as u32;
                if cp < 0x100 {
                    out.push_str(&format!("\\x{cp:02x}"));
                } else if cp < 0x10000 {
                    out.push_str(&format!("\\u{cp:04x}"));
                } else {
                    out.push_str(&format!("\\U{cp:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `format(v, "g")` for floats as used in summaries (`{v:g}`).
pub fn fmt_g(v: f64) -> String {
    fmt_general(v, 6)
}

/// Python `format(v, ".{prec}g")`.
pub fn fmt_general(v: f64, prec: usize) -> String {
    if v == 0.0 {
        return if v.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    if !v.is_finite() {
        return if v.is_nan() {
            "nan".into()
        } else if v > 0.0 {
            "inf".into()
        } else {
            "-inf".into()
        };
    }
    let p = prec.max(1);
    // Exponent after rounding to p significant digits.
    let sci = format!("{:.*e}", p - 1, v);
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    if exp < -4 || exp >= p as i32 {
        let mant = trim_fraction(mant);
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mant}e{sign}{:02}", exp.abs())
    } else {
        let decimals = (p as i32 - 1 - exp).max(0) as usize;
        trim_fraction(&format!("{v:.decimals$}")).to_string()
    }
}

fn trim_fraction(s: &str) -> &str {
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.') } else { s }
}

/// XML-escape text content like `xml.sax.saxutils.escape`, and drop the
/// characters XML 1.0 forbids (C0 controls other than tab and newlines,
/// U+FFFE, U+FFFF), which decoded text can contain and which would make the
/// whole SVG unparsable.
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\t' | '\n' | '\r' => out.push(c),
            '\0'..='\x1f' | '\u{fffe}' | '\u{ffff}' => {}
            c => out.push(c),
        }
    }
    out
}

/// A quoted XML attribute value like `xml.sax.saxutils.quoteattr`.
pub fn quote_attr(s: &str) -> String {
    let mut esc = xml_escape(s).replace('\n', "&#10;").replace('\r', "&#13;").replace('\t', "&#9;");
    if esc.contains('"') {
        if esc.contains('\'') {
            esc = esc.replace('"', "&quot;");
            format!("\"{esc}\"")
        } else {
            format!("'{esc}'")
        }
    } else {
        format!("\"{esc}\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_is_bankers() {
        assert_eq!(py_round(0.5), 0);
        assert_eq!(py_round(1.5), 2);
        assert_eq!(py_round(2.5), 2);
        assert_eq!(py_round(-0.5), 0);
    }

    #[test]
    fn floor_division() {
        assert_eq!(floor_div(-7, 2), -4);
        assert_eq!(floor_div(7, 2), 3);
        assert_eq!(floor_div(-6, 3), -2);
    }

    #[test]
    fn general_format() {
        assert_eq!(fmt_g(12.0), "12");
        assert_eq!(fmt_g(10.5), "10.5");
        assert_eq!(fmt_general(1234.0, 3), "1.23e+03");
        assert_eq!(fmt_general(0.000123, 3), "0.000123");
        assert_eq!(fmt_general(-45.0, 3), "-45");
    }

    #[test]
    fn signed24() {
        assert_eq!(s24_at(&[0xFF, 0xFF, 0xFF], 0), -1);
        assert_eq!(s24_at(&[0x00, 0x01, 0x00], 0), 256);
    }

    #[test]
    fn repr_quotes() {
        assert_eq!(py_repr("O1ABC"), "'O1ABC'");
        assert_eq!(py_repr("it's"), "\"it's\"");
    }
}
