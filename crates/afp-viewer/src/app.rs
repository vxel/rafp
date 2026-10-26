//! Main window: header bar and menu, banners, the inspector / page split,
//! file loading, and the navigation shared by the two panes.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use afp_core::codepages::{Codec, UI_CODEPAGES};
use afp_core::model::ResourceKind;
use gtk::glib::clone;
use gtk::{gdk, gio, glib};

use crate::i18n::{gettext, gettext_f, ngettext_f, safe};
use crate::inspector::Inspector;
use crate::page_view::PageView;
use crate::settings::{Settings, Zoom};
use crate::worker::{Doc, LoadResult, load_document};

/// Receives the panes' events.
pub type EventHandler = Rc<dyn Fn(Event)>;

/// What one pane tells the window.
pub enum Event {
    /// The user picked a field in the inspector.
    RowSelected(usize),
    /// Find moved to a PTX field: show its page and flash its text.
    FindHit(usize),
    /// The page view moved to another page by itself.
    PageShown(usize),
    /// Rendered content produced by the field at this offset was clicked.
    ContentClicked(usize),
    /// The zoom changed (menu, keys, Ctrl + wheel, pinch).
    Zoomed(Zoom),
    /// A short message for the user.
    Toast(String),
}

struct Source {
    name: String,
    bytes: Arc<Vec<u8>>,
}

#[derive(Default)]
struct State {
    source: Option<Source>,
    doc: Option<Rc<Doc>>,
    error: Option<String>,
    generation: u64,
    loading: bool,
    /// Folder of the last file opened from disk.
    source_dir: Option<PathBuf>,
}

pub struct App {
    pub window: adw::ApplicationWindow,
    title: adw::WindowTitle,
    toasts: adw::ToastOverlay,
    banners: gtk::Box,
    stack: gtk::Stack,
    paned: gtk::Paned,
    sidebar_button: gtk::ToggleButton,
    inspector: Rc<Inspector>,
    page_view: Rc<PageView>,
    settings: RefCell<Settings>,
    state: RefCell<State>,
}

fn main_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let file = gio::Menu::new();
    file.append(Some(&gettext("Open AFP File…")), Some("app.open"));
    menu.append_section(None, &file);

    let options = gio::Menu::new();
    let encoding = gio::Menu::new();
    let pages = gio::Menu::new();
    for (code, label) in UI_CODEPAGES {
        let item = gio::MenuItem::new(Some(&codepage_label(code, label)), None);
        item.set_action_and_target_value(Some("win.codepage"), Some(&code.to_variant()));
        pages.append_item(&item);
    }
    encoding.append_section(None, &pages);
    options.append_submenu(Some(&gettext("Default Encoding")), &encoding);
    options.append(Some(&gettext("Small Text in Embedded Font")), Some("win.embed-small-fonts"));
    menu.append_section(None, &options);

    let help = gio::Menu::new();
    help.append(Some(&gettext("About rafp")), Some("app.about"));
    menu.append_section(None, &help);
    menu
}

fn landing() -> adw::StatusPage {
    let open = gtk::Button::builder()
        .label(gettext("Open AFP File…"))
        .action_name("app.open")
        .halign(gtk::Align::Center)
        .css_classes(["pill", "suggested-action"])
        .build();
    adw::StatusPage::builder()
        .icon_name("document-open-symbolic")
        .title(gettext("Open an AFP File"))
        .description(gettext(
            "Inspect its structured fields and render its pages. You can also drop a file on this window.",
        ))
        .child(&open)
        .build()
}

impl App {
    pub fn new(app: &adw::Application) -> Rc<App> {
        let settings = Settings::load();
        let inspector = Inspector::new();
        let page_view = PageView::new();

        // Header bar.
        let menu = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&main_menu())
            .tooltip_text(gettext("Main Menu"))
            .primary(true)
            .build();
        let sidebar_button = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text(gettext("Toggle the Inspector (F9)"))
            .action_name("win.show-inspector")
            .visible(false)
            .build();
        let title = adw::WindowTitle::new("rafp", "");
        let header = adw::HeaderBar::new();
        header.pack_start(&sidebar_button);
        header.pack_end(&menu);
        header.set_title_widget(Some(&title));

        // Content: banners above the landing page or the split view.
        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(&inspector.root)
            .end_child(&page_view.root)
            .resize_start_child(false)
            .shrink_start_child(false)
            .shrink_end_child(false)
            .wide_handle(false)
            .build();
        inspector.root.set_size_request(320, -1);
        page_view.root.set_size_request(320, -1);
        let stack = gtk::Stack::builder().vexpand(true).transition_type(gtk::StackTransitionType::Crossfade).build();
        stack.add_named(&landing(), Some("landing"));
        stack.add_named(&paned, Some("doc"));
        let banners = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&banners);
        body.append(&stack);

        // Drop feedback over the whole content.
        let drop_hint = gtk::Label::builder()
            .label(gettext("Drop the AFP file to open it"))
            .css_classes(["drop-overlay", "title-1"])
            .visible(false)
            .can_target(false)
            .build();
        let overlay = gtk::Overlay::builder().child(&body).build();
        overlay.add_overlay(&drop_hint);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&overlay));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&toasts));

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("rafp")
            .default_width(settings.width)
            .default_height(settings.height)
            .width_request(640)
            .height_request(400)
            .content(&toolbar)
            .build();
        if settings.maximized {
            window.maximize();
        }
        paned.set_position(settings.inspector_width.unwrap_or(settings.width / 2));
        page_view.set_zoom(settings.zoom);

        let this = Rc::new(App {
            window,
            title,
            toasts,
            banners,
            stack,
            paned,
            sidebar_button,
            inspector,
            page_view,
            settings: RefCell::new(settings),
            state: RefCell::default(),
        });
        this.install_actions(app);
        this.connect_events();
        this.connect_drop(&drop_hint);
        this.update_layout();
        // The window's handlers only hold weak references: the window keeps
        // the app state alive until it is destroyed.
        let keep = RefCell::new(Some(this.clone()));
        this.window.connect_destroy(move |_| drop(keep.take()));
        this
    }

    fn install_actions(self: &Rc<Self>, app: &adw::Application) {
        let this = self;
        let st = self.settings.borrow();

        let open = gio::SimpleAction::new("open", None);
        open.connect_activate(clone!(
            #[weak]
            this,
            move |_, _| {
                glib::spawn_future_local(async move { this.open_dialog().await });
            }
        ));
        app.add_action(&open);
        let about = gio::SimpleAction::new("about", None);
        about.connect_activate(clone!(
            #[weak]
            this,
            move |_, _| this.about()
        ));
        app.add_action(&about);
        let quit = gio::SimpleAction::new("quit", None);
        quit.connect_activate(clone!(
            #[weak]
            this,
            move |_, _| this.window.close()
        ));
        app.add_action(&quit);

        // Preferences: each change applies at once and is saved.
        self.add_setting("codepage", st.codepage.to_variant(), |this, value| {
            this.settings.borrow_mut().codepage = value.str().unwrap_or("cp500").to_string();
            this.reload();
        });
        self.add_setting("embed-small-fonts", st.embed_small_fonts.to_variant(), |this, value| {
            this.settings.borrow_mut().embed_small_fonts = value.get().unwrap_or(false);
            this.reload();
        });
        self.add_setting("show-inspector", st.show_inspector.to_variant(), |this, value| {
            this.settings.borrow_mut().show_inspector = value.get().unwrap_or(true);
            this.update_layout();
        });
        let zoom = gio::SimpleAction::new_stateful("zoom", Some(glib::VariantTy::STRING), &st.zoom.key().to_variant());
        zoom.connect_change_state(clone!(
            #[weak]
            this,
            move |a, value| {
                if let Some(z) = value.and_then(|v| v.str()).and_then(Zoom::from_key) {
                    a.set_state(&z.key().to_variant());
                    this.page_view.set_zoom(z);
                }
            }
        ));
        self.window.add_action(&zoom);
        drop(st);

        self.add_simple("find", |this| {
            if this.state.borrow().doc.is_none() {
                return;
            }
            if !this.inspector.root.is_visible()
                && let Some(show) = this.window.lookup_action("show-inspector")
            {
                show.change_state(&true.to_variant());
            }
            this.inspector.focus_find();
        });
        self.add_simple("zoom-in", |this| this.page_view.zoom_step(1));
        self.add_simple("zoom-out", |this| this.page_view.zoom_step(-1));
        self.add_simple("zoom-reset", |this| this.page_view.set_zoom(Zoom::Factor(1.0)));
        self.add_simple("copy-text", |this| this.page_view.copy_text());
        self.add_export("save-text", |pv| async move { pv.save_text().await });
        self.add_export("export-svg", |pv| async move { pv.export_page(false).await });
        self.add_export("export-png", |pv| async move { pv.export_page(true).await });
        self.add_export("export-pdf", |pv| async move { pv.export_pdf(true).await });
        self.add_export("export-page-pdf", |pv| async move { pv.export_pdf(false).await });

        app.set_accels_for_action("app.open", &["<Control>o"]);
        app.set_accels_for_action("app.quit", &["<Control>q"]);
        app.set_accels_for_action("win.find", &["<Control>f"]);
        app.set_accels_for_action("win.show-inspector", &["F9"]);
        app.set_accels_for_action("win.zoom-in", &["<Control>plus", "<Control>equal", "<Control>KP_Add"]);
        app.set_accels_for_action("win.zoom-out", &["<Control>minus", "<Control>KP_Subtract"]);
        app.set_accels_for_action("win.zoom-reset", &["<Control>0"]);
        self.set_doc_actions_enabled(false);
    }

    /// A window action running `f`.
    fn add_simple(self: &Rc<Self>, name: &str, f: fn(&Rc<App>)) {
        let this = self;
        let action = gio::SimpleAction::new(name, None);
        action.connect_activate(clone!(
            #[weak]
            this,
            move |_, _| f(&this)
        ));
        self.window.add_action(&action);
    }

    /// A window action running an export of the page view (which asks
    /// where to save, so it runs as a task).
    fn add_export<F: Future<Output = ()> + 'static>(self: &Rc<Self>, name: &str, f: fn(Rc<PageView>) -> F) {
        let this = self;
        let action = gio::SimpleAction::new(name, None);
        action.connect_activate(clone!(
            #[weak]
            this,
            move |_, _| {
                glib::spawn_future_local(f(this.page_view.clone()));
            }
        ));
        self.window.add_action(&action);
    }

    /// A stateful window action for a preference: `apply` stores the new
    /// value; the settings are then saved.
    fn add_setting(self: &Rc<Self>, name: &str, initial: glib::Variant, apply: fn(&Rc<App>, &glib::Variant)) {
        let this = self;
        // String choices take the value as parameter (radio items); booleans
        // toggle on activation (check items).
        let parameter = (initial.type_() == glib::VariantTy::STRING).then_some(glib::VariantTy::STRING);
        let action = gio::SimpleAction::new_stateful(name, parameter, &initial);
        action.connect_change_state(clone!(
            #[weak]
            this,
            move |a, value| {
                let Some(value) = value else { return };
                a.set_state(value);
                apply(&this, value);
                this.save_settings();
            }
        ));
        self.window.add_action(&action);
    }

    fn set_doc_actions_enabled(&self, pages: bool) {
        for name in [
            "zoom",
            "zoom-in",
            "zoom-out",
            "zoom-reset",
            "copy-text",
            "save-text",
            "export-svg",
            "export-png",
            "export-pdf",
            "export-page-pdf",
        ] {
            if let Some(a) = self.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                a.set_enabled(pages);
            }
        }
    }

    fn connect_events(self: &Rc<Self>) {
        let this = self;
        let handler: EventHandler = Rc::new(clone!(
            #[weak]
            this,
            move |event| this.handle(event)
        ));
        self.inspector.connect_events(handler.clone());
        self.page_view.connect_events(handler);

        // Page Up / Page Down page while the pointer or the focus is in the page pane.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(clone!(
            #[weak]
            this,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, mods| {
                let plain = !mods.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK);
                if plain && this.page_view.wants_page_keys() && this.page_view.handle_key(key) {
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        ));
        self.window.add_controller(keys);

        self.window.connect_close_request(clone!(
            #[weak]
            this,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_| {
                this.save_settings();
                glib::Propagation::Proceed
            }
        ));
    }

    fn handle(&self, event: Event) {
        let event = match event {
            Event::Zoomed(zoom) => {
                if let Some(a) = self.window.lookup_action("zoom").and_downcast::<gio::SimpleAction>() {
                    a.set_state(&zoom.key().to_variant());
                }
                return;
            }
            Event::Toast(msg) => {
                self.toasts.add_toast(adw::Toast::new(&msg));
                return;
            }
            event => event,
        };
        let Some(doc) = self.state.borrow().doc.clone() else { return };
        match event {
            Event::RowSelected(i) | Event::FindHit(i) => {
                let row = &doc.rows[i];
                if let Some(p) = row.page
                    && p < doc.pages.len()
                {
                    self.page_view.show_page(p);
                    self.inspector.set_current_page(Some(p));
                    self.page_view.flash(row.offset);
                }
            }
            Event::PageShown(p) => {
                self.inspector.set_current_page(Some(p));
                if let Some(Some(row)) = doc.page_first_row.get(p) {
                    self.inspector.scroll_to_row(*row, false);
                }
            }
            Event::ContentClicked(src) => {
                if let Some(&row) = doc.row_of_offset.get(&src) {
                    self.inspector.scroll_to_row(row, true);
                }
            }
            Event::Zoomed(_) | Event::Toast(_) => {}
        }
    }

    fn connect_drop(self: &Rc<Self>, hint: &gtk::Label) {
        let this = self;
        let target = gtk::DropTarget::new(gio::File::static_type(), gdk::DragAction::COPY);
        target.connect_enter(clone!(
            #[weak]
            hint,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |_, _, _| {
                hint.set_visible(true);
                gdk::DragAction::COPY
            }
        ));
        target.connect_leave(clone!(
            #[weak]
            hint,
            move |_| hint.set_visible(false)
        ));
        target.connect_drop(clone!(
            #[weak]
            this,
            #[weak]
            hint,
            #[upgrade_or]
            false,
            move |_, value, _, _| {
                hint.set_visible(false);
                let Ok(file) = value.get::<gio::File>() else { return false };
                this.open_file(file);
                true
            }
        ));
        self.window.add_controller(target);
    }

    fn codepage(&self) -> Codec {
        Codec::lookup(&self.settings.borrow().codepage).unwrap_or_else(Codec::cp500)
    }

    /// Read `file` without blocking the window (large files, network
    /// mounts), then decode it.
    pub fn open_file(self: &Rc<Self>, file: gio::File) {
        if let Some(dir) =
            file.path().and_then(|p| std::fs::canonicalize(p).ok()).and_then(|p| p.parent().map(Path::to_path_buf))
        {
            self.state.borrow_mut().source_dir = Some(dir);
        }
        let name = file.basename().map_or_else(|| file.uri().to_string(), |n| n.to_string_lossy().into_owned());
        let this = self.clone();
        glib::spawn_future_local(async move {
            match file.load_contents_future().await {
                Ok((bytes, _)) => this.open_bytes(name, Arc::new(bytes.to_vec())),
                Err(e) => this.show_error(gettext_f(
                    "Cannot read {file}: {error}",
                    &[("file", &name), ("error", &e.to_string())],
                )),
            }
        });
    }

    fn open_bytes(self: &Rc<Self>, name: String, bytes: Arc<Vec<u8>>) {
        self.state.borrow_mut().source = Some(Source { name, bytes });
        self.reload();
    }

    fn show_error(&self, error: String) {
        self.state.borrow_mut().error = Some(error);
        self.update_banners();
    }

    /// Where file dialogs start: the open file's folder, else the folder the
    /// app was launched from (`run.sh` passes it as `RAFP_START_DIR`).
    fn dialog_dir(&self) -> Option<PathBuf> {
        if let Some(dir) = self.state.borrow().source_dir.clone() {
            return Some(dir);
        }
        std::env::var_os("RAFP_START_DIR").map(PathBuf::from).or_else(|| std::env::current_dir().ok())
    }

    async fn open_dialog(self: Rc<Self>) {
        let afp = gtk::FileFilter::new();
        afp.set_name(Some(&gettext("AFP files")));
        for ext in ["afp", "afpds", "list3820", "AFP", "AFPDS"] {
            afp.add_suffix(ext);
        }
        let all = gtk::FileFilter::new();
        all.set_name(Some(&gettext("All files")));
        all.add_pattern("*");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&afp);
        filters.append(&all);
        let dialog = gtk::FileDialog::builder().title(gettext("Open AFP File")).modal(true).filters(&filters).build();
        if let Some(dir) = self.dialog_dir() {
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        match dialog.open_future(Some(&self.window)).await {
            Ok(file) => self.open_file(file),
            Err(e) if e.matches(gtk::DialogError::Dismissed) || e.matches(gtk::DialogError::Cancelled) => {}
            Err(e) => {
                log::error!("file chooser failed: {e}");
                self.show_error(gettext_f(
                    "The file chooser could not be shown: {error}",
                    &[("error", &e.to_string())],
                ));
            }
        }
    }

    /// (Re-)inspect the current source, e.g. after a code page change.
    fn reload(self: &Rc<Self>) {
        let (name, bytes, generation) = {
            let mut st = self.state.borrow_mut();
            let Some(src) = &st.source else { return };
            let (name, bytes) = (src.name.clone(), src.bytes.clone());
            st.generation += 1;
            st.loading = true;
            (name, bytes, st.generation)
        };
        self.title.set_subtitle(&gettext("Loading…"));
        let (codepage, embed) = (self.codepage(), self.settings.borrow().embed_small_fonts);
        let this = self.clone();
        glib::spawn_future_local(async move {
            let result = load_document(name, bytes, codepage, embed, generation).await;
            if this.state.borrow().generation == generation {
                this.loaded(result);
            }
        });
    }

    fn loaded(&self, result: LoadResult) {
        self.state.borrow_mut().loading = false;
        self.title.set_subtitle("");
        match result {
            LoadResult::Loaded(doc) => {
                let doc: Rc<Doc> = Rc::from(doc);
                let same_file = self
                    .state
                    .borrow()
                    .doc
                    .as_ref()
                    .is_some_and(|d| d.name == doc.name && Arc::ptr_eq(&d.bytes, &doc.bytes));
                {
                    let mut st = self.state.borrow_mut();
                    st.doc = Some(doc.clone());
                    st.error = None;
                }
                self.title.set_title(&doc.name);
                self.window.set_title(Some(&format!("{} — rafp", doc.name)));
                self.page_view.set_dialog_dir(self.dialog_dir());
                self.page_view.set_doc(Some(doc.clone()), same_file);
                self.inspector.set_doc(Some(doc.clone()), same_file);
                self.inspector.focus_table();
                if !doc.pages.is_empty() {
                    self.inspector.set_current_page(Some(self.page_view.page()));
                }
            }
            LoadResult::Failed { name, error } => {
                self.title.set_title(&name);
                self.window.set_title(Some(&format!("{name} — rafp")));
                {
                    let mut st = self.state.borrow_mut();
                    st.doc = None;
                    st.error = Some(error);
                }
                self.page_view.set_doc(None, false);
                self.inspector.set_doc(None, false);
            }
        }
        self.update_banners();
        self.update_layout();
    }

    /// Show the landing page, the split view, or the inspector alone.
    fn update_layout(&self) {
        let st = self.state.borrow();
        let has_pages = st.doc.as_ref().is_some_and(|d| !d.pages.is_empty());
        self.stack.set_visible_child_name(if st.doc.is_some() { "doc" } else { "landing" });
        let show_inspector = !has_pages || self.settings.borrow().show_inspector;
        self.inspector.root.set_visible(show_inspector);
        self.page_view.root.set_visible(has_pages);
        self.sidebar_button.set_visible(has_pages);
        let footer = st.doc.as_ref().filter(|_| !has_pages).map(|d| d.summary_line());
        self.inspector.set_footer(footer.as_deref());
        self.set_doc_actions_enabled(has_pages);
        if let Some(a) = self.window.lookup_action("find").and_downcast::<gio::SimpleAction>() {
            a.set_enabled(st.doc.is_some());
        }
    }

    fn update_banners(&self) {
        while let Some(child) = self.banners.first_child() {
            self.banners.remove(&child);
        }
        let st = self.state.borrow();
        if let Some(err) = &st.error {
            self.banners.append(&banner("error", &gettext("Cannot open this file"), std::slice::from_ref(err), &[]));
        }
        let Some(doc) = &st.doc else { return };
        if let Some(kind) = doc.resource_kind {
            let text = if doc.pages.is_empty() {
                gettext_f(
                    "This file is an AFP resource ({kind}), not a printable document: it has no pages (no BDT…EDT \
                     document structure for a viewer to lay out), which is why most AFP viewers can't open it at all. \
                     Its structured fields are listed below; it carries no drawable content to render.",
                    &[("kind", &resource_kind_label(kind))],
                )
            } else {
                gettext_f(
                    "This file is an AFP resource ({kind}), not a printable document: it has no pages (no BDT…EDT \
                     document structure for a viewer to lay out), which is why most AFP viewers can't open it at all. \
                     A synthesized view of its contents is shown on the right.",
                    &[("kind", &resource_kind_label(kind))],
                )
            };
            self.banners.append(&banner("resource", &gettext("AFP Resource"), &[text], &[]));
        }
        if !doc.missing_resources.is_empty() {
            let n = doc.missing_resources.len();
            let text = ngettext_f(
                "This document references {n} external font resource that is not embedded in the file. Like other \
                 AFP viewers, rafp can't load it, so text using it is drawn with a substitute font:",
                "This document references {n} external font resources that are not embedded in the file. Like other \
                 AFP viewers, rafp can't load them, so text using them is drawn with a substitute font:",
                n,
                &[("n", &n.to_string())],
            );
            let items: Vec<(String, String)> = doc
                .missing_resources
                .iter()
                .map(|r| {
                    let text = match &r.codec {
                        Some(c) => gettext_f("{name} (text decoded via {codec})", &[("name", &r.name), ("codec", c)]),
                        None => r.name.clone(),
                    };
                    (text, resource_kind_label(r.kind))
                })
                .collect();
            self.banners.append(&banner("missing", &gettext("Missing Resources"), &[text], &items));
        }
    }

    fn about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name("rafp")
            .application_icon(crate::APP_ID)
            .version(env!("CARGO_PKG_VERSION"))
            .developer_name("Cédric de Launois")
            .comments(gettext(
                "AFP / MO:DCA file inspector and renderer, a Rust port of readAFP.\n\nDecodes PTOCA text, IOCA images \
                 (JPEG, MMR, LZW, CMYK bands), IM images, BCOCA QR codes, GOCA vector graphics and FOCA raster, \
                 Type 1 and CFF fonts.",
            ))
            // Translators: your name(s), one per line, shown in the About dialog.
            .translator_credits(gettext("translator-credits"))
            .license_type(gtk::License::MitX11)
            .build();
        // A credit entry "Label https://…" shows the label as a link.
        let link = format!("{} https://github.com/bone1966/readAFP", gettext("readAFP on GitHub"));
        about.add_credit_section(Some(&gettext("Based on readAFP")), &["Keith Lutz", &link]);
        about.present(Some(&self.window));
    }

    fn save_settings(&self) {
        if std::env::var_os("RAFP_SCREENSHOT").is_some() {
            return; // automated runs must not overwrite the user's preferences
        }
        let mut s = self.settings.borrow_mut();
        s.maximized = self.window.is_maximized();
        let (w, h) = self.window.default_size();
        if w > 0 && h > 0 {
            s.width = w;
            s.height = h;
        }
        if self.inspector.root.is_visible() && self.page_view.root.is_visible() {
            s.inspector_width = Some(self.paned.position());
        }
        s.zoom = self.page_view.zoom();
        s.save();
    }

    /// Whether the window shows a finished document (for screenshots).
    pub fn ready(&self) -> bool {
        let st = self.state.borrow();
        !st.loading && st.doc.as_ref().is_none_or(|d| d.pages.is_empty() || self.page_view.is_rendered())
    }

    pub fn set_find(&self, query: &str) {
        self.inspector.set_query(query);
    }

    /// Show page `page` (0-based) of the open document.
    pub fn show_page(&self, page: usize) {
        self.page_view.show_page(page);
        self.inspector.set_current_page(Some(self.page_view.page()));
    }

    /// Whether a document is open and decoded.
    pub fn has_doc(&self) -> bool {
        let st = self.state.borrow();
        !st.loading && st.doc.is_some()
    }
}

/// The translated menu label of a code page from `UI_CODEPAGES`.
fn codepage_label(code: &str, label: &str) -> String {
    match code {
        "cp500" => gettext("cp500 (international, default)"),
        "cp037" => gettext("cp037 (US / Canada)"),
        "cp273" => gettext("cp273 (Germany / Austria)"),
        "cp1047" => gettext("cp1047 (Latin-1 open systems)"),
        "cp1141" => gettext("cp1141 (Germany, euro)"),
        _ => label.to_string(),
    }
}

/// The translated name of a resource kind reported by `afp_core::inspect`.
fn resource_kind_label(kind: ResourceKind) -> String {
    match kind {
        ResourceKind::FontCharacterSet => gettext("font character set"),
        ResourceKind::CharacterSet => gettext("character set"),
        ResourceKind::CodedFont => gettext("coded font"),
        ResourceKind::CodePage => gettext("code page"),
        ResourceKind::PageOverlay => gettext("page overlay"),
        ResourceKind::PageSegment => gettext("page segment"),
        ResourceKind::IocaImage => gettext("IOCA image"),
        ResourceKind::GocaGraphics => gettext("GOCA graphics"),
        ResourceKind::BcocaBarCode => gettext("BCOCA bar code"),
        ResourceKind::ObjectContainer => gettext("object container"),
        ResourceKind::ResourceGroup => gettext("resource group"),
        ResourceKind::ObjectResource => gettext("object resource"),
    }
}

/// A colored notice above the content: title, paragraphs, and an optional
/// list of monospace items with tooltips.
fn banner(kind: &str, title: &str, paragraphs: &[String], items: &[(String, String)]) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    b.add_css_class("rafp-banner");
    b.add_css_class(kind);
    let heading = gtk::Label::builder().label(title).xalign(0.0).css_classes(["heading"]).build();
    b.append(&heading);
    for p in paragraphs {
        let l = gtk::Label::builder().label(safe(p)).wrap(true).xalign(0.0).selectable(true).build();
        b.append(&l);
    }
    for (text, tip) in items {
        let l = gtk::Label::builder().label(safe(text)).xalign(0.0).selectable(true).tooltip_text(safe(tip)).build();
        l.add_css_class("monospace");
        b.append(&l);
    }
    b
}
