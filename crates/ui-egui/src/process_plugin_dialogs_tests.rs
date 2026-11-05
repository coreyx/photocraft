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
    let open = json!({"dialogId": "settings", "title": "AI Settings", "modal": false, "tree": gallery()});
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
