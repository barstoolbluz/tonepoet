//! PR 1 — core data contracts for the conversion pipeline rebuild.
//!
//! Every public type the staged pipeline (PRs 2–10) implements
//! against is defined here. PRs 2+ add implementation structs and
//! function bodies; they do not alter these contracts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::convert::AudioFormat;

// ===========================================================================
// Secret handling
// ===========================================================================

/// A string whose value must never appear in logs, transcripts, or
/// durable reports. `Debug` and `Display` always print a redaction
/// marker. Queue persistence is the only permitted unredacted
/// serialization path (the `Serialize`/`Deserialize` impls below).
#[derive(Clone, Serialize, Deserialize)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    /// Expose the inner value. Callers must not log the result.
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

// ===========================================================================
// Request contract
// ===========================================================================

/// Everything a pipeline run needs, fixed before `materialize`
/// starts. This is the resumable job input — the queue persists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineRequest {
    pub job_id: String,
    pub item_id: String,
    pub container: PathBuf,
    pub source: SourceOptions,
    pub target_format: AudioFormat,
    pub encode: EncodeOptions,
    pub merge: bool,
    pub output_root: PathBuf,
    pub naming: NamingPolicy,
    pub publish: PublishPolicy,
    pub log: LogPolicy,
    pub stages: StagePolicy,
    pub failure_policy: FailurePolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncodeOptions {
    pub backend: EncodeBackend,
    pub bitrate: Option<u32>,
    pub compression_level: Option<u8>,
    pub dither: DitherPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EncodeBackend {
    Auto,
    Ffmpeg,
    Sox,
    BackendCrate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DitherPolicy {
    Auto,
    Off,
    On,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverwritePolicy {
    FailIfExists,
    ReplaceWithBackup,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogPolicy {
    pub root: PathBuf,
    pub write_for_blocked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagePolicy {
    pub metadata: StageRequirement,
    pub replaygain: StageRequirement,
    pub features: StageRequirement,
}

/// Per-stage requirement. The user either enables a stage or
/// doesn't. An enabled stage that fails blocks the album; a
/// disabled stage is skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageRequirement {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailurePolicy {
    /// Archival default — any track failure blocks the album.
    FailAlbumOnAnyTrackFailure,
    /// Explicit opt-in — failed tracks are dropped, album marked partial.
    AllowPartialAlbum,
}

/// A `PipelineRequest` with every secret replaced by a redaction
/// marker. Durable logs and reports serialize this, never the raw
/// request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactedPipelineRequest {
    pub job_id: String,
    pub item_id: String,
    pub container: PathBuf,
    pub source: RedactedSourceOptions,
    pub target_format: AudioFormat,
    pub encode: EncodeOptions,
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
    /// `Some("<redacted>")` when a password was present, else `None`.
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
            target_format: req.target_format,
            encode: req.encode.clone(),
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

// ===========================================================================
// Source and track identity
// ===========================================================================

/// Stable per-track key. Track number alone is not stable: multi-disc
/// albums reuse numbers and filtered selections skip them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TrackId {
    /// 1-based order in the original source.
    pub source_ordinal: u32,
    pub disc_number: Option<u32>,
    /// Tag-visible track number.
    pub track_number: u32,
}

/// How to obtain one track's audio. A materializer produces these;
/// it never cuts/decodes/transcodes — `realize_track` does that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackSourceRef {
    /// A discrete file already on disk (extracted archive member).
    StagedFile(PathBuf),
    /// A sample-range of a single-image audio file — not yet cut.
    ImageSegment {
        image: PathBuf,
        start_sample: u64,
        samples: u64,
    },
    /// A track within a SACD ISO — not yet decoded.
    SacdTrack {
        iso: PathBuf,
        track_index: u32,
        area: SacdArea,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SacdArea {
    Stereo,
    MultiChannel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
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
    /// Escape hatch for format-specific tags — no later PR adds a field.
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
    pub bit_depth: Option<u32>,
}

/// A source container resolved into its ordered tracks + provenance.
/// Re-derivable from `container`; never persisted as resumable state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedSource {
    pub container: PathBuf,
    pub kind: SourceKind,
    pub tracks: Vec<PreparedTrack>,
    pub album_metadata: AlbumMetadata,
    pub provenance: ExtractionProvenance,
}

// ===========================================================================
// Output planning and artifacts
// ===========================================================================

/// Output-planning result — `(TrackId, final_path)` pairs. The
/// manifest never carries final paths; `plan_outputs` builds this.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackArtifact {
    pub track_id: TrackId,
    pub staged_path: PathBuf,
    pub final_path: PathBuf,
    pub samples: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedArtifact {
    pub staged_path: PathBuf,
    pub final_path: PathBuf,
    pub total_samples: u64,
    pub source_tracks: Vec<TrackId>,
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

/// User-facing artifacts published with an album: audio + sidecars.
/// Does not carry the durable run log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactSet {
    pub audio: AudioArtifacts,
    pub sidecars: Vec<SidecarArtifact>,
}

// ===========================================================================
// Publish contract
// ===========================================================================

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedEntry {
    pub final_path: PathBuf,
    pub role: PublishRole,
    pub bytes: u64,
}

// ===========================================================================
// Outcome, stage, and report contracts
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackOutcome {
    Ok,
    /// Always non-empty error text.
    Err(String),
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
    /// Always non-empty error text.
    Failed(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageRecord {
    pub stage: PipelineStage,
    pub outcome: StageOutcome,
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

/// Whole-album outcome. Every variant carries the full stage-record
/// list so the durable log can show which stages ran.
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

/// Full record of one pipeline run, serialized into the durable log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineReport {
    pub request: RedactedPipelineRequest,
    pub source: Option<PreparedSource>,
    pub plan: Option<AlbumPlan>,
    pub artifacts: Option<ArtifactSet>,
    pub published: Option<PublishedAlbum>,
    pub outcome: AlbumOutcome,
    pub durable_log: Option<PathBuf>,
}

// ===========================================================================
// Staging
// ===========================================================================

/// Job-scoped staging directory. `Drop` deletes its tree unless it
/// has been disarmed (publish takes ownership and disarms on a
/// successful final move).
#[derive(Debug)]
pub struct StagingDir {
    pub root: PathBuf,
    pub job_id: String,
    armed: bool,
}

impl StagingDir {
    /// Create a staging dir handle. Does not create the directory —
    /// the runtime owns directory creation.
    pub fn new(root: PathBuf, job_id: String) -> Self {
        Self {
            root,
            job_id,
            armed: true,
        }
    }

    /// Suppress `Drop` cleanup — the tree is now owned by a
    /// successful publish.
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_dir_all(&self.root);
            // Try to remove the staging parent (.tonepoet-staging) if now empty.
            // remove_dir only succeeds on empty directories, so this is safe
            // when other jobs are still using sibling staging dirs.
            if let Some(parent) = self.root.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
}
