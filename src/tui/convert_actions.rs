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
    options.pipeline_settings = format_state_to_pipeline_settings(&format).ok();
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
        naming_template: Some(output_opts.filename_template.clone()),
        folder_template: Some(output_opts.folder_template.clone()),
        output_dir: output_opts.dest_path.clone(),
        merge_to_single: matches!(merge, MergeMode::SingleImage),
        preserve_metadata: true,
        append_lineage_to_comment: config.conversion.append_lineage_to_comment,
        write_log_file: config.conversion.write_log_file,
        generate_cue_files: config.conversion.generate_cue_files,
        cue_generation_mode: config.conversion.cue_generation_mode.clone(),
        pipeline_settings: Some(pipeline_settings),
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
    let target_format = map_audio_format(*format.format.selected_value());
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
    if is_dsd {
        dsd.noise_shaper = *format.noise_shaper.selected_value();
        dsd.modulator_order = *format.modulator_order.selected_value();
        dsd.pcm_to_dsd_filter = *format.conversion_preset.selected_value();
    }

    let settings = PipelineSettings {
        target_format,
        target_sample_rate,
        target_bit_depth,
        resample_quality: pipeline_enums::ResampleQuality::Ultra,
        nyquist_transition,
        dither_type,
        preferred_tool,
        force_encode: false,
        // settings-sentinel-allow: codec sub-struct defaults until TUI exposes them
        flac: Default::default(),
        mp3: Default::default(),
        aac: Default::default(),
        opus: Default::default(),
        // settings-sentinel-allow: remaining sub-struct defaults
        wavpack: Default::default(),
        ssrc: Default::default(),
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

fn map_audio_format(format: AudioFormat) -> pipeline_enums::AudioFormat {
    match format {
        AudioFormat::Flac => pipeline_enums::AudioFormat::Flac,
        AudioFormat::Wav => pipeline_enums::AudioFormat::Wav,
        AudioFormat::Aiff => pipeline_enums::AudioFormat::Aiff,
        AudioFormat::WavPack => pipeline_enums::AudioFormat::WavPack,
        AudioFormat::Mp3 => pipeline_enums::AudioFormat::Mp3,
        AudioFormat::Aac => pipeline_enums::AudioFormat::Aac,
        AudioFormat::Opus => pipeline_enums::AudioFormat::Opus,
        AudioFormat::Alac => pipeline_enums::AudioFormat::Alac,
        AudioFormat::Dsf => pipeline_enums::AudioFormat::Dsf,
        AudioFormat::Dff => pipeline_enums::AudioFormat::Dff,
    }
}

fn map_dither(dither: crate::convert::simple_wizard::DitherType) -> pipeline_enums::DitherType {
    match dither {
        crate::convert::simple_wizard::DitherType::None => pipeline_enums::DitherType::None,
        crate::convert::simple_wizard::DitherType::TPDF => pipeline_enums::DitherType::Tpdf,
        crate::convert::simple_wizard::DitherType::Shibata => pipeline_enums::DitherType::Shibata,
        crate::convert::simple_wizard::DitherType::Lipshitz => pipeline_enums::DitherType::Lipshitz,
        crate::convert::simple_wizard::DitherType::Gesemann => pipeline_enums::DitherType::Gesemann,
        crate::convert::simple_wizard::DitherType::LowShibata => pipeline_enums::DitherType::LowShibata,
        crate::convert::simple_wizard::DitherType::HighShibata => pipeline_enums::DitherType::HighShibata,
        _ => pipeline_enums::DitherType::None,
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
    let existing = app.manager.get_items_clone();
    let mut outcome = CommitOutcome::default();

    for path in paths {
        let already_queued = existing.iter().any(|item| {
            item.input_path == *path
                && !matches!(
                    item.status,
                    ConversionStatus::Completed { .. }
                        | ConversionStatus::Failed { .. }
                        | ConversionStatus::Cancelled
                )
        });

        if already_queued {
            outcome.skipped += 1;
            continue;
        }

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

        match app
            .manager
            .add_file_ready_for_processing(path.clone(), options.clone(), archive_pw)
        {
            Ok(_) => outcome.enqueued += 1,
            Err(_) => outcome.errors += 1,
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
}
