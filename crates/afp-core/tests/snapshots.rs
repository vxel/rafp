//! Regression snapshots: for each public test file, a fingerprint of the full
//! JSON view (inspector rows, triplets, summaries, diagnostics, page model)
//! and of every page's SVG, compared with `tests/snapshots/corpus.txt`.
//!
//! These outputs were checked against readAFP at the end of the port; the
//! snapshots catch any later change. After an intended change, rewrite them:
//!
//! ```text
//! RAFP_UPDATE_SNAPSHOTS=1 cargo test -p afp-core --test snapshots
//! ```
//!
//! Files come from `RAFP_TESTDATA` (default `local/testdata`), or the bundled
//! samples with a `samples:` prefix. Each file is named explicitly (only public
//! files); missing ones are skipped and keep their recorded snapshot.

use std::collections::BTreeMap;
use std::path::PathBuf;

use afp_core::codepages::Codec;
use afp_core::dump::inspection_json;
use afp_core::inspect::inspect;
use afp_core::render_svg::page_to_svg;

const FILES: &[&str] = &[
    "alpheus-corpus/minimal.afp",
    "alpheus-corpus/large_ibm273.afp",
    "alpheus-corpus/perf_ptx.afp",
    "alpheus-corpus/external/afplib_ende.afp",
    "alpheus-corpus/external/afplib_start.afp",
    "alpheus-corpus/external/openafp_goca.afp",
    "alpheus-corpus/external/openafp_ioca.afp",
    "alpheus-corpus/external/openafp_sample.afp",
    "alpheus-corpus/external/random_test.afp",
    "alpheus-corpus/external/renderer_fontoutl.afp",
    "alpheus-corpus/external/renderer_minimal.afp",
    "alpheus-corpus/external/renderer_vseoutln.afp",
    "alpheus-corpus/modca-reference-10/Chapter_1.afp",
    "fop-pairs/fonts.afp",
    "fop-pairs/images.afp",
    "fop-pairs/list.afp",
    "fop-pairs/normal.afp",
    "fop-pairs/readme.afp",
    "fop-pairs/simple.afp",
    "fop-pairs/table.afp",
    "fop-pairs/textdeko.afp",
    "github-samples/afplib/asciiAndEbcdicComment.afp",
    "github-samples/afplib/asciiComment.afp",
    "github-samples/afplib/bim.afp",
    "github-samples/afplib/C0X00006.afp",
    "github-samples/afplib/cs.afp",
    "github-samples/afplib/fnirg10.afp",
    "github-samples/afplib/hello.afp",
    "github-samples/afplib/IPDSpan.afp",
    "github-samples/afplib/repeatingGroupVariableLength.afp",
    "github-samples/afplib/unknownSF.afp",
    "github-samples/fop/expected_named_resource.afp",
    "github-samples/fop/expected_resource.afp",
    "github-samples/fop/resource_any_name.afp",
    "github-samples/fop/resource_name_match.afp",
    "github-samples/fop/resource_name_mismatch.afp",
    "github-samples/fop/resource_no_end_name.afp",
    "headline_sample/headline.afp",
    "sample1_health/01_Health_Coverage.afp",
    "bcoca_sample.afp",
    "cff_document_sample.afp",
    "coded_font_sample.afp",
    "foca_cff_sample.afp",
    "foca_sample.afp",
    "goca_arc_sample.afp",
    "goca_sample.afp",
    "overlay_sample.afp",
    "pagelimit_520.afp",
    "samples:bcoca_qr.afp",
    "samples:foca_font.afp",
    "samples:goca_demo.afp",
    "samples:health_coverage.afp",
    "samples:ioca_image.afp",
    "samples:overlay_demo.afp",
];

/// FNV-1a, 64 bits: stable across platforms and Rust versions.
fn fnv(bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn snapshot_path() -> PathBuf {
    manifest_dir().join("tests/snapshots/corpus.txt")
}

fn read_file(name: &str) -> Option<Vec<u8>> {
    let path = match name.strip_prefix("samples:") {
        Some(sample) => manifest_dir().join("tests/samples").join(sample),
        None => std::env::var_os("RAFP_TESTDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| manifest_dir().join("../../local/testdata"))
            .join(name),
    };
    std::fs::read(path).ok()
}

/// One snapshot line (without the file name), for both text settings.
fn fingerprint(data: &[u8]) -> String {
    let mut parts = Vec::new();
    for embed_small_fonts in [false, true] {
        let tag = if embed_small_fonts { "embed" } else { "plain" };
        match inspect(data, Codec::cp500(), embed_small_fonts) {
            Ok(ins) => {
                let model = fnv(inspection_json(&ins).to_string().into_bytes());
                let svg = ins.pages.iter().fold(fnv([]), |h, p| h ^ fnv(page_to_svg(p).into_bytes()).rotate_left(7));
                if !embed_small_fonts {
                    parts.push(format!("pages={} fields={}", ins.pages.len(), ins.rows.len()));
                }
                parts.push(format!("{tag}:model={model:016x} {tag}:svg={svg:016x}"));
            }
            Err(e) => parts.push(format!("{tag}:error={:016x}", fnv(e.to_string().into_bytes()))),
        }
    }
    parts.join(" ")
}

fn load_snapshots() -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(snapshot_path()).unwrap_or_default();
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| l.split_once(' ').map(|(name, rest)| (name.to_string(), rest.to_string())))
        .collect()
}

#[test]
fn corpus_matches_snapshots() {
    let update = std::env::var_os("RAFP_UPDATE_SNAPSHOTS").is_some();
    let mut snapshots = load_snapshots();
    let mut checked = 0;
    let mut failures = Vec::new();
    for name in FILES {
        let Some(data) = read_file(name) else {
            eprintln!("skipping: {name} not available");
            continue;
        };
        checked += 1;
        let actual = fingerprint(&data);
        match snapshots.get(*name) {
            Some(expected) if *expected == actual => {}
            _ if update => {
                snapshots.insert(name.to_string(), actual);
            }
            Some(expected) => failures.push(format!("{name}\n  expected {expected}\n  actual   {actual}")),
            None => failures.push(format!("{name}: no snapshot recorded")),
        }
    }
    if update {
        let mut out = String::from(
            "# Regression snapshots of the public test files (see tests/snapshots.rs).\n\
             # Regenerate after an intended change: RAFP_UPDATE_SNAPSHOTS=1 cargo test -p afp-core --test snapshots\n",
        );
        for (name, line) in &snapshots {
            out.push_str(&format!("{name} {line}\n"));
        }
        std::fs::create_dir_all(snapshot_path().parent().unwrap()).unwrap();
        std::fs::write(snapshot_path(), out).unwrap();
        eprintln!("snapshots written for {checked} file(s)");
        return;
    }
    assert!(
        failures.is_empty(),
        "{} of {checked} file(s) changed (set RAFP_UPDATE_SNAPSHOTS=1 if intended):\n{}",
        failures.len(),
        failures.join("\n")
    );
}
