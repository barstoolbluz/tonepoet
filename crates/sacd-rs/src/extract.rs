//! Track extraction orchestration: read DSD frames from an ISO sector
//! range, demux/passthrough to the selected output format, write a
//! valid DSF or DSDIFF file.
//!
//! ## Scope
//!
//! This module handles uncompressed DSD frames and DST-encoded frames.
//! DST payloads are decoded into the same clustered-frame byte layout
//! that uncompressed SACD sectors already carry before being forwarded
//! to the DSF/DFF writers.
//!
//! ## Error semantics
//!
//! On any error mid-stream, the low-level writer is dropped without
//! calling `finish()`. Callers that pass their own `Write + Seek` sink remain
//! responsible for discarding partial output. User-facing path APIs such as
//! [`extract_track_to_path`] and [`write_dsd_source_to_path`] wrap the same
//! writers in an [`crate::output_transaction::OutputTransaction`] so final
//! paths are published only after the full operation succeeds.
//!
//! ## Channel-count parameter
//!
//! `channel_count` must match the SACD area's actual channel layout.
//! Uncompressed DSD frames don't self-describe channel count
//! (`Frame.channel_count` is always 0 for uncompressed); the
//! orchestrator trusts the caller's parameter and never validates.

use crate::dff_footer::{render_dff_footer, DffMetadata};
use crate::dff_dst_writer::{DffDstWriter, DffDstWriterStats};
use crate::dff_writer::DffWriter;
use crate::dsf_writer::{DsfWriter, SACD_SAMPLING_FREQUENCY};
use crate::dst::{DstEncoderEffort, DstEncoderOptions, DstError, RawDstFallbackPolicy};
use crate::frame::{
    DroppedFrameEvent, FrameError, FrameFormat, FrameReaderStats, FrameTimeFilter, RecoveryEvent,
};
use crate::source_model::{
    DsdSource, DsdSourceError, DsdSourceFrame, IsoTrackSource, IsoTrackSourceOptions,
    SourceDsdFrame,
};
use crate::id3::{render_id3v24, Id3Metadata};
use crate::iso_reader::IsoReader;
use crate::output_transaction::{OutputOverwritePolicy, OutputTransaction, OutputTransactionError};
use std::io::{self, Seek, Write};
use std::path::{Path, PathBuf};

/// Output container format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Sony DSF (.dsf). Per-channel deinterleaved, LSB-first byte
    /// ordering, 4096-byte blocks per channel.
    Dsf,
    /// Philips DSDIFF (.dff). Clustered-frame passthrough,
    /// MSB-first byte ordering.
    Dff,
    /// Philips DSDIFF/DST (.dff with `CMPR = "DST "`). Source DST frames
    /// are preserved byte-for-byte whenever possible; plain DSD frames are
    /// encoded only when the verified predictive DST subset is profitable.
    /// This is lossless DST syntax with material-dependent compression, not
    /// compression parity with SACD mastering encoders. If compression is
    /// unavailable, extraction fails rather than emitting raw DST fallback
    /// frames with uncertain common-decoder portability. Use
    /// [`OutputFormat::Dff`] or [`SourceDstHandling::DecodeToDsdiffDsd`] for
    /// DSDIFF/DSD output in that case.
    DffDst,
}

/// Policy for source SACD frames that are already DST-coded.
///
/// The mode name describes the actual operation, including whether the output
/// remains DSDIFF/DST or is deliberately decoded to ordinary DSDIFF/DSD. The
/// default is [`Self::PassthroughExistingDst`]: preserve professional source
/// compression byte-for-byte and decode only to compute the mandatory DSDIFF
/// `DSTC` checksum. Re-encoding source DST is never implicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceDstHandling {
    /// Preserve the original source DST payload byte-for-byte and decode it
    /// only to compute the mandatory DSDIFF `DSTC` checksum. This is the
    /// archival/default behavior for DST-coded SACD areas.
    PassthroughExistingDst,
    /// Run the configured encoder against the decoded DSD for telemetry, but
    /// still write the original source DST payload. Use for corpus analysis
    /// when measuring the in-tree encoder against professional SACD frames.
    AnalyzeThenPassthroughExistingDst,
    /// Decode source DST to DSD and encode it again with the configured
    /// encoder policy. This is not recommended for archival extraction; use it
    /// only to evaluate or normalize encoder output.
    ReencodeDst,
    /// Decode source DST frames and write an ordinary DSDIFF/DSD stream instead
    /// of a DSDIFF/DST stream. This mode is a target-container policy, not an
    /// encoder fallback; extraction routes through `DffWriter` so the output
    /// declares `CMPR = "DSD "` and contains no `DSTF` chunks.
    DecodeToDsdiffDsd,
    /// Reject source DST frames instead of preserving, decoding, or re-encoding
    /// them. This is useful for tests that must prove the extraction path saw
    /// no source compression.
    RejectSourceDst,
}

#[allow(non_upper_case_globals)]
impl SourceDstHandling {
    /// Backward-compatible alias for [`Self::PassthroughExistingDst`].
    pub const Preserve: Self = Self::PassthroughExistingDst;
    /// Backward-compatible alias for [`Self::AnalyzeThenPassthroughExistingDst`].
    pub const AnalyzeThenPreserve: Self = Self::AnalyzeThenPassthroughExistingDst;
    /// Backward-compatible alias for [`Self::ReencodeDst`].
    pub const DecodeAndReencode: Self = Self::ReencodeDst;
    /// Backward-compatible alias for [`Self::RejectSourceDst`].
    pub const Reject: Self = Self::RejectSourceDst;
}

/// Policy for plain DSD source frames when the requested target is
/// DSDIFF/DST, or when a DFF/DST request is explicitly re-routed to ordinary
/// DSDIFF/DSD by [`SourceDstHandling::DecodeToDsdiffDsd`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlainDsdDstHandling {
    /// Encode plain DSD as verified predictive DST only. Raw DST fallback is
    /// disabled even if the embedded encoder options were built with it
    /// enabled. This is the portable production default for uncompressed SACD
    /// source areas.
    EncodeUncompressedSourceToDst,
    /// Encode plain DSD predictively, but permit explicit raw `DSTCoded = 0`
    /// fallback for controlled decoder sets. This mode is opt-in because common
    /// external-decoder portability of raw DST frames has not been proven.
    EncodeUncompressedSourceToDstWithRawFallback,
    /// Write plain DSD unchanged as ordinary DSDIFF/DSD. This state is valid
    /// only with [`SourceDstHandling::DecodeToDsdiffDsd`], where the whole
    /// request intentionally routes through [`DffWriter`] and emits `CMPR =
    /// "DSD "` rather than any `DSTF` chunks.
    WriteUncompressedSourceAsDsdiffDsd,
    /// Reject plain DSD frames. Use with
    /// [`SourceDstHandling::PassthroughExistingDst`] for a passthrough-only
    /// extraction profile.
    RejectUncompressedSource,
}

#[allow(non_upper_case_globals)]
impl PlainDsdDstHandling {
    /// Backward-compatible alias for [`Self::EncodeUncompressedSourceToDst`].
    pub const PredictiveOnly: Self = Self::EncodeUncompressedSourceToDst;
    /// Backward-compatible alias for [`Self::EncodeUncompressedSourceToDstWithRawFallback`].
    pub const PredictiveWithRawFallback: Self = Self::EncodeUncompressedSourceToDstWithRawFallback;
    /// Backward-compatible alias for [`Self::RejectUncompressedSource`].
    pub const Reject: Self = Self::RejectUncompressedSource;
}

/// DSDIFF/DST extraction policy and encoder mechanism.
///
/// The outer policy decides what to do with source DST and plain DSD frames;
/// [`DstEncoderOptions`] remains the mechanism that controls predictive search,
/// verification, candidate orders, quantization, pruning, and raw-fallback
/// eligibility. Preset constructors cover the common operating modes while
/// still allowing callers to supply an exact encoder configuration. Generated
/// DST output is described conservatively: verified predictive subset,
/// material-dependent compression, no mastering-encoder parity claim, and no
/// broad external-corpus claim until the formal acceptance gate passes.
#[derive(Debug, Clone)]
pub struct DstExtractionOptions {
    /// Handling for already-DST-coded source frames.
    pub source_dst: SourceDstHandling,
    /// Handling for uncompressed/plain DSD source frames.
    pub plain_dsd: PlainDsdDstHandling,
    /// Predictive encoder configuration used whenever this extraction path
    /// needs to generate a new DST frame or analyze a source frame.
    pub encoder: DstEncoderOptions,
}

impl Default for DstExtractionOptions {
    fn default() -> Self {
        Self::portable()
    }
}

impl DstExtractionOptions {
    /// Production/archival profile: preserve source DST; encode plain DSD only
    /// as verified, profitable predictive DST. No raw DST fallback.
    pub fn portable() -> Self {
        let mut encoder = DstEncoderOptions::default();
        encoder.raw_fallback = RawDstFallbackPolicy::Disabled;
        Self {
            source_dst: SourceDstHandling::PassthroughExistingDst,
            plain_dsd: PlainDsdDstHandling::EncodeUncompressedSourceToDst,
            encoder,
        }
    }

    /// Strict generated-predictive profile: all written frames are generated by
    /// this encoder and must be predictive. Source DST is decoded and re-coded,
    /// so this is for encoder validation rather than archival extraction.
    pub fn strict_predictive_only() -> Self {
        let mut encoder = DstEncoderOptions::default();
        encoder.raw_fallback = RawDstFallbackPolicy::Disabled;
        Self {
            source_dst: SourceDstHandling::ReencodeDst,
            plain_dsd: PlainDsdDstHandling::EncodeUncompressedSourceToDst,
            encoder,
        }
    }

    /// Passthrough-only profile: write source DST byte-for-byte and reject plain
    /// DSD frames. This is appropriate when the caller expects a DST-coded SACD
    /// area and wants to fail if the source is not already compressed.
    pub fn passthrough_only() -> Self {
        let mut encoder = DstEncoderOptions::default();
        encoder.raw_fallback = RawDstFallbackPolicy::Disabled;
        Self {
            source_dst: SourceDstHandling::PassthroughExistingDst,
            plain_dsd: PlainDsdDstHandling::RejectUncompressedSource,
            encoder,
        }
    }


    /// Write any DSD source as ordinary DSDIFF/DSD. Source DST frames are
    /// decoded to plain DSD, and plain DSD source frames are copied unchanged;
    /// the output declares `CMPR = "DSD "` and contains no `DSTF` chunks. This
    /// is the explicit compatibility escape hatch when the caller wants a DFF
    /// file but does not want source DST passthrough or re-encoding.
    pub fn decode_to_dsdiff_dsd() -> Self {
        let mut encoder = DstEncoderOptions::default();
        encoder.raw_fallback = RawDstFallbackPolicy::Disabled;
        Self {
            source_dst: SourceDstHandling::DecodeToDsdiffDsd,
            plain_dsd: PlainDsdDstHandling::WriteUncompressedSourceAsDsdiffDsd,
            encoder,
        }
    }

    /// Explicit compatibility-test profile: preserve source DST and allow raw
    /// DST fallback for plain DSD when predictive compression is unavailable or
    /// unprofitable. Do not use as a default for broadly playable files.
    pub fn raw_fallback_compatibility() -> Self {
        let mut encoder = DstEncoderOptions::default();
        encoder.raw_fallback = RawDstFallbackPolicy::Enabled;
        Self {
            source_dst: SourceDstHandling::PassthroughExistingDst,
            plain_dsd: PlainDsdDstHandling::EncodeUncompressedSourceToDstWithRawFallback,
            encoder,
        }
    }

    /// Fast profile: fewer predictor candidates, shared tables only, exact
    /// verification retained, raw fallback disabled.
    pub fn fast() -> Self {
        let mut encoder = DstEncoderOptions::default();
        encoder.effort = DstEncoderEffort::Fast;
        encoder.candidate_prediction_orders = vec![8, 16];
        encoder.coefficient_quantization_scales = vec![192, 128];
        encoder.coefficient_prune_thresholds = vec![0, 2];
        encoder.per_channel_filters = false;
        encoder.verify = true;
        encoder.raw_fallback = RawDstFallbackPolicy::Disabled;
        Self {
            source_dst: SourceDstHandling::PassthroughExistingDst,
            plain_dsd: PlainDsdDstHandling::EncodeUncompressedSourceToDst,
            encoder,
        }
    }

    /// Higher-effort compression profile: larger predictor-order, coefficient
    /// scale, and pruning search; exact verification retained; raw fallback
    /// disabled.
    pub fn high_compression() -> Self {
        let mut encoder = DstEncoderOptions::default();
        encoder.effort = DstEncoderEffort::HighCompression;
        encoder.fast_prescreen = false;
        encoder.candidate_prediction_orders = vec![4, 8, 12, 16, 24, 32, 48, 64, 96, 128];
        encoder.coefficient_quantization_scales = vec![320, 255, 224, 192, 160, 128, 96, 64];
        encoder.coefficient_prune_thresholds = vec![0, 1, 2, 3, 4, 6, 8];
        encoder.per_channel_filters = true;
        encoder.verify = true;
        encoder.raw_fallback = RawDstFallbackPolicy::Disabled;
        Self {
            source_dst: SourceDstHandling::PassthroughExistingDst,
            plain_dsd: PlainDsdDstHandling::EncodeUncompressedSourceToDst,
            encoder,
        }
    }

    /// Corpus-analysis profile: preserve professional source DST payloads while
    /// running the in-tree encoder against their decoded DSD to populate
    /// aggregate telemetry. Plain DSD uses the high-compression predictive path.
    pub fn corpus_analysis() -> Self {
        let mut this = Self::high_compression();
        this.source_dst = SourceDstHandling::AnalyzeThenPassthroughExistingDst;
        this
    }

    /// Start from an exact encoder mechanism while keeping production policy.
    pub fn with_encoder_options(mut self, encoder: DstEncoderOptions) -> Self {
        self.encoder = encoder;
        self
    }

    /// Override source-DST handling.
    pub fn with_source_dst_handling(mut self, handling: SourceDstHandling) -> Self {
        self.source_dst = handling;
        match handling {
            SourceDstHandling::DecodeToDsdiffDsd => {
                self.plain_dsd = PlainDsdDstHandling::WriteUncompressedSourceAsDsdiffDsd;
            }
            _ if self.plain_dsd == PlainDsdDstHandling::WriteUncompressedSourceAsDsdiffDsd => {
                self.plain_dsd = PlainDsdDstHandling::EncodeUncompressedSourceToDst;
            }
            _ => {}
        }
        self
    }

    /// Override plain-DSD handling.
    pub fn with_plain_dsd_handling(mut self, handling: PlainDsdDstHandling) -> Self {
        self.plain_dsd = handling;
        self
    }

    fn encoder_for_plain_dsd(&self) -> DstEncoderOptions {
        let mut encoder = self.encoder.clone();
        encoder.raw_fallback = match self.plain_dsd {
            PlainDsdDstHandling::EncodeUncompressedSourceToDst
            | PlainDsdDstHandling::WriteUncompressedSourceAsDsdiffDsd
            | PlainDsdDstHandling::RejectUncompressedSource => RawDstFallbackPolicy::Disabled,
            PlainDsdDstHandling::EncodeUncompressedSourceToDstWithRawFallback => {
                RawDstFallbackPolicy::Enabled
            }
        };
        encoder
    }
}

/// Time-based frame filter for excluding pre-gap and inter-track
/// pause frames. Matches sacd_extract's `frame_read_callback`
/// default behavior (`audio_frame_trimming = 1`): frames whose
/// absolute timecode is outside `[start_frame, start_frame +
/// duration_frames)` get silently dropped.
///
/// Source the values from SACDTRL2 — in tonepoet's parser, that's
/// `TrackEntry.start_time` and `TrackEntry.duration` (both as
/// `PlayTime`). Convert each to 75fps frame counts:
/// `m * 60 * 75 + s * 75 + f`. This formula is identical to
/// sacd_extract's `TIME_FRAMECOUNT` macro
/// (libsacd/scarletbook.h).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeFilter {
    /// Track start absolute timecode in 75fps frame units.
    pub start_frame: u32,
    /// Track duration in 75fps frame units.
    pub duration_frames: u32,
}

impl TimeFilter {
    /// Construct a filter from frame counts. Use the formula
    /// `m * 60 * 75 + s * 75 + f` to derive each value from a
    /// (minutes, seconds, frames) timecode triple.
    pub fn new(start_frame: u32, duration_frames: u32) -> Self {
        Self {
            start_frame,
            duration_frames,
        }
    }

    /// True iff `tc ∈ [start_frame, start_frame + duration_frames)`,
    /// matching sacd_extract's keep-frame condition. Uses saturating
    /// arithmetic so adversarial inputs (e.g. `duration_frames =
    /// u32::MAX`) don't panic.
    pub fn includes(&self, tc: u32) -> bool {
        let end = self.start_frame.saturating_add(self.duration_frames);
        tc >= self.start_frame && tc < end
    }
}

/// Options bundle for [`extract_track`].
///
/// This struct intentionally remains source-compatible with the original
/// public API: callers may still construct it with a struct literal using
/// exactly these public fields. DSDIFF/DST policy lives in
/// [`DstExtractionOptions`] and is supplied through the DST-specific
/// extraction entry points.
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    pub start_lsn: u64,
    pub end_lsn: u64,
    pub channel_count: u8,
    pub format: OutputFormat,
    /// If set, frames whose timecode is outside the filter's range
    /// are silently dropped. Matches sacd_extract's default
    /// `audio_frame_trimming = 1` behavior. Use when passing the
    /// wider `area_toc.track_start..track_start_lsn[next]` LSN
    /// range; leave `None` when passing pre-trimmed SACDTRL1
    /// ranges (no frames will be out of timecode bounds anyway).
    pub time_filter: Option<TimeFilter>,
    /// If set, an ID3v2.4 footer is appended to the output after
    /// the audio data (DSF only). Matches sacd_extract's default
    /// `id3_tag_mode = 4` behavior.
    pub id3_metadata: Option<Id3Metadata>,
    /// If set, the DSDIFF footer (DIIN + COMT + ID3 chunks) is
    /// appended to DFF output after audio. Matches sacd_extract's
    /// non-edit-master default footer.
    pub dff_metadata: Option<DffMetadata>,
}


/// Output-side options for writing any common [`DsdSource`].
///
/// Unlike [`ExtractOptions`], this struct does not describe an ISO sector
/// range. It is the sink policy used by the common source-model pipeline for
/// SACD ISO sources, DSF files, DSDIFF/DSD files, and DSDIFF/DST files.
#[derive(Debug, Clone)]
pub struct DsdSourceExtractOptions {
    pub format: OutputFormat,
    pub id3_metadata: Option<Id3Metadata>,
    pub dff_metadata: Option<DffMetadata>,
    pub dst: DstExtractionOptions,
    /// When true, DST decode failures produce a silence frame (0x55)
    /// instead of aborting extraction. Matches the reference C
    /// extractor's behavior of logging errors and continuing.
    pub recover_decode_errors: bool,
}

impl DsdSourceExtractOptions {
    pub fn new(format: OutputFormat) -> Self {
        Self {
            format,
            id3_metadata: None,
            dff_metadata: None,
            dst: DstExtractionOptions::default(),
            recover_decode_errors: false,
        }
    }

    pub fn with_id3_metadata(mut self, metadata: Id3Metadata) -> Self {
        self.id3_metadata = Some(metadata);
        self
    }

    pub fn with_dff_metadata(mut self, metadata: DffMetadata) -> Self {
        self.dff_metadata = Some(metadata);
        self
    }

    pub fn with_recover_decode_errors(mut self, recover: bool) -> Self {
        self.recover_decode_errors = recover;
        self
    }

    pub fn with_dst_options(mut self, dst: DstExtractionOptions) -> Self {
        self.dst = dst;
        self
    }
}

impl From<&ExtractOptions> for DsdSourceExtractOptions {
    fn from(opts: &ExtractOptions) -> Self {
        Self {
            format: opts.format,
            id3_metadata: opts.id3_metadata.clone(),
            dff_metadata: opts.dff_metadata.clone(),
            dst: DstExtractionOptions::default(),
            recover_decode_errors: false,
        }
    }
}

impl ExtractOptions {
    /// Construct options for the no-filter case (matches
    /// sacd_extract's `-b pauses` flag behavior).
    pub fn new(start_lsn: u64, end_lsn: u64, channel_count: u8, format: OutputFormat) -> Self {
        Self {
            start_lsn,
            end_lsn,
            channel_count,
            format,
            time_filter: None,
            id3_metadata: None,
            dff_metadata: None,
        }
    }

    /// Attach a time filter (sacd_extract's default behavior).
    pub fn with_time_filter(mut self, filter: TimeFilter) -> Self {
        self.time_filter = Some(filter);
        self
    }

    /// Attach ID3v2.4 metadata. When set on a DSF extraction, the
    /// rendered tag is appended after audio + pad and the DSF
    /// header's `metadata_offset` is updated to point to it.
    /// Matches sacd_extract's default `id3_tag_mode = 4`.
    pub fn with_id3_metadata(mut self, meta: Id3Metadata) -> Self {
        self.id3_metadata = Some(meta);
        self
    }

    /// Attach DFF footer metadata (DIIN + COMT + ID3 chunks). When
    /// set on a DFF extraction, the rendered footer is appended
    /// after audio + pad and the FRM8 chunk_data_size is updated to
    /// include the footer length. Matches sacd_extract's
    /// non-edit-master default footer.
    pub fn with_dff_metadata(mut self, meta: DffMetadata) -> Self {
        self.dff_metadata = Some(meta);
        self
    }

}

/// Additive controls for integrity-sensitive extraction.
///
/// These controls were deliberately not added to [`ExtractOptions`], because
/// `ExtractOptions` is public and existing downstream callers may construct it
/// with a struct literal. Use [`extract_track_with_integrity_options`] when the
/// caller can supply area-TOC state or wants damaged-disc recovery.
#[derive(Debug, Clone)]
/// SB-AUDIT: SB-AUDIO-012..SB-AUDIO-015
pub struct ExtractIntegrityOptions {
    /// Authoritative area-TOC frame format. When set, extraction validates
    /// every audio sector header against it and uses it to choose DST versus
    /// plain-DSD frame-info layout.
    pub frame_format: Option<FrameFormat>,
    /// If true, the frame reader records and skips malformed/unreadable
    /// sectors instead of failing immediately. The in-progress frame is
    /// discarded whenever a sector is skipped so unrelated payloads are never
    /// joined.
    pub recover_sector_errors: bool,
    /// If true, a DST frame whose channel hint disagrees with the area TOC
    /// aborts extraction. The default is true: validation workflows are
    /// fail-fast unless the caller deliberately opts into salvage mode.
    pub strict_channel_count: bool,
}

impl Default for ExtractIntegrityOptions {
    fn default() -> Self {
        Self {
            frame_format: None,
            recover_sector_errors: false,
            strict_channel_count: true,
        }
    }
}

impl ExtractIntegrityOptions {
    /// Strict validation profile. Malformed sectors, read errors, invalid
    /// timecodes, incomplete frames, frame-format mismatches, and strict
    /// channel-count mismatches fail the extraction. This is the default and
    /// should be used for regression validation and byte-exact comparisons.
    pub fn strict() -> Self {
        Self::default()
    }

    pub fn new() -> Self {
        Self::strict()
    }

    /// Deliberate damaged-disc salvage profile. The frame reader skips
    /// unreadable/malformed sectors, discards any in-progress frame at each
    /// skip boundary, records every skipped-sector and dropped-frame detail
    /// in [`ExtractReport::integrity`], and returns success only with an
    /// explicit non-clean integrity report.
    pub fn salvage() -> Self {
        Self {
            recover_sector_errors: true,
            strict_channel_count: false,
            ..Self::default()
        }
    }

    /// Attach the area-TOC frame format. This is the preferred constructor
    /// path for high-integrity extraction because it makes the area TOC the
    /// source of truth and rejects sectors whose compression bit disagrees.
    pub fn with_frame_format(mut self, frame_format: FrameFormat) -> Self {
        self.frame_format = Some(frame_format);
        self
    }

    /// Continue extraction after malformed or unreadable sectors. Use when
    /// salvaging damaged ISOs; leave disabled for validation runs.
    pub fn with_sector_recovery(mut self, recover: bool) -> Self {
        self.recover_sector_errors = recover;
        self
    }

    /// Treat DST channel-count hint mismatches as fatal parser errors.
    pub fn with_strict_channel_count(mut self, strict: bool) -> Self {
        self.strict_channel_count = strict;
        self
    }
}

/// Errors from `extract_track`.
#[derive(Debug)]
pub enum ExtractError {
    /// Failure parsing the ISO frame stream.
    Frame(FrameError),
    /// Failure writing to the output sink.
    Io(io::Error),
    /// Failure decoding a DST-encoded frame.
    Dst(DstError),
    /// Failure in the common source-model layer.
    Source(DsdSourceError),
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Frame(e) => write!(f, "frame read error: {}", e),
            Self::Io(e) => write!(f, "output write error: {}", e),
            Self::Dst(e) => write!(f, "DST decode error: {}", e),
            Self::Source(e) => write!(f, "DSD source error: {}", e),
        }
    }
}

impl std::error::Error for ExtractError {}

impl From<FrameError> for ExtractError {
    fn from(e: FrameError) -> Self {
        Self::Frame(e)
    }
}

impl From<io::Error> for ExtractError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<DstError> for ExtractError {
    fn from(e: DstError) -> Self {
        Self::Dst(e)
    }
}

impl From<DsdSourceError> for ExtractError {
    fn from(e: DsdSourceError) -> Self {
        match e {
            DsdSourceError::Io(e) => Self::Io(e),
            DsdSourceError::Frame(e) => Self::Frame(e),
            DsdSourceError::Dst(e) => Self::Dst(e),
            other => Self::Source(other),
        }
    }
}

/// Error returned by path-based, transaction-protected extraction and conversion.
///
/// This error preserves the final path, temporary path, original extraction or
/// conversion failure, and any cleanup failure from removing the temporary file.
#[derive(Debug)]
pub enum ExtractToPathError {
    /// Creating, committing, or explicitly aborting the output transaction
    /// failed before or after the extraction body ran.
    Transaction(OutputTransactionError),
    /// The extraction/conversion body failed after the temporary file had been
    /// created. The final output path was not published. `cleanup_error` is
    /// populated only if removing the temporary file also failed.
    OperationFailed {
        final_path: PathBuf,
        temp_path: PathBuf,
        source: ExtractError,
        cleanup_error: Option<OutputTransactionError>,
    },
}

impl ExtractToPathError {
    /// Destination path requested by the caller.
    pub fn final_path(&self) -> &Path {
        match self {
            Self::Transaction(e) => e.final_path(),
            Self::OperationFailed { final_path, .. } => final_path.as_path(),
        }
    }

    /// Temporary path involved in the failure, if a transaction had already
    /// been created.
    pub fn temp_path(&self) -> Option<&Path> {
        match self {
            Self::Transaction(e) => e.temp_path(),
            Self::OperationFailed { temp_path, .. } => Some(temp_path.as_path()),
        }
    }
}

impl std::fmt::Display for ExtractToPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transaction(e) => write!(f, "{}", e),
            Self::OperationFailed {
                final_path,
                temp_path,
                source,
                cleanup_error,
            } => {
                write!(
                    f,
                    "transactional extraction to {} failed while writing temporary file {}: {}",
                    final_path.display(),
                    temp_path.display(),
                    source
                )?;
                if let Some(cleanup_error) = cleanup_error {
                    write!(f, "; temporary cleanup also failed: {}", cleanup_error)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ExtractToPathError {}

impl From<OutputTransactionError> for ExtractToPathError {
    fn from(e: OutputTransactionError) -> Self {
        Self::Transaction(e)
    }
}

/// Summary returned by the source-compatible [`extract_track`] API.
///
/// This struct intentionally keeps the original two public fields so existing
/// downstream struct literals and field accesses continue to compile. Use
/// [`extract_track_with_integrity_options`] to obtain the full
/// [`ExtractReport`] with sector recovery and parser-integrity diagnostics.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExtractStats {
    /// Number of frames **written to output**. Frames dropped by
    /// the parser time filter are not counted, matching sacd_extract's
    /// `count_frames` semantics (incremented only inside the
    /// keep-range branch of `frame_read_callback`).
    pub frames_read: u64,
    /// Total audio bytes pushed to the writer (pre-pad,
    /// post-filter).
    pub audio_bytes: u64,
}

/// Detailed parser and recovery state returned by
/// [`extract_track_with_integrity_options`].
#[derive(Debug, Clone, Default)]
/// SB-AUDIT: SB-AUDIO-014..SB-AUDIO-015
pub struct ExtractIntegrityReport {
    /// Number of sectors successfully read from the ISO frame range.
    pub sectors_read: u64,
    /// Number of sectors skipped by damaged-disc recovery. A non-zero
    /// value means the extraction succeeded only by omitting source data.
    pub sectors_skipped: u64,
    /// Number of syntactically malformed audio sectors encountered.
    pub malformed_sectors: u64,
    /// Number of sector I/O errors observed while recovery was enabled.
    pub io_errors: u64,
    /// Number of complete parser frames emitted before container writes.
    pub parser_frames_emitted: u64,
    /// Number of parser frames rejected by the timecode filter.
    pub frames_filtered: u64,
    /// Number of partial frames discarded at a frame boundary, sector skip,
    /// or end-of-range flush.
    pub frames_dropped_incomplete: u64,
    /// Number of DST frame channel hints that disagreed with the area TOC.
    pub channel_mismatches: u64,
    /// Number of sector header compression bits that disagreed with the
    /// authoritative area-TOC frame format.
    pub frame_format_mismatches: u64,
    /// Number of frame-info timecodes outside the normalized SACD ranges
    /// (`seconds >= 60` or `frames >= 75`).
    pub invalid_timecodes: u64,
    /// Bytes emitted by the frame parser before DST decoding. For DST this
    /// is encoded bytes; `ExtractStats::audio_bytes` is bytes actually written.
    pub parser_bytes_emitted: u64,
    /// Exact sector-level recovery log. A non-empty list means the
    /// extraction result was salvaged, not clean. Each entry includes the
    /// LSN, failure phase, and error text from the reader/parser.
    pub recovery_events: Vec<RecoveryEvent>,
    /// Exact partial-frame loss log. In normal mode these conditions are
    /// fatal; in recovery mode they are reported here so callers can decide
    /// whether a salvaged output is acceptable.
    pub dropped_frame_events: Vec<DroppedFrameEvent>,
}

impl ExtractIntegrityReport {
    fn from_reader_stats(reader: FrameReaderStats) -> Self {
        let report = Self {
            sectors_read: reader.sectors_read,
            sectors_skipped: reader.sectors_skipped,
            malformed_sectors: reader.malformed_sectors,
            io_errors: reader.io_errors,
            parser_frames_emitted: reader.frames_emitted,
            frames_filtered: reader.frames_filtered,
            frames_dropped_incomplete: reader.frames_dropped_incomplete,
            channel_mismatches: reader.channel_mismatches,
            frame_format_mismatches: reader.frame_format_mismatches,
            invalid_timecodes: reader.invalid_timecodes,
            parser_bytes_emitted: reader.bytes_emitted,
            recovery_events: reader.recovery_events,
            dropped_frame_events: reader.dropped_frame_events,
        };
        debug_assert_eq!(report.sectors_skipped as usize, report.recovery_events.len());
        debug_assert!(
            report.dropped_frame_events.len() <= report.frames_dropped_incomplete as usize
        );
        report
    }

    /// Per-sector recovery details, suitable for logging in the caller/UI.
    pub fn recovery_events(&self) -> &[RecoveryEvent] {
        &self.recovery_events
    }

    /// Partial-frame loss details, suitable for logging in the caller/UI.
    pub fn dropped_frame_events(&self) -> &[DroppedFrameEvent] {
        &self.dropped_frame_events
    }

    /// True when no parser/recovery anomaly was observed.
    pub fn is_clean(&self) -> bool {
        !self.integrity_loss_detected()
    }

    /// True when the extraction succeeded only under the damaged-disc
    /// salvage contract.
    pub fn is_salvaged(&self) -> bool {
        self.integrity_loss_detected()
    }

    /// True when a nominally successful extraction still lost integrity
    /// because damaged-sector recovery, incomplete-frame dropping, area
    /// frame-format disagreement, channel disagreement, or invalid timecode
    /// handling was required.
    pub fn integrity_loss_detected(&self) -> bool {
        self.sectors_skipped != 0
            || self.malformed_sectors != 0
            || self.io_errors != 0
            || self.frames_dropped_incomplete != 0
            || self.channel_mismatches != 0
            || self.frame_format_mismatches != 0
            || self.invalid_timecodes != 0
    }
}

/// Full extraction result returned by the additive high-integrity API.
#[derive(Debug, Clone, Default)]
pub struct ExtractReport {
    /// Source-compatible frame/byte counters.
    pub stats: ExtractStats,
    /// Parser, sector-recovery, and integrity diagnostics.
    pub integrity: ExtractIntegrityReport,
    /// DSDIFF/DST writer telemetry. Present for [`OutputFormat::DffDst`] only when the selected policy actually writes DSDIFF/DST, not when [`SourceDstHandling::DecodeToDsdiffDsd`] routes to ordinary DSDIFF/DSD.
    pub dff_dst: Option<DffDstWriterStats>,
}

impl ExtractReport {
    pub fn integrity_loss_detected(&self) -> bool {
        self.integrity.integrity_loss_detected()
    }

    pub fn is_clean(&self) -> bool {
        self.integrity.is_clean()
    }

    pub fn is_salvaged(&self) -> bool {
        self.integrity.is_salvaged()
    }

    pub fn recovery_events(&self) -> &[RecoveryEvent] {
        self.integrity.recovery_events()
    }

    pub fn dropped_frame_events(&self) -> &[DroppedFrameEvent] {
        self.integrity.dropped_frame_events()
    }

    /// DSDIFF/DST writer telemetry, when the output format was
    /// [`OutputFormat::DffDst`] and the selected policy wrote DSDIFF/DST rather
    /// than routing through [`SourceDstHandling::DecodeToDsdiffDsd`].
    pub fn dff_dst_stats(&self) -> Option<&DffDstWriterStats> {
        self.dff_dst.as_ref()
    }
}

/// Extract a single track's DSD audio from `iso` into `output`,
/// per `opts`.
///
/// `opts.channel_count` must match the SACD area's channel layout
/// (2, 5, or 6 for real SACDs). Uncompressed DSD frames don't
/// self-describe channel count; the orchestrator trusts the caller.
///
/// ## LSN range + time filter
///
/// Two valid call patterns produce identical output for real SACDs:
///
/// 1. **Pre-trimmed LSN range, no filter** — pass tonepoet's
///    `TrackEntry.start_lsn` + `length_lsn` from SACDTRL1, with
///    `opts.time_filter = None`. The SACDTRL1 range already
///    excludes pre-gaps + inter-track pauses, so no frame filter
///    is needed.
///
/// 2. **Wide LSN range + time filter** — pass the
///    `area_toc.track_start..track_start_lsn[next_track]` range
///    plus `opts.time_filter = Some(TimeFilter { ... })` built from
///    SACDTRL2's per-track start time + duration. This matches
///    sacd_extract's default behavior (`audio_frame_trimming = 1`).
///
/// Both patterns produce sacd_extract-default-equivalent audio
/// output. Pattern 1 is more efficient (fewer sectors read);
/// pattern 2 is sacd_extract-faithful when reproducing legacy
/// behavior matters.
///
/// On error, the output writer is dropped without `finish()`;
/// the file ends up with a placeholder header (zero chunk sizes)
/// plus partial audio bytes. **Discard the output on any error.**
pub fn extract_track<W: Write + Seek>(
    iso: &mut IsoReader,
    output: &mut W,
    opts: ExtractOptions,
) -> Result<ExtractStats, ExtractError> {
    let report = extract_track_impl(
        iso,
        output,
        opts,
        ExtractIntegrityOptions::strict(),
        DstExtractionOptions::default(),
    )?;
    // A trailing incomplete frame at the end of the extraction range
    // is normal — TOC sector ranges rarely align to DSD frame boundaries.
    // The frame reader records it in stats but does not error. Only
    // assert no integrity loss beyond expected end-of-range truncation.
    debug_assert!(
        report.integrity.sectors_skipped == 0
            && report.integrity.malformed_sectors == 0
            && report.integrity.io_errors == 0
            && report.integrity.channel_mismatches == 0
            && report.integrity.frame_format_mismatches == 0
            && report.integrity.invalid_timecodes == 0,
        "unexpected integrity loss in strict extraction: {:?}",
        report.integrity
    );
    Ok(report.stats)
}

/// High-integrity extraction entry point.
///
/// This additive API preserves [`extract_track`] while returning all recovery,
/// parser-integrity, and DSDIFF/DST writer telemetry state.
/// In strict/default mode (`recover_sector_errors == false`), malformed
/// sectors, read errors, invalid timecodes, frame-format mismatches, strict
/// channel-count mismatches, and incomplete pending frames fail the operation.
/// In salvage mode ([`ExtractIntegrityOptions::salvage`]), the operation may
/// succeed after damaged sectors are skipped, but every skipped sector and
/// dropped partial frame is retained in [`ExtractReport::integrity`]. A caller
/// must treat `report.integrity_loss_detected() == true` as a salvaged,
/// non-verification-grade extraction.
pub fn extract_track_with_integrity_options<W: Write + Seek>(
    iso: &mut IsoReader,
    output: &mut W,
    opts: ExtractOptions,
    integrity_options: ExtractIntegrityOptions,
) -> Result<ExtractReport, ExtractError> {
    extract_track_impl(
        iso,
        output,
        opts,
        integrity_options,
        DstExtractionOptions::default(),
    )
}

/// Extract one ISO track with explicit DSDIFF/DST handling while preserving
/// the source-compatible [`ExtractOptions`] shape.
///
/// Use this when requesting [`OutputFormat::DffDst`] and the caller needs a
/// policy other than [`DstExtractionOptions::default`]. Existing DST-coded
/// source frames are passed through by default; decode-and-reencode and raw DST
/// fallback are explicit through [`DstExtractionOptions`].
pub fn extract_track_with_dst_options<W: Write + Seek>(
    iso: &mut IsoReader,
    output: &mut W,
    opts: ExtractOptions,
    dst_options: DstExtractionOptions,
) -> Result<ExtractStats, ExtractError> {
    let report = extract_track_impl(
        iso,
        output,
        opts,
        ExtractIntegrityOptions::strict(),
        dst_options,
    )?;
    Ok(report.stats)
}

/// High-integrity extraction with explicit DSDIFF/DST handling.
pub fn extract_track_with_integrity_and_dst_options<W: Write + Seek>(
    iso: &mut IsoReader,
    output: &mut W,
    opts: ExtractOptions,
    integrity_options: ExtractIntegrityOptions,
    dst_options: DstExtractionOptions,
) -> Result<ExtractReport, ExtractError> {
    extract_track_impl(iso, output, opts, integrity_options, dst_options)
}

/// Extract one ISO track directly to a final filesystem path using a
/// transaction-protected output file.
///
/// The low-level writers still stream to caller-owned sinks. This path-facing
/// helper is the application-level safety wrapper: it writes to a unique
/// temporary file in the destination directory, commits only after extraction
/// and writer finalization succeed, and removes the temporary file on failure.
/// Existing final files are refused unless `overwrite` is
/// [`OutputOverwritePolicy::ReplaceExisting`].
pub fn extract_track_to_path<P: AsRef<Path>>(
    iso: &mut IsoReader,
    final_path: P,
    opts: ExtractOptions,
    integrity_options: ExtractIntegrityOptions,
    overwrite: OutputOverwritePolicy,
) -> Result<ExtractReport, ExtractToPathError> {
    extract_track_to_path_with_dst_options(
        iso,
        final_path,
        opts,
        integrity_options,
        DstExtractionOptions::default(),
        overwrite,
    )
}

/// Extract one ISO track directly to a final filesystem path with explicit
/// DSDIFF/DST handling and transaction-protected publication.
pub fn extract_track_to_path_with_dst_options<P: AsRef<Path>>(
    iso: &mut IsoReader,
    final_path: P,
    opts: ExtractOptions,
    integrity_options: ExtractIntegrityOptions,
    dst_options: DstExtractionOptions,
    overwrite: OutputOverwritePolicy,
) -> Result<ExtractReport, ExtractToPathError> {
    let final_path = final_path.as_ref().to_path_buf();
    let mut tx = OutputTransaction::create(&final_path, overwrite)?;
    let temp_path = tx.temp_path().to_path_buf();
    match extract_track_with_integrity_and_dst_options(
        iso,
        &mut tx,
        opts,
        integrity_options,
        dst_options,
    ) {
        Ok(report) => {
            tx.commit()?;
            Ok(report)
        }
        Err(source) => {
            let cleanup_error = tx.abort().err();
            Err(ExtractToPathError::OperationFailed {
                final_path,
                temp_path,
                source,
                cleanup_error,
            })
        }
    }
}

/// Write any common DSD source directly to a final filesystem path using the
/// same transaction contract as [`extract_track_to_path`].
pub fn write_dsd_source_to_path<S, P>(
    source: &mut S,
    final_path: P,
    opts: DsdSourceExtractOptions,
    overwrite: OutputOverwritePolicy,
) -> Result<ExtractReport, ExtractToPathError>
where
    S: DsdSource + ?Sized,
    P: AsRef<Path>,
{
    let final_path = final_path.as_ref().to_path_buf();
    let mut tx = OutputTransaction::create(&final_path, overwrite)?;
    let temp_path = tx.temp_path().to_path_buf();
    match write_dsd_source(source, &mut tx, opts) {
        Ok(report) => {
            tx.commit()?;
            Ok(report)
        }
        Err(source) => {
            let cleanup_error = tx.abort().err();
            Err(ExtractToPathError::OperationFailed {
                final_path,
                temp_path,
                source,
                cleanup_error,
            })
        }
    }
}

fn extract_track_impl<W: Write + Seek>(
    iso: &mut IsoReader,
    output: &mut W,
    opts: ExtractOptions,
    integrity_options: ExtractIntegrityOptions,
    dst_options: DstExtractionOptions,
) -> Result<ExtractReport, ExtractError> {
    let mut source_opts = IsoTrackSourceOptions::new(
        opts.start_lsn,
        opts.end_lsn,
        opts.channel_count,
        SACD_SAMPLING_FREQUENCY,
    )
    .with_strict_channel_count(integrity_options.strict_channel_count)
    .with_sector_recovery(integrity_options.recover_sector_errors);

    if let Some(frame_format) = integrity_options.frame_format {
        source_opts = source_opts.with_frame_format(frame_format);
    }
    if let Some(filter) = opts.time_filter {
        source_opts = source_opts.with_time_filter(FrameTimeFilter::new(
            filter.start_frame,
            filter.duration_frames,
        ));
    }

    let mut source = IsoTrackSource::new(iso, source_opts);
    let mut sink_options = DsdSourceExtractOptions::from(&opts);
    sink_options.dst = dst_options;
    sink_options.recover_decode_errors = integrity_options.recover_sector_errors;
    let mut report = write_dsd_source(&mut source, output, sink_options)?;
    report.integrity = ExtractIntegrityReport::from_reader_stats(source.frame_reader_stats());
    Ok(report)
}

/// Write any common DSD source to a DSF, DSDIFF/DSD, or DSDIFF/DST sink.
///
/// This is the extraction/conversion core after the common source-model
/// refactor. SACD ISO extraction is now just one producer of [`DsdSource`]
/// frames; DSF, DSDIFF/DSD, and DSDIFF/DST file inputs can feed the same sink
/// policy. Existing source DST frames stay encoded until this function's
/// output policy explicitly decodes or re-encodes them.
pub fn write_dsd_source<S, W>(
    source: &mut S,
    output: &mut W,
    opts: DsdSourceExtractOptions,
) -> Result<ExtractReport, ExtractError>
where
    S: DsdSource + ?Sized,
    W: Write + Seek,
{
    let channel_count = source_channel_count_u8(source.source_info().channel_count)?;
    let sample_rate = source.source_info().sample_rate;

    match opts.format {
        OutputFormat::Dsf => {
            let mut writer = DsfWriter::new(output, channel_count, sample_rate)?;
            if let Some(ref meta) = opts.id3_metadata {
                writer.set_id3_footer(render_id3v24(meta));
            }
            let report = drain_source_decoded(source, opts.recover_decode_errors, |frame| {
                writer.write_interleaved(&frame.data).map_err(ExtractError::Io)
            })?;
            writer.finish()?;
            Ok(report)
        }
        OutputFormat::Dff => {
            let mut writer = DffWriter::new(output, channel_count, sample_rate)?;
            if let Some(ref meta) = opts.dff_metadata {
                writer.set_footer_bytes(render_dff_footer(meta));
            }
            let report = drain_source_decoded(source, opts.recover_decode_errors, |frame| {
                writer.write_frame(&frame.data).map_err(ExtractError::Io)
            })?;
            writer.finish()?;
            Ok(report)
        }
        OutputFormat::DffDst => {
            if opts.dst.source_dst == SourceDstHandling::DecodeToDsdiffDsd {
                if opts.dst.plain_dsd != PlainDsdDstHandling::WriteUncompressedSourceAsDsdiffDsd {
                    return Err(ExtractError::Io(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "DecodeToDsdiffDsd requires plain_dsd = WriteUncompressedSourceAsDsdiffDsd so all source frames route to ordinary DSDIFF/DSD",
                    )));
                }
                let mut writer = DffWriter::new(output, channel_count, sample_rate)?;
                if let Some(ref meta) = opts.dff_metadata {
                    writer.set_footer_bytes(render_dff_footer(meta));
                }
                let report = drain_source_decoded(source, opts.recover_decode_errors, |frame| {
                    writer.write_frame(&frame.data).map_err(ExtractError::Io)
                })?;
                writer.finish()?;
                return Ok(report);
            }

            let mut writer = DffDstWriter::new(output, channel_count, sample_rate)?;
            if let Some(ref meta) = opts.dff_metadata {
                writer.set_footer_bytes(render_dff_footer(meta));
            }
            let mut report = drain_source_to_dff_dst(source, &mut writer, &opts.dst)?;
            report.dff_dst = Some(writer.stats().clone());
            writer.finish()?;
            Ok(report)
        }
    }
}

fn source_channel_count_u8(channel_count: u16) -> Result<u8, ExtractError> {
    let channels = u8::try_from(channel_count).map_err(|_| {
        ExtractError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("source channel count {} exceeds writer interface", channel_count),
        ))
    })?;
    if channels == 0 {
        return Err(ExtractError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source channel count is zero",
        )));
    }
    Ok(channels)
}

fn drain_source_decoded<S, F>(
    source: &mut S,
    recover_decode_errors: bool,
    mut write_frame: F,
) -> Result<ExtractReport, ExtractError>
where
    S: DsdSource + ?Sized,
    F: FnMut(&SourceDsdFrame) -> Result<(), ExtractError>,
{
    use crate::dsd_file::inspect::DsdByteOrder;

    let mut stats = ExtractStats::default();
    let mut integrity = ExtractIntegrityReport::default();
    while let Some(frame) = source.next_source_frame()? {
        let decoded = match frame.into_decoded_dsd() {
            Ok(d) => d,
            Err(err) if recover_decode_errors => {
                // DST decode failed — write DSD silence (0x55, alternating
                // bits) for the expected frame size and continue. Matches the
                // reference C extractor's behavior of logging errors and not
                // aborting. The silence frame preserves frame count and
                // duration so downstream validation doesn't drift.
                eprintln!("DST decode error (recovered with silence): {err}");
                integrity.frames_dropped_incomplete += 1;
                let info = source.source_info();
                let silence_len = expected_decoded_frame_len(info.channel_count, info.sample_rate);
                let silence_frame = SourceDsdFrame {
                    data: vec![0x55u8; silence_len],
                    frame_index: stats.frames_read,
                    channel_count: info.channel_count,
                    sample_rate: info.sample_rate,
                    byte_order: DsdByteOrder::MsbFirst,
                    timecode: None,
                    is_final: false,
                };
                write_frame(&silence_frame)?;
                stats.frames_read += 1;
                stats.audio_bytes += silence_len as u64;
                continue;
            }
            Err(err) => return Err(err.into()),
        };
        write_frame(&decoded)?;
        stats.frames_read = stats.frames_read.checked_add(1).ok_or_else(|| {
            ExtractError::Io(io::Error::new(io::ErrorKind::InvalidData, "extract frame counter overflow"))
        })?;
        stats.audio_bytes = stats.audio_bytes.checked_add(decoded.data.len() as u64).ok_or_else(|| {
            ExtractError::Io(io::Error::new(io::ErrorKind::InvalidData, "extract byte counter overflow"))
        })?;
    }
    Ok(ExtractReport { stats, integrity, dff_dst: None })
}

/// Expected decoded frame size in bytes for a given channel count and sample rate.
fn expected_decoded_frame_len(channel_count: u16, sample_rate: u32) -> usize {
    use crate::frame::FRAME_SIZE_UNCOMPRESSED;
    let channels = channel_count as usize;
    let rate_multiplier = (sample_rate / 44_100).max(64) / 64;
    FRAME_SIZE_UNCOMPRESSED * channels * rate_multiplier as usize
}

fn drain_source_to_dff_dst<S, W>(
    source: &mut S,
    writer: &mut DffDstWriter<W>,
    dst_options: &DstExtractionOptions,
) -> Result<ExtractReport, ExtractError>
where
    S: DsdSource + ?Sized,
    W: Write + Seek,
{
    let mut stats = ExtractStats::default();
    while let Some(frame) = source.next_source_frame()? {
        match frame {
            DsdSourceFrame::Dst(dst) => {
                let decoded = dst.decode_checked()?;
                match dst_options.source_dst {
                    SourceDstHandling::PassthroughExistingDst => {
                        writer
                            .write_passthrough_frame(&dst.encoded, &decoded.data)
                            .map_err(ExtractError::Io)?;
                    }
                    SourceDstHandling::AnalyzeThenPassthroughExistingDst => {
                        writer
                            .analyze_interleaved_frame_with_options(&decoded.data, &dst_options.encoder)
                            .map_err(ExtractError::Io)?;
                        writer
                            .write_passthrough_frame(&dst.encoded, &decoded.data)
                            .map_err(ExtractError::Io)?;
                    }
                    SourceDstHandling::ReencodeDst => {
                        writer
                            .write_interleaved_frame_with_options(&decoded.data, &dst_options.encoder)
                            .map_err(ExtractError::Io)?;
                    }
                    SourceDstHandling::DecodeToDsdiffDsd => {
                        return Err(ExtractError::Io(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "DecodeToDsdiffDsd must route through the DSDIFF/DSD writer before DSTF emission",
                        )));
                    }
                    SourceDstHandling::RejectSourceDst => {
                        return Err(ExtractError::Io(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "source DST frame encountered but DST extraction policy is RejectSourceDst",
                        )));
                    }
                }
                stats.frames_read = stats.frames_read.checked_add(1).ok_or_else(|| {
                    ExtractError::Io(io::Error::new(io::ErrorKind::InvalidData, "extract frame counter overflow"))
                })?;
                stats.audio_bytes = stats.audio_bytes.checked_add(decoded.data.len() as u64).ok_or_else(|| {
                    ExtractError::Io(io::Error::new(io::ErrorKind::InvalidData, "extract byte counter overflow"))
                })?;
            }
            DsdSourceFrame::Dsd(dsd) => {
                match dst_options.plain_dsd {
                    PlainDsdDstHandling::RejectUncompressedSource => {
                        return Err(ExtractError::Io(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "plain DSD frame encountered but DSDIFF/DST policy is passthrough-only; use OutputFormat::Dff for DSDIFF/DSD or select a predictive DST policy",
                        )));
                    }
                    PlainDsdDstHandling::WriteUncompressedSourceAsDsdiffDsd => {
                        return Err(ExtractError::Io(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "WriteUncompressedSourceAsDsdiffDsd is valid only with SourceDstHandling::DecodeToDsdiffDsd; use OutputFormat::Dff for ordinary DSDIFF/DSD or select a DFF/DST plain-DSD policy",
                        )));
                    }
                    PlainDsdDstHandling::EncodeUncompressedSourceToDst
                    | PlainDsdDstHandling::EncodeUncompressedSourceToDstWithRawFallback => {
                        let encoder = dst_options.encoder_for_plain_dsd();
                        writer
                            .write_interleaved_frame_with_options(&dsd.data, &encoder)
                            .map_err(ExtractError::Io)?;
                    }
                }
                stats.frames_read = stats.frames_read.checked_add(1).ok_or_else(|| {
                    ExtractError::Io(io::Error::new(io::ErrorKind::InvalidData, "extract frame counter overflow"))
                })?;
                stats.audio_bytes = stats.audio_bytes.checked_add(dsd.data.len() as u64).ok_or_else(|| {
                    ExtractError::Io(io::Error::new(io::ErrorKind::InvalidData, "extract byte counter overflow"))
                })?;
            }
        }
    }
    Ok(ExtractReport { stats, integrity: ExtractIntegrityReport::default(), dff_dst: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsf_writer::BLOCK_SIZE_PER_CHANNEL;
    use crate::frame::{FrameFormat, Timecode, DATA_TYPE_AUDIO, FRAME_SIZE_UNCOMPRESSED};
    use crate::iso_reader::SECTOR_SIZE;
    use crate::test_util::{
        sha256_hex, synth_audio_sector, synth_continuation_sector, synth_dst_sector, tc_at,
        write_iso,
    };

    const PART_SIZE: usize = 2000;

    /// Build sectors that encode `frame_bytes` as a single uncompressed
    /// frame starting with frame_start in the first sector.
    fn synth_uncompressed_frame_sectors(frame_bytes: &[u8], tc: Timecode) -> Vec<Vec<u8>> {
        let mut sectors = Vec::new();
        let first = frame_bytes.len().min(PART_SIZE);
        sectors.push(synth_audio_sector(true, &frame_bytes[..first], tc));
        let mut off = first;
        while off < frame_bytes.len() {
            let chunk = (frame_bytes.len() - off).min(PART_SIZE);
            sectors.push(synth_continuation_sector(&frame_bytes[off..off + chunk]));
            off += chunk;
        }
        sectors
    }

    fn synth_dst_frame_sectors(payload: &[u8], channel_count: u8, tc: Timecode) -> Vec<Vec<u8>> {
        fn continuation(payload: &[u8]) -> Vec<u8> {
            assert!(payload.len() <= 2045);
            let mut s = Vec::with_capacity(SECTOR_SIZE as usize);
            s.push(1u8 | (1u8 << 5));
            s.extend_from_slice(&[
                ((DATA_TYPE_AUDIO & 0x07) << 3) | (((payload.len() as u16 >> 8) as u8) & 0x07),
                ((payload.len() & 0xff) as u8),
            ]);
            s.extend_from_slice(payload);
            s.resize(SECTOR_SIZE as usize, 0);
            s
        }

        let first_len = payload.len().min(2041);
        let remaining = payload.len().saturating_sub(first_len);
        let continuation_count = (remaining + 2044) / 2045;
        let sector_count = u8::try_from(1 + continuation_count).expect("synthetic DST sector count fits u8");
        let mut sectors = vec![synth_dst_sector(&payload[..first_len], channel_count, sector_count, tc)];
        let mut off = first_len;
        while off < payload.len() {
            let chunk = (payload.len() - off).min(2045);
            sectors.push(continuation(&payload[off..off + chunk]));
            off += chunk;
        }
        sectors
    }

    /// Test pattern: byte i = (i & 0xFF). Easy to spot demux/bit-flip bugs.
    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i & 0xFF) as u8).collect()
    }


    fn malformed_audio_sector_packet_too_large() -> Vec<u8> {
        let mut sector = vec![0u8; SECTOR_SIZE as usize];
        sector[0] = 1 << 5; // one packet, no frame info, uncompressed
        sector[1] = (2 << 3) | 0x07; // DATA_TYPE_AUDIO, length high bits = 7
        sector[2] = 0xff; // length = 2047 > MAX_PACKET_SIZE
        sector
    }

    fn read_u16_be(b: &[u8], off: usize) -> u16 {
        u16::from_be_bytes(b[off..off + 2].try_into().unwrap())
    }
    fn read_u32_le(b: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
    }
    fn read_u64_be(b: &[u8], off: usize) -> u64 {
        u64::from_be_bytes(b[off..off + 8].try_into().unwrap())
    }
    fn read_u64_le(b: &[u8], off: usize) -> u64 {
        u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
    }

    /// DSF bit-reverse table (LSB-first storage). Computed inline so
    /// the test doesn't depend on dsf_writer's private const.
    fn bit_reverse(b: u8) -> u8 {
        let mut r = 0u8;
        let mut v = b;
        for _ in 0..8 {
            r = (r << 1) | (v & 1);
            v >>= 1;
        }
        r
    }

    fn run_extract(
        sectors: Vec<Vec<u8>>,
        channel_count: u8,
        format: OutputFormat,
    ) -> (Vec<u8>, ExtractStats) {
        run_extract_with(sectors, channel_count, format, None)
    }

    /// Same as `run_extract` but with an optional time filter. End
    /// LSN is set to the sector count.
    fn run_extract_with(
        sectors: Vec<Vec<u8>>,
        channel_count: u8,
        format: OutputFormat,
        time_filter: Option<TimeFilter>,
    ) -> (Vec<u8>, ExtractStats) {
        let end_lsn = sectors.len() as u64;
        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let mut opts = ExtractOptions::new(0, end_lsn, channel_count, format);
        if let Some(tf) = time_filter {
            opts = opts.with_time_filter(tf);
        }
        let stats = extract_track(&mut iso, &mut output, opts).expect("extract should succeed");
        (output.into_inner(), stats)
    }

    fn run_extract_report_with_dst_options(
        sectors: Vec<Vec<u8>>,
        channel_count: u8,
        dst: DstExtractionOptions,
    ) -> (Vec<u8>, ExtractReport) {
        let end_lsn = sectors.len() as u64;
        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let opts = ExtractOptions::new(0, end_lsn, channel_count, OutputFormat::DffDst);
        let report = extract_track_with_integrity_and_dst_options(
            &mut iso,
            &mut output,
            opts,
            ExtractIntegrityOptions::strict(),
            dst,
        )
        .expect("extract should succeed");
        (output.into_inner(), report)
    }

    #[test]
    fn extract_options_struct_literal_remains_source_compatible() {
        let opts = ExtractOptions {
            start_lsn: 0,
            end_lsn: 1,
            channel_count: 2,
            format: OutputFormat::Dsf,
            time_filter: None,
            id3_metadata: None,
            dff_metadata: None,
        };
        assert_eq!(opts.start_lsn, 0);
        assert_eq!(opts.end_lsn, 1);
        assert_eq!(opts.channel_count, 2);
        assert_eq!(opts.format, OutputFormat::Dsf);
    }

    #[test]
    fn extract_uncompressed_stereo_to_dff_preserves_bytes() {
        // One stereo uncompressed frame = 2 * 4704 = 9408 bytes.
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );
        let (out, stats) = run_extract(sectors, 2, OutputFormat::Dff);

        // DFF stereo header = 144 bytes. Audio payload starts at 144,
        // length = 9408 (even — no pad byte).
        assert_eq!(out.len(), 144 + 9408);
        assert_eq!(&out[144..144 + 9408], &frame[..]);
        // DSD-data.chunk_data_size (BE u64 at offset 136) = 9408.
        assert_eq!(read_u64_be(&out, 136), 9408);
        assert_eq!(stats.frames_read, 1);
        assert_eq!(stats.audio_bytes, 9408);
        // Hash-pinned canonical output. If this fails after an
        // intentional output-format change (PR 3c DFF footers,
        // etc.), copy the actual hash from the failure message.
        assert_eq!(
            sha256_hex(&out),
            "10c9f7c4adb39d98bc7b6056a79afdcf34df23ed9d85e6e6a108201d37e91961",
        );
    }

    #[test]
    fn extract_uncompressed_stereo_to_dff_dst_writes_decodable_dstf() {
        let frame = vec![0u8; FRAME_SIZE_UNCOMPRESSED * 2];
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );
        let (out, report) = run_extract_report_with_dst_options(
            sectors,
            2,
            DstExtractionOptions::raw_fallback_compatibility(),
        );
        let stats = report.stats;

        let mut cursor = std::io::Cursor::new(out.clone());
        let info = crate::container::inspect_dsd_container(&mut cursor).unwrap();
        assert_eq!(info.compression, crate::container::DsdCompression::Dst);
        assert_eq!(info.channel_count, 2);
        assert_eq!(info.sample_count_per_channel, Some(37_632));

        let dstf = out.windows(4).position(|w| w == b"DSTF").unwrap();
        let dstf_size = read_u64_be(&out, dstf + 4) as usize;
        let dstf_payload = &out[dstf + 12..dstf + 12 + dstf_size];
        assert_eq!(crate::dst::decode_frame(dstf_payload, 2).unwrap(), frame);

        let frte = out.windows(4).position(|w| w == b"FRTE").unwrap();
        assert_eq!(&out[frte + 12..frte + 16], &1u32.to_be_bytes());
        assert_eq!(&out[frte + 16..frte + 18], &75u16.to_be_bytes());

        assert_eq!(stats.frames_read, 1);
        assert_eq!(stats.audio_bytes, 9408);
    }

    #[test]
    fn extract_uncompressed_mono_to_dff_dst_rejects_implicit_raw_fallback() {
        let frame = vec![0u8; FRAME_SIZE_UNCOMPRESSED];
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );
        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let opts = ExtractOptions::new(0, sectors.len() as u64, 1, OutputFormat::DffDst);
        let err = extract_track(&mut iso, &mut output, opts).unwrap_err();
        match err {
            ExtractError::Io(io_err) => {
                assert_eq!(io_err.kind(), io::ErrorKind::InvalidInput);
                let msg = io_err.to_string();
                assert!(
                    msg.contains("raw fallback") || msg.contains("unavailable") || msg.contains("DSDIFF/DSD"),
                    "unexpected error: {}",
                    io_err
                );
            }
            other => panic!("unexpected error: {:?}", other),
        }

        let (dff, stats) = run_extract(sectors, 1, OutputFormat::Dff);
        assert_eq!(stats.frames_read, 1);
        assert_eq!(stats.audio_bytes, FRAME_SIZE_UNCOMPRESSED as u64);
        let mut cursor = std::io::Cursor::new(dff.clone());
        let info = crate::container::inspect_dsd_container(&mut cursor).unwrap();
        assert_eq!(info.compression, crate::container::DsdCompression::Dsd);
        let start = info.data_offset as usize;
        assert_eq!(&dff[start..start + FRAME_SIZE_UNCOMPRESSED], &frame[..]);
    }

    #[test]
    fn extract_dst_source_to_dff_dst_preserves_original_dstf_payload() {
        let frame = vec![0u8; FRAME_SIZE_UNCOMPRESSED * 2];
        let encoded = crate::dst::encode_uncompressed_frame_interleaved(&frame, 2)
            .expect("raw DST source fixture should encode");
        let sectors = synth_dst_frame_sectors(
            &encoded,
            2,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );

        let (out, stats) = run_extract(sectors, 2, OutputFormat::DffDst);
        let dstf = out.windows(4).position(|w| w == b"DSTF").unwrap();
        let dstf_size = read_u64_be(&out, dstf + 4) as usize;
        let dstf_payload = &out[dstf + 12..dstf + 12 + dstf_size];

        assert_eq!(dstf_payload, encoded.as_slice());
        assert_eq!(crate::dst::decode_frame(dstf_payload, 2).unwrap(), frame);
        assert_eq!(stats.frames_read, 1);
        assert_eq!(stats.audio_bytes, FRAME_SIZE_UNCOMPRESSED as u64 * 2);
    }

    #[test]
    fn extraction_can_explicitly_decode_source_dst_to_dsdiff_dsd() {
        let frame = vec![0u8; FRAME_SIZE_UNCOMPRESSED * 2];
        let encoded = crate::dst::encode_uncompressed_frame_interleaved(&frame, 2)
            .expect("raw DST source fixture should encode");
        let sectors = synth_dst_frame_sectors(
            &encoded,
            2,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );

        let (out, report) = run_extract_report_with_dst_options(
            sectors,
            2,
            DstExtractionOptions::decode_to_dsdiff_dsd(),
        );
        assert!(report.dff_dst_stats().is_none());
        assert!(out.windows(4).all(|w| w != b"DSTF"));

        let mut cursor = std::io::Cursor::new(out.clone());
        let info = crate::container::inspect_dsd_container(&mut cursor).unwrap();
        assert_eq!(info.compression, crate::container::DsdCompression::Dsd);
        assert_eq!(info.channel_count, 2);
        let start = info.data_offset as usize;
        assert_eq!(&out[start..start + frame.len()], &frame[..]);
        assert_eq!(report.stats.frames_read, 1);
        assert_eq!(report.stats.audio_bytes, frame.len() as u64);
    }

    #[test]
    fn extraction_raw_fallback_mode_threads_encoder_policy() {
        let frame = vec![0u8; FRAME_SIZE_UNCOMPRESSED];
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );
        let (out, report) = run_extract_report_with_dst_options(
            sectors,
            1,
            DstExtractionOptions::raw_fallback_compatibility(),
        );
        let stats = report.dff_dst_stats().expect("DFF/DST stats should be present");
        assert_eq!(stats.frames_written, 1);
        assert_eq!(stats.raw_frames_written, 1);
        assert_eq!(stats.predictive_frames_written, 0);
        assert_eq!(stats.frames[0].mode, crate::dff_dst_writer::DffDstFrameMode::RawFallback);
        let dstf = out.windows(4).position(|w| w == b"DSTF").unwrap();
        let dstf_size = read_u64_be(&out, dstf + 4) as usize;
        let dstf_payload = &out[dstf + 12..dstf + 12 + dstf_size];
        assert_eq!(dstf_payload[0], 0);
        assert_eq!(crate::dst::decode_frame(dstf_payload, 1).unwrap(), frame);
    }

    #[test]
    fn extraction_passthrough_only_rejects_plain_dsd() {
        let frame = vec![0u8; FRAME_SIZE_UNCOMPRESSED * 2];
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );
        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let opts = ExtractOptions::new(0, sectors.len() as u64, 2, OutputFormat::DffDst);
        let err = extract_track_with_dst_options(
            &mut iso,
            &mut output,
            opts,
            DstExtractionOptions::passthrough_only(),
        )
        .unwrap_err();
        match err {
            ExtractError::Io(io_err) => {
                assert_eq!(io_err.kind(), io::ErrorKind::InvalidInput);
                assert!(io_err.to_string().contains("passthrough-only"));
            }
            other => panic!("unexpected error: {:?}", other),
        }
    }

    #[test]
    fn extraction_corpus_analysis_preserves_source_dst_and_records_attempt() {
        let frame = vec![0u8; FRAME_SIZE_UNCOMPRESSED * 2];
        let encoded = crate::dst::encode_uncompressed_frame_interleaved(&frame, 2)
            .expect("raw DST source fixture should encode");
        let sectors = synth_dst_frame_sectors(
            &encoded,
            2,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );

        let (out, report) = run_extract_report_with_dst_options(
            sectors,
            2,
            DstExtractionOptions::corpus_analysis(),
        );
        let dstf = out.windows(4).position(|w| w == b"DSTF").unwrap();
        let dstf_size = read_u64_be(&out, dstf + 4) as usize;
        let dstf_payload = &out[dstf + 12..dstf + 12 + dstf_size];
        assert_eq!(dstf_payload, encoded.as_slice());

        let stats = report.dff_dst_stats().expect("DFF/DST stats should be present");
        assert_eq!(stats.frames_written, 1);
        assert_eq!(stats.passthrough_frames_written, 1);
        assert_eq!(stats.encode_attempts, 1);
        assert!(stats.predictive_candidates > 0);
    }

    #[test]
    fn dst_extraction_presets_separate_policy_from_mechanism() {
        let mut encoder = crate::dst::DstEncoderOptions::default();
        encoder.minimum_savings_bytes = 17;
        encoder.raw_fallback = RawDstFallbackPolicy::Enabled;

        let opts = DstExtractionOptions::portable().with_encoder_options(encoder.clone());
        assert_eq!(opts.source_dst, SourceDstHandling::PassthroughExistingDst);
        assert_eq!(opts.plain_dsd, PlainDsdDstHandling::EncodeUncompressedSourceToDst);
        assert_eq!(opts.encoder.minimum_savings_bytes, 17);
        assert_eq!(opts.encoder.raw_fallback, RawDstFallbackPolicy::Enabled);
        assert_eq!(opts.encoder_for_plain_dsd().raw_fallback, RawDstFallbackPolicy::Disabled);

        let raw = DstExtractionOptions::raw_fallback_compatibility();
        assert_eq!(
            raw.plain_dsd,
            PlainDsdDstHandling::EncodeUncompressedSourceToDstWithRawFallback
        );
        assert_eq!(
            raw.encoder_for_plain_dsd().raw_fallback,
            RawDstFallbackPolicy::Enabled
        );

        let decode = DstExtractionOptions::decode_to_dsdiff_dsd();
        assert_eq!(decode.source_dst, SourceDstHandling::DecodeToDsdiffDsd);
        assert_eq!(
            decode.plain_dsd,
            PlainDsdDstHandling::WriteUncompressedSourceAsDsdiffDsd
        );
        let decode_builder =
            DstExtractionOptions::portable().with_source_dst_handling(SourceDstHandling::DecodeToDsdiffDsd);
        assert_eq!(
            decode_builder.plain_dsd,
            PlainDsdDstHandling::WriteUncompressedSourceAsDsdiffDsd
        );
        let restored = decode_builder.with_source_dst_handling(SourceDstHandling::PassthroughExistingDst);
        assert_eq!(
            restored.plain_dsd,
            PlainDsdDstHandling::EncodeUncompressedSourceToDst
        );

        let corpus = DstExtractionOptions::corpus_analysis();
        assert_eq!(corpus.source_dst, SourceDstHandling::AnalyzeThenPassthroughExistingDst);
        assert_eq!(corpus.plain_dsd, PlainDsdDstHandling::EncodeUncompressedSourceToDst);
        assert!(
            corpus.encoder.candidate_prediction_orders.len()
                > DstExtractionOptions::fast()
                    .encoder
                    .candidate_prediction_orders
                    .len()
        );
    }

    #[test]
    fn extract_uncompressed_stereo_to_dsf_demuxes_correctly() {
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );
        let (out, stats) = run_extract(sectors, 2, OutputFormat::Dsf);

        // DSF header = 92 bytes. Per-channel data = 4704 bytes,
        // emitted as one full 4096-byte block + 608 real + 3488 zero
        // bytes in a second block. So per channel: 2 * 4096 = 8192.
        // Total file: 92 + 2 channels * 8192 = 92 + 16384 = 16476.
        assert_eq!(out.len(), 92 + 2 * 2 * BLOCK_SIZE_PER_CHANNEL);

        // ch0 block 0 at offset 92..(92+4096). Each byte i is the
        // bit-reverse of frame[i * 2] (channel 0 = even-indexed
        // bytes of the interleaved input).
        for i in 0..BLOCK_SIZE_PER_CHANNEL {
            assert_eq!(
                out[92 + i],
                bit_reverse(frame[i * 2]),
                "ch0 block0 byte {} mismatch",
                i
            );
        }
        // ch1 block 0 at offset (92+4096)..(92+2*4096). Channel 1 =
        // odd-indexed bytes.
        let ch1_b0 = 92 + BLOCK_SIZE_PER_CHANNEL;
        for i in 0..BLOCK_SIZE_PER_CHANNEL {
            assert_eq!(
                out[ch1_b0 + i],
                bit_reverse(frame[i * 2 + 1]),
                "ch1 block0 byte {} mismatch",
                i
            );
        }
        // ch0 block 1: first 608 bytes are real (continuing the
        // even-indexed stream), rest are zero pad.
        let ch0_b1 = 92 + 2 * BLOCK_SIZE_PER_CHANNEL;
        for i in 0..608 {
            assert_eq!(
                out[ch0_b1 + i],
                bit_reverse(frame[(BLOCK_SIZE_PER_CHANNEL + i) * 2]),
                "ch0 block1 real byte {} mismatch",
                i
            );
        }
        for i in 608..BLOCK_SIZE_PER_CHANNEL {
            assert_eq!(out[ch0_b1 + i], 0, "ch0 block1 pad byte {} not zero", i);
        }
        // sample_count (fmt chunk, offset 64, LE u64) = real bits per
        // channel = 4704 * 8 = 37_632. No padding contribution.
        assert_eq!(read_u64_le(&out, 64), (FRAME_SIZE_UNCOMPRESSED as u64) * 8);
        assert_eq!(stats.frames_read, 1);
        assert_eq!(stats.audio_bytes, 9408);
        // Hash-pinned canonical output. Re-derive on writer changes
        // (PR 3b DSF ID3 footer, etc.).
        assert_eq!(
            sha256_hex(&out),
            "f19d02521726829bf74bf410dfaac73e13a46e6783e1a428b41b7ff1c52c089c",
        );
    }

    #[test]
    fn extract_six_channel_to_dff_passes_clustered_bytes_through() {
        // One 6-channel uncompressed frame = 6 * 4704 = 28_224 bytes.
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 6);
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );
        let (out, _stats) = run_extract(sectors, 6, OutputFormat::Dff);

        // 6-channel DFF header = 160 bytes. Audio payload follows.
        assert_eq!(out.len(), 160 + 28_224);
        assert_eq!(&out[160..160 + 28_224], &frame[..]);
        // CHNL chunk_count = 6 (BE u16 at offset 76).
        assert_eq!(read_u16_be(&out, 76), 6);
        // Hash-pinned canonical output for 6-channel DFF.
        assert_eq!(
            sha256_hex(&out),
            "5c113971a54c52abba78c07fd2ff1a765e0b36630e7e05680a2710a79343c4d1",
        );
    }

    #[test]
    fn extract_six_channel_to_dsf_demuxes_correctly() {
        // One 6-channel uncompressed frame = 6 * 4704 = 28_224 bytes.
        // After write_interleaved: each of 6 channels gets 4704 bytes
        // (4096 in block 0 + 608 in block 1, padded to 4096 with
        // zeros). File = 92 header + 6 * 2 * 4096 = 49,244 bytes.
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 6);
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );
        let (out, stats) = run_extract(sectors, 6, OutputFormat::Dsf);

        // Structural.
        assert_eq!(out.len(), 92 + 6 * 2 * BLOCK_SIZE_PER_CHANNEL);
        assert_eq!(stats.frames_read, 1);
        assert_eq!(stats.audio_bytes, 28_224);

        // fmt chunk fields:
        //   channel_type = 7 (Surround51) at offset 48 (LE u32)
        //   channel_count = 6 at offset 52 (LE u32)
        //   sample_count = 4704 * 8 at offset 64 (LE u64) — same
        //     per-channel sample count as stereo since each channel
        //     still has FRAME_SIZE_UNCOMPRESSED real bytes.
        assert_eq!(read_u32_le(&out, 48), 7);
        assert_eq!(read_u32_le(&out, 52), 6);
        assert_eq!(read_u64_le(&out, 64), (FRAME_SIZE_UNCOMPRESSED as u64) * 8);

        // Per-channel block 0 first byte verifies the 6-channel
        // demux cycle: ch_c receives input bytes at indices
        // c, c+6, c+12, ... so ch_c's first byte = bit_reverse(frame[c]).
        for c in 0..6 {
            let block_start = 92 + c * BLOCK_SIZE_PER_CHANNEL;
            assert_eq!(
                out[block_start],
                bit_reverse(frame[c]),
                "ch{} block0 byte 0 mismatch",
                c,
            );
        }

        // Block 1 zero-pad zones: 608 real bytes + (4096 - 608)
        // zero-pad bytes per channel. Verify the pad zone for all
        // 6 channels.
        for c in 0..6 {
            let block_start = 92 + (6 + c) * BLOCK_SIZE_PER_CHANNEL;
            assert!(
                out[block_start + 608..block_start + BLOCK_SIZE_PER_CHANNEL]
                    .iter()
                    .all(|&b| b == 0),
                "ch{} block1 pad zone non-zero",
                c,
            );
        }

        // Hash-pinned canonical output for 6-channel DSF.
        // Re-derive on writer changes (PR 3b DSF ID3 footer, etc.).
        assert_eq!(
            sha256_hex(&out),
            "84a657bab020e3206afe62722deeb9b4b2374334afa99f7d56de4ba7607dc24f",
        );
    }

    #[test]
    fn extract_five_channel_to_dff_passes_clustered_bytes_through() {
        // 5-channel uncompressed = 5 * 4704 = 23_520 bytes per frame.
        // Real SACDs with 5.0 (no-LFE) surround exist; this pins the
        // 5-channel DFF orchestration path.
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 5);
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );
        let (out, _stats) = run_extract(sectors, 5, OutputFormat::Dff);

        // 5-channel DFF header = 156 bytes. Audio payload follows.
        assert_eq!(out.len(), 156 + 23_520);
        assert_eq!(&out[156..156 + 23_520], &frame[..]);
        // CHNL chunk_count = 5 (BE u16 at offset 76).
        assert_eq!(read_u16_be(&out, 76), 5);
        // Hash-pinned canonical output for 5-channel DFF.
        assert_eq!(
            sha256_hex(&out),
            "b5cdbac6d433b98b111e51a46d33d3f271551686ddfcfda1df849e20301dbb4f",
        );
    }

    #[test]
    fn extract_five_channel_to_dsf_demuxes_correctly() {
        // 5-channel uncompressed = 23_520 bytes per frame.
        // Per-channel real = 4704 bytes = 1 full block + 608 partial.
        // File = 92 header + 5 * 2 * 4096 = 41_052 bytes.
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 5);
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );
        let (out, stats) = run_extract(sectors, 5, OutputFormat::Dsf);

        // Structural.
        assert_eq!(out.len(), 92 + 5 * 2 * BLOCK_SIZE_PER_CHANNEL);
        assert_eq!(stats.frames_read, 1);
        assert_eq!(stats.audio_bytes, 23_520);

        // fmt chunk: channel_type=6 (Surround5), channel_count=5,
        // sample_count = 4704 * 8.
        assert_eq!(read_u32_le(&out, 48), 6);
        assert_eq!(read_u32_le(&out, 52), 5);
        assert_eq!(read_u64_le(&out, 64), (FRAME_SIZE_UNCOMPRESSED as u64) * 8);

        // Per-channel block 0 first byte verifies the 5-channel demux
        // cycle: ch_c receives input bytes at indices c, c+5, c+10, …
        for c in 0..5 {
            let block_start = 92 + c * BLOCK_SIZE_PER_CHANNEL;
            assert_eq!(
                out[block_start],
                bit_reverse(frame[c]),
                "ch{} block0 byte 0 mismatch",
                c,
            );
        }

        // Block 1 zero-pad zones: 608 real bytes + 3488 zero pad per
        // channel. Verify the pad zone for all 5 channels.
        for c in 0..5 {
            let block_start = 92 + (5 + c) * BLOCK_SIZE_PER_CHANNEL;
            assert!(
                out[block_start + 608..block_start + BLOCK_SIZE_PER_CHANNEL]
                    .iter()
                    .all(|&b| b == 0),
                "ch{} block1 pad zone non-zero",
                c,
            );
        }

        // Hash-pinned canonical output for 5-channel DSF.
        assert_eq!(
            sha256_hex(&out),
            "74fc7f71c95448f429dba77d21d338bb1b7384131cee907068d197dc2b9955bd",
        );
    }

    #[test]
    fn extract_six_channel_with_filter_drops_out_of_range_dsf() {
        // Two 6-channel frames: tc=100 (dropped), tc=200 (kept).
        // Filter [150, 250). Verifies 6-channel + filter interaction
        // on the DSF format path.
        let frame_pre = pattern(FRAME_SIZE_UNCOMPRESSED * 6);
        let frame_mid: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 6))
            .map(|i| ((i + 17) & 0xFF) as u8)
            .collect();
        let mut sectors = synth_uncompressed_frame_sectors(&frame_pre, tc_at(100));
        sectors.extend(synth_uncompressed_frame_sectors(&frame_mid, tc_at(200)));

        let (out, stats) = run_extract_with(
            sectors,
            6,
            OutputFormat::Dsf,
            Some(TimeFilter::new(150, 100)),
        );

        assert_eq!(stats.frames_read, 1);
        assert_eq!(stats.audio_bytes, 28_224);
        // File = 92 + 6 channels × 2 blocks × 4096 = 49_244 bytes.
        assert_eq!(out.len(), 92 + 6 * 2 * BLOCK_SIZE_PER_CHANNEL);
        // ch0 block 0 byte 0 = bit_reverse(frame_mid[0]); NOT frame_pre[0].
        // This is the load-bearing check that the kept frame survived
        // demux correctly.
        assert_eq!(out[92], bit_reverse(frame_mid[0]));
        // Hash-pinned: pins 6-channel + filter DSF interaction.
        assert_eq!(
            sha256_hex(&out),
            "bbd3af4d297ed2da380c56e02a9af69bbad81204d71ab54486ae05c820ebc8a9",
        );
    }

    #[test]
    fn extract_six_channel_with_filter_drops_out_of_range_dff() {
        // 6-channel + filter on the DFF path. Same setup as the DSF
        // variant above.
        let frame_pre = pattern(FRAME_SIZE_UNCOMPRESSED * 6);
        let frame_mid: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 6))
            .map(|i| ((i + 17) & 0xFF) as u8)
            .collect();
        let mut sectors = synth_uncompressed_frame_sectors(&frame_pre, tc_at(100));
        sectors.extend(synth_uncompressed_frame_sectors(&frame_mid, tc_at(200)));

        let (out, stats) = run_extract_with(
            sectors,
            6,
            OutputFormat::Dff,
            Some(TimeFilter::new(150, 100)),
        );

        assert_eq!(stats.frames_read, 1);
        assert_eq!(stats.audio_bytes, 28_224);
        // 6-channel DFF: 160 header + 28_224 audio = 28_384 bytes.
        assert_eq!(out.len(), 160 + 28_224);
        // Audio = just frame_mid (clustered passthrough).
        assert_eq!(&out[160..160 + 28_224], &frame_mid[..]);
        // Hash-pinned.
        assert_eq!(
            sha256_hex(&out),
            "9ac9eec69511b2faf5a4a77190c99f232b686f995ed283435330cac6dbe6f952",
        );
    }

    #[test]
    fn extract_partial_block_in_dsf_pads_with_zeros() {
        // Same input as the demux test, but assert ONLY the padding
        // contract: per-channel real bytes = 4704, which is 4096 +
        // 608. The 608 real bytes start each second block; the
        // remaining 3488 must be zero. Independent of the
        // bit-reverse correctness (covered by the demux test).
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );
        let (out, _) = run_extract(sectors, 2, OutputFormat::Dsf);

        let ch0_b1 = 92 + 2 * BLOCK_SIZE_PER_CHANNEL;
        let ch1_b1 = 92 + 3 * BLOCK_SIZE_PER_CHANNEL;
        // Zero-pad zones in both block 1's.
        assert!(out[ch0_b1 + 608..ch0_b1 + BLOCK_SIZE_PER_CHANNEL]
            .iter()
            .all(|&b| b == 0));
        assert!(out[ch1_b1 + 608..ch1_b1 + BLOCK_SIZE_PER_CHANNEL]
            .iter()
            .all(|&b| b == 0));
    }

    #[test]
    fn extract_bad_dst_frame_returns_decode_error() {
        let payload = vec![0xDEu8; 100];
        let sectors = vec![synth_dst_sector(
            &payload,
            2,
            1, // sector_count: decrements to 0 after the audio packet → complete
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        )];
        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let err = extract_track(
            &mut iso,
            &mut output,
            ExtractOptions::new(0, 1, 2, OutputFormat::Dff),
        )
        .expect_err("malformed DST frame must error");
        assert!(matches!(err, ExtractError::Dst(_)), "got {:?}", err);
    }

    #[test]
    fn extract_zero_frames_produces_header_only_dff() {
        // Empty range — nothing read, header-only output.
        let td = write_iso(&[]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let stats = extract_track(
            &mut iso,
            &mut output,
            ExtractOptions::new(0, 0, 2, OutputFormat::Dff),
        )
        .unwrap();
        let out = output.into_inner();
        assert_eq!(out.len(), 144);
        assert_eq!(read_u64_be(&out, 136), 0); // DSD-data.chunk_data_size
        assert_eq!(read_u64_be(&out, 4), 132); // FRM8.chunk_data_size
        assert_eq!(stats.frames_read, 0);
        assert_eq!(stats.audio_bytes, 0);
        // Hash-pinned: cross-test invariant with
        // `filter_drops_out_of_range_dst_frame_silently` — both
        // produce the same finalized empty 2-channel DFF.
        assert_eq!(
            sha256_hex(&out),
            "5eb7736a725cf433c7d7fc75ceb07942d758cd9d0b832667621d47f12f45bed9",
        );
    }

    #[test]
    fn extract_zero_frames_produces_header_only_dsf() {
        let td = write_iso(&[]);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let stats = extract_track(
            &mut iso,
            &mut output,
            ExtractOptions::new(0, 0, 2, OutputFormat::Dsf),
        )
        .unwrap();
        let out = output.into_inner();
        assert_eq!(out.len(), 92);
        assert_eq!(read_u64_le(&out, 64), 0); // sample_count
        assert_eq!(stats.frames_read, 0);
        assert_eq!(stats.audio_bytes, 0);
        // Hash-pinned: pins the 92-byte empty DSF header (all fmt
        // chunk fields, magic, sample_count, etc.).
        assert_eq!(
            sha256_hex(&out),
            "e41afb408919fb9f59f0b7bd5b071dfc1fcaf3a5660706b8388ec5346f3be94a",
        );
    }

    // ============================================================
    //  PR 3a — TimeFilter tests
    // ============================================================

    #[test]
    fn time_filter_includes_in_range_frame() {
        // Range [150, 11281) — Solo Monk track 1 from PR 1e
        // validation.
        let tf = TimeFilter::new(150, 11131);
        assert!(!tf.includes(0), "tc 0 should be out (pre-gap)");
        assert!(!tf.includes(149), "tc 149 (one before start) should be out");
        assert!(tf.includes(150), "tc 150 (start, inclusive) should be in");
        assert!(tf.includes(5000), "tc 5000 (mid-track) should be in");
        assert!(
            tf.includes(11280),
            "tc 11280 (end-1, inclusive) should be in"
        );
        assert!(
            !tf.includes(11281),
            "tc 11281 (end, exclusive) should be out"
        );
        assert!(!tf.includes(50000), "tc 50000 (post-track) should be out");
    }

    #[test]
    fn time_filter_with_zero_duration_rejects_everything() {
        let tf = TimeFilter::new(100, 0);
        for tc in [0, 99, 100, 101, 1000, u32::MAX] {
            assert!(
                !tf.includes(tc),
                "tc {} should be rejected (duration=0)",
                tc
            );
        }
    }

    #[test]
    fn time_filter_overflow_saturates() {
        // start=u32::MAX - 50, duration=100 would mathematically end
        // at u32::MAX + 50; saturating arithmetic clamps end to
        // u32::MAX. The half-open interval [MAX-50, MAX) thus
        // includes MAX-50..MAX-1 inclusive but excludes MAX itself.
        // No panic, deterministic behavior on adversarial inputs.
        let tf = TimeFilter::new(u32::MAX - 50, 100);
        assert!(tf.includes(u32::MAX - 50), "start (inclusive) included");
        assert!(tf.includes(u32::MAX - 1), "in-range value included");
        assert!(!tf.includes(u32::MAX), "MAX is the exclusive saturated end");
        assert!(!tf.includes(u32::MAX - 51), "before start excluded");
    }

    #[test]
    fn extract_with_filter_drops_pre_gap_frames() {
        // Three frames at tc=100, tc=200, tc=300. Filter [150, 250)
        // keeps only the tc=200 frame.
        let frame_pre = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let frame_mid: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| ((i + 17) & 0xFF) as u8)
            .collect();
        let frame_post: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| ((i + 31) & 0xFF) as u8)
            .collect();
        let mut sectors = synth_uncompressed_frame_sectors(&frame_pre, tc_at(100));
        sectors.extend(synth_uncompressed_frame_sectors(&frame_mid, tc_at(200)));
        sectors.extend(synth_uncompressed_frame_sectors(&frame_post, tc_at(300)));

        let (out, stats) = run_extract_with(
            sectors,
            2,
            OutputFormat::Dff,
            Some(TimeFilter::new(150, 100)), // range [150, 250)
        );

        assert_eq!(stats.frames_read, 1, "only frame_mid kept");
        assert_eq!(stats.audio_bytes, 9408);
        // DFF header (144) + 9408 audio bytes (just frame_mid).
        assert_eq!(out.len(), 144 + 9408);
        assert_eq!(&out[144..144 + 9408], &frame_mid[..]);
        // Hash-pinned: pins the filter execution path output.
        assert_eq!(
            sha256_hex(&out),
            "785b247e0cb9a3b0a124d312f9024d89893d04fa961781faa31f129a05a4b97c",
        );
    }

    #[test]
    fn extract_with_filter_drops_post_track_frames() {
        // Three frames at tc=100, tc=200, tc=300. Filter [50, 200)
        // keeps tc=100 only (tc=200 is at the exclusive end).
        let frame_a = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let frame_b: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| ((i + 17) & 0xFF) as u8)
            .collect();
        let frame_c: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| ((i + 31) & 0xFF) as u8)
            .collect();
        let mut sectors = synth_uncompressed_frame_sectors(&frame_a, tc_at(100));
        sectors.extend(synth_uncompressed_frame_sectors(&frame_b, tc_at(200)));
        sectors.extend(synth_uncompressed_frame_sectors(&frame_c, tc_at(300)));

        let (out, stats) = run_extract_with(
            sectors,
            2,
            OutputFormat::Dff,
            Some(TimeFilter::new(50, 150)), // range [50, 200)
        );

        assert_eq!(stats.frames_read, 1, "only frame_a kept (tc=100)");
        assert_eq!(&out[144..144 + 9408], &frame_a[..]);
    }

    #[test]
    fn extract_with_filter_boundary_frames() {
        // tc=150 (= start, INCLUDED), tc=11280 (= end-1, INCLUDED),
        // tc=11281 (= end, EXCLUDED). Filter {start:150, dur:11131}.
        let frame_at_start = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let frame_at_end_minus_one: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| ((i + 7) & 0xFF) as u8)
            .collect();
        let frame_at_end: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| ((i + 41) & 0xFF) as u8)
            .collect();
        let mut sectors = synth_uncompressed_frame_sectors(&frame_at_start, tc_at(150));
        sectors.extend(synth_uncompressed_frame_sectors(
            &frame_at_end_minus_one,
            tc_at(11280),
        ));
        sectors.extend(synth_uncompressed_frame_sectors(
            &frame_at_end,
            tc_at(11281),
        ));

        let (out, stats) = run_extract_with(
            sectors,
            2,
            OutputFormat::Dff,
            Some(TimeFilter::new(150, 11131)), // range [150, 11281)
        );

        assert_eq!(stats.frames_read, 2, "start and end-1 kept; end excluded");
        assert_eq!(stats.audio_bytes, 9408 * 2);
        assert_eq!(&out[144..144 + 9408], &frame_at_start[..]);
        assert_eq!(
            &out[144 + 9408..144 + 9408 * 2],
            &frame_at_end_minus_one[..]
        );
    }

    #[test]
    fn extract_with_filter_on_dsf_drops_out_of_range_frames() {
        // Two frames; only tc=200 in range. DSF demuxes + bit-reverses
        // just that single frame's bytes.
        let frame_pre = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let frame_mid: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| ((i + 17) & 0xFF) as u8)
            .collect();
        let mut sectors = synth_uncompressed_frame_sectors(&frame_pre, tc_at(100));
        sectors.extend(synth_uncompressed_frame_sectors(&frame_mid, tc_at(200)));

        let (out, stats) = run_extract_with(
            sectors,
            2,
            OutputFormat::Dsf,
            Some(TimeFilter::new(150, 100)), // range [150, 250) — drops pre
        );

        assert_eq!(stats.frames_read, 1);
        // Per-channel real bytes = 4704 = 1 full block + 608 partial.
        // File = 92 header + 2 * 2 * 4096 = 16476 bytes.
        assert_eq!(out.len(), 92 + 2 * 2 * BLOCK_SIZE_PER_CHANNEL);
        // ch0 block 0 byte 0 = bit_reverse of frame_mid[0] (not
        // frame_pre[0]).
        assert_eq!(out[92], bit_reverse(frame_mid[0]));
        assert_eq!(out[92 + 4096], bit_reverse(frame_mid[1]));
        // sample_count = 4704 * 8 (real bytes/channel × 8 bits).
        assert_eq!(read_u64_le(&out, 64), (FRAME_SIZE_UNCOMPRESSED as u64) * 8);
        // Hash-pinned: pins DSF + filter interaction.
        assert_eq!(
            sha256_hex(&out),
            "fe112487cab4fb38be81212595f29038fd1eaaaaccd7a487bebb50c9ad71f0b9",
        );
    }

    #[test]
    fn extract_with_id3_metadata_appends_footer_and_updates_dsf_header() {
        // Verifies DsfWriter's footer support end-to-end:
        // - the rendered ID3 bytes appear after the audio payload
        // - DSD chunk's metadata_offset points to the footer
        // - total_file_size includes the footer length
        // - audio bytes still hash to the same canonical value
        //   (regression: PR 1e audio gate must hold when footer present)
        use crate::id3::{render_id3v24, Id3Metadata};

        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );
        let meta = Id3Metadata {
            tit2: Some("TEST TITLE".into()),
            ..Default::default()
        };
        let footer_bytes = render_id3v24(&meta);

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let opts = ExtractOptions::new(0, sectors.len() as u64, 2, OutputFormat::Dsf)
            .with_id3_metadata(meta.clone());
        let stats = extract_track(&mut iso, &mut output, opts).unwrap();
        let out = output.into_inner();

        // Audio bytes: 1 stereo frame = 9408 bytes → ch0 4096 + 608 pad,
        // ch1 4096 + 608 pad → audio_data_size = 16384.
        let audio_data_size = 16384u64;
        let expected_total = 92 + audio_data_size + footer_bytes.len() as u64;
        assert_eq!(out.len() as u64, expected_total);

        // DSD chunk header fields (LE u64 at the relevant offsets):
        // total_file_size at 12..20, metadata_offset at 20..28.
        assert_eq!(
            read_u64_le(&out, 12),
            expected_total,
            "total_file_size must include footer length",
        );
        assert_eq!(
            read_u64_le(&out, 20),
            92 + audio_data_size,
            "metadata_offset must point to footer start",
        );

        // Footer bytes appear verbatim after the audio.
        let footer_start = (92 + audio_data_size) as usize;
        assert_eq!(
            &out[footer_start..footer_start + footer_bytes.len()],
            &footer_bytes[..],
        );

        assert_eq!(stats.frames_read, 1);
        assert_eq!(stats.audio_bytes, 9408);
    }

    #[test]
    fn extract_with_dff_metadata_appends_footer_and_updates_frm8() {
        // End-to-end test: DffWriter's footer support correctly
        // attaches the rendered footer and updates FRM8.chunk_data_size.
        use crate::dff_footer::{render_dff_footer, DffMetadata};
        use crate::id3::Id3Metadata;

        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );
        let meta = DffMetadata {
            diar: Some("ARTIST".into()),
            diti: Some("TITLE".into()),
            duration_minutes_total: 0,
            duration_seconds: 1,
            duration_frames: 0,
            disc_date_year: 2026,
            disc_date_month_1_indexed: 5,
            disc_date_day: 13,
            disc_or_album_title: "ALBUM".into(),
            creation_year: 2026,
            creation_month_0_indexed: 4,
            creation_day: 13,
            creation_hour: 12,
            creation_minute: 0,
            creating_machine: "test".into(),
            id3: Id3Metadata {
                tit2: Some("TITLE".into()),
                ..Default::default()
            },
        };
        let footer_bytes = render_dff_footer(&meta);

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let opts = ExtractOptions::new(0, sectors.len() as u64, 2, OutputFormat::Dff)
            .with_dff_metadata(meta);
        let _stats = extract_track(&mut iso, &mut output, opts).unwrap();
        let out = output.into_inner();

        // Stereo DFF: header = 144, audio = 9408 (even, no pad)
        let header = 144usize;
        let audio = 9408usize;
        let footer = footer_bytes.len();
        assert_eq!(out.len(), header + audio + footer);

        // FRM8.chunk_data_size at offset 4..12 (BE u64) =
        // header + audio + footer - 12.
        let frm8_size = read_u64_be(&out, 4);
        assert_eq!(frm8_size as usize, header + audio + footer - 12);

        // Footer bytes appear verbatim after audio.
        let footer_start = header + audio;
        assert_eq!(&out[footer_start..footer_start + footer], &footer_bytes[..]);
    }

    #[test]
    fn extract_dff_dst_with_dff_metadata_appends_footer_and_updates_frm8() {
        use crate::dff_footer::{render_dff_footer, DffMetadata};
        use crate::id3::Id3Metadata;

        let frame = vec![0u8; FRAME_SIZE_UNCOMPRESSED * 2];
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );
        let meta = DffMetadata {
            diar: Some("ARTIST".into()),
            diti: Some("TITLE".into()),
            duration_minutes_total: 0,
            duration_seconds: 1,
            duration_frames: 0,
            disc_date_year: 2026,
            disc_date_month_1_indexed: 5,
            disc_date_day: 13,
            disc_or_album_title: "ALBUM".into(),
            creation_year: 2026,
            creation_month_0_indexed: 4,
            creation_day: 13,
            creation_hour: 12,
            creation_minute: 0,
            creating_machine: "test".into(),
            id3: Id3Metadata {
                tit2: Some("TITLE".into()),
                ..Default::default()
            },
        };
        let footer_bytes = render_dff_footer(&meta);

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let opts = ExtractOptions::new(0, sectors.len() as u64, 2, OutputFormat::DffDst)
            .with_dff_metadata(meta);
        let report = extract_track_with_integrity_and_dst_options(
            &mut iso,
            &mut output,
            opts,
            ExtractIntegrityOptions::strict(),
            DstExtractionOptions::raw_fallback_compatibility(),
        )
        .unwrap();
        assert!(report.dff_dst_stats().is_some());

        let out = output.into_inner();
        assert_eq!(read_u64_be(&out, 4) as usize, out.len() - 12);
        assert!(out.ends_with(&footer_bytes));
        let dsti_pos = out.windows(4).position(|w| w == b"DSTI").unwrap();
        let footer_pos = out.len() - footer_bytes.len();
        assert!(dsti_pos < footer_pos, "metadata footer must follow DSTI");
    }

    #[test]
    fn extract_no_dff_metadata_omits_footer() {
        // Regression: when dff_metadata = None, DFF output has
        // no footer (PR 1e behavior preserved).
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );
        let (out, _) = run_extract(sectors, 2, OutputFormat::Dff);
        // No footer → file size = header (144) + audio (9408) = 9552.
        assert_eq!(out.len(), 144 + 9408);
    }

    #[test]
    fn extract_no_id3_metadata_leaves_metadata_offset_zero() {
        // Regression: when id3_metadata = None, DsfWriter must NOT
        // append a footer and the DSD-chunk's metadata_offset must
        // be 0 (matches PR 1e canonical Solo Monk output mode).
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let sectors = synth_uncompressed_frame_sectors(
            &frame,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );
        let (out, _) = run_extract(sectors, 2, OutputFormat::Dsf);
        assert_eq!(read_u64_le(&out, 20), 0, "no footer → metadata_offset = 0");
    }

    #[test]
    fn filter_drops_out_of_range_dst_frame_silently() {
        // Critical ordering check: filter MUST run before the DST
        // check. Out-of-range DST frames should drop silently (no
        // DST decode error) — matching sacd_extract's
        // frame_read_callback nesting where the timecode filter is
        // the outer guard.
        //
        // If someone refactors to DST-then-filter, the in-range
        // case still errors but THIS case starts erroring too,
        // diverging from sacd_extract behavior. This test pins the
        // semantic contract.
        let payload = vec![0xDEu8; 100];
        let sectors = vec![synth_dst_sector(
            &payload,
            2,
            1,
            tc_at(50), // tc=50, outside filter [150, 250)
        )];
        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let opts = ExtractOptions::new(0, 1, 2, OutputFormat::Dff)
            .with_time_filter(TimeFilter::new(150, 100));
        let stats = extract_track(&mut iso, &mut output, opts)
            .expect("out-of-range DST must drop silently, not error");
        assert_eq!(stats.frames_read, 0);
        assert_eq!(stats.audio_bytes, 0);
        // Output is a valid header-only DFF (filter dropped everything).
        let out = output.into_inner();
        assert_eq!(out.len(), 144);
        // Hash-pinned: pins the 2-channel filter-drops-all DFF output.
        // MUST equal the hash in `extract_zero_frames_produces_header_only_dff`
        // (cross-test invariant: both paths produce identical 144-byte
        // finalized empty DFF headers via serialize_header(2, _, 0)).
        assert_eq!(
            sha256_hex(&out),
            "5eb7736a725cf433c7d7fc75ceb07942d758cd9d0b832667621d47f12f45bed9",
        );
    }

    #[test]
    fn filter_keeps_in_range_dst_frame_then_errors() {
        // Complement to the silent-drop test: when filter includes
        // a DST frame, the orchestrator errors (because the decoder must run and report malformed data for
        // this synthetic payload). This pins the second half of the
        // filter-then-DST nesting.
        let payload = vec![0xDEu8; 100];
        let sectors = vec![synth_dst_sector(
            &payload,
            2,
            1,
            tc_at(200), // tc=200, inside filter [150, 250)
        )];
        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let opts = ExtractOptions::new(0, 1, 2, OutputFormat::Dff)
            .with_time_filter(TimeFilter::new(150, 100));
        let err = extract_track(&mut iso, &mut output, opts).expect_err("in-range DST must error");
        assert!(matches!(err, ExtractError::Dst(_)), "got {:?}", err);
    }

    #[test]
    fn extract_stats_reports_correct_counts_for_two_frames() {
        // Two complete stereo frames, back-to-back. First frame_start
        // sector for frame A, continuation sectors, then a fresh
        // frame_start sector for frame B which finalizes A, then more
        // continuation, then EOR flushes B.
        let frame_a = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let frame_b: Vec<u8> = (0..(FRAME_SIZE_UNCOMPRESSED * 2))
            .map(|i| ((i + 13) & 0xFF) as u8)
            .collect();
        let mut sectors = synth_uncompressed_frame_sectors(
            &frame_a,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 1,
            },
        );
        sectors.extend(synth_uncompressed_frame_sectors(
            &frame_b,
            Timecode {
                minutes: 0,
                seconds: 0,
                frames: 2,
            },
        ));
        let (out, stats) = run_extract(sectors, 2, OutputFormat::Dff);

        assert_eq!(stats.frames_read, 2);
        assert_eq!(stats.audio_bytes, 9408 * 2);
        // Concatenated audio = frame_a then frame_b.
        assert_eq!(&out[144..144 + 9408], &frame_a[..]);
        assert_eq!(&out[144 + 9408..144 + 9408 * 2], &frame_b[..]);
    }

    #[test]
    fn extract_stats_exposes_recovered_malformed_sector() {
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let mut sectors = vec![malformed_audio_sector_packet_too_large()];
        sectors.extend(synth_uncompressed_frame_sectors(&frame, tc_at(1)));

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let opts = ExtractOptions::new(0, sectors.len() as u64, 2, OutputFormat::Dff);
        let report = extract_track_with_integrity_options(
            &mut iso,
            &mut output,
            opts,
            ExtractIntegrityOptions::new().with_sector_recovery(true),
        )
        .unwrap();
        let stats = report.stats;
        let integrity = &report.integrity;

        assert_eq!(stats.frames_read, 1);
        assert_eq!(stats.audio_bytes, 9408);
        assert_eq!(integrity.sectors_read, sectors.len() as u64);
        assert_eq!(integrity.sectors_skipped, 1);
        assert_eq!(integrity.malformed_sectors, 1);
        assert_eq!(integrity.io_errors, 0);
        assert_eq!(integrity.frames_dropped_incomplete, 0);
        assert_eq!(integrity.recovery_events.len(), 1);
        assert_eq!(integrity.recovery_events[0].lsn, 0);
        assert_eq!(integrity.recovery_events[0].kind.to_string(), "malformed-sector");
        assert!(integrity.recovery_events[0].error.contains("LSN 0"));
        assert!(integrity.recovery_events[0].error.contains("packet length"));
        assert!(integrity.recovery_events[0].to_string().contains("LSN 0 malformed-sector"));
        assert!(report.integrity_loss_detected());
    }

    #[test]
    fn extract_stats_exposes_incomplete_frame_dropped_by_recovery() {
        let dropped = pattern(PART_SIZE);
        let kept = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let mut sectors = vec![synth_audio_sector(true, &dropped, tc_at(1))];
        sectors.push(malformed_audio_sector_packet_too_large());
        sectors.extend(synth_uncompressed_frame_sectors(&kept, tc_at(2)));

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let opts = ExtractOptions::new(0, sectors.len() as u64, 2, OutputFormat::Dff);
        let report = extract_track_with_integrity_options(
            &mut iso,
            &mut output,
            opts,
            ExtractIntegrityOptions::new().with_sector_recovery(true),
        )
        .unwrap();
        let stats = report.stats;
        let integrity = &report.integrity;

        assert_eq!(stats.frames_read, 1);
        assert_eq!(stats.audio_bytes, 9408);
        assert_eq!(integrity.sectors_skipped, 1);
        assert_eq!(integrity.malformed_sectors, 1);
        assert_eq!(integrity.frames_dropped_incomplete, 1);
        assert_eq!(integrity.dropped_frame_events.len(), 1);
        assert_eq!(integrity.dropped_frame_events()[0].lsn, 1);
        assert_eq!(integrity.dropped_frame_events()[0].bytes, PART_SIZE);
        assert!(integrity.dropped_frame_events()[0].to_string().contains("dropped incomplete DSD frame"));
        assert_eq!(integrity.recovery_events.len(), 1);
        assert_eq!(integrity.recovery_events[0].lsn, 1);
        assert!(integrity.recovery_events[0].error.contains("LSN 1"));
        assert!(report.integrity_loss_detected());
    }

    #[test]
    fn incomplete_trailing_frame_is_dropped_silently_in_normal_extraction() {
        let partial = pattern(PART_SIZE);
        let sectors = vec![synth_audio_sector(true, &partial, tc_at(1))];

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let opts = ExtractOptions::new(0, sectors.len() as u64, 2, OutputFormat::Dff);
        // A trailing incomplete frame at end-of-range is normal for SACD
        // track boundaries — the TOC sector range rarely aligns to DSD
        // frame boundaries. The frame is dropped from output but
        // extraction succeeds.
        let stats = extract_track(&mut iso, &mut output, opts)
            .expect("trailing incomplete frame at end-of-range should not fail extraction");
        assert_eq!(stats.frames_read, 0, "incomplete frame should not be emitted");
    }

    #[test]
    fn incomplete_frame_is_reported_in_recovery_extraction() {
        let partial = pattern(PART_SIZE);
        let sectors = vec![synth_audio_sector(true, &partial, tc_at(1))];

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let opts = ExtractOptions::new(0, sectors.len() as u64, 2, OutputFormat::Dff);
        let report = extract_track_with_integrity_options(
            &mut iso,
            &mut output,
            opts,
            ExtractIntegrityOptions::new().with_sector_recovery(true),
        )
        .unwrap();
        let stats = report.stats;
        let integrity = &report.integrity;

        assert_eq!(stats.frames_read, 0);
        assert_eq!(integrity.frames_dropped_incomplete, 1);
        assert_eq!(integrity.dropped_frame_events().len(), 1);
        assert_eq!(integrity.dropped_frame_events()[0].lsn, sectors.len() as u64);
        assert_eq!(integrity.dropped_frame_events()[0].bytes, PART_SIZE);
        assert!(report.integrity_loss_detected());
    }

    #[test]
    fn area_frame_format_is_authoritative_for_extraction() {
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let sectors = synth_uncompressed_frame_sectors(&frame, tc_at(1));

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let opts = ExtractOptions::new(0, sectors.len() as u64, 2, OutputFormat::Dff);
        let report = extract_track_with_integrity_options(
            &mut iso,
            &mut output,
            opts,
            ExtractIntegrityOptions::new().with_frame_format(FrameFormat::Dsd3In14),
        )
        .unwrap();
        let stats = report.stats;
        let integrity = &report.integrity;

        assert_eq!(stats.frames_read, 1);
        assert_eq!(integrity.frame_format_mismatches, 0);
        assert!(!report.integrity_loss_detected());
    }

    #[test]
    fn area_frame_format_mismatch_fails_normal_extraction() {
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let sectors = synth_uncompressed_frame_sectors(&frame, tc_at(1));

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let opts = ExtractOptions::new(0, sectors.len() as u64, 2, OutputFormat::Dff);
        let err = extract_track_with_integrity_options(
            &mut iso,
            &mut output,
            opts,
            ExtractIntegrityOptions::new().with_frame_format(FrameFormat::Dst),
        )
        .expect_err("area TOC says DST but sector header says DSD");
        assert!(matches!(err, ExtractError::Frame(FrameError::FrameFormatMismatch { .. })), "got {:?}", err);
    }

    #[test]
    fn area_frame_format_mismatch_is_reported_in_recovery_extraction() {
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let sectors = synth_uncompressed_frame_sectors(&frame, tc_at(1));

        let td = write_iso(&sectors);
        let mut iso = IsoReader::open(&td.path().join("test.iso")).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let opts = ExtractOptions::new(0, sectors.len() as u64, 2, OutputFormat::Dff);
        let report = extract_track_with_integrity_options(
            &mut iso,
            &mut output,
            opts,
            ExtractIntegrityOptions::new()
                .with_frame_format(FrameFormat::Dst)
                .with_sector_recovery(true),
        )
        .unwrap();
        let stats = report.stats;
        let integrity = &report.integrity;

        assert_eq!(stats.frames_read, 0);
        assert_eq!(integrity.sectors_skipped, sectors.len() as u64);
        assert_eq!(integrity.frame_format_mismatches, sectors.len() as u64);
        assert!(integrity.recovery_events().iter().all(|e| e.error.contains("frame-format mismatch")));
        assert!(report.integrity_loss_detected());
    }


    #[test]
    fn common_source_dsdiff_dsd_to_dsf_uses_extraction_sink() {
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let mut input = std::io::Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffWriter::new(&mut input, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_frame(&frame).unwrap();
            writer.finish().unwrap();
        }
        input.set_position(0);
        let mut source = crate::source_model::open_dsd_source(input).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());

        let report = write_dsd_source(
            &mut source,
            &mut output,
            DsdSourceExtractOptions::new(OutputFormat::Dsf),
        )
        .unwrap();
        assert_eq!(report.stats.frames_read, 1);
        assert_eq!(report.stats.audio_bytes, frame.len() as u64);

        output.set_position(0);
        let mut roundtrip = crate::source_model::open_dsd_source(output).unwrap();
        let mut decoded = Vec::new();
        while let Some(got) = roundtrip.next_source_frame().unwrap() {
            match got {
                DsdSourceFrame::Dsd(dsd) => decoded.extend_from_slice(&dsd.data),
                DsdSourceFrame::Dst(_) => panic!("DSF output must yield decoded DSD"),
            }
        }
        assert_eq!(decoded, frame);
    }

    #[test]
    fn common_source_plain_dsd_with_decode_to_dsdiff_dsd_writes_ordinary_dff() {
        let frame = pattern(FRAME_SIZE_UNCOMPRESSED * 2);
        let mut input = std::io::Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffWriter::new(&mut input, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_frame(&frame).unwrap();
            writer.finish().unwrap();
        }
        input.set_position(0);
        let mut source = crate::source_model::open_dsd_source(input).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());

        let report = write_dsd_source(
            &mut source,
            &mut output,
            DsdSourceExtractOptions::new(OutputFormat::DffDst)
                .with_dst_options(DstExtractionOptions::decode_to_dsdiff_dsd()),
        )
        .unwrap();
        assert_eq!(report.stats.frames_read, 1);
        assert_eq!(report.stats.audio_bytes, frame.len() as u64);
        assert!(report.dff_dst_stats().is_none());

        let out = output.into_inner();
        assert!(out.windows(4).all(|w| w != b"DSTF"));
        let mut cursor = std::io::Cursor::new(out.clone());
        let info = crate::container::inspect_dsd_container(&mut cursor).unwrap();
        assert_eq!(info.compression, crate::container::DsdCompression::Dsd);
        assert_eq!(info.channel_count, 2);
        let start = info.data_offset as usize;
        assert_eq!(&out[start..start + frame.len()], &frame[..]);
    }

    #[test]
    fn common_source_dsdiff_dst_to_dff_dst_preserves_source_dst_payload() {
        let frame = vec![0; FRAME_SIZE_UNCOMPRESSED * 2];
        let mut input = std::io::Cursor::new(Vec::<u8>::new());
        {
            let mut writer = DffDstWriter::new(&mut input, 2, SACD_SAMPLING_FREQUENCY).unwrap();
            writer.write_interleaved_frame_allowing_raw_fallback(&frame).unwrap();
            writer.finish().unwrap();
        }
        let input_bytes = input.into_inner();

        let mut original = crate::source_model::open_dsd_source(std::io::Cursor::new(input_bytes.clone())).unwrap();
        let original_encoded = match original.next_source_frame().unwrap().unwrap() {
            DsdSourceFrame::Dst(dst) => dst.encoded,
            DsdSourceFrame::Dsd(_) => panic!("expected DSDIFF/DST source"),
        };

        let mut source = crate::source_model::open_dsd_source(std::io::Cursor::new(input_bytes)).unwrap();
        let mut output = std::io::Cursor::new(Vec::<u8>::new());
        let report = write_dsd_source(
            &mut source,
            &mut output,
            DsdSourceExtractOptions::new(OutputFormat::DffDst),
        )
        .unwrap();
        assert_eq!(report.stats.frames_read, 1);
        let dst_stats = report.dff_dst_stats().unwrap();
        assert_eq!(dst_stats.passthrough_frames_written, 1);

        output.set_position(0);
        let mut roundtrip = crate::source_model::open_dsd_source(output).unwrap();
        let got_encoded = match roundtrip.next_source_frame().unwrap().unwrap() {
            DsdSourceFrame::Dst(dst) => dst.encoded,
            DsdSourceFrame::Dsd(_) => panic!("DffDst output must stay DST"),
        };
        assert_eq!(got_encoded, original_encoded);
    }


    #[derive(Debug)]
    struct PathTestSource {
        info: crate::source_model::DsdSourceInfo,
        frames: std::collections::VecDeque<SourceDsdFrame>,
    }

    impl PathTestSource {
        fn one_frame(channel_count: u16) -> Self {
            let frame = SourceDsdFrame {
                frame_index: 0,
                data: vec![0x69, 0x96, 0x00, 0xff],
                channel_count,
                sample_rate: SACD_SAMPLING_FREQUENCY,
                byte_order: crate::container::DsdByteOrder::MsbFirst,
                timecode: None,
                is_final: true,
            };
            Self {
                info: crate::source_model::DsdSourceInfo {
                    kind: crate::source_model::DsdSourceKind::DsdiffDsd,
                    channel_count,
                    sample_rate: SACD_SAMPLING_FREQUENCY,
                    compression: crate::container::DsdCompression::Dsd,
                    sample_count_per_channel: None,
                    container: None,
                    iso_range: None,
                },
                frames: std::collections::VecDeque::from([frame]),
            }
        }

        fn invalid_zero_channel() -> Self {
            Self {
                info: crate::source_model::DsdSourceInfo {
                    kind: crate::source_model::DsdSourceKind::DsdiffDsd,
                    channel_count: 0,
                    sample_rate: SACD_SAMPLING_FREQUENCY,
                    compression: crate::container::DsdCompression::Dsd,
                    sample_count_per_channel: None,
                    container: None,
                    iso_range: None,
                },
                frames: std::collections::VecDeque::new(),
            }
        }
    }

    impl DsdSource for PathTestSource {
        fn source_info(&self) -> &crate::source_model::DsdSourceInfo {
            &self.info
        }

        fn next_source_frame(&mut self) -> Result<Option<DsdSourceFrame>, DsdSourceError> {
            Ok(self.frames.pop_front().map(DsdSourceFrame::Dsd))
        }
    }

    #[test]
    fn write_dsd_source_to_path_commits_only_after_success() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("track.dff");
        let mut source = PathTestSource::one_frame(2);
        let report = write_dsd_source_to_path(
            &mut source,
            &final_path,
            DsdSourceExtractOptions::new(OutputFormat::Dff),
            OutputOverwritePolicy::RefuseExisting,
        )
        .unwrap();
        assert_eq!(report.stats.frames_read, 1);
        assert!(final_path.exists());
        assert_eq!(
            std::fs::read_dir(dir.path())
                .unwrap()
                .filter(|entry| entry.as_ref().unwrap().path() != final_path)
                .count(),
            0
        );
    }

    #[test]
    fn write_dsd_source_to_path_refuses_existing_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("track.dff");
        std::fs::write(&final_path, b"existing-good-file").unwrap();
        let mut source = PathTestSource::one_frame(2);
        let err = write_dsd_source_to_path(
            &mut source,
            &final_path,
            DsdSourceExtractOptions::new(OutputFormat::Dff),
            OutputOverwritePolicy::RefuseExisting,
        )
        .unwrap_err();
        assert!(matches!(err, ExtractToPathError::Transaction(OutputTransactionError::ExistingOutputRefused { .. })));
        assert_eq!(std::fs::read(&final_path).unwrap(), b"existing-good-file");
    }

    #[test]
    fn write_dsd_source_to_path_failure_removes_temp_and_keeps_no_final() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("track.dff");
        let mut source = PathTestSource::invalid_zero_channel();
        let err = write_dsd_source_to_path(
            &mut source,
            &final_path,
            DsdSourceExtractOptions::new(OutputFormat::Dff),
            OutputOverwritePolicy::RefuseExisting,
        )
        .unwrap_err();
        let temp_path = err.temp_path().unwrap().to_path_buf();
        assert!(!final_path.exists());
        assert!(!temp_path.exists());
    }

    #[test]
    fn write_dsd_source_to_path_forced_failure_preserves_existing_final() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("track.dff");
        std::fs::write(&final_path, b"existing-good-file").unwrap();
        let mut source = PathTestSource::invalid_zero_channel();
        let err = write_dsd_source_to_path(
            &mut source,
            &final_path,
            DsdSourceExtractOptions::new(OutputFormat::Dff),
            OutputOverwritePolicy::ReplaceExisting,
        )
        .unwrap_err();
        let temp_path = err.temp_path().unwrap().to_path_buf();
        assert_eq!(std::fs::read(&final_path).unwrap(), b"existing-good-file");
        assert!(!temp_path.exists());
    }

    #[test]
    fn write_dsd_source_to_path_repeated_forced_runs_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let final_path = dir.path().join("track.dff");
        let mut source = PathTestSource::one_frame(2);
        write_dsd_source_to_path(
            &mut source,
            &final_path,
            DsdSourceExtractOptions::new(OutputFormat::Dff),
            OutputOverwritePolicy::ReplaceExisting,
        )
        .unwrap();
        let first = std::fs::read(&final_path).unwrap();

        let mut source = PathTestSource::one_frame(2);
        write_dsd_source_to_path(
            &mut source,
            &final_path,
            DsdSourceExtractOptions::new(OutputFormat::Dff),
            OutputOverwritePolicy::ReplaceExisting,
        )
        .unwrap();
        assert_eq!(std::fs::read(&final_path).unwrap(), first);
    }

}
