//! `pixels.*`, `selection.*` and `mask.*`: moving pixels and coverage between documents and
//! plug-ins.
//!
//! Colour pixels always travel as straight-alpha RGBA in sRGB, whatever the document's depth,
//! mode and profile: reads go through the compositor (which turns every mode into RGB in the
//! document's composite profile, as the canvas shows it) and then through a CMS transform to
//! sRGB; writes take the sRGB pixels through the inverse transform into the document's own mode
//! and depth. An 8-bit sRGB document needs no transform at all, so its round trip is exact.
//!
//! Coverage (selections and masks) travels as `gray8`.

use std::sync::Arc;

use photocraft_cms::{Builtin, Profile, Transform};
use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Layer, LayerContent};
use photocraft_process_plugin_protocol::message::{ErrorBody, ErrorCode};
use photocraft_process_plugin_protocol::method::{
    CreatePixelLayerParams, CreatedLayer, DocRef, MaskKind, PixelSource, ReadMaskParams, ReadPixelsParams, ReadSelectionParams, SelectionBounds,
    SelectionReadResult, SetSelectionParams, WritePixelsParams,
};
use photocraft_process_plugin_protocol::types::{PixelFormat as WireFormat, Rect as WireRect};
use photocraft_raster::Surface;
use serde_json::Value;

use super::layers::{check_name, find, place};
use super::{Outcome, json, ok, params};
use crate::Session;

/// Largest payload one request may send or receive: the frame limit of the protocol.
const MAX_PAYLOAD: u64 = 512 << 20;

/// The straight-alpha float RGBA format pixels are staged in between the wire and a document.
const STAGING: PixelFormat = PixelFormat::new(ColorMode::Rgb, SampleType::F32, true);

/// A wire rectangle as a canvas rectangle, refusing ones that don't fit the canvas coordinate
/// range, are empty, or would need a payload past [`MAX_PAYLOAD`] in `format`.
pub(super) fn canvas_rect(r: &WireRect, format: WireFormat) -> Result<photocraft_geom::Rect, ErrorBody> {
    if r.is_empty() {
        return Err(ErrorBody::new(ErrorCode::Invalid, "the rectangle is empty"));
    }
    let fits = |v: i64| i32::try_from(v).is_ok();
    if !fits(r.x) || !fits(r.y) || !fits(r.x + i64::from(r.width)) || !fits(r.y + i64::from(r.height)) {
        return Err(ErrorBody::new(ErrorCode::Invalid, "the rectangle is outside the canvas coordinate range"));
    }
    let bytes = format.payload_len(r.width, r.height).unwrap_or(u64::MAX);
    if bytes > MAX_PAYLOAD {
        return Err(ErrorBody::new(ErrorCode::TooLarge, format!("{}x{} pixels are {bytes} bytes, more than {MAX_PAYLOAD}", r.width, r.height)));
    }
    // `fits` above makes these conversions exact.
    Ok(photocraft_geom::Rect::from_xywh(r.x as i32, r.y as i32, r.width, r.height))
}

/// Refuse a payload whose length isn't exactly `rect` in `format`.
pub(super) fn check_payload(payload: &[u8], rect: &WireRect, format: WireFormat) -> Result<(), ErrorBody> {
    let expected = format.payload_len(rect.width, rect.height).unwrap_or(u64::MAX);
    if payload.len() as u64 != expected {
        return Err(ErrorBody::new(
            ErrorCode::Invalid,
            format!("{}x{} {format:?} pixels are {expected} bytes, got {}", rect.width, rect.height, payload.len()),
        ));
    }
    Ok(())
}

/// Refuse documents whose colour mode has no meaningful sRGB view: Bitmap, Indexed, Multichannel
/// and Duotone documents would lose their structure on a write.
pub(super) fn check_mode(doc: &Document) -> Result<(), ErrorBody> {
    match doc.mode {
        ColorMode::Rgb | ColorMode::Grayscale | ColorMode::Cmyk | ColorMode::Lab => Ok(()),
        mode => Err(ErrorBody::new(ErrorCode::Unsupported, format!("{mode:?} documents can't be converted to and from RGB"))),
    }
}

fn is_srgb(profile: &Profile) -> bool {
    profile.to_bytes().as_ref() == Builtin::Srgb.profile().to_bytes().as_ref()
}

/// The transform from `from` to `to`, or `None` when both are sRGB.
fn transform(from: &Profile, to: &Profile) -> Result<Option<Transform>, ErrorBody> {
    if is_srgb(from) && is_srgb(to) {
        return Ok(None);
    }
    // The canvas's own intent, so a read matches what the user sees on an sRGB display.
    Transform::new(from, to, crate::display_color::DISPLAY_INTENT, crate::display_color::DISPLAY_BPC)
        .map(Some)
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("can't convert the document's colours: {e}")))
}

/// The open document `id`, as a snapshot.
fn snapshot(s: &Session, id: &str) -> Result<Arc<Document>, ErrorBody> {
    let index = s.plugin_doc_index(id)?;
    s.docs.get(index).map(|st| st.doc.clone()).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, "no such document"))
}

/// `layer` rendered on its own, as plug-ins mean "its pixels": shown, fully opaque, normal blend,
/// without its mask, clipping or effects (a hidden layer still has pixels).
fn bare(layer: &Layer) -> Layer {
    let mut l = layer.clone();
    l.visible = true;
    l.opacity = 1.0;
    l.fill_opacity = 1.0;
    l.blend = photocraft_color::BlendMode::Normal;
    l.clipped = false;
    l.mask = None;
    l.vector_mask = None;
    l.effects.enabled = false;
    l.blend_if = Default::default();
    l.advanced = Default::default();
    l.excluded_channels = 0;
    l
}

/// Straight-alpha RGBA of `rect` in the document's composite profile.
fn render(doc: &Document, source: &PixelSource, exclude: &[String], rect: photocraft_geom::Rect) -> Result<Vec<[f32; 4]>, ErrorBody> {
    let buffer = match source {
        PixelSource::Layer { uid } | PixelSource::Group { uid } => {
            let id = find(doc, uid)?;
            let layer = doc.layer(id).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("no layer '{uid}' in this document")))?;
            match (source, layer.is_group()) {
                (PixelSource::Group { .. }, false) => return Err(ErrorBody::new(ErrorCode::Invalid, format!("'{uid}' is not a group"))),
                (PixelSource::Layer { .. }, true) => return Err(ErrorBody::new(ErrorCode::Invalid, format!("'{uid}' is a group; read it as a group"))),
                _ => {}
            }
            photocraft_compose::render_layer_in(doc, &bare(layer), rect)
        }
        PixelSource::Composite => {
            if exclude.is_empty() {
                photocraft_compose::render(doc, rect)
            } else {
                let mut hidden = doc.clone();
                for uid in exclude {
                    let id = find(&hidden, uid)?;
                    if let Some(layer) = hidden.layer_mut(id) {
                        layer.visible = false;
                    }
                }
                photocraft_compose::render(&hidden, rect)
            }
        }
    };
    if buffer.rect != rect {
        return Err(ErrorBody::new(ErrorCode::TooLarge, "the rectangle is too large to render at once"));
    }
    Ok(buffer.into_px())
}

/// Composite-profile RGBA → sRGB, in place.
fn to_srgb(doc: &Document, px: &mut [[f32; 4]]) -> Result<(), ErrorBody> {
    if let Some(t) = transform(&crate::color_cmds::composite_profile(doc), Builtin::Srgb.profile())? {
        let src = px.as_flattened().to_vec();
        t.convert_f32(&src, 4, px.as_flattened_mut(), 4, true);
    }
    Ok(())
}

/// Float RGBA → wire bytes. Gray8 is never a colour format (callers refuse it first).
fn encode(px: &[[f32; 4]], format: WireFormat) -> Vec<u8> {
    let u8_of = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let u16_of = |v: f32| (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
    let mut out = Vec::with_capacity(px.len() * format.bytes_per_pixel() as usize);
    match format {
        WireFormat::Rgba8 | WireFormat::Gray8 => out.extend(px.iter().flat_map(|p| p.map(u8_of))),
        WireFormat::Bgra8 => out.extend(px.iter().flat_map(|p| [u8_of(p[2]), u8_of(p[1]), u8_of(p[0]), u8_of(p[3])])),
        WireFormat::Rgba16 => out.extend(px.iter().flat_map(|p| p.map(u16_of)).flat_map(u16::to_le_bytes)),
        WireFormat::Rgba32f => out.extend(px.iter().flat_map(|p| *p).flat_map(f32::to_le_bytes)),
    }
    out
}

/// Wire bytes → float RGBA (the payload length was checked).
fn decode(bytes: &[u8], format: WireFormat) -> Vec<f32> {
    let from_u8 = |v: u8| f32::from(v) / 255.0;
    match format {
        WireFormat::Rgba8 | WireFormat::Gray8 => bytes.iter().map(|&v| from_u8(v)).collect(),
        WireFormat::Bgra8 => bytes.as_chunks::<4>().0.iter().flat_map(|p| [from_u8(p[2]), from_u8(p[1]), from_u8(p[0]), from_u8(p[3])]).collect(),
        WireFormat::Rgba16 => bytes.as_chunks::<2>().0.iter().map(|b| f32::from(u16::from_le_bytes(*b)) / 65535.0).collect(),
        WireFormat::Rgba32f => bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect(),
    }
}

fn colour_format(format: WireFormat) -> Result<WireFormat, ErrorBody> {
    if format == WireFormat::Gray8 {
        return Err(ErrorBody::new(ErrorCode::Invalid, "gray8 is for selections and masks; colour pixels are bgra8, rgba8, rgba16 or rgba32f"));
    }
    Ok(format)
}

/// sRGB pixels from the payload as a surface in the document's mode and depth (`target`),
/// covering `rect`.
pub(super) fn import(doc: &Document, payload: &[u8], format: WireFormat, rect: photocraft_geom::Rect, target: PixelFormat) -> Result<Surface, ErrorBody> {
    let mut staged = Surface::new(STAGING);
    staged.write_region(rect, &decode(payload, format));
    let profile = crate::color_cmds::document_profile(doc);
    Ok(match transform(Builtin::Srgb.profile(), &profile)? {
        Some(t) if doc.mode == target.mode => crate::color_cmds::convert_surface(&staged, ColorMode::Rgb, target, &t).convert(target),
        _ => staged.convert(target),
    })
}

pub(super) fn read(s: &mut Session, p: &Value) -> Outcome {
    let p: ReadPixelsParams = params("pixels.read", p)?;
    let format = colour_format(p.format)?;
    let rect = canvas_rect(&p.rect, format)?;
    let doc = snapshot(s, &p.doc)?;
    check_mode(&doc)?;
    // The plug-in's transient preview joins a composite only when asked for.
    let shown = (p.include_preview && p.source == PixelSource::Composite).then(|| s.plugin_preview_document(&doc)).flatten();
    let doc = shown.map(Arc::new).unwrap_or(doc);
    let mut px = render(&doc, &p.source, &p.exclude, rect)?;
    to_srgb(&doc, &mut px)?;
    Ok((Value::Object(serde_json::Map::new()), encode(&px, format)))
}

/// Refuse writing into a layer whose pixels are locked.
fn check_writable(layer: &Layer, uid: &str) -> Result<(), ErrorBody> {
    if layer.locks.all || layer.locks.pixels {
        return Err(ErrorBody::new(ErrorCode::Invalid, format!("layer '{uid}' is locked")));
    }
    Ok(())
}

pub(super) fn write(s: &mut Session, plugin: &str, p: &Value, payload: &[u8]) -> Outcome {
    let p: WritePixelsParams = params("pixels.write", p)?;
    let format = colour_format(p.format)?;
    let rect = canvas_rect(&p.rect, format)?;
    check_payload(payload, &p.rect, format)?;
    s.plugin_edit(plugin, &p.doc, "Apply Image", |doc, _| {
        check_mode(doc)?;
        let id = find(doc, &p.uid)?;
        let layer = doc.layer(id).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("no layer '{}'", p.uid)))?;
        check_writable(layer, &p.uid)?;
        let target = match &layer.content {
            LayerContent::Raster(surface) => Some(surface.format()),
            _ => None,
        }
        .ok_or_else(|| ErrorBody::new(ErrorCode::Invalid, format!("'{}' is not a pixel layer", p.uid)))?;
        let converted = import(doc, payload, format, rect, target)?;
        let bytes = converted.to_interleaved(rect);
        let surface = doc.layer_mut(id).and_then(Layer::surface_mut).ok_or_else(|| ErrorBody::new(ErrorCode::Internal, "the layer changed"))?;
        surface.try_write_interleaved(rect, &bytes).map_err(|e| ErrorBody::new(ErrorCode::TooLarge, format!("can't store the pixels: {e:?}")))?;
        surface.prune();
        Ok(())
    })?;
    ok()
}

pub(super) fn create_layer(s: &mut Session, plugin: &str, p: &Value, payload: &[u8]) -> Outcome {
    let p: CreatePixelLayerParams = params("pixels.createLayer", p)?;
    check_name(&p.name)?;
    let format = colour_format(p.format)?;
    let rect = canvas_rect(&p.rect, format)?;
    check_payload(payload, &p.rect, format)?;
    let uid = s.plugin_edit(plugin, &p.doc, "New Layer", |doc, active| {
        check_mode(doc)?;
        let mut surface = import(doc, payload, format, rect, doc.pixel_format())?;
        surface.prune();
        let layer = Layer::new(p.name.clone(), LayerContent::Raster(surface));
        let uid = layer.uid;
        *active = Some(place(doc, &p.placement, layer)?);
        Ok(uid)
    })?;
    json(&CreatedLayer { uid: photocraft_doc::uid_string(uid) })
}

/// `surface`'s coverage over `rect` as `gray8`.
fn coverage(surface: &Surface, rect: photocraft_geom::Rect) -> Vec<u8> {
    if surface.format() == PixelFormat::GRAY8 { surface.to_interleaved(rect) } else { surface.convert(PixelFormat::GRAY8).to_interleaved(rect) }
}

pub(super) fn selection_read(s: &mut Session, p: &Value) -> Outcome {
    let p: ReadSelectionParams = params("selection.read", p)?;
    let rect = canvas_rect(&p.rect, WireFormat::Gray8)?;
    let doc = snapshot(s, &p.doc)?;
    match &doc.selection {
        None => json(&SelectionReadResult { none: true }),
        Some(selection) => Ok((Value::Object(serde_json::Map::new()), coverage(selection, rect))),
    }
}

/// Tight bounds of a selection on the canvas, `None` when it selects nothing there.
pub(super) fn selection_bounds(doc: &Document) -> Option<photocraft_geom::Rect> {
    let selection = doc.selection.as_ref()?;
    // A selection whose untouched pixels are selected covers the whole canvas.
    let reach = if selection.default_pixel().first().is_some_and(|v| *v > 0.0) { doc.bounds() } else { selection.content_bounds() };
    let bounds = reach.intersect(&doc.bounds());
    (!bounds.is_empty()).then_some(bounds)
}

pub(super) fn selection_bounds_of(s: &mut Session, p: &Value) -> Outcome {
    let p: DocRef = params("selection.bounds", p)?;
    let doc = snapshot(s, &p.doc)?;
    let bounds = selection_bounds(&doc).map(|r| WireRect::new(i64::from(r.x0), i64::from(r.y0), r.width(), r.height()));
    json(&SelectionBounds { bounds })
}

pub(super) fn selection_set(s: &mut Session, plugin: &str, p: &Value, payload: &[u8]) -> Outcome {
    let p: SetSelectionParams = params("selection.set", p)?;
    if p.clear {
        // Nothing selected: nothing to do and no history step, as Select › Deselect does.
        if snapshot(s, &p.doc)?.selection.is_none() {
            return ok();
        }
        s.plugin_edit(plugin, &p.doc, "Deselect", |doc, _| {
            doc.selection = None;
            Ok(())
        })?;
        return ok();
    }
    let wire = p.rect.ok_or_else(|| ErrorBody::new(ErrorCode::Invalid, "selection.set needs a rect and a gray8 payload, or clear"))?;
    let rect = canvas_rect(&wire, WireFormat::Gray8)?;
    check_payload(payload, &wire, WireFormat::Gray8)?;
    s.plugin_edit(plugin, &p.doc, "Select", |doc, _| {
        let mut selection = Surface::new(PixelFormat::GRAY8);
        selection.try_write_interleaved(rect, payload).map_err(|e| ErrorBody::new(ErrorCode::TooLarge, format!("can't store the selection: {e:?}")))?;
        selection.prune();
        doc.selection = Some(selection);
        Ok(())
    })?;
    ok()
}

pub(super) fn mask_read(s: &mut Session, p: &Value) -> Outcome {
    let p: ReadMaskParams = params("mask.read", p)?;
    let rect = canvas_rect(&p.rect, WireFormat::Gray8)?;
    let doc = snapshot(s, &p.doc)?;
    let id = find(&doc, &p.uid)?;
    let layer = doc.layer(id).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("no layer '{}'", p.uid)))?;
    let bytes = match p.which {
        MaskKind::LayerMask => {
            let mask = layer.mask.as_ref().ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("layer '{}' has no layer mask", p.uid)))?;
            coverage(&mask.surface, rect)
        }
        MaskKind::Alpha => {
            let buffer = photocraft_compose::render_layer_in(&doc, &bare(layer), rect);
            if buffer.rect != rect {
                return Err(ErrorBody::new(ErrorCode::TooLarge, "the rectangle is too large to render at once"));
            }
            buffer.px.iter().map(|p| (p[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect()
        }
    };
    Ok((Value::Object(serde_json::Map::new()), bytes))
}
