//! Sox command builder

use crate::mapping;
use crate::types::*;
use crate::{ConversionCommand, ConversionSettings, Result};
use std::collections::HashMap;
use std::path::Path;

pub struct SoxBuilder;

impl SoxBuilder {
    pub fn new() -> Self {
        Self
    }

    pub fn build(
        &self,
        input: &Path,
        output: &Path,
        settings: &ConversionSettings,
    ) -> Result<ConversionCommand> {
        let mut args = Vec::new();

        // Input file
        args.push(input.to_string_lossy().to_string());

        // Output format options (before output file)
        self.add_output_format(&mut args, settings)?;

        // Output file
        args.push(output.to_string_lossy().to_string());

        // Effects chain (after output file)
        self.add_effects(&mut args, settings)?;

        // Build description
        let description = format!(
            "Convert to {} with sox{}{}",
            settings.format.extension(),
            if let Some(rate) = settings.sample_rate {
                if rate > 0 {
                    format!(" @ {}Hz", rate)
                } else {
                    String::new()
                }
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
            program: "sox".to_string(),
            arguments: args,
            environment: HashMap::new(),
            expected_duration: None,
            description,
        })
    }

    fn add_output_format(
        &self,
        args: &mut Vec<String>,
        settings: &ConversionSettings,
    ) -> Result<()> {
        // Bit depth and encoding
        if let Some(depth) = settings.bit_depth {
            if depth > 0 {
                args.push("-b".to_string());
                if depth == 320 || depth == 33 {
                    // Float encoding (320 new convention, 33 legacy compatibility)
                    args.push("32".to_string());
                    args.push("-e".to_string());
                    args.push("float".to_string());
                } else {
                    args.push(depth.to_string());
                }
            }
        }

        // Sample rate (if specified before resampling)
        // Sox handles this differently - rate conversion goes in effects

        // Format-specific encoding options
        match settings.format {
            AudioFormat::Mp3 => {
                // MP3 compression settings
                args.push("-C".to_string());
                match settings.mp3_mode {
                    Some(Mp3Mode::Cbr) => {
                        if let Some(bitrate) = settings.mp3_bitrate {
                            args.push(bitrate.to_string());
                        }
                    }
                    Some(Mp3Mode::Vbr) => {
                        if let Some(quality) = settings.mp3_quality {
                            // Sox uses negative values for VBR
                            args.push(format!("-{}", quality));
                        }
                    }
                    Some(Mp3Mode::Abr) => {
                        if let Some(bitrate) = settings.mp3_bitrate {
                            // Sox uses ~ prefix for ABR
                            args.push(format!("~{}", bitrate));
                        }
                    }
                    None => {}
                }
            }
            AudioFormat::Flac => {
                // FLAC compression level
                if let Some(level) = settings.compression_level {
                    args.push("-C".to_string());
                    args.push(level.to_string());
                }
            }
            AudioFormat::Opus => {
                // Opus bitrate
                if let Some(bitrate) = settings.mp3_bitrate {
                    args.push("-C".to_string());
                    args.push(bitrate.to_string());
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn add_effects(&self, args: &mut Vec<String>, settings: &ConversionSettings) -> Result<()> {
        let mut _has_effects = false;

        // Resampling
        if let Some(sample_rate) = settings.sample_rate {
            if sample_rate > 0 {
                args.push("rate".to_string());

                // Add quality flag
                let quality_flag = mapping::get_sox_resample_flag(settings.resample_quality);
                args.push(quality_flag.to_string());

                // Add sample rate
                args.push(sample_rate.to_string());

                // Add rolloff if specified
                if let Some(transition) = settings.nyquist_transition {
                    let rolloff = mapping::get_sox_rolloff(Some(transition));
                    args.push(rolloff.to_string());
                }

                _has_effects = true;
            }
        }

        // Dithering (must come after rate conversion)
        if let Some(dither) = settings.dither_type {
            if dither != DitherType::None {
                let dither_args = mapping::get_sox_dither_args(dither);
                for arg in dither_args {
                    args.push(arg);
                }
                _has_effects = true;
            }
        }

        Ok(())
    }
}
