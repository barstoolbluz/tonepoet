//! Unified `ConversionItem` -> `PipelineRequest` builder.
//!
//! This is the only processor-side request builder. It preserves every setting
//! the UI/backend surface exposes and maps them into the pure planner's
//! `PipelineSettings` type before the orchestrator starts materialization.

use std::path::{Path, PathBuf};

use tonepoet_pipeline::{
    AacProfile as PlannerAacProfile, AudioFormat as PlannerAudioFormat, BitDepthTarget,
    DitherType as PlannerDitherType, DsdRate, DsdSettings, Mp3Mode as PlannerMp3Mode, NyquistTransition,
    OpusContentType, PcmBitDepth, PipelineSettings, PreferredTool, RateTarget,
    ReplayGainMode as PlannerReplayGainMode, ResampleQuality,
    SsrcProfile, WavPackMode as PlannerWavPackMode,
};

use crate::convert::formats::{AacProfile, Mp3BitrateMode, QualitySettings, WavPackMode};
use crate::convert::pipeline::{
    CueSidecarPolicy, DvdaDownmixPolicy, DvdaGroupSelection, FailurePolicy, LogPolicy, NamingCollisionPolicy,
    MetadataTextOverride, NamingPolicy, OverwritePolicy, PipelineRequest, PublishPolicy,
    RequestMetadataOverrides, SecretString, SourceOptions, StagePolicy, StageRequirement,
    TrackSelection,
};
use crate::convert::{ConversionError, ConversionItem, ConversionResult};

pub fn build_pipeline_request(item: &ConversionItem) -> ConversionResult<PipelineRequest> {
    // Return a prebuilt PipelineRequest with full PipelineSettings, while still
    // honoring queue-time source overrides. The request may have been built
    // before browse expansion attached its sidecar-CUE decision, so the
    // ConversionItem remains authoritative for this field.
    if let Some(mut request) = item.pipeline_request.clone() {
        request.settings.validate().map_err(|err| {
            ConversionError::ValidationError(format!("invalid prebuilt pipeline settings: {err}"))
        })?;
        if let Some(cue_sidecar_override) = item.cue_sidecar_override {
            request.source.cue_sidecar = cue_sidecar_override;
        }
        merge_request_metadata_overrides_for_item(&mut request, item);
        return Ok(request);
    }

    if let Some(settings) = item.pipeline_settings.clone() {
        return build_pipeline_request_from_settings(item, settings);
    }

    if let Some(settings) = item.options.pipeline_settings.clone() {
        return build_pipeline_request_from_settings(item, settings);
    }

    Err(ConversionError::ValidationError(
        "ConversionItem is missing full PipelineSettings; UI/CLI callers must set \
         ConversionOptions.pipeline_settings, ConversionItem.pipeline_settings, attach full \
         Chunk 1 settings with attach_full_pipeline_settings(), or call \
         process_item_with_pipeline_settings()"
            .to_string(),
    ))
}

/// Attach exact, already unified planner settings to a conversion item.
///
/// This is the mandatory UI/CLI handoff path for normal execution. It avoids
/// projecting rich planner settings through the legacy `ConversionOptions`
/// struct and therefore prevents silent field loss.
pub fn attach_full_pipeline_settings(
    mut item: ConversionItem,
    settings: PipelineSettings,
) -> ConversionResult<ConversionItem> {
    item.options.pipeline_settings = Some(settings.clone());
    item.pipeline_settings = Some(settings.clone());
    let request = build_pipeline_request_from_settings(&item, settings)?;
    item.pipeline_request = Some(request);
    Ok(item)
}

/// Build a `PipelineRequest` from a caller-supplied, already unified
/// `PipelineSettings` value. UI/TUI code that exposes Chunk 1 settings should
/// call this path rather than round-tripping through the legacy option surface.
/// The function validates the settings but otherwise preserves them byte-for-byte.
pub fn build_pipeline_request_from_settings(
    item: &ConversionItem,
    settings: PipelineSettings,
) -> ConversionResult<PipelineRequest> {
    settings.validate().map_err(|err| {
        ConversionError::ValidationError(format!("invalid pipeline settings: {err}"))
    })?;

    let output_root = item
        .options
        .output_dir
        .clone()
        .map(|p| expand_tilde(&p))
        .unwrap_or_else(|| {
            item.input_path
                .parent()
                .unwrap_or(Path::new("."))
                .to_path_buf()
        });

    let cue_policy = item
        .cue_sidecar_override
        .unwrap_or_else(|| cue_policy_for_input_path(&item.input_path));

    Ok(PipelineRequest {
        job_id: format!("job-{}", item.id),
        item_id: item.id.clone(),
        // The processor re-applies this via apply_conversion_options_request_contract;
        // seed the same value so both builder paths agree.
        actions: item.options.actions.clone(),
        container: item.input_path.clone(),
        source: SourceOptions {
            archive_password: item
                .archive_password
                .as_ref()
                .map(|password| SecretString::new(password.clone())),
            sacd_area: None,
            dvda_group: None,
            dvda_group_selection: DvdaGroupSelection::Default,
            dvda_assume_decrypted: false,
            dvda_downmix_policy: DvdaDownmixPolicy::Auto,
            cue_sidecar: cue_policy,
            track_selection: TrackSelection::All,
            dvdv_vts: None,
            dvdv_title: None,
            dvdv_audio_stream: None,
            dvdv_angle: None,
            bluray_playlist: None,
            bluray_audio_pid: None,
            bluray_audio_stream: None,
            bluray_angle: None,
        },
        settings,
        worker_count: None,
        scratch_staging: None,
        merge: item.options.merge_to_single,
        output_root: output_root.clone(),
        naming: NamingPolicy {
            template: item
                .options
                .naming_template
                .clone()
                .unwrap_or_else(|| "%NN% - %TITLE%".to_string()),
            folder_template: item.options.folder_template.clone(),
            per_album_subdir: true,
            collision_policy: if item.options.overwrite {
                NamingCollisionPolicy::Fail
            } else {
                NamingCollisionPolicy::AppendStableSuffix
            },
            // Direct compatibility callers have no TonepoetConfig handle.
            // UI/CLI production paths attach a prebuilt request carrying the
            // configured policy before this fallback is reached.
            windows_portable: false,
        },
        publish: PublishPolicy {
            overwrite: if item.options.overwrite {
                OverwritePolicy::ReplaceWithBackup
            } else {
                OverwritePolicy::FailIfExists
            },
            same_filesystem_required: false,
            write_manifest: false,
        },
        log: LogPolicy {
            root: output_root.join(".tonepoet-logs"),
            write_for_blocked: true,
            write_json_log: item.options.write_log_file,
            write_conversion_log: item.options.write_log_file,
        },
        stages: StagePolicy {
            metadata: if item.options.preserve_metadata {
                StageRequirement::Enabled
            } else {
                StageRequirement::Disabled
            },
            replaygain: if item.options.calculate_replaygain {
                StageRequirement::Enabled
            } else {
                StageRequirement::Disabled
            },
            features: StageRequirement::Enabled,
            generate_cue: item.options.generate_cue_files,
        },
        failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
        container_extension: item.options.container_extension.clone(),
        container_ffmpeg_flags: item.options.container_ffmpeg_flags.clone(),
        companion: super::types::CompanionCopyPolicy {
            extensions: item.options.effective_companion_extensions(),
            folders: item.options.effective_companion_folders(),
            exclude_files: item.options.effective_companion_exclude_files(),
        },
        pre_extracted_staging: item.pre_extracted_staging.clone(),
        archive_metadata_overrides: Vec::new(),
        metadata_overrides: request_metadata_overrides_for_item(item),
        batch_resolved_identity: None,
        album_batch: None,
        album_batch_track: None,
        expected_album_track_count: None,
        suppress_incremental_conversion_log_append: false,
    })
}

fn request_metadata_overrides_for_item(item: &ConversionItem) -> RequestMetadataOverrides {
    RequestMetadataOverrides {
        album_artist: item
            .options
            .album_artist_override
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(|value| MetadataTextOverride::Set(value.to_string()))
            .unwrap_or_default(),
    }
}

fn merge_request_metadata_overrides_for_item(
    request: &mut PipelineRequest,
    item: &ConversionItem,
) {
    let overrides = request_metadata_overrides_for_item(item);
    if !overrides.album_artist.is_keep() {
        request.metadata_overrides.album_artist = overrides.album_artist;
    }
}

/// Explicit compatibility-only projection from legacy `ConversionOptions`.
///
/// This function is intentionally not used by `build_pipeline_request()`. It is
/// retained for migration tests and one-off compatibility tooling where a caller
/// cannot yet construct full Chunk 1 `PipelineSettings`. Normal UI/CLI entry
/// points must use `attach_full_pipeline_settings()` or provide a prebuilt
/// `PipelineRequest` on `ConversionItem`.
#[deprecated(
    note = "legacy ConversionOptions cannot represent all PipelineSettings; attach full PipelineSettings instead"
)]
pub fn build_pipeline_request_from_legacy_options(
    item: &ConversionItem,
) -> ConversionResult<PipelineRequest> {
    let settings = legacy_pipeline_settings_for_item(item)?;
    build_pipeline_request_from_settings(item, settings)
}

/// Build checked `PipelineSettings` from legacy `ConversionOptions` fields.
///
/// This compatibility projection is still used at CLI/TUI boundaries that have
/// not yet been migrated to construct the planner settings directly. Invalid
/// numeric depth requests are rejected; `0` preserves the explicit `Source`
/// policy instead of being mistaken for a target-format default.
pub fn pipeline_settings_from_legacy_options(
    options: &crate::convert::formats::ConversionOptions,
) -> ConversionResult<PipelineSettings> {
    use tonepoet_pipeline::enums as pe;

    // settings-sentinel-allow: legacy bridge constructs default then overrides from ConversionOptions
    let mut settings = PipelineSettings::default();
    settings.target_format = main_audio_format_to_planner(options.output_format);
    settings.force_encode = options.force_encode || options.reencode_flac;
    settings.metadata.transfer_tags = options.preserve_metadata;
    settings.metadata.preserve_artwork = options.preserve_metadata;
    settings.target_sample_rate =
        sample_rate_target_for_format(&settings.target_format, options.target_sample_rate)?;
    settings.target_bit_depth = bit_depth_target(options.target_bit_depth)?;
    if let Some(dither) = options.dither_type {
        settings.dither_type = match dither {
            crate::convert::simple_wizard::DitherType::None => pe::DitherType::None,
            crate::convert::simple_wizard::DitherType::TPDF => pe::DitherType::Tpdf,
            crate::convert::simple_wizard::DitherType::Shibata => pe::DitherType::Shibata,
            crate::convert::simple_wizard::DitherType::LowShibata => pe::DitherType::LowShibata,
            crate::convert::simple_wizard::DitherType::HighShibata => pe::DitherType::HighShibata,
            crate::convert::simple_wizard::DitherType::Gesemann => pe::DitherType::Gesemann,
            crate::convert::simple_wizard::DitherType::Lipshitz => pe::DitherType::Lipshitz,
            crate::convert::simple_wizard::DitherType::FWeighted => pe::DitherType::FWeighted,
            crate::convert::simple_wizard::DitherType::ModifiedEWeighted => {
                pe::DitherType::ModifiedEWeighted
            }
            crate::convert::simple_wizard::DitherType::ImprovedEWeighted => {
                pe::DitherType::ImprovedEWeighted
            }
            crate::convert::simple_wizard::DitherType::SloppedTPDF => pe::DitherType::SlopedTpdf,
        };
    }
    if options.calculate_replaygain {
        settings.replay_gain.mode = options.replaygain_mode.as_ref().map(|mode| match mode {
            crate::convert::simple_wizard::ReplayGainMode::Album => pe::ReplayGainMode::Album,
            crate::convert::simple_wizard::ReplayGainMode::Track => pe::ReplayGainMode::Track,
            crate::convert::simple_wizard::ReplayGainMode::Both => pe::ReplayGainMode::Both,
        });
    }
    apply_legacy_resampler_defaults(&mut settings);
    settings.validate().map_err(|error| {
        ConversionError::ValidationError(format!(
            "invalid legacy conversion settings: {error}"
        ))
    })?;
    Ok(settings)
}

// settings-sentinel-allow: legacy bridge constructs default then overrides from ConversionItem
fn legacy_pipeline_settings_for_item(item: &ConversionItem) -> ConversionResult<PipelineSettings> {
    let mut settings = PipelineSettings::default();
    settings.target_format = main_audio_format_to_planner(item.output_format);
    settings.force_encode = item.options.reencode_flac;
    settings.metadata.transfer_tags = item.options.preserve_metadata;
    settings.metadata.preserve_artwork = item.options.preserve_metadata;
    settings.metadata.store_source_audio_md5 = item.options.preserve_metadata
        && item
            .options
            .original_settings
            .as_ref()
            .and_then(|settings| settings.store_md5)
            .unwrap_or(false);
    settings.replay_gain.mode = replaygain_mode(item);
    settings.verification.verify_after_encode = item
        .options
        .original_settings
        .as_ref()
        .and_then(|settings| settings.verify_encoding)
        .unwrap_or(false);
    settings.verification.prefer_native_flac_verify = true;
    settings.preferred_tool = preferred_tool_from_option(item.options.preferred_backend);
    settings.resample_quality = resample_quality(item.options.resample_quality);
    settings.nyquist_transition = item
        .options
        .nyquist_transition
        .map(nyquist_transition)
        .or_else(|| {
            item.options
                .original_settings
                .as_ref()
                .and_then(|settings| settings.nyquist_transition)
                .map(backend_nyquist_transition)
        })
        .unwrap_or(NyquistTransition::Gentle);
    settings.ssrc.force = matches!(settings.nyquist_transition, NyquistTransition::BrickWall);

    settings.ssrc.insane_mode = item
        .options
        .ssrc_insane_mode
        .or_else(|| {
            item.options
                .original_settings
                .as_ref()
                .and_then(|settings| settings.ssrc_insane_mode)
        })
        .unwrap_or(false);
    if settings.ssrc.insane_mode {
        settings.ssrc.profile = Some(SsrcProfile::Insane);
    }
    settings.dither_type = item
        .options
        .dither_type
        .map(dither_type)
        .or_else(|| {
            item.options
                .original_settings
                .as_ref()
                .and_then(|settings| settings.dither_type)
                .map(backend_dither_type)
        })
        .unwrap_or(PlannerDitherType::None);
    settings.target_sample_rate = sample_rate_target_for_format(
        &settings.target_format,
        item.options
            .target_sample_rate
            .or_else(|| item.options.original_settings.as_ref().and_then(|s| s.sample_rate)),
    )?;
    settings.target_bit_depth = bit_depth_target(
        item.options
            .target_bit_depth
            .map(u32::from)
            .or_else(|| item.options.original_settings.as_ref().and_then(|s| s.bit_depth)),
    )?;

    apply_quality_settings(&mut settings, &item.options.quality, item)?;
    apply_explicit_pipeline_defaults(&mut settings, item);
    settings
        .validate()
        .map_err(|err| ConversionError::ValidationError(format!("invalid pipeline settings: {err}")))?;
    Ok(settings)
}

fn main_audio_format_to_planner(format: crate::convert::AudioFormat) -> PlannerAudioFormat {
    match format {
        crate::convert::AudioFormat::Flac => PlannerAudioFormat::Flac,
        crate::convert::AudioFormat::Wav => PlannerAudioFormat::Wav,
        crate::convert::AudioFormat::Aiff => PlannerAudioFormat::Aiff,
        crate::convert::AudioFormat::WavPack => PlannerAudioFormat::WavPack,
        crate::convert::AudioFormat::Mp3 => PlannerAudioFormat::Mp3,
        crate::convert::AudioFormat::Aac => PlannerAudioFormat::Aac,
        crate::convert::AudioFormat::Opus => PlannerAudioFormat::Opus,
        crate::convert::AudioFormat::Alac => PlannerAudioFormat::Alac,
        crate::convert::AudioFormat::Dsf => PlannerAudioFormat::Dsf,
        crate::convert::AudioFormat::Dff => PlannerAudioFormat::Dff,
        crate::convert::AudioFormat::Dts => PlannerAudioFormat::Dts,
        crate::convert::AudioFormat::Ac3 => PlannerAudioFormat::Ac3,
        // Decode-only source formats are never output targets; default to FLAC
        // like the pre-existing Ape arm.
        crate::convert::AudioFormat::Ape
        | crate::convert::AudioFormat::Musepack
        | crate::convert::AudioFormat::Shorten
        | crate::convert::AudioFormat::Ogg
        | crate::convert::AudioFormat::Tta => PlannerAudioFormat::Flac,
        crate::convert::AudioFormat::Lpcm => PlannerAudioFormat::Wav, // LPCM maps to WAV container
    }
}

fn apply_quality_settings(settings: &mut PipelineSettings, quality: &QualitySettings, item: &ConversionItem) -> ConversionResult<()> {
    if let Some(original) = item.options.original_settings.as_ref() {
        if let Some(level) = original.compression_level {
            settings.flac.compression_level = level.min(8);
        }
        if let Some(rate) = original.sample_rate {
            settings.target_sample_rate = sample_rate_target_for_format(&settings.target_format, Some(rate))?;
        }
        if let Some(depth) = original.bit_depth {
            settings.target_bit_depth = bit_depth_target(Some(depth))?;
        }
        if let Some(mode) = original.mp3_mode {
            settings.mp3.mode = match mode {
                tonepoet_backend::Mp3Mode::Cbr => PlannerMp3Mode::Cbr,
                tonepoet_backend::Mp3Mode::Vbr => PlannerMp3Mode::Vbr,
                tonepoet_backend::Mp3Mode::Abr => PlannerMp3Mode::Abr,
            };
        }
        if let Some(bitrate) = original.mp3_bitrate {
            settings.mp3.bitrate_kbps = bitrate;
        }
        if let Some(quality) = original.mp3_quality {
            settings.mp3.vbr_quality = quality.min(9);
        }
        if let Some(profile) = original.aac_profile {
            settings.aac.profile = backend_aac_profile(profile);
        }
        if let Some(content_type) = original.opus_content_type {
            settings.opus.content_type = match content_type {
                tonepoet_backend::OpusContentType::Music => OpusContentType::Music,
                tonepoet_backend::OpusContentType::Speech => OpusContentType::Speech,
                tonepoet_backend::OpusContentType::Auto => OpusContentType::Auto,
            };
        }
    }

    match quality {
        QualitySettings::Flac { compression_level } => {
            settings.flac.compression_level = (*compression_level).min(8);
        }
        QualitySettings::Wav { bit_depth, sample_rate }
        | QualitySettings::Aiff { bit_depth, sample_rate } => {
            settings.target_bit_depth = bit_depth_target((*bit_depth).map(u32::from))?;
            settings.target_sample_rate =
                sample_rate_target_for_format(&settings.target_format, *sample_rate)?;
        }
        QualitySettings::WavPack {
            compression_mode,
            hybrid_mode,
            correction_file,
        } => {
            settings.wavpack.mode = match compression_mode {
                WavPackMode::Fast => PlannerWavPackMode::Fast,
                WavPackMode::Normal => PlannerWavPackMode::Normal,
                WavPackMode::High => PlannerWavPackMode::High,
                WavPackMode::VeryHigh => PlannerWavPackMode::VeryHigh,
            };
            settings.wavpack.hybrid = *hybrid_mode;
            // wavpack.hybrid_bitrate_kbps is not carried by QualitySettings;
            // keep the planner default (set during PipelineSettings::default()).
            settings.wavpack.correction_file = *correction_file;
        }
        QualitySettings::Mp3 { bitrate_mode, quality } => match bitrate_mode {
            Mp3BitrateMode::Cbr { bitrate } => {
                settings.mp3.mode = PlannerMp3Mode::Cbr;
                settings.mp3.bitrate_kbps = *bitrate;
            }
            Mp3BitrateMode::Abr { bitrate } => {
                settings.mp3.mode = PlannerMp3Mode::Abr;
                settings.mp3.bitrate_kbps = *bitrate;
            }
            Mp3BitrateMode::Vbr { quality: vbr_quality } => {
                settings.mp3.mode = PlannerMp3Mode::Vbr;
                settings.mp3.vbr_quality = (*vbr_quality).min(9);
                settings.mp3.bitrate_kbps = 320;
                let _ = quality;
            }
        },
        QualitySettings::Aac { bitrate, profile } => {
            settings.aac.bitrate_kbps = *bitrate;
            settings.aac.profile = match profile {
                AacProfile::Lc => PlannerAacProfile::LcAac,
                AacProfile::He => PlannerAacProfile::HeAac,
                AacProfile::HeV2 => PlannerAacProfile::HeAacV2,
            };
        }
        QualitySettings::Opus { bitrate, complexity } => {
            settings.opus.content_type = OpusContentType::Auto;
            settings.opus.bitrate_kbps = *bitrate;
            settings.opus.complexity = (*complexity).min(10);
        }
        QualitySettings::Alac => {}
    }

    Ok(())
}


/// Apply planner fields that are not represented by `ConversionOptions`.
///
/// Compatibility-only legacy projection helper. Normal CLI/TUI handoff must
/// attach a complete `PipelineRequest` or call `attach_full_pipeline_settings`.
/// This function assigns every remaining `PipelineSettings` field deliberately
/// instead of relying on hidden planner defaults when migration tooling still
/// needs legacy `ConversionOptions` support.
fn apply_explicit_pipeline_defaults(settings: &mut PipelineSettings, item: &ConversionItem) {
    settings.force_encode = item
        .options
        .original_settings
        .as_ref()
        .and_then(|original| original.reencode_flac)
        .unwrap_or(item.options.reencode_flac);

    settings.flac.verify = settings.verification.verify_after_encode
        && matches!(settings.target_format, PlannerAudioFormat::Flac)
        && settings.verification.prefer_native_flac_verify;

    settings.ssrc.force = settings.ssrc.force
        || matches!(settings.nyquist_transition, NyquistTransition::BrickWall);

    if settings.ssrc.insane_mode {
        settings.ssrc.profile = Some(SsrcProfile::Insane);
    } else if settings.ssrc.force && settings.ssrc.profile.is_none() {
        settings.ssrc.profile = Some(SsrcProfile::High);
    }

    // The legacy options struct has no public DSD tuning controls. Assign the
    // complete planner DSD struct explicitly so the default is visible and
    // reproducible. Advanced callers retain full DSD control by attaching a
    // prebuilt PipelineRequest, which this builder returns unchanged.
    settings.dsd = DsdSettings::default();
    if matches!(settings.target_format, PlannerAudioFormat::Dsf | PlannerAudioFormat::Dff) {
        settings.target_bit_depth = BitDepthTarget::Source;
        if !matches!(settings.target_sample_rate, RateTarget::Dsd(_) | RateTarget::Source) {
            settings.target_sample_rate = RateTarget::Source;
        }
    }

    apply_legacy_resampler_defaults(settings);

    settings.replay_gain.prevent_clipping = true;
}

/// Make the lossy legacy bridge explicit for resampler controls that legacy
/// `ConversionOptions`/`QualitySettings` cannot express.
///
/// Broad resampling intent is mapped elsewhere via `resample_quality` and
/// `nyquist_transition`. These field-level SoX/SoXR overrides are available
/// only on full `PipelineSettings`, so the compatibility builder deliberately
/// leaves each override at its planner default instead of silently inheriting an
/// implicit value. Full-fidelity callers must attach a complete
/// `PipelineSettings`/`PipelineRequest`, which bypasses this legacy projection.
fn apply_legacy_resampler_defaults(settings: &mut PipelineSettings) {
    // sox_resampler.chebyshev has no legacy source field.
    settings.sox_resampler.chebyshev = false;
    // sox_resampler.bandwidth_pct has no legacy source field.
    settings.sox_resampler.bandwidth_pct = None;
    // sox_resampler.phase has no legacy source field.
    settings.sox_resampler.phase = None;
    // sox_resampler.allow_aliasing has no legacy source field.
    settings.sox_resampler.allow_aliasing = false;
    // sox_resampler.sinc_taps has no legacy source field.
    settings.sox_resampler.sinc_taps = None;
    // sox_resampler.sinc_attenuation_db has no legacy source field.
    settings.sox_resampler.sinc_attenuation_db = None;
    // sox_resampler.sinc_passband_hz has no legacy source field.
    settings.sox_resampler.sinc_passband_hz = None;
    // sox_resampler.sinc_transition_hz has no legacy source field.
    settings.sox_resampler.sinc_transition_hz = None;
    // sox_resampler.sinc_kaiser_beta has no legacy source field.
    settings.sox_resampler.sinc_kaiser_beta = None;
    // sox_resampler.sinc_phase has no legacy source field.
    settings.sox_resampler.sinc_phase = None;

    // soxr_resampler.chebyshev has no legacy source field.
    settings.soxr_resampler.chebyshev = false;
    // soxr_resampler.cutoff has no legacy source field.
    settings.soxr_resampler.cutoff = None;
    // soxr_resampler.phase has no legacy source field.
    settings.soxr_resampler.phase = None;
}

fn sample_rate_target_for_format(
    format: &PlannerAudioFormat,
    value: Option<u32>,
) -> ConversionResult<RateTarget> {
    match value {
        Some(0) | None => Ok(RateTarget::Source),
        Some(hz) if matches!(format, PlannerAudioFormat::Dsf | PlannerAudioFormat::Dff) => {
            DsdRate::from_hz(hz).map(RateTarget::Dsd).ok_or_else(|| {
                ConversionError::ValidationError(format!(
                    "DSD target format requires a DSD sample rate; got {hz} Hz"
                ))
            })
        }
        Some(hz) => Ok(RateTarget::PcmHz(hz)),
    }
}

fn bit_depth_target(value: Option<u32>) -> ConversionResult<BitDepthTarget> {
    match value {
        Some(0) | None => Ok(BitDepthTarget::Source),
        Some(8) => Ok(BitDepthTarget::Pcm(PcmBitDepth::Int8)),
        Some(16) => Ok(BitDepthTarget::Pcm(PcmBitDepth::Int16)),
        Some(24) => Ok(BitDepthTarget::Pcm(PcmBitDepth::Int24)),
        Some(32) => Ok(BitDepthTarget::Pcm(PcmBitDepth::Int32)),
        Some(320) => Ok(BitDepthTarget::Pcm(PcmBitDepth::Float32)),
        Some(640) => Ok(BitDepthTarget::Pcm(PcmBitDepth::Float64)),
        Some(value) => Err(ConversionError::ValidationError(format!(
            "unsupported target bit depth {value}; expected 8, 16, 24, 32, 320 (32f), 640 (64f), or 0/source"
        ))),
    }
}

fn resample_quality(value: Option<u8>) -> ResampleQuality {
    match value.unwrap_or(2) {
        0 => ResampleQuality::Low,
        1 => ResampleQuality::Medium,
        2 => ResampleQuality::High,
        3 => ResampleQuality::VeryHigh,
        _ => ResampleQuality::Ultra,
    }
}

fn nyquist_transition(value: crate::convert::simple_wizard::NyquistTransition) -> NyquistTransition {
    match value {
        crate::convert::simple_wizard::NyquistTransition::Gentle => NyquistTransition::Gentle,
        crate::convert::simple_wizard::NyquistTransition::Steep => NyquistTransition::Steep,
        crate::convert::simple_wizard::NyquistTransition::BrickWall => NyquistTransition::BrickWall,
    }
}

fn backend_nyquist_transition(value: tonepoet_backend::NyquistTransition) -> NyquistTransition {
    match value {
        tonepoet_backend::NyquistTransition::Gentle => NyquistTransition::Gentle,
        tonepoet_backend::NyquistTransition::Medium => NyquistTransition::Medium,
        tonepoet_backend::NyquistTransition::Sharp | tonepoet_backend::NyquistTransition::Steep => NyquistTransition::Steep,
        tonepoet_backend::NyquistTransition::BrickWall => NyquistTransition::BrickWall,
    }
}

fn dither_type(value: crate::convert::simple_wizard::DitherType) -> PlannerDitherType {
    match value {
        crate::convert::simple_wizard::DitherType::None => PlannerDitherType::None,
        crate::convert::simple_wizard::DitherType::TPDF => PlannerDitherType::Tpdf,
        crate::convert::simple_wizard::DitherType::SloppedTPDF => PlannerDitherType::SlopedTpdf,
        crate::convert::simple_wizard::DitherType::Shibata => PlannerDitherType::Shibata,
        crate::convert::simple_wizard::DitherType::Lipshitz => PlannerDitherType::Lipshitz,
        crate::convert::simple_wizard::DitherType::FWeighted => PlannerDitherType::FWeighted,
        crate::convert::simple_wizard::DitherType::ModifiedEWeighted => PlannerDitherType::ModifiedEWeighted,
        crate::convert::simple_wizard::DitherType::ImprovedEWeighted => PlannerDitherType::ImprovedEWeighted,
        crate::convert::simple_wizard::DitherType::Gesemann => PlannerDitherType::Gesemann,
        crate::convert::simple_wizard::DitherType::LowShibata => PlannerDitherType::LowShibata,
        crate::convert::simple_wizard::DitherType::HighShibata => PlannerDitherType::HighShibata,
    }
}

fn backend_dither_type(value: tonepoet_backend::DitherType) -> PlannerDitherType {
    match value {
        tonepoet_backend::DitherType::None => PlannerDitherType::None,
        tonepoet_backend::DitherType::Tpdf => PlannerDitherType::Tpdf,
        tonepoet_backend::DitherType::Shibata => PlannerDitherType::Shibata,
        tonepoet_backend::DitherType::LowShibata => PlannerDitherType::LowShibata,
        tonepoet_backend::DitherType::HighShibata => PlannerDitherType::HighShibata,
        tonepoet_backend::DitherType::FShaped => PlannerDitherType::FWeighted,
        tonepoet_backend::DitherType::ModifiedE => PlannerDitherType::ModifiedEWeighted,
        tonepoet_backend::DitherType::ImprovedE => PlannerDitherType::ImprovedEWeighted,
        tonepoet_backend::DitherType::Gesemann => PlannerDitherType::Gesemann,
    }
}

fn backend_aac_profile(value: tonepoet_backend::AacProfile) -> PlannerAacProfile {
    match value {
        tonepoet_backend::AacProfile::LcAac => PlannerAacProfile::LcAac,
        tonepoet_backend::AacProfile::HeAac => PlannerAacProfile::HeAac,
        tonepoet_backend::AacProfile::HeAacV2 => PlannerAacProfile::HeAacV2,
    }
}

fn replaygain_mode(item: &ConversionItem) -> Option<PlannerReplayGainMode> {
    if !item.options.calculate_replaygain {
        return None;
    }
    item.options
        .replaygain_mode
        .as_ref()
        .map(|mode| match mode {
            crate::convert::simple_wizard::ReplayGainMode::Track => PlannerReplayGainMode::Track,
            crate::convert::simple_wizard::ReplayGainMode::Album => PlannerReplayGainMode::Album,
            crate::convert::simple_wizard::ReplayGainMode::Both => PlannerReplayGainMode::Both,
        })
        .or_else(|| {
            item.options
                .original_settings
                .as_ref()
                .and_then(|settings| settings.replaygain_mode)
                .map(|mode| match mode {
                    tonepoet_backend::ReplayGainMode::Track => PlannerReplayGainMode::Track,
                    tonepoet_backend::ReplayGainMode::Album => PlannerReplayGainMode::Album,
                    tonepoet_backend::ReplayGainMode::Both => PlannerReplayGainMode::Both,
                })
        })
        .or(Some(PlannerReplayGainMode::Album))
}

fn preferred_tool_from_option(value: Option<tonepoet_backend::Backend>) -> PreferredTool {
    match value {
        Some(tonepoet_backend::Backend::FFmpeg) => PreferredTool::Ffmpeg,
        Some(tonepoet_backend::Backend::Sox) => PreferredTool::Sox,
        None => PreferredTool::Auto,
    }
}

fn cue_policy_for_input_path(path: &Path) -> CueSidecarPolicy {
    if has_extension(path, "cue") {
        // A queued `.cue` is the authoritative control file for STRUCTURE and
        // image resolution. Metadata authority is subtler: the metadata editor
        // writes corrections to the referenced image (flat tags + regenerated
        // embedded CUESHEET), so sidecar resolution upgrades to the image's
        // embedded sheet when it structurally matches — see
        // try_upgrade_sidecar_to_embedded_image_cue in the CUE materializer.
        CueSidecarPolicy::SidecarOnly
    } else if is_cue_capable_path(path) {
        CueSidecarPolicy::PreferEmbedded
    } else {
        CueSidecarPolicy::IgnoreCue
    }
}

fn is_cue_capable_path(path: &Path) -> bool {
    const AUDIO_IMAGE_EXTENSIONS: &[&str] = &[
        "flac", "wav", "wave", "aiff", "aif", "aifc", "wv", "mp3", "m4a", "mp4", "aac", "opus",
        "ogg", "ape", "w64", "rf64",
    ];
    path.extension()
        .and_then(|value| value.to_str())
        .map(|ext| AUDIO_IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn has_extension(path: &Path, expected: &str) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|ext| ext.eq_ignore_ascii_case(expected))
        .unwrap_or(false)
}

/// Expand a leading `~` or `~/` to the user's home directory.
pub(crate) fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod cue_sidecar_override_request_tests {
    use super::*;
    use crate::convert::queue::ConversionItem;

    fn item_with_settings(path: &str) -> ConversionItem {
        let mut item = ConversionItem::default();
        item.id = "cue-sidecar-test".to_string();
        item.input_path = PathBuf::from(path);
        item.options.pipeline_settings = Some(PipelineSettings::default());
        item.pipeline_settings = Some(PipelineSettings::default());
        item
    }

    #[test]
    fn conversion_item_override_is_copied_to_pipeline_request_source() {
        let mut item = item_with_settings("/tmp/album/01.flac");
        item.cue_sidecar_override = Some(CueSidecarPolicy::EmbeddedOnly);

        let request = build_pipeline_request(&item).expect("build request with override");

        assert_eq!(request.source.cue_sidecar, CueSidecarPolicy::EmbeddedOnly);
    }

    #[test]
    fn explicit_individual_audio_without_override_uses_default_prefer_embedded_policy() {
        let item = item_with_settings("/tmp/album/01.flac");

        let request = build_pipeline_request(&item).expect("build request without override");

        assert_eq!(request.source.cue_sidecar, CueSidecarPolicy::PreferEmbedded);
    }

    #[test]
    fn prebuilt_pipeline_request_still_honors_item_override() {
        let mut item = item_with_settings("/tmp/album/01.flac");
        let mut prebuilt = build_pipeline_request(&item).expect("build base prebuilt request");
        prebuilt.source.cue_sidecar = CueSidecarPolicy::PreferSidecar;
        item.pipeline_request = Some(prebuilt);
        item.cue_sidecar_override = Some(CueSidecarPolicy::EmbeddedOnly);

        let request = build_pipeline_request(&item).expect("build request from prebuilt path");

        assert_eq!(request.source.cue_sidecar, CueSidecarPolicy::EmbeddedOnly);
    }

    #[test]
    fn bit_depth_target_maps_float64_without_falling_back_to_source() {
        assert_eq!(
            bit_depth_target(Some(640)).expect("64f mapping"),
            BitDepthTarget::Pcm(PcmBitDepth::Float64)
        );
    }

    #[test]
    fn bit_depth_target_rejects_unmappable_numeric_values() {
        let error = bit_depth_target(Some(20)).expect_err("20 has no direct planner target");
        assert!(error.to_string().contains("unsupported target bit depth 20"));
    }


    #[test]
    fn legacy_cli_projection_preserves_unset_and_explicit_source_depth() {
        let mut options = crate::convert::formats::ConversionOptions::default();
        options.output_format = crate::convert::AudioFormat::Flac;

        let unset = pipeline_settings_from_legacy_options(&options)
            .expect("unset CLI depth projection");
        assert_eq!(unset.target_bit_depth, BitDepthTarget::Source);

        options.target_bit_depth = Some(0);
        let explicit = pipeline_settings_from_legacy_options(&options)
            .expect("explicit Source CLI depth projection");
        assert_eq!(explicit.target_bit_depth, BitDepthTarget::Source);
    }

    #[test]
    fn legacy_cli_projection_rejects_unmappable_numeric_depth() {
        let mut options = crate::convert::formats::ConversionOptions::default();
        options.output_format = crate::convert::AudioFormat::Wav;
        options.target_bit_depth = Some(20);

        let error = pipeline_settings_from_legacy_options(&options)
            .expect_err("unmappable CLI depth must fail closed");
        assert!(error.to_string().contains("unsupported target bit depth 20"));
    }

}
