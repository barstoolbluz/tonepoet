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
