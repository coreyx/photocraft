//! Tests for process plug-ins' actions: fifteen registered actions in the menu and Keyboard
//! Shortcuts, triggered from either, a user shortcut that survives a restart, and the standard
//! conflict prompt.

use std::sync::{Arc, Mutex};

use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use photocraft_process_plugin_protocol::frame::Frame;
use photocraft_process_plugin_protocol::message::{ErrorCode, Header};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::process_plugin_actions::command_id;
use crate::process_plugin_panels_tests::Recorder;

const P: &str = "org.example.p";

type Sent = Arc<Mutex<Vec<(String, Frame)>>>;

fn app_with(session: photocraft_engine::Session) -> (PhotocraftApp, Sent) {
    let sent: Sent = Arc::default();
    let services = crate::Services { process_plugins: Some(Box::new(Recorder(sent.clone()))), ..Default::default() };
    let mut app = PhotocraftApp::new(session, services);
    app.run("file.new", json!({"width": 8, "height": 8})).unwrap();
    (app, sent)
}

fn register(app: &mut PhotocraftApp, params: Value) -> Result<Value, ErrorCode> {
    app.serve_plugin_action(P, "action.register", &params).unwrap().map_err(|e| e.code)
}

/// The fifteen actions Krita AI Diffusion registers, give or take: one with a default key, a
/// few in a submenu.
fn register_fifteen(app: &mut PhotocraftApp) {
    for i in 0..15 {
        let mut p = json!({"id": format!("action_{i}"), "label": format!("AI Action {i}")});
        if i == 0 {
            p["shortcut"] = json!("Cmd+Alt+Shift+F11");
        }
        if i >= 12 {
            p["menu"] = json!("Live");
        }
        register(app, p).unwrap();
    }
}

fn triggered(sent: &Sent) -> Vec<Value> {
    sent.lock()
        .unwrap()
        .iter()
        .filter_map(|(_, f)| match &f.header {
            Header::Event { topic, data } if topic == "action.triggered" => Some(data.clone()),
            _ => None,
        })
        .collect()
}

fn press(app: &mut PhotocraftApp, key: Key, modifiers: Modifiers) {
    let ctx = egui::Context::default();
    ctx.begin_pass(egui::RawInput { events: vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }], ..Default::default() });
    crate::shortcuts::handle(app, &ctx);
    ctx.end_pass().textures_delta.clear();
}

#[test]
fn fifteen_actions_are_in_the_menu_and_shortcuts_and_reach_the_plugin() {
    let (mut app, sent) = app_with(photocraft_engine::Session::new());
    register_fifteen(&mut app);
    let items = crate::menus::menu_items(&app);
    let rows: Vec<_> = items.iter().filter(|i| i.id.starts_with(crate::process_plugin_actions::PREFIX)).collect();
    assert_eq!(rows.len(), 15);
    assert!(rows.iter().all(|r| r.path.starts_with(&["Filter".to_string(), "Plug-ins".to_string(), P.to_string()])), "under the plug-in's menu");
    assert_eq!(rows.iter().filter(|r| r.path.last().map(String::as_str) == Some("Live")).count(), 3);
    assert_eq!(rows.iter().find(|r| r.id == command_id(P, "action_0")).and_then(|r| r.shortcut.clone()).as_deref(), Some("Cmd+Alt+Shift+F11"));
    let listed = crate::prefs_ui::shortcut_items(&app);
    assert_eq!(listed.iter().filter(|i| i.0.starts_with(crate::process_plugin_actions::PREFIX)).count(), 15, "all in Keyboard Shortcuts");
    let ctx = egui::Context::default();
    let doc = app.session.active().unwrap().doc.id.0.to_string();
    for i in 0..15 {
        crate::menus::invoke(&mut app, &ctx, &command_id(P, &format!("action_{i}")), json!({})).unwrap();
    }
    // From its default shortcut too.
    press(&mut app, Key::F11, Modifiers::COMMAND | Modifiers::ALT | Modifiers::SHIFT);
    let got = triggered(&sent);
    assert_eq!(got.len(), 16);
    assert_eq!(got[3], json!({"id": "action_3", "doc": doc}));
    assert_eq!(got[15]["id"], "action_0");
}

#[test]
fn disabled_and_checked_actions_show_their_state() {
    let (mut app, sent) = app_with(photocraft_engine::Session::new());
    register(&mut app, json!({"id": "live", "label": "Live Painting"})).unwrap();
    let id = command_id(P, "live");
    app.serve_plugin_action(P, "action.update", &json!({"id": "live", "enabled": false, "checked": true})).unwrap().unwrap();
    let row = crate::menus::menu_items(&app).into_iter().find(|i| i.id == id).unwrap();
    assert_eq!((row.enabled, row.checked), (false, Some(true)));
    assert!(crate::menus::invoke(&mut app, &egui::Context::default(), &id, json!({})).is_err());
    assert!(triggered(&sent).is_empty());
    let missing = app.serve_plugin_action(P, "action.update", &json!({"id": "nope", "enabled": true})).unwrap().map_err(|e| e.code);
    assert_eq!(missing, Err(ErrorCode::NotFound));
    assert_eq!(register(&mut app, json!({"id": "bad id", "label": "x"})), Err(ErrorCode::Invalid));
    // A stopped plug-in's actions leave the menus.
    app.process_plugin_state_change_for_tests(P, photocraft_process_plugin_protocol::host::PluginState::Stopped);
    assert!(!crate::menus::menu_items(&app).iter().any(|i| i.id == id));
}

#[test]
fn a_users_shortcut_works_after_a_restart() {
    let (mut app, sent) = app_with(photocraft_engine::Session::new());
    register_fifteen(&mut app);
    let id = command_id(P, "action_7");
    app.run("edit.keyboardShortcuts", json!({"set": {id.clone(): "F9"}, "allowUnknown": true, "removeConflicts": false})).unwrap();
    press(&mut app, Key::F9, Modifiers::NONE);
    assert_eq!(triggered(&sent).last().map(|v| v["id"].clone()), Some(json!("action_7")));
    // Restart: the preferences come back, and the plug-in registers its actions again.
    let saved = app.session.prefs_to_json();
    let mut session = photocraft_engine::Session::new();
    session.load_prefs_json(&saved).unwrap();
    let (mut restarted, sent) = app_with(session);
    press(&mut restarted, Key::F9, Modifiers::NONE);
    assert!(triggered(&sent).is_empty(), "nothing to run before the plug-in registers it");
    register_fifteen(&mut restarted);
    press(&mut restarted, Key::F9, Modifiers::NONE);
    assert_eq!(triggered(&sent).last().map(|v| v["id"].clone()), Some(json!("action_7")));
}

#[test]
fn assigning_a_key_in_use_shows_the_standard_conflict() {
    let session = photocraft_engine::Session::new();
    let mut h = Harness::builder().with_size(egui::vec2(1100.0, 760.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut app = PhotocraftApp::new(session, crate::Services::default());
        register_fifteen(&mut app);
        app
    });
    let ctx = h.ctx.clone();
    let id = command_id(P, "action_2");
    let dialog = crate::menus::invoke(h.state_mut(), &ctx, "edit.keyboardShortcuts", json!({})).unwrap()["dialog"].as_u64().unwrap();
    let f = &mut h.state_mut().ui.dialog_mut(dialog).unwrap().fields;
    f.insert("selected".into(), json!(id));
    f.insert("capture".into(), json!(true));
    h.run_steps(3);
    // ⌘C: Copy's key.
    h.event(Event::Key { key: Key::C, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND });
    h.run_steps(2);
    h.event(Event::Key { key: Key::C, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::COMMAND });
    h.run_steps(2);
    let conflict = h.state().ui.dialogs.iter().find(|d| d.id == dialog).and_then(|d| d.fields.get("conflict").cloned()).unwrap_or(Value::Null);
    assert_eq!(conflict["with"], json!("edit.copy"), "{conflict}");
    assert_eq!(conflict["command"], json!(id));
}
