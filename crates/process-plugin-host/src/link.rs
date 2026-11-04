//! One running plug-in: the threads that talk to it and watch it.
//!
//! Each plug-in gets a reader thread (frames from its stdout), a writer thread (frames to its
//! stdin) and a supervisor thread (exit detection, the handshake deadline and pings). Protocol
//! housekeeping (`hello`, `ping`) is answered here, so a plug-in stays connected even while the UI
//! thread is busy; only application frames are forwarded to the UI.
//!
//! The streams and the child process are abstracted ([`ChildHandle`], any `Read`/`Write`), so the
//! logic is unit-tested with in-memory pipes and a fake process.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use photocraft_process_plugin_protocol::PROTOCOL_VERSION;
use photocraft_process_plugin_protocol::frame::{self, Frame, Limits};
use photocraft_process_plugin_protocol::host::{HostEvent, PluginState, Waker};
use photocraft_process_plugin_protocol::message::{self, ErrorCode, Header};
use photocraft_process_plugin_protocol::method::{self, Method, Welcome};

/// Request ids the supervisor uses for its own `ping`/`shutdown` requests start here, so they
/// never collide with ids the UI hands out (which count up from 1).
pub const INTERNAL_ID_BASE: u64 = 1 << 62;

/// How long things may take.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// The plug-in must send `hello` within this time of being launched.
    pub handshake: Duration,
    /// A `ping` is sent this often.
    pub ping_interval: Duration,
    /// After this many pings in a row go unanswered (and nothing else arrives), the plug-in is
    /// marked not responding.
    pub missed_pings: u32,
    /// How long a plug-in gets to exit after `shutdown` before it is ended.
    pub shutdown_grace: Duration,
    /// How often the supervisor wakes up.
    pub tick: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            handshake: Duration::from_secs(10),
            ping_interval: Duration::from_secs(5),
            missed_pings: 3,
            shutdown_grace: Duration::from_secs(5),
            tick: Duration::from_millis(100),
        }
    }
}

/// The plug-in process, as the supervisor sees it.
pub trait ChildHandle: Send {
    /// `Some(description)` once the process has exited.
    fn try_wait(&mut self) -> Option<String>;
    /// End the process now.
    fn kill(&mut self);
}

impl ChildHandle for std::process::Child {
    fn try_wait(&mut self) -> Option<String> {
        match std::process::Child::try_wait(self) {
            Ok(Some(status)) => Some(match status.code() {
                Some(code) => format!("exited with code {code}"),
                None => "was terminated by a signal".to_string(),
            }),
            Ok(None) => None,
            Err(e) => Some(format!("could not be waited for: {e}")),
        }
    }

    fn kill(&mut self) {
        // Already exited is fine; there is nothing else to do with an error here.
        let _ = std::process::Child::kill(self);
        let _ = std::process::Child::wait(self);
    }
}

/// What a link needs to know about its plug-in and the host.
#[derive(Clone)]
pub struct LinkContext {
    /// The key the host knows the plug-in by (its id).
    pub plugin: String,
    /// The nonce passed at launch, which `hello` must echo.
    pub nonce: String,
    /// The `hello` answer.
    pub welcome: Welcome,
    pub timing: Timing,
    pub limits: Limits,
    /// Wakes the UI after an event is queued.
    pub waker: Waker,
}

/// Mutable state shared by a link's threads and the manager.
struct Shared {
    state: Mutex<PluginState>,
    /// When the last frame from the plug-in arrived.
    last_seen: Mutex<Instant>,
    /// Pings sent since anything arrived.
    unanswered: AtomicU64,
    /// Ids of in-flight internal requests (`ping`, `shutdown`), answered by the supervisor.
    internal: Mutex<HashSet<u64>>,
    /// Set when the host is stopping the plug-in, so its exit isn't reported as a crash.
    stopping: AtomicBool,
    events: Sender<HostEvent>,
    waker: Waker,
    plugin: String,
}

impl Shared {
    /// Queue an event for the UI and wake it. Returns false once the manager is gone.
    fn emit(&self, event: HostEvent) -> bool {
        let sent = self.events.send(event).is_ok();
        (self.waker)();
        sent
    }

    fn state(&self) -> PluginState {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Change state and tell the UI, unless nothing changed. A finished state (stopped, crashed)
    /// is final for this link.
    fn set_state(&self, next: PluginState) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let finished = matches!(*state, PluginState::Stopped | PluginState::Crashed { .. } | PluginState::Incompatible { .. });
        if *state == next || finished {
            return;
        }
        *state = next.clone();
        drop(state);
        // The manager may already be gone during shutdown; then nobody needs the event.
        self.emit(HostEvent::State { plugin: self.plugin.clone(), state: next });
    }

    /// The plug-in ended or misbehaved: record why (as a crash, unless the host stopped it).
    fn finish(&self, reason: String) {
        if self.stopping.load(Ordering::SeqCst) {
            self.set_state(PluginState::Stopped);
        } else {
            self.set_state(PluginState::Crashed { reason });
        }
    }

    fn touch(&self) {
        *self.last_seen.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now();
        self.unanswered.store(0, Ordering::SeqCst);
        if self.state() == PluginState::NotResponding {
            self.set_state(PluginState::Running);
        }
    }
}

/// A running plug-in's connection.
pub struct Link {
    shared: Arc<Shared>,
    outgoing: Sender<Frame>,
    child: Arc<Mutex<Box<dyn ChildHandle>>>,
    next_internal: AtomicU64,
}

impl Link {
    /// Start the threads for a just-launched plug-in.
    pub fn start(
        ctx: LinkContext,
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        child: Box<dyn ChildHandle>,
        events: Sender<HostEvent>,
    ) -> Result<Link, String> {
        let shared = Arc::new(Shared {
            state: Mutex::new(PluginState::Starting),
            last_seen: Mutex::new(Instant::now()),
            unanswered: AtomicU64::new(0),
            internal: Mutex::new(HashSet::new()),
            stopping: AtomicBool::new(false),
            events,
            waker: ctx.waker.clone(),
            plugin: ctx.plugin.clone(),
        });
        // Announce the start so the UI shows "starting" right away.
        shared.emit(HostEvent::State { plugin: ctx.plugin.clone(), state: PluginState::Starting });
        let child = Arc::new(Mutex::new(child));
        let (outgoing, to_write) = mpsc::channel::<Frame>();

        spawn(&ctx.plugin, "writer", {
            let shared = shared.clone();
            let limits = ctx.limits;
            move || write_loop(writer, to_write, limits, &shared)
        })?;
        spawn(&ctx.plugin, "reader", {
            let shared = shared.clone();
            let outgoing = outgoing.clone();
            let child = child.clone();
            let ctx = ctx.clone();
            move || read_loop(reader, &ctx, &shared, &outgoing, &child)
        })?;
        let link = Link { shared, outgoing, child, next_internal: AtomicU64::new(INTERNAL_ID_BASE) };
        let name = ctx.plugin.clone();
        spawn(&name, "supervisor", {
            let shared = link.shared.clone();
            let outgoing = link.outgoing.clone();
            let child = link.child.clone();
            move || supervise(&ctx, &shared, &outgoing, &child)
        })?;
        Ok(link)
    }

    pub fn state(&self) -> PluginState {
        self.shared.state()
    }

    /// Queue a frame for the plug-in. Fails once the plug-in is no longer running.
    pub fn send(&self, frame: Frame) -> Result<(), String> {
        if !self.state().is_live() {
            return Err(format!("plug-in '{}' is not running", self.shared.plugin));
        }
        self.outgoing.send(frame).map_err(|_| format!("plug-in '{}' is not running", self.shared.plugin))
    }

    /// Ask the plug-in to exit (sends `shutdown`); see [`Link::wait_or_kill`].
    pub fn request_stop(&self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        let id = self.next_internal.fetch_add(1, Ordering::SeqCst);
        self.shared.internal.lock().unwrap_or_else(PoisonError::into_inner).insert(id);
        if let Ok(header) = Header::request::<method::Shutdown>(id, &method::Empty {}) {
            let _ = self.outgoing.send(Frame::new(header));
        }
    }

    /// Whether the process has exited (and, if so, record it).
    pub fn has_exited(&self) -> bool {
        let exited = self.child.lock().unwrap_or_else(PoisonError::into_inner).try_wait();
        match exited {
            Some(how) => {
                self.shared.finish(format!("the plug-in {how}"));
                true
            }
            None => false,
        }
    }

    /// End the process now and mark the plug-in stopped.
    pub fn kill(&self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        self.child.lock().unwrap_or_else(PoisonError::into_inner).kill();
        self.shared.set_state(PluginState::Stopped);
    }

    /// After [`Link::request_stop`]: wait up to the grace period for the process to exit, then
    /// end it.
    pub fn wait_or_kill(&self, grace: Duration) {
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if self.has_exited() {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        self.kill();
    }
}

fn spawn(plugin: &str, role: &str, f: impl FnOnce() + Send + 'static) -> Result<(), String> {
    thread::Builder::new()
        .name(format!("plug-in {plugin} {role}"))
        .spawn(f)
        .map(|_| ())
        .map_err(|e| format!("could not start the {role} thread for plug-in '{plugin}': {e}"))
}

fn write_loop(mut writer: impl Write, frames: Receiver<Frame>, limits: Limits, shared: &Shared) {
    while let Ok(frame) = frames.recv() {
        if let Err(e) = frame::write_frame(&mut writer, &frame, limits) {
            shared.finish(format!("writing to the plug-in failed: {e}"));
            return;
        }
    }
}

fn read_loop(mut reader: impl Read, ctx: &LinkContext, shared: &Shared, outgoing: &Sender<Frame>, child: &Mutex<Box<dyn ChildHandle>>) {
    let mut greeted = false;
    loop {
        let frame = match frame::read_frame(&mut reader, ctx.limits) {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                shared.finish("the plug-in closed its connection".into());
                return;
            }
            Err(e) => {
                log::warn!("plug-in '{}': {e}", ctx.plugin);
                shared.finish(format!("the plug-in sent an invalid message: {e}"));
                child.lock().unwrap_or_else(PoisonError::into_inner).kill();
                return;
            }
        };
        shared.touch();
        if !greeted {
            match greet(&frame.header, ctx) {
                Ok(reply) => {
                    let _ = outgoing.send(reply);
                    greeted = true;
                    shared.set_state(PluginState::Running);
                }
                Err((reply, state)) => {
                    if let Some(reply) = reply {
                        let _ = outgoing.send(reply);
                    }
                    shared.set_state(state);
                    // Give the writer a moment to deliver the refusal before ending the process.
                    thread::sleep(Duration::from_millis(50));
                    child.lock().unwrap_or_else(PoisonError::into_inner).kill();
                    return;
                }
            }
            continue;
        }
        let internal = match &frame.header {
            Header::Response { id, .. } | Header::Error { id, .. } => shared.internal.lock().unwrap_or_else(PoisonError::into_inner).remove(id),
            _ => false,
        };
        if !internal && !shared.emit(HostEvent::Frame { plugin: ctx.plugin.clone(), frame }) {
            return;
        }
    }
}

/// Check the plug-in's first message. On success returns the `welcome` frame; otherwise the
/// refusal to send back (if the message was a request) and the state to record.
fn greet(header: &Header, ctx: &LinkContext) -> Result<Frame, (Option<Frame>, PluginState)> {
    let Header::Request { id, method, params } = header else {
        return Err((None, PluginState::Crashed { reason: "the plug-in's first message was not 'hello'".into() }));
    };
    let refuse = |code, reason: String, state: PluginState| Err((Some(Frame::new(Header::error(*id, code, reason))), state));
    if method != method::Hello::NAME {
        let reason = format!("the plug-in's first message was '{method}', not 'hello'");
        return refuse(ErrorCode::Unsupported, reason.clone(), PluginState::Crashed { reason });
    }
    let hello = match message::params::<method::Hello>(params) {
        Ok(hello) => hello,
        Err(e) => return refuse(ErrorCode::Invalid, e.message.clone(), PluginState::Crashed { reason: format!("invalid hello: {}", e.message) }),
    };
    if hello.nonce != ctx.nonce {
        let reason = "the plug-in's hello did not carry the launch nonce".to_string();
        return refuse(ErrorCode::Invalid, reason.clone(), PluginState::Crashed { reason });
    }
    if hello.protocol != PROTOCOL_VERSION {
        let reason = format!("the plug-in speaks protocol {}, PhotoCraft speaks {PROTOCOL_VERSION}", hello.protocol);
        return refuse(ErrorCode::Unsupported, reason.clone(), PluginState::Incompatible { reason });
    }
    match Header::response(*id, &ctx.welcome) {
        Ok(header) => Ok(Frame::new(header)),
        Err(e) => Err((None, PluginState::Crashed { reason: e.message })),
    }
}

fn supervise(ctx: &LinkContext, shared: &Shared, outgoing: &Sender<Frame>, child: &Mutex<Box<dyn ChildHandle>>) {
    let started = Instant::now();
    let mut last_ping = Instant::now();
    // Pings use their own block of internal ids, apart from the `shutdown` ids the link issues.
    let mut next_id = INTERNAL_ID_BASE + (1 << 60);
    loop {
        thread::sleep(ctx.timing.tick);
        let exited = child.lock().unwrap_or_else(PoisonError::into_inner).try_wait();
        if let Some(how) = exited {
            shared.finish(format!("the plug-in {how}"));
            return;
        }
        let state = shared.state();
        match state {
            PluginState::Starting if started.elapsed() > ctx.timing.handshake => {
                shared.set_state(PluginState::Crashed { reason: format!("the plug-in did not say hello within {} s", ctx.timing.handshake.as_secs()) });
                child.lock().unwrap_or_else(PoisonError::into_inner).kill();
                return;
            }
            PluginState::Running | PluginState::NotResponding if last_ping.elapsed() >= ctx.timing.ping_interval => {
                last_ping = Instant::now();
                let unanswered = shared.unanswered.fetch_add(1, Ordering::SeqCst) + 1;
                if unanswered > u64::from(ctx.timing.missed_pings) {
                    shared.set_state(PluginState::NotResponding);
                }
                next_id += 1;
                shared.internal.lock().unwrap_or_else(PoisonError::into_inner).insert(next_id);
                if let Ok(header) = Header::request::<method::Ping>(next_id, &method::Empty {}) {
                    let _ = outgoing.send(Frame::new(header));
                }
            }
            PluginState::Stopped | PluginState::Crashed { .. } | PluginState::Incompatible { .. } => return,
            _ => {}
        }
    }
}
