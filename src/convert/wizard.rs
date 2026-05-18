//! Conversion wizard for guided configuration

use super::formats::{AudioFormat, ConversionOptions, Mp3BitrateMode, QualitySettings};
use serde::{Deserialize, Serialize};

/// Steps in the conversion wizard
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WizardStep {
    /// Select output format
    FormatSelection,
    /// Configure quality settings
    QualitySettings,
    /// Processing options (metadata, ReplayGain)
    ProcessingOptions,
    /// Output options (naming, folder structure)
    OutputOptions,
    /// Review and confirm
    Review,
}

impl WizardStep {
    /// Get the next step
    pub fn next(&self) -> Option<Self> {
        match self {
            Self::FormatSelection => Some(Self::QualitySettings),
            Self::QualitySettings => Some(Self::ProcessingOptions),
            Self::ProcessingOptions => Some(Self::OutputOptions),
            Self::OutputOptions => Some(Self::Review),
            Self::Review => None,
        }
    }

    /// Get the previous step
    pub fn previous(&self) -> Option<Self> {
        match self {
            Self::FormatSelection => None,
            Self::QualitySettings => Some(Self::FormatSelection),
            Self::ProcessingOptions => Some(Self::QualitySettings),
            Self::OutputOptions => Some(Self::ProcessingOptions),
            Self::Review => Some(Self::OutputOptions),
        }
    }

    /// Get step title
    pub fn title(&self) -> &'static str {
        match self {
            Self::FormatSelection => "Select Output Format",
            Self::QualitySettings => "Quality Settings",
            Self::ProcessingOptions => "Processing Options",
            Self::OutputOptions => "Output Options",
            Self::Review => "Review & Start",
        }
    }

    /// Get step description
    pub fn description(&self) -> &'static str {
        match self {
            Self::FormatSelection => "Choose the audio format for your converted files",
            Self::QualitySettings => "Configure quality settings for the selected format",
            Self::ProcessingOptions => "Choose how to process metadata and audio",
            Self::OutputOptions => "Configure output file naming and organization",
            Self::Review => "Review your settings and start conversion",
        }
    }
}

/// State of the conversion wizard
#[derive(Debug, Clone)]
pub struct ConversionWizard {
    /// Current step
    pub current_step: WizardStep,

    /// Options being configured
    pub options: ConversionOptions,

    /// Selected format
    pub selected_format: AudioFormat,

    /// Whether converting from downloads
    pub from_downloads: bool,

    /// Custom output directory
    pub output_directory: Option<String>,

    /// Whether to delete source files after conversion
    pub delete_source: bool,
}

impl ConversionWizard {
    /// Create a new wizard
    pub fn new(from_downloads: bool) -> Self {
        Self {
            current_step: WizardStep::FormatSelection,
            options: ConversionOptions::default(),
            selected_format: AudioFormat::Flac,
            from_downloads,
            output_directory: None,
            delete_source: false,
        }
    }

    /// Move to the next step
    pub fn next_step(&mut self) -> bool {
        if let Some(next) = self.current_step.next() {
            self.current_step = next;
            true
        } else {
            false
        }
    }

    /// Move to the previous step
    pub fn previous_step(&mut self) -> bool {
        if let Some(prev) = self.current_step.previous() {
            self.current_step = prev;
            true
        } else {
            false
        }
    }

    /// Set the selected format
    pub fn set_format(&mut self, format: AudioFormat) {
        self.selected_format = format;
        self.options.output_format = format;
        self.options.quality = format.default_quality();
    }

    /// Get format options for selection
    pub fn format_options(&self) -> Vec<FormatOption> {
        AudioFormat::all()
            .into_iter()
            .map(|format| FormatOption {
                format,
                name: format.name().to_string(),
                description: format_description(format),
                is_lossless: format.is_lossless(),
                selected: format == self.selected_format,
            })
            .collect()
    }

    /// Get quality presets for the selected format
    pub fn quality_presets(&self) -> Vec<QualityPreset> {
        match self.selected_format {
            AudioFormat::Flac => vec![
                QualityPreset {
                    name: "Fast".to_string(),
                    description: "Fastest encoding, larger files".to_string(),
                    settings: QualitySettings::Flac {
                        compression_level: 0,
                    },
                },
                QualityPreset {
                    name: "Balanced".to_string(),
                    description: "Good compression/speed balance".to_string(),
                    settings: QualitySettings::Flac {
                        compression_level: 5,
                    },
                },
                QualityPreset {
                    name: "Best".to_string(),
                    description: "Best compression, slower encoding".to_string(),
                    settings: QualitySettings::Flac {
                        compression_level: 8,
                    },
                },
            ],
            AudioFormat::Mp3 => vec![
                QualityPreset {
                    name: "High Quality".to_string(),
                    description: "320 kbps CBR".to_string(),
                    settings: QualitySettings::Mp3 {
                        bitrate_mode: Mp3BitrateMode::Cbr { bitrate: 320 },
                        quality: 0,
                    },
                },
                QualityPreset {
                    name: "Standard".to_string(),
                    description: "V2 VBR (~190 kbps)".to_string(),
                    settings: QualitySettings::Mp3 {
                        bitrate_mode: Mp3BitrateMode::Vbr { quality: 2 },
                        quality: 2,
                    },
                },
                QualityPreset {
                    name: "Portable".to_string(),
                    description: "V4 VBR (~165 kbps)".to_string(),
                    settings: QualitySettings::Mp3 {
                        bitrate_mode: Mp3BitrateMode::Vbr { quality: 4 },
                        quality: 4,
                    },
                },
            ],
            AudioFormat::Opus => vec![
                QualityPreset {
                    name: "High Quality".to_string(),
                    description: "256 kbps".to_string(),
                    settings: QualitySettings::Opus {
                        bitrate: 256,
                        complexity: 10,
                    },
                },
                QualityPreset {
                    name: "Standard".to_string(),
                    description: "128 kbps".to_string(),
                    settings: QualitySettings::Opus {
                        bitrate: 128,
                        complexity: 10,
                    },
                },
                QualityPreset {
                    name: "Low Bandwidth".to_string(),
                    description: "64 kbps".to_string(),
                    settings: QualitySettings::Opus {
                        bitrate: 64,
                        complexity: 10,
                    },
                },
            ],
            _ => vec![QualityPreset {
                name: "Default".to_string(),
                description: "Default settings for this format".to_string(),
                settings: self.selected_format.default_quality(),
            }],
        }
    }

    /// Get a summary of current settings
    pub fn get_summary(&self) -> WizardSummary {
        WizardSummary {
            output_format: self.selected_format.name().to_string(),
            quality_description: quality_description(&self.options.quality),
            preserve_metadata: self.options.preserve_metadata,
            calculate_replaygain: self.options.calculate_replaygain,
            output_directory: self.output_directory.clone(),
            delete_source: self.delete_source,
            from_downloads: self.from_downloads,
        }
    }
}

/// Format option for display
#[derive(Debug, Clone)]
pub struct FormatOption {
    pub format: AudioFormat,
    pub name: String,
    pub description: String,
    pub is_lossless: bool,
    pub selected: bool,
}

/// Quality preset for a format
#[derive(Debug, Clone)]
pub struct QualityPreset {
    pub name: String,
    pub description: String,
    pub settings: QualitySettings,
}

/// Summary of wizard settings
#[derive(Debug, Clone)]
pub struct WizardSummary {
    pub output_format: String,
    pub quality_description: String,
    pub preserve_metadata: bool,
    pub calculate_replaygain: bool,
    pub output_directory: Option<String>,
    pub delete_source: bool,
    pub from_downloads: bool,
}

/// Get description for a format
fn format_description(format: AudioFormat) -> String {
    match format {
        AudioFormat::Flac => "Free Lossless Audio Codec - Best quality, moderate file size",
        AudioFormat::Wav => "Uncompressed audio - Largest file size, perfect quality",
        AudioFormat::Aiff => "Apple's uncompressed format - Large files, perfect quality",
        AudioFormat::WavPack => "Hybrid lossless compression - Good compression, less common",
        AudioFormat::Mp3 => "Most compatible lossy format - Small files, good quality",
        AudioFormat::Aac => "Modern lossy format - Better than MP3 at same bitrate",
        AudioFormat::Opus => "Best lossy codec - Excellent quality at low bitrates",
        AudioFormat::Alac => "Apple Lossless Audio Codec - Lossless compression, Apple ecosystem",
    }
    .to_string()
}

/// Get description for quality settings
fn quality_description(settings: &QualitySettings) -> String {
    match settings {
        QualitySettings::Flac { compression_level } => {
            format!("FLAC compression level {}", compression_level)
        }
        QualitySettings::Wav {
            bit_depth,
            sample_rate,
        } => {
            format!("{}-bit / {} Hz", bit_depth, sample_rate)
        }
        QualitySettings::Aiff {
            bit_depth,
            sample_rate,
        } => {
            format!("{}-bit / {} Hz", bit_depth, sample_rate)
        }
        QualitySettings::WavPack {
            compression_mode,
            hybrid_mode,
            ..
        } => {
            format!(
                "{:?} mode{}",
                compression_mode,
                if *hybrid_mode { " (hybrid)" } else { "" }
            )
        }
        QualitySettings::Mp3 { bitrate_mode, .. } => match bitrate_mode {
            Mp3BitrateMode::Cbr { bitrate } => format!("{} kbps CBR", bitrate),
            Mp3BitrateMode::Vbr { quality } => format!("V{} VBR", quality),
            Mp3BitrateMode::Abr { bitrate } => format!("{} kbps ABR", bitrate),
        },
        QualitySettings::Aac { bitrate, profile } => {
            format!("{} kbps {:?}", bitrate, profile)
        }
        QualitySettings::Opus { bitrate, .. } => {
            format!("{} kbps", bitrate)
        }
        QualitySettings::Alac => "Lossless (no configurable quality)".to_string(),
    }
}
