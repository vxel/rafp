//! Command-line dump of an AFP file: structured fields, page text, SVG or
//! PDF export, or the JSON view that the regression snapshots fingerprint.

use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use afp_core::codepages::Codec;
use afp_core::dump::inspection_json;
use afp_core::inspect::{Inspection, inspect};
use afp_core::model::Page;
use afp_core::render_svg::page_to_svg;
use serde_json::json;

const USAGE: &str =
    "usage: afp-dump [--codepage CP] [--embed-small-fonts] [--json | --text | --svg DIR | --pdf OUT] FILE

  (default)            list structured fields with their decoded summary
  --json               full inspector + page model as JSON
  --text               extracted text, pages separated by '----- page break -----'
  --svg DIR            write one SVG per page into DIR
  --pdf OUT            write all pages as one PDF
  --codepage CP        code page for text whose font declares none (default cp500)
  --embed-small-fonts  draw small text in the file's embedded raster fonts";

enum Mode {
    Fields,
    Json,
    Text,
    Svg(PathBuf),
    Pdf(PathBuf),
}

struct Args {
    mode: Mode,
    codepage: Codec,
    embed_small: bool,
    file: PathBuf,
}

enum Parsed {
    Run(Args),
    Help,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Parsed, String> {
    let mut mode = Mode::Fields;
    let mut codepage = Codec::cp500();
    let mut embed_small = false;
    let mut file: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        let mut value = |what: &str| args.next().ok_or_else(|| format!("{a} needs {what}"));
        match a.as_str() {
            "--json" => mode = Mode::Json,
            "--text" => mode = Mode::Text,
            "--svg" => mode = Mode::Svg(value("a directory")?.into()),
            "--pdf" => mode = Mode::Pdf(value("an output file")?.into()),
            "--codepage" => {
                let name = value("a code page name")?;
                codepage = Codec::lookup(&name).ok_or_else(|| format!("unknown code page: {name}"))?;
            }
            "--embed-small-fonts" => embed_small = true,
            "-h" | "--help" => return Ok(Parsed::Help),
            flag if flag.starts_with('-') => return Err(format!("unknown option: {flag}")),
            _ if file.is_none() => file = Some(a.into()),
            _ => return Err("only one file can be dumped at a time".into()),
        }
    }
    let file = file.ok_or("no file given")?;
    Ok(Parsed::Run(Args { mode, codepage, embed_small, file }))
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(Parsed::Run(args)) => args,
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("afp-dump: {e}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    let data = match std::fs::read(&args.file) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{}: {e}", args.file.display());
            return ExitCode::FAILURE;
        }
    };
    let ins = match inspect(&data, args.codepage, args.embed_small) {
        Ok(i) => i,
        Err(e) => {
            if matches!(args.mode, Mode::Json) {
                println!("{}", json!({"error": format!("Not a valid AFP file: {e}")}));
                return ExitCode::SUCCESS;
            }
            eprintln!("Not a valid AFP file: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut out = BufWriter::new(io::stdout().lock());
    match dump(&args, &ins, &mut out).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        // The reader went away (e.g. `afp-dump file.afp | head`): not an error.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("afp-dump: {e}");
            ExitCode::FAILURE
        }
    }
}

fn dump(args: &Args, ins: &Inspection, out: &mut impl Write) -> io::Result<()> {
    match &args.mode {
        Mode::Json => writeln!(out, "{}", inspection_json(ins))?,
        Mode::Text => {
            let texts: Vec<String> = ins.pages.iter().map(Page::plain_text).collect();
            writeln!(out, "{}", texts.join("\n\n----- page break -----\n\n"))?;
        }
        Mode::Svg(dir) => {
            std::fs::create_dir_all(dir).map_err(|e| with_path(e, dir))?;
            for (i, page) in ins.pages.iter().enumerate() {
                let path = dir.join(format!("page-{:04}.svg", i + 1));
                std::fs::write(&path, page_to_svg(page)).map_err(|e| with_path(e, &path))?;
            }
            eprintln!("{} page(s) written to {}", ins.pages.len(), dir.display());
        }
        Mode::Pdf(path) => {
            let rasterizer = afp_core::raster::Rasterizer::new();
            let title = args.file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let pdf = afp_core::pdf::pages_to_pdf(&ins.pages, &title, &rasterizer).map_err(io::Error::other)?;
            std::fs::write(path, pdf).map_err(|e| with_path(e, path))?;
            eprintln!("{} page(s) written to {}", ins.pages.len(), path.display());
        }
        Mode::Fields => {
            for r in &ins.rows {
                let summary = if r.data.is_empty() { &r.preview } else { &r.data };
                writeln!(
                    out,
                    "{:>10}  {}  {}{:<44} {:<8} {}",
                    r.offset,
                    r.sf_id_hex(),
                    "  ".repeat(r.depth),
                    r.name,
                    r.token,
                    summary
                )?;
            }
            eprintln!("{} fields, {} page(s)", ins.rows.len(), ins.pages.len());
        }
    }
    Ok(())
}

/// An I/O error that names the file it concerns.
fn with_path(e: io::Error, path: &std::path::Path) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}
