//! Tests for process plug-ins' dialogs: tree dialogs (modeless and modal), message boxes
//! answered later with the button chosen, and file pickers answered with the path or a cancel.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use photocraft_process_plugin_protocol::frame::Frame;
use photocraft_process_plugin_protocol::message::{ErrorCode, Header};
use photocraft_process_plugin_protocol::widget::gallery;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::file_dialog::{FileDialogAnswer, FileDialogReply, FileDialogRequest};
use crate::process_plugin_panels_tests::Recorder;

const P: &str = "org.example.p";

type Sent = Arc<Mutex<Vec<(String, Frame)>>>;
type Pickers = Rc<RefCell<Vec<(FileDialogRequest, FileDialogReply)>>>;

fn app() -> (PhotocraftApp, Sent, Pickers) {
    let sent: Sent = Arc::default();
    let pickers: Pickers = Rc::default();
    let held = pickers.clone();
    let services = crate::Services {
        process_plugins: Some(Box::new(Recorder(sent.clone()))),
        file_dialog: Some(Box::new(move |request, _parent, reply| held.borrow_mut().push((request, reply)))),
        ..Default::default()
    };
    (PhotocraftApp::new(photocraft_engine::Session::new(), services), sent, pickers)
}

fn request(app: &mut PhotocraftApp, id: u64, method: &str, params: Value) -> Option<Header> {
    let frame = Frame::new(Header::Request { id, method: method.into(), params });
    app.serve_plugin_frame_for_tests(P, frame).map(|f| f.header)
}

/// What the shell sent the plug-in outside a request's own reply: later replies and events.
fn sent_headers(sent: &Sent) -> Vec<Header> {
    sent.lock().unwrap().iter().map(|(_, f)| f.header.clone()).collect()
}

fn harness(app: PhotocraftApp) -> Harness<'static, PhotocraftApp> {
    let mut h = Harness::builder().with_size(vec2(1000.0, 800.0)).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            crate::process_plugin_dialogs::show(app, ui.ctx());
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
    h.run_steps(4);
    h
}

#[test]
fn a_dialog_draws_its_tree_and_sends_input_and_its_closing() {
    let (mut app, sent, _) = app();
    let open = json!({"dialogId": "settings", "title": "Sample Settings", "modal": false, "tree": gallery()});
    assert!(matches!(request(&mut app, 1, "dialog.open", open), Some(Header::Response { id: 1, .. })));
    let mut h = harness(app);
    h.get_by_label("Generate").click();
    h.run_steps(2);
    let headers = sent_headers(&sent);
    let input = headers.iter().find_map(|h| match h {
        Header::Event { topic, data } if topic == "dialog.input" => Some(data.clone()),
        _ => None,
    });
    assert_eq!(input.map(|d| (d["owner"].clone(), d["nodeId"].clone())), Some((json!("settings"), json!("button"))));
    // The plug-in updates it, then the user closes it.
    let patch = json!({"dialogId": "settings", "ops": [{"op": "set", "id": "button", "props": {"text": "Start"}}]});
    request(h.state_mut(), 2, "dialog.patch", patch).unwrap();
    h.run_steps(2);
    assert!(h.query_by_label("Start").is_some());
    h.state_mut().close_plugin_dialog_for_tests(P, "settings");
    let closed =
        sent_headers(&sent).into_iter().any(|h| matches!(h, Header::Event { topic, data } if topic == "dialog.closed" && data["dialogId"] == "settings"));
    assert!(closed);
    assert!(h.state().process_plugin_ui.dialogs.open.is_empty());
}

#[test]
fn a_modal_dialog_closes_with_escape_and_the_plugin_can_close_it() {
    let (mut app, sent, _) = app();
    let tree = json!({"id": "root", "kind": "column", "children": [{"id": "l", "kind": "label", "props": {"text": "Downloading models"}}]});
    request(&mut app, 1, "dialog.open", json!({"dialogId": "dl", "title": "Download", "modal": true, "tree": tree.clone()})).unwrap();
    let mut h = harness(app);
    assert!(h.query_by_label("Downloading models").is_some());
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().process_plugin_ui.dialogs.open.is_empty(), "Escape closes it");
    assert!(sent_headers(&sent).into_iter().any(|h| matches!(h, Header::Event { topic, .. } if topic == "dialog.closed")));
    // Closed by the plug-in: no closing event.
    let before = sent_headers(&sent).len();
    request(h.state_mut(), 2, "dialog.open", json!({"dialogId": "dl", "title": "Download", "modal": true, "tree": tree})).unwrap();
    request(h.state_mut(), 3, "dialog.close", json!({"dialogId": "dl"})).unwrap();
    assert_eq!(sent_headers(&sent).len(), before);
}

#[test]
fn a_message_box_answers_with_the_button_chosen() {
    let (mut app, sent, _) = app();
    let ask = json!({"kind": "question", "title": "Discard results?", "text": "The generated images are lost.", "buttons": ["Keep", "Discard"]});
    assert!(request(&mut app, 7, "dialog.message", ask.clone()).is_none(), "no reply until the user chooses");
    let mut h = harness(app);
    assert!(sent_headers(&sent).is_empty());
    h.get_by_label("Discard").click();
    h.run_steps(2);
    let headers = sent_headers(&sent);
    assert!(matches!(&headers[0], Header::Response { id: 7, result } if result["button"] == "Discard"), "{headers:?}");
    // Closed without a button: no button in the answer.
    request(h.state_mut(), 8, "dialog.message", ask).unwrap_or(Header::Event { topic: String::new(), data: Value::Null });
    h.run_steps(2);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    let headers = sent_headers(&sent);
    assert!(matches!(&headers[1], Header::Response { id: 8, result } if result.get("button").is_none()), "{headers:?}");
}

#[test]
fn a_file_picker_answers_with_the_path_or_a_cancel() {
    let (mut app, sent, pickers) = app();
    let ctx = egui::Context::default();
    let ask = json!({"mode": "save", "title": "Save workflow", "filters": [{"name": "Workflows", "extensions": ["json"]}], "start": "workflow.json"});
    assert!(request(&mut app, 3, "dialog.file", ask.clone()).is_none());
    app.poll_file_dialog(&ctx, None);
    let (picker, reply) = pickers.borrow_mut().remove(0);
    assert_eq!(picker, FileDialogRequest::Save { suggested: "workflow.json".into() });
    // Only one picker at a time.
    let busy = request(&mut app, 4, "dialog.file", ask.clone());
    assert!(busy.is_none());
    assert!(matches!(&sent_headers(&sent)[0], Header::Error { id: 4, error } if error.code == ErrorCode::Busy));
    reply.send(Some(FileDialogAnswer::SaveTo("/work/workflow.json".into())));
    app.poll_file_dialog(&ctx, None);
    assert!(matches!(&sent_headers(&sent)[1], Header::Response { id: 3, result } if result["path"] == "/work/workflow.json"));
    let open = json!({"mode": "open", "filters": [{"name": "Images", "extensions": [".PNG", "webp"]}]});
    request(&mut app, 5, "dialog.file", open).unwrap_or(Header::Event { topic: String::new(), data: Value::Null });
    app.poll_file_dialog(&ctx, None);
    let (picker, reply) = pickers.borrow_mut().remove(0);
    assert!(matches!(picker, FileDialogRequest::Open { multiple: false, extensions: Some(e), .. } if e == ["png", "webp"]));
    reply.send(None);
    app.poll_file_dialog(&ctx, None);
    assert!(matches!(&sent_headers(&sent)[2], Header::Response { id: 5, result } if result.get("path").is_none()), "a cancel answers with no path");
}

#[test]
fn a_stopped_plugins_dialogs_go() {
    let (mut app, _, _) = app();
    request(&mut app, 1, "dialog.open", json!({"dialogId": "d", "title": "T", "tree": gallery()})).unwrap();
    request(&mut app, 2, "dialog.message", json!({"kind": "information", "title": "T", "text": "x", "buttons": []}));
    assert_eq!(app.process_plugin_ui.dialogs.messages[0].params.buttons, ["OK"], "a box without buttons gets OK");
    app.process_plugin_state_change_for_tests(P, photocraft_process_plugin_protocol::host::PluginState::Stopped);
    assert!(app.process_plugin_ui.dialogs.open.is_empty() && app.process_plugin_ui.dialogs.messages.is_empty());
}

// -- layout: stretch and sizes (spec plugin-dialog-layout-fix)

/// A synthetic node.
fn n(id: &str, kind: &str, props: Value, children: Vec<Value>) -> Value {
    json!({"id": id, "kind": kind, "props": props, "children": children})
}

fn button(id: &str, text: &str) -> Value {
    n(id, "button", json!({"text": text}), vec![])
}

fn label(id: &str, text: &str) -> Value {
    n(id, "label", json!({"text": text}), vec![])
}

fn grow_spacer(id: &str) -> Value {
    n(id, "spacer", json!({"grow": 1}), vec![])
}

/// A harness the size of a desktop window with `tree` opened as a dialog of `size`, after
/// `steps` frames.
fn sized_dialog(tree: Value, size: Option<(u32, u32)>, min: Option<(u32, u32)>, steps: usize) -> Harness<'static, PhotocraftApp> {
    let (mut app, _, _) = app();
    let mut open = json!({"dialogId": "layout", "title": "Layout", "modal": false, "tree": tree});
    if let Some((w, h)) = size {
        open["width"] = json!(w);
        open["height"] = json!(h);
    }
    if let Some((w, h)) = min {
        open["minWidth"] = json!(w);
        open["minHeight"] = json!(h);
    }
    request(&mut app, 1, "dialog.open", open).unwrap();
    let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            crate::process_plugin_dialogs::show(app, ui.ctx());
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
    h.run_steps(steps);
    h
}

/// The dialog window's rectangle: the one area that isn't the whole screen.
fn dialog_rect(h: &Harness<'static, PhotocraftApp>) -> egui::Rect {
    let screen = h.ctx.content_rect();
    h.ctx.memory(|m| m.layer_ids().filter_map(|l| m.area_rect(l.id)).find(|r| *r != screen)).expect("a dialog window")
}

fn rect_of(h: &Harness<'static, PhotocraftApp>, label: &str) -> egui::Rect {
    h.get_by_label(label).rect()
}

#[test]
fn items_after_a_grow_spacer_go_to_the_right_edge() {
    let row = n("row", "row", json!({}), vec![button("l", "Left"), grow_spacer("s"), button("r", "Right")]);
    let h = sized_dialog(row, Some((600, 300)), None, 6);
    let window = dialog_rect(&h);
    let (left, right) = (rect_of(&h, "Left"), rect_of(&h, "Right"));
    assert!(left.left() < window.left() + 60.0, "Left at the left: {left:?} in {window:?}");
    assert!(right.right() <= window.right() && right.right() > window.right() - 60.0, "Right at the right edge: {right:?} in {window:?}");
}

#[test]
fn items_between_two_grow_spacers_are_centred() {
    let row = n("row", "row", json!({}), vec![button("a", "A"), grow_spacer("s1"), label("m", "Middle"), grow_spacer("s2"), button("b", "B")]);
    let h = sized_dialog(row, Some((600, 300)), None, 6);
    let (window, middle) = (dialog_rect(&h), rect_of(&h, "Middle"));
    assert!((middle.center().x - window.center().x).abs() < 40.0, "centred: {middle:?} in {window:?}");
}

#[test]
fn a_growing_line_edit_leaves_room_for_the_button_after_it() {
    let field = n("url", "lineEdit", json!({"text": "127.0.0.1:8188", "grow": 1}), vec![]);
    let row = n("row", "row", json!({}), vec![field, button("c", "Connect")]);
    let h = sized_dialog(row, Some((600, 300)), None, 6);
    let (window, connect) = (dialog_rect(&h), rect_of(&h, "Connect"));
    assert!(connect.right() <= window.right(), "Connect inside: {connect:?} in {window:?}");
    assert!(connect.left() > window.left() + 300.0, "the field took the rest: {connect:?}");
}

#[test]
fn a_grow_spacer_in_a_scroll_area_adds_no_height() {
    let scroll = n("scroll", "scroll", json!({}), vec![n("col", "column", json!({}), vec![grow_spacer("s"), label("b", "Below")])]);
    let h = sized_dialog(n("root", "column", json!({}), vec![scroll]), Some((600, 300)), None, 6);
    let (window, below) = (dialog_rect(&h), rect_of(&h, "Below"));
    // At most the visible height (Qt's stretch fills a scroll area's viewport), never the
    // 10,000 points the spacer used to add.
    assert!(below.top() < window.bottom(), "Below starts in view: {below:?} in {window:?}");
}

#[test]
fn a_dialog_opens_at_its_size_and_minimum() {
    let content = n("root", "column", json!({}), vec![label("t", "Text")]);
    let h = sized_dialog(content.clone(), Some((700, 400)), None, 10);
    let size = dialog_rect(&h).size();
    assert!((size.x - 700.0).abs() < 1.0 && (size.y - 400.0).abs() < 1.0, "700×400: {size:?}");
    let h = sized_dialog(content.clone(), Some((300, 200)), Some((500, 350)), 10);
    let size = dialog_rect(&h).size();
    assert!(size.x >= 499.0 && size.y >= 349.0, "at least the minimum: {size:?}");
    let h = sized_dialog(content, None, None, 10);
    assert!(dialog_rect(&h).width() < 500.0, "without a size, the default");
}

#[test]
fn a_settings_page_keeps_its_size_and_shows_its_controls() {
    // The shape of Krita AI Diffusion's Connection page as the mirror sends it.
    let list = n("pages", "list", json!({"items": [{"id": "c", "text": "Connection"}, {"id": "s", "text": "Styles"}], "selected": ["c"]}), vec![]);
    let krita = n("krita", "row", json!({}), vec![label("k", "Detected setup"), grow_spacer("ks"), button("d", "Detect")]);
    let url = n("url-row", "row", json!({}), vec![n("url", "lineEdit", json!({"text": "127.0.0.1:8188", "grow": 1}), vec![]), button("c", "Connect")]);
    // The server panel ends with a stretch, as upstream's does, which must not push the bottom row out.
    let server = n("server", "column", json!({}), vec![label("u", "Server URL"), url, label("st", "Connected"), grow_spacer("ss")]);
    let content = n(
        "content",
        "column",
        json!({}),
        vec![krita, label("long", "A configuration found on this computer"), n("panels", "stack", json!({"grow": 1}), vec![server])],
    );
    let page = n("page", "column", json!({}), vec![label("h", "Server Configuration"), n("scroll", "scroll", json!({"grow": 1}), vec![content])]);
    let bottom = n(
        "bottom",
        "row",
        json!({}),
        vec![button("rd", "Restore Defaults"), grow_spacer("b1"), label("v", "Version 1.0"), grow_spacer("b2"), button("ok", "Ok")],
    );
    let column = n("main", "column", json!({"grow": 1}), vec![n("stack", "stack", json!({"grow": 1}), vec![page]), bottom]);
    let mut h = sized_dialog(n("root", "row", json!({}), vec![list, column]), Some((960, 480)), Some((960, 480)), 5);
    let first = dialog_rect(&h);
    h.run_steps(20);
    let window = dialog_rect(&h);
    assert_eq!(first, window, "the dialog keeps its size");
    assert!((window.width() - 960.0).abs() < 1.0 && (window.height() - 480.0).abs() < 1.0, "960×480: {window:?}");
    for name in ["Server URL", "Connect", "Connected"] {
        let r = rect_of(&h, name);
        assert!(window.contains_rect(r), "{name} visible: {r:?} in {window:?}");
    }
    let ok = rect_of(&h, "Ok");
    assert!(window.contains_rect(ok) && ok.left() > window.center().x, "Ok visible, at the right: {ok:?} in {window:?}");
    assert!(window.contains_rect(rect_of(&h, "Restore Defaults")), "Restore Defaults visible");
}

#[test]
fn a_slider_after_a_stretch_leaves_the_growing_label_room() {
    // The Diffusion page's setting row: an expanding description, a stretch, then a slider with
    // its number box. egui's slider takes all the width it's offered.
    let text = "Selection Feather. The border is expanded and blurred by a fraction of selection size";
    let description = n("d", "label", json!({"text": text, "wrap": true, "grow": 1}), vec![]);
    let slider = n("s", "sliderSpin", json!({"min": 0, "max": 25, "value": 10, "suffix": "%"}), vec![]);
    let row = n("row", "row", json!({}), vec![description, grow_spacer("st"), slider]);
    let mut h = sized_dialog(n("root", "column", json!({}), vec![row]), Some((960, 480)), None, 5);
    h.run_steps(20);
    let (window, label) = (dialog_rect(&h), rect_of(&h, text));
    assert!(label.width() > 250.0, "the description keeps a readable width: {label:?} in {window:?}");
}
