//! Data process plug-ins keep in documents: named binary blobs per document
//! ([`crate::Document::plugin_data`]) and small text values per layer
//! ([`crate::Layer::plugin_data`]), with the limits on both and their file encodings where a
//! format has no field of its own for them.
//!
//! - **PSD/PSB**: the document's blobs go in image resource [`PSD_RESOURCE`] as the
//!   [`encode`]d container; a layer's values go in its XMP packet as `pcpp:data`
//!   ([`layer_xmp`] / [`take_layer_xmp`]).
//! - **`.pcraft`** stores both natively.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::plugin_xmp;

/// PSD image resource holding the document's plug-in blobs. Photoshop reserves 4000–4999 for
/// plug-in resources.
pub const PSD_RESOURCE: u16 = 4242;
/// The PSD resource's name.
pub const PSD_RESOURCE_NAME: &str = "PhotoCraft plug-in data";
/// Most bytes of blobs one document holds (all plug-ins together).
pub const MAX_DOCUMENT_BYTES: u64 = 256 << 20;
/// Longest blob name or layer key, in bytes.
pub const MAX_NAME: usize = 256;
/// Most bytes of text values one layer holds (keys included).
pub const MAX_LAYER_BYTES: usize = 1 << 20;

/// Bytes of all of a document's blobs.
pub fn document_bytes(data: &BTreeMap<String, Arc<Vec<u8>>>) -> u64 {
    data.values().map(|v| v.len() as u64).sum()
}

/// Bytes of a layer's values, keys included.
pub fn layer_bytes(data: &BTreeMap<String, String>) -> usize {
    data.iter().map(|(k, v)| k.len() + v.len()).sum()
}

/// The PSD container: a big-endian `u32` count, then per blob a `u16` key length, the UTF-8 key,
/// a `u64` length and the bytes.
pub fn encode(data: &BTreeMap<String, Arc<Vec<u8>>>) -> Vec<u8> {
    let size = 4 + data.iter().map(|(k, v)| 2 + k.len() + 8 + v.len()).sum::<usize>();
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    for (key, value) in data {
        // Keys are at most MAX_NAME bytes plus a plug-in id, far below u16::MAX.
        out.extend_from_slice(&(key.len().min(usize::from(u16::MAX)) as u16).to_be_bytes());
        out.extend_from_slice(&key.as_bytes()[..key.len().min(usize::from(u16::MAX))]);
        out.extend_from_slice(&(value.len() as u64).to_be_bytes());
        out.extend_from_slice(value);
    }
    out
}

/// The blobs of an [`encode`]d container, or `None` when it is malformed (truncated, a key that
/// isn't UTF-8, trailing bytes, or more than [`MAX_DOCUMENT_BYTES`]).
pub fn decode(bytes: &[u8]) -> Option<BTreeMap<String, Arc<Vec<u8>>>> {
    let mut at = 0usize;
    let mut take = |n: usize| -> Option<&[u8]> {
        let piece = bytes.get(at..at.checked_add(n)?)?;
        at += n;
        Some(piece)
    };
    let count = u32::from_be_bytes(take(4)?.try_into().ok()?);
    let mut out = BTreeMap::new();
    let mut total = 0u64;
    for _ in 0..count {
        let key_len = usize::from(u16::from_be_bytes(take(2)?.try_into().ok()?));
        let key = std::str::from_utf8(take(key_len)?).ok()?.to_string();
        let len = u64::from_be_bytes(take(8)?.try_into().ok()?);
        total = total.checked_add(len)?;
        if total > MAX_DOCUMENT_BYTES {
            return None;
        }
        let value = take(usize::try_from(len).ok()?)?.to_vec();
        out.insert(key, Arc::new(value));
    }
    (at == bytes.len()).then_some(out)
}

/// `xmp` with the layer's values added as `pcpp:data` (a JSON object), or `None` when the layer
/// has none (the packet is written as it was).
pub fn layer_xmp(xmp: Option<&str>, data: &BTreeMap<String, String>) -> Option<String> {
    if data.is_empty() {
        return None;
    }
    let json = serde_json::to_string(data).ok()?;
    plugin_xmp::set(xmp, "data", &json)
}

/// The layer values in `xmp`'s `pcpp:data`, and the packet without it (`None` when only a
/// packet PhotoCraft made for its own properties is left). Malformed data stays in the packet,
/// so nothing is lost.
pub fn take_layer_xmp(xmp: &str) -> (BTreeMap<String, String>, Option<String>) {
    let Some(json) = plugin_xmp::get(xmp, "data") else { return (BTreeMap::new(), Some(xmp.to_string())) };
    match serde_json::from_str::<BTreeMap<String, String>>(&json) {
        Ok(data) => (data, plugin_xmp::remove(xmp, "data")),
        Err(_) => (BTreeMap::new(), Some(xmp.to_string())),
    }
}

/// Whether the document holds any plug-in data, its own or on a layer.
pub fn any(doc: &crate::Document) -> bool {
    !doc.plugin_data.is_empty() || doc.walk().into_iter().any(|(_, _, l)| !l.plugin_data.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blobs() -> BTreeMap<String, Arc<Vec<u8>>> {
        BTreeMap::from([
            ("org.example.p/ui.json".to_string(), Arc::new(b"{\"a\":1}".to_vec())),
            ("org.example.p/result0.webp".to_string(), Arc::new((0..=255).collect())),
            ("org.example.q/empty".to_string(), Arc::new(Vec::new())),
        ])
    }

    #[test]
    fn the_container_round_trips() {
        let data = blobs();
        assert_eq!(decode(&encode(&data)), Some(data));
        assert_eq!(decode(&encode(&BTreeMap::new())), Some(BTreeMap::new()));
    }

    #[test]
    fn malformed_containers_are_refused() {
        let bytes = encode(&blobs());
        for cut in 0..bytes.len() {
            assert_eq!(decode(&bytes[..cut]), None, "truncated at {cut}");
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(decode(&trailing), None);
        // A claimed length past the limit is refused before anything is allocated.
        let mut huge = vec![0, 0, 0, 1, 0, 1, b'k'];
        huge.extend_from_slice(&u64::MAX.to_be_bytes());
        assert_eq!(decode(&huge), None);
        let mut many = u32::MAX.to_be_bytes().to_vec();
        many.extend_from_slice(&[0, 0]);
        assert_eq!(decode(&many), None);
        let bad_utf8 = [0, 0, 0, 1, 0, 1, 0xff, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(decode(&bad_utf8), None);
    }

    #[test]
    fn layer_values_go_into_and_out_of_xmp() {
        let data = BTreeMap::from([("org.example.p/role".to_string(), "control \"depth\" <&>".to_string())]);
        let packet = layer_xmp(None, &data).unwrap();
        let (back, rest) = take_layer_xmp(&packet);
        assert_eq!(back, data);
        assert_eq!(rest, None, "the packet made for the data disappears");
        let user = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" dc:format=\"x\"/></rdf:RDF></x:xmpmeta>";
        let (back, rest) = take_layer_xmp(&layer_xmp(Some(user), &data).unwrap());
        assert_eq!((back, rest.as_deref()), (data, Some(user)));
        assert_eq!(layer_xmp(Some(user), &BTreeMap::new()), None);
        // Data that isn't what PhotoCraft writes stays where it is.
        let odd = plugin_xmp::set(Some(user), "data", "not json").unwrap();
        assert_eq!(take_layer_xmp(&odd), (BTreeMap::new(), Some(odd.clone())));
    }
}
