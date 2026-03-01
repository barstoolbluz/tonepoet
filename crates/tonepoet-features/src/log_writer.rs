//! Conversion log file writing implementation

use super::{ConversionResult, ConversionConfig, FeatureResult, FeatureError};
use std::path::{Path, PathBuf};
use chrono::{DateTime, Utc};
use tokio::fs;

/// Comprehensive conversion log data
#[derive(Debug, Clone)]
pub struct ConversionLogData {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub settings: ConversionLogSettings,
    pub input_summary: InputSummary,
    pub results: Vec<ConversionResult>,
    pub auxiliary_files: Vec<AuxiliaryFile>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ConversionLogSettings {
    pub backend: String,
    pub worker_count: usize,
    pub process_priority: String,
    pub output_format: String,
    pub quality_settings: String,
    pub copy_options: String,
    pub source_type: String, // "Archive" or "Individual files"
    pub merge_to_single: bool,
    pub merged_track_count: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct InputSummary {
    pub total_files: usize,
    pub source_directory: String,
    pub input_formats: Vec<(String, usize)>, // Format name, count
    pub total_input_size: u64,
}

#[derive(Debug, Clone)]
pub struct AuxiliaryFile {
    pub name: String,
    pub size: u64,
    pub action: String, // "Preserved", "Skipped", "Failed"
}

/// Write conversion log to destination directory
pub async fn write_conversion_log(
    output_dir: &Path,
    results: &[ConversionResult],
    config: &ConversionConfig,
    conversion_options: Option<&str>,
) -> FeatureResult<PathBuf> {
    // Generate log data
    let log_data = collect_log_data(output_dir, results, config, conversion_options).await?;
    
    // Generate timestamped filename
    let timestamp = log_data.timestamp.format("%Y%m%d-%H%M%S");
    let log_filename = format!("conversion-log-{}.txt", timestamp);
    let log_path = output_dir.join(&log_filename);
    
    // Generate log content
    let log_content = generate_log_content(&log_data);
    
    // Write log file
    fs::write(&log_path, log_content).await
        .map_err(|e| FeatureError::Permission(format!("Cannot write log file: {}", e)))?;
    
    log::info!("Conversion log written to: {}", log_path.display());
    
    Ok(log_path)
}

/// Derive the actual backend string from pipeline commands rather than config preference
fn derive_actual_backend(results: &[ConversionResult], preferred: &str) -> String {
    let mut tools = std::collections::BTreeSet::new();
    for result in results {
        if let Some(ref pipeline) = result.conversion_pipeline {
            for cmd in &pipeline.commands {
                // Skip no-op commands, shell wrappers, and utility commands
                match cmd.program.as_str() {
                    "true" | "sh" | "bash" | "cp" | "find" | "metaflac" | "AtomicParsley" => continue,
                    _ => { tools.insert(cmd.program.clone()); }
                }
            }
        }
    }
    if tools.is_empty() {
        return preferred.to_string();
    }

    let mut parts = Vec::new();
    if tools.contains("ffmpeg") { parts.push("FFmpeg"); }
    if tools.contains("sox") { parts.push("SoX"); }
    if tools.contains("ssrc") { parts.push("SSRC"); }
    if tools.contains("flac") { parts.push("FLAC encoder"); }
    if tools.contains("wavpack") { parts.push("WavPack"); }
    if tools.contains("loudgain") { parts.push("loudgain"); }

    if parts.is_empty() {
        // Unknown tools — fall back to preferred
        preferred.to_string()
    } else {
        parts.join(" + ")
    }
}

/// Collect all data needed for comprehensive log
async fn collect_log_data(
    output_dir: &Path,
    results: &[ConversionResult],
    config: &ConversionConfig,
    conversion_options: Option<&str>,
) -> FeatureResult<ConversionLogData> {
    let timestamp = Utc::now();
    let session_id = format!("conv_{}_{:x}",
        timestamp.format("%Y%m%d_%H%M%S"),
        timestamp.timestamp() as u32);

    // Analyze input files for summary
    let input_summary = analyze_input_files(results).await?;

    // Detect auxiliary files in output directory
    let auxiliary_files = detect_auxiliary_files(output_dir).await?;

    // Extract quality settings from conversion options if available
    let quality_settings = conversion_options
        .and_then(|json_str| format_quality_settings_from_json(json_str))
        .unwrap_or_else(|| "Quality settings from conversion".to_string());

    // Extract merge_to_single from conversion options
    let merge_to_single = conversion_options
        .and_then(|json_str| {
            serde_json::from_str::<serde_json::Value>(json_str).ok()
                .and_then(|v| v.get("merge_to_single").and_then(|m| m.as_bool()))
        })
        .unwrap_or(false);

    // Detect merged output by finding duplicate output_file entries
    let merged_track_count = if merge_to_single {
        detect_merged_output(results).map(|(_, indices)| indices.len())
    } else {
        None
    };

    // Extract settings for log
    let settings = ConversionLogSettings {
        backend: derive_actual_backend(results, &config.preferred_backend),
        worker_count: config.worker_count,
        process_priority: format_priority(config.process_priority),
        output_format: detect_output_format(results),
        quality_settings,
        copy_options: format_copy_options(config, conversion_options),
        source_type: if results.len() > 3 { "Archive".to_string() } else { "Individual files".to_string() },
        merge_to_single,
        merged_track_count,
    };
    
    Ok(ConversionLogData {
        timestamp,
        session_id,
        settings,
        input_summary,
        results: results.to_vec(),
        auxiliary_files,
        errors: collect_errors(results),
    })
}

/// Generate formatted log content
fn generate_log_content(data: &ConversionLogData) -> String {
    let mut log = String::new();
    
    // Header
    log.push_str("HEXLOAD-TUI CONVERSION LOG\n");
    log.push_str(&format!("Generated: {}\n", data.timestamp.format("%Y-%m-%d %H:%M:%S UTC")));
    log.push_str(&format!("Session ID: {}\n", data.session_id));
    log.push_str("\n============================================\n\n");
    
    // Settings section
    log.push_str("CONVERSION SETTINGS:\n");
    log.push_str(&format!("- Backend: {}\n", data.settings.backend));
    log.push_str(&format!("- Workers: {} concurrent\n", data.settings.worker_count));
    log.push_str(&format!("- Process Priority: {}\n", data.settings.process_priority));
    log.push_str(&format!("- Output Format: {}\n", data.settings.output_format));
    log.push_str(&format!("- Quality: {}\n", data.settings.quality_settings));
    log.push_str(&format!("- Copy Options: {}\n", data.settings.copy_options));
    log.push_str(&format!("- Source: {}\n", data.settings.source_type));
    if data.settings.merge_to_single {
        if let Some(count) = data.settings.merged_track_count {
            log.push_str(&format!("- Merge Tracks: Yes ({} tracks merged to single file)\n", count));
        } else {
            log.push_str("- Merge Tracks: Yes\n");
        }
    }
    log.push_str("\n");
    
    // Input files section
    log.push_str("INPUT FILES:\n");
    log.push_str(&format!("- Total: {} files processed\n", data.input_summary.total_files));
    log.push_str(&format!("- Source Directory: {}\n", data.input_summary.source_directory));
    log.push_str(&format!("- Total Input Size: {}\n", format_file_size(data.input_summary.total_input_size)));
    log.push_str("\nInput Formats:\n");
    for (format, count) in &data.input_summary.input_formats {
        log.push_str(&format!("- {}: {} files\n", format, count));
    }
    log.push_str("\n");
    
    // Conversion results section
    log.push_str("CONVERSION RESULTS:\n");

    // Check if results contain a merge operation
    if let Some((merged_file, merged_indices)) = detect_merged_output(&data.results) {
        // Display merged output
        let first_result = &data.results[merged_indices[0]];
        let timestamp = first_result.start_time.format("%H:%M:%S");
        let merged_filename = merged_file.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        let total_input: u64 = merged_indices.iter()
            .map(|&i| data.results[i].source_size)
            .sum();
        let output_size = first_result.output_size;

        let compression_reduction = 100.0 - ((output_size as f64 / total_input as f64) * 100.0);
        let size_change_label = if compression_reduction >= 0.0 {
            format!("Size Reduction: {:.1}%", compression_reduction)
        } else {
            format!("Size Increase: {:.1}%", -compression_reduction)
        };

        log.push_str(&format!(
            "[{}] ✅ Merged {} tracks → {}\n           Total Input: {} | Output: {} | {}\n",
            timestamp,
            merged_indices.len(),
            merged_filename,
            format_file_size(total_input),
            format_file_size(output_size),
            size_change_label
        ));

        // List individual tracks
        for &idx in &merged_indices {
            let r = &data.results[idx];
            let track_name = r.source_file.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");
            log.push_str(&format!(
                "           Track {:02}: {} ({})\n",
                idx + 1,
                track_name,
                format_file_size(r.source_size)
            ));
        }

        // Display source info from first track
        if let Some(ref src) = first_result.source_info {
            let mut parts = vec![src.format.clone()];

            if let Some(depth) = src.bit_depth {
                if depth == 320 {
                    parts.push("32-bit float".to_string());
                } else {
                    parts.push(format!("{}bit", depth));
                }
            }

            if let Some(rate) = src.sample_rate {
                parts.push(format!("{}kHz", rate / 1000));
            }

            if let Some(ch) = src.channels {
                if ch != 2 {
                    parts.push(format!("{}ch", ch));
                }
            }

            log.push_str(&format!("           Source: {}\n", parts.join(" ")));
        }

        // Display ReplayGain values if available
        if let Some(ref rg) = first_result.replaygain_values {
            if rg.track_gain.is_some() || rg.album_gain.is_some() {
                log.push_str("           ReplayGain:");

                if let Some(ref gain) = rg.track_gain {
                    log.push_str(&format!(" Track: {}", gain));
                }

                if let Some(ref gain) = rg.album_gain {
                    log.push_str(&format!(" | Album: {}", gain));
                }

                log.push_str("\n");
            }
        }
    } else {
        // Normal per-file display (no merge)
        for (_i, result) in data.results.iter().enumerate() {
        let timestamp = result.start_time.format("%H:%M:%S");
        let status_symbol = match result.status {
            super::ConversionStatus::Success => "✅",
            super::ConversionStatus::Failed => "❌",
        };
        
        let source_name = result.source_file.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        let output_name = result.output_file.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
            
        match result.status {
            super::ConversionStatus::Success => {
                let compression_reduction = 100.0 - result.compression_ratio();
                let size_change_label = if compression_reduction >= 0.0 {
                    format!("Size Reduction: {:.1}%", compression_reduction)
                } else {
                    format!("Size Increase: {:.1}%", -compression_reduction)
                };
                log.push_str(&format!(
                    "[{}] {} {} → {}\n           Input: {} | Output: {} | {} | Duration: {:.1}s\n",
                    timestamp,
                    status_symbol,
                    source_name,
                    output_name,
                    format_file_size(result.source_size),
                    format_file_size(result.output_size),
                    size_change_label,
                    result.duration().num_seconds()
                ));

                // Display source info if available
                log::debug!("Log writer checking source_info for {:?}: {:?}", result.source_file, result.source_info);
                if let Some(ref src) = result.source_info {
                    let mut parts = vec![src.format.clone()];

                    if let Some(depth) = src.bit_depth {
                        if depth == 320 {
                            parts.push("32-bit float".to_string());
                        } else {
                            parts.push(format!("{}bit", depth));
                        }
                    }

                    if let Some(rate) = src.sample_rate {
                        parts.push(format!("{}kHz", rate / 1000));
                    }

                    if let Some(ch) = src.channels {
                        if ch != 2 {  // Only show if not stereo
                            parts.push(format!("{}ch", ch));
                        }
                    }

                    log.push_str(&format!("           Source: {}\n", parts.join(" ")));
                }

                // Display conversion commands if available
                if let Some(ref pipeline) = result.conversion_pipeline {
                    if pipeline.commands.len() == 1 {
                        // Single command - display inline
                        let cmd = &pipeline.commands[0];
                        if cmd.program == "true" && cmd.arguments.is_empty() {
                            log.push_str(&format!("           [Skipped] {}\n", cmd.description));
                        } else {
                            log.push_str(&format!("           Command: {} {}\n",
                                cmd.program, cmd.arguments.join(" ")));
                        }
                    } else if pipeline.commands.len() > 1 {
                        // Multiple commands - display numbered list
                        log.push_str("           Commands:\n");
                        for (idx, cmd) in pipeline.commands.iter().enumerate() {
                            // For "true" (no-op) commands, show the description instead
                            if cmd.program == "true" && cmd.arguments.is_empty() {
                                log.push_str(&format!("             {}. [Skipped] {}\n",
                                    idx + 1, cmd.description));
                            } else {
                                log.push_str(&format!("             {}. {} {}\n",
                                    idx + 1, cmd.program, cmd.arguments.join(" ")));
                            }
                        }
                    }
                }

                // Display ReplayGain values if available
                if let Some(ref rg) = result.replaygain_values {
                    if rg.track_gain.is_some() || rg.album_gain.is_some() {
                        log.push_str("           ReplayGain:");

                        if let Some(ref gain) = rg.track_gain {
                            log.push_str(&format!(" Track: {}", gain));
                        }

                        if let Some(ref gain) = rg.album_gain {
                            log.push_str(&format!(" | Album: {}", gain));
                        }

                        log.push_str("\n");
                    }
                }
            }
            super::ConversionStatus::Failed => {
                log.push_str(&format!(
                    "[{}] {} {} → CONVERSION FAILED\n           Error: {}\n           Duration: {:.1}s\n",
                    timestamp,
                    status_symbol,
                    source_name,
                    result.error_message.as_deref().unwrap_or("Unknown error"),
                    result.duration().num_seconds()
                ));
            }
        }
        }
    }
    log.push_str("\n");
    
    // Summary statistics
    let successful = data.results.iter().filter(|r| matches!(r.status, super::ConversionStatus::Success)).count();
    let failed = data.results.len() - successful;
    let total_input_size: u64 = data.results.iter().map(|r| r.source_size).sum();

    // Calculate total output size, accounting for merged files
    let total_output_size: u64 = if let Some((_, merged_indices)) = detect_merged_output(&data.results) {
        // For merged files: count merged output once + any non-merged outputs
        let merged_output_size = data.results[merged_indices[0]].output_size;
        let non_merged_size: u64 = data.results.iter()
            .enumerate()
            .filter(|(i, r)| !merged_indices.contains(i) && matches!(r.status, super::ConversionStatus::Success))
            .map(|(_, r)| r.output_size)
            .sum();
        merged_output_size + non_merged_size
    } else {
        // No merges: sum all output sizes normally
        data.results.iter()
            .filter(|r| matches!(r.status, super::ConversionStatus::Success))
            .map(|r| r.output_size)
            .sum()
    };
    let avg_compression = if total_input_size > 0 {
        (total_output_size as f32 / total_input_size as f32) * 100.0
    } else {
        0.0
    };

    // Calculate total duration, accounting for merged files
    let total_duration: i64 = if let Some((_, merged_indices)) = detect_merged_output(&data.results) {
        // For merged files: count merged duration once + any non-merged durations
        let merged_duration = data.results[merged_indices[0]].duration().num_seconds();
        let non_merged_duration: i64 = data.results.iter()
            .enumerate()
            .filter(|(i, _)| !merged_indices.contains(i))
            .map(|(_, r)| r.duration().num_seconds())
            .sum();
        merged_duration + non_merged_duration
    } else {
        // No merges: sum all durations normally
        data.results.iter()
            .map(|r| r.duration().num_seconds())
            .sum()
    };
        
    log.push_str("CONVERSION SUMMARY:\n");
    log.push_str(&format!("- Successful: {}/{} files ({:.1}%)\n", successful, data.results.len(), (successful as f32 / data.results.len() as f32) * 100.0));
    if failed > 0 {
        log.push_str(&format!("- Failed: {}/{} files ({:.1}%)\n", failed, data.results.len(), (failed as f32 / data.results.len() as f32) * 100.0));
    }
    log.push_str(&format!("- Total Input Size: {}\n", format_file_size(total_input_size)));
    log.push_str(&format!("- Total Output Size: {}\n", format_file_size(total_output_size)));
    let avg_reduction = 100.0 - avg_compression;
    if avg_reduction >= 0.0 {
        log.push_str(&format!("- Average Size Reduction: {:.1}%\n", avg_reduction));
    } else {
        log.push_str(&format!("- Average Size Increase: {:.1}%\n", -avg_reduction));
    }
    log.push_str(&format!("- Total Duration: {} minutes {} seconds\n", total_duration / 60, total_duration % 60));
    if total_duration > 0 {
        log.push_str(&format!("- Average Speed: {:.1} files/minute\n", (data.results.len() as f32 / total_duration as f32) * 60.0));
    }
    log.push_str(&format!("- Backend: {} engine\n", data.settings.backend));
    log.push_str("\n");
    
    // Errors section (if any)
    if !data.errors.is_empty() {
        log.push_str("CONVERSION ERRORS:\n");
        for error in &data.errors {
            log.push_str(&format!("- {}\n", error));
        }
        log.push_str("\n");
    }
    
    // Auxiliary files section
    if !data.auxiliary_files.is_empty() {
        log.push_str("AUXILIARY FILES PROCESSED:\n");
        for aux in &data.auxiliary_files {
            log.push_str(&format!("- {} ({}) - {}\n", aux.name, format_file_size(aux.size), aux.action));
        }
        log.push_str("\n");
    }
    
    // Footer
    log.push_str("============================================\n");
    log.push_str("Log generated by tonepoet v0.1.0\n");
    log.push_str("\nEND OF CONVERSION LOG\n");
    
    log
}

// Helper functions
async fn analyze_input_files(results: &[ConversionResult]) -> FeatureResult<InputSummary> {
    use std::collections::HashMap;

    // Analyze file formats
    let mut format_counts: HashMap<String, usize> = HashMap::new();
    let mut source_dir = String::new();

    for result in results {
        // Extract format from source file extension
        let format = result.source_file.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("unknown")
            .to_uppercase();
        *format_counts.entry(format).or_insert(0) += 1;

        // Extract source directory from first file
        if source_dir.is_empty() {
            source_dir = result.source_file.parent()
                .and_then(|p| p.to_str())
                .unwrap_or("Unknown")
                .to_string();
        }
    }

    let mut input_formats: Vec<(String, usize)> = format_counts.into_iter().collect();
    input_formats.sort_by(|a, b| b.1.cmp(&a.1)); // Sort by count descending

    Ok(InputSummary {
        total_files: results.len(),
        source_directory: if source_dir.is_empty() { "Current directory".to_string() } else { source_dir },
        input_formats,
        total_input_size: results.iter().map(|r| r.source_size).sum(),
    })
}

async fn detect_auxiliary_files(_output_dir: &Path) -> FeatureResult<Vec<AuxiliaryFile>> {
    // Detect non-audio files in output directory
    let mut auxiliary_files = Vec::new();

    let mut entries = tokio::fs::read_dir(_output_dir).await
        .map_err(|e| FeatureError::Io(e))?;

    while let Some(entry) = entries.next_entry().await
        .map_err(|e| FeatureError::Io(e))? {
        let path = entry.path();
        if path.is_file() {
            let filename = path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");

            // Check if it's an auxiliary file (not audio)
            let is_auxiliary = match path.extension().and_then(|e| e.to_str()) {
                Some("txt") | Some("ini") | Some("jpg") | Some("jpeg") |
                Some("png") | Some("pdf") | Some("log") => true,
                Some("opus") | Some("mp3") | Some("flac") | Some("aac") |
                Some("wav") | Some("aiff") | Some("cue") => false,
                _ => false,
            };

            if is_auxiliary {
                let metadata = tokio::fs::metadata(&path).await
                    .map_err(|e| FeatureError::Io(e))?;
                auxiliary_files.push(AuxiliaryFile {
                    name: filename.to_string(),
                    size: metadata.len(),
                    action: "Preserved".to_string(),
                });
            }
        }
    }

    Ok(auxiliary_files)
}

fn format_priority(priority: i8) -> String {
    match priority {
        -20..=-1 => "High".to_string(),
        0 => "Normal".to_string(),
        1..=10 => "Low".to_string(),
        _ => "Very Low".to_string(),
    }
}

fn detect_output_format(_results: &[ConversionResult]) -> String {
    // Detect format from first successful result
    for result in _results {
        if matches!(result.status, super::ConversionStatus::Success) {
            if let Some(ext) = result.output_file.extension().and_then(|e| e.to_str()) {
                return match ext.to_lowercase().as_str() {
                    "opus" => "Opus".to_string(),
                    "mp3" => "MP3".to_string(),
                    "flac" => "FLAC".to_string(),
                    "aac" | "m4a" => "AAC".to_string(),
                    "wav" => "WAV".to_string(),
                    "aiff" | "aif" => "AIFF".to_string(),
                    _ => format!("Unknown ({})", ext),
                };
            }
        }
    }
    "Unknown".to_string()
}

fn format_copy_options(_config: &ConversionConfig, conversion_options: Option<&str>) -> String {
    if let Some(json_str) = conversion_options {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(json_str) {
            // Extract boolean fields (default to true per ConversionOptions defaults)
            let copy_aux = value.get("copy_auxiliary_files").and_then(|v| v.as_bool()).unwrap_or(true);
            let copy_subdirs = value.get("copy_subdirectories").and_then(|v| v.as_bool()).unwrap_or(true);

            if is_copy_mode(&value) {
                // Copy mode: show FLAC + settings
                let aux_str = if copy_aux { "yes" } else { "no" };
                let subdir_str = if copy_subdirs { "yes" } else { "no" };
                return format!("FLAC copied without transcoding (Auxiliary files: {}, Subdirectories: {})", aux_str, subdir_str);
            } else {
                // Normal conversion: show settings
                let aux_str = if copy_aux { "enabled" } else { "disabled" };
                let subdir_str = if copy_subdirs { "enabled" } else { "disabled" };
                return format!("Auxiliary files: {}, Subdirectories: {}", aux_str, subdir_str);
            }
        }
    }

    // Fallback if no JSON or parse failed (use correct defaults)
    "Auxiliary files: enabled, Subdirectories: enabled".to_string()
}

fn collect_errors(results: &[ConversionResult]) -> Vec<String> {
    results.iter()
        .filter_map(|r| r.error_message.clone())
        .collect()
}

/// Detect merged output by finding multiple results with same output_file
fn detect_merged_output(results: &[ConversionResult]) -> Option<(PathBuf, Vec<usize>)> {
    use std::collections::HashMap;

    let mut output_map: HashMap<PathBuf, Vec<usize>> = HashMap::new();

    for (i, result) in results.iter().enumerate() {
        output_map.entry(result.output_file.clone())
            .or_insert_with(Vec::new)
            .push(i);
    }

    // Find output with multiple sources (indicates merge)
    output_map.into_iter()
        .find(|(_, indices)| indices.len() > 1)
}

fn format_file_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{} {}", bytes, UNITS[unit_index])
    } else {
        format!("{:.1} {}", size, UNITS[unit_index])
    }
}

/// Format quality settings from ConversionSettings JSON value
fn format_from_conversion_settings(value: &serde_json::Value) -> Option<String> {
    let format = value.get("format")?.as_str()?;
    let selected_quality = value.get("selected_quality").and_then(|v| v.as_str());

    let mut parts = Vec::new();

    // Check for copy mode FIRST
    if is_copy_mode(value) {
        parts.push("FLAC (copied)".to_string());

        // Only include ReplayGain if present (skip compression/resample)
        if let Some(rg_mode) = value.get("replaygain_mode").and_then(|v| v.as_str()) {
            parts.push(format!("ReplayGain: {}", rg_mode));
        }

        return Some(parts.join(", "));
    }

    // Format main quality
    let format_str = match format {
        "Opus" => {
            // Map quality descriptor to bitrate
            let bitrate = match selected_quality {
                Some("Insane") => 320,
                Some("Very High") => 256,
                Some("High") => 192,
                Some("Medium") => 128,
                Some("Low") => 64,
                _ => 128,
            };
            format!("Opus {} kbps", bitrate)
        },
        "Flac" => {
            let level = value.get("compression_level").and_then(|v| v.as_u64()).unwrap_or(8);
            format!("FLAC compression level {}", level)
        },
        "Mp3" => {
            if let Some(bitrate) = value.get("mp3_bitrate").and_then(|v| v.as_u64()) {
                format!("MP3 {} kbps", bitrate)
            } else {
                "MP3".to_string()
            }
        },
        "Aac" => {
            let profile = value.get("aac_profile").and_then(|v| v.as_str()).unwrap_or("LcAac");
            format!("AAC {}", profile)
        },
        "Wav" | "Aiff" => {
            let bit_depth = value.get("bit_depth").and_then(|v| v.as_u64()).unwrap_or(16);
            let sample_rate = value.get("sample_rate").and_then(|v| v.as_u64()).unwrap_or(44100);
            let depth_str = if bit_depth == 0 {
                "Same As Source".to_string()
            } else if bit_depth == 320 || bit_depth == 33 {
                "32-bit float".to_string()
            } else {
                format!("{}bit", bit_depth)
            };
            format!("{} {}kHz/{}", format, sample_rate / 1000, depth_str)
        },
        _ => format.to_string(),
    };
    parts.push(format_str);

    // Add dither type (only when bit depth reduction is requested)
    if let Some(dither) = value.get("dither_type").and_then(|v| v.as_str()) {
        let bit_depth = value.get("bit_depth").and_then(|v| v.as_u64());
        if bit_depth.is_some() && bit_depth != Some(0) {
            parts.push(format!("Dither: {}", dither));
        }
    }

    // Add nyquist transition / filter (only when resampling is requested)
    if let Some(nyquist) = value.get("nyquist_transition").and_then(|v| v.as_str()) {
        let sample_rate = value.get("sample_rate").and_then(|v| v.as_u64());
        if sample_rate.is_some() && sample_rate != Some(0) {
            let mut filter_text = format!("Filter: {}", nyquist);
            if nyquist == "BrickWall" {
                if let Some(true) = value.get("ssrc_insane_mode").and_then(|v| v.as_bool()) {
                    filter_text.push_str(" (Insane)");
                }
            }
            parts.push(filter_text);
        }
    }

    // Add opus content type
    if format == "Opus" {
        if let Some(content) = value.get("opus_content_type").and_then(|v| v.as_str()) {
            parts.push(format!("Content: {}", content));
        }
    }

    // Add verify encoding
    if let Some(true) = value.get("verify_encoding").and_then(|v| v.as_bool()) {
        parts.push("Verify: Enabled".to_string());
    }

    // Add ReplayGain
    if let Some(rg_mode) = value.get("replaygain_mode").and_then(|v| v.as_str()) {
        parts.push(format!("ReplayGain: {}", rg_mode));
    }

    // Add resample quality (only when resampling is requested)
    if let Some(resample) = value.get("resample_quality").and_then(|v| v.as_u64()) {
        let sample_rate = value.get("sample_rate").and_then(|v| v.as_u64());
        if sample_rate.is_some() && sample_rate != Some(0) {
            let quality_name = match resample {
                0 => "Ultra",
                1 => "VHQ",
                2 => "HQ",
                3 => "MQ",
                4 => "LQ",
                _ => "Unknown",
            };
            parts.push(format!("Resample: {}", quality_name));
        }
    }

    Some(parts.join(", "))
}

/// Detect if copy mode was used (FLAC→FLAC with no processing)
fn is_copy_mode(value: &serde_json::Value) -> bool {
    // Check ConversionSettings format (has "format" field)
    if let Some(format) = value.get("format").and_then(|v| v.as_str()) {
        if format != "Flac" { return false; }

        let reencode = value.get("reencode_flac").and_then(|v| v.as_bool()).unwrap_or(false);
        if reencode { return false; }

        let bit_depth = value.get("bit_depth").and_then(|v| v.as_u64());
        let sample_rate = value.get("sample_rate").and_then(|v| v.as_u64());

        // No resampling/bit depth change = copy mode
        return (bit_depth.is_none() || bit_depth == Some(0))
            && (sample_rate.is_none() || sample_rate == Some(0));
    }

    // Check ConversionOptions format (has "output_format" field)
    if let Some(format) = value.get("output_format").and_then(|v| v.as_str()) {
        if format != "Flac" { return false; }

        let reencode = value.get("reencode_flac").and_then(|v| v.as_bool()).unwrap_or(false);
        if reencode { return false; }

        let target_sr = value.get("target_sample_rate").and_then(|v| v.as_u64());
        let target_bd = value.get("target_bit_depth").and_then(|v| v.as_u64());
        let dither = value.get("dither_type");

        // Match copy mode conditions from processor.rs:1540-1546
        return (target_sr.is_none() || target_sr == Some(0))
            && (target_bd.is_none() || target_bd == Some(0))
            && !(dither.is_some() && target_bd.is_some()
                 && (target_bd == Some(16) || target_bd == Some(24)));
    }

    false
}

/// Format quality settings from JSON-serialized ConversionSettings or ConversionOptions
fn format_quality_settings_from_json(json_str: &str) -> Option<String> {
    // Parse the JSON to extract quality settings
    let value: serde_json::Value = serde_json::from_str(json_str).ok()?;

    // Check if this is ConversionSettings (has "format" field) or ConversionOptions (has "output_format")
    let is_conversion_settings = value.get("format").is_some();

    if is_conversion_settings {
        // Parse as ConversionSettings for comprehensive logging
        return format_from_conversion_settings(&value);
    }

    // Fall back to ConversionOptions parsing
    let output_format = value.get("output_format")?.as_str()?;

    // Check for copy mode in ConversionOptions path
    if is_copy_mode(&value) {
        let mut parts = vec!["FLAC (copied)".to_string()];

        // Only add ReplayGain if enabled
        if let Some(true) = value.get("calculate_replaygain").and_then(|v| v.as_bool()) {
            if let Some(mode) = value.get("replaygain_mode").and_then(|v| v.as_str()) {
                parts.push(format!("ReplayGain: {}", mode));
            }
        }

        return Some(parts.join(", "));
    }

    let quality = value.get("quality")?;

    let settings_str = match output_format {
        "Flac" => {
            let level = quality.get("Flac")?.get("compression_level")?.as_u64()?;
            format!("FLAC compression level {}", level)
        },
        "Wav" => {
            let wav = quality.get("Wav")?;
            let bit_depth = wav.get("bit_depth")?.as_u64()?;
            let sample_rate = wav.get("sample_rate")?.as_u64()?;
            let depth_str = if bit_depth == 0 {
                "Same As Source".to_string()
            } else if bit_depth == 320 || bit_depth == 33 {
                "32-bit float".to_string()
            } else {
                format!("{}bit", bit_depth)
            };
            format!("WAV {}kHz/{}", sample_rate / 1000, depth_str)
        },
        "Aiff" => {
            let aiff = quality.get("Aiff")?;
            let bit_depth = aiff.get("bit_depth")?.as_u64()?;
            let sample_rate = aiff.get("sample_rate")?.as_u64()?;
            let depth_str = if bit_depth == 0 {
                "Same As Source".to_string()
            } else if bit_depth == 320 || bit_depth == 33 {
                "32-bit float".to_string()
            } else {
                format!("{}bit", bit_depth)
            };
            format!("AIFF {}kHz/{}", sample_rate / 1000, depth_str)
        },
        "Mp3" => {
            let mp3 = quality.get("Mp3")?;
            let bitrate_mode = mp3.get("bitrate_mode")?;
            if let Some(cbr) = bitrate_mode.get("Cbr") {
                let bitrate = cbr.get("bitrate")?.as_u64()?;
                format!("MP3 CBR {} kbps", bitrate)
            } else if let Some(vbr) = bitrate_mode.get("Vbr") {
                let vbr_quality = vbr.get("quality")?.as_u64()?;
                format!("MP3 VBR V{}", vbr_quality)
            } else if let Some(abr) = bitrate_mode.get("Abr") {
                let bitrate = abr.get("bitrate")?.as_u64()?;
                format!("MP3 ABR {} kbps", bitrate)
            } else {
                "MP3".to_string()
            }
        },
        "Aac" => {
            let aac = quality.get("Aac")?;
            let bitrate = aac.get("bitrate")?.as_u64()?;
            let profile = aac.get("profile")?.as_str()?;
            format!("AAC {} {} kbps", profile, bitrate)
        },
        "Opus" => {
            let opus = quality.get("Opus")?;
            let bitrate = opus.get("bitrate")?.as_u64()?;
            let complexity = opus.get("complexity")?.as_u64()?;
            format!("Opus {} kbps (complexity {})", bitrate, complexity)
        },
        "WavPack" => {
            let wv = quality.get("WavPack")?;
            let mode = wv.get("compression_mode")?.as_str()?;
            format!("WavPack {}", mode)
        },
        _ => "Unknown format".to_string(),
    };

    // Add ReplayGain info if enabled
    let mut full_settings = settings_str;
    if let Some(true) = value.get("calculate_replaygain").and_then(|v| v.as_bool()) {
        if let Some(mode) = value.get("replaygain_mode").and_then(|v| v.as_str()) {
            full_settings.push_str(&format!(", ReplayGain: {}", mode));
        } else {
            full_settings.push_str(", ReplayGain: enabled");
        }
    }

    // Add SSRC Insane mode if enabled (overrides quality)
    if let Some(true) = value.get("ssrc_insane_mode").and_then(|v| v.as_bool()) {
        if let Some("BrickWall") = value.get("nyquist_transition").and_then(|v| v.as_str()) {
            full_settings.push_str(", Resample: Insane (200 dB)");
        }
    } else if let Some(resample) = value.get("resample_quality").and_then(|v| v.as_u64()) {
        // Add resample quality if specified
        let quality_name = match resample {
            0 => "Ultra",
            1 => "VHQ",
            2 => "HQ",
            3 => "MQ",
            4 => "LQ",
            _ => "Unknown",
        };
        full_settings.push_str(&format!(", Resample: {}", quality_name));
    }

    // Add nyquist transition filter info
    if let Some(nyquist) = value.get("nyquist_transition").and_then(|v| v.as_str()) {
        full_settings.push_str(&format!(", Filter: {}", nyquist));
    }

    Some(full_settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_file_size_formatting() {
        assert_eq!(format_file_size(500), "500 B");
        assert_eq!(format_file_size(1536), "1.5 KB");
        assert_eq!(format_file_size(2097152), "2.0 MB");
    }
    
    #[test]
    fn test_compression_ratio() {
        let result = ConversionResult {
            source_file: PathBuf::from("test.flac"),
            output_file: PathBuf::from("test.opus"),
            status: super::super::ConversionStatus::Success,
            source_size: 10000000, // 10MB
            output_size: 8000000,  // 8MB
            start_time: Utc::now(),
            end_time: Utc::now(),
            error_message: None,
        };
        
        assert_eq!(result.compression_ratio(), 80.0); // 80% of original size
    }
}