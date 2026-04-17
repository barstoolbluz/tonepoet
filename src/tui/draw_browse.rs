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
use super::probe::MetadataField;
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
    let hover = app.hover_target;
    draw_browse_list(f, list_area, &mut app.browse, hover);
    draw_browse_info(f, content_chunks[1], &app.browse, &mut app.button_map, hover);

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

    let display = if let Some(ref arc) = browse.archive {
        // Inside an archive: show "archive.7z:/inner/path"
        let archive_name = arc.listing.archive_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if arc.inner_path.is_empty() {
            format!("{}:/", archive_name)
        } else {
            format!("{}:/{}", archive_name, arc.inner_path)
        }
    } else {
        let path_str = browse.current_dir.display().to_string();
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() && path_str.starts_with(&home) {
            format!("~{}", &path_str[home.len()..])
        } else {
            path_str
        }
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
fn draw_browse_list(
    f: &mut Frame,
    area: Rect,
    browse: &mut BrowseState,
    hover: Option<super::button_map::TuiButton>,
) {
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
        let msg = if browse.scan_pending.is_some() {
            "   Loading..."
        } else {
            "   (empty)"
        };
        lines.push(bordered_line(
            border_color,
            w,
            vec![Span::styled(msg, theme::muted())],
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
            let is_hovered = !is_selected
                && hover == Some(super::button_map::TuiButton::BrowseEntry(i));
            lines.push(render_entry_line(
                border_color,
                w,
                name_w,
                entry,
                is_selected,
                is_checked,
                is_hovered,
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
    is_hovered: bool,
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

    // Entry name color. Broken symlinks override to red regardless of kind.
    let name_style = if entry.is_broken_symlink {
        Style::default().fg(theme::RED)
    } else {
        match &entry.kind {
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
        }
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

    // Selected row gets a subtle bg highlight; hovered row gets a dimmer one.
    let bg = if is_selected {
        Some(theme::BORDER_DIM)
    } else if is_hovered {
        Some(theme::HOVER_BG)
    } else {
        None
    };
    if let Some(bg_color) = bg {
        for span in spans.iter_mut() {
            if !matches!(span.content.as_ref(), "│") {
                span.style = span.style.bg(bg_color);
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
/// Content returned by `entry_info_lines`: the visual lines plus a
/// mapping of which metadata fields appear at which line indices (for
/// registering click targets).
struct InfoContent {
    lines: Vec<Vec<Span<'static>>>,
    /// (field, line_index) for each clickable metadata row. Fields
    /// that are absent but have a "(click to add)" placeholder also
    /// appear here.
    meta_field_rows: Vec<(MetadataField, usize)>,
    /// Line index of the analyze pill (if present).
    analyze_pill_row: Option<usize>,
}

fn draw_browse_info(
    f: &mut Frame,
    area: Rect,
    browse: &BrowseState,
    buttons: &mut ButtonRenderMap,
    hover: Option<super::button_map::TuiButton>,
) {
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
    let analyze_hovered = hover == Some(super::button_map::TuiButton::BrowseInfoAnalyze);
    let info = if let Some(entry) = browse.selected_entry() {
        entry_info_lines(entry, browse, content_width, analyze_hovered)
    } else {
        InfoContent {
            lines: vec![vec![Span::styled("   (no selection)", theme::muted())]],
            meta_field_rows: Vec::new(),
            analyze_pill_row: None,
        }
    };

    // Render content lines with border
    for line_spans in info.lines.iter().take(content_height) {
        lines.push(bordered_line(border_color, w, line_spans.clone()));
    }

    // Fill remaining
    let rendered = info.lines.len().min(content_height);
    for _ in rendered..content_height {
        lines.push(bordered_line(border_color, w, vec![]));
    }

    lines.push(bot_line);

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);

    // Register clickable metadata fields in the info pane. Only for
    // lines that fit within the visible content area.
    let info_y_start = area.y + 1; // below top border
    for (field, line_idx) in &info.meta_field_rows {
        if *line_idx < content_height {
            buttons.record_button(
                TuiButton::BrowseInfoMeta(*field),
                Rect::new(area.x + 1, info_y_start + *line_idx as u16, (w - 2) as u16, 1),
            );
        }
    }

    // Register the analyze pill button if it fits.
    if let Some(row) = info.analyze_pill_row {
        if row < content_height {
            buttons.record_button(
                TuiButton::BrowseInfoAnalyze,
                Rect::new(area.x + 1, info_y_start + row as u16, (w - 2) as u16, 1),
            );
        }
    }
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
/// Returns `InfoContent` with both the visual lines and a mapping of
/// clickable metadata field positions.
fn entry_info_lines(
    entry: &BrowseEntry,
    browse: &BrowseState,
    content_width: usize,
    analyze_hovered: bool,
) -> InfoContent {
    // Maximum width for free-form text values: subtract the 3-space indent
    let max_value_chars = content_width.saturating_sub(3);

    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    let mut meta_field_rows: Vec<(MetadataField, usize)> = Vec::new();

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
            // Show directory stats if cached, or "computing…" if a stats
            // task is currently in flight for this directory.
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
            } else if browse.dir_stats_pending.contains(&entry.path) {
                lines.push(vec![
                    Span::styled("   files   ", theme::muted()),
                    Span::styled("computing…", Style::default().fg(theme::TEXT_DIM)),
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

                // Metadata tags — always show the section (with placeholders
                // for absent fields) so users can click to add new tags.
                let meta = &cached.metadata;
                {
                    let inline_max = max_value_chars.saturating_sub(11);
                    lines.push(vec![]);

                    // Title: 2-line layout (label + value). Clickable on value row.
                    lines.push(vec![Span::styled("   title", theme::muted())]);
                    let title_value_row = lines.len();
                    if let Some(title) = &meta.title {
                        lines.push(vec![
                            Span::raw("   "),
                            Span::styled(truncate_to(title, max_value_chars), theme::bright()),
                        ]);
                    } else {
                        lines.push(vec![Span::styled(
                            "   (click to add)",
                            Style::default().fg(theme::TEXT_DIM),
                        )]);
                    }
                    meta_field_rows.push((MetadataField::Title, title_value_row));

                    // Artist: inline label + value. Clickable on the whole line.
                    let artist_row = lines.len();
                    if let Some(artist) = &meta.artist {
                        lines.push(vec![
                            Span::styled("   artist  ", theme::muted()),
                            Span::styled(truncate_to(artist, inline_max), theme::text()),
                        ]);
                    } else {
                        lines.push(vec![
                            Span::styled("   artist  ", theme::muted()),
                            Span::styled("(click to add)", Style::default().fg(theme::TEXT_DIM)),
                        ]);
                    }
                    meta_field_rows.push((MetadataField::Artist, artist_row));

                    // Album
                    let album_row = lines.len();
                    if let Some(album) = &meta.album {
                        lines.push(vec![
                            Span::styled("   album   ", theme::muted()),
                            Span::styled(truncate_to(album, inline_max), theme::text()),
                        ]);
                    } else {
                        lines.push(vec![
                            Span::styled("   album   ", theme::muted()),
                            Span::styled("(click to add)", Style::default().fg(theme::TEXT_DIM)),
                        ]);
                    }
                    meta_field_rows.push((MetadataField::Album, album_row));

                    // Genre
                    let genre_row = lines.len();
                    if let Some(genre) = &meta.genre {
                        lines.push(vec![
                            Span::styled("   genre   ", theme::muted()),
                            Span::styled(truncate_to(genre, inline_max), theme::text()),
                        ]);
                    } else {
                        lines.push(vec![
                            Span::styled("   genre   ", theme::muted()),
                            Span::styled("(click to add)", Style::default().fg(theme::TEXT_DIM)),
                        ]);
                    }
                    meta_field_rows.push((MetadataField::Genre, genre_row));

                    // Year
                    let year_row = lines.len();
                    if let Some(year) = &meta.year {
                        lines.push(vec![
                            Span::styled("   year    ", theme::muted()),
                            Span::styled(truncate_to(year, inline_max), theme::text()),
                        ]);
                    } else {
                        lines.push(vec![
                            Span::styled("   year    ", theme::muted()),
                            Span::styled("(click to add)", Style::default().fg(theme::TEXT_DIM)),
                        ]);
                    }
                    meta_field_rows.push((MetadataField::Year, year_row));
                }

                // ReplayGain / R128 section. Only render if at least one
                // gain field is present. The label clarifies the type.
                let has_rg = meta.rg_track_gain.is_some()
                    || meta.rg_album_gain.is_some()
                    || meta.rg_track_peak.is_some()
                    || meta.rg_album_peak.is_some();
                let has_r128 = meta.r128_track_gain.is_some() || meta.r128_album_gain.is_some();
                if has_rg || has_r128 {
                    lines.push(vec![]);
                    let label = match (has_rg, has_r128) {
                        (true, true) => "replaygain + r128",
                        (true, false) => match (
                            meta.rg_track_gain.is_some(),
                            meta.rg_album_gain.is_some(),
                        ) {
                            (true, true) => "replaygain (track + album)",
                            (false, true) => "replaygain (album)",
                            _ => "replaygain (track)",
                        },
                        (false, true) => "r128",
                        _ => "loudness",
                    };
                    lines.push(vec![Span::styled(
                        format!("   {}", label),
                        theme::muted(),
                    )]);

                    let inline_max = max_value_chars.saturating_sub(11);

                    if let Some(g) = &meta.rg_track_gain {
                        lines.push(vec![
                            Span::styled("   tk gain ", theme::muted()),
                            Span::styled(truncate_to(g, inline_max), theme::text()),
                        ]);
                    }
                    if let Some(p) = &meta.rg_track_peak {
                        lines.push(vec![
                            Span::styled("   tk peak ", theme::muted()),
                            Span::styled(truncate_to(p, inline_max), theme::text()),
                        ]);
                    }
                    if let Some(g) = &meta.rg_album_gain {
                        lines.push(vec![
                            Span::styled("   al gain ", theme::muted()),
                            Span::styled(truncate_to(g, inline_max), theme::text()),
                        ]);
                    }
                    if let Some(p) = &meta.rg_album_peak {
                        lines.push(vec![
                            Span::styled("   al peak ", theme::muted()),
                            Span::styled(truncate_to(p, inline_max), theme::text()),
                        ]);
                    }
                    if let Some(g) = &meta.r128_track_gain {
                        lines.push(vec![
                            Span::styled("   r128 tk ", theme::muted()),
                            Span::styled(truncate_to(g, inline_max), theme::text()),
                        ]);
                    }
                    if let Some(g) = &meta.r128_album_gain {
                        lines.push(vec![
                            Span::styled("   r128 al ", theme::muted()),
                            Span::styled(truncate_to(g, inline_max), theme::text()),
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

            // Analyze pill — right-aligned, purple (brighter on hover).
            lines.push(vec![]);
            let analyze_row = lines.len();
            let pill_label = " analyze ";
            let pill_w = pill_label.chars().count();
            let pad = content_width.saturating_sub(pill_w + 3);
            let pill_bg = if analyze_hovered {
                theme::BLUE
            } else {
                theme::PURPLE
            };
            lines.push(vec![
                Span::raw(" ".repeat(pad)),
                Span::styled(
                    pill_label,
                    Style::default()
                        .fg(theme::PILL_ACTIVE_FG)
                        .bg(pill_bg)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
            ]);
            return InfoContent {
                lines,
                meta_field_rows,
                analyze_pill_row: Some(analyze_row),
            };
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

    // Symlink indicator (applies to all entry kinds).
    if entry.is_symlink {
        lines.push(vec![]);
        let (label, color) = if entry.is_broken_symlink {
            ("symlink (broken)", theme::RED)
        } else {
            ("symlink", theme::AMBER)
        };
        lines.push(vec![
            Span::styled("   ", theme::muted()),
            Span::styled(label, Style::default().fg(color)),
        ]);
    }

    InfoContent {
        lines,
        meta_field_rows,
        analyze_pill_row: None,
    }
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
