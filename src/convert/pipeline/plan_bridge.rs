//! Per-track bridge into `tonepoet-pipeline`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sacd_rs::{
    inspect_dsd_container, DsdCompression, DsdContainerError, DsdContainerFormat,
    DsdContainerInfo,
};
use tonepoet_pipeline::{
    AudioCodec as PlannerCodec, AudioFormat as PlannerFormat, BitDepthTarget,
    PcmBitDepth, PipelineSettings, PlanRequest, PreferredTool, SampleKind, SourceInfo,
};

use super::errors::ConvertError;
use super::types::{
    AlbumMetadata, PlannedMetadataSatisfaction, PipelineRequest, PreparedSource, PreparedTrack,
    CueSegmentCarrier, SourceKind, StageRequirement, TrackMetadata, TrackSourceRef, CUE_ARTWORK_PATH_EXTRA_KEY,
};

pub fn plan_request_for_track(
    request: &PipelineRequest,
    track: &PreparedTrack,
    realized_input: &Path,
    staged_output: &Path,
    intermediate_dir: PathBuf,
) -> Result<PlanRequest, ConvertError> {
    let source = source_info_for_realized_track(track, realized_input)?;
    source
        .validate()
        .map_err(|err| ConvertError::Backend(format!("invalid source facts for planner: {err}")))?;

    if let Some(message) =
        source_audio_md5_policy_downgrade_message(request, track, realized_input, &source)
    {
        log::warn!("{message}");
    }

    let mut settings = request.settings.clone();
    // Blu-ray compressed-codec realization decodes through FFmpeg into a PCM WAV
    // carrier. In Auto mode, keep the encode leg on FFmpeg as well: FFmpeg
    // preserves decoded WAV precision without requiring the SoX-only `-b` flag
    // that previously defaulted unresolved compressed-codec depth to 16-bit.
    // Explicit user tool preferences remain explicit; the materializer still
    // provides a concrete decoded bit depth so a requested SoX path receives a
    // 24-bit target instead of the legacy 16-bit fallback.
    if matches!(&track.source_ref, TrackSourceRef::BluRayTrack { .. })
        && matches!(settings.preferred_tool, PreferredTool::Auto)
    {
        settings.preferred_tool = PreferredTool::Ffmpeg;
    }

    // CUE materialization now produces an audio-only, validated PCM S32LE WAV
    // carrier. It is a sample-bounded decoded segment, not the original image
    // container, so the planner must not claim source tag/artwork transfer or
    // source-audio MD5 from this carrier. Authoritative CUE tags remain owned by
    // the post-encode metadata stage. Force an encode step so a target WAV does
    // not passthrough-copy the S32 carrier when the requested target depth is
    // `Source` and the original image depth happened to match the carrier.
    if cue_pcm_segment_carrier_bit_depth(track).is_some() {
        settings.force_encode = true;
        if settings.metadata.transfer_tags {
            log::warn!(
                "metadata.transfer_tags requested for a CUE PCM segment carrier; source-container tag transfer from the original image is unsupported on this path and will be skipped"
            );
            disable_planner_source_tag_transfer(&mut settings);
        }
        if settings.metadata.preserve_artwork {
            log::warn!(
                "metadata.preserve_artwork requested for a CUE PCM segment carrier; planner artwork transfer from the audio-only carrier is disabled; original image artwork, when extracted by the CUE materializer, is handled by the post-encode metadata/artwork stage"
            );
            disable_planner_artwork_transfer(&mut settings);
        }
        disable_planner_source_audio_md5(&mut settings);
    }
    for message in apply_unsupported_target_metadata_policy_downgrades(&mut settings) {
        log::warn!("{message}");
    }
    // Metadata stays in the per-track planner request for ordinary sources. The
    // planner can preserve source-container tags/artwork and store source MD5, but
    // it cannot write authoritative materializer metadata from PreparedSource/
    // PreparedTrack. SACD ISO materialization is a special case: its realized DSF
    // files are generated audio carriers, not the metadata authority. The current
    // SACD materializer extracts audio plus sidecar/TOC text metadata only; it has
    // no source-container tag/artwork extraction path and no FLAC STREAMINFO audio
    // MD5 on the materialized DSF/DFF carrier. Therefore source tag/artwork copy
    // and source-audio MD5 storage from the materialized DSD carrier must be
    // disabled here, and those original SACD policies are treated as unsupported
    // rather than satisfied in metadata_obligations_for_request().
    if matches!(
        &track.source_ref,
        TrackSourceRef::SacdTrack { .. } | TrackSourceRef::DvdVideoTrack { .. }
    ) {
        disable_planner_source_tag_transfer(&mut settings);
        disable_planner_artwork_transfer(&mut settings);
    }
    // Source-audio MD5 can only be planned when the realized input actually
    // exposed a value. Today this bridge obtains that value from FLAC
    // STREAMINFO, so DSF/DFF/WAV/AIFF/MP3 and other non-FLAC realized inputs
    // must not carry a still-enabled store_source_audio_md5 request into the
    // planner, where validation would otherwise fail before the orchestrator
    // can write authoritative metadata. This is capability-based rather than
    // source-kind-based: standalone FLAC can keep the policy; standalone DSD
    // cannot.
    if source.audio_md5.is_none() {
        disable_planner_source_audio_md5(&mut settings);
    }
    // ReplayGain remains orchestrator-owned because album mode requires all
    // completed tracks.
    settings.replay_gain.mode = None;
    if matches!(settings.target_bit_depth, BitDepthTarget::Source) {
        let resolved_source_depth = track
            .bit_depth
            .and_then(pcm_bit_depth_from_u32)
            .or(source.bit_depth);
        if let Some(depth) = resolved_source_depth {
            settings.target_bit_depth = BitDepthTarget::Pcm(depth);
        }
    }
    settings
        .validate()
        .map_err(|err| ConvertError::Backend(format!("invalid pipeline settings: {err}")))?;

    // settings-sentinel-allow: settings originates from request.settings.clone() (line 26)
    Ok(PlanRequest {
        input_path: realized_input.to_path_buf(),
        output_path: staged_output.to_path_buf(),
        source,
        settings,
        intermediate_dir: Some(intermediate_dir),
        container_ffmpeg_flags: request.container_ffmpeg_flags.clone(),
    })
}

/// Disable planner-owned source tag transfer through one named policy gate.
///
/// The bridge is allowed to downgrade impossible per-track metadata policies
/// when the realized carrier or target format cannot support them, but raw field
/// writes are deliberately centralized here so future planner-disposition changes
/// cannot silently fork the policy across call sites.
fn disable_planner_source_tag_transfer(settings: &mut PipelineSettings) {
    let metadata = &mut settings.metadata;
    metadata.transfer_tags = false;
}

/// Disable planner-owned embedded artwork transfer through the same named policy
/// gate used for source tag transfer.
fn disable_planner_artwork_transfer(settings: &mut PipelineSettings) {
    let metadata = &mut settings.metadata;
    metadata.preserve_artwork = false;
}

/// Disable planner-owned source-audio MD5 storage when source facts prove that
/// no authoritative MD5 is available for the realized track.
fn disable_planner_source_audio_md5(settings: &mut PipelineSettings) {
    let metadata = &mut settings.metadata;
    metadata.store_source_audio_md5 = false;
}

/// Return a user-visible warning when a requested source-audio MD5 policy must
/// be disabled for a concrete realized track.
///
/// `metadata.store_source_audio_md5` is only implementable when the realized
/// input exposes an actual nonzero FLAC STREAMINFO MD5 in `SourceInfo`. When the
/// user requested that policy but the source lacks the required fact, the bridge
/// disables the per-track planner flag to avoid a late planner validation error.
/// This helper keeps that downgrade explicit and testable rather than silent.
#[must_use]
pub fn source_audio_md5_policy_downgrade_message(
    request: &PipelineRequest,
    track: &PreparedTrack,
    realized_input: &Path,
    source: &SourceInfo,
) -> Option<String> {
    if !request.settings.metadata.store_source_audio_md5 || source.audio_md5.is_some() {
        return None;
    }

    Some(format!(
        "metadata.store_source_audio_md5 requested for track {} ({}) but the realized source {} does not expose a nonzero FLAC STREAMINFO audio MD5; SOURCE_AUDIO_MD5 metadata is unsupported for this track and will be skipped",
        track.id.source_ordinal,
        track_label_for_policy_message(track),
        realized_input.display()
    ))
}

fn track_label_for_policy_message(track: &PreparedTrack) -> String {
    track
        .metadata
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or("untitled")
        .to_string()
}

/// Disable requested source metadata policies that the selected target format
/// cannot represent through the planner/plugin metadata path, returning
/// user-visible warnings for each downgrade.
///
/// This keeps per-track planner validation aligned with the obligation model:
/// unsupported target metadata policies are not treated as required work, and
/// they are not left enabled for the planner to reject later.
#[must_use]
pub fn apply_unsupported_target_metadata_policy_downgrades(
    settings: &mut PipelineSettings,
) -> Vec<String> {
    let mut messages = Vec::new();

    if settings.metadata.transfer_tags
        && !settings.target_format.supports_planner_source_tag_transfer()
    {
        messages.push(format!(
            "metadata.transfer_tags requested for target format {:?}, but the planner/plugin metadata path cannot represent source tags for that target; source tag transfer is unsupported for this track and will be skipped",
            settings.target_format
        ));
        disable_planner_source_tag_transfer(settings);
    }

    if settings.metadata.preserve_artwork
        && !settings.target_format.supports_planner_embedded_artwork_transfer()
    {
        messages.push(format!(
            "metadata.preserve_artwork requested for target format {:?}, but the planner/plugin metadata path cannot preserve embedded artwork for that target; artwork preservation is unsupported for this track and will be skipped",
            settings.target_format
        ));
        disable_planner_artwork_transfer(settings);
    }

    messages
}

/// Return whether the planner request asks for any planner-owned metadata effect.
#[must_use]
pub fn settings_request_metadata(settings: &PipelineSettings) -> bool {
    settings.metadata.transfer_tags
        || settings.metadata.preserve_artwork
        || settings.metadata.store_source_audio_md5
}

/// Return the metadata obligations that can be decided from the album request
/// and source/materializer facts plus planner-owned target-format capabilities.
///
/// Source-audio MD5 is intentionally **not** computed here: the planner can
/// write `SOURCE_AUDIO_MD5` only when the realized per-track `SourceInfo`
/// contains an actual parsed FLAC STREAMINFO MD5. A path extension is not a
/// capability proof. Use `planner_metadata_obligations_for_track()` after
/// `plan_request_for_track()` has parsed the realized input.
#[must_use]
pub fn metadata_obligations_for_request(
    req: &PipelineRequest,
    source: &PreparedSource,
) -> PlannedMetadataSatisfaction {
    PlannedMetadataSatisfaction {
        source_tags_transferred: req.settings.metadata.transfer_tags
            && source_supports_source_tag_transfer(req, source),
        artwork_transferred: req.settings.metadata.preserve_artwork
            && source_supports_source_artwork_preservation(req, source),
        source_audio_md5_written: false,
        authoritative_tags_applied: matches!(req.stages.metadata, StageRequirement::Enabled)
            && source_needs_authoritative_metadata(source),
    }
}

/// Return planner-owned metadata obligations for a concrete realized track.
///
/// This function uses the already-built `PlanRequest`, so source-MD5 support
/// comes from the same parsed `SourceInfo::audio_md5` fact that the planner will
/// validate and use. This avoids mismatches where an extension such as `.flac`
/// is treated as proof that a usable STREAMINFO MD5 exists.
#[must_use]
pub fn planner_metadata_obligations_for_track(
    req: &PipelineRequest,
    plan_request: &PlanRequest,
) -> PlannedMetadataSatisfaction {
    PlannedMetadataSatisfaction {
        source_tags_transferred: req.settings.metadata.transfer_tags
            && plan_request.settings.metadata.transfer_tags
            && plan_request.settings.target_format.supports_planner_source_tag_transfer(),
        artwork_transferred: req.settings.metadata.preserve_artwork
            && plan_request.settings.metadata.preserve_artwork
            && plan_request.settings.target_format.supports_planner_embedded_artwork_transfer(),
        source_audio_md5_written: req.settings.metadata.store_source_audio_md5
            && plan_request.settings.metadata.store_source_audio_md5
            && plan_request.source.audio_md5.is_some(),
        authoritative_tags_applied: false,
    }
}

/// Return whether the original source is a meaningful source-container tag
/// authority for the per-track planner and the selected target has planner/plugin
/// support for source tag transfer.
///
/// SACD ISO tracks are realized as generated DSF/DFF audio carriers; the useful
/// text metadata comes from the SACD TOC or sidecar XML and is handled by the
/// orchestrator metadata stage instead. Other source kinds still require a
/// target format whose planner-owned metadata-transfer capability can carry text tags. This keeps
/// the obligation model aligned with the planner's typed command effects rather
/// than assuming every non-SACD target can receive source tags.
#[must_use]
pub fn source_supports_source_tag_transfer(
    req: &PipelineRequest,
    source: &PreparedSource,
) -> bool {
    !matches!(source.kind, SourceKind::SacdIso | SourceKind::CueImage | SourceKind::DvdVideo)
        && req.settings.target_format.supports_planner_source_tag_transfer()
}

/// Return whether the current source/materializer plus target-format path can
/// preserve source artwork. Ordinary file sources rely on planner-owned source
/// artwork transfer. CUE image sources are different: they use an audio-only
/// staged WAV carrier plus a materializer-extracted artwork sidecar, so their
/// artwork obligation is owned by the post-encode metadata/artwork stage.
///
/// Concrete command-level satisfaction is carried either by typed planner
/// metadata effects or by the orchestrator metadata/artwork stage. This function
/// only defines which original artwork requests are meaningful obligations for
/// the skip gate.
#[must_use]
pub fn source_supports_source_artwork_preservation(
    req: &PipelineRequest,
    source: &PreparedSource,
) -> bool {
    match source.kind {
        SourceKind::CueImage => cue_source_has_extracted_artwork(source)
            && req
                .settings
                .target_format
                .supports_cue_post_encode_artwork_embedding(),
        SourceKind::SacdIso | SourceKind::DvdVideo => false,
        _ => req.settings.target_format.supports_planner_embedded_artwork_transfer(),
    }
}

fn cue_source_has_extracted_artwork(source: &PreparedSource) -> bool {
    source
        .album_metadata
        .extra
        .get(CUE_ARTWORK_PATH_EXTRA_KEY)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

/// Decide whether the legacy album metadata stage is still necessary after the
/// planner ran. The orchestrator may skip its metadata pass only when every
/// originally requested dimension has been satisfied; satisfying one metadata
/// sub-policy must never stand in for another.
#[must_use]
pub fn orchestrator_metadata_stage_required(
    satisfaction: PlannedMetadataSatisfaction,
    stage: StageRequirement,
    required: PlannedMetadataSatisfaction,
) -> bool {
    matches!(stage, StageRequirement::Enabled)
        && required.any()
        && !satisfaction.satisfies(required)
}

#[must_use]
pub fn source_needs_authoritative_metadata(source: &PreparedSource) -> bool {
    matches!(source.kind, SourceKind::CueImage | SourceKind::SacdIso | SourceKind::DvdVideo)
        && prepared_source_has_metadata(source)
}

fn prepared_source_has_metadata(source: &PreparedSource) -> bool {
    album_metadata_has_tags(&source.album_metadata)
        || source.tracks.iter().any(|track| track_metadata_has_tags(&track.metadata))
}

fn has_non_empty_text(value: &Option<String>) -> bool {
    value.as_deref().map_or(false, |value| !value.trim().is_empty())
}

fn album_metadata_has_tags(album: &AlbumMetadata) -> bool {
    has_non_empty_text(&album.album)
        || has_non_empty_text(&album.album_artist)
        || has_non_empty_text(&album.genre)
        || has_non_empty_text(&album.date)
        || album.total_tracks > 0
        || album.total_discs.is_some()
        || album.disc_number.is_some()
        || !album.extra.is_empty()
}

fn track_metadata_has_tags(track: &TrackMetadata) -> bool {
    has_non_empty_text(&track.title)
        || has_non_empty_text(&track.artist)
        || has_non_empty_text(&track.album_artist)
        || has_non_empty_text(&track.composer)
        || has_non_empty_text(&track.performer)
        || has_non_empty_text(&track.genre)
        || has_non_empty_text(&track.date)
        || track.track_number.is_some()
        || track.disc_number.is_some()
        || has_non_empty_text(&track.isrc)
        || has_non_empty_text(&track.publisher)
        || has_non_empty_text(&track.copyright)
        || has_non_empty_text(&track.comment)
        || track.pre_emphasis
        || !track.extra.is_empty()
}


/// Standalone DSD source kind derived from DSF/DSDIFF container inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdPlannerSourceKind {
    Dsf,
    DsdiffDsd,
    DsdiffDst,
}

/// Header-validation state for planner-facing DSD source metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DsdPlannerValidationStatus {
    Clean,
    Warnings { count: usize },
    Errors { count: usize },
}

/// Planner-facing metadata for standalone DSF/DFF sources.
///
/// `tonepoet-pipeline::SourceInfo` carries the sample-rate, channel-count,
/// duration, format, and codec facts that affect command planning. This companion
/// record preserves the DSD-specific source kind and DST-compression fact that
/// are not representable in the current planner struct, so tests and callers can
/// prove `.dff` was classified by `CMPR`/container inspection rather than by
/// extension alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DsdPlannerSourceMetadata {
    pub source_kind: DsdPlannerSourceKind,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub sample_count_per_channel: Option<u64>,
    pub dst_compressed: bool,
    pub validation: DsdPlannerValidationStatus,
}

impl DsdPlannerSourceMetadata {
    fn from_container(info: &DsdContainerInfo) -> Self {
        let source_kind = match (info.format, info.compression) {
            (DsdContainerFormat::Dsf, _) => DsdPlannerSourceKind::Dsf,
            (DsdContainerFormat::Dsdiff, DsdCompression::Dst) => DsdPlannerSourceKind::DsdiffDst,
            (DsdContainerFormat::Dsdiff, _) => DsdPlannerSourceKind::DsdiffDsd,
        };
        let diagnostic_count = info.diagnostics.len();
        let validation = if diagnostic_count == 0 {
            DsdPlannerValidationStatus::Clean
        } else if info.has_errors() {
            DsdPlannerValidationStatus::Errors { count: diagnostic_count }
        } else {
            DsdPlannerValidationStatus::Warnings { count: diagnostic_count }
        };
        Self {
            source_kind,
            sample_rate_hz: info.sample_rate,
            channels: info.channel_count,
            sample_count_per_channel: info.sample_count_per_channel,
            dst_compressed: info.compression == DsdCompression::Dst,
            validation,
        }
    }

    fn duration(&self) -> Option<Duration> {
        let samples = self.sample_count_per_channel?;
        if self.sample_rate_hz == 0 {
            return None;
        }
        let nanos = (u128::from(samples) * 1_000_000_000_u128) / u128::from(self.sample_rate_hz);
        if nanos > u128::from(u64::MAX) * 1_000_000_000_u128 + 999_999_999 {
            return None;
        }
        Some(Duration::new(
            (nanos / 1_000_000_000_u128) as u64,
            (nanos % 1_000_000_000_u128) as u32,
        ))
    }
}

/// Inspect standalone DSF/DFF input and return planner-facing DSD metadata.
///
/// DFF classification must come from the DSDIFF `CMPR`/container structure, not
/// from the `.dff` extension: DSDIFF/DSD and DSDIFF/DST share that extension but
/// require different provenance and validation reporting.
pub fn dsd_source_metadata_from_path(
    path: &Path,
) -> Result<Option<DsdPlannerSourceMetadata>, ConvertError> {
    let Some(format) = planner_format_from_path(path) else {
        return Ok(None);
    };
    if !format.is_dsd() {
        return Ok(None);
    }

    let mut file = File::open(path).map_err(|err| {
        ConvertError::Backend(format!(
            "failed to open standalone DSD source {} for inspection: {err}",
            path.display()
        ))
    })?;
    match inspect_dsd_container(&mut file) {
        Ok(info) => Ok(Some(DsdPlannerSourceMetadata::from_container(&info))),
        Err(DsdContainerError::NotDsdContainer { .. }) => Err(ConvertError::Backend(format!(
            "standalone DSD source {} has a DSF/DFF extension but is not a DSF/DSDIFF container",
            path.display()
        ))),
        Err(err) => Err(ConvertError::Backend(format!(
            "failed to inspect standalone DSD source {}: {err}",
            path.display()
        ))),
    }
}

pub fn source_info_for_realized_track(
    track: &PreparedTrack,
    realized_input: &Path,
) -> Result<SourceInfo, ConvertError> {
    let format = planner_format_from_path(realized_input).unwrap_or_else(|| match &track.source_ref {
        TrackSourceRef::SacdTrack { .. } => PlannerFormat::Dsf,
        TrackSourceRef::DvdVideoTrack { .. } => PlannerFormat::Wav,
        _ => PlannerFormat::Flac,
    });
    let codec = codec_for_format(&format);
    // For SACD-realized tracks, DSD container inspection may fail on test
    // placeholders or when the file was already validated by the extraction
    // stage. Fall back gracefully rather than blocking the planner.
    let dsd_metadata = match dsd_source_metadata_from_path(realized_input) {
        Ok(metadata) => metadata,
        Err(_) if matches!(&track.source_ref, TrackSourceRef::SacdTrack { .. }) => None,
        Err(err) => return Err(err),
    };
    let is_dsd = format.is_dsd() || codec.is_dsd();
    let bit_depth = if is_dsd {
        None
    } else {
        cue_pcm_segment_carrier_bit_depth(track)
            .or(track.bit_depth)
            .and_then(pcm_bit_depth_from_u32)
    };
    let sample_kind = if is_dsd {
        Some(SampleKind::Dsd)
    } else {
        bit_depth.map(|depth| match depth {
            PcmBitDepth::Float32 | PcmBitDepth::Float64 => SampleKind::Float,
            _ => SampleKind::SignedInteger,
        })
    };
    let sample_rate_hz = dsd_metadata
        .as_ref()
        .map(|metadata| metadata.sample_rate_hz)
        .or(track.sample_rate);
    let channels = dsd_metadata.as_ref().map(|metadata| metadata.channels);
    let duration = dsd_metadata
        .as_ref()
        .and_then(DsdPlannerSourceMetadata::duration);

    Ok(SourceInfo {
        format,
        codec,
        sample_rate_hz,
        bit_depth,
        sample_kind,
        channels,
        duration,
        audio_md5: flac_streaminfo_audio_md5(realized_input),
    })
}

pub fn planner_format_from_main(format: crate::convert::AudioFormat) -> PlannerFormat {
    match format {
        crate::convert::AudioFormat::Flac => PlannerFormat::Flac,
        crate::convert::AudioFormat::Wav => PlannerFormat::Wav,
        crate::convert::AudioFormat::Aiff => PlannerFormat::Aiff,
        crate::convert::AudioFormat::WavPack => PlannerFormat::WavPack,
        crate::convert::AudioFormat::Mp3 => PlannerFormat::Mp3,
        crate::convert::AudioFormat::Aac => PlannerFormat::Aac,
        crate::convert::AudioFormat::Opus => PlannerFormat::Opus,
        crate::convert::AudioFormat::Alac => PlannerFormat::Alac,
        crate::convert::AudioFormat::Dsf => PlannerFormat::Dsf,
        crate::convert::AudioFormat::Dff => PlannerFormat::Dff,
        crate::convert::AudioFormat::Dts => PlannerFormat::Dts,
        crate::convert::AudioFormat::Ac3 => PlannerFormat::Ac3,
        crate::convert::AudioFormat::Ape => PlannerFormat::Flac, // Ape is decode-only; default target is FLAC
        crate::convert::AudioFormat::Lpcm => PlannerFormat::Wav, // LPCM maps to WAV container
    }
}

pub fn planner_format_from_path(path: &Path) -> Option<PlannerFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "flac" => Some(PlannerFormat::Flac),
        "wav" | "wave" => Some(PlannerFormat::Wav),
        "aiff" | "aif" | "aifc" => Some(PlannerFormat::Aiff),
        "wv" => Some(PlannerFormat::WavPack),
        "mp3" => Some(PlannerFormat::Mp3),
        "aac" | "m4a" | "mp4" => Some(PlannerFormat::Aac),
        "opus" => Some(PlannerFormat::Opus),
        "alac" => Some(PlannerFormat::Alac),
        "dsf" => Some(PlannerFormat::Dsf),
        "dff" => Some(PlannerFormat::Dff),
        _ => None,
    }
}

fn codec_for_format(format: &PlannerFormat) -> PlannerCodec {
    match format {
        PlannerFormat::Flac => PlannerCodec::Flac,
        PlannerFormat::Wav | PlannerFormat::Aiff => PlannerCodec::PcmSigned,
        PlannerFormat::WavPack => PlannerCodec::WavPack,
        PlannerFormat::Mp3 => PlannerCodec::Mp3,
        PlannerFormat::Aac => PlannerCodec::Aac,
        PlannerFormat::Opus => PlannerCodec::Opus,
        PlannerFormat::Alac => PlannerCodec::Alac,
        PlannerFormat::Dsf | PlannerFormat::Dff => PlannerCodec::Dsd,
        PlannerFormat::Dts => PlannerCodec::Custom("DTS".to_string()),
        PlannerFormat::Ac3 => PlannerCodec::Custom("AC3".to_string()),
        PlannerFormat::Custom { display_name, .. } => PlannerCodec::Custom(display_name.clone()),
    }
}

fn pcm_bit_depth_from_u32(bits: u32) -> Option<PcmBitDepth> {
    match bits {
        8 => Some(PcmBitDepth::Int8),
        16 => Some(PcmBitDepth::Int16),
        24 => Some(PcmBitDepth::Int24),
        32 => Some(PcmBitDepth::Int32),
        _ => None,
    }
}

fn cue_pcm_segment_carrier_bit_depth(track: &PreparedTrack) -> Option<u32> {
    match &track.source_ref {
        TrackSourceRef::CueSegmentCarrier { carrier, .. } => Some(carrier.bit_depth()),
        // Legacy callers that still use ImageSegment are realized by stages.rs as
        // the same validated PCM S32LE WAV carrier. New CUE materialization must
        // use the typed CueSegmentCarrier variant instead of relying on a path
        // convention.
        TrackSourceRef::ImageSegment { .. } => Some(CueSegmentCarrier::PcmS32LeWav.bit_depth()),
        _ => None,
    }
}

fn flac_streaminfo_audio_md5(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut magic = [0_u8; 4];
    file.read_exact(&mut magic).ok()?;
    if &magic != b"fLaC" {
        return None;
    }

    loop {
        let mut header = [0_u8; 4];
        file.read_exact(&mut header).ok()?;
        let is_last = header[0] & 0x80 != 0;
        let block_type = header[0] & 0x7f;
        let len = ((header[1] as u32) << 16) | ((header[2] as u32) << 8) | header[3] as u32;

        if block_type == 0 {
            if len < 34 {
                return None;
            }
            let mut streaminfo = vec![0_u8; len as usize];
            file.read_exact(&mut streaminfo).ok()?;
            let md5 = &streaminfo[18..34];
            if md5.iter().all(|byte| *byte == 0) {
                return None;
            }
            return Some(md5.iter().map(|byte| format!("{byte:02x}")).collect());
        }

        file.seek(SeekFrom::Current(len as i64)).ok()?;
        if is_last {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;
    use tonepoet_pipeline::{
        AudioFormat as PlannerFormat, PipelineSettings, PlanAction, PlanOperation, PlanRequest,
        PreferredTool, TopologyPlan,
    };

    use super::{
        apply_unsupported_target_metadata_policy_downgrades, dsd_source_metadata_from_path,
        flac_streaminfo_audio_md5, metadata_obligations_for_request,
        orchestrator_metadata_stage_required, plan_request_for_track,
        planner_metadata_obligations_for_track, source_audio_md5_policy_downgrade_message,
        source_info_for_realized_track, source_needs_authoritative_metadata,
        DsdPlannerSourceKind,
    };
    use crate::disc::bluray_backend::BluRayAudioCoding;
    use crate::convert::pipeline::types::{
        AlbumMetadata, CueSidecarPolicy, DvdaDownmixPolicy, DvdaGroupSelection,
        ExtractionProvenance, FailurePolicy, LogPolicy, CueSegmentCarrier,
        PlannedMetadataSatisfaction, NamingCollisionPolicy, NamingPolicy, OverwritePolicy,
        PipelineRequest, PreparedSource, PreparedTrack, PublishPolicy, SacdArea,
        SourceAudioDescriptor, SourceKind, SourceOptions, StagePolicy, StageRequirement, TrackId,
        TrackMetadata, TrackSelection, TrackSourceRef, CUE_ARTWORK_PATH_EXTRA_KEY,
    };

    fn request(root: &Path) -> PipelineRequest {
        PipelineRequest {
            job_id: "job".to_string(),
            item_id: "item".to_string(),
            container: root.join("album.iso"),
            source: SourceOptions {
                archive_password: None,
                sacd_area: Some(SacdArea::Stereo),
                dvda_group: None,
                dvda_group_selection: DvdaGroupSelection::Default,
                dvda_assume_decrypted: false,
                dvda_downmix_policy: DvdaDownmixPolicy::Auto,
                dvdv_vts: None,
                dvdv_title: None,
                dvdv_audio_stream: None,
                dvdv_angle: None,
                bluray_playlist: None,
                bluray_audio_pid: None,
                bluray_audio_stream: None,
                bluray_angle: None,
                cue_sidecar: CueSidecarPolicy::PreferSidecar,
                track_selection: TrackSelection::All,
            },
            settings: PipelineSettings::default(),
            worker_count: Some(1),
            merge: false,
            output_root: root.join("out"),
            naming: NamingPolicy {
                template: "%NN% - %TITLE%".to_string(),
                folder_template: None,
                per_album_subdir: true,
                collision_policy: NamingCollisionPolicy::Fail,
            },
            publish: PublishPolicy {
                overwrite: OverwritePolicy::FailIfExists,
                same_filesystem_required: false,
                write_manifest: false,
            },
            log: LogPolicy {
                root: root.join("logs"),
                write_for_blocked: false,
                write_json_log: false,
            },
            stages: StagePolicy {
                metadata: StageRequirement::Enabled,
                replaygain: StageRequirement::Disabled,
                features: StageRequirement::Disabled,
                generate_cue: false,
            },
            failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
            album_batch: None,
            album_batch_track: None,
            suppress_incremental_conversion_log_append: false,
            companion: Default::default(),
            pre_extracted_staging: None,
            archive_metadata_overrides: Vec::new(),
            expected_album_track_count: None,
            container_extension: None,
            container_ffmpeg_flags: Vec::new(),
        }
    }

    fn track(source_ref: TrackSourceRef) -> PreparedTrack {
        PreparedTrack {
            id: TrackId {
                source_ordinal: 1,
                disc_number: None,
                track_number: 1,
            },
            source_ref,
            metadata: TrackMetadata {
                title: Some("Track One".to_string()),
                track_number: Some(1),
                ..TrackMetadata::default()
            },
            expected_samples: Some(1_000),
            sample_rate: Some(2_822_400),
            bit_depth: None,
        source_audio: SourceAudioDescriptor::default(),
        }
    }

    fn cue_carrier(path: PathBuf, source_image: PathBuf, start_sample: u64, samples: u64) -> TrackSourceRef {
        TrackSourceRef::CueSegmentCarrier {
            path,
            source_image,
            start_sample,
            samples,
            carrier: CueSegmentCarrier::PcmS32LeWav,
        }
    }

    fn bluray_track_ref(source: PathBuf) -> TrackSourceRef {
        TrackSourceRef::BluRayTrack {
            source,
            playlist_number: 12,
            title_index: 0,
            angle_number: 1,
            chapter_number: 1,
            chapter_start_pts_90k: 0,
            chapter_end_pts_90k: Some(90_000),
            audio_pid: 0x1100,
            audio_stream_index: 0,
            audio_coding: BluRayAudioCoding::TrueHd,
            sample_rate: Some(192_000),
            bit_depth: Some(24),
            channels: Some(2),
            channel_layout: None,
        }
    }

    fn source(kind: SourceKind, track: PreparedTrack, root: &Path) -> PreparedSource {
        PreparedSource {
            container: root.join("album.iso"),
            kind,
            tracks: vec![track],
            album_metadata: AlbumMetadata {
                album: Some("Album".to_string()),
                album_artist: Some("Artist".to_string()),
                total_tracks: 1,
                ..AlbumMetadata::default()
            },
            provenance: ExtractionProvenance {
                source_kind: kind,
                source_sha256: None,
                tool_versions: BTreeMap::new(),
                extracted_at: chrono::Utc::now(),
            },
        }
    }

    fn planned_command_args(plan_request: &PlanRequest) -> Vec<Vec<String>> {
        let plan = tonepoet_pipeline::plan_conversion(plan_request).expect("planner builds command plan");
        match plan.action {
            PlanAction::Execute { commands, .. } => commands.into_iter().map(|cmd| cmd.args).collect(),
            PlanAction::PassthroughCopy { .. } => Vec::new(),
        }
    }

    fn planned_command_programs(plan_request: &PlanRequest) -> Vec<String> {
        let plan = tonepoet_pipeline::plan_conversion(plan_request).expect("planner builds command plan");
        match plan.action {
            PlanAction::Execute { commands, .. } => {
                commands.into_iter().map(|cmd| cmd.tool.program().to_string()).collect()
            }
            PlanAction::PassthroughCopy { .. } => {
                panic!("expected executable command plan, got passthrough copy")
            }
        }
    }

    fn has_adjacent_args(args: &[String], left: &str, right: &str) -> bool {
        args.windows(2).any(|window| window[0] == left && window[1] == right)
    }

    fn has_input_arg(args: &[String], expected_suffix: &str) -> bool {
        args.windows(2).any(|window| {
            window[0] == "-i" && window[1].replace('\\', "/").ends_with(expected_suffix)
        })
    }

    fn write_minimal_flac_with_md5(path: &Path) {
        let mut streaminfo = vec![0_u8; 34];
        streaminfo[18..34].copy_from_slice(&[
            0x00, 0x11, 0x22, 0x33,
            0x44, 0x55, 0x66, 0x77,
            0x88, 0x99, 0xaa, 0xbb,
            0xcc, 0xdd, 0xee, 0xff,
        ]);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"fLaC");
        bytes.extend_from_slice(&[0x80, 0x00, 0x00, 34]);
        bytes.extend_from_slice(&streaminfo);
        std::fs::write(path, bytes).expect("write minimal FLAC STREAMINFO");
    }

    fn write_minimal_flac_with_zero_md5(path: &Path) {
        let streaminfo = vec![0_u8; 34];
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"fLaC");
        bytes.extend_from_slice(&[0x80, 0x00, 0x00, 34]);
        bytes.extend_from_slice(&streaminfo);
        std::fs::write(path, bytes).expect("write minimal FLAC STREAMINFO with zero MD5");
    }


    fn write_minimal_dsf(path: &Path) {
        let file = std::fs::File::create(path).expect("create DSF fixture");
        let mut writer = sacd_rs::dsf_writer::DsfWriter::new(file, 2, 2_822_400)
            .expect("create DSF writer");
        writer
            .write_interleaved(&vec![0x69; 4_096])
            .expect("write DSF DSD payload");
        writer.finish().expect("finish DSF fixture");
    }

    fn write_minimal_dff_dsd(path: &Path) {
        let file = std::fs::File::create(path).expect("create DFF/DSD fixture");
        let mut writer = sacd_rs::dff_writer::DffWriter::new(file, 2, 2_822_400)
            .expect("create DFF/DSD writer");
        writer
            .write_frame(&vec![0x69; 4_096])
            .expect("write DFF/DSD payload");
        writer.finish().expect("finish DFF/DSD fixture");
    }

    fn write_minimal_dff_dst(path: &Path) {
        let file = std::fs::File::create(path).expect("create DFF/DST fixture");
        let mut writer = sacd_rs::dff_dst_writer::DffDstWriter::new(file, 2, 2_822_400)
            .expect("create DFF/DST writer");
        let decoded_crc_source = vec![
            0x69;
            sacd_rs::dst::dst_interleaved_frame_len_for_rate(sacd_rs::dst::DstRate::Dsd64, 2)
                .expect("DSD64 stereo DST frame geometry")
        ];
        writer
            .write_encoded_frame(&[0x80], &decoded_crc_source)
            .expect("write caller-supplied DST frame");
        writer.finish().expect("finish DFF/DST fixture");
    }


    fn topology_operations(plan_request: &PlanRequest) -> Vec<PlanOperation> {
        match tonepoet_pipeline::plan_topology(plan_request).expect("topology builds") {
            TopologyPlan::Execute { steps, .. } => {
                steps.into_iter().map(|step| step.operation).collect()
            }
            TopologyPlan::Passthrough { reason } => {
                panic!("expected executable topology, got passthrough: {reason}");
            }
        }
    }

    #[test]
    fn standalone_dsf_source_info_comes_from_container_header() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("source.dsf");
        write_minimal_dsf(&input);
        let mut prepared = track(TrackSourceRef::StagedFile(input.clone()));
        prepared.sample_rate = None;
        prepared.expected_samples = None;

        let metadata = dsd_source_metadata_from_path(&input)
            .expect("DSF inspection succeeds")
            .expect("DSF metadata exists");
        let source = source_info_for_realized_track(&prepared, &input).expect("source facts");

        assert_eq!(metadata.source_kind, DsdPlannerSourceKind::Dsf);
        assert!(!metadata.dst_compressed);
        assert_eq!(source.format, PlannerFormat::Dsf);
        assert_eq!(source.sample_rate_hz, Some(2_822_400));
        assert_eq!(source.channels, Some(2));
        assert_eq!(source.sample_kind, Some(tonepoet_pipeline::SampleKind::Dsd));
        assert!(source.bit_depth.is_none());
        assert!(source.duration.is_some());
    }

    #[test]
    fn dff_cmpr_classifies_dsdiff_dsd_and_dst_inputs() {
        let temp = TempDir::new().expect("temp dir");
        let dsd = temp.path().join("plain.dff");
        let dst = temp.path().join("compressed.dff");
        write_minimal_dff_dsd(&dsd);
        write_minimal_dff_dst(&dst);

        let dsd_metadata = dsd_source_metadata_from_path(&dsd)
            .expect("DFF/DSD inspection succeeds")
            .expect("DFF/DSD metadata exists");
        let dst_metadata = dsd_source_metadata_from_path(&dst)
            .expect("DFF/DST inspection succeeds")
            .expect("DFF/DST metadata exists");

        assert_eq!(dsd_metadata.source_kind, DsdPlannerSourceKind::DsdiffDsd);
        assert!(!dsd_metadata.dst_compressed);
        assert_eq!(dst_metadata.source_kind, DsdPlannerSourceKind::DsdiffDst);
        assert!(dst_metadata.dst_compressed);
    }

    #[test]
    fn standalone_dsf_to_flac_plans_dsd_to_pcm() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("source.dsf");
        write_minimal_dsf(&input);
        let output = temp.path().join("out.flac");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Flac;
        let track = track(TrackSourceRef::StagedFile(input.clone()));

        let planned = plan_request_for_track(&req, &track, &input, &output, temp.path().join("work"))
            .expect("DSF to FLAC plan request builds");
        let operations = topology_operations(&planned);

        assert_eq!(planned.source.format, PlannerFormat::Dsf);
        assert_eq!(planned.source.sample_rate_hz, Some(2_822_400));
        assert!(operations.iter().any(|operation| matches!(
            operation,
            PlanOperation::DsdToPcm {
                target_format: PlannerFormat::Flac,
                ..
            }
        )));
    }

    #[test]
    fn standalone_dff_dsd_to_dsf_plans_dsd_container_conversion() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("plain.dff");
        write_minimal_dff_dsd(&input);
        let output = temp.path().join("out.dsf");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Dsf;
        let track = track(TrackSourceRef::StagedFile(input.clone()));

        let metadata = dsd_source_metadata_from_path(&input)
            .expect("DFF/DSD inspection succeeds")
            .expect("DFF/DSD metadata exists");
        let planned = plan_request_for_track(&req, &track, &input, &output, temp.path().join("work"))
            .expect("DFF/DSD to DSF plan request builds");
        let operations = topology_operations(&planned);

        assert_eq!(metadata.source_kind, DsdPlannerSourceKind::DsdiffDsd);
        assert_eq!(planned.source.format, PlannerFormat::Dff);
        assert_eq!(planned.source.sample_rate_hz, Some(2_822_400));
        assert!(operations.iter().any(|operation| matches!(
            operation,
            PlanOperation::DsdRateChange {
                target_format: PlannerFormat::Dsf,
                ..
            }
        )));
    }

    #[test]
    fn standalone_dff_dst_to_dsf_plans_dst_decode_dsd_output() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("compressed.dff");
        write_minimal_dff_dst(&input);
        let output = temp.path().join("out.dsf");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Dsf;
        let track = track(TrackSourceRef::StagedFile(input.clone()));

        let metadata = dsd_source_metadata_from_path(&input)
            .expect("DFF/DST inspection succeeds")
            .expect("DFF/DST metadata exists");
        let planned = plan_request_for_track(&req, &track, &input, &output, temp.path().join("work"))
            .expect("DFF/DST to DSF plan request builds");
        let operations = topology_operations(&planned);

        assert_eq!(metadata.source_kind, DsdPlannerSourceKind::DsdiffDst);
        assert!(metadata.dst_compressed);
        assert_eq!(planned.source.format, PlannerFormat::Dff);
        assert_eq!(planned.source.sample_rate_hz, Some(2_822_400));
        assert!(operations.iter().any(|operation| matches!(
            operation,
            PlanOperation::DsdRateChange {
                target_format: PlannerFormat::Dsf,
                ..
            }
        )));
    }


    #[test]
    fn flac_streaminfo_audio_md5_reads_nonzero_streaminfo_md5() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("source.flac");
        write_minimal_flac_with_md5(&input);

        assert_eq!(
            flac_streaminfo_audio_md5(&input).as_deref(),
            Some("00112233445566778899aabbccddeeff"),
            "source-MD5 capability must come from the parsed FLAC STREAMINFO MD5 bytes"
        );
    }

    #[test]
    fn flac_streaminfo_audio_md5_rejects_zero_streaminfo_md5() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("source.flac");
        write_minimal_flac_with_zero_md5(&input);

        assert!(
            flac_streaminfo_audio_md5(&input).is_none(),
            "an all-zero FLAC STREAMINFO MD5 is not a usable source-audio MD5 obligation"
        );
    }

    #[test]
    fn flac_streaminfo_audio_md5_rejects_non_flac_input() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("source.dsf");
        std::fs::write(&input, b"DSD \x00\x00not flac").expect("write non-FLAC source");

        assert!(
            flac_streaminfo_audio_md5(&input).is_none(),
            "non-FLAC realized inputs must not create a source-MD5 capability"
        );
    }

    #[test]
    fn bluray_auto_plan_request_prefers_ffmpeg_and_resolves_24_bit_depth() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("realized.wav");
        std::fs::write(&input, b"placeholder pcm wav").expect("placeholder staged WAV");
        let output = temp.path().join("out.flac");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Flac;
        req.settings.preferred_tool = PreferredTool::Auto;
        let mut track = track(bluray_track_ref(temp.path().join("album.iso")));
        track.sample_rate = Some(192_000);
        track.bit_depth = Some(24);

        let planned = plan_request_for_track(&req, &track, &input, &output, temp.path().join("work"))
            .expect("Blu-ray plan request builds");

        assert_eq!(planned.settings.preferred_tool, PreferredTool::Ffmpeg);
        assert_eq!(planned.source.bit_depth, Some(tonepoet_pipeline::PcmBitDepth::Int24));
        assert_eq!(
            planned.settings.target_bit_depth,
            tonepoet_pipeline::BitDepthTarget::Pcm(tonepoet_pipeline::PcmBitDepth::Int24)
        );
    }

    #[test]
    fn bluray_auto_plan_builds_ffmpeg_encode_command_not_sox() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("realized.wav");
        std::fs::write(&input, b"placeholder pcm wav").expect("placeholder staged WAV");
        let output = temp.path().join("out.flac");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Flac;
        req.settings.preferred_tool = PreferredTool::Auto;
        let mut track = track(bluray_track_ref(temp.path().join("album.iso")));
        track.sample_rate = Some(192_000);
        track.bit_depth = Some(24);

        let planned = plan_request_for_track(&req, &track, &input, &output, temp.path().join("work"))
            .expect("Blu-ray plan request builds");
        let programs = planned_command_programs(&planned);

        assert!(
            programs.iter().any(|program| program == "ffmpeg"),
            "Blu-ray Auto FLAC encode should route through ffmpeg; got {programs:?}"
        );
        assert!(
            !programs.iter().any(|program| program == "sox"),
            "Blu-ray Auto FLAC encode must not route through sox; got {programs:?}"
        );
    }

    #[test]
    fn bluray_plan_request_keeps_explicit_user_tool_preference() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("realized.wav");
        std::fs::write(&input, b"placeholder pcm wav").expect("placeholder staged WAV");
        let output = temp.path().join("out.flac");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Flac;
        req.settings.preferred_tool = PreferredTool::Sox;
        let mut track = track(bluray_track_ref(temp.path().join("album.iso")));
        track.sample_rate = Some(192_000);
        track.bit_depth = Some(24);

        let planned = plan_request_for_track(&req, &track, &input, &output, temp.path().join("work"))
            .expect("Blu-ray plan request builds");

        assert_eq!(planned.settings.preferred_tool, PreferredTool::Sox);
        assert_eq!(
            planned.settings.target_bit_depth,
            tonepoet_pipeline::BitDepthTarget::Pcm(tonepoet_pipeline::PcmBitDepth::Int24)
        );
    }

    #[test]
    fn sacd_plan_request_suppresses_unsupported_source_tag_artwork_md5_policy() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("realized.dsf");
        write_minimal_dsf(&input);
        let output = temp.path().join("out.flac");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Flac;
        req.settings.metadata.transfer_tags = true;
        req.settings.metadata.preserve_artwork = true;
        req.settings.metadata.store_source_audio_md5 = true;
        let track = track(TrackSourceRef::SacdTrack {
            iso: temp.path().join("album.iso"),
            track_index: 1,
            area: SacdArea::Stereo,
        });

        let planned = plan_request_for_track(&req, &track, &input, &output, temp.path().join("work"))
            .expect("SACD plan request builds");

        assert!(!planned.settings.metadata.transfer_tags);
        assert!(!planned.settings.metadata.preserve_artwork);
        assert!(!planned.settings.metadata.store_source_audio_md5);
    }

    #[test]
    fn cue_pcm_segment_disables_source_container_metadata_policies() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("staging/cue-segments/realized-segment.wav");
        std::fs::create_dir_all(input.parent().unwrap()).expect("cue staging dir");
        std::fs::write(&input, b"placeholder pcm wav").expect("placeholder staged WAV");
        let output = temp.path().join("out.flac");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Flac;
        req.settings.metadata.transfer_tags = true;
        req.settings.metadata.preserve_artwork = true;
        req.settings.metadata.store_source_audio_md5 = true;
        let mut track = track(cue_carrier(input.clone(), temp.path().join("album.flac"), 0, 44_100));
        track.sample_rate = Some(44_100);
        track.bit_depth = Some(16);

        let planned = plan_request_for_track(&req, &track, &input, &output, temp.path().join("work"))
            .expect("CUE image-segment plan request builds");

        assert!(!planned.settings.metadata.transfer_tags);
        assert!(!planned.settings.metadata.preserve_artwork);
        assert!(!planned.settings.metadata.store_source_audio_md5);
        assert!(planned.settings.force_encode);
        assert_eq!(planned.source.bit_depth, Some(tonepoet_pipeline::PcmBitDepth::Int32));
        assert_eq!(planned.settings.target_bit_depth, tonepoet_pipeline::BitDepthTarget::Pcm(tonepoet_pipeline::PcmBitDepth::Int16));
    }

    #[test]
    fn staged_file_path_under_cue_segments_is_not_a_cue_carrier_without_typed_fact() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("staging/cue-segments/not-a-cue-carrier.wav");
        std::fs::create_dir_all(input.parent().unwrap()).expect("cue-looking staging dir");
        std::fs::write(&input, b"ordinary staged wav").expect("placeholder staged WAV");
        let output = temp.path().join("out.flac");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Flac;
        req.settings.metadata.transfer_tags = true;
        req.settings.metadata.preserve_artwork = true;
        let mut track = track(TrackSourceRef::StagedFile(input.clone()));
        track.sample_rate = Some(44_100);
        track.bit_depth = Some(16);

        let planned = plan_request_for_track(&req, &track, &input, &output, temp.path().join("work"))
            .expect("ordinary staged WAV plan request builds");

        assert!(planned.settings.metadata.transfer_tags);
        assert!(planned.settings.metadata.preserve_artwork);
        assert!(!planned.settings.force_encode);
        assert_eq!(planned.source.bit_depth, Some(tonepoet_pipeline::PcmBitDepth::Int16));
    }

    #[test]
    fn cue_metadata_obligations_require_both_source_transfer_and_authoritative_tags() {
        let temp = TempDir::new().expect("temp dir");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Flac;
        req.settings.metadata.transfer_tags = true;
        req.settings.metadata.preserve_artwork = true;
        let src = source(
            SourceKind::CueImage,
            track(TrackSourceRef::ImageSegment {
                image: temp.path().join("album.flac"),
                start_sample: 0,
                samples: 44_100,
            }),
            temp.path(),
        );

        let obligations = metadata_obligations_for_request(&req, &src);

        assert!(!obligations.source_tags_transferred);
        assert!(!obligations.artwork_transferred);
        assert!(obligations.authoritative_tags_applied);
        assert!(!obligations.source_audio_md5_written);
    }

    #[test]
    fn cue_artwork_obligation_is_present_only_with_extracted_sidecar_and_supported_target() {
        let temp = TempDir::new().expect("temp dir");
        let mut req = request(temp.path());
        req.settings.metadata.preserve_artwork = true;
        req.settings.target_format = PlannerFormat::Mp3;
        let mut src = source(
            SourceKind::CueImage,
            track(TrackSourceRef::ImageSegment {
                image: temp.path().join("album.flac"),
                start_sample: 0,
                samples: 44_100,
            }),
            temp.path(),
        );
        src.album_metadata.extra.insert(
            CUE_ARTWORK_PATH_EXTRA_KEY.to_string(),
            temp.path().join("cue-artwork/cover.jpg").display().to_string(),
        );

        let obligations = metadata_obligations_for_request(&req, &src);
        assert!(obligations.artwork_transferred);

        req.settings.target_format = PlannerFormat::Opus;
        let opus_obligations = metadata_obligations_for_request(&req, &src);
        assert!(
            !opus_obligations.artwork_transferred,
            "Opus/Ogg CUE artwork remains unsupported until a METADATA_BLOCK_PICTURE writer exists"
        );
    }

    #[test]
    fn cue_validated_pcm_segment_plans_directly_to_multiformat_targets() {
        let formats = [
            (PlannerFormat::Flac, "flac"),
            (PlannerFormat::Wav, "wav"),
            (PlannerFormat::WavPack, "wv"),
            (PlannerFormat::Opus, "opus"),
            (PlannerFormat::Aac, "m4a"),
            (PlannerFormat::Mp3, "mp3"),
            (PlannerFormat::Alac, "m4a"),
        ];

        for (format, extension) in formats.iter().cloned() {
            let temp = TempDir::new().expect("temp dir");
            let input = temp.path().join("staging/cue-segments/realized-segment.wav");
            std::fs::create_dir_all(input.parent().unwrap()).expect("cue staging dir");
            std::fs::write(&input, b"placeholder pcm wav").expect("placeholder staged WAV");
            let output = temp.path().join(format!("out.{extension}"));
            let mut req = request(temp.path());
            req.settings.target_format = format.clone();
            req.settings.metadata.transfer_tags = true;
            req.settings.metadata.preserve_artwork = true;
            let mut prepared = track(cue_carrier(input.clone(), temp.path().join("album.flac"), 0, 44_100));
            prepared.sample_rate = Some(44_100);
            prepared.bit_depth = Some(16);

            let planned = plan_request_for_track(
                &req,
                &prepared,
                &input,
                &output,
                temp.path().join("work"),
            )
            .expect("CUE image-segment plan request builds for target");

            assert_eq!(planned.source.format, PlannerFormat::Wav);
            assert_eq!(planned.source.bit_depth, Some(tonepoet_pipeline::PcmBitDepth::Int32));
            assert_eq!(planned.settings.target_bit_depth, tonepoet_pipeline::BitDepthTarget::Pcm(tonepoet_pipeline::PcmBitDepth::Int16));
            assert!(planned.settings.force_encode);
            assert_eq!(planned.settings.target_format, format);
            assert_eq!(
                planned.output_path.extension().and_then(|value| value.to_str()),
                Some(extension)
            );
            let _ = tonepoet_pipeline::plan_conversion(&planned)
                .expect("planner accepts validated PCM WAV input for target");
        }
    }

    #[test]
    fn cue_wav_target_reencodes_from_s32_carrier_to_original_source_depth() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("staging/cue-segments/track01.wav");
        std::fs::create_dir_all(input.parent().unwrap()).expect("cue staging dir");
        std::fs::write(&input, b"placeholder pcm wav").expect("placeholder staged WAV");
        let output = temp.path().join("out.wav");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Wav;
        req.settings.target_bit_depth = tonepoet_pipeline::BitDepthTarget::Source;
        req.settings.metadata.transfer_tags = true;
        req.settings.metadata.preserve_artwork = true;
        let mut prepared = track(cue_carrier(input.clone(), temp.path().join("album.flac"), 0, 44_100));
        prepared.sample_rate = Some(44_100);
        prepared.bit_depth = Some(16);

        let planned = plan_request_for_track(
            &req,
            &prepared,
            &input,
            &output,
            temp.path().join("work"),
        )
        .expect("CUE staged WAV plan request builds");
        let plan = tonepoet_pipeline::plan_topology(&planned).expect("topology builds");

        assert!(planned.settings.force_encode);
        assert_eq!(planned.source.bit_depth, Some(tonepoet_pipeline::PcmBitDepth::Int32));
        assert_eq!(planned.settings.target_bit_depth, tonepoet_pipeline::BitDepthTarget::Pcm(tonepoet_pipeline::PcmBitDepth::Int16));
        match plan {
            tonepoet_pipeline::TopologyPlan::Execute { steps, .. } => {
                assert!(steps.iter().any(|step| matches!(
                    step.operation,
                    tonepoet_pipeline::PlanOperation::EncodePcm {
                        target_format: PlannerFormat::Wav,
                        target_bit_depth: tonepoet_pipeline::PcmBitDepth::Int16,
                        ..
                    }
                )));
            }
            tonepoet_pipeline::TopologyPlan::Passthrough { reason } => {
                panic!("CUE staged S32 WAV must not passthrough-copy: {reason}");
            }
        }
    }

    #[test]
    fn staged_dsf_plan_request_preserves_source_tag_artwork_but_disables_unavailable_source_md5() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("source.dsf");
        write_minimal_dsf(&input);
        let output = temp.path().join("out.flac");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Flac;
        req.settings.metadata.transfer_tags = true;
        req.settings.metadata.preserve_artwork = true;
        req.settings.metadata.store_source_audio_md5 = true;
        let track = track(TrackSourceRef::StagedFile(input.clone()));

        let planned = plan_request_for_track(&req, &track, &input, &output, temp.path().join("work"))
            .expect("staged DSF plan request builds");

        assert!(planned.settings.metadata.transfer_tags);
        assert!(planned.settings.metadata.preserve_artwork);
        assert!(
            !planned.settings.metadata.store_source_audio_md5,
            "standalone DSF has no FLAC STREAMINFO MD5 and must not fail planner validation"
        );
    }

    #[test]
    fn missing_source_audio_md5_policy_downgrade_is_reportable() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("source.dsf");
        write_minimal_dsf(&input);
        let mut req = request(temp.path());
        req.settings.metadata.store_source_audio_md5 = true;
        let mut track = track(TrackSourceRef::StagedFile(input.clone()));
        track.metadata.title = Some("Track With No Source MD5".to_string());
        let source = source_info_for_realized_track(&track, &input).expect("source facts");

        let message = source_audio_md5_policy_downgrade_message(&req, &track, &input, &source)
            .expect("requested-but-unavailable source MD5 should be reported");

        assert!(message.contains("metadata.store_source_audio_md5 requested"));
        assert!(message.contains("Track With No Source MD5"));
        assert!(message.contains("source.dsf"));
        assert!(message.contains("unsupported for this track"));
    }

    #[test]
    fn available_source_audio_md5_policy_does_not_report_downgrade() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("source.flac");
        write_minimal_flac_with_md5(&input);
        let mut req = request(temp.path());
        req.settings.metadata.store_source_audio_md5 = true;
        let track = track(TrackSourceRef::StagedFile(input.clone()));
        let source = source_info_for_realized_track(&track, &input).expect("source facts");

        assert!(source_audio_md5_policy_downgrade_message(&req, &track, &input, &source).is_none());
    }


    #[test]
    fn unsupported_target_metadata_policy_downgrade_is_reportable_and_disables_flags() {
        let mut settings = PipelineSettings::default();
        settings.target_format = PlannerFormat::Dsf;
        settings.metadata.transfer_tags = true;
        settings.metadata.preserve_artwork = true;

        let messages = apply_unsupported_target_metadata_policy_downgrades(&mut settings);

        assert!(!settings.metadata.transfer_tags);
        assert!(!settings.metadata.preserve_artwork);
        assert_eq!(messages.len(), 2);
        assert!(messages.iter().any(|message| message.contains("metadata.transfer_tags requested")));
        assert!(messages.iter().any(|message| message.contains("metadata.preserve_artwork requested")));
        assert!(messages.iter().all(|message| message.contains("unsupported for this track")));
    }

    #[test]
    fn opus_target_downgrades_planner_source_tags_and_artwork() {
        let mut settings = PipelineSettings::default();
        settings.target_format = PlannerFormat::Opus;
        settings.metadata.transfer_tags = true;
        settings.metadata.preserve_artwork = true;

        let messages = apply_unsupported_target_metadata_policy_downgrades(&mut settings);

        assert!(
            !settings.metadata.transfer_tags,
            "Opus authoritative CUE tags are written by the post-encode opustags stage, not by planner source-tag transfer"
        );
        assert!(
            !settings.metadata.preserve_artwork,
            "Opus/Ogg artwork remains unsupported until a METADATA_BLOCK_PICTURE writer exists"
        );
        assert_eq!(messages.len(), 2);
        assert!(messages.iter().any(|message| message.contains("metadata.transfer_tags requested")));
        assert!(messages.iter().any(|message| message.contains("metadata.preserve_artwork requested")));
    }

    #[test]
    fn unsupported_target_metadata_policy_is_disabled_before_planning() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("source.dsf");
        write_minimal_dsf(&input);
        let output = temp.path().join("out.dsf");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Dsf;
        req.settings.metadata.transfer_tags = true;
        req.settings.metadata.preserve_artwork = true;
        let track = track(TrackSourceRef::StagedFile(input.clone()));

        let planned = plan_request_for_track(&req, &track, &input, &output, temp.path().join("work"))
            .expect("plan request downgrades unsupported target metadata policies before validation");

        assert!(!planned.settings.metadata.transfer_tags);
        assert!(!planned.settings.metadata.preserve_artwork);
    }

    #[test]
    fn staged_flac_plan_preserves_available_source_md5_policy() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("source.flac");
        write_minimal_flac_with_md5(&input);
        let output = temp.path().join("out.flac");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Flac;
        req.settings.metadata.store_source_audio_md5 = true;
        let track = track(TrackSourceRef::StagedFile(input.clone()));

        let planned = plan_request_for_track(&req, &track, &input, &output, temp.path().join("work"))
            .expect("staged FLAC plan request builds");

        assert!(
            planned.settings.metadata.store_source_audio_md5,
            "standalone FLAC with STREAMINFO MD5 keeps source-audio MD5 policy"
        );
        assert_eq!(
            planned.source.audio_md5.as_deref(),
            Some("00112233445566778899aabbccddeeff")
        );
    }

    #[test]
    fn sacd_flac_plan_has_no_ffmpeg_map_metadata_or_source_md5_from_materialized_dsf() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("realized.dsf");
        write_minimal_dsf(&input);
        let output = temp.path().join("out.flac");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Flac;
        req.settings.metadata.transfer_tags = true;
        req.settings.metadata.preserve_artwork = true;
        req.settings.metadata.store_source_audio_md5 = true;
        let track = track(TrackSourceRef::SacdTrack {
            iso: temp.path().join("album.iso"),
            track_index: 1,
            area: SacdArea::Stereo,
        });

        let planned = plan_request_for_track(&req, &track, &input, &output, temp.path().join("work"))
            .expect("SACD plan request builds");
        let commands = planned_command_args(&planned);

        assert!(
            !commands.iter().any(|args| has_adjacent_args(args, "-map_metadata", "1")),
            "SACD materialized DSF must not be used as an FFmpeg metadata source: {commands:?}"
        );
        assert!(
            !commands.iter().any(|args| has_input_arg(args, "realized.dsf") && has_adjacent_args(args, "-map_metadata", "1")),
            "no command may copy metadata from the materialized SACD carrier: {commands:?}"
        );
        assert!(
            !commands.iter().flatten().any(|arg| arg.contains("SOURCE_AUDIO_MD5")),
            "SACD source-audio MD5 is unsupported for materialized DSF/DFF carriers and must not be planned: {commands:?}"
        );
    }

    #[test]
    fn staged_dsf_and_dff_flac_plans_still_use_ffmpeg_source_metadata_transfer() {
        for ext in ["dsf", "dff"] {
            let temp = TempDir::new().expect("temp dir");
            let input = temp.path().join(format!("source.{ext}"));
            match ext {
                "dsf" => write_minimal_dsf(&input),
                "dff" => write_minimal_dff_dsd(&input),
                _ => unreachable!(),
            }
            let output = temp.path().join(format!("out-{ext}.flac"));
            let mut req = request(temp.path());
            req.settings.target_format = PlannerFormat::Flac;
            req.settings.metadata.transfer_tags = true;
            req.settings.metadata.preserve_artwork = true;
            let track = track(TrackSourceRef::StagedFile(input.clone()));

            let planned = plan_request_for_track(&req, &track, &input, &output, temp.path().join("work"))
                .expect("staged DSD plan request builds");
            let commands = planned_command_args(&planned);
            let expected_source = format!("source.{ext}");

            assert!(
                commands.iter().any(|args| {
                    has_adjacent_args(args, "-map_metadata", "1") && has_input_arg(args, &expected_source)
                }),
                "standalone/staged {ext} keeps its source metadata transfer path: {commands:?}"
            );
        }
    }

    #[test]
    fn sacd_metadata_obligations_exclude_unsupported_source_tag_artwork_md5_copy() {
        let temp = TempDir::new().expect("temp dir");
        let mut req = request(temp.path());
        req.settings.metadata.transfer_tags = true;
        req.settings.metadata.preserve_artwork = true;
        req.settings.metadata.store_source_audio_md5 = true;
        let src = source(
            SourceKind::SacdIso,
            track(TrackSourceRef::SacdTrack {
                iso: temp.path().join("album.iso"),
                track_index: 1,
                area: SacdArea::Stereo,
            }),
            temp.path(),
        );

        let obligations = metadata_obligations_for_request(&req, &src);

        assert!(source_needs_authoritative_metadata(&src));
        assert!(obligations.authoritative_tags_applied);
        assert!(
            !obligations.source_audio_md5_written,
            "SACD source-audio MD5 is unsupported because the materialized DSF/DFF source has no FLAC STREAMINFO MD5"
        );
        assert!(
            !obligations.source_tags_transferred,
            "SACD sidecar/TOC metadata is authoritative; generated DSF source tags are not"
        );
        assert!(
            !obligations.artwork_transferred,
            "SACD artwork_transferred preservation is unsupported by the current materializer and must not be counted as satisfied"
        );
    }

    #[test]
    fn single_file_dsf_metadata_obligations_keep_source_tag_artwork_for_artwork_capable_target_but_not_md5_policy() {
        let temp = TempDir::new().expect("temp dir");
        let mut req = request(temp.path());
        req.settings.metadata.transfer_tags = true;
        req.settings.metadata.preserve_artwork = true;
        req.settings.metadata.store_source_audio_md5 = true;
        let src = source(
            SourceKind::SingleFile,
            track(TrackSourceRef::StagedFile(temp.path().join("source.dsf"))),
            temp.path(),
        );

        let obligations = metadata_obligations_for_request(&req, &src);

        assert!(obligations.source_tags_transferred);
        assert!(obligations.artwork_transferred);
        assert!(
            !obligations.source_audio_md5_written,
            "DSF source-audio MD5 is unsupported unless the realized input exposes FLAC STREAMINFO MD5"
        );
        assert!(!obligations.authoritative_tags_applied);
    }




    #[test]
    fn single_file_dsf_metadata_obligations_do_not_require_artwork_for_artwork_incapable_target() {
        let temp = TempDir::new().expect("temp dir");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Wav;
        req.settings.metadata.transfer_tags = true;
        req.settings.metadata.preserve_artwork = true;
        let src = source(
            SourceKind::SingleFile,
            track(TrackSourceRef::StagedFile(temp.path().join("source.dsf"))),
            temp.path(),
        );

        let obligations = metadata_obligations_for_request(&req, &src);

        assert!(
            obligations.source_tags_transferred,
            "WAV can still carry text tags through the planner policy"
        );
        assert!(
            !obligations.artwork_transferred,
            "artwork preservation must not be required for targets whose metadata transfer plugin cannot preserve artwork"
        );
    }

    #[test]
    fn source_tag_obligations_do_not_require_transfer_for_tag_incapable_target() {
        let temp = TempDir::new().expect("temp dir");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Dsf;
        req.settings.metadata.transfer_tags = true;
        let src = source(
            SourceKind::SingleFile,
            track(TrackSourceRef::StagedFile(temp.path().join("source.dsf"))),
            temp.path(),
        );

        let obligations = metadata_obligations_for_request(&req, &src);

        assert!(
            !obligations.source_tags_transferred,
            "source-tag obligations must be limited to targets whose planner/plugin metadata path can carry tags"
        );
    }

    #[test]
    fn planner_track_obligations_do_not_require_source_tags_for_tag_incapable_target() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("source.dsf");
        write_minimal_dsf(&input);
        let output = temp.path().join("out.dsf");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Dsf;
        req.settings.metadata.transfer_tags = true;
        let track = track(TrackSourceRef::StagedFile(input.clone()));
        let planned = PlanRequest {
            input_path: input.clone(),
            output_path: output,
            source: source_info_for_realized_track(&track, &input).expect("source facts"),
            settings: req.settings.clone(),
            intermediate_dir: Some(temp.path().join("work")),
            container_ffmpeg_flags: Vec::new(),
        };

        let obligations = planner_metadata_obligations_for_track(&req, &planned);

        assert!(planned.settings.metadata.transfer_tags);
        assert!(
            !obligations.source_tags_transferred,
            "a requested transfer_tags flag is not an obligation when the target/plugin cannot represent source tags"
        );
    }

    #[test]
    fn source_level_metadata_obligations_do_not_infer_md5_from_flac_extension() {
        let temp = TempDir::new().expect("temp dir");
        let mut req = request(temp.path());
        req.settings.metadata.store_source_audio_md5 = true;
        let src = source(
            SourceKind::SingleFile,
            track(TrackSourceRef::StagedFile(temp.path().join("source.flac"))),
            temp.path(),
        );

        let obligations = metadata_obligations_for_request(&req, &src);

        assert!(
            !obligations.source_audio_md5_written,
            "source-level obligations must not treat a .flac extension as proof of a usable STREAMINFO MD5"
        );
        assert!(!obligations.authoritative_tags_applied);
    }

    #[test]
    fn planner_track_obligations_keep_source_md5_only_when_streaminfo_md5_is_present() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("source.flac");
        write_minimal_flac_with_md5(&input);
        let output = temp.path().join("out.flac");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Flac;
        req.settings.metadata.store_source_audio_md5 = true;
        let track = track(TrackSourceRef::StagedFile(input.clone()));

        let planned = plan_request_for_track(&req, &track, &input, &output, temp.path().join("work"))
            .expect("staged FLAC plan request builds");
        let obligations = planner_metadata_obligations_for_track(&req, &planned);

        assert!(obligations.source_audio_md5_written);
        assert_eq!(planned.source.audio_md5.as_deref(), Some("00112233445566778899aabbccddeeff"));
    }

    #[test]
    fn planner_track_obligations_drop_source_md5_for_flac_with_missing_streaminfo_md5() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("source.flac");
        write_minimal_flac_with_zero_md5(&input);
        let output = temp.path().join("out.flac");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Flac;
        req.settings.metadata.store_source_audio_md5 = true;
        let track = track(TrackSourceRef::StagedFile(input.clone()));

        let planned = plan_request_for_track(&req, &track, &input, &output, temp.path().join("work"))
            .expect("staged FLAC plan request builds without source MD5");
        let obligations = planner_metadata_obligations_for_track(&req, &planned);

        assert!(!planned.settings.metadata.store_source_audio_md5);
        assert!(planned.source.audio_md5.is_none());
        assert!(
            !obligations.source_audio_md5_written,
            "a .flac path with absent/zero STREAMINFO MD5 must not create an unsatisfiable source-MD5 obligation"
        );
    }


    #[test]
    fn planner_track_obligations_drop_source_md5_for_non_flac_input() {
        let temp = TempDir::new().expect("temp dir");
        let input = temp.path().join("source.dsf");
        write_minimal_dsf(&input);
        let output = temp.path().join("out.flac");
        let mut req = request(temp.path());
        req.settings.target_format = PlannerFormat::Flac;
        req.settings.metadata.store_source_audio_md5 = true;
        let track = track(TrackSourceRef::StagedFile(input.clone()));

        let planned = plan_request_for_track(&req, &track, &input, &output, temp.path().join("work"))
            .expect("non-FLAC plan request builds without source MD5");
        let obligations = planner_metadata_obligations_for_track(&req, &planned);

        assert!(!planned.settings.metadata.store_source_audio_md5);
        assert!(planned.source.audio_md5.is_none());
        assert!(
            !obligations.source_audio_md5_written,
            "non-FLAC inputs do not expose FLAC STREAMINFO MD5 and must not create source-MD5 obligations"
        );
    }

    #[test]
    fn md5_satisfaction_cannot_stand_in_for_authoritative_tags() {
        let required = PlannedMetadataSatisfaction {
            source_tags_transferred: true,
            artwork_transferred: false,
            source_audio_md5_written: true,
            authoritative_tags_applied: true,
        };
        let actual = PlannedMetadataSatisfaction {
            source_audio_md5_written: true,
            ..PlannedMetadataSatisfaction::none()
        };

        assert!(orchestrator_metadata_stage_required(
            actual,
            StageRequirement::Enabled,
            required,
        ));
    }

    #[test]
    fn exact_dimensional_satisfaction_allows_skip() {
        let required = PlannedMetadataSatisfaction {
            source_tags_transferred: true,
            artwork_transferred: true,
            source_audio_md5_written: true,
            authoritative_tags_applied: false,
        };
        let actual = PlannedMetadataSatisfaction {
            source_tags_transferred: true,
            artwork_transferred: true,
            source_audio_md5_written: true,
            authoritative_tags_applied: false,
        };

        assert!(!orchestrator_metadata_stage_required(
            actual,
            StageRequirement::Enabled,
            required,
        ));
    }
}
