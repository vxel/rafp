use std::time::Instant;
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let data = std::fs::read(path).unwrap();
    let t = Instant::now();
    let fields = afp_core::parser::parse_fields(&data).unwrap();
    eprintln!("parse {:?}", t.elapsed());
    let t = Instant::now();
    let _rows = afp_core::inspect::field_rows(&fields);
    eprintln!("rows {:?}", t.elapsed());
    let t = Instant::now();
    let fonts = afp_core::foca::parse_fonts(&fields);
    eprintln!("fonts {:?} ({} fonts)", t.elapsed(), fonts.len());
    let t = Instant::now();
    let pages = afp_core::ptoca::extract_pages(&fields, afp_core::codepages::Codec::cp500(), false);
    eprintln!("pages {:?} ({})", t.elapsed(), pages.len());
}
