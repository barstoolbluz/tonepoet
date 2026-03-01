//! Settings validation

use crate::types::*;
use crate::{Result, ConversionError};

/// Validate conversion settings before building commands
pub fn validate_settings(settings: &ConversionSettings) -> Result<()> {
    // Validate bit depth
    if let Some(depth) = settings.bit_depth {
        if depth != 0 && depth != 8 && depth != 16 && depth != 24 && depth != 32 && depth != 320 && depth != 33 {
            // Provide helpful error messages for common mistakes  
            let suggestion = match depth {
                64 => "Invalid bit depth: 64. Did you mean 32 (integer) or 320 (float32)?".to_string(),
                _ => format!("Invalid bit depth: {}. Supported: 8, 16, 24, 32, 320 (float32)", depth)
            };
            return Err(ConversionError::InvalidSettings(suggestion));
        }
        
        // Check float support (for both 33 and 320)
        if (depth == 320 || depth == 33) && !settings.format.supports_float() {
            log::warn!(
                "Format {} doesn't support float audio, will use 32-bit integer instead",
                settings.format.extension()
            );
        }
        
        // Legacy compatibility: silently convert bit_depth=33 to 320
        if depth == 33 {
            log::info!("Converting legacy bit_depth=33 to bit_depth=320 (float32)");
        }
    }
    
    // Validate sample rate
    if let Some(rate) = settings.sample_rate {
        if rate != 0 && (rate < 8000 || rate > 384000) {
            return Err(ConversionError::InvalidSettings(
                format!("Invalid sample rate: {}. Range: 8000-384000 Hz", rate)
            ));
        }
    }
    
    // Validate resample quality
    if let Some(quality) = settings.resample_quality {
        if quality > 4 {
            return Err(ConversionError::InvalidSettings(
                format!("Invalid resample quality: {}. Range: 0-4", quality)
            ));
        }
    }
    
    // Validate compression level based on format
    if let Some(level) = settings.compression_level {
        match settings.format {
            AudioFormat::Flac => {
                if level > 8 {
                    return Err(ConversionError::InvalidSettings(
                        format!("FLAC compression level must be 0-8, got {}", level)
                    ));
                }
            }
            AudioFormat::WavPack => {
                if level > 6 {
                    return Err(ConversionError::InvalidSettings(
                        format!("WavPack compression level must be 0-6, got {}", level)
                    ));
                }
            }
            _ => {
                // Other formats don't use compression_level
                if level > 0 {
                    log::warn!(
                        "Compression level {} ignored for format {}",
                        level,
                        settings.format.extension()
                    );
                }
            }
        }
    }
    
    // Validate MP3 settings
    if settings.format == AudioFormat::Mp3 {
        if let Some(mode) = settings.mp3_mode {
            match mode {
                Mp3Mode::Cbr | Mp3Mode::Abr => {
                    if settings.mp3_bitrate.is_none() {
                        return Err(ConversionError::InvalidSettings(
                            "MP3 CBR/ABR mode requires bitrate setting".to_string()
                        ));
                    }
                }
                Mp3Mode::Vbr => {
                    if settings.mp3_quality.is_none() {
                        return Err(ConversionError::InvalidSettings(
                            "MP3 VBR mode requires quality setting".to_string()
                        ));
                    }
                }
            }
        }
        
        // Validate bitrate range
        if let Some(bitrate) = settings.mp3_bitrate {
            if bitrate < 32 || bitrate > 320 {
                return Err(ConversionError::InvalidSettings(
                    format!("MP3 bitrate must be 32-320 kbps, got {}", bitrate)
                ));
            }
        }
        
        // Validate VBR quality
        if let Some(quality) = settings.mp3_quality {
            if quality > 9 {
                return Err(ConversionError::InvalidSettings(
                    format!("MP3 VBR quality must be 0-9, got {}", quality)
                ));
            }
        }
    }
    
    // Validate format-specific settings
    match settings.format {
        AudioFormat::Opus => {
            // Opus doesn't support custom bit depths
            if let Some(depth) = settings.bit_depth {
                if depth != 0 && depth != 16 {
                    log::warn!("Opus always uses 16-bit depth internally, ignoring bit_depth={}", depth);
                }
            }
        }
        AudioFormat::Mp3 | AudioFormat::Aac => {
            // Lossy formats don't support bit depth selection
            if settings.bit_depth.is_some() {
                log::warn!(
                    "{} is a lossy format, bit depth setting will be ignored",
                    settings.format.extension()
                );
            }
        }
        _ => {}
    }
    
    Ok(())
}

/// Check if settings require resampling
pub fn needs_resampling(settings: &ConversionSettings) -> bool {
    match (settings.sample_rate, settings.source_sample_rate) {
        (Some(target), Some(source)) if target > 0 && target != source => {
            log::debug!("🔍 Resampling needed: {} Hz → {} Hz", source, target);
            true
        },
        _ => {
            log::debug!("🔍 No resampling needed");
            false
        }
    }
}

/// Check if settings require dithering
pub fn needs_dithering(settings: &ConversionSettings) -> bool {
    if let Some(dither) = settings.dither_type {
        if dither == DitherType::None {
            return false;
        }
        // Dithering only makes sense when reducing bit depth
        needs_bit_depth_reduction(settings)
    } else {
        false
    }
}

/// Check if settings require bit depth reduction
/// Returns true if target bit depth is less than source bit depth
pub fn needs_bit_depth_reduction(settings: &ConversionSettings) -> bool {
    match (settings.bit_depth, settings.source_bit_depth) {
        (Some(target), Some(source)) if target > 0 && target < source as u32 => {
            log::debug!("🔍 Bit depth reduction needed: {} → {} bit", source, target);
            true
        },
        _ => {
            log::debug!("🔍 No bit depth reduction needed");
            false
        }
    }
}