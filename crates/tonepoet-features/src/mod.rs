//! Conversion Features Implementation
//! 
//! Implements log file writing and cue file generation functionality
//! for the options configured in the Options Wizard.

pub mod log_writer;
pub mod cue_generator;

pub use log_writer::{write_conversion_log, ConversionLogData, ConversionLogSettings};
pub use cue_generator::{generate_cue_file, AlbumMetadata, CueFileError};

// Re-export common types
use std::path::PathBuf;
use std::collections::HashSet;
use chrono::{DateTime, Utc};
use tonepoet_backend::ConversionPipeline;

/// Result type for conversion feature operations
pub type FeatureResult<T> = Result<T, FeatureError>;

/// Errors that can occur during feature implementation
#[derive(Debug, thiserror::Error)]
pub enum FeatureError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("Metadata extraction error: {0}")]
    Metadata(String),
    
    #[error("File format error: {0}")]
    Format(String),
    
    #[error("Permission denied: {0}")]
    Permission(String),
}

/// Configuration struct that matches main app's conversion config
#[derive(Debug, Clone)]
pub struct ConversionConfig {
    pub write_log_file: bool,
    pub generate_cue_files: bool,
    pub cue_generation_mode: String, // "Always" or "IfMerging"
    pub preferred_backend: String,
    pub worker_count: usize,
    pub process_priority: i8,
    pub overwrite_behavior: String,
}

/// Integration helper to check if features should be enabled
pub fn should_write_log(config: &ConversionConfig) -> bool {
    config.write_log_file
}

/// Integration function for main conversion processor
pub async fn post_conversion_features(
    output_dir: &PathBuf,
    conversion_results: &[ConversionResult],
    audio_files: &[PathBuf],
    config: &ConversionConfig,
    conversion_options: Option<&str>, // JSON-serialized ConversionOptions for logging
) -> FeatureResult<()> {
    // Generate log file if enabled
    if should_write_log(config) {
        if let Err(e) = write_conversion_log(output_dir, conversion_results, config, conversion_options).await {
            log::warn!("Failed to write conversion log: {}", e);
            // Don't propagate error - log writing shouldn't break conversions
        }
    }

    // Generate CUE file if enabled
    log::debug!("CUE check: generate_cue_files={}, mode={}", config.generate_cue_files, config.cue_generation_mode);
    if config.generate_cue_files {
        // Detect merge: multiple sources -> single output
        let unique_outputs: HashSet<_> = conversion_results
            .iter()
            .map(|r| &r.output_file)
            .collect();
        let is_merge = conversion_results.len() > 1 && unique_outputs.len() == 1;
        log::debug!("CUE merge detection: results={}, unique_outputs={}, is_merge={}",
            conversion_results.len(), unique_outputs.len(), is_merge);

        // For non-merge operations, generate CUE based on mode setting
        // For merge operations, skip here (already handled at merge sites in processor.rs)
        if !is_merge {
            let should_generate = match config.cue_generation_mode.as_str() {
                "Always" => true,
                "If merging multiple tracks" => false,  // Don't generate for non-merge when mode is IfMerging
                "IfMerging" => false,  // Legacy value support
                _ => false,
            };
            log::debug!("CUE should_generate={} for mode={}", should_generate, config.cue_generation_mode);

            if should_generate {
                log::info!("Generating CUE file for {} audio files in {:?}", audio_files.len(), output_dir);
                if let Err(e) = generate_cue_file(output_dir, audio_files, config, conversion_results).await {
                    log::warn!("Failed to generate cue file: {}", e);
                    // Don't propagate error - cue generation shouldn't break conversions
                } else {
                    log::info!("CUE file generated successfully");
                }
            }
        } else {
            log::debug!("Skipping CUE generation for merge (handled at merge site)");
        }
    }

    Ok(())
}

/// ReplayGain values extracted from audio file tags
#[derive(Debug, Clone)]
pub struct ReplayGainValues {
    pub track_gain: Option<String>,
    pub track_peak: Option<String>,
    pub album_gain: Option<String>,
    pub album_peak: Option<String>,
}

/// Source file information detected during conversion
#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub format: String,           // "FLAC", "WAV", "AIFF", "MP3", etc.
    pub bit_depth: Option<u16>,   // Some(24), Some(320) for float, None for lossy
    pub sample_rate: Option<u32>, // Some(96000), etc.
    pub channels: Option<u8>,     // Some(2), etc.
}

/// Conversion result data for logging
#[derive(Debug, Clone)]
pub struct ConversionResult {
    pub source_file: PathBuf,
    pub output_file: PathBuf,
    pub status: ConversionStatus,
    pub source_size: u64,
    pub output_size: u64,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub error_message: Option<String>,
    pub replaygain_values: Option<ReplayGainValues>,
    pub source_info: Option<SourceInfo>,
    pub conversion_pipeline: Option<ConversionPipeline>,
}

#[derive(Debug, Clone)]
pub enum ConversionStatus {
    Success,
    Failed,
}

impl ConversionResult {
    pub fn compression_ratio(&self) -> f32 {
        if self.source_size > 0 {
            (self.output_size as f32 / self.source_size as f32) * 100.0
        } else {
            0.0
        }
    }
    
    pub fn duration(&self) -> chrono::Duration {
        self.end_time - self.start_time
    }
}