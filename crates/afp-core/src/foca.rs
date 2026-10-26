//! FOCA (Font Object Content Architecture) font decoding.
//!
//! An AFP font character set is bracketed by sf::BFN...sf::EFN and carries the
//! actual glyph shapes. For raster (bitmap) fonts the relevant fields are:
//!
//! ```text
//! sf::FNC (Font Control)       resolution, pattern-data alignment
//! sf::FND (Font Descriptor)    typeface description name
//! sf::FNI (Font Index)         per character: GCGID, increment, sf::FNM index
//! sf::FNM (Font Patterns Map)  per pattern: box width/height, data offset
//! sf::FNG (Font Patterns)      the concatenated 1-bit-per-pel bitmaps
//! ```
//!
//! Raster glyph bitmaps are rebuilt as PNGs (dark pel on white). Outline
//! fonts (Type 1 / CFF) are decoded to vector glyphs by the `type1` and
//! `cff` modules.
//!
//! Reference: Font Object Content Architecture Reference, AFPC-0001-06.

use std::sync::Arc;

use indexmap::IndexMap;

use crate::cff::CffFont;
use crate::codepages::ebcdic;
use crate::outline::Glyph;
use crate::parser::{StructuredField, sf};
use crate::png::pack_png;
use crate::type1::Type1Font;
use crate::util::{fmt_g, is_printable, py_repr, py_strip, s16_at, u16_at, u32_at};

/// sf::FNC byte 1, Pattern Technology Identifier.
pub const PATTECH_RASTER: u8 = 0x05;
pub const PATTECH_TYPE1: u8 = 0x1E;
pub const PATTECH_CID: u8 = 0x1F;

const MAX_GLYPHS: usize = 1024;
const MAX_CHARS: usize = 4096;
const MAX_PATTERN_BYTES: usize = 1 << 20;

/// One raster glyph rebuilt as a PNG bitmap.
#[derive(Debug, Clone)]
pub struct RasterGlyph {
    pub gcgid: String,
    pub width: u32,
    pub height: u32,
    /// Inline advance, in the font's metric units.
    pub char_increment: i64,
    /// 1-bit grayscale PNG, dark pel on white.
    pub png: Arc<Vec<u8>>,
    /// Box-bottom depth below the baseline.
    pub baseline_offset: i64,
    /// sf::FNI A-space: box left edge offset from the pen.
    pub left_bearing: i64,
}

/// One character's identity and inline advance, with no shape data.
#[derive(Debug, Clone, Default)]
pub struct CharMetric {
    pub gcgid: String,
    pub char_increment: i64,
    /// Readable character name from the Font Name Map.
    pub name: String,
}

/// One sf::BFN...sf::EFN font character set.
#[derive(Debug, Clone, Default)]
pub struct Font {
    pub name: String,
    pub typeface: String,
    pub pattern_tech: u8,
    pub glyphs: Vec<RasterGlyph>,
    pub chars: Vec<CharMetric>,
    pub orientations: usize,
    pub outline_format: String,
    pub units_per_em: i64,
    pub outline_glyphs: IndexMap<String, Glyph>,
    /// Raster pattern resolution (pels/inch), from sf::FNC.
    pub resolution: i64,
    /// Nominal font size (points), from sf::FND.
    pub point_size: f64,
    /// sf::FND WeightClass (5 = medium, >= 7 = bold), 0 = n/a.
    pub weight_class: u8,
    /// sf::FNC unit base X'02': metrics in 1000/em.
    pub relative_metrics: bool,
}

impl Font {
    pub fn is_raster(&self) -> bool {
        self.pattern_tech == PATTECH_RASTER
    }

    pub fn is_outline(&self) -> bool {
        self.pattern_tech == PATTECH_TYPE1 || self.pattern_tech == PATTECH_CID
    }

    /// Human-readable technology, preferring the sf::FNG-detected format.
    pub fn tech_label(&self) -> String {
        if !self.outline_format.is_empty() {
            return self.outline_format.clone();
        }
        match self.pattern_tech {
            PATTECH_RASTER => "laser-matrix raster".into(),
            PATTECH_TYPE1 => "Type 1 (PFB) outline".into(),
            PATTECH_CID => "CID-keyed outline".into(),
            t => format!("unknown (X'{t:02X}')"),
        }
    }
}

/// Decode an EBCDIC name field, trimmed to its printable run.
pub fn decode_name(raw: &[u8]) -> String {
    let text = ebcdic(raw);
    let run: String = text.chars().take_while(|&c| is_printable(c) && c != '\0').collect();
    py_strip(&run).to_string()
}

/// Build a 1-bpp PNG from a FOCA raster pattern (1 = toned/dark).
fn glyph_png(pattern: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let row_bytes = (width as usize).div_ceil(8);
    if row_bytes * height as usize > MAX_PATTERN_BYTES {
        return None;
    }
    let mut out = vec![0xFFu8; row_bytes * height as usize];
    for (i, b) in out.iter_mut().enumerate() {
        if let Some(p) = pattern.get(i) {
            *b = p ^ 0xFF;
        }
    }
    Some(pack_png(width, height, 1, 0, row_bytes, &out))
}

fn decode_raster_glyphs(fnc: &[u8], fni: &[u8], fnm: &[u8], fng: &[u8]) -> Vec<RasterGlyph> {
    let align = match fnc.get(16) {
        Some(0x00) => 1,
        Some(0x02) => 4,
        Some(0x03) => 8,
        _ => 1,
    };
    let fni_rg = fnc.get(15).map(|&b| b as usize).unwrap_or(28);

    // sf::FNM index → (GCGID, increment, baseline offset, A-space).
    let mut by_pattern: IndexMap<usize, (String, i64, i64, i64)> = IndexMap::new();
    if fni_rg >= 18 {
        let mut i = 0;
        while i + fni_rg <= fni.len() {
            let gcgid = decode_name(&fni[i..i + 8]);
            let char_inc = u16_at(fni, i + 8) as i64;
            let baseline = s16_at(fni, i + 12) as i64;
            let fnm_index = u16_at(fni, i + 16) as usize;
            let aspace = if fni_rg >= 20 { s16_at(fni, i + 18) as i64 } else { 0 };
            by_pattern.entry(fnm_index).or_insert((gcgid, char_inc, baseline, aspace));
            i += fni_rg;
        }
    }

    let mut glyphs = Vec::new();
    let count = fnm.len() / 8;
    for idx in 0..count.min(MAX_GLYPHS) {
        let off = idx * 8;
        let box_w = u16_at(fnm, off) as u32 + 1;
        let box_h = u16_at(fnm, off + 2) as u32 + 1;
        let pat_off = u32_at(fnm, off + 4) as usize * align;
        let row_bytes = (box_w as usize).div_ceil(8);
        let pattern = crate::util::slice(fng, pat_off, pat_off + row_bytes * box_h as usize);
        let Some(png) = glyph_png(pattern, box_w, box_h) else { continue };
        let (gcgid, char_inc, baseline, aspace) = by_pattern.get(&idx).cloned().unwrap_or((String::new(), 0, 0, 0));
        glyphs.push(RasterGlyph {
            gcgid,
            width: box_w,
            height: box_h,
            char_increment: char_inc,
            png: Arc::new(png),
            baseline_offset: baseline,
            left_bearing: aspace,
        });
    }
    glyphs
}

/// GCGID + inline increment from the Font Index, ignoring shapes.
fn fni_metrics(fnc: &[u8], fni: &[u8]) -> Vec<CharMetric> {
    let fni_rg = fnc.get(15).map(|&b| b as usize).unwrap_or(28);
    if fni_rg < 10 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i + fni_rg <= fni.len() {
        out.push(CharMetric {
            gcgid: decode_name(&fni[i..i + 8]),
            char_increment: u16_at(fni, i + 8) as i64,
            name: String::new(),
        });
        if out.len() >= MAX_CHARS {
            break;
        }
        i += fni_rg;
    }
    out
}

/// Each GCGID's readable character name from the Font Name Map.
fn fnn_glyph_names(fnn: &[u8]) -> IndexMap<String, String> {
    let mut names = IndexMap::new();
    let mut i = 2;
    while i + 12 <= fnn.len() && names.len() < MAX_CHARS {
        let gcgid = decode_name(&fnn[i..i + 8]);
        let off = u32_at(fnn, i + 8) as usize;
        i += 12;
        if !gcgid.chars().next().is_some_and(char::is_alphabetic) {
            break; // reached the name pool
        }
        if off >= fnn.len() {
            continue;
        }
        let ln = fnn[off] as usize;
        if ln < 2 {
            continue;
        }
        let raw = crate::util::slice(fnn, off + 1, off + ln);
        if raw.is_ascii() {
            names.insert(gcgid, raw.iter().map(|&b| b as char).collect());
        }
    }
    names
}

/// True if the sf::FNG payload is a CFF program (bare or OTTO-wrapped).
fn is_cff(fng: &[u8]) -> bool {
    fng.starts_with(b"OTTO") || (fng.len() >= 4 && fng[0] == 1 && fng[1] == 0 && (4..=32).contains(&fng[2]))
}

/// Identify an outline font program from its sf::FNG payload signature.
fn sniff_outline_format(fng: &[u8]) -> String {
    let head = &fng[..fng.len().min(512)];
    if crate::util::find_bytes(head, b"%!PS-AdobeFont").is_some() || crate::util::find_bytes(head, b".PFB").is_some() {
        return "Adobe Type 1 (PFB) outline".into();
    }
    if fng.starts_with(b"OTTO") {
        return "CFF (OpenType) outline".into();
    }
    if is_cff(fng) {
        return "CFF / CID-keyed outline".into();
    }
    String::new()
}

/// Collapse per-orientation sf::FNI records to one entry per character.
fn dedup_orientations(raw: Vec<CharMetric>) -> (Vec<CharMetric>, usize) {
    let total = raw.len();
    let mut seen: IndexMap<String, CharMetric> = IndexMap::new();
    for cm in raw {
        seen.entry(cm.gcgid.clone()).or_insert(cm);
    }
    let chars: Vec<CharMetric> = seen.into_values().collect();
    let orientations = if chars.is_empty() { 1 } else { total / chars.len() };
    (chars, orientations.max(1))
}

/// Decode each character's outline from the embedded sf::FNG font program.
fn decode_outlines(fng: &[u8], chars: &[CharMetric]) -> (i64, IndexMap<String, Glyph>) {
    enum Program {
        Cff(CffFont),
        T1(Type1Font),
    }
    let program = if is_cff(fng) {
        match CffFont::new(fng) {
            Some(f) => Program::Cff(f),
            None => {
                log::warn!("Outline program decode failed");
                return (0, IndexMap::new());
            }
        }
    } else {
        Program::T1(Type1Font::new(fng))
    };
    let (empty, units_per_em) = match &program {
        Program::Cff(f) => (f.glyph_names().is_empty(), f.units_per_em),
        Program::T1(f) => (f.glyph_names().is_empty(), f.units_per_em),
    };
    if empty {
        return (0, IndexMap::new());
    }
    let mut outlines = IndexMap::new();
    for cm in chars {
        if cm.name.is_empty() {
            continue;
        }
        let glyph = match &program {
            Program::Cff(f) if f.has_glyph(&cm.name) => f.glyph(&cm.name),
            Program::T1(f) if f.has_glyph(&cm.name) => f.glyph(&cm.name),
            _ => None,
        };
        if let Some(g) = glyph {
            outlines.insert(cm.gcgid.clone(), g);
        }
    }
    (units_per_em, outlines)
}

/// A readable one-line summary of a FOCA font metric field, or "".
pub fn describe_foca_field(field: &StructuredField) -> String {
    let (sid, d) = (field.sf_id, &field.data[..]);
    match sid {
        sf::FND if d.len() >= 36 => {
            let name = decode_name(&d[..32]);
            let name = if name.is_empty() { "?".to_string() } else { name };
            format!(
                "FaceName={name} WeightClass={} WidthClass={} MaxVertSize={}pt",
                d[32],
                d[33],
                fmt_g(u16_at(d, 34) as f64 / 10.0)
            )
        }
        sf::FNC if d.len() >= 20 => {
            let tech = match d[1] {
                PATTECH_RASTER => "laser-matrix raster".to_string(),
                PATTECH_TYPE1 => "Type 1 outline".to_string(),
                PATTECH_CID => "CID/Type 1 outline".to_string(),
                t => format!("0x{t:02X}"),
            };
            format!(
                "MaxW={} MaxH={} PatternsSize={} FontTech={tech}",
                u16_at(d, 10) as u32 + 1,
                u16_at(d, 12) as u32 + 1,
                u16_at(d, 18)
            )
        }
        sf::FNO if d.len() >= 10 => {
            format!("CharRotation={} SpaceCharInc={}", u16_at(d, 0), u16_at(d, 8))
        }
        sf::FNP if d.len() >= 10 => {
            format!("MaxAscender={} MaxDescender={}", u16_at(d, 6), u16_at(d, 8))
        }
        sf::CPD if d.len() >= 38 => {
            let name = decode_name(&d[..32]);
            let name = if name.is_empty() { "?".to_string() } else { name };
            let cpgid = if d.len() >= 42 { u16_at(d, 40) } else { 0 };
            format!(
                "CodePage={} NumCodePoints={} GCGIDLen={} CPGID={cpgid}",
                py_repr(&name),
                u16_at(d, 36),
                u16_at(d, 32)
            )
        }
        sf::CPC if d.len() >= 9 => format!("DefaultChar={} SpaceCharVal={}", decode_name(&d[..8]), d[8]),
        sf::CPI => {
            let cp = parse_code_page(d);
            if cp.is_empty() {
                return String::new();
            }
            let sample: Vec<String> = cp.iter().take(3).map(|(k, v)| format!("0x{k:02X}→{v}")).collect();
            let more = if cp.len() > 3 { format!(", +{} more", cp.len() - 3) } else { String::new() };
            format!("{} code points: {}{more}", cp.len(), sample.join(", "))
        }
        _ => String::new(),
    }
}

/// Each code point's GCGID from a Code Page Index (10-byte records).
pub fn parse_code_page(cpi: &[u8]) -> IndexMap<u8, String> {
    let mut out = IndexMap::new();
    let mut i = 0;
    while i + 10 <= cpi.len() {
        let gcgid = decode_name(&cpi[i..i + 8]);
        if gcgid.chars().next().is_some_and(char::is_alphabetic) {
            out.insert(cpi[i + 9], gcgid);
        }
        i += 10;
    }
    out
}

/// Raster pattern resolution (pels/inch) from a Font Control field.
fn fnc_resolution(fnc: &[u8]) -> i64 {
    for off in [24, 6] {
        if fnc.len() >= off + 2 {
            let v = u16_at(fnc, off);
            if v != 0 {
                return v as i64 / 10;
            }
        }
    }
    0
}

/// Each coded-font name's (char-set, code-page) names, from sf::BCF...sf::ECF.
pub fn parse_coded_fonts(fields: &[StructuredField]) -> IndexMap<String, (String, String)> {
    let mut out = IndexMap::new();
    let mut name = String::new();
    let mut in_cf = false;
    for f in fields {
        if f.sf_id == sf::BCF {
            in_cf = true;
            name = decode_name(crate::util::slice(&f.data, 0, 8));
        } else if f.sf_id == sf::ECF {
            in_cf = false;
            name.clear();
        } else if in_cf && f.sf_id == sf::CFI && !name.is_empty() && !out.contains_key(&name) && f.data.len() >= 16 {
            let cs = decode_name(&f.data[..8]);
            let cp = decode_name(&f.data[8..16]);
            if !cs.is_empty() {
                out.insert(name.clone(), (cs, cp));
            }
        }
    }
    out
}

/// Every sf::BFN...sf::EFN font character set in a parsed file.
pub fn parse_fonts(fields: &[StructuredField]) -> Vec<Font> {
    let mut fonts = Vec::new();
    let mut name = String::new();
    let (mut fnc, mut fnd): (&[u8], &[u8]) = (&[], &[]);
    let (mut fni, mut fnm, mut fng, mut fnn) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut in_font = false;

    for f in fields {
        if f.sf_id == sf::BFN {
            in_font = true;
            name = decode_name(crate::util::slice(&f.data, 0, 8));
            fnc = &[];
            fnd = &[];
            fni.clear();
            fnm.clear();
            fng.clear();
            fnn.clear();
            continue;
        }
        if !in_font {
            continue;
        }
        match f.sf_id {
            sf::FNC => fnc = &f.data,
            sf::FND => fnd = &f.data,
            sf::FNI => fni.extend_from_slice(&f.data),
            sf::FNM => fnm.extend_from_slice(&f.data),
            sf::FNG => fng.extend_from_slice(&f.data),
            sf::FNN => fnn.extend_from_slice(&f.data),
            sf::EFN => {
                in_font = false;
                let tech = fnc.get(1).copied().unwrap_or(0);
                let typeface = if fnd.is_empty() { String::new() } else { decode_name(&fnd[..fnd.len().min(32)]) };
                let typeface = py_strip(typeface.split('@').next().unwrap_or("")).to_string();
                let mut font =
                    Font { name: name.clone(), typeface, pattern_tech: tech, orientations: 1, ..Default::default() };
                if tech == PATTECH_RASTER && !fnm.is_empty() && !fng.is_empty() {
                    font.glyphs = decode_raster_glyphs(fnc, &fni, &fnm, &fng);
                } else if (tech == PATTECH_TYPE1 || tech == PATTECH_CID) && !fni.is_empty() {
                    let (mut chars, orientations) = dedup_orientations(fni_metrics(fnc, &fni));
                    let names = if fnn.is_empty() { IndexMap::new() } else { fnn_glyph_names(&fnn) };
                    for cm in &mut chars {
                        cm.name = names.get(&cm.gcgid).cloned().unwrap_or_default();
                    }
                    font.outline_format = sniff_outline_format(&fng);
                    let (upem, outlines) = decode_outlines(&fng, &chars);
                    font.chars = chars;
                    font.orientations = orientations;
                    font.units_per_em = upem;
                    font.outline_glyphs = outlines;
                }
                font.resolution = fnc_resolution(fnc);
                font.point_size = if fnd.len() >= 36 { u16_at(fnd, 34) as f64 / 10.0 } else { 0.0 };
                font.weight_class = fnd.get(32).copied().unwrap_or(0);
                font.relative_metrics = fnc.len() >= 5 && fnc[4] == 0x02;
                fonts.push(font);
            }
            _ => {}
        }
    }
    fonts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_stops_at_unprintable() {
        // "AB" then NUL then "C"
        assert_eq!(decode_name(&[0xC1, 0xC2, 0x00, 0xC3]), "AB");
        // trailing spaces are stripped
        assert_eq!(decode_name(&[0xC1, 0x40, 0x40]), "A");
    }

    #[test]
    fn code_page_index() {
        let mut cpi = Vec::new();
        cpi.extend([0xD3, 0xC1, 0xF0, 0xF1, 0xF0, 0xF0, 0xF0, 0xF0, 0x00, 0x81]); // LA010000 @0x81
        cpi.extend([0x40; 8]);
        cpi.extend([0x00, 0x82]); // blank GCGID: skipped
        let cp = parse_code_page(&cpi);
        assert_eq!(cp.len(), 1);
        assert_eq!(cp[&0x81], "LA010000");
    }

    #[test]
    fn resolution_order() {
        let mut fnc = vec![0u8; 26];
        fnc[6..8].copy_from_slice(&2400u16.to_be_bytes());
        assert_eq!(fnc_resolution(&fnc), 240);
        fnc[24..26].copy_from_slice(&3000u16.to_be_bytes());
        assert_eq!(fnc_resolution(&fnc), 300);
    }
}
