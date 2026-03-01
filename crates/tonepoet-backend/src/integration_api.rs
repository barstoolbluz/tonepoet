//! Complete integration API for tonepoet conversion backend
//!
//! This module provides the complete interface that the main project needs
//! to integrate the conversion backend into its concurrent processing system.

use crate::types::*;
use crate::integration::*;
use crate::{Result, Backend, PipelineBuilder, CommandBuilder, ConversionPipeline};
use std::path::Path;
use tokio::sync::mpsc;
use std::collections::HashMap;

/// Complete conversion backend interface for main project integration
pub struct ConversionBackend {
    preferred_backend: Backend,
    pipeline_builder: PipelineBuilder,
}

impl ConversionBackend {
    /// Create new conversion backend instance
    pub fn new(preferred_backend: Backend) -> Self {
        Self {
            preferred_backend,
            pipeline_builder: PipelineBuilder::new(preferred_backend),
        }
    }
    
    /// Convert a single ConversionItem using the conversion backend
    /// This is the main interface that replaces the old format-specific convert functions
    pub async fn convert_item(
        &self,
        item: &ConversionItem,
        input_path: &Path,
        output_path: &Path,
        progress_tx: &mpsc::Sender<ProgressUpdate>,
    ) -> Result<(std::path::PathBuf, ConversionPipeline)> {
        // Map main project types to backend settings
        let settings = map_conversion_item_to_settings(item);

        // Call private helper with settings
        self.convert_item_with_settings(item, input_path, output_path, progress_tx, settings).await
    }

    /// Convert item with pre-built settings (allows injecting lineage path)
    /// This is a private helper to support settings modification
    async fn convert_item_with_settings(
        &self,
        item: &ConversionItem,
        input_path: &Path,
        output_path: &Path,
        progress_tx: &mpsc::Sender<ProgressUpdate>,
        settings: ConversionSettings,
    ) -> Result<(std::path::PathBuf, ConversionPipeline)> {
        // Send initial progress update
        let _ = progress_tx.send(ProgressUpdate {
            item_id: item.id.clone(),
            progress: 40.0, // Start of Converting phase
            status: ConversionStatus::Processing {
                progress: 40.0,
                message: Some(format!("Starting conversion to {}", settings.format.extension())),
                file_progress: None,
                phase: Some(ConversionPhase::Converting),
                phase_progress: Some(0.0),
            },
        }).await;

        // Build conversion pipeline
        let pipeline = self.pipeline_builder.build_pipeline(
            input_path,
            output_path,
            &settings,
        )?;

        // Clone pipeline for logging before execution
        let pipeline_for_logging = pipeline.clone();

        // Execute pipeline with phase progress integration
        let _outputs = pipeline.execute_with_phase_progress(
            progress_tx,
            &item.id,
            ConversionPhase::Converting,
        ).await?;

        // Send completion status
        let _ = progress_tx.send(ProgressUpdate {
            item_id: item.id.clone(),
            progress: 90.0, // End of Converting phase
            status: ConversionStatus::Completed {
                output_path: output_path.to_path_buf()
            },
        }).await;

        Ok((output_path.to_path_buf(), pipeline_for_logging))
    }
    
    /// Check if all required tools are available for the conversion backend
    pub fn check_tool_availability(&self) -> Result<ToolAvailability> {
        let mut available_tools = HashMap::new();
        let mut missing_tools = Vec::new();
        
        // Check core backends
        if CommandBuilder::new(Backend::FFmpeg).is_available() {
            available_tools.insert("ffmpeg".to_string(), true);
        } else {
            missing_tools.push("ffmpeg".to_string());
        }
        
        if CommandBuilder::new(Backend::Sox).is_available() {
            available_tools.insert("sox".to_string(), true);
        } else {
            missing_tools.push("sox".to_string());
        }
        
        // Check specialized tools
        let specialized_tools = vec![
            "ssrc",     // Brick wall resampling
            "flac",     // FLAC encoding/decoding
            "metaflac", // FLAC metadata 
            "loudgain", // ReplayGain analysis
            "opusenc",  // Opus encoding
            "wavpack",  // WavPack encoding
        ];
        
        for tool in specialized_tools {
            if self.check_tool_available(tool) {
                available_tools.insert(tool.to_string(), true);
            } else {
                available_tools.insert(tool.to_string(), false);
            }
        }
        
        let backend_functional = missing_tools.is_empty();
        
        Ok(ToolAvailability {
            available_tools,
            missing_critical_tools: missing_tools,
            backend_functional,
        })
    }
    
    /// Check if a specific tool is available
    fn check_tool_available(&self, tool: &str) -> bool {
        // Some tools (like ssrc) don't support --version or --help and always return non-zero
        // For those, we check if the command can be found at all
        let arg = match tool {
            "opusenc" => "--help",
            "ssrc" => {
                // ssrc always exits non-zero; just check if the binary is found
                return std::process::Command::new(tool)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .is_ok();
            }
            _ => "--version",
        };
        std::process::Command::new(tool)
            .arg(arg)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    
    /// Get conversion capabilities for a given format
    pub fn get_format_capabilities(&self, format: AudioFormat) -> FormatCapabilities {
        FormatCapabilities {
            supports_float: format.supports_float(),
            supports_high_sample_rates: true, // All formats support high rates
            supports_replaygain: true, // All formats support ReplayGain
            optimal_backend: self.get_optimal_backend_for_format(format),
            specialized_tools: self.get_specialized_tools_for_format(format),
        }
    }
    
    /// Determine optimal backend for a specific format
    fn get_optimal_backend_for_format(&self, format: AudioFormat) -> Backend {
        match format {
            // FFmpeg is generally better for modern formats
            AudioFormat::Opus | AudioFormat::Aac => Backend::FFmpeg,
            // SoX is better for advanced audio processing
            AudioFormat::Wav | AudioFormat::Aiff => {
                if self.preferred_backend == Backend::Sox {
                    Backend::Sox
                } else {
                    Backend::FFmpeg
                }
            },
            // Use preferred backend for other formats
            _ => self.preferred_backend,
        }
    }
    
    /// Get specialized tools needed for a format
    fn get_specialized_tools_for_format(&self, format: AudioFormat) -> Vec<String> {
        match format {
            AudioFormat::Flac => vec!["flac".to_string(), "metaflac".to_string()],
            AudioFormat::Opus => vec!["opusenc".to_string()],
            AudioFormat::WavPack => vec!["wavpack".to_string()],
            _ => vec![],
        }
    }
}

/// Tool availability information
#[derive(Debug)]
pub struct ToolAvailability {
    pub available_tools: HashMap<String, bool>,
    pub missing_critical_tools: Vec<String>,
    pub backend_functional: bool,
}

/// Format capability information
#[derive(Debug)]
pub struct FormatCapabilities {
    pub supports_float: bool,
    pub supports_high_sample_rates: bool,
    pub supports_replaygain: bool,
    pub optimal_backend: Backend,
    pub specialized_tools: Vec<String>,
}

impl ToolAvailability {
    /// Check if a specific advanced feature is available
    pub fn supports_brick_wall_resampling(&self) -> bool {
        self.available_tools.get("ssrc").copied().unwrap_or(false)
    }
    
    pub fn supports_gesemann_dithering(&self) -> bool {
        self.available_tools.get("sox").copied().unwrap_or(false)
    }
    
    pub fn supports_format_specific_encoding(&self, format: AudioFormat) -> bool {
        match format {
            AudioFormat::Flac => {
                self.available_tools.get("flac").copied().unwrap_or(false)
            },
            AudioFormat::Opus => {
                self.available_tools.get("opusenc").copied().unwrap_or(false)
            },
            AudioFormat::WavPack => {
                self.available_tools.get("wavpack").copied().unwrap_or(false)
            },
            _ => true, // FFmpeg/SoX can handle other formats
        }
    }
}

/// Find Lineage.txt in source file's directory
fn find_lineage_file(source_file: &Path) -> Option<std::path::PathBuf> {
    let source_dir = source_file.parent()?;

    // Try case-insensitive variants
    for filename in ["Lineage.txt", "lineage.txt", "LINEAGE.TXT"] {
        let lineage_path = source_dir.join(filename);
        if lineage_path.exists() && lineage_path.is_file() {
            log::info!("Found {} in {}", filename, source_dir.display());
            return Some(lineage_path);
        }
    }

    log::debug!("No Lineage.txt found in {}", source_dir.display());
    None
}

/// Convenience function for main project integration
/// This is the primary function the main project should call
pub async fn convert_with_backend(
    item: &ConversionItem,
    input_path: &Path,
    output_path: &Path,
    progress_tx: &mpsc::Sender<ProgressUpdate>,
    preferred_backend: Option<Backend>,
) -> Result<(std::path::PathBuf, ConversionPipeline)> {
    let backend = ConversionBackend::new(preferred_backend.unwrap_or(Backend::FFmpeg));

    // Map settings
    let mut settings = map_conversion_item_to_settings(item);

    // If user enabled lineage feature, find Lineage.txt file
    if item.append_lineage {
        log::debug!("Lineage feature is ENABLED for {}", input_path.display());
        settings.lineage_file_path = find_lineage_file(input_path);
        if settings.lineage_file_path.is_none() {
            log::debug!("Lineage feature enabled but no Lineage.txt found for {}", input_path.display());
        } else {
            log::info!("Lineage feature enabled and file found: {:?}", settings.lineage_file_path);
        }
    } else {
        log::debug!("Lineage feature is DISABLED for {}", input_path.display());
    }

    // Call private helper with modified settings
    backend.convert_item_with_settings(item, input_path, output_path, progress_tx, settings).await
}