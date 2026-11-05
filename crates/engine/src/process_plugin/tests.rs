//! Tests for the process plug-in broker: documents, layers, undo groups and hostile input.

use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Document, Layer, Size};
use photocraft_process_plugin_protocol::message::ErrorCode;
use photocraft_process_plugin_protocol::types::{LayerInfo, LayerKind};
use serde_json::{Value, json};

use crate::Session;

const P: &str = "org.example.test";

/// A session with one RGB document: Background, a group holding "Child", and "Top".
fn session() -> (Session, String) {
    let mut s = Session::new();
    let mut doc = Document::new("Test", Size::new(64, 48), ColorMode::Rgb, SampleType::U8);
    let fmt = doc.pixel_format();
    let mut bg = Layer::raster("Background", fmt);
    if let Some(surface) = bg.surface_mut() {
        surface.write_region(photocraft_geom::Rect::from_xywh(0, 0, 64, 48), &vec![0.5; 64 * 48 * 4]);
    }
    let mut child = Layer::raster("Child", fmt);
    if let Some(surface) = child.surface_mut() {
        surface.write_region(photocraft_geom::Rect::from_xywh(10, 5, 4, 3), &[1.0; 4 * 3 * 4]);
    }
    doc.layers = vec![bg, Layer::group("Group", vec![child]), Layer::raster("Top", fmt)];
    let index = s.add_document(doc, None);
    let id = s.documents()[index].doc.id.0.to_string();
    (s, id)
}

fn call(s: &mut Session, method: &str, params: Value) -> Result<Value, ErrorCode> {
    s.plugin_request(P, method, &params, &[]).map(|(v, _)| v).map_err(|e| e.code)
}

fn tree(s: &mut Session, doc: &str) -> Vec<LayerInfo> {
    serde_json::from_value(call(s, "layer.tree", json!({"doc": doc})).unwrap()).unwrap()
}

fn uid_of(tree: &[LayerInfo], name: &str) -> String {
    fn find(t: &[LayerInfo], name: &str) -> Option<String> {
        t.iter().find_map(|l| if l.name == name { Some(l.uid.clone()) } else { find(&l.children, name) })
    }
    find(tree, name).unwrap()
}

fn names(tree: &[LayerInfo]) -> Vec<String> {
    tree.iter().map(|l| l.name.clone()).collect()
}

#[test]
fn documents_are_listed_and_described() {
    let (mut s, doc) = session();
    let list = call(&mut s, "doc.list", Value::Null).unwrap();
    assert_eq!(list[0]["doc"], json!(doc));
    assert_eq!(list[0]["width"], 64);
    assert_eq!(list[0]["depth"], "u8");
    assert_eq!(list[0]["mode"], "rgb");
    assert_eq!(list[0]["active"], true);
    assert_eq!(call(&mut s, "doc.info", json!({"doc": doc})).unwrap()["name"], "Test");
    assert_eq!(call(&mut s, "doc.info", json!({"doc": "999999"})), Err(ErrorCode::NotFound));
    assert_eq!(call(&mut s, "doc.info", json!({"doc": "nope"})), Err(ErrorCode::Invalid));
}

#[test]
fn the_layer_tree_has_parents_order_kinds_and_bounds() {
    let (mut s, doc) = session();
    let t = tree(&mut s, &doc);
    assert_eq!(names(&t), ["Background", "Group", "Top"]);
    assert_eq!(t[1].kind, LayerKind::Group);
    assert_eq!(t[1].children[0].name, "Child");
    assert_eq!(t[1].children[0].kind, LayerKind::Pixel);
    let child = &t[1].children[0].bounds;
    assert_eq!((child.x, child.y, child.width, child.height), (10, 5, 4, 3));
    assert_eq!((t[1].bounds.x, t[1].bounds.width), (10, 4), "a group spans its children");
    assert_eq!(t[0].blend, "normal");
    assert_eq!(t[1].blend, "passThrough");
}

#[test]
fn a_psd_round_trip_keeps_the_tree_the_plugin_sees() {
    let (mut s, doc) = session();
    let before = tree(&mut s, &doc);
    let st = &s.documents()[0];
    let opts = photocraft_io::ExportOptions { layer_uids: true, ..Default::default() };
    let bytes = photocraft_io::export(&st.doc, "x.psd", &opts).unwrap().bytes;
    let back = photocraft_io::import("x.psd", &bytes).unwrap().document;
    let after = super::layers::tree_of(&back);
    assert_eq!(names(&after), names(&before));
    assert_eq!(after[1].children[0].uid, before[1].children[0].uid);
    assert_eq!(after[1].children[0].bounds, before[1].children[0].bounds);
}

#[test]
fn every_layer_change_is_one_undoable_step() {
    let (mut s, doc) = session();
    let top = uid_of(&tree(&mut s, &doc), "Top");
    let group = uid_of(&tree(&mut s, &doc), "Group");
    let steps: Vec<(&str, Value)> = vec![
        ("layer.create", json!({"doc": doc, "kind": "pixel", "name": "New"})),
        ("layer.create", json!({"doc": doc, "kind": "group", "name": "Region 1", "parent": group})),
        ("layer.update", json!({"doc": doc, "uid": top, "name": "Renamed", "visible": false, "locked": true, "opacity": 0.5, "blend": "multiply"})),
        ("layer.move", json!({"doc": doc, "uid": top, "parent": group})),
        ("layer.delete", json!({"doc": doc, "uid": top})),
    ];
    for (method, params) in steps {
        let before = tree(&mut s, &doc);
        call(&mut s, method, params).unwrap_or_else(|e| panic!("{method}: {e:?}"));
        assert_ne!(tree(&mut s, &doc), before, "{method} changed the document");
        assert!(s.undo(), "{method} can be undone");
        assert_eq!(tree(&mut s, &doc), before, "{method} undone in one step");
        assert!(s.redo());
    }
}

#[test]
fn created_layers_go_where_placed() {
    let (mut s, doc) = session();
    let t = tree(&mut s, &doc);
    let (bg, group, child) = (uid_of(&t, "Background"), uid_of(&t, "Group"), uid_of(&t, "Child"));
    call(&mut s, "layer.create", json!({"doc": doc, "kind": "pixel", "name": "Above BG", "above": bg})).unwrap();
    call(&mut s, "layer.create", json!({"doc": doc, "kind": "pixel", "name": "In group", "parent": group})).unwrap();
    let t = tree(&mut s, &doc);
    assert_eq!(names(&t), ["Background", "Above BG", "Group", "Top"]);
    assert_eq!(names(&t[2].children), ["Child", "In group"]);
    // The new layer becomes the active one.
    assert_eq!(call(&mut s, "layer.active", json!({"doc": doc})).unwrap(), json!(uid_of(&t, "In group")));
    // 'above' must be a child of 'parent', and a pixel layer can't be a parent.
    assert_eq!(call(&mut s, "layer.create", json!({"doc": doc, "kind": "pixel", "name": "x", "parent": group, "above": bg})), Err(ErrorCode::Invalid));
    assert_eq!(call(&mut s, "layer.create", json!({"doc": doc, "kind": "pixel", "name": "x", "parent": child})), Err(ErrorCode::Invalid));
}

#[test]
fn a_group_cannot_move_into_itself() {
    let (mut s, doc) = session();
    let t = tree(&mut s, &doc);
    let (group, child) = (uid_of(&t, "Group"), uid_of(&t, "Child"));
    assert_eq!(call(&mut s, "layer.move", json!({"doc": doc, "uid": group, "parent": group})), Err(ErrorCode::Invalid));
    assert_eq!(call(&mut s, "layer.move", json!({"doc": doc, "uid": group, "above": child})), Err(ErrorCode::Invalid));
    assert_eq!(tree(&mut s, &doc), t);
}

#[test]
fn an_undo_group_is_one_step() {
    let (mut s, doc) = session();
    let before = tree(&mut s, &doc);
    let group = call(&mut s, "undo.begin", json!({"doc": doc, "label": "Apply Result"})).unwrap()["group"].clone();
    let a = call(&mut s, "layer.create", json!({"doc": doc, "kind": "pixel", "name": "A"})).unwrap()["uid"].clone();
    call(&mut s, "layer.create", json!({"doc": doc, "kind": "pixel", "name": "B"})).unwrap();
    call(&mut s, "layer.update", json!({"doc": doc, "uid": a, "name": "A2"})).unwrap();
    call(&mut s, "undo.end", json!({"group": group})).unwrap();
    assert_eq!(s.documents()[0].history.undo_label(), Some("Apply Result"));
    assert!(s.undo());
    assert_eq!(tree(&mut s, &doc), before, "one undo reverts all three");
    // Ending it twice is an error.
    assert_eq!(call(&mut s, "undo.end", json!({"group": group})), Err(ErrorCode::NotFound));
}

#[test]
fn a_disconnected_plugins_open_group_is_closed() {
    let (mut s, doc) = session();
    call(&mut s, "undo.begin", json!({"doc": doc, "label": "Pending"})).unwrap();
    call(&mut s, "layer.create", json!({"doc": doc, "kind": "pixel", "name": "A"})).unwrap();
    s.plugin_disconnected(P);
    call(&mut s, "layer.create", json!({"doc": doc, "kind": "pixel", "name": "B"})).unwrap();
    assert_eq!(s.documents()[0].history.undo_label(), Some("New Layer"), "the next edit has a step of its own");
}

#[test]
fn stale_layers_and_documents_change_nothing() {
    let (mut s, doc) = session();
    let before = tree(&mut s, &doc);
    let ghost = photocraft_doc::uid_string(photocraft_doc::new_uid());
    for (method, params) in [
        ("layer.update", json!({"doc": doc, "uid": ghost, "name": "x"})),
        ("layer.delete", json!({"doc": doc, "uid": ghost})),
        ("layer.move", json!({"doc": doc, "uid": ghost})),
        ("layer.setActive", json!({"doc": doc, "uid": ghost})),
        ("layer.create", json!({"doc": doc, "kind": "pixel", "name": "x", "above": ghost})),
        ("layer.tree", json!({"doc": "424242"})),
    ] {
        assert_eq!(call(&mut s, method, params), Err(ErrorCode::NotFound), "{method}");
    }
    assert_eq!(tree(&mut s, &doc), before);
    assert!(!s.documents()[0].history.can_undo(), "no history step was recorded");
}

#[test]
fn a_background_document_is_edited_without_switching_to_it() {
    let (mut s, doc) = session();
    let other = Document::new("Other", Size::new(8, 8), ColorMode::Rgb, SampleType::U8);
    let other_index = s.add_document(other, None);
    s.set_active(other_index);
    call(&mut s, "layer.create", json!({"doc": doc, "kind": "pixel", "name": "Late result"})).unwrap();
    assert_eq!(s.active_index(), Some(other_index));
    assert!(names(&tree(&mut s, &doc)).contains(&"Late result".to_string()));
    assert!(s.documents()[0].history.can_undo());
    assert!(!s.documents()[other_index].history.can_undo());
}

#[test]
fn masks_come_from_an_image_or_transparency() {
    let (mut s, doc) = session();
    let top = uid_of(&tree(&mut s, &doc), "Top");
    let gray = vec![128u8; 64 * 48];
    s.plugin_request(P, "layer.addMask", &json!({"doc": doc, "uid": top, "from": "image"}), &gray).unwrap();
    assert!(tree(&mut s, &doc)[2].has_mask);
    assert_eq!(
        s.plugin_request(P, "layer.addMask", &json!({"doc": doc, "uid": top, "from": "image"}), &[0; 10]).map(|_| ()).map_err(|e| e.code),
        Err(ErrorCode::Invalid),
        "a payload of the wrong size"
    );
    let child = uid_of(&tree(&mut s, &doc), "Child");
    call(&mut s, "layer.addMask", json!({"doc": doc, "uid": child, "from": "alpha"})).unwrap();
    assert!(tree(&mut s, &doc)[1].children[0].has_mask);
}

#[test]
fn thumbnails_are_png() {
    let (mut s, doc) = session();
    let child = uid_of(&tree(&mut s, &doc), "Child");
    let (_, png) = s.plugin_request(P, "layer.thumbnail", &json!({"doc": doc, "uid": child, "width": 32, "height": 32}), &[]).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
}

#[test]
fn canvas_resize_and_activation_work() {
    let (mut s, doc) = session();
    call(&mut s, "doc.resizeCanvas", json!({"doc": doc, "width": 128, "height": 96, "anchor": "center"})).unwrap();
    assert_eq!(s.documents()[0].doc.size, Size::new(128, 96));
    assert!(s.undo());
    assert_eq!(call(&mut s, "doc.resizeCanvas", json!({"doc": doc, "width": 0, "height": 96})), Err(ErrorCode::Invalid));
    let other = s.add_document(Document::new("Other", Size::new(8, 8), ColorMode::Rgb, SampleType::U8), None);
    let other_id = s.documents()[other].doc.id.0.to_string();
    call(&mut s, "doc.setActive", json!({"doc": doc})).unwrap();
    assert_eq!(s.active_index(), Some(0));
    call(&mut s, "doc.setActive", json!({"doc": other_id})).unwrap();
    assert_eq!(s.active_index(), Some(other));
}

#[test]
fn unknown_methods_are_unsupported() {
    let (mut s, _) = session();
    assert_eq!(call(&mut s, "layer.teleport", Value::Null), Err(ErrorCode::Unsupported));
}

/// Every method, with every kind of hostile parameter: never a panic, always an answer.
#[test]
fn hostile_parameters_never_panic() {
    let (mut s, doc) = session();
    let uid = uid_of(&tree(&mut s, &doc), "Child");
    let methods = [
        "doc.list",
        "doc.info",
        "doc.setActive",
        "doc.resizeCanvas",
        "doc.open",
        "layer.tree",
        "layer.active",
        "layer.setActive",
        "layer.create",
        "layer.update",
        "layer.move",
        "layer.delete",
        "layer.addMask",
        "layer.thumbnail",
        "undo.begin",
        "undo.end",
    ];
    let values = [
        Value::Null,
        json!(true),
        json!(-1),
        json!(1e308),
        json!(u64::MAX),
        json!(""),
        json!("\u{0}"),
        json!([]),
        json!({}),
        json!({"doc": doc}),
        json!({"doc": doc, "uid": uid}),
        json!({"doc": doc, "uid": uid, "width": u32::MAX, "height": u32::MAX}),
        json!({"doc": doc, "uid": "zz", "kind": "pixel", "name": "x".repeat(5000)}),
        json!({"doc": doc, "uid": uid, "opacity": -3.0, "blend": "nonsense"}),
        json!({"doc": doc, "uid": uid, "parent": uid, "above": uid}),
        json!({"doc": doc, "uid": uid, "from": "image"}),
        json!({"doc": doc, "label": "", "group": u64::MAX}),
        json!({"doc": doc, "path": "\u{0}/nothing/here.psd"}),
        json!({"doc": "-1", "uid": "-"}),
    ];
    for method in methods {
        for v in &values {
            for payload in [&[][..], &[0u8; 3][..]] {
                let _ = s.plugin_request(P, method, v, payload);
            }
        }
    }
}
