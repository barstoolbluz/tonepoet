//! Top-level draw_ui() dispatching by screen

use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{AppScreen, AppState, ConfigFocus};
use super::convert_screen::draw_convert_screen;
use super::draw_overlays::draw_overlay;
use super::draw_queue::draw_queue_screen;

/// Main draw function dispatching to screen-specific renderers
pub fn draw_ui(f: &mut Frame, app: &mut AppState) {
    let theme = app.theme;
    // Clear button map for this frame
    app.button_map.clear();

    // Background fill
    let bg = Paragraph::new("").style(Style::default().bg(theme.bg));
    f.render_widget(bg, f.size());

    match app.current_screen {
        AppScreen::Convert => {
            draw_convert_screen(f, f.size(), app, theme);
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

            super::draw_header::draw_header(f, chunks[0], theme);
            super::draw_status::draw_queue_stats_strip(f, chunks[2], app, theme);
            draw_queue_screen(f, chunks[4], app, theme);
            let status_msg = app.status_message.as_ref().map(|(s, _)| s.as_str());
            super::draw_footer::draw_footer(
                f,
                chunks[5],
                app.current_screen,
                &mut app.button_map,
                status_msg,
                theme,
            );
        }
        AppScreen::Wizard => {
            draw_wizard_screen(f, app, theme);
        }
        AppScreen::Config => {
            draw_settings_screen(f, f.size(), app, theme);
        }
        AppScreen::Browse => {
            super::draw_browse::draw_browse_screen(f, f.size(), app, theme);
        }
        AppScreen::Library => {
            draw_placeholder_screen(f, f.size(), app, theme);
        }
    }

    // Draw any active overlay on top
    draw_overlay(f, app, theme);
}

/// Draw the wizard screen by delegating to tonepoet_wizard
fn draw_wizard_screen(f: &mut Frame, app: &mut AppState, theme: super::theme::Theme) {
    if let Some(wizard) = &app.wizard {
        let areas = tonepoet_wizard::draw_wizard_with_theme(
            f,
            wizard,
            wizard_theme_from_tonepoet_theme(theme),
        );
        app.wizard_mouse_areas = Some(areas);
    } else {
        let msg = Paragraph::new("No wizard active. Press Esc to return.")
            .style(Style::default().fg(theme.text).bg(theme.bg));
        f.render_widget(msg, f.size());
    }
}

fn wizard_theme_from_tonepoet_theme(theme: super::theme::Theme) -> tonepoet_wizard::WizardTheme {
    tonepoet_wizard::WizardTheme {
        background: theme.bg,
        surface: theme.surface,
        overlay: theme.dropdown_bg,
        border: theme.border,
        title: theme.title,
        text: theme.text,
        text_muted: theme.text_muted,
        text_dim: theme.text_dim,
        accent: theme.info,
        selected_bg: theme.selection_bg,
        selected_fg: theme.panel_bg,
        hover_bg: theme.hover_bg,
        focus_bg: theme.input_focused_bg,
        success: theme.success,
        warning: theme.warning,
        error: theme.error,
        error_dim: theme.error_dim,
        disabled_bg: theme.input_disabled_bg,
        disabled_fg: theme.text_dim,
        input_bg: theme.input_unfocused_bg,
    }
}

/// Draw settings screen showing conversion configuration + password keychain
fn draw_settings_screen(f: &mut Frame, area: ratatui::layout::Rect, app: &mut AppState, theme: super::theme::Theme) {
    use ratatui::style::Modifier;
    use ratatui::widgets::{Block, Borders};

    // Ensure keychain is loaded on first visit.
    app.keychain.ensure_loaded();

    // Top-level: appearance pane + settings pane + keychain pane + footer
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),  // appearance settings
            Constraint::Length(12), // conversion settings
            Constraint::Min(6),     // keychain
            Constraint::Length(2),  // footer
        ])
        .split(area);

    // ── Appearance pane ───────────────────────────────────────────
    let appearance_border = if app.config_focus == ConfigFocus::Appearance {
        theme.cyan
    } else {
        theme.text_dim
    };
    let appearance_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(appearance_border))
        .title(Span::styled(
            " Appearance ",
            Style::default()
                .fg(theme.cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let appearance_inner = appearance_block.inner(chunks[0]);
    f.render_widget(appearance_block, chunks[0]);

    let current_theme = &app.theme;
    let theme_line = Line::from(vec![
        Span::styled("  Theme:               ", theme.muted()),
        Span::styled("◀ ", Style::default().fg(theme.text_dim)),
        Span::styled(current_theme.name, theme.bright()),
        Span::styled(" ▶", Style::default().fg(theme.text_dim)),
        Span::styled(
            "   h/l or ←/→ to change",
            Style::default().fg(theme.text_dim),
        ),
    ]);
    let desc_line = Line::from(vec![
        Span::styled("  ", theme.muted()),
        Span::styled(current_theme.description, Style::default().fg(theme.text_muted)),
    ]);
    f.render_widget(Paragraph::new(vec![theme_line, desc_line]), appearance_inner);

    // ── Conversion settings pane ─────────────────────────────────
    let settings_border = if app.config_focus == ConfigFocus::Conversion {
        theme.cyan
    } else {
        theme.text_dim
    };
    let settings_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(settings_border))
        .title(Span::styled(
            " Conversion Settings ",
            Style::default()
                .fg(theme.cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let settings_inner = settings_block.inner(chunks[1]);
    f.render_widget(settings_block, chunks[1]);

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
            Span::styled("  Backend:              ", theme.muted()),
            Span::styled(&cfg.preferred_backend, theme.bright()),
        ]),
        Line::from(vec![
            Span::styled("  Workers:              ", theme.muted()),
            Span::styled(cfg.worker_count.to_string(), theme.bright()),
        ]),
        Line::from(vec![
            Span::styled("  ReplayGain:           ", theme.muted()),
            Span::styled(
                if cfg.calculate_replaygain {
                    "Enabled"
                } else {
                    "Disabled"
                },
                Style::default().fg(if cfg.calculate_replaygain {
                    theme.green
                } else {
                    theme.text_dim
                }),
            ),
        ]),
        Line::from(vec![
            Span::styled("  CUE files:            ", theme.muted()),
            Span::styled(
                if cfg.generate_cue_files {
                    format!("Enabled ({})", cfg.cue_generation_mode)
                } else {
                    "Disabled".to_string()
                },
                Style::default().fg(if cfg.generate_cue_files {
                    theme.green
                } else {
                    theme.text_dim
                }),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Log files:            ", theme.muted()),
            Span::styled(
                if cfg.write_log_file {
                    "Enabled"
                } else {
                    "Disabled"
                },
                Style::default().fg(if cfg.write_log_file {
                    theme.green
                } else {
                    theme.text_dim
                }),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Persist queue:        ", theme.muted()),
            Span::styled(
                if cfg.persist_queue {
                    "Enabled"
                } else {
                    "Disabled"
                },
                Style::default().fg(if cfg.persist_queue {
                    theme.green
                } else {
                    theme.text_dim
                }),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Destination:          ", theme.muted()),
            Span::styled(dest_str, theme.bright()),
        ]),
        Line::from(vec![
            Span::styled("  Scratch directory:    ", theme.muted()),
            Span::styled(scratch_str, theme.bright()),
        ]),
    ];

    f.render_widget(Paragraph::new(settings_lines), settings_inner);

    // ── Password keychain pane ───────────────────────────────────
    let kc_border = if app.config_focus == ConfigFocus::Keychain {
        theme.amber
    } else {
        theme.text_dim
    };
    let kc_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(kc_border))
        .title(Span::styled(
            " Archive Passwords ",
            Style::default()
                .fg(theme.amber)
                .add_modifier(Modifier::BOLD),
        ));
    let kc_inner = kc_block.inner(chunks[2]);
    f.render_widget(kc_block, chunks[2]);

    if kc_inner.height < 2 {
        // Too small to render anything.
    } else if app.keychain.passwords.is_empty() {
        let empty_lines = vec![
            Line::from(Span::styled("  No saved passwords", theme.muted())),
            Line::from(Span::styled(
                "  Press 'a' to add a password",
                theme.muted(),
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
            let is_sel = idx == selected && app.config_focus == ConfigFocus::Keychain;

            let display = if app.keychain.reveal {
                format!("  {} {}", idx + 1, pw)
            } else {
                let masked: String = std::iter::repeat('*').take(pw.len().min(20)).collect();
                format!("  {} {}", idx + 1, masked)
            };

            let style = if is_sel {
                Style::default()
                    .bg(theme.selection_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text)
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
        chunks[3],
        app.current_screen,
        &mut app.button_map,
        status_msg,
        theme,
    );
}

/// Draw a placeholder screen for unimplemented tabs
fn draw_placeholder_screen(f: &mut Frame, area: ratatui::layout::Rect, app: &mut AppState, theme: super::theme::Theme) {

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10), Constraint::Length(2)])
        .split(area);

    let label = app.current_screen.tab_label();
    let msg = Paragraph::new(Line::from(vec![Span::styled(
        format!("  {} — coming soon", label),
        theme.muted(),
    )]));
    f.render_widget(msg, chunks[0]);

    let status_msg = app.status_message.as_ref().map(|(s, _)| s.as_str());
    super::draw_footer::draw_footer(
        f,
        chunks[1],
        app.current_screen,
        &mut app.button_map,
        status_msg,
        theme,
    );
}

#[cfg(test)]
mod theme_render_tests {
    use super::*;
    use crate::config::TonepoetConfig;
    use crate::tui::app::{AppScreen, AppState};
    use crate::tui::test_support::XdgConfigHomeGuard;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn draw_ui_uses_app_theme_on_the_next_frame() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-draw-ui-theme");
        let mut app = AppState::new(TonepoetConfig::default());
        app.current_screen = AppScreen::Library;
        app.theme = crate::tui::theme::theme_by_slug("catppuccin-latte").expect("theme");
        app.config.ui.theme = app.theme.slug.to_string();

        let backend = TestBackend::new(40, 8);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw_ui(frame, &mut app)).expect("draw");

        let cell = terminal.backend().buffer().get(0, 0);
        assert_eq!(cell.bg, app.theme.bg);
    }

    #[test]
    fn wizard_fallback_uses_app_theme() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-wizard-theme");
        let mut app = AppState::new(TonepoetConfig::default());
        app.current_screen = AppScreen::Wizard;
        app.wizard = None;
        app.theme = crate::tui::theme::theme_by_slug("catppuccin-latte").expect("theme");
        app.config.ui.theme = app.theme.slug.to_string();

        let backend = TestBackend::new(48, 4);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw_ui(frame, &mut app)).expect("draw");

        let cell = terminal.backend().buffer().get(0, 0);
        assert_eq!(cell.fg, app.theme.text);
        assert_eq!(cell.bg, app.theme.bg);
    }

    #[test]
    fn active_wizard_uses_app_theme() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-active-wizard-theme");
        let mut app = AppState::new(TonepoetConfig::default());
        app.current_screen = AppScreen::Wizard;
        app.wizard = Some(tonepoet_wizard::SimpleWizard::new());
        app.theme = crate::tui::theme::theme_by_slug("catppuccin-latte").expect("theme");
        app.config.ui.theme = app.theme.slug.to_string();

        let backend = TestBackend::new(120, 50);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw_ui(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        assert!(
            buffer.content().iter().any(|cell| cell.bg == app.theme.bg),
            "active wizard should render background from app.theme.bg"
        );
        assert!(
            buffer.content().iter().any(|cell| cell.fg == app.theme.border),
            "active wizard should render border foreground from app.theme.border"
        );
        assert!(
            buffer.content().iter().any(|cell| cell.fg == app.theme.title),
            "active wizard should render title foreground from app.theme.title"
        );
        assert!(
            buffer.content().iter().any(|cell| cell.fg == app.theme.info),
            "active wizard should render accent foreground from app.theme.info"
        );
        assert!(
            buffer.content().iter().any(|cell| cell.bg == app.theme.selection_bg),
            "active wizard should render selected/highlight background from app.theme.selection_bg"
        );
    }

    #[test]
    fn wizard_theme_adapter_maps_tonepoet_theme_roles() {
        let theme = crate::tui::theme::theme_by_slug("tokyo-night").expect("theme");
        let mapped = wizard_theme_from_tonepoet_theme(theme);
        assert_eq!(mapped.background, theme.bg);
        assert_eq!(mapped.surface, theme.surface);
        assert_eq!(mapped.overlay, theme.dropdown_bg);
        assert_eq!(mapped.border, theme.border);
        assert_eq!(mapped.title, theme.title);
        assert_eq!(mapped.text, theme.text);
        assert_eq!(mapped.text_muted, theme.text_muted);
        assert_eq!(mapped.text_dim, theme.text_dim);
        assert_eq!(mapped.accent, theme.info);
        assert_eq!(mapped.selected_bg, theme.selection_bg);
        assert_eq!(mapped.selected_fg, theme.panel_bg);
        assert_eq!(mapped.hover_bg, theme.hover_bg);
        assert_eq!(mapped.focus_bg, theme.input_focused_bg);
        assert_eq!(mapped.error, theme.error);
        assert_eq!(mapped.success, theme.success);
        assert_eq!(mapped.warning, theme.warning);
        assert_eq!(mapped.error_dim, theme.error_dim);
        assert_eq!(mapped.disabled_bg, theme.input_disabled_bg);
        assert_eq!(mapped.disabled_fg, theme.text_dim);
        assert_eq!(mapped.input_bg, theme.input_unfocused_bg);
        assert_ne!(mapped.error_dim, mapped.error);
    }

}
