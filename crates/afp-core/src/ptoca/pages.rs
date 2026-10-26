//! Page extraction: one walk over the structured fields that builds a page
//! model per BPG...EPG, captures overlays, image, IM image, bar code and
//! graphics objects, and maps the fonts the text uses.

use super::*;

/// Walk a parsed document and build a page model per BPG...EPG.
///
/// PTX fields outside any page bracket flow onto implicit pages appended
/// after the bracketed ones. Image objects are decoded wherever they
/// appear: inline on a page they are placed directly; in resources they
/// are indexed by name for IOB/IPS inclusion; in page-less files each
/// becomes its own page. Font resource files get specimen sheets.
pub fn extract_pages(fields: &[StructuredField], codepage: Codec, embed_small_fonts: bool) -> Vec<Page> {
    let mut builder = PageBuilder::new(fields, codepage, embed_small_fonts);
    for field in fields {
        builder.field(field);
    }
    builder.finish(fields)
}

/// The fonts and code pages embedded in the file, indexed by name for the
/// MCF fields that map them to local ids.
struct FontResources {
    code_pages: HashMap<String, CodePageMap>,
    fonts: Vec<Font>,
    glyphs: HashMap<String, Arc<HashMap<String, RasterGlyph>>>,
    /// Character set → (resolution, point size, relative metrics).
    metrics: HashMap<String, (i64, f64, bool)>,
    outlines: HashMap<String, (Arc<IndexMap<String, crate::outline::Glyph>>, i64)>,
    typefaces: HashMap<String, String>,
    weights: HashMap<String, u8>,
    /// Coded-font name → (character set, code page).
    coded_fonts: IndexMap<String, (String, String)>,
    /// The same, keyed without the rotation selector (2nd character).
    coded_fonts_norm: HashMap<String, (String, String)>,
}

impl FontResources {
    fn collect(fields: &[StructuredField]) -> FontResources {
        let mut res = FontResources {
            code_pages: scan_code_pages(fields),
            fonts: parse_fonts(fields),
            glyphs: HashMap::new(),
            metrics: HashMap::new(),
            outlines: HashMap::new(),
            typefaces: HashMap::new(),
            weights: HashMap::new(),
            coded_fonts: parse_coded_fonts(fields),
            coded_fonts_norm: HashMap::new(),
        };
        for font in res.fonts.iter().filter(|f| !f.name.is_empty()) {
            if !font.glyphs.is_empty() {
                let map: HashMap<String, RasterGlyph> =
                    font.glyphs.iter().map(|g| (g.gcgid.clone(), g.clone())).collect();
                res.glyphs.insert(font.name.clone(), Arc::new(map));
                res.metrics.insert(font.name.clone(), (font.resolution, font.point_size, font.relative_metrics));
            }
            if !font.outline_glyphs.is_empty() {
                res.outlines.insert(font.name.clone(), (Arc::new(font.outline_glyphs.clone()), font.units_per_em));
            }
            if !font.typeface.is_empty() {
                res.typefaces.insert(font.name.clone(), font.typeface.clone());
            }
            if font.weight_class != 0 {
                res.weights.insert(font.name.clone(), font.weight_class);
            }
        }
        for (name, pair) in &res.coded_fonts {
            if let Some(norm) = rotation_insensitive(name) {
                res.coded_fonts_norm.entry(norm).or_insert_with(|| pair.clone());
            }
        }
        res
    }

    /// (character set, code page) of a coded font, also when it is cited
    /// with another rotation selector.
    fn coded_font(&self, name: &str) -> Option<&(String, String)> {
        self.coded_fonts.get(name).or_else(|| rotation_insensitive(name).and_then(|n| self.coded_fonts_norm.get(&n)))
    }

    fn is_embedded(&self, char_set: &str) -> bool {
        self.glyphs.get(char_set).is_some_and(|g| !g.is_empty()) || self.outlines.contains_key(char_set)
    }
}

/// The object area (OBD / OBP) of the object being captured.
#[derive(Default, Clone, Copy)]
struct ObjectArea<'a> {
    obd: Option<&'a [u8]>,
    obp: Option<&'a [u8]>,
}

/// An IOCA image object being captured (BIM...EIM).
struct ImageCapture {
    ipd: Vec<u8>,
    name: Option<String>,
}

/// An IM image object being captured (BII...EII).
struct ImCapture<'a> {
    iid: Option<&'a [u8]>,
    ioc: Option<&'a [u8]>,
    cells: Vec<(Option<Vec<u8>>, Vec<u8>)>,
    icp: Option<Vec<u8>>,
    name: Option<String>,
}

/// A bar code object being captured (BBC...EBC).
struct BarcodeCapture<'a> {
    bdd: &'a [u8],
    bdas: Vec<&'a [u8]>,
}

/// A graphics object being captured (BGR...EGR).
struct GraphicCapture<'a> {
    gdd: &'a [u8],
    gads: Vec<u8>,
}

/// The state of the walk over the fields.
struct PageBuilder<'a> {
    fonts: FontResources,
    sh: SharedFonts,
    /// Local ids whose font was set by an MDR; a later MCF must not clobber them.
    mdr_lids: HashSet<u8>,
    pages: Vec<Page>,
    /// The page (or overlay) being built, with its text state.
    current: Option<(Page, TextState)>,
    /// Loose text outside any page bracket.
    implicit: Option<(Page, TextState)>,
    /// Page size from a PGD seen outside a page, for the pages that follow.
    pgd_default: Option<(i64, i64, i64)>,
    /// Object container bytes by name, shared so every IOB placing an
    /// object reuses them.
    resources: HashMap<String, Arc<Vec<u8>>>,
    container: Option<String>,
    image_resources: HashMap<String, ImageObject>,
    /// Image objects outside any page.
    loose_images: Vec<ImageObject>,
    resource_name: Option<String>,
    overlays: Overlays,
    mpo_map: IndexMap<u8, String>,
    /// Name of the overlay being captured (BMO...EMO).
    overlay_name: Option<String>,
    area: ObjectArea<'a>,
    image: Option<ImageCapture>,
    im_image: Option<ImCapture<'a>>,
    barcode: Option<BarcodeCapture<'a>>,
    graphic: Option<GraphicCapture<'a>>,
}

fn new_page(pgd: Option<(i64, i64, i64)>) -> Page {
    match pgd {
        Some((w, h, u)) => Page::with_size(w, h, u),
        None => Page::default(),
    }
}

impl<'a> PageBuilder<'a> {
    fn new(fields: &[StructuredField], codepage: Codec, embed_small_fonts: bool) -> PageBuilder<'a> {
        PageBuilder {
            fonts: FontResources::collect(fields),
            sh: SharedFonts::new(codepage, embed_small_fonts),
            mdr_lids: HashSet::new(),
            pages: Vec::new(),
            current: None,
            implicit: None,
            pgd_default: None,
            resources: HashMap::new(),
            container: None,
            image_resources: HashMap::new(),
            loose_images: Vec::new(),
            resource_name: None,
            overlays: Overlays::default(),
            mpo_map: IndexMap::new(),
            overlay_name: None,
            area: ObjectArea::default(),
            image: None,
            im_image: None,
            barcode: None,
            graphic: None,
        }
    }

    fn capturing_object(&self) -> bool {
        self.image.is_some() || self.barcode.is_some() || self.graphic.is_some()
    }

    fn field(&mut self, f: &'a StructuredField) {
        let d = &f.data[..];
        match f.sf_id {
            // Object containers: their bytes, by name, for IOB.
            sf::BOC => self.container = f.token_name(),
            sf::EOC => self.container = None,
            sf::OCD => {
                if let Some(c) = &self.container {
                    Arc::make_mut(self.resources.entry(c.clone()).or_default()).extend_from_slice(d);
                }
            }
            // Resources and page segments: keep the outer non-blank name.
            sf::BRS | sf::BPS => {
                let name = py_strip(&f.token_name().unwrap_or_default()).to_string();
                if !name.is_empty() {
                    self.resource_name = Some(name);
                }
            }
            sf::ERS | sf::EPS => self.resource_name = None,
            sf::BIM => {
                self.image = Some(ImageCapture { ipd: Vec::new(), name: f.token_name() });
                self.area = ObjectArea::default();
            }
            sf::EIM if self.image.is_some() => self.end_image(),
            sf::OBD if self.capturing_object() => self.area.obd = Some(d),
            sf::OBP if self.capturing_object() => self.area.obp = Some(d),
            sf::IPD => {
                if let Some(image) = &mut self.image {
                    image.ipd.extend_from_slice(d);
                }
            }
            sf::BII => {
                self.im_image =
                    Some(ImCapture { iid: None, ioc: None, cells: Vec::new(), icp: None, name: f.token_name() });
            }
            sf::IID | sf::IOC | sf::ICP | sf::IRD => {
                if let Some(im) = &mut self.im_image {
                    match f.sf_id {
                        sf::IID => im.iid = Some(d),
                        sf::IOC => im.ioc = Some(d),
                        sf::ICP => im.icp = Some(d.to_vec()),
                        _ => im.cells.push((im.icp.take(), d.to_vec())),
                    }
                }
            }
            sf::EII if self.im_image.is_some() => self.end_im_image(),
            sf::BBC => {
                self.barcode = Some(BarcodeCapture { bdd: &[], bdas: Vec::new() });
                self.area = ObjectArea::default();
            }
            sf::BDD | sf::BDA => {
                if let Some(bar) = &mut self.barcode {
                    if f.sf_id == sf::BDD {
                        bar.bdd = d;
                    } else {
                        bar.bdas.push(d);
                    }
                }
            }
            sf::EBC if self.barcode.is_some() => self.end_barcode(),
            sf::BGR => {
                self.graphic = Some(GraphicCapture { gdd: &[], gads: Vec::new() });
                self.area = ObjectArea::default();
            }
            sf::GDD | sf::GAD => {
                if let Some(g) = &mut self.graphic {
                    if f.sf_id == sf::GDD {
                        g.gdd = d;
                    } else {
                        g.gads.extend_from_slice(d);
                    }
                }
            }
            sf::EGR if self.graphic.is_some() => self.end_graphic(),
            sf::IOB => {
                if let Some((page, _)) = self.current.as_mut()
                    && let Some(img) = parse_iob(d, &self.resources, &self.image_resources, page.units_per_inch)
                {
                    page.push_image(img);
                }
            }
            sf::MPO => self.mpo_map.extend(parse_mpo(d)),
            sf::IPO => {
                if let Some((page, _)) = self.current.as_mut()
                    && let Some(overlay) = self.overlays.resolve(d, &self.mpo_map)
                {
                    include_overlay(page, overlay, d);
                }
            }
            sf::IPS => {
                if let Some((page, _)) = self.current.as_mut() {
                    include_segment(page, &self.image_resources, d);
                }
            }
            // An overlay is captured like a page, then stored by name.
            sf::BMO => {
                self.current = Some((new_page(self.pgd_default), TextState::new()));
                self.overlay_name = Some(py_strip(&f.token_name().unwrap_or_default()).to_string());
            }
            sf::EMO => self.end_overlay(),
            sf::BPG => self.current = Some((new_page(self.pgd_default), TextState::new())),
            sf::EPG => {
                if let Some((mut page, _)) = self.current.take() {
                    estimate_font_sizes(&mut page, &self.sh.fonts);
                    self.pages.push(page);
                }
            }
            sf::MDR => {
                for (lid, info) in parse_mdr_fonts(d) {
                    self.mdr_lids.insert(lid);
                    self.sh.fonts.insert(lid, info);
                }
            }
            sf::MCF2 | sf::MCF1 => self.map_coded_fonts(d, f.sf_id == sf::MCF1),
            sf::PGD if d.len() >= 12 => {
                let parsed = parse_pgd(d);
                match self.current.as_mut() {
                    Some((page, _)) => (page.width, page.height, page.units_per_inch) = parsed,
                    None => self.pgd_default = Some(parsed),
                }
            }
            sf::PTX => self.text(f),
            _ => {}
        }
    }

    /// File a decoded image object: placed on the current page, or indexed
    /// by name for later inclusion and kept as a loose image.
    fn file_object(
        &mut self,
        obj: ImageObject,
        name: Option<String>,
        place: impl FnOnce(&ImageObject, i64) -> ImageRef,
    ) {
        if let Some((page, _)) = self.current.as_mut() {
            let img = place(&obj, page.units_per_inch);
            page.push_image(img);
            return;
        }
        for key in [&name, &self.resource_name].into_iter().flatten() {
            if !key.is_empty() {
                self.image_resources.entry(key.clone()).or_insert_with(|| obj.clone());
            }
        }
        self.loose_images.push(obj);
    }

    fn end_image(&mut self) {
        let Some(image) = self.image.take() else { return };
        let Some(obj) = finish_image_object(&image.ipd, self.area.obd, self.area.obp) else { return };
        self.file_object(obj, image.name, |obj, upi| {
            let mut img = ImageRef::new(
                obj.x,
                obj.y,
                scale(obj.width, obj.upi, upi),
                scale(obj.height, obj.upi, upi),
                &obj.mime,
                obj.blob.clone(),
            );
            img.bands = obj.bands.clone();
            img.crisp = obj.crisp;
            img
        });
    }

    fn end_im_image(&mut self) {
        let Some(capture) = self.im_image.take() else { return };
        let Some(im) = capture.iid.and_then(|iid| decode_im_image(iid, &capture.cells)) else { return };
        let (x, y) = capture.ioc.map(parse_ioc_origin).unwrap_or((0, 0));
        let res = if im.resolution != 0 { im.resolution } else { 1440 };
        let obj = ImageObject {
            mime: "image/png".into(),
            blob: Arc::new(im.png),
            upi: res,
            width: im.width as i64,
            height: im.height as i64,
            x,
            y,
            bands: None,
            crisp: true,
            scaled_pos: true,
        };
        self.file_object(obj, capture.name, |obj, upi| {
            let mut img = ImageRef::new(
                scale(obj.x, res, upi),
                scale(obj.y, res, upi),
                scale(obj.width, res, upi),
                scale(obj.height, res, upi),
                &obj.mime,
                obj.blob.clone(),
            );
            img.crisp = true;
            img
        });
    }

    fn end_barcode(&mut self) {
        let Some(bar_object) = self.barcode.take() else { return };
        let Some((page, _)) = self.current.as_mut() else { return };
        let (ox, oy) = self.area.obp.map(parse_obp).unwrap_or((0, 0));
        let upi = page.units_per_inch;
        for bda in &bar_object.bdas {
            let Some(bar) = parse_barcode(bar_object.bdd, bda) else { continue };
            let Some((png, modules)) = barcode_png(&bar) else { continue };
            let side = py_round(modules as f64 * bar.module_mils as f64 * upi as f64 / 1000.0);
            let mut img = ImageRef::new(
                ox + scale(bar.x, bar.upi, upi),
                oy + scale(bar.y, bar.upi, upi),
                side,
                side,
                "image/png",
                Arc::new(png),
            );
            img.crisp = true;
            page.push_image(img);
        }
    }

    fn end_graphic(&mut self) {
        let Some(graphic) = self.graphic.take() else { return };
        let Some((page, _)) = self.current.as_mut() else { return };
        if graphic.gdd.is_empty() {
            return;
        }
        let Some(goca) = draw_goca(graphic.gdd, &graphic.gads) else { return };
        let (ox, oy) = self.area.obp.map(parse_obp).unwrap_or((0, 0));
        let (obd_upi, ow, oh) = self.area.obd.map(parse_obd).unwrap_or((None, 0, 0));
        let upi = page.units_per_inch;
        let from = obd_upi.unwrap_or(upi);
        page.push_graphic(VectorGraphic {
            x: ox,
            y: oy,
            width: scale(ow, from, upi),
            height: scale(oh, from, upi),
            graphic: Arc::new(goca),
            rotate: None,
        });
    }

    /// EMO: store the captured overlay under its name and its resource name.
    fn end_overlay(&mut self) {
        let overlay_name = self.overlay_name.take();
        let Some((mut page, _)) = self.current.take() else { return };
        let Some(overlay_name) = overlay_name else { return };
        estimate_font_sizes(&mut page, &self.sh.fonts);
        let idx = self.overlays.pages.len();
        let mut stored = false;
        let resource = self.resource_name.as_deref().map(py_strip).unwrap_or("").to_string();
        for key in [overlay_name, resource] {
            if !key.is_empty() {
                self.overlays.by_name.insert(key, idx);
                stored = true;
            }
        }
        if stored {
            self.overlays.pages.push(page);
        }
    }

    /// PTX: run the text controls on the current page, or on the implicit
    /// page of loose text.
    fn text(&mut self, f: &StructuredField) {
        let pgd_default = self.pgd_default;
        let (page, state) = match self.current.as_mut() {
            Some((p, s)) => (p, s),
            None => {
                let (p, s) = self.implicit.get_or_insert_with(|| {
                    let p = new_page(pgd_default);
                    let mut s = TextState::new();
                    s.wrap_width = Some(p.width - 480);
                    s.i = 240;
                    s.b = 320;
                    (p, s)
                });
                (p, s)
            }
        };
        state.field_offset = Some(f.offset);
        for cs in parse_control_sequences(&f.data) {
            state.apply(&cs, page, &self.sh);
        }
    }

    /// MCF: map each local id to its coded font, decoder, embedded glyphs
    /// and substitute font, and note when a substitute is drawn.
    fn map_coded_fonts(&mut self, d: &[u8], fmt1: bool) {
        for (lid, cp) in parse_mcf_codepages(d, fmt1) {
            if let Some(c) = cp.codec {
                self.sh.font_codepages.insert(lid, c);
            }
        }
        let mcf_cf = mcf_coded_fonts(d, fmt1);
        for (lid, (cp_name, cs_name)) in mcf_font_resources(d, fmt1) {
            let (mut cp_name, mut cs_name) = (cp_name.unwrap_or_default(), cs_name.unwrap_or_default());
            if cs_name.is_empty()
                && let Some(cf_name) = mcf_cf.get(&lid)
                && let Some((cs, cp)) = self.fonts.coded_font(cf_name)
            {
                cs_name = cs.clone();
                cp_name = cp.clone();
                if let Some(codec) = codec_for_codepage_name(&cp_name) {
                    self.sh.font_codepages.entry(lid).or_insert(codec);
                }
            }
            self.map_embedded_font(lid, &cp_name, &cs_name);
            self.map_substitute_font(lid, &cs_name);
        }
    }

    /// Draw text of `lid` with the file's own glyphs when both its code page
    /// map and its character set are available.
    fn map_embedded_font(&mut self, lid: u8, cp_name: &str, cs_name: &str) {
        let fonts = &self.fonts;
        let mut cp_map: Option<Arc<CodePageMap>> =
            fonts.code_pages.get(cp_name).filter(|m| !m.is_empty()).map(|m| Arc::new(m.clone()));
        if let Some(m) = &cp_map {
            self.sh.font_cp_maps.insert(lid, m.clone());
        }
        let glyphs = fonts.glyphs.get(cs_name);
        let outlines = fonts.outlines.get(cs_name);
        // External code page but embedded character set: bridge byte → GCGID
        // through the codec.
        if cp_map.is_none()
            && let Some(codec) = self.sh.font_codepages.get(&lid).copied()
        {
            let mut gset: HashSet<String> = HashSet::new();
            if let Some(g) = glyphs {
                gset.extend(g.keys().cloned());
            }
            if let Some((o, _)) = outlines {
                gset.extend(o.keys().cloned());
            }
            let bridged = bridge_code_page(codec, &gset);
            if !bridged.is_empty() {
                cp_map = Some(Arc::new(bridged));
            }
        }
        let Some(cp_map) = cp_map else { return };
        let codec = self.sh.font_codepages.get(&lid).copied();
        if let Some(glyphs) = glyphs.filter(|g| !g.is_empty()) {
            let ref_height = glyphs.values().map(|g| g.height as i64).max().unwrap_or(1);
            let (resolution, point_size, relative_metrics) =
                fonts.metrics.get(cs_name).copied().unwrap_or((0, 0.0, false));
            self.sh.embedded_text_fonts.insert(
                lid,
                EmbeddedFont {
                    cp_map,
                    glyphs: glyphs.clone(),
                    ref_height,
                    outline_glyphs: None,
                    units_per_em: 0,
                    codec,
                    resolution,
                    point_size,
                    relative_metrics,
                },
            );
        } else if let Some((outline_glyphs, em)) = outlines.filter(|(o, _)| !o.is_empty()) {
            self.sh.embedded_text_fonts.insert(
                lid,
                EmbeddedFont {
                    cp_map,
                    glyphs: Arc::new(HashMap::new()),
                    ref_height: 1,
                    outline_glyphs: Some(outline_glyphs.clone()),
                    units_per_em: *em,
                    codec,
                    resolution: 0,
                    point_size: 0.0,
                    relative_metrics: false,
                },
            );
        }
    }

    /// The substitute font for `lid`: the embedded character set's typeface,
    /// else inferred from the coded-font name; plus a fidelity note when
    /// the text is drawn with it.
    fn map_substitute_font(&mut self, lid: u8, cs_name: &str) {
        let fonts = &self.fonts;
        let sub =
            fonts.typefaces.get(cs_name).and_then(|t| substitute_font(t)).or_else(|| coded_font_substitute(cs_name));
        let weight_class = fonts.weights.get(cs_name).copied().unwrap_or(0);
        let psize = fonts.metrics.get(cs_name).map(|m| m.1).unwrap_or(0.0);
        let size_pt = if psize != 0.0 { Some(psize) } else { coded_font_point_size(cs_name) };
        let from_mdr = self.mdr_lids.contains(&lid);
        if let (Some((family, weight)), false) = (sub, from_mdr) {
            let weight = if weight_class >= 7 { FontWeight::Bold } else { weight };
            self.sh
                .fonts
                .insert(lid, FontInfo { family: family.into(), weight, size: None, size_pt, notes: Vec::new() });
        } else if !from_mdr && !self.sh.embedded_text_fonts.contains_key(&lid) {
            // External font with no inferable typeface: register it so the run
            // is still flagged.
            self.sh.fonts.insert(lid, FontInfo { size_pt, ..FontInfo::default() });
        }
        if self.sh.embedded_text_fonts.contains_key(&lid) {
            return;
        }
        let Some(info) = self.sh.fonts.get_mut(&lid) else { return };
        let label = typeface_label(cs_name, &fonts.typefaces);
        let fam = family_label(&info.family).to_string();
        let disp = py_strip(cs_name);
        let disp = if disp.is_empty() { "?" } else { disp };
        let msg = if fonts.is_embedded(cs_name) {
            let for_label = if label.is_empty() { String::new() } else { format!(" for {label}") };
            format!("Drawn in substitute font {fam}{for_label} — the embedded glyphs aren't used at this size.")
        } else {
            let who = if label.is_empty() { format!("“{disp}”") } else { format!("{label} (“{disp}”)") };
            format!(
                "{who} is referenced but not embedded in this file — rendered with the system {fam}; glyph widths are approximated."
            )
        };
        if !info.notes.iter().any(|n| n.msg == msg) {
            info.notes.push(FidelityNote::new(NoteCategory::Font, msg));
        }
    }

    /// The pages, after the loose text, and the fallbacks for files without
    /// pages: font specimens, standalone overlays, standalone images.
    fn finish(mut self, fields: &[StructuredField]) -> Vec<Page> {
        if let Some((mut page, _)) = self.implicit.take()
            && (!page.texts.is_empty() || !page.rules.is_empty())
        {
            estimate_font_sizes(&mut page, &self.sh.fonts);
            self.pages.extend(paginate_implicit(page));
        }
        let mut pages = self.pages;
        if pages.is_empty() {
            // Font resource files get a specimen sheet of their glyphs.
            let specimen = specimen::font_specimen_pages(&self.fonts.fonts);
            if !specimen.is_empty() {
                return specimen;
            }
        }
        if pages.is_empty() && !self.overlays.pages.is_empty() && !fields.iter().any(|f| f.sf_id == sf::BDT) {
            // Standalone overlay resources: each overlay as its own page, in
            // the order an IPO name index would list them.
            let mut seen = HashSet::new();
            for &idx in self.overlays.by_name.values() {
                if seen.insert(idx) && self.overlays.pages[idx].has_content() {
                    pages.push(self.overlays.pages[idx].clone());
                }
            }
        }
        if pages.is_empty() {
            // Standalone image objects: each at its own extent.
            for obj in &self.loose_images {
                let mut page = Page::with_size(obj.width, obj.height, obj.upi);
                let mut img = ImageRef::new(0, 0, obj.width, obj.height, &obj.mime, obj.blob.clone());
                img.bands = obj.bands.clone();
                page.push_image(img);
                pages.push(page);
            }
        }
        annotate_image_notes(&mut pages);
        pages
    }
}
