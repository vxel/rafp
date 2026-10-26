//! Translations with gettext (domain `rafp`). The catalogs come from `po/`:
//! `build.rs` compiles them for development runs, and the installer puts them
//! in `<prefix>/share/locale` next to `<prefix>/bin/rafp`.
//!
//! Strings use named `{placeholders}` so translators can reorder them.

use std::path::PathBuf;

use gettextrs::{LocaleCategory, bind_textdomain_codeset, bindtextdomain, setlocale, textdomain};
pub use gettextrs::{gettext, ngettext, pgettext};

const GETTEXT_PACKAGE: &str = "rafp";

/// Select the user's locale and the `rafp` catalogs. Call early in `main`,
/// before any other thread starts.
pub fn init() {
    // SAFETY: called at the start of `main`, while the process is still
    // single-threaded, so nothing reads the locale concurrently.
    unsafe {
        setlocale(LocaleCategory::LcAll, "");
    }
    if let Some(dir) = locale_dir()
        && let Err(e) = bindtextdomain(GETTEXT_PACKAGE, dir)
    {
        log::warn!("cannot bind the translations: {e}");
    }
    let _ = bind_textdomain_codeset(GETTEXT_PACKAGE, "UTF-8");
    let _ = textdomain(GETTEXT_PACKAGE);
}

/// `RAFP_LOCALEDIR`, else `<prefix>/share/locale` when installed, else the
/// catalogs compiled by `build.rs`.
fn locale_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("RAFP_LOCALEDIR") {
        return Some(dir.into());
    }
    let exe = std::env::current_exe().ok()?;
    let installed = exe.parent()?.parent()?.join("share/locale");
    let has_catalog = |dir: &PathBuf| {
        std::fs::read_dir(dir).is_ok_and(|langs| {
            langs.flatten().any(|l| l.path().join("LC_MESSAGES").join(format!("{GETTEXT_PACKAGE}.mo")).is_file())
        })
    };
    if has_catalog(&installed) {
        return Some(installed);
    }
    option_env!("RAFP_BUILD_LOCALEDIR").map(PathBuf::from)
}

/// Replace each `{name}` of `text` by its value, in one pass, so values
/// that themselves contain braces are left alone.
fn fill(text: String, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let value = after.find('}').and_then(|close| {
            let name = &after[..close];
            args.iter().find(|(n, _)| *n == name).map(|(_, v)| (*v, close))
        });
        match value {
            Some((v, close)) => {
                out.push_str(v);
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `gettext` with `{name}` placeholders.
pub fn gettext_f(msgid: &str, args: &[(&str, &str)]) -> String {
    fill(gettext(msgid), args)
}

/// `ngettext` with `{name}` placeholders.
pub fn ngettext_f(msgid: &str, plural: &str, n: usize, args: &[(&str, &str)]) -> String {
    fill(ngettext(msgid, plural, n as u32), args)
}

/// An integer with the locale's digit grouping.
pub fn number(n: usize) -> String {
    // Translators: digit group separator in large numbers (French: a narrow no-break space).
    let sep = pgettext("thousands separator", ",");
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push_str(&sep);
        }
        out.push(c);
    }
    out
}

/// Text as GTK accepts it: C strings cannot hold NUL characters, which
/// decoded AFP text may contain; show them as the "␀" symbol.
pub fn safe(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains('\0') { s.replace('\0', "\u{2400}").into() } else { s.into() }
}

/// A decimal with one digit after the locale's decimal separator.
pub fn decimal1(v: f32) -> String {
    // Translators: decimal separator (French: a comma).
    format!("{v:.1}").replace('.', &pgettext("decimal separator", "."))
}

#[cfg(test)]
mod tests {
    use super::fill;

    #[test]
    fn fill_replaces_each_placeholder_once() {
        let args = [("file", "{error}.afp"), ("error", "denied")];
        assert_eq!(fill("Cannot read {file}: {error}".into(), &args), "Cannot read {error}.afp: denied");
        assert_eq!(fill("{unknown} {".into(), &args), "{unknown} {");
    }
}
