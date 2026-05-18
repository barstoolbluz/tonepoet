//! Preset indicator bar: active preset name, modified flag, shortcut hints

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::PresetState;
use super::button_map::{ButtonRenderMap, TuiButton};
use super::theme;

/// Draw the preset bar row and register clickable regions
pub fn draw_preset_bar(
    f: &mut Frame,
    area: Rect,
    preset: &PresetState,
    buttons: &mut ButtonRenderMap,
) {
    if area.width < 20 {
        return;
    }

    let mut spans = vec![Span::styled("  preset  ", theme::muted())];

    // Track x position of the preset pill for click registration
    let mut preset_pill_rect: Option<Rect> = None;

    if let Some(name) = &preset.active_preset {
        let pill_text = format!(" {} ", name);
        // Use char count for display width (handles multibyte names)
        let pill_width = pill_text.chars().count() as u16;
        let pill_x = area.x + 10; // after "  preset  "
        preset_pill_rect = Some(Rect::new(pill_x, area.y, pill_width, 1));

        spans.push(Span::styled(
            pill_text,
            Style::default()
                .fg(theme::PILL_PRESET_FG)
                .bg(theme::PILL_PRESET_BG)
                .add_modifier(Modifier::BOLD),
        ));

        if preset.modified {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                "(modified)",
                Style::default()
                    .fg(theme::TEXT_MUTED)
                    .add_modifier(Modifier::DIM),
            ));
        }
    } else {
        spans.push(Span::styled("none", Style::default().fg(theme::TEXT_DIM)));
    }

    // Right-aligned shortcut hints: "p presets  s save" = 17 chars
    let hints_text_width: usize = 17;
    let left_width: usize = spans.iter().map(|s| s.width()).sum();
    let gap = (area.width as usize).saturating_sub(left_width + hints_text_width + 2);

    spans.push(Span::raw(" ".repeat(gap)));
    spans.push(Span::styled("p", theme::muted()));
    spans.push(Span::styled(" presets", theme::accent()));
    spans.push(Span::raw("  "));
    spans.push(Span::styled("s", theme::muted()));
    spans.push(Span::styled(" save", theme::accent()));

    // Compute button positions for right-side hints
    let hints_start_x = area.x + (left_width + gap) as u16;
    let presets_btn_rect = Rect::new(hints_start_x, area.y, 9, 1); // "p presets"
    let save_btn_rect = Rect::new(hints_start_x + 11, area.y, 6, 1); // "s save" (after 9 + 2 space)

    let line = Paragraph::new(Line::from(spans));
    f.render_widget(line, area);

    // Register clickable regions
    if let Some(rect) = preset_pill_rect {
        buttons.record_button(TuiButton::PresetsButton, rect);
    }
    buttons.record_button(TuiButton::PresetsButton, presets_btn_rect);
    buttons.record_button(TuiButton::SaveButton, save_btn_rect);
}
