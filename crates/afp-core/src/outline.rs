//! Outline glyphs shared by the Type 1 and CFF interpreters: path segments,
//! the glyph type, the failure marker, work limits, and SVG path output.

use std::fmt::Write;

/// One outline path segment, in glyph design units (y up).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Segment {
    M(f64, f64),
    L(f64, f64),
    C(f64, f64, f64, f64, f64, f64),
    Z,
}

impl Segment {
    pub(crate) fn shifted(self, dx: f64, dy: f64) -> Segment {
        match self {
            Segment::M(x, y) => Segment::M(x + dx, y + dy),
            Segment::L(x, y) => Segment::L(x + dx, y + dy),
            Segment::C(a, b, c, d, e, f) => Segment::C(a + dx, b + dy, c + dx, d + dy, e + dx, f + dy),
            Segment::Z => Segment::Z,
        }
    }
}

/// One decoded outline glyph.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Glyph {
    /// Inline advance width, in design units.
    pub advance: f64,
    pub segments: Vec<Segment>,
}

/// Marker for a charstring that failed to interpret (truncated operand,
/// short stack, runaway recursion); the glyph is then dropped.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Malformed;

pub(crate) type Res<T> = Result<T, Malformed>;

/// Work limits per glyph, so a crafted charstring (subroutines calling
/// themselves twice, `seac` loops) fails instead of hanging or exhausting
/// memory. Real glyphs stay far below them.
pub(crate) const MAX_GLYPH_OPS: usize = 200_000;
pub(crate) const MAX_GLYPH_SEGMENTS: usize = 20_000;
pub(crate) const MAX_GLYPH_STACK: usize = 512;

/// Units per em from the FontMatrix x scale (0.001 → 1000). Values outside
/// a plausible range come from corrupt fonts and would overflow the glyph
/// geometry; they fall back to 1000.
pub(crate) fn units_per_em(matrix_x: f64) -> i64 {
    let upem = if matrix_x != 0.0 { crate::util::py_round(1.0 / matrix_x) } else { 1000 };
    if (16..=16_384).contains(&upem) { upem } else { 1000 }
}

/// Render a glyph's segments as an SVG path, flipping to y-down space.
///
/// `scale` maps design units to the target size; (`ox`, `oy`) places the
/// glyph origin (baseline) in the target coordinate system.
pub fn glyph_to_path_d(glyph: &Glyph, scale: f64, ox: f64, oy: f64) -> String {
    let mut out = String::new();
    let px = |x: f64| ox + x * scale;
    let py = |y: f64| oy - y * scale;
    for seg in &glyph.segments {
        // Writing to a String cannot fail.
        let _ = match *seg {
            Segment::M(x, y) => write!(out, "M{:.1} {:.1}", px(x), py(y)),
            Segment::L(x, y) => write!(out, "L{:.1} {:.1}", px(x), py(y)),
            Segment::C(a, b, c, d, e, f) => {
                write!(out, "C{:.1} {:.1} {:.1} {:.1} {:.1} {:.1}", px(a), py(b), px(c), py(d), px(e), py(f))
            }
            Segment::Z => out.write_char('Z'),
        };
    }
    out
}
