//! Render the process plug-in widget gallery (one widget of every kind, see
//! `photocraft_process_plugin_protocol::widget::gallery`) offscreen and save a PNG of each page,
//! side by side, in PhotoCraft's theme: the snapshot reviewers check against when the renderer
//! changes.
//!
//! ```sh
//! cargo run --release -p photocraft-ui-egui --example plugin_gallery -- --out gallery.png --theme 0
//! ```
//!
//! `--theme N` picks the theme by its index in `ThemeKind::ALL`.

use photocraft_process_plugin_protocol::widget::{WidgetKind, gallery};
use photocraft_ui_egui::PhotocraftApp;
use photocraft_ui_egui::process_plugin_ui::{ImageCache, Tree, render};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

/// An 8x8 two-colour thumbnail, for the image and thumbnail widgets.
fn thumbnail() -> Vec<u8> {
    let px: Vec<u8> = (0..64).flat_map(|i| if (i / 8 + i % 8) % 2 == 0 { [208, 135, 112, 255] } else { [94, 129, 172, 255] }).collect();
    let img = photocraft_codecs::Image::from_u8(8, 8, photocraft_codecs::ChannelLayout::Rgba, px).expect("thumbnail");
    photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &photocraft_codecs::EncodeOptions::default()).expect("png")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = arg(&args, "--out").unwrap_or_else(|| "plugin_gallery.png".into());
    let theme_index: usize = arg(&args, "--theme").and_then(|s| s.parse().ok()).unwrap_or(0);
    let theme = photocraft_ui_egui::theme::ThemeKind::ALL.get(theme_index).copied().unwrap_or_default();
    // One tree per tab page, each showing that page.
    let pages: Vec<Tree> = (0..4)
        .filter_map(|i| {
            let mut root = gallery();
            let tabs = root.children.iter_mut().find(|c| c.kind == WidgetKind::Tabs)?;
            tabs.props.insert("current".into(), serde_json::json!(i));
            Tree::new(root).ok()
        })
        .collect();
    let mut images = ImageCache::default();
    images.put("gallery", "thumb", "png", &thumbnail()).expect("thumbnail");
    let state = (pages, images);
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(4.0 * 340.0, 760.0))
        .with_pixels_per_point(1.0)
        .with_render_options(eframe::egui_wgpu::RendererOptions::PREDICTABLE)
        .wgpu()
        .build_ui_state(
            |ui, (pages, images): &mut (Vec<Tree>, ImageCache)| {
                if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                    return;
                }
                let fill = photocraft_ui_egui::theme::Tokens::get(ui.ctx()).dock;
                ui.painter().rect_filled(ui.max_rect(), 0.0, fill);
                ui.horizontal_top(|ui| {
                    for (i, tree) in pages.iter_mut().enumerate() {
                        ui.allocate_ui(egui::vec2(330.0, 750.0), |ui| {
                            ui.set_width(330.0);
                            ui.push_id(i, |ui| render::show(ui, "gallery", &format!("page-{i}"), tree, images));
                        });
                    }
                });
            },
            state,
        );
    PhotocraftApp::setup_context(&harness.ctx, theme);
    harness.run_steps(6);
    let img = harness.render().expect("render");
    img.save(&out).expect("save png");
    println!("wrote {out} ({}×{})", img.width(), img.height());
}
