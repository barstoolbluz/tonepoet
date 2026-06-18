//! Browse screen: file browser with directory tree + info pane

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
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
            Constraint::Length(7), // header banner
            Constraint::Length(3), // breadcrumb (bordered)
            Constraint::Min(10),   // main content (list + info)
            Constraint::Length(2), // footer (tabs + context)
        ])
        .split(area);

    draw_header(f, chunks[0]);
    draw_breadcrumb(f, chunks[1], &app.browse);
    app.button_map.record_button(TuiButton::BrowseBreadcrumb, chunks[1]);

    // Split main content horizontally: list (2/3) + info (1/3)
    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(66), Constraint::Percentage(34)])
        .split(chunks[2]);

    let list_area = content_chunks[0];
    let hover = app.hover_target;
    draw_browse_list(f, list_area, &mut app.browse, hover);
    draw_browse_info(
        f,
        content_chunks[1],
        &app.browse,
        &mut app.button_map,
        hover,
    );

    let status_msg = app.status_message.as_ref().map(|(s, _)| s.as_str());
    draw_footer(
        f,
        chunks[3],
        app.current_screen,
        &mut app.button_map,
        status_msg,
    );

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
    let name_w =
        inner_w - ROW_PREFIX - ROW_TRAILING - COL_SIZE_W - COL_DATE_W - COL_TYPE_W - COL_GAPS;

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

    // Search toggle in the top border (right-aligned "search" label).
    {
        let search_label_w = if browse.search.active { 10u16 } else { 8u16 }; // " search ✓ " or " search "
        let search_x = area.x + area.width - search_label_w - 1;
        buttons.record_button(
            TuiButton::BrowseSearchToggle,
            Rect::new(search_x, area.y, search_label_w, 1),
        );
    }

    // Search panel toggle pills (if search is active, rows 2-3 inside the border).
    if browse.search.active {
        let panel_y = area.y + 2; // row after column headers
                                  // Recursive pill: right side of row 1 (width varies with state)
        let rec_w = if browse.search.recursive {
            13u16
        } else {
            11u16
        };
        let rec_x = area.x + area.width - rec_w - 1;
        buttons.record_button(
            TuiButton::BrowseSearchRecursive,
            Rect::new(rec_x, panel_y, rec_w, 1),
        );

        // Mode, Sort, and AudioOnly: row 2 — widths must match draw code.
        let panel_y2 = panel_y + 1;
        // mode: " mode: <label> " — all ASCII, .len() == display width
        let mode_w = (7 + browse.search.mode.label().len() + 1) as u16;
        buttons.record_button(
            TuiButton::BrowseSearchMode,
            Rect::new(area.x + 1, panel_y2, mode_w, 1),
        );
        // sort: " sort: <label> ▲ " — arrow is 1 display col
        let sort_w = (7 + browse.search.sort.label().len() + 1 + 1 + 1) as u16;
        let sort_x = area.x + 1 + mode_w + 1;
        buttons.record_button(
            TuiButton::BrowseSearchSort,
            Rect::new(sort_x, panel_y2, sort_w, 1),
        );
        // audio: " audio ✓ " (9) or " all files " (11)
        let audio_w = if browse.search.audio_only {
            9u16
        } else {
            11u16
        };
        let audio_x = sort_x + sort_w + 1;
        buttons.record_button(
            TuiButton::BrowseSearchAudioOnly,
            Rect::new(audio_x, panel_y2, audio_w, 1),
        );
    }

    // Entry rows: below header (and search panel if active), above bottom border.
    let search_rows = if browse.search.active { 2u16 } else { 0 };
    let entry_y_start = area.y + 2 + search_rows;
    let content_height = (area.height as usize).saturating_sub(3 + search_rows as usize);
    let start = browse.scroll_offset;
    let end = (start + content_height).min(browse.entries.len());
    for (row, i) in (start..end).enumerate() {
        let y = entry_y_start + row as u16;
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

    let border_color = if browse.path_input.is_some() {
        theme::BLUE
    } else {
        theme::BORDER_DIM
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.width < 4 {
        return;
    }

    // Editable path input mode
    if let Some(ref input) = browse.path_input {
        let prefix = " path: ";
        let prefix_w = prefix.chars().count();
        let input_max = (inner.width as usize).saturating_sub(prefix_w).saturating_sub(1);
        let cursor_char_pos = input.text[..input.cursor].chars().count();

        let scroll_offset = if cursor_char_pos > input_max {
            cursor_char_pos - input_max
        } else {
            0
        };
        let visible: String = input.text.chars().skip(scroll_offset).take(input_max).collect();
        let cursor_in_visible = cursor_char_pos.saturating_sub(scroll_offset);

        let text_style = if input.select_all {
            Style::default().fg(theme::BG).bg(Color::Rgb(180, 190, 210))
        } else {
            theme::bright()
        };
        let spans = vec![
            Span::styled(prefix, Style::default().fg(theme::BLUE)),
            Span::styled(visible, text_style),
        ];
        let line = Paragraph::new(Line::from(spans));
        f.render_widget(line, inner);

        let cursor_x = inner.x + prefix_w as u16 + cursor_in_visible as u16;
        if cursor_x < inner.x + inner.width {
            f.set_cursor(cursor_x, inner.y);
        }
        return;
    }

    // Read-only display mode
    let display = if let Some(ref arc) = browse.archive {
        let archive_name = arc
            .listing
            .archive_path
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

    let filter_suffix = if !browse.filter_text.is_empty() {
        format!("   filter: {}", browse.filter_text)
    } else {
        String::new()
    };

    let type_ahead_suffix = if browse.type_ahead_active() {
        format!("   jump: {}", browse.type_ahead_buffer)
    } else {
        String::new()
    };

    let prefix = " path: ";
    let prefix_w = prefix.chars().count();
    let suffix_w = filter_suffix.chars().count() + type_ahead_suffix.chars().count();
    let path_max = (inner.width as usize)
        .saturating_sub(prefix_w)
        .saturating_sub(suffix_w)
        .saturating_sub(1);
    let display_truncated = truncate_left(&display, path_max);

    let mut spans = vec![
        Span::styled(prefix, theme::muted()),
        Span::styled(display_truncated, theme::bright()),
    ];
    if !filter_suffix.is_empty() {
        spans.push(Span::styled(
            filter_suffix,
            Style::default().fg(theme::AMBER),
        ));
    }
    if !type_ahead_suffix.is_empty() {
        spans.push(Span::styled(
            type_ahead_suffix,
            Style::default().fg(theme::CYAN),
        ));
    }

    let line = Paragraph::new(Line::from(spans));
    f.render_widget(line, inner);
}

/// Truncate a string from the LEFT to fit `max` chars, prepending `…` if cut.
/// Used so the end of paths (most contextual portion) stays visible.
fn truncate_left(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max < 2 {
        return s
            .chars()
            .rev()
            .take(max)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
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
    let search_label = if browse.search.active {
        " search ✓ "
    } else {
        " search "
    };
    let search_display_w = search_label.chars().count();
    // ┌ + title + dashes + search_label + ┐ = w
    let dash_count = w.saturating_sub(1 + title.len() + search_display_w + 1);

    let search_style = if browse.search.active {
        Style::default()
            .fg(theme::GREEN)
            .add_modifier(ratatui::style::Modifier::BOLD)
    } else {
        theme::border(border_color)
    };
    let top_line = Line::from(vec![
        Span::styled("┌", theme::border(border_color)),
        Span::styled(title, theme::border(border_color)),
        Span::styled("─".repeat(dash_count), theme::border(border_color)),
        Span::styled(search_label, search_style),
        Span::styled("┐", theme::border(border_color)),
    ]);

    let bot_line = Line::from(Span::styled(
        format!("└{}┘", "─".repeat(w.saturating_sub(2))),
        theme::border(border_color),
    ));

    // Content rows = total - top border - header - bottom border
    // (-1 if filter row, -2 if search panel).
    let has_filter = browse.filter_input.is_some();
    let has_search = browse.search.active;
    let reserved = if has_search {
        5 // top border + header + 2 search rows + bottom border
    } else if has_filter {
        4
    } else {
        3
    };
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

    // Search panel (2 rows when active).
    if browse.search.active {
        // Row 1: search input + [recursive] toggle
        // Layout: │ + " / "(3) + input(input_w) + gap(≥1) + rec_pill(pill_w) + │
        let rec_pill = if browse.search.recursive {
            Span::styled(
                " recursive ✓ ",
                Style::default()
                    .fg(theme::PILL_ACTIVE_FG)
                    .bg(theme::GREEN)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            )
        } else {
            Span::styled(
                " recursive ",
                Style::default().fg(theme::TEXT_DIM).bg(theme::SURFACE),
            )
        };
        let pill_w = rec_pill.width();
        let input_w = inner_w.saturating_sub(3 + 1 + pill_w); // " / " + gap + pill
        let (view, _cursor_col) = browse.search.input.view(input_w);
        // Pad view to input_w so the pill stays right-aligned.
        let view_len = view.chars().count();
        let padded = if view.is_empty() {
            " ".repeat(input_w.max(1))
        } else {
            let pad = input_w.saturating_sub(view_len);
            format!("{}{}", view, " ".repeat(pad))
        };
        let search_pad = inner_w.saturating_sub(3 + input_w + pill_w);
        lines.push(Line::from(vec![
            Span::styled("│", theme::border(border_color)),
            Span::styled(" / ", Style::default().fg(theme::AMBER)),
            Span::styled(
                padded,
                Style::default().fg(theme::TEXT_BRIGHT).bg(theme::SURFACE),
            ),
            Span::raw(" ".repeat(search_pad.max(1))),
            rec_pill,
            Span::styled("│", theme::border(border_color)),
        ]));

        // Row 2: mode cycle + sort cycle + [audio] toggle
        let mode_label = format!(" mode: {} ", browse.search.mode.label());
        let sort_arrow = match browse.search.sort_dir {
            super::browse::SortDir::Asc => "▲",
            super::browse::SortDir::Desc => "▼",
        };
        let sort_label = format!(" sort: {} {} ", browse.search.sort.label(), sort_arrow);
        let audio_pill = if browse.search.audio_only {
            Span::styled(
                " audio ✓ ",
                Style::default()
                    .fg(theme::PILL_ACTIVE_FG)
                    .bg(theme::GREEN)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            )
        } else {
            Span::styled(
                " all files ",
                Style::default().fg(theme::TEXT_DIM).bg(theme::SURFACE),
            )
        };

        let sort_display_w = sort_label.chars().count();
        // Inner content: mode + gap + sort + gap + audio (no borders counted).
        let row2_content = mode_label.len() + 1 + sort_display_w + 1 + audio_pill.width();
        let row2_pad = inner_w.saturating_sub(row2_content);
        lines.push(Line::from(vec![
            Span::styled("│", theme::border(border_color)),
            Span::styled(mode_label, Style::default().fg(theme::CYAN)),
            Span::raw(" "),
            Span::styled(sort_label, Style::default().fg(theme::AMBER)),
            Span::raw(" "),
            audio_pill,
            Span::raw(" ".repeat(row2_pad)),
            Span::styled("│", theme::border(border_color)),
        ]));
    }

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
            let is_hovered =
                !is_selected && hover == Some(super::button_map::TuiButton::BrowseEntry(i));
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

    // Position the terminal cursor inside the search input or filter input.
    if browse.search.active && browse.search.focus == super::browse::SearchFocus::Input {
        let pill_w = if browse.search.recursive { 13 } else { 11 };
        let input_w = inner_w.saturating_sub(3 + 1 + pill_w);
        let (_, cursor_col) = browse.search.input.view(input_w);
        let cursor_x = area.x + 1 + 3 + cursor_col; // border + " / " prefix
        let cursor_y = area.y + 2; // top border + header + first search row
        f.set_cursor(cursor_x, cursor_y);
    } else if let Some(col_in_view) = filter_cursor {
        let cursor_x = area.x + 1 + 3 + col_in_view;
        let cursor_y = area.y + area.height - 2;
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

    let header_cell =
        |label: &'static str, col: SortBy, col_w: usize, right_align: bool| -> Vec<Span<'static>> {
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
                vec![Span::raw(" ".repeat(pad)), Span::styled(text, style)]
            } else {
                vec![Span::styled(text, style), Span::raw(" ".repeat(pad))]
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
            EntryKind::DvdAudioDir | EntryKind::DvdVideoDir => Style::default().fg(theme::PURPLE),
            EntryKind::AudioFile(_) => {
                if is_selected {
                    Style::default()
                        .fg(theme::TEXT_BRIGHT)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::TEXT)
                }
            }
            EntryKind::Archive => Style::default().fg(theme::AMBER),
            EntryKind::SacdIso | EntryKind::DvdAudioIso | EntryKind::DvdVideoIso => Style::default().fg(theme::PURPLE),
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
    /// Line index of the edit tags pill (if present).
    edit_tags_pill_row: Option<usize>,
    /// Line index of the Audio Streams pill (if present).
    audio_streams_pill_row: Option<usize>,
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
    let edit_tags_hovered = hover == Some(super::button_map::TuiButton::BrowseInfoEditTags);
    let audio_streams_hovered = hover == Some(super::button_map::TuiButton::BrowseInfoAudioStreams);
    let info = if let Some(entry) = browse.selected_entry() {
        entry_info_lines(
            entry,
            browse,
            content_width,
            analyze_hovered,
            edit_tags_hovered,
            audio_streams_hovered,
        )
    } else {
        InfoContent {
            lines: vec![vec![Span::styled("   (no selection)", theme::muted())]],
            meta_field_rows: Vec::new(),
            analyze_pill_row: None,
            edit_tags_pill_row: None,
            audio_streams_pill_row: None,
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
                Rect::new(
                    area.x + 1,
                    info_y_start + *line_idx as u16,
                    (w - 2) as u16,
                    1,
                ),
            );
        }
    }

    // Register pill buttons if they fit.
    if let Some(row) = info.analyze_pill_row {
        if row < content_height {
            buttons.record_button(
                TuiButton::BrowseInfoAnalyze,
                Rect::new(area.x + 1, info_y_start + row as u16, (w - 2) as u16, 1),
            );
        }
    }
    if let Some(row) = info.edit_tags_pill_row {
        if row < content_height {
            buttons.record_button(
                TuiButton::BrowseInfoEditTags,
                Rect::new(area.x + 1, info_y_start + row as u16, (w - 2) as u16, 1),
            );
        }
    }
    if let Some(row) = info.audio_streams_pill_row {
        if row < content_height {
            buttons.record_button(
                TuiButton::BrowseInfoAudioStreams,
                Rect::new(area.x + 1, info_y_start + row as u16, (w - 2) as u16, 1),
            );
        }
    }
}

/// Truncate a string to fit within `max_chars` columns, adding ellipsis if needed.
#[allow(dead_code)]
pub(crate) fn truncate_for_disc_overlay(s: &str, max_chars: usize) -> String { truncate_to(s, max_chars) }

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
    edit_tags_hovered: bool,
    audio_streams_hovered: bool,
) -> InfoContent {
    // Maximum width for free-form text values: subtract the 3-space indent
    let max_value_chars = content_width.saturating_sub(3);

    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    let mut meta_field_rows: Vec<(MetadataField, usize)> = Vec::new();
    // Pill rows set by branches that emit them after their content.
    // SacdIso is the only branch using this besides AudioFile (which
    // returns early), but the pattern generalises if more arms grow
    // pill rendering.
    let mut sacd_edit_tags_row: Option<usize> = None;
    let mut audio_streams_pill_row: Option<usize> = None;

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
            #[allow(unused_assignments)]
            let mut analyze_row = 0usize;
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

                // Pre-emphasis — show if metadata evidence detected.
                if let Some(ref pe) = cached.metadata.preemphasis_metadata {
                    lines.push(vec![
                        Span::styled("   pre-emph", theme::muted()),
                        Span::raw(" "),
                        Span::styled(
                            truncate_to(
                                &format!("detected ({})", pe),
                                max_value_chars.saturating_sub(11),
                            ),
                            Style::default().fg(theme::RED),
                        ),
                    ]);
                }

                // HDCD — shown if previously analyzed and detected.
                // "HDCD" in the value text rendered gold.
                if let Some(ref hdcd) = cached.metadata.hdcd_detail {
                    let val_max = max_value_chars.saturating_sub(11);
                    let mut spans = vec![Span::styled("   HDCD    ", theme::muted())];
                    if let Some(rest) = hdcd.strip_prefix("HDCD") {
                        spans.push(Span::styled(
                            "HDCD",
                            Style::default()
                                .fg(theme::AMBER)
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        ));
                        spans.push(Span::styled(
                            truncate_to(rest, val_max.saturating_sub(4)),
                            theme::text(),
                        ));
                    } else {
                        spans.push(Span::styled(truncate_to(hdcd, val_max), theme::text()));
                    }
                    lines.push(spans);
                }

                // ReplayGain / R128 — shown with technical info since
                // these are measurement data, not user-editable metadata.
                let meta = &cached.metadata;
                let has_rg = meta.rg_track_gain.is_some()
                    || meta.rg_album_gain.is_some()
                    || meta.rg_track_peak.is_some()
                    || meta.rg_album_peak.is_some();
                let has_r128 = meta.r128_track_gain.is_some() || meta.r128_album_gain.is_some();
                if has_rg || has_r128 {
                    lines.push(vec![]);
                    let label = match (has_rg, has_r128) {
                        (true, true) => "replaygain + r128",
                        (true, false) => {
                            match (meta.rg_track_gain.is_some(), meta.rg_album_gain.is_some()) {
                                (true, true) => "replaygain (track + album)",
                                (false, true) => "replaygain (album)",
                                _ => "replaygain (track)",
                            }
                        }
                        (false, true) => "r128",
                        _ => "loudness",
                    };
                    lines.push(vec![Span::styled(format!("   {}", label), theme::muted())]);

                    let rg_inline_max = max_value_chars.saturating_sub(11);
                    if let Some(g) = &meta.rg_track_gain {
                        lines.push(vec![
                            Span::styled("   tk gain ", theme::muted()),
                            Span::styled(truncate_to(g, rg_inline_max), theme::text()),
                        ]);
                    }
                    if let Some(p) = &meta.rg_track_peak {
                        lines.push(vec![
                            Span::styled("   tk peak ", theme::muted()),
                            Span::styled(truncate_to(p, rg_inline_max), theme::text()),
                        ]);
                    }
                    if let Some(g) = &meta.rg_album_gain {
                        lines.push(vec![
                            Span::styled("   al gain ", theme::muted()),
                            Span::styled(truncate_to(g, rg_inline_max), theme::text()),
                        ]);
                    }
                    if let Some(p) = &meta.rg_album_peak {
                        lines.push(vec![
                            Span::styled("   al peak ", theme::muted()),
                            Span::styled(truncate_to(p, rg_inline_max), theme::text()),
                        ]);
                    }
                    if let Some(g) = &meta.r128_track_gain {
                        lines.push(vec![
                            Span::styled("   r128 tk ", theme::muted()),
                            Span::styled(truncate_to(g, rg_inline_max), theme::text()),
                        ]);
                    }
                    if let Some(g) = &meta.r128_album_gain {
                        lines.push(vec![
                            Span::styled("   r128 al ", theme::muted()),
                            Span::styled(truncate_to(g, rg_inline_max), theme::text()),
                        ]);
                    }
                }

                // Analyze pill — after technical info + RG, before metadata.
                lines.push(vec![]);
                analyze_row = lines.len();
                let analyze_label = " analyze ";
                let analyze_w = analyze_label.chars().count();
                let analyze_pad = content_width.saturating_sub(analyze_w + 3);
                let analyze_bg = if analyze_hovered {
                    theme::BLUE
                } else {
                    theme::PURPLE
                };
                lines.push(vec![
                    Span::raw(" ".repeat(analyze_pad)),
                    Span::styled(
                        analyze_label,
                        Style::default()
                            .fg(theme::PILL_ACTIVE_FG)
                            .bg(analyze_bg)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                ]);

                // Metadata tags — always show the section (with placeholders
                // for absent fields) so users can click to add new tags.
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

                // Analyze pill after basic info.
                lines.push(vec![]);
                let analyze_row_unprobed = lines.len();
                let a_label = " analyze ";
                let a_w = a_label.chars().count();
                let a_pad = content_width.saturating_sub(a_w + 3);
                let a_bg = if analyze_hovered {
                    theme::BLUE
                } else {
                    theme::PURPLE
                };
                lines.push(vec![
                    Span::raw(" ".repeat(a_pad)),
                    Span::styled(
                        a_label,
                        Style::default()
                            .fg(theme::PILL_ACTIVE_FG)
                            .bg(a_bg)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                ]);
                // Return early — no metadata to show.
                lines.push(vec![]);
                let et_row = lines.len();
                let et_label = " edit tags ";
                let et_w2 = et_label.chars().count();
                let et_pad2 = content_width.saturating_sub(et_w2 + 3);
                let et_bg2 = if edit_tags_hovered {
                    theme::BLUE
                } else {
                    theme::PURPLE
                };
                lines.push(vec![
                    Span::raw(" ".repeat(et_pad2)),
                    Span::styled(
                        et_label,
                        Style::default()
                            .fg(theme::PILL_ACTIVE_FG)
                            .bg(et_bg2)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                ]);
                return InfoContent {
                    lines,
                    meta_field_rows,
                    analyze_pill_row: Some(analyze_row_unprobed),
                    edit_tags_pill_row: Some(et_row),
                    audio_streams_pill_row,
                };
            }

            // Edit tags pill — after metadata/RG section.
            lines.push(vec![]);
            let edit_tags_row = lines.len();
            let et_label = " edit tags ";
            let et_w = et_label.chars().count();
            let et_pad = content_width.saturating_sub(et_w + 3);
            let et_bg = if edit_tags_hovered {
                theme::BLUE
            } else {
                theme::PURPLE
            };
            lines.push(vec![
                Span::raw(" ".repeat(et_pad)),
                Span::styled(
                    et_label,
                    Style::default()
                        .fg(theme::PILL_ACTIVE_FG)
                        .bg(et_bg)
                        .add_modifier(ratatui::style::Modifier::BOLD),
                ),
            ]);
            return InfoContent {
                lines,
                meta_field_rows,
                analyze_pill_row: Some(analyze_row),
                edit_tags_pill_row: Some(edit_tags_row),
                audio_streams_pill_row,
            };
        }
        EntryKind::DvdAudioIso | EntryKind::DvdAudioDir | EntryKind::DvdVideoIso | EntryKind::DvdVideoDir => {
            lines.push(vec![
                Span::styled("   kind    ", theme::muted()),
                Span::styled(
                    if matches!(entry.kind, EntryKind::DvdVideoDir) { "DVD-Video directory" } else if matches!(entry.kind, EntryKind::DvdVideoIso) { "DVD-Video ISO" } else if matches!(entry.kind, EntryKind::DvdAudioDir) { "DVD-Audio directory" } else { "DVD-Audio ISO" },
                    theme::bold(theme::PURPLE),
                ),
            ]);
            if let Some(contents) = browse
                .disc_probe_cache
                .get(&entry.path)
                .and_then(|cache| cache.contents_if_current(&entry.path))
            {
                lines.push(vec![
                    Span::styled("   streams ", theme::muted()),
                    Span::styled(crate::tui::disc_browser::disc_summary(contents.as_ref()), theme::text()),
                ]);
                lines.push(vec![
                    Span::styled("   copy protection", theme::muted()),
                    Span::raw(" "),
                    Span::styled(
                        truncate_to(&contents.copy_protection.description, max_value_chars.saturating_sub(18)),
                        theme::text(),
                    ),
                ]);
                lines.push(vec![]);
                for (idx, presentation) in contents.presentations.iter().enumerate().take(6) {
                    lines.push(vec![
                        Span::styled("   ", theme::muted()),
                        Span::styled(
                            truncate_to(&crate::tui::disc_browser::presentation_summary(idx, presentation), max_value_chars),
                            theme::text(),
                        ),
                    ]);
                }
                if contents.presentations.len() > 6 {
                    lines.push(vec![Span::styled(
                        format!("   … {} more audio streams", contents.presentations.len() - 6),
                        theme::muted(),
                    )]);
                }
                if contents.presentations.len() >= 2 {
                    lines.push(vec![]);
                    let row = lines.len();
                    let label = " audio streams ";
                    let width = label.chars().count();
                    let pad = content_width.saturating_sub(width + 3);
                    let bg = if audio_streams_hovered { theme::BLUE } else { theme::PURPLE };
                    lines.push(vec![
                        Span::raw(" ".repeat(pad)),
                        Span::styled(
                            label,
                            Style::default()
                                .fg(theme::PILL_ACTIVE_FG)
                                .bg(bg)
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        ),
                    ]);
                    audio_streams_pill_row = Some(row);
                }
            } else if let Some(error) = browse
                .disc_probe_cache
                .get(&entry.path)
                .and_then(|cache| cache.error_if_current(&entry.path))
            {
                lines.push(vec![
                    Span::styled("   status  ", theme::muted()),
                    Span::styled(truncate_to(error, max_value_chars.saturating_sub(10)), Style::default().fg(theme::RED)),
                ]);
                lines.push(vec![Span::styled("   size    ", theme::muted()), Span::styled(size_str(entry.size), theme::text())]);
            } else {
                lines.push(vec![Span::styled("   status  ", theme::muted()), Span::styled("Analyzing disc…", theme::muted())]);
                lines.push(vec![Span::styled("   size    ", theme::muted()), Span::styled(size_str(entry.size), theme::text())]);
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
        EntryKind::SacdIso => {
            lines.push(vec![
                Span::styled("   kind    ", theme::muted()),
                Span::styled("SACD ISO (ScarletBook)", theme::text()),
            ]);
            if let Some(cached) = browse.current_cached_info() {
                let info = &cached.source;
                lines.push(vec![
                    Span::styled("   format  ", theme::muted()),
                    Span::styled(info.format_name.clone(), theme::bold(theme::PURPLE)),
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

                // Album-level metadata block (from sidecar overlay
                // when present, ScarletBook fallback otherwise).
                let meta = &cached.metadata;
                let inline_max = max_value_chars.saturating_sub(11);
                let has_any = meta.album.is_some()
                    || meta.artist.is_some()
                    || meta.genre.is_some()
                    || meta.year.is_some()
                    || meta.catalog_number.is_some();
                if has_any {
                    lines.push(vec![]);
                    if let Some(s) = &meta.artist {
                        lines.push(vec![
                            Span::styled("   artist  ", theme::muted()),
                            Span::styled(truncate_to(s, inline_max), theme::text()),
                        ]);
                    }
                    if let Some(s) = &meta.album {
                        lines.push(vec![
                            Span::styled("   album   ", theme::muted()),
                            Span::styled(truncate_to(s, inline_max), theme::text()),
                        ]);
                    }
                    if let Some(s) = &meta.genre {
                        lines.push(vec![
                            Span::styled("   genre   ", theme::muted()),
                            Span::styled(truncate_to(s, inline_max), theme::text()),
                        ]);
                    }
                    if let Some(s) = &meta.year {
                        lines.push(vec![
                            Span::styled("   year    ", theme::muted()),
                            Span::styled(truncate_to(s, inline_max), theme::text()),
                        ]);
                    }
                    if let Some(s) = &meta.catalog_number {
                        lines.push(vec![
                            Span::styled("   catalog ", theme::muted()),
                            Span::styled(truncate_to(s, inline_max), theme::text()),
                        ]);
                    }
                }

                if let Some(contents) = browse
                    .disc_probe_cache
                    .get(&entry.path)
                    .and_then(|cache| cache.contents_if_current(&entry.path))
                {
                    if contents.presentations.len() >= 2 {
                        lines.push(vec![]);
                        let row = lines.len();
                        let label = " audio streams ";
                        let width = label.chars().count();
                        let pad = content_width.saturating_sub(width + 3);
                        let bg = if audio_streams_hovered { theme::BLUE } else { theme::PURPLE };
                        lines.push(vec![
                            Span::raw(" ".repeat(pad)),
                            Span::styled(
                                label,
                                Style::default()
                                    .fg(theme::PILL_ACTIVE_FG)
                                    .bg(bg)
                                    .add_modifier(ratatui::style::Modifier::BOLD),
                            ),
                        ]);
                        audio_streams_pill_row = Some(row);
                    }
                }
                // SACD stream summary uses the shared DiscContents cache when available.
                // Edit-tags pill — parity with the AudioFile arm so
                // SACD ISOs have a clickable mouse path to the
                // metadata editor (keyboard via :tags, context menu
                // via right-click already exist).
                lines.push(vec![]);
                let edit_tags_row = lines.len();
                let et_label = " edit tags ";
                let et_w = et_label.chars().count();
                let et_pad = content_width.saturating_sub(et_w + 3);
                let et_bg = if edit_tags_hovered {
                    theme::BLUE
                } else {
                    theme::PURPLE
                };
                lines.push(vec![
                    Span::raw(" ".repeat(et_pad)),
                    Span::styled(
                        et_label,
                        Style::default()
                            .fg(theme::PILL_ACTIVE_FG)
                            .bg(et_bg)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                ]);
                sacd_edit_tags_row = Some(edit_tags_row);
            } else {
                // Not yet probed (async probe pending) — fall back to
                // size only, but still emit the edit-tags pill so
                // the mouse path stays available during the probe
                // window (matches AudioFile arm's behaviour).
                lines.push(vec![
                    Span::styled("   size    ", theme::muted()),
                    Span::styled(size_str(entry.size), theme::text()),
                ]);
                lines.push(vec![]);
                let edit_tags_row = lines.len();
                let et_label = " edit tags ";
                let et_w = et_label.chars().count();
                let et_pad = content_width.saturating_sub(et_w + 3);
                let et_bg = if edit_tags_hovered {
                    theme::BLUE
                } else {
                    theme::PURPLE
                };
                lines.push(vec![
                    Span::raw(" ".repeat(et_pad)),
                    Span::styled(
                        et_label,
                        Style::default()
                            .fg(theme::PILL_ACTIVE_FG)
                            .bg(et_bg)
                            .add_modifier(ratatui::style::Modifier::BOLD),
                    ),
                ]);
                sacd_edit_tags_row = Some(edit_tags_row);
            }
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
        edit_tags_pill_row: sacd_edit_tags_row,
        audio_streams_pill_row,
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
