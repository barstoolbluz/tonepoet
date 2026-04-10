//! Footer: 5-tab view bar + context keybinding bar

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::AppScreen;
use super::theme;

/// Draw both footer rows (tabs + context bar) into a 2-line area
pub fn draw_footer(f: &mut Frame, area: Rect, current_screen: AppScreen) {
    if area.height < 2 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    draw_tab_bar(f, chunks[0], current_screen);
    draw_context_bar(f, chunks[1], current_screen);
}

/// Draw the numbered tab bar: 1 convert | 2 browse | 3 library | 4 queue | 5 config
fn draw_tab_bar(f: &mut Frame, area: Rect, current: AppScreen) {
    let tabs = AppScreen::tabs();
    let tab_width = area.width as usize / tabs.len();

    let mut spans: Vec<Span> = Vec::new();

    for (i, tab) in tabs.iter().enumerate() {
        let num = tab.tab_number().unwrap_or(0);
        let label = tab.tab_label();
        let is_active = *tab == current;

        if i > 0 {
            spans.push(Span::styled("│", Style::default().fg(theme::BORDER_DIM)));
        }

        // Key badge
        let key_style = if is_active {
            Style::default()
                .fg(theme::BG)
                .bg(theme::BLUE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme::TEXT_MUTED)
                .bg(theme::BORDER_DIM)
        };

        let label_style = if is_active {
            Style::default().fg(theme::TEXT_BRIGHT)
        } else {
            Style::default().fg(theme::TEXT_MUTED)
        };

        // Pad to distribute evenly
        let cell_content = format!(" {} {} ", num, label);
        let pad = tab_width.saturating_sub(cell_content.len());
        let pad_left = pad / 2;
        let pad_right = pad.saturating_sub(pad_left);

        spans.push(Span::raw(" ".repeat(pad_left)));
        spans.push(Span::styled(format!("{}", num), key_style));
        spans.push(Span::styled(format!(" {}", label), label_style));
        spans.push(Span::raw(" ".repeat(pad_right)));
    }

    let bar = Paragraph::new(Line::from(spans))
        .style(Style::default().bg(theme::SURFACE));
    f.render_widget(bar, area);
}

/// Draw the context-sensitive keybinding bar
fn draw_context_bar(f: &mut Frame, area: Rect, current: AppScreen) {
    let bindings: Vec<(&str, &str, ratatui::style::Color)> = match current {
        AppScreen::Convert => vec![
            ("↑↓", "navigate", theme::BLUE),
            ("tab", "pane", theme::BLUE),
            ("←→", "select", theme::BLUE),
            ("e", "edit", theme::BLUE),
            ("a", "advanced", theme::PURPLE),
            ("|", "", theme::BORDER_DIM),
            ("p", "presets", theme::CYAN),
            ("s", "save", theme::CYAN),
            ("f", "effects", theme::AMBER),
            ("|", "", theme::BORDER_DIM),
            ("enter", "convert", theme::GREEN),
            ("+", "queue", theme::AMBER),
            ("|", "", theme::BORDER_DIM),
            ("q", "quit", theme::RED),
        ],
        AppScreen::Queue => vec![
            ("↑↓", "navigate", theme::BLUE),
            ("space", "select", theme::BLUE),
            ("a", "add files", theme::BLUE),
            ("c", "configure", theme::PURPLE),
            ("|", "", theme::BORDER_DIM),
            ("s", "start", theme::GREEN),
            ("p", "pause", theme::AMBER),
            ("|", "", theme::BORDER_DIM),
            ("q", "quit", theme::RED),
        ],
        _ => vec![
            ("q", "quit", theme::RED),
            ("1", "convert", theme::BLUE),
        ],
    };

    let mut spans: Vec<Span> = vec![Span::raw(" ")];

    for (key, label, color) in &bindings {
        if *key == "|" {
            spans.push(Span::styled(" │ ", Style::default().fg(theme::BORDER_DIM)));
        } else {
            spans.push(Span::styled(*key, Style::default().fg(*color)));
            if !label.is_empty() {
                spans.push(Span::styled(
                    format!(" {} ", label),
                    Style::default().fg(theme::TEXT_MUTED),
                ));
            }
        }
    }

    let bar = Paragraph::new(Line::from(spans));
    f.render_widget(bar, area);
}
