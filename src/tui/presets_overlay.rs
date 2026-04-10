//! Preset overlay: floating panel for loading/saving/managing presets

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::app::PresetState;
use super::theme;

/// Draw the preset overlay (floating panel, bottom-right)
pub fn draw_presets_overlay(f: &mut Frame, preset: &PresetState) {
    let area = f.size();
    let width: u16 = 36;
    let list_height = preset.overlay_list.len() as u16;
    // Header(1) + "saved presets" label(1) + list + separator(1) + actions(3) + separator(1) + esc(1) + borders(2)
    let height = (list_height + 10).min(area.height.saturating_sub(4));
    let x = area.width.saturating_sub(width + 2);
    let y = area.height.saturating_sub(height + 3);
    let overlay_area = Rect::new(x, y, width, height);

    f.render_widget(Clear, overlay_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::TEXT_DIM))
        .title(Span::styled(
            " PRESETS ",
            Style::default()
                .fg(theme::TEXT_MUTED)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    let mut lines: Vec<Line> = Vec::new();

    // Check if we're in naming mode
    if let Some(input) = &preset.naming_input {
        lines.push(Line::from(Span::styled(
            "save as:",
            Style::default().fg(theme::TEXT_MUTED),
        )));
        // Scrolled view: leave 2 cols for leading/trailing space
        let visible_width = (inner.width as usize).saturating_sub(2);
        let (view, cursor_col) = input.view(visible_width);
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", view),
                Style::default().fg(theme::TEXT_BRIGHT).bg(theme::SURFACE),
            ),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("enter", Style::default().fg(theme::GREEN)),
            Span::styled(" save  ", Style::default().fg(theme::TEXT_MUTED)),
            Span::styled("esc", Style::default().fg(theme::RED)),
            Span::styled(" cancel", Style::default().fg(theme::TEXT_MUTED)),
        ]));

        let p = Paragraph::new(lines);
        f.render_widget(p, inner);

        // Position cursor (after the leading space)
        f.set_cursor(inner.x + 1 + cursor_col, inner.y + 1);
        return;
    }

    // Section label
    lines.push(Line::from(Span::styled(
        "saved presets",
        Style::default().fg(theme::TEXT_MUTED),
    )));

    // Preset list
    if preset.overlay_list.is_empty() {
        lines.push(Line::from(Span::styled(
            " (none)",
            Style::default().fg(theme::TEXT_DIM),
        )));
    } else {
        for (i, name) in preset.overlay_list.iter().enumerate() {
            let is_selected = i == preset.overlay_selected;
            let is_active = preset.active_preset.as_deref() == Some(name.as_str());

            let marker = if is_selected { "▸ " } else { "  " };

            let mut spans = vec![
                Span::styled(
                    marker,
                    if is_selected {
                        Style::default().fg(theme::BLUE)
                    } else {
                        Style::default().fg(theme::TEXT_DIM)
                    },
                ),
                Span::styled(
                    name.clone(),
                    if is_selected {
                        Style::default().fg(theme::TEXT_BRIGHT)
                    } else {
                        Style::default().fg(theme::TEXT)
                    },
                ),
            ];

            if is_active {
                spans.push(Span::styled(
                    " active",
                    Style::default().fg(theme::CYAN),
                ));
            }

            lines.push(Line::from(spans));
        }
    }

    // Separator
    lines.push(Line::from(Span::styled(
        "─".repeat(inner.width as usize),
        Style::default().fg(theme::BORDER_DIM),
    )));

    // Actions
    lines.push(Line::from(vec![
        Span::styled(" n", Style::default().fg(theme::GREEN)),
        Span::styled("  save as new preset", Style::default().fg(theme::TEXT)),
    ]));
    lines.push(Line::from(vec![
        Span::styled(" d", Style::default().fg(theme::AMBER)),
        Span::styled("  duplicate", Style::default().fg(theme::TEXT)),
    ]));
    lines.push(Line::from(vec![
        Span::styled(" x", Style::default().fg(theme::RED)),
        Span::styled("  delete", Style::default().fg(theme::TEXT)),
    ]));

    // Separator
    lines.push(Line::from(Span::styled(
        "─".repeat(inner.width as usize),
        Style::default().fg(theme::BORDER_DIM),
    )));

    // Close hint
    lines.push(Line::from(vec![
        Span::styled(" esc", Style::default().fg(theme::TEXT_MUTED)),
        Span::styled("  close", Style::default().fg(theme::TEXT_MUTED)),
    ]));

    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}
