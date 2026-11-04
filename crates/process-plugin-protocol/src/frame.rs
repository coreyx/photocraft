//! The frame codec.
//!
//! A frame is laid out as:
//!
//! ```text
//! magic "PCPP" (4 bytes) | header length u32 LE | payload length u64 LE | header JSON | payload
//! ```
//!
//! The header is UTF-8 JSON (a [`Header`]); the payload is raw bytes whose meaning the header's
//! method or topic defines (for example pixel rows). Both lengths are checked against [`Limits`]
//! *before* anything is allocated, so a hostile or corrupt length can't exhaust memory, and every
//! malformed input is an error, never a panic.

use std::io::{self, Read, Write};

use crate::message::Header;

/// The four bytes that start every frame.
pub const MAGIC: [u8; 4] = *b"PCPP";

/// Size of the fixed part of a frame: magic, header length and payload length.
pub const PREFIX_LEN: usize = 4 + 4 + 8;

/// Upper bounds on a frame's parts. Anything larger is refused without being read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest accepted JSON header, in bytes.
    pub max_header: u32,
    /// Largest accepted binary payload, in bytes.
    pub max_payload: u64,
}

impl Default for Limits {
    /// 1 MiB of header and 512 MiB of payload: room for a 8192 x 8192 RGBA 16-bit image, while a
    /// runaway length is still refused.
    fn default() -> Self {
        Limits { max_header: 1 << 20, max_payload: 512 << 20 }
    }
}

/// One decoded message: its header and its (possibly empty) binary payload.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub header: Header,
    pub payload: Vec<u8>,
}

impl Frame {
    /// A frame with no payload.
    pub fn new(header: Header) -> Self {
        Frame { header, payload: Vec::new() }
    }

    /// A frame carrying binary data.
    pub fn with_payload(header: Header, payload: Vec<u8>) -> Self {
        Frame { header, payload }
    }
}

/// Everything that can go wrong reading or writing a frame.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("not a plug-in frame (bad magic {0:02x?})")]
    BadMagic([u8; 4]),
    #[error("frame header of {0} bytes exceeds the {1}-byte limit")]
    HeaderTooLarge(u64, u32),
    #[error("frame payload of {0} bytes exceeds the {1}-byte limit")]
    PayloadTooLarge(u64, u64),
    #[error("invalid frame header: {0}")]
    InvalidHeader(String),
    #[error("frame ended early")]
    Truncated,
    #[error("frame I/O: {0}")]
    Io(String),
}

impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::UnexpectedEof { FrameError::Truncated } else { FrameError::Io(e.to_string()) }
    }
}

/// Serialize a frame to bytes, checking it against `limits`.
pub fn encode(frame: &Frame, limits: Limits) -> Result<Vec<u8>, FrameError> {
    let header = serde_json::to_vec(&frame.header).map_err(|e| FrameError::InvalidHeader(e.to_string()))?;
    let header_len =
        u32::try_from(header.len()).ok().filter(|&n| n <= limits.max_header).ok_or(FrameError::HeaderTooLarge(header.len() as u64, limits.max_header))?;
    let payload_len = frame.payload.len() as u64;
    if payload_len > limits.max_payload {
        return Err(FrameError::PayloadTooLarge(payload_len, limits.max_payload));
    }
    let mut out = Vec::with_capacity(PREFIX_LEN + header.len() + frame.payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&header_len.to_le_bytes());
    out.extend_from_slice(&payload_len.to_le_bytes());
    out.extend_from_slice(&header);
    out.extend_from_slice(&frame.payload);
    Ok(out)
}

/// Write one frame to `out`. The frame is assembled first and written with a single call, so
/// frames from one writer never interleave.
pub fn write_frame<W: Write>(out: &mut W, frame: &Frame, limits: Limits) -> Result<(), FrameError> {
    let bytes = encode(frame, limits)?;
    out.write_all(&bytes)?;
    out.flush()?;
    Ok(())
}

/// The lengths read from a frame's fixed prefix, already checked against the limits.
struct Prefix {
    header_len: usize,
    payload_len: usize,
}

fn parse_prefix(prefix: &[u8; PREFIX_LEN], limits: Limits) -> Result<Prefix, FrameError> {
    let mut magic = [0u8; 4];
    magic.copy_from_slice(&prefix[0..4]);
    if magic != MAGIC {
        return Err(FrameError::BadMagic(magic));
    }
    let mut word = [0u8; 4];
    word.copy_from_slice(&prefix[4..8]);
    let header_len = u32::from_le_bytes(word);
    let mut dword = [0u8; 8];
    dword.copy_from_slice(&prefix[8..16]);
    let payload_len = u64::from_le_bytes(dword);
    if header_len > limits.max_header {
        return Err(FrameError::HeaderTooLarge(u64::from(header_len), limits.max_header));
    }
    if payload_len > limits.max_payload {
        return Err(FrameError::PayloadTooLarge(payload_len, limits.max_payload));
    }
    // Both lengths are now bounded by the limits, which fit in usize on every supported target.
    let header_len = usize::try_from(header_len).map_err(|_| FrameError::HeaderTooLarge(u64::from(header_len), limits.max_header))?;
    let payload_len = usize::try_from(payload_len).map_err(|_| FrameError::PayloadTooLarge(payload_len, limits.max_payload))?;
    Ok(Prefix { header_len, payload_len })
}

fn parse_header(bytes: &[u8]) -> Result<Header, FrameError> {
    serde_json::from_slice(bytes).map_err(|e| FrameError::InvalidHeader(e.to_string()))
}

/// Read the next frame from `input`.
///
/// Returns `Ok(None)` when the stream ends cleanly between frames (the peer closed its end), and
/// [`FrameError::Truncated`] when it ends inside one.
pub fn read_frame<R: Read>(input: &mut R, limits: Limits) -> Result<Option<Frame>, FrameError> {
    let mut prefix = [0u8; PREFIX_LEN];
    let mut filled = 0;
    while filled < PREFIX_LEN {
        let slice = prefix.get_mut(filled..).ok_or(FrameError::Truncated)?;
        match input.read(slice) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    let lens = parse_prefix(&prefix, limits)?;
    let mut header = vec![0u8; lens.header_len];
    input.read_exact(&mut header)?;
    let header = parse_header(&header)?;
    let mut payload = vec![0u8; lens.payload_len];
    input.read_exact(&mut payload)?;
    Ok(Some(Frame { header, payload }))
}

/// Decode one frame from the start of `bytes`.
///
/// Returns the frame and the number of bytes it used. [`FrameError::Truncated`] means `bytes`
/// holds only part of a frame; more input may complete it.
pub fn decode(bytes: &[u8], limits: Limits) -> Result<(Frame, usize), FrameError> {
    let prefix: &[u8; PREFIX_LEN] = bytes.get(..PREFIX_LEN).and_then(|s| s.try_into().ok()).ok_or(FrameError::Truncated)?;
    let lens = parse_prefix(prefix, limits)?;
    let header_end = PREFIX_LEN.checked_add(lens.header_len).ok_or(FrameError::Truncated)?;
    let end = header_end.checked_add(lens.payload_len).ok_or(FrameError::Truncated)?;
    let header_bytes = bytes.get(PREFIX_LEN..header_end).ok_or(FrameError::Truncated)?;
    let payload = bytes.get(header_end..end).ok_or(FrameError::Truncated)?;
    let header = parse_header(header_bytes)?;
    Ok((Frame { header, payload: payload.to_vec() }, end))
}
