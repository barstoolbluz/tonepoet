//! Source pane: file path, format info, duration + browse pill (amber border)

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::SourceState;
use super::theme;

/// Label shown on the clickable "browse files" pill on the source pane.
pub const BROWSE_PILL_LABEL: &str = " browse files ";

/// Draw the source pane with amber border
pub fn draw_source_pane(f: &mut Frame, area: Rect, source: &SourceState, focused: bool) {
    if area.height < 4 || area.width < 30 {
        return;
    }

    let border_color = if focused { theme::AMBER } else { theme::TEXT_DIM };
    let w = area.width as usize;

    // Top border with title: ┌ source ─── advanced ┐
    let title = " source ";
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

    // Bottom border: └───┘
    let bot_line = Line::from(Span::styled(
        format!("└{}┘", "─".repeat(w.saturating_sub(2))),
        theme::border(border_color),
    ));

    // Content lines
    let content_lines = if let Some(info) = &source.info {
        let path_display = source
            .file_path
            .as_ref()
            .map(|p| {
                let s = p.display().to_string();
                // Shorten home directory
                if let Ok(home) = std::env::var("HOME") {
                    if s.starts_with(&home) {
                        return format!("~{}", &s[home.len()..]);
                    }
                }
                s
            })
            .unwrap_or_else(|| "—".to_string());

        let max_path = w.saturating_sub(16);
        let path_truncated = if path_display.chars().count() > max_path && max_path > 3 {
            let skip = path_display.chars().count() - (max_path - 3);
            let truncated: String = path_display.chars().skip(skip).collect();
            format!("...{}", truncated)
        } else {
            path_display
        };

        // Format info line: WAV │ PCM 24-bit │ 96.0 kHz │ stereo │ 847.3 MB
        let mut format_parts = vec![
            Span::styled("   format    ", theme::muted()),
            Span::styled(info.format_name.clone(), theme::bold(theme::BLUE)),
        ];
        if !info.codec.is_empty() {
            format_parts.push(Span::styled(" │ ", theme::muted()));
            format_parts.push(Span::styled(info.codec_display(), theme::text()));
        }
        if info.sample_rate > 0 {
            format_parts.push(Span::styled(" │ ", theme::muted()));
            format_parts.push(Span::styled(info.sample_rate_display(), theme::text()));
        }
        if info.channels > 0 {
            format_parts.push(Span::styled(" │ ", theme::muted()));
            format_parts.push(Span::styled(info.channels_display(), theme::text()));
        }
        if info.file_size > 0 {
            format_parts.push(Span::styled(" │ ", theme::muted()));
            format_parts.push(Span::styled(info.size_display(), theme::text()));
        }

        vec![
            bordered_line(border_color, w, vec![
                Span::styled("   path      ", theme::muted()),
                Span::styled(path_truncated, theme::bright()),
            ]),
            bordered_line(border_color, w, format_parts),
            bordered_line(border_color, w, vec![
                Span::styled("   duration  ", theme::muted()),
                Span::styled(info.duration_display(), theme::text()),
            ]),
            browse_pill_row(border_color, w),
        ]
    } else {
        // No file loaded
        vec![
            bordered_line(border_color, w, vec![]),
            bordered_line(border_color, w, vec![
                Span::styled(
                    "   press :browse or click the pill below to pick a source file",
                    theme::muted(),
                ),
            ]),
            bordered_line(border_color, w, vec![]),
            browse_pill_row(border_color, w),
        ]
    };

    let mut lines = vec![top_line];
    lines.extend(content_lines);
    lines.push(bot_line);

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
}

/// Render the "browse files" pill row, centered inside the source pane borders.
fn browse_pill_row(border_color: ratatui::style::Color, width: usize) -> Line<'static> {
    let pill_style = Style::default()
        .fg(theme::PILL_ACTIVE_FG)
        .bg(theme::PILL_ACTIVE_BG)
        .add_modifier(ratatui::style::Modifier::BOLD);

    let pill_w = BROWSE_PILL_LABEL.chars().count();
    let inner_w = width.saturating_sub(2);
    // Right-align the pill with a 3-space right margin.
    let right_margin = 3;
    let left_pad = inner_w.saturating_sub(pill_w + right_margin);

    Line::from(vec![
        Span::styled("│", theme::border(border_color)),
        Span::raw(" ".repeat(left_pad)),
        Span::styled(BROWSE_PILL_LABEL, pill_style),
        Span::raw(" ".repeat(right_margin)),
        Span::styled("│", theme::border(border_color)),
    ])
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
