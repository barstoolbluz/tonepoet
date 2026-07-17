//! Format pane: dynamic PCM/DSD output settings pills (green border)

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{DsdGainMode, FormatField, FormatState, ResamplerChoice};
use super::pill::render_pill_spans;

/// Draw the format pane with green border.
///
/// Height is 10 rows for DSD targets, 9 rows for ordinary PCM targets, and 11
/// rows for DSD-to-PCM targets. DSD-to-PCM targets include both the gain mode
/// row and its manual dB value row so Manual mode is directly user-editable.
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
    let is_dsd = format_state.is_dsd_selected();

    let top_line = format_title_line(border_color, w, maximized, theme);

    let bot_line = Line::from(Span::styled(
        format!("└{}┘", "─".repeat(w.saturating_sub(2))),
        theme.border(border_color),
    ));

    let mut lines = vec![top_line];
    lines.push(bordered_line(border_color, w, vec![], theme));
    lines.push(pill_row(
        border_color,
        w,
        "format     ",
        "",
        &render_pill_spans(
            &format_state.format,
            focused && format_state.field_focus == FormatField::Format, theme),
        focused && format_state.field_focus == FormatField::Format, theme));

    if is_dsd {
        lines.push(pill_row(
            border_color,
            w,
            "DSD rate   ",
            "",
            &render_pill_spans(
                &format_state.sample_rate,
                focused && format_state.field_focus == FormatField::DsdRate, theme),
            focused && format_state.field_focus == FormatField::DsdRate, theme));
        lines.push(static_row(
            border_color,
            w,
            "bit depth  ",
            "1-bit",
            focused && format_state.field_focus == FormatField::BitDepth, theme));
        lines.push(pill_row(
            border_color,
            w,
            "noise sh.  ",
            "",
            &render_pill_spans(
                &format_state.noise_shaper,
                focused && format_state.field_focus == FormatField::NoiseShaper, theme),
            focused && format_state.field_focus == FormatField::NoiseShaper, theme));
        lines.push(pill_row(
            border_color,
            w,
            "mod order  ",
            "",
            &render_pill_spans(
                &format_state.modulator_order,
                focused && format_state.field_focus == FormatField::ModulatorOrder, theme),
            focused && format_state.field_focus == FormatField::ModulatorOrder, theme));
        lines.push(pill_row(
            border_color,
            w,
            "preset     ",
            "",
            &render_pill_spans(
                &format_state.conversion_preset,
                focused && format_state.field_focus == FormatField::ConversionPreset, theme),
            focused && format_state.field_focus == FormatField::ConversionPreset, theme));
    } else {
        lines.push(pill_row(
            border_color,
            w,
            "sample rate",
            "kHz",
            &render_pill_spans(
                &format_state.sample_rate,
                focused && format_state.field_focus == FormatField::SampleRate, theme),
            focused && format_state.field_focus == FormatField::SampleRate, theme));
        let bit_depth_focused = focused && format_state.field_focus == FormatField::BitDepth;
        if let Some(preset_spans) = lossy_preset_spans(format_state, bit_depth_focused, theme) {
            lines.push(pill_row(
                border_color,
                w,
                "preset     ",
                "",
                &preset_spans,
                bit_depth_focused,
                theme,
            ));
        } else {
            lines.push(pill_row(
                border_color,
                w,
                "bit depth  ",
                "bit",
                &render_pill_spans(&format_state.bit_depth, bit_depth_focused, theme),
                bit_depth_focused,
                theme,
            ));
        }
        lines.push(pill_row(
            border_color,
            w,
            "resampler  ",
            "",
            &render_pill_spans(
                &format_state.resampler,
                focused && format_state.field_focus == FormatField::Resampler, theme),
            focused && format_state.field_focus == FormatField::Resampler, theme));
        let ssrc_dither_override = format_state.ssrc_dither_override_active();
        let dither_focused = focused
            && format_state.field_focus == FormatField::Dither
            && !ssrc_dither_override;
        let rendered_dither = render_pill_spans(&format_state.dither, dither_focused, theme);
        let dither_spans = if ssrc_dither_override {
            dim_pill_spans(&rendered_dither, theme)
        } else {
            rendered_dither
        };
        lines.push(pill_row(
            border_color,
            w,
            "dither     ",
            format_state.ssrc_dither_status_label().unwrap_or(""),
            &dither_spans,
            dither_focused, theme));
        lines.push(pill_row(
            border_color,
            w,
            "replaygain ",
            "",
            &render_pill_spans(
                &format_state.replaygain,
                focused && format_state.field_focus == FormatField::ReplayGain, theme),
            focused && format_state.field_focus == FormatField::ReplayGain, theme));
        if format_state.dsd_to_pcm_gain_available() {
            lines.push(pill_row(
                border_color,
                w,
                "DSD gain  ",
                "",
                &render_pill_spans(
                    &format_state.dsd_gain_mode,
                    focused && format_state.field_focus == FormatField::DsdGain, theme),
                focused && format_state.field_focus == FormatField::DsdGain, theme));
            lines.push(dsd_gain_db_row(
                border_color,
                w,
                format_state.dsd_gain_db,
                *format_state.dsd_gain_mode.selected_value() == DsdGainMode::Manual,
                focused && format_state.field_focus == FormatField::DsdGainDb, theme));
        }
    }

    // Below-the-fold: container selector when maximized and codec has alternatives.
    let containers = format_state.format.selected_value().available_containers();
    let has_format_settings = matches!(
        *format_state.format.selected_value(),
        crate::convert::formats::AudioFormat::Flac
            | crate::convert::formats::AudioFormat::Aac
            | crate::convert::formats::AudioFormat::Opus
            | crate::convert::formats::AudioFormat::Mp3
            | crate::convert::formats::AudioFormat::WavPack
    );
    if maximized && containers.len() > 1 {
        lines.push(bordered_line(border_color, w, vec![], theme));
        let container_spans: Vec<Span> = containers
            .iter()
            .enumerate()
            .flat_map(|(i, c)| {
                let selected = i == format_state.selected_container_index;
                let style = if !c.enabled {
                    Style::default().fg(theme.text_dim)
                } else if selected {
                    Style::default().fg(theme.green).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text_dim)
                };
                let mut spans = vec![Span::styled(format!(" {} ", c.display_name), style)];
                if i + 1 < containers.len() {
                    spans.push(Span::styled(" ", Style::default()));
                }
                spans
            })
            .collect();
        if has_format_settings {
            let fmt_name = format_state.format.selected_value().name().to_lowercase();
            lines.push(container_row_with_settings_pill(
                border_color, w, &container_spans, focused, &fmt_name, theme));
        } else {
            lines.push(pill_row(
                border_color,
                w,
                "container ",
                "",
                &container_spans,
                focused, theme));
        }
    }

    // Below-the-fold: quality pill row when maximized and resampler is active.
    let resampler_active = maximized
        && !matches!(
            *format_state.resampler.selected_value(),
            ResamplerChoice::None
        );
    if resampler_active {
        lines.push(bordered_line(border_color, w, vec![], theme));
        let resample_label_style = if focused { theme.bright() } else { theme.muted() };
        lines.push(bordered_line(
            border_color,
            w,
            vec![Span::styled("   resampling", resample_label_style)], theme));
        use tonepoet_pipeline::enums::ResampleQuality;
        let mut quality_list: Vec<(ResampleQuality, &str)> = vec![
            (ResampleQuality::Low, "low"),
            (ResampleQuality::Medium, "med"),
            (ResampleQuality::High, "high"),
            (ResampleQuality::VeryHigh, "vhigh"),
            (ResampleQuality::Ultra, "ultra"),
        ];
        if matches!(*format_state.resampler.selected_value(), ResamplerChoice::Sox | ResamplerChoice::Ssrc) {
            quality_list.push((ResampleQuality::Insane, "insane"));
        }
        let quality_count = quality_list.len();
        let quality_pills: Vec<Span> = quality_list
            .iter()
            .enumerate()
            .flat_map(|(i, (q, label))| {
                let selected = *q == format_state.resample_quality;
                let style = if selected {
                    Style::default()
                        .fg(theme.pill_active_fg)
                        .bg(theme.green)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text_dim)
                };
                let mut spans = vec![Span::styled(format!(" {} ", label), style)];
                if i < quality_count - 1 {
                    spans.push(Span::styled(" ", Style::default()));
                }
                spans
            })
            .collect();

        let resampler_name = match *format_state.resampler.selected_value() {
            ResamplerChoice::Ssrc => Some("ssrc"),
            ResamplerChoice::Sox => Some("sox"),
            ResamplerChoice::Soxr => Some("soxr"),
            _ => None,
        };
        if let Some(name) = resampler_name {
            lines.push(row_with_settings_pill(
                border_color,
                w,
                "preset     ",
                &quality_pills,
                focused,
                name, theme));
        } else {
            lines.push(pill_row(
                border_color,
                w,
                "preset     ",
                "",
                &quality_pills,
                focused, theme));
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

fn static_row<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    label: &'a str,
    value: &'a str,
    focused: bool,
    theme: super::theme::Theme,
) -> Line<'a> {
    let label_style = if focused { theme.bright() } else { theme.muted() };
    let value_style = if focused { theme.bright() } else { theme.muted() };
    let mut spans = vec![
        Span::styled("│", theme.border(border_color)),
        Span::styled(format!("   {}  ", label), label_style),
        Span::styled(value, value_style),
    ];
    let content_width: usize = spans.iter().map(|s| s.width()).sum();
    let padding = width.saturating_sub(content_width + 1);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", theme.border(border_color)));
    Line::from(spans)
}

fn dsd_gain_db_row(
    border_color: ratatui::style::Color,
    width: usize,
    gain_db: f32,
    manual_enabled: bool,
    focused: bool,
    theme: super::theme::Theme,
) -> Line<'static> {
    let label_style = if focused { theme.bright() } else { theme.muted() };
    let control_style = if focused {
        theme.bright().add_modifier(Modifier::BOLD)
    } else if manual_enabled {
        theme.muted()
    } else {
        Style::default().fg(theme.text_dim)
    };
    let hint_style = if manual_enabled {
        theme.muted()
    } else {
        Style::default().fg(theme.text_dim)
    };

    let mut spans = vec![
        Span::styled("│", theme.border(border_color)),
        Span::styled("   gain dB    ", label_style),
        Span::styled("< ", control_style),
        Span::styled(format!("{gain_db:+.2} dB"), control_style),
        Span::styled(" >", control_style),
    ];
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        if manual_enabled { "left/right adjust" } else { "select manual to apply" },
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
        spans.push(Span::styled(suffix, theme.muted()));
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
