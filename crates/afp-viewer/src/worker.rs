//! Background work: parsing/inspecting a document and rasterizing pages,
//! so the UI stays responsive on large files.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};
use std::thread;

use afp_core::codepages::Codec;
use afp_core::inspect::{FieldRow, MissingResource, inspect};
use afp_core::model::{Page, ResourceKind};
use afp_core::raster::{BoxF, Rasterizer, crisp_image_resolution, element_boxes, usvg};
use afp_core::render_svg::page_to_svg_with_ids;
use gtk::gio;

use crate::i18n::{gettext, gettext_f, ngettext_f, number};

/// A loaded, inspected document.
pub struct Doc {
    pub name: String,
    pub bytes: Arc<Vec<u8>>,
    pub rows: Vec<FieldRow>,
    pub summary: Vec<(String, usize)>,
    pub pages: Arc<Vec<Page>>,
    pub mcf_note: String,
    pub resource_kind: Option<ResourceKind>,
    pub missing_resources: Vec<MissingResource>,
    /// Index of the first inspector row of each page.
    pub page_first_row: Vec<Option<usize>>,
    /// Inspector row index by structured-field offset.
    pub row_of_offset: HashMap<usize, usize>,
    /// Unique id of this load (render results are matched against it).
    pub generation: u64,
}

impl Doc {
    /// One-line document details: pages, fields, size, declared code pages.
    pub fn summary_line(&self) -> String {
        let (pages, fields, bytes) = (self.pages.len(), self.rows.len(), self.bytes.len());
        let mut parts = vec![
            ngettext_f("{n} page", "{n} pages", pages, &[("n", &number(pages))]),
            ngettext_f("{n} structured field", "{n} structured fields", fields, &[("n", &number(fields))]),
            ngettext_f("{n} byte", "{n} bytes", bytes, &[("n", &number(bytes))]),
        ];
        if !self.mcf_note.is_empty() {
            parts.push(gettext_f("code page from MCF: {pages}", &[("pages", &self.mcf_note)]));
        }
        parts.join(" · ")
    }
}

pub enum LoadResult {
    Loaded(Box<Doc>),
    Failed { name: String, error: String },
}

/// Inspect `bytes` on a worker thread.
pub async fn load_document(
    name: String,
    bytes: Arc<Vec<u8>>,
    codepage: Codec,
    embed_small_fonts: bool,
    generation: u64,
) -> LoadResult {
    let fallback = name.clone();
    gio::spawn_blocking(move || build_doc(name, bytes, codepage, embed_small_fonts, generation))
        .await
        .unwrap_or_else(|_| LoadResult::Failed { name: fallback, error: gettext("The decoder crashed on this file.") })
}

fn build_doc(
    name: String,
    bytes: Arc<Vec<u8>>,
    codepage: Codec,
    embed_small_fonts: bool,
    generation: u64,
) -> LoadResult {
    match inspect(&bytes, codepage, embed_small_fonts) {
        Ok(ins) => {
            let mut page_first_row = vec![None; ins.pages.len()];
            for (i, row) in ins.rows.iter().enumerate() {
                if let Some(p) = row.page
                    && p < page_first_row.len()
                    && page_first_row[p].is_none()
                {
                    page_first_row[p] = Some(i);
                }
            }
            let row_of_offset = ins.rows.iter().enumerate().map(|(i, r)| (r.offset, i)).collect();
            LoadResult::Loaded(Box::new(Doc {
                name,
                bytes,
                rows: ins.rows,
                summary: ins.summary,
                pages: Arc::new(ins.pages),
                mcf_note: ins.mcf_note,
                resource_kind: ins.resource_kind,
                missing_resources: ins.missing_resources,
                page_first_row,
                row_of_offset,
                generation,
            }))
        }
        Err(e) => {
            LoadResult::Failed { name, error: gettext_f("Not a valid AFP file: {error}", &[("error", &e.to_string())]) }
        }
    }
}

/// Element boxes of a rendered page, by element id.
pub type Boxes = Arc<HashMap<String, BoxF>>;

/// A parsed page: (generation, page, render shrinks its pixel-exact images).
type TreeKey = (u64, usize, bool);

/// A request to rasterize one page at a given scale (pixels per L-unit).
pub struct RenderJob {
    pub generation: u64,
    pub pages: Arc<Vec<Page>>,
    pub page: usize,
    pub scale: f32,
}

/// A rasterized page (premultiplied RGBA) plus the page-unit box of every
/// drawable element.
pub struct RenderedPage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub boxes: Boxes,
}

/// The answer to a [`RenderJob`]: the page, or why it could not be drawn.
pub struct RenderResult {
    pub generation: u64,
    pub page: usize,
    pub scale: f32,
    pub outcome: Result<RenderedPage, String>,
}

/// A long-lived rasterizer thread. Only the newest pending job is served.
pub struct Renderer {
    jobs: Sender<RenderJob>,
}

/// Parsed trees of recently shown pages, so zoom changes re-render without
/// re-decoding the SVG and its embedded images.
struct TreeCache {
    rasterizer: Rasterizer,
    trees: Vec<(TreeKey, Arc<usvg::Tree>, Boxes)>,
}

impl TreeCache {
    fn render(&mut self, job: &RenderJob) -> Result<RenderedPage, String> {
        let page = job.pages.get(job.page).ok_or("no such page")?;
        let shrinking = crisp_image_resolution(page).is_some_and(|r| job.scale < r);
        let key = (job.generation, job.page, shrinking);
        let (tree, boxes) = match self.trees.iter().position(|(k, _, _)| *k == key) {
            Some(i) => {
                let entry = self.trees.remove(i);
                let out = (entry.1.clone(), entry.2.clone());
                self.trees.push(entry);
                out
            }
            None => {
                let svg = page_to_svg_with_ids(page);
                let tree = Arc::new(self.rasterizer.parse_for_scale(&svg, shrinking).map_err(|e| e.to_string())?);
                let boxes = Arc::new(element_boxes(&tree));
                self.trees.push((key, tree.clone(), boxes.clone()));
                if self.trees.len() > 6 {
                    self.trees.remove(0);
                }
                (tree, boxes)
            }
        };
        let pixmap = self.rasterizer.render(&tree, job.scale).ok_or("the page is too large to draw")?;
        Ok(RenderedPage { width: pixmap.width(), height: pixmap.height(), rgba: pixmap.take(), boxes })
    }
}

impl Renderer {
    /// Start the thread; results arrive on the returned channel.
    pub fn spawn() -> (Renderer, async_channel::Receiver<RenderResult>) {
        let (job_tx, job_rx) = channel::<RenderJob>();
        let (res_tx, res_rx) = async_channel::unbounded();
        let worker = move || {
            let mut cache = TreeCache { rasterizer: Rasterizer::new(), trees: Vec::new() };
            while let Ok(mut job) = job_rx.recv() {
                while let Ok(newer) = job_rx.try_recv() {
                    job = newer;
                }
                // A decoder panic fails this page only, not the whole session.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cache.render(&job)))
                    .unwrap_or_else(|_| {
                        cache.trees.clear();
                        Err("the renderer crashed on this page".into())
                    });
                if let Err(e) = &outcome {
                    log::warn!("page {}: {e}", job.page + 1);
                }
                let result = RenderResult { generation: job.generation, page: job.page, scale: job.scale, outcome };
                if res_tx.send_blocking(result).is_err() {
                    break;
                }
            }
        };
        thread::Builder::new().name("rafp-render".into()).spawn(worker).expect("render thread");
        (Renderer { jobs: job_tx }, res_rx)
    }

    pub fn request(&self, job: RenderJob) {
        let _ = self.jobs.send(job);
    }
}
