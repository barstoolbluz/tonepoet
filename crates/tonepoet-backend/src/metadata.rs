//! FLAC metadata preservation for multi-stage pipelines
//!
//! This module handles extracting, preserving, and reapplying FLAC vorbis comments
//! through multi-stage audio processing pipelines that use intermediate WAV files.

use crate::{Result, ConversionError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::fs;

/// FLAC metadata extracted as vorbis comments
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlacMetadata {
    /// Standard vorbis comment fields
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub albumartist: Option<String>,
    pub track: Option<String>,
    pub tracktotal: Option<String>,
    pub disc: Option<String>,
    pub disctotal: Option<String>,
    pub date: Option<String>,
    pub year: Option<String>,
    pub genre: Option<String>,
    pub comment: Option<String>,
    
    /// Technical fields
    pub encoder: Option<String>,
    pub encoding: Option<String>,
    
    /// Custom/additional fields not covered above
    pub custom_fields: HashMap<String, String>,
    
    /// Preserved for validation
    pub original_field_count: usize,
    pub source_file: PathBuf,
}

impl FlacMetadata {
    /// Create empty metadata structure
    pub fn new(source_file: PathBuf) -> Self {
        Self {
            title: None,
            artist: None,
            album: None,
            albumartist: None,
            track: None,
            tracktotal: None,
            disc: None,
            disctotal: None,
            date: None,
            year: None,
            genre: None,
            comment: None,
            encoder: None,
            encoding: None,
            custom_fields: HashMap::new(),
            original_field_count: 0,
            source_file,
        }
    }
    
    /// Get total field count (standard + custom)
    pub fn total_field_count(&self) -> usize {
        let standard_count = [
            &self.title, &self.artist, &self.album, &self.albumartist,
            &self.track, &self.tracktotal, &self.disc, &self.disctotal,
            &self.date, &self.year, &self.genre, &self.comment,
            &self.encoder, &self.encoding
        ].iter().filter(|field| field.is_some()).count();
        
        standard_count + self.custom_fields.len()
    }
    
    /// Validate metadata integrity
    pub fn validate(&self) -> Result<()> {
        if self.total_field_count() == 0 {
            return Err(ConversionError::InvalidSettings(
                "Metadata is empty - extraction may have failed".to_string()
            ));
        }
        
        // Warn if significant field loss
        if self.original_field_count > 0 && self.total_field_count() < self.original_field_count / 2 {
            log::warn!(
                "Significant metadata loss: {} original fields → {} preserved", 
                self.original_field_count, 
                self.total_field_count()
            );
        }
        
        Ok(())
    }
}

/// FLAC metadata extractor
pub struct FlacMetadataExtractor;

impl FlacMetadataExtractor {
    pub fn new() -> Self {
        Self
    }
    
    /// Extract all vorbis comments from a FLAC file
    pub fn extract(&self, flac_file: &Path) -> Result<FlacMetadata> {
        if !flac_file.exists() {
            return Err(ConversionError::InvalidSettings(
                format!("FLAC file does not exist: {}", flac_file.display())
            ));
        }
        
        // Verify it's actually a FLAC file
        if !flac_file.extension().map_or(false, |ext| ext == "flac") {
            return Err(ConversionError::InvalidSettings(
                format!("File is not a FLAC file: {}", flac_file.display())
            ));
        }
        
        let mut metadata = FlacMetadata::new(flac_file.to_path_buf());
        
        // Use metaflac to export all tags
        let output = Command::new("metaflac")
            .arg("--export-tags-to=-")
            .arg(flac_file)
            .output()
            .map_err(|e| ConversionError::BackendUnavailable(
                format!("Failed to run metaflac: {}", e)
            ))?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ConversionError::InvalidSettings(
                format!("metaflac failed: {}", stderr)
            ));
        }
        
        let tags_text = String::from_utf8_lossy(&output.stdout);
        
        // Parse vorbis comment format: FIELD=VALUE
        let mut field_count = 0;
        for line in tags_text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            
            if let Some((key, value)) = line.split_once('=') {
                field_count += 1;
                let key_lower = key.to_lowercase();
                let value = value.to_string();
                
                // Map to standard fields (case-insensitive)
                match key_lower.as_str() {
                    "title" => metadata.title = Some(value),
                    "artist" => metadata.artist = Some(value),
                    "album" => metadata.album = Some(value),
                    "albumartist" | "album_artist" => metadata.albumartist = Some(value),
                    "track" | "tracknumber" => metadata.track = Some(value),
                    "tracktotal" => metadata.tracktotal = Some(value),
                    "disc" | "discnumber" => metadata.disc = Some(value),
                    "disctotal" => metadata.disctotal = Some(value),
                    "date" => metadata.date = Some(value),
                    "year" => metadata.year = Some(value),
                    "genre" => metadata.genre = Some(value),
                    "comment" => metadata.comment = Some(value),
                    "encoder" => metadata.encoder = Some(value),
                    "encoding" => metadata.encoding = Some(value),
                    _ => {
                        // Store non-standard fields as custom
                        metadata.custom_fields.insert(key.to_string(), value);
                    }
                }
            } else {
                log::warn!("Invalid vorbis comment line: {}", line);
            }
        }
        
        metadata.original_field_count = field_count;
        
        // Validate extraction succeeded
        metadata.validate()?;
        
        log::info!("Extracted {} metadata fields from {}", 
                   metadata.total_field_count(), 
                   flac_file.display());
        
        Ok(metadata)
    }
}

/// FLAC metadata applier
pub struct FlacMetadataApplier;

impl FlacMetadataApplier {
    pub fn new() -> Self {
        Self
    }
    
    /// Apply vorbis comments to a FLAC file
    pub fn apply(&self, metadata: &FlacMetadata, flac_file: &Path) -> Result<()> {
        if !flac_file.exists() {
            return Err(ConversionError::InvalidSettings(
                format!("FLAC file does not exist: {}", flac_file.display())
            ));
        }
        
        // Generate vorbis comment text
        let mut tags_text = String::new();
        
        // Add standard fields
        if let Some(ref title) = metadata.title {
            tags_text.push_str(&format!("TITLE={}\n", title));
        }
        if let Some(ref artist) = metadata.artist {
            tags_text.push_str(&format!("ARTIST={}\n", artist));
        }
        if let Some(ref album) = metadata.album {
            tags_text.push_str(&format!("ALBUM={}\n", album));
        }
        if let Some(ref albumartist) = metadata.albumartist {
            tags_text.push_str(&format!("ALBUMARTIST={}\n", albumartist));
        }
        if let Some(ref track) = metadata.track {
            tags_text.push_str(&format!("TRACK={}\n", track));
        }
        if let Some(ref tracktotal) = metadata.tracktotal {
            tags_text.push_str(&format!("TRACKTOTAL={}\n", tracktotal));
        }
        if let Some(ref disc) = metadata.disc {
            tags_text.push_str(&format!("DISC={}\n", disc));
        }
        if let Some(ref disctotal) = metadata.disctotal {
            tags_text.push_str(&format!("DISCTOTAL={}\n", disctotal));
        }
        if let Some(ref date) = metadata.date {
            tags_text.push_str(&format!("DATE={}\n", date));
        }
        if let Some(ref year) = metadata.year {
            tags_text.push_str(&format!("YEAR={}\n", year));
        }
        if let Some(ref genre) = metadata.genre {
            tags_text.push_str(&format!("GENRE={}\n", genre));
        }
        if let Some(ref comment) = metadata.comment {
            tags_text.push_str(&format!("COMMENT={}\n", comment));
        }
        if let Some(ref encoder) = metadata.encoder {
            tags_text.push_str(&format!("ENCODER={}\n", encoder));
        }
        if let Some(ref encoding) = metadata.encoding {
            tags_text.push_str(&format!("ENCODING={}\n", encoding));
        }
        
        // Add custom fields
        for (key, value) in &metadata.custom_fields {
            tags_text.push_str(&format!("{}={}\n", key, value));
        }
        
        if tags_text.is_empty() {
            log::warn!("No metadata to apply to {}", flac_file.display());
            return Ok(());
        }
        
        // Apply tags using metaflac
        let mut cmd = Command::new("metaflac");
        cmd.arg("--remove-all-tags")
           .arg("--import-tags-from=-")
           .arg(flac_file);
        
        let mut child = cmd.stdin(std::process::Stdio::piped())
                           .stdout(std::process::Stdio::piped())
                           .stderr(std::process::Stdio::piped())
                           .spawn()
                           .map_err(|e| ConversionError::BackendUnavailable(
                               format!("Failed to spawn metaflac: {}", e)
                           ))?;
        
        // Write tags to stdin
        if let Some(stdin) = child.stdin.take() {
            use std::io::Write;
            let mut stdin = stdin;
            stdin.write_all(tags_text.as_bytes())
                 .map_err(|e| ConversionError::InvalidSettings(
                     format!("Failed to write tags to metaflac: {}", e)
                 ))?;
        }
        
        let output = child.wait_with_output()
            .map_err(|e| ConversionError::BackendUnavailable(
                format!("Failed to wait for metaflac: {}", e)
            ))?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ConversionError::InvalidSettings(
                format!("metaflac tag application failed: {}", stderr)
            ));
        }
        
        log::info!("Applied {} metadata fields to {}", 
                   metadata.total_field_count(), 
                   flac_file.display());
        
        Ok(())
    }
    
    /// Verify metadata was applied correctly by re-extracting and comparing
    pub fn verify(&self, metadata: &FlacMetadata, flac_file: &Path) -> Result<bool> {
        let extractor = FlacMetadataExtractor::new();
        let reextracted = extractor.extract(flac_file)?;
        
        // Compare key fields with flexible field mapping
        let mut matches = 0;
        let mut total_checks = 0;
        
        // Check standard fields
        if metadata.title.is_some() {
            total_checks += 1;
            if metadata.title == reextracted.title {
                matches += 1;
            }
        }
        
        if metadata.artist.is_some() {
            total_checks += 1;
            if metadata.artist == reextracted.artist {
                matches += 1;
            }
        }
        
        if metadata.album.is_some() {
            total_checks += 1;
            if metadata.album == reextracted.album {
                matches += 1;
            }
        }
        
        // Handle comment vs DESCRIPTION field mapping
        let original_comment = metadata.comment.as_ref()
            .or_else(|| metadata.custom_fields.get("DESCRIPTION"))
            .or_else(|| metadata.custom_fields.get("description"));
            
        let reextracted_comment = reextracted.comment.as_ref()
            .or_else(|| reextracted.custom_fields.get("DESCRIPTION"))
            .or_else(|| reextracted.custom_fields.get("description"));
        
        if original_comment.is_some() {
            total_checks += 1;
            if original_comment == reextracted_comment {
                matches += 1;
            }
        }
        
        // Check overall field count (allow for ReplayGain additions)
        let original_count = metadata.total_field_count();
        let reextracted_count = reextracted.total_field_count();
        
        if reextracted_count >= original_count {
            total_checks += 1;
            matches += 1; // Field count maintained or increased
        }
        
        // Require at least 80% of checks to pass
        let success_rate = if total_checks > 0 { 
            matches as f64 / total_checks as f64 
        } else { 
            1.0 
        };
        
        let success = success_rate >= 0.8;
        
        if !success {
            log::warn!("FLAC metadata verification failed for {}: {}/{} checks passed ({:.1}%)", 
                       flac_file.display(), matches, total_checks, success_rate * 100.0);
        } else {
            log::info!("FLAC metadata verification passed: {}/{} checks passed ({:.1}%)", 
                       matches, total_checks, success_rate * 100.0);
        }
        
        Ok(success)
    }
}

/// Metadata-preserving wrapper around existing ConversionPipeline
pub struct MetadataPreservingPipeline {
    flac_extractor: FlacMetadataExtractor,
    flac_applier: FlacMetadataApplier,
    wv_extractor: WavPackMetadataExtractor,
    wv_applier: WavPackMetadataApplier,
    opus_extractor: OpusMetadataExtractor,
    opus_applier: OpusMetadataApplier,
    aac_extractor: AacMetadataExtractor,
    aac_applier: AacMetadataApplier,
    metadata_file: Option<PathBuf>,
}

impl MetadataPreservingPipeline {
    pub fn new() -> Self {
        Self {
            flac_extractor: FlacMetadataExtractor::new(),
            flac_applier: FlacMetadataApplier::new(),
            wv_extractor: WavPackMetadataExtractor::new(),
            wv_applier: WavPackMetadataApplier::new(),
            opus_extractor: OpusMetadataExtractor::new(),
            opus_applier: OpusMetadataApplier::new(),
            aac_extractor: AacMetadataExtractor::new(),
            aac_applier: AacMetadataApplier::new(),
            metadata_file: None,
        }
    }
    
    /// Execute a conversion pipeline with metadata preservation
    pub fn execute_with_metadata_preservation(
        &mut self,
        pipeline: &crate::ConversionPipeline,
        input: &Path,
        output: &Path,
    ) -> Result<()> {
        // Phase 1: Extract metadata (FLAC, WavPack, Opus, or AAC/M4A inputs)
        let metadata = if input.extension().map_or(false, |ext| ext == "flac") {
            log::info!("Extracting metadata from FLAC input: {}", input.display());
            Some(self.flac_extractor.extract(input)?)
        } else if input.extension().map_or(false, |ext| ext == "wv") {
            log::info!("Extracting metadata from WavPack input: {}", input.display());
            Some(self.wv_extractor.extract(input)?)
        } else if input.extension().map_or(false, |ext| ext == "opus") {
            log::info!("Extracting metadata from Opus input: {}", input.display());
            Some(self.opus_extractor.extract(input)?)
        } else if input.extension().map_or(false, |ext| ext == "m4a") {
            log::info!("Extracting metadata from AAC/M4A input: {}", input.display());
            Some(self.aac_extractor.extract(input)?)
        } else {
            log::info!("Unsupported input format for metadata extraction, skipping");
            None
        };
        
        // Save metadata to temporary JSON file if extracted
        if let Some(ref metadata) = metadata {
            let metadata_file = output.with_extension("metadata.json");
            let json_content = serde_json::to_string_pretty(metadata)
                .map_err(|e| ConversionError::InvalidSettings(
                    format!("Failed to serialize metadata: {}", e)
                ))?;
            
            fs::write(&metadata_file, json_content)
                .map_err(|e| ConversionError::InvalidSettings(
                    format!("Failed to write metadata file: {}", e)
                ))?;
            
            self.metadata_file = Some(metadata_file);
            log::info!("Saved metadata to {}", self.metadata_file.as_ref().unwrap().display());
        }
        
        // Phase 2: Execute audio processing pipeline (existing logic)
        log::info!("Executing audio pipeline...");
        for (i, command) in pipeline.commands.iter().enumerate() {
            log::info!("Pipeline stage {}/{}: {}", i + 1, pipeline.commands.len(), command.description);
            
            let output = command.execute()
                .map_err(|e| ConversionError::InvalidSettings(
                    format!("Pipeline stage {} failed: {}", i + 1, e)
                ))?;
            
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(ConversionError::InvalidSettings(
                    format!("Pipeline stage {} failed: {}", i + 1, stderr)
                ));
            }
        }
        
        // Phase 3: Reapply metadata (FLAC, WavPack, or Opus outputs)
        if let Some(metadata) = metadata {
            if output.extension().map_or(false, |ext| ext == "flac") {
                log::info!("Reapplying metadata to FLAC output: {}", output.display());
                self.flac_applier.apply(&metadata, output)?;
                
                // Verify metadata was applied correctly
                if !self.flac_applier.verify(&metadata, output)? {
                    log::error!("FLAC metadata verification failed for {}", output.display());
                    return Err(ConversionError::InvalidSettings(
                        "FLAC metadata verification failed after application".to_string()
                    ));
                }
                
                log::info!("FLAC metadata preservation completed successfully");
            } else if output.extension().map_or(false, |ext| ext == "wv") {
                log::info!("Reapplying metadata to WavPack output: {}", output.display());
                self.wv_applier.apply(&metadata, output)?;
                
                // Verify metadata was applied correctly
                if !self.wv_applier.verify(&metadata, output)? {
                    log::error!("WavPack metadata verification failed for {}", output.display());
                    return Err(ConversionError::InvalidSettings(
                        "WavPack metadata verification failed after application".to_string()
                    ));
                }
                
                log::info!("WavPack metadata preservation completed successfully");
            } else if output.extension().map_or(false, |ext| ext == "opus") {
                log::info!("Reapplying metadata to Opus output: {}", output.display());
                self.opus_applier.apply(&metadata, output)?;
                
                // Verify metadata was applied correctly
                if !self.opus_applier.verify(&metadata, output)? {
                    log::error!("Opus metadata verification failed for {}", output.display());
                    return Err(ConversionError::InvalidSettings(
                        "Opus metadata verification failed after application".to_string()
                    ));
                }
                
                log::info!("Opus metadata preservation completed successfully");
            } else if output.extension().map_or(false, |ext| ext == "m4a") {
                log::info!("Reapplying metadata to AAC/M4A output: {}", output.display());
                self.aac_applier.apply(&metadata, output)?;

                log::info!("AAC/M4A metadata preservation completed successfully");
            } else {
                log::warn!("Supported input format but unsupported output format for metadata preservation");
            }
        }
        
        // Cleanup temporary metadata file
        if let Some(ref metadata_file) = self.metadata_file {
            if metadata_file.exists() {
                let _ = fs::remove_file(metadata_file);
            }
        }
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    
    #[test]
    fn test_metadata_extraction_real_file() {
        // This test requires a real FLAC file
        let test_file = Path::new("./space/testes/3ds - Beautiful Things (1993) [FLAC] {7-inch  24-96} [PBThal]/01 - Beautiful Things.flac");
        
        if !test_file.exists() {
            println!("Skipping test - no real FLAC file available");
            return;
        }
        
        let extractor = FlacMetadataExtractor::new();
        let metadata = extractor.extract(test_file).expect("Failed to extract metadata");
        
        // Verify we got reasonable metadata
        assert!(metadata.total_field_count() > 0, "No metadata extracted");
        assert!(metadata.title.is_some(), "Missing title field");
        
        println!("Extracted metadata:");
        println!("  Title: {:?}", metadata.title);
        println!("  Artist: {:?}", metadata.artist);
        println!("  Comment: {:?}", metadata.comment);
        println!("  Total fields: {}", metadata.total_field_count());
    }
    
    #[test]
    fn test_metadata_round_trip() {
        let test_file = Path::new("./space/testes/3ds - Beautiful Things (1993) [FLAC] {7-inch  24-96} [PBThal]/01 - Beautiful Things.flac");
        
        if !test_file.exists() {
            println!("Skipping test - no real FLAC file available");
            return;
        }
        
        let temp_dir = tempdir().expect("Failed to create temp dir");
        let temp_flac = temp_dir.path().join("test_copy.flac");
        
        // Copy original file
        fs::copy(test_file, &temp_flac).expect("Failed to copy file");
        
        let extractor = FlacMetadataExtractor::new();
        let applier = FlacMetadataApplier::new();
        
        // Extract metadata
        let metadata = extractor.extract(&temp_flac).expect("Failed to extract metadata");
        let original_count = metadata.total_field_count();
        
        // Remove all tags
        let output = Command::new("metaflac")
            .arg("--remove-all-tags")
            .arg(&temp_flac)
            .output()
            .expect("Failed to remove tags");
        
        assert!(output.status.success(), "Failed to remove tags");
        
        // Verify tags were removed (expect extraction to succeed but return empty metadata)
        let empty_metadata = match extractor.extract(&temp_flac) {
            Ok(metadata) => metadata,
            Err(_) => {
                // Empty metadata after tag removal is expected - create dummy
                FlacMetadata::new(temp_flac.clone())
            }
        };
        assert_eq!(empty_metadata.total_field_count(), 0, "Tags not fully removed");
        
        // Reapply metadata
        applier.apply(&metadata, &temp_flac).expect("Failed to apply metadata");
        
        // Verify restoration
        assert!(applier.verify(&metadata, &temp_flac).expect("Verification failed"), 
                "Metadata round-trip failed");
        
        let restored_metadata = extractor.extract(&temp_flac).expect("Failed to extract restored metadata");
        assert_eq!(restored_metadata.total_field_count(), original_count, 
                   "Field count mismatch after round-trip");
        
        println!("Round-trip test passed: {} fields preserved", original_count);
    }
}

/// WavPack metadata extractor using FFmpeg
pub struct WavPackMetadataExtractor;

impl WavPackMetadataExtractor {
    pub fn new() -> Self {
        Self
    }
    
    /// Extract metadata from WavPack file using FFmpeg JSON output
    pub fn extract(&self, wv_file: &Path) -> Result<FlacMetadata> {
        if !wv_file.exists() {
            return Err(ConversionError::InvalidSettings(
                format!("WavPack file does not exist: {}", wv_file.display())
            ));
        }
        
        if !wv_file.extension().map_or(false, |ext| ext == "wv") {
            return Err(ConversionError::InvalidSettings(
                format!("File is not a WavPack file: {}", wv_file.display())
            ));
        }
        
        // Use FFmpeg to extract metadata as JSON
        let output = Command::new("ffprobe")
            .arg("-v").arg("error")
            .arg("-print_format").arg("json")
            .arg("-show_entries").arg("format_tags")
            .arg(wv_file)
            .output()
            .map_err(|e| ConversionError::BackendUnavailable(
                format!("Failed to run ffprobe: {}", e)
            ))?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ConversionError::InvalidSettings(
                format!("ffprobe failed: {}", stderr)
            ));
        }
        
        let json_text = String::from_utf8_lossy(&output.stdout);
        
        // Parse FFmpeg JSON output
        #[derive(serde::Deserialize)]
        struct FFprobeOutput {
            format: Option<FFprobeFormat>,
        }
        
        #[derive(serde::Deserialize)]
        struct FFprobeFormat {
            tags: Option<std::collections::HashMap<String, String>>,
        }
        
        let ffprobe_data: FFprobeOutput = serde_json::from_str(&json_text)
            .map_err(|e| ConversionError::InvalidSettings(
                format!("Failed to parse ffprobe JSON: {}", e)
            ))?;
        
        let mut metadata = FlacMetadata::new(wv_file.to_path_buf());
        
        if let Some(format) = ffprobe_data.format {
            if let Some(tags) = format.tags {
                let mut field_count = 0;
                
                for (key, value) in tags {
                    field_count += 1;
                    let key_lower = key.to_lowercase();
                    
                    // Map FFmpeg tag names to our standard fields (case-insensitive)
                    match key_lower.as_str() {
                        "title" => metadata.title = Some(value),
                        "artist" => metadata.artist = Some(value),
                        "album" => metadata.album = Some(value),
                        "albumartist" | "album_artist" => metadata.albumartist = Some(value),
                        "track" | "tracknumber" => metadata.track = Some(value),
                        "tracktotal" => metadata.tracktotal = Some(value),
                        "disc" | "discnumber" => metadata.disc = Some(value),
                        "disctotal" => metadata.disctotal = Some(value),
                        "date" => metadata.date = Some(value),
                        "year" => metadata.year = Some(value),
                        "genre" => metadata.genre = Some(value),
                        "comment" | "description" => metadata.comment = Some(value), // Handle both
                        "encoder" => metadata.encoder = Some(value),
                        "encoding" => metadata.encoding = Some(value),
                        _ => {
                            // Store non-standard fields as custom
                            metadata.custom_fields.insert(key, value);
                        }
                    }
                }
                
                metadata.original_field_count = field_count;
            }
        }
        
        log::info!("Extracted {} WavPack metadata fields from {}", 
                   metadata.total_field_count(), 
                   wv_file.display());
        
        Ok(metadata)
    }
}

/// WavPack metadata applier using FFmpeg
pub struct WavPackMetadataApplier;

impl WavPackMetadataApplier {
    pub fn new() -> Self {
        Self
    }
    
    /// Apply metadata to WavPack file using FFmpeg
    pub fn apply(&self, metadata: &FlacMetadata, wv_file: &Path) -> Result<()> {
        if !wv_file.exists() {
            return Err(ConversionError::InvalidSettings(
                format!("WavPack file does not exist: {}", wv_file.display())
            ));
        }
        
        // Create temporary copy for metadata application
        let temp_file = wv_file.with_file_name(
            format!("{}_temp.wv", wv_file.file_stem().unwrap().to_string_lossy())
        );
        
        // Build FFmpeg metadata arguments
        let mut args = vec![
            "-nostdin".to_string(),
            "-i".to_string(),
            wv_file.to_string_lossy().to_string(),
            "-c".to_string(),
            "copy".to_string(), // Copy streams without re-encoding
            "-f".to_string(),
            "wv".to_string(), // Explicitly specify WavPack format
        ];
        
        // Add metadata arguments
        if let Some(ref title) = metadata.title {
            args.push("-metadata".to_string());
            args.push(format!("title={}", title));
        }
        if let Some(ref artist) = metadata.artist {
            args.push("-metadata".to_string());
            args.push(format!("artist={}", artist));
        }
        if let Some(ref album) = metadata.album {
            args.push("-metadata".to_string());
            args.push(format!("album={}", album));
        }
        if let Some(ref albumartist) = metadata.albumartist {
            args.push("-metadata".to_string());
            args.push(format!("album_artist={}", albumartist));
        }
        if let Some(ref track) = metadata.track {
            args.push("-metadata".to_string());
            args.push(format!("track={}", track));
        }
        if let Some(ref date) = metadata.date {
            args.push("-metadata".to_string());
            args.push(format!("date={}", date));
        }
        if let Some(ref genre) = metadata.genre {
            args.push("-metadata".to_string());
            args.push(format!("genre={}", genre));
        }
        if let Some(ref comment) = metadata.comment {
            args.push("-metadata".to_string());
            args.push(format!("comment={}", comment));
        }
        
        // Add custom fields
        for (key, value) in &metadata.custom_fields {
            args.push("-metadata".to_string());
            args.push(format!("{}={}", key, value));
        }
        
        // Output file
        args.push("-y".to_string());
        args.push(temp_file.to_string_lossy().to_string());
        
        // Execute FFmpeg metadata application
        let output = Command::new("ffmpeg")
            .args(&args)
            .output()
            .map_err(|e| ConversionError::BackendUnavailable(
                format!("Failed to run ffmpeg for WavPack metadata: {}", e)
            ))?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Clean up temp file
            let _ = fs::remove_file(&temp_file);
            return Err(ConversionError::InvalidSettings(
                format!("FFmpeg WavPack metadata application failed: {}", stderr)
            ));
        }
        
        // Replace original with tagged version
        fs::rename(&temp_file, wv_file)
            .map_err(|e| ConversionError::InvalidSettings(
                format!("Failed to replace WavPack file with tagged version: {}", e)
            ))?;
        
        log::info!("Applied {} metadata fields to WavPack file {}", 
                   metadata.total_field_count(), 
                   wv_file.display());
        
        Ok(())
    }
    
    /// Verify WavPack metadata application
    pub fn verify(&self, metadata: &FlacMetadata, wv_file: &Path) -> Result<bool> {
        let extractor = WavPackMetadataExtractor::new();
        let reextracted = extractor.extract(wv_file)?;
        
        // Compare key fields with proper field mapping handling
        let mut matches = 0;
        let mut total_checks = 0;
        
        // Check standard fields
        if metadata.title.is_some() {
            total_checks += 1;
            if metadata.title == reextracted.title {
                matches += 1;
            }
        }
        
        if metadata.artist.is_some() {
            total_checks += 1;
            if metadata.artist == reextracted.artist {
                matches += 1;
            }
        }
        
        if metadata.album.is_some() {
            total_checks += 1;
            if metadata.album == reextracted.album {
                matches += 1;
            }
        }
        
        // Handle comment vs DESCRIPTION field mapping
        let original_comment = metadata.comment.as_ref()
            .or_else(|| metadata.custom_fields.get("DESCRIPTION"))
            .or_else(|| metadata.custom_fields.get("description"));
            
        let reextracted_comment = reextracted.comment.as_ref()
            .or_else(|| reextracted.custom_fields.get("DESCRIPTION"))
            .or_else(|| reextracted.custom_fields.get("description"));
        
        if original_comment.is_some() {
            total_checks += 1;
            if original_comment == reextracted_comment {
                matches += 1;
            }
        }
        
        // Require at least 80% of fields to match
        let success_rate = if total_checks > 0 { 
            matches as f64 / total_checks as f64 
        } else { 
            1.0 
        };
        
        let success = success_rate >= 0.8;
        
        if !success {
            log::warn!("WavPack metadata verification failed for {}: {}/{} fields matched ({:.1}%)", 
                       wv_file.display(), matches, total_checks, success_rate * 100.0);
        } else {
            log::info!("WavPack metadata verification passed: {}/{} fields matched ({:.1}%)", 
                       matches, total_checks, success_rate * 100.0);
        }
        
        Ok(success)
    }
}

/// Opus metadata extractor using FFmpeg
pub struct OpusMetadataExtractor;

impl OpusMetadataExtractor {
    pub fn new() -> Self {
        Self
    }
    
    /// Extract metadata from Opus file using opustags
    pub fn extract(&self, opus_file: &Path) -> Result<FlacMetadata> {
        if !opus_file.exists() {
            return Err(ConversionError::InvalidSettings(
                format!("Opus file does not exist: {}", opus_file.display())
            ));
        }
        
        if !opus_file.extension().map_or(false, |ext| ext == "opus") {
            return Err(ConversionError::InvalidSettings(
                format!("File is not an Opus file: {}", opus_file.display())
            ));
        }
        
        // Use opustags to extract vorbis comments (with full flox path)
        let opustags_path = std::env::var("FLOX_ENV_DIRS")
            .map(|env_paths| {
                // FLOX_ENV_DIRS can have multiple paths, use the first one that contains opustags
                for env_path in env_paths.split(':') {
                    let opustags_full = format!("{}/bin/opustags", env_path);
                    if Path::new(&opustags_full).exists() {
                        return opustags_full;
                    }
                }
                "opustags".to_string()
            })
            .unwrap_or_else(|_| "opustags".to_string());
            
        let output = Command::new(&opustags_path)
            .arg(opus_file)
            .output()
            .map_err(|e| ConversionError::BackendUnavailable(
                format!("Failed to run opustags: {}", e)
            ))?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ConversionError::InvalidSettings(
                format!("opustags failed: {}", stderr)
            ));
        }
        
        let tags_text = String::from_utf8_lossy(&output.stdout);
        
        let mut metadata = FlacMetadata::new(opus_file.to_path_buf());
        let mut field_count = 0;
        
        // Parse vorbis comment format: FIELD=VALUE (same as FLAC)
        for line in tags_text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            
            if let Some((key, value)) = line.split_once('=') {
                field_count += 1;
                let key_lower = key.to_lowercase();
                let value = value.to_string();
                
                // Map to standard fields (case-insensitive)
                match key_lower.as_str() {
                    "title" => metadata.title = Some(value),
                    "artist" => metadata.artist = Some(value),
                    "album" => metadata.album = Some(value),
                    "albumartist" | "album_artist" => metadata.albumartist = Some(value),
                    "track" | "tracknumber" => metadata.track = Some(value),
                    "tracktotal" => metadata.tracktotal = Some(value),
                    "disc" | "discnumber" => metadata.disc = Some(value),
                    "disctotal" => metadata.disctotal = Some(value),
                    "date" => metadata.date = Some(value),
                    "year" => metadata.year = Some(value),
                    "genre" => metadata.genre = Some(value),
                    "comment" | "description" => metadata.comment = Some(value),
                    "encoder" => metadata.encoder = Some(value),
                    "encoding" => metadata.encoding = Some(value),
                    _ => {
                        // Store non-standard fields as custom
                        metadata.custom_fields.insert(key.to_string(), value);
                    }
                }
            } else {
                log::warn!("Invalid vorbis comment line in Opus file: {}", line);
            }
        }
        
        metadata.original_field_count = field_count;
        
        // Validate extraction succeeded
        metadata.validate()?;
        
        log::info!("Extracted {} Opus metadata fields from {}", 
                   metadata.total_field_count(), 
                   opus_file.display());
        
        Ok(metadata)
    }
}

/// Opus metadata applier using opustags
pub struct OpusMetadataApplier;

impl OpusMetadataApplier {
    pub fn new() -> Self {
        Self
    }
    
    /// Apply metadata to Opus file using opustags
    pub fn apply(&self, metadata: &FlacMetadata, opus_file: &Path) -> Result<()> {
        if !opus_file.exists() {
            return Err(ConversionError::InvalidSettings(
                format!("Opus file does not exist: {}", opus_file.display())
            ));
        }
        
        // Generate vorbis comment text for opustags
        let mut tags_text = String::new();
        
        // Add standard fields
        if let Some(ref title) = metadata.title {
            tags_text.push_str(&format!("TITLE={}\n", title));
        }
        if let Some(ref artist) = metadata.artist {
            tags_text.push_str(&format!("ARTIST={}\n", artist));
        }
        if let Some(ref album) = metadata.album {
            tags_text.push_str(&format!("ALBUM={}\n", album));
        }
        if let Some(ref albumartist) = metadata.albumartist {
            tags_text.push_str(&format!("ALBUMARTIST={}\n", albumartist));
        }
        if let Some(ref track) = metadata.track {
            tags_text.push_str(&format!("TRACKNUMBER={}\n", track));
        }
        if let Some(ref tracktotal) = metadata.tracktotal {
            tags_text.push_str(&format!("TRACKTOTAL={}\n", tracktotal));
        }
        if let Some(ref disc) = metadata.disc {
            tags_text.push_str(&format!("DISCNUMBER={}\n", disc));
        }
        if let Some(ref disctotal) = metadata.disctotal {
            tags_text.push_str(&format!("DISCTOTAL={}\n", disctotal));
        }
        if let Some(ref date) = metadata.date {
            tags_text.push_str(&format!("DATE={}\n", date));
        }
        if let Some(ref year) = metadata.year {
            tags_text.push_str(&format!("YEAR={}\n", year));
        }
        if let Some(ref genre) = metadata.genre {
            tags_text.push_str(&format!("GENRE={}\n", genre));
        }
        if let Some(ref comment) = metadata.comment {
            tags_text.push_str(&format!("DESCRIPTION={}\n", comment)); // Opus uses DESCRIPTION for comments
        }
        if let Some(ref encoder) = metadata.encoder {
            tags_text.push_str(&format!("ENCODER={}\n", encoder));
        }
        if let Some(ref encoding) = metadata.encoding {
            tags_text.push_str(&format!("ENCODING={}\n", encoding));
        }
        
        // Add custom fields
        for (key, value) in &metadata.custom_fields {
            tags_text.push_str(&format!("{}={}\n", key, value));
        }
        
        if tags_text.is_empty() {
            log::warn!("No metadata to apply to {}", opus_file.display());
            return Ok(());
        }
        
        // Apply tags using opustags --delete-all --set-all (with full flox path)
        let opustags_path = std::env::var("FLOX_ENV_DIRS")
            .map(|env_paths| {
                // FLOX_ENV_DIRS can have multiple paths, use the first one that contains opustags
                for env_path in env_paths.split(':') {
                    let opustags_full = format!("{}/bin/opustags", env_path);
                    if Path::new(&opustags_full).exists() {
                        return opustags_full;
                    }
                }
                "opustags".to_string()
            })
            .unwrap_or_else(|_| "opustags".to_string());
            
        let mut cmd = Command::new(&opustags_path);
        cmd.arg("--delete-all")
           .arg("--set-all")
           .arg("--in-place")
           .arg(opus_file);
        
        let mut child = cmd.stdin(std::process::Stdio::piped())
                           .stdout(std::process::Stdio::piped())
                           .stderr(std::process::Stdio::piped())
                           .spawn()
                           .map_err(|e| ConversionError::BackendUnavailable(
                               format!("Failed to spawn opustags: {}", e)
                           ))?;
        
        // Write tags to stdin
        if let Some(stdin) = child.stdin.take() {
            use std::io::Write;
            let mut stdin = stdin;
            stdin.write_all(tags_text.as_bytes())
                 .map_err(|e| ConversionError::InvalidSettings(
                     format!("Failed to write tags to opustags: {}", e)
                 ))?;
        }
        
        let output = child.wait_with_output()
            .map_err(|e| ConversionError::BackendUnavailable(
                format!("Failed to wait for opustags: {}", e)
            ))?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ConversionError::InvalidSettings(
                format!("opustags tag application failed: {}", stderr)
            ));
        }
        
        log::info!("Applied {} metadata fields to Opus file {}", 
                   metadata.total_field_count(), 
                   opus_file.display());
        
        Ok(())
    }
    
    /// Verify Opus metadata application
    pub fn verify(&self, metadata: &FlacMetadata, opus_file: &Path) -> Result<bool> {
        let extractor = OpusMetadataExtractor::new();
        let reextracted = extractor.extract(opus_file)?;
        
        // Compare key fields with proper field mapping handling
        let mut matches = 0;
        let mut total_checks = 0;
        
        // Check standard fields
        if metadata.title.is_some() {
            total_checks += 1;
            if metadata.title == reextracted.title {
                matches += 1;
            }
        }
        
        if metadata.artist.is_some() {
            total_checks += 1;
            if metadata.artist == reextracted.artist {
                matches += 1;
            }
        }
        
        if metadata.album.is_some() {
            total_checks += 1;
            if metadata.album == reextracted.album {
                matches += 1;
            }
        }
        
        // Handle comment vs DESCRIPTION field mapping (Opus uses DESCRIPTION)
        let original_comment = metadata.comment.as_ref()
            .or_else(|| metadata.custom_fields.get("DESCRIPTION"))
            .or_else(|| metadata.custom_fields.get("description"));
            
        let reextracted_comment = reextracted.comment.as_ref()
            .or_else(|| reextracted.custom_fields.get("DESCRIPTION"))
            .or_else(|| reextracted.custom_fields.get("description"));
        
        if original_comment.is_some() {
            total_checks += 1;
            if original_comment == reextracted_comment {
                matches += 1;
            }
        }
        
        // Require at least 80% of fields to match
        let success_rate = if total_checks > 0 { 
            matches as f64 / total_checks as f64 
        } else { 
            1.0 
        };
        
        let success = success_rate >= 0.8;
        
        if !success {
            log::warn!("Opus metadata verification failed for {}: {}/{} fields matched ({:.1}%)",
                       opus_file.display(), matches, total_checks, success_rate * 100.0);
        } else {
            log::info!("Opus metadata verification passed: {}/{} fields matched ({:.1}%)",
                       matches, total_checks, success_rate * 100.0);
        }

        Ok(success)
    }
}

// ============================================================================
// AAC/M4A METADATA HANDLING (AtomicParsley-based)
// ============================================================================

/// AAC/M4A metadata extractor using AtomicParsley
pub struct AacMetadataExtractor;

impl AacMetadataExtractor {
    pub fn new() -> Self {
        Self
    }

    /// Find AtomicParsley binary in PATH or FLOX_ENV
    fn find_atomicparsley() -> Result<PathBuf> {
        // Try standard PATH lookup first
        if Command::new("AtomicParsley")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Ok(PathBuf::from("AtomicParsley"));
        }

        // Fallback to FLOX_ENV
        if let Ok(flox_env) = std::env::var("FLOX_ENV") {
            let ap_path = PathBuf::from(flox_env).join("bin/AtomicParsley");
            if ap_path.exists() {
                return Ok(ap_path);
            }
        }

        Err(ConversionError::BackendUnavailable(
            "AtomicParsley not found in PATH or FLOX_ENV".to_string()
        ))
    }

    /// Extract metadata from AAC/M4A file using AtomicParsley
    pub fn extract(&self, m4a_file: &Path) -> Result<FlacMetadata> {
        let ap_path = Self::find_atomicparsley()?;

        log::info!("Extracting AAC/M4A metadata from: {}", m4a_file.display());

        // Run AtomicParsley -t to list all atoms
        let output = Command::new(&ap_path)
            .arg(m4a_file)
            .arg("-t")
            .output()
            .map_err(|e| ConversionError::InvalidSettings(
                format!("Failed to run AtomicParsley: {}", e)
            ))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ConversionError::InvalidSettings(
                format!("AtomicParsley extraction failed: {}", stderr)
            ));
        }

        let tags_text = String::from_utf8_lossy(&output.stdout);
        let mut metadata = FlacMetadata::new(m4a_file.to_path_buf());
        let mut field_count = 0;

        // Parse AtomicParsley output
        // Format: Atom "©nam" contains: Value
        // Format: Atom "----" [com.apple.iTunes;field_name] contains: Value
        for line in tags_text.lines() {
            if !line.contains("Atom") || !line.contains("contains:") {
                continue;
            }

            // Extract atom name and value
            if let Some(atom_start) = line.find("Atom \"") {
                let after_atom = &line[atom_start + 6..];

                // Check if this is a reverse DNS atom (----)
                if after_atom.starts_with("----") {
                    // Parse reverse DNS atom: ----" [com.apple.iTunes;field_name] contains: value
                    if let Some(bracket_start) = after_atom.find("[com.apple.iTunes;") {
                        let after_bracket = &after_atom[bracket_start + 18..];
                        if let Some(bracket_end) = after_bracket.find(']') {
                            let field_name = &after_bracket[..bracket_end];
                            if let Some(value_start) = line.find("contains: ") {
                                let value = line[value_start + 10..].trim().to_string();
                                field_count += 1;

                                // Map ReplayGain reverse DNS atoms to uppercase custom_fields
                                // Handle both lowercase (proper format) and uppercase (loudgain format)
                                match field_name {
                                    "replaygain_track_gain" | "REPLAYGAIN_TRACK_GAIN" => {
                                        metadata.custom_fields.insert(
                                            "REPLAYGAIN_TRACK_GAIN".to_string(),
                                            value
                                        );
                                    }
                                    "replaygain_track_peak" | "REPLAYGAIN_TRACK_PEAK" => {
                                        metadata.custom_fields.insert(
                                            "REPLAYGAIN_TRACK_PEAK".to_string(),
                                            value
                                        );
                                    }
                                    "replaygain_album_gain" | "REPLAYGAIN_ALBUM_GAIN" => {
                                        metadata.custom_fields.insert(
                                            "REPLAYGAIN_ALBUM_GAIN".to_string(),
                                            value
                                        );
                                    }
                                    "replaygain_album_peak" | "REPLAYGAIN_ALBUM_PEAK" => {
                                        metadata.custom_fields.insert(
                                            "REPLAYGAIN_ALBUM_PEAK".to_string(),
                                            value
                                        );
                                    }
                                    _ => {
                                        // Store other reverse DNS atoms as-is
                                        metadata.custom_fields.insert(field_name.to_string(), value);
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // Standard atom
                    if let Some(quote_end) = after_atom.find('"') {
                        let atom_name = &after_atom[..quote_end];
                        if let Some(value_start) = line.find("contains: ") {
                            let value = line[value_start + 10..].trim().to_string();
                            field_count += 1;

                            // Map standard M4A atoms to FlacMetadata fields
                            match atom_name {
                                "©nam" => metadata.title = Some(value),
                                "©ART" => metadata.artist = Some(value),
                                "©alb" => metadata.album = Some(value),
                                "aART" => metadata.albumartist = Some(value),
                                "©day" => metadata.date = Some(value.clone()),
                                "©gen" | "gnre" => metadata.genre = Some(value),
                                "©cmt" => metadata.comment = Some(value),
                                "©too" => metadata.encoder = Some(value),
                                "trkn" => {
                                    // Format: "3 of 12"
                                    let parts: Vec<_> = value.split(" of ").collect();
                                    if let Some(num) = parts.first() {
                                        metadata.track = Some(num.to_string());
                                    }
                                    if let Some(total) = parts.get(1) {
                                        metadata.tracktotal = Some(total.to_string());
                                    }
                                }
                                "disk" => {
                                    // Format: "1 of 2"
                                    let parts: Vec<_> = value.split(" of ").collect();
                                    if let Some(num) = parts.first() {
                                        metadata.disc = Some(num.to_string());
                                    }
                                    if let Some(total) = parts.get(1) {
                                        metadata.disctotal = Some(total.to_string());
                                    }
                                }
                                _ => {
                                    // Unknown atom - store in custom fields
                                    metadata.custom_fields.insert(atom_name.to_string(), value);
                                }
                            }
                        }
                    }
                }
            }
        }

        metadata.original_field_count = field_count;
        log::info!("Extracted {} metadata fields from AAC/M4A file", field_count);

        Ok(metadata)
    }
}

/// AAC/M4A metadata applier using AtomicParsley
pub struct AacMetadataApplier;

impl AacMetadataApplier {
    pub fn new() -> Self {
        Self
    }

    /// Apply metadata to AAC/M4A file using AtomicParsley
    pub fn apply(&self, metadata: &FlacMetadata, m4a_file: &Path) -> Result<()> {
        let ap_path = AacMetadataExtractor::find_atomicparsley()?;

        log::info!("Applying metadata to AAC/M4A file: {}", m4a_file.display());

        let mut cmd = Command::new(&ap_path);
        cmd.arg(m4a_file);

        // Apply standard atoms
        if let Some(ref title) = metadata.title {
            cmd.arg("--title").arg(title);
        }
        if let Some(ref artist) = metadata.artist {
            cmd.arg("--artist").arg(artist);
        }
        if let Some(ref album) = metadata.album {
            cmd.arg("--album").arg(album);
        }
        if let Some(ref albumartist) = metadata.albumartist {
            cmd.arg("--albumArtist").arg(albumartist);
        }
        if let Some(ref date) = metadata.date {
            cmd.arg("--year").arg(date);
        } else if let Some(ref year) = metadata.year {
            cmd.arg("--year").arg(year);
        }
        if let Some(ref genre) = metadata.genre {
            cmd.arg("--genre").arg(genre);
        }
        if let Some(ref comment) = metadata.comment {
            cmd.arg("--comment").arg(comment);
        }

        // Track number (use "/" format: "3/12")
        if let (Some(ref track), Some(ref total)) = (&metadata.track, &metadata.tracktotal) {
            cmd.arg("--tracknum").arg(format!("{}/{}", track, total));
        } else if let Some(ref track) = metadata.track {
            cmd.arg("--tracknum").arg(track);
        }

        // Disc number (use "/" format: "1/2")
        if let (Some(ref disc), Some(ref total)) = (&metadata.disc, &metadata.disctotal) {
            cmd.arg("--disk").arg(format!("{}/{}", disc, total));
        } else if let Some(ref disc) = metadata.disc {
            cmd.arg("--disk").arg(disc);
        }

        // Apply ReplayGain as reverse DNS atoms
        // Map from uppercase custom_fields to lowercase reverse DNS names
        // First, remove any existing uppercase tags (from loudgain)
        let has_replaygain = metadata.custom_fields.contains_key("REPLAYGAIN_TRACK_GAIN") ||
                             metadata.custom_fields.contains_key("REPLAYGAIN_TRACK_PEAK") ||
                             metadata.custom_fields.contains_key("REPLAYGAIN_ALBUM_GAIN") ||
                             metadata.custom_fields.contains_key("REPLAYGAIN_ALBUM_PEAK");

        if has_replaygain {
            // Remove uppercase ReplayGain tags (from loudgain) by setting them to empty string
            cmd.arg("--rDNSatom").arg("").arg("name=REPLAYGAIN_TRACK_GAIN").arg("domain=com.apple.iTunes");
            cmd.arg("--rDNSatom").arg("").arg("name=REPLAYGAIN_TRACK_PEAK").arg("domain=com.apple.iTunes");
            cmd.arg("--rDNSatom").arg("").arg("name=REPLAYGAIN_ALBUM_GAIN").arg("domain=com.apple.iTunes");
            cmd.arg("--rDNSatom").arg("").arg("name=REPLAYGAIN_ALBUM_PEAK").arg("domain=com.apple.iTunes");
        }

        if let Some(ref track_gain) = metadata.custom_fields.get("REPLAYGAIN_TRACK_GAIN") {
            cmd.arg("--rDNSatom")
               .arg(track_gain)
               .arg("name=replaygain_track_gain")
               .arg("domain=com.apple.iTunes");
        }
        if let Some(ref track_peak) = metadata.custom_fields.get("REPLAYGAIN_TRACK_PEAK") {
            cmd.arg("--rDNSatom")
               .arg(track_peak)
               .arg("name=replaygain_track_peak")
               .arg("domain=com.apple.iTunes");
        }
        if let Some(ref album_gain) = metadata.custom_fields.get("REPLAYGAIN_ALBUM_GAIN") {
            cmd.arg("--rDNSatom")
               .arg(album_gain)
               .arg("name=replaygain_album_gain")
               .arg("domain=com.apple.iTunes");
        }
        if let Some(ref album_peak) = metadata.custom_fields.get("REPLAYGAIN_ALBUM_PEAK") {
            cmd.arg("--rDNSatom")
               .arg(album_peak)
               .arg("name=replaygain_album_peak")
               .arg("domain=com.apple.iTunes");
        }

        // Overwrite the file
        cmd.arg("--overWrite");

        // Execute
        let output = cmd.output()
            .map_err(|e| ConversionError::InvalidSettings(
                format!("Failed to run AtomicParsley: {}", e)
            ))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ConversionError::InvalidSettings(
                format!("AtomicParsley apply failed: {}", stderr)
            ));
        }

        log::info!("Successfully applied metadata to AAC/M4A file");
        Ok(())
    }
}