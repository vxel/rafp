//! Rasterize page SVGs with resvg (feature `raster`).
//!
//! The viewer renders [`crate::render_svg::page_to_svg_with_ids`] output: each
//! drawable element carries an `id` (`t12` text run, `r3` rule, `i5` image,
//! `g2` vector graphic) so its exact on-page box can be read back from the
//! parsed tree for hit-testing, flashing and the fidelity overlay.

use std::collections::HashMap;
use std::sync::Arc;

use crate::model::Page;

pub use resvg::{tiny_skia, usvg};

/// An axis-aligned box in page L-units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Largest render, in pixels (1.6 GB of RGBA; a 600 dpi A3 page is about
/// 70 million).
pub const MAX_RENDER_PIXELS: u64 = 400_000_000;

/// Parses and rasterizes page SVGs with the system fonts.
pub struct Rasterizer {
    options: usvg::Options<'static>,
}

fn has_family(db: &usvg::fontdb::Database, family: &str) -> bool {
    db.faces().any(|f| f.families.iter().any(|(n, _)| n.eq_ignore_ascii_case(family)))
}

fn first_available(db: &usvg::fontdb::Database, candidates: &[&str]) -> Option<String> {
    candidates.iter().find(|c| has_family(db, c)).map(|c| (*c).to_string())
}

impl Default for Rasterizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Rasterizer {
    /// Load the system fonts and pick metric-compatible substitutes for the
    /// generic families (Arial, Times New Roman, Courier New).
    pub fn new() -> Rasterizer {
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        let sans = first_available(
            &db,
            &["Arial", "Liberation Sans", "Arimo", "Helvetica", "Nimbus Sans", "DejaVu Sans", "Noto Sans"],
        );
        let serif = first_available(
            &db,
            &["Times New Roman", "Liberation Serif", "Tinos", "Times", "Nimbus Roman", "DejaVu Serif", "Noto Serif"],
        );
        let mono = first_available(
            &db,
            &["Courier New", "Liberation Mono", "Cousine", "Nimbus Mono PS", "DejaVu Sans Mono", "Noto Sans Mono"],
        );
        if let Some(f) = &sans {
            db.set_sans_serif_family(f.clone());
        }
        if let Some(f) = &serif {
            db.set_serif_family(f.clone());
        }
        if let Some(f) = &mono {
            db.set_monospace_family(f.clone());
        }
        let mut options = usvg::Options::default();
        if let Some(f) = sans {
            options.font_family = f;
        }
        options.fontdb = Arc::new(db);
        Rasterizer { options }
    }

    /// Replace font families the system lacks by their metric-compatible
    /// substitutes (e.g. Arial → Liberation Sans), so text keeps its widths.
    pub fn substitute_families(&self, svg: &str) -> String {
        let db = &self.options.fontdb;
        let mut out = svg.to_string();
        for (wanted, generic) in [
            ("Arial", "sans-serif"),
            ("Helvetica", "sans-serif"),
            ("Verdana", "sans-serif"),
            ("Times New Roman", "serif"),
            ("Courier New", "monospace"),
            ("Consolas", "monospace"),
        ] {
            if !has_family(db, wanted) {
                out = out
                    .replace(&format!("\"{wanted}, "), "\"")
                    .replace(&format!("\"{wanted}\""), &format!("\"{generic}\""));
            }
        }
        out
    }

    pub fn parse(&self, svg: &str) -> Result<usvg::Tree, usvg::Error> {
        usvg::Tree::from_str(&self.substitute_families(svg), &self.options)
    }

    /// Like [`Rasterizer::parse`], for a render that shrinks the page's
    /// pixel-exact images (see [`crisp_image_resolution`]) when `shrinking`:
    /// they are then filtered smoothly, as nearest-neighbour sampling would
    /// drop most of their pixels and break thin strokes apart.
    pub fn parse_for_scale(&self, svg: &str, shrinking: bool) -> Result<usvg::Tree, usvg::Error> {
        if shrinking {
            self.parse(&svg.replace("image-rendering:pixelated", "image-rendering:optimizeQuality"))
        } else {
            self.parse(svg)
        }
    }

    /// Render at `scale` pixels per L-unit; `None` for an empty size or one
    /// over [`MAX_RENDER_PIXELS`] (a corrupt page size).
    pub fn render(&self, tree: &usvg::Tree, scale: f32) -> Option<tiny_skia::Pixmap> {
        let size = tree.size();
        let (w, h) = ((size.width() * scale).ceil().max(1.0), (size.height() * scale).ceil().max(1.0));
        if f64::from(w) * f64::from(h) > MAX_RENDER_PIXELS as f64 {
            log::warn!("not rendering a {w}x{h} pixel page");
            return None;
        }
        let (w, h) = (w as u32, h as u32);
        let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
        pixmap.fill(tiny_skia::Color::WHITE);
        resvg::render(tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
        Some(pixmap)
    }
}

/// The finest resolution, in pixels per L-unit, of the page's pixel-exact
/// (`crisp`) PNG images: a render below it shrinks them.
pub fn crisp_image_resolution(page: &Page) -> Option<f32> {
    page.images
        .iter()
        .filter(|im| im.crisp && im.mime == "image/png" && im.width > 0)
        .filter_map(|im| {
            // PNG: the IHDR width follows the 8-byte signature and the chunk header.
            let w = u32::from_be_bytes(im.data.get(16..20)?.try_into().ok()?);
            Some(w as f32 / im.width as f32)
        })
        .reduce(f32::max)
}

/// The absolute bounding box of every node with an `id`, in page units.
pub fn element_boxes(tree: &usvg::Tree) -> HashMap<String, BoxF> {
    let mut out = HashMap::new();
    collect(tree.root(), &mut out);
    out
}

fn collect(group: &usvg::Group, out: &mut HashMap<String, BoxF>) {
    for node in group.children() {
        let id = node.id();
        if !id.is_empty() && !out.contains_key(id) {
            let r = node.abs_bounding_box();
            out.insert(id.to_string(), BoxF { x: r.x(), y: r.y(), w: r.width(), h: r.height() });
        }
        if let usvg::Node::Group(g) = node {
            collect(g, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Page, Rule, TextRun};
    use crate::render_svg::page_to_svg_with_ids;

    #[test]
    fn renders_and_reports_boxes() {
        let r = Rasterizer::new();
        let mut page = Page::with_size(1440, 720, 1440);
        page.rules.push(Rule {
            x: 100,
            y: 100,
            length: 400,
            thickness: 20,
            axis: crate::model::RuleAxis::Inline,
            color: "#ff0000".into(),
            src: Some(1),
        });
        page.texts.push(TextRun::new(100, 400, "Hello"));
        let tree = r.parse(&page_to_svg_with_ids(&page)).unwrap();
        let boxes = element_boxes(&tree);
        let rule = boxes["r0"];
        assert_eq!((rule.x, rule.y, rule.w, rule.h), (100.0, 100.0, 400.0, 20.0));
        let pix = r.render(&tree, 0.1).unwrap();
        assert_eq!((pix.width(), pix.height()), (144, 72));
        // a pixel on the red rule
        let p = pix.pixel(30, 11).unwrap();
        assert_eq!((p.red(), p.green(), p.blue()), (255, 0, 0));
        if has_family(&r.options.fontdb, "Liberation Sans") || has_family(&r.options.fontdb, "Arial") {
            assert!(boxes.contains_key("t0"));
        }
    }
}
