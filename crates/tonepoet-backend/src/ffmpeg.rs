//! FFmpeg command builder

use crate::{ConversionCommand, ConversionSettings, Result};
use crate::types::*;
use crate::mapping;
use std::path::Path;
use std::collections::HashMap;

pub struct FFmpegBuilder;

impl FFmpegBuilder {
    pub fn new() -> Self {
        Self
    }
    
    pub fn build(
        &self,
        input: &Path,
        output: &Path,
        settings: &ConversionSettings
    ) -> Result<ConversionCommand> {
        let mut args = Vec::new();
        
        // Prevent hanging on user input
        args.push("-nostdin".to_string());
        
        // Input file
        args.push("-i".to_string());
        args.push(input.to_string_lossy().to_string());
        
        // Build audio codec and format settings
        self.add_codec_settings(&mut args, settings)?;
        
        // Build audio filters if needed (resampling, etc.)
        let filters = self.build_audio_filters(settings)?;
        if !filters.is_empty() {
            args.push("-af".to_string());
            args.push(filters.join(","));
        }
        
        // Add format-specific options
        self.add_format_options(&mut args, settings)?;
        
        // Overwrite flag
        // Always overwrite for conversion operations
        args.push("-y".to_string());
        
        // Output file
        args.push(output.to_string_lossy().to_string());
        
        // Build description
        let description = format!(
            "Convert to {} with ffmpeg{}{}",
            settings.format.extension(),
            if let Some(rate) = settings.sample_rate {
                if rate > 0 { format!(" @ {}Hz", rate) } else { String::new() }
            } else {
                String::new()
            },
            if let Some(depth) = settings.bit_depth {
                if depth == 33 {
                    " (32-bit float)".to_string()
                } else if depth > 0 {
                    format!(" ({}-bit)", depth)
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        );
        
        Ok(ConversionCommand {
            program: "ffmpeg".to_string(),
            arguments: args,
            environment: HashMap::new(),
            expected_duration: None,
            description,
        })
    }
    
    fn add_codec_settings(&self, args: &mut Vec<String>, settings: &ConversionSettings) -> Result<()> {
        let _codec = match settings.format {
            AudioFormat::Flac => {
                args.push("-c:a".to_string());
                args.push("flac".to_string());
                
                // Compression level
                if let Some(level) = settings.compression_level {
                    args.push("-compression_level".to_string());
                    args.push(level.to_string());
                }
                return Ok(());
            }
            
            AudioFormat::Wav | AudioFormat::Aiff => {
                // Only set codec if bit depth is explicitly specified
                // None means "preserve source", but FFmpeg defaults to 16-bit
                // So we need to detect source or use a sensible default
                if let Some(depth) = settings.bit_depth {
                    if depth > 0 {
                        let is_big_endian = settings.format == AudioFormat::Aiff;
                        let codec = mapping::get_pcm_codec(settings.bit_depth, is_big_endian, settings.format)?;
                        args.push("-c:a".to_string());
                        args.push(codec);
                    }
                    // If depth == 0, don't specify codec (let FFmpeg decide, though it will default to 16-bit)
                }
                // If None, don't specify codec at all
                return Ok(());
            }
            
            AudioFormat::Mp3 => {
                args.push("-map_metadata".to_string());
                args.push("0".to_string());

                args.push("-c:a".to_string());
                args.push("libmp3lame".to_string());

                // Bitrate settings
                match settings.mp3_mode {
                    Some(Mp3Mode::Cbr) => {
                        if let Some(bitrate) = settings.mp3_bitrate {
                            args.push("-b:a".to_string());
                            args.push(format!("{}k", bitrate));
                        }
                    }
                    Some(Mp3Mode::Vbr) => {
                        if let Some(quality) = settings.mp3_quality {
                            args.push("-q:a".to_string());
                            args.push(quality.to_string());
                        }
                    }
                    Some(Mp3Mode::Abr) => {
                        if let Some(bitrate) = settings.mp3_bitrate {
                            args.push("-b:a".to_string());
                            args.push(format!("{}k", bitrate));
                            args.push("-abr".to_string());
                            args.push("1".to_string());
                        }
                    }
                    None => {}
                }
                return Ok(());
            }
            
            AudioFormat::Aac => {
                args.push("-map_metadata".to_string());
                args.push("0".to_string());

                // Skip video streams (album art) to avoid codec issues with M4A container
                args.push("-vn".to_string());

                args.push("-c:a".to_string());

                // Choose encoder based on profile
                let encoder = match settings.aac_profile {
                    Some(AacProfile::HeAac) | Some(AacProfile::HeAacV2) => "libfdk_aac",
                    _ => "aac", // LC and LD can use built-in encoder
                };
                args.push(encoder.to_string());

                // AAC profile
                if let Some(profile) = settings.aac_profile {
                    args.push("-profile:a".to_string());
                    args.push(mapping::get_aac_profile_string(profile));
                }

                // Bitrate
                if let Some(bitrate) = settings.mp3_bitrate { // Reusing mp3_bitrate for AAC
                    args.push("-b:a".to_string());
                    args.push(format!("{}k", bitrate));
                }
                return Ok(());
            }
            
            AudioFormat::Opus => {
                args.push("-c:a".to_string());
                args.push("libopus".to_string());
                
                // Opus content type
                if let Some(content) = settings.opus_content_type {
                    args.push("-application".to_string());
                    args.push(mapping::get_opus_application(content));
                }
                
                // Bitrate
                if let Some(bitrate) = settings.mp3_bitrate { // Reusing mp3_bitrate for Opus
                    args.push("-b:a".to_string());
                    args.push(format!("{}k", bitrate));
                }
                return Ok(());
            }
            
            AudioFormat::WavPack => {
                args.push("-c:a".to_string());
                args.push("wavpack".to_string());
                
                // Compression level
                if let Some(level) = settings.compression_level {
                    args.push("-compression_level".to_string());
                    args.push(level.to_string());
                }
                return Ok(());
            }
        };
        
        // Remove unreachable code
        // Ok(())
    }
    
    fn build_audio_filters(&self, settings: &ConversionSettings) -> Result<Vec<String>> {
        let mut filters = Vec::new();
        
        // Resampling filter
        if let Some(sample_rate) = settings.sample_rate {
            if sample_rate > 0 {
                // Build resampler with SoXR
                let mut resample_opts = vec![
                    "aresample=resampler=soxr".to_string(),
                    format!("out_sample_rate={}", sample_rate),
                ];
                
                // Map resample quality to SoXR precision
                let precision = mapping::get_soxr_precision(settings.resample_quality);
                resample_opts.push(format!("precision={}", precision));
                
                // Add dithering if specified
                if let Some(dither) = settings.dither_type {
                    if dither != DitherType::None {
                        let dither_method = mapping::get_soxr_dither(dither);
                        resample_opts.push(format!("dither_method={}", dither_method));
                    }
                }

                // Add Nyquist filter cutoff if specified
                if let Some(transition) = settings.nyquist_transition {
                    let cutoff = mapping::get_ffmpeg_cutoff(transition);
                    resample_opts.push(format!("cutoff={}", cutoff));
                }

                filters.push(resample_opts.join(":"));
            }
        }
        
        Ok(filters)
    }
    
    fn add_format_options(&self, args: &mut Vec<String>, settings: &ConversionSettings) -> Result<()> {
        // Add any format-specific options that aren't handled elsewhere
        
        // MD5 verification for FLAC
        if settings.format == AudioFormat::Flac {
            if let Some(true) = settings.store_md5 {
                args.push("-write_id3v2".to_string());
                args.push("1".to_string());
            }
        }
        
        Ok(())
    }
}