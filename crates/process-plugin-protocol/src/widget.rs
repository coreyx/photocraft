//! Widget trees: how a plug-in describes UI that the host draws natively.
//!
//! A panel or dialog is a tree of [`Node`]s. The plug-in sends the whole tree once, then small
//! [`Patch`] operations as it changes. The host keeps its own copy and draws it every frame with
//! its own widgets and theme, so a slow plug-in never stalls the UI; user input comes back as
//! `panel.input` / `dialog.input` events.
//!
//! Each node's `props` is a JSON object whose meaning depends on its [`WidgetKind`]. Common
//! props: `text`, `tooltip`, `enabled` (default true), `visible` (default true), `icon` (an
//! `"image:<hash>"` or `"icon:<name>"` reference), `grow` (share of spare space, default 0).
//! The per-kind props are listed in `docs/process-plugins.md`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Deepest widget tree the host accepts; deeper trees are refused instead of recursed into.
pub const MAX_DEPTH: usize = 64;

/// Most nodes the host accepts in one panel or dialog.
pub const MAX_NODES: usize = 20_000;

/// One widget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    /// Unique within its panel or dialog; input events and patches refer to it.
    pub id: String,
    pub kind: WidgetKind,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub props: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

impl Node {
    /// A node with no props and no children.
    pub fn new(id: impl Into<String>, kind: WidgetKind) -> Self {
        Node { id: id.into(), kind, props: Map::new(), children: Vec::new() }
    }

    /// Builder: set one prop.
    pub fn prop(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.props.insert(key.to_owned(), value.into());
        self
    }

    /// Builder: append a child.
    pub fn child(mut self, child: Node) -> Self {
        self.children.push(child);
        self
    }

    /// Check that the tree is within [`MAX_DEPTH`] and [`MAX_NODES`] and that ids are unique.
    /// Returns a description of the first problem found.
    pub fn validate(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        let mut stack: Vec<(&Node, usize)> = vec![(self, 1)];
        while let Some((node, depth)) = stack.pop() {
            if depth > MAX_DEPTH {
                return Err(format!("widget tree deeper than {MAX_DEPTH} levels at '{}'", node.id));
            }
            if !seen.insert(node.id.as_str()) {
                return Err(format!("duplicate widget id '{}'", node.id));
            }
            if seen.len() > MAX_NODES {
                return Err(format!("widget tree has more than {MAX_NODES} nodes"));
            }
            stack.extend(node.children.iter().map(|c| (c, depth + 1)));
        }
        Ok(())
    }
}

/// Every kind of widget the host can draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WidgetKind {
    // layout
    /// Children left to right.
    Row,
    /// Children top to bottom.
    Column,
    /// Children in a grid; each child's `row`/`col` (and `rowSpan`/`colSpan`) props place it.
    Grid,
    /// Label/field pairs; children alternate label, field.
    Form,
    /// A titled frame around its children (`title`, optional `checkable`/`checked`).
    Group,
    /// A header that shows or hides its children (`title`, `expanded`).
    Collapsible,
    /// Tabs; each child is a page whose `title` prop labels its tab (`current` = index).
    Tabs,
    /// Shows one child at a time (`current` = index).
    Stack,
    /// Scrolls its single child.
    Scroll,
    /// Children separated by draggable dividers (`orientation`, `sizes`).
    Splitter,
    /// Empty space (`size`, or `grow` to stretch).
    Spacer,
    // display
    /// Text (`text`, `format`: "plain" | "rich" | "markdown", `wrap`, `elide`).
    Label,
    /// An icon (`icon`, `size`).
    Icon,
    /// An image (`image`, `width`, `height`, `fit`); reports pointer input when `interactive`.
    Image,
    /// A progress bar (`value` 0..1 or null for busy, `text`).
    Progress,
    /// A horizontal or vertical line (`orientation`).
    Separator,
    // input
    /// A push button (`text`, `icon`, `default`).
    Button,
    /// A flat button for toolbars (`text`, `icon`, `checkable`, `checked`, `menu`).
    ToolButton,
    /// A button that stays pressed (`checked`).
    ToggleButton,
    /// A button with a main action and a drop-down menu (`text`, `menu`).
    SplitButton,
    /// A check box (`text`, `checked`, `tristate`).
    Checkbox,
    /// An on/off switch (`checked`, `text`).
    Switch,
    /// Mutually exclusive options (`items`: [{id, text}], `value`).
    RadioGroup,
    /// A drop-down (`items`: [{id, text, icon, kind: "item"|"separator"|"header"}], `value`).
    Combo,
    /// A slider (`min`, `max`, `step`, `value`, `float`).
    Slider,
    /// A number box (`min`, `max`, `step`, `value`, `decimals`, `prefix`, `suffix`).
    Spin,
    /// A slider and number box in one (`min`, `max`, `step`, `value`, `decimals`, `suffix`).
    SliderSpin,
    /// A single-line text field (`text`, `placeholder`, `password`).
    LineEdit,
    /// Multi-line text (`text`, `placeholder`, `readOnly`, `spans` for highlighting,
    /// `completions` for a pop-up list, `minLines`, `maxLines`, `resizable`).
    TextEdit,
    /// A path field with a browse button (`text`, `mode`: "open" | "save" | "folder", `filters`).
    PathField,
    // collections
    /// A list (`items`: [{id, text, icon, tooltip}], `selected`, `selectionMode`, `reorderable`,
    /// `contextMenu`).
    List,
    /// A grid of thumbnails (`items`: [{id, image, overlay, tooltip}], `selected`,
    /// `selectionMode`, `thumbSize`, `contextMenu`).
    ThumbGrid,
    /// A vertical list of child rows that are themselves widget trees (regions, control layers).
    Repeater,
    // overlay
    /// A pop-up anchored to another widget (`anchor` = node id, `open`).
    Popup,
    /// A menu (`items`: [{id, text, icon, checkable, checked, enabled, items}]); shown by
    /// buttons that reference it or as a context menu.
    Menu,
}

/// One change to a widget tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum Patch {
    /// Replace the whole tree.
    Replace { node: Node },
    /// Merge props into a node; a `null` value removes the prop.
    Set { id: String, props: Map<String, Value> },
    /// Insert a subtree as the `index`-th child of `parent` (clamped to the end).
    Insert { parent: String, index: usize, node: Node },
    /// Remove a node and its subtree.
    Remove { id: String },
    /// Move a node to be the `index`-th child of `parent`.
    Move { id: String, parent: String, index: usize },
}
