//! Convert screen actions: build ConversionOptions from pills, add to queue, start conversion

use tokio::sync::mpsc;

use crate::config::TonepoetConfig;
use crate::convert::formats::{
    AacProfile, AudioFormat, ConversionOptions, Mp3BitrateMode, QualitySettings, WavPackMode,
};
use crate::convert::simple_wizard::ReplayGainMode;
use tonepoet_pipeline::enums as pipeline_enums;
use tonepoet_pipeline::PipelineSettings;
use crate::convert::{ConversionStatus, LifecycleEvent, ProgressUpdate};

use super::app::*;
use super::message::AppMessage;


fn progress_to_app_message(update: ProgressUpdate) -> AppMessage {
    AppMessage::ConversionProgress {
        item_id: update.item_id,
        track_index: update.track_index,
        track_epoch: update.track_epoch,
        progress: update.progress,
        status: update.status,
    }
}

fn lifecycle_to_app_message(event: LifecycleEvent) -> AppMessage {
    match event {
        LifecycleEvent::ClearTrack {
            item_id,
            track_index,
            track_epoch,
        } => AppMessage::ClearTrackProgress {
            item_id,
            track_index,
            track_epoch,
        },
        LifecycleEvent::ItemTerminal {
            item_id,
            progress,
            status,
        } => AppMessage::ConversionProgress {
            item_id,
            track_index: None,
            track_epoch: None,
            progress,
            status,
        },
    }
}

async fn forward_conversion_events(
    mut progress_rx: tokio::sync::broadcast::Receiver<ProgressUpdate>,
    mut lifecycle_rx: mpsc::UnboundedReceiver<LifecycleEvent>,
    ui_tx: mpsc::Sender<AppMessage>,
) {
    let mut progress_closed = false;
    let mut lifecycle_closed = false;

    while !progress_closed || !lifecycle_closed {
        tokio::select! {
            lifecycle = lifecycle_rx.recv(), if !lifecycle_closed => {
                match lifecycle {
                    Some(event) => {
                        let _ = ui_tx.try_send(lifecycle_to_app_message(event));
                    }
                    None => lifecycle_closed = true,
                }
            }
            progress = progress_rx.recv(), if !progress_closed => {
                match progress {
                    Ok(update) => {
                        let _ = ui_tx.try_send(progress_to_app_message(update));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => progress_closed = true,
                }
            }
            else => break,
        }
    }
}

/// Build ConversionOptions from current pill and pane state.
///
/// Compatibility entry point for older call sites. It never panics. New
/// user-facing code should call `try_pills_to_options` so validation failures
/// can be rendered as status messages instead of silently falling back.
pub fn pills_to_options(
    format: &FormatState,
    output_opts: &OutputOptionsState,
    config: &TonepoetConfig,
) -> ConversionOptions {
    match try_pills_to_options(format, output_opts, config) {
        Ok(options) => options,
        Err(err) => {
            log::error!("TUI format state produced invalid pipeline settings: {err}");
            let mut clamped = format.clone();
            clamped.apply_format_constraints();
            match try_pills_to_options(&clamped, output_opts, config) {
                Ok(options) => options,
                Err(clamped_err) => {
                    log::error!("clamped TUI format state still invalid: {clamped_err}");
                    fallback_options(output_opts, config)
                }
            }
        }
    }
}


fn fallback_options(output_opts: &OutputOptionsState, config: &TonepoetConfig) -> ConversionOptions {
    let format = FormatState::new();
    let mut options = ConversionOptions::default();
    options.output_format = *format.format.selected_value();
    options.target_sample_rate = (*format.sample_rate.selected_value() != crate::tui::app::SOURCE_SAMPLE_RATE_SENTINEL)
        .then_some(*format.sample_rate.selected_value());
    options.target_bit_depth = (!format.bit_depth.selected_value().is_source())
        .then_some(format.bit_depth.selected_value().to_backend_depth());
    options.dither_type = Some(*format.dither.selected_value());
    options.naming_template = Some(output_opts.filename_template.clone());
    options.folder_template = Some(output_opts.folder_template.clone());
    options.output_dir = output_opts.dest_path.clone();
    options.merge_to_single = matches!(*output_opts.merge.selected_value(), MergeMode::SingleImage);
    options.preserve_metadata = true;
    options.append_lineage_to_comment = config.conversion.append_lineage_to_comment;
    options.write_log_file = config.conversion.write_log_file;
    options.generate_cue_files = config.conversion.generate_cue_files;
    options.cue_generation_mode = config.conversion.cue_generation_mode.clone();
    options.force_encode = *output_opts.force_encode.selected_value();
    options.create_disc_subfolders = *output_opts.disc_subfolders.selected_value();
    options.actions = output_opts.actions.clone();
    if let Some(settings) = options.pipeline_settings.as_mut() {
        settings.force_encode = *output_opts.force_encode.selected_value();
    }
    options.pipeline_settings = format_state_to_pipeline_settings(&format).ok();
    if let Some(settings) = options.pipeline_settings.as_mut() {
        settings.force_encode = *output_opts.force_encode.selected_value();
    }
    // Keep the raw user template here. The conversion/request construction
    // boundary is responsible for applying `create_disc_subfolders` through
    // `ConversionOptions::effective_naming_template`, so every entrypoint uses
    // one canonical projection.
    options.naming_template = Some(output_opts.filename_template.clone());
    options
}

/// Build ConversionOptions and validate unified pipeline settings on the release path.
pub fn try_pills_to_options(
    format: &FormatState,
    output_opts: &OutputOptionsState,
    config: &TonepoetConfig,
) -> Result<ConversionOptions, String> {
    let output_format = *format.format.selected_value();
    let target_sample_rate = *format.sample_rate.selected_value();
    let bit_depth = *format.bit_depth.selected_value();
    let dither = *format.dither.selected_value();
    let rg = *format.replaygain.selected_value();
    let merge = *output_opts.merge.selected_value();
    let is_dsd = format.is_dsd_selected();
    let legacy_bit_depth_applies = matches!(
        output_format,
        AudioFormat::Flac | AudioFormat::Wav | AudioFormat::Aiff | AudioFormat::WavPack | AudioFormat::Alac
    );

    // Preserve source-relative policy in the legacy compatibility carrier.
    // The live TUI path attaches full PipelineSettings, but serialized or test
    // consumers must never see a guessed 24-bit depth or raw zero rate.
    let backend_depth = (!bit_depth.is_source()).then_some(bit_depth.to_backend_depth());
    let legacy_sample_rate = (target_sample_rate
        != crate::tui::app::SOURCE_SAMPLE_RATE_SENTINEL)
        .then_some(target_sample_rate);

    let quality = match output_format {
        AudioFormat::Flac => QualitySettings::Flac {
            compression_level: 5,
        },
        AudioFormat::Wav => QualitySettings::Wav {
            bit_depth: backend_depth.map(|depth| depth as u16),
            sample_rate: legacy_sample_rate,
        },
        AudioFormat::Aiff => QualitySettings::Aiff {
            bit_depth: backend_depth.map(|depth| depth as u16),
            sample_rate: legacy_sample_rate,
        },
        AudioFormat::WavPack => QualitySettings::WavPack {
            compression_mode: WavPackMode::Normal,
            hybrid_mode: false,
            correction_file: false,
        },
        AudioFormat::Mp3 => QualitySettings::Mp3 {
            bitrate_mode: Mp3BitrateMode::Vbr { quality: 2 },
            quality: 2,
        },
        AudioFormat::Aac => QualitySettings::Aac {
            bitrate: 256,
            profile: AacProfile::Lc,
        },
        AudioFormat::Opus => QualitySettings::Opus {
            bitrate: 128,
            complexity: 10,
        },
        AudioFormat::Alac => QualitySettings::Alac,
        AudioFormat::Dsf | AudioFormat::Dff => QualitySettings::Flac {
            compression_level: 0,
        },
        // Input-only / decode-only formats: fall back to FLAC defaults
        AudioFormat::Dts
        | AudioFormat::Ac3
        | AudioFormat::Ape
        | AudioFormat::Musepack
        | AudioFormat::Shorten
        | AudioFormat::Ogg
        | AudioFormat::Tta
        | AudioFormat::Lpcm => {
            QualitySettings::Flac { compression_level: 8 }
        }
    };

    let (calculate_replaygain, replaygain_mode) = if is_dsd {
        (false, None)
    } else {
        match rg {
            ReplayGainChoice::Album | ReplayGainChoice::AlbumIfMissing => (true, Some(ReplayGainMode::Album)),
            ReplayGainChoice::Track | ReplayGainChoice::TrackIfMissing => (true, Some(ReplayGainMode::Track)),
            ReplayGainChoice::Both | ReplayGainChoice::BothIfMissing => (true, Some(ReplayGainMode::Both)),
            ReplayGainChoice::Off => (false, None),
        }
    };

    // Keep legacy fields consistent with the unified settings. Hidden DSD and
    // lossy-codec rows do not leak stale bit-depth, dither, or ReplayGain values.
    let dither_type = if legacy_bit_depth_applies && !is_dsd {
        Some(dither)
    } else {
        None
    };
    let target_bit_depth = if legacy_bit_depth_applies && !is_dsd && !bit_depth.is_source() {
        backend_depth
    } else {
        None
    };

    let pipeline_settings = format_state_to_pipeline_settings(format)?;

    Ok(ConversionOptions {
        output_format,
        quality,
        target_sample_rate: (target_sample_rate != crate::tui::app::SOURCE_SAMPLE_RATE_SENTINEL)
            .then_some(target_sample_rate),
        target_bit_depth,
        dither_type,
        calculate_replaygain,
        replaygain_mode,
        // Store the raw editable template. `create_disc_subfolders` is applied
        // exactly once by `ConversionOptions::effective_naming_template` when
        // building the real PipelineRequest/NamingPolicy.
        naming_template: Some(output_opts.filename_template.clone()),
        folder_template: Some(output_opts.folder_template.clone()),
        output_dir: output_opts.dest_path.clone(),
        merge_to_single: matches!(merge, MergeMode::SingleImage),
        preserve_metadata: true,
        append_lineage_to_comment: config.conversion.append_lineage_to_comment,
        write_log_file: config.conversion.write_log_file,
        generate_cue_files: config.conversion.generate_cue_files,
        cue_generation_mode: config.conversion.cue_generation_mode.clone(),
        force_encode: *output_opts.force_encode.selected_value(),
        create_disc_subfolders: *output_opts.disc_subfolders.selected_value(),
        actions: output_opts.actions.clone(),
        pipeline_settings: Some({
            let mut settings = pipeline_settings;
            settings.force_encode = *output_opts.force_encode.selected_value();
            settings
        }),
        container_extension: if format.selected_container_index > 0 {
            Some(format.selected_extension().to_string())
        } else {
            None
        },
        container_ffmpeg_flags: format.selected_container().ffmpeg_flags
            .iter()
            .map(|s| s.to_string())
            .collect(),
        ..ConversionOptions::default()
    })
}

/// Build the unified pipeline settings from TUI format state.
///
/// This is the lossless handoff from dynamic TUI rows into command planning.
/// It validates on every build, including release builds.
pub fn format_state_to_pipeline_settings(format: &FormatState) -> Result<PipelineSettings, String> {
    if format.dsd_reference_controls_available() && !format.reference_target_confirmed {
        return Err(tonepoet_pipeline::reference_error_text(
            tonepoet_pipeline::ReferenceErrorCode::CanonicalTarget,
        )
        .to_string());
    }
    let container_ext = if format.selected_container_index > 0 {
        Some(format.selected_extension())
    } else {
        None
    };
    let target_format = map_audio_format(*format.format.selected_value(), container_ext);
    let selected_rate = *format.sample_rate.selected_value();
    let is_dsd = format.is_dsd_selected();

    let (target_sample_rate, target_bit_depth, dither_type, preferred_tool, nyquist_transition) =
        if is_dsd {
            let rate_target = if selected_rate == crate::tui::app::SOURCE_SAMPLE_RATE_SENTINEL {
                pipeline_enums::RateTarget::Source
            } else {
                pipeline_enums::RateTarget::Dsd(
                    pipeline_enums::DsdRate::from_hz(selected_rate)
                        .ok_or_else(|| format!("{} is not a supported DSD target rate", selected_rate))?,
                )
            };
            (
                rate_target,
                pipeline_enums::BitDepthTarget::Source,
                pipeline_enums::DitherType::None,
                pipeline_enums::PreferredTool::Sox,
                pipeline_enums::NyquistTransition::Gentle,
            )
        } else {
            let target_depth = match *format.bit_depth.selected_value() {
                BitDepthChoice::Source => pipeline_enums::BitDepthTarget::Source,
                BitDepthChoice::Int16 => pipeline_enums::BitDepthTarget::Pcm(pipeline_enums::PcmBitDepth::Int16),
                BitDepthChoice::Int24 => pipeline_enums::BitDepthTarget::Pcm(pipeline_enums::PcmBitDepth::Int24),
                BitDepthChoice::Int32 => pipeline_enums::BitDepthTarget::Pcm(pipeline_enums::PcmBitDepth::Int32),
                BitDepthChoice::Float32 => pipeline_enums::BitDepthTarget::Pcm(pipeline_enums::PcmBitDepth::Float32),
                BitDepthChoice::Float64 => pipeline_enums::BitDepthTarget::Pcm(pipeline_enums::PcmBitDepth::Float64),
            };
            let (tool, transition) = match *format.resampler.selected_value() {
                ResamplerChoice::None => (
                    pipeline_enums::PreferredTool::Auto,
                    pipeline_enums::NyquistTransition::Gentle,
                ),
                ResamplerChoice::Sox => (
                    pipeline_enums::PreferredTool::Sox,
                    pipeline_enums::NyquistTransition::Gentle,
                ),
                ResamplerChoice::Ssrc => (
                    pipeline_enums::PreferredTool::Ssrc,
                    pipeline_enums::NyquistTransition::BrickWall,
                ),
                ResamplerChoice::Soxr => (
                    pipeline_enums::PreferredTool::Ffmpeg,
                    pipeline_enums::NyquistTransition::Gentle,
                ),
            };
            (
                if selected_rate == crate::tui::app::SOURCE_SAMPLE_RATE_SENTINEL {
                    pipeline_enums::RateTarget::Source
                } else {
                    pipeline_enums::RateTarget::PcmHz(selected_rate)
                },
                target_depth,
                map_dither(*format.dither.selected_value()),
                tool,
                transition,
            )
        };

    let replay_gain_mode = if is_dsd {
        None
    } else {
        match *format.replaygain.selected_value() {
            ReplayGainChoice::Album | ReplayGainChoice::AlbumIfMissing => Some(pipeline_enums::ReplayGainMode::Album),
            ReplayGainChoice::Track | ReplayGainChoice::TrackIfMissing => Some(pipeline_enums::ReplayGainMode::Track),
            ReplayGainChoice::Both | ReplayGainChoice::BothIfMissing => Some(pipeline_enums::ReplayGainMode::Both),
            ReplayGainChoice::Off => None,
        }
    };

    // settings-sentinel-allow: sub-struct defaults are correct here — user-facing
    // settings (format, rate, depth, dither, resampler, RG) are set from pill state;
    // codec-specific sub-structs (flac, mp3, aac, etc.) use defaults until the TUI
    // exposes those settings.
    let mut dsd: tonepoet_pipeline::DsdSettings = Default::default();
    if format.dsd_to_pcm_gain_available() {
        if dsd.is_native_v2() && format.dsd_reference_controls_available() {
            dsd.from_dsd.pathway = *format.dsd_pathway.selected_value();
            dsd.from_dsd.profile = *format.dsd_profile.selected_value();
            match *format.dsd_gain_mode.selected_value() {
                DsdGainMode::Reference => {
                    dsd.from_dsd.gain_mode = tonepoet_pipeline::DsdSourceGainMode::Reference;
                    dsd.from_dsd.fixed_gain_db = None;
                }
                DsdGainMode::NativeLevel => {
                    dsd.from_dsd.gain_mode = tonepoet_pipeline::DsdSourceGainMode::NativeLevel;
                    dsd.from_dsd.fixed_gain_db = None;
                }
                DsdGainMode::NormalizePeak => {
                    dsd.from_dsd.gain_mode = tonepoet_pipeline::DsdSourceGainMode::NormalizePeak;
                    dsd.from_dsd.fixed_gain_db = None;
                    dsd.from_dsd.normalize_peak_target_dbfs = format.dsd_normalize_target_dbfs;
                }
                DsdGainMode::Fixed => {
                    dsd.from_dsd.gain_mode = tonepoet_pipeline::DsdSourceGainMode::Fixed;
                    dsd.from_dsd.fixed_gain_db = Some(format.dsd_gain_db);
                }
                DsdGainMode::Disabled | DsdGainMode::Auto => {
                    return Err("legacy DSD gain mode is unavailable after Reference promotion".to_string());
                }
            }
        } else {
            let (mode, margin, gain) = match *format.dsd_gain_mode.selected_value() {
                DsdGainMode::Disabled => (
                    tonepoet_pipeline::DsdToPcmGainMode::Disabled,
                    0.15,
                    None,
                ),
                DsdGainMode::Auto => (
                    tonepoet_pipeline::DsdToPcmGainMode::Auto,
                    (format.dsd_auto_gain_margin_db.0 as f64 / 1_000_000_000.0) as f32,
                    None,
                ),
                DsdGainMode::Fixed => (
                    tonepoet_pipeline::DsdToPcmGainMode::Manual,
                    0.15,
                    Some((format.dsd_gain_db.0 as f64 / 1_000_000_000.0) as f32),
                ),
                DsdGainMode::Reference
                | DsdGainMode::NativeLevel
                | DsdGainMode::NormalizePeak => {
                    return Err("native Reference DSD gain mode is unavailable before policy promotion".to_string());
                }
            };
            dsd.set_legacy_dsd_to_pcm_gain(mode, margin, gain)
                .map_err(|error| error.to_string())?;
        }
    }
    if is_dsd {
        dsd.pcm_to_dsd.noise_shaper = *format.noise_shaper.selected_value();
        dsd.pcm_to_dsd.modulator_order = *format.modulator_order.selected_value();
        dsd.pcm_to_dsd.filter = *format.conversion_preset.selected_value();
    }

    let target_format_is_wavpack =
        matches!(target_format, pipeline_enums::AudioFormat::WavPack);
    let settings = PipelineSettings {
        target_format,
        target_sample_rate,
        target_bit_depth,
        resample_quality: format.resample_quality,
        nyquist_transition,
        dither_type,
        dither_explicit: !is_dsd && format.dither_overridden,
        preferred_tool,
        force_encode: false,
        flac: tonepoet_pipeline::FlacSettings {
            compression_level: format.flac_compression_level,
            verify: *format.flac_verify.selected_value(),
            write_md5: *format.flac_md5.selected_value(),
        },
        mp3: tonepoet_pipeline::Mp3Settings {
            mode: format.mp3_mode,
            bitrate_kbps: format.mp3_bitrate_kbps,
            vbr_quality: format.mp3_vbr_quality,
        },
        aac: tonepoet_pipeline::AacSettings {
            profile: format.aac_profile,
            bitrate_kbps: format.aac_bitrate_kbps,
        },
        opus: tonepoet_pipeline::OpusSettings {
            content_type: format.opus_content_type,
            bitrate_kbps: format.opus_bitrate_kbps,
            complexity: format.opus_complexity,
        },
        // settings-sentinel-allow: remaining sub-struct defaults
        wavpack: tonepoet_pipeline::WavPackSettings {
            mode: format.wavpack_mode,
            hybrid: format.wavpack_hybrid,
            hybrid_bitrate_kbps: format.wavpack_bitrate_kbps,
            // The generic WavPack UI defaults this dormant flag on. Native
            // Reference canonicalizes non-hybrid WavPack to no correction
            // sidecar; an actually hybrid request remains visible to the
            // planner and is rejected fail-closed.
            correction_file: if format.dsd_reference_controls_available()
                && target_format_is_wavpack
                && !format.wavpack_hybrid
            {
                false
            } else {
                format.wavpack_correction
            },
        },
        ssrc: tonepoet_pipeline::SsrcSettings {
            force: false,
            insane_mode: false,
            profile: None,
            attenuation_db: format.ssrc_attenuation_db,
            min_phase: format.ssrc_min_phase,
            dither_id: format.ssrc_dither_id,
            pdf_type: format.ssrc_pdf_type,
        },
        sox_resampler: tonepoet_pipeline::SoxResamplerSettings {
            chebyshev: format.sox_chebyshev,
            bandwidth_pct: format.sox_bandwidth,
            phase: format.sox_phase,
            allow_aliasing: format.sox_allow_aliasing,
            sinc_taps: format.sox_sinc_taps,
            sinc_attenuation_db: format.sox_sinc_attenuation,
            sinc_passband_hz: format.sox_sinc_passband,
            sinc_transition_hz: format.sox_sinc_transition,
            sinc_kaiser_beta: format.sox_sinc_kaiser_beta,
            sinc_phase: format.sox_sinc_phase,
        },
        soxr_resampler: tonepoet_pipeline::SoxrResamplerSettings {
            chebyshev: format.soxr_chebyshev,
            cutoff: format.soxr_cutoff.map(|pct| pct / 100.0), // TUI stores %, pipeline needs 0.0-1.0
            phase: format.soxr_phase,
        },
        dsd,
        // settings-sentinel-allow: metadata/verification defaults until TUI exposes them
        metadata: Default::default(),
        verification: Default::default(),
        // settings-sentinel-allow: replay_gain.mode set from pill state below
        replay_gain: {
            let mut replay_gain: tonepoet_pipeline::ReplayGainSettings = Default::default();
            replay_gain.mode = replay_gain_mode;
            replay_gain.existing_tags = match *format.replaygain.selected_value() {
                ReplayGainChoice::AlbumIfMissing
                | ReplayGainChoice::TrackIfMissing
                | ReplayGainChoice::BothIfMissing => {
                    tonepoet_pipeline::ReplayGainExistingTagPolicy::SkipIfComplete
                }
                _ => tonepoet_pipeline::ReplayGainExistingTagPolicy::Rescan,
            };
            replay_gain
        },
    };

    if !format.dsd_reference_controls_available() {
        settings
            .validate()
            .map_err(|err| format!("invalid PipelineSettings from TUI state: {err}"))?;
    }
    // Once a promotion release exposes native Reference controls, its unsupported
    // cells remain planner-owned so they surface stable DSD-REF-P0 diagnostics.
    // Pre-promotion legacy settings take the ordinary validation path above.
    Ok(settings)
}

fn map_audio_format(format: AudioFormat, container_ext: Option<&str>) -> pipeline_enums::AudioFormat {
    match (format, container_ext) {
        // Backward-compatible DSD container routing: a DSF target with the
        // DFF container override should still route to the pipeline's Dff
        // variant. The distinct DFF format pill maps directly below.
        (AudioFormat::Dsf, Some("dff")) => pipeline_enums::AudioFormat::Dff,
        // All other formats map 1:1.
        (AudioFormat::Flac, _) => pipeline_enums::AudioFormat::Flac,
        (AudioFormat::Wav, _) => pipeline_enums::AudioFormat::Wav,
        (AudioFormat::Aiff, _) => pipeline_enums::AudioFormat::Aiff,
        (AudioFormat::WavPack, _) => pipeline_enums::AudioFormat::WavPack,
        (AudioFormat::Mp3, _) => pipeline_enums::AudioFormat::Mp3,
        (AudioFormat::Aac, _) => pipeline_enums::AudioFormat::Aac,
        (AudioFormat::Opus, _) => pipeline_enums::AudioFormat::Opus,
        (AudioFormat::Alac, _) => pipeline_enums::AudioFormat::Alac,
        (AudioFormat::Dsf, _) => pipeline_enums::AudioFormat::Dsf,
        (AudioFormat::Dff, _) => pipeline_enums::AudioFormat::Dff,
        (AudioFormat::Dts, _) => pipeline_enums::AudioFormat::Dts,
        (AudioFormat::Ac3, _) => pipeline_enums::AudioFormat::Ac3,
        // Decode-only source formats are never output targets; default to FLAC
        // like the pre-existing Ape arm.
        (AudioFormat::Ape, _)
        | (AudioFormat::Musepack, _)
        | (AudioFormat::Shorten, _)
        | (AudioFormat::Ogg, _)
        | (AudioFormat::Tta, _) => pipeline_enums::AudioFormat::Flac,
        (AudioFormat::Lpcm, Some("aiff")) => pipeline_enums::AudioFormat::Aiff,
        (AudioFormat::Lpcm, _) => pipeline_enums::AudioFormat::Wav,
    }
}

fn map_dither(dither: crate::convert::simple_wizard::DitherType) -> pipeline_enums::DitherType {
    use crate::convert::simple_wizard::DitherType as UiDitherType;

    // Keep this match exhaustive: a newly-added TUI dither variant must choose
    // an explicit pipeline mapping at compile time. Falling through to `None`
    // can silently disable dither/noise shaping, which is the wrong failure
    // mode for final integer audio output.
    match dither {
        UiDitherType::None => pipeline_enums::DitherType::None,
        UiDitherType::TPDF => pipeline_enums::DitherType::Tpdf,
        UiDitherType::SloppedTPDF => pipeline_enums::DitherType::SlopedTpdf,
        UiDitherType::Shibata => pipeline_enums::DitherType::Shibata,
        UiDitherType::LowShibata => pipeline_enums::DitherType::LowShibata,
        UiDitherType::HighShibata => pipeline_enums::DitherType::HighShibata,
        UiDitherType::Lipshitz => pipeline_enums::DitherType::Lipshitz,
        UiDitherType::FWeighted => pipeline_enums::DitherType::FWeighted,
        UiDitherType::ModifiedEWeighted => pipeline_enums::DitherType::ModifiedEWeighted,
        UiDitherType::ImprovedEWeighted => pipeline_enums::DitherType::ImprovedEWeighted,
        UiDitherType::Gesemann => pipeline_enums::DitherType::Gesemann,
    }
}

/// Start processing all queued items. Shared between convert screen and queue screen.
pub fn start_processing(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let acquired_items = match app.persist_conversion_run_acquisition() {
        Ok(items) => items,
        Err(error) => {
            log::error!(
                "conversion run not started because durable acquisition failed: {error}"
            );
            app.set_status(format!("Conversion not started: {error}"));
            return;
        }
    };

    if acquired_items.is_empty() {
        app.set_status("No items ready for conversion");
        return;
    }

    app.processing_active = true;
    let cancel_token = app
        .manager
        .conversion_cancel_token_for_items(acquired_items);

    let queue = app.manager.queue.clone();
    let processor_config = crate::convert::ProcessorConfig {
        worker_count: app.config.conversion.worker_count,
        tool_paths: std::collections::HashMap::new(),
        default_destination_directory: app.config.conversion.default_destination.clone(),
        scratch_directory: app.config.conversion.scratch_directory.clone(),
        scratch_memory_limit_percent: app.config.conversion.scratch_memory_limit_percent,
    };

    let tx_clone = tx.clone();
    tokio::spawn(async move {
        let mut processor = crate::convert::ConversionProcessor::new(processor_config);
        processor.set_cancel_token(cancel_token);

        // Bridge progress broadcasts and lifecycle events to the TUI event loop.
        let (progress_tx, progress_rx) = tokio::sync::broadcast::channel::<ProgressUpdate>(256);
        let (lifecycle_tx, lifecycle_rx) = mpsc::unbounded_channel::<LifecycleEvent>();
        processor.set_progress_channel(progress_tx);
        processor.set_lifecycle_channel(lifecycle_tx);

        let ui_tx = tx_clone.clone();
        let progress_forwarder = tokio::spawn(async move {
            forward_conversion_events(progress_rx, lifecycle_rx, ui_tx).await;
        });

        let process_result = processor
            .process_queue_with_progress(queue.clone(), None)
            .await;

        // Processor finished — drop it so its broadcast sender closes,
        // which causes the forwarder to exit.
        drop(processor);
        let _ = progress_forwarder.await;

        if let Err(error) = process_result {
            let _ = tx_clone
                .send(AppMessage::ConversionError {
                    message: format!("Conversion error: {}", error),
                })
                .await;
            return;
        }

        if let Ok(q) = queue.try_read() {
            let completed = q
                .all_items()
                .iter()
                .filter(|i| {
                    matches!(
                        i.status,
                        ConversionStatus::Completed { .. }
                            | ConversionStatus::CompletedWithActionErrors { .. }
                    )
                })
                .count();
            let failed = q
                .all_items()
                .iter()
                .filter(|i| matches!(i.status, ConversionStatus::Failed { .. }))
                .count();
            let _ = tx_clone
                .send(AppMessage::ConversionComplete { completed, failed })
                .await;
        } else {
            let _ = tx_clone
                .send(AppMessage::ConversionComplete {
                    completed: 0,
                    failed: 0,
                })
                .await;
        }
    });
}

#[cfg(test)]
mod lifecycle_forwarder_tests {
    use super::*;
    use std::path::PathBuf;

    fn processing_update(item_id: &str, progress: f32) -> ProgressUpdate {
        ProgressUpdate {
            item_id: item_id.to_string(),
            track_index: Some(0),
            track_epoch: Some(1),
            progress,
            status: ConversionStatus::Processing {
                progress,
                message: Some(format!("Track 1 - step 1 of 1 - encoding · {progress}% of current track")),
                file_progress: None,
                phase: Some(crate::convert::ConversionPhase::Converting),
                phase_progress: Some(progress),
            },
        }
    }

    #[tokio::test]
    async fn dual_channel_forwarder_skips_lagged_progress_and_forwards_lifecycle() {
        let (progress_tx, progress_rx) = tokio::sync::broadcast::channel(1);
        let (lifecycle_tx, lifecycle_rx) = mpsc::unbounded_channel();
        let (ui_tx, mut ui_rx) = mpsc::channel(8);

        let _ = progress_tx.send(processing_update("stale", 10.0));
        let _ = progress_tx.send(processing_update("fresh", 20.0));
        lifecycle_tx
            .send(LifecycleEvent::ClearTrack {
                item_id: "item-1".to_string(),
                track_index: 2,
                track_epoch: 9,
            })
            .expect("lifecycle send succeeds");
        drop(progress_tx);
        drop(lifecycle_tx);

        forward_conversion_events(progress_rx, lifecycle_rx, ui_tx).await;

        let mut saw_fresh_progress = false;
        let mut saw_stale_progress = false;
        let mut saw_clear = false;
        while let Ok(message) = ui_rx.try_recv() {
            match message {
                AppMessage::ConversionProgress { item_id, progress, .. } => {
                    if item_id == "fresh" && (progress - 20.0).abs() < f32::EPSILON {
                        saw_fresh_progress = true;
                    }
                    if item_id == "stale" {
                        saw_stale_progress = true;
                    }
                }
                AppMessage::ClearTrackProgress {
                    item_id,
                    track_index,
                    track_epoch,
                } => {
                    saw_clear = item_id == "item-1" && track_index == 2 && track_epoch == 9;
                }
                _ => {}
            }
        }

        assert!(saw_fresh_progress);
        assert!(!saw_stale_progress);
        assert!(saw_clear);
    }

    #[tokio::test]
    async fn lifecycle_forwarder_does_not_block_when_ui_channel_is_full() {
        let (progress_tx, progress_rx) = tokio::sync::broadcast::channel(1);
        let (lifecycle_tx, lifecycle_rx) = mpsc::unbounded_channel();
        let (ui_tx, mut ui_rx) = mpsc::channel(1);
        ui_tx
            .try_send(AppMessage::StatusMessage("filled".to_string()))
            .expect("pre-fill UI channel");

        let handle = tokio::spawn(forward_conversion_events(progress_rx, lifecycle_rx, ui_tx));
        lifecycle_tx
            .send(LifecycleEvent::ClearTrack {
                item_id: "item-1".to_string(),
                track_index: 0,
                track_epoch: 1,
            })
            .expect("lifecycle send succeeds");
        drop(progress_tx);
        drop(lifecycle_tx);

        tokio::time::timeout(std::time::Duration::from_millis(10), handle)
            .await
            .expect("forwarder exits without waiting for UI capacity")
            .expect("forwarder task succeeds");

        match ui_rx.try_recv().expect("original fill message remains") {
            AppMessage::StatusMessage(value) => assert_eq!(value, "filled"),
            other => panic!("expected original status message, got {other:?}"),
        }
        assert!(ui_rx.try_recv().is_err());
    }

    #[test]
    fn lifecycle_item_terminal_maps_to_item_progress_message() {
        let message = lifecycle_to_app_message(LifecycleEvent::ItemTerminal {
            item_id: "item-1".to_string(),
            progress: 100.0,
            status: ConversionStatus::Completed {
                warning_count: 0,
                output_path: PathBuf::from("/tmp/out.flac"),
                log_path: None,
            },
        });

        match message {
            AppMessage::ConversionProgress {
                item_id,
                track_index,
                track_epoch,
                progress,
                status,
            } => {
                assert_eq!(item_id, "item-1");
                assert_eq!(track_index, None);
                assert_eq!(track_epoch, None);
                assert_eq!(progress, 100.0);
                assert!(matches!(status, ConversionStatus::Completed { .. }));
            }
            other => panic!("expected item progress message, got {other:?}"),
        }
    }

    fn assert_legacy_gain(
        settings: &PipelineSettings,
        mode: tonepoet_pipeline::DsdToPcmGainMode,
        margin: f32,
        gain: Option<f32>,
    ) {
        assert!(!settings.dsd.is_native_v2());
        let legacy = settings
            .dsd
            .legacy_behavior()
            .expect("pre-promotion TUI settings retain legacy authority");
        assert_eq!(legacy.lowpass, tonepoet_pipeline::DsdLowpassMethod::Auto);
        assert_eq!(legacy.gain_mode, mode);
        assert!((legacy.auto_gain_margin_db - margin).abs() < 1e-6);
        assert_eq!(legacy.gain_db, gain);
        let encoded = serde_json::to_value(settings).expect("serialize exact legacy TUI settings");
        let dsd = encoded["dsd"].as_object().expect("legacy DSD object");
        assert!(!dsd.contains_key("schema_version"));
        assert!(!dsd.contains_key("from_dsd"));
        assert_eq!(
            dsd.get("dsd_to_pcm_gain_mode").and_then(serde_json::Value::as_str),
            // The frozen legacy-v1 wire predates any rename_all attribute:
            // variant names serialize capitalized, and byte compatibility
            // with historical queue/settings files pins that spelling.
            Some(match mode {
                tonepoet_pipeline::DsdToPcmGainMode::Disabled => "Disabled",
                tonepoet_pipeline::DsdToPcmGainMode::Auto => "Auto",
                tonepoet_pipeline::DsdToPcmGainMode::Manual => "Manual",
            })
        );
        let encoded_margin = dsd
            .get("dsd_to_pcm_auto_gain_margin_db")
            .and_then(serde_json::Value::as_f64)
            .expect("legacy auto-gain margin is serialized");
        assert!((encoded_margin - f64::from(margin)).abs() < 1e-6);
        match gain {
            Some(expected) => {
                let encoded_gain = dsd
                    .get("dsd_to_pcm_gain_db")
                    .and_then(serde_json::Value::as_f64)
                    .expect("legacy manual gain is serialized");
                assert!((encoded_gain - f64::from(expected)).abs() < 1e-6);
            }
            None => assert!(
                dsd.get("dsd_to_pcm_gain_db")
                    .is_some_and(serde_json::Value::is_null),
                "non-manual legacy modes serialize a null manual gain"
            ),
        }
    }

    fn planned_legacy_dsd_commands(format: &FormatState) -> Vec<Vec<String>> {
        let settings = format_state_to_pipeline_settings(format).expect("TUI settings");
        let plan = tonepoet_pipeline::plan_conversion(&tonepoet_pipeline::PlanRequest {
            input_path: PathBuf::from("source.dff"),
            output_path: PathBuf::from("output.flac"),
            source: tonepoet_pipeline::SourceInfo {
                format: tonepoet_pipeline::AudioFormat::Dff,
                codec: tonepoet_pipeline::AudioCodec::Dsd,
                sample_rate_hz: Some(2_822_400),
                bit_depth: None,
                true_source_depth: None,
                source_representation: tonepoet_pipeline::SourceRepresentationKind::Dsd,
                sample_kind: Some(tonepoet_pipeline::SampleKind::Dsd),
                channels: Some(2),
                duration: Some(std::time::Duration::from_secs(60)),
                dsd_source_kind: Some(tonepoet_pipeline::DsdSourceKind::DsdiffUncompressed),
                audio_md5: None,
            },
            settings,
            intermediate_dir: Some(PathBuf::from("work")),
            container_ffmpeg_flags: Vec::new(),
            resolved_output_target: Some(tonepoet_pipeline::ResolvedOutputTarget::FlacNative),
            reference_programme_scope: tonepoet_pipeline::ReferenceProgrammeScope::Singleton,
            planned_riff_non_audio_upper_bound_bytes: None,
        })
        .expect("legacy DSD-to-PCM plan");
        match plan.action {
            tonepoet_pipeline::PlanAction::Execute { commands, steps, .. } => {
                assert!(steps.is_empty(), "legacy plan must not use Reference steps");
                commands.into_iter().map(|command| command.args).collect()
            }
            other => panic!("expected executable legacy DSD plan, got {other:?}"),
        }
    }

    fn command_has_pair(commands: &[Vec<String>], left: &str, right: &str) -> bool {
        commands.iter().any(|args| {
            args.windows(2).any(|pair| pair[0] == left && pair[1] == right)
        })
    }

    #[test]
    fn pre_promotion_tui_gain_modes_reach_the_legacy_sox_argv() {
        let mut disabled = FormatState::new();
        disabled.set_source_is_dsd(true);
        assert!(disabled.dsd_gain_mode.select_value(&DsdGainMode::Disabled));
        let disabled_commands = planned_legacy_dsd_commands(&disabled);
        assert!(!disabled_commands
            .iter()
            .any(|args| args.iter().any(|arg| arg == "norm")));

        let mut auto = FormatState::new();
        auto.set_source_is_dsd(true);
        assert!(auto.dsd_gain_mode.select_value(&DsdGainMode::Auto));
        auto.dsd_auto_gain_margin_db = "0.500000000".parse().unwrap();
        let auto_commands = planned_legacy_dsd_commands(&auto);
        assert!(command_has_pair(&auto_commands, "norm", "-0.50"));

        let mut manual = FormatState::new();
        manual.set_source_is_dsd(true);
        assert!(manual.dsd_gain_mode.select_value(&DsdGainMode::Fixed));
        manual.dsd_gain_db = "2.250000000".parse().unwrap();
        let manual_commands = planned_legacy_dsd_commands(&manual);
        assert!(command_has_pair(&manual_commands, "gain", "+2.25"));
    }

    #[test]
    fn pre_promotion_tui_disabled_selection_builds_exact_legacy_disabled_wire() {
        let mut format = FormatState::new();
        format.set_source_is_dsd(true);
        assert!(format.dsd_gain_mode.select_value(&DsdGainMode::Disabled));
        let settings = format_state_to_pipeline_settings(&format).unwrap();
        assert_legacy_gain(
            &settings,
            tonepoet_pipeline::DsdToPcmGainMode::Disabled,
            0.15,
            None,
        );
    }

    #[test]
    fn pre_promotion_tui_auto_selection_builds_exact_legacy_auto_wire() {
        let mut format = FormatState::new();
        format.set_source_is_dsd(true);
        assert!(format.dsd_gain_mode.select_value(&DsdGainMode::Auto));
        format.dsd_auto_gain_margin_db = "0.500000000".parse().unwrap();
        let settings = format_state_to_pipeline_settings(&format).unwrap();
        assert_legacy_gain(
            &settings,
            tonepoet_pipeline::DsdToPcmGainMode::Auto,
            0.5,
            None,
        );
    }

    #[test]
    fn pre_promotion_tui_fixed_selection_builds_exact_legacy_manual_wire() {
        let mut format = FormatState::new();
        format.set_source_is_dsd(true);
        assert!(format.dsd_gain_mode.select_value(&DsdGainMode::Fixed));
        format.dsd_gain_db = "2.250000000".parse().unwrap();
        let settings = format_state_to_pipeline_settings(&format).unwrap();
        assert_legacy_gain(
            &settings,
            tonepoet_pipeline::DsdToPcmGainMode::Manual,
            0.15,
            Some(2.25),
        );
    }

    #[test]
    fn pre_promotion_tui_hidden_dsd_controls_leave_pcm_source_settings_legacy() {
        let mut format = FormatState::new();
        format.dsd_gain_db = "2.250000000".parse().unwrap();
        let settings = format_state_to_pipeline_settings(&format).unwrap();
        assert_legacy_gain(
            &settings,
            tonepoet_pipeline::DsdToPcmGainMode::Disabled,
            0.15,
            None,
        );
    }


    #[test]
    fn pre_promotion_tui_out_of_range_auto_margin_is_rejected() {
        let mut format = FormatState::new();
        format.set_source_is_dsd(true);
        assert!(format.dsd_gain_mode.select_value(&DsdGainMode::Auto));
        format.dsd_auto_gain_margin_db = tonepoet_pipeline::DbNano(7_000_000_000);
        let error = format_state_to_pipeline_settings(&format).unwrap_err();
        assert!(error.contains("auto gain safety margin must be between 0 and 6 dB"));
    }

    #[test]
    fn pre_promotion_tui_out_of_range_fixed_gain_is_rejected() {
        let mut format = FormatState::new();
        format.set_source_is_dsd(true);
        assert!(format.dsd_gain_mode.select_value(&DsdGainMode::Fixed));
        format.dsd_gain_db = tonepoet_pipeline::DbNano(123_000_000_000);
        let error = format_state_to_pipeline_settings(&format).unwrap_err();
        assert!(error.contains("gain must be between -24 and +24 dB"));
    }

    #[test]
    fn map_dither_maps_every_known_tui_variant_explicitly() {
        use crate::convert::simple_wizard::DitherType as UiDitherType;

        let cases = [
            (UiDitherType::None, pipeline_enums::DitherType::None),
            (UiDitherType::TPDF, pipeline_enums::DitherType::Tpdf),
            (UiDitherType::SloppedTPDF, pipeline_enums::DitherType::SlopedTpdf),
            (UiDitherType::Shibata, pipeline_enums::DitherType::Shibata),
            (UiDitherType::LowShibata, pipeline_enums::DitherType::LowShibata),
            (UiDitherType::HighShibata, pipeline_enums::DitherType::HighShibata),
            (UiDitherType::Lipshitz, pipeline_enums::DitherType::Lipshitz),
            (UiDitherType::FWeighted, pipeline_enums::DitherType::FWeighted),
            (
                UiDitherType::ModifiedEWeighted,
                pipeline_enums::DitherType::ModifiedEWeighted,
            ),
            (
                UiDitherType::ImprovedEWeighted,
                pipeline_enums::DitherType::ImprovedEWeighted,
            ),
            (UiDitherType::Gesemann, pipeline_enums::DitherType::Gesemann),
        ];

        for (ui, expected_pipeline) in cases {
            assert_eq!(map_dither(ui), expected_pipeline);
        }
    }

    #[test]
    fn format_state_to_pipeline_settings_preserves_dither_choice_and_explicitness() {
        let mut format = FormatState::new();
        format
            .dither
            .select_value(&crate::convert::simple_wizard::DitherType::Shibata);
        format.dither_overridden = true;

        let settings = format_state_to_pipeline_settings(&format).unwrap();

        assert_eq!(settings.dither_type, pipeline_enums::DitherType::Shibata);
        assert!(settings.dither_explicit);

        format.dither_overridden = false;
        let automatic = format_state_to_pipeline_settings(&format).unwrap();
        assert_eq!(automatic.dither_type, pipeline_enums::DitherType::Shibata);
        assert!(!automatic.dither_explicit);
    }

    #[test]
    fn dsd_targets_never_forward_pcm_dither_explicitness() {
        let mut format = FormatState::new();
        format.format.select_value(&AudioFormat::Dsf);
        // Mirror the UI cascade so a valid DSD target rate is armed; without it
        // the sample-rate pill keeps its PCM default (44.1 kHz) and settings
        // conversion fails before the DSD dither-explicitness guard under test
        // is ever reached.
        format.apply_format_constraints();
        // Arm the override AFTER the cascade so the test genuinely exercises the
        // `!is_dsd && dither_overridden` guard rather than an empty override.
        format.dither_overridden = true;

        let settings = format_state_to_pipeline_settings(&format).unwrap();

        assert_eq!(settings.dither_type, pipeline_enums::DitherType::None);
        assert!(!settings.dither_explicit);
    }

    #[test]
    fn source_relative_pills_remain_honest_in_legacy_and_pipeline_carriers() {
        use crate::convert::formats::QualitySettings;
        use crate::tui::app::{BitDepthChoice, SOURCE_SAMPLE_RATE_SENTINEL};

        let mut format = FormatState::new();
        format.format.select_value(&AudioFormat::Wav);
        format.sample_rate.select_value(&SOURCE_SAMPLE_RATE_SENTINEL);
        format.bit_depth.select_value(&BitDepthChoice::Source);
        let output = OutputOptionsState::new();

        let options = try_pills_to_options(&format, &output, &TonepoetConfig::default())
            .expect("source-relative WAV policy is valid");

        assert_eq!(options.target_sample_rate, None);
        assert_eq!(options.target_bit_depth, None);
        assert_eq!(
            options.quality,
            QualitySettings::Wav {
                bit_depth: None,
                sample_rate: None,
            }
        );
        let settings = options.pipeline_settings.expect("pipeline settings attached");
        assert_eq!(settings.target_sample_rate, pipeline_enums::RateTarget::Source);
        assert_eq!(settings.target_bit_depth, pipeline_enums::BitDepthTarget::Source);
    }

    #[test]
    fn pills_to_options_keeps_user_template_raw_until_request_boundary() {
        let format = FormatState::new();
        let mut output = OutputOptionsState::new();
        output.filename_template = "%ARTIST% - %ALBUM%/{%TITLE_EXTRA% }%TRACK% - %TITLE%".to_string();
        output.disc_subfolders.select_value(&true);

        let options = try_pills_to_options(&format, &output, &TonepoetConfig::default())
            .expect("valid default TUI state");

        assert_eq!(
            options.naming_template.as_deref(),
            Some("%ARTIST% - %ALBUM%/{%TITLE_EXTRA% }%TRACK% - %TITLE%")
        );
        assert!(options.create_disc_subfolders);
        assert_eq!(
            options.effective_naming_template("%NN% - %TITLE%"),
            "%DISC_FOLDER%/%ARTIST% - %ALBUM%/{%TITLE_EXTRA% }%TRACK% - %TITLE%",
            "disc-folder projection must prepend to the user's arbitrary filename template, not replace it with the default"
        );
    }

}
