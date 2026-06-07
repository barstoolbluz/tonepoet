//! DST (Direct Stream Transfer) decoder.
//!
//! Pure-Rust DST decoder/encoder support. The decoder keeps SACD ISO
//! extraction DSD64-specific at the caller boundary, but the decoder core is
//! parameterized for DSD64, DSD128, and DSD256 frame geometry and supports legal
//! channel counts 1 through 6. It parses the general DST segment and mapping
//! tables used by MPEG-4 DST rather than assuming the SACD common single-segment
//! layout.
//!
//! Validation: byte-exact parity with `sacd_extract` for Solo Monk
//! (uncompressed stereo), Al Jarreau *All I Got* DST stereo, and
//! Al Jarreau *All I Got* DST 6-channel (70/70 tracks, ~20.5 GB).
//!
//! [upstream]: https://github.com/Sound-Linux-More/sacd-extract

#![forbid(unsafe_code)]

mod bitreader;
mod decoder;
mod encoder;
mod tables;

pub use decoder::{
    decode_frame, decode_frame_into, decode_frame_with_rate, decode_frame_with_rate_into,
    DstDecoder, DstRate,
};
pub use encoder::{
    dst_interleaved_frame_len, dst_interleaved_frame_len_for_rate, dst_rate_from_sample_rate,
    encode_frame_interleaved, encode_frame_interleaved_with_rate,
    encode_frame_interleaved_with_rate_and_telemetry, encode_frame_interleaved_with_telemetry,
    encode_frames_interleaved_ordered,
    encode_predictive_frame_interleaved, encode_predictive_frame_interleaved_with_rate,
    encode_predictive_frame_interleaved_with_rate_and_telemetry,
    encode_predictive_frame_interleaved_with_telemetry,
    encode_uncompressed_frame_interleaved, encode_uncompressed_frame_interleaved_padded,
    encode_uncompressed_frame_interleaved_padded_with_rate,
    encode_uncompressed_frame_interleaved_with_rate,
    is_legal_dst_channel_count, supports_dst_policy, supports_predictive_dst_channel_count,
    supports_predictive_dst_layout, supports_raw_dst_fallback_channel_count,
    supports_raw_dst_fallback_layout, supports_verified_dst_channel_count, validate_dst_policy,
    DstEncodeError, DstEncodeFailureClass, DstEncoderEffort,
    DstEncoderOptions, DstFrameEncodeTelemetry, DstFrameEncoding, DstPolicyScope,
    DstSelectedPredictor, DstTableStrategy, DstVerificationFailureKind,
    DstVerificationFailurePolicy, EncodedDstFrame, RawDstFallbackPolicy,
};

/// DST decoder error. Every C `assert()` / failure path in the upstream
/// decoder maps to one of these variants — no panics on malformed input.
#[derive(Debug)]
pub enum DstError {
    /// Bit/byte reader ran past the end of the input frame.
    /// `consumed` is whole bytes consumed from `input` before exhaustion.
    UnexpectedEof { consumed: usize },
    /// Channel count is outside the legal DST range of 1 through 6.
    InvalidChannelCount { channel_count: u8 },
    /// DSD rate is not one of the legal DST frame rates.
    UnsupportedRate { sample_rate: u32 },
    /// Frame header or stream syntax violated the DST spec.
    MalformedFrame(&'static str),
    /// Segment table syntax or boundaries are invalid.
    InvalidSegment(&'static str),
    /// Filter/probability table mapping is invalid.
    InvalidMapping(&'static str),
    /// Probability table syntax or values are invalid.
    InvalidProbabilityTable(&'static str),
    /// Arithmetic-coded payload could not be decoded.
    InvalidArithmeticCode,
    /// Arithmetic decoder reached an impossible state.
    ArithmeticDecodeFailure(&'static str),
    /// Caller-supplied output buffer is smaller than the decoded frame geometry.
    OutputBufferTooSmall { required: usize, actual: usize },
    /// Decoder produced the wrong number of bytes.
    OutputSizeMismatch { expected: usize, actual: usize },
    /// Decoder tried to emit more output than the fixed decoded-frame budget.
    ///
    /// Public decode APIs pre-slice their output buffer to the exact DSD frame
    /// geometry before decoding, so valid calls cannot grow past the budget.
    /// This variant covers internal geometry-invariant violations and keeps the
    /// failure structured if a future sink decouples decode geometry from the
    /// caller-provided slice length.
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
            DstError::InvalidChannelCount { channel_count } => {
                write!(f, "invalid DST channel count {}; expected 1 through 6", channel_count)
            }
            DstError::UnsupportedRate { sample_rate } => {
                write!(f, "unsupported DST sample rate {}; expected DSD64, DSD128, or DSD256", sample_rate)
            }
            DstError::MalformedFrame(msg) => write!(f, "malformed DST frame: {}", msg),
            DstError::InvalidSegment(msg) => write!(f, "invalid DST segment table: {}", msg),
            DstError::InvalidMapping(msg) => write!(f, "invalid DST mapping table: {}", msg),
            DstError::InvalidProbabilityTable(msg) => write!(f, "invalid DST probability table: {}", msg),
            DstError::InvalidArithmeticCode => write!(f, "invalid DST arithmetic-code start bit"),
            DstError::ArithmeticDecodeFailure(msg) => write!(f, "DST arithmetic decode failure: {}", msg),
            DstError::OutputBufferTooSmall { required, actual } => {
                write!(f, "DST output buffer has {} bytes; required {}", actual, required)
            }
            DstError::OutputSizeMismatch { expected, actual } => {
                write!(f, "decoded DST output has {} bytes; expected {}", actual, expected)
            }
            DstError::OutputOverflow { limit } => {
                write!(f, "decoded output exceeded budget ({} bytes)", limit)
            }
            DstError::InternalDecodeError(msg) => write!(f, "internal decoder error: {}", msg),
        }
    }
}

impl std::error::Error for DstError {}

