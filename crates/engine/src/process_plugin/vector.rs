//! `vector.*`: editable vector content (OpenPose skeletons) as a group of shape layers.
//!
//! PhotoCraft has no multi-shape vector layer, so a plug-in's vector content is a group marked
//! as such in its layer data, holding one shape layer per SVG `<circle>` or `<line>`. The user
//! moves joints with the Move and Path Selection tools and copies them with Duplicate Layer, all
//! existing tools; the change events report it. Each shape keeps its SVG element id in its layer
//! data, so a duplicate carries the same id (the plug-in treats a repeated id as a new element,
//! as Krita AI Diffusion does). Every element also has a distinct handle, its layer uid.
//!
//! Geometry is read back from the shapes' paths: a closed path is a circle (its centre and
//! radius from the path's bounds), an open one a line (its first and last anchors).

use photocraft_doc::{Document, Fill, Layer, LayerContent, ShapeLayer};
use photocraft_process_plugin_protocol::message::{ErrorBody, ErrorCode};
use photocraft_process_plugin_protocol::method::{AddSvgParams, CreateVectorParams, CreatedLayer, LayerRef, RemoveShapesParams, VectorShape, VectorShapes};
use serde_json::Value;

use super::layers::{check_name, find, place};
use super::{Outcome, json, ok, params};
use crate::Session;

/// Layer data keys, in the calling plug-in's namespace.
const GROUP_KEY: &str = "vector.group";
const ID_KEY: &str = "vector.id";
/// Largest SVG a request may carry, and most elements one group may hold.
const MAX_SVG: usize = 4 << 20;
const MAX_SHAPES: usize = 5000;
/// Most pixels the SVG import may render while parsing (it renders nothing it can keep as paths).
const MAX_IMPORT_PIXELS: u64 = 1 << 26;

fn key(plugin: &str, name: &str) -> String {
    format!("{plugin}/{name}")
}

fn invalid(message: impl Into<String>) -> ErrorBody {
    ErrorBody::new(ErrorCode::Invalid, message)
}

/// The shape layers of `svg`, in paint order, each named and tagged with its element id and
/// cached for `doc`. Only circles and lines (paths PhotoCraft keeps as shapes) are accepted.
fn shapes_from_svg(plugin: &str, doc: &Document, svg: &str) -> Result<Vec<Layer>, ErrorBody> {
    if svg.len() > MAX_SVG {
        return Err(ErrorBody::new(ErrorCode::TooLarge, format!("the SVG is larger than {MAX_SVG} bytes")));
    }
    let imported = photocraft_io::svg::import_svg_bounded_layers("vector.svg", svg.as_bytes(), MAX_IMPORT_PIXELS)
        .map_err(|e| invalid(format!("the SVG can't be read: {e}")))?
        .document;
    let mut out = Vec::new();
    for (_, _, layer) in imported.walk() {
        match &layer.content {
            LayerContent::Group(_) => continue,
            LayerContent::Shape(shape) => {
                if out.len() >= MAX_SHAPES {
                    return Err(ErrorBody::new(ErrorCode::TooLarge, format!("more than {MAX_SHAPES} elements")));
                }
                let mut shape = shape.clone();
                shape.cache = Some(photocraft_vector::render_shape(&shape, doc.pixel_format(), doc.bounds()));
                let mut element = Layer::new(layer.name.clone(), LayerContent::Shape(shape));
                element.plugin_data.insert(key(plugin, ID_KEY), layer.name.clone());
                out.push(element);
            }
            _ => return Err(invalid(format!("'{}' isn't a circle or a line", layer.name))),
        }
    }
    if out.is_empty() {
        return Err(invalid("the SVG has no circles or lines"));
    }
    Ok(out)
}

/// The vector group with uid `uid` in `doc`, refused when it isn't one of `plugin`'s.
fn group<'a>(plugin: &str, doc: &'a Document, uid: &str) -> Result<&'a Layer, ErrorBody> {
    let id = find(doc, uid)?;
    let layer = doc.layer(id).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("no layer '{uid}'")))?;
    if !layer.is_group() || !layer.plugin_data.contains_key(&key(plugin, GROUP_KEY)) {
        return Err(invalid(format!("'{uid}' isn't a vector layer")));
    }
    Ok(layer)
}

fn hex(fill: &Fill) -> String {
    match fill {
        Fill::Solid(c) => {
            let [r, g, b, _] = c.to_rgba8();
            format!("#{r:02x}{g:02x}{b:02x}")
        }
        _ => "#000000".into(),
    }
}

/// A shape as an SVG `<circle>` or `<line>` with its current geometry, or `None` when its path
/// is neither (the user reshaped it).
fn fragment(id: &str, shape: &ShapeLayer, layer_opacity: f32) -> Option<String> {
    let sub = shape.path.subpaths.first()?;
    let id = escape(id);
    if sub.closed {
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for k in &sub.knots {
            x0 = x0.min(k.anchor.x);
            y0 = y0.min(k.anchor.y);
            x1 = x1.max(k.anchor.x);
            y1 = y1.max(k.anchor.y);
        }
        if x0 > x1 {
            return None;
        }
        let fill = shape.fill.as_ref().map_or_else(|| "none".to_string(), hex);
        let (cx, cy, r) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0, ((x1 - x0) + (y1 - y0)) / 4.0);
        let opacity = if layer_opacity < 1.0 { format!(" opacity=\"{layer_opacity}\"") } else { String::new() };
        Some(format!("<circle id=\"{id}\" cx=\"{cx}\" cy=\"{cy}\" r=\"{r}\" fill=\"{fill}\"{opacity}/>"))
    } else {
        let (a, b) = (sub.knots.first()?.anchor, sub.knots.last()?.anchor);
        let stroke = shape.stroke.as_ref()?;
        let opacity = stroke.opacity * layer_opacity;
        Some(format!(
            "<line id=\"{id}\" x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"{}\" stroke-width=\"{}\" stroke-opacity=\"{opacity}\"/>",
            a.x,
            a.y,
            b.x,
            b.y,
            hex(&stroke.paint),
            stroke.width
        ))
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

pub(super) fn create(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: CreateVectorParams = params("vector.createFromSvg", p)?;
    check_name(&p.name)?;
    let uid = s.plugin_edit(plugin, &p.doc, "New Vector Layer", |doc, active| {
        let children = shapes_from_svg(plugin, doc, &p.svg)?;
        let mut group = Layer::group(p.name.clone(), children);
        if let LayerContent::Group(g) = &mut group.content {
            // Shown collapsed, so a skeleton's dozens of shapes stay tidy in the Layers panel.
            g.expanded = false;
        }
        group.plugin_data.insert(key(plugin, GROUP_KEY), "1".into());
        let uid = group.uid;
        *active = Some(place(doc, &p.placement, group)?);
        Ok(uid)
    })?;
    json(&CreatedLayer { uid: photocraft_doc::uid_string(uid) })
}

pub(super) fn shapes(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: LayerRef = params("vector.shapes", p)?;
    let index = s.plugin_doc_index(&p.doc)?;
    let doc = s.docs.get(index).map(|st| st.doc.clone()).ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, "no such document"))?;
    let group = group(plugin, &doc, &p.uid)?;
    let id_key = key(plugin, ID_KEY);
    let mut shapes = Vec::new();
    for layer in group.children().unwrap_or_default() {
        let LayerContent::Shape(shape) = &layer.content else { continue };
        let id = layer.plugin_data.get(&id_key).cloned().unwrap_or_else(|| layer.name.clone());
        if let Some(svg) = fragment(&id, shape, layer.opacity) {
            shapes.push(VectorShape { id, handle: photocraft_doc::uid_string(layer.uid), svg });
        }
    }
    json(&VectorShapes { shapes })
}

pub(super) fn add(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: AddSvgParams = params("vector.addSvg", p)?;
    s.plugin_edit(plugin, &p.doc, "Edit Vector Layer", |doc, _| {
        let id = group(plugin, doc, &p.uid)?.id;
        let new = shapes_from_svg(plugin, doc, &p.svg)?;
        let children = doc.layer_mut(id).and_then(Layer::children_mut).ok_or_else(|| invalid("not a vector layer"))?;
        if children.len() + new.len() > MAX_SHAPES {
            return Err(ErrorBody::new(ErrorCode::TooLarge, format!("more than {MAX_SHAPES} elements")));
        }
        children.extend(new);
        Ok(())
    })?;
    ok()
}

pub(super) fn remove(s: &mut Session, plugin: &str, p: &Value) -> Outcome {
    let p: RemoveShapesParams = params("vector.removeShapes", p)?;
    let handles: Vec<u128> =
        p.ids.iter().map(|h| photocraft_doc::parse_uid(h).ok_or_else(|| invalid(format!("'{h}' is not a shape handle")))).collect::<Result<_, _>>()?;
    s.plugin_edit(plugin, &p.doc, "Edit Vector Layer", |doc, _| {
        let id = group(plugin, doc, &p.uid)?.id;
        let children = doc.layer_mut(id).and_then(Layer::children_mut).ok_or_else(|| invalid("not a vector layer"))?;
        children.retain(|l| !handles.contains(&l.uid));
        Ok(())
    })?;
    ok()
}
