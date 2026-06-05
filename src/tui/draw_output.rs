//! Format pane: dynamic PCM/DSD output settings pills (green border)

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{FormatField, FormatState, ResamplerChoice, DsdGainMode};
use super::pill::render_pill_spans;
use super::theme;

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
) {
    if area.height < 6 || area.width < 30 {
        return;
    }

    let border_color = if focused { theme::GREEN } else { theme::TEXT_DIM };
    let w = area.width as usize;
    let is_dsd = format_state.is_dsd_selected();

    let top_line = format_title_line(border_color, w, maximized);

    let bot_line = Line::from(Span::styled(
        format!("└{}┘", "─".repeat(w.saturating_sub(2))),
        theme::border(border_color),
    ));

    let mut lines = vec![top_line];
    lines.push(bordered_line(border_color, w, vec![]));
    lines.push(pill_row(
        border_color,
        w,
        "format     ",
        "",
        &render_pill_spans(
            &format_state.format,
            focused && format_state.field_focus == FormatField::Format,
        ),
        focused && format_state.field_focus == FormatField::Format,
    ));

    if is_dsd {
        lines.push(pill_row(
            border_color,
            w,
            "DSD rate   ",
            "",
            &render_pill_spans(
                &format_state.sample_rate,
                focused && format_state.field_focus == FormatField::DsdRate,
            ),
            focused && format_state.field_focus == FormatField::DsdRate,
        ));
        lines.push(static_row(
            border_color,
            w,
            "bit depth  ",
            "1-bit",
            focused && format_state.field_focus == FormatField::BitDepth,
        ));
        lines.push(pill_row(
            border_color,
            w,
            "noise sh.  ",
            "",
            &render_pill_spans(
                &format_state.noise_shaper,
                focused && format_state.field_focus == FormatField::NoiseShaper,
            ),
            focused && format_state.field_focus == FormatField::NoiseShaper,
        ));
        lines.push(pill_row(
            border_color,
            w,
            "mod order  ",
            "",
            &render_pill_spans(
                &format_state.modulator_order,
                focused && format_state.field_focus == FormatField::ModulatorOrder,
            ),
            focused && format_state.field_focus == FormatField::ModulatorOrder,
        ));
        lines.push(pill_row(
            border_color,
            w,
            "preset     ",
            "",
            &render_pill_spans(
                &format_state.conversion_preset,
                focused && format_state.field_focus == FormatField::ConversionPreset,
            ),
            focused && format_state.field_focus == FormatField::ConversionPreset,
        ));
    } else {
        lines.push(pill_row(
            border_color,
            w,
            "sample rate",
            "kHz",
            &render_pill_spans(
                &format_state.sample_rate,
                focused && format_state.field_focus == FormatField::SampleRate,
            ),
            focused && format_state.field_focus == FormatField::SampleRate,
        ));
        lines.push(pill_row(
            border_color,
            w,
            "bit depth  ",
            "bit",
            &render_pill_spans(
                &format_state.bit_depth,
                focused && format_state.field_focus == FormatField::BitDepth,
            ),
            focused && format_state.field_focus == FormatField::BitDepth,
        ));
        lines.push(pill_row(
            border_color,
            w,
            "resampler  ",
            "",
            &render_pill_spans(
                &format_state.resampler,
                focused && format_state.field_focus == FormatField::Resampler,
            ),
            focused && format_state.field_focus == FormatField::Resampler,
        ));
        lines.push(pill_row(
            border_color,
            w,
            "dither     ",
            "",
            &render_pill_spans(
                &format_state.dither,
                focused && format_state.field_focus == FormatField::Dither,
            ),
            focused && format_state.field_focus == FormatField::Dither,
        ));
        lines.push(pill_row(
            border_color,
            w,
            "replaygain ",
            "",
            &render_pill_spans(
                &format_state.replaygain,
                focused && format_state.field_focus == FormatField::ReplayGain,
            ),
            focused && format_state.field_focus == FormatField::ReplayGain,
        ));
        if format_state.dsd_to_pcm_gain_available() {
            lines.push(pill_row(
                border_color,
                w,
                "DSD gain  ",
                "",
                &render_pill_spans(
                    &format_state.dsd_gain_mode,
                    focused && format_state.field_focus == FormatField::DsdGain,
                ),
                focused && format_state.field_focus == FormatField::DsdGain,
            ));
            lines.push(dsd_gain_db_row(
                border_color,
                w,
                format_state.dsd_gain_db,
                *format_state.dsd_gain_mode.selected_value() == DsdGainMode::Manual,
                focused && format_state.field_focus == FormatField::DsdGainDb,
            ));
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
        lines.push(bordered_line(border_color, w, vec![]));
        let container_spans: Vec<Span> = containers
            .iter()
            .enumerate()
            .flat_map(|(i, c)| {
                let selected = i == format_state.selected_container_index;
                let style = if !c.enabled {
                    Style::default().fg(ratatui::style::Color::DarkGray)
                } else if selected {
                    Style::default().fg(theme::GREEN).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::TEXT_DIM)
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
                border_color, w, &container_spans, focused, &fmt_name,
            ));
        } else {
            lines.push(pill_row(
                border_color,
                w,
                "container ",
                "",
                &container_spans,
                focused,
            ));
        }
    }

    // Below-the-fold: quality pill row when maximized and resampler is active.
    let resampler_active = maximized
        && !matches!(
            *format_state.resampler.selected_value(),
            ResamplerChoice::None
        );
    if resampler_active {
        lines.push(bordered_line(border_color, w, vec![]));
        let resample_label_style = if focused { theme::bright() } else { theme::muted() };
        lines.push(bordered_line(
            border_color,
            w,
            vec![Span::styled("   resampling", resample_label_style)],
        ));
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
                        .fg(theme::PILL_ACTIVE_FG)
                        .bg(theme::GREEN)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::TEXT_DIM)
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
                name,
            ));
        } else {
            lines.push(pill_row(
                border_color,
                w,
                "preset     ",
                "",
                &quality_pills,
                focused,
            ));
        }
    }

    lines.push(bordered_line(border_color, w, vec![]));
    let target_len_before_bottom = area.height.saturating_sub(1) as usize;
    while lines.len() < target_len_before_bottom {
        lines.push(bordered_line(border_color, w, vec![]));
    }
    lines.push(bot_line);

    f.render_widget(Paragraph::new(lines), area);
}


/// Draw the collapsed format title bar.
pub fn draw_format_title_bar(f: &mut Frame, area: Rect, focused: bool) {
    if area.height < 1 || area.width < 12 {
        return;
    }
    let border_color = if focused { theme::GREEN } else { theme::TEXT_DIM };
    f.render_widget(
        Paragraph::new(vec![format_title_line(border_color, area.width as usize, false)]),
        area,
    );
}

fn format_title_line<'a>(border_color: ratatui::style::Color, width: usize, maximized: bool) -> Line<'a> {
    let title = " format ";
    let indicator = if maximized { "◼" } else { "◻" };
    let left_spans = vec![
        Span::styled("╒ ", theme::border(border_color)),
        Span::styled(indicator, theme::border(border_color)),
        Span::styled(title, theme::border(border_color)),
    ];
    let right_spans = vec![
        Span::styled("a", theme::muted()),
        Span::styled("dvanced", theme::border(border_color)),
        Span::styled(" ╕", theme::border(border_color)),
    ];
    let fixed_width = Line::from(left_spans.clone()).width()
        + Line::from(right_spans.clone()).width();
    let fill_count = width.saturating_sub(fixed_width);
    let mut spans = left_spans;
    spans.push(Span::styled(
        "═".repeat(fill_count),
        theme::border(border_color),
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
) -> Line<'a> {
    let label_style = if focused { theme::bright() } else { theme::muted() };
    let value_style = if focused { theme::bright() } else { theme::muted() };
    let mut spans = vec![
        Span::styled("│", theme::border(border_color)),
        Span::styled(format!("   {}  ", label), label_style),
        Span::styled(value, value_style),
    ];
    let content_width: usize = spans.iter().map(|s| s.width()).sum();
    let padding = width.saturating_sub(content_width + 1);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", theme::border(border_color)));
    Line::from(spans)
}

fn dsd_gain_db_row(
    border_color: ratatui::style::Color,
    width: usize,
    gain_db: f32,
    manual_enabled: bool,
    focused: bool,
) -> Line<'static> {
    let label_style = if focused { theme::bright() } else { theme::muted() };
    let control_style = if focused {
        theme::bright().add_modifier(Modifier::BOLD)
    } else if manual_enabled {
        theme::muted()
    } else {
        Style::default().fg(ratatui::style::Color::DarkGray)
    };
    let hint_style = if manual_enabled {
        theme::muted()
    } else {
        Style::default().fg(ratatui::style::Color::DarkGray)
    };

    let mut spans = vec![
        Span::styled("│", theme::border(border_color)),
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
    spans.push(Span::styled("│", theme::border(border_color)));

    Line::from(spans)
}

fn pill_row<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    label: &'a str,
    suffix: &'a str,
    pills: &[Span<'a>],
    focused: bool,
) -> Line<'a> {
    let label_style = if focused { theme::bright() } else { theme::muted() };

    let mut spans = vec![
        Span::styled("│", theme::border(border_color)),
        Span::styled(format!("   {}  ", label), label_style),
    ];
    spans.extend_from_slice(pills);

    if !suffix.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(suffix, theme::muted()));
    }

    let content_width: usize = spans.iter().map(|s| s.width()).sum();
    let padding = width.saturating_sub(content_width + 1);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", theme::border(border_color)));

    Line::from(spans)
}

fn bordered_line<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    content: Vec<Span<'a>>,
) -> Line<'a> {
    let content_width: usize = content.iter().map(|s| s.width()).sum();
    let padding = width.saturating_sub(2 + content_width);

    let mut spans = vec![Span::styled("│", theme::border(border_color))];
    spans.extend(content);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", theme::border(border_color)));
    Line::from(spans)
}

/// Container row with a right-aligned `<format> settings` pill.
fn container_row_with_settings_pill<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    pills: &[Span<'a>],
    focused: bool,
    settings_name: &str,
) -> Line<'a> {
    row_with_settings_pill(border_color, width, "container  ", pills, focused, settings_name)
}

/// Generic row with left-aligned pills and a right-aligned settings pill.
fn row_with_settings_pill<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    label: &'a str,
    pills: &[Span<'a>],
    focused: bool,
    settings_name: &str,
) -> Line<'a> {
    let label_style = if focused { theme::bright() } else { theme::muted() };
    let pill_text = format!(" {} settings ", settings_name);
    let pill_width = pill_text.len();
    let settings_pill = Span::styled(
        pill_text,
        Style::default()
            .fg(theme::PILL_ACTIVE_FG)
            .bg(theme::PURPLE)
            .add_modifier(Modifier::BOLD),
    );

    let mut spans = vec![
        Span::styled("│", theme::border(border_color)),
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
    spans.push(Span::styled("│", theme::border(border_color)));
    Line::from(spans)
}
