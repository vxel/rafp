//! The page canvas: draws the rasterized page at the current zoom, centered
//! in the view, with the flash and fidelity overlays on top. It scrolls
//! itself (`GtkScrollable`) so a zoom can keep the point under the pointer
//! in place within the same layout pass.

use std::cell::{Cell, RefCell};

use afp_core::raster::BoxF;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib, graphene, gsk, prelude::*};

use crate::settings::Zoom;

/// Ctrl + wheel zoom limits (1.0 = real size).
const MIN_ZOOM: f32 = 0.1;
const MAX_ZOOM: f32 = 8.0;
/// Space around the page.
const MARGIN: f32 = 12.0;
/// Largest texture side rendered, in pixels (deeper zooms are upscaled).
const MAX_TEXTURE_SIDE: f32 = 12_000.0;

const FLASH: (f32, f32, f32) = (1.0, 0.416, 0.0);
const FIDELITY: (f32, f32, f32) = (1.0, 0.69, 0.125);

type ZoomHandler = Box<dyn Fn(Zoom)>;

/// Page size in L-units.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PageGeom {
    pub width: f32,
    pub height: f32,
    pub units_per_inch: f32,
}

mod imp {
    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::PageCanvas)]
    pub struct PageCanvas {
        #[property(override_interface = gtk::Scrollable, get, set = Self::set_hadjustment)]
        pub hadjustment: RefCell<Option<gtk::Adjustment>>,
        #[property(override_interface = gtk::Scrollable, get, set = Self::set_vadjustment)]
        pub vadjustment: RefCell<Option<gtk::Adjustment>>,
        #[property(override_interface = gtk::Scrollable, get, set)]
        pub hscroll_policy: Cell<gtk::ScrollablePolicy>,
        #[property(override_interface = gtk::Scrollable, get, set)]
        pub vscroll_policy: Cell<gtk::ScrollablePolicy>,

        pub geom: Cell<Option<PageGeom>>,
        pub zoom: Cell<Zoom>,
        pub texture: RefCell<Option<gdk::Texture>>,
        pub message: RefCell<Option<String>>,
        pub flash: RefCell<Vec<BoxF>>,
        pub flash_alpha: Cell<f32>,
        pub fidelity: RefCell<Vec<BoxF>>,
        /// Displayed page size (points) and its top-left corner in content
        /// coordinates, from the last layout.
        pub size: Cell<(f32, f32)>,
        pub origin: Cell<(f32, f32)>,
        /// Keep this page point (fractions of the page) under this view
        /// point at the next layout.
        pub anchor: Cell<Option<(f32, f32, f32, f32)>>,
        pub on_layout: RefCell<Option<Box<dyn Fn()>>>,
        pub on_zoom: RefCell<Option<ZoomHandler>>,
        /// `value-changed` handlers on the current adjustments.
        pub adjustment_handlers: RefCell<Vec<(gtk::Adjustment, glib::SignalHandlerId)>>,
    }

    impl Default for PageCanvas {
        fn default() -> Self {
            PageCanvas {
                hadjustment: RefCell::default(),
                vadjustment: RefCell::default(),
                hscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
                vscroll_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
                geom: Cell::new(None),
                zoom: Cell::new(Zoom::FitWidth),
                texture: RefCell::default(),
                message: RefCell::default(),
                flash: RefCell::default(),
                flash_alpha: Cell::new(0.0),
                fidelity: RefCell::default(),
                size: Cell::new((0.0, 0.0)),
                origin: Cell::new((0.0, 0.0)),
                anchor: Cell::new(None),
                on_layout: RefCell::default(),
                on_zoom: RefCell::default(),
                adjustment_handlers: RefCell::default(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PageCanvas {
        const NAME: &'static str = "RafpPageCanvas";
        type Type = super::PageCanvas;
        type ParentType = gtk::Widget;
        type Interfaces = (gtk::Scrollable,);

        fn class_init(klass: &mut Self::Class) {
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for PageCanvas {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.connect_scale_factor_notify(|c| c.imp().emit_layout());
        }
    }

    impl ScrollableImpl for PageCanvas {}

    impl PageCanvas {
        fn set_hadjustment(&self, adj: Option<gtk::Adjustment>) {
            self.set_adjustment(&self.hadjustment, adj);
        }

        fn set_vadjustment(&self, adj: Option<gtk::Adjustment>) {
            self.set_adjustment(&self.vadjustment, adj);
        }

        fn set_adjustment(&self, slot: &RefCell<Option<gtk::Adjustment>>, adj: Option<gtk::Adjustment>) {
            if let Some(old) = slot.borrow().as_ref() {
                let mut handlers = self.adjustment_handlers.borrow_mut();
                if let Some(i) = handlers.iter().position(|(a, _)| a == old) {
                    let (a, id) = handlers.swap_remove(i);
                    a.disconnect(id);
                }
            }
            if let Some(adj) = &adj {
                let obj = self.obj().downgrade();
                let id = adj.connect_value_changed(move |_| {
                    if let Some(obj) = obj.upgrade() {
                        obj.queue_draw();
                    }
                });
                self.adjustment_handlers.borrow_mut().push((adj.clone(), id));
            }
            slot.replace(adj);
            self.obj().queue_allocate();
        }

        pub fn scroll(&self) -> (f32, f32) {
            let value = |a: &RefCell<Option<gtk::Adjustment>>| a.borrow().as_ref().map_or(0.0, |a| a.value() as f32);
            (value(&self.hadjustment), value(&self.vadjustment))
        }

        pub fn display_size(&self, view_w: f32, view_h: f32) -> (f32, f32) {
            let Some(g) = self.geom.get() else { return (0.0, 0.0) };
            let width = match self.zoom.get() {
                Zoom::FitWidth => (view_w - 2.0 * MARGIN).max(120.0),
                Zoom::FitHeight => ((view_h - 2.0 * MARGIN) * g.width / g.height).max(120.0),
                Zoom::Factor(z) => g.width / g.units_per_inch * 96.0 * z,
            };
            (width, width * g.height / g.width)
        }

        pub fn emit_layout(&self) {
            if let Some(f) = self.on_layout.borrow().as_ref() {
                f();
            }
        }

        /// A page box (L-units) in view coordinates.
        pub fn to_view(&self, b: &BoxF) -> graphene::Rect {
            let Some(g) = self.geom.get() else { return graphene::Rect::zero() };
            let (sw, sh) = self.size.get();
            let (ox, oy) = self.origin.get();
            let (hx, vy) = self.scroll();
            let (sx, sy) = (sw / g.width, sh / g.height);
            graphene::Rect::new(ox - hx + b.x * sx, oy - vy + b.y * sy, (b.w * sx).max(1.0), (b.h * sy).max(1.0))
        }
    }

    fn rgba((r, g, b): (f32, f32, f32), a: f32) -> gdk::RGBA {
        gdk::RGBA::new(r, g, b, a)
    }

    fn grow(r: &graphene::Rect, dx: f32, dy: f32) -> graphene::Rect {
        graphene::Rect::new(r.x() - dx, r.y() - dy, r.width() + 2.0 * dx, r.height() + 2.0 * dy)
    }

    fn outline(snapshot: &gtk::Snapshot, r: &graphene::Rect, radius: f32, width: f32, color: gdk::RGBA) {
        let rounded = gsk::RoundedRect::from_rect(*r, radius);
        snapshot.append_border(&rounded, &[width; 4], &[color; 4]);
    }

    impl WidgetImpl for PageCanvas {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            // The content size goes to the adjustments, not to the parent.
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            let (w, h) = (width as f32, height as f32);
            let before = self.size.get();
            let (sw, sh) = self.display_size(w, h);
            let content_w = (sw + 2.0 * MARGIN).max(w);
            let content_h = (sh + 2.0 * MARGIN).max(h);
            let origin = ((content_w - sw) / 2.0, MARGIN);
            self.size.set((sw, sh));
            self.origin.set(origin);
            let (mut hx, mut vy) = self.scroll();
            if let Some((fx, fy, px, py)) = self.anchor.take() {
                hx = origin.0 + fx * sw - px;
                vy = origin.1 + fy * sh - py;
            }
            let configure = |adj: &RefCell<Option<gtk::Adjustment>>, value: f32, upper: f32, page: f32| {
                if let Some(adj) = adj.borrow().as_ref() {
                    let value = value.clamp(0.0, (upper - page).max(0.0));
                    adj.configure(value as f64, 0.0, upper as f64, 40.0, page as f64 * 0.9, page as f64);
                }
            };
            configure(&self.hadjustment, hx, content_w, w);
            configure(&self.vadjustment, vy, content_h, h);
            if before != (sw, sh) {
                self.emit_layout();
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let Some(g) = self.geom.get() else { return };
            let (sw, _) = self.size.get();
            let page = self.to_view(&BoxF { x: 0.0, y: 0.0, w: g.width, h: g.height });
            let shadow = gsk::RoundedRect::from_rect(page, 0.0);
            snapshot.append_outset_shadow(&shadow, &gdk::RGBA::new(0.0, 0.0, 0.0, 0.35), 0.0, 1.0, 0.0, 6.0);
            snapshot.append_color(&gdk::RGBA::WHITE, &page);
            if let Some(tex) = self.texture.borrow().as_ref() {
                snapshot.append_scaled_texture(tex, gsk::ScalingFilter::Linear, &page);
            } else if let Some(msg) = self.message.borrow().as_deref() {
                let layout = self.obj().create_pango_layout(Some(msg));
                let (lw, _) = layout.pixel_size();
                snapshot.save();
                snapshot.translate(&graphene::Point::new(page.x() + (sw - lw as f32) / 2.0, page.y() + 32.0));
                snapshot.append_layout(&layout, &gdk::RGBA::new(0.45, 0.45, 0.5, 1.0));
                snapshot.restore();
            }
            let alpha = self.flash_alpha.get();
            if alpha > 0.0 {
                for b in self.flash.borrow().iter() {
                    let r = grow(&self.to_view(b), 2.0, 2.0);
                    snapshot.append_color(&rgba(FLASH, alpha * 0.45), &r);
                    outline(snapshot, &grow(&r, 2.0, 2.0), 2.0, 2.0, rgba(FLASH, alpha));
                }
            }
            for b in self.fidelity.borrow().iter() {
                let r = fidelity_rect(&self.to_view(b));
                let radius = (r.height() * 0.06).max(2.0);
                snapshot.append_color(&rgba(FIDELITY, 0.18), &r);
                outline(snapshot, &grow(&r, 1.0, 1.0), radius, 1.0, rgba(FIDELITY, 1.0));
            }
        }
    }

    /// The highlighted area of a fidelity-noted element (padded box).
    pub fn fidelity_rect(r: &graphene::Rect) -> graphene::Rect {
        let pad = (r.height() * 0.12).max(3.0);
        grow(r, pad, pad * 0.4)
    }
}

glib::wrapper! {
    pub struct PageCanvas(ObjectSubclass<imp::PageCanvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

impl Default for PageCanvas {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl PageCanvas {
    /// Called after a layout changed the displayed page size (re-render).
    pub fn connect_layout(&self, f: impl Fn() + 'static) {
        self.imp().on_layout.replace(Some(Box::new(f)));
    }

    /// Called when the zoom changes (menu, Ctrl + wheel).
    pub fn connect_zoom(&self, f: impl Fn(Zoom) + 'static) {
        self.imp().on_zoom.replace(Some(Box::new(f)));
    }

    pub fn set_geom(&self, geom: Option<PageGeom>) {
        if self.imp().geom.replace(geom) != geom {
            self.queue_allocate();
        }
        self.queue_draw();
    }

    pub fn zoom(&self) -> Zoom {
        self.imp().zoom.get()
    }

    pub fn set_zoom(&self, zoom: Zoom) {
        let imp = self.imp();
        if imp.zoom.replace(zoom) == zoom {
            return;
        }
        // Keep the center of the view in place.
        let (w, h) = (self.width() as f32, self.height() as f32);
        imp.anchor.set(self.anchor_at(w / 2.0, h / 2.0));
        self.queue_allocate();
        if let Some(f) = imp.on_zoom.borrow().as_ref() {
            f(zoom);
        }
    }

    /// Multiply the zoom by `factor`, keeping the page point under the
    /// view point (`x`, `y`) where it is.
    pub fn zoom_at(&self, factor: f32, x: f32, y: f32) {
        let imp = self.imp();
        let Some(g) = imp.geom.get() else { return };
        let real = g.width / g.units_per_inch * 96.0;
        let old = imp.size.get().0 / real;
        let new = (old * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        if (new - old).abs() < 1e-4 {
            return;
        }
        imp.zoom.set(Zoom::Factor(new));
        imp.anchor.set(self.anchor_at(x, y));
        self.queue_allocate();
        if let Some(f) = imp.on_zoom.borrow().as_ref() {
            f(Zoom::Factor(new));
        }
    }

    fn anchor_at(&self, x: f32, y: f32) -> Option<(f32, f32, f32, f32)> {
        let imp = self.imp();
        let (sw, sh) = imp.size.get();
        if sw <= 0.0 || sh <= 0.0 {
            return None;
        }
        let (ox, oy) = imp.origin.get();
        let (hx, vy) = imp.scroll();
        Some(((hx + x - ox) / sw, (vy + y - oy) / sh, x, y))
    }

    pub fn scroll_to_top(&self) {
        if let Some(adj) = self.imp().vadjustment.borrow().as_ref() {
            adj.set_value(0.0);
        }
    }

    /// Pixels per L-unit for a sharp render at the current zoom.
    pub fn render_scale(&self) -> Option<f32> {
        let imp = self.imp();
        let g = imp.geom.get()?;
        let (sw, _) = imp.size.get();
        if sw <= 0.0 {
            return None;
        }
        let device = self.native().and_then(|n| n.surface()).map_or(self.scale_factor() as f64, |s| s.scale());
        let mut scale = sw * device as f32 / g.width;
        let longest = g.width.max(g.height) * scale;
        if longest > MAX_TEXTURE_SIDE {
            scale *= MAX_TEXTURE_SIDE / longest;
        }
        Some(scale)
    }

    pub fn set_texture(&self, texture: Option<gdk::Texture>) {
        self.imp().texture.replace(texture);
        self.queue_draw();
    }

    pub fn set_message(&self, message: Option<&str>) {
        self.imp().message.replace(message.map(str::to_string));
        self.queue_draw();
    }

    pub fn set_flash(&self, boxes: Vec<BoxF>, alpha: f32) {
        self.imp().flash.replace(boxes);
        self.imp().flash_alpha.set(alpha);
        self.queue_draw();
    }

    pub fn set_flash_alpha(&self, alpha: f32) {
        self.imp().flash_alpha.set(alpha);
        self.queue_draw();
    }

    pub fn set_fidelity(&self, boxes: Vec<BoxF>) {
        self.imp().fidelity.replace(boxes);
        self.queue_draw();
    }

    /// View point → page point in L-units (`None` outside the page).
    pub fn page_point(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        let imp = self.imp();
        let g = imp.geom.get()?;
        let (sw, sh) = imp.size.get();
        let (ox, oy) = imp.origin.get();
        let (hx, vy) = imp.scroll();
        let fx = (x + hx - ox) / sw;
        let fy = (y + vy - oy) / sh;
        ((0.0..=1.0).contains(&fx) && (0.0..=1.0).contains(&fy)).then_some((fx * g.width, fy * g.height))
    }

    /// Whether the view point lies in the (padded) box `b` of the page.
    pub fn hits(&self, b: &BoxF, x: f32, y: f32, fidelity: bool) -> bool {
        let r = self.imp().to_view(b);
        let r = if fidelity {
            imp::fidelity_rect(&r)
        } else {
            graphene::Rect::new(r.x() - 2.0, r.y() - 2.0, r.width() + 4.0, r.height() + 4.0)
        };
        r.contains_point(&graphene::Point::new(x, y))
    }
}
