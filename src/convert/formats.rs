//! Audio format detection and conversion options

use crate::convert::simple_wizard::{DitherType, NyquistTransition, ReplayGainMode};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Supported file formats (archives and audio)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileFormat {
    /// Archive container handled by the external 7z/7zz extractor.
    #[serde(alias = "SevenZip")]
    Archive,
    /// CUE sheet control file. The CUE materializer resolves referenced audio
    /// image(s); the `.cue` file itself is not probed as audio and is not an
    /// archive surrogate.
    CueSheet,
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
            Self::Dsf => "DSF",
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
            Self::Dff,
            Self::Dts,
            Self::Ac3,
            Self::Lpcm,
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


/// Default loose companion-file extensions copied with conversions, as the
/// comma-separated display form of `default_companion_extensions()`.
///
/// Companion copying is on by default for the conventional album extras below;
/// users opt out by blanking the include field (the TUI treats a non-empty
/// include list as the on/off switch) or disabling `copy_auxiliary_files`.
pub const DEFAULT_COMPANION_EXTENSIONS: &str =
    ".png, .bmp, .jpeg, .jpg, .gif, .webp, .log, .cue, .txt, .nfo, .pdf, .m3u, .m3u8";

/// Parse a comma-separated extension list into normalized, de-duplicated values.
///
/// Empty tokens are ignored, leading dots are optional, and all extensions are
/// lowercased because extension matching is case-insensitive across supported
/// filesystems. Path-like tokens are ignored rather than interpreted, because
/// companion-file matching is intentionally limited to top-level loose files.
pub fn parse_companion_extensions(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in input.split(',') {
        let trimmed = token.trim();
        if trimmed.is_empty() || trimmed == "." || trimmed.contains('/') || trimmed.contains('\\') {
            continue;
        }

        let without_dot = trimmed.trim_start_matches('.');
        if without_dot.is_empty() {
            continue;
        }

        let normalized = format!(".{}", without_dot.to_ascii_lowercase());
        if !out.iter().any(|existing| existing == &normalized) {
            out.push(normalized);
        }
    }
    out
}

/// Parse a comma-separated list of bare folder names into a de-duplicated list.
///
/// Folder matching is always relative to the source directory. Tokens that look
/// like paths or traversal components are dropped defensively so UI/preset input
/// cannot escape the source root. Case folding is deliberately not applied here;
/// the copy stage applies platform-appropriate matching when it sees the actual
/// filesystem entries.
pub fn parse_companion_folders(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in input.split(',') {
        let trimmed = token.trim();
        if trimmed.is_empty()
            || trimmed == "."
            || trimmed == ".."
            || trimmed.contains('/')
            || trimmed.contains('\\')
        {
            continue;
        }
        if !out.iter().any(|existing| existing == trimmed) {
            out.push(trimmed.to_string());
        }
    }
    out
}

/// Default loose companion-file extensions: common artwork, rip documentation,
/// and playlist sidecars. Companion copying only runs when the include list is
/// non-empty (the TUI treats the field as the on/off switch), so these defaults
/// make a fresh install copy the conventional album extras out of the box.
pub fn default_companion_extensions() -> Vec<String> {
    [
        "png", "bmp", "jpeg", "jpg", "gif", "webp", "log", "cue", "txt", "nfo", "pdf", "m3u",
        "m3u8",
    ]
    .iter()
    .map(|ext| format!(".{ext}"))
    .collect()
}

/// Parse a comma-separated list of companion file names to exclude from
/// loose-file copying.
///
/// Include is extension-based, so exclusion works at file-name granularity: an
/// extension the user does not include is never copied in the first place.
/// Tokens are exact names or simple wildcards (`*` matches any run of
/// characters, `?` a single character), e.g. `EXIGO*` or `*_dr.txt`. Tokens
/// that look like paths are dropped defensively. Matching at the copy stage is
/// case-insensitive; tokens are normalized to lowercase here.
pub fn parse_companion_exclude_files(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in input.split(',') {
        let trimmed = token.trim();
        if trimmed.is_empty()
            || trimmed == "."
            || trimmed == ".."
            || trimmed.contains('/')
            || trimmed.contains('\\')
        {
            continue;
        }
        let normalized = trimmed.to_lowercase();
        if !out.iter().any(|existing| existing == &normalized) {
            out.push(normalized);
        }
    }
    out
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

    /// Legacy master toggle for loose companion files. New callers should
    /// prefer `companion_extensions`; this remains for backwards compatibility.
    #[serde(default = "default_true")]
    pub copy_auxiliary_files: bool,

    /// Legacy master toggle for companion folders. New callers should prefer
    /// `companion_folders`; this remains for backwards compatibility.
    #[serde(default = "default_true")]
    pub copy_subdirectories: bool,

    /// Normalized loose companion-file extensions to copy from the source
    /// directory after publish. Empty means copy no loose files.
    #[serde(default = "default_companion_extensions")]
    pub companion_extensions: Vec<String>,

    /// Bare companion-folder names to copy recursively from the source directory
    /// after publish. Empty means copy no folders.
    #[serde(default)]
    pub companion_folders: Vec<String>,

    /// Companion file names (case-insensitive; `*`/`?` wildcards allowed) that
    /// loose-file copying must skip even when their extension is included.
    /// Empty excludes nothing.
    #[serde(default)]
    pub companion_exclude_files: Vec<String>,

    /// Force same-format re-encoding instead of passthrough - defaults to false
    #[serde(default)]
    pub force_encode: bool,

    /// Create `disc NN` subfolders for detected multi-disc sets - defaults to false
    #[serde(default)]
    pub create_disc_subfolders: bool,

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
            copy_auxiliary_files: true,
            copy_subdirectories: true,
            companion_extensions: default_companion_extensions(),
            companion_folders: Vec::new(),
            companion_exclude_files: Vec::new(),
            force_encode: false,
            create_disc_subfolders: false,
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

impl ConversionOptions {
    /// Effective file naming template for pipeline/request construction.
    ///
    /// `create_disc_subfolders` is a first-class conversion option, so queue and
    /// processor code must not rely on the TUI having already mutated the raw
    /// filename template. This method is the canonical handoff point from
    /// `ConversionOptions` into `PipelineRequest::naming.template`.
    #[must_use]
    pub fn effective_naming_template(&self, default_template: &str) -> String {
        let template = self
            .naming_template
            .clone()
            .unwrap_or_else(|| default_template.to_string());
        naming_template_with_disc_subfolder(template, self.create_disc_subfolders)
    }

    /// Effective, normalized loose companion-file extensions for pipeline use.
    #[must_use]
    pub fn effective_companion_extensions(&self) -> Vec<String> {
        if !self.copy_auxiliary_files {
            return Vec::new();
        }
        parse_companion_extensions(&self.companion_extensions.join(","))
    }

    /// Effective, validated companion-folder names for pipeline use.
    #[must_use]
    pub fn effective_companion_folders(&self) -> Vec<String> {
        if !self.copy_subdirectories {
            return Vec::new();
        }
        parse_companion_folders(&self.companion_folders.join(","))
    }

    /// Effective, normalized companion file names excluded from loose-file
    /// copying. Only meaningful when loose-file copying is active.
    #[must_use]
    pub fn effective_companion_exclude_files(&self) -> Vec<String> {
        if !self.copy_auxiliary_files {
            return Vec::new();
        }
        parse_companion_exclude_files(&self.companion_exclude_files.join(","))
    }
}


/// Token expanded by the planner into `Disc NN` for detected multi-disc sets.
pub const DISC_FOLDER_TEMPLATE_TOKEN: &str = "%DISC_FOLDER%";

/// Return `template` with a leading disc-folder component when requested.
///
/// This helper is intentionally backend-owned rather than TUI-owned: the UI may
/// surface `create_disc_subfolders`, but the conversion/processor handoff is
/// responsible for preserving the option for every entrypoint. Existing explicit
/// `%DISC_FOLDER%` tokens are respected to keep the operation idempotent.
#[must_use]
pub fn naming_template_with_disc_subfolder(
    template: impl Into<String>,
    create_disc_subfolders: bool,
) -> String {
    let template = template.into();
    if !create_disc_subfolders || template.contains(DISC_FOLDER_TEMPLATE_TOKEN) {
        return template;
    }
    format!("{DISC_FOLDER_TEMPLATE_TOKEN}/{template}")
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
    /// Detect file format from file path (archive, structured disc source, or audio).
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
            return Ok(FileFormat::Archive);
        }

        // Structured disc directories do not necessarily have filename
        // extensions. Admit recognized disc source directories before
        // extension-based detection so the pipeline source-kind detector can
        // route them to the appropriate materializer. FileFormat::Archive is
        // the existing container admission class used for ISOs, disc
        // directories, and other non-audio inputs.
        if path.is_dir() {
            if crate::disc::bluray_utils::is_bluray_source(path)
                || crate::disc::dvdv_utils::is_dvdv_source(path)
                || crate::disc::dvda_utils::is_dvda_source(path)
            {
                return Ok(FileFormat::Archive);
            }
        }

        let Some(extension) = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_lowercase())
        else {
            // Extensionless Blu-ray image files are uncommon, but possible.
            // Keep this check after the cheap directory fast path so normal
            // extension-bearing file scans do not pay an ISO probe cost.
            if path.is_file() && crate::disc::bluray_utils::is_bluray_source(path) {
                return Ok(FileFormat::Archive);
            }
            return Err(super::ConversionError::UnsupportedFormat(format!(
                "No file extension found for: {}",
                path.display()
            )));
        };

        match extension.as_str() {
            // Archives — all handled by 7zz/7z extraction pipeline.
            "7z" | "zip" | "rar" | "tar" | "iso" | "cab" | "dmg" | "tgz" | "tbz2" | "txz" => {
                Ok(FileFormat::Archive)
            }
            // Control files that route to the CUE materializer. These are
            // deliberately distinct from Archive: treating `.cue` as an
            // archive lets it pass the detector but sends the wrong signal to
            // downstream code and hides probe/routing mistakes.
            "cue" => Ok(FileFormat::CueSheet),
            // Audio formats.
            "flac" => Ok(FileFormat::Audio(AudioFormat::Flac)),
            "wav" | "wave" => Ok(FileFormat::Audio(AudioFormat::Wav)),
            "aiff" | "aif" | "aifc" => Ok(FileFormat::Audio(AudioFormat::Aiff)),
            "wv" => Ok(FileFormat::Audio(AudioFormat::WavPack)),
            "mp3" => Ok(FileFormat::Audio(AudioFormat::Mp3)),
            "m4a" | "mp4" => Ok(FileFormat::Audio(Self::detect_m4a_codec(path))),
            "aac" => Ok(FileFormat::Audio(AudioFormat::Aac)),
            "opus" => Ok(FileFormat::Audio(AudioFormat::Opus)),
            "dsf" => Ok(FileFormat::Audio(AudioFormat::Dsf)),
            "dff" => Ok(FileFormat::Audio(AudioFormat::Dff)),
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
            FileFormat::Archive => Err(super::ConversionError::UnsupportedFormat(
                "Expected audio file, found archive".to_string(),
            )),
            FileFormat::CueSheet => Err(super::ConversionError::UnsupportedFormat(
                "Expected audio file, found CUE sheet".to_string(),
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
fn default_true() -> bool {
    true
}

fn default_cue_generation_mode() -> String {
    "IfMerging".to_string()
}


#[cfg(test)]
mod tests {
    use super::{
        default_companion_extensions, parse_companion_exclude_files, parse_companion_extensions,
        parse_companion_folders, AudioFormat, FileFormat, FormatDetector,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            let path = std::env::temp_dir().join(format!(
                "tonepoet-format-detector-{name}-{}-{nanos}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_minimal_bluray_layout(root: &Path) {
        let bdmv = root.join("BDMV");
        fs::create_dir_all(bdmv.join("PLAYLIST")).expect("create PLAYLIST");
        fs::create_dir_all(bdmv.join("STREAM")).expect("create STREAM");
        fs::write(bdmv.join("index.bdmv"), b"index").expect("write index.bdmv");
        fs::write(bdmv.join("MovieObject.bdmv"), b"movie").expect("write MovieObject.bdmv");
        fs::write(bdmv.join("PLAYLIST").join("00000.mpls"), b"playlist")
            .expect("write playlist");
        fs::write(bdmv.join("STREAM").join("00000.m2ts"), b"stream").expect("write stream");
    }

    #[test]
    fn detect_archive_extensions_as_generic_archive_format() {
        for name in [
            "album.7z",
            "album.zip",
            "album.rar",
            "album.tar",
            "album.iso",
            "album.cab",
            "album.dmg",
            "album.tgz",
            "album.tbz2",
            "album.txz",
            "album.tar.gz",
            "album.tar.bz2",
            "album.tar.xz",
            "album.tar.zst",
            "album.tar.lz",
            "album.tar.lzma",
        ] {
            assert_eq!(
                FormatDetector::detect(Path::new(name)).expect("archive format"),
                FileFormat::Archive,
                "{name} should be admitted as a generic archive"
            );
        }
    }

    #[test]
    fn detect_accepts_bluray_disc_root_without_extension() {
        let temp = TempDir::new("bluray-root");
        write_minimal_bluray_layout(&temp.path);

        assert_eq!(
            FormatDetector::detect(&temp.path).expect("Blu-ray root is queue-admissible"),
            FileFormat::Archive
        );
    }

    #[test]
    fn detect_accepts_bdmv_directory_without_extension() {
        let temp = TempDir::new("bdmv-dir");
        write_minimal_bluray_layout(&temp.path);

        assert_eq!(
            FormatDetector::detect(&temp.path.join("BDMV"))
                .expect("BDMV directory is queue-admissible"),
            FileFormat::Archive
        );
    }

    #[test]
    fn detect_still_rejects_non_bluray_directory_without_extension() {
        let temp = TempDir::new("ordinary-dir");
        let err = FormatDetector::detect(&temp.path)
            .expect_err("ordinary extensionless directory must not be admitted")
            .to_string();

        assert!(
            err.contains("No file extension found for"),
            "unexpected detector error: {err}"
        );
    }

    #[test]
    fn companion_extension_parser_normalizes_and_deduplicates() {
        assert_eq!(
            parse_companion_extensions("png, .JPG, jpg, ./bad, Scans/foo, .cue"),
            vec![".png", ".jpg", ".cue"]
        );
    }

    #[test]
    fn companion_extension_defaults_cover_album_extras_and_stay_consistent() {
        // The const is the display form of the canonical default list; parsing
        // it must round-trip to the same normalized extensions.
        assert_eq!(
            parse_companion_extensions(super::DEFAULT_COMPANION_EXTENSIONS),
            super::default_companion_extensions()
        );

        let options = super::ConversionOptions::default();
        assert_eq!(options.companion_extensions, super::default_companion_extensions());
        assert_eq!(
            options.effective_companion_extensions(),
            super::default_companion_extensions()
        );
    }

    #[test]
    fn companion_folder_parser_accepts_only_bare_names() {
        assert_eq!(
            parse_companion_folders("Scans, Artwork, ../escape, foo/bar, Scans, .., ."),
            vec!["Scans", "Artwork"]
        );
    }

    #[test]
    fn detect_accepts_standalone_dsf_and_dff_as_audio() {
        assert_eq!(
            FormatDetector::detect(Path::new("album.dsf")).expect("DSF is supported"),
            FileFormat::Audio(AudioFormat::Dsf)
        );
        assert_eq!(
            FormatDetector::detect(Path::new("album.dff")).expect("DFF is supported"),
            FileFormat::Audio(AudioFormat::Dff)
        );
    }

    #[test]
    fn detect_audio_accepts_standalone_dsf_and_dff() {
        assert_eq!(
            FormatDetector::detect_audio(Path::new("track.DSF")).expect("uppercase DSF is supported"),
            AudioFormat::Dsf
        );
        assert_eq!(
            FormatDetector::detect_audio(Path::new("track.DFF")).expect("uppercase DFF is supported"),
            AudioFormat::Dff
        );
    }

    #[test]
    fn default_companion_extensions_cover_conventional_album_extras() {
        let defaults = default_companion_extensions();
        for ext in [
            ".png", ".bmp", ".jpeg", ".jpg", ".gif", ".webp", ".log", ".cue", ".txt", ".nfo",
            ".pdf", ".m3u", ".m3u8",
        ] {
            assert!(defaults.iter().any(|entry| entry == ext), "missing {ext}");
        }
        assert_eq!(defaults.len(), 13);
    }

    #[test]
    fn parse_companion_exclude_files_normalizes_and_rejects_paths() {
        assert_eq!(
            parse_companion_exclude_files("Foo_DR.txt, cover.JPG , foo_dr.txt,, ../evil.txt, a/b.txt"),
            vec!["foo_dr.txt".to_string(), "cover.jpg".to_string()]
        );
        assert!(parse_companion_exclude_files("").is_empty());
    }

    #[test]
    fn effective_companion_exclude_files_gated_by_copy_toggle() {
        let mut options = super::ConversionOptions::default();
        options.companion_exclude_files = vec!["Foo_DR.txt".to_string()];
        assert_eq!(
            options.effective_companion_exclude_files(),
            vec!["foo_dr.txt".to_string()]
        );
        options.copy_auxiliary_files = false;
        assert!(options.effective_companion_exclude_files().is_empty());
    }
}
