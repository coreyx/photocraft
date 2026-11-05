//! The host side's view of its process plug-ins: status types and the [`ProcessPluginHost`]
//! interface.
//!
//! The UI (`photocraft-ui-egui`) has to show plug-ins, route their messages and restart them, but
//! it must build for the browser, where processes don't exist. So it only knows this interface;
//! the desktop app hands it an implementation (`photocraft-process-plugin-host`) that launches
//! and supervises the plug-in processes. Liveness (the handshake, pings, crash detection) is
//! handled by that implementation's own threads and never depends on the UI thread.

use serde::{Deserialize, Serialize};

use crate::frame::Frame;

/// Where a plug-in is in its life.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum PluginState {
    /// Installed and compatible, but turned off by the user.
    Disabled,
    /// Installed but can't run here; `reason` says why (wrong protocol version, bad manifest).
    Incompatible { reason: String },
    /// Launched, waiting for its `hello`.
    Starting,
    /// Handshake done; exchanging messages.
    Running,
    /// Running but missed several pings; its panels show "not responding".
    NotResponding,
    /// Not running: stopped by the user, by PhotoCraft quitting, or never started.
    Stopped,
    /// Ended unexpectedly; `reason` describes how (exit code, broken pipe, bad frame, timeout).
    Crashed { reason: String },
}

impl PluginState {
    /// Whether the plug-in process is up and talking (possibly slowly).
    pub fn is_live(&self) -> bool {
        matches!(self, PluginState::Running | PluginState::NotResponding)
    }
}

/// What the host knows about one installed plug-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginStatus {
    pub id: String,
    pub name: String,
    pub version: String,
    pub license: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Folder the plug-in is installed in.
    pub folder: String,
    /// Whether the user wants it to run.
    pub enabled: bool,
    pub state: PluginState,
}

/// Something the host's plug-in supervisor has for the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    /// A frame from a running plug-in that the UI must handle: a request to serve, a response to
    /// a request the UI sent, or an event. Handshake and ping traffic never shows up here.
    Frame { plugin: String, frame: Frame },
    /// A plug-in changed state (started, crashed, stopped ...).
    State { plugin: String, state: PluginState },
}

/// Called (from any thread) whenever the host queues something for the UI, so an idle UI wakes
/// up to [`ProcessPluginHost::drain`] it instead of polling.
pub type Waker = std::sync::Arc<dyn Fn() + Send + Sync>;

/// What the UI hands the host when it starts the plug-ins.
#[derive(Clone)]
pub struct StartSettings {
    /// The UI language, passed to plug-ins in `welcome`.
    pub locale: String,
    /// Whether the UI theme is dark, passed to plug-ins in `welcome`.
    pub dark_theme: bool,
    /// Plug-ins the user turned off (from preferences), by id.
    pub disabled: Vec<String>,
    /// Called whenever events arrive for the UI.
    pub waker: Waker,
}

/// The UI's handle on its process plug-ins.
pub trait ProcessPluginHost {
    /// Launch the enabled plug-ins. Called once, when the UI is ready.
    fn start(&mut self, settings: StartSettings);

    /// Every installed plug-in, in a stable order.
    fn statuses(&self) -> Vec<PluginStatus>;

    /// Take everything that arrived since the last call. Cheap when there is nothing; call it once
    /// per UI frame.
    fn drain(&mut self) -> Vec<HostEvent>;

    /// Send a frame (usually a response or an event) to a running plug-in.
    fn send(&mut self, plugin: &str, frame: Frame) -> Result<(), String>;

    /// A fresh id for a request the UI sends to a plug-in.
    fn next_request_id(&mut self) -> u64;

    /// Stop the plug-in if it runs, then start it again.
    fn restart(&mut self, plugin: &str) -> Result<(), String>;

    /// Turn a plug-in on (starting it) or off (stopping it).
    fn set_enabled(&mut self, plugin: &str, enabled: bool) -> Result<(), String>;

    /// Stop a plug-in and delete its installed folder (not its data folder).
    fn remove(&mut self, plugin: &str) -> Result<(), String>;

    /// Install a plug-in package (a `.zip` with a manifest at its root or in one top-level
    /// folder), replacing an installed copy with the same id, and start it if enabled. Returns
    /// the plug-in id.
    fn install(&mut self, package: &str) -> Result<String, String>;

    /// Install a newer package of `plugin`, at its own request (`plugin.update`): as
    /// [`install`](Self::install), but refused unless the package's manifest is `plugin`'s, so
    /// a plug-in can update itself and nothing else.
    fn update(&mut self, plugin: &str, package: &str) -> Result<(), String> {
        let _ = (plugin, package);
        Err("this PhotoCraft can't update plug-ins".into())
    }

    /// Look for newly installed or removed plug-ins and start the newly found enabled ones.
    fn rescan(&mut self);

    /// Ask every running plug-in to exit, wait briefly, then end any that remain.
    fn shutdown(&mut self);
}
