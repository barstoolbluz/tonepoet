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
    CueSidecarPolicy, FailurePolicy, LogPolicy, NamingCollisionPolicy, NamingPolicy,
    OverwritePolicy, PipelineRequest, PublishPolicy, SecretString, SourceOptions, StagePolicy,
    StageRequirement, TrackSelection,
};
use crate::convert::{ConversionError, ConversionItem, ConversionResult};

pub fn build_pipeline_request(item: &ConversionItem) -> ConversionResult<PipelineRequest> {
    // Return a prebuilt PipelineRequest with full PipelineSettings — bypass all builders.
    if let Some(request) = item.pipeline_request.clone() {
        request.settings.validate().map_err(|err| {
            ConversionError::ValidationError(format!("invalid prebuilt pipeline settings: {err}"))
        })?;
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

    let cue_policy = if is_cue_capable_path(&item.input_path) {
        CueSidecarPolicy::PreferSidecar
    } else {
        CueSidecarPolicy::IgnoreCue
    };

    Ok(PipelineRequest {
        job_id: format!("job-{}", item.id),
        item_id: item.id.clone(),
        container: item.input_path.clone(),
        source: SourceOptions {
            archive_password: item
                .archive_password
                .as_ref()
                .map(|password| SecretString::new(password.clone())),
            sacd_area: None,
            cue_sidecar: cue_policy,
            track_selection: TrackSelection::All,
        },
        settings,
        worker_count: None,
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
    })
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

/// Build PipelineSettings from legacy ConversionOptions fields.
/// Used as a fallback when pipeline_settings is None (CLI path).
// settings-sentinel-allow: legacy bridge constructs default then overrides from ConversionOptions
pub fn pipeline_settings_from_legacy_options(options: &crate::convert::formats::ConversionOptions) -> PipelineSettings {
    use tonepoet_pipeline::enums as pe;
    let mut settings = PipelineSettings::default();
    settings.target_format = main_audio_format_to_planner(options.output_format);
    settings.force_encode = options.reencode_flac;
    settings.metadata.transfer_tags = options.preserve_metadata;
    settings.metadata.preserve_artwork = options.preserve_metadata;
    if let Some(rate) = options.target_sample_rate {
        if rate >= 2_822_400 {
            if let Some(dsd) = pe::DsdRate::from_hz(rate) {
                settings.target_sample_rate = pe::RateTarget::Dsd(dsd);
            }
        } else {
            settings.target_sample_rate = pe::RateTarget::PcmHz(rate);
        }
    }
    if let Some(depth) = options.target_bit_depth {
        let pcm = match depth {
            16 => pe::PcmBitDepth::Int16,
            24 => pe::PcmBitDepth::Int24,
            32 => pe::PcmBitDepth::Int32,
            320 => pe::PcmBitDepth::Float32,
            640 => pe::PcmBitDepth::Float64,
            _ => pe::PcmBitDepth::Int24,
        };
        settings.target_bit_depth = pe::BitDepthTarget::Pcm(pcm);
    }
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
            crate::convert::simple_wizard::DitherType::ModifiedEWeighted => pe::DitherType::ModifiedEWeighted,
            crate::convert::simple_wizard::DitherType::ImprovedEWeighted => pe::DitherType::ImprovedEWeighted,
            crate::convert::simple_wizard::DitherType::SloppedTPDF => pe::DitherType::SlopedTpdf,
        };
    }
    if options.calculate_replaygain {
        settings.replay_gain.mode = options.replaygain_mode.as_ref().map(|mode| {
            match mode {
                crate::convert::simple_wizard::ReplayGainMode::Album => pe::ReplayGainMode::Album,
                crate::convert::simple_wizard::ReplayGainMode::Track => pe::ReplayGainMode::Track,
                crate::convert::simple_wizard::ReplayGainMode::Both => pe::ReplayGainMode::Both,
            }
        });
    }
    settings
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
    settings.ssrc.two_pass = true;
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
    );

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
        crate::convert::AudioFormat::Ape => PlannerAudioFormat::Flac, // Ape is decode-only; default target is FLAC
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
            settings.target_bit_depth = bit_depth_target(Some(depth));
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
            settings.target_bit_depth = bit_depth_target(Some(u32::from(*bit_depth)));
            settings.target_sample_rate = sample_rate_target_for_format(&settings.target_format, Some(*sample_rate))?;
        }
        QualitySettings::WavPack { compression_mode, .. } => {
            settings.wavpack.mode = match compression_mode {
                WavPackMode::Fast => PlannerWavPackMode::Fast,
                WavPackMode::Normal => PlannerWavPackMode::Normal,
                WavPackMode::High => PlannerWavPackMode::High,
                WavPackMode::VeryHigh => PlannerWavPackMode::VeryHigh,
            };
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

    settings.aac.prefer_fdk_for_he = matches!(
        settings.aac.profile,
        PlannerAacProfile::HeAac | PlannerAacProfile::HeAacV2
    );

    settings.ssrc.force = settings.ssrc.force
        || matches!(settings.nyquist_transition, NyquistTransition::BrickWall);
    settings.ssrc.two_pass = true;
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

    settings.replay_gain.prevent_clipping = true;
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

fn bit_depth_target(value: Option<u32>) -> BitDepthTarget {
    match value {
        Some(0) | None => BitDepthTarget::Source,
        Some(8) => BitDepthTarget::Pcm(PcmBitDepth::Int8),
        Some(16) => BitDepthTarget::Pcm(PcmBitDepth::Int16),
        Some(24) => BitDepthTarget::Pcm(PcmBitDepth::Int24),
        Some(32) => BitDepthTarget::Pcm(PcmBitDepth::Int32),
        Some(320) => BitDepthTarget::Pcm(PcmBitDepth::Float32),
        Some(_) => BitDepthTarget::Source,
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
        tonepoet_backend::AacProfile::LdAac => PlannerAacProfile::LdAac,
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

fn is_cue_capable_path(path: &Path) -> bool {
    const AUDIO_IMAGE_EXTENSIONS: &[&str] = &[
        "flac", "wav", "wave", "aiff", "aif", "aifc", "wv", "mp3", "m4a", "mp4", "aac", "opus",
        "ogg", "ape", "w64", "rf64", "cue",
    ];
    path.extension()
        .and_then(|value| value.to_str())
        .map(|ext| AUDIO_IMAGE_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Expand a leading `~` or `~/` to the user's home directory.
fn expand_tilde(path: &Path) -> PathBuf {
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
