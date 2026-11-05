//! Every request in the protocol, with its parameter and result types.
//!
//! Each method is a unit struct implementing [`Method`], which ties its wire name to its typed
//! parameters and result, so both sides encode and decode a request the same way:
//!
//! ```
//! use photocraft_process_plugin_protocol::{Header, method::{self, Method}};
//! let header = Header::request::<method::LayerTree>(7, &method::DocRef { doc: "d1".into() });
//! ```
//!
//! Unless noted, requests go from the plug-in to the host. `hello` is the plug-in's first
//! message; `ping` and `shutdown` go from the host to the plug-in. Methods that move bulk data
//! (pixels, images, blobs) carry it in the frame payload, described per method.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::types::{DocId, DocInfo, LayerInfo, LayerUid, PixelFormat, Rect};
use crate::widget::{Node, Patch};

/// A request type: its wire name, parameters and result.
pub trait Method {
    /// The method name sent on the wire, for example `"layer.tree"`.
    const NAME: &'static str;
    type Params: Serialize + DeserializeOwned;
    type Result: Serialize + DeserializeOwned;
}

/// Declares a method marker type and its `Method` impl.
macro_rules! methods {
    ($( $(#[$meta:meta])* $ty:ident = $name:literal : $params:ty => $result:ty; )*) => {
        $(
            $(#[$meta])*
            #[derive(Debug, Clone, Copy)]
            pub struct $ty;
            impl Method for $ty {
                const NAME: &'static str = $name;
                type Params = $params;
                type Result = $result;
            }
        )*
        /// Every method name, for dispatch tables, documentation and tests.
        pub const ALL: &[&str] = &[$($name),*];
    };
}

/// No parameters, or a result that only signals success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Empty {}

/// Parameters naming a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocRef {
    pub doc: DocId,
}

/// Parameters naming a layer of a document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerRef {
    pub doc: DocId,
    pub uid: LayerUid,
}

// --- session ---

/// The plug-in's first message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloParams {
    /// The protocol version the plug-in speaks.
    pub protocol: u32,
    /// The nonce the host passed in `PHOTOCRAFT_PLUGIN_NONCE`, proving this is the launched process.
    pub nonce: String,
    pub plugin: PluginIdentity,
}

/// Who the plug-in says it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginIdentity {
    pub id: String,
    pub version: String,
}

/// The host's answer to `hello`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Welcome {
    pub host_version: String,
    pub protocol: u32,
    /// UI language, as a BCP 47 tag such as `"en"` or `"ja"`.
    pub locale: String,
    /// Folder where the plug-in keeps its settings and data.
    pub data_dir: String,
    /// Folder where the plug-in writes its logs.
    pub log_dir: String,
    pub theme: Theme,
}

/// The host's appearance, so plug-in icons can match it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Theme {
    pub dark: bool,
}

// --- documents ---

/// Where a resized canvas keeps the existing content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Anchor {
    #[default]
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResizeCanvasParams {
    pub doc: DocId,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub anchor: Anchor,
}

/// Resample the whole image to `width` x `height` pixels (Image › Image Size, bilinear),
/// optionally setting its resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResizeImageParams {
    pub doc: DocId,
    pub width: u32,
    pub height: u32,
    /// Pixels per inch after the resize; unchanged when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpi: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenParams {
    pub path: String,
}

// --- layers ---

/// The kinds of layer a plug-in can create directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NewLayerKind {
    Pixel,
    Group,
}

/// Where a new or moved layer goes: inside `parent` (default: the document root), directly above
/// the sibling `above` (default: on top of its siblings).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<LayerUid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub above: Option<LayerUid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateLayerParams {
    pub doc: DocId,
    pub kind: NewLayerKind,
    pub name: String,
    #[serde(flatten)]
    pub placement: Placement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedLayer {
    pub uid: LayerUid,
}

/// Properties to change; omitted fields stay as they are.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateLayerParams {
    pub doc: DocId,
    pub uid: LayerUid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveLayerParams {
    pub doc: DocId,
    pub uid: LayerUid,
    #[serde(flatten)]
    pub placement: Placement,
}

/// Where a new layer mask gets its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MaskSource {
    /// The current selection.
    Selection,
    /// The layer's own transparency.
    Alpha,
    /// A `gray8` image in the payload, covering the whole canvas.
    Image,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddMaskParams {
    pub doc: DocId,
    pub uid: LayerUid,
    pub from: MaskSource,
}

/// A thumbnail request; the result payload is a PNG no larger than `width` x `height`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailParams {
    pub doc: DocId,
    pub uid: LayerUid,
    pub width: u32,
    pub height: u32,
}

// --- undo ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoBeginParams {
    pub doc: DocId,
    /// The label shown in Edit › Undo and the History panel.
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoGroup {
    pub group: u64,
}

// --- pixels ---

/// Which pixels a read returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PixelSource {
    /// One layer's own pixels, without its mask or effects.
    Layer { uid: LayerUid },
    /// A group's composite.
    Group { uid: LayerUid },
    /// The whole document's composite.
    Composite,
}

/// Read pixels. The result payload holds the rows in `format` (see [`PixelFormat`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadPixelsParams {
    pub doc: DocId,
    pub source: PixelSource,
    pub rect: Rect,
    pub format: PixelFormat,
    /// Layers to leave out of a composite (as if hidden).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<LayerUid>,
    /// Include the plug-in's transient preview layer in a composite (default: no).
    #[serde(default)]
    pub include_preview: bool,
}

/// Write pixels into a pixel layer. The payload holds the rows in `format`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WritePixelsParams {
    pub doc: DocId,
    pub uid: LayerUid,
    pub rect: Rect,
    pub format: PixelFormat,
}

/// Create a pixel layer from the image in the payload, placed at `offset`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePixelLayerParams {
    pub doc: DocId,
    pub name: String,
    #[serde(flatten)]
    pub placement: Placement,
    /// Where the image's top-left pixel lands; `width`/`height` give its size.
    pub rect: Rect,
    pub format: PixelFormat,
}

// --- selection and masks ---

/// Read the selection as `gray8` coverage for `rect`; the result says whether there is one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadSelectionParams {
    pub doc: DocId,
    pub rect: Rect,
}

/// Result of a selection read. When `none` is true there is no selection and no payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionReadResult {
    #[serde(default)]
    pub none: bool,
}

/// Tight bounds of the selection, or `None` when nothing is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionBounds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Rect>,
}

/// Replace the selection with the `gray8` coverage in the payload, or clear it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSelectionParams {
    pub doc: DocId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<Rect>,
    #[serde(default)]
    pub clear: bool,
}

/// Which coverage of a layer to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MaskKind {
    /// The layer's alpha channel.
    Alpha,
    /// The layer mask.
    LayerMask,
}

/// Read a layer's coverage as `gray8` for `rect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadMaskParams {
    pub doc: DocId,
    pub uid: LayerUid,
    pub which: MaskKind,
    pub rect: Rect,
}

// --- stored data ---

/// A document blob, namespaced to the calling plug-in. `blob.set` carries the bytes in the
/// payload; `blob.get` returns them in the result payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobParams {
    pub doc: DocId,
    pub name: String,
}

/// Result of `blob.get`: whether the blob exists (its bytes are the payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobFound {
    pub found: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobNames {
    pub names: Vec<String>,
}

/// Read (`value` omitted) or write a string value stored on a layer for the calling plug-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerDataParams {
    pub doc: DocId,
    pub uid: LayerUid,
    pub key: String,
    /// The value to store; `null` removes the key (for `layerData.set`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerDataValue {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

// --- vector content ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateVectorParams {
    pub doc: DocId,
    pub name: String,
    #[serde(flatten)]
    pub placement: Placement,
    /// SVG made of `<circle>` and `<line>` elements with `id` attributes.
    pub svg: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddSvgParams {
    pub doc: DocId,
    pub uid: LayerUid,
    pub svg: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveShapesParams {
    pub doc: DocId,
    pub uid: LayerUid,
    pub ids: Vec<String>,
}

/// One element of a vector group, as an SVG fragment with its current geometry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorShape {
    /// The element's own `id`; duplicates are possible after the user duplicates a shape.
    pub id: String,
    /// A unique handle for this element, for `vector.removeShapes`.
    pub handle: String,
    pub svg: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VectorShapes {
    pub shapes: Vec<VectorShape>,
}

// --- preview ---

/// Show (or replace) the plug-in's transient preview of the image in the payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShowPreviewParams {
    pub doc: DocId,
    /// Shown directly above this layer (default: on top of everything).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub above: Option<LayerUid>,
    /// The label shown for the preview in the Layers panel.
    pub label: String,
    /// Where the image goes and its size.
    pub rect: Rect,
    pub format: PixelFormat,
    /// Shade the canvas outside this rectangle with a hatch pattern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hatch_outside: Option<Rect>,
}

// --- UI ---

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterPanelParams {
    pub panel_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub tree: Node,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchPanelParams {
    pub panel_id: String,
    pub ops: Vec<Patch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelRef {
    pub panel_id: String,
}

/// Store an image (PNG, or SVG for icons) in the payload in the host's cache under `hash`, so
/// widget trees can refer to it as `"image:<hash>"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutImageParams {
    pub hash: String,
    /// `"png"` or `"svg"`.
    pub encoding: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenDialogParams {
    pub dialog_id: String,
    pub title: String,
    #[serde(default)]
    pub modal: bool,
    pub tree: Node,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchDialogParams {
    pub dialog_id: String,
    pub ops: Vec<Patch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DialogRef {
    pub dialog_id: String,
}

/// The icon a message box shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageKind {
    Information,
    Question,
    Warning,
    Critical,
}

/// A message box; the host answers with the label of the button the user picked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageParams {
    pub kind: MessageKind,
    pub title: String,
    pub text: String,
    /// Button labels, left to right; the first is the default.
    pub buttons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageAnswer {
    /// The button picked, or `None` when the box was closed without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button: Option<String>,
}

/// What a file dialog picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileDialogMode {
    Open,
    Save,
    Folder,
}

/// One file-type filter, e.g. `{ "name": "Workflows", "extensions": ["json"] }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDialogParams {
    pub mode: FileDialogMode,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<FileFilter>,
    /// Folder or file the dialog starts at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PickedPath {
    /// The chosen path, or `None` when the user cancelled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

// --- actions ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterActionParams {
    /// Unique within the plug-in, e.g. `"generate"`.
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Default shortcut in the host's notation (`"Ctrl+Shift+G"`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<String>,
    /// Submenu under the plug-in's menu, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menu: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateActionParams {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
}

// --- services ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoticeParams {
    pub level: NoticeLevel,
    pub text: String,
}

/// Show or update a progress indicator in the host's status area. Never locks a document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressParams {
    pub task_id: String,
    /// 0..1, or `None` for an indeterminate indicator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f32>,
    pub message: String,
    /// Whether to show a Cancel button (which sends `progress.cancel`).
    #[serde(default)]
    pub cancellable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRef {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UrlParams {
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathParams {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardText {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeParams {
    /// Event topics to receive (see [`crate::event`]); an empty list unsubscribes from all.
    pub topics: Vec<String>,
}

methods! {
    // session
    /// The plug-in introduces itself (first message, plug-in to host).
    Hello = "hello": HelloParams => Welcome;
    /// Liveness check (host to plug-in).
    Ping = "ping": Empty => Empty;
    /// Ask the plug-in to exit (host to plug-in).
    Shutdown = "shutdown": Empty => Empty;

    // documents
    DocList = "doc.list": Empty => Vec<DocInfo>;
    DocInfoMethod = "doc.info": DocRef => DocInfo;
    DocSetActive = "doc.setActive": DocRef => Empty;
    DocResizeCanvas = "doc.resizeCanvas": ResizeCanvasParams => Empty;
    DocResizeImage = "doc.resizeImage": ResizeImageParams => Empty;
    DocOpen = "doc.open": OpenParams => DocRef;

    // layers
    LayerTree = "layer.tree": DocRef => Vec<LayerInfo>;
    LayerActive = "layer.active": DocRef => Option<LayerUid>;
    LayerSetActive = "layer.setActive": LayerRef => Empty;
    LayerCreate = "layer.create": CreateLayerParams => CreatedLayer;
    LayerUpdate = "layer.update": UpdateLayerParams => Empty;
    LayerMove = "layer.move": MoveLayerParams => Empty;
    LayerDelete = "layer.delete": LayerRef => Empty;
    LayerAddMask = "layer.addMask": AddMaskParams => Empty;
    LayerThumbnail = "layer.thumbnail": ThumbnailParams => Empty;

    // undo
    UndoBegin = "undo.begin": UndoBeginParams => UndoGroup;
    UndoEnd = "undo.end": UndoGroup => Empty;

    // pixels, selection, masks
    PixelsRead = "pixels.read": ReadPixelsParams => Empty;
    PixelsWrite = "pixels.write": WritePixelsParams => Empty;
    PixelsCreateLayer = "pixels.createLayer": CreatePixelLayerParams => CreatedLayer;
    SelectionRead = "selection.read": ReadSelectionParams => SelectionReadResult;
    SelectionBoundsMethod = "selection.bounds": DocRef => SelectionBounds;
    SelectionSet = "selection.set": SetSelectionParams => Empty;
    MaskRead = "mask.read": ReadMaskParams => Empty;

    // stored data
    BlobGet = "blob.get": BlobParams => BlobFound;
    BlobSet = "blob.set": BlobParams => Empty;
    BlobDelete = "blob.delete": BlobParams => Empty;
    BlobList = "blob.list": DocRef => BlobNames;
    LayerDataGet = "layerData.get": LayerDataParams => LayerDataValue;
    LayerDataSet = "layerData.set": LayerDataParams => Empty;

    // vector content
    VectorCreateFromSvg = "vector.createFromSvg": CreateVectorParams => CreatedLayer;
    VectorShapesMethod = "vector.shapes": LayerRef => VectorShapes;
    VectorAddSvg = "vector.addSvg": AddSvgParams => Empty;
    VectorRemoveShapes = "vector.removeShapes": RemoveShapesParams => Empty;

    // preview
    PreviewShow = "preview.show": ShowPreviewParams => Empty;
    PreviewHide = "preview.hide": DocRef => Empty;

    // UI
    PanelRegister = "panel.register": RegisterPanelParams => Empty;
    PanelPatch = "panel.patch": PatchPanelParams => Empty;
    PanelUnregister = "panel.unregister": PanelRef => Empty;
    ImagePut = "image.put": PutImageParams => Empty;
    DialogOpen = "dialog.open": OpenDialogParams => Empty;
    DialogPatch = "dialog.patch": PatchDialogParams => Empty;
    DialogClose = "dialog.close": DialogRef => Empty;
    DialogMessage = "dialog.message": MessageParams => MessageAnswer;
    DialogFile = "dialog.file": FileDialogParams => PickedPath;

    // actions
    ActionRegister = "action.register": RegisterActionParams => Empty;
    ActionUpdate = "action.update": UpdateActionParams => Empty;

    // services
    NoticeShow = "notice.show": NoticeParams => Empty;
    ProgressSet = "progress.set": ProgressParams => Empty;
    ProgressClear = "progress.clear": TaskRef => Empty;
    OpenUrl = "open.url": UrlParams => Empty;
    OpenPath = "open.path": PathParams => Empty;
    PluginUpdate = "plugin.update": PathParams => Empty;
    ClipboardGet = "clipboard.get": Empty => ClipboardText;
    ClipboardSet = "clipboard.set": ClipboardText => Empty;
    EventsSubscribe = "events.subscribe": SubscribeParams => Empty;
}
