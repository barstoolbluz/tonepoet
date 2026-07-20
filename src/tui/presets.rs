//! TUI preset management: save/load pill state to TOML files

use std::fs;
use std::io::Write;
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
    crate::convert::formats::default_companion_extensions().join(", ")
}

fn normalize_optional_text_override(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}


/// A TUI-native preset that stores pill values directly
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuiPreset {
    pub name: String,
    pub description: Option<String>,
    pub version: u32, // 2 = legacy TUI, 3 = dynamic pane, 4 = native DSD Reference

    // Format pane
    pub format: String, // "flac", "opus", etc.
    pub sample_rate: u32,
    pub bit_depth: String,  // "source", "16", "24", "32", "32f", "64f"
    pub dither: String,     // "tpdf", "none", "shibata", ...
    pub replaygain: String, // "album", "track", "both", optional "-if-missing", or "off"
    #[serde(default = "default_resampler")]
    pub resampler: String, // "sox", "ssrc", "soxr"
    #[serde(default)]
    pub noise_shaper: Option<String>, // "clans", "sdm", "crfb"
    #[serde(default)]
    pub modulator_order: Option<u8>, // 4-8
    #[serde(default)]
    pub dsd_filter_preset: Option<String>, // "auto", "sinc"
    /// Canonical resolved codec/container identity. Required by v4.
    #[serde(default)]
    pub output_target: Option<String>,
    /// Native-v2 DSD-source pathway reservation.
    #[serde(default)]
    pub dsd_path: Option<String>,
    /// Native-v2 DSD reconstruction profile.
    #[serde(default)]
    pub dsd_profile: Option<String>,
    /// Native-v2 DSD gain mode.
    #[serde(default)]
    pub dsd_gain: Option<String>,
    /// Canonical fixed gain text.
    #[serde(default)]
    pub dsd_gain_db: Option<String>,
    /// Canonical NormalizePeak target text.
    #[serde(default)]
    pub dsd_normalize_target_dbfs: Option<String>,

    // Metadata pane
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_artist_for_conversion: Option<String>,

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
    pub companion_exclude_files: String,
    #[serde(default)]
    pub force_encode: bool,
    #[serde(default)]
    pub disc_subfolders: bool,
    #[serde(default)]
    pub write_log: bool,
    #[serde(default, skip_serializing_if = "crate::convert::pipeline::ActionPipeline::is_empty")]
    pub actions: crate::convert::pipeline::ActionPipeline,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetWireLegacy {
    name: String,
    description: Option<String>,
    version: u32,
    format: String,
    sample_rate: u32,
    bit_depth: String,
    dither: String,
    replaygain: String,
    #[serde(default = "default_resampler")]
    resampler: String,
    #[serde(default)]
    noise_shaper: Option<String>,
    #[serde(default)]
    modulator_order: Option<u8>,
    #[serde(default)]
    dsd_filter_preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    album_artist_for_conversion: Option<String>,
    #[serde(default)]
    dest_path: Option<String>,
    folder_template: String,
    filename_template: String,
    merge: String,
    #[serde(default = "default_companion_extensions")]
    companion_extensions: String,
    #[serde(default)]
    companion_folders: String,
    #[serde(default)]
    companion_exclude_files: String,
    #[serde(default)]
    force_encode: bool,
    #[serde(default)]
    disc_subfolders: bool,
    #[serde(default)]
    write_log: bool,
    #[serde(default)]
    actions: crate::convert::pipeline::ActionPipeline,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetWireV4 {
    name: String,
    description: Option<String>,
    version: u32,
    format: String,
    sample_rate: u32,
    bit_depth: String,
    dither: String,
    replaygain: String,
    #[serde(default = "default_resampler")]
    resampler: String,
    #[serde(default)]
    noise_shaper: Option<String>,
    #[serde(default)]
    modulator_order: Option<u8>,
    #[serde(default)]
    dsd_filter_preset: Option<String>,
    output_target: String,
    #[serde(default)]
    dsd_path: Option<String>,
    #[serde(default)]
    dsd_profile: Option<String>,
    #[serde(default)]
    dsd_gain: Option<String>,
    #[serde(default)]
    dsd_gain_db: Option<String>,
    #[serde(default)]
    dsd_normalize_target_dbfs: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    album_artist_for_conversion: Option<String>,
    #[serde(default)]
    dest_path: Option<String>,
    folder_template: String,
    filename_template: String,
    merge: String,
    #[serde(default = "default_companion_extensions")]
    companion_extensions: String,
    #[serde(default)]
    companion_folders: String,
    #[serde(default)]
    companion_exclude_files: String,
    #[serde(default)]
    force_encode: bool,
    #[serde(default)]
    disc_subfolders: bool,
    #[serde(default)]
    write_log: bool,
    #[serde(default)]
    actions: crate::convert::pipeline::ActionPipeline,
}

// V2 and V3 deliberately retain the same historical field schema, but use
// distinct transparent wire types. Dispatch and migration can therefore evolve
// independently without allowing one version to fall through to another.
#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct PresetWireV2(PresetWireLegacy);

#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct PresetWireV3(PresetWireLegacy);

impl PresetWireV2 {
    fn version(&self) -> u32 {
        self.0.version
    }

    fn into_preset(self) -> TuiPreset {
        self.0.into_preset()
    }
}

impl PresetWireV3 {
    fn version(&self) -> u32 {
        self.0.version
    }

    fn into_preset(self) -> TuiPreset {
        self.0.into_preset()
    }
}

impl PresetWireLegacy {
    fn into_preset(self) -> TuiPreset {
        TuiPreset {
            name: self.name,
            description: self.description,
            version: self.version,
            format: self.format,
            sample_rate: self.sample_rate,
            bit_depth: self.bit_depth,
            dither: self.dither,
            replaygain: self.replaygain,
            resampler: self.resampler,
            noise_shaper: self.noise_shaper,
            modulator_order: self.modulator_order,
            dsd_filter_preset: self.dsd_filter_preset,
            output_target: None,
            dsd_path: None,
            dsd_profile: None,
            dsd_gain: None,
            dsd_gain_db: None,
            dsd_normalize_target_dbfs: None,
            album_artist_for_conversion: self.album_artist_for_conversion,
            dest_path: self.dest_path,
            folder_template: self.folder_template,
            filename_template: self.filename_template,
            merge: self.merge,
            companion_extensions: self.companion_extensions,
            companion_folders: self.companion_folders,
            companion_exclude_files: self.companion_exclude_files,
            force_encode: self.force_encode,
            disc_subfolders: self.disc_subfolders,
            write_log: self.write_log,
            actions: self.actions,
        }
    }
}

impl PresetWireV4 {
    fn into_preset(self, path: &Path) -> Result<TuiPreset, String> {
        let output_target = self.output_target.trim();
        if output_target.is_empty() {
            return Err(format!(
                "Preset v4 '{}' has an empty required output_target",
                path.display()
            ));
        }
        Ok(TuiPreset {
            name: self.name,
            description: self.description,
            version: self.version,
            format: self.format,
            sample_rate: self.sample_rate,
            bit_depth: self.bit_depth,
            dither: self.dither,
            replaygain: self.replaygain,
            resampler: self.resampler,
            noise_shaper: self.noise_shaper,
            modulator_order: self.modulator_order,
            dsd_filter_preset: self.dsd_filter_preset,
            output_target: Some(output_target.to_string()),
            dsd_path: self.dsd_path,
            dsd_profile: self.dsd_profile,
            dsd_gain: self.dsd_gain,
            dsd_gain_db: self.dsd_gain_db,
            dsd_normalize_target_dbfs: self.dsd_normalize_target_dbfs,
            album_artist_for_conversion: self.album_artist_for_conversion,
            dest_path: self.dest_path,
            folder_template: self.folder_template,
            filename_template: self.filename_template,
            merge: self.merge,
            companion_extensions: self.companion_extensions,
            companion_folders: self.companion_folders,
            companion_exclude_files: self.companion_exclude_files,
            force_encode: self.force_encode,
            disc_subfolders: self.disc_subfolders,
            write_log: self.write_log,
            actions: self.actions,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PresetApplyReport {
    /// Preset fields whose values could not be parsed or whose pill option was
    /// unavailable/disabled under the resulting format constraints.
    pub refused_fields: Vec<String>,
}

impl PresetApplyReport {
    pub fn is_complete(&self) -> bool {
        self.refused_fields.is_empty()
    }

    pub fn status_suffix(&self) -> String {
        if self.refused_fields.is_empty() {
            String::new()
        } else {
            format!("; refused fields: {}", self.refused_fields.join(", "))
        }
    }

    fn record(&mut self, field: &str, applied: bool) {
        if !applied && !self.refused_fields.iter().any(|existing| existing == field) {
            self.refused_fields.push(field.to_string());
        }
    }
}

impl TuiPreset {
    /// Capture current pill state into a preset
    pub fn from_pill_state(
        name: &str,
        format: &FormatState,
        output_opts: &OutputOptionsState,
        metadata: &MetadataState,
    ) -> Self {
        let dsd_gain_parameter = if !format.dsd_reference_controls_available()
            && *format.dsd_gain_mode.selected_value() == DsdGainMode::Auto
        {
            format.dsd_auto_gain_margin_db.render(false)
        } else {
            format.dsd_normalize_target_dbfs.render(false)
        };
        Self {
            name: name.to_string(),
            description: None,
            version: 4,
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
            output_target: format
                .format
                .selected_value()
                .resolved_output_target(format.selected_container())
                .map(|target| target.key().to_string()),
            dsd_path: Some(match *format.dsd_pathway.selected_value() {
                tonepoet_pipeline::DsdSourcePathway::Reference => "reference",
                tonepoet_pipeline::DsdSourcePathway::Manual => "manual",
            }
            .to_string()),
            dsd_profile: Some(match *format.dsd_profile.selected_value() {
                tonepoet_pipeline::DsdReconstructionSelection::Reference => "reference",
                tonepoet_pipeline::DsdReconstructionSelection::Wideband => "wideband",
            }
            .to_string()),
            dsd_gain: Some(format.dsd_gain_mode.selected_value().preset_key().to_string()),
            dsd_gain_db: Some(format.dsd_gain_db.render(false)),
            // v4 has one auxiliary DSD-gain scalar. Pre-promotion Auto
            // stores its positive legacy margin directly. The `dsd_gain` key
            // distinguishes Auto from native NormalizePeak; apply still accepts
            // a negative historical token by magnitude for compatibility.
            dsd_normalize_target_dbfs: Some(dsd_gain_parameter),
            album_artist_for_conversion: normalize_optional_text_override(
                metadata.album_artist_for_conversion.as_deref(),
            ),
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
            companion_exclude_files: output_opts.companion_exclude_files.clone(),
            force_encode: *output_opts.force_encode.selected_value(),
            disc_subfolders: *output_opts.disc_subfolders.selected_value(),
            write_log: *output_opts.write_log.selected_value(),
            actions: output_opts.actions.clone(),
        }
    }

    /// Apply preset values to pill state. Refusal is derived exclusively
    /// from the preset's own format and constraints; the pills' previous
    /// selected values never turn a disabled request into a success.
    fn select_enabled<T: PartialEq + Clone>(
        pill: &mut crate::tui::pill::PillState<T>,
        value: &T,
    ) -> bool {
        pill.select_value(value)
    }

    pub fn apply_to_pills(
        &self,
        format_state: &mut FormatState,
        output_opts: &mut OutputOptionsState,
        metadata: &mut MetadataState,
    ) -> PresetApplyReport {
        let mut report = PresetApplyReport::default();

        let Some(preset_format) = parse_format(&self.format) else {
            report.record("format", false);
            return report;
        };
        report.record(
            "format",
            Self::select_enabled(&mut format_state.format, &preset_format),
        );
        if *format_state.format.selected_value() != preset_format {
            // Do not interpret downstream fields under the pre-apply format.
            // The caller rolls the entire application back on this refusal.
            return report;
        }
        format_state.apply_format_constraints();
        if self.version < 4 {
            format_state.reference_target_confirmed = false;
        }

        if self.version >= 4 {
            let target_applied = self.output_target.as_deref().is_some_and(|wanted| {
                let containers = preset_format.available_containers();
                containers.iter().enumerate().find_map(|(index, container)| {
                    preset_format
                        .resolved_output_target(container)
                        .filter(|target| target.key() == wanted)
                        .map(|_| index)
                })
                .map(|index| {
                    format_state.selected_container_index = index;
                    format_state.reference_target_confirmed = true;
                    true
                })
                .unwrap_or(false)
            });
            report.record("output_target", target_applied);

            // DSD-source fields are meaningful only for an actual DSD-to-PCM
            // conversion. Irrelevant fields are ignored rather than reported as
            // refusals, so preset application is independent of disabled stale pills.
            if format_state.dsd_to_pcm_gain_available() {
                let native = format_state.dsd_reference_controls_available();
                if native {
                    match self.dsd_path.as_deref().unwrap_or("reference") {
                        "reference" => report.record(
                            "dsd_path",
                            format_state.dsd_pathway.select_value(
                                &tonepoet_pipeline::DsdSourcePathway::Reference,
                            ),
                        ),
                        _ => report.record("dsd_path", false),
                    }
                    match self.dsd_profile.as_deref().unwrap_or("reference") {
                        "reference" => report.record(
                            "dsd_profile",
                            format_state.dsd_profile.select_value(
                                &tonepoet_pipeline::DsdReconstructionSelection::Reference,
                            ),
                        ),
                        "wideband" => report.record(
                            "dsd_profile",
                            format_state.dsd_profile.select_value(
                                &tonepoet_pipeline::DsdReconstructionSelection::Wideband,
                            ),
                        ),
                        _ => report.record("dsd_profile", false),
                    }
                } else {
                    // The v4 defaults `reference/reference` carry no legacy
                    // behavior and are accepted as no-ops. Native-only requests
                    // remain explicit refusals before promotion.
                    if !matches!(self.dsd_path.as_deref(), None | Some("reference")) {
                        report.record("dsd_path", false);
                    }
                    if !matches!(self.dsd_profile.as_deref(), None | Some("reference")) {
                        report.record("dsd_profile", false);
                    }
                }

                let raw_gain = self.dsd_gain.as_deref().unwrap_or("reference");
                let gain_mode = if native {
                    match raw_gain {
                        "reference" => Some(DsdGainMode::Reference),
                        "native" => Some(DsdGainMode::NativeLevel),
                        "fixed" | "manual" => Some(DsdGainMode::Fixed),
                        "normalize" => Some(DsdGainMode::NormalizePeak),
                        _ => None,
                    }
                } else {
                    match raw_gain {
                        // Old v4 presets captured `reference` even while the
                        // exact legacy default was Disabled. Preserve that
                        // behavior during pre-promotion application.
                        "reference" | "disabled" => Some(DsdGainMode::Disabled),
                        "auto" | "normalize" => Some(DsdGainMode::Auto),
                        "fixed" | "manual" => Some(DsdGainMode::Fixed),
                        _ => None,
                    }
                };
                if let Some(mode) = gain_mode {
                    report.record("dsd_gain", format_state.dsd_gain_mode.select_value(&mode));
                    if mode == DsdGainMode::Fixed {
                        if let Some(raw) = self.dsd_gain_db.as_deref() {
                            match raw.parse::<tonepoet_pipeline::DbNano>() {
                                Ok(value)
                                    if (tonepoet_pipeline::DbNano::MIN_FIXED_GAIN
                                        ..=tonepoet_pipeline::DbNano::MAX_FIXED_GAIN)
                                        .contains(&value) =>
                                {
                                    format_state.dsd_gain_db = value;
                                    report.record("dsd_gain_db", true);
                                }
                                _ => report.record("dsd_gain_db", false),
                            }
                        }
                    }
                    if matches!(mode, DsdGainMode::Auto | DsdGainMode::NormalizePeak) {
                        if let Some(raw) = self.dsd_normalize_target_dbfs.as_deref() {
                            match raw.parse::<tonepoet_pipeline::DbNano>() {
                                Ok(value) if mode == DsdGainMode::Auto => {
                                    let magnitude = value.0.unsigned_abs();
                                    if magnitude <= 6_000_000_000 {
                                        format_state.dsd_auto_gain_margin_db =
                                            tonepoet_pipeline::DbNano(magnitude as i64);
                                        report.record("dsd_normalize_target_dbfs", true);
                                    } else {
                                        report.record("dsd_normalize_target_dbfs", false);
                                    }
                                }
                                Ok(value)
                                    if (tonepoet_pipeline::DbNano::MIN_NORMALIZE_TARGET
                                        ..=tonepoet_pipeline::DbNano::MAX_NORMALIZE_TARGET)
                                        .contains(&value) =>
                                {
                                    format_state.dsd_normalize_target_dbfs = value;
                                    report.record("dsd_normalize_target_dbfs", true);
                                }
                                _ => report.record("dsd_normalize_target_dbfs", false),
                            }
                        }
                    }
                } else {
                    report.record("dsd_gain", false);
                }
            }
        }

        let is_dsd = matches!(preset_format, AudioFormat::Dsf | AudioFormat::Dff);
        let pcm_depth_is_meaningful = matches!(
            preset_format,
            AudioFormat::Flac
                | AudioFormat::Wav
                | AudioFormat::WavPack
                | AudioFormat::Aiff
                | AudioFormat::Alac
                | AudioFormat::Lpcm
                | AudioFormat::Ape
                | AudioFormat::Shorten
                | AudioFormat::Tta
        );

        let sample_rate_applied =
            Self::select_enabled(&mut format_state.sample_rate, &self.sample_rate);
        report.record("sample_rate", sample_rate_applied);
        if sample_rate_applied {
            format_state.mark_sample_rate_user_policy();
        }

        if pcm_depth_is_meaningful {
            match parse_bit_depth(&self.bit_depth) {
                Some(value) => {
                    let applied = Self::select_enabled(&mut format_state.bit_depth, &value);
                    report.record("bit_depth", applied);
                    if applied {
                        format_state.mark_bit_depth_user_policy();
                    }
                }
                None => report.record("bit_depth", false),
            }
            match parse_dither(&self.dither) {
                Some(value) => {
                    let applied = Self::select_enabled(&mut format_state.dither, &value);
                    report.record("dither", applied);
                    if applied {
                        format_state.dither_overridden = true;
                    }
                }
                None => report.record("dither", false),
            }
        }

        if !is_dsd {
            match parse_replaygain(&self.replaygain) {
                Some(value) => report.record(
                    "replaygain",
                    Self::select_enabled(&mut format_state.replaygain, &value),
                ),
                None => report.record("replaygain", false),
            }
            match parse_resampler(&self.resampler) {
                Some(value) => {
                    let applied = Self::select_enabled(&mut format_state.resampler, &value);
                    report.record("resampler", applied);
                    if applied {
                        format_state.resampler_overridden = true;
                    }
                }
                None => report.record("resampler", false),
            }
        } else {
            if let Some(ref serialized) = self.noise_shaper {
                match parse_noise_shaper(serialized) {
                    Some(value) => report.record(
                        "noise_shaper",
                        Self::select_enabled(&mut format_state.noise_shaper, &value),
                    ),
                    None => report.record("noise_shaper", false),
                }
            }
            if let Some(serialized) = self.modulator_order {
                match parse_modulator_order(serialized) {
                    Some(value) => report.record(
                        "modulator_order",
                        Self::select_enabled(&mut format_state.modulator_order, &value),
                    ),
                    None => report.record("modulator_order", false),
                }
            }
            if let Some(ref serialized) = self.dsd_filter_preset {
                match parse_dsd_filter_preset(serialized) {
                    Some(value) => report.record(
                        "dsd_filter_preset",
                        Self::select_enabled(&mut format_state.conversion_preset, &value),
                    ),
                    None => report.record("dsd_filter_preset", false),
                }
            }
        }

        if let Some(ref path) = self.dest_path {
            output_opts.dest_path = Some(std::path::PathBuf::from(path));
        }
        output_opts.folder_template = self.folder_template.clone();
        output_opts.filename_template = self.filename_template.clone();
        output_opts.companion_extensions = self.companion_extensions.clone();
        output_opts.companion_folders = self.companion_folders.clone();
        output_opts.companion_exclude_files = self.companion_exclude_files.clone();
        report.record(
            "force_encode",
            Self::select_enabled(&mut output_opts.force_encode, &self.force_encode),
        );
        report.record(
            "disc_subfolders",
            Self::select_enabled(&mut output_opts.disc_subfolders, &self.disc_subfolders),
        );
        report.record(
            "write_log",
            Self::select_enabled(&mut output_opts.write_log, &self.write_log),
        );
        output_opts.actions = self.actions.clone();

        match parse_merge(&self.merge) {
            Some(value) => report.record(
                "merge",
                Self::select_enabled(&mut output_opts.merge, &value),
            ),
            None => report.record("merge", false),
        }

        metadata.album_artist_for_conversion = normalize_optional_text_override(
            self.album_artist_for_conversion.as_deref(),
        );

        // Re-run the cascade and verify every semantically meaningful value
        // that was accepted remains selected. A constraint snap is a refusal,
        // not a silent substitution.
        let expected_sample_rate = self.sample_rate;
        let expected_bit_depth = pcm_depth_is_meaningful
            .then(|| parse_bit_depth(&self.bit_depth))
            .flatten();
        let expected_dither = pcm_depth_is_meaningful
            .then(|| parse_dither(&self.dither))
            .flatten();
        let expected_replaygain = (!is_dsd)
            .then(|| parse_replaygain(&self.replaygain))
            .flatten();
        let expected_resampler = (!is_dsd)
            .then(|| parse_resampler(&self.resampler))
            .flatten();
        let expected_noise_shaper = is_dsd
            .then(|| self.noise_shaper.as_deref().and_then(parse_noise_shaper))
            .flatten();
        let expected_modulator = is_dsd
            .then(|| self.modulator_order.and_then(parse_modulator_order))
            .flatten();
        let expected_filter = is_dsd
            .then(|| {
                self.dsd_filter_preset
                    .as_deref()
                    .and_then(parse_dsd_filter_preset)
            })
            .flatten();

        format_state.apply_format_constraints();
        report.record(
            "format",
            *format_state.format.selected_value() == preset_format,
        );
        report.record(
            "sample_rate",
            *format_state.sample_rate.selected_value() == expected_sample_rate,
        );
        if let Some(value) = expected_bit_depth {
            report.record("bit_depth", format_state.bit_depth.selected_value() == &value);
        }
        if let Some(value) = expected_dither {
            report.record("dither", format_state.dither.selected_value() == &value);
        }
        if let Some(value) = expected_replaygain {
            report.record(
                "replaygain",
                format_state.replaygain.selected_value() == &value,
            );
        }
        if let Some(value) = expected_resampler {
            report.record(
                "resampler",
                format_state.resampler.selected_value() == &value,
            );
        }
        if let Some(value) = expected_noise_shaper {
            report.record(
                "noise_shaper",
                format_state.noise_shaper.selected_value() == &value,
            );
        }
        if let Some(value) = expected_modulator {
            report.record(
                "modulator_order",
                format_state.modulator_order.selected_value() == &value,
            );
        }
        if let Some(value) = expected_filter {
            report.record(
                "dsd_filter_preset",
                format_state.conversion_preset.selected_value() == &value,
            );
        }

        report
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
            output_target: None,
            dsd_path: None,
            dsd_profile: None,
            dsd_gain: None,
            dsd_gain_db: None,
            dsd_normalize_target_dbfs: None,
            album_artist_for_conversion: None,
            dest_path: None,
            folder_template: "%ARTIST%/%ALBUM% (%YEAR%)".to_string(),
            filename_template: "%TRACKNN% - %TITLE%.%EXT%".to_string(),
            merge: merge.to_string(),
            companion_extensions: default_companion_extensions(),
            companion_folders: String::new(),
            companion_exclude_files: String::new(),
            force_encode: false,
            disc_subfolders: false,
            write_log: false,
            actions: crate::convert::pipeline::ActionPipeline::default(),
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

    let root = contents
        .parse::<toml::Value>()
        .map_err(|error| format!("Failed to parse preset '{}': {error}", path.display()))?;
    let version = root.get("version").and_then(toml::Value::as_integer);
    if let Some(version) = version {
        let version = u32::try_from(version).map_err(|_| {
            format!("Invalid negative preset version in '{}'", path.display())
        })?;
        let mut preset = match version {
            2 => {
                let wire = toml::from_str::<PresetWireV2>(&contents).map_err(|error| {
                    format!("Invalid preset v2 '{}': {error}", path.display())
                })?;
                if wire.version() != 2 {
                    return Err(format!(
                        "Preset version changed while parsing '{}'",
                        path.display()
                    ));
                }
                wire.into_preset()
            }
            3 => {
                let wire = toml::from_str::<PresetWireV3>(&contents).map_err(|error| {
                    format!("Invalid preset v3 '{}': {error}", path.display())
                })?;
                if wire.version() != 3 {
                    return Err(format!(
                        "Preset version changed while parsing '{}'",
                        path.display()
                    ));
                }
                wire.into_preset()
            }
            4 => {
                let wire = toml::from_str::<PresetWireV4>(&contents).map_err(|error| {
                    format!("Invalid preset v4 '{}': {error}", path.display())
                })?;
                if wire.version != 4 {
                    return Err(format!(
                        "Preset version changed while parsing '{}'",
                        path.display()
                    ));
                }
                wire.into_preset(path)?
            }
            _ => {
                return Err(format!(
                    "Unsupported preset version {version} in '{}'; supported versions are 2, 3, and 4",
                    path.display()
                ));
            }
        };
        preset.name = display_name.to_string();
        return Ok(preset);
    }

    // Versionless files are the only inputs eligible for the legacy wizard wire.
    let mut preset = TuiPreset::from_legacy(
        &toml::from_str::<tonepoet_wizard::ConversionPreset>(&contents)
            .map_err(|error| format!("Invalid versionless legacy preset '{}': {error}", path.display()))?,
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
    if preset.version != 4 {
        return Err(format!(
            "Refusing to save preset version {}; current saves require version 4",
            preset.version
        ));
    }
    if preset.output_target.as_deref().map(str::trim).filter(|value| !value.is_empty()).is_none() {
        return Err("Refusing to save preset v4 without output_target".to_string());
    }
    let contents = toml::to_string_pretty(preset)
        .map_err(|error| format!("Failed to serialize preset: {error}"))?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("Failed to create preset temporary file in '{}': {error}", parent.display()))?;
    temp.write_all(contents.as_bytes())
        .and_then(|_| temp.as_file_mut().flush())
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|error| format!("Failed to durably write preset '{}': {error}", path.display()))?;
    temp.persist(path)
        .map_err(|error| format!("Failed to atomically replace preset '{}': {}", path.display(), error.error))?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("Failed to sync preset directory '{}': {error}", parent.display()))?;

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
        AudioFormat::Shorten => "shn",
        AudioFormat::Ogg => "ogg",
        AudioFormat::Tta => "tta",
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
        "shn" => Some(AudioFormat::Shorten),
        "ogg" => Some(AudioFormat::Ogg),
        "tta" => Some(AudioFormat::Tta),
        "lpcm" | "pcm" => Some(AudioFormat::Lpcm),
        _ => None,
    }
}

fn parse_bit_depth(s: &str) -> Option<BitDepthChoice> {
    match s {
        "source" => Some(BitDepthChoice::Source),
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
        // from_pill_state serializes the pill label lowercased, and the
        // default pill selection is None — the parser must round-trip it.
        "none" | "off" => Some(ResamplerChoice::None),
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
        "album if missing" | "album-if-missing" => Some(ReplayGainChoice::AlbumIfMissing),
        "track if missing" | "track-if-missing" => Some(ReplayGainChoice::TrackIfMissing),
        "both if missing" | "both-if-missing" => Some(ReplayGainChoice::BothIfMissing),
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
    fn legacy_v3_preset_requires_explicit_reference_target_reconfirmation() {
        let preset: TuiPreset = toml::from_str(
            r#"
name = "legacy-reference"
version = 3
format = "flac"
sample_rate = 176400
bit_depth = "24"
dither = "tpdf"
replaygain = "off"
folder_template = "%ARTIST%/%ALBUM%"
filename_template = "%TRACKNN% - %TITLE%.%EXT%"
merge = "multi-file"
"#,
        )
        .expect("legacy preset parses");
        let mut format = FormatState::new();
        let mut output = OutputOptionsState::new();
        let mut metadata = MetadataState::default();
        let report = preset.apply_to_pills(&mut format, &mut output, &mut metadata);
        assert!(report.is_complete());
        assert!(!format.reference_target_confirmed);
    }

    #[test]
    fn native_v4_preset_confirms_exact_reference_target() {
        let format = FormatState::new();
        let output = OutputOptionsState::new();
        let metadata = MetadataState::default();
        let preset = TuiPreset::from_pill_state("native-reference", &format, &output, &metadata);
        assert_eq!(preset.version, 4);
        assert!(preset.output_target.is_some());

        let mut restored_format = FormatState::new();
        restored_format.reference_target_confirmed = false;
        let mut restored_output = OutputOptionsState::new();
        let mut restored_metadata = MetadataState::default();
        let report = preset.apply_to_pills(
            &mut restored_format,
            &mut restored_output,
            &mut restored_metadata,
        );
        assert!(report.is_complete());
        assert!(restored_format.reference_target_confirmed);
    }

    #[test]
    fn versioned_preset_loader_rejects_unknown_and_cross_version_fields() {
        let temp = tempfile::tempdir().expect("preset tempdir");
        let v3 = temp.path().join("bad-v3.toml");
        std::fs::write(
            &v3,
            r#"
name = "bad-v3"
version = 3
format = "flac"
sample_rate = 176400
bit_depth = "24"
dither = "tpdf"
replaygain = "off"
output_target = "flac_native"
folder_template = "%ARTIST%/%ALBUM%"
filename_template = "%TRACKNN% - %TITLE%.%EXT%"
merge = "multi-file"
"#,
        )
        .expect("write bad v3 preset");
        let error = load_preset_from_path(&v3).expect_err("v3 must reject v4 fields");
        assert!(error.contains("output_target"));

        let v4 = temp.path().join("bad-v4.toml");
        std::fs::write(
            &v4,
            r#"
name = "bad-v4"
version = 4
format = "flac"
sample_rate = 176400
bit_depth = "24"
dither = "tpdf"
replaygain = "off"
output_target = "flac_native"
folder_template = "%ARTIST%/%ALBUM%"
filename_template = "%TRACKNN% - %TITLE%.%EXT%"
merge = "multi-file"
future_field = true
"#,
        )
        .expect("write bad v4 preset");
        let error = load_preset_from_path(&v4).expect_err("v4 must reject unknown fields");
        assert!(error.contains("future_field"));

        let missing_target = temp.path().join("missing-target-v4.toml");
        std::fs::write(
            &missing_target,
            r#"
name = "missing-target-v4"
version = 4
format = "flac"
sample_rate = 176400
bit_depth = "24"
dither = "tpdf"
replaygain = "off"
folder_template = "%ARTIST%/%ALBUM%"
filename_template = "%TRACKNN% - %TITLE%.%EXT%"
merge = "multi-file"
"#,
        )
        .expect("write v4 preset without output target");
        let error = load_preset_from_path(&missing_target)
            .expect_err("v4 must require an exact output_target");
        assert!(error.contains("output_target"));
    }

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

        // Presets that predate the companion fields inherit the product
        // default include list (companion copying on for conventional album
        // extras); folders and exclusions stay empty.
        assert_eq!(
            preset.companion_extensions,
            crate::convert::formats::default_companion_extensions().join(", ")
        );
        assert!(preset.companion_folders.is_empty());
        assert!(preset.companion_exclude_files.is_empty());
        assert!(!preset.force_encode);
        assert!(!preset.write_log);
    }

    #[test]
    fn companion_fields_round_trip_through_preset_capture_and_apply() {
        let format = FormatState::new();
        let mut output = OutputOptionsState::new();
        let metadata = MetadataState::default();
        output.companion_extensions = ".jpg, .pdf".to_string();
        output.companion_folders = "Scans, Artwork".to_string();
        output.companion_exclude_files = "EXIGO*, foo_dr.txt".to_string();
        output.force_encode.select_value(&true);
        output.write_log.select_value(&true);

        let preset = TuiPreset::from_pill_state("companions", &format, &output, &metadata);
        let mut restored_format = FormatState::new();
        let mut restored_output = OutputOptionsState::new();
        restored_output.companion_extensions.clear();
        restored_output.companion_folders.clear();
        restored_output.companion_exclude_files.clear();

        let mut restored_metadata = MetadataState::default();
        preset.apply_to_pills(
            &mut restored_format,
            &mut restored_output,
            &mut restored_metadata,
        );

        assert_eq!(restored_output.companion_extensions, ".jpg, .pdf");
        assert_eq!(restored_output.companion_folders, "Scans, Artwork");
        assert_eq!(restored_output.companion_exclude_files, "EXIGO*, foo_dr.txt");
        assert!(*restored_output.force_encode.selected_value());
        assert!(*restored_output.write_log.selected_value());
    }

    #[test]
    fn album_artist_override_round_trips_through_preset_capture_and_apply() {
        let format = FormatState::new();
        let output = OutputOptionsState::new();
        let mut metadata = MetadataState::default();
        metadata.album_artist_for_conversion = Some("  The Allman Brothers Band  ".to_string());

        let preset = TuiPreset::from_pill_state("album-artist", &format, &output, &metadata);
        assert_eq!(
            preset.album_artist_for_conversion.as_deref(),
            Some("The Allman Brothers Band")
        );

        let mut restored_format = FormatState::new();
        let mut restored_output = OutputOptionsState::new();
        let mut restored_metadata = MetadataState::default();
        restored_metadata.album_artist_for_conversion = Some("Stale Override".to_string());

        preset.apply_to_pills(
            &mut restored_format,
            &mut restored_output,
            &mut restored_metadata,
        );

        assert_eq!(
            restored_metadata.album_artist_for_conversion.as_deref(),
            Some("The Allman Brothers Band")
        );
    }

    #[test]
    fn legacy_preset_without_actions_deserializes_with_empty_pipeline() {
        let preset: TuiPreset = toml::from_str(
            r#"
name = "legacy-actions"
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
        .expect("preset without actions should deserialize");

        assert!(preset.actions.is_empty());
    }

    #[test]
    fn actions_round_trip_through_preset_capture_and_apply() {
        use crate::convert::pipeline::{
            ActionPipeline, ConversionAction, CreateFolderAction, RunScriptAction,
        };

        let format = FormatState::new();
        let mut output = OutputOptionsState::new();
        let metadata = MetadataState::default();
        output.actions = ActionPipeline {
            pre: vec![ConversionAction::CreateFolder(CreateFolderAction {
                path: PathBuf::from("prepared"),
                continue_on_error: false,
            })],
            post: vec![ConversionAction::Runscript(RunScriptAction {
                script: PathBuf::from("/usr/local/bin/catalog-album"),
                args: vec!["--quiet".to_string()],
                timeout_seconds: 45,
                continue_on_error: true,
            })],
        };

        let preset = TuiPreset::from_pill_state("actions", &format, &output, &metadata);
        let encoded = toml::to_string(&preset).expect("serialize action preset");
        let decoded: TuiPreset = toml::from_str(&encoded).expect("deserialize action preset");
        assert_eq!(decoded.actions, output.actions);

        let mut restored_format = FormatState::new();
        let mut restored_output = OutputOptionsState::new();
        let mut restored_metadata = MetadataState::default();
        decoded.apply_to_pills(
            &mut restored_format,
            &mut restored_output,
            &mut restored_metadata,
        );
        assert_eq!(restored_output.actions, output.actions);
    }

    #[test]
    fn apply_to_pills_reports_values_refused_by_format_constraints_and_parsing() {
        let format = FormatState::new();
        let output = OutputOptionsState::new();
        let metadata = MetadataState::default();
        let mut preset = TuiPreset::from_pill_state("refused", &format, &output, &metadata);
        preset.format = "alac".to_string();
        preset.bit_depth = "32".to_string();
        preset.merge = "not-a-merge-mode".to_string();

        let mut restored_format = FormatState::new();
        let mut restored_output = OutputOptionsState::new();
        let mut restored_metadata = MetadataState::default();
        let report = preset.apply_to_pills(
            &mut restored_format,
            &mut restored_output,
            &mut restored_metadata,
        );

        assert_eq!(
            report.refused_fields,
            vec![
                "output_target".to_string(),
                "bit_depth".to_string(),
                "merge".to_string(),
            ]
        );
        assert!(!report.is_complete());
        assert!(report
            .status_suffix()
            .contains("output_target, bit_depth, merge"));
        assert_eq!(*restored_format.format.selected_value(), AudioFormat::Alac);
        assert_ne!(
            *restored_format.bit_depth.selected_value(),
            BitDepthChoice::Int32,
            "ALAC must not silently retain the refused 32-bit selection"
        );
    }

    #[test]
    fn dsd_preset_refusal_is_independent_of_disabled_pcm_prestate() {
        let format = FormatState::new();
        let output = OutputOptionsState::new();
        let metadata = MetadataState::default();
        let mut preset = TuiPreset::from_pill_state("dsd-deterministic", &format, &output, &metadata);
        preset.format = "dsf".to_string();
        preset.sample_rate = tonepoet_pipeline::DsdRate::Dsd64.hz();
        // These PCM-only fields are serialized for backward compatibility but
        // are semantically irrelevant under a DSD target and must not be
        // accepted/refused based on whatever disabled value the UI retained.
        preset.bit_depth = "64f".to_string();
        preset.dither = "shibata".to_string();
        preset.replaygain = "album".to_string();
        preset.resampler = "ssrc".to_string();

        let mut first_format = FormatState::new();
        first_format.bit_depth.select_value(&BitDepthChoice::Int16);
        let mut first_output = OutputOptionsState::new();
        let mut first_metadata = MetadataState::default();
        let first = preset.apply_to_pills(
            &mut first_format,
            &mut first_output,
            &mut first_metadata,
        );

        let mut second_format = FormatState::new();
        second_format.bit_depth.select_value(&BitDepthChoice::Float64);
        let mut second_output = OutputOptionsState::new();
        let mut second_metadata = MetadataState::default();
        let second = preset.apply_to_pills(
            &mut second_format,
            &mut second_output,
            &mut second_metadata,
        );

        assert_eq!(first, second);
        assert_eq!(first.refused_fields, vec!["output_target".to_string()]);
        assert_eq!(*first_format.format.selected_value(), AudioFormat::Dsf);
        assert_eq!(*second_format.format.selected_value(), AudioFormat::Dsf);
    }

    #[test]
    fn pre_promotion_v4_preset_applies_legacy_manual_gain_without_native_refusals() {
        let mut source = FormatState::new();
        source.set_source_is_dsd(true);
        let output = OutputOptionsState::new();
        let metadata = MetadataState::default();
        let mut preset = TuiPreset::from_pill_state("legacy-manual", &source, &output, &metadata);
        preset.dsd_path = Some("reference".to_string());
        preset.dsd_profile = Some("reference".to_string());
        preset.dsd_gain = Some("manual".to_string());
        preset.dsd_gain_db = Some("2.250000000".to_string());

        let mut restored = FormatState::new();
        restored.set_source_is_dsd(true);
        let mut restored_output = OutputOptionsState::new();
        let mut restored_metadata = MetadataState::default();
        let report = preset.apply_to_pills(
            &mut restored,
            &mut restored_output,
            &mut restored_metadata,
        );

        assert!(report.is_complete(), "unexpected refusals: {:?}", report.refused_fields);
        assert_eq!(*restored.dsd_gain_mode.selected_value(), DsdGainMode::Fixed);
        assert_eq!(restored.dsd_gain_db, "2.250000000".parse().unwrap());
    }

    #[test]
    fn pre_promotion_v4_preset_maps_normalize_to_exact_legacy_auto() {
        let mut source = FormatState::new();
        source.set_source_is_dsd(true);
        let output = OutputOptionsState::new();
        let metadata = MetadataState::default();
        let mut preset = TuiPreset::from_pill_state("legacy-auto", &source, &output, &metadata);
        preset.dsd_gain = Some("normalize".to_string());
        preset.dsd_normalize_target_dbfs = Some("-0.500000000".to_string());

        let mut restored = FormatState::new();
        restored.set_source_is_dsd(true);
        let mut restored_output = OutputOptionsState::new();
        let mut restored_metadata = MetadataState::default();
        let report = preset.apply_to_pills(
            &mut restored,
            &mut restored_output,
            &mut restored_metadata,
        );

        assert!(report.is_complete(), "unexpected refusals: {:?}", report.refused_fields);
        assert_eq!(*restored.dsd_gain_mode.selected_value(), DsdGainMode::Auto);
        assert_eq!(restored.dsd_auto_gain_margin_db, "0.500000000".parse().unwrap());
    }

    #[test]
    fn pre_promotion_v4_auto_preset_round_trips_the_exact_legacy_margin() {
        let mut source = FormatState::new();
        source.set_source_is_dsd(true);
        assert!(source.dsd_gain_mode.select_value(&DsdGainMode::Auto));
        source.dsd_auto_gain_margin_db = "0.350000000".parse().unwrap();
        let output = OutputOptionsState::new();
        let metadata = MetadataState::default();
        let preset = TuiPreset::from_pill_state("legacy-auto-roundtrip", &source, &output, &metadata);
        assert_eq!(preset.dsd_gain.as_deref(), Some("auto"));
        assert_eq!(preset.dsd_normalize_target_dbfs.as_deref(), Some("0.350000000"));

        let mut restored = FormatState::new();
        restored.set_source_is_dsd(true);
        let mut restored_output = OutputOptionsState::new();
        let mut restored_metadata = MetadataState::default();
        let report = preset.apply_to_pills(
            &mut restored,
            &mut restored_output,
            &mut restored_metadata,
        );
        assert!(report.is_complete(), "unexpected refusals: {:?}", report.refused_fields);
        assert_eq!(*restored.dsd_gain_mode.selected_value(), DsdGainMode::Auto);
        assert_eq!(restored.dsd_auto_gain_margin_db, "0.350000000".parse().unwrap());
    }

    #[test]
    fn pre_promotion_v4_preset_refuses_native_only_dsd_requests() {
        let mut source = FormatState::new();
        source.set_source_is_dsd(true);
        let output = OutputOptionsState::new();
        let metadata = MetadataState::default();
        let mut preset = TuiPreset::from_pill_state("native-only", &source, &output, &metadata);
        preset.dsd_path = Some("manual".to_string());
        preset.dsd_profile = Some("wideband".to_string());
        preset.dsd_gain = Some("native".to_string());

        let mut restored = FormatState::new();
        restored.set_source_is_dsd(true);
        let mut restored_output = OutputOptionsState::new();
        let mut restored_metadata = MetadataState::default();
        let report = preset.apply_to_pills(
            &mut restored,
            &mut restored_output,
            &mut restored_metadata,
        );

        assert_eq!(
            report.refused_fields,
            vec![
                "dsd_path".to_string(),
                "dsd_profile".to_string(),
                "dsd_gain".to_string(),
            ]
        );
    }

    #[test]
    fn legacy_preset_without_album_artist_override_clears_stale_metadata_override() {
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
        .expect("preset without metadata override should deserialize");

        let mut format = FormatState::new();
        let mut output = OutputOptionsState::new();
        let mut metadata = MetadataState::default();
        metadata.album_artist_for_conversion = Some("Stale Override".to_string());

        preset.apply_to_pills(&mut format, &mut output, &mut metadata);

        assert!(metadata.album_artist_for_conversion.is_none());
    }
    #[test]
    fn dsd_source_rate_preset_round_trips_without_a_loaded_source() {
        let mut format = FormatState::new();
        format.format.select_value(&AudioFormat::Dsf);
        format.apply_format_constraints();
        format.sample_rate.select_value(&SOURCE_SAMPLE_RATE_SENTINEL);
        let output = OutputOptionsState::new();
        let metadata = MetadataState::default();
        let preset = TuiPreset::from_pill_state("dsd-source-rate", &format, &output, &metadata);

        let mut restored_format = FormatState::new();
        let mut restored_output = OutputOptionsState::new();
        let mut restored_metadata = MetadataState::default();
        let report = preset.apply_to_pills(
            &mut restored_format,
            &mut restored_output,
            &mut restored_metadata,
        );

        assert!(report.is_complete(), "unexpected refusals: {:?}", report.refused_fields);
        assert_eq!(*restored_format.format.selected_value(), AudioFormat::Dsf);
        assert_eq!(*restored_format.sample_rate.selected_value(), SOURCE_SAMPLE_RATE_SENTINEL);
    }

    #[test]
    fn source_coupled_and_replaygain_policy_values_round_trip() {
        let mut format = FormatState::new();
        format.sample_rate.select_value(&SOURCE_SAMPLE_RATE_SENTINEL);
        format.bit_depth.select_value(&BitDepthChoice::Source);
        format.dither.select_value(&DitherType::Shibata);
        format.resampler.select_value(&ResamplerChoice::Soxr);
        format.replaygain.select_value(&ReplayGainChoice::BothIfMissing);
        let output = OutputOptionsState::new();
        let metadata = MetadataState::default();
        let preset = TuiPreset::from_pill_state("source-coupled", &format, &output, &metadata);
        assert_eq!(preset.sample_rate, SOURCE_SAMPLE_RATE_SENTINEL);
        assert_eq!(preset.bit_depth, "source");
        assert_eq!(preset.replaygain, "both if missing");

        let mut restored_format = FormatState::new();
        let mut restored_output = OutputOptionsState::new();
        let mut restored_metadata = MetadataState::default();
        let report = preset.apply_to_pills(
            &mut restored_format,
            &mut restored_output,
            &mut restored_metadata,
        );
        assert!(report.is_complete(), "unexpected refusals: {:?}", report.refused_fields);
        assert_eq!(*restored_format.sample_rate.selected_value(), SOURCE_SAMPLE_RATE_SENTINEL);
        assert_eq!(*restored_format.bit_depth.selected_value(), BitDepthChoice::Source);
        assert_eq!(*restored_format.dither.selected_value(), DitherType::Shibata);
        assert_eq!(*restored_format.resampler.selected_value(), ResamplerChoice::Soxr);
        assert_eq!(*restored_format.replaygain.selected_value(), ReplayGainChoice::BothIfMissing);
        assert!(restored_format.sample_rate_overridden);
        assert!(restored_format.bit_depth_overridden);
        assert!(restored_format.dither_overridden);
        assert!(restored_format.resampler_overridden);
        assert_eq!(restored_format.source_derived_sample_rate, None);
        assert_eq!(restored_format.source_derived_bit_depth, None);
    }

}
