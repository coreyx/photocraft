//! `doc.*`: listing, activating, resizing and opening documents.

use photocraft_color::{ColorMode, SampleType};
use photocraft_process_plugin_protocol::message::{ErrorBody, ErrorCode};
use photocraft_process_plugin_protocol::method::{Anchor, DocRef, OpenParams, ResizeCanvasParams, ResizeImageParams};
use photocraft_process_plugin_protocol::types::{self, DocInfo};
use serde_json::{Value, json};

use super::{Outcome, engine_error, ok, params};
use crate::Session;

/// Largest canvas side a plug-in may ask for, as Photoshop's PSB limit.
const MAX_SIDE: u32 = 300_000;

fn depth(d: SampleType) -> types::Depth {
    match d {
        SampleType::U8 => types::Depth::U8,
        SampleType::U16 => types::Depth::U16,
        SampleType::F32 => types::Depth::F32,
    }
}

fn mode(m: ColorMode) -> types::ColorMode {
    match m {
        ColorMode::Bitmap => types::ColorMode::Bitmap,
        ColorMode::Grayscale => types::ColorMode::Grayscale,
        ColorMode::Indexed => types::ColorMode::Indexed,
        ColorMode::Rgb => types::ColorMode::Rgb,
        ColorMode::Cmyk => types::ColorMode::Cmyk,
        ColorMode::Lab => types::ColorMode::Lab,
        ColorMode::Multichannel => types::ColorMode::Multichannel,
        ColorMode::Duotone => types::ColorMode::Duotone,
    }
}

/// What the protocol says about the document at `index`.
pub(super) fn describe(s: &Session, index: usize) -> Option<DocInfo> {
    let st = s.docs.get(index)?;
    let d = &st.doc;
    Some(DocInfo {
        doc: d.id.0.to_string(),
        name: d.name.clone(),
        path: st.path.clone(),
        width: d.size.width,
        height: d.size.height,
        dpi: d.resolution_dpi,
        depth: depth(d.depth),
        mode: mode(d.mode),
        active: s.active == Some(index),
    })
}

pub(super) fn list(s: &mut Session) -> Outcome {
    let docs: Vec<DocInfo> = (0..s.docs.len()).filter_map(|i| describe(s, i)).collect();
    super::json(&docs)
}

pub(super) fn info(s: &mut Session, p: &Value) -> Outcome {
    let p: DocRef = params("doc.info", p)?;
    let index = s.plugin_doc_index(&p.doc)?;
    super::json(&describe(s, index))
}

pub(super) fn set_active(s: &mut Session, p: &Value) -> Outcome {
    let p: DocRef = params("doc.setActive", p)?;
    let index = s.plugin_doc_index(&p.doc)?;
    s.set_active(index);
    ok()
}

fn anchor_name(a: Anchor) -> &'static str {
    match a {
        Anchor::TopLeft => "topLeft",
        Anchor::Top => "top",
        Anchor::TopRight => "topRight",
        Anchor::Left => "left",
        Anchor::Center => "center",
        Anchor::Right => "right",
        Anchor::BottomLeft => "bottomLeft",
        Anchor::Bottom => "bottom",
        Anchor::BottomRight => "bottomRight",
    }
}

/// Resize the canvas through Image › Canvas Size, so it behaves exactly as the menu command
/// (anchoring, layer offsets, guides, samplers).
pub(super) fn resize_canvas(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: ResizeCanvasParams = params("doc.resizeCanvas", p)?;
    if p.width == 0 || p.height == 0 || p.width > MAX_SIDE || p.height > MAX_SIDE {
        return Err(ErrorBody::new(ErrorCode::Invalid, format!("canvas size must be 1 to {MAX_SIDE} pixels a side")));
    }
    let doc_id = s.docs.get(s.plugin_doc_index(&p.doc)?).map(|st| st.doc.id).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, "no such document"))?;
    let key = super::undo::open_group(s, plugin, doc_id).map(|(k, _)| k);
    s.with_plugin_doc(&p.doc, |s| {
        let mut args = json!({"width": p.width, "height": p.height, "anchor": anchor_name(p.anchor)});
        if let (Some(key), Some(map)) = (key, args.as_object_mut()) {
            map.insert("coalesce".into(), Value::String(key));
        }
        s.execute("image.canvasSize", args).map(|_| ()).map_err(engine_error)
    })?;
    ok()
}

/// Resample the image (Image › Image Size, bilinear) as one undoable step, or part of the
/// plug-in's open undo group.
pub(super) fn resize_image(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: ResizeImageParams = params("doc.resizeImage", p)?;
    if p.width == 0 || p.height == 0 || p.width > MAX_SIDE || p.height > MAX_SIDE {
        return Err(ErrorBody::new(ErrorCode::Invalid, format!("image size must be 1 to {MAX_SIDE} pixels a side")));
    }
    if p.dpi.is_some_and(|dpi| !dpi.is_finite() || dpi <= 0.0) {
        return Err(ErrorBody::new(ErrorCode::Invalid, "the resolution must be a positive number"));
    }
    let doc_id = s.docs.get(s.plugin_doc_index(&p.doc)?).map(|st| st.doc.id).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, "no such document"))?;
    let key = super::undo::open_group(s, plugin, doc_id).map(|(k, _)| k);
    s.with_plugin_doc(&p.doc, |s| {
        let mut args = json!({"width": p.width, "height": p.height, "resample": "bilinear"});
        if let Some(map) = args.as_object_mut() {
            if let Some(dpi) = p.dpi {
                map.insert("resolution".into(), json!(dpi));
            }
            if let Some(key) = key {
                map.insert("coalesce".into(), Value::String(key));
            }
        }
        s.execute("image.imageSize", args).map(|_| ()).map_err(engine_error)
    })?;
    ok()
}

/// Open a file as a new document and make it active.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn open(s: &mut Session, p: &Value) -> Outcome {
    let p: OpenParams = params("doc.open", p)?;
    let path = std::path::Path::new(&p.path);
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).ok_or_else(|| ErrorBody::new(ErrorCode::Invalid, "the path names no file"))?;
    let bytes = std::fs::read(path).map_err(|e| ErrorBody::new(ErrorCode::NotFound, format!("cannot read {}: {e}", p.path)))?;
    let imported = photocraft_io::import(&name, &bytes).map_err(|e| ErrorBody::new(ErrorCode::Invalid, format!("cannot open {}: {e}", p.path)))?;
    let index = s.add_document(imported.document, Some(p.path.clone()));
    let doc = s.docs.get(index).map(|st| st.doc.id.0.to_string()).unwrap_or_default();
    super::json(&DocRef { doc })
}

/// The browser build has no file system for plug-ins to name.
#[cfg(target_arch = "wasm32")]
pub(super) fn open(_s: &mut Session, _p: &Value) -> Outcome {
    Err(ErrorBody::new(ErrorCode::Unsupported, "opening files by path is not available here"))
}
