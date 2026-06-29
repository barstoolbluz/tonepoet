//! Source pane: file path, format info, duration + browse pill (amber border)

use std::path::Path;

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{SourceMode, SourceState};
use super::probe::SourceInfo;
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
        SourceMode::Batch {
            paths,
            cursor,
            probe_notice,
            cursor_probe_notice,
            ..
        } => {
            let n = paths.len();
            if n <= 1 {
                return BASE;
            }
            // Header: summary(1) + selected cursor preview(1) + optional
            // persistent warning(1) + formats(1). Pill: expand(1).
            let effective_notice = effective_batch_probe_notice(
                paths,
                *cursor,
                probe_notice.as_deref(),
                cursor_probe_notice.as_ref(),
            );
            let header: u16 = 3 + if effective_notice.is_some() { 1 } else { 0 };
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
            info,
            probe_notice,
            ..
        } => {
            let n = tracks.len();
            if n == 0 {
                return BASE;
            }
            // Header: filename(1) + album info(1 if present) + source
            // properties/notice(1 if present) + track count(1).
            let header: u16 = 2
                + if album_title.is_some() { 1 } else { 0 }
                + if info.is_some() || probe_notice.is_some() { 1 } else { 0 };
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
pub fn draw_source_pane(
    f: &mut Frame,
    area: Rect,
    source: &SourceState,
    focused: bool,
    maximized: bool,
    theme: super::theme::Theme,
) {
    if area.height < 4 || area.width < 30 {
        return;
    }

    let border_color = if focused {
        theme.amber
    } else {
        theme.text_dim
    };
    let w = area.width as usize;

    // Top border with title-bar chrome.
    let title = match source.mode {
        SourceMode::Batch { .. } => " source (batch) ",
        SourceMode::MultiTrack { .. } => " source (tracks) ",
        _ => " source ",
    };
    let top_line = source_title_line(border_color, w, title, maximized, theme);

    // Bottom border: └───┘
    let bot_line = Line::from(Span::styled(
        format!("└{}┘", "─".repeat(w.saturating_sub(2))),
        theme.border(border_color),
    ));

    let content_lines = match &source.mode {
        SourceMode::Empty => render_empty(border_color, w, theme),
        SourceMode::Single {
            path,
            info,
            probe_notice,
            ..
        } => render_single(
            border_color,
            w,
            path,
            info.as_ref(),
            probe_notice.as_deref(), theme),
        SourceMode::MultiTrack {
            path,
            info,
            tracks,
            area_label,
            album_title,
            album_artist,
            probe_notice,
            scroll,
            cursor,
            selected,
            disc_contents,
            selected_presentation_id,
            ..
        } => render_multi_track(
            border_color,
            w,
            path,
            info.as_ref(),
            tracks,
            area_label.as_deref(),
            album_title.as_deref(),
            album_artist.as_deref(),
            probe_notice.as_deref(),
            *scroll,
            *cursor,
            selected,
            area.height,
            disc_contents.as_deref(),
            selected_presentation_id.as_ref(), theme),
        SourceMode::Batch {
            paths,
            cursor,
            cursor_info,
            probe_notice,
            cursor_probe_notice,
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
            effective_batch_probe_notice(
                paths,
                *cursor,
                probe_notice.as_deref(),
                cursor_probe_notice.as_ref(),
            ),
            *total_size,
            *album_count,
            format_histogram,
            area.height, theme),
    };

    let mut lines = vec![top_line];
    lines.extend(content_lines);
    let target_len_before_bottom = area.height.saturating_sub(1) as usize;
    while lines.len() < target_len_before_bottom {
        lines.push(bordered_line(border_color, w, vec![], theme));
    }
    lines.push(bot_line);

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
}


/// Draw the collapsed source title bar.
pub fn draw_source_title_bar(f: &mut Frame, area: Rect, source: &SourceState, focused: bool, theme: super::theme::Theme) {
    if area.height < 1 || area.width < 12 {
        return;
    }
    let border_color = if focused { theme.amber } else { theme.text_dim };
    let title = match source.mode {
        SourceMode::Batch { .. } => " source (batch) ",
        SourceMode::MultiTrack { .. } => " source (tracks) ",
        _ => " source ",
    };
    f.render_widget(Paragraph::new(vec![source_title_line(
        border_color,
        area.width as usize,
        title,
        false, theme)]), area);
}

fn source_title_line<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    title: &'a str,
    maximized: bool,
    theme: super::theme::Theme,
) -> Line<'a> {
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

/// Render the Empty placeholder content.
fn render_empty<'a>(border_color: ratatui::style::Color, w: usize,
    theme: super::theme::Theme,
) -> Vec<Line<'a>> {
    vec![
        bordered_line(border_color, w, vec![], theme),
        bordered_line(
            border_color,
            w,
            vec![Span::styled(
                "   press :browse or click the pill below to pick a source file",
                theme.muted(),
            )], theme),
        bordered_line(border_color, w, vec![], theme),
        browse_pill_row(border_color, w, theme),
    ]
}

/// Render the single-file content (path, format info, duration, browse pill).
fn render_single<'a>(
    border_color: ratatui::style::Color,
    w: usize,
    path: &std::path::Path,
    info: Option<&SourceInfo>,
    probe_notice: Option<&str>,
    theme: super::theme::Theme,
) -> Vec<Line<'a>> {
    let Some(info) = info else {
        // Path is known but no reliable probe info is available yet. When the
        // source carries a durable notice (notably a malformed/empty direct
        // `.cue`), show that warning instead of an indefinite probing state.
        let status_line = if let Some(notice) = probe_notice {
            bordered_line(
                border_color,
                w,
                vec![
                    Span::styled("   warning   ", theme.muted()),
                    Span::styled(
                        truncate_to(notice, w.saturating_sub(15)),
                        Style::default().fg(theme.amber),
                    ),
                ], theme)
        } else {
            bordered_line(
                border_color,
                w,
                vec![Span::styled("   probing…", theme.muted())], theme)
        };
        return vec![
            bordered_line(
                border_color,
                w,
                vec![
                    Span::styled("   path      ", theme.muted()),
                    Span::styled(shorten_path(path, w.saturating_sub(16)), theme.bright()),
                ], theme),
            status_line,
            bordered_line(border_color, w, vec![], theme),
            browse_pill_row(border_color, w, theme),
        ];
    };

    let path_truncated = shorten_path(path, w.saturating_sub(16));

    let mut format_parts = vec![
        Span::styled("   format    ", theme.muted()),
        Span::styled(info.format_name.clone(), theme.bold(theme.blue)),
    ];
    if !info.codec.is_empty() {
        format_parts.push(Span::styled(" │ ", theme.muted()));
        format_parts.push(Span::styled(info.codec_display(), theme.text_style()));
    }
    if info.sample_rate > 0 {
        format_parts.push(Span::styled(" │ ", theme.muted()));
        format_parts.push(Span::styled(info.sample_rate_display(), theme.text_style()));
    }
    if info.channels > 0 {
        format_parts.push(Span::styled(" │ ", theme.muted()));
        format_parts.push(Span::styled(info.channels_display(), theme.text_style()));
    }
    if info.file_size > 0 {
        format_parts.push(Span::styled(" │ ", theme.muted()));
        format_parts.push(Span::styled(info.size_display(), theme.text_style()));
    }

    vec![
        bordered_line(
            border_color,
            w,
            vec![
                Span::styled("   path      ", theme.muted()),
                Span::styled(path_truncated, theme.bright()),
            ], theme),
        bordered_line(border_color, w, format_parts, theme),
        bordered_line(
            border_color,
            w,
            vec![
                Span::styled("   duration  ", theme.muted()),
                Span::styled(info.duration_display(), theme.text_style()),
            ], theme),
        two_pill_row(border_color, w, BROWSE_PILL_LABEL, theme),
    ]
}

fn effective_batch_probe_notice<'a>(
    paths: &'a [std::path::PathBuf],
    cursor: usize,
    batch_notice: Option<&'a str>,
    cursor_notice: Option<&'a (std::path::PathBuf, String)>,
) -> Option<&'a str> {
    if let (Some(current), Some((notice_path, notice))) = (paths.get(cursor), cursor_notice) {
        if current == notice_path {
            return Some(notice.as_str());
        }
    }
    batch_notice
}

/// Render the multi-file batch content: summary + inline file list + pill.
/// `pane_height` is the total allocated pane height including borders.
#[allow(clippy::too_many_arguments)]
fn render_batch<'a>(
    border_color: ratatui::style::Color,
    w: usize,
    paths: &[std::path::PathBuf],
    cursor: usize,
    cursor_info: Option<&SourceInfo>,
    probe_notice: Option<&str>,
    total_size: u64,
    album_count: usize,
    format_histogram: &[(AudioFormat, usize)],
    pane_height: u16,
    theme: super::theme::Theme,
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
            Span::styled("   batch     ", theme.muted()),
            Span::styled(summary_line, theme.bold(theme.blue)),
        ], theme));

    // Line 2: currently previewed file. This is the same shared cursor used
    // by the metadata file list, so moving in either pane keeps the source
    // audio preview and metadata tag preview synchronized.
    let cursor = cursor.min(n.saturating_sub(1));
    let cursor_name = paths
        .get(cursor)
        .map(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_default();
    let cursor_audio = cursor_info
        .map(batch_cursor_audio_summary)
        .unwrap_or_else(|| "probing…".to_string());
    let selected_text = format!(
        "{}/{} {} │ {}",
        cursor + 1,
        n,
        cursor_name,
        cursor_audio
    );
    lines.push(bordered_line(
        border_color,
        w,
        vec![
            Span::styled("   selected  ", theme.muted()),
            Span::styled(
                truncate_to(&selected_text, w.saturating_sub(15)),
                theme.bold(theme.cyan),
            ),
        ], theme));

    // Line 3: persistent batch-level probe notice, when present. This is
    // intentionally separate from the status bar so warnings remain visible
    // until the source changes.
    let header_rows: u16 = if let Some(notice) = probe_notice {
        lines.push(bordered_line(
            border_color,
            w,
            vec![
                Span::styled("   warning   ", theme.muted()),
                Span::styled(
                    truncate_to(notice, w.saturating_sub(15)),
                    Style::default().fg(theme.amber),
                ),
            ], theme));
        4
    } else {
        3
    };

    // Next line: format histogram, e.g. "FLAC (3) · WAV (2)"
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
            Span::styled("   formats   ", theme.muted()),
            Span::styled(truncate_to(&hist_str, w.saturating_sub(15)), theme.text_style()),
        ], theme));

    // Inline file list. The list is cursor-windowed rather than always
    // starting at zero, so cursor moves from the source pane and from the
    // metadata pane stay visible without adding another persistent scroll
    // field to SourceMode::Batch.
    let tracks_per_row: usize = if w >= 100 { 2 } else { 1 };
    let list_rows = pane_height.saturating_sub(2 + header_rows + 1) as usize; // -borders -header -pill
    let has_overflow = n > list_rows.saturating_mul(tracks_per_row);
    let item_rows = if has_overflow && list_rows > 1 {
        list_rows - 1
    } else {
        list_rows
    };
    let max_visible = item_rows.saturating_mul(tracks_per_row);
    let start = batch_window_start(cursor, n, max_visible);
    let end = n.min(start.saturating_add(max_visible));
    let col_width = if tracks_per_row == 2 {
        w.saturating_sub(4) / 2
    } else {
        w.saturating_sub(4)
    };

    // Column-first layout within the visible window.
    let visible_count = end.saturating_sub(start);
    let num_rows = (visible_count + tracks_per_row - 1) / tracks_per_row;
    for row in 0..num_rows {
        let mut row_spans = Vec::new();
        for col in 0..tracks_per_row {
            let offset = row + col * num_rows;
            if offset >= visible_count {
                break;
            }
            let abs = start + offset;
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
                theme.cyan
            } else {
                theme.text_bright
            };
            let prefix = if abs == cursor { "▶ " } else { "  " };
            let numbered = format!("{}{}. {}", prefix, abs + 1, filename);
            let truncated = truncate_to(&numbered, col_width.saturating_sub(3));
            let padded = format!("   {:<width$}", truncated, width = col_width.saturating_sub(3));
            row_spans.push(Span::styled(padded, Style::default().fg(fg)));
        }
        lines.push(bordered_line(border_color, w, row_spans, theme));
    }

    if has_overflow && list_rows > item_rows {
        let before = start;
        let after = n.saturating_sub(end);
        let message = if before > 0 && after > 0 {
            format!("   showing {}-{} of {} · {} before · {} after", start + 1, end, n, before, after)
        } else if before > 0 {
            format!("   showing {}-{} of {} · {} before", start + 1, end, n, before)
        } else {
            format!("   showing {}-{} of {} · {} after", start + 1, end, n, after)
        };
        lines.push(bordered_line(
            border_color,
            w,
            vec![Span::styled(
                truncate_to(&message, w.saturating_sub(4)),
                Style::default().fg(theme.text_dim),
            )], theme));
    }

    lines.push(two_pill_row(border_color, w, EXPAND_PILL_LABEL, theme));

    lines
}

fn batch_cursor_audio_summary(info: &SourceInfo) -> String {
    let mut parts = Vec::new();
    if !info.format_name.is_empty() {
        parts.push(info.format_name.clone());
    }
    if !info.codec.is_empty() {
        parts.push(info.codec_display());
    }
    if info.sample_rate > 0 {
        parts.push(info.sample_rate_display());
    }
    if info.channels > 0 {
        parts.push(info.channels_display());
    }
    let duration = info.duration_display();
    if !duration.is_empty() {
        parts.push(duration);
    }
    if info.file_size > 0 {
        parts.push(info.size_display());
    }
    if parts.is_empty() {
        "probed".to_string()
    } else {
        parts.join(" · ")
    }
}

fn batch_window_start(cursor: usize, total: usize, visible_capacity: usize) -> usize {
    if total == 0 || visible_capacity == 0 || total <= visible_capacity {
        return 0;
    }
    let cursor = cursor.min(total - 1);
    let half_window = visible_capacity / 2;
    cursor
        .saturating_sub(half_window)
        .min(total.saturating_sub(visible_capacity))
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
    let display = if let Ok(home) = std::env::var("HOME") {
        let home_path = std::path::Path::new(&home);
        if let Ok(rest) = path.strip_prefix(home_path) {
            let rest = rest.display().to_string();
            if rest.is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", rest)
            }
        } else {
            path.display().to_string()
        }
    } else {
        path.display().to_string()
    };

    truncate_left_to_chars(&display, max_chars)
}

/// Return the terminal-cell display width ratatui will use for this text.
fn text_width(s: &str) -> usize {
    Line::from(s).width()
}

/// Left-truncate to at most `max_width` terminal cells without slicing the
/// input at a byte offset. Wide Unicode characters and combining marks are
/// measured using ratatui's display-width calculation.
fn truncate_left_to_chars(s: &str, max_width: usize) -> String {
    if text_width(s) <= max_width {
        return s.to_string();
    }
    if max_width == 0 {
        return String::new();
    }

    let ellipsis = "...";
    let ellipsis_width = text_width(ellipsis);
    if max_width <= ellipsis_width {
        let mut out = String::new();
        for ch in s.chars().rev() {
            let candidate = format!("{}{}", ch, out);
            if text_width(&candidate) > max_width {
                break;
            }
            out = candidate;
        }
        return out;
    }

    let mut tail = String::new();
    for ch in s.chars().rev() {
        let candidate = format!("{}{}", ch, tail);
        if ellipsis_width + text_width(&candidate) > max_width {
            break;
        }
        tail = candidate;
    }
    format!("{}{}", ellipsis, tail)
}

/// Render the "browse files" pill row, right-aligned inside the source pane.
fn browse_pill_row(border_color: ratatui::style::Color, width: usize, theme: super::theme::Theme) -> Line<'static> {
    pill_row(border_color, width, BROWSE_PILL_LABEL, theme)
}

/// Row with two pills: a primary pill (right-aligned, theme) and an analyze pill
/// to its left. Used when a file is loaded (Single/Batch).
fn two_pill_row(
    border_color: ratatui::style::Color,
    width: usize,
    primary_label: &'static str,
    theme: super::theme::Theme,
) -> Line<'static> {
    let pill_style = Style::default()
        .fg(theme.pill_active_fg)
        .bg(theme.pill_active_bg)
        .add_modifier(ratatui::style::Modifier::BOLD);
    let analyze_style = Style::default()
        .fg(theme.pill_active_fg)
        .bg(theme.purple)
        .add_modifier(ratatui::style::Modifier::BOLD);

    let primary_w = primary_label.chars().count();
    let analyze_w = ANALYZE_PILL_LABEL.chars().count();
    let gap = 2;
    let right_margin = 3;
    let total_pills = analyze_w + gap + primary_w;
    let inner_w = width.saturating_sub(2);
    let left_pad = inner_w.saturating_sub(total_pills + right_margin);

    Line::from(vec![
        Span::styled("│", theme.border(border_color)),
        Span::raw(" ".repeat(left_pad)),
        Span::styled(ANALYZE_PILL_LABEL, analyze_style),
        Span::raw(" ".repeat(gap)),
        Span::styled(primary_label, pill_style),
        Span::raw(" ".repeat(right_margin)),
        Span::styled("│", theme.border(border_color)),
    ])
}

/// Shared pill-row renderer: right-aligned pill with a small margin.
fn pill_row(
    border_color: ratatui::style::Color,
    width: usize,
    label: &'static str,
    theme: super::theme::Theme,
) -> Line<'static> {
    let pill_style = Style::default()
        .fg(theme.pill_active_fg)
        .bg(theme.pill_active_bg)
        .add_modifier(ratatui::style::Modifier::BOLD);

    let pill_w = label.chars().count();
    let inner_w = width.saturating_sub(2);
    let right_margin = 3;
    let left_pad = inner_w.saturating_sub(pill_w + right_margin);

    Line::from(vec![
        Span::styled("│", theme.border(border_color)),
        Span::raw(" ".repeat(left_pad)),
        Span::styled(label, pill_style),
        Span::raw(" ".repeat(right_margin)),
        Span::styled("│", theme.border(border_color)),
    ])
}

/// Create a line with │ content ... │ border
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

/// Render multi-track source (SACD ISO or CUE+image).
/// `pane_height` is the total allocated pane height including borders.
#[allow(clippy::too_many_arguments)]
fn render_multi_track<'a>(
    border_color: ratatui::style::Color,
    w: usize,
    path: &Path,
    info: Option<&SourceInfo>,
    tracks: &[super::app::MultiTrackEntry],
    area_label: Option<&str>,
    album_title: Option<&str>,
    album_artist: Option<&str>,
    probe_notice: Option<&str>,
    scroll: usize,
    cursor: usize,
    selected: &[bool],
    pane_height: u16,
    disc_contents: Option<&crate::disc::DiscContents>,
    selected_presentation_id: Option<&crate::disc::PresentationId>,
    theme: super::theme::Theme,
) -> Vec<Line<'a>> {
    let mut lines = Vec::new();
    let name = path.file_name().unwrap_or_default().to_string_lossy();

    // Header: filename + area label
    let mut header_spans = vec![
        Span::styled("   ", Style::default()),
        Span::styled(
            truncate_to(&name, w.saturating_sub(6)),
            Style::default().fg(theme.text_bright),
        ),
    ];
    let has_stream_pill = disc_contents
        .map(|dc| dc.presentations.len() >= 2)
        .unwrap_or(false);

    if has_stream_pill {
        // Find the label for the currently selected presentation
        let current_label = disc_contents
            .and_then(|dc| {
                selected_presentation_id.and_then(|sel_id| {
                    dc.presentations
                        .iter()
                        .find(|p| &p.id == sel_id)
                        .map(|p| p.label.as_str())
                })
            })
            .or(area_label)
            .unwrap_or("Unknown");
        header_spans.push(Span::styled(
            format!("  [◀ {} ▶]", current_label),
            Style::default()
                .fg(theme.pill_active_fg)
                .bg(theme.cyan)
                .add_modifier(ratatui::style::Modifier::BOLD),
        ));
    } else if let Some(area) = area_label {
        header_spans.push(Span::styled(
            format!("  [{}]", area),
            Style::default().fg(theme.cyan),
        ));
    }
    lines.push(bordered_line(border_color, w, header_spans, theme));

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
                Style::default().fg(theme.text_dim),
            )], theme));
    }

    if let Some(info) = info {
        lines.push(bordered_line(
            border_color,
            w,
            vec![
                Span::styled("   source    ", theme.muted()),
                Span::styled(
                    truncate_to(&batch_cursor_audio_summary(info), w.saturating_sub(15)),
                    theme.text_style(),
                ),
            ], theme));
    } else if let Some(notice) = probe_notice {
        lines.push(bordered_line(
            border_color,
            w,
            vec![
                Span::styled("   source    ", theme.muted()),
                Span::styled(
                    truncate_to(notice, w.saturating_sub(15)),
                    Style::default().fg(theme.amber),
                ),
            ], theme));
    }

    // Track count
    lines.push(bordered_line(
        border_color,
        w,
        vec![Span::styled(
            format!("   {} tracks", tracks.len()),
            Style::default().fg(theme.text_muted),
        )], theme));

    // Derive max visible tracks from pane height.
    // pane_height = borders(2) + header_rows + track_rows + pill(1, theme) [+ overflow(1)]
    let header_rows: u16 = lines.len() as u16;
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
                theme.cyan
            } else if checked {
                theme.text_bright
            } else {
                theme.text_dim
            };
            let truncated = truncate_to(&entry, col_width.saturating_sub(1));
            let padded = format!("   {:<width$}", truncated, width = col_width.saturating_sub(3));
            row_spans.push(Span::styled(padded, Style::default().fg(fg)));
        }
        lines.push(bordered_line(border_color, w, row_spans, theme));
    }

    if end < tracks.len() {
        lines.push(bordered_line(
            border_color,
            w,
            vec![Span::styled(
                format!("   ... and {} more", tracks.len() - end),
                Style::default().fg(theme.text_dim),
            )], theme));
    }

    // Expand + analyze pill row
    lines.push(two_pill_row(border_color, w, EXPAND_PILL_LABEL, theme));

    lines
}

/// Right-truncate to at most `max_width` terminal cells without slicing the
/// input at a byte offset. Wide Unicode characters and combining marks are
/// measured using ratatui's display-width calculation.
fn truncate_to(s: &str, max_width: usize) -> String {
    if text_width(s) <= max_width {
        return s.to_string();
    }
    if max_width == 0 {
        return String::new();
    }

    let ellipsis = "...";
    let ellipsis_width = text_width(ellipsis);
    if max_width <= ellipsis_width {
        let mut out = String::new();
        for ch in s.chars() {
            let candidate = format!("{}{}", out, ch);
            if text_width(&candidate) > max_width {
                break;
            }
            out = candidate;
        }
        return out;
    }

    let mut head = String::new();
    for ch in s.chars() {
        let candidate = format!("{}{}", head, ch);
        if text_width(&candidate) + ellipsis_width > max_width {
            break;
        }
        head = candidate;
    }
    format!("{}{}", head, ellipsis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::probe::SourceMetadata;
    use std::path::PathBuf;

    fn batch_mode_with_notice(probe_notice: Option<String>) -> SourceMode {
        SourceMode::Batch {
            paths: vec![PathBuf::from("/tmp/a.cue"), PathBuf::from("/tmp/b.flac")],
            cursor: 0,
            cursor_info: None,
            cursor_metadata: SourceMetadata::default(),
            probe_notice,
            cursor_probe_notice: None,
            total_size: 0,
            album_count: 1,
            format_histogram: Vec::new(),
        }
    }


    #[test]
    fn render_single_without_info_shows_persistent_probe_notice() {
        let theme = crate::tui::theme::theme_by_slug("catppuccin-latte").expect("theme");
        let lines = render_single(
            theme.text_bright,
            96,
            Path::new("/tmp/empty.cue"),
            None,
            Some("CUE sheet has no audio tracks"), theme);
        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("");

        assert!(rendered.contains("warning"));
        assert!(rendered.contains("CUE sheet has no audio tracks"));
        assert!(!rendered.contains("probing"));
    }

    #[test]
    fn cursor_probe_notice_only_renders_for_matching_batch_cursor_path() {
        let paths = vec![PathBuf::from("/tmp/a.flac"), PathBuf::from("/tmp/b.cue")];
        let notice = Some((PathBuf::from("/tmp/b.cue"), "CUE image probe failed; set format manually".to_string()));

        assert_eq!(
            effective_batch_probe_notice(&paths, 1, None, notice.as_ref()),
            Some("CUE image probe failed; set format manually")
        );
        assert_eq!(effective_batch_probe_notice(&paths, 0, None, notice.as_ref()), None);
    }

    #[test]
    fn batch_probe_notice_gets_source_pane_height() {
        let without = source_pane_height(&batch_mode_with_notice(None), 80);
        let with = source_pane_height(
            &batch_mode_with_notice(Some("mixed source properties; set format manually".to_string())),
            80,
        );

        assert!(with > without);
    }

    #[test]
    fn render_batch_includes_persistent_probe_notice() {
        let theme = crate::tui::theme::theme_by_slug("catppuccin-latte").expect("theme");
        let paths = vec![PathBuf::from("/tmp/a.cue"), PathBuf::from("/tmp/b.flac")];
        let lines = render_batch(
            theme.text_bright,
            96,
            &paths,
            0,
            None,
            Some("mixed source properties; set format manually"),
            0,
            1,
            &[],
            10, theme);
        let rendered = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("");

        assert!(rendered.contains("warning"));
        assert!(rendered.contains("mixed source properties; set format manually"));
    }
}
