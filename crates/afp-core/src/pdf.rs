//! PDF export (feature `pdf`): each page's SVG, parsed like the on-screen
//! render, is converted by krilla into vector PDF content with embedded,
//! selectable text.

use std::fmt;

use krilla::geom::Size;
use krilla::page::PageSettings;
use krilla::{Document, metadata::Metadata};
use krilla_svg::{SurfaceExt, SvgSettings};

use crate::model::Page;
use crate::raster::{Rasterizer, usvg};
use crate::render_svg::page_to_svg;

/// Why a PDF could not be produced.
#[derive(Debug)]
pub enum PdfError {
    /// A page's SVG did not parse (1-based page number).
    Svg { page: usize, source: usvg::Error },
    /// A page has no area (1-based page number).
    EmptyPage(usize),
    /// A page's drawing could not be converted (1-based page number).
    Convert(usize),
    /// The PDF could not be assembled.
    Write(String),
}

impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PdfError::Svg { page, source } => write!(f, "page {page}: {source}"),
            PdfError::EmptyPage(page) => write!(f, "page {page} has no area"),
            PdfError::Convert(page) => write!(f, "page {page}: cannot convert"),
            PdfError::Write(e) => write!(f, "cannot write the PDF: {e}"),
        }
    }
}

impl std::error::Error for PdfError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PdfError::Svg { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// A PDF of `pages`, one PDF page each, at the pages' physical size.
pub fn pages_to_pdf(pages: &[Page], title: &str, rasterizer: &Rasterizer) -> Result<Vec<u8>, PdfError> {
    let mut doc = Document::new();
    doc.set_metadata(Metadata::new().title(title.to_string()).creator("rafp".to_string()));
    for (i, page) in pages.iter().enumerate() {
        let number = i + 1;
        let tree = rasterizer.parse(&page_to_svg(page)).map_err(|source| PdfError::Svg { page: number, source })?;
        let upi = page.units_per_inch.max(1) as f32;
        let points = |units: i64| units.max(1) as f32 / upi * 72.0;
        let size = Size::from_wh(points(page.width), points(page.height)).ok_or(PdfError::EmptyPage(number))?;
        let mut pdf_page = doc.start_page_with(PageSettings::new(size));
        let mut surface = pdf_page.surface();
        surface.draw_svg(&tree, size, SvgSettings::default()).ok_or(PdfError::Convert(number))?;
        surface.finish();
        pdf_page.finish();
    }
    doc.finish().map_err(|e| PdfError::Write(format!("{e:?}")))
}
