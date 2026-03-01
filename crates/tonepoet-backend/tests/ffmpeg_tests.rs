//! Tests for FFmpeg command generation

use conversion_backend::*;
use std::path::Path;

#[test]
fn test_aiff_with_float_fallback() {
    // Test the specific case from the user's preset
    let settings = ConversionSettings {
        format: AudioFormat::Aiff,
        bit_depth: Some(320),  // User wants float (new convention), but AIFF doesn't support it
        sample_rate: Some(192000),
        resample_quality: Some(0), // Ultra
        ..Default::default()
    };
    
    let builder = CommandBuilder::new(Backend::FFmpeg);
    let cmd = builder.build(
        Path::new("input.flac"),
        Path::new("output.aiff"),
        &settings
    ).unwrap();
    
    // Should use 32-bit integer since AIFF doesn't support float
    assert!(cmd.arguments.contains(&"pcm_s32be".to_string()));
    
    // Should use precision=16 for LQ resampling
    let filter_arg = cmd.arguments.iter()
        .position(|arg| arg == "-af")
        .and_then(|i| cmd.arguments.get(i + 1))
        .unwrap();
    assert!(filter_arg.contains("precision=16"));
    assert!(filter_arg.contains("out_sample_rate=192000"));
}

#[test]
fn test_wav_with_float() {
    // WAV supports float, so bit_depth=320 should work
    let settings = ConversionSettings {
        format: AudioFormat::Wav,
        bit_depth: Some(320),  // Float32 (new convention)
        sample_rate: Some(96000),
        resample_quality: Some(4), // LQ
        ..Default::default()
    };
    
    let builder = CommandBuilder::new(Backend::FFmpeg);
    let cmd = builder.build(
        Path::new("input.flac"),
        Path::new("output.wav"),
        &settings
    ).unwrap();
    
    // Should use float PCM
    assert!(cmd.arguments.contains(&"pcm_f32le".to_string()));
    
    // Should use precision=32 for Ultra resampling
    let filter_arg = cmd.arguments.iter()
        .position(|arg| arg == "-af")
        .and_then(|i| cmd.arguments.get(i + 1))
        .unwrap();
    assert!(filter_arg.contains("precision=32"));
}

#[test]
fn test_flac_conversion() {
    let settings = ConversionSettings {
        format: AudioFormat::Flac,
        compression_level: Some(8),
        ..Default::default()
    };
    
    let builder = CommandBuilder::new(Backend::FFmpeg);
    let cmd = builder.build(
        Path::new("input.wav"),
        Path::new("output.flac"),
        &settings
    ).unwrap();
    
    assert!(cmd.arguments.contains(&"flac".to_string()));
    assert!(cmd.arguments.contains(&"-compression_level".to_string()));
    assert!(cmd.arguments.contains(&"8".to_string()));
}

#[test]
fn test_mp3_vbr() {
    let settings = ConversionSettings {
        format: AudioFormat::Mp3,
        mp3_mode: Some(Mp3Mode::Vbr),
        mp3_quality: Some(2),
        ..Default::default()
    };
    
    let builder = CommandBuilder::new(Backend::FFmpeg);
    let cmd = builder.build(
        Path::new("input.flac"),
        Path::new("output.mp3"),
        &settings
    ).unwrap();
    
    assert!(cmd.arguments.contains(&"libmp3lame".to_string()));
    assert!(cmd.arguments.contains(&"-q:a".to_string()));
    assert!(cmd.arguments.contains(&"2".to_string()));
}

#[test]
fn test_no_resampling_when_not_needed() {
    // sample_rate = 0 or None means keep source rate
    let settings = ConversionSettings {
        format: AudioFormat::Flac,
        sample_rate: Some(0),
        ..Default::default()
    };
    
    let builder = CommandBuilder::new(Backend::FFmpeg);
    let cmd = builder.build(
        Path::new("input.wav"),
        Path::new("output.flac"),
        &settings
    ).unwrap();
    
    // Should not have audio filter for resampling
    assert!(!cmd.arguments.contains(&"-af".to_string()));
}

#[test]
fn test_resampling_quality_mapping() {
    let quality_tests = vec![
        (0, "precision=16"),  // LQ
        (1, "precision=20"),  // MQ
        (2, "precision=24"),  // HQ
        (3, "precision=28"),  // VHQ
        (4, "precision=32"),  // Ultra
    ];
    
    for (quality, expected_precision) in quality_tests {
        let settings = ConversionSettings {
            format: AudioFormat::Wav,
            sample_rate: Some(48000),
            resample_quality: Some(quality),
            ..Default::default()
        };
        
        let builder = CommandBuilder::new(Backend::FFmpeg);
        let cmd = builder.build(
            Path::new("input.flac"),
            Path::new("output.wav"),
            &settings
        ).unwrap();
        
        let filter_arg = cmd.arguments.iter()
            .position(|arg| arg == "-af")
            .and_then(|i| cmd.arguments.get(i + 1));
            
        assert!(filter_arg.is_some(), "Missing -af for quality {}", quality);
        assert!(
            filter_arg.unwrap().contains(expected_precision),
            "Quality {} should map to {}, got: {}",
            quality,
            expected_precision,
            filter_arg.unwrap()
        );
    }
}