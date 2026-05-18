//! Recent files overlay: floating panel listing recently-used source files.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::recent_files::RecentFilesState;
use super::theme;

/// Draw the recent files overlay as a centered floating panel.
/// Takes `&mut RecentFilesState` so it can publish the visible row count
/// back into the state (used by `ensure_visible` for scroll math).
pub fn draw_recent_overlay(f: &mut Frame, state: &mut RecentFilesState) {
    let area = f.size();
    let width: u16 = 60.min(area.width.saturating_sub(4));
    let list_height = state.entries.len() as u16;
    // Header "recent files" (1) + blank (1) + list + blank (1) + help line (1) + borders (2)
    let height = (list_height + 6).min(area.height.saturating_sub(4)).max(8);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let overlay_area = Rect::new(x, y, width, height);

    f.render_widget(Clear, overlay_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::CYAN))
        .title(Span::styled(
            " RECENT FILES ",
            Style::default()
                .fg(theme::CYAN)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(""));

    if state.entries.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("(no recent files)", theme::muted()),
        ]));
    } else {
        // Compute per-line width budget for the path after the marker, name, and age columns.
        let inner_w = inner.width as usize;
        let age_col_w = 10; // enough for "99mo ago"
        let marker_w = 2;
        let name_col_w = inner_w / 3; // up to 1/3 for the filename
        let path_col_w = inner_w
            .saturating_sub(marker_w)
            .saturating_sub(name_col_w)
            .saturating_sub(age_col_w)
            .saturating_sub(3); // gaps

        // Compute vertical budget for entry rows (leading blank + trailing blank + help = 3).
        let max_visible_rows = (inner.height as usize).saturating_sub(3);
        // Publish the row budget so ensure_visible (called from navigation) works.
        state.overlay_visible_rows = max_visible_rows;

        // Clamp scroll in case entries shrank after a previous render.
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

            let name = entry
                .path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();

            let parent = entry
                .path
                .parent()
                .map(|p| {
                    let s = p.display().to_string();
                    if let Ok(home) = std::env::var("HOME") {
                        if s.starts_with(&home) {
                            return format!("~{}", &s[home.len()..]);
                        }
                    }
                    s
                })
                .unwrap_or_default();

            let name_display = truncate_middle(&name, name_col_w);
            let parent_display = truncate_left(&parent, path_col_w);
            let age = entry.relative_time();

            let name_style = if is_selected {
                Style::default()
                    .fg(theme::TEXT_BRIGHT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::TEXT)
            };

            // Build spans: marker + name + gap + parent + gap + age (right-ish)
            let name_w = name_display.chars().count();
            let name_pad = name_col_w.saturating_sub(name_w);

            let parent_w = parent_display.chars().count();
            let parent_pad = path_col_w.saturating_sub(parent_w);

            lines.push(Line::from(vec![
                Span::styled(marker, marker_style),
                Span::styled(name_display, name_style),
                Span::raw(" ".repeat(name_pad + 1)),
                Span::styled(parent_display, theme::muted()),
                Span::raw(" ".repeat(parent_pad + 1)),
                Span::styled(age, theme::muted()),
            ]));
        }
    }

    // Blank + help pills
    use super::draw_overlays::{footer_pill_pub as pill, pill_gap_pub as gap};
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        pill("Enter load", theme::GREEN),
        gap(),
        pill("d delete", theme::RED),
        gap(),
        pill("Esc close", theme::PURPLE),
    ]));

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, inner);
}

/// Truncate to `max` chars, adding an ellipsis at the end if cut.
fn truncate_middle(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    if max < 2 {
        return s.chars().take(max).collect();
    }
    let take = max - 1;
    let truncated: String = s.chars().take(take).collect();
    format!("{}…", truncated)
}

/// Truncate from the LEFT (preserve end), prepending `…`.
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
