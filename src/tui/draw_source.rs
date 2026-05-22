//! Source pane: file path, format info, duration + browse pill (amber border)

use std::path::Path;

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{SourceMode, SourceState};
use super::probe::SourceInfo;
use super::theme;
use crate::convert::formats::AudioFormat;

/// Label shown on the clickable "browse files" pill on the source pane.
pub const BROWSE_PILL_LABEL: &str = " browse files ";

/// Label shown on the clickable "expand" pill in Batch mode (opens the
/// BatchList overlay to view / manage the full file list).
pub const EXPAND_PILL_LABEL: &str = " edit batch ";

/// Label shown on the clickable "analyze" pill on the source pane.
pub const ANALYZE_PILL_LABEL: &str = " analyze ";

/// Compute the source pane height (border-inclusive) for the current mode.
///
/// Empty / Single: always 6 (top + 4 content + bottom).
/// Batch / MultiTrack: grows based on item count and terminal width, capped at 12.
pub fn source_pane_height(mode: &SourceMode, terminal_width: u16) -> u16 {
    const BASE: u16 = 6;
    const MAX: u16 = 12;

    let per_row: usize = if terminal_width >= 100 { 2 } else { 1 };

    match mode {
        SourceMode::Empty | SourceMode::Single { .. } => BASE,
        SourceMode::Batch { paths, .. } => {
            let n = paths.len();
            if n <= 1 {
                return BASE;
            }
            // Header: summary(1) + formats(1). Pill: expand(1).
            let header: u16 = 2;
            let visible = n.min(10);
            let track_rows = ((visible + per_row - 1) / per_row) as u16;
            let overflow: u16 = if n > 10 { 1 } else { 0 };
            let pill: u16 = 1;
            let total = header + track_rows + overflow + pill + 2;
            total.min(MAX)
        }
        SourceMode::MultiTrack {
            tracks,
            album_title,
            ..
        } => {
            let n = tracks.len();
            if n == 0 {
                return BASE;
            }
            // Header: filename(1) + album info(1 if present) + track count(1)
            let header: u16 = if album_title.is_some() { 3 } else { 2 };
            let visible = n.min(10);
            let track_rows = ((visible + per_row - 1) / per_row) as u16;
            let overflow: u16 = if n > 10 { 1 } else { 0 };
            let pill: u16 = 1;
            let total = header + track_rows + overflow + pill + 2;
            total.min(MAX)
        }
    }
}

/// Draw the source pane with amber border. Dispatches to the right
/// renderer based on `source.mode`:
/// - `Empty` → placeholder "press :browse..."
/// - `Single { .. }` → rich single-file layout (path, format, duration)
/// - `Batch { .. }` → summary + inline list + `[expand]` pill
pub fn draw_source_pane(f: &mut Frame, area: Rect, source: &SourceState, focused: bool) {
    if area.height < 4 || area.width < 30 {
        return;
    }

    let border_color = if focused {
        theme::AMBER
    } else {
        theme::TEXT_DIM
    };
    let w = area.width as usize;

    // Top border with title: ┌ source ─── advanced ┐
    let title = match source.mode {
        SourceMode::Batch { .. } => " source (batch) ",
        SourceMode::MultiTrack { .. } => " source (tracks) ",
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
        SourceMode::MultiTrack {
            path,
            tracks,
            area_label,
            album_title,
            album_artist,
            scroll,
            cursor,
            selected,
            ..
        } => render_multi_track(
            border_color,
            w,
            path,
            tracks,
            area_label.as_deref(),
            album_title.as_deref(),
            album_artist.as_deref(),
            *scroll,
            *cursor,
            selected,
            area.height,
        ),
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
            area.height,
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
        bordered_line(
            border_color,
            w,
            vec![Span::styled(
                "   press :browse or click the pill below to pick a source file",
                theme::muted(),
            )],
        ),
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
            bordered_line(
                border_color,
                w,
                vec![
                    Span::styled("   path      ", theme::muted()),
                    Span::styled(shorten_path(path, w.saturating_sub(16)), theme::bright()),
                ],
            ),
            bordered_line(
                border_color,
                w,
                vec![Span::styled("   probing…", theme::muted())],
            ),
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
        bordered_line(
            border_color,
            w,
            vec![
                Span::styled("   path      ", theme::muted()),
                Span::styled(path_truncated, theme::bright()),
            ],
        ),
        bordered_line(border_color, w, format_parts),
        bordered_line(
            border_color,
            w,
            vec![
                Span::styled("   duration  ", theme::muted()),
                Span::styled(info.duration_display(), theme::text()),
            ],
        ),
        two_pill_row(border_color, w, BROWSE_PILL_LABEL),
    ]
}

/// Render the multi-file batch content: summary + inline file list + pill.
/// `pane_height` is the total allocated pane height including borders.
#[allow(clippy::too_many_arguments)]
fn render_batch<'a>(
    border_color: ratatui::style::Color,
    w: usize,
    paths: &[std::path::PathBuf],
    cursor: usize,
    _cursor_info: Option<&SourceInfo>,
    total_size: u64,
    album_count: usize,
    format_histogram: &[(AudioFormat, usize)],
    pane_height: u16,
) -> Vec<Line<'a>> {
    let n = paths.len();
    let mut lines = Vec::new();

    // Line 1: "batch: 5 files · 2 albums · 892 MB"
    let summary_line = format!(
        "{} files · {} album{} · {}",
        n,
        album_count,
        if album_count == 1 { "" } else { "s" },
        format_size(total_size),
    );
    lines.push(bordered_line(
        border_color,
        w,
        vec![
            Span::styled("   batch     ", theme::muted()),
            Span::styled(summary_line, theme::bold(theme::BLUE)),
        ],
    ));

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
    lines.push(bordered_line(
        border_color,
        w,
        vec![
            Span::styled("   formats   ", theme::muted()),
            Span::styled(hist_str, theme::text()),
        ],
    ));

    // Inline file list (same horizontal layout as multi-track).
    let header_rows: u16 = 2; // summary + formats
    let tracks_per_row: usize = if w >= 100 { 2 } else { 1 };
    let mut track_area = pane_height.saturating_sub(2 + header_rows + 1) as usize; // -borders -header -pill
    let tentative_max = track_area * tracks_per_row;
    if n > tentative_max {
        track_area = track_area.saturating_sub(1); // reserve for overflow row
    }
    let max_visible = track_area * tracks_per_row;
    let end = n.min(max_visible);
    let col_width = if tracks_per_row == 2 {
        w.saturating_sub(4) / 2
    } else {
        w.saturating_sub(4)
    };

    // Column-first layout: items 1..N in left column, N+1..2N in right.
    let num_rows = (end + tracks_per_row - 1) / tracks_per_row;
    for row in 0..num_rows {
        let mut row_spans = Vec::new();
        for col in 0..tracks_per_row {
            let abs = row + col * num_rows;
            if abs >= end {
                break;
            }
            let filename = paths
                .get(abs)
                .map(|p| {
                    p.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                })
                .unwrap_or_default();
            let fg = if abs == cursor {
                Color::Cyan
            } else {
                Color::White
            };
            let truncated = truncate_to(&filename, col_width.saturating_sub(4));
            let padded = format!("   {:<width$}", truncated, width = col_width.saturating_sub(3));
            row_spans.push(Span::styled(padded, Style::default().fg(fg)));
        }
        lines.push(bordered_line(border_color, w, row_spans));
    }

    if end < n {
        lines.push(bordered_line(
            border_color,
            w,
            vec![Span::styled(
                format!("   ... and {} more", n - end),
                Style::default().fg(Color::DarkGray),
            )],
        ));
    }

    lines.push(two_pill_row(border_color, w, EXPAND_PILL_LABEL));

    lines
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

/// Render the "browse files" pill row, right-aligned inside the source pane.
fn browse_pill_row(border_color: ratatui::style::Color, width: usize) -> Line<'static> {
    pill_row(border_color, width, BROWSE_PILL_LABEL)
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
    spans.push(Span::styled("│", theme::border(border_color)));
    Line::from(spans)
}

/// Render multi-track source (SACD ISO or CUE+image).
/// `pane_height` is the total allocated pane height including borders.
#[allow(clippy::too_many_arguments)]
fn render_multi_track<'a>(
    border_color: ratatui::style::Color,
    w: usize,
    path: &Path,
    tracks: &[super::app::MultiTrackEntry],
    area_label: Option<&str>,
    album_title: Option<&str>,
    album_artist: Option<&str>,
    scroll: usize,
    cursor: usize,
    selected: &[bool],
    pane_height: u16,
) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    let name = path.file_name().unwrap_or_default().to_string_lossy();

    // Header: filename + area label
    let mut header_spans = vec![
        Span::styled("   ", Style::default()),
        Span::styled(
            truncate_to(&name, w.saturating_sub(6)),
            Style::default().fg(Color::White),
        ),
    ];
    if let Some(area) = area_label {
        header_spans.push(Span::styled(
            format!("  [{}]", area),
            Style::default().fg(Color::Cyan),
        ));
    }
    lines.push(bordered_line(border_color, w, header_spans));

    // Album info
    if let Some(title) = album_title {
        let mut info = format!("   {}", title);
        if let Some(artist) = album_artist {
            info = format!("   {} — {}", artist, title);
        }
        lines.push(bordered_line(
            border_color,
            w,
            vec![Span::styled(
                truncate_to(&info, w.saturating_sub(4)),
                Style::default().fg(Color::DarkGray),
            )],
        ));
    }

    // Track count
    lines.push(bordered_line(
        border_color,
        w,
        vec![Span::styled(
            format!("   {} tracks", tracks.len()),
            Style::default().fg(Color::Gray),
        )],
    ));

    // Derive max visible tracks from pane height.
    // pane_height = borders(2) + header_rows + track_rows + pill(1) [+ overflow(1)]
    let header_rows: u16 = if album_title.is_some() { 3 } else { 2 };
    let tracks_per_row: usize = if w >= 100 { 2 } else { 1 };
    let mut track_area = pane_height.saturating_sub(2 + header_rows + 1) as usize; // -borders -header -pill
    // If there will be an overflow indicator, reserve a row for it.
    let tentative_max = track_area * tracks_per_row;
    if tracks.len() > scroll + tentative_max {
        track_area = track_area.saturating_sub(1);
    }
    let max_visible = track_area * tracks_per_row;

    // Track listing (scrollable) with selection checkboxes.
    // Two tracks per row on wide terminals, one on narrow.
    let end = tracks.len().min(scroll + max_visible);
    let col_width = if tracks_per_row == 2 {
        w.saturating_sub(4) / 2
    } else {
        w.saturating_sub(4)
    };

    // Column-first layout: tracks 1..N in left column, N+1..2N in right.
    let visible_count = end - scroll;
    let num_rows = (visible_count + tracks_per_row - 1) / tracks_per_row;
    for row in 0..num_rows {
        let mut row_spans = Vec::new();
        for col in 0..tracks_per_row {
            let abs = scroll + row + col * num_rows;
            if abs >= end {
                break;
            }
            let t = &tracks[abs];
            let checked = selected.get(abs).copied().unwrap_or(true);
            let check = if checked { "[x]" } else { "[ ]" };
            let title_str = t.title.as_deref().unwrap_or("—");
            let dur_str = t.duration_display.as_deref().unwrap_or("");
            let entry = if dur_str.is_empty() {
                format!("{} {:2}. {}", check, t.number, title_str)
            } else {
                format!("{} {:2}. {} [{}]", check, t.number, title_str, dur_str)
            };
            let fg = if abs == cursor {
                Color::Cyan
            } else if checked {
                Color::White
            } else {
                Color::DarkGray
            };
            let truncated = truncate_to(&entry, col_width.saturating_sub(1));
            let padded = format!("   {:<width$}", truncated, width = col_width.saturating_sub(3));
            row_spans.push(Span::styled(padded, Style::default().fg(fg)));
        }
        lines.push(bordered_line(border_color, w, row_spans));
    }

    if end < tracks.len() {
        lines.push(bordered_line(
            border_color,
            w,
            vec![Span::styled(
                format!("   ... and {} more", tracks.len() - end),
                Style::default().fg(Color::DarkGray),
            )],
        ));
    }

    // Expand + analyze pill row
    lines.push(two_pill_row(border_color, w, EXPAND_PILL_LABEL));

    lines
}

fn truncate_to(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if max > 3 {
        format!("{}...", &s[..max - 3])
    } else {
        s[..max].to_string()
    }
}
