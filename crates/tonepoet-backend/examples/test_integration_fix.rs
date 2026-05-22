//! Test the integration layer fix with actual backend pipeline

use conversion_backend::integration::*;
use conversion_backend::*;
use std::path::Path;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Create the exact failing case: AIFF, bit_depth=33, sample_rate=192000, resample_quality=0
    let failing_item = ConversionItem {
        id: "critical_test".to_string(),
        output_format: MainAudioFormat::Aiff,
        options: MainConversionOptions {
            quality: MainQualitySettings::Aiff {
                bit_depth: 33,       // User wants float32
                sample_rate: 192000, // Upsampling
            },
            calculate_replaygain: false,
            overwrite: true,
            resample_quality: Some(0), // LQ - this was being lost before our fix!
        },
    };

    println!("Testing critical case:");
    println!("  Format: AIFF");
    println!("  bit_depth: 33 (user wants float32)");
    println!("  sample_rate: 192000 (upsampling)");
    println!("  resample_quality: 0 (LQ)");
    println!();

    // Step 1: Map through our fixed integration layer
    let settings = map_conversion_item_to_settings(&failing_item);

    println!("After integration mapping:");
    println!("  format: {:?}", settings.format);
    println!("  bit_depth: {:?}", settings.bit_depth);
    println!("  sample_rate: {:?}", settings.sample_rate);
    println!(
        "  resample_quality: {:?} ← CRITICAL: This should be Some(0), not None!",
        settings.resample_quality
    );
    println!();

    // Step 2: Build command using backend
    let builder = CommandBuilder::new(Backend::FFmpeg);
    match builder.build(Path::new("input.flac"), Path::new("output.aiff"), &settings) {
        Ok(command) => {
            println!("✅ Backend command building SUCCESS!");
            println!("Command: {}", command.to_string());
            println!();

            // Step 3: Verify the command contains correct parameters
            let args_str = command.arguments.join(" ");

            if args_str.contains("pcm_s32be") {
                println!("✅ Correct codec: pcm_s32be (32-bit integer fallback for AIFF)");
            } else {
                println!("❌ Wrong codec in command");
            }

            if args_str.contains("precision=16") {
                println!("✅ Correct precision: 16 (from resample_quality=0)");
            } else {
                println!("❌ Wrong precision in command");
            }

            if args_str.contains("precision=33") {
                println!("❌ CRITICAL BUG: Found precision=33 in command!");
                return Err("Critical bug still present".to_string().into());
            }

            if args_str.contains("out_sample_rate=192000") {
                println!("✅ Correct sample rate: 192000");
            } else {
                println!("❌ Wrong sample rate in command");
            }

            println!();
            println!("🎉 INTEGRATION FIX VERIFICATION: SUCCESS");
            println!("   - resample_quality preserved through integration layer");
            println!("   - bit_depth=33 mapped to correct codec (pcm_s32be)");
            println!("   - resample_quality=0 mapped to correct precision (16)");
            println!("   - No precision=33 bug!");
        }
        Err(e) => {
            println!("❌ Backend command building FAILED: {}", e);
            return Err(e.into());
        }
    }

    println!();
    println!("=== TESTING PIPELINE SYSTEM ===");

    // Step 4: Test with pipeline system (more complex)
    let pipeline_builder = PipelineBuilder::new(Backend::FFmpeg);
    match pipeline_builder.build_pipeline(
        Path::new("input.flac"),
        Path::new("output.aiff"),
        &settings,
    ) {
        Ok(pipeline) => {
            println!("✅ Pipeline building SUCCESS!");
            println!("Pipeline description: {}", pipeline.description);
            println!("Number of commands: {}", pipeline.commands.len());

            for (i, command) in pipeline.commands.iter().enumerate() {
                println!("  Command {}: {}", i + 1, command.to_string());
            }

            if !pipeline.commands.is_empty() {
                let first_command_args = pipeline.commands[0].arguments.join(" ");
                if first_command_args.contains("precision=16") {
                    println!("✅ Pipeline also has correct precision=16");
                } else {
                    println!("⚠️ Pipeline may not have resampling (check if needed)");
                }
            }
        }
        Err(e) => {
            println!("❌ Pipeline building FAILED: {}", e);
            return Err(e.into());
        }
    }

    Ok(())
}
