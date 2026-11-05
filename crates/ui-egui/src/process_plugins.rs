//! The shell's side of process plug-ins (`Services::process_plugins`).
//!
//! Once per frame the shell starts the enabled plug-ins (the first time), then drains what they
//! sent. Requests that nothing in the shell serves yet are answered with an `unsupported` error,
//! so a plug-in never waits on a request the host can't handle. Plug-ins are launched and
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
use photocraft_process_plugin_protocol::message::{ErrorCode, Header};
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
                HostEvent::State { plugin, state } => self.process_plugin_state(&plugin, &state),
                HostEvent::Frame { plugin, frame } => {
                    if let Some(reply) = unserved_reply(&frame.header)
                        && let Some(host) = self.services.process_plugins.as_mut()
                        && let Err(e) = host.send(&plugin, reply)
                    {
                        log::debug!("plug-in '{plugin}': {e}");
                    }
                }
            }
        }
    }

    /// Ask the plug-ins to exit with the application.
    pub(crate) fn shutdown_process_plugins(&mut self) {
        if let Some(host) = self.services.process_plugins.as_mut() {
            host.shutdown();
        }
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

    fn process_plugin_name(&self, plugin: &str) -> String {
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

/// The answer to a plug-in request nothing in the shell serves: `unsupported`. Responses and
/// events need no answer.
fn unserved_reply(header: &Header) -> Option<Frame> {
    match header {
        Header::Request { id, method, .. } => Some(Frame::new(Header::error(*id, ErrorCode::Unsupported, format!("PhotoCraft does not support '{method}'")))),
        _ => None,
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
    fn unserved_requests_get_unsupported_and_others_no_reply() {
        let request = Header::Request { id: 4, method: "layer.tree".into(), params: serde_json::Value::Null };
        let reply = unserved_reply(&request).unwrap();
        assert!(matches!(reply.header, Header::Error { id: 4, error } if error.code == ErrorCode::Unsupported && error.message.contains("layer.tree")));
        assert!(unserved_reply(&Header::Event { topic: "x".into(), data: serde_json::Value::Null }).is_none());
        assert!(unserved_reply(&Header::Response { id: 1, result: serde_json::Value::Null }).is_none());
    }

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

    fn status(id: &str, enabled: bool, state: PluginState) -> PluginStatus {
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
