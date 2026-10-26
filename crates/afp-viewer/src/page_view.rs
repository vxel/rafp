//! Render pane: page navigation, zoom, the rasterized page, and the
//! interactive overlays (flash of the selected field's content, fidelity
//! notes, pointer position).

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use afp_core::model::{FidelityNote, NoteCategory, Page};
use afp_core::raster::BoxF;
use gtk::glib::clone;
use gtk::{gdk, gio, glib, prelude::*};

use crate::app::{Event, EventHandler};
use crate::canvas::{PageCanvas, PageGeom};
use crate::i18n::{decimal1, gettext, gettext_f, ngettext_f, safe};
use crate::settings::Zoom;
use crate::worker::{Boxes, Doc, RenderJob, RenderResult, Renderer};

/// How long a flashed element stays highlighted (microseconds).
const FLASH_US: i64 = 1_600_000;

/// Zoom presets of the zoom menu.
const ZOOM_PRESETS: [Zoom; 7] = [
    Zoom::FitWidth,
    Zoom::FitHeight,
    Zoom::Factor(0.5),
    Zoom::Factor(1.0),
    Zoom::Factor(1.5),
    Zoom::Factor(2.0),
    Zoom::Factor(4.0),
];

#[derive(Default)]
struct State {
    doc: Option<Rc<Doc>>,
    page: usize,
    /// What the canvas texture shows: (generation, page, scale).
    shown: Option<(u64, usize, f32)>,
    requested: Option<(u64, usize, f32)>,
    boxes: Option<(u64, usize, Boxes)>,
    /// Content to flash: (page, source field offset, start time).
    flash: Option<(usize, usize, i64)>,
    flash_tick: Option<gtk::TickCallbackId>,
    fidelity_on: bool,
    fonts_only: bool,
    pointer: Option<(f32, f32)>,
}

pub struct PageView {
    pub root: gtk::Box,
    canvas: PageCanvas,
    first: gtk::Button,
    prev: gtk::Button,
    next: gtk::Button,
    last: gtk::Button,
    page_entry: gtk::Entry,
    of_label: gtk::Label,
    zoom_button: gtk::MenuButton,
    fid_toggle: gtk::ToggleButton,
    fid_label: gtk::Label,
    fid_scope: gtk::DropDown,
    summary: gtk::Label,
    pointer_label: gtk::Label,
    renderer: Renderer,
    state: RefCell<State>,
    hovered: Cell<bool>,
    events: RefCell<Option<EventHandler>>,
    /// Folder the save dialogs start in.
    dialog_dir: RefCell<Option<PathBuf>>,
}

fn icon_button(icon: &str, tooltip: String) -> gtk::Button {
    gtk::Button::builder().icon_name(icon).tooltip_text(tooltip).build()
}

fn zoom_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let steps = gio::Menu::new();
    steps.append(Some(&gettext("Zoom In")), Some("win.zoom-in"));
    steps.append(Some(&gettext("Zoom Out")), Some("win.zoom-out"));
    menu.append_section(None, &steps);
    let presets = gio::Menu::new();
    for z in ZOOM_PRESETS {
        let item = gio::MenuItem::new(Some(&z.label()), None);
        item.set_action_and_target_value(Some("win.zoom"), Some(&z.key().to_variant()));
        presets.append_item(&item);
    }
    menu.append_section(None, &presets);
    menu
}

fn export_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let document = gio::Menu::new();
    document.append(Some(&gettext("Document as PDF…")), Some("win.export-pdf"));
    document.append(Some(&gettext("All Text as .txt…")), Some("win.save-text"));
    menu.append_section(None, &document);
    let page = gio::Menu::new();
    page.append(Some(&gettext("Page as PDF…")), Some("win.export-page-pdf"));
    page.append(Some(&gettext("Page as SVG…")), Some("win.export-svg"));
    page.append(Some(&gettext("Page as PNG (300 dpi)…")), Some("win.export-png"));
    menu.append_section(None, &page);
    menu
}

impl PageView {
    pub fn new() -> Rc<PageView> {
        let (renderer, results) = Renderer::spawn();

        // Navigation bar.
        let first = icon_button("go-first-symbolic", gettext("First Page"));
        let prev = icon_button("go-previous-symbolic", gettext("Previous Page (Page Up)"));
        let next = icon_button("go-next-symbolic", gettext("Next Page (Page Down)"));
        let last = icon_button("go-last-symbolic", gettext("Last Page"));
        let page_entry = gtk::Entry::builder().width_chars(4).max_width_chars(6).xalign(1.0).build();
        page_entry.set_input_purpose(gtk::InputPurpose::Digits);
        page_entry.update_property(&[gtk::accessible::Property::Label(&gettext("Page Number"))]);
        let of_label = gtk::Label::builder().css_classes(["dim-label", "numeric"]).build();
        let pages = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let back = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        back.add_css_class("linked");
        back.append(&first);
        back.append(&prev);
        let fwd = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        fwd.add_css_class("linked");
        fwd.append(&next);
        fwd.append(&last);
        pages.append(&back);
        pages.append(&page_entry);
        pages.append(&of_label);
        pages.append(&fwd);

        let copy = gtk::Button::builder()
            .child(&adw::ButtonContent::builder().icon_name("edit-copy-symbolic").label(gettext("Copy Text")).build())
            .tooltip_text(gettext("Copy this page's text"))
            .action_name("win.copy-text")
            .build();
        let export = gtk::MenuButton::builder()
            .child(&adw::ButtonContent::builder().icon_name("document-save-symbolic").label(gettext("Export")).build())
            .menu_model(&export_menu())
            .tooltip_text(gettext("Export the document or this page"))
            .build();
        let zoom_button = gtk::MenuButton::builder()
            .label(Zoom::FitWidth.label())
            .menu_model(&zoom_menu())
            .tooltip_text(gettext("Zoom (Ctrl + wheel)"))
            .build();
        zoom_button.add_css_class("numeric");

        let nav = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        nav.add_css_class("toolbar");
        nav.append(&pages);
        nav.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        nav.append(&copy);
        nav.append(&export);
        let spacer = gtk::Box::builder().hexpand(true).build();
        nav.append(&spacer);
        nav.append(&zoom_button);

        // The page.
        let canvas = PageCanvas::default();
        canvas.set_has_tooltip(true);
        let scroll = gtk::ScrolledWindow::builder().vexpand(true).hexpand(true).child(&canvas).build();
        scroll.add_css_class("page-area");

        // Footer.
        let fid_label = gtk::Label::new(Some(&gettext("Fidelity Notes")));
        let fid_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        fid_content.append(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
        fid_content.append(&fid_label);
        let fid_toggle = gtk::ToggleButton::builder().child(&fid_content).build();
        let fid_scope = gtk::DropDown::from_strings(&[&gettext("All Notes"), &gettext("Fonts Only")]);
        fid_scope.set_tooltip_text(Some(&gettext("Which fidelity notes to highlight")));
        let fid = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        fid.add_css_class("linked");
        fid.append(&fid_toggle);
        fid.append(&fid_scope);
        let summary = gtk::Label::builder()
            .css_classes(["dim-label"])
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let pointer_label = gtk::Label::builder().css_classes(["dim-label", "numeric"]).xalign(1.0).build();
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        footer.add_css_class("toolbar");
        footer.append(&fid);
        footer.append(&summary);
        footer.append(&pointer_label);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&nav);
        root.append(&scroll);
        root.append(&footer);

        let view = Rc::new(PageView {
            root,
            canvas,
            first,
            prev,
            next,
            last,
            page_entry,
            of_label,
            zoom_button,
            fid_toggle,
            fid_label,
            fid_scope,
            summary,
            pointer_label,
            renderer,
            state: RefCell::default(),
            hovered: Cell::new(false),
            events: RefCell::default(),
            dialog_dir: RefCell::default(),
        });
        view.connect_signals();
        // Upgraded per result: holding the view across the await would keep it
        // (and its renderer, which feeds this channel) alive for good.
        let weak = Rc::downgrade(&view);
        glib::spawn_future_local(async move {
            while let Ok(result) = results.recv().await {
                let Some(view) = weak.upgrade() else { break };
                view.accept(result);
            }
        });
        view
    }

    pub fn connect_events(&self, f: EventHandler) {
        self.events.replace(Some(f));
    }

    fn emit(&self, event: Event) {
        let f = self.events.borrow().clone();
        if let Some(f) = f {
            f(event);
        }
    }

    fn connect_signals(self: &Rc<Self>) {
        self.connect_navigation();
        self.connect_fidelity();
        self.connect_canvas();
    }

    /// Page buttons, the page number entry, and the zoom menu label.
    fn connect_navigation(self: &Rc<Self>) {
        let this = self;
        let nav = |button: &gtk::Button, target: fn(usize, usize) -> usize| {
            button.connect_clicked(clone!(
                #[weak]
                this,
                move |_| this.goto(target(this.page(), this.page_count()))
            ));
        };
        nav(&self.first, |_, _| 0);
        nav(&self.prev, |p, _| p.saturating_sub(1));
        nav(&self.next, |p, n| (p + 1).min(n.saturating_sub(1)));
        nav(&self.last, |_, n| n.saturating_sub(1));
        self.page_entry.connect_activate(clone!(
            #[weak]
            this,
            move |e| match e.text().trim().parse::<usize>() {
                Ok(p) if this.page_count() > 0 => this.goto(p.clamp(1, this.page_count()) - 1),
                _ => this.sync_nav(),
            }
        ));
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(clone!(
            #[weak]
            this,
            move |_| this.sync_nav()
        ));
        self.page_entry.add_controller(focus);
        self.canvas.connect_zoom(clone!(
            #[weak]
            this,
            move |zoom| {
                this.zoom_button.set_label(&zoom.label());
                this.emit(Event::Zoomed(zoom));
            }
        ));
    }

    /// The fidelity-notes toggle and scope.
    fn connect_fidelity(self: &Rc<Self>) {
        let this = self;
        self.fid_toggle.connect_toggled(clone!(
            #[weak]
            this,
            move |b| {
                this.state.borrow_mut().fidelity_on = b.is_active();
                this.update_fidelity();
            }
        ));
        self.fid_scope.connect_selected_notify(clone!(
            #[weak]
            this,
            move |d| {
                this.state.borrow_mut().fonts_only = d.selected() == 1;
                this.update_fidelity();
            }
        ));
    }

    /// The page itself: re-render after layout, pointer position, hover,
    /// Ctrl + wheel and pinch zoom, clicks, and the fidelity tooltips.
    fn connect_canvas(self: &Rc<Self>) {
        let this = self;
        self.canvas.connect_layout(clone!(
            #[weak]
            this,
            // Rendering waits for the layout pass to finish.
            move || {
                glib::idle_add_local_once(move || this.ensure_render());
            }
        ));
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(clone!(
            #[weak]
            this,
            move |_, x, y| this.pointer_moved(Some((x as f32, y as f32)))
        ));
        motion.connect_leave(clone!(
            #[weak]
            this,
            move |_| this.pointer_moved(None)
        ));
        self.canvas.add_controller(motion);
        let hover = gtk::EventControllerMotion::new();
        hover.connect_enter(clone!(
            #[weak]
            this,
            move |_, _, _| this.hovered.set(true)
        ));
        hover.connect_leave(clone!(
            #[weak]
            this,
            move |_| this.hovered.set(false)
        ));
        self.root.add_controller(hover);

        // Ctrl + wheel: zoom around the pointer.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        scroll.connect_scroll(clone!(
            #[weak]
            this,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |c, _, dy| {
                if !c.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
                    return glib::Propagation::Proceed;
                }
                let center = (this.canvas.width() as f32 / 2.0, this.canvas.height() as f32 / 2.0);
                let (x, y) = this.state.borrow().pointer.unwrap_or(center);
                this.canvas.zoom_at(1.1f32.powf(-dy as f32), x, y);
                glib::Propagation::Stop
            }
        ));
        self.canvas.add_controller(scroll);

        // Touchpad pinch: the gesture reports the scale since it began.
        let pinch = gtk::GestureZoom::new();
        let last = Rc::new(Cell::new(1.0f64));
        pinch.connect_begin(clone!(
            #[strong]
            last,
            move |_, _| last.set(1.0)
        ));
        pinch.connect_scale_changed(clone!(
            #[weak]
            this,
            move |g, scale| {
                let (x, y) = g.bounding_box_center().unwrap_or((0.0, 0.0));
                this.canvas.zoom_at((scale / last.get()) as f32, x as f32, y as f32);
                last.set(scale);
            }
        ));
        self.canvas.add_controller(pinch);

        // Click on rendered content selects the field that produced it.
        let click = gtk::GestureClick::new();
        click.connect_released(clone!(
            #[weak]
            this,
            move |_, n, x, y| {
                if n == 1 {
                    this.clicked(x as f32, y as f32);
                }
            }
        ));
        self.canvas.add_controller(click);

        self.canvas.connect_query_tooltip(clone!(
            #[weak]
            this,
            #[upgrade_or]
            false,
            move |_, x, y, keyboard, tooltip| {
                let notes = if keyboard { Vec::new() } else { this.notes_at(x as f32, y as f32) };
                if notes.is_empty() {
                    return false;
                }
                let mut markup = format!("<b>{}</b>", glib::markup_escape_text(&gettext("Why this may differ")));
                for n in &notes {
                    markup.push_str(&format!("\n• {}: {}", n.cat.as_str(), glib::markup_escape_text(&safe(&n.msg))));
                }
                tooltip.set_markup(Some(&markup));
                true
            }
        ));
    }

    /// Whether the current page has been rendered (or failed to).
    pub fn is_rendered(&self) -> bool {
        let st = self.state.borrow();
        st.shown.is_some_and(|(g, p, _)| st.doc.as_ref().is_some_and(|d| d.generation == g) && p == st.page)
    }

    pub fn page(&self) -> usize {
        self.state.borrow().page
    }

    fn page_count(&self) -> usize {
        self.state.borrow().doc.as_ref().map_or(0, |d| d.pages.len())
    }

    pub fn set_dialog_dir(&self, dir: Option<PathBuf>) {
        self.dialog_dir.replace(dir);
    }

    /// Whether page keys should go to this pane: the pointer is over it, or
    /// the keyboard focus is inside it.
    pub fn wants_page_keys(&self) -> bool {
        self.hovered.get() || self.root.root().and_then(|w| w.focus()).is_some_and(|f| f.is_ancestor(&self.root))
    }

    pub fn zoom(&self) -> Zoom {
        self.canvas.zoom()
    }

    pub fn set_zoom(&self, zoom: Zoom) {
        self.canvas.set_zoom(zoom);
    }

    /// Zoom one step in (`dir` > 0) or out, around the view center.
    pub fn zoom_step(&self, dir: i32) {
        let (w, h) = (self.canvas.width() as f32, self.canvas.height() as f32);
        self.canvas.zoom_at(if dir > 0 { 1.25 } else { 0.8 }, w / 2.0, h / 2.0);
    }

    /// Show a document; keep the current page when `keep_page`.
    pub fn set_doc(&self, doc: Option<Rc<Doc>>, keep_page: bool) {
        {
            let mut st = self.state.borrow_mut();
            let n = doc.as_ref().map_or(0, |d| d.pages.len());
            st.page = if keep_page { st.page.min(n.saturating_sub(1)) } else { 0 };
            st.doc = doc;
            st.shown = None;
            st.requested = None;
            st.boxes = None;
            st.flash = None;
            if let Some(tick) = st.flash_tick.take() {
                tick.remove();
            }
            self.summary.set_text(&st.doc.as_ref().map(|d| d.summary_line()).unwrap_or_default());
        }
        self.canvas.set_texture(None);
        self.canvas.set_flash(Vec::new(), 0.0);
        self.page_changed();
    }

    /// The user moved to `page`: show it and let the inspector follow.
    fn goto(&self, page: usize) {
        if page == self.page() {
            return;
        }
        self.show_page(page);
        self.emit(Event::PageShown(page));
    }

    /// Show `page` (no event).
    pub fn show_page(&self, page: usize) {
        let n = self.page_count();
        if n == 0 {
            return;
        }
        let page = page.min(n - 1);
        if self.state.borrow().page == page {
            return;
        }
        self.state.borrow_mut().page = page;
        self.canvas.set_texture(None);
        self.canvas.scroll_to_top();
        self.page_changed();
    }

    fn page_changed(&self) {
        let geom = {
            let st = self.state.borrow();
            st.doc.as_ref().and_then(|d| d.pages.get(st.page)).map(|p| PageGeom {
                width: p.width.max(1) as f32,
                height: p.height.max(1) as f32,
                units_per_inch: p.units_per_inch.max(1) as f32,
            })
        };
        self.canvas.set_geom(geom);
        self.canvas.set_message(Some(&gettext("Rendering…")));
        self.sync_nav();
        self.update_fidelity();
        self.ensure_render();
    }

    fn sync_nav(&self) {
        let (page, n) = (self.page(), self.page_count());
        let label = gettext_f("Page {n} of {total}", &[("n", &(page + 1).to_string()), ("total", &n.to_string())]);
        self.canvas.update_property(&[gtk::accessible::Property::Label(&label)]);
        self.page_entry.set_text(&(page + 1).to_string());
        self.of_label.set_text(&gettext_f("of {n}", &[("n", &n.to_string())]));
        self.first.set_sensitive(page > 0);
        self.prev.set_sensitive(page > 0);
        self.next.set_sensitive(page + 1 < n);
        self.last.set_sensitive(page + 1 < n);
    }

    /// Page Up / Page Down while the pointer is over the pane.
    pub fn handle_key(&self, key: gdk::Key) -> bool {
        let (page, n) = (self.page(), self.page_count());
        match key {
            gdk::Key::Page_Down if page + 1 < n => self.goto(page + 1),
            gdk::Key::Page_Up if page > 0 => self.goto(page - 1),
            _ => return false,
        }
        true
    }

    fn ensure_render(&self) {
        let Some(scale) = self.canvas.render_scale() else { return };
        let mut st = self.state.borrow_mut();
        let Some(doc) = st.doc.clone() else { return };
        if st.page >= doc.pages.len() {
            return;
        }
        let close = |a: f32, b: f32| (a - b).abs() <= 0.01 * a.max(b);
        let same = |k: Option<(u64, usize, f32)>| {
            k.is_some_and(|(g, p, s)| g == doc.generation && p == st.page && close(s, scale))
        };
        if same(st.shown) || same(st.requested) {
            return;
        }
        st.requested = Some((doc.generation, st.page, scale));
        self.renderer.request(RenderJob { generation: doc.generation, pages: doc.pages.clone(), page: st.page, scale });
    }

    /// Take a finished rasterization.
    fn accept(&self, r: RenderResult) {
        let rendered = {
            let mut st = self.state.borrow_mut();
            let current = st.doc.as_ref().is_some_and(|d| d.generation == r.generation) && st.page == r.page;
            if !current {
                return;
            }
            // Also on failure, so the page is not requested again.
            st.shown = Some((r.generation, r.page, r.scale));
            match r.outcome {
                Ok(page) => {
                    st.boxes = Some((r.generation, r.page, page.boxes));
                    Some((page.width, page.height, page.rgba))
                }
                Err(_) => {
                    st.boxes = None;
                    None
                }
            }
        };
        let Some((width, height, rgba)) = rendered else {
            self.canvas.set_texture(None);
            self.canvas.set_message(Some(&gettext("This page cannot be drawn.")));
            return;
        };
        let bytes = glib::Bytes::from_owned(rgba);
        let texture = gdk::MemoryTexture::new(
            width as i32,
            height as i32,
            gdk::MemoryFormat::R8g8b8a8Premultiplied,
            &bytes,
            width as usize * 4,
        );
        self.canvas.set_message(None);
        self.canvas.set_texture(Some(texture.upcast()));
        self.update_fidelity();
        self.start_flash();
    }

    fn current_boxes(&self) -> Option<Boxes> {
        let st = self.state.borrow();
        let doc = st.doc.as_ref()?;
        match &st.boxes {
            Some((g, p, b)) if *g == doc.generation && *p == st.page => Some(b.clone()),
            _ => None,
        }
    }

    fn with_page<T>(&self, f: impl FnOnce(&Page) -> T) -> Option<T> {
        let st = self.state.borrow();
        st.doc.as_ref().and_then(|d| d.pages.get(st.page)).map(f)
    }

    /// Element ids of the current page carrying in-scope fidelity notes.
    fn fidelity_items(&self) -> Vec<(String, Vec<FidelityNote>)> {
        let fonts_only = self.state.borrow().fonts_only;
        let in_scope = |notes: &[FidelityNote]| -> Vec<FidelityNote> {
            notes.iter().filter(|n| !fonts_only || n.cat == NoteCategory::Font).cloned().collect()
        };
        self.with_page(|page| {
            let mut out = Vec::new();
            for (i, t) in page.texts.iter().enumerate() {
                let n = in_scope(&t.notes);
                if !n.is_empty() {
                    out.push((format!("t{i}"), n));
                }
            }
            for (i, im) in page.images.iter().enumerate() {
                let n = in_scope(&im.notes);
                if !n.is_empty() {
                    out.push((format!("i{i}"), n));
                }
            }
            out
        })
        .unwrap_or_default()
    }

    fn update_fidelity(&self) {
        let items = self.fidelity_items();
        let count = items.len();
        let on = self.state.borrow().fidelity_on;
        let count_text = count.to_string();
        let n = [("n", count_text.as_str())];
        self.fid_label.set_text(&if count > 0 {
            gettext_f("Fidelity Notes ({n})", &n)
        } else {
            gettext("Fidelity Notes")
        });
        let tip = if count == 0 {
            gettext("Nothing on this page is approximated: it is rendered faithfully.")
        } else if on {
            ngettext_f(
                "{n} approximated region highlighted; hover it to see why. Click to hide.",
                "{n} approximated regions highlighted; hover one to see why. Click to hide.",
                count,
                &n,
            )
        } else {
            ngettext_f(
                "{n} approximated region on this page: click to highlight it and see why on hover.",
                "{n} approximated regions on this page: click to highlight them and see why on hover.",
                count,
                &n,
            )
        };
        self.fid_toggle.set_tooltip_text(Some(&tip));
        if count > 0 && !on {
            self.fid_toggle.add_css_class("warning");
        } else {
            self.fid_toggle.remove_css_class("warning");
        }
        let boxes = match (on, self.current_boxes()) {
            (true, Some(b)) => items.iter().filter_map(|(id, _)| b.get(id).copied()).collect(),
            _ => Vec::new(),
        };
        self.canvas.set_fidelity(boxes);
    }

    fn notes_at(&self, x: f32, y: f32) -> Vec<FidelityNote> {
        if !self.state.borrow().fidelity_on {
            return Vec::new();
        }
        let Some(boxes) = self.current_boxes() else { return Vec::new() };
        let mut out: Vec<FidelityNote> = Vec::new();
        for (id, notes) in self.fidelity_items() {
            if boxes.get(&id).is_some_and(|b| self.canvas.hits(b, x, y, true)) {
                for n in notes {
                    if !out.contains(&n) {
                        out.push(n);
                    }
                }
            }
        }
        out
    }

    fn pointer_moved(&self, pos: Option<(f32, f32)>) {
        self.state.borrow_mut().pointer = pos;
        let upi = self.with_page(|p| p.units_per_inch.max(1) as f32).unwrap_or(1440.0);
        let text = pos.and_then(|(x, y)| self.canvas.page_point(x, y)).map_or(String::new(), |(x, y)| {
            gettext_f("X {x} mm   Y {y} mm", &[("x", &decimal1(x / upi * 25.4)), ("y", &decimal1(y / upi * 25.4))])
        });
        self.pointer_label.set_text(&text);
    }

    fn clicked(&self, x: f32, y: f32) {
        let Some(boxes) = self.current_boxes() else { return };
        let hit = self.with_page(|page| {
            page.texts
                .iter()
                .enumerate()
                .filter_map(|(i, t)| Some((format!("t{i}"), t.src?)))
                .chain(page.rules.iter().enumerate().filter_map(|(i, r)| Some((format!("r{i}"), r.src?))))
                .find(|(id, _)| boxes.get(id).is_some_and(|b| self.canvas.hits(b, x, y, false)))
                .map(|(_, src)| src)
        });
        if let Some(Some(src)) = hit {
            self.flash(src);
            self.emit(Event::ContentClicked(src));
        }
    }

    /// Highlight, for a moment, the content produced by the field at
    /// offset `src` on the current page.
    pub fn flash(&self, src: usize) {
        {
            let mut st = self.state.borrow_mut();
            st.flash = Some((st.page, src, glib::monotonic_time()));
        }
        self.start_flash();
    }

    fn start_flash(&self) {
        let Some((page, src, started)) = self.state.borrow().flash else { return };
        if page != self.page() || glib::monotonic_time() - started >= FLASH_US {
            return;
        }
        let Some(boxes) = self.current_boxes() else { return }; // retried when the render arrives
        let rects: Vec<BoxF> = self
            .with_page(|p| {
                let texts =
                    p.texts.iter().enumerate().filter(|(_, t)| t.src == Some(src)).map(|(i, _)| format!("t{i}"));
                let rules =
                    p.rules.iter().enumerate().filter(|(_, r)| r.src == Some(src)).map(|(i, _)| format!("r{i}"));
                texts.chain(rules).filter_map(|id| boxes.get(&id).copied()).collect()
            })
            .unwrap_or_default();
        if let Some(old) = self.state.borrow_mut().flash_tick.take() {
            old.remove();
        }
        if rects.is_empty() {
            return;
        }
        self.canvas.set_flash(rects, 1.0);
        let tick = self.canvas.add_tick_callback(move |canvas, _| {
            let alpha = 1.0 - (glib::monotonic_time() - started) as f32 / FLASH_US as f32;
            canvas.set_flash_alpha(alpha.max(0.0));
            if alpha <= 0.0 { glib::ControlFlow::Break } else { glib::ControlFlow::Continue }
        });
        self.state.borrow_mut().flash_tick = Some(tick);
    }

    pub fn copy_text(&self) {
        if let Some(text) = self.with_page(Page::plain_text) {
            self.root.clipboard().set_text(&safe(&text));
            self.emit(Event::Toast(gettext("Page text copied")));
        }
    }

    fn window(&self) -> Option<gtk::Window> {
        self.root.root().and_downcast::<gtk::Window>()
    }

    /// Ask where to save, starting in the document's folder.
    async fn ask_save(&self, name: String, label: &str, pattern: &str) -> Option<PathBuf> {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(label));
        filter.add_pattern(pattern);
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder().modal(true).initial_name(name).filters(&filters).build();
        if let Some(dir) = self.dialog_dir.borrow().as_ref() {
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        dialog.save_future(self.window().as_ref()).await.ok()?.path()
    }

    /// Save all pages' text as one file.
    pub async fn save_text(&self) {
        let Some(doc) = self.state.borrow().doc.clone() else { return };
        let Some(path) = self.ask_save(format!("{}.txt", doc.name), &gettext("Text"), "*.txt").await else { return };
        let pages = doc.pages.clone();
        let result = gio::spawn_blocking(move || {
            let texts: Vec<String> = pages.iter().map(Page::plain_text).collect();
            std::fs::write(&path, texts.join("\n\n----- page break -----\n\n")).map(|_| path)
        })
        .await;
        self.report_saved(result);
    }

    /// Export the current page as SVG, or PNG at 300 dpi.
    pub async fn export_page(&self, png: bool) {
        let Some(doc) = self.state.borrow().doc.clone() else { return };
        let page_idx = self.page();
        let stem = file_stem(&doc.name);
        let (ext, label) = if png { ("png", gettext("PNG image")) } else { ("svg", gettext("SVG image")) };
        let name = format!("{stem}-page{}.{ext}", page_idx + 1);
        let Some(path) = self.ask_save(name, &label, &format!("*.{ext}")).await else { return };
        let pages = doc.pages.clone();
        let result = gio::spawn_blocking(move || {
            let bytes = render_export(&pages[page_idx], png).map_err(std::io::Error::other)?;
            std::fs::write(&path, bytes).map(|_| path)
        })
        .await;
        self.report_saved(result);
    }

    /// Export the whole document, or only the current page, as PDF.
    pub async fn export_pdf(&self, whole: bool) {
        let Some(doc) = self.state.borrow().doc.clone() else { return };
        let page_idx = self.page();
        let stem = file_stem(&doc.name);
        let name = if whole { format!("{stem}.pdf") } else { format!("{stem}-page{}.pdf", page_idx + 1) };
        let Some(path) = self.ask_save(name, &gettext("PDF document"), "*.pdf").await else { return };
        let pages = doc.pages.clone();
        let title = doc.name.clone();
        if whole && pages.len() > 1 {
            self.emit(Event::Toast(gettext_f("Exporting {n} pages as PDF…", &[("n", &pages.len().to_string())])));
        }
        let result = gio::spawn_blocking(move || {
            let selected = if whole { &pages[..] } else { &pages[page_idx..=page_idx] };
            let rasterizer = afp_core::raster::Rasterizer::new();
            let pdf = afp_core::pdf::pages_to_pdf(selected, &title, &rasterizer).map_err(std::io::Error::other)?;
            std::fs::write(&path, pdf).map(|_| path)
        })
        .await;
        self.report_saved(result);
    }

    fn report_saved(&self, result: Result<std::io::Result<PathBuf>, Box<dyn std::any::Any + Send>>) {
        let msg = match result {
            Ok(Ok(path)) => {
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                gettext_f("Saved {file}", &[("file", &name)])
            }
            Ok(Err(e)) => gettext_f("Could not save: {error}", &[("error", &e.to_string())]),
            Err(_) => gettext("Could not save: the export failed"),
        };
        self.emit(Event::Toast(msg));
    }
}

/// The document name without its extension, for exported file names.
fn file_stem(name: &str) -> String {
    std::path::Path::new(name).file_stem().map_or_else(|| name.to_string(), |s| s.to_string_lossy().into_owned())
}

fn render_export(page: &Page, png: bool) -> Result<Vec<u8>, String> {
    let svg = afp_core::render_svg::page_to_svg(page);
    if !png {
        return Ok(svg.into_bytes());
    }
    let rasterizer = afp_core::raster::Rasterizer::new();
    let scale = 300.0 / page.units_per_inch.max(1) as f32;
    let shrinking = afp_core::raster::crisp_image_resolution(page).is_some_and(|r| scale < r);
    let tree = rasterizer.parse_for_scale(&svg, shrinking).map_err(|e| e.to_string())?;
    let pixmap = rasterizer.render(&tree, scale).ok_or("cannot render the page")?;
    pixmap.encode_png().map_err(|e| e.to_string())
}
