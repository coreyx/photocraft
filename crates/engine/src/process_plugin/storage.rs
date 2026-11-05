//! `blob.*` and `layerData.*`: data process plug-ins keep in documents.
//!
//! Each plug-in sees only its own entries: names are stored as `<plugin-id>/<name>`
//! ([`photocraft_doc::Document::plugin_data`], [`photocraft_doc::Layer::plugin_data`]).
//!
//! Storing data is not an edit the user can undo: it changes no pixels and takes no history
//! step, and undoing other edits keeps it (see `carry_outside_history`). It does mark the
//! document modified, so the data is saved.

use std::sync::Arc;

use photocraft_doc::Document;
use photocraft_doc::plugin_data::{MAX_DOCUMENT_BYTES, MAX_LAYER_BYTES, MAX_NAME, document_bytes, layer_bytes};
use photocraft_process_plugin_protocol::message::{ErrorBody, ErrorCode};
use photocraft_process_plugin_protocol::method::{BlobFound, BlobNames, BlobParams, DocRef, LayerDataParams, LayerDataValue};
use serde_json::Value;

use super::layers::find;
use super::{Outcome, json, ok, params};
use crate::Session;

fn check_name(what: &str, name: &str) -> Result<(), ErrorBody> {
    if name.is_empty() || name.len() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(ErrorBody::new(ErrorCode::Invalid, format!("a {what} must be 1 to {MAX_NAME} bytes without control characters")));
    }
    Ok(())
}

fn key(plugin: &str, name: &str) -> String {
    format!("{plugin}/{name}")
}

/// Change document `doc` outside the undo history with `f`, which has already been checked
/// to succeed. The document becomes modified; the revision moves as a view change, so nothing
/// recomposites and no pixel change is reported.
fn change(s: &mut Session, doc: &str, f: impl FnOnce(&mut Document)) -> Result<(), ErrorBody> {
    let index = s.plugin_doc_index(doc)?;
    let st = s.docs.get_mut(index).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, "no such document"))?;
    f(Arc::make_mut(&mut st.doc));
    st.revision += 1;
    st.view_revision += 1;
    st.last_damage = Some(photocraft_geom::Rect::EMPTY);
    Ok(())
}

fn document(s: &Session, doc: &str) -> Result<Arc<Document>, ErrorBody> {
    let index = s.plugin_doc_index(doc)?;
    s.docs.get(index).map(|st| st.doc.clone()).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, "no such document"))
}

pub(super) fn blob_get(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: BlobParams = params("blob.get", p)?;
    check_name("blob name", &p.name)?;
    let doc = document(s, &p.doc)?;
    match doc.plugin_data.get(&key(plugin, &p.name)) {
        Some(bytes) => json(&BlobFound { found: true }).map(|(v, _)| (v, bytes.to_vec())),
        None => json(&BlobFound { found: false }),
    }
}

pub(super) fn blob_set(s: &mut Session, plugin: &str, p: &Value, payload: &[u8]) -> Outcome {
    let p: BlobParams = params("blob.set", p)?;
    check_name("blob name", &p.name)?;
    let doc = document(s, &p.doc)?;
    let key = key(plugin, &p.name);
    let replaced = doc.plugin_data.get(&key).map_or(0, |b| b.len() as u64);
    let total = document_bytes(&doc.plugin_data) - replaced + payload.len() as u64;
    if total > MAX_DOCUMENT_BYTES {
        return Err(ErrorBody::new(ErrorCode::TooLarge, format!("the document's plug-in data would be {total} bytes, more than {MAX_DOCUMENT_BYTES}")));
    }
    if doc.plugin_data.get(&key).is_some_and(|b| b.as_slice() == payload) {
        return ok();
    }
    let bytes = Arc::new(payload.to_vec());
    change(s, &p.doc, |d| {
        d.plugin_data.insert(key, bytes);
    })?;
    ok()
}

pub(super) fn blob_delete(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: BlobParams = params("blob.delete", p)?;
    check_name("blob name", &p.name)?;
    let key = key(plugin, &p.name);
    if document(s, &p.doc)?.plugin_data.contains_key(&key) {
        change(s, &p.doc, |d| {
            d.plugin_data.remove(&key);
        })?;
    }
    ok()
}

pub(super) fn blob_list(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: DocRef = params("blob.list", p)?;
    let prefix = key(plugin, "");
    let names = document(s, &p.doc)?.plugin_data.keys().filter_map(|k| k.strip_prefix(&prefix)).map(str::to_string).collect();
    json(&BlobNames { names })
}

pub(super) fn layer_data_get(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: LayerDataParams = params("layerData.get", p)?;
    check_name("key", &p.key)?;
    let doc = document(s, &p.doc)?;
    let id = find(&doc, &p.uid)?;
    let value = doc.layer(id).and_then(|l| l.plugin_data.get(&key(plugin, &p.key)).cloned());
    json(&LayerDataValue { value })
}

pub(super) fn layer_data_set(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: LayerDataParams = params("layerData.set", p)?;
    check_name("key", &p.key)?;
    let doc = document(s, &p.doc)?;
    let id = find(&doc, &p.uid)?;
    let layer = doc.layer(id).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("no layer '{}'", p.uid)))?;
    let key = key(plugin, &p.key);
    if layer.plugin_data.get(&key) == p.value.as_ref() {
        return ok();
    }
    let mut data = layer.plugin_data.clone();
    match p.value {
        Some(value) => data.insert(key, value),
        None => data.remove(&key),
    };
    if layer_bytes(&data) > MAX_LAYER_BYTES {
        return Err(ErrorBody::new(ErrorCode::TooLarge, format!("a layer holds at most {MAX_LAYER_BYTES} bytes of plug-in values")));
    }
    change(s, &p.doc, |d| {
        if let Some(layer) = d.layer_mut(id) {
            layer.plugin_data = data;
        }
    })?;
    ok()
}
