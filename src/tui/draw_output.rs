//! Format pane: dynamic PCM/DSD output settings pills (green border)

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{FormatField, FormatState, ResamplerChoice};
use super::pill::render_pill_spans;

/// Draw the format pane with green border.
///
/// When native-v2 is the release default, DSD-to-PCM targets expose the P0
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
                    FormatField::DsdGain => lines.push(pill_row(
                        border_color,
                        w,
                        "DSD gain   ",
                        "",
                        &render_enabled_pill_spans(&format_state.dsd_gain_mode, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::DsdGainScope => lines.push(pill_row(
                        border_color,
                        w,
                        "gain scope ",
                        "",
                        &render_pill_spans(&format_state.dsd_auto_gain_scope, row_focused, theme),
                        row_focused,
                        theme,
                    )),
                    FormatField::DsdTruePeakScan => lines.push(pill_row(
                        border_color,
                        w,
                        "TP underread",
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
                    FormatField::DsdNormalizeTarget => {
                        let (label, value) = if format_state.dsd_reference_controls_available() {
                            ("normalize  ", format_state.dsd_normalize_target_dbfs)
                        } else {
                            ("auto margin", format_state.dsd_auto_gain_margin_db)
                        };
                        lines.push(dsd_db_value_row(
                            border_color,
                            w,
                            label,
                            value,
                            true,
                            row_focused,
                            "",
                            theme,
                        ));
                    }
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
