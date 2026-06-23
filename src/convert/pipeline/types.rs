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

use crate::disc::bluray_backend::BluRayAudioCoding;

fn deserialize_optional_nonzero_u32<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<u32>::deserialize(deserializer)?;
    Ok(value.filter(|hz| *hz != 0))
}

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


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumBatchSourceContext {
    /// Human-readable source kind to print in the unified album conversion log.
    /// Independent single-file folder batches should use the default
    /// "folder album batch" value so the assembled log does not describe the
    /// album as a representative `SingleFile` track job.
    pub source_kind: String,
    /// Batch-level source context shown as the conversion log container path.
    /// For folder/album dispatch this is normally the source grouping root, not
    /// an arbitrary representative track file.
    pub container_path: PathBuf,
}

impl AlbumBatchSourceContext {
    #[must_use]
    pub fn folder_album_batch(container_path: PathBuf) -> Self {
        Self {
            source_kind: "folder album batch".to_string(),
            container_path,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumBatchContext {
    /// Explicit shared conversion-log batch id assigned once by the folder/album
    /// dispatcher before it enqueues independent single-file track jobs. This id
    /// must be fresh for each album conversion attempt, not deterministically
    /// derived from artist/album/path metadata. Reusing it across attempts would
    /// make stale fragments from a crashed or cancelled run eligible for a later
    /// assembly. This is intentionally separate from `PipelineRequest::job_id`,
    /// which may remain job-scoped for worker staging, run locks, reporting, and
    /// retry identity. Every track job in the same album batch must carry the
    /// same value here, even when each track job has its own distinct `job_id`.
    #[serde(alias = "album_batch_id")]
    pub(crate) conversion_log_batch_id: String,
    /// Total source tracks expected for this album/folder batch. This must be
    /// computed by the folder/album dispatcher from the source group it is about
    /// to enqueue, not from per-file tags such as TOTALTRACKS/TOTALDISCS and not
    /// from an individual single-file job's one prepared source track. This is
    /// the only authoritative count used for conversion-log last-track
    /// detection.
    pub(crate) expected_track_count: usize,
    /// Album output directory associated with this batch. For production
    /// independent-file dispatch this starts as a provisional source-path
    /// estimate, because the real directory is chosen later by `plan_outputs()`
    /// from materialized metadata and naming templates. Fragment identity must
    /// therefore bind to the planner-resolved directory once a track has been
    /// planned. Tests and already-planned callers may mark this value as
    /// planner-resolved.
    pub(crate) album_output_dir: PathBuf,
    /// True only when `album_output_dir` came from the same output-planning
    /// logic that chooses final audio paths. False means the value is a
    /// dispatcher fallback used only until the first planned/published track
    /// supplies the actual album directory.
    #[serde(default)]
    pub(crate) album_output_dir_is_planner_resolved: bool,
    /// Stable root used to group the source files that belong to this album
    /// batch. This prevents fragments from unrelated folders/runs from sharing
    /// a batch id by accident.
    pub(crate) source_grouping_root: PathBuf,
    /// Explicit batch-level source context for the final unified log. This keeps
    /// the album log from inheriting `SingleFile` source semantics and a single
    /// track's path from the representative fragment. Older serialized requests
    /// that omit this field default to `folder album batch` at
    /// `source_grouping_root`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_context: Option<AlbumBatchSourceContext>,
}

impl AlbumBatchContext {
    /// Crate-internal constructor for tests and migration code. Production
    /// independent single-file folder conversions must not construct this
    /// contract directly; they must call
    /// `prepare_independent_single_file_album_batch_for_dispatch(...)`, which
    /// generates a fresh per-attempt `conversion_log_batch_id`, validates the
    /// source group, and attaches per-track ordering context.
    #[must_use]
    pub(crate) fn new(
        conversion_log_batch_id: impl Into<String>,
        expected_track_count: usize,
        album_output_dir: PathBuf,
        source_grouping_root: PathBuf,
    ) -> Self {
        Self {
            conversion_log_batch_id: conversion_log_batch_id.into(),
            expected_track_count,
            album_output_dir,
            album_output_dir_is_planner_resolved: true,
            source_context: Some(AlbumBatchSourceContext::folder_album_batch(
                source_grouping_root.clone(),
            )),
            source_grouping_root,
        }
    }

    /// Crate-internal constructor used by the dispatch helper after it has
    /// generated a fresh per-attempt batch id and validated the source group.
    /// Production callers should not call this directly: use
    /// `prepare_independent_single_file_album_batch_for_dispatch(...)` so
    /// fragment mode cannot be entered with an album-stable or caller-supplied
    /// deterministic id.
    pub(crate) fn from_dispatcher_source_count(
        conversion_log_batch_id: impl Into<String>,
        dispatcher_source_count: usize,
        album_output_dir: PathBuf,
        source_grouping_root: PathBuf,
    ) -> Result<Self, String> {
        if dispatcher_source_count == 0 {
            return Err("album batch dispatcher source count must be greater than zero".to_string());
        }
        Ok(Self::new(
            conversion_log_batch_id,
            dispatcher_source_count,
            album_output_dir,
            source_grouping_root,
        )
        .with_provisional_album_output_dir())
    }

    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn with_source_context(mut self, source_context: AlbumBatchSourceContext) -> Self {
        self.source_context = Some(source_context);
        self
    }

    #[must_use]
    pub(crate) fn with_provisional_album_output_dir(mut self) -> Self {
        self.album_output_dir_is_planner_resolved = false;
        self
    }

    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn with_planner_resolved_album_output_dir(mut self, album_output_dir: PathBuf) -> Self {
        self.album_output_dir = album_output_dir;
        self.album_output_dir_is_planner_resolved = true;
        self
    }

    #[must_use]
    pub(crate) fn album_output_dir_is_planner_resolved(&self) -> bool {
        self.album_output_dir_is_planner_resolved
    }

    #[must_use]
    pub fn source_context(&self) -> AlbumBatchSourceContext {
        self.source_context.clone().unwrap_or_else(|| {
            AlbumBatchSourceContext::folder_album_batch(self.source_grouping_root.clone())
        })
    }
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumBatchTrackContext {
    /// Dispatcher-owned source ordinal for this file within the album batch.
    /// This is available before source materialization and is used only as a
    /// deterministic tie-breaker in the unified conversion-log ordering.
    pub source_ordinal: u32,
    /// Dispatcher-owned disc number when the folder grouping or filename parser
    /// can determine it before materialization. `None` means single-disc or
    /// unknown; it must not split the album batch identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disc_number: Option<u32>,
    /// Dispatcher-owned track number for this file within its disc or album.
    /// This lets corrupt/unsupported files that fail before `PreparedSource`
    /// still contribute a forensic failure fragment in the correct position.
    pub track_number: u32,
}

impl AlbumBatchTrackContext {
    #[must_use]
    pub const fn new(source_ordinal: u32, disc_number: Option<u32>, track_number: u32) -> Self {
        Self {
            source_ordinal,
            disc_number,
            track_number,
        }
    }

    #[must_use]
    pub fn track_id(&self) -> TrackId {
        TrackId {
            source_ordinal: self.source_ordinal,
            disc_number: self.disc_number,
            track_number: self.track_number,
        }
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
    /// Album/folder batch contract for independent single-file jobs. Production
    /// callers must obtain this through
    /// `prepare_independent_single_file_album_batch_for_dispatch(...)`; direct
    /// construction is intentionally crate-private so external callers cannot
    /// supply a generated-looking deterministic id. Its
    /// `conversion_log_batch_id` is the fragment identity; `job_id` remains per
    /// job. Fragment-backed conversion-log assembly requires this field; when
    /// it is absent, single-file jobs use the legacy per-job conversion log path
    /// instead of guessing from tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_batch: Option<AlbumBatchContext>,
    /// Per-request track identity supplied by the same folder/album dispatcher
    /// that creates `album_batch`. This must be available before source
    /// materialization so a corrupt, missing, or unsupported file can still
    /// publish a minimal failed-track conversion-log fragment. Successful
    /// materialized tracks may carry richer tag-derived metadata, but fragment
    /// ordering and pre-materialization failure fragments use this dispatcher
    /// key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_batch_track: Option<AlbumBatchTrackContext>,
    /// Set by the production folder dispatcher only when it has identified an
    /// independent single-file album group but cannot safely use either ordered
    /// fragment assembly or legacy append semantics, for example because the
    /// queued tracks have incompatible conversion settings. Missing or ambiguous
    /// track-number metadata alone is not fatal: those jobs intentionally fall
    /// back to the legacy completion-order incremental conversion.log append
    /// path rather than pretending to have authoritative track order.
    #[serde(default)]
    pub suppress_incremental_conversion_log_append: bool,
    /// Deprecated compatibility field accepted only so older serialized
    /// requests can still be read. The conversion-log fragment path ignores
    /// this field completely; only `album_batch.expected_track_count`, supplied
    /// by the folder/album dispatcher, may define the completion threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_album_track_count: Option<usize>,
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

    #[must_use]
    pub fn with_album_batch(mut self, album_batch: AlbumBatchContext) -> Self {
        self.album_batch = Some(album_batch);
        self
    }

    #[must_use]
    pub fn with_album_batch_track(mut self, album_batch_track: AlbumBatchTrackContext) -> Self {
        self.album_batch_track = Some(album_batch_track);
        self
    }
}


/// Dispatcher output for an independent single-file album/folder batch. The
/// folder dispatcher constructs this once, before enqueueing per-track jobs, so
/// every job receives the same album-level conversion-log contract.
#[derive(Debug, Clone)]
pub struct AlbumBatchDispatch {
    pub album_batch: AlbumBatchContext,
    pub requests: Vec<PipelineRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceOptions {
    pub archive_password: Option<SecretString>,
    pub sacd_area: Option<SacdArea>,
    /// DVD-Audio group-selection policy. This is the active internal contract;
    /// CLI wiring can map future flags onto this enum without changing the
    /// materializer again.
    #[serde(default)]
    pub dvda_group_selection: DvdaGroupSelection,
    /// Backward-compatible legacy field for older serialized requests that
    /// carried only one group number. New code should set `dvda_group_selection`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dvda_group: Option<u8>,
    /// Treat a DVD-Audio source with `DVDAUDIO.MKB` metadata as already decrypted.
    /// This is an explicit caller override for edge cases where the first-AOB
    /// probe cannot classify the payload but the user knows the AOB sectors are readable.
    #[serde(default)]
    pub dvda_assume_decrypted: bool,
    /// DVD-Audio downmix policy requested by the caller. `Auto` preserves native
    /// output except for structurally identified authored stereo presentations.
    #[serde(default)]
    pub dvda_downmix_policy: DvdaDownmixPolicy,
    /// DVD-Video VTS selection. `None` lets the materializer score all supported
    /// VTS/title/stream candidates and choose the likely main program.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dvdv_vts: Option<u8>,
    /// DVD-Video title number within the selected/scored VTS. `None` keeps title
    /// selection in the main-program scorer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dvdv_title: Option<u8>,
    /// DVD-Video audio stream index from IFO attributes. `None` keeps stream
    /// selection in the main-program scorer: chapter count and duration choose
    /// the likely program, then stereo/lossless/codec quality break stream ties.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dvdv_audio_stream: Option<u8>,
    /// DVD-Video camera angle number (1-based). `None` means angle 1, matching
    /// the DVD-Video default angle. Multi-angle/interleaved titles are filtered
    /// through this policy rather than extracting every cell in the angle block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dvdv_angle: Option<u8>,
    /// Blu-ray playlist number (`00000.mpls` as integer) selected by the caller.
    /// `None` lets the materializer use the same scored default presentation as
    /// the disc browser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bluray_playlist: Option<u32>,
    /// Blu-ray primary audio PID selected by the caller. When this and
    /// `bluray_audio_stream` are both set, the materializer validates that they
    /// name the same stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bluray_audio_pid: Option<u16>,
    /// Zero-based Blu-ray audio stream index from playlist metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bluray_audio_stream: Option<u8>,
    /// One-based Blu-ray display angle. `None` means angle 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bluray_angle: Option<u8>,
    pub cue_sidecar: CueSidecarPolicy,
    pub track_selection: TrackSelection,
}

impl SourceOptions {
    #[must_use]
    pub fn effective_dvda_group_selection(&self) -> DvdaGroupSelection {
        match self.dvda_group_selection {
            DvdaGroupSelection::Default => self
                .dvda_group
                .map(DvdaGroupSelection::Group)
                .unwrap_or(DvdaGroupSelection::Default),
            selection => selection,
        }
    }

    #[must_use]
    pub fn explicit_dvda_requested(&self) -> bool {
        !matches!(
            self.effective_dvda_group_selection(),
            DvdaGroupSelection::Default
        ) || self.dvda_assume_decrypted
            || !matches!(self.dvda_downmix_policy, DvdaDownmixPolicy::Auto)
    }

    #[must_use]
    pub fn explicit_dvdv_requested(&self) -> bool {
        self.dvdv_vts.is_some()
            || self.dvdv_title.is_some()
            || self.dvdv_audio_stream.is_some()
            || self.dvdv_angle.is_some()
    }

    #[must_use]
    pub fn explicit_bluray_requested(&self) -> bool {
        self.bluray_playlist.is_some()
            || self.bluray_audio_pid.is_some()
            || self.bluray_audio_stream.is_some()
            || self.bluray_angle.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DvdaGroupSelection {
    /// Preserve Phase 2 default behavior: group 1 when present, otherwise the
    /// first parsed audio group.
    Default,
    /// Materialize one explicit 1-based DVD-Audio group number.
    Group(u8),
    /// Materialize every parsed audio group into one prepared source.
    All,
    /// Pick the best structurally identified two-channel group, falling back to
    /// `Default` when the IFO model cannot prove a stereo group.
    PreferStereo,
    /// Pick the best structurally identified group with more than two channels,
    /// falling back to `Default` when no multichannel group is provable.
    PreferMultichannel,
    /// Pick the structurally proven group with the highest rate/depth profile,
    /// falling back to `Default` when no group exposes comparable facts.
    PreferHighestResolution,
}

impl Default for DvdaGroupSelection {
    fn default() -> Self {
        Self::Default
    }
}

impl DvdaGroupSelection {
    #[must_use]
    pub const fn is_default(self) -> bool {
        matches!(self, Self::Default)
    }

    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Default => "default".to_string(),
            Self::Group(group_nr) => format!("group:{group_nr}"),
            Self::All => "all".to_string(),
            Self::PreferStereo => "prefer_stereo".to_string(),
            Self::PreferMultichannel => "prefer_multichannel".to_string(),
            Self::PreferHighestResolution => "prefer_highest_resolution".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DvdaDownmixPolicy {
    /// Preserve existing behavior except for authored stereo presentations that
    /// the materializer can identify from structure. This is the source-option
    /// default, not a realized track policy.
    Auto,
    /// Extract all channels as-is. Also used as the backward-compatible default
    /// for older serialized `TrackSourceRef::DvdaTrack` values.
    None,
    /// Apply the foo_input_dvda-compatible conservative stereo matrix.
    FooInputDvdaCompatible,
    /// Ask ffmpeg to choose its default stereo rematrixing with `-ac 2`.
    FfmpegDefault,
}

impl Default for DvdaDownmixPolicy {
    fn default() -> Self {
        Self::Auto
    }
}

impl DvdaDownmixPolicy {
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::FooInputDvdaCompatible | Self::FfmpegDefault)
    }

    #[must_use]
    pub const fn output_channel_count(self) -> Option<u32> {
        if self.is_active() {
            Some(2)
        } else {
            None
        }
    }

    #[must_use]
    pub const fn realized_default() -> Self {
        Self::None
    }

    #[must_use]
    pub const fn cache_tag(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::None => 1,
            Self::FooInputDvdaCompatible => 2,
            Self::FfmpegDefault => 3,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
            Self::FooInputDvdaCompatible => "foo_input_dvda_compatible",
            Self::FfmpegDefault => "ffmpeg_default",
        }
    }

    #[must_use]
    pub const fn behavior(self) -> &'static str {
        match self {
            Self::Auto => "resolve per track during DVD-Audio materialization",
            Self::None => "extract native channel count without downmix DSP",
            Self::FooInputDvdaCompatible => {
                "apply foo_input_dvda-compatible conservative stereo downmix during realization"
            }
            Self::FfmpegDefault => {
                "ask ffmpeg to apply its default stereo rematrixing during realization"
            }
        }
    }
}

fn default_realized_dvda_downmix_policy() -> DvdaDownmixPolicy {
    DvdaDownmixPolicy::realized_default()
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
    pub album_batch: Option<AlbumBatchContext>,
    pub album_batch_track: Option<AlbumBatchTrackContext>,
    pub suppress_incremental_conversion_log_append: bool,
    pub expected_album_track_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactedSourceOptions {
    pub archive_password: Option<String>,
    pub sacd_area: Option<SacdArea>,
    #[serde(default)]
    pub dvda_group_selection: DvdaGroupSelection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dvda_group: Option<u8>,
    /// Treat a DVD-Audio source with `DVDAUDIO.MKB` metadata as already decrypted.
    /// This is an explicit caller override for edge cases where the first-AOB
    /// probe cannot classify the payload but the user knows the AOB sectors are readable.
    #[serde(default)]
    pub dvda_assume_decrypted: bool,
    #[serde(default)]
    pub dvda_downmix_policy: DvdaDownmixPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dvdv_vts: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dvdv_title: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dvdv_audio_stream: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dvdv_angle: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bluray_playlist: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bluray_audio_pid: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bluray_audio_stream: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bluray_angle: Option<u8>,
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
                dvda_group_selection: req.source.effective_dvda_group_selection(),
                dvda_group: req.source.dvda_group,
                dvda_assume_decrypted: req.source.dvda_assume_decrypted,
                dvda_downmix_policy: req.source.dvda_downmix_policy,
                dvdv_vts: req.source.dvdv_vts,
                dvdv_title: req.source.dvdv_title,
                dvdv_audio_stream: req.source.dvdv_audio_stream,
                dvdv_angle: req.source.dvdv_angle,
                bluray_playlist: req.source.bluray_playlist,
                bluray_audio_pid: req.source.bluray_audio_pid,
                bluray_audio_stream: req.source.bluray_audio_stream,
                bluray_angle: req.source.bluray_angle,
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
            album_batch: req.album_batch.clone(),
            album_batch_track: req.album_batch_track.clone(),
            suppress_incremental_conversion_log_append: req.suppress_incremental_conversion_log_append,
            expected_album_track_count: req.expected_album_track_count,
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
    DvdaTrack {
        /// DVD-Audio volume backing this track. Phase 3 must open tracks through
        /// this typed source rather than guessing whether sectors refer to a
        /// directory tree, an ISO, or a staged AUDIO_TS copy.
        volume_source: DvdaVolumeSourceRef,
        group_nr: u8,
        /// ATS number when this track came from an ATSI title/chapter. SAMG-only
        /// tracks may not have a proven ATS mapping at Phase 2 materialization
        /// time, so these fields are optional by design.
        title_set_nr: Option<u8>,
        /// Raw ATS PGC/title identifier from ATSI, when known.
        title_nr: Option<u8>,
        /// 1-based ATS title ordinal used by AMG/AOTT group references, when known.
        title_ordinal: Option<u8>,
        /// 1-based playback ordinal within the selected DVD-Audio group. This is
        /// the only field that should be used for group-level sequencing and SAMG
        /// correlation; ATS chapter numbers restart per title and must not be
        /// treated as group track numbers.
        group_track_ordinal: u32,
        /// 1-based ATS-local chapter/track number when this track came from ATSI.
        ats_track_nr: Option<u8>,
        /// 1-based SAMG group track number when this track came from or was
        /// correlated with AUDIO_PP.IFO.
        samg_track_nr: Option<u8>,
        /// SAMG flat track-list ordinal when the source reference came from or was
        /// correlated with AUDIO_PP.IFO.
        samg_ordinal: Option<u16>,
        /// Names whether `sector_ranges` are relative to an ATS AOB address space
        /// or absolute SAMG sector addresses. Phase 3 must dispatch on this
        /// rather than assuming all ranges can be read from an ATS inventory.
        sector_address_space: DvdaSectorAddressSpace,
        /// Materializer-proven elementary stream kind for AOB-less cross-ATS tracks.
        /// This is group-scoped evidence stamped onto every track in the group so
        /// realization workers do not have to rediscover an MLP/LPCM hint from each
        /// track's own starting sector. Normal ATS-relative tracks leave this empty
        /// and keep strict packet validation behavior unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        elementary_stream_kind_hint: Option<DvdaElementaryStreamKind>,
        /// First PTS reported by ATSI/SAMG for this track. Phase 3 readers use
        /// this for packet-boundary validation, trimming, and diagnostics without
        /// parsing string metadata.
        #[serde(default)]
        first_pts: u32,
        /// Track duration in 90 kHz PTS ticks as reported by ATSI/SAMG.
        #[serde(default)]
        len_in_pts: u32,
        /// Raw ATS track-type byte when the track came from ATSI. SAMG-only tracks
        /// do not carry this field in AUDIO_PP.IFO, so the value is optional.
        track_type: Option<u8>,
        /// Starting ATS index number when the track came from ATSI. SAMG-only
        /// tracks do not carry ATS index numbers.
        index_start: Option<u8>,
        /// ATS downmix matrix selector for this track, when present.
        downmix_matrix: Option<u8>,
        /// Realized downmix policy selected for this track. Missing values from
        /// older manifests preserve the native all-channel behavior.
        #[serde(default = "default_realized_dvda_downmix_policy")]
        dvda_downmix_policy: DvdaDownmixPolicy,
        /// ATS PGC/title table byte offset captured for Phase 3 diagnostics and
        /// cross-checking, when the source reference came from ATSI.
        title_table_offset: Option<u32>,
        /// ATS title duration in 90 kHz PTS ticks, when known.
        title_len_in_pts: Option<u32>,
        /// Declared ATS track count for the containing title, when known.
        title_track_count_declared: Option<u8>,
        /// Declared ATS index count for the containing title, when known.
        title_index_count_declared: Option<u8>,
        /// Active ATS audio-format table entry when Phase 2 can determine it
        /// from structure alone. This is intentionally optional because real
        /// discs do not reliably encode the format in `track_type`, and SAMG-only
        /// records do not identify an ATS format table entry.
        audio_format_index: Option<u8>,
        /// IFO-derived scalar sample rate used to validate decoded DVD-Audio WAV output.
        /// This remains optional for multi-format ATS records where Phase 2 cannot
        /// identify a single active audio-format table entry from structure alone.
        #[serde(
            default,
            deserialize_with = "deserialize_optional_nonzero_u32",
            skip_serializing_if = "Option::is_none"
        )]
        expected_sample_rate: Option<u32>,
        /// IFO-derived total channel count used to validate decoded DVD-Audio WAV output.
        /// This remains optional when the IFO record does not expose a channel assignment.
        #[serde(
            default,
            deserialize_with = "deserialize_optional_nonzero_u32",
            skip_serializing_if = "Option::is_none"
        )]
        expected_channel_count: Option<u32>,
        /// IFO-derived source bit depth. DVD-Audio MLP is decoded to `pcm_s32le`
        /// for the pipeline carrier, so this records the source-depth assertion
        /// rather than the WAV container sample format.
        #[serde(
            default,
            deserialize_with = "deserialize_optional_nonzero_u32",
            skip_serializing_if = "Option::is_none"
        )]
        expected_bit_depth: Option<u32>,
        /// IFO channel-assignment code from the active ATS/SAMG audio-format record.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_channel_assignment_code: Option<u8>,
        /// IFO group-format details from the active ATS/SAMG audio-format record.
        #[serde(
            default,
            deserialize_with = "deserialize_optional_nonzero_u32",
            skip_serializing_if = "Option::is_none"
        )]
        expected_group1_sample_rate: Option<u32>,
        #[serde(
            default,
            deserialize_with = "deserialize_optional_nonzero_u32",
            skip_serializing_if = "Option::is_none"
        )]
        expected_group2_sample_rate: Option<u32>,
        #[serde(
            default,
            deserialize_with = "deserialize_optional_nonzero_u32",
            skip_serializing_if = "Option::is_none"
        )]
        expected_group1_bit_depth: Option<u32>,
        #[serde(
            default,
            deserialize_with = "deserialize_optional_nonzero_u32",
            skip_serializing_if = "Option::is_none"
        )]
        expected_group2_bit_depth: Option<u32>,
        #[serde(
            default,
            deserialize_with = "deserialize_optional_nonzero_u32",
            skip_serializing_if = "Option::is_none"
        )]
        expected_group1_channel_count: Option<u32>,
        #[serde(
            default,
            deserialize_with = "deserialize_optional_nonzero_u32",
            skip_serializing_if = "Option::is_none"
        )]
        expected_group2_channel_count: Option<u32>,
        sector_ranges: Vec<DvdaSectorRangeRef>,
        aob_files: Vec<DvdaAobFileRef>,
    },
    DvdVideoTrack {
        /// User-supplied DVD-Video source path. This can be an ISO image, a
        /// mounted/copied DVD root containing `VIDEO_TS/`, or the `VIDEO_TS`
        /// directory itself.
        source: PathBuf,
        /// VTS number (1-based).
        vts_number: u8,
        /// Title number within the VTS (1-based).
        title_number: u8,
        /// Selected camera angle number (1-based). Angle 1 is the DVD-Video
        /// default; multi-angle cell blocks are filtered to this path.
        angle_number: u8,
        /// Chapter number (1-based).
        chapter_number: u16,
        /// DVD-Video audio stream index (0-7) from the VTS IFO stream table.
        audio_stream_index: u8,
        /// IFO coding mode for the selected audio stream.
        audio_coding: DvdVideoAudioCoding,
        /// Cell sector ranges for this chapter, relative to the concatenated
        /// title-VOB address space (VTS_xx_1.VOB, VTS_xx_2.VOB, ...).
        /// Ranges are inclusive and validated by the materializer.
        cell_sectors: Vec<(u32, u32)>,
        /// VOB file inventory for the selected VTS title VOBs. Cell sectors
        /// must be resolved through this map, never by adding a VTSI_MAT
        /// sector pointer to the relative cell sector.
        vob_files: Vec<DvdVideoVobFileRef>,
        /// IFO-derived sample rate in Hz when available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sample_rate: Option<u32>,
        /// LPCM bit depth when available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bit_depth: Option<u32>,
        /// Channel count when available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channels: Option<u8>,
    },
    BluRayTrack {
        /// User-supplied Blu-ray source path. This can be an ISO image, a disc
        /// root containing `BDMV/`, or the `BDMV` directory itself.
        source: PathBuf,
        /// Five-digit MPLS playlist number as an integer.
        playlist_number: u32,
        /// Zero-based backend title index kept for efficient Phase 3 title open.
        title_index: usize,
        /// Selected one-based display angle.
        angle_number: u8,
        /// Blu-ray chapter number (1-based).
        chapter_number: u32,
        /// Chapter start in BD-ROM 90 kHz PTS units.
        chapter_start_pts_90k: u64,
        /// Chapter end in BD-ROM 90 kHz PTS units, from the next chapter start
        /// or from the selected title duration for the final chapter.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chapter_end_pts_90k: Option<u64>,
        /// Primary audio PID selected from playlist/clip metadata.
        audio_pid: u16,
        /// Zero-based Blu-ray audio stream index.
        audio_stream_index: u8,
        /// BD-ROM audio coding for the selected stream.
        audio_coding: BluRayAudioCoding,
        /// Stream sample rate in Hz when available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sample_rate: Option<u32>,
        /// Probed LPCM bit depth. Compressed streams carry `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bit_depth: Option<u32>,
        /// Stream channel count when available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channels: Option<u8>,
        /// Backend-reported or derived channel layout label.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channel_layout: Option<String>,
    },
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DvdVideoVobFileRef {
    /// VTS number owning this title VOB file.
    pub vts_number: u8,
    /// 1-based VOB part number from `VTS_xx_N.VOB`.
    pub vob_index: u8,
    pub file_name: String,
    /// Filesystem path for directory-backed DVD-Video sources. ISO-backed
    /// sources leave this as `None` and use `lba` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Absolute ISO logical block address of this VOB file extent. Ignored for
    /// directory-backed sources where `path` is set.
    pub lba: u32,
    pub byte_len: u64,
    /// First sector contributed by this file, relative to the concatenated
    /// title-VOB address space for the VTS.
    pub block_first: u32,
    /// Inclusive last sector contributed by this file, relative to the
    /// concatenated title-VOB address space for the VTS.
    pub block_last: u32,
}

impl DvdVideoVobFileRef {
    #[must_use]
    pub const fn contains(&self, block: u32) -> bool {
        block >= self.block_first && block <= self.block_last
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DvdVideoAudioCoding {
    Lpcm,
    Ac3,
    Dts,
    Mpeg,
}

impl DvdVideoAudioCoding {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lpcm => "LPCM",
            Self::Ac3 => "AC-3",
            Self::Dts => "DTS",
            Self::Mpeg => "MPEG",
        }
    }

    #[must_use]
    pub const fn is_lossless(self) -> bool {
        matches!(self, Self::Lpcm)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DvdaVolumeSourceRef {
    /// User supplied a DVD-Audio directory or an AUDIO_TS directory.
    Directory { root: PathBuf },
    /// User supplied an ISO image. The backend records the filesystem path that
    /// proved DVD-Audio identity during detection/materialization. Phase 3 should
    /// reopen the image through this backend rather than probing again.
    Iso {
        path: PathBuf,
        backend: DvdaIsoBackend,
    },
    /// AUDIO_TS was copied into the staging area from another container. The
    /// current Phase 2 ISO path does not use this, but the variant keeps future
    /// extraction-based fallbacks explicit.
    StagedAudioTs { original: PathBuf, root: PathBuf },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DvdaIsoBackend {
    /// DVD-Audio identity and materialization use the UDF filesystem.
    Udf,
    /// DVD-Audio identity and materialization use the ISO9660 bridge filesystem.
    Iso9660Bridge,
    /// DVD-Audio identity came only from an explicit raw AMG scan. This is a
    /// diagnostic state; Phase 2 materialization requires a filesystem backend.
    ExplicitRawMagicOnly,
}

impl DvdaVolumeSourceRef {
    #[must_use]
    pub fn root_or_image(&self) -> &PathBuf {
        match self {
            DvdaVolumeSourceRef::Directory { root } => root,
            DvdaVolumeSourceRef::Iso { path, .. } => path,
            DvdaVolumeSourceRef::StagedAudioTs { root, .. } => root,
        }
    }

    #[must_use]
    pub fn original_container(&self) -> &PathBuf {
        match self {
            DvdaVolumeSourceRef::Directory { root } => root,
            DvdaVolumeSourceRef::Iso { path, .. } => path,
            DvdaVolumeSourceRef::StagedAudioTs { original, .. } => original,
        }
    }

    #[must_use]
    pub fn staged_audio_ts_root(&self) -> Option<&PathBuf> {
        match self {
            DvdaVolumeSourceRef::StagedAudioTs { root, .. } => Some(root),
            DvdaVolumeSourceRef::Directory { .. } | DvdaVolumeSourceRef::Iso { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DvdaElementaryStreamKind {
    Mlp,
    Lpcm,
    /// LPCM carried in DVD-Video VOB Private Stream 1 packets. Unlike DVD-Audio
    /// LPCM, byte 3 is part of the first-access-unit pointer, not an
    /// extra-header length.
    DvdVideoLpcm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DvdaSectorAddressSpace {
    /// Sector numbers are relative to the beginning of the selected ATS AOB
    /// logical address space and should be resolved through `aob_files`.
    AtsAobRelative { title_set_nr: u8 },
    /// Sector numbers are absolute logical block addresses on the original disc.
    /// This is used for AOB-less ATS presentations whose PGC sector ranges are
    /// resolved from verified SAMG VOB evidence when available, with legacy
    /// AMG/AOTT + ATSI metadata retained only as a fallback.
    DiscAbsolute { title_set_nr: u8 },
    /// Sector numbers came directly from SAMG absolute-sector fields. Realization
    /// resolves these through raw ISO sector reads because copied directory trees
    /// do not preserve disc logical sector addresses.
    SamgAbsolute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DvdaSectorRangeRef {
    pub index_nr: u8,
    /// First sector in the address space named by `DvdaTrack::sector_address_space`.
    pub first: u32,
    /// Inclusive last sector in the address space named by
    /// `DvdaTrack::sector_address_space`.
    pub last: u32,
}

impl DvdaSectorRangeRef {
    #[must_use]
    pub const fn block_count(&self) -> u32 {
        if self.last < self.first {
            0
        } else {
            self.last - self.first + 1
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DvdaAobFileRef {
    pub title_set_nr: u8,
    pub part_nr: u8,
    pub file_name: String,
    pub exists: bool,
    pub byte_len: u64,
    /// First sector contributed by this AOB part, relative to the ATS AOB
    /// sector address space.
    pub block_first: u32,
    /// Inclusive last sector contributed by this AOB part, relative to the ATS
    /// AOB sector address space.
    pub block_last: u32,
}

impl DvdaAobFileRef {
    #[must_use]
    pub const fn contains(&self, block: u32) -> bool {
        self.exists && block >= self.block_first && block <= self.block_last
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockedSource {
    pub source: PreparedSource,
    pub reason: SourceBlockReason,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceBlockReason {
    DvdaCppm(DvdaCopyProtectionBlock),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DvdaCopyProtectionScheme {
    Cppm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DvdaCopyProtectionEvidenceSource {
    DvdaudioMkb,
    AobMpegPsProbe,
    ParserDiagnostic,
    UserOverride,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DvdaCopyProtectionHandlingPolicy {
    /// Detect CPPM evidence, explain the block, and skip realization. This
    /// build intentionally does not include CPPM decryption or key-management.
    DetectExplainSkip,
}

impl Default for DvdaCopyProtectionHandlingPolicy {
    fn default() -> Self {
        Self::DetectExplainSkip
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DvdaCopyProtectionBlock {
    pub scheme: DvdaCopyProtectionScheme,
    pub evidence_source: DvdaCopyProtectionEvidenceSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_filename: Option<String>,
    pub mkb_present: bool,
    pub cppm_detected: bool,
    #[serde(default)]
    pub handling_policy: DvdaCopyProtectionHandlingPolicy,
    pub decryption_supported: bool,
    pub skip_reason: String,
    pub user_explanation: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<String>,
}

impl DvdaCopyProtectionBlock {
    #[must_use]
    pub fn log_label(&self) -> String {
        let filename = self
            .evidence_filename
            .as_deref()
            .unwrap_or("unknown source file");
        format!(
            "CPPM detected from {filename} (MKB present: {}, parser CPPM flag: {}, policy: {:?}, decryption supported: {})",
            self.mkb_present,
            self.cppm_detected,
            self.handling_policy,
            self.decryption_supported
        )
    }
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
    DvdAudio,
    DvdVideo,
    BluRay,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceAudioCoding {
    Pcm,
    Dsd,
    DvdaUnknown,
    Unknown,
}

impl Default for SourceAudioCoding {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelGroupDescriptor {
    pub group_nr: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_nonzero_u32"
    )]
    pub sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceAudioDescriptor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coding: Option<SourceAudioCoding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channel_groups: Vec<ChannelGroupDescriptor>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_nonzero_u32"
    )]
    pub primary_sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u32>,
}

impl Default for SourceAudioDescriptor {
    fn default() -> Self {
        Self {
            coding: None,
            channel_groups: Vec::new(),
            primary_sample_rate: None,
            bit_depth: None,
        }
    }
}

impl SourceAudioDescriptor {
    #[must_use]
    pub fn from_scalar(
        sample_rate: Option<u32>,
        bit_depth: Option<u32>,
        coding: Option<SourceAudioCoding>,
    ) -> Self {
        Self {
            coding,
            channel_groups: Vec::new(),
            primary_sample_rate: sample_rate.filter(|hz| *hz != 0),
            bit_depth,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedTrack {
    pub id: TrackId,
    pub source_ref: TrackSourceRef,
    pub metadata: TrackMetadata,
    pub expected_samples: Option<u64>,
    /// Primary scalar sample rate when the source can be represented by one
    /// authoritative rate at materialization time. DVD-Audio tracks with
    /// multiple possible IFO audio formats or split channel-group rates leave
    /// this as `None` until Phase 3 packet inspection resolves the stream.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_optional_nonzero_u32"
    )]
    pub sample_rate: Option<u32>,
    /// Typed source-domain audio facts. This prevents decode-boundary code from
    /// having to recover rates, depths, coding, or DVD-A channel-group details
    /// from string metadata.
    #[serde(default)]
    pub source_audio: SourceAudioDescriptor,
    /// Probed bit depth of the original source image/file when available. For
    /// CUE image tracks this remains the original image depth; it is not the
    /// `pcm_s32le` depth of the staged segment WAV carrier. For DVD-Audio this
    /// mirrors `source_audio.bit_depth` only when one scalar bit depth is known.
    pub bit_depth: Option<u32>,
}

impl PreparedTrack {
    /// Returns the scalar sample rate only when the materializer has one
    /// authoritative rate for the whole prepared track. DVD-Audio tracks with
    /// multiple IFO formats or split channel-group rates intentionally return
    /// `None` until Phase 3 reads packet sub-headers.
    #[must_use]
    pub fn scalar_sample_rate(&self) -> Option<u32> {
        self.sample_rate
            .filter(|hz| *hz != 0)
            .or_else(|| self.source_audio.primary_sample_rate.filter(|hz| *hz != 0))
    }

    /// Returns true when conversion logic may safely compare this track to a
    /// scalar target rate. Callers that need DVD-A channel-group facts should
    /// inspect `source_audio.channel_groups` instead.
    #[must_use]
    pub fn has_scalar_sample_rate(&self) -> bool {
        self.scalar_sample_rate().is_some()
    }

    /// Returns the scalar rate or a caller-owned message explaining why the
    /// rate is unavailable. This gives non-DVD-A callers an explicit migration
    /// path without reintroducing numeric sentinels.
    pub fn require_scalar_sample_rate(
        &self,
        context: &'static str,
    ) -> Result<u32, MissingSourceAudioFact> {
        self.scalar_sample_rate().ok_or(MissingSourceAudioFact {
            context,
            track_id: self.id.clone(),
            fact: "scalar sample rate",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingSourceAudioFact {
    pub context: &'static str,
    pub track_id: TrackId,
    pub fact: &'static str,
}

impl std::fmt::Display for MissingSourceAudioFact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let disc = self
            .track_id
            .disc_number
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown-disc".to_string());

        write!(
            f,
            "{} requires {} for track {}-{}-{}",
            self.context, self.fact, self.track_id.source_ordinal, disc, self.track_id.track_number
        )
    }
}

impl std::error::Error for MissingSourceAudioFact {}

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
            source_audio_md5_written: self.source_audio_md5_written
                || other.source_audio_md5_written,
            authoritative_tags_applied: self.authoritative_tags_applied
                || other.authoritative_tags_applied,
        }
    }

    #[must_use]
    pub const fn any(self) -> bool {
        self.source_tags_transferred
            || self.artwork_transferred
            || self.source_audio_md5_written
            || self.authoritative_tags_applied
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

        assert_eq!(
            json["authoritative_tags_applied"],
            serde_json::Value::Bool(true)
        );
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

fn default_publish_track_count() -> usize {
    0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishPlan {
    pub album_dir: PathBuf,
    pub entries: Vec<PublishEntry>,
    /// Number of source tracks represented by this publish plan's audio payload
    /// for this one job. This stays at 1 for successful independent single-file
    /// track jobs even when those jobs participate in a larger album/folder
    /// batch. It is allowed to be 0 for terminal failure publishes that carry a
    /// forensic conversion-log fragment but no audio artifact.
    ///
    /// Do not use this field for conversion-log fragment completion. Use
    /// `expected_album_track_count` for the album-level last-track threshold.
    #[serde(default = "default_publish_track_count")]
    pub source_audio_track_count: usize,
    /// Total source tracks expected in the album/folder conversion batch. For
    /// ordinary one-job publishes this usually equals `source_audio_track_count`;
    /// for fragment-backed independent single-file jobs it is carried by the
    /// fragment sidecar and may be greater than the current job's payload count.
    #[serde(default = "default_publish_track_count")]
    pub expected_album_track_count: usize,
    /// True when the production dispatcher identified an independent multi-file
    /// album group but could not prove canonical track identity. Such plans must
    /// not use the legacy incremental conversion.log append path, because that
    /// path writes in worker-completion order.
    #[serde(default)]
    pub suppress_incremental_conversion_log_append: bool,
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
    /// The source structure was materialized, but this track must not enter
    /// decode/encode stages. This is used for known blocked sources such as
    /// CPPM-protected DVD-Audio discs so reports and logs can retain per-track
    /// structure without retrying doomed realization paths.
    Blocked(String),
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
        self.dst_passthrough_frames = self
            .dst_passthrough_frames
            .saturating_add(other.dst_passthrough_frames);
        self.dst_decoded_frames = self
            .dst_decoded_frames
            .saturating_add(other.dst_decoded_frames);
        self.dst_reencoded_frames = self
            .dst_reencoded_frames
            .saturating_add(other.dst_reencoded_frames);
        self.dst_raw_fallback_frames = self
            .dst_raw_fallback_frames
            .saturating_add(other.dst_raw_fallback_frames);
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
    EncryptedSource,
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

#[cfg(test)]
mod prepared_track_sample_rate_contract {
    use super::*;
    fn track_json(sample_rate: &str, source_audio: &str) -> String {
        format!(
            r#"{{
                "id": {{ "source_ordinal": 1, "disc_number": 1, "track_number": 1 }},
                "source_ref": {{ "StagedFile": "track.wav" }},
                "metadata": {{
                    "title": null, "artist": null, "album_artist": null,
                    "composer": null, "performer": null, "genre": null, "date": null,
                    "track_number": null, "disc_number": null, "isrc": null,
                    "publisher": null, "copyright": null, "comment": null,
                    "pre_emphasis": false, "extra": {{}}
                }},
                "expected_samples": null,
                "sample_rate": {sample_rate},
                "source_audio": {source_audio},
                "bit_depth": null
            }}"#
        )
    }

    #[test]
    fn scalar_sample_rate_deserializes_legacy_numeric_value() {
        let track: PreparedTrack =
            serde_json::from_str(&track_json("44100", r#"{"primary_sample_rate":44100}"#))
                .expect("legacy scalar sample_rate should deserialize");
        assert_eq!(track.scalar_sample_rate(), Some(44_100));
    }

    #[test]
    fn scalar_sample_rate_deserializes_missing_as_unknown() {
        let json = track_json("null", "{}");
        let track: PreparedTrack =
            serde_json::from_str(&json).expect("unknown scalar sample_rate should deserialize");
        assert_eq!(track.scalar_sample_rate(), None);
        assert!(!track.has_scalar_sample_rate());
    }

    #[test]
    fn scalar_sample_rate_treats_zero_as_unknown_for_backward_compatibility() {
        let track: PreparedTrack =
            serde_json::from_str(&track_json("0", r#"{"primary_sample_rate":0}"#))
                .expect("historic zero sentinel should deserialize");
        assert_eq!(track.sample_rate, None);
        assert_eq!(track.source_audio.primary_sample_rate, None);
        assert_eq!(track.scalar_sample_rate(), None);
    }

    #[test]
    fn scalar_sample_rate_can_fall_back_to_source_audio_descriptor() {
        let track: PreparedTrack =
            serde_json::from_str(&track_json("null", r#"{"primary_sample_rate":96000}"#))
                .expect("source_audio primary rate should deserialize");
        assert_eq!(track.sample_rate, None);
        assert_eq!(track.scalar_sample_rate(), Some(96_000));
    }
}
