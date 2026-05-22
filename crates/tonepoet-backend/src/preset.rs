//! Preset loading and parsing functionality

use crate::types::*;
use crate::{ConversionError, Result};
use std::fs;
use std::path::Path;

/// Load a conversion preset from a TOML file
pub fn load_preset<P: AsRef<Path>>(path: P) -> Result<ConversionSettings> {
    let content = fs::read_to_string(path)
        .map_err(|e| ConversionError::InvalidSettings(format!("Failed to read preset: {}", e)))?;

    parse_preset_toml(&content)
}

/// Parse TOML content into ConversionSettings
pub fn parse_preset_toml(content: &str) -> Result<ConversionSettings> {
    #[derive(serde::Deserialize)]
    struct PresetToml {
        name: Option<String>,
        version: Option<u32>,
        selected_format: String,
        selected_quality: Option<String>,
        bit_depth: Option<u32>,
        sample_rate: Option<u32>,
        compression_level: Option<u8>,
        resample_quality: Option<u8>,
        dither_type: Option<String>,
        nyquist_transition: Option<String>,
        verify_encoding: Option<bool>,
        store_md5: Option<bool>,
        opus_content_type: Option<String>,
        aac_profile: Option<String>,
        replaygain_mode: Option<String>,
        copy_files_enabled: Option<bool>,
        copy_files_extensions: Option<String>,
        copy_subdirectories_enabled: Option<bool>,
        copy_subdirectories: Option<String>,
        merge_to_single: Option<bool>,
        reencode_flac: Option<bool>,
    }

    let preset: PresetToml = toml::from_str(content)
        .map_err(|e| ConversionError::InvalidSettings(format!("Invalid TOML: {}", e)))?;

    // Convert string format to enum (case-insensitive)
    let format = match preset.selected_format.to_lowercase().as_str() {
        "flac" => AudioFormat::Flac,
        "wav" => AudioFormat::Wav,
        "aiff" => AudioFormat::Aiff,
        "wavpack" => AudioFormat::WavPack,
        "mp3" => AudioFormat::Mp3,
        "aac" => AudioFormat::Aac,
        "opus" => AudioFormat::Opus,
        "alac" => AudioFormat::Alac,
        other => {
            return Err(ConversionError::InvalidSettings(format!(
                "Unknown audio format: {}",
                other
            )))
        }
    };

    // Convert string dither type to enum (case-insensitive)
    let dither_type = preset
        .dither_type
        .and_then(|s| match s.to_lowercase().as_str() {
            "none" => Some(DitherType::None),
            "tpdf" => Some(DitherType::Tpdf),
            "shibata" => Some(DitherType::Shibata),
            "lowshibata" => Some(DitherType::LowShibata),
            "highshibata" => Some(DitherType::HighShibata),
            "fshaped" => Some(DitherType::FShaped),
            "modifiede" => Some(DitherType::ModifiedE),
            "improvede" => Some(DitherType::ImprovedE),
            "gesemann" => Some(DitherType::Gesemann),
            _ => None,
        });

    // Convert string nyquist transition to enum (case-insensitive)
    let nyquist_transition =
        preset
            .nyquist_transition
            .and_then(|s| match s.to_lowercase().as_str() {
                "sharp" => Some(NyquistTransition::Sharp),
                "medium" => Some(NyquistTransition::Medium),
                "gentle" => Some(NyquistTransition::Gentle),
                "steep" => Some(NyquistTransition::Steep),
                "brickwall" => Some(NyquistTransition::BrickWall),
                _ => None,
            });

    // Convert other string enums (case-insensitive)
    let opus_content_type =
        preset
            .opus_content_type
            .and_then(|s| match s.to_lowercase().as_str() {
                "music" => Some(OpusContentType::Music),
                "speech" => Some(OpusContentType::Speech),
                "auto" => Some(OpusContentType::Auto),
                _ => None,
            });

    let aac_profile = preset
        .aac_profile
        .and_then(|s| match s.to_lowercase().as_str() {
            "lcaac" => Some(AacProfile::LcAac),
            "heaac" => Some(AacProfile::HeAac),
            "heaacv2" => Some(AacProfile::HeAacV2),
            "ldaac" => Some(AacProfile::LdAac),
            _ => None,
        });

    let replaygain_mode = preset
        .replaygain_mode
        .and_then(|s| match s.to_lowercase().as_str() {
            "track" => Some(ReplayGainMode::Track),
            "album" => Some(ReplayGainMode::Album),
            "both" => Some(ReplayGainMode::Both),
            _ => None,
        });

    // Parse selected_quality string for format-specific parameters
    let (mp3_bitrate, mp3_quality, mp3_mode) = if format == AudioFormat::Mp3 {
        parse_mp3_quality(&preset.selected_quality)
    } else {
        (None, None, None)
    };

    Ok(ConversionSettings {
        name: preset.name,
        version: preset.version,
        format,
        selected_quality: preset.selected_quality,
        bit_depth: preset.bit_depth,
        sample_rate: preset.sample_rate,
        source_bit_depth: None, // Presets don't have source info (will be populated at runtime)
        source_sample_rate: None,
        resample_quality: preset.resample_quality,
        compression_level: preset.compression_level,
        dither_type,
        nyquist_transition,
        opus_content_type,
        aac_profile,
        mp3_bitrate,
        mp3_quality,
        mp3_mode,
        verify_encoding: preset.verify_encoding,
        store_md5: preset.store_md5,
        replaygain_mode,
        copy_files_enabled: preset.copy_files_enabled,
        copy_files_extensions: preset.copy_files_extensions,
        copy_subdirectories_enabled: preset.copy_subdirectories_enabled,
        copy_subdirectories: preset.copy_subdirectories,
        merge_to_single: preset.merge_to_single,
        reencode_flac: preset.reencode_flac,
        ssrc_insane_mode: None, // Presets don't include insane mode (user checkbox only)
        lineage_file_path: None, // Not part of presets (runtime-determined)
        overwrite: false,       // Default
    })
}

/// Parse MP3 selected_quality string into bitrate, quality, and mode
fn parse_mp3_quality(quality_str: &Option<String>) -> (Option<u32>, Option<u8>, Option<Mp3Mode>) {
    let quality = match quality_str {
        Some(s) => s,
        None => return (None, None, None),
    };

    // Parse common MP3 quality patterns (case-insensitive)
    let quality_lower = quality.to_lowercase();

    if quality_lower.contains("320 kbps") || quality_lower.contains("320kbps") {
        (Some(320), None, Some(Mp3Mode::Cbr))
    } else if quality_lower.contains("256 kbps") || quality_lower.contains("256kbps") {
        (Some(256), None, Some(Mp3Mode::Cbr))
    } else if quality_lower.contains("192 kbps") || quality_lower.contains("192kbps") {
        (Some(192), None, Some(Mp3Mode::Cbr))
    } else if quality_lower.contains("128 kbps") || quality_lower.contains("128kbps") {
        (Some(128), None, Some(Mp3Mode::Cbr))
    } else if quality_lower.contains("v0") {
        (None, Some(0), Some(Mp3Mode::Vbr))
    } else if quality_lower.contains("v1") {
        (None, Some(1), Some(Mp3Mode::Vbr))
    } else if quality_lower.contains("v2") {
        (None, Some(2), Some(Mp3Mode::Vbr))
    } else if quality_lower.contains("v3") {
        (None, Some(3), Some(Mp3Mode::Vbr))
    } else if quality_lower.contains("v4") {
        (None, Some(4), Some(Mp3Mode::Vbr))
    } else if quality_lower.contains("v5") {
        (None, Some(5), Some(Mp3Mode::Vbr))
    } else {
        // Default fallback
        (None, None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic_preset() {
        let toml_content = r#"
name = "Test Preset"
version = 1
selected_format = "Flac"
bit_depth = 24
sample_rate = 44100
resample_quality = 2
dither_type = "Shibata"
nyquist_transition = "Gentle"
verify_encoding = true
store_md5 = false
replaygain_mode = "Both"
copy_files_enabled = true
merge_to_single = false
        "#;

        let settings = parse_preset_toml(toml_content).expect("Failed to parse preset");

        assert_eq!(settings.format, AudioFormat::Flac);
        assert_eq!(settings.bit_depth, Some(24));
        assert_eq!(settings.sample_rate, Some(44100));
        assert_eq!(settings.dither_type, Some(DitherType::Shibata));
        assert_eq!(settings.nyquist_transition, Some(NyquistTransition::Gentle));
        assert_eq!(settings.verify_encoding, Some(true));
        assert_eq!(settings.store_md5, Some(false));
        assert_eq!(settings.replaygain_mode, Some(ReplayGainMode::Both));
    }

    #[test]
    fn test_parse_brick_wall_preset() {
        let toml_content = r#"
name = "Brick Wall Test"
version = 1
selected_format = "Flac"
bit_depth = 16
sample_rate = 44100
dither_type = "Gesemann"
nyquist_transition = "BrickWall"
        "#;

        let settings = parse_preset_toml(toml_content).expect("Failed to parse preset");

        assert_eq!(settings.dither_type, Some(DitherType::Gesemann));
        assert_eq!(
            settings.nyquist_transition,
            Some(NyquistTransition::BrickWall)
        );
    }

    #[test]
    fn test_parse_mp3_preset() {
        let toml_content = r#"
name = "MP3 Test"
version = 1
selected_format = "Mp3"
selected_quality = "320 kbps"
bit_depth = 0
        "#;

        let settings = parse_preset_toml(toml_content).expect("Failed to parse preset");

        assert_eq!(settings.format, AudioFormat::Mp3);
        assert_eq!(settings.mp3_bitrate, Some(320));
        assert_eq!(settings.mp3_mode, Some(Mp3Mode::Cbr));
        assert_eq!(settings.mp3_quality, None);
    }

    #[test]
    fn test_parse_mp3_vbr_preset() {
        let toml_content = r#"
name = "MP3 VBR Test"
version = 1
selected_format = "Mp3"
selected_quality = "V0 (VBR ~245 kbps)"
        "#;

        let settings = parse_preset_toml(toml_content).expect("Failed to parse preset");

        assert_eq!(settings.format, AudioFormat::Mp3);
        assert_eq!(settings.mp3_quality, Some(0));
        assert_eq!(settings.mp3_mode, Some(Mp3Mode::Vbr));
        assert_eq!(settings.mp3_bitrate, None);
    }
}
