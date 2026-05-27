//! Format-pane interaction helpers for key and mouse handlers.
//!
//! These helpers centralize row side effects so keyboard and mouse paths cannot
//! diverge: format changes refresh constraints, bit-depth changes apply
//! source-aware auto-dither, and explicit dither clicks preserve the override.

use super::app::{ConvertState, FormatField, FormatState};
use super::button_map::TuiButton;

/// Apply a keyboard-style next/previous action to the focused format row.
pub fn handle_format_row_step(format: &mut FormatState, forward: bool, source_bits: Option<u32>) {
    if forward {
        format.select_focused_next(source_bits);
    } else {
        format.select_focused_prev(source_bits);
    }
}

/// Convert-screen wrapper that supplies the probed source bit depth.
pub fn handle_convert_format_row_step(convert: &mut ConvertState, forward: bool) {
    let before_dsd = convert.format.is_dsd_selected();
    let source_bits = convert.current_source_bit_depth();
    handle_format_row_step(&mut convert.format, forward, source_bits);
    cascade_dsd_source_to_pcm(convert, before_dsd);
}

/// Apply a mouse click on a format-pane pill. Returns true when handled.
pub fn handle_format_button(
    format: &mut FormatState,
    button: TuiButton,
    source_bits: Option<u32>,
) -> bool {
    match button {
        TuiButton::FormatPill(index) => format.select_row_index(FormatField::Format, index, source_bits),
        TuiButton::RatePill(index) => {
            let row = if format.is_dsd_selected() {
                FormatField::DsdRate
            } else {
                FormatField::SampleRate
            };
            format.select_row_index(row, index, source_bits);
        }
        TuiButton::DepthPill(index) => format.select_row_index(FormatField::BitDepth, index, source_bits),
        TuiButton::ResamplerPill(index) => format.select_row_index(FormatField::Resampler, index, source_bits),
        TuiButton::DitherPill(index) => format.select_row_index(FormatField::Dither, index, source_bits),
        TuiButton::ReplayGainPill(index) => format.select_row_index(FormatField::ReplayGain, index, source_bits),
        TuiButton::NoiseShaperPill(index) => format.select_row_index(FormatField::NoiseShaper, index, source_bits),
        TuiButton::ModulatorOrderPill(index) => format.select_row_index(FormatField::ModulatorOrder, index, source_bits),
        TuiButton::ConversionPresetPill(index) => format.select_row_index(FormatField::ConversionPreset, index, source_bits),
        _ => return false,
    }
    true
}

/// Convert-screen wrapper that supplies the probed source bit depth.
pub fn handle_convert_format_button(convert: &mut ConvertState, button: TuiButton) -> bool {
    let before_dsd = convert.format.is_dsd_selected();
    let source_bits = convert.current_source_bit_depth();
    let handled = handle_format_button(&mut convert.format, button, source_bits);
    if handled {
        cascade_dsd_source_to_pcm(convert, before_dsd);
    }
    handled
}

/// If the format just changed from DSD to PCM and the source is a DSD file,
/// auto-select the recommended PCM sample rate and 24-bit depth.
fn cascade_dsd_source_to_pcm(convert: &mut ConvertState, was_dsd_before: bool) {
    // Only cascade when transitioning FROM DSD output TO PCM output
    if !was_dsd_before || convert.format.is_dsd_selected() {
        return;
    }
    if let Some(source_rate) = convert.current_source_sample_rate() {
        convert.format.cascade_dsd_source_to_pcm_defaults(source_rate);
    }
}
