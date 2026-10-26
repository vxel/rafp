//! Build check on the readAFP demo samples (MIT, see
//! `tests/samples/LICENSE-readAFP`): each file inspects and renders. They
//! live with the tests only and are not shipped in the application.

use afp_core::codepages::Codec;
use afp_core::inspect::inspect;
use afp_core::render_svg::page_to_svg;

fn sample(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/samples/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

#[test]
fn every_sample_inspects_and_renders() {
    for (name, pages, has) in [
        ("health_coverage.afp", 1, "text"),
        ("goca_demo.afp", 1, "graphics"),
        ("ioca_image.afp", 1, "images"),
        ("foca_font.afp", 2, "images"),
        ("bcoca_qr.afp", 1, "images"),
        ("overlay_demo.afp", 1, "text"),
    ] {
        let ins = inspect(&sample(name), Codec::cp500(), false).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(ins.pages.len(), pages, "{name}");
        let p = &ins.pages[0];
        let present = match has {
            "text" => !p.texts.is_empty(),
            "graphics" => !p.graphics.is_empty(),
            _ => !p.images.is_empty(),
        };
        assert!(present, "{name}: no {has}");
        assert!(page_to_svg(p).starts_with("<svg"), "{name}");
    }
}

#[test]
fn overlay_is_composited_under_the_page() {
    let ins = inspect(&sample("overlay_demo.afp"), Codec::cp500(), false).unwrap();
    assert!(ins.pages[0].plain_text().contains("ACME ASSURANCE"));
}

#[cfg(feature = "raster")]
#[test]
fn samples_rasterize() {
    use afp_core::raster::Rasterizer;
    let r = Rasterizer::new();
    for name in ["health_coverage.afp", "ioca_image.afp", "goca_demo.afp"] {
        let ins = inspect(&sample(name), Codec::cp500(), false).unwrap();
        let tree = r.parse(&page_to_svg(&ins.pages[0])).unwrap();
        assert!(r.render(&tree, 0.05).is_some(), "{name}");
    }
}

#[cfg(feature = "pdf")]
#[test]
fn samples_export_pdf() {
    use afp_core::raster::Rasterizer;
    let r = Rasterizer::new();
    for name in ["health_coverage.afp", "foca_font.afp", "goca_demo.afp", "bcoca_qr.afp"] {
        let ins = inspect(&sample(name), Codec::cp500(), false).unwrap();
        let pdf = afp_core::pdf::pages_to_pdf(&ins.pages, name, &r).unwrap();
        assert!(pdf.starts_with(b"%PDF-"), "{name}");
        let pages = pdf.windows(10).filter(|w| w == b"/Type /Pag" || w == b"/Type/Page").count();
        assert!(pages >= ins.pages.len(), "{name}: {pages} page objects for {} pages", ins.pages.len());
    }
}
