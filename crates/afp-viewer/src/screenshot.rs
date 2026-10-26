//! Automated screenshots for visual checks. With `RAFP_SCREENSHOT=out.png`
//! the app captures its window once the document is loaded and rendered,
//! saves it as PNG and quits, without touching the saved preferences.
//!
//! Presets for the captured state (read only in this mode):
//! - `RAFP_SCREENSHOT_WAIT`: extra wait once ready, in ms (default 500)
//! - `RAFP_VIEW`: `split` (inspector shown) or `render` (hidden)
//! - `RAFP_ZOOM`: `fit-width`, `fit-height` or a percentage
//! - `RAFP_PAGE`: page to show (1-based)
//! - `RAFP_FIND`: Find query

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk::{glib, prelude::*};

use crate::app::App;
use crate::settings::Zoom;

pub struct Plan {
    path: PathBuf,
    wait: u64,
}

impl Plan {
    pub fn from_env() -> Option<Plan> {
        let path = std::env::var_os("RAFP_SCREENSHOT")?.into();
        let wait = std::env::var("RAFP_SCREENSHOT_WAIT").ok().and_then(|v| v.parse().ok()).unwrap_or(500);
        Some(Plan { path, wait })
    }

    pub fn run(self, app: Rc<App>) {
        if let Ok(view) = std::env::var("RAFP_VIEW") {
            let show = view != "render";
            if let Some(a) = app.window.lookup_action("show-inspector") {
                a.change_state(&show.to_variant());
            }
        }
        if let Some(z) = std::env::var("RAFP_ZOOM").ok().as_deref().and_then(Zoom::from_key)
            && let Some(a) = app.window.lookup_action("zoom")
        {
            a.change_state(&z.key().to_variant());
        }
        if let Ok(query) = std::env::var("RAFP_FIND") {
            app.set_find(&query);
        }
        let mut page = std::env::var("RAFP_PAGE").ok().and_then(|p| p.parse::<usize>().ok());
        glib::timeout_add_local(Duration::from_millis(100), move || {
            if app.has_doc()
                && let Some(p) = page.take()
            {
                app.show_page(p.saturating_sub(1));
            }
            if !app.ready() {
                return glib::ControlFlow::Continue;
            }
            let path = self.path.clone();
            let app = app.clone();
            glib::timeout_add_local_once(Duration::from_millis(self.wait), move || {
                // The window may not have been drawn yet: try again shortly.
                let mut tries = 0;
                glib::timeout_add_local(Duration::from_millis(100), move || {
                    tries += 1;
                    if !capture(&app.window, &path) && tries < 50 {
                        return glib::ControlFlow::Continue;
                    }
                    if let Some(application) = app.window.application() {
                        application.quit();
                    }
                    glib::ControlFlow::Break
                });
            });
            glib::ControlFlow::Break
        });
    }
}

/// Save the window as PNG; `false` when it has nothing drawn yet.
fn capture(window: &adw::ApplicationWindow, path: &std::path::Path) -> bool {
    let (w, h) = (window.width(), window.height());
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, w as f64, h as f64);
    let (Some(node), Some(renderer)) = (snapshot.to_node(), window.renderer()) else { return false };
    let texture = renderer.render_texture(node, None);
    if let Err(e) = texture.save_to_png(path) {
        eprintln!("cannot write {}: {e}", path.display());
    }
    true
}
