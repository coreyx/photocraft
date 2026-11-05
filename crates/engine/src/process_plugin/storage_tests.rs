//! Tests for data plug-ins store in documents: blobs and layer values, their round trips
//! through `.pcraft` and PSD, editing without the plug-in, undo, duplication and limits.

use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Document, Layer, Size};
use photocraft_process_plugin_protocol::message::ErrorCode;
use serde_json::{Value, json};

use crate::Session;

const P: &str = "org.example.test";
const OTHER: &str = "org.example.other";

fn session() -> (Session, String) {
    let mut s = Session::new();
    let mut doc = Document::new("Stored", Size::new(32, 24), ColorMode::Rgb, SampleType::U8);
    let fmt = doc.pixel_format();
    doc.layers = vec![Layer::raster("Background", fmt), Layer::raster("Region", fmt)];
    let index = s.add_document(doc, None);
    let id = s.documents()[index].doc.id.0.to_string();
    (s, id)
}

fn request(s: &mut Session, plugin: &str, method: &str, params: Value, payload: &[u8]) -> Result<(Value, Vec<u8>), ErrorCode> {
    s.plugin_request(plugin, method, &params, payload).map_err(|e| e.code)
}

fn get(s: &mut Session, plugin: &str, doc: &str, name: &str) -> Option<Vec<u8>> {
    let (result, bytes) = request(s, plugin, "blob.get", json!({"doc": doc, "name": name}), &[]).unwrap();
    (result["found"] == true).then_some(bytes)
}

fn uid(s: &Session, name: &str) -> String {
    let doc = &s.documents()[0].doc;
    doc.walk().into_iter().find(|(_, _, l)| l.name == name).map(|(_, _, l)| photocraft_doc::uid_string(l.uid)).unwrap()
}

fn layer_value(s: &mut Session, doc: &str, layer: &str, key: &str) -> Value {
    request(s, P, "layerData.get", json!({"doc": doc, "uid": layer, "key": key}), &[]).unwrap().0["value"].clone()
}

/// Every byte value, so a lossy encoding anywhere shows.
fn webp_like() -> Vec<u8> {
    (0..4096u32).map(|i| (i * 7 % 256) as u8).collect()
}

/// `s`'s document, saved as `ext` and opened again in a fresh session.
fn reopen(s: &Session, ext: &str) -> (Session, String) {
    let name = format!("stored.{ext}");
    let bytes = photocraft_io::export(&s.documents()[0].doc, &name, &photocraft_io::ExportOptions::default()).unwrap().bytes;
    let doc = photocraft_io::import(&name, &bytes).unwrap().document;
    let mut fresh = Session::new();
    let index = fresh.add_document(doc, None);
    let id = fresh.documents()[index].doc.id.0.to_string();
    (fresh, id)
}

/// A document holding blobs from two plug-ins and a value on a layer.
fn stored() -> (Session, String) {
    let (mut s, doc) = session();
    request(&mut s, P, "blob.set", json!({"doc": doc, "name": "result0.webp"}), &webp_like()).unwrap();
    request(&mut s, P, "blob.set", json!({"doc": doc, "name": "ui.json"}), br#"{"prompt":"a fox"}"#).unwrap();
    request(&mut s, OTHER, "blob.set", json!({"doc": doc, "name": "ui.json"}), b"other").unwrap();
    let region = uid(&s, "Region");
    request(&mut s, P, "layerData.set", json!({"doc": doc, "uid": region, "key": "role", "value": "region \"1\" <&>"}), &[]).unwrap();
    (s, doc)
}

#[test]
fn blobs_are_set_read_listed_and_deleted_per_plugin() {
    let (mut s, doc) = stored();
    assert_eq!(get(&mut s, P, &doc, "result0.webp"), Some(webp_like()));
    assert_eq!(get(&mut s, OTHER, &doc, "ui.json"), Some(b"other".to_vec()), "each plug-in has its own names");
    assert_eq!(get(&mut s, OTHER, &doc, "result0.webp"), None);
    let names = request(&mut s, P, "blob.list", json!({"doc": doc}), &[]).unwrap().0;
    assert_eq!(names["names"], json!(["result0.webp", "ui.json"]));
    request(&mut s, P, "blob.delete", json!({"doc": doc, "name": "ui.json"}), &[]).unwrap();
    assert_eq!(get(&mut s, P, &doc, "ui.json"), None);
    assert_eq!(get(&mut s, OTHER, &doc, "ui.json"), Some(b"other".to_vec()));
}

#[test]
fn storing_data_marks_the_document_modified_without_a_history_step() {
    let (mut s, doc) = session();
    let steps = s.documents()[0].history.undo_label().map(str::to_string);
    assert!(!s.documents()[0].is_dirty());
    request(&mut s, P, "blob.set", json!({"doc": doc, "name": "ui.json"}), b"{}").unwrap();
    assert!(s.documents()[0].is_dirty(), "the data is saved with the document");
    assert_eq!(s.documents()[0].history.undo_label().map(str::to_string), steps, "no history step");
}

#[test]
fn undo_keeps_the_current_data() {
    let (mut s, doc) = stored();
    let region = uid(&s, "Region");
    // An edit, then new data: undoing the edit keeps the newer data.
    request(&mut s, P, "layer.create", json!({"doc": doc, "kind": "pixel", "name": "Result"}), &[]).unwrap();
    request(&mut s, P, "blob.set", json!({"doc": doc, "name": "ui.json"}), b"newer").unwrap();
    request(&mut s, P, "layerData.set", json!({"doc": doc, "uid": region, "key": "role", "value": "newer"}), &[]).unwrap();
    assert!(s.undo());
    assert_eq!(s.documents()[0].doc.layers.len(), 2, "the edit was undone");
    assert_eq!(get(&mut s, P, &doc, "ui.json"), Some(b"newer".to_vec()));
    assert_eq!(layer_value(&mut s, &doc, &region, "role"), "newer");
    assert!(s.redo());
    assert_eq!(get(&mut s, P, &doc, "ui.json"), Some(b"newer".to_vec()));
}

#[test]
fn blobs_survive_pcraft_and_psd_byte_for_byte() {
    let (s, _) = stored();
    for ext in ["pcraft", "psd", "psb"] {
        let (mut back, doc) = reopen(&s, ext);
        assert_eq!(get(&mut back, P, &doc, "result0.webp"), Some(webp_like()), "{ext}");
        assert_eq!(get(&mut back, P, &doc, "ui.json"), Some(br#"{"prompt":"a fox"}"#.to_vec()), "{ext}");
        assert_eq!(get(&mut back, OTHER, &doc, "ui.json"), Some(b"other".to_vec()), "{ext}");
        let region = uid(&back, "Region");
        assert_eq!(layer_value(&mut back, &doc, &region, "role"), "region \"1\" <&>", "{ext}");
        assert_eq!(region, uid(&s, "Region"), "{ext}: the layer keeps its uid");
        assert!(back.documents()[0].doc.walk().into_iter().all(|(_, _, l)| l.xmp.is_none()), "{ext}: no XMP left behind");
    }
}

#[test]
fn editing_and_saving_without_the_plugin_keeps_the_data() {
    let (s, _) = stored();
    for ext in ["pcraft", "psd"] {
        let (mut back, doc) = reopen(&s, ext);
        // No plug-in here: the user edits and saves again.
        let background = uid(&back, "Background");
        back.plugin_request("org.example.unrelated", "layer.update", &json!({"doc": doc, "uid": background, "name": "Base"}), &[]).unwrap();
        back.execute("select.all", json!({})).unwrap();
        let (mut again, doc) = reopen(&back, ext);
        assert_eq!(get(&mut again, P, &doc, "result0.webp"), Some(webp_like()), "{ext}");
        assert_eq!(get(&mut again, OTHER, &doc, "ui.json"), Some(b"other".to_vec()), "{ext}");
    }
}

#[test]
fn layer_values_follow_duplicates() {
    let (mut s, doc) = stored();
    let region = uid(&s, "Region");
    let id = s.documents()[0].doc.walk().into_iter().find(|(_, _, l)| l.name == "Region").map(|(_, _, l)| l.id).unwrap();
    s.edit("Duplicate Layer", |d, _| {
        let copy = d.layer(id).map(Layer::duplicate).ok_or(crate::EngineError::NoDocument)?;
        d.layers.push(copy);
        Ok(())
    })
    .unwrap();
    let copy = photocraft_doc::uid_string(s.documents()[0].doc.layers.last().unwrap().uid);
    assert_ne!(copy, region);
    assert_eq!(layer_value(&mut s, &doc, &copy, "role"), "region \"1\" <&>");
    request(&mut s, P, "layerData.set", json!({"doc": doc, "uid": copy, "key": "role"}), &[]).unwrap();
    assert_eq!(layer_value(&mut s, &doc, &copy, "role"), Value::Null, "removed from the copy");
    assert_eq!(layer_value(&mut s, &doc, &region, "role"), "region \"1\" <&>", "the original keeps it");
}

#[test]
fn limits_are_enforced() {
    let (mut s, doc) = session();
    let too_big = vec![0u8; (photocraft_doc::plugin_data::MAX_DOCUMENT_BYTES + 1) as usize];
    assert_eq!(request(&mut s, P, "blob.set", json!({"doc": doc, "name": "big"}), &too_big).map(|_| ()), Err(ErrorCode::TooLarge));
    assert!(!s.documents()[0].is_dirty(), "nothing stored");
    assert_eq!(request(&mut s, P, "blob.set", json!({"doc": doc, "name": ""}), b"x").map(|_| ()), Err(ErrorCode::Invalid));
    let region = uid(&s, "Region");
    let long = "v".repeat(photocraft_doc::plugin_data::MAX_LAYER_BYTES);
    let r = request(&mut s, P, "layerData.set", json!({"doc": doc, "uid": region, "key": "k", "value": long}), &[]);
    assert_eq!(r.map(|_| ()), Err(ErrorCode::TooLarge));
}

#[test]
fn formats_that_cannot_keep_the_data_say_so() {
    let (s, _) = stored();
    let png = photocraft_io::export(&s.documents()[0].doc, "flat.png", &photocraft_io::ExportOptions::default()).unwrap();
    assert!(png.warnings.iter().any(|w| w.contains("plug-in data")));
    let psd = photocraft_io::export(&s.documents()[0].doc, "kept.psd", &photocraft_io::ExportOptions::default()).unwrap();
    assert!(!psd.warnings.iter().any(|w| w.contains("plug-in data")));
}
