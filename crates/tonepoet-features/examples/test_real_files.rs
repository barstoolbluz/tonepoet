//! Test with real audio files

use chrono::Utc;
use conversion_features::{
    generate_cue_file, post_conversion_features, write_conversion_log, ConversionConfig,
    ConversionResult, ConversionStatus,
};
use std::path::{Path, PathBuf};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get directory from command line
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <audio_directory>", args[0]);
        std::process::exit(1);
    }

    let audio_dir = PathBuf::from(&args[1]);
    if !audio_dir.exists() {
        eprintln!("Directory does not exist: {}", audio_dir.display());
        std::process::exit(1);
    }

    println!("🎵 Testing with Real Audio Files");
    println!("================================\n");
    println!("📁 Source directory: {}", audio_dir.display());

    // Find audio files
    let audio_files = find_audio_files(&audio_dir).await?;

    if audio_files.is_empty() {
        println!("❌ No audio files found in directory");
        return Ok(());
    }

    println!("📊 Found {} audio files:", audio_files.len());
    for (i, file) in audio_files.iter().enumerate().take(10) {
        println!(
            "  {}. {}",
            i + 1,
            file.file_name().unwrap().to_string_lossy()
        );
    }
    if audio_files.len() > 10 {
        println!("  ... and {} more", audio_files.len() - 10);
    }

    // Create output directory
    let output_dir = PathBuf::from("./real_files_test_output");
    tokio::fs::create_dir_all(&output_dir).await?;

    // Simulate conversion results based on real files
    println!("\n📝 Generating conversion results from real files...");
    let conversion_results = create_results_from_files(&audio_files).await?;

    // Configuration
    let config = ConversionConfig {
        write_log_file: true,
        generate_cue_files: true,
        cue_generation_mode: "Always".to_string(),
        preferred_backend: "FFmpeg".to_string(),
        worker_count: 8,
        process_priority: 0,
        overwrite_behavior: "KeepBoth".to_string(),
    };

    // Test 1: Generate log file
    println!("\n📄 Generating log file...");
    match write_conversion_log(&output_dir, &conversion_results, &config).await {
        Ok(log_path) => {
            println!("✅ Log file created: {}", log_path.display());

            // Show preview
            let content = tokio::fs::read_to_string(&log_path).await?;
            let lines: Vec<&str> = content.lines().take(30).collect();
            println!("\n📋 Log preview:");
            for line in lines {
                println!("  {}", line);
            }
            println!("  ...\n");
        }
        Err(e) => {
            println!("❌ Failed to generate log: {}", e);
        }
    }

    // Test 2: Generate cue file
    println!("📀 Generating cue file...");

    // Copy or simulate audio files in output directory for cue generation
    let output_files = simulate_output_files(&audio_files, &output_dir).await?;

    match generate_cue_file(&output_dir, &output_files, &config, &[]).await {
        Ok(cue_path) => {
            println!("✅ Cue file created: {}", cue_path.display());

            // Show content
            let content = tokio::fs::read_to_string(&cue_path).await?;
            println!("\n📀 Cue file content:");
            println!("{}", content);
        }
        Err(e) => {
            println!("❌ Failed to generate cue: {}", e);
        }
    }

    // Test 3: Use integration function
    println!("\n🔄 Testing integrated post_conversion_features...");
    let result =
        post_conversion_features(&output_dir, &conversion_results, &output_files, &config).await;

    match result {
        Ok(_) => println!("✅ Integration function successful"),
        Err(e) => println!("❌ Integration function failed: {}", e),
    }

    println!("\n✅ Testing complete!");
    println!("📁 Check ./real_files_test_output/ for generated files");

    Ok(())
}

async fn find_audio_files(dir: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut audio_files = Vec::new();
    let mut entries = tokio::fs::read_dir(dir).await?;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let ext_lower = ext.to_lowercase();
                if matches!(
                    ext_lower.as_str(),
                    "flac" | "mp3" | "opus" | "m4a" | "aac" | "wav" | "aiff" | "ogg" | "wv"
                ) {
                    audio_files.push(path);
                }
            }
        }
    }

    audio_files.sort();
    Ok(audio_files)
}

async fn create_results_from_files(
    files: &[PathBuf],
) -> Result<Vec<ConversionResult>, std::io::Error> {
    let mut results = Vec::new();
    let now = Utc::now();

    for (i, file) in files.iter().enumerate() {
        let metadata = tokio::fs::metadata(file).await?;
        let file_size = metadata.len();

        // Simulate conversion (90% success rate)
        let is_success = i % 10 != 9;

        // Determine output format (simulate conversion to Opus)
        let output_file = if let Some(stem) = file.file_stem() {
            PathBuf::from(format!("{}.opus", stem.to_string_lossy()))
        } else {
            PathBuf::from(format!("converted_{}.opus", i))
        };

        results.push(ConversionResult {
            source_file: file.clone(),
            output_file: output_file.clone(),
            status: if is_success {
                ConversionStatus::Success
            } else {
                ConversionStatus::Failed
            },
            source_size: file_size,
            output_size: if is_success {
                (file_size as f64 * 0.8) as u64
            } else {
                0
            },
            start_time: now + chrono::Duration::seconds(i as i64 * 10),
            end_time: now + chrono::Duration::seconds((i + 1) as i64 * 10),
            error_message: if !is_success {
                Some(format!("Simulated error for testing on file {}", i + 1))
            } else {
                None
            },
        });
    }

    Ok(results)
}

async fn simulate_output_files(
    input_files: &[PathBuf],
    output_dir: &Path,
) -> Result<Vec<PathBuf>, std::io::Error> {
    let mut output_files = Vec::new();

    for file in input_files {
        // Get just the filename and change extension to .opus
        if let Some(filename) = file.file_name() {
            let filename_str = filename.to_string_lossy();
            let output_name = if let Some(dot_pos) = filename_str.rfind('.') {
                format!("{}.opus", &filename_str[..dot_pos])
            } else {
                format!("{}.opus", filename_str)
            };

            let output_path = output_dir.join(output_name);

            // Create a dummy file (in real integration, these would be actual converted files)
            tokio::fs::write(&output_path, b"dummy audio content for testing").await?;
            output_files.push(output_path);
        }
    }

    Ok(output_files)
}
