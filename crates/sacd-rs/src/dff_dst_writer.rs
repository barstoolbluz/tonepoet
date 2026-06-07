// SPDX-License-Identifier: GPL-2.0-or-later
//! Philips DSDIFF/DST writer.
//!
//! The writer emits a DSDIFF file with `CMPR = "DST "`, a top-level `DST `
//! data chunk, `FRTE`, one `DSTF` chunk per frame, a `DSTC` checksum chunk for
//! each frame, and a trailing `DSTI` frame index. Frame payloads can either be
//! supplied by callers via [`Self::write_encoded_frame`] or produced from
//! interleaved DSD bytes with [`Self::write_interleaved_frame`]. Use
//! [`Self::write_passthrough_frame`] for source frames that are already DST
//! coded; this preserves the original professional DST payload while still
//! writing the mandatory DSDIFF `DSTC` checksum.
//!
//! Channel policy is deliberately split by operation:
//!
//! * container construction, reader/decode validation, source-DST passthrough,
//!   and caller-supplied encoded frames accept legal 1-through-6-channel DST
//!   layouts;
//! * predictive DST generation is narrower and currently treated as verified
//!   only for stereo and 5.1/six-channel layouts;
//! * raw `DSTCoded = 0` fallback is explicit opt-in compatibility behavior,
//!   limited to legal 1-through-6-channel DST layouts, and is never selected by
//!   the default writer policy.
//!
//! The generated encoder writes a verified predictive DST subset. That is
//! lossless DST syntax, not a claim of compression parity with SACD mastering
//! encoders. Compression depends on source material and encoder effort; broad
//! external-decoder corpus validation is an acceptance-gate item, not assumed.

use crate::dst::{
    dst_interleaved_frame_len_for_rate, dst_rate_from_sample_rate,
    encode_frame_interleaved_with_rate_and_telemetry, validate_dst_policy,
    DstEncodeError, DstEncodeFailureClass, DstEncoderOptions,
    DstFrameEncodeTelemetry, DstFrameEncoding, DstPolicyScope, DstRate, DstTableStrategy,
    DstVerificationFailureKind, DstVerificationFailurePolicy, RawDstFallbackPolicy,
};
use std::io::{self, Seek, SeekFrom, Write};
use std::time::Duration;

/// DSD64 sample rate: 64 × 44.1 kHz. SACDs are always DSD64.
pub const SACD_SAMPLING_FREQUENCY: u32 = 2_822_400;

const DSDIFF_VERSION: u32 = 0x0105_0000;
const DST_FRAME_RATE: u16 = 75;
const CHUNK_HEADER_SIZE: u64 = 12;

const FRM8: &[u8; 4] = b"FRM8";
const FVER: &[u8; 4] = b"FVER";
const PROP: &[u8; 4] = b"PROP";
const FS: &[u8; 4] = b"FS  ";
const CHNL: &[u8; 4] = b"CHNL";
const CMPR: &[u8; 4] = b"CMPR";
const LSCO: &[u8; 4] = b"LSCO";
const DSD: &[u8; 4] = b"DSD ";
const DST: &[u8; 4] = b"DST ";
const SND: &[u8; 4] = b"SND ";
const FRTE: &[u8; 4] = b"FRTE";
const DSTF: &[u8; 4] = b"DSTF";
const DSTC: &[u8; 4] = b"DSTC";
const DSTI: &[u8; 4] = b"DSTI";

const SLFT: &[u8; 4] = b"SLFT";
const SRGT: &[u8; 4] = b"SRGT";
const MLFT: &[u8; 4] = b"MLFT";
const MRGT: &[u8; 4] = b"MRGT";
const C_ID: &[u8; 4] = b"C   ";
const LS_ID: &[u8; 4] = b"LS  ";
const RS_ID: &[u8; 4] = b"RS  ";
const LFE_ID: &[u8; 4] = b"LFE ";

const CMPR_NAME: &[u8] = b"DST encoded";
const LS_CONFIG_2_CHNL: u16 = 0;
const LS_CONFIG_5_CHNL: u16 = 3;
const LS_CONFIG_6_CHNL: u16 = 4;
const LS_CONFIG_UNDEFINED: u16 = 65535;

/// User-facing capability statement for generated DSDIFF/DST output.
///
/// Keep this wording conservative. It intentionally separates valid, lossless
/// DST syntax from useful compression and avoids claiming parity with SACD
/// authoring/mastering encoders.
pub const DFF_DST_CAPABILITY_STATEMENT: &str =
    "DSDIFF/DST output uses source-DST passthrough for legal 1-6-channel source \
     DST streams when available. Predictive generation is verified only for \
     stereo and six-channel layouts. Raw DST fallback is explicit opt-in, limited \
     to legal 1-6-channel layouts, and broad external-decoder corpus acceptance \
     is not assumed.";


/// Per-frame mode selected by the DSDIFF/DST writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DffDstFrameMode {
    /// Newly encoded predictive DST frame.
    Predictive,
    /// Explicit opt-in raw `DSTCoded = 0` fallback frame.
    RawFallback,
    /// Source SACD DST payload preserved byte-for-byte.
    SourceDstPassthrough,
    /// Caller supplied an encoded DST payload directly.
    CallerSuppliedEncoded,
}

/// Telemetry for one frame physically written to `DSTF`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DffDstFrameTelemetry {
    /// Zero-based frame index in the DSDIFF/DST stream.
    pub frame_index: u64,
    /// Selected writer mode.
    pub mode: DffDstFrameMode,
    /// Raw interleaved DSD bytes represented by this frame.
    pub raw_bytes: u64,
    /// Encoded `DSTF` payload bytes written for this frame.
    pub encoded_bytes: u64,
    /// Predictor order used by the selected frame, when predictive encoding won.
    pub prediction_order: Option<usize>,
    /// Table strategy used by the selected frame, when predictive encoding won.
    pub table_strategy: Option<DstTableStrategy>,
    /// Quantization scale used by the selected frame, when predictive encoding won.
    pub coefficient_scale: Option<i32>,
    /// Prune threshold used by the selected frame, when predictive encoding won.
    pub coefficient_prune_threshold: Option<i32>,
    /// Predictive candidates materialized while evaluating this frame.
    pub predictive_candidates: u64,
    /// Predictive candidates that decode-verified exactly while evaluating this frame.
    pub verified_predictive_candidates: u64,
    /// Number of candidate predictive frames rejected by decode verification.
    pub verification_failures: u64,
    /// Predictive candidates for which verification decode returned an error.
    pub verification_decode_errors: u64,
    /// Predictive candidates that decoded but did not match the source DSD.
    pub verification_mismatches: u64,
    /// Last exact verification failure kind observed while encoding this frame.
    pub last_verification_failure: Option<DstVerificationFailureKind>,
    /// Terminal failure class for rejected encode attempts. Written frames use
    /// `None` because a payload was accepted.
    pub terminal_error: Option<DstEncodeFailureClass>,
    /// Failure class that caused raw fallback to be selected for this frame.
    pub raw_fallback_reason: Option<DstEncodeFailureClass>,
    /// Verified predictive candidates that failed the savings threshold.
    pub unprofitable_predictive_candidates: u64,
    /// True when a fast encoder pre-screen rejected predictive search for this frame.
    pub prescreen_rejected: bool,
    /// Distinct byte values in the pre-screen sample window.
    pub prescreen_unique_bytes: usize,
    /// Approximate adjacent bit-transition percentage in the pre-screen sample.
    pub prescreen_transition_percent: u32,
    /// Wall-clock encode time for generated frames; passthrough/caller-supplied
    /// frames record zero because no encoder search was run by this writer.
    pub encode_time: Duration,
    /// Largest byte expansion avoided while selecting or falling back for this frame.
    pub worst_expansion_avoided_bytes: u64,
}

impl DffDstFrameTelemetry {
    /// Raw bytes divided by encoded bytes for this frame. Values greater than
    /// 1.0 indicate compression relative to raw DST syntax.
    pub fn compression_ratio(&self) -> Option<f64> {
        if self.encoded_bytes == 0 {
            None
        } else {
            Some(self.raw_bytes as f64 / self.encoded_bytes as f64)
        }
    }
}

/// Aggregate telemetry for a DSDIFF/DST writer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DffDstWriterStats {
    /// Frames successfully written to `DSTF`.
    pub frames_written: u64,
    /// Raw interleaved DSD bytes represented by written frames.
    pub total_raw_bytes: u64,
    /// Encoded `DSTF` payload bytes written.
    pub total_encoded_bytes: u64,
    /// Newly encoded predictive frames written.
    pub predictive_frames_written: u64,
    /// Explicit raw fallback frames written.
    pub raw_frames_written: u64,
    /// Source DST frames preserved byte-for-byte.
    pub passthrough_frames_written: u64,
    /// Caller-supplied encoded frames written through `write_encoded_frame`.
    pub caller_supplied_frames_written: u64,
    /// Encode attempts made by this writer, including failed attempts.
    pub encode_attempts: u64,
    /// Predictive candidates materialized by all encode attempts.
    pub predictive_candidates: u64,
    /// Predictive candidates that decode-verified exactly.
    pub verified_predictive_candidates: u64,
    /// Predictive candidates rejected by decode verification.
    pub verification_failures: u64,
    /// Predictive candidates for which verification decode returned an error.
    pub verification_decode_errors: u64,
    /// Predictive candidates that decoded but did not match the source DSD.
    pub verification_mismatches: u64,
    /// Last exact verification failure kind observed by any encode attempt.
    pub last_verification_failure: Option<DstVerificationFailureKind>,
    /// Encode attempts that failed without writing a frame.
    pub terminal_failures: u64,
    /// Last terminal encode failure class observed by the writer.
    pub last_terminal_error: Option<DstEncodeFailureClass>,
    /// Raw fallbacks selected after verification failures. This should remain
    /// zero for strict/default production policies.
    pub raw_fallbacks_after_verification_failure: u64,
    /// Raw fallbacks selected because predictive coding was unavailable or not
    /// profitable enough.
    pub raw_fallbacks_after_non_verification_failure: u64,
    /// Verified predictive candidates that failed the savings threshold.
    pub unprofitable_predictive_candidates: u64,
    /// Encode attempts rejected by the fast pre-screen before candidate materialization.
    pub prescreen_rejections: u64,
    /// Total encode-search time for generated frames and rejected attempts.
    pub total_encode_time: Duration,
    /// Maximum encode-search time for any generated frame or rejected attempt.
    pub max_encode_time: Duration,
    /// Largest byte expansion avoided by model selection or raw fallback.
    pub worst_expansion_avoided_bytes: u64,
    /// Per-frame telemetry for frames actually written.
    pub frames: Vec<DffDstFrameTelemetry>,
}

impl Default for DffDstWriterStats {
    fn default() -> Self {
        Self {
            frames_written: 0,
            total_raw_bytes: 0,
            total_encoded_bytes: 0,
            predictive_frames_written: 0,
            raw_frames_written: 0,
            passthrough_frames_written: 0,
            caller_supplied_frames_written: 0,
            encode_attempts: 0,
            predictive_candidates: 0,
            verified_predictive_candidates: 0,
            verification_failures: 0,
            verification_decode_errors: 0,
            verification_mismatches: 0,
            last_verification_failure: None,
            terminal_failures: 0,
            last_terminal_error: None,
            raw_fallbacks_after_verification_failure: 0,
            raw_fallbacks_after_non_verification_failure: 0,
            unprofitable_predictive_candidates: 0,
            prescreen_rejections: 0,
            total_encode_time: Duration::from_nanos(0),
            max_encode_time: Duration::from_nanos(0),
            worst_expansion_avoided_bytes: 0,
            frames: Vec::new(),
        }
    }
}

/// Evidence supplied by integration tests, release jobs, or external QA.
///
/// These flags deliberately live outside [`DffDstWriterStats`]: writer telemetry
/// can prove what this process emitted, but it cannot prove that canonical
/// hashes, legal/provenance review, broad external decoder checks, or
/// performance budgets passed in CI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DffDstAcceptanceEvidence {
    /// Set true only when canonical generated-output vectors are hash-pinned in tests.
    pub canonical_output_hashes_pinned: bool,
    /// FFmpeg/common-player/sacd_extract-style external corpus checks passed for
    /// the relevant policy profile. This must include more than in-tree decode
    /// verification before user-facing copy may imply broad playability.
    pub external_decoder_corpus_passed: bool,
    /// Provenance/clean-room review for the arithmetic encoder is complete.
    pub provenance_review_complete: bool,
    /// Large-disc encode budget or throughput gate passed for the selected
    /// effort profile.
    pub performance_budget_passed: bool,
}

/// Formal release gate for DSDIFF/DST output.
///
/// The default gate is intentionally strict enough for user-facing release
/// claims. Callers can construct looser gates for local experiments, but should
/// not use those reports to advertise portable DST compression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DffDstAcceptanceGate {
    pub require_non_empty_output: bool,
    pub require_complete_frame_telemetry: bool,
    pub allow_source_dst_passthrough: bool,
    pub allow_caller_supplied_encoded: bool,
    pub allow_raw_fallback: bool,
    pub require_no_terminal_failures: bool,
    pub require_no_verification_failures: bool,
    pub require_no_raw_fallback_after_verification_failure: bool,
    pub require_predictive_frames_verified: bool,
    pub require_predictive_frames_smaller_than_raw: bool,
    /// Optional minimum per-generated-predictive-frame compression ratio in
    /// milli-units. `1001` means encoded payload must be at least slightly
    /// smaller than raw DST syntax.
    pub minimum_predictive_compression_ratio_milli: Option<u32>,
    pub require_canonical_output_hashes: bool,
    pub require_external_decoder_corpus: bool,
    pub require_provenance_review: bool,
    pub require_performance_budget: bool,
}

impl Default for DffDstAcceptanceGate {
    fn default() -> Self {
        Self::production_release()
    }
}

impl DffDstAcceptanceGate {
    /// Strict gate for public release or UI copy that claims DSDIFF/DST writing.
    pub fn production_release() -> Self {
        Self {
            require_non_empty_output: true,
            require_complete_frame_telemetry: true,
            allow_source_dst_passthrough: true,
            allow_caller_supplied_encoded: false,
            allow_raw_fallback: false,
            require_no_terminal_failures: true,
            require_no_verification_failures: true,
            require_no_raw_fallback_after_verification_failure: true,
            require_predictive_frames_verified: true,
            require_predictive_frames_smaller_than_raw: true,
            minimum_predictive_compression_ratio_milli: Some(1001),
            require_canonical_output_hashes: true,
            require_external_decoder_corpus: true,
            require_provenance_review: true,
            require_performance_budget: true,
        }
    }

    /// Gate appropriate for in-tree unit tests that cannot run external players.
    pub fn in_tree_regression() -> Self {
        Self {
            require_external_decoder_corpus: false,
            require_performance_budget: false,
            ..Self::production_release()
        }
    }
}

/// Exact failure emitted by [`DffDstAcceptanceGate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DffDstAcceptanceFailure {
    EmptyOutput,
    IncompleteFrameTelemetry { frames_written: u64, telemetry_frames: usize },
    SourceDstPassthroughDisallowed { frames: u64 },
    CallerSuppliedEncodedDisallowed { frames: u64 },
    RawFallbackDisallowed { frames: u64 },
    TerminalEncodeFailures { count: u64, last: Option<DstEncodeFailureClass> },
    VerificationFailures {
        count: u64,
        decode_errors: u64,
        mismatches: u64,
        last: Option<DstVerificationFailureKind>,
    },
    RawFallbackAfterVerificationFailure { frames: u64 },
    PredictiveFrameMissingVerification { frame_index: u64 },
    PredictiveFrameMissingPredictorTelemetry { frame_index: u64 },
    PredictiveFrameNotSmallerThanRaw { frame_index: u64, raw_bytes: u64, encoded_bytes: u64 },
    PredictiveFrameBelowCompressionRatio {
        frame_index: u64,
        required_ratio_milli: u32,
        actual_ratio_milli: u32,
    },
    CanonicalOutputHashesNotPinned,
    ExternalDecoderCorpusNotPassed,
    ProvenanceReviewIncomplete,
    PerformanceBudgetNotPassed,
}

/// Result of applying a formal acceptance gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DffDstAcceptanceReport {
    pub accepted: bool,
    pub failures: Vec<DffDstAcceptanceFailure>,
}

impl DffDstAcceptanceReport {
    pub fn accepted() -> Self {
        Self { accepted: true, failures: Vec::new() }
    }

    pub fn rejected(failures: Vec<DffDstAcceptanceFailure>) -> Self {
        Self { accepted: failures.is_empty(), failures }
    }
}

impl DffDstWriterStats {
    /// Raw bytes divided by encoded bytes for all written frames. Values greater
    /// than 1.0 indicate aggregate compression.
    pub fn compression_ratio(&self) -> Option<f64> {
        if self.total_encoded_bytes == 0 {
            None
        } else {
            Some(self.total_raw_bytes as f64 / self.total_encoded_bytes as f64)
        }
    }

    /// Encoded bytes divided by raw bytes for all written frames. Values below
    /// 1.0 indicate aggregate compression.
    pub fn encoded_to_raw_ratio(&self) -> Option<f64> {
        if self.total_raw_bytes == 0 {
            None
        } else {
            Some(self.total_encoded_bytes as f64 / self.total_raw_bytes as f64)
        }
    }

    /// Conservative user-facing status string suitable for CLI/TUI display.
    ///
    /// This copy intentionally does not imply SACD mastering-encoder parity or
    /// broad external-decoder proof. Callers that have passed a stricter release
    /// gate can add their own environment-specific validation note.
    pub fn user_facing_summary(&self) -> String {
        let ratio = self
            .compression_ratio()
            .map(|r| format!("{:.3}:1", r))
            .unwrap_or_else(|| "n/a".to_string());
        format!(
            "DSDIFF/DST: {} frame(s), {} source-DST passthrough, {} verified predictive subset, {} explicit raw fallback, compression ratio {}; not SACD-mastering parity; broad external-corpus playback not assumed",
            self.frames_written,
            self.passthrough_frames_written,
            self.predictive_frames_written,
            self.raw_frames_written,
            ratio,
        )
    }

    /// Evaluate this writer's telemetry against a formal DSDIFF/DST acceptance gate.
    pub fn evaluate_acceptance_gate(
        &self,
        gate: &DffDstAcceptanceGate,
        evidence: DffDstAcceptanceEvidence,
    ) -> DffDstAcceptanceReport {
        let mut failures = Vec::new();

        if gate.require_non_empty_output && self.frames_written == 0 {
            failures.push(DffDstAcceptanceFailure::EmptyOutput);
        }
        if gate.require_complete_frame_telemetry && self.frames.len() != self.frames_written as usize {
            failures.push(DffDstAcceptanceFailure::IncompleteFrameTelemetry {
                frames_written: self.frames_written,
                telemetry_frames: self.frames.len(),
            });
        }
        if !gate.allow_source_dst_passthrough && self.passthrough_frames_written != 0 {
            failures.push(DffDstAcceptanceFailure::SourceDstPassthroughDisallowed {
                frames: self.passthrough_frames_written,
            });
        }
        if !gate.allow_caller_supplied_encoded && self.caller_supplied_frames_written != 0 {
            failures.push(DffDstAcceptanceFailure::CallerSuppliedEncodedDisallowed {
                frames: self.caller_supplied_frames_written,
            });
        }
        if !gate.allow_raw_fallback && self.raw_frames_written != 0 {
            failures.push(DffDstAcceptanceFailure::RawFallbackDisallowed {
                frames: self.raw_frames_written,
            });
        }
        if gate.require_no_terminal_failures && self.terminal_failures != 0 {
            failures.push(DffDstAcceptanceFailure::TerminalEncodeFailures {
                count: self.terminal_failures,
                last: self.last_terminal_error,
            });
        }
        if gate.require_no_verification_failures && self.verification_failures != 0 {
            failures.push(DffDstAcceptanceFailure::VerificationFailures {
                count: self.verification_failures,
                decode_errors: self.verification_decode_errors,
                mismatches: self.verification_mismatches,
                last: self.last_verification_failure,
            });
        }
        if gate.require_no_raw_fallback_after_verification_failure
            && self.raw_fallbacks_after_verification_failure != 0
        {
            failures.push(DffDstAcceptanceFailure::RawFallbackAfterVerificationFailure {
                frames: self.raw_fallbacks_after_verification_failure,
            });
        }

        for frame in &self.frames {
            if frame.mode == DffDstFrameMode::Predictive {
                if gate.require_predictive_frames_verified && frame.verified_predictive_candidates == 0 {
                    failures.push(DffDstAcceptanceFailure::PredictiveFrameMissingVerification {
                        frame_index: frame.frame_index,
                    });
                }
                if frame.prediction_order.is_none() || frame.table_strategy.is_none() {
                    failures.push(DffDstAcceptanceFailure::PredictiveFrameMissingPredictorTelemetry {
                        frame_index: frame.frame_index,
                    });
                }
                if gate.require_predictive_frames_smaller_than_raw && frame.encoded_bytes >= frame.raw_bytes {
                    failures.push(DffDstAcceptanceFailure::PredictiveFrameNotSmallerThanRaw {
                        frame_index: frame.frame_index,
                        raw_bytes: frame.raw_bytes,
                        encoded_bytes: frame.encoded_bytes,
                    });
                }
                if let Some(required) = gate.minimum_predictive_compression_ratio_milli {
                    let actual = compression_ratio_milli(frame.raw_bytes, frame.encoded_bytes);
                    if actual < required {
                        failures.push(DffDstAcceptanceFailure::PredictiveFrameBelowCompressionRatio {
                            frame_index: frame.frame_index,
                            required_ratio_milli: required,
                            actual_ratio_milli: actual,
                        });
                    }
                }
            }
        }

        if gate.require_canonical_output_hashes && !evidence.canonical_output_hashes_pinned {
            failures.push(DffDstAcceptanceFailure::CanonicalOutputHashesNotPinned);
        }
        if gate.require_external_decoder_corpus && !evidence.external_decoder_corpus_passed {
            failures.push(DffDstAcceptanceFailure::ExternalDecoderCorpusNotPassed);
        }
        if gate.require_provenance_review && !evidence.provenance_review_complete {
            failures.push(DffDstAcceptanceFailure::ProvenanceReviewIncomplete);
        }
        if gate.require_performance_budget && !evidence.performance_budget_passed {
            failures.push(DffDstAcceptanceFailure::PerformanceBudgetNotPassed);
        }

        DffDstAcceptanceReport::rejected(failures)
    }

    /// Average encode-search time across generated-frame attempts, including
    /// rejected attempts. Passthrough and caller-supplied frames are not counted.
    pub fn average_encode_time(&self) -> Option<Duration> {
        if self.encode_attempts == 0 {
            None
        } else {
            Some(duration_div(self.total_encode_time, self.encode_attempts))
        }
    }

    fn record_encode_attempt(&mut self, telemetry: &DstFrameEncodeTelemetry) -> io::Result<()> {
        self.encode_attempts = checked_add_u64(self.encode_attempts, 1, "DST encode-attempt count overflow")?;
        self.predictive_candidates = checked_add_u64(
            self.predictive_candidates,
            telemetry.predictive_candidates,
            "DST predictive-candidate count overflow",
        )?;
        self.verified_predictive_candidates = checked_add_u64(
            self.verified_predictive_candidates,
            telemetry.verified_predictive_candidates,
            "DST verified-candidate count overflow",
        )?;
        self.verification_failures = checked_add_u64(
            self.verification_failures,
            telemetry.verification_failures,
            "DST verification-failure count overflow",
        )?;
        self.verification_decode_errors = checked_add_u64(
            self.verification_decode_errors,
            telemetry.verification_decode_errors,
            "DST verification-decode-error count overflow",
        )?;
        self.verification_mismatches = checked_add_u64(
            self.verification_mismatches,
            telemetry.verification_mismatches,
            "DST verification-mismatch count overflow",
        )?;
        if let Some(kind) = telemetry.last_verification_failure {
            self.last_verification_failure = Some(kind);
        }
        if let Some(error) = telemetry.terminal_error {
            self.terminal_failures = checked_add_u64(
                self.terminal_failures,
                1,
                "DST terminal-failure count overflow",
            )?;
            self.last_terminal_error = Some(error);
        }
        if let Some(reason) = telemetry.raw_fallback_reason {
            if reason == DstEncodeFailureClass::VerificationFailed {
                self.raw_fallbacks_after_verification_failure = checked_add_u64(
                    self.raw_fallbacks_after_verification_failure,
                    1,
                    "DST verification-raw-fallback count overflow",
                )?;
            } else {
                self.raw_fallbacks_after_non_verification_failure = checked_add_u64(
                    self.raw_fallbacks_after_non_verification_failure,
                    1,
                    "DST non-verification-raw-fallback count overflow",
                )?;
            }
        }
        self.unprofitable_predictive_candidates = checked_add_u64(
            self.unprofitable_predictive_candidates,
            telemetry.unprofitable_predictive_candidates,
            "DST unprofitable-candidate count overflow",
        )?;
        if telemetry.prescreen_rejected {
            self.prescreen_rejections = checked_add_u64(
                self.prescreen_rejections,
                1,
                "DST prescreen-rejection count overflow",
            )?;
        }
        self.total_encode_time = self
            .total_encode_time
            .checked_add(telemetry.encode_time)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "DST encode-time overflow"))?;
        self.max_encode_time = self.max_encode_time.max(telemetry.encode_time);
        self.worst_expansion_avoided_bytes = self
            .worst_expansion_avoided_bytes
            .max(usize_to_u64(telemetry.worst_expansion_avoided_bytes)?);
        Ok(())
    }

    fn record_rejected_encode(&mut self, telemetry: &DstFrameEncodeTelemetry) -> io::Result<()> {
        self.record_encode_attempt(telemetry)
    }

    fn record_generated_frame(
        &mut self,
        frame_index: u64,
        mode: DffDstFrameMode,
        telemetry: &DstFrameEncodeTelemetry,
    ) -> io::Result<()> {
        self.record_encode_attempt(telemetry)?;
        let raw_bytes = usize_to_u64(telemetry.input_raw_bytes)?;
        let encoded_bytes = usize_to_u64(telemetry.encoded_bytes)?;
        let predictor = telemetry.selected_predictor.as_ref();
        self.record_frame(DffDstFrameTelemetry {
            frame_index,
            mode,
            raw_bytes,
            encoded_bytes,
            prediction_order: predictor.map(|p| p.prediction_order),
            table_strategy: predictor.map(|p| p.table_strategy),
            coefficient_scale: predictor.map(|p| p.coefficient_scale),
            coefficient_prune_threshold: predictor.map(|p| p.coefficient_prune_threshold),
            predictive_candidates: telemetry.predictive_candidates,
            verified_predictive_candidates: telemetry.verified_predictive_candidates,
            verification_failures: telemetry.verification_failures,
            verification_decode_errors: telemetry.verification_decode_errors,
            verification_mismatches: telemetry.verification_mismatches,
            last_verification_failure: telemetry.last_verification_failure,
            terminal_error: telemetry.terminal_error,
            raw_fallback_reason: telemetry.raw_fallback_reason,
            unprofitable_predictive_candidates: telemetry.unprofitable_predictive_candidates,
            prescreen_rejected: telemetry.prescreen_rejected,
            prescreen_unique_bytes: telemetry.prescreen_unique_bytes,
            prescreen_transition_percent: telemetry.prescreen_transition_percent,
            encode_time: telemetry.encode_time,
            worst_expansion_avoided_bytes: usize_to_u64(telemetry.worst_expansion_avoided_bytes)?,
        })
    }

    fn record_plain_frame(
        &mut self,
        frame_index: u64,
        mode: DffDstFrameMode,
        raw_bytes: usize,
        encoded_bytes: usize,
    ) -> io::Result<()> {
        self.record_frame(DffDstFrameTelemetry {
            frame_index,
            mode,
            raw_bytes: usize_to_u64(raw_bytes)?,
            encoded_bytes: usize_to_u64(encoded_bytes)?,
            prediction_order: None,
            table_strategy: None,
            coefficient_scale: None,
            coefficient_prune_threshold: None,
            predictive_candidates: 0,
            verified_predictive_candidates: 0,
            verification_failures: 0,
            verification_decode_errors: 0,
            verification_mismatches: 0,
            last_verification_failure: None,
            terminal_error: None,
            raw_fallback_reason: None,
            unprofitable_predictive_candidates: 0,
            prescreen_rejected: false,
            prescreen_unique_bytes: 0,
            prescreen_transition_percent: 0,
            encode_time: Duration::from_nanos(0),
            worst_expansion_avoided_bytes: 0,
        })
    }

    fn record_frame(&mut self, frame: DffDstFrameTelemetry) -> io::Result<()> {
        self.frames_written = checked_add_u64(self.frames_written, 1, "DST writer frame count overflow")?;
        self.total_raw_bytes = checked_add_u64(self.total_raw_bytes, frame.raw_bytes, "DST raw-byte count overflow")?;
        self.total_encoded_bytes = checked_add_u64(
            self.total_encoded_bytes,
            frame.encoded_bytes,
            "DST encoded-byte count overflow",
        )?;
        match frame.mode {
            DffDstFrameMode::Predictive => {
                self.predictive_frames_written = checked_add_u64(
                    self.predictive_frames_written,
                    1,
                    "DST predictive-frame count overflow",
                )?;
            }
            DffDstFrameMode::RawFallback => {
                self.raw_frames_written = checked_add_u64(
                    self.raw_frames_written,
                    1,
                    "DST raw-frame count overflow",
                )?;
            }
            DffDstFrameMode::SourceDstPassthrough => {
                self.passthrough_frames_written = checked_add_u64(
                    self.passthrough_frames_written,
                    1,
                    "DST passthrough-frame count overflow",
                )?;
            }
            DffDstFrameMode::CallerSuppliedEncoded => {
                self.caller_supplied_frames_written = checked_add_u64(
                    self.caller_supplied_frames_written,
                    1,
                    "DST caller-supplied frame count overflow",
                )?;
            }
        }
        self.frames.push(frame);
        Ok(())
    }
}

/// Streaming DSDIFF/DST writer.
///
/// Construction writes a placeholder `FRM8`, `DST ` size, and `FRTE` frame
/// count. [`Self::finish`] appends `DSTI`, then seeks back and patches all
/// deferred size/count fields. Dropping without `finish` leaves an intentionally
/// incomplete file, just like the DSF/DSDIFF writers in this crate.
pub struct DffDstWriter<W: Write + Seek> {
    writer: W,
    channel_count: u8,
    rate: DstRate,
    frm8_size_offset: u64,
    dst_size_offset: u64,
    frte_frame_count_offset: u64,
    /// Bytes contained by the top-level `DST ` chunk payload. This includes
    /// `FRTE`, all `DSTF` chunks, and all `DSTC` chunks, but not the trailing
    /// top-level `DSTI` index chunk.
    dst_payload_bytes: u64,
    frames_written: u64,
    frame_index: Vec<(u64, u32)>,
    /// Optional DSDIFF footer chunks (DIIN + COMT + ID3) appended after the
    /// top-level DSTI index. These bytes are expected to be a sequence of
    /// complete, padded DSDIFF top-level chunks, normally produced by
    /// [`crate::dff_footer::render_dff_footer`].
    footer_bytes: Option<Vec<u8>>,
    stats: DffDstWriterStats,
}

impl<W: Write + Seek> DffDstWriter<W> {
    /// Create a writer and emit the static DSDIFF/DST header. The writer's
    /// stream position is reset to zero.
    pub fn new(mut writer: W, channel_count: u8, sample_rate: u32) -> io::Result<Self> {
        let rate = dst_rate_from_sample_rate(sample_rate).map_err(invalid_input)?;
        validate_dst_policy(DstPolicyScope::Container, rate, channel_count).map_err(invalid_input)?;
        writer.seek(SeekFrom::Start(0))?;

        writer.write_all(FRM8)?;
        let frm8_size_offset = writer.stream_position()?;
        writer.write_all(&0u64.to_be_bytes())?;
        writer.write_all(DSD)?;

        write_chunk(&mut writer, FVER, &DSDIFF_VERSION.to_be_bytes())?;
        let prop = serialize_prop_payload(channel_count, sample_rate)?;
        write_chunk(&mut writer, PROP, &prop)?;

        writer.write_all(DST)?;
        let dst_size_offset = writer.stream_position()?;
        writer.write_all(&0u64.to_be_bytes())?;

        writer.write_all(FRTE)?;
        writer.write_all(&6u64.to_be_bytes())?;
        let frte_frame_count_offset = writer.stream_position()?;
        writer.write_all(&0u32.to_be_bytes())?;
        writer.write_all(&DST_FRAME_RATE.to_be_bytes())?;
        Ok(Self {
            writer,
            channel_count,
            rate,
            frm8_size_offset,
            dst_size_offset,
            frte_frame_count_offset,
            dst_payload_bytes: CHUNK_HEADER_SIZE + 6,
            frames_written: 0,
            frame_index: Vec::new(),
            footer_bytes: None,
            stats: DffDstWriterStats::default(),
        })
    }

    /// Set DSDIFF metadata/footer chunks to append after the `DSTI` index.
    ///
    /// Pass the output of [`crate::dff_footer::render_dff_footer`]. The footer
    /// is written as ordinary top-level DSDIFF chunks after the DSDIFF/DST audio
    /// structure, and `FRM8.chunk_data_size` is patched by [`Self::finish`] to
    /// include it. The `DST ` chunk size is intentionally unchanged because it
    /// covers only `FRTE`, `DSTF`, and `DSTC` data, not trailing `DSTI` or
    /// metadata chunks.
    pub fn set_footer_bytes(&mut self, bytes: Vec<u8>) {
        self.footer_bytes = Some(bytes);
    }

    /// Write one full interleaved DSD frame inside a `DSTF` chunk, followed by
    /// its `DSTC` checksum.
    ///
    /// The default encoder attempts verified predictive DST coding and returns
    /// an error when the compressed candidate is not smaller or cannot be
    /// verified. This avoids implicit raw DST frames whose common-decoder
    /// portability is not proven. `interleaved_dsd` must contain exactly
    /// one full DST frame for this writer's sample rate in DSDIFF/SACD clustered layout. For short
    /// final frames, use [`crate::dst::encode_uncompressed_frame_interleaved_padded`]
    /// yourself and pass the returned padded DSD source to
    /// [`Self::write_encoded_frame`].
    pub fn write_interleaved_frame(&mut self, interleaved_dsd: &[u8]) -> io::Result<()> {
        self.write_interleaved_frame_with_options(interleaved_dsd, &DstEncoderOptions::default())
    }

    /// Write one full interleaved DSD frame with explicit raw-DST fallback.
    ///
    /// This is an opt-in compatibility mode for controlled decoder sets. The
    /// production default deliberately avoids this path because FFmpeg-derived
    /// decoders and players may not accept every raw `DSTCoded = 0` layout.
    pub fn write_interleaved_frame_allowing_raw_fallback(
        &mut self,
        interleaved_dsd: &[u8],
    ) -> io::Result<()> {
        let mut options = DstEncoderOptions::default();
        options.raw_fallback = RawDstFallbackPolicy::Enabled;
        self.write_interleaved_frame_with_options(interleaved_dsd, &options)
    }

    /// Write one full interleaved DSD frame with raw fallback also permitted
    /// after predictive decode-verification failure. This is intentionally
    /// separate from [`Self::write_interleaved_frame_allowing_raw_fallback`] so
    /// strict compatibility mode can allow raw fallback for unprofitable frames
    /// while still failing on suspected encoder bugs.
    pub fn write_interleaved_frame_allowing_raw_fallback_after_verification_failure(
        &mut self,
        interleaved_dsd: &[u8],
    ) -> io::Result<()> {
        let mut options = DstEncoderOptions::default();
        options.raw_fallback = RawDstFallbackPolicy::Enabled;
        options.verification_failure_policy = DstVerificationFailurePolicy::AllowRawFallback;
        self.write_interleaved_frame_with_options(interleaved_dsd, &options)
    }

    /// Write one full interleaved DSD frame using explicit DST encoder options.
    ///
    /// This is the only writer path that generates a new `DSTF` payload from
    /// plain DSD. It therefore follows the predictive-generation policy: the
    /// default path is limited to channel counts for which predictive output is
    /// currently verified. If predictive generation is unsupported or
    /// unprofitable, raw fallback is available only when the caller explicitly
    /// enables [`RawDstFallbackPolicy::Enabled`], and only for legal raw-fallback
    /// channel counts. Passthrough and caller-supplied encoded DST use separate
    /// methods and are not constrained by predictive-generation support.
    pub fn write_interleaved_frame_with_options(
        &mut self,
        interleaved_dsd: &[u8],
        options: &DstEncoderOptions,
    ) -> io::Result<()> {
        let (encoded, telemetry) = encode_frame_interleaved_with_rate_and_telemetry(
            interleaved_dsd,
            self.channel_count,
            self.rate,
            options,
        );
        let encoded = match encoded {
            Ok(encoded) => encoded,
            Err(err) => {
                self.stats.record_rejected_encode(&telemetry)?;
                return Err(invalid_input(err));
            }
        };

        let mode = match encoded.encoding {
            DstFrameEncoding::Predictive => DffDstFrameMode::Predictive,
            DstFrameEncoding::Uncompressed => DffDstFrameMode::RawFallback,
        };
        self.write_encoded_frame_payload(&encoded.bytes, interleaved_dsd)?;
        let frame_index = self.last_written_frame_index()?;
        self.stats.record_generated_frame(frame_index, mode, &telemetry)?;
        Ok(())
    }

    /// Run the DST encoder against one full interleaved DSD frame only to
    /// collect aggregate telemetry. No `DSTF` or `DSTC` chunk is written, and
    /// no per-frame selected-mode record is added because the physical output
    /// stream is unchanged. This is used by extraction corpus-analysis mode to
    /// compare the in-tree encoder against source DST frames while preserving
    /// the original professional payloads.
    pub fn analyze_interleaved_frame_with_options(
        &mut self,
        interleaved_dsd: &[u8],
        options: &DstEncoderOptions,
    ) -> io::Result<()> {
        let (encoded, telemetry) = encode_frame_interleaved_with_rate_and_telemetry(
            interleaved_dsd,
            self.channel_count,
            self.rate,
            options,
        );
        self.stats.record_encode_attempt(&telemetry)?;
        match encoded {
            Ok(_) => Ok(()),
            Err(err) => match err {
                DstEncodeError::InvalidChannelCount { .. }
                | DstEncodeError::InvalidFrameLength { .. }
                | DstEncodeError::InvalidPredictionOrder { .. } => Err(invalid_input(err)),
                _ => Ok(()),
            },
        }
    }

    /// Structured aggregate and per-frame telemetry collected so far.
    pub fn stats(&self) -> &DffDstWriterStats {
        &self.stats
    }

    /// Per-frame selected-mode telemetry for frames physically written to `DSTF`.
    pub fn frame_telemetry(&self) -> &[DffDstFrameTelemetry] {
        &self.stats.frames
    }

    /// Number of frames accepted from [`Self::write_interleaved_frame`] as
    /// predictive DST-coded payloads.
    pub fn predictive_frames_written(&self) -> u64 {
        self.stats.predictive_frames_written
    }

    /// Number of frames accepted as raw DST syntax payloads after explicit
    /// opt-in fallback.
    pub fn raw_frames_written(&self) -> u64 {
        self.stats.raw_frames_written
    }

    /// Number of frames accepted as source-DST passthrough payloads.
    pub fn passthrough_frames_written(&self) -> u64 {
        self.stats.passthrough_frames_written
    }

    /// Write an already-DST-coded source frame without re-encoding it.
    ///
    /// This is the preferred path when extracting a DST-coded SACD ISO area to
    /// DSDIFF/DST. The original DST frame bytes are stored verbatim as the
    /// `DSTF` payload, while `decoded_interleaved_dsd_for_crc` is used only for
    /// the mandatory `DSTC` checksum. The decoded DSD must represent exactly
    /// the same full frame as `encoded_dst_frame`; callers normally obtain it
    /// by decode-verifying the source payload before calling this method.
    pub fn write_passthrough_frame(
        &mut self,
        encoded_dst_frame: &[u8],
        decoded_interleaved_dsd_for_crc: &[u8],
    ) -> io::Result<()> {
        self.write_encoded_frame_payload(encoded_dst_frame, decoded_interleaved_dsd_for_crc)?;
        let frame_index = self.last_written_frame_index()?;
        self.stats.record_plain_frame(
            frame_index,
            DffDstFrameMode::SourceDstPassthrough,
            decoded_interleaved_dsd_for_crc.len(),
            encoded_dst_frame.len(),
        )?;
        Ok(())
    }

    /// Write a caller-supplied encoded DST frame and its checksum source.
    ///
    /// `encoded_dst_frame` is the raw DST frame payload stored in `DSTF`.
    /// `interleaved_dsd_for_crc` must be the exact decoded DSD represented by
    /// the frame and is used for the DSDIFF `DSTC` checksum. It must be one full
    /// frame long for the writer's channel count. Because this method does not
    /// run the encoder, the per-frame mode is recorded as
    /// [`DffDstFrameMode::CallerSuppliedEncoded`].
    pub fn write_encoded_frame(
        &mut self,
        encoded_dst_frame: &[u8],
        interleaved_dsd_for_crc: &[u8],
    ) -> io::Result<()> {
        self.write_encoded_frame_payload(encoded_dst_frame, interleaved_dsd_for_crc)?;
        let frame_index = self.last_written_frame_index()?;
        self.stats.record_plain_frame(
            frame_index,
            DffDstFrameMode::CallerSuppliedEncoded,
            interleaved_dsd_for_crc.len(),
            encoded_dst_frame.len(),
        )?;
        Ok(())
    }

    fn write_encoded_frame_payload(
        &mut self,
        encoded_dst_frame: &[u8],
        interleaved_dsd_for_crc: &[u8],
    ) -> io::Result<()> {
        let expected = dst_interleaved_frame_len_for_rate(self.rate, self.channel_count).map_err(invalid_input)?;
        if interleaved_dsd_for_crc.len() != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "DST CRC source has {} bytes; expected {}",
                    interleaved_dsd_for_crc.len(),
                    expected
                ),
            ));
        }
        if encoded_dst_frame.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "encoded DST frame must not be empty",
            ));
        }

        let frame_offset_in_dst = self.dst_payload_bytes;
        let dstf_total = checked_chunk_total(encoded_dst_frame.len() as u64)?;
        let dstf_total_u32 = u32::try_from(dstf_total).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("DSTF chunk total {} exceeds DSTI u32 field", dstf_total),
            )
        })?;
        self.frame_index.push((frame_offset_in_dst, dstf_total_u32));

        let written_dstf = write_chunk(&mut self.writer, DSTF, encoded_dst_frame)?;
        debug_assert_eq!(written_dstf, u64::from(dstf_total_u32));

        let crc = dst_frame_crc(interleaved_dsd_for_crc);
        let written_dstc = write_chunk(&mut self.writer, DSTC, &crc.to_be_bytes())?;
        debug_assert_eq!(written_dstc, CHUNK_HEADER_SIZE + 4);

        self.dst_payload_bytes = self
            .dst_payload_bytes
            .checked_add(written_dstf)
            .and_then(|n| n.checked_add(written_dstc))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "DST chunk size overflow"))?;
        self.frames_written = self
            .frames_written
            .checked_add(1)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "DST frame count overflow"))?;
        Ok(())
    }

    fn last_written_frame_index(&self) -> io::Result<u64> {
        self.frames_written.checked_sub(1).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "DST frame telemetry requested before any frame was written",
            )
        })
    }

    /// Finalize the file by appending `DSTI` and patching `FRM8`, `DST `, and
    /// `FRTE` fields.
    pub fn finish(mut self) -> io::Result<()> {
        let frame_count = u32::try_from(self.frames_written).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} DST frames exceed FRTE u32 frame-count field", self.frames_written),
            )
        })?;

        let dsti_capacity = self.frame_index.len().checked_mul(12).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "DSTI capacity overflow")
        })?;
        let mut dsti = Vec::with_capacity(dsti_capacity);
        for &(offset, size) in &self.frame_index {
            dsti.extend_from_slice(&offset.to_be_bytes());
            dsti.extend_from_slice(&size.to_be_bytes());
        }
        write_chunk(&mut self.writer, DSTI, &dsti)?;
        if let Some(ref footer) = self.footer_bytes {
            self.writer.write_all(footer)?;
        }

        let end_pos = self.writer.stream_position()?;
        let frm8_size = end_pos
            .checked_sub(CHUNK_HEADER_SIZE)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "FRM8 size underflow"))?;

        self.writer.seek(SeekFrom::Start(self.frm8_size_offset))?;
        self.writer.write_all(&frm8_size.to_be_bytes())?;
        self.writer.seek(SeekFrom::Start(self.dst_size_offset))?;
        self.writer.write_all(&self.dst_payload_bytes.to_be_bytes())?;
        self.writer.seek(SeekFrom::Start(self.frte_frame_count_offset))?;
        self.writer.write_all(&frame_count.to_be_bytes())?;
        self.writer.seek(SeekFrom::Start(end_pos))?;
        self.writer.flush()
    }
}

/// Compute the DSDIFF DST frame checksum (`DSTC`) over MSB-first interleaved
/// DSD bytes.
///
/// The generator polynomial is `x^32 + x^31 + x^4 + 1`, represented as
/// `0x80000011` with the implicit `x^32` term omitted from the feedback step.
pub fn dst_frame_crc(interleaved_dsd: &[u8]) -> u32 {
    const POLY: u32 = 0x8000_0011;
    let mut crc = 0u32;
    for &byte in interleaved_dsd {
        for bit in (0..8).rev() {
            let input = u32::from((byte >> bit) & 1);
            let feedback = (crc >> 31) ^ input;
            crc = (crc << 1) ^ if feedback != 0 { POLY } else { 0 };
        }
    }
    crc
}

fn serialize_prop_payload(channel_count: u8, sample_rate: u32) -> io::Result<Vec<u8>> {
    let mut prop = Vec::new();
    prop.extend_from_slice(SND);

    append_chunk(&mut prop, FS, &sample_rate.to_be_bytes())?;

    let mut chnl = Vec::with_capacity(2 + 4 * usize::from(channel_count));
    chnl.extend_from_slice(&(channel_count as u16).to_be_bytes());
    write_channel_ids(&mut chnl, channel_count);
    append_chunk(&mut prop, CHNL, &chnl)?;

    let mut cmpr = Vec::with_capacity(4 + 1 + CMPR_NAME.len());
    cmpr.extend_from_slice(DST);
    cmpr.push(CMPR_NAME.len() as u8);
    cmpr.extend_from_slice(CMPR_NAME);
    append_chunk(&mut prop, CMPR, &cmpr)?;

    append_chunk(&mut prop, LSCO, &loudspeaker_config(channel_count).to_be_bytes())?;
    Ok(prop)
}

fn append_chunk(buf: &mut Vec<u8>, id: &[u8; 4], data: &[u8]) -> io::Result<()> {
    buf.extend_from_slice(id);
    buf.extend_from_slice(&(data.len() as u64).to_be_bytes());
    buf.extend_from_slice(data);
    if data.len() & 1 != 0 {
        buf.push(0);
    }
    Ok(())
}

fn write_chunk<W: Write>(writer: &mut W, id: &[u8; 4], data: &[u8]) -> io::Result<u64> {
    writer.write_all(id)?;
    writer.write_all(&(data.len() as u64).to_be_bytes())?;
    writer.write_all(data)?;
    if data.len() & 1 != 0 {
        writer.write_all(&[0])?;
    }
    checked_chunk_total(data.len() as u64)
}

fn checked_chunk_total(data_len: u64) -> io::Result<u64> {
    data_len
        .checked_add(data_len & 1)
        .and_then(|n| n.checked_add(CHUNK_HEADER_SIZE))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "DSDIFF chunk size overflow"))
}

fn checked_add_u64(lhs: u64, rhs: u64, msg: &'static str) -> io::Result<u64> {
    lhs.checked_add(rhs)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, msg))
}

fn usize_to_u64(value: usize) -> io::Result<u64> {
    u64::try_from(value).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("usize value {} exceeds u64", value),
        )
    })
}

fn duration_div(total: Duration, divisor: u64) -> Duration {
    if divisor == 0 {
        return Duration::from_nanos(0);
    }
    let nanos = total.as_nanos() / u128::from(divisor);
    Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64)
}

fn compression_ratio_milli(raw_bytes: u64, encoded_bytes: u64) -> u32 {
    if encoded_bytes == 0 {
        return u32::MAX;
    }
    let milli = (u128::from(raw_bytes) * 1000) / u128::from(encoded_bytes);
    milli.min(u128::from(u32::MAX)) as u32
}

fn invalid_input<E: std::fmt::Display>(err: E) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, err.to_string())
}

fn write_channel_ids(buf: &mut Vec<u8>, channel_count: u8) {
    match channel_count {
        2 => {
            buf.extend_from_slice(SLFT);
            buf.extend_from_slice(SRGT);
        }
        5 => {
            buf.extend_from_slice(MLFT);
            buf.extend_from_slice(MRGT);
            buf.extend_from_slice(C_ID);
            buf.extend_from_slice(LS_ID);
            buf.extend_from_slice(RS_ID);
        }
        6 => {
            buf.extend_from_slice(MLFT);
            buf.extend_from_slice(MRGT);
            buf.extend_from_slice(C_ID);
            buf.extend_from_slice(LFE_ID);
            buf.extend_from_slice(LS_ID);
            buf.extend_from_slice(RS_ID);
        }
        n => {
            for i in 0..n {
                let id = [b'C', b'0' + (i / 100), b'0' + ((i / 10) % 10), b'0' + (i % 10)];
                buf.extend_from_slice(&id);
            }
        }
    }
}

fn loudspeaker_config(channel_count: u8) -> u16 {
    match channel_count {
        2 => LS_CONFIG_2_CHNL,
        5 => LS_CONFIG_5_CHNL,
        6 => LS_CONFIG_6_CHNL,
        _ => LS_CONFIG_UNDEFINED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::{inspect_dsd_container, DsdCompression, DsdContainerFormat};
    use crate::dst::{
        decode_frame, decode_frame_with_rate, dst_interleaved_frame_len,
        dst_interleaved_frame_len_for_rate, encode_uncompressed_frame_interleaved_with_rate,
        is_legal_dst_channel_count, supports_dst_policy, supports_predictive_dst_channel_count,
        supports_raw_dst_fallback_channel_count, supports_verified_dst_channel_count,
    };
    use std::io::{self, Cursor, Read, Seek, SeekFrom};

    fn read_u32_be(buf: &[u8], off: usize) -> u32 {
        u32::from_be_bytes(buf[off..off + 4].try_into().unwrap())
    }

    fn read_u64_be(buf: &[u8], off: usize) -> u64 {
        u64::from_be_bytes(buf[off..off + 8].try_into().unwrap())
    }

    #[derive(Debug, Clone, Copy)]
    struct ChunkView {
        id: [u8; 4],
        start: usize,
        payload_start: usize,
        payload_len: usize,
        total_len: usize,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct DstiEntry {
        offset_in_dst_payload: u64,
        dstf_total_size: u32,
    }

    fn parse_chunks_in_range(bytes: &[u8], mut pos: usize, end: usize) -> Vec<ChunkView> {
        let mut chunks = Vec::new();
        while pos < end {
            assert!(pos + 12 <= end, "truncated chunk header at {pos} before {end}");
            let payload_len = read_u64_be(bytes, pos + 4) as usize;
            let total_len = 12usize
                .checked_add(payload_len)
                .and_then(|n| n.checked_add(payload_len & 1))
                .expect("test chunk length overflow");
            assert!(
                pos + total_len <= end,
                "chunk {:?} at {pos} extends past range end {end}",
                std::str::from_utf8(&bytes[pos..pos + 4]).unwrap_or("????")
            );
            chunks.push(ChunkView {
                id: bytes[pos..pos + 4].try_into().unwrap(),
                start: pos,
                payload_start: pos + 12,
                payload_len,
                total_len,
            });
            pos += total_len;
        }
        assert_eq!(pos, end);
        chunks
    }

    fn parse_top_level_chunks(bytes: &[u8]) -> Vec<ChunkView> {
        assert!(bytes.len() >= 16, "DSDIFF file shorter than FRM8 form header");
        assert_eq!(&bytes[0..4], FRM8);
        let frm8_size = read_u64_be(bytes, 4) as usize;
        assert_eq!(
            frm8_size + CHUNK_HEADER_SIZE as usize,
            bytes.len(),
            "FRM8 size does not match physical file length"
        );
        assert_eq!(&bytes[12..16], DSD);
        parse_chunks_in_range(bytes, 16, bytes.len())
    }

    fn find_top_level_chunk(bytes: &[u8], id: &[u8; 4]) -> ChunkView {
        parse_top_level_chunks(bytes)
            .into_iter()
            .find(|chunk| &chunk.id == id)
            .unwrap_or_else(|| {
                panic!(
                    "missing top-level chunk {:?}",
                    std::str::from_utf8(id).unwrap()
                )
            })
    }

    fn parse_dsti_entries(dsti_payload: &[u8]) -> Vec<DstiEntry> {
        assert_eq!(
            dsti_payload.len() % 12,
            0,
            "DSTI payload must be an integral sequence of 12-byte index entries"
        );
        let mut entries = Vec::new();
        for off in (0..dsti_payload.len()).step_by(12) {
            entries.push(DstiEntry {
                offset_in_dst_payload: read_u64_be(dsti_payload, off),
                dstf_total_size: read_u32_be(dsti_payload, off + 8),
            });
        }
        entries
    }


    #[test]
    fn dst_channel_policy_split_is_explicit() {
        for channel_count in 1..=6 {
            assert!(
                is_legal_dst_channel_count(channel_count),
                "legal container/decode/passthrough channel count rejected: {}",
                channel_count
            );
            assert!(
                supports_raw_dst_fallback_channel_count(channel_count),
                "legal raw-fallback channel count rejected: {}",
                channel_count
            );
        }
        for channel_count in [0, 7] {
            assert!(!is_legal_dst_channel_count(channel_count));
            assert!(!supports_raw_dst_fallback_channel_count(channel_count));
            assert!(!supports_predictive_dst_channel_count(channel_count));
            assert!(!supports_verified_dst_channel_count(channel_count));
        }

        assert!(supports_predictive_dst_channel_count(2));
        assert!(supports_predictive_dst_channel_count(6));
        assert!(supports_verified_dst_channel_count(2));
        assert!(supports_verified_dst_channel_count(6));
        for channel_count in [1, 3, 4, 5] {
            assert!(is_legal_dst_channel_count(channel_count));
            assert!(supports_raw_dst_fallback_channel_count(channel_count));
            assert!(!supports_predictive_dst_channel_count(channel_count));
            assert!(!supports_verified_dst_channel_count(channel_count));
        }
    }

    #[test]
    fn writer_accepts_legal_dst_channel_counts_independently_of_predictive_scope() {
        for channel_count in 1..=6 {
            let mut cursor = Cursor::new(Vec::<u8>::new());
            let writer = DffDstWriter::new(&mut cursor, channel_count, SACD_SAMPLING_FREQUENCY)
                .unwrap_or_else(|err| {
                    panic!("legal DST channel count {} rejected: {}", channel_count, err)
                });
            writer.finish().unwrap();
        }

        for channel_count in [0, 7] {
            let mut cursor = Cursor::new(Vec::<u8>::new());
            let err = match DffDstWriter::new(&mut cursor, channel_count, SACD_SAMPLING_FREQUENCY) {
                Ok(_) => panic!("unexpectedly accepted {} channel(s)", channel_count),
                Err(err) => err,
            };
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
            let msg = err.to_string();
            assert!(
                msg.contains("1 through 6") || msg.contains("1..=6") || msg.contains("expected 1"),
                "unexpected error for {} channel(s): {}",
                channel_count,
                err
            );
        }
    }

    #[test]
    fn writer_rejects_unsupported_dst_sample_rate() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let err = match DffDstWriter::new(&mut cursor, 2, 96_000) {
            Ok(_) => panic!("unexpectedly accepted unsupported sample rate"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("unsupported DST sample rate"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn writer_policy_matrix_is_rate_aware() {
        for rate in [DstRate::Dsd64, DstRate::Dsd128, DstRate::Dsd256] {
            for channel_count in 1..=6 {
                assert!(supports_dst_policy(DstPolicyScope::Container, rate, channel_count));
                assert!(supports_dst_policy(DstPolicyScope::SourceDstPassthrough, rate, channel_count));
                assert!(supports_dst_policy(DstPolicyScope::CallerSuppliedDst, rate, channel_count));
                assert!(supports_dst_policy(DstPolicyScope::RawFallback, rate, channel_count));
            }
        }
        assert!(supports_dst_policy(DstPolicyScope::PredictiveGeneration, DstRate::Dsd64, 2));
        assert!(supports_dst_policy(DstPolicyScope::PredictiveGeneration, DstRate::Dsd64, 6));
        assert!(!supports_dst_policy(DstPolicyScope::PredictiveGeneration, DstRate::Dsd128, 2));
        assert!(!supports_dst_policy(DstPolicyScope::PredictiveGeneration, DstRate::Dsd256, 6));
    }

    #[test]
    fn writer_uses_dsd128_and_dsd256_frame_geometry_for_caller_supplied_dst() {
        for (rate, sample_rate) in [
            (DstRate::Dsd128, 5_644_800),
            (DstRate::Dsd256, 11_289_600),
        ] {
            let frame = vec![0u8; dst_interleaved_frame_len_for_rate(rate, 2).unwrap()];
            let encoded = encode_uncompressed_frame_interleaved_with_rate(&frame, 2, rate).unwrap();
            let mut cursor = Cursor::new(Vec::<u8>::new());
            {
                let mut writer = DffDstWriter::new(&mut cursor, 2, sample_rate).unwrap();
                writer.write_encoded_frame(&encoded, &frame).unwrap();
                assert_eq!(writer.stats().caller_supplied_frames_written, 1);
                assert_eq!(writer.stats().predictive_frames_written, 0);
                assert_eq!(writer.stats().raw_frames_written, 0);
                writer.finish().unwrap();
            }
            let bytes = cursor.into_inner();
            let dstf = bytes.windows(4).position(|w| w == b"DSTF").unwrap();
            let dstf_size = read_u64_be(&bytes, dstf + 4) as usize;
            let dstf_payload = &bytes[dstf + 12..dstf + 12 + dstf_size];
            assert_eq!(decode_frame_with_rate(dstf_payload, 2, rate).unwrap(), frame);
        }
    }

    #[test]
    fn writer_accepts_higher_rate_source_dst_passthrough_without_reencoding() {
        let rate = DstRate::Dsd128;
        let frame = vec![0u8; dst_interleaved_frame_len_for_rate(rate, 2).unwrap()];
        let encoded = encode_uncompressed_frame_interleaved_with_rate(&frame, 2, rate).unwrap();
        let mut cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, rate.sample_rate()).unwrap();
            writer.write_passthrough_frame(&encoded, &frame).unwrap();
            assert_eq!(writer.stats().passthrough_frames_written, 1);
            assert_eq!(writer.stats().caller_supplied_frames_written, 0);
            assert_eq!(writer.stats().encode_attempts, 0);
            assert_eq!(writer.stats().predictive_frames_written, 0);
            assert_eq!(writer.stats().raw_frames_written, 0);
            writer.finish().unwrap();
        }
        let bytes = cursor.into_inner();
        let dstf = bytes.windows(4).position(|w| w == b"DSTF").unwrap();
        let dstf_size = read_u64_be(&bytes, dstf + 4) as usize;
        let dstf_payload = &bytes[dstf + 12..dstf + 12 + dstf_size];
        assert_eq!(dstf_payload, encoded.as_slice());
        assert_eq!(decode_frame_with_rate(dstf_payload, 2, rate).unwrap(), frame);
    }

    #[test]
    fn writer_rejects_dsd64_sized_crc_source_for_higher_rate_dst() {
        let rate = DstRate::Dsd128;
        let sample_rate = rate.sample_rate();
        let frame = vec![0u8; dst_interleaved_frame_len_for_rate(rate, 2).unwrap()];
        let encoded = encode_uncompressed_frame_interleaved_with_rate(&frame, 2, rate).unwrap();
        let dsd64_sized_crc_source = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let mut writer = DffDstWriter::new(&mut cursor, 2, sample_rate).unwrap();
        let err = writer
            .write_encoded_frame(&encoded, &dsd64_sized_crc_source)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains(&frame.len().to_string()),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn higher_rate_predictive_writer_rejects_without_implicit_raw_fallback() {
        let rate = DstRate::Dsd128;
        let frame = vec![0u8; dst_interleaved_frame_len_for_rate(rate, 2).unwrap()];
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let mut writer = DffDstWriter::new(&mut cursor, 2, rate.sample_rate()).unwrap();
        let err = writer.write_interleaved_frame(&frame).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("predictive DST generation for 2 channel(s) at 5644800 Hz"),
            "unexpected error: {}",
            err
        );
        assert_eq!(writer.predictive_frames_written(), 0);
        assert_eq!(writer.raw_frames_written(), 0);

        let mut options = DstEncoderOptions::default();
        options.raw_fallback = RawDstFallbackPolicy::Enabled;
        writer.write_interleaved_frame_with_options(&frame, &options).unwrap();
        assert_eq!(writer.raw_frames_written(), 1);
    }

    #[test]
    fn mono_raw_fallback_is_allowed_only_when_explicitly_requested() {
        let frame = vec![0u8; dst_interleaved_frame_len(1).unwrap()];
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let mut writer = DffDstWriter::new(&mut cursor, 1, SACD_SAMPLING_FREQUENCY)
            .expect("legal mono DST container should be accepted");

        let err = writer
            .write_interleaved_frame_with_options(&frame, &DstEncoderOptions::default())
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("predictive DST generation for 1 channel"),
            "unexpected error: {}",
            err
        );
        assert_eq!(writer.raw_frames_written(), 0);

        let options = DstEncoderOptions {
            raw_fallback: RawDstFallbackPolicy::Enabled,
            ..DstEncoderOptions::default()
        };
        writer.write_interleaved_frame_with_options(&frame, &options).unwrap();
        assert_eq!(writer.raw_frames_written(), 1);
        assert_eq!(writer.predictive_frames_written(), 0);
        assert_eq!(
            writer.frame_telemetry()[0].mode,
            DffDstFrameMode::RawFallback
        );
        writer.finish().unwrap();
    }

    #[test]
    fn five_channel_generation_uses_only_explicit_raw_fallback() {
        let frame = vec![0u8; dst_interleaved_frame_len(5).unwrap()];
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let mut writer = DffDstWriter::new(&mut cursor, 5, SACD_SAMPLING_FREQUENCY)
            .expect("5-channel DST container should be legal");

        let err = writer.write_interleaved_frame(&frame).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("predictive DST generation for 5 channel"),
            "unexpected error: {}",
            err
        );
        assert_eq!(writer.predictive_frames_written(), 0);
        assert_eq!(writer.raw_frames_written(), 0);

        writer
            .write_interleaved_frame_allowing_raw_fallback(&frame)
            .expect("explicit raw fallback should be accepted for legal 5-channel DST");
        assert_eq!(writer.predictive_frames_written(), 0);
        assert_eq!(writer.raw_frames_written(), 1);
        assert_eq!(
            writer.frame_telemetry()[0].mode,
            DffDstFrameMode::RawFallback
        );
        writer.finish().unwrap();
    }

    #[test]
    fn five_channel_passthrough_preserves_caller_supplied_dstf_payload() {
        let frame = vec![0u8; dst_interleaved_frame_len(5).unwrap()];
        let encoded = crate::dst::encode_uncompressed_frame_interleaved(&frame, 5)
            .expect("raw DST helper should support legal 5-channel frames");
        let mut cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 5, SACD_SAMPLING_FREQUENCY)
                .expect("5-channel DST passthrough container should be accepted");
            writer.write_passthrough_frame(&encoded, &frame).unwrap();
            assert_eq!(writer.passthrough_frames_written(), 1);
            assert_eq!(writer.predictive_frames_written(), 0);
            assert_eq!(writer.raw_frames_written(), 0);
            assert_eq!(
                writer.frame_telemetry()[0].mode,
                DffDstFrameMode::SourceDstPassthrough
            );
            writer.finish().unwrap();
        }

        let bytes = cursor.into_inner();
        let mut inspect_cursor = Cursor::new(bytes.clone());
        let info = inspect_dsd_container(&mut inspect_cursor).unwrap();
        assert_eq!(info.format, DsdContainerFormat::Dsdiff);
        assert_eq!(info.compression, DsdCompression::Dst);
        assert_eq!(info.channel_count, 5);

        let dstf = bytes.windows(4).position(|w| w == b"DSTF").unwrap();
        let dstf_size = read_u64_be(&bytes, dstf + 4) as usize;
        let dstf_payload = &bytes[dstf + 12..dstf + 12 + dstf_size];
        assert_eq!(dstf_payload, encoded.as_slice());
        assert_eq!(decode_frame(dstf_payload, 5).unwrap(), frame);
    }

    #[test]
    fn crc_known_empty_and_single_byte_values() {
        assert_eq!(dst_frame_crc(&[]), 0);
        assert_eq!(dst_frame_crc(&[0x00]), 0);
        assert_eq!(dst_frame_crc(&[0x80]), 0x8000_0f0f);
    }

    #[test]
    fn writes_inspectable_dst_dsdiff() {
        let frame = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let mut cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved_frame_allowing_raw_fallback(&frame).unwrap();
            writer.finish().unwrap();
        }

        cursor.set_position(0);
        let info = inspect_dsd_container(&mut cursor).unwrap();
        assert_eq!(info.format, DsdContainerFormat::Dsdiff);
        assert_eq!(info.compression, DsdCompression::Dst);
        assert_eq!(info.channel_count, 2);
        assert_eq!(info.sample_count_per_channel, Some(37_632));
        assert!(info.diagnostics.is_empty(), "{:?}", info.diagnostics);
    }

    #[test]
    fn first_dstf_payload_decodes_to_source_frame() {
        let frame = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let mut cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved_frame_allowing_raw_fallback(&frame).unwrap();
            writer.finish().unwrap();
        }
        let bytes = cursor.into_inner();
        let dstf = bytes.windows(4).position(|w| w == b"DSTF").unwrap();
        let dstf_size = read_u64_be(&bytes, dstf + 4) as usize;
        let dstf_payload = &bytes[dstf + 12..dstf + 12 + dstf_size];
        assert_eq!(decode_frame(dstf_payload, 2).unwrap(), frame);
    }


    #[test]
    fn passthrough_writer_preserves_caller_supplied_dstf_payload() {
        let frame = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let encoded = crate::dst::encode_predictive_frame_interleaved(
            &frame,
            2,
            &crate::dst::DstEncoderOptions {
                prediction_order: 1,
                ..crate::dst::DstEncoderOptions::default()
            },
        )
        .unwrap();
        let mut cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_passthrough_frame(&encoded, &frame).unwrap();
            assert_eq!(writer.passthrough_frames_written(), 1);
            assert_eq!(writer.predictive_frames_written(), 0);
            assert_eq!(writer.raw_frames_written(), 0);
            assert_eq!(writer.stats().frames_written, 1);
            assert_eq!(writer.stats().encode_attempts, 0);
            assert_eq!(writer.stats().total_raw_bytes, frame.len() as u64);
            assert_eq!(writer.stats().total_encoded_bytes, encoded.len() as u64);
            assert_eq!(
                writer.frame_telemetry()[0].mode,
                DffDstFrameMode::SourceDstPassthrough
            );
            assert_eq!(writer.frame_telemetry()[0].prediction_order, None);
            writer.finish().unwrap();
        }

        let bytes = cursor.into_inner();
        let dstf = bytes.windows(4).position(|w| w == b"DSTF").unwrap();
        let dstf_size = read_u64_be(&bytes, dstf + 4) as usize;
        let dstf_payload = &bytes[dstf + 12..dstf + 12 + dstf_size];
        assert_eq!(dstf_payload, encoded.as_slice());
        assert_eq!(decode_frame(dstf_payload, 2).unwrap(), frame);
    }


    #[test]
    fn default_interleaved_writer_rejects_unprofitable_synthetic_frame_without_raw_fallback() {
        let frame = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
        let err = writer.write_interleaved_frame(&frame).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(writer.predictive_frames_written(), 0);
        assert_eq!(writer.raw_frames_written(), 0);
        assert_eq!(writer.stats().frames_written, 0);
        assert_eq!(writer.stats().terminal_failures, 1);
    }

    #[test]
    fn explicit_raw_fallback_writer_emits_decodable_dstf_for_synthetic_frame() {
        let frame = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let mut cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved_frame_allowing_raw_fallback(&frame).unwrap();
            assert_eq!(writer.predictive_frames_written(), 0);
            assert_eq!(writer.raw_frames_written(), 1);
            assert_eq!(writer.stats().frames_written, 1);
            assert_eq!(writer.stats().total_raw_bytes, frame.len() as u64);
            assert_eq!(writer.stats().total_encoded_bytes, (frame.len() + 1) as u64);
            assert_eq!(writer.stats().encode_attempts, 1);
            let frame_stats = &writer.frame_telemetry()[0];
            assert_eq!(frame_stats.mode, DffDstFrameMode::RawFallback);
            assert_eq!(frame_stats.raw_fallback_reason, Some(DstEncodeFailureClass::CompressionNotBeneficial));
            writer.finish().unwrap();
        }

        let bytes = cursor.into_inner();
        let dstf = bytes.windows(4).position(|w| w == b"DSTF").unwrap();
        let dstf_size = read_u64_be(&bytes, dstf + 4) as usize;
        let dstf_payload = &bytes[dstf + 12..dstf + 12 + dstf_size];
        assert_eq!(dstf_payload[0], 0);
        assert_eq!(decode_frame(dstf_payload, 2).unwrap(), frame);
    }

    #[test]
    fn default_writer_rejects_raw_fallback_when_savings_margin_is_not_met() {
        let frame = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
        let options = DstEncoderOptions {
            minimum_savings_bytes: frame.len() + 1,
            ..DstEncoderOptions::default()
        };
        let err = writer
            .write_interleaved_frame_with_options(&frame, &options)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(writer.predictive_frames_written(), 0);
        assert_eq!(writer.raw_frames_written(), 0);
        assert_eq!(writer.stats().frames_written, 0);
        assert_eq!(writer.stats().encode_attempts, 1);
        assert!(writer.stats().unprofitable_predictive_candidates > 0);
    }

    #[test]
    fn raw_dst_fallback_requires_explicit_encoder_option() {
        let frame = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let mut cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            let options = DstEncoderOptions {
                minimum_savings_bytes: frame.len() + 1,
                raw_fallback: RawDstFallbackPolicy::Enabled,
                ..DstEncoderOptions::default()
            };
            writer
                .write_interleaved_frame_with_options(&frame, &options)
                .unwrap();
            assert_eq!(writer.predictive_frames_written(), 0);
            assert_eq!(writer.raw_frames_written(), 1);
            assert_eq!(writer.stats().frames_written, 1);
            assert_eq!(writer.stats().encode_attempts, 1);
            assert_eq!(
                writer.frame_telemetry()[0].mode,
                DffDstFrameMode::RawFallback
            );
            assert_eq!(writer.frame_telemetry()[0].encoded_bytes, (frame.len() + 1) as u64);
            writer.finish().unwrap();
        }
        let bytes = cursor.into_inner();
        let dstf = bytes.windows(4).position(|w| w == b"DSTF").unwrap();
        let dstf_size = read_u64_be(&bytes, dstf + 4) as usize;
        let dstf_payload = &bytes[dstf + 12..dstf + 12 + dstf_size];
        assert_eq!(dstf_payload[0], 0);
        assert_eq!(decode_frame(dstf_payload, 2).unwrap(), frame);
    }

    #[test]
    fn writer_stats_record_raw_fallback_reason_class() {
        let frame = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
        let options = DstEncoderOptions {
            minimum_savings_bytes: frame.len() + 1,
            raw_fallback: RawDstFallbackPolicy::Enabled,
            ..DstEncoderOptions::default()
        };
        writer.write_interleaved_frame_with_options(&frame, &options).unwrap();
        assert_eq!(writer.stats().raw_fallbacks_after_non_verification_failure, 1);
        assert_eq!(writer.stats().raw_fallbacks_after_verification_failure, 0);
        assert_eq!(
            writer.frame_telemetry()[0].raw_fallback_reason,
            Some(DstEncodeFailureClass::CompressionNotBeneficial)
        );
    }

    #[test]
    fn rejected_writer_encode_records_terminal_failure_class() {
        let frame = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
        let options = DstEncoderOptions {
            minimum_savings_bytes: frame.len() + 1,
            ..DstEncoderOptions::default()
        };
        let err = writer.write_interleaved_frame_with_options(&frame, &options).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(writer.stats().terminal_failures, 1);
        assert_eq!(
            writer.stats().last_terminal_error,
            Some(DstEncodeFailureClass::CompressionNotBeneficial)
        );
    }

    #[test]
    fn analysis_mode_records_encode_attempt_without_writing_frame() {
        let frame = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
        writer
            .analyze_interleaved_frame_with_options(&frame, &DstEncoderOptions::default())
            .unwrap();
        assert_eq!(writer.stats().frames_written, 0);
        assert_eq!(writer.stats().encode_attempts, 1);
        assert!(writer.stats().predictive_candidates > 0);
        assert!(writer.frame_telemetry().is_empty());
    }

    #[test]
    fn dsti_entries_match_every_dstf_physical_offset_size_and_payload_padding() {
        let frame = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let encoded_frames = vec![
            vec![0x81, 0x02, 0x03],
            vec![0x84, 0x05, 0x06, 0x07, 0x08, 0x09],
            vec![0x8a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10],
        ];

        let mut cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            for encoded in &encoded_frames {
                writer.write_encoded_frame(encoded, &frame).unwrap();
            }
            writer.finish().unwrap();
        }

        let bytes = cursor.into_inner();
        let dst = find_top_level_chunk(&bytes, DST);
        let dsti = find_top_level_chunk(&bytes, DSTI);
        assert_eq!(dsti.payload_len, encoded_frames.len() * 12);

        let dst_subchunks =
            parse_chunks_in_range(&bytes, dst.payload_start, dst.payload_start + dst.payload_len);
        assert_eq!(&dst_subchunks[0].id, FRTE);
        assert_eq!(dst_subchunks.len(), 1 + encoded_frames.len() * 2);
        for frame_idx in 0..encoded_frames.len() {
            assert_eq!(&dst_subchunks[1 + frame_idx * 2].id, DSTF);
            assert_eq!(&dst_subchunks[2 + frame_idx * 2].id, DSTC);
        }

        let dstf_chunks: Vec<_> = dst_subchunks
            .iter()
            .copied()
            .filter(|chunk| &chunk.id == DSTF)
            .collect();
        assert_eq!(dstf_chunks.len(), encoded_frames.len());

        let entries =
            parse_dsti_entries(&bytes[dsti.payload_start..dsti.payload_start + dsti.payload_len]);
        assert_eq!(entries.len(), encoded_frames.len());

        for (idx, ((entry, chunk), expected_payload)) in entries
            .iter()
            .zip(dstf_chunks.iter())
            .zip(encoded_frames.iter())
            .enumerate()
        {
            let expected_physical_offset =
                dst.payload_start + entry.offset_in_dst_payload as usize;
            assert_eq!(
                expected_physical_offset, chunk.start,
                "DSTI entry {idx} does not point at the physical DSTF chunk start"
            );
            assert_eq!(&bytes[expected_physical_offset..expected_physical_offset + 4], DSTF);

            let mut seek_cursor = Cursor::new(bytes.as_slice());
            seek_cursor
                .seek(SeekFrom::Start(expected_physical_offset as u64))
                .unwrap();
            let mut chunk_id = [0u8; 4];
            seek_cursor.read_exact(&mut chunk_id).unwrap();
            assert_eq!(&chunk_id, DSTF, "DSTI entry {idx} seek did not land on DSTF");
            let mut size_bytes = [0u8; 8];
            seek_cursor.read_exact(&mut size_bytes).unwrap();
            let physical_payload_len = u64::from_be_bytes(size_bytes) as usize;

            assert_eq!(physical_payload_len, expected_payload.len());
            assert_eq!(chunk.payload_len, expected_payload.len());
            assert_eq!(
                entry.dstf_total_size as usize, chunk.total_len,
                "DSTI entry {idx} size does not match padded DSTF physical chunk length"
            );
            assert_eq!(
                entry.dstf_total_size as usize,
                12 + expected_payload.len() + (expected_payload.len() & 1)
            );
            assert_eq!(
                &bytes[chunk.payload_start..chunk.payload_start + chunk.payload_len],
                expected_payload.as_slice()
            );
            if expected_payload.len() & 1 != 0 {
                assert_eq!(
                    bytes[chunk.payload_start + chunk.payload_len], 0,
                    "odd-sized DSTF payload {idx} is not padded on disk"
                );
            }
        }
    }

    #[test]
    fn finish_appends_metadata_footer_after_dsti_and_patches_frm8_only() {
        let frame = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let mut footer = Vec::new();
        footer.extend_from_slice(b"DIIN");
        footer.extend_from_slice(&4u64.to_be_bytes());
        footer.extend_from_slice(b"meta");

        let mut cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.set_footer_bytes(footer.clone());
            writer.write_interleaved_frame_allowing_raw_fallback(&frame).unwrap();
            writer.finish().unwrap();
        }

        let bytes = cursor.into_inner();
        assert_eq!(read_u64_be(&bytes, 4) as usize, bytes.len() - CHUNK_HEADER_SIZE as usize);

        let top = parse_top_level_chunks(&bytes);
        assert_eq!(&top[top.len() - 2].id, DSTI, "DSTI must precede metadata footer");
        assert_eq!(&top[top.len() - 1].id, b"DIIN", "metadata footer chunk must be top-level");
        assert_eq!(top[top.len() - 1].payload_len, 4);
        assert_eq!(
            &bytes[top[top.len() - 1].payload_start..top[top.len() - 1].payload_start + 4],
            b"meta"
        );

        let dst = find_top_level_chunk(&bytes, DST);
        let dst_subchunks =
            parse_chunks_in_range(&bytes, dst.payload_start, dst.payload_start + dst.payload_len);
        assert!(dst_subchunks.iter().any(|chunk| &chunk.id == DSTF));
        assert!(dst_subchunks.iter().any(|chunk| &chunk.id == DSTC));
        assert!(dst_subchunks.iter().all(|chunk| &chunk.id != DSTI));
        assert_eq!(bytes.ends_with(&footer), true);
    }

    #[test]
    fn finish_patches_frte_frame_count() {
        let frame = vec![0u8; dst_interleaved_frame_len(2).unwrap()];
        let mut cursor = Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffDstWriter::new(&mut cursor, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved_frame_allowing_raw_fallback(&frame).unwrap();
            writer.write_interleaved_frame_allowing_raw_fallback(&frame).unwrap();
            writer.finish().unwrap();
        }
        let bytes = cursor.into_inner();
        let frte = bytes.windows(4).position(|w| w == b"FRTE").unwrap();
        assert_eq!(&bytes[frte + 12..frte + 16], &2u32.to_be_bytes());
        assert_eq!(&bytes[frte + 16..frte + 18], &75u16.to_be_bytes());
    }

    #[test]
    fn user_facing_dst_summary_does_not_overclaim_compression() {
        let stats = DffDstWriterStats::default();
        let summary = stats.user_facing_summary();
        assert!(summary.contains("verified predictive subset"));
        assert!(summary.contains("not SACD-mastering parity"));
        assert!(summary.contains("broad external-corpus playback not assumed"));
        assert!(DFF_DST_CAPABILITY_STATEMENT.contains("Predictive generation"));
        assert!(DFF_DST_CAPABILITY_STATEMENT.contains("Raw DST fallback"));
    }

    #[test]
    fn production_acceptance_gate_requires_external_evidence() {
        let mut stats = DffDstWriterStats::default();
        stats.frames_written = 1;
        stats.total_raw_bytes = 9408;
        stats.total_encoded_bytes = 4096;
        stats.predictive_frames_written = 1;
        stats.predictive_candidates = 1;
        stats.verified_predictive_candidates = 1;
        stats.frames.push(DffDstFrameTelemetry {
            frame_index: 0,
            mode: DffDstFrameMode::Predictive,
            raw_bytes: 9408,
            encoded_bytes: 4096,
            prediction_order: Some(16),
            table_strategy: Some(DstTableStrategy::Shared),
            coefficient_scale: Some(255),
            coefficient_prune_threshold: Some(0),
            predictive_candidates: 1,
            verified_predictive_candidates: 1,
            verification_failures: 0,
            verification_decode_errors: 0,
            verification_mismatches: 0,
            last_verification_failure: None,
            terminal_error: None,
            raw_fallback_reason: None,
            unprofitable_predictive_candidates: 0,
            prescreen_rejected: false,
            prescreen_unique_bytes: 1,
            prescreen_transition_percent: 0,
            encode_time: Duration::from_millis(1),
            worst_expansion_avoided_bytes: 0,
        });

        let gate = DffDstAcceptanceGate::production_release();
        let report = stats.evaluate_acceptance_gate(&gate, DffDstAcceptanceEvidence::default());
        assert!(!report.accepted);
        assert!(report.failures.contains(&DffDstAcceptanceFailure::CanonicalOutputHashesNotPinned));
        assert!(report.failures.contains(&DffDstAcceptanceFailure::ExternalDecoderCorpusNotPassed));
        assert!(report.failures.contains(&DffDstAcceptanceFailure::ProvenanceReviewIncomplete));
        assert!(report.failures.contains(&DffDstAcceptanceFailure::PerformanceBudgetNotPassed));

        let evidence = DffDstAcceptanceEvidence {
            canonical_output_hashes_pinned: true,
            external_decoder_corpus_passed: true,
            provenance_review_complete: true,
            performance_budget_passed: true,
        };
        let report = stats.evaluate_acceptance_gate(&gate, evidence);
        assert!(report.accepted, "unexpected acceptance failures: {:?}", report.failures);
    }

    #[test]
    fn acceptance_gate_rejects_raw_fallback_and_unverified_predictive_frames() {
        let mut stats = DffDstWriterStats::default();
        stats.frames_written = 2;
        stats.total_raw_bytes = 18816;
        stats.total_encoded_bytes = 9500;
        stats.predictive_frames_written = 1;
        stats.raw_frames_written = 1;
        stats.frames.push(DffDstFrameTelemetry {
            frame_index: 0,
            mode: DffDstFrameMode::Predictive,
            raw_bytes: 9408,
            encoded_bytes: 4095,
            prediction_order: Some(16),
            table_strategy: Some(DstTableStrategy::Shared),
            coefficient_scale: Some(255),
            coefficient_prune_threshold: Some(0),
            predictive_candidates: 1,
            verified_predictive_candidates: 0,
            verification_failures: 0,
            verification_decode_errors: 0,
            verification_mismatches: 0,
            last_verification_failure: None,
            terminal_error: None,
            raw_fallback_reason: None,
            unprofitable_predictive_candidates: 0,
            prescreen_rejected: false,
            prescreen_unique_bytes: 1,
            prescreen_transition_percent: 0,
            encode_time: Duration::from_millis(1),
            worst_expansion_avoided_bytes: 0,
        });
        stats.frames.push(DffDstFrameTelemetry {
            frame_index: 1,
            mode: DffDstFrameMode::RawFallback,
            raw_bytes: 9408,
            encoded_bytes: 9409,
            prediction_order: None,
            table_strategy: None,
            coefficient_scale: None,
            coefficient_prune_threshold: None,
            predictive_candidates: 0,
            verified_predictive_candidates: 0,
            verification_failures: 0,
            verification_decode_errors: 0,
            verification_mismatches: 0,
            last_verification_failure: None,
            terminal_error: None,
            raw_fallback_reason: Some(DstEncodeFailureClass::CompressionNotBeneficial),
            unprofitable_predictive_candidates: 1,
            prescreen_rejected: false,
            prescreen_unique_bytes: 0,
            prescreen_transition_percent: 0,
            encode_time: Duration::from_millis(1),
            worst_expansion_avoided_bytes: 0,
        });
        let evidence = DffDstAcceptanceEvidence {
            canonical_output_hashes_pinned: true,
            external_decoder_corpus_passed: true,
            provenance_review_complete: true,
            performance_budget_passed: true,
        };
        let report = stats.evaluate_acceptance_gate(&DffDstAcceptanceGate::production_release(), evidence);
        assert!(!report.accepted);
        assert!(report.failures.contains(&DffDstAcceptanceFailure::RawFallbackDisallowed { frames: 1 }));
        assert!(report.failures.contains(&DffDstAcceptanceFailure::PredictiveFrameMissingVerification { frame_index: 0 }));
    }

}
