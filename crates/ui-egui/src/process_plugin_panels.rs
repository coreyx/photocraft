//! Process plug-ins' docked panels: `panel.register`, `panel.patch` and `panel.unregister`, and
//! how their panels live in PhotoCraft's dock.
//!
//! A plug-in panel is a dock panel like the built-in ones, under the id `plugin:<plug-in
//! id>/<panel id>` ([`dock_id`]). It moves, tabs, floats, closes and comes back through the
//! Window menu like them, and its place is saved with the workspace:
//! - before the user rearranges anything, the dock shows every plug-in panel in one tab group
//!   at the bottom of the dock column (the plug-ins' group);
//! - in a rearranged workspace, a newly registered panel joins the other plug-in panels' group,
//!   or opens below the dock column's last group;
//! - a panel whose plug-in stopped keeps its place and says so, until the plug-in registers it
//!   again (after a restart) or the user closes it.
//!
//! Input is never lost to a plug-in that is busy: while it isn't responding its panels are
//! greyed and their input waits, at most [`MAX_QUEUED`] events per plug-in, newest kept.

use std::collections::{BTreeMap, HashMap, VecDeque};

use photocraft_process_plugin_protocol::event::{self, InputEvent};
use photocraft_process_plugin_protocol::frame::Frame;
use photocraft_process_plugin_protocol::host::PluginState;
use photocraft_process_plugin_protocol::message::{ErrorBody, ErrorCode, Header, decode_value};
use photocraft_process_plugin_protocol::method::{PanelRef, PatchPanelParams, RegisterPanelParams};
use serde_json::{Map, Value};

use crate::PhotocraftApp;
use crate::process_plugin_ui::{Owner, Tree};
use crate::theme::Tokens;

/// The prefix of plug-in panels' dock ids.
pub const PREFIX: &str = "plugin:";
/// Most input events kept for a plug-in that isn't responding.
pub const MAX_QUEUED: usize = 256;
/// Most panels one plug-in may register.
const MAX_PANELS: usize = 32;

/// A registered plug-in panel.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelInfo {
    pub owner: Owner,
    pub title: String,
}

/// Plug-in panels and the state their input depends on.
#[derive(Default)]
pub struct Panels {
    /// Registered panels by dock id, in registration order of their ids.
    pub registered: BTreeMap<String, PanelInfo>,
    /// Titles of panels seen this session, so a stopped plug-in's panel keeps its tab name.
    titles: HashMap<String, String>,
    /// Panels registered since the dock last placed new ones.
    pub(crate) fresh: Vec<String>,
    /// Each plug-in's last known state.
    pub(crate) states: HashMap<String, PluginState>,
    /// Input waiting for plug-ins that aren't responding.
    pub(crate) queued: HashMap<String, VecDeque<InputEvent>>,
}

/// The dock id of `plugin`'s panel `panel`.
pub fn dock_id(plugin: &str, panel: &str) -> String {
    format!("{PREFIX}{plugin}/{panel}")
}

/// Whether `id` names a plug-in panel (registered or not).
pub fn is_plugin_panel(id: &str) -> bool {
    id.strip_prefix(PREFIX).and_then(|rest| rest.split_once('/')).is_some_and(|(plugin, panel)| !plugin.is_empty() && valid_panel_id(panel))
}

/// A panel id: 1–64 ASCII letters, digits, `.`, `-` or `_`.
fn valid_panel_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn params<T: serde::de::DeserializeOwned>(method: &str, p: &Value) -> Result<T, ErrorBody> {
    decode_value(p, ErrorCode::Invalid).map_err(|e| ErrorBody::new(ErrorCode::Invalid, format!("{method}: {}", e.message)))
}

fn empty() -> Value {
    Value::Object(Map::new())
}

impl PhotocraftApp {
    /// `panel.register`, `panel.patch` and `panel.unregister`; `None` for other methods.
    pub(crate) fn serve_plugin_panel(&mut self, plugin: &str, method: &str, p: &Value) -> Option<Result<Value, ErrorBody>> {
        Some(match method {
            "panel.register" => self.register_plugin_panel(plugin, p),
            "panel.patch" => self.patch_plugin_panel(plugin, p),
            "panel.unregister" => self.unregister_plugin_panel(plugin, p),
            _ => return None,
        })
    }

    fn register_plugin_panel(&mut self, plugin: &str, p: &Value) -> Result<Value, ErrorBody> {
        let p: RegisterPanelParams = params("panel.register", p)?;
        if !valid_panel_id(&p.panel_id) {
            return Err(ErrorBody::new(ErrorCode::Invalid, "a panel id is 1 to 64 letters, digits, '.', '-' or '_'"));
        }
        let title: String = p.title.chars().take(64).collect();
        if title.trim().is_empty() {
            return Err(ErrorBody::new(ErrorCode::Invalid, "a panel needs a title"));
        }
        let id = dock_id(plugin, &p.panel_id);
        let panels = &mut self.process_plugin_ui.panels;
        if !panels.registered.contains_key(&id) && panels.registered.values().filter(|i| i.owner.plugin == plugin).count() >= MAX_PANELS {
            return Err(ErrorBody::new(ErrorCode::TooLarge, format!("at most {MAX_PANELS} panels per plug-in")));
        }
        let tree = Tree::new(p.tree).map_err(|e| ErrorBody::new(ErrorCode::Invalid, e))?;
        let owner = Owner { plugin: plugin.to_string(), id: p.panel_id };
        self.process_plugin_ui.trees.insert(owner.clone(), tree);
        let panels = &mut self.process_plugin_ui.panels;
        panels.titles.insert(id.clone(), title.clone());
        if panels.registered.insert(id.clone(), PanelInfo { owner, title }).is_none() {
            panels.fresh.push(id);
        }
        Ok(empty())
    }

    fn patch_plugin_panel(&mut self, plugin: &str, p: &Value) -> Result<Value, ErrorBody> {
        let p: PatchPanelParams = params("panel.patch", p)?;
        let owner = Owner { plugin: plugin.to_string(), id: p.panel_id.clone() };
        let tree = self.process_plugin_ui.trees.get_mut(&owner).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("no panel '{}'", p.panel_id)))?;
        // All the patches or none: a plug-in's half-applied update would show a tree it never sent.
        let mut next = tree.clone();
        for op in p.ops {
            next.apply(op).map_err(|e| ErrorBody::new(ErrorCode::Invalid, e))?;
        }
        *tree = next;
        Ok(empty())
    }

    fn unregister_plugin_panel(&mut self, plugin: &str, p: &Value) -> Result<Value, ErrorBody> {
        let p: PanelRef = params("panel.unregister", p)?;
        let id = dock_id(plugin, &p.panel_id);
        self.process_plugin_ui.panels.registered.remove(&id);
        self.process_plugin_ui.trees.remove(&Owner { plugin: plugin.to_string(), id: p.panel_id });
        // An unregistered panel leaves the dock; a stopped plug-in's panels keep their place.
        if let Some(workspace) = self.ui.docking.as_ref()
            && workspace.layout.contains(&id)
        {
            let _ = self.run("window.panel.close", serde_json::json!({"panel": id}));
        }
        Ok(empty())
    }

    /// A plug-in changed state: track whether its panels take input, and send what waited for
    /// it once it responds again.
    pub(crate) fn plugin_panels_state(&mut self, plugin: &str, state: &PluginState) {
        let panels = &mut self.process_plugin_ui.panels;
        panels.states.insert(plugin.to_string(), state.clone());
        if !state.is_live() {
            panels.registered.retain(|_, info| info.owner.plugin != plugin);
            panels.queued.remove(plugin);
            return;
        }
        if *state == PluginState::Running
            && let Some(waiting) = panels.queued.remove(plugin)
        {
            for event in waiting {
                self.send_panel_input(plugin, event);
            }
        }
    }

    fn plugin_responding(&self, plugin: &str) -> bool {
        !matches!(self.process_plugin_ui.panels.states.get(plugin), Some(PluginState::NotResponding))
    }

    /// Send `event` to `plugin`, or keep it while the plug-in isn't responding.
    fn send_panel_input(&mut self, plugin: &str, event: InputEvent) {
        if !self.plugin_responding(plugin) {
            let queue = self.process_plugin_ui.panels.queued.entry(plugin.to_string()).or_default();
            if queue.len() >= MAX_QUEUED {
                queue.pop_front();
            }
            queue.push_back(event);
            return;
        }
        let sent = Header::event(event::PANEL_INPUT, &event).map_err(|e| e.message).and_then(|header| match self.services.process_plugins.as_mut() {
            Some(host) => host.send(plugin, Frame::new(header)),
            None => Ok(()),
        });
        if let Err(e) = sent {
            log::debug!("plug-in '{plugin}': {e}");
        }
    }

    #[cfg(test)]
    pub(crate) fn send_panel_input_for_tests(&mut self, plugin: &str, value: usize) {
        let event = InputEvent {
            owner: "main".into(),
            node_id: "go".into(),
            kind: event::InputKind::Click,
            value: serde_json::json!(value),
            modifiers: Default::default(),
        };
        self.send_panel_input(plugin, event);
    }

    /// The tab name of plug-in panel `id`.
    pub(crate) fn plugin_panel_label(&self, id: &str) -> String {
        let panels = &self.process_plugin_ui.panels;
        panels.registered.get(id).map(|i| i.title.clone()).or_else(|| panels.titles.get(id).cloned()).unwrap_or_else(|| tl!("Plug-in panel").to_string())
    }
}

/// Draw plug-in panel `id`'s body: its widget tree, greyed while the plug-in isn't responding,
/// or a line saying the plug-in isn't running.
pub(crate) fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, id: &str) {
    let Some(info) = app.process_plugin_ui.panels.registered.get(id).cloned() else {
        let t = Tokens::get(ui.ctx());
        ui.add_space(8.0);
        ui.label(egui::RichText::new(tl!("This plug-in isn't running. Start it in Edit › Preferences › Plug-ins.")).color(t.text_dim));
        return;
    };
    let responding = app.plugin_responding(&info.owner.plugin);
    let ui_state = &mut app.process_plugin_ui;
    let Some(tree) = ui_state.trees.get_mut(&info.owner) else { return };
    let events = egui::ScrollArea::vertical()
        .id_salt(("plugin-panel", id))
        .auto_shrink([false, false])
        .show(ui, |ui| crate::process_plugin_ui::render::show(ui, &info.owner.plugin, &info.owner.id, tree, &mut ui_state.images, false))
        .inner;
    if !responding {
        // Greyed over, with what is happening: the input still reaches the plug-in later.
        let rect = ui.min_rect();
        let t = Tokens::get(ui.ctx());
        ui.painter().rect_filled(rect, 0.0, t.card.gamma_multiply(0.6));
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, tl!("Not responding…"), egui::FontId::proportional(13.0), t.text_dim);
    }
    for event in events {
        app.send_panel_input(&info.owner.plugin, event);
    }
}

/// The plug-in panels the native dock shows in its plug-ins' group, in order.
pub(crate) fn native_group(app: &PhotocraftApp) -> Vec<String> {
    app.process_plugin_ui.panels.registered.keys().cloned().collect()
}

/// Window menu rows for the registered plug-in panels, after the built-in panels.
pub(crate) fn insert_window_items(app: &PhotocraftApp, items: &mut Vec<crate::menus::MenuItem>) {
    let Some(last) = items.iter().rposition(|i| i.id.starts_with("window.panel.")) else { return };
    let path = items[last].path.clone();
    let extra: Vec<_> = app
        .process_plugin_ui
        .panels
        .registered
        .iter()
        .map(|(id, info)| crate::menus::MenuItem {
            id: format!("window.panel.{id}"),
            label: info.title.clone(),
            path: path.clone(),
            shortcut: None,
            enabled: true,
            checked: Some(crate::panel_docking::plugin_panel_shown(app, id)),
            color: None,
        })
        .collect();
    items.splice(last + 1..last + 1, extra);
}
