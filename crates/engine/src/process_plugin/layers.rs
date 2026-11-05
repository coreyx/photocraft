//! `layer.*`: the layer tree, as plug-ins see it and change it.
//!
//! Plug-ins name layers by their persistent uid ([`photocraft_doc::Layer::uid`]), which survives
//! save and reload, never by the session's layer ids.

use photocraft_color::BlendMode;
use photocraft_doc::{Document, Layer, LayerContent, LayerId};
use photocraft_process_plugin_protocol::message::{ErrorBody, ErrorCode};
use photocraft_process_plugin_protocol::method::{
    AddMaskParams, CreateLayerParams, CreatedLayer, DocRef, LayerRef, MaskSource, MoveLayerParams, NewLayerKind, Placement, ThumbnailParams, UpdateLayerParams,
};
use photocraft_process_plugin_protocol::types::{LayerInfo, LayerKind, PixelFormat as WireFormat, Rect};
use serde_json::{Value, json};

use super::{Outcome, engine_error, ok, params};
use crate::Session;

/// Longest layer name a plug-in may set.
const MAX_NAME: usize = 1000;
/// Largest thumbnail side.
const MAX_THUMBNAIL: u32 = 1024;

fn not_found(uid: &str) -> ErrorBody {
    ErrorBody::new(ErrorCode::NotFound, format!("no layer '{uid}' in this document"))
}

/// The session id of the layer with persistent uid `uid` in `doc`.
pub(super) fn find(doc: &Document, uid: &str) -> Result<LayerId, ErrorBody> {
    let wanted = photocraft_doc::parse_uid(uid).ok_or_else(|| ErrorBody::new(ErrorCode::Invalid, format!("'{uid}' is not a layer uid")))?;
    doc.walk().into_iter().find(|(_, _, l)| l.uid == wanted).map(|(_, _, l)| l.id).ok_or_else(|| not_found(uid))
}

/// A blend mode as the protocol spells it: `normal`, `multiply`, `passThrough` ...
pub(super) fn blend_name(b: BlendMode) -> String {
    let name = serde_json::to_value(b).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let mut chars = name.chars();
    chars.next().map(|c| c.to_ascii_lowercase().to_string() + chars.as_str()).unwrap_or_default()
}

fn parse_blend(name: &str) -> Result<BlendMode, ErrorBody> {
    let mut chars = name.chars();
    let variant = chars.next().map(|c| c.to_ascii_uppercase().to_string() + chars.as_str()).unwrap_or_default();
    serde_json::from_value(Value::String(variant)).map_err(|_| ErrorBody::new(ErrorCode::Invalid, format!("unknown blend mode '{name}'")))
}

fn kind(l: &Layer) -> LayerKind {
    if l.video.is_some() {
        return LayerKind::Video;
    }
    match &l.content {
        LayerContent::Raster(_) => LayerKind::Pixel,
        LayerContent::Group(_) => LayerKind::Group,
        LayerContent::Shape(_) => LayerKind::Shape,
        LayerContent::Text(_) => LayerKind::Text,
        LayerContent::Smart(_) => LayerKind::Smart,
        LayerContent::Adjustment(_) => LayerKind::Adjustment,
        LayerContent::Fill(_) => LayerKind::Fill,
        #[allow(unreachable_patterns)]
        _ => LayerKind::Other,
    }
}

fn wire_rect(r: photocraft_geom::Rect) -> Rect {
    if r.is_empty() { Rect::default() } else { Rect::new(i64::from(r.x0), i64::from(r.y0), r.width(), r.height()) }
}

/// Bounds of a layer's content: its pixels, or the union of a group's children.
fn content_bounds(l: &Layer) -> photocraft_geom::Rect {
    match l.children() {
        Some(children) => children.iter().map(content_bounds).filter(|r| !r.is_empty()).reduce(|a, b| a.union(&b)).unwrap_or(photocraft_geom::Rect::EMPTY),
        None => l.surface().map(|s| s.content_bounds()).unwrap_or(photocraft_geom::Rect::EMPTY),
    }
}

/// One layer and its subtree, as the protocol describes them.
pub(super) fn describe(l: &Layer) -> LayerInfo {
    LayerInfo {
        uid: photocraft_doc::uid_string(l.uid),
        name: l.name.clone(),
        kind: kind(l),
        visible: l.visible,
        locked: l.locks.all,
        opacity: l.opacity,
        blend: blend_name(l.blend),
        bounds: wire_rect(content_bounds(l)),
        has_mask: l.mask.is_some(),
        animated: l.video.is_some(),
        children: l.children().map(|c| c.iter().map(describe).collect()).unwrap_or_default(),
    }
}

/// The document's layer tree, bottom to top.
pub(super) fn tree_of(doc: &Document) -> Vec<LayerInfo> {
    doc.layers.iter().map(describe).collect()
}

pub(super) fn tree(s: &mut Session, p: &Value) -> Outcome {
    let p: DocRef = params("layer.tree", p)?;
    let index = s.plugin_doc_index(&p.doc)?;
    let tree = s.docs.get(index).map(|st| tree_of(&st.doc)).unwrap_or_default();
    super::json(&tree)
}

pub(super) fn active(s: &mut Session, p: &Value) -> Outcome {
    let p: DocRef = params("layer.active", p)?;
    let index = s.plugin_doc_index(&p.doc)?;
    let uid = s.docs.get(index).and_then(|st| st.active_layer.and_then(|id| st.doc.layer(id)).map(|l| photocraft_doc::uid_string(l.uid)));
    super::json(&uid)
}

pub(super) fn set_active(s: &mut Session, p: &Value) -> Outcome {
    let p: LayerRef = params("layer.setActive", p)?;
    let index = s.plugin_doc_index(&p.doc)?;
    let st = s.docs.get_mut(index).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, "no such document"))?;
    let id = find(&st.doc, &p.uid)?;
    // Selecting a layer is not a history step, as in the Layers panel.
    st.active_layer = Some(id);
    st.selected_layers = vec![id];
    st.layer_anchor = Some(id);
    s.sync_tools_mask_target();
    ok()
}

pub(super) fn check_name(name: &str) -> Result<(), ErrorBody> {
    if name.len() > MAX_NAME || name.contains('\0') {
        return Err(ErrorBody::new(ErrorCode::Invalid, format!("a layer name must be at most {MAX_NAME} bytes")));
    }
    Ok(())
}

/// Insert `layer` where `placement` says: directly above `above`, else on top of `parent`'s
/// children, else on top of the document.
pub(super) fn place(doc: &mut Document, placement: &Placement, layer: Layer) -> Result<LayerId, ErrorBody> {
    if let Some(above) = &placement.above {
        let above = find(doc, above)?;
        if let Some(parent) = &placement.parent {
            let parent = find(doc, parent)?;
            let parent_path = doc.path_of(parent).unwrap_or_default();
            let above_path = doc.path_of(above).unwrap_or_default();
            if above_path.len() != parent_path.len() + 1 || !above_path.starts_with(&parent_path) {
                return Err(ErrorBody::new(ErrorCode::Invalid, "'above' is not a child of 'parent'"));
            }
        }
        return Ok(doc.insert_above(Some(above), layer));
    }
    if let Some(parent) = &placement.parent {
        let parent = find(doc, parent)?;
        let id = layer.id;
        let group = doc.layer_mut(parent).and_then(Layer::children_mut).ok_or_else(|| ErrorBody::new(ErrorCode::Invalid, "'parent' is not a group"))?;
        group.push(layer);
        return Ok(id);
    }
    let id = layer.id;
    doc.layers.push(layer);
    Ok(id)
}

pub(super) fn create(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: CreateLayerParams = params("layer.create", p)?;
    check_name(&p.name)?;
    let uid = s.plugin_edit(plugin, &p.doc, "New Layer", |doc, active| {
        let layer = match p.kind {
            NewLayerKind::Pixel => Layer::raster(p.name.clone(), doc.pixel_format()),
            NewLayerKind::Group => Layer::group(p.name.clone(), Vec::new()),
        };
        let uid = layer.uid;
        let id = place(doc, &p.placement, layer)?;
        *active = Some(id);
        Ok(uid)
    })?;
    super::json(&CreatedLayer { uid: photocraft_doc::uid_string(uid) })
}

pub(super) fn update(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: UpdateLayerParams = params("layer.update", p)?;
    if let Some(name) = &p.name {
        check_name(name)?;
    }
    let blend = p.blend.as_deref().map(parse_blend).transpose()?;
    if let Some(o) = p.opacity
        && !(0.0..=1.0).contains(&o)
    {
        return Err(ErrorBody::new(ErrorCode::Invalid, "opacity must be between 0 and 1"));
    }
    s.plugin_edit(plugin, &p.doc, "Layer Properties", |doc, _| {
        let id = find(doc, &p.uid)?;
        let layer = doc.layer_mut(id).ok_or_else(|| not_found(&p.uid))?;
        if let Some(name) = &p.name {
            layer.name.clone_from(name);
        }
        if let Some(visible) = p.visible {
            layer.visible = visible;
        }
        if let Some(locked) = p.locked {
            layer.locks.all = locked;
        }
        if let Some(blend) = blend {
            // Pass Through is for groups only.
            if blend == BlendMode::PassThrough && !layer.is_group() {
                return Err(ErrorBody::new(ErrorCode::Invalid, "only groups can use pass-through"));
            }
            layer.blend = blend;
        }
        if let Some(opacity) = p.opacity {
            layer.opacity = opacity;
        }
        Ok(())
    })?;
    ok()
}

pub(super) fn move_layer(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: MoveLayerParams = params("layer.move", p)?;
    s.plugin_edit(plugin, &p.doc, "Move Layer", |doc, _| {
        let id = find(doc, &p.uid)?;
        let path = doc.path_of(id).ok_or_else(|| not_found(&p.uid))?;
        // A group can't move into itself or anything inside it.
        for target in [&p.placement.parent, &p.placement.above].into_iter().flatten() {
            let target_path = doc.path_of(find(doc, target)?).unwrap_or_default();
            if target_path.starts_with(&path) {
                return Err(ErrorBody::new(ErrorCode::Invalid, "a layer can't move into itself"));
            }
        }
        let layer = doc.remove(id).ok_or_else(|| not_found(&p.uid))?;
        place(doc, &p.placement, layer)?;
        Ok(())
    })?;
    ok()
}

pub(super) fn delete(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: LayerRef = params("layer.delete", p)?;
    s.plugin_edit(plugin, &p.doc, "Delete Layer", |doc, active| {
        let id = find(doc, &p.uid)?;
        doc.remove(id).ok_or_else(|| not_found(&p.uid))?;
        if active.is_some_and(|a| doc.layer(a).is_none()) {
            *active = doc.top_layer();
        }
        Ok(())
    })?;
    ok()
}

/// Add a layer mask: from the selection or the layer's transparency through Layer › Layer Mask
/// (as the menu does it), or from a `gray8` image covering the canvas.
pub(super) fn add_mask(s: &mut Session, plugin: &str, p: &Value, payload: &[u8]) -> Outcome {
    let p: AddMaskParams = params("layer.addMask", p)?;
    match p.from {
        MaskSource::Image => {
            s.plugin_edit(plugin, &p.doc, "Add Layer Mask", |doc, _| {
                let id = find(doc, &p.uid)?;
                let (w, h) = (doc.size.width, doc.size.height);
                let expected = WireFormat::Gray8.payload_len(w, h).unwrap_or(u64::MAX);
                if payload.len() as u64 != expected {
                    return Err(ErrorBody::new(
                        ErrorCode::Invalid,
                        format!("a mask for this document is {w}x{h} gray8 ({expected} bytes), got {}", payload.len()),
                    ));
                }
                let mut surface = photocraft_raster::Surface::new(photocraft_color::PixelFormat::GRAY8);
                surface.write_interleaved(photocraft_geom::Rect::from_xywh(0, 0, w, h), payload);
                let layer = doc.layer_mut(id).ok_or_else(|| not_found(&p.uid))?;
                let mut mask = photocraft_doc::LayerMask::reveal_all();
                mask.surface = surface;
                layer.mask = Some(mask);
                Ok(())
            })?;
        }
        MaskSource::Selection | MaskSource::Alpha => {
            let command = if p.from == MaskSource::Selection { "layer.layerMask.revealSelection" } else { "layer.layerMask.fromTransparency" };
            let doc_id = s.docs.get(s.plugin_doc_index(&p.doc)?).map(|st| st.doc.id).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, "no such document"))?;
            let key = super::undo::open_group(s, plugin, doc_id).map(|(k, _)| k);
            s.with_plugin_doc(&p.doc, |s| {
                let index = s.active.ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, "no such document"))?;
                let id = s.docs.get(index).map(|st| find(&st.doc, &p.uid)).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, "no such document"))??;
                let mut args = json!({"layer": id.0});
                if let (Some(key), Some(map)) = (key, args.as_object_mut()) {
                    map.insert("coalesce".into(), Value::String(key));
                }
                s.execute(command, args).map(|_| ()).map_err(engine_error)
            })?;
        }
    }
    ok()
}

/// A PNG of one layer on its own, at most `width` x `height`.
pub(super) fn thumbnail(s: &mut Session, p: &Value) -> Outcome {
    let p: ThumbnailParams = params("layer.thumbnail", p)?;
    if p.width == 0 || p.height == 0 {
        return Err(ErrorBody::new(ErrorCode::Invalid, "the thumbnail size must not be zero"));
    }
    let index = s.plugin_doc_index(&p.doc)?;
    let doc = s.docs.get(index).map(|st| st.doc.clone()).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, "no such document"))?;
    let id = find(&doc, &p.uid)?;
    let mut layer = doc.layer(id).cloned().ok_or_else(|| not_found(&p.uid))?;
    layer.visible = true;
    let mut solo = (*doc).clone();
    solo.layers = vec![layer];
    let side = p.width.max(p.height).min(MAX_THUMBNAIL);
    let image = photocraft_compose::thumbnail(&solo, side);
    let encoded = photocraft_codecs::Image::from_u8(image.width, image.height, photocraft_codecs::ChannelLayout::Rgba, image.pixels)
        .and_then(|img| photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &photocraft_codecs::EncodeOptions::default()))
        .map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("could not encode the thumbnail: {e}")))?;
    Ok((Value::Object(serde_json::Map::new()), encoded))
}
