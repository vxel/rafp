//! Read and inspect AFP (Advanced Function Presentation / MO:DCA) files.
//!
//! Initially ported from readAFP: [`parser`] splits a file into structured
//! fields, [`inspect`] turns them into readable inspector rows, and
//! [`ptoca::extract_pages`] builds a page model that [`render_svg`] draws as
//! SVG. [`dump`] gives a JSON view of it all. With the `raster` feature,
//! `raster` turns pages into pixels; with `pdf`, `pdf` exports them as PDF.

pub(crate) mod bcoca;
pub(crate) mod ccitt;
pub(crate) mod cff;
pub mod codepages;
pub mod dump;
pub mod foca;
pub(crate) mod gcgid;
pub(crate) mod goca;
pub(crate) mod imimage;
pub mod inspect;
pub(crate) mod ioca;
pub mod model;
pub(crate) mod outline;
pub mod parser;
#[cfg(feature = "pdf")]
pub mod pdf;
pub(crate) mod png;
pub mod ptoca;
#[cfg(feature = "raster")]
pub mod raster;
pub mod render_svg;
pub(crate) mod triplets;
pub(crate) mod type1;
pub(crate) mod util;
