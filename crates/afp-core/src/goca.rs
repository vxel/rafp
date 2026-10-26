//! GOCA (Graphics Object Content Architecture) drawing-order decoder.
//!
//! GAD fields carry drawing order streams: Begin Segment commands (0x70)
//! each followed by the drawing orders of that segment. Orders come in four
//! formats (fixed 1-byte, fixed 2-byte, long, extended 0xFE).
//!
//! GPS Y points up, SVG Y down: every GPS point maps to
//! `(gx − xlwind, ytwind − gy)`.
//!
//! Differences from readAFP (deliberate fixes): coordinates keep their full
//! precision (readAFP formatted them with `.3g`, so 1234 became `1.23e+03`),
//! and GSECOL accepts the standard `X'00nn'` color values as well as
//! `X'FFnn'`.
//!
//! Reference: GOCA Reference, AFPC-0008-03.

use std::f64::consts::PI;

use crate::model::{DEFAULT_COLOR, oca_named_color};
use crate::util::{quote_attr, s16_at, u16_at, u32_at};

/// GOCA color: the OCA named colors, also as X'FFnn', plus X'0007' white;
/// X'FF07' (device default) and X'FF00' (drawing default) draw black.
fn oca_color(value: u16) -> &'static str {
    if value == 0x0007 {
        return "#ffffff";
    }
    let named = if value & 0xFF00 == 0xFF00 { value & 0x00FF } else { value };
    oca_named_color(named).unwrap_or(DEFAULT_COLOR)
}

/// GSLT line-type code → dash pattern (multiples of stroke-width); `None`
/// means invisible.
fn dash_array(line_type: u8) -> Option<&'static [f64]> {
    Some(match line_type {
        0x01 => &[1.0, 3.0],
        0x02 => &[6.0, 3.0],
        0x03 => &[6.0, 3.0, 1.0, 3.0],
        0x04 => &[1.0, 1.0],
        0x05 => &[12.0, 3.0],
        0x06 => &[6.0, 3.0, 1.0, 1.0, 1.0, 3.0],
        0x08 => return None,
        _ => &[],
    })
}

/// A GOCA graphic rendered to SVG; coordinates are in GPS units.
#[derive(Debug, Clone, PartialEq)]
pub struct GocaGraphic {
    /// SVG fragment with GPS-unit coordinates (Y flipped).
    pub svg: String,
    pub gps_w: i64,
    pub gps_h: i64,
}

/// Format a coordinate compactly without losing precision.
pub fn num(v: f64) -> String {
    let v = if v.abs() < 5e-4 { 0.0 } else { v };
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Format {
    Fixed1,
    Fixed2,
    Long,
    Extended,
}

fn order_format(code: u8) -> Format {
    if code == 0x00 {
        Format::Fixed1
    } else if code == 0xFE {
        Format::Extended
    } else if code == 0x71 || ((code >> 4) < 8 && (code & 0x0F) >= 8) {
        Format::Fixed2
    } else {
        Format::Long
    }
}

/// `(order_code, params)` from concatenated GAD data; unchained segments are
/// skipped. Extended orders yield `0xFExx`.
pub fn parse_orders(data: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    let n = data.len();
    let mut pos = 0usize;
    while pos < n {
        if data[pos] != 0x70 {
            pos += 1;
            continue;
        }
        // Begin Segment: code(1) + length(1=12) + NAME(4) FLAG1 FLAG2 SEGL(2) P/SNAME(4)
        if pos + 14 > n {
            break;
        }
        let unchained = data[pos + 7] & 0x80 != 0;
        let segl = u16_at(data, pos + 8) as usize;
        pos += 14;
        let seg_end = (pos + segl).min(n);
        if unchained {
            pos = seg_end;
            continue;
        }
        while pos < seg_end {
            let oc = data[pos];
            match order_format(oc) {
                Format::Fixed1 => {
                    out.push((oc as u16, &data[pos..pos]));
                    pos += 1;
                }
                Format::Fixed2 => {
                    if pos + 2 > seg_end {
                        pos = seg_end;
                        break;
                    }
                    out.push((oc as u16, &data[pos + 1..pos + 2]));
                    pos += 2;
                }
                Format::Long => {
                    if pos + 2 > seg_end {
                        pos = seg_end;
                        break;
                    }
                    let end = pos + 2 + data[pos + 1] as usize;
                    if end > seg_end {
                        pos = seg_end;
                        break;
                    }
                    out.push((oc as u16, &data[pos + 2..end]));
                    pos = end;
                }
                Format::Extended => {
                    if pos + 4 > seg_end {
                        pos = seg_end;
                        break;
                    }
                    let qualifier = data[pos + 1];
                    let ext_end = pos + 4 + u16_at(data, pos + 2) as usize;
                    if ext_end > seg_end {
                        pos = seg_end;
                        break;
                    }
                    out.push((0xFE00 | qualifier as u16, &data[pos + 4..ext_end]));
                    pos = ext_end;
                }
            }
        }
    }
    out
}

/// GDD Window Specification: `(gps_upi, xlwind, xrwind, ybwind, ytwind)`.
pub fn parse_gdd(data: &[u8]) -> Option<(f64, i64, i64, i64, i64)> {
    let mut pos = 0usize;
    while pos + 2 <= data.len() {
        let (code, length) = (data[pos], data[pos + 1] as usize);
        let end = pos + 2 + length;
        if end > data.len() {
            break;
        }
        if code == 0xF6 && length >= 18 {
            let p = &data[pos + 2..];
            let xresol = u16_at(p, 4) as f64;
            let gps_upi = if p[3] == 0 { xresol / 10.0 } else { xresol * 0.254 };
            return Some((
                gps_upi,
                s16_at(p, 10) as i64,
                s16_at(p, 12) as i64,
                s16_at(p, 14) as i64,
                s16_at(p, 16) as i64,
            ));
        }
        pos = end;
    }
    None
}

fn gspcol_to_css(p: &[u8]) -> String {
    if p.len() < 6 {
        return DEFAULT_COLOR.into();
    }
    match p[1] {
        0x01 if p.len() >= 9 => format!("#{:02x}{:02x}{:02x}", p[6], p[7], p[8]),
        0x04 if p.len() >= 10 => {
            let f = |v: u8| v as f64 / 255.0;
            let (c, m, y, k) = (f(p[6]), f(p[7]), f(p[8]), f(p[9]));
            let ch = |v: f64| (255.0 * v).trunc() as u8;
            format!(
                "#{:02x}{:02x}{:02x}",
                ch((1.0 - c) * (1.0 - k)),
                ch((1.0 - m) * (1.0 - k)),
                ch((1.0 - y) * (1.0 - k))
            )
        }
        _ => DEFAULT_COLOR.into(),
    }
}

struct State {
    cx: f64,
    cy: f64,
    color: String,
    stroke_width: f64,
    line_type: u8,
    line_cap: &'static str,
    line_join: &'static str,
    pattern_sym: u8,
    in_area: bool,
    area_boundary: bool,
    area_path: Vec<String>,
    arc_p: f64,
    arc_q: f64,
    arc_r: f64,
    arc_s: f64,
}

impl Default for State {
    fn default() -> Self {
        State {
            cx: 0.0,
            cy: 0.0,
            color: DEFAULT_COLOR.into(),
            stroke_width: 1.0,
            line_type: 7,
            line_cap: "round",
            line_join: "round",
            pattern_sym: 0x10,
            in_area: false,
            area_boundary: true,
            area_path: Vec::new(),
            // GSAP defaults per spec: P=Q=1, R=S=0 (unit circle).
            arc_p: 1.0,
            arc_q: 1.0,
            arc_r: 0.0,
            arc_s: 0.0,
        }
    }
}

struct Ctx {
    xlwind: f64,
    ytwind: f64,
    st: State,
    out: Vec<String>,
}

fn pair(p: &[u8], off: usize) -> (f64, f64) {
    (s16_at(p, off) as f64, s16_at(p, off + 2) as f64)
}

fn s8(b: u8) -> f64 {
    b as i8 as f64
}

impl Ctx {
    fn sx(&self, gx: f64) -> f64 {
        gx - self.xlwind
    }

    fn sy(&self, gy: f64) -> f64 {
        self.ytwind - gy
    }

    fn coord(&self, gx: f64, gy: f64) -> String {
        format!("{},{}", num(self.sx(gx)), num(self.sy(gy)))
    }

    fn stroke_attrs(&self) -> String {
        let s = &self.st;
        let Some(dash) = dash_array(s.line_type) else {
            return "stroke=\"none\"".into();
        };
        let mut parts = vec![
            format!("stroke={}", quote_attr(&s.color)),
            format!("stroke-width=\"{}\"", num(s.stroke_width)),
            format!("stroke-linecap=\"{}\"", s.line_cap),
            format!("stroke-linejoin=\"{}\"", s.line_join),
        ];
        if !dash.is_empty() {
            let sw = if s.stroke_width != 0.0 { s.stroke_width } else { 1.0 };
            let vals: Vec<String> = dash.iter().map(|v| num(v * sw)).collect();
            parts.push(format!("stroke-dasharray=\"{}\"", vals.join(" ")));
        }
        parts.join(" ")
    }

    fn fill_style(&self) -> String {
        match self.st.pattern_sym {
            0x00 | 0x0F | 0x40 => "none".into(),
            0x10 => self.st.color.clone(),
            // Built-in hatch patterns are approximated as a solid fill.
            0x01..=0x08 => self.st.color.clone(),
            _ => "none".into(),
        }
    }

    fn emit_polyline(&mut self, pts: Vec<String>) {
        if pts.is_empty() {
            return;
        }
        if self.st.in_area {
            let first = if self.st.area_path.is_empty() { "M" } else { "L" };
            self.st.area_path.push(format!("{first} {}", pts[0]));
            for p in &pts[1..] {
                self.st.area_path.push(format!("L {p}"));
            }
            return;
        }
        let attrs = self.stroke_attrs();
        self.out.push(format!("<polyline points=\"{}\" fill=\"none\" {attrs}/>", pts.join(" ")));
    }

    fn gline(&mut self, p: &[u8], at_given: bool) {
        let mut pts = Vec::new();
        let mut i = 0;
        if at_given {
            if p.len() < 4 {
                return;
            }
            let (x0, y0) = pair(p, 0);
            self.st.cx = x0;
            self.st.cy = y0;
            pts.push(self.coord(x0, y0));
            i = 4;
        } else {
            pts.push(self.coord(self.st.cx, self.st.cy));
        }
        while i + 3 < p.len() {
            let (gx, gy) = pair(p, i);
            pts.push(self.coord(gx, gy));
            self.st.cx = gx;
            self.st.cy = gy;
            i += 4;
        }
        self.emit_polyline(pts);
    }

    fn grline(&mut self, p: &[u8], at_given: bool) {
        let mut pts = Vec::new();
        let mut i = 0;
        if at_given {
            if p.len() < 4 {
                return;
            }
            let (x0, y0) = pair(p, 0);
            self.st.cx = x0;
            self.st.cy = y0;
            pts.push(self.coord(x0, y0));
            i = 4;
        } else {
            pts.push(self.coord(self.st.cx, self.st.cy));
        }
        while i + 1 < p.len() {
            self.st.cx += s8(p[i]);
            self.st.cy += s8(p[i + 1]);
            pts.push(self.coord(self.st.cx, self.st.cy));
            i += 2;
        }
        self.emit_polyline(pts);
    }

    fn gbox(&mut self, p: &[u8], at_given: bool) {
        let ((x0, y0), (x1, y1)) = if at_given {
            if p.len() < 10 {
                return;
            }
            (pair(p, 2), pair(p, 6))
        } else {
            if p.len() < 6 {
                return;
            }
            ((self.st.cx, self.st.cy), pair(p, 2))
        };
        self.st.cx = x0;
        self.st.cy = y0;
        if self.st.in_area {
            let path = format!(
                "M {} L {} L {} L {} Z",
                self.coord(x0, y0),
                self.coord(x1, y0),
                self.coord(x1, y1),
                self.coord(x0, y1)
            );
            self.st.area_path.push(path);
            return;
        }
        let (rx, ry) = (self.sx(x0.min(x1)), self.sy(y0.max(y1)));
        let (w, h) = ((x1 - x0).abs(), (y1 - y0).abs());
        let fill = self.fill_style();
        let stroke = if self.st.line_type != 0x08 { self.stroke_attrs() } else { "stroke=\"none\"".into() };
        self.out.push(format!(
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill={} {stroke}/>",
            num(rx),
            num(ry),
            num(w),
            num(h),
            quote_attr(&fill)
        ));
    }

    fn arc_matrix(&self, m: f64) -> (f64, f64, f64, f64) {
        (self.st.arc_p * m, self.st.arc_q * m, self.st.arc_r * m, self.st.arc_s * m)
    }

    fn gfarc(&mut self, p: &[u8], at_given: bool) {
        let (gx, gy, mh, mfr) = if at_given {
            if p.len() < 6 {
                return;
            }
            let (x, y) = pair(p, 0);
            (x, y, p[4], p[5])
        } else {
            if p.len() < 2 {
                return;
            }
            (self.st.cx, self.st.cy, p[0], p[1])
        };
        self.st.cx = gx;
        self.st.cy = gy;
        let mult = mh as f64 + mfr as f64 / 256.0;
        if mult < 1e-6 {
            return;
        }
        // GSAP transform: X' = P·X + R·Y, Y' = S·X + Q·Y; the columns (P, S)
        // and (R, Q) are the ellipse semi-axes.
        let (pp, q, r, s) = self.arc_matrix(mult);
        let rx = (pp * pp + s * s).sqrt();
        let ry = (r * r + q * q).sqrt();
        let (scx, scy) = (self.sx(gx), self.sy(gy));
        if self.st.in_area {
            self.st.area_path.push(format!(
                "M {},{} A {},{} 0 1 0 {},{} A {},{} 0 1 0 {},{} Z",
                num(scx - rx),
                num(scy),
                num(rx),
                num(ry),
                num(scx + rx),
                num(scy),
                num(rx),
                num(ry),
                num(scx - rx),
                num(scy)
            ));
            return;
        }
        if rx < 1e-6 || ry < 1e-6 {
            return;
        }
        let angle_svg = -s.atan2(pp).to_degrees();
        let fill = quote_attr(&self.fill_style());
        let stroke = self.stroke_attrs();
        let markup = if (rx - ry).abs() < 0.01 && angle_svg.abs() < 0.5 {
            format!("<circle cx=\"{}\" cy=\"{}\" r=\"{}\" fill={fill} {stroke}/>", num(scx), num(scy), num(rx))
        } else if angle_svg.abs() < 0.5 {
            format!(
                "<ellipse cx=\"{}\" cy=\"{}\" rx=\"{}\" ry=\"{}\" fill={fill} {stroke}/>",
                num(scx),
                num(scy),
                num(rx),
                num(ry)
            )
        } else {
            format!(
                "<ellipse cx=\"{}\" cy=\"{}\" rx=\"{}\" ry=\"{}\" fill={fill} {stroke} transform=\"rotate({} {} {})\"/>",
                num(scx),
                num(scy),
                num(rx),
                num(ry),
                num(angle_svg),
                num(scx),
                num(scy)
            )
        };
        self.out.push(markup);
    }

    fn gparc(&mut self, p: &[u8], at_given: bool) {
        let ((x0, y0), (gcx, gcy), mh, mfr, start_raw, sweep_raw) = if at_given {
            if p.len() < 18 {
                return;
            }
            (pair(p, 0), pair(p, 4), p[8], p[9], u32_at(p, 10), u32_at(p, 14))
        } else {
            if p.len() < 14 {
                return;
            }
            ((self.st.cx, self.st.cy), pair(p, 0), p[4], p[5], u32_at(p, 6), u32_at(p, 10))
        };
        let mult = mh as f64 + mfr as f64 / 256.0;
        if mult < 1e-6 {
            return;
        }
        let (pp, q, r, s) = self.arc_matrix(mult);
        let rx = (pp * pp + s * s).sqrt();
        let ry = (r * r + q * q).sqrt();
        if rx < 1e-6 || ry < 1e-6 {
            return;
        }
        let start_deg = (start_raw as f64 / 65536.0) % 360.0;
        let sweep_deg = (sweep_raw as f64 / 65536.0) % 360.0;
        if sweep_deg < 1e-6 {
            return;
        }
        let angle_svg = -s.atan2(pp).to_degrees();
        // A unit-circle parameter angle maps to centre + M·(cos a, sin a).
        let endpoint = |a_deg: f64| {
            let a = a_deg * PI / 180.0;
            let (ca, sa) = (a.cos(), a.sin());
            (gcx + pp * ca + r * sa, gcy + s * ca + q * sa)
        };
        let (gxs, gys) = endpoint(start_deg);
        let (gxe, gye) = endpoint(start_deg + sweep_deg);
        let large_arc = if sweep_deg.abs() > 180.0 { 1 } else { 0 };
        // Determinant P·Q − R·S > 0 sweeps CCW; the Y-flip turns that into
        // SVG flag 0. A reflecting matrix reverses it once more.
        let sweep_flag = if pp * q - r * s >= 0.0 { 0 } else { 1 };
        let d = format!(
            "M {} L {} A {},{} {} {large_arc} {sweep_flag} {}",
            self.coord(x0, y0),
            self.coord(gxs, gys),
            num(rx),
            num(ry),
            num(angle_svg),
            self.coord(gxe, gye)
        );
        // The current position moves to the endpoint of the arc.
        self.st.cx = gxe;
        self.st.cy = gye;
        let attrs = self.stroke_attrs();
        self.out.push(format!("<path d={} fill=\"none\" {attrs}/>", quote_attr(&d)));
    }

    fn gbez(&mut self, p: &[u8], at_given: bool) {
        let (start, first) = if at_given {
            if p.len() < 16 {
                return;
            }
            (pair(p, 0), 4)
        } else {
            if p.len() < 12 {
                return;
            }
            ((self.st.cx, self.st.cy), 0)
        };
        let mut pairs = Vec::new();
        let mut i = first;
        while i + 3 < p.len() {
            pairs.push(pair(p, i));
            i += 4;
        }
        if pairs.len() < 3 {
            return;
        }
        self.st.cx = start.0;
        self.st.cy = start.1;
        let mut d = vec![format!("M {}", self.coord(start.0, start.1))];
        let mut k = 0;
        while k + 2 < pairs.len() {
            let (c1, c2, end) = (pairs[k], pairs[k + 1], pairs[k + 2]);
            d.push(format!("C {} {} {}", self.coord(c1.0, c1.1), self.coord(c2.0, c2.1), self.coord(end.0, end.1)));
            self.st.cx = end.0;
            self.st.cy = end.1;
            k += 3;
        }
        let attrs = self.stroke_attrs();
        self.out.push(format!("<path d={} fill=\"none\" {attrs}/>", quote_attr(&d.join(" "))));
    }

    fn gear(&mut self) {
        if !self.st.in_area {
            return;
        }
        self.st.in_area = false;
        if self.st.area_path.is_empty() {
            return;
        }
        let d = self.st.area_path.join(" ") + " Z";
        let fill = self.fill_style();
        let stroke = if self.st.area_boundary { self.stroke_attrs() } else { "stroke=\"none\"".into() };
        self.out.push(format!("<path d={} fill={} {stroke}/>", quote_attr(&d), quote_attr(&fill)));
        self.st.area_path.clear();
    }

    fn dispatch(&mut self, code: u16, p: &[u8]) {
        match code {
            0x08 => {} // GSPS Set Pattern Set
            0x0A => {
                if let Some(&b) = p.first() {
                    self.st.color = oca_color(0xFF00 | b as u16).into();
                }
            }
            0x11 => {
                if p.len() >= 2 {
                    let m = p[0] as f64 + p[1] as f64 / 256.0;
                    if m > 0.0 {
                        self.st.stroke_width = m;
                    }
                }
            }
            0x18 => {
                if let Some(&b) = p.first() {
                    self.st.line_type = b;
                }
            }
            0x19 => {
                if let Some(&b) = p.first() {
                    self.st.stroke_width = b.max(1) as f64;
                }
            }
            0x1A => {
                if let Some(&b) = p.first() {
                    self.st.line_cap = match b {
                        0x01 => "butt",
                        0x02 => "square",
                        _ => "round",
                    };
                }
            }
            0x1B => {
                if let Some(&b) = p.first() {
                    self.st.line_join = match b {
                        0x01 => "bevel",
                        0x03 => "miter",
                        _ => "round",
                    };
                }
            }
            0x21 => {
                if p.len() >= 4 {
                    (self.st.cx, self.st.cy) = pair(p, 0);
                }
            }
            0x22 => {
                if p.len() >= 8 {
                    self.st.arc_p = s16_at(p, 0) as f64;
                    self.st.arc_q = s16_at(p, 2) as f64;
                    self.st.arc_r = s16_at(p, 4) as f64;
                    self.st.arc_s = s16_at(p, 6) as f64;
                }
            }
            0x26 => {
                if p.len() >= 2 {
                    self.st.color = oca_color(u16_at(p, 0)).into();
                }
            }
            0x28 => {
                if let Some(&b) = p.first() {
                    self.st.pattern_sym = if b == 0 { 0x10 } else { b };
                }
            }
            0x60 => self.gear(),
            0x68 => {
                self.st.in_area = true;
                self.st.area_boundary = p.first().is_some_and(|b| b & 0x40 != 0);
                self.st.area_path.clear();
            }
            0x80 => self.gbox(p, false),
            0x81 | 0x85 => self.gline(p, false), // GCLINE, GCFLT (fillet approximated)
            0x87 => self.gfarc(p, false),
            0xA1 => self.grline(p, false),
            0xA3 => self.gparc(p, false),
            0xA5 => self.gbez(p, false),
            0xB2 => self.st.color = gspcol_to_css(p),
            0xC0 => self.gbox(p, true),
            0xC1 | 0xC5 => self.gline(p, true), // GLINE, GFLT (fillet approximated)
            0xC7 => self.gfarc(p, true),
            0xE1 => self.grline(p, true),
            0xE3 => self.gparc(p, true),
            0xE5 => self.gbez(p, true),
            // No-ops: GNOP, comments, mix, custom line types, markers,
            // character strings and attributes, images, patterns, gradients.
            _ => {}
        }
    }
}

/// Decode a GOCA object to an SVG fragment; `None` if the GDD has no
/// Window Specification, the window is degenerate, or nothing is drawn.
pub fn draw_goca(gdd: &[u8], gad: &[u8]) -> Option<GocaGraphic> {
    let (_upi, xlwind, xrwind, ybwind, ytwind) = parse_gdd(gdd)?;
    let (gps_w, gps_h) = (xrwind - xlwind, ytwind - ybwind);
    if gps_w <= 0 || gps_h <= 0 {
        return None;
    }
    let mut ctx = Ctx { xlwind: xlwind as f64, ytwind: ytwind as f64, st: State::default(), out: Vec::new() };
    for (code, params) in parse_orders(gad) {
        ctx.dispatch(code, params);
    }
    let svg = ctx.out.concat();
    if svg.is_empty() {
        return None;
    }
    Some(GocaGraphic { svg, gps_w, gps_h })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn gdd(xl: i16, xr: i16, yb: i16, yt: i16) -> Vec<u8> {
        let mut p = vec![0u8, 0, 0, 0];
        p.extend(14400u16.to_be_bytes());
        p.extend(14400u16.to_be_bytes());
        p.extend([0, 0]);
        for v in [xl, xr, yb, yt] {
            p.extend(v.to_be_bytes());
        }
        let mut d = vec![0xF6, p.len() as u8];
        d.extend(p);
        d
    }

    pub fn segment(orders: &[u8]) -> Vec<u8> {
        let mut s = vec![0x70, 0x0C, 0, 0, 0, 0, 0, 0];
        s.extend((orders.len() as u16).to_be_bytes());
        s.extend([0, 0, 0, 0]);
        s.extend(orders);
        s
    }

    fn coords(v: &[i16]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }

    #[test]
    fn order_formats() {
        assert_eq!(order_format(0x00), Format::Fixed1);
        assert_eq!(order_format(0x68), Format::Fixed2);
        assert_eq!(order_format(0x71), Format::Fixed2);
        assert_eq!(order_format(0xC1), Format::Long);
        assert_eq!(order_format(0x21), Format::Long);
        assert_eq!(order_format(0xFE), Format::Extended);
    }

    #[test]
    fn unchained_segments_are_skipped() {
        let mut data = segment(&[0x0A, 0x02]);
        let mut unchained = segment(&[0x0A, 0x04]);
        unchained[7] = 0x80;
        data.extend(unchained);
        assert_eq!(parse_orders(&data), vec![(0x0A, &[0x02][..])]);
    }

    #[test]
    fn polyline_keeps_precision() {
        let mut orders = vec![0xC1, 12];
        orders.extend(coords(&[0, 0, 1234, 0, 1234, 999]));
        let g = draw_goca(&gdd(0, 2000, 0, 1000), &segment(&orders)).unwrap();
        assert_eq!((g.gps_w, g.gps_h), (2000, 1000));
        assert!(g.svg.starts_with("<polyline points=\"0,1000 1234,1000 1234,1\""), "{}", g.svg);
    }

    #[test]
    fn box_and_colors() {
        let mut orders = vec![0x0A, 0x02, 0xC0, 10, 0, 0];
        orders.extend(coords(&[100, 100, 300, 50]));
        let g = draw_goca(&gdd(0, 1000, 0, 800), &segment(&orders)).unwrap();
        assert_eq!(
            g.svg,
            "<rect x=\"100\" y=\"700\" width=\"200\" height=\"50\" fill=\"#ff0000\" stroke=\"#ff0000\" stroke-width=\"1\" stroke-linecap=\"round\" stroke-linejoin=\"round\"/>"
        );
        let mut orders = vec![0x26, 2, 0x00, 0x05, 0xC0, 10, 0, 0];
        orders.extend(coords(&[0, 0, 1, 1]));
        let g = draw_goca(&gdd(0, 10, 0, 10), &segment(&orders)).unwrap();
        assert!(g.svg.contains("fill=\"#00ffff\""));
    }

    #[test]
    fn process_color_and_area() {
        let mut orders = vec![0xB2, 9, 0, 1, 8, 8, 8, 0, 0x10, 0x20, 0x30];
        orders.extend([0x68, 0x00, 0xC1, 12]);
        orders.extend(coords(&[0, 0, 10, 0, 10, 10]));
        orders.extend([0x60, 0x00]);
        let g = draw_goca(&gdd(0, 10, 0, 10), &segment(&orders)).unwrap();
        assert_eq!(g.svg, "<path d=\"M 0,10 L 10,10 L 10,0 Z\" fill=\"#102030\" stroke=\"none\"/>");
    }

    #[test]
    fn no_window_or_nothing_drawn() {
        assert!(draw_goca(&[], &segment(&[0x0A, 0x02])).is_none());
        assert!(draw_goca(&gdd(0, 10, 0, 10), &segment(&[0x0A, 0x02])).is_none());
        assert!(draw_goca(&gdd(10, 10, 0, 10), &segment(&[0x0A, 0x02])).is_none());
    }

    #[test]
    fn partial_arc_flags() {
        // Unit arc params P=Q=100, start 0°, sweep 90° around (500, 500).
        let mut orders = vec![0x22, 8];
        orders.extend(coords(&[100, 100, 0, 0]));
        orders.extend([0xE3, 18]);
        orders.extend(coords(&[600, 500, 500, 500]));
        orders.extend([1, 0]);
        orders.extend(0u32.to_be_bytes());
        orders.extend((90u32 << 16).to_be_bytes());
        let g = draw_goca(&gdd(0, 1000, 0, 1000), &segment(&orders)).unwrap();
        assert!(g.svg.contains("d=\"M 600,500 L 600,500 A 100,100 0 0 0 500,400\""), "{}", g.svg);
    }
}
