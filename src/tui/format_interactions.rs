//! Format-pane interaction helpers for key and mouse handlers.
//!
//! These helpers centralize row side effects so keyboard and mouse paths cannot
//! diverge: format changes refresh constraints, bit-depth changes apply
//! source-aware auto-dither, and explicit dither clicks preserve the override.

use super::app::{ConvertState, FormatField, FormatState};
use super::button_map::TuiButton;
use tonepoet_pipeline::{DbNano, TruePeakTargetOrigin};

/// Parse an interactive true-peak ceiling. An unsigned positive decimal means
/// headroom *below* 0 dBTP; an explicit minus is already a signed ceiling.
/// Keep `+` explicit rather than silently accepting a positive dBTP ceiling.
/// Zero remains valid, as it was before this interaction was introduced.
/// Custom input is quantized to a 0.0001 dB grid (four decimals). This avoids
/// offering arbitrary nano-dB set points that cannot be usefully resolved by
/// the true-peak terminal. A hard ceiling remains a maximum, not a promise to
/// produce a sample exactly at that level; qualified terminal headroom varies
/// with bit depth, dither, and processing path.
const TRUE_PEAK_TARGET_GRID_NANO: i64 = 100_000;

pub(super) fn parse_true_peak_target(raw: &str) -> Result<DbNano, String> {
    let raw = raw.trim();
    let parsed = raw.parse::<DbNano>()?;
    if parsed.0 > 0 && raw.starts_with('+') {
        return Err("a positive dBTP ceiling is not supported; omit '+' for headroom below zero, or use '-'".to_string());
    }
    let target = if parsed.0 > 0 { DbNano(-parsed.0) } else { parsed };
    if !(DbNano::MIN_NORMALIZE_TARGET..=DbNano::MAX_NORMALIZE_TARGET).contains(&target) {
        return Err("true-peak headroom must be between 0 and 12 dB (ceiling -12 through 0 dBTP)".to_string());
    }
    validate_true_peak_input_precision(target)?;
    Ok(target)
}

pub(super) fn validate_true_peak_input_precision(target: DbNano) -> Result<(), String> {
    if target.0 % TRUE_PEAK_TARGET_GRID_NANO != 0 {
        return Err("true-peak ceiling accepts at most four decimal places (0.0001 dBTP increments)".to_string());
    }
    Ok(())
}

/// Shared user-setting path for both `:set` and the in-place custom editor.
/// Validation precedes every state mutation; failed admission retains the
/// selected gain mode, value, and provenance intact.
pub(super) fn set_true_peak_target(
    format: &mut FormatState,
    field: FormatField,
    target: DbNano,
) -> Result<(), &'static str> {
    if !(DbNano::MIN_NORMALIZE_TARGET..=DbNano::MAX_NORMALIZE_TARGET).contains(&target) {
        return Err("true-peak ceiling must be between -12 and 0 dBTP");
    }
    if target.0 % TRUE_PEAK_TARGET_GRID_NANO != 0 {
        return Err("true-peak ceiling must be a multiple of 0.0001 dBTP");
    }
    match field {
        FormatField::PcmTruePeakTarget => {
            if !matches!(
                *format.pcm_gain_mode.selected_value(),
                super::app::PcmGainMode::TruePeakGuard | super::app::PcmGainMode::TruePeakNormalize
            ) && !format.pcm_gain_mode.select_value(&super::app::PcmGainMode::TruePeakGuard) {
                return Err("PCM true-peak gain is unavailable for the selected output");
            }
            format.pcm_true_peak_target_dbtp = target;
            format.pcm_true_peak_target_origin = TruePeakTargetOrigin::User;
            format.pcm_gain_overridden = true;
        }
        FormatField::DsdTruePeakTarget => {
            if *format.dsd_pathway.selected_value() == tonepoet_pipeline::DsdSourcePathway::Reference
                && !format.dsd_gain_mode.select_value(&super::app::DsdGainMode::ReferenceAuto)
            {
                return Err("Reference true-peak gain is unavailable for the current settings");
            }
            format.dsd_true_peak_target_dbtp = target;
            format.dsd_true_peak_target_origin = TruePeakTargetOrigin::User;
            format.dsd_gain_overridden = true;
        }
        _ => return Err("not a true-peak ceiling control"),
    }
    Ok(())
}

#[cfg(test)]
mod r30_tests {
    use super::*;

    #[test]
    fn r30_unsigned_headroom_and_signed_ceilings_are_exact() {
        for (input, expected) in [
            ("0.0001", -100_000),
            ("-0.0001", -100_000),
            ("0.0010", -1_000_000),
            ("0", 0),
            ("-12", -12_000_000_000),
            ("+0", 0),
        ] {
            assert_eq!(parse_true_peak_target(input).unwrap(), DbNano(expected), "{input}");
        }
        for input in ["+0.0001", "12.000000001", "-12.000000001", "12.1", "x", "-", "0.0000000001", "0.00001", "0.000000001"] {
            assert!(parse_true_peak_target(input).is_err(), "{input} must be refused");
        }
    }
}

/// Apply a keyboard-style next/previous action to the focused format row.
pub fn handle_format_row_step(format: &mut FormatState, forward: bool, source_bits: Option<u32>, source_rate: Option<u32>) {
    if forward {
        format.select_focused_next(source_bits, source_rate);
    } else {
        format.select_focused_prev(source_bits, source_rate);
    }
}

/// Convert-screen wrapper that supplies the probed source bit depth and rate.
pub fn handle_convert_format_row_step(convert: &mut ConvertState, forward: bool) {
    let before_dsd = convert.format.is_dsd_selected();
    let source_bits = convert.current_source_bit_depth();
    let source_rate = convert.current_source_sample_rate();
    handle_format_row_step(&mut convert.format, forward, source_bits, source_rate);
    cascade_dsd_source_to_pcm(convert, before_dsd);
}

/// Apply a mouse click on a format-pane pill.
///
/// Returns true only when the target pill exists and is enabled. Re-clicking
/// the enabled current pill is accepted because it converts an automatic value
/// into deliberate user policy.
pub fn handle_format_button(
    format: &mut FormatState,
    button: TuiButton,
    source_bits: Option<u32>,
    source_rate: Option<u32>,
) -> bool {
    match button {
        TuiButton::FormatPill(index) => format.select_row_index(
            FormatField::Format,
            index,
            source_bits,
            source_rate,
        ),
        TuiButton::RatePill(index) => {
            let row = if format.is_dsd_selected() {
                FormatField::DsdRate
            } else {
                FormatField::SampleRate
            };
            format.select_row_index(row, index, source_bits, source_rate)
        }
        TuiButton::DepthPill(index) => format.select_row_index(
            FormatField::BitDepth,
            index,
            source_bits,
            source_rate,
        ),
        TuiButton::ResamplerPill(index) => format.select_row_index(
            FormatField::Resampler,
            index,
            source_bits,
            source_rate,
        ),
        TuiButton::DitherPill(index) => format.select_row_index(
            FormatField::Dither,
            index,
            source_bits,
            source_rate,
        ),
        TuiButton::HdcdPill(index) => format.select_row_index(
            FormatField::HdcdDecode, index, source_bits, source_rate,
        ),
        TuiButton::DeemphasisPill(index) => format.select_row_index(
            FormatField::Deemphasis,
            index,
            source_bits,
            source_rate,
        ),
        TuiButton::ReplayGainPill(index) => format.select_row_index(
            FormatField::ReplayGain, index, source_bits, source_rate,
        ),
        TuiButton::PcmTruePeakPill(index) => format.select_row_index(
            FormatField::PcmTruePeak, index, source_bits, source_rate,
        ),
        TuiButton::TruePeakScopePill(index) => format.select_row_index(
            FormatField::TruePeakScope, index, source_bits, source_rate,
        ),
        TuiButton::PcmTruePeakScanPill(index) => format.select_row_index(
            FormatField::PcmTruePeakScan, index, source_bits, source_rate,
        ),
        TuiButton::PcmTruePeakTargetField => format.select_row_index(
            FormatField::PcmTruePeakTarget, 0, source_bits, source_rate,
        ),
        TuiButton::PcmGainDbField => format.select_row_index(
            FormatField::PcmGainDb, 0, source_bits, source_rate,
        ),
        TuiButton::NoiseShaperPill(index) => format.select_row_index(
            FormatField::NoiseShaper,
            index,
            source_bits,
            source_rate,
        ),
        TuiButton::ModulatorOrderPill(index) => format.select_row_index(
            FormatField::ModulatorOrder,
            index,
            source_bits,
            source_rate,
        ),
        TuiButton::ConversionPresetPill(index) => format.select_row_index(
            FormatField::ConversionPreset,
            index,
            source_bits,
            source_rate,
        ),
        TuiButton::DsdPathPill(index) => format.select_row_index(
            FormatField::DsdPath,
            index,
            source_bits,
            source_rate,
        ),
        TuiButton::DsdProfilePill(index) => format.select_row_index(
            FormatField::DsdProfile,
            index,
            source_bits,
            source_rate,
        ),
        TuiButton::DsdCustomReconstructionPill(index) => format.select_row_index(
            FormatField::DsdCustomReconstruction,
            index,
            source_bits,
            source_rate,
        ),
        TuiButton::DsdCustomLowpassPill(index) => format.select_row_index(
            FormatField::DsdCustomLowpass,
            index,
            source_bits,
            source_rate,
        ),
        TuiButton::DsdCustomExportLevelPill(index) => format.select_row_index(
            FormatField::DsdCustomExportLevel,
            index,
            source_bits,
            source_rate,
        ),
        TuiButton::DsdGainPill(index) => format.select_row_index(
            FormatField::DsdGain,
            index,
            source_bits,
            source_rate,
        ),
        TuiButton::DsdTruePeakScopePill(index) => format.select_row_index(
            FormatField::DsdTruePeakScope,
            index,
            source_bits,
            source_rate,
        ),
        TuiButton::DsdTruePeakScanPill(index) => format.select_row_index(
            FormatField::DsdTruePeakScan,
            index,
            source_bits,
            source_rate,
        ),
        TuiButton::DsdGainDbField => format.select_row_index(
            FormatField::DsdGainDb,
            0,
            source_bits,
            source_rate,
        ),
        TuiButton::DsdTruePeakTargetField => format.select_row_index(
            FormatField::DsdTruePeakTarget,
            0,
            source_bits,
            source_rate,
        ),
        TuiButton::ContainerPill(index) => format.select_row_index(
            FormatField::Container,
            index,
            source_bits,
            source_rate,
        ),
        TuiButton::ResampleQualityPill(index) => format.select_row_index(
            FormatField::ResampleQuality,
            index,
            source_bits,
            source_rate,
        ),
        _ => false,
    }
}

/// Convert-screen wrapper that supplies the probed source bit depth and rate.
pub fn handle_convert_format_button(convert: &mut ConvertState, button: TuiButton) -> bool {
    let before_dsd = convert.format.is_dsd_selected();
    let source_bits = convert.current_source_bit_depth();
    let source_rate = convert.current_source_sample_rate();
    let accepted = handle_format_button(&mut convert.format, button, source_bits, source_rate);
    if accepted {
        cascade_dsd_source_to_pcm(convert, before_dsd);
    }
    accepted
}

/// If the format just changed from DSD to PCM and the source is a DSD file,
/// auto-select the recommended PCM sample rate and 24-bit depth.
pub(crate) fn cascade_dsd_source_to_pcm(convert: &mut ConvertState, was_dsd_before: bool) {
    // Only cascade when transitioning FROM DSD output TO PCM output
    if !was_dsd_before || convert.format.is_dsd_selected() {
        return;
    }
    if let Some(source_rate) = convert.current_source_sample_rate() {
        convert.format.cascade_dsd_source_to_pcm_defaults(source_rate);
    }
}


#[cfg(test)]
mod r33_precision_regressions {
    use super::*;

    #[test]
    fn r33_custom_true_peak_grid_accepts_four_decimals_and_rejects_unrealizable_resolution() {
        for (text, value) in [
            ("0", 0), ("0.0001", -100_000), ("-0.1234", -123_400_000),
            ("11.9999", -11_999_900_000), ("-12.0000", -12_000_000_000),
        ] {
            assert_eq!(parse_true_peak_target(text).unwrap(), DbNano(value));
        }
        for input in ["0.00001", "-0.000000001", "-12.00001"] {
            assert!(parse_true_peak_target(input).is_err(), "{input}");
        }
    }
}
