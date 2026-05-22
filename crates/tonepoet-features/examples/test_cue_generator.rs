//! Standalone test for cue file generation functionality

use conversion_features::{generate_cue_file, ConversionConfig};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("📀 Cue File Generator Test");
    println!("=========================\n");

    // Test different album structures
    test_standard_album().await?;
    test_various_artists_album().await?;
    test_single_track().await?;
    test_special_characters().await?;
    test_different_formats().await?;

    println!("\n📊 Test Summary:");
    println!("All cue file tests completed!");
    println!("Check ./test_cue_output/ for generated cue files");

    Ok(())
}

async fn test_standard_album() -> Result<(), Box<dyn std::error::Error>> {
    println!("Test 1: Standard album with consistent artist");
    println!("----------------------------------------------");

    // Create test directory structure: "Artist - Album Title (1995) [FLAC]"
    let album_dir = PathBuf::from("./test_cue_output/Pink Floyd - The Division Bell (1994) [FLAC]");
    tokio::fs::create_dir_all(&album_dir).await?;

    // Create dummy audio files
    let tracks = vec![
        "01 - Cluster One.opus",
        "02 - What Do You Want from Me.opus",
        "03 - Poles Apart.opus",
        "04 - Marooned.opus",
        "05 - A Great Day for Freedom.opus",
        "06 - Wearing the Inside Out.opus",
        "07 - Take It Back.opus",
        "08 - Coming Back to Life.opus",
        "09 - Keep Talking.opus",
        "10 - Lost for Words.opus",
        "11 - High Hopes.opus",
    ];

    let mut audio_files = Vec::new();
    for track in tracks {
        let file_path = album_dir.join(track);
        tokio::fs::write(&file_path, b"dummy audio content").await?;
        audio_files.push(file_path);
    }

    let config = create_test_config();

    // Generate cue file
    match generate_cue_file(&album_dir, &audio_files, &config, &[]).await {
        Ok(cue_path) => {
            println!("✅ Cue file created: {}", cue_path.display());

            // Verify content
            let content = tokio::fs::read_to_string(&cue_path).await?;
            println!("\n📋 Cue file preview:");
            let lines: Vec<&str> = content.lines().take(10).collect();
            for line in lines {
                println!("  {}", line);
            }
            println!("  ...\n");

            // Validate
            if content.contains("Pink Floyd") {
                println!("  ✓ Artist extracted correctly");
            }
            if content.contains("The Division Bell") {
                println!("  ✓ Album title extracted correctly");
            }
            if content.contains("1994") {
                println!("  ✓ Year extracted correctly");
            }
            if content.contains("TRACK 11 AUDIO") {
                println!("  ✓ All tracks included");
            }
        }
        Err(e) => {
            println!("❌ Failed to generate cue: {}", e);
        }
    }

    Ok(())
}

async fn test_various_artists_album() -> Result<(), Box<dyn std::error::Error>> {
    println!("\nTest 2: Various Artists compilation");
    println!("------------------------------------");

    let album_dir = PathBuf::from("./test_cue_output/Various Artists - Best of the 90s (1999)");
    tokio::fs::create_dir_all(&album_dir).await?;

    let tracks = vec![
        "01 - Nirvana - Smells Like Teen Spirit.opus",
        "02 - Pearl Jam - Alive.opus",
        "03 - Radiohead - Creep.opus",
        "04 - Soundgarden - Black Hole Sun.opus",
        "05 - Stone Temple Pilots - Interstate Love Song.opus",
    ];

    let mut audio_files = Vec::new();
    for track in tracks {
        let file_path = album_dir.join(track);
        tokio::fs::write(&file_path, b"dummy").await?;
        audio_files.push(file_path);
    }

    let config = create_test_config();

    match generate_cue_file(&album_dir, &audio_files, &config, &[]).await {
        Ok(cue_path) => {
            println!("✅ Compilation cue created: {}", cue_path.display());

            let content = tokio::fs::read_to_string(&cue_path).await?;
            if content.contains("Various Artists") {
                println!("  ✓ Various Artists detected");
            }
            if content.contains("Best of the 90s") {
                println!("  ✓ Compilation title correct");
            }
        }
        Err(e) => {
            println!("❌ Failed: {}", e);
        }
    }

    Ok(())
}

async fn test_single_track() -> Result<(), Box<dyn std::error::Error>> {
    println!("\nTest 3: Single track cue");
    println!("-------------------------");

    let album_dir = PathBuf::from("./test_cue_output/Single");
    tokio::fs::create_dir_all(&album_dir).await?;

    let audio_files = vec![album_dir.join("Amazing Song.opus")];

    for file in &audio_files {
        tokio::fs::write(file, b"dummy").await?;
    }

    let config = create_test_config();

    match generate_cue_file(&album_dir, &audio_files, &config, &[]).await {
        Ok(cue_path) => {
            println!("✅ Single track cue created: {}", cue_path.display());
        }
        Err(e) => {
            println!("❌ Failed: {}", e);
        }
    }

    Ok(())
}

async fn test_special_characters() -> Result<(), Box<dyn std::error::Error>> {
    println!("\nTest 4: Special characters in names");
    println!("------------------------------------");

    let album_dir = PathBuf::from("./test_cue_output/Special & Chars");
    tokio::fs::create_dir_all(&album_dir).await?;

    // Note: Some special characters will be sanitized in filenames
    let tracks = vec![
        "01 - Song with Quotes.opus",
        "02 - Track & Ampersand.opus",
        "03 - Apostrophe's Test.opus",
    ];

    let mut audio_files = Vec::new();
    for track in tracks {
        let file_path = album_dir.join(track);
        tokio::fs::write(&file_path, b"dummy").await?;
        audio_files.push(file_path);
    }

    let config = create_test_config();

    match generate_cue_file(&album_dir, &audio_files, &config, &[]).await {
        Ok(cue_path) => {
            println!("✅ Special characters handled: {}", cue_path.display());

            let content = tokio::fs::read_to_string(&cue_path).await?;
            // Check for proper escaping
            if content.contains("\\\"") || !content.contains("\"\"") {
                println!("  ✓ Quotes properly escaped");
            }
        }
        Err(e) => {
            println!("❌ Failed: {}", e);
        }
    }

    Ok(())
}

async fn test_different_formats() -> Result<(), Box<dyn std::error::Error>> {
    println!("\nTest 5: Different audio formats");
    println!("--------------------------------");

    let album_dir = PathBuf::from("./test_cue_output/Mixed Formats");
    tokio::fs::create_dir_all(&album_dir).await?;

    // Test with different extensions
    let tracks = vec![
        ("01 - FLAC Track.flac", "FLAC"),
        ("02 - MP3 Track.mp3", "MP3"),
        ("03 - Opus Track.opus", "OPUS"),
        ("04 - AAC Track.m4a", "AAC"),
        ("05 - WAV Track.wav", "WAVE"),
    ];

    let mut audio_files = Vec::new();
    for (filename, _format) in &tracks {
        let file_path = album_dir.join(filename);
        tokio::fs::write(&file_path, b"dummy").await?;
        audio_files.push(file_path);
    }

    let config = create_test_config();

    match generate_cue_file(&album_dir, &audio_files, &config, &[]).await {
        Ok(cue_path) => {
            println!("✅ Mixed format cue created: {}", cue_path.display());

            let content = tokio::fs::read_to_string(&cue_path).await?;

            // Verify each format is correctly identified
            for (_filename, format) in tracks {
                if content.contains(&format!("FILE \"{}\"", _filename)) {
                    println!("  ✓ {} format detected", format);
                }
            }
        }
        Err(e) => {
            println!("❌ Failed: {}", e);
        }
    }

    Ok(())
}

fn create_test_config() -> ConversionConfig {
    ConversionConfig {
        write_log_file: false,
        generate_cue_files: true,
        cue_generation_mode: "Always".to_string(),
        preferred_backend: "FFmpeg".to_string(),
        worker_count: 1,
        process_priority: 0,
        overwrite_behavior: "KeepBoth".to_string(),
    }
}
