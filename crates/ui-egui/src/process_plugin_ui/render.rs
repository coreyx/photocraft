//! Drawing a plug-in widget tree with PhotoCraft's widgets, and turning what the user does into
//! `panel.input` / `dialog.input` events ([`InputEvent`]).
//!
//! Every widget kind of the protocol is drawn here; the per-kind props are documented on
//! [`WidgetKind`]. Common props: `visible` and `enabled` (default true), `tooltip`, `grow`.
//! A change the user makes shows at once from the tree's local values (see [`super::Tree`]).

use std::collections::HashMap;

use egui::{Align, Layout, Rect, RichText, Sense, Ui, vec2};
use photocraft_process_plugin_protocol::event::{InputEvent, InputKind};
use photocraft_process_plugin_protocol::types::Modifiers;
use photocraft_process_plugin_protocol::widget::{Node, WidgetKind};
use serde_json::{Value, json};

use super::{ImageCache, Tree};
use crate::theme::Tokens;

/// What drawing one tree needs and produces.
struct Cx<'a> {
    plugin: &'a str,
    owner: &'a str,
    images: &'a mut ImageCache,
    local: &'a mut HashMap<(String, &'static str), Value>,
    menus: HashMap<&'a str, &'a Node>,
    /// The widgets pop-ups are anchored to, and where each was drawn this frame.
    anchors: std::collections::HashSet<String>,
    rects: HashMap<String, Rect>,
    events: Vec<InputEvent>,
}

/// Draw `tree` (belonging to `plugin`'s panel or dialog `owner`) and return what the user did.
pub fn show(ui: &mut Ui, plugin: &str, owner: &str, tree: &mut Tree, images: &mut ImageCache) -> Vec<InputEvent> {
    let Tree { root, local } = tree;
    let mut menus = HashMap::new();
    let mut popups = Vec::new();
    let mut stack = vec![&*root];
    while let Some(n) = stack.pop() {
        match n.kind {
            WidgetKind::Menu => {
                menus.insert(n.id.as_str(), n);
            }
            WidgetKind::Popup => popups.push(n),
            _ => {}
        }
        stack.extend(n.children.iter());
    }
    let anchors = popups.iter().map(|p| text(p, "anchor").to_string()).collect();
    let mut cx = Cx { plugin, owner, images, local, menus, anchors, rects: HashMap::new(), events: Vec::new() };
    node(ui, root, &mut cx);
    for popup in popups {
        show_popup(ui, popup, &mut cx);
    }
    cx.events
}

// ----------------------------------------------------------------------------- props

fn prop<'n>(n: &'n Node, key: &str) -> Option<&'n Value> {
    n.props.get(key)
}

fn text<'n>(n: &'n Node, key: &str) -> &'n str {
    prop(n, key).and_then(Value::as_str).unwrap_or("")
}

fn flag(n: &Node, key: &str, default: bool) -> bool {
    prop(n, key).and_then(Value::as_bool).unwrap_or(default)
}

fn num(n: &Node, key: &str, default: f64) -> f64 {
    prop(n, key).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(default)
}

fn items(n: &Node) -> &[Value] {
    prop(n, "items").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn item_str<'v>(item: &'v Value, key: &str) -> &'v str {
    item.get(key).and_then(Value::as_str).unwrap_or("")
}

impl Cx<'_> {
    /// The value the widget shows for `key`: the user's unconfirmed one, else the plug-in's.
    fn get(&self, n: &Node, key: &'static str) -> Option<Value> {
        self.local.get(&(n.id.clone(), key)).cloned().or_else(|| prop(n, key).cloned())
    }

    /// Show `value` for `key` until the plug-in sets it.
    fn set(&mut self, n: &Node, key: &'static str, value: Value) {
        self.local.insert((n.id.clone(), key), value);
    }

    fn emit(&mut self, ui: &Ui, n: &Node, kind: InputKind, value: Value) {
        let m = ui.input(|i| i.modifiers);
        let modifiers = Modifiers { shift: m.shift, ctrl: m.ctrl, alt: m.alt, meta: m.mac_cmd || (m.command && !m.ctrl) };
        self.events.push(InputEvent { owner: self.owner.to_string(), node_id: n.id.clone(), kind, value, modifiers });
    }

    /// The user changed a value: show it and tell the plug-in.
    fn change(&mut self, ui: &Ui, n: &Node, key: &'static str, value: Value) {
        self.set(n, key, value.clone());
        self.emit(ui, n, InputKind::Change, value);
    }
}

// ----------------------------------------------------------------------------- tree

fn node(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) {
    if !flag(n, "visible", true) || matches!(n.kind, WidgetKind::Menu | WidgetKind::Popup) {
        return;
    }
    let enabled = flag(n, "enabled", true);
    let response = ui.add_enabled_ui(enabled, |ui| widget(ui, n, cx)).inner;
    let response = match prop(n, "tooltip").and_then(Value::as_str) {
        Some(tip) if !tip.is_empty() => response.on_hover_text(tip),
        _ => response,
    };
    if cx.anchors.contains(&n.id) {
        cx.rects.insert(n.id.clone(), response.rect);
    }
}

fn children(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) {
    for child in &n.children {
        node(ui, child, cx);
    }
}

/// Draw `n`, returning its response (for its tooltip and as a pop-up anchor).
fn widget(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    use WidgetKind as K;
    match n.kind {
        K::Row => ui.horizontal(|ui| children(ui, n, cx)).response,
        K::Column => ui.vertical(|ui| children(ui, n, cx)).response,
        K::Grid => grid(ui, n, cx),
        K::Form => form(ui, n, cx),
        K::Group => group(ui, n, cx),
        K::Collapsible => collapsible(ui, n, cx),
        K::Tabs => tabs(ui, n, cx),
        K::Stack => {
            let current = cx.get(n, "current").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            ui.vertical(|ui| {
                if let Some(page) = n.children.get(current) {
                    node(ui, page, cx);
                }
            })
            .response
        }
        K::Scroll => {
            let max = num(n, "maxHeight", f64::INFINITY) as f32;
            egui::ScrollArea::vertical()
                .id_salt(("plugin-scroll", &n.id))
                .max_height(max)
                .auto_shrink([false, true])
                .show(ui, |ui| children(ui, n, cx))
                .inner_rect_response(ui)
        }
        K::Splitter => splitter(ui, n, cx),
        K::Spacer => {
            let size = num(n, "size", 8.0) as f32;
            let grow = num(n, "grow", 0.0) > 0.0;
            let horizontal = ui.layout().main_dir().is_horizontal();
            let space = if grow { if horizontal { ui.available_width() } else { ui.available_height().min(1e4) } } else { size };
            ui.allocate_response(if horizontal { vec2(space.max(0.0), 1.0) } else { vec2(1.0, space.max(0.0)) }, Sense::hover())
        }
        K::Label => label(ui, n, cx),
        K::Icon => {
            let size = num(n, "size", 16.0) as f32;
            let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
            paint_icon(ui, rect, text(n, "icon"), cx);
            response
        }
        K::Image => image(ui, n, cx),
        K::Progress => {
            let value = prop(n, "value").and_then(Value::as_f64).map(|v| v.clamp(0.0, 1.0) as f32);
            // A bar with text inside is as tall as the text; a bare one is a thin line.
            let height = if text(n, "text").is_empty() { 8.0 } else { 18.0 };
            let mut bar = egui::ProgressBar::new(value.unwrap_or(0.0)).desired_height(height);
            if value.is_none() {
                bar = bar.animate(true);
            }
            if !text(n, "text").is_empty() {
                bar = bar.text(text(n, "text"));
            }
            ui.add(bar)
        }
        K::Separator => {
            if text(n, "orientation") == "vertical" {
                ui.add(egui::Separator::default().vertical())
            } else {
                ui.separator()
            }
        }
        K::Button => button(ui, n, cx),
        K::ToolButton => tool_button(ui, n, cx),
        K::ToggleButton => {
            let mut on = cx.get(n, "checked").and_then(|v| v.as_bool()).unwrap_or(false);
            let r = ui.toggle_value(&mut on, text(n, "text"));
            if r.clicked() {
                cx.change(ui, n, "checked", json!(on));
            }
            r
        }
        K::SplitButton => split_button(ui, n, cx),
        K::Checkbox => {
            let mut on = cx.get(n, "checked").and_then(|v| v.as_bool()).unwrap_or(false);
            let r = crate::widgets::checkbox(ui, &mut on, text(n, "text"));
            if r.changed() || r.clicked() {
                cx.change(ui, n, "checked", json!(on));
            }
            r
        }
        K::Switch => {
            let mut on = cx.get(n, "checked").and_then(|v| v.as_bool()).unwrap_or(false);
            let r = crate::widgets::toggle(ui, &mut on, text(n, "text"));
            if r.changed() || r.clicked() {
                cx.change(ui, n, "checked", json!(on));
            }
            r
        }
        K::RadioGroup => radio_group(ui, n, cx),
        K::Combo => combo(ui, n, cx),
        K::Slider => slider(ui, n, cx, false),
        K::Spin => spin(ui, n, cx),
        K::SliderSpin => slider(ui, n, cx, true),
        K::LineEdit => line_edit(ui, n, cx),
        K::TextEdit => text_edit(ui, n, cx),
        K::PathField => path_field(ui, n, cx),
        K::List => list(ui, n, cx),
        K::ThumbGrid => thumb_grid(ui, n, cx),
        K::Repeater => ui.vertical(|ui| children(ui, n, cx)).response,
        K::Popup | K::Menu => ui.allocate_response(vec2(0.0, 0.0), Sense::hover()),
    }
}

/// [`egui::scroll_area::ScrollAreaOutput`] has no response; give the caller one for its rect.
trait InnerRectResponse {
    fn inner_rect_response(self, ui: &Ui) -> egui::Response;
}

impl<R> InnerRectResponse for egui::scroll_area::ScrollAreaOutput<R> {
    fn inner_rect_response(self, ui: &Ui) -> egui::Response {
        ui.interact(self.inner_rect, self.id.with("plugin-rect"), Sense::hover())
    }
}

// ----------------------------------------------------------------------------- layout

fn grid(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    // Children name their cell; spans aren't drawn (the cell takes the first column it spans).
    let mut cells: Vec<(u64, u64, &Node)> = n.children.iter().map(|c| (num(c, "row", 0.0) as u64, num(c, "col", 0.0) as u64, c)).collect();
    cells.sort_by_key(|(r, c, _)| (*r, *c));
    egui::Grid::new(("plugin-grid", &n.id))
        .spacing(vec2(8.0, 4.0))
        .show(ui, |ui| {
            let mut row = cells.first().map_or(0, |c| c.0);
            let mut col = 0;
            for (r, c, child) in cells {
                while row < r {
                    ui.end_row();
                    row += 1;
                    col = 0;
                }
                while col < c {
                    ui.label("");
                    col += 1;
                }
                node(ui, child, cx);
                col += 1;
            }
        })
        .response
}

fn form(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    egui::Grid::new(("plugin-form", &n.id))
        .num_columns(2)
        .spacing(vec2(8.0, 6.0))
        .show(ui, |ui| {
            for pair in n.children.chunks(2) {
                for child in pair {
                    node(ui, child, cx);
                }
                ui.end_row();
            }
        })
        .response
}

fn group(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    egui::Frame::group(ui.style())
        .stroke(egui::Stroke::new(1.0, t.field_border))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                let mut open = true;
                if flag(n, "checkable", false) {
                    let mut on = cx.get(n, "checked").and_then(|v| v.as_bool()).unwrap_or(true);
                    if crate::widgets::checkbox(ui, &mut on, text(n, "title")).clicked() {
                        cx.change(ui, n, "checked", json!(on));
                    }
                    open = on;
                } else if !text(n, "title").is_empty() {
                    crate::widgets::section_label(ui, text(n, "title"));
                }
                ui.add_enabled_ui(open, |ui| children(ui, n, cx));
            })
        })
        .response
}

fn collapsible(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    let expanded = cx.get(n, "expanded").and_then(|v| v.as_bool()).unwrap_or(false);
    let header = egui::CollapsingHeader::new(text(n, "title")).id_salt(("plugin-collapsible", &n.id)).open(Some(expanded)).show(ui, |ui| children(ui, n, cx));
    if header.header_response.clicked() {
        cx.change(ui, n, "expanded", json!(!expanded));
    }
    header.header_response
}

fn tabs(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    let current = cx.get(n, "current").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            for (i, page) in n.children.iter().enumerate() {
                if crate::widgets::pill_tab(ui, text(page, "title"), i == current).clicked() && i != current {
                    cx.change(ui, n, "current", json!(i));
                }
            }
        });
        ui.add_space(4.0);
        if let Some(page) = n.children.get(current) {
            ui.vertical(|ui| children(ui, page, cx));
        }
    })
    .response
}

fn splitter(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    // Panes stacked with a divider the user drags to give the first pane more or less room.
    let vertical = text(n, "orientation") != "horizontal";
    ui.vertical(|ui| {
        for (i, child) in n.children.iter().enumerate() {
            if i > 0 {
                ui.separator();
            }
            if vertical && i + 1 < n.children.len() {
                let id = ui.scope_id().with(("plugin-splitter", &n.id, i));
                let default = prop(n, "sizes").and_then(|s| s.get(i)).and_then(Value::as_f64).unwrap_or(160.0) as f32;
                egui::Resize::default().id_salt(id).default_height(default).min_height(24.0).resizable([false, true]).show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt(id.with("scroll")).show(ui, |ui| node(ui, child, cx));
                });
            } else {
                node(ui, child, cx);
            }
        }
    })
    .response
}

// ----------------------------------------------------------------------------- display

/// `format: "rich"` text: `<b>`, `<i>` and `<a href="…">` are drawn, other tags dropped.
fn rich_runs(text: &str) -> Vec<(String, bool, bool, Option<String>)> {
    let mut runs = Vec::new();
    let (mut bold, mut italic, mut link): (bool, bool, Option<String>) = (false, false, None);
    let mut rest = text;
    let mut buf = String::new();
    while let Some(lt) = rest.find('<') {
        buf.push_str(&rest[..lt]);
        let Some(gt) = rest[lt..].find('>') else {
            buf.push_str(&rest[lt..]);
            rest = "";
            break;
        };
        let tag = rest[lt + 1..lt + gt].trim().to_ascii_lowercase();
        let raw = &rest[lt + 1..lt + gt];
        if !buf.is_empty() {
            runs.push((std::mem::take(&mut buf), bold, italic, link.clone()));
        }
        match tag.as_str() {
            "b" | "strong" => bold = true,
            "/b" | "/strong" => bold = false,
            "i" | "em" => italic = true,
            "/i" | "/em" => italic = false,
            "/a" => link = None,
            "br" | "br/" | "br /" => runs.push(("\n".into(), bold, italic, link.clone())),
            t if t.starts_with("a ") => {
                link = raw.split("href=").nth(1).map(|v| v.trim_start_matches(['"', '\'']).split(['"', '\'']).next().unwrap_or("").to_string());
            }
            _ => {}
        }
        rest = &rest[lt + gt + 1..];
    }
    buf.push_str(rest);
    if !buf.is_empty() {
        runs.push((buf, bold, italic, link));
    }
    let unescape = |s: String| s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&nbsp;", " ").replace("&amp;", "&");
    runs.into_iter().map(|(s, b, i, l)| (unescape(s), b, i, l)).collect()
}

fn label(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let wrap = flag(n, "wrap", false);
    let content = text(n, "text");
    if text(n, "format") == "rich" {
        let runs = rich_runs(content);
        let inner = ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (s, bold, italic, link) in runs {
                let mut rt = RichText::new(s);
                if bold {
                    rt = rt.strong();
                }
                if italic {
                    rt = rt.italics();
                }
                match link {
                    Some(href) => {
                        if ui.link(rt.color(t.accent)).clicked() {
                            cx.emit(ui, n, InputKind::Click, json!(href));
                        }
                    }
                    None => {
                        ui.label(rt);
                    }
                }
            }
        });
        return inner.response;
    }
    if flag(n, "elide", false) {
        return crate::widgets::elided_label(ui, content, ui.available_width());
    }
    let mut l = egui::Label::new(content);
    if wrap {
        l = l.wrap();
    }
    if flag(n, "link", false) {
        let r = ui.link(RichText::new(content).color(t.accent));
        if r.clicked() {
            cx.emit(ui, n, InputKind::Click, json!(1));
        }
        return r;
    }
    ui.add(l)
}

fn paint_icon(ui: &Ui, rect: Rect, reference: &str, cx: &mut Cx<'_>) {
    let t = Tokens::get(ui.ctx());
    if let Some(name) = reference.strip_prefix("icon:") {
        crate::icons::paint(ui, rect, name, rect.height().min(rect.width()) * 0.8, t.text);
    } else if let Some(texture) = cx.images.texture(ui.ctx(), cx.plugin, reference) {
        ui.painter().image(texture.id(), rect, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
    }
}

fn image(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    let texture = cx.images.texture(ui.ctx(), cx.plugin, text(n, "image"));
    let natural = texture.as_ref().map_or(vec2(64.0, 64.0), |t| t.size_vec2());
    let w = num(n, "width", f64::from(natural.x)) as f32;
    let h = num(n, "height", f64::from(natural.y)) as f32;
    let box_size = vec2(w.min(ui.available_width().max(1.0)), h);
    let sense = if flag(n, "interactive", false) { Sense::click_and_drag() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(box_size, sense);
    if let Some(texture) = texture {
        // `fit: contain` (default) keeps the aspect ratio inside the box; `fill` stretches.
        let shown = if text(n, "fit") == "fill" {
            rect
        } else {
            let k = (rect.width() / natural.x).min(rect.height() / natural.y);
            Rect::from_center_size(rect.center(), natural * k)
        };
        ui.painter().image(texture.id(), shown, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
    }
    if flag(n, "interactive", false)
        && let Some(pos) = response.interact_pointer_pos()
    {
        let action = if response.drag_started() || response.clicked() {
            "press"
        } else if response.drag_stopped() {
            "release"
        } else if response.dragged() {
            "move"
        } else {
            ""
        };
        if !action.is_empty() {
            let button = if response.secondary_clicked() { "right" } else { "left" };
            let local = pos - rect.min;
            cx.emit(ui, n, InputKind::Pointer, json!({"x": local.x, "y": local.y, "button": button, "action": action}));
        }
    }
    response
}

// ----------------------------------------------------------------------------- buttons, menus

/// Menu entries from `items` (or the `Menu` node a `menu` prop names), each sending a `menu`
/// event with its id; entries with `items` open a submenu.
fn menu_entries(ui: &mut Ui, n: &Node, entries: &[Value], cx: &mut Cx<'_>) {
    for entry in entries {
        if item_str(entry, "kind") == "separator" {
            ui.separator();
            continue;
        }
        let label = item_str(entry, "text");
        let enabled = entry.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        if let Some(sub) = entry.get("items").and_then(Value::as_array) {
            ui.menu_button(label, |ui| menu_entries(ui, n, sub, cx));
            continue;
        }
        let checkable = entry.get("checkable").and_then(Value::as_bool).unwrap_or(false);
        let r = if checkable {
            let mut on = entry.get("checked").and_then(Value::as_bool).unwrap_or(false);
            ui.add_enabled(enabled, egui::Checkbox::new(&mut on, label))
        } else {
            ui.add_enabled(enabled, egui::Button::new(label))
        };
        if r.clicked() {
            cx.emit(ui, n, InputKind::Menu, json!(item_str(entry, "id")));
            ui.close();
        }
    }
}

/// A widget's menu: inline `menu` items, or the id of a `Menu` node.
fn menu_of<'a>(n: &'a Node, cx: &Cx<'a>) -> Option<Vec<Value>> {
    match prop(n, "menu")? {
        Value::Array(entries) => Some(entries.clone()),
        Value::String(id) => cx.menus.get(id.as_str()).map(|m| items(m).to_vec()),
        _ => None,
    }
}

fn button(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    let label = text(n, "text");
    let r = if flag(n, "default", false) {
        crate::widgets::primary_button(ui, label, 0.0)
    } else if prop(n, "icon").is_some() && label.is_empty() {
        icon_button(ui, n, cx)
    } else {
        crate::widgets::secondary_button(ui, label, 0.0)
    };
    if r.clicked() {
        cx.emit(ui, n, InputKind::Click, json!(1));
    }
    r
}

fn icon_button(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    let size = num(n, "size", 22.0) as f32;
    let t = Tokens::get(ui.ctx());
    let (rect, r) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let checked = cx.get(n, "checked").and_then(|v| v.as_bool()).unwrap_or(false);
    if checked || r.hovered() {
        ui.painter().rect_filled(rect, t.radius_sm, if checked { t.row_selected } else { t.hover });
    }
    paint_icon(ui, rect.shrink(3.0), text(n, "icon"), cx);
    r
}

fn tool_button(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    let has_icon = prop(n, "icon").is_some();
    let r = if has_icon && text(n, "text").is_empty() {
        icon_button(ui, n, cx)
    } else {
        let checked = cx.get(n, "checked").and_then(|v| v.as_bool()).unwrap_or(false);
        ui.add(egui::Button::new(text(n, "text")).frame(false).selected(checked))
    };
    if let Some(entries) = menu_of(n, cx) {
        egui::Popup::menu(&r).show(|ui| menu_entries(ui, n, &entries, cx));
        return r;
    }
    if r.clicked() {
        if flag(n, "checkable", false) {
            let checked = cx.get(n, "checked").and_then(|v| v.as_bool()).unwrap_or(false);
            cx.change(ui, n, "checked", json!(!checked));
        } else {
            cx.emit(ui, n, InputKind::Click, json!(1));
        }
    }
    r
}

fn split_button(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 1.0;
        let main = crate::widgets::primary_button(ui, text(n, "text"), 0.0);
        if main.clicked() {
            cx.emit(ui, n, InputKind::Click, json!(1));
        }
        let arrow = crate::widgets::primary_button(ui, "▾", 0.0);
        if let Some(entries) = menu_of(n, cx) {
            egui::Popup::menu(&arrow).show(|ui| menu_entries(ui, n, &entries, cx));
        }
        main
    })
    .inner
}

// ----------------------------------------------------------------------------- choices

fn radio_group(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    let value = cx.get(n, "value").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let horizontal = text(n, "orientation") == "horizontal";
    let mut draw = |ui: &mut Ui| {
        for item in items(n) {
            let id = item_str(item, "id");
            if ui.radio(value == id, item_str(item, "text")).clicked() && value != id {
                cx.change(ui, n, "value", json!(id));
            }
        }
    };
    if horizontal { ui.horizontal(|ui| draw(ui)).response } else { ui.vertical(|ui| draw(ui)).response }
}

fn combo(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    let value = cx.get(n, "value").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let shown = items(n).iter().find(|i| item_str(i, "id") == value).map(|i| item_str(i, "text").to_string()).unwrap_or_default();
    let width = num(n, "width", f64::from(ui.available_width().min(220.0))) as f32;
    let mut chosen = None;
    let r = craft_ui::ComboBox::from_id_salt(("plugin-combo", &n.id)).selected_text(shown).width(width).show_ui(ui, |ui| {
        for item in items(n) {
            match item_str(item, "kind") {
                "separator" => {
                    ui.separator();
                }
                "header" => {
                    ui.label(RichText::new(item_str(item, "text")).small().weak());
                }
                _ => {
                    let id = item_str(item, "id");
                    let enabled = item.get("enabled").and_then(Value::as_bool).unwrap_or(true);
                    if ui.add_enabled(enabled, egui::Button::selectable(value == id, item_str(item, "text"))).clicked() {
                        chosen = Some(id.to_string());
                    }
                }
            }
        }
    });
    if let Some(id) = chosen.filter(|id| *id != value) {
        cx.change(ui, n, "value", json!(id));
    }
    r.response
}

// ----------------------------------------------------------------------------- numbers

fn number(n: &Node, cx: &Cx<'_>) -> (f64, f64, f64, f64, bool) {
    let min = num(n, "min", 0.0);
    let max = num(n, "max", 100.0).max(min);
    let float = flag(n, "float", false) || num(n, "decimals", 0.0) > 0.0;
    let step = num(n, "step", if float { 0.01 } else { 1.0 }).abs().max(1e-9);
    let value = cx.get(n, "value").and_then(|v| v.as_f64()).unwrap_or(min).clamp(min, max);
    (min, max, step, value, float)
}

fn send_number(ui: &Ui, n: &Node, cx: &mut Cx<'_>, value: f64, float: bool) {
    let v = if float { json!(value) } else { json!(value.round() as i64) };
    cx.change(ui, n, "value", v);
}

fn slider(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>, with_spin: bool) -> egui::Response {
    let (min, max, step, value, float) = number(n, cx);
    ui.horizontal(|ui| {
        let mut v = value as f32;
        let width = if with_spin { (ui.available_width() - 64.0).max(60.0) } else { ui.available_width().max(60.0) };
        let r = ui.scope(|ui| {
            ui.set_width(width);
            crate::widgets::slider(ui, &mut v, min as f32..=max as f32, None)
        });
        let mut new = f64::from(v);
        if !float {
            new = (new / step).round() * step;
        }
        if r.inner.changed() && (new - value).abs() > f64::EPSILON {
            send_number(ui, n, cx, new, float);
        }
        if with_spin {
            let mut s = cx.get(n, "value").and_then(|v| v.as_f64()).unwrap_or(new);
            let decimals = num(n, "decimals", if float { 2.0 } else { 0.0 }) as usize;
            let dv = egui::DragValue::new(&mut s).range(min..=max).speed(step).max_decimals(decimals).suffix(text(n, "suffix").to_string());
            if ui.add(dv).changed() {
                send_number(ui, n, cx, s, float);
            }
        }
        r.inner
    })
    .inner
}

fn spin(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    let (min, max, step, mut value, float) = number(n, cx);
    let decimals = num(n, "decimals", if float { 2.0 } else { 0.0 }) as usize;
    let r = ui.add(
        egui::DragValue::new(&mut value)
            .range(min..=max)
            .speed(step)
            .max_decimals(decimals)
            .prefix(text(n, "prefix").to_string())
            .suffix(text(n, "suffix").to_string()),
    );
    if r.changed() {
        send_number(ui, n, cx, value, float);
    }
    r
}

// ----------------------------------------------------------------------------- text

fn line_edit(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    let mut value = cx.get(n, "text").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let edit =
        egui::TextEdit::singleline(&mut value).hint_text(text(n, "placeholder")).password(flag(n, "password", false)).desired_width(ui.available_width());
    let r = ui.add(edit);
    if r.changed() {
        cx.change(ui, n, "text", json!(value));
    }
    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        cx.emit(ui, n, InputKind::Key, json!("Enter"));
    }
    r
}

/// Highlight spans: `[{start, end, color: "#rrggbb", bold?, italic?}]`, in characters.
fn highlighted(ui: &Ui, text: &str, spans: Option<&Value>, wrap_width: f32) -> egui::text::LayoutJob {
    let style = ui.style();
    let font = egui::TextStyle::Monospace.resolve(style);
    let base = egui::TextFormat { font_id: font.clone(), color: style.visuals.text_color(), ..Default::default() };
    let mut job = egui::text::LayoutJob { wrap: egui::text::TextWrapping { max_width: wrap_width, ..Default::default() }, ..Default::default() };
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let byte_at = |c: usize| chars.get(c).map_or(text.len(), |(b, _)| *b);
    let mut spans: Vec<(usize, usize, egui::TextFormat)> = spans
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|s| {
                    let (start, end) = (s.get("start")?.as_u64()? as usize, s.get("end")?.as_u64()? as usize);
                    let mut format = base.clone();
                    if let Some(color) = s.get("color").and_then(Value::as_str).and_then(|c| egui::Color32::from_hex(c).ok()) {
                        format.color = color;
                    }
                    if s.get("italic").and_then(Value::as_bool).unwrap_or(false) {
                        format.italics = true;
                    }
                    (start < end).then_some((byte_at(start), byte_at(end), format))
                })
                .collect()
        })
        .unwrap_or_default();
    spans.sort_by_key(|s| s.0);
    let mut at = 0;
    for (start, end, format) in spans {
        if start < at {
            continue;
        }
        if start > at {
            job.append(&text[at..start], 0.0, base.clone());
        }
        job.append(&text[start..end.min(text.len())], 0.0, format);
        at = end.min(text.len());
    }
    if at < text.len() {
        job.append(&text[at..], 0.0, base);
    }
    job
}

fn text_edit(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    let mut value = cx.get(n, "text").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let min_lines = num(n, "minLines", 3.0).max(1.0) as usize;
    let max_lines = num(n, "maxLines", 12.0).max(min_lines as f64) as usize;
    let lines = value.lines().count().clamp(min_lines, max_lines);
    let rows = if flag(n, "resizable", false) { cx.get(n, "lines").and_then(|v| v.as_u64()).map_or(lines, |l| l as usize) } else { lines };
    let spans = prop(n, "spans").cloned();
    let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
        let job = highlighted(ui, buf.as_str(), spans.as_ref(), wrap_width);
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let output = egui::TextEdit::multiline(&mut value)
        .id_salt(("plugin-text", &n.id))
        .hint_text(text(n, "placeholder"))
        .interactive(!flag(n, "readOnly", false))
        .desired_rows(rows)
        .desired_width(ui.available_width())
        .layouter(&mut layouter)
        .show(ui);
    let r = output.response.response.clone();
    if r.changed() {
        cx.change(ui, n, "text", json!(value));
    }
    if let Some(range) = output.cursor_range
        && r.has_focus()
    {
        let (anchor, cursor) = (range.secondary.index.0, range.primary.index.0);
        let previous = cx.local.get(&(n.id.clone(), "cursor")).cloned();
        let now = json!({"anchor": anchor, "cursor": cursor});
        if previous.as_ref() != Some(&now) {
            cx.set(n, "cursor", now.clone());
            cx.emit(ui, n, InputKind::Cursor, now);
        }
        // Completions pop up at the cursor; choosing one sends a `menu` event with its index.
        if let Some(list) = prop(n, "completions").and_then(Value::as_array).filter(|l| !l.is_empty()) {
            let at = output.galley_pos + output.galley.pos_from_cursor(range.primary).left_bottom().to_vec2();
            let mut chosen = None;
            egui::Area::new(ui.scope_id().with(("plugin-completions", &n.id))).order(egui::Order::Foreground).fixed_pos(at).show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    for (i, entry) in list.iter().take(50).enumerate() {
                        let label =
                            entry.as_str().map(str::to_string).or_else(|| entry.get("text").and_then(Value::as_str).map(str::to_string)).unwrap_or_default();
                        if ui.add(egui::Button::selectable(false, label)).clicked() {
                            chosen = Some(i);
                        }
                    }
                });
            });
            if let Some(i) = chosen {
                cx.emit(ui, n, InputKind::Menu, json!(i));
            }
        }
    }
    if flag(n, "resizable", false) {
        // A grip under the field: dragging it changes how many lines it shows.
        let (grip, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::drag());
        ui.painter().hline(grip.center().x - 12.0..=grip.center().x + 12.0, grip.center().y, egui::Stroke::new(2.0, Tokens::get(ui.ctx()).text_faint));
        if resp.dragged() {
            let line_h = ui.text_style_height(&egui::TextStyle::Monospace);
            let delta = (resp.drag_delta().y / line_h).round() as i64;
            if delta != 0 {
                let new = (rows as i64 + delta).clamp(min_lines as i64, max_lines as i64);
                cx.set(n, "lines", json!(new));
            }
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
        }
    }
    r
}

fn path_field(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    ui.horizontal(|ui| {
        let mut value = cx.get(n, "text").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
        let r = ui.add(egui::TextEdit::singleline(&mut value).desired_width((ui.available_width() - 34.0).max(40.0)));
        if r.changed() {
            cx.change(ui, n, "text", json!(value));
        }
        // The plug-in opens the picker it wants (`dialog.file`) when told Browse was clicked.
        if crate::widgets::secondary_button(ui, "…", 28.0).on_hover_text(tl!("Browse…")).clicked() {
            cx.emit(ui, n, InputKind::Click, json!("browse"));
        }
        r
    })
    .inner
}

// ----------------------------------------------------------------------------- collections

fn selected_ids(n: &Node, cx: &Cx<'_>) -> Vec<String> {
    match cx.get(n, "selected") {
        Some(Value::Array(ids)) => ids.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        Some(Value::String(id)) => vec![id],
        _ => Vec::new(),
    }
}

/// A click on item `id`: single selection, or ⌘/Ctrl to toggle and ⇧ to extend in `multi` mode.
fn select(ui: &Ui, n: &Node, cx: &mut Cx<'_>, id: &str, order: &[String]) {
    let multi = text(n, "selectionMode") == "multi";
    let m = ui.input(|i| i.modifiers);
    let mut selected = selected_ids(n, cx);
    if multi && m.command {
        if let Some(at) = selected.iter().position(|s| s == id) {
            selected.remove(at);
        } else {
            selected.push(id.to_string());
        }
    } else if multi && m.shift && !selected.is_empty() {
        let anchor = selected.last().and_then(|a| order.iter().position(|o| o == a)).unwrap_or(0);
        let here = order.iter().position(|o| o == id).unwrap_or(0);
        let (a, b) = (anchor.min(here), anchor.max(here));
        selected = order[a..=b].to_vec();
    } else {
        selected = vec![id.to_string()];
    }
    cx.set(n, "selected", json!(selected));
    cx.emit(ui, n, InputKind::Select, json!(selected));
}

/// The item order with any reorder the user made shown at once.
fn item_order(n: &Node, cx: &Cx<'_>) -> Vec<String> {
    let given: Vec<String> = items(n).iter().map(|i| item_str(i, "id").to_string()).collect();
    match cx.get(n, "order") {
        Some(Value::Array(ids)) => {
            let order: Vec<String> = ids.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
            if order.len() == given.len() && order.iter().all(|id| given.contains(id)) { order } else { given }
        }
        _ => given,
    }
}

/// Context menu, drag reordering and selection shared by lists and thumbnail grids.
fn item_interaction(ui: &Ui, n: &Node, cx: &mut Cx<'_>, r: &egui::Response, id: &str, order: &[String]) {
    if r.clicked() {
        select(ui, n, cx, id, order);
    }
    if r.double_clicked() {
        cx.emit(ui, n, InputKind::Click, json!({"item": id, "count": 2}));
    }
    if r.secondary_clicked() {
        cx.emit(ui, n, InputKind::Context, json!(id));
    }
    if let Some(entries) = menu_of_key(n, cx, "contextMenu") {
        r.context_menu(|ui| menu_entries(ui, n, &entries, cx));
    }
    if flag(n, "reorderable", false) {
        r.dnd_set_drag_payload(PluginItemDrag { owner: n.id.clone(), item: id.to_string() });
        if let Some(dragged) = r.dnd_release_payload::<PluginItemDrag>()
            && dragged.owner == n.id
            && dragged.item != id
        {
            let mut new: Vec<String> = order.iter().filter(|o| **o != dragged.item).cloned().collect();
            let at = new.iter().position(|o| o == id).unwrap_or(new.len());
            new.insert(at, dragged.item.clone());
            cx.set(n, "order", json!(new));
            cx.emit(ui, n, InputKind::Drag, json!(new));
        }
    }
}

/// An item dragged within a plug-in list.
#[derive(Clone)]
struct PluginItemDrag {
    owner: String,
    item: String,
}

fn menu_of_key<'a>(n: &'a Node, cx: &Cx<'a>, key: &str) -> Option<Vec<Value>> {
    match prop(n, key)? {
        Value::Array(entries) => Some(entries.clone()),
        Value::String(id) => cx.menus.get(id.as_str()).map(|m| items(m).to_vec()),
        _ => None,
    }
}

fn list(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    let order = item_order(n, cx);
    let selected = selected_ids(n, cx);
    let by_id: HashMap<&str, &Value> = items(n).iter().map(|i| (item_str(i, "id"), i)).collect();
    ui.vertical(|ui| {
        for id in &order {
            let Some(item) = by_id.get(id.as_str()) else { continue };
            let r = ui.horizontal(|ui| {
                if let Some(icon) = item.get("icon").and_then(Value::as_str) {
                    let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    paint_icon(ui, rect, icon, cx);
                }
                ui.add(egui::Button::selectable(selected.contains(id), item_str(item, "text")).sense(Sense::click_and_drag()))
            });
            let mut r = r.inner;
            if let Some(tip) = item.get("tooltip").and_then(Value::as_str) {
                r = r.on_hover_text(tip);
            }
            item_interaction(ui, n, cx, &r, id, &order);
        }
    })
    .response
}

fn thumb_grid(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) -> egui::Response {
    let order = item_order(n, cx);
    let selected = selected_ids(n, cx);
    let size = num(n, "thumbSize", 96.0).clamp(16.0, 512.0) as f32;
    let t = Tokens::get(ui.ctx());
    let by_id: HashMap<&str, &Value> = items(n).iter().map(|i| (item_str(i, "id"), i)).collect();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        for id in &order {
            let Some(item) = by_id.get(id.as_str()) else { continue };
            let (rect, mut r) = ui.allocate_exact_size(vec2(size, size), Sense::click_and_drag());
            // Off-screen thumbnails keep their place without drawing (hundreds of results).
            if ui.is_rect_visible(rect) {
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, t.radius_sm, t.field);
                if let Some(texture) = cx.images.texture(ui.ctx(), cx.plugin, item_str(item, "image")) {
                    let natural = texture.size_vec2();
                    let k = (rect.width() / natural.x).min(rect.height() / natural.y);
                    let shown = Rect::from_center_size(rect.center(), natural * k);
                    painter.image(texture.id(), shown, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                }
                let overlay = item_str(item, "overlay");
                if !overlay.is_empty() {
                    let font = egui::FontId::proportional(11.0);
                    let galley = painter.layout_no_wrap(overlay.to_string(), font, egui::Color32::WHITE);
                    let bg = Rect::from_min_size(rect.left_bottom() - vec2(0.0, galley.size().y + 4.0), vec2(galley.size().x + 8.0, galley.size().y + 4.0));
                    painter.rect_filled(bg, 0.0, egui::Color32::from_black_alpha(160));
                    painter.galley(bg.min + vec2(4.0, 2.0), galley, egui::Color32::WHITE);
                }
                if selected.contains(id) {
                    painter.rect_stroke(rect.shrink(1.0), t.radius_sm, egui::Stroke::new(2.0, t.accent), egui::StrokeKind::Inside);
                } else if r.hovered() {
                    painter.rect_stroke(rect.shrink(0.5), t.radius_sm, egui::Stroke::new(1.0, t.field_border), egui::StrokeKind::Inside);
                }
            }
            if let Some(tip) = item.get("tooltip").and_then(Value::as_str) {
                r = r.on_hover_text(tip);
            }
            item_interaction(ui, n, cx, &r, id, &order);
        }
    })
    .response
}

// ----------------------------------------------------------------------------- pop-ups

/// A `popup` shown under the widget its `anchor` names while `open`; clicking outside closes it
/// (a `change` event with `false`).
fn show_popup(ui: &mut Ui, n: &Node, cx: &mut Cx<'_>) {
    let open = cx.get(n, "open").and_then(|v| v.as_bool()).unwrap_or(false);
    if !open || !flag(n, "visible", true) {
        return;
    }
    let anchor = cx.rects.get(text(n, "anchor")).copied().unwrap_or_else(|| ui.min_rect());
    let area =
        egui::Area::new(ui.scope_id().with(("plugin-popup", &n.id))).order(egui::Order::Foreground).fixed_pos(anchor.left_bottom()).show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(num(n, "width", 280.0) as f32);
                ui.with_layout(Layout::top_down(Align::Min), |ui| children(ui, n, cx));
            });
        });
    if area.response.clicked_elsewhere() {
        cx.change(ui, n, "open", json!(false));
    }
}
