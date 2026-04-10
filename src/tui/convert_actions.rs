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

/// Add current source to queue with pill-derived options, optionally start conversion.
/// Returns true if the action succeeded.
pub fn convert_or_queue(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    start: bool,
) -> bool {
    // Validate source file
    let source_path = match &app.convert.source.file_path {
        Some(p) => p.clone(),
        None => {
            app.set_status("No source file loaded. Press 'e' or :e <path>");
            return false;
        }
    };

    if app.convert.source.info.is_none() {
        app.set_status("Source file not probed. Try reloading with :e");
        return false;
    }

    // Check for duplicate: is this file already in the queue?
    let already_queued = app.manager.get_items_clone().iter().any(|item| {
        item.input_path == source_path
            && !matches!(
                item.status,
                ConversionStatus::Completed { .. }
                    | ConversionStatus::Failed { .. }
                    | ConversionStatus::Cancelled
            )
    });

    if already_queued {
        app.set_status("File already in queue. Switch to queue tab (4) to manage.");
        return false;
    }

    // Build options from pills
    let options = pills_to_options(
        &app.convert.format,
        &app.convert.output_options,
        &app.config,
    );

    let format_name = options.output_format.name();

    // Add to queue as ready-for-processing (status = Queued)
    match app.manager.add_file_ready_for_processing(source_path.clone(), options) {
        Ok(_) => {}
        Err(e) => {
            app.set_status(format!("Failed to queue: {}", e));
            return false;
        }
    }

    // Save queue if persistence is enabled
    app.manager
        .save_queue(app.config.conversion.persist_queue)
        .ok();

    let filename = source_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();

    if start {
        if app.processing_active {
            // Already processing — file was added to queue, it'll be picked up
            app.set_status(format!(
                "Queued: {} → {} (conversion active)",
                filename, format_name
            ));
        } else {
            app.set_status(format!("Converting: {} → {}", filename, format_name));
            start_processing(app, tx);
            // Note: start_processing may overwrite status if 0 items ready,
            // but that shouldn't happen since we just added one as Queued.
        }
    } else {
        app.set_status(format!("Queued: {} → {}", filename, format_name));
    }

    true
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
