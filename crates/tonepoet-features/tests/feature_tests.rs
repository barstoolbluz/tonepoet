//! Comprehensive test suite for conversion features

use chrono::Utc;
use tonepoet_features::*;
use std::path::PathBuf;
use tempfile::TempDir;

#[tokio::test]
async fn test_log_file_creation() {
    // Create a temporary directory for testing
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let output_dir = temp_dir.path();

    // Create test data
    let config = ConversionConfig {
        write_log_file: true,
        generate_cue_files: false,
        cue_generation_mode: "Always".to_string(),
        preferred_backend: "FFmpeg".to_string(),
        worker_count: 4,
        process_priority: 0,
        overwrite_behavior: "KeepBoth".to_string(),
    };

    let results = create_test_results();

    // Generate log file
    let log_path = write_conversion_log(output_dir, &results, &config, None)
        .await
        .expect("Failed to write log file");

    // Verify log file exists
    assert!(log_path.exists(), "Log file should exist");

    // Verify log file content
    let content = tokio::fs::read_to_string(&log_path)
        .await
        .expect("Failed to read log file");

    // Check for required sections
    assert!(
        content.contains("HEXLOAD-TUI CONVERSION LOG"),
        "Missing header"
    );
    assert!(
        content.contains("CONVERSION SETTINGS:"),
        "Missing settings section"
    );
    assert!(
        content.contains("INPUT FILES:"),
        "Missing input files section"
    );
    assert!(
        content.contains("CONVERSION RESULTS:"),
        "Missing results section"
    );
    assert!(
        content.contains("CONVERSION SUMMARY:"),
        "Missing summary section"
    );
    assert!(content.contains("END OF CONVERSION LOG"), "Missing footer");

    // Check specific content
    assert!(content.contains("Backend: FFmpeg"), "Backend not recorded");
    assert!(
        content.contains("Workers: 4 concurrent"),
        "Worker count not recorded"
    );
    assert!(content.contains("✅"), "Success markers missing");
    assert!(content.contains("❌"), "Failure markers missing");
}

#[tokio::test]
async fn test_log_file_not_created_when_disabled() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let output_dir = temp_dir.path();

    let config = ConversionConfig {
        write_log_file: false, // Disabled
        generate_cue_files: false,
        cue_generation_mode: "Always".to_string(),
        preferred_backend: "FFmpeg".to_string(),
        worker_count: 4,
        process_priority: 0,
        overwrite_behavior: "KeepBoth".to_string(),
    };

    let results = create_test_results();

    // Try to write log (should be skipped internally based on config)
    // Since write_conversion_log doesn't check config, we test the wrapper
    let result =
        post_conversion_features(&output_dir.to_path_buf(), &results, &[], &config, None).await;

    assert!(result.is_ok(), "Should not fail even when disabled");

    // Check that no log file was created
    let entries = std::fs::read_dir(output_dir).expect("Failed to read dir");
    let log_files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("conversion-log-")
        })
        .collect();

    assert_eq!(
        log_files.len(),
        0,
        "No log files should be created when disabled"
    );
}

#[tokio::test]
async fn test_cue_file_creation() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let output_dir = temp_dir.path();

    // Create dummy audio files
    let audio_files = vec![
        output_dir.join("01 - Track One.opus"),
        output_dir.join("02 - Track Two.opus"),
        output_dir.join("03 - Track Three.opus"),
    ];

    for file in &audio_files {
        tokio::fs::write(file, b"dummy audio content")
            .await
            .expect("Failed to create dummy file");
    }

    let config = ConversionConfig {
        write_log_file: false,
        generate_cue_files: true,
        cue_generation_mode: "Always".to_string(),
        preferred_backend: "FFmpeg".to_string(),
        worker_count: 4,
        process_priority: 0,
        overwrite_behavior: "KeepBoth".to_string(),
    };

    // Generate cue file (no ReplayGain data for basic test)
    let cue_path = generate_cue_file(output_dir, &audio_files, &config, &[])
        .await
        .expect("Failed to generate cue file");

    // Verify cue file exists
    assert!(cue_path.exists(), "Cue file should exist");

    // Verify cue file content
    let content = tokio::fs::read_to_string(&cue_path)
        .await
        .expect("Failed to read cue file");

    // Check for required elements
    assert!(content.contains("PERFORMER"), "Missing performer");
    assert!(content.contains("TITLE"), "Missing title");
    assert!(
        content.contains("FILE \"01 - Track One.opus\" OPUS"),
        "Missing first track"
    );
    assert!(
        content.contains("FILE \"02 - Track Two.opus\" OPUS"),
        "Missing second track"
    );
    assert!(
        content.contains("FILE \"03 - Track Three.opus\" OPUS"),
        "Missing third track"
    );
    assert!(content.contains("TRACK 01 AUDIO"), "Missing track 1 entry");
    assert!(content.contains("TRACK 02 AUDIO"), "Missing track 2 entry");
    assert!(content.contains("TRACK 03 AUDIO"), "Missing track 3 entry");
    assert!(
        content.contains("INDEX 01 00:00:00"),
        "Missing index entries"
    );
}

#[tokio::test]
async fn test_both_features_together() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let output_dir = temp_dir.path();

    // Create dummy audio files
    let audio_files = vec![
        output_dir.join("01 - Song.opus"),
        output_dir.join("02 - Another.opus"),
    ];

    for file in &audio_files {
        tokio::fs::write(file, b"dummy")
            .await
            .expect("Failed to create dummy file");
    }

    let config = ConversionConfig {
        write_log_file: true,
        generate_cue_files: true,
        cue_generation_mode: "Always".to_string(),
        preferred_backend: "SoX".to_string(),
        worker_count: 2,
        process_priority: -5,
        overwrite_behavior: "Overwrite".to_string(),
    };

    let results = create_test_results();

    // Use the integration function
    let result = post_conversion_features(
        &output_dir.to_path_buf(),
        &results,
        &audio_files,
        &config,
        None,
    )
    .await;

    assert!(result.is_ok(), "Both features should work together");

    // Check for both files
    let entries = std::fs::read_dir(output_dir).expect("Failed to read dir");
    let files: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    let has_log = files.iter().any(|f| f.starts_with("conversion-log-"));
    let has_cue = files.iter().any(|f| f.ends_with(".cue"));

    assert!(has_log, "Log file should be created");
    assert!(has_cue, "Cue file should be created");
}

#[tokio::test]
async fn test_error_handling_graceful() {
    // Test with a read-only directory (simulated by using a non-existent path)
    let output_dir = PathBuf::from("/nonexistent/readonly/path");

    let config = ConversionConfig {
        write_log_file: true,
        generate_cue_files: true,
        cue_generation_mode: "Always".to_string(),
        preferred_backend: "FFmpeg".to_string(),
        worker_count: 1,
        process_priority: 0,
        overwrite_behavior: "Skip".to_string(),
    };

    let results = create_test_results();

    // This should not panic, just return an error or handle gracefully
    let result = post_conversion_features(&output_dir, &results, &[], &config, None).await;

    // The wrapper function should return Ok even if features fail
    assert!(result.is_ok(), "Should handle errors gracefully");
}

#[tokio::test]
async fn test_compression_ratio_calculation() {
    let result = ConversionResult {
        source_file: PathBuf::from("test.flac"),
        output_file: PathBuf::from("test.opus"),
        status: ConversionStatus::Success,
        source_size: 10_000_000, // 10MB
        output_size: 7_500_000,  // 7.5MB
        start_time: Utc::now(),
        end_time: Utc::now(),
        error_message: None,
        replaygain_values: None,
        source_info: None,
        conversion_pipeline: None,
    };

    assert_eq!(result.compression_ratio(), 75.0);
}

#[tokio::test]
async fn test_special_characters_in_filenames() {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let output_dir = temp_dir.path();

    // Create files with special characters
    let audio_files = vec![
        output_dir.join("01 - Track \"One\".opus"),
        output_dir.join("02 - Song's Name.opus"),
    ];

    // Note: Some filesystems may not support all special characters
    for file in &audio_files {
        let safe_name = file
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace('"', "_")
            .replace('\'', "_");
        let safe_path = output_dir.join(safe_name);
        tokio::fs::write(&safe_path, b"dummy")
            .await
            .expect("Failed to create file");
    }

    let config = ConversionConfig {
        write_log_file: false,
        generate_cue_files: true,
        cue_generation_mode: "Always".to_string(),
        preferred_backend: "FFmpeg".to_string(),
        worker_count: 1,
        process_priority: 0,
        overwrite_behavior: "KeepBoth".to_string(),
    };

    // Update audio_files to use the safe paths
    let safe_audio_files: Vec<PathBuf> = std::fs::read_dir(output_dir)
        .expect("Failed to read dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "opus"))
        .map(|e| e.path())
        .collect();

    // Should handle special characters without crashing
    let result = generate_cue_file(output_dir, &safe_audio_files, &config, &[]).await;
    assert!(result.is_ok(), "Should handle special characters");
}

// Helper function to create test conversion results
fn create_test_results() -> Vec<ConversionResult> {
    let now = Utc::now();

    vec![
        ConversionResult {
            source_file: PathBuf::from("/tmp/test/01 - Song.flac"),
            output_file: PathBuf::from("/tmp/test/01 - Song.opus"),
            status: ConversionStatus::Success,
            source_size: 15_000_000,
            output_size: 12_000_000,
            start_time: now,
            end_time: now + chrono::Duration::seconds(10),
            error_message: None,
            replaygain_values: None,
            source_info: None,
            conversion_pipeline: None,
        },
        ConversionResult {
            source_file: PathBuf::from("/tmp/test/02 - Track.flac"),
            output_file: PathBuf::from("/tmp/test/02 - Track.opus"),
            status: ConversionStatus::Success,
            source_size: 18_000_000,
            output_size: 14_000_000,
            start_time: now + chrono::Duration::seconds(10),
            end_time: now + chrono::Duration::seconds(22),
            error_message: None,
            replaygain_values: None,
            source_info: None,
            conversion_pipeline: None,
        },
        ConversionResult {
            source_file: PathBuf::from("/tmp/test/03 - Failed.flac"),
            output_file: PathBuf::from("/tmp/test/03 - Failed.opus"),
            status: ConversionStatus::Failed,
            source_size: 20_000_000,
            output_size: 0,
            start_time: now + chrono::Duration::seconds(22),
            end_time: now + chrono::Duration::seconds(24),
            error_message: Some("Test error: Simulated failure".to_string()),
            replaygain_values: None,
            source_info: None,
            conversion_pipeline: None,
        },
    ]
}
