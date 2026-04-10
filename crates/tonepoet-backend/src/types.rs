//! Common types for the conversion backend

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use std::path::Path;

/// Audio formats supported
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum AudioFormat {
    Flac,
    Wav,
    Aiff,
    WavPack,
    Mp3,
    Aac,
    Opus,
    Alac,
}

impl AudioFormat {
    /// Check if format supports floating point samples
    pub fn supports_float(&self) -> bool {
        match self {
            AudioFormat::Wav => true,
            AudioFormat::Flac => false,
            AudioFormat::Aiff => false, // AIFF-C supports float, but standard AIFF doesn't
            AudioFormat::WavPack => true,
            AudioFormat::Mp3 => false,
            AudioFormat::Aac => false,
            AudioFormat::Opus => false,
            AudioFormat::Alac => false,
        }
    }
    
    /// Get file extension
    pub fn extension(&self) -> &str {
        match self {
            AudioFormat::Flac => "flac",
            AudioFormat::Wav => "wav",
            AudioFormat::Aiff => "aiff",
            AudioFormat::WavPack => "wv",
            AudioFormat::Mp3 => "mp3",
            AudioFormat::Aac => "m4a",
            AudioFormat::Opus => "opus",
            AudioFormat::Alac => "m4a",
        }
    }
}

/// Dithering algorithms
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum DitherType {
    None,
    Tpdf,           // Triangular PDF
    Shibata,        // Noise shaping
    LowShibata,     // Low-frequency optimized
    HighShibata,    // High-frequency optimized
    FShaped,        // F-weighted noise shaping
    ModifiedE,      // Modified E-weighted
    ImprovedE,      // Improved E-weighted
    Gesemann,       // Gesemann dithering (SoX only)
}

/// Nyquist filter transition bands
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum NyquistTransition {
    Sharp,    // Steep rolloff, more aliasing
    Medium,   // Balanced
    Gentle,   // Gradual rolloff, less aliasing
    Steep,    // Alias for Sharp
    BrickWall, // Requires SSRC
}

/// Opus content types
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum OpusContentType {
    Music,
    Speech,
    Auto,
}

/// AAC profiles
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum AacProfile {
    LcAac,      // Low Complexity
    HeAac,      // High Efficiency (SBR)
    HeAacV2,    // HE-AAC v2 (SBR + PS)
    LdAac,      // Low Delay
}

/// ReplayGain modes
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ReplayGainMode {
    Track,
    Album,
    Both,
}

/// Conversion settings from the wizard
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionSettings {
    // Preset metadata
    pub name: Option<String>,
    pub version: Option<u32>,
    
    // Target format
    pub format: AudioFormat,
    pub selected_quality: Option<String>,  // "Maximum (Best compression)", "High", etc.
    
    // Audio parameters
    pub bit_depth: Option<u32>,      // 8, 16, 24, 32, 320 (float32), 0 (keep source)
    pub sample_rate: Option<u32>,    // Target sample rate (0 = keep source)

    // Source metadata (detected from input file)
    pub source_bit_depth: Option<u16>,   // Detected source bit depth (16, 24, 320=float)
    pub source_sample_rate: Option<u32>, // Detected source sample rate

    // Quality settings  
    pub resample_quality: Option<u8>, // 0-4: LQ, MQ, HQ, VHQ, Ultra
    pub compression_level: Option<u8>, // Format-specific compression
    
    // Processing options
    pub dither_type: Option<DitherType>,
    pub nyquist_transition: Option<NyquistTransition>,
    pub ssrc_insane_mode: Option<bool>,

    // Format-specific
    pub opus_content_type: Option<OpusContentType>,
    pub aac_profile: Option<AacProfile>,
    
    // MP3 specific
    pub mp3_bitrate: Option<u32>,     // CBR/ABR bitrate in kbps
    pub mp3_quality: Option<u8>,      // VBR quality (0-9, 0=best)
    pub mp3_mode: Option<Mp3Mode>,    // CBR, VBR, or ABR
    
    // Encoding verification
    pub verify_encoding: Option<bool>,
    pub store_md5: Option<bool>,
    
    // ReplayGain settings
    pub replaygain_mode: Option<ReplayGainMode>,
    
    // Post-processing file operations
    pub copy_files_enabled: Option<bool>,
    pub copy_files_extensions: Option<String>,  // "txt, cue, log, ..."
    pub copy_subdirectories_enabled: Option<bool>,
    pub copy_subdirectories: Option<String>,    // "*" or specific patterns
    pub merge_to_single: Option<bool>,
    pub reencode_flac: Option<bool>,

    // Lineage.txt metadata
    pub lineage_file_path: Option<std::path::PathBuf>,  // Path to Lineage.txt if exists

    // File handling
    pub overwrite: bool,
}

/// MP3 encoding modes
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Mp3Mode {
    Cbr,  // Constant bitrate
    Vbr,  // Variable bitrate  
    Abr,  // Average bitrate
}

/// Output of the command builder
#[derive(Debug, Clone)]
pub struct ConversionCommand {
    /// Program to execute ("ffmpeg" or "sox")
    pub program: String,
    
    /// Command-line arguments
    pub arguments: Vec<String>,
    
    /// Environment variables if needed
    pub environment: HashMap<String, String>,
    
    /// Expected duration for progress estimation
    pub expected_duration: Option<Duration>,
    
    /// Human-readable description of what this command does
    pub description: String,
}

/// Progress callback for real-time command execution updates
pub type ProgressCallback = Box<dyn Fn(f32) + Send + Sync>;

/// Result of duration estimation
#[derive(Debug, Clone)]
pub struct DurationEstimate {
    /// Estimated total duration for the command
    pub total_duration: Duration,
    /// Confidence level (0.0-1.0) in the estimate
    pub confidence: f32,
    /// Method used for estimation
    pub method: EstimationMethod,
}

/// Methods used for duration estimation
#[derive(Debug, Clone)]
pub enum EstimationMethod {
    /// Estimated from audio file metadata (duration * complexity factor)
    AudioMetadata { source_duration: Duration, complexity_factor: f32 },
    /// Fixed estimate based on file size
    FileSize { file_size: u64, processing_speed: f64 },
    /// Fallback estimate when metadata unavailable
    Fallback { base_seconds: u64 },
}

impl ConversionCommand {
    /// Get the full command as a string (for logging)
    pub fn to_string(&self) -> String {
        format!("{} {}", self.program, self.arguments.join(" "))
    }
    
    /// Execute the command (original method without progress reporting)
    pub fn execute(&self) -> std::io::Result<std::process::Output> {
        // Use original simple execution for performance
        let mut cmd = std::process::Command::new(&self.program);
        cmd.args(&self.arguments);
        
        for (key, value) in &self.environment {
            cmd.env(key, value);
        }
        
        cmd.output()
    }
    
    /// Execute command with optional timeout (in seconds)
    pub fn execute_with_timeout(&self, timeout_secs: Option<u64>) -> std::io::Result<std::process::Output> {
        let mut cmd = std::process::Command::new(&self.program);
        cmd.args(&self.arguments);
        
        for (key, value) in &self.environment {
            cmd.env(key, value);
        }
        
        // Set timeout - default to appropriate time for each tool type
        let timeout = timeout_secs.unwrap_or(
            match self.program.as_str() {
                "ssrc" => 1200, // 20 minutes for SSRC brick wall resampling (very slow)
                "flac" | "metaflac" | "loudgain" => 120, // 2 minutes for simple tools
                "7z" => 600, // 10 minutes for archive extraction
                _ => 300, // 5 minutes for other complex operations
            }
        );
        
        // Configure child process with piped outputs
        cmd.stdout(std::process::Stdio::piped())
           .stderr(std::process::Stdio::piped());
        
        // Spawn child process
        let mut child = cmd.spawn()?;
        
        // Use a simple timeout mechanism
        let start_time = std::time::Instant::now();
        let timeout_duration = std::time::Duration::from_secs(timeout);
        
        loop {
            match child.try_wait()? {
                Some(_status) => {
                    // Process completed - get the full output
                    return child.wait_with_output();
                }
                None => {
                    // Process still running - check timeout
                    if start_time.elapsed() > timeout_duration {
                        // Kill the process
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!("Command '{}' timed out after {} seconds. Args: {:?}", 
                                   self.program, timeout, self.arguments)
                        ));
                    }
                    
                    // Sleep briefly before checking again
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
    }
    
    /// Execute the command with progress reporting
    pub fn execute_with_progress(
        &self,
        progress_callback: Option<ProgressCallback>
    ) -> std::io::Result<std::process::Output> {
        // If no progress callback provided, use simple execution
        let callback = match progress_callback {
            Some(cb) => cb,
            None => return self.execute(),
        };

        // Determine if we can parse real-time progress from this tool
        match self.program.as_str() {
            "ffmpeg" => self.execute_ffmpeg_with_progress(callback),
            "sox" => self.execute_sox_with_progress(callback),
            "ssrc" => self.execute_with_proportion_progress(callback, "SSRC resampling"),
            "flac" => self.execute_with_proportion_progress(callback, "FLAC encoding"),
            "metaflac" => self.execute_with_proportion_progress(callback, "ReplayGain analysis"),
            "loudgain" => self.execute_with_proportion_progress(callback, "ReplayGain analysis"),
            _ => self.execute_with_estimated_progress(callback),
        }
    }

    /// Execute ffmpeg with real-time progress parsing
    fn execute_ffmpeg_with_progress(
        &self,
        callback: ProgressCallback
    ) -> std::io::Result<std::process::Output> {
        // Modify ffmpeg command to output progress
        let mut cmd = std::process::Command::new(&self.program);
        
        // Add progress output to ffmpeg command
        let mut args = vec!["-progress".to_string(), "pipe:1".to_string()];
        args.extend(self.arguments.iter().cloned());
        cmd.args(&args);
        
        for (key, value) in &self.environment {
            cmd.env(key, value);
        }
        
        // Spawn with piped outputs
        cmd.stdout(std::process::Stdio::piped())
           .stderr(std::process::Stdio::piped());
        
        let mut child = cmd.spawn()?;
        
        // Get expected duration for progress calculation
        let total_duration = self.expected_duration
            .map(|d| d.as_secs_f64())
            .unwrap_or(1.0); // Fallback to prevent division by zero
        
        // Monitor stdout for progress updates
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let callback_clone = std::sync::Arc::new(callback);
        let progress_callback = std::sync::Arc::clone(&callback_clone);
        
        let progress_handle = std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            let reader = BufReader::new(stdout);
            let mut last_progress = 0.0f32;
            
            for line in reader.lines() {
                if let Ok(line) = line {
                    if let Some(progress) = Self::parse_ffmpeg_progress(&line, total_duration) {
                        // Only send updates when progress changes significantly
                        if (progress - last_progress).abs() > 0.5 {
                            progress_callback(progress);
                            last_progress = progress;
                        }
                    }
                }
            }
        });
        
        // Wait for PROCESS completion (not wait_with_output which consumes stdio)
        let status = child.wait()?;
        
        // Collect stderr separately (stdout was consumed by our thread)
        let stderr_data = {
            use std::io::{Read, BufReader};
            let mut stderr_buffer = Vec::new();
            BufReader::new(stderr).read_to_end(&mut stderr_buffer)?;
            stderr_buffer
        };
        
        // Wait for progress parsing to complete
        progress_handle.join().ok();
        
        // Send final 100% progress
        callback_clone(100.0);
        
        // Reconstruct Output (stdout was consumed by our parsing)
        Ok(std::process::Output {
            status,
            stdout: Vec::new(), // We consumed this
            stderr: stderr_data,
        })
    }

    /// Parse ffmpeg progress output line
    pub fn parse_ffmpeg_progress(line: &str, total_duration_seconds: f64) -> Option<f32> {
        // Look for "out_time=" lines
        if line.starts_with("out_time=") && !line.contains("N/A") {
            let time_str = &line[9..]; // Skip "out_time="
            
            // Parse time format: 00:01:23.456789 or just seconds
            if let Some(seconds) = Self::parse_time_to_seconds(time_str) {
                let progress = (seconds / total_duration_seconds * 100.0).min(99.0);
                return Some(progress as f32);
            }
        }
        None
    }
    
    /// Parse Sox progress output line
    pub fn parse_sox_progress(line: &str) -> Option<f32> {
        // Look for "In:XX.X%" pattern in Sox output
        // Sox can output multiple progress updates on one line, so find the LAST one
        if line.contains("In:") && line.contains("%") {
            // Find all "In:X.X%" patterns and take the last one
            let mut last_progress = None;
            
            let mut search_start = 0;
            while let Some(start) = line[search_start..].find("In:") {
                let actual_start = search_start + start;
                let remaining = &line[actual_start + 3..];
                if let Some(end) = remaining.find('%') {
                    let progress_str = &remaining[..end];
                    if let Ok(progress) = progress_str.parse::<f32>() {
                        last_progress = Some(progress.min(99.0)); // Cap at 99% until completion
                    }
                }
                search_start = actual_start + 3; // Continue searching after this match
            }
            
            return last_progress;
        }
        None
    }
    
    /// Parse time string to seconds (handles HH:MM:SS.mmm or just seconds)
    pub fn parse_time_to_seconds(time_str: &str) -> Option<f64> {
        if time_str.contains(':') {
            // Format: HH:MM:SS.mmm
            let parts: Vec<&str> = time_str.split(':').collect();
            if parts.len() >= 3 {
                if let (Ok(hours), Ok(minutes), Ok(seconds)) = (
                    parts[0].parse::<f64>(),
                    parts[1].parse::<f64>(),
                    parts[2].parse::<f64>(),
                ) {
                    return Some(hours * 3600.0 + minutes * 60.0 + seconds);
                }
            }
            None
        } else {
            // Just seconds as float
            time_str.parse::<f64>().ok()
        }
    }

    /// Execute sox with progress parsing
    fn execute_sox_with_progress(
        &self,
        callback: ProgressCallback
    ) -> std::io::Result<std::process::Output> {
        // Modify sox command to show progress
        let mut cmd = std::process::Command::new(&self.program);
        
        // Add progress flag to sox command
        let mut args = vec!["-S".to_string()]; // Show progress
        args.extend(self.arguments.iter().cloned());
        cmd.args(&args);
        
        for (key, value) in &self.environment {
            cmd.env(key, value);
        }
        
        // Spawn with piped outputs
        cmd.stdout(std::process::Stdio::piped())
           .stderr(std::process::Stdio::piped());
        
        let mut child = cmd.spawn()?;
        
        // Monitor stderr for Sox progress updates (Sox outputs progress to stderr)
        let stderr = child.stderr.take().unwrap();
        let callback_clone = std::sync::Arc::new(callback);
        let progress_callback = std::sync::Arc::clone(&callback_clone);
        let stop_signal = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_stop_signal = std::sync::Arc::clone(&stop_signal);
        
        let progress_handle = std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            let reader = BufReader::new(stderr);
            let mut last_progress = 0.0f32;
            
            for line in reader.lines() {
                if thread_stop_signal.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                
                if let Ok(line) = line {
                    if let Some(progress) = Self::parse_sox_progress(&line) {
                        // Only send updates when progress changes significantly
                        if (progress - last_progress).abs() > 0.5 {
                            progress_callback(progress);
                            last_progress = progress;
                        }
                    }
                }
            }
        });
        
        // Wait for command completion
        let output = child.wait_with_output()?;
        
        // Signal the progress thread to stop
        stop_signal.store(true, std::sync::atomic::Ordering::Relaxed);
        
        // Wait for progress parsing to complete
        progress_handle.join().ok();
        
        // Send final 100% progress
        callback_clone(100.0);
        
        Ok(output)
    }

    /// Execute with proportion-of-total-work progress (for tools without progress output)
    fn execute_with_proportion_progress(
        &self,
        callback: ProgressCallback,
        _operation_name: &str,
    ) -> std::io::Result<std::process::Output> {
        let mut cmd = std::process::Command::new(&self.program);
        cmd.args(&self.arguments);
        
        for (key, value) in &self.environment {
            cmd.env(key, value);
        }
        
        // Use spawn for progress updates
        cmd.stdout(std::process::Stdio::piped())
           .stderr(std::process::Stdio::piped());
        
        let child = cmd.spawn()?;
        
        // Get expected duration for this operation
        let expected_duration = self.expected_duration.unwrap_or(std::time::Duration::from_secs(30));
        let start_time = std::time::Instant::now();
        
        // Create progress callback and stop signal
        let callback_clone = std::sync::Arc::new(callback);
        let progress_callback = std::sync::Arc::clone(&callback_clone);
        let stop_signal = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_stop_signal = std::sync::Arc::clone(&stop_signal);
        
        // Start proportion-based progress updates
        let progress_handle = std::thread::spawn(move || {
            let mut last_progress = 0.0f32;
            
            while !thread_stop_signal.load(std::sync::atomic::Ordering::Relaxed) {
                let elapsed = start_time.elapsed();
                
                // Calculate progress as proportion of expected time
                let time_ratio = elapsed.as_secs_f64() / expected_duration.as_secs_f64();
                
                // Use a smooth curve that reaches ~90% at expected duration
                let progress = if time_ratio <= 1.0 {
                    (time_ratio * 90.0) as f32 // Linear up to 90%
                } else {
                    // Slow approach to 95% if operation takes longer than expected
                    (90.0 + ((time_ratio - 1.0) * 5.0).min(5.0)) as f32
                };
                
                // Send updates every few percent
                if (progress - last_progress).abs() > 2.0 {
                    progress_callback(progress);
                    last_progress = progress;
                }
                
                std::thread::sleep(std::time::Duration::from_millis(1000)); // Update every second
            }
        });
        
        // Wait for command completion
        let output = child.wait_with_output()?;
        
        // Signal the progress thread to stop
        stop_signal.store(true, std::sync::atomic::Ordering::Relaxed);
        
        // Send final 100% progress
        callback_clone(100.0);
        
        // Clean up progress thread
        progress_handle.join().ok();
        
        Ok(output)
    }

    /// Execute with estimated progress (fallback for tools without progress output)
    fn execute_with_estimated_progress(
        &self,
        callback: ProgressCallback
    ) -> std::io::Result<std::process::Output> {
        let mut cmd = std::process::Command::new(&self.program);
        cmd.args(&self.arguments);
        
        for (key, value) in &self.environment {
            cmd.env(key, value);
        }
        
        // Use spawn for real-time progress updates
        cmd.stdout(std::process::Stdio::piped())
           .stderr(std::process::Stdio::piped());
        
        let child = cmd.spawn()?;
        
        // Start progress monitoring in background thread
        let duration = self.expected_duration.unwrap_or(Duration::from_secs(10));
        let start_time = std::time::Instant::now();
        
        // Create a shared callback for the thread and a stop signal
        let callback_clone = std::sync::Arc::new(callback);
        let thread_callback = std::sync::Arc::clone(&callback_clone);
        let stop_signal = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_stop_signal = std::sync::Arc::clone(&stop_signal);
        
        // Spawn progress updates in a separate thread
        let progress_handle = std::thread::spawn(move || {
            let mut last_progress = 0.0f32;
            
            while !thread_stop_signal.load(std::sync::atomic::Ordering::Relaxed) {
                let elapsed = start_time.elapsed();
                
                // Use S-curve (sigmoid) for smooth progress
                let time_ratio = elapsed.as_secs_f64() / duration.as_secs_f64();
                let sigmoid = 1.0 / (1.0 + (-6.0 * (time_ratio - 0.5)).exp());
                let current_progress = (sigmoid * 95.0) as f32; // Cap at 95% until completion
                
                // Only send updates when progress changes significantly
                if (current_progress - last_progress).abs() > 1.0 {
                    thread_callback(current_progress);
                    last_progress = current_progress;
                }
                
                std::thread::sleep(Duration::from_millis(500));
            }
        });
        
        // Wait for command completion
        let output = child.wait_with_output()?;
        
        // Signal the progress thread to stop
        stop_signal.store(true, std::sync::atomic::Ordering::Relaxed);
        
        // Send final 100% progress
        callback_clone(100.0);
        
        // Clean up progress thread
        progress_handle.join().ok();
        
        Ok(output)
    }
    
    /// Estimate duration for this command based on input file
    pub fn estimate_duration(&mut self, input_path: &Path) -> Result<DurationEstimate, crate::ConversionError> {
        // Try to get audio metadata first
        if let Ok(metadata) = Self::get_audio_metadata(input_path) {
            if let Some(duration) = metadata.duration {
                let complexity_factor = self.calculate_complexity_factor();
                let estimated_duration = Duration::from_secs_f64(duration * complexity_factor as f64);
                
                let estimate = DurationEstimate {
                    total_duration: estimated_duration,
                    confidence: 0.8, // High confidence with audio metadata
                    method: EstimationMethod::AudioMetadata { 
                        source_duration: Duration::from_secs_f64(duration),
                        complexity_factor 
                    },
                };
                
                // Update the command's expected_duration
                self.expected_duration = Some(estimated_duration);
                
                return Ok(estimate);
            }
        }
        
        // Fallback to file size estimation
        if let Ok(file_metadata) = std::fs::metadata(input_path) {
            let file_size = file_metadata.len();
            // Rough estimate: 1MB per second processing speed for audio
            let processing_speed = 1_000_000.0; // bytes per second
            let estimated_seconds = (file_size as f64 / processing_speed).max(5.0).min(300.0);
            let estimated_duration = Duration::from_secs(estimated_seconds as u64);
            
            let estimate = DurationEstimate {
                total_duration: estimated_duration,
                confidence: 0.4, // Medium confidence with file size
                method: EstimationMethod::FileSize { file_size, processing_speed },
            };
            
            self.expected_duration = Some(estimated_duration);
            
            return Ok(estimate);
        }
        
        // Final fallback
        let fallback_duration = Duration::from_secs(30);
        let estimate = DurationEstimate {
            total_duration: fallback_duration,
            confidence: 0.1, // Low confidence
            method: EstimationMethod::Fallback { base_seconds: 30 },
        };
        
        self.expected_duration = Some(fallback_duration);
        
        Ok(estimate)
    }
    
    /// Calculate complexity factor based on command arguments
    pub fn calculate_complexity_factor(&self) -> f32 {
        let mut factor = 1.0;
        
        // Check for resampling (increases time)
        if self.arguments.iter().any(|arg| arg.contains("aresample") || arg.contains("rate")) {
            factor *= 1.5;
        }
        
        // Check for high-quality resampling (increases time more)
        if self.arguments.iter().any(|arg| arg.contains("precision=32") || arg.contains("-v")) {
            factor *= 2.0;
        }
        
        // Check for dithering
        if self.arguments.iter().any(|arg| arg.contains("dither")) {
            factor *= 1.2;
        }
        
        // Check for complex formats (encoding complexity)
        if self.arguments.iter().any(|arg| arg.contains("libmp3lame") || arg.contains("libopus")) {
            factor *= 1.3;
        }
        
        // Check for SSRC (brick wall filtering is slow)
        if self.program == "ssrc" {
            factor *= 3.0;
        }
        
        factor
    }
    
    /// Get audio metadata using ffprobe
    pub fn get_audio_metadata(input_path: &Path) -> Result<AudioMetadata, std::io::Error> {
        let output = std::process::Command::new("ffprobe")
            .args(&[
                "-v", "quiet",
                "-print_format", "json",
                "-show_format",
                "-show_streams",
                input_path.to_string_lossy().as_ref(),
            ])
            .output()?;
        
        if !output.status.success() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "ffprobe failed"
            ));
        }
        
        // Parse JSON output to extract duration
        let json_str = String::from_utf8_lossy(&output.stdout);
        Self::parse_ffprobe_json(&json_str)
    }
    
    /// Parse ffprobe JSON output to extract audio metadata
    fn parse_ffprobe_json(json: &str) -> Result<AudioMetadata, std::io::Error> {
        // Parse JSON properly using serde_json
        let parsed: serde_json::Value = serde_json::from_str(json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        
        // Try to extract duration from format section
        let duration = parsed
            .get("format")
            .and_then(|format| format.get("duration"))
            .and_then(|dur| {
                if let Some(dur_str) = dur.as_str() {
                    dur_str.parse::<f64>().ok()
                } else if let Some(dur_f64) = dur.as_f64() {
                    Some(dur_f64)
                } else {
                    None
                }
            });
        
        Ok(AudioMetadata { duration })
    }
}

/// Simple audio metadata structure
#[derive(Debug)]
pub struct AudioMetadata {
    pub duration: Option<f64>, // Duration in seconds
}

impl Default for ConversionSettings {
    fn default() -> Self {
        Self {
            name: None,
            version: None,
            format: AudioFormat::Flac,
            selected_quality: None,
            bit_depth: None,
            sample_rate: None,
            source_bit_depth: None,
            source_sample_rate: None,
            resample_quality: None,
            compression_level: None,
            dither_type: None,
            nyquist_transition: None,
            opus_content_type: None,
            aac_profile: None,
            mp3_bitrate: None,
            mp3_quality: None,
            mp3_mode: None,
            verify_encoding: None,
            store_md5: None,
            replaygain_mode: None,
            copy_files_enabled: None,
            copy_files_extensions: None,
            copy_subdirectories_enabled: None,
            copy_subdirectories: None,
            merge_to_single: None,
            reencode_flac: None,
            ssrc_insane_mode: None,
            lineage_file_path: None,
            overwrite: false,
        }
    }
}