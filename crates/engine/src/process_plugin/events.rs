//! Change events: telling subscribed plug-ins what changed in the open documents.
//!
//! Nothing in the ~700 commands reports changes. Instead, the shell calls
//! [`Session::plugin_events`] once per frame, and it compares each open document with the
//! snapshot it took last time:
//! - the document `Arc` (unchanged means nothing in the document changed, so the comparison
//!   costs nothing for documents nobody is editing);
//! - a digest of the layer tree as plug-ins see it (uid, nesting, order, name, kind,
//!   visibility, lock, opacity, blend);
//! - a fingerprint per layer of its content: the identity of its pixel tiles, which are
//!   copy-on-write, so any edit gives the touched tiles new identities;
//! - the selection's fingerprint, the active layer, the name, path and save count, and the
//!   active document.
//!
//! Layer bounds are left out of the tree digest: they grow with nearly every brush stroke, and a
//! stroke is a pixel change, not a tree change. `layers.changed` still carries the bounds as they
//! are when it is sent.
//!
//! One frame produces at most one event per kind per document, however many edits it held.
//! Changes made by a plug-in's own request carry the plug-in as their `origin`.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use photocraft_doc::{DocId, Document, Layer, LayerContent};
use photocraft_process_plugin_protocol::event::{self, DocumentEvent, LayerActivated, LayersChanged, PixelsChanged, SelectionChanged};
use photocraft_process_plugin_protocol::message::{ErrorBody, ErrorCode};
use photocraft_process_plugin_protocol::method::SubscribeParams;
use photocraft_process_plugin_protocol::types::Rect as WireRect;
use photocraft_raster::Surface;
use serde_json::Value;

use super::{Outcome, ok, params};
use crate::Session;

/// The document topics a plug-in can subscribe to; the other topics (panel input, actions …)
/// go to the plug-in they concern without a subscription.
const DOCUMENT_TOPICS: &[&str] = &[
    event::DOCUMENT_OPENED,
    event::DOCUMENT_CLOSED,
    event::DOCUMENT_SAVED,
    event::DOCUMENT_RENAMED,
    event::DOCUMENT_ACTIVATED,
    event::LAYERS_CHANGED,
    event::PIXELS_CHANGED,
    event::SELECTION_CHANGED,
    event::LAYER_ACTIVATED,
];

/// One event for one plug-in.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginEvent {
    pub plugin: String,
    pub topic: &'static str,
    pub data: Value,
}

/// What a document looked like at the last comparison.
#[derive(Clone)]
struct DocSnap {
    /// Held so the tiles the fingerprints point at stay alive: a freed tile's address can't be
    /// reused by a new one and fake "unchanged".
    doc: Arc<Document>,
    revision: u64,
    content_revision: u64,
    saves: u64,
    path: Option<String>,
    tree: u64,
    selection: u64,
    active_layer: Option<u128>,
    layers: HashMap<u128, u64>,
}

/// The snapshots of every open document and which one was active.
#[derive(Clone, Default)]
pub(super) struct Watch {
    docs: HashMap<DocId, DocSnap>,
    active: Option<DocId>,
}

fn digest(value: &impl Hash) -> u64 {
    let mut h = DefaultHasher::new();
    value.hash(&mut h);
    h.finish()
}

/// Identity of a surface's content: its default pixel and every tile's position and identity,
/// combined order-independently (tile maps don't iterate in a fixed order).
fn surface_print(s: &Surface) -> u64 {
    let base = digest(&s.default_pixel().iter().map(|v| v.to_bits()).collect::<Vec<_>>());
    s.tiles().fold(base, |acc, (c, t)| acc.wrapping_add(digest(&(c.tx, c.ty, Arc::as_ptr(t) as usize))))
}

/// Identity of a layer's content and mask.
fn layer_print(l: &Layer) -> u64 {
    let content = match &l.content {
        LayerContent::Group(_) => 0,
        LayerContent::Raster(_) | LayerContent::Text(_) | LayerContent::Shape(_) | LayerContent::Smart(_) => l.surface().map_or(1, surface_print),
        // Fill and adjustment layers are small descriptions without tiles.
        other => digest(&format!("{other:?}")),
    };
    let mask = l.mask.as_ref().map_or(0, |m| surface_print(&m.surface) ^ u64::from(m.enabled));
    content.wrapping_mul(31).wrapping_add(mask)
}

fn clear_bounds(tree: &mut [photocraft_process_plugin_protocol::types::LayerInfo]) {
    for layer in tree {
        layer.bounds = WireRect::default();
        clear_bounds(&mut layer.children);
    }
}

fn snap(st: &crate::DocState, previous: Option<&DocSnap>) -> DocSnap {
    let doc = &st.doc;
    let active_layer = st.active_layer.and_then(|id| doc.layer(id)).map(|l| l.uid);
    let base = DocSnap {
        doc: doc.clone(),
        revision: st.revision,
        content_revision: st.revision.saturating_sub(st.view_revision),
        saves: st.saves,
        path: st.path.clone(),
        tree: 0,
        selection: 0,
        active_layer,
        layers: HashMap::new(),
    };
    if let Some(p) = previous.filter(|p| Arc::ptr_eq(&p.doc, doc)) {
        return DocSnap { tree: p.tree, selection: p.selection, layers: p.layers.clone(), ..base };
    }
    let mut tree = super::layers::tree_of(doc);
    clear_bounds(&mut tree);
    let tree = serde_json::to_string(&tree).map(|s| digest(&s)).unwrap_or_default();
    let selection = doc.selection.as_ref().map_or(0, surface_print);
    let layers = doc.walk().into_iter().map(|(_, _, l)| (l.uid, layer_print(l))).collect();
    DocSnap { tree, selection, layers, ..base }
}

impl Watch {
    fn of(s: &Session, previous: Option<&Watch>) -> Watch {
        let docs = s.docs.iter().map(|st| (st.doc.id, snap(st, previous.and_then(|w| w.docs.get(&st.doc.id))))).collect();
        Watch { docs, active: s.active.and_then(|i| s.docs.get(i)).map(|st| st.doc.id) }
    }
}

fn wire(r: photocraft_geom::Rect) -> WireRect {
    WireRect::new(i64::from(r.x0), i64::from(r.y0), r.width(), r.height())
}

fn doc_event(id: DocId, snap: &DocSnap) -> DocumentEvent {
    DocumentEvent { doc: id.0.to_string(), name: snap.doc.name.clone(), path: snap.path.clone() }
}

impl Session {
    /// The events every subscribed plug-in should get for what changed since the last call.
    /// The shell calls this once per frame, after the frame's edits.
    pub fn plugin_events(&mut self) -> Vec<PluginEvent> {
        if self.process_plugin.subscriptions.is_empty() {
            return Vec::new();
        }
        let previous = self.process_plugin.watch.take().unwrap_or_else(|| Watch::of(self, None));
        let now = Watch::of(self, Some(&previous));
        let mut changes: Vec<(&'static str, Value)> = Vec::new();
        let mut push = |topic: &'static str, data: Result<Value, serde_json::Error>| {
            if let Ok(data) = data {
                changes.push((topic, data));
            }
        };
        for (id, old) in &previous.docs {
            if !now.docs.contains_key(id) {
                push(event::DOCUMENT_CLOSED, serde_json::to_value(doc_event(*id, old)));
            }
        }
        let mut ids: Vec<&DocId> = now.docs.keys().collect();
        ids.sort_by_key(|id| id.0);
        for id in ids {
            let Some(new) = now.docs.get(id) else { continue };
            let doc = id.0.to_string();
            let Some(old) = previous.docs.get(id) else {
                push(event::DOCUMENT_OPENED, serde_json::to_value(doc_event(*id, new)));
                continue;
            };
            if old.doc.name != new.doc.name || old.path != new.path {
                push(event::DOCUMENT_RENAMED, serde_json::to_value(doc_event(*id, new)));
            }
            if new.saves != old.saves {
                push(event::DOCUMENT_SAVED, serde_json::to_value(doc_event(*id, new)));
            }
            if new.tree != old.tree {
                let tree = super::layers::tree_of(&new.doc);
                push(event::LAYERS_CHANGED, serde_json::to_value(LayersChanged { doc: doc.clone(), revision: new.revision, tree }));
            }
            let mut changed: Vec<u128> =
                new.layers.iter().filter(|(uid, print)| old.layers.get(uid).is_some_and(|o| o != *print)).map(|(uid, _)| *uid).collect();
            // A content change no fingerprint explains (a document-wide edit such as a canvas
            // size change) is still reported, without layers.
            let unexplained = new.content_revision != old.content_revision && new.tree == old.tree && new.selection == old.selection;
            if !changed.is_empty() || (unexplained && !Arc::ptr_eq(&old.doc, &new.doc)) {
                changed.sort_unstable();
                let st = self.docs.iter().find(|st| st.doc.id == *id);
                // The latest edit's damage covers the frame only when the frame held one edit.
                let bounds = st.filter(|_| new.content_revision == old.content_revision + 1).and_then(|st| st.last_damage).map(wire);
                let origin = self.process_plugin.origins.get(id).filter(|(revision, _)| *revision == new.revision).map(|(_, plugin)| plugin.clone());
                let layers = changed.into_iter().map(photocraft_doc::uid_string).collect();
                push(event::PIXELS_CHANGED, serde_json::to_value(PixelsChanged { doc: doc.clone(), revision: new.revision, layers, bounds, origin }));
            }
            if new.selection != old.selection {
                let bounds = super::pixels::selection_bounds(&new.doc).map(wire);
                push(event::SELECTION_CHANGED, serde_json::to_value(SelectionChanged { doc: doc.clone(), bounds }));
            }
            if new.active_layer != old.active_layer {
                push(event::LAYER_ACTIVATED, serde_json::to_value(LayerActivated { doc: doc.clone(), uid: new.active_layer.map(photocraft_doc::uid_string) }));
            }
        }
        if now.active != previous.active
            && let Some((id, snap)) = now.active.and_then(|id| now.docs.get(&id).map(|s| (id, s)))
        {
            push(event::DOCUMENT_ACTIVATED, serde_json::to_value(doc_event(id, snap)));
        }
        self.process_plugin.origins.retain(|id, _| now.docs.contains_key(id));
        self.process_plugin.watch = Some(now);
        let mut plugins: Vec<&String> = self.process_plugin.subscriptions.keys().collect();
        plugins.sort();
        changes
            .iter()
            .flat_map(|(topic, data)| {
                plugins.iter().filter(|p| self.process_plugin.subscriptions.get(**p).is_some_and(|t| t.contains(*topic))).map(|p| PluginEvent {
                    plugin: (*p).clone(),
                    topic,
                    data: data.clone(),
                })
            })
            .collect()
    }
}

/// `events.subscribe`: replace the plug-in's document topics (an empty list unsubscribes).
pub(super) fn subscribe(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: SubscribeParams = params("events.subscribe", p)?;
    let mut topics = BTreeSet::new();
    for topic in p.topics {
        let known = DOCUMENT_TOPICS.iter().find(|t| **t == topic).copied();
        match known {
            Some(t) => {
                topics.insert(t);
            }
            None if event::ALL.contains(&topic.as_str()) => {}
            None => return Err(ErrorBody::new(ErrorCode::Invalid, format!("unknown event topic '{topic}'"))),
        }
    }
    if topics.is_empty() {
        s.process_plugin.subscriptions.remove(plugin);
        if s.process_plugin.subscriptions.is_empty() {
            s.process_plugin.watch = None;
        }
        return ok();
    }
    // The first subscriber starts the comparisons from the documents as they are now.
    if s.process_plugin.watch.is_none() {
        s.process_plugin.watch = Some(Watch::of(s, None));
    }
    s.process_plugin.subscriptions.insert(plugin.to_string(), topics);
    ok()
}
