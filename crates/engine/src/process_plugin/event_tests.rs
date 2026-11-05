//! Tests for change events: what each kind of change reports, coalescing, origins,
//! subscriptions and how quickly a change is reported.

use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Document, Layer, Size};
use photocraft_geom::Rect;
use photocraft_process_plugin_protocol::message::ErrorCode;
use serde_json::{Value, json};

use super::PluginEvent;
use crate::Session;

const P: &str = "org.example.test";
const OTHER: &str = "org.example.other";

fn document(name: &str) -> Document {
    let mut doc = Document::new(name, Size::new(64, 48), ColorMode::Rgb, SampleType::U8);
    let fmt = doc.pixel_format();
    doc.layers = vec![Layer::raster("Background", fmt), Layer::raster("Paint", fmt), Layer::raster("Top", fmt)];
    doc
}

/// A session with one document, `P` subscribed to every document topic.
fn session() -> (Session, String) {
    let mut s = Session::new();
    let index = s.add_document(document("One"), None);
    let id = s.documents()[index].doc.id.0.to_string();
    let all = ["document.opened", "document.closed", "document.saved", "document.renamed", "document.activated"];
    let more = ["layers.changed", "pixels.changed", "selection.changed", "layer.activated"];
    let topics: Vec<&str> = all.iter().chain(more.iter()).copied().collect();
    s.plugin_request(P, "events.subscribe", &json!({ "topics": topics }), &[]).unwrap();
    (s, id)
}

fn call(s: &mut Session, plugin: &str, method: &str, params: Value) -> Result<Value, ErrorCode> {
    s.plugin_request(plugin, method, &params, &[]).map(|(v, _)| v).map_err(|e| e.code)
}

fn uid(s: &Session, name: &str) -> String {
    let doc = &s.documents()[0].doc;
    doc.walk().into_iter().find(|(_, _, l)| l.name == name).map(|(_, _, l)| photocraft_doc::uid_string(l.uid)).unwrap()
}

fn topics(events: &[PluginEvent]) -> Vec<&'static str> {
    events.iter().map(|e| e.topic).collect()
}

/// A user's brush dab on "Paint": an ordinary edit touching a few pixels.
fn dab(s: &mut Session, x: i32) {
    let id = s.documents()[0].doc.walk().into_iter().find(|(_, _, l)| l.name == "Paint").map(|(_, _, l)| l.id).unwrap();
    s.edit("Brush Tool", |doc, _| {
        if let Some(surface) = doc.layer_mut(id).and_then(Layer::surface_mut) {
            surface.write_region(Rect::from_xywh(x, 4, 2, 2), &[1.0; 16]);
        }
        Ok(())
    })
    .unwrap();
}

#[test]
fn nothing_is_reported_without_subscribers_or_changes() {
    let mut s = Session::new();
    s.add_document(document("One"), None);
    dab(&mut s, 0);
    assert!(s.plugin_events().is_empty(), "nobody subscribed");
    let (mut s, _) = session();
    assert!(s.plugin_events().is_empty(), "nothing changed since subscribing");
}

#[test]
fn a_stroke_is_one_pixels_event_per_frame() {
    let (mut s, doc) = session();
    // Several dabs in one frame: one event.
    for x in 0..5 {
        dab(&mut s, x * 3);
    }
    let events = s.plugin_events();
    assert_eq!(topics(&events), ["pixels.changed"]);
    let data = &events[0].data;
    assert_eq!(data["doc"], doc);
    assert_eq!(data["layers"], json!([uid(&s, "Paint")]));
    assert!(data.get("origin").is_none(), "the user made it");
    assert_eq!(data["revision"], s.documents()[0].revision);
    // The next frame, after one more dab, reports again.
    dab(&mut s, 40);
    assert_eq!(topics(&s.plugin_events()), ["pixels.changed"]);
    assert!(s.plugin_events().is_empty());
}

#[test]
fn selection_layer_and_document_switches_are_reported() {
    let (mut s, doc) = session();
    s.execute("select.all", json!({})).unwrap();
    let events = s.plugin_events();
    assert_eq!(topics(&events), ["selection.changed"]);
    assert_eq!(events[0].data["bounds"], json!({"x": 0, "y": 0, "width": 64, "height": 48}));
    s.execute("select.deselect", json!({})).unwrap();
    assert_eq!(s.plugin_events()[0].data, json!({"doc": doc}), "no bounds: nothing selected");

    let paint = uid(&s, "Paint");
    call(&mut s, P, "layer.setActive", json!({"doc": doc, "uid": paint})).unwrap();
    let events = s.plugin_events();
    assert_eq!(topics(&events), ["layer.activated"]);
    assert_eq!(events[0].data["uid"], paint);

    let second = s.add_document(document("Two"), None);
    let events = s.plugin_events();
    assert_eq!(topics(&events), ["document.opened", "document.activated"]);
    assert_eq!(events[0].data["name"], "Two");
    s.set_active(0);
    let events = s.plugin_events();
    assert_eq!(topics(&events), ["document.activated"]);
    assert_eq!(events[0].data["doc"], doc);
    s.close(second);
    assert_eq!(topics(&s.plugin_events()), ["document.closed"]);
}

#[test]
fn renaming_and_reordering_layers_send_the_tree() {
    let (mut s, doc) = session();
    let top = uid(&s, "Top");
    call(&mut s, P, "layer.update", json!({"doc": doc, "uid": top, "name": "Renamed"})).unwrap();
    let events = s.plugin_events();
    assert_eq!(topics(&events), ["layers.changed"]);
    assert_eq!(events[0].data["tree"][2]["name"], "Renamed");
    let background = uid(&s, "Background");
    call(&mut s, P, "layer.move", json!({"doc": doc, "uid": background})).unwrap();
    let events = s.plugin_events();
    assert_eq!(topics(&events), ["layers.changed"]);
    assert_eq!(events[0].data["tree"][2]["name"], "Background", "moved to the top");
}

#[test]
fn document_renames_and_saves_are_reported() {
    let (mut s, _) = session();
    if let Some(st) = s.active_mut() {
        std::sync::Arc::make_mut(&mut st.doc).name = "Renamed".into();
        st.path = Some("renamed.pcraft".into());
    }
    let events = s.plugin_events();
    assert_eq!(topics(&events), ["document.renamed"]);
    assert_eq!(events[0].data["path"], "renamed.pcraft");
    dab(&mut s, 0);
    let _ = s.plugin_events();
    if let Some(st) = s.active_mut() {
        st.saved_to("saved.pcraft".into());
    }
    let events = s.plugin_events();
    assert_eq!(topics(&events), ["document.renamed", "document.saved"], "Save As renames too");
    if let Some(st) = s.active_mut() {
        st.saved_to("saved.pcraft".into());
    }
    assert_eq!(topics(&s.plugin_events()), ["document.saved"]);
}

#[test]
fn a_plugins_own_changes_carry_its_origin() {
    let (mut s, doc) = session();
    let paint = uid(&s, "Paint");
    s.plugin_request(OTHER, "pixels.write", &json!({"doc": doc, "uid": paint, "rect": {"x": 0, "y": 0, "width": 2, "height": 2}, "format": "rgba8"}), &[9; 16])
        .unwrap();
    let events = s.plugin_events();
    assert_eq!(topics(&events), ["pixels.changed"]);
    assert_eq!(events[0].data["origin"], OTHER);
    assert_eq!(events[0].plugin, P, "delivered to the subscriber");
    // A user edit after the plug-in's, in the same frame, is the user's.
    s.plugin_request(OTHER, "pixels.write", &json!({"doc": doc, "uid": paint, "rect": {"x": 0, "y": 0, "width": 2, "height": 2}, "format": "rgba8"}), &[3; 16])
        .unwrap();
    dab(&mut s, 20);
    assert!(s.plugin_events()[0].data.get("origin").is_none());
}

#[test]
fn subscriptions_choose_topics_and_end_with_the_plugin() {
    let (mut s, _) = session();
    call(&mut s, OTHER, "events.subscribe", json!({"topics": ["selection.changed", "panel.input"]})).unwrap();
    dab(&mut s, 0);
    s.execute("select.all", json!({})).unwrap();
    let events = s.plugin_events();
    let to_other: Vec<_> = events.iter().filter(|e| e.plugin == OTHER).map(|e| e.topic).collect();
    assert_eq!(to_other, ["selection.changed"]);
    assert_eq!(events.iter().filter(|e| e.plugin == P).count(), 2);
    assert_eq!(call(&mut s, OTHER, "events.subscribe", json!({"topics": ["nonsense"]})), Err(ErrorCode::Invalid));
    s.plugin_disconnected(OTHER);
    call(&mut s, P, "events.subscribe", json!({"topics": []})).unwrap();
    dab(&mut s, 9);
    assert!(s.plugin_events().is_empty(), "nobody is subscribed any more");
}

/// REQ-4.1/4.2: a change is reported by the next frame's comparison, which must leave
/// most of the frame for everything else.
#[test]
fn comparing_a_large_document_takes_well_under_a_frame() {
    let (mut s, _) = session();
    let mut doc = Document::new("Large", Size::new(4096, 4096), ColorMode::Rgb, SampleType::U8);
    let fmt = doc.pixel_format();
    doc.layers = (0..30)
        .map(|i| {
            let mut l = Layer::raster(format!("Layer {i}"), fmt);
            if let Some(surface) = l.surface_mut() {
                surface.fill_rect(Rect::from_xywh(0, 0, 4096, 4096), &[0.5, 0.5, 0.5, 1.0]);
            }
            l
        })
        .collect();
    s.add_document(doc, None);
    let _ = s.plugin_events();
    let start = std::time::Instant::now();
    let id = s.documents()[1].doc.layers[10].id;
    s.edit("Brush Tool", |doc, _| {
        if let Some(surface) = doc.layer_mut(id).and_then(Layer::surface_mut) {
            surface.write_region(Rect::from_xywh(100, 100, 4, 4), &[1.0; 64]);
        }
        Ok(())
    })
    .unwrap();
    let edit = start.elapsed();
    let start = std::time::Instant::now();
    let events = s.plugin_events();
    let compare = start.elapsed();
    assert_eq!(topics(&events), ["pixels.changed"]);
    assert!(compare.as_millis() < 50, "comparison took {compare:?} (the edit itself {edit:?})");
}
