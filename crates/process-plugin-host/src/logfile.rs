//! A plug-in's stderr, kept as a bounded, rotated log file.
//!
//! Each launch appends to `<log dir>/<plugin id>.log`. When that file is already larger than
//! [`ROTATE_BYTES`] at launch it becomes `.1.log` (shifting older ones up, keeping [`KEEP`]), and
//! a run that writes more than [`MAX_RUN_BYTES`] stops logging with a note, so a chatty or broken
//! plug-in can't fill the disk.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::thread;

/// Size at which the log is rotated when the plug-in starts.
pub const ROTATE_BYTES: u64 = 10 * 1024 * 1024;
/// Most bytes one run may log.
pub const MAX_RUN_BYTES: u64 = 20 * 1024 * 1024;
/// How many rotated logs are kept.
pub const KEEP: u32 = 4;

/// The log file path for a plug-in.
pub fn path(dir: &Path, plugin: &str) -> PathBuf {
    dir.join(format!("{plugin}.log"))
}

fn rotated(dir: &Path, plugin: &str, n: u32) -> PathBuf {
    dir.join(format!("{plugin}.{n}.log"))
}

/// Rotate the log if it is large, then open it for appending.
pub fn open(dir: &Path, plugin: &str) -> Option<File> {
    let current = path(dir, plugin);
    if std::fs::metadata(&current).map(|m| m.len() > ROTATE_BYTES).unwrap_or(false) {
        let _ = std::fs::remove_file(rotated(dir, plugin, KEEP));
        for n in (1..KEEP).rev() {
            let _ = std::fs::rename(rotated(dir, plugin, n), rotated(dir, plugin, n + 1));
        }
        let _ = std::fs::rename(&current, rotated(dir, plugin, 1));
    }
    OpenOptions::new().create(true).append(true).open(&current).ok()
}

/// Copy `stream` into the plug-in's log on a background thread until the stream ends.
pub fn capture(mut stream: impl Read + Send + 'static, dir: &Path, plugin: &str) {
    let file = open(dir, plugin);
    let name = format!("plug-in {plugin} log");
    let spawned = thread::Builder::new().name(name).spawn(move || {
        let mut file = file;
        let mut written: u64 = 0;
        let mut buffer = [0u8; 8192];
        loop {
            let n = match stream.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            let Some(out) = file.as_mut() else { continue };
            if written >= MAX_RUN_BYTES {
                continue; // keep draining so the plug-in never blocks on a full pipe
            }
            let chunk = buffer.get(..n).unwrap_or_default();
            if out.write_all(chunk).is_err() {
                file = None;
                continue;
            }
            written = written.saturating_add(n as u64);
            if written >= MAX_RUN_BYTES {
                let _ = writeln!(out, "\n[PhotoCraft: log limit of {MAX_RUN_BYTES} bytes reached; further output dropped]");
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("could not capture the log of plug-in '{plugin}': {e}");
    }
}
