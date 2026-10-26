//! MO:DCA triplet decoding and per-field triplet location.
//!
//! A triplet is a self-identifying parameter:
//!
//! ```text
//! u8 length            counts the length and id bytes too
//! u8 id                triplet identifier
//! data                 (length - 2 bytes)
//! ```
//!
//! Triplets ride on most structured fields, either directly after a fixed
//! prefix (Begin/End fields put them after the 8-byte name) or inside
//! repeating groups introduced by a u16 group length (MCF, MDR, MPS, ...).
//!
//! A run of triplets is only reported when it parses cleanly through to the
//! end of its slot; anything else is shown as plain hex by the caller, so a
//! wrong offset can't dress arbitrary bytes up as decoded structure.

use indexmap::{IndexMap, IndexSet};
use serde::Serialize;

use crate::codepages::{Codec, codec_for_codepage_name, codec_for_cpgid, ebcdic, ebcdic_strip, utf16be_if_likely};
use crate::model::ResourceKind;
use crate::parser::{StructuredField, TYPE_BEGIN, TYPE_END, sf};
use crate::util::{be_uint, fmt_g, hex_sep, is_all_digits, py_strip, u16_at};

pub const TRIPLET_NAMES: &[(u8, &str)] = &[
    (0x01, "Coded Graphic Character Set Global Identifier"),
    (0x02, "Fully Qualified Name"),
    (0x04, "Mapping Option"),
    (0x10, "Object Classification"),
    (0x18, "MO:DCA Interchange Set"),
    (0x1F, "Font Descriptor Specification"),
    (0x21, "Object Function Set Specification"),
    (0x22, "Extended Resource Local Identifier"),
    (0x24, "Resource Local Identifier"),
    (0x25, "Resource Section Number"),
    (0x26, "Character Rotation"),
    (0x2D, "Object Byte Offset"),
    (0x36, "Attribute Value"),
    (0x43, "Descriptor Position"),
    (0x45, "Media Eject Control"),
    (0x46, "Page Overlay Conditional Processing"),
    (0x47, "Resource Usage Attribute"),
    (0x4B, "Measurement Units"),
    (0x4C, "Object Area Size"),
    (0x4D, "Area Definition"),
    (0x4E, "Color Specification"),
    (0x50, "Encoding Scheme ID"),
    (0x56, "Medium Map Page Number"),
    (0x57, "Object Byte Extent"),
    (0x58, "Object Structured Field Offset"),
    (0x59, "Object Structured Field Extent"),
    (0x5A, "Object Offset"),
    (0x62, "Local Date and Time Stamp"),
    (0x65, "Comment"),
    (0x68, "Medium Orientation"),
    (0x6C, "Resource Object Include"),
    (0x70, "Presentation Space Reset Mixing"),
    (0x71, "Presentation Space Mixing Rule"),
    (0x72, "Universal Date and Time Stamp"),
    (0x74, "Toner Saver"),
    (0x75, "Color Fidelity"),
    (0x78, "Font Fidelity"),
    (0x80, "Attribute Qualifier"),
    (0x81, "Page Position Information"),
    (0x82, "Parameter Value"),
    (0x83, "Presentation Control"),
    (0x84, "Font Resolution and Metric Technology"),
    (0x85, "Finishing Operation"),
    (0x86, "Text Fidelity"),
    (0x87, "Media Fidelity"),
    (0x88, "Finishing Fidelity"),
    (0x8B, "Data Object Font Descriptor"),
    (0x8C, "Locale Selector"),
    (0x8E, "UP3i Finishing Operation"),
    (0x91, "Color Management Resource Descriptor"),
    (0x95, "Rendering Intent"),
    (0x96, "CMR Tag Fidelity"),
    (0x97, "Device Appearance"),
    (0x9A, "Image Resolution"),
    (0x9C, "Object Container Presentation Space Size"),
];

/// Fully Qualified Name (0x02) type codes.
pub const FQN_TYPES: &[(u8, &str)] = &[
    (0x01, "Replace First GID Name"),
    (0x07, "Font Family Name"),
    (0x08, "Font Typeface Name"),
    (0x09, "MO:DCA Resource Hierarchy Reference"),
    (0x0A, "Begin Resource Group Reference"),
    (0x0B, "Attribute GID"),
    (0x0C, "Process Element GID"),
    (0x0D, "Begin Page Group Reference"),
    (0x11, "Media Type Reference"),
    (0x41, "Color Management Resource Reference"),
    (0x6E, "Data Object Font Base Font Identifier"),
    (0x7E, "Data Object Font Linked Font Identifier"),
    (0x83, "Begin Document Reference"),
    (0x84, "Resource Object Reference"),
    (0x85, "Code Page Name Reference"),
    (0x86, "Font Character Set Name Reference"),
    (0x87, "Begin Document Index Reference"),
    (0x8D, "Begin Overlay Reference"),
    (0x8E, "Data Object Resource Reference"),
    (0xBE, "Coded Font Name Reference"),
    (0xCE, "Other Object Data Reference"),
    (0xDE, "Data Object External Resource Reference"),
];

const OBJECT_CLASSES: &[(u8, &str)] = &[
    (0x01, "time-invariant paginated presentation object"),
    (0x10, "time-variant presentation object"),
    (0x20, "executable program"),
    (0x30, "setup file"),
    (0x40, "secondary resource"),
    (0x41, "data-object font"),
];

const RESOURCE_ID_TYPES: &[(u8, &str)] =
    &[(0x00, "usage-dependent"), (0x02, "page overlay"), (0x05, "coded font"), (0x07, "color attribute table")];

const ORIENTATIONS: &[(u8, &str)] = &[
    (0x00, "portrait"),
    (0x01, "landscape"),
    (0x02, "reverse portrait"),
    (0x03, "reverse landscape"),
    (0x04, "portrait 90°"),
    (0x05, "landscape 90°"),
];

fn lookup<K: PartialEq + Copy>(table: &[(K, &'static str)], key: K) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

pub fn triplet_name(tid: u8) -> &'static str {
    lookup(TRIPLET_NAMES, tid).unwrap_or("Unknown triplet")
}

/// Structured fields whose data is a fixed prefix followed by triplets;
/// values are candidate start offsets, tried in order.
fn triplet_offsets(sf_id: u32) -> Option<&'static [usize]> {
    Some(match sf_id {
        sf::TLE => &[0],       // TLE: triplets only
        sf::OBD => &[0],       // OBD: triplets only
        sf::IMM => &[8],       // IMM: name(8)
        sf::IOB => &[27],      // IOB: object placement prefix
        sf::IPS => &[14],      // IPS: name(8) + Xpsoset(3) + Ypsoset(3)
        sf::IPO => &[14],      // IPO: name(8) + Xoloset(3) + Yoloset(3)
        sf::PGD => &[15, 14],  // PGD: bases/units/size + reserved
        sf::MDD => &[13],      // MDD: bases/units/size + flags byte
        sf::PTD2 => &[14, 12], // PTD format 2
        _ => return None,
    })
}

/// Structured fields built of u16-length repeating groups of triplets.
fn is_grouped(sf_id: u32) -> bool {
    matches!(
        sf_id,
        sf::MCF2 // MCF format 2
            | sf::MDR // MDR
            | sf::MPG // MPS
            | sf::MPO // MPO
            | sf::MBC // MBC
    )
}

pub const MCF_FORMAT_1: u32 = sf::MCF1;
pub const MCF_FORMAT_2: u32 = sf::MCF2;

/// Split `data` into length-prefixed records: a `prefix`-byte header whose
/// first byte(s) give the record length (1 byte for triplets, 2 for
/// repeating groups), including the header. Stops at the first malformed
/// length; also returns whether the records tile `data` exactly.
fn split_records(data: &[u8], prefix: usize) -> (Vec<&[u8]>, bool) {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 2 <= data.len() {
        let length = if prefix == 1 { data[pos] as usize } else { u16_at(data, pos) as usize };
        if length < 2 || pos + length > data.len() {
            return (out, false);
        }
        out.push(&data[pos..pos + length]);
        pos += length;
    }
    let exact = !out.is_empty() && pos == data.len();
    (out, exact)
}

/// (triplet id, triplet data) from a run of MO:DCA triplets, stopping
/// quietly at the first malformed one.
pub fn parse_triplets(data: &[u8]) -> Vec<(u8, &[u8])> {
    split_records(data, 1).0.into_iter().map(|t| (t[1], &t[2..])).collect()
}

/// Parse triplets that must tile `data` exactly, else `None`.
fn consume_triplets(data: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    let (records, exact) = split_records(data, 1);
    exact.then(|| records.into_iter().map(|t| (t[1], &t[2..])).collect())
}

/// One located triplet: repeating-group index (grouped fields only), id, data.
pub type LocatedTriplet<'a> = (Option<usize>, u8, &'a [u8]);

/// Locate a field's triplets. Empty when the field type carries no triplets
/// or its bytes do not parse cleanly as a run of them.
pub fn field_triplets(field: &StructuredField) -> Vec<LocatedTriplet<'_>> {
    let data = &field.data[..];
    if is_grouped(field.sf_id) {
        return grouped_triplets(data);
    }
    let begin_end_ffff: &[usize] = &[8, 10, 2];
    let begin_end: &[usize] = &[8, 10];
    let offsets = match triplet_offsets(field.sf_id) {
        Some(o) => o,
        None if field.type_code() == TYPE_BEGIN || field.type_code() == TYPE_END => {
            // Begin/End: 8-byte name, sometimes followed by 2 reserved bytes
            // (BDT, BRS), or a 2-byte X'FFFF' "match any" token instead.
            if data.len() >= 2 && data[0] == 0xFF && data[1] == 0xFF { begin_end_ffff } else { begin_end }
        }
        None => return Vec::new(),
    };
    for &off in offsets {
        if data.len() > off
            && let Some(trips) = consume_triplets(&data[off..])
        {
            return trips.into_iter().map(|(tid, d)| (None, tid, d)).collect();
        }
    }
    Vec::new()
}

fn grouped_triplets(data: &[u8]) -> Vec<LocatedTriplet<'_>> {
    let (groups, exact) = split_records(data, 2);
    if !exact {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (index, group) in groups.into_iter().enumerate() {
        let Some(trips) = consume_triplets(&group[2..]) else { return Vec::new() };
        out.extend(trips.into_iter().map(|(tid, d)| (Some(index), tid, d)));
    }
    out
}

/// Decode a GID character string: UTF-16BE when it looks like it, else
/// EBCDIC. Producers that declare CCSID 1200 encode FQN names in UTF-16BE;
/// Latin text there has a zero high byte for nearly every character.
pub fn name_string(b: &[u8]) -> String {
    match utf16be_if_likely(b) {
        Some(s) => py_strip(&s).to_string(),
        None => ebcdic_strip(b),
    }
}

fn codec_suffix(codec: Option<Codec>) -> String {
    codec.map(|c| format!(" → {}", c.name())).unwrap_or_default()
}

fn cgcsgid_detail(t: &[u8]) -> String {
    if t.len() < 4 {
        return String::new();
    }
    let (gcsgid, cpgid) = (u16_at(t, 0), u16_at(t, 2));
    // A zero GCSGID means the second half is a CCSID, not a CPGID.
    let label = if gcsgid == 0 { format!("CCSID {cpgid}") } else { format!("GCSGID {gcsgid}, CPGID {cpgid}") };
    label + &codec_suffix(codec_for_cpgid(cpgid as u32))
}

fn fqn_detail(t: &[u8]) -> String {
    if t.len() < 2 {
        return String::new();
    }
    let (fqn_type, fqn_format) = (t[0], t[1]);
    let raw = &t[2..];
    let name = match fqn_format {
        0x20 => Codec::lookup("ascii").unwrap().decode_replace(raw), // URL
        0x10 => format!("OID {}", crate::util::hex(raw)),            // ASN.1 OID
        _ => name_string(raw),
    };
    let kind = lookup(FQN_TYPES, fqn_type).map(str::to_string).unwrap_or_else(|| format!("type 0x{fqn_type:02X}"));
    let mut detail = format!("{kind}: {name}");
    if fqn_type == 0x85 {
        detail += &codec_suffix(codec_for_codepage_name(&name));
    }
    detail
}

fn objclass_detail(t: &[u8]) -> String {
    if t.is_empty() {
        return String::new();
    }
    let mut parts =
        vec![lookup(OBJECT_CLASSES, t[0]).map(str::to_string).unwrap_or_else(|| format!("class 0x{:02X}", t[0]))];
    if t.len() >= 53 {
        let type_name = ebcdic_strip(&t[21..53]);
        if !type_name.is_empty() {
            parts.push(type_name);
        }
    }
    parts.join(", ")
}

fn resource_id_detail(t: &[u8]) -> String {
    if t.len() < 2 {
        return String::new();
    }
    let kind = lookup(RESOURCE_ID_TYPES, t[0]).map(str::to_string).unwrap_or_else(|| format!("type 0x{:02X}", t[0]));
    format!("{kind}, local id {}", t[1])
}

fn rotation_detail(t: &[u8]) -> String {
    if t.len() < 2 {
        return String::new();
    }
    match u16_at(t, 0) {
        0x0000 => "0°".into(),
        0x2D00 => "90°".into(),
        0x5A00 => "180°".into(),
        0x8700 => "270°".into(),
        v => format!("X'{v:04X}'"),
    }
}

fn units_detail(t: &[u8]) -> String {
    if t.len() < 6 {
        return String::new();
    }
    let base = match t[0] {
        0x00 => "10 in".to_string(),
        0x01 => "10 cm".to_string(),
        b => format!("base 0x{b:02X}"),
    };
    format!("{} × {} units per {base}", u16_at(t, 2), u16_at(t, 4))
}

fn area_size_detail(t: &[u8]) -> String {
    if t.len() < 7 {
        return String::new();
    }
    format!("{} × {}", be_uint(t, 1, 3), be_uint(t, 4, 3))
}

fn datetime_detail(t: &[u8]) -> String {
    if t.len() < 15 {
        return String::new();
    }
    let kind = match t[0] {
        0x00 => "created".to_string(),
        0x01 => "retired".to_string(),
        0x03 => "revised".to_string(),
        b => format!("type 0x{b:02X}"),
    };
    // EBCDIC digits: century flag, YY, day-of-year DDD, HH, MM, SS, hundredths.
    let s: Vec<char> = ebcdic(&t[1..15]).chars().collect();
    let part = |a: usize, b: usize| -> String { s[a..b].iter().collect() };
    let (century, yy, ddd) = (s[0], part(1, 3), part(3, 6));
    let (hh, mm, ss) = (part(6, 8), part(8, 10), part(10, 12));
    if !is_all_digits(&yy) {
        return kind;
    }
    let base = if century == ' ' || century == '0' {
        1900
    } else if century.is_ascii_digit() {
        1900 + 100 * century.to_digit(10).unwrap() as i64
    } else {
        return kind;
    };
    let year = base + yy.parse::<i64>().unwrap_or(0);
    format!("{kind} {year} day {ddd}, {hh}:{mm}:{ss}")
}

fn comment_detail(t: &[u8]) -> String {
    if t.is_empty() {
        return String::new();
    }
    if t.iter().all(|&b| (0x20..0x7F).contains(&b) || b == 0x09) {
        return format!("\"{}\" (ASCII)", t.iter().map(|&b| b as char).collect::<String>());
    }
    format!("\"{}\" (EBCDIC)", ebcdic_strip(t))
}

fn attribute_value_detail(t: &[u8]) -> String {
    if t.len() < 2 {
        return String::new();
    }
    format!("\"{}\"", ebcdic_strip(&t[2..]))
}

fn orientation_detail(t: &[u8]) -> String {
    if t.is_empty() {
        return String::new();
    }
    lookup(ORIENTATIONS, t[0]).map(str::to_string).unwrap_or_else(|| format!("0x{:02X}", t[0]))
}

fn font_descriptor_detail(t: &[u8]) -> String {
    if t.len() < 4 {
        return String::new();
    }
    format!("{} pt", fmt_g(u16_at(t, 2) as f64 / 20.0))
}

fn byte_extent_detail(t: &[u8]) -> String {
    if t.len() < 4 {
        return String::new();
    }
    format!("{} bytes", be_uint(t, 0, 4))
}

fn encoding_scheme_detail(t: &[u8]) -> String {
    if t.len() < 2 {
        return String::new();
    }
    format!("scheme X'{:04X}'", u16_at(t, 0))
}

/// Human-readable decode of a triplet's data; empty when none is known.
pub fn describe_triplet(tid: u8, t: &[u8]) -> String {
    match tid {
        0x01 => cgcsgid_detail(t),
        0x02 => fqn_detail(t),
        0x10 => objclass_detail(t),
        0x24 => resource_id_detail(t),
        0x25 => t.first().map(|b| format!("section {b}")).unwrap_or_default(),
        0x26 => rotation_detail(t),
        0x36 => attribute_value_detail(t),
        0x4B => units_detail(t),
        0x4C => area_size_detail(t),
        0x50 => encoding_scheme_detail(t),
        0x57 => byte_extent_detail(t),
        0x62 => datetime_detail(t),
        0x65 => comment_detail(t),
        0x68 => orientation_detail(t),
        0x8B => font_descriptor_detail(t),
        _ => String::new(),
    }
}

/// One decoded triplet row for the inspector.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TripletRow {
    pub id: String,
    pub name: String,
    pub detail: String,
    pub hex: String,
    pub group: Option<usize>,
}

/// Decode a field's triplets into display rows. MCF format 1 is not
/// triplet-based; its fixed groups get one row each.
pub fn describe_field(field: &StructuredField) -> Vec<TripletRow> {
    if field.sf_id == MCF_FORMAT_1 {
        return describe_mcf1(&field.data);
    }
    field_triplets(field)
        .into_iter()
        .map(|(group, tid, t)| TripletRow {
            id: format!("0x{tid:02X}"),
            name: triplet_name(tid).to_string(),
            detail: describe_triplet(tid, t),
            hex: hex_sep(t, " "),
            group,
        })
        .collect()
}

/// One MCF format-1 repeating group.
#[derive(Debug, Clone)]
pub struct Mcf1Group<'a> {
    pub local_id: u8,
    pub coded_font: Option<String>,
    pub code_page: Option<String>,
    pub char_set: Option<String>,
    pub raw: &'a [u8],
}

/// MCF format-1 fixed repeating groups.
///
/// Header: RG length(1) + reserved(3). Each group: coded font local id(1),
/// reserved(3), coded font name(8), code page name(8), font character set
/// name(8); X'FF...' means "not present".
pub fn mcf1_groups(data: &[u8]) -> Vec<Mcf1Group<'_>> {
    if data.len() < 4 {
        return Vec::new();
    }
    let rg_len = data[0] as usize;
    if rg_len < 28 || !(data.len() - 4).is_multiple_of(rg_len) {
        return Vec::new();
    }
    let name = |rg: &[u8], start: usize| -> Option<String> {
        let raw = crate::util::slice(rg, start, start + 8);
        // X'FF' fill is the "not present" sentinel; some producers write only
        // a partial fill, so any leading X'FF' means absent.
        if raw.is_empty() || raw[0] == 0xFF {
            return None;
        }
        let s = ebcdic(raw);
        let s = py_strip(s.trim_matches(|c| c == '\0' || c == ' ')).to_string();
        if s.is_empty() { None } else { Some(s) }
    };
    (4..data.len())
        .step_by(rg_len)
        .map(|pos| {
            let rg = &data[pos..(pos + rg_len).min(data.len())];
            Mcf1Group {
                local_id: rg[0],
                coded_font: name(rg, 4),
                code_page: name(rg, 12),
                char_set: name(rg, 20),
                raw: rg,
            }
        })
        .collect()
}

fn describe_mcf1(data: &[u8]) -> Vec<TripletRow> {
    mcf1_groups(data)
        .into_iter()
        .enumerate()
        .map(|(group, g)| {
            let mut bits = vec![format!("coded font local id {}", g.local_id)];
            if let Some(cf) = &g.coded_font {
                bits.push(format!("coded font {cf}"));
            }
            if let Some(cp) = &g.code_page {
                bits.push(format!("code page {cp}{}", codec_suffix(codec_for_codepage_name(cp))));
            }
            if let Some(cs) = &g.char_set {
                bits.push(format!("character set {cs}"));
            }
            TripletRow {
                id: "—".into(),
                name: "Coded-font mapping".into(),
                detail: bits.join(" · "),
                hex: hex_sep(g.raw, " "),
                group: Some(group),
            }
        })
        .collect()
}

/// A code page an MCF assigned to a coded-font local id.
#[derive(Debug, Clone, PartialEq)]
pub struct McfCodePage {
    /// As labeled in the file ("T1V10500", "CCSID 500", ...).
    pub name: String,
    /// Decoder, when the label resolves to one.
    pub codec: Option<Codec>,
}

/// The triplets of each u16-length repeating group (MCF format 2, MDR, MPO),
/// stopping at the first malformed group length.
pub(crate) fn parse_groups(data: &[u8]) -> Vec<Vec<(u8, &[u8])>> {
    split_records(data, 2).0.into_iter().map(|g| parse_triplets(&g[2..])).collect()
}

fn is_local_id(t: &[u8]) -> bool {
    t.len() >= 2 && (t[0] == 0x00 || t[0] == 0x05)
}

/// Code page assignments per coded-font local id from an MCF.
pub fn parse_mcf_codepages(data: &[u8], format1: bool) -> IndexMap<u8, McfCodePage> {
    let mut out = IndexMap::new();
    if format1 {
        for g in mcf1_groups(data) {
            if let Some(cp) = g.code_page {
                let codec = codec_for_codepage_name(&cp);
                out.insert(g.local_id, McfCodePage { name: cp, codec });
            }
        }
        return out;
    }
    for group in parse_groups(data) {
        let mut local_id = None;
        let mut cp_name: Option<String> = None;
        let mut cgcsgid: Option<(u16, u16)> = None;
        for (tid, t) in group {
            if tid == 0x24 && is_local_id(t) {
                local_id = Some(t[1]);
            } else if tid == 0x02 && t.len() >= 3 && t[0] == 0x85 {
                cp_name = Some(ebcdic_strip(&t[2..]));
            } else if tid == 0x01 && t.len() >= 4 {
                cgcsgid = Some((u16_at(t, 0), u16_at(t, 2)));
            }
        }
        let mut codec = cgcsgid.and_then(|(_, cp)| codec_for_cpgid(cp as u32));
        if codec.is_none()
            && let Some(n) = &cp_name
        {
            codec = codec_for_codepage_name(n);
        }
        let label =
            cp_name.or_else(|| cgcsgid.map(|(g, cp)| format!("{} {cp}", if g == 0 { "CCSID" } else { "CPGID" })));
        if let (Some(lid), Some(label)) = (local_id, label)
            && !label.is_empty()
        {
            out.insert(lid, McfCodePage { name: label, codec });
        }
    }
    out
}

/// Each coded-font local id's (code page, character set) names.
pub fn mcf_font_resources(data: &[u8], format1: bool) -> IndexMap<u8, (Option<String>, Option<String>)> {
    let mut out = IndexMap::new();
    if format1 {
        for g in mcf1_groups(data) {
            out.insert(g.local_id, (g.code_page, g.char_set));
        }
        return out;
    }
    for group in parse_groups(data) {
        let mut local_id = None;
        let (mut cp_name, mut cs_name) = (None, None);
        for (tid, t) in group {
            if tid == 0x24 && is_local_id(t) {
                local_id = Some(t[1]);
            } else if tid == 0x02 && t.len() >= 3 && t[0] == 0x85 {
                cp_name = Some(ebcdic_strip(&t[2..]));
            } else if tid == 0x02 && t.len() >= 3 && t[0] == 0x86 {
                cs_name = Some(ebcdic_strip(&t[2..]));
            }
        }
        if let Some(lid) = local_id {
            out.insert(lid, (cp_name, cs_name));
        }
    }
    out
}

/// Each local font id's coded-font name, when an MCF group names one
/// directly (FQN X'8E' in format 2, the coded-font slot in format 1).
pub fn mcf_coded_fonts(data: &[u8], format1: bool) -> IndexMap<u8, String> {
    let mut out = IndexMap::new();
    if format1 {
        for g in mcf1_groups(data) {
            if let Some(cf) = g.coded_font {
                out.insert(g.local_id, cf);
            }
        }
        return out;
    }
    for group in parse_groups(data) {
        let mut local_id = None;
        let mut cf_name: Option<String> = None;
        for (tid, t) in group {
            if tid == 0x24 && is_local_id(t) {
                local_id = Some(t[1]);
            } else if tid == 0x02 && t.len() >= 3 && t[0] == 0x8E {
                cf_name = Some(ebcdic_strip(&t[2..]));
            }
        }
        if let (Some(lid), Some(cf)) = (local_id, cf_name)
            && !cf.is_empty()
        {
            out.insert(lid, cf);
        }
    }
    out
}

/// Resource kinds an MCF can reference.
pub const RESOURCE_KINDS: [ResourceKind; 3] =
    [ResourceKind::CodedFont, ResourceKind::CodePage, ResourceKind::CharacterSet];

/// Names of the font resources an MCF references, keyed by kind.
pub fn mcf_resource_names(data: &[u8], format1: bool) -> IndexMap<ResourceKind, IndexSet<String>> {
    let mut refs: IndexMap<ResourceKind, IndexSet<String>> =
        RESOURCE_KINDS.iter().map(|k| (*k, IndexSet::new())).collect();
    if format1 {
        for g in mcf1_groups(data) {
            if let Some(cf) = g.coded_font {
                refs[&ResourceKind::CodedFont].insert(cf);
            }
            if let Some(cp) = g.code_page {
                refs[&ResourceKind::CodePage].insert(cp);
            }
            if let Some(cs) = g.char_set {
                refs[&ResourceKind::CharacterSet].insert(cs);
            }
        }
        return refs;
    }
    for group in parse_groups(data) {
        for (tid, t) in group {
            if tid == 0x02 && t.len() >= 3 {
                let kind = match t[0] {
                    0x85 => ResourceKind::CodePage,
                    0x86 => ResourceKind::CharacterSet,
                    0x8E => ResourceKind::CodedFont,
                    _ => continue,
                };
                refs[&kind].insert(ebcdic_strip(&t[2..]));
            }
        }
    }
    refs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(sf_id: u32, data: Vec<u8>) -> StructuredField {
        StructuredField { offset: 0, sf_id, flags: 0, sequence: 0, data }
    }

    fn ebc(s: &str) -> Vec<u8> {
        // Encode ASCII letters/digits/space into cp500 for test fixtures.
        s.bytes()
            .map(|c| match c {
                b' ' => 0x40,
                b'0'..=b'9' => 0xF0 + (c - b'0'),
                b'A'..=b'I' => 0xC1 + (c - b'A'),
                b'J'..=b'R' => 0xD1 + (c - b'J'),
                b'S'..=b'Z' => 0xE2 + (c - b'S'),
                _ => panic!("unsupported"),
            })
            .collect()
    }

    #[test]
    fn begin_field_triplets_after_name() {
        let mut data = ebc("DOC00001");
        data.extend([0x06, 0x01, 0x00, 0x00, 0x01, 0xF4]); // CGCSGID CCSID 500
        let rows = describe_field(&field(sf::BDT, data));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "0x01");
        assert_eq!(rows[0].detail, "CCSID 500 → cp500");
        assert_eq!(rows[0].hex, "00 00 01 f4");
    }

    #[test]
    fn unclean_run_gives_no_triplets() {
        let mut data = ebc("DOC00001");
        data.extend([0x09, 0x01, 0x00]);
        assert!(describe_field(&field(sf::BDT, data)).is_empty());
    }

    #[test]
    fn mcf2_groups() {
        let mut grp = vec![];
        let mut fqn = vec![0x0C, 0x02, 0x85, 0x00];
        fqn.extend(ebc("T1V10500"));
        grp.extend(fqn);
        grp.extend([0x04, 0x24, 0x05, 0x01]);
        let mut data = ((grp.len() + 2) as u16).to_be_bytes().to_vec();
        data.extend(grp);
        let cps = parse_mcf_codepages(&data, false);
        assert_eq!(cps[&1].name, "T1V10500");
        assert_eq!(cps[&1].codec.unwrap().name(), "cp500");
        let rows = describe_field(&field(MCF_FORMAT_2, data));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].group, Some(0));
        assert_eq!(rows[0].detail, "Code Page Name Reference: T1V10500 → cp500");
    }

    #[test]
    fn mcf1_absent_slots() {
        let mut data = vec![28, 0, 0, 0];
        let mut rg = vec![3, 0, 0, 0];
        rg.extend(ebc("X0ABCDEF"));
        rg.extend([0xFF; 8]);
        rg.extend([0xFF, 0xFF, 0, 0, 0, 0, 0, 0]);
        data.extend(rg);
        let g = mcf1_groups(&data);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].local_id, 3);
        assert_eq!(g[0].coded_font.as_deref(), Some("X0ABCDEF"));
        assert!(g[0].code_page.is_none() && g[0].char_set.is_none());
    }

    #[test]
    fn details() {
        assert_eq!(describe_triplet(0x8B, &[0, 0, 0, 240]), "12 pt");
        assert_eq!(describe_triplet(0x4B, &[0, 0, 0x38, 0x40, 0x38, 0x40]), "14400 × 14400 units per 10 in");
        assert_eq!(describe_triplet(0x26, &[0x2D, 0x00]), "90°");
        assert_eq!(describe_triplet(0x65, b"hello"), "\"hello\" (ASCII)");
    }
}
