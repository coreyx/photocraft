//! The shell's services for process plug-ins: notices, opening URLs and folders, clipboard
//! text, progress in the status bar, the image cache (`image.put`), and telling plug-ins when
//! the UI language changes.
//!
//! Document requests go to the session's broker (`Session::plugin_request`); these need the
//! window and the platform, so the shell answers them itself. Plug-in progress is only a status
//! display: it takes no job lock, so the user keeps working on every document while it shows.

use egui::{RichText, Sense, vec2};
use photocraft_process_plugin_protocol::event::{self, LocaleChanged, ProgressCancel};
use photocraft_process_plugin_protocol::frame::Frame;
use photocraft_process_plugin_protocol::message::{ErrorBody, ErrorCode, Header, decode_value};
use photocraft_process_plugin_protocol::method::{ClipboardText, NoticeLevel, NoticeParams, PathParams, ProgressParams, PutImageParams, TaskRef, UrlParams};
use serde_json::{Map, Value};

use crate::theme::Tokens;
use crate::{PhotocraftApp, i18n, notices};

/// Longest notice text shown; longer text is cut.
const MAX_NOTICE: usize = 4000;
/// Largest clipboard text a plug-in may set.
const MAX_CLIPBOARD: usize = 16 << 20;
/// Most progress indicators one plug-in may show at once.
const MAX_TASKS: usize = 16;

/// One plug-in progress indicator in the status bar.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginProgress {
    pub plugin: String,
    pub task_id: String,
    /// 0..1, or `None` while indeterminate.
    pub fraction: Option<f32>,
    pub message: String,
    /// Shows a Cancel button; cleared once Cancel was clicked, until the plug-in clears the task.
    pub cancellable: bool,
}

fn params<T: serde::de::DeserializeOwned>(method: &str, p: &Value) -> Result<T, ErrorBody> {
    decode_value(p, ErrorCode::Invalid).map_err(|e| ErrorBody::new(ErrorCode::Invalid, format!("{method}: {}", e.message)))
}

fn empty() -> Value {
    Value::Object(Map::new())
}

/// Whether a plug-in may open `url`: web pages and mail only, never a scheme that runs something.
fn openable_url(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    ["https://", "http://", "mailto:"].iter().any(|scheme| lower.starts_with(scheme)) && !url.chars().any(char::is_control)
}

impl PhotocraftApp {
    /// Answer `method` if it is one of the shell's services; `None` passes it on to the broker.
    pub(crate) fn serve_plugin_service(
        &mut self,
        ctx: &egui::Context,
        plugin: &str,
        method: &str,
        p: &Value,
        payload: &[u8],
    ) -> Option<Result<Value, ErrorBody>> {
        Some(match method {
            "image.put" => self.plugin_image_put(plugin, p, payload),
            "notice.show" => self.plugin_notice(plugin, p),
            "open.url" => self.plugin_open_url(ctx, p),
            "open.path" => self.plugin_open_path(p),
            "clipboard.get" => self.plugin_clipboard_get(),
            "clipboard.set" => self.plugin_clipboard_set(ctx, p),
            "progress.set" => self.plugin_progress_set(plugin, p),
            "progress.clear" => self.plugin_progress_clear(plugin, p),
            _ => return None,
        })
    }

    fn plugin_image_put(&mut self, plugin: &str, p: &Value, payload: &[u8]) -> Result<Value, ErrorBody> {
        let p: PutImageParams = params("image.put", p)?;
        self.process_plugin_ui.images.put(plugin, &p.hash, &p.encoding, payload).map_err(|e| ErrorBody::new(ErrorCode::Invalid, e))?;
        Ok(empty())
    }

    fn plugin_notice(&mut self, plugin: &str, p: &Value) -> Result<Value, ErrorBody> {
        let p: NoticeParams = params("notice.show", p)?;
        let text: String = p.text.chars().take(MAX_NOTICE).collect();
        if text.trim().is_empty() {
            return Err(ErrorBody::new(ErrorCode::Invalid, "a notice needs some text"));
        }
        let title = self.process_plugin_name(plugin);
        notices::post(self, title, vec![text], p.level == NoticeLevel::Error, None);
        if p.level == NoticeLevel::Warning {
            log::warn!("plug-in '{plugin}': {}", p.text);
        }
        Ok(empty())
    }

    fn plugin_open_url(&mut self, ctx: &egui::Context, p: &Value) -> Result<Value, ErrorBody> {
        let p: UrlParams = params("open.url", p)?;
        if !openable_url(&p.url) {
            return Err(ErrorBody::new(ErrorCode::Invalid, "only http, https and mailto links can be opened"));
        }
        crate::links::open(self, ctx, &p.url);
        Ok(empty())
    }

    fn plugin_open_path(&mut self, p: &Value) -> Result<Value, ErrorBody> {
        let p: PathParams = params("open.path", p)?;
        let path = std::path::Path::new(&p.path);
        if !path.is_absolute() || !path.exists() {
            return Err(ErrorBody::new(ErrorCode::NotFound, "the path must be an absolute path that exists"));
        }
        let open = self.services.open_url.as_ref().ok_or_else(|| ErrorBody::new(ErrorCode::Unsupported, "this PhotoCraft can't open folders"))?;
        open(&p.path).map_err(|e| ErrorBody::new(ErrorCode::Internal, format!("can't open the path: {e}")))?;
        Ok(empty())
    }

    fn plugin_clipboard_get(&mut self) -> Result<Value, ErrorBody> {
        let read =
            self.services.clipboard_get_text.as_mut().ok_or_else(|| ErrorBody::new(ErrorCode::Unsupported, "this PhotoCraft can't read clipboard text"))?;
        let text = read().map_err(|e| ErrorBody::new(ErrorCode::Busy, format!("can't read the clipboard: {e}")))?;
        serde_json::to_value(ClipboardText { text: text.unwrap_or_default() }).map_err(|e| ErrorBody::new(ErrorCode::Internal, e.to_string()))
    }

    fn plugin_clipboard_set(&mut self, ctx: &egui::Context, p: &Value) -> Result<Value, ErrorBody> {
        let p: ClipboardText = params("clipboard.set", p)?;
        if p.text.len() > MAX_CLIPBOARD {
            return Err(ErrorBody::new(ErrorCode::TooLarge, format!("clipboard text is limited to {MAX_CLIPBOARD} bytes")));
        }
        match self.services.clipboard_set_text.as_mut() {
            Some(write) => write(&p.text).map_err(|e| ErrorBody::new(ErrorCode::Busy, format!("can't write the clipboard: {e}")))?,
            None => ctx.copy_text(p.text),
        }
        Ok(empty())
    }

    fn plugin_progress_set(&mut self, plugin: &str, p: &Value) -> Result<Value, ErrorBody> {
        let p: ProgressParams = params("progress.set", p)?;
        if p.task_id.is_empty() || p.task_id.len() > 200 {
            return Err(ErrorBody::new(ErrorCode::Invalid, "the task id must be 1 to 200 bytes"));
        }
        let fraction = p.fraction.filter(|f| f.is_finite()).map(|f| f.clamp(0.0, 1.0));
        let message: String = p.message.chars().take(200).collect();
        let tasks = &mut self.process_plugin_progress;
        match tasks.iter_mut().find(|t| t.plugin == plugin && t.task_id == p.task_id) {
            Some(task) => {
                task.fraction = fraction;
                task.message = message;
                // A Cancel already clicked stays clicked.
                task.cancellable &= p.cancellable;
            }
            None => {
                if tasks.iter().filter(|t| t.plugin == plugin).count() >= MAX_TASKS {
                    return Err(ErrorBody::new(ErrorCode::TooLarge, format!("at most {MAX_TASKS} progress indicators per plug-in")));
                }
                tasks.push(PluginProgress { plugin: plugin.to_string(), task_id: p.task_id, fraction, message, cancellable: p.cancellable });
            }
        }
        Ok(empty())
    }

    fn plugin_progress_clear(&mut self, plugin: &str, p: &Value) -> Result<Value, ErrorBody> {
        let p: TaskRef = params("progress.clear", p)?;
        self.process_plugin_progress.retain(|t| !(t.plugin == plugin && t.task_id == p.task_id));
        Ok(empty())
    }

    /// Drop the progress indicators, widget trees and images of a plug-in that stopped.
    pub(crate) fn clear_plugin_progress(&mut self, plugin: &str) {
        self.process_plugin_progress.retain(|t| t.plugin != plugin);
        self.process_plugin_ui.drop_plugin(plugin);
    }

    /// Cancel was clicked on a plug-in's progress: ask the plug-in to stop that task.
    pub(crate) fn cancel_plugin_task(&mut self, plugin: &str, task_id: &str) {
        if let Some(task) = self.process_plugin_progress.iter_mut().find(|t| t.plugin == plugin && t.task_id == task_id) {
            task.cancellable = false;
        }
        let event = Header::event(event::PROGRESS_CANCEL, &ProgressCancel { task_id: task_id.to_string() });
        if let (Ok(header), Some(host)) = (event, self.services.process_plugins.as_mut())
            && let Err(e) = host.send(plugin, Frame::new(header))
        {
            log::debug!("plug-in '{plugin}': {e}");
        }
    }

    /// Tell the running plug-ins when the UI language changed since the last call.
    pub(crate) fn tell_plugins_the_locale(&mut self) {
        let locale = i18n::current().code();
        if self.process_plugin_locale == locale {
            return;
        }
        let first = self.process_plugin_locale.is_empty();
        self.process_plugin_locale = locale.to_string();
        // Plug-ins learn the language they start in from their welcome.
        if first {
            return;
        }
        let Some(host) = self.services.process_plugins.as_mut() else { return };
        let Ok(header) = Header::event(event::LOCALE_CHANGED, &LocaleChanged { locale: locale.to_string() }) else { return };
        for status in host.statuses().into_iter().filter(|s| s.state.is_live()) {
            if let Err(e) = host.send(&status.id, Frame::new(header.clone())) {
                log::debug!("plug-in '{}': {e}", status.id);
            }
        }
    }
}

/// Plug-in progress in the status bar, after any job's: right to left, Cancel (when offered),
/// the percentage, the bar and the message. Several tasks show the first one and a count.
pub fn status_progress(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let Some(task) = app.process_plugin_progress.first().cloned() else { return };
    let t = Tokens::get(ui.ctx());
    let more = app.process_plugin_progress.len() - 1;
    ui.add_space(6.0);
    if task.cancellable {
        let (xr, xresp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
        if xresp.hovered() {
            ui.painter().rect_filled(xr, t.radius_sm, t.hover);
        }
        crate::icons::paint(ui, xr, "x", 11.0, if xresp.hovered() { t.text } else { t.text_dim });
        if xresp.on_hover_text(tl!("Cancel")).clicked() {
            app.cancel_plugin_task(&task.plugin, &task.task_id);
        }
    }
    if let Some(f) = task.fraction {
        ui.label(RichText::new(format!("{:.0}%", f * 100.0)).color(t.text_dim).size(11.5).monospace());
    } else {
        // An indeterminate bar animates.
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
    }
    let (br, _) = ui.allocate_exact_size(vec2(if t.pro { 120.0 } else { 140.0 }, 6.0), Sense::hover());
    crate::jobs_ui::bar(ui, br, task.fraction, &t);
    let label = if more > 0 { format!("{} (+{more})", task.message) } else { task.message.clone() };
    ui.label(RichText::new(label).color(t.text_dim).size(12.0));
}

#[cfg(test)]
mod tests {
    use super::openable_url;

    #[test]
    fn only_web_and_mail_links_open() {
        for ok in ["https://example.com/docs", "http://example.org", "MAILTO:someone@example.com", "  https://example.net"] {
            assert!(openable_url(ok), "{ok}");
        }
        for refused in ["javascript:alert(1)", "file:///etc/passwd", "ms-settings:", "C:\\Windows", "https://example.com/\nx", ""] {
            assert!(!openable_url(refused), "{refused}");
        }
    }
}
