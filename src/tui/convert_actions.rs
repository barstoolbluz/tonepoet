//! Convert screen actions: build ConversionOptions from pills, add to queue, start conversion

use tokio::sync::mpsc;

use crate::config::TonepoetConfig;
use crate::convert::formats::{
    AacProfile, AudioFormat, ConversionOptions, Mp3BitrateMode, QualitySettings, WavPackMode,
};
use crate::convert::simple_wizard::ReplayGainMode;
use crate::convert::ConversionStatus;

use super::app::*;
use super::message::AppMessage;

/// Build ConversionOptions from current pill and pane state
pub fn pills_to_options(
    format: &FormatState,
    output_opts: &OutputOptionsState,
    config: &TonepoetConfig,
) -> ConversionOptions {
    let output_format = *format.format.selected_value();
    let target_sample_rate = *format.sample_rate.selected_value();
    let bit_depth = *format.bit_depth.selected_value();
    let dither = *format.dither.selected_value();
    let rg = *format.replaygain.selected_value();
    let merge = *output_opts.merge.selected_value();

    // Use backend bit depth for quality settings too (320 for float32)
    let backend_depth = bit_depth.to_backend_depth();

    // Build format-specific quality settings
    let quality = match output_format {
        AudioFormat::Flac => QualitySettings::Flac { compression_level: 5 },
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
    };

    // Map ReplayGain choice
    let (calculate_replaygain, replaygain_mode) = match rg {
        ReplayGainChoice::Album => (true, Some(ReplayGainMode::Album)),
        ReplayGainChoice::Track => (true, Some(ReplayGainMode::Track)),
        ReplayGainChoice::Both => (true, Some(ReplayGainMode::Both)),
        ReplayGainChoice::Off => (false, None),
    };

    // Dither: only for lossless formats. Lossy codecs handle their own quantization.
    let dither_type = if output_format.is_lossless() {
        Some(dither)
    } else {
        None
    };

    ConversionOptions {
        output_format,
        quality,
        target_sample_rate: Some(target_sample_rate),
        target_bit_depth: Some(backend_depth),
        dither_type,
        calculate_replaygain,
        replaygain_mode,
        output_dir: output_opts.dest_path.clone(),
        merge_to_single: matches!(merge, MergeMode::SingleImage),
        preserve_metadata: true,
        append_lineage_to_comment: config.conversion.append_lineage_to_comment,
        write_log_file: config.conversion.write_log_file,
        generate_cue_files: config.conversion.generate_cue_files,
        cue_generation_mode: config.conversion.cue_generation_mode.clone(),
        ..ConversionOptions::default()
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
    app.manager.clear_stop_request();

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
