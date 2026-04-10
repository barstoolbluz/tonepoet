//! Browse screen: file browser with directory tree + info pane

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::AppState;
use super::browse::{BrowseEntry, BrowseState, EntryKind};
use super::draw_footer::draw_footer;
use super::draw_header::draw_header;
use super::theme;

/// Draw the full browse screen
pub fn draw_browse_screen(f: &mut Frame, area: Rect, app: &mut AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),  // header banner
            Constraint::Length(1),  // blank
            Constraint::Length(1),  // breadcrumb
            Constraint::Length(1),  // blank
            Constraint::Min(10),    // main content (list + info)
            Constraint::Length(2),  // footer (tabs + context)
        ])
        .split(area);

    draw_header(f, chunks[0]);
    draw_breadcrumb(f, chunks[2], &app.browse);

    // Split main content horizontally: list (2/3) + info (1/3)
    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(66), Constraint::Percentage(34)])
        .split(chunks[4]);

    draw_browse_list(f, content_chunks[0], &mut app.browse);
    draw_browse_info(f, content_chunks[1], &app.browse);

    draw_footer(f, chunks[5], app.current_screen);
}

/// Draw the breadcrumb bar showing the current directory path
fn draw_breadcrumb(f: &mut Frame, area: Rect, browse: &BrowseState) {
    if area.width < 10 {
        return;
    }

    let path_str = browse.current_dir.display().to_string();
    let home = std::env::var("HOME").unwrap_or_default();
    let display = if !home.is_empty() && path_str.starts_with(&home) {
        format!("~{}", &path_str[home.len()..])
    } else {
        path_str
    };

    let mut spans = vec![Span::styled("  path  ", theme::muted())];
    spans.push(Span::styled(display, theme::bright()));

    let line = Paragraph::new(Line::from(spans));
    f.render_widget(line, area);
}

/// Draw the directory listing (left pane)
fn draw_browse_list(f: &mut Frame, area: Rect, browse: &mut BrowseState) {
    if area.height < 4 || area.width < 20 {
        return;
    }

    let border_color = theme::CYAN;
    let w = area.width as usize;

    // Top border with title
    let title = " browse ";
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

    // Content area: inside border, inside 1-row top/bottom padding
    let content_height = (area.height as usize).saturating_sub(2);
    browse.visible_height = content_height;

    let mut lines: Vec<Line> = vec![top_line];

    if let Some(err) = &browse.error {
        lines.push(bordered_line(
            border_color,
            w,
            vec![Span::styled(
                format!("   {}", err),
                Style::default().fg(theme::RED),
            )],
        ));
        // Fill remaining space
        for _ in 1..content_height {
            lines.push(bordered_line(border_color, w, vec![]));
        }
    } else if browse.entries.is_empty() {
        lines.push(bordered_line(
            border_color,
            w,
            vec![Span::styled("   (empty)", theme::muted())],
        ));
        for _ in 1..content_height {
            lines.push(bordered_line(border_color, w, vec![]));
        }
    } else {
        let start = browse.scroll_offset;
        let end = (start + content_height).min(browse.entries.len());

        for i in start..end {
            let entry = &browse.entries[i];
            let is_selected = i == browse.selected_index;
            let is_checked = browse.is_multi_selected(&entry.path);
            lines.push(render_entry_line(border_color, w, entry, is_selected, is_checked));
        }

        // Fill remaining space with empty bordered lines
        let rendered = end - start;
        for _ in rendered..content_height {
            lines.push(bordered_line(border_color, w, vec![]));
        }
    }

    lines.push(bot_line);

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
}

/// Render a single entry row
fn render_entry_line<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    entry: &'a BrowseEntry,
    is_selected: bool,
    is_checked: bool,
) -> Line<'a> {
    // Cursor indicator
    let cursor = if is_selected { "▸ " } else { "  " };
    let cursor_style = if is_selected {
        Style::default().fg(theme::BLUE)
    } else {
        Style::default().fg(theme::TEXT_DIM)
    };

    // Multi-select checkbox
    let check = if is_checked { "●" } else { " " };
    let check_style = if is_checked {
        Style::default().fg(theme::CYAN)
    } else {
        Style::default().fg(theme::TEXT_DIM)
    };

    // Entry name color
    let name_style = match &entry.kind {
        EntryKind::ParentDir => Style::default().fg(theme::TEXT_MUTED),
        EntryKind::Directory => Style::default().fg(theme::BLUE),
        EntryKind::AudioFile(_) => {
            if is_selected {
                Style::default().fg(theme::TEXT_BRIGHT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::TEXT)
            }
        }
        EntryKind::Archive => Style::default().fg(theme::AMBER),
        EntryKind::OtherFile => Style::default().fg(theme::TEXT_DIM),
    };

    // Display name (truncated to leave room for right-aligned info)
    let info_text = match &entry.kind {
        EntryKind::ParentDir => String::new(),
        EntryKind::Directory => String::from("dir"),
        EntryKind::AudioFile(fmt) => format!("{}  {}", fmt.name(), size_str(entry.size)),
        EntryKind::Archive => format!("7z  {}", size_str(entry.size)),
        EntryKind::OtherFile => size_str(entry.size),
    };

    // Layout: │ cursor(2) check(1) space(1) name... info │
    let prefix_width = 2 + 1 + 1; // cursor + check + space
    let info_width = info_text.len() + 2; // +2 for trailing spaces
    let name_max = width
        .saturating_sub(2) // borders
        .saturating_sub(prefix_width)
        .saturating_sub(info_width);

    let name_display = if entry.name.chars().count() > name_max && name_max > 3 {
        let truncated: String = entry.name.chars().take(name_max - 1).collect();
        format!("{}…", truncated)
    } else {
        entry.name.clone()
    };

    let name_width = name_display.chars().count();
    let gap = name_max.saturating_sub(name_width);

    let mut spans = vec![
        Span::styled("│", theme::border(border_color)),
        Span::styled(cursor, cursor_style),
        Span::styled(check, check_style),
        Span::raw(" "),
        Span::styled(name_display, name_style),
        Span::raw(" ".repeat(gap)),
        Span::styled(info_text, theme::muted()),
        Span::raw("  "),
        Span::styled("│", theme::border(border_color)),
    ];

    // Selected row gets a subtle bg highlight
    if is_selected {
        for span in spans.iter_mut() {
            // Only add bg to the content spans, not the borders
            if !matches!(span.content.as_ref(), "│") {
                let current = span.style;
                span.style = current.bg(theme::BORDER_DIM);
            }
        }
    }

    Line::from(spans)
}

/// Draw the info pane (right pane) showing details for the selected entry
fn draw_browse_info(f: &mut Frame, area: Rect, browse: &BrowseState) {
    if area.height < 4 || area.width < 15 {
        return;
    }

    let border_color = theme::AMBER;
    let w = area.width as usize;

    // Top border
    let title = " info ";
    let dash_count = w.saturating_sub(2 + title.len());

    let top_line = Line::from(vec![
        Span::styled("┌", theme::border(border_color)),
        Span::styled(title, theme::border(border_color)),
        Span::styled("─".repeat(dash_count), theme::border(border_color)),
        Span::styled("┐", theme::border(border_color)),
    ]);

    let bot_line = Line::from(Span::styled(
        format!("└{}┘", "─".repeat(w.saturating_sub(2))),
        theme::border(border_color),
    ));

    let content_height = (area.height as usize).saturating_sub(2);

    let mut lines: Vec<Line> = vec![top_line];

    // Available width for content (inside borders, after the 3-space indent)
    let content_width = w.saturating_sub(2);
    let content_lines = if let Some(entry) = browse.selected_entry() {
        entry_info_lines(entry, browse, content_width)
    } else {
        vec![vec![Span::styled("   (no selection)", theme::muted())]]
    };

    // Render content lines with border
    for line_spans in content_lines.iter().take(content_height) {
        lines.push(bordered_line(border_color, w, line_spans.clone()));
    }

    // Fill remaining
    let rendered = content_lines.len().min(content_height);
    for _ in rendered..content_height {
        lines.push(bordered_line(border_color, w, vec![]));
    }

    lines.push(bot_line);

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
}

/// Truncate a string to fit within `max_chars` columns, adding ellipsis if needed.
fn truncate_to(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars || max_chars < 2 {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars - 1).collect();
    format!("{}…", truncated)
}

/// Build content lines for the info pane based on the entry kind.
/// `content_width` is the width available inside the pane borders.
fn entry_info_lines(
    entry: &BrowseEntry,
    browse: &BrowseState,
    content_width: usize,
) -> Vec<Vec<Span<'static>>> {
    // Maximum width for free-form text values: subtract the 3-space indent
    let max_value_chars = content_width.saturating_sub(3);

    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();

    // Blank
    lines.push(vec![]);

    // Name section
    lines.push(vec![Span::styled("   name", theme::muted())]);
    lines.push(vec![
        Span::raw("   "),
        Span::styled(truncate_to(&entry.name, max_value_chars), theme::bright()),
    ]);
    lines.push(vec![]);

    match &entry.kind {
        EntryKind::ParentDir => {
            lines.push(vec![Span::styled("   parent directory", theme::muted())]);
        }
        EntryKind::Directory => {
            lines.push(vec![
                Span::styled("   kind    ", theme::muted()),
                Span::styled("directory", theme::text()),
            ]);
            // Show directory stats if cached
            if let Some(stats) = browse.current_dir_stats() {
                lines.push(vec![
                    Span::styled("   files   ", theme::muted()),
                    Span::styled(stats.file_count.to_string(), theme::text()),
                ]);
                if stats.audio_count > 0 {
                    lines.push(vec![
                        Span::styled("   audio   ", theme::muted()),
                        Span::styled(stats.audio_count.to_string(), theme::accent()),
                    ]);
                }
                lines.push(vec![
                    Span::styled("   size    ", theme::muted()),
                    Span::styled(size_str(stats.total_size), theme::text()),
                ]);
            }
        }
        EntryKind::AudioFile(fmt) => {
            // Show cached probe info if available
            if let Some(cached) = browse.current_cached_info() {
                let info = &cached.source;
                lines.push(vec![
                    Span::styled("   format  ", theme::muted()),
                    Span::styled(info.format_name.clone(), theme::bold(theme::BLUE)),
                ]);
                lines.push(vec![
                    Span::styled("   codec   ", theme::muted()),
                    Span::styled(info.codec_display(), theme::text()),
                ]);
                if info.sample_rate > 0 {
                    lines.push(vec![
                        Span::styled("   rate    ", theme::muted()),
                        Span::styled(info.sample_rate_display(), theme::text()),
                    ]);
                }
                if info.channels > 0 {
                    lines.push(vec![
                        Span::styled("   channels", theme::muted()),
                        Span::raw(" "),
                        Span::styled(info.channels_display(), theme::text()),
                    ]);
                }
                if info.duration_secs > 0.0 {
                    lines.push(vec![
                        Span::styled("   duration", theme::muted()),
                        Span::raw(" "),
                        Span::styled(info.duration_display(), theme::text()),
                    ]);
                }
                lines.push(vec![
                    Span::styled("   size    ", theme::muted()),
                    Span::styled(info.size_display(), theme::text()),
                ]);

                // Metadata tags
                let meta = &cached.metadata;
                let has_meta = meta.title.is_some()
                    || meta.artist.is_some()
                    || meta.album.is_some()
                    || meta.year.is_some();
                if has_meta {
                    // Inline labels (artist/album/year) get less room because of the label prefix
                    let inline_max = max_value_chars.saturating_sub(11); // " ARTIST  " = 11 chars
                    lines.push(vec![]);
                    if let Some(title) = &meta.title {
                        lines.push(vec![Span::styled("   title", theme::muted())]);
                        lines.push(vec![
                            Span::raw("   "),
                            Span::styled(truncate_to(title, max_value_chars), theme::bright()),
                        ]);
                    }
                    if let Some(artist) = &meta.artist {
                        lines.push(vec![
                            Span::styled("   artist  ", theme::muted()),
                            Span::styled(truncate_to(artist, inline_max), theme::text()),
                        ]);
                    }
                    if let Some(album) = &meta.album {
                        lines.push(vec![
                            Span::styled("   album   ", theme::muted()),
                            Span::styled(truncate_to(album, inline_max), theme::text()),
                        ]);
                    }
                    if let Some(year) = &meta.year {
                        lines.push(vec![
                            Span::styled("   year    ", theme::muted()),
                            Span::styled(truncate_to(year, inline_max), theme::text()),
                        ]);
                    }
                }
            } else {
                // Not yet probed or probe failed — show basic info
                lines.push(vec![
                    Span::styled("   format  ", theme::muted()),
                    Span::styled(fmt.name().to_string(), theme::bold(theme::BLUE)),
                ]);
                lines.push(vec![
                    Span::styled("   size    ", theme::muted()),
                    Span::styled(size_str(entry.size), theme::text()),
                ]);
            }
        }
        EntryKind::Archive => {
            lines.push(vec![
                Span::styled("   kind    ", theme::muted()),
                Span::styled("archive (7z)", theme::text()),
            ]);
            lines.push(vec![
                Span::styled("   size    ", theme::muted()),
                Span::styled(size_str(entry.size), theme::text()),
            ]);
        }
        EntryKind::OtherFile => {
            lines.push(vec![
                Span::styled("   kind    ", theme::muted()),
                Span::styled("file", theme::text()),
            ]);
            lines.push(vec![
                Span::styled("   size    ", theme::muted()),
                Span::styled(size_str(entry.size), theme::text()),
            ]);
        }
    }

    lines
}

/// Create a bordered line
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

/// Format a file size for display
fn size_str(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1_073_741_824.0 {
        format!("{:.1} GB", b / 1_073_741_824.0)
    } else if b >= 1_048_576.0 {
        format!("{:.1} MB", b / 1_048_576.0)
    } else if b >= 1024.0 {
        format!("{:.1} KB", b / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}
