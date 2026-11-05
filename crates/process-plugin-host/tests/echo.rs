//! End-to-end tests: the real manager against the real `process-plugin-echo` plug-in process.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use photocraft_process_plugin_host::{Config, Manager, Timing};
use photocraft_process_plugin_protocol::frame::Frame;
use photocraft_process_plugin_protocol::host::{HostEvent, PluginState, ProcessPluginHost, StartSettings};
use photocraft_process_plugin_protocol::message::Header;

const ID: &str = "org.example.echo";

/// A plug-ins folder with the echo plug-in installed in `mode`.
struct Fixture {
    dir: tempfile::TempDir,
    manager: Manager,
}

fn echo_program() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_process-plugin-echo"))
}

fn fixture(mode: &str, timing: Timing) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    let folder = plugins.join("echo");
    std::fs::create_dir_all(folder.join("bin")).unwrap();
    let program = folder.join("bin").join(echo_program().file_name().unwrap());
    std::fs::copy(echo_program(), &program).unwrap();
    let manifest = format!(
        "id = \"{ID}\"\nname = \"Echo\"\nversion = \"1.0.0\"\nprotocol = 1\nlicense = \"MIT\"\ncommand = [\"bin/{}\", \"{mode}\"]\n",
        program.file_name().unwrap().to_string_lossy()
    );
    std::fs::write(folder.join("photocraft-plugin.toml"), manifest).unwrap();
    let mut config = Config::new(plugins, dir.path().join("data"), dir.path().join("logs"), "test", "en");
    config.timing = timing;
    let manager = Manager::new(config);
    Fixture { dir, manager }
}

fn quick() -> Timing {
    Timing { ping_interval: Duration::from_millis(100), missed_pings: 3, shutdown_grace: Duration::from_millis(1500), ..Timing::default() }
}

fn start(fixture: &mut Fixture) {
    fixture.manager.start(StartSettings { locale: "en".into(), dark_theme: true, disabled: Vec::new(), waker: Arc::new(|| {}) });
}

/// Drain until `wanted` matches the plug-in's state, collecting frames on the way.
fn wait_state(manager: &mut Manager, within: Duration, wanted: impl Fn(&PluginState) -> bool) -> (PluginState, Vec<Frame>) {
    let deadline = Instant::now() + within;
    let mut frames = Vec::new();
    loop {
        for event in manager.drain() {
            if let HostEvent::Frame { frame, .. } = event {
                frames.push(frame);
            }
        }
        let state = manager.statuses().into_iter().find(|s| s.id == ID).map(|s| s.state).unwrap();
        if wanted(&state) || Instant::now() > deadline {
            return (state, frames);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn log_text(fixture: &Fixture) -> String {
    std::fs::read_to_string(fixture.dir.path().join("logs").join(format!("{ID}.log"))).unwrap_or_default()
}

#[test]
fn a_conforming_plugin_starts_talks_and_shuts_down() {
    let mut f = fixture("conformance", quick());
    let started = Instant::now();
    start(&mut f);
    let (state, mut frames) = wait_state(&mut f.manager, Duration::from_secs(5), |s| *s == PluginState::Running);
    assert_eq!(state, PluginState::Running);
    assert!(started.elapsed() < Duration::from_secs(5), "running within 5 s");

    // Its first request reaches the UI side; answer it like the UI would.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !frames.iter().any(|f| matches!(&f.header, Header::Request { id: 2, method, .. } if method == "doc.list")) && Instant::now() < deadline {
        frames.extend(f.manager.drain().into_iter().filter_map(|e| if let HostEvent::Frame { frame, .. } = e { Some(frame) } else { None }));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(frames.iter().any(|f| matches!(&f.header, Header::Request { id: 2, .. })));
    f.manager.send(ID, Frame::new(Header::response(2, &Vec::<u8>::new()).unwrap())).unwrap();

    // Pings are answered, so it stays running across several intervals.
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(wait_state(&mut f.manager, Duration::from_millis(10), |_| true).0, PluginState::Running);

    let stopping = Instant::now();
    f.manager.shutdown();
    assert!(stopping.elapsed() < Duration::from_secs(10));
    assert_eq!(f.manager.statuses()[0].state, PluginState::Stopped);
    // The plug-in's stderr went to its log, including the host's answer to its request.
    let log = log_text(&f);
    assert!(log.contains("welcome"), "log: {log}");
    assert!(log.contains("id: 2"), "log: {log}");
}

#[test]
fn a_crashing_plugin_is_reported_with_its_exit_code() {
    let mut f = fixture("crash", quick());
    start(&mut f);
    let (state, _) = wait_state(&mut f.manager, Duration::from_secs(5), |s| matches!(s, PluginState::Crashed { .. }));
    assert!(matches!(&state, PluginState::Crashed { reason } if reason.contains('7')), "{state:?}");
    // It can be started again.
    f.manager.restart(ID).unwrap();
    let (state, _) = wait_state(&mut f.manager, Duration::from_secs(5), |s| matches!(s, PluginState::Crashed { .. }));
    assert!(matches!(state, PluginState::Crashed { .. }));
}

#[test]
fn invalid_and_oversized_frames_disconnect_the_plugin() {
    for mode in ["invalid-frame", "oversize"] {
        let mut f = fixture(mode, quick());
        start(&mut f);
        let (state, _) = wait_state(&mut f.manager, Duration::from_secs(5), |s| matches!(s, PluginState::Crashed { .. }));
        assert!(matches!(&state, PluginState::Crashed { reason } if reason.contains("invalid message")), "{mode}: {state:?}");
        if mode == "oversize" {
            assert!(matches!(&state, PluginState::Crashed { reason } if reason.contains("exceeds")), "{state:?}");
        }
    }
}

#[test]
fn a_hello_without_the_launch_nonce_is_refused() {
    let mut f = fixture("wrong-nonce", quick());
    start(&mut f);
    let (state, _) = wait_state(&mut f.manager, Duration::from_secs(5), |s| matches!(s, PluginState::Crashed { .. }));
    assert!(matches!(&state, PluginState::Crashed { reason } if reason.contains("nonce")), "{state:?}");
    assert!(log_text(&f).contains("refused"), "the plug-in saw its hello refused");
}

#[test]
fn a_silent_plugin_is_marked_not_responding_and_ended_on_shutdown() {
    let mut f = fixture("hang", quick());
    start(&mut f);
    let (state, _) = wait_state(&mut f.manager, Duration::from_secs(5), |s| *s == PluginState::NotResponding);
    assert_eq!(state, PluginState::NotResponding);
    let stopping = Instant::now();
    f.manager.shutdown();
    assert!(stopping.elapsed() < Duration::from_secs(10));
    assert_eq!(f.manager.statuses()[0].state, PluginState::Stopped);
}

#[test]
fn a_plugin_ignoring_shutdown_is_ended_after_the_grace_period() {
    let mut f = fixture("ignore-shutdown", quick());
    start(&mut f);
    assert_eq!(wait_state(&mut f.manager, Duration::from_secs(5), |s| *s == PluginState::Running).0, PluginState::Running);
    let heartbeat = f.dir.path().join("data").join(ID).join("heartbeat");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !heartbeat.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let stopping = Instant::now();
    f.manager.shutdown();
    let took = stopping.elapsed();
    assert!(took >= Duration::from_millis(1400), "waited the grace period ({took:?})");
    assert!(took < Duration::from_secs(10));
    assert!(!still_beating(&heartbeat), "the process was ended");
}

#[test]
fn disabling_stops_and_enabling_restarts_without_restarting_photocraft() {
    let mut f = fixture("conformance", quick());
    start(&mut f);
    assert_eq!(wait_state(&mut f.manager, Duration::from_secs(5), |s| *s == PluginState::Running).0, PluginState::Running);
    f.manager.set_enabled(ID, false).unwrap();
    assert_eq!(f.manager.statuses()[0].state, PluginState::Disabled);
    f.manager.set_enabled(ID, true).unwrap();
    assert_eq!(wait_state(&mut f.manager, Duration::from_secs(5), |s| *s == PluginState::Running).0, PluginState::Running);
    f.manager.restart(ID).unwrap();
    assert_eq!(wait_state(&mut f.manager, Duration::from_secs(5), |s| *s == PluginState::Running).0, PluginState::Running);
    f.manager.shutdown();
}

#[test]
fn dropping_the_manager_ends_its_plugins() {
    let mut f = fixture("ignore-shutdown", quick());
    start(&mut f);
    assert_eq!(wait_state(&mut f.manager, Duration::from_secs(5), |s| *s == PluginState::Running).0, PluginState::Running);
    let heartbeat = f.dir.path().join("data").join(ID).join("heartbeat");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !heartbeat.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let Fixture { dir, manager } = f;
    drop(manager);
    assert!(!still_beating(&heartbeat));
    drop(dir);
}

/// Whether the heartbeat file still changes over a few beats.
fn still_beating(path: &Path) -> bool {
    std::thread::sleep(Duration::from_millis(100));
    let before = std::fs::read_to_string(path).unwrap_or_default();
    std::thread::sleep(Duration::from_millis(300));
    std::fs::read_to_string(path).unwrap_or_default() != before
}

/// Build a plug-in package: the echo program and a manifest, inside `top` (or at the root).
fn package(dir: &Path, top: Option<&str>, mode: &str, extra: &[(&str, &[u8])]) -> PathBuf {
    use std::io::Write;
    let path = dir.join("echo-package.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).unix_permissions(0o755);
    let prefix = top.map(|t| format!("{t}/")).unwrap_or_default();
    let name = echo_program().file_name().unwrap().to_string_lossy().to_string();
    zip.start_file(format!("{prefix}bin/{name}"), opts).unwrap();
    zip.write_all(&std::fs::read(echo_program()).unwrap()).unwrap();
    let manifest = format!("id = \"{ID}\"\nname = \"Echo\"\nversion = \"1.0.0\"\nprotocol = 1\nlicense = \"MIT\"\ncommand = [\"bin/{name}\", \"{mode}\"]\n");
    zip.start_file(format!("{prefix}photocraft-plugin.toml"), opts).unwrap();
    zip.write_all(manifest.as_bytes()).unwrap();
    for (entry, bytes) in extra {
        zip.start_file(*entry, opts).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    path
}

fn empty_manager(dir: &Path) -> Manager {
    let mut config = Config::new(dir.join("plugins"), dir.join("data"), dir.join("logs"), "test", "en");
    config.timing = quick();
    let mut manager = Manager::new(config);
    manager.start(StartSettings { locale: "en".into(), dark_theme: true, disabled: Vec::new(), waker: Arc::new(|| {}) });
    manager
}

#[test]
fn a_package_installs_starts_and_can_be_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = empty_manager(dir.path());
    let pkg = package(dir.path(), Some("echo-1.0.0"), "conformance", &[]);
    assert_eq!(manager.install(pkg.to_str().unwrap()).unwrap(), ID);
    assert!(dir.path().join("plugins").join(ID).join("photocraft-plugin.toml").is_file());
    assert_eq!(wait_state(&mut manager, Duration::from_secs(5), |s| *s == PluginState::Running).0, PluginState::Running);

    // Installing again replaces the running copy and starts the new one.
    let pkg = package(dir.path(), None, "conformance", &[]);
    assert_eq!(manager.install(pkg.to_str().unwrap()).unwrap(), ID);
    assert_eq!(manager.statuses().len(), 1);
    assert_eq!(wait_state(&mut manager, Duration::from_secs(5), |s| *s == PluginState::Running).0, PluginState::Running);
    // No staging folder is left behind.
    let leftovers: Vec<_> =
        std::fs::read_dir(dir.path().join("plugins")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with('.')).collect();
    assert!(leftovers.is_empty());
    manager.shutdown();
}

#[test]
fn a_plugin_can_update_itself_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = empty_manager(dir.path());
    let pkg = package(dir.path(), None, "conformance", &[]);
    assert_eq!(manager.install(pkg.to_str().unwrap()).unwrap(), ID);
    assert_eq!(wait_state(&mut manager, Duration::from_secs(5), |s| *s == PluginState::Running).0, PluginState::Running);

    // Its own newer package: replaced and running again.
    let newer = package(dir.path(), Some("echo-1.1.0"), "conformance", &[("echo-1.1.0/CHANGELOG.md", b"1.1.0")]);
    manager.update(ID, newer.to_str().unwrap()).unwrap();
    assert!(dir.path().join("plugins").join(ID).join("CHANGELOG.md").is_file());
    assert_eq!(wait_state(&mut manager, Duration::from_secs(5), |s| *s == PluginState::Running).0, PluginState::Running);

    // Another plug-in's id: refused, nothing installed.
    let err = manager.update("org.example.other", newer.to_str().unwrap()).unwrap_err();
    assert!(err.contains("not 'org.example.other'"), "{err}");
    assert_eq!(manager.statuses().len(), 1);
    manager.shutdown();
}

#[test]
fn packages_cannot_escape_the_plugin_folder() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = empty_manager(dir.path());
    let pkg = package(dir.path(), None, "conformance", &[("../escaped.txt", b"x")]);
    let err = manager.install(pkg.to_str().unwrap()).unwrap_err();
    assert!(err.contains("outside"), "{err}");
    assert!(!dir.path().join("escaped.txt").exists());
    assert!(manager.statuses().is_empty());
}

#[test]
fn a_package_without_a_manifest_is_refused() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let mut manager = empty_manager(dir.path());
    let path = dir.path().join("nothing.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    zip.start_file("readme.txt", zip::write::SimpleFileOptions::default()).unwrap();
    zip.write_all(b"hello").unwrap();
    zip.finish().unwrap();
    assert!(manager.install(path.to_str().unwrap()).unwrap_err().contains("photocraft-plugin.toml"));
    let not_zip = dir.path().join("not.zip");
    std::fs::write(&not_zip, b"plain text").unwrap();
    assert!(manager.install(not_zip.to_str().unwrap()).unwrap_err().contains(".zip"));
}

#[test]
fn plugins_disabled_in_preferences_stay_stopped() {
    let mut f = fixture("conformance", quick());
    f.manager.start(StartSettings { locale: "en".into(), dark_theme: true, disabled: vec![ID.to_string()], waker: Arc::new(|| {}) });
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(f.manager.statuses()[0].state, PluginState::Disabled);
    assert!(!f.manager.statuses()[0].enabled);
}
