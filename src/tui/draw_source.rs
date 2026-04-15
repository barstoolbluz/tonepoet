//! Source pane: file path, format info, duration + browse pill (amber border)

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::convert::formats::AudioFormat;
use super::app::{SourceMode, SourceState};
use super::probe::SourceInfo;
use super::theme;

/// Label shown on the clickable "browse files" pill on the source pane.
pub const BROWSE_PILL_LABEL: &str = " browse files ";

/// Label shown on the clickable "expand" pill in Batch mode (opens the
/// BatchList overlay to view / manage the full file list).
pub const EXPAND_PILL_LABEL: &str = " expand ";

/// Label shown on the clickable "analyze" pill on the source pane.
pub const ANALYZE_PILL_LABEL: &str = " analyze ";

/// Draw the source pane with amber border. Dispatches to the right
/// renderer based on `source.mode`:
/// - `Empty` → placeholder "press :browse..."
/// - `Single { .. }` → rich single-file layout (path, format, duration)
/// - `Batch { .. }` → summary + inline list + `[expand]` pill
pub fn draw_source_pane(f: &mut Frame, area: Rect, source: &SourceState, focused: bool) {
    if area.height < 4 || area.width < 30 {
        return;
    }

    let border_color = if focused { theme::AMBER } else { theme::TEXT_DIM };
    let w = area.width as usize;

    // Top border with title: ┌ source ─── advanced ┐
    let title = match source.mode {
        SourceMode::Batch { .. } => " source (batch) ",
        _ => " source ",
    };
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

    let content_lines = match &source.mode {
        SourceMode::Empty => render_empty(border_color, w),
        SourceMode::Single { path, info, .. } => {
            render_single(border_color, w, path, info.as_ref())
        }
        SourceMode::Batch {
            paths,
            cursor,
            cursor_info,
            total_size,
            album_count,
            format_histogram,
            ..
        } => render_batch(
            border_color,
            w,
            paths,
            *cursor,
            cursor_info.as_ref(),
            *total_size,
            *album_count,
            format_histogram,
        ),
    };

    let mut lines = vec![top_line];
    lines.extend(content_lines);
    lines.push(bot_line);

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
}

/// Render the Empty placeholder content.
fn render_empty<'a>(border_color: ratatui::style::Color, w: usize) -> Vec<Line<'a>> {
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
}

/// Render the single-file content (path, format info, duration, browse pill).
fn render_single<'a>(
    border_color: ratatui::style::Color,
    w: usize,
    path: &std::path::Path,
    info: Option<&SourceInfo>,
) -> Vec<Line<'a>> {
    let Some(info) = info else {
        // Path is known but probe hasn't completed yet — show a minimal
        // placeholder so the layout doesn't collapse.
        return vec![
            bordered_line(border_color, w, vec![
                Span::styled("   path      ", theme::muted()),
                Span::styled(shorten_path(path, w.saturating_sub(16)), theme::bright()),
            ]),
            bordered_line(border_color, w, vec![
                Span::styled("   probing…", theme::muted()),
            ]),
            bordered_line(border_color, w, vec![]),
            browse_pill_row(border_color, w),
        ];
    };

    let path_truncated = shorten_path(path, w.saturating_sub(16));

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
        two_pill_row(border_color, w, BROWSE_PILL_LABEL),
    ]
}

/// Render the multi-file batch content: summary line + format histogram
/// + inline file list + [expand] pill.
#[allow(clippy::too_many_arguments)]
fn render_batch<'a>(
    border_color: ratatui::style::Color,
    w: usize,
    paths: &[std::path::PathBuf],
    cursor: usize,
    cursor_info: Option<&SourceInfo>,
    total_size: u64,
    album_count: usize,
    format_histogram: &[(AudioFormat, usize)],
) -> Vec<Line<'a>> {
    let n = paths.len();

    // Line 1: "batch: 5 files · 2 albums · 892 MB"
    let summary_line = format!(
        "{} files · {} album{} · {}",
        n,
        album_count,
        if album_count == 1 { "" } else { "s" },
        format_size(total_size),
    );

    // Line 2: format histogram, e.g. "FLAC (3) · WAV (2)"
    let hist_str = if format_histogram.is_empty() {
        "(no recognised audio extensions)".to_string()
    } else {
        format_histogram
            .iter()
            .map(|(f, c)| format!("{} ({})", f.name(), c))
            .collect::<Vec<_>>()
            .join(" · ")
    };

    // Line 3: inline file list with [1/N] cursor highlight — first file
    // gets the cursor by default. Shows the cursor file info if probed.
    let cursor_info_str = cursor_info
        .map(|i| {
            let mut parts = Vec::new();
            if !i.codec.is_empty() {
                parts.push(i.codec_display());
            }
            if i.sample_rate > 0 {
                parts.push(i.sample_rate_display());
            }
            parts.join(" · ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "probing…".to_string());

    let cursor_file = paths
        .get(cursor)
        .map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned())
        .unwrap_or_default();

    let cursor_line = format!("[{}/{}] {} — {}", cursor + 1, n, cursor_file, cursor_info_str);
    let cursor_line_trunc = truncate_display(&cursor_line, w.saturating_sub(16));

    vec![
        bordered_line(border_color, w, vec![
            Span::styled("   batch     ", theme::muted()),
            Span::styled(summary_line, theme::bold(theme::BLUE)),
        ]),
        bordered_line(border_color, w, vec![
            Span::styled("   formats   ", theme::muted()),
            Span::styled(hist_str, theme::text()),
        ]),
        bordered_line(border_color, w, vec![
            Span::styled("   preview   ", theme::muted()),
            Span::styled(cursor_line_trunc, theme::bright()),
        ]),
        expand_pill_row(border_color, w),
    ]
}

/// Format a byte count as human-readable (e.g., "892.3 MB").
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;
    if bytes >= TB {
        format!("{:.1} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Shorten a path for display: replace $HOME with ~ and left-truncate
/// with "..." if it's longer than `max_chars`.
fn shorten_path(path: &std::path::Path, max_chars: usize) -> String {
    let s = path.display().to_string();
    let s = if let Ok(home) = std::env::var("HOME") {
        if s.starts_with(&home) {
            format!("~{}", &s[home.len()..])
        } else {
            s
        }
    } else {
        s
    };
    if s.chars().count() > max_chars && max_chars > 3 {
        let skip = s.chars().count() - (max_chars - 3);
        let trunc: String = s.chars().skip(skip).collect();
        format!("...{}", trunc)
    } else {
        s
    }
}

/// Left-truncate a display string with "…" if longer than `max_chars`.
fn truncate_display(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars || max_chars < 2 {
        return s.to_string();
    }
    let trunc: String = s.chars().take(max_chars - 1).collect();
    format!("{}…", trunc)
}

/// Render the "browse files" pill row, right-aligned inside the source pane.
fn browse_pill_row(border_color: ratatui::style::Color, width: usize) -> Line<'static> {
    pill_row(border_color, width, BROWSE_PILL_LABEL)
}

/// Render the "expand" pill row for Batch mode, right-aligned inside the
/// source pane. Clicking / activating opens the BatchList overlay.
fn expand_pill_row(border_color: ratatui::style::Color, width: usize) -> Line<'static> {
    pill_row(border_color, width, EXPAND_PILL_LABEL)
}

/// Row with two pills: a primary pill (right-aligned) and an analyze pill
/// to its left. Used when a file is loaded (Single/Batch).
fn two_pill_row(
    border_color: ratatui::style::Color,
    width: usize,
    primary_label: &'static str,
) -> Line<'static> {
    let pill_style = Style::default()
        .fg(theme::PILL_ACTIVE_FG)
        .bg(theme::PILL_ACTIVE_BG)
        .add_modifier(ratatui::style::Modifier::BOLD);
    let analyze_style = Style::default()
        .fg(theme::PILL_ACTIVE_FG)
        .bg(theme::PURPLE)
        .add_modifier(ratatui::style::Modifier::BOLD);

    let primary_w = primary_label.chars().count();
    let analyze_w = ANALYZE_PILL_LABEL.chars().count();
    let gap = 2;
    let right_margin = 3;
    let total_pills = analyze_w + gap + primary_w;
    let inner_w = width.saturating_sub(2);
    let left_pad = inner_w.saturating_sub(total_pills + right_margin);

    Line::from(vec![
        Span::styled("│", theme::border(border_color)),
        Span::raw(" ".repeat(left_pad)),
        Span::styled(ANALYZE_PILL_LABEL, analyze_style),
        Span::raw(" ".repeat(gap)),
        Span::styled(primary_label, pill_style),
        Span::raw(" ".repeat(right_margin)),
        Span::styled("│", theme::border(border_color)),
    ])
}

/// Shared pill-row renderer: right-aligned pill with a small margin.
fn pill_row(
    border_color: ratatui::style::Color,
    width: usize,
    label: &'static str,
) -> Line<'static> {
    let pill_style = Style::default()
        .fg(theme::PILL_ACTIVE_FG)
        .bg(theme::PILL_ACTIVE_BG)
        .add_modifier(ratatui::style::Modifier::BOLD);

    let pill_w = label.chars().count();
    let inner_w = width.saturating_sub(2);
    let right_margin = 3;
    let left_pad = inner_w.saturating_sub(pill_w + right_margin);

    Line::from(vec![
        Span::styled("│", theme::border(border_color)),
        Span::raw(" ".repeat(left_pad)),
        Span::styled(label, pill_style),
        Span::raw(" ".repeat(right_margin)),
        Span::styled("│", theme::border(border_color)),
    ])
}

/// Create a line with │ content ... │ border
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
    spans.push(Span::styled(
        "│",
        theme::border(border_color),
    ));
    Line::from(spans)
}
