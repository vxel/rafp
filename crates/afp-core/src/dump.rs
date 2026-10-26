//! A JSON view of an inspection: inspector rows, triplets, summaries,
//! diagnostics and the full page model. `afp-dump --json` prints it, and the
//! regression snapshots hash it.

use serde_json::{Value, json};

use crate::inspect::Inspection;
use crate::model::{FidelityNote, Page, ResourceKind};

fn notes(n: &[FidelityNote]) -> Value {
    json!(n.iter().map(|n| json!({"cat": n.cat.as_str(), "msg": n.msg})).collect::<Vec<_>>())
}

/// One page of the model.
pub fn page_json(p: &Page) -> Value {
    json!({
        "width": p.width, "height": p.height, "upi": p.units_per_inch,
        "truncated": p.truncated, "plain_text": p.plain_text(),
        "texts": p.texts.iter().map(|t| json!({
            "x": t.x, "y": t.y, "text": t.text, "color": t.color, "font_id": t.font_id,
            "font_size": t.font_size, "font_family": t.font_family, "font_weight": t.font_weight.as_str(),
            "orientation": t.orientation, "src": t.src, "fit": t.fit, "space_width": t.space_width,
            "notes": notes(&t.notes),
        })).collect::<Vec<_>>(),
        "text_layer": p.text_layer.iter().map(|t| json!({"x": t.x, "y": t.y, "text": t.text})).collect::<Vec<_>>(),
        "rules": p.rules.iter().map(|r| json!({
            "x": r.x, "y": r.y, "length": r.length, "thickness": r.thickness,
            "axis": r.axis.as_char().to_string(), "color": r.color, "src": r.src,
        })).collect::<Vec<_>>(),
        "images": p.images.iter().map(|i| json!({
            "x": i.x, "y": i.y, "width": i.width, "height": i.height, "mime": i.mime,
            "crisp": i.crisp, "recolor": i.recolor,
            "rotate": i.rotate.map(|(a, x, y)| vec![a, x, y]),
            "bands": i.bands.as_ref().map_or(0, |b| b.len()),
            "jpeg_len": if i.mime == "image/jpeg" { Some(i.data.len()) } else { None },
            "notes": notes(&i.notes),
        })).collect::<Vec<_>>(),
        "graphics": p.graphics.iter().map(|g| json!({
            "x": g.x, "y": g.y, "width": g.width, "height": g.height,
            "gps_w": g.graphic.gps_w, "gps_h": g.graphic.gps_h,
            "rotate": g.rotate.map(|(a, x, y)| vec![a, x, y]), "svg": g.graphic.svg,
        })).collect::<Vec<_>>(),
    })
}

/// The whole inspection.
pub fn inspection_json(ins: &Inspection) -> Value {
    json!({
        "error": null,
        "rows": ins.rows.iter().map(|r| json!({
            "offset": r.offset, "sf_id": r.sf_id_hex(), "name": r.name, "token": r.token,
            "size": r.size, "depth": r.depth, "preview": r.preview, "page": r.page,
            "search_text": r.search_text, "data": r.data, "triplets": r.triplets,
        })).collect::<Vec<_>>(),
        "summary": ins.summary.iter().map(|(n, c)| json!([n, c])).collect::<Vec<_>>(),
        "mcf_note": ins.mcf_note,
        "resource_kind": ins.resource_kind.map_or("", ResourceKind::as_str),
        "missing_resources": ins.missing_resources,
        "pages": ins.pages.iter().map(page_json).collect::<Vec<_>>(),
    })
}
