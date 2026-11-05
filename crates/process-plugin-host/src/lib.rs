//! Discovers, launches and supervises PhotoCraft's process plug-ins (desktop only).
//!
//! A process plug-in is an installed folder with a manifest ([`manifest`]). [`Manager`] finds
//! them under the plug-ins folder, launches the enabled, compatible ones with piped stdio, and
//! supervises each through a [`link::Link`]. It implements the UI's
//! [`ProcessPluginHost`] interface, so the UI can list plug-ins, exchange frames with them and
//! restart or disable them without knowing about processes.
//!
//! Each plug-in is launched with these environment variables:
//!
//! | Variable | Value |
//! |---|---|
//! | `PHOTOCRAFT_PLUGIN_NONCE` | random value its `hello` must echo |
//! | `PHOTOCRAFT_PLUGIN_DIR` | its installed folder (also the working directory) |
//! | `PHOTOCRAFT_PLUGIN_DATA_DIR` | where it keeps settings and data (created for it) |
//! | `PHOTOCRAFT_PLUGIN_LOG_DIR` | where it may write logs (created for it) |
//! | `PHOTOCRAFT_LOCALE` | the UI language |
//! | `PHOTOCRAFT_HOST_VERSION` | the PhotoCraft version |
//!
//! Its stderr is appended to `<log dir>/<plugin id>.log`.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod install;
pub mod link;
mod logfile;
pub mod manifest;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};

use photocraft_process_plugin_protocol::PROTOCOL_VERSION;
use photocraft_process_plugin_protocol::frame::{Frame, Limits};
use photocraft_process_plugin_protocol::host::{HostEvent, PluginState, PluginStatus, ProcessPluginHost, StartSettings, Waker};
use photocraft_process_plugin_protocol::method::{Theme, Welcome};

pub use link::Timing;
pub use manifest::Manifest;

use link::{Link, LinkContext};

/// Where plug-ins live and what the host tells them.
#[derive(Debug, Clone)]
pub struct Config {
    /// Folder holding one subfolder per installed plug-in.
    pub plugins_dir: PathBuf,
    /// Each plug-in's data folder is `<data_root>/<plugin id>`.
    pub data_root: PathBuf,
    /// Plug-in stderr logs go here.
    pub log_dir: PathBuf,
    pub host_version: String,
    pub locale: String,
    pub dark_theme: bool,
    /// Plug-ins the user turned off, by id.
    pub disabled: BTreeSet<String>,
    pub timing: Timing,
    pub limits: Limits,
}

impl Config {
    /// A configuration with the default timing and limits and nothing disabled.
    pub fn new(plugins_dir: PathBuf, data_root: PathBuf, log_dir: PathBuf, host_version: impl Into<String>, locale: impl Into<String>) -> Self {
        Config {
            plugins_dir,
            data_root,
            log_dir,
            host_version: host_version.into(),
            locale: locale.into(),
            dark_theme: true,
            disabled: BTreeSet::new(),
            timing: Timing::default(),
            limits: Limits::default(),
        }
    }
}

/// One installed plug-in folder.
struct Slot {
    folder: PathBuf,
    manifest: Result<Manifest, String>,
    link: Option<Link>,
    /// The last state reported for a plug-in without a live link (stopped, crashed ...).
    resting: PluginState,
}

impl Slot {
    fn state(&self, enabled: bool) -> PluginState {
        match (&self.manifest, &self.link) {
            (Err(reason), _) => PluginState::Incompatible { reason: reason.clone() },
            (Ok(m), _) if m.protocol != PROTOCOL_VERSION => {
                PluginState::Incompatible { reason: format!("needs protocol {}, PhotoCraft speaks {PROTOCOL_VERSION}", m.protocol) }
            }
            (Ok(_), Some(link)) => link.state(),
            (Ok(_), None) if !enabled => PluginState::Disabled,
            (Ok(_), None) => self.resting.clone(),
        }
    }
}

/// Finds, launches and supervises process plug-ins.
pub struct Manager {
    config: Config,
    /// Keyed by plug-in id (or by folder name when the manifest can't be read).
    slots: BTreeMap<String, Slot>,
    events_tx: Sender<HostEvent>,
    events_rx: Receiver<HostEvent>,
    next_request: u64,
    waker: Waker,
}

impl Manager {
    /// Create a manager and look for installed plug-ins (nothing is launched yet).
    pub fn new(config: Config) -> Self {
        let (events_tx, events_rx) = mpsc::channel();
        let waker: Waker = std::sync::Arc::new(|| {});
        let mut manager = Manager { config, slots: BTreeMap::new(), events_tx, events_rx, next_request: 0, waker };
        manager.scan();
        manager
    }

    /// Launch every enabled, compatible plug-in that isn't running. Failures are reported as
    /// `Crashed` states, never as errors, so one broken plug-in doesn't stop the others.
    pub fn start_enabled(&mut self) {
        let keys: Vec<String> = self.slots.keys().cloned().collect();
        for key in keys {
            if self.config.disabled.contains(&key) {
                continue;
            }
            if let Err(e) = self.launch_one(&key) {
                log::warn!("plug-in '{key}': {e}");
            }
        }
    }

    /// The ids of the plug-ins the user turned off, for saving in preferences.
    pub fn disabled(&self) -> &BTreeSet<String> {
        &self.config.disabled
    }

    /// Re-read the plug-ins folder. Running plug-ins whose folder disappeared are stopped.
    fn scan(&mut self) {
        let mut found: BTreeMap<String, (PathBuf, Result<Manifest, String>)> = BTreeMap::new();
        if let Ok(entries) = std::fs::read_dir(&self.config.plugins_dir) {
            for entry in entries.flatten() {
                let folder = entry.path();
                if !folder.is_dir() {
                    continue;
                }
                let manifest = Manifest::load(&folder);
                let key = match &manifest {
                    Ok(m) => m.id.clone(),
                    Err(_) => format!("folder:{}", entry.file_name().to_string_lossy()),
                };
                if found.contains_key(&key) {
                    // Two folders claim one id: keep the first (sorted) and report the other.
                    let name = entry.file_name().to_string_lossy().to_string();
                    found.insert(format!("folder:{name}"), (folder, Err(format!("another installed plug-in already uses the id '{key}'"))));
                    continue;
                }
                found.insert(key, (folder, manifest));
            }
        }
        let gone: Vec<String> = self.slots.keys().filter(|k| !found.contains_key(*k)).cloned().collect();
        for key in gone {
            if let Some(slot) = self.slots.remove(&key)
                && let Some(link) = slot.link
            {
                link.kill();
            }
        }
        for (key, (folder, manifest)) in found {
            let slot =
                self.slots.entry(key).or_insert_with(|| Slot { folder: folder.clone(), manifest: manifest.clone(), link: None, resting: PluginState::Stopped });
            if slot.link.is_none() {
                slot.folder = folder;
                slot.manifest = manifest;
            }
        }
    }

    /// Launch one plug-in (no-op if it already runs).
    fn launch_one(&mut self, key: &str) -> Result<(), String> {
        let enabled = !self.config.disabled.contains(key);
        let slot = self.slots.get_mut(key).ok_or_else(|| format!("no plug-in '{key}' is installed"))?;
        if let Some(link) = &slot.link
            && (link.state().is_live() || link.state() == PluginState::Starting)
        {
            return Ok(());
        }
        let state = slot.state(enabled);
        if let PluginState::Incompatible { reason } = state {
            return Err(reason);
        }
        let Ok(manifest) = slot.manifest.clone() else {
            return Err("the plug-in's manifest is invalid".into());
        };
        match launch(&self.config, &manifest, &slot.folder, self.events_tx.clone(), self.waker.clone()) {
            Ok(link) => {
                slot.link = Some(link);
                Ok(())
            }
            Err(reason) => {
                slot.link = None;
                slot.resting = PluginState::Crashed { reason: reason.clone() };
                let _ = self.events_tx.send(HostEvent::State { plugin: key.to_string(), state: slot.resting.clone() });
                (self.waker)();
                Err(reason)
            }
        }
    }

    /// Stop one plug-in: ask it to exit, wait the grace period, then end it.
    fn stop(&mut self, key: &str) {
        if let Some(slot) = self.slots.get_mut(key)
            && let Some(link) = slot.link.take()
        {
            link.request_stop();
            link.wait_or_kill(self.config.timing.shutdown_grace);
            slot.resting = PluginState::Stopped;
        }
    }
}

impl ProcessPluginHost for Manager {
    fn start(&mut self, settings: StartSettings) {
        self.config.locale = settings.locale;
        self.config.dark_theme = settings.dark_theme;
        self.config.disabled.extend(settings.disabled);
        self.waker = settings.waker;
        self.start_enabled();
    }

    fn statuses(&self) -> Vec<PluginStatus> {
        self.slots
            .iter()
            .map(|(key, slot)| {
                let enabled = !self.config.disabled.contains(key);
                let (id, name, version, license, description) = match &slot.manifest {
                    Ok(m) => (m.id.clone(), m.name.clone(), m.version.clone(), m.license.clone(), m.description.clone()),
                    Err(_) => (
                        key.clone(),
                        slot.folder.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                        String::new(),
                        String::new(),
                        String::new(),
                    ),
                };
                PluginStatus { id, name, version, license, description, folder: slot.folder.display().to_string(), enabled, state: slot.state(enabled) }
            })
            .collect()
    }

    fn drain(&mut self) -> Vec<HostEvent> {
        self.events_rx.try_iter().collect()
    }

    fn send(&mut self, plugin: &str, frame: Frame) -> Result<(), String> {
        let link = self.slots.get(plugin).and_then(|s| s.link.as_ref()).ok_or_else(|| format!("plug-in '{plugin}' is not running"))?;
        link.send(frame)
    }

    fn next_request_id(&mut self) -> u64 {
        self.next_request = self.next_request.saturating_add(1);
        self.next_request
    }

    fn restart(&mut self, plugin: &str) -> Result<(), String> {
        self.stop(plugin);
        self.launch_one(plugin)
    }

    fn set_enabled(&mut self, plugin: &str, enabled: bool) -> Result<(), String> {
        if !self.slots.contains_key(plugin) {
            return Err(format!("no plug-in '{plugin}' is installed"));
        }
        if enabled {
            self.config.disabled.remove(plugin);
            self.launch_one(plugin)
        } else {
            self.config.disabled.insert(plugin.to_string());
            self.stop(plugin);
            let _ = self.events_tx.send(HostEvent::State { plugin: plugin.to_string(), state: PluginState::Disabled });
            Ok(())
        }
    }

    fn remove(&mut self, plugin: &str) -> Result<(), String> {
        self.stop(plugin);
        let slot = self.slots.remove(plugin).ok_or_else(|| format!("no plug-in '{plugin}' is installed"))?;
        // Only ever delete a folder directly inside the plug-ins folder.
        if slot.folder.parent() != Some(self.config.plugins_dir.as_path()) {
            return Err("the plug-in folder is outside the plug-ins folder".into());
        }
        std::fs::remove_dir_all(&slot.folder).map_err(|e| format!("could not remove {}: {e}", slot.folder.display()))
    }

    fn install(&mut self, package: &str) -> Result<String, String> {
        let plugins_dir = self.config.plugins_dir.clone();
        let staged = install::stage(Path::new(package), &plugins_dir)?;
        let id = staged.manifest.id.clone();
        // The installed copy's files may be in use: stop it before replacing them.
        self.stop(&id);
        self.slots.remove(&id);
        staged.place(&plugins_dir)?;
        drop(staged);
        let manifest = staged_manifest(&plugins_dir, &id)?;
        self.scan();
        if !self.config.disabled.contains(&manifest.id) {
            self.launch_one(&manifest.id)?;
        }
        Ok(manifest.id)
    }

    fn rescan(&mut self) {
        self.scan();
        self.start_enabled();
    }

    fn shutdown(&mut self) {
        let links: Vec<Link> = self.slots.values_mut().filter_map(|s| s.link.take()).collect();
        for link in &links {
            link.request_stop();
        }
        // One shared grace period for all plug-ins, not one each.
        let deadline = std::time::Instant::now() + self.config.timing.shutdown_grace;
        for link in &links {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            link.wait_or_kill(remaining);
        }
        for slot in self.slots.values_mut() {
            slot.resting = PluginState::Stopped;
        }
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        // Never leave plug-in processes behind, even if shutdown wasn't called.
        for slot in self.slots.values_mut() {
            if let Some(link) = slot.link.take() {
                link.kill();
            }
        }
    }
}

/// The manifest of a just-installed plug-in.
fn staged_manifest(plugins_dir: &Path, id: &str) -> Result<Manifest, String> {
    Manifest::load(&plugins_dir.join(id))
}

/// A random hex string for the launch nonce.
fn nonce() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| format!("no randomness for the plug-in nonce: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Launch a plug-in process and start its link.
fn launch(config: &Config, manifest: &Manifest, folder: &Path, events: Sender<HostEvent>, waker: Waker) -> Result<Link, String> {
    let (program, args) = manifest.resolve_command(folder)?;
    if !program.is_file() {
        return Err(format!("the plug-in's program {} is missing", program.display()));
    }
    let data_dir = config.data_root.join(&manifest.id);
    std::fs::create_dir_all(&data_dir).map_err(|e| format!("could not create {}: {e}", data_dir.display()))?;
    std::fs::create_dir_all(&config.log_dir).map_err(|e| format!("could not create {}: {e}", config.log_dir.display()))?;
    let nonce = nonce()?;

    let mut command = Command::new(&program);
    command
        .args(&args)
        .current_dir(folder)
        .env("PHOTOCRAFT_PLUGIN_NONCE", &nonce)
        .env("PHOTOCRAFT_PLUGIN_DIR", folder)
        .env("PHOTOCRAFT_PLUGIN_DATA_DIR", &data_dir)
        .env("PHOTOCRAFT_PLUGIN_LOG_DIR", &config.log_dir)
        .env("PHOTOCRAFT_LOCALE", &config.locale)
        .env("PHOTOCRAFT_HOST_VERSION", &config.host_version)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: a console plug-in would otherwise flash a console window.
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().map_err(|e| format!("could not start {}: {e}", program.display()))?;
    let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        return Err("the plug-in's standard streams are unavailable".into());
    };
    logfile::capture(stderr, &config.log_dir, &manifest.id);
    let welcome = Welcome {
        host_version: config.host_version.clone(),
        protocol: PROTOCOL_VERSION,
        locale: config.locale.clone(),
        data_dir: data_dir.display().to_string(),
        log_dir: config.log_dir.display().to_string(),
        theme: Theme { dark: config.dark_theme },
    };
    let ctx = LinkContext { plugin: manifest.id.clone(), nonce, welcome, timing: config.timing, limits: config.limits, waker };
    Link::start(ctx, stdout, stdin, Box::new(child), events)
}
