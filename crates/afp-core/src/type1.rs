//! Adobe Type 1 (PFB) outline font interpreter.
//!
//! FOCA outline fonts (PatTech X'1E'/X'1F') embed a vendor font program in
//! their FNG pattern data. For Adobe Type 1 fonts that program is a PFB
//! stream: a cleartext header, an `eexec`-encrypted binary block holding the
//! Private dict / Subrs / CharStrings, and a cleartext trailer.
//!
//! This module reconstructs the program, decrypts it, and interprets each
//! glyph's Type 1 charstring into an outline path in the font's own design
//! units (typically a 1000-unit em, y up).
//!
//! References: Adobe Type 1 Font Format, chapters 2 and 6.

use std::collections::HashMap;
use std::sync::OnceLock;

use indexmap::IndexMap;
use regex::bytes::Regex;

use crate::outline::{
    Glyph, MAX_GLYPH_OPS, MAX_GLYPH_SEGMENTS, MAX_GLYPH_STACK, Malformed, Res, Segment, units_per_em,
};
use crate::util::find_bytes;

const C1: u32 = 52845;
const C2: u32 = 22719;
const EEXEC_R: u32 = 55665;
const CHARSTRING_R: u32 = 4330;

const MAX_SUBR_DEPTH: usize = 30;
const MAX_SUBRS: usize = 65536;

fn decrypt(cipher: &[u8], mut r: u32, skip: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(cipher.len());
    for &b in cipher {
        out.push(b ^ (r >> 8) as u8);
        r = ((b as u32 + r).wrapping_mul(C1).wrapping_add(C2)) & 0xFFFF;
    }
    if skip < out.len() { out.split_off(skip) } else { Vec::new() }
}

/// Reconstruct the raw Type 1 program from a PFB-segmented FNG blob.
fn pfb_payload(fng: &[u8]) -> Vec<u8> {
    let start = fng.iter().position(|&b| b == 0x80);
    let Some(start) = start.filter(|s| s + 6 <= fng.len()) else {
        return match find_bytes(fng, b"%!") {
            Some(h) => fng[h..].to_vec(),
            None => fng.to_vec(),
        };
    };
    let mut out = Vec::new();
    let mut i = start;
    while i + 6 <= fng.len() && fng[i] == 0x80 {
        if fng[i + 1] == 3 {
            break; // EOF
        }
        let length = u32::from_le_bytes([fng[i + 2], fng[i + 3], fng[i + 4], fng[i + 5]]) as usize;
        let end = (i + 6).saturating_add(length).min(fng.len());
        out.extend_from_slice(&fng[i + 6..end]);
        i = (i + 6).saturating_add(length);
    }
    out
}

fn eexec_private(program: &[u8]) -> Vec<u8> {
    let Some(marker) = find_bytes(program, b"eexec") else {
        return Vec::new();
    };
    let mut pos = marker + 5;
    while pos < program.len() && b" \t\r\n".contains(&program[pos]) {
        pos += 1;
    }
    let mut cipher = program[pos..].to_vec();
    // Binary PFB hands us raw bytes; a raw stream may be ASCII-hex instead.
    let sample = &cipher[..cipher.len().min(4)];
    if sample.iter().all(|c| b"0123456789abcdefABCDEF \t\r\n".contains(c)) {
        let hexed: Vec<u8> = cipher.iter().copied().filter(u8::is_ascii_hexdigit).collect();
        let pairs = hexed.len() / 2;
        cipher = (0..pairs)
            .map(|k| {
                let s = std::str::from_utf8(&hexed[2 * k..2 * k + 2]).unwrap();
                u8::from_str_radix(s, 16).unwrap()
            })
            .collect();
    }
    decrypt(&cipher, EEXEC_R, 4)
}

/// Adobe StandardEncoding — code point → glyph name, for `seac`.
const STD_ENCODING: &[(i64, &str)] = &[
    (32, "space"),
    (33, "exclam"),
    (34, "quotedbl"),
    (35, "numbersign"),
    (36, "dollar"),
    (37, "percent"),
    (38, "ampersand"),
    (39, "quoteright"),
    (40, "parenleft"),
    (41, "parenright"),
    (42, "asterisk"),
    (43, "plus"),
    (44, "comma"),
    (45, "hyphen"),
    (46, "period"),
    (47, "slash"),
    (48, "zero"),
    (49, "one"),
    (50, "two"),
    (51, "three"),
    (52, "four"),
    (53, "five"),
    (54, "six"),
    (55, "seven"),
    (56, "eight"),
    (57, "nine"),
    (58, "colon"),
    (59, "semicolon"),
    (60, "less"),
    (61, "equal"),
    (62, "greater"),
    (63, "question"),
    (64, "at"),
    (65, "A"),
    (66, "B"),
    (67, "C"),
    (68, "D"),
    (69, "E"),
    (70, "F"),
    (71, "G"),
    (72, "H"),
    (73, "I"),
    (74, "J"),
    (75, "K"),
    (76, "L"),
    (77, "M"),
    (78, "N"),
    (79, "O"),
    (80, "P"),
    (81, "Q"),
    (82, "R"),
    (83, "S"),
    (84, "T"),
    (85, "U"),
    (86, "V"),
    (87, "W"),
    (88, "X"),
    (89, "Y"),
    (90, "Z"),
    (91, "bracketleft"),
    (92, "backslash"),
    (93, "bracketright"),
    (94, "asciicircum"),
    (95, "underscore"),
    (96, "quoteleft"),
    (97, "a"),
    (98, "b"),
    (99, "c"),
    (100, "d"),
    (101, "e"),
    (102, "f"),
    (103, "g"),
    (104, "h"),
    (105, "i"),
    (106, "j"),
    (107, "k"),
    (108, "l"),
    (109, "m"),
    (110, "n"),
    (111, "o"),
    (112, "p"),
    (113, "q"),
    (114, "r"),
    (115, "s"),
    (116, "t"),
    (117, "u"),
    (118, "v"),
    (119, "w"),
    (120, "x"),
    (121, "y"),
    (122, "z"),
    (123, "braceleft"),
    (124, "bar"),
    (125, "braceright"),
    (126, "asciitilde"),
    (161, "exclamdown"),
    (162, "cent"),
    (163, "sterling"),
    (164, "fraction"),
    (165, "yen"),
    (166, "florin"),
    (167, "section"),
    (168, "currency"),
    (169, "quotesingle"),
    (170, "quotedblleft"),
    (171, "guillemotleft"),
    (172, "guilsinglleft"),
    (173, "guilsinglright"),
    (174, "fi"),
    (175, "fl"),
    (177, "endash"),
    (178, "dagger"),
    (179, "daggerdbl"),
    (180, "periodcentered"),
    (182, "paragraph"),
    (183, "bullet"),
    (184, "quotesinglbase"),
    (185, "quotedblbase"),
    (186, "quotedblright"),
    (187, "guillemotright"),
    (188, "ellipsis"),
    (189, "perthousand"),
    (191, "questiondown"),
    (193, "grave"),
    (194, "acute"),
    (195, "circumflex"),
    (196, "tilde"),
    (197, "macron"),
    (198, "breve"),
    (199, "dotaccent"),
    (200, "dieresis"),
    (202, "ring"),
    (203, "cedilla"),
    (205, "hungarumlaut"),
    (206, "ogonek"),
    (207, "caron"),
    (208, "emdash"),
    (225, "AE"),
    (227, "ordfeminine"),
    (232, "Lslash"),
    (233, "Oslash"),
    (234, "OE"),
    (235, "ordmasculine"),
    (241, "ae"),
    (245, "dotlessi"),
    (248, "lslash"),
    (249, "oslash"),
    (250, "oe"),
    (251, "germandbls"),
];

fn std_encoding(code: i64) -> Option<&'static str> {
    STD_ENCODING.iter().find(|(c, _)| *c == code).map(|(_, n)| *n)
}

struct Patterns {
    len_iv: Regex,
    font_matrix: Regex,
    entry: Regex,
    subr: Regex,
}

fn patterns() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        len_iv: Regex::new(r"(?-u)/lenIV[ \t\n\r\x0b\x0c]+([0-9]+)").unwrap(),
        font_matrix: Regex::new(r"(?s-u)/FontMatrix[ \t\n\r\x0b\x0c]*\[([^\]]+)\]").unwrap(),
        entry: Regex::new(r"(?s-u)/([^ \t\r\n/{}\[\]()]+)[ \t]+([0-9]+)[ \t]+(RD|-\|) ").unwrap(),
        subr: Regex::new(r"(?-u)dup[ \t]+([0-9]+)[ \t]+([0-9]+)[ \t]+(RD|-\|) ").unwrap(),
    })
}

fn parse_usize(b: &[u8]) -> usize {
    std::str::from_utf8(b).ok().and_then(|s| s.parse().ok()).unwrap_or(usize::MAX)
}

/// A parsed Adobe Type 1 font program decoded from FOCA FNG bytes.
pub struct Type1Font {
    pub units_per_em: i64,
    subrs: Vec<Vec<u8>>,
    charstrings: IndexMap<String, Vec<u8>>,
}

impl Type1Font {
    pub fn new(fng: &[u8]) -> Type1Font {
        let program = pfb_payload(fng);
        let font_matrix = parse_font_matrix(&program);
        let units_per_em = units_per_em(font_matrix[0]);
        let private = eexec_private(&program);
        let len_iv = patterns().len_iv.captures(&private).map(|c| parse_usize(&c[1])).unwrap_or(4);
        let subrs = parse_subrs(&private, len_iv);
        let charstrings = parse_charstrings(&private, len_iv);
        Type1Font { units_per_em, subrs, charstrings }
    }

    pub fn glyph_names(&self) -> Vec<String> {
        self.charstrings.keys().cloned().collect()
    }

    pub fn has_glyph(&self, name: &str) -> bool {
        self.charstrings.contains_key(name)
    }

    /// Interpret a glyph's charstring into an outline, or `None`.
    pub fn glyph(&self, name: &str) -> Option<Glyph> {
        let cs = self.charstrings.get(name)?;
        let mut interp = Interpreter::new(&self.subrs, &self.charstrings);
        match interp.run(cs, 0) {
            Ok(()) => Some(Glyph { advance: interp.width, segments: interp.segments }),
            Err(Malformed) => {
                log::warn!("Type 1 charstring {name:?} failed");
                None
            }
        }
    }
}

fn parse_font_matrix(program: &[u8]) -> [f64; 6] {
    let default = [0.001, 0.0, 0.0, 0.001, 0.0, 0.0];
    let Some(c) = patterns().font_matrix.captures(program) else {
        return default;
    };
    let text = String::from_utf8_lossy(&c[1]);
    let vals: Result<Vec<f64>, _> = text.split_ascii_whitespace().map(str::parse::<f64>).collect();
    match vals {
        Ok(v) if v.len() == 6 => [v[0], v[1], v[2], v[3], v[4], v[5]],
        _ => default,
    }
}

fn parse_charstrings(private: &[u8], len_iv: usize) -> IndexMap<String, Vec<u8>> {
    let region = match find_bytes(private, b"/CharStrings") {
        Some(idx) => &private[idx..],
        None => private,
    };
    let mut chars = IndexMap::new();
    for c in patterns().entry.captures_iter(region) {
        let length = parse_usize(&c[2]);
        let start = c.get(0).unwrap().end();
        let raw = crate::util::slice(region, start, start.saturating_add(length));
        let key: String = c[1].iter().map(|&b| b as char).collect();
        chars.entry(key).or_insert_with(|| decrypt(raw, CHARSTRING_R, len_iv));
    }
    chars
}

fn parse_subrs(private: &[u8], len_iv: usize) -> Vec<Vec<u8>> {
    let Some(idx) = find_bytes(private, b"/Subrs") else {
        return Vec::new();
    };
    // End the scan at CharStrings so glyph entries are not mistaken for subrs.
    let end = match find_bytes(private, b"/CharStrings") {
        Some(e) if e > idx => e,
        _ => private.len(),
    };
    let region = &private[idx..end];
    let mut subrs: HashMap<usize, Vec<u8>> = HashMap::new();
    for c in patterns().subr.captures_iter(region) {
        let num = parse_usize(&c[1]);
        if num >= MAX_SUBRS {
            log::warn!("ignoring Type 1 subr {num} (limit {MAX_SUBRS})");
            continue;
        }
        let length = parse_usize(&c[2]);
        let start = c.get(0).unwrap().end();
        let raw = crate::util::slice(region, start, start.saturating_add(length));
        subrs.insert(num, decrypt(raw, CHARSTRING_R, len_iv));
    }
    let Some(&max) = subrs.keys().max() else {
        return Vec::new();
    };
    (0..=max).map(|i| subrs.remove(&i).unwrap_or_default()).collect()
}

struct Interpreter<'a> {
    subrs: &'a [Vec<u8>],
    charstrings: &'a IndexMap<String, Vec<u8>>,
    stack: Vec<f64>,
    ps_stack: Vec<f64>,
    segments: Vec<Segment>,
    x: f64,
    y: f64,
    width: f64,
    sbx: f64,
    open: bool,
    flex: bool,
    flex_pts: Vec<(f64, f64)>,
    done: bool,
    /// Operations run so far (see [`MAX_GLYPH_OPS`]).
    ops: usize,
    /// Interpreting a `seac` component, which must not itself use `seac`.
    in_seac: bool,
}

impl<'a> Interpreter<'a> {
    fn new(subrs: &'a [Vec<u8>], charstrings: &'a IndexMap<String, Vec<u8>>) -> Self {
        Interpreter {
            subrs,
            charstrings,
            stack: Vec::new(),
            ps_stack: Vec::new(),
            segments: Vec::new(),
            x: 0.0,
            y: 0.0,
            width: 0.0,
            sbx: 0.0,
            open: false,
            flex: false,
            flex_pts: Vec::new(),
            done: false,
            ops: 0,
            in_seac: false,
        }
    }

    fn moveto(&mut self, dx: f64, dy: f64) {
        self.x += dx;
        self.y += dy;
        if self.flex {
            self.flex_pts.push((self.x, self.y));
            return;
        }
        if self.open {
            self.segments.push(Segment::Z);
        }
        self.segments.push(Segment::M(self.x, self.y));
        self.open = true;
    }

    fn lineto(&mut self, dx: f64, dy: f64) {
        self.x += dx;
        self.y += dy;
        self.segments.push(Segment::L(self.x, self.y));
    }

    fn curveto(&mut self, d: &[f64]) -> Res<()> {
        if d.len() != 6 {
            return Err(Malformed);
        }
        let (x1, y1) = (self.x + d[0], self.y + d[1]);
        let (x2, y2) = (x1 + d[2], y1 + d[3]);
        let (x3, y3) = (x2 + d[4], y2 + d[5]);
        self.segments.push(Segment::C(x1, y1, x2, y2, x3, y3));
        self.x = x3;
        self.y = y3;
        Ok(())
    }

    fn close(&mut self) {
        if self.open {
            self.segments.push(Segment::Z);
            self.open = false;
        }
    }

    fn last(&self, k: usize) -> Res<f64> {
        let n = self.stack.len();
        if n >= k { Ok(self.stack[n - k]) } else { Err(Malformed) }
    }

    fn at(&self, k: usize) -> Res<f64> {
        self.stack.get(k).copied().ok_or(Malformed)
    }

    fn run(&mut self, cs: &[u8], depth: usize) -> Res<()> {
        if depth > MAX_SUBR_DEPTH {
            return Err(Malformed);
        }
        let byte = |i: usize| cs.get(i).copied().ok_or(Malformed);
        let mut i = 0usize;
        while i < cs.len() && !self.done {
            self.ops += 1;
            if self.ops > MAX_GLYPH_OPS
                || self.segments.len() > MAX_GLYPH_SEGMENTS
                || self.stack.len() > MAX_GLYPH_STACK
            {
                return Err(Malformed);
            }
            let b = cs[i];
            i += 1;
            if b >= 32 {
                let v = match b {
                    32..=246 => b as f64 - 139.0,
                    247..=250 => {
                        let v = (b as f64 - 247.0) * 256.0 + byte(i)? as f64 + 108.0;
                        i += 1;
                        v
                    }
                    251..=254 => {
                        let v = -(b as f64 - 251.0) * 256.0 - byte(i)? as f64 - 108.0;
                        i += 1;
                        v
                    }
                    _ => {
                        if i + 4 > cs.len() {
                            return Err(Malformed);
                        }
                        let v = i32::from_be_bytes([cs[i], cs[i + 1], cs[i + 2], cs[i + 3]]);
                        i += 4;
                        v as f64
                    }
                };
                self.stack.push(v);
                continue;
            }
            i = self.operator(b, cs, i, depth)?;
        }
        Ok(())
    }

    fn operator(&mut self, b: u8, cs: &[u8], mut i: usize, depth: usize) -> Res<usize> {
        match b {
            13 => {
                // hsbw: sbx wx
                self.sbx = self.stack.first().copied().unwrap_or(0.0);
                self.width = self.stack.get(1).copied().unwrap_or(0.0);
                self.x = self.sbx;
                self.y = 0.0;
                self.stack.clear();
            }
            21 => {
                if self.stack.len() >= 2 {
                    let (dx, dy) = (self.last(2)?, self.last(1)?);
                    self.moveto(dx, dy);
                } else {
                    self.moveto(0.0, 0.0);
                }
                self.stack.clear();
            }
            22 => {
                let dx = self.stack.last().copied().unwrap_or(0.0);
                self.moveto(dx, 0.0);
                self.stack.clear();
            }
            4 => {
                let dy = self.stack.last().copied().unwrap_or(0.0);
                self.moveto(0.0, dy);
                self.stack.clear();
            }
            5 => {
                let (dx, dy) = (self.last(2)?, self.last(1)?);
                self.lineto(dx, dy);
                self.stack.clear();
            }
            6 => {
                let dx = self.last(1)?;
                self.lineto(dx, 0.0);
                self.stack.clear();
            }
            7 => {
                let dy = self.last(1)?;
                self.lineto(0.0, dy);
                self.stack.clear();
            }
            8 => {
                let n = self.stack.len();
                let d: Vec<f64> = self.stack[n.saturating_sub(6)..].to_vec();
                self.curveto(&d)?;
                self.stack.clear();
            }
            30 => {
                // vhcurveto: dy1 dx2 dy2 dx3
                let d = [0.0, self.at(0)?, self.at(1)?, self.at(2)?, self.at(3)?, 0.0];
                self.curveto(&d)?;
                self.stack.clear();
            }
            31 => {
                // hvcurveto: dx1 dx2 dy2 dy3
                let d = [self.at(0)?, 0.0, self.at(1)?, self.at(2)?, 0.0, self.at(3)?];
                self.curveto(&d)?;
                self.stack.clear();
            }
            9 => {
                self.close();
                self.stack.clear();
            }
            1 | 3 => self.stack.clear(),
            10 => {
                let idx = match self.stack.pop() {
                    Some(v) => v.trunc() as i64,
                    None => -1,
                };
                if idx >= 0 && (idx as usize) < self.subrs.len() {
                    let subrs = self.subrs;
                    self.run(&subrs[idx as usize], depth + 1)?;
                }
            }
            11 => return Ok(i),
            14 => {
                self.close();
                self.done = true;
            }
            12 => {
                let b2 = *cs.get(i).ok_or(Malformed)?;
                i += 1;
                self.escape(b2)?;
            }
            _ => self.stack.clear(),
        }
        Ok(i)
    }

    fn escape(&mut self, b2: u8) -> Res<()> {
        match b2 {
            6 => {
                // seac: asb adx ady bchar achar
                if self.stack.len() < 5 || self.in_seac {
                    return Err(Malformed);
                }
                let n = self.stack.len();
                let a: Vec<f64> = self.stack[n - 5..].to_vec();
                self.seac(a[0], a[1], a[2], a[3], a[4])?;
                self.stack.clear();
            }
            7 => {
                // sbw
                self.sbx = self.stack.first().copied().unwrap_or(0.0);
                self.width = self.stack.get(2).copied().unwrap_or(0.0);
                self.x = self.sbx;
                self.y = self.stack.get(1).copied().unwrap_or(0.0);
                self.stack.clear();
            }
            12 => {
                // div
                let a = self.stack.pop().ok_or(Malformed)?;
                let b = self.stack.pop().ok_or(Malformed)?;
                self.stack.push(if a != 0.0 { b / a } else { 0.0 });
            }
            16 => self.callothersubr()?,
            17 => {
                let v = self.ps_stack.pop().unwrap_or(0.0);
                self.stack.push(v);
            }
            33 => {
                if self.stack.len() >= 2 {
                    self.x = self.last(2)?;
                    self.y = self.last(1)?;
                }
                self.stack.clear();
            }
            _ => self.stack.clear(),
        }
        Ok(())
    }

    fn callothersubr(&mut self) -> Res<()> {
        let othersubr = self.stack.pop().map(|v| v.trunc() as i64).unwrap_or(0);
        let count = self.stack.pop().map(|v| v.trunc() as i64).unwrap_or(0);
        let mut args = Vec::new();
        for _ in 0..count.max(0) {
            args.push(self.stack.pop().ok_or(Malformed)?);
        }
        args.reverse();
        match othersubr {
            1 => {
                self.flex = true;
                self.flex_pts.clear();
            }
            2 => {}
            0 => {
                self.flex = false;
                self.emit_flex();
                let end_y = args.get(2).copied().unwrap_or(self.y);
                let end_x = args.get(1).copied().unwrap_or(self.x);
                self.ps_stack = vec![end_y, end_x];
            }
            3 => self.ps_stack = vec![args.first().copied().unwrap_or(3.0)],
            _ => self.ps_stack = args,
        }
        Ok(())
    }

    fn emit_flex(&mut self) {
        let p = &self.flex_pts;
        if p.len() < 7 {
            return;
        }
        // p[0] is the reference point; p[1..3] and p[4..6] are the curves.
        let c1 = Segment::C(p[1].0, p[1].1, p[2].0, p[2].1, p[3].0, p[3].1);
        let c2 = Segment::C(p[4].0, p[4].1, p[5].0, p[5].1, p[6].0, p[6].1);
        let end = p[6];
        self.segments.push(c1);
        self.segments.push(c2);
        self.x = end.0;
        self.y = end.1;
    }

    /// An interpreter for a `seac` component glyph, sharing the work budget.
    fn seac_component(&self) -> Interpreter<'a> {
        let mut sub = Interpreter::new(self.subrs, self.charstrings);
        sub.ops = self.ops;
        sub.in_seac = true;
        sub
    }

    fn seac(&mut self, asb: f64, adx: f64, ady: f64, bchar: f64, achar: f64) -> Res<()> {
        let base = std_encoding(bchar.trunc() as i64);
        let accent = std_encoding(achar.trunc() as i64);
        if let Some(cs) = base.and_then(|n| self.charstrings.get(n)) {
            let mut sub = self.seac_component();
            sub.run(cs, 0)?;
            self.ops = sub.ops;
            self.segments.extend(sub.segments);
            if sub.width != 0.0 {
                self.width = sub.width;
            }
        }
        if let Some(cs) = accent.and_then(|n| self.charstrings.get(n)) {
            let mut sub = self.seac_component();
            sub.run(cs, 0)?;
            self.ops = sub.ops;
            let (ox, oy) = (self.sbx - asb + adx, ady);
            self.segments.extend(sub.segments.into_iter().map(|s| s.shifted(ox, oy)));
        }
        self.open = false;
        self.done = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encrypt(plain: &[u8], mut r: u32) -> Vec<u8> {
        let mut out = Vec::new();
        for &p in plain {
            let c = p ^ (r >> 8) as u8;
            r = ((c as u32 + r).wrapping_mul(C1).wrapping_add(C2)) & 0xFFFF;
            out.push(c);
        }
        out
    }

    fn num(v: i32) -> Vec<u8> {
        if (-107..=107).contains(&v) {
            vec![(v + 139) as u8]
        } else {
            let mut o = vec![255];
            o.extend(v.to_be_bytes());
            o
        }
    }

    fn font_with(glyphs: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut private = b"dup /Private 8 dict dup begin /lenIV 4 def\n/CharStrings 2 dict dup begin\n".to_vec();
        for (name, cs) in glyphs {
            let mut plain = vec![0, 0, 0, 0];
            plain.extend(cs);
            let enc = encrypt(&plain, CHARSTRING_R);
            private.extend(format!("/{name} {} RD ", enc.len()).as_bytes());
            private.extend(enc);
            private.extend(b" ND\n");
        }
        private.extend(b"end\n");
        let mut plain = vec![0, 0, 0, 0];
        plain.extend(private);
        let mut prog =
            b"%!PS-AdobeFont-1.0: Test\n/FontMatrix [0.001 0 0 0.001 0 0] readonly def\ncurrentfile eexec\n".to_vec();
        prog.extend(encrypt(&plain, EEXEC_R));
        prog
    }

    #[test]
    fn interprets_a_square() {
        let mut cs = Vec::new();
        cs.extend(num(0));
        cs.extend(num(500));
        cs.push(13); // hsbw
        cs.extend(num(100));
        cs.extend(num(0));
        cs.push(21); // rmoveto
        cs.extend(num(300));
        cs.push(6); // hlineto
        cs.extend(num(300));
        cs.push(7); // vlineto
        cs.extend(num(-300));
        cs.push(6);
        cs.push(9); // closepath
        cs.push(14); // endchar
        let font = Type1Font::new(&font_with(&[("square", cs)]));
        assert_eq!(font.units_per_em, 1000);
        assert!(font.has_glyph("square"));
        let g = font.glyph("square").unwrap();
        assert_eq!(g.advance, 500.0);
        assert_eq!(
            g.segments,
            vec![
                Segment::M(100.0, 0.0),
                Segment::L(400.0, 0.0),
                Segment::L(400.0, 300.0),
                Segment::L(100.0, 300.0),
                Segment::Z
            ]
        );
        assert_eq!(
            crate::outline::glyph_to_path_d(&g, 1.0, 0.0, 800.0),
            "M100.0 800.0L400.0 800.0L400.0 500.0L100.0 500.0Z"
        );
    }

    #[test]
    fn malformed_charstring_is_none() {
        let font = Type1Font::new(&font_with(&[("bad", vec![5])]));
        assert!(font.glyph("bad").is_none());
    }
}
