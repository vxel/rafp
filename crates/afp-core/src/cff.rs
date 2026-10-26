//! CFF (Compact Font Format) / Type 2 charstring outline interpreter.
//!
//! When a FOCA outline font's FNG program is CFF — a bare `CFF` table or
//! the `CFF ` table of an `OTTO` OpenType wrapper — its glyphs are Type 2
//! charstrings. CID-keyed fonts add an `ROS` operator, per-glyph Private
//! dicts selected through FDArray / FDSelect, and a charset of CIDs.
//!
//! References: Adobe Technical Notes #5176 (CFF) and #5177 (Type 2).

use std::collections::HashMap;

use crate::outline::{Glyph, MAX_GLYPH_OPS, MAX_GLYPH_SEGMENTS, MAX_GLYPH_STACK, Malformed, Res, Segment};

const MAX_SUBR_DEPTH: usize = 60;

/// The 391 standard strings; only the names needed to key glyphs are spelled
/// out, the rest are `sidN` placeholders.
const STD_STRINGS_NAMED: &[&str] = &[
    ".notdef",
    "space",
    "exclam",
    "quotedbl",
    "numbersign",
    "dollar",
    "percent",
    "ampersand",
    "quoteright",
    "parenleft",
    "parenright",
    "asterisk",
    "plus",
    "comma",
    "hyphen",
    "period",
    "slash",
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "colon",
    "semicolon",
    "less",
    "equal",
    "greater",
    "question",
    "at",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "bracketleft",
    "backslash",
    "bracketright",
    "asciicircum",
    "underscore",
    "quoteleft",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "braceleft",
    "bar",
    "braceright",
    "asciitilde",
    "exclamdown",
    "cent",
    "sterling",
    "fraction",
    "yen",
    "florin",
    "section",
    "currency",
    "quotesingle",
    "quotedblleft",
    "guillemotleft",
    "guilsinglleft",
    "guilsinglright",
    "fi",
    "fl",
    "endash",
    "dagger",
    "daggerdbl",
    "periodcentered",
    "paragraph",
    "bullet",
    "quotesinglbase",
    "quotedblbase",
    "quotedblright",
    "guillemotright",
    "ellipsis",
    "perthousand",
    "questiondown",
    "grave",
    "acute",
    "circumflex",
    "tilde",
    "macron",
    "breve",
    "dotaccent",
    "dieresis",
    "ring",
    "cedilla",
    "hungarumlaut",
    "ogonek",
    "caron",
    "emdash",
    "AE",
    "ordfeminine",
    "Lslash",
    "Oslash",
    "OE",
    "ordmasculine",
    "ae",
    "dotlessi",
    "lslash",
    "oslash",
    "oe",
    "germandbls",
];
const STD_STRING_COUNT: usize = 391;

fn std_string(sid: usize) -> String {
    match STD_STRINGS_NAMED.get(sid) {
        Some(n) => (*n).to_string(),
        None => format!("sid{sid}"),
    }
}

fn get(data: &[u8], pos: usize) -> Res<u8> {
    data.get(pos).copied().ok_or(Malformed)
}

fn card16(data: &[u8], pos: usize) -> Res<usize> {
    Ok(((get(data, pos)? as usize) << 8) | get(data, pos + 1)? as usize)
}

fn read_offset(data: &[u8], pos: usize, size: usize) -> Res<usize> {
    let mut v = 0usize;
    for k in 0..size {
        v = (v << 8) | get(data, pos + k)? as usize;
    }
    Ok(v)
}

/// Parse a CFF INDEX at `pos`; return (objects, position after the index).
fn parse_index(data: &[u8], pos: usize) -> Res<(Vec<Vec<u8>>, usize)> {
    let count = card16(data, pos)?;
    let mut pos = pos + 2;
    if count == 0 {
        return Ok((Vec::new(), pos));
    }
    let off_size = get(data, pos)? as usize;
    if !(1..=4).contains(&off_size) {
        return Err(Malformed);
    }
    pos += 1;
    let mut offsets = Vec::with_capacity(count + 1);
    for k in 0..=count {
        offsets.push(read_offset(data, pos + k * off_size, off_size)?);
    }
    pos += (count + 1) * off_size;
    let base = pos - 1; // offsets are 1-based from the byte before the data
    // Offsets start at 1, never decrease and stay inside the data, so the
    // objects are disjoint and their total size is bounded by the font.
    let in_bounds = offsets.first() == Some(&1)
        && offsets.windows(2).all(|w| w[0] <= w[1])
        && offsets.last().is_some_and(|&last| base.checked_add(last).is_some_and(|end| end <= data.len()));
    if !in_bounds {
        return Err(Malformed);
    }
    let objs =
        (0..count).map(|k| crate::util::slice(data, base + offsets[k], base + offsets[k + 1]).to_vec()).collect();
    Ok((objs, base + offsets[count]))
}

type Dict = HashMap<u32, Vec<f64>>;

/// Parse a CFF DICT into {operator: operands}.
fn parse_dict(data: &[u8]) -> Res<Dict> {
    let mut out = Dict::new();
    let mut operands = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let b = data[i];
        match b {
            0..=21 => {
                let mut op = b as u32;
                i += 1;
                if op == 12 {
                    op = 1200 + get(data, i)? as u32;
                    i += 1;
                }
                out.insert(op, std::mem::take(&mut operands));
            }
            28 => {
                if i + 3 > data.len() {
                    return Err(Malformed);
                }
                operands.push(i16::from_be_bytes([data[i + 1], data[i + 2]]) as f64);
                i += 3;
            }
            29 => {
                if i + 5 > data.len() {
                    return Err(Malformed);
                }
                let v = i32::from_be_bytes([data[i + 1], data[i + 2], data[i + 3], data[i + 4]]);
                operands.push(v as f64);
                i += 5;
            }
            30 => {
                let (v, next) = parse_real(data, i + 1);
                operands.push(v);
                i = next;
            }
            32..=246 => {
                operands.push(b as f64 - 139.0);
                i += 1;
            }
            247..=250 => {
                operands.push((b as f64 - 247.0) * 256.0 + get(data, i + 1)? as f64 + 108.0);
                i += 2;
            }
            251..=254 => {
                operands.push(-(b as f64 - 251.0) * 256.0 - get(data, i + 1)? as f64 - 108.0);
                i += 2;
            }
            _ => i += 1, // reserved in DICTs
        }
    }
    Ok(out)
}

/// Decode an operand-30 real number; return (value, next position).
fn parse_real(data: &[u8], mut pos: usize) -> (f64, usize) {
    let mut s = String::new();
    let parse = |s: &str| if s.is_empty() { 0.0 } else { s.parse::<f64>().unwrap_or(0.0) };
    while pos < data.len() {
        let b = data[pos];
        pos += 1;
        for nib in [b >> 4, b & 0xF] {
            match nib {
                0xF => return (parse(&s), pos),
                0x0..=0x9 => s.push((b'0' + nib) as char),
                0xA => s.push('.'),
                0xB => s.push('E'),
                0xC => s.push_str("E-"),
                0xE => s.push('-'),
                _ => {}
            }
        }
    }
    (parse(&s), pos)
}

/// Type 2 subroutine index bias from the subr count.
pub fn subr_bias(count: usize) -> i64 {
    if count < 1240 {
        107
    } else if count < 33900 {
        1131
    } else {
        32768
    }
}

/// Parse the charset; return SID (or CID) per glyph index.
fn parse_charset(data: &[u8], mut pos: usize, nglyphs: usize) -> Res<Vec<usize>> {
    let mut sids = vec![0usize];
    let fmt = get(data, pos)?;
    pos += 1;
    match fmt {
        0 => {
            for _ in 0..nglyphs.saturating_sub(1) {
                sids.push(card16(data, pos)?);
                pos += 2;
            }
        }
        1 | 2 => {
            while sids.len() < nglyphs {
                let first = card16(data, pos)?;
                pos += 2;
                let left = if fmt == 1 {
                    let l = get(data, pos)? as usize;
                    pos += 1;
                    l
                } else {
                    let l = card16(data, pos)?;
                    pos += 2;
                    l
                };
                for k in 0..=left {
                    if sids.len() >= nglyphs {
                        break;
                    }
                    sids.push(first + k);
                }
            }
        }
        _ => {}
    }
    Ok(sids)
}

/// Parse FDSelect; return the font-dict index per glyph.
fn parse_fdselect(data: &[u8], mut pos: usize, nglyphs: usize) -> Res<Vec<usize>> {
    let fmt = get(data, pos)?;
    pos += 1;
    let mut fds = vec![0usize; nglyphs];
    if fmt == 0 {
        for (g, fd) in fds.iter_mut().enumerate() {
            *fd = get(data, pos + g)? as usize;
        }
    } else if fmt == 3 {
        let nranges = card16(data, pos)?;
        pos += 2;
        let mut ranges = Vec::new();
        for _ in 0..nranges {
            let first = card16(data, pos)?;
            let fd = get(data, pos + 2)? as usize;
            ranges.push((first, fd));
            pos += 3;
        }
        ranges.push((card16(data, pos)?, 0));
        for k in 0..nranges {
            let (first, fd) = ranges[k];
            let nxt = ranges[k + 1].0;
            for slot in fds.iter_mut().take(nxt.min(nglyphs)).skip(first) {
                *slot = fd;
            }
        }
    }
    Ok(fds)
}

/// A resolved Private DICT: width defaults and local subroutines.
struct Private {
    default_width: f64,
    nominal_width: f64,
    local_subrs: Vec<Vec<u8>>,
    local_bias: i64,
}

impl Private {
    fn new(default_width: f64, nominal_width: f64, local_subrs: Vec<Vec<u8>>) -> Private {
        let local_bias = subr_bias(local_subrs.len());
        Private { default_width, nominal_width, local_subrs, local_bias }
    }
}

fn first(dict: &Dict, op: u32) -> Res<Option<f64>> {
    match dict.get(&op) {
        None => Ok(None),
        Some(v) => v.first().copied().map(Some).ok_or(Malformed),
    }
}

/// A parsed CFF font program decoded from raw CFF (or OTTO) bytes.
pub struct CffFont {
    data: Vec<u8>,
    strings: Vec<Vec<u8>>,
    gsubrs: Vec<Vec<u8>>,
    gbias: i64,
    pub is_cid: bool,
    pub units_per_em: i64,
    charstrings: Vec<Vec<u8>>,
    privates: Vec<Private>,
    fdselect: Vec<usize>,
    charset: Vec<String>,
    name_to_gid: HashMap<String, usize>,
}

impl CffFont {
    /// Parse a CFF program; `None` when its structures are malformed.
    pub fn new(data: &[u8]) -> Option<CffFont> {
        Self::parse(data).ok()
    }

    fn parse(data: &[u8]) -> Res<CffFont> {
        let data = extract_cff(data)?.to_vec();
        let hdr_size = get(&data, 2)? as usize;
        let (_names, pos) = parse_index(&data, hdr_size)?;
        let (top_dicts, pos) = parse_index(&data, pos)?;
        let (strings, pos) = parse_index(&data, pos)?;
        let (gsubrs, _) = parse_index(&data, pos)?;
        let gbias = subr_bias(gsubrs.len());

        let top = match top_dicts.first() {
            Some(d) => parse_dict(d)?,
            None => Dict::new(),
        };
        let is_cid = top.contains_key(&1230);
        let font_matrix = match top.get(&1207) {
            Some(fm) if fm.len() == 6 => [fm[0], fm[1], fm[2], fm[3], fm[4], fm[5]],
            _ => [0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
        };
        let units_per_em = crate::outline::units_per_em(font_matrix[0]);
        let cs_off = first(&top, 17)?.map(|v| v as i64 as usize).unwrap_or(0);
        let (charstrings, _) = parse_index(&data, cs_off)?;
        let nglyphs = charstrings.len();

        let mut font = CffFont {
            data,
            strings,
            gsubrs,
            gbias,
            is_cid,
            units_per_em,
            charstrings,
            privates: Vec::new(),
            fdselect: Vec::new(),
            charset: Vec::new(),
            name_to_gid: HashMap::new(),
        };
        if is_cid {
            let (privates, fdselect) = font.parse_cid_privates(&top, nglyphs)?;
            font.privates = privates;
            font.fdselect = fdselect;
        } else {
            font.privates = vec![font.parse_private(top.get(&18))?];
            font.fdselect = vec![0; nglyphs];
        }
        let charset_off = first(&top, 15)?.map(|v| v as i64 as usize).unwrap_or(0);
        font.charset = font.build_charset(charset_off, nglyphs)?;
        for (gid, name) in font.charset.iter().enumerate() {
            font.name_to_gid.insert(name.clone(), gid);
        }
        Ok(font)
    }

    fn build_charset(&self, off: usize, nglyphs: usize) -> Res<Vec<String>> {
        let sids = if off <= 2 { (0..nglyphs).collect() } else { parse_charset(&self.data, off, nglyphs)? };
        Ok(if self.is_cid {
            sids.iter().map(|sid| format!("cid{sid:05}")).collect()
        } else {
            sids.iter().map(|&sid| self.sid_name(sid)).collect()
        })
    }

    fn sid_name(&self, sid: usize) -> String {
        if sid < STD_STRING_COUNT {
            return std_string(sid);
        }
        match self.strings.get(sid - STD_STRING_COUNT) {
            Some(s) => s.iter().map(|&b| b as char).collect(),
            None => format!("sid{sid}"),
        }
    }

    fn parse_private(&self, spec: Option<&Vec<f64>>) -> Res<Private> {
        let Some(spec) = spec.filter(|s| s.len() >= 2) else {
            return Ok(Private::new(0.0, 0.0, Vec::new()));
        };
        let (size, offset) = (spec[0] as i64 as usize, spec[1] as i64 as usize);
        let pdict = parse_dict(crate::util::slice(&self.data, offset, offset.saturating_add(size)))?;
        let default_w = first(&pdict, 20)?.unwrap_or(0.0);
        let nominal_w = first(&pdict, 21)?.unwrap_or(0.0);
        let mut local_subrs = Vec::new();
        if let Some(rel) = first(&pdict, 19)? {
            // Subrs offset is relative to the Private DICT start.
            let subr_off = (offset as i64 + rel as i64).max(0) as usize;
            local_subrs = parse_index(&self.data, subr_off)?.0;
        }
        Ok(Private::new(default_w, nominal_w, local_subrs))
    }

    fn parse_cid_privates(&self, top: &Dict, nglyphs: usize) -> Res<(Vec<Private>, Vec<usize>)> {
        let fdarray_off = first(top, 1236)?.map(|v| v as i64 as usize).unwrap_or(0);
        let fdselect_off = first(top, 1237)?.map(|v| v as i64 as usize).unwrap_or(0);
        let (fd_dicts, _) = parse_index(&self.data, fdarray_off)?;
        let mut privates = Vec::new();
        for fd in &fd_dicts {
            let d = parse_dict(fd)?;
            privates.push(self.parse_private(d.get(&18))?);
        }
        if privates.is_empty() {
            privates.push(Private::new(0.0, 0.0, Vec::new()));
        }
        let fdselect =
            if fdselect_off != 0 { parse_fdselect(&self.data, fdselect_off, nglyphs)? } else { vec![0; nglyphs] };
        Ok((privates, fdselect))
    }

    pub fn glyph_names(&self) -> Vec<String> {
        self.charset.clone()
    }

    pub fn has_glyph(&self, name: &str) -> bool {
        self.name_to_gid.contains_key(name)
    }

    pub fn glyph(&self, name: &str) -> Option<Glyph> {
        self.glyph_by_gid(*self.name_to_gid.get(name)?)
    }

    /// Interpret glyph `gid`'s Type 2 charstring into an outline.
    pub fn glyph_by_gid(&self, gid: usize) -> Option<Glyph> {
        let cs = self.charstrings.get(gid)?;
        let priv_ = self.privates.get(*self.fdselect.get(gid)?)?;
        let mut interp = Interpreter::new(&self.gsubrs, self.gbias, priv_);
        let ok = interp.run(cs, 0).map(|_| interp.finish());
        match ok {
            Ok(()) => Some(Glyph { advance: interp.width, segments: interp.segments }),
            Err(Malformed) => {
                log::warn!("Type 2 charstring gid {gid} failed");
                None
            }
        }
    }
}

/// Raw CFF bytes, unwrapping an OTTO OpenType container.
fn extract_cff(data: &[u8]) -> Res<&[u8]> {
    if data.starts_with(b"OTTO") {
        let num_tables = card16(data, 4)?;
        let mut pos = 12;
        for _ in 0..num_tables {
            if pos + 16 > data.len() {
                return Err(Malformed);
            }
            let tag = &data[pos..pos + 4];
            let offset = u32::from_be_bytes(data[pos + 8..pos + 12].try_into().unwrap()) as usize;
            let length = u32::from_be_bytes(data[pos + 12..pos + 16].try_into().unwrap()) as usize;
            if tag == b"CFF " {
                return Ok(crate::util::slice(data, offset, offset.saturating_add(length)));
            }
            pos += 16;
        }
        return Err(Malformed);
    }
    Ok(data)
}

struct Interpreter<'a> {
    gsubrs: &'a [Vec<u8>],
    gbias: i64,
    subrs: &'a [Vec<u8>],
    lbias: i64,
    nominal_width: f64,
    width: f64,
    stack: Vec<f64>,
    segments: Vec<Segment>,
    x: f64,
    y: f64,
    nstems: usize,
    have_width: bool,
    open: bool,
    done: bool,
    /// Operations run so far (see [`MAX_GLYPH_OPS`]).
    ops: usize,
}

impl<'a> Interpreter<'a> {
    fn new(gsubrs: &'a [Vec<u8>], gbias: i64, priv_: &'a Private) -> Self {
        Interpreter {
            gsubrs,
            gbias,
            subrs: &priv_.local_subrs,
            lbias: priv_.local_bias,
            nominal_width: priv_.nominal_width,
            width: priv_.default_width,
            stack: Vec::new(),
            segments: Vec::new(),
            x: 0.0,
            y: 0.0,
            nstems: 0,
            have_width: false,
            open: false,
            done: false,
            ops: 0,
        }
    }

    fn moveto(&mut self, dx: f64, dy: f64) {
        self.x += dx;
        self.y += dy;
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

    fn curveto(&mut self, dx1: f64, dy1: f64, dx2: f64, dy2: f64, dx3: f64, dy3: f64) {
        let (x1, y1) = (self.x + dx1, self.y + dy1);
        let (x2, y2) = (x1 + dx2, y1 + dy2);
        let (x3, y3) = (x2 + dx3, y2 + dy3);
        self.segments.push(Segment::C(x1, y1, x2, y2, x3, y3));
        self.x = x3;
        self.y = y3;
    }

    fn curve6(&mut self, s: &[f64]) {
        self.curveto(s[0], s[1], s[2], s[3], s[4], s[5]);
    }

    fn finish(&mut self) {
        if self.open {
            self.segments.push(Segment::Z);
            self.open = false;
        }
    }

    /// Consume the optional leading width operand. `even` is the expected
    /// parity of the operand count *without* a width.
    fn take_width(&mut self, even: bool) {
        if self.have_width {
            return;
        }
        let has_extra = if even { self.stack.len() % 2 == 1 } else { self.stack.len() > 1 };
        if has_extra {
            self.width = self.nominal_width + self.stack.remove(0);
        }
        self.have_width = true;
    }

    fn run(&mut self, cs: &[u8], depth: usize) -> Res<()> {
        if depth > MAX_SUBR_DEPTH {
            return Err(Malformed);
        }
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
            if b >= 32 || b == 28 {
                i = self.operand(b, cs, i)?;
                continue;
            }
            i = self.operator(b, cs, i, depth)?;
            if b == 11 {
                return Ok(());
            }
        }
        Ok(())
    }

    fn operand(&mut self, b: u8, cs: &[u8], mut i: usize) -> Res<usize> {
        let byte = |i: usize| cs.get(i).copied().ok_or(Malformed);
        let v = match b {
            28 => {
                if i + 2 > cs.len() {
                    return Err(Malformed);
                }
                let v = i16::from_be_bytes([cs[i], cs[i + 1]]) as f64;
                i += 2;
                v
            }
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
                // 255: 16.16 fixed point
                if i + 4 > cs.len() {
                    return Err(Malformed);
                }
                let v = i32::from_be_bytes([cs[i], cs[i + 1], cs[i + 2], cs[i + 3]]) as f64 / 65536.0;
                i += 4;
                v
            }
        };
        self.stack.push(v);
        Ok(i)
    }

    fn operator(&mut self, b: u8, cs: &[u8], mut i: usize, depth: usize) -> Res<usize> {
        let s = |st: &Vec<f64>, k: usize| st.get(k).copied().unwrap_or(0.0);
        match b {
            1 | 3 | 18 | 23 => {
                self.take_width(true);
                self.nstems += self.stack.len() / 2;
                self.stack.clear();
            }
            19 | 20 => {
                // hintmask / cntrmask: skip the mask bytes
                self.take_width(true);
                self.nstems += self.stack.len() / 2;
                self.stack.clear();
                return Ok(i + self.nstems.div_ceil(8));
            }
            21 => {
                self.take_width(true);
                let (dx, dy) = (s(&self.stack, 0), s(&self.stack, 1));
                self.moveto(dx, dy);
                self.stack.clear();
            }
            22 => {
                self.take_width(false);
                let dx = s(&self.stack, 0);
                self.moveto(dx, 0.0);
                self.stack.clear();
            }
            4 => {
                self.take_width(false);
                let dy = s(&self.stack, 0);
                self.moveto(0.0, dy);
                self.stack.clear();
            }
            5 => {
                let st = std::mem::take(&mut self.stack);
                let mut k = 0;
                while k + 1 < st.len() {
                    self.lineto(st[k], st[k + 1]);
                    k += 2;
                }
            }
            6 | 7 => {
                let st = std::mem::take(&mut self.stack);
                let mut horizontal = b == 6;
                for v in st {
                    if horizontal {
                        self.lineto(v, 0.0);
                    } else {
                        self.lineto(0.0, v);
                    }
                    horizontal = !horizontal;
                }
            }
            8 => {
                let st = std::mem::take(&mut self.stack);
                let mut k = 0;
                while k + 5 < st.len() {
                    self.curve6(&st[k..k + 6]);
                    k += 6;
                }
            }
            24 => {
                // rcurveline
                let st = std::mem::take(&mut self.stack);
                let mut k = 0usize;
                while (k + 6) as i64 <= st.len() as i64 - 2 {
                    self.curve6(&st[k..k + 6]);
                    k += 6;
                }
                if k + 2 <= st.len() {
                    self.lineto(st[k], st[k + 1]);
                }
            }
            25 => {
                // rlinecurve
                let st = std::mem::take(&mut self.stack);
                let mut k = 0usize;
                while (k + 2) as i64 <= st.len() as i64 - 6 {
                    self.lineto(st[k], st[k + 1]);
                    k += 2;
                }
                if k + 6 <= st.len() {
                    self.curve6(&st[k..k + 6]);
                }
            }
            26 => {
                // vvcurveto
                let st = std::mem::take(&mut self.stack);
                let (mut i, mut dx1) = (0, 0.0);
                if st.len() % 4 == 1 {
                    dx1 = st[0];
                    i = 1;
                }
                while i + 4 <= st.len() {
                    self.curveto(dx1, st[i], st[i + 1], st[i + 2], 0.0, st[i + 3]);
                    dx1 = 0.0;
                    i += 4;
                }
            }
            27 => {
                // hhcurveto
                let st = std::mem::take(&mut self.stack);
                let (mut i, mut dy1) = (0, 0.0);
                if st.len() % 4 == 1 {
                    dy1 = st[0];
                    i = 1;
                }
                while i + 4 <= st.len() {
                    self.curveto(st[i], dy1, st[i + 1], st[i + 2], st[i + 3], 0.0);
                    dy1 = 0.0;
                    i += 4;
                }
            }
            30 | 31 => {
                // vhcurveto / hvcurveto
                let st = std::mem::take(&mut self.stack);
                let n = st.len();
                let mut i = 0;
                let mut vertical = b == 30;
                while i + 4 <= n {
                    let last = i + 8 > n;
                    let df = if last && n - i == 5 { st[i + 4] } else { 0.0 };
                    if vertical {
                        self.curveto(0.0, st[i], st[i + 1], st[i + 2], st[i + 3], df);
                    } else {
                        self.curveto(st[i], 0.0, st[i + 1], st[i + 2], df, st[i + 3]);
                    }
                    vertical = !vertical;
                    i += 4;
                }
            }
            10 | 29 => {
                let (subrs, bias) = if b == 10 { (self.subrs, self.lbias) } else { (self.gsubrs, self.gbias) };
                let idx = match self.stack.pop() {
                    Some(v) => v.trunc() as i64 + bias,
                    None => -1,
                };
                if idx >= 0 && (idx as usize) < subrs.len() {
                    self.run(&subrs[idx as usize], depth + 1)?;
                }
                return Ok(i);
            }
            11 => return Ok(i),
            14 => {
                self.take_width(true);
                self.finish();
                self.done = true;
            }
            12 => {
                let b2 = *cs.get(i).ok_or(Malformed)?;
                i += 1;
                self.escape(b2);
                return Ok(i);
            }
            _ => self.stack.clear(),
        }
        Ok(i)
    }

    fn escape(&mut self, b2: u8) {
        let s = std::mem::take(&mut self.stack);
        match b2 {
            35 if s.len() >= 12 => {
                // flex
                self.curve6(&s[0..6]);
                self.curve6(&s[6..12]);
            }
            34 if s.len() >= 7 => {
                // hflex: dx1 dx2 dy2 dx3 dx4 dx5 dx6
                self.curveto(s[0], 0.0, s[1], s[2], s[3], 0.0);
                self.curveto(s[4], 0.0, s[5], -s[2], s[6], 0.0);
            }
            36 if s.len() >= 9 => {
                // hflex1: dx1 dy1 dx2 dy2 dx3 dx4 dx5 dy5 dx6
                self.curveto(s[0], s[1], s[2], s[3], s[4], 0.0);
                let dy = -(s[1] + s[3] + s[7]);
                self.curveto(s[5], 0.0, s[6], s[7], s[8], dy);
            }
            37 if s.len() >= 11 => {
                // flex1
                let dx = s[0] + s[2] + s[4] + s[6] + s[8];
                let dy = s[1] + s[3] + s[5] + s[7] + s[9];
                self.curveto(s[0], s[1], s[2], s[3], s[4], s[5]);
                if dx.abs() > dy.abs() {
                    self.curveto(s[6], s[7], s[8], s[9], s[10], -dy);
                } else {
                    self.curveto(s[6], s[7], s[8], s[9], -dx, s[10]);
                }
            }
            _ => {} // arithmetic / unsupported: discard operands
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bias() {
        assert_eq!(subr_bias(0), 107);
        assert_eq!(subr_bias(1239), 107);
        assert_eq!(subr_bias(1240), 1131);
        assert_eq!(subr_bias(33900), 32768);
    }

    #[test]
    fn real_operand() {
        // -2.25 → nibbles e 2 a 2 5 f
        let (v, next) = parse_real(&[0xE2, 0xA2, 0x5F], 0);
        assert_eq!(v, -2.25);
        assert_eq!(next, 3);
    }

    #[test]
    fn fdselect_formats() {
        assert_eq!(parse_fdselect(&[0, 1, 0, 1], 0, 3).unwrap(), vec![1, 0, 1]);
        // format 3: 2 ranges (0→fd1, 2→fd0), sentinel 4
        let data = [3, 0, 2, 0, 0, 1, 0, 2, 0, 0, 4];
        assert_eq!(parse_fdselect(&data, 0, 4).unwrap(), vec![1, 1, 0, 0]);
    }

    #[test]
    fn charset_formats() {
        assert_eq!(parse_charset(&[0, 0, 5, 0, 9], 0, 3).unwrap(), vec![0, 5, 9]);
        assert_eq!(parse_charset(&[1, 0, 10, 2], 0, 4).unwrap(), vec![0, 10, 11, 12]);
    }
}
