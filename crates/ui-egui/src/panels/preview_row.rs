//! The Layers panel row of a process plug-in's transient preview (`preview.show`).
//!
//! The preview isn't part of the document (see `photocraft_engine::process_plugin`), so it has
//! no layer of its own: this row only shows where it sits in the stack. It is drawn locked and
//! dimmed, can't be selected, renamed, dragged or painted on, and a click says why.

use super::{PhotocraftApp, Rect, Sense, Tokens, icons, vec2};

/// Draw the preview row labelled `label` at `depth` (the depth of the layer it sits above).
pub(super) fn preview_row(app: &mut PhotocraftApp, ui: &mut egui::Ui, label: &str, depth: usize) -> Rect {
    let t = Tokens::get(ui.ctx());
    let (_, row_h) = crate::layers_panel_ui::thumbnail_geometry(app.ui.layers_panel_options.thumbnail_size, t.pro);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, false, label));
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, t.hover.gamma_multiply(0.35));
    // Where the eye column of a layer row is, the lock: the preview can't be hidden or edited here.
    let lock = Rect::from_center_size(egui::pos2(rect.left() + 6.0 + 10.0, rect.center().y), vec2(14.0, 14.0));
    icons::paint(ui, lock, "lock", 12.0, t.text_faint);
    let x = rect.left() + 6.0 + 28.0 + 14.0 * depth as f32;
    let font = egui::TextStyle::Body.resolve(ui.style());
    let text = egui::RichText::new(label).italics().color(t.text_faint);
    let galley = egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Truncate), (rect.right() - x - 6.0).max(0.0), font);
    painter.galley(egui::pos2(x, rect.center().y - galley.size().y / 2.0), galley, t.text_faint);
    let resp = resp.on_hover_text(tl!("A plug-in's preview. It isn't part of the document and can't be edited."));
    if resp.clicked() {
        app.ui.status = tl!("A plug-in's preview can't be selected or edited. Apply it from the plug-in to keep it.").to_string();
        app.ui.status_error = false;
    }
    rect
}
