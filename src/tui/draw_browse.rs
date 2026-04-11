//! Browse screen: file browser with directory tree + info pane

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::AppState;
use super::browse::{BrowseEntry, BrowseState, EntryKind, SortBy, SortDir};
use super::button_map::{ButtonRenderMap, ColumnKind, TuiButton};
use super::draw_footer::draw_footer;
use super::draw_header::draw_header;
use super::theme;

/// Fixed column widths (inside the list border). Name is flexible.
const COL_SIZE_W: usize = 9;
const COL_DATE_W: usize = 12;
const COL_TYPE_W: usize = 8;
/// Spaces between name|size, size|date, date|type columns.
const COL_GAPS: usize = 3;
/// Prefix: cursor(2) + check(1) + space(1).
const ROW_PREFIX: usize = 4;
/// Trailing space before the right border.
const ROW_TRAILING: usize = 2;

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

    let list_area = content_chunks[0];
    draw_browse_list(f, list_area, &mut app.browse);
    draw_browse_info(f, content_chunks[1], &app.browse);

    let status_msg = app.status_message.as_ref().map(|(s, _)| s.as_str());
    draw_footer(f, chunks[5], app.current_screen, &mut app.button_map, status_msg);

    // Register clickable regions for mouse support (split borrow)
    register_browse_buttons(&mut app.button_map, list_area, &app.browse);
}

/// Register mouse click targets for the browse list: column headers,
/// individual entry rows, and a catch-all list area for scroll wheel routing.
fn register_browse_buttons(buttons: &mut ButtonRenderMap, area: Rect, browse: &BrowseState) {
    if area.height < 4 || area.width < 20 {
        return;
    }

    // The whole list area (outer rect) is the scroll-wheel catch-all.
    buttons.record_button(TuiButton::BrowseList, area);

    let w = area.width as usize;
    let inner_w = w.saturating_sub(2);
    if inner_w <= ROW_PREFIX + ROW_TRAILING + COL_SIZE_W + COL_DATE_W + COL_TYPE_W + COL_GAPS {
        return;
    }
    let name_w = inner_w - ROW_PREFIX - ROW_TRAILING - COL_SIZE_W - COL_DATE_W - COL_TYPE_W - COL_GAPS;

    // Column x-offsets (relative to area.x). Header row is area.y + 1 (inside top border).
    let name_x0 = area.x + 1 + ROW_PREFIX as u16;
    let size_x0 = name_x0 + name_w as u16 + 1;
    let date_x0 = size_x0 + COL_SIZE_W as u16 + 1;
    let type_x0 = date_x0 + COL_DATE_W as u16 + 1;

    let header_y = area.y + 1;
    buttons.record_button(
        TuiButton::BrowseColumn(ColumnKind::Name),
        Rect::new(name_x0, header_y, name_w as u16, 1),
    );
    buttons.record_button(
        TuiButton::BrowseColumn(ColumnKind::Size),
        Rect::new(size_x0, header_y, COL_SIZE_W as u16, 1),
    );
    buttons.record_button(
        TuiButton::BrowseColumn(ColumnKind::Date),
        Rect::new(date_x0, header_y, COL_DATE_W as u16, 1),
    );
    buttons.record_button(
        TuiButton::BrowseColumn(ColumnKind::Type),
        Rect::new(type_x0, header_y, COL_TYPE_W as u16, 1),
    );

    // Entry rows: lines below the header row, above the bottom border.
    let content_height = (area.height as usize).saturating_sub(3); // top border + header + bottom border
    let start = browse.scroll_offset;
    let end = (start + content_height).min(browse.entries.len());
    for (row, i) in (start..end).enumerate() {
        let y = area.y + 2 + row as u16;
        buttons.record_button(
            TuiButton::BrowseEntry(i),
            Rect::new(area.x + 1, y, (inner_w) as u16, 1),
        );
    }
}

/// Draw the breadcrumb bar showing the current directory path
/// (and the active text filter, if any).
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

    // Filter suffix appears only when a text filter is active.
    let filter_suffix = if !browse.filter_text.is_empty() {
        format!("   filter: {}", browse.filter_text)
    } else {
        String::new()
    };

    // Reserve space for prefix + filter suffix; truncate the path from the left
    // so the most contextual portion (current directory) stays visible.
    let prefix = "  path  ";
    let prefix_w = prefix.chars().count();
    let suffix_w = filter_suffix.chars().count();
    let path_max = (area.width as usize)
        .saturating_sub(prefix_w)
        .saturating_sub(suffix_w)
        .saturating_sub(1); // safety margin
    let display_truncated = truncate_left(&display, path_max);

    let mut spans = vec![
        Span::styled(prefix, theme::muted()),
        Span::styled(display_truncated, theme::bright()),
    ];
    if !filter_suffix.is_empty() {
        spans.push(Span::styled(filter_suffix, Style::default().fg(theme::AMBER)));
    }

    let line = Paragraph::new(Line::from(spans));
    f.render_widget(line, area);
}

/// Truncate a string from the LEFT to fit `max` chars, prepending `…` if cut.
/// Used so the end of paths (most contextual portion) stays visible.
fn truncate_left(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max < 2 {
        return s.chars().rev().take(max).collect::<String>().chars().rev().collect();
    }
    let skip = count - (max - 1);
    let truncated: String = s.chars().skip(skip).collect();
    format!("…{}", truncated)
}

/// Draw the directory listing (left pane) with a sortable column header row.
/// Reserves an extra row for the live filter input when one is active.
fn draw_browse_list(f: &mut Frame, area: Rect, browse: &mut BrowseState) {
    if area.height < 4 || area.width < 20 {
        return;
    }

    let border_color = theme::CYAN;
    let w = area.width as usize;
    let inner_w = w.saturating_sub(2);

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

    // Content rows = total - top border - header - bottom border (-1 if filter row).
    let has_filter = browse.filter_input.is_some();
    let reserved = if has_filter { 4 } else { 3 };
    let content_height = (area.height as usize).saturating_sub(reserved);
    browse.visible_height = content_height;

    // Compute name column width, guarding against narrow widths.
    let fixed = ROW_PREFIX + ROW_TRAILING + COL_SIZE_W + COL_DATE_W + COL_TYPE_W + COL_GAPS;
    let name_w = inner_w.saturating_sub(fixed);

    let mut lines: Vec<Line> = vec![top_line];

    // Header row
    lines.push(render_header_row(
        border_color,
        w,
        name_w,
        browse.sort_by,
        browse.sort_dir,
    ));

    if let Some(err) = &browse.error {
        lines.push(bordered_line(
            border_color,
            w,
            vec![Span::styled(
                format!("   {}", err),
                Style::default().fg(theme::RED),
            )],
        ));
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
            lines.push(render_entry_line(
                border_color,
                w,
                name_w,
                entry,
                is_selected,
                is_checked,
            ));
        }

        let rendered = end - start;
        for _ in rendered..content_height {
            lines.push(bordered_line(border_color, w, vec![]));
        }
    }

    // Filter input row (just above the bottom border) when active.
    let mut filter_cursor: Option<u16> = None;
    if let Some(input) = &browse.filter_input {
        // Inside row layout: │ + " / " + <input view> + padding + │
        // Reserve 1 (left border) + 3 (" / ") + 2 (right padding + border) = 6
        let input_width = inner_w.saturating_sub(4); // " / " prefix takes 3 + 1 trailing space
        let (visible, cursor_col_in_view) = input.view(input_width);
        filter_cursor = Some(cursor_col_in_view);

        let visible_w = visible.chars().count();
        let pad = input_width.saturating_sub(visible_w);
        lines.push(Line::from(vec![
            Span::styled("│", theme::border(border_color)),
            Span::styled(" / ", Style::default().fg(theme::CYAN)),
            Span::styled(visible, Style::default().fg(theme::TEXT_BRIGHT)),
            Span::raw(" ".repeat(pad)),
            Span::raw(" "),
            Span::styled("│", theme::border(border_color)),
        ]));
    }

    lines.push(bot_line);

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);

    // Position the terminal cursor inside the filter input row.
    if let Some(col_in_view) = filter_cursor {
        let cursor_x = area.x + 1 + 3 + col_in_view; // border + " / " prefix
        let cursor_y = area.y + area.height - 2; // row above the bottom border
        f.set_cursor(cursor_x, cursor_y);
    }
}

/// Render the column header row with sort indicator (▲/▼) on the active column.
fn render_header_row(
    border_color: ratatui::style::Color,
    width: usize,
    name_w: usize,
    sort_by: SortBy,
    sort_dir: SortDir,
) -> Line<'static> {
    let arrow = match sort_dir {
        SortDir::Asc => "▲",
        SortDir::Desc => "▼",
    };

    let header_cell = |label: &'static str, col: SortBy, col_w: usize, right_align: bool| -> Vec<Span<'static>> {
        let is_active = sort_by == col;
        let style = if is_active {
            Style::default()
                .fg(theme::CYAN)
                .add_modifier(Modifier::BOLD)
        } else {
            theme::muted()
        };
        // Cell text: "label ▲" if active, else "label"
        let text: String = if is_active {
            format!("{} {}", label, arrow)
        } else {
            label.to_string()
        };
        let text_w = text.chars().count();
        let pad = col_w.saturating_sub(text_w);
        if right_align {
            vec![
                Span::raw(" ".repeat(pad)),
                Span::styled(text, style),
            ]
        } else {
            vec![
                Span::styled(text, style),
                Span::raw(" ".repeat(pad)),
            ]
        }
    };

    let mut spans = vec![
        Span::styled("│", theme::border(border_color)),
        Span::raw(" ".repeat(ROW_PREFIX)),
    ];
    spans.extend(header_cell("name", SortBy::Name, name_w, false));
    spans.push(Span::raw(" "));
    spans.extend(header_cell("size", SortBy::Size, COL_SIZE_W, true));
    spans.push(Span::raw(" "));
    spans.extend(header_cell("date", SortBy::Date, COL_DATE_W, false));
    spans.push(Span::raw(" "));
    spans.extend(header_cell("type", SortBy::Type, COL_TYPE_W, false));
    spans.push(Span::raw(" ".repeat(ROW_TRAILING)));
    spans.push(Span::styled("│", theme::border(border_color)));

    // Pad any shortfall to reach the right border cleanly (safety net).
    let used: usize = spans.iter().map(|s| s.width()).sum();
    if used < width {
        // Insert padding before the closing border
        let pad = width - used;
        let last = spans.pop().unwrap();
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(last);
    }

    Line::from(spans)
}

/// Render a single entry row with fixed columns: name | size | date | type
fn render_entry_line(
    border_color: ratatui::style::Color,
    width: usize,
    name_w: usize,
    entry: &BrowseEntry,
    is_selected: bool,
    is_checked: bool,
) -> Line<'static> {
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

    // Name (truncated to name_w)
    let name_display = pad_or_truncate(&entry.name, name_w, false);

    // Size (right-aligned, hidden for dirs/parent)
    let size_text = match &entry.kind {
        EntryKind::ParentDir | EntryKind::Directory => String::new(),
        _ => size_str(entry.size),
    };
    let size_display = pad_or_truncate(&size_text, COL_SIZE_W, true);

    // Date (left-aligned)
    let date_display = pad_or_truncate(&entry.date_label(), COL_DATE_W, false);

    // Type (left-aligned)
    let type_display = pad_or_truncate(&entry.type_label(), COL_TYPE_W, false);

    let mut spans = vec![
        Span::styled("│", theme::border(border_color)),
        Span::styled(cursor, cursor_style),
        Span::styled(check, check_style),
        Span::raw(" "),
        Span::styled(name_display, name_style),
        Span::raw(" "),
        Span::styled(size_display, theme::muted()),
        Span::raw(" "),
        Span::styled(date_display, theme::muted()),
        Span::raw(" "),
        Span::styled(type_display, theme::muted()),
        Span::raw(" ".repeat(ROW_TRAILING)),
        Span::styled("│", theme::border(border_color)),
    ];

    // Pad any shortfall before the right border (safety net on narrow widths).
    let used: usize = spans.iter().map(|s| s.width()).sum();
    if used < width {
        let pad = width - used;
        let last = spans.pop().unwrap();
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(last);
    }

    // Selected row gets a subtle bg highlight
    if is_selected {
        for span in spans.iter_mut() {
            if !matches!(span.content.as_ref(), "│") {
                let current = span.style;
                span.style = current.bg(theme::BORDER_DIM);
            }
        }
    }

    Line::from(spans)
}

/// Pad a string to `width` chars, or truncate with ellipsis if too long.
/// `right_align` pads on the left when true.
fn pad_or_truncate(s: &str, width: usize, right_align: bool) -> String {
    let count = s.chars().count();
    if count == width {
        return s.to_string();
    }
    if count > width {
        if width < 2 {
            return s.chars().take(width).collect();
        }
        let truncated: String = s.chars().take(width - 1).collect();
        return format!("{}…", truncated);
    }
    let pad = width - count;
    if right_align {
        format!("{}{}", " ".repeat(pad), s)
    } else {
        format!("{}{}", s, " ".repeat(pad))
    }
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
