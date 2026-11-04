//! The shell's side of process plug-ins (`Services::process_plugins`).
//!
//! Once per frame the shell starts the enabled plug-ins (the first time), then drains what they
//! sent. Requests that nothing in the shell serves yet are answered with an `unsupported` error,
//! so a plug-in never waits on a request the host can't handle. Plug-ins are launched and
//! supervised outside the UI thread (`photocraft-process-plugin-host` on the desktop); the web
//! build has no process plug-ins and leaves the service unset.

use std::sync::Arc;

use photocraft_process_plugin_protocol::frame::Frame;
use photocraft_process_plugin_protocol::host::{HostEvent, PluginState};
use photocraft_process_plugin_protocol::message::{ErrorCode, Header};

use crate::PhotocraftApp;
use crate::i18n;

impl PhotocraftApp {
    /// Start the plug-ins on the first frame, then handle everything they sent since last frame.
    pub(crate) fn pump_process_plugins(&mut self, ctx: &egui::Context) {
        let Some(host) = self.services.process_plugins.as_mut() else { return };
        if !self.process_plugins_started {
            self.process_plugins_started = true;
            let repaint = ctx.clone();
            let dark = crate::theme::Tokens::get(ctx).dark();
            host.start(i18n::current().code(), dark, Arc::new(move || repaint.request_repaint()));
        }
        for event in host.drain() {
            match event {
                HostEvent::State { plugin, state } => log_state(&plugin, &state),
                HostEvent::Frame { plugin, frame } => {
                    if let Some(reply) = unserved_reply(&frame.header)
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
}

/// The answer to a plug-in request nothing in the shell serves: `unsupported`. Responses and
/// events need no answer.
fn unserved_reply(header: &Header) -> Option<Frame> {
    match header {
        Header::Request { id, method, .. } => Some(Frame::new(Header::error(*id, ErrorCode::Unsupported, format!("PhotoCraft does not support '{method}'")))),
        _ => None,
    }
}

fn log_state(plugin: &str, state: &PluginState) {
    match state {
        PluginState::Crashed { reason } => log::warn!("plug-in '{plugin}' stopped unexpectedly: {reason}"),
        PluginState::Incompatible { reason } => log::warn!("plug-in '{plugin}' can't run: {reason}"),
        other => log::info!("plug-in '{plugin}': {other:?}"),
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
}
