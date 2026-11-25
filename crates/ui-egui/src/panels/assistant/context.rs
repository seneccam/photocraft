//! What the Assistant sends with each message: a short text description of the active document
//! (size, layers, selection, the person's pins) and, when asked, a preview of the canvas plus a
//! 100% crop around each pin, so the model sees what the person sees and where they point.

use base64::Engine as _;
use serde_json::{Value, json};

use super::Pin;
use crate::PhotocraftApp;

/// The longest side of the canvas preview.
pub(crate) const PREVIEW_SIDE: u32 = 1024;
/// The side of the 100% crop around a pin.
pub(crate) const PIN_CROP: u32 = 384;
/// Layers listed in full; the rest are counted.
const MAX_LAYERS: usize = 80;

/// The message content (Messages API blocks) for `text`, with the context of the active document.
pub(crate) fn message(app: &PhotocraftApp, text: &str, pins: &[Pin], send_canvas: bool) -> Value {
    let mut blocks = Vec::new();
    let mut lines = vec!["[PhotoCraft context]".to_owned()];
    let docs = app.session.documents();
    match app.session.active() {
        None => lines.push("No document is open.".into()),
        Some(st) => {
            let doc = &st.doc;
            lines.push(format!(
                "Active document: \"{}\", {}×{} px, {:?} {}-bit.",
                doc.name,
                doc.size.width,
                doc.size.height,
                doc.mode,
                doc.depth.bits()
            ));
            if docs.len() > 1 {
                let names: Vec<String> = docs.iter().map(|d| format!("\"{}\"", d.doc.name)).collect();
                lines.push(format!("Open documents: {}.", names.join(", ")));
            }
            let inspect = photocraft_engine::inspect::document(st);
            let selected: Vec<String> =
                inspect["selectedLayers"].as_array().into_iter().flatten().filter_map(|id| find_layer(&inspect["layers"], id)).map(layer_name).collect();
            if !selected.is_empty() {
                lines.push(format!("Selected layer(s): {}.", selected.join(", ")));
            }
            let mut rows = Vec::new();
            layer_rows(&inspect["layers"], 0, &mut rows);
            let total = rows.len();
            rows.truncate(MAX_LAYERS);
            lines.push("Layers, top to bottom:".into());
            lines.extend(rows);
            if total > MAX_LAYERS {
                lines.push(format!("  … and {} more", total - MAX_LAYERS));
            }
            match inspect["selectionBounds"].as_array() {
                Some(b) if b.len() == 4 => lines.push(format!("Selection: bounds x={} y={} w={} h={}.", b[0], b[1], b[2], b[3])),
                _ => lines.push("Selection: none.".into()),
            }
            let pins: Vec<&Pin> = pins.iter().filter(|p| p.doc == doc.id).collect();
            for (i, pin) in pins.iter().enumerate() {
                lines.push(format!("Pin {} at document pixel ({:.0}, {:.0}).", i + 1, pin.x, pin.y));
            }
            if send_canvas {
                if let Some(png) = preview(doc) {
                    blocks.push(image(&png));
                    lines.push("Image 1 is the whole canvas, scaled down.".into());
                }
                for (i, pin) in pins.iter().enumerate() {
                    if let Some((png, rect)) = pin_crop(doc, pin) {
                        blocks.push(image(&png));
                        lines.push(format!(
                            "Image {} is pin {} at 100%: document pixels x {}..{}, y {}..{}.",
                            blocks.len(),
                            i + 1,
                            rect.x0,
                            rect.x1,
                            rect.y0,
                            rect.y1
                        ));
                    }
                }
            }
        }
    }
    lines.push(String::new());
    lines.push(text.to_owned());
    blocks.push(json!({"type": "text", "text": lines.join("\n")}));
    Value::Array(blocks)
}

fn find_layer<'a>(layers: &'a Value, id: &Value) -> Option<&'a Value> {
    layers.as_array()?.iter().find_map(|l| if l.get("id") == Some(id) { Some(l) } else { find_layer(&l["children"], id) })
}

fn layer_name(l: &Value) -> String {
    format!("\"{}\" (id {})", l["name"].as_str().unwrap_or("?"), l["id"])
}

fn layer_rows(layers: &Value, depth: usize, out: &mut Vec<String>) {
    for l in layers.as_array().into_iter().flatten() {
        let hidden = if l.get("visible").and_then(Value::as_bool) == Some(false) { ", hidden" } else { "" };
        out.push(format!("{}- {} {}{hidden}", "  ".repeat(depth + 1), layer_name(l), l["kind"].as_str().unwrap_or("layer")));
        layer_rows(&l["children"], depth + 1, out);
    }
}

fn image(png: &[u8]) -> Value {
    json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": base64::engine::general_purpose::STANDARD.encode(png)}})
}

fn png(width: u32, height: u32, pixels: Vec<u8>) -> Option<Vec<u8>> {
    let image = photocraft_codecs::Image::from_u8(width, height, photocraft_codecs::ChannelLayout::Rgba, pixels).ok()?;
    photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).ok()
}

/// The whole canvas at most [`PREVIEW_SIDE`] pixels on its longer side.
fn preview(doc: &photocraft_doc::Document) -> Option<Vec<u8>> {
    let img = photocraft_compose::thumbnail(doc, PREVIEW_SIDE);
    png(img.width, img.height, img.pixels)
}

/// The document's pixels around `pin` at 100%, clamped inside the document.
fn pin_crop(doc: &photocraft_doc::Document, pin: &Pin) -> Option<(Vec<u8>, photocraft_geom::Rect)> {
    let (w, h) = (doc.size.width, doc.size.height);
    let side = PIN_CROP.min(w).min(h);
    let half = f64::from(side) / 2.0;
    let clamp = |c: f64, len: u32| (c - half).round().clamp(0.0, f64::from(len - side)) as i32;
    let rect = photocraft_geom::Rect::from_xywh(clamp(pin.x, w), clamp(pin.y, h), side, side);
    let img = photocraft_compose::render(doc, rect).to_rgba8();
    Some((png(img.width, img.height, img.pixels)?, rect))
}
