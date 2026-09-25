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
#[serde(deny_unknown_fields)]
pub struct TuiPreset {
    pub name: String,
    pub description: Option<String>,
    pub version: u32, // 2 = legacy TUI, 3 = dynamic pane, 5 = strict Phase-2 gain schema

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
    /// Canonical resolved codec/container identity. Required by v5.
    #[serde(default)]
    pub output_target: Option<String>,
    /// Strict DSD-source pathway: Custom processing or qualified Reference delivery.
    #[serde(default)]
    pub dsd_path: Option<String>,
    /// Custom-path reconstruction authority: native or the protected Reference recipe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsd_reconstruction: Option<String>,
    /// Custom-path low-pass implementation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsd_lowpass: Option<String>,
    /// Qualified Reference reconstruction profile (also used by protected Custom reconstruction).
    #[serde(default)]
    pub dsd_profile: Option<String>,
    /// Custom protected-reconstruction export level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsd_export_level: Option<String>,
    /// Strict Phase-2 DSD gain policy token.
    #[serde(default)]
    pub dsd_gain: Option<String>,
    /// Canonical fixed gain text.
    #[serde(default)]
    pub dsd_gain_db: Option<String>,
    /// Certified DSD true-peak target in dBTP for Custom or Reference.
    #[serde(default)]
    pub dsd_true_peak_target_dbtp: Option<String>,
    /// DSD certified true-peak scope for Custom or Reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsd_true_peak_scope: Option<String>,
    /// Certified true-peak scan tier for both Track and Album scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsd_true_peak_scan: Option<String>,
    /// Ordinary PCM gain mode using the strict Phase-2 tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pcm_gain: Option<String>,
    /// User-supplied PCM fixed gain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pcm_fixed_gain_db: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pcm_true_peak_target_dbtp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pcm_true_peak_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pcm_true_peak_scan: Option<String>,

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
            dsd_reconstruction: None,
            dsd_lowpass: None,
            dsd_profile: None,
            dsd_export_level: None,
            dsd_gain: None,
            dsd_gain_db: None,
            dsd_true_peak_target_dbtp: None,
            dsd_true_peak_scope: None,
            dsd_true_peak_scan: None,
            pcm_gain: None,
            pcm_fixed_gain_db: None,
            pcm_true_peak_target_dbtp: None,
            pcm_true_peak_scope: None,
            pcm_true_peak_scan: None,
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

fn validate_v5_preset(preset: &TuiPreset, path: &Path) -> Result<(), String> {
    if preset.version != 5 {
        return Err(format!("Preset version changed while parsing '{}'", path.display()));
    }
    if preset.output_target.as_deref().is_none_or(|value| value.trim().is_empty()) {
        return Err(format!("Preset v5 '{}' has an empty required output_target", path.display()));
    }
    if let Some(mode) = preset.pcm_gain.as_deref() {
        if !matches!(mode, "off" | "true-peak-guard" | "true-peak-normalize" | "fixed-gain") {
            return Err(format!(
                "Preset v5 '{}' uses unsupported pcm_gain '{mode}'; obsolete auto/boost forms are not migrated",
                path.display()
            ));
        }
    }
    if let Some(pathway) = preset.dsd_path.as_deref() {
        if !matches!(pathway, "custom" | "reference") {
            return Err(format!(
                "Preset v5 '{}' uses unsupported dsd_path '{pathway}'; the retired manual pathway is not migrated",
                path.display()
            ));
        }
    }
    if let Some(value) = preset.dsd_reconstruction.as_deref() {
        if !matches!(value, "native" | "reference") {
            return Err(format!(
                "Preset v5 '{}' uses unsupported dsd_reconstruction '{value}'",
                path.display()
            ));
        }
    }
    if let Some(value) = preset.dsd_lowpass.as_deref() {
        if !matches!(value, "auto" | "sox-ultra" | "sinc") {
            return Err(format!(
                "Preset v5 '{}' uses unsupported dsd_lowpass '{value}'",
                path.display()
            ));
        }
    }
    if let Some(value) = preset.dsd_export_level.as_deref() {
        if !matches!(value, "native" | "compensated" | "protected") {
            return Err(format!(
                "Preset v5 '{}' uses unsupported dsd_export_level '{value}'",
                path.display()
            ));
        }
    }
    if let Some(mode) = preset.dsd_gain.as_deref() {
        if !matches!(
            mode,
            "off"
                | "true-peak-guard"
                | "true-peak-normalize"
                | "fixed-gain"
                | "auto"
        ) {
            return Err(format!(
                "Preset v5 '{}' uses unsupported dsd_gain '{mode}'; obsolete auto/normalize/manual forms are not migrated",
                path.display()
            ));
        }
    }
    for (name, scan) in [
        ("pcm_true_peak_scan", preset.pcm_true_peak_scan.as_deref()),
        ("dsd_true_peak_scan", preset.dsd_true_peak_scan.as_deref()),
    ] {
        if let Some(scan) = scan {
            if !matches!(scan, "fast066v2_reference" | "fast066v2_standard" | "fast066v2_fast") {
                return Err(format!(
                    "Preset v5 '{}' uses unsupported {name} value '{scan}'",
                    path.display()
                ));
            }
        }
    }
    Ok(())
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
    /// Render the effective rate/depth/dither policy captured by this preset.
    /// This deliberately describes sentinels as policy rather than resolving
    /// them against whichever source happens to be loaded now.
    pub fn resolved_semantics_summary(&self) -> String {
        let rate = if self.sample_rate == SOURCE_SAMPLE_RATE_SENTINEL {
            "keep source".to_string()
        } else if self.sample_rate % 1_000 == 0 {
            format!("{} kHz", self.sample_rate / 1_000)
        } else {
            format!("{:.1} kHz", self.sample_rate as f64 / 1_000.0)
        };
        let depth = match self.bit_depth.trim().to_ascii_lowercase().as_str() {
            "source" => "keep source".to_string(),
            "16" => "16-bit".to_string(),
            "24" => "24-bit".to_string(),
            "32" => "32-bit".to_string(),
            "32f" => "32-bit float".to_string(),
            "64f" => "64-bit float".to_string(),
            _ => self.bit_depth.clone(),
        };
        let dither = if self.dither.eq_ignore_ascii_case("none") {
            "none".to_string()
        } else {
            self.dither.clone()
        };
        format!("rate: {rate} | depth: {depth} | dither: {dither}")
    }

    /// Capture current pill state into a preset
    pub fn from_pill_state(
        name: &str,
        format: &FormatState,
        output_opts: &OutputOptionsState,
        metadata: &MetadataState,
    ) -> Self {
        Self {
            name: name.to_string(),
            description: None,
            version: 5,
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
            dsd_path: match *format.dsd_pathway.selected_value() {
                tonepoet_pipeline::DsdSourcePathway::Custom => Some("custom".to_string()),
                tonepoet_pipeline::DsdSourcePathway::Reference => Some("reference".to_string()),
                tonepoet_pipeline::DsdSourcePathway::Manual => None,
            },
            dsd_reconstruction: (*format.dsd_pathway.selected_value()
                == tonepoet_pipeline::DsdSourcePathway::Custom)
                .then(|| match *format.dsd_custom_reconstruction.selected_value() {
                    tonepoet_pipeline::DsdGeneralReconstruction::General => "native".to_string(),
                    tonepoet_pipeline::DsdGeneralReconstruction::ReferenceProtected => "reference".to_string(),
                }),
            dsd_lowpass: (*format.dsd_pathway.selected_value()
                == tonepoet_pipeline::DsdSourcePathway::Custom)
                .then(|| match *format.dsd_custom_lowpass.selected_value() {
                    tonepoet_pipeline::enums::DsdLowpassMethod::Auto => "auto".to_string(),
                    tonepoet_pipeline::enums::DsdLowpassMethod::SoxUltra => "sox-ultra".to_string(),
                    tonepoet_pipeline::enums::DsdLowpassMethod::Sinc => "sinc".to_string(),
                }),
            dsd_profile: ((*format.dsd_pathway.selected_value() == tonepoet_pipeline::DsdSourcePathway::Reference)
                || (*format.dsd_pathway.selected_value() == tonepoet_pipeline::DsdSourcePathway::Custom
                    && *format.dsd_custom_reconstruction.selected_value()
                        == tonepoet_pipeline::DsdGeneralReconstruction::ReferenceProtected))
                .then(|| match *format.dsd_profile.selected_value() {
                    tonepoet_pipeline::DsdReconstructionSelection::Reference => "reference".to_string(),
                    tonepoet_pipeline::DsdReconstructionSelection::Wideband => "wideband".to_string(),
                }),
            dsd_export_level: (*format.dsd_pathway.selected_value()
                == tonepoet_pipeline::DsdSourcePathway::Custom
                && *format.dsd_custom_reconstruction.selected_value()
                    == tonepoet_pipeline::DsdGeneralReconstruction::ReferenceProtected)
                .then(|| match *format.dsd_custom_export_level.selected_value() {
                    tonepoet_pipeline::DsdGeneralExportLevel::Native => "native".to_string(),
                    tonepoet_pipeline::DsdGeneralExportLevel::NominalCompensated => "compensated".to_string(),
                    tonepoet_pipeline::DsdGeneralExportLevel::ProtectedR64 => "protected".to_string(),
                    tonepoet_pipeline::DsdGeneralExportLevel::NativeWithOffset { .. } => "native".to_string(),
                }),
            dsd_gain: Some(format.dsd_gain_mode.selected_value().preset_key().to_string()),
            dsd_gain_db: matches!(
                *format.dsd_gain_mode.selected_value(),
                DsdGainMode::FixedGain
            )
            .then(|| format.dsd_gain_db.render(false)),
            dsd_true_peak_target_dbtp: (matches!(
                    (*format.dsd_pathway.selected_value(), *format.dsd_gain_mode.selected_value()),
                    (tonepoet_pipeline::DsdSourcePathway::Custom, DsdGainMode::TruePeakGuard | DsdGainMode::TruePeakNormalize)
                        | (tonepoet_pipeline::DsdSourcePathway::Reference, DsdGainMode::ReferenceAuto)
                ))
                .then(|| format.dsd_true_peak_target_dbtp.render(false)),
            dsd_true_peak_scope: (matches!(
                    (*format.dsd_pathway.selected_value(), *format.dsd_gain_mode.selected_value()),
                    (tonepoet_pipeline::DsdSourcePathway::Custom, DsdGainMode::TruePeakGuard | DsdGainMode::TruePeakNormalize)
                        | (tonepoet_pipeline::DsdSourcePathway::Reference, DsdGainMode::ReferenceAuto)
                ))
                .then(|| match format.dsd_true_peak_scope.selected_value() {
                    tonepoet_pipeline::TruePeakScope::Track => "track",
                    tonepoet_pipeline::TruePeakScope::Album => "album",
                }.to_string()),
            dsd_true_peak_scan: (matches!(
                    (*format.dsd_pathway.selected_value(), *format.dsd_gain_mode.selected_value()),
                    (tonepoet_pipeline::DsdSourcePathway::Custom, DsdGainMode::TruePeakGuard | DsdGainMode::TruePeakNormalize)
                        | (tonepoet_pipeline::DsdSourcePathway::Reference, DsdGainMode::ReferenceAuto)
                ))
                .then(|| match format.dsd_true_peak_scan_mode.selected_value() {
                    tonepoet_pipeline::TruePeakScanTier::Reference => "fast066v2_reference",
                    tonepoet_pipeline::TruePeakScanTier::Standard => "fast066v2_standard",
                    tonepoet_pipeline::TruePeakScanTier::Fast => "fast066v2_fast",
                }.to_string()),
            pcm_gain: Some(format.pcm_gain_mode.selected_value().preset_key().to_string()),
            pcm_fixed_gain_db: (*format.pcm_gain_mode.selected_value()
                == crate::tui::app::PcmGainMode::FixedGain)
                .then(|| format.pcm_fixed_gain_db.render(false)),
            pcm_true_peak_target_dbtp: Some(format.pcm_true_peak_target_dbtp.render(false)),
            pcm_true_peak_scope: Some(match format.pcm_true_peak_scope.selected_value() {
                tonepoet_pipeline::TruePeakScope::Track => "track",
                tonepoet_pipeline::TruePeakScope::Album => "album",
            }.to_string()),
            pcm_true_peak_scan: Some(match format.pcm_true_peak_scan_mode.selected_value() {
                tonepoet_pipeline::TruePeakScanTier::Standard => "fast066v2_standard",
                tonepoet_pipeline::TruePeakScanTier::Fast => "fast066v2_fast",
                tonepoet_pipeline::TruePeakScanTier::Reference => "fast066v2_reference",
            }.to_string()),
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

        if self.version >= 5 {
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
            // conversion. The strict v5 schema stores one explicit mode and
            // never interprets retired auto/normalize/manual tokens.
            if format_state.dsd_to_pcm_gain_available() {
                let gain_fields_present = self.dsd_gain.is_some()
                    || self.dsd_gain_db.is_some()
                    || self.dsd_true_peak_target_dbtp.is_some()
                    || self.dsd_true_peak_scope.is_some()
                    || self.dsd_true_peak_scan.is_some();
                if gain_fields_present {
                    format_state.dsd_gain_overridden = true;
                }

                if let Some(raw_path) = self.dsd_path.as_deref() {
                    let pathway = match raw_path {
                        "custom" => Some(tonepoet_pipeline::DsdSourcePathway::Custom),
                        "reference" => Some(tonepoet_pipeline::DsdSourcePathway::Reference),
                        _ => None,
                    };
                    let applied = pathway
                        .is_some_and(|value| format_state.dsd_pathway.select_value(&value));
                    report.record("dsd_path", applied);
                    if applied {
                        // Mode availability is pathway-dependent. Recompute it only
                        // after the requested pathway has been selected. A Reference
                        // preset starts from the canonical Reference defaults every
                        // time so applying it is independent of prior UI state; any
                        // explicit preset fields below then override those defaults.
                        format_state.apply_format_constraints();
                        if pathway == Some(tonepoet_pipeline::DsdSourcePathway::Reference) {
                            format_state.apply_reference_gain_defaults();
                        }
                    }
                }

                if *format_state.dsd_pathway.selected_value()
                    == tonepoet_pipeline::DsdSourcePathway::Custom
                {
                    if let Some(raw) = self.dsd_reconstruction.as_deref() {
                        let value = match raw {
                            "native" => Some(tonepoet_pipeline::DsdGeneralReconstruction::General),
                            "reference" => Some(tonepoet_pipeline::DsdGeneralReconstruction::ReferenceProtected),
                            _ => None,
                        };
                        report.record(
                            "dsd_reconstruction",
                            value.is_some_and(|value| format_state.dsd_custom_reconstruction.select_value(&value)),
                        );
                        format_state.apply_format_constraints();
                    }
                    if let Some(raw) = self.dsd_lowpass.as_deref() {
                        let value = match raw {
                            "auto" => Some(tonepoet_pipeline::enums::DsdLowpassMethod::Auto),
                            "sox-ultra" => Some(tonepoet_pipeline::enums::DsdLowpassMethod::SoxUltra),
                            "sinc" => Some(tonepoet_pipeline::enums::DsdLowpassMethod::Sinc),
                            _ => None,
                        };
                        report.record(
                            "dsd_lowpass",
                            value.is_some_and(|value| format_state.dsd_custom_lowpass.select_value(&value)),
                        );
                    }
                    if let Some(raw) = self.dsd_export_level.as_deref() {
                        let value = match raw {
                            "native" => Some(tonepoet_pipeline::DsdGeneralExportLevel::Native),
                            "compensated" => Some(tonepoet_pipeline::DsdGeneralExportLevel::NominalCompensated),
                            "protected" => Some(tonepoet_pipeline::DsdGeneralExportLevel::ProtectedR64),
                            _ => None,
                        };
                        report.record(
                            "dsd_export_level",
                            value.is_some_and(|value| format_state.dsd_custom_export_level.select_value(&value)),
                        );
                    }
                }

                if *format_state.dsd_pathway.selected_value()
                    == tonepoet_pipeline::DsdSourcePathway::Reference
                    || (*format_state.dsd_pathway.selected_value()
                        == tonepoet_pipeline::DsdSourcePathway::Custom
                        && *format_state.dsd_custom_reconstruction.selected_value()
                            == tonepoet_pipeline::DsdGeneralReconstruction::ReferenceProtected)
                {
                    if let Some(raw_profile) = self.dsd_profile.as_deref() {
                        let profile = match raw_profile {
                            "reference" => Some(tonepoet_pipeline::DsdReconstructionSelection::Reference),
                            "wideband" => Some(tonepoet_pipeline::DsdReconstructionSelection::Wideband),
                            _ => None,
                        };
                        report.record(
                            "dsd_profile",
                            profile.is_some_and(|value| format_state.dsd_profile.select_value(&value)),
                        );
                    }
                }

                if let Some(raw_gain) = self.dsd_gain.as_deref() {
                    let mode = match raw_gain {
                        "off" => Some(DsdGainMode::Off),
                        "true-peak-guard" => Some(DsdGainMode::TruePeakGuard),
                        "true-peak-normalize" => Some(DsdGainMode::TruePeakNormalize),
                        "fixed-gain" => Some(DsdGainMode::FixedGain),
                        "auto" => Some(DsdGainMode::ReferenceAuto),
                        _ => None,
                    };
                    report.record(
                        "dsd_gain",
                        mode.is_some_and(|value| format_state.dsd_gain_mode.select_value(&value)),
                    );
                }

                if let Some(raw) = self.dsd_gain_db.as_deref().filter(|_| {
                        *format_state.dsd_pathway.selected_value()
                    == tonepoet_pipeline::DsdSourcePathway::Custom
                    && *format_state.dsd_gain_mode.selected_value() == DsdGainMode::FixedGain
                    }) {
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
                let certified_true_peak_selected = matches!(
                    (*format_state.dsd_pathway.selected_value(), *format_state.dsd_gain_mode.selected_value()),
                    (tonepoet_pipeline::DsdSourcePathway::Custom, DsdGainMode::TruePeakGuard | DsdGainMode::TruePeakNormalize)
                        | (tonepoet_pipeline::DsdSourcePathway::Reference, DsdGainMode::ReferenceAuto)
                );
                if let Some(raw) = self.dsd_true_peak_target_dbtp.as_deref().filter(|_| {
                        certified_true_peak_selected
                    }) {
                    match raw.parse::<tonepoet_pipeline::DbNano>() {
                        Ok(value)
                            if (tonepoet_pipeline::DbNano::MIN_NORMALIZE_TARGET
                                ..=tonepoet_pipeline::DbNano::MAX_NORMALIZE_TARGET)
                                .contains(&value) =>
                        {
                            format_state.dsd_true_peak_target_dbtp = value;
                            report.record("dsd_true_peak_target_dbtp", true);
                        }
                        _ => report.record("dsd_true_peak_target_dbtp", false),
                    }
                }
                if let Some(raw_scope) = self.dsd_true_peak_scope.as_deref().filter(|_| {
                        certified_true_peak_selected
                    }) {
                    let scope = match raw_scope {
                        "track" => Some(tonepoet_pipeline::TruePeakScope::Track),
                        "album" => Some(tonepoet_pipeline::TruePeakScope::Album),
                        _ => None,
                    };
                    report.record(
                        "dsd_true_peak_scope",
                        scope.is_some_and(|value| format_state.dsd_true_peak_scope.select_value(&value)),
                    );
                }
                if let Some(raw_scan) = self.dsd_true_peak_scan.as_deref().filter(|_| {
                        certified_true_peak_selected
                    }) {
                    let scan = match raw_scan {
                        "fast066v2_reference" => Some(tonepoet_pipeline::TruePeakScanTier::Reference),
                        "fast066v2_standard" => Some(tonepoet_pipeline::TruePeakScanTier::Standard),
                        "fast066v2_fast" => Some(tonepoet_pipeline::TruePeakScanTier::Fast),
                        _ => None,
                    };
                    report.record(
                        "dsd_true_peak_scan",
                        scan.is_some_and(|value| format_state.dsd_true_peak_scan_mode.select_value(&value)),
                    );
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

        let reference_dsd_to_pcm = format_state.dsd_reference_path_selected();

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
            if !reference_dsd_to_pcm {
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
        }

        if !is_dsd {
            match parse_replaygain(&self.replaygain) {
                Some(value) => report.record(
                    "replaygain",
                    Self::select_enabled(&mut format_state.replaygain, &value),
                ),
                None => report.record("replaygain", false),
            }
            let gain_fields_present = self.pcm_gain.is_some()
                || self.pcm_fixed_gain_db.is_some()
                || self.pcm_true_peak_target_dbtp.is_some()
                || self.pcm_true_peak_scope.is_some()
                || self.pcm_true_peak_scan.is_some();
            if gain_fields_present {
                format_state.pcm_gain_overridden = true;
            }
            if let Some(raw_mode) = self.pcm_gain.as_deref() {
                let mode = match raw_mode {
                    "off" => Some(PcmGainMode::Off),
                    "true-peak-guard" => Some(PcmGainMode::TruePeakGuard),
                    "true-peak-normalize" => Some(PcmGainMode::TruePeakNormalize),
                    "fixed-gain" => Some(PcmGainMode::FixedGain),
                    _ => None,
                };
                report.record(
                    "pcm_gain",
                    mode.is_some_and(|value| format_state.pcm_gain_mode.select_value(&value)),
                );
            }
            if let Some(raw) = self.pcm_fixed_gain_db.as_deref() {
                match raw.parse::<tonepoet_pipeline::DbNano>() {
                    Ok(value)
                        if (tonepoet_pipeline::DbNano::MIN_FIXED_GAIN
                            ..=tonepoet_pipeline::DbNano::MAX_FIXED_GAIN)
                            .contains(&value) =>
                    {
                        format_state.pcm_fixed_gain_db = value;
                        report.record("pcm_fixed_gain_db", true);
                    }
                    _ => report.record("pcm_fixed_gain_db", false),
                }
            }
            if let Some(raw) = self.pcm_true_peak_target_dbtp.as_deref() {
                match raw.parse::<tonepoet_pipeline::DbNano>() {
                    Ok(value)
                        if (tonepoet_pipeline::DbNano::MIN_NORMALIZE_TARGET
                            ..=tonepoet_pipeline::DbNano::MAX_NORMALIZE_TARGET)
                            .contains(&value) =>
                    {
                        format_state.pcm_true_peak_target_dbtp = value;
                        report.record("pcm_true_peak_target_dbtp", true);
                    }
                    _ => report.record("pcm_true_peak_target_dbtp", false),
                }
            }
            if let Some(raw_scope) = self.pcm_true_peak_scope.as_deref() {
                let scope = match raw_scope {
                    "track" => Some(tonepoet_pipeline::TruePeakScope::Track),
                    "album" => Some(tonepoet_pipeline::TruePeakScope::Album),
                    _ => None,
                };
                report.record(
                    "pcm_true_peak_scope",
                    scope.is_some_and(|value| format_state.pcm_true_peak_scope.select_value(&value)),
                );
            }
            if let Some(raw_scan) = self.pcm_true_peak_scan.as_deref() {
                let scan = match raw_scan {
                    "fast066v2_reference" => Some(tonepoet_pipeline::TruePeakScanTier::Reference),
                    "fast066v2_standard" => Some(tonepoet_pipeline::TruePeakScanTier::Standard),
                    "fast066v2_fast" => Some(tonepoet_pipeline::TruePeakScanTier::Fast),
                    _ => None,
                };
                report.record(
                    "pcm_true_peak_scan",
                    scan.is_some_and(|value| format_state.pcm_true_peak_scan_mode.select_value(&value)),
                );
            }

            if !reference_dsd_to_pcm {
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
        let expected_dither = (pcm_depth_is_meaningful && !reference_dsd_to_pcm)
            .then(|| parse_dither(&self.dither))
            .flatten();
        let expected_replaygain = (!is_dsd)
            .then(|| parse_replaygain(&self.replaygain))
            .flatten();
        let expected_resampler = (!is_dsd && !reference_dsd_to_pcm)
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
        format_state.apply_auto_gain_defaults();
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

    /// Project this on-disk TUI preset into the same `ConversionOptions`
    /// carrier used by the live TUI.
    ///
    /// A CLI preset is loaded before its inputs are probed, while the TUI
    /// normally applies a preset after source identity is known.  Preserve
    /// both source-relative policy branches here: ordinary PCM true-peak
    /// policy comes from a PCM-source projection and DSD-to-PCM policy comes
    /// from a DSD-source projection.  The production planner then selects the
    /// relevant branch from the actual admitted source instead of whichever
    /// source class happened to be assumed while loading the preset.
    pub fn to_conversion_options(
        &self,
        config: &crate::config::TonepoetConfig,
    ) -> Result<crate::convert::ConversionOptions, String> {
        fn project(
            preset: &TuiPreset,
            config: &crate::config::TonepoetConfig,
            source_is_dsd: bool,
        ) -> Result<(
            crate::convert::ConversionOptions,
            OutputOptionsState,
            MetadataState,
        ), String> {
            let mut format = FormatState::new();
            format.set_source_is_dsd(source_is_dsd);
            let mut output = OutputOptionsState::new();
            let mut metadata = MetadataState::default();
            let report = preset.apply_to_pills(&mut format, &mut output, &mut metadata);
            if !report.is_complete() {
                return Err(format!(
                    "preset '{}' cannot be applied{}",
                    preset.name,
                    report.status_suffix(),
                ));
            }
            let mut options = super::convert_actions::try_pills_to_options(
                &format,
                &output,
                config,
            )?;
            output.apply_companion_copying_to_conversion_options(&mut options);
            options.album_artist_override = metadata
                .album_artist_for_conversion
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            Ok((options, output, metadata))
        }

        let (mut options, _, _) = project(self, config, false)?;
        let (dsd_options, _, _) = project(self, config, true)?;
        match (
            options.pipeline_settings.as_mut(),
            dsd_options.pipeline_settings.as_ref(),
        ) {
            (Some(settings), Some(dsd_settings)) => {
                // Keep PCM-source policy from the first projection, but retain
                // the dormant DSD-source policy that is visible only after a
                // DSD source has been probed in the TUI.
                settings.dsd.from_dsd = dsd_settings.dsd.from_dsd.clone();
                settings.dsd.general_from_dsd = dsd_settings.dsd.general_from_dsd.clone();
            }
            _ => return Err(format!("preset '{}' did not produce pipeline settings", self.name)),
        }
        Ok(options)
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
                return Err(format!(
                    "Preset v4 '{}' uses the retired ambiguous gain schema; recreate or re-save it with Off, True-peak guard, True-peak normalize, or Fixed gain",
                    path.display()
                ));
            }
            5 => {
                let preset = toml::from_str::<TuiPreset>(&contents).map_err(|error| {
                    format!("Invalid preset v5 '{}': {error}", path.display())
                })?;
                validate_v5_preset(&preset, path)?;
                preset
            }
            _ => {
                return Err(format!(
                    "Unsupported preset version {version} in '{}'; supported versions are 2, 3, and 5",
                    path.display()
                ));
            }
        };
        preset.name = display_name.to_string();
        return Ok(preset);
    }

    Err(format!(
        "Preset '{}' has no TUI preset version; versionless wizard presets are not supported",
        path.display(),
    ))
}

/// Save a preset to disk in the configured presets directory.
pub fn save_preset(preset: &TuiPreset) -> Result<(), String> {
    let path = preset_file_path(&preset.name);
    save_preset_to_path(preset, &path)
}

/// Save a preset to an explicit path returned by a file picker.
pub fn save_preset_to_path(preset: &TuiPreset, path: &Path) -> Result<(), String> {
    let (_lock, resolved) = crate::config::StoreFileLock::acquire_for_path(path)
        .map_err(|e| format!("Failed to lock preset '{}': {e}", path.display()))?;
    save_preset_to_path_locked(preset, &resolved)
}

fn save_preset_to_path_locked(preset: &TuiPreset, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create preset directory '{}': {}", parent.display(), e))?;
    }
    if preset.version != 5 {
        return Err(format!(
            "Refusing to save preset version {}; current saves require version 5",
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
    let path = preset_file_path(name);
    let (_lock, resolved) = crate::config::StoreFileLock::acquire_for_path(&path)
        .map_err(|e| format!("Failed to lock preset '{}': {e}", name))?;
    fs::remove_file(&resolved).map_err(|e| format!("Failed to delete preset '{}': {}", name, e))?;
    if let Some(parent) = resolved.parent() {
        fs::File::open(parent).and_then(|directory| directory.sync_all())
            .map_err(|e| format!("Failed to sync preset directory '{}': {e}", parent.display()))?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresetCommitOutcome {
    Committed,
    CommittedWithIndexWarning(String),
}

impl PresetCommitOutcome {
    pub fn index_warning(&self) -> Option<&str> {
        match self {
            Self::Committed => None,
            Self::CommittedWithIndexWarning(error) => Some(error.as_str()),
        }
    }
}

/// Save a preset to both TOML and SQLite. The TOML file is the durable
/// source of truth; an index failure after publication is reported as a
/// committed result with a warning so callers never pretend the TOML rolled
/// back. Startup repair converges the SQLite index later.
pub fn save_preset_with_db(
    preset: &TuiPreset,
    db: &crate::db::Database,
) -> Result<PresetCommitOutcome, String> {
    let path = preset_file_path(&preset.name);
    let (_lock, resolved) = crate::config::StoreFileLock::acquire_for_path(&path)
        .map_err(|e| format!("Failed to lock preset '{}': {e}", preset.name))?;
    save_preset_to_path_locked(preset, &resolved)?;
    match store_preset_in_db(preset, db) {
        Ok(()) => Ok(PresetCommitOutcome::Committed),
        Err(error) => {
            log::warn!(
                "preset '{}' committed to TOML but SQLite index update failed: {}",
                preset.name,
                error
            );
            Ok(PresetCommitOutcome::CommittedWithIndexWarning(error))
        }
    }
}

/// Save a preset to an explicit path and index it in SQLite under the file stem.
pub fn save_preset_to_path_with_db(
    preset: &TuiPreset,
    path: &Path,
    db: &crate::db::Database,
) -> Result<PresetCommitOutcome, String> {
    let (_lock, resolved) = crate::config::StoreFileLock::acquire_for_path(path)
        .map_err(|e| format!("Failed to lock preset '{}': {e}", path.display()))?;
    save_preset_to_path_locked(preset, &resolved)?;
    match store_preset_in_db(preset, db) {
        Ok(()) => Ok(PresetCommitOutcome::Committed),
        Err(error) => {
            log::warn!(
                "preset '{}' committed to TOML at {} but SQLite index update failed: {}",
                preset.name, resolved.display(), error
            );
            Ok(PresetCommitOutcome::CommittedWithIndexWarning(error))
        }
    }
}

fn store_preset_in_db(preset: &TuiPreset, db: &crate::db::Database) -> Result<(), String> {
    db.store_preset(
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
    )
}

/// Delete a preset from both TOML and SQLite.
pub fn delete_preset_with_db(
    name: &str,
    db: &crate::db::Database,
) -> Result<PresetCommitOutcome, String> {
    let path = preset_file_path(name);
    let (_lock, resolved) = crate::config::StoreFileLock::acquire_for_path(&path)
        .map_err(|e| format!("Failed to lock preset '{}': {e}", name))?;
    match fs::remove_file(&resolved) {
        Ok(()) => {
            if let Some(parent) = resolved.parent() {
                fs::File::open(parent).and_then(|directory| directory.sync_all())
                    .map_err(|e| format!("Failed to sync preset directory '{}': {e}", parent.display()))?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("Failed to delete preset '{}': {e}", name)),
    }
    match db.delete_preset(name) {
        Ok(()) => Ok(PresetCommitOutcome::Committed),
        Err(error) => {
            log::warn!(
                "preset '{}' removed from TOML but SQLite index update failed: {}",
                name,
                error
            );
            Ok(PresetCommitOutcome::CommittedWithIndexWarning(error))
        }
    }
}

/// Sync TOML presets into the SQLite database. Imports any TOML
/// presets not already in the DB (handles first-run and externally-
/// added presets like manual file copies or syncs from another machine).
pub fn import_presets_to_db(db: &crate::db::Database) {
    // TOML is the source of truth. Refresh every present row (not only missing
    // rows) and remove DB-only rows. Each same-name file/index pair is protected
    // by the same per-preset store lock used by interactive save/delete.
    let file_names = list_presets();
    let file_set = file_names.iter().cloned().collect::<std::collections::BTreeSet<_>>();
    for name in &file_names {
        let path = preset_file_path(name);
        let Ok((_lock, resolved)) = crate::config::StoreFileLock::acquire_for_path(&path) else { continue };
        if let Ok(preset) = load_preset_from_path(&resolved) {
            let _ = store_preset_in_db(&preset, db);
        }
    }
    for name in db.list_preset_names() {
        if file_set.contains(&name) { continue; }
        let path = preset_file_path(&name);
        if let Ok((_lock, _)) = crate::config::StoreFileLock::acquire_for_path(&path) {
            let _ = db.delete_preset(&name);
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
        AudioFormat::Musepack => "mpc",
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
        "mpc" => Some(AudioFormat::Musepack),
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

    fn current_test_preset(name: &str) -> TuiPreset {
        TuiPreset::from_pill_state(
            name,
            &FormatState::new(),
            &OutputOptionsState::new(),
            &MetadataState::default(),
        )
    }

    #[test]
    fn toml_save_commit_survives_sqlite_index_failure_as_warning() {
        let _home = crate::tui::test_support::XdgConfigHomeGuard::new(
            "tonepoet-preset-index-warning-save",
        );
        let db = crate::db::Database::open_memory().expect("db");
        db.install_preset_index_failure_for_tests().expect("failure trigger");
        let preset = current_test_preset("index-warning-save");

        let outcome = save_preset_with_db(&preset, &db).expect("TOML commit remains success");
        assert!(matches!(outcome, PresetCommitOutcome::CommittedWithIndexWarning(_)));
        assert!(preset_file_path(&preset.name).exists());
        assert_eq!(load_preset(&preset.name).expect("load committed TOML").name, preset.name);
    }

    #[test]
    fn toml_delete_commit_survives_sqlite_index_failure_as_warning() {
        let _home = crate::tui::test_support::XdgConfigHomeGuard::new(
            "tonepoet-preset-index-warning-delete",
        );
        let db = crate::db::Database::open_memory().expect("db");
        let preset = current_test_preset("index-warning-delete");
        save_preset_with_db(&preset, &db).expect("seed preset");
        assert!(preset_file_path(&preset.name).exists());
        db.install_preset_index_failure_for_tests().expect("failure trigger");

        let outcome = delete_preset_with_db(&preset.name, &db).expect("TOML delete remains success");
        assert!(matches!(outcome, PresetCommitOutcome::CommittedWithIndexWarning(_)));
        assert!(!preset_file_path(&preset.name).exists());
    }

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
    fn v5_preset_confirms_exact_reference_target() {
        let format = FormatState::new();
        let output = OutputOptionsState::new();
        let metadata = MetadataState::default();
        let preset = TuiPreset::from_pill_state("native-reference", &format, &output, &metadata);
        assert_eq!(preset.version, 5);
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
    fn versioned_preset_loader_rejects_cross_version_retired_and_unknown_fields() {
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
        let error = load_preset_from_path(&v3).expect_err("v3 must reject v5 fields");
        assert!(error.contains("output_target"));

        let v4 = temp.path().join("retired-v4.toml");
        std::fs::write(
            &v4,
            r#"
name = "retired-v4"
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
        .expect("write retired v4 preset");
        let error = load_preset_from_path(&v4).expect_err("v4 gain schema must be rejected");
        assert!(error.contains("retired ambiguous gain schema"));

        let mut current = current_test_preset("bad-v5");
        current.version = 5;
        let mut encoded = toml::to_string_pretty(&current).expect("encode v5");
        encoded.push_str("\nfuture_field = true\n");
        let bad_v5 = temp.path().join("bad-v5.toml");
        std::fs::write(&bad_v5, encoded).expect("write bad v5 preset");
        let error = load_preset_from_path(&bad_v5).expect_err("v5 must reject unknown fields");
        assert!(error.contains("future_field"));
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
    fn aac_preset_with_now_impossible_concrete_rate_is_refused_explicitly() {
        let format = FormatState::new();
        let output = OutputOptionsState::new();
        let metadata = MetadataState::default();
        let mut preset = TuiPreset::from_pill_state("aac-legacy-rate", &format, &output, &metadata);
        preset.format = "aac".to_string();
        preset.sample_rate = 192_000;

        let mut restored_format = FormatState::new();
        let mut restored_output = OutputOptionsState::new();
        let mut restored_metadata = MetadataState::default();
        let report = preset.apply_to_pills(
            &mut restored_format,
            &mut restored_output,
            &mut restored_metadata,
        );

        assert!(report.refused_fields.iter().any(|field| field == "sample_rate"));
        assert!(report.status_suffix().contains("sample_rate"));
        assert_eq!(*restored_format.format.selected_value(), AudioFormat::Aac);
        assert_ne!(*restored_format.sample_rate.selected_value(), 192_000);
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
    fn v5_dsd_true_peak_guard_round_trips_target_scope_and_scan_for_track_or_album() {
        let mut source = FormatState::new();
        source.set_source_is_dsd(true);
        assert!(source.dsd_gain_mode.select_value(&DsdGainMode::TruePeakGuard));
        source.dsd_true_peak_target_dbtp = "-0.650000000".parse().unwrap();
        assert!(source
            .dsd_true_peak_scope
            .select_value(&tonepoet_pipeline::TruePeakScope::Track));
        assert!(source
            .dsd_true_peak_scan_mode
            .select_value(&tonepoet_pipeline::TruePeakScanTier::Standard));
        let preset = TuiPreset::from_pill_state(
            "dsd-guard-track",
            &source,
            &OutputOptionsState::new(),
            &MetadataState::default(),
        );
        assert_eq!(preset.version, 5);
        assert_eq!(preset.dsd_gain.as_deref(), Some("true-peak-guard"));
        assert_eq!(preset.dsd_true_peak_target_dbtp.as_deref(), Some("-0.650000000"));
        assert_eq!(preset.dsd_true_peak_scope.as_deref(), Some("track"));
        assert_eq!(preset.dsd_true_peak_scan.as_deref(), Some("fast066v2_standard"));

        let mut restored = FormatState::new();
        restored.set_source_is_dsd(true);
        let mut output = OutputOptionsState::new();
        let mut metadata = MetadataState::default();
        let report = preset.apply_to_pills(&mut restored, &mut output, &mut metadata);
        assert!(report.is_complete(), "unexpected refusals: {:?}", report.refused_fields);
        assert_eq!(restored.dsd_gain_mode.selected_value(), &DsdGainMode::TruePeakGuard);
        assert_eq!(restored.dsd_true_peak_target_dbtp, "-0.650000000".parse().unwrap());
        assert_eq!(restored.dsd_true_peak_scope.selected_value(), &tonepoet_pipeline::TruePeakScope::Track);
        assert_eq!(restored.dsd_true_peak_scan_mode.selected_value(), &tonepoet_pipeline::TruePeakScanTier::Standard);
    }

    #[test]
    fn v5_dsd_true_peak_normalize_round_trips_album_scope_and_reference_scan() {
        let mut source = FormatState::new();
        source.set_source_is_dsd(true);
        assert!(source.dsd_gain_mode.select_value(&DsdGainMode::TruePeakNormalize));
        source.dsd_true_peak_target_dbtp = "-1.250000000".parse().unwrap();
        assert!(source.dsd_true_peak_scope.select_value(&tonepoet_pipeline::TruePeakScope::Album));
        assert!(source.dsd_true_peak_scan_mode.select_value(&tonepoet_pipeline::TruePeakScanTier::Reference));
        let preset = TuiPreset::from_pill_state(
            "dsd-normalize-album",
            &source,
            &OutputOptionsState::new(),
            &MetadataState::default(),
        );

        let mut restored = FormatState::new();
        restored.set_source_is_dsd(true);
        let mut output = OutputOptionsState::new();
        let mut metadata = MetadataState::default();
        let report = preset.apply_to_pills(&mut restored, &mut output, &mut metadata);
        assert!(report.is_complete(), "unexpected refusals: {:?}", report.refused_fields);
        assert_eq!(restored.dsd_gain_mode.selected_value(), &DsdGainMode::TruePeakNormalize);
        assert_eq!(restored.dsd_true_peak_scope.selected_value(), &tonepoet_pipeline::TruePeakScope::Album);
        assert_eq!(restored.dsd_true_peak_scan_mode.selected_value(), &tonepoet_pipeline::TruePeakScanTier::Reference);
    }

    #[test]
    fn v5_dsd_fixed_gain_and_reference_auto_remain_distinct() {
        let mut source = FormatState::new();
        source.set_source_is_dsd(true);
        assert!(source.dsd_gain_mode.select_value(&DsdGainMode::FixedGain));
        source.dsd_gain_db = "2.250000000".parse().unwrap();
        let fixed = TuiPreset::from_pill_state(
            "dsd-fixed",
            &source,
            &OutputOptionsState::new(),
            &MetadataState::default(),
        );
        assert_eq!(fixed.dsd_gain.as_deref(), Some("fixed-gain"));
        assert_eq!(fixed.dsd_gain_db.as_deref(), Some("2.250000000"));

        source.dsd_pathway.select_value(&tonepoet_pipeline::DsdSourcePathway::Reference);
        source.apply_format_constraints();
        assert!(source.dsd_gain_mode.select_value(&DsdGainMode::ReferenceAuto));
        source.dsd_true_peak_target_dbtp = "-0.750000000".parse().unwrap();
        source.dsd_true_peak_scope.select_value(&tonepoet_pipeline::TruePeakScope::Album);
        source.dsd_true_peak_scan_mode.select_value(&tonepoet_pipeline::TruePeakScanTier::Standard);
        let sample_peak = TuiPreset::from_pill_state(
            "reference-auto",
            &source,
            &OutputOptionsState::new(),
            &MetadataState::default(),
        );
        assert_eq!(sample_peak.dsd_path.as_deref(), Some("reference"));
        assert_eq!(sample_peak.dsd_gain.as_deref(), Some("auto"));
        assert_eq!(sample_peak.dsd_true_peak_target_dbtp.as_deref(), Some("-0.750000000"));
        assert_eq!(sample_peak.dsd_true_peak_scope.as_deref(), Some("album"));
        assert_eq!(sample_peak.dsd_true_peak_scan.as_deref(), Some("fast066v2_standard"));
    }

    #[test]
    fn sparse_reference_auto_preset_receives_reference_defaults() {
        let mut source = FormatState::new();
        source.set_source_is_dsd(true);
        let before = *source.dsd_pathway.selected_value();
        assert!(source
            .dsd_pathway
            .select_value(&tonepoet_pipeline::DsdSourcePathway::Reference));
        source.apply_format_constraints();
        source.apply_user_dsd_path_transition(before);
        let mut preset = TuiPreset::from_pill_state(
            "reference-defaults",
            &source,
            &OutputOptionsState::new(),
            &MetadataState::default(),
        );
        preset.dsd_true_peak_target_dbtp = None;
        preset.dsd_true_peak_scope = None;
        preset.dsd_true_peak_scan = None;

        let mut restored = FormatState::new();
        restored.set_source_is_dsd(true);
        let mut output = OutputOptionsState::new();
        let mut metadata = MetadataState::default();
        let report = preset.apply_to_pills(&mut restored, &mut output, &mut metadata);

        assert!(report.is_complete(), "unexpected refusals: {:?}", report.refused_fields);
        assert_eq!(*restored.dsd_gain_mode.selected_value(), DsdGainMode::ReferenceAuto);
        assert_eq!(
            restored.dsd_true_peak_target_dbtp,
            tonepoet_pipeline::DbNano::DEFAULT_REFERENCE_TRUE_PEAK_TARGET
        );
        assert_eq!(
            *restored.dsd_true_peak_scope.selected_value(),
            tonepoet_pipeline::TruePeakScope::Album
        );
        assert_eq!(
            *restored.dsd_true_peak_scan_mode.selected_value(),
            tonepoet_pipeline::TruePeakScanTier::Standard
        );

        // Reapplying the sparse preset must not inherit edits made after the
        // first application merely because the pathway is already Reference.
        restored.dsd_true_peak_target_dbtp = "-0.250000000".parse().unwrap();
        restored
            .dsd_true_peak_scope
            .select_value(&tonepoet_pipeline::TruePeakScope::Track);
        restored
            .dsd_true_peak_scan_mode
            .select_value(&tonepoet_pipeline::TruePeakScanTier::Fast);
        let report = preset.apply_to_pills(&mut restored, &mut output, &mut metadata);
        assert!(report.is_complete(), "unexpected refusals: {:?}", report.refused_fields);
        assert_eq!(
            restored.dsd_true_peak_target_dbtp,
            tonepoet_pipeline::DbNano::DEFAULT_REFERENCE_TRUE_PEAK_TARGET
        );
        assert_eq!(
            *restored.dsd_true_peak_scope.selected_value(),
            tonepoet_pipeline::TruePeakScope::Album
        );
        assert_eq!(
            *restored.dsd_true_peak_scan_mode.selected_value(),
            tonepoet_pipeline::TruePeakScanTier::Standard
        );
    }

    #[test]
    fn reference_off_preset_overrides_canonical_reference_defaults() {
        let mut source = FormatState::new();
        source.set_source_is_dsd(true);
        assert!(source
            .dsd_pathway
            .select_value(&tonepoet_pipeline::DsdSourcePathway::Reference));
        source.apply_format_constraints();
        assert!(source.dsd_gain_mode.select_value(&DsdGainMode::Off));

        let preset = TuiPreset::from_pill_state(
            "reference-off",
            &source,
            &OutputOptionsState::new(),
            &MetadataState::default(),
        );
        assert_eq!(preset.dsd_path.as_deref(), Some("reference"));
        assert_eq!(preset.dsd_gain.as_deref(), Some("off"));

        let mut restored = FormatState::new();
        restored.set_source_is_dsd(true);
        let mut output = OutputOptionsState::new();
        let mut metadata = MetadataState::default();
        let report = preset.apply_to_pills(&mut restored, &mut output, &mut metadata);

        assert!(report.is_complete(), "unexpected refusals: {:?}", report.refused_fields);
        assert_eq!(
            *restored.dsd_pathway.selected_value(),
            tonepoet_pipeline::DsdSourcePathway::Reference
        );
        assert_eq!(*restored.dsd_gain_mode.selected_value(), DsdGainMode::Off);
    }

    #[test]
    fn malformed_dsd_scope_or_scan_is_refused_without_fallback_guessing() {
        let mut source = FormatState::new();
        source.set_source_is_dsd(true);
        assert!(source.dsd_gain_mode.select_value(&DsdGainMode::TruePeakGuard));
        let mut preset = TuiPreset::from_pill_state(
            "malformed-dsd-certified",
            &source,
            &OutputOptionsState::new(),
            &MetadataState::default(),
        );
        preset.dsd_true_peak_scope = Some("bogus".to_string());
        preset.dsd_true_peak_scan = Some("bogus".to_string());

        let mut restored = FormatState::new();
        restored.set_source_is_dsd(true);
        let mut output = OutputOptionsState::new();
        let mut metadata = MetadataState::default();
        let report = preset.apply_to_pills(&mut restored, &mut output, &mut metadata);
        assert!(report.refused_fields.contains(&"dsd_true_peak_scope".to_string()));
        assert!(report.refused_fields.contains(&"dsd_true_peak_scan".to_string()));
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

    #[test]
    fn pcm_true_peak_guard_and_normalize_round_trip_as_distinct_v5_policies() {
        for (mode, token) in [
            (PcmGainMode::TruePeakGuard, "true-peak-guard"),
            (PcmGainMode::TruePeakNormalize, "true-peak-normalize"),
        ] {
            let mut format = FormatState::new();
            assert!(format.pcm_gain_mode.select_value(&mode));
            format.pcm_true_peak_target_dbtp = "-0.625000000".parse().unwrap();
            assert!(format.pcm_true_peak_scope.select_value(&tonepoet_pipeline::TruePeakScope::Album));
            assert!(format.pcm_true_peak_scan_mode.select_value(&tonepoet_pipeline::TruePeakScanTier::Reference));
            let preset = TuiPreset::from_pill_state(
                token,
                &format,
                &OutputOptionsState::new(),
                &MetadataState::default(),
            );

            assert_eq!(preset.version, 5);
            assert_eq!(preset.pcm_gain.as_deref(), Some(token));
            assert_eq!(preset.pcm_true_peak_target_dbtp.as_deref(), Some("-0.625000000"));
            assert_eq!(preset.pcm_true_peak_scope.as_deref(), Some("album"));
            assert_eq!(preset.pcm_true_peak_scan.as_deref(), Some("fast066v2_reference"));

            let mut restored = FormatState::new();
            let mut output = OutputOptionsState::new();
            let mut metadata = MetadataState::default();
            let report = preset.apply_to_pills(&mut restored, &mut output, &mut metadata);
            assert!(report.is_complete(), "unexpected refusals: {:?}", report.refused_fields);
            assert_eq!(restored.pcm_gain_mode.selected_value(), &mode);
            assert_eq!(restored.pcm_true_peak_target_dbtp, "-0.625000000".parse().unwrap());
            assert_eq!(restored.pcm_true_peak_scope.selected_value(), &tonepoet_pipeline::TruePeakScope::Album);
            assert_eq!(restored.pcm_true_peak_scan_mode.selected_value(), &tonepoet_pipeline::TruePeakScanTier::Reference);
        }
    }

    #[test]
    fn pcm_fixed_gain_round_trips_as_mutually_exclusive_v5_policy() {
        let mut format = FormatState::new();
        assert!(format.pcm_gain_mode.select_value(&PcmGainMode::FixedGain));
        format.pcm_fixed_gain_db = "3.250000000".parse().unwrap();

        let preset = TuiPreset::from_pill_state(
            "pcm-fixed-gain",
            &format,
            &OutputOptionsState::new(),
            &MetadataState::default(),
        );
        assert_eq!(preset.pcm_gain.as_deref(), Some("fixed-gain"));
        assert_eq!(preset.pcm_fixed_gain_db.as_deref(), Some("3.250000000"));

        let mut restored = FormatState::new();
        let mut output = OutputOptionsState::new();
        let mut metadata = MetadataState::default();
        let report = preset.apply_to_pills(&mut restored, &mut output, &mut metadata);
        assert!(report.is_complete(), "unexpected refusals: {:?}", report.refused_fields);
        assert_eq!(restored.pcm_gain_mode.selected_value(), &PcmGainMode::FixedGain);
        assert_eq!(restored.pcm_fixed_gain_db, "3.250000000".parse().unwrap());
    }

    #[test]
    fn resolved_semantics_discloses_source_coupling_and_explicit_dither_off() {
        let mut format = FormatState::new();
        assert!(format.sample_rate.select_value(&SOURCE_SAMPLE_RATE_SENTINEL));
        assert!(format.bit_depth.select_value(&BitDepthChoice::Source));
        assert!(format.dither.select_value(&DitherType::None));
        let preset = TuiPreset::from_pill_state(
            "source-coupled-summary",
            &format,
            &OutputOptionsState::new(),
            &MetadataState::default(),
        );

        assert_eq!(
            preset.resolved_semantics_summary(),
            "rate: keep source | depth: keep source | dither: none"
        );
    }

    #[test]
    fn resolved_semantics_discloses_explicit_192khz_and_depth_label() {
        let mut format = FormatState::new();
        assert!(format.sample_rate.select_value(&192_000));
        assert!(format.bit_depth.select_value(&BitDepthChoice::Int32));
        assert!(format.dither.select_value(&DitherType::TPDF));
        let preset = TuiPreset::from_pill_state(
            "explicit-summary",
            &format,
            &OutputOptionsState::new(),
            &MetadataState::default(),
        );

        assert_eq!(
            preset.resolved_semantics_summary(),
            "rate: 192 kHz | depth: 32-bit | dither: tpdf"
        );
    }

}
