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

fn default_mp3_mode() -> String {
    "vbr".to_string()
}

fn default_mp3_vbr_quality() -> u8 {
    0
}

fn default_mp3_bitrate_kbps() -> u32 {
    320
}

fn default_aac_profile() -> String {
    "lc".to_string()
}

fn default_aac_bitrate_kbps() -> u32 {
    256
}

fn default_opus_content_type() -> String {
    "auto".to_string()
}

fn default_opus_bitrate_kbps() -> u32 {
    192
}

fn default_opus_complexity() -> u8 {
    10
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

    // Above-the-fold lossy preset row state. Named values round-trip as stable
    // keys; explicit custom is serialized as "custom" so a restored preset does
    // not silently relabel exact-match manual settings as a named preset. Missing
    // keys from older presets are inferred from the numeric codec settings.
    #[serde(default)]
    pub mp3_lossy_preset: Option<String>, // "v0", "v2", "320-cbr", "custom"
    #[serde(default)]
    pub aac_lossy_preset: Option<String>, // "256-vbr", "192-vbr", "128-vbr", "custom"
    #[serde(default)]
    pub opus_lossy_preset: Option<String>, // "128", "96", "64", "custom"

    // Lossy codec settings. These are persisted independently of the visible
    // lossy preset label so custom/manual settings round-trip exactly. Defaults
    // preserve compatibility with presets saved before the lossy rows existed.
    #[serde(default = "default_mp3_mode")]
    pub mp3_mode: String, // "vbr", "cbr", "abr"
    #[serde(default)]
    pub mp3_quality_preset: Option<usize>,
    #[serde(default = "default_mp3_vbr_quality")]
    pub mp3_vbr_quality: u8,
    #[serde(default = "default_mp3_bitrate_kbps")]
    pub mp3_bitrate_kbps: u32,
    #[serde(default = "default_aac_profile")]
    pub aac_profile: String, // "lc", "he", "hev2"
    #[serde(default)]
    pub aac_quality_preset: Option<usize>,
    #[serde(default = "default_aac_bitrate_kbps")]
    pub aac_bitrate_kbps: u32,
    #[serde(default = "default_opus_content_type")]
    pub opus_content_type: String, // "auto", "music", "speech"
    #[serde(default)]
    pub opus_quality_preset: Option<usize>,
    #[serde(default = "default_opus_bitrate_kbps")]
    pub opus_bitrate_kbps: u32,
    #[serde(default = "default_opus_complexity")]
    pub opus_complexity: u8,

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
    pub force_encode: bool,
    #[serde(default)]
    pub disc_subfolders: bool,
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
            // Store a stable, canonical key rather than the UI label. Labels are
            // presentation strings and may change (for example, DSF was previously
            // displayed as a generic "DSD" family label), while preset files need
            // durable, unambiguous identifiers.
            format: format_key(*format.format.selected_value()).to_string(),
            sample_rate: *format.sample_rate.selected_value(),
            bit_depth: format.bit_depth.selected_label().to_string(),
            dither: format.dither.selected_label().to_lowercase(),
            replaygain: format.replaygain.selected_label().to_lowercase(),
            resampler: format.resampler.selected_label().to_lowercase(),
            noise_shaper: Some(format.noise_shaper.selected_label().to_lowercase()),
            modulator_order: Some(format.modulator_order.selected_value().value()),
            dsd_filter_preset: Some(format.conversion_preset.selected_label().to_lowercase()),
            mp3_lossy_preset: Some(format.mp3_lossy_preset_key().to_string()),
            aac_lossy_preset: Some(format.aac_lossy_preset_key().to_string()),
            opus_lossy_preset: Some(format.opus_lossy_preset_key().to_string()),
            mp3_mode: mp3_mode_key(format.mp3_mode).to_string(),
            mp3_quality_preset: format.mp3_quality_preset,
            mp3_vbr_quality: format.mp3_vbr_quality,
            mp3_bitrate_kbps: format.mp3_bitrate_kbps,
            aac_profile: aac_profile_key(format.aac_profile).to_string(),
            aac_quality_preset: format.aac_quality_preset,
            aac_bitrate_kbps: format.aac_bitrate_kbps,
            opus_content_type: opus_content_type_key(format.opus_content_type).to_string(),
            opus_quality_preset: format.opus_quality_preset,
            opus_bitrate_kbps: format.opus_bitrate_kbps,
            opus_complexity: format.opus_complexity,
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
            force_encode: *output_opts.force_encode.selected_value(),
            disc_subfolders: *output_opts.disc_subfolders.selected_value(),
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

        if let Some(value) = parse_mp3_mode(&self.mp3_mode) {
            format_state.mp3_mode = value;
        }
        format_state.mp3_quality_preset = self
            .mp3_quality_preset
            .filter(|idx| *idx < MP3_BITRATE_PRESETS.len());
        format_state.mp3_vbr_quality = self.mp3_vbr_quality.min(9);
        format_state.mp3_bitrate_kbps = self.mp3_bitrate_kbps.clamp(8, 1000);

        if let Some(value) = parse_aac_profile(&self.aac_profile) {
            format_state.aac_profile = value;
        }
        format_state.aac_quality_preset = self
            .aac_quality_preset
            .filter(|idx| *idx < aac_presets_for_profile(format_state.aac_profile).len());
        format_state.aac_bitrate_kbps = self.aac_bitrate_kbps.clamp(8, 1024);

        if let Some(value) = parse_opus_content_type(&self.opus_content_type) {
            format_state.opus_content_type = value;
        }
        format_state.opus_quality_preset = self
            .opus_quality_preset
            .filter(|idx| *idx < OPUS_PRESETS.len());
        format_state.opus_bitrate_kbps = self.opus_bitrate_kbps.clamp(6, 510);
        format_state.opus_complexity = self.opus_complexity.min(10);

        format_state.set_mp3_lossy_preset_from_key(self.mp3_lossy_preset.as_deref());
        format_state.set_aac_lossy_preset_from_key(self.aac_lossy_preset.as_deref());
        format_state.set_opus_lossy_preset_from_key(self.opus_lossy_preset.as_deref());

        // Output options
        if let Some(ref p) = self.dest_path {
            output_opts.dest_path = Some(std::path::PathBuf::from(p));
        }
        output_opts.folder_template = self.folder_template.clone();
        output_opts.filename_template = self.filename_template.clone();
        output_opts.companion_extensions = self.companion_extensions.clone();
        output_opts.companion_folders = self.companion_folders.clone();
        output_opts.force_encode.select_value(&self.force_encode);
        output_opts.disc_subfolders.select_value(&self.disc_subfolders);
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
            mp3_lossy_preset: None,
            aac_lossy_preset: None,
            opus_lossy_preset: None,
            mp3_mode: default_mp3_mode(),
            mp3_quality_preset: None,
            mp3_vbr_quality: default_mp3_vbr_quality(),
            mp3_bitrate_kbps: default_mp3_bitrate_kbps(),
            aac_profile: default_aac_profile(),
            aac_quality_preset: Some(1),
            aac_bitrate_kbps: default_aac_bitrate_kbps(),
            opus_content_type: default_opus_content_type(),
            opus_quality_preset: Some(1),
            opus_bitrate_kbps: default_opus_bitrate_kbps(),
            opus_complexity: default_opus_complexity(),
            dest_path: None,
            folder_template: "%ARTIST%/%ALBUM% (%YEAR%)".to_string(),
            filename_template: "%TRACKNN% - %TITLE%.%EXT%".to_string(),
            merge: merge.to_string(),
            companion_extensions: default_companion_extensions(),
            companion_folders: String::new(),
            force_encode: false,
            disc_subfolders: false,
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
        AudioFormat::Dsf,
        AudioFormat::Dff,
        AudioFormat::Opus,
        AudioFormat::Aac,
        AudioFormat::Mp3,
        AudioFormat::Dts,
        AudioFormat::Ac3,
        AudioFormat::Ape,
        AudioFormat::Lpcm,
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
        AudioFormat::Dsf,
        AudioFormat::Dff,
        AudioFormat::Opus,
        AudioFormat::Aac,
        AudioFormat::Mp3,
        AudioFormat::Dts,
        AudioFormat::Ac3,
        AudioFormat::Ape,
        AudioFormat::Lpcm,
    ];
    let mut result = Vec::new();
    for fmt in &display_order {
        if let Some((_, names)) = groups
            .iter()
            .find(|(stored, _)| parse_format(stored) == Some(*fmt))
        {
            result.push((Some(*fmt), names.clone()));
        }
    }
    // Unknown formats at the end.
    for (fmt_str, names) in &groups {
        let is_known = matches!(parse_format(fmt_str), Some(fmt) if display_order.contains(&fmt));
        if !is_known {
            result.push((None, names.clone()));
        }
    }
    result
}

// ── String → type parsers ────────────────────────────────────────────

fn format_key(format: AudioFormat) -> &'static str {
    match format {
        AudioFormat::Flac => "flac",
        AudioFormat::Wav => "wav",
        AudioFormat::Aiff => "aiff",
        AudioFormat::WavPack => "wavpack",
        AudioFormat::Mp3 => "mp3",
        AudioFormat::Aac => "aac",
        AudioFormat::Opus => "opus",
        AudioFormat::Alac => "alac",
        AudioFormat::Dsf => "dsf",
        AudioFormat::Dff => "dff",
        AudioFormat::Dts => "dts",
        AudioFormat::Ac3 => "ac3",
        AudioFormat::Ape => "ape",
        AudioFormat::Lpcm => "lpcm",
    }
}

fn parse_format(s: &str) -> Option<AudioFormat> {
    let normalized = s.trim().to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "flac" => Some(AudioFormat::Flac),
        "wav" | "wave" => Some(AudioFormat::Wav),
        "aiff" | "aif" => Some(AudioFormat::Aiff),
        "wavpack" | "wav-pack" | "wv" => Some(AudioFormat::WavPack),
        "mp3" => Some(AudioFormat::Mp3),
        "aac" | "m4a-aac" => Some(AudioFormat::Aac),
        "opus" => Some(AudioFormat::Opus),
        "alac" | "m4a-alac" => Some(AudioFormat::Alac),
        "dsf" => Some(AudioFormat::Dsf),
        // Backward compatibility for presets captured while the DSF pill was
        // labeled generically as "DSD".
        "dsd" => Some(AudioFormat::Dsf),
        "dff" | "dsdiff" => Some(AudioFormat::Dff),
        "dts" => Some(AudioFormat::Dts),
        "ac3" | "ac-3" => Some(AudioFormat::Ac3),
        "ape" => Some(AudioFormat::Ape),
        "lpcm" | "pcm" => Some(AudioFormat::Lpcm),
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

fn mp3_mode_key(mode: tonepoet_pipeline::enums::Mp3Mode) -> &'static str {
    use tonepoet_pipeline::enums::Mp3Mode;
    match mode {
        Mp3Mode::Vbr => "vbr",
        Mp3Mode::Cbr => "cbr",
        Mp3Mode::Abr => "abr",
    }
}

fn parse_mp3_mode(s: &str) -> Option<tonepoet_pipeline::enums::Mp3Mode> {
    use tonepoet_pipeline::enums::Mp3Mode;
    match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "vbr" | "variable" | "variable-bitrate" => Some(Mp3Mode::Vbr),
        "cbr" | "constant" | "constant-bitrate" => Some(Mp3Mode::Cbr),
        "abr" | "average" | "average-bitrate" => Some(Mp3Mode::Abr),
        _ => None,
    }
}

fn aac_profile_key(profile: tonepoet_pipeline::enums::AacProfile) -> &'static str {
    use tonepoet_pipeline::enums::AacProfile;
    match profile {
        AacProfile::LcAac => "lc",
        AacProfile::HeAac => "he",
        AacProfile::HeAacV2 => "hev2",
    }
}

fn parse_aac_profile(s: &str) -> Option<tonepoet_pipeline::enums::AacProfile> {
    use tonepoet_pipeline::enums::AacProfile;
    match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "lc" | "lc-aac" | "aac-lc" => Some(AacProfile::LcAac),
        "he" | "he-aac" | "aac-he" => Some(AacProfile::HeAac),
        "hev2" | "he-v2" | "he-aac-v2" | "aac-he-v2" => Some(AacProfile::HeAacV2),
        _ => None,
    }
}

fn opus_content_type_key(content_type: tonepoet_pipeline::enums::OpusContentType) -> &'static str {
    use tonepoet_pipeline::enums::OpusContentType;
    match content_type {
        OpusContentType::Auto => "auto",
        OpusContentType::Music => "music",
        OpusContentType::Speech => "speech",
    }
}

fn parse_opus_content_type(s: &str) -> Option<tonepoet_pipeline::enums::OpusContentType> {
    use tonepoet_pipeline::enums::OpusContentType;
    match s.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "auto" => Some(OpusContentType::Auto),
        "music" => Some(OpusContentType::Music),
        "speech" | "voice" => Some(OpusContentType::Speech),
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
        assert!(!preset.force_encode);
        assert!(!preset.write_log);
    }

    #[test]
    fn companion_fields_round_trip_through_preset_capture_and_apply() {
        let format = FormatState::new();
        let mut output = OutputOptionsState::new();
        output.companion_extensions = ".jpg, .pdf".to_string();
        output.companion_folders = "Scans, Artwork".to_string();
        output.force_encode.select_value(&true);
        output.write_log.select_value(&true);

        let preset = TuiPreset::from_pill_state("companions", &format, &output);
        let mut restored_format = FormatState::new();
        let mut restored_output = OutputOptionsState::new();
        restored_output.companion_extensions.clear();
        restored_output.companion_folders.clear();

        preset.apply_to_pills(&mut restored_format, &mut restored_output);

        assert_eq!(restored_output.companion_extensions, ".jpg, .pdf");
        assert_eq!(restored_output.companion_folders, "Scans, Artwork");
        assert!(*restored_output.force_encode.selected_value());
        assert!(*restored_output.write_log.selected_value());
    }

    #[test]
    fn lossy_codec_settings_round_trip_through_preset_capture_and_apply() {
        use tonepoet_pipeline::enums::{AacProfile, Mp3Mode, OpusContentType};

        let mut format = FormatState::new();
        format.mp3_mode = Mp3Mode::Vbr;
        format.mp3_quality_preset = None;
        format.mp3_lossy_preset = None;
        format.mp3_vbr_quality = 2;
        format.mp3_bitrate_kbps = 190;
        format.aac_profile = AacProfile::HeAac;
        format.aac_quality_preset = Some(1);
        format.aac_lossy_preset = None;
        format.aac_bitrate_kbps = 80;
        format.opus_content_type = OpusContentType::Speech;
        format.opus_quality_preset = Some(4);
        format.opus_lossy_preset = Some(2);
        format.opus_bitrate_kbps = 64;
        format.opus_complexity = 6;

        let output = OutputOptionsState::new();
        let preset = TuiPreset::from_pill_state("lossy", &format, &output);

        assert_eq!(preset.mp3_lossy_preset.as_deref(), Some("custom"));
        assert_eq!(preset.aac_lossy_preset.as_deref(), Some("custom"));
        assert_eq!(preset.opus_lossy_preset.as_deref(), Some("64"));
        assert_eq!(preset.mp3_mode, "vbr");
        assert_eq!(preset.mp3_vbr_quality, 2);
        assert_eq!(preset.mp3_bitrate_kbps, 190);
        assert_eq!(preset.aac_profile, "he");
        assert_eq!(preset.aac_bitrate_kbps, 80);
        assert_eq!(preset.opus_content_type, "speech");
        assert_eq!(preset.opus_bitrate_kbps, 64);
        assert_eq!(preset.opus_complexity, 6);

        let mut restored_format = FormatState::new();
        let mut restored_output = OutputOptionsState::new();
        preset.apply_to_pills(&mut restored_format, &mut restored_output);

        assert_eq!(restored_format.mp3_lossy_preset, None);
        assert_eq!(restored_format.aac_lossy_preset, None);
        assert_eq!(restored_format.opus_lossy_preset, Some(2));
        assert_eq!(restored_format.mp3_mode, Mp3Mode::Vbr);
        assert_eq!(restored_format.mp3_quality_preset, None);
        assert_eq!(restored_format.mp3_vbr_quality, 2);
        assert_eq!(restored_format.mp3_bitrate_kbps, 190);
        assert_eq!(restored_format.aac_profile, AacProfile::HeAac);
        assert_eq!(restored_format.aac_quality_preset, Some(1));
        assert_eq!(restored_format.aac_bitrate_kbps, 80);
        assert_eq!(restored_format.opus_content_type, OpusContentType::Speech);
        assert_eq!(restored_format.opus_quality_preset, Some(4));
        assert_eq!(restored_format.opus_bitrate_kbps, 64);
        assert_eq!(restored_format.opus_complexity, 6);
    }

    #[test]
    fn named_lossy_preset_keys_round_trip_through_toml() {
        let mut format = FormatState::new();
        let output = OutputOptionsState::new();

        format.format.select_value(&AudioFormat::Mp3);
        assert!(format.select_lossy_preset_index(1));
        format.format.select_value(&AudioFormat::Aac);
        assert!(format.select_lossy_preset_index(0));
        format.format.select_value(&AudioFormat::Opus);
        assert!(format.select_lossy_preset_index(2));

        let preset = TuiPreset::from_pill_state("named-lossy", &format, &output);
        let toml = toml::to_string(&preset).expect("serialize preset");
        assert!(toml.contains("mp3_lossy_preset = \"v2\""));
        assert!(toml.contains("aac_lossy_preset = \"256-vbr\""));
        assert!(toml.contains("opus_lossy_preset = \"64\""));

        let restored: TuiPreset = toml::from_str(&toml).expect("deserialize preset");
        let mut restored_format = FormatState::new();
        let mut restored_output = OutputOptionsState::new();
        restored.apply_to_pills(&mut restored_format, &mut restored_output);

        restored_format.format.select_value(&AudioFormat::Mp3);
        assert_eq!(restored_format.lossy_preset_index(), Some(1));
        restored_format.format.select_value(&AudioFormat::Aac);
        assert_eq!(restored_format.lossy_preset_index(), Some(0));
        restored_format.format.select_value(&AudioFormat::Opus);
        assert_eq!(restored_format.lossy_preset_index(), Some(2));
    }

    #[test]
    fn explicit_custom_lossy_preset_keys_round_trip_through_toml() {
        let mut format = FormatState::new();
        let output = OutputOptionsState::new();

        format.format.select_value(&AudioFormat::Mp3);
        assert!(format.select_lossy_preset_index(0));
        assert!(format.select_lossy_preset_index(3));
        format.format.select_value(&AudioFormat::Aac);
        assert!(format.select_lossy_preset_index(0));
        assert!(format.select_lossy_preset_index(3));
        format.format.select_value(&AudioFormat::Opus);
        assert!(format.select_lossy_preset_index(0));
        assert!(format.select_lossy_preset_index(3));

        let preset = TuiPreset::from_pill_state("custom-lossy", &format, &output);
        let toml = toml::to_string(&preset).expect("serialize preset");
        assert!(toml.contains("mp3_lossy_preset = \"custom\""));
        assert!(toml.contains("aac_lossy_preset = \"custom\""));
        assert!(toml.contains("opus_lossy_preset = \"custom\""));

        let restored: TuiPreset = toml::from_str(&toml).expect("deserialize preset");
        let mut restored_format = FormatState::new();
        let mut restored_output = OutputOptionsState::new();
        restored.apply_to_pills(&mut restored_format, &mut restored_output);

        restored_format.format.select_value(&AudioFormat::Mp3);
        assert_eq!(restored_format.lossy_preset_index(), Some(3));
        restored_format.format.select_value(&AudioFormat::Aac);
        assert_eq!(restored_format.lossy_preset_index(), Some(3));
        restored_format.format.select_value(&AudioFormat::Opus);
        assert_eq!(restored_format.lossy_preset_index(), Some(3));
    }

    #[test]
    fn older_tui_presets_get_lossy_codec_defaults() {
        let preset: TuiPreset = toml::from_str(
            r#"
name = "legacy-lossy"
version = 3
format = "mp3"
sample_rate = 44100
bit_depth = "24"
dither = "tpdf"
replaygain = "off"
folder_template = "%ARTIST%/%ALBUM%"
filename_template = "%TRACKNN% - %TITLE%.%EXT%"
merge = "multi-file"
"#,
        )
        .expect("preset without lossy codec fields should deserialize");

        assert_eq!(preset.mp3_lossy_preset, None);
        assert_eq!(preset.aac_lossy_preset, None);
        assert_eq!(preset.opus_lossy_preset, None);
        assert_eq!(preset.mp3_mode, "vbr");
        assert_eq!(preset.mp3_vbr_quality, 0);
        assert_eq!(preset.mp3_bitrate_kbps, 320);
        assert_eq!(preset.aac_profile, "lc");
        assert_eq!(preset.aac_bitrate_kbps, 256);
        assert_eq!(preset.opus_content_type, "auto");
        assert_eq!(preset.opus_bitrate_kbps, 192);
        assert_eq!(preset.opus_complexity, 10);
    }
}
