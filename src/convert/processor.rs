//! Audio conversion processor with parallel execution

use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, broadcast, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use log::{info, warn};
use html_escape::decode_html_entities;
use super::{ConversionError, ConversionResult, ConversionQueue, ConversionStatus, ConversionItem, ConversionPhase};
use super::formats::{QualitySettings, Mp3BitrateMode, ConversionOptions};

// Import conversion backend
use tonepoet_backend::convert_with_backend;

// Import conversion features (log files, cue files)
use tonepoet_features::{
    post_conversion_features,
    ConversionConfig as FeaturesConfig,
    ConversionResult as FeaturesResult,
    ConversionStatus as FeaturesStatus,
    ReplayGainValues,
};

/// Normalize a file stem for matching: lowercase and normalize whitespace
/// Handles cases where renaming normalizes multiple spaces to single space
fn normalize_stem(stem: &str) -> String {
    stem.to_lowercase()
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
}

/// Check if a 32-bit audio file is float or integer using ffprobe
fn is_32bit_float(file_path: &Path) -> bool {
    use std::process::Command;

    let output = Command::new("ffprobe")
        .args(&[
            "-v", "error",
            "-select_streams", "a:0",
            "-show_entries", "stream=sample_fmt",
            "-of", "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(file_path)
        .output();

    if let Ok(output) = output {
        let fmt = String::from_utf8_lossy(&output.stdout);
        fmt.trim().starts_with("flt")  // "flt" or "fltp" = float
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
            Some(320)  // Maps to pcm_f32le
        } else {
            Some(32)   // Maps to pcm_s32le
        }
    } else {
        Some(depth as u16)
    }
}

/// Detect comprehensive source file information
fn detect_source_info(file_path: &Path) -> Option<tonepoet_features::SourceInfo> {
    use lofty::prelude::*;
    use lofty::file::FileType;

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
    }.to_string();

    // Get bit depth (reuse existing function)
    let bit_depth = detect_audio_bit_depth(file_path);

    Some(tonepoet_features::SourceInfo {
        format,
        bit_depth,
        sample_rate: props.sample_rate(),
        channels: props.channels(),
    })
}

/// Read ReplayGain tags from an audio file
/// Supports both traditional ReplayGain tags and R128 (EBU R128 loudness) tags
fn read_replaygain_tags(file_path: &Path) -> Option<ReplayGainValues> {
    use lofty::prelude::*;
    use lofty::tag::ItemKey;

    // Read the audio file
    let tagged_file = lofty::read_from_path(file_path).ok()?;

    // Get the primary tag, or fall back to the first available tag
    let tag = tagged_file.primary_tag().or_else(|| tagged_file.first_tag())?;

    // Try standard ReplayGain tags first
    let track_gain = tag.get_string(&ItemKey::ReplayGainTrackGain).map(String::from);
    let track_peak = tag.get_string(&ItemKey::ReplayGainTrackPeak).map(String::from);
    let album_gain = tag.get_string(&ItemKey::ReplayGainAlbumGain).map(String::from);
    let album_peak = tag.get_string(&ItemKey::ReplayGainAlbumPeak).map(String::from);

    // If standard tags found, return them
    if track_gain.is_some() || album_gain.is_some() {
        return Some(ReplayGainValues {
            track_gain,
            track_peak,
            album_gain,
            album_peak,
        });
    }

    // Fall back to R128 tags (EBU R128 loudness normalization)
    // R128 values are in hundredths of a dB (e.g., -2186 = -21.86 dB)
    // We need to iterate through items since ItemKey::Unknown may not work for all tag types
    let mut r128_track: Option<String> = None;
    let mut r128_album: Option<String> = None;

    for item in tag.items() {
        if let ItemKey::Unknown(ref key_str) = item.key() {
            match key_str.as_str() {
                "R128_TRACK_GAIN" => {
                    if let Some(text) = item.value().text() {
                        if let Ok(val) = text.parse::<i32>() {
                            r128_track = Some(format!("{:.2} dB", val as f64 / 100.0));
                        }
                    }
                }
                "R128_ALBUM_GAIN" => {
                    if let Some(text) = item.value().text() {
                        if let Ok(val) = text.parse::<i32>() {
                            r128_album = Some(format!("{:.2} dB", val as f64 / 100.0));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Return R128 values if found
    if r128_track.is_some() || r128_album.is_some() {
        return Some(ReplayGainValues {
            track_gain: r128_track,
            track_peak: None, // R128 doesn't have peak values
            album_gain: r128_album,
            album_peak: None,
        });
    }

    // No ReplayGain or R128 tags found
    None
}

/// Fix M4A ReplayGain atom names from uppercase (loudgain format) to lowercase (iTunes format)
/// loudgain writes: REPLAYGAIN_TRACK_GAIN
/// iTunes/AtomicParsley expects: replaygain_track_gain
async fn fix_m4a_replaygain_atom_names(m4a_file: &Path) -> Result<(), ConversionError> {
    use tonepoet_backend::{AacMetadataExtractor, AacMetadataApplier};

    // Extract metadata (this will read the uppercase atoms and map them correctly)
    let extractor = AacMetadataExtractor::new();
    let metadata = extractor.extract(m4a_file)
        .map_err(|e| ConversionError::ToolError(format!("Failed to extract M4A metadata: {}", e)))?;

    // Only proceed if we have ReplayGain tags
    let has_replaygain = metadata.custom_fields.contains_key("REPLAYGAIN_TRACK_GAIN") ||
                         metadata.custom_fields.contains_key("REPLAYGAIN_TRACK_PEAK") ||
                         metadata.custom_fields.contains_key("REPLAYGAIN_ALBUM_GAIN") ||
                         metadata.custom_fields.contains_key("REPLAYGAIN_ALBUM_PEAK");

    if !has_replaygain {
        return Ok(());
    }

    // Reapply metadata (this will write them with proper lowercase atom names)
    let applier = AacMetadataApplier::new();
    applier.apply(&metadata, m4a_file)
        .map_err(|e| ConversionError::ToolError(format!("Failed to apply M4A metadata: {}", e)))?;

    log::debug!("Fixed M4A ReplayGain atom names for: {}", m4a_file.display());
    Ok(())
}

/// Apply album metadata to merged audio file from source FLAC file
/// Extracts metadata from source and applies using FFmpeg
/// Supports MP3 and M4A formats
/// Does NOT apply comment field (reserved for lineage)
async fn apply_album_metadata_to_merged_file(
    output_file: &Path,
    source_file: &Path,
) -> Result<(), ConversionError> {
    use lofty::prelude::*;

    let format = output_file.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("unknown");

    log::info!("Applying album metadata to merged {} from: {}", format, source_file.display());

    // Extract metadata from source FLAC
    let tagged_file = lofty::read_from_path(source_file)
        .map_err(|e| ConversionError::ToolError(format!("Failed to read metadata from source: {}", e)))?;

    let tag = tagged_file.primary_tag()
        .or_else(|| tagged_file.first_tag())
        .ok_or_else(|| ConversionError::ToolError("No tags found in source file".to_string()))?;

    // Build FFmpeg command with metadata flags
    let mut args = vec![
        "-i".to_string(),
        output_file.to_string_lossy().to_string(),
    ];

    // Add standard metadata fields
    if let Some(artist) = tag.artist() {
        if !artist.is_empty() {
            args.push("-metadata".to_string());
            args.push(format!("artist={}", artist));
        }
    }

    if let Some(album) = tag.album() {
        if !album.is_empty() {
            args.push("-metadata".to_string());
            args.push(format!("album={}", album));

            // Use album name as title for merged file
            args.push("-metadata".to_string());
            args.push(format!("title={}", album));
        }
    }

    // Album artist
    use lofty::tag::ItemKey;
    if let Some(album_artist) = tag.get_string(&ItemKey::AlbumArtist) {
        if !album_artist.is_empty() {
            args.push("-metadata".to_string());
            args.push(format!("album_artist={}", album_artist));
        }
    }

    // Genre
    if let Some(genre) = tag.genre() {
        if !genre.is_empty() {
            args.push("-metadata".to_string());
            args.push(format!("genre={}", genre));
        }
    }

    // Year/Date
    if let Some(year) = tag.year() {
        args.push("-metadata".to_string());
        args.push(format!("date={}", year));
    }

    // NOTE: Do NOT apply comment field - reserved for lineage via pipeline

    // Output settings
    args.push("-c".to_string());
    args.push("copy".to_string());

    // Add format-specific settings
    if format == "mp3" {
        args.push("-id3v2_version".to_string());
        args.push("3".to_string());
    }

    args.push("-y".to_string());

    // Create temp file with proper extension so FFmpeg recognizes the format
    let temp_file = output_file.with_extension(format!("metadata_temp.{}", format));
    args.push(temp_file.to_string_lossy().to_string());

    // Execute FFmpeg
    use tokio::process::Command as TokioCommand;
    let output = TokioCommand::new("ffmpeg")
        .arg("-nostdin")
        .args(&args)
        .output()
        .await
        .map_err(|e| ConversionError::ToolError(format!("Failed to run ffmpeg: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ConversionError::ToolError(
            format!("FFmpeg metadata application failed: {}", stderr)
        ));
    }

    // Rename temp file to original
    tokio::fs::rename(&temp_file, output_file)
        .await
        .map_err(|e| ConversionError::Io(e))?;

    log::info!("✓ Applied metadata to merged {} file", format);
    Ok(())
}

/// Apply album metadata to merged Opus file from source FLAC file
/// Uses opustags for in-place modification
/// Does NOT apply comment field (reserved for lineage)
async fn apply_album_metadata_to_opus(
    opus_file: &Path,
    source_file: &Path,
) -> Result<(), ConversionError> {
    use lofty::prelude::*;

    log::info!("Applying album metadata to merged Opus from: {}", source_file.display());

    // Extract metadata from source FLAC
    let tagged_file = lofty::read_from_path(source_file)
        .map_err(|e| ConversionError::ToolError(format!("Failed to read metadata from source: {}", e)))?;

    let tag = tagged_file.primary_tag()
        .or_else(|| tagged_file.first_tag())
        .ok_or_else(|| ConversionError::ToolError("No tags found in source file".to_string()))?;

    // Build opustags command with multiple -s flags for each tag
    use tokio::process::Command as TokioCommand;
    let mut cmd = TokioCommand::new("opustags");
    cmd.arg("--in-place");

    // Delete existing tags first, then set new ones
    if let Some(artist) = tag.artist() {
        if !artist.is_empty() {
            cmd.arg("--delete").arg("ARTIST");
            cmd.arg("-s").arg(format!("ARTIST={}", artist));
        }
    }

    if let Some(album) = tag.album() {
        if !album.is_empty() {
            cmd.arg("--delete").arg("ALBUM");
            cmd.arg("-s").arg(format!("ALBUM={}", album));

            // Use album name as title for merged file
            cmd.arg("--delete").arg("TITLE");
            cmd.arg("-s").arg(format!("TITLE={}", album));
        }
    }

    // Album artist
    use lofty::tag::ItemKey;
    if let Some(album_artist) = tag.get_string(&ItemKey::AlbumArtist) {
        if !album_artist.is_empty() {
            cmd.arg("--delete").arg("ALBUMARTIST");
            cmd.arg("-s").arg(format!("ALBUMARTIST={}", album_artist));
        }
    }

    // Genre
    if let Some(genre) = tag.genre() {
        if !genre.is_empty() {
            cmd.arg("--delete").arg("GENRE");
            cmd.arg("-s").arg(format!("GENRE={}", genre));
        }
    }

    // Year/Date
    if let Some(year) = tag.year() {
        cmd.arg("--delete").arg("DATE");
        cmd.arg("-s").arg(format!("DATE={}", year));
    }

    cmd.arg(opus_file);

    // Execute opustags
    let output = cmd
        .output()
        .await
        .map_err(|e| ConversionError::ToolError(format!("Failed to run opustags: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ConversionError::ToolError(
            format!("opustags metadata application failed: {}", stderr)
        ));
    }

    log::info!("✓ Applied metadata to merged Opus file");
    Ok(())
}

/// Parse loudgain output (with -o flag) to extract album gain and peak
/// Output format: "Album\t0\t-1.86\t28959.000000\t0\t0"
/// Returns (gain_db, peak) as strings in proper format
fn parse_loudgain_album_output(stdout: &str) -> Option<(String, String)> {
    for line in stdout.lines() {
        if line.starts_with("Album\t") {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() >= 4 {
                // Field 2 is dB gain (e.g., "-1.86")
                let gain_db = fields[2].trim();

                // Field 3 is max amplitude (e.g., "28959.000000")
                // Need to normalize to 0-1 range (divide by 32768.0 for 16-bit)
                if let Ok(amplitude) = fields[3].trim().parse::<f64>() {
                    let peak = amplitude / 32768.0;

                    // Format: "+X.XX dB" for gain, "X.XXXXXX" for peak
                    let gain_str = if gain_db.starts_with('-') {
                        format!("{} dB", gain_db)
                    } else {
                        format!("+{} dB", gain_db)
                    };
                    let peak_str = format!("{:.6}", peak);

                    return Some((gain_str, peak_str));
                }
            }
        }
    }
    None
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
        let result = self.process_queue_with_progress_arc(queue_arc.clone(), None).await;
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
        self.process_queue_with_progress_arc(queue, progress_rx).await
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
        info!("Queue has {} total items, {} queued", total_items, queued_count);

        // Debug to file
        if let Ok(mut debug_file) = std::fs::OpenOptions::new()
            .append(true)
            .open("conversion-debug.log")
        {
            use std::io::Write;
            writeln!(debug_file, "\n[PROCESSOR START] process_queue called").ok();
            writeln!(debug_file, "  Queue total items: {}", total_items).ok();
            writeln!(debug_file, "  Queue queued items: {}", queued_count).ok();
            writeln!(debug_file, "  Progress channel set? {}", self.progress_tx.is_some()).ok();
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
                writeln!(debug_file, "  ⚠ Creating new local progress channel (not connected to UI!)").ok();
            }
            let (tx, _rx) = broadcast::channel::<ProgressUpdate>(100);
            self.progress_tx = Some(tx.clone());
            tx
        };

        // Create worker pool
        let mut workers: JoinSet<ConversionResult<(String, ConversionStatus)>> = JoinSet::new();

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
            writeln!(debug_file, "\n[PROCESSOR] Got {} items to process:", queued_items.len()).ok();
            for item in &queued_items {
                writeln!(debug_file, "  Will process: id={}, path={}", item.id, item.input_path.display()).ok();
            }
        }

        // Process items
        for (i, mut item) in queued_items.into_iter().enumerate() {
            // Apply default destination directory if item has no output_dir set
            if item.options.output_dir.is_none() {
                if let Some(ref default_dest) = self.config.default_destination_directory {
                    log::info!("Applying default destination to item {}: {:?}", item.id, default_dest);
                    item.options.output_dir = Some(default_dest.clone());
                }
            }

            info!("Worker {} processing item: {} ({:?})", i, item.input_path.display(), item.input_format);

            // Debug: Log marking as processing
            if let Ok(mut debug_file) = std::fs::OpenOptions::new()
                .append(true)
                .open("conversion-debug.log")
            {
                use std::io::Write;
                writeln!(debug_file, "\n[PROCESSOR] Worker {} starting item id={}", i, item.id).ok();
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
                        writeln!(debug_file, "  ✗ Could not find item {} in queue to mark as Processing!", item.id).ok();
                    }
                }
            } // Write lock released here

            let progress_tx = progress_tx.clone();
            let tool_paths = self.config.tool_paths.clone();
            let item_id_for_err = item.id.clone();
            let file_semaphore = file_semaphore.clone();
            let scratch_dir = self.config.scratch_directory.clone();

            workers.spawn(async move {
                match process_item(item, progress_tx, tool_paths, file_semaphore, scratch_dir).await {
                    Ok(result) => Ok(result),
                    Err(e) => {
                        // Convert error into a Failed status so the queue item gets updated
                        Ok((item_id_for_err, ConversionStatus::Failed { error: e.to_string(), log_path: None }))
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
                Ok(_update) = async {
                    if let Some(ref mut rx) = progress_rx {
                        rx.recv().await
                    } else {
                        std::future::pending::<Result<ProgressUpdate, broadcast::error::RecvError>>().await
                    }
                }, if progress_rx.is_some() => {
                    // Intentionally not writing to queue — UI handles this
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
                            let progress = match &final_status {
                                ConversionStatus::Completed { .. } => 100.0,
                                _ => 0.0,
                            };
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
                q.queued_items()
                    .into_iter()
                    .take(1)
                    .cloned()
                    .next()
            };

            if let Some(mut item) = next_item {
                // Apply default destination directory if item has no output_dir set
                if item.options.output_dir.is_none() {
                    if let Some(ref default_dest) = self.config.default_destination_directory {
                        log::info!("Applying default destination to item {}: {:?}", item.id, default_dest);
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
                    match process_item(item, progress_tx, tool_paths, file_semaphore, scratch_dir).await {
                        Ok(result) => Ok(result),
                        Err(e) => {
                            Ok((item_id_for_err, ConversionStatus::Failed { error: e.to_string(), log_path: None }))
                        }
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
    use tokio::process::Command as TokioCommand;
    use crate::convert::simple_wizard::ReplayGainMode;

    // Phase 1: Setup working directory (0-10%)
    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Extracting, // Reuse "Extracting" phase name
        0.0,
        Some("Setting up copy mode...".to_string()),
        None,
    ).await;

    let output_base = item.options.output_dir.as_ref()
        .map(|p| p.as_path())
        .unwrap_or_else(|| Path::new("."));

    let file_stem = item.input_path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("flac_file");

    let working_dir = output_base.join(format!(".flac_copy_{}", file_stem));
    std::fs::create_dir_all(&working_dir)
        .map_err(|e| ConversionError::ConversionFailed(format!("Failed to create working dir: {}", e)))?;

    // Copy FLAC file to working directory
    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Extracting,
        50.0,
        Some("Copying FLAC file...".to_string()),
        None,
    ).await;

    let file_name = item.input_path.file_name()
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
    ).await;

    // Phase 2: Renaming (10-50%)
    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Renaming,
        0.0,
        Some("Analyzing folder structure...".to_string()),
        None,
    ).await;

    let audio_files = vec![&copied_file];
    let renamed_folder = match crate::convert::apply_folder_renaming(
        &working_dir,
        &audio_files,
        None,
        Some(&item.output_format)
    ) {
        Ok(folder) => {
            send_phase_update(
                progress_tx,
                &item.id,
                ConversionPhase::Renaming,
                50.0,
                Some("Folder renamed".to_string()),
                None,
            ).await;
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
    ).await;

    let renamed_files = match crate::convert::rename_audio_files(&renamed_folder) {
        Ok(files) => {
            send_phase_update(
                progress_tx,
                &item.id,
                ConversionPhase::Renaming,
                100.0,
                Some(format!("Renamed {} file(s)", files.len())),
                None,
            ).await;
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
    ).await;

    match crate::convert::update_album_tags(&renamed_folder) {
        Ok(count) => {
            send_phase_update(
                progress_tx,
                &item.id,
                ConversionPhase::Tagging,
                50.0,
                Some(format!("Updated {} album tag(s)", count)),
                None,
            ).await;
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
            ).await;
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
        log::debug!("Copy mode: Lineage feature ENABLED, searching in source directory: {:?}", source_dir);
        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::Tagging,
            75.0,
            Some("Appending Lineage.txt to COMMENT tags...".to_string()),
            None,
        ).await;

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
                    .arg(format!("--set-tag-from-file=COMMENT={}", lineage_path.display()))
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
            ).await;
        } else {
            log::debug!("Lineage.txt not found in source directory: {:?}", source_dir.map(|d| d.display()));
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
        ).await;

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
            args.push("i".to_string());  // Write ReplayGain 2.0 tags

            // Add files
            for file in &renamed_files {
                args.push(file.to_string_lossy().to_string());
            }

            // Execute loudgain
            let output = TokioCommand::new("loudgain")
                .args(&args)
                .output()
                .await
                .map_err(|e| ConversionError::ToolError(format!("Failed to run loudgain: {}", e)))?;

            if output.status.success() {
                // Fix M4A ReplayGain atom names (uppercase → lowercase)
                for file in &renamed_files {
                    if file.extension().map_or(false, |ext| ext == "m4a") {
                        if let Err(e) = fix_m4a_replaygain_atom_names(file).await {
                            log::warn!("Failed to fix M4A ReplayGain atom names for {}: {}", file.display(), e);
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
                ).await;
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
                ).await;
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
    ).await;

    Ok(renamed_folder)
}

/// Check if copy mode can be used (Tier 2: full pipeline with ReplayGain support)
/// Returns false if user requested re-encoding via checkbox
fn can_use_copy_mode(item: &ConversionItem) -> bool {
    use crate::convert::FileFormat;
    use crate::convert::AudioFormat;

    log::debug!("🔍 Checking copy mode eligibility:");
    log::debug!("  reencode_flac: {}", item.options.reencode_flac);
    log::debug!("  dither_type: {:?}", item.options.dither_type);
    log::debug!("  target_bit_depth: {:?}", item.options.target_bit_depth);
    log::debug!("  target_sample_rate: {:?}", item.options.target_sample_rate);

    // User must NOT have requested re-encoding
    if item.options.reencode_flac {
        log::debug!("Copy mode skipped: Re-encode checkbox is checked");
        return false;  // Re-encoding requested → don't use copy mode
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
    if item.options.dither_type.is_some() &&
       item.options.target_bit_depth.is_some() &&
       (item.options.target_bit_depth == Some(16) || item.options.target_bit_depth == Some(24)) {
        log::debug!("Copy mode skipped: Dithering requested");
        return false;
    }

    true
}


fn cue_capable_input_path(path: &Path) -> bool {
    const AUDIO_IMAGE_EXTENSIONS: &[&str] = &[
        "flac", "wav", "wave", "aiff", "aif", "aifc", "wv", "mp3", "m4a", "mp4",
        "aac", "opus", "ogg", "ape", "w64", "rf64",
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
                crate::convert::formats::QualitySettings::Mp3 { bitrate_mode, .. } => match bitrate_mode {
                    crate::convert::formats::Mp3BitrateMode::Cbr { bitrate } => Some(*bitrate),
                    crate::convert::formats::Mp3BitrateMode::Vbr { quality } => Some(*quality as u32),
                    crate::convert::formats::Mp3BitrateMode::Abr { bitrate } => Some(*bitrate),
                },
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
            template: "%NN% - %TITLE%".to_string(),
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
        },
        stages: StagePolicy {
            metadata: StageRequirement::Enabled,
            replaygain: if item.options.calculate_replaygain {
                StageRequirement::Enabled
            } else {
                StageRequirement::Disabled
            },
            features: StageRequirement::Enabled,
        },
        failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
    }
}

fn cue_pipeline_policy_for_item(
    item: &ConversionItem,
) -> Result<Option<crate::convert::pipeline::CueSidecarPolicy>, crate::convert::pipeline::SourceDetectError> {
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
                        crate::convert::formats::Mp3BitrateMode::Vbr { quality } => Some(*quality as u32),
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
            template: "%NN% - %TITLE%".to_string(),
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
        },
        stages: StagePolicy {
            metadata: StageRequirement::Enabled,
            replaygain: if item.options.calculate_replaygain {
                StageRequirement::Enabled
            } else {
                StageRequirement::Disabled
            },
            features: StageRequirement::Enabled,
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
        map_album_outcome, run_pipeline_item, RealToolRunner, RecordingReporter,
    };

    let pipeline_req = pipeline_request_for_sacd_item(item);
    let runner = RealToolRunner::new(tool_paths);
    let reporter = RecordingReporter::new();
    let cancel = CancellationToken::new();
    let report = run_pipeline_item(pipeline_req, &runner, &reporter, &cancel).await;

    let status = map_album_outcome(
        &report.outcome,
        report.published.as_ref(),
        report.durable_log.as_deref(),
    );

    let _ = progress_tx.send(ProgressUpdate {
        item_id: item.id.clone(),
        progress: 100.0,
        status: status.clone(),
    });

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
    use crate::convert::pipeline::{map_album_outcome, RealToolRunner, RecordingReporter, run_pipeline_item};

    let pipeline_req = pipeline_request_for_cue_item(item, fallback_cue_policy);
    let runner = RealToolRunner::new(tool_paths);
    let reporter = RecordingReporter::new();
    let cancel = CancellationToken::new();
    let report = run_pipeline_item(pipeline_req, &runner, &reporter, &cancel).await;

    let status = map_album_outcome(
        &report.outcome,
        report.published.as_ref(),
        report.durable_log.as_deref(),
    );

    let _ = progress_tx.send(ProgressUpdate {
        item_id: item.id.clone(),
        progress: 100.0,
        status: status.clone(),
    });

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
    file_semaphore: Arc<Semaphore>,
    scratch_directory: Option<PathBuf>,
) -> ConversionResult<(String, ConversionStatus)> {  // Return item_id and final status
    // Pre-flight: verify input file exists
    if !item.input_path.exists() {
        let error_msg = format!("Source file not found: {}", item.input_path.display());
        log::error!("{}", error_msg);
        return Ok((item.id.clone(), ConversionStatus::Failed { error: error_msg, log_path: None }));
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
                RealToolRunner, RecordingReporter, PipelineRequest, SourceOptions,
                EncodeOptions, EncodeBackend, DitherPolicy, NamingPolicy,
                NamingCollisionPolicy, PublishPolicy, OverwritePolicy, LogPolicy,
                StagePolicy, StageRequirement, FailurePolicy, SecretString,
                CueSidecarPolicy, TrackSelection, run_pipeline_item,
            };

            let pipeline_req = PipelineRequest {
                job_id: format!("job-{}", item.id),
                item_id: item.id.clone(),
                container: item.input_path.clone(),
                source: SourceOptions {
                    archive_password: item.archive_password.as_ref().map(|p| SecretString::new(p.clone())),
                    sacd_area: None,
                    cue_sidecar: CueSidecarPolicy::PreferSidecar,
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
                                crate::convert::formats::Mp3BitrateMode::Vbr { quality } => Some(*quality as u32),
                                crate::convert::formats::Mp3BitrateMode::Abr { bitrate } => Some(*bitrate),
                            }
                        }
                        _ => None,
                    },
                    compression_level: match &item.options.quality {
                        crate::convert::formats::QualitySettings::Flac { compression_level } => Some(*compression_level),
                        _ => None,
                    },
                    dither: DitherPolicy::Auto,
                },
                merge: item.options.merge_to_single,
                output_root: item.options.output_dir.clone()
                    .unwrap_or_else(|| item.input_path.parent()
                        .unwrap_or(Path::new("."))
                        .to_path_buf()),
                naming: NamingPolicy {
                    template: "%NN% - %TITLE%".to_string(),
                    per_album_subdir: true,
                    collision_policy: NamingCollisionPolicy::Fail,
                },
                publish: PublishPolicy {
                    overwrite: OverwritePolicy::FailIfExists,
                    same_filesystem_required: false,
                },
                log: LogPolicy {
                    root: item.options.output_dir.clone()
                        .unwrap_or_else(|| item.input_path.parent()
                            .unwrap_or(Path::new("."))
                            .to_path_buf())
                        .join(".tonepoet-logs"),
                    write_for_blocked: true,
                },
                stages: StagePolicy {
                    metadata: StageRequirement::Enabled,
                    replaygain: if item.options.calculate_replaygain {
                        StageRequirement::Enabled
                    } else {
                        StageRequirement::Disabled
                    },
                    features: StageRequirement::Enabled,
                },
                failure_policy: FailurePolicy::FailAlbumOnAnyTrackFailure,
            };

            let runner = RealToolRunner::new(tool_paths);
            let reporter = RecordingReporter::new();
            let cancel = CancellationToken::new();

            let report = run_pipeline_item(pipeline_req, &runner, &reporter, &cancel).await;

            // Map pipeline outcome to legacy status.
            use crate::convert::pipeline::{AlbumOutcome, map_album_outcome};
            let status = map_album_outcome(
                &report.outcome,
                report.published.as_ref(),
                report.durable_log.as_deref(),
            );

            match &status {
                ConversionStatus::Completed { .. } | ConversionStatus::Partial { .. } => {}
                ConversionStatus::Failed { error, .. } => {
                    return Ok((item.id.clone(), ConversionStatus::Failed {
                        error: error.clone(),
                        log_path: report.durable_log,
                    }));
                }
                _ => {}
            }

            // Send completion progress.
            let _ = progress_tx.send(ProgressUpdate {
                item_id: item.id.clone(),
                progress: 100.0,
                status: status.clone(),
            });

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
            ).await;
            
            // Small delay to make phase visible
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
            
            send_phase_update(
                &progress_tx,
                &item.id,
                ConversionPhase::Analyzing,
                100.0,
                Some("Analysis complete".to_string()),
                None,
            ).await;
            
            // Jump to Converting phase (main work)
            send_phase_update(
                &progress_tx,
                &item.id,
                ConversionPhase::Converting,
                0.0,
                Some(format!("Starting conversion to {}...", item.output_format)),
                None,
            ).await;

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
            let (backend_progress_tx, mut backend_progress_rx) = mpsc::channel::<tonepoet_backend::integration::ProgressUpdate>(100);
            
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
                item.options.preferred_backend // Use backend from wizard/preset
            ).await;

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
                Err(tonepoet_backend::ConversionError::Io(e)) => {
                    Err(ConversionError::Io(e))
                }
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
        ).await;
        
        send_phase_update(
            &progress_tx,
            &item.id,
            ConversionPhase::PostProcessing,
            100.0,
            Some("Post-processing complete".to_string()),
            None,
        ).await;
    }
    
    // Finalizing phase
    send_phase_update(
        &progress_tx,
        &item.id,
        ConversionPhase::Finalizing,
        50.0,
        Some("Finalizing conversion...".to_string()),
        None,
    ).await;
    
    // Update status to completed - 100%
    let final_status = ConversionStatus::Completed { output_path: output_path.clone(), log_path: None };
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
            start_time: now,  // Approximate
            end_time: now,
            error_message: None,
            replaygain_values: None,
            source_info: None,
            conversion_pipeline: None,
        };

        let feature_config = FeaturesConfig {
            write_log_file: true,
            generate_cue_files: false,  // Don't generate CUE for individual files
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
        ).await {
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
        let stem = input_path.file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| ConversionError::Io(
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid filename")
            ))?;
        
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
        crate::convert::AudioFormat::WavPack => tonepoet_backend::integration::MainAudioFormat::WavPack,
        crate::convert::AudioFormat::Mp3 => tonepoet_backend::integration::MainAudioFormat::Mp3,
        crate::convert::AudioFormat::Aac => tonepoet_backend::integration::MainAudioFormat::Aac,
        crate::convert::AudioFormat::Opus => tonepoet_backend::integration::MainAudioFormat::Opus,
        crate::convert::AudioFormat::Alac => tonepoet_backend::integration::MainAudioFormat::Alac,
    };
    
    // Map quality settings
    let quality = match &item.options.quality {
        QualitySettings::Flac { compression_level } => {
            tonepoet_backend::integration::MainQualitySettings::Flac { compression_level: *compression_level }
        }
        QualitySettings::Wav { bit_depth, sample_rate } => {
            // If bit_depth is 0 (same as source), detect from input file
            let actual_bit_depth = if *bit_depth == 0 {
                detect_audio_bit_depth(&item.input_path).unwrap_or(24)
            } else {
                *bit_depth
            };
            tonepoet_backend::integration::MainQualitySettings::Wav { bit_depth: actual_bit_depth, sample_rate: *sample_rate }
        }
        QualitySettings::Aiff { bit_depth, sample_rate } => {
            // If bit_depth is 0 (same as source), detect from input file
            let actual_bit_depth = if *bit_depth == 0 {
                detect_audio_bit_depth(&item.input_path).unwrap_or(24)
            } else {
                *bit_depth
            };
            tonepoet_backend::integration::MainQualitySettings::Aiff { bit_depth: actual_bit_depth, sample_rate: *sample_rate }
        }
        QualitySettings::Mp3 { bitrate_mode, quality } => {
            let mapped_bitrate_mode = match bitrate_mode {
                Mp3BitrateMode::Cbr { bitrate } => tonepoet_backend::integration::MainMp3BitrateMode::Cbr { bitrate: *bitrate },
                Mp3BitrateMode::Vbr { quality } => tonepoet_backend::integration::MainMp3BitrateMode::Vbr { quality: *quality },
                Mp3BitrateMode::Abr { bitrate } => tonepoet_backend::integration::MainMp3BitrateMode::Abr { bitrate: *bitrate },
            };
            tonepoet_backend::integration::MainQualitySettings::Mp3 { bitrate_mode: mapped_bitrate_mode, quality: *quality }
        }
        QualitySettings::Aac { bitrate, profile } => {
            let mapped_profile = match profile {
                super::formats::AacProfile::Lc => tonepoet_backend::integration::MainAacProfile::Lc,
                super::formats::AacProfile::He => tonepoet_backend::integration::MainAacProfile::He,
                super::formats::AacProfile::HeV2 => tonepoet_backend::integration::MainAacProfile::HeV2,
            };
            tonepoet_backend::integration::MainQualitySettings::Aac { bitrate: *bitrate, profile: mapped_profile }
        }
        QualitySettings::Opus { bitrate, complexity } => {
            tonepoet_backend::integration::MainQualitySettings::Opus { bitrate: *bitrate, complexity: *complexity }
        }
        QualitySettings::WavPack { compression_mode, hybrid_mode, correction_file } => {
            let mapped_mode = match compression_mode {
                super::formats::WavPackMode::Fast => tonepoet_backend::integration::MainWavPackMode::Fast,
                super::formats::WavPackMode::Normal => tonepoet_backend::integration::MainWavPackMode::Normal,
                super::formats::WavPackMode::High => tonepoet_backend::integration::MainWavPackMode::High,
                super::formats::WavPackMode::VeryHigh => tonepoet_backend::integration::MainWavPackMode::VeryHigh,
            };
            tonepoet_backend::integration::MainQualitySettings::WavPack {
                compression_mode: mapped_mode,
                hybrid_mode: *hybrid_mode,
                correction_file: *correction_file
            }
        }
        QualitySettings::Alac => {
            tonepoet_backend::integration::MainQualitySettings::Alac
        }
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
                crate::convert::simple_wizard::DitherType::SloppedTPDF => MainDitherType::SloppedTPDF,
                crate::convert::simple_wizard::DitherType::Shibata => MainDitherType::Shibata,
                crate::convert::simple_wizard::DitherType::Lipshitz => MainDitherType::Lipshitz,
                crate::convert::simple_wizard::DitherType::FWeighted => MainDitherType::FWeighted,
                crate::convert::simple_wizard::DitherType::ModifiedEWeighted => MainDitherType::ModifiedEWeighted,
                crate::convert::simple_wizard::DitherType::ImprovedEWeighted => MainDitherType::ImprovedEWeighted,
                crate::convert::simple_wizard::DitherType::Gesemann => MainDitherType::Gesemann,
                crate::convert::simple_wizard::DitherType::LowShibata => MainDitherType::LowShibata,
                crate::convert::simple_wizard::DitherType::HighShibata => MainDitherType::HighShibata,
            }
        }),
        nyquist_transition: item.options.nyquist_transition.map(|nt| {
            use tonepoet_backend::integration::MainNyquistTransition;
            match nt {
                crate::convert::simple_wizard::NyquistTransition::Gentle => MainNyquistTransition::Gentle,
                crate::convert::simple_wizard::NyquistTransition::Steep => MainNyquistTransition::Steep,
                crate::convert::simple_wizard::NyquistTransition::BrickWall => MainNyquistTransition::BrickWall,
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
fn map_backend_status_to_main(backend_status: tonepoet_backend::integration::ConversionStatus) -> ConversionStatus {
    match backend_status {
        tonepoet_backend::integration::ConversionStatus::NotConfigured => ConversionStatus::NotConfigured,
        tonepoet_backend::integration::ConversionStatus::Queued => ConversionStatus::Queued,
        tonepoet_backend::integration::ConversionStatus::Processing { progress, message, file_progress, phase, phase_progress } => {
            // Map backend phase to main project phase
            let main_phase = phase.map(|p| match p {
                tonepoet_backend::integration::ConversionPhase::Extracting => ConversionPhase::Extracting,
                tonepoet_backend::integration::ConversionPhase::Analyzing => ConversionPhase::Analyzing,
                tonepoet_backend::integration::ConversionPhase::Renaming => ConversionPhase::Renaming,
                tonepoet_backend::integration::ConversionPhase::Tagging => ConversionPhase::Tagging,
                tonepoet_backend::integration::ConversionPhase::Converting => ConversionPhase::Converting,
                tonepoet_backend::integration::ConversionPhase::PostProcessing => ConversionPhase::PostProcessing,
                tonepoet_backend::integration::ConversionPhase::Finalizing => ConversionPhase::Finalizing,
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
            ConversionStatus::Completed { output_path, log_path: None }
        }
        tonepoet_backend::integration::ConversionStatus::Failed { error } => {
            ConversionStatus::Failed { error, log_path: None }
        }
        tonepoet_backend::integration::ConversionStatus::Paused => ConversionStatus::Paused,
        tonepoet_backend::integration::ConversionStatus::Cancelled => ConversionStatus::Cancelled,
    }
}

/// Extract track number from filename (e.g., "01 - Title.flac" -> 1)
fn extract_track_number(path: &Path) -> u32 {
    path.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| {
            s.split('-')
                .next()
                .and_then(|num| num.trim().parse::<u32>().ok())
        })
        .unwrap_or(0)
}

/// Check available disk space on the filesystem containing the given path
fn check_available_disk_space(path: &Path) -> Option<u64> {
    // Get filesystem stats
    let path_str = path.to_string_lossy();
    let output = std::process::Command::new("df")
        .arg("-k") // Output in KB
        .arg(&*path_str)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();

    if lines.len() < 2 {
        return None;
    }

    // Parse second line: Filesystem 1K-blocks Used Available Use% Mounted
    let fields: Vec<&str> = lines[1].split_whitespace().collect();
    if fields.len() < 4 {
        return None;
    }

    // Available is in KB (field index 3)
    fields[3].parse::<u64>().ok().map(|kb| kb * 1024) // Convert to bytes
}

/// Check if processing (SSRC/dither) is needed based on conversion options
fn needs_processing(
    options: &ConversionOptions,
    current_sample_rate: u32,
    current_bit_depth: Option<u16>,
) -> bool {
    // Check if sample rate conversion is needed
    let needs_resampling = match options.target_sample_rate {
        Some(0) | None => false, // "Same as source" or not specified
        Some(target) => target != current_sample_rate,
    };

    // Check if bit depth conversion is needed
    let needs_bit_depth_change = match (options.target_bit_depth, current_bit_depth) {
        (Some(0), _) | (None, _) => false, // "Same as source" or not specified
        (Some(target), Some(current)) => (target as u16) != current,
        (Some(_target), None) => true, // Target specified but current unknown
    };

    // Check if dithering is needed
    let needs_dither = options.dither_type.is_some()
        && matches!(options.target_bit_depth, Some(16) | Some(24));

    // Processing needed if any operation required
    let processing_needed = needs_resampling || needs_bit_depth_change || needs_dither;

    log::debug!("Processing check: resampling={}, bit_depth={}, dither={} → {}",
        needs_resampling, needs_bit_depth_change, needs_dither, processing_needed);

    processing_needed
}

/// Extract album metadata (artist, title, year) from audio file tags
/// Falls back to "Unknown" values if tags are missing or unreadable
/// Applies proper capitalization to artist and album names
fn extract_album_metadata_from_tags(file_path: &Path) -> (String, String, Option<String>) {
    use lofty::prelude::*;

    if let Ok(tagged_file) = lofty::read_from_path(file_path) {
        if let Some(tag) = tagged_file.primary_tag().or_else(|| tagged_file.first_tag()) {
            let raw_artist = tag.artist()
                .and_then(|c| if c.is_empty() { None } else { Some(c.to_string()) })
                .unwrap_or_else(|| "Unknown Artist".to_string());

            let raw_album = tag.album()
                .and_then(|c| if c.is_empty() { None } else { Some(c.to_string()) })
                .unwrap_or_else(|| "Unknown Album".to_string());

            let year = tag.year().map(|y| y.to_string());

            // Apply capitalization (handles prepositions, articles, etc.)
            let artist = crate::convert::renaming::capitalize_section(&raw_artist);
            let album = crate::convert::renaming::capitalize_section(&raw_album);

            return (artist, album, year);
        }
    }

    // Fallback if tags couldn't be read
    ("Unknown Artist".to_string(), "Unknown Album".to_string(), None)
}

/// Extract track title from audio file tags
/// Falls back to parsing filename if tags are missing or unreadable
/// Applies proper title capitalization to ensure consistent formatting
fn extract_title_from_tags(file_path: &Path) -> String {
    use lofty::prelude::*;

    let raw_title = if let Ok(tagged_file) = lofty::read_from_path(file_path) {
        if let Some(tag) = tagged_file.primary_tag().or_else(|| tagged_file.first_tag()) {
            if let Some(title) = tag.title() {
                if !title.is_empty() {
                    title.to_string()
                } else {
                    // Fallback: parse title from filename (XX - Title.ext pattern)
                    file_path.file_stem()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.split('-').nth(1))
                        .map(|s| s.trim().to_string())
                        .unwrap_or_else(|| "Unknown".to_string())
                }
            } else {
                // Fallback: parse title from filename (XX - Title.ext pattern)
                file_path.file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.split('-').nth(1))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|| "Unknown".to_string())
            }
        } else {
            // Fallback: parse title from filename (XX - Title.ext pattern)
            file_path.file_stem()
                .and_then(|s| s.to_str())
                .and_then(|s| s.split('-').nth(1))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "Unknown".to_string())
        }
    } else {
        // Fallback: parse title from filename (XX - Title.ext pattern)
        file_path.file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.split('-').nth(1))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "Unknown".to_string())
    };

    // Apply title capitalization (handles prepositions, articles, etc.)
    crate::convert::renaming::capitalize_title(&raw_title)
}

/// Validate all files have compatible audio format (sample rate, bit depth, channels)
fn validate_files_compatible(files: &[PathBuf]) -> Result<(), ConversionError> {
    use lofty::probe::Probe;
    use lofty::prelude::AudioFile;

    if files.is_empty() {
        return Err(ConversionError::ConversionFailed("No files to merge".to_string()));
    }

    if files.len() == 1 {
        return Ok(()); // Single file, nothing to validate
    }

    let first_file = &files[0];
    let first_probe = Probe::open(first_file)
        .and_then(|p| p.read())
        .map_err(|e| ConversionError::ConversionFailed(format!("Failed to read {}: {}", first_file.display(), e)))?;

    let first_props = first_probe.properties();
    let first_sample_rate = first_props.sample_rate();
    let first_bit_depth = first_props.bit_depth();
    let first_channels = first_props.channels();

    for file in files.iter().skip(1) {
        let probe = Probe::open(file)
            .and_then(|p| p.read())
            .map_err(|e| ConversionError::ConversionFailed(format!("Failed to read {}: {}", file.display(), e)))?;

        let props = probe.properties();

        if props.sample_rate() != first_sample_rate {
            return Err(ConversionError::ConversionFailed(
                format!("Incompatible sample rates: {:?} Hz vs {:?} Hz",
                    first_sample_rate, props.sample_rate())
            ));
        }

        if props.bit_depth() != first_bit_depth {
            return Err(ConversionError::ConversionFailed(
                format!("Incompatible bit depths: {:?} vs {:?}",
                    first_bit_depth, props.bit_depth())
            ));
        }

        if props.channels() != first_channels {
            return Err(ConversionError::ConversionFailed(
                format!("Incompatible channel counts: {:?} vs {:?}",
                    first_channels, props.channels())
            ));
        }
    }

    Ok(())
}

/// Get audio duration
fn get_audio_duration(path: &Path) -> Result<std::time::Duration, ConversionError> {
    use lofty::probe::Probe;
    use lofty::prelude::AudioFile;

    let probe = Probe::open(path)
        .and_then(|p| p.read())
        .map_err(|e| ConversionError::ConversionFailed(format!("Failed to read {}: {}", path.display(), e)))?;

    let duration = probe.properties().duration();
    Ok(duration)
}

/// Merge audio files using FFmpeg or Sox
async fn merge_audio_files(
    input_files: &[PathBuf],
    output_path: &Path,
    backend: Option<tonepoet_backend::Backend>,
) -> Result<(), ConversionError> {
    use tokio::process::Command as TokioCommand;
    use tokio::fs;
    use tonepoet_backend::Backend;

    if input_files.is_empty() {
        return Err(ConversionError::ConversionFailed("No files to merge".to_string()));
    }

    if input_files.len() == 1 {
        fs::copy(&input_files[0], output_path).await
            .map_err(|e| ConversionError::ConversionFailed(format!("Failed to copy file: {}", e)))?;
        return Ok(());
    }

    let selected_backend = backend.unwrap_or(Backend::FFmpeg);

    match selected_backend {
        Backend::FFmpeg => {
            let concat_list_path = output_path.with_extension("txt");
            let mut concat_content = String::new();
            for file in input_files {
                // Escape single quotes in paths by replacing ' with '\''
                let escaped = file.to_string_lossy().replace("'", "'\\''");
                concat_content.push_str(&format!("file '{}'\n", escaped));
            }
            fs::write(&concat_list_path, concat_content).await
                .map_err(|e| ConversionError::ConversionFailed(format!("Failed to write concat list: {}", e)))?;

            let output = TokioCommand::new("ffmpeg")
                .arg("-f").arg("concat")
                .arg("-safe").arg("0")
                .arg("-i").arg(&concat_list_path)
                .arg("-c").arg("copy")
                .arg(output_path)
                .output()
                .await
                .map_err(|e| ConversionError::ToolError(format!("Failed to run ffmpeg: {}", e)))?;

            let _ = fs::remove_file(&concat_list_path).await;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(ConversionError::ConversionFailed(format!("FFmpeg merge failed: {}", stderr)));
            }

            Ok(())
        }
        Backend::Sox => {
            let mut args = Vec::new();
            for file in input_files {
                args.push(file.to_string_lossy().to_string());
            }
            args.push(output_path.to_string_lossy().to_string());

            let output = TokioCommand::new("sox")
                .args(&args)
                .output()
                .await
                .map_err(|e| ConversionError::ToolError(format!("Failed to run sox: {}", e)))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(ConversionError::ConversionFailed(format!("Sox merge failed: {}", stderr)));
            }

            Ok(())
        }
    }
}

/// Move a directory, falling back to copy+delete if rename fails with EXDEV (cross-filesystem).
async fn move_dir_cross_fs(src: &Path, dst: &Path) -> Result<(), ConversionError> {
    match tokio::fs::rename(src, dst).await {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(18) => {  // EXDEV
            log::info!("Cross-filesystem move (EXDEV), copying {:?} -> {:?}", src, dst);
            copy_dir_recursive(src, dst).await?;
            tokio::fs::remove_dir_all(src).await.map_err(|e|
                ConversionError::ConversionFailed(
                    format!("Failed to clean up after cross-fs copy: {}", e)
                )
            )?;
            Ok(())
        }
        Err(e) => Err(ConversionError::ConversionFailed(
            format!("Failed to move {:?} to {:?}: {}", src, dst, e)
        )),
    }
}

async fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), ConversionError> {
    tokio::fs::create_dir_all(dst).await.map_err(|e|
        ConversionError::ConversionFailed(format!("Failed to create {:?}: {}", dst, e))
    )?;
    let mut entries = tokio::fs::read_dir(src).await.map_err(|e|
        ConversionError::ConversionFailed(format!("Failed to read {:?}: {}", src, e))
    )?;
    while let Some(entry) = entries.next_entry().await.map_err(|e|
        ConversionError::ConversionFailed(format!("readdir {:?}: {}", src, e))
    )? {
        let ft = entry.file_type().await.map_err(|e|
            ConversionError::ConversionFailed(format!("file_type: {}", e))
        )?;
        let dest_path = dst.join(entry.file_name());
        if ft.is_dir() {
            Box::pin(copy_dir_recursive(&entry.path(), &dest_path)).await?;
        } else {
            tokio::fs::copy(&entry.path(), &dest_path).await.map_err(|e|
                ConversionError::ConversionFailed(
                    format!("copy {:?} -> {:?}: {}", entry.path(), dest_path, e)
                )
            )?;
        }
    }
    Ok(())
}

