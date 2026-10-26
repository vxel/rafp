//! Render extracted pages as SVG markup.
//!
//! Coordinates are L-units, so the SVG viewBox is the page size. Text runs
//! are drawn at their PTOCA coordinates in a substitute font, rules as
//! rectangles, images as data URIs (CMYK planes composited with ink
//! filters), and vector graphics as nested `<svg>` viewports. Elements
//! produced by a PTX field carry `data-src` (the field offset) so viewers can
//! link them back to the inspector.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;

use crate::model::{
    FidelityNote, FontWeight, ImageRef, MAX_RUNS_PER_PAGE, Page, Rotation, RuleAxis, TextRun, VectorGraphic,
};
use crate::util::{py_strip, quote_attr, xml_escape};

fn fidelity_attr(notes: &[FidelityNote]) -> String {
    if notes.is_empty() {
        return String::new();
    }
    let payload = serde_json::to_string(notes).unwrap_or_default();
    format!(" data-fidelity={}", quote_attr(&payload))
}

/// Each CMYK plane is grayscale (ink amount); these filters map a plane to
/// its complement color so multiply-blending the four planes composes the
/// inks: R = (1-C)(1-K), G = (1-M)(1-K), B = (1-Y)(1-K).
const INK_FILTERS: &str = concat!(
    "<defs>",
    "<filter id=\"ink-c\" color-interpolation-filters=\"sRGB\">",
    "<feColorMatrix values=\"-1 0 0 0 1  0 0 0 0 1  0 0 0 0 1  0 0 0 1 0\"/>",
    "</filter>",
    "<filter id=\"ink-m\" color-interpolation-filters=\"sRGB\">",
    "<feColorMatrix values=\"0 0 0 0 1  0 -1 0 0 1  0 0 0 0 1  0 0 0 1 0\"/>",
    "</filter>",
    "<filter id=\"ink-y\" color-interpolation-filters=\"sRGB\">",
    "<feColorMatrix values=\"0 0 0 0 1  0 0 0 0 1  0 0 -1 0 1  0 0 0 1 0\"/>",
    "</filter>",
    "<filter id=\"ink-k\" color-interpolation-filters=\"sRGB\">",
    "<feColorMatrix values=\"-1 0 0 0 1  0 -1 0 0 1  0 0 -1 0 1  0 0 0 1 0\"/>",
    "</filter>",
    "</defs>",
);

fn glyph_ink_id(color: &str) -> String {
    let clean: String = color.chars().filter(|c| c.is_alphanumeric()).collect();
    format!("glyphink-{clean}")
}

/// Filters recoloring 1-bit black-on-white glyph bitmaps to a text color:
/// invert, luminance to alpha, then flood the color through that mask.
fn glyph_ink_filters(page: &Page) -> String {
    let mut colors: Vec<&str> = page.images.iter().filter_map(|i| i.recolor.as_deref()).collect();
    colors.sort();
    colors.dedup();
    if colors.is_empty() {
        return String::new();
    }
    let mut out = String::from("<defs>");
    for color in colors {
        out.push_str(&format!(
            "<filter id=\"{}\" color-interpolation-filters=\"sRGB\">\
             <feColorMatrix type=\"matrix\" values=\"-1 0 0 0 1  0 -1 0 0 1  0 0 -1 0 1  0 0 0 1 0\"/>\
             <feColorMatrix type=\"luminanceToAlpha\" result=\"m\"/>\
             <feFlood flood-color={} result=\"c\"/>\
             <feComposite in=\"c\" in2=\"m\" operator=\"in\"/>\
             </filter>",
            glyph_ink_id(color),
            quote_attr(color)
        ));
    }
    out.push_str("</defs>");
    out
}

fn rotate_attr(rotate: Option<Rotation>) -> String {
    match rotate {
        Some((a, cx, cy)) => format!(" transform=\"rotate({a} {cx} {cy})\""),
        None => String::new(),
    }
}

fn image_markup(img: &ImageRef, id: &str) -> String {
    let bx = format!(
        "x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" preserveAspectRatio=\"xMidYMid meet\"",
        img.x, img.y, img.width, img.height
    );
    let fid = fidelity_attr(&img.notes);
    let xf = rotate_attr(img.rotate);
    match &img.bands {
        None => {
            let crisp = if img.crisp { " style=\"image-rendering:pixelated\"" } else { "" };
            let ink =
                img.recolor.as_deref().map(|c| format!(" filter=\"url(#{})\"", glyph_ink_id(c))).unwrap_or_default();
            format!(
                "<image{id} {bx}{crisp}{ink}{xf}{fid} href=\"data:{};base64,{}\"/>",
                img.mime,
                B64.encode(&*img.data)
            )
        }
        Some(bands) => {
            let mut out = format!("<g{id} style=\"isolation:isolate\"{fid}>");
            for (ink, blob) in ['c', 'm', 'y', 'k'].iter().zip(bands.iter()) {
                let mime = if blob.starts_with(b"\x89PNG") { "image/png" } else { "image/jpeg" };
                let blend = if *ink == 'c' { "" } else { " style=\"mix-blend-mode:multiply\"" };
                out.push_str(&format!(
                    "<image {bx} filter=\"url(#ink-{ink})\"{blend} href=\"data:{mime};base64,{}\"/>",
                    B64.encode(blob)
                ));
            }
            out.push_str("</g>");
            out
        }
    }
}

/// Stretch a run to the width the AFP's own positioning implies, when the
/// next run sits on the same baseline.
fn fit(texts: &[TextRun], i: usize) -> String {
    let run = &texts[i];
    if !run.fit || run.orientation != 0 {
        return String::new();
    }
    let Some(nxt) = texts.get(i + 1) else { return String::new() };
    if nxt.y != run.y || nxt.x <= run.x {
        return String::new();
    }
    if py_strip(&run.text).chars().count() < 4 {
        return String::new();
    }
    let avail = nxt.x - run.x - (0.3 * run.font_size as f64).trunc() as i64;
    let est = run.text.chars().count() as f64 * 0.52 * run.font_size as f64;
    if est <= 0.0 {
        return String::new();
    }
    let ratio = avail as f64 / est;
    if !(0.7..=1.4).contains(&ratio) {
        return String::new();
    }
    format!(" textLength=\"{avail}\" lengthAdjust=\"spacingAndGlyphs\"")
}

/// Arial's space glyph advances about this fraction of the em.
const ARIAL_SPACE_RATIO: f64 = 0.278;

/// Honor a run's SVI space width via SVG word-spacing.
fn word_spacing(run: &TextRun) -> String {
    let Some(sw) = run.space_width else { return String::new() };
    let size = run.font_size as f64;
    let delta = (sw as f64 - ARIAL_SPACE_RATIO * size).max(-0.12 * size);
    format!(" word-spacing=\"{delta:.1}\"")
}

fn vector_graphic_markup(vg: &VectorGraphic, id: &str) -> String {
    let g = &vg.graphic;
    let xf = rotate_attr(vg.rotate);
    let inner_id = if xf.is_empty() { id } else { "" };
    let inner = format!(
        "<svg{inner_id} x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\" overflow=\"hidden\">{}</svg>",
        vg.x, vg.y, vg.width, vg.height, g.gps_w, g.gps_h, g.svg
    );
    if xf.is_empty() { inner } else { format!("<g{id}{xf}>{inner}</g>") }
}

/// Build an SVG document for one page.
pub fn page_to_svg(page: &Page) -> String {
    page_svg(page, false)
}

/// Like [`page_to_svg`], with an `id` on every drawable element (`t<i>` text
/// run, `r<i>` rule, `i<i>` image, `g<i>` vector graphic, indexed in the
/// page's lists) so viewers can locate them in the rendered tree.
pub fn page_to_svg_with_ids(page: &Page) -> String {
    page_svg(page, true)
}

fn element_id(ids: bool, kind: char, i: usize) -> String {
    if ids { format!(" id=\"{kind}{i}\"") } else { String::new() }
}

fn page_svg(page: &Page, ids: bool) -> String {
    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\" data-upi=\"{u}\" \
         font-family=\"Arial, Helvetica, sans-serif\"><rect width=\"{w}\" height=\"{h}\" fill=\"#ffffff\"/>",
        w = page.width,
        h = page.height,
        u = page.units_per_inch
    );
    if page.images.iter().any(|i| i.bands.is_some()) {
        out.push_str(INK_FILTERS);
    }
    out.push_str(&glyph_ink_filters(page));
    for (ri, rule) in page.rules.iter().enumerate() {
        // Rules extend from the current position in the +I/+B direction;
        // negative length or width extends the other way.
        let (mut x, mut y) = (rule.x, rule.y);
        let (mut w, mut h) =
            if rule.axis == RuleAxis::Inline { (rule.length, rule.thickness) } else { (rule.thickness, rule.length) };
        if w < 0 {
            x += w;
            w = -w;
        }
        if h < 0 {
            y += h;
            h = -h;
        }
        let src = rule.src.map(|s| format!(" data-src=\"{s}\"")).unwrap_or_default();
        out.push_str(&format!(
            "<rect{} x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\"{src} fill={}/>",
            element_id(ids, 'r', ri),
            quote_attr(&rule.color)
        ));
    }
    for (ii, img) in page.images.iter().enumerate() {
        out.push_str(&image_markup(img, &element_id(ids, 'i', ii)));
    }
    for (gi, vg) in page.graphics.iter().enumerate() {
        out.push_str(&vector_graphic_markup(vg, &element_id(ids, 'g', gi)));
    }
    for (i, run) in page.texts.iter().enumerate() {
        let weight = if run.font_weight == FontWeight::Bold { " font-weight=\"bold\"" } else { "" };
        let family = if run.font_family != "Arial" {
            format!(" font-family={}", quote_attr(&run.font_family))
        } else {
            String::new()
        };
        let src = run.src.map(|s| format!(" data-src=\"{s}\"")).unwrap_or_default();
        let rot = if run.orientation != 0 {
            format!(" transform=\"rotate({},{},{})\"", run.orientation, run.x, run.y)
        } else {
            String::new()
        };
        // An explicit SVI space width wins over the next-run estimate.
        let ws = word_spacing(run);
        let width = if ws.is_empty() { fit(&page.texts, i) } else { ws };
        out.push_str(&format!(
            "<text{} x=\"{}\" y=\"{}\" font-size=\"{}\"{family}{weight}{src}{rot}{width}{} fill={}>{}</text>",
            element_id(ids, 't', i),
            run.x,
            run.y,
            run.font_size,
            fidelity_attr(&run.notes),
            quote_attr(&run.color),
            xml_escape(&run.text)
        ));
    }
    if page.truncated {
        out.push_str(&format!(
            "<text x=\"60\" y=\"{}\" font-size=\"200\" fill=\"#9aa0b8\">[render truncated: page content exceeds {MAX_RUNS_PER_PAGE} runs]</text>",
            page.height - 80
        ));
    }
    out.push_str("</svg>");
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::model::{Rule, TextRun};

    #[test]
    fn rules_and_text() {
        let mut page = Page::with_size(1000, 800, 1440);
        page.rules.push(Rule {
            x: 100,
            y: 100,
            length: -50,
            thickness: 10,
            axis: RuleAxis::Inline,
            color: "#000000".into(),
            src: Some(3),
        });
        let mut t = TextRun::new(10, 20, "a<b");
        t.font_family = "Courier New, monospace".into();
        t.src = Some(9);
        page.texts.push(t);
        let svg = page_to_svg(&page);
        assert!(svg.contains("<rect x=\"50\" y=\"100\" width=\"50\" height=\"10\" data-src=\"3\" fill=\"#000000\"/>"));
        assert!(svg.contains(
            "<text x=\"10\" y=\"20\" font-size=\"240\" font-family=\"Courier New, monospace\" data-src=\"9\" fill=\"#000000\">a&lt;b</text>"
        ));
    }

    #[test]
    fn width_fit() {
        let mut page = Page::default();
        page.texts.push(TextRun::new(0, 100, "abcdef"));
        page.texts.push(TextRun::new(800, 100, "next"));
        let svg = page_to_svg(&page);
        assert!(svg.contains("textLength=\"728\" lengthAdjust=\"spacingAndGlyphs\""), "{svg}");
    }

    #[test]
    fn cmyk_and_recolor() {
        let mut page = Page::default();
        let mut img = ImageRef::new(0, 0, 10, 10, "image/png", Arc::new(vec![1]));
        img.bands = Some(Arc::new(vec![vec![0xFF, 0xD8]; 4]));
        page.images.push(img);
        let mut glyph = ImageRef::new(0, 0, 10, 10, "image/png", Arc::new(vec![1]));
        glyph.recolor = Some("#ff0000".into());
        page.images.push(glyph);
        let svg = page_to_svg(&page);
        assert!(svg.contains("id=\"ink-k\""));
        assert!(svg.contains("mix-blend-mode:multiply"));
        assert!(svg.contains("filter=\"url(#glyphink-ff0000)\""));
    }
}
