//! Process plug-ins' dialogs: windows drawn from widget trees (`dialog.open`, `dialog.patch`,
//! `dialog.close`), message boxes (`dialog.message`) and file pickers (`dialog.file`).
//!
//! A message box or picker answers its request only when the user has chosen: the request's
//! reply is sent later ([`PhotocraftApp::reply_to_plugin`]), and the plug-in keeps running
//! meanwhile. The file picker is the app's own (`Services::file_dialog`), modal to the window.

use egui::{RichText, vec2};
use photocraft_process_plugin_protocol::event::{self, DialogClosed, InputEvent};
use photocraft_process_plugin_protocol::frame::Frame;
use photocraft_process_plugin_protocol::message::{ErrorBody, ErrorCode, Header, decode_value};
use photocraft_process_plugin_protocol::method::{
    DialogRef, FileDialogMode, FileDialogParams, MessageAnswer, MessageKind, MessageParams, OpenDialogParams, PatchDialogParams, PickedPath,
};
use serde_json::{Map, Value};

use crate::PhotocraftApp;
use crate::file_dialog::{FileDialogAnswer, FileDialogRequest};
use crate::process_plugin_ui::{Owner, Tree};
use crate::theme::Tokens;

/// Most dialogs one plug-in may have open, and message boxes waiting.
const MAX_DIALOGS: usize = 8;
const MAX_MESSAGES: usize = 8;

/// An open plug-in dialog. Its tree is in `PluginUi::trees` under [`tree_owner`].
#[derive(Debug, Clone, PartialEq)]
pub struct Dialog {
    pub plugin: String,
    pub id: String,
    pub title: String,
    pub modal: bool,
    /// The initial size the plug-in gave (`dialog.open` `width`/`height`), in points.
    pub size: Option<(u32, u32)>,
    /// The minimum size the plug-in gave (`minWidth`/`minHeight`), in points.
    pub min_size: Option<(u32, u32)>,
}

/// A dialog's size when the plug-in gives none.
const DEFAULT_DIALOG_SIZE: egui::Vec2 = vec2(420.0, 320.0);
/// Room kept between a dialog the plug-in sized and the edges of PhotoCraft's window.
const DIALOG_MARGIN: f32 = 48.0;

impl Dialog {
    /// The size to open at and the minimum size, in points, kept within `area` (PhotoCraft's
    /// window less a margin): the plug-in's own when it gave them, else the default and none.
    fn sizes(&self, area: egui::Vec2) -> (egui::Vec2, Option<egui::Vec2>) {
        let fit = |(w, h): (u32, u32)| vec2((w as f32).min(area.x), (h as f32).min(area.y));
        let min = self.min_size.map(fit);
        let size = self.size.map(fit).unwrap_or(DEFAULT_DIALOG_SIZE.min(area));
        (min.map_or(size, |m| size.max(m)), min)
    }
}

/// A message box waiting for the user, and the request it answers.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub plugin: String,
    pub request: u64,
    pub params: MessageParams,
}

/// Dialogs and message boxes on screen.
#[derive(Default)]
pub struct Dialogs {
    pub open: Vec<Dialog>,
    pub messages: Vec<Message>,
}

/// Where a dialog's tree is kept: apart from the plug-in's panels, which may share its id.
fn tree_owner(plugin: &str, dialog: &str) -> Owner {
    Owner { plugin: plugin.to_string(), id: format!("dialog/{dialog}") }
}

fn params<T: serde::de::DeserializeOwned>(method: &str, p: &Value) -> Result<T, ErrorBody> {
    decode_value(p, ErrorCode::Invalid).map_err(|e| ErrorBody::new(ErrorCode::Invalid, format!("{method}: {}", e.message)))
}

fn empty() -> Value {
    Value::Object(Map::new())
}

impl PhotocraftApp {
    /// `dialog.open`, `dialog.patch` and `dialog.close`; `None` for other methods.
    pub(crate) fn serve_plugin_dialog(&mut self, plugin: &str, method: &str, p: &Value) -> Option<Result<Value, ErrorBody>> {
        Some(match method {
            "dialog.open" => self.open_plugin_dialog(plugin, p),
            "dialog.patch" => self.patch_plugin_dialog(plugin, p),
            "dialog.close" => params::<DialogRef>("dialog.close", p).map(|p| {
                self.close_plugin_dialog(plugin, &p.dialog_id, false);
                empty()
            }),
            _ => return None,
        })
    }

    /// Requests answered once the user has chosen (`dialog.message`, `dialog.file`): `true`
    /// when `method` is one, whose reply is then sent later or refused now.
    pub(crate) fn serve_plugin_deferred(&mut self, plugin: &str, id: u64, method: &str, p: &Value) -> bool {
        let refused = match method {
            "dialog.message" => self.ask_plugin_message(plugin, id, p),
            "dialog.file" => self.ask_plugin_file(plugin, id, p),
            _ => return false,
        };
        if let Err(error) = refused {
            self.reply_to_plugin(plugin, Frame::new(Header::Error { id, error }));
        }
        true
    }

    fn open_plugin_dialog(&mut self, plugin: &str, p: &Value) -> Result<Value, ErrorBody> {
        let p: OpenDialogParams = params("dialog.open", p)?;
        if p.dialog_id.is_empty() || p.dialog_id.len() > 64 {
            return Err(ErrorBody::new(ErrorCode::Invalid, "a dialog id is 1 to 64 bytes"));
        }
        let open = &self.process_plugin_ui.dialogs.open;
        let reopened = open.iter().any(|d| d.plugin == plugin && d.id == p.dialog_id);
        if !reopened && open.iter().filter(|d| d.plugin == plugin).count() >= MAX_DIALOGS {
            return Err(ErrorBody::new(ErrorCode::TooLarge, format!("at most {MAX_DIALOGS} dialogs per plug-in")));
        }
        let (size, min_size) = (p.size(), p.min_size());
        let tree = Tree::new(p.tree).map_err(|e| ErrorBody::new(ErrorCode::Invalid, e))?;
        self.process_plugin_ui.trees.insert(tree_owner(plugin, &p.dialog_id), tree);
        let dialog = Dialog { plugin: plugin.to_string(), id: p.dialog_id, title: p.title.chars().take(120).collect(), modal: p.modal, size, min_size };
        let open = &mut self.process_plugin_ui.dialogs.open;
        match open.iter_mut().find(|d| d.plugin == plugin && d.id == dialog.id) {
            Some(existing) => *existing = dialog,
            None => open.push(dialog),
        }
        Ok(empty())
    }

    fn patch_plugin_dialog(&mut self, plugin: &str, p: &Value) -> Result<Value, ErrorBody> {
        let p: PatchDialogParams = params("dialog.patch", p)?;
        let tree = self
            .process_plugin_ui
            .trees
            .get_mut(&tree_owner(plugin, &p.dialog_id))
            .ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("no dialog '{}'", p.dialog_id)))?;
        let mut next = tree.clone();
        for op in p.ops {
            next.apply(op).map_err(|e| ErrorBody::new(ErrorCode::Invalid, e))?;
        }
        *tree = next;
        Ok(empty())
    }

    /// Close `plugin`'s dialog `id`; `by_user` tells the plug-in (`dialog.closed`).
    fn close_plugin_dialog(&mut self, plugin: &str, id: &str, by_user: bool) {
        self.process_plugin_ui.dialogs.open.retain(|d| !(d.plugin == plugin && d.id == id));
        self.process_plugin_ui.trees.remove(&tree_owner(plugin, id));
        if by_user && let Ok(header) = Header::event(event::DIALOG_CLOSED, &DialogClosed { dialog_id: id.to_string() }) {
            self.reply_to_plugin(plugin, Frame::new(header));
        }
    }

    fn ask_plugin_message(&mut self, plugin: &str, id: u64, p: &Value) -> Result<(), ErrorBody> {
        let mut p: MessageParams = params("dialog.message", p)?;
        if self.process_plugin_ui.dialogs.messages.iter().filter(|m| m.plugin == plugin).count() >= MAX_MESSAGES {
            return Err(ErrorBody::new(ErrorCode::Busy, "too many message boxes are waiting"));
        }
        p.buttons.truncate(6);
        if p.buttons.is_empty() {
            p.buttons.push(tl!("OK").to_string());
        }
        self.process_plugin_ui.dialogs.messages.push(Message { plugin: plugin.to_string(), request: id, params: p });
        Ok(())
    }

    fn ask_plugin_file(&mut self, plugin: &str, id: u64, p: &Value) -> Result<(), ErrorBody> {
        let p: FileDialogParams = params("dialog.file", p)?;
        let extensions: Vec<String> = p.filters.iter().flat_map(|f| f.extensions.iter().map(|e| e.trim_start_matches('.').to_ascii_lowercase())).collect();
        let request = match p.mode {
            FileDialogMode::Open => {
                FileDialogRequest::Open { multiple: false, initial_dir: p.start.clone(), extensions: (!extensions.is_empty()).then_some(extensions) }
            }
            FileDialogMode::Folder => FileDialogRequest::Folder { initial_dir: p.start.clone() },
            FileDialogMode::Save => FileDialogRequest::Save { suggested: p.start.clone().unwrap_or_default() },
        };
        let plugin_id = plugin.to_string();
        self.ask_file_or_cancel(request, move |app, answer| {
            let path = match answer {
                Some(FileDialogAnswer::Paths(paths)) => paths.into_iter().next(),
                Some(FileDialogAnswer::SaveTo(path) | FileDialogAnswer::FolderTo(path)) => Some(path),
                _ => None,
            };
            let reply = Header::response(id, &PickedPath { path }).unwrap_or_else(|e| Header::Error { id, error: e });
            app.reply_to_plugin(&plugin_id, Frame::new(reply));
        })
        .map(drop)
        .map_err(|e| ErrorBody::new(ErrorCode::Busy, e))
    }

    /// Send a frame to `plugin` outside the request it answers (a later reply, an event).
    pub(crate) fn reply_to_plugin(&mut self, plugin: &str, frame: Frame) {
        if let Some(host) = self.services.process_plugins.as_mut()
            && let Err(e) = host.send(plugin, frame)
        {
            log::debug!("plug-in '{plugin}': {e}");
        }
    }

    #[cfg(test)]
    pub(crate) fn close_plugin_dialog_for_tests(&mut self, plugin: &str, id: &str) {
        self.close_plugin_dialog(plugin, id, true);
    }

    /// Forget a stopped plug-in's dialogs and message boxes.
    pub(crate) fn drop_plugin_dialogs(&mut self, plugin: &str) {
        let dialogs = &mut self.process_plugin_ui.dialogs;
        dialogs.open.retain(|d| d.plugin != plugin);
        dialogs.messages.retain(|m| m.plugin != plugin);
    }
}

/// Draw the plug-ins' dialogs and the first waiting message box, and send what the user did.
pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    for dialog in app.process_plugin_ui.dialogs.open.clone() {
        let owner = tree_owner(&dialog.plugin, &dialog.id);
        let mut open = true;
        let mut events: Vec<InputEvent> = Vec::new();
        let ui_state = &mut app.process_plugin_ui;
        let Some(tree) = ui_state.trees.get_mut(&owner) else { continue };
        let images = &mut ui_state.images;
        let area = (ctx.content_rect().size() - vec2(DIALOG_MARGIN, DIALOG_MARGIN)).max(vec2(320.0, 200.0));
        let (size, min_size) = dialog.sizes(area);
        let mut draw = |ui: &mut egui::Ui| {
            ui.set_min_width(320.0);
            events = crate::process_plugin_ui::render::show(ui, &dialog.plugin, &dialog.id, tree, images);
        };
        if dialog.modal {
            let modal = egui::Modal::new(egui::Id::unique(("plugin-dialog", &dialog.plugin, &dialog.id))).show(ctx, |ui| {
                ui.heading(&dialog.title);
                ui.add_space(6.0);
                if dialog.size.is_some() {
                    ui.set_min_size(size);
                    ui.set_max_size(size);
                }
                draw(ui);
            });
            if modal.should_close() {
                open = false;
            }
        } else {
            egui::Window::new(&dialog.title)
                .id(egui::Id::unique(("plugin-dialog", &dialog.plugin, &dialog.id)))
                .open(&mut open)
                .resizable(true)
                .collapsible(false)
                .default_size(size)
                .min_size(min_size.unwrap_or(vec2(0.0, 0.0)))
                .max_size(area)
                .show(ctx, |ui| draw(ui));
        }
        for event in events {
            if let Ok(header) = Header::event(event::DIALOG_INPUT, &event) {
                app.reply_to_plugin(&dialog.plugin, Frame::new(header));
            }
        }
        if !open {
            app.close_plugin_dialog(&dialog.plugin, &dialog.id, true);
        }
    }
    message_box(app, ctx);
}

/// The first waiting message box: its icon, text and buttons, the first the default.
fn message_box(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(message) = app.process_plugin_ui.dialogs.messages.first().cloned() else { return };
    let t = Tokens::get(ctx);
    let p = &message.params;
    let mut chosen: Option<Option<String>> = None;
    let modal = egui::Modal::new(egui::Id::unique(("plugin-message", &message.plugin, message.request))).show(ctx, |ui| {
        ui.set_max_width(420.0);
        let (icon, color) = match p.kind {
            MessageKind::Information => ("info", t.accent),
            MessageKind::Question => ("info", t.accent),
            MessageKind::Warning => ("triangle-alert", t.warning),
            MessageKind::Critical => ("triangle-alert", t.warning),
        };
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(24.0, 24.0), egui::Sense::hover());
            crate::icons::paint(ui, rect, icon, 20.0, color);
            ui.label(RichText::new(&p.title).strong());
        });
        ui.add_space(6.0);
        ui.label(&p.text);
        ui.add_space(10.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (i, label) in p.buttons.iter().enumerate().rev() {
                let r = if i == 0 { crate::widgets::primary_button(ui, label, 72.0) } else { crate::widgets::secondary_button(ui, label, 72.0) };
                if r.clicked() {
                    chosen = Some(Some(label.clone()));
                }
            }
        });
        if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            chosen = Some(p.buttons.first().cloned());
        }
    });
    if chosen.is_none() && modal.should_close() {
        chosen = Some(None);
    }
    if let Some(button) = chosen {
        app.process_plugin_ui.dialogs.messages.remove(0);
        let reply = Header::response(message.request, &MessageAnswer { button }).unwrap_or_else(|e| Header::Error { id: message.request, error: e });
        app.reply_to_plugin(&message.plugin, Frame::new(reply));
    }
}
