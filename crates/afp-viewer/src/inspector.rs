//! Inspector pane: the structured-field table (a `GtkColumnView` whose rows
//! expand into their decoded triplets), field-type filters and the Find bar.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use afp_core::inspect::FieldRow;
use afp_core::parser::sf;
use gtk::glib::BoxedAnyObject;
use gtk::glib::clone;
use gtk::{gdk, gio, glib, prelude::*};

use crate::app::{Event, EventHandler};
use crate::i18n::{gettext, gettext_f, ngettext_f, safe};
use crate::worker::Doc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FindMode {
    Ptx,
    Nop,
    Tle,
    Type,
}

const FIND_MODES: [FindMode; 4] = [FindMode::Ptx, FindMode::Nop, FindMode::Tle, FindMode::Type];

impl FindMode {
    fn label(self) -> String {
        match self {
            FindMode::Ptx => gettext("PTX Text"),
            FindMode::Nop => gettext("NOP Text"),
            FindMode::Tle => gettext("TLE Tags"),
            FindMode::Type => gettext("Field Type"),
        }
    }
}

/// One line of the table: a field, or one of its triplets.
#[derive(Clone, Copy, Debug)]
enum Line {
    Field(usize),
    Triplet(usize, usize),
}

impl Line {
    fn row(self) -> usize {
        match self {
            Line::Field(i) | Line::Triplet(i, _) => i,
        }
    }
}

#[derive(Default)]
struct State {
    doc: Option<Rc<Doc>>,
    filter: HashSet<String>,
    query: String,
    mode: Option<FindMode>,
    case: bool,
    matches: Vec<usize>,
    match_set: HashSet<usize>,
    current_match: Option<usize>,
    current_page: Option<usize>,
    /// Lowercased (search text, name) of each row, for case-insensitive
    /// Find; computed on first use for each document.
    lowered: Option<Rc<Vec<(String, String)>>>,
}

impl State {
    fn lowered_rows(&mut self, doc: &Doc) -> Rc<Vec<(String, String)>> {
        self.lowered
            .get_or_insert_with(|| {
                Rc::new(doc.rows.iter().map(|r| (r.search_text.to_lowercase(), r.name.to_lowercase())).collect())
            })
            .clone()
    }

    fn mode(&self) -> FindMode {
        self.mode.unwrap_or(FindMode::Ptx)
    }

    fn haystack<'a>(&self, row: &'a FieldRow) -> Option<&'a str> {
        let mode = self.mode();
        if mode == FindMode::Type {
            return Some(&row.name);
        }
        let kind = match row.sf_id {
            sf::PTX => FindMode::Ptx,
            sf::NOP => FindMode::Nop,
            _ => FindMode::Tle,
        };
        (!row.search_text.is_empty() && kind == mode).then_some(row.search_text.as_str())
    }

    fn visible(&self, row: usize) -> bool {
        let Some(doc) = &self.doc else { return false };
        self.filter.is_empty() || self.filter.contains(&doc.rows[row].name) || self.match_set.contains(&row)
    }

    /// Row classes: Find hit, current Find hit.
    fn row_classes(&self, row: usize) -> [(&'static str, bool); 2] {
        let current = self.current_match.map(|c| self.matches[c]) == Some(row);
        [("find-current", current), ("find-match", !current && self.match_set.contains(&row))]
    }

    fn on_page(&self, row: usize) -> bool {
        let Some(doc) = &self.doc else { return false };
        self.current_page.is_some() && doc.rows[row].page == self.current_page
    }
}

/// A bound cell: its widget, the line it shows, and whether it is the
/// offset cell (which carries the current-page marker).
struct BoundCell {
    item: glib::WeakRef<gtk::ListItem>,
    column: usize,
    /// The cell widget, which carries the row classes.
    cell: glib::WeakRef<gtk::Widget>,
    line: Line,
}

/// The bound cells, by child widget: to restyle or rebind the visible rows.
type Bound = HashMap<usize, BoundCell>;

pub struct Inspector {
    pub root: gtk::Box,
    entry: gtk::SearchEntry,
    mode: gtk::DropDown,
    case: gtk::ToggleButton,
    prev: gtk::Button,
    next: gtk::Button,
    count: gtk::Label,
    counts: gtk::Expander,
    chips: gtk::FlowBox,
    /// The chip of each field type, by name.
    chip_by_name: RefCell<Vec<(String, gtk::ToggleButton)>>,
    clear_filter: gtk::Button,
    footer: gtk::Label,
    view: gtk::ColumnView,
    store: gio::ListStore,
    filter: gtk::CustomFilter,
    tree: gtk::TreeListModel,
    selection: gtk::SingleSelection,
    state: Rc<RefCell<State>>,
    bound: Rc<RefCell<Bound>>,
    /// Set while the selection or chips change programmatically.
    quiet: Cell<bool>,
    events: RefCell<Option<EventHandler>>,
}

fn line_of(item: Option<glib::Object>) -> Option<Line> {
    let row = item?.downcast::<gtk::TreeListRow>().ok()?;
    let obj = row.item()?.downcast::<BoxedAnyObject>().ok()?;
    Some(*obj.borrow::<Line>())
}

fn is_dark() -> bool {
    adw::StyleManager::default().is_dark()
}

fn hex_color() -> &'static str {
    if is_dark() { "#9ab88c" } else { "#3a7a28" }
}

fn link_color() -> &'static str {
    if is_dark() { "#78aeed" } else { "#1c71d8" }
}

fn esc(s: &str) -> glib::GString {
    glib::markup_escape_text(&safe(s))
}

fn mono(text: &str, color: &str) -> String {
    format!("<span font_family=\"monospace\" foreground=\"{color}\">{}</span>", esc(text))
}

impl Inspector {
    pub fn new() -> Rc<Inspector> {
        let state: Rc<RefCell<State>> = Rc::default();

        // Find bar.
        let entry =
            gtk::SearchEntry::builder().placeholder_text(gettext("Find…")).hexpand(true).width_chars(14).build();
        let labels = FIND_MODES.map(FindMode::label);
        let mode = gtk::DropDown::from_strings(&labels.iter().map(String::as_str).collect::<Vec<_>>());
        mode.set_tooltip_text(Some(&gettext("What to search")));
        let case = gtk::ToggleButton::builder().label("Aa").tooltip_text(gettext("Match Case")).build();
        let prev = gtk::Button::builder().icon_name("go-up-symbolic").tooltip_text(gettext("Previous Match")).build();
        let next = gtk::Button::builder().icon_name("go-down-symbolic").tooltip_text(gettext("Next Match")).build();
        let arrows = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        arrows.add_css_class("linked");
        arrows.append(&prev);
        arrows.append(&next);
        let count = gtk::Label::builder().css_classes(["dim-label", "numeric"]).width_chars(9).xalign(1.0).build();
        let find_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        for w in [entry.upcast_ref::<gtk::Widget>(), mode.upcast_ref(), case.upcast_ref(), arrows.upcast_ref()] {
            find_bar.append(w);
        }
        find_bar.append(&count);

        // Field counts / type filter.
        let chips = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .column_spacing(4)
            .row_spacing(4)
            .max_children_per_line(64)
            .homogeneous(false)
            .valign(gtk::Align::Start)
            .build();
        let chips_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .max_content_height(150)
            .propagate_natural_height(true)
            .child(&chips)
            .build();
        let hint = gtk::Label::builder()
            .label(gettext("Click a type to filter the table (several at once)."))
            .css_classes(["dim-label", "caption"])
            .xalign(0.0)
            .build();
        let clear_filter =
            gtk::Button::builder().label(gettext("Clear Filter")).halign(gtk::Align::Start).visible(false).build();
        clear_filter.add_css_class("small");
        let counts_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        counts_box.set_margin_top(4);
        counts_box.append(&hint);
        counts_box.append(&chips_scroll);
        counts_box.append(&clear_filter);
        let counts = gtk::Expander::builder().expanded(true).child(&counts_box).build();

        // The table.
        let store = gio::ListStore::new::<BoxedAnyObject>();
        let filter = {
            let state = state.clone();
            gtk::CustomFilter::new(move |obj| {
                let Some(obj) = obj.downcast_ref::<BoxedAnyObject>() else { return true };
                let line = *obj.borrow::<Line>();
                state.borrow().visible(line.row())
            })
        };
        let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
        filtered.set_incremental(false);
        let tree = {
            let state = state.clone();
            gtk::TreeListModel::new(filtered, false, false, move |obj| {
                let line = *obj.downcast_ref::<BoxedAnyObject>()?.borrow::<Line>();
                let Line::Field(i) = line else { return None };
                let n = state.borrow().doc.as_ref()?.rows[i].triplets.len();
                if n == 0 {
                    return None;
                }
                let children = gio::ListStore::new::<BoxedAnyObject>();
                let items: Vec<BoxedAnyObject> = (0..n).map(|t| BoxedAnyObject::new(Line::Triplet(i, t))).collect();
                children.extend_from_slice(&items);
                Some(children.upcast())
            })
        };
        let selection = gtk::SingleSelection::builder().model(&tree).autoselect(false).can_unselect(true).build();
        let view = gtk::ColumnView::builder().model(&selection).show_column_separators(true).reorderable(false).build();
        view.add_css_class("inspector-table");
        view.add_css_class("data-table");
        let table_scroll = gtk::ScrolledWindow::builder().vexpand(true).child(&view).build();
        table_scroll.add_css_class("card");

        let footer = gtk::Label::builder()
            .css_classes(["dim-label"])
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .visible(false)
            .build();

        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_start(6)
            .margin_end(6)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        root.append(&find_bar);
        root.append(&counts);
        root.append(&table_scroll);
        root.append(&footer);

        let inspector = Rc::new(Inspector {
            root,
            entry,
            mode,
            case,
            prev,
            next,
            count,
            counts,
            chips,
            chip_by_name: RefCell::default(),
            clear_filter,
            footer,
            view,
            store,
            filter,
            tree,
            selection,
            state,
            bound: Rc::default(),
            quiet: Cell::new(false),
            events: RefCell::default(),
        });
        inspector.add_columns();
        inspector.connect_signals();
        inspector.update_count();
        inspector
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

    fn add_columns(self: &Rc<Self>) {
        let this = self;
        let columns: [(String, i32, bool); 6] = [
            (gettext("Offset"), 80, false),
            // Translators: column of structured-field identifiers (hexadecimal codes).
            (gettext("ID"), 96, false),
            (gettext("Structured Field"), 220, false),
            (gettext("Name"), 84, false),
            (gettext("Size"), 60, false),
            (gettext("Field Data"), 240, true),
        ];
        for (col, (title, width, expand)) in columns.into_iter().enumerate() {
            let factory = gtk::SignalListItemFactory::new();
            factory.connect_setup(move |_, item| {
                let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
                let label = gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::End).build();
                if col == 2 {
                    let expander = gtk::TreeExpander::builder().indent_for_icon(true).child(&label).build();
                    item.set_child(Some(&expander));
                } else {
                    item.set_child(Some(&label));
                }
            });
            factory.connect_bind(clone!(
                #[weak]
                this,
                move |_, item| {
                    let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
                    this.bind_cell(item, col);
                }
            ));
            factory.connect_unbind(clone!(
                #[weak]
                this,
                move |_, item| {
                    let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
                    if let Some(child) = item.child() {
                        this.bound.borrow_mut().remove(&(child.as_ptr() as usize));
                    }
                }
            ));
            let column = gtk::ColumnViewColumn::builder()
                .title(title)
                .factory(&factory)
                .resizable(true)
                .fixed_width(width)
                .expand(expand)
                .build();
            self.view.append_column(&column);
        }
    }

    fn bind_cell(&self, item: &gtk::ListItem, col: usize) {
        let Some(line) = line_of(item.item()) else { return };
        let Some(child) = item.child() else { return };
        let state = self.state.borrow();
        let Some(doc) = state.doc.clone() else { return };
        let label = match child.downcast_ref::<gtk::TreeExpander>() {
            Some(expander) => {
                expander.set_list_row(item.item().and_downcast_ref::<gtk::TreeListRow>());
                expander.set_margin_start(doc.rows[line.row()].depth as i32 * 14);
                expander.child().and_downcast::<gtk::Label>()
            }
            None => child.clone().downcast::<gtk::Label>().ok(),
        };
        let Some(label) = label else { return };
        label.set_tooltip_text(None);
        label.set_css_classes(&[]);
        let hex = hex_color();
        match line {
            Line::Field(i) => {
                let row = &doc.rows[i];
                match col {
                    0 => {
                        label.set_markup(&mono(&row.offset.to_string(), "gray"));
                    }
                    1 => label.set_markup(&mono(&row.sf_id_hex(), hex)),
                    2 => label.set_text(&safe(&row.name)),
                    3 => {
                        label.set_text(&safe(&row.token));
                        label.add_css_class("monospace");
                    }
                    4 => {
                        label.set_text(&row.size.to_string());
                        label.add_css_class("monospace");
                        label.set_tooltip_text(Some(&ngettext_f(
                            "{n} data byte",
                            "{n} data bytes",
                            row.data_len,
                            &[("n", &row.data_len.to_string())],
                        )));
                    }
                    _ => {
                        label.set_markup(&data_markup(&state, row, i));
                        if !row.data.is_empty() {
                            label.set_tooltip_text(Some(&safe(&gettext_f(
                                "{data}\n\nhex: {hex}",
                                &[("data", &row.data), ("hex", &row.preview)],
                            ))));
                        }
                    }
                }
            }
            Line::Triplet(i, t) => {
                let trip = &doc.rows[i].triplets[t];
                match col {
                    1 => label.set_markup(&mono(&trip.id, hex)),
                    2 => {
                        let rg = trip.group.map(|g| format!("RG {g} · ")).unwrap_or_default();
                        label.set_text(&safe(&format!("{rg}{}", trip.name)));
                        label.set_css_classes(&["dim-label", "caption"]);
                    }
                    5 => {
                        let mut markup = String::new();
                        if !trip.detail.is_empty() {
                            markup.push_str(&esc(&trip.detail));
                            markup.push_str("   ");
                        }
                        markup.push_str(&format!("<small>{}</small>", mono(&trip.hex, hex)));
                        label.set_markup(&markup);
                        let tip = if trip.detail.is_empty() {
                            trip.hex.clone()
                        } else {
                            format!("{}\n\n{}", trip.detail, trip.hex)
                        };
                        label.set_tooltip_text(Some(&safe(&tip)));
                    }
                    _ => label.set_text(""),
                }
            }
        }
        let cell: gtk::Widget = child.parent().unwrap_or_else(|| child.clone());
        apply_classes(&cell, &state, line, col == 0);
        drop(state);
        self.bound.borrow_mut().insert(
            child.as_ptr() as usize,
            BoundCell { item: item.downgrade(), column: col, cell: cell.downgrade(), line },
        );
    }

    fn connect_signals(self: &Rc<Self>) {
        let this = self;
        self.entry.connect_search_changed(clone!(
            #[weak]
            this,
            move |e| {
                this.state.borrow_mut().query = e.text().to_string();
                this.recompute_find(true);
            }
        ));
        self.entry.connect_activate(clone!(
            #[weak]
            this,
            move |_| this.step(1)
        ));
        self.entry.connect_next_match(clone!(
            #[weak]
            this,
            move |_| this.step(1)
        ));
        self.entry.connect_previous_match(clone!(
            #[weak]
            this,
            move |_| this.step(-1)
        ));
        // Shift+Enter: previous match.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(clone!(
            #[weak]
            this,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, mods| {
                let enter = matches!(key, gdk::Key::Return | gdk::Key::KP_Enter);
                if !(enter && mods.contains(gdk::ModifierType::SHIFT_MASK)) {
                    return glib::Propagation::Proceed;
                }
                this.step(-1);
                glib::Propagation::Stop
            }
        ));
        self.entry.add_controller(keys);
        self.mode.connect_selected_notify(clone!(
            #[weak]
            this,
            move |d| {
                this.state.borrow_mut().mode = FIND_MODES.get(d.selected() as usize).copied();
                this.recompute_find(true);
            }
        ));
        self.case.connect_toggled(clone!(
            #[weak]
            this,
            move |b| {
                this.state.borrow_mut().case = b.is_active();
                this.recompute_find(true);
            }
        ));
        self.prev.connect_clicked(clone!(
            #[weak]
            this,
            move |_| this.step(-1)
        ));
        self.next.connect_clicked(clone!(
            #[weak]
            this,
            move |_| this.step(1)
        ));
        self.clear_filter.connect_clicked(clone!(
            #[weak]
            this,
            move |_| {
                this.state.borrow_mut().filter.clear();
                this.filter_changed();
            }
        ));
        self.selection.connect_selection_changed(clone!(
            #[weak]
            this,
            move |sel, _, _| {
                if !this.quiet.get()
                    && let Some(Line::Field(i)) = line_of(sel.selected_item())
                {
                    this.emit(Event::RowSelected(i));
                }
            }
        ));
        // Enter / double-click toggles a field's triplets.
        self.view.connect_activate(clone!(
            #[weak]
            this,
            move |_, pos| {
                if let Some(row) = this.tree.item(pos).and_downcast::<gtk::TreeListRow>() {
                    row.set_expanded(!row.is_expanded());
                }
            }
        ));
        adw::StyleManager::default().connect_dark_notify(clone!(
            #[weak]
            this,
            move |_| this.rebind_all()
        ));
    }

    /// Show a document; when it is the same file re-decoded, keep the filter
    /// and the current Find match instead of jumping to the first one.
    pub fn set_doc(&self, doc: Option<Rc<Doc>>, same_file: bool) {
        let previous_match = if same_file { self.state.borrow().current_match } else { None };
        {
            let mut st = self.state.borrow_mut();
            st.doc = doc.clone();
            st.lowered = None;
            st.current_page = None;
            if !same_file {
                st.filter.clear();
            }
            st.matches.clear();
            st.match_set.clear();
            st.current_match = None;
        }
        self.bound.borrow_mut().clear();
        let n = doc.as_ref().map_or(0, |d| d.rows.len());
        let items: Vec<BoxedAnyObject> = (0..n).map(|i| BoxedAnyObject::new(Line::Field(i))).collect();
        self.store.remove_all();
        self.store.extend_from_slice(&items);
        self.rebuild_chips();
        self.recompute_find(!same_file);
        if let Some(c) = previous_match {
            {
                let mut st = self.state.borrow_mut();
                if c < st.matches.len() {
                    st.current_match = Some(c);
                }
            }
            self.refresh_marks();
            self.update_count();
        }
    }

    /// The summary line shown under the table (inspector-only documents).
    pub fn set_footer(&self, text: Option<&str>) {
        self.footer.set_visible(text.is_some());
        self.footer.set_text(text.unwrap_or_default());
    }

    pub fn focus_table(&self) {
        self.view.grab_focus();
    }

    pub fn focus_find(&self) {
        self.entry.grab_focus();
    }

    pub fn set_query(&self, query: &str) {
        self.entry.set_text(query);
    }

    fn rebuild_chips(&self) {
        while let Some(child) = self.chips.first_child() {
            self.chips.remove(&child);
        }
        self.chip_by_name.borrow_mut().clear();
        let state = self.state.borrow();
        let Some(doc) = &state.doc else { return };
        let n = doc.summary.len();
        self.counts.set_label(Some(&ngettext_f(
            "Field Counts ({n} type)",
            "Field Counts ({n} types)",
            n,
            &[("n", &n.to_string())],
        )));
        let state_rc = Rc::downgrade(&self.state);
        for (name, n) in &doc.summary {
            let label = gtk::Label::new(None);
            label.set_markup(&format!("{} <span alpha=\"60%\">{n}</span>", esc(name)));
            let chip = gtk::ToggleButton::builder().child(&label).active(state.filter.contains(name)).build();
            chip.add_css_class("field-chip");
            chip.set_valign(gtk::Align::Center);
            let name = name.clone();
            self.chip_by_name.borrow_mut().push((name.clone(), chip.clone()));
            let state_rc = state_rc.clone();
            let this_filter = self.filter.clone();
            let clear = self.clear_filter.clone();
            chip.connect_toggled(move |b| {
                let Some(state) = state_rc.upgrade() else { return };
                let name = name.clone();
                {
                    let mut st = state.borrow_mut();
                    if b.is_active() == st.filter.contains(&name) {
                        return; // programmatic sync
                    }
                    if b.is_active() {
                        st.filter.insert(name);
                    } else {
                        st.filter.remove(&name);
                    }
                    clear.set_visible(!st.filter.is_empty());
                }
                this_filter.changed(gtk::FilterChange::Different);
            });
            self.chips.append(&chip);
        }
        self.clear_filter.set_visible(!state.filter.is_empty());
    }

    /// The filter set changed: refilter and sync the chips.
    fn filter_changed(&self) {
        let active: HashSet<String> = self.state.borrow().filter.clone();
        for (name, chip) in self.chip_by_name.borrow().iter() {
            chip.set_active(active.contains(name));
        }
        self.clear_filter.set_visible(!active.is_empty());
        self.filter.changed(gtk::FilterChange::Different);
    }

    /// Recompute the Find matches (clears the field-type filter, like the
    /// original: find and filter don't mix).
    /// Recompute the Find matches; with `jump`, go to the first one.
    fn recompute_find(&self, jump: bool) {
        let cleared_filter = {
            let mut guard = self.state.borrow_mut();
            let st = &mut *guard;
            st.matches.clear();
            st.current_match = None;
            let mut cleared = false;
            if let Some(doc) = st.doc.clone()
                && !st.query.is_empty()
            {
                cleared = !st.filter.is_empty();
                st.filter.clear();
                let needle = if st.case { st.query.clone() } else { st.query.to_lowercase() };
                let lowered = (!st.case).then(|| st.lowered_rows(&doc));
                let by_type = st.mode() == FindMode::Type;
                for (i, row) in doc.rows.iter().enumerate() {
                    let Some(hay) = st.haystack(row) else { continue };
                    let hay = match &lowered {
                        Some(low) if by_type => low[i].1.as_str(),
                        Some(low) => low[i].0.as_str(),
                        None => hay,
                    };
                    if hay.contains(&needle) {
                        st.matches.push(i);
                    }
                }
                if !st.matches.is_empty() {
                    st.current_match = Some(0);
                }
            }
            st.match_set = st.matches.iter().copied().collect();
            cleared
        };
        if cleared_filter {
            self.filter_changed();
        }
        self.rebind_all();
        self.update_count();
        if jump {
            self.focus_current();
        }
    }

    fn step(&self, delta: isize) {
        {
            let mut st = self.state.borrow_mut();
            if st.matches.is_empty() {
                return;
            }
            let n = st.matches.len() as isize;
            let cur = st.current_match.unwrap_or(0) as isize;
            st.current_match = Some(((cur + delta + n) % n) as usize);
        }
        self.refresh_marks();
        self.update_count();
        self.focus_current();
    }

    fn update_count(&self) {
        let st = self.state.borrow();
        let text = match st.current_match {
            Some(c) => gettext_f(
                "{current} / {total}",
                &[("current", &(c + 1).to_string()), ("total", &st.matches.len().to_string())],
            ),
            None if !st.query.is_empty() => gettext("No matches"),
            None => String::new(),
        };
        self.count.set_text(&text);
        let several = st.matches.len() >= 2;
        self.prev.set_sensitive(several);
        self.next.set_sensitive(several);
    }

    fn focus_current(&self) {
        let (row, ptx) = {
            let st = self.state.borrow();
            let Some(c) = st.current_match else { return };
            (st.matches[c], st.mode() == FindMode::Ptx)
        };
        self.scroll_to_row(row, false);
        if ptx {
            self.emit(Event::FindHit(row));
        }
    }

    /// Mark the rows of the shown page.
    pub fn set_current_page(&self, page: Option<usize>) {
        if self.state.borrow().current_page == page {
            return;
        }
        self.state.borrow_mut().current_page = page;
        self.refresh_marks();
    }

    /// Re-apply the row classes on the bound cells.
    fn refresh_marks(&self) {
        let state = self.state.borrow();
        for b in self.bound.borrow().values() {
            if let Some(w) = b.cell.upgrade() {
                apply_classes(&w, &state, b.line, b.column == 0);
            }
        }
    }

    /// Rebind every visible cell (text highlight or theme changed).
    fn rebind_all(&self) {
        let cells: Vec<(gtk::ListItem, usize)> =
            self.bound.borrow().values().filter_map(|b| Some((b.item.upgrade()?, b.column))).collect();
        for (item, column) in cells {
            self.bind_cell(&item, column);
        }
    }

    /// Field index shown at a table position.
    fn field_at(&self, pos: u32) -> Option<usize> {
        line_of(self.tree.item(pos)).map(Line::row)
    }

    /// Scroll to (and optionally select) field `row`, or the nearest
    /// visible one after it.
    pub fn scroll_to_row(&self, row: usize, select: bool) {
        let n = self.tree.n_items();
        if n == 0 {
            return;
        }
        // Positions are ordered by field index: binary search.
        let (mut lo, mut hi) = (0u32, n);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.field_at(mid).is_some_and(|f| f < row) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let pos = lo.min(n - 1);
        let flags = if select { gtk::ListScrollFlags::SELECT } else { gtk::ListScrollFlags::NONE };
        self.quiet.set(true);
        self.view.scroll_to(pos, None::<&gtk::ColumnViewColumn>, flags, None);
        self.quiet.set(false);
    }
}

/// The field-data cell: decoded summary (Find hits highlighted) or hex,
/// plus the triplet count.
fn data_markup(state: &State, row: &FieldRow, i: usize) -> String {
    let n = row.triplets.len();
    let suffix = if n == 0 {
        String::new()
    } else {
        let label = ngettext_f("{n} triplet", "{n} triplets", n, &[("n", &n.to_string())]);
        format!("   <span foreground=\"{}\">{}</span>", link_color(), esc(&label))
    };
    if row.data.is_empty() {
        return format!("{}{suffix}", mono(&row.preview, hex_color()));
    }
    let highlight = state.match_set.contains(&i) && state.mode() != FindMode::Type && !state.query.is_empty();
    let text = if highlight { highlighted(&row.data, &state.query, state.case) } else { esc(&row.data).to_string() };
    format!("{text}{suffix}")
}

fn apply_classes(widget: &gtk::Widget, state: &State, line: Line, offset_cell: bool) {
    let field = matches!(line, Line::Field(_));
    for (class, on) in state.row_classes(line.row()) {
        set_class(widget, class, field && on);
    }
    set_class(widget, "on-page", offset_cell && field && state.on_page(line.row()));
}

fn set_class(widget: &gtk::Widget, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

/// Escaped `text` with every occurrence of `needle` highlighted.
fn highlighted(text: &str, needle: &str, case: bool) -> String {
    if needle.is_empty() {
        return esc(text).to_string();
    }
    // Case-insensitive matching on a lowered copy with a char-aligned map
    // back to the original byte offsets.
    let (hay, map): (String, Vec<usize>) = if case {
        (text.to_string(), text.char_indices().map(|(b, _)| b).collect())
    } else {
        let mut hay = String::new();
        let mut map = Vec::new();
        for (b, c) in text.char_indices() {
            for lc in c.to_lowercase() {
                hay.push(lc);
                map.push(b);
            }
        }
        (hay, map)
    };
    let nee = if case { needle.to_string() } else { needle.to_lowercase() };
    let hay_chars: Vec<(usize, char)> = hay.char_indices().collect();
    let to_orig = |hay_byte: usize| -> usize {
        let ci = hay_chars.iter().position(|(b, _)| *b == hay_byte).unwrap_or(hay_chars.len());
        map.get(ci).copied().unwrap_or(text.len())
    };
    let mut out = String::new();
    let mut pos = 0usize; // in original
    let mut search_from = 0usize; // in hay
    while let Some(found) = hay[search_from..].find(&nee) {
        let start_h = search_from + found;
        let end_h = start_h + nee.len();
        let (start, end) = (to_orig(start_h), to_orig(end_h));
        if start < pos {
            break;
        }
        out.push_str(&esc(&text[pos..start]));
        out.push_str("<span background=\"#ffd84a\" foreground=\"black\">");
        out.push_str(&esc(&text[start..end]));
        out.push_str("</span>");
        pos = end;
        search_from = end_h;
        if search_from >= hay.len() {
            break;
        }
    }
    out.push_str(&esc(&text[pos..]));
    out
}
