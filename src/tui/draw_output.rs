//! Format pane: dynamic PCM/DSD output settings pills (green border)

use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{FormatField, FormatState};
use super::button_map::{ButtonRenderMap, TuiButton};
use super::pill::{render_pill_spans, PillState};
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
            "DSD rate  ",
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
            "bit depth ",
            "1-bit",
            focused && format_state.field_focus == FormatField::BitDepth,
        ));
        lines.push(pill_row(
            border_color,
            w,
            "noise sh. ",
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
            "mod order ",
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
            "preset    ",
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
            "resampler ",
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
            "dither    ",
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
            "replaygain",
            "",
            &render_pill_spans(
                &format_state.replaygain,
                focused && format_state.field_focus == FormatField::ReplayGain,
            ),
            focused && format_state.field_focus == FormatField::ReplayGain,
        ));
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

/// Register all click targets for the dynamic format pane. This mirrors the
/// rendered 10-row layout above and is intended for `convert_screen::register_buttons`.
pub fn register_format_pane_buttons(
    buttons: &mut ButtonRenderMap,
    area: Rect,
    format_state: &FormatState,
) {
    let label_col = area.x + 17;
    register_pill_row(
        buttons,
        &format_state.format,
        area.y + 2,
        label_col,
        |i| TuiButton::FormatPill(i),
    );

    if format_state.is_dsd_selected() {
        register_pill_row(
            buttons,
            &format_state.sample_rate,
            area.y + 3,
            label_col,
            |i| TuiButton::RatePill(i),
        );
        register_pill_row(
            buttons,
            &format_state.noise_shaper,
            area.y + 5,
            label_col,
            |i| TuiButton::NoiseShaperPill(i),
        );
        register_pill_row(
            buttons,
            &format_state.modulator_order,
            area.y + 6,
            label_col,
            |i| TuiButton::ModulatorOrderPill(i),
        );
        register_pill_row(
            buttons,
            &format_state.conversion_preset,
            area.y + 7,
            label_col,
            |i| TuiButton::ConversionPresetPill(i),
        );
    } else {
        register_pill_row(
            buttons,
            &format_state.sample_rate,
            area.y + 3,
            label_col,
            |i| TuiButton::RatePill(i),
        );
        register_pill_row(
            buttons,
            &format_state.bit_depth,
            area.y + 4,
            label_col,
            |i| TuiButton::DepthPill(i),
        );
        register_pill_row(
            buttons,
            &format_state.resampler,
            area.y + 5,
            label_col,
            |i| TuiButton::ResamplerPill(i),
        );
        register_pill_row(
            buttons,
            &format_state.dither,
            area.y + 6,
            label_col,
            |i| TuiButton::DitherPill(i),
        );
        register_pill_row(
            buttons,
            &format_state.replaygain,
            area.y + 7,
            label_col,
            |i| TuiButton::ReplayGainPill(i),
        );
    }
}

fn register_pill_row<T: Clone>(
    buttons: &mut ButtonRenderMap,
    state: &PillState<T>,
    y: u16,
    start_x: u16,
    make_button: impl Fn(usize) -> TuiButton,
) {
    let mut x = start_x;
    for (i, opt) in state.options.iter().enumerate() {
        if i > 0 {
            x += 2;
        }
        let pill_width = opt.label.chars().count() as u16 + 2;
        if opt.enabled {
            buttons.record_button(make_button(i), Rect::new(x, y, pill_width, 1));
        }
        x += pill_width;
    }
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
