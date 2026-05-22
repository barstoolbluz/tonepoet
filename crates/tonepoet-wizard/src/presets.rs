use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::PathBuf;

use crate::types::{
    AacProfile, AudioFormat, DitherType, NyquistTransition, OpusContentType, ReplayGainMode,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversionPreset {
    pub name: String,
    pub description: Option<String>,
    pub version: u32, // For future compatibility

    // Core wizard state
    pub selected_format: AudioFormat,
    pub selected_quality: Option<String>,

    // FLAC Advanced Options
    pub bit_depth: Option<u32>,
    pub sample_rate: Option<u32>,
    pub compression_level: Option<u8>,
    pub resample_quality: Option<u8>,
    pub dither_type: Option<DitherType>,
    pub nyquist_transition: Option<NyquistTransition>,
    pub verify_encoding: Option<bool>,
    pub store_md5: Option<bool>,

    // Format-specific options
    pub opus_content_type: Option<OpusContentType>,
    pub aac_profile: Option<AacProfile>,

    // Additional options
    pub replaygain_mode: Option<ReplayGainMode>,
    pub copy_files_enabled: bool,
    pub copy_files_extensions: String,
    pub copy_subdirectories_enabled: bool,
    pub copy_subdirectories: String,
    pub merge_to_single: Option<bool>,
    pub reencode_flac: Option<bool>,
}

pub struct PresetManager {
    presets_dir: PathBuf,
}

impl PresetManager {
    pub fn new() -> io::Result<Self> {
        let presets_dir = Self::get_presets_directory()?;

        // Create directory if it doesn't exist
        fs::create_dir_all(&presets_dir)?;

        Ok(Self { presets_dir })
    }

    fn get_presets_directory() -> io::Result<PathBuf> {
        // Try to get XDG config directory first
        if let Ok(xdg_config) = std::env::var("XDG_CONFIG_HOME") {
            Ok(PathBuf::from(xdg_config).join("tonepoet").join("presets"))
        } else if let Ok(home) = std::env::var("HOME") {
            Ok(PathBuf::from(home)
                .join(".config")
                .join("tonepoet")
                .join("presets"))
        } else {
            // Fallback to current directory
            Ok(PathBuf::from("./presets"))
        }
    }

    pub fn save_preset(&self, preset: &ConversionPreset) -> io::Result<()> {
        let filename = format!("{}.toml", preset.name);
        let file_path = self.presets_dir.join(&filename);

        let toml_str =
            toml::to_string_pretty(preset).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        fs::write(file_path, toml_str)?;

        Ok(())
    }

    #[allow(dead_code)]
    pub fn load_preset(&self, name: &str) -> io::Result<ConversionPreset> {
        let filename = format!("{}.toml", name);
        let file_path = self.presets_dir.join(&filename);

        let toml_str = fs::read_to_string(file_path)?;
        let preset: ConversionPreset =
            toml::from_str(&toml_str).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

        Ok(preset)
    }

    pub fn list_presets(&self) -> io::Result<Vec<String>> {
        let mut presets = Vec::new();

        if let Ok(entries) = fs::read_dir(&self.presets_dir) {
            for entry in entries {
                if let Ok(entry) = entry {
                    let path = entry.path();
                    if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("toml") {
                        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                            presets.push(stem.to_string());
                        }
                    }
                }
            }
        }

        presets.sort();
        Ok(presets)
    }

    #[allow(dead_code)]
    pub fn delete_preset(&self, name: &str) -> io::Result<()> {
        let filename = format!("{}.toml", name);
        let file_path = self.presets_dir.join(&filename);

        fs::remove_file(file_path)?;

        Ok(())
    }

    pub fn preset_exists(&self, name: &str) -> bool {
        let filename = format!("{}.toml", name);
        let file_path = self.presets_dir.join(&filename);

        file_path.exists()
    }
}

impl From<&crate::types::SimpleWizard> for ConversionPreset {
    fn from(wizard: &crate::types::SimpleWizard) -> Self {
        ConversionPreset {
            name: String::new(), // Will be set by the user
            description: None,
            version: 1,
            selected_format: wizard.selected_format.unwrap_or(AudioFormat::Flac),
            selected_quality: wizard.selected_quality.clone(),
            bit_depth: wizard.bit_depth,
            sample_rate: wizard.sample_rate,
            compression_level: wizard.compression_level,
            resample_quality: wizard.resample_quality,
            dither_type: wizard.dither_type,
            nyquist_transition: wizard.nyquist_transition,
            verify_encoding: wizard.verify_encoding,
            store_md5: wizard.store_md5,
            opus_content_type: wizard.opus_content_type,
            aac_profile: wizard.aac_profile,
            replaygain_mode: wizard.replaygain_mode,
            copy_files_enabled: wizard.copy_files_enabled,
            copy_files_extensions: wizard.copy_files_extensions.clone(),
            copy_subdirectories_enabled: wizard.copy_subdirectories_enabled,
            copy_subdirectories: wizard.copy_subdirectories.clone(),
            merge_to_single: wizard.merge_to_single,
            reencode_flac: wizard.reencode_flac,
        }
    }
}
