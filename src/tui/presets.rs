//! TUI preset management: save/load pill state to TOML files

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::convert::formats::AudioFormat;
use crate::convert::simple_wizard::DitherType;
use tonepoet_pipeline::enums::{DsdFilterPreset, DsdNoiseShaper, ModulatorOrder};

use super::app::*;

fn default_resampler() -> String {
    "sox".to_string()
}

fn default_companion_extensions() -> String {
    String::new()
}

/// A TUI-native preset that stores pill values directly
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuiPreset {
    pub name: String,
    pub description: Option<String>,
    pub version: u32, // 2 = legacy TUI preset, 3 = dynamic format-pane preset

    // Format pane
    pub format: String, // "flac", "opus", etc.
    pub sample_rate: u32,
    pub bit_depth: String,  // "16", "24", "32", "32f", "64f"
    pub dither: String,     // "tpdf", "none", "shibata", ...
    pub replaygain: String, // "album", "track", "both", "off"
    #[serde(default = "default_resampler")]
    pub resampler: String, // "sox", "ssrc", "soxr"
    #[serde(default)]
    pub noise_shaper: Option<String>, // "clans", "sdm", "crfb"
    #[serde(default)]
    pub modulator_order: Option<u8>, // 4-8
    #[serde(default)]
    pub dsd_filter_preset: Option<String>, // "auto", "sinc"

    // Output options pane
    #[serde(default)]
    pub dest_path: Option<String>,
    pub folder_template: String,
    pub filename_template: String,
    pub merge: String, // "multi-file", "single-image"
    #[serde(default = "default_companion_extensions")]
    pub companion_extensions: String,
    #[serde(default)]
    pub companion_folders: String,
    #[serde(default)]
    pub write_log: bool,
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
            version: 3,
            format: format.format.selected_label().to_lowercase(),
            sample_rate: *format.sample_rate.selected_value(),
            bit_depth: format.bit_depth.selected_label().to_string(),
            dither: format.dither.selected_label().to_lowercase(),
            replaygain: format.replaygain.selected_label().to_lowercase(),
            resampler: format.resampler.selected_label().to_lowercase(),
            noise_shaper: Some(format.noise_shaper.selected_label().to_lowercase()),
            modulator_order: Some(format.modulator_order.selected_value().value()),
            dsd_filter_preset: Some(format.conversion_preset.selected_label().to_lowercase()),
            dest_path: output_opts
                .dest_path
                .as_ref()
                .map(|p| p.to_string_lossy().to_string()),
            folder_template: output_opts.folder_template.clone(),
            filename_template: output_opts.filename_template.clone(),
            merge: match *output_opts.merge.selected_value() {
                MergeMode::MultiFile => "multi-file".to_string(),
                MergeMode::SingleImage => "single-image".to_string(),
            },
            companion_extensions: output_opts.companion_extensions.clone(),
            companion_folders: output_opts.companion_folders.clone(),
            write_log: *output_opts.write_log.selected_value(),
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

        if let Some(resampler) = parse_resampler(&self.resampler) {
            format_state.resampler.select_value(&resampler);
        }
        if let Some(ref shaper) = self.noise_shaper {
            if let Some(value) = parse_noise_shaper(shaper) {
                format_state.noise_shaper.select_value(&value);
            }
        }
        if let Some(order) = self.modulator_order.and_then(parse_modulator_order) {
            format_state.modulator_order.select_value(&order);
        }
        if let Some(ref preset) = self.dsd_filter_preset {
            if let Some(value) = parse_dsd_filter_preset(preset) {
                format_state.conversion_preset.select_value(&value);
            }
        }

        // Output options
        if let Some(ref p) = self.dest_path {
            output_opts.dest_path = Some(std::path::PathBuf::from(p));
        }
        output_opts.folder_template = self.folder_template.clone();
        output_opts.filename_template = self.filename_template.clone();
        output_opts.companion_extensions = self.companion_extensions.clone();
        output_opts.companion_folders = self.companion_folders.clone();
        output_opts.write_log.select_value(&self.write_log);

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

        let dither = preset
            .dither_type
            .as_ref()
            .map(|dt| {
                use tonepoet_wizard::DitherType as WD;
                match dt {
                    WD::None => "none",
                    WD::Tpdf => "tpdf",
                    WD::Shibata | WD::LowShibata | WD::HighShibata => "shaped",
                    WD::Gesemann => "shaped",
                    WD::SlopedTpdf => "tpdf",
                }
            })
            .unwrap_or("tpdf");

        let replaygain = preset
            .replaygain_mode
            .as_ref()
            .map(|rg| {
                use tonepoet_wizard::ReplayGainMode as WR;
                match rg {
                    WR::Track => "track",
                    WR::Album => "album",
                    WR::Both => "both",
                    WR::Off => "off",
                }
            })
            .unwrap_or("off");

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
            resampler: default_resampler(),
            noise_shaper: Some("clans".to_string()),
            modulator_order: Some(8),
            dsd_filter_preset: Some("auto".to_string()),
            dest_path: None,
            folder_template: "%ARTIST%/%ALBUM% (%YEAR%)".to_string(),
            filename_template: "%TRACKNN% - %TITLE%.%EXT%".to_string(),
            merge: merge.to_string(),
            companion_extensions: default_companion_extensions(),
            companion_folders: String::new(),
            write_log: false,
        }
    }
}

// ── Preset file I/O ──────────────────────────────────────────────────

/// Get the presets directory path
pub fn presets_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg).join("tonepoet").join("presets")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
            .join(".config")
            .join("tonepoet")
            .join("presets")
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

/// List presets grouped by their output audio format (codec). Each group
/// is `(AudioFormat, Vec<preset_name>)`, sorted by format name. Groups
/// with zero presets are omitted. Presets whose format can't be parsed
/// are collected under a final "unknown" group (returned as `None`).
///
/// Loads each preset to peek at the format field — fine for typical
/// counts (5–20 files, ~1ms each).
pub fn list_presets_by_format() -> Vec<(Option<AudioFormat>, Vec<String>)> {
    use std::collections::HashMap;

    let names = list_presets();
    let mut groups: HashMap<Option<AudioFormat>, Vec<String>> = HashMap::new();

    for name in names {
        let fmt = load_preset(&name)
            .ok()
            .and_then(|p| parse_format(&p.format));
        groups.entry(fmt).or_default().push(name);
    }

    // Produce groups in a stable display order. Known formats first,
    // unknown ("Other") last. Codecs with no presets are omitted.
    let mut result: Vec<(Option<AudioFormat>, Vec<String>)> = Vec::new();
    let display_order = [
        AudioFormat::Flac,
        AudioFormat::Wav,
        AudioFormat::WavPack,
        AudioFormat::Aiff,
        AudioFormat::Alac,
        AudioFormat::Opus,
        AudioFormat::Aac,
        AudioFormat::Mp3,
    ];
    for fmt in &display_order {
        if let Some(names) = groups.remove(&Some(*fmt)) {
            result.push((Some(*fmt), names));
        }
    }
    // Unknown at the end (presets whose format field didn't parse).
    if let Some(names) = groups.remove(&None) {
        result.push((None, names));
    }
    result
}

/// Return the canonical file path for a named preset in the configured presets directory.
pub fn preset_file_path(name: &str) -> PathBuf {
    presets_dir().join(format!("{}.toml", name))
}

/// Load a preset by name from the configured presets directory.
pub fn load_preset(name: &str) -> Result<TuiPreset, String> {
    let path = preset_file_path(name);
    load_preset_from_path(&path)
}

/// Load a preset from an explicit path returned by a file picker.
pub fn load_preset_from_path(path: &Path) -> Result<TuiPreset, String> {
    let display_name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("preset");
    let contents = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read preset '{}': {}", path.display(), e))?;

    // Try parsing as TuiPreset (version 2) first.
    if let Ok(mut preset) = toml::from_str::<TuiPreset>(&contents) {
        preset.name = display_name.to_string();
        return Ok(preset);
    }

    // Fall back to legacy wizard preset.
    let mut preset = TuiPreset::from_legacy(
        &toml::from_str::<tonepoet_wizard::ConversionPreset>(&contents)
            .map_err(|e| format!("Failed to parse preset '{}': {}", path.display(), e))?,
    );
    preset.name = display_name.to_string();
    Ok(preset)
}

/// Save a preset to disk in the configured presets directory.
pub fn save_preset(preset: &TuiPreset) -> Result<(), String> {
    let path = preset_file_path(&preset.name);
    save_preset_to_path(preset, &path)
}

/// Save a preset to an explicit path returned by a file picker.
pub fn save_preset_to_path(preset: &TuiPreset, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create preset directory '{}': {}", parent.display(), e))?;
    }
    let contents =
        toml::to_string_pretty(preset).map_err(|e| format!("Failed to serialize preset: {}", e))?;

    fs::write(path, contents)
        .map_err(|e| format!("Failed to write preset '{}': {}", path.display(), e))?;

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

    fs::remove_file(&path).map_err(|e| format!("Failed to delete preset '{}': {}", name, e))?;

    Ok(())
}

/// Save a preset to both TOML and SQLite.
pub fn save_preset_with_db(preset: &TuiPreset, db: &crate::db::Database) -> Result<(), String> {
    save_preset(preset)?;
    store_preset_in_db(preset, db);
    Ok(())
}

/// Save a preset to an explicit path and index it in SQLite under the file stem.
pub fn save_preset_to_path_with_db(
    preset: &TuiPreset,
    path: &Path,
    db: &crate::db::Database,
) -> Result<(), String> {
    save_preset_to_path(preset, path)?;
    store_preset_in_db(preset, db);
    Ok(())
}

fn store_preset_in_db(preset: &TuiPreset, db: &crate::db::Database) {
    let _ = db.store_preset(
        &preset.name,
        &preset.format,
        preset.description.as_deref(),
        Some(preset.sample_rate),
        Some(&preset.bit_depth),
        Some(&preset.dither),
        Some(&preset.replaygain),
        Some(&preset.folder_template),
        Some(&preset.filename_template),
        Some(&preset.merge),
    );
}

/// Delete a preset from both TOML and SQLite.
pub fn delete_preset_with_db(name: &str, db: &crate::db::Database) -> Result<(), String> {
    delete_preset(name)?;
    let _ = db.delete_preset(name);
    Ok(())
}

/// Sync TOML presets into the SQLite database. Imports any TOML
/// presets not already in the DB (handles first-run and externally-
/// added presets like manual file copies or syncs from another machine).
pub fn import_presets_to_db(db: &crate::db::Database) {
    let db_names = db.list_preset_names();
    for name in list_presets() {
        if db_names.iter().any(|n| n == &name) {
            continue; // Already in DB.
        }
        if let Ok(preset) = load_preset(&name) {
            let _ = db.store_preset(
                &preset.name,
                &preset.format,
                preset.description.as_deref(),
                Some(preset.sample_rate),
                Some(&preset.bit_depth),
                Some(&preset.dither),
                Some(&preset.replaygain),
                Some(&preset.folder_template),
                Some(&preset.filename_template),
                Some(&preset.merge),
            );
        }
    }
}

/// List presets grouped by format using the SQLite index.
/// Falls back to the file-based scan if the DB is empty.
pub fn list_presets_by_format_db(
    db: &crate::db::Database,
) -> Vec<(Option<AudioFormat>, Vec<String>)> {
    let groups = db.list_presets_by_format();
    if groups.is_empty() {
        // Fall back to file-based scan (DB might not be populated yet).
        return list_presets_by_format();
    }

    // Convert format strings to AudioFormat.
    let display_order = [
        AudioFormat::Flac,
        AudioFormat::Wav,
        AudioFormat::WavPack,
        AudioFormat::Aiff,
        AudioFormat::Alac,
        AudioFormat::Opus,
        AudioFormat::Aac,
        AudioFormat::Mp3,
    ];
    let mut result = Vec::new();
    for fmt in &display_order {
        let fmt_str = fmt.name().to_lowercase();
        if let Some((_, names)) = groups.iter().find(|(f, _)| f.to_lowercase() == fmt_str) {
            result.push((Some(*fmt), names.clone()));
        }
    }
    // Unknown formats at the end.
    for (fmt_str, names) in &groups {
        let is_known = display_order
            .iter()
            .any(|f| f.name().to_lowercase() == fmt_str.to_lowercase());
        if !is_known {
            result.push((None, names.clone()));
        }
    }
    result
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
        "dsf" => Some(AudioFormat::Dsf),
        "dff" => Some(AudioFormat::Dff),
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
        "low-shibata" | "low_shibata" => Some(DitherType::LowShibata),
        "high-shibata" | "high_shibata" => Some(DitherType::HighShibata),
        "gesemann" => Some(DitherType::Gesemann),
        "lipshitz" => Some(DitherType::Lipshitz),
        _ => None,
    }
}

fn parse_resampler(s: &str) -> Option<ResamplerChoice> {
    match s {
        "sox" => Some(ResamplerChoice::Sox),
        "ssrc" => Some(ResamplerChoice::Ssrc),
        "soxr" => Some(ResamplerChoice::Soxr),
        _ => None,
    }
}

fn parse_noise_shaper(s: &str) -> Option<DsdNoiseShaper> {
    match s {
        "clans" => Some(DsdNoiseShaper::Clans),
        "sdm" => Some(DsdNoiseShaper::Sdm),
        "crfb" => Some(DsdNoiseShaper::Crfb),
        _ => None,
    }
}

fn parse_modulator_order(value: u8) -> Option<ModulatorOrder> {
    match value {
        4 => Some(ModulatorOrder::Order4),
        5 => Some(ModulatorOrder::Order5),
        6 => Some(ModulatorOrder::Order6),
        7 => Some(ModulatorOrder::Order7),
        8 => Some(ModulatorOrder::Order8),
        _ => None,
    }
}

fn parse_dsd_filter_preset(s: &str) -> Option<DsdFilterPreset> {
    match s {
        "auto" => Some(DsdFilterPreset::Auto),
        "sinc" => Some(DsdFilterPreset::Sinc),
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


#[cfg(test)]
mod companion_preset_tests {
    use super::*;

    #[test]
    fn legacy_presets_without_companion_fields_get_backward_compatible_defaults() {
        let preset: TuiPreset = toml::from_str(
            r#"
name = "legacy"
version = 3
format = "flac"
sample_rate = 44100
bit_depth = "24"
dither = "tpdf"
replaygain = "off"
folder_template = "%ARTIST%/%ALBUM%"
filename_template = "%TRACKNN% - %TITLE%.%EXT%"
merge = "multi-file"
"#,
        )
        .expect("preset without companion fields should deserialize");

        assert!(preset.companion_extensions.is_empty());
        assert!(preset.companion_folders.is_empty());
        assert!(!preset.write_log);
    }

    #[test]
    fn companion_fields_round_trip_through_preset_capture_and_apply() {
        let format = FormatState::new();
        let mut output = OutputOptionsState::new();
        output.companion_extensions = ".jpg, .pdf".to_string();
        output.companion_folders = "Scans, Artwork".to_string();
        output.write_log.select_value(&true);

        let preset = TuiPreset::from_pill_state("companions", &format, &output);
        let mut restored_format = FormatState::new();
        let mut restored_output = OutputOptionsState::new();
        restored_output.companion_extensions.clear();
        restored_output.companion_folders.clear();

        preset.apply_to_pills(&mut restored_format, &mut restored_output);

        assert_eq!(restored_output.companion_extensions, ".jpg, .pdf");
        assert_eq!(restored_output.companion_folders, "Scans, Artwork");
        assert!(*restored_output.write_log.selected_value());
    }
}
