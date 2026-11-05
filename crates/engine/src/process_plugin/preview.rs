//! `preview.show` / `preview.hide`: a plug-in's transient preview on the canvas.
//!
//! A preview is kept here, never in the document: it takes no history step, is never saved and
//! can't be edited, selected or painted on. The canvas shows it by compositing
//! [`Session::plugin_preview_document`], a copy of the document with the preview inserted as a
//! locked layer (and, for live painting, a hatch multiplied over the canvas outside a rectangle).
//! Composite reads leave it out unless they ask for it (`includePreview`).
//!
//! One preview per document: a newer one replaces it, whichever plug-in showed it. It goes when
//! the plug-in hides it, when its document closes or stops being the active one, and when the
//! plug-in disconnects.

use std::sync::Arc;

use photocraft_color::{BlendMode, PixelFormat};
use photocraft_doc::{DocId, Document, Layer, LayerContent};
use photocraft_geom::Rect;
use photocraft_process_plugin_protocol::message::{ErrorBody, ErrorCode};
use photocraft_process_plugin_protocol::method::{DocRef, ShowPreviewParams};
use photocraft_process_plugin_protocol::types::PixelFormat as WireFormat;
use photocraft_raster::Surface;
use serde_json::Value;

use super::layers::find;
use super::{Outcome, ok, params};
use crate::Session;

/// Spacing of the hatch's diagonal lines, and their width, in pixels.
const HATCH_PERIOD: i32 = 8;
const HATCH_WIDTH: i32 = 2;
/// How dark the hatch lines make the canvas (multiplied).
const HATCH_SHADE: f32 = 0.55;

/// A preview shown on one document.
#[derive(Clone)]
pub(super) struct Preview {
    plugin: String,
    /// Shown directly above the layer with this uid, or on top when `None` (or when that layer
    /// is gone).
    above: Option<u128>,
    label: String,
    /// The image, in the document's pixel format, at its canvas position.
    image: Arc<Surface>,
    /// The rectangle left clear and the hatch over the rest of the canvas.
    hatch: Option<(Rect, Arc<Surface>)>,
}

/// What the Layers panel shows for a document's preview: its label and the layer it sits above
/// (`None`: on top).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRow {
    pub label: String,
    pub above: Option<photocraft_doc::LayerId>,
}

/// One tile of the hatch, in RGBA float: dark diagonal lines on transparency, except where
/// `keep` covers it. The lines repeat every [`HATCH_PERIOD`] pixels, which divides the tile
/// size, so every tile `keep` doesn't touch is the same.
fn hatch_tile(tile: Rect, keep: Rect) -> Vec<f32> {
    let mut px = vec![0.0f32; tile.width() as usize * tile.height() as usize * 4];
    for (i, p) in px.chunks_mut(4).enumerate() {
        let x = tile.x0 + (i % tile.width() as usize) as i32;
        let y = tile.y0 + (i / tile.width() as usize) as i32;
        if (x + y).rem_euclid(HATCH_PERIOD) < HATCH_WIDTH && !keep.contains(x, y) {
            p.copy_from_slice(&[HATCH_SHADE, HATCH_SHADE, HATCH_SHADE, 1.0]);
        }
    }
    px
}

/// The hatch over `canvas` outside `keep`, in `fmt`. The tiles `keep` doesn't touch share one
/// tile, so a large canvas costs a few tiles, not a full-size image.
fn hatch_surface(fmt: PixelFormat, canvas: Rect, keep: Rect) -> Surface {
    let mut out = Surface::new(fmt);
    let mut shared: Option<Arc<photocraft_raster::Tile>> = None;
    for coord in canvas.tiles() {
        let tile = coord.rect();
        let clear = tile.intersect(&keep).is_empty();
        if clear && let Some(t) = &shared {
            out.put_tiles([(coord, t.clone())]);
            continue;
        }
        let mut staged = Surface::new(PixelFormat::RGBA32F);
        staged.write_region(tile, &hatch_tile(tile, keep));
        if let Some(t) = staged.convert(fmt).tile(coord).cloned() {
            if clear {
                shared = Some(t.clone());
            }
            out.put_tiles([(coord, t)]);
        }
    }
    out
}

/// The hatch layer: [`hatch_surface`] multiplied over everything below it.
fn hatch_layer(hatch: &Arc<Surface>) -> Layer {
    let mut layer = Layer::new("Hatch", LayerContent::Raster((**hatch).clone()));
    layer.blend = BlendMode::Multiply;
    layer.locks.all = true;
    layer
}

impl Session {
    /// The document as the canvas shows it with its plug-in preview, or `None` when it has no
    /// preview. Built afresh on each call: callers keep it while
    /// [`Session::plugin_preview_generation`] and the document stay the same.
    pub fn plugin_preview_document(&self, doc: &Document) -> Option<Document> {
        let preview = self.process_plugin.previews.get(&doc.id)?;
        let mut shown = doc.clone();
        let mut layer = Layer::new(preview.label.clone(), LayerContent::Raster((*preview.image).clone()));
        layer.locks.all = true;
        let mut inserted = Vec::with_capacity(2);
        let above = preview.above.and_then(|uid| shown.walk().into_iter().find(|(_, _, l)| l.uid == uid).map(|(_, _, l)| l.id));
        let at = above.and_then(|id| shown.path_of(id));
        // Bottom to top: the image, then the hatch over everything below it.
        inserted.push(layer);
        if let Some((_, hatch)) = &preview.hatch {
            inserted.push(hatch_layer(hatch));
        }
        match (above, at) {
            (Some(id), Some(_)) => {
                let mut below = id;
                for layer in inserted {
                    below = shown.insert_above(Some(below), layer);
                }
            }
            _ => shown.layers.extend(inserted),
        }
        Some(shown)
    }

    /// Changes whenever any preview is shown or hidden.
    pub fn plugin_preview_generation(&self) -> u64 {
        self.process_plugin.preview_generation
    }

    /// The Layers panel row for document `doc`'s preview, if it has one.
    pub fn plugin_preview_row(&self, doc: &Document) -> Option<PreviewRow> {
        let preview = self.process_plugin.previews.get(&doc.id)?;
        let above = preview.above.and_then(|uid| doc.walk().into_iter().find(|(_, _, l)| l.uid == uid).map(|(_, _, l)| l.id));
        Some(PreviewRow { label: preview.label.clone(), above })
    }

    /// Drop previews whose document closed or is no longer the active one.
    pub(super) fn prune_plugin_previews(&mut self) {
        if self.process_plugin.previews.is_empty() {
            return;
        }
        let active = self.active.and_then(|i| self.docs.get(i)).map(|st| st.doc.id);
        let before = self.process_plugin.previews.len();
        self.process_plugin.previews.retain(|id, _| Some(*id) == active);
        if self.process_plugin.previews.len() != before {
            self.process_plugin.preview_generation += 1;
        }
    }

    /// Drop the previews `plugin` showed.
    pub(super) fn drop_plugin_previews(&mut self, plugin: &str) {
        let before = self.process_plugin.previews.len();
        self.process_plugin.previews.retain(|_, p| p.plugin != plugin);
        if self.process_plugin.previews.len() != before {
            self.process_plugin.preview_generation += 1;
        }
    }
}

pub(super) fn show(s: &mut Session, plugin: &str, p: &Value, payload: &[u8]) -> Outcome {
    let p: ShowPreviewParams = params("preview.show", p)?;
    if p.label.is_empty() || p.label.len() > 200 {
        return Err(ErrorBody::new(ErrorCode::Invalid, "the preview label must be 1 to 200 bytes"));
    }
    if p.format == WireFormat::Gray8 {
        return Err(ErrorBody::new(ErrorCode::Invalid, "a preview is a colour image: bgra8, rgba8, rgba16 or rgba32f"));
    }
    let rect = super::pixels::canvas_rect(&p.rect, p.format)?;
    super::pixels::check_payload(payload, &p.rect, p.format)?;
    let index = s.plugin_doc_index(&p.doc)?;
    let doc = s.docs.get(index).map(|st| st.doc.clone()).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, "no such document"))?;
    super::pixels::check_mode(&doc)?;
    let above = match &p.above {
        Some(uid) => {
            let id = find(&doc, uid)?;
            doc.layer(id).map(|l| l.uid)
        }
        None => None,
    };
    let hatch_outside = match &p.hatch_outside {
        Some(r) => Some(super::pixels::canvas_rect(r, WireFormat::Gray8)?),
        None => None,
    };
    let mut image = super::pixels::import(&doc, payload, p.format, rect, doc.pixel_format())?;
    image.prune();
    // Live painting shows a new preview every few seconds with the same hatch: keep it.
    let previous = s.process_plugin.previews.get(&doc.id).and_then(|p| p.hatch.clone());
    let hatch = hatch_outside.map(|keep| match previous {
        Some((kept, surface)) if kept == keep && surface.format() == doc.pixel_format() => (keep, surface),
        _ => (keep, Arc::new(hatch_surface(doc.pixel_format(), doc.bounds(), keep))),
    });
    s.process_plugin.preview_generation += 1;
    s.process_plugin.previews.insert(doc.id, Preview { plugin: plugin.to_string(), above, label: p.label, image: Arc::new(image), hatch });
    ok()
}

pub(super) fn hide(s: &mut Session, p: &Value) -> Outcome {
    let p: DocRef = params("preview.hide", p)?;
    let index = s.plugin_doc_index(&p.doc)?;
    let id: Option<DocId> = s.docs.get(index).map(|st| st.doc.id);
    if let Some(id) = id
        && s.process_plugin.previews.remove(&id).is_some()
    {
        s.process_plugin.preview_generation += 1;
    }
    ok()
}
