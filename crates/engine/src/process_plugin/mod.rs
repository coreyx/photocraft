//! Serving process plug-ins' requests against the session's documents.
//!
//! The shell drains each plug-in's frames (`Services::process_plugins`) and hands its requests to
//! [`Session::plugin_request`], which answers with a JSON result and an optional binary payload,
//! or a typed protocol error. This module owns the document side of the process plug-in protocol
//! (`photocraft-process-plugin-protocol`): documents, the layer tree, pixels, selections and masks,
//! undo groups, the change events subscribed plug-ins receive, the data plug-ins store in
//! documents, and transient previews.
//!
//! Every change a plug-in makes is an ordinary [`Session::edit`], so it is one undoable history
//! step labelled by the plug-in, or part of one when it runs inside an undo group
//! (`undo.begin`/`undo.end`), which coalesces the group's edits into a single step. A plug-in may
//! edit a document that isn't the active one (a result arriving after the user switched tabs): the
//! edit runs against that document without switching the window to it.
//!
//! Requests are validated as hostile input: a stale document or layer, a malformed parameter or an
//! absurd size is an error the plug-in can act on, never a panic.

mod docs;
#[cfg(test)]
mod event_tests;
mod events;
mod layers;
#[cfg(test)]
mod pixel_tests;
mod pixels;
mod preview;
#[cfg(test)]
mod preview_tests;
mod storage;
#[cfg(test)]
mod storage_tests;
#[cfg(test)]
mod tests;
mod undo;

use std::collections::{BTreeSet, HashMap};

use photocraft_doc::{DocId, Document, LayerId};
use photocraft_process_plugin_protocol::message::{ErrorBody, ErrorCode};
use serde_json::Value;

pub use events::PluginEvent;
pub use preview::PreviewRow;

use crate::{EngineError, Session};

/// What a request returns: its JSON result and an optional binary payload.
pub type Reply = (Value, Vec<u8>);

/// A request's outcome.
pub type Outcome = Result<Reply, ErrorBody>;

/// Process plug-in state kept by the session: open undo groups and event subscriptions.
#[derive(Default)]
pub struct Broker {
    groups: HashMap<u64, undo::Group>,
    next_group: u64,
    /// Each subscribed plug-in's document topics.
    subscriptions: HashMap<String, BTreeSet<&'static str>>,
    /// The documents as the last [`Session::plugin_events`] saw them (while anyone subscribes).
    watch: Option<events::Watch>,
    /// Per document, the revision a plug-in's edit produced and the plug-in: the next change
    /// event at that revision carries it as its origin.
    origins: HashMap<DocId, (u64, String)>,
    /// Each document's transient preview (`preview.show`).
    previews: HashMap<DocId, preview::Preview>,
    /// Moves whenever a preview is shown or hidden.
    preview_generation: u64,
}

impl Session {
    /// Serve one request from process plug-in `plugin`: `method` with `params` and the frame's
    /// binary `payload`.
    pub fn plugin_request(&mut self, plugin: &str, method: &str, params: &Value, payload: &[u8]) -> Outcome {
        match method {
            "doc.list" => docs::list(self),
            "doc.info" => docs::info(self, params),
            "doc.setActive" => docs::set_active(self, params),
            "doc.resizeCanvas" => docs::resize_canvas(self, plugin, params),
            "doc.open" => docs::open(self, params),
            "layer.tree" => layers::tree(self, params),
            "layer.active" => layers::active(self, params),
            "layer.setActive" => layers::set_active(self, params),
            "layer.create" => layers::create(self, plugin, params),
            "layer.update" => layers::update(self, plugin, params),
            "layer.move" => layers::move_layer(self, plugin, params),
            "layer.delete" => layers::delete(self, plugin, params),
            "layer.addMask" => layers::add_mask(self, plugin, params, payload),
            "layer.thumbnail" => layers::thumbnail(self, params),
            "pixels.read" => pixels::read(self, params),
            "pixels.write" => pixels::write(self, plugin, params, payload),
            "pixels.createLayer" => pixels::create_layer(self, plugin, params, payload),
            "selection.read" => pixels::selection_read(self, params),
            "selection.bounds" => pixels::selection_bounds_of(self, params),
            "selection.set" => pixels::selection_set(self, plugin, params, payload),
            "mask.read" => pixels::mask_read(self, params),
            "undo.begin" => undo::begin(self, plugin, params),
            "undo.end" => undo::end(self, plugin, params),
            "events.subscribe" => events::subscribe(self, plugin, params),
            "preview.show" => preview::show(self, plugin, params, payload),
            "preview.hide" => preview::hide(self, params),
            "blob.get" => storage::blob_get(self, plugin, params),
            "blob.set" => storage::blob_set(self, plugin, params, payload),
            "blob.delete" => storage::blob_delete(self, plugin, params),
            "blob.list" => storage::blob_list(self, plugin, params),
            "layerData.get" => storage::layer_data_get(self, plugin, params),
            "layerData.set" => storage::layer_data_set(self, plugin, params),
            other => Err(ErrorBody::new(ErrorCode::Unsupported, format!("PhotoCraft does not support '{other}'"))),
        }
    }

    /// A plug-in disconnected (stopped, crashed or removed): close the undo groups it left open,
    /// so the next edit records a step of its own, and drop its previews and event subscriptions.
    pub fn plugin_disconnected(&mut self, plugin: &str) {
        undo::close_all(self, plugin);
        self.drop_plugin_previews(plugin);
        self.process_plugin.subscriptions.remove(plugin);
        if self.process_plugin.subscriptions.is_empty() {
            self.process_plugin.watch = None;
        }
    }

    /// Index of the open document with `id`.
    fn plugin_doc_index(&self, id: &str) -> Result<usize, ErrorBody> {
        let raw: u64 = id.parse().map_err(|_| ErrorBody::new(ErrorCode::Invalid, format!("'{id}' is not a document id")))?;
        self.docs.iter().position(|st| st.doc.id == DocId(raw)).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("no open document '{id}'")))
    }

    /// Run `f` with document `id` as the active one, then restore the active document. The
    /// window isn't told about the switch: it is an implementation detail of editing a
    /// background document, not something the user did.
    fn with_plugin_doc<R>(&mut self, id: &str, f: impl FnOnce(&mut Session) -> Result<R, ErrorBody>) -> Result<R, ErrorBody> {
        let index = self.plugin_doc_index(id)?;
        let previous = self.active;
        self.active = Some(index);
        let result = f(self);
        self.active = previous;
        result
    }

    /// Apply an undoable edit to document `doc` on behalf of `plugin`, as one history step
    /// labelled `label`, or as part of the plug-in's open undo group on that document.
    fn plugin_edit<R>(
        &mut self,
        plugin: &str,
        doc: &str,
        label: &str,
        f: impl FnOnce(&mut Document, &mut Option<LayerId>) -> Result<R, ErrorBody>,
    ) -> Result<R, ErrorBody> {
        let doc_id =
            self.plugin_doc_index(doc).map(|i| self.docs.get(i).map(|st| st.doc.id))?.ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, "no such document"))?;
        let (key, label) = match undo::open_group(self, plugin, doc_id) {
            Some((key, group_label)) => (Some(key), group_label),
            None => (None, label.to_string()),
        };
        self.with_plugin_doc(doc, |s| {
            let previous = std::mem::replace(&mut s.coalesce_request, key);
            let mut failure: Option<ErrorBody> = None;
            let result = s.edit(&label, |d, active| {
                f(d, active).map_err(|e| {
                    let message = e.message.clone();
                    failure = Some(e);
                    EngineError::Other(message)
                })
            });
            s.coalesce_request = previous;
            match result {
                Ok(r) => {
                    if let Some(revision) = s.active.and_then(|i| s.docs.get(i)).map(|st| st.revision) {
                        s.process_plugin.origins.insert(doc_id, (revision, plugin.to_string()));
                    }
                    Ok(r)
                }
                Err(e) => Err(failure.unwrap_or_else(|| engine_error(e))),
            }
        })
    }
}

/// Map an engine error to the protocol's error codes.
pub(crate) fn engine_error(e: EngineError) -> ErrorBody {
    let code = match &e {
        EngineError::NoDocument | EngineError::NoLayer(_) | EngineError::UnknownLayer { .. } => ErrorCode::NotFound,
        EngineError::BadParams { .. } => ErrorCode::Invalid,
        EngineError::UnknownCommand(_) => ErrorCode::Unsupported,
        EngineError::Disabled(..) => ErrorCode::Busy,
        EngineError::Cancelled => ErrorCode::Cancelled,
        _ => ErrorCode::Internal,
    };
    ErrorBody::new(code, e.to_string())
}

/// Decode a request's parameters, naming the method in the error.
pub(crate) fn params<T: serde::de::DeserializeOwned>(method: &str, params: &Value) -> Result<T, ErrorBody> {
    photocraft_process_plugin_protocol::message::decode_value(params, ErrorCode::Invalid)
        .map_err(|e| ErrorBody::new(ErrorCode::Invalid, format!("{method}: {}", e.message)))
}

/// A JSON result with no payload.
pub(crate) fn json<T: serde::Serialize>(value: &T) -> Outcome {
    serde_json::to_value(value).map(|v| (v, Vec::new())).map_err(|e| ErrorBody::new(ErrorCode::Internal, e.to_string()))
}

/// The empty success result.
pub(crate) fn ok() -> Outcome {
    Ok((Value::Object(serde_json::Map::new()), Vec::new()))
}
