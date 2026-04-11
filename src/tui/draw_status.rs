//! Queue screen stats strip: persistent conversion progress display.
//!
//! Transient status messages are shown globally via the footer context bar,
//! so this strip is concerned only with queue-specific progress stats.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::AppState;
use super::theme;

/// Draw a 1-row strip showing persistent queue conversion stats.
/// Transient `status_message` content is intentionally NOT rendered here —
/// it belongs in the footer context bar so every screen can display it.
pub fn draw_queue_stats_strip(f: &mut Frame, area: Rect, app: &AppState) {
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

    let mut spans: Vec<Span> = vec![Span::raw("  ")];

    if total > 0 {
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
            "empty queue — press a to add files",
            theme::muted(),
        ));
    }

    if app.processing_active {
        if app.manager.is_paused() {
            spans.push(Span::raw(" | "));
            spans.push(Span::styled(
                "PAUSED",
                Style::default().fg(theme::AMBER).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::raw(" | "));
            spans.push(Span::styled(
                "CONVERTING",
                Style::default().fg(theme::GREEN).add_modifier(Modifier::BOLD),
            ));
        }
    }

    let line = Paragraph::new(Line::from(spans));
    f.render_widget(line, area);
}
