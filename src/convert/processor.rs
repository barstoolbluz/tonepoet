//! Audio conversion processor with parallel execution

use super::formats::{Mp3BitrateMode, QualitySettings};
use super::{
    ConversionError, ConversionItem, ConversionPhase, ConversionQueue, ConversionResult,
    ConversionStatus,
};
use log::{info, warn};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

// Import conversion backend
use tonepoet_backend::convert_with_backend;

// Import conversion features (log files, cue files)
use tonepoet_features::{
    ConversionConfig as FeaturesConfig, ConversionResult as FeaturesResult,
    ConversionStatus as FeaturesStatus,
};

/// Check if a 32-bit audio file is float or integer using ffprobe
fn is_32bit_float(file_path: &Path) -> bool {
    use std::process::Command;

    let output = Command::new("ffprobe")
        .args(&[
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=sample_fmt",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(file_path)
        .output();

    if let Ok(output) = output {
        let fmt = String::from_utf8_lossy(&output.stdout);
        fmt.trim().starts_with("flt") // "flt" or "fltp" = float
    } else {
        // If ffprobe fails, assume float (safer default)
        true
    }
}

/// Detect audio bit depth from file metadata
/// Returns detected bit depth, or None if detection fails
/// For 32-bit files, distinguishes between integer (32) and float (320)
fn detect_audio_bit_depth(file_path: &Path) -> Option<u16> {
    use lofty::prelude::*;

    let tagged_file = lofty::read_from_path(file_path).ok()?;
    let depth = tagged_file.properties().bit_depth()?;

    // Special case: lofty returns 32 for both int and float
    // Use ffprobe to distinguish
    if depth == 32 {
        if is_32bit_float(file_path) {
            Some(320) // Maps to pcm_f32le
        } else {
            Some(32) // Maps to pcm_s32le
        }
    } else {
        Some(depth as u16)
    }
}

/// Detect comprehensive source file information
fn detect_source_info(file_path: &Path) -> Option<tonepoet_features::SourceInfo> {
    use lofty::file::FileType;
    use lofty::prelude::*;

    let tagged_file = lofty::read_from_path(file_path).ok()?;
    let props = tagged_file.properties();

    // Get format name
    let format = match tagged_file.file_type() {
        FileType::Flac => "FLAC",
        FileType::Wav => "WAV",
        FileType::Aiff => "AIFF",
        FileType::Mpeg => "MP3",
        FileType::Aac => "AAC",
        FileType::Opus => "Opus",
        FileType::WavPack => "WavPack",
        _ => "Unknown",
    }
    .to_string();

    // Get bit depth (reuse existing function)
    let bit_depth = detect_audio_bit_depth(file_path);

    Some(tonepoet_features::SourceInfo {
        format,
        bit_depth,
        sample_rate: props.sample_rate(),
        channels: props.channels(),
    })
}

/// Fix M4A ReplayGain atom names from uppercase (loudgain format) to lowercase (iTunes format)
/// loudgain writes: REPLAYGAIN_TRACK_GAIN
/// iTunes/AtomicParsley expects: replaygain_track_gain
async fn fix_m4a_replaygain_atom_names(m4a_file: &Path) -> Result<(), ConversionError> {
    use tonepoet_backend::{AacMetadataApplier, AacMetadataExtractor};

    // Extract metadata (this will read the uppercase atoms and map them correctly)
    let extractor = AacMetadataExtractor::new();
    let metadata = extractor.extract(m4a_file).map_err(|e| {
        ConversionError::ToolError(format!("Failed to extract M4A metadata: {}", e))
    })?;

    // Only proceed if we have ReplayGain tags
    let has_replaygain = metadata.custom_fields.contains_key("REPLAYGAIN_TRACK_GAIN")
        || metadata.custom_fields.contains_key("REPLAYGAIN_TRACK_PEAK")
        || metadata.custom_fields.contains_key("REPLAYGAIN_ALBUM_GAIN")
        || metadata.custom_fields.contains_key("REPLAYGAIN_ALBUM_PEAK");

    if !has_replaygain {
        return Ok(());
    }

    // Reapply metadata (this will write them with proper lowercase atom names)
    let applier = AacMetadataApplier::new();
    applier
        .apply(&metadata, m4a_file)
        .map_err(|e| ConversionError::ToolError(format!("Failed to apply M4A metadata: {}", e)))?;

    log::debug!(
        "Fixed M4A ReplayGain atom names for: {}",
        m4a_file.display()
    );
    Ok(())
}

/// Configuration for the conversion processor
pub struct ProcessorConfig {
    /// Number of parallel workers
    pub worker_count: usize,
    /// Paths to external tools
    pub tool_paths: HashMap<String, PathBuf>,
    /// Default destination directory for converted output (from app config)
    pub default_destination_directory: Option<PathBuf>,
    /// Local scratch directory for extraction (avoids FUSE overhead on NTFS)
    pub scratch_directory: Option<PathBuf>,
}

/// Handles the actual conversion of audio files
pub struct ConversionProcessor {
    config: ProcessorConfig,
    progress_tx: Option<broadcast::Sender<ProgressUpdate>>,
}

/// Progress update from a worker
#[derive(Debug, Clone)]
pub struct ProgressUpdate {
    pub item_id: String,
    pub progress: f32,
    pub status: ConversionStatus,
}

/// Helper to send phase progress updates
async fn send_phase_update(
    tx: &broadcast::Sender<ProgressUpdate>,
    item_id: &str,
    phase: ConversionPhase,
    phase_progress: f32,
    message: Option<String>,
    file_progress: Option<(u32, u32)>,
) {
    let overall_progress = phase.calculate_overall_progress(phase_progress);

    let _ = tx.send(ProgressUpdate {
        item_id: item_id.to_string(),
        progress: overall_progress,
        status: ConversionStatus::Processing {
            progress: overall_progress,
            message,
            file_progress,
            phase: Some(phase),
            phase_progress: Some(phase_progress),
        },
    });
}

fn terminal_progress_for_status(
    status: &ConversionStatus,
    last_known_progress: Option<f32>,
) -> f32 {
    match status {
        ConversionStatus::Processing { progress, .. } => *progress,
        ConversionStatus::Completed { .. } | ConversionStatus::Partial { .. } => 100.0,
        ConversionStatus::Failed { .. } | ConversionStatus::Cancelled => {
            last_known_progress.unwrap_or(0.0).clamp(0.0, 100.0)
        }
        ConversionStatus::Queued | ConversionStatus::Paused | ConversionStatus::NotConfigured => {
            0.0
        }
    }
}

impl ConversionProcessor {
    /// Create a new processor
    pub fn new(config: ProcessorConfig) -> Self {
        Self {
            config,
            progress_tx: None,
        }
    }

    /// Set progress channel
    pub fn set_progress_channel(&mut self, tx: broadcast::Sender<ProgressUpdate>) {
        self.progress_tx = Some(tx);
    }

    /// Process the conversion queue (backward compatibility)
    pub async fn process_queue(&mut self, queue: &mut ConversionQueue) -> ConversionResult<()> {
        // For backward compat, wrap in Arc<RwLock> temporarily
        let queue_arc = std::sync::Arc::new(tokio::sync::RwLock::new(std::mem::take(queue)));
        let result = self
            .process_queue_with_progress_arc(queue_arc.clone(), None)
            .await;
        // Move the queue back
        *queue = std::mem::take(&mut *queue_arc.write().await);
        result
    }

    /// Process the conversion queue with fine-grained locking
    /// Takes an Arc<RwLock<>> so it can lock/unlock as needed for UI updates
    pub async fn process_queue_with_progress(
        &mut self,
        queue: std::sync::Arc<tokio::sync::RwLock<ConversionQueue>>,
        progress_rx: Option<broadcast::Receiver<ProgressUpdate>>,
    ) -> ConversionResult<()> {
        self.process_queue_with_progress_arc(queue, progress_rx)
            .await
    }

    /// Internal implementation
    async fn process_queue_with_progress_arc(
        &mut self,
        queue: std::sync::Arc<tokio::sync::RwLock<ConversionQueue>>,
        mut progress_rx: Option<broadcast::Receiver<ProgressUpdate>>,
    ) -> ConversionResult<()> {
        info!("ConversionProcessor::process_queue starting");

        // Create a global semaphore to limit concurrent file conversions across all albums
        let file_semaphore = Arc::new(Semaphore::new(self.config.worker_count));

        // Read initial queue stats
        let (total_items, queued_count) = {
            let q = queue.read().await;
            (q.total_items(), q.queued_items().len())
        };
        info!(
            "Queue has {} total items, {} queued",
            total_items, queued_count
        );

        // Debug to file
        if let Ok(mut debug_file) = std::fs::OpenOptions::new()
            .append(true)
            .open("conversion-debug.log")
        {
            use std::io::Write;
            writeln!(debug_file, "\n[PROCESSOR START] process_queue called").ok();
            writeln!(debug_file, "  Queue total items: {}", total_items).ok();
            writeln!(debug_file, "  Queue queued items: {}", queued_count).ok();
            writeln!(
                debug_file,
                "  Progress channel set? {}",
                self.progress_tx.is_some()
            )
            .ok();
        }

        // Use existing UI channel if set, don't overwrite it
        let progress_tx = if let Some(tx) = &self.progress_tx {
            // Debug: Log that we're using existing channel
            if let Ok(mut debug_file) = std::fs::OpenOptions::new()
                .append(true)
                .open("conversion-debug.log")
            {
                use std::io::Write;
                writeln!(debug_file, "  ✓ Using existing progress channel (from UI)").ok();
            }
            tx.clone()
        } else {
            // Fallback: create local channel if no UI channel set
            if let Ok(mut debug_file) = std::fs::OpenOptions::new()
                .append(true)
                .open("conversion-debug.log")
            {
                use std::io::Write;
                writeln!(
                    debug_file,
                    "  ⚠ Creating new local progress channel (not connected to UI!)"
                )
                .ok();
            }
            let (tx, _rx) = broadcast::channel::<ProgressUpdate>(100);
            self.progress_tx = Some(tx.clone());
            tx
        };

        // Keep a local progress receiver so processor-level final status broadcasts
        // can preserve last-known progress even when the caller did not pass a
        // receiver. This does not write progress into the shared queue; it only
        // records the last broadcast percentage per item.
        progress_rx = progress_rx.or_else(|| Some(progress_tx.subscribe()));

        // Create worker pool
        let mut workers: JoinSet<ConversionResult<(String, ConversionStatus)>> = JoinSet::new();
        let mut last_progress_by_item: HashMap<String, f32> = HashMap::new();

        // Don't spawn local progress handler - let UI handle updates

        // Get queued items - use short lock
        let queued_items: Vec<_> = {
            let q = queue.read().await;
            q.queued_items()
                .into_iter()
                .take(self.config.worker_count)
                .cloned()
                .collect()
        };

        // Debug: Log items we're about to process
        if let Ok(mut debug_file) = std::fs::OpenOptions::new()
            .append(true)
            .open("conversion-debug.log")
        {
            use std::io::Write;
            writeln!(
                debug_file,
                "\n[PROCESSOR] Got {} items to process:",
                queued_items.len()
            )
            .ok();
            for item in &queued_items {
                writeln!(
                    debug_file,
                    "  Will process: id={}, path={}",
                    item.id,
                    item.input_path.display()
                )
                .ok();
            }
        }

        // Process items
        for (i, mut item) in queued_items.into_iter().enumerate() {
            // Apply default destination directory if item has no output_dir set
            if item.options.output_dir.is_none() {
                if let Some(ref default_dest) = self.config.default_destination_directory {
                    log::info!(
                        "Applying default destination to item {}: {:?}",
                        item.id,
                        default_dest
                    );
                    item.options.output_dir = Some(default_dest.clone());
                }
            }

            info!(
                "Worker {} processing item: {} ({:?})",
                i,
                item.input_path.display(),
                item.input_format
            );

            // Debug: Log marking as processing
            if let Ok(mut debug_file) = std::fs::OpenOptions::new()
                .append(true)
                .open("conversion-debug.log")
            {
                use std::io::Write;
                writeln!(
                    debug_file,
                    "\n[PROCESSOR] Worker {} starting item id={}",
                    i, item.id
                )
                .ok();
            }

            // Mark as processing in queue before starting - use short write lock
            {
                let mut q = queue.write().await;
                if let Some(queue_item) = q.find_item_mut(&item.id) {
                    queue_item.status = ConversionStatus::Processing {
                        progress: 0.0,
                        message: Some(format!("Starting conversion to {}", item.output_format)),
                        file_progress: None,
                        phase: Some(ConversionPhase::Extracting),
                        phase_progress: Some(0.0),
                    };

                    // Debug: Log successful marking
                    if let Ok(mut debug_file) = std::fs::OpenOptions::new()
                        .append(true)
                        .open("conversion-debug.log")
                    {
                        use std::io::Write;
                        writeln!(debug_file, "  ✓ Marked item as Processing in queue").ok();
                    }
                } else {
                    // Debug: Log failure to find
                    if let Ok(mut debug_file) = std::fs::OpenOptions::new()
                        .append(true)
                        .open("conversion-debug.log")
                    {
                        use std::io::Write;
                        writeln!(
                            debug_file,
                            "  ✗ Could not find item {} in queue to mark as Processing!",
                            item.id
                        )
                        .ok();
                    }
                }
            } // Write lock released here

            let progress_tx = progress_tx.clone();
            let tool_paths = self.config.tool_paths.clone();
            let item_id_for_err = item.id.clone();
            let file_semaphore = file_semaphore.clone();
            let scratch_dir = self.config.scratch_directory.clone();

            workers.spawn(async move {
                match process_item(item, progress_tx, tool_paths, file_semaphore, scratch_dir).await
                {
                    Ok(result) => Ok(result),
                    Err(e) => {
                        // Convert error into a Failed status so the queue item gets updated
                        Ok((
                            item_id_for_err,
                            ConversionStatus::Failed {
                                error: e.to_string(),
                                log_path: None,
                            },
                        ))
                    }
                }
            });
        }

        // Wait for all workers to complete, polling for progress updates
        loop {
            tokio::select! {
                // Drain progress updates to keep the broadcast channel flowing.
                // We do NOT write these to the shared queue — the UI handles
                // progress updates via its own broadcast → AppMessage pipeline.
                // Writing here caused write-lock contention that starved the UI's
                // try_read(), making the queue list appear empty during conversion.
                Ok(update) = async { if let Some(ref mut rx) = progress_rx { rx.recv().await } else { std::future::pending::<Result<ProgressUpdate, broadcast::error::RecvError>>().await } }, if progress_rx.is_some() => {
                    last_progress_by_item.insert(update.item_id.clone(), update.progress);
                    // Intentionally not writing to queue - UI handles this
                }

                // Wait for worker completion
                Some(result) = workers.join_next() => {
                    match result {
                        Ok(Ok((item_id, final_status))) => {
                            info!("Worker completed for item {}: {:?}",
                                item_id,
                                match &final_status {
                                    ConversionStatus::Completed { .. } => "Completed",
                                    ConversionStatus::Failed { .. } => "Failed",
                                    _ => "Other",
                                });
                            // Broadcast final status to UI so it renders the result
                            let progress = terminal_progress_for_status(&final_status, last_progress_by_item.get(&item_id).copied());
                            let _ = progress_tx.send(ProgressUpdate {
                                item_id: item_id.clone(),
                                progress,
                                status: final_status.clone(),
                            });
                            // Update queue with final status - use short write lock
                            let mut q = queue.write().await;
                            if let Some(queue_item) = q.find_item_mut(&item_id) {
                                queue_item.status = final_status;
                                queue_item.completed_at = Some(chrono::Utc::now());
                            }
                            // Lock released here
                        }
                        Ok(Err(e)) => {
                            log::error!("Conversion error: {}", e);
                        }
                        Err(e) => {
                            log::error!("Worker panic: {}", e);
                        }
                    }
                }

                // No more workers and no pending progress updates
                else => break,
            }

            // Get next queued item - use short lock
            let next_item = {
                let q = queue.read().await;
                q.queued_items().into_iter().take(1).cloned().next()
            };

            if let Some(mut item) = next_item {
                // Apply default destination directory if item has no output_dir set
                if item.options.output_dir.is_none() {
                    if let Some(ref default_dest) = self.config.default_destination_directory {
                        log::info!(
                            "Applying default destination to item {}: {:?}",
                            item.id,
                            default_dest
                        );
                        item.options.output_dir = Some(default_dest.clone());
                    }
                }

                // Mark as processing - use short write lock
                {
                    let mut q = queue.write().await;
                    if let Some(queue_item) = q.find_item_mut(&item.id) {
                        queue_item.status = ConversionStatus::Processing {
                            progress: 0.0,
                            message: Some(format!("Starting conversion to {}", item.output_format)),
                            file_progress: None,
                            phase: None,
                            phase_progress: None,
                        };
                    }
                } // Lock released here

                let progress_tx = progress_tx.clone();
                let tool_paths = self.config.tool_paths.clone();
                let item_id_for_err = item.id.clone();
                let file_semaphore = file_semaphore.clone();
                let scratch_dir = self.config.scratch_directory.clone();

                workers.spawn(async move {
                    match process_item(item, progress_tx, tool_paths, file_semaphore, scratch_dir)
                        .await
                    {
                        Ok(result) => Ok(result),
                        Err(e) => Ok((
                            item_id_for_err,
                            ConversionStatus::Failed {
                                error: e.to_string(),
                                log_path: None,
                            },
                        )),
                    }
                });
            } else if workers.is_empty() {
                // All workers done, no more queued items — drop the progress
                // receiver so the select branch is disabled and `else => break` fires
                progress_rx = None;
            }
        }

        Ok(())
    }
}

/// Copy FLAC file with full pipeline (rename, retag, ReplayGain)
async fn copy_flac_with_full_pipeline(
    item: &ConversionItem,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
) -> ConversionResult<PathBuf> {
    use crate::convert::simple_wizard::ReplayGainMode;
    use tokio::process::Command as TokioCommand;

    // Phase 1: Setup working directory (0-10%)
    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Extracting, // Reuse "Extracting" phase name
        0.0,
        Some("Setting up copy mode...".to_string()),
        None,
    )
    .await;

    let output_base = item
        .options
        .output_dir
        .as_ref()
        .map(|p| p.as_path())
        .unwrap_or_else(|| Path::new("."));

    let file_stem = item
        .input_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("flac_file");

    let working_dir = output_base.join(format!(".flac_copy_{}", file_stem));
    std::fs::create_dir_all(&working_dir).map_err(|e| {
        ConversionError::ConversionFailed(format!("Failed to create working dir: {}", e))
    })?;

    // Copy FLAC file to working directory
    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Extracting,
        50.0,
        Some("Copying FLAC file...".to_string()),
        None,
    )
    .await;

    let file_name = item
        .input_path
        .file_name()
        .ok_or_else(|| ConversionError::ConversionFailed("Invalid input filename".to_string()))?;
    let copied_file = working_dir.join(file_name);
    tokio::fs::copy(&item.input_path, &copied_file).await?;

    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Extracting,
        100.0,
        Some("Copy complete".to_string()),
        None,
    )
    .await;

    // Phase 2: Renaming (10-50%)
    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Renaming,
        0.0,
        Some("Analyzing folder structure...".to_string()),
        None,
    )
    .await;

    let audio_files = vec![&copied_file];
    let renamed_folder = match crate::convert::apply_folder_renaming(
        &working_dir,
        &audio_files,
        None,
        Some(&item.output_format),
    ) {
        Ok(folder) => {
            send_phase_update(
                progress_tx,
                &item.id,
                ConversionPhase::Renaming,
                50.0,
                Some("Folder renamed".to_string()),
                None,
            )
            .await;
            info!("Renamed folder to: {}", folder.display());
            folder
        }
        Err(e) => {
            warn!("Folder renaming failed: {}, using original path", e);
            working_dir.clone()
        }
    };

    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Renaming,
        70.0,
        Some("Renaming audio file...".to_string()),
        None,
    )
    .await;

    let renamed_files = match crate::convert::rename_audio_files(&renamed_folder) {
        Ok(files) => {
            send_phase_update(
                progress_tx,
                &item.id,
                ConversionPhase::Renaming,
                100.0,
                Some(format!("Renamed {} file(s)", files.len())),
                None,
            )
            .await;
            info!("Renamed {} audio file(s)", files.len());
            files
        }
        Err(e) => {
            warn!("File renaming failed: {}, using original file", e);
            vec![renamed_folder.join(file_name)]
        }
    };

    // Phase 3: Tagging (50-70%)
    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Tagging,
        10.0,
        Some("Updating metadata...".to_string()),
        None,
    )
    .await;

    match crate::convert::update_album_tags(&renamed_folder) {
        Ok(count) => {
            send_phase_update(
                progress_tx,
                &item.id,
                ConversionPhase::Tagging,
                50.0,
                Some(format!("Updated {} album tag(s)", count)),
                None,
            )
            .await;
            if count > 0 {
                info!("Updated album tags for {} file(s)", count);
            }
        }
        Err(e) => {
            warn!("Album tag update failed: {}", e);
        }
    }

    match crate::convert::update_title_tags(&renamed_folder) {
        Ok(count) => {
            send_phase_update(
                progress_tx,
                &item.id,
                ConversionPhase::Tagging,
                70.0,
                Some(format!("Updated {} title tag(s)", count)),
                None,
            )
            .await;
            if count > 0 {
                info!("Updated title tags for {} file(s)", count);
            }
        }
        Err(e) => {
            warn!("Title tag update failed: {}", e);
        }
    }

    // Lineage.txt metadata tagging (if enabled) - FLAC copy mode path
    if item.options.append_lineage_to_comment {
        // Look for Lineage.txt in the SOURCE file's parent directory, not the working directory
        let source_dir = item.input_path.parent();
        log::debug!(
            "Copy mode: Lineage feature ENABLED, searching in source directory: {:?}",
            source_dir
        );
        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::Tagging,
            75.0,
            Some("Appending Lineage.txt to COMMENT tags...".to_string()),
            None,
        )
        .await;

        // Look for Lineage.txt in the SOURCE directory (case-insensitive)
        let lineage_path = source_dir.and_then(|dir| {
            ["Lineage.txt", "lineage.txt", "LINEAGE.TXT"]
                .iter()
                .map(|name| dir.join(name))
                .find(|path| path.exists() && path.is_file())
        });

        if let Some(lineage_path) = lineage_path {
            info!("Found Lineage.txt, appending to COMMENT tags");

            // Apply to all FLAC files
            for file in &renamed_files {
                let result = TokioCommand::new("metaflac")
                    .arg(format!(
                        "--set-tag-from-file=COMMENT={}",
                        lineage_path.display()
                    ))
                    .arg(file)
                    .output()
                    .await;

                match result {
                    Ok(output) if output.status.success() => {
                        log::debug!("Applied Lineage.txt to {}", file.display());
                    }
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        warn!("metaflac failed for {}: {}", file.display(), stderr);
                    }
                    Err(e) => {
                        warn!("Failed to run metaflac for {}: {}", file.display(), e);
                    }
                }
            }

            send_phase_update(
                progress_tx,
                &item.id,
                ConversionPhase::Tagging,
                85.0,
                Some("Lineage.txt applied to COMMENT tags".to_string()),
                None,
            )
            .await;
        } else {
            log::debug!(
                "Lineage.txt not found in source directory: {:?}",
                source_dir.map(|d| d.display())
            );
        }
    }

    // Phase 4: ReplayGain (70-90%)
    if item.options.calculate_replaygain {
        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::PostProcessing,
            0.0,
            Some("Calculating ReplayGain...".to_string()),
            None,
        )
        .await;

        let mut args = vec![];

        // Mode selection
        match item.options.replaygain_mode.as_ref() {
            Some(ReplayGainMode::Album) => args.push("-a".to_string()),
            Some(ReplayGainMode::Track) => args.push("-r".to_string()),
            Some(ReplayGainMode::Both) => args.push("-a".to_string()), // Album mode calculates both
            None => {
                warn!("ReplayGain enabled but mode not specified, skipping");
            }
        }

        if !args.is_empty() {
            // Additional loudgain flags
            args.push("-k".to_string()); // Keep existing tags (noclip)
            args.push("-s".to_string()); // Tag mode
            args.push("i".to_string()); // Write ReplayGain 2.0 tags

            // Add files
            for file in &renamed_files {
                args.push(file.to_string_lossy().to_string());
            }

            // Execute loudgain
            let output = TokioCommand::new("loudgain")
                .args(&args)
                .output()
                .await
                .map_err(|e| {
                    ConversionError::ToolError(format!("Failed to run loudgain: {}", e))
                })?;

            if output.status.success() {
                // Fix M4A ReplayGain atom names (uppercase → lowercase)
                for file in &renamed_files {
                    if file.extension().map_or(false, |ext| ext == "m4a") {
                        if let Err(e) = fix_m4a_replaygain_atom_names(file).await {
                            log::warn!(
                                "Failed to fix M4A ReplayGain atom names for {}: {}",
                                file.display(),
                                e
                            );
                        }
                    }
                }

                send_phase_update(
                    progress_tx,
                    &item.id,
                    ConversionPhase::PostProcessing,
                    100.0,
                    Some("ReplayGain calculated".to_string()),
                    None,
                )
                .await;
                info!("ReplayGain tags applied successfully");
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!("loudgain failed: {}", stderr);
                send_phase_update(
                    progress_tx,
                    &item.id,
                    ConversionPhase::PostProcessing,
                    100.0,
                    Some("ReplayGain calculation failed (continuing)".to_string()),
                    None,
                )
                .await;
            }
        }
    }

    // Phase 5: Finalizing (90-100%)
    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Finalizing,
        100.0,
        Some("Copy mode complete".to_string()),
        None,
    )
    .await;

    Ok(renamed_folder)
}

/// Check if copy mode can be used (Tier 2: full pipeline with ReplayGain support)
/// Returns false if user requested re-encoding via checkbox
fn can_use_copy_mode(item: &ConversionItem) -> bool {
    use crate::convert::AudioFormat;
    use crate::convert::FileFormat;

    log::debug!("🔍 Checking copy mode eligibility:");
    log::debug!("  reencode_flac: {}", item.options.reencode_flac);
    log::debug!("  dither_type: {:?}", item.options.dither_type);
    log::debug!("  target_bit_depth: {:?}", item.options.target_bit_depth);
    log::debug!(
        "  target_sample_rate: {:?}",
        item.options.target_sample_rate
    );

    // User must NOT have requested re-encoding
    if item.options.reencode_flac {
        log::debug!("Copy mode skipped: Re-encode checkbox is checked");
        return false; // Re-encoding requested → don't use copy mode
    }

    // Must be FLAC → FLAC
    let input_audio_format = match item.input_format {
        FileFormat::Audio(fmt) => fmt,
        _ => return false, // Not audio (e.g., 7z archive)
    };

    if input_audio_format != AudioFormat::Flac {
        return false;
    }

    if item.output_format != AudioFormat::Flac {
        return false;
    }

    // Tier 2: ReplayGain is now supported in copy mode!

    // No resampling
    if item.options.target_sample_rate.is_some() && item.options.target_sample_rate != Some(0) {
        log::debug!("Copy mode skipped: Resampling requested");
        return false;
    }

    // No bit depth change
    if item.options.target_bit_depth.is_some() && item.options.target_bit_depth != Some(0) {
        log::debug!("Copy mode skipped: Bit depth change requested");
        return false;
    }

    // No dithering (only matters when bit depth is actually 16 or 24)
    if item.options.dither_type.is_some()
        && item.options.target_bit_depth.is_some()
        && (item.options.target_bit_depth == Some(16) || item.options.target_bit_depth == Some(24))
    {
        log::debug!("Copy mode skipped: Dithering requested");
        return false;
    }

    true
}

fn cue_capable_input_path(path: &Path) -> bool {
    const AUDIO_IMAGE_EXTENSIONS: &[&str] = &[
        "flac", "wav", "wave", "aiff", "aif", "aifc", "wv", "mp3", "m4a", "mp4", "aac", "opus",
        "ogg", "ape", "w64", "rf64",
    ];

    path.extension()
        .and_then(|value| value.to_str())
        .map(|ext| {
            let ext = ext.to_ascii_lowercase();
            ext == "cue" || AUDIO_IMAGE_EXTENSIONS.contains(&ext.as_str())
        })
        .unwrap_or(false)
}

fn pipeline_request_for_cue_item(
    item: &ConversionItem,
    fallback_cue_policy: crate::convert::pipeline::CueSidecarPolicy,
) -> crate::convert::pipeline::PipelineRequest {
    if let Some(req) = &item.pipeline_request {
        return req.clone();
    }

    use crate::convert::pipeline::{
        CueSidecarPolicy, DitherPolicy, EncodeBackend, EncodeOptions, FailurePolicy, LogPolicy,
        NamingCollisionPolicy, NamingPolicy, OverwritePolicy, PipelineRequest, PublishPolicy,
        SourceOptions, StagePolicy, StageRequirement, TrackSelection,
    };

    let output_root = item.options.output_dir.clone().unwrap_or_else(|| {
        item.input_path
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf()
    });

    PipelineRequest {
        job_id: format!("job-{}", item.id),
        item_id: item.id.clone(),
        container: item.input_path.clone(),
        source: SourceOptions {
            archive_password: None,
            sacd_area: None,
            cue_sidecar: match fallback_cue_policy {
                CueSidecarPolicy::PreferSidecar => CueSidecarPolicy::PreferSidecar,
                CueSidecarPolicy::SidecarOnly => CueSidecarPolicy::SidecarOnly,
                CueSidecarPolicy::EmbeddedOnly => CueSidecarPolicy::EmbeddedOnly,
                CueSidecarPolicy::IgnoreCue => CueSidecarPolicy::IgnoreCue,
            },
            track_selection: TrackSelection::All,
        },
        target_format: item.output_format,
        encode: EncodeOptions {
            backend: EncodeBackend::Auto,
            bitrate: match &item.options.quality {
                crate::convert::formats::QualitySettings::Opus { bitrate, .. } => Some(*bitrate),
                crate::convert::formats::QualitySettings::Aac { bitrate, .. } => Some(*bitrate),
                crate::convert::formats::QualitySettings::Mp3 { bitrate_mode, .. } => {
                    match bitrate_mode {
                        crate::convert::formats::Mp3BitrateMode::Cbr { bitrate } => Some(*bitrate),
                        crate::convert::formats::Mp3BitrateMode::Vbr { quality } => {
                            Some(*quality as u32)
                        }
                        crate::convert::formats::Mp3BitrateMode::Abr { bitrate } => Some(*bitrate),
                    }
                }
                _ => None,
            },
            compression_level: match &item.options.quality {
                crate::convert::formats::QualitySettings::Flac { compression_level } => {
                    Some(*compression_level)
                }
                _ => None,
            },
            dither: DitherPolicy::Auto,
        },
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
            collision_policy: NamingCollisionPolicy::Fail,
        },
        publish: PublishPolicy {
            overwrite: OverwritePolicy::FailIfExists,
            same_filesystem_required: false,
        },
        log: LogPolicy {
            root: output_root.join(".tonepoet-logs"),
            write_for_blocked: true,
            write_json_log: false,
        },
        stages: StagePolicy {
            metadata: StageRequirement::Enabled,
            replaygain: if item.options.calculate_replaygain {
                StageRequirement::Enabled
            } else {
                StageRequirement::Disabled
            },
            features: StageRequirement::Enabled,
            generate_cue: false,
        },
        failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
    }
}

fn cue_pipeline_policy_for_item(
    item: &ConversionItem,
) -> Result<
    Option<crate::convert::pipeline::CueSidecarPolicy>,
    crate::convert::pipeline::SourceDetectError,
> {
    use crate::convert::pipeline::{detect_source_kind, CueSidecarPolicy, SourceKind};

    if matches!(item.input_format, crate::convert::FileFormat::SevenZip) {
        return Ok(None);
    }

    let candidate_req = if let Some(req) = &item.pipeline_request {
        req.clone()
    } else if cue_capable_input_path(&item.input_path) {
        pipeline_request_for_cue_item(item, CueSidecarPolicy::PreferSidecar)
    } else {
        return Ok(None);
    };

    match candidate_req.source.cue_sidecar {
        CueSidecarPolicy::IgnoreCue => Ok(None),
        CueSidecarPolicy::SidecarOnly | CueSidecarPolicy::EmbeddedOnly => {
            Ok(Some(candidate_req.source.cue_sidecar))
        }
        CueSidecarPolicy::PreferSidecar => match detect_source_kind(&candidate_req) {
            Ok(SourceKind::CueImage) => Ok(Some(CueSidecarPolicy::PreferSidecar)),
            Ok(_) | Err(crate::convert::pipeline::SourceDetectError::UnknownSource) => Ok(None),
            Err(err) => Err(err),
        },
    }
}

fn sacd_capable_input_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("iso"))
        .unwrap_or(false)
}

fn pipeline_request_for_sacd_item(
    item: &ConversionItem,
) -> crate::convert::pipeline::PipelineRequest {
    if let Some(req) = &item.pipeline_request {
        return req.clone();
    }
    use crate::convert::pipeline::{
        CueSidecarPolicy, DitherPolicy, EncodeBackend, EncodeOptions, FailurePolicy, LogPolicy,
        NamingCollisionPolicy, NamingPolicy, OverwritePolicy, PipelineRequest, PublishPolicy,
        SourceOptions, StagePolicy, StageRequirement, TrackSelection,
    };

    let output_root = item.options.output_dir.clone().unwrap_or_else(|| {
        item.input_path
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf()
    });
    PipelineRequest {
        job_id: format!("job-{}", item.id),
        item_id: item.id.clone(),
        container: item.input_path.clone(),
        source: SourceOptions {
            archive_password: None,
            sacd_area: None,
            cue_sidecar: CueSidecarPolicy::IgnoreCue,
            track_selection: TrackSelection::All,
        },
        target_format: item.output_format,
        encode: EncodeOptions {
            backend: EncodeBackend::Auto,
            bitrate: match &item.options.quality {
                crate::convert::formats::QualitySettings::Opus { bitrate, .. } => Some(*bitrate),
                crate::convert::formats::QualitySettings::Aac { bitrate, .. } => Some(*bitrate),
                crate::convert::formats::QualitySettings::Mp3 { bitrate_mode, .. } => {
                    match bitrate_mode {
                        crate::convert::formats::Mp3BitrateMode::Cbr { bitrate } => Some(*bitrate),
                        crate::convert::formats::Mp3BitrateMode::Vbr { quality } => {
                            Some(*quality as u32)
                        }
                        crate::convert::formats::Mp3BitrateMode::Abr { bitrate } => Some(*bitrate),
                    }
                }
                _ => None,
            },
            compression_level: match &item.options.quality {
                crate::convert::formats::QualitySettings::Flac { compression_level } => {
                    Some(*compression_level)
                }
                _ => None,
            },
            dither: DitherPolicy::Auto,
        },
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
            collision_policy: NamingCollisionPolicy::Fail,
        },
        publish: PublishPolicy {
            overwrite: OverwritePolicy::FailIfExists,
            same_filesystem_required: false,
        },
        log: LogPolicy {
            root: output_root.join(".tonepoet-logs"),
            write_for_blocked: true,
            write_json_log: false,
        },
        stages: StagePolicy {
            metadata: StageRequirement::Enabled,
            replaygain: if item.options.calculate_replaygain {
                StageRequirement::Enabled
            } else {
                StageRequirement::Disabled
            },
            features: StageRequirement::Enabled,
            generate_cue: false,
        },
        failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
    }
}

fn sacd_pipeline_candidate_for_item(
    item: &ConversionItem,
) -> Result<bool, crate::convert::pipeline::SourceDetectError> {
    use crate::convert::pipeline::{detect_source_kind, SourceKind};

    let candidate_req = if let Some(req) = &item.pipeline_request {
        req.clone()
    } else if sacd_capable_input_path(&item.input_path) {
        pipeline_request_for_sacd_item(item)
    } else {
        return Ok(false);
    };

    match detect_source_kind(&candidate_req) {
        Ok(SourceKind::SacdIso) => Ok(true),
        Ok(_) | Err(crate::convert::pipeline::SourceDetectError::UnknownSource) => Ok(false),
        Err(err) => Err(err),
    }
}

async fn run_sacd_pipeline_conversion_item(
    item: &ConversionItem,
    progress_tx: broadcast::Sender<ProgressUpdate>,
    tool_paths: HashMap<String, PathBuf>,
) -> ConversionResult<(String, ConversionStatus)> {
    use crate::convert::pipeline::{
        map_album_outcome, run_pipeline_item_with_tool_paths, BroadcastReporter, RealToolRunner,
    };

    let pipeline_req = pipeline_request_for_sacd_item(item);
    let runner = RealToolRunner::new(tool_paths.clone());
    let reporter = BroadcastReporter::new(progress_tx.clone(), item.id.clone());
    let cancel = CancellationToken::new();
    let report =
        run_pipeline_item_with_tool_paths(pipeline_req, &runner, &reporter, &cancel, &tool_paths)
            .await;

    let status = map_album_outcome(
        &report.outcome,
        report.published.as_ref(),
        report.durable_log.as_deref(),
    );

    // Final pipeline status is emitted by BroadcastReporter.

    if let ConversionStatus::Failed { error, .. } = &status {
        return Ok((
            item.id.clone(),
            ConversionStatus::Failed {
                error: error.clone(),
                log_path: report.durable_log,
            },
        ));
    }

    Ok((item.id.clone(), status))
}

async fn run_cue_pipeline_conversion_item(
    item: &ConversionItem,
    progress_tx: broadcast::Sender<ProgressUpdate>,
    tool_paths: HashMap<String, PathBuf>,
    fallback_cue_policy: crate::convert::pipeline::CueSidecarPolicy,
) -> ConversionResult<(String, ConversionStatus)> {
    use crate::convert::pipeline::{
        map_album_outcome, run_pipeline_item_with_tool_paths, BroadcastReporter, RealToolRunner,
    };

    let pipeline_req = pipeline_request_for_cue_item(item, fallback_cue_policy);
    let runner = RealToolRunner::new(tool_paths.clone());
    let reporter = BroadcastReporter::new(progress_tx.clone(), item.id.clone());
    let cancel = CancellationToken::new();
    let report =
        run_pipeline_item_with_tool_paths(pipeline_req, &runner, &reporter, &cancel, &tool_paths)
            .await;

    let status = map_album_outcome(
        &report.outcome,
        report.published.as_ref(),
        report.durable_log.as_deref(),
    );

    // Final pipeline status is emitted by BroadcastReporter.

    if let ConversionStatus::Failed { error, .. } = &status {
        return Ok((
            item.id.clone(),
            ConversionStatus::Failed {
                error: error.clone(),
                log_path: report.durable_log,
            },
        ));
    }

    Ok((item.id.clone(), status))
}

async fn run_sevenzip_pipeline_conversion_item(
    item: &ConversionItem,
    progress_tx: broadcast::Sender<ProgressUpdate>,
    tool_paths: HashMap<String, PathBuf>,
) -> ConversionResult<(String, ConversionStatus)> {
    use crate::convert::pipeline::{
        map_album_outcome, run_pipeline_item_with_tool_paths, BroadcastReporter, CueSidecarPolicy,
        DitherPolicy, EncodeBackend, EncodeOptions, FailurePolicy, LogPolicy,
        NamingCollisionPolicy, NamingPolicy, OverwritePolicy, PipelineRequest, PublishPolicy,
        RealToolRunner, SecretString, SourceOptions, StagePolicy, StageRequirement, TrackSelection,
    };

    let pipeline_req = if let Some(req) = item.pipeline_request.clone() {
        req
    } else {
        let output_root = item.options.output_dir.clone().unwrap_or_else(|| {
            item.input_path
                .parent()
                .unwrap_or(Path::new("."))
                .to_path_buf()
        });
        PipelineRequest {
            job_id: format!("job-{}", item.id),
            item_id: item.id.clone(),
            container: item.input_path.clone(),
            source: SourceOptions {
                archive_password: item
                    .archive_password
                    .as_ref()
                    .map(|password| SecretString::new(password.clone())),
                sacd_area: None,
                cue_sidecar: CueSidecarPolicy::PreferSidecar,
                track_selection: TrackSelection::All,
            },
            target_format: item.output_format,
            encode: EncodeOptions {
                backend: EncodeBackend::Auto,
                bitrate: match &item.options.quality {
                    crate::convert::formats::QualitySettings::Opus { bitrate, .. } => {
                        Some(*bitrate)
                    }
                    crate::convert::formats::QualitySettings::Aac { bitrate, .. } => Some(*bitrate),
                    crate::convert::formats::QualitySettings::Mp3 { bitrate_mode, .. } => {
                        match bitrate_mode {
                            crate::convert::formats::Mp3BitrateMode::Cbr { bitrate }
                            | crate::convert::formats::Mp3BitrateMode::Abr { bitrate } => {
                                Some(*bitrate)
                            }
                            crate::convert::formats::Mp3BitrateMode::Vbr { quality } => {
                                Some(*quality as u32)
                            }
                        }
                    }
                    _ => None,
                },
                compression_level: match &item.options.quality {
                    crate::convert::formats::QualitySettings::Flac { compression_level } => {
                        Some(*compression_level)
                    }
                    _ => None,
                },
                dither: DitherPolicy::Auto,
            },
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
                collision_policy: NamingCollisionPolicy::Fail,
            },
            publish: PublishPolicy {
                overwrite: OverwritePolicy::FailIfExists,
                same_filesystem_required: false,
            },
            log: LogPolicy {
                root: output_root.join(".tonepoet-logs"),
                write_for_blocked: true,
                write_json_log: false,
            },
            stages: StagePolicy {
                metadata: StageRequirement::Enabled,
                replaygain: if item.options.calculate_replaygain {
                    StageRequirement::Enabled
                } else {
                    StageRequirement::Disabled
                },
                features: StageRequirement::Enabled,
                generate_cue: false,
            },
            failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
        }
    };

    let runner = RealToolRunner::new(tool_paths.clone());
    let reporter = BroadcastReporter::new(progress_tx, item.id.clone());
    let cancel = CancellationToken::new();
    let report =
        run_pipeline_item_with_tool_paths(pipeline_req, &runner, &reporter, &cancel, &tool_paths)
            .await;
    let status = map_album_outcome(
        &report.outcome,
        report.published.as_ref(),
        report.durable_log.as_deref(),
    );

    if let ConversionStatus::Failed { error, .. } = &status {
        return Ok((
            item.id.clone(),
            ConversionStatus::Failed {
                error: error.clone(),
                log_path: report.durable_log,
            },
        ));
    }

    Ok((item.id.clone(), status))
}

/// Process a single conversion item
pub async fn process_item(
    mut item: ConversionItem,
    progress_tx: broadcast::Sender<ProgressUpdate>,
    tool_paths: HashMap<String, PathBuf>,
    _file_semaphore: Arc<Semaphore>,
    _scratch_directory: Option<PathBuf>,
) -> ConversionResult<(String, ConversionStatus)> {
    // Return item_id and final status
    // Pre-flight: verify input file exists
    if !item.input_path.exists() {
        let error_msg = format!("Source file not found: {}", item.input_path.display());
        log::error!("{}", error_msg);
        return Ok((
            item.id.clone(),
            ConversionStatus::Failed {
                error: error_msg,
                log_path: None,
            },
        ));
    }

    // Update status to processing - 0%
    let _ = progress_tx.send(ProgressUpdate {
        item_id: item.id.clone(),
        progress: 0.0,
        status: ConversionStatus::Processing {
            progress: 0.0,
            message: Some(format!("Starting conversion to {}", item.output_format)),
            file_progress: None,
            phase: None,
            phase_progress: None,
        },
    });

    // Route staged pipeline jobs before legacy 10%/25% setup updates.
    // The staged reporter owns the full 0-100 stage-window model, so sending
    // legacy setup updates first would make the user-visible stream regress.
    if let Ok(true) = sacd_pipeline_candidate_for_item(&item) {
        return run_sacd_pipeline_conversion_item(&item, progress_tx.clone(), tool_paths.clone())
            .await;
    }

    match cue_pipeline_policy_for_item(&item) {
        Ok(Some(policy)) => {
            return run_cue_pipeline_conversion_item(
                &item,
                progress_tx.clone(),
                tool_paths.clone(),
                policy,
            )
            .await;
        }
        Ok(None) => {}
        Err(err) => {
            return Ok((
                item.id.clone(),
                ConversionStatus::Failed {
                    error: format!("CUE source detection failed: {err}"),
                    log_path: None,
                },
            ));
        }
    }

    if matches!(item.input_format, crate::convert::FileFormat::SevenZip) {
        return run_sevenzip_pipeline_conversion_item(
            &item,
            progress_tx.clone(),
            tool_paths.clone(),
        )
        .await;
    }

    // Determine output path - 10%
    let output_path = determine_output_path(&item)?;
    item.output_path = Some(output_path.clone());

    let _ = progress_tx.send(ProgressUpdate {
        item_id: item.id.clone(),
        progress: 10.0,
        status: ConversionStatus::Processing {
            progress: 10.0,
            message: Some("Preparing output file".to_string()),
            file_progress: None,
            phase: None,
            phase_progress: None,
        },
    });

    // Create output directory if needed
    if let Some(parent) = output_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Send 25% progress before starting actual conversion
    let _ = progress_tx.send(ProgressUpdate {
        item_id: item.id.clone(),
        progress: 25.0,
        status: ConversionStatus::Processing {
            progress: 25.0,
            message: Some(format!("Converting to {}", item.output_format)),
            file_progress: None,
            phase: None,
            phase_progress: None,
        },
    });

    // Route SACD ISO and single-image CUE albums through the staged pipeline
    // before the legacy one-file audio path. SACD routing comes first because
    // `.iso` has no legacy one-file audio semantics in this processor.
    match sacd_pipeline_candidate_for_item(&item) {
        Ok(true) => {
            return run_sacd_pipeline_conversion_item(
                &item,
                progress_tx.clone(),
                tool_paths.clone(),
            )
            .await;
        }
        Ok(false) => {}
        Err(err) => {
            return Ok((
                item.id.clone(),
                ConversionStatus::Failed {
                    error: format!("SACD source detection failed: {err}"),
                    log_path: None,
                },
            ));
        }
    }

    // Route single-image CUE albums through the staged pipeline before the
    // legacy one-file audio path. Explicit SidecarOnly and EmbeddedOnly
    // requests route here so missing CUE inputs fail in materialization;
    // IgnoreCue is the only policy that intentionally stays legacy.
    match cue_pipeline_policy_for_item(&item) {
        Ok(Some(policy)) => {
            return run_cue_pipeline_conversion_item(
                &item,
                progress_tx.clone(),
                tool_paths.clone(),
                policy,
            )
            .await;
        }
        Ok(None) => {}
        Err(err) => {
            return Ok((
                item.id.clone(),
                ConversionStatus::Failed {
                    error: format!("CUE source detection failed: {err}"),
                    log_path: None,
                },
            ));
        }
    }

    // Perform conversion based on format
    match &item.input_format {
        crate::convert::FileFormat::SevenZip => {
            // Route 7z archives through the new staged pipeline.
            use crate::convert::pipeline::{
                run_pipeline_item_with_tool_paths, BroadcastReporter, CueSidecarPolicy,
                DitherPolicy, EncodeBackend, EncodeOptions, FailurePolicy, LogPolicy,
                NamingCollisionPolicy, NamingPolicy, OverwritePolicy, PipelineRequest,
                PublishPolicy, RealToolRunner, SecretString, SourceOptions, StagePolicy,
                StageRequirement, TrackSelection,
            };

            // Use pre-built pipeline request if the CLI set one.
            let pipeline_req = if let Some(req) = item.pipeline_request.clone() {
                req
            } else {
                PipelineRequest {
                    job_id: format!("job-{}", item.id),
                    item_id: item.id.clone(),
                    container: item.input_path.clone(),
                    source: SourceOptions {
                        archive_password: item
                            .archive_password
                            .as_ref()
                            .map(|p| SecretString::new(p.clone())),
                        sacd_area: None,
                        cue_sidecar: CueSidecarPolicy::PreferSidecar,
                        track_selection: TrackSelection::All,
                    },
                    target_format: item.output_format,
                    encode: EncodeOptions {
                        backend: EncodeBackend::Auto,
                        bitrate: match &item.options.quality {
                            crate::convert::formats::QualitySettings::Opus { bitrate, .. } => {
                                Some(*bitrate)
                            }
                            crate::convert::formats::QualitySettings::Aac { bitrate, .. } => {
                                Some(*bitrate)
                            }
                            crate::convert::formats::QualitySettings::Mp3 {
                                bitrate_mode, ..
                            } => match bitrate_mode {
                                crate::convert::formats::Mp3BitrateMode::Cbr { bitrate } => {
                                    Some(*bitrate)
                                }
                                crate::convert::formats::Mp3BitrateMode::Vbr { quality } => {
                                    Some(*quality as u32)
                                }
                                crate::convert::formats::Mp3BitrateMode::Abr { bitrate } => {
                                    Some(*bitrate)
                                }
                            },
                            _ => None,
                        },
                        compression_level: match &item.options.quality {
                            crate::convert::formats::QualitySettings::Flac {
                                compression_level,
                            } => Some(*compression_level),
                            _ => None,
                        },
                        dither: DitherPolicy::Auto,
                    },
                    merge: item.options.merge_to_single,
                    output_root: item.options.output_dir.clone().unwrap_or_else(|| {
                        item.input_path
                            .parent()
                            .unwrap_or(Path::new("."))
                            .to_path_buf()
                    }),
                    naming: NamingPolicy {
                        template: item
                            .options
                            .naming_template
                            .clone()
                            .unwrap_or_else(|| "%NN% - %TITLE%".to_string()),
                        folder_template: item.options.folder_template.clone(),
                        per_album_subdir: true,
                        collision_policy: NamingCollisionPolicy::Fail,
                    },
                    publish: PublishPolicy {
                        overwrite: OverwritePolicy::FailIfExists,
                        same_filesystem_required: false,
                    },
                    log: LogPolicy {
                        root: item
                            .options
                            .output_dir
                            .clone()
                            .unwrap_or_else(|| {
                                item.input_path
                                    .parent()
                                    .unwrap_or(Path::new("."))
                                    .to_path_buf()
                            })
                            .join(".tonepoet-logs"),
                        write_for_blocked: true,
                        write_json_log: false,
                    },
                    stages: StagePolicy {
                        metadata: StageRequirement::Enabled,
                        replaygain: if item.options.calculate_replaygain {
                            StageRequirement::Enabled
                        } else {
                            StageRequirement::Disabled
                        },
                        features: StageRequirement::Enabled,
                        generate_cue: false,
                    },
                    failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
                }
            };

            let runner = RealToolRunner::new(tool_paths.clone());
            let reporter = BroadcastReporter::new(progress_tx.clone(), item.id.clone());
            let cancel = CancellationToken::new();

            let report = run_pipeline_item_with_tool_paths(
                pipeline_req,
                &runner,
                &reporter,
                &cancel,
                &tool_paths,
            )
            .await;

            // Map pipeline outcome to legacy status.
            use crate::convert::pipeline::map_album_outcome;
            let status = map_album_outcome(
                &report.outcome,
                report.published.as_ref(),
                report.durable_log.as_deref(),
            );

            match &status {
                ConversionStatus::Completed { .. } | ConversionStatus::Partial { .. } => {}
                ConversionStatus::Failed { error, .. } => {
                    return Ok((
                        item.id.clone(),
                        ConversionStatus::Failed {
                            error: error.clone(),
                            log_path: report.durable_log,
                        },
                    ));
                }
                _ => {}
            }

            // Send completion progress.
            // Final pipeline status is emitted by BroadcastReporter.

            return Ok((item.id.clone(), status));
        }
        crate::convert::FileFormat::Audio(_input_audio_format) => {
            // For audio files, we still use phases but skip extraction/renaming

            // Analyzing phase - quick file analysis
            send_phase_update(
                &progress_tx,
                &item.id,
                ConversionPhase::Analyzing,
                10.0,
                Some("Analyzing audio file...".to_string()),
                None,
            )
            .await;

            // Small delay to make phase visible
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

            send_phase_update(
                &progress_tx,
                &item.id,
                ConversionPhase::Analyzing,
                100.0,
                Some("Analysis complete".to_string()),
                None,
            )
            .await;

            // Jump to Converting phase (main work)
            send_phase_update(
                &progress_tx,
                &item.id,
                ConversionPhase::Converting,
                0.0,
                Some(format!("Starting conversion to {}...", item.output_format)),
                None,
            )
            .await;

            // Check if we can use fast copy mode (Tier 2: full pipeline with ReplayGain)
            if can_use_copy_mode(&item) {
                info!("🚀 Using FLAC copy mode with full pipeline (rename + retag + ReplayGain)");

                // Run complete copy pipeline
                let _output_folder = copy_flac_with_full_pipeline(&item, &progress_tx).await?;

                // Skip backend conversion entirely - copy mode handled everything
            } else {
                // Audio to audio conversion - Use conversion backend for all formats
                // Detect source info for this file to pass to backend
                let source_info = detect_source_info(&item.input_path);
                // Create conversion backend adapter item since backend has its own ConversionItem type
                let backend_item = create_backend_conversion_item(&item, source_info.as_ref());

                // Create progress adapter channel since backend uses different ProgressUpdate type
                let (backend_progress_tx, mut backend_progress_rx) =
                    mpsc::channel::<tonepoet_backend::integration::ProgressUpdate>(100);

                // Forward backend progress to main project progress format
                let main_progress_tx = progress_tx.clone();
                let _item_id_clone = item.id.clone();
                let progress_forwarder = tokio::spawn(async move {
                    while let Some(backend_update) = backend_progress_rx.recv().await {
                        // Map backend progress to main project format
                        let main_update = ProgressUpdate {
                            item_id: backend_update.item_id,
                            progress: backend_update.progress,
                            status: map_backend_status_to_main(backend_update.status),
                        };
                        let _ = main_progress_tx.send(main_update);
                    }
                });

                // Replace entire format-specific dispatch with single backend call
                let backend_result = convert_with_backend(
                    &backend_item,
                    &item.input_path,
                    &output_path,
                    &backend_progress_tx,
                    item.options.preferred_backend, // Use backend from wizard/preset
                )
                .await;

                // Wait for conversion to complete and cleanup progress forwarder
                let result = match backend_result {
                    Ok((_, _pipeline)) => {
                        // Conversion successful - backend handled the conversion
                        // (Pipeline ignored for individual file conversions - no logs generated)
                        Ok(())
                    }
                    Err(tonepoet_backend::ConversionError::InvalidSettings(msg)) => {
                        Err(ConversionError::ValidationError(msg))
                    }
                    Err(tonepoet_backend::ConversionError::UnsupportedFormat(msg)) => {
                        Err(ConversionError::UnsupportedFormat(msg))
                    }
                    Err(tonepoet_backend::ConversionError::BackendUnavailable(msg)) => {
                        Err(ConversionError::ToolError(msg))
                    }
                    Err(tonepoet_backend::ConversionError::Io(e)) => Err(ConversionError::Io(e)),
                };

                // Cleanup progress forwarder
                progress_forwarder.abort();

                result?;
            } // end else (backend conversion)
        }
    }

    // PostProcessing phase - ReplayGain is handled by the conversion backend if enabled
    if item.options.calculate_replaygain {
        send_phase_update(
            &progress_tx,
            &item.id,
            ConversionPhase::PostProcessing,
            50.0,
            Some("ReplayGain applied by conversion backend".to_string()),
            None,
        )
        .await;

        send_phase_update(
            &progress_tx,
            &item.id,
            ConversionPhase::PostProcessing,
            100.0,
            Some("Post-processing complete".to_string()),
            None,
        )
        .await;
    }

    // Finalizing phase
    send_phase_update(
        &progress_tx,
        &item.id,
        ConversionPhase::Finalizing,
        50.0,
        Some("Finalizing conversion...".to_string()),
        None,
    )
    .await;

    // Update status to completed - 100%
    let final_status = ConversionStatus::Completed {
        output_path: output_path.clone(),
        log_path: None,
    };
    let _ = progress_tx.send(ProgressUpdate {
        item_id: item.id.clone(),
        progress: 100.0,
        status: final_status.clone(),
    });

    // Write conversion log if enabled
    if item.options.write_log_file {
        use chrono::Utc;

        // Get file sizes
        let source_size = tokio::fs::metadata(&item.input_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        let output_size = tokio::fs::metadata(&output_path)
            .await
            .map(|m| m.len())
            .unwrap_or(0);

        let now = Utc::now();
        let result = FeaturesResult {
            source_file: item.input_path.clone(),
            output_file: output_path.clone(),
            status: FeaturesStatus::Success,
            source_size,
            output_size,
            start_time: now, // Approximate
            end_time: now,
            error_message: None,
            replaygain_values: None,
            source_info: None,
            conversion_pipeline: None,
        };

        let feature_config = FeaturesConfig {
            write_log_file: true,
            generate_cue_files: false, // Don't generate CUE for individual files
            cue_generation_mode: "IfMerging".to_string(),
            preferred_backend: "FFmpeg".to_string(),
            worker_count: 8,
            process_priority: 0,
            overwrite_behavior: "KeepBoth".to_string(),
        };

        if let Err(e) = tonepoet_features::write_conversion_log(
            output_path.parent().unwrap_or(&output_path),
            &[result],
            &feature_config,
            None,
        )
        .await
        {
            log::warn!("Failed to write conversion log for individual file: {}", e);
        }
    }

    Ok((item.id, final_status))
}

/// Determine output path for a conversion
fn determine_output_path(item: &ConversionItem) -> ConversionResult<PathBuf> {
    let input_path = &item.input_path;
    let output_ext = item.output_format.extension();

    // For now, simple implementation - same directory, different extension
    let mut output_path = input_path.with_extension(output_ext);

    // Handle overwrite
    if !item.options.overwrite && output_path.exists() {
        // Add number suffix
        let stem = input_path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| {
                ConversionError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Invalid filename",
                ))
            })?;

        let mut counter = 1;
        loop {
            output_path = input_path.with_file_name(format!("{}_{}.{}", stem, counter, output_ext));
            if !output_path.exists() {
                break;
            }
            counter += 1;
        }
    }

    Ok(output_path)
}

/// Create backend ConversionItem from main project ConversionItem
fn create_backend_conversion_item(
    item: &ConversionItem,
    source_info: Option<&tonepoet_features::SourceInfo>,
) -> tonepoet_backend::integration::ConversionItem {
    // Map main project types to backend types
    let output_format = match item.output_format {
        crate::convert::AudioFormat::Flac => tonepoet_backend::integration::MainAudioFormat::Flac,
        crate::convert::AudioFormat::Wav => tonepoet_backend::integration::MainAudioFormat::Wav,
        crate::convert::AudioFormat::Aiff => tonepoet_backend::integration::MainAudioFormat::Aiff,
        crate::convert::AudioFormat::WavPack => {
            tonepoet_backend::integration::MainAudioFormat::WavPack
        }
        crate::convert::AudioFormat::Mp3 => tonepoet_backend::integration::MainAudioFormat::Mp3,
        crate::convert::AudioFormat::Aac => tonepoet_backend::integration::MainAudioFormat::Aac,
        crate::convert::AudioFormat::Opus => tonepoet_backend::integration::MainAudioFormat::Opus,
        crate::convert::AudioFormat::Alac => tonepoet_backend::integration::MainAudioFormat::Alac,
    };

    // Map quality settings
    let quality = match &item.options.quality {
        QualitySettings::Flac { compression_level } => {
            tonepoet_backend::integration::MainQualitySettings::Flac {
                compression_level: *compression_level,
            }
        }
        QualitySettings::Wav {
            bit_depth,
            sample_rate,
        } => {
            // If bit_depth is 0 (same as source), detect from input file
            let actual_bit_depth = if *bit_depth == 0 {
                detect_audio_bit_depth(&item.input_path).unwrap_or(24)
            } else {
                *bit_depth
            };
            tonepoet_backend::integration::MainQualitySettings::Wav {
                bit_depth: actual_bit_depth,
                sample_rate: *sample_rate,
            }
        }
        QualitySettings::Aiff {
            bit_depth,
            sample_rate,
        } => {
            // If bit_depth is 0 (same as source), detect from input file
            let actual_bit_depth = if *bit_depth == 0 {
                detect_audio_bit_depth(&item.input_path).unwrap_or(24)
            } else {
                *bit_depth
            };
            tonepoet_backend::integration::MainQualitySettings::Aiff {
                bit_depth: actual_bit_depth,
                sample_rate: *sample_rate,
            }
        }
        QualitySettings::Mp3 {
            bitrate_mode,
            quality,
        } => {
            let mapped_bitrate_mode = match bitrate_mode {
                Mp3BitrateMode::Cbr { bitrate } => {
                    tonepoet_backend::integration::MainMp3BitrateMode::Cbr { bitrate: *bitrate }
                }
                Mp3BitrateMode::Vbr { quality } => {
                    tonepoet_backend::integration::MainMp3BitrateMode::Vbr { quality: *quality }
                }
                Mp3BitrateMode::Abr { bitrate } => {
                    tonepoet_backend::integration::MainMp3BitrateMode::Abr { bitrate: *bitrate }
                }
            };
            tonepoet_backend::integration::MainQualitySettings::Mp3 {
                bitrate_mode: mapped_bitrate_mode,
                quality: *quality,
            }
        }
        QualitySettings::Aac { bitrate, profile } => {
            let mapped_profile = match profile {
                super::formats::AacProfile::Lc => tonepoet_backend::integration::MainAacProfile::Lc,
                super::formats::AacProfile::He => tonepoet_backend::integration::MainAacProfile::He,
                super::formats::AacProfile::HeV2 => {
                    tonepoet_backend::integration::MainAacProfile::HeV2
                }
            };
            tonepoet_backend::integration::MainQualitySettings::Aac {
                bitrate: *bitrate,
                profile: mapped_profile,
            }
        }
        QualitySettings::Opus {
            bitrate,
            complexity,
        } => tonepoet_backend::integration::MainQualitySettings::Opus {
            bitrate: *bitrate,
            complexity: *complexity,
        },
        QualitySettings::WavPack {
            compression_mode,
            hybrid_mode,
            correction_file,
        } => {
            let mapped_mode = match compression_mode {
                super::formats::WavPackMode::Fast => {
                    tonepoet_backend::integration::MainWavPackMode::Fast
                }
                super::formats::WavPackMode::Normal => {
                    tonepoet_backend::integration::MainWavPackMode::Normal
                }
                super::formats::WavPackMode::High => {
                    tonepoet_backend::integration::MainWavPackMode::High
                }
                super::formats::WavPackMode::VeryHigh => {
                    tonepoet_backend::integration::MainWavPackMode::VeryHigh
                }
            };
            tonepoet_backend::integration::MainQualitySettings::WavPack {
                compression_mode: mapped_mode,
                hybrid_mode: *hybrid_mode,
                correction_file: *correction_file,
            }
        }
        QualitySettings::Alac => tonepoet_backend::integration::MainQualitySettings::Alac,
    };

    let options = tonepoet_backend::integration::MainConversionOptions {
        quality,
        calculate_replaygain: item.options.calculate_replaygain,
        replaygain_mode: item.options.replaygain_mode.clone().map(|mode| {
            use tonepoet_backend::integration::MainReplayGainMode;
            match mode {
                crate::convert::simple_wizard::ReplayGainMode::Track => MainReplayGainMode::Track,
                crate::convert::simple_wizard::ReplayGainMode::Album => MainReplayGainMode::Album,
                crate::convert::simple_wizard::ReplayGainMode::Both => MainReplayGainMode::Both,
            }
        }),
        overwrite: item.options.overwrite,
        resample_quality: item.options.resample_quality,
        dither_type: item.options.dither_type.map(|dt| {
            use tonepoet_backend::integration::MainDitherType;
            match dt {
                crate::convert::simple_wizard::DitherType::None => MainDitherType::None,
                crate::convert::simple_wizard::DitherType::TPDF => MainDitherType::TPDF,
                crate::convert::simple_wizard::DitherType::SloppedTPDF => {
                    MainDitherType::SloppedTPDF
                }
                crate::convert::simple_wizard::DitherType::Shibata => MainDitherType::Shibata,
                crate::convert::simple_wizard::DitherType::Lipshitz => MainDitherType::Lipshitz,
                crate::convert::simple_wizard::DitherType::FWeighted => MainDitherType::FWeighted,
                crate::convert::simple_wizard::DitherType::ModifiedEWeighted => {
                    MainDitherType::ModifiedEWeighted
                }
                crate::convert::simple_wizard::DitherType::ImprovedEWeighted => {
                    MainDitherType::ImprovedEWeighted
                }
                crate::convert::simple_wizard::DitherType::Gesemann => MainDitherType::Gesemann,
                crate::convert::simple_wizard::DitherType::LowShibata => MainDitherType::LowShibata,
                crate::convert::simple_wizard::DitherType::HighShibata => {
                    MainDitherType::HighShibata
                }
            }
        }),
        nyquist_transition: item.options.nyquist_transition.map(|nt| {
            use tonepoet_backend::integration::MainNyquistTransition;
            match nt {
                crate::convert::simple_wizard::NyquistTransition::Gentle => {
                    MainNyquistTransition::Gentle
                }
                crate::convert::simple_wizard::NyquistTransition::Steep => {
                    MainNyquistTransition::Steep
                }
                crate::convert::simple_wizard::NyquistTransition::BrickWall => {
                    MainNyquistTransition::BrickWall
                }
            }
        }),
        target_sample_rate: item.options.target_sample_rate,
        target_bit_depth: item.options.target_bit_depth,
        copy_auxiliary_files: item.options.copy_auxiliary_files,
        copy_subdirectories: item.options.copy_subdirectories,
        ssrc_insane_mode: item.options.ssrc_insane_mode,
        append_lineage_to_comment: item.options.append_lineage_to_comment,
    };

    tonepoet_backend::integration::ConversionItem {
        id: item.id.clone(),
        output_format,
        options,
        source_bit_depth: source_info.and_then(|info| info.bit_depth),
        source_sample_rate: source_info.and_then(|info| info.sample_rate),
        append_lineage: item.options.append_lineage_to_comment,
    }
}

/// Map backend ConversionStatus to main project ConversionStatus
fn map_backend_status_to_main(
    backend_status: tonepoet_backend::integration::ConversionStatus,
) -> ConversionStatus {
    match backend_status {
        tonepoet_backend::integration::ConversionStatus::NotConfigured => {
            ConversionStatus::NotConfigured
        }
        tonepoet_backend::integration::ConversionStatus::Queued => ConversionStatus::Queued,
        tonepoet_backend::integration::ConversionStatus::Processing {
            progress,
            message,
            file_progress,
            phase,
            phase_progress,
        } => {
            // Map backend phase to main project phase
            let main_phase = phase.map(|p| match p {
                tonepoet_backend::integration::ConversionPhase::Extracting => {
                    ConversionPhase::Extracting
                }
                tonepoet_backend::integration::ConversionPhase::Analyzing => {
                    ConversionPhase::Analyzing
                }
                tonepoet_backend::integration::ConversionPhase::Renaming => {
                    ConversionPhase::Renaming
                }
                tonepoet_backend::integration::ConversionPhase::Tagging => ConversionPhase::Tagging,
                tonepoet_backend::integration::ConversionPhase::Converting => {
                    ConversionPhase::Converting
                }
                tonepoet_backend::integration::ConversionPhase::PostProcessing => {
                    ConversionPhase::PostProcessing
                }
                tonepoet_backend::integration::ConversionPhase::Finalizing => {
                    ConversionPhase::Finalizing
                }
            });

            ConversionStatus::Processing {
                progress,
                message,
                file_progress,
                phase: main_phase,
                phase_progress,
            }
        }
        tonepoet_backend::integration::ConversionStatus::Completed { output_path } => {
            ConversionStatus::Completed {
                output_path,
                log_path: None,
            }
        }
        tonepoet_backend::integration::ConversionStatus::Failed { error } => {
            ConversionStatus::Failed {
                error,
                log_path: None,
            }
        }
        tonepoet_backend::integration::ConversionStatus::Paused => ConversionStatus::Paused,
        tonepoet_backend::integration::ConversionStatus::Cancelled => ConversionStatus::Cancelled,
    }
}
