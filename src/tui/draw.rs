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
use super::draw_overlays::draw_overlay;
use super::draw_queue::draw_queue_screen;

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
                    Constraint::Length(7), // header banner
                    Constraint::Length(1), // blank
                    Constraint::Length(1), // stats strip
                    Constraint::Length(1), // blank
                    Constraint::Min(10),   // content
                    Constraint::Length(2), // footer (tabs + context)
                ])
                .split(f.size());

            super::draw_header::draw_header(f, chunks[0]);
            super::draw_status::draw_queue_stats_strip(f, chunks[2], app);
            draw_queue_screen(f, chunks[4], app);
            let status_msg = app.status_message.as_ref().map(|(s, _)| s.as_str());
            super::draw_footer::draw_footer(
                f,
                chunks[5],
                app.current_screen,
                &mut app.button_map,
                status_msg,
            );
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

/// Draw settings screen showing conversion configuration + password keychain
fn draw_settings_screen(f: &mut Frame, area: ratatui::layout::Rect, app: &mut AppState) {
    use super::theme;
    use ratatui::style::Modifier;
    use ratatui::widgets::{Block, Borders};

    // Ensure keychain is loaded on first visit.
    app.keychain.ensure_loaded();

    // Top-level: settings pane + keychain pane + footer
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(12), // conversion settings
            Constraint::Min(6),     // keychain
            Constraint::Length(2),  // footer
        ])
        .split(area);

    // ── Conversion settings pane ─────────────────────────────────
    let settings_border = if !app.keychain.focused {
        theme::CYAN
    } else {
        theme::TEXT_DIM
    };
    let settings_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(settings_border))
        .title(Span::styled(
            " Conversion Settings ",
            Style::default()
                .fg(theme::CYAN)
                .add_modifier(Modifier::BOLD),
        ));
    let settings_inner = settings_block.inner(chunks[0]);
    f.render_widget(settings_block, chunks[0]);

    let cfg = &app.config.conversion;

    let dest_str = cfg
        .default_destination
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(ask every time)".to_string());
    let scratch_str = cfg
        .scratch_directory
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(system default)".to_string());

    let settings_lines = vec![
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
                if cfg.calculate_replaygain {
                    "Enabled"
                } else {
                    "Disabled"
                },
                Style::default().fg(if cfg.calculate_replaygain {
                    theme::GREEN
                } else {
                    theme::TEXT_DIM
                }),
            ),
        ]),
        Line::from(vec![
            Span::styled("  CUE files:            ", theme::muted()),
            Span::styled(
                if cfg.generate_cue_files {
                    format!("Enabled ({})", cfg.cue_generation_mode)
                } else {
                    "Disabled".to_string()
                },
                Style::default().fg(if cfg.generate_cue_files {
                    theme::GREEN
                } else {
                    theme::TEXT_DIM
                }),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Log files:            ", theme::muted()),
            Span::styled(
                if cfg.write_log_file {
                    "Enabled"
                } else {
                    "Disabled"
                },
                Style::default().fg(if cfg.write_log_file {
                    theme::GREEN
                } else {
                    theme::TEXT_DIM
                }),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Persist queue:        ", theme::muted()),
            Span::styled(
                if cfg.persist_queue {
                    "Enabled"
                } else {
                    "Disabled"
                },
                Style::default().fg(if cfg.persist_queue {
                    theme::GREEN
                } else {
                    theme::TEXT_DIM
                }),
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
    ];

    f.render_widget(Paragraph::new(settings_lines), settings_inner);

    // ── Password keychain pane ───────────────────────────────────
    let kc_border = if app.keychain.focused {
        theme::AMBER
    } else {
        theme::TEXT_DIM
    };
    let kc_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(kc_border))
        .title(Span::styled(
            " Archive Passwords ",
            Style::default()
                .fg(theme::AMBER)
                .add_modifier(Modifier::BOLD),
        ));
    let kc_inner = kc_block.inner(chunks[1]);
    f.render_widget(kc_block, chunks[1]);

    if kc_inner.height < 2 {
        // Too small to render anything.
    } else if app.keychain.passwords.is_empty() {
        let empty_lines = vec![
            Line::from(Span::styled("  No saved passwords", theme::muted())),
            Line::from(Span::styled(
                "  Press 'a' to add a password",
                theme::muted(),
            )),
        ];
        f.render_widget(Paragraph::new(empty_lines), kc_inner);
    } else {
        let visible_rows = kc_inner.height as usize;
        let total = app.keychain.passwords.len();
        let selected = app.keychain.selected.min(total.saturating_sub(1));

        // Simple scroll to keep selected in view.
        let scroll = if selected >= visible_rows {
            selected + 1 - visible_rows
        } else {
            0
        };

        for row in 0..visible_rows {
            let idx = scroll + row;
            if idx >= total {
                break;
            }
            let pw = &app.keychain.passwords[idx];
            let is_sel = idx == selected && app.keychain.focused;

            let display = if app.keychain.reveal {
                format!("  {} {}", idx + 1, pw)
            } else {
                let masked: String = std::iter::repeat('*').take(pw.len().min(20)).collect();
                format!("  {} {}", idx + 1, masked)
            };

            let style = if is_sel {
                Style::default()
                    .bg(ratatui::style::Color::Rgb(52, 56, 80))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::TEXT)
            };

            let row_area =
                ratatui::layout::Rect::new(kc_inner.x, kc_inner.y + row as u16, kc_inner.width, 1);
            f.render_widget(Paragraph::new(Span::styled(display, style)), row_area);
        }
    }

    // Footer
    let status_msg = app.status_message.as_ref().map(|(s, _)| s.as_str());
    super::draw_footer::draw_footer(
        f,
        chunks[2],
        app.current_screen,
        &mut app.button_map,
        status_msg,
    );
}

/// Draw a placeholder screen for unimplemented tabs
fn draw_placeholder_screen(f: &mut Frame, area: ratatui::layout::Rect, app: &mut AppState) {
    use super::theme;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10), Constraint::Length(2)])
        .split(area);

    let label = app.current_screen.tab_label();
    let msg = Paragraph::new(Line::from(vec![Span::styled(
        format!("  {} — coming soon", label),
        theme::muted(),
    )]));
    f.render_widget(msg, chunks[0]);

    let status_msg = app.status_message.as_ref().map(|(s, _)| s.as_str());
    super::draw_footer::draw_footer(
        f,
        chunks[1],
        app.current_screen,
        &mut app.button_map,
        status_msg,
    );
}
