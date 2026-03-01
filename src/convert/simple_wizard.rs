use crate::convert::AudioFormat;
use serde::{Serialize, Deserialize};

/// ReplayGain scan mode
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReplayGainMode {
    Track,
    Album,
    Both,
}

/// Dither type for bit depth reduction
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum DitherType {
    None,
    TPDF,                    // Standard TPDF white noise
    SloppedTPDF,             // -S option
    Shibata,                 // -s option (noise shaping with shibata)
    Lipshitz,                // -f lipshitz
    FWeighted,               // -f f-weighted
    ModifiedEWeighted,       // -f modified-e-weighted
    ImprovedEWeighted,       // -f improved-e-weighted
    Gesemann,                // -f gesemann
    LowShibata,              // -f low-shibata
    HighShibata,             // -f high-shibata
}

/// Nyquist filter transition for resampling
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum NyquistTransition {
    Gentle,     // Gentle rolloff, less aliasing
    Steep,      // Steep rolloff, more aliasing
    BrickWall,  // SSRC brick wall resampler
}

/// Sections within the FLAC advanced options
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlacSection {
    BitDepth,
    Dithering,
    SampleRate,
    CompressionLevel,
    ResamplingQuality,
    ProcessingOptions,
}

/// A simple wizard that actually works
#[derive(Debug, Clone)]
pub struct SimpleWizard {
    /// Current step (0 = format, 1 = quality, 2 = additional options, 3 = confirm)
    pub current_step: usize,
    
    /// Selected format
    pub selected_format: Option<AudioFormat>,
    
    /// Selected quality (for now just a string)
    pub selected_quality: Option<String>,
    
    /// Selected option index for current step
    pub selected_index: usize,
    
    /// Current section for FLAC options
    pub flac_section: FlacSection,
    
    /// Selected index within the current FLAC section
    pub flac_section_index: usize,
    
    /// Selected index for Additional Options page (step 2)
    pub additional_options_index: usize,
    
    /// Scroll offset for content area
    pub scroll_offset: usize,
    
    /// Total content height (for scroll calculation)
    pub content_height: usize,
    
    // FLAC Advanced Options
    /// Bit depth (0 = same as source)
    pub bit_depth: Option<u32>,
    
    /// Sample rate (0 = same as source)
    pub sample_rate: Option<u32>,
    
    /// FLAC compression level (0-8)
    pub compression_level: Option<u8>,
    
    /// Resampling quality (0-4: LQ,MQ,HQ,VHQ,Ultra)
    pub resample_quality: Option<u8>,
    
    /// Dithering option
    pub dither_type: Option<DitherType>,
    
    /// Processing options
    pub verify_encoding: Option<bool>,
    pub calculate_replaygain: Option<bool>,
    pub store_md5: Option<bool>,
    pub delete_source: Option<bool>,
    
    // Additional options (step 2)
    /// ReplayGain scan mode
    pub replaygain_mode: Option<ReplayGainMode>,
    
    /// Copy associated files
    pub copy_text_files: Option<bool>,
    pub copy_image_files: Option<bool>,
    
    /// Merge into single file with cue
    pub merge_to_single: Option<bool>,
}

impl SimpleWizard {
    pub fn new() -> Self {
        Self {
            current_step: 0,
            selected_format: None,
            selected_quality: None,
            selected_index: 0,
            flac_section: FlacSection::BitDepth,
            flac_section_index: 0,
            additional_options_index: 0,
            scroll_offset: 0,
            content_height: 0,
            // FLAC defaults
            bit_depth: Some(0), // Same as source
            sample_rate: Some(0), // Same as source
            compression_level: Some(8), // Best compression (default)
            resample_quality: Some(2), // HQ
            dither_type: None, // Will be set based on bit depth selection
            verify_encoding: Some(true),
            calculate_replaygain: Some(true),
            store_md5: Some(false),
            delete_source: Some(false),
            // Additional options defaults
            replaygain_mode: Some(ReplayGainMode::Album),
            copy_text_files: Some(true),
            copy_image_files: Some(true),
            merge_to_single: Some(false),
        }
    }
    
    pub fn next_step(&mut self) {
        if self.can_proceed() {
            self.current_step += 1;
            self.selected_index = 0; // Reset selection for new step
            self.scroll_offset = 0; // Reset scroll position
            
            // Auto-select high-quality defaults when entering quality step
            if self.current_step == 1 && self.selected_quality.is_none() {
                let default_quality = match self.selected_format.as_ref() {
                    Some(AudioFormat::Flac) => "High",
                    Some(AudioFormat::Mp3) => "320 kbps",     // Highest quality, most compatible
                    Some(AudioFormat::Aac) => "256 kbps",     // LC @ 256 kbps, excellent quality
                    Some(AudioFormat::Opus) => "Very High",   // ~256-320 kbps, archival quality
                    Some(AudioFormat::Wav) | Some(AudioFormat::Aiff) => "Original",
                    _ => "Default",
                };
                self.selected_quality = Some(default_quality.to_string());
            }
        }
    }
    
    pub fn prev_step(&mut self) {
        if self.current_step > 0 {
            self.current_step -= 1;
            self.selected_index = 0; // Reset selection for new step
            self.scroll_offset = 0; // Reset scroll position
        }
    }
    
    pub fn can_proceed(&self) -> bool {
        match self.current_step {
            0 => self.selected_format.is_some(),
            1 => self.selected_quality.is_some(),
            2 => true, // Additional options page can always proceed
            _ => false,
        }
    }
    
    pub fn is_complete(&self) -> bool {
        self.current_step >= 3 && self.selected_format.is_some() && self.selected_quality.is_some()
    }
}