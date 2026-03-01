//! Multi-tool pipeline system for complex audio conversions
//!
//! This module handles cases where a single tool can't fulfill all requirements,
//! requiring intelligent switching between FFmpeg, SoX, SSRC, and format-specific tools.

use crate::types::*;
use crate::{Result, ConversionError, Backend};
use std::collections::HashMap;
use std::path::Path;

/// Represents a complete conversion pipeline with multiple stages
#[derive(Debug, Clone)]
pub struct ConversionPipeline {
    /// Ordered list of commands to execute
    pub commands: Vec<ConversionCommand>,
    
    /// Intermediate temporary files that need cleanup
    pub temp_files: Vec<String>,
    
    /// Metadata preservation strategy
    pub metadata_strategy: MetadataStrategy,
    
    /// Expected total duration for progress reporting
    pub expected_duration: Option<std::time::Duration>,
    
    /// Description of the pipeline for logging
    pub description: String,
}

/// Strategy for preserving metadata through multi-stage pipelines
#[derive(Debug, Clone)]
pub enum MetadataStrategy {
    /// Use format-specific tools (metaflac, wvtag, opustags)
    FormatSpecific {
        export_command: ConversionCommand,
        import_command: ConversionCommand,
        temp_file: String,
    },
    
    /// Use FFmpeg JSON extraction/reapplication
    FFmpegJson {
        extract_command: ConversionCommand,
        apply_command: ConversionCommand, 
        temp_file: String,
    },
    
    /// No metadata preservation needed (same format, lossless)
    None,
}

/// Analyzes settings and builds appropriate pipeline
pub struct PipelineBuilder {
    /// User's preferred backend (FFmpeg or SoX)
    preferred_backend: Backend,
}

impl PipelineBuilder {
    pub fn new(preferred_backend: Backend) -> Self {
        Self { preferred_backend }
    }
    
    /// Build complete pipeline from settings
    pub fn build_pipeline(
        &self,
        input: &Path,
        output: &Path,
        settings: &ConversionSettings,
    ) -> Result<ConversionPipeline> {
        // Analyze what operations are needed
        let operations = self.analyze_required_operations(settings)?;
        
        // Check if preferred backend can handle everything
        if self.can_single_backend_handle(&operations, settings) {
            return self.build_single_backend_pipeline(input, output, settings, &operations);
        }
        
        // Build multi-tool pipeline
        self.build_multi_tool_pipeline(input, output, settings, &operations)
    }
    
    /// Determine what operations are required
    fn analyze_required_operations(&self, settings: &ConversionSettings) -> Result<RequiredOperations> {
        let mut ops = RequiredOperations::default();
        
        // Check if format conversion needed
        ops.needs_format_conversion = true; // Always assume we're converting format
        
        // Check if resampling needed
        ops.needs_resampling = crate::validation::needs_resampling(settings);
        if ops.needs_resampling {
            ops.resample_type = self.determine_resample_type(settings);
        }
        
        // Check if dithering needed
        ops.needs_dithering = crate::validation::needs_dithering(settings);
        if ops.needs_dithering {
            ops.dither_complexity = self.determine_dither_complexity(settings.dither_type);
        }
        
        // Check if post-processing needed
        ops.needs_replaygain = settings.replaygain_mode.is_some();
        
        Ok(ops)
    }
    
    /// Determine type of resampling required
    fn determine_resample_type(&self, settings: &ConversionSettings) -> ResampleType {
        match settings.nyquist_transition {
            Some(NyquistTransition::BrickWall) => ResampleType::BrickWall,
            _ => ResampleType::Standard,
        }
    }
    
    /// Determine complexity of dithering
    fn determine_dither_complexity(&self, dither_type: Option<DitherType>) -> DitherComplexity {
        match dither_type {
            Some(DitherType::Gesemann) => DitherComplexity::GesemmannOnly, // Only SoX supports
            Some(DitherType::Shibata) | Some(DitherType::LowShibata) | Some(DitherType::HighShibata) => {
                DitherComplexity::Advanced // SoX or SSRC
            }
            Some(DitherType::Tpdf) => DitherComplexity::Basic, // Most tools support
            _ => DitherComplexity::None,
        }
    }

    /// Check if we need a processing step (resample/bit-depth/dither) before final encode
    fn needs_processing_step(&self, settings: &ConversionSettings, operations: &RequiredOperations) -> bool {
        // Skip if brick wall (SSRC handles everything)
        if operations.resample_type == ResampleType::BrickWall {
            return false;
        }

        let is_lossy = matches!(settings.format,
            AudioFormat::Opus | AudioFormat::Mp3 | AudioFormat::Aac);

        // For lossy formats, only process for resampling (skip bit reduction/dithering)
        // Lossy codec handles quantization internally
        if is_lossy {
            return operations.needs_resampling &&
                   operations.resample_type == ResampleType::Standard;
        }

        // For lossless formats:
        // If Gesemann, only need processing for resampling (Gesemann step handles dithering)
        if operations.dither_complexity == DitherComplexity::GesemmannOnly {
            return operations.needs_resampling &&
                   operations.resample_type == ResampleType::Standard;
        }

        // For other cases, need processing if bit reduction OR resample OR dither
        let needs_bit_reduction = crate::validation::needs_bit_depth_reduction(settings);
        let needs_standard_resample = operations.needs_resampling &&
                                       operations.resample_type == ResampleType::Standard;

        needs_bit_reduction || needs_standard_resample || operations.needs_dithering
    }

    /// Check if single backend can handle all operations
    fn can_single_backend_handle(&self, ops: &RequiredOperations, settings: &ConversionSettings) -> bool {
        // WavPack always requires multi-tool pipeline (needs WAV input, must decode first)
        if settings.format == AudioFormat::WavPack {
            return false;
        }

        // FLAC with any processing requires multi-tool (native encoder can't process)
        if settings.format == AudioFormat::Flac {
            if ops.needs_resampling || ops.needs_dithering || crate::validation::needs_bit_depth_reduction(settings) {
                return false; // Force multi-tool for processing
            }
            // Simple FLAC transcode: single backend can handle (FLAC encoder accepts FLAC input)
            return true;
        }

        // Lossy formats with dithering or bit reduction should use multi-tool
        // (single-backend would incorrectly apply dithering/reduction before lossy encoding)
        let is_lossy = matches!(settings.format, AudioFormat::Opus | AudioFormat::Mp3 | AudioFormat::Aac);
        if is_lossy && (ops.needs_dithering || crate::validation::needs_bit_depth_reduction(settings)) {
            return false; // Force multi-tool to properly skip dithering/reduction
        }

        match self.preferred_backend {
            Backend::FFmpeg => {
                // From LODESTAR Dither Capability Matrix:
                // FFmpeg can't do: TPDF, Shibata variants, Gesemann (only "None")
                // From Tool Capabilities: FFmpeg has "Limited" dither support
                let can_dither = match ops.dither_complexity {
                    DitherComplexity::None => true,
                    _ => false, // FFmpeg can't do any advanced dithering
                };

                // FFmpeg can't do brick wall (requires SSRC)
                let can_resample = ops.resample_type != ResampleType::BrickWall;

                can_dither && can_resample
            }
            Backend::Sox => {
                // From LODESTAR matrices:
                // SoX can do: All dither types EXCEPT it can't do Brick Wall resampling
                // SoX supports all dither types including Gesemann
                let can_dither = true; // SoX supports all dither types

                // SoX can't do brick wall (requires SSRC)
                let can_resample = ops.resample_type != ResampleType::BrickWall;

                can_dither && can_resample
            }
        }
    }
    
    /// Build pipeline using single backend
    fn build_single_backend_pipeline(
        &self,
        input: &Path,
        output: &Path,
        settings: &ConversionSettings,
        _operations: &RequiredOperations,
    ) -> Result<ConversionPipeline> {
        // Use format-specific encoders for FLAC and WavPack, otherwise use preferred backend
        let command = match settings.format {
            AudioFormat::Flac | AudioFormat::WavPack => {
                // Use dedicated tools for optimal encoding
                self.build_format_specific_encode(input, output, settings)?
            }
            _ => {
                // Use preferred backend for other formats
                match self.preferred_backend {
                    Backend::FFmpeg => {
                        let builder = crate::FFmpegBuilder::new();
                        builder.build(input, output, settings)?
                    }
                    Backend::Sox => {
                        let builder = crate::SoxBuilder::new();
                        builder.build(input, output, settings)?
                    }
                }
            }
        };
        
        // No need for duration estimation with stage-weighted progress
        let mut commands = vec![command];
        
        // Add file copying if enabled (same as multi-tool pipeline)
        if settings.copy_files_enabled == Some(true) {
            commands.push(self.build_file_copy_command(input, output, settings)?);
        }
        
        // Add subdirectory copying if enabled
        if settings.copy_subdirectories_enabled == Some(true) {
            commands.push(self.build_subdirectory_copy_command(input, output, settings)?);
        }
        
        // Add merge functionality if enabled
        if settings.merge_to_single == Some(true) {
            commands.push(self.build_merge_command(input, output, settings)?);
        }
        
        // Add ReplayGain if enabled (missing from single-backend mode)
        if settings.replaygain_mode.is_some() {
            commands.push(self.build_replaygain_command(output, settings)?);
        }

        // Add Lineage.txt metadata if provided
        if settings.lineage_file_path.is_some() {
            commands.push(self.build_lineage_metadata_command(output, settings)?);
        }

        // Update description if additional commands were added
        let description = if commands.len() > 1 {
            format!("Multi-tool pipeline: {} + copying/merging", 
                   format!("{:?}", self.preferred_backend).to_lowercase())
        } else {
            format!("Single {} command: {}", 
                   format!("{:?}", self.preferred_backend).to_lowercase(),
                   commands[0].description)
        };
        
        Ok(ConversionPipeline {
            commands,
            temp_files: vec![],
            metadata_strategy: MetadataStrategy::None, // Single command preserves metadata
            expected_duration: None, // Using stage-weighted progress
            description,
        })
    }
    
    /// Build complex multi-tool pipeline
    fn build_multi_tool_pipeline(
        &self,
        input: &Path,
        output: &Path,
        settings: &ConversionSettings,
        operations: &RequiredOperations,
    ) -> Result<ConversionPipeline> {
        let mut commands = Vec::new();
        let mut temp_files = Vec::new();
        let mut current_input = input.to_path_buf();

        // Create encode_settings early (will be modified as pipeline steps complete)
        let mut encode_settings = settings.clone();

        // Step 1: Decode if needed (for compressed input formats)
        let needs_decode = self.needs_decode_step(input, operations, settings);
        if needs_decode {
            let temp_output = format!("temp_decode_{}.wav", uuid::Uuid::new_v4());
            commands.push(self.build_decode_command(input, &temp_output, settings)?);
            temp_files.push(temp_output.clone());
            current_input = temp_output.into();
        }
        
        // Step 2: Handle brick wall resampling (requires SSRC)
        if operations.resample_type == ResampleType::BrickWall {
            let temp_output = format!("temp_ssrc_{}.wav", uuid::Uuid::new_v4());
            commands.push(self.build_ssrc_command(&current_input, &temp_output, settings)?);
            temp_files.push(temp_output.clone());
            current_input = temp_output.into();

            // SSRC handled resampling and bit depth, clear from encode_settings
            encode_settings.sample_rate = None;
            // Also clear dither for lossy formats (pointless after SSRC)
            if matches!(settings.format, AudioFormat::Opus | AudioFormat::Mp3 | AudioFormat::Aac) {
                encode_settings.dither_type = None;
            }
        }

        // Step 3: Processing step (NEW) - resample/bit-depth/dither when not SSRC
        if self.needs_processing_step(settings, operations) {
            let temp_output = format!("temp_process_{}.wav", uuid::Uuid::new_v4());
            log::info!("  📊 Processing: resample/bit-depth/dither → {}", temp_output);

            commands.push(self.build_processing_command(&current_input, &temp_output, settings)?);
            temp_files.push(temp_output.clone());
            current_input = temp_output.into();

            // Clear from encode_settings what was processed
            if operations.needs_resampling && operations.resample_type == ResampleType::Standard {
                encode_settings.sample_rate = None;
            }

            // Clear dithering based on what was processed
            let is_lossy = matches!(settings.format,
                AudioFormat::Opus | AudioFormat::Mp3 | AudioFormat::Aac);

            if is_lossy {
                // Always clear dithering for lossy (codec handles quantization)
                encode_settings.dither_type = None;
            } else if settings.dither_type.is_some() &&
                      settings.dither_type != Some(DitherType::Gesemann) {
                // For lossless, clear non-Gesemann dithering (was applied by processing)
                encode_settings.dither_type = None;
            }
            // If lossless + Gesemann, keep it for Gesemann step to handle
        }

        // Step 4: Handle Gesemann dithering (requires SoX)
        if encode_settings.dither_type == Some(DitherType::Gesemann) {
            let temp_output = format!("temp_dither_{}.wav", uuid::Uuid::new_v4());
            commands.push(self.build_sox_dither_command(&current_input, &temp_output, settings)?);
            temp_files.push(temp_output.clone());
            current_input = temp_output.into();

            // Gesemann step handled dithering, clear it
            encode_settings.dither_type = None;
        }

        // Step 5: Final encode to target format
        let encode_command = self.build_encode_command(&current_input, output, &encode_settings)?;
        commands.push(encode_command);

        // Step 6: Import preserved metadata BEFORE ReplayGain
        // This ensures original tags (ARTIST, ALBUM, etc.) are restored first
        // ReplayGain will then add its tags without being overwritten by metadata import
        let metadata_strategy = self.build_metadata_strategy(input, output, settings)?;

        match &metadata_strategy {
            MetadataStrategy::FormatSpecific { export_command, import_command, temp_file } => {
                // Insert export command at the beginning (before any conversion)
                commands.insert(0, export_command.clone());

                // Insert import command HERE (before ReplayGain)
                commands.push(import_command.clone());

                // Track temp metadata files for cleanup (both filtered and unfiltered)
                temp_files.push(temp_file.clone());
                // Also track the unfiltered version for FLAC, WavPack, Opus, M4A, and MP3
                if settings.format == AudioFormat::Flac
                    || settings.format == AudioFormat::WavPack
                    || settings.format == AudioFormat::Opus
                    || settings.format == AudioFormat::Aac
                    || settings.format == AudioFormat::Mp3 {
                    let unfiltered = temp_file.replace("temp_filtered_", "temp_metadata_");
                    temp_files.push(unfiltered);
                }
            }
            MetadataStrategy::FFmpegJson { extract_command, apply_command, temp_file } => {
                // Insert extract command at the beginning
                commands.insert(0, extract_command.clone());

                // Insert apply command HERE (before ReplayGain)
                commands.push(apply_command.clone());

                // Track temp metadata file for cleanup
                temp_files.push(temp_file.clone());
            }
            MetadataStrategy::None => {
                // No metadata preservation needed
            }
        }

        // Step 6.5: Add ReplayGain AFTER metadata import
        // This ensures ReplayGain tags are not wiped out by metadata import operations
        if operations.needs_replaygain {
            commands.push(self.build_replaygain_command(output, settings)?);
        }

        // Step 7: Add Lineage.txt metadata if provided
        // This happens AFTER both metadata import and ReplayGain
        if settings.lineage_file_path.is_some() {
            commands.push(self.build_lineage_metadata_command(output, settings)?);
        }

        // Step 8: Add file copying if enabled
        if settings.copy_files_enabled == Some(true) {
            commands.push(self.build_file_copy_command(input, output, settings)?);
        }

        // Step 9: Add subdirectory copying if enabled
        if settings.copy_subdirectories_enabled == Some(true) {
            commands.push(self.build_subdirectory_copy_command(input, output, settings)?);
        }

        // Step 10: Add merge functionality if enabled
        if settings.merge_to_single == Some(true) {
            commands.push(self.build_merge_command(input, output, settings)?);
        }

        Ok(ConversionPipeline {
            commands,
            temp_files,
            metadata_strategy,
            expected_duration: None, // Using stage-weighted progress instead
            description: self.build_pipeline_description(operations),
        })
    }
    
    /// Check if we need a decode step before processing
    fn needs_decode_step(&self, input: &Path, operations: &RequiredOperations, settings: &ConversionSettings) -> bool {
        // Need decode if:
        // 1. Input is compressed format (FLAC, MP3, etc.) AND
        // 2. We need SSRC (which only accepts WAV) OR WavPack (which only accepts WAV) OR other tools that need uncompressed input

        let input_ext = input.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_lowercase())
            .unwrap_or_default();

        let is_compressed = matches!(input_ext.as_str(), "flac" | "mp3" | "ogg" | "opus" | "m4a" | "aac" | "wv");
        let needs_ssrc = operations.resample_type == ResampleType::BrickWall;
        let needs_wavpack = settings.format == AudioFormat::WavPack;

        is_compressed && (needs_ssrc || needs_wavpack)
    }
    
    /// Build decode command to convert compressed input to WAV
    fn build_decode_command(&self, input: &Path, output: &str, settings: &ConversionSettings) -> Result<ConversionCommand> {
        let mut args = Vec::new();
        
        let input_ext = input.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_lowercase())
            .unwrap_or_default();
        
        match input_ext.as_str() {
            "flac" => {
                // Use sox to create standard PCM WAV format that SSRC accepts
                // The flac decoder creates WAVE_FORMAT_EXTENSIBLE (0xFFFE) for 24-bit,
                // but SSRC only accepts WAVE_FORMAT_PCM (0x0001).
                // Sox with -t wavpcm forces classic PCM format while preserving bit depth.

                args.push(input.to_string_lossy().to_string());

                // If we know source bit depth, use it explicitly to preserve quality
                if let Some(source_depth) = settings.source_bit_depth {
                    // Convert 320 (float marker) to 32 for sox
                    let bit_depth = if source_depth == 320 { 32 } else { source_depth };
                    args.push("-b".to_string());
                    args.push(bit_depth.to_string());
                }
                // Otherwise let sox auto-detect from FLAC metadata (preserves source bit depth)

                args.push("-t".to_string());
                args.push("wavpcm".to_string());  // Force standard PCM format for SSRC compatibility
                args.push(output.to_string());

                Ok(ConversionCommand {
                    program: "sox".to_string(),
                    arguments: args,
                    environment: HashMap::new(),
                    expected_duration: None,
                    description: if let Some(depth) = settings.source_bit_depth {
                        format!("Decode FLAC to {}-bit PCM WAV (SSRC-compatible)",
                               if depth == 320 { 32 } else { depth })
                    } else {
                        format!("Decode FLAC to PCM WAV (SSRC-compatible, auto bit-depth)")
                    },
                })
            }
            _ => {
                // Use FFmpeg for other formats: ffmpeg -i input.ext -f wav output.wav
                args.push("-nostdin".to_string());
                args.push("-i".to_string());
                args.push(input.to_string_lossy().to_string());
                args.push("-f".to_string());
                args.push("wav".to_string());
                args.push("-y".to_string());
                args.push(output.to_string());
                
                Ok(ConversionCommand {
                    program: "ffmpeg".to_string(),
                    arguments: args,
                    environment: HashMap::new(),
                    expected_duration: None,
                    description: format!("Decode {} to WAV", input_ext.to_uppercase()),
                })
            }
        }
    }

    /// Build SSRC command for brick wall filtering
    fn build_ssrc_command(
        &self,
        input: &Path,
        output: &str,
        settings: &ConversionSettings,
    ) -> Result<ConversionCommand> {
        let mut args = vec![];
        
        // Sample rate
        if let Some(rate) = settings.sample_rate {
            args.push("--rate".to_string());
            args.push(rate.to_string());
        }
        
        // Bit depth strategy depends on target format:
        // - Lossy formats (Opus, MP3, AAC): preserve source bit depth (no reduction/dithering)
        // - Lossless formats (FLAC, WAV, AIFF): use target bit depth with dithering
        args.push("--bits".to_string());

        let is_lossy_target = matches!(settings.format,
            AudioFormat::Opus | AudioFormat::Mp3 | AudioFormat::Aac);

        let ssrc_bits = if is_lossy_target {
            // For lossy: preserve source bit depth through resampling
            // Let the lossy codec handle bit depth conversion internally
            settings.source_bit_depth.unwrap_or(24) as u32
        } else {
            // For lossless: use target bit depth (apply dithering if reducing)
            match settings.bit_depth {
                Some(320) | Some(33) => 24, // Float request → use 24-bit
                Some(depth) if depth > 0 => depth,
                _ => settings.source_bit_depth.unwrap_or(24) as u32, // Fallback to source
            }
        };

        args.push(ssrc_bits.to_string());

        // Quality profile based on resample quality
        let profile = crate::mapping::get_ssrc_profile(
            settings.resample_quality,
            settings.ssrc_insane_mode.unwrap_or(false)
        );
        args.push("--profile".to_string());
        args.push(profile.to_string());
        
        // Quality-based options: for LQ (0), optimize for speed
        let quality = settings.resample_quality.unwrap_or(2);
        
        // Two-pass processing for higher quality levels (slower but better)
        if quality >= 2 { // HQ and above get two-pass
            args.push("--twopass".to_string());
        }
        
        // Normalization for highest quality levels
        if quality >= 3 { // VHQ and above get normalization
            args.push("--normalize".to_string());
        }

        // Dithering: Only apply when reducing bit depth
        // For lossy targets at source bit depth (24→24), dithering adds noise with no benefit
        let is_reducing_bit_depth = if is_lossy_target {
            false // Never reducing for lossy (we preserve source depth)
        } else {
            // For lossless: check if target < source
            settings.bit_depth.is_some() &&
            settings.source_bit_depth.is_some() &&
            settings.bit_depth.unwrap() < settings.source_bit_depth.unwrap() as u32
        };

        let dither_id = if is_reducing_bit_depth {
            // PDF and dithering (triangular PDF)
            args.push("--pdf".to_string());
            args.push("1".to_string()); // Triangular PDF

            args.push("--dither".to_string());
            let id = crate::mapping::get_ssrc_dither_id(settings.dither_type);
            args.push(id.to_string());
            id
        } else {
            // No bit reduction: disable dithering (avoid adding noise)
            args.push("--dither".to_string());
            args.push("0".to_string()); // 0 = no dither
            0
        };
        
        // Input and output files (SSRC doesn't support piping)
        args.push(input.to_string_lossy().to_string());
        args.push(output.to_string());
        
        Ok(ConversionCommand {
            program: "ssrc".to_string(),
            arguments: args,
            environment: HashMap::new(),
            expected_duration: None,
            description: format!("SSRC brick wall resample to {} Hz (profile: {}, dither: {})", 
                               settings.sample_rate.unwrap_or(0), profile, dither_id),
        })
    }
    
    /// Build SoX command for advanced dithering
    fn build_sox_dither_command(
        &self,
        input: &Path,
        output: &str,
        settings: &ConversionSettings,
    ) -> Result<ConversionCommand> {
        let mut args = vec![input.to_string_lossy().to_string()];
        
        // Output format (preserve intermediate quality)
        if let Some(depth) = settings.bit_depth {
            args.push("-b".to_string());
            if depth == 320 || depth == 33 {
                args.push("32".to_string());
                args.push("-e".to_string());
                args.push("float".to_string());
            } else if depth > 0 {
                args.push(depth.to_string());
            }
        }
        
        args.push(output.to_string());
        
        // Add dithering effect
        if let Some(dither) = settings.dither_type {
            let dither_args = crate::mapping::get_sox_dither_args(dither);
            args.extend(dither_args);
        }
        
        Ok(ConversionCommand {
            program: "sox".to_string(),
            arguments: args,
            environment: HashMap::new(),
            expected_duration: None,
            description: format!("SoX dithering with {:?}", settings.dither_type.unwrap_or(DitherType::None)),
        })
    }

    /// Build processing command (resample/bit-depth/dither) using backend
    fn build_processing_command(
        &self,
        input: &Path,
        output: &str,
        settings: &ConversionSettings,
    ) -> Result<ConversionCommand> {
        let mut process_settings = settings.clone();
        process_settings.format = AudioFormat::Wav; // Force WAV output for intermediate

        let is_lossy = matches!(settings.format,
            AudioFormat::Opus | AudioFormat::Mp3 | AudioFormat::Aac);

        if is_lossy {
            // For lossy targets, only resample - don't reduce bit depth or dither
            // Lossy codec handles quantization internally
            process_settings.bit_depth = None;
            process_settings.dither_type = None;
        } else if process_settings.dither_type == Some(DitherType::Gesemann) {
            // For lossless with Gesemann, clear dither (Gesemann step handles it)
            process_settings.dither_type = None;
        }

        // Determine which backend to use
        // FFmpeg can only apply dithering during resampling, so use Sox if:
        // - FFmpeg backend selected
        // - Dithering requested (after Gesemann/lossy clearing above)
        // - No resampling
        let needs_dithering = process_settings.dither_type.is_some() &&
                              process_settings.dither_type != Some(DitherType::None);
        let is_resampling = process_settings.sample_rate.is_some() &&
                            process_settings.sample_rate != Some(0);

        let backend_to_use = if self.preferred_backend == Backend::FFmpeg &&
                                needs_dithering &&
                                !is_resampling {
            log::debug!("🔄 Override: Using Sox for processing step (FFmpeg can't dither without resampling)");
            Backend::Sox
        } else {
            self.preferred_backend
        };

        match backend_to_use {
            Backend::FFmpeg => {
                let builder = crate::FFmpegBuilder::new();
                builder.build(input, Path::new(output), &process_settings)
            }
            Backend::Sox => {
                let builder = crate::SoxBuilder::new();
                builder.build(input, Path::new(output), &process_settings)
            }
        }
    }

    /// Build final encoding command
    fn build_encode_command(
        &self,
        input: &Path,
        output: &Path,
        settings: &ConversionSettings,
    ) -> Result<ConversionCommand> {
        // Choose best backend for the target format
        match settings.format {
            AudioFormat::Flac | AudioFormat::WavPack => {
                // Use format-specific encoders for optimal compression
                self.build_format_specific_encode(input, output, settings)
            }
            _ => {
                // Use preferred backend
                match self.preferred_backend {
                    Backend::FFmpeg => {
                        let builder = crate::FFmpegBuilder::new();
                        builder.build(input, output, settings)
                    }
                    Backend::Sox => {
                        let builder = crate::SoxBuilder::new();
                        builder.build(input, output, settings)
                    }
                }
            }
        }
    }
    
    /// Build format-specific encoding command
    fn build_format_specific_encode(
        &self,
        input: &Path,
        output: &Path,
        settings: &ConversionSettings,
    ) -> Result<ConversionCommand> {
        match settings.format {
            AudioFormat::Flac => {
                let mut args = vec![];
                
                // Force overwrite if requested
                if settings.overwrite {
                    args.push("-f".to_string());
                }
                
                // Compression level
                if let Some(level) = settings.compression_level {
                    args.push(format!("-{}", level));
                }
                
                // Verification
                if settings.verify_encoding == Some(true) {
                    args.push("--verify".to_string());
                }
                
                // MD5 handling - CORRECTED: --no-md5sum option doesn't exist in FLAC
                // Based on testing, FLAC doesn't have --no-md5sum option
                // MD5 checksum is controlled automatically by FLAC encoder
                
                args.push(input.to_string_lossy().to_string());
                args.push("-o".to_string());
                args.push(output.to_string_lossy().to_string());
                
                Ok(ConversionCommand {
                    program: "flac".to_string(),
                    arguments: args,
                    environment: HashMap::new(),
                    expected_duration: None,
                    description: format!("FLAC encode with compression level {}", 
                                       settings.compression_level.unwrap_or(8)),
                })
            }
            AudioFormat::WavPack => {
                // WavPack tool requires WAV input - need to decode first if input is FLAC
                let input_ext = input.extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.to_lowercase())
                    .unwrap_or_default();
                
                // Multi-tool pipeline should have decoded to WAV by this point
                // If we get FLAC input here, it means pipeline setup is wrong
                if input_ext == "flac" {
                    // This shouldn't happen in multi-tool mode - decode stage should have run first
                    eprintln!("WARNING: WavPack encode getting FLAC input - decode stage missing?");
                }
                
                // For WAV input, use dedicated wavpack tool
                let mut args = vec![];
                
                // Always overwrite without prompting
                args.push("-y".to_string());
                
                // Compression level mapping for wavpack
                if let Some(level) = settings.compression_level {
                    match level {
                        0..=2 => args.push("-f".to_string()), // Fast
                        3..=5 => args.push("-h".to_string()), // High  
                        6..=8 => args.push("-hh".to_string()), // Very high
                        _ => args.push("-h".to_string()), // Default high
                    }
                }
                
                // Verification (wavpack uses -v)
                if settings.verify_encoding == Some(true) {
                    args.push("-v".to_string());
                }
                
                // Bit depth handling
                if let Some(bit_depth) = settings.bit_depth {
                    if bit_depth == 33 {
                        // 32-bit float - use Adobe Audition mode
                        args.push("-a".to_string());
                    }
                }
                
                // Input and output
                args.push(input.to_string_lossy().to_string());
                args.push("-o".to_string());
                args.push(output.to_string_lossy().to_string());
                
                Ok(ConversionCommand {
                    program: "wavpack".to_string(),
                    arguments: args,
                    environment: HashMap::new(),
                    expected_duration: None,
                    description: format!("WavPack encode with compression level {}", 
                                       settings.compression_level.unwrap_or(5)),
                })
            }
            _ => {
                // Fallback to backend builders
                let builder = crate::FFmpegBuilder::new();
                builder.build(input, output, settings)
            }
        }
    }
    
    /// Build ReplayGain command
    fn build_replaygain_command(
        &self,
        output: &Path,
        settings: &ConversionSettings,
    ) -> Result<ConversionCommand> {
        log::info!("🎯 Building ReplayGain command for format {:?} with mode {:?}",
            settings.format, settings.replaygain_mode);

        let (program, args, description) = match settings.format {
            _ => {
                // Use loudgain for all formats (including FLAC)
                // NOTE: FLAC previously used metaflac, but it doesn't respect mode selection
                // (always writes both tags with identical values, making Album mode broken)
                let mut args = vec![];

                // Mode selection for loudgain
                match settings.replaygain_mode {
                    Some(ReplayGainMode::Album) | Some(ReplayGainMode::Both) => {
                        // Use track-only mode during per-file processing
                        // Album gain will be calculated in post-processing batch phase
                        args.push("-r".to_string());
                    }
                    Some(ReplayGainMode::Track) => args.push("-r".to_string()),
                    _ => return Err(ConversionError::InvalidSettings("ReplayGain mode not specified".to_string())),
                }
                
                // loudgain flags
                args.push("-k".to_string()); // Keep existing tags (noclip)
                args.push("-s".to_string()); // Tag mode
                args.push("i".to_string()); // Write ReplayGain 2.0 tags
                args.push(output.to_string_lossy().to_string());
                
                ("loudgain".to_string(), args, "ReplayGain with loudgain")
            }
        };
        
        Ok(ConversionCommand {
            program,
            arguments: args,
            environment: HashMap::new(),
            expected_duration: None,
            description: description.to_string(),
        })
    }

    /// Build command to set COMMENT tag from Lineage.txt
    fn build_lineage_metadata_command(
        &self,
        output: &Path,
        settings: &ConversionSettings,
    ) -> Result<ConversionCommand> {
        let lineage_path = match &settings.lineage_file_path {
            Some(path) if path.exists() => path,
            Some(path) => {
                log::warn!("Lineage file does not exist: {}", path.display());
                return Err(ConversionError::InvalidSettings(
                    format!("Lineage file not found: {}", path.display())
                ));
            }
            None => return Err(ConversionError::InvalidSettings(
                "No lineage file path provided".to_string()
            )),
        };

        // Skip lineage embedding for WAV/AIFF to preserve ReplayGain tags
        // WAV/AIFF use ID3v2 chunks for ReplayGain, which FFmpeg strips during metadata operations
        // Lineage.txt file is still copied to output directory for reference
        if settings.format == AudioFormat::Wav || settings.format == AudioFormat::Aiff {
            return Ok(ConversionCommand {
                program: "true".to_string(),
                arguments: vec![],
                environment: HashMap::new(),
                expected_duration: None,
                description: "Skip lineage for WAV/AIFF (preserves ReplayGain for CUE)".to_string(),
            });
        }

        match settings.format {
            AudioFormat::Aac => {
                // Use AtomicParsley to set comment tag from lineage
                // Read lineage content
                let content = std::fs::read_to_string(lineage_path)
                    .map_err(|e| ConversionError::Io(e))?;

                // Store lineage as reverse DNS atom to preserve multi-line content
                // Standard ©cmt atom only stores first line, so use custom field instead
                Ok(ConversionCommand {
                    program: "AtomicParsley".to_string(),
                    arguments: vec![
                        output.display().to_string(),
                        "--rDNSatom".to_string(),
                        content,
                        "name=LINEAGE".to_string(),
                        "domain=com.apple.iTunes".to_string(),
                        "--overWrite".to_string(),
                    ],
                    environment: HashMap::new(),
                    expected_duration: None,
                    description: "Set LINEAGE tag from Lineage.txt (AtomicParsley)".to_string(),
                })
            }
            AudioFormat::Flac => {
                // Use metaflac --set-tag-from-file (reads file verbatim, preserves multiline)
                Ok(ConversionCommand {
                    program: "metaflac".to_string(),
                    arguments: vec![
                        format!("--set-tag-from-file=COMMENT={}", lineage_path.display()),
                        output.display().to_string(),
                    ],
                    environment: HashMap::new(),
                    expected_duration: None,
                    description: "Set COMMENT tag from Lineage.txt".to_string(),
                })
            }
            AudioFormat::Opus => {
                // Read lineage content
                let content = std::fs::read_to_string(lineage_path)
                    .map_err(|e| ConversionError::Io(e))?;

                // Escape single quotes for shell (replace ' with '\\'')
                let output_escaped = output.display().to_string().replace("'", "'\\''");
                let content_escaped = content.replace("'", "'\\''");

                Ok(ConversionCommand {
                    program: "sh".to_string(),
                    arguments: vec![
                        "-c".to_string(),
                        format!(
                            "opustags --delete COMMENT -a 'COMMENT={}' --in-place '{}'",
                            content_escaped,
                            output_escaped
                        ),
                    ],
                    environment: HashMap::new(),
                    expected_duration: None,
                    description: "Set COMMENT tag from Lineage.txt (opustags)".to_string(),
                })
            }
            AudioFormat::Mp3 => {
                // MP3: Use FFmpeg with -id3v2_version 3 for compatibility
                let content = std::fs::read_to_string(lineage_path)
                    .map_err(|e| ConversionError::Io(e))?;

                // Preserve original extension by appending to filename, not replacing extension
                let temp_output = {
                    let stem = output.file_stem()
                        .ok_or_else(|| ConversionError::InvalidSettings(
                            "Output file has no stem".to_string()))?
                        .to_string_lossy();
                    let ext = output.extension()
                        .ok_or_else(|| ConversionError::InvalidSettings(
                            "Output file has no extension".to_string()))?
                        .to_string_lossy();
                    let parent = output.parent()
                        .ok_or_else(|| ConversionError::InvalidSettings(
                            "Output file has no parent".to_string()))?;

                    parent.join(format!("{}.lineage_temp.{}", stem, ext))
                };

                // Only escape quotes, keep newlines as literal newlines
                let script = format!(
                    r#"ffmpeg -nostdin -i "{input}" -c copy -id3v2_version 3 -metadata comment="{comment}" -y "{temp}" && mv "{temp}" "{input}""#,
                    input = output.display(),
                    temp = temp_output.display(),
                    comment = content.replace('"', r#"\""#),
                );

                Ok(ConversionCommand {
                    program: "bash".to_string(),
                    arguments: vec!["-c".to_string(), script],
                    environment: HashMap::new(),
                    expected_duration: None,
                    description: "Set comment metadata from Lineage.txt (FFmpeg with ID3v2.3)".to_string(),
                })
            }
            _ => {
                // For non-FLAC, read content and use FFmpeg
                // Use bash to handle temp file + move atomically
                let content = std::fs::read_to_string(lineage_path)
                    .map_err(|e| ConversionError::Io(e))?;

                // Preserve original extension by appending to filename, not replacing extension
                let temp_output = {
                    let stem = output.file_stem()
                        .ok_or_else(|| ConversionError::InvalidSettings(
                            "Output file has no stem".to_string()))?
                        .to_string_lossy();
                    let ext = output.extension()
                        .ok_or_else(|| ConversionError::InvalidSettings(
                            "Output file has no extension".to_string()))?
                        .to_string_lossy();
                    let parent = output.parent()
                        .ok_or_else(|| ConversionError::InvalidSettings(
                            "Output file has no parent".to_string()))?;

                    parent.join(format!("{}.lineage_temp.{}", stem, ext))
                };

                // Only escape quotes, keep newlines as literal newlines
                let script = format!(
                    r#"ffmpeg -nostdin -i "{input}" -c copy -metadata comment="{comment}" -y "{temp}" && mv "{temp}" "{input}""#,
                    input = output.display(),
                    temp = temp_output.display(),
                    comment = content.replace('"', r#"\""#),
                );

                Ok(ConversionCommand {
                    program: "bash".to_string(),
                    arguments: vec!["-c".to_string(), script],
                    environment: HashMap::new(),
                    expected_duration: None,
                    description: "Set comment metadata from Lineage.txt (FFmpeg)".to_string(),
                })
            }
        }
    }

    /// Build metadata preservation strategy
    fn build_metadata_strategy(
        &self,
        input: &Path,
        output: &Path,
        settings: &ConversionSettings,
    ) -> Result<MetadataStrategy> {
        // Detect input format from extension
        let input_ext = input.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        // Check if we need metadata preservation (FLAC→FLAC, FLAC→WavPack, WavPack→WavPack, etc.)
        let needs_preservation = match (input_ext.as_str(), settings.format) {
            ("flac", AudioFormat::Flac) => true,
            ("flac", AudioFormat::WavPack) => true,
            ("wv", AudioFormat::WavPack) => true,
            ("flac", AudioFormat::Opus) => true,
            ("opus", AudioFormat::Opus) => true,
            ("flac", AudioFormat::Aac) => true,
            ("m4a", AudioFormat::Aac) => true,
            ("flac", AudioFormat::Mp3) => true,
            ("mp3", AudioFormat::Mp3) => true,
            _ => false,
        };

        if !needs_preservation {
            return Ok(MetadataStrategy::None);
        }

        // Build format-specific metadata preservation
        match settings.format {
            AudioFormat::Flac => {
                // Use metaflac for FLAC metadata preservation
                let temp_metadata = format!("/tmp/temp_metadata_{}.txt", uuid::Uuid::new_v4());
                let temp_filtered = format!("/tmp/temp_filtered_{}.txt", uuid::Uuid::new_v4());

                // Escape single quotes in paths by replacing ' with '\''
                let input_escaped = input.display().to_string().replace("'", "'\\''");
                let output_escaped = output.display().to_string().replace("'", "'\\''");

                Ok(MetadataStrategy::FormatSpecific {
                    export_command: ConversionCommand {
                        program: "sh".to_string(),
                        arguments: vec![
                            "-c".to_string(),
                            format!(
                                "metaflac --export-tags-to='{}' '{}' && grep -v '^[[:space:]]*$' '{}' | grep '=' > '{}' || cp '{}' '{}'",
                                temp_metadata,
                                input_escaped,
                                temp_metadata,
                                temp_filtered,
                                temp_metadata,
                                temp_filtered
                            ),
                        ],
                        environment: std::collections::HashMap::new(),
                        expected_duration: None,
                        description: "Export and filter FLAC metadata".to_string(),
                    },
                    import_command: ConversionCommand {
                        program: "sh".to_string(),
                        arguments: vec![
                            "-c".to_string(),
                            format!(
                                "if [ -s '{}' ]; then metaflac --import-tags-from='{}' '{}'; fi",
                                temp_filtered,
                                temp_filtered,
                                output_escaped
                            ),
                        ],
                        environment: std::collections::HashMap::new(),
                        expected_duration: None,
                        description: "Import filtered FLAC metadata".to_string(),
                    },
                    temp_file: temp_filtered,
                })
            }
            AudioFormat::WavPack => {
                // Handle different input formats
                if input_ext == "flac" {
                    // FLAC→WavPack: Use metaflac to export, wvtag to import
                    let temp_metadata = format!("/tmp/temp_metadata_{}.txt", uuid::Uuid::new_v4());
                    let temp_filtered = format!("/tmp/temp_filtered_{}.txt", uuid::Uuid::new_v4());

                    // Escape single quotes in paths
                    let input_escaped = input.display().to_string().replace("'", "'\\''");
                    let output_escaped = output.display().to_string().replace("'", "'\\''");

                    Ok(MetadataStrategy::FormatSpecific {
                        export_command: ConversionCommand {
                            program: "sh".to_string(),
                            arguments: vec![
                                "-c".to_string(),
                                format!(
                                    "metaflac --export-tags-to='{}' '{}' && grep -v '^[[:space:]]*$' '{}' | grep '=' > '{}' || cp '{}' '{}'",
                                    temp_metadata,
                                    input_escaped,
                                    temp_metadata,
                                    temp_filtered,
                                    temp_metadata,
                                    temp_filtered
                                ),
                            ],
                            environment: std::collections::HashMap::new(),
                            expected_duration: None,
                            description: "Export FLAC metadata for WavPack".to_string(),
                        },
                        import_command: ConversionCommand {
                            program: "sh".to_string(),
                            arguments: vec![
                                "-c".to_string(),
                                format!(
                                    r#"if [ -s '{}' ]; then
    while IFS= read -r line; do
        if [ -n "$line" ]; then
            wvtag -q -w "$line" '{}' 2>/dev/null || true
        fi
    done < '{}'
fi"#,
                                    temp_filtered,
                                    output_escaped,
                                    temp_filtered
                                ),
                            ],
                            environment: std::collections::HashMap::new(),
                            expected_duration: None,
                            description: "Import FLAC metadata to WavPack using wvtag".to_string(),
                        },
                        temp_file: temp_filtered,
                    })
                } else {
                    // WavPack→WavPack: Use FFmpeg for metadata preservation
                    let temp_metadata = format!("/tmp/temp_metadata_{}.txt", uuid::Uuid::new_v4());
                    let temp_filtered = format!("/tmp/temp_filtered_{}.txt", uuid::Uuid::new_v4());

                    // Escape single quotes in paths
                    let input_escaped = input.display().to_string().replace("'", "'\\''");
                    let output_escaped = output.display().to_string().replace("'", "'\\''");

                    Ok(MetadataStrategy::FormatSpecific {
                        export_command: ConversionCommand {
                            program: "sh".to_string(),
                            arguments: vec![
                                "-c".to_string(),
                                format!(
                                    r#"ffmpeg -nostdin -i '{}' -f ffmetadata '{}' 2>/dev/null && awk '
BEGIN {{ skip=0 }}
/^REPLAYGAIN_/ {{ next }}
/^comment=/ {{
    if ($0 ~ /\\$/) {{ skip=1 }}
    next
}}
{{
    if (skip) {{
        if ($0 ~ /\\$/) {{
            next
        }} else {{
            skip=0
            next
        }}
    }} else {{
        print
    }}
}}' '{}' > '{}' || touch '{}'"#,
                                    input_escaped,
                                    temp_metadata,
                                    temp_metadata,
                                    temp_filtered,
                                    temp_filtered
                                ),
                            ],
                            environment: std::collections::HashMap::new(),
                            expected_duration: None,
                            description: "Export WavPack metadata using FFmpeg".to_string(),
                        },
                        import_command: ConversionCommand {
                            program: "sh".to_string(),
                            arguments: vec![
                                "-c".to_string(),
                                format!(
                                    "if [ -s '{}' ]; then ffmpeg -nostdin -i '{}' -i '{}' -map_metadata 1 -c copy -y '{}.tmp' 2>/dev/null && mv '{}.tmp' '{}'; fi",
                                    temp_filtered,
                                    output_escaped,
                                    temp_filtered,
                                    output_escaped,
                                    output_escaped,
                                    output_escaped
                                ),
                            ],
                            environment: std::collections::HashMap::new(),
                            expected_duration: None,
                            description: "Import WavPack metadata using FFmpeg".to_string(),
                        },
                        temp_file: temp_filtered,
                    })
                }
            }
            AudioFormat::Opus => {
                // Handle different input formats
                if input_ext == "flac" {
                    // FLAC→Opus: Use metaflac to export, opustags to import
                    let temp_metadata = format!("/tmp/temp_metadata_{}.txt", uuid::Uuid::new_v4());
                    let temp_filtered = format!("/tmp/temp_filtered_{}.txt", uuid::Uuid::new_v4());

                    // Escape single quotes in paths by replacing ' with '\''
                    let input_escaped = input.display().to_string().replace("'", "'\\''");
                    let output_escaped = output.display().to_string().replace("'", "'\\''");

                    Ok(MetadataStrategy::FormatSpecific {
                        export_command: ConversionCommand {
                            program: "sh".to_string(),
                            arguments: vec![
                                "-c".to_string(),
                                format!(
                                    "metaflac --export-tags-to='{}' '{}' && grep -v '^[[:space:]]*$' '{}' | grep '=' > '{}' || cp '{}' '{}'",
                                    temp_metadata,
                                    input_escaped,
                                    temp_metadata,
                                    temp_filtered,
                                    temp_metadata,
                                    temp_filtered
                                ),
                            ],
                            environment: std::collections::HashMap::new(),
                            expected_duration: None,
                            description: "Export FLAC metadata for Opus".to_string(),
                        },
                        import_command: ConversionCommand {
                            program: "sh".to_string(),
                            arguments: vec![
                                "-c".to_string(),
                                format!(
                                    "if [ -s '{}' ]; then opustags --set-all --in-place '{}' < '{}'; fi",
                                    temp_filtered,
                                    output_escaped,
                                    temp_filtered
                                ),
                            ],
                            environment: std::collections::HashMap::new(),
                            expected_duration: None,
                            description: "Import FLAC metadata to Opus".to_string(),
                        },
                        temp_file: temp_filtered,
                    })
                } else {
                    // Opus→Opus: Use opustags for metadata preservation
                    let temp_metadata = format!("/tmp/temp_metadata_{}.txt", uuid::Uuid::new_v4());
                    let temp_filtered = format!("/tmp/temp_filtered_{}.txt", uuid::Uuid::new_v4());

                    // Escape single quotes in paths by replacing ' with '\''
                    let input_escaped = input.display().to_string().replace("'", "'\\''");
                    let output_escaped = output.display().to_string().replace("'", "'\\''");

                    Ok(MetadataStrategy::FormatSpecific {
                        export_command: ConversionCommand {
                            program: "sh".to_string(),
                            arguments: vec![
                                "-c".to_string(),
                                format!(
                                    "opustags '{}' > '{}' && grep -v '^[[:space:]]*$' '{}' | grep '=' > '{}' || cp '{}' '{}'",
                                    input_escaped,
                                    temp_metadata,
                                    temp_metadata,
                                    temp_filtered,
                                    temp_metadata,
                                    temp_filtered
                                ),
                            ],
                            environment: std::collections::HashMap::new(),
                            expected_duration: None,
                            description: "Export and filter Opus metadata".to_string(),
                        },
                        import_command: ConversionCommand {
                            program: "sh".to_string(),
                            arguments: vec![
                                "-c".to_string(),
                                format!(
                                    "if [ -s '{}' ]; then opustags --set-all --in-place '{}' < '{}'; fi",
                                    temp_filtered,
                                    output_escaped,
                                    temp_filtered
                                ),
                            ],
                            environment: std::collections::HashMap::new(),
                            expected_duration: None,
                            description: "Import filtered Opus metadata".to_string(),
                        },
                        temp_file: temp_filtered,
                    })
                }
            }
            AudioFormat::Aac => {
                // M4A/AAC: Use FFmpeg for metadata preservation (works for any input format)
                let temp_metadata = format!("/tmp/temp_metadata_{}.txt", uuid::Uuid::new_v4());
                let temp_filtered = format!("/tmp/temp_filtered_{}.txt", uuid::Uuid::new_v4());

                // Escape single quotes in paths
                let input_escaped = input.display().to_string().replace("'", "'\\''");
                let output_escaped = output.display().to_string().replace("'", "'\\''");

                Ok(MetadataStrategy::FormatSpecific {
                    export_command: ConversionCommand {
                        program: "sh".to_string(),
                        arguments: vec![
                            "-c".to_string(),
                            format!(
                                r#"ffmpeg -nostdin -i '{}' -f ffmetadata '{}' 2>/dev/null && awk '
BEGIN {{ skip=0 }}
/^REPLAYGAIN_/ {{ next }}
/^comment=/ {{
    if ($0 ~ /\\$/) {{ skip=1 }}
    next
}}
{{
    if (skip) {{
        if ($0 ~ /\\$/) {{
            next
        }} else {{
            skip=0
            next
        }}
    }} else {{
        print
    }}
}}' '{}' > '{}' || touch '{}'"#,
                                input_escaped,
                                temp_metadata,
                                temp_metadata,
                                temp_filtered,
                                temp_filtered
                            ),
                        ],
                        environment: std::collections::HashMap::new(),
                        expected_duration: None,
                        description: "Export metadata using FFmpeg".to_string(),
                    },
                    import_command: ConversionCommand {
                        program: "sh".to_string(),
                        arguments: vec![
                            "-c".to_string(),
                            format!(
                                "if [ -s '{}' ]; then ffmpeg -nostdin -i '{}' -i '{}' -map_metadata 1 -c copy -y '{}.tmp' 2>/dev/null && mv '{}.tmp' '{}'; fi",
                                temp_filtered,
                                output_escaped,
                                temp_filtered,
                                output_escaped,
                                output_escaped,
                                output_escaped
                            ),
                        ],
                        environment: std::collections::HashMap::new(),
                        expected_duration: None,
                        description: "Import metadata using FFmpeg".to_string(),
                    },
                    temp_file: temp_filtered,
                })
            }
            AudioFormat::Mp3 => {
                // MP3: Use FFmpeg for metadata preservation (works for any input format)
                let temp_metadata = format!("/tmp/temp_metadata_{}.txt", uuid::Uuid::new_v4());
                let temp_filtered = format!("/tmp/temp_filtered_{}.txt", uuid::Uuid::new_v4());

                // Escape single quotes in paths
                let input_escaped = input.display().to_string().replace("'", "'\\''");
                let output_escaped = output.display().to_string().replace("'", "'\\''");

                Ok(MetadataStrategy::FormatSpecific {
                    export_command: ConversionCommand {
                        program: "sh".to_string(),
                        arguments: vec![
                            "-c".to_string(),
                            format!(
                                r#"ffmpeg -nostdin -i '{}' -f ffmetadata '{}' 2>/dev/null && awk '
BEGIN {{ skip=0 }}
/^REPLAYGAIN_/ {{ next }}
/^comment=/ {{
    if ($0 ~ /\\$/) {{ skip=1 }}
    next
}}
{{
    if (skip) {{
        if ($0 ~ /\\$/) {{
            next
        }} else {{
            skip=0
            next
        }}
    }} else {{
        print
    }}
}}' '{}' > '{}' || touch '{}'"#,
                                input_escaped,
                                temp_metadata,
                                temp_metadata,
                                temp_filtered,
                                temp_filtered
                            ),
                        ],
                        environment: std::collections::HashMap::new(),
                        expected_duration: None,
                        description: "Export metadata using FFmpeg".to_string(),
                    },
                    import_command: ConversionCommand {
                        program: "sh".to_string(),
                        arguments: vec![
                            "-c".to_string(),
                            format!(
                                "if [ -s '{}' ]; then ffmpeg -nostdin -i '{}' -i '{}' -map_metadata 1 -c copy -id3v2_version 3 -y '{}.tmp' 2>/dev/null && mv '{}.tmp' '{}'; fi",
                                temp_filtered,
                                output_escaped,
                                temp_filtered,
                                output_escaped,
                                output_escaped,
                                output_escaped
                            ),
                        ],
                        environment: std::collections::HashMap::new(),
                        expected_duration: None,
                        description: "Import metadata using FFmpeg (ID3v2.3)".to_string(),
                    },
                    temp_file: temp_filtered,
                })
            }
            _ => Ok(MetadataStrategy::None),
        }
    }
    
    /// Calculate total expected duration for a pipeline
    #[allow(dead_code)]
    fn calculate_pipeline_duration(
        &self,
        commands: &[ConversionCommand],
        _input_path: &Path,
    ) -> Result<Option<std::time::Duration>> {
        if commands.is_empty() {
            return Ok(None);
        }
        
        // Sum the duration estimates from individual commands
        // (Commands should already have their durations estimated when built)
        let mut total_duration = std::time::Duration::from_secs(0);
        let mut any_estimate_found = false;
        
        for command in commands {
            match command.expected_duration {
                Some(duration) => {
                    total_duration += duration;
                    any_estimate_found = true;
                }
                None => {
                    // Use fallback for commands without estimates
                    let fallback = self.get_fallback_duration_for_command(command);
                    total_duration += fallback;
                    any_estimate_found = true;
                }
            }
        }
        
        if any_estimate_found {
            Ok(Some(total_duration))
        } else {
            Ok(None)
        }
    }
    
    /// Get fallback duration for a command when estimation fails
    #[allow(dead_code)]
    fn get_fallback_duration_for_command(&self, command: &ConversionCommand) -> std::time::Duration {
        match command.program.as_str() {
            "ssrc" => std::time::Duration::from_secs(60), // SSRC is slow
            "sox" => std::time::Duration::from_secs(30),  // Sox is medium
            "flac" => std::time::Duration::from_secs(20), // FLAC encoding is fast
            "metaflac" => std::time::Duration::from_secs(5), // Metadata operations are very fast
            "loudgain" => std::time::Duration::from_secs(15), // ReplayGain analysis takes time
            "ffmpeg" => std::time::Duration::from_secs(30), // FFmpeg varies
            _ => std::time::Duration::from_secs(30), // Generic fallback
        }
    }

    /// Build human-readable pipeline description
    fn build_pipeline_description(&self, operations: &RequiredOperations) -> String {
        let mut parts = vec![];
        
        if operations.resample_type == ResampleType::BrickWall {
            parts.push("SSRC brick wall");
        }
        
        if operations.dither_complexity == DitherComplexity::GesemmannOnly {
            parts.push("SoX Gesemann dither");
        }
        
        if operations.needs_replaygain {
            parts.push("ReplayGain");
        }
        
        if parts.is_empty() {
            "Multi-tool pipeline".to_string()
        } else {
            format!("Multi-tool pipeline: {}", parts.join(" → "))
        }
    }
    
    /// Build file copying command
    fn build_file_copy_command(
        &self,
        input: &Path,
        output: &Path,
        settings: &ConversionSettings,
    ) -> Result<ConversionCommand> {
        let input_dir = input.parent().ok_or_else(|| {
            ConversionError::InvalidSettings("Input file has no parent directory".to_string())
        })?;
        let output_dir = if let Some(parent) = output.parent() {
            if parent.as_os_str().is_empty() {
                // Parent exists but is empty string - use current directory
                std::env::current_dir()
                    .map_err(|e| ConversionError::InvalidSettings(format!("Cannot get current directory: {}", e)))?
            } else {
                parent.to_path_buf()
            }
        } else {
            // Output file has no parent, use current working directory  
            std::env::current_dir()
                .map_err(|e| ConversionError::InvalidSettings(format!("Cannot get current directory: {}", e)))?
        };
        
        // Skip copying if source and destination are the same directory
        // This happens in archive processing where files are already in the right location
        if input_dir == output_dir {
            // Return a no-op command instead of an error
            return Ok(ConversionCommand {
                program: "true".to_string(),  // No-op command that always succeeds
                arguments: vec![],
                environment: HashMap::new(),
                expected_duration: None,
                description: "Skip file copy - auxiliary files already in target directory".to_string(),
            });
        }
        
        // Get the base filename (without extension) to find related files
        let _input_stem = input.file_stem().ok_or_else(|| {
            ConversionError::InvalidSettings("Input file has no stem".to_string())
        })?.to_string_lossy();
        
        // Parse extensions to copy
        let default_extensions = "txt,cue,log".to_string();
        let extensions = settings.copy_files_extensions
            .as_ref()
            .unwrap_or(&default_extensions)
            .split(',')
            .map(|s| s.trim())
            .collect::<Vec<_>>();
        
        // Build simpler approach: just use cp to copy specific files
        let mut args = Vec::new();
        
        // Find auxiliary files manually - look for any files with specified extensions
        for entry in std::fs::read_dir(input_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                // Exclude internal files (concat_list.txt used by merge operations)
                if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                    if filename == "concat_list.txt" {
                        continue;
                    }
                }

                if let Some(file_ext) = path.extension().and_then(|ext| ext.to_str()) {
                    if extensions.contains(&file_ext.to_lowercase().as_str()) {
                        args.push(path.to_string_lossy().to_string());
                    }
                }
            }
        }

        // If no auxiliary files found, return a no-op command instead of failing
        // This is normal for archives that don't include .cue, .log, or .txt files
        if args.is_empty() {
            return Ok(ConversionCommand {
                program: "true".to_string(),  // No-op command that always succeeds
                arguments: vec![],
                environment: HashMap::new(),
                expected_duration: None,
                description: "Skip file copy - no auxiliary files found".to_string(),
            });
        }
        
        // Add destination directory  
        let dest_dir = output_dir.to_string_lossy().to_string();
        args.push(dest_dir.clone());
        
        let file_count = args.len() - 1;  // Count before moving args
        
        
        Ok(ConversionCommand {
            program: "cp".to_string(),
            arguments: args,
            environment: HashMap::new(),
            expected_duration: None,
            description: format!("Copy {} auxiliary files to {}", file_count, dest_dir),
        })
    }
    
    /// Build subdirectory copying command
    fn build_subdirectory_copy_command(
        &self,
        input: &Path,
        output: &Path,
        settings: &ConversionSettings,
    ) -> Result<ConversionCommand> {
        let input_dir = input.parent().ok_or_else(|| {
            ConversionError::InvalidSettings("Input file has no parent directory".to_string())
        })?;
        let output_dir = output.parent().ok_or_else(|| {
            ConversionError::InvalidSettings("Output file has no parent directory".to_string())
        })?;
        
        // Skip copying if source and destination are the same directory
        // This happens in archive processing where subdirectories are already in the right location
        if input_dir == output_dir {
            // Return a no-op command instead of an error
            return Ok(ConversionCommand {
                program: "true".to_string(),  // No-op command that always succeeds
                arguments: vec![],
                environment: HashMap::new(),
                expected_duration: None,
                description: "Skip subdirectory copy - already in target directory".to_string(),
            });
        }
        
        // Use find + cp for more reliable subdirectory copying
        let default_pattern = "*".to_string();
        let patterns = settings.copy_subdirectories
            .as_ref()
            .unwrap_or(&default_pattern);
        
        let mut args = vec![
            input_dir.to_string_lossy().to_string(),
            "-type".to_string(), "d".to_string(), // Find directories only
            "-mindepth".to_string(), "1".to_string(), // Skip the input directory itself
        ];
        
        // Add pattern matching if not "*"
        if patterns != "*" {
            args.push("-name".to_string());
            if patterns.contains(',') {
                // Multiple patterns - use regex or multiple -name options
                args.push("(".to_string());
                for (i, pattern) in patterns.split(',').enumerate() {
                    if i > 0 {
                        args.push("-o".to_string());
                    }
                    args.push("-name".to_string());
                    args.push(pattern.trim().to_string());
                }
                args.push(")".to_string());
            } else {
                args.push(patterns.to_string());
            }
        }
        
        // Use find + exec to copy each directory
        args.extend(vec![
            "-exec".to_string(),
            "cp".to_string(),
            "-r".to_string(),
            "{}".to_string(),
            output_dir.to_string_lossy().to_string(),
            ";".to_string(),
        ]);
        
        Ok(ConversionCommand {
            program: "find".to_string(),
            arguments: args,
            environment: HashMap::new(),
            expected_duration: None,
            description: format!("Copy subdirectories ({})", patterns),
        })
    }
    
    /// Build merge command for combining multiple files
    fn build_merge_command(
        &self,
        input: &Path,
        output: &Path,
        settings: &ConversionSettings,
    ) -> Result<ConversionCommand> {
        // For merge functionality, we need to handle multiple input files
        // This is a placeholder implementation - needs more context about what files to merge
        let input_dir = input.parent().ok_or_else(|| {
            ConversionError::InvalidSettings("Input file has no parent directory".to_string())
        })?;
        
        match settings.format {
            AudioFormat::Mp3 => {
                // Use ffmpeg for MP3 concatenation
                let args = vec![
                    "-f".to_string(), "concat".to_string(),
                    "-safe".to_string(), "0".to_string(),
                    "-i".to_string(), format!("{}/filelist.txt", input_dir.display()),
                    "-c".to_string(), "copy".to_string(),
                    output.to_string_lossy().to_string(),
                ];
                
                Ok(ConversionCommand {
                    program: "ffmpeg".to_string(),
                    arguments: args,
                    environment: HashMap::new(),
                    expected_duration: None,
                    description: "Merge multiple files into single output".to_string(),
                })
            }
            _ => {
                // Use sox for other formats - manually expand glob pattern
                let mut args = Vec::new();
                
                // Find all audio files in the directory to merge
                let _pattern = format!("*.{}", settings.format.extension());
                for entry in std::fs::read_dir(input_dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    if path.is_file() {
                        if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                            if filename.ends_with(&format!(".{}", settings.format.extension())) {
                                args.push(path.to_string_lossy().to_string());
                            }
                        }
                    }
                }
                
                // Add output file
                args.push(output.to_string_lossy().to_string());
                
                if args.len() < 2 {
                    return Err(ConversionError::InvalidSettings("No files found to merge".to_string()));
                }
                
                let file_count = args.len() - 1; // Count before moving args
                
                Ok(ConversionCommand {
                    program: "sox".to_string(),
                    arguments: args,
                    environment: HashMap::new(),
                    expected_duration: None,
                    description: format!("Merge {} audio files with sox", file_count),
                })
            }
        }
    }
}

/// Analysis of what operations are required for a conversion
#[derive(Debug, Clone, Default)]
struct RequiredOperations {
    needs_format_conversion: bool,
    needs_resampling: bool,
    resample_type: ResampleType,
    needs_dithering: bool,
    dither_complexity: DitherComplexity,
    needs_replaygain: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ResampleType {
    None,
    Standard,    // Regular SoXR or SoX resampling
    BrickWall,   // Requires SSRC
}

impl Default for ResampleType {
    fn default() -> Self {
        ResampleType::None
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DitherComplexity {
    None,
    Basic,           // TPDF - most tools support
    Advanced,        // Shibata variants - SoX or SSRC
    GesemmannOnly,   // Only SoX supports
}

impl Default for DitherComplexity {
    fn default() -> Self {
        DitherComplexity::None
    }
}

impl ConversionPipeline {
    /// Execute the entire pipeline with stage-weighted progress reporting
    pub fn execute_with_progress(
        &self,
        _input_path: &Path,
        progress_callback: Option<ProgressCallback>
    ) -> std::io::Result<Vec<std::process::Output>> {
        let mut outputs = Vec::new();
        
        // Define stage weights based on typical operation complexity
        let stage_weights = self.calculate_stage_weights();
        let mut cumulative_progress = 0.0f32;
        
        for (index, command) in self.commands.iter().enumerate() {
            let stage_weight = stage_weights.get(index).copied().unwrap_or(10.0);
            
            // Execute command (no individual progress for simplicity)
            let output = command.execute()?;
            
            // Check if command failed (non-zero exit code)
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                
                // Clean up temp files before returning error
                self.cleanup_temp_files();
                
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Stage {} ({}) failed with exit code {:?}.\nStderr: {}\nStdout: {}\nCommand: {} {}",
                           index + 1,
                           command.program,
                           output.status.code(),
                           stderr.trim(),
                           stdout.trim(),
                           command.program,
                           command.arguments.join(" ")
                    )
                ));
            }
            
            outputs.push(output);
            
            // Update progress based on stage completion
            cumulative_progress += stage_weight;
            
            if let Some(ref callback) = progress_callback {
                callback(cumulative_progress);
            }
            
            println!("✅ Stage {} ({}) completed - Progress: {:.1}%", 
                     index + 1, command.program, cumulative_progress);
        }
        
        // Ensure we reach 100% at the end
        if let Some(ref callback) = progress_callback {
            callback(100.0);
        }
        
        // Clean up temporary files
        self.cleanup_temp_files();
        
        Ok(outputs)
    }
    
    /// Execute pipeline with progress mapped to specific phase for main project integration
    pub async fn execute_with_phase_progress(
        &self,
        progress_tx: &tokio::sync::mpsc::Sender<crate::integration::ProgressUpdate>,
        item_id: &str,
        target_phase: crate::integration::ConversionPhase,
    ) -> crate::Result<Vec<std::process::Output>> {
        let mut outputs = Vec::new();
        
        // Calculate stage weights (already implemented)
        let stage_weights = self.calculate_stage_weights();
        let mut cumulative_progress = 0.0f32;
        
        for (index, command) in self.commands.iter().enumerate() {
            let stage_weight = stage_weights.get(index).copied().unwrap_or(10.0);
            
            // Send progress update before starting stage
            let stage_progress = cumulative_progress;
            let overall_progress = target_phase.calculate_overall_progress(stage_progress);
            
            let _ = progress_tx.send(crate::integration::ProgressUpdate {
                item_id: item_id.to_string(),
                progress: overall_progress,
                status: crate::integration::ConversionStatus::Processing {
                    progress: overall_progress,
                    message: Some(format!("Stage {}/{}: {}", index + 1, self.commands.len(), command.description)),
                    file_progress: None,
                    phase: Some(target_phase),
                    phase_progress: Some(stage_progress),
                },
            }).await;

            // Execute command with timeout (use robust execution)
            // CRITICAL: execute_with_timeout() is blocking, so run it in a dedicated thread pool
            // to avoid blocking the async runtime
            let command_clone = command.clone();
            let output = tokio::task::spawn_blocking(move || {
                command_clone.execute_with_timeout(None)
            }).await
                .map_err(|e| crate::ConversionError::Io(
                    std::io::Error::new(std::io::ErrorKind::Other, format!("Task join error: {}", e))
                ))??;
            
            // Check if command failed (critical for robustness)
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let error_msg = format!(
                    "Stage {} ({}) failed with exit code {:?}. Command: {}\nError: {}",
                    index + 1,
                    command.program,
                    output.status.code(),
                    command.to_string(),
                    stderr.trim()
                );
                
                // Send failure update
                let _ = progress_tx.send(crate::integration::ProgressUpdate {
                    item_id: item_id.to_string(), 
                    progress: overall_progress,
                    status: crate::integration::ConversionStatus::Failed { error: error_msg.clone() },
                }).await;
                
                // Clean up temp files before returning error
                self.cleanup_temp_files();
                
                return Err(crate::ConversionError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    error_msg
                )));
            }
            
            outputs.push(output);
            
            // Update cumulative progress
            cumulative_progress += stage_weight;
        }
        
        // Send final completion within Converting phase
        let final_progress = target_phase.calculate_overall_progress(100.0);
        let _ = progress_tx.send(crate::integration::ProgressUpdate {
            item_id: item_id.to_string(),
            progress: final_progress,
            status: crate::integration::ConversionStatus::Processing {
                progress: final_progress,
                message: Some("Conversion pipeline complete".to_string()),
                file_progress: None,
                phase: Some(target_phase),
                phase_progress: Some(100.0),
            },
        }).await;
        
        // Clean up temporary files
        self.cleanup_temp_files();
        
        Ok(outputs)
    }
    
    /// Calculate stage weights based on operation types
    pub fn calculate_stage_weights(&self) -> Vec<f32> {
        let mut weights = Vec::new();
        
        for command in &self.commands {
            let weight = match command.program.as_str() {
                // Heavy operations
                "7z" => 50.0,      // Archive extraction is heaviest
                "ssrc" => 25.0,    // Brick wall resampling is heavy
                
                // Medium operations  
                "ffmpeg" => 15.0,  // Audio conversion
                "sox" => 10.0,     // Dithering/effects
                "flac" => 10.0,    // FLAC encoding
                "lame" => 10.0,    // MP3 encoding
                
                // Light operations
                "metaflac" => 3.0, // Metadata operations
                "loudgain" => 5.0, // ReplayGain analysis
                "opustags" => 2.0, // Tag operations
                
                // Unknown operations
                _ => 10.0,
            };
            weights.push(weight);
        }
        
        // Normalize weights to sum to 100%
        let total_weight: f32 = weights.iter().sum();
        if total_weight > 0.0 {
            for weight in &mut weights {
                *weight = (*weight / total_weight) * 100.0;
            }
        }
        
        weights
    }
    
    /// Execute pipeline without progress reporting (original method)
    pub fn execute(&self) -> std::io::Result<Vec<std::process::Output>> {
        let mut outputs = Vec::new();
        
        for (i, command) in self.commands.iter().enumerate() {
            let output = command.execute()?;
            
            // Check if command failed (non-zero exit code)
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                
                // Clean up temp files before returning error
                self.cleanup_temp_files();
                
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Stage {} ({}) failed with exit code {:?}.\nStderr: {}\nStdout: {}\nCommand: {} {}",
                           i + 1,
                           command.program,
                           output.status.code(),
                           stderr.trim(),
                           stdout.trim(),
                           command.program,
                           command.arguments.join(" ")
                    )
                ));
            }
            
            outputs.push(output);
        }
        
        // Clean up temporary files
        self.cleanup_temp_files();
        
        Ok(outputs)
    }
    
    /// Clean up temporary files
    fn cleanup_temp_files(&self) {
        for temp_file in &self.temp_files {
            if let Err(e) = std::fs::remove_file(temp_file) {
                log::warn!("Failed to cleanup temp file {}: {}", temp_file, e);
            }
        }
    }
    
    /// Estimate total duration for the pipeline
    pub fn estimate_total_duration(&mut self, input_path: &Path) -> Result<DurationEstimate> {
        let mut total_duration = std::time::Duration::from_secs(0);
        let mut min_confidence = 1.0f32;
        let mut estimation_methods = Vec::new();
        
        for command in &mut self.commands {
            match command.estimate_duration(input_path) {
                Ok(estimate) => {
                    total_duration += estimate.total_duration;
                    min_confidence = min_confidence.min(estimate.confidence);
                    estimation_methods.push(estimate.method);
                }
                Err(_) => {
                    // Fallback duration for failed estimates
                    total_duration += std::time::Duration::from_secs(30);
                    min_confidence = min_confidence.min(0.1);
                    estimation_methods.push(EstimationMethod::Fallback { base_seconds: 30 });
                }
            }
        }
        
        // Update pipeline's expected duration
        self.expected_duration = Some(total_duration);
        
        Ok(DurationEstimate {
            total_duration,
            confidence: min_confidence,
            method: EstimationMethod::AudioMetadata { 
                source_duration: total_duration,
                complexity_factor: 1.0, // Already calculated per command
            },
        })
    }
}