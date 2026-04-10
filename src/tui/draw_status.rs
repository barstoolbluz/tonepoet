//! Legacy header bar and status bar rendering (used by Queue screen)

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{AppScreen, AppState};
use super::theme;

/// Draw a simple header bar for non-convert screens
pub fn draw_header(f: &mut Frame, area: Rect, app: &mut AppState) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(20),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(area);

    let title = Paragraph::new(Line::from(vec![
        Span::styled("tonepoet", Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD)),
    ]));
    f.render_widget(title, chunks[0]);

    let tab_spans: Vec<Span> = vec![
        Span::styled(" Queue ", if app.current_screen == AppScreen::Queue {
            Style::default().fg(theme::AMBER).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            theme::muted()
        }),
        Span::styled(" | ", Style::default().fg(theme::TEXT_DIM)),
        Span::styled(" Config ", if app.current_screen == AppScreen::Config {
            Style::default().fg(theme::AMBER).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            theme::muted()
        }),
    ];

    let tab_line = Paragraph::new(Line::from(tab_spans));
    f.render_widget(tab_line, chunks[1]);
}

/// Draw the status bar at the bottom
pub fn draw_status_bar(f: &mut Frame, area: Rect, app: &AppState) {
    let items = &app.items_snapshot;

    let total = items.len();
    let completed = items.iter()
        .filter(|i| matches!(i.status, crate::convert::ConversionStatus::Completed { .. }))
        .count();
    let failed = items.iter()
        .filter(|i| matches!(i.status, crate::convert::ConversionStatus::Failed { .. }))
        .count();
    let processing = items.iter()
        .filter(|i| matches!(i.status, crate::convert::ConversionStatus::Processing { .. }))
        .count();
    let queued = items.iter()
        .filter(|i| matches!(i.status, crate::convert::ConversionStatus::Queued))
        .count();

    let workers = app.config.conversion.worker_count;

    let mut spans = Vec::new();

    if let Some((msg, _)) = &app.status_message {
        spans.push(Span::styled(msg.clone(), Style::default().fg(theme::AMBER)));
    } else if total > 0 {
        spans.push(Span::styled(
            format!("{}/{} completed", completed, total),
            Style::default().fg(theme::GREEN),
        ));
        if failed > 0 {
            spans.push(Span::raw(" | "));
            spans.push(Span::styled(format!("{} failed", failed), Style::default().fg(theme::RED)));
        }
        if processing > 0 {
            spans.push(Span::raw(" | "));
            spans.push(Span::styled(format!("{} processing", processing), Style::default().fg(theme::CYAN)));
        }
        if queued > 0 {
            spans.push(Span::raw(" | "));
            spans.push(Span::styled(format!("{} queued", queued), theme::bright()));
        }
        spans.push(Span::raw(" | "));
        spans.push(Span::styled(format!("{} workers", workers), theme::muted()));
    } else {
        spans.push(Span::styled(
            "Empty queue - press 'a' to add files",
            theme::muted(),
        ));
    }

    if app.processing_active {
        if app.manager.is_paused() {
            spans.push(Span::raw(" | "));
            spans.push(Span::styled("PAUSED", Style::default().fg(theme::AMBER).add_modifier(Modifier::BOLD)));
        } else {
            spans.push(Span::raw(" | "));
            spans.push(Span::styled("CONVERTING", Style::default().fg(theme::GREEN).add_modifier(Modifier::BOLD)));
        }
    }

    let status = Paragraph::new(Line::from(spans));
    f.render_widget(status, area);
}
