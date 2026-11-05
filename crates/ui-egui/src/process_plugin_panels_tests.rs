//! Tests for process plug-ins' docked panels: registration, the dock and Window menu, moving and
//! closing, a plug-in that stops and comes back, and one that stops responding.

use std::sync::{Arc, Mutex};

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use photocraft_process_plugin_protocol::frame::Frame;
use photocraft_process_plugin_protocol::host::{HostEvent, PluginState, PluginStatus, ProcessPluginHost, StartSettings};
use photocraft_process_plugin_protocol::message::{ErrorCode, Header};
use photocraft_process_plugin_protocol::widget::{Node, WidgetKind};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::process_plugin_panels::{MAX_QUEUED, dock_id};

const P: &str = "org.example.p";

/// A host that records what the shell sends.
struct Recorder(Arc<Mutex<Vec<(String, Frame)>>>);

impl ProcessPluginHost for Recorder {
    fn start(&mut self, _: StartSettings) {}
    fn statuses(&self) -> Vec<PluginStatus> {
        Vec::new()
    }
    fn drain(&mut self) -> Vec<HostEvent> {
        Vec::new()
    }
    fn send(&mut self, plugin: &str, frame: Frame) -> Result<(), String> {
        self.0.lock().unwrap().push((plugin.to_string(), frame));
        Ok(())
    }
    fn next_request_id(&mut self) -> u64 {
        1
    }
    fn restart(&mut self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn set_enabled(&mut self, _: &str, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn remove(&mut self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn install(&mut self, _: &str) -> Result<String, String> {
        Err("no".into())
    }
    fn rescan(&mut self) {}
    fn shutdown(&mut self) {}
}

type Sent = Arc<Mutex<Vec<(String, Frame)>>>;

fn app() -> (PhotocraftApp, Sent) {
    let sent: Sent = Arc::default();
    let services = crate::Services { process_plugins: Some(Box::new(Recorder(sent.clone()))), ..Default::default() };
    (PhotocraftApp::new(photocraft_engine::Session::new(), services), sent)
}

fn tree(button: &str) -> Value {
    serde_json::to_value(Node::new("root", WidgetKind::Column).child(Node::new("go", WidgetKind::Button).prop("text", button))).unwrap()
}

fn register(app: &mut PhotocraftApp, panel: &str, title: &str) -> Result<Value, ErrorCode> {
    app.serve_plugin_panel(P, "panel.register", &json!({"panelId": panel, "title": title, "tree": tree("Generate")})).unwrap().map_err(|e| e.code)
}

fn layout(app: &PhotocraftApp) -> craft_ui::docking::Layout<String> {
    app.ui.docking.as_ref().expect("a rearranged workspace").layout.clone()
}

fn inputs(sent: &Sent) -> Vec<Value> {
    sent.lock()
        .unwrap()
        .iter()
        .filter_map(|(_, f)| match &f.header {
            Header::Event { topic, data } if topic == "panel.input" => Some(data.clone()),
            _ => None,
        })
        .collect()
}

fn window_item(app: &PhotocraftApp, id: &str) -> Option<crate::menus::MenuItem> {
    crate::menus::menu_items(app).into_iter().find(|i| i.id == format!("window.panel.{id}"))
}

#[test]
fn a_registered_panel_is_docked_drawn_and_sends_input() {
    let (mut app, sent) = app();
    register(&mut app, "main", "AI Image").unwrap();
    let mut h = Harness::builder().with_size(vec2(1440.0, 1000.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app
    });
    h.run_steps(8);
    // The native dock lays it out in the plug-ins' group, under its title.
    assert!(crate::panel_docking::probe::tab_rect(&h.ctx, &dock_id(P, "main")).is_some(), "the panel's tab is drawn");
    h.get_by_label("Generate").click();
    h.run_steps(2);
    let input = inputs(&sent);
    assert_eq!(input.len(), 1);
    assert_eq!((input[0]["owner"].as_str(), input[0]["nodeId"].as_str(), input[0]["kind"].as_str()), (Some("main"), Some("go"), Some("click")));
}

#[test]
fn the_window_menu_lists_closes_and_reopens_plugin_panels() {
    let (mut app, _) = app();
    register(&mut app, "main", "AI Image").unwrap();
    let id = dock_id(P, "main");
    let item = window_item(&app, &id).expect("a Window menu item");
    assert_eq!((item.label.as_str(), item.checked), ("AI Image", Some(true)));
    app.run(&format!("window.panel.{id}"), json!({"show": false})).unwrap();
    assert!(!layout(&app).contains(&id), "closed");
    assert_eq!(window_item(&app, &id).and_then(|i| i.checked), Some(false));
    // A plug-in registering it again (after a restart) doesn't bring back what the user closed.
    register(&mut app, "main", "AI Image").unwrap();
    crate::panel_docking::place_new_plugin_panels_for_tests(&mut app);
    assert!(!layout(&app).contains(&id));
    app.run(&format!("window.panel.{id}"), json!({})).unwrap();
    assert!(layout(&app).contains(&id), "shown again");
}

#[test]
fn plugin_panels_float_dock_and_tab_like_built_in_ones() {
    let (mut app, _) = app();
    register(&mut app, "main", "AI Image").unwrap();
    let id = dock_id(P, "main");
    app.run("window.panel.float", json!({"panel": id})).unwrap();
    assert!(layout(&app).floating.iter().any(|w| w.contains(&id)));
    app.run("window.panel.dock", json!({"panel": id})).unwrap();
    assert!(!layout(&app).floating.iter().any(|w| w.contains(&id)));
    app.run("window.panel.move", json!({"panel": id, "anchor": "layers"})).unwrap();
    assert!(crate::panel_docking::probe::group_of(&app, &id).contains(&"layers".to_string()), "tabbed with Layers");
    // A second panel of a rearranged workspace joins the plug-in panel's group.
    register(&mut app, "history", "AI History").unwrap();
    crate::panel_docking::place_new_plugin_panels_for_tests(&mut app);
    assert!(crate::panel_docking::probe::group_of(&app, &dock_id(P, "history")).contains(&id));
    // And the arrangement is saved with the workspace.
    let saved = serde_json::to_value(&app.ui).unwrap();
    let restored: crate::state::UiState = serde_json::from_value(saved).unwrap();
    assert_eq!(restored.docking, app.ui.docking);
}

#[test]
fn a_stopped_plugins_panels_keep_their_place_until_it_comes_back() {
    let (mut app, _) = app();
    register(&mut app, "main", "AI Image").unwrap();
    let id = dock_id(P, "main");
    app.run("window.panel.move", json!({"panel": id, "anchor": "layers"})).unwrap();
    let arranged = layout(&app);
    app.process_plugin_state_change_for_tests(P, PluginState::Crashed { reason: "x".into() });
    assert_eq!(layout(&app), arranged, "the panel stays where the user put it");
    assert!(!app.process_plugin_ui.panels.registered.contains_key(&id));
    assert_eq!(app.plugin_panel_label(&id), "AI Image", "its tab keeps its name");
    app.process_plugin_state_change_for_tests(P, PluginState::Running);
    register(&mut app, "main", "AI Image").unwrap();
    crate::panel_docking::place_new_plugin_panels_for_tests(&mut app);
    assert_eq!(layout(&app), arranged, "the restarted plug-in's panel is in the same place, once");
}

#[test]
fn input_waits_for_a_plugin_that_is_not_responding() {
    let (mut app, sent) = app();
    register(&mut app, "main", "AI Image").unwrap();
    app.process_plugin_state_change_for_tests(P, PluginState::NotResponding);
    for i in 0..(MAX_QUEUED + 10) {
        app.send_panel_input_for_tests(P, i);
    }
    assert!(inputs(&sent).is_empty(), "nothing sent while it hangs");
    app.process_plugin_state_change_for_tests(P, PluginState::Running);
    let got = inputs(&sent);
    assert_eq!(got.len(), MAX_QUEUED, "at most {MAX_QUEUED} kept");
    assert_eq!(got[0]["value"], json!(10), "the newest kept, in order");
    assert_eq!(got.last().map(|v| v["value"].clone()), Some(json!(MAX_QUEUED + 9)));
}

#[test]
fn bad_registrations_and_patches_are_refused() {
    let (mut app, _) = app();
    assert_eq!(register(&mut app, "has space", "T"), Err(ErrorCode::Invalid));
    assert_eq!(register(&mut app, "main", "  "), Err(ErrorCode::Invalid));
    register(&mut app, "main", "AI Image").unwrap();
    let patch =
        |app: &mut PhotocraftApp, ops: Value| app.serve_plugin_panel(P, "panel.patch", &json!({"panelId": "main", "ops": ops})).unwrap().map_err(|e| e.code);
    let good = json!({"op": "set", "id": "go", "props": {"text": "Queue"}});
    let bad = json!({"op": "remove", "id": "nope"});
    assert_eq!(patch(&mut app, json!([good, bad])), Err(ErrorCode::Invalid));
    let owner = crate::process_plugin_ui::Owner { plugin: P.into(), id: "main".into() };
    let text = |app: &PhotocraftApp| app.process_plugin_ui.trees[&owner].find("go").and_then(|n| n.props.get("text").cloned());
    assert_eq!(text(&app), Some(json!("Generate")), "all patches or none");
    patch(&mut app, json!([good])).unwrap();
    assert_eq!(text(&app), Some(json!("Queue")));
    let other = app.serve_plugin_panel("org.example.other", "panel.patch", &json!({"panelId": "main", "ops": []})).unwrap().map_err(|e| e.code);
    assert_eq!(other, Err(ErrorCode::NotFound), "a plug-in reaches only its own panels");
    app.serve_plugin_panel(P, "panel.unregister", &json!({"panelId": "main"})).unwrap().unwrap();
    assert!(window_item(&app, &dock_id(P, "main")).is_none());
}
