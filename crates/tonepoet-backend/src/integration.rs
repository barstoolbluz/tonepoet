//! Integration support for tonepoet conversion backend
//!
//! This module provides the functions needed to integrate the conversion backend
//! with the tonepoet concurrent processing system.

use crate::types::{ConversionSettings, AudioFormat, OpusContentType, AacProfile, Mp3Mode, ReplayGainMode, DitherType, NyquistTransition};

/// Maps from the main project's ConversionOptions to our ConversionSettings
pub fn map_conversion_item_to_settings(item: &ConversionItem) -> ConversionSettings {
    // Get the base format
    let format = match item.output_format {
        // Direct mapping for formats that exist in both
        MainAudioFormat::Flac => AudioFormat::Flac,
        MainAudioFormat::Wav => AudioFormat::Wav,
        MainAudioFormat::Aiff => AudioFormat::Aiff,
        MainAudioFormat::WavPack => AudioFormat::WavPack,
        MainAudioFormat::Mp3 => AudioFormat::Mp3,
        MainAudioFormat::Aac => AudioFormat::Aac,
        MainAudioFormat::Opus => AudioFormat::Opus,
    };
    
    // Extract settings from quality settings
    let (bit_depth, compression_level, mp3_bitrate, mp3_quality, mp3_mode, aac_profile) =
        match &item.options.quality {
            MainQualitySettings::Flac { compression_level } => {
                (None, Some(*compression_level), None, None, None, None)
            },
            MainQualitySettings::Wav { bit_depth, sample_rate: _ } => {
                let mapped_bit_depth = if *bit_depth == 0 { None } else { Some(*bit_depth as u32) };
                (mapped_bit_depth, None, None, None, None, None)
            },
            MainQualitySettings::Aiff { bit_depth, sample_rate: _ } => {
                let mapped_bit_depth = if *bit_depth == 0 { None } else { Some(*bit_depth as u32) };
                (mapped_bit_depth, None, None, None, None, None)
            },
            MainQualitySettings::Mp3 { bitrate_mode, quality: _ } => {
                let (bitrate, mp3_quality, mode) = match bitrate_mode {
                    MainMp3BitrateMode::Cbr { bitrate } => (Some(*bitrate), None, Some(Mp3Mode::Cbr)),
                    MainMp3BitrateMode::Vbr { quality } => (None, Some(*quality), Some(Mp3Mode::Vbr)),
                    MainMp3BitrateMode::Abr { bitrate } => (Some(*bitrate), None, Some(Mp3Mode::Abr)),
                };
                (None, None, bitrate, mp3_quality, mode, None)
            },
            MainQualitySettings::Aac { bitrate, profile } => {
                let aac_profile = match profile {
                    MainAacProfile::Lc => Some(AacProfile::LcAac),
                    MainAacProfile::He => Some(AacProfile::HeAac),
                    MainAacProfile::HeV2 => Some(AacProfile::HeAacV2),
                };
                (None, None, Some(*bitrate), None, None, aac_profile)
            },
            MainQualitySettings::Opus { bitrate, complexity: _ } => {
                (None, None, Some(*bitrate), None, None, None)
            },
            MainQualitySettings::WavPack { compression_mode, hybrid_mode: _, correction_file: _ } => {
                let compression_level = match compression_mode {
                    MainWavPackMode::Fast => Some(0),
                    MainWavPackMode::Normal => Some(2),
                    MainWavPackMode::High => Some(4),
                    MainWavPackMode::VeryHigh => Some(6),
                };
                (None, compression_level, None, None, None, None)
            },
        };

    // Use target_sample_rate from ConversionOptions for ALL formats
    let sample_rate = item.options.target_sample_rate;

    // Use bit_depth from quality settings if available, otherwise fall back to target_bit_depth
    // This ensures FLAC/WavPack get user's bit depth selection (only WAV/AIFF store in quality)
    let bit_depth_from_quality = bit_depth;
    let bit_depth = bit_depth_from_quality.or(item.options.target_bit_depth);

    log::debug!("🎯 Bit depth resolution: quality={:?}, target={:?}, final={:?}",
                bit_depth_from_quality, item.options.target_bit_depth, bit_depth);

    ConversionSettings {
        // Preset metadata
        name: None,
        version: None,

        // Target format
        format,
        selected_quality: None,

        // Audio parameters
        bit_depth,
        sample_rate,

        // Source metadata (from main project detection)
        source_bit_depth: item.source_bit_depth,
        source_sample_rate: item.source_sample_rate,

        // Quality settings - properly mapped from main project
        resample_quality: item.options.resample_quality,
        compression_level,

        // Processing options
        dither_type: item.options.dither_type.map(|dt| {
            match dt {
                MainDitherType::None => DitherType::None,
                MainDitherType::TPDF => DitherType::Tpdf,
                MainDitherType::SloppedTPDF => DitherType::Tpdf,
                MainDitherType::Shibata => DitherType::Shibata,
                MainDitherType::Lipshitz => DitherType::Tpdf,
                MainDitherType::FWeighted => DitherType::FShaped,
                MainDitherType::ModifiedEWeighted => DitherType::ModifiedE,
                MainDitherType::ImprovedEWeighted => DitherType::ImprovedE,
                MainDitherType::Gesemann => DitherType::Gesemann,
                MainDitherType::LowShibata => DitherType::LowShibata,
                MainDitherType::HighShibata => DitherType::HighShibata,
            }
        }),
        nyquist_transition: item.options.nyquist_transition.map(|nt| {
            match nt {
                MainNyquistTransition::Gentle => NyquistTransition::Gentle,
                MainNyquistTransition::Steep => NyquistTransition::Steep,
                MainNyquistTransition::BrickWall => NyquistTransition::BrickWall,
            }
        }),
        
        // Format-specific
        opus_content_type: Some(OpusContentType::Music),
        aac_profile,
        
        // MP3 specific
        mp3_bitrate,
        mp3_quality,
        mp3_mode,
        
        // Encoding verification
        verify_encoding: None,
        store_md5: None,
        
        // ReplayGain settings
        replaygain_mode: if item.options.calculate_replaygain {
            let mode = item.options.replaygain_mode.clone().map(|mode| {
                // Convert from main project's ReplayGainMode to backend's ReplayGainMode
                match mode {
                    MainReplayGainMode::Track => ReplayGainMode::Track,
                    MainReplayGainMode::Album => ReplayGainMode::Album,
                    MainReplayGainMode::Both => ReplayGainMode::Both,
                }
            }).or(Some(ReplayGainMode::Track)); // Default to Track if no mode specified
            log::info!("🎯 Backend ReplayGain mode for item {}: {:?} (from options: {:?})",
                item.id, mode, item.options.replaygain_mode);
            mode
        } else {
            log::info!("🎯 Backend ReplayGain disabled for item {}", item.id);
            None
        },
        
        // Post-processing file operations - properly mapped from main project
        copy_files_enabled: Some(item.options.copy_auxiliary_files),
        copy_files_extensions: Some("txt,cue,log,ini,nfo,m3u,m3u8,sfv".to_string()),
        copy_subdirectories_enabled: Some(item.options.copy_subdirectories),
        copy_subdirectories: Some("*".to_string()),
        merge_to_single: None,
        reencode_flac: None,
        ssrc_insane_mode: item.options.ssrc_insane_mode,
        lineage_file_path: None, // Set later in convert_with_backend

        // File handling
        overwrite: item.options.overwrite,
    }
}

/// Progress update structure that matches the main project
#[derive(Debug)]
pub struct ProgressUpdate {
    pub item_id: String,
    pub progress: f32,
    pub status: ConversionStatus,
}

/// ConversionPhase that matches the main project  
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConversionPhase {
    Extracting,      // 0% → 15%
    Analyzing,       // 15% → 20%
    Renaming,        // 20% → 30%
    Tagging,         // 30% → 40%
    Converting,      // 40% → 90%  ← CONVERSION BACKEND INTEGRATION POINT
    PostProcessing,  // 90% → 95%
    Finalizing,      // 95% → 100%
}

impl ConversionPhase {
    /// Calculate overall progress based on phase progress
    pub fn calculate_overall_progress(&self, phase_progress: f32) -> f32 {
        let (start_percent, range_percent) = match self {
            ConversionPhase::Extracting => (0.0, 15.0),
            ConversionPhase::Analyzing => (15.0, 5.0),
            ConversionPhase::Renaming => (20.0, 10.0),
            ConversionPhase::Tagging => (30.0, 10.0),
            ConversionPhase::Converting => (40.0, 50.0),
            ConversionPhase::PostProcessing => (90.0, 5.0),
            ConversionPhase::Finalizing => (95.0, 5.0),
        };
        
        start_percent + (phase_progress / 100.0) * range_percent
    }
}

/// ConversionStatus that matches the main project
#[derive(Debug, Clone, PartialEq)]
pub enum ConversionStatus {
    NotConfigured,
    Queued,
    Processing { 
        progress: f32,
        message: Option<String>,
        file_progress: Option<(u32, u32)>,
        phase: Option<ConversionPhase>,
        phase_progress: Option<f32>,
    },
    Completed { output_path: std::path::PathBuf },
    Failed { error: String },
    Paused,
    Cancelled,
}

/// Placeholder types that would be imported from the main project
/// These represent the main project's types
pub struct ConversionItem {
    pub id: String,
    pub output_format: MainAudioFormat,
    pub options: MainConversionOptions,
    pub source_bit_depth: Option<u16>,     // Detected source bit depth
    pub source_sample_rate: Option<u32>,   // Detected source sample rate
    pub append_lineage: bool,              // Whether to append Lineage.txt to COMMENT tag
}

#[derive(Debug, Clone, Copy)]
pub enum MainAudioFormat {
    Flac, Wav, Aiff, WavPack, Mp3, Aac, Opus,
}

#[derive(Debug, Clone, Copy)]
pub enum MainReplayGainMode {
    Track,
    Album,
    Both,
}

pub struct MainConversionOptions {
    pub quality: MainQualitySettings,
    pub calculate_replaygain: bool,
    pub replaygain_mode: Option<MainReplayGainMode>,
    pub overwrite: bool,
    pub resample_quality: Option<u8>,
    pub nyquist_transition: Option<MainNyquistTransition>,
    pub dither_type: Option<MainDitherType>,
    pub target_sample_rate: Option<u32>,
    pub target_bit_depth: Option<u32>,
    pub copy_auxiliary_files: bool,
    pub copy_subdirectories: bool,
    pub ssrc_insane_mode: Option<bool>,
    pub append_lineage_to_comment: bool,   // Append Lineage.txt content to COMMENT tag
}

#[derive(Debug, Clone)]
pub enum MainQualitySettings {
    Flac { compression_level: u8 },
    Wav { bit_depth: u16, sample_rate: u32 },
    Aiff { bit_depth: u16, sample_rate: u32 },
    WavPack { compression_mode: MainWavPackMode, hybrid_mode: bool, correction_file: bool },
    Mp3 { bitrate_mode: MainMp3BitrateMode, quality: u8 },
    Aac { bitrate: u32, profile: MainAacProfile },
    Opus { bitrate: u32, complexity: u8 },
}

#[derive(Debug, Clone, Copy)]
pub enum MainWavPackMode { Fast, Normal, High, VeryHigh }

#[derive(Debug, Clone)]
pub enum MainMp3BitrateMode {
    Cbr { bitrate: u32 },
    Vbr { quality: u8 },
    Abr { bitrate: u32 },
}

#[derive(Debug, Clone, Copy)]
pub enum MainAacProfile { Lc, He, HeV2 }

#[derive(Debug, Clone, Copy)]
pub enum MainNyquistTransition {
    Gentle,
    Steep,
    BrickWall,
}

#[derive(Debug, Clone, Copy)]
pub enum MainDitherType {
    None,
    TPDF,
    SloppedTPDF,
    Shibata,
    Lipshitz,
    FWeighted,
    ModifiedEWeighted,
    ImprovedEWeighted,
    Gesemann,
    LowShibata,
    HighShibata,
}

/// Helper function to calculate phase progress mapping
pub fn calculate_phase_progress(
    stage_progress: f32,
    target_phase: ConversionPhase,
) -> (f32, f32) {
    // stage_progress is 0-100% within the pipeline
    // Return (overall_progress, phase_progress)
    let overall_progress = target_phase.calculate_overall_progress(stage_progress);
    (overall_progress, stage_progress)
}