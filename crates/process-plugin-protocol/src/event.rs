//! Events the host sends to plug-ins.
//!
//! Document events go only to plug-ins subscribed to their topic (`events.subscribe`); UI events
//! (`panel.input`, `dialog.*`, `action.triggered`, `progress.cancel`) always go to the plug-in
//! that owns the panel, dialog, action or task. The host coalesces document events, so a burst
//! of changes (a brush stroke) produces at most one event per topic per display frame.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{DocId, LayerInfo, LayerUid, Modifiers, Rect};

pub const DOCUMENT_OPENED: &str = "document.opened";
pub const DOCUMENT_CLOSED: &str = "document.closed";
pub const DOCUMENT_SAVED: &str = "document.saved";
pub const DOCUMENT_RENAMED: &str = "document.renamed";
pub const DOCUMENT_ACTIVATED: &str = "document.activated";
pub const LAYERS_CHANGED: &str = "layers.changed";
pub const PIXELS_CHANGED: &str = "pixels.changed";
pub const SELECTION_CHANGED: &str = "selection.changed";
pub const LAYER_ACTIVATED: &str = "layer.activated";
pub const PANEL_INPUT: &str = "panel.input";
pub const DIALOG_INPUT: &str = "dialog.input";
pub const DIALOG_CLOSED: &str = "dialog.closed";
pub const ACTION_TRIGGERED: &str = "action.triggered";
pub const PROGRESS_CANCEL: &str = "progress.cancel";
pub const LOCALE_CHANGED: &str = "locale.changed";

/// Every event topic.
pub const ALL: &[&str] = &[
    DOCUMENT_OPENED,
    DOCUMENT_CLOSED,
    DOCUMENT_SAVED,
    DOCUMENT_RENAMED,
    DOCUMENT_ACTIVATED,
    LAYERS_CHANGED,
    PIXELS_CHANGED,
    SELECTION_CHANGED,
    LAYER_ACTIVATED,
    PANEL_INPUT,
    DIALOG_INPUT,
    DIALOG_CLOSED,
    ACTION_TRIGGERED,
    PROGRESS_CANCEL,
    LOCALE_CHANGED,
];

/// `document.*` lifecycle events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentEvent {
    pub doc: DocId,
    /// The document's name after the event (for `document.renamed`, the new name).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The file path after the event, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// `layers.changed`: the layer tree or layer properties changed. Carries the whole tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayersChanged {
    pub doc: DocId,
    pub revision: u64,
    pub tree: Vec<LayerInfo>,
}

/// `pixels.changed`: layer content changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PixelsChanged {
    pub doc: DocId,
    pub revision: u64,
    /// The layers whose content changed.
    pub layers: Vec<LayerUid>,
    /// Union of the changed areas, when the host knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Rect>,
    /// The plug-in whose request caused the change, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// `selection.changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionChanged {
    pub doc: DocId,
    /// Tight bounds of the new selection, or `None` when nothing is selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Rect>,
}

/// `layer.activated`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerActivated {
    pub doc: DocId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<LayerUid>,
}

/// What the user did to a plug-in widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InputKind {
    /// A button, link or list item was activated (`value` is the click count).
    Click,
    /// A value changed: text, number, check state, combo index (`value` is the new value).
    Change,
    /// A key was pressed in a focused widget (`value` is the key name, e.g. `"Enter"`).
    Key,
    /// The selection in a list changed (`value` is the selected item ids).
    Select,
    /// A menu entry was chosen (`value` is the entry id).
    Menu,
    /// A context menu was requested (`value` is the item id under the pointer, if any).
    Context,
    /// List items were reordered by dragging (`value` is the new item id order).
    Drag,
    /// The text cursor or text selection moved (`value` is `{ "anchor": n, "cursor": n }`).
    Cursor,
    /// A pointer event on an image widget (`value` is `{ "x", "y", "button", "action" }`, in
    /// the image's own points; `action` is `"click"`, or `"press"`, `"move"` and `"release"` for
    /// a drag).
    Pointer,
    /// The widget gained or lost focus (`value` is a boolean).
    Focus,
}

/// `panel.input` and `dialog.input`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputEvent {
    /// The panel or dialog the widget belongs to.
    pub owner: String,
    pub node_id: String,
    pub kind: InputKind,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub modifiers: Modifiers,
}

/// `dialog.closed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DialogClosed {
    pub dialog_id: String,
}

/// `action.triggered`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionTriggered {
    pub id: String,
    /// The active document when the action was triggered, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<DocId>,
}

/// `progress.cancel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressCancel {
    pub task_id: String,
}

/// `locale.changed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocaleChanged {
    pub locale: String,
}
