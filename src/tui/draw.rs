//! Top-level draw_ui() dispatching by screen

use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{AppScreen, AppState};
use super::convert_screen::draw_convert_screen;
use super::draw_queue::draw_queue_screen;
use super::draw_overlays::draw_overlay;

/// Main draw function dispatching to screen-specific renderers
pub fn draw_ui(f: &mut Frame, app: &mut AppState) {
    // Clear button map for this frame
    app.button_map.clear();

    // Background fill
    let bg = Paragraph::new("").style(Style::default().bg(super::theme::BG));
    f.render_widget(bg, f.size());

    match app.current_screen {
        AppScreen::Convert => {
            draw_convert_screen(f, f.size(), app);
        }
        AppScreen::Queue => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(7),  // header banner
                    Constraint::Length(1),  // blank
                    Constraint::Length(1),  // stats strip
                    Constraint::Length(1),  // blank
                    Constraint::Min(10),    // content
                    Constraint::Length(2),  // footer (tabs + context)
                ])
                .split(f.size());

            super::draw_header::draw_header(f, chunks[0]);
            super::draw_status::draw_queue_stats_strip(f, chunks[2], app);
            draw_queue_screen(f, chunks[4], app);
            let status_msg = app.status_message.as_ref().map(|(s, _)| s.as_str());
            super::draw_footer::draw_footer(f, chunks[5], app.current_screen, &mut app.button_map, status_msg);
        }
        AppScreen::Wizard => {
            draw_wizard_screen(f, app);
        }
        AppScreen::Config => {
            draw_settings_screen(f, f.size(), app);
        }
        AppScreen::Browse => {
            super::draw_browse::draw_browse_screen(f, f.size(), app);
        }
        AppScreen::Library => {
            draw_placeholder_screen(f, f.size(), app);
        }
    }

    // Draw any active overlay on top
    draw_overlay(f, app);
}

/// Draw the wizard screen by delegating to tonepoet_wizard
fn draw_wizard_screen(f: &mut Frame, app: &mut AppState) {
    if let Some(wizard) = &app.wizard {
        let areas = tonepoet_wizard::draw_wizard(f, wizard);
        app.wizard_mouse_areas = Some(areas);
    } else {
        let msg = Paragraph::new("No wizard active. Press Esc to return.");
        f.render_widget(msg, f.size());
    }
}

/// Draw settings screen showing conversion configuration
fn draw_settings_screen(f: &mut Frame, area: ratatui::layout::Rect, app: &mut AppState) {
    use ratatui::widgets::{Block, Borders};
    use ratatui::style::Modifier;
    use super::theme;

    // Use convert screen layout with just footer for tab navigation
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),
            Constraint::Length(2),
        ])
        .split(area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::TEXT_DIM))
        .title(Span::styled(
            " Conversion Settings ",
            Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(chunks[0]);
    f.render_widget(block, chunks[0]);

    let cfg = &app.config.conversion;

    let dest_str = cfg.default_destination.as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(ask every time)".to_string());
    let scratch_str = cfg.scratch_directory.as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(system default)".to_string());

    let lines = vec![
        Line::from(vec![
            Span::styled("  Backend:              ", theme::muted()),
            Span::styled(&cfg.preferred_backend, theme::bright()),
        ]),
        Line::from(vec![
            Span::styled("  Workers:              ", theme::muted()),
            Span::styled(cfg.worker_count.to_string(), theme::bright()),
        ]),
        Line::from(vec![
            Span::styled("  ReplayGain:           ", theme::muted()),
            Span::styled(
                if cfg.calculate_replaygain { "Enabled" } else { "Disabled" },
                Style::default().fg(if cfg.calculate_replaygain { theme::GREEN } else { theme::TEXT_DIM }),
            ),
        ]),
        Line::from(vec![
            Span::styled("  CUE files:            ", theme::muted()),
            Span::styled(
                if cfg.generate_cue_files { format!("Enabled ({})", cfg.cue_generation_mode) } else { "Disabled".to_string() },
                Style::default().fg(if cfg.generate_cue_files { theme::GREEN } else { theme::TEXT_DIM }),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Log files:            ", theme::muted()),
            Span::styled(
                if cfg.write_log_file { "Enabled" } else { "Disabled" },
                Style::default().fg(if cfg.write_log_file { theme::GREEN } else { theme::TEXT_DIM }),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Persist queue:        ", theme::muted()),
            Span::styled(
                if cfg.persist_queue { "Enabled" } else { "Disabled" },
                Style::default().fg(if cfg.persist_queue { theme::GREEN } else { theme::TEXT_DIM }),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Destination:          ", theme::muted()),
            Span::styled(dest_str, theme::bright()),
        ]),
        Line::from(vec![
            Span::styled("  Scratch directory:    ", theme::muted()),
            Span::styled(scratch_str, theme::bright()),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Edit ~/.config/tonepoet/config.toml to change settings",
            theme::muted(),
        )),
    ];

    let p = Paragraph::new(lines);
    f.render_widget(p, inner);

    // Footer
    let status_msg = app.status_message.as_ref().map(|(s, _)| s.as_str());
    super::draw_footer::draw_footer(f, chunks[1], app.current_screen, &mut app.button_map, status_msg);
}

/// Draw a placeholder screen for unimplemented tabs
fn draw_placeholder_screen(f: &mut Frame, area: ratatui::layout::Rect, app: &mut AppState) {
    use super::theme;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),
            Constraint::Length(2),
        ])
        .split(area);

    let label = app.current_screen.tab_label();
    let msg = Paragraph::new(Line::from(vec![
        Span::styled(
            format!("  {} — coming soon", label),
            theme::muted(),
        ),
    ]));
    f.render_widget(msg, chunks[0]);

    let status_msg = app.status_message.as_ref().map(|(s, _)| s.as_str());
    super::draw_footer::draw_footer(f, chunks[1], app.current_screen, &mut app.button_map, status_msg);
}
