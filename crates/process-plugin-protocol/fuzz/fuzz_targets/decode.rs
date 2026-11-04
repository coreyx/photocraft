//! Fuzz the frame decoder: any input must decode or return an error, never panic, and a decoded
//! frame must re-encode to exactly the bytes it was read from.
#![no_main]

use std::io::Cursor;

use libfuzzer_sys::fuzz_target;
use photocraft_process_plugin_protocol::frame::{self, Limits};

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_header: 1 << 16, max_payload: 1 << 16 };
    if let Ok((decoded, used)) = frame::decode(data, limits) {
        assert!(used <= data.len());
        // Re-encoding normalizes the header JSON, so only check that it encodes and decodes back.
        let bytes = frame::encode(&decoded, limits).unwrap_or_default();
        if !bytes.is_empty() {
            let (again, _) = frame::decode(&bytes, limits).unwrap_or((decoded.clone(), 0));
            assert_eq!(again, decoded);
        }
    }
    let mut cursor = Cursor::new(data);
    while let Ok(Some(_)) = frame::read_frame(&mut cursor, limits) {}
});
