//! Core data contracts for the unified conversion pipeline.
//!
//! Chunk 2 deletes the former encode-wrapper path. A
//! `PipelineRequest` now carries the complete `tonepoet_pipeline::PipelineSettings`
//! object and each realized track becomes a `tonepoet_pipeline::PlanRequest`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tonepoet_pipeline::PipelineSettings;

#[derive(Clone, Serialize, Deserialize)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl std::fmt::Display for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineRequest {
    pub job_id: String,
    pub item_id: String,
    pub container: PathBuf,
    pub source: SourceOptions,
    pub settings: PipelineSettings,
    /// Worker pool size for this job. None means cores-1.
    pub worker_count: Option<usize>,
    pub merge: bool,
    pub output_root: PathBuf,
    pub naming: NamingPolicy,
    pub publish: PublishPolicy,
    pub log: LogPolicy,
    pub stages: StagePolicy,
    pub failure_policy: FailurePolicy,
    /// Container extension override. `None` = codec default extension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_extension: Option<String>,
    /// Extra ffmpeg output flags for the selected container.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub container_ffmpeg_flags: Vec<String>,
}

impl PipelineRequest {
    pub fn target_format(&self) -> tonepoet_pipeline::AudioFormat {
        self.settings.target_format.clone()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceOptions {
    pub archive_password: Option<SecretString>,
    pub sacd_area: Option<SacdArea>,
    pub cue_sidecar: CueSidecarPolicy,
    pub track_selection: TrackSelection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CueSidecarPolicy {
    PreferEmbedded,
    PreferSidecar,
    SidecarOnly,
    EmbeddedOnly,
    IgnoreCue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackSelection {
    All,
    Range { start: u32, end: u32 },
    Set(BTreeSet<u32>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamingPolicy {
    pub template: String,
    pub folder_template: Option<String>,
    pub per_album_subdir: bool,
    pub collision_policy: NamingCollisionPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NamingCollisionPolicy {
    Fail,
    AppendStableSuffix,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishPolicy {
    pub overwrite: OverwritePolicy,
    pub same_filesystem_required: bool,
    /// Write `.tonepoet-manifest.json` to the output directory. Used by the
    /// rerun gate to detect identical conversions. Default: false.
    #[serde(default)]
    pub write_manifest: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverwritePolicy {
    FailIfExists,
    ReplaceWithBackup,
    SkipIfManifestMatch,
    VerifyIfManifestMatch,
    AlwaysRedo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogPolicy {
    pub root: PathBuf,
    pub write_for_blocked: bool,
    #[serde(default)]
    pub write_json_log: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagePolicy {
    pub metadata: StageRequirement,
    pub replaygain: StageRequirement,
    pub features: StageRequirement,
    #[serde(default)]
    pub generate_cue: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageRequirement {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailurePolicy {
    FailAlbumOnAnyTrackFailure,
    AllowPartialAlbum,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactedPipelineRequest {
    pub job_id: String,
    pub item_id: String,
    pub container: PathBuf,
    pub source: RedactedSourceOptions,
    pub settings: PipelineSettings,
    /// Worker pool size for this job. None means cores-1.
    pub worker_count: Option<usize>,
    pub merge: bool,
    pub output_root: PathBuf,
    pub naming: NamingPolicy,
    pub publish: PublishPolicy,
    pub log: LogPolicy,
    pub stages: StagePolicy,
    pub failure_policy: FailurePolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactedSourceOptions {
    pub archive_password: Option<String>,
    pub sacd_area: Option<SacdArea>,
    pub cue_sidecar: CueSidecarPolicy,
    pub track_selection: TrackSelection,
}

impl From<&PipelineRequest> for RedactedPipelineRequest {
    fn from(req: &PipelineRequest) -> Self {
        Self {
            job_id: req.job_id.clone(),
            item_id: req.item_id.clone(),
            container: req.container.clone(),
            source: RedactedSourceOptions {
                archive_password: req
                    .source
                    .archive_password
                    .as_ref()
                    .map(|_| "<redacted>".to_string()),
                sacd_area: req.source.sacd_area,
                cue_sidecar: req.source.cue_sidecar,
                track_selection: req.source.track_selection.clone(),
            },
            settings: req.settings.clone(),
            worker_count: req.worker_count,
            merge: req.merge,
            output_root: req.output_root.clone(),
            naming: req.naming.clone(),
            publish: req.publish.clone(),
            log: req.log.clone(),
            stages: req.stages.clone(),
            failure_policy: req.failure_policy,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TrackId {
    pub source_ordinal: u32,
    pub disc_number: Option<u32>,
    pub track_number: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackSourceRef {
    StagedFile(PathBuf),
    CueSegmentCarrier {
        /// Validated, sample-bounded CUE segment carrier produced by the
        /// materializer. This is an audio-only `pcm_s32le` WAV file and must
        /// not be treated as the original source image for tag, artwork, MD5,
        /// or source-bit-depth policy.
        path: PathBuf,
        /// Original image file from which this CUE segment was decoded. Kept as
        /// provenance only; CUE cuts must not stream-copy compressed packets
        /// from this image.
        source_image: PathBuf,
        start_sample: u64,
        samples: u64,
        carrier: CueSegmentCarrier,
    },
    ImageSegment {
        image: PathBuf,
        start_sample: u64,
        samples: u64,
    },
    SacdTrack {
        iso: PathBuf,
        track_index: u32,
        area: SacdArea,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CueSegmentCarrier {
    PcmS32LeWav,
}

impl CueSegmentCarrier {
    #[must_use]
    pub const fn bit_depth(self) -> u32 {
        match self {
            Self::PcmS32LeWav => 32,
        }
    }

    #[must_use]
    pub const fn codec_name(self) -> &'static str {
        match self {
            Self::PcmS32LeWav => "pcm_s32le",
        }
    }

    #[must_use]
    pub const fn container_name(self) -> &'static str {
        match self {
            Self::PcmS32LeWav => "wav",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SacdArea {
    Stereo,
    MultiChannel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    SingleFile,
    SevenZip,
    CueImage,
    SacdIso,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrackMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album_artist: Option<String>,
    pub composer: Option<String>,
    pub performer: Option<String>,
    pub genre: Option<String>,
    pub date: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub isrc: Option<String>,
    pub publisher: Option<String>,
    pub copyright: Option<String>,
    pub comment: Option<String>,
    pub pre_emphasis: bool,
    pub extra: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AlbumMetadata {
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub genre: Option<String>,
    pub date: Option<String>,
    pub total_tracks: u32,
    pub total_discs: Option<u32>,
    pub disc_number: Option<u32>,
    pub extra: BTreeMap<String, String>,
}

/// AlbumMetadata.extra key used by the CUE materializer to hand original
/// image artwork to the post-encode metadata/artwork stage. The staged CUE
/// WAV carrier is audio-only; this sidecar preserves the artwork source fact
/// without pretending the WAV contains it.
pub const CUE_ARTWORK_PATH_EXTRA_KEY: &str = "tonepoet_cue_artwork_path";
pub const CUE_ARTWORK_MIME_EXTRA_KEY: &str = "tonepoet_cue_artwork_mime";
pub const CUE_ARTWORK_SOURCE_EXTRA_KEY: &str = "tonepoet_cue_artwork_source";
pub const CUE_ARTWORK_UNSUPPORTED_EXTRA_KEY: &str = "tonepoet_cue_artwork_unsupported";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionProvenance {
    pub source_kind: SourceKind,
    pub source_sha256: Option<String>,
    pub tool_versions: BTreeMap<String, String>,
    pub extracted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedTrack {
    pub id: TrackId,
    pub source_ref: TrackSourceRef,
    pub metadata: TrackMetadata,
    pub expected_samples: Option<u64>,
    pub sample_rate: u32,
    /// Probed bit depth of the original source image/file when available. For
    /// CUE image tracks this remains the original image depth; it is not the
    /// `pcm_s32le` depth of the staged segment WAV carrier.
    pub bit_depth: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedSource {
    pub container: PathBuf,
    pub kind: SourceKind,
    pub tracks: Vec<PreparedTrack>,
    pub album_metadata: AlbumMetadata,
    pub provenance: ExtractionProvenance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumPlan {
    pub album_dir: PathBuf,
    pub entries: Vec<PlannedTrackOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedTrackOutput {
    pub track_id: TrackId,
    pub final_path: PathBuf,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedMetadataSatisfaction {
    /// Original source-container text tags were transferred by the per-track planner.
    #[serde(default)]
    pub source_tags_transferred: bool,
    /// Original source artwork/video metadata was transferred by the per-track planner.
    #[serde(default)]
    pub artwork_transferred: bool,
    /// Source-audio MD5 metadata was written by the per-track planner.
    #[serde(default)]
    pub source_audio_md5_written: bool,
    /// Authoritative Tonepoet/materializer album and track tags have already
    /// been applied by an explicit owner. Current per-track planner commands do
    /// not set this; the orchestrator normally owns `apply_metadata()`.
    ///
    /// Backward-compatible alias: earlier dimensional metadata-state snapshots
    /// used `authoritative_tags_written`. Preserve that spelling on deserialize
    /// so persisted manifests/logs do not silently lose this bit.
    #[serde(default, alias = "authoritative_tags_written")]
    pub authoritative_tags_applied: bool,
}

impl PlannedMetadataSatisfaction {
    #[must_use]
    pub const fn none() -> Self {
        Self {
            source_tags_transferred: false,
            artwork_transferred: false,
            source_audio_md5_written: false,
            authoritative_tags_applied: false,
        }
    }

    #[must_use]
    pub const fn satisfies(self, required: Self) -> bool {
        (!required.source_tags_transferred || self.source_tags_transferred)
            && (!required.artwork_transferred || self.artwork_transferred)
            && (!required.source_audio_md5_written || self.source_audio_md5_written)
            && (!required.authoritative_tags_applied || self.authoritative_tags_applied)
    }

    #[must_use]
    pub const fn merge(self, other: Self) -> Self {
        Self {
            source_tags_transferred: self.source_tags_transferred || other.source_tags_transferred,
            artwork_transferred: self.artwork_transferred || other.artwork_transferred,
            source_audio_md5_written: self.source_audio_md5_written || other.source_audio_md5_written,
            authoritative_tags_applied: self.authoritative_tags_applied || other.authoritative_tags_applied,
        }
    }

    #[must_use]
    pub const fn any(self) -> bool {
        self.source_tags_transferred || self.artwork_transferred || self.source_audio_md5_written || self.authoritative_tags_applied
    }
}

#[cfg(test)]
mod metadata_satisfaction_serde_tests {
    use super::PlannedMetadataSatisfaction;

    #[test]
    fn accepts_legacy_authoritative_tags_written_field() {
        let value: PlannedMetadataSatisfaction = serde_json::from_str(
            r#"{
                "source_tags_transferred": false,
                "artwork_transferred": false,
                "source_audio_md5_written": false,
                "authoritative_tags_written": true
            }"#,
        )
        .expect("legacy metadata satisfaction JSON should deserialize");

        assert!(value.authoritative_tags_applied);
    }

    #[test]
    fn serializes_canonical_authoritative_tags_applied_field() {
        let value = PlannedMetadataSatisfaction {
            authoritative_tags_applied: true,
            ..PlannedMetadataSatisfaction::none()
        };

        let json = serde_json::to_value(value).expect("serialize metadata satisfaction");

        assert_eq!(json["authoritative_tags_applied"], serde_json::Value::Bool(true));
        assert!(json.get("authoritative_tags_written").is_none());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackArtifact {
    pub track_id: TrackId,
    pub staged_path: PathBuf,
    pub final_path: PathBuf,
    pub samples: Option<u64>,
    /// Dimension-by-dimension record of the metadata obligations satisfied by
    /// the planner-owned per-track plan. The album post-processing gate must
    /// compare this against the original request; it must not collapse distinct
    /// obligations into a single boolean.
    #[serde(default)]
    pub metadata_satisfaction: PlannedMetadataSatisfaction,
    /// Dimension-by-dimension metadata obligations that were meaningful for
    /// this realized track after source facts were parsed. In particular,
    /// `source_audio_md5_written` is required only when the realized source
    /// actually exposed a parsed `SourceInfo::audio_md5`, not merely because a
    /// path ended in `.flac`.
    #[serde(default)]
    pub metadata_required: PlannedMetadataSatisfaction,
    /// SHA-256 of the planned command sequence, computed during encoding.
    /// Used by the manifest for rerun identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_command_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedArtifact {
    pub staged_path: PathBuf,
    pub final_path: PathBuf,
    pub total_samples: u64,
    pub source_tracks: Vec<TrackId>,
    /// SHA-256 of the planned merge command sequence. Single-track merge
    /// shortcuts reuse the wrapped track artifact hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_command_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AudioArtifacts {
    Tracks(Vec<TrackArtifact>),
    Merged(MergedArtifact),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SidecarKind {
    ConversionLog,
    CueSheet,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarArtifact {
    pub kind: SidecarKind,
    pub staged_path: PathBuf,
    pub final_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactSet {
    pub audio: AudioArtifacts,
    pub sidecars: Vec<SidecarArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishPlan {
    pub album_dir: PathBuf,
    pub entries: Vec<PublishEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishEntry {
    pub staged_path: PathBuf,
    pub final_path: PathBuf,
    pub role: PublishRole,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublishRole {
    Audio,
    Sidecar(SidecarKind),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedAlbum {
    pub album_dir: PathBuf,
    pub entries: Vec<PublishedEntry>,
    /// Path to the manifest file written during publish, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedEntry {
    pub final_path: PathBuf,
    pub role: PublishRole,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackOutcome {
    Ok,
    Err(String),
}


/// DSD/DST operation counters carried through reports and durable logs.
///
/// The app fills this from sacd-rs extraction reports when available and from
/// source-independent DSF/DSDIFF validation when a track enters or leaves the
/// planner as a standalone DSD file. Zero fields are meaningful: for example, a
/// DSF -> FLAC conversion can have frames read but no DSD frames emitted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DsdDstPipelineStats {
    #[serde(default)]
    pub frames_read: u64,
    #[serde(default)]
    pub frames_decoded: u64,
    #[serde(default)]
    pub frames_emitted: u64,
    #[serde(default)]
    pub crc_checked: u64,
    #[serde(default)]
    pub crc_passed: u64,
    #[serde(default)]
    pub crc_failed: u64,
    #[serde(default)]
    pub crc_missing: u64,
    #[serde(default)]
    pub dst_passthrough_frames: u64,
    #[serde(default)]
    pub dst_decoded_frames: u64,
    #[serde(default)]
    pub dst_reencoded_frames: u64,
    #[serde(default)]
    pub dst_raw_fallback_frames: u64,
    #[serde(default)]
    pub bytes_read: u64,
    #[serde(default)]
    pub bytes_written: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_error_frame: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_error_offset: Option<u64>,
}

impl DsdDstPipelineStats {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.frames_read == 0
            && self.frames_decoded == 0
            && self.frames_emitted == 0
            && self.crc_checked == 0
            && self.crc_passed == 0
            && self.crc_failed == 0
            && self.crc_missing == 0
            && self.dst_passthrough_frames == 0
            && self.dst_decoded_frames == 0
            && self.dst_reencoded_frames == 0
            && self.dst_raw_fallback_frames == 0
            && self.bytes_read == 0
            && self.bytes_written == 0
            && self.first_error_frame.is_none()
            && self.first_error_offset.is_none()
    }

    pub fn merge(&mut self, other: &Self) {
        self.frames_read = self.frames_read.saturating_add(other.frames_read);
        self.frames_decoded = self.frames_decoded.saturating_add(other.frames_decoded);
        self.frames_emitted = self.frames_emitted.saturating_add(other.frames_emitted);
        self.crc_checked = self.crc_checked.saturating_add(other.crc_checked);
        self.crc_passed = self.crc_passed.saturating_add(other.crc_passed);
        self.crc_failed = self.crc_failed.saturating_add(other.crc_failed);
        self.crc_missing = self.crc_missing.saturating_add(other.crc_missing);
        self.dst_passthrough_frames = self.dst_passthrough_frames.saturating_add(other.dst_passthrough_frames);
        self.dst_decoded_frames = self.dst_decoded_frames.saturating_add(other.dst_decoded_frames);
        self.dst_reencoded_frames = self.dst_reencoded_frames.saturating_add(other.dst_reencoded_frames);
        self.dst_raw_fallback_frames = self.dst_raw_fallback_frames.saturating_add(other.dst_raw_fallback_frames);
        self.bytes_read = self.bytes_read.saturating_add(other.bytes_read);
        self.bytes_written = self.bytes_written.saturating_add(other.bytes_written);
        if self.first_error_frame.is_none() {
            self.first_error_frame = other.first_error_frame;
        }
        if self.first_error_offset.is_none() {
            self.first_error_offset = other.first_error_offset;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackRecord {
    pub track_id: TrackId,
    pub outcome: TrackOutcome,
    pub source_ref: TrackSourceRef,
    pub realized_input: Option<PathBuf>,
    pub output_file: Option<PathBuf>,
    pub commands: Vec<crate::convert::pipeline::tool::CommandRecord>,
    pub bytes_in: Option<u64>,
    pub bytes_out: Option<u64>,
    pub duration: Option<Duration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsd_dst_stats: Option<DsdDstPipelineStats>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PipelineStage {
    Materialize,
    PlanOutputs,
    Convert,
    Merge,
    Metadata,
    ReplayGain,
    Features,
    Publish,
    DurableLog,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageOutcome {
    Ok,
    Skipped,
    Failed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageRecord {
    pub stage: PipelineStage,
    pub outcome: StageOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsd_dst_stats: Option<DsdDstPipelineStats>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockReason {
    TrackFailures,
    RequiredStageFailure(PipelineStage),
    MaterializeFailed,
    PlanFailed,
    PublishFailed,
    DurableLogFailed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlbumOutcome {
    Complete {
        tracks: Vec<TrackRecord>,
        stages: Vec<StageRecord>,
    },
    Partial {
        successful: Vec<TrackRecord>,
        failed: Vec<TrackRecord>,
        stages: Vec<StageRecord>,
    },
    Blocked {
        successful: Vec<TrackRecord>,
        failed: Vec<TrackRecord>,
        stages: Vec<StageRecord>,
        reason: BlockReason,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineReport {
    pub request: RedactedPipelineRequest,
    pub source: Option<PreparedSource>,
    pub plan: Option<AlbumPlan>,
    pub artifacts: Option<ArtifactSet>,
    pub published: Option<PublishedAlbum>,
    pub outcome: AlbumOutcome,
    pub durable_log: Option<PathBuf>,
    /// Settings fingerprint for conversion identity tracking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings_fingerprint: Option<tonepoet_pipeline::SettingsFingerprint>,
    /// Path to the conversion manifest, if one was written during publish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<PathBuf>,
}

#[derive(Debug)]
pub struct StagingDir {
    pub root: PathBuf,
    pub job_id: String,
    armed: bool,
}

impl StagingDir {
    pub fn new(root: PathBuf, job_id: String) -> Self {
        Self {
            root,
            job_id,
            armed: true,
        }
    }

    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// Construct a non-owning staging handle for worker tasks that need the
    /// staged root path but must not delete it when the task-local handle drops.
    pub fn borrowed(root: PathBuf, job_id: String) -> Self {
        Self {
            root,
            job_id,
            armed: false,
        }
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_dir_all(&self.root);
            if let Some(parent) = self.root.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
}

#[cfg(test)]
mod chunk_2_1_3_staging_cleanup_tests {
    use super::*;

    #[test]
    fn armed_staging_dir_drop_deletes_partial_materialization_tree() {
        let temp = tempfile::tempdir().expect("temp dir");
        let staging_parent = temp.path().join(".tonepoet-staging");
        let staging_root = staging_parent.join("job-item");
        let partial = staging_root.join("materialized").join("partial.flac");
        std::fs::create_dir_all(partial.parent().unwrap()).expect("partial parent");
        std::fs::write(&partial, b"partial").expect("partial file");

        {
            let _staging = StagingDir::new(staging_root.clone(), "job".to_string());
        }

        assert!(!staging_root.exists());
        assert!(!staging_parent.exists());
    }

    #[test]
    fn borrowed_staging_handle_does_not_delete_owner_tree() {
        let temp = tempfile::tempdir().expect("temp dir");
        let staging_root = temp.path().join("staging");
        let file = staging_root.join("converted").join("01.flac");
        std::fs::create_dir_all(file.parent().unwrap()).expect("converted parent");
        std::fs::write(&file, b"audio").expect("audio file");

        {
            let _borrowed = StagingDir::borrowed(staging_root.clone(), "job".to_string());
        }

        assert!(file.exists());
    }

    #[test]
    fn disarmed_staging_survives_successful_publish_boundary() {
        let temp = tempfile::tempdir().expect("temp dir");
        let staging_root = temp.path().join("staging");
        std::fs::create_dir_all(&staging_root).expect("staging root");
        let mut staging = StagingDir::new(staging_root.clone(), "job".to_string());
        staging.disarm();
        drop(staging);

        assert!(staging_root.exists());
    }
}
