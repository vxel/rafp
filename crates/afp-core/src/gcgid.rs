//! Bridge an external code page to an embedded font's glyphs via GCGID.
//!
//! When a document's coded font names an *external* code page we have the
//! codec (byte → Unicode) but not the code page's byte → GCGID map. Yet the
//! file may still embed the *character set*, whose glyphs are keyed by IBM
//! Graphic Character Global Identifiers. This module reconstructs the
//! missing byte → GCGID map from the codec plus an authoritative
//! character → GCGID table for IBM character set 103 (FOCA reference
//! Figure 56, "EBCDIC Code Page 500 With Character Set 103"). For other
//! characters the algorithmic `UNICxxxx` identifier is tried.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::codepages::{Codec, ebcdic};

/// IBM character set 103: EBCDIC code page 500 byte → GCGID.
const CS103_CP500: &[(u8, &str)] = &[
    (0x40, "SP010000"),
    (0x4A, "SM060000"),
    (0x4B, "SP110000"),
    (0x4C, "SA030000"),
    (0x4D, "SP060000"),
    (0x4E, "SA010000"),
    (0x4F, "SP020000"),
    (0x50, "SM030000"),
    (0x5A, "SM080000"),
    (0x5B, "SC030000"),
    (0x5C, "SM040000"),
    (0x5D, "SP070000"),
    (0x5E, "SP140000"),
    (0x5F, "SD150000"),
    (0x60, "SP100000"),
    (0x61, "SP120000"),
    (0x6B, "SP080000"),
    (0x6C, "SM020000"),
    (0x6D, "SP090000"),
    (0x6E, "SA050000"),
    (0x6F, "SP150000"),
    (0x79, "SD130000"),
    (0x7A, "SP130000"),
    (0x7B, "SM010000"),
    (0x7C, "SM050000"),
    (0x7D, "SP050000"),
    (0x7E, "SA040000"),
    (0x7F, "SP040000"),
    (0x81, "LA010000"),
    (0x82, "LB010000"),
    (0x83, "LC010000"),
    (0x84, "LD010000"),
    (0x85, "LE010000"),
    (0x86, "LF010000"),
    (0x87, "LG010000"),
    (0x88, "LH010000"),
    (0x89, "LI010000"),
    (0x91, "LJ010000"),
    (0x92, "LK010000"),
    (0x93, "LL010000"),
    (0x94, "LM010000"),
    (0x95, "LN010000"),
    (0x96, "LO010000"),
    (0x97, "LP010000"),
    (0x98, "LQ010000"),
    (0x99, "LR010000"),
    (0xA1, "SD190000"),
    (0xA2, "LS010000"),
    (0xA3, "LT010000"),
    (0xA4, "LU010000"),
    (0xA5, "LV010000"),
    (0xA6, "LW010000"),
    (0xA7, "LX010000"),
    (0xA8, "LY010000"),
    (0xA9, "LZ010000"),
    (0xBB, "SM130000"),
    (0xC0, "SM110000"),
    (0xC1, "LA020000"),
    (0xC2, "LB020000"),
    (0xC3, "LC020000"),
    (0xC4, "LD020000"),
    (0xC5, "LE020000"),
    (0xC6, "LF020000"),
    (0xC7, "LG020000"),
    (0xC8, "LH020000"),
    (0xC9, "LI020000"),
    (0xD0, "SM140000"),
    (0xD1, "LJ020000"),
    (0xD2, "LK020000"),
    (0xD3, "LL020000"),
    (0xD4, "LM020000"),
    (0xD5, "LN020000"),
    (0xD6, "LO020000"),
    (0xD7, "LP020000"),
    (0xD8, "LQ020000"),
    (0xD9, "LR020000"),
    (0xE0, "SM070000"),
    (0xE2, "LS020000"),
    (0xE3, "LT020000"),
    (0xE4, "LU020000"),
    (0xE5, "LV020000"),
    (0xE6, "LW020000"),
    (0xE7, "LX020000"),
    (0xE8, "LY020000"),
    (0xE9, "LZ020000"),
    (0xF0, "ND100000"),
    (0xF1, "ND010000"),
    (0xF2, "ND020000"),
    (0xF3, "ND030000"),
    (0xF4, "ND040000"),
    (0xF5, "ND050000"),
    (0xF6, "ND060000"),
    (0xF7, "ND070000"),
    (0xF8, "ND080000"),
    (0xF9, "ND090000"),
];

struct Maps {
    char_to_gcgid: HashMap<char, &'static str>,
    gcgid_to_char: HashMap<&'static str, char>,
}

fn maps() -> &'static Maps {
    static MAPS: OnceLock<Maps> = OnceLock::new();
    MAPS.get_or_init(|| {
        let mut char_to_gcgid = HashMap::new();
        let mut gcgid_to_char = HashMap::new();
        for &(b, g) in CS103_CP500 {
            let c = ebcdic(&[b]).chars().next().unwrap();
            char_to_gcgid.insert(c, g);
            gcgid_to_char.insert(g, c);
        }
        Maps { char_to_gcgid, gcgid_to_char }
    })
}

/// The authoritative GCGID for a character: CS103 first, then `UNICxxxx`
/// for any other BMP scalar.
pub fn gcgid_for_char(c: char) -> Option<String> {
    if let Some(g) = maps().char_to_gcgid.get(&c) {
        return Some((*g).to_string());
    }
    let code = c as u32;
    if code > 0 && code <= 0xFFFF { Some(format!("UNIC{code:04X}")) } else { None }
}

/// The character a GCGID identifies: CS103 names, then `UNICxxxx`.
pub fn char_for_gcgid(gcgid: &str) -> Option<char> {
    if let Some(c) = maps().gcgid_to_char.get(gcgid) {
        return Some(*c);
    }
    if gcgid.len() == 8 && gcgid.starts_with("UNIC") {
        let code = u32::from_str_radix(&gcgid[4..], 16).ok()?;
        if code > 0 && code <= 0xFFFF && !(0xD800..=0xDFFF).contains(&code) {
            return char::from_u32(code);
        }
    }
    None
}

/// A code page's byte → GCGID map.
pub type CodePageMap = HashMap<u8, String>;

/// Decode bytes through an embedded code page; `None` unless **every** byte
/// resolves, so a partially decodable run falls back as a whole.
pub fn decode_via_code_page(data: &[u8], cp_map: &CodePageMap) -> Option<String> {
    let mut out = String::with_capacity(data.len());
    for b in data {
        out.push(char_for_gcgid(cp_map.get(b)?)?);
    }
    Some(out)
}

/// Build a byte → GCGID map for an external code page, keeping only the
/// GCGIDs the embedded font actually carries.
pub fn bridge_code_page(codec: Codec, font_gcgids: &HashSet<String>) -> CodePageMap {
    let mut out = HashMap::new();
    for b in 0..=255u8 {
        let Some(c) = codec.decode_byte(b) else { continue };
        if let Some(g) = gcgid_for_char(c)
            && font_gcgids.contains(&g)
        {
            out.insert(b, g);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cs103_roundtrip() {
        assert_eq!(gcgid_for_char('a').as_deref(), Some("LA010000"));
        assert_eq!(gcgid_for_char('A').as_deref(), Some("LA020000"));
        assert_eq!(gcgid_for_char(' ').as_deref(), Some("SP010000"));
        assert_eq!(gcgid_for_char('€').as_deref(), Some("UNIC20AC"));
        assert_eq!(char_for_gcgid("ND010000"), Some('1'));
        assert_eq!(char_for_gcgid("UNIC0052"), Some('R'));
        assert_eq!(char_for_gcgid("UNICD800"), None);
        assert_eq!(char_for_gcgid("XX000000"), None);
    }

    #[test]
    fn decode_requires_all_bytes() {
        let mut m = CodePageMap::new();
        m.insert(0xC1, "LA020000".into());
        assert_eq!(decode_via_code_page(&[0xC1, 0xC1], &m).as_deref(), Some("AA"));
        assert_eq!(decode_via_code_page(&[0xC1, 0xC2], &m), None);
    }

    #[test]
    fn bridge_keeps_only_font_glyphs() {
        let font: HashSet<String> = ["LA020000".to_string()].into_iter().collect();
        let m = bridge_code_page(Codec::lookup("cp037").unwrap(), &font);
        assert_eq!(m.len(), 1);
        assert_eq!(m[&0xC1], "LA020000");
    }
}
