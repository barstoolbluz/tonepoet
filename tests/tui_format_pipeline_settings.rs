//! Chunk 3 regression tests for dynamic TUI format state.
//!
//! If the crate keeps TUI modules private, move this file under a crate-local
//! `#[cfg(test)] mod tests` and keep the assertions unchanged.

use tonepoet::convert::formats::AudioFormat;
use tonepoet::convert::simple_wizard::DitherType;
use tonepoet::tui::app::{
    BitDepthChoice, FormatField, FormatState, MergeMode, OutputOptionsState, ReplayGainChoice,
    ResamplerChoice,
};
use tonepoet::tui::convert_actions::{format_state_to_pipeline_settings, try_pills_to_options};
use tonepoet::tui::presets::TuiPreset;
use tonepoet_pipeline::enums::{
    BitDepthTarget, DsdFilterPreset, DsdNoiseShaper, DsdRate, ModulatorOrder,
    NyquistTransition, PcmBitDepth, PreferredTool, RateTarget, ReplayGainMode,
};

fn config() -> tonepoet::config::TonepoetConfig {
    tonepoet::config::TonepoetConfig::default()
}

#[test]
fn pcm_format_state_maps_to_pipeline_settings_without_loss() {
    let mut state = FormatState::new();
    state.format.select_value(&AudioFormat::Flac);
    state.sample_rate.select_value(&96_000);
    state.bit_depth.select_value(&BitDepthChoice::Int24);
    state.resampler.select_value(&ResamplerChoice::Ssrc);
    state.dither.select_value(&DitherType::TPDF);
    state.replaygain.select_value(&ReplayGainChoice::Both);
    state.apply_format_constraints();

    let settings = format_state_to_pipeline_settings(&state).unwrap();

    assert_eq!(settings.target_format, tonepoet_pipeline::enums::AudioFormat::Flac);
    assert_eq!(settings.target_sample_rate, RateTarget::PcmHz(96_000));
    assert_eq!(settings.target_bit_depth, BitDepthTarget::Pcm(PcmBitDepth::Int24));
    assert_eq!(settings.preferred_tool, PreferredTool::Ssrc);
    assert_eq!(settings.nyquist_transition, NyquistTransition::BrickWall);
    assert_eq!(settings.replay_gain.mode, Some(ReplayGainMode::Both));
    settings.validate().unwrap();
}

#[test]
fn dsd_format_state_suppresses_hidden_pcm_and_replaygain_state() {
    let mut state = FormatState::new();
    state.replaygain.select_value(&ReplayGainChoice::Both);
    state.dither.select_value(&DitherType::Gesemann);
    state.format.select_value(&AudioFormat::Dsf);
    state.apply_format_constraints(); // enable DSD rates before selecting one
    state.sample_rate.select_value(&5_644_800);
    state.noise_shaper.select_value(&DsdNoiseShaper::Crfb);
    state.modulator_order.select_value(&ModulatorOrder::Order6);
    state.conversion_preset.select_value(&DsdFilterPreset::Sinc);

    let settings = format_state_to_pipeline_settings(&state).unwrap();

    assert_eq!(settings.target_format, tonepoet_pipeline::enums::AudioFormat::Dsf);
    assert_eq!(settings.target_sample_rate, RateTarget::Dsd(DsdRate::Dsd128));
    assert_eq!(settings.target_bit_depth, BitDepthTarget::Source);
    assert_eq!(settings.dither_type, tonepoet_pipeline::enums::DitherType::None);
    assert_eq!(settings.replay_gain.mode, None);
    assert_eq!(settings.dsd.noise_shaper, DsdNoiseShaper::Crfb);
    assert_eq!(settings.dsd.modulator_order, ModulatorOrder::Order6);
    assert_eq!(settings.dsd.pcm_to_dsd_filter, DsdFilterPreset::Sinc);
    settings.validate().unwrap();
}

#[test]
fn pills_to_options_keeps_legacy_fields_consistent_for_dsd() {
    let mut state = FormatState::new();
    state.format.select_value(&AudioFormat::Dff);
    state.sample_rate.select_value(&2_822_400);
    state.dither.select_value(&DitherType::Shibata);
    state.replaygain.select_value(&ReplayGainChoice::Album);
    state.apply_format_constraints();

    let output = OutputOptionsState::new();
    let options = try_pills_to_options(&state, &output, &config()).unwrap();

    assert_eq!(options.output_format, AudioFormat::Dff);
    assert_eq!(options.target_bit_depth, None);
    assert_eq!(options.dither_type, None);
    assert!(!options.calculate_replaygain);
    assert_eq!(options.replaygain_mode, None);
    assert!(options.pipeline_settings.is_some());
}

#[test]
fn auto_dither_selects_defaults_and_preserves_manual_choice() {
    let mut state = FormatState::new();

    state.select_bit_depth(BitDepthChoice::Int16, Some(24));
    assert_eq!(*state.dither.selected_value(), DitherType::Shibata);

    state.select_bit_depth(BitDepthChoice::Int24, Some(32));
    assert_eq!(*state.dither.selected_value(), DitherType::TPDF);

    state.select_bit_depth(BitDepthChoice::Int32, Some(24));
    assert_eq!(*state.dither.selected_value(), DitherType::None);

    state.dither.select_value(&DitherType::Gesemann);
    state.mark_dither_overridden();
    state.select_bit_depth(BitDepthChoice::Int16, Some(24));
    assert_eq!(*state.dither.selected_value(), DitherType::Gesemann);
}

#[test]
fn format_navigation_skips_hidden_rows() {
    let pcm_rows = FormatField::visible_rows(false);
    assert!(pcm_rows.contains(&FormatField::Resampler));
    assert!(pcm_rows.contains(&FormatField::Dither));
    assert!(!pcm_rows.contains(&FormatField::NoiseShaper));

    let dsd_rows = FormatField::visible_rows(true);
    assert!(dsd_rows.contains(&FormatField::DsdRate));
    assert!(dsd_rows.contains(&FormatField::NoiseShaper));
    assert!(!dsd_rows.contains(&FormatField::Resampler));
}

#[test]
fn preset_v3_round_trips_new_format_fields() {
    let mut format = FormatState::new();
    format.format.select_value(&AudioFormat::Dsf);
    format.apply_format_constraints(); // enable DSD rates before selecting one
    format.sample_rate.select_value(&11_289_600);
    format.noise_shaper.select_value(&DsdNoiseShaper::Sdm);
    format.modulator_order.select_value(&ModulatorOrder::Order7);
    format.conversion_preset.select_value(&DsdFilterPreset::Sinc);
    format.resampler.select_value(&ResamplerChoice::Soxr);

    let mut output = OutputOptionsState::new();
    output.merge.select_value(&MergeMode::SingleImage);

    let preset = TuiPreset::from_pill_state("roundtrip", &format, &output);
    let encoded = toml::to_string(&preset).unwrap();
    let decoded: TuiPreset = toml::from_str(&encoded).unwrap();

    let mut new_format = FormatState::new();
    let mut new_output = OutputOptionsState::new();
    decoded.apply_to_pills(&mut new_format, &mut new_output);

    assert_eq!(*new_format.format.selected_value(), AudioFormat::Dsf);
    assert_eq!(*new_format.sample_rate.selected_value(), 11_289_600);
    assert_eq!(*new_format.noise_shaper.selected_value(), DsdNoiseShaper::Sdm);
    assert_eq!(*new_format.modulator_order.selected_value(), ModulatorOrder::Order7);
    assert_eq!(*new_format.conversion_preset.selected_value(), DsdFilterPreset::Sinc);
    assert_eq!(*new_output.merge.selected_value(), MergeMode::SingleImage);
}

#[test]
fn preset_v2_loads_with_v3_defaults() {
    let preset: TuiPreset = toml::from_str(
        r#"
name = "v2"
version = 2
format = "flac"
sample_rate = 96000
bit_depth = "24"
dither = "tpdf"
replaygain = "off"
folder_template = "%ARTIST%/%ALBUM%"
filename_template = "%TRACKNN% - %TITLE%.%EXT%"
merge = "multi-file"
"#,
    )
    .unwrap();

    assert_eq!(preset.resampler, "sox");
    assert_eq!(preset.noise_shaper, None);
    assert_eq!(preset.modulator_order, None);
    assert_eq!(preset.dsd_filter_preset, None);
}

#[test]
fn auto_dither_unknown_source_depth_does_not_guess_reduction() {
    let mut state = FormatState::new();
    state.select_bit_depth(BitDepthChoice::Int24, None);
    assert_eq!(*state.dither.selected_value(), DitherType::None);

    state.select_bit_depth(BitDepthChoice::Int16, None);
    assert_eq!(*state.dither.selected_value(), DitherType::None);
}

#[test]
fn all_format_families_have_expected_visible_rows_and_valid_pipeline_mapping() {
    let cases = [
        (AudioFormat::Flac, false),
        (AudioFormat::Wav, false),
        (AudioFormat::Aiff, false),
        (AudioFormat::WavPack, false),
        (AudioFormat::Mp3, false),
        (AudioFormat::Aac, false),
        (AudioFormat::Opus, false),
        (AudioFormat::Alac, false),
        (AudioFormat::Dsf, true),
        (AudioFormat::Dff, true),
    ];

    for (format, is_dsd) in cases {
        let mut state = FormatState::new();
        state.format.select_value(&format);
        state.apply_format_constraints();

        let rows = FormatField::visible_rows(is_dsd);
        assert_eq!(state.is_dsd_selected(), is_dsd, "{:?}", format);
        assert_eq!(rows.contains(&FormatField::Resampler), !is_dsd, "{:?}", format);
        assert_eq!(rows.contains(&FormatField::Dither), !is_dsd, "{:?}", format);
        assert_eq!(rows.contains(&FormatField::ReplayGain), !is_dsd, "{:?}", format);
        assert_eq!(rows.contains(&FormatField::DsdRate), is_dsd, "{:?}", format);
        assert_eq!(rows.contains(&FormatField::NoiseShaper), is_dsd, "{:?}", format);
        assert_eq!(rows.contains(&FormatField::ModulatorOrder), is_dsd, "{:?}", format);
        assert_eq!(rows.contains(&FormatField::ConversionPreset), is_dsd, "{:?}", format);

        let pcm_rates_enabled = state
            .sample_rate
            .options
            .iter()
            .filter(|o| o.enabled && o.value < 2_822_400)
            .count();
        let dsd_rates_enabled = state
            .sample_rate
            .options
            .iter()
            .filter(|o| o.enabled && o.value >= 2_822_400)
            .count();
        assert_eq!(pcm_rates_enabled > 0, !is_dsd, "{:?}", format);
        assert_eq!(dsd_rates_enabled > 0, is_dsd, "{:?}", format);

        let settings = format_state_to_pipeline_settings(&state)
            .unwrap_or_else(|err| panic!("{:?}: {err}", format));
        settings.validate().unwrap();
    }
}

#[test]
fn mouse_registration_exposes_dynamic_format_buttons() {
    use ratatui::layout::Rect;
    use tonepoet::tui::button_map::{ButtonRenderMap, TuiButton};
    use tonepoet::tui::draw_output::register_format_pane_buttons;

    let area = Rect::new(0, 0, 120, 10);

    let mut pcm = FormatState::new();
    pcm.format.select_value(&AudioFormat::Flac);
    pcm.apply_format_constraints();
    let mut buttons = ButtonRenderMap::new();
    register_format_pane_buttons(&mut buttons, area, &pcm);
    assert!(matches!(buttons.find_button_at(17, 5), Some(TuiButton::ResamplerPill(_))));
    assert!(matches!(buttons.find_button_at(17, 6), Some(TuiButton::DitherPill(_))));
    assert!(matches!(buttons.find_button_at(17, 7), Some(TuiButton::ReplayGainPill(_))));

    let mut dsd = FormatState::new();
    dsd.format.select_value(&AudioFormat::Dsf);
    dsd.apply_format_constraints();
    let mut buttons = ButtonRenderMap::new();
    register_format_pane_buttons(&mut buttons, area, &dsd);
    assert!(matches!(buttons.find_button_at(17, 5), Some(TuiButton::NoiseShaperPill(_))));
    assert!(matches!(buttons.find_button_at(17, 6), Some(TuiButton::ModulatorOrderPill(_))));
    assert!(matches!(buttons.find_button_at(17, 7), Some(TuiButton::ConversionPresetPill(_))));
}
