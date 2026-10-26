//! rafp — desktop viewer and inspector for AFP (MO:DCA) files, built with
//! GTK 4 and libadwaita.

mod app;
mod canvas;
mod i18n;
mod inspector;
mod page_view;
mod screenshot;
mod settings;
mod worker;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

pub const APP_ID: &str = "org.delaunois.Rafp";

/// The app icon is named after `APP_ID`. Installed, it is in the hicolor
/// theme; in development runs it is taken from the source tree.
fn load_icons() {
    let dev_icons = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/icons");
    if let Some(display) = gdk::Display::default()
        && std::path::Path::new(dev_icons).is_dir()
    {
        gtk::IconTheme::for_display(&display).add_search_path(dev_icons);
    }
    gtk::Window::set_default_icon_name(APP_ID);
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

fn main() -> glib::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    i18n::init();
    glib::set_application_name("rafp");
    let app = adw::Application::builder()
        .application_id(APP_ID)
        // One window per launch, like most document viewers started from a terminal.
        .flags(gio::ApplicationFlags::HANDLES_OPEN | gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.connect_startup(|_| {
        load_icons();
        load_css();
    });
    app.connect_activate(|app| start(app, None));
    app.connect_open(|app, files, _| {
        if files.len() > 1 {
            log::warn!("opening only the first of {} files", files.len());
        }
        start(app, files.first().cloned());
    });
    app.run()
}

fn start(app: &adw::Application, file: Option<gio::File>) {
    let window = app::App::new(app);
    if let Some(file) = file {
        window.open_file(file);
    }
    window.window.present();
    if let Some(plan) = screenshot::Plan::from_env() {
        plan.run(window);
    }
}
