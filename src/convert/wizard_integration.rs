//! CORRECTED Integration bridge between tonepoet_wizard::SimpleWizard and ConversionManager
//!
//! This is the corrected version that uses the actual tonepoet_wizard::SimpleWizard
//! instead of the convert module's SimpleWizard

use crate::convert::formats::{
    AacProfile, AudioFormat, Mp3BitrateMode, QualitySettings, WavPackMode,
};
use crate::convert::simple_wizard::{DitherType, NyquistTransition};
use crate::convert::{ConversionManager, ConversionOptions, ConversionStatus};

/// Extract conversion settings from the tui_wizard_core wizard
///
/// NOTE: This assumes tonepoet_wizard::SimpleWizard has similar fields to
/// convert::simple_wizard::SimpleWizard. You may need to adjust based on
/// the actual structure of tonepoet_wizard::SimpleWizard.
pub fn extract_wizard_settings(
    wizard: &tonepoet_wizard::SimpleWizard,
) -> (AudioFormat, ConversionOptions) {
    // Convert from tonepoet_wizard::AudioFormat to our AudioFormat
    let format = match wizard.selected_format {
        Some(tonepoet_wizard::AudioFormat::Flac) => AudioFormat::Flac,
        Some(tonepoet_wizard::AudioFormat::Wav) => AudioFormat::Wav,
        Some(tonepoet_wizard::AudioFormat::Aiff) => AudioFormat::Aiff,
        Some(tonepoet_wizard::AudioFormat::Mp3) => AudioFormat::Mp3,
        Some(tonepoet_wizard::AudioFormat::Aac) => AudioFormat::Aac,
        Some(tonepoet_wizard::AudioFormat::Opus) => AudioFormat::Opus,
        Some(tonepoet_wizard::AudioFormat::WavPack) => AudioFormat::WavPack,
        None => AudioFormat::Flac, // Default to FLAC
    };

    // Extract quality string or use default
    let quality_str = wizard
        .selected_quality
        .as_ref()
        .map(|s| s.as_str())
        .unwrap_or("Default");

    // Build quality settings based on format and wizard selections
    let quality = match format {
        AudioFormat::Flac => {
            // Use compression level from wizard or default to 8 (best)
            let compression_level = wizard.compression_level.unwrap_or(8);
            QualitySettings::Flac { compression_level }
        }

        AudioFormat::Wav | AudioFormat::Aiff => {
            // Extract bit depth from wizard
            let bit_depth = match wizard.bit_depth {
                Some(16) => 16,
                Some(24) => 24,
                Some(32) => 32,
                Some(0) | None => 0, // 0 means "same as source", pass through to backend
                Some(other) => other as u16,
            };

            // Extract sample rate
            let sample_rate = wizard.sample_rate.unwrap_or(44100);

            if matches!(format, AudioFormat::Wav) {
                QualitySettings::Wav {
                    bit_depth,
                    sample_rate,
                }
            } else {
                QualitySettings::Aiff {
                    bit_depth,
                    sample_rate,
                }
            }
        }

        AudioFormat::Mp3 => {
            // Parse quality string to determine bitrate
            let (bitrate_mode, quality) = match quality_str {
                "128 kbps" => (Mp3BitrateMode::Cbr { bitrate: 128 }, 2),
                "192 kbps" => (Mp3BitrateMode::Cbr { bitrate: 192 }, 2),
                "256 kbps" => (Mp3BitrateMode::Cbr { bitrate: 256 }, 2),
                "320 kbps" => (Mp3BitrateMode::Cbr { bitrate: 320 }, 2),
                "VBR High" => (Mp3BitrateMode::Vbr { quality: 2 }, 2),
                "VBR Medium" => (Mp3BitrateMode::Vbr { quality: 4 }, 4),
                _ => (Mp3BitrateMode::Cbr { bitrate: 320 }, 2), // Default to highest quality
            };

            QualitySettings::Mp3 {
                bitrate_mode,
                quality,
            }
        }

        AudioFormat::Aac => {
            // Parse AAC bitrate
            let bitrate = match quality_str {
                "128 kbps" => 128,
                "192 kbps" => 192,
                "256 kbps" => 256,
                "320 kbps" => 320,
                _ => 256, // Default to high quality
            };

            QualitySettings::Aac {
                bitrate,
                profile: AacProfile::Lc, // Default to Low Complexity
            }
        }

        AudioFormat::Opus => {
            // Parse Opus quality (both descriptors and explicit bitrates)
            let bitrate = match quality_str {
                // Quality descriptors (from UI options)
                "Low" => 64,
                "Medium" => 128,
                "High" => 192,
                "Very High" => 256, // Target bitrate for "Very High" quality
                "Insane" => 320,
                // Explicit bitrates
                "64 kbps" => 64,
                "96 kbps" => 96,
                "128 kbps" => 128,
                "192 kbps" => 192,
                "256 kbps" => 256,
                "320 kbps" => 320,
                _ => 256, // Default to high quality (was 128)
            };

            QualitySettings::Opus {
                bitrate,
                complexity: 10, // Maximum quality
            }
        }

        AudioFormat::WavPack => {
            // Default WavPack settings
            QualitySettings::WavPack {
                compression_mode: WavPackMode::Normal,
                hybrid_mode: false,
                correction_file: false,
            }
        }

        AudioFormat::Alac => QualitySettings::Alac,
    };

    // Extract destination path from wizard
    let output_dir = match &wizard.destination_mode {
        tonepoet_wizard::DestinationMode::Custom(path) => Some(std::path::PathBuf::from(path)),
        tonepoet_wizard::DestinationMode::AskEveryTime => None,
    };

    // Build conversion options
    log::info!(
        "🎯 Wizard ReplayGain settings: calculate={:?}, mode={:?}",
        wizard.calculate_replaygain,
        wizard.replaygain_mode
    );

    let replaygain_mode = wizard.replaygain_mode.clone().and_then(|mode| {
        use crate::convert::simple_wizard::ReplayGainMode;
        let converted = match mode {
            tonepoet_wizard::ReplayGainMode::Track => Some(ReplayGainMode::Track),
            tonepoet_wizard::ReplayGainMode::Album => Some(ReplayGainMode::Album),
            tonepoet_wizard::ReplayGainMode::Both => Some(ReplayGainMode::Both),
            tonepoet_wizard::ReplayGainMode::Off => None,
        };
        log::info!("🎯 Converted wizard mode {:?} -> {:?}", mode, converted);
        converted
    });

    // CRITICAL FIX: If a ReplayGain mode is selected (not Off), enable calculation
    // The mode selection should override the calculate_replaygain checkbox
    let calculate_replaygain = replaygain_mode.is_some();
    log::info!(
        "🎯 Final calculate_replaygain={} (mode present: {})",
        calculate_replaygain,
        replaygain_mode.is_some()
    );

    // NOTE: The old tonepoet_wizard::SimpleWizard doesn't have backend selection
    // Backend selection is only available in tui_options_wizard (options mode)
    // So for this wizard, backend stays None (defaults to FFmpeg in conversion_backend)

    let options = ConversionOptions {
        output_format: format,
        quality,
        preserve_metadata: true, // Always preserve metadata
        calculate_replaygain,
        replaygain_mode,
        naming_template: None, // Use default naming
        overwrite: false,      // Don't overwrite by default
        output_dir,
        resample_quality: wizard.resample_quality, // Pass through resample quality (0-4)
        nyquist_transition: if matches!(
            format,
            AudioFormat::Flac
                | AudioFormat::Wav
                | AudioFormat::Aiff
                | AudioFormat::WavPack
                | AudioFormat::Opus
        ) {
            wizard.nyquist_transition.map(|nt| match nt {
                tonepoet_wizard::NyquistTransition::Gentle => NyquistTransition::Gentle,
                tonepoet_wizard::NyquistTransition::Steep => NyquistTransition::Steep,
                tonepoet_wizard::NyquistTransition::BrickWall => NyquistTransition::BrickWall,
            })
        } else {
            None // Clear for MP3/AAC - not exposed in UI
        },
        dither_type: if wizard.should_show_dithering() {
            wizard.dither_type.map(|dt| {
                use tonepoet_wizard::DitherType as WizDither;
                match dt {
                    WizDither::None => DitherType::None,
                    WizDither::Tpdf => DitherType::TPDF,
                    WizDither::Shibata => DitherType::Shibata,
                    WizDither::LowShibata => DitherType::LowShibata,
                    WizDither::HighShibata => DitherType::HighShibata,
                    WizDither::Gesemann => DitherType::Gesemann,
                    WizDither::SlopedTpdf => DitherType::SloppedTPDF,
                }
            })
        } else {
            None
        },
        target_sample_rate: wizard.sample_rate, // Extract sample rate for ALL formats
        target_bit_depth: wizard.bit_depth, // Extract bit depth for ALL formats (FLAC/WavPack need this)
        copy_auxiliary_files: wizard.copy_files_enabled,
        copy_subdirectories: wizard.copy_subdirectories_enabled,
        reencode_flac: wizard.get_effective_reencode_flac(),
        merge_to_single: wizard.merge_to_single.unwrap_or(false),
        preferred_backend: None, // Old wizard doesn't support backend selection
        original_settings: None, // Not from preset
        ssrc_insane_mode: wizard.ssrc_insane_mode,
        append_lineage_to_comment: false, // Set from app config, not wizard
        write_log_file: false,            // Set from app config, not wizard
        generate_cue_files: false,        // Set from app config, not wizard
        cue_generation_mode: "IfMerging".to_string(), // Set from app config, not wizard
    };

    (format, options)
}

/// Apply wizard settings to all items in the conversion queue
pub async fn apply_settings_to_queue(manager: &mut ConversionManager, options: ConversionOptions) {
    let mut queue = manager.queue.write().await;

    // Check if any items are selected
    let has_selection = queue.all_items().iter().any(|item| item.selected);
    let total_items = queue.all_items().len();
    let selected_count = queue
        .all_items()
        .iter()
        .filter(|item| item.selected)
        .count();

    log::info!(
        "apply_settings_to_queue: has_selection={}, total_items={}, selected_count={}",
        has_selection,
        total_items,
        selected_count
    );

    for item in queue.all_items_mut() {
        // If there are selected items, only apply to selected ones
        // Otherwise, apply to all items (legacy behavior)
        if has_selection && !item.selected {
            log::debug!("Skipping item {} (not selected)", item.id);
            continue;
        }

        // Only configure items that haven't started processing
        match item.status {
            ConversionStatus::NotConfigured
            | ConversionStatus::Queued
            | ConversionStatus::Paused => {
                log::info!(
                    "Applying settings to item {} (selected={})",
                    item.id,
                    item.selected
                );
                // Apply the output format and options
                item.output_format = options.output_format;
                item.options = options.clone();
                // Mark as ready for processing
                item.status = ConversionStatus::Queued;
            }
            _ => {
                // Skip items that are already processing or completed
                log::debug!("Skipping item {} (status={:?})", item.id, item.status);
            }
        }
    }
}

/// Validate that conversion is ready to start
pub async fn validate_conversion_ready(manager: &ConversionManager) -> Result<(), String> {
    let queue = manager.queue.read().await;
    let items = queue.all_items();

    if items.is_empty() {
        return Err(
            "No files in conversion queue. Use 'Add Files' or 'Add Folder' first.".to_string(),
        );
    }

    // Check if we have any items ready to process
    let ready_count = items
        .iter()
        .filter(|item| {
            matches!(
                item.status,
                ConversionStatus::Queued | ConversionStatus::NotConfigured
            )
        })
        .count();

    if ready_count == 0 {
        return Err(
            "No items ready for conversion. All items may be completed or failed.".to_string(),
        );
    }

    Ok(())
}

/// Get a human-readable summary of the conversion
pub fn get_conversion_summary(manager: &ConversionManager) -> String {
    // Use try_read for non-blocking access since this is called from UI
    match manager.queue.try_read() {
        Ok(queue) => {
            let items = queue.all_items();

            if items.is_empty() {
                return "No files in queue".to_string();
            }

            let ready = items
                .iter()
                .filter(|item| matches!(item.status, ConversionStatus::Queued))
                .count();

            let processing = items
                .iter()
                .filter(|item| matches!(item.status, ConversionStatus::Processing { .. }))
                .count();

            let completed = items
                .iter()
                .filter(|item| matches!(item.status, ConversionStatus::Completed { .. }))
                .count();

            if let Some(first_ready) = items
                .iter()
                .find(|item| matches!(item.status, ConversionStatus::Queued))
            {
                let format = first_ready.output_format;
                format!(
                    "Converting {} files to {} ({} processing, {} completed)",
                    ready,
                    format.name(),
                    processing,
                    completed
                )
            } else {
                format!("{} files in queue", items.len())
            }
        }
        Err(_) => "Queue busy...".to_string(),
    }
}

/// Check if the wizard has valid selections for conversion
pub fn validate_wizard_selections(wizard: &tonepoet_wizard::SimpleWizard) -> Result<(), String> {
    log::info!("Validating wizard selections:");
    log::info!("  selected_format: {:?}", wizard.selected_format);
    log::info!("  selected_quality: {:?}", wizard.selected_quality);
    log::info!("  compression_level: {:?}", wizard.compression_level);
    log::info!("  current_step: {}", wizard.current_step);

    if wizard.selected_format.is_none() {
        log::error!("Validation failed: No output format selected");
        return Err("No output format selected".to_string());
    }

    // For FLAC and other lossless formats, quality string is optional
    // They use format-specific settings like compression_level instead
    let format = wizard.selected_format.unwrap();
    match format {
        tonepoet_wizard::AudioFormat::Flac => {
            // FLAC uses compression_level, not quality string
            log::info!("FLAC format detected - using compression_level instead of quality string");
        }
        tonepoet_wizard::AudioFormat::Wav | tonepoet_wizard::AudioFormat::Aiff => {
            // WAV/AIFF use bit_depth and sample_rate, not quality string
            log::info!(
                "WAV/AIFF format detected - using bit_depth/sample_rate instead of quality string"
            );
        }
        _ => {
            // For lossy formats, we need a quality string
            if wizard.selected_quality.is_none() {
                log::error!("Validation failed: No quality settings selected for lossy format");
                return Err("No quality settings selected".to_string());
            }
        }
    }

    // Check if wizard is at the confirmation step
    if wizard.current_step < 3 {
        return Err("Wizard not completed - please finish all steps".to_string());
    }

    Ok(())
}

/// Convert ConversionSettings from a preset file to ConversionOptions
pub fn preset_to_conversion_options(
    settings: tonepoet_backend::types::ConversionSettings,
) -> ConversionOptions {
    use tonepoet_backend::types::AudioFormat as BackendFormat;

    // Convert format
    let format = match settings.format {
        BackendFormat::Flac => AudioFormat::Flac,
        BackendFormat::Wav => AudioFormat::Wav,
        BackendFormat::Aiff => AudioFormat::Aiff,
        BackendFormat::WavPack => AudioFormat::WavPack,
        BackendFormat::Mp3 => AudioFormat::Mp3,
        BackendFormat::Aac => AudioFormat::Aac,
        BackendFormat::Opus => AudioFormat::Opus,
        BackendFormat::Alac => AudioFormat::Alac,
    };

    // Convert quality settings based on format
    let quality = match format {
        AudioFormat::Flac => {
            let compression_level = settings.compression_level.unwrap_or(8);
            QualitySettings::Flac { compression_level }
        }
        AudioFormat::Wav => {
            let bit_depth = settings.bit_depth.unwrap_or(16) as u16;
            let sample_rate = settings.sample_rate.unwrap_or(44100);
            QualitySettings::Wav {
                bit_depth,
                sample_rate,
            }
        }
        AudioFormat::Aiff => {
            let bit_depth = settings.bit_depth.unwrap_or(16) as u16;
            let sample_rate = settings.sample_rate.unwrap_or(44100);
            QualitySettings::Aiff {
                bit_depth,
                sample_rate,
            }
        }
        AudioFormat::WavPack => QualitySettings::WavPack {
            compression_mode: WavPackMode::Normal,
            hybrid_mode: false,
            correction_file: false,
        },
        AudioFormat::Mp3 => {
            let (bitrate_mode, quality) = if let Some(bitrate) = settings.mp3_bitrate {
                (Mp3BitrateMode::Cbr { bitrate }, 2)
            } else if let Some(quality) = settings.mp3_quality {
                (Mp3BitrateMode::Vbr { quality }, quality)
            } else {
                (Mp3BitrateMode::Cbr { bitrate: 320 }, 2)
            };
            QualitySettings::Mp3 {
                bitrate_mode,
                quality,
            }
        }
        AudioFormat::Aac => {
            let profile = settings
                .aac_profile
                .map(|p| match p {
                    tonepoet_backend::types::AacProfile::LcAac => AacProfile::Lc,
                    tonepoet_backend::types::AacProfile::HeAac => AacProfile::He,
                    tonepoet_backend::types::AacProfile::HeAacV2 => AacProfile::HeV2,
                    tonepoet_backend::types::AacProfile::LdAac => AacProfile::Lc, // Map LD to LC
                })
                .unwrap_or(AacProfile::Lc);
            let bitrate = 256; // Default AAC bitrate
            QualitySettings::Aac { bitrate, profile }
        }
        AudioFormat::Opus => {
            // Parse Opus quality from preset selected_quality string
            let bitrate = match settings.selected_quality.as_deref() {
                Some("Insane") => 320,
                Some("Very High") => 256,
                Some("High") => 192,
                Some("Medium") => 128,
                Some("Low") => 64,
                // Support explicit bitrate strings
                Some("320 kbps") => 320,
                Some("256 kbps") => 256,
                Some("192 kbps") => 192,
                Some("128 kbps") => 128,
                Some("64 kbps") => 64,
                _ => 128, // Default if not specified
            };
            let complexity = 10; // Maximum quality
            QualitySettings::Opus {
                bitrate,
                complexity,
            }
        }
        AudioFormat::Alac => QualitySettings::Alac,
    };

    ConversionOptions {
        output_format: format,
        quality,
        preserve_metadata: true,
        calculate_replaygain: settings.replaygain_mode.is_some(),
        replaygain_mode: settings.replaygain_mode.clone().map(|mode| {
            use crate::convert::simple_wizard::ReplayGainMode;
            use tonepoet_backend::types::ReplayGainMode as BackendMode;
            match mode {
                BackendMode::Track => ReplayGainMode::Track,
                BackendMode::Album => ReplayGainMode::Album,
                BackendMode::Both => ReplayGainMode::Both,
            }
        }),
        naming_template: None,
        overwrite: settings.overwrite,
        output_dir: None,
        resample_quality: settings.resample_quality,
        nyquist_transition: if matches!(
            format,
            AudioFormat::Flac
                | AudioFormat::Wav
                | AudioFormat::Aiff
                | AudioFormat::WavPack
                | AudioFormat::Opus
        ) {
            settings.nyquist_transition.map(|nt| {
                use tonepoet_backend::types::NyquistTransition as BackendNyquist;
                match nt {
                    BackendNyquist::Sharp => NyquistTransition::Steep,
                    BackendNyquist::Medium => NyquistTransition::Gentle,
                    BackendNyquist::Gentle => NyquistTransition::Gentle,
                    BackendNyquist::Steep => NyquistTransition::Steep,
                    BackendNyquist::BrickWall => NyquistTransition::BrickWall,
                }
            })
        } else {
            None // Clear for MP3/AAC - not exposed in UI
        },
        dither_type: settings.dither_type.map(|dt| {
            use tonepoet_backend::types::DitherType as BackendDither;
            match dt {
                BackendDither::None => DitherType::None,
                BackendDither::Tpdf => DitherType::TPDF,
                BackendDither::Shibata => DitherType::Shibata,
                BackendDither::LowShibata => DitherType::LowShibata,
                BackendDither::HighShibata => DitherType::HighShibata,
                BackendDither::FShaped => DitherType::FWeighted,
                BackendDither::ModifiedE => DitherType::ModifiedEWeighted,
                BackendDither::ImprovedE => DitherType::ImprovedEWeighted,
                BackendDither::Gesemann => DitherType::Gesemann,
            }
        }),
        target_sample_rate: settings.sample_rate,
        target_bit_depth: settings.bit_depth, // Extract bit depth from preset
        copy_auxiliary_files: settings.copy_files_enabled.unwrap_or(true),
        copy_subdirectories: settings.copy_subdirectories_enabled.unwrap_or(true),
        reencode_flac: settings.reencode_flac.unwrap_or(false),
        merge_to_single: false,
        preferred_backend: None, // Presets don't store backend selection - that's a runtime config
        original_settings: None, // Set by caller in main.rs
        ssrc_insane_mode: settings.ssrc_insane_mode,
        append_lineage_to_comment: false, // Presets don't include this (set from app config)
        write_log_file: false,            // Presets don't include this (set from app config)
        generate_cue_files: false,        // Presets don't include this (set from app config)
        cue_generation_mode: "IfMerging".to_string(), // Presets don't include this (set from app config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flac_settings_extraction() {
        let mut wizard = tonepoet_wizard::SimpleWizard::new();
        wizard.selected_format = Some(tonepoet_wizard::AudioFormat::Flac);
        wizard.selected_quality = Some("High".to_string());
        wizard.compression_level = Some(8);
        wizard.current_step = 3; // Confirmation step

        let (format, options) = extract_wizard_settings(&wizard);

        assert_eq!(format, AudioFormat::Flac);
        assert_eq!(options.output_format, AudioFormat::Flac);

        if let QualitySettings::Flac { compression_level } = options.quality {
            assert_eq!(compression_level, 8);
        } else {
            panic!("Expected FLAC quality settings");
        }
    }

    #[tokio::test]
    async fn test_validation_empty_queue() {
        let config = crate::convert::ConversionConfig::default();
        let manager = ConversionManager::new(config);

        let result = validate_conversion_ready(&manager).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No files"));
    }
}
