//! PTOCA control sequences, TRN text decoding and the text interpreter.

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::codepages::Codec;
use crate::foca::RasterGlyph;
use crate::gcgid::{CodePageMap, decode_via_code_page};
use crate::goca::GocaGraphic;
use crate::model::{
    DEFAULT_COLOR, FidelityNote, FontInfo, ImageRef, MAX_RUNS_PER_PAGE, NoteCategory, Page, Rotation, Rule, RuleAxis,
    TextRun, VectorGraphic, oca_named_color,
};
use crate::outline::{Glyph, glyph_to_path_d};
use crate::util::{has_non_space, py_round, s16_at, trunc, u16_at};

/// Escape introducing a control sequence chain.
pub const ESCAPE: [u8; 2] = [0x2B, 0xD3];

/// "Codec used" sentinel: the run was decoded through the code page the
/// file itself embeds (no fallback note warranted).
pub const EMBEDDED_CP: &str = "the file's own code page";

const CHAR_ADVANCE_RATIO: f64 = 0.55;

/// Control sequence function names, keyed by the unchained (even) type.
pub fn cs_name(cs_type: u8) -> String {
    match cs_type {
        0x74 => "STC (Set Text Color)".into(),
        0x80 => "SEC (Set Extended Text Color)".into(),
        0xC0 => "SIM (Set Inline Margin)".into(),
        0xC2 => "SIA (Set Intercharacter Adjustment)".into(),
        0xC4 => "SVI (Set Variable Space Increment)".into(),
        0xC6 => "AMI (Absolute Move Inline)".into(),
        0xC8 => "RMI (Relative Move Inline)".into(),
        0xD0 => "SBI (Set Baseline Increment)".into(),
        0xD2 => "AMB (Absolute Move Baseline)".into(),
        0xD4 => "RMB (Relative Move Baseline)".into(),
        0xD8 => "BLN (Begin Line)".into(),
        0xDA => "TRN (Transparent Data)".into(),
        0xE4 => "DIR (Draw I-axis Rule)".into(),
        0xE6 => "DBR (Draw B-axis Rule)".into(),
        0xF0 => "SCFL (Set Coded Font Local)".into(),
        0xF2 => "BSU (Begin Suppression)".into(),
        0xF4 => "ESU (End Suppression)".into(),
        0xF6 => "STO (Set Text Orientation)".into(),
        0xF8 => "NOP (No Operation)".into(),
        t => format!("Unknown (0x{t:02X})"),
    }
}

/// STC color: the OCA named colors; anything else (X'FF07' default
/// included) draws in the default color.
fn stc_color(v: u16) -> &'static str {
    oca_named_color(v).unwrap_or(DEFAULT_COLOR)
}

/// One decoded PTOCA control sequence.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlSequence<'a> {
    /// Unchained (even) function type.
    pub cs_type: u8,
    pub params: &'a [u8],
}

/// PTOCA control sequences of PTX field data. Malformed tails are logged
/// and dropped rather than failing.
pub fn parse_control_sequences(data: &[u8]) -> Vec<ControlSequence<'_>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut chained = false;
    while pos < data.len() {
        if !chained {
            match crate::util::find_bytes_from(data, &ESCAPE, pos) {
                Some(esc) => pos = esc + 2,
                None => break,
            }
        }
        if pos + 2 > data.len() {
            log::warn!("truncated control sequence at offset {pos}");
            break;
        }
        let (length, cs_type) = (data[pos] as usize, data[pos + 1]);
        if length < 2 || pos + length > data.len() {
            log::warn!("bad control sequence length {length} at offset {pos}");
            break;
        }
        out.push(ControlSequence { cs_type: cs_type & 0xFE, params: &data[pos + 2..pos + length] });
        chained = cs_type & 0x01 != 0;
        pos += length;
    }
    out
}

/// True when a byte run reads as ASCII/Latin-1 rather than EBCDIC. Any byte
/// in 0x80–0x9F (EBCDIC a–r, Latin-1 C1 controls) vetoes it.
pub fn looks_like_ascii(params: &[u8]) -> bool {
    let (mut ascii_alnum, mut ebcdic_alnum) = (0, 0);
    for &b in params {
        if (0x80..=0x9F).contains(&b) {
            return false;
        }
        if b.is_ascii_alphanumeric() {
            ascii_alnum += 1;
        } else if matches!(b, 0xA2..=0xA9 | 0xC1..=0xC9 | 0xD1..=0xD9 | 0xE2..=0xE9 | 0xF0..=0xF9) {
            ebcdic_alnum += 1;
        }
    }
    ascii_alnum > 0 && ascii_alnum > ebcdic_alnum
}

/// Drop C0/C1 control characters (keep tab/newline/CR).
pub fn strip_controls(text: &str) -> String {
    let keep = |c: char| matches!(c, '\t' | '\n' | '\r') || (c >= ' ' && !('\x7f'..='\u{9f}').contains(&c));
    text.chars().filter(|&c| keep(c)).collect()
}

/// How a TRN run was decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextDecoding {
    /// UTF-16BE (TrueType flows).
    Utf16Be,
    /// The code page embedded in the file (CPI).
    Embedded,
    /// A codec, by its name.
    Codec(&'static str),
}

impl TextDecoding {
    pub fn as_str(self) -> &'static str {
        match self {
            TextDecoding::Utf16Be => "utf-16-be",
            TextDecoding::Embedded => EMBEDDED_CP,
            TextDecoding::Codec(name) => name,
        }
    }
}

/// A decoded TRN run.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedText {
    pub text: String,
    /// Code points dropped by [`strip_controls`].
    pub n_stripped: usize,
    /// How the bytes were decoded.
    pub used: TextDecoding,
    /// EBCDIC substitute characters (X'3F') seen.
    pub n_substitute: usize,
}

/// Decode TRN text: UTF-16BE for TrueType flows, else the embedded code
/// page, else (when `autodetect`) Latin-1 for ASCII-looking runs, else the
/// given code page (falling back to cp500).
pub fn decode_trn_counted(
    params: &[u8],
    codepage: Codec,
    autodetect: bool,
    cp_map: Option<&CodePageMap>,
) -> DecodedText {
    if let Some(text) = crate::codepages::utf16be_if_likely(params) {
        return DecodedText { text, n_stripped: 0, used: TextDecoding::Utf16Be, n_substitute: 0 };
    }
    let mut used = TextDecoding::Codec(codepage.name());
    let exact = cp_map.filter(|m| !m.is_empty()).and_then(|m| decode_via_code_page(params, m));
    let text = if let Some(t) = exact {
        used = TextDecoding::Embedded;
        t
    } else if autodetect && looks_like_ascii(params) {
        used = TextDecoding::Codec("latin-1");
        Codec::latin1().decode_replace(params)
    } else {
        match codepage.decode_strict(params) {
            Some(t) => t,
            None => {
                used = TextDecoding::Codec("cp500");
                Codec::cp500().decode_replace(params)
            }
        }
    };
    // X'3F' (U+001A, SUBSTITUTE): a lone one is FOP's list bullet; a run of
    // them means the producer dropped glyphs it had no AFP font for.
    let n_substitute = text.chars().filter(|&c| c == '\x1a').count();
    let text = text.replace('\x1a', "•");
    let stripped = strip_controls(&text);
    let n_stripped = text.chars().count() - stripped.chars().count();
    DecodedText { text: stripped, n_stripped, used, n_substitute }
}

/// Decode TRN text bytes (text only).
pub fn decode_trn(params: &[u8], codepage: Codec, autodetect: bool) -> String {
    decode_trn_counted(params, codepage, autodetect, None).text
}

fn sec_color(p: &[u8]) -> Option<String> {
    if p.len() >= 13 && p[1] == 0x01 && p[6..9] == [8, 8, 8] {
        Some(format!("#{:02x}{:02x}{:02x}", p[10], p[11], p[12]))
    } else {
        None
    }
}

/// A coded font whose code page and glyphs are both embedded, so its text
/// can be drawn in the file's own font.
#[derive(Debug, Clone)]
pub struct EmbeddedFont {
    pub cp_map: Arc<CodePageMap>,
    /// Raster glyphs by GCGID (empty for outline fonts).
    pub glyphs: Arc<HashMap<String, RasterGlyph>>,
    /// Tallest glyph box (pels): the fallback scale base.
    pub ref_height: i64,
    pub outline_glyphs: Option<Arc<IndexMap<String, Glyph>>>,
    pub units_per_em: i64,
    /// Codec for the hidden text-extraction layer.
    pub codec: Option<Codec>,
    pub resolution: i64,
    pub point_size: f64,
    pub relative_metrics: bool,
}

/// Minimum fraction of a run's bytes that must resolve to embedded glyphs.
const EMBED_COVERAGE_MIN: f64 = 0.7;
/// Raster glyphs are only drawn for display-size fonts by default.
const EMBED_MIN_POINT_SIZE: f64 = 20.0;

/// Font tables shared by every page's interpreter (filled by MDR/MCF fields
/// as they are met).
#[derive(Debug, Clone)]
pub struct SharedFonts {
    pub codepage: Codec,
    pub embed_small_fonts: bool,
    pub fonts: HashMap<u8, FontInfo>,
    /// Per-coded-font code pages from MCF labels.
    pub font_codepages: HashMap<u8, Codec>,
    pub embedded_text_fonts: HashMap<u8, EmbeddedFont>,
    /// Byte → GCGID maps of the code pages the file embeds, per local id.
    pub font_cp_maps: HashMap<u8, Arc<CodePageMap>>,
}

impl SharedFonts {
    pub fn new(codepage: Codec, embed_small_fonts: bool) -> SharedFonts {
        SharedFonts {
            codepage,
            embed_small_fonts,
            fonts: HashMap::new(),
            font_codepages: HashMap::new(),
            embedded_text_fonts: HashMap::new(),
            font_cp_maps: HashMap::new(),
        }
    }
}

/// Mutable PTOCA interpreter state, carried across PTX fields of a page.
#[derive(Debug, Clone, Default)]
pub struct TextState {
    pub i: i64,
    pub b: i64,
    pub inline_margin: i64,
    pub baseline_increment: i64,
    pub space_increment: Option<i64>,
    pub color: String,
    pub font_id: Option<u8>,
    pub field_offset: Option<usize>,
    /// Implicit pages flow text and wrap at this inline position.
    pub wrap_width: Option<i64>,
    /// STO inline direction, clockwise degrees.
    pub orientation: i64,
}

impl TextState {
    pub fn new() -> TextState {
        TextState { color: DEFAULT_COLOR.into(), ..Default::default() }
    }

    pub fn apply(&mut self, cs: &ControlSequence, page: &mut Page, sh: &SharedFonts) {
        let (t, p) = (cs.cs_type, cs.params);
        match t {
            0xC6 if p.len() >= 2 => self.i = s16_at(p, 0) as i64,  // AMI
            0xC8 if p.len() >= 2 => self.i += s16_at(p, 0) as i64, // RMI
            0xD2 if p.len() >= 2 => self.b = s16_at(p, 0) as i64,  // AMB
            0xD4 if p.len() >= 2 => self.b += s16_at(p, 0) as i64, // RMB
            0xC0 if p.len() >= 2 => self.inline_margin = u16_at(p, 0) as i64, // SIM
            0xC4 if p.len() >= 2 => self.space_increment = Some(u16_at(p, 0) as i64), // SVI
            0xD0 if p.len() >= 2 => self.baseline_increment = s16_at(p, 0) as i64, // SBI
            0xD8 => {
                // BLN
                self.b += self.baseline_increment;
                self.i = self.inline_margin;
            }
            0xF0 if !p.is_empty() => self.font_id = Some(p[0]), // SCFL
            0x74 if p.len() >= 2 => self.color = stc_color(u16_at(p, 0)).into(), // STC
            0x80 => {
                if let Some(c) = sec_color(p) {
                    self.color = c;
                }
            }
            0xF6 if p.len() >= 2 => {
                // STO: INLORENT in 1/128 degree, clockwise from +X.
                self.orientation = (u16_at(p, 0) as i64 / 128) % 360;
                self.i = 0;
                self.b = 0;
            }
            0xDA => self.trn(p, page, sh),
            0xE4 if p.len() >= 2 => self.add_rule(page, p, RuleAxis::Inline),
            0xE6 if p.len() >= 2 => self.add_rule(page, p, RuleAxis::Baseline),
            _ => {}
        }
    }

    fn font_info<'a>(&self, sh: &'a SharedFonts) -> Option<&'a FontInfo> {
        self.font_id.and_then(|id| sh.fonts.get(&id))
    }

    fn trn(&mut self, p: &[u8], page: &mut Page, sh: &SharedFonts) {
        let default_info = FontInfo::default();
        let info = self.font_info(sh).unwrap_or(&default_info);
        let upi = page.units_per_inch;
        // MDR L-unit size; else the coded-font point size; else ~12pt.
        let size = match info.size {
            Some(s) if s != 0 => s,
            _ => {
                let from_pt = match info.size_pt {
                    Some(pt) if pt != 0.0 => py_round(pt * upi as f64 / 72.0),
                    _ => 0,
                };
                if from_pt != 0 { from_pt } else { (upi / 6).max(8) }
            }
        };
        let emb = self.font_id.and_then(|id| sh.embedded_text_fonts.get(&id));
        if let Some(emb) = emb {
            let drawn = if emb.outline_glyphs.as_ref().is_some_and(|o| !o.is_empty()) {
                self.emit_embedded_outlines(page, p, emb, size)
            } else {
                self.emit_embedded_glyphs(page, p, emb, size, false, sh)
            };
            if drawn {
                return;
            }
        }
        let declared = self.font_id.is_some_and(|id| sh.font_codepages.contains_key(&id));
        let cp = self.font_id.and_then(|id| sh.font_codepages.get(&id)).copied().unwrap_or(sh.codepage);
        let cp_map = self.font_id.and_then(|id| sh.font_cp_maps.get(&id)).map(|m| m.as_ref());
        let dec = decode_trn_counted(p, cp, !declared, cp_map);
        let text = dec.text;
        // A run of only invisible characters may be a symbol whose shape
        // lives in the embedded glyph (e.g. a euro at X'A0').
        if let Some(emb) = emb
            && !emb.glyphs.is_empty()
            && !has_non_space(&text)
            && text.chars().any(|c| c != ' ')
            && self.emit_embedded_glyphs(page, p, emb, size, true, sh)
        {
            return;
        }
        let nchars = text.chars().count() as i64;
        if let Some(wrap) = self.wrap_width
            && self.i > 0
            && self.i as f64 + nchars as f64 * size as f64 * CHAR_ADVANCE_RATIO > wrap as f64
        {
            self.i = 240;
            self.b += trunc(size as f64 * 1.4);
        }
        if has_non_space(&text) {
            if page.texts.len() < MAX_RUNS_PER_PAGE {
                let (tx, ty) =
                    if self.orientation == 90 || self.orientation == 270 { (self.b, self.i) } else { (self.i, self.b) };
                let mut notes = info.notes.clone();
                if dec.n_stripped > 0 {
                    notes.push(FidelityNote::new(
                        NoteCategory::Glyph,
                        format!(
                            "{} character(s) omitted — code point(s) {} can't map (usually a producer-specific symbol).",
                            dec.n_stripped,
                            dec.used.as_str()
                        ),
                    ));
                }
                let nonspace = text.chars().filter(|&c| c != ' ').count();
                if dec.n_substitute >= 3 && nonspace > 0 && dec.n_substitute as f64 / nonspace as f64 > 0.5 {
                    notes.push(FidelityNote::new(
                        NoteCategory::Glyph,
                        format!(
                            "{} glyph(s) the producer couldn't encode in the AFP code page (wrote substitute X'3F') — the original glyphs (e.g. symbol/dingbat font) are absent from this AFP, not just unrendered here.",
                            dec.n_substitute
                        ),
                    ));
                }
                if matches!(dec.used, TextDecoding::Codec(_)) && !declared {
                    notes.push(FidelityNote::new(
                        NoteCategory::Codepage,
                        format!(
                            "No code page declared for this font — text decoded with the fallback {}.",
                            dec.used.as_str()
                        ),
                    ));
                }
                let sw = if text.contains(' ') { self.space_increment } else { None };
                page.texts.push(TextRun {
                    x: tx,
                    y: ty,
                    text: text.clone(),
                    color: self.color.clone(),
                    font_id: self.font_id,
                    font_size: size,
                    font_family: info.family.clone(),
                    font_weight: info.weight,
                    orientation: self.orientation,
                    src: self.field_offset,
                    fit: true,
                    space_width: sw,
                    notes,
                });
            } else {
                page.truncated = true;
            }
        }
        // Advance the pen: glyphs by the flat estimate, spaces by SVI.
        let n_space = text.chars().filter(|&c| c == ' ').count() as f64;
        let glyph_adv = (nchars as f64 - n_space) * size as f64 * CHAR_ADVANCE_RATIO;
        let space_adv = n_space
            * match self.space_increment {
                Some(s) => s as f64,
                None => size as f64 * CHAR_ADVANCE_RATIO,
            };
        self.i += trunc(glyph_adv + space_adv);
    }

    fn add_rule(&mut self, page: &mut Page, p: &[u8], axis: RuleAxis) {
        if page.rules.len() >= MAX_RUNS_PER_PAGE {
            page.truncated = true;
            return;
        }
        let length = s16_at(p, 0) as i64;
        let mut thickness = if p.len() >= 4 { s16_at(p, 2) as i64 } else { 20 };
        // Keep hairlines visible: a ~0.5pt floor at the page resolution.
        let min_thick = py_round(page.units_per_inch as f64 / 144.0).max(1);
        if thickness != 0 && thickness.abs() < min_thick {
            thickness = if thickness >= 0 { min_thick } else { -min_thick };
        }
        page.rules.push(Rule {
            x: self.i,
            y: self.b,
            length,
            thickness,
            axis,
            color: self.color.clone(),
            src: self.field_offset,
        });
    }

    fn embed_covers(data: &[u8], emb: &EmbeddedFont, has: impl Fn(&str) -> bool) -> bool {
        if data.is_empty() {
            return true;
        }
        let mapped = data.iter().filter(|b| emb.cp_map.get(b).is_some_and(|g| has(g))).count();
        mapped as f64 / data.len() as f64 >= EMBED_COVERAGE_MIN
    }

    /// Keep a run's decoded text for export when it is drawn as glyphs.
    fn record_embedded_text(&self, page: &mut Page, data: &[u8], emb: &EmbeddedFont) {
        let text = match decode_via_code_page(data, &emb.cp_map) {
            Some(t) => t,
            None => match emb.codec {
                Some(c) => c.decode_replace(data),
                None => return,
            },
        };
        if has_non_space(&text) {
            let mut run = TextRun::new(self.i, self.b, text);
            run.font_id = self.font_id;
            run.src = self.field_offset;
            page.push_hidden_text(run);
        }
    }

    /// Page-space run origin and rotation for the current STO orientation.
    fn oriented_origin(&self) -> (i64, i64, Option<Rotation>) {
        let o = self.orientation;
        let (ox, oy) = if o == 90 || o == 270 { (self.b, self.i) } else { (self.i, self.b) };
        (ox, oy, if o != 0 { Some((o, ox, oy)) } else { None })
    }

    /// Draw a TRN run with the file's own embedded raster glyphs.
    fn emit_embedded_glyphs(
        &mut self,
        page: &mut Page,
        data: &[u8],
        emb: &EmbeddedFont,
        size: i64,
        force: bool,
        sh: &SharedFonts,
    ) -> bool {
        let small = emb.point_size < EMBED_MIN_POINT_SIZE;
        if small && !sh.embed_small_fonts && !force {
            return false;
        }
        let crisp = !small;
        if !Self::embed_covers(data, emb, |g| emb.glyphs.contains_key(g)) {
            return false;
        }
        self.record_embedded_text(page, data, emb);
        let (ox, oy, rot) = self.oriented_origin();
        let upi = page.units_per_inch as f64;
        let pel = if emb.resolution != 0 {
            upi / emb.resolution as f64
        } else {
            size as f64 / (if emb.ref_height != 0 { emb.ref_height } else { 1 }) as f64
        };
        let em = if emb.point_size != 0.0 { py_round(emb.point_size / 72.0 * upi) } else { size };
        let default_adv = py_round(0.5 * em as f64).max(1);
        let recolor =
            if !self.color.is_empty() && self.color != DEFAULT_COLOR { Some(self.color.clone()) } else { None };
        let scaled = |v: i64| {
            if emb.relative_metrics { py_round(v as f64 / 1000.0 * em as f64) } else { py_round(v as f64 * pel) }
        };
        let mut lx = 0i64;
        for b in data {
            if page.images.len() >= MAX_RUNS_PER_PAGE {
                page.truncated = true;
                break;
            }
            let Some(glyph) = emb.cp_map.get(b).and_then(|g| emb.glyphs.get(g)) else {
                lx += default_adv;
                continue;
            };
            let adv = if glyph.char_increment == 0 { default_adv } else { scaled(glyph.char_increment) };
            if !glyph.png.is_empty() && glyph.height > 0 && glyph.width > 0 {
                let w = py_round(glyph.width as f64 * pel).max(1);
                let h = py_round(glyph.height as f64 * pel).max(1);
                let drop = scaled(glyph.baseline_offset);
                let bear = scaled(glyph.left_bearing);
                let mut img = ImageRef::new(ox + lx + bear, oy - h + drop, w, h, "image/png", glyph.png.clone());
                img.crisp = crisp;
                img.recolor = recolor.clone();
                img.rotate = rot;
                page.images.push(img);
            }
            lx += adv.max(1);
        }
        self.i += lx;
        true
    }

    /// Most SVG path data that outline text may add to one page.
    const MAX_PAGE_PATH_BYTES: usize = 64 * 1024 * 1024;

    /// Draw a TRN run with the file's own embedded outline glyphs, as one
    /// vector graphic in design-unit space scaled to the run size.
    fn emit_embedded_outlines(&mut self, page: &mut Page, data: &[u8], emb: &EmbeddedFont, size: i64) -> bool {
        let Some(outlines) = emb.outline_glyphs.as_ref() else { return false };
        if !Self::embed_covers(data, emb, |g| outlines.contains_key(g)) {
            return false;
        }
        self.record_embedded_text(page, data, emb);
        if page.graphics.len() >= MAX_RUNS_PER_PAGE {
            page.truncated = true;
            return true;
        }
        let em = if emb.units_per_em != 0 { emb.units_per_em } else { 1000 };
        let ascent = py_round(0.90 * em as f64);
        let box_h = py_round(1.20 * em as f64);
        let space = py_round(0.5 * em as f64) as f64;
        let scale = size as f64 / em as f64;
        // Vector data already on the page: outline text repeats each glyph's
        // path for every character, so cap the page's total.
        let budget = Self::MAX_PAGE_PATH_BYTES.saturating_sub(page.graphics.iter().map(|g| g.graphic.svg.len()).sum());
        let mut paths = String::new();
        let mut x_design = 0.0f64;
        for b in data {
            if paths.len() > budget {
                page.truncated = true;
                break;
            }
            let glyph = emb.cp_map.get(b).and_then(|g| outlines.get(g));
            match glyph {
                Some(g) if !g.segments.is_empty() => {
                    paths.push_str(&glyph_to_path_d(g, 1.0, x_design, ascent as f64));
                    x_design += if g.advance != 0.0 { g.advance } else { space };
                }
                _ => x_design += space,
            }
        }
        if !paths.is_empty() {
            let fill = if self.color.is_empty() { DEFAULT_COLOR.to_string() } else { self.color.clone() };
            let (ox, oy, rot) = self.oriented_origin();
            page.graphics.push(VectorGraphic {
                x: ox,
                y: oy - py_round(ascent as f64 * scale),
                width: py_round(x_design * scale).max(1),
                height: py_round(box_h as f64 * scale).max(1),
                graphic: Arc::new(GocaGraphic {
                    svg: format!("<path d=\"{paths}\" fill=\"{fill}\"/>"),
                    gps_w: py_round(x_design).max(1),
                    gps_h: box_h,
                }),
                rotate: rot,
            });
        }
        self.i += py_round(x_design * scale);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chained_sequences() {
        // AMI 100 (chained) then TRN "A" unchained after an escape.
        let data = [0x2B, 0xD3, 0x04, 0xC7, 0x00, 0x64, 0x03, 0xDA, 0xC1];
        let cs = parse_control_sequences(&data);
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].cs_type, 0xC6);
        assert_eq!(cs[1].cs_type, 0xDA);
        assert_eq!(cs[1].params, &[0xC1]);
        assert_eq!(cs_name(0xDA), "TRN (Transparent Data)");
    }

    #[test]
    fn trn_decoding() {
        let cp500 = Codec::cp500();
        assert_eq!(decode_trn(&[0xC8, 0x85, 0x93, 0x93, 0x96], cp500, false), "Hello");
        assert_eq!(decode_trn(&[0, 0x48, 0, 0x69], cp500, false), "Hi");
        assert_eq!(decode_trn(b"Hello", cp500, true), "Hello");
        // EBCDIC lowercase (0x81–0x99) vetoes the ASCII guess.
        assert_eq!(decode_trn(&[0x81, 0x41], cp500, true), "a\u{a0}");
        let d = decode_trn_counted(&[0x3F, 0xC1], cp500, false, None);
        assert_eq!(d.text, "•A");
        assert_eq!(d.n_substitute, 1);
        let d = decode_trn_counted(&[0x07, 0xC1], cp500, false, None);
        assert_eq!((d.text.as_str(), d.n_stripped), ("A", 1));
    }

    #[test]
    fn text_state_moves_and_text() {
        let sh = SharedFonts::new(Codec::cp500(), false);
        let mut page = Page::default();
        let mut st = TextState::new();
        st.field_offset = Some(7);
        let data = [
            0x2B, 0xD3, 0x04, 0xC7, 0x01, 0x00, // AMI 256
            0x04, 0xD3, 0x02, 0x00, // AMB 512
            0x07, 0xDA, 0xC8, 0x85, 0x93, 0x93, 0x96, // TRN Hello
        ];
        for cs in parse_control_sequences(&data) {
            st.apply(&cs, &mut page, &sh);
        }
        assert_eq!(page.texts.len(), 1);
        let run = &page.texts[0];
        assert_eq!((run.x, run.y, run.text.as_str(), run.font_size, run.src), (256, 512, "Hello", 240, Some(7)));
        assert_eq!(run.notes[0].cat, NoteCategory::Codepage);
        assert_eq!(st.i, 256 + 660);
    }

    #[test]
    fn rule_floor() {
        let sh = SharedFonts::new(Codec::cp500(), false);
        let mut page = Page::with_size(2000, 2000, 240);
        let mut st = TextState::new();
        st.apply(&ControlSequence { cs_type: 0xE4, params: &[0x00, 0x64, 0x00, 0x01] }, &mut page, &sh);
        assert_eq!(page.rules[0].thickness, 2);
        assert_eq!(page.rules[0].axis, RuleAxis::Inline);
    }
}
