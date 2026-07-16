//! Integration demonstration - Shows how to add log and cue file features

use chrono::Utc;
use tonepoet_features::{
    generate_cue_file, write_conversion_log, ConversionConfig, ConversionResult, ConversionStatus,
};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🎯 Conversion Features Integration Demo");
    println!("=====================================\n");

    // Simulate conversion configuration (from Options Wizard)
    let config = ConversionConfig {
        write_log_file: true,
        generate_cue_files: true,
        cue_generation_mode: "Always".to_string(),
        preferred_backend: "FFmpeg".to_string(),
        worker_count: 8,
        process_priority: 0,
        overwrite_behavior: "KeepBoth".to_string(),
    };

    // Simulate conversion results
    let results = create_test_conversion_results();

    // Simulate output directory
    let output_dir = PathBuf::from("./test_output");
    tokio::fs::create_dir_all(&output_dir).await?;

    // Create some dummy audio files for testing
    create_test_audio_files(&output_dir).await?;

    println!("📄 Testing Log File Generation...");

    // Test log file writing
    match write_conversion_log(&output_dir, &results, &config, None).await {
        Ok(log_path) => {
            println!("✅ Log file created: {}", log_path.display());

            // Show log content
            let log_content = tokio::fs::read_to_string(&log_path).await?;
            println!("\n📋 Log File Content Preview:");
            println!("{}", &log_content[..500.min(log_content.len())]);
            if log_content.len() > 500 {
                println!("... [truncated] ...");
            }
        }
        Err(e) => {
            println!("❌ Log file generation failed: {}", e);
        }
    }

    println!("\n📀 Testing Cue File Generation...");

    // Get audio files for cue generation
    let audio_files = get_audio_files(&output_dir).await?;

    // Test cue file generation
    match generate_cue_file(&output_dir, &audio_files, &config, &[]).await {
        Ok(cue_path) => {
            println!("✅ Cue file created: {}", cue_path.display());

            // Show cue content
            let cue_content = tokio::fs::read_to_string(&cue_path).await?;
            println!("\n📀 Cue File Content:");
            println!("{}", cue_content);
        }
        Err(e) => {
            println!("❌ Cue file generation failed: {}", e);
        }
    }

    println!("\n🎯 Integration Demo Complete");
    println!("Check ./test_output/ directory for generated files");

    Ok(())
}

fn create_test_conversion_results() -> Vec<ConversionResult> {
    let now = Utc::now();

    vec![
        ConversionResult {
            source_file: PathBuf::from("01 - Beautiful Things.flac"),
            output_file: PathBuf::from("01 - Beautiful Things.opus"),
            status: ConversionStatus::Success,
            source_size: 25_000_000, // 25MB
            output_size: 20_000_000, // 20MB
            start_time: now,
            end_time: now + chrono::Duration::seconds(15),
            error_message: None,
            replaygain_values: None,
            source_info: None,
            conversion_pipeline: None,
        },
        ConversionResult {
            source_file: PathBuf::from("02 - Summer Stone.flac"),
            output_file: PathBuf::from("02 - Summer Stone.opus"),
            status: ConversionStatus::Success,
            source_size: 28_000_000, // 28MB
            output_size: 22_000_000, // 22MB
            start_time: now + chrono::Duration::seconds(15),
            end_time: now + chrono::Duration::seconds(32),
            error_message: None,
            replaygain_values: None,
            source_info: None,
            conversion_pipeline: None,
        },
        ConversionResult {
            source_file: PathBuf::from("03 - Track Name.flac"),
            output_file: PathBuf::from("03 - Track Name.opus"),
            status: ConversionStatus::Failed,
            source_size: 22_000_000,
            output_size: 0,
            start_time: now + chrono::Duration::seconds(32),
            end_time: now + chrono::Duration::seconds(34),
            error_message: Some(
                "Backend error: Invalid sample rate (192000 Hz not supported)".to_string(),
            ),
            replaygain_values: None,
            source_info: None,
            conversion_pipeline: None,
        },
    ]
}

async fn create_test_audio_files(output_dir: &PathBuf) -> Result<(), std::io::Error> {
    // Create dummy audio files for testing
    tokio::fs::write(
        output_dir.join("01 - Beautiful Things.opus"),
        b"dummy opus content",
    )
    .await?;
    tokio::fs::write(
        output_dir.join("02 - Summer Stone.opus"),
        b"dummy opus content",
    )
    .await?;

    // Create auxiliary files
    tokio::fs::write(output_dir.join("lineage.txt"), b"lineage information").await?;
    tokio::fs::write(output_dir.join("orangecd.ini"), b"[config]\nversion=1").await?;

    Ok(())
}

async fn get_audio_files(output_dir: &PathBuf) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut audio_files = Vec::new();

    let mut entries = tokio::fs::read_dir(output_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if matches!(ext, "opus" | "mp3" | "flac" | "aac" | "wav" | "aiff") {
                    audio_files.push(path);
                }
            }
        }
    }

    audio_files.sort();
    Ok(audio_files)
}

/*
INTEGRATION TESTING CHECKLIST:

Run this demo and verify:

✅ LOG FILE TESTING:
- [ ] Log file created in ./test_output/
- [ ] Filename format: conversion-log-YYYYMMDD-HHMMSS.txt
- [ ] Contains all required sections (header, settings, results, summary)
- [ ] Shows conversion statistics (success/fail counts, compression ratios)
- [ ] Error details for failed conversions
- [ ] Auxiliary files section

✅ CUE FILE TESTING:
- [ ] Cue file created in ./test_output/
- [ ] Filename format: {album}.cue
- [ ] Valid cue format with PERFORMER, TITLE, FILE entries
- [ ] Correct track numbers and filenames
- [ ] Proper audio format identification (OPUS)
- [ ] Escaped special characters

✅ INTEGRATION TESTING:
- [ ] Both files created when both features enabled
- [ ] No interference between log and cue generation
- [ ] Performance reasonable (< 1 second total for demo)
- [ ] Error handling graceful (no crashes)

Run with: cargo run --example integration_demo
*/
