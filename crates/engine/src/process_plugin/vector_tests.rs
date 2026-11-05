//! Tests for vector (pose) content: an 18-joint, 17-bone pose skeleton drawn as circles and lines,
//! rendered, dragged with the Move tool's command, duplicated, edited and saved.

use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Document, Layer, Size};
use photocraft_process_plugin_protocol::message::ErrorCode;
use photocraft_process_plugin_protocol::method::{VectorShape, VectorShapes};
use serde_json::{Value, json};

use crate::Session;

const P: &str = "org.example.test";
const W: u32 = 300;
const H: u32 = 420;

/// A standing figure: joints, the joint pairs of each bone, and the colour of each joint and
/// bone, as pose editors commonly draw them.
const JOINTS: [(f64, f64); 18] = [
    (153.0, 107.0),
    (152.0, 146.0),
    (128.0, 146.0),
    (116.0, 193.0),
    (100.0, 238.0),
    (176.0, 146.0),
    (188.0, 193.0),
    (204.0, 238.0),
    (131.0, 227.0),
    (129.0, 311.0),
    (128.0, 394.0),
    (174.0, 227.0),
    (176.0, 311.0),
    (178.0, 394.0),
    (143.0, 100.0),
    (162.0, 100.0),
    (132.0, 105.0),
    (174.0, 105.0),
];
const BONES: [(usize, usize); 17] =
    [(1, 2), (1, 5), (2, 3), (3, 4), (5, 6), (6, 7), (1, 8), (8, 9), (9, 10), (1, 11), (11, 12), (12, 13), (1, 0), (0, 14), (14, 16), (0, 15), (15, 17)];
const COLORS: [&str; 18] = [
    "ff0000", "ff5500", "ffaa00", "ffff00", "aaff00", "55ff00", "00ff00", "00ff55", "00ffaa", "00ffff", "00aaff", "0055ff", "0000ff", "5500ff", "aa00ff",
    "ff00ff", "ff00aa", "ff0055",
];

/// The skeleton as `Pose.to_svg` writes it: bones first (under the joints), then joints.
fn skeleton() -> String {
    let mut svg = format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{W}\" height=\"{H}\" viewBox=\"0 0 {W} {H}\">");
    for (i, (a, b)) in BONES.iter().enumerate() {
        let ((x1, y1), (x2, y2)) = (JOINTS[*a], JOINTS[*b]);
        svg += &format!(
            "<line id=\"P00_B{i:02}\" x1=\"{x1}\" y1=\"{y1}\" x2=\"{x2}\" y2=\"{y2}\" stroke=\"#{}\" stroke-width=\"4\" stroke-opacity=\"0.6\"/>",
            COLORS[i]
        );
    }
    for (i, (x, y)) in JOINTS.iter().enumerate() {
        svg += &format!("<circle id=\"P00_J{i:02}\" cx=\"{x}\" cy=\"{y}\" r=\"4\" fill=\"#{}\"/>", COLORS[i]);
    }
    svg + "</svg>"
}

fn session() -> (Session, String) {
    let mut s = Session::new();
    let mut doc = Document::new("Pose", Size::new(W, H), ColorMode::Rgb, SampleType::U8);
    doc.layers = vec![Layer::raster("Background", doc.pixel_format())];
    let index = s.add_document(doc, None);
    let id = s.documents()[index].doc.id.0.to_string();
    (s, id)
}

fn request(s: &mut Session, method: &str, params: Value) -> Result<Value, ErrorCode> {
    s.plugin_request(P, method, &params, &[]).map(|(v, _)| v).map_err(|e| e.code)
}

fn create(s: &mut Session, doc: &str) -> String {
    let created = request(s, "vector.createFromSvg", json!({"doc": doc, "name": "Pose", "svg": skeleton()})).unwrap();
    created["uid"].as_str().unwrap().to_string()
}

fn shapes(s: &mut Session, doc: &str, group: &str) -> Vec<VectorShape> {
    let v: VectorShapes = serde_json::from_value(request(s, "vector.shapes", json!({"doc": doc, "uid": group})).unwrap()).unwrap();
    v.shapes
}

fn attr(svg: &str, name: &str) -> f64 {
    let key = format!(" {name}=\"");
    let at = svg.find(&key).unwrap() + key.len();
    svg[at..].split('"').next().unwrap().parse().unwrap()
}

fn shape<'a>(all: &'a [VectorShape], id: &str) -> &'a VectorShape {
    all.iter().find(|s| s.id == id).unwrap()
}

#[test]
fn the_skeleton_renders_with_its_colours_and_opacity() {
    let (mut s, doc) = session();
    create(&mut s, &doc);
    let read = json!({"doc": doc, "source": {"kind": "composite"}, "rect": {"x": 0, "y": 0, "width": W, "height": H}, "format": "rgba8"});
    let image = s.plugin_request(P, "pixels.read", &read, &[]).unwrap().1;
    let at = |x: f64, y: f64| {
        let i = ((y as u32 * W + x as u32) * 4) as usize;
        [image[i], image[i + 1], image[i + 2], image[i + 3]]
    };
    let rgb = |hex: &str| [0, 2, 4].map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap());
    for (i, (x, y)) in JOINTS.iter().enumerate() {
        let p = at(*x, *y);
        assert_eq!(p[..3], rgb(COLORS[i]), "joint {i}");
        assert_eq!(p[3], 255, "joint {i} is opaque");
    }
    // Midway along a long bone (the right thigh, bone 7), no joint covers it: the bone's colour at
    // 60 % opacity.
    let ((x1, y1), (x2, y2)) = (JOINTS[8], JOINTS[9]);
    let p = at((x1 + x2) / 2.0, (y1 + y2) / 2.0);
    assert_eq!(p[..3], rgb(COLORS[7]));
    assert!(p[3].abs_diff(153) <= 2, "alpha {}", p[3]);
}

#[test]
fn shapes_read_back_with_ids_handles_and_geometry() {
    let (mut s, doc) = session();
    let group = create(&mut s, &doc);
    let all = shapes(&mut s, &doc, &group);
    assert_eq!(all.len(), 35);
    let nose = shape(&all, "P00_J00");
    assert!(nose.svg.starts_with("<circle id=\"P00_J00\""));
    assert!((attr(&nose.svg, "cx") - 153.0).abs() < 0.01 && (attr(&nose.svg, "cy") - 107.0).abs() < 0.01);
    assert!((attr(&nose.svg, "r") - 4.0).abs() < 0.01);
    assert!(nose.svg.contains("fill=\"#ff0000\""));
    let thigh = shape(&all, "P00_B07");
    assert!(thigh.svg.starts_with("<line"));
    assert_eq!([attr(&thigh.svg, "x1"), attr(&thigh.svg, "y1"), attr(&thigh.svg, "x2"), attr(&thigh.svg, "y2")], [131.0, 227.0, 129.0, 311.0]);
    assert!((attr(&thigh.svg, "stroke-opacity") - 0.6).abs() < 0.01 && (attr(&thigh.svg, "stroke-width") - 4.0).abs() < 0.01);
    let handles: std::collections::HashSet<&str> = all.iter().map(|s| s.handle.as_str()).collect();
    assert_eq!(handles.len(), 35, "every handle is distinct");
}

#[test]
fn dragging_a_joint_moves_it_and_reports_the_change() {
    let (mut s, doc) = session();
    let group = create(&mut s, &doc);
    request(&mut s, "events.subscribe", json!({"topics": ["pixels.changed", "layers.changed"]})).unwrap();
    let nose = shape(&shapes(&mut s, &doc, &group), "P00_J00").handle.clone();
    request(&mut s, "layer.setActive", json!({"doc": doc, "uid": nose})).unwrap();
    let _ = s.plugin_events();
    // What the Move tool does.
    s.execute("layer.translate", json!({"dx": 10.0, "dy": -5.0})).unwrap();
    let events = s.plugin_events();
    let pixels = events.iter().find(|e| e.topic == "pixels.changed").expect("a change event");
    assert_eq!(pixels.data["layers"], json!([nose]));
    let moved = shapes(&mut s, &doc, &group);
    let nose = shape(&moved, "P00_J00");
    assert!((attr(&nose.svg, "cx") - 163.0).abs() < 0.01 && (attr(&nose.svg, "cy") - 102.0).abs() < 0.01, "{}", nose.svg);
}

#[test]
fn a_duplicated_joint_is_a_second_element_with_the_same_id() {
    let (mut s, doc) = session();
    let group = create(&mut s, &doc);
    request(&mut s, "events.subscribe", json!({"topics": ["layers.changed"]})).unwrap();
    let wrist = shape(&shapes(&mut s, &doc, &group), "P00_J04").handle.clone();
    request(&mut s, "layer.setActive", json!({"doc": doc, "uid": wrist})).unwrap();
    let _ = s.plugin_events();
    s.execute("layer.duplicate", json!({})).unwrap();
    assert_eq!(s.plugin_events().iter().map(|e| e.topic).collect::<Vec<_>>(), ["layers.changed"]);
    let all = shapes(&mut s, &doc, &group);
    assert_eq!(all.len(), 36);
    let copies: Vec<&VectorShape> = all.iter().filter(|s| s.id == "P00_J04").collect();
    assert_eq!(copies.len(), 2, "both are read back");
    assert_ne!(copies[0].handle, copies[1].handle, "with distinct handles");
}

#[test]
fn elements_are_added_and_removed() {
    let (mut s, doc) = session();
    let group = create(&mut s, &doc);
    let svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{W}\" height=\"{H}\"><circle id=\"P01_J00\" cx=\"50\" cy=\"60\" r=\"4\" fill=\"#ff0000\"/></svg>"
    );
    request(&mut s, "vector.addSvg", json!({"doc": doc, "uid": group, "svg": svg})).unwrap();
    let all = shapes(&mut s, &doc, &group);
    assert_eq!(all.len(), 36);
    let bones: Vec<String> = all.iter().filter(|s| s.id.starts_with("P00_B")).map(|s| s.handle.clone()).collect();
    request(&mut s, "vector.removeShapes", json!({"doc": doc, "uid": group, "ids": bones})).unwrap();
    assert_eq!(shapes(&mut s, &doc, &group).len(), 19);
    assert!(s.undo(), "one undoable step");
    assert_eq!(shapes(&mut s, &doc, &group).len(), 36);
}

#[test]
fn the_pose_survives_saving_and_reopening() {
    let (mut s, doc) = session();
    let group = create(&mut s, &doc);
    let before = shapes(&mut s, &doc, &group);
    for ext in ["pcraft", "psd"] {
        let name = format!("pose.{ext}");
        let bytes = photocraft_io::export(&s.documents()[0].doc, &name, &photocraft_io::ExportOptions::default()).unwrap().bytes;
        let back = photocraft_io::import(&name, &bytes).unwrap().document;
        let mut fresh = Session::new();
        let index = fresh.add_document(back, None);
        let id = fresh.documents()[index].doc.id.0.to_string();
        let after = shapes(&mut fresh, &id, &group);
        let ids = |v: &[VectorShape]| v.iter().map(|s| s.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&after), ids(&before), "{ext}");
        let nose = shape(&after, "P00_J00");
        assert!((attr(&nose.svg, "cx") - 153.0).abs() < 0.01, "{ext}: {}", nose.svg);
        let thigh = shape(&after, "P00_B07");
        assert!((attr(&thigh.svg, "stroke-opacity") - 0.6).abs() < 0.01, "{ext}: {}", thigh.svg);
    }
}

#[test]
fn other_groups_and_other_content_are_refused() {
    let (mut s, doc) = session();
    let plain = request(&mut s, "layer.create", json!({"doc": doc, "kind": "group", "name": "Plain"})).unwrap()["uid"].as_str().unwrap().to_string();
    assert_eq!(request(&mut s, "vector.shapes", json!({"doc": doc, "uid": plain})).map(|_| ()), Err(ErrorCode::Invalid));
    let group = create(&mut s, &doc);
    let other = s.plugin_request("org.example.other", "vector.shapes", &json!({"doc": doc, "uid": group}), &[]).map(|_| ()).map_err(|e| e.code);
    assert_eq!(other, Err(ErrorCode::Invalid), "a plug-in sees only its own vector layers");
    let empty = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"></svg>";
    assert_eq!(request(&mut s, "vector.createFromSvg", json!({"doc": doc, "name": "E", "svg": empty})).map(|_| ()), Err(ErrorCode::Invalid));
    assert_eq!(request(&mut s, "vector.createFromSvg", json!({"doc": doc, "name": "E", "svg": "<svg"})).map(|_| ()), Err(ErrorCode::Invalid));
}

#[test]
fn moving_a_joint_with_path_selection_is_read_back_and_reported() {
    let (mut s, doc) = session();
    let group = create(&mut s, &doc);
    request(&mut s, "events.subscribe", json!({"topics": ["pixels.changed"]})).unwrap();
    let neck = shape(&shapes(&mut s, &doc, &group), "P00_J01").handle.clone();
    request(&mut s, "layer.setActive", json!({"doc": doc, "uid": neck})).unwrap();
    let _ = s.plugin_events();
    // What the Path Selection tool does with every anchor of the shape selected.
    let knots = s.documents()[0].doc.walk().into_iter().find(|(_, _, l)| photocraft_doc::uid_string(l.uid) == neck).map(|(_, _, l)| match &l.content {
        photocraft_doc::LayerContent::Shape(sh) => sh.path.subpaths[0].knots.len(),
        _ => 0,
    });
    let anchors: Vec<[usize; 2]> = (0..knots.unwrap_or(0)).map(|k| [0, k]).collect();
    s.execute("path.moveAnchors", json!({"name": "layer", "anchors": anchors, "move": [-6, 8]})).unwrap();
    assert_eq!(s.plugin_events().iter().map(|e| e.topic).collect::<Vec<_>>(), ["pixels.changed"]);
    let moved = shapes(&mut s, &doc, &group);
    let neck = shape(&moved, "P00_J01");
    assert!((attr(&neck.svg, "cx") - 146.0).abs() < 0.01 && (attr(&neck.svg, "cy") - 154.0).abs() < 0.01, "{}", neck.svg);
}
