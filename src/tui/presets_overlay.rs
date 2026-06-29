//! Preset overlay: floating panel for loading/saving/managing presets

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::app::PresetState;

/// Draw the preset overlay (floating panel, bottom-right)
pub fn draw_presets_overlay(f: &mut Frame, preset: &PresetState, theme: super::theme::Theme) {
    let area = f.size();
    let width: u16 = 36;
    let list_height = preset.overlay_list.len() as u16;
    // Header(1) + "saved presets" label(1) + list + separator(1) + pill row(1) + borders(2)
    let height = (list_height + 6).min(area.height.saturating_sub(4));
    let x = area.width.saturating_sub(width + 2);
    let y = area.height.saturating_sub(height + 3);
    let overlay_area = Rect::new(x, y, width, height);

    f.render_widget(Clear, overlay_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.text_dim))
        .title(Span::styled(
            " PRESETS ",
            Style::default()
                .fg(theme.text_muted)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(overlay_area);
    f.render_widget(block, overlay_area);

    let mut lines: Vec<Line> = Vec::new();

    // Check if we're in naming mode
    if let Some(input) = &preset.naming_input {
        lines.push(Line::from(Span::styled(
            "save as:",
            Style::default().fg(theme.text_muted),
        )));
        // Scrolled view: leave 2 cols for leading/trailing space
        let visible_width = (inner.width as usize).saturating_sub(2);
        let (view, cursor_col) = input.view(visible_width);
        lines.push(Line::from(vec![Span::styled(
            format!(" {} ", view),
            Style::default().fg(theme.text_bright).bg(theme.surface),
        )]));
        use super::draw_overlays::{footer_pill_pub as pill, pill_gap_pub as gap};
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            pill("Enter save", theme.green, theme),
            gap(),
            pill("Esc cancel", theme.purple, theme),
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
        Style::default().fg(theme.text_muted),
    )));

    // Preset list
    if preset.overlay_list.is_empty() {
        lines.push(Line::from(Span::styled(
            " (none)",
            Style::default().fg(theme.text_dim),
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
                        Style::default().fg(theme.blue)
                    } else {
                        Style::default().fg(theme.text_dim)
                    },
                ),
                Span::styled(
                    name.clone(),
                    if is_selected {
                        Style::default().fg(theme.text_bright)
                    } else {
                        Style::default().fg(theme.text)
                    },
                ),
            ];

            if is_active {
                spans.push(Span::styled(" active", Style::default().fg(theme.cyan)));
            }

            lines.push(Line::from(spans));
        }
    }

    // Separator
    lines.push(Line::from(Span::styled(
        "─".repeat(inner.width as usize),
        Style::default().fg(theme.border_dim),
    )));

    // Action pills
    use super::draw_overlays::{footer_pill_pub as pill, pill_gap_pub as gap};
    lines.push(Line::from(vec![
        pill("n new", theme.green, theme),
        gap(),
        pill("d dup", theme.amber, theme),
        gap(),
        pill("x del", theme.destructive, theme),
        gap(),
        pill("Esc close", theme.purple, theme),
    ]));

    let p = Paragraph::new(lines);
    f.render_widget(p, inner);
}
