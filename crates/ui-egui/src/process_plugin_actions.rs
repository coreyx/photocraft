//! Process plug-ins' actions (`action.register`, `action.update`): commands like the built-in
//! ones, under the ids `pluginAction:<plug-in id>/<action id>` ([`command_id`]).
//!
//! A registered action is a row of Filter › Plug-ins › <plug-in name> (› its own submenu), so the
//! command palette and Edit › Keyboard Shortcuts, which list the menus' rows, list it too. Its
//! default shortcut and the user's own work like any command's: the user's are kept in the
//! shortcut preferences by command id, so they come back with the plug-in after a restart, and
//! the editor's conflict prompt covers them. Triggering one sends `action.triggered` with the
//! active document.

use std::collections::BTreeMap;

use photocraft_process_plugin_protocol::event::{self, ActionTriggered};
use photocraft_process_plugin_protocol::frame::Frame;
use photocraft_process_plugin_protocol::message::{ErrorBody, ErrorCode, Header, decode_value};
use photocraft_process_plugin_protocol::method::{RegisterActionParams, UpdateActionParams};
use serde_json::{Map, Value};

use crate::PhotocraftApp;

/// The prefix of plug-in actions' command ids.
pub const PREFIX: &str = "pluginAction:";
/// Most actions one plug-in may register.
const MAX_ACTIONS: usize = 200;

/// A registered plug-in action.
#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    pub plugin: String,
    pub id: String,
    pub label: String,
    /// The plug-in's default shortcut, in the host's notation.
    pub shortcut: Option<String>,
    /// Submenu path under the plug-in's menu (`"Generate/Advanced"`), if any.
    pub menu: Option<String>,
    pub enabled: bool,
    pub checked: Option<bool>,
}

/// The command id of `plugin`'s action `action`.
pub fn command_id(plugin: &str, action: &str) -> String {
    format!("{PREFIX}{plugin}/{action}")
}

/// An action id: 1–100 ASCII letters, digits, `.`, `-` or `_`.
fn valid_action_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn params<T: serde::de::DeserializeOwned>(method: &str, p: &Value) -> Result<T, ErrorBody> {
    decode_value(p, ErrorCode::Invalid).map_err(|e| ErrorBody::new(ErrorCode::Invalid, format!("{method}: {}", e.message)))
}

impl PhotocraftApp {
    /// `action.register` and `action.update`; `None` for other methods.
    pub(crate) fn serve_plugin_action(&mut self, plugin: &str, method: &str, p: &Value) -> Option<Result<Value, ErrorBody>> {
        Some(match method {
            "action.register" => self.register_plugin_action(plugin, p),
            "action.update" => self.update_plugin_action(plugin, p),
            _ => return None,
        })
    }

    fn register_plugin_action(&mut self, plugin: &str, p: &Value) -> Result<Value, ErrorBody> {
        let p: RegisterActionParams = params("action.register", p)?;
        if !valid_action_id(&p.id) {
            return Err(ErrorBody::new(ErrorCode::Invalid, "an action id is 1 to 100 letters, digits, '.', '-' or '_'"));
        }
        let label: String = p.label.chars().take(100).collect();
        if label.trim().is_empty() {
            return Err(ErrorBody::new(ErrorCode::Invalid, "an action needs a label"));
        }
        // A default shortcut PhotoCraft can't read is dropped rather than refusing the action.
        let shortcut = p.shortcut.filter(|s| crate::shortcuts::parse(s).is_some());
        let menu = p.menu.map(|m| m.split('/').map(str::trim).filter(|s| !s.is_empty()).take(3).collect::<Vec<_>>().join("/")).filter(|m| !m.is_empty());
        let id = command_id(plugin, &p.id);
        let actions = &mut self.process_plugin_ui.actions;
        if !actions.contains_key(&id) && actions.values().filter(|a| a.plugin == plugin).count() >= MAX_ACTIONS {
            return Err(ErrorBody::new(ErrorCode::TooLarge, format!("at most {MAX_ACTIONS} actions per plug-in")));
        }
        let previous = actions.get(&id);
        let (enabled, checked) = previous.map_or((true, None), |a| (a.enabled, a.checked));
        actions.insert(id, Action { plugin: plugin.to_string(), id: p.id, label, shortcut, menu, enabled, checked });
        Ok(Value::Object(Map::new()))
    }

    fn update_plugin_action(&mut self, plugin: &str, p: &Value) -> Result<Value, ErrorBody> {
        let p: UpdateActionParams = params("action.update", p)?;
        let action = self
            .process_plugin_ui
            .actions
            .get_mut(&command_id(plugin, &p.id))
            .ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("no action '{}'", p.id)))?;
        if let Some(enabled) = p.enabled {
            action.enabled = enabled;
        }
        if let Some(checked) = p.checked {
            action.checked = Some(checked);
        }
        Ok(Value::Object(Map::new()))
    }

    /// Forget a stopped plug-in's actions; the user's shortcuts for them stay in the preferences.
    pub(crate) fn drop_plugin_actions(&mut self, plugin: &str) {
        self.process_plugin_ui.actions.retain(|_, a| a.plugin != plugin);
    }
}

/// Run plug-in action `id`: tell its plug-in, with the active document. `None` for other ids.
pub(crate) fn menu(app: &mut PhotocraftApp, id: &str, _params: &Value) -> Option<Result<Value, String>> {
    if !id.starts_with(PREFIX) {
        return None;
    }
    let Some(action) = app.process_plugin_ui.actions.get(id).cloned() else {
        return Some(Err("that plug-in action isn't available (is its plug-in running?)".into()));
    };
    if !action.enabled {
        return Some(Err(format!("{} is not available right now", action.label)));
    }
    let doc = app.session.active().map(|st| st.doc.id.0.to_string());
    let header = match Header::event(event::ACTION_TRIGGERED, &ActionTriggered { id: action.id.clone(), doc }) {
        Ok(header) => header,
        Err(e) => return Some(Err(e.message)),
    };
    app.reply_to_plugin(&action.plugin, Frame::new(header));
    Some(Ok(Value::Null))
}

/// Whether plug-in action `id` can run now; `None` for other ids.
pub(crate) fn is_enabled(app: &PhotocraftApp, id: &str) -> Option<bool> {
    id.starts_with(PREFIX).then(|| app.process_plugin_ui.actions.get(id).is_some_and(|a| a.enabled))
}

/// Whether plug-in action `id` shows a check mark; `None` when it has none or isn't one.
pub(crate) fn checked(app: &PhotocraftApp, id: &str) -> Option<bool> {
    app.process_plugin_ui.actions.get(id).and_then(|a| a.checked)
}

/// The registered plug-in actions with their default shortcuts, for the key bindings.
pub(crate) fn default_shortcuts(app: &PhotocraftApp) -> Vec<(String, Option<String>)> {
    app.process_plugin_ui.actions.iter().map(|(id, a)| (id.clone(), a.shortcut.clone())).collect()
}

/// Menu rows for the registered actions: Filter › Plug-ins › <plug-in name> (› submenu), after
/// the plug-ins already there.
pub(crate) fn insert_menu_items(app: &PhotocraftApp, items: &mut Vec<crate::menus::MenuItem>) {
    let actions = &app.process_plugin_ui.actions;
    if actions.is_empty() {
        return;
    }
    let base = vec!["Filter".to_string(), "Plug-ins".to_string()];
    let at = items.iter().rposition(|i| i.path.starts_with(&base)).map_or(items.len(), |i| i + 1);
    let mut by_plugin: BTreeMap<String, Vec<&Action>> = BTreeMap::new();
    for action in actions.values() {
        by_plugin.entry(app.process_plugin_name(&action.plugin)).or_default().push(action);
    }
    let rows: Vec<crate::menus::MenuItem> = by_plugin
        .into_iter()
        .flat_map(|(name, list)| {
            let base = base.clone();
            list.into_iter().map(move |a| {
                let mut path = base.clone();
                path.push(name.clone());
                path.extend(a.menu.iter().flat_map(|m| m.split('/').map(str::to_string)));
                let id = command_id(&a.plugin, &a.id);
                crate::menus::MenuItem {
                    shortcut: crate::shortcuts::effective_shortcut(app, &id, a.shortcut.as_deref()),
                    id,
                    label: a.label.clone(),
                    path,
                    enabled: a.enabled,
                    checked: a.checked,
                    color: None,
                }
            })
        })
        .collect();
    items.splice(at..at, rows);
}
