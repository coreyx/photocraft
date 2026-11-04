//! Wire protocol between PhotoCraft and *process plug-ins*.
//!
//! A process plug-in is a separate program that PhotoCraft launches and talks to over the child
//! process's stdin and stdout. Unlike the sandboxed WebAssembly filter plug-ins
//! (`photocraft-plugins`), a process plug-in can show panels, react to document changes and keep
//! state, so it can implement whole features. Running in its own process keeps it isolated: a
//! plug-in that crashes or hangs never takes PhotoCraft down, and it shares no code or license
//! with PhotoCraft.
//!
//! This crate is the shared vocabulary only, with no I/O beyond `std::io::Read`/`Write`:
//!
//! - [`frame`]: the binary frame codec. Every message is one frame: a magic tag, a JSON header
//!   and an optional raw binary payload (pixels, images), so bulk data never goes through JSON.
//! - [`message`]: the frame header: requests, responses, errors and events.
//! - [`method`]: every request a plug-in or the host can send, with typed parameters and results.
//! - [`event`]: every event the host sends.
//! - [`widget`]: the widget trees plug-ins describe and the host draws natively.
//! - [`types`]: shared value types (rectangles, pixel formats, document and layer info).
//! - [`host`]: the host's view of its plug-ins (status, and the interface the UI drives them by).
//!
//! The full protocol reference is `docs/process-plugins.md`.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod event;
pub mod frame;
pub mod host;
pub mod message;
pub mod method;
pub mod types;
pub mod widget;

#[cfg(test)]
mod tests;

pub use frame::{Frame, FrameError, Limits, MAGIC};
pub use message::{ErrorBody, ErrorCode, Header};
pub use method::Method;

/// The protocol version this build speaks. The `hello` handshake refuses any other version, so a
/// plug-in built for a different protocol is reported as incompatible instead of misbehaving.
pub const PROTOCOL_VERSION: u32 = 1;
