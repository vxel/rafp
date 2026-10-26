//! Compile the translations (`po/*.po`, listed in `po/LINGUAS`) into gettext
//! catalogs for development runs; `src/i18n.rs` finds them through
//! `RAFP_BUILD_LOCALEDIR`. Without `msgfmt` the UI stays in English.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let po = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../po");
    println!("cargo:rerun-if-changed={}", po.display());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("locale");
    let linguas = std::fs::read_to_string(po.join("LINGUAS")).unwrap_or_default();
    for lang in linguas.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let dir = out.join(lang).join("LC_MESSAGES");
        std::fs::create_dir_all(&dir).unwrap();
        let status = Command::new("msgfmt")
            .arg("--check")
            .arg("-o")
            .arg(dir.join("rafp.mo"))
            .arg(po.join(format!("{lang}.po")))
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(_) => panic!("msgfmt rejected po/{lang}.po"),
            Err(e) => println!("cargo:warning=msgfmt not available ({e}): the UI will be in English only"),
        }
    }
    println!("cargo:rustc-env=RAFP_BUILD_LOCALEDIR={}", out.display());
}
