//! Audio Conversion Backend
//!
//! A robust module for constructing audio conversion commands from wizard settings.

pub mod ffmpeg;
pub mod integration;
pub mod integration_api;
pub mod mapping;
pub mod metadata;
pub mod pipeline;
pub mod preset;
pub mod sox;
pub mod types;
pub mod validation;

pub use ffmpeg::FFmpegBuilder;
pub use integration::{
    calculate_phase_progress, map_conversion_item_to_settings, ConversionPhase, ConversionStatus,
    ProgressUpdate,
};
pub use integration_api::{
    convert_with_backend, ConversionBackend, FormatCapabilities, ToolAvailability,
};
pub use metadata::{
    AacMetadataApplier, AacMetadataExtractor, FlacMetadata, FlacMetadataApplier,
    FlacMetadataExtractor, MetadataPreservingPipeline, OpusMetadataApplier, OpusMetadataExtractor,
    WavPackMetadataApplier, WavPackMetadataExtractor,
};
pub use pipeline::{ConversionPipeline, MetadataStrategy, PipelineBuilder};
pub use sox::SoxBuilder;
pub use types::*;

use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConversionError {
    #[error("Invalid settings: {0}")]
    InvalidSettings(String),

    #[error("Unsupported format combination: {0}")]
    UnsupportedFormat(String),

    #[error("Backend not available: {0}")]
    BackendUnavailable(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, ConversionError>;

/// Available conversion backends
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Backend {
    FFmpeg,
    Sox,
}

/// Main interface for building conversion commands
pub struct CommandBuilder {
    backend: Backend,
}

impl CommandBuilder {
    pub fn new(backend: Backend) -> Self {
        Self { backend }
    }

    /// Build a conversion command from settings
    pub fn build(
        &self,
        input: &Path,
        output: &Path,
        settings: &ConversionSettings,
    ) -> Result<ConversionCommand> {
        // Validate settings first
        validation::validate_settings(settings)?;

        match self.backend {
            Backend::FFmpeg => {
                let builder = FFmpegBuilder::new();
                builder.build(input, output, settings)
            }
            Backend::Sox => {
                // Check if SoX can write this format (based on testing)
                match settings.format {
                    AudioFormat::Opus | AudioFormat::Aac => {
                        return Err(ConversionError::UnsupportedFormat(format!(
                            "SoX cannot write {} format (read-only)",
                            settings.format.extension()
                        )));
                    }
                    _ => {}
                }
                let builder = SoxBuilder::new();
                builder.build(input, output, settings)
            }
        }
    }

    /// Check if a backend is available on the system
    pub fn is_available(&self) -> bool {
        match self.backend {
            Backend::FFmpeg => {
                // Check if ffmpeg is in PATH
                std::process::Command::new("ffmpeg")
                    .arg("-version")
                    .output()
                    .is_ok()
            }
            Backend::Sox => {
                // Check if sox is in PATH
                std::process::Command::new("sox")
                    .arg("--version")
                    .output()
                    .is_ok()
            }
        }
    }
}
