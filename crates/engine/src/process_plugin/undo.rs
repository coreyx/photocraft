//! `undo.begin` / `undo.end`: grouping a plug-in's edits into one history step.
//!
//! A group is the session's coalescing key (the same mechanism that makes one step of a slider
//! drag): every plug-in edit on the group's document while the group is open carries the key, so
//! consecutive edits share the first one's history step, labelled with the group's label. An edit
//! by the user in between ends the coalescing, as it should: the user's step stays its own.

use photocraft_doc::DocId;
use photocraft_process_plugin_protocol::message::{ErrorBody, ErrorCode};
use photocraft_process_plugin_protocol::method::{UndoBeginParams, UndoGroup};
use serde_json::Value;

use super::{Outcome, ok, params};
use crate::Session;

/// Most undo groups one session keeps open at a time; a plug-in that never ends its groups can't
/// grow the table without bound.
const MAX_OPEN_GROUPS: usize = 64;

/// An open undo group.
pub(super) struct Group {
    plugin: String,
    doc: DocId,
    label: String,
}

pub(super) fn begin(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: UndoBeginParams = params("undo.begin", p)?;
    let index = s.plugin_doc_index(&p.doc)?;
    let doc = s.docs.get(index).map(|st| st.doc.id).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, "no such document"))?;
    if p.label.trim().is_empty() || p.label.len() > 200 {
        return Err(ErrorBody::new(ErrorCode::Invalid, "the undo label must be 1 to 200 characters"));
    }
    if s.process_plugin.groups.len() >= MAX_OPEN_GROUPS {
        return Err(ErrorBody::new(ErrorCode::TooLarge, format!("more than {MAX_OPEN_GROUPS} undo groups are open")));
    }
    // A new group on a document ends the plug-in's previous one there.
    s.process_plugin.groups.retain(|_, g| !(g.plugin == plugin && g.doc == doc));
    s.process_plugin.next_group = s.process_plugin.next_group.wrapping_add(1);
    let id = s.process_plugin.next_group;
    s.process_plugin.groups.insert(id, Group { plugin: plugin.to_string(), doc, label: p.label });
    super::json(&UndoGroup { group: id })
}

pub(super) fn end(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: UndoGroup = params("undo.end", p)?;
    match s.process_plugin.groups.get(&p.group) {
        Some(g) if g.plugin == plugin => {
            s.process_plugin.groups.remove(&p.group);
            // The next edit records a step of its own, even with the same key.
            for st in &mut s.docs {
                if st.coalesce.as_deref().is_some_and(|k| k == key(p.group)) {
                    st.coalesce = None;
                }
            }
            ok()
        }
        _ => Err(ErrorBody::new(ErrorCode::NotFound, format!("no open undo group {}", p.group))),
    }
}

/// The coalescing key and label of the plug-in's open group on `doc`, if any.
pub(super) fn open_group(s: &Session, plugin: &str, doc: DocId) -> Option<(String, String)> {
    s.process_plugin.groups.iter().find(|(_, g)| g.plugin == plugin && g.doc == doc).map(|(id, g)| (key(*id), g.label.clone()))
}

/// Close every group a plug-in left open.
pub(super) fn close_all(s: &mut Session, plugin: &str) {
    let ids: Vec<u64> = s.process_plugin.groups.iter().filter(|(_, g)| g.plugin == plugin).map(|(id, _)| *id).collect();
    for id in ids {
        s.process_plugin.groups.remove(&id);
        for st in &mut s.docs {
            if st.coalesce.as_deref() == Some(key(id).as_str()) {
                st.coalesce = None;
            }
        }
    }
}

fn key(group: u64) -> String {
    format!("process-plugin-undo-group:{group}")
}
