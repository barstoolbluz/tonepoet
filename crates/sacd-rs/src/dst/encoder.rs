// SPDX-License-Identifier: GPL-2.0-or-later
//! DST frame encoder support.
//!
//! The encoder writes the same simple, SACD-oriented DST subset that this crate
//! decodes: one frame-length segment, shared segment layout, table maps over
//! channels, FIR prediction coefficients, probability tables, and arithmetic
//! coded residual bits. Every predictive frame produced by the public helper is
//! decode-verified by default and accepted only when it is smaller than raw DST
//! syntax.
//!
//! This is useful lossless DST syntax, not compression parity with SACD mastering
//! encoders. Compression ratio is material-dependent, raw `DSTCoded = 0` frames
//! remain explicit compatibility experiments, and broad external-player corpus
//! acceptance must be proven by an acceptance gate before UI or release copy
//! claims more.

use super::decoder::decode_frame;
use super::tables::{
    log2_floor_usize, prob_dst_x_bit, FRAME_BITS_PER_CHANNEL, FRAME_BYTES_PER_CHANNEL,
    MAX_CHANNELS, MAX_TABLE_LEN,
};
use std::fmt;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_PREDICTION_ORDER: usize = 16;
const DEFAULT_CANDIDATE_ORDERS: &[usize] = &[4, 8, 12, 16, 24, 32, 48, 64];
const DEFAULT_COEFFICIENT_SCALES: &[i32] = &[255, 192, 128, 96];
const DEFAULT_COEFFICIENT_PRUNE_THRESHOLDS: &[i32] = &[0, 1, 2];
const MIN_COMPRESSED_CHANNELS: u8 = 2;
const MAX_COMPRESSED_CHANNELS: u8 = 6;
const ARITHMETIC_BITS: u32 = 12;
const ARITHMETIC_ONE: u32 = 1 << ARITHMETIC_BITS;
const ARITHMETIC_HALF: u32 = ARITHMETIC_ONE >> 1;
const PROBABILITY_TABLE_LEN: usize = 64;
const MAX_PRED_COEFF_ABS: i32 = 255;
const PRESCREEN_SAMPLE_BYTES: usize = 4096;
const PRESCREEN_MIN_UNIQUE_BYTES: usize = 240;
const PRESCREEN_MIN_TRANSITION_PERCENT: u32 = 48;

/// Errors returned by DST frame encoding helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DstEncodeError {
    /// DST frame helpers support one to six DSD channels.
    InvalidChannelCount { channel_count: u8 },
    /// The caller supplied a frame whose byte count does not match one full
    /// DST frame for the declared channel count.
    InvalidFrameLength { expected: usize, actual: usize },
    /// Predictive DST coding supports prediction orders in 1..=128.
    InvalidPredictionOrder { prediction_order: usize },
    /// Predictive DST coding is intentionally implemented only for channel
    /// layouts exercised by this crate's decoder and validation corpus.
    PredictiveUnsupportedChannelCount { channel_count: u8 },
    /// A predictive candidate was valid but not smaller than raw DST syntax by
    /// the caller's required margin. In compatibility-safe mode, callers should
    /// write DSDIFF/DSD instead of emitting a raw DST fallback frame.
    CompressionNotBeneficial {
        predictive_len: usize,
        raw_len: usize,
        minimum_savings_bytes: usize,
    },
    /// A candidate compressed frame did not decode back to its source.
    VerificationFailed,
}

impl fmt::Display for DstEncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidChannelCount { channel_count } => {
                write!(
                    f,
                    "invalid DST channel count {}; expected 1..={}",
                    channel_count, MAX_CHANNELS
                )
            }
            Self::InvalidFrameLength { expected, actual } => {
                write!(f, "invalid DST frame length {}; expected {} bytes", actual, expected)
            }
            Self::InvalidPredictionOrder { prediction_order } => {
                write!(
                    f,
                    "invalid DST prediction order {}; expected 1..={}",
                    prediction_order, MAX_TABLE_LEN
                )
            }
            Self::PredictiveUnsupportedChannelCount { channel_count } => write!(
                f,
                "predictive DST generation for {} channel(s) is unavailable; legal source-DST can still be decoded or passed through, caller-supplied encoded DST can still be written, and raw fallback requires explicit opt-in",
                channel_count
            ),
            Self::CompressionNotBeneficial {
                predictive_len,
                raw_len,
                minimum_savings_bytes,
            } => write!(
                f,
                "predictive DST frame is not smaller enough for portable DSDIFF/DST output (predictive {} bytes, raw {} bytes, required savings {} bytes); write DSDIFF/DSD or explicitly enable raw DST fallback",
                predictive_len, raw_len, minimum_savings_bytes
            ),
            Self::VerificationFailed => write!(f, "predictive DST frame failed decode verification"),
        }
    }
}

impl std::error::Error for DstEncodeError {}

/// Stable, telemetry-friendly class for an encoder failure.
///
/// This intentionally avoids embedding byte counts or channel numbers so stats
/// can aggregate failure causes across a large SACD corpus without retaining a
/// full error object per attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DstEncodeFailureClass {
    /// Caller supplied an invalid channel count, frame length, prediction order,
    /// or another structurally invalid input.
    InvalidInput,
    /// Predictive DST coding is not implemented for the requested channel
    /// layout.
    PredictiveUnsupportedChannelCount,
    /// Predictive coding produced only candidates that were absent,
    /// pre-screened, larger than raw DST syntax, or smaller by less than the
    /// configured savings threshold.
    CompressionNotBeneficial,
    /// One or more predictive candidates failed exact decode verification and
    /// no verified candidate was accepted.
    VerificationFailed,
    /// The caller allowed raw fallback, but raw `DSTCoded = 0` frame generation
    /// itself failed.
    RawFallbackEncodingFailed,
}

impl DstEncodeError {
    /// Return the stable telemetry class for this concrete error.
    pub fn failure_class(&self) -> DstEncodeFailureClass {
        match self {
            Self::InvalidChannelCount { .. }
            | Self::InvalidFrameLength { .. }
            | Self::InvalidPredictionOrder { .. } => DstEncodeFailureClass::InvalidInput,
            Self::PredictiveUnsupportedChannelCount { .. } => {
                DstEncodeFailureClass::PredictiveUnsupportedChannelCount
            }
            Self::CompressionNotBeneficial { .. } => DstEncodeFailureClass::CompressionNotBeneficial,
            Self::VerificationFailed => DstEncodeFailureClass::VerificationFailed,
        }
    }
}

/// Exact class of a predictive-candidate verification failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DstVerificationFailureKind {
    /// The decoder rejected the candidate bitstream syntactically or
    /// structurally.
    DecodeError,
    /// The decoder accepted the candidate, but the decoded DSD bytes did not
    /// exactly match the source frame.
    DecodedDsdMismatch,
}

/// Raw-DST fallback policy for [`encode_frame_interleaved`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawDstFallbackPolicy {
    /// Do not emit `DSTCoded = 0` frames implicitly. This is the production
    /// default until raw DST frame portability has been proven against common
    /// decoders such as FFmpeg-based players.
    Disabled,
    /// Permit `DSTCoded = 0` raw fallback frames when predictive coding is
    /// unavailable or not beneficial. Verification failures are governed by
    /// [`DstVerificationFailurePolicy`] so strict callers can reject them even
    /// when raw fallback is enabled.
    Enabled,
}

/// Policy for predictive candidates that fail exact decode verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DstVerificationFailurePolicy {
    /// Treat verification failure as terminal. This is the default: a failed
    /// predictive candidate means the encoder implementation or model search is
    /// suspect, and raw fallback must not hide that fact.
    Fail,
    /// Permit raw `DSTCoded = 0` fallback after verification failure, but only
    /// when [`DstEncoderOptions::raw_fallback`] is also enabled. This is for
    /// controlled compatibility experiments, not archival defaults.
    AllowRawFallback,
}

/// Encoder effort preset. The effort level controls default candidate breadth
/// and whether fast pre-screening may skip frames that look statistically
/// hostile to predictive DST coding. Explicit candidate vectors on
/// [`DstEncoderOptions`] still take precedence where supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DstEncoderEffort {
    /// Minimal search for interactive extraction. Verification remains exact.
    Fast,
    /// Production default: conservative pre-screening and moderate search.
    Balanced,
    /// Wider search for archival corpus work. No pre-screen skip by default.
    HighCompression,
}

/// Encoder knobs for predictive DST frame writing.
#[derive(Debug, Clone)]
pub struct DstEncoderOptions {
    /// Compatibility field and always-included FIR order. Valid range: 1..=128.
    ///
    /// The encoder no longer commits to this single order. It is inserted into
    /// `candidate_prediction_orders`, so older callers that tuned this field
    /// still influence the search.
    pub prediction_order: usize,
    /// Try one independent FIR/probability table per channel as well as a
    /// shared table. When false, only the shared-table candidate is attempted.
    ///
    /// This keeps the original API shape while allowing the default encoder to
    /// perform shared-vs-per-channel model selection.
    pub per_channel_filters: bool,
    /// Candidate FIR orders for Levinson-Durbin fitting. Empty means
    /// `prediction_order` only. Defaults cover short, medium, and SACD-sized
    /// predictor tables while respecting the DST 128-tap table limit.
    pub candidate_prediction_orders: Vec<usize>,
    /// Quantization scales applied to floating-point LPC coefficients before
    /// writing signed 9-bit DST FIR coefficients. Smaller scales can reduce
    /// entropy-model overconfidence and sometimes improve coded size.
    pub coefficient_quantization_scales: Vec<i32>,
    /// Absolute-value thresholds applied after quantization. Thresholded tail
    /// taps become zero and trailing zeros are removed, so this directly
    /// participates in table-length optimization.
    pub coefficient_prune_thresholds: Vec<i32>,
    /// Decode predictive candidates in memory and require exact equality before
    /// accepting them. This should remain enabled for production writing.
    pub verify: bool,
    /// Accept predictive coding only when it saves at least this many bytes
    /// relative to raw DST frame syntax.
    pub minimum_savings_bytes: usize,
    /// Encoder effort preset. This separates speed policy from the DST syntax
    /// mechanism. Existing explicit candidate vectors are honored, while the
    /// preset supplies sane defaults and pre-screen behavior for extraction
    /// presets.
    pub effort: DstEncoderEffort,
    /// Enable fast frame pre-screening. In `Fast` mode, frames that look
    /// statistically hostile to predictive coding are rejected before the FIR
    /// candidate matrix is built. `Balanced` keeps at least one small candidate
    /// search unless the caller explicitly disables predictive coding by policy.
    pub fast_prescreen: bool,
    /// Optional worker count for batch helpers. A value of 0 or 1 encodes
    /// serially. Streaming writers preserve output order and remain serial; use
    /// [`encode_frames_interleaved_ordered`] when the caller can batch frames.
    pub parallel_workers: usize,
    /// Whether implicit raw DST fallback is allowed. Disabled by default for
    /// portable output. Explicit raw helpers are still available for callers who
    /// knowingly want `DSTCoded = 0` test vectors.
    pub raw_fallback: RawDstFallbackPolicy,
    /// Whether verification failures may be downgraded to raw fallback when raw
    /// fallback is otherwise enabled. Defaults to [`DstVerificationFailurePolicy::Fail`]
    /// so exact verification failures remain visible in strict production paths.
    pub verification_failure_policy: DstVerificationFailurePolicy,
}

impl Default for DstEncoderOptions {
    fn default() -> Self {
        Self {
            prediction_order: DEFAULT_PREDICTION_ORDER,
            per_channel_filters: true,
            candidate_prediction_orders: DEFAULT_CANDIDATE_ORDERS.to_vec(),
            coefficient_quantization_scales: DEFAULT_COEFFICIENT_SCALES.to_vec(),
            coefficient_prune_thresholds: DEFAULT_COEFFICIENT_PRUNE_THRESHOLDS.to_vec(),
            verify: true,
            minimum_savings_bytes: 1,
            effort: DstEncoderEffort::Balanced,
            fast_prescreen: true,
            parallel_workers: 0,
            raw_fallback: RawDstFallbackPolicy::Disabled,
            verification_failure_policy: DstVerificationFailurePolicy::Fail,
        }
    }
}

/// The representation selected for a DST frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DstFrameEncoding {
    /// DSTCoded=1: FIR-predicted, arithmetic-coded residuals.
    Predictive,
    /// DSTCoded=0: byte-aligned raw DSD payload inside DST syntax.
    Uncompressed,
}

/// Encoded DST frame plus the coding mode selected by the encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedDstFrame {
    /// Raw DST frame payload to store in a DSDIFF `DSTF` chunk.
    pub bytes: Vec<u8>,
    /// Coding mode used for `bytes`.
    pub encoding: DstFrameEncoding,
}


/// Public table-map strategy chosen for a predictive DST frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DstTableStrategy {
    /// One FIR/probability table is shared by all channels.
    Shared,
    /// Each channel gets its own FIR/probability table.
    PerChannel,
}

impl DstTableStrategy {
    fn table_for_channel(self, channels: usize) -> Vec<usize> {
        match self {
            Self::Shared => vec![0; channels],
            Self::PerChannel => (0..channels).collect(),
        }
    }

    fn uses_per_channel_tables(self) -> bool {
        matches!(self, Self::PerChannel)
    }
}

/// Predictor metadata for the selected verified predictive candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DstSelectedPredictor {
    /// FIR order requested before trailing-zero table-length reduction.
    pub prediction_order: usize,
    /// Shared-table or per-channel table strategy.
    pub table_strategy: DstTableStrategy,
    /// Quantization scale applied to the floating-point LPC coefficients.
    pub coefficient_scale: i32,
    /// Absolute coefficient value at or below which a tap was pruned.
    pub coefficient_prune_threshold: i32,
    /// Actual emitted FIR table lengths after pruning trailing zero taps.
    pub filter_table_lengths: Vec<usize>,
}

/// Per-frame telemetry produced by the DST encoder.
///
/// The encoder is lossless, so frame acceptance is governed by exact decode
/// verification and size. `compression_ratio()` reports `raw_dst_bytes /
/// encoded_bytes`; values greater than 1.0 mean the selected DST frame is
/// smaller than raw `DSTCoded = 0` syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DstFrameEncodeTelemetry {
    /// Interleaved DSD bytes supplied by the caller.
    pub input_raw_bytes: usize,
    /// Bytes that a raw `DSTCoded = 0` frame would occupy.
    pub raw_dst_bytes: usize,
    /// Bytes in the selected encoded frame, or zero when no frame was accepted.
    pub encoded_bytes: usize,
    /// Final selected representation. `None` means the encode attempt failed
    /// and the caller should use DSDIFF/DSD or another compatibility-safe path.
    pub selected_encoding: Option<DstFrameEncoding>,
    /// Predictor metadata for a selected predictive frame.
    pub selected_predictor: Option<DstSelectedPredictor>,
    /// Number of predictive candidates actually materialized.
    pub predictive_candidates: u64,
    /// Number of predictive candidates that decode-verified exactly.
    pub verified_predictive_candidates: u64,
    /// Number of predictive candidates rejected by decode verification.
    pub verification_failures: u64,
    /// Predictive candidates for which the verifier's decoder returned an error.
    pub verification_decode_errors: u64,
    /// Predictive candidates that decoded but did not byte-match the source DSD.
    pub verification_mismatches: u64,
    /// Last verification failure kind observed for this frame, if any.
    pub last_verification_failure: Option<DstVerificationFailureKind>,
    /// Terminal failure class for this encode attempt when no frame was selected.
    pub terminal_error: Option<DstEncodeFailureClass>,
    /// Failure class that caused raw fallback to be selected, if raw fallback was
    /// explicitly enabled and used.
    pub raw_fallback_reason: Option<DstEncodeFailureClass>,
    /// Verified predictive candidates that did not satisfy the configured
    /// savings threshold against raw DST syntax.
    pub unprofitable_predictive_candidates: u64,
    /// Largest byte expansion avoided by not selecting a worse predictive
    /// candidate. For raw-fallback modes this also captures the expansion that
    /// raw fallback avoided.
    pub worst_expansion_avoided_bytes: usize,
    /// True when the fast pre-screen rejected the frame before the candidate
    /// matrix was materialized.
    pub prescreen_rejected: bool,
    /// Number of byte positions sampled by the pre-screen.
    pub prescreen_sample_bytes: usize,
    /// Distinct byte values seen in the pre-screen window.
    pub prescreen_unique_bytes: usize,
    /// Approximate adjacent bit-transition percentage in the sampled window.
    pub prescreen_transition_percent: u32,
    /// Wall-clock time spent in the encoder for this frame.
    pub encode_time: Duration,
}

impl DstFrameEncodeTelemetry {
    fn new(input_raw_bytes: usize) -> Self {
        Self {
            input_raw_bytes,
            raw_dst_bytes: input_raw_bytes.saturating_add(1),
            encoded_bytes: 0,
            selected_encoding: None,
            selected_predictor: None,
            predictive_candidates: 0,
            verified_predictive_candidates: 0,
            verification_failures: 0,
            verification_decode_errors: 0,
            verification_mismatches: 0,
            last_verification_failure: None,
            terminal_error: None,
            raw_fallback_reason: None,
            unprofitable_predictive_candidates: 0,
            worst_expansion_avoided_bytes: 0,
            prescreen_rejected: false,
            prescreen_sample_bytes: 0,
            prescreen_unique_bytes: 0,
            prescreen_transition_percent: 0,
            encode_time: Duration::from_nanos(0),
        }
    }

    fn finish(mut self, started_at: Instant) -> Self {
        self.encode_time = started_at.elapsed();
        self
    }

    /// Raw-DST bytes divided by accepted encoded bytes. Values greater than
    /// 1.0 indicate compression relative to raw DST syntax.
    pub fn compression_ratio(&self) -> Option<f64> {
        if self.encoded_bytes == 0 {
            None
        } else {
            Some(self.raw_dst_bytes as f64 / self.encoded_bytes as f64)
        }
    }
}

/// Number of interleaved DSD bytes in a full DST frame for `channel_count`.
///
/// DST/SACD frames carry 37,632 one-bit samples per channel at DSD64, i.e.
/// 4,704 bytes per channel. The byte layout expected here is the same clustered
/// MSB-first interleaving used by SACD sectors and DSDIFF/DSD payloads:
/// `ch0_byte0, ch1_byte0, ..., chN_byte0, ch0_byte1, ...`.
pub fn dst_interleaved_frame_len(channel_count: u8) -> Result<usize, DstEncodeError> {
    validate_channel_count(channel_count)?;
    Ok(FRAME_BYTES_PER_CHANNEL * usize::from(channel_count))
}

/// Returns true for legal DST container/read/decode/passthrough channel counts.
///
/// DST syntax and the in-tree decoder support channel counts 1 through 6. This
/// is intentionally broader than predictive generation support: a legal
/// source-DST frame may be decoded or passed through even when this crate cannot
/// yet prove newly generated predictive DST for that layout.
pub fn is_legal_dst_channel_count(channel_count: u8) -> bool {
    (1..=MAX_COMPRESSED_CHANNELS).contains(&channel_count)
}

/// Returns true for channel counts accepted by explicit raw `DSTCoded = 0`
/// fallback helpers.
///
/// Raw fallback is never implicit. It is an explicit compatibility/test mode,
/// currently limited to the same legal 1-through-6 DST channel-count range as
/// container validation and decode support.
pub fn supports_raw_dst_fallback_channel_count(channel_count: u8) -> bool {
    is_legal_dst_channel_count(channel_count)
}

/// Returns true for channel counts this crate can generate as verified
/// predictive DST.
///
/// Predictive generation is intentionally narrower than container/decode/
/// passthrough support. The current in-tree predictive encoder is treated as
/// verified only for stereo and six-channel layouts.
pub fn supports_predictive_dst_channel_count(channel_count: u8) -> bool {
    supports_predictive_channel_count(channel_count)
}

/// Compatibility alias for callers that previously used this predicate for
/// generated DSDIFF/DST output. It means verified predictive generation support,
/// not legal container/read/decode/passthrough support.
pub fn supports_verified_dst_channel_count(channel_count: u8) -> bool {
    supports_predictive_dst_channel_count(channel_count)
}

/// Encode one full interleaved DSD frame using predictive DST coding when it is
/// profitable and verified.
///
/// By default this function does **not** fall back to raw `DSTCoded = 0` frames,
/// because raw DST-frame interoperability has not been proven against common
/// external decoders. If predictive compression is unavailable, not smaller by
/// `minimum_savings_bytes`, or fails verification, the function returns an
/// error so the caller can write DSDIFF/DSD instead. Set
/// [`DstEncoderOptions::raw_fallback`] to [`RawDstFallbackPolicy::Enabled`] only
/// for explicit compatibility testing or for a controlled decoder set known to
/// accept raw DST frames. Verification failures additionally require
/// [`DstVerificationFailurePolicy::AllowRawFallback`] before they can be
/// downgraded to raw output.
pub fn encode_frame_interleaved(
    interleaved_dsd: &[u8],
    channel_count: u8,
    options: &DstEncoderOptions,
) -> Result<EncodedDstFrame, DstEncodeError> {
    encode_frame_interleaved_with_telemetry(interleaved_dsd, channel_count, options).0
}

/// Encode one full interleaved DSD frame and return telemetry even when the
/// frame is rejected.
pub fn encode_frame_interleaved_with_telemetry(
    interleaved_dsd: &[u8],
    channel_count: u8,
    options: &DstEncoderOptions,
) -> (Result<EncodedDstFrame, DstEncodeError>, DstFrameEncodeTelemetry) {
    let started_at = Instant::now();
    let mut telemetry = DstFrameEncodeTelemetry::new(interleaved_dsd.len());

    let expected = match dst_interleaved_frame_len(channel_count) {
        Ok(expected) => expected,
        Err(err) => return finish_encode_error(err, telemetry, started_at),
    };
    if interleaved_dsd.len() != expected {
        return finish_encode_error(
            DstEncodeError::InvalidFrameLength {
                expected,
                actual: interleaved_dsd.len(),
            },
            telemetry,
            started_at,
        );
    }
    if let Err(err) = validate_prediction_order(options.prediction_order) {
        return finish_encode_error(err, telemetry, started_at);
    }

    let raw_len = telemetry.raw_dst_bytes;

    if !supports_predictive_channel_count(channel_count) {
        return raw_fallback_or_error_with_telemetry(
            interleaved_dsd,
            channel_count,
            options,
            telemetry,
            started_at,
            DstEncodeError::PredictiveUnsupportedChannelCount { channel_count },
        );
    }

    match encode_predictive_search(interleaved_dsd, channel_count, options, &mut telemetry) {
        Ok(candidate) if is_worth_using(candidate.bytes.len(), raw_len, options.minimum_savings_bytes) => {
            telemetry.encoded_bytes = candidate.bytes.len();
            telemetry.selected_encoding = Some(DstFrameEncoding::Predictive);
            telemetry.selected_predictor = Some(candidate.predictor);
            finish_encode_result(
                Ok(EncodedDstFrame {
                    bytes: candidate.bytes,
                    encoding: DstFrameEncoding::Predictive,
                }),
                telemetry,
                started_at,
            )
        }
        Ok(candidate) => {
            let err = DstEncodeError::CompressionNotBeneficial {
                predictive_len: candidate.bytes.len(),
                raw_len,
                minimum_savings_bytes: options.minimum_savings_bytes,
            };
            raw_fallback_or_error_with_telemetry(
                interleaved_dsd,
                channel_count,
                options,
                telemetry,
                started_at,
                err,
            )
        }
        Err(err) => raw_fallback_or_error_with_telemetry(
            interleaved_dsd,
            channel_count,
            options,
            telemetry,
            started_at,
            err,
        ),
    }
}


/// Encode a batch of independent full DST frames while preserving result order.
///
/// This helper gives callers an optional parallel path for large-disc workflows
/// that can buffer frames before writing. The streaming DSDIFF/DST writer stays
/// serial so it can maintain ordered `DSTF`/`DSTC`/`DSTI` emission without an
/// internal unbounded queue. `options.parallel_workers` is used when `workers`
/// is zero.
pub fn encode_frames_interleaved_ordered(
    frames: &[Vec<u8>],
    channel_count: u8,
    options: &DstEncoderOptions,
    workers: usize,
) -> Vec<(Result<EncodedDstFrame, DstEncodeError>, DstFrameEncodeTelemetry)> {
    let worker_count = workers.max(options.parallel_workers).max(1).min(frames.len().max(1));
    if worker_count <= 1 || frames.len() <= 1 {
        return frames
            .iter()
            .map(|frame| encode_frame_interleaved_with_telemetry(frame, channel_count, options))
            .collect();
    }

    let (tx, rx) = mpsc::channel();
    let mut ordered: Vec<Option<(Result<EncodedDstFrame, DstEncodeError>, DstFrameEncodeTelemetry)>> =
        (0..frames.len()).map(|_| None).collect();

    thread::scope(|scope| {
        for worker in 0..worker_count {
            let tx = tx.clone();
            scope.spawn(move || {
                let mut index = worker;
                while index < frames.len() {
                    let result = encode_frame_interleaved_with_telemetry(
                        &frames[index],
                        channel_count,
                        options,
                    );
                    if tx.send((index, result)).is_err() {
                        return;
                    }
                    index += worker_count;
                }
            });
        }
        drop(tx);
        for (index, result) in rx {
            ordered[index] = Some(result);
        }
    });
    ordered
        .into_iter()
        .map(|entry| entry.expect("DST batch worker did not return a frame result"))
        .collect()
}

/// Encode one full interleaved DSD frame as the smallest verified predictive DST frame.
///
/// The encoder evaluates a matrix of lossless predictive candidates: multiple
/// FIR orders, Levinson-Durbin coefficient fits, quantization scales,
/// coefficient-pruning thresholds, and shared/per-channel table maps. Each
/// candidate gets its own probability table and arithmetic residual stream. The
/// smallest candidate that decode-verifies exactly is returned.
pub fn encode_predictive_frame_interleaved(
    interleaved_dsd: &[u8],
    channel_count: u8,
    options: &DstEncoderOptions,
) -> Result<Vec<u8>, DstEncodeError> {
    encode_predictive_frame_interleaved_with_telemetry(interleaved_dsd, channel_count, options).0
}

/// Encode one full interleaved DSD frame predictively and return candidate-search
/// telemetry. This helper does not apply the raw-fallback policy; callers that
/// need production selection should use [`encode_frame_interleaved_with_telemetry`].
pub fn encode_predictive_frame_interleaved_with_telemetry(
    interleaved_dsd: &[u8],
    channel_count: u8,
    options: &DstEncoderOptions,
) -> (Result<Vec<u8>, DstEncodeError>, DstFrameEncodeTelemetry) {
    let started_at = Instant::now();
    let mut telemetry = DstFrameEncodeTelemetry::new(interleaved_dsd.len());

    let expected = match dst_interleaved_frame_len(channel_count) {
        Ok(expected) => expected,
        Err(err) => return finish_predictive_error(err, telemetry, started_at),
    };
    if interleaved_dsd.len() != expected {
        return finish_predictive_error(
            DstEncodeError::InvalidFrameLength {
                expected,
                actual: interleaved_dsd.len(),
            },
            telemetry,
            started_at,
        );
    }
    if !supports_predictive_channel_count(channel_count) {
        return finish_predictive_error(
            DstEncodeError::PredictiveUnsupportedChannelCount { channel_count },
            telemetry,
            started_at,
        );
    }

    match encode_predictive_search(interleaved_dsd, channel_count, options, &mut telemetry) {
        Ok(candidate) => {
            telemetry.encoded_bytes = candidate.bytes.len();
            telemetry.selected_encoding = Some(DstFrameEncoding::Predictive);
            telemetry.selected_predictor = Some(candidate.predictor);
            finish_predictive_result(Ok(candidate.bytes), telemetry, started_at)
        }
        Err(err) => finish_predictive_error(err, telemetry, started_at),
    }
}

struct PredictiveCandidate {
    bytes: Vec<u8>,
    predictor: DstSelectedPredictor,
}

fn encode_predictive_search(
    interleaved_dsd: &[u8],
    channel_count: u8,
    options: &DstEncoderOptions,
    telemetry: &mut DstFrameEncodeTelemetry,
) -> Result<PredictiveCandidate, DstEncodeError> {
    let expected = dst_interleaved_frame_len(channel_count)?;
    if interleaved_dsd.len() != expected {
        return Err(DstEncodeError::InvalidFrameLength {
            expected,
            actual: interleaved_dsd.len(),
        });
    }
    if !supports_predictive_channel_count(channel_count) {
        return Err(DstEncodeError::PredictiveUnsupportedChannelCount { channel_count });
    }

    if let Some(prescreen) = fast_prescreen(interleaved_dsd, options) {
        telemetry.prescreen_sample_bytes = prescreen.sample_bytes;
        telemetry.prescreen_unique_bytes = prescreen.unique_bytes;
        telemetry.prescreen_transition_percent = prescreen.transition_percent;
        if prescreen.reject_predictive {
            telemetry.prescreen_rejected = true;
            return Err(DstEncodeError::CompressionNotBeneficial {
                predictive_len: 0,
                raw_len: 1 + interleaved_dsd.len(),
                minimum_savings_bytes: options.minimum_savings_bytes,
            });
        }
    }

    let channels = usize::from(channel_count);
    let orders = candidate_orders(options)?;
    let scales = candidate_scales(options);
    let prune_thresholds = candidate_prune_thresholds(options);
    let max_order = orders.iter().copied().max().unwrap_or(DEFAULT_PREDICTION_ORDER);
    let channel_autocorr = channel_autocorrelations(interleaved_dsd, channels, max_order);
    let shared_autocorr = shared_autocorrelation(&channel_autocorr);

    let mut best: Option<PredictiveCandidate> = None;
    let mut saw_unverified_candidate = false;

    for layout in candidate_layouts(options, channels) {
        for &order in &orders {
            let lpc_by_table: Vec<Vec<f64>> = match layout {
                DstTableStrategy::Shared => vec![levinson_durbin(&shared_autocorr[..=order], order)],
                DstTableStrategy::PerChannel => (0..channels)
                    .map(|ch| levinson_durbin(&channel_autocorr[ch][..=order], order))
                    .collect(),
            };

            for &scale in &scales {
                for &prune_threshold in &prune_thresholds {
                    let filters = lpc_by_table
                        .iter()
                        .map(|coeffs| quantize_lpc_coefficients(coeffs, scale, prune_threshold))
                        .collect::<Vec<_>>();
                    if filters.iter().any(Vec::is_empty) {
                        continue;
                    }

                    let table_for_channel = layout.table_for_channel(channels);
                    let candidate = encode_predictive_candidate(
                        interleaved_dsd,
                        channels,
                        &filters,
                        &table_for_channel,
                        layout,
                    );
                    telemetry.predictive_candidates = telemetry.predictive_candidates.saturating_add(1);

                    if options.verify {
                        match verify_predictive_candidate(&candidate, channel_count, interleaved_dsd) {
                            Ok(()) => {}
                            Err(kind) => {
                                saw_unverified_candidate = true;
                                record_verification_failure(telemetry, kind);
                                continue;
                            }
                        }
                    }

                    telemetry.verified_predictive_candidates = telemetry
                        .verified_predictive_candidates
                        .saturating_add(1);
                    if !is_worth_using(
                        candidate.len(),
                        telemetry.raw_dst_bytes,
                        options.minimum_savings_bytes,
                    ) {
                        telemetry.unprofitable_predictive_candidates = telemetry
                            .unprofitable_predictive_candidates
                            .saturating_add(1);
                    }
                    if candidate.len() > telemetry.raw_dst_bytes {
                        telemetry.worst_expansion_avoided_bytes = telemetry
                            .worst_expansion_avoided_bytes
                            .max(candidate.len() - telemetry.raw_dst_bytes);
                    }

                    let predictor = DstSelectedPredictor {
                        prediction_order: order,
                        table_strategy: layout,
                        coefficient_scale: scale,
                        coefficient_prune_threshold: prune_threshold,
                        filter_table_lengths: filters.iter().map(Vec::len).collect(),
                    };
                    let candidate = PredictiveCandidate { bytes: candidate, predictor };

                    if best
                        .as_ref()
                        .map_or(true, |current| candidate.bytes.len() < current.bytes.len())
                    {
                        best = Some(candidate);
                    }
                }
            }
        }
    }

    match best {
        Some(frame) => Ok(frame),
        None if saw_unverified_candidate => Err(DstEncodeError::VerificationFailed),
        None => Err(DstEncodeError::CompressionNotBeneficial {
            predictive_len: 0,
            raw_len: 1 + interleaved_dsd.len(),
            minimum_savings_bytes: options.minimum_savings_bytes,
        }),
    }
}

fn verify_predictive_candidate(
    candidate: &[u8],
    channel_count: u8,
    source: &[u8],
) -> Result<(), DstVerificationFailureKind> {
    match decode_frame(candidate, channel_count) {
        Ok(decoded) if decoded.as_slice() == source => Ok(()),
        Ok(_) => Err(DstVerificationFailureKind::DecodedDsdMismatch),
        Err(_) => Err(DstVerificationFailureKind::DecodeError),
    }
}

fn record_verification_failure(
    telemetry: &mut DstFrameEncodeTelemetry,
    kind: DstVerificationFailureKind,
) {
    telemetry.verification_failures = telemetry.verification_failures.saturating_add(1);
    telemetry.last_verification_failure = Some(kind);
    match kind {
        DstVerificationFailureKind::DecodeError => {
            telemetry.verification_decode_errors = telemetry.verification_decode_errors.saturating_add(1);
        }
        DstVerificationFailureKind::DecodedDsdMismatch => {
            telemetry.verification_mismatches = telemetry.verification_mismatches.saturating_add(1);
        }
    }
}

/// Encode one full interleaved DSD frame as a valid uncompressed DST frame.
///
/// The emitted bitstream is:
///
/// ```text
/// DSTCoded = 0
/// DstXbits = 0
/// Reserved = 000000
/// raw interleaved DSD bytes
/// ```
///
/// This raw form is useful for internal round-trip tests and for controlled
/// decoder compatibility experiments. It is **not** selected implicitly by the
/// default encoder path because common-player portability is not yet proven.
pub fn encode_uncompressed_frame_interleaved(
    interleaved_dsd: &[u8],
    channel_count: u8,
) -> Result<Vec<u8>, DstEncodeError> {
    let expected = dst_interleaved_frame_len(channel_count)?;
    if interleaved_dsd.len() != expected {
        return Err(DstEncodeError::InvalidFrameLength {
            expected,
            actual: interleaved_dsd.len(),
        });
    }

    let mut out = Vec::with_capacity(1 + interleaved_dsd.len());
    // Eight header bits: DSTCoded=0, DstXbits=0, six reserved zero bits.
    // Because the header is exactly one byte, the following DSD payload is
    // byte-aligned and the decoder's raw byte path can copy it verbatim.
    out.push(0);
    out.extend_from_slice(interleaved_dsd);
    Ok(out)
}

/// Encode a possibly short final DSD frame by zero-padding it to a full DST
/// frame first. Returns `(encoded_frame, padded_interleaved_source)` so callers
/// can compute DSTC over the exact DSD bits represented by the frame.
///
/// DSDIFF/DST records frame count, not an arbitrary final sample count. Padding
/// therefore becomes audible duration unless the caller stores an external edit
/// list or trims downstream. This helper emits explicit raw DST syntax and is
/// intended for controlled compatibility tests, not default production output.
pub fn encode_uncompressed_frame_interleaved_padded(
    interleaved_dsd: &[u8],
    channel_count: u8,
) -> Result<(Vec<u8>, Vec<u8>), DstEncodeError> {
    let expected = dst_interleaved_frame_len(channel_count)?;
    if interleaved_dsd.len() > expected {
        return Err(DstEncodeError::InvalidFrameLength {
            expected,
            actual: interleaved_dsd.len(),
        });
    }

    let mut padded = vec![0u8; expected];
    padded[..interleaved_dsd.len()].copy_from_slice(interleaved_dsd);
    let encoded = encode_uncompressed_frame_interleaved(&padded, channel_count)?;
    Ok((encoded, padded))
}


fn raw_fallback_or_error_with_telemetry(
    interleaved_dsd: &[u8],
    channel_count: u8,
    options: &DstEncoderOptions,
    mut telemetry: DstFrameEncodeTelemetry,
    started_at: Instant,
    err: DstEncodeError,
) -> (Result<EncodedDstFrame, DstEncodeError>, DstFrameEncodeTelemetry) {
    let failure_class = err.failure_class();
    let verification_failure = failure_class == DstEncodeFailureClass::VerificationFailed;
    let raw_fallback_allowed = options.raw_fallback == RawDstFallbackPolicy::Enabled
        && (!verification_failure
            || options.verification_failure_policy == DstVerificationFailurePolicy::AllowRawFallback);

    if raw_fallback_allowed {
        match encode_uncompressed_frame_interleaved(interleaved_dsd, channel_count) {
            Ok(bytes) => {
                telemetry.encoded_bytes = bytes.len();
                telemetry.selected_encoding = Some(DstFrameEncoding::Uncompressed);
                telemetry.raw_fallback_reason = Some(failure_class);
                finish_encode_result(
                    Ok(EncodedDstFrame {
                        bytes,
                        encoding: DstFrameEncoding::Uncompressed,
                    }),
                    telemetry,
                    started_at,
                )
            }
            Err(raw_err) => {
                telemetry.terminal_error = Some(DstEncodeFailureClass::RawFallbackEncodingFailed);
                finish_encode_result(Err(raw_err), telemetry, started_at)
            }
        }
    } else {
        telemetry.terminal_error = Some(failure_class);
        finish_encode_result(Err(err), telemetry, started_at)
    }
}

fn finish_encode_error(
    err: DstEncodeError,
    mut telemetry: DstFrameEncodeTelemetry,
    started_at: Instant,
) -> (Result<EncodedDstFrame, DstEncodeError>, DstFrameEncodeTelemetry) {
    telemetry.terminal_error = Some(err.failure_class());
    finish_encode_result(Err(err), telemetry, started_at)
}

fn finish_encode_result(
    result: Result<EncodedDstFrame, DstEncodeError>,
    telemetry: DstFrameEncodeTelemetry,
    started_at: Instant,
) -> (Result<EncodedDstFrame, DstEncodeError>, DstFrameEncodeTelemetry) {
    (result, telemetry.finish(started_at))
}

fn finish_predictive_error(
    err: DstEncodeError,
    mut telemetry: DstFrameEncodeTelemetry,
    started_at: Instant,
) -> (Result<Vec<u8>, DstEncodeError>, DstFrameEncodeTelemetry) {
    telemetry.terminal_error = Some(err.failure_class());
    finish_predictive_result(Err(err), telemetry, started_at)
}

fn finish_predictive_result(
    result: Result<Vec<u8>, DstEncodeError>,
    telemetry: DstFrameEncodeTelemetry,
    started_at: Instant,
) -> (Result<Vec<u8>, DstEncodeError>, DstFrameEncodeTelemetry) {
    (result, telemetry.finish(started_at))
}

fn validate_channel_count(channel_count: u8) -> Result<(), DstEncodeError> {
    if is_legal_dst_channel_count(channel_count) {
        Ok(())
    } else {
        Err(DstEncodeError::InvalidChannelCount { channel_count })
    }
}

fn validate_prediction_order(prediction_order: usize) -> Result<(), DstEncodeError> {
    if (1..=MAX_TABLE_LEN).contains(&prediction_order) {
        Ok(())
    } else {
        Err(DstEncodeError::InvalidPredictionOrder { prediction_order })
    }
}

fn supports_predictive_channel_count(channel_count: u8) -> bool {
    channel_count == MIN_COMPRESSED_CHANNELS || channel_count == MAX_COMPRESSED_CHANNELS
}

fn is_worth_using(candidate_len: usize, raw_len: usize, minimum_savings: usize) -> bool {
    candidate_len
        .checked_add(minimum_savings)
        .map_or(false, |threshold| threshold <= raw_len)
}

fn write_segment_header(writer: &mut BitWriter) {
    writer.write_bit(1); // PSameSegAsF.
    writer.write_bit(1); // SameSegAllCh for filter segmentation.
    writer.write_bit(1); // One segment ending at frame end.
}

fn write_table_map(writer: &mut BitWriter, channels: usize, per_channel: bool) {
    if !per_channel || channels == 1 {
        writer.write_bit(1); // SameMapAllCh.
        return;
    }

    writer.write_bit(0); // Explicit channel map.
    let mut existing_tables = 1usize;
    for _ch in 1..channels {
        let bits = log2_floor_usize(existing_tables) + 1;
        writer.write_bits(existing_tables as u32, bits);
        existing_tables += 1;
    }
}

fn candidate_layouts(options: &DstEncoderOptions, channels: usize) -> Vec<DstTableStrategy> {
    if options.per_channel_filters && channels > 1 {
        vec![DstTableStrategy::Shared, DstTableStrategy::PerChannel]
    } else {
        vec![DstTableStrategy::Shared]
    }
}

fn candidate_orders(options: &DstEncoderOptions) -> Result<Vec<usize>, DstEncodeError> {
    let mut orders = if options.candidate_prediction_orders.is_empty() {
        match options.effort {
            DstEncoderEffort::Fast => vec![8, 16],
            DstEncoderEffort::Balanced => DEFAULT_CANDIDATE_ORDERS.to_vec(),
            DstEncoderEffort::HighCompression => vec![4, 8, 12, 16, 24, 32, 48, 64, 96, 128],
        }
    } else {
        options.candidate_prediction_orders.clone()
    };
    orders.push(options.prediction_order);
    orders.sort_unstable();
    orders.dedup();
    for &order in &orders {
        validate_prediction_order(order)?;
    }
    Ok(orders)
}

fn candidate_scales(options: &DstEncoderOptions) -> Vec<i32> {
    let mut scales = if options.coefficient_quantization_scales.is_empty() {
        match options.effort {
            DstEncoderEffort::Fast => vec![192, 128],
            DstEncoderEffort::Balanced => DEFAULT_COEFFICIENT_SCALES.to_vec(),
            DstEncoderEffort::HighCompression => vec![320, 255, 224, 192, 160, 128, 96, 64],
        }
    } else {
        options.coefficient_quantization_scales.clone()
    };
    for scale in &mut scales {
        *scale = (*scale).clamp(1, MAX_PRED_COEFF_ABS);
    }
    scales.sort_unstable();
    scales.dedup();
    scales.reverse();
    scales
}

fn candidate_prune_thresholds(options: &DstEncoderOptions) -> Vec<i32> {
    let mut thresholds = if options.coefficient_prune_thresholds.is_empty() {
        match options.effort {
            DstEncoderEffort::Fast => vec![0, 2],
            DstEncoderEffort::Balanced => DEFAULT_COEFFICIENT_PRUNE_THRESHOLDS.to_vec(),
            DstEncoderEffort::HighCompression => vec![0, 1, 2, 3, 4, 6, 8],
        }
    } else {
        options.coefficient_prune_thresholds.clone()
    };
    for threshold in &mut thresholds {
        *threshold = (*threshold).clamp(0, MAX_PRED_COEFF_ABS);
    }
    thresholds.sort_unstable();
    thresholds.dedup();
    thresholds
}

fn encode_predictive_candidate(
    interleaved_dsd: &[u8],
    channels: usize,
    filters: &[Vec<i32>],
    table_for_channel: &[usize],
    layout: DstTableStrategy,
) -> Vec<u8> {
    let filter_lut = build_filter_lut(filters);
    let probabilities = derive_probability_tables(
        interleaved_dsd,
        channels,
        &filter_lut,
        table_for_channel,
    );

    let mut writer = BitWriter::new();
    writer.write_bit(1); // DSTCoded = 1.

    write_segment_header(&mut writer);
    writer.write_bit(1); // PSameMapAsF: probability maps match filter maps.
    write_table_map(&mut writer, channels, layout.uses_per_channel_tables());
    for _ in 0..channels {
        writer.write_bit(0); // HalfProb disabled; use learned probability table from sample zero.
    }

    for coeffs in filters {
        writer.write_bits((coeffs.len() - 1) as u32, 7);
        writer.write_bit(0); // uncoded coefficient table
        for &coeff in coeffs {
            writer.write_signed(coeff, 9);
        }
    }

    for table in &probabilities {
        writer.write_bits((table.len() - 1) as u32, 6);
        writer.write_bit(0); // uncoded probability table
        for &probability in table {
            writer.write_bits((probability - 1) as u32, 7);
        }
    }

    writer.write_bit(0); // arithmetic-code start bit required by decoder.

    let first_filter_coeff = filters
        .first()
        .and_then(|coeffs| coeffs.first())
        .copied()
        .unwrap_or(0);
    let arithmetic_bits = encode_residuals(
        interleaved_dsd,
        channels,
        &filter_lut,
        &probabilities,
        table_for_channel,
        first_filter_coeff,
    );
    writer.write_bit_slice(&arithmetic_bits);
    writer.into_bytes()
}

fn channel_autocorrelations(interleaved_dsd: &[u8], channels: usize, max_order: usize) -> Vec<Vec<f64>> {
    (0..channels)
        .map(|channel| autocorrelation_for_channel(interleaved_dsd, channels, channel, max_order))
        .collect()
}

fn autocorrelation_for_channel(
    interleaved_dsd: &[u8],
    channels: usize,
    channel: usize,
    max_order: usize,
) -> Vec<f64> {
    let mut autocorr = vec![0.0f64; max_order + 1];
    if max_order >= FRAME_BITS_PER_CHANNEL {
        return autocorr;
    }

    let sample_count = FRAME_BITS_PER_CHANNEL - max_order;
    for bit_index in max_order..FRAME_BITS_PER_CHANNEL {
        let current = dsd_lpc_value(get_interleaved_bit(interleaved_dsd, channels, bit_index, channel));
        for lag in 0..=max_order {
            let previous = dsd_lpc_value(get_interleaved_bit(
                interleaved_dsd,
                channels,
                bit_index - lag,
                channel,
            ));
            autocorr[lag] += current * previous;
        }
    }

    let norm = sample_count.max(1) as f64;
    for value in &mut autocorr {
        *value /= norm;
    }
    autocorr[0] = autocorr[0].max(1.0e-12);
    autocorr
}

fn shared_autocorrelation(channel_autocorr: &[Vec<f64>]) -> Vec<f64> {
    let first = match channel_autocorr.first() {
        Some(first) => first,
        None => return Vec::new(),
    };
    let mut shared = vec![0.0; first.len()];
    for autocorr in channel_autocorr {
        for (dst, &src) in shared.iter_mut().zip(autocorr) {
            *dst += src;
        }
    }
    let norm = channel_autocorr.len().max(1) as f64;
    for value in &mut shared {
        *value /= norm;
    }
    shared[0] = shared[0].max(1.0e-12);
    shared
}

fn levinson_durbin(autocorr: &[f64], order: usize) -> Vec<f64> {
    debug_assert!(autocorr.len() > order);
    let mut coeffs = vec![0.0f64; order];
    let mut error = autocorr[0].max(1.0e-12);

    for i in 0..order {
        let mut acc = autocorr[i + 1];
        for j in 0..i {
            acc -= coeffs[j] * autocorr[i - j];
        }

        let reflection = (acc / error).clamp(-0.999_999, 0.999_999);
        let mut next = coeffs.clone();
        next[i] = reflection;
        for j in 0..i {
            next[j] = coeffs[j] - reflection * coeffs[i - 1 - j];
        }
        coeffs = next;
        error *= 1.0 - reflection * reflection;
        if !error.is_finite() || error <= 1.0e-12 {
            error = 1.0e-12;
        }
    }

    coeffs
}

fn quantize_lpc_coefficients(coeffs: &[f64], scale: i32, prune_threshold: i32) -> Vec<i32> {
    let mut quantized = coeffs
        .iter()
        .map(|&coeff| {
            // The decoder's status convention uses +1 for one-bits and -1 for
            // zero-bits, while the LPC fit below models DSD as +1 for zero and
            // -1 for one. Negating the LPC coefficient therefore makes a
            // positive predictor correspond to a predicted zero-bit.
            let value = -(coeff * f64::from(scale)).round() as i32;
            let pruned = if value.abs() <= prune_threshold { 0 } else { value };
            pruned.clamp(-MAX_PRED_COEFF_ABS, MAX_PRED_COEFF_ABS)
        })
        .collect::<Vec<_>>();

    while quantized.len() > 1 && quantized.last().copied() == Some(0) {
        quantized.pop();
    }
    if quantized.is_empty() {
        quantized.push(0);
    }
    quantized
}

fn dsd_lpc_value(bit: u8) -> f64 {
    if bit == 0 { 1.0 } else { -1.0 }
}

fn get_interleaved_bit(interleaved_dsd: &[u8], channels: usize, bit_index: usize, channel: usize) -> u8 {
    let byte_base = (bit_index >> 3) * channels;
    let bit_in_byte = 7 - (bit_index & 7);
    (interleaved_dsd[byte_base + channel] >> bit_in_byte) & 1
}

fn push_status_bit(status: &mut [u8; 16], bit: u8) {
    let mut carry = bit & 1;
    for byte in status.iter_mut() {
        let next_carry = (*byte >> 7) & 1;
        *byte = (*byte << 1) | carry;
        carry = next_carry;
    }
}


struct FramePrescreen {
    sample_bytes: usize,
    unique_bytes: usize,
    transition_percent: u32,
    reject_predictive: bool,
}

fn fast_prescreen(interleaved_dsd: &[u8], options: &DstEncoderOptions) -> Option<FramePrescreen> {
    if !options.fast_prescreen || interleaved_dsd.is_empty() {
        return None;
    }

    let sample = interleaved_dsd.len().min(PRESCREEN_SAMPLE_BYTES);
    let mut seen = [false; 256];
    let mut unique = 0usize;
    let mut transitions = 0usize;
    let mut bit_count = 0usize;
    let mut previous: Option<u8> = None;

    for &byte in &interleaved_dsd[..sample] {
        if !seen[usize::from(byte)] {
            seen[usize::from(byte)] = true;
            unique += 1;
        }
        for bit in (0..8).rev() {
            let current = (byte >> bit) & 1;
            if let Some(prev) = previous {
                if prev != current {
                    transitions += 1;
                }
            }
            previous = Some(current);
            bit_count += 1;
        }
    }

    let denom = bit_count.saturating_sub(1).max(1);
    let transition_percent = ((transitions * 100) / denom) as u32;
    let looks_random = unique >= PRESCREEN_MIN_UNIQUE_BYTES
        && transition_percent >= PRESCREEN_MIN_TRANSITION_PERCENT;
    let reject_predictive = matches!(options.effort, DstEncoderEffort::Fast) && looks_random;

    Some(FramePrescreen {
        sample_bytes: sample,
        unique_bytes: unique,
        transition_percent,
        reject_predictive,
    })
}

fn derive_probability_tables(
    interleaved_dsd: &[u8],
    channels: usize,
    filter_lut: &[[[i16; 256]; 16]],
    table_for_channel: &[usize],
) -> Vec<Vec<i32>> {
    let table_count = filter_lut.len().max(1);
    let mut zeros = vec![[0u32; PROBABILITY_TABLE_LEN]; table_count];
    let mut totals = vec![[0u32; PROBABILITY_TABLE_LEN]; table_count];
    let mut status = [[0xAAu8; 16]; MAX_CHANNELS];

    for bit_index in 0..FRAME_BITS_PER_CHANNEL {
        for ch in 0..channels {
            let table = table_for_channel[ch].min(table_count - 1);
            let predict = predict_from_lut(&filter_lut[table], &status[ch]);
            let actual = get_interleaved_bit(interleaved_dsd, channels, bit_index, ch);
            let residual = (((predict >> 15) as u8) ^ actual) & 1;
            let bin = probability_bin(predict, PROBABILITY_TABLE_LEN);
            totals[table][bin] = totals[table][bin].saturating_add(1);
            if residual == 0 {
                zeros[table][bin] = zeros[table][bin].saturating_add(1);
            }
            push_status_bit(&mut status[ch], actual);
        }
    }

    let mut probabilities = Vec::with_capacity(table_count);
    for table in 0..table_count {
        let mut row = Vec::with_capacity(PROBABILITY_TABLE_LEN);
        for bin in 0..PROBABILITY_TABLE_LEN {
            let total = totals[table][bin];
            let probability = if total == 0 {
                64
            } else {
                let zero = zeros[table][bin];
                (((zero * 128) + (total / 2)) / total).clamp(1, 128) as i32
            };
            row.push(probability);
        }
        while row.len() > 1 && row.last().copied() == Some(64) {
            row.pop();
        }
        probabilities.push(row);
    }
    probabilities
}

fn encode_residuals(
    interleaved_dsd: &[u8],
    channels: usize,
    filter_lut: &[[[i16; 256]; 16]],
    probabilities: &[Vec<i32>],
    table_for_channel: &[usize],
    first_filter_coeff: i32,
) -> Vec<u8> {
    let mut arithmetic = ArithmeticBitEncoder::new();
    let mut status = [[0xAAu8; 16]; MAX_CHANNELS];

    arithmetic.encode_bit(0, prob_dst_x_bit(first_filter_coeff) as u32);

    for bit_index in 0..FRAME_BITS_PER_CHANNEL {
        for ch in 0..channels {
            let table = table_for_channel[ch].min(filter_lut.len().saturating_sub(1));
            let predict = predict_from_lut(&filter_lut[table], &status[ch]);
            let actual = get_interleaved_bit(interleaved_dsd, channels, bit_index, ch);
            let residual = (((predict >> 15) as u8) ^ actual) & 1;
            let probability = probability_for_predict(predict, &probabilities[table]) as u32;
            arithmetic.encode_bit(residual, probability);
            push_status_bit(&mut status[ch], actual);
        }
    }

    arithmetic.finish()
}

fn build_filter_lut(filters: &[Vec<i32>]) -> Vec<[[i16; 256]; 16]> {
    filters
        .iter()
        .map(|coeffs| {
            let mut table = [[0i16; 256]; 16];
            for byte_tap in 0..16 {
                let base = byte_tap * 8;
                let available = coeffs.len().saturating_sub(base).min(8);
                for history in 0..256usize {
                    let mut total = 0i32;
                    for bit in 0..available {
                        let history_bit = if ((history >> bit) & 1) != 0 { 1 } else { -1 };
                        total += history_bit * coeffs[base + bit];
                    }
                    table[byte_tap][history] = total.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
                }
            }
            table
        })
        .collect()
}

fn predict_from_lut(filter_lut: &[[i16; 256]; 16], status: &[u8; 16]) -> i32 {
    let mut total = 0i32;
    for tap in 0..16 {
        total += i32::from(filter_lut[tap][usize::from(status[tap])]);
    }
    total
}

fn probability_for_predict(predict: i32, table: &[i32]) -> i32 {
    let idx = probability_bin(predict, table.len());
    table[idx]
}

fn probability_bin(predict: i32, table_len: usize) -> usize {
    let abs_predict = if predict < 0 { (-predict) as usize } else { predict as usize };
    let idx = abs_predict >> 3;
    idx.min(table_len.saturating_sub(1))
}

#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    bit_pos: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            bit_pos: 0,
        }
    }

    fn write_bit(&mut self, bit: u8) {
        if self.bit_pos == 0 {
            self.bytes.push(0);
        }
        if bit & 1 != 0 {
            let idx = self.bytes.len() - 1;
            self.bytes[idx] |= 1u8 << (7 - self.bit_pos);
        }
        self.bit_pos = (self.bit_pos + 1) & 7;
    }

    fn write_bits(&mut self, value: u32, bits: usize) {
        for bit in (0..bits).rev() {
            self.write_bit(((value >> bit) & 1) as u8);
        }
    }

    fn write_signed(&mut self, value: i32, bits: usize) {
        let mask = if bits == 32 { u32::MAX } else { (1u32 << bits) - 1 };
        self.write_bits((value as u32) & mask, bits);
    }

    fn write_bit_slice(&mut self, bits: &[u8]) {
        for &bit in bits {
            self.write_bit(bit);
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Minimal DST arithmetic bit encoder.
///
/// Provenance note: this encoder is maintained as a first-party inverse of the
/// arithmetic decoder in `decoder.rs` and the public DST bitstream state update
/// (`a`, `c`, probability-scaled split, and renormalization). It must not be
/// synchronized against or copied from GPL-3.0-only encoder code in the
/// `cladst` reference source. Future changes to this block should cite the
/// public decoder/spec behavior they invert, and reviewers should reject patches
/// whose structure or comments are derived from cladst rather than from that
/// public decoding contract.
struct ArithmeticBitEncoder {
    a: u32,
    c: u32,
    bits: Vec<u8>,
    pending_bit: u8,
    pending_ones: u32,
    has_pending: bool,
}

impl ArithmeticBitEncoder {
    fn new() -> Self {
        Self {
            a: ARITHMETIC_ONE - 1,
            c: 0,
            bits: Vec::new(),
            pending_bit: 0,
            pending_ones: 0,
            has_pending: false,
        }
    }

    fn encode_bit(&mut self, bit: u8, probability_zero: u32) {
        debug_assert!((1..=128).contains(&probability_zero));
        let k = (self.a >> 8) | ((self.a >> 7) & 1);
        let q = k * probability_zero;
        let a_minus_q = self.a - q;

        if bit == 0 {
            self.c += a_minus_q;
            self.a = q;
        } else {
            self.a = a_minus_q;
        }

        while self.a < ARITHMETIC_HALF {
            self.shift_out();
            self.a <<= 1;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        for _ in 0..ARITHMETIC_BITS {
            self.shift_out();
        }
        self.flush_pending();
        self.bits
    }

    fn shift_out(&mut self) {
        if self.c >= ARITHMETIC_ONE {
            self.c -= ARITHMETIC_ONE;
            self.apply_carry();
        }
        let bit = ((self.c >> (ARITHMETIC_BITS - 1)) & 1) as u8;
        self.emit_bit(bit);
        self.c = (self.c << 1) & (ARITHMETIC_ONE - 1);
    }

    fn emit_bit(&mut self, bit: u8) {
        if !self.has_pending {
            self.pending_bit = bit;
            self.pending_ones = 0;
            self.has_pending = true;
        } else if bit == 1 {
            self.pending_ones += 1;
        } else {
            self.flush_pending();
            self.pending_bit = 0;
            self.pending_ones = 0;
            self.has_pending = true;
        }
    }

    fn flush_pending(&mut self) {
        if !self.has_pending {
            return;
        }
        self.bits.push(self.pending_bit);
        for _ in 0..self.pending_ones {
            self.bits.push(1);
        }
        self.has_pending = false;
        self.pending_ones = 0;
    }

    fn apply_carry(&mut self) {
        if !self.has_pending {
            propagate_carry(&mut self.bits);
            return;
        }

        let ones = self.pending_ones;
        if self.pending_bit == 0 {
            self.bits.push(1);
            for _ in 0..ones {
                self.bits.push(0);
            }
        } else {
            propagate_carry(&mut self.bits);
            self.bits.push(0);
            for _ in 0..ones {
                self.bits.push(0);
            }
        }
        self.has_pending = false;
        self.pending_ones = 0;
    }
}

fn propagate_carry(bits: &mut Vec<u8>) {
    for idx in (0..bits.len()).rev() {
        if bits[idx] == 0 {
            bits[idx] = 1;
            return;
        }
        bits[idx] = 0;
    }
    bits.insert(0, 1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dst::decode_frame;

    fn patterned_frame(channel_count: u8) -> Vec<u8> {
        let mut interleaved = vec![0u8; dst_interleaved_frame_len(channel_count).unwrap()];
        for (i, b) in interleaved.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
        interleaved
    }

    #[test]
    fn uncompressed_stereo_frame_roundtrips_through_decoder() {
        let interleaved = patterned_frame(2);
        let encoded = encode_uncompressed_frame_interleaved(&interleaved, 2).unwrap();
        assert_eq!(encoded.len(), 1 + interleaved.len());
        assert_eq!(encoded[0], 0);
        assert_eq!(decode_frame(&encoded, 2).unwrap(), interleaved);
    }

    #[test]
    fn uncompressed_six_channel_frame_roundtrips_through_decoder() {
        let interleaved = patterned_frame(6);
        let encoded = encode_uncompressed_frame_interleaved(&interleaved, 6).unwrap();
        assert_eq!(decode_frame(&encoded, 6).unwrap(), interleaved);
    }

    #[test]
    fn predictive_stereo_synthetic_frame_roundtrips_when_requested_directly() {
        let interleaved = patterned_frame(2);
        let options = DstEncoderOptions::default();
        let encoded = encode_predictive_frame_interleaved(&interleaved, 2, &options).unwrap();
        assert_ne!(encoded[0] & 0x80, 0);
        assert_eq!(decode_frame(&encoded, 2).unwrap(), interleaved);
    }

    #[test]
    fn predictive_six_channel_synthetic_frame_roundtrips_when_requested_directly() {
        let interleaved = patterned_frame(6);
        let options = DstEncoderOptions::default();
        let encoded = encode_predictive_frame_interleaved(&interleaved, 6, &options).unwrap();
        assert_ne!(encoded[0] & 0x80, 0);
        assert_eq!(decode_frame(&encoded, 6).unwrap(), interleaved);
    }

    #[test]
    fn default_encoder_rejects_raw_fallback_for_unsupported_layout() {
        let interleaved = vec![0u8; dst_interleaved_frame_len(1).unwrap()];
        let err = encode_frame_interleaved(&interleaved, 1, &DstEncoderOptions::default())
            .unwrap_err();
        assert_eq!(
            err,
            DstEncodeError::PredictiveUnsupportedChannelCount { channel_count: 1 }
        );
    }

    #[test]
    fn verification_failure_is_terminal_even_when_raw_fallback_is_enabled_by_default() {
        let interleaved = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let options = DstEncoderOptions {
            raw_fallback: RawDstFallbackPolicy::Enabled,
            verification_failure_policy: DstVerificationFailurePolicy::Fail,
            ..DstEncoderOptions::default()
        };
        let telemetry = DstFrameEncodeTelemetry::new(interleaved.len());
        let (result, telemetry) = raw_fallback_or_error_with_telemetry(
            &interleaved,
            2,
            &options,
            telemetry,
            Instant::now(),
            DstEncodeError::VerificationFailed,
        );
        assert_eq!(result.unwrap_err(), DstEncodeError::VerificationFailed);
        assert_eq!(telemetry.selected_encoding, None);
        assert_eq!(telemetry.terminal_error, Some(DstEncodeFailureClass::VerificationFailed));
        assert_eq!(telemetry.raw_fallback_reason, None);
    }

    #[test]
    fn verification_failure_can_be_downgraded_only_by_explicit_policy() {
        let interleaved = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let options = DstEncoderOptions {
            raw_fallback: RawDstFallbackPolicy::Enabled,
            verification_failure_policy: DstVerificationFailurePolicy::AllowRawFallback,
            ..DstEncoderOptions::default()
        };
        let telemetry = DstFrameEncodeTelemetry::new(interleaved.len());
        let (result, telemetry) = raw_fallback_or_error_with_telemetry(
            &interleaved,
            2,
            &options,
            telemetry,
            Instant::now(),
            DstEncodeError::VerificationFailed,
        );
        let encoded = result.unwrap();
        assert_eq!(encoded.encoding, DstFrameEncoding::Uncompressed);
        assert_eq!(telemetry.selected_encoding, Some(DstFrameEncoding::Uncompressed));
        assert_eq!(telemetry.terminal_error, None);
        assert_eq!(telemetry.raw_fallback_reason, Some(DstEncodeFailureClass::VerificationFailed));
    }

    #[test]
    fn raw_fallback_telemetry_records_non_verification_reason() {
        let interleaved = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let options = DstEncoderOptions {
            minimum_savings_bytes: interleaved.len() + 1,
            raw_fallback: RawDstFallbackPolicy::Enabled,
            ..DstEncoderOptions::default()
        };
        let (result, telemetry) = encode_frame_interleaved_with_telemetry(&interleaved, 2, &options);
        assert_eq!(result.unwrap().encoding, DstFrameEncoding::Uncompressed);
        assert_eq!(
            telemetry.raw_fallback_reason,
            Some(DstEncodeFailureClass::CompressionNotBeneficial)
        );
        assert_eq!(telemetry.terminal_error, None);
    }

    #[test]
    fn verification_failure_kind_distinguishes_decode_error_and_mismatch() {
        let source = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        assert_eq!(
            verify_predictive_candidate(&[0x80], 2, &source).unwrap_err(),
            DstVerificationFailureKind::DecodeError
        );

        let other = vec![0xffu8; dst_interleaved_frame_len(2).unwrap()];
        let encoded = encode_uncompressed_frame_interleaved(&other, 2).unwrap();
        assert_eq!(
            verify_predictive_candidate(&encoded, 2, &source).unwrap_err(),
            DstVerificationFailureKind::DecodedDsdMismatch
        );
    }

    #[test]
    fn raw_fallback_is_opt_in_for_unsupported_layout() {
        let interleaved = vec![0u8; dst_interleaved_frame_len(1).unwrap()];
        let options = DstEncoderOptions {
            raw_fallback: RawDstFallbackPolicy::Enabled,
            ..DstEncoderOptions::default()
        };
        let encoded = encode_frame_interleaved(&interleaved, 1, &options).unwrap();
        assert_eq!(encoded.encoding, DstFrameEncoding::Uncompressed);
        assert_eq!(encoded.bytes[0], 0);
        assert_eq!(&encoded.bytes[1..], &interleaved[..]);
    }

    #[test]
    fn default_encoder_rejects_when_predictive_savings_margin_is_not_met() {
        let interleaved = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let options = DstEncoderOptions {
            minimum_savings_bytes: interleaved.len() + 1,
            ..DstEncoderOptions::default()
        };
        match encode_frame_interleaved(&interleaved, 2, &options).unwrap_err() {
            DstEncodeError::CompressionNotBeneficial {
                predictive_len,
                raw_len,
                minimum_savings_bytes,
            } => {
                assert!(predictive_len > 0);
                assert_eq!(raw_len, interleaved.len() + 1);
                assert_eq!(minimum_savings_bytes, interleaved.len() + 1);
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[test]
    fn predictive_candidate_can_be_requested_directly() {
        let interleaved = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let options = DstEncoderOptions {
            prediction_order: 8,
            ..DstEncoderOptions::default()
        };
        let encoded = encode_predictive_frame_interleaved(&interleaved, 2, &options).unwrap();
        assert_ne!(encoded[0] & 0x80, 0);
        assert_eq!(decode_frame(&encoded, 2).unwrap(), interleaved);
    }


    #[test]
    fn predictive_search_considers_multiple_orders_and_selects_smallest_verified_frame() {
        let interleaved = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let single_candidate_options = DstEncoderOptions {
            prediction_order: 1,
            per_channel_filters: false,
            candidate_prediction_orders: vec![1],
            coefficient_quantization_scales: vec![255],
            coefficient_prune_thresholds: vec![0],
            ..DstEncoderOptions::default()
        };
        let search_options = DstEncoderOptions {
            prediction_order: 1,
            per_channel_filters: true,
            candidate_prediction_orders: vec![1, 4, 8, 16],
            coefficient_quantization_scales: vec![255, 192],
            coefficient_prune_thresholds: vec![0, 1],
            ..DstEncoderOptions::default()
        };

        let single = encode_predictive_frame_interleaved(&interleaved, 2, &single_candidate_options).unwrap();
        let searched = encode_predictive_frame_interleaved(&interleaved, 2, &search_options).unwrap();

        assert!(searched.len() <= single.len());
        assert_eq!(decode_frame(&searched, 2).unwrap(), interleaved);
    }

    #[test]
    fn rejects_invalid_candidate_prediction_order() {
        let interleaved = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let options = DstEncoderOptions {
            prediction_order: 8,
            candidate_prediction_orders: vec![8, MAX_TABLE_LEN + 1],
            ..DstEncoderOptions::default()
        };
        assert_eq!(
            encode_predictive_frame_interleaved(&interleaved, 2, &options).unwrap_err(),
            DstEncodeError::InvalidPredictionOrder {
                prediction_order: MAX_TABLE_LEN + 1
            }
        );
    }

    #[test]
    fn rejects_wrong_frame_length() {
        let err = encode_uncompressed_frame_interleaved(&[0; 17], 2).unwrap_err();
        assert_eq!(
            err,
            DstEncodeError::InvalidFrameLength {
                expected: FRAME_BYTES_PER_CHANNEL * 2,
                actual: 17,
            }
        );
    }

    #[test]
    fn rejects_invalid_prediction_order() {
        let interleaved = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let options = DstEncoderOptions {
            prediction_order: 0,
            ..DstEncoderOptions::default()
        };
        assert_eq!(
            encode_frame_interleaved(&interleaved, 2, &options).unwrap_err(),
            DstEncodeError::InvalidPredictionOrder { prediction_order: 0 }
        );
    }



    #[test]
    fn predictive_encoder_returns_structured_telemetry_for_direct_selection() {
        let interleaved = patterned_frame(2);
        let (encoded, telemetry) =
            encode_predictive_frame_interleaved_with_telemetry(&interleaved, 2, &DstEncoderOptions::default());
        let encoded = encoded.unwrap();

        assert_eq!(telemetry.selected_encoding, Some(DstFrameEncoding::Predictive));
        assert_eq!(telemetry.input_raw_bytes, interleaved.len());
        assert_eq!(telemetry.encoded_bytes, encoded.len());
        assert_eq!(decode_frame(&encoded, 2).unwrap(), interleaved);
        assert!(telemetry.predictive_candidates > 0);
        assert!(telemetry.verified_predictive_candidates > 0);
        let predictor = telemetry.selected_predictor.as_ref().unwrap();
        assert!(predictor.prediction_order >= 1);
        assert!(matches!(
            predictor.table_strategy,
            DstTableStrategy::Shared | DstTableStrategy::PerChannel
        ));
    }

    #[test]
    fn rejected_encoder_attempt_still_returns_candidate_telemetry() {
        let interleaved = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let options = DstEncoderOptions {
            minimum_savings_bytes: interleaved.len() + 1,
            ..DstEncoderOptions::default()
        };
        let (result, telemetry) = encode_frame_interleaved_with_telemetry(&interleaved, 2, &options);

        assert!(matches!(
            result.unwrap_err(),
            DstEncodeError::CompressionNotBeneficial { .. }
        ));
        assert_eq!(telemetry.selected_encoding, None);
        assert_eq!(telemetry.encoded_bytes, 0);
        assert!(telemetry.predictive_candidates > 0);
        assert!(telemetry.verified_predictive_candidates > 0);
        assert!(telemetry.unprofitable_predictive_candidates > 0);
    }

    #[test]
    fn padded_final_frame_returns_crc_source() {
        let (encoded, padded) = encode_uncompressed_frame_interleaved_padded(&[0xaa, 0x55], 2).unwrap();
        assert_eq!(padded.len(), FRAME_BYTES_PER_CHANNEL * 2);
        assert_eq!(&padded[..2], &[0xaa, 0x55]);
        assert!(padded[2..].iter().all(|&b| b == 0));
        assert_eq!(&encoded[1..], &padded[..]);
    }
}
