//! Audio conversion processor with parallel execution

use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, broadcast, Semaphore};
use tokio::task::JoinSet;
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
                        Ok((item_id_for_err, ConversionStatus::Failed { error: e.to_string() }))
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
                            Ok((item_id_for_err, ConversionStatus::Failed { error: e.to_string() }))
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
        return Ok((item.id.clone(), ConversionStatus::Failed { error: error_msg }));
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
    
    // Perform conversion based on format
    match &item.input_format {
        crate::convert::FileFormat::SevenZip => {
            // Extract and convert 7z archive
            extract_and_convert_7z(&item, &output_path, &tool_paths, &progress_tx, file_semaphore, scratch_directory.as_deref()).await?;
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
    let final_status = ConversionStatus::Completed { output_path: output_path.clone() };
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
            ConversionStatus::Completed { output_path }
        }
        tonepoet_backend::integration::ConversionStatus::Failed { error } => {
            ConversionStatus::Failed { error }
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

/// Extract and convert 7z archive
async fn extract_and_convert_7z(
    item: &ConversionItem,
    output_path: &Path,
    _tool_paths: &HashMap<String, PathBuf>,
    progress_tx: &broadcast::Sender<ProgressUpdate>,
    file_semaphore: Arc<Semaphore>,
    scratch_directory: Option<&Path>,
) -> ConversionResult<()> {
    use tokio::process::Command as TokioCommand;
    use crate::convert::simple_wizard::ReplayGainMode;
    use crate::convert::AudioFormat;

    // Send initial progress - Extracting phase
    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Extracting,
        0.0,
        Some("Extracting archive...".to_string()),
        None,
    ).await;

    // Diagnostic: log output_dir at entry
    log::info!("[DIAG] extract_and_convert_7z: item.options.output_dir = {:?}", item.options.output_dir);
    log::info!("[DIAG] extract_and_convert_7z: output_path = {:?}", output_path);

    // Create stable directory for extraction (not auto-cleaned)
    // Prefer scratch directory (fast local FS) over NTFS output directory
    let extract_base = if let Some(scratch) = scratch_directory {
        std::fs::create_dir_all(scratch).map_err(|e|
            ConversionError::ConversionFailed(
                format!("Failed to create scratch directory {:?}: {}", scratch, e)
            )
        )?;
        scratch
    } else {
        item.options.output_dir.as_ref()
            .map(|p| p.as_path())
            .unwrap_or_else(|| output_path.parent()
                .unwrap_or_else(|| std::path::Path::new(".")))
    };
    let archive_stem = item.input_path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("archive");
    // Decode HTML entities in filename (e.g., &#x27; → ')
    let decoded_stem = decode_html_entities(archive_stem);
    let extract_path = extract_base.join(format!(".extract_{}", decoded_stem));

    // Clean up existing extraction directory to ensure fresh extraction
    // Do this if using custom output directory or scratch directory (extraction is truly temporary)
    // If no custom output, .extract_* may contain previous final output
    if (item.options.output_dir.is_some() || scratch_directory.is_some()) && extract_path.exists() {
        log::info!("Removing existing extraction directory (using custom output): {:?}", extract_path);
        std::fs::remove_dir_all(&extract_path)
            .map_err(|e| ConversionError::ConversionFailed(
                format!("Failed to remove old extraction dir {:?}: {}", extract_path, e)
            ))?;
    }

    // Create the extraction directory
    std::fs::create_dir_all(&extract_path)
        .map_err(|e| ConversionError::ConversionFailed(format!("Failed to create extraction dir: {}", e)))?;
    
    
    // Update progress to show extraction starting
    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Extracting,
        10.0,
        Some("Starting archive extraction...".to_string()),
        None,
    ).await;
    
    // Extract 7z archive with progress tracking
    let password = item.archive_password.as_deref();
    let mut cmd = TokioCommand::new("7z");
    cmd.arg("x")
        .arg(&item.input_path);
    if let Some(pw) = password {
        cmd.arg(&format!("-p{}", pw));
    }
    let child = cmd
        .arg(&format!("-o{}", extract_path.display()))
        .arg("-y") // Yes to all prompts
        .stdout(std::process::Stdio::piped())  // Capture stdout for error reporting
        .stderr(std::process::Stdio::piped())  // Capture stderr for error reporting
        .spawn()
        .map_err(|e| ConversionError::ToolError(format!("Failed to spawn 7z: {}", e)))?;
    
    // Get archive size for time estimation
    let archive_size = std::fs::metadata(&item.input_path)
        .map(|m| m.len())
        .unwrap_or(100_000_000); // Default to 100MB
    
    // Estimate extraction time: ~1-3 MB/sec for 7z depending on compression
    // Password-protected archives are slower
    let extraction_speed = if item.archive_password.is_some() {
        1_000_000.0  // ~1MB/sec for encrypted
    } else {
        2_000_000.0  // ~2MB/sec for normal
    };
    let estimated_seconds = (archive_size as f64 / extraction_speed) as u64;
    let estimated_duration = std::time::Duration::from_secs(estimated_seconds.max(10));
    
    // Progress updates that match estimated time
    let tx_clone = progress_tx.clone();
    let item_id_clone = item.id.clone();
    let start_time = std::time::Instant::now();
    
    let progress_handle = tokio::spawn(async move {
        let update_interval = tokio::time::Duration::from_millis(500);
        let mut progress = 10.0;
        
        loop {
            tokio::time::sleep(update_interval).await;
            
            let elapsed = start_time.elapsed();
            let time_ratio = (elapsed.as_secs_f64() / estimated_duration.as_secs_f64()).min(1.0);
            
            // Use a realistic curve that slows down near the end
            // Quick start, steady middle, slow finish
            let target = if time_ratio < 0.2 {
                // First 20% of time -> reach 40% progress
                10.0 + (time_ratio * 5.0 * 30.0) as f32
            } else if time_ratio < 0.8 {
                // Middle 60% of time -> reach 85% progress  
                40.0 + ((time_ratio - 0.2) * 1.667 * 45.0) as f32
            } else {
                // Last 20% of time -> approach 95% slowly
                85.0 + ((time_ratio - 0.8) * 5.0 * 10.0) as f32
            };
            
            // Smooth progress updates - don't jump too fast
            let max_step = 2.0;
            if target > progress {
                progress = (progress + max_step).min(target).min(95.0);
            }
            
            let overall = ConversionPhase::Extracting.calculate_overall_progress(progress);
            let _ = tx_clone.send(ProgressUpdate {
                item_id: item_id_clone.clone(),
                progress: overall,
                status: ConversionStatus::Processing {
                    progress: overall,
                    message: Some(format!("Extracting archive... ({}%)", progress as u32)),
                    file_progress: None,
                    phase: Some(ConversionPhase::Extracting),
                    phase_progress: Some(progress),
                },
            });
            
            // Keep updating until the child process completes
        }
    });
    
    // Wait for extraction to complete and capture output
    let output = child.wait_with_output().await
        .map_err(|e| ConversionError::ToolError(format!("Failed to wait for 7z: {}", e)))?;

    // Stop the progress updates
    progress_handle.abort();

    if !output.status.success() {
        let stderr_output = String::from_utf8_lossy(&output.stderr);
        let stdout_output = String::from_utf8_lossy(&output.stdout);
        log::error!("7z extraction failed with exit code: {:?}", output.status.code());
        log::error!("7z stdout: {}", stdout_output);
        log::error!("7z stderr: {}", stderr_output);

        // Provide more helpful error message based on common issues
        let error_msg = if stderr_output.contains("Wrong password") || stdout_output.contains("Wrong password") {
            format!("7z extraction failed: Wrong password (exit code: {:?})", output.status.code())
        } else if stderr_output.contains("Can not open") || stdout_output.contains("Can not open") {
            format!("7z extraction failed: Cannot open archive file (exit code: {:?})", output.status.code())
        } else if !stderr_output.is_empty() {
            format!("7z extraction failed (exit code: {:?}): {}", output.status.code(), stderr_output.trim())
        } else if !stdout_output.is_empty() {
            format!("7z extraction failed (exit code: {:?}): {}", output.status.code(), stdout_output.trim())
        } else {
            format!("7z extraction failed with exit code: {:?}", output.status.code())
        };

        return Err(ConversionError::ToolError(error_msg));
    }

    // Send completion of extraction (only after confirmed success)
    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Extracting,
        100.0,
        Some("Extraction complete".to_string()),
        None,
    ).await;

    // Find all audio files in the extracted content
    let mut audio_files = Vec::new();
    find_audio_files_recursive(&extract_path, &mut audio_files)?;
    
    
    if audio_files.is_empty() {
        return Err(ConversionError::ConversionFailed("No audio files found in archive".to_string()));
    }
    
    // Send renaming phase progress
    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Renaming,
        10.0,
        Some("Analyzing folder structure...".to_string()),
        None,
    ).await;

    // Check if archive has single folder structure (flatten if needed)
    // Many archives contain a single folder with all content inside
    let actual_content_dir = {
        let entries: Vec<_> = std::fs::read_dir(&extract_path)
            .ok()
            .and_then(|entries| entries.collect::<Result<Vec<_>, _>>().ok())
            .unwrap_or_default();

        // If extraction contains exactly one directory, use that as the content directory
        if entries.len() == 1 && entries[0].path().is_dir() {
            log::info!("Archive contains single folder, will flatten structure");
            entries[0].path()
        } else {
            extract_path.to_path_buf()
        }
    };

    // Apply folder renaming based on metadata
    let renamed_folder = match crate::convert::apply_folder_renaming(&actual_content_dir, &audio_files, None, Some(&item.output_format)) {
        Ok(renamed) => {
            send_phase_update(
                progress_tx,
                &item.id,
                ConversionPhase::Renaming,
                50.0,
                Some("Renaming folder...".to_string()),
                None,
            ).await;
            info!("Renamed folder to: {}", renamed.display());
            renamed
        }
        Err(e) => {
            warn!("Folder renaming failed: {}, using original path", e);
            extract_path.to_path_buf()
        }
    };
    
    // Rename audio files within the folder
    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Renaming,
        70.0,
        Some("Renaming audio files...".to_string()),
        None,
    ).await;
    
    let renamed_files = match crate::convert::rename_audio_files(&renamed_folder) {
        Ok(files) => {
            send_phase_update(
                progress_tx,
                &item.id,
                ConversionPhase::Renaming,
                100.0,
                Some(format!("Renamed {} files", files.len())),
                None,
            ).await;
            info!("Renamed {} audio files", files.len());
            files
        }
        Err(e) => {
            warn!("File renaming failed: {}, using original files", e);
            // If renaming failed, we need to re-find the files in the renamed folder
            // because the folder itself was renamed
            let mut updated_files = Vec::new();
            find_audio_files_recursive(&renamed_folder, &mut updated_files)?;
            updated_files
        }
    };
    
    // Update audio_files to use renamed paths
    audio_files = if !renamed_files.is_empty() {
        renamed_files
    } else {
        // Re-find files in renamed folder if no renamed files returned
        let mut updated_files = Vec::new();
        find_audio_files_recursive(&renamed_folder, &mut updated_files)?;
        updated_files
    };
    
    
    // Send tagging phase progress
    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Tagging,
        10.0,
        Some("Reading metadata...".to_string()),
        None,
    ).await;
    
    // Update album tags with pressing info
    match crate::convert::update_album_tags(&renamed_folder) {
        Ok(count) => {
            send_phase_update(
                progress_tx,
                &item.id,
                ConversionPhase::Tagging,
                50.0,
                Some(format!("Updated {} album tags", count)),
                None,
            ).await;
            if count > 0 {
                info!("Updated album tags for {} files", count);
            }
        }
        Err(e) => {
            warn!("Album tag update failed: {}", e);
        }
    }
    
    // Update title tags to match renamed files
    match crate::convert::update_title_tags(&renamed_folder) {
        Ok(count) => {
            send_phase_update(
                progress_tx,
                &item.id,
                ConversionPhase::Tagging,
                70.0,
                Some(format!("Updated {} title tags", count)),
                None,
            ).await;
            if count > 0 {
                info!("Updated title tags for {} files", count);
            }
        }
        Err(e) => {
            warn!("Title tag update failed: {}", e);
        }
    }

    // Lineage.txt metadata tagging (if enabled) - 7z archive path
    if item.options.append_lineage_to_comment {
        use tokio::process::Command as TokioCommand;

        log::debug!("7z path: Lineage feature ENABLED, searching in: {:?}", renamed_folder);
        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::Tagging,
            75.0,
            Some("Appending Lineage.txt to COMMENT tags...".to_string()),
            None,
        ).await;

        // Look for Lineage.txt in the extracted/renamed folder (case-insensitive)
        let lineage_path = ["Lineage.txt", "lineage.txt", "LINEAGE.TXT"]
            .iter()
            .map(|name| renamed_folder.join(name))
            .find(|path| path.exists() && path.is_file());

        if let Some(lineage_path) = lineage_path {
            log::info!("Found Lineage.txt in 7z archive, appending to COMMENT tags");

            // Apply to all audio files
            for file in &audio_files {
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
            log::debug!("Lineage.txt not found in extracted archive: {}", renamed_folder.display());
        }
    }

    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Tagging,
        100.0,
        Some("Tagging complete".to_string()),
        None,
    ).await;

    let total_files = audio_files.len();

    // Collect file sizes BEFORE conversion (files will be deleted after conversion)
    let mut file_sizes = std::collections::HashMap::new();
    for audio_file in &audio_files {
        let size = tokio::fs::metadata(audio_file).await
            .map(|m| m.len())
            .unwrap_or(0);
        file_sizes.insert(audio_file.clone(), size);
    }

    // Pre-collect source info before conversion (files will be deleted after conversion)
    let mut source_infos = std::collections::HashMap::new();
    for audio_file in &audio_files {
        if let Some(info) = detect_source_info(audio_file) {
            source_infos.insert(audio_file.clone(), info);
        }
    }

    // Track conversion pipelines for logging
    let mut conversion_pipelines = std::collections::HashMap::new();

    // Track conversion timing per file
    let mut conversion_timings = std::collections::HashMap::new();

    // Track successful conversions
    let mut successful_conversions = std::collections::HashSet::new();

    // Output file map for logging: lowercase_stem -> (path, size, ReplayGain)
    let mut output_file_map: std::collections::HashMap<String, (PathBuf, u64, Option<ReplayGainValues>)> =
        std::collections::HashMap::new();

    // Track if optimized merge already completed (to skip old merge logic)
    let mut optimized_merge_complete = false;

    // Convert directly in the renamed folder (still in temp location)
    // We'll move to custom output AFTER conversion succeeds
    let output_dir = renamed_folder.clone();

    // Audio files remain in renamed_folder for conversion
    // No path updates needed since we're converting in place

    // OPTIMIZED: Merge-before-encode path for multi-file merge
    // Decode all → concatenate → process → encode once → ReplayGain
    // Avoids N separate encodes + duplicate ReplayGain calculations
    if item.options.merge_to_single && audio_files.len() >= 2 {
        log::info!("🚀 Using optimized merge-before-encode for {} files", audio_files.len());

        // Pre-flight disk space check: need 3× source size + 500MB buffer
        let total_source_size: u64 = audio_files.iter()
            .filter_map(|f| std::fs::metadata(f).ok())
            .map(|m| m.len())
            .sum();

        let required_space = (total_source_size * 3) + (500 * 1024 * 1024); // 500MB buffer

        // Check available space on output directory
        if let Some(available) = check_available_disk_space(&output_dir) {
            if available < required_space {
                return Err(ConversionError::ConversionFailed(
                    format!(
                        "Insufficient disk space: need {} MB, have {} MB available",
                        required_space / (1024 * 1024),
                        available / (1024 * 1024)
                    )
                ));
            }
            log::debug!("Disk space check passed: {} MB required, {} MB available",
                required_space / (1024 * 1024), available / (1024 * 1024));
        }

        // Detect max sample rate across all files for mixed-rate albums
        let mut max_sample_rate = 0u32;
        let mut max_bit_depth: Option<u16> = None;
        let mut max_channels = 0u8;

        for audio_file in &audio_files {
            use lofty::prelude::AudioFile;
            if let Ok(probe) = lofty::probe::Probe::open(audio_file).and_then(|p| p.read()) {
                let props = probe.properties();
                max_sample_rate = max_sample_rate.max(props.sample_rate().unwrap_or(0));
                max_bit_depth = match (max_bit_depth, props.bit_depth()) {
                    (Some(current), Some(new)) => Some(current.max(new as u16)),
                    (Some(current), None) => Some(current),
                    (None, Some(new)) => Some(new as u16),
                    (None, None) => None,
                };
                max_channels = max_channels.max(props.channels().unwrap_or(0));
            }
        }

        log::info!("Detected max properties: {}Hz, {:?}-bit, {} channels",
            max_sample_rate, max_bit_depth, max_channels);

        // Phase 1: Batch decode all files to WAV (0-40%)
        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::Converting,
            0.0,
            Some("Decoding all tracks for merge...".to_string()),
            None,
        ).await;

        let merge_start = chrono::Utc::now();
        let temp_dir = output_dir.join(".tonepoet_merge_temp");
        tokio::fs::create_dir_all(&temp_dir).await?;

        let mut decoded_wavs = Vec::new();
        let total_decode_files = audio_files.len();

        for (idx, audio_file) in audio_files.iter().enumerate() {
            let progress = ((idx + 1) as f32 / total_decode_files as f32) * 40.0;
            send_phase_update(
                progress_tx,
                &item.id,
                ConversionPhase::Converting,
                progress,
                Some(format!("Decoding track {} of {}", idx + 1, total_decode_files)),
                Some(((idx + 1) as u32, total_decode_files as u32)),
            ).await;

            let wav_name = format!("decoded_{:03}.wav", idx);
            let wav_path = temp_dir.join(wav_name);

            // Decode to WAV, normalizing to max sample rate if mixed rates
            let mut ffmpeg_args = vec![
                "-i".to_string(),
                audio_file.to_string_lossy().to_string(),
            ];

            // Force sample rate if we have mixed rates
            if max_sample_rate > 0 {
                ffmpeg_args.extend_from_slice(&[
                    "-ar".to_string(),
                    max_sample_rate.to_string(),
                ]);
            }

            ffmpeg_args.extend_from_slice(&[
                "-f".to_string(),
                "wav".to_string(),
                wav_path.to_string_lossy().to_string(),
            ]);

            let output = tokio::process::Command::new("ffmpeg")
                .args(&ffmpeg_args)
                .output()
                .await?;

            if !output.status.success() {
                // Cleanup on failure
                let _ = tokio::fs::remove_dir_all(&temp_dir).await;
                return Err(ConversionError::ConversionFailed(
                    format!("Failed to decode {}: {}",
                        audio_file.display(),
                        String::from_utf8_lossy(&output.stderr))
                ));
            }

            decoded_wavs.push(wav_path);
        }

        // Phase 2: Concatenate WAVs (40-45%)
        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::Converting,
            40.0,
            Some("Concatenating decoded tracks...".to_string()),
            None,
        ).await;

        let concat_list = temp_dir.join("concat_list.txt");
        let concat_list_content = decoded_wavs
            .iter()
            .map(|p| {
                // Escape single quotes in paths by replacing ' with '\''
                let escaped = p.to_string_lossy().replace("'", "'\\''");
                format!("file '{}'", escaped)
            })
            .collect::<Vec<_>>()
            .join("\n");
        tokio::fs::write(&concat_list, concat_list_content).await?;

        let concat_wav = temp_dir.join("concatenated.wav");
        let concat_output = tokio::process::Command::new("ffmpeg")
            .args(&[
                "-f", "concat",
                "-safe", "0",
                "-i", &concat_list.to_string_lossy(),
                "-c", "copy",
                &concat_wav.to_string_lossy(),
            ])
            .output()
            .await?;

        if !concat_output.status.success() {
            let _ = tokio::fs::remove_dir_all(&temp_dir).await;
            return Err(ConversionError::ConversionFailed(
                format!("Failed to concatenate: {}",
                    String::from_utf8_lossy(&concat_output.stderr))
            ));
        }

        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::Converting,
            45.0,
            Some("Concatenation complete".to_string()),
            None,
        ).await;

        // Phase 3: Process concatenated file if needed (45-80%)
        let processed_wav = if needs_processing(&item.options, max_sample_rate, max_bit_depth) {
            send_phase_update(
                progress_tx,
                &item.id,
                ConversionPhase::Converting,
                45.0,
                Some("Processing concatenated audio...".to_string()),
                None,
            ).await;

            let processed_path = temp_dir.join("processed.wav");

            // Build processing pipeline for merged file
            let mut audio_item = item.clone();
            audio_item.input_path = concat_wav.clone();
            audio_item.output_path = Some(processed_path.clone());
            audio_item.output_format = AudioFormat::Wav; // Force WAV output for processing stage

            // Override source sample rate with detected max
            let source_info = tonepoet_features::SourceInfo {
                format: "WAV".to_string(),
                bit_depth: max_bit_depth,
                sample_rate: Some(max_sample_rate),
                channels: Some(max_channels),
            };

            let backend_item = create_backend_conversion_item(&audio_item, Some(&source_info));
            let (backend_progress_tx, mut backend_progress_rx) = mpsc::channel::<tonepoet_backend::integration::ProgressUpdate>(100);

            let main_progress_tx = progress_tx.clone();
            let progress_forwarder = tokio::spawn(async move {
                while let Some(backend_update) = backend_progress_rx.recv().await {
                    let adjusted_progress = 45.0 + (backend_update.progress * 0.35); // 45-80%
                    let main_update = ProgressUpdate {
                        item_id: backend_update.item_id.clone(),
                        progress: adjusted_progress,
                        status: map_backend_status_to_main(backend_update.status),
                    };
                    let _ = main_progress_tx.send(main_update);
                }
            });

            match convert_with_backend(
                &backend_item,
                &concat_wav,
                &processed_path,
                &backend_progress_tx,
                item.options.preferred_backend,
            ).await {
                Ok(_) => {
                    progress_forwarder.abort();
                    processed_path
                }
                Err(e) => {
                    progress_forwarder.abort();
                    let _ = tokio::fs::remove_dir_all(&temp_dir).await;
                    return Err(ConversionError::ConversionFailed(
                        format!("Processing failed: {}", e)
                    ));
                }
            }
        } else {
            log::debug!("No processing needed, using concatenated WAV directly");
            concat_wav
        };

        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::Converting,
            80.0,
            Some("Processing complete".to_string()),
            None,
        ).await;

        // Phase 4: Encode to final format (80-95%)
        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::Converting,
            80.0,
            Some("Encoding merged file...".to_string()),
            None,
        ).await;

        let album_name = output_dir.file_name()
            .and_then(|s| s.to_str())
            .and_then(|dir_name| {
                dir_name.split(" - ").nth(1)
                    .and_then(|s| s.split('(').next())
                    .map(|s| s.trim().to_string())
            })
            .unwrap_or_else(|| "Album".to_string());

        let merged_filename = format!("{}.{}", album_name, item.output_format.extension());
        let merged_path = output_dir.join(&merged_filename);

        // Build encode pipeline
        let mut encode_item = item.clone();
        encode_item.input_path = processed_wav.clone();
        encode_item.output_path = Some(merged_path.clone());

        // Determine actual properties of processed_wav
        // If target was specified (> 0), processing converted to target; otherwise remains at max
        let final_sample_rate = item.options.target_sample_rate
            .filter(|&r| r > 0)
            .unwrap_or(max_sample_rate);
        let final_bit_depth = item.options.target_bit_depth
            .filter(|&d| d > 0)
            .map(|d| d as u16)
            .or(max_bit_depth);

        let encode_source_info = tonepoet_features::SourceInfo {
            format: "WAV".to_string(),
            bit_depth: final_bit_depth,
            sample_rate: Some(final_sample_rate),
            channels: Some(max_channels),
        };

        let encode_backend_item = create_backend_conversion_item(&encode_item, Some(&encode_source_info));
        let (encode_progress_tx, mut encode_progress_rx) = mpsc::channel::<tonepoet_backend::integration::ProgressUpdate>(100);

        let main_progress_tx = progress_tx.clone();
        let encode_forwarder = tokio::spawn(async move {
            while let Some(backend_update) = encode_progress_rx.recv().await {
                let adjusted_progress = 80.0 + (backend_update.progress * 0.15); // 80-95%
                let main_update = ProgressUpdate {
                    item_id: backend_update.item_id.clone(),
                    progress: adjusted_progress,
                    status: map_backend_status_to_main(backend_update.status),
                };
                let _ = main_progress_tx.send(main_update);
            }
        });

        match convert_with_backend(
            &encode_backend_item,
            &processed_wav,
            &merged_path,
            &encode_progress_tx,
            item.options.preferred_backend,
        ).await {
            Ok((_, pipeline)) => {
                encode_forwarder.abort();

                // Store pipeline for logging (use for all source files)
                for audio_file in &audio_files {
                    conversion_pipelines.insert(audio_file.clone(), pipeline.clone());
                }
            }
            Err(e) => {
                encode_forwarder.abort();
                let _ = tokio::fs::remove_dir_all(&temp_dir).await;
                return Err(ConversionError::ConversionFailed(
                    format!("Encoding failed: {}", e)
                ));
            }
        }

        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::Converting,
            95.0,
            Some("Encoding complete".to_string()),
            None,
        ).await;

        // Phase 5: ReplayGain (95-100%)
        if item.options.calculate_replaygain {
            send_phase_update(
                progress_tx,
                &item.id,
                ConversionPhase::Converting,
                95.0,
                Some("Calculating ReplayGain...".to_string()),
                None,
            ).await;

            let mut rg_args = vec![];
            match item.options.replaygain_mode.as_ref() {
                Some(ReplayGainMode::Album) | Some(ReplayGainMode::Both) => {
                    rg_args.push("-a".to_string());
                }
                Some(ReplayGainMode::Track) => rg_args.push("-r".to_string()),
                None => {}
            }

            if !rg_args.is_empty() {
                rg_args.extend_from_slice(&[
                    "-k".to_string(),
                    "-s".to_string(),
                    "i".to_string(),
                    merged_path.to_string_lossy().to_string(),
                ]);

                let _ = tokio::process::Command::new("loudgain")
                    .args(&rg_args)
                    .output()
                    .await;
            }
        }

        let merge_end = chrono::Utc::now();

        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::Converting,
            100.0,
            Some("Merge complete".to_string()),
            None,
        ).await;

        // Cleanup temp directory
        let _ = tokio::fs::remove_dir_all(&temp_dir).await;

        // Apply metadata to merged MP3 file (after ReplayGain, before CUE generation)
        // DEBUG: Write marker file to verify this code runs
        let debug_marker = merged_path.with_extension("metadata_debug");
        let _ = tokio::fs::write(&debug_marker, format!("Format: {:?}\nFirst file exists: {}\n", item.output_format, audio_files.first().is_some())).await;

        log::info!("DEBUG: Checking if metadata should be applied. Format: {:?}, Merged path: {}", item.output_format, merged_path.display());
        if item.output_format == AudioFormat::Mp3 || item.output_format == AudioFormat::Aac {
            log::info!("DEBUG: Format is MP3/AAC, checking for first_file");
            if let Some(first_file) = audio_files.first() {
                log::info!("DEBUG: Calling apply_album_metadata_to_merged_file with source: {}", first_file.display());
                if let Err(e) = apply_album_metadata_to_merged_file(&merged_path, first_file).await {
                    log::warn!("Failed to apply album metadata to merged file: {}", e);
                    let _ = tokio::fs::write(&debug_marker, format!("ERROR: {}", e)).await;
                    // Non-fatal - continue with conversion
                } else {
                    let _ = tokio::fs::write(&debug_marker, "SUCCESS").await;
                }
            } else {
                log::warn!("DEBUG: No first_file found in audio_files");
                let _ = tokio::fs::write(&debug_marker, "No first file").await;
            }
        } else if item.output_format == AudioFormat::Opus {
            log::info!("DEBUG: Format is Opus, checking for first_file");
            if let Some(first_file) = audio_files.first() {
                log::info!("DEBUG: Calling apply_album_metadata_to_opus with source: {}", first_file.display());
                if let Err(e) = apply_album_metadata_to_opus(&merged_path, first_file).await {
                    log::warn!("Failed to apply album metadata to merged Opus: {}", e);
                    let _ = tokio::fs::write(&debug_marker, format!("ERROR: {}", e)).await;
                    // Non-fatal - continue with conversion
                } else {
                    let _ = tokio::fs::write(&debug_marker, "SUCCESS").await;
                }
            } else {
                log::warn!("DEBUG: No first_file found in audio_files");
                let _ = tokio::fs::write(&debug_marker, "No first file").await;
            }
        } else {
            log::info!("DEBUG: Format is not MP3/AAC/Opus, skipping metadata application");
        }

        // Generate cue sheet BEFORE deleting source files (needs to read durations and tags)

        // Extract album-level metadata from first file's tags
        let (album_artist, album_title, album_year) = if let Some(first_file) = audio_files.first() {
            extract_album_metadata_from_tags(first_file)
        } else {
            ("Unknown Artist".to_string(), "Unknown Album".to_string(), None)
        };

        let mut track_durations = Vec::new();
        let mut track_titles = Vec::new();

        for audio_file in &audio_files {
            if let Ok(duration) = get_audio_duration(audio_file) {
                track_durations.push(duration);
            } else {
                track_durations.push(std::time::Duration::from_secs(0));
            }

            // Extract title from tags, with filename fallback
            let title = extract_title_from_tags(audio_file);
            track_titles.push(title);
        }

        // Generate merged CUE file if user config allows
        if item.options.generate_cue_files {
            let should_generate = match item.options.cue_generation_mode.as_str() {
                "Always" => true,
                "If merging multiple tracks" => true,  // This IS a merge operation
                "IfMerging" => true,  // Legacy value support
                _ => false,
            };

            if should_generate {
                let _ = tonepoet_features::cue_generator::generate_merged_cue_file(
                    &output_dir,
                    &merged_filename,
                    &album_artist,
                    &album_title,
                    album_year.as_deref(),
                    &track_titles,
                    &track_durations,
                ).await;
            }
        }

        // Delete original source files (they've been merged)
        for audio_file in &audio_files {
            if audio_file.exists() {
                if let Err(e) = tokio::fs::remove_file(audio_file).await {
                    log::warn!("Failed to remove source file {:?}: {}", audio_file, e);
                }
            }
        }

        // Mark all files as successfully converted
        for audio_file in &audio_files {
            successful_conversions.insert(audio_file.clone());
            conversion_timings.insert(audio_file.clone(), (merge_start, merge_end));
        }

        // Build output_file_map - map each source file to merged output
        let merged_size = tokio::fs::metadata(&merged_path).await
            .map(|m| m.len())
            .unwrap_or(0);

        let rg_values = if item.options.calculate_replaygain {
            read_replaygain_tags(&merged_path)
        } else {
            None
        };

        output_file_map.clear();
        for audio_file in &audio_files {
            if let Some(stem) = audio_file.file_stem().and_then(|s| s.to_str()) {
                let normalized_stem = normalize_stem(stem);
                output_file_map.insert(
                    normalized_stem,
                    (merged_path.clone(), merged_size, rg_values.clone())
                );
            }
        }

        log::info!("✅ Optimized merge complete: {} → {}", audio_files.len(), merged_path.display());

        // Mark optimized merge as complete to skip old merge logic later
        optimized_merge_complete = true;

        // Skip remaining conversion logic - jump to logging/finalization
        // (output_file_map, conversion_pipelines, conversion_timings, successful_conversions all populated)
    } else {
    // NEW: Check if we can use copy mode for FLAC→FLAC with no processing
    let can_skip_transcoding = item.output_format == AudioFormat::Flac
        && !item.options.reencode_flac
        && (item.options.target_sample_rate.is_none() || item.options.target_sample_rate == Some(0))
        && (item.options.target_bit_depth.is_none() || item.options.target_bit_depth == Some(0))
        && !(item.options.dither_type.is_some()
             && item.options.target_bit_depth.is_some()
             && (item.options.target_bit_depth == Some(16) || item.options.target_bit_depth == Some(24)));

    if can_skip_transcoding {
        // Copy mode path for 7z archives
        log::info!("🚀 Using FLAC copy mode for 7z archive (skipping transcoding)");

        // Send Converting phase start
        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::Converting,
            0.0,
            Some("Skipping transcoding (copy mode)...".to_string()),
            None,
        ).await;

        // Mark all files as successful (instant "conversion")
        let copy_time = chrono::Utc::now();
        for audio_path in &audio_files {
            successful_conversions.insert(audio_path.clone());
            conversion_timings.insert(audio_path.clone(), (copy_time, copy_time));
        }

        // Send Converting phase progress
        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::Converting,
            50.0,
            Some(format!("Copy mode: {} files ready", audio_files.len())),
            None,
        ).await;

        // Calculate ReplayGain if requested
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
                Some(ReplayGainMode::Both) => args.push("-a".to_string()),
                None => {
                    log::warn!("ReplayGain enabled but mode not specified, skipping");
                }
            }

            if !args.is_empty() {
                args.push("-k".to_string()); // Keep existing tags (noclip)
                args.push("-s".to_string()); // Tag mode
                args.push("i".to_string());  // Write ReplayGain 2.0 tags

                // Add all files
                for file in &audio_files {
                    args.push(file.to_string_lossy().to_string());
                }

                // Execute loudgain
                match TokioCommand::new("loudgain")
                    .args(&args)
                    .output()
                    .await
                {
                    Ok(output) => {
                        if output.status.success() {
                            log::info!("ReplayGain tags applied successfully");
                        } else {
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            log::warn!("loudgain failed: {}", stderr);
                        }
                    }
                    Err(e) => {
                        log::warn!("Failed to run loudgain: {}", e);
                    }
                }
            }

            send_phase_update(
                progress_tx,
                &item.id,
                ConversionPhase::PostProcessing,
                100.0,
                Some("ReplayGain calculation complete".to_string()),
                None,
            ).await;
        }

        // Send Converting phase complete
        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::Converting,
            100.0,
            Some("Copy mode complete".to_string()),
            None,
        ).await;

        // Move files from duplicate nested folders to root (matching conversion mode behavior)
        // This allows cleanup to work correctly while preserving multi-disc subdirectories
        let parent_folder_name = output_dir.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase();

        let mut read_dir = tokio::fs::read_dir(&output_dir).await
            .map_err(|e| ConversionError::ConversionFailed(format!("Failed to read output dir: {}", e)))?;

        while let Some(entry) = read_dir.next_entry().await
            .map_err(|e| ConversionError::ConversionFailed(format!("Failed to read dir entry: {}", e)))? {
            let subdir_path = entry.path();

            if subdir_path.is_dir() {
                if let Some(subfolder_name) = subdir_path.file_name().and_then(|n| n.to_str()) {
                    let subfolder_lower = subfolder_name.to_lowercase();

                    // Use same logic as cleanup to identify duplicate folders
                    if folders_are_similar(&parent_folder_name, &subfolder_lower) {
                        log::info!("Moving files from duplicate nested folder to root: {:?}", subdir_path);

                        // Move all files from this duplicate subdirectory to root
                        let mut subdir_files = tokio::fs::read_dir(&subdir_path).await
                            .map_err(|e| ConversionError::ConversionFailed(format!("Failed to read subdir: {}", e)))?;

                        while let Some(file_entry) = subdir_files.next_entry().await
                            .map_err(|e| ConversionError::ConversionFailed(format!("Failed to read file entry: {}", e)))? {
                            let file_path = file_entry.path();

                            if let Some(filename) = file_path.file_name() {
                                let target_path = output_dir.join(filename);

                                if let Err(e) = tokio::fs::rename(&file_path, &target_path).await {
                                    log::warn!("Failed to move {:?} to root: {}", file_path, e);
                                } else {
                                    log::debug!("Moved {:?} to root", filename);
                                }
                            }
                        }
                    }
                }
            }
        }

    } else {
        // Parallel file conversion using global semaphore
        let available_permits = file_semaphore.available_permits();
        log::info!("[PARALLEL] Starting parallel conversion of {} files with {} semaphore permits",
            total_files, available_permits);

        // Wrap shared state in Arc for concurrent access
        let source_infos_arc = Arc::new(source_infos);
        let conversion_pipelines_arc =
            Arc::new(tokio::sync::Mutex::new(std::mem::take(&mut conversion_pipelines)));
        let conversion_timings_arc =
            Arc::new(tokio::sync::Mutex::new(std::mem::take(&mut conversion_timings)));
        let successful_conversions_arc =
            Arc::new(tokio::sync::Mutex::new(std::mem::take(&mut successful_conversions)));
        let failed_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let completed_files = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let mut file_tasks: JoinSet<()> = JoinSet::new();

        for audio_path in audio_files.iter() {
            // Verify the file exists before spawning task
            if !audio_path.exists() {
                log::error!("File does not exist, skipping: {:?}", audio_path);
                failed_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                continue;
            }

            // Determine output filename
            let filename = audio_path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            let output_file_path = output_dir.join(format!("{}.{}", filename, item.output_format.extension()));

            // Create a conversion item for this individual audio file
            let mut audio_item = item.clone();
            audio_item.input_path = audio_path.to_path_buf();
            audio_item.output_path = Some(output_file_path.clone());

            // Enable overwrite for in-place conversion (source == destination)
            if *audio_path == output_file_path {
                log::debug!("Enabling overwrite for in-place conversion: {:?}", audio_path);
                audio_item.options.overwrite = true;
            }

            // Look up pre-collected source info for this file
            let source_info_owned = source_infos_arc.get(audio_path).cloned();
            let backend_item = create_backend_conversion_item(&audio_item, source_info_owned.as_ref());

            // Clone shared state for the spawned task
            let semaphore = file_semaphore.clone();
            let progress_tx = progress_tx.clone();
            let item_id = item.id.clone();
            let audio_path = audio_path.clone();
            let conversion_pipelines_arc = conversion_pipelines_arc.clone();
            let conversion_timings_arc = conversion_timings_arc.clone();
            let successful_conversions_arc = successful_conversions_arc.clone();
            let failed_count = failed_count.clone();
            let completed_files = completed_files.clone();
            let total_files = total_files;
            let preferred_backend = item.options.preferred_backend;

            file_tasks.spawn(async move {
                // Acquire semaphore permit — blocks until a CPU slot is free
                log::info!("[PARALLEL] File waiting for permit: {}", audio_path.display());
                let _permit = match semaphore.acquire_owned().await {
                    Ok(permit) => {
                        log::info!("[PARALLEL] File acquired permit: {}", audio_path.display());
                        permit
                    }
                    Err(_) => {
                        log::error!("Semaphore closed for file: {}", audio_path.display());
                        failed_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        return;
                    }
                };

                // Create progress adapter for this individual file
                let (backend_progress_tx, mut backend_progress_rx) =
                    mpsc::channel::<tonepoet_backend::integration::ProgressUpdate>(100);

                // Forward progress
                let main_progress_tx = progress_tx.clone();
                let progress_forwarder = tokio::spawn(async move {
                    while let Some(backend_update) = backend_progress_rx.recv().await {
                        let main_update = ProgressUpdate {
                            item_id: backend_update.item_id,
                            progress: backend_update.progress,
                            status: map_backend_status_to_main(backend_update.status),
                        };
                        let _ = main_progress_tx.send(main_update);
                    }
                });

                // Capture start time
                let conversion_start = chrono::Utc::now();

                // Run conversion
                match convert_with_backend(
                    &backend_item,
                    &audio_path,
                    &output_file_path,
                    &backend_progress_tx,
                    preferred_backend,
                ).await {
                    Ok((_, pipeline)) => {
                        let conversion_end = chrono::Utc::now();
                        let elapsed = conversion_end.signed_duration_since(conversion_start);
                        log::info!("[PARALLEL] File finished ({:.1}s): {}", elapsed.num_milliseconds() as f64 / 1000.0, audio_path.display());
                        conversion_timings_arc.lock().await.insert(audio_path.clone(), (conversion_start, conversion_end));
                        successful_conversions_arc.lock().await.insert(audio_path.clone());
                        conversion_pipelines_arc.lock().await.insert(audio_path.clone(), pipeline);

                        // Remove original file to avoid duplication
                        if audio_path != output_file_path && audio_path.exists() {
                            if let Err(e) = tokio::fs::remove_file(&audio_path).await {
                                log::warn!("Failed to remove original file {:?}: {}", audio_path, e);
                            }
                        }
                        progress_forwarder.abort();
                    }
                    Err(e) => {
                        progress_forwarder.abort();
                        log::error!("Backend conversion failed for {}: {}", audio_path.display(), e);
                        failed_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }

                // Increment completed counter and send aggregate progress
                let done = completed_files.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                let phase_progress = (done as f32 / total_files as f32) * 100.0;
                send_phase_update(
                    &progress_tx,
                    &item_id,
                    ConversionPhase::Converting,
                    phase_progress,
                    Some(format!("Converted {} of {} files", done, total_files)),
                    Some((done as u32, total_files as u32)),
                ).await;
            });
        }

        // Drain all file tasks
        while let Some(result) = file_tasks.join_next().await {
            if let Err(e) = result {
                log::error!("File conversion task panicked: {}", e);
                failed_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }

        // Log failures but do NOT return early — allow downstream code
        // (renaming, ReplayGain, move-to-output-dir) to run on whatever succeeded
        let num_failed = failed_count.load(std::sync::atomic::Ordering::Relaxed);
        let num_succeeded = successful_conversions_arc.lock().await.len();
        log::info!("[PARALLEL] All file tasks drained: {} succeeded, {} failed out of {} total",
            num_succeeded, num_failed, total_files);
        if num_failed > 0 {
            log::warn!("{} of {} file conversion(s) failed", num_failed, total_files);
        }

        // If ALL files failed, bail out
        if num_succeeded == 0 && total_files > 0 {
            return Err(ConversionError::ConversionFailed(
                format!("All {} file conversions failed", total_files)
            ));
        }

        // Unwrap Arc state back to owned values for downstream code
        // All tasks have been drained, so we are the sole owner of each Arc
        source_infos = match Arc::try_unwrap(source_infos_arc) {
            Ok(v) => v,
            Err(arc) => (*arc).clone(),
        };
        conversion_pipelines = match Arc::try_unwrap(conversion_pipelines_arc) {
            Ok(m) => m.into_inner(),
            Err(arc) => arc.lock().await.clone(),
        };
        conversion_timings = match Arc::try_unwrap(conversion_timings_arc) {
            Ok(m) => m.into_inner(),
            Err(arc) => arc.lock().await.clone(),
        };
        successful_conversions = match Arc::try_unwrap(successful_conversions_arc) {
            Ok(m) => m.into_inner(),
            Err(arc) => arc.lock().await.clone(),
        };
    } // End of if can_skip_transcoding / else block
    } // End of if merge_to_single optimized path / else block

    // Clean up any nested subdirectories that duplicate the parent folder name
    // This handles the case where extraction/backend creates redundant nested folders
    // In copy mode, files have already been moved from duplicate folders, so cleanup safely removes empty dirs
    if let Err(e) = cleanup_duplicate_nested_folders(&output_dir).await {
        log::warn!("Failed to clean up duplicate nested folders: {}", e);
        // Continue anyway - not critical
    }

    // Rename converted audio files to apply proper capitalization
    // This runs AFTER conversion, so it operates on the final format (.opus, .mp3, etc.)
    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Finalizing,
        50.0,
        Some("Applying final file naming...".to_string()),
        None,
    ).await;

    match crate::convert::rename_audio_files(&output_dir) {
        Ok(renamed) => {
            info!("Applied capitalization to {} converted audio files", renamed.len());

            // Update title tags to match the final corrected filenames
            match crate::convert::update_title_tags(&output_dir) {
                Ok(count) => {
                    if count > 0 {
                        info!("Updated {} title tags after final renaming", count);
                    }
                }
                Err(e) => {
                    warn!("Post-conversion title tag update failed: {}", e);
                }
            }
        }
        Err(e) => {
            warn!("Post-conversion renaming failed: {}, files will keep current names", e);
            // Not critical - conversion is complete
        }
    }

    // Scan for final output files after renaming to get accurate sizes
    // Skip if optimized merge already populated output_file_map
    if !optimized_merge_complete {
        let mut final_output_files = Vec::new();
        find_audio_files_recursive(&output_dir, &mut final_output_files)?;

        // Build map: lowercase_stem -> (PathBuf, size, Option<ReplayGainValues>)
        // (already declared earlier for optimized merge path)

        for output_file in final_output_files {
        let size = tokio::fs::metadata(&output_file).await
            .map(|m| m.len())
            .unwrap_or(0);

        // Read ReplayGain tags if calculation was enabled
        let rg_values = if item.options.calculate_replaygain {
            read_replaygain_tags(&output_file)
        } else {
            None
        };

        if let Some(stem) = output_file.file_stem().and_then(|s| s.to_str()) {
            let normalized_stem = normalize_stem(stem);
            output_file_map.insert(normalized_stem, (output_file, size, rg_values));
        }
    }
    } // End if !optimized_merge_complete

    // Merge tracks if requested (old merge logic - only run if optimized merge didn't already complete)
    if item.options.merge_to_single && !optimized_merge_complete {
        use tokio::process::Command as TokioCommand;
        use crate::convert::simple_wizard::ReplayGainMode;

        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::PostProcessing,
            0.0,
            Some("Merging tracks...".to_string()),
            None,
        ).await;

        // Collect output files from map
        let mut output_files: Vec<PathBuf> = output_file_map
            .values()
            .map(|(path, _, _)| path.clone())
            .collect();

        // Sort by track number
        output_files.sort_by_key(|path| extract_track_number(path));

        // Validate compatibility
        validate_files_compatible(&output_files)?;

        // Extract album-level metadata from first file's tags
        let (album_artist, album_title, album_year) = if let Some(first_file) = output_files.first() {
            extract_album_metadata_from_tags(first_file)
        } else {
            ("Unknown Artist".to_string(), "Unknown Album".to_string(), None)
        };

        // Get durations and titles for cue sheet
        let mut track_durations = Vec::new();
        let mut track_titles = Vec::new();
        for path in &output_files {
            let duration = get_audio_duration(path)?;
            track_durations.push(duration);

            // Extract title from tags, with filename fallback
            let title = extract_title_from_tags(path);
            track_titles.push(title);
        }

        // Determine merged filename from directory
        let album_name = output_dir.file_name()
            .and_then(|s| s.to_str())
            .and_then(|dir_name| {
                dir_name.split(" - ").nth(1)
                    .and_then(|s| s.split('(').next())
                    .map(|s| s.trim().to_string())
            })
            .unwrap_or_else(|| "Album".to_string());

        let merged_filename = format!("{}.{}", album_name, item.output_format.extension());
        let merged_path = output_dir.join(&merged_filename);

        // Merge files
        merge_audio_files(&output_files, &merged_path, item.options.preferred_backend).await?;

        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::PostProcessing,
            33.0,
            Some("Tracks merged successfully".to_string()),
            None,
        ).await;

        // Reapply ReplayGain to merged file (Album mode)
        if item.options.calculate_replaygain {
            send_phase_update(
                progress_tx,
                &item.id,
                ConversionPhase::PostProcessing,
                40.0,
                Some("Calculating ReplayGain for merged file...".to_string()),
                None,
            ).await;

            let mut args = vec![];
            // Always use Album mode for merged files
            match item.options.replaygain_mode.as_ref() {
                Some(ReplayGainMode::Album) | Some(ReplayGainMode::Both) => {
                    args.push("-a".to_string());
                }
                Some(ReplayGainMode::Track) => args.push("-r".to_string()),
                None => {}
            }

            if !args.is_empty() {
                args.push("-k".to_string());
                args.push("-s".to_string());
                args.push("i".to_string());
                args.push(merged_path.to_string_lossy().to_string());

                let _ = TokioCommand::new("loudgain")
                    .args(&args)
                    .output()
                    .await;
            }
        }

        // Apply metadata to merged MP3/AAC/Opus file (after ReplayGain, before CUE generation)
        if item.output_format == AudioFormat::Mp3 || item.output_format == AudioFormat::Aac {
            if let Some(first_file) = output_files.first() {
                if let Err(e) = apply_album_metadata_to_merged_file(&merged_path, first_file).await {
                    log::warn!("Failed to apply album metadata to merged file: {}", e);
                    // Non-fatal - continue with conversion
                }
            }
        } else if item.output_format == AudioFormat::Opus {
            if let Some(first_file) = output_files.first() {
                if let Err(e) = apply_album_metadata_to_opus(&merged_path, first_file).await {
                    log::warn!("Failed to apply album metadata to merged Opus: {}", e);
                    // Non-fatal - continue with conversion
                }
            }
        }

        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::PostProcessing,
            66.0,
            Some("Generating merged cue sheet...".to_string()),
            None,
        ).await;

        // Generate merged cue sheet
        // Generate merged CUE file if user config allows
        if item.options.generate_cue_files {
            let should_generate = match item.options.cue_generation_mode.as_str() {
                "Always" => true,
                "If merging multiple tracks" => true,  // This IS a merge operation
                "IfMerging" => true,  // Legacy value support
                _ => false,
            };

            if should_generate {
                let _ = tonepoet_features::cue_generator::generate_merged_cue_file(
                    &output_dir,
                    &merged_filename,
                    &album_artist,
                    &album_title,
                    album_year.as_deref(),
                    &track_titles,
                    &track_durations,
                ).await;
            }
        }

        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::PostProcessing,
            80.0,
            Some("Cleaning up individual files...".to_string()),
            None,
        ).await;

        // Delete individual files
        for file in &output_files {
            let _ = tokio::fs::remove_file(file).await;
        }

        // Rebuild output_file_map - map each source file to merged output for logging
        let size = tokio::fs::metadata(&merged_path).await
            .map(|m| m.len())
            .unwrap_or(0);
        let rg_values = if item.options.calculate_replaygain {
            read_replaygain_tags(&merged_path)
        } else {
            None
        };

        output_file_map.clear();
        for source_file in &output_files {
            if let Some(stem) = source_file.file_stem().and_then(|s| s.to_str()) {
                let normalized_stem = normalize_stem(stem);
                output_file_map.insert(
                    normalized_stem,
                    (merged_path.clone(), size, rg_values.clone())
                );
            }
        }

        send_phase_update(
            progress_tx,
            &item.id,
            ConversionPhase::PostProcessing,
            100.0,
            Some("Merge complete".to_string()),
            None,
        ).await;
    }

    send_phase_update(
        progress_tx,
        &item.id,
        ConversionPhase::Finalizing,
        100.0,
        Some("Finalization complete".to_string()),
        None,
    ).await;

    // Send completion
    let _ = progress_tx.send(ProgressUpdate {
        item_id: item.id.clone(),
        progress: 100.0,
        status: ConversionStatus::Completed {
            output_path: output_dir.clone(),
        },
    });
    
    // Generate log and cue files if enabled
    let feature_config = FeaturesConfig {
        write_log_file: item.options.write_log_file,
        generate_cue_files: item.options.generate_cue_files,
        cue_generation_mode: item.options.cue_generation_mode.clone(),
        preferred_backend: "FFmpeg".to_string(),
        worker_count: 8,
        process_priority: 0,
        overwrite_behavior: "KeepBoth".to_string(),
    };
    
    // Collect conversion results for logging
    let mut conversion_results = Vec::new();
    for audio_file in &audio_files {
        // Use pre-collected file size (file may have been deleted after conversion)
        let source_size = file_sizes.get(audio_file).copied().unwrap_or(0);

        // Match output file by normalized stem (case-insensitive + whitespace normalized)
        let source_stem = audio_file.file_stem()
            .and_then(|s| s.to_str())
            .map(normalize_stem)
            .unwrap_or_default();

        let (output_file, output_size, replaygain_values) = output_file_map.get(&source_stem)
            .map(|(path, size, rg)| (path.clone(), *size, rg.clone()))
            .unwrap_or_else(|| {
                // Fallback for files that weren't converted
                let fallback_path = output_dir.join("unknown");
                (fallback_path, 0, None)
            });

        // Use tracked timing and success status
        let (start_time, end_time) = conversion_timings.get(audio_file)
            .copied()
            .unwrap_or_else(|| {
                let now = chrono::Utc::now();
                (now, now)
            });

        let status = if successful_conversions.contains(audio_file) {
            FeaturesStatus::Success
        } else {
            FeaturesStatus::Failed
        };

        // Retrieve pre-collected source info
        let source_info = source_infos.get(audio_file).cloned();
        log::debug!("Source info for {:?}: {:?}", audio_file, source_info);

        // Retrieve pipeline for logging
        let conversion_pipeline = conversion_pipelines.get(audio_file).cloned();
        log::debug!("Pipeline for {:?}: {} commands", audio_file,
                    conversion_pipeline.as_ref().map(|p| p.commands.len()).unwrap_or(0));

        conversion_results.push(FeaturesResult {
            source_file: audio_file.clone(),
            output_file: output_file.clone(),
            status,
            source_size,
            output_size,
            start_time,
            end_time,
            error_message: None, // TODO: Capture actual conversion errors
            replaygain_values,
            source_info,
            conversion_pipeline,
        });
    }
    
    // Serialize conversion options for logging
    // Prefer original settings if available for comprehensive logging
    let settings_json = if let Some(ref settings) = item.options.original_settings {
        serde_json::to_string(settings.as_ref()).ok()
    } else {
        serde_json::to_string(&item.options).ok()
    };

    // Phase 2: Album-level ReplayGain calculation (if Album or Both mode selected)
    if item.options.calculate_replaygain {
        if let Some(replaygain_mode) = &item.options.replaygain_mode {
            if matches!(replaygain_mode, ReplayGainMode::Album | ReplayGainMode::Both) {
                send_phase_update(
                    progress_tx,
                    &item.id,
                    ConversionPhase::PostProcessing,
                    0.0,
                    Some("Calculating album-level ReplayGain...".to_string()),
                    None,
                ).await;

                // Collect all output files for album scan
                let album_files: Vec<PathBuf> = conversion_results.iter()
                    .map(|r| r.output_file.clone())
                    .collect();

                if !album_files.is_empty() {
                    // Check if we're dealing with M4A files (loudgain crashes on M4A album mode)
                    let is_m4a = album_files.first()
                        .and_then(|f| f.extension())
                        .map(|ext| ext == "m4a")
                        .unwrap_or(false);

                    // Build loudgain command for album scan
                    let mut album_rg_args = vec![
                        "-a".to_string(),                    // Album mode
                    ];

                    // For M4A files, use output-only mode to avoid segfault
                    // For other formats, use normal tag writing mode
                    if is_m4a {
                        album_rg_args.push("-o".to_string()); // Output-only mode (no tag writing)
                    } else {
                        album_rg_args.push("-k".to_string()); // Keep existing tags
                        album_rg_args.push("-s".to_string()); // Tag mode
                        album_rg_args.push("i".to_string());  // Write ReplayGain 2.0 tags
                    }

                    // Add all files to single command
                    for file in &album_files {
                        album_rg_args.push(file.to_string_lossy().to_string());
                    }

                    // Execute album ReplayGain scan
                    use tokio::process::Command as TokioCommand;
                    match TokioCommand::new("loudgain")
                        .args(&album_rg_args)
                        .output()
                        .await
                    {
                        Ok(output) => {
                            if !output.status.success() {
                                let stderr = String::from_utf8_lossy(&output.stderr);
                                warn!("Album ReplayGain calculation failed: {}", stderr);
                                warn!("Track-level ReplayGain tags remain intact");
                            } else {
                                log::info!("✓ Album ReplayGain calculated for {} files", album_files.len());

                                // For M4A files, parse loudgain output and apply tags manually
                                if is_m4a {
                                    let stdout = String::from_utf8_lossy(&output.stdout);

                                    // Parse album gain and peak from output
                                    // Format: "Album\t0\t-1.86\t28959.000000\t0\t0"
                                    if let Some((album_gain, album_peak)) = parse_loudgain_album_output(&stdout) {
                                        log::debug!("Parsed album ReplayGain: gain={}, peak={}", album_gain, album_peak);

                                        // Apply album tags to all M4A files via AtomicParsley
                                        use tonepoet_backend::AacMetadataExtractor;
                                        let extractor = AacMetadataExtractor::new();

                                        for file in &album_files {
                                            // Extract existing metadata
                                            match extractor.extract(file) {
                                                Ok(mut metadata) => {
                                                    // Add album ReplayGain tags
                                                    metadata.custom_fields.insert(
                                                        "REPLAYGAIN_ALBUM_GAIN".to_string(),
                                                        album_gain.clone()
                                                    );
                                                    metadata.custom_fields.insert(
                                                        "REPLAYGAIN_ALBUM_PEAK".to_string(),
                                                        album_peak.clone()
                                                    );

                                                    // Reapply all metadata (will write lowercase atoms)
                                                    use tonepoet_backend::AacMetadataApplier;
                                                    let applier = AacMetadataApplier::new();
                                                    if let Err(e) = applier.apply(&metadata, file) {
                                                        log::warn!("Failed to apply album ReplayGain to {}: {}", file.display(), e);
                                                    }
                                                }
                                                Err(e) => {
                                                    log::warn!("Failed to extract metadata from {}: {}", file.display(), e);
                                                }
                                            }
                                        }
                                    } else {
                                        log::warn!("Failed to parse album ReplayGain from loudgain output");
                                    }
                                } else {
                                    // For non-M4A files, fix atom names if needed
                                    for file in &album_files {
                                        if file.extension().map_or(false, |ext| ext == "m4a") {
                                            if let Err(e) = fix_m4a_replaygain_atom_names(file).await {
                                                log::warn!("Failed to fix M4A ReplayGain atom names for {}: {}", file.display(), e);
                                            }
                                        }
                                    }
                                }

                                // Re-read ReplayGain values to update conversion_results
                                for result in &mut conversion_results {
                                    if item.options.calculate_replaygain {
                                        if let Some(updated_rg) = read_replaygain_tags(&result.output_file) {
                                            result.replaygain_values = Some(updated_rg);
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Failed to run album ReplayGain: {}", e);
                            warn!("Track-level ReplayGain tags remain intact");
                        }
                    }
                }
            }
        }
    }

    // Collect output file paths for CUE generation (must match conversion_results output paths)
    let output_files: Vec<PathBuf> = conversion_results.iter()
        .map(|r| r.output_file.clone())
        .collect();

    // Generate post-conversion features (log and cue files)
    if let Err(e) = post_conversion_features(
        &output_dir,
        &conversion_results,
        &output_files,
        &feature_config,
        settings_json.as_deref(),
    ).await {
        log::warn!("Failed to generate post-conversion features: {}", e);
        // Don't fail conversion for feature generation errors
    }

    // After successful conversion, move to custom output directory with atomic rename
    log::info!("[DIAG] Pre-move: item.options.output_dir = {:?}, output_dir = {:?}", item.options.output_dir, output_dir);
    if let Some(custom_output) = &item.options.output_dir {
        let target_name = output_dir.file_name().unwrap_or_else(|| std::ffi::OsStr::new("output"));
        let custom_output_dir = custom_output.join(target_name);

        tokio::fs::create_dir_all(custom_output.as_path()).await
            .map_err(|e| ConversionError::ConversionFailed(format!("Failed to create custom output directory: {}", e)))?;

        if output_dir != custom_output_dir {
            // Atomic rename with backup to prevent data loss
            if custom_output_dir.exists() {
                // Create timestamped backup path in same parent directory
                let parent_dir = custom_output.as_path();
                let folder_name = target_name.to_string_lossy();
                let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
                let backup_path = parent_dir.join(format!("{}.backup_{}", folder_name, timestamp));

                log::info!("Existing output detected, creating backup: {}", backup_path.display());

                // Step 1: Backup existing output
                tokio::fs::rename(&custom_output_dir, &backup_path).await
                    .map_err(|e| ConversionError::ConversionFailed(format!("Failed to backup existing output: {}", e)))?;

                // Step 2: Move new output to final location (supports cross-filesystem)
                match move_dir_cross_fs(&output_dir, &custom_output_dir).await {
                    Ok(_) => {
                        // Success - remove backup
                        log::info!("Move successful, removing backup");
                        if let Err(e) = tokio::fs::remove_dir_all(&backup_path).await {
                            log::warn!("Failed to remove backup (not critical): {}", e);
                        }
                    }
                    Err(e) => {
                        // Move failed - restore backup
                        log::error!("Move failed, restoring backup");
                        tokio::fs::rename(&backup_path, &custom_output_dir).await
                            .map_err(|rollback_err| ConversionError::ConversionFailed(
                                format!("Move failed AND rollback failed: move error: {}, rollback error: {}", e, rollback_err)
                            ))?;
                        return Err(ConversionError::ConversionFailed(format!("Failed to move to custom output directory: {}", e)));
                    }
                }
            } else {
                // No existing output - direct move (supports cross-filesystem)
                move_dir_cross_fs(&output_dir, &custom_output_dir).await?;
            }
            log::info!("Moved converted output to: {}", custom_output_dir.display());
        }
    }

    // Clean up temporary extraction directory if it still exists
    // Only delete if output has been moved OUT of the extraction directory
    if let Some(custom_output) = &item.options.output_dir {
        let target_name = renamed_folder.file_name().unwrap_or_else(|| std::ffi::OsStr::new("output"));
        let final_output_dir = custom_output.join(target_name);

        // Safe to delete extract_path because output was moved to custom location
        if extract_path.exists() && !final_output_dir.starts_with(&extract_path) {
            if let Err(e) = std::fs::remove_dir_all(&extract_path) {
                log::warn!("Failed to clean up extraction directory {:?}: {}", extract_path, e);
            }
        }

        // Also clean up any orphaned .extract_ directory in the source (input) directory
        // This handles leftovers from previous runs where output_dir was not set
        let source_dir = item.input_path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let source_extract_path = source_dir.join(format!(".extract_{}", decoded_stem));
        if source_extract_path != extract_path && source_extract_path.exists() {
            log::info!("Cleaning up orphaned extraction directory in source dir: {:?}", source_extract_path);
            if let Err(e) = std::fs::remove_dir_all(&source_extract_path) {
                log::warn!("Failed to clean up orphaned extraction directory {:?}: {}", source_extract_path, e);
            }
        }
    }
    // If no custom output, leave temp directory in place (it contains final output)

    Ok(())
}

/// Clean up nested subdirectories that have similar names to the parent folder
/// This removes redundant nested folders created during extraction/conversion process
async fn cleanup_duplicate_nested_folders(output_dir: &Path) -> ConversionResult<()> {
    let parent_folder_name = output_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_lowercase();
    
    if parent_folder_name.is_empty() {
        return Ok(());
    }
    
    // Read directory contents
    let mut read_dir = tokio::fs::read_dir(output_dir).await?;
    while let Some(entry) = read_dir.next_entry().await? {
        let path = entry.path();
        
        // Only examine subdirectories
        if path.is_dir() {
            if let Some(subfolder_name) = path.file_name().and_then(|name| name.to_str()) {
                let subfolder_lower = subfolder_name.to_lowercase();
                
                // Check if subfolder name is similar to parent folder name
                // This catches cases like:
                // Parent: "Devo - Be Stiff (1978) [Opus] {UK 7-inch 24-96} [PBThal]"  
                // Child:  "Devo - Be Stiff (7 Inch UK)"
                if folders_are_similar(&parent_folder_name, &subfolder_lower) {
                    log::info!("Removing duplicate nested folder: {:?}", path);
                    if let Err(e) = tokio::fs::remove_dir_all(&path).await {
                        log::warn!("Failed to remove duplicate nested folder {:?}: {}", path, e);
                    } else {
                        log::info!("Successfully removed duplicate nested folder: {:?}", path);
                    }
                }
            }
        }
    }
    
    Ok(())
}

/// Check if two folder names are similar enough to be considered duplicates
/// Uses case-insensitive matching and checks for common substring patterns
fn folders_are_similar(parent_lower: &str, child_lower: &str) -> bool {
    // Extract the core artist-album part before any metadata additions
    // Parent: "devo - be stiff (1978) [opus] {uk 7-inch 24-96} [pbthal]"
    // Child:  "devo - be stiff (7 inch uk)"
    
    // Get the base part before parentheses/brackets for parent
    let parent_base = extract_artist_album_base(parent_lower);
    let child_base = extract_artist_album_base(child_lower);
    
    // Check if the base artist-album parts match
    parent_base == child_base
}

/// Extract the artist-album base from a folder name (before metadata additions)
fn extract_artist_album_base(folder_name: &str) -> String {
    // Remove everything after first parenthesis or bracket
    let mut base = folder_name;
    
    // Find first occurrence of metadata indicators
    if let Some(pos) = folder_name.find(" (") {
        base = &folder_name[..pos];
    } else if let Some(pos) = folder_name.find(" [") {
        base = &folder_name[..pos];
    } else if let Some(pos) = folder_name.find(" {") {
        base = &folder_name[..pos];
    }
    
    // Clean up extra spaces and return
    base.trim().to_string()
}

/// Recursively find audio files in a directory
fn find_audio_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) -> ConversionResult<()> {
    use std::fs;
    
    
    for entry in fs::read_dir(dir)
        .map_err(|e| {
            log::error!("Failed to read directory {:?}: {}", dir, e);
            ConversionError::ConversionFailed(format!("Failed to read dir: {}", e))
        })? {
        let entry = entry
            .map_err(|e| ConversionError::ConversionFailed(format!("Failed to read entry: {}", e)))?;
        let path = entry.path();
        
        if path.is_dir() {
            find_audio_files_recursive(&path, files)?;
        } else if path.is_file() {
            if let Some(ext) = path.extension() {
                let ext_str = ext.to_string_lossy().to_lowercase();
                if matches!(ext_str.as_str(), 
                    "flac" | "wav" | "aiff" | "aif" | "wv" | 
                    "mp3" | "m4a" | "aac" | "opus" | "ogg" | "ape"
                ) {
                    files.push(path);
                }
            }
        }
    }
    
    Ok(())
}










