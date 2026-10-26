//! Inspector model: one readable row per structured field, plus file-level
//! notes (resource kind, missing resources, declared code pages) and the
//! rendered page model.

use indexmap::{IndexMap, IndexSet};
use serde::Serialize;

use crate::codepages::{Codec, codec_for_codepage_name, ebcdic, ebcdic_strip};
use crate::foca::{decode_name, describe_foca_field, parse_coded_fonts};
use crate::model::{Page, ResourceKind};
use crate::parser::{AfpParseError, StructuredField, TYPE_BEGIN, TYPE_END, parse_fields, sf};
use crate::ptoca::{decode_trn, extract_pages, parse_control_sequences, parse_mpo, parse_pgd, parse_ptd};
use crate::triplets::{
    MCF_FORMAT_1, MCF_FORMAT_2, RESOURCE_KINDS, TripletRow, describe_field, mcf_coded_fonts, mcf_font_resources,
    mcf_resource_names, parse_mcf_codepages, parse_triplets,
};
use crate::util::{be_uint, collapse_ws, has_non_space, hex_sep, is_printable, py_repr, s24_at, u16_at};

/// One inspector row.
#[derive(Debug, Clone, Serialize)]
pub struct FieldRow {
    pub offset: usize,
    pub sf_id: u32,
    pub name: String,
    pub token: String,
    /// Full structured-field record size (9 header bytes + data).
    pub size: usize,
    pub data_len: usize,
    pub depth: usize,
    /// First 16 data bytes as spaced hex.
    pub preview: String,
    /// Page (BPG...EPG bracket, or implicit page) the field belongs to.
    pub page: Option<usize>,
    pub triplets: Vec<TripletRow>,
    /// Decoded searchable text (PTX / NOP / TLE).
    pub search_text: String,
    /// Readable one-line summary ("" when only hex is available).
    pub data: String,
}

impl FieldRow {
    pub fn sf_id_hex(&self) -> String {
        format!("0x{:06X}", self.sf_id)
    }
}

/// A font resource the document references but does not embed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MissingResource {
    pub kind: ResourceKind,
    pub name: String,
    pub codec: Option<String>,
}

/// Everything the viewer shows for one file.
#[derive(Debug, Clone)]
pub struct Inspection {
    pub fields: Vec<StructuredField>,
    pub rows: Vec<FieldRow>,
    /// Field counts by name, most common first.
    pub summary: Vec<(String, usize)>,
    pub pages: Vec<Page>,
    pub filesize: usize,
    pub mcf_note: String,
    /// What a page-less file holds (`None` for a document with pages).
    pub resource_kind: Option<ResourceKind>,
    pub missing_resources: Vec<MissingResource>,
}

/// Parse and inspect AFP bytes. `codepage` decodes text in fonts the file
/// leaves unlabeled.
pub fn inspect(data: &[u8], codepage: Codec, embed_small_fonts: bool) -> Result<Inspection, AfpParseError> {
    let fields = parse_fields(data)?;
    let mut rows = field_rows(&fields);
    let mut counts: IndexMap<String, usize> = IndexMap::new();
    for r in &rows {
        *counts.entry(r.name.clone()).or_default() += 1;
    }
    let mut summary: Vec<(String, usize)> = counts.into_iter().collect();
    summary.sort_by(|a, b| b.1.cmp(&a.1)); // stable: ties keep first-seen order
    let pages = extract_pages(&fields, codepage, embed_small_fonts);
    let bracketed = rows.iter().filter(|r| r.sf_id == sf::BPG).count();
    if pages.len() > bracketed {
        // Loose PTX flowed onto implicit pages: link rows through `src`.
        let mut src_to_page: IndexMap<usize, usize> = IndexMap::new();
        for (idx, page) in pages.iter().enumerate() {
            for src in page.texts.iter().map(|t| t.src).chain(page.rules.iter().map(|r| r.src)).flatten() {
                src_to_page.entry(src).or_insert(idx);
            }
        }
        for row in &mut rows {
            if row.page.is_none() && row.sf_id == sf::PTX {
                row.page = src_to_page.get(&row.offset).copied();
            }
        }
    }
    Ok(Inspection {
        mcf_note: mcf_codepage_note(&fields),
        resource_kind: resource_kind(&fields),
        missing_resources: missing_resources(&fields),
        fields,
        rows,
        summary,
        pages,
        filesize: data.len(),
    })
}

/// Begin fields that mark a page-less stream as an AFP resource, most
/// specific first.
const RESOURCE_KIND_FIELDS: &[(u32, ResourceKind)] = &[
    (sf::BFN, ResourceKind::FontCharacterSet),
    (sf::BCF, ResourceKind::CodedFont),
    (sf::BCP, ResourceKind::CodePage),
    (sf::BMO, ResourceKind::PageOverlay),
    (sf::BPS, ResourceKind::PageSegment),
    (sf::BIM, ResourceKind::IocaImage),
    (sf::BGR, ResourceKind::GocaGraphics),
    (sf::BBC, ResourceKind::BcocaBarCode),
    (sf::BOC, ResourceKind::ObjectContainer),
    (sf::BRG, ResourceKind::ResourceGroup),
    (sf::BRS, ResourceKind::ObjectResource),
];

/// `None` when the stream has real pages (a BPG), else its resource kind.
pub fn resource_kind(fields: &[StructuredField]) -> Option<ResourceKind> {
    if fields.iter().any(|f| f.sf_id == sf::BPG) {
        return None;
    }
    RESOURCE_KIND_FIELDS.iter().find(|(sid, _)| fields.iter().any(|f| f.sf_id == *sid)).map(|(_, kind)| *kind)
}

/// The field that embeds a resource of an MCF-referenced kind.
fn embed_field(kind: ResourceKind) -> u32 {
    match kind {
        ResourceKind::CodePage => sf::BCP,
        ResourceKind::CharacterSet => sf::BFN,
        _ => sf::BCF,
    }
}

/// A coded-font name without its rotation selector (2nd character).
fn norm_cf(name: &str) -> String {
    crate::ptoca::rotation_insensitive(name).unwrap_or_else(|| name.to_string())
}

/// Font resources referenced via MCF but not embedded.
pub fn missing_resources(fields: &[StructuredField]) -> Vec<MissingResource> {
    let mut refs: IndexMap<ResourceKind, IndexSet<String>> =
        RESOURCE_KINDS.iter().map(|k| (*k, IndexSet::new())).collect();
    for f in fields {
        if f.sf_id == MCF_FORMAT_1 || f.sf_id == MCF_FORMAT_2 {
            for (kind, names) in mcf_resource_names(&f.data, f.sf_id == MCF_FORMAT_1) {
                refs[&kind].extend(names);
            }
        }
    }
    let embedded = |kind: ResourceKind| -> IndexSet<String> {
        let sid = embed_field(kind);
        fields.iter().filter(|f| f.sf_id == sid).filter_map(|f| f.token_name()).filter(|t| !t.is_empty()).collect()
    };
    let embedded_cf_norm: IndexSet<String> = embedded(ResourceKind::CodedFont).iter().map(|n| norm_cf(n)).collect();
    let mut missing = Vec::new();
    for kind in RESOURCE_KINDS {
        let have = embedded(kind);
        let mut names: Vec<&String> = refs[&kind].iter().filter(|n| !have.contains(*n)).collect();
        names.sort();
        for name in names {
            if kind == ResourceKind::CodedFont && embedded_cf_norm.contains(&norm_cf(name)) {
                continue; // present under a different rotation selector
            }
            let codec = if kind == ResourceKind::CodePage {
                codec_for_codepage_name(name).map(|c| c.name().to_string())
            } else {
                None
            };
            missing.push(MissingResource { kind, name: name.clone(), codec });
        }
    }
    missing
}

/// The code pages MCF fields label their fonts with, e.g. `T1V10500 → cp500`.
pub fn mcf_codepage_note(fields: &[StructuredField]) -> String {
    let mut labels: IndexSet<String> = IndexSet::new();
    for f in fields {
        if f.sf_id != MCF_FORMAT_1 && f.sf_id != MCF_FORMAT_2 {
            continue;
        }
        for cp in parse_mcf_codepages(&f.data, f.sf_id == MCF_FORMAT_1).values() {
            labels.insert(match cp.codec {
                Some(c) => format!("{} → {}", cp.name, c.name()),
                None => cp.name.clone(),
            });
        }
    }
    labels.into_iter().collect::<Vec<_>>().join(", ")
}

/// Decode bytes as whichever of EBCDIC / Latin-1 is more printable.
pub fn readable(data: &[u8]) -> String {
    let e = ebcdic(data);
    let a = Codec::latin1().decode_replace(data);
    let printable = |s: &str| s.chars().filter(|&c| is_printable(c)).count();
    let best = if printable(&e) >= printable(&a) { e } else { a };
    collapse_ws(&best)
}

/// The searchable text a field carries: PTX presentation text, NOP
/// payloads, TLE attribute name and value.
pub fn field_search_text(field: &StructuredField) -> String {
    match field.sf_id {
        sf::PTX => parse_control_sequences(&field.data)
            .iter()
            .filter(|cs| cs.cs_type == 0xDA)
            .map(|cs| decode_trn(cs.params, Codec::cp500(), true))
            .filter(|r| has_non_space(r))
            .collect::<Vec<_>>()
            .join(" "),
        sf::NOP => readable(&field.data),
        sf::TLE => parse_triplets(&field.data)
            .into_iter()
            .filter(|(tid, t)| (*tid == 0x02 || *tid == 0x36) && t.len() > 2)
            .map(|(_, t)| readable(&t[2..]))
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn glyph_list(label: &str, items: &[String]) -> String {
    let shown: Vec<String> = items.iter().take(8).enumerate().map(|(i, v)| format!("{i}={v}")).collect();
    let more = if items.len() > 8 { format!(", +{} more", items.len() - 8) } else { String::new() };
    format!("{} {label}: {}{more}", items.len(), shown.join(", "))
}

fn resolve_coded_font<'a>(
    name: &str,
    coded_fonts: &'a IndexMap<String, (String, String)>,
) -> Option<&'a (String, String)> {
    let hit = coded_fonts.get(name);
    if hit.is_some() || name.chars().count() < 2 {
        return hit;
    }
    let norm = norm_cf(name);
    coded_fonts.iter().find(|(cf, _)| cf.chars().count() >= 2 && norm_cf(cf) == norm).map(|(_, p)| p)
}

fn mapping_option(v: u8) -> String {
    match v {
        0x00 => "Position".into(),
        0x10 => "Position and Trim".into(),
        0x20 => "Scale to Fit".into(),
        0x30 => "Center and Trim".into(),
        0x41 => "Replicate and Trim".into(),
        0x60 => "Scale to Fill".into(),
        o => format!("option 0x{o:02X}"),
    }
}

/// A readable one-line summary of a field's contents, or "".
pub fn field_data_summary(
    field: &StructuredField,
    search_text: &str,
    triplets: &[TripletRow],
    font_fnc: &[u8],
    coded_fonts: &IndexMap<String, (String, String)>,
) -> String {
    if !search_text.is_empty() {
        return search_text.to_string();
    }
    let (sid, d) = (field.sf_id, &field.data[..]);
    let b16 = |a: usize| u16_at(d, a) as u64;
    if sid == sf::PGD && d.len() >= 12 {
        let (w, h, upi) = parse_pgd(d);
        return format!("page {w}×{h} L-units · {upi} units/inch");
    }
    if sid == sf::PTD1 || sid == sf::PTD2 {
        let format1 = sid == sf::PTD1;
        if d.len() >= if format1 { 10 } else { 12 } {
            let (w, h, upi) = parse_ptd(d, format1);
            return format!("text area {w}×{h} L-units · {upi} units/inch");
        }
    }
    if sid == sf::IDD && d.len() >= 9 {
        return format!("image {}×{} pels · {}×{} DPI", b16(5), b16(7), b16(1) / 10, b16(3) / 10);
    }
    if sid == sf::IID && d.len() >= 32 {
        return format!("IM image {}×{} pels · {} DPI", b16(18), b16(20), b16(14) / 10);
    }
    if sid == sf::ICP && d.len() >= 8 {
        return format!("cell @{},{} size {}×{}", b16(0), b16(2), b16(4), b16(6));
    }
    if sid == sf::IRD {
        return format!("IM raster data ({} bytes)", d.len());
    }
    if sid == MCF_FORMAT_1 || sid == MCF_FORMAT_2 {
        let format1 = sid == MCF_FORMAT_1;
        let res = mcf_font_resources(d, format1);
        if !res.is_empty() {
            let cfs = mcf_coded_fonts(d, format1);
            let parts: Vec<String> = res
                .into_iter()
                .map(|(lid, (cp, cs))| {
                    let (mut cp, mut cs) = (cp.filter(|s| !s.is_empty()), cs.filter(|s| !s.is_empty()));
                    let cf = cfs.get(&lid);
                    if cs.is_none()
                        && let Some((rcs, rcp)) = cf.and_then(|cf| resolve_coded_font(cf, coded_fonts))
                    {
                        cs = Some(rcs.clone()).filter(|s| !s.is_empty());
                        cp = Some(rcp.clone()).filter(|s| !s.is_empty());
                    }
                    let via = cf.map(|cf| format!(" via CF={cf}")).unwrap_or_default();
                    format!("{lid}:(CP={} CS={}){via}", cp.as_deref().unwrap_or("?"), cs.as_deref().unwrap_or("?"))
                })
                .collect();
            return parts.join("; ");
        }
    }
    if sid == sf::MIO {
        let tail = crate::util::tail(d, 2);
        if let Some((_, t)) = parse_triplets(tail).into_iter().find(|(tid, t)| *tid == 0x04 && !t.is_empty()) {
            return format!("Image-to-Object Mapping={}", mapping_option(t[0]));
        }
    }
    if sid == sf::IPD {
        if d.starts_with(&[0xFE, 0x92]) && d.len() >= 4 {
            return format!("ImageDataLen={} <image data>", b16(2));
        }
        return format!("image data ({} bytes)", d.len());
    }
    if sid == sf::OBD {
        let (mut size, mut dpi, mut pos) = (None, None, None);
        for (tid, t) in parse_triplets(d) {
            if tid == 0x4C && t.len() >= 7 {
                size = Some(format!("Size={}×{}", be_uint(t, 1, 3), be_uint(t, 4, 3)));
            } else if tid == 0x4B && t.len() >= 6 {
                dpi = Some(format!("DPI={}×{}", u16_at(t, 2) / 10, u16_at(t, 4) / 10));
            } else if tid == 0x43 && !t.is_empty() {
                pos = Some(format!("DescriptorPos={}", t[0]));
            }
        }
        let ordered: Vec<String> = [size, dpi, pos].into_iter().flatten().collect();
        if !ordered.is_empty() {
            return ordered.join(" ");
        }
    }
    if sid == sf::OBP && d.len() >= 23 {
        let b = |a: usize, z: usize| be_uint(d, a, z - a);
        return format!(
            "AreaPos={},{} AreaRot={},{} ContentPos={},{} ContentRot={},{}",
            b(2, 5),
            b(5, 8),
            b(8, 10) / 128,
            b(10, 12) / 128,
            b(13, 16),
            b(16, 19),
            b(19, 21) / 128,
            b(21, 23) / 128
        );
    }
    if sid == sf::MPO {
        let mapping = parse_mpo(d);
        if !mapping.is_empty() {
            return mapping.iter().map(|(i, n)| format!("id {i} → {n}")).collect::<Vec<_>>().join("; ");
        }
    }
    if sid == sf::IPO && d.len() >= 8 {
        let reference = ebcdic_strip(&d[..8]);
        let ox = if d.len() >= 11 { s24_at(d, 8) } else { 0 };
        let oy = if d.len() >= 14 { s24_at(d, 11) } else { 0 };
        return format!("overlay {} @ offset {ox},{oy}", py_repr(&reference));
    }
    if sid == sf::FNI && !font_fnc.is_empty() {
        let rg = font_fnc.get(15).map(|&b| b as usize).unwrap_or(28);
        if rg >= 8 {
            let mut gcgids = Vec::new();
            let mut i = 0;
            while i + rg <= d.len() {
                gcgids.push(decode_name(&d[i..i + 8]));
                i += rg;
            }
            return glyph_list("chars", &gcgids);
        }
    }
    if sid == sf::FNM && d.len() >= 8 {
        let mut boxes = Vec::new();
        let mut i = 0;
        while i + 8 <= d.len() {
            boxes.push(format!("{}x{}", u16_at(d, i) as u32 + 1, u16_at(d, i + 2) as u32 + 1));
            i += 8;
        }
        return glyph_list("patterns", &boxes);
    }
    let foca = describe_foca_field(field);
    if !foca.is_empty() {
        return foca;
    }
    if !triplets.is_empty() {
        let bits: Vec<String> = triplets
            .iter()
            .take(3)
            .map(|t| {
                let name = t.name.split(" (").next().unwrap_or("");
                if t.detail.is_empty() { name.to_string() } else { format!("{name}: {}", t.detail) }
            })
            .collect();
        let more = if triplets.len() > 3 { format!(" (+{} more)", triplets.len() - 3) } else { String::new() };
        return bits.join("; ") + &more;
    }
    String::new()
}

/// Flatten structured fields into display rows with nesting depth and page.
pub fn field_rows(fields: &[StructuredField]) -> Vec<FieldRow> {
    let mut rows = Vec::with_capacity(fields.len());
    let mut depth = 0usize;
    let mut page_idx: Option<usize> = None;
    let mut current_page: Option<usize> = None;
    let mut font_fnc: &[u8] = &[];
    let coded_fonts = parse_coded_fonts(fields);
    for f in fields {
        match f.sf_id {
            sf::BFN | sf::EFN => font_fnc = &[], // BFN / EFN
            sf::FNC => font_fnc = &f.data,       // FNC
            _ => {}
        }
        if f.type_code() == TYPE_END && depth > 0 {
            depth -= 1;
        }
        if f.sf_id == sf::BPG {
            let next = page_idx.map_or(0, |p| p + 1);
            page_idx = Some(next);
            current_page = Some(next);
        }
        let triplets = describe_field(f);
        let search_text = field_search_text(f);
        let data = field_data_summary(f, &search_text, &triplets, font_fnc, &coded_fonts);
        rows.push(FieldRow {
            offset: f.offset,
            sf_id: f.sf_id,
            name: f.name(),
            token: f.token_name().unwrap_or_default(),
            size: f.data.len() + 9,
            data_len: f.data.len(),
            depth,
            preview: hex_sep(&f.data[..f.data.len().min(16)], " "),
            page: current_page,
            triplets,
            search_text,
            data,
        });
        if f.sf_id == sf::EPG {
            current_page = None;
        }
        if f.type_code() == TYPE_BEGIN {
            depth += 1;
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sf(sf_id: u32, data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x5A];
        out.extend_from_slice(&((data.len() + 8) as u16).to_be_bytes());
        out.extend_from_slice(&sf_id.to_be_bytes()[1..]);
        out.extend_from_slice(&[0, 0, 0]);
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn depth_pages_and_text() {
        let mut file = sf(sf::BDT, &[0x40; 8]);
        file.extend(sf(sf::BPG, &[0x40; 8]));
        file.extend(sf(sf::PTX, &[0x2B, 0xD3, 0x07, 0xDA, 0xC8, 0x85, 0x93, 0x93, 0x96]));
        file.extend(sf(sf::EPG, &[0x40; 8]));
        file.extend(sf(sf::EDT, &[0x40; 8]));
        let ins = inspect(&file, Codec::cp500(), false).unwrap();
        let depths: Vec<usize> = ins.rows.iter().map(|r| r.depth).collect();
        assert_eq!(depths, vec![0, 1, 2, 1, 0]);
        let pages: Vec<Option<usize>> = ins.rows.iter().map(|r| r.page).collect();
        assert_eq!(pages, vec![None, Some(0), Some(0), Some(0), None]);
        assert_eq!(ins.rows[2].data, "Hello");
        assert_eq!(ins.rows[2].sf_id_hex(), "0xD3EE9B");
        assert_eq!(ins.pages.len(), 1);
        assert_eq!(ins.resource_kind, None);
        assert_eq!(ins.summary[0].1, 1);
    }

    #[test]
    fn readable_prefers_more_printable() {
        assert_eq!(readable(b"hello  world"), "hello world");
        assert_eq!(readable(&[0xC8, 0x85, 0x93, 0x93, 0x96]), "Hello");
    }
}
