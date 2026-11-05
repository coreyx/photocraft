//! Tests for transient previews: shown above the right layer, never in history or saved files,
//! left out of composite reads unless asked for, dropped when no longer relevant, and the hatch.

use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Document, Layer, Size};
use photocraft_geom::Rect;
use photocraft_process_plugin_protocol::message::ErrorCode;
use serde_json::{Value, json};

use crate::Session;

const P: &str = "org.example.test";
const W: u32 = 40;
const H: u32 = 30;

fn session() -> (Session, String) {
    let mut s = Session::new();
    let mut doc = Document::new("Preview", Size::new(W, H), ColorMode::Rgb, SampleType::U8);
    let fmt = doc.pixel_format();
    let mut base = Layer::raster("Base", fmt);
    if let Some(surface) = base.surface_mut() {
        surface.fill_rect(Rect::from_xywh(0, 0, W, H), &[1.0, 1.0, 1.0, 1.0]);
    }
    doc.layers = vec![base, Layer::raster("Top", fmt)];
    let index = s.add_document(doc, None);
    let id = s.documents()[index].doc.id.0.to_string();
    (s, id)
}

fn request(s: &mut Session, method: &str, params: Value, payload: &[u8]) -> Result<(Value, Vec<u8>), ErrorCode> {
    s.plugin_request(P, method, &params, payload).map_err(|e| e.code)
}

fn uid(s: &Session, name: &str) -> String {
    let doc = &s.documents()[0].doc;
    doc.walk().into_iter().find(|(_, _, l)| l.name == name).map(|(_, _, l)| photocraft_doc::uid_string(l.uid)).unwrap()
}

/// A red 10x10 preview at (5, 5).
fn show(s: &mut Session, doc: &str, extra: Value) -> Result<(Value, Vec<u8>), ErrorCode> {
    let mut params = json!({"doc": doc, "label": "[Preview] a fox", "rect": {"x": 5, "y": 5, "width": 10, "height": 10}, "format": "rgba8"});
    if let (Some(map), Some(more)) = (params.as_object_mut(), extra.as_object()) {
        map.extend(more.clone());
    }
    let red: Vec<u8> = (0..100).flat_map(|_| [255, 0, 0, 255]).collect();
    request(s, "preview.show", params, &red)
}

fn composite(s: &mut Session, doc: &str, include_preview: bool) -> Vec<u8> {
    let params = json!({"doc": doc, "source": {"kind": "composite"}, "rect": {"x": 0, "y": 0, "width": W, "height": H}, "format": "rgba8", "includePreview": include_preview});
    request(s, "pixels.read", params, &[]).unwrap().1
}

fn pixel(image: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [image[i], image[i + 1], image[i + 2], image[i + 3]]
}

#[test]
fn a_preview_changes_neither_the_document_nor_its_history() {
    let (mut s, doc) = session();
    let before = s.documents()[0].doc.clone();
    let revision = s.documents()[0].revision;
    let label = s.documents()[0].history.undo_label().map(str::to_string);
    show(&mut s, &doc, json!({})).unwrap();
    show(&mut s, &doc, json!({})).unwrap();
    request(&mut s, "preview.hide", json!({"doc": doc}), &[]).unwrap();
    let st = &s.documents()[0];
    assert!(std::sync::Arc::ptr_eq(&st.doc, &before), "the document is untouched");
    assert_eq!(st.revision, revision);
    assert_eq!(st.history.undo_label().map(str::to_string), label, "no undo steps");
    assert!(!st.is_dirty());
    // While shown, it is in no saved file either.
    show(&mut s, &doc, json!({})).unwrap();
    let opts = photocraft_io::ExportOptions::default();
    for ext in ["pcraft", "psd"] {
        let name = format!("saved.{ext}");
        let bytes = photocraft_io::export(&s.documents()[0].doc, &name, &opts).unwrap().bytes;
        let back = photocraft_io::import(&name, &bytes).unwrap().document;
        assert!(back.walk().into_iter().all(|(_, _, l)| !l.name.contains("Preview")), "{ext}");
    }
}

#[test]
fn the_preview_is_shown_above_its_layer_and_locked() {
    let (mut s, doc) = session();
    let base = uid(&s, "Base");
    show(&mut s, &doc, json!({"above": base})).unwrap();
    let shown = s.plugin_preview_document(&s.documents()[0].doc).unwrap();
    let names: Vec<&str> = shown.layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Base", "[Preview] a fox", "Top"]);
    assert!(shown.layers[1].locks.all);
    let row = s.plugin_preview_row(&s.documents()[0].doc).unwrap();
    assert_eq!(row.label, "[Preview] a fox");
    assert_eq!(row.above, Some(s.documents()[0].doc.layers[0].id));
    // Without `above`, on top.
    show(&mut s, &doc, json!({})).unwrap();
    let shown = s.plugin_preview_document(&s.documents()[0].doc).unwrap();
    assert_eq!(shown.layers.last().map(|l| l.name.as_str()), Some("[Preview] a fox"));
    // The plug-in sees no such layer, and the active layer stays what it was.
    let tree = request(&mut s, "layer.tree", json!({"doc": doc}), &[]).unwrap().0;
    assert_eq!(tree.as_array().map(Vec::len), Some(2));
    assert_eq!(s.documents()[0].active_layer, Some(s.documents()[0].doc.layers[1].id));
}

#[test]
fn composite_reads_leave_the_preview_out_unless_asked() {
    let (mut s, doc) = session();
    let plain = composite(&mut s, &doc, false);
    show(&mut s, &doc, json!({})).unwrap();
    assert_eq!(composite(&mut s, &doc, false), plain, "default reads ignore it");
    let with = composite(&mut s, &doc, true);
    assert_eq!(pixel(&with, 8, 8), [255, 0, 0, 255]);
    assert_eq!(pixel(&with, 1, 1), pixel(&plain, 1, 1));
}

#[test]
fn the_hatch_darkens_outside_its_rectangle_only() {
    let (mut s, doc) = session();
    show(&mut s, &doc, json!({"hatchOutside": {"x": 0, "y": 0, "width": 20, "height": 30}})).unwrap();
    let with = composite(&mut s, &doc, true);
    // Inside the kept rectangle: the white base, untouched.
    assert!((0..20).all(|x| (0..H).all(|y| (5..15).contains(&x) && (5..15).contains(&y) || pixel(&with, x, y) == [255, 255, 255, 255])));
    // Outside: diagonal lines every 8 pixels, 2 wide, multiplied over white.
    let shade = (0.55f32 * 255.0 + 0.5) as u8;
    for y in 0..H {
        for x in 20..W {
            let expected = if (x + y) % 8 < 2 { [shade, shade, shade, 255] } else { [255, 255, 255, 255] };
            assert_eq!(pixel(&with, x, y), expected, "({x}, {y})");
        }
    }
    // A large canvas's hatch shares its untouched tiles.
    let mut big = Document::new("Big", Size::new(4096, 4096), ColorMode::Rgb, SampleType::U8);
    big.layers = vec![Layer::raster("Base", big.pixel_format())];
    let index = s.add_document(big, None);
    let id = s.documents()[index].doc.id.0.to_string();
    show(&mut s, &id, json!({"hatchOutside": {"x": 0, "y": 0, "width": 300, "height": 300}})).unwrap();
    let shown = s.plugin_preview_document(&s.documents()[index].doc).unwrap();
    let hatch = shown.layers.last().and_then(Layer::surface).unwrap();
    let distinct: std::collections::HashSet<usize> = hatch.tiles().map(|(_, t)| std::sync::Arc::as_ptr(t) as usize).collect();
    assert!(distinct.len() <= 5, "{} distinct tiles", distinct.len());
}

#[test]
fn previews_go_with_a_document_switch_or_a_disconnect() {
    let (mut s, doc) = session();
    show(&mut s, &doc, json!({})).unwrap();
    let generation = s.plugin_preview_generation();
    let _ = s.plugin_events();
    assert!(s.plugin_preview_row(&s.documents()[0].doc).is_some(), "still the active document");
    let mut other = Document::new("Other", Size::new(4, 4), ColorMode::Rgb, SampleType::U8);
    other.layers = vec![Layer::raster("L", other.pixel_format())];
    s.add_document(other, None);
    let _ = s.plugin_events();
    assert!(s.plugin_preview_row(&s.documents()[0].doc).is_none(), "dropped on the switch");
    assert_ne!(s.plugin_preview_generation(), generation, "the canvas redraws");
    s.set_active(0);
    show(&mut s, &doc, json!({})).unwrap();
    s.plugin_disconnected(P);
    assert!(s.plugin_preview_row(&s.documents()[0].doc).is_none(), "dropped with its plug-in");
}

#[test]
fn bad_previews_are_refused() {
    let (mut s, doc) = session();
    assert_eq!(show(&mut s, &doc, json!({"label": ""})).map(|_| ()), Err(ErrorCode::Invalid));
    assert_eq!(show(&mut s, &doc, json!({"above": "00000000-0000-0000-0000-000000000001"})).map(|_| ()), Err(ErrorCode::NotFound));
    assert_eq!(show(&mut s, &doc, json!({"format": "gray8"})).map(|_| ()), Err(ErrorCode::Invalid));
    let short =
        request(&mut s, "preview.show", json!({"doc": doc, "label": "x", "rect": {"x": 0, "y": 0, "width": 2, "height": 2}, "format": "rgba8"}), &[0; 3]);
    assert_eq!(short.map(|_| ()), Err(ErrorCode::Invalid));
    assert!(s.plugin_preview_row(&s.documents()[0].doc).is_none());
}
