//! DST (Direct Stream Transfer) decoder.
//!
//! Pure-Rust port of `libdstdec/` from
//! [Sound-Linux-More/sacd-extract][upstream]. See `INTEGRATION.md` in
//! this directory for the integration contract, fixture provenance,
//! and acceptance criteria.
//!
//! [upstream]: https://github.com/Sound-Linux-More/sacd-extract
//!
//! Status: **stub** — entry point returns `DstError::InternalDecodeError`
//! until the port lands (PR 2).

/// DST decoder error. Every C `assert()` / failure path in the upstream
/// decoder must map to one of these variants — no panics on malformed
/// input.
#[derive(Debug)]
pub enum DstError {
    /// Bit/byte reader ran past the end of the input frame.
    /// `consumed` is whole bytes consumed from `input` before
    /// exhaustion.
    UnexpectedEof { consumed: usize },
    /// Frame header or stream syntax violated the DST spec. Also
    /// used for invalid `channel_count` and for short output
    /// (decoder finished with fewer than `channel_count * 4704`
    /// bytes).
    MalformedFrame(&'static str),
    /// Decoder tried to emit more output than the fixed budget.
    /// `limit` is `channel_count * 4704`.
    OutputOverflow { limit: usize },
    /// Catch-all for upstream `return -1` paths that don't fit a more
    /// specific variant. Use sparingly; prefer adding a typed variant
    /// when the cause is identifiable.
    InternalDecodeError(&'static str),
}

impl std::fmt::Display for DstError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DstError::UnexpectedEof { consumed } => {
                write!(f, "unexpected EOF in DST stream after {} bytes", consumed)
            }
            DstError::MalformedFrame(msg) => write!(f, "malformed DST frame: {}", msg),
            DstError::OutputOverflow { limit } => {
                write!(f, "decoded output exceeded budget ({} bytes)", limit)
            }
            DstError::InternalDecodeError(msg) => write!(f, "internal decoder error: {}", msg),
        }
    }
}

impl std::error::Error for DstError {}

/// Decode one DST-encoded SACD frame into clustered-frame DSD bytes.
///
/// - `input`: raw DST payload of exactly one frame (variable length).
/// - `channel_count`: 2 (stereo) or 6 (multi-channel). Any other
///   value must return `DstError::MalformedFrame("invalid channel_count")`.
/// - Returns a `Vec<u8>` whose `len()` is exactly `channel_count * 4704`,
///   byte-interleaved across channels, with each byte MSB-first in time
///   order (oldest sample in the high bit). Bit-identical to what an
///   uncompressed SACD frame carries in `Frame::data`.
///
/// **Not yet implemented.** Tracked by PR 2; see `INTEGRATION.md`.
pub fn decode_frame(_input: &[u8], _channel_count: u8) -> Result<Vec<u8>, DstError> {
    Err(DstError::InternalDecodeError("DST decoder not yet ported (PR 2)"))
}
