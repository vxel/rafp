//! Text codecs: the code pages AFP text and names are encoded in.
//!
//! A [`Codec`] is identified by the same name the original readAFP used for
//! its Python codec (`"cp500"`, `"latin-1"`, `"utf-16-be"`, ...), since those
//! names surface in the inspector ("T1V10500 → cp500") and fidelity notes.

mod tables;

use std::fmt;

use tables::{TABLES, UNDEFINED};

#[derive(Clone, Copy)]
enum Kind {
    Table(&'static [u16; 256]),
    Ascii,
    Latin1,
    Utf8,
    Utf16Be,
    Multi(&'static encoding_rs::Encoding),
}

/// A named byte → text decoder.
#[derive(Clone, Copy)]
pub struct Codec {
    name: &'static str,
    kind: Kind,
}

impl fmt::Debug for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Codec({})", self.name)
    }
}

impl PartialEq for Codec {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Codec {}

impl Codec {
    /// Look a codec up by its (Python) name. Accepts the canonical names
    /// returned by [`codec_for_cpgid`] plus a few aliases.
    pub fn lookup(name: &str) -> Option<Codec> {
        let lower = name.to_ascii_lowercase();
        let (canon, kind): (&'static str, Kind) = match lower.as_str() {
            "ascii" | "us-ascii" => ("ascii", Kind::Ascii),
            "latin-1" | "latin1" | "iso-8859-1" | "iso8859-1" => ("latin-1", Kind::Latin1),
            "utf-8" | "utf8" => ("utf-8", Kind::Utf8),
            "cp65001" => ("cp65001", Kind::Utf8),
            "utf-16-be" | "utf-16be" | "utf16be" => ("utf-16-be", Kind::Utf16Be),
            "cp932" => ("cp932", Kind::Multi(encoding_rs::SHIFT_JIS)),
            "cp936" => ("cp936", Kind::Multi(encoding_rs::GBK)),
            "cp949" => ("cp949", Kind::Multi(encoding_rs::EUC_KR)),
            "cp950" => ("cp950", Kind::Multi(encoding_rs::BIG5)),
            other => {
                let (n, t) = TABLES.iter().find(|(n, _)| *n == other)?;
                (n, Kind::Table(t))
            }
        };
        Some(Codec { name: canon, kind })
    }

    /// EBCDIC code page 500, the AFP default.
    pub fn cp500() -> Codec {
        // Looked up once: EBCDIC names are decoded with it all the time.
        static CP500: std::sync::OnceLock<Codec> = std::sync::OnceLock::new();
        *CP500.get_or_init(|| Codec::lookup("cp500").expect("cp500 table"))
    }

    pub fn latin1() -> Codec {
        Codec { name: "latin-1", kind: Kind::Latin1 }
    }

    pub fn utf16be() -> Codec {
        Codec { name: "utf-16-be", kind: Kind::Utf16Be }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Strict decode: `None` when any byte is undefined in this code page
    /// (Python's `bytes.decode(codec)` raising `UnicodeDecodeError`).
    pub fn decode_strict(&self, data: &[u8]) -> Option<String> {
        match self.kind {
            Kind::Table(t) => {
                let mut out = String::with_capacity(data.len());
                for &b in data {
                    let u = t[b as usize];
                    if u == UNDEFINED {
                        return None;
                    }
                    out.push(char::from_u32(u as u32)?);
                }
                Some(out)
            }
            Kind::Ascii => {
                if data.iter().all(|b| *b < 0x80) {
                    Some(data.iter().map(|&b| b as char).collect())
                } else {
                    None
                }
            }
            Kind::Latin1 => Some(data.iter().map(|&b| b as char).collect()),
            Kind::Utf8 => std::str::from_utf8(data).ok().map(str::to_owned),
            Kind::Utf16Be => {
                if !data.len().is_multiple_of(2) {
                    return None;
                }
                let units = data.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]]));
                char::decode_utf16(units).collect::<Result<String, _>>().ok()
            }
            Kind::Multi(enc) => enc.decode_without_bom_handling_and_without_replacement(data).map(|c| c.into_owned()),
        }
    }

    /// Lenient decode: undefined bytes become U+FFFD (Python
    /// `errors="replace"`).
    pub fn decode_replace(&self, data: &[u8]) -> String {
        match self.kind {
            Kind::Table(t) => data
                .iter()
                .map(|&b| {
                    let u = t[b as usize];
                    if u == UNDEFINED { '\u{FFFD}' } else { char::from_u32(u as u32).unwrap_or('\u{FFFD}') }
                })
                .collect(),
            Kind::Ascii => data.iter().map(|&b| if b < 0x80 { b as char } else { '\u{FFFD}' }).collect(),
            Kind::Latin1 => data.iter().map(|&b| b as char).collect(),
            Kind::Utf8 => String::from_utf8_lossy(data).into_owned(),
            Kind::Utf16Be => {
                let mut units: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                let mut s: String = char::decode_utf16(units.drain(..)).map(|r| r.unwrap_or('\u{FFFD}')).collect();
                if data.len() % 2 == 1 {
                    s.push('\u{FFFD}');
                }
                s
            }
            Kind::Multi(enc) => enc.decode_without_bom_handling(data).0.into_owned(),
        }
    }

    /// Decode a single byte, if this codec maps it on its own.
    pub fn decode_byte(&self, b: u8) -> Option<char> {
        let s = self.decode_strict(&[b])?;
        let mut it = s.chars();
        let c = it.next()?;
        if it.next().is_some() { None } else { Some(c) }
    }
}

/// Decode with the default AFP code page (cp500, never fails).
pub fn ebcdic(data: &[u8]) -> String {
    Codec::cp500().decode_replace(data)
}

/// EBCDIC (cp500) decode, then strip surrounding whitespace: how resource
/// names and other fixed-length name fields read.
pub fn ebcdic_strip(data: &[u8]) -> String {
    crate::util::py_strip(&ebcdic(data)).to_string()
}

/// `data` as UTF-16BE text when it looks like it: an even length with a zero
/// high byte for at least 80 % of the characters (Latin text), and valid
/// UTF-16.
pub fn utf16be_if_likely(data: &[u8]) -> Option<String> {
    if data.len() < 2 || !data.len().is_multiple_of(2) {
        return None;
    }
    let zeros = data.iter().step_by(2).filter(|&&hi| hi == 0).count();
    if (zeros as f64) < (data.len() / 2) as f64 * 0.8 {
        return None;
    }
    Codec::utf16be().decode_strict(data)
}

/// Python codec name for an IBM CPGID / CCSID, if a decoder exists.
pub fn codec_for_cpgid(cpgid: u32) -> Option<Codec> {
    if cpgid == 0 {
        return None;
    }
    let special = match cpgid {
        367 => Some("ascii"),
        819 => Some("latin-1"),
        1200 | 13488 => Some("utf-16-be"),
        1208 => Some("utf-8"),
        _ => None,
    };
    if let Some(name) = special {
        return Codec::lookup(name);
    }
    Codec::lookup(&format!("cp{cpgid}")).or_else(|| Codec::lookup(&format!("cp{cpgid:03}")))
}

/// Resolve a code page resource name like `T1V10500` or `T1001140`: IBM
/// names end in the CPGID digits.
pub fn codec_for_codepage_name(name: &str) -> Option<Codec> {
    let chars: Vec<char> = name.chars().collect();
    for digits in [5usize, 4] {
        if chars.len() < digits {
            continue;
        }
        let tail: String = chars[chars.len() - digits..].iter().collect();
        if tail.chars().all(|c| c.is_ascii_digit())
            && let Ok(n) = tail.parse::<u32>()
            && let Some(c) = codec_for_cpgid(n)
        {
            return Some(c);
        }
    }
    None
}

/// Code pages offered in the UI for text the file leaves unlabeled.
pub const UI_CODEPAGES: &[(&str, &str)] = &[
    ("cp500", "cp500 (international, default)"),
    ("cp037", "cp037 (US / Canada)"),
    ("cp273", "cp273 (Germany / Austria)"),
    ("cp1047", "cp1047 (Latin-1 open systems)"),
    ("cp1141", "cp1141 (Germany, euro)"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp500_letters() {
        assert_eq!(ebcdic(&[0xC1, 0xC2, 0x81, 0xF0, 0x40]), "ABa0 ");
    }

    #[test]
    fn cp1047_and_cp1141_exist() {
        assert_eq!(Codec::lookup("cp1047").unwrap().decode_strict(&[0x5F]).unwrap(), "^");
        assert_eq!(Codec::lookup("cp1141").unwrap().decode_strict(&[0x9F]).unwrap(), "€");
    }

    #[test]
    fn cpgid_names() {
        assert_eq!(codec_for_cpgid(500).unwrap().name(), "cp500");
        assert_eq!(codec_for_cpgid(37).unwrap().name(), "cp037");
        assert_eq!(codec_for_cpgid(819).unwrap().name(), "latin-1");
        assert_eq!(codec_for_cpgid(1200).unwrap().name(), "utf-16-be");
        assert!(codec_for_cpgid(0).is_none());
        assert!(codec_for_cpgid(99999).is_none());
        assert_eq!(codec_for_codepage_name("T1V10500").unwrap().name(), "cp500");
        assert_eq!(codec_for_codepage_name("T1001140").unwrap().name(), "cp1140");
        assert!(codec_for_codepage_name("T1TR0388").is_none());
    }

    #[test]
    fn strict_decode_fails_on_undefined() {
        let cp1252 = Codec::lookup("cp1252").unwrap();
        assert!(cp1252.decode_strict(&[0x81]).is_none());
        assert_eq!(cp1252.decode_replace(&[0x41, 0x81]), "A\u{FFFD}");
    }
}
