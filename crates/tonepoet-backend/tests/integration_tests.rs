//! Integration layer tests

use conversion_backend::integration::*;
use conversion_backend::types::*;

#[test]
fn test_resample_quality_preservation() {
    // Test that resample_quality is no longer lost in mapping
    let item = ConversionItem {
        id: "test".to_string(),
        output_format: MainAudioFormat::Aiff,
        options: MainConversionOptions {
            quality: MainQualitySettings::Aiff { bit_depth: 33, sample_rate: 192000 },
            calculate_replaygain: false,
            overwrite: true,
            resample_quality: Some(0), // Ultra quality - this was being lost!
        },
    };
    
    let settings = map_conversion_item_to_settings(&item);
    
    // CRITICAL: Verify resample_quality is preserved
    assert_eq!(settings.resample_quality, Some(0));
    
    // Verify other mappings still work
    assert_eq!(settings.format, AudioFormat::Aiff);
    assert_eq!(settings.bit_depth, Some(33));
    assert_eq!(settings.sample_rate, Some(192000));
}

#[test]
fn test_aac_profile_preservation() {
    // Test that AAC profile is no longer lost in mapping
    let item = ConversionItem {
        id: "test".to_string(),
        output_format: MainAudioFormat::Aac,
        options: MainConversionOptions {
            quality: MainQualitySettings::Aac { bitrate: 256, profile: MainAacProfile::He },
            calculate_replaygain: false,
            overwrite: true,
            resample_quality: Some(2), // HQ
        },
    };
    
    let settings = map_conversion_item_to_settings(&item);
    
    // CRITICAL: Verify AAC profile is preserved  
    assert_eq!(settings.aac_profile, Some(AacProfile::HeAac));
    
    // Verify resample quality is preserved
    assert_eq!(settings.resample_quality, Some(2));
}

#[test]
fn test_wavpack_mapping_improvement() {
    // Test that WavPack settings are now mapped instead of ignored
    let item = ConversionItem {
        id: "test".to_string(),
        output_format: MainAudioFormat::WavPack,
        options: MainConversionOptions {
            quality: MainQualitySettings::WavPack { 
                compression_mode: MainWavPackMode::VeryHigh, 
                hybrid_mode: true, 
                correction_file: false 
            },
            calculate_replaygain: false,
            overwrite: true,
            resample_quality: Some(3), // MQ
        },
    };
    
    let settings = map_conversion_item_to_settings(&item);
    
    // IMPROVED: WavPack compression mode now mapped
    assert_eq!(settings.compression_level, Some(6)); // VeryHigh -> 6
    
    // Verify resample quality preserved
    assert_eq!(settings.resample_quality, Some(3));
    
    // TODO: hybrid_mode and correction_file need backend support
}

#[test]
fn test_opus_complexity_todo() {
    // Test demonstrates Opus complexity field that needs mapping
    let item = ConversionItem {
        id: "test".to_string(),
        output_format: MainAudioFormat::Opus,
        options: MainConversionOptions {
            quality: MainQualitySettings::Opus { bitrate: 128, complexity: 8 },
            calculate_replaygain: false,
            overwrite: true,
            resample_quality: Some(1), // VHQ
        },
    };
    
    let settings = map_conversion_item_to_settings(&item);
    
    // Verify basic mapping works
    assert_eq!(settings.format, AudioFormat::Opus);
    assert_eq!(settings.mp3_bitrate, Some(128)); // Reused for Opus
    assert_eq!(settings.resample_quality, Some(1));
    
    // TODO: Need to add opus_complexity field to backend ConversionSettings
    // Currently complexity=8 is lost in mapping
}