//! Tests for `pixels.*`, `selection.*` and `mask.*`: exact 8-bit round trips, colour conversion
//! for every supported depth and mode against the canvas's own display conversion, selections,
//! composite exclusion, masks and hostile input.

use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Layer, LayerContent, Size};
use photocraft_geom::Rect;
use photocraft_process_plugin_protocol::message::ErrorCode;
use photocraft_process_plugin_protocol::types::LayerInfo;
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::Session;

const P: &str = "org.example.test";
const W: u32 = 40;
const H: u32 = 30;

/// A varied, partly transparent test pattern (every alpha non-zero, so colour survives).
fn pattern(w: u32, h: u32, channels: usize) -> Vec<f32> {
    let mut v = Vec::with_capacity((w * h) as usize * (channels + 1));
    for y in 0..h {
        for x in 0..w {
            for c in 0..channels {
                v.push(((x * 7 + y * 3 + c as u32 * 50) % 256) as f32 / 255.0);
            }
            v.push((((x + y) * 9) % 255 + 1) as f32 / 255.0);
        }
    }
    v
}

/// A document in `mode`/`depth`: "Base" holding the pattern and an empty "Top", in a session.
fn session_with(mode: ColorMode, depth: SampleType) -> (Session, String) {
    let mut s = Session::new();
    let mut doc = Document::new("Pixels", Size::new(W, H), mode, depth);
    let fmt = doc.pixel_format();
    let mut base = Surface::new(fmt);
    base.write_region(Rect::from_xywh(0, 0, W, H), &pattern(W, H, fmt.mode.color_channels()));
    doc.layers = vec![Layer::new("Base", LayerContent::Raster(base)), Layer::raster("Top", fmt)];
    let index = s.add_document(doc, None);
    let id = s.documents()[index].doc.id.0.to_string();
    (s, id)
}

fn request(s: &mut Session, method: &str, params: Value, payload: &[u8]) -> Result<(Value, Vec<u8>), ErrorCode> {
    s.plugin_request(P, method, &params, payload).map_err(|e| e.code)
}

fn uid(s: &mut Session, doc: &str, name: &str) -> String {
    let tree: Vec<LayerInfo> = serde_json::from_value(request(s, "layer.tree", json!({"doc": doc}), &[]).unwrap().0).unwrap();
    fn find(t: &[LayerInfo], name: &str) -> Option<String> {
        t.iter().find_map(|l| if l.name == name { Some(l.uid.clone()) } else { find(&l.children, name) })
    }
    find(&tree, name).unwrap()
}

fn rect(x: i64, y: i64, w: u32, h: u32) -> Value {
    json!({"x": x, "y": y, "width": w, "height": h})
}

fn read(s: &mut Session, doc: &str, source: Value, format: &str) -> Vec<u8> {
    request(s, "pixels.read", json!({"doc": doc, "source": source, "rect": rect(0, 0, W, H), "format": format}), &[]).unwrap().1
}

fn composite(s: &mut Session, doc: &str) -> Vec<u8> {
    read(s, doc, json!({"kind": "composite"}), "rgba8")
}

/// What the canvas shows for the document, as straight RGBA8 on an sRGB monitor.
fn shown(s: &Session) -> Vec<u8> {
    let doc = &s.documents()[0].doc;
    let buffer = photocraft_compose::render(doc, Rect::from_xywh(0, 0, W, H));
    s.color.canvas_display(doc).unwrap().to_rgba8(&buffer).pixels
}

/// Largest difference between two equally long byte images, colour channels of visible pixels
/// only (a fully transparent pixel has no colour to compare).
fn max_diff(a: &[u8], b: &[u8]) -> u8 {
    assert_eq!(a.len(), b.len());
    a.as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0.iter())
        .filter(|(p, q)| p[3] > 8 && q[3] > 8)
        .flat_map(|(p, q)| (0..4).map(move |i| p[i].abs_diff(q[i])))
        .max()
        .unwrap_or(0)
}

fn layer_bytes(s: &Session, name: &str) -> Vec<u8> {
    let doc = &s.documents()[0].doc;
    let layer = doc.walk().into_iter().map(|(_, _, l)| l).find(|l| l.name == name).unwrap();
    layer.surface().unwrap().to_interleaved(Rect::from_xywh(0, 0, W, H))
}

#[test]
fn an_8_bit_srgb_round_trip_is_exact() {
    let (mut s, doc) = session_with(ColorMode::Rgb, SampleType::U8);
    let base = uid(&mut s, &doc, "Base");
    let stored = layer_bytes(&s, "Base");
    let rgba = read(&mut s, &doc, json!({"kind": "layer", "uid": base}), "rgba8");
    assert_eq!(rgba, stored, "an 8-bit sRGB layer reads as its stored values");
    let bgra = read(&mut s, &doc, json!({"kind": "layer", "uid": base}), "bgra8");
    assert!(bgra.as_chunks::<4>().0.iter().zip(rgba.as_chunks::<4>().0.iter()).all(|(b, r)| *b == [r[2], r[1], r[0], r[3]]));
    request(&mut s, "pixels.write", json!({"doc": doc, "uid": base, "rect": rect(0, 0, W, H), "format": "bgra8"}), &bgra).unwrap();
    assert_eq!(layer_bytes(&s, "Base"), stored, "writing the read pixels back changes nothing");
}

#[test]
fn a_16_bit_rgb_document_converts_within_one_step() {
    let (mut s, doc) = session_with(ColorMode::Rgb, SampleType::U16);
    let base = uid(&mut s, &doc, "Base");
    let reference: Vec<u8> = pattern(W, H, 3).iter().map(|v| (v * 255.0 + 0.5) as u8).collect();
    let rgba = read(&mut s, &doc, json!({"kind": "layer", "uid": base}), "rgba8");
    assert!(max_diff(&rgba, &reference) <= 1);
    // 16-bit reads keep the extra precision.
    let wide = read(&mut s, &doc, json!({"kind": "layer", "uid": base}), "rgba16");
    assert_eq!(wide.len(), (W * H * 8) as usize);
    // An 8-bit write into the 16-bit layer shows as what was written.
    let written: Vec<u8> = (0..W * H * 4).map(|i| if i % 4 == 3 { 255 } else { (i * 13 % 251) as u8 }).collect();
    request(&mut s, "pixels.write", json!({"doc": doc, "uid": base, "rect": rect(0, 0, W, H), "format": "rgba8"}), &written).unwrap();
    assert!(max_diff(&read(&mut s, &doc, json!({"kind": "layer", "uid": base}), "rgba8"), &written) <= 1);
    assert!(max_diff(&shown(&s), &written) <= 1, "the display matches within 1/255");
}

#[test]
fn cmyk_lab_gray_and_float_documents_read_as_the_canvas_shows_them() {
    for (mode, depth) in [
        (ColorMode::Cmyk, SampleType::U8),
        (ColorMode::Lab, SampleType::U16),
        (ColorMode::Lab, SampleType::F32),
        (ColorMode::Grayscale, SampleType::U8),
        (ColorMode::Rgb, SampleType::F32),
    ] {
        let (mut s, doc) = session_with(mode, depth);
        let read = composite(&mut s, &doc);
        let diff = max_diff(&read, &shown(&s));
        assert!(diff <= 2, "{mode:?} {depth:?}: read differs from the display by {diff}");
    }
}

#[test]
fn writes_into_other_modes_show_what_was_written() {
    // Muted colours, inside every one of these modes' gamuts.
    let written: Vec<u8> = (0..W * H).flat_map(|i| [90 + (i % 60) as u8, 110, 140 - (i % 40) as u8, 255]).collect();
    for (mode, depth) in [(ColorMode::Cmyk, SampleType::U8), (ColorMode::Lab, SampleType::F32), (ColorMode::Rgb, SampleType::F32)] {
        let (mut s, doc) = session_with(mode, depth);
        let base = uid(&mut s, &doc, "Base");
        request(&mut s, "pixels.write", json!({"doc": doc, "uid": base, "rect": rect(0, 0, W, H), "format": "rgba8"}), &written).unwrap();
        let diff = max_diff(&shown(&s), &written);
        assert!(diff <= 2, "{mode:?} {depth:?}: the written pixels show {diff} off");
    }
    let gray: Vec<u8> = (0..W * H).flat_map(|i| [(i % 200) as u8 + 20; 3].into_iter().chain([255])).collect();
    let (mut s, doc) = session_with(ColorMode::Grayscale, SampleType::U16);
    let base = uid(&mut s, &doc, "Base");
    request(&mut s, "pixels.write", json!({"doc": doc, "uid": base, "rect": rect(0, 0, W, H), "format": "rgba8"}), &gray).unwrap();
    assert!(max_diff(&shown(&s), &gray) <= 2, "neutral pixels written to a gray document show unchanged");
}

#[test]
fn modes_without_an_rgb_view_are_unsupported() {
    for mode in [ColorMode::Bitmap, ColorMode::Indexed, ColorMode::Multichannel, ColorMode::Duotone] {
        let (mut s, doc) = session_with(mode, SampleType::U8);
        let r = request(&mut s, "pixels.read", json!({"doc": doc, "source": {"kind": "composite"}, "rect": rect(0, 0, 4, 4), "format": "rgba8"}), &[]);
        assert_eq!(r.map(|_| ()), Err(ErrorCode::Unsupported), "{mode:?}");
    }
}

#[test]
fn excluding_a_layer_is_the_same_as_hiding_it() {
    let (mut s, doc) = session_with(ColorMode::Rgb, SampleType::U8);
    let top = uid(&mut s, &doc, "Top");
    let red: Vec<u8> = (0..10 * 10).flat_map(|_| [255, 0, 0, 200]).collect();
    request(&mut s, "pixels.write", json!({"doc": doc, "uid": top, "rect": rect(5, 5, 10, 10), "format": "rgba8"}), &red).unwrap();
    let with_top = composite(&mut s, &doc);
    let excluded = request(
        &mut s,
        "pixels.read",
        json!({"doc": doc, "source": {"kind": "composite"}, "rect": rect(0, 0, W, H), "format": "rgba8", "exclude": [top]}),
        &[],
    )
    .unwrap()
    .1;
    assert_ne!(excluded, with_top);
    request(&mut s, "layer.update", json!({"doc": doc, "uid": top, "visible": false}), &[]).unwrap();
    assert_eq!(composite(&mut s, &doc), excluded);
}

#[test]
fn a_layer_reads_without_its_opacity_mask_or_visibility() {
    let (mut s, doc) = session_with(ColorMode::Rgb, SampleType::U8);
    let base = uid(&mut s, &doc, "Base");
    let before = read(&mut s, &doc, json!({"kind": "layer", "uid": base}), "rgba8");
    request(&mut s, "layer.update", json!({"doc": doc, "uid": base, "visible": false, "opacity": 0.25}), &[]).unwrap();
    request(&mut s, "layer.addMask", json!({"doc": doc, "uid": base, "from": "image"}), &vec![0u8; (W * H) as usize]).unwrap();
    assert_eq!(read(&mut s, &doc, json!({"kind": "layer", "uid": base}), "rgba8"), before);
    // The mask itself reads back, and the alpha is the layer's own.
    let mask = request(&mut s, "mask.read", json!({"doc": doc, "uid": base, "which": "layerMask", "rect": rect(0, 0, W, H)}), &[]).unwrap().1;
    assert!(mask.iter().all(|v| *v == 0));
    let alpha = request(&mut s, "mask.read", json!({"doc": doc, "uid": base, "which": "alpha", "rect": rect(0, 0, W, H)}), &[]).unwrap().1;
    assert!(alpha.iter().zip(before.as_chunks::<4>().0.iter()).all(|(a, p)| *a == p[3]));
    let top = uid(&mut s, &doc, "Top");
    let none = request(&mut s, "mask.read", json!({"doc": doc, "uid": top, "which": "layerMask", "rect": rect(0, 0, 4, 4)}), &[]);
    assert_eq!(none.map(|_| ()), Err(ErrorCode::NotFound));
}

#[test]
fn a_group_reads_as_its_composite() {
    let (mut s, doc) = session_with(ColorMode::Rgb, SampleType::U8);
    let base = uid(&mut s, &doc, "Base");
    let created = request(&mut s, "layer.create", json!({"doc": doc, "kind": "group", "name": "G"}), &[]).unwrap().0;
    let group = created["uid"].as_str().unwrap().to_string();
    request(&mut s, "layer.move", json!({"doc": doc, "uid": base, "parent": group}), &[]).unwrap();
    let as_layer = read(&mut s, &doc, json!({"kind": "layer", "uid": base}), "rgba8");
    assert_eq!(read(&mut s, &doc, json!({"kind": "group", "uid": group}), "rgba8"), as_layer);
    let wrong =
        request(&mut s, "pixels.read", json!({"doc": doc, "source": {"kind": "layer", "uid": group}, "rect": rect(0, 0, 4, 4), "format": "rgba8"}), &[]);
    assert_eq!(wrong.map(|_| ()), Err(ErrorCode::Invalid));
}

#[test]
fn a_new_layer_holds_the_pixels_at_their_offset() {
    let (mut s, doc) = session_with(ColorMode::Rgb, SampleType::U8);
    let top = uid(&mut s, &doc, "Top");
    let image: Vec<u8> = (0..8 * 6).flat_map(|i| [i as u8, 2 * i as u8, 255 - i as u8, 255]).collect();
    let created =
        request(&mut s, "pixels.createLayer", json!({"doc": doc, "name": "Result", "above": top, "rect": rect(12, 9, 8, 6), "format": "rgba8"}), &image)
            .unwrap()
            .0;
    let new = created["uid"].as_str().unwrap().to_string();
    let back = request(&mut s, "pixels.read", json!({"doc": doc, "source": {"kind": "layer", "uid": new}, "rect": rect(12, 9, 8, 6), "format": "rgba8"}), &[])
        .unwrap()
        .1;
    assert_eq!(back, image);
    let doc_state = &s.documents()[0].doc;
    assert_eq!(doc_state.layers.last().map(|l| l.name.as_str()), Some("Result"), "placed above Top");
    assert!(s.undo());
    assert_eq!(s.documents()[0].doc.layers.len(), 2, "one undo removes it");
}

#[test]
fn a_write_is_one_undoable_step() {
    let (mut s, doc) = session_with(ColorMode::Rgb, SampleType::U8);
    let base = uid(&mut s, &doc, "Base");
    let before = layer_bytes(&s, "Base");
    request(&mut s, "pixels.write", json!({"doc": doc, "uid": base, "rect": rect(0, 0, 2, 2), "format": "rgba8"}), &[7u8; 16]).unwrap();
    assert_ne!(layer_bytes(&s, "Base"), before);
    assert!(s.undo());
    assert_eq!(layer_bytes(&s, "Base"), before);
}

#[test]
fn an_elliptical_selection_reads_exactly() {
    let (mut s, doc) = session_with(ColorMode::Rgb, SampleType::U8);
    let none = request(&mut s, "selection.read", json!({"doc": doc, "rect": rect(0, 0, W, H)}), &[]).unwrap();
    assert_eq!(none.0["none"], true);
    assert!(none.1.is_empty());
    assert_eq!(request(&mut s, "selection.bounds", json!({"doc": doc}), &[]).unwrap().0, json!({}));
    s.execute("select.rect", json!({"x": 4, "y": 3, "width": 25, "height": 17, "ellipse": true})).unwrap();
    let (result, mask) = request(&mut s, "selection.read", json!({"doc": doc, "rect": rect(0, 0, W, H)}), &[]).unwrap();
    assert_ne!(result["none"], true);
    let selection = s.documents()[0].doc.selection.clone().unwrap();
    assert_eq!(mask, selection.convert(PixelFormat::GRAY8).to_interleaved(Rect::from_xywh(0, 0, W, H)));
    assert!(mask.contains(&255) && mask.iter().any(|v| *v > 0 && *v < 255), "anti-aliased edge");
    let bounds = request(&mut s, "selection.bounds", json!({"doc": doc}), &[]).unwrap().0;
    assert_eq!(bounds["bounds"], rect(4, 3, 25, 17));
}

#[test]
fn a_selection_is_set_and_cleared() {
    let (mut s, doc) = session_with(ColorMode::Rgb, SampleType::U8);
    let coverage: Vec<u8> = (0..10 * 4).map(|i| (i * 6) as u8).collect();
    request(&mut s, "selection.set", json!({"doc": doc, "rect": rect(2, 2, 10, 4)}), &coverage).unwrap();
    let back = request(&mut s, "selection.read", json!({"doc": doc, "rect": rect(2, 2, 10, 4)}), &[]).unwrap().1;
    assert_eq!(back, coverage);
    request(&mut s, "selection.set", json!({"doc": doc, "clear": true}), &[]).unwrap();
    assert!(s.documents()[0].doc.selection.is_none());
    assert!(s.undo(), "clearing is undoable");
    assert!(s.documents()[0].doc.selection.is_some());
}

#[test]
fn bad_pixel_requests_are_refused() {
    let (mut s, doc) = session_with(ColorMode::Rgb, SampleType::U8);
    let base = uid(&mut s, &doc, "Base");
    let mut refused = |params: Value, payload: &[u8], method: &str| request(&mut s, method, params, payload).map(|_| ()).unwrap_err();
    let src = json!({"kind": "layer", "uid": base});
    assert_eq!(refused(json!({"doc": doc, "source": src, "rect": rect(0, 0, 4, 4), "format": "gray8"}), &[], "pixels.read"), ErrorCode::Invalid);
    assert_eq!(refused(json!({"doc": doc, "source": src, "rect": rect(0, 0, 0, 4), "format": "rgba8"}), &[], "pixels.read"), ErrorCode::Invalid);
    assert_eq!(refused(json!({"doc": doc, "source": src, "rect": rect(i64::MAX, 0, 4, 4), "format": "rgba8"}), &[], "pixels.read"), ErrorCode::Invalid);
    assert_eq!(refused(json!({"doc": doc, "source": src, "rect": rect(0, 0, 60000, 60000), "format": "rgba32f"}), &[], "pixels.read"), ErrorCode::TooLarge);
    assert_eq!(refused(json!({"doc": doc, "uid": base, "rect": rect(0, 0, 4, 4), "format": "rgba8"}), &[0; 3], "pixels.write"), ErrorCode::Invalid);
    let group = refused(json!({"doc": doc, "source": {"kind": "group", "uid": base}, "rect": rect(0, 0, 4, 4), "format": "rgba8"}), &[], "pixels.read");
    assert_eq!(group, ErrorCode::Invalid);
    request(&mut s, "layer.update", json!({"doc": doc, "uid": base, "locked": true}), &[]).unwrap();
    let locked = request(&mut s, "pixels.write", json!({"doc": doc, "uid": base, "rect": rect(0, 0, 1, 1), "format": "rgba8"}), &[0; 4]);
    assert_eq!(locked.map(|_| ()), Err(ErrorCode::Invalid));
}

/// Host-side cost of the transfers REQ-3.1 budgets: run with
/// `cargo test -p photocraft-engine --release -- --ignored pixel_transfer_timing --nocapture`.
#[test]
#[ignore = "timing on the reference machine, release build"]
fn pixel_transfer_timing() {
    let mut s = Session::new();
    let mut doc = Document::new("Big", Size::new(2048, 2048), ColorMode::Rgb, SampleType::U8);
    let fmt = doc.pixel_format();
    let mut base = Surface::new(fmt);
    base.fill_rect(Rect::from_xywh(0, 0, 2048, 2048), &[0.2, 0.4, 0.6, 1.0]);
    let mut paint = Surface::new(fmt);
    paint.fill_rect(Rect::from_xywh(100, 100, 1500, 1500), &[0.9, 0.1, 0.1, 0.5]);
    doc.layers = vec![Layer::new("Base", LayerContent::Raster(base)), Layer::new("Paint", LayerContent::Raster(paint))];
    let index = s.add_document(doc, None);
    let doc = s.documents()[index].doc.id.0.to_string();
    let paint = uid(&mut s, &doc, "Paint");
    let read_params = json!({"doc": doc, "source": {"kind": "composite"}, "rect": rect(0, 0, 2048, 2048), "format": "bgra8"});
    let image = vec![128u8; 1024 * 1024 * 4];
    let write_params = json!({"doc": doc, "uid": paint, "rect": rect(512, 512, 1024, 1024), "format": "bgra8"});
    let best = |s: &mut Session, method: &str, params: &Value, payload: &[u8]| {
        (0..5)
            .map(|_| {
                let start = std::time::Instant::now();
                request(s, method, params.clone(), payload).unwrap();
                start.elapsed()
            })
            .min()
            .unwrap()
    };
    let read = best(&mut s, "pixels.read", &read_params, &[]);
    let write = best(&mut s, "pixels.write", &write_params, &image);
    println!("2048x2048 composite read: {read:?}; 1024x1024 layer write: {write:?}");
    assert!(read.as_millis() <= 150 && write.as_millis() <= 150);
}
