//! The page model built by [`crate::ptoca::extract_pages`]: positioned text
//! runs, rules, images and vector graphics, all in page L-units.

use std::sync::Arc;

use serde::Serialize;

use crate::goca::GocaGraphic;

pub const DEFAULT_COLOR: &str = "#000000";
/// The color of a standard OCA named-color value (X'0001' blue to X'0010'
/// brown), shared by PTOCA (STC) and GOCA (GSCOL); `None` for the default
/// and unnamed values, which each caller resolves.
pub fn oca_named_color(value: u16) -> Option<&'static str> {
    Some(match value {
        0x0001 => "#0000ff", // blue
        0x0002 => "#ff0000", // red
        0x0003 => "#ff00ff", // pink / magenta
        0x0004 => "#00ff00", // green
        0x0005 => "#00ffff", // cyan / turquoise
        0x0006 => "#ffff00", // yellow
        0x0008 => "#000000", // black
        0x0010 => "#a52a2a", // brown
        _ => return None,
    })
}

/// 12pt at 1440 units/inch.
pub const DEFAULT_FONT_SIZE: i64 = 240;
/// Cap on each kind of content per page (text runs, rules, images,
/// graphics, hidden text), so pathological files produce a bounded render.
pub const MAX_RUNS_PER_PAGE: usize = 5000;

/// A reason the render of an element may differ from the original.
///
/// `cat` groups notes: `font` (substitute font, external reference,
/// estimated size), `glyph` (code point omitted), `codepage` (fallback code
/// page), `image` (composite approximation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FidelityNote {
    pub cat: NoteCategory,
    pub msg: String,
}

impl FidelityNote {
    pub fn new(cat: NoteCategory, msg: impl Into<String>) -> FidelityNote {
        FidelityNote { cat, msg: msg.into() }
    }
}

/// What a fidelity note is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum NoteCategory {
    /// A substitute or estimated font.
    Font,
    /// Characters omitted or replaced.
    Glyph,
    /// Text decoded with a fallback code page.
    Codepage,
    /// An approximated image (CMYK compositing).
    Image,
}

impl NoteCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            NoteCategory::Font => "font",
            NoteCategory::Glyph => "glyph",
            NoteCategory::Codepage => "codepage",
            NoteCategory::Image => "image",
        }
    }
}

/// A kind of AFP resource: what a page-less file holds, or what an MCF
/// references.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    FontCharacterSet,
    /// A character set referenced by an MCF.
    CharacterSet,
    CodedFont,
    CodePage,
    PageOverlay,
    PageSegment,
    IocaImage,
    GocaGraphics,
    BcocaBarCode,
    ObjectContainer,
    ResourceGroup,
    ObjectResource,
}

impl ResourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ResourceKind::FontCharacterSet => "font character set",
            ResourceKind::CharacterSet => "character set",
            ResourceKind::CodedFont => "coded font",
            ResourceKind::CodePage => "code page",
            ResourceKind::PageOverlay => "page overlay",
            ResourceKind::PageSegment => "page segment",
            ResourceKind::IocaImage => "IOCA image resource",
            ResourceKind::GocaGraphics => "GOCA graphics resource",
            ResourceKind::BcocaBarCode => "BCOCA bar code resource",
            ResourceKind::ObjectContainer => "object container",
            ResourceKind::ResourceGroup => "resource group",
            ResourceKind::ObjectResource => "object resource",
        }
    }
}

impl Serialize for ResourceKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Font weight of a text run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontWeight {
    #[default]
    Normal,
    Bold,
}

impl FontWeight {
    pub fn as_str(self) -> &'static str {
        match self {
            FontWeight::Normal => "normal",
            FontWeight::Bold => "bold",
        }
    }
}

/// Direction of a PTOCA rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleAxis {
    /// Along the inline direction (DIR, horizontal).
    Inline,
    /// Along the baseline direction (DBR, vertical).
    Baseline,
}

impl RuleAxis {
    /// The PTOCA letter: 'I' or 'B'.
    pub fn as_char(self) -> char {
        match self {
            RuleAxis::Inline => 'I',
            RuleAxis::Baseline => 'B',
        }
    }
}

/// A font mapped to a local id by MDR/MCF, as far as it can be decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct FontInfo {
    pub family: String,
    pub weight: FontWeight,
    /// L-units (at 1440/inch, 1pt = 20 units).
    pub size: Option<i64>,
    /// Point size from an external coded-font name; resolved to L-units at
    /// render time.
    pub size_pt: Option<f64>,
    pub notes: Vec<FidelityNote>,
}

impl Default for FontInfo {
    fn default() -> Self {
        FontInfo { family: "Arial".into(), weight: FontWeight::Normal, size: None, size_pt: None, notes: Vec::new() }
    }
}

/// A run of text positioned on a page.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    pub x: i64,
    pub y: i64,
    pub text: String,
    pub color: String,
    pub font_id: Option<u8>,
    pub font_size: i64,
    pub font_family: String,
    pub font_weight: FontWeight,
    /// Clockwise degrees (0/90/180/270) from STO.
    pub orientation: i64,
    /// Offset of the PTX field that produced it.
    pub src: Option<usize>,
    /// Allow width-fitting; false for fixed synthetic layout.
    pub fit: bool,
    /// SVI space width in L-units.
    pub space_width: Option<i64>,
    pub notes: Vec<FidelityNote>,
}

impl TextRun {
    pub fn new(x: i64, y: i64, text: impl Into<String>) -> TextRun {
        TextRun {
            x,
            y,
            text: text.into(),
            color: DEFAULT_COLOR.into(),
            font_id: None,
            font_size: DEFAULT_FONT_SIZE,
            font_family: "Arial".into(),
            font_weight: FontWeight::Normal,
            orientation: 0,
            src: None,
            fit: true,
            space_width: None,
            notes: Vec::new(),
        }
    }
}

/// Page-space rotation `(angle_deg, cx, cy)`.
pub type Rotation = (i64, i64, i64);

/// A raster image placed on a page.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageRef {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    pub mime: String,
    pub data: Arc<Vec<u8>>,
    /// CMYK plane images (JPEG or PNG) for band-interleaved images.
    pub bands: Option<Arc<Vec<Vec<u8>>>>,
    /// Scale without smoothing (bar codes, bilevel IM images).
    pub crisp: bool,
    /// Recolor a black-on-white glyph bitmap to this color.
    pub recolor: Option<String>,
    pub notes: Vec<FidelityNote>,
    pub rotate: Option<Rotation>,
}

impl ImageRef {
    pub fn new(x: i64, y: i64, width: i64, height: i64, mime: &str, data: Arc<Vec<u8>>) -> ImageRef {
        ImageRef {
            x,
            y,
            width,
            height,
            mime: mime.into(),
            data,
            bands: None,
            crisp: false,
            recolor: None,
            notes: Vec::new(),
            rotate: None,
        }
    }
}

/// A solid rule (line) on a page.
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub x: i64,
    pub y: i64,
    pub length: i64,
    pub thickness: i64,
    pub axis: RuleAxis,
    pub color: String,
    pub src: Option<usize>,
}

/// A vector graphic (GOCA object or outline glyph run) placed on a page.
#[derive(Debug, Clone, PartialEq)]
pub struct VectorGraphic {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
    pub graphic: Arc<GocaGraphic>,
    pub rotate: Option<Rotation>,
}

/// One page's geometry and content.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub width: i64,
    pub height: i64,
    pub units_per_inch: i64,
    pub texts: Vec<TextRun>,
    pub rules: Vec<Rule>,
    pub images: Vec<ImageRef>,
    pub graphics: Vec<VectorGraphic>,
    /// Decoded text for runs drawn as embedded glyphs (not rendered, but
    /// kept for text extraction).
    pub text_layer: Vec<TextRun>,
    /// Content dropped after [`MAX_RUNS_PER_PAGE`].
    pub truncated: bool,
}

impl Default for Page {
    fn default() -> Self {
        Page {
            width: 12240,
            height: 15840,
            units_per_inch: 1440,
            texts: Vec::new(),
            rules: Vec::new(),
            images: Vec::new(),
            graphics: Vec::new(),
            text_layer: Vec::new(),
            truncated: false,
        }
    }
}

impl Page {
    pub fn with_size(width: i64, height: i64, units_per_inch: i64) -> Page {
        Page { width, height, units_per_inch, ..Default::default() }
    }

    /// Text runs joined in reading order (top-down, then left-right),
    /// including the hidden layer of runs drawn as embedded glyphs.
    pub fn plain_text(&self) -> String {
        let mut ordered: Vec<&TextRun> = self.texts.iter().chain(self.text_layer.iter()).collect();
        ordered.sort_by_key(|r| (r.y, r.x));
        let mut out = String::new();
        let mut last_y: Option<i64> = None;
        for run in ordered {
            match last_y {
                Some(ly) if (run.y - ly).abs() > 40 => out.push('\n'),
                _ if !out.is_empty() || last_y.is_some() => out.push(' '),
                _ => {}
            }
            out.push_str(&run.text);
            last_y = Some(run.y);
        }
        out
    }

    pub fn push_text(&mut self, t: TextRun) {
        push_capped(&mut self.texts, t, &mut self.truncated);
    }

    pub fn push_rule(&mut self, r: Rule) {
        push_capped(&mut self.rules, r, &mut self.truncated);
    }

    pub fn push_image(&mut self, im: ImageRef) {
        push_capped(&mut self.images, im, &mut self.truncated);
    }

    pub fn push_graphic(&mut self, g: VectorGraphic) {
        push_capped(&mut self.graphics, g, &mut self.truncated);
    }

    pub fn push_hidden_text(&mut self, t: TextRun) {
        push_capped(&mut self.text_layer, t, &mut self.truncated);
    }

    /// Whether the page carries anything to draw.
    pub fn has_content(&self) -> bool {
        !(self.texts.is_empty() && self.rules.is_empty() && self.images.is_empty() && self.graphics.is_empty())
    }
}

/// Add `item` unless `list` is full ([`MAX_RUNS_PER_PAGE`]); then flag the
/// page as truncated instead.
fn push_capped<T>(list: &mut Vec<T>, item: T, truncated: &mut bool) {
    if list.len() < MAX_RUNS_PER_PAGE {
        list.push(item);
    } else {
        *truncated = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_reading_order() {
        let mut p = Page::default();
        p.texts.push(TextRun::new(500, 100, "world"));
        p.texts.push(TextRun::new(100, 100, "hello"));
        p.text_layer.push(TextRun::new(100, 300, "next"));
        assert_eq!(p.plain_text(), "hello world\nnext");
    }
}
