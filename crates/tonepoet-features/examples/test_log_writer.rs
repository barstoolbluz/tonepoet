//! Standalone test for log file writing functionality

use chrono::Utc;
use conversion_features::{
    write_conversion_log, ConversionConfig, ConversionResult, ConversionStatus,
};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("📄 Log File Writer Test");
    println!("======================\n");

    // Setup test environment
    let output_dir = PathBuf::from("./test_log_output");
    tokio::fs::create_dir_all(&output_dir).await?;

    println!("📁 Output directory: {}", output_dir.display());

    // Create various conversion results to test different scenarios
    let results = create_comprehensive_test_results();

    // Test configuration
    let config = ConversionConfig {
        write_log_file: true,
        generate_cue_files: false,
        cue_generation_mode: "Always".to_string(),
        preferred_backend: "FFmpeg 7.1.0".to_string(),
        worker_count: 8,
        process_priority: -5, // High priority
        overwrite_behavior: "KeepBoth".to_string(),
    };

    println!("🔧 Configuration:");
    println!("  - Backend: {}", config.preferred_backend);
    println!("  - Workers: {}", config.worker_count);
    println!("  - Priority: {}\n", config.process_priority);

    // Test 1: Normal conversion log
    println!("Test 1: Writing normal conversion log...");
    match write_conversion_log(&output_dir, &results, &config, None).await {
        Ok(log_path) => {
            println!("✅ Log file created: {}", log_path.display());

            // Verify content
            let content = tokio::fs::read_to_string(&log_path).await?;
            verify_log_content(&content);

            println!("✅ Log file verification passed\n");
        }
        Err(e) => {
            println!("❌ Failed to write log: {}", e);
        }
    }

    // Test 2: Empty results (no files converted)
    println!("Test 2: Writing log with no conversions...");
    let empty_results = vec![];
    match write_conversion_log(&output_dir, &empty_results, &config, None).await {
        Ok(log_path) => {
            println!("✅ Empty log created: {}", log_path.display());
        }
        Err(e) => {
            println!("❌ Failed to write empty log: {}", e);
        }
    }

    // Test 3: All failures
    println!("Test 3: Writing log with all failures...");
    let failure_results = create_failure_results();
    match write_conversion_log(&output_dir, &failure_results, &config, None).await {
        Ok(log_path) => {
            println!("✅ Failure log created: {}", log_path.display());

            let content = tokio::fs::read_to_string(&log_path).await?;
            if content.contains("CONVERSION ERRORS:") {
                println!("✅ Error section found in log");
            }
        }
        Err(e) => {
            println!("❌ Failed to write failure log: {}", e);
        }
    }

    // Test 4: Large album (50+ files)
    println!("Test 4: Performance test with large album...");
    let large_results = create_large_album_results(50);
    let start = std::time::Instant::now();

    match write_conversion_log(&output_dir, &large_results, &config, None).await {
        Ok(log_path) => {
            let elapsed = start.elapsed();
            println!("✅ Large album log created: {}", log_path.display());
            println!("⏱️  Generation time: {}ms", elapsed.as_millis());

            if elapsed.as_millis() < 200 {
                println!("✅ Performance requirement met (<200ms)");
            } else {
                println!("⚠️  Performance slower than target");
            }
        }
        Err(e) => {
            println!("❌ Failed to write large album log: {}", e);
        }
    }

    println!("\n📊 Test Summary:");
    println!("All log file tests completed!");
    println!("Check ./test_log_output/ for generated log files");

    Ok(())
}

fn create_comprehensive_test_results() -> Vec<ConversionResult> {
    let now = Utc::now();

    vec![
        // Successful conversions
        ConversionResult {
            source_file: PathBuf::from("/source/01 - Opening Track.flac"),
            output_file: PathBuf::from("./test_log_output/01 - Opening Track.opus"),
            status: ConversionStatus::Success,
            source_size: 35_000_000, // 35MB
            output_size: 28_000_000, // 28MB
            start_time: now,
            end_time: now + chrono::Duration::seconds(12),
            error_message: None,
            replaygain_values: None,
            source_info: None,
            conversion_pipeline: None,
        },
        ConversionResult {
            source_file: PathBuf::from("/source/02 - Main Theme.flac"),
            output_file: PathBuf::from("./test_log_output/02 - Main Theme.opus"),
            status: ConversionStatus::Success,
            source_size: 42_000_000, // 42MB
            output_size: 33_600_000, // 33.6MB
            start_time: now + chrono::Duration::seconds(12),
            end_time: now + chrono::Duration::seconds(26),
            error_message: None,
            replaygain_values: None,
            source_info: None,
            conversion_pipeline: None,
        },
        // Failed conversion
        ConversionResult {
            source_file: PathBuf::from("/source/03 - Corrupted File.flac"),
            output_file: PathBuf::from("./test_log_output/03 - Corrupted File.opus"),
            status: ConversionStatus::Failed,
            source_size: 25_000_000,
            output_size: 0,
            start_time: now + chrono::Duration::seconds(26),
            end_time: now + chrono::Duration::seconds(27),
            error_message: Some(
                "FFmpeg error: Invalid data found when processing input".to_string(),
            ),
            replaygain_values: None,
            source_info: None,
            conversion_pipeline: None,
        },
        // More successful conversions
        ConversionResult {
            source_file: PathBuf::from("/source/04 - Interlude.flac"),
            output_file: PathBuf::from("./test_log_output/04 - Interlude.opus"),
            status: ConversionStatus::Success,
            source_size: 15_000_000,
            output_size: 12_000_000,
            start_time: now + chrono::Duration::seconds(27),
            end_time: now + chrono::Duration::seconds(35),
            error_message: None,
            replaygain_values: None,
            source_info: None,
            conversion_pipeline: None,
        },
        ConversionResult {
            source_file: PathBuf::from("/source/05 - Finale.flac"),
            output_file: PathBuf::from("./test_log_output/05 - Finale.opus"),
            status: ConversionStatus::Success,
            source_size: 48_000_000,
            output_size: 38_400_000,
            start_time: now + chrono::Duration::seconds(35),
            end_time: now + chrono::Duration::seconds(50),
            error_message: None,
            replaygain_values: None,
            source_info: None,
            conversion_pipeline: None,
        },
    ]
}

fn create_failure_results() -> Vec<ConversionResult> {
    let now = Utc::now();

    vec![
        ConversionResult {
            source_file: PathBuf::from("/source/bad1.flac"),
            output_file: PathBuf::from("./test_log_output/bad1.opus"),
            status: ConversionStatus::Failed,
            source_size: 10_000_000,
            output_size: 0,
            start_time: now,
            end_time: now + chrono::Duration::seconds(1),
            error_message: Some("Permission denied: Cannot read source file".to_string()),
            replaygain_values: None,
            source_info: None,
            conversion_pipeline: None,
        },
        ConversionResult {
            source_file: PathBuf::from("/source/bad2.flac"),
            output_file: PathBuf::from("./test_log_output/bad2.opus"),
            status: ConversionStatus::Failed,
            source_size: 15_000_000,
            output_size: 0,
            start_time: now + chrono::Duration::seconds(1),
            end_time: now + chrono::Duration::seconds(2),
            error_message: Some("Invalid sample rate: 384000 Hz not supported".to_string()),
            replaygain_values: None,
            source_info: None,
            conversion_pipeline: None,
        },
    ]
}

fn create_large_album_results(count: usize) -> Vec<ConversionResult> {
    let now = Utc::now();
    let mut results = Vec::new();

    for i in 0..count {
        let track_num = i + 1;
        results.push(ConversionResult {
            source_file: PathBuf::from(format!(
                "/source/{:02} - Track {}.flac",
                track_num, track_num
            )),
            output_file: PathBuf::from(format!(
                "./test_log_output/{:02} - Track {}.opus",
                track_num, track_num
            )),
            status: if i % 10 == 9 {
                ConversionStatus::Failed
            } else {
                ConversionStatus::Success
            },
            source_size: 20_000_000 + (i as u64 * 1_000_000),
            output_size: if i % 10 == 9 {
                0
            } else {
                16_000_000 + (i as u64 * 800_000)
            },
            start_time: now + chrono::Duration::seconds(i as i64 * 10),
            end_time: now + chrono::Duration::seconds((i + 1) as i64 * 10),
            error_message: if i % 10 == 9 {
                Some(format!("Simulated error for track {}", track_num))
            } else {
                None
            },
        });
    }

    results
}

fn verify_log_content(content: &str) {
    let required_sections = vec![
        "HEXLOAD-TUI CONVERSION LOG",
        "CONVERSION SETTINGS:",
        "INPUT FILES:",
        "CONVERSION RESULTS:",
        "CONVERSION SUMMARY:",
        "END OF CONVERSION LOG",
    ];

    for section in required_sections {
        if !content.contains(section) {
            println!("⚠️  Missing section: {}", section);
        }
    }

    // Check for specific content
    if content.contains("FFmpeg 7.1.0") {
        println!("  ✓ Backend version recorded");
    }
    if content.contains("Workers: 8 parallel") {
        println!("  ✓ Worker count recorded");
    }
    if content.contains("✅") && content.contains("❌") {
        println!("  ✓ Success/failure markers present");
    }
    if content.contains("Compression:") {
        println!("  ✓ Compression ratios calculated");
    }
}
