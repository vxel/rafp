//! Integration tests on the public sample corpus, with the reference values
//! of the original readAFP test suite.
//!
//! Test data lives outside the repository: `RAFP_TESTDATA` (default
//! `local/testdata`). Each test names its public file explicitly
//! and is skipped when the file is not available.

use std::path::PathBuf;

use afp_core::codepages::Codec;
use afp_core::foca::{parse_coded_fonts, parse_fonts};
use afp_core::inspect::inspect;
use afp_core::model::Page;
use afp_core::parser::parse_fields;
use afp_core::ptoca::extract_pages;
use afp_core::render_svg::page_to_svg;

fn testdata(rel: &str) -> Option<Vec<u8>> {
    let root = std::env::var_os("RAFP_TESTDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../local/testdata"));
    let path = root.join(rel);
    match std::fs::read(&path) {
        Ok(d) => Some(d),
        Err(_) => {
            eprintln!("skipping: {} not available", path.display());
            None
        }
    }
}

fn pages(rel: &str) -> Option<Vec<Page>> {
    let data = testdata(rel)?;
    let fields = parse_fields(&data).expect("valid AFP");
    Some(extract_pages(&fields, Codec::cp500(), false))
}

fn png_size(png: &[u8]) -> (u32, u32) {
    (u32::from_be_bytes(png[16..20].try_into().unwrap()), u32::from_be_bytes(png[20..24].try_into().unwrap()))
}

#[test]
fn fop_images_places_ioca_images() {
    let Some(pages) = pages("fop-pairs/images.afp") else { return };
    assert_eq!(pages.len(), 5);
    assert_eq!(pages[0].images.len(), 5);
    let img = &pages[0].images[0];
    assert_eq!((img.x, img.y, img.width, img.height), (706, 460, 737, 320));
    assert_eq!(img.mime, "image/png");
    assert_eq!(png_size(&img.data), (221, 96));
}

#[test]
fn ipd_span_zero_length_image_data() {
    let Some(pages) = pages("github-samples/afplib/IPDSpan.afp") else { return };
    assert_eq!(pages.len(), 1);
    assert_eq!((pages[0].width, pages[0].height), (1344, 30));
}

#[test]
fn resource_name_match_resolves_iob() {
    let Some(pages) = pages("github-samples/fop/resource_name_match.afp") else { return };
    let img = pages.iter().flat_map(|p| &p.images).next().expect("an image");
    assert_eq!(png_size(&img.data), (608, 200));
}

#[test]
fn cmyk_bands_composited() {
    let Some(pages) = pages("github-samples/afplib/bim.afp") else { return };
    let img = pages.iter().flat_map(|p| &p.images).find(|i| i.bands.is_some()).expect("a CMYK image");
    assert_eq!(img.bands.as_ref().unwrap().len(), 4);
    assert_eq!(img.notes[0].cat, afp_core::model::NoteCategory::Image);
    let svg = page_to_svg(&pages[0]);
    assert!(svg.contains("ink-k") && svg.contains("mix-blend-mode:multiply"));
}

#[test]
fn qr_sample_is_crisp() {
    let Some(pages) = pages("bcoca_sample.afp") else { return };
    let img = &pages[0].images[0];
    assert!(img.crisp);
    assert!(page_to_svg(&pages[0]).contains("image-rendering:pixelated"));
}

#[test]
fn data_matrix_is_not_drawn() {
    let Some(pages) = pages("alpheus-corpus/external/afplib_ende.afp") else { return };
    assert!(pages.iter().all(|p| p.images.is_empty()));
}

#[test]
fn outline_font_metrics_and_names() {
    let Some(data) = testdata("github-samples/afplib/C0X00006.afp") else { return };
    let fields = parse_fields(&data).unwrap();
    let fonts = parse_fonts(&fields);
    let font = &fonts[0];
    assert!(font.is_outline() && !font.is_raster());
    assert_eq!(font.tech_label(), "Adobe Type 1 (PFB) outline");
    assert_eq!(font.orientations, 4);
    let width = |g: &str| font.chars.iter().find(|c| c.gcgid == g).unwrap().char_increment;
    assert_eq!((width("LM010000"), width("LO010000"), width("LF010000")), (833, 556, 278));
    let name = |g: &str| font.chars.iter().find(|c| c.gcgid == g).unwrap().name.clone();
    assert_eq!((name("LA010000"), name("GA010000"), name("GD010000")), ("a".into(), "alpha".into(), "delta".into()));
    assert_eq!(font.units_per_em, 1000);
    // Type 1 advances match the FNI increments.
    for cm in &font.chars {
        if let Some(g) = font.outline_glyphs.get(&cm.gcgid) {
            assert_eq!(g.advance as i64, cm.char_increment, "{}", cm.gcgid);
        }
    }
    let pages = extract_pages(&fields, Codec::cp500(), false);
    assert_eq!(pages.len(), 1);
    assert!(pages[0].texts[0].text.starts_with("Embedded outline font:"));
    assert!(pages[0].graphics.len() > 100);
    assert!(pages[0].texts.iter().any(|t| t.font_family == "Consolas"));
}

#[test]
fn raster_font_specimen() {
    let Some(pages) = pages("foca_sample.afp") else { return };
    let page = &pages[0];
    assert!(!page.images.is_empty());
    assert_eq!(page.texts.len(), page.images.len() + 1);
    assert!(page.texts[0].text.starts_with("Embedded raster font:"));
    assert!(page.texts.iter().all(|t| !t.fit));
    assert!(!page_to_svg(page).contains("textLength"));
}

#[test]
fn coded_font_chain() {
    let Some(data) = testdata("coded_font_sample.afp") else { return };
    let fields = parse_fields(&data).unwrap();
    let cf = parse_coded_fonts(&fields);
    assert_eq!(cf.get("X0TSTB00"), Some(&("C0AAAB00".to_string(), "T1TSTB00".to_string())));
}

#[test]
fn cff_document_draws_outline_paths() {
    let Some(pages) = pages("cff_document_sample.afp") else { return };
    let g = &pages[0].graphics[0];
    assert_eq!(g.graphic.gps_w, 1900);
    assert_eq!(g.graphic.svg.matches('M').count(), 5);
    assert_eq!(page_to_svg(&pages[0]).matches("<path").count(), 1);
}

#[test]
fn foca_cff_specimen_paths() {
    let Some(pages) = pages("foca_cff_sample.afp") else { return };
    assert_eq!(page_to_svg(&pages[0]).matches("<path").count(), 4);
}

#[test]
fn goca_arcs() {
    let Some(pages) = pages("goca_arc_sample.afp") else { return };
    assert_eq!(pages[0].graphics.len(), 7);
    let arcs = pages[0].graphics.iter().filter(|g| g.graphic.svg.contains(" A ")).count();
    assert!(arcs >= 6);
}

#[test]
fn page_limit_file_keeps_every_page() {
    let Some(pages) = pages("pagelimit_520.afp") else { return };
    assert_eq!(pages.len(), 520);
}

#[test]
fn fop_list_pagination() {
    let Some(pages) = pages("fop-pairs/list.afp") else { return };
    assert_eq!(pages.len(), 12);
    let text: String = pages.iter().map(Page::plain_text).collect::<Vec<_>>().join("\n");
    for n in 1..=17 {
        assert!(text.contains(&format!("{n}")), "item {n}");
    }
}

#[test]
fn health_sample_inspection() {
    let Some(data) = testdata("sample1_health/01_Health_Coverage.afp") else { return };
    let ins = inspect(&data, Codec::cp500(), false).unwrap();
    assert_eq!(ins.rows.len(), 25);
    assert_eq!(ins.pages.len(), 1);
    assert!(ins.pages[0].plain_text().contains("Continuing Health Coverage"));
    assert_eq!(ins.resource_kind, None);
}

#[test]
fn placeholder_corpus_file_is_a_bare_document() {
    let Some(data) = testdata("alpheus-corpus/minimal.afp") else { return };
    let ins = inspect(&data, Codec::cp500(), false).unwrap();
    assert_eq!(ins.rows[0].token, "DOC00001");
    assert!(ins.pages.is_empty());
}
