//! Preset indicator bar: active preset name, modified flag, shortcut hints

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::PresetState;
use super::theme;

/// Draw the preset bar row
pub fn draw_preset_bar(f: &mut Frame, area: Rect, preset: &PresetState) {
    if area.width < 20 {
        return;
    }

    let mut spans = vec![Span::styled("  preset  ", theme::muted())];

    if let Some(name) = &preset.active_preset {
        // Active preset pill
        spans.push(Span::styled(
            format!(" {} ", name),
            Style::default()
                .fg(theme::PILL_PRESET_FG)
                .bg(theme::PILL_PRESET_BG)
                .add_modifier(Modifier::BOLD),
        ));

        if preset.modified {
            spans.push(Span::raw("  "));
            spans.push(Span::styled("(modified)", Style::default().fg(theme::TEXT_MUTED).add_modifier(Modifier::DIM)));
        }
    } else {
        spans.push(Span::styled("none", Style::default().fg(theme::TEXT_DIM)));
    }

    // Right-aligned shortcut hints
    let hints_width = 22; // "  p presets  s save"
    let left_width: usize = spans.iter().map(|s| s.width()).sum();
    let gap = (area.width as usize).saturating_sub(left_width + hints_width + 2);

    spans.push(Span::raw(" ".repeat(gap)));
    spans.push(Span::styled("p", theme::muted()));
    spans.push(Span::styled(" presets", theme::accent()));
    spans.push(Span::raw("  "));
    spans.push(Span::styled("s", theme::muted()));
    spans.push(Span::styled(" save", theme::accent()));

    let line = Paragraph::new(Line::from(spans));
    f.render_widget(line, area);
}
