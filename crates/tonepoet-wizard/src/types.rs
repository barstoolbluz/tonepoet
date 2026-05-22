use crate::ui::ButtonId;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum AudioFormat {
    Flac,
    Wav,
    Aiff,
    WavPack,
    Mp3,
    Aac,
    Opus,
}

impl fmt::Display for AudioFormat {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            AudioFormat::Flac => write!(f, "FLAC"),
            AudioFormat::Wav => write!(f, "WAV"),
            AudioFormat::Aiff => write!(f, "AIFF"),
            AudioFormat::WavPack => write!(f, "WavPack"),
            AudioFormat::Mp3 => write!(f, "MP3"),
            AudioFormat::Aac => write!(f, "AAC"),
            AudioFormat::Opus => write!(f, "Opus"),
        }
    }
}

/// Dithering algorithms for quantization noise shaping when reducing bit depth
/// These correspond to SoX dither effect options for minimizing quantization artifacts:
/// - None = No dithering applied
/// - Tpdf = `dither` (default TPDF - Triangular Probability Density Function)
/// - Shibata = `dither -s -f shibata` (noise shaping with Shibata filter)
/// - LowShibata = `dither -s -f low-shibata` (noise shaping, optimized for lower frequencies)
/// - HighShibata = `dither -s -f high-shibata` (noise shaping, optimized for higher frequencies)
/// - Gesemann = `dither -s -f gesemann` (noise shaping with Gesemann filter)
/// - SlopedTpdf = `dither -S` (sloped TPDF without noise shaping)
///
/// Note: When using Brick Wall (SSRC) resampling, SSRC applies its own dithering
/// internally and these settings are ignored. SSRC's dithering behavior depends
/// on the target bit depth and sample rate combination.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum DitherType {
    None,        // No dithering
    Tpdf,        // dither (default TPDF)
    Shibata,     // dither -s -f shibata
    LowShibata,  // dither -s -f low-shibata
    HighShibata, // dither -s -f high-shibata
    Gesemann,    // dither -s -f gesemann
    SlopedTpdf,  // dither -S (sloped TPDF, no noise shaping)
}

/// Nyquist transition (anti-aliasing filter) settings for resampling
/// These control how aggressively the anti-aliasing filter removes frequencies
/// near the Nyquist limit during resampling operations:
/// - Gentle (95%) = Preserves 95% of Nyquist frequency, gentler rolloff
/// - Steep (99.7%) = Preserves 99.7% of Nyquist frequency, sharper rolloff  
/// - BrickWall (SSRC) = Uses SSRC resampler instead of SoX, overrides quality setting
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum NyquistTransition {
    Gentle,    // 95% - SoX rate with gentle rolloff
    Steep,     // 99.7% - SoX rate with steep rolloff
    BrickWall, // SSRC - Uses Secret Rabbit Code resampler
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum OpusContentType {
    Music,
    Voice,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum AacProfile {
    LcAac,   // Low Complexity AAC
    HeAac,   // High Efficiency AAC (AAC+)
    HeAacV2, // High Efficiency AAC v2 (AAC+ with PS)
}

impl fmt::Display for DitherType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DitherType::None => write!(f, "None"),
            DitherType::Tpdf => write!(f, "TPDF"),
            DitherType::Shibata => write!(f, "Shibata"),
            DitherType::LowShibata => write!(f, "Low Shibata"),
            DitherType::HighShibata => write!(f, "High Shibata"),
            DitherType::Gesemann => write!(f, "Gesemann"),
            DitherType::SlopedTpdf => write!(f, "Sloped TPDF"),
        }
    }
}

impl fmt::Display for NyquistTransition {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            NyquistTransition::Gentle => write!(f, "Gentle (95%)"),
            NyquistTransition::Steep => write!(f, "Steep (99.7%)"),
            NyquistTransition::BrickWall => write!(f, "Brick Wall (SSRC)"),
        }
    }
}

impl fmt::Display for OpusContentType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            OpusContentType::Music => write!(f, "Music"),
            OpusContentType::Voice => write!(f, "Voice"),
        }
    }
}

impl fmt::Display for AacProfile {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            AacProfile::LcAac => write!(f, "LC-AAC (standard)"),
            AacProfile::HeAac => write!(f, "HE-AAC (AAC+)"),
            AacProfile::HeAacV2 => write!(f, "HE-AACv2 (AAC+ with PS)"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ReplayGainMode {
    Album,
    Track,
    Both,
    Off,
}

impl fmt::Display for ReplayGainMode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ReplayGainMode::Album => write!(f, "Album mode (consistent volume across album)"),
            ReplayGainMode::Track => write!(f, "Track mode (consistent volume per track)"),
            ReplayGainMode::Both => write!(f, "Both (scan and tag for both album and track)"),
            ReplayGainMode::Off => write!(f, "Off (no ReplayGain scanning)"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum FlacSection {
    BitDepth,
    Dithering,
    SampleRate,
    CompressionLevel,
    ResamplingQuality,
    NyquistTransition,
    ProcessingOptions,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EditingField {
    CopyFiles,
    CopySubdirectories,
    CustomDestination,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DestinationMode {
    AskEveryTime,
    Custom(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FormatSpecificHelp {
    WavPackCompression,
    Mp3Bitrate,
    AacProfile,
    AacBitrate,
    OpusQuality,
    OpusContentType,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AdditionalOptionsHelp {
    ReplayGain,
    CopyFiles,
    CopySubdirectories,
    MergeToSingle,
    SourceFiles,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PopupType {
    PresetName,
    #[allow(dead_code)]
    TextInput {
        field: EditingField,
    },
    OverwriteConfirm {
        preset_name: String,
    },
    PresetList {
        presets: Vec<String>,
        selected_index: usize,
    },
    FileBrowser(Box<FileBrowser>),
    NewFolder {
        parent_path: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PopupFocus {
    Input,
    OkButton,
    CancelButton,
}

#[derive(Debug, Clone)]
pub struct PopupState {
    pub popup_type: PopupType,
    pub input_text: String,
    pub cursor_pos: usize,
    pub view_offset: usize,
    pub error_message: Option<String>,
    pub focused_element: PopupFocus,
}

#[derive(Debug, Clone)]
pub struct SimpleWizard {
    pub current_step: usize,
    pub selected_format: Option<AudioFormat>,
    pub selected_quality: Option<String>,
    pub selected_index: usize,
    pub quality_index: usize,  // Separate index for quality options
    pub in_quality_area: bool, // Track if we've tabbed to quality options
    pub resampling_page_section: FlacSection, // Track which section is focused on page 1
    pub additional_options_index: usize,
    pub scroll_offset: usize,
    #[allow(dead_code)]
    pub content_height: usize,

    // FLAC Advanced Options
    pub bit_depth: Option<u32>,
    pub sample_rate: Option<u32>,
    pub compression_level: Option<u8>,
    pub resample_quality: Option<u8>,
    pub dither_type: Option<DitherType>,
    pub nyquist_transition: Option<NyquistTransition>,
    pub ssrc_insane_mode: Option<bool>,
    pub verify_encoding: Option<bool>,
    pub calculate_replaygain: Option<bool>,
    pub store_md5: Option<bool>,
    pub reencode_flac: Option<bool>,

    // Opus-specific options
    pub opus_content_type: Option<OpusContentType>,

    // AAC-specific options
    pub aac_profile: Option<AacProfile>,

    // Additional options (step 2)
    pub replaygain_mode: Option<ReplayGainMode>,
    pub copy_files_enabled: bool,
    pub copy_files_extensions: String,
    pub copy_subdirectories_enabled: bool,
    pub copy_subdirectories: String,
    pub merge_to_single: Option<bool>,
    pub destination_mode: DestinationMode,

    // UI state
    pub show_help_for: Option<FlacSection>,
    pub show_additional_help_for: Option<AdditionalOptionsHelp>,
    pub show_format_help_for: Option<FormatSpecificHelp>,
    pub help_page: usize, // For multi-page help
    pub editing_field: Option<EditingField>,
    pub last_click_field: Option<usize>,
    pub last_click_time: std::time::Instant,
    pub should_start_conversion: bool, // Flag to signal conversion should start
    pub needs_destination_selection: bool, // Flag to signal we need to select destination before conversion
    pub popup_state: Option<PopupState>,   // For showing popup dialogs
    pub should_exit: bool,                 // Flag to signal wizard should exit
    pub hovered_button: Option<ButtonId>,  // Track which button is being hovered
    pub browse_button_focused: bool, // Track if Browse button is focused when on Custom destination
    pub focused_nav_button: Option<ButtonId>, // Track which navigation button is focused (Back/Next/Cancel)
}

impl Default for SimpleWizard {
    fn default() -> Self {
        Self {
            current_step: 0,
            selected_format: None,
            selected_quality: None,
            selected_index: 0,
            quality_index: 0,
            in_quality_area: false,
            resampling_page_section: FlacSection::BitDepth,
            additional_options_index: 0,
            scroll_offset: 0,
            content_height: 0,
            bit_depth: Some(0),                                  // Same as source
            sample_rate: Some(0),                                // Same as source
            compression_level: Some(8), // Best (perf difference is negligible)
            resample_quality: Some(0),  // Ultra
            dither_type: Some(DitherType::Shibata), // Default for 16-bit
            nyquist_transition: Some(NyquistTransition::Gentle), // Default to Gentle (95%)
            verify_encoding: Some(true),
            calculate_replaygain: Some(false),
            store_md5: Some(true),
            reencode_flac: Some(false), // Default: don't re-encode (copy is default)
            opus_content_type: Some(OpusContentType::Music),
            aac_profile: Some(AacProfile::LcAac),
            replaygain_mode: Some(ReplayGainMode::Both),
            copy_files_enabled: true,
            copy_files_extensions:
                "txt, cue, log, nfo, pdf, png, jpg, jpeg, gif, tif, tiff, bmp, webp".to_string(),
            copy_subdirectories_enabled: true,
            copy_subdirectories: "*".to_string(),
            merge_to_single: Some(false),
            ssrc_insane_mode: Some(false),
            destination_mode: DestinationMode::AskEveryTime,
            show_help_for: None,
            show_additional_help_for: None,
            show_format_help_for: None,
            help_page: 0,
            editing_field: None,
            last_click_field: None,
            last_click_time: std::time::Instant::now(),
            should_start_conversion: false,
            needs_destination_selection: false,
            popup_state: None,
            should_exit: false,
            hovered_button: None,
            browse_button_focused: false,
            focused_nav_button: None,
        }
    }
}

impl SimpleWizard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get WavPack compression flags based on the selected quality string
    /// Returns the command-line flags needed for the wavpack encoder
    #[allow(dead_code)]
    pub fn get_wavpack_compression_flags(&self) -> &'static str {
        match self.selected_quality.as_deref() {
            Some("Fast (Low CPU, larger files)") => "-f",
            Some("High (Balanced)") => "-h",
            Some("Very High (Smaller files)") => "-hh",
            Some("Maximum (Best compression)") => "-hhh",
            Some("Ultra (Very slow)") => "-hh -x",
            Some("Extreme (Slowest, smallest)") => "-hh -x4",
            _ => "-h", // Default to High
        }
    }

    /// Get SoX resampling flags based on quality and Nyquist settings
    /// Returns the SoX rate command flags for resampling operations
    #[allow(dead_code)]
    pub fn get_sox_resampling_flags(&self) -> String {
        match self.nyquist_transition {
            Some(NyquistTransition::BrickWall) => {
                // SSRC resampler overrides quality setting
                "rate -s".to_string() // Use Secret Rabbit Code
            }
            _ => {
                // Use SoX rate with quality flag
                let quality_flag = match self.resample_quality {
                    Some(0) => "-v", // Ultra (highest)
                    Some(1) => "-h", // VHQ
                    Some(2) => "-m", // HQ (default)
                    Some(3) => "-l", // MQ
                    _ => "-m",       // Default to HQ
                };

                let transition_flag = match self.nyquist_transition {
                    Some(NyquistTransition::Gentle) => "-p 95", // 95% passband
                    Some(NyquistTransition::Steep) => "-p 99.7", // 99.7% passband
                    _ => "-p 95",                               // Default to gentle
                };

                format!("rate {} {}", quality_flag, transition_flag)
            }
        }
    }

    /// Get SoX dither flags based on the selected dither type
    /// Returns the SoX dither command flags for quantization noise shaping
    ///
    /// Note: When using Brick Wall (SSRC) Nyquist Transition, SSRC handles its own
    /// dithering internally based on bit depth and sample rate, so these flags are
    /// not used. SSRC applies appropriate dithering automatically:
    /// - 24-bit: Flat TPDF = `dither` or `dither -S` (minimal shaping at -144 dBFS)
    /// - 16-bit low SR (8-16 kHz): Flat TPDF = `dither` (no shaping)
    /// - 16-bit mid SR (22.05-32 kHz): Gentle shaping = `dither -s -f gesemann`
    /// - 16-bit standard SR (44.1/48 kHz): Shibata = `dither -s -f shibata`
    /// - 16-bit high SR (88.2-192 kHz): High-Shibata = `dither -s -f high-shibata`
    #[allow(dead_code)]
    pub fn get_sox_dither_flags(&self) -> String {
        // If using SSRC resampler, dither settings are ignored
        if self.nyquist_transition == Some(NyquistTransition::BrickWall) {
            return String::new(); // SSRC handles dithering internally
        }

        match self.dither_type {
            Some(DitherType::None) => String::new(), // No dithering
            Some(DitherType::Tpdf) => "dither".to_string(), // Default TPDF
            Some(DitherType::Shibata) => "dither -s -f shibata".to_string(),
            Some(DitherType::LowShibata) => "dither -s -f low-shibata".to_string(),
            Some(DitherType::HighShibata) => "dither -s -f high-shibata".to_string(),
            Some(DitherType::Gesemann) => "dither -s -f gesemann".to_string(),
            Some(DitherType::SlopedTpdf) => "dither -S".to_string(),
            None => "dither".to_string(), // Default to TPDF if not set
        }
    }

    pub fn get_aac_bitrates(&self) -> Vec<&'static str> {
        match self.aac_profile {
            Some(AacProfile::LcAac) => vec![
                "320 kbps", "256 kbps", "192 kbps", "160 kbps", "128 kbps", "96 kbps", "64 kbps",
            ],
            Some(AacProfile::HeAac) => vec!["128 kbps", "96 kbps", "64 kbps", "48 kbps", "32 kbps"],
            Some(AacProfile::HeAacV2) => {
                vec!["96 kbps", "64 kbps", "48 kbps", "32 kbps", "24 kbps"]
            }
            None => vec![
                "320 kbps", "256 kbps", "192 kbps", "160 kbps", "128 kbps", "96 kbps", "64 kbps",
            ], // Default to LC-AAC
        }
    }

    pub fn next_step(&mut self) {
        if self.current_step < 4 {
            self.current_step += 1;
            self.selected_index = 0;
            self.additional_options_index = 0;
            self.scroll_offset = 0;
            self.browse_button_focused = false;
            self.focused_nav_button = None;

            // Auto-select high-quality defaults when entering quality step
            if self.current_step == 1 && self.selected_quality.is_none() {
                let default_quality = match self.selected_format {
                    Some(AudioFormat::Flac) => Some("High"),
                    Some(AudioFormat::Mp3) => Some("320 kbps"), // Highest quality, most compatible
                    Some(AudioFormat::Aac) => Some("256 kbps"), // LC @ 256 kbps, excellent quality
                    Some(AudioFormat::Opus) => Some("Very High"), // ~256-320 kbps, archival quality
                    Some(AudioFormat::Wav)
                    | Some(AudioFormat::Aiff)
                    | Some(AudioFormat::WavPack) => None, // Lossless formats don't need quality
                    None => None,
                };
                if let Some(quality) = default_quality {
                    self.selected_quality = Some(quality.to_string());
                }
            }
        }
    }

    pub fn previous_step(&mut self) {
        if self.current_step > 0 {
            self.current_step -= 1;
            self.selected_index = 0;
            self.additional_options_index = 0;
            self.scroll_offset = 0;
            self.browse_button_focused = false;
            self.focused_nav_button = None;
        }
    }

    pub fn is_in_quality_options(&self) -> bool {
        self.in_quality_area
    }

    pub fn get_bit_depth_options() -> Vec<(u32, &'static str)> {
        vec![
            (0, "Same as source"),
            (33, "32-bit float"), // Using 33 to differentiate from 32-bit integer
            (32, "32-bit"),
            (24, "24-bit"),
            (16, "16-bit"),
        ]
    }

    /// Get available dither options based on target bit depth
    ///
    /// For 16-bit: Full range of noise shaping filters available
    /// For 24-bit: Only flat/sloped TPDF (noise floor already at -144 dBFS)
    ///
    /// Note: When Brick Wall (SSRC) is selected, these options are shown but
    /// SSRC will override with its own optimal dithering based on the
    /// bit depth and sample rate combination.
    pub fn get_dither_options(&self) -> Vec<DitherType> {
        if self.bit_depth == Some(16) {
            vec![
                DitherType::None,
                DitherType::Tpdf,
                DitherType::Shibata,
                DitherType::LowShibata,
                DitherType::HighShibata,
                DitherType::Gesemann,
            ]
        } else {
            // 24-bit or 32-bit: only flat/sloped TPDF makes sense
            vec![DitherType::None, DitherType::Tpdf, DitherType::SlopedTpdf]
        }
    }

    pub fn get_sample_rate_options() -> Vec<(u32, &'static str)> {
        vec![
            (0, "Same as source"),
            (44100, "44.1 kHz"),
            (48000, "48 kHz"),
            (88200, "88.2 kHz"),
            (96000, "96 kHz"),
            (176400, "176.4 kHz"),
            (192000, "192 kHz"),
        ]
    }

    pub fn get_sample_rate_options_for_format(&self) -> Vec<(u32, &'static str)> {
        match self.selected_format {
            Some(AudioFormat::Mp3) => vec![
                (0, "Same as source"),
                (44100, "44.1 kHz"),
                (48000, "48 kHz"),
            ],
            Some(AudioFormat::Aac) => vec![
                (0, "Same as source"),
                (44100, "44.1 kHz"),
                (48000, "48 kHz"),
                (88200, "88.2 kHz"),
                (96000, "96 kHz"),
                (176400, "176.4 kHz"),
                (192000, "192 kHz"),
            ],
            Some(AudioFormat::Opus) => vec![
                (0, "Same as source"),
                (48000, "48 kHz (override built-in resampling)"),
            ],
            _ => Self::get_sample_rate_options(), // Full list for lossless formats
        }
    }

    pub fn get_compression_level_options() -> Vec<(u8, &'static str)> {
        vec![(0, "0 - Fastest"), (5, "5 - Balanced"), (8, "8 - Best")]
    }

    /// Get resampling quality options
    /// Resample quality levels (0-4):
    /// - 0 = Ultra (highest quality, slowest) - SoXR 32-bit / SoX -v / SSRC long
    /// - 1 = VHQ (very high quality) - SoXR 28-bit / SoX -h / SSRC long
    /// - 2 = HQ (high quality, default) - SoXR 24-bit / SoX -m / SSRC normal
    /// - 3 = MQ (medium quality) - SoXR 20-bit / SoX -l / SSRC short
    /// - 4 = LQ (lowest quality, fastest) - SoXR 16-bit / SoX -q / SSRC short
    pub fn get_resample_quality_options() -> Vec<(u8, &'static str)> {
        vec![
            (0, "Ultra"), // Highest quality (SoXR 32-bit / SoX -v)
            (1, "VHQ"),   // Very high quality (SoXR 28-bit / SoX -h)
            (2, "HQ"),    // High quality, default (SoXR 24-bit / SoX -m)
            (3, "MQ"),    // Medium quality (SoXR 20-bit / SoX -l)
        ]
    }

    pub fn get_nyquist_transition_options() -> Vec<NyquistTransition> {
        vec![
            NyquistTransition::Gentle,
            NyquistTransition::Steep,
            NyquistTransition::BrickWall,
        ]
    }

    pub fn should_show_dithering(&self) -> bool {
        // Only show dithering for 16-bit and 24-bit, not for 32-bit or "Same as source"
        self.bit_depth.is_some() && (self.bit_depth == Some(16) || self.bit_depth == Some(24))
    }

    pub fn should_show_resampling(&self) -> bool {
        self.sample_rate.is_some() && self.sample_rate != Some(0)
    }

    /// Check if processing options force re-encoding (can't use copy mode)
    /// Returns true if any option requires transcoding: resampling, bit depth change, or dithering
    pub fn is_reencode_forced(&self) -> bool {
        // Resampling requires re-encoding
        if self.sample_rate.is_some() && self.sample_rate != Some(0) {
            return true;
        }

        // Bit depth change requires re-encoding
        if self.bit_depth.is_some() && self.bit_depth != Some(0) {
            return true;
        }

        // Dithering requires re-encoding (only when bit depth is actually 16 or 24)
        if self.dither_type.is_some()
            && self.bit_depth.is_some()
            && (self.bit_depth == Some(16) || self.bit_depth == Some(24))
        {
            return true;
        }

        false
    }

    /// Get effective re-encode state (user choice OR forced by processing options)
    /// Used to display and extract the actual re-encode behavior
    pub fn get_effective_reencode_flac(&self) -> bool {
        self.reencode_flac.unwrap_or(false) || self.is_reencode_forced()
    }

    /// Check if SSRC Insane mode should be enabled
    /// Only available when Brick Wall is selected
    pub fn is_insane_mode_available(&self) -> bool {
        self.nyquist_transition == Some(NyquistTransition::BrickWall)
    }

    pub fn extract_settings(&self) -> ConversionSettings {
        ConversionSettings {
            format: self.selected_format.unwrap_or(AudioFormat::Flac),
            quality: self.selected_quality.clone(),

            // For lossless formats
            bit_depth: self.bit_depth,
            sample_rate: self.sample_rate,
            dither_type: self.dither_type,

            // For FLAC
            compression_level: self.compression_level,
            verify_encoding: self.verify_encoding,
            store_md5: self.store_md5,

            // For lossy formats
            aac_profile: self.aac_profile,
            opus_content_type: self.opus_content_type,

            // Resampling
            resample_quality: self.resample_quality,
            nyquist_transition: self.nyquist_transition,
            ssrc_insane_mode: self.ssrc_insane_mode,

            // Additional options
            replaygain_mode: self.replaygain_mode,
            copy_files: if self.copy_files_enabled {
                Some(self.copy_files_extensions.clone())
            } else {
                None
            },
            copy_subdirectories: if self.copy_subdirectories_enabled {
                Some(self.copy_subdirectories.clone())
            } else {
                None
            },
            merge_to_single: self.merge_to_single,
            destination_mode: self.destination_mode.clone(),
        }
    }

    pub fn show_destination_browser(&mut self) {
        // Show the file browser popup for selecting destination
        let start_path = match &self.destination_mode {
            DestinationMode::Custom(path) => std::path::PathBuf::from(path),
            DestinationMode::AskEveryTime => {
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
            }
        };

        let browser = FileBrowser::new(start_path);

        self.popup_state = Some(PopupState {
            popup_type: PopupType::FileBrowser(Box::new(browser)),
            input_text: String::new(),
            cursor_pos: 0,
            view_offset: 0,
            error_message: None,
            focused_element: PopupFocus::Input,
        });
    }

    /// Check if destination needs to be selected and show browser if needed
    /// Returns true if browser was shown, false if ready to start conversion
    pub fn check_and_prompt_for_destination(&mut self) -> bool {
        if self.destination_mode == DestinationMode::AskEveryTime {
            self.needs_destination_selection = true;
            self.show_destination_browser();
            true // Browser shown, not ready to start yet
        } else {
            false // Has destination, ready to start
        }
    }

    pub fn load_preset(&mut self, preset: &crate::presets::ConversionPreset) {
        // Load core wizard state
        self.selected_format = Some(preset.selected_format);
        self.selected_quality = preset.selected_quality.clone();

        // Load FLAC Advanced Options
        self.bit_depth = preset.bit_depth;
        self.sample_rate = preset.sample_rate;
        self.compression_level = preset.compression_level;
        self.resample_quality = preset.resample_quality;
        self.dither_type = preset.dither_type;
        self.nyquist_transition = preset.nyquist_transition;
        self.verify_encoding = preset.verify_encoding;
        self.store_md5 = preset.store_md5;

        // Load format-specific options
        self.opus_content_type = preset.opus_content_type;
        self.aac_profile = preset.aac_profile;

        // Load additional options
        self.replaygain_mode = preset.replaygain_mode;
        self.copy_files_enabled = preset.copy_files_enabled;
        self.copy_files_extensions = preset.copy_files_extensions.clone();
        self.copy_subdirectories_enabled = preset.copy_subdirectories_enabled;
        self.copy_subdirectories = preset.copy_subdirectories.clone();
        self.merge_to_single = preset.merge_to_single;
        self.reencode_flac = preset.reencode_flac;

        // Reset UI state to first page
        self.selected_index = 0;
        self.quality_index = 0;
        self.in_quality_area = false;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionSettings {
    pub format: AudioFormat,
    /// Quality setting as a string. For WavPack, this maps to compression flags:
    /// - "Fast (Low CPU, larger files)" → -f
    /// - "High (Balanced)" → -h
    /// - "Very High (Smaller files)" → -hh
    /// - "Maximum (Best compression)" → -hhh
    /// - "Ultra (Very slow)" → -hh -x
    /// - "Extreme (Slowest, smallest)" → -hh -x4
    pub quality: Option<String>,

    // For lossless formats
    pub bit_depth: Option<u32>,
    pub sample_rate: Option<u32>,
    pub dither_type: Option<DitherType>,

    // For FLAC
    pub compression_level: Option<u8>,
    pub verify_encoding: Option<bool>,
    pub store_md5: Option<bool>,

    // For lossy formats
    pub aac_profile: Option<AacProfile>,
    pub opus_content_type: Option<OpusContentType>,

    // Resampling
    pub resample_quality: Option<u8>,
    pub nyquist_transition: Option<NyquistTransition>,
    pub ssrc_insane_mode: Option<bool>,

    // Additional options
    pub replaygain_mode: Option<ReplayGainMode>,
    pub copy_files: Option<String>,
    pub copy_subdirectories: Option<String>,
    pub merge_to_single: Option<bool>,

    // Destination
    pub destination_mode: DestinationMode,
}

// File browser types
#[derive(Debug, Clone, PartialEq)]
pub struct FileBrowser {
    pub current_path: PathBuf,
    pub entries: Vec<FileEntry>,
    pub selected_index: usize,
    pub show_hidden: bool,
    pub last_click: Option<(usize, Instant)>,
    pub focus: BrowserFocus,
    pub show_new_folder_popup: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BrowserFocus {
    List,
    NewButton,
    SelectButton,
    CancelButton,
}

#[derive(Debug)]
pub enum BrowserAction {
    Selected(PathBuf),
    Cancelled,
    Continue,
}

impl FileBrowser {
    pub fn new(start_path: PathBuf) -> Self {
        // Canonicalize the path to ensure it's absolute
        let canonical_path = start_path.canonicalize().unwrap_or(start_path);
        let mut browser = Self {
            current_path: canonical_path,
            entries: Vec::new(),
            selected_index: 0,
            show_hidden: false,
            last_click: None,
            focus: BrowserFocus::List,
            show_new_folder_popup: false,
        };
        browser.refresh_entries();
        browser
    }

    pub fn refresh_entries(&mut self) {
        self.entries.clear();

        // Debug logging
        use std::fs::OpenOptions;
        use std::io::Write;
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open("wizard_areas.log")
        {
            let _ = writeln!(file, "\n=== FileBrowser refresh_entries ===");
            let _ = writeln!(file, "Current path: {:?}", self.current_path);
        }

        // Add parent directory option
        if let Some(parent) = self.current_path.parent() {
            // Canonicalize parent path to ensure it's absolute
            let parent_path = parent
                .canonicalize()
                .unwrap_or_else(|_| parent.to_path_buf());
            self.entries.push(FileEntry {
                name: "..".to_string(),
                path: parent_path,
                is_dir: true,
                size: None,
                modified: None,
            });
        }

        // Read directory entries
        match std::fs::read_dir(&self.current_path) {
            Ok(entries) => {
                let mut dir_entries = Vec::new();
                if let Ok(mut file) = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("wizard_areas.log")
                {
                    let _ = writeln!(file, "Successfully reading directory");
                }

                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();

                    // Skip hidden files if not showing them
                    if !self.show_hidden && name.starts_with('.') {
                        continue;
                    }

                    let metadata = entry.metadata().ok();
                    let is_dir = metadata.as_ref().map_or(false, |m| m.is_dir());

                    // Removed debug logging

                    // Only show directories in the file browser
                    if !is_dir {
                        continue;
                    }

                    let size =
                        metadata
                            .as_ref()
                            .and_then(|m| if !is_dir { Some(m.len()) } else { None });
                    let modified = metadata.as_ref().and_then(|m| m.modified().ok());

                    dir_entries.push(FileEntry {
                        name,
                        path,
                        is_dir,
                        size,
                        modified,
                    });
                }

                // Sort entries by name (directories first)
                dir_entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                });

                self.entries.extend(dir_entries);
            }
            Err(e) => {
                if let Ok(mut file) = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("wizard_areas.log")
                {
                    let _ = writeln!(file, "Error reading directory: {}", e);
                }
            }
        }

        // Reset selection if out of bounds
        if self.selected_index >= self.entries.len() {
            self.selected_index = 0;
        }
    }

    pub fn enter_selected(&mut self) {
        use std::fs::OpenOptions;
        use std::io::Write;

        if let Some(entry) = self.entries.get(self.selected_index) {
            if let Ok(mut file) = OpenOptions::new()
                .create(true)
                .append(true)
                .open("wizard_areas.log")
            {
                let _ = writeln!(file, "\n=== enter_selected ===");
                let _ = writeln!(file, "Selected: {} (path: {:?})", entry.name, entry.path);
            }

            if entry.is_dir {
                // Canonicalize the path to handle .. properly
                if let Ok(canonical) = entry.path.canonicalize() {
                    if let Ok(mut file) = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open("wizard_areas.log")
                    {
                        let _ = writeln!(file, "Canonicalized to: {:?}", canonical);
                    }
                    self.current_path = canonical;
                } else {
                    if let Ok(mut file) = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open("wizard_areas.log")
                    {
                        let _ = writeln!(file, "Failed to canonicalize, using: {:?}", entry.path);
                    }
                    self.current_path = entry.path.clone();
                }
                self.selected_index = 0;
                self.refresh_entries();
            }
        }
    }
}
