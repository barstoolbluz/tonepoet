//! DST (Direct Stream Transfer) decoder.
//!
//! Pure-Rust port of the DST frame path used by `libdstdec/` in
//! [Sound-Linux-More/sacd-extract][upstream]. See `INTEGRATION.md` in
//! this directory for the integration contract, fixture provenance,
//! and acceptance criteria.
//!
//! [upstream]: https://github.com/Sound-Linux-More/sacd-extract

#![forbid(unsafe_code)]

mod bitreader;
mod decoder;
mod tables;

pub use decoder::decode_frame;

/// DST decoder error. Every C `assert()` / failure path in the upstream
/// decoder maps to one of these variants — no panics on malformed input.
#[derive(Debug)]
pub enum DstError {
    /// Bit/byte reader ran past the end of the input frame.
    /// `consumed` is whole bytes consumed from `input` before exhaustion.
    UnexpectedEof { consumed: usize },
    /// Frame header or stream syntax violated the DST spec. Also used for
    /// invalid `channel_count` and for short output.
    MalformedFrame(&'static str),
    /// Decoder tried to emit more output than the fixed budget.
    /// `limit` is `channel_count * 4704`.
    OutputOverflow { limit: usize },
    /// Catch-all for upstream `return -1` paths that don't fit a more
    /// specific variant.
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

#[cfg(test)]
mod fixture_tests {
    use super::decode_frame;

    fn pair(n: u8) -> (&'static [u8], &'static [u8]) {
        match n {
            1 => (
                include_bytes!("fixtures/frame_001.dst.bin"),
                include_bytes!("fixtures/frame_001.dsd.bin"),
            ),
            2 => (
                include_bytes!("fixtures/frame_002.dst.bin"),
                include_bytes!("fixtures/frame_002.dsd.bin"),
            ),
            3 => (
                include_bytes!("fixtures/frame_003.dst.bin"),
                include_bytes!("fixtures/frame_003.dsd.bin"),
            ),
            _ => unreachable!(),
        }
    }

    #[test]
    fn frame_1_byte_exact() {
        let (inp, expect) = pair(1);
        let got = decode_frame(inp, 2).expect("decode");
        assert_eq!(got, expect);
    }

    #[test]
    fn frame_2_byte_exact() {
        let (inp, expect) = pair(2);
        let got = decode_frame(inp, 2).expect("decode");
        assert_eq!(got, expect);
    }

    #[test]
    fn frame_3_byte_exact() {
        let (inp, expect) = pair(3);
        let got = decode_frame(inp, 2).expect("decode");
        assert_eq!(got, expect);
    }

    #[test]
    fn invalid_channel_count_is_malformed() {
        let (inp, _) = pair(1);
        let err = decode_frame(inp, 5).expect_err("invalid channel count");
        assert!(matches!(err, super::DstError::MalformedFrame("invalid channel_count")));
    }
}
