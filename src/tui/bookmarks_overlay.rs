//! Bookmarks overlay: floating panel for managing browse-screen shortcuts.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::bookmarks::{BookmarkNaming, BookmarksState};
use super::theme;

/// Draw the bookmarks overlay as a centered floating panel.
/// Takes `&mut BookmarksState` so the renderer can publish `overlay_visible_rows`.
pub fn draw_bookmarks_overlay(f: &mut Frame, state: &mut BookmarksState) {
    let area = f.size();
    let width: u16 = 64.min(area.width.saturating_sub(4));
    let list_height = state.entries.len() as u16;
    // top blank (1) + list + bottom blank (1) + help (1) + borders (2) = 5
    let height = (list_height + 5).min(area.height.saturating_sub(4)).max(8);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let overlay_area = Rect::new(x, y, width, height);

    f.render_widget(Clear, overlay_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::GREEN))
        .title(Span::styled(
            " BOOKMARKS ",
            Style::default()
                .fg(theme::GREEN)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    // Branch on naming mode first — it replaces the list entirely.
    if let Some(naming) = &state.naming {
        draw_naming_mode(f, inner, naming);
        return;
    }

    // ── Browse mode: list + help ───────────────────────────────────
    let mut lines: Vec<Line> = vec![Line::from("")];

    if state.entries.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("(no bookmarks yet — press ", theme::muted()),
            Span::styled("a", Style::default().fg(theme::BLUE)),
            Span::styled(" to add the current directory)", theme::muted()),
        ]));
    } else {
        let inner_w = inner.width as usize;
        let marker_w = 2;
        let name_col_w = inner_w / 3;
        let path_col_w = inner_w
            .saturating_sub(marker_w)
            .saturating_sub(name_col_w)
            .saturating_sub(2); // one gap

        // Leading blank (1) + trailing blank (1) + help (1) = 3 rows reserved.
        let max_visible_rows = (inner.height as usize).saturating_sub(3);
        state.overlay_visible_rows = max_visible_rows;

        if state.overlay_scroll + max_visible_rows > state.entries.len()
            && state.entries.len() > max_visible_rows
        {
            state.overlay_scroll = state.entries.len() - max_visible_rows;
        } else if state.entries.len() <= max_visible_rows {
            state.overlay_scroll = 0;
        }

        let start = state.overlay_scroll;
        let end = (start + max_visible_rows).min(state.entries.len());
        for i in start..end {
            let entry = &state.entries[i];
            let is_selected = i == state.overlay_selected;

            let marker = if is_selected { "▸ " } else { "  " };
            let marker_style = if is_selected {
                Style::default().fg(theme::BLUE)
            } else {
                Style::default().fg(theme::TEXT_DIM)
            };

            let name_display = truncate_right(&entry.name, name_col_w);
            let name_w = name_display.chars().count();
            let name_pad = name_col_w.saturating_sub(name_w);

            let path_display = {
                let s = entry.path.display().to_string();
                if let Ok(home) = std::env::var("HOME") {
                    if s.starts_with(&home) {
                        format!("~{}", &s[home.len()..])
                    } else {
                        s
                    }
                } else {
                    s
                }
            };
            let path_display = truncate_left(&path_display, path_col_w);

            let name_style = if is_selected {
                Style::default()
                    .fg(theme::TEXT_BRIGHT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::TEXT)
            };

            lines.push(Line::from(vec![
                Span::styled(marker, marker_style),
                Span::styled(name_display, name_style),
                Span::raw(" ".repeat(name_pad + 1)),
                Span::styled(path_display, theme::muted()),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("↑↓", Style::default().fg(theme::BLUE)),
        Span::styled(" nav  ", theme::muted()),
        Span::styled("enter", Style::default().fg(theme::GREEN)),
        Span::styled(" cd  ", theme::muted()),
        Span::styled("a", Style::default().fg(theme::BLUE)),
        Span::styled(" add  ", theme::muted()),
        Span::styled("d", Style::default().fg(theme::RED)),
        Span::styled(" delete  ", theme::muted()),
        Span::styled("e", Style::default().fg(theme::AMBER)),
        Span::styled(" rename  ", theme::muted()),
        Span::styled("esc", Style::default().fg(theme::RED)),
        Span::styled(" close", theme::muted()),
    ]));

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, inner);
}

/// Draw the naming sub-mode: either adding or renaming. Replaces the list
/// with a single-line input labeled appropriately, plus a help line below.
/// Compact layout (5-6 rows) fits inside the minimum overlay inner height.
fn draw_naming_mode(f: &mut Frame, inner: Rect, naming: &BookmarkNaming) {
    let (label, input, subtitle) = match naming {
        BookmarkNaming::Add { input, path } => {
            let path_display = {
                let s = path.display().to_string();
                if let Ok(home) = std::env::var("HOME") {
                    if s.starts_with(&home) {
                        format!("~{}", &s[home.len()..])
                    } else {
                        s
                    }
                } else {
                    s
                }
            };
            (
                "add bookmark (name)",
                input,
                Some(path_display),
            )
        }
        BookmarkNaming::Rename { input, .. } => ("rename bookmark", input, None),
    };

    let mut lines: Vec<Line> = Vec::new();

    // Line 0: label
    lines.push(Line::from(Span::styled(
        format!("  {}", label),
        theme::muted(),
    )));

    // Line 1 (add only): subtitle showing the captured path
    if let Some(sub) = &subtitle {
        lines.push(Line::from(vec![
            Span::styled("    → ", theme::muted()),
            Span::styled(sub.clone(), Style::default().fg(theme::CYAN)),
        ]));
    }

    // Spacer
    lines.push(Line::from(""));

    // Input row
    let visible_width = (inner.width as usize).saturating_sub(4);
    let (view, cursor_col_in_view) = input.view(visible_width);
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!(" {} ", view),
            Style::default().fg(theme::TEXT_BRIGHT).bg(theme::SURFACE),
        ),
    ]));

    // Spacer + help
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("enter", Style::default().fg(theme::GREEN)),
        Span::styled(" save  ", theme::muted()),
        Span::styled("esc", Style::default().fg(theme::RED)),
        Span::styled(" cancel", theme::muted()),
    ]));

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, inner);

    // Terminal cursor for the input row.
    // Row layout:
    //   0: label
    //   1: subtitle (only if add)
    //   1 or 2: blank spacer
    //   2 or 3: input  ← cursor here
    //   3 or 4: blank
    //   4 or 5: help
    let input_line_offset: u16 = if subtitle.is_some() { 3 } else { 2 };
    let cursor_x = inner.x + 3 + cursor_col_in_view; // 2 leading spaces + 1 inside-span space
    let cursor_y = inner.y + input_line_offset;
    f.set_cursor(cursor_x, cursor_y);
}

/// Truncate to `max` chars, trailing ellipsis if cut.
fn truncate_right(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max < 2 {
        return s.chars().take(max).collect();
    }
    let truncated: String = s.chars().take(max - 1).collect();
    format!("{}…", truncated)
}

/// Truncate from the left (preserve end) with leading ellipsis.
fn truncate_left(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max < 2 {
        let skip = count - max;
        return s.chars().skip(skip).collect();
    }
    let skip = count - (max - 1);
    let truncated: String = s.chars().skip(skip).collect();
    format!("…{}", truncated)
}
