//! Tests for plug-in widget trees: patches, the image cache, and the renderer drawing every
//! widget kind of the gallery and turning clicks, typing and drags into input events.

use egui::{accesskit::Role, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use photocraft_process_plugin_protocol::event::{InputEvent, InputKind};
use photocraft_process_plugin_protocol::widget::{Node, Patch, WidgetKind, gallery};
use serde_json::{Map, json};

use super::{ImageCache, Tree};

const P: &str = "org.example.test";

/// Every widget kind; the `match` fails to compile when a kind is added and not listed.
fn all_kinds() -> Vec<WidgetKind> {
    use WidgetKind as K;
    let all = vec![
        K::Row,
        K::Column,
        K::Grid,
        K::Form,
        K::Group,
        K::Collapsible,
        K::Tabs,
        K::Stack,
        K::Scroll,
        K::Splitter,
        K::Spacer,
        K::Label,
        K::Icon,
        K::Image,
        K::Progress,
        K::Separator,
        K::Button,
        K::ToolButton,
        K::ToggleButton,
        K::SplitButton,
        K::Checkbox,
        K::Switch,
        K::RadioGroup,
        K::Combo,
        K::Slider,
        K::Spin,
        K::SliderSpin,
        K::LineEdit,
        K::TextEdit,
        K::PathField,
        K::List,
        K::ThumbGrid,
        K::Repeater,
        K::Popup,
        K::Menu,
    ];
    for k in &all {
        match k {
            K::Row | K::Column | K::Grid | K::Form | K::Group | K::Collapsible | K::Tabs | K::Stack | K::Scroll | K::Splitter | K::Spacer => {}
            K::Label | K::Icon | K::Image | K::Progress | K::Separator => {}
            K::Button | K::ToolButton | K::ToggleButton | K::SplitButton | K::Checkbox | K::Switch | K::RadioGroup | K::Combo => {}
            K::Slider | K::Spin | K::SliderSpin | K::LineEdit | K::TextEdit | K::PathField => {}
            K::List | K::ThumbGrid | K::Repeater | K::Popup | K::Menu => {}
        }
    }
    all
}

fn kinds(node: &Node, out: &mut Vec<WidgetKind>) {
    out.push(node.kind);
    for c in &node.children {
        kinds(c, out);
    }
}

/// A small PNG, for the image cache.
fn png(w: u32, h: u32) -> Vec<u8> {
    let img = photocraft_codecs::Image::from_u8(w, h, photocraft_codecs::ChannelLayout::Rgba, vec![200; (w * h * 4) as usize]).unwrap();
    photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &photocraft_codecs::EncodeOptions::default()).unwrap()
}

struct State {
    tree: Tree,
    images: ImageCache,
    events: Vec<InputEvent>,
}

/// A harness drawing `root` in PhotoCraft's theme, collecting its events.
fn harness(root: Node) -> Harness<'static, State> {
    let mut images = ImageCache::default();
    images.put(P, "thumb", "png", &png(8, 8)).unwrap();
    let state = State { tree: Tree::new(root).unwrap(), images, events: Vec::new() };
    let mut h = Harness::builder().with_size(vec2(700.0, 900.0)).build_ui_state(
        |ui, s: &mut State| {
            // PhotoCraft's fonts arrive with the theme, a frame after it is set.
            if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            let events = super::render::show(ui, P, "panel", &mut s.tree, &mut s.images, false);
            s.events.extend(events);
        },
        state,
    );
    crate::PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
    h.run_steps(4);
    h
}

fn page(index: usize) -> Node {
    let mut root = gallery();
    if let Some(tabs) = root.children.iter_mut().find(|c| c.kind == WidgetKind::Tabs) {
        tabs.props.insert("current".into(), json!(index));
    }
    root
}

fn events_of<'a>(h: &'a Harness<'_, State>, id: &str) -> Vec<&'a InputEvent> {
    h.state().events.iter().filter(|e| e.node_id == id).collect()
}

#[test]
fn the_gallery_holds_every_widget_kind() {
    let mut found = Vec::new();
    kinds(&gallery(), &mut found);
    for kind in all_kinds() {
        assert!(found.contains(&kind), "{kind:?} is missing from the gallery");
    }
    assert!(gallery().validate().is_ok());
}

#[test]
fn every_page_of_the_gallery_draws() {
    for index in 0..4 {
        let mut h = harness(page(index));
        h.run_steps(3);
        assert!(h.state().events.is_empty(), "drawing alone sends nothing (page {index})");
    }
}

#[test]
fn buttons_send_clicks_and_toggles_show_their_new_state() {
    let mut h = harness(page(0));
    h.get_by_label("Generate").click();
    h.run_steps(2);
    assert_eq!(events_of(&h, "button")[0].kind, InputKind::Click);
    h.get_by_label("Seed fixed").click();
    h.run_steps(2);
    let check = events_of(&h, "check");
    assert_eq!((check[0].kind, check[0].value.clone()), (InputKind::Change, json!(true)));
    // Shown checked at once, before the plug-in answers.
    assert_eq!(h.state().tree.local.get(&("check".to_string(), "checked")), Some(&json!(true)));
    // The plug-in's own value wins once it sets it.
    let mut props = Map::new();
    props.insert("checked".into(), json!(false));
    h.state_mut().tree.apply(Patch::Set { id: "check".into(), props }).unwrap();
    assert!(!h.state().tree.local.contains_key(&("check".to_string(), "checked")));
}

#[test]
fn typing_sends_text_and_menus_send_their_entry() {
    let mut h = harness(page(0));
    h.query_all_by_role(Role::TextInput).next().unwrap().click();
    h.run_steps(1);
    h.query_all_by_role(Role::TextInput).next().unwrap().type_text("blurry");
    h.run_steps(2);
    assert!(h.state().events.iter().any(|e| e.kind == InputKind::Change && e.value.as_str().is_some_and(|s| s.contains("blurry"))));
    h.get_by_label("Tools").click();
    h.run_steps(2);
    h.get_by_label("Copy").click();
    h.run_steps(2);
    let tool = events_of(&h, "tool");
    assert!(tool.iter().any(|e| e.kind == InputKind::Menu && e.value == json!("copy")), "{tool:?}");
}

#[test]
fn tabs_lists_and_combos_report_choices() {
    let mut h = harness(page(0));
    h.get_by_label("Collections").click();
    h.run_steps(2);
    assert_eq!(events_of(&h, "tabs")[0].value, json!(3));
    h.get_by_label("Region B").click();
    h.run_steps(2);
    let select = events_of(&h, "list");
    assert_eq!((select[0].kind, select[0].value.clone()), (InputKind::Select, json!(["b"])));
    let mut h = harness(page(0));
    // The combo shows its choice as its value.
    h.query_all_by_role(Role::ComboBox).next().unwrap().click();
    h.run_steps(2);
    h.get_by_label("Flux").click();
    h.run_steps(2);
    assert_eq!(events_of(&h, "model")[0].value, json!("flux"));
}

#[test]
fn rich_labels_send_their_links() {
    let mut h = harness(page(1));
    h.get_by_label("a link").click();
    h.run_steps(2);
    assert_eq!(events_of(&h, "rich")[0].value, json!("https://example.com"));
}

#[test]
fn hidden_and_disabled_widgets_send_nothing() {
    let root = Node::new("root", WidgetKind::Column)
        .child(Node::new("off", WidgetKind::Button).prop("text", "Disabled").prop("enabled", false))
        .child(Node::new("gone", WidgetKind::Button).prop("text", "Hidden").prop("visible", false));
    let mut h = harness(root);
    assert!(h.query_by_label("Hidden").is_none());
    h.get_by_label("Disabled").click();
    h.run_steps(2);
    assert!(h.state().events.is_empty());
}

/// The strength row of Krita AI Diffusion's Generate panel: a slider with a spin box, then two
/// tool buttons. The slider takes what the buttons leave (spec `generate-panel-rows-fit`).
#[test]
fn a_slider_leaves_room_for_the_buttons_after_it() {
    let row = Node::new("row", WidgetKind::Row)
        .child(Node::new("strength", WidgetKind::SliderSpin).prop("min", 0).prop("max", 100).prop("value", 100))
        .child(Node::new("control", WidgetKind::ToolButton).prop("text", "Control"))
        .child(Node::new("region", WidgetKind::ToolButton).prop("text", "Region"));
    let state = State { tree: Tree::new(Node::new("root", WidgetKind::Column).child(row)).unwrap(), images: ImageCache::default(), events: Vec::new() };
    let mut h = Harness::builder().with_size(vec2(300.0, 200.0)).build_ui_state(
        |ui, s: &mut State| {
            if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            super::render::show(ui, P, "panel", &mut s.tree, &mut s.images, false);
        },
        state,
    );
    crate::PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
    h.run_steps(6);
    let edge = h.ctx.content_rect().right();
    for label in ["Control", "Region"] {
        let rect = h.get_by_label(label).rect();
        assert!(rect.right() <= edge + 0.5, "{label} ends at {} past {edge}", rect.right());
    }
}

/// The upscaler row of Krita AI Diffusion's Upscale panel, with the server's long default name
/// selected, and the Scale row after it. The drop-down cuts its text rather than widening the
/// panel's layout, which put every right-aligned item after it past the edge (spec
/// `upscale-panel-rows-fit`).
#[test]
fn a_long_combo_choice_stays_inside_the_panel() {
    const NAME: &str = "Default (4x_NMKD-Superscale-SP_178000_G.pth)";
    let combo = Node::new("model", WidgetKind::Combo).prop("value", "default").prop("items", json!([{ "id": "default", "text": NAME }]));
    // In a row, as upstream puts it after the workspace button: there a `Ui` extends text
    // rather than wrapping it.
    let top = Node::new("top", WidgetKind::Row).child(Node::new("workspace", WidgetKind::ToolButton).prop("text", "Upscale")).child(combo);
    let row = Node::new("row", WidgetKind::Row)
        .child(Node::new("factor", WidgetKind::Label).prop("text", "Factor").prop("grow", true))
        .child(Node::new("scale", WidgetKind::Spin).prop("min", 1).prop("max", 4).prop("value", 2).prop("prefix", "Scale: "));
    let root = Node::new("root", WidgetKind::Column).child(top).child(row);
    let state = State { tree: Tree::new(root).unwrap(), images: ImageCache::default(), events: Vec::new() };
    let mut h = Harness::builder().with_size(vec2(300.0, 200.0)).build_ui_state(
        |ui, s: &mut State| {
            if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            // As a panel's body draws it (`process_plugin_panels.rs`).
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                super::render::show(ui, P, "panel", &mut s.tree, &mut s.images, false);
            });
        },
        state,
    );
    crate::PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
    h.run_steps(6);
    let edge = h.ctx.content_rect().right();
    let combo = h.get_by_role(Role::ComboBox);
    assert!(combo.rect().right() <= edge + 0.5, "the drop-down ends at {} past {edge}", combo.rect().right());
    assert_eq!(combo.accesskit_node().value().as_deref(), Some(NAME), "a screen reader still hears the whole name");
    let spin = h.get_by_role(Role::SpinButton);
    assert!(spin.rect().right() <= edge + 0.5, "the spin box ends at {} past {edge}", spin.rect().right());
    // The cut name shows whole on hover.
    let before = h.query_all_by_label(NAME).count();
    h.get_by_role(Role::ComboBox).hover();
    h.run_steps(30);
    assert!(h.query_all_by_label(NAME).count() > before, "hovering shows the whole name");
}

/// A context menu the user dismissed by clicking elsewhere leaves a local `open: false`. When the
/// plug-in removes that popup and later opens a menu under the same id, the new one shows.
#[test]
fn a_removed_popup_leaves_no_local_state() {
    let popup = || {
        Node::new("go.menu", WidgetKind::Popup)
            .prop("anchor", "go")
            .prop("open", true)
            .child(Node::new("go.menu.0", WidgetKind::Button).prop("text", "Copy Prompt"))
    };
    let root = Node::new("root", WidgetKind::Column).child(Node::new("go", WidgetKind::Button).prop("text", "History")).child(popup());
    let mut h = harness(root);
    assert!(h.query_by_label("Copy Prompt").is_some(), "open at first");

    h.state_mut().tree.local.insert(("go.menu".into(), "open"), json!(false));
    h.run_steps(2);
    assert!(h.query_by_label("Copy Prompt").is_none(), "dismissed");

    h.state_mut().tree.apply(Patch::Remove { id: "go.menu".into() }).unwrap();
    h.state_mut().tree.apply(Patch::Insert { parent: "root".into(), index: 1, node: popup() }).unwrap();
    h.run_steps(2);
    assert!(!h.state().tree.local.contains_key(&("go.menu".to_string(), "open")));
    assert!(h.query_by_label("Copy Prompt").is_some(), "the reopened menu shows");
}

#[test]
fn patches_change_the_tree_and_bad_ones_change_nothing() {
    let mut tree = Tree::new(gallery()).unwrap();
    let before = tree.root.clone();
    for bad in [
        Patch::Remove { id: "root".into() },
        Patch::Remove { id: "nope".into() },
        Patch::Move { id: "tabs".into(), parent: "page-inputs".into(), index: 0 },
        Patch::Insert { parent: "buttons".into(), index: 0, node: Node::new("button", WidgetKind::Button) },
        Patch::Set { id: "nope".into(), props: Map::new() },
    ] {
        assert!(tree.apply(bad.clone()).is_err(), "{bad:?}");
        assert_eq!(tree.root, before, "{bad:?} left the tree as it was");
    }
    tree.apply(Patch::Insert { parent: "buttons".into(), index: 99, node: Node::new("cancel", WidgetKind::Button).prop("text", "Cancel") }).unwrap();
    assert_eq!(tree.find("buttons").map(|b| b.children.last().map(|c| c.id.clone())), Some(Some("cancel".to_string())));
    tree.apply(Patch::Move { id: "cancel".into(), parent: "buttons".into(), index: 0 }).unwrap();
    assert_eq!(tree.find("buttons").map(|b| b.children[0].id.clone()), Some("cancel".to_string()));
    let mut props = Map::new();
    props.insert("text".into(), json!("Stop"));
    props.insert("tooltip".into(), json!(null));
    tree.apply(Patch::Set { id: "cancel".into(), props }).unwrap();
    assert_eq!(tree.find("cancel").and_then(|n| n.props.get("text")), Some(&json!("Stop")));
    tree.apply(Patch::Remove { id: "cancel".into() }).unwrap();
    assert!(tree.find("cancel").is_none());
    tree.apply(Patch::Replace { node: Node::new("root", WidgetKind::Column) }).unwrap();
    assert!(tree.root.children.is_empty());
}

#[test]
fn the_image_cache_keeps_valid_images_within_its_budget() {
    let mut cache = ImageCache::default();
    assert!(cache.put(P, "../escape", "png", &png(2, 2)).is_err());
    assert!(cache.put(P, "ok", "gif", &png(2, 2)).is_err());
    assert!(cache.put(P, "ok", "png", &[]).is_err());
    cache.put(P, "ok", "png", &png(4, 3)).unwrap();
    cache.put("org.example.other", "ok", "png", &png(2, 2)).unwrap();
    let ctx = egui::Context::default();
    let texture = cache.texture(&ctx, P, "image:ok").unwrap();
    assert_eq!(texture.size(), [4, 3]);
    assert!(cache.texture(&ctx, P, "image:missing").is_none());
    cache.put(P, "broken", "png", b"not a png").unwrap();
    assert!(cache.texture(&ctx, P, "image:broken").is_none(), "undecodable images draw nothing");
    cache.drop_plugin(P);
    assert!(!cache.contains(P, "ok") && cache.contains("org.example.other", "ok"));
    let big = vec![0u8; super::images::MAX_IMAGE];
    for i in 0..(super::images::MAX_BYTES / super::images::MAX_IMAGE + 2) {
        cache.put(P, &format!("big{i}"), "png", &big).unwrap();
    }
    assert!(cache.bytes() <= super::images::MAX_BYTES);
    assert!(!cache.contains(P, "big0"), "the oldest went first");
}

/// A text field hands the keys the plug-in asked for to the plug-in instead of editing with them
/// (Ctrl+Up re-weights a prompt, Shift+Enter generates), and leaves every other key alone.
#[test]
fn a_text_field_forwards_the_keys_it_was_given() {
    let prompt = Node::new("prompt", WidgetKind::TextEdit)
        .prop("text", "a cat")
        .prop("keys", json!(["Ctrl+Up", "Enter", "Shift+Enter"]))
        .prop("completions", json!(["cat ears", "catgirl"]))
        .prop("completionIndex", 1);
    let mut h = harness(Node::new("root", WidgetKind::Column).child(prompt));
    h.query_all_by_role(Role::MultilineTextInput).next().unwrap().click();
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::ArrowUp);
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Enter);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    let keys: Vec<_> = events_of(&h, "prompt")
        .into_iter()
        .filter(|e| e.kind == InputKind::Key)
        .map(|e| (e.value.clone(), e.modifiers.shift, e.modifiers.ctrl || e.modifiers.meta))
        .collect();
    assert_eq!(keys, vec![(json!("Up"), false, true), (json!("Enter"), true, false), (json!("Enter"), false, false)]);
    assert!(!events_of(&h, "prompt").iter().any(|e| e.kind == InputKind::Change), "forwarded keys don't edit the text");
    // Other keys still type.
    h.query_all_by_role(Role::MultilineTextInput).next().unwrap().type_text("!");
    h.run_steps(2);
    assert!(events_of(&h, "prompt").iter().any(|e| e.kind == InputKind::Change));
}

/// A list's keys apply while the pointer is over it; a thumbnail grid's header items take a row
/// of their own and can be clicked like thumbnails.
#[test]
fn lists_forward_keys_and_grids_show_headers() {
    let items = json!([
        {"id": "h1", "kind": "header", "text": "12:00 - a cat", "tooltip": "a cat"},
        {"id": "r1", "image": "image:thumb"},
        {"id": "r2", "image": "image:thumb"}
    ]);
    let grid = Node::new("history", WidgetKind::ThumbGrid).prop("items", items).prop("keys", json!(["Delete", "Space"])).prop("thumbSize", 48);
    let mut h = harness(Node::new("root", WidgetKind::Column).child(grid));
    h.get_by_label("12:00 - a cat").click();
    h.run_steps(2);
    let history = events_of(&h, "history");
    assert_eq!((history[0].kind, history[0].value.clone()), (InputKind::Select, json!(["h1"])));
    h.get_by_label("12:00 - a cat").hover();
    h.run_steps(1);
    h.key_press(egui::Key::Delete);
    h.key_press(egui::Key::Space);
    h.run_steps(2);
    let keys: Vec<_> = events_of(&h, "history").into_iter().filter(|e| e.kind == InputKind::Key).map(|e| e.value.clone()).collect();
    assert_eq!(keys, vec![json!("Delete"), json!("Space")]);
}

/// A plain primary click at screen point `p`, as the user makes one.
fn click_at(h: &mut Harness<'static, State>, p: egui::Pos2) {
    h.event(egui::Event::PointerMoved(p));
    h.run_steps(1);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.run_steps(1);
    }
    h.run_steps(1);
}

/// In a list allowing several selected items, a click that hits no item clears the selection, as
/// in Qt; Krita AI Diffusion's history deselects its result (and hides its preview) this way. A
/// click on an item still selects it, and a single-selection list keeps its selection.
#[test]
fn a_click_on_a_grids_empty_space_clears_its_selection() {
    let items = json!([
        {"id": "h1", "kind": "header", "text": "12:00 - a cat"},
        {"id": "r1", "image": "image:thumb"},
        {"id": "r2", "image": "image:thumb"}
    ]);
    let grid =
        Node::new("history", WidgetKind::ThumbGrid).prop("items", items).prop("selected", json!(["r1"])).prop("selectionMode", "multi").prop("thumbSize", 48);
    let mut h = harness(Node::new("root", WidgetKind::Column).child(grid));
    // The header takes a row of its own; the thumbnails follow it, 48 points apart plus spacing.
    let header = h.get_by_label("12:00 - a cat").rect();
    let row_y = header.bottom() + 4.0 + 24.0;
    let empty = egui::pos2(header.left() + 3.0 * 52.0 + 100.0, row_y);
    let r2 = egui::pos2(header.left() + 52.0 + 24.0, row_y);

    click_at(&mut h, empty);
    let sent: Vec<_> = events_of(&h, "history").into_iter().map(|e| (e.kind, e.value.clone())).collect();
    assert_eq!(sent, vec![(InputKind::Select, json!([]))], "empty space deselects");

    click_at(&mut h, empty);
    assert_eq!(events_of(&h, "history").len(), 1, "nothing selected, nothing to send");

    click_at(&mut h, r2);
    let last = events_of(&h, "history").last().map(|e| (e.kind, e.value.clone()));
    assert_eq!(last, Some((InputKind::Select, json!(["r2"]))), "a thumbnail still selects");

    let list = Node::new("pages", WidgetKind::List)
        .prop("items", json!([{"id": "a", "text": "Region A"}, {"id": "b", "text": "Region B"}]))
        .prop("selected", json!(["a"]));
    let mut h = harness(Node::new("root", WidgetKind::Column).child(list));
    let a = h.get_by_label("Region A").rect();
    click_at(&mut h, egui::pos2(a.right() + 200.0, a.center().y));
    assert!(events_of(&h, "pages").is_empty(), "a single-selection list keeps its selection");
}

/// REQ-5.5: a 200-thumbnail result list scrolls at 60 fps. Run in release on the reference
/// machine: `cargo test -p photocraft-ui-egui --release -- --ignored thumbnail_list_timing --nocapture`.
#[test]
#[ignore = "timing on the reference machine, release build"]
fn thumbnail_list_timing() {
    let items: Vec<_> = (0..200).map(|i| json!({"id": format!("r{i}"), "image": format!("image:t{}", i % 20), "overlay": format!("{i}")})).collect();
    let root = Node::new("root", WidgetKind::Scroll).child(Node::new("thumbs", WidgetKind::ThumbGrid).prop("items", json!(items)).prop("thumbSize", 96));
    let mut h = harness(root);
    for i in 0..20 {
        h.state_mut().images.put(P, &format!("t{i}"), "png", &png(256, 256)).unwrap();
    }
    h.run_steps(4);
    let start = std::time::Instant::now();
    let frames = 120;
    for _ in 0..frames {
        h.event(egui::Event::MouseWheel {
            source: egui::MouseWheelSource::Unknown,
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -40.0),
            modifiers: Default::default(),
            phase: egui::TouchPhase::Move,
        });
        h.step();
    }
    let per_frame = start.elapsed() / frames;
    println!("200-thumbnail grid: {per_frame:?} per frame");
    assert!(per_frame.as_secs_f64() < 1.0 / 60.0);
}
