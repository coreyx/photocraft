//! Unit tests: manifests, discovery, log rotation, and the link's handshake and supervision over
//! in-memory pipes with a fake process. End-to-end tests against a real plug-in process live in
//! `tests/` and use the `process-plugin-echo` example.

use std::io::{PipeReader, PipeWriter};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use photocraft_process_plugin_protocol::PROTOCOL_VERSION;
use photocraft_process_plugin_protocol::frame::{self, Frame, Limits};
use photocraft_process_plugin_protocol::host::{HostEvent, PluginState, ProcessPluginHost, StartSettings};
use photocraft_process_plugin_protocol::message::{ErrorCode, Header};
use photocraft_process_plugin_protocol::method::{self, HelloParams, Method, PluginIdentity, Theme, Welcome};

use crate::link::{ChildHandle, Link, LinkContext, Timing};
use crate::manifest::Manifest;
use crate::{Config, Manager, logfile};

// --- manifests ---

const GOOD: &str = r#"
id = "org.example.echo"
name = "Echo"
version = "1.0.0"
protocol = 1
license = "MIT"
command = ["bin/echo", "--mode", "conformance"]
"#;

#[test]
fn manifest_parses_and_resolves_its_command() {
    let m = Manifest::parse(GOOD).unwrap();
    assert_eq!(m.id, "org.example.echo");
    let (program, args) = m.resolve_command(std::path::Path::new("plugins/echo")).unwrap();
    assert!(program.ends_with("bin/echo"));
    assert_eq!(args, ["--mode", "conformance"]);
}

#[test]
fn manifest_refuses_programs_outside_its_folder() {
    for command in [r#"["/usr/bin/sh"]"#, r#"["../escape"]"#, r#"["bin/../../escape"]"#, r#"["sh"]"#, r#"[]"#, r#"["C:\\Windows\\cmd.exe"]"#] {
        let text = GOOD.replace(r#"["bin/echo", "--mode", "conformance"]"#, command);
        assert!(Manifest::parse(&text).is_err(), "{command} should be refused");
    }
    assert!(Manifest::parse(&GOOD.replace(r#"["bin/echo", "--mode", "conformance"]"#, r#"["./echo"]"#)).is_ok());
}

#[test]
fn manifest_refuses_bad_ids_and_unknown_fields() {
    for id in ["", "Echo", "noperiod", "org.example.echo!", ".org.x", "org.x."] {
        let text = GOOD.replace("org.example.echo", id);
        assert!(Manifest::parse(&text).is_err(), "id '{id}' should be refused");
    }
    assert!(Manifest::parse(&format!("{GOOD}\nextra = 1\n")).is_err());
    assert!(Manifest::parse("not toml at all [").is_err());
}

// --- discovery ---

fn config(dir: &tempfile::TempDir) -> Config {
    Config::new(dir.path().join("plugins"), dir.path().join("data"), dir.path().join("logs"), "0.0.0-test", "en")
}

fn install(plugins: &std::path::Path, folder: &str, manifest: &str) {
    let path = plugins.join(folder);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join(crate::manifest::FILE_NAME), manifest).unwrap();
}

#[test]
fn scan_lists_valid_broken_and_incompatible_plugins() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(&dir);
    install(&cfg.plugins_dir, "echo", GOOD);
    install(&cfg.plugins_dir, "broken", "id = 3");
    install(&cfg.plugins_dir, "future", &GOOD.replace("org.example.echo", "org.example.future").replace("protocol = 1", "protocol = 99"));
    let manager = Manager::new(cfg);
    let statuses = manager.statuses();
    assert_eq!(statuses.len(), 3);
    let echo = statuses.iter().find(|s| s.id == "org.example.echo").unwrap();
    assert_eq!(echo.state, PluginState::Stopped);
    assert!(echo.enabled);
    let broken = statuses.iter().find(|s| s.id == "folder:broken").unwrap();
    assert!(matches!(broken.state, PluginState::Incompatible { .. }));
    let future = statuses.iter().find(|s| s.id == "org.example.future").unwrap();
    assert!(matches!(&future.state, PluginState::Incompatible { reason } if reason.contains("99")));
}

#[test]
fn duplicate_ids_are_reported_not_merged() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(&dir);
    install(&cfg.plugins_dir, "a", GOOD);
    install(&cfg.plugins_dir, "b", GOOD);
    let manager = Manager::new(cfg);
    let statuses = manager.statuses();
    assert_eq!(statuses.len(), 2);
    assert!(statuses.iter().any(|s| matches!(&s.state, PluginState::Incompatible { reason } if reason.contains("already uses"))));
}

#[test]
fn a_missing_program_is_reported_as_a_crash_without_failing_others() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(&dir);
    install(&cfg.plugins_dir, "echo", GOOD);
    let mut manager = Manager::new(cfg);
    manager.start_enabled();
    let status = &manager.statuses()[0];
    assert!(matches!(&status.state, PluginState::Crashed { reason } if reason.contains("missing")));
    assert!(manager.drain().iter().any(|e| matches!(e, HostEvent::State { state: PluginState::Crashed { .. }, .. })));
}

#[test]
fn disabling_and_removing_work_on_stopped_plugins() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(&dir);
    install(&cfg.plugins_dir, "echo", GOOD);
    let folder = cfg.plugins_dir.join("echo");
    let mut manager = Manager::new(cfg);
    manager.set_enabled("org.example.echo", false).unwrap();
    assert_eq!(manager.statuses()[0].state, PluginState::Disabled);
    assert!(manager.disabled().contains("org.example.echo"));
    assert!(manager.set_enabled("org.example.nothing", true).is_err());
    manager.remove("org.example.echo").unwrap();
    assert!(!folder.exists());
    assert!(manager.statuses().is_empty());
}

#[test]
fn starting_wakes_the_ui_for_every_event() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(&dir);
    install(&cfg.plugins_dir, "echo", GOOD);
    let mut manager = Manager::new(cfg);
    let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = wakes.clone();
    manager.start(StartSettings {
        locale: "ja".into(),
        dark_theme: false,
        disabled: Vec::new(),
        waker: Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    });
    // The missing program becomes a crash event, and the UI was woken for it.
    assert!(wakes.load(Ordering::SeqCst) >= 1);
    assert_eq!(manager.drain().len(), 1);
}

#[test]
fn request_ids_count_up_from_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut manager = Manager::new(config(&dir));
    assert_eq!(manager.next_request_id(), 1);
    assert_eq!(manager.next_request_id(), 2);
}

// --- log files ---

#[test]
fn large_logs_rotate_at_launch() {
    let dir = tempfile::tempdir().unwrap();
    let log = logfile::path(dir.path(), "p");
    std::fs::write(&log, vec![b'x'; (logfile::ROTATE_BYTES + 1) as usize]).unwrap();
    let mut file = logfile::open(dir.path(), "p").unwrap();
    use std::io::Write;
    writeln!(file, "fresh").unwrap();
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "fresh\n");
    assert!(dir.path().join("p.1.log").exists());
}

#[test]
fn captured_output_lands_in_the_log() {
    let dir = tempfile::tempdir().unwrap();
    logfile::capture(std::io::Cursor::new(b"hello from stderr\n".to_vec()), dir.path(), "p");
    let log = logfile::path(dir.path(), "p");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && std::fs::read_to_string(&log).map(|s| s.is_empty()).unwrap_or(true) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(std::fs::read_to_string(&log).unwrap(), "hello from stderr\n");
}

// --- links over in-memory pipes ---

/// A stand-in for the plug-in process that tests can "exit".
#[derive(Clone, Default)]
struct FakeChild {
    exited: Arc<AtomicBool>,
    killed: Arc<AtomicBool>,
}

impl ChildHandle for FakeChild {
    fn try_wait(&mut self) -> Option<String> {
        (self.exited.load(Ordering::SeqCst) || self.killed.load(Ordering::SeqCst)).then(|| "exited with code 3".to_string())
    }
    fn kill(&mut self) {
        self.killed.store(true, Ordering::SeqCst);
    }
}

/// The plug-in end of a link under test.
struct FakePlugin {
    to_host: PipeWriter,
    from_host: PipeReader,
}

impl FakePlugin {
    fn send(&mut self, header: Header) {
        frame::write_frame(&mut self.to_host, &Frame::new(header), Limits::default()).unwrap();
    }
    fn recv(&mut self) -> Frame {
        frame::read_frame(&mut self.from_host, Limits::default()).unwrap().unwrap()
    }
}

fn fast_timing() -> Timing {
    Timing {
        handshake: Duration::from_millis(300),
        ping_interval: Duration::from_millis(50),
        missed_pings: 2,
        shutdown_grace: Duration::from_millis(200),
        tick: Duration::from_millis(10),
    }
}

fn welcome() -> Welcome {
    Welcome {
        host_version: "test".into(),
        protocol: PROTOCOL_VERSION,
        locale: "en".into(),
        data_dir: "d".into(),
        log_dir: "l".into(),
        theme: Theme { dark: false },
    }
}

fn start_link(timing: Timing) -> (Link, FakePlugin, FakeChild, Receiver<HostEvent>) {
    let (host_reads, plugin_writes) = std::io::pipe().unwrap();
    let (plugin_reads, host_writes) = std::io::pipe().unwrap();
    let child = FakeChild::default();
    let (events_tx, events_rx) = mpsc::channel();
    let ctx =
        LinkContext { plugin: "org.example.echo".into(), nonce: "n0nce".into(), welcome: welcome(), timing, limits: Limits::default(), waker: Arc::new(|| {}) };
    let link = Link::start(ctx, host_reads, host_writes, Box::new(child.clone()), events_tx).unwrap();
    (link, FakePlugin { to_host: plugin_writes, from_host: plugin_reads }, child, events_rx)
}

fn hello(nonce: &str, protocol: u32) -> Header {
    let params = HelloParams { protocol, nonce: nonce.into(), plugin: PluginIdentity { id: "org.example.echo".into(), version: "1".into() } };
    Header::request::<method::Hello>(1, &params).unwrap()
}

fn wait_for_state(link: &Link, wanted: impl Fn(&PluginState) -> bool) -> PluginState {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let state = link.state();
        if wanted(&state) || Instant::now() > deadline {
            return state;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_kill(child: &FakeChild) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !child.killed.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    child.killed.load(Ordering::SeqCst)
}

#[test]
fn hello_with_the_nonce_is_welcomed_and_app_frames_reach_the_ui() {
    let (link, mut plugin, _child, events) = start_link(Timing::default());
    plugin.send(hello("n0nce", PROTOCOL_VERSION));
    let reply = plugin.recv();
    let Header::Response { id: 1, result } = reply.header else { panic!("expected welcome, got {:?}", reply.header) };
    let welcome: Welcome = serde_json::from_value(result).unwrap();
    assert_eq!(welcome.protocol, PROTOCOL_VERSION);
    assert_eq!(wait_for_state(&link, |s| *s == PluginState::Running), PluginState::Running);

    plugin.send(Header::Request { id: 2, method: "doc.list".into(), params: serde_json::Value::Null });
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got_frame = false;
    while !got_frame && Instant::now() < deadline {
        if let Ok(HostEvent::Frame { plugin, frame }) = events.recv_timeout(Duration::from_millis(50)) {
            assert_eq!(plugin, "org.example.echo");
            assert!(matches!(frame.header, Header::Request { id: 2, .. }));
            got_frame = true;
        }
    }
    assert!(got_frame, "the request was forwarded to the UI");

    link.send(Frame::new(Header::response(2, &Vec::<u32>::new()).unwrap())).unwrap();
    assert!(matches!(plugin.recv().header, Header::Response { id: 2, .. }));
}

#[test]
fn a_wrong_nonce_is_refused_and_the_process_ended() {
    let (link, mut plugin, child, _events) = start_link(Timing::default());
    plugin.send(hello("guess", PROTOCOL_VERSION));
    let reply = plugin.recv();
    assert!(matches!(reply.header, Header::Error { error, .. } if error.code == ErrorCode::Invalid));
    assert!(matches!(wait_for_state(&link, |s| matches!(s, PluginState::Crashed { .. })), PluginState::Crashed { .. }));
    // The refusal is delivered first, then the process is ended.
    assert!(wait_for_kill(&child));
}

#[test]
fn another_protocol_version_is_incompatible() {
    let (link, mut plugin, _child, _events) = start_link(Timing::default());
    plugin.send(hello("n0nce", PROTOCOL_VERSION + 1));
    assert!(matches!(plugin.recv().header, Header::Error { error, .. } if error.code == ErrorCode::Unsupported));
    assert!(matches!(wait_for_state(&link, |s| matches!(s, PluginState::Incompatible { .. })), PluginState::Incompatible { .. }));
}

#[test]
fn a_first_message_other_than_hello_is_a_crash() {
    let (link, mut plugin, _child, _events) = start_link(Timing::default());
    plugin.send(Header::Request { id: 1, method: "doc.list".into(), params: serde_json::Value::Null });
    let _ = plugin.recv();
    assert!(matches!(wait_for_state(&link, |s| matches!(s, PluginState::Crashed { .. })), PluginState::Crashed { reason } if reason.contains("hello")));
}

#[test]
fn garbage_on_the_pipe_is_a_crash() {
    use std::io::Write;
    let (link, mut plugin, child, _events) = start_link(Timing::default());
    plugin.to_host.write_all(b"this is not a frame at all....").unwrap();
    assert!(matches!(wait_for_state(&link, |s| matches!(s, PluginState::Crashed { .. })), PluginState::Crashed { reason } if reason.contains("invalid")));
    assert!(wait_for_kill(&child));
}

#[test]
fn no_hello_in_time_is_a_crash() {
    let (link, _plugin, child, _events) = start_link(fast_timing());
    assert!(matches!(wait_for_state(&link, |s| matches!(s, PluginState::Crashed { .. })), PluginState::Crashed { reason } if reason.contains("hello")));
    assert!(wait_for_kill(&child));
}

#[test]
fn pings_are_sent_and_silence_means_not_responding() {
    let (link, mut plugin, _child, _events) = start_link(fast_timing());
    plugin.send(hello("n0nce", PROTOCOL_VERSION));
    let _welcome = plugin.recv();
    let ping = plugin.recv();
    let Header::Request { id, method, .. } = ping.header else { panic!("expected a ping") };
    assert_eq!(method, method::Ping::NAME);
    // Answering keeps it running; the answer is consumed by the supervisor, not forwarded.
    plugin.send(Header::response(id, &method::Empty {}).unwrap());
    assert_eq!(link.state(), PluginState::Running);
    // Then stay silent: after the allowed missed pings it is not responding ...
    assert_eq!(wait_for_state(&link, |s| *s == PluginState::NotResponding), PluginState::NotResponding);
    // ... and any message brings it back.
    plugin.send(Header::Event { topic: "x".into(), data: serde_json::Value::Null });
    assert_eq!(wait_for_state(&link, |s| *s == PluginState::Running), PluginState::Running);
}

#[test]
fn an_exiting_process_is_a_crash_unless_stopped() {
    let (link, mut plugin, child, events) = start_link(Timing::default());
    plugin.send(hello("n0nce", PROTOCOL_VERSION));
    let _ = plugin.recv();
    child.exited.store(true, Ordering::SeqCst);
    assert!(matches!(wait_for_state(&link, |s| matches!(s, PluginState::Crashed { .. })), PluginState::Crashed { reason } if reason.contains("code 3")));
    let states: Vec<PluginState> = events.try_iter().filter_map(|e| if let HostEvent::State { state, .. } = e { Some(state) } else { None }).collect();
    assert!(states.contains(&PluginState::Starting) && states.contains(&PluginState::Running));

    let (link, mut plugin, child, _events) = start_link(Timing::default());
    plugin.send(hello("n0nce", PROTOCOL_VERSION));
    let _ = plugin.recv();
    link.request_stop();
    let shutdown = plugin.recv();
    assert!(matches!(shutdown.header, Header::Request { method, .. } if method == method::Shutdown::NAME));
    child.exited.store(true, Ordering::SeqCst);
    link.wait_or_kill(Duration::from_secs(2));
    assert_eq!(link.state(), PluginState::Stopped);
    assert!(!child.killed.load(Ordering::SeqCst), "it exited on its own, so it wasn't killed");
}

#[test]
fn a_plugin_ignoring_shutdown_is_killed_after_the_grace_period() {
    let (link, mut plugin, child, _events) = start_link(Timing::default());
    plugin.send(hello("n0nce", PROTOCOL_VERSION));
    let _ = plugin.recv();
    link.request_stop();
    let started = Instant::now();
    link.wait_or_kill(Duration::from_millis(150));
    assert!(started.elapsed() >= Duration::from_millis(150));
    assert!(child.killed.load(Ordering::SeqCst));
    assert_eq!(link.state(), PluginState::Stopped);
    assert!(link.send(Frame::new(Header::Event { topic: "x".into(), data: serde_json::Value::Null })).is_err());
}

#[test]
fn closing_the_connection_is_a_crash() {
    let (link, plugin, _child, _events) = start_link(Timing::default());
    drop(plugin);
    assert!(matches!(wait_for_state(&link, |s| matches!(s, PluginState::Crashed { .. })), PluginState::Crashed { reason } if reason.contains("closed")));
}
