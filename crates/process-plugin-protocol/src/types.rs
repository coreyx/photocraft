//! Value types shared by methods and events.

use serde::{Deserialize, Serialize};

/// A document, identified for the lifetime of the host session.
pub type DocId = String;

/// A layer, identified by its persistent UUID (stable across save and reload).
pub type LayerUid = String;

/// An axis-aligned pixel rectangle. `x`/`y` may be negative (layers can extend past the canvas).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn new(x: i64, y: i64, width: u32, height: u32) -> Self {
        Rect { x, y, width, height }
    }

    /// Whether the rectangle covers no pixels.
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Number of pixels, or `None` on overflow.
    pub fn area(&self) -> Option<u64> {
        u64::from(self.width).checked_mul(u64::from(self.height))
    }
}

/// How pixel data in a payload is laid out. Rows are tightly packed, top to bottom, with no
/// padding, so a payload is exactly `width * height * bytes_per_pixel` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PixelFormat {
    /// 8-bit blue, green, red, alpha (straight alpha), the order Qt's ARGB32 and Windows' DIBs use.
    Bgra8,
    /// 8-bit red, green, blue, alpha (straight alpha).
    Rgba8,
    /// 16-bit little-endian red, green, blue, alpha (straight alpha).
    Rgba16,
    /// 32-bit float little-endian red, green, blue, alpha (straight alpha, 0..1 nominal).
    Rgba32f,
    /// 8-bit coverage (selections and masks): 0 = outside, 255 = fully inside.
    Gray8,
}

impl PixelFormat {
    /// Bytes per pixel.
    pub const fn bytes_per_pixel(self) -> u64 {
        match self {
            PixelFormat::Bgra8 | PixelFormat::Rgba8 => 4,
            PixelFormat::Rgba16 => 8,
            PixelFormat::Rgba32f => 16,
            PixelFormat::Gray8 => 1,
        }
    }

    /// Exact payload size for a `width` x `height` image, or `None` on overflow.
    pub fn payload_len(self, width: u32, height: u32) -> Option<u64> {
        Rect::new(0, 0, width, height).area()?.checked_mul(self.bytes_per_pixel())
    }
}

/// Bit depth of a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Depth {
    U8,
    U16,
    F32,
}

/// Colour model of a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColorMode {
    Bitmap,
    Grayscale,
    Indexed,
    Rgb,
    Cmyk,
    Lab,
    Multichannel,
    Duotone,
}

/// What a document looks like to a plug-in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocInfo {
    pub doc: DocId,
    pub name: String,
    /// File the document was opened from or last saved to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub width: u32,
    pub height: u32,
    pub dpi: f32,
    pub depth: Depth,
    pub mode: ColorMode,
    /// Whether this is the active document.
    pub active: bool,
}

/// The kind of a layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LayerKind {
    Pixel,
    Group,
    Shape,
    Text,
    Smart,
    Adjustment,
    Fill,
    Video,
    /// A vector group created with `vector.createFromSvg` (for example a pose skeleton).
    VectorGroup,
    /// Any other content.
    Other,
}

/// One node of a document's layer tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerInfo {
    pub uid: LayerUid,
    pub name: String,
    pub kind: LayerKind,
    pub visible: bool,
    pub locked: bool,
    pub opacity: f32,
    /// Blend mode, as the PSD key's lowercase name (`"normal"`, `"multiply"`, ...).
    pub blend: String,
    /// Bounds of the layer's non-transparent content, in document pixels.
    pub bounds: Rect,
    /// Whether the layer has a layer mask.
    pub has_mask: bool,
    /// Whether the layer carries frames (a video layer).
    pub animated: bool,
    /// Children, bottom to top (groups only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<LayerInfo>,
}

/// Keyboard modifiers held during an input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Modifiers {
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    /// Command on macOS, the Windows key elsewhere.
    #[serde(default)]
    pub meta: bool,
}
