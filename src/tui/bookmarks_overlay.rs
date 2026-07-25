//! Responsive bookmark manager for the Browse screen.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use super::bookmarks::{
    BookmarkDetailState, BookmarkNaming, BookmarkTargetStatus, BookmarksState,
};
use super::button_map::{ButtonRenderMap, ScrollbarSurface, TuiButton};

const PREFERRED_WIDTH: u16 = 104;
const PREFERRED_HEIGHT: u16 = 30;
const TWO_COLUMN_MIN_WIDTH: u16 = 92;
const FULL_LAYOUT_MIN_INNER_WIDTH: u16 = 32;
const FULL_LAYOUT_MIN_INNER_HEIGHT: u16 = 8;
const DETAIL_PEEK_LIMIT: usize = 7;

fn bookmarks_overlay_fits(screen: Rect) -> bool {
    // At 8x6 the bordered overlay has a 6x4 interior: one input row,
    // one spacer, and two selectable rows. Smaller terminals cannot render a
    // minimally operable manager and are closed rather than left modal.
    screen.width >= 8 && screen.height >= 6
}

fn bookmarks_overlay_uses_compact_layout(screen: Rect) -> bool {
    let area = bookmarks_overlay_area(screen);
    let inner_width = area.width.saturating_sub(2);
    let inner_height = area.height.saturating_sub(2);
    inner_width < FULL_LAYOUT_MIN_INNER_WIDTH || inner_height < FULL_LAYOUT_MIN_INNER_HEIGHT
}

pub fn bookmarks_overlay_area(screen: Rect) -> Rect {
    let width = PREFERRED_WIDTH
        .min(screen.width.saturating_sub(2))
        .max(32.min(screen.width));
    let height = PREFERRED_HEIGHT
        .min(screen.height.saturating_sub(2))
        .max(12.min(screen.height));
    Rect::new(
        screen.x + screen.width.saturating_sub(width) / 2,
        screen.y + screen.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

pub fn draw_bookmarks_overlay(
    f: &mut Frame,
    state: &mut BookmarksState,
    current_dir: &std::path::Path,
    button_map: &mut ButtonRenderMap,
    theme: super::theme::Theme,
) {
    if !bookmarks_overlay_fits(f.size()) {
        state.close_overlay();
        return;
    }
    let area = bookmarks_overlay_area(f.size());

    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.green))
        .title(Span::styled(
            " ▾ bookmarks ",
            Style::default()
                .fg(theme.text_bright)
                .bg(theme.green)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Tiny terminals use a real list-only fallback rather than forcing the
    // full header/body/footer constraints into an impossible rectangle. The
    // compact renderer supplies one input row, selectable rows, a visible
    // feedback row, scrollbar geometry, and mouse targets.
    if bookmarks_overlay_uses_compact_layout(f.size()) {
        draw_compact_bookmarks_overlay(f, inner, state, button_map, theme);
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    draw_header(f, rows[0], state, current_dir, theme);

    let two_column = inner.width >= TWO_COLUMN_MIN_WIDTH;
    let body_columns = if two_column {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(43), Constraint::Length(2), Constraint::Percentage(57)])
            .split(rows[2])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(100), Constraint::Length(0), Constraint::Length(0)])
            .split(rows[2])
    };

    draw_bookmark_list(f, body_columns[0], state, button_map, theme);
    if two_column {
        draw_detail_card(f, body_columns[2], state, theme);
    }

    draw_footer(f, rows[3], theme);
    draw_feedback(f, rows[4], state, theme);
}

fn draw_header(
    f: &mut Frame,
    area: Rect,
    state: &BookmarksState,
    current_dir: &std::path::Path,
    theme: super::theme::Theme,
) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    let missing = state.missing_count();
    let mut left = vec![Span::styled(
        format!("{} bookmark{}", state.entries.len(), if state.entries.len() == 1 { "" } else { "s" }),
        Style::default().fg(theme.label),
    )];
    if missing > 0 {
        left.push(Span::styled(
            format!(" · {missing} missing"),
            Style::default()
                .fg(theme.destructive)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let unavailable = state.unavailable_count();
    if unavailable > 0 {
        left.push(Span::styled(
            format!(" · {unavailable} unavailable"),
            Style::default().fg(theme.amber),
        ));
    }
    if state.has_unknown_targets() {
        left.push(Span::styled(
            " · checking…",
            Style::default().fg(theme.text_dim),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(left)), columns[0]);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("in: ", Style::default().fg(theme.text_dim)),
            Span::styled(abbreviate_home(current_dir), Style::default().fg(theme.value)),
        ]))
        .alignment(Alignment::Right),
        columns[1],
    );
}

fn draw_bookmark_list(
    f: &mut Frame,
    area: Rect,
    state: &mut BookmarksState,
    button_map: &mut ButtonRenderMap,
    theme: super::theme::Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let input_area = Rect::new(area.x, area.y, area.width, 1);
    draw_input_row(f, input_area, state, theme);

    let list_area = Rect::new(
        area.x,
        area.y.saturating_add(2),
        area.width,
        area.height.saturating_sub(2),
    );
    draw_bookmark_rows(f, list_area, state, button_map, theme);
}

fn draw_compact_bookmarks_overlay(
    f: &mut Frame,
    inner: Rect,
    state: &mut BookmarksState,
    button_map: &mut ButtonRenderMap,
    theme: super::theme::Theme,
) {
    let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
    draw_input_row(f, input_area, state, theme);

    let feedback_height = u16::from(inner.height >= 3);
    let list_area = Rect::new(
        inner.x,
        inner.y.saturating_add(1),
        inner.width,
        inner.height.saturating_sub(1).saturating_sub(feedback_height),
    );
    draw_bookmark_rows(f, list_area, state, button_map, theme);

    if feedback_height == 1 {
        draw_feedback(
            f,
            Rect::new(
                inner.x,
                inner.bottom().saturating_sub(1),
                inner.width,
                1,
            ),
            state,
            theme,
        );
    }
}

fn draw_bookmark_rows(
    f: &mut Frame,
    list_area: Rect,
    state: &mut BookmarksState,
    button_map: &mut ButtonRenderMap,
    theme: super::theme::Theme,
) {
    if list_area.width == 0 || list_area.height == 0 {
        state.set_overlay_visible_rows(1);
        return;
    }

    let filtered = state.filtered_indices();
    let has_scrollbar = filtered.len() > list_area.height as usize && list_area.width > 1;
    let content_width = list_area.width.saturating_sub(u16::from(has_scrollbar));
    state.set_overlay_visible_rows(list_area.height as usize);
    if filtered.is_empty() {
        state.overlay_selected = 0;
        state.overlay_scroll = 0;
    } else if !filtered.contains(&state.overlay_selected) {
        state.overlay_selected = filtered[0];
        state.overlay_scroll = 0;
    }
    let max_scroll = filtered.len().saturating_sub(state.overlay_visible_rows);
    state.overlay_scroll = state.overlay_scroll.min(max_scroll);

    if filtered.is_empty() {
        let text = if state.filter_text().is_empty() {
            "(no bookmarks yet — press a to add the current directory)"
        } else {
            "(no bookmarks match the filter)"
        };
        f.render_widget(
            Paragraph::new(text).style(Style::default().fg(theme.text_dim)),
            list_area,
        );
        return;
    }

    let start = state.overlay_scroll;
    let end = (start + state.overlay_visible_rows).min(filtered.len());
    let mut row_targets = Vec::with_capacity(end.saturating_sub(start));
    for (row, entry_index) in filtered[start..end].iter().copied().enumerate() {
        let entry = &state.entries[entry_index];
        let selected = entry_index == state.overlay_selected;
        let target_status = state.target_status(&entry.path);
        let missing = target_status == Some(BookmarkTargetStatus::Missing);
        let unavailable = target_status == Some(BookmarkTargetStatus::Unavailable);
        let marker = if missing {
            "!"
        } else if unavailable {
            "?"
        } else if selected {
            "▸"
        } else {
            " "
        };
        let row_area = Rect::new(
            list_area.x,
            list_area.y.saturating_add(row as u16),
            content_width,
            1,
        );
        let style = if selected {
            Style::default()
                .fg(theme.text_bright)
                .bg(theme.selection_bg)
                .add_modifier(Modifier::BOLD)
        } else if missing {
            Style::default()
                .fg(theme.destructive)
                .add_modifier(Modifier::DIM)
        } else if unavailable {
            Style::default().fg(theme.amber).add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(theme.text)
        };
        let available = content_width.saturating_sub(3) as usize;
        let name = super::display_width::truncate_right(&entry.name, available);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!(" {marker} "), style),
                Span::styled(name, style),
            ]))
            .style(style),
            row_area,
        );
        row_targets.push((entry_index, row_area));
    }

    let scrollbar_targets = if has_scrollbar {
        draw_scrollbar(
            f,
            Rect::new(
                list_area.right().saturating_sub(1),
                list_area.y,
                1,
                list_area.height,
            ),
            filtered.len(),
            state.overlay_visible_rows,
            state.overlay_scroll,
            theme,
        )
    } else {
        None
    };

    // Second pass: register immutable geometry after drawing.
    for (entry_index, rect) in row_targets {
        button_map.record_button(TuiButton::BookmarkManagerRow(entry_index), rect);
    }
    if let Some((track, thumb)) = scrollbar_targets {
        button_map.record_button(
            TuiButton::ScrollbarTrack(ScrollbarSurface::BookmarkManager),
            track,
        );
        button_map.record_button(
            TuiButton::ScrollbarThumb(ScrollbarSurface::BookmarkManager),
            thumb,
        );
    }
}

fn draw_input_row(
    f: &mut Frame,
    area: Rect,
    state: &BookmarksState,
    theme: super::theme::Theme,
) {
    if let Some(naming) = &state.naming {
        let (prefix, input) = match naming {
            BookmarkNaming::Add { input, .. } => ("a add: ", input),
            BookmarkNaming::Rename { input, .. } => ("e rename: ", input),
        };
        let prefix_width = super::display_width::width(prefix) as u16;
        let visible = area.width.saturating_sub(prefix_width).saturating_sub(1) as usize;
        let (view, cursor) = input.view(visible);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prefix, Style::default().fg(theme.amber)),
                Span::styled(view, Style::default().fg(theme.text_bright).bg(theme.surface)),
            ])),
            area,
        );
        if let Some(cursor_x) = clamped_input_cursor_x(area, prefix_width, cursor) {
            f.set_cursor(cursor_x, area.y);
        }
        return;
    }

    if let Some(input) = &state.filter_input {
        let visible = area.width.saturating_sub(3) as usize;
        let (view, cursor) = input.view(visible);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("/ ", Style::default().fg(theme.cyan)),
                Span::styled(view, Style::default().fg(theme.text_bright).bg(theme.surface)),
            ])),
            area,
        );
        if let Some(cursor_x) = clamped_input_cursor_x(area, 2, cursor) {
            f.set_cursor(cursor_x, area.y);
        }
    } else {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("/ ", Style::default().fg(theme.cyan)),
                Span::styled("filter…", Style::default().fg(theme.text_dim)),
            ])),
            area,
        );
    }
}

fn clamped_input_cursor_x(area: Rect, prefix_width: u16, input_cursor: u16) -> Option<u16> {
    if area.width == 0 {
        return None;
    }
    let relative = prefix_width
        .saturating_add(input_cursor)
        .min(area.width.saturating_sub(1));
    Some(area.x.saturating_add(relative))
}

fn draw_detail_card(
    f: &mut Frame,
    area: Rect,
    state: &BookmarksState,
    theme: super::theme::Theme,
) {
    let Some(bookmark) = state.selected_filtered() else {
        return;
    };
    let card = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.text_dim))
        .title(Span::styled(
            format!(" {} ", bookmark.name),
            Style::default().fg(theme.title).add_modifier(Modifier::BOLD),
        ));
    let inner = card.inner(area);
    f.render_widget(card, area);

    let target_status = state.target_status(&bookmark.path);
    let missing = target_status == Some(BookmarkTargetStatus::Missing);
    let (status_text, status_color) = match target_status {
        Some(BookmarkTargetStatus::Reachable) => ("● reachable", theme.green),
        Some(BookmarkTargetStatus::Missing) => ("✕ missing", theme.destructive),
        Some(BookmarkTargetStatus::Unavailable) => ("? unavailable", theme.amber),
        None => ("… checking", theme.amber),
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("path    ", Style::default().fg(theme.label)),
            Span::styled(
                abbreviate_home(&bookmark.path),
                Style::default().fg(theme.value),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("status  ", Style::default().fg(theme.label)),
            Span::styled(
                status_text,
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    if missing {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "target no longer exists — e rename, d delete",
            Style::default()
                .fg(theme.destructive)
                .add_modifier(Modifier::DIM),
        )));
    } else if target_status == Some(BookmarkTargetStatus::Unavailable) {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "target cannot currently be inspected — check permissions or mount state",
            Style::default().fg(theme.amber).add_modifier(Modifier::DIM),
        )));
    } else {
        match state.detail_state(&bookmark.path) {
            Some(BookmarkDetailState::Ready(detail)) => {
                lines.push(Line::from(vec![
                    Span::styled("target  ", Style::default().fg(theme.label)),
                    Span::styled(
                        format!(
                            "directory · {} item{}",
                            detail.item_count,
                            if detail.item_count == 1 { "" } else { "s" },
                        ),
                        Style::default().fg(theme.value),
                    ),
                ]));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "contents",
                    Style::default().fg(theme.label).add_modifier(Modifier::BOLD),
                )));
                for entry in detail.entries.iter().take(DETAIL_PEEK_LIMIT) {
                    lines.push(Line::from(vec![
                        Span::styled(
                            if entry.is_dir { " ▸ " } else { "   " },
                            Style::default().fg(if entry.is_dir { theme.cyan } else { theme.text_dim }),
                        ),
                        Span::styled(entry.name.clone(), Style::default().fg(theme.text)),
                    ]));
                }
                if detail.omitted_count > 0 {
                    lines.push(Line::from(Span::styled(
                        format!("   … {} more", detail.omitted_count),
                        Style::default().fg(theme.text_dim),
                    )));
                }
            }
            Some(BookmarkDetailState::Queued) => {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "waiting for detail worker…",
                    Style::default().fg(theme.text_dim),
                )));
            }
            Some(BookmarkDetailState::Loading) => {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "loading contents…",
                    Style::default().fg(theme.text_dim),
                )));
            }
            Some(BookmarkDetailState::QueueUnavailable(message)) => {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    message.as_str(),
                    Style::default().fg(theme.amber),
                )));
            }
            Some(BookmarkDetailState::WorkerUnavailable(message)) => {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    message.as_str(),
                    Style::default().fg(theme.destructive),
                )));
            }
            Some(BookmarkDetailState::Error(error)) => {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("contents unavailable: {error}"),
                    Style::default().fg(theme.destructive),
                )));
            }
            None => {
                lines.push(Line::from(""));
                let message = match target_status {
                    None => "waiting for target check…",
                    Some(BookmarkTargetStatus::Reachable) => "contents not requested",
                    Some(BookmarkTargetStatus::Missing) => "target is missing",
                    Some(BookmarkTargetStatus::Unavailable) => "target is unavailable",
                };
                lines.push(Line::from(Span::styled(
                    message,
                    Style::default().fg(theme.text_dim),
                )));
            }
        }
    }

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_footer(f: &mut Frame, area: Rect, theme: super::theme::Theme) {
    use super::draw_overlays::{footer_pill_pub as pill, pill_gap_pub as gap};
    f.render_widget(
        Paragraph::new(Line::from(vec![
            pill("a add", theme.cyan, theme),
            gap(),
            pill("e rename", theme.amber, theme),
            gap(),
            pill("d delete", theme.destructive, theme),
            gap(),
            pill("J/K move", theme.title, theme),
            gap(),
            pill("Enter go", theme.green, theme),
            gap(),
            pill("Esc close", theme.purple, theme),
        ])),
        area,
    );
}

fn draw_feedback(f: &mut Frame, area: Rect, state: &BookmarksState, theme: super::theme::Theme) {
    if let Some(message) = state.feedback.as_ref().or(state.last_warning.as_ref()) {
        f.render_widget(
            Paragraph::new(message.as_str()).style(Style::default().fg(theme.text_dim)),
            area,
        );
    }
}

fn draw_scrollbar(
    f: &mut Frame,
    area: Rect,
    total: usize,
    visible: usize,
    offset: usize,
    theme: super::theme::Theme,
) -> Option<(Rect, Rect)> {
    let metrics = tui_file_picker::ScrollbarMetrics::new(total, visible, offset, area.height as usize)?;
    let track_lines = (0..area.height)
        .map(|_| Line::from("░"))
        .collect::<Vec<_>>();
    f.render_widget(
        Paragraph::new(track_lines).style(Style::default().fg(theme.text_dim)),
        area,
    );
    let thumb = Rect::new(
        area.x,
        area.y.saturating_add(metrics.thumb_start as u16),
        1,
        metrics.thumb_len as u16,
    );
    let thumb_lines = (0..metrics.thumb_len)
        .map(|_| Line::from("█"))
        .collect::<Vec<_>>();
    f.render_widget(
        Paragraph::new(thumb_lines).style(Style::default().fg(theme.title)),
        thumb,
    );
    Some((area, thumb))
}

pub fn load_bookmark_detail(path: &std::path::Path) -> Result<super::bookmarks::BookmarkDetail, String> {
    let mut item_count = 0usize;
    let mut preview = Vec::with_capacity(DETAIL_PEEK_LIMIT.saturating_add(1));

    for entry in std::fs::read_dir(path).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        item_count = item_count.saturating_add(1);
        preview.push(super::bookmarks::BookmarkDetailEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir: file_type.is_dir(),
        });
        preview.sort_by(|left, right| {
            right
                .is_dir
                .cmp(&left.is_dir)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.name.cmp(&right.name))
        });
        preview.truncate(DETAIL_PEEK_LIMIT);
    }

    Ok(super::bookmarks::BookmarkDetail {
        item_count,
        omitted_count: item_count.saturating_sub(preview.len()),
        entries: preview,
    })
}

fn abbreviate_home(path: &std::path::Path) -> String {
    let display = path.display().to_string();
    let Some(home) = std::env::var_os("HOME") else {
        return display;
    };
    let home = std::path::PathBuf::from(home);
    match path.strip_prefix(&home) {
        Ok(relative) if relative.as_os_str().is_empty() => "~".to_string(),
        Ok(relative) => format!("~/{}", relative.display()),
        Err(_) => display,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_minimum_geometry_is_explicit() {
        assert!(!bookmarks_overlay_fits(Rect::new(0, 0, 7, 6)));
        assert!(!bookmarks_overlay_fits(Rect::new(0, 0, 8, 5)));
        assert!(bookmarks_overlay_fits(Rect::new(0, 0, 8, 6)));
        assert!(bookmarks_overlay_uses_compact_layout(Rect::new(0, 0, 8, 6)));
        assert!(!bookmarks_overlay_uses_compact_layout(Rect::new(0, 0, 104, 30)));

        let compact_inner = Block::default()
            .borders(Borders::ALL)
            .inner(bookmarks_overlay_area(Rect::new(0, 0, 8, 6)));
        assert_eq!(compact_inner.height, 4);
        let feedback_height = u16::from(compact_inner.height >= 3);
        let compact_list_height = compact_inner
            .height
            .saturating_sub(1)
            .saturating_sub(feedback_height);
        assert_eq!(compact_list_height, 2);
        assert_eq!(feedback_height, 1);
    }

    #[test]
    fn input_cursor_is_clamped_inside_narrow_area() {
        let area = Rect::new(40, 7, 3, 1);
        assert_eq!(clamped_input_cursor_x(area, 10, 20), Some(42));
        assert_eq!(clamped_input_cursor_x(Rect::new(1, 1, 0, 1), 2, 3), None);
    }

    #[test]
    fn detail_loader_sorts_directories_before_files_and_caps_preview() {
        let root = std::env::temp_dir().join(format!(
            "tonepoet-bookmark-detail-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("z-dir")).expect("create dir");
        for index in 0..9 {
            std::fs::write(root.join(format!("file-{index}")), b"x").expect("write file");
        }
        let detail = load_bookmark_detail(&root).expect("detail");
        assert_eq!(detail.item_count, 10);
        assert!(detail.entries[0].is_dir);
        assert_eq!(detail.entries.len(), DETAIL_PEEK_LIMIT);
        assert_eq!(detail.omitted_count, 3);
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
