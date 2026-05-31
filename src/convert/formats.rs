//! Audio format detection and conversion options

use crate::convert::simple_wizard::{DitherType, NyquistTransition, ReplayGainMode};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Supported file formats (archives and audio)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileFormat {
    /// 7-Zip archive (may contain audio files)
    SevenZip,
    /// Audio formats
    Audio(AudioFormat),
}

/// Supported audio formats
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AudioFormat {
    /// Free Lossless Audio Codec
    Flac,
    /// Waveform Audio File Format
    Wav,
    /// Audio Interchange File Format
    Aiff,
    /// WavPack lossless compression
    WavPack,
    /// MPEG-1/2 Audio Layer III
    Mp3,
    /// Advanced Audio Coding
    Aac,
    /// Opus Interactive Audio Codec
    Opus,
    /// Apple Lossless Audio Codec
    Alac,
    /// DSD Stream File
    Dsf,
    /// DSDIFF (Philips DSD Interchange File Format)
    Dff,
    /// DTS Coherent Acoustics
    Dts,
    /// Dolby Digital (AC-3)
    Ac3,
    /// Monkey's Audio (decode-only — not encodable by ffmpeg or SoX)
    Ape,
    /// Linear PCM (raw headerless, or via WAV/AIFF container)
    Lpcm,
}

impl AudioFormat {
    /// Get the file extension for this format
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Flac => "flac",
            Self::Wav => "wav",
            Self::Aiff => "aiff",
            Self::WavPack => "wv",
            Self::Mp3 => "mp3",
            Self::Aac => "m4a",
            Self::Opus => "opus",
            Self::Alac => "m4a",
            Self::Dsf => "dsf",
            Self::Dff => "dff",
            Self::Dts => "dts",
            Self::Ac3 => "ac3",
            Self::Ape => "ape",
            Self::Lpcm => "pcm",
        }
    }

    /// Get a human-readable name
    pub fn name(&self) -> &'static str {
        match self {
            Self::Flac => "FLAC",
            Self::Wav => "WAV",
            Self::Aiff => "AIFF",
            Self::WavPack => "WavPack",
            Self::Mp3 => "MP3",
            Self::Aac => "AAC",
            Self::Opus => "Opus",
            Self::Alac => "ALAC",
            Self::Dsf => "DSD",
            Self::Dff => "DFF",
            Self::Dts => "DTS",
            Self::Ac3 => "AC3",
            Self::Ape => "APE",
            Self::Lpcm => "LPCM",
        }
    }

    /// Check if this is a lossless format
    pub fn is_lossless(&self) -> bool {
        matches!(
            self,
            Self::Flac | Self::Wav | Self::Aiff | Self::WavPack | Self::Alac | Self::Dsf | Self::Dff | Self::Ape | Self::Lpcm
        )
    }

    /// Get all supported output formats
    pub fn all() -> Vec<Self> {
        vec![
            Self::Flac,
            Self::Wav,
            Self::Aiff,
            Self::WavPack,
            Self::Mp3,
            Self::Aac,
            Self::Opus,
            Self::Alac,
            Self::Dsf,
            Self::Dff,
            Self::Dts,
            Self::Ac3,
            Self::Ape,
            Self::Lpcm,
        ]
    }

    /// Additional output formats shown below-the-fold when the format pane is maximized.
    pub fn advanced_output() -> Vec<Self> {
        vec![Self::Dts, Self::Ac3, Self::Ape, Self::Lpcm]
    }

    /// Formats shown as pills in the main TUI convert screen
    pub fn common_output() -> Vec<Self> {
        vec![
            Self::Flac,
            Self::Opus,
            Self::Aac,
            Self::Mp3,
            Self::Alac,
            Self::Wav,
            Self::Aiff,
            Self::WavPack,
            Self::Dsf,
        ]
    }

    /// Available container options for this codec. Index 0 is the default.
    pub fn available_containers(&self) -> &'static [ContainerOption] {
        match self {
            Self::Flac => &[
                ContainerOption { extension: "flac", display_name: "FLAC", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "ogg", display_name: "OGG", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mka", display_name: "MKA", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mkv", display_name: "MKV", ffmpeg_flags: &[], enabled: true },
            ],
            Self::Wav => &[
                ContainerOption { extension: "wav", display_name: "WAV", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "wav", display_name: "RF64", ffmpeg_flags: &["-rf64", "auto"], enabled: true },
                ContainerOption { extension: "w64", display_name: "W64", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mka", display_name: "MKA", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mkv", display_name: "MKV", ffmpeg_flags: &[], enabled: true },
            ],
            Self::Aiff => &[
                ContainerOption { extension: "aiff", display_name: "AIFF", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mka", display_name: "MKA", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mkv", display_name: "MKV", ffmpeg_flags: &[], enabled: true },
            ],
            Self::WavPack => &[
                ContainerOption { extension: "wv", display_name: "WavPack", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mka", display_name: "MKA", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mkv", display_name: "MKV", ffmpeg_flags: &[], enabled: true },
            ],
            Self::Mp3 => &[
                ContainerOption { extension: "mp3", display_name: "MP3", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mka", display_name: "MKA", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mkv", display_name: "MKV", ffmpeg_flags: &[], enabled: true },
            ],
            Self::Aac => &[
                ContainerOption { extension: "m4a", display_name: "M4A", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "aac", display_name: "AAC (raw)", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mp4", display_name: "MP4", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "m4b", display_name: "M4B", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mka", display_name: "MKA", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mkv", display_name: "MKV", ffmpeg_flags: &[], enabled: true },
            ],
            Self::Opus => &[
                ContainerOption { extension: "opus", display_name: "Opus", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "webm", display_name: "WebM", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "weba", display_name: "WebA", ffmpeg_flags: &["-f", "webm"], enabled: true },
                ContainerOption { extension: "mka", display_name: "MKA", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mkv", display_name: "MKV", ffmpeg_flags: &[], enabled: true },
            ],
            Self::Alac => &[
                ContainerOption { extension: "m4a", display_name: "M4A", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mp4", display_name: "MP4", ffmpeg_flags: &[], enabled: true },
            ],
            Self::Dsf => &[
                ContainerOption { extension: "dsf", display_name: "DSF", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "dff", display_name: "DFF", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "wv", display_name: "WavPack", ffmpeg_flags: &[], enabled: false },
                ContainerOption { extension: "flac", display_name: "FLAC (DoP)", ffmpeg_flags: &[], enabled: false },
                ContainerOption { extension: "wav", display_name: "WAV (DoP)", ffmpeg_flags: &[], enabled: false },
            ],
            Self::Dff => &[
                ContainerOption { extension: "dff", display_name: "DFF", ffmpeg_flags: &[], enabled: true },
            ],
            Self::Dts => &[
                ContainerOption { extension: "dts", display_name: "DTS", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mka", display_name: "MKA", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mkv", display_name: "MKV", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mp4", display_name: "MP4", ffmpeg_flags: &[], enabled: true },
            ],
            Self::Ac3 => &[
                ContainerOption { extension: "ac3", display_name: "AC3", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mka", display_name: "MKA", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mkv", display_name: "MKV", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "mp4", display_name: "MP4", ffmpeg_flags: &[], enabled: true },
            ],
            Self::Ape => &[
                ContainerOption { extension: "ape", display_name: "APE", ffmpeg_flags: &[], enabled: false },
            ],
            Self::Lpcm => &[
                ContainerOption { extension: "wav", display_name: "WAV", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "aiff", display_name: "AIFF", ffmpeg_flags: &[], enabled: true },
                ContainerOption { extension: "pcm", display_name: "PCM (raw)", ffmpeg_flags: &[], enabled: false },
            ],
        }
    }

    /// Default container for this codec (index 0 of available_containers).
    pub fn default_container(&self) -> &'static ContainerOption {
        &self.available_containers()[0]
    }
}

/// A container option for an audio codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContainerOption {
    /// File extension without the leading dot.
    pub extension: &'static str,
    /// Human-readable name shown in the UI.
    pub display_name: &'static str,
    /// Extra ffmpeg output flags needed for this container.
    /// Empty for containers that ffmpeg auto-detects from extension.
    pub ffmpeg_flags: &'static [&'static str],
    /// Whether this container is currently functional. Disabled containers
    /// are shown grayed out in the UI and are not selectable.
    pub enabled: bool,
}

impl std::fmt::Display for AudioFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// Options for audio conversion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionOptions {
    /// Target output format
    pub output_format: AudioFormat,

    /// Quality settings
    pub quality: QualitySettings,

    /// Whether to preserve metadata
    pub preserve_metadata: bool,

    /// Whether to calculate ReplayGain
    pub calculate_replaygain: bool,

    /// ReplayGain mode (Track, Album, or Both)
    pub replaygain_mode: Option<ReplayGainMode>,

    /// Output filename template
    pub naming_template: Option<String>,

    /// Output album/folder template
    pub folder_template: Option<String>,

    /// Whether to overwrite existing files
    pub overwrite: bool,

    /// Output directory for converted files
    pub output_dir: Option<PathBuf>,

    /// Resampling quality (0-4: LQ, MQ, HQ, VHQ, Ultra)
    pub resample_quality: Option<u8>,

    /// Nyquist filter transition for resampling (Gentle, Steep, BrickWall)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nyquist_transition: Option<NyquistTransition>,

    /// Dither type for bit depth reduction
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dither_type: Option<DitherType>,

    /// Target sample rate for resampling (applies to all formats)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_sample_rate: Option<u32>,

    /// Target bit depth for conversion (applies to all formats)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_bit_depth: Option<u32>,

    /// Copy auxiliary files (txt, cue, log, etc.) - defaults to true
    pub copy_auxiliary_files: bool,

    /// Copy subdirectories from source - defaults to true
    pub copy_subdirectories: bool,

    /// Force FLAC re-encoding instead of copying - defaults to false
    pub reencode_flac: bool,

    /// Merge all tracks into single file - defaults to false
    pub merge_to_single: bool,

    /// Preferred conversion backend (FFmpeg or Sox)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_backend: Option<tonepoet_backend::Backend>,

    /// Original preset settings (for comprehensive logging)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_settings: Option<Box<tonepoet_backend::types::ConversionSettings>>,

    /// Exact Chunk 1 planner settings selected by the UI/CLI.
    ///
    /// This is the lossless handoff path for the unified orchestrator. The
    /// legacy fields above remain for display, migration, and compatibility,
    /// but production queue processing requires this field or a prebuilt
    /// `PipelineRequest` on the `ConversionItem`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_settings: Option<tonepoet_pipeline::PipelineSettings>,

    /// Enable SSRC Insane mode (requires BrickWall nyquist transition)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssrc_insane_mode: Option<bool>,

    /// Append content from Lineage.txt to COMMENT tag
    pub append_lineage_to_comment: bool,

    /// Whether to write a conversion log file
    #[serde(default)]
    pub write_log_file: bool,

    /// Whether to generate CUE files
    #[serde(default)]
    pub generate_cue_files: bool,

    /// CUE generation mode: "Always" or "IfMerging"
    #[serde(default = "default_cue_generation_mode")]
    pub cue_generation_mode: String,

    /// Container extension override. `None` = codec default.
    /// When set, the output file uses this extension instead of the
    /// codec's default (e.g., `Some("webm")` for Opus in WebM).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_extension: Option<String>,

    /// Extra ffmpeg output flags for the selected container.
    /// Empty for containers that ffmpeg auto-detects from extension.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub container_ffmpeg_flags: Vec<String>,
}

impl Default for ConversionOptions {
    fn default() -> Self {
        Self {
            output_format: AudioFormat::Flac,
            quality: QualitySettings::default(),
            preserve_metadata: true,
            calculate_replaygain: false,
            replaygain_mode: None,
            naming_template: None,
            folder_template: None,
            overwrite: false,
            output_dir: None,
            resample_quality: None,
            nyquist_transition: None,
            dither_type: None,
            target_sample_rate: None,
            target_bit_depth: None,
            copy_auxiliary_files: true, // Match wizard default
            copy_subdirectories: true,  // Match wizard default
            reencode_flac: false,       // Match wizard default (don't re-encode, copy is default)
            merge_to_single: false,
            preferred_backend: None,
            original_settings: None,
            pipeline_settings: None,
            ssrc_insane_mode: None,
            append_lineage_to_comment: false, // Default to off
            write_log_file: false,
            generate_cue_files: false,
            cue_generation_mode: "IfMerging".to_string(),
            container_extension: None,
            container_ffmpeg_flags: Vec::new(),
        }
    }
}

/// Quality settings for different formats
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum QualitySettings {
    /// FLAC compression level (0-8, where 8 is best compression)
    Flac { compression_level: u8 },

    /// WAV settings
    Wav { bit_depth: u16, sample_rate: u32 },

    /// AIFF settings
    Aiff { bit_depth: u16, sample_rate: u32 },

    /// WavPack settings
    WavPack {
        compression_mode: WavPackMode,
        hybrid_mode: bool,
        correction_file: bool,
    },

    /// MP3 settings
    Mp3 {
        bitrate_mode: Mp3BitrateMode,
        quality: u8, // 0-9, where 0 is best
    },

    /// AAC settings
    Aac {
        bitrate: u32, // kbps
        profile: AacProfile,
    },

    /// Opus settings
    Opus {
        bitrate: u32,   // 6-510 kbps
        complexity: u8, // 0-10
    },

    /// ALAC (lossless, no user-configurable quality)
    Alac,
}

impl Default for QualitySettings {
    fn default() -> Self {
        Self::Flac {
            compression_level: 5,
        }
    }
}

/// WavPack compression modes
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum WavPackMode {
    Fast,
    Normal,
    High,
    VeryHigh,
}

/// MP3 bitrate modes
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Mp3BitrateMode {
    /// Constant bitrate
    Cbr { bitrate: u32 },
    /// Variable bitrate
    Vbr { quality: u8 },
    /// Average bitrate
    Abr { bitrate: u32 },
}

/// AAC profiles
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum AacProfile {
    /// Low Complexity
    Lc,
    /// High Efficiency
    He,
    /// High Efficiency v2
    HeV2,
}

/// Format detector
pub struct FormatDetector;

impl FormatDetector {
    /// Detect file format from file path (archive or audio)
    pub fn detect(path: &Path) -> Result<FileFormat, super::ConversionError> {
        // Check for compound tar extensions first (.tar.gz, .tar.bz2, etc.)
        // because Path::extension() only returns the last component.
        let name_lower = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_lowercase())
            .unwrap_or_default();
        if name_lower.ends_with(".tar.gz")
            || name_lower.ends_with(".tar.bz2")
            || name_lower.ends_with(".tar.xz")
            || name_lower.ends_with(".tar.zst")
            || name_lower.ends_with(".tar.lz")
            || name_lower.ends_with(".tar.lzma")
        {
            return Ok(FileFormat::SevenZip);
        }

        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_lowercase())
            .ok_or_else(|| {
                super::ConversionError::UnsupportedFormat(format!(
                    "No file extension found for: {}",
                    path.display()
                ))
            })?;

        match extension.as_str() {
            // Archives — all handled by 7zz/7z extraction pipeline.
            "7z" | "zip" | "rar" | "tar" | "iso" | "cab" | "dmg" | "tgz" | "tbz2" | "txz" => {
                Ok(FileFormat::SevenZip)
            }
            // Audio formats.
            "flac" => Ok(FileFormat::Audio(AudioFormat::Flac)),
            "wav" | "wave" => Ok(FileFormat::Audio(AudioFormat::Wav)),
            "aiff" | "aif" | "aifc" => Ok(FileFormat::Audio(AudioFormat::Aiff)),
            "wv" => Ok(FileFormat::Audio(AudioFormat::WavPack)),
            "mp3" => Ok(FileFormat::Audio(AudioFormat::Mp3)),
            "m4a" | "mp4" => Ok(FileFormat::Audio(Self::detect_m4a_codec(path))),
            "aac" => Ok(FileFormat::Audio(AudioFormat::Aac)),
            "opus" => Ok(FileFormat::Audio(AudioFormat::Opus)),
            _ => Err(super::ConversionError::UnsupportedFormat(format!(
                "Unsupported format: .{}",
                extension
            ))),
        }
    }

    /// Distinguish ALAC from AAC in .m4a/.mp4 containers using ffmpeg-next
    fn detect_m4a_codec(path: &Path) -> AudioFormat {
        // Ensure ffmpeg is initialized
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            ffmpeg_next::init().ok();
        });

        // Try in-process ffmpeg probe to check the actual codec
        if let Ok(ctx) = ffmpeg_next::format::input(&path) {
            if let Some(stream) = ctx.streams().best(ffmpeg_next::media::Type::Audio) {
                if let Ok(codec_ctx) =
                    ffmpeg_next::codec::context::Context::from_parameters(stream.parameters())
                {
                    if let Ok(audio) = codec_ctx.decoder().audio() {
                        if let Some(codec) = audio.codec() {
                            if codec.name() == "alac" {
                                return AudioFormat::Alac;
                            }
                        }
                    }
                }
            }
        }
        // Default to AAC if probe fails or codec isn't ALAC
        AudioFormat::Aac
    }

    /// Detect audio format specifically
    pub fn detect_audio(path: &Path) -> Result<AudioFormat, super::ConversionError> {
        match Self::detect(path)? {
            FileFormat::Audio(format) => Ok(format),
            FileFormat::SevenZip => Err(super::ConversionError::UnsupportedFormat(
                "Expected audio file, found archive".to_string(),
            )),
        }
    }

    /// Check if a file is a supported format
    pub fn is_supported(path: &Path) -> bool {
        Self::detect(path).is_ok()
    }
}

/// Get default quality settings for a format
impl AudioFormat {
    pub fn default_quality(&self) -> QualitySettings {
        match self {
            AudioFormat::Flac => QualitySettings::Flac {
                compression_level: 5,
            },
            AudioFormat::Wav => QualitySettings::Wav {
                bit_depth: 16,
                sample_rate: 44100,
            },
            AudioFormat::Aiff => QualitySettings::Aiff {
                bit_depth: 16,
                sample_rate: 44100,
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
            AudioFormat::Dsf | AudioFormat::Dff => QualitySettings::Flac {
                compression_level: 0, // DSD passthrough — no compression parameter
            },
            AudioFormat::Dts => QualitySettings::Flac { compression_level: 0 },
            AudioFormat::Ac3 => QualitySettings::Flac { compression_level: 0 },
            AudioFormat::Ape => QualitySettings::Flac { compression_level: 0 },
            AudioFormat::Lpcm => QualitySettings::Wav { bit_depth: 16, sample_rate: 44100 },
        }
    }
}

/// Default CUE generation mode for backward compatibility
fn default_cue_generation_mode() -> String {
    "IfMerging".to_string()
}
