//! Format pane: dynamic PCM/DSD output settings pills (green border)

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{ConvertDeemphasisEvidence, FormatField, FormatState, ResamplerChoice};
use super::pill::render_pill_spans;

/// Draw the format pane with green border.
///
/// For qualified Reference DSD-to-PCM, DSD-to-PCM targets expose the P0
/// Reference control surface and omit generic resampler/dither rows owned by
/// that policy. Pre-promotion releases render the ordinary legacy PCM controls.
pub fn draw_format_pane(
    f: &mut Frame,
    area: Rect,
    format_state: &FormatState,
    focused: bool,
    maximized: bool,
    theme: super::theme::Theme,
) {
    if area.height < 6 || area.width < 30 {
        return;
    }

    let border_color = if focused { theme.green } else { theme.text_dim };
    let w = area.width as usize;
    let top_line = format_title_line(border_color, w, maximized, theme);
    let bot_line = Line::from(Span::styled(
        format!("└{}┘", "─".repeat(w.saturating_sub(2))),
        theme.border(border_color),
    ));

    let mut lines = vec![top_line];
    // Owned outside the loop: `pill_row` borrows its suffix for as long as
    // `lines` lives.
    let hdcd_suffix: String = if let Some(warning) = format_state.hdcd_manual_off_warning() {
        warning.to_string()
    } else if format_state.hdcd_is_actionable() {
        "\u{24d8} HDCD detected".to_string()
    } else {
        String::new()
    };
    for row in format_state.pane_rows(maximized) {
        use super::app::FormatPaneRow;
        match row {
            FormatPaneRow::Spacer => lines.push(bordered_line(border_color, w, vec![], theme)),
            FormatPaneRow::ResamplingHeader => {
                let style = if focused { theme.bright() } else { theme.muted() };
                lines.push(bordered_line(
                    border_color,
                    w,
                    vec![Span::styled("   resampling", style)],
                    theme,
                ));
            }
            FormatPaneRow::Field(field) => {
                let row_focused = focused && format_state.field_focus == field;
                match field {
                    FormatField::Format => lines.push(pill_row(
                        border_color,
                        w,
                        "format     ",
                        "",
                        &render_pill_spans(&format_state.format, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::DsdRate => lines.push(pill_row(
                        border_color,
                        w,
                        "DSD rate   ",
                        "",
                        &render_pill_spans(&format_state.sample_rate, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::SampleRate => lines.push(pill_row(
                        border_color,
                        w,
                        "sample rate",
                        "kHz",
                        &render_pill_spans(&format_state.sample_rate, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::BitDepth => {
                        if let Some(preset_spans) = lossy_preset_spans(format_state, row_focused, theme) {
                            lines.push(pill_row(
                                border_color,
                                w,
                                "preset     ",
                                "",
                                &preset_spans,
                                row_focused,
                                theme,
                            ));
                        } else {
                            lines.push(pill_row(
                                border_color,
                                w,
                                "bit depth  ",
                                "bit",
                                &render_pill_spans(&format_state.bit_depth, row_focused, theme),
                                row_focused,
                                theme,
                            ));
                        }
                    }
                    FormatField::Resampler => lines.push(pill_row(
                        border_color,
                        w,
                        "resampler  ",
                        "",
                        &render_pill_spans(&format_state.resampler, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::Dither => {
                        let ssrc_override = format_state.ssrc_dither_override_active();
                        let rendered = render_pill_spans(
                            &format_state.dither,
                            row_focused && !ssrc_override,
                            theme,
                        );
                        let spans = if ssrc_override {
                            dim_pill_spans(&rendered, theme)
                        } else {
                            rendered
                        };
                        lines.push(pill_row(
                            border_color,
                            w,
                            "dither     ",
                            format_state.ssrc_dither_status_label().unwrap_or(""),
                            &spans,
                            row_focused && !ssrc_override,
                            theme,
                        ));
                    }
                    FormatField::OutputChannelLayout => lines.push(pill_row(
                        border_color,
                        w,
                        "channels   ",
                        "",
                        &render_pill_spans(&format_state.output_channel_layout, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::RemoveDcOffset => lines.push(pill_row(
                        border_color,
                        w,
                        "DC removal ",
                        "2 Hz HPF",
                        &deemphasis_pill_spans(format_state.remove_dc_offset, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::Deemphasis => {
                        let suffix = deemphasis_status_suffix(format_state);
                        lines.push(pill_row(
                            border_color,
                            w,
                            "de-emphasis",
                            suffix,
                            &deemphasis_pill_spans(format_state.deemphasis_enabled, row_focused, theme),
                            row_focused,
                            theme,
                        ));
                    }
                    FormatField::HdcdDecode => {
                        let spans = deemphasis_pill_spans(
                            format_state.hdcd_decode_enabled, row_focused, theme);
                        lines.push(pill_row(
                            border_color, w, "HDCD Decode", hdcd_suffix.as_str(), &spans,
                            row_focused, theme,
                        ));
                    }
                    FormatField::PcmTruePeak => lines.push(pill_row(
                        border_color, w, "PCM gain   ", "",
                        &render_pill_spans(&format_state.pcm_gain_mode, row_focused, theme),
                        row_focused, theme,
                    )),
                    FormatField::PcmGainDb => lines.push(dsd_db_value_row(
                        border_color,
                        w,
                        "gain       ",
                        format_state.pcm_fixed_gain_db,
                        true,
                        row_focused,
                        "",
                        theme,
                    )),
                    FormatField::PcmTruePeakTarget => lines.push(pcm_true_peak_db_value_row(
                        border_color,
                        w,
                        format_state.pcm_true_peak_target_dbtp,
                        (format_state.true_peak_target_editing == Some(field))
                            .then_some(&format_state.true_peak_target_input),
                        row_focused,
                        format_state.pcm_true_peak_target_is_capped(),
                        theme,
                    )),
                    FormatField::TruePeakScope => lines.push(pill_row(
                        border_color, w, "TP scope   ", "",
                        &render_pill_spans(&format_state.pcm_true_peak_scope, row_focused, theme),
                        row_focused, theme,
                    )),
                    FormatField::PcmTruePeakScan => lines.push(pill_row(
                        border_color, w, "TP scan    ", "",
                        &render_pill_spans(
                            &format_state.pcm_true_peak_scan_mode,
                            row_focused,
                            theme,
                        ),
                        row_focused, theme,
                    )),
                    FormatField::ReplayGain => lines.push(pill_row(
                        border_color,
                        w,
                        "replaygain ",
                        "",
                        &render_pill_spans(&format_state.replaygain, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::NoiseShaper => lines.push(pill_row(
                        border_color,
                        w,
                        "noise sh.  ",
                        "",
                        &render_pill_spans(&format_state.noise_shaper, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::ModulatorOrder => lines.push(pill_row(
                        border_color,
                        w,
                        "mod order  ",
                        "",
                        &render_pill_spans(&format_state.modulator_order, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::ConversionPreset => lines.push(pill_row(
                        border_color,
                        w,
                        "preset     ",
                        "",
                        &render_pill_spans(&format_state.conversion_preset, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::DsdPath => lines.push(pill_row(
                        border_color,
                        w,
                        "DSD path   ",
                        "",
                        &render_pill_spans(&format_state.dsd_pathway, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::DsdProfile => lines.push(pill_row(
                        border_color,
                        w,
                        "DSD profile",
                        "",
                        &render_pill_spans(&format_state.dsd_profile, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::DsdCustomReconstruction => lines.push(pill_row(
                        border_color,
                        w,
                        "reconstruct ",
                        "",
                        &render_pill_spans(&format_state.dsd_custom_reconstruction, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::DsdCustomLowpass => lines.push(pill_row(
                        border_color,
                        w,
                        "low-pass   ",
                        "",
                        &render_pill_spans(&format_state.dsd_custom_lowpass, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::DsdCustomExportLevel => lines.push(pill_row(
                        border_color,
                        w,
                        "base level ",
                        "",
                        &render_pill_spans(&format_state.dsd_custom_export_level, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::DsdGain => lines.push(pill_row(
                        border_color,
                        w,
                        "DSD gain   ",
                        "",
                        &render_enabled_pill_spans(&format_state.dsd_gain_mode, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::DsdTruePeakScope => lines.push(pill_row(
                        border_color,
                        w,
                        "TP scope   ",
                        "",
                        &render_pill_spans(&format_state.dsd_true_peak_scope, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::DsdTruePeakScan => lines.push(pill_row(
                        border_color,
                        w,
                        "TP scan    ",
                        "",
                        &render_pill_spans(&format_state.dsd_true_peak_scan_mode, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::DsdGainDb => lines.push(dsd_db_value_row(
                        border_color,
                        w,
                        "gain dB    ",
                        format_state.dsd_gain_db,
                        true,
                        row_focused,
                        "",
                        theme,
                    )),
                    FormatField::DsdTruePeakTarget => lines.push(pcm_true_peak_db_value_row(
                        border_color,
                        w,
                        format_state.dsd_true_peak_target_dbtp,
                        (format_state.true_peak_target_editing == Some(field))
                            .then_some(&format_state.true_peak_target_input),
                        row_focused,
                        false,
                        theme,
                    )),
                    FormatField::Container => {
                        let containers = format_state.format.selected_value().available_containers();
                        let spans: Vec<Span> = containers
                            .iter()
                            .enumerate()
                            .flat_map(|(i, container)| {
                                let selected = i == format_state.selected_container_index;
                                let style = if !container.enabled {
                                    Style::default().fg(theme.text_dim)
                                } else if selected && row_focused {
                                    Style::default()
                                        .fg(theme.pill_active_fg)
                                        .bg(theme.green)
                                        .add_modifier(Modifier::BOLD)
                                } else if selected {
                                    Style::default().fg(theme.green).add_modifier(Modifier::BOLD)
                                } else {
                                    Style::default().fg(theme.text_dim)
                                };
                                let mut spans = vec![Span::styled(
                                    format!(" {} ", container.display_name),
                                    style,
                                )];
                                if i + 1 < containers.len() {
                                    spans.push(Span::styled(" ", Style::default()));
                                }
                                spans
                            })
                            .collect();
                        let has_settings = matches!(
                            *format_state.format.selected_value(),
                            crate::convert::formats::AudioFormat::Flac
                                | crate::convert::formats::AudioFormat::Aac
                                | crate::convert::formats::AudioFormat::Opus
                                | crate::convert::formats::AudioFormat::Mp3
                                | crate::convert::formats::AudioFormat::WavPack
                        );
                        if has_settings {
                            let name = format_state.format.selected_value().name().to_lowercase();
                            lines.push(container_row_with_settings_pill(
                                border_color,
                                w,
                                &spans,
                                row_focused,
                                &name,
                                theme,
                            ));
                        } else {
                            lines.push(pill_row(
                                border_color,
                                w,
                                "container  ",
                                "",
                                &spans,
                                row_focused,
                                theme,
                            ));
                        }
                    }
                    FormatField::ResampleQuality => {
                        let qualities = format_state.resample_quality_choices();
                        let count = qualities.len();
                        let spans: Vec<Span> = qualities
                            .iter()
                            .enumerate()
                            .flat_map(|(i, (quality, label))| {
                                let selected = *quality == format_state.resample_quality;
                                let style = if selected && row_focused {
                                    Style::default()
                                        .fg(theme.pill_active_fg)
                                        .bg(theme.green)
                                        .add_modifier(Modifier::BOLD)
                                } else if selected {
                                    Style::default().fg(theme.green).add_modifier(Modifier::BOLD)
                                } else {
                                    Style::default().fg(theme.text_dim)
                                };
                                let mut spans = vec![Span::styled(format!(" {label} "), style)];
                                if i + 1 < count {
                                    spans.push(Span::styled(" ", Style::default()));
                                }
                                spans
                            })
                            .collect();
                        let resampler_name = match *format_state.resampler.selected_value() {
                            ResamplerChoice::Ssrc => Some("ssrc"),
                            ResamplerChoice::Sox => Some("sox"),
                            ResamplerChoice::Soxr => Some("soxr"),
                            ResamplerChoice::None => None,
                        };
                        if let Some(name) = resampler_name {
                            lines.push(row_with_settings_pill(
                                border_color,
                                w,
                                "preset     ",
                                &spans,
                                row_focused,
                                name,
                                theme,
                            ));
                        } else {
                            lines.push(pill_row(
                                border_color,
                                w,
                                "preset     ",
                                "",
                                &spans,
                                row_focused,
                                theme,
                            ));
                        }
                    }
                }
            }
        }
    }

    lines.push(bordered_line(border_color, w, vec![], theme));
    let target_len_before_bottom = area.height.saturating_sub(1) as usize;
    while lines.len() < target_len_before_bottom {
        lines.push(bordered_line(border_color, w, vec![], theme));
    }
    lines.push(bot_line);
    f.render_widget(Paragraph::new(lines), area);
}

fn deemphasis_status_suffix(format_state: &FormatState) -> &'static str {
    match (
        format_state.deemphasis_enabled,
        format_state.deemphasis_evidence,
    ) {
        (_, ConvertDeemphasisEvidence::None) => "",
        (true, _) => "ⓘ pre-emphasis will be corrected.",
        (false, ConvertDeemphasisEvidence::ExplicitTag) => {
            "ⓘ pre-emphasis detected: turn on to correct it."
        }
        (false, ConvertDeemphasisEvidence::CueFlag) => {
            "ⓘ pre-emphasis flagged in the CUE: turn on to correct it."
        }
        (false, ConvertDeemphasisEvidence::CatalogExact) => {
            "ⓘ this pressing is known pre-emphasised: turn on to correct it."
        }
    }
}

fn deemphasis_pill_spans(
    enabled: bool,
    row_focused: bool,
    theme: super::theme::Theme,
) -> Vec<Span<'static>> {
    [false, true]
        .into_iter()
        .enumerate()
        .flat_map(|(index, value)| {
            let selected = value == enabled;
            let style = if selected {
                Style::default()
                    .fg(theme.pill_active_fg)
                    .bg(theme.pill_active_bg)
                    .add_modifier(Modifier::BOLD)
            } else if row_focused {
                Style::default().fg(theme.text_muted)
            } else {
                Style::default().fg(theme.text_dim)
            };
            let mut spans = Vec::new();
            if index > 0 {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(if value { " on " } else { " off " }, style));
            spans
        })
        .collect()
}

fn render_enabled_pill_spans<T: Clone>(
    state: &super::pill::PillState<T>,
    row_focused: bool,
    theme: super::theme::Theme,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (visible_index, (index, option)) in state
        .options
        .iter()
        .enumerate()
        .filter(|(_, option)| option.enabled)
        .enumerate()
    {
        if visible_index > 0 {
            spans.push(Span::raw("  "));
        }
        let selected = index == state.selected;
        let style = if selected {
            Style::default()
                .fg(theme.pill_active_fg)
                .bg(theme.pill_active_bg)
                .add_modifier(Modifier::BOLD)
        } else if row_focused {
            Style::default().fg(theme.text_muted)
        } else {
            Style::default().fg(theme.text_dim)
        };
        spans.push(Span::styled(format!(" {} ", option.label), style));
    }
    spans
}

/// Draw the collapsed format title bar.
pub fn draw_format_title_bar(f: &mut Frame, area: Rect, focused: bool, theme: super::theme::Theme) {
    if area.height < 1 || area.width < 12 {
        return;
    }
    let border_color = if focused { theme.green } else { theme.text_dim };
    f.render_widget(
        Paragraph::new(vec![format_title_line(border_color, area.width as usize, false, theme)]),
        area,
    );
}

fn format_title_line<'a>(border_color: ratatui::style::Color, width: usize, maximized: bool,
    theme: super::theme::Theme,
) -> Line<'a> {
    let title = " format ";
    let indicator = if maximized { "▾" } else { "▸" };
    let bar_style = Style::default().fg(theme.bg).bg(border_color);
    let left_spans = vec![
        Span::styled("┌", theme.border(border_color)),
        Span::styled(format!(" {indicator}{title}"), bar_style),
    ];
    let right_spans = vec![
        Span::styled("a", Style::default().fg(theme.text_muted).bg(border_color)),
        Span::styled("dvanced ", bar_style),
        Span::styled("┐", theme.border(border_color)),
    ];
    let fixed_width = Line::from(left_spans.clone()).width()
        + Line::from(right_spans.clone()).width();
    let fill_count = width.saturating_sub(fixed_width);
    let mut spans = left_spans;
    spans.push(Span::styled(
        " ".repeat(fill_count),
        bar_style,
    ));
    spans.extend(right_spans);
    Line::from(spans)
}

const TRUE_PEAK_EDITOR_WIDTH: usize = 12;
const TRUE_PEAK_CUSTOM_PILL: &str = " custom ";
// Shared by drawing and button registration. The initial border occupies
// one cell; the value begins in the same control column as the other rows.
const TRUE_PEAK_ROW_BEFORE_VALUE: &str = "   TP target    ";

/// Exact human-readable ceiling: at least two decimals, all significant
/// nanodecimals. A numeric format specifier must not round an exact target.
pub(super) fn true_peak_ceiling_label(value: tonepoet_pipeline::DbNano) -> String {
    let exact = value.render(false);
    let (whole, fraction) = exact.split_once('.').expect("DbNano renders a decimal");
    let significant = fraction.trim_end_matches('0');
    if significant.len() < 2 {
        format!("{whole}.{:0<2}", significant)
    } else {
        format!("{whole}.{significant}")
    }
}

/// Offset of the visible `custom` pill from the left edge of its format row.
/// Drawing and mouse hit registration use this exact layout calculation.
pub(super) fn true_peak_custom_pill_offset(
    value: tonepoet_pipeline::DbNano,
    editing: bool,
    row_width: usize,
) -> Option<u16> {
    let offset = true_peak_value_end_offset(value, editing);
    (offset + TRUE_PEAK_CUSTOM_PILL.len() + 1 <= row_width)
        .then_some(offset as u16)
}

fn true_peak_value_end_offset(value: tonepoet_pipeline::DbNano, editing: bool) -> usize {
    let value_width = if editing {
        TRUE_PEAK_EDITOR_WIDTH + " dBTP".len()
    } else {
        true_peak_ceiling_label(value).len() + " dBTP".len()
    };
    1 + TRUE_PEAK_ROW_BEFORE_VALUE.len() + "< ".len() + value_width + " >  ".len()
}

/// Display-cell offset of the end-of-controls glyph, or None if it cannot
/// fit. Custom-pill and glyph hit targets follow precisely the drawn row.
pub(super) fn true_peak_info_offset(
    value: tonepoet_pipeline::DbNano,
    editing: bool,
    row_width: usize,
) -> Option<u16> {
    let offset = true_peak_custom_pill_offset(value, editing, row_width)
        .map(|start| start as usize + TRUE_PEAK_CUSTOM_PILL.len() + 2)
        .unwrap_or_else(|| true_peak_value_end_offset(value, editing));
    (offset + 2 + 1 <= row_width).then_some(offset as u16)
}

fn pcm_true_peak_db_value_row(
    border_color: ratatui::style::Color,
    width: usize,
    value: tonepoet_pipeline::DbNano,
    edit_input: Option<&super::text_input::TextInputState>,
    focused: bool,
    lossy_capped: bool,
    theme: super::theme::Theme,
) -> Line<'static> {
    let label_style = if focused { theme.bright() } else { theme.muted() };
    let control_style = if focused {
        theme.bright().add_modifier(Modifier::BOLD)
    } else {
        theme.muted()
    };
    let mut spans = vec![
        Span::styled("│", theme.border(border_color)),
        Span::styled(TRUE_PEAK_ROW_BEFORE_VALUE, label_style),
        Span::styled("< ", control_style),
    ];
    if let Some(input) = edit_input {
        spans.extend(super::inline_edit::render_inline_value_with_embedded_cursor(
            input,
            TRUE_PEAK_EDITOR_WIDTH,
            theme,
        ));
        spans.push(Span::styled(" dBTP", control_style));
    } else {
        spans.push(Span::styled(
            format!("{} dBTP", true_peak_ceiling_label(value)),
            control_style,
        ));
    }
    spans.push(Span::styled(" >", control_style));
    spans.push(Span::raw("  "));
    if true_peak_custom_pill_offset(value, edit_input.is_some(), width).is_some() {
        let custom_style = if edit_input.is_some() {
            Style::default()
                .fg(theme.pill_active_fg)
                .bg(theme.pill_active_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(if focused { theme.text_bright } else { theme.text_muted })
                .bg(theme.pill_dim_bg)
                .add_modifier(Modifier::BOLD)
        };
        spans.push(Span::styled(TRUE_PEAK_CUSTOM_PILL, custom_style));
    }
    if true_peak_info_offset(value, edit_input.is_some(), width).is_some() {
        if true_peak_custom_pill_offset(value, edit_input.is_some(), width).is_some() {
            spans.push(Span::raw("  "));
        }
        // U+24D8 may paint across two cells; its guard cell uses the same
        // contrasting foreground, as on the de-emphasis and HDCD controls.
        spans.push(Span::styled(
            "ⓘ ",
            Style::default()
                .fg(if theme.dark { theme.info } else { theme.value })
                .add_modifier(Modifier::BOLD),
        ));
    }
    let hint = if edit_input.is_some() {
        "  Enter: apply  Esc: cancel  unsigned = headroom".to_string()
    } else if lossy_capped {
        format!(
            "  lossy encoder input capped at {} dBTP",
            tonepoet_pipeline::PCM_TRUE_PEAK_LOSSY_MAX_TARGET_DBTP.render(true),
        )
    } else {
        String::new()
    };
    let room = width.saturating_sub(spans.iter().map(|span| span.width()).sum::<usize>() + 1);
    let hint = hint.chars().take(room).collect::<String>();
    spans.push(Span::styled(hint, theme.muted()));
    let content_width: usize = spans.iter().map(|span| span.width()).sum();
    spans.push(Span::raw(" ".repeat(width.saturating_sub(content_width + 1))));
    spans.push(Span::styled("│", theme.border(border_color)));
    Line::from(spans)
}

fn dsd_db_value_row(
    border_color: ratatui::style::Color,
    width: usize,
    label: &'static str,
    value: tonepoet_pipeline::DbNano,
    enabled: bool,
    focused: bool,
    disabled_hint: &'static str,
    theme: super::theme::Theme,
) -> Line<'static> {
    let label_style = if focused { theme.bright() } else { theme.muted() };
    let control_style = if focused {
        theme.bright().add_modifier(Modifier::BOLD)
    } else if enabled {
        theme.muted()
    } else {
        Style::default().fg(theme.text_dim)
    };
    let hint_style = if enabled {
        theme.muted()
    } else {
        Style::default().fg(theme.text_dim)
    };

    let mut spans = vec![
        Span::styled("│", theme.border(border_color)),
        Span::styled(format!("   {label}"), label_style),
        Span::styled("< ", control_style),
        Span::styled(format!("{} dB", value.render(true)), control_style),
        Span::styled(" >", control_style),
    ];
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        if enabled { "left/right adjust" } else { disabled_hint },
        hint_style,
    ));

    let content_width: usize = spans.iter().map(|s| s.width()).sum();
    let padding = width.saturating_sub(content_width + 1);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", theme.border(border_color)));

    Line::from(spans)
}

fn lossy_preset_spans(
    format_state: &FormatState,
    focused: bool,
    theme: super::theme::Theme,
) -> Option<Vec<Span<'static>>> {
    let labels = format_state.lossy_preset_labels()?;
    let selected_index = format_state
        .lossy_preset_index()
        .unwrap_or_else(|| labels.len().saturating_sub(1));
    let count = labels.len();
    Some(
        labels
            .into_iter()
            .enumerate()
            .flat_map(|(i, label)| {
                let selected = i == selected_index;
                let style = if selected && focused {
                    Style::default()
                        .fg(theme.pill_active_fg)
                        .bg(theme.green)
                        .add_modifier(Modifier::BOLD)
                } else if selected {
                    Style::default().fg(theme.green).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text_dim)
                };
                let mut spans = vec![Span::styled(format!(" {label} "), style)];
                if i + 1 < count {
                    spans.push(Span::styled(" ", Style::default()));
                }
                spans
            })
            .collect(),
    )
}

fn dim_pill_spans<'a>(spans: &[Span<'a>],
    theme: super::theme::Theme,
) -> Vec<Span<'a>> {
    spans
        .iter()
        .map(|span| Span::styled(span.content.clone(), Style::default().fg(theme.text_dim)))
        .collect()
}

fn pill_row<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    label: &'a str,
    suffix: &'a str,
    pills: &[Span<'a>],
    focused: bool,
    theme: super::theme::Theme,
) -> Line<'a> {
    let label_style = if focused { theme.bright() } else { theme.muted() };

    let mut spans = vec![
        Span::styled("│", theme.border(border_color)),
        Span::styled(format!("   {}  ", label), label_style),
    ];
    spans.extend_from_slice(pills);

    if !suffix.is_empty() {
        spans.push(Span::raw("  "));
        if let Some(rest) = suffix.strip_prefix('ⓘ') {
            // U+24D8 is East Asian Width=Ambiguous. Ratatui/unicode-width
            // treats it as one cell, while some terminals paint it across two.
            // Give the immediately following delimiter cell the same style so
            // either rendering width paints the entire glyph with one
            // foreground color. Consuming the suffix's existing space keeps
            // layout width unchanged on one-cell terminals.
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            let info_fg = if theme.dark { theme.info } else { theme.value };
            spans.push(Span::styled(
                "ⓘ ",
                Style::default()
                    .fg(info_fg)
                    .add_modifier(Modifier::BOLD),
            ));
            if !rest.is_empty() {
                spans.push(Span::styled(rest, theme.muted()));
            }
        } else {
            spans.push(Span::styled(suffix, theme.muted()));
        }
    }

    let content_width: usize = spans.iter().map(|s| s.width()).sum();
    let padding = width.saturating_sub(content_width + 1);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", theme.border(border_color)));

    Line::from(spans)
}

fn bordered_line<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    content: Vec<Span<'a>>,
    theme: super::theme::Theme,
) -> Line<'a> {
    let content_width: usize = content.iter().map(|s| s.width()).sum();
    let padding = width.saturating_sub(2 + content_width);

    let mut spans = vec![Span::styled("│", theme.border(border_color))];
    spans.extend(content);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", theme.border(border_color)));
    Line::from(spans)
}

/// Container row with a right-aligned `<format> settings` pill.
fn container_row_with_settings_pill<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    pills: &[Span<'a>],
    focused: bool,
    settings_name: &str,
    theme: super::theme::Theme,
) -> Line<'a> {
    row_with_settings_pill(border_color, width, "container  ", pills, focused, settings_name, theme)
}

/// Generic row with left-aligned pills and a right-aligned settings pill.
fn row_with_settings_pill<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    label: &'a str,
    pills: &[Span<'a>],
    focused: bool,
    settings_name: &str,
    theme: super::theme::Theme,
) -> Line<'a> {
    let label_style = if focused { theme.bright() } else { theme.muted() };
    let pill_text = format!(" {} settings ", settings_name);
    let pill_width = pill_text.len();
    let settings_pill = Span::styled(
        pill_text,
        Style::default()
            .fg(theme.pill_active_fg)
            .bg(theme.purple)
            .add_modifier(Modifier::BOLD),
    );

    let mut spans = vec![
        Span::styled("│", theme.border(border_color)),
        Span::styled(format!("   {}  ", label), label_style),
    ];
    spans.extend_from_slice(pills);

    let content_width: usize = spans.iter().map(|s| s.width()).sum();
    let needed = content_width + pill_width + 1; // +1 for right border
    if needed + 1 <= width {
        // Enough room: pad, then pill, then border.
        let padding = width - needed;
        spans.push(Span::raw(" ".repeat(padding)));
        spans.push(settings_pill);
    } else {
        // Too narrow: skip the pill, pad normally.
        let padding = width.saturating_sub(content_width + 1);
        spans.push(Span::raw(" ".repeat(padding)));
    }
    spans.push(Span::styled("│", theme.border(border_color)));
    Line::from(spans)
}

#[cfg(test)]
mod r24_deemphasis_info_affordance_tests {
    use super::*;

    #[test]
    fn deemphasis_info_glyph_uses_foreground_only_high_contrast_theme_color() {
        for palette in super::super::theme::palettes() {
            let theme = super::super::theme::theme_by_slug(palette.slug).expect("shipped theme");
            let expected_fg = if theme.dark { theme.info } else { theme.value };
            for focused in [false, true] {
                let line = pill_row(
                    theme.green,
                    96,
                    "De-emphasis",
                    "ⓘ pre-emphasis will be corrected.",
                    &[],
                    focused,
                    theme,
                );
                let info = line
                    .spans
                    .iter()
                    .find(|span| span.content.as_ref() == "ⓘ ")
                    .expect("information glyph + guard-cell span");

                assert_eq!(
                    info.style.fg,
                    Some(expected_fg),
                    "theme {}, focused={focused}",
                    palette.slug,
                );
                assert_eq!(
                    info.style.bg,
                    None,
                    "theme {}, focused={focused}",
                    palette.slug,
                );
                assert_ne!(
                    expected_fg,
                    theme.text_muted,
                    "theme {} needs a distinct affordance color",
                    palette.slug,
                );
                assert!(
                    super::super::theme::contrast_ratio(expected_fg, theme.panel_bg) >= 4.5,
                    "theme {} needs a legible information glyph",
                    palette.slug,
                );
                assert!(info.style.add_modifier.contains(Modifier::BOLD));
                assert_eq!(
                    info.width(),
                    2,
                    "glyph guard span must reserve the existing delimiter cell",
                );
                let info_index = line
                    .spans
                    .iter()
                    .position(|span| span.content.as_ref() == "ⓘ ")
                    .expect("information span index");
                let guidance = line
                    .spans
                    .get(info_index + 1)
                    .expect("guidance after information span");
                assert_eq!(
                    guidance.content.as_ref(),
                    "pre-emphasis will be corrected.",
                    "the guard cell must consume, not add, the existing delimiter",
                );
            }
        }
    }
}

#[cfg(test)]
mod r30_true_peak_custom_control_tests {
    use super::*;
    use tonepoet_pipeline::DbNano;

    #[test]
    fn r30_true_peak_labels_have_two_decimals_without_losing_nano_precision() {
        for (value, expected) in [
            (DbNano(-100_000_000), "-0.10"),
            (DbNano(-12_000_000_000), "-12.00"),
            (DbNano(-125_000_000), "-0.125"),
            (DbNano(-100_000), "-0.0001"),
            (DbNano(-1), "-0.000000001"),
            (DbNano::ZERO, "0.00"),
            (DbNano(i64::MIN), "-9223372036.854775808"),
        ] {
            assert_eq!(true_peak_ceiling_label(value), expected);
        }
    }

    #[test]
    fn r30_custom_pill_hit_offset_matches_drawn_span_for_both_display_modes() {
        let theme = super::super::theme::theme_by_slug(
            super::super::theme::default_theme_slug(),
        )
        .expect("default theme");
        for value in [DbNano(-100_000_000), DbNano(-1)] {
            for editing in [false, true] {
                let input = editing.then(|| {
                    super::super::text_input::TextInputState::new_selected(
                        true_peak_ceiling_label(value),
                    )
                });
                let line = pcm_true_peak_db_value_row(
                    theme.border,
                    120,
                    value,
                    input.as_ref(),
                    true,
                    false,
                    theme,
                );
                let pill_idx = line.spans.iter().position(|span| {
                    span.content.as_ref() == TRUE_PEAK_CUSTOM_PILL
                }).expect("clickable custom pill");
                let drawn_offset: usize = line.spans[..pill_idx]
                    .iter()
                    .map(Span::width)
                    .sum();
                assert_eq!(
                    true_peak_custom_pill_offset(value, editing, 120),
                    Some(drawn_offset as u16),
                );
                assert_eq!(line.spans[pill_idx].width(), TRUE_PEAK_CUSTOM_PILL.len());
            }
            assert_eq!(true_peak_custom_pill_offset(value, false, 34), None);
        }
    }

}

#[cfg(test)]
mod r32_true_peak_help_renderer_tests {
    use super::*;
    use tonepoet_pipeline::DbNano;

    #[test]
    fn r32_help_glyph_matches_registered_cell_offset_and_existing_affordance_contrast() {
        for palette in super::super::theme::palettes() {
            let theme = super::super::theme::theme_by_slug(palette.slug).expect("shipped theme");
            for focused in [false, true] {
                let row = pcm_true_peak_db_value_row(
                    theme.green,
                    120,
                    DbNano(-100_000_000),
                    None,
                    focused,
                    false,
                    theme,
                );
                let idx = row.spans.iter().position(|s| s.content.as_ref() == "\u{24d8} ")
                    .expect("true-peak info affordance");
                let x: usize = row.spans[..idx].iter().map(Span::width).sum();
                assert_eq!(
                    Some(x as u16),
                    true_peak_info_offset(DbNano(-100_000_000), false, 120),
                );
                let icon = &row.spans[idx];
                assert_eq!(icon.width(), 2);
                assert_eq!(icon.style.fg, Some(if theme.dark { theme.info } else { theme.value }));
                assert_eq!(icon.style.bg, None);
                assert!(icon.style.add_modifier.contains(Modifier::BOLD));
                assert!(
                    super::super::theme::contrast_ratio(
                        icon.style.fg.expect("information foreground"), theme.panel_bg,
                    ) >= 4.5,
                    "theme {} must show the glyph clearly",
                    palette.slug,
                );
            }
        }
    }

    #[test]
    fn r5_true_peak_help_follows_the_custom_pill_and_tracks_editor_width() {
        let theme = super::super::theme::theme_by_slug(
            super::super::theme::default_theme_slug(),
        ).expect("default theme");
        for value in [DbNano(-100_000_000), DbNano(-1)] {
            for editing in [false, true] {
                let input = editing.then(|| {
                    super::super::text_input::TextInputState::new_selected(
                        true_peak_ceiling_label(value),
                    )
                });
                let row = pcm_true_peak_db_value_row(
                    theme.green, 120, value, input.as_ref(), true, false, theme,
                );
                let custom_idx = row.spans.iter().position(|s| s.content.as_ref() == TRUE_PEAK_CUSTOM_PILL)
                    .expect("custom pill");
                let icon_idx = row.spans.iter().position(|s| s.content.as_ref() == "ⓘ ")
                    .expect("end-of-controls info glyph");
                assert!(icon_idx > custom_idx, "info glyph must follow custom pill");
                let drawn_offset: usize = row.spans[..icon_idx].iter().map(Span::width).sum();
                assert_eq!(true_peak_info_offset(value, editing, 120), Some(drawn_offset as u16));
            }
        }
        assert_eq!(true_peak_info_offset(DbNano(-100_000_000), false, 34), None);
    }

    #[test]
    fn r32_no_true_peak_row_advertises_set_and_lossy_warning_survives() {
        let theme = super::super::theme::theme_by_slug(
            super::super::theme::default_theme_slug(),
        ).expect("default theme");
        let value = DbNano(-100_000_000);
        let edit = super::super::text_input::TextInputState::new_selected("-0.10".to_string());
        for (editing, lossy_capped) in [(false, false), (false, true), (true, false)] {
            let line = pcm_true_peak_db_value_row(
                theme.green,
                120,
                value,
                editing.then_some(&edit),
                true,
                lossy_capped,
                theme,
            );
            let text: String = line.spans.iter().map(|span| span.content.as_ref()).collect();
            assert!(!text.contains(":set "), "{text}");
            assert!(!text.contains("0.25 dB steps"), "{text}");
            assert_eq!(text.contains("lossy encoder input capped"), lossy_capped && !editing);
            if editing {
                assert!(text.contains("Enter: apply"));
            }
        }
    }
}
