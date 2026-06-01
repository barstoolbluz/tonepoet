//! Format pane: dynamic PCM/DSD output settings pills (green border)

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{FormatField, FormatState};
use super::pill::render_pill_spans;
use super::theme;

/// Draw the format pane with green border.
///
/// Height stays at 10 rows:
///   0 top border
///   1 blank
///   2 format
///   3 rate
///   4 bit depth / static 1-bit
///   5 resampler / noise shaper
///   6 dither / modulator order
///   7 replaygain / conversion preset
///   8 blank
///   9 bottom border
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
    format_name: &str,
) -> Line<'a> {
    let label_style = if focused { theme::bright() } else { theme::muted() };
    let pill_text = format!(" {} settings ", format_name);
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
        Span::styled("   container   ", label_style),
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
