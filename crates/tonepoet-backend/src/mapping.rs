//! Parameter mapping utilities
//! 
//! Maps wizard settings to tool-specific parameters

use crate::types::*;
use crate::{Result, ConversionError};

/// Get PCM codec string for ffmpeg based on bit depth and endianness
pub fn get_pcm_codec(bit_depth: Option<u32>, big_endian: bool, format: AudioFormat) -> Result<String> {
    let depth = bit_depth.unwrap_or(16); // Default to 16-bit
    
    let codec = match depth {
        8 => {
            if big_endian { "pcm_s8" } else { "pcm_u8" }
        }
        16 => {
            if big_endian { "pcm_s16be" } else { "pcm_s16le" }
        }
        24 => {
            if big_endian { "pcm_s24be" } else { "pcm_s24le" }
        }
        32 => {
            if big_endian { "pcm_s32be" } else { "pcm_s32le" }
        }
        320 | 33 => {
            // Special case: 320 (new) or 33 (legacy) means float32
            // Check if format supports float
            if format.supports_float() {
                if big_endian { "pcm_f32be" } else { "pcm_f32le" }
            } else {
                // Fall back to 32-bit integer
                log::warn!("Format {} doesn't support float audio, using 32-bit integer", 
                          format.extension());
                if big_endian { "pcm_s32be" } else { "pcm_s32le" }
            }
        }
        _ => {
            return Err(ConversionError::InvalidSettings(
                format!("Unsupported bit depth: {}", depth)
            ));
        }
    };
    
    Ok(codec.to_string())
}

/// Map resample quality (0-4) to SoXR precision value
pub fn get_soxr_precision(quality: Option<u8>) -> u8 {
    match quality {
        Some(0) => 32,  // Ultra - Maximum quality (slowest)
        Some(1) => 28,  // VHQ - Very High Quality
        Some(2) => 24,  // HQ - High Quality (default)
        Some(3) => 20,  // MQ - Medium Quality
        Some(4) => 16,  // LQ - Low Quality (fastest)
        None => 24,     // Default to HQ
        Some(_) => 24,  // Invalid value, use default
    }
}

/// Map resample quality to sox resampling flag
pub fn get_sox_resample_flag(quality: Option<u8>) -> &'static str {
    match quality {
        Some(0) => "-v",  // Very high (175 dB rejection) - Highest
        Some(1) => "-h",  // High (125 dB rejection)
        Some(2) => "-m",  // Medium (100 dB rejection) - Default
        Some(3) => "-l",  // Low (100 dB rejection)
        Some(4) => "-q",  // Quick (~30 dB rejection) - Lowest
        None => "-m",     // Default to medium (balanced)
        Some(_) => "-m",  // Invalid value, use default
    }
}

/// Get SoXR dither method string
pub fn get_soxr_dither(dither: DitherType) -> &'static str {
    match dither {
        DitherType::None => "none",
        DitherType::Tpdf => "triangular",
        DitherType::Shibata | DitherType::LowShibata | DitherType::HighShibata => "shibata",
        DitherType::FShaped => "f_weighted",
        DitherType::ModifiedE | DitherType::ImprovedE => "modified_e_weighted",
        DitherType::Gesemann => "none",  // SoXR doesn't support Gesemann, force SoX
    }
}

/// Get sox dither arguments
pub fn get_sox_dither_args(dither: DitherType) -> Vec<String> {
    match dither {
        DitherType::None => vec![],
        DitherType::Tpdf => vec!["dither".to_string()],
        DitherType::Shibata => vec!["dither".to_string(), "-s".to_string()],
        DitherType::LowShibata => {
            // CORRECTED: Based on testing, low-shibata requires -s flag
            vec!["dither".to_string(), "-s".to_string(), "-f".to_string(), "low-shibata".to_string()]
        }
        DitherType::HighShibata => {
            // CORRECTED: Based on testing, high-shibata requires -s flag
            vec!["dither".to_string(), "-s".to_string(), "-f".to_string(), "high-shibata".to_string()]
        }
        DitherType::FShaped => {
            vec!["dither".to_string(), "-s".to_string(), "-f".to_string(), "f-weighted".to_string()]
        }
        DitherType::ModifiedE => {
            vec!["dither".to_string(), "-s".to_string(), "-f".to_string(), "modified-e-weighted".to_string()]
        }
        DitherType::ImprovedE => {
            vec!["dither".to_string(), "-s".to_string(), "-f".to_string(), "improved-e-weighted".to_string()]
        }
        DitherType::Gesemann => {
            // CORRECTED: Based on testing, correct syntax is dither -f gesemann
            vec!["dither".to_string(), "-f".to_string(), "gesemann".to_string()]
        }
    }
}

/// Get AAC profile string for ffmpeg
pub fn get_aac_profile_string(profile: AacProfile) -> String {
    match profile {
        AacProfile::LcAac => "aac_low".to_string(),
        AacProfile::HeAac => "aac_he".to_string(),
        AacProfile::HeAacV2 => "aac_he_v2".to_string(),
        AacProfile::LdAac => "aac_ld".to_string(),
    }
}

/// Get Opus application string for ffmpeg
pub fn get_opus_application(content: OpusContentType) -> String {
    match content {
        OpusContentType::Music => "audio".to_string(),
        OpusContentType::Speech => "voip".to_string(),
        OpusContentType::Auto => "audio".to_string(), // Default to audio
    }
}

/// Get Nyquist filter rolloff for sox
pub fn get_sox_rolloff(transition: Option<NyquistTransition>) -> f32 {
    match transition {
        Some(NyquistTransition::Sharp) => 0.997,  // Sharp/steep rolloff - 99.7% of Nyquist (deprecated, use Steep)
        Some(NyquistTransition::Medium) => 0.97,  // Medium rolloff - balanced
        Some(NyquistTransition::Gentle) => 0.95,  // Gentle rolloff - 95% of Nyquist, gradual transition
        Some(NyquistTransition::Steep) => 0.997,  // Steep rolloff - 99.7% of Nyquist, sharp transition
        Some(NyquistTransition::BrickWall) => 0.997, // Brick wall requires SSRC, fallback value
        None => 0.95, // Default to gentle (95%)
    }
}

/// Get Nyquist filter cutoff for FFmpeg (SWResampler)
/// FFmpeg's cutoff parameter: 0.0-1.0, where 1.0 = Nyquist frequency
pub fn get_ffmpeg_cutoff(transition: NyquistTransition) -> f32 {
    match transition {
        NyquistTransition::Sharp => 0.997,  // Sharp/steep rolloff - 99.7% of Nyquist (deprecated, use Steep)
        NyquistTransition::Medium => 0.97,  // Medium rolloff - balanced
        NyquistTransition::Gentle => 0.95,  // Gentle rolloff - 95% of Nyquist, gradual transition
        NyquistTransition::Steep => 0.997,  // Steep rolloff - 99.7% of Nyquist, sharp transition
        NyquistTransition::BrickWall => 0.997, // Brick wall requires SSRC, fallback value
    }
}

/// Get SSRC dither ID for different dither types
/// Based on actual testing results from TOOL_TESTING_LOG.md
pub fn get_ssrc_dither_id(dither: Option<DitherType>) -> u8 {
    match dither {
        Some(DitherType::None) => 99,        // No shaper (confirmed)
        Some(DitherType::Tpdf) => 98,        // Simple triangular (confirmed)
        Some(DitherType::Shibata) => 2,      // ATH Curve A, Intensity 2 (confirmed)
        Some(DitherType::LowShibata) => 0,   // ATH Curve A, Intensity 0 (confirmed)
        Some(DitherType::HighShibata) => 6,  // ATH Curve A, Intensity 6 (confirmed)
        Some(DitherType::Gesemann) => 99,    // SSRC doesn't support Gesemann, use no shaper
        _ => 99, // Default to no shaper for other unsupported types
    }
}

/// Get SSRC profile based on resample quality
///
/// Maps quality levels to SSRC 2.42.x profiles based on stop-band attenuation:
/// - 0 (Ultra): `high` profile - 170 dB attenuation, double precision
/// - 1 (VHQ): `long` profile - 145 dB attenuation, double precision
/// - 2 (HQ): `standard` profile - 145 dB attenuation, single precision [DEFAULT]
/// - 3 (MQ): `short` profile - 96 dB attenuation, single precision
/// - 4 (LQ): `fast` profile - 96 dB attenuation, single precision, faster FFT
///
/// SSRC 2.42.x Profiles (from slowest to fastest):
/// - `insane` (262144 FFT, 200 dB, double) - Extreme quality (enabled via checkbox)
/// - `high` (65536 FFT, 170 dB, double) - Ultra quality
/// - `long` (32768 FFT, 145 dB, double) - VHQ quality
/// - `standard` (16384 FFT, 145 dB, single) - HQ/default quality
/// - `short` (4096 FFT, 96 dB, single) - MQ quality
/// - `fast` (1024 FFT, 96 dB, single) - LQ quality
/// - `lightning` (256 FFT, 96 dB, single) - Not used (real-time only)
///
/// Backend: SSRC brick wall resampler (only used when Nyquist=BrickWall)
pub fn get_ssrc_profile(quality: Option<u8>, insane_mode: bool) -> &'static str {
    if insane_mode {
        return "insane";
    }
    match quality {
        Some(0) => "high",      // Ultra: 170 dB (vs Sox 175 dB)
        Some(1) => "long",      // VHQ: 145 dB (vs Sox 125 dB)
        Some(2) => "standard",  // HQ: 145 dB (vs Sox 100 dB) - default
        Some(3) => "short",     // MQ: 96 dB (vs Sox 100 dB)
        Some(4) => "fast",      // LQ: 96 dB (vs Sox ~30 dB)
        None => "standard",     // Default to standard
        Some(_) => "standard",  // Invalid: default to standard
    }
}