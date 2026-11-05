//! The shell's side of process plug-ins (`Services::process_plugins`).
//!
//! Once per frame the shell starts the enabled plug-ins (the first time), then drains what they
//! sent; at the end of the frame it sends subscribed plug-ins the change events for the frame's
//! edits (`Session::plugin_events`). Each request is answered by the session's broker (`Session::plugin_request`): a result,
//! or a typed error such as `unsupported` for a method the host doesn't serve, so a plug-in never
//! waits on a request. Plug-ins are launched and
//! supervised outside the UI thread (`photocraft-process-plugin-host` on the desktop); the web
//! build has no process plug-ins and leaves the service unset.
//!
//! Preferences › Plug-ins lists the installed plug-ins with their state and lets the user
//! restart, turn off or remove them, and install a package. Turning one off is remembered in
//! `plugIns.disabledProcessPlugins`.

use std::sync::Arc;

use egui::RichText;
use photocraft_process_plugin_protocol::frame::Frame;
use photocraft_process_plugin_protocol::host::{HostEvent, PluginState, PluginStatus, StartSettings};
use photocraft_process_plugin_protocol::message::Header;
use serde_json::{Map, Value, json};

use crate::file_dialog::{FileDialogAnswer, FileDialogRequest};
use crate::theme::Tokens;
use crate::{PhotocraftApp, i18n, notices};

impl PhotocraftApp {
    /// Start the plug-ins on the first frame, then handle everything they sent since last frame.
    pub(crate) fn pump_process_plugins(&mut self, ctx: &egui::Context) {
        if self.services.process_plugins.is_none() {
            return;
        }
        self.tell_plugins_the_locale();
        if !self.process_plugins_started {
            self.process_plugins_started = true;
            let repaint = ctx.clone();
            let settings = StartSettings {
                locale: i18n::current().code().to_string(),
                dark_theme: Tokens::get(ctx).dark(),
                disabled: self.session.prefs().plug_ins.disabled_process_plugins.clone(),
                waker: Arc::new(move || repaint.request_repaint()),
            };
            if let Some(host) = self.services.process_plugins.as_mut() {
                host.start(settings);
            }
        }
        let events = self.services.process_plugins.as_mut().map(|host| host.drain()).unwrap_or_default();
        for event in events {
            match event {
                HostEvent::State { plugin, state } => self.process_plugin_state_change(&plugin, &state),
                HostEvent::Frame { plugin, frame } => {
                    if let Some(reply) = self.serve_plugin_frame(ctx, &plugin, frame)
                        && let Some(host) = self.services.process_plugins.as_mut()
                        && let Err(e) = host.send(&plugin, reply)
                    {
                        log::debug!("plug-in '{plugin}': {e}");
                    }
                }
            }
        }
    }

    /// Send each subscribed plug-in the change events for this frame's edits, by the user and by
    /// plug-ins alike. Called once per frame, after the frame's UI pass made its edits, so a change
    /// reaches plug-ins in the frame it happened in.
    pub(crate) fn publish_process_plugin_events(&mut self) {
        let Some(host) = self.services.process_plugins.as_mut() else { return };
        for event in self.session.plugin_events() {
            let sent = Header::event(event.topic, &event.data).map_err(|e| e.message).and_then(|header| host.send(&event.plugin, Frame::new(header)));
            if let Err(e) = sent {
                log::debug!("plug-in '{}': {e}", event.plugin);
            }
        }
    }

    /// Ask the plug-ins to exit with the application.
    pub(crate) fn shutdown_process_plugins(&mut self) {
        if let Some(host) = self.services.process_plugins.as_mut() {
            host.shutdown();
        }
    }

    /// A plug-in changed state: when it is no longer running, whatever it left half-done (an
    /// open undo group, its progress indicators) ends with it.
    fn process_plugin_state_change(&mut self, plugin: &str, state: &PluginState) {
        self.plugin_panels_state(plugin, state);
        if !state.is_live() && *state != PluginState::Starting {
            self.session.plugin_disconnected(plugin);
            self.clear_plugin_progress(plugin);
        }
        self.process_plugin_state(plugin, state);
    }

    #[cfg(test)]
    pub(crate) fn process_plugin_state_change_for_tests(&mut self, plugin: &str, state: PluginState) {
        self.process_plugin_state_change(plugin, &state);
    }

    /// Log a plug-in's state change, and tell the user when one stops unexpectedly.
    fn process_plugin_state(&mut self, plugin: &str, state: &PluginState) {
        match state {
            PluginState::Crashed { reason } => {
                log::warn!("plug-in '{plugin}' stopped unexpectedly: {reason}");
                let name = self.process_plugin_name(plugin);
                let title = i18n::fmt(tl!("The plug-in {name} stopped"), &[("name", name.as_str())]);
                let hint = tl!("Restart it from Edit › Preferences › Plug-ins. Your documents are not affected.").to_string();
                notices::post(self, title, vec![reason.clone(), hint], true, None);
            }
            PluginState::Incompatible { reason } => log::warn!("plug-in '{plugin}' can't run: {reason}"),
            other => log::info!("plug-in '{plugin}': {other:?}"),
        }
    }

    pub(crate) fn process_plugin_name(&self, plugin: &str) -> String {
        let statuses = self.services.process_plugins.as_ref().map(|h| h.statuses()).unwrap_or_default();
        statuses.into_iter().find(|s| s.id == plugin).map_or_else(|| plugin.to_string(), |s| s.name)
    }

    /// The installed plug-ins as JSON for the Preferences dialog (`null` without the service).
    pub(crate) fn process_plugin_statuses(&self) -> Value {
        self.services.process_plugins.as_ref().map_or(Value::Null, |host| json!(host.statuses()))
    }

    /// Carry out what the user chose in the plug-in list: `{"action", "id"}`. `form` is the
    /// Preferences dialog's working copy, kept in step so OK doesn't undo the change.
    pub(crate) fn process_plugin_action(&mut self, action: &Value, form: &mut Map<String, Value>) {
        let kind = action.get("action").and_then(Value::as_str).unwrap_or_default();
        let id = action.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        if kind == "install" {
            self.install_process_plugin();
            return;
        }
        let Some(host) = self.services.process_plugins.as_mut() else { return };
        let result = match kind {
            "restart" => host.restart(&id),
            "enable" => host.set_enabled(&id, true),
            "disable" => host.set_enabled(&id, false),
            "remove" => host.remove(&id),
            _ => Ok(()),
        };
        if matches!(kind, "enable" | "disable" | "remove") {
            let off = kind == "disable";
            self.session.edit_prefs(|p| set_disabled(&mut p.plug_ins.disabled_process_plugins, &id, off));
            if let Some(list) = form.get_mut("values").and_then(|v| v.pointer_mut("/plugIns/disabledProcessPlugins")).and_then(Value::as_array_mut) {
                let mut ids: Vec<String> = list.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
                set_disabled(&mut ids, &id, off);
                *list = ids.into_iter().map(Value::String).collect();
            }
        }
        if let Err(e) = result {
            notices::error(self, e);
        }
    }

    /// Install Plug-in Package…: pick a `.zip` and install it.
    fn install_process_plugin(&mut self) {
        let request = FileDialogRequest::Open { multiple: false, initial_dir: None, extensions: Some(vec!["zip".into()]) };
        let asked = self.ask_file(request, |app, answer| {
            let FileDialogAnswer::Paths(paths) = answer else {
                return Err(tl!("Plug-in packages can only be installed from a file on disk.").to_string());
            };
            let Some(path) = paths.first() else { return Ok(Value::Null) };
            let host = app.services.process_plugins.as_mut().ok_or("process plug-ins aren't available")?;
            match host.install(path) {
                Ok(id) => {
                    let name = app.process_plugin_name(&id);
                    let title = i18n::fmt(tl!("Installed {name}"), &[("name", name.as_str())]);
                    notices::post(app, title, Vec::new(), false, None);
                    Ok(json!({ "installed": id }))
                }
                Err(e) => Err(e),
            }
        });
        if let Err(e) = asked {
            log::debug!("install plug-in: {e}");
        }
    }
}

/// Add or remove `id` in a disabled list, keeping it sorted and free of duplicates.
fn set_disabled(list: &mut Vec<String>, id: &str, disabled: bool) {
    list.retain(|x| x != id);
    if disabled {
        list.push(id.to_string());
        list.sort();
    }
}

impl PhotocraftApp {
    /// Answer a plug-in's request: the shell's own services (`process_plugin_services`), else
    /// document requests through the session's broker, which replies with a result (and payload)
    /// or a typed error. Responses and events need no answer.
    fn serve_plugin_frame(&mut self, ctx: &egui::Context, plugin: &str, frame: Frame) -> Option<Frame> {
        let Header::Request { id, method, params } = frame.header else { return None };
        if let Some(answer) = self.serve_plugin_service(ctx, plugin, &method, &params, &frame.payload) {
            return Some(match answer {
                Ok(result) => Frame::new(Header::Response { id, result }),
                Err(error) => Frame::new(Header::Error { id, error }),
            });
        }
        Some(match self.session.plugin_request(plugin, &method, &params, &frame.payload) {
            Ok((result, payload)) => Frame::with_payload(Header::Response { id, result }, payload),
            Err(error) => Frame::new(Header::Error { id, error }),
        })
    }
}

/// A plug-in's state for the list, and whether it is a problem.
fn state_text(state: &PluginState) -> (String, bool) {
    match state {
        PluginState::Running => (tl!("Running").to_string(), false),
        PluginState::Starting => (tl!("Starting…").to_string(), false),
        PluginState::NotResponding => (tl!("Not responding").to_string(), true),
        PluginState::Stopped => (tl!("Stopped").to_string(), false),
        PluginState::Disabled => (tl!("Off").to_string(), false),
        PluginState::Crashed { reason } => (format!("{} ({reason})", tl!("Stopped unexpectedly")), true),
        PluginState::Incompatible { reason } => (format!("{} ({reason})", tl!("Can't run")), true),
    }
}

/// Preferences › Plug-ins: the process plug-ins, from `form["__processPlugins"]`. A button sets
/// `form["__processPluginAction"]`, which the dialog's owner carries out after drawing.
pub(crate) fn prefs_rows(ui: &mut egui::Ui, form: &mut Map<String, Value>) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(10.0);
    ui.label(RichText::new(tl!("Process Plug-ins")).font(crate::theme::semibold(13.0)).color(t.text));
    let Some(list) = form.get("__processPlugins").filter(|v| !v.is_null()).cloned() else {
        ui.label(RichText::new(tl!("Process plug-ins aren't available in this version of PhotoCraft.")).color(t.text_faint));
        return;
    };
    let statuses: Vec<PluginStatus> = serde_json::from_value(list).unwrap_or_default();
    if statuses.is_empty() {
        ui.label(RichText::new(tl!("No process plug-ins are installed.")).color(t.text_faint));
    }
    let confirm = form.get("__confirmRemove").and_then(Value::as_str).map(str::to_string);
    let mut chosen: Option<Value> = None;
    for status in &statuses {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let title = if status.version.is_empty() { status.name.clone() } else { format!("{} {}", status.name, status.version) };
            ui.label(RichText::new(title).color(t.text));
            let (text, problem) = state_text(&status.state);
            ui.label(RichText::new(text).color(if problem { t.danger } else { t.text_dim }));
        });
        if !status.license.is_empty() || !status.description.is_empty() {
            let detail = [status.description.as_str(), status.license.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · ");
            ui.label(RichText::new(detail).color(t.text_faint));
        }
        ui.horizontal(|ui| {
            let incompatible = matches!(status.state, PluginState::Incompatible { .. });
            if status.enabled && !incompatible && crate::widgets::secondary_button(ui, tl!("Restart"), 80.0).clicked() {
                chosen = Some(json!({"action": "restart", "id": status.id}));
            }
            if !incompatible {
                let (label, action) = if status.enabled { (tl!("Turn Off"), "disable") } else { (tl!("Turn On"), "enable") };
                if crate::widgets::secondary_button(ui, label, 80.0).clicked() {
                    chosen = Some(json!({"action": action, "id": status.id}));
                }
            }
            let confirming = confirm.as_deref() == Some(status.id.as_str());
            let label = if confirming { tl!("Really Remove") } else { tl!("Remove…") };
            if crate::widgets::secondary_button(ui, label, 110.0).clicked() {
                if confirming {
                    chosen = Some(json!({"action": "remove", "id": status.id}));
                } else {
                    form.insert("__confirmRemove".into(), json!(status.id));
                }
            }
        });
    }
    ui.add_space(6.0);
    if crate::widgets::secondary_button(ui, tl!("Install Plug-in Package…"), 190.0).clicked() {
        chosen = Some(json!({"action": "install"}));
    }
    if let Some(action) = chosen {
        form.remove("__confirmRemove");
        form.insert("__processPluginAction".into(), action);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_lists_stay_sorted_and_unique() {
        let mut list = vec!["b".to_string()];
        set_disabled(&mut list, "a", true);
        set_disabled(&mut list, "a", true);
        assert_eq!(list, ["a", "b"]);
        set_disabled(&mut list, "b", false);
        assert_eq!(list, ["a"]);
    }

    #[test]
    fn problem_states_are_flagged() {
        assert!(!state_text(&PluginState::Running).1);
        assert!(state_text(&PluginState::Crashed { reason: "x".into() }).1);
        assert!(state_text(&PluginState::Incompatible { reason: "y".into() }).0.contains('y'));
    }
}

#[cfg(test)]
mod ui_tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use photocraft_process_plugin_protocol::host::{PluginState, PluginStatus};
    use serde_json::{Map, Value, json};

    pub(super) fn status(id: &str, enabled: bool, state: PluginState) -> PluginStatus {
        PluginStatus {
            id: id.into(),
            name: id.into(),
            version: "1".into(),
            license: "MIT".into(),
            description: String::new(),
            folder: String::new(),
            enabled,
            state,
        }
    }

    /// A harness drawing the plug-in rows with PhotoCraft's fonts and theme installed.
    fn harness(height: f32, form: Map<String, Value>) -> Harness<'static, Map<String, Value>> {
        let mut h = Harness::builder().with_size(egui::vec2(700.0, height)).build_ui_state(
            |ui, form| {
                // The first frame runs before the fonts below are installed; draw from the next.
                if ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                    super::prefs_rows(ui, form);
                }
            },
            form,
        );
        crate::PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        h.run_steps(2);
        h
    }

    fn form(statuses: Vec<PluginStatus>) -> Map<String, Value> {
        let mut form = Map::new();
        form.insert("__processPlugins".into(), json!(statuses));
        form
    }

    #[test]
    fn plugin_rows_offer_the_actions_that_fit_each_state() {
        let statuses = vec![
            status("on", true, PluginState::Running),
            status("off", false, PluginState::Disabled),
            status("bad", true, PluginState::Incompatible { reason: "r".into() }),
        ];
        let mut h = harness(500.0, form(statuses));
        // Restart and Turn Off for the running one, Turn On for the one that's off, nothing but
        // Remove for the incompatible one.
        assert_eq!(h.query_all_by_label("Restart").count(), 1);
        assert_eq!(h.query_all_by_label("Turn Off").count(), 1);
        assert_eq!(h.query_all_by_label("Turn On").count(), 1);
        assert_eq!(h.query_all_by_label("Remove…").count(), 3);
        h.get_by_label("Turn On").click();
        h.run_steps(2);
        assert_eq!(h.state()["__processPluginAction"], json!({"action": "enable", "id": "off"}));
    }

    #[test]
    fn removing_asks_for_a_second_click() {
        let mut h = harness(300.0, form(vec![status("p", true, PluginState::Running)]));
        h.get_by_label("Remove…").click();
        h.run_steps(2);
        assert!(h.state().get("__processPluginAction").is_none());
        h.get_by_label("Really Remove").click();
        h.run_steps(2);
        assert_eq!(h.state()["__processPluginAction"], json!({"action": "remove", "id": "p"}));
    }

    #[test]
    fn without_the_service_the_page_says_so() {
        let h = harness(200.0, Map::new());
        h.get_by_label("Process plug-ins aren't available in this version of PhotoCraft.");
    }
}

#[cfg(test)]
mod pump_tests {
    use std::sync::{Arc, Mutex};

    use photocraft_process_plugin_protocol::frame::Frame;
    use photocraft_process_plugin_protocol::host::{HostEvent, PluginState, PluginStatus, ProcessPluginHost, StartSettings};
    use photocraft_process_plugin_protocol::message::{ErrorCode, Header};
    use serde_json::json;

    use crate::{PhotocraftApp, Services};

    /// A host that delivers queued events and records what the shell sends back.
    struct FakeHost {
        inbox: Vec<HostEvent>,
        sent: Arc<Mutex<Vec<(String, Frame)>>>,
        started: Arc<Mutex<Option<Vec<String>>>>,
        running: Vec<PluginStatus>,
    }

    impl ProcessPluginHost for FakeHost {
        fn start(&mut self, settings: StartSettings) {
            *self.started.lock().unwrap() = Some(settings.disabled);
        }
        fn statuses(&self) -> Vec<PluginStatus> {
            self.running.clone()
        }
        fn drain(&mut self) -> Vec<HostEvent> {
            std::mem::take(&mut self.inbox)
        }
        fn send(&mut self, plugin: &str, frame: Frame) -> Result<(), String> {
            self.sent.lock().unwrap().push((plugin.to_string(), frame));
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

    fn request(id: u64, method: &str, params: serde_json::Value) -> HostEvent {
        HostEvent::Frame { plugin: "org.example.p".into(), frame: Frame::new(Header::Request { id, method: method.into(), params }) }
    }

    #[test]
    fn requests_are_answered_by_the_session_and_plugins_start_with_the_disabled_list() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let started = Arc::new(Mutex::new(None));
        let host = FakeHost {
            inbox: vec![request(7, "doc.list", serde_json::Value::Null), request(8, "layer.teleport", json!({}))],
            sent: sent.clone(),
            started: started.clone(),
            running: Vec::new(),
        };
        let services = Services { process_plugins: Some(Box::new(host)), ..Default::default() };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.session.edit_prefs(|p| p.plug_ins.disabled_process_plugins = vec!["org.example.off".into()]);
        app.pump_process_plugins(&egui::Context::default());
        assert_eq!(started.lock().unwrap().clone(), Some(vec!["org.example.off".to_string()]));
        let sent = sent.lock().unwrap();
        assert!(matches!(&sent[0].1.header, Header::Response { id: 7, result } if result.is_array()));
        assert!(matches!(&sent[1].1.header, Header::Error { id: 8, error } if error.code == ErrorCode::Unsupported));
    }

    #[test]
    fn subscribed_plugins_get_the_frames_change_events() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let host = FakeHost {
            inbox: vec![request(1, "events.subscribe", json!({"topics": ["selection.changed"]}))],
            sent: sent.clone(),
            started: Arc::new(Mutex::new(None)),
            running: Vec::new(),
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services { process_plugins: Some(Box::new(host)), ..Default::default() });
        let doc = photocraft_doc::Document::new("d", photocraft_geom::Size::new(4, 4), photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::U8);
        app.session.add_document(doc, None);
        app.pump_process_plugins(&egui::Context::default());
        app.session.execute("select.all", json!({})).unwrap();
        app.publish_process_plugin_events();
        let sent = sent.lock().unwrap();
        assert!(matches!(&sent[0].1.header, Header::Response { id: 1, .. }));
        assert!(matches!(&sent[1].1.header, Header::Event { topic, data } if topic == "selection.changed" && data["bounds"]["width"] == 4));
        assert_eq!(sent.len(), 2);
    }

    /// The frames the shell sent to plug-ins, with the plug-in each went to.
    type Sent = Arc<Mutex<Vec<(String, Frame)>>>;

    /// What the services stubs saw: opened URLs and paths, and the clipboard's text.
    #[derive(Default)]
    struct Platform {
        opened: Vec<String>,
        clipboard: String,
    }

    /// An app with one document, whose plug-ins send `inbox`, and whose platform services
    /// record into the returned `Platform`.
    fn app_with(inbox: Vec<HostEvent>) -> (PhotocraftApp, Sent, Arc<Mutex<Platform>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let platform = Arc::new(Mutex::new(Platform::default()));
        let running = vec![super::ui_tests::status("org.example.p", true, PluginState::Running)];
        let host = FakeHost { inbox, sent: sent.clone(), started: Arc::new(Mutex::new(None)), running };
        let (a, b, c) = (platform.clone(), platform.clone(), platform.clone());
        let services = Services {
            process_plugins: Some(Box::new(host)),
            open_url: Some(Box::new(move |url: &str| {
                a.lock().unwrap().opened.push(url.to_string());
                Ok(())
            })),
            clipboard_get_text: Some(Box::new(move || Ok(Some(b.lock().unwrap().clipboard.clone())))),
            clipboard_set_text: Some(Box::new(move |text: &str| {
                c.lock().unwrap().clipboard = text.to_string();
                Ok(())
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.run("file.new", json!({"width": 32, "height": 32})).unwrap();
        (app, sent, platform)
    }

    fn answer(sent: &[(String, Frame)], id: u64) -> &Header {
        &sent.iter().find(|(_, f)| matches!(&f.header, Header::Response { id: i, .. } | Header::Error { id: i, .. } if *i == id)).unwrap().1.header
    }

    fn refused(sent: &[(String, Frame)], id: u64) -> Option<ErrorCode> {
        match answer(sent, id) {
            Header::Error { error, .. } => Some(error.code),
            _ => None,
        }
    }

    #[test]
    fn services_answer_plugins_like_built_in_features() {
        let folder = std::env::temp_dir().to_string_lossy().into_owned();
        let (mut app, sent, platform) = app_with(vec![
            request(1, "notice.show", json!({"level": "error", "text": "The server is unreachable."})),
            request(2, "open.url", json!({"url": "https://example.com/docs"})),
            request(3, "open.url", json!({"url": "javascript:alert(1)"})),
            request(4, "open.path", json!({"path": folder})),
            request(5, "open.path", json!({"path": "relative/folder"})),
            request(6, "clipboard.set", json!({"text": "a fox, golden hour"})),
            request(7, "clipboard.get", json!({})),
            request(8, "notice.show", json!({"level": "info", "text": "   "})),
        ]);
        app.pump_process_plugins(&egui::Context::default());
        let sent = sent.lock().unwrap();
        let notice = app.ui.notices.last().unwrap();
        assert!(notice.error && notice.lines == ["The server is unreachable."]);
        assert_eq!(refused(&sent, 3), Some(ErrorCode::Invalid), "only web and mail links");
        assert_eq!(refused(&sent, 5), Some(ErrorCode::NotFound), "only absolute paths that exist");
        assert_eq!(refused(&sent, 8), Some(ErrorCode::Invalid), "a notice needs text");
        assert_eq!(platform.lock().unwrap().opened, ["https://example.com/docs".to_string(), folder]);
        assert!(matches!(answer(&sent, 7), Header::Response { result, .. } if result["text"] == "a fox, golden hour"));
        for id in [1, 2, 4, 6] {
            assert!(refused(&sent, id).is_none(), "request {id} succeeded");
        }
    }

    #[test]
    fn progress_shows_cancels_and_never_blocks_painting() {
        let set = json!({"taskId": "gen-1", "fraction": 0.25, "message": "Generating", "cancellable": true});
        let (mut app, sent, _) = app_with(vec![request(1, "progress.set", set)]);
        app.pump_process_plugins(&egui::Context::default());
        assert_eq!(app.process_plugin_progress.len(), 1);
        assert_eq!(app.process_plugin_progress[0].fraction, Some(0.25));
        // The user keeps painting on the document meanwhile: no job holds it.
        assert!(!app.session.has_jobs());
        let steps = app.session.active().unwrap().history.entries().len();
        app.run("edit.fill", json!({"color": "#336699"})).unwrap();
        assert_eq!(app.session.active().unwrap().history.entries().len(), steps + 1);
        // Cancel asks the plug-in to stop and hides the button until it clears the task.
        app.cancel_plugin_task("org.example.p", "gen-1");
        assert!(!app.process_plugin_progress[0].cancellable);
        let cancel = sent.lock().unwrap().iter().any(|(p, f)| {
            p == "org.example.p" && matches!(&f.header, Header::Event { topic, data } if topic == "progress.cancel" && data["taskId"] == "gen-1")
        });
        assert!(cancel, "progress.cancel sent");
        app.process_plugin_state_change_for_tests("org.example.p", PluginState::Crashed { reason: "x".into() });
        assert!(app.process_plugin_progress.is_empty(), "a stopped plug-in's progress goes");
    }

    #[test]
    fn progress_is_cleared_by_the_plugin() {
        let (mut app, _, _) = app_with(vec![
            request(1, "progress.set", json!({"taskId": "a", "message": "Downloading"})),
            request(2, "progress.set", json!({"taskId": "b", "fraction": 7.0, "message": "Uploading"})),
            request(3, "progress.clear", json!({"taskId": "a"})),
        ]);
        app.pump_process_plugins(&egui::Context::default());
        assert_eq!(app.process_plugin_progress.len(), 1);
        assert_eq!(app.process_plugin_progress[0].fraction, Some(1.0), "clamped");
    }

    #[test]
    fn running_plugins_hear_about_a_language_change() {
        let (mut app, sent, _) = app_with(Vec::new());
        crate::i18n::set_current(crate::i18n::Lang::from_pref("en"));
        app.pump_process_plugins(&egui::Context::default());
        assert!(sent.lock().unwrap().is_empty(), "the welcome told them the first language");
        crate::i18n::set_current(crate::i18n::Lang::from_pref("de"));
        app.pump_process_plugins(&egui::Context::default());
        crate::i18n::set_current(crate::i18n::Lang::from_pref("en"));
        let sent = sent.lock().unwrap();
        assert!(matches!(&sent[0].1.header, Header::Event { topic, data } if topic == "locale.changed" && data["locale"] == "de"));
    }

    #[test]
    fn a_stopped_plugin_leaves_no_undo_group_open() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let host = FakeHost { inbox: Vec::new(), sent, started: Arc::new(Mutex::new(None)), running: Vec::new() };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services { process_plugins: Some(Box::new(host)), ..Default::default() });
        let doc = photocraft_doc::Document::new("d", photocraft_geom::Size::new(4, 4), photocraft_color::ColorMode::Rgb, photocraft_color::SampleType::U8);
        app.session.add_document(doc, None);
        let id = app.session.documents()[0].doc.id.0.to_string();
        let plugin = "org.example.p";
        app.session.plugin_request(plugin, "undo.begin", &json!({"doc": id, "label": "Group"}), &[]).unwrap();
        app.session.plugin_request(plugin, "layer.create", &json!({"doc": id, "kind": "pixel", "name": "A"}), &[]).unwrap();
        // The plug-in crashes; its next edit after a restart is a step of its own.
        app.process_plugin_state_change_for_tests(plugin, PluginState::Crashed { reason: "x".into() });
        app.session.plugin_request(plugin, "layer.create", &json!({"doc": id, "kind": "pixel", "name": "B"}), &[]).unwrap();
        assert_eq!(app.session.documents()[0].history.undo_label(), Some("New Layer"));
    }
}
