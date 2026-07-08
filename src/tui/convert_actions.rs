//! Convert screen actions: build ConversionOptions from pills, add to queue, start conversion

use tokio::sync::mpsc;

use crate::config::TonepoetConfig;
use crate::convert::formats::{
    AacProfile, AudioFormat, ConversionOptions, Mp3BitrateMode, QualitySettings, WavPackMode,
};
use crate::convert::simple_wizard::ReplayGainMode;
use tonepoet_pipeline::enums as pipeline_enums;
use tonepoet_pipeline::PipelineSettings;
use crate::convert::{
    queue_identity_path, ConversionItem, ConversionStatus, LifecycleEvent, ProgressUpdate,
};

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
    options.target_sample_rate = Some(*format.sample_rate.selected_value());
    options.target_bit_depth = Some(format.bit_depth.selected_value().to_backend_depth());
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

    // Use backend bit depth for quality settings when the legacy path still needs it.
    let backend_depth = bit_depth.to_backend_depth();

    let quality = match output_format {
        AudioFormat::Flac => QualitySettings::Flac {
            compression_level: 5,
        },
        AudioFormat::Wav => QualitySettings::Wav {
            bit_depth: backend_depth as u16,
            sample_rate: target_sample_rate,
        },
        AudioFormat::Aiff => QualitySettings::Aiff {
            bit_depth: backend_depth as u16,
            sample_rate: target_sample_rate,
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
        AudioFormat::Dts | AudioFormat::Ac3 | AudioFormat::Ape | AudioFormat::Lpcm => {
            QualitySettings::Flac { compression_level: 8 }
        }
    };

    let (calculate_replaygain, replaygain_mode) = if is_dsd {
        (false, None)
    } else {
        match rg {
            ReplayGainChoice::Album => (true, Some(ReplayGainMode::Album)),
            ReplayGainChoice::Track => (true, Some(ReplayGainMode::Track)),
            ReplayGainChoice::Both => (true, Some(ReplayGainMode::Both)),
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
    let target_bit_depth = if legacy_bit_depth_applies && !is_dsd {
        Some(backend_depth)
    } else {
        None
    };

    let pipeline_settings = format_state_to_pipeline_settings(format)?;

    Ok(ConversionOptions {
        output_format,
        quality,
        target_sample_rate: Some(target_sample_rate),
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
            let dsd_rate = pipeline_enums::DsdRate::from_hz(selected_rate)
                .ok_or_else(|| format!("{} is not a supported DSD target rate", selected_rate))?;
            (
                pipeline_enums::RateTarget::Dsd(dsd_rate),
                pipeline_enums::BitDepthTarget::Source,
                pipeline_enums::DitherType::None,
                pipeline_enums::PreferredTool::Sox,
                pipeline_enums::NyquistTransition::Gentle,
            )
        } else {
            let target_depth = match *format.bit_depth.selected_value() {
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
                pipeline_enums::RateTarget::PcmHz(selected_rate),
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
            ReplayGainChoice::Album => Some(pipeline_enums::ReplayGainMode::Album),
            ReplayGainChoice::Track => Some(pipeline_enums::ReplayGainMode::Track),
            ReplayGainChoice::Both => Some(pipeline_enums::ReplayGainMode::Both),
            ReplayGainChoice::Off => None,
        }
    };

    // settings-sentinel-allow: sub-struct defaults are correct here — user-facing
    // settings (format, rate, depth, dither, resampler, RG) are set from pill state;
    // codec-specific sub-structs (flac, mp3, aac, etc.) use defaults until the TUI
    // exposes those settings.
    let mut dsd: tonepoet_pipeline::DsdSettings = Default::default();
    if format.dsd_to_pcm_gain_available() {
        dsd.dsd_to_pcm_gain_mode = *format.dsd_gain_mode.selected_value();
        dsd.dsd_to_pcm_auto_gain_margin_db = format.dsd_auto_gain_margin_db;
        dsd.dsd_to_pcm_gain_db = if *format.dsd_gain_mode.selected_value()
            == pipeline_enums::DsdToPcmGainMode::Manual
        {
            Some(if format.dsd_gain_db.is_finite() {
                format
                    .dsd_gain_db
                    .clamp(DSD_TO_PCM_GAIN_DB_MIN, DSD_TO_PCM_GAIN_DB_MAX)
            } else {
                0.0
            })
        } else {
            None
        };
    } else {
        dsd.dsd_to_pcm_gain_mode = pipeline_enums::DsdToPcmGainMode::Disabled;
        dsd.dsd_to_pcm_gain_db = None;
    }
    if is_dsd {
        dsd.noise_shaper = *format.noise_shaper.selected_value();
        dsd.modulator_order = *format.modulator_order.selected_value();
        dsd.pcm_to_dsd_filter = *format.conversion_preset.selected_value();
    }

    let settings = PipelineSettings {
        target_format,
        target_sample_rate,
        target_bit_depth,
        resample_quality: format.resample_quality,
        nyquist_transition,
        dither_type,
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
            correction_file: format.wavpack_correction,
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
            replay_gain
        },
    };

    settings
        .validate()
        .map_err(|err| format!("invalid PipelineSettings from TUI state: {err}"))?;
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
        (AudioFormat::Ape, _) => pipeline_enums::AudioFormat::Flac, // Ape is decode-only; target FLAC
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

/// Outcome of a batch commit: counts for the caller to report.
#[derive(Debug, Default)]
pub struct CommitOutcome {
    /// Files successfully added to the queue (new items).
    pub enqueued: usize,
    /// Files skipped because they were already in the queue.
    pub skipped: usize,
    /// Files that failed to enqueue (e.g. add_file errored).
    pub errors: usize,
    /// Files that were previously converted (warning, not blocking).
    pub previously_converted: usize,
    /// Last error message (for status display when errors > 0).
    pub last_error: Option<String>,
}

/// Commit a batch of paths to the queue with the given conversion options.
///
/// Checks each path against the current queue to avoid duplicates (skips
/// items already queued, processing, or paused). Persists the queue to
/// disk on completion (if `persist_queue` is enabled).
///
/// Returns a `CommitOutcome` with counts; does NOT set any status message
/// or touch navigation state — that's the caller's job. Also does NOT
/// start processing; use `start_processing` separately for that.
///
/// Works for single-file and multi-file batches uniformly. A single-file
/// caller can pass `&[path]`.
pub fn commit_batch(
    app: &mut AppState,
    paths: &[std::path::PathBuf],
    options: &ConversionOptions,
) -> CommitOutcome {
    commit_batch_with_cue_artifacts(app, paths, &std::collections::HashSet::new(), options)
}

/// Map queue-expansion CUE-artifact metadata onto the sidecar policy for one
/// queued path. Shared by the TUI commit path and the CLI folder scan so both
/// front ends apply identical CUE semantics.
pub fn cue_sidecar_override_for_commit_path(
    path: &std::path::Path,
    cue_artifact_audio: &std::collections::HashSet<std::path::PathBuf>,
) -> Option<crate::convert::pipeline::CueSidecarPolicy> {
    cue_artifact_audio
        .contains(path)
        .then_some(crate::convert::pipeline::CueSidecarPolicy::EmbeddedOnly)
}

fn is_active_commit_item(item: &ConversionItem) -> bool {
    !matches!(
        item.status,
        ConversionStatus::Completed { .. }
            | ConversionStatus::Failed { .. }
            | ConversionStatus::Cancelled
    )
}

fn active_queue_identity_set(
    existing: &[ConversionItem],
) -> std::collections::HashSet<std::path::PathBuf> {
    existing
        .iter()
        .filter(|item| is_active_commit_item(item))
        .map(|item| queue_identity_path(&item.input_path))
        .collect()
}

fn commit_path_already_queued(
    active_queue_identities: &std::collections::HashSet<std::path::PathBuf>,
    path: &std::path::Path,
) -> bool {
    active_queue_identities.contains(&queue_identity_path(path))
}

fn options_for_queue_request(options: &ConversionOptions) -> ConversionOptions {
    let mut queued = options.clone();
    queued.naming_template = Some(options.effective_naming_template("%NN% - %TITLE%"));
    queued
}

/// Commit a batch and mark paths whose sibling CUE was already suppressed by
/// browse expansion as sidecar-CUE metadata artifacts.
pub fn commit_batch_with_cue_artifacts(
    app: &mut AppState,
    paths: &[std::path::PathBuf],
    cue_artifact_audio: &std::collections::HashSet<std::path::PathBuf>,
    options: &ConversionOptions,
) -> CommitOutcome {
    let existing = app.manager.get_items_clone();
    let mut active_queue_identities = active_queue_identity_set(&existing);
    let mut outcome = CommitOutcome::default();
    let queued_options = options_for_queue_request(options);

    for path in paths {
        if commit_path_already_queued(&active_queue_identities, path) {
            outcome.skipped += 1;
            continue;
        }
        let path_identity = queue_identity_path(path);

        // Check if this file was previously converted (non-blocking warning).
        if app.db.was_previously_converted(&path.display().to_string()) {
            outcome.previously_converted += 1;
        }

        // For encrypted archives, resolve password:
        // session override → keychain MRU → config → None.
        let archive_pw = if crate::is_encrypted_archive_ext(path) {
            app.archive_passwords
                .get(path)
                .cloned()
                .or_else(|| {
                    app.keychain.ensure_loaded();
                    app.keychain.passwords.first().cloned()
                })
                .or_else(|| app.config.conversion.archive_password.clone())
        } else {
            None
        };

        let cue_sidecar_override =
            cue_sidecar_override_for_commit_path(path, cue_artifact_audio);

        match app.manager.add_file_ready_for_processing_with_cue_sidecar_override(
            path.clone(),
            queued_options.clone(),
            archive_pw,
            cue_sidecar_override,
        ) {
            Ok(_) => {
                outcome.enqueued += 1;
                active_queue_identities.insert(path_identity);
            }
            Err(err) => {
                log::warn!("commit failed for {}: {err}", path.display());
                outcome.errors += 1;
                outcome.last_error = Some(format!("{err}"));
            }
        }
    }

    app.save_queue();

    outcome
}

/// Start processing all queued items. Shared between convert screen and queue screen.
pub fn start_processing(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let ready_count = app
        .manager
        .get_items_clone()
        .iter()
        .filter(|i| matches!(i.status, ConversionStatus::Queued))
        .count();

    if ready_count == 0 {
        app.set_status("No items ready for conversion");
        return;
    }

    app.processing_active = true;
    let cancel_token = app.manager.conversion_cancel_token();

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

        if let Err(e) = processor
            .process_queue_with_progress(queue.clone(), None)
            .await
        {
            let _ = tx_clone
                .send(AppMessage::ConversionError {
                    message: format!("Conversion error: {}", e),
                })
                .await;
        }

        // Processor finished — drop it so its broadcast sender closes,
        // which causes the forwarder to exit.
        drop(processor);
        let _ = progress_forwarder.await;

        if let Ok(q) = queue.try_read() {
            let completed = q
                .all_items()
                .iter()
                .filter(|i| matches!(i.status, ConversionStatus::Completed { .. }))
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

    #[test]
    fn format_state_to_pipeline_settings_maps_dsd_to_pcm_gain() {
        let mut format = FormatState::new();
        format.set_source_is_dsd(true);
        format.dsd_gain_mode.select_value(&DsdGainMode::Auto);
        format.dsd_auto_gain_margin_db = 0.5;

        let settings = format_state_to_pipeline_settings(&format).unwrap();

        assert_eq!(
            settings.dsd.dsd_to_pcm_gain_mode,
            pipeline_enums::DsdToPcmGainMode::Auto
        );
        assert!((settings.dsd.dsd_to_pcm_auto_gain_margin_db - 0.5).abs() < f32::EPSILON);
        assert_eq!(settings.dsd.dsd_to_pcm_gain_db, None);
    }

    #[test]
    fn format_state_to_pipeline_settings_maps_manual_dsd_to_pcm_gain() {
        let mut format = FormatState::new();
        format.set_source_is_dsd(true);
        format.dsd_gain_mode.select_value(&DsdGainMode::Manual);
        format.dsd_gain_db = 2.25;

        let settings = format_state_to_pipeline_settings(&format).unwrap();

        assert_eq!(
            settings.dsd.dsd_to_pcm_gain_mode,
            pipeline_enums::DsdToPcmGainMode::Manual
        );
        assert_eq!(settings.dsd.dsd_to_pcm_gain_db, Some(2.25));
    }


    #[test]
    fn format_state_to_pipeline_settings_disables_hidden_dsd_gain_for_pcm_source() {
        let mut format = FormatState::new();
        format.dsd_gain_mode.select_value(&DsdGainMode::Manual);
        format.dsd_gain_db = 2.25;

        let settings = format_state_to_pipeline_settings(&format).unwrap();

        assert_eq!(
            settings.dsd.dsd_to_pcm_gain_mode,
            pipeline_enums::DsdToPcmGainMode::Disabled
        );
        assert_eq!(settings.dsd.dsd_to_pcm_gain_db, None);
    }

    #[test]
    fn format_state_to_pipeline_settings_clamps_manual_dsd_to_pcm_gain() {
        let mut format = FormatState::new();
        format.set_source_is_dsd(true);
        format.dsd_gain_mode.select_value(&DsdGainMode::Manual);
        format.dsd_gain_db = DSD_TO_PCM_GAIN_DB_MAX + 99.0;

        let settings = format_state_to_pipeline_settings(&format).unwrap();

        assert_eq!(settings.dsd.dsd_to_pcm_gain_db, Some(DSD_TO_PCM_GAIN_DB_MAX));
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
    fn format_state_to_pipeline_settings_preserves_dither_choice() {
        let mut format = FormatState::new();
        format.dither.select_value(&crate::convert::simple_wizard::DitherType::Shibata);

        let settings = format_state_to_pipeline_settings(&format).unwrap();

        assert_eq!(settings.dither_type, pipeline_enums::DitherType::Shibata);
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

#[cfg(test)]
mod cue_sidecar_commit_metadata_tests {
    use super::*;
    use std::collections::HashSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            let path = std::env::temp_dir().join(format!(
                "tonepoet-commit-identity-{name}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_minimal_bluray_layout(root: &Path) {
        let bdmv = root.join("BDMV");
        fs::create_dir_all(bdmv.join("PLAYLIST")).expect("create PLAYLIST");
        fs::create_dir_all(bdmv.join("STREAM")).expect("create STREAM");
        fs::write(bdmv.join("index.bdmv"), b"index").expect("write index.bdmv");
        fs::write(bdmv.join("MovieObject.bdmv"), b"movie").expect("write MovieObject.bdmv");
        fs::write(bdmv.join("PLAYLIST").join("00000.mpls"), b"playlist")
            .expect("write playlist");
        fs::write(bdmv.join("STREAM").join("00000.m2ts"), b"stream").expect("write stream");
    }

    fn active_item(path: PathBuf) -> ConversionItem {
        let mut options = ConversionOptions::default();
        options.pipeline_settings = Some(PipelineSettings::default());
        let mut item = ConversionItem::new(path, crate::convert::FileFormat::Archive, options);
        item.status = ConversionStatus::Queued;
        item
    }

    #[test]
    fn commit_override_is_computed_only_from_current_batch_metadata() {
        let path = PathBuf::from("/tmp/album/01.flac");
        let mut artifact_audio = HashSet::new();
        artifact_audio.insert(path.clone());

        assert_eq!(
            cue_sidecar_override_for_commit_path(&path, &artifact_audio),
            Some(crate::convert::pipeline::CueSidecarPolicy::EmbeddedOnly)
        );

        let later_no_artifact_batch = HashSet::new();
        assert_eq!(
            cue_sidecar_override_for_commit_path(&path, &later_no_artifact_batch),
            None,
            "a later batch with the same path vector must not inherit stale artifact metadata"
        );
    }


    #[test]
    fn queue_request_options_apply_effective_disc_subfolder_template_once() {
        let mut options = ConversionOptions::default();
        options.naming_template = Some("%ARTIST%/%TRACK% - %TITLE% {%TITLE_EXTRA%}".to_string());
        options.create_disc_subfolders = true;

        let queued = options_for_queue_request(&options);

        assert_eq!(
            queued.naming_template.as_deref(),
            Some("%DISC_FOLDER%/%ARTIST%/%TRACK% - %TITLE% {%TITLE_EXTRA%}")
        );
        assert!(queued.create_disc_subfolders);
        assert_eq!(
            options_for_queue_request(&queued).naming_template.as_deref(),
            Some("%DISC_FOLDER%/%ARTIST%/%TRACK% - %TITLE% {%TITLE_EXTRA%}"),
            "queue/request normalization must be idempotent and must preserve the user's arbitrary template"
        );
    }

    #[test]
    fn commit_duplicate_detection_skips_bdmv_child_when_disc_root_is_active() {
        let temp = TempDir::new("root-active");
        write_minimal_bluray_layout(&temp.path);
        let bdmv = temp.path.join("BDMV");
        let existing = vec![active_item(temp.path.clone())];
        let identities = active_queue_identity_set(&existing);

        assert!(commit_path_already_queued(&identities, &bdmv));
    }

    #[test]
    fn commit_duplicate_detection_skips_disc_root_when_bdmv_child_is_active() {
        let temp = TempDir::new("bdmv-active");
        write_minimal_bluray_layout(&temp.path);
        let bdmv = temp.path.join("BDMV");
        let existing = vec![active_item(bdmv)];
        let identities = active_queue_identity_set(&existing);

        assert!(commit_path_already_queued(&identities, &temp.path));
    }

    #[test]
    fn commit_duplicate_detection_keeps_ordinary_path_identity_semantics() {
        let temp = TempDir::new("ordinary");
        let first = temp.path.join("one.flac");
        let second = temp.path.join("two.flac");
        fs::write(&first, b"one").expect("write first");
        fs::write(&second, b"two").expect("write second");
        let existing = vec![active_item(first.clone())];
        let identities = active_queue_identity_set(&existing);

        assert!(commit_path_already_queued(
            &identities,
            &temp.path.join(".").join("one.flac")
        ));
        assert!(!commit_path_already_queued(&identities, &second));
    }

}
