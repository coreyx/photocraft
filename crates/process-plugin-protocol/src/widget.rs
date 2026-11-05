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

/// A panel holding one widget of every [`WidgetKind`], with the props each kind reads: what the
/// host's renderer tests, snapshots and the echo test plug-in draw, and a worked example of
/// the vocabulary.
pub fn gallery() -> Node {
    use WidgetKind as K;
    use serde_json::json;
    let n = Node::new;
    let menu =
        n("menu", K::Menu).prop("items", json!([{"id": "copy", "text": "Copy"}, {"id": "more", "text": "More", "items": [{"id": "deep", "text": "Deeper"}]}]));
    let combo_items = json!([
        {"kind": "header", "text": "Models"},
        {"id": "sdxl", "text": "SDXL"},
        {"id": "flux", "text": "Flux"},
        {"kind": "separator"},
        {"id": "custom", "text": "Custom…"}
    ]);
    let list_items = json!([{"id": "a", "text": "Region A", "tooltip": "First"}, {"id": "b", "text": "Region B"}, {"id": "c", "text": "Region C"}]);
    let thumbs = json!([{"id": "r1", "image": "image:thumb", "overlay": "1"}, {"id": "r2", "image": "image:thumb", "overlay": "2"}]);
    let spans = json!([{"start": 0, "end": 4, "color": "#d08770"}, {"start": 10, "end": 14, "color": "#88c0d0", "italic": true}]);
    n("root", K::Column).child(menu).child(
        n("tabs", K::Tabs)
            .prop("current", 0)
            .child(
                n("page-inputs", K::Column)
                    .prop("title", "Inputs")
                    .child(
                        n("buttons", K::Row)
                            .child(n("button", K::Button).prop("text", "Generate").prop("default", true).prop("tooltip", "Generate an image"))
                            .child(n("tool", K::ToolButton).prop("text", "Tools").prop("menu", "menu"))
                            .child(n("toggle", K::ToggleButton).prop("text", "Live"))
                            .child(n("split", K::SplitButton).prop("text", "Queue").prop("menu", json!([{"id": "front", "text": "Front of queue"}]))),
                    )
                    .child(
                        n("form", K::Form)
                            .child(n("strength-label", K::Label).prop("text", "Strength"))
                            .child(n("strength", K::SliderSpin).prop("min", 0).prop("max", 100).prop("value", 60).prop("suffix", "%"))
                            .child(n("steps-label", K::Label).prop("text", "Steps"))
                            .child(n("steps", K::Spin).prop("min", 1).prop("max", 100).prop("value", 20))
                            .child(n("cfg-label", K::Label).prop("text", "Guidance"))
                            .child(n("cfg", K::Slider).prop("min", 1).prop("max", 20).prop("value", 7).prop("float", true))
                            .child(n("model-label", K::Label).prop("text", "Model"))
                            .child(n("model", K::Combo).prop("items", combo_items).prop("value", "sdxl")),
                    )
                    .child(n("check", K::Checkbox).prop("text", "Seed fixed"))
                    .child(n("switch", K::Switch).prop("text", "Advanced"))
                    .child(
                        n("radio", K::RadioGroup).prop("items", json!([{"id": "fast", "text": "Fast"}, {"id": "best", "text": "Best"}])).prop("value", "fast"),
                    )
                    .child(n("line", K::LineEdit).prop("placeholder", "Negative prompt"))
                    .child(
                        n("prompt", K::TextEdit)
                            .prop("text", "cat, sitting on a mat")
                            .prop("spans", spans)
                            .prop("minLines", 2)
                            .prop("maxLines", 6)
                            .prop("resizable", true),
                    )
                    .child(n("path", K::PathField).prop("text", "").prop("mode", "folder")),
            )
            .child(
                n("page-display", K::Column)
                    .prop("title", "Display")
                    .child(n("label", K::Label).prop("text", "Plain label").prop("wrap", true))
                    .child(n("rich", K::Label).prop("format", "rich").prop("text", "<b>Bold</b> and <a href=\"https://example.com\">a link</a>"))
                    .child(n("icon", K::Icon).prop("icon", "icon:lock").prop("size", 16))
                    .child(n("image", K::Image).prop("image", "image:thumb").prop("width", 64).prop("height", 64).prop("interactive", true))
                    .child(n("progress", K::Progress).prop("value", 0.4).prop("text", "40%"))
                    .child(n("busy", K::Progress))
                    .child(n("sep", K::Separator)),
            )
            .child(
                n("page-layout", K::Column)
                    .prop("title", "Layout")
                    .child(
                        n("grid", K::Grid)
                            .child(n("g00", K::Label).prop("text", "A").prop("row", 0).prop("col", 0))
                            .child(n("g01", K::Label).prop("text", "B").prop("row", 0).prop("col", 1))
                            .child(n("g10", K::Label).prop("text", "C").prop("row", 1).prop("col", 0)),
                    )
                    .child(n("group", K::Group).prop("title", "Group").child(n("in-group", K::Label).prop("text", "Inside")))
                    .child(
                        n("collapsible", K::Collapsible)
                            .prop("title", "History")
                            .prop("expanded", true)
                            .child(n("in-collapse", K::Label).prop("text", "Hidden")),
                    )
                    .child(
                        n("stack", K::Stack).prop("current", 1).child(n("s0", K::Label).prop("text", "Page 0")).child(n("s1", K::Label).prop("text", "Page 1")),
                    )
                    .child(n("scroll", K::Scroll).prop("maxHeight", 60).child(n("in-scroll", K::Label).prop("text", "Scrolled")))
                    .child(
                        n("splitter", K::Splitter)
                            .child(n("pane-a", K::Label).prop("text", "Top pane"))
                            .child(n("pane-b", K::Label).prop("text", "Bottom pane")),
                    )
                    .child(n("spacer", K::Spacer).prop("size", 8))
                    .child(n("popup-anchor", K::Button).prop("text", "Options"))
                    .child(n("popup", K::Popup).prop("anchor", "popup-anchor").prop("open", false).child(n("in-popup", K::Label).prop("text", "Pop-up"))),
            )
            .child(
                n("page-collections", K::Column)
                    .prop("title", "Collections")
                    .child(n("list", K::List).prop("items", list_items).prop("selectionMode", "multi").prop("reorderable", true).prop("contextMenu", "menu"))
                    .child(n("thumbs", K::ThumbGrid).prop("items", thumbs).prop("thumbSize", 48).prop("contextMenu", "menu"))
                    .child(
                        n("repeater", K::Repeater)
                            .child(n("row-1", K::Row).child(n("row-1-label", K::Label).prop("text", "Region 1")))
                            .child(n("row-2", K::Row).child(n("row-2-label", K::Label).prop("text", "Region 2"))),
                    ),
            ),
    )
}
