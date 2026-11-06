//! Unit tests for the frame codec, message helpers and wire format.

use std::io::Cursor;

use serde_json::json;

use crate::event::{self, InputEvent, InputKind};
use crate::frame::{self, Frame, FrameError, Limits, PREFIX_LEN};
use crate::message::{self, ErrorCode, Header};
use crate::method::{self, Method};
use crate::types::{Modifiers, PixelFormat, Rect};
use crate::widget::{MAX_DEPTH, Node, Patch, WidgetKind};

fn request(id: u64) -> Header {
    Header::Request { id, method: "layer.tree".into(), params: json!({"doc": "d1"}) }
}

#[test]
fn frame_round_trips_through_bytes() {
    let original = Frame::with_payload(request(3), vec![1, 2, 3, 255]);
    let bytes = frame::encode(&original, Limits::default()).unwrap();
    assert_eq!(&bytes[0..4], b"PCPP");
    let (decoded, used) = frame::decode(&bytes, Limits::default()).unwrap();
    assert_eq!(decoded, original);
    assert_eq!(used, bytes.len());
}

#[test]
fn frames_round_trip_through_a_stream() {
    let frames = vec![Frame::new(request(1)), Frame::with_payload(request(2), vec![9; 1000]), Frame::new(request(3))];
    let mut buffer = Vec::new();
    for f in &frames {
        frame::write_frame(&mut buffer, f, Limits::default()).unwrap();
    }
    let mut cursor = Cursor::new(buffer);
    for expected in &frames {
        let got = frame::read_frame(&mut cursor, Limits::default()).unwrap().unwrap();
        assert_eq!(&got, expected);
    }
    assert!(frame::read_frame(&mut cursor, Limits::default()).unwrap().is_none(), "clean end of stream");
}

#[test]
fn stream_ending_inside_a_frame_is_truncated() {
    let bytes = frame::encode(&Frame::with_payload(request(1), vec![0; 64]), Limits::default()).unwrap();
    for cut in [1, PREFIX_LEN - 1, PREFIX_LEN + 3, bytes.len() - 1] {
        let mut cursor = Cursor::new(bytes[..cut].to_vec());
        assert!(matches!(frame::read_frame(&mut cursor, Limits::default()), Err(FrameError::Truncated)), "cut at {cut}");
        assert!(matches!(frame::decode(&bytes[..cut], Limits::default()), Err(FrameError::Truncated)), "cut at {cut}");
    }
}

#[test]
fn bad_magic_is_refused() {
    let mut bytes = frame::encode(&Frame::new(request(1)), Limits::default()).unwrap();
    bytes[0] = b'X';
    assert!(matches!(frame::decode(&bytes, Limits::default()), Err(FrameError::BadMagic(_))));
    assert!(matches!(frame::read_frame(&mut Cursor::new(bytes), Limits::default()), Err(FrameError::BadMagic(_))));
}

#[test]
fn oversized_lengths_are_refused_before_allocating() {
    let limits = Limits { max_header: 64, max_payload: 16 };
    // A prefix claiming a 4 GiB header and a huge payload, with no data behind it.
    let mut bytes = b"PCPP".to_vec();
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    assert!(matches!(frame::decode(&bytes, limits), Err(FrameError::HeaderTooLarge(_, 64))));
    let mut bytes = b"PCPP".to_vec();
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(frame::read_frame(&mut Cursor::new(bytes), limits), Err(FrameError::PayloadTooLarge(_, 16))));
}

#[test]
fn encoding_respects_limits() {
    let limits = Limits { max_header: 1 << 20, max_payload: 8 };
    let too_big = Frame::with_payload(request(1), vec![0; 9]);
    assert!(matches!(frame::encode(&too_big, limits), Err(FrameError::PayloadTooLarge(9, 8))));
    let limits = Limits { max_header: 10, max_payload: 8 };
    assert!(matches!(frame::encode(&Frame::new(request(1)), limits), Err(FrameError::HeaderTooLarge(_, 10))));
}

#[test]
fn malformed_header_json_is_an_error() {
    let mut bytes = b"PCPP".to_vec();
    let header = b"{not json";
    bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(header);
    assert!(matches!(frame::decode(&bytes, Limits::default()), Err(FrameError::InvalidHeader(_))));
    // Valid JSON of the wrong shape is refused too.
    let mut bytes = b"PCPP".to_vec();
    let header = br#"{"type":"teleport","id":1}"#;
    bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes());
    bytes.extend_from_slice(header);
    assert!(matches!(frame::decode(&bytes, Limits::default()), Err(FrameError::InvalidHeader(_))));
}

#[test]
fn headers_have_the_documented_wire_shape() {
    let header = Header::request::<method::LayerTree>(7, &method::DocRef { doc: "d1".into() }).unwrap();
    assert_eq!(serde_json::to_value(&header).unwrap(), json!({"type": "request", "id": 7, "method": "layer.tree", "params": {"doc": "d1"}}));
    let error = Header::error(7, ErrorCode::NotFound, "no layer");
    assert_eq!(serde_json::to_value(&error).unwrap(), json!({"type": "error", "id": 7, "error": {"code": "not_found", "message": "no layer"}}));
    let event = Header::event(event::SELECTION_CHANGED, &event::SelectionChanged { doc: "d1".into(), bounds: None }).unwrap();
    assert_eq!(serde_json::to_value(&event).unwrap(), json!({"type": "event", "topic": "selection.changed", "data": {"doc": "d1"}}));
    assert_eq!(header.id(), Some(7));
    assert_eq!(event.id(), None);
}

#[test]
fn typed_params_decode_and_reject_bad_input() {
    let value = json!({"doc": "d1", "source": {"kind": "layer", "uid": "u1"}, "rect": {"x": -4, "y": 0, "width": 8, "height": 2}, "format": "bgra8", "includePreview": true});
    let params = message::params::<method::PixelsRead>(&value).unwrap();
    assert_eq!(params.rect, Rect::new(-4, 0, 8, 2));
    assert_eq!(params.format, PixelFormat::Bgra8);
    assert!(params.include_preview);
    assert!(params.exclude.is_empty());
    let error = message::params::<method::PixelsRead>(&json!({"doc": "d1"})).unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);
    // `null` params mean "none", which methods without parameters accept.
    message::params::<method::DocList>(&serde_json::Value::Null).unwrap();
}

#[test]
fn placement_flattens_into_create_params() {
    let value = json!({"doc": "d", "kind": "group", "name": "Region 1", "parent": "p", "above": "a"});
    let params = message::params::<method::LayerCreate>(&value).unwrap();
    assert_eq!(params.placement.parent.as_deref(), Some("p"));
    assert_eq!(params.placement.above.as_deref(), Some("a"));
    assert_eq!(serde_json::to_value(&params).unwrap(), value);
}

#[test]
fn every_method_name_is_unique_and_dotted() {
    let mut names = method::ALL.to_vec();
    names.sort_unstable();
    let count = names.len();
    names.dedup();
    assert_eq!(names.len(), count, "duplicate method names");
    for name in method::ALL {
        assert!(*name == "hello" || *name == "ping" || *name == "shutdown" || name.contains('.'), "{name}");
    }
    assert!(method::ALL.contains(&method::PixelsRead::NAME));
}

#[test]
fn every_event_topic_is_unique() {
    let mut topics = event::ALL.to_vec();
    topics.sort_unstable();
    topics.dedup();
    assert_eq!(topics.len(), event::ALL.len());
}

#[test]
fn pixel_payload_sizes_are_exact_and_overflow_safe() {
    assert_eq!(PixelFormat::Bgra8.payload_len(4, 3), Some(48));
    assert_eq!(PixelFormat::Rgba16.payload_len(2, 2), Some(32));
    assert_eq!(PixelFormat::Rgba32f.payload_len(1, 1), Some(16));
    assert_eq!(PixelFormat::Gray8.payload_len(5, 5), Some(25));
    assert_eq!(PixelFormat::Rgba32f.payload_len(u32::MAX, u32::MAX), None);
    assert!(Rect::new(0, 0, 0, 5).is_empty());
}

#[test]
fn widget_trees_round_trip_and_validate() {
    let tree = Node::new("root", WidgetKind::Column)
        .child(Node::new("prompt", WidgetKind::TextEdit).prop("text", "a cat").prop("minLines", 2))
        .child(Node::new("go", WidgetKind::Button).prop("text", "Generate"));
    let value = serde_json::to_value(&tree).unwrap();
    assert_eq!(value["children"][0]["kind"], "textEdit");
    let back: Node = serde_json::from_value(value).unwrap();
    assert_eq!(back, tree);
    tree.validate().unwrap();

    let duplicate = Node::new("a", WidgetKind::Row).child(Node::new("a", WidgetKind::Label));
    assert!(duplicate.validate().unwrap_err().contains("duplicate"));

    let mut deep = Node::new("n0", WidgetKind::Column);
    for i in 1..=MAX_DEPTH {
        deep = Node::new(format!("n{i}"), WidgetKind::Column).child(deep);
    }
    assert!(deep.validate().unwrap_err().contains("deeper"));
}

#[test]
fn patches_have_the_documented_wire_shape() {
    let patch = Patch::Set { id: "go".into(), props: serde_json::from_value(json!({"enabled": false})).unwrap() };
    assert_eq!(serde_json::to_value(&patch).unwrap(), json!({"op": "set", "id": "go", "props": {"enabled": false}}));
    let insert: Patch = serde_json::from_value(json!({"op": "insert", "parent": "root", "index": 0, "node": {"id": "x", "kind": "label"}})).unwrap();
    assert!(matches!(insert, Patch::Insert { index: 0, .. }));
}

#[test]
fn input_events_default_their_modifiers() {
    let value = json!({"owner": "main", "nodeId": "go", "kind": "click", "value": 1});
    let input: InputEvent = serde_json::from_value(value).unwrap();
    assert_eq!(input.kind, InputKind::Click);
    assert_eq!(input.modifiers, Modifiers::default());
}

#[test]
fn typed_results_decode() {
    let welcome = json!({"hostVersion": "0.20.1", "protocol": 1, "locale": "ja", "dataDir": "d", "logDir": "l", "theme": {"dark": true}});
    let welcome = message::result::<method::Hello>(&welcome).unwrap();
    assert_eq!(welcome.locale, "ja");
    assert!(welcome.theme.dark);
    assert_eq!(crate::PROTOCOL_VERSION, 1);
}

#[test]
fn dialogs_may_give_their_size() {
    let plain = json!({"dialogId": "d", "title": "T", "modal": false, "tree": {"id": "root", "kind": "column"}});
    let params: method::OpenDialogParams = serde_json::from_value(plain.clone()).unwrap();
    assert_eq!((params.size(), params.min_size()), (None, None));
    assert_eq!(serde_json::to_value(&params).unwrap(), plain, "absent sizes stay absent on the wire");

    let mut sized = plain;
    sized["width"] = json!(1000);
    sized["height"] = json!(600);
    sized["minWidth"] = json!(960);
    sized["minHeight"] = json!(480);
    let params: method::OpenDialogParams = serde_json::from_value(sized.clone()).unwrap();
    assert_eq!((params.size(), params.min_size()), (Some((1000, 600)), Some((960, 480))));
    assert_eq!(serde_json::to_value(&params).unwrap(), sized);

    sized["height"] = json!(0);
    sized.as_object_mut().unwrap().remove("minHeight");
    let params: method::OpenDialogParams = serde_json::from_value(sized).unwrap();
    assert_eq!((params.size(), params.min_size()), (None, None), "0 or one side alone counts as not given");
}
