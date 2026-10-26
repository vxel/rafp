//! User preferences, kept as JSON in the user's config directory.

use std::path::PathBuf;

use gtk::glib;
use serde::{Deserialize, Serialize};

use crate::i18n::{gettext, gettext_f};

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum Zoom {
    FitWidth,
    FitHeight,
    /// 1.0 = real size (96 px per inch).
    Factor(f32),
}

impl Zoom {
    pub fn label(self) -> String {
        match self {
            Zoom::FitWidth => gettext("Fit Width"),
            Zoom::FitHeight => gettext("Fit Height"),
            // Translators: a zoom level; French writes "150 %" with a no-break space.
            Zoom::Factor(f) => gettext_f("{n}%", &[("n", &format!("{:.0}", f * 100.0))]),
        }
    }

    /// Action target: `fit-width`, `fit-height` or a percentage.
    pub fn key(self) -> String {
        match self {
            Zoom::FitWidth => "fit-width".into(),
            Zoom::FitHeight => "fit-height".into(),
            Zoom::Factor(f) => format!("{:.0}", f * 100.0),
        }
    }

    pub fn from_key(key: &str) -> Option<Zoom> {
        match key {
            "fit-width" => Some(Zoom::FitWidth),
            "fit-height" => Some(Zoom::FitHeight),
            pct => pct.trim_end_matches('%').parse::<f32>().ok().filter(|p| *p > 0.0).map(|p| Zoom::Factor(p / 100.0)),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub codepage: String,
    pub embed_small_fonts: bool,
    /// Show the inspector pane beside the render.
    pub show_inspector: bool,
    pub zoom: Zoom,
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
    /// Width of the inspector pane.
    pub inspector_width: Option<i32>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            codepage: "cp500".into(),
            embed_small_fonts: false,
            show_inspector: true,
            zoom: Zoom::FitWidth,
            width: 1440,
            height: 920,
            maximized: false,
            inspector_width: None,
        }
    }
}

fn path() -> PathBuf {
    glib::user_config_dir().join("rafp").join("settings.json")
}

impl Settings {
    pub fn load() -> Settings {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let path = path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_vec_pretty(self) {
            Ok(json) => {
                // Write a temporary file and rename it, so a crash never
                // leaves a truncated settings file.
                let tmp = path.with_extension("json.tmp");
                if let Err(e) = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, &path)) {
                    log::warn!("cannot save {}: {e}", path.display());
                }
            }
            Err(e) => log::warn!("cannot serialize settings: {e}"),
        }
    }
}
