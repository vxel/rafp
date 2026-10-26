//! Specimen pages for font resource files (no document pages): raster glyph
//! grids, outline glyph grids, or an outline metadata listing.

use std::sync::Arc;

use crate::foca::Font;
use crate::goca::GocaGraphic;
use crate::model::{FontWeight, ImageRef, Page, TextRun, VectorGraphic};
use crate::outline::glyph_to_path_d;
use crate::util::py_round;

const UPI: i64 = 1440;
const PAGE_W: i64 = 12240;
const MARGIN: i64 = 600;
const TITLE_SIZE: i64 = 440;

fn styled(x: i64, y: i64, text: String, color: &str, size: i64) -> TextRun {
    let mut t = TextRun::new(x, y, text);
    t.color = color.into();
    t.font_size = size;
    t
}

fn mono(x: i64, y: i64, text: String, color: &str, size: i64) -> TextRun {
    let mut t = styled(x, y, text, color, size);
    t.font_family = "Consolas".into();
    t
}

fn title(text: String) -> TextRun {
    let mut t = TextRun::new(MARGIN, MARGIN, text);
    t.font_size = TITLE_SIZE;
    t.font_weight = FontWeight::Bold;
    t
}

fn label_of(font: &Font) -> &str {
    if !font.typeface.is_empty() {
        &font.typeface
    } else if !font.name.is_empty() {
        &font.name
    } else {
        "embedded font"
    }
}

/// Lay out each embedded font as a labeled specimen sheet.
pub fn font_specimen_pages(fonts: &[Font]) -> Vec<Page> {
    let label_size = 170;
    let glyph_h = 700;
    let cell_w = 1700;
    let cell_h = 1300;
    let grid_top = 980;

    let mut pages = Vec::new();
    for font in fonts {
        let glyphs: Vec<_> = font.glyphs.iter().filter(|g| g.width >= 3 && g.height >= 3).collect();
        if glyphs.is_empty() {
            if font.is_outline() && !font.chars.is_empty() {
                pages.push(outline_font_page(font));
            }
            continue;
        }
        let cols = ((PAGE_W - 2 * MARGIN) / cell_w).max(1);
        let rows = (glyphs.len() as i64 + cols - 1) / cols;
        let mut page = Page::with_size(PAGE_W, grid_top + rows * cell_h + MARGIN, UPI);
        page.texts.push(title(format!("Embedded raster font: {} — {} glyphs", label_of(font), glyphs.len())));
        for (i, g) in glyphs.iter().enumerate() {
            let (col, row) = (i as i64 % cols, i as i64 / cols);
            let cell_x = MARGIN + col * cell_w;
            let cell_y = grid_top + row * cell_h;
            let mut gw = py_round(glyph_h as f64 * g.width as f64 / g.height as f64);
            let mut gh = glyph_h;
            let max_w = cell_w - 200;
            if gw > max_w {
                gh = py_round(gh as f64 * max_w as f64 / gw as f64);
                gw = max_w;
            }
            page.images.push(ImageRef::new(
                cell_x + (cell_w - gw).div_euclid(2),
                cell_y + (glyph_h - gh),
                gw,
                gh,
                "image/png",
                g.png.clone(),
            ));
            let gid = if g.gcgid.is_empty() { "?".to_string() } else { g.gcgid.clone() };
            page.texts.push(mono(cell_x + 40, cell_y + glyph_h + label_size + 60, gid, "#666666", label_size));
        }
        for t in &mut page.texts {
            t.fit = false; // fixed grid layout: never width-fit
        }
        pages.push(page);
    }
    pages
}

const OUTLINE_CHARS_SHOWN: usize = 256;

/// An outline font's decoded glyph shapes as a specimen grid.
fn outline_glyph_page(font: &Font) -> Page {
    let sub_size = 220;
    let label_size = 160;
    let cell_w = 1500;
    let cell_h = 1500;
    let glyph_h = 1000;
    let grid_top = 1640;

    let em = if font.units_per_em != 0 { font.units_per_em } else { 1000 };
    let ascent = py_round(0.80 * em as f64);
    let descent = py_round(0.24 * em as f64);
    let box_h = ascent + descent;
    let gw = py_round(glyph_h as f64 * em as f64 / box_h as f64);

    let drawable: Vec<_> = font
        .chars
        .iter()
        .filter_map(|cm| font.outline_glyphs.get(&cm.gcgid).filter(|g| !g.segments.is_empty()).map(|g| (cm, g)))
        .collect();
    let cols = ((PAGE_W - 2 * MARGIN) / cell_w).max(1);
    let rows = (drawable.len() as i64 + cols - 1) / cols;
    let mut page = Page::with_size(PAGE_W, grid_top + rows * cell_h + MARGIN, UPI);
    let mut t = title(format!("Embedded outline font: {}", label_of(font)));
    t.fit = false;
    page.texts.push(t);
    let mut t = styled(
        MARGIN,
        MARGIN + sub_size + 160,
        format!("{} glyphs · {} · {em}-unit em — actual embedded outlines", drawable.len(), font.tech_label()),
        "#666666",
        sub_size,
    );
    t.fit = false;
    page.texts.push(t);
    for (i, (cm, glyph)) in drawable.iter().enumerate() {
        let (col, row) = (i as i64 % cols, i as i64 / cols);
        let cell_x = MARGIN + col * cell_w;
        let cell_y = grid_top + row * cell_h;
        let d = glyph_to_path_d(glyph, 1.0, 0.0, ascent as f64);
        page.graphics.push(VectorGraphic {
            x: cell_x + (cell_w - gw).div_euclid(2),
            y: cell_y,
            width: gw,
            height: glyph_h,
            graphic: Arc::new(GocaGraphic {
                svg: format!("<path d=\"{d}\" fill=\"#1a1a2a\"/>"),
                gps_w: em,
                gps_h: box_h,
            }),
            rotate: None,
        });
        let name = if cm.name.is_empty() { cm.gcgid.clone() } else { cm.name.clone() };
        let mut t = mono(cell_x + 40, cell_y + glyph_h + label_size + 40, name, "#666666", label_size);
        t.fit = false;
        page.texts.push(t);
    }
    page
}

/// Specimen page for an outline font: its glyph shapes when decoded, else a
/// metadata listing of characters and increments.
fn outline_font_page(font: &Font) -> Page {
    if font.outline_glyphs.values().any(|g| !g.segments.is_empty()) {
        return outline_glyph_page(font);
    }
    let sub_size = 220;
    let label_size = 200;
    let named = font.chars.iter().any(|c| !c.name.is_empty());
    let cols: i64 = if named { 3 } else { 4 };
    let cell_w = (PAGE_W - 2 * MARGIN) / cols;
    let row_h = 340;
    let grid_top = 1640;

    let chars = &font.chars[..font.chars.len().min(OUTLINE_CHARS_SHOWN)];
    let remaining = font.chars.len() - chars.len();
    let rows = (chars.len() as i64 + cols - 1) / cols;
    let extra = if remaining > 0 { 1 } else { 0 };
    let mut page = Page::with_size(PAGE_W, grid_top + (rows + extra) * row_h + MARGIN, UPI);

    page.texts.push(title(format!("Embedded outline font: {}", label_of(font))));
    let orient = if font.orientations > 1 { format!(" × {} orientations", font.orientations) } else { String::new() };
    page.texts.push(styled(
        MARGIN,
        MARGIN + sub_size + 160,
        format!("{} — {} characters{orient}; glyph shapes not rasterized", font.tech_label(), font.chars.len()),
        "#666666",
        sub_size,
    ));
    let heading = if named {
        "GCGID · glyph name · inline increment (font design units):"
    } else {
        "GCGID + inline increment (font design units):"
    };
    page.texts.push(styled(MARGIN, MARGIN + 2 * (sub_size + 160), heading.into(), "#666666", sub_size));
    for (i, cm) in chars.iter().enumerate() {
        let (col, row) = (i as i64 % cols, i as i64 / cols);
        let gid = if cm.gcgid.is_empty() { "?" } else { &cm.gcgid };
        let cell = if named {
            format!("{gid:<8}  {}  {}", cm.name, cm.char_increment)
        } else {
            format!("{gid:<8}  {}", cm.char_increment)
        };
        page.texts.push(mono(MARGIN + col * cell_w, grid_top + row * row_h, cell, "#444444", label_size));
    }
    if remaining > 0 {
        page.texts.push(styled(
            MARGIN,
            grid_top + rows * row_h + row_h / 2,
            format!("... and {remaining} more characters"),
            "#666666",
            sub_size,
        ));
    }
    for t in &mut page.texts {
        t.fit = false;
    }
    page
}
