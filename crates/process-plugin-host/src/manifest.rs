//! Plug-in manifests (`photocraft-plugin.toml`).
//!
//! Every installed plug-in is a folder holding a manifest:
//!
//! ```toml
//! id = "org.example.my-plugin"     # reverse-DNS, lowercase
//! name = "My Plug-in"
//! version = "1.2.0"
//! protocol = 1                     # the process plug-in protocol version it speaks
//! license = "MIT"
//! description = "What it does."    # optional
//! command = ["bin/my-plugin"]      # program inside the folder, then its arguments
//! ```
//!
//! The program must live inside the plug-in folder: an absolute path, `..` or a bare program
//! name looked up on `PATH` is refused, so installing a plug-in can't run something else.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

/// The manifest's file name.
pub const FILE_NAME: &str = "photocraft-plugin.toml";

/// Largest manifest read, in bytes.
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

/// A parsed, validated manifest.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub protocol: u32,
    pub license: String,
    #[serde(default)]
    pub description: String,
    pub command: Vec<String>,
}

impl Manifest {
    /// Parse and validate a manifest's text.
    pub fn parse(text: &str) -> Result<Manifest, String> {
        let manifest: Manifest = toml::from_str(text).map_err(|e| format!("invalid manifest: {e}"))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Read and validate the manifest in `folder`.
    pub fn load(folder: &Path) -> Result<Manifest, String> {
        let path = folder.join(FILE_NAME);
        let meta = std::fs::metadata(&path).map_err(|e| format!("no {FILE_NAME}: {e}"))?;
        if meta.len() > MAX_MANIFEST_BYTES {
            return Err(format!("{FILE_NAME} is larger than {MAX_MANIFEST_BYTES} bytes"));
        }
        let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {FILE_NAME}: {e}"))?;
        Manifest::parse(&text)
    }

    fn validate(&self) -> Result<(), String> {
        let id_ok = !self.id.is_empty()
            && self.id.len() <= 128
            && self.id.contains('.')
            && self.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'))
            && !self.id.starts_with('.')
            && !self.id.ends_with('.');
        if !id_ok {
            return Err(format!("invalid id '{}': use lowercase reverse-DNS such as 'org.example.plugin'", self.id));
        }
        for (field, value) in [("name", &self.name), ("version", &self.version), ("license", &self.license)] {
            if value.trim().is_empty() || value.len() > 200 {
                return Err(format!("'{field}' must be 1 to 200 characters"));
            }
        }
        let program = self.command.first().ok_or("'command' is empty")?;
        relative_program(program)?;
        if self.command.len() > 64 || self.command.iter().any(|a| a.len() > 4096 || a.contains('\0')) {
            return Err("'command' has too many or too long arguments".into());
        }
        Ok(())
    }

    /// The program to run and its arguments, with the program resolved inside `folder`.
    pub fn resolve_command(&self, folder: &Path) -> Result<(PathBuf, Vec<String>), String> {
        let program = self.command.first().ok_or("'command' is empty")?;
        let path = folder.join(relative_program(program)?);
        let args = self.command.get(1..).unwrap_or_default().to_vec();
        Ok((path, args))
    }
}

/// Check that a manifest's program is a relative path that stays inside the plug-in folder.
fn relative_program(program: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(program);
    if program.is_empty() || path.is_absolute() || path.has_root() {
        return Err(format!("command '{program}' must be a path inside the plug-in folder"));
    }
    if !path.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir)) {
        return Err(format!("command '{program}' must not leave the plug-in folder"));
    }
    if path.components().count() < 2 && !program.starts_with("./") && !program.contains('/') && !program.contains('\\') {
        // A bare name would be looked up on PATH by the OS; require an explicit folder path.
        return Err(format!("command '{program}' must name a file in the plug-in folder, e.g. './{program}'"));
    }
    Ok(path)
}
