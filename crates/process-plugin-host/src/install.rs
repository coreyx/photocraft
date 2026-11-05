//! Installing a plug-in package.
//!
//! A package is a `.zip` holding a plug-in folder: the manifest either at the archive's root or
//! inside one top-level folder. Installing happens in two steps so a running copy can be stopped
//! in between: [`stage`] extracts the package into a staging folder inside the plug-ins folder
//! and validates its manifest, then [`Staged::place`] moves it into place as
//! `<plugins dir>/<plugin id>`, replacing an installed copy with the same id. A package can't
//! write outside its staging folder (entries with absolute paths or `..` and links are refused)
//! and is capped in size and entry count.
//!
//! Files extracted here are new files written by PhotoCraft, so they carry no download
//! quarantine flag (macOS) even when the `.zip` itself does.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::manifest::{FILE_NAME, Manifest};

/// Largest total size a package may extract to.
pub const MAX_EXTRACTED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Most entries a package may have.
pub const MAX_ENTRIES: usize = 100_000;

/// An extracted, validated package waiting to be moved into place. Its staging folder is removed
/// when it is dropped, whether or not it was placed.
pub struct Staged {
    pub manifest: Manifest,
    root: PathBuf,
    _cleanup: Cleanup,
}

/// Removes the staging folder when dropped, including when staging fails part-way.
struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Extract `package` into a staging folder inside `plugins_dir` and validate its manifest.
pub fn stage(package: &Path, plugins_dir: &Path) -> Result<Staged, String> {
    std::fs::create_dir_all(plugins_dir).map_err(|e| format!("could not create {}: {e}", plugins_dir.display()))?;
    let staging = plugins_dir.join(format!(".installing-{}", std::process::id()));
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(|e| format!("could not clear {}: {e}", staging.display()))?;
    }
    let cleanup = Cleanup(staging.clone());
    extract(package, &staging)?;
    let root = package_root(&staging)?;
    let manifest = Manifest::load(&root)?;
    Ok(Staged { manifest, root, _cleanup: cleanup })
}

impl Staged {
    /// Move the package into `<plugins_dir>/<id>`, replacing whatever is there. The installed copy
    /// must already be stopped.
    pub fn place(&self, plugins_dir: &Path) -> Result<PathBuf, String> {
        let target = plugins_dir.join(&self.manifest.id);
        if target.exists() {
            remove_with_retry(&target).map_err(|e| format!("could not replace the installed copy at {}: {e}", target.display()))?;
        }
        std::fs::rename(&self.root, &target).map_err(|e| format!("could not move the plug-in into {}: {e}", target.display()))?;
        Ok(target)
    }
}

/// A just-stopped process can keep its program file locked for a moment on Windows: retry
/// briefly instead of failing the install.
fn remove_with_retry(path: &Path) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(e) if attempt < 20 => {
                log::debug!("retrying removal of {}: {e}", path.display());
                attempt += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
}

/// The folder holding the manifest: the staging folder itself, or its single top-level folder.
fn package_root(staging: &Path) -> Result<PathBuf, String> {
    if staging.join(FILE_NAME).is_file() {
        return Ok(staging.to_path_buf());
    }
    let entries: Vec<PathBuf> = std::fs::read_dir(staging).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).collect();
    match entries.as_slice() {
        [only] if only.is_dir() && only.join(FILE_NAME).is_file() => Ok(only.clone()),
        _ => Err(format!("the package has no {FILE_NAME} at its root or in a single top-level folder")),
    }
}

fn extract(package: &Path, dest: &Path) -> Result<(), String> {
    let file = File::open(package).map_err(|e| format!("could not open {}: {e}", package.display()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("not a plug-in package (.zip): {e}"))?;
    if archive.len() > MAX_ENTRIES {
        return Err(format!("the package has more than {MAX_ENTRIES} entries"));
    }
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut total: u64 = 0;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|e| format!("damaged package: {e}"))?;
        let name = entry.name().map(|n| n.to_string()).unwrap_or_else(|_| format!("#{index}"));
        let Some(relative) = entry.enclosed_name() else {
            return Err(format!("the package entry '{name}' points outside the plug-in folder"));
        };
        let path = dest.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
            continue;
        }
        if entry.is_symlink() {
            return Err(format!("the package entry '{name}' is a link, which plug-in packages may not contain"));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = File::create(&path).map_err(|e| format!("could not write {}: {e}", path.display()))?;
        // Count what is actually inflated, not the size the archive claims.
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let n = entry.read(&mut buffer).map_err(|e| format!("damaged package entry '{name}': {e}"))?;
            if n == 0 {
                break;
            }
            total = total.saturating_add(n as u64);
            if total > MAX_EXTRACTED_BYTES {
                return Err(format!("the package extracts to more than {MAX_EXTRACTED_BYTES} bytes"));
            }
            out.write_all(buffer.get(..n).unwrap_or_default()).map_err(|e| format!("could not write {}: {e}", path.display()))?;
        }
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            // Keep the executable bits (the plug-in's program); never set-uid/set-gid/sticky.
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode & 0o777));
        }
    }
    Ok(())
}
