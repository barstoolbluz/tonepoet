//! Format pane: format/sample rate/bit depth/dither/replaygain pills (green border)

use ratatui::{
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{FormatField, FormatState};
use super::pill::render_pill_spans;
use super::theme;

/// Draw the format pane with green border
pub fn draw_format_pane(f: &mut Frame, area: Rect, format_state: &FormatState, focused: bool) {
    if area.height < 6 || area.width < 30 {
        return;
    }

    let border_color = if focused { theme::GREEN } else { theme::TEXT_DIM };
    let w = area.width as usize;

    // Top border with title
    let title = " format ";
    let adv_label = " advanced ";
    let dash_count = w.saturating_sub(2 + title.len() + adv_label.len() + 2);

    let top_line = Line::from(vec![
        Span::styled("┌", theme::border(border_color)),
        Span::styled(title, theme::border(border_color)),
        Span::styled("─".repeat(dash_count), theme::border(border_color)),
        Span::raw(" "),
        Span::styled("a", theme::muted()),
        Span::styled("dvanced", theme::border(border_color)),
        Span::styled(" ┐", theme::border(border_color)),
    ]);

    let bot_line = Line::from(Span::styled(
        format!("└{}┘", "─".repeat(w.saturating_sub(2))),
        theme::border(border_color),
    ));

    let is_format_focused = focused && format_state.field_focus == FormatField::Format;
    let is_rate_focused = focused && format_state.field_focus == FormatField::SampleRate;
    let is_depth_focused = focused && format_state.field_focus == FormatField::BitDepth;
    let is_dither_focused = focused && format_state.field_focus == FormatField::Dither;
    let is_rg_focused = focused && format_state.field_focus == FormatField::ReplayGain;

    let format_row = pill_row(
        border_color, w, "format     ", "",
        &render_pill_spans(&format_state.format, is_format_focused),
        is_format_focused,
    );

    let rate_row = pill_row(
        border_color, w, "sample rate", "kHz",
        &render_pill_spans(&format_state.sample_rate, is_rate_focused),
        is_rate_focused,
    );

    let depth_row = pill_row(
        border_color, w, "bit depth  ", "bit",
        &render_pill_spans(&format_state.bit_depth, is_depth_focused),
        is_depth_focused,
    );

    let dither_row = pill_row(
        border_color, w, "dither     ", "",
        &render_pill_spans(&format_state.dither, is_dither_focused),
        is_dither_focused,
    );

    let rg_row = pill_row(
        border_color, w, "replaygain ", "",
        &render_pill_spans(&format_state.replaygain, is_rg_focused),
        is_rg_focused,
    );

    let mut lines = vec![top_line];
    lines.push(bordered_line(border_color, w, vec![])); // blank
    lines.push(format_row);
    lines.push(bordered_line(border_color, w, vec![])); // blank after format
    lines.push(rate_row);
    lines.push(depth_row);
    lines.push(dither_row);
    lines.push(rg_row);
    lines.push(bordered_line(border_color, w, vec![])); // blank
    lines.push(bot_line);

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
}

/// Build a bordered line with a label, pill spans, and optional suffix
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

/// Create a line with │ content ... │ border
fn bordered_line<'a>(border_color: ratatui::style::Color, width: usize, content: Vec<Span<'a>>) -> Line<'a> {
    let content_width: usize = content.iter().map(|s| s.width()).sum();
    let padding = width.saturating_sub(2 + content_width);

    let mut spans = vec![Span::styled("│", theme::border(border_color))];
    spans.extend(content);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", theme::border(border_color)));
    Line::from(spans)
}
