//! The Layers panel row of a plug-in's preview: shown above its layer, locked, and a click on it
//! selects nothing and changes nothing.

use egui::{accesskit::Role, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::json;

use crate::PhotocraftApp;

#[test]
fn the_preview_row_is_shown_and_refuses_clicks() {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 200, "height": 120})).unwrap();
    s.execute("layer.new.layer", json!({"name": "Paint"})).unwrap();
    let doc = s.documents()[0].doc.id.0.to_string();
    let image = vec![255u8; 10 * 10 * 4];
    let show = json!({"doc": doc, "label": "[Preview] a fox", "rect": {"x": 0, "y": 0, "width": 10, "height": 10}, "format": "rgba8"});
    s.plugin_request("org.example.p", "preview.show", &show, &image).unwrap();
    let mut h = Harness::builder().with_size(vec2(1440.0, 1000.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        PhotocraftApp::new(s, crate::Services::default())
    });
    h.run_steps(8);
    let steps = h.state().session.active().unwrap().history.entries().len();
    let active = h.state().session.active().unwrap().active_layer;
    // A label's text is its accessible value.
    let row = h.query_all_by_role(Role::Label).find(|n| n.accesskit_node().value().as_deref() == Some("[Preview] a fox")).map(|n| n.rect());
    let row = row.expect("the preview row is in the Layers panel");
    let paint = h.query_all_by_role(Role::Button).find(|n| n.accesskit_node().label().as_deref() == Some("Paint")).map(|n| n.rect()).unwrap();
    assert!(row.bottom() <= paint.top() + 1.0, "drawn above the top layer");
    h.hover_at(row.center());
    h.run_steps(1);
    h.drag_at(row.center());
    h.run_steps(1);
    h.drop_at(row.center());
    h.run_steps(2);
    let st = h.state().session.active().unwrap();
    assert_eq!(st.history.entries().len(), steps, "no history step");
    assert_eq!(st.active_layer, active, "nothing selected");
    assert!(h.state().ui.status.contains("preview"), "the click says why: {}", h.state().ui.status);
}
