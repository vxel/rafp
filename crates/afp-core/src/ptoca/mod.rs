//! PTOCA presentation-text decoding and page extraction.
//!
//! PTX structured fields carry chains of PTOCA control sequences. Together
//! with the image, graphics, bar code, overlay and font fields around them
//! they are interpreted into a [`Page`] model per BPG...EPG bracket.
//!
//! Coordinates are L-units along the inline (I) and baseline (B) axes,
//! scaled by the PGD units per base (typically 1440/inch).
//!
//! Reference: PTOCA Reference, AFPC-0009-04.

mod pages;
mod specimen;
mod text;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use indexmap::IndexMap;

pub use pages::extract_pages;
pub use text::{
    ControlSequence, DecodedText, EMBEDDED_CP, EmbeddedFont, SharedFonts, TextState, cs_name, decode_trn,
    decode_trn_counted, looks_like_ascii, parse_control_sequences, strip_controls,
};

use crate::bcoca::{barcode_png, parse_barcode};
use crate::codepages::{Codec, codec_for_codepage_name, ebcdic_strip};
use crate::foca::{Font, RasterGlyph, parse_code_page, parse_coded_fonts, parse_fonts};
use crate::gcgid::{CodePageMap, bridge_code_page};
use crate::goca::draw_goca;
use crate::imimage::{decode_im_image, parse_ioc_origin};
use crate::ioca::{cmyk_jpeg_bands, image_blob, lzw_cmyk_bands, parse_image_segment};
use crate::model::{
    DEFAULT_FONT_SIZE, FidelityNote, FontInfo, FontWeight, ImageRef, NoteCategory, Page, VectorGraphic,
};
use crate::parser::{StructuredField, sf};
use crate::triplets::{mcf_coded_fonts, mcf_font_resources, parse_groups, parse_mcf_codepages, parse_triplets};
use crate::util::{be_uint, floor_div, py_round, py_strip, s24_at, trunc, u16_at};

/// Pick a CSS (family, weight) for a FOCA typeface, or `None`.
pub fn substitute_font(typeface: &str) -> Option<(&'static str, FontWeight)> {
    let t = typeface.to_uppercase();
    let weight = if t.contains("BOLD") { FontWeight::Bold } else { FontWeight::Normal };
    if t.contains("COURIER") || t.contains("MONO") {
        Some(("Courier New, monospace", weight))
    } else if t.contains("TIMES") || t.contains("ROMAN") || t.contains("SERIF") {
        Some(("Times New Roman, serif", weight))
    } else if t.contains("VERDANA") {
        Some(("Verdana, sans-serif", weight))
    } else if t.contains("HELVETICA") || t.contains("ARIAL") || t.contains("SANS") {
        Some(("Arial, sans-serif", weight))
    } else {
        None
    }
}

/// IBM / FOP coded-font names encode the typeface in their 3rd character.
fn coded_font_typeface(c: char) -> &'static str {
    match c {
        'H' => "HELVETICA",
        '4' => "COURIER",
        'N' | 'T' | 'S' | '5' => "TIMES",
        _ => "",
    }
}

fn nth(s: &str, n: usize) -> Option<char> {
    s.chars().nth(n)
}

/// Substitute font inferred from an IBM/FOP coded-font name.
pub fn coded_font_substitute(name: &str) -> Option<(&'static str, FontWeight)> {
    if name.chars().count() >= 3 && name.starts_with("C0") {
        return substitute_font(coded_font_typeface(nth(name, 2)?));
    }
    None
}

/// Point size encoded in the 7th character of an IBM character-set name:
/// '0' is 10pt, otherwise 10 + the letter's position in the alphabet.
pub fn coded_font_point_size(name: &str) -> Option<f64> {
    if name.chars().count() < 7 || !name.starts_with("C0") {
        return None;
    }
    match nth(name, 6)? {
        '0' => Some(10.0),
        c @ 'A'..='Z' => Some(10.0 + (c as u32 - 'A' as u32 + 1) as f64),
        _ => None,
    }
}

fn family_label(family: &str) -> &str {
    match family {
        "Arial, sans-serif" => "Arial",
        "Times New Roman, serif" => "Times New Roman",
        "Courier New, monospace" => "Courier New",
        f => f,
    }
}

/// Python `str.title()`.
fn py_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_cased = false;
    for c in s.chars() {
        if c.is_alphabetic() {
            if prev_cased {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
            prev_cased = true;
        } else {
            out.push(c);
            prev_cased = false;
        }
    }
    out
}

/// Readable typeface for a character set: embedded name, else coded-font.
fn typeface_label(cs_name: &str, typefaces: &HashMap<String, String>) -> String {
    if let Some(tf) = typefaces.get(cs_name).filter(|t| !t.is_empty()) {
        return py_title(tf).replace('-', " ");
    }
    if cs_name.chars().count() >= 3 && cs_name.starts_with("C0") {
        return py_title(coded_font_typeface(nth(cs_name, 2).unwrap()));
    }
    String::new()
}

/// Font name/size per local id from an MDR (Map Data Resource).
pub fn parse_mdr_fonts(data: &[u8]) -> IndexMap<u8, FontInfo> {
    let mut fonts = IndexMap::new();
    for group in parse_groups(data) {
        let (mut name, mut size, mut local_id): (Option<String>, Option<i64>, Option<u8>) = (None, None, None);
        for (tid, t) in group {
            if tid == 0x02 && t.len() >= 3 {
                if t[0] == 0xDE {
                    name = Some(py_strip(&decode_trn(&t[2..], Codec::cp500(), false)).to_string());
                } else if t[0] == 0xBE {
                    local_id = t.last().copied();
                }
            } else if tid == 0x8B && t.len() >= 4 {
                size = Some(u16_at(t, 2) as i64);
            }
        }
        if let Some(lid) = local_id {
            let mut family = name.filter(|n| !n.is_empty()).unwrap_or_else(|| "Arial".into());
            let mut weight = FontWeight::Normal;
            for marker in [" Bold", " bold"] {
                if family.contains(marker) {
                    family = family.replace(marker, "");
                    weight = FontWeight::Bold;
                }
            }
            fonts.insert(lid, FontInfo { family, weight, size, size_pt: None, notes: Vec::new() });
        }
    }
    fonts
}

/// Set each text run's font size from observed spacing, for fonts whose
/// size was not declared.
pub fn estimate_font_sizes(page: &mut Page, known_fonts: &HashMap<u8, FontInfo>) {
    let sized: HashSet<u8> = known_fonts
        .iter()
        .filter(|(_, i)| i.size.is_some_and(|s| s != 0) || i.size_pt.is_some_and(|s| s != 0.0))
        .map(|(id, _)| *id)
        .collect();
    let is_sized = |fid: Option<u8>| fid.is_some_and(|f| sized.contains(&f));
    let upi = page.units_per_inch;
    let lo = (upi / 18).max(4);
    let hi = upi * 5 / 6;

    // Primary estimate: baseline pitch (~1.2× the point size).
    let mut baselines: IndexMap<Option<u8>, Vec<i64>> = IndexMap::new();
    for r in &page.texts {
        if !is_sized(r.font_id) {
            baselines.entry(r.font_id).or_default().push(r.y);
        }
    }
    let mut pitch_est: HashMap<Option<u8>, i64> = HashMap::new();
    for (fid, ys) in &baselines {
        let mut ordered = ys.clone();
        ordered.sort();
        ordered.dedup();
        let mut diffs: Vec<i64> = ordered.windows(2).map(|w| w[1] - w[0]).filter(|&d| d > 0 && d <= upi / 2).collect();
        if !diffs.is_empty() {
            diffs.sort();
            let pitch = diffs[diffs.len() / 2];
            pitch_est.insert(*fid, lo.max(hi.min(trunc(pitch as f64 / 1.2))));
        }
    }

    // Fallback for single-line fonts: plausible horizontal character widths.
    let min_char = upi as f64 / 30.0;
    let max_char = upi as f64 / 4.0;
    let mut samples: HashMap<Option<u8>, Vec<f64>> = HashMap::new();
    for w in page.texts.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if a.y == b.y && b.x > a.x && !a.text.is_empty() && !is_sized(a.font_id) && !pitch_est.contains_key(&a.font_id)
        {
            let per_char = (b.x - a.x) as f64 / (a.text.chars().count() + 1) as f64;
            if min_char <= per_char && per_char <= max_char {
                samples.entry(a.font_id).or_default().push(per_char);
            }
        }
    }
    for widths in samples.values_mut() {
        widths.sort_by(|a, b| a.partial_cmp(b).unwrap());
    }

    for run in &mut page.texts {
        if is_sized(run.font_id) {
            continue;
        }
        let mut inferred = false;
        if let Some(&p) = pitch_est.get(&run.font_id) {
            run.font_size = p;
            inferred = true;
        } else if let Some(widths) = samples.get(&run.font_id).filter(|w| !w.is_empty()) {
            let median = widths[widths.len() / 2];
            run.font_size = lo.max(hi.min(trunc(median / 0.52)));
            inferred = true;
        }
        let pt = py_round(run.font_size as f64 / upi as f64 * 72.0);
        let msg = if inferred {
            format!("Point size not declared — estimated ≈{pt}pt from the producer's line spacing.")
        } else {
            format!("Point size not declared and not inferable (single line) — using a default of ≈{pt}pt.")
        };
        if !run.notes.iter().any(|n| n.msg == msg) {
            run.notes.push(FidelityNote::new(NoteCategory::Font, msg));
        }
    }
}

fn annotate_image_notes(pages: &mut [Page]) {
    for page in pages {
        for img in &mut page.images {
            if img.bands.is_some() && !img.notes.iter().any(|n| n.cat == NoteCategory::Image) {
                img.notes.push(FidelityNote::new(
                    NoteCategory::Image,
                    "CMYK image composited from 4 ink planes via SVG filters + multiply blend — on-screen colours are an optical approximation of the printed inks.",
                ));
            }
        }
    }
}

/// The MIME type if the bytes look like a known raster format.
fn sniff_image(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if data.starts_with(b"\x89PNG") {
        Some("image/png")
    } else if data.starts_with(b"GIF8") {
        Some("image/gif")
    } else {
        None
    }
}

/// A decoded BIM...EIM (or IM) image object, ready to place or page.
#[derive(Debug, Clone)]
struct ImageObject {
    mime: String,
    blob: Arc<Vec<u8>>,
    /// Object-area units per inch.
    upi: i64,
    width: i64,
    height: i64,
    x: i64,
    y: i64,
    bands: Option<Arc<Vec<Vec<u8>>>>,
    crisp: bool,
    /// x/y are in `upi` units (IM image), not page units.
    scaled_pos: bool,
}

/// `(units_per_inch, width, height)` from OBD triplets.
pub fn parse_obd(data: &[u8]) -> (Option<i64>, i64, i64) {
    let (mut upi, mut width, mut height) = (None, 0, 0);
    for (tid, t) in parse_triplets(data) {
        if tid == 0x4B && t.len() >= 4 {
            let v = u16_at(t, 2) as i64 / 10;
            upi = if v != 0 { Some(v) } else { None };
        } else if tid == 0x4C && t.len() >= 7 {
            width = be_uint(t, 1, 3) as i64;
            height = be_uint(t, 4, 3) as i64;
        }
    }
    (upi, width, height)
}

/// The object area's (x, y) origin from an OBP field.
pub fn parse_obp(data: &[u8]) -> (i64, i64) {
    if data.len() < 8 {
        return (0, 0);
    }
    (s24_at(data, 2) as i64, s24_at(data, 5) as i64)
}

fn finish_image_object(ipd: &[u8], obd: Option<&[u8]>, obp: Option<&[u8]>) -> Option<ImageObject> {
    let segment = parse_image_segment(ipd)?;
    let (mime, payload, bands) = match image_blob(&segment) {
        Some((mime, payload)) => (mime.to_string(), payload, None),
        None => {
            let Some(bands) = cmyk_jpeg_bands(&segment).or_else(|| lzw_cmyk_bands(&segment)) else {
                log::info!(
                    "skipping IOCA image: compression 0x{:02X}, {} bits/IDE not decoded",
                    segment.compression,
                    segment.bits
                );
                return None;
            };
            ("image/png".to_string(), Vec::new(), Some(Arc::new(bands)))
        }
    };
    let (mut upi, mut width, mut height) = obd.map(parse_obd).unwrap_or((None, 0, 0));
    if width <= 0 || height <= 0 {
        // No usable object area: the pixel grid at the image's resolution.
        let r = segment.hres as i64 / 10;
        upi = if r != 0 { Some(r) } else { None };
        width = segment.width as i64;
        height = segment.height as i64;
    }
    if width <= 0 || height <= 0 {
        return None;
    }
    let (x, y) = obp.map(parse_obp).unwrap_or((0, 0));
    Some(ImageObject {
        mime,
        blob: Arc::new(payload),
        upi: upi.unwrap_or(1440),
        width,
        height,
        x,
        y,
        bands,
        crisp: false,
        scaled_pos: false,
    })
}

/// Rescale between unit systems (`value * to / from`, floor division).
pub fn scale(value: i64, from_upi: i64, to_upi: i64) -> i64 {
    if from_upi != 0 { floor_div(value * to_upi, from_upi) } else { value }
}

fn parse_iob(
    data: &[u8],
    resources: &HashMap<String, Arc<Vec<u8>>>,
    images: &HashMap<String, ImageObject>,
    page_upi: i64,
) -> Option<ImageRef> {
    if data.len() < 27 {
        return None;
    }
    let name = ebcdic_strip(&data[..8]);
    let (x, y) = (s24_at(data, 10) as i64, s24_at(data, 13) as i64);
    let (mut upi, mut width, mut height) = (None, 0i64, 0i64);
    for (tid, t) in parse_triplets(&data[27..]) {
        if tid == 0x4C && t.len() >= 7 {
            width = be_uint(t, 1, 3) as i64;
            height = be_uint(t, 4, 3) as i64;
        } else if tid == 0x4B && t.len() >= 4 {
            let v = u16_at(t, 2) as i64 / 10;
            upi = if v != 0 { Some(v) } else { None };
        }
    }
    let (mime, blob, bands, crisp) = if let Some(blob) = resources.get(&name) {
        (sniff_image(blob)?.to_string(), Arc::clone(blob), None, false)
    } else {
        let obj = images.get(&name)?;
        if width <= 0 || height <= 0 {
            upi = Some(obj.upi);
            width = obj.width;
            height = obj.height;
        }
        (obj.mime.clone(), obj.blob.clone(), obj.bands.clone(), obj.crisp)
    };
    if width <= 0 || height <= 0 {
        return None;
    }
    let from = upi.unwrap_or(page_upi);
    let mut img = ImageRef::new(x, y, scale(width, from, page_upi), scale(height, from, page_upi), &mime, blob);
    img.bands = bands;
    img.crisp = crisp;
    Some(img)
}

/// `(width, height, units_per_inch)` from PGD field data.
pub fn parse_pgd(data: &[u8]) -> (i64, i64, i64) {
    let units = u16_at(data, 2) as i64;
    let width = be_uint(data, 6, 3) as i64;
    let height = be_uint(data, 9, 3) as i64;
    (width, height, if units >= 10 { units / 10 } else { 1440 })
}

/// `(width, height, units_per_inch)` from PTD data: 2-byte extents in
/// format 1, 3-byte in format 2.
pub fn parse_ptd(data: &[u8], format1: bool) -> (i64, i64, i64) {
    let units = u16_at(data, 2) as i64;
    let n = if format1 { 2 } else { 3 };
    let width = be_uint(data, 6, n) as i64;
    let height = be_uint(data, 6 + n, n) as i64;
    (width, height, if units >= 10 { units / 10 } else { 1440 })
}

/// Each embedded code page (BCP...ECP) as code point → GCGID.
fn scan_code_pages(fields: &[StructuredField]) -> HashMap<String, CodePageMap> {
    let mut out = HashMap::new();
    let mut name: Option<String> = None;
    let mut cpi = Vec::new();
    for f in fields {
        match f.sf_id {
            sf::BCP => {
                name = Some(py_strip(&f.token_name().unwrap_or_default()).to_string());
                cpi.clear();
            }
            sf::CPI => cpi.extend_from_slice(&f.data),
            sf::ECP => {
                if let Some(n) = name.take().filter(|n| !n.is_empty())
                    && !cpi.is_empty()
                {
                    out.insert(n, parse_code_page(&cpi).into_iter().collect());
                }
                cpi.clear();
            }
            _ => {}
        }
    }
    out
}

/// Overlay local id → name from a Map Page Overlay (MPO) field.
pub fn parse_mpo(data: &[u8]) -> IndexMap<u8, String> {
    let mut out = IndexMap::new();
    for group in parse_groups(data) {
        let (mut local_id, mut name) = (None, None);
        for (tid, t) in group {
            if tid == 0x24 && !t.is_empty() {
                local_id = t.last().copied();
            } else if tid == 0x02 && t.len() >= 3 {
                name = Some(ebcdic_strip(&t[2..]));
            }
        }
        if let (Some(lid), Some(n)) = (local_id, name)
            && !n.is_empty()
        {
            out.insert(lid, n);
        }
    }
    out
}

/// Overlays captured from BMO...EMO, indexed by every name an IPO may cite.
#[derive(Default)]
struct Overlays {
    pages: Vec<Page>,
    by_name: IndexMap<String, usize>,
}

impl Overlays {
    fn resolve(&self, ipo: &[u8], mpo_map: &IndexMap<u8, String>) -> Option<&Page> {
        let name = ebcdic_strip(crate::util::slice(ipo, 0, 8));
        if !name.is_empty()
            && let Some(&i) = self.by_name.get(&name)
        {
            return Some(&self.pages[i]);
        }
        let nonblank: Vec<u8> =
            crate::util::slice(ipo, 0, 8).iter().copied().filter(|&b| b != 0x00 && b != 0x40).collect();
        if nonblank.len() == 1 {
            let mapped = mpo_map.get(&nonblank[0])?;
            return self.by_name.get(mapped).map(|&i| &self.pages[i]);
        }
        None
    }
}

/// Composite a page overlay onto a page (IPO field): content shifted by the
/// IPO offset and rescaled to the page resolution.
fn include_overlay(page: &mut Page, overlay: &Page, ipo: &[u8]) {
    let ox = if ipo.len() >= 11 { s24_at(ipo, 8) as i64 } else { 0 };
    let oy = if ipo.len() >= 14 { s24_at(ipo, 11) as i64 } else { 0 };
    let src = if overlay.units_per_inch != 0 { overlay.units_per_inch } else { page.units_per_inch };
    let dst = page.units_per_inch;
    let sz = |v: i64| scale(v, src, dst);
    page.truncated |= overlay.truncated;
    for t in &overlay.texts {
        let mut t = t.clone();
        t.x = ox + sz(t.x);
        t.y = oy + sz(t.y);
        t.font_size = sz(t.font_size);
        page.push_text(t);
    }
    for r in &overlay.rules {
        let mut r = r.clone();
        r.x = ox + sz(r.x);
        r.y = oy + sz(r.y);
        r.length = sz(r.length);
        r.thickness = sz(r.thickness);
        page.push_rule(r);
    }
    for im in &overlay.images {
        let mut im = im.clone();
        im.x = ox + sz(im.x);
        im.y = oy + sz(im.y);
        im.width = sz(im.width);
        im.height = sz(im.height);
        page.push_image(im);
    }
    for g in &overlay.graphics {
        let mut g = g.clone();
        g.x = ox + sz(g.x);
        g.y = oy + sz(g.y);
        g.width = sz(g.width);
        g.height = sz(g.height);
        page.push_graphic(g);
    }
}

/// Composite a page segment's image onto a page (IPS field).
fn include_segment(page: &mut Page, images: &HashMap<String, ImageObject>, ips: &[u8]) {
    let name = ebcdic_strip(crate::util::slice(ips, 0, 8));
    let Some(obj) = images.get(&name) else { return };
    let ox = if ips.len() >= 11 { s24_at(ips, 8) as i64 } else { 0 };
    let oy = if ips.len() >= 14 { s24_at(ips, 11) as i64 } else { 0 };
    let upi = page.units_per_inch;
    // IOCA positions (OBP) are page units; an IM image's IOC origin is not.
    let px = if obj.scaled_pos { scale(obj.x, obj.upi, upi) } else { obj.x };
    let py = if obj.scaled_pos { scale(obj.y, obj.upi, upi) } else { obj.y };
    let mut img = ImageRef::new(
        ox + px,
        oy + py,
        scale(obj.width, obj.upi, upi),
        scale(obj.height, obj.upi, upi),
        &obj.mime,
        obj.blob.clone(),
    );
    img.bands = obj.bands.clone();
    img.crisp = obj.crisp;
    page.push_image(img);
}

/// Split an implicit page's flowed content into page-height chunks.
fn paginate_implicit(mut page: Page) -> Vec<Page> {
    let max_x = page.texts.iter().map(|r| r.x).chain(page.rules.iter().map(|r| r.x)).max();
    if let Some(mx) = max_x {
        page.width = page.width.max(mx + 6 * DEFAULT_FONT_SIZE);
    }
    let usable = (page.height - 320).max(1);
    let mut chunks: BTreeMap<i64, Page> = BTreeMap::new();
    let blank = |p: &Page| Page::with_size(p.width, p.height, p.units_per_inch);
    for mut t in std::mem::take(&mut page.texts) {
        let idx = floor_div(t.y, usable);
        t.y -= idx * usable;
        chunks.entry(idx).or_insert_with(|| blank(&page)).texts.push(t);
    }
    for mut r in std::mem::take(&mut page.rules) {
        let idx = floor_div(r.y, usable);
        r.y -= idx * usable;
        chunks.entry(idx).or_insert_with(|| blank(&page)).rules.push(r);
    }
    let mut ordered: Vec<Page> = chunks.into_values().collect();
    if page.truncated
        && let Some(last) = ordered.last_mut()
    {
        last.truncated = true;
    }
    ordered
}

/// A coded-font name without its rotation selector (2nd character).
pub fn rotation_insensitive(name: &str) -> Option<String> {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() < 2 {
        return None;
    }
    let mut out = String::new();
    out.push(chars[0]);
    out.extend(&chars[2..]);
    Some(out)
}

#[cfg(test)]
mod tests;
