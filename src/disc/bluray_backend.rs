//! Backend-neutral Blu-ray types for Phase 0.
//!
//! Backend adapters must translate their native structs into these types before
//! data reaches the rest of tonepoet. This keeps libbluray FFI isolated and
//! leaves room for another backend behind the same trait.

use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// BD-ROM primary audio coding values used by tonepoet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BluRayAudioCoding {
    Lpcm,
    Ac3,
    Eac3,
    Dts,
    TrueHd,
    DtsHd,
    DtsHdMaster,
}

impl BluRayAudioCoding {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lpcm => "LPCM",
            Self::Ac3 => "AC-3",
            Self::Eac3 => "E-AC-3",
            Self::Dts => "DTS",
            Self::TrueHd => "TrueHD",
            Self::DtsHd => "DTS-HD HR",
            Self::DtsHdMaster => "DTS-HD MA",
        }
    }

    #[must_use]
    pub const fn is_lossless(self) -> bool {
        matches!(self, Self::Lpcm | Self::TrueHd | Self::DtsHdMaster)
    }

    /// Higher values sort ahead of lower values.
    #[must_use]
    pub const fn codec_rank(self) -> u8 {
        match self {
            Self::Lpcm => 7,
            Self::TrueHd => 6,
            Self::DtsHdMaster => 5,
            Self::DtsHd => 4,
            Self::Dts => 3,
            Self::Eac3 => 2,
            Self::Ac3 => 1,
        }
    }

    /// Elementary stream extension used when extracting a raw audio elementary
    /// stream before containerization or decode.
    #[must_use]
    pub const fn elementary_extension(self) -> &'static str {
        match self {
            Self::Lpcm => "pcm",
            Self::Ac3 => "ac3",
            Self::Eac3 => "eac3",
            Self::Dts | Self::DtsHd | Self::DtsHdMaster => "dts",
            Self::TrueHd => "thd",
        }
    }

    /// Optional ffmpeg input format hint for raw elementary streams.
    #[must_use]
    pub const fn ffmpeg_format_hint(self) -> Option<&'static str> {
        match self {
            Self::Lpcm => Some("pcm_bluray"),
            Self::Ac3 => Some("ac3"),
            Self::Eac3 => Some("eac3"),
            Self::Dts | Self::DtsHd | Self::DtsHdMaster => Some("dts"),
            Self::TrueHd => Some("truehd"),
        }
    }
}

/// Distinguishes primary audio streams from Blu-ray secondary audio streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BluRayAudioStreamKind {
    Primary,
    Secondary,
}

impl BluRayAudioStreamKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Secondary => "secondary",
        }
    }
}

/// Opaque title selector returned by a backend.
///
/// The numeric title index and playlist number are exposed through accessors for
/// diagnostics, but callers cannot construct or mutate a key directly. Backend
/// implementations own the mapping between this key and their internal title
/// representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlurayTitleKey {
    title_index: u32,
    playlist_number: u32,
}

impl BlurayTitleKey {
    #[must_use]
    pub(crate) const fn from_libbluray(title_index: u32, playlist_number: u32) -> Self {
        Self {
            title_index,
            playlist_number,
        }
    }

    /// Zero-based libbluray title index. Exposed for diagnostics only.
    #[must_use]
    pub const fn title_index(self) -> u32 {
        self.title_index
    }

    /// Five-digit MPLS playlist number as reported by libbluray.
    #[must_use]
    pub const fn playlist_number(self) -> u32 {
        self.playlist_number
    }
}

/// Summary of a libbluray/BD-ROM playlist title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlurayTitleInfo {
    pub key: BlurayTitleKey,
    pub playlist_number: u32,
    pub duration_pts_90k: u64,
    pub angle_count: u8,
    pub chapter_count: u32,
    pub clip_count: u32,
}

impl BlurayTitleInfo {
    #[must_use]
    pub fn duration_secs(&self) -> f64 {
        self.duration_pts_90k as f64 / 90_000.0
    }
}

/// A title chapter in BD-ROM 90 kHz clock units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlurayChapterInfo {
    pub chapter_number: u32,
    pub start_pts_90k: u64,
    pub end_pts_90k: Option<u64>,
    pub duration_pts_90k: Option<u64>,
    pub byte_offset: Option<u64>,
    pub clip_ref: Option<u32>,
}

/// LPCM PES-header probe depth requested by a caller.
///
/// Plain `streams()` is strictly metadata-only and uses `ProbeDepth::None`.
/// Callers that want LPCM bit-depth discovery must opt in through
/// `streams_with_probe_policy` with `Bounded` or `Exhaustive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "depth")]
pub enum ProbeDepth {
    /// Do not read title data while enumerating stream metadata.
    None,
    /// Read at most `max_bytes` and stop once `max_duration` has elapsed.
    Bounded {
        max_bytes: u64,
        max_duration: Duration,
    },
    /// Read until every target LPCM PID is found, EOF is reached, or a read
    /// error occurs.
    Exhaustive,
}

impl ProbeDepth {
    pub const DEFAULT_MAX_BYTES: u64 = 256 * 1024 * 1024;
    pub const DEFAULT_MAX_DURATION: Duration = Duration::from_secs(3);

    #[must_use]
    pub const fn bounded_default() -> Self {
        Self::Bounded {
            max_bytes: Self::DEFAULT_MAX_BYTES,
            max_duration: Self::DEFAULT_MAX_DURATION,
        }
    }
}

impl Default for ProbeDepth {
    fn default() -> Self {
        Self::None
    }
}

/// Operational reason an LPCM PES-header probe stopped before all requested
/// primary LPCM PIDs yielded a valid Blu-ray LPCM subheader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum BlurayLpcmProbeStopReason {
    ByteLimit,
    TimeLimit,
    EndOfTitle,
    ReadError { message: String },
}

/// Parser-level reason a target PID did not yield a valid Blu-ray LPCM PES
/// payload header before the probe stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum BlurayLpcmPidProbeFailureReason {
    /// The probe did not see a payload-unit-start packet for this PID.
    PesStartNotFound,
    /// A PES prefix began for this PID, but the probe stopped before the full
    /// optional PES header plus Blu-ray LPCM four-byte payload header arrived.
    LpcmSubheaderIncomplete,
    /// Payload data for this PID did not begin with a PES start-code prefix.
    InvalidPesPrefix,
    /// A complete LPCM subheader arrived but used reserved or unsupported
    /// channel, sample-rate, or bit-depth codes.
    InvalidLpcmHeader { message: String },
}

/// Parser-level failure for one requested LPCM PID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlurayLpcmPidProbeFailure {
    pub pid: u16,
    pub reason: BlurayLpcmPidProbeFailureReason,
}

/// Material reason LPCM bit-depth probing failed for a stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum BlurayLpcmBitDepthProbeFailure {
    ByteLimit { missing_pids: Vec<BlurayLpcmPidProbeFailure> },
    TimeLimit { missing_pids: Vec<BlurayLpcmPidProbeFailure> },
    EndOfTitle { missing_pids: Vec<BlurayLpcmPidProbeFailure> },
    ReadError { message: String, missing_pids: Vec<BlurayLpcmPidProbeFailure> },
}

impl BlurayLpcmBitDepthProbeFailure {
    #[must_use]
    pub fn missing_pids(&self) -> &[BlurayLpcmPidProbeFailure] {
        match self {
            Self::ByteLimit { missing_pids }
            | Self::TimeLimit { missing_pids }
            | Self::EndOfTitle { missing_pids }
            | Self::ReadError { missing_pids, .. } => missing_pids,
        }
    }
}

/// Reason an LPCM stream was reported without probing title data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlurayLpcmNotProbedReason {
    /// The caller requested `ProbeDepth::None`.
    ProbePolicyNone,
    /// Probe initialization did not run. This should only appear in defensive
    /// fallback paths before a concrete probe report is applied.
    PrimaryProbeNotRun,
    /// Phase 0 reads the selected title's main transport stream only; secondary
    /// audio can live in a secondary/subpath stream.
    SecondaryStreamNotInMainTransport,
}

impl BlurayLpcmNotProbedReason {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ProbePolicyNone => "probe policy none",
            Self::PrimaryProbeNotRun => "primary probe not run",
            Self::SecondaryStreamNotInMainTransport => {
                "secondary stream not in main transport"
            }
        }
    }
}

/// Structured bit-depth status for a Blu-ray audio stream.
///
/// Non-LPCM streams use `NotApplicable`. LPCM streams must reach `Probed`
/// before materialization; otherwise the materializer validation gate returns a
/// targeted error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum BlurayLpcmBitDepth {
    NotApplicable,
    Probed { bit_depth: u32, scanned_bytes: u64 },
    ProbeFailed {
        reason: BlurayLpcmBitDepthProbeFailure,
        bytes_scanned: u64,
    },
    NotProbed { reason: BlurayLpcmNotProbedReason },
}

impl BlurayLpcmBitDepth {
    #[must_use]
    pub const fn bit_depth(&self) -> Option<u32> {
        match self {
            Self::Probed { bit_depth, .. } => Some(*bit_depth),
            Self::NotApplicable | Self::ProbeFailed { .. } | Self::NotProbed { .. } => None,
        }
    }

    #[must_use]
    pub const fn is_probed(&self) -> bool {
        matches!(self, Self::Probed { .. })
    }
}

/// Audio stream description lifted out of a BD-ROM clip stream table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlurayAudioStreamInfo {
    pub kind: BluRayAudioStreamKind,
    pub pid: u16,
    pub stream_index: u8,
    pub coding: BluRayAudioCoding,
    pub sample_rate: Option<u32>,
    pub bit_depth: BlurayLpcmBitDepth,
    pub channels: Option<u8>,
    pub channel_layout: Option<String>,
    pub language: Option<String>,
}

/// Materializer-level validation error for a stream that cannot be rendered
/// safely from metadata alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "error")]
pub enum BlurayMaterializationValidationError {
    LpcmBitDepthRequired {
        pid: u16,
        stream_index: u8,
        kind: BluRayAudioStreamKind,
        bit_depth: BlurayLpcmBitDepth,
    },
}

impl std::fmt::Display for BlurayMaterializationValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LpcmBitDepthRequired {
                pid,
                stream_index,
                kind,
                bit_depth,
            } => write!(
                f,
                "Blu-ray {} LPCM stream {} pid 0x{:04x} cannot be materialized without probed bit depth: {:?}",
                kind.label(),
                u16::from(*stream_index) + 1,
                *pid,
                bit_depth
            ),
        }
    }
}

impl std::error::Error for BlurayMaterializationValidationError {}

/// Gate materializers must call before sending Blu-ray LPCM tracks to the
/// realizer. Metadata enumeration remains permissive, but materialization fails
/// early unless every LPCM track has a probed bit depth.
pub fn validate_bluray_streams_for_materialization(
    streams: &[BlurayAudioStreamInfo],
) -> Result<(), BlurayMaterializationValidationError> {
    for stream in streams {
        if stream.coding == BluRayAudioCoding::Lpcm && !stream.bit_depth.is_probed() {
            return Err(BlurayMaterializationValidationError::LpcmBitDepthRequired {
                pid: stream.pid,
                stream_index: stream.stream_index,
                kind: stream.kind,
                bit_depth: stream.bit_depth.clone(),
            });
        }
    }
    Ok(())
}

/// A continuous PTS range in a title. Backends that expose clip-to-title PTS
/// maps may return these segments; other backends must report the capability as
/// unsupported rather than returning an empty segment list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlurayPtsContinuitySegment {
    pub title_start_pts_90k: u64,
    pub title_end_pts_90k: u64,
    pub clip_ref: u32,
    pub clip_start_pts_90k: u64,
    pub clip_end_pts_90k: u64,
}

/// Result for an optional backend capability.
///
/// `Supported(Vec::new())` means the backend can answer and found no entries.
/// `Unsupported { .. }` means the backend cannot answer that question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>", serialize = "T: Serialize"))]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum BlurayBackendCapability<T> {
    Supported { value: T },
    Unsupported { reason: String },
}

impl<T> BlurayBackendCapability<T> {
    #[must_use]
    pub const fn supported(value: T) -> Self {
        Self::Supported { value }
    }

    #[must_use]
    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self::Unsupported {
            reason: reason.into(),
        }
    }

    #[must_use]
    pub const fn is_supported(&self) -> bool {
        matches!(self, Self::Supported { .. })
    }
}

/// Phase 6 hook for a backend that needs an external stream decryptor.
pub trait BlurayStreamDecryptor {
    fn decrypt_bytes(&mut self, buf: &mut [u8]) -> Result<(), String>;
}

/// Backend adapter boundary. Implementors may use FFI or pure Rust internally,
/// but exposed data must remain tonepoet-owned.
pub trait BlurayBackend {
    type Disc;
    type TitleSource: Read + Seek;

    fn open(path: &Path) -> Result<Self::Disc, String>;

    fn disc_label(disc: &Self::Disc, source: &Path) -> Option<String>;

    fn titles(disc: &Self::Disc) -> Result<Vec<BlurayTitleInfo>, String>;

    fn title_by_playlist(
        disc: &Self::Disc,
        playlist_number: u32,
    ) -> Result<BlurayTitleKey, String>;

    fn chapters(
        disc: &Self::Disc,
        title: BlurayTitleKey,
        angle_number: u8,
    ) -> Result<Vec<BlurayChapterInfo>, String>;

    fn streams(
        disc: &Self::Disc,
        title: BlurayTitleKey,
    ) -> Result<Vec<BlurayAudioStreamInfo>, String> {
        Self::streams_with_probe_policy(disc, title, ProbeDepth::None)
    }

    fn streams_with_probe_policy(
        disc: &Self::Disc,
        title: BlurayTitleKey,
        policy: ProbeDepth,
    ) -> Result<Vec<BlurayAudioStreamInfo>, String>;

    fn max_angle(disc: &Self::Disc, title: BlurayTitleKey) -> Result<u8, String>;

    fn open_title(
        disc: &Self::Disc,
        title: BlurayTitleKey,
        angle_number: u8,
        decryptor: Option<&mut dyn BlurayStreamDecryptor>,
    ) -> Result<Self::TitleSource, String>;

    fn pts_continuity_segments(
        source: &Self::TitleSource,
    ) -> Result<BlurayBackendCapability<Vec<BlurayPtsContinuitySegment>>, String>;
}

/// One-based display number for a zero-based BD audio stream index.
#[must_use]
pub fn bluray_audio_stream_display_number(audio_stream_index: u8) -> u16 {
    u16::from(audio_stream_index) + 1
}

/// Human-friendly fallback label from a source path.
#[must_use]
pub fn bluray_source_stem(source: &Path) -> Option<String> {
    source
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Thin wrapper used by tests and smoke tools when they need a stable source
/// reference alongside backend output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BluraySourceRef {
    pub path: PathBuf,
    pub title: BlurayTitleKey,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_coding_helpers_are_stable() {
        assert_eq!(BluRayAudioCoding::Lpcm.label(), "LPCM");
        assert_eq!(BluRayAudioCoding::Lpcm.codec_rank(), 7);
        assert!(BluRayAudioCoding::TrueHd.is_lossless());
        assert!(BluRayAudioCoding::DtsHdMaster.is_lossless());
        assert!(!BluRayAudioCoding::Ac3.is_lossless());
        assert_eq!(BluRayAudioCoding::DtsHd.elementary_extension(), "dts");
        assert_eq!(BluRayAudioCoding::Lpcm.ffmpeg_format_hint(), Some("pcm_bluray"));
    }

    #[test]
    fn stream_display_number_is_one_based() {
        assert_eq!(bluray_audio_stream_display_number(0), 1);
        assert_eq!(bluray_audio_stream_display_number(7), 8);
    }

    #[test]
    fn title_key_is_constructed_internally_and_exposes_diagnostic_accessors() {
        let key = BlurayTitleKey::from_libbluray(7, 800);

        assert_eq!(key.title_index(), 7);
        assert_eq!(key.playlist_number(), 800);
    }

    #[test]
    fn stream_kind_labels_are_stable() {
        assert_eq!(BluRayAudioStreamKind::Primary.label(), "primary");
        assert_eq!(BluRayAudioStreamKind::Secondary.label(), "secondary");
    }

    #[test]
    fn lpcm_bit_depth_status_reports_probed_depth() {
        let status = BlurayLpcmBitDepth::Probed {
            bit_depth: 24,
            scanned_bytes: 188,
        };

        assert_eq!(status.bit_depth(), Some(24));
        assert!(status.is_probed());
    }

    #[test]
    fn materialization_gate_rejects_unprobed_lpcm() {
        let streams = vec![BlurayAudioStreamInfo {
            kind: BluRayAudioStreamKind::Primary,
            pid: 0x1100,
            stream_index: 0,
            coding: BluRayAudioCoding::Lpcm,
            sample_rate: Some(48_000),
            bit_depth: BlurayLpcmBitDepth::ProbeFailed {
                bytes_scanned: 4096,
                reason: BlurayLpcmBitDepthProbeFailure::ByteLimit {
                    missing_pids: vec![BlurayLpcmPidProbeFailure {
                        pid: 0x1100,
                        reason: BlurayLpcmPidProbeFailureReason::PesStartNotFound,
                    }],
                },
            },
            channels: Some(2),
            channel_layout: Some("stereo".to_string()),
            language: Some("eng".to_string()),
        }];

        let err = validate_bluray_streams_for_materialization(&streams).unwrap_err();
        assert!(err.to_string().contains("cannot be materialized"));
    }

    #[test]
    fn materialization_gate_accepts_probed_lpcm_and_non_lpcm() {
        let streams = vec![
            BlurayAudioStreamInfo {
                kind: BluRayAudioStreamKind::Primary,
                pid: 0x1100,
                stream_index: 0,
                coding: BluRayAudioCoding::Lpcm,
                sample_rate: Some(48_000),
                bit_depth: BlurayLpcmBitDepth::Probed {
                    bit_depth: 24,
                    scanned_bytes: 188,
                },
                channels: Some(2),
                channel_layout: Some("stereo".to_string()),
                language: Some("eng".to_string()),
            },
            BlurayAudioStreamInfo {
                kind: BluRayAudioStreamKind::Primary,
                pid: 0x1101,
                stream_index: 1,
                coding: BluRayAudioCoding::Ac3,
                sample_rate: Some(48_000),
                bit_depth: BlurayLpcmBitDepth::NotApplicable,
                channels: Some(6),
                channel_layout: Some("5.1".to_string()),
                language: Some("eng".to_string()),
            },
        ];

        validate_bluray_streams_for_materialization(&streams).unwrap();
    }

    #[test]
    fn capability_distinguishes_unsupported_from_empty_supported_result() {
        let supported =
            BlurayBackendCapability::supported(Vec::<BlurayPtsContinuitySegment>::new());
        let unsupported = BlurayBackendCapability::<Vec<BlurayPtsContinuitySegment>>::unsupported(
            "not exposed by backend",
        );

        assert!(supported.is_supported());
        assert!(!unsupported.is_supported());
    }
}
