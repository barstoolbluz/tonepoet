//! TUI preset management: save/load pill state to TOML files

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::convert::formats::AudioFormat;
use crate::convert::simple_wizard::DitherType;

use super::app::*;

/// A TUI-native preset that stores pill values directly
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuiPreset {
    pub name: String,
    pub description: Option<String>,
    pub version: u32, // 2 = TUI preset

    // Format pane
    pub format: String,       // "flac", "opus", etc.
    pub sample_rate: u32,
    pub bit_depth: String,    // "16", "24", "32", "32f", "64f"
    pub dither: String,       // "tpdf", "none", "shaped"
    pub replaygain: String,   // "album", "track", "both", "off"

    // Output options pane
    pub folder_template: String,
    pub filename_template: String,
    pub merge: String, // "multi-file", "single-image"
}

impl TuiPreset {
    /// Capture current pill state into a preset
    pub fn from_pill_state(
        name: &str,
        format: &FormatState,
        output_opts: &OutputOptionsState,
    ) -> Self {
        Self {
            name: name.to_string(),
            description: None,
            version: 2,
            format: format.format.selected_label().to_lowercase(),
            sample_rate: *format.sample_rate.selected_value(),
            bit_depth: format.bit_depth.selected_label().to_string(),
            dither: format.dither.selected_label().to_lowercase(),
            replaygain: format.replaygain.selected_label().to_lowercase(),
            folder_template: output_opts.folder_template.clone(),
            filename_template: output_opts.filename_template.clone(),
            merge: match *output_opts.merge.selected_value() {
                MergeMode::MultiFile => "multi-file".to_string(),
                MergeMode::SingleImage => "single-image".to_string(),
            },
        }
    }

    /// Apply preset values to pill state
    pub fn apply_to_pills(
        &self,
        format_state: &mut FormatState,
        output_opts: &mut OutputOptionsState,
    ) {
        // Format
        if let Some(fmt) = parse_format(&self.format) {
            format_state.format.select_value(&fmt);
            format_state.apply_format_constraints();
        }

        // Sample rate
        format_state.sample_rate.select_value(&self.sample_rate);

        // Bit depth
        if let Some(bd) = parse_bit_depth(&self.bit_depth) {
            format_state.bit_depth.select_value(&bd);
        }

        // Dither
        if let Some(dt) = parse_dither(&self.dither) {
            format_state.dither.select_value(&dt);
        }

        // ReplayGain
        if let Some(rg) = parse_replaygain(&self.replaygain) {
            format_state.replaygain.select_value(&rg);
        }

        // Output options
        output_opts.folder_template = self.folder_template.clone();
        output_opts.filename_template = self.filename_template.clone();

        if let Some(mm) = parse_merge(&self.merge) {
            output_opts.merge.select_value(&mm);
        }

        // Re-apply constraints after all pills are set
        format_state.apply_format_constraints();
    }

    /// Import from a legacy wizard ConversionPreset
    pub fn from_legacy(preset: &tonepoet_wizard::ConversionPreset) -> Self {
        use tonepoet_wizard::AudioFormat as WF;

        let format = match preset.selected_format {
            WF::Flac => "flac",
            WF::Wav => "wav",
            WF::Aiff => "aiff",
            WF::WavPack => "wavpack",
            WF::Mp3 => "mp3",
            WF::Aac => "aac",
            WF::Opus => "opus",
        };

        let bit_depth = match preset.bit_depth {
            Some(16) => "16",
            Some(24) => "24",
            Some(32) => "32",
            Some(320) => "32f",
            _ => "24", // default
        };

        let dither = preset.dither_type.as_ref().map(|dt| {
            use tonepoet_wizard::DitherType as WD;
            match dt {
                WD::None => "none",
                WD::Tpdf => "tpdf",
                WD::Shibata | WD::LowShibata | WD::HighShibata => "shaped",
                WD::Gesemann => "shaped",
                WD::SlopedTpdf => "tpdf",
            }
        }).unwrap_or("tpdf");

        let replaygain = preset.replaygain_mode.as_ref().map(|rg| {
            use tonepoet_wizard::ReplayGainMode as WR;
            match rg {
                WR::Track => "track",
                WR::Album => "album",
                WR::Both => "both",
                WR::Off => "off",
            }
        }).unwrap_or("off");

        let merge = if preset.merge_to_single == Some(true) {
            "single-image"
        } else {
            "multi-file"
        };

        Self {
            name: preset.name.clone(),
            description: preset.description.clone(),
            version: 2,
            format: format.to_string(),
            sample_rate: preset.sample_rate.unwrap_or(44100),
            bit_depth: bit_depth.to_string(),
            dither: dither.to_string(),
            replaygain: replaygain.to_string(),
            folder_template: "%ARTIST%/%ALBUM% (%YEAR%)".to_string(),
            filename_template: "%TRACKNN% - %TITLE%.%EXT%".to_string(),
            merge: merge.to_string(),
        }
    }
}

// ── Preset file I/O ──────────────────────────────────────────────────

/// Get the presets directory path
pub fn presets_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg).join("tonepoet").join("presets")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config").join("tonepoet").join("presets")
    } else {
        PathBuf::from("./presets")
    }
}

/// List all preset names (sorted)
pub fn list_presets() -> Vec<String> {
    let dir = presets_dir();
    let mut names = Vec::new();

    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("toml") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    names.push(stem.to_string());
                }
            }
        }
    }

    names.sort();
    names
}

/// Load a preset by name
pub fn load_preset(name: &str) -> Result<TuiPreset, String> {
    let dir = presets_dir();
    let path = dir.join(format!("{}.toml", name));

    let contents = fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read preset '{}': {}", name, e))?;

    // Try parsing as TuiPreset (version 2) first
    if let Ok(preset) = toml::from_str::<TuiPreset>(&contents) {
        return Ok(preset);
    }

    // Fall back to legacy wizard preset
    let legacy: tonepoet_wizard::ConversionPreset = toml::from_str(&contents)
        .map_err(|e| format!("Failed to parse preset '{}': {}", name, e))?;

    Ok(TuiPreset::from_legacy(&legacy))
}

/// Save a preset to disk
pub fn save_preset(preset: &TuiPreset) -> Result<(), String> {
    let dir = presets_dir();
    fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create presets directory: {}", e))?;

    let path = dir.join(format!("{}.toml", preset.name));
    let contents = toml::to_string_pretty(preset)
        .map_err(|e| format!("Failed to serialize preset: {}", e))?;

    fs::write(&path, contents)
        .map_err(|e| format!("Failed to write preset '{}': {}", preset.name, e))?;

    Ok(())
}

/// Find an unused preset name, starting with `base` and appending `-1`, `-2`, etc.
/// if the base name already exists in `existing`.
pub fn find_unique_preset_name(base: &str, existing: &[String]) -> String {
    if !existing.iter().any(|n| n == base) {
        return base.to_string();
    }
    let mut i = 1;
    loop {
        let candidate = format!("{}-{}", base, i);
        if !existing.iter().any(|n| n == &candidate) {
            return candidate;
        }
        i += 1;
    }
}

/// Delete a preset by name
pub fn delete_preset(name: &str) -> Result<(), String> {
    let dir = presets_dir();
    let path = dir.join(format!("{}.toml", name));

    fs::remove_file(&path)
        .map_err(|e| format!("Failed to delete preset '{}': {}", name, e))?;

    Ok(())
}

// ── String → type parsers ────────────────────────────────────────────

fn parse_format(s: &str) -> Option<AudioFormat> {
    match s {
        "flac" => Some(AudioFormat::Flac),
        "wav" => Some(AudioFormat::Wav),
        "aiff" => Some(AudioFormat::Aiff),
        "wavpack" => Some(AudioFormat::WavPack),
        "mp3" => Some(AudioFormat::Mp3),
        "aac" => Some(AudioFormat::Aac),
        "opus" => Some(AudioFormat::Opus),
        "alac" => Some(AudioFormat::Alac),
        _ => None,
    }
}

fn parse_bit_depth(s: &str) -> Option<BitDepthChoice> {
    match s {
        "16" => Some(BitDepthChoice::Int16),
        "24" => Some(BitDepthChoice::Int24),
        "32" => Some(BitDepthChoice::Int32),
        "32f" => Some(BitDepthChoice::Float32),
        "64f" => Some(BitDepthChoice::Float64),
        _ => None,
    }
}

fn parse_dither(s: &str) -> Option<DitherType> {
    match s {
        "tpdf" => Some(DitherType::TPDF),
        "none" => Some(DitherType::None),
        "shaped" | "shibata" => Some(DitherType::Shibata),
        _ => None,
    }
}

fn parse_replaygain(s: &str) -> Option<ReplayGainChoice> {
    match s {
        "album" => Some(ReplayGainChoice::Album),
        "track" => Some(ReplayGainChoice::Track),
        "both" => Some(ReplayGainChoice::Both),
        "off" => Some(ReplayGainChoice::Off),
        _ => None,
    }
}

fn parse_merge(s: &str) -> Option<MergeMode> {
    match s {
        "multi-file" => Some(MergeMode::MultiFile),
        "single-image" => Some(MergeMode::SingleImage),
        _ => None,
    }
}
