//! Images and icons plug-ins send once (`image.put`) and then refer to as `"image:<hash>"`.
//!
//! Each is kept encoded (PNG or SVG) until first drawn, then as a texture. The cache is
//! bounded: past [`MAX_BYTES`] of encoded data, the least recently used images go, and a
//! widget showing one draws nothing until the plug-in sends it again.

use std::collections::HashMap;

/// Most encoded bytes kept for all plug-ins.
pub const MAX_BYTES: usize = 256 << 20;
/// Largest single image.
pub const MAX_IMAGE: usize = 32 << 20;
/// Largest side an SVG icon is drawn at, and largest decoded image side.
const MAX_SIDE: u32 = 4096;

struct Entry {
    svg: bool,
    data: Vec<u8>,
    texture: Option<Option<egui::TextureHandle>>,
    used: u64,
}

/// The image cache, keyed by plug-in and hash.
#[derive(Default)]
pub struct ImageCache {
    entries: HashMap<(String, String), Entry>,
    bytes: usize,
    clock: u64,
}

/// Whether `hash` is a usable cache key: 1–128 ASCII letters, digits, `-` or `_`.
fn valid_hash(hash: &str) -> bool {
    !hash.is_empty() && hash.len() <= 128 && hash.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl ImageCache {
    /// Store `data` (`encoding` "png" or "svg") from `plugin` under `hash`.
    pub fn put(&mut self, plugin: &str, hash: &str, encoding: &str, data: &[u8]) -> Result<(), String> {
        if !valid_hash(hash) {
            return Err("an image hash is 1 to 128 letters, digits, '-' or '_'".into());
        }
        let svg = match encoding {
            "png" => false,
            "svg" => true,
            other => return Err(format!("unknown image encoding '{other}' (png or svg)")),
        };
        if data.is_empty() || data.len() > MAX_IMAGE {
            return Err(format!("an image is 1 byte to {MAX_IMAGE} bytes"));
        }
        let key = (plugin.to_string(), hash.to_string());
        if let Some(old) = self.entries.remove(&key) {
            self.bytes -= old.data.len();
        }
        self.clock += 1;
        self.bytes += data.len();
        self.entries.insert(key, Entry { svg, data: data.to_vec(), texture: None, used: self.clock });
        self.evict();
        Ok(())
    }

    fn evict(&mut self) {
        while self.bytes > MAX_BYTES {
            let Some(oldest) = self.entries.iter().min_by_key(|(_, e)| e.used).map(|(k, _)| k.clone()) else { break };
            if let Some(e) = self.entries.remove(&oldest) {
                self.bytes -= e.data.len();
            }
        }
    }

    /// The texture for `plugin`'s `reference` (`"image:<hash>"`), decoding it on first use.
    /// `None` when it isn't cached or can't be decoded.
    pub fn texture(&mut self, ctx: &egui::Context, plugin: &str, reference: &str) -> Option<egui::TextureHandle> {
        let hash = reference.strip_prefix("image:")?;
        self.clock += 1;
        let clock = self.clock;
        let entry = self.entries.get_mut(&(plugin.to_string(), hash.to_string()))?;
        entry.used = clock;
        if entry.texture.is_none() {
            let decoded = decode(entry.svg, &entry.data);
            if decoded.is_none() {
                log::debug!("plug-in '{plugin}': image '{hash}' can't be decoded");
            }
            entry.texture = Some(decoded.map(|image| ctx.load_texture(format!("plugin-image:{plugin}:{hash}"), image, egui::TextureOptions::LINEAR)));
        }
        entry.texture.clone().flatten()
    }

    /// Whether `plugin` has an image under `hash`.
    pub fn contains(&self, plugin: &str, hash: &str) -> bool {
        self.entries.contains_key(&(plugin.to_string(), hash.to_string()))
    }

    /// Forget `plugin`'s images.
    pub fn drop_plugin(&mut self, plugin: &str) {
        let gone: Vec<_> = self.entries.keys().filter(|(p, _)| p == plugin).cloned().collect();
        for key in gone {
            if let Some(e) = self.entries.remove(&key) {
                self.bytes -= e.data.len();
            }
        }
    }

    /// Encoded bytes held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

fn decode(svg: bool, data: &[u8]) -> Option<egui::ColorImage> {
    let (w, h, rgba) = if svg {
        photocraft_io::svg::rasterize_rgba8(data).ok()?
    } else {
        let image = photocraft_codecs::decode(data).ok()?;
        (image.width(), image.height(), image.to_rgba8())
    };
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || rgba.len() != (w * h * 4) as usize {
        return None;
    }
    Some(egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba))
}
