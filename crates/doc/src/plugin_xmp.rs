//! PhotoCraft's own properties inside a layer's XMP packet.
//!
//! Formats without a field of their own for PhotoCraft's layer data (PSD, PSB, layered TIFF) carry
//! it in the layer's XMP packet as attributes in the `pcpp` namespace
//! ([`NAMESPACE`]) on the packet's first `rdf:Description`: `pcpp:uid` for the persistent layer
//! uid, and so on. Export merges them into the user's packet and import takes them back out, so the
//! XMP the user sees and edits never shows them. A layer without XMP of its own gets a minimal
//! packet ([`minimal_packet`]) that disappears again on import.
//!
//! This is deliberately a small text transform rather than an XMP parser: it only ever touches its
//! own namespace's attributes and leaves every other byte of the packet as it was.

/// The namespace URI of PhotoCraft's layer properties.
pub const NAMESPACE: &str = "https://photocraft.invalid/ns/process-plugin/1.0/";
/// The namespace prefix used when writing.
pub const PREFIX: &str = "pcpp";

const DESCRIPTION: &str = "<rdf:Description";

/// The packet written for a layer that has no XMP of its own (then holding only `pcpp` attributes).
pub fn minimal_packet() -> String {
    "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\"/></rdf:RDF></x:xmpmeta>".to_string()
}

/// The value of `pcpp:<name>` in `xmp`, if present.
pub fn get(xmp: &str, name: &str) -> Option<String> {
    let (start, end) = attribute_span(xmp, name)?;
    let value = xmp.get(start..end)?;
    let open = value.find('"')?;
    let close = value.rfind('"')?;
    value.get(open + 1..close).map(unescape)
}

/// `xmp` (or a minimal packet when `None`) with `pcpp:<name>` set to `value`. Returns `None` when
/// the packet has no `rdf:Description` to carry it.
pub fn set(xmp: Option<&str>, name: &str, value: &str) -> Option<String> {
    let base = match xmp {
        Some(x) => x.to_string(),
        None => minimal_packet(),
    };
    let attribute = format!("{PREFIX}:{name}=\"{}\"", escape(value));
    if let Some((start, end)) = attribute_span(&base, name) {
        let mut out = base;
        out.replace_range(start..end, &attribute);
        return Some(out);
    }
    let at = base.find(DESCRIPTION)? + DESCRIPTION.len();
    let declaration = if base.contains(&format!("xmlns:{PREFIX}=")) { String::new() } else { format!(" xmlns:{PREFIX}=\"{NAMESPACE}\"") };
    let mut out = base;
    out.insert_str(at, &format!("{declaration} {attribute}"));
    Some(out)
}

/// `xmp` without `pcpp:<name>`. When nothing but the minimal packet is left, returns `None`
/// (the layer had no XMP of its own).
pub fn remove(xmp: &str, name: &str) -> Option<String> {
    let mut out = xmp.to_string();
    if let Some((start, end)) = attribute_span(&out, name) {
        // Also drop the space before the attribute.
        let start = if out.get(..start).is_some_and(|s| s.ends_with(' ')) { start - 1 } else { start };
        out.replace_range(start..end, "");
    }
    // Once no pcpp attribute is left, drop the namespace declaration this code added.
    if !has_attribute(&out) {
        out = out.replacen(&format!(" xmlns:{PREFIX}=\"{NAMESPACE}\""), "", 1);
    }
    if out == minimal_packet() { None } else { Some(out) }
}

/// Whether any `pcpp:` attribute is in `xmp` (the `xmlns:pcpp=` declaration doesn't count).
fn has_attribute(xmp: &str) -> bool {
    xmp.contains(&format!("{PREFIX}:"))
}

/// Byte range of the ` pcpp:<name>="…"` attribute text (without the leading space), if present.
fn attribute_span(xmp: &str, name: &str) -> Option<(usize, usize)> {
    let key = format!("{PREFIX}:{name}=\"");
    let start = xmp.find(&key)?;
    let value_start = start + key.len();
    let close = xmp.get(value_start..)?.find('"')?;
    Some((start, value_start + close + 1))
}

fn escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

fn unescape(value: &str) -> String {
    value.replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title>Sky</dc:title></rdf:Description></rdf:RDF></x:xmpmeta>";

    #[test]
    fn set_get_and_remove_round_trip_the_users_packet() {
        let with = set(Some(USER), "uid", "00ff").unwrap();
        assert!(with.contains("xmlns:pcpp="));
        assert_eq!(get(&with, "uid").as_deref(), Some("00ff"));
        assert!(with.contains("<dc:title>Sky</dc:title>"));
        // Setting again replaces the value instead of adding a second attribute.
        let again = set(Some(&with), "uid", "abcd").unwrap();
        assert_eq!(again.matches("pcpp:uid=").count(), 1);
        assert_eq!(get(&again, "uid").as_deref(), Some("abcd"));
        assert_eq!(remove(&again, "uid").as_deref(), Some(USER));
    }

    #[test]
    fn a_layer_without_xmp_gets_a_minimal_packet_that_goes_away() {
        let packet = set(None, "uid", "1").unwrap();
        assert_eq!(get(&packet, "uid").as_deref(), Some("1"));
        assert_eq!(remove(&packet, "uid"), None);
    }

    #[test]
    fn values_are_escaped() {
        let packet = set(None, "data", "{\"a\":\"<b>&c\"}").unwrap();
        assert!(!packet.contains("<b>"));
        assert_eq!(get(&packet, "data").as_deref(), Some("{\"a\":\"<b>&c\"}"));
    }

    #[test]
    fn several_properties_coexist() {
        let packet = set(None, "uid", "1").unwrap();
        let packet = set(Some(&packet), "data", "x").unwrap();
        assert_eq!(packet.matches("xmlns:pcpp=").count(), 1);
        let packet = remove(&packet, "uid").unwrap();
        assert_eq!(get(&packet, "data").as_deref(), Some("x"));
        assert!(packet.contains("xmlns:pcpp="), "still declared while pcpp:data uses it");
        assert_eq!(remove(&packet, "data"), None);
    }

    #[test]
    fn a_packet_without_a_description_is_left_alone() {
        assert_eq!(set(Some("<x:xmpmeta/>"), "uid", "1"), None);
        assert_eq!(get("<x:xmpmeta/>", "uid"), None);
        assert_eq!(remove("<x:xmpmeta/>", "uid").as_deref(), Some("<x:xmpmeta/>"));
    }
}
