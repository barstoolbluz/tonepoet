//! Top-level draw_ui() dispatching by screen

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::app::{AppScreen, AppState, ConfigFocus};
use super::button_map::TuiButton;
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
fn draw_settings_screen(f: &mut Frame, area: Rect, app: &mut AppState, theme: super::theme::Theme) {
    // Ensure keychain is loaded on first visit. A failed backend access is
    // surfaced in the keychain pane below via `load_error` (and retried on
    // the next explicit user action), so the per-frame Result is redundant
    // here.
    let _ = app.keychain.ensure_loaded();

    // Top-level: appearance pane + conversion pane + performance pane + keychain pane + footer
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(12), // appearance settings
            Constraint::Length(10), // conversion settings
            Constraint::Length(6),  // browsing performance
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
    draw_appearance_pane(f, appearance_inner, app, theme);

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
        Line::from(vec![
            Span::styled("  Scratch memory limit: ", theme.muted()),
            Span::styled(
                format!("{}%", cfg.scratch_memory_limit_percent.min(90)),
                theme.bright(),
            ),
        ]),
    ];

    f.render_widget(Paragraph::new(settings_lines), settings_inner);

    // ── Performance pane ──────────────────────────────────────────
    let perf_border = if app.config_focus == ConfigFocus::Performance {
        theme.cyan
    } else {
        theme.text_dim
    };
    let perf_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(perf_border))
        .title(Span::styled(
            " Performance ",
            Style::default()
                .fg(theme.cyan)
                .add_modifier(Modifier::BOLD),
        ));
    let perf_inner = perf_block.inner(chunks[2]);
    f.render_widget(perf_block, chunks[2]);

    let browsing_cfg = &app.config.performance.browsing;
    let listing_mode = crate::tui::archive_listing::ArchiveListingMode::from_config(
        &browsing_cfg.archive_listing,
    );
    let timeout_label = if browsing_cfg.archive_listing_timeout == 0 {
        "Disabled".to_string()
    } else {
        format!("{}s", browsing_cfg.archive_listing_timeout)
    };
    let performance_lines = vec![
        Line::from(Span::styled("  Browsing", theme.muted())),
        Line::from(vec![
            Span::styled("  Archive listing:      ", theme.muted()),
            Span::styled("◂ ", theme.muted()),
            Span::styled(listing_mode.display_label(), theme.bright()),
            Span::styled(" ▸", theme.muted()),
        ]),
        Line::from(vec![
            Span::styled("  Listing timeout:      ", theme.muted()),
            Span::styled(timeout_label, theme.bright()),
        ]),
        Line::from(Span::styled(
            "  Left/Right changes mode, +/- changes timeout, 0 disables",
            theme.muted(),
        )),
    ];
    f.render_widget(Paragraph::new(performance_lines), perf_inner);

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
    let kc_inner = kc_block.inner(chunks[3]);
    f.render_widget(kc_block, chunks[3]);

    if kc_inner.height < 2 {
        // Too small to render anything.
    } else if let Some(error) = app.keychain.load_error.as_ref() {
        let error_lines = vec![
            Line::from(Span::styled(
                format!("  Keychain unavailable: {error}"),
                Style::default().fg(theme.red),
            )),
            Line::from(Span::styled(
                "  Unlock the platform keychain, then retry with any keychain action",
                theme.muted(),
            )),
        ];
        f.render_widget(Paragraph::new(error_lines), kc_inner);
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
        chunks[4],
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
    use ratatui::style::Color;
    use ratatui::Terminal;

    #[test]
    fn draw_ui_uses_app_theme_on_the_next_frame() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-draw-ui-theme");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
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
        let mut app = AppState::new_for_test(TonepoetConfig::default());
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
        let mut app = AppState::new_for_test(TonepoetConfig::default());
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


    fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        let area = buffer.area;
        let mut out = String::new();
        for y in area.y..area.y.saturating_add(area.height) {
            for x in area.x..area.x.saturating_add(area.width) {
                out.push_str(buffer.get(x, y).symbol());
            }
            out.push('\n');
        }
        out
    }

    fn unique_colors(colors: &[Color]) -> Vec<Color> {
        let mut unique = Vec::new();
        for color in colors.iter().copied() {
            if !unique.contains(&color) {
                unique.push(color);
            }
        }
        unique
    }

    fn rendered_swatch_color_count(buffer: &ratatui::buffer::Buffer, expected_colors: &[Color]) -> usize {
        unique_colors(expected_colors)
            .into_iter()
            .filter(|expected| {
                buffer.content().iter().any(|cell| {
                    cell.symbol().chars().any(|ch| ch == '█')
                        && (cell.fg == *expected || cell.bg == *expected)
                })
            })
            .count()
    }

    fn assert_palette_ribbon_rendered(buffer: &ratatui::buffer::Buffer, expected_colors: &[Color]) {
        let block_cells = buffer.content().iter().filter(|cell| {
            cell.symbol().chars().any(|ch| ch == '█')
                && expected_colors.iter().any(|color| cell.fg == *color || cell.bg == *color)
        }).count();
        assert!(
            block_cells >= expected_colors.len(),
            "palette ribbon should render at least one styled block cell per requested swatch",
        );

        let expected = unique_colors(expected_colors).len();
        let rendered = rendered_swatch_color_count(buffer, expected_colors);
        assert_eq!(
            rendered,
            expected,
            "palette ribbon should render every expected swatch color as styled block cells",
        );
    }

    fn cached_theme_choice() -> crate::tui::theme::ThemeChoice {
        let mut accents = [Color::Rgb(0, 0, 0); crate::tui::theme::THEME_ACCENT_COUNT];
        for (index, color) in accents.iter_mut().enumerate() {
            *color = Color::Rgb(
                10u8.saturating_add(index as u8),
                80u8.saturating_add(index as u8),
                150u8.saturating_add(index as u8),
            );
        }
        crate::tui::theme::ThemeChoice {
            slug: "cached-render-only".to_string(),
            name: "Cached Render Only".to_string(),
            description: "Loaded from injected cache, not discovery".to_string(),
            dark: true,
            built_in: false,
            author_lock_count: 0,
            accents,
        }
    }

    #[test]
    fn appearance_pane_renders_mockup_content_without_old_clutter() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-appearance-pane-render");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Config;
        app.set_config_focus(crate::tui::app::ConfigFocus::Appearance);
        app.theme = crate::tui::theme::theme_by_slug("gruvbox").expect("theme");
        app.config.ui.theme = app.theme.slug.to_string();
        app.refresh_theme_library();
        let expected_accents = app.theme_library.choices()[app.theme_library.active_index(&app.config.ui.theme, app.theme.slug)].accents;

        let backend = TestBackend::new(100, 36);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw_ui(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let text = buffer_text(buffer);
        assert!(text.contains("Appearance"));
        assert!(text.contains("Theme"));
        assert!(text.contains("Gruvbox material"));
        assert!(text.contains("Warm dark Gruvbox material palette"));
        assert!(text.contains(" / "), "theme counter should be visible");
        assert!(text.contains(&format!("{} built-in", app.theme_library.built_in_count())));
        assert!(text.contains("0 custom"));
        assert!(text.contains("Browse all"));
        assert!(text.contains("change"));
        assert!(text.contains("mode"));
        assert!(text.contains("browse"));
        assert!(text.contains("help"));
        assert!(!text.contains("h/l"), "old inline h/l instructions must not render");
        assert!(!text.contains("e Edit"), "old inline Edit instruction must not render");
        assert!(!text.contains("n New"), "old inline New instruction must not render");
        assert!(!text.contains(".config"), "filesystem path trivia must not render");
        assert_palette_ribbon_rendered(buffer, &expected_accents[..10]);
    }

    #[test]
    fn appearance_theme_name_clips_before_palette_ribbon() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-appearance-long-theme-name");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Config;
        app.set_config_focus(crate::tui::app::ConfigFocus::Appearance);
        app.theme = crate::tui::theme::theme_by_slug("gruvbox").expect("theme");
        app.config.ui.theme = app.theme.slug.to_string();
        app.refresh_theme_library();
        let idx = app.theme_library.active_index(&app.config.ui.theme, app.theme.slug);
        app.theme_library.choices[idx].name = "An absurdly long custom theme name that would previously crowd out every palette swatch".to_string();
        let expected_accents = app.theme_library.choices[idx].accents;

        let backend = TestBackend::new(82, 36);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw_ui(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let text = buffer_text(buffer);
        assert!(text.contains('…'), "long theme name should be visibly clipped");
        assert_palette_ribbon_rendered(buffer, &expected_accents[..10]);
    }

    #[test]
    fn appearance_renderer_uses_injected_cached_theme_library() {
        let _xdg = XdgConfigHomeGuard::new("tonepoet-appearance-cache-injection");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Config;
        app.set_config_focus(crate::tui::app::ConfigFocus::Appearance);
        app.theme = crate::tui::theme::theme_by_slug("gruvbox").expect("theme");
        app.config.ui.theme = "cached-render-only".to_string();
        let cached_choice = cached_theme_choice();
        let expected_accents = cached_choice.accents;
        app.theme_library = crate::tui::theme::ThemeLibrarySnapshot { choices: vec![cached_choice] };

        let backend = TestBackend::new(100, 36);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw_ui(frame, &mut app)).expect("draw");

        let buffer = terminal.backend().buffer();
        let text = buffer_text(buffer);
        assert!(text.contains("Cached Render Only"));
        assert!(text.contains("Loaded from injected cache, not discovery"));
        assert!(text.contains("1 / 1"));
        assert!(text.contains("0 built-in"));
        assert!(text.contains("1 custom"));
        assert!(!text.contains("Gruvbox material"), "render should not fall back to runtime theme metadata when cached library data is present");
        assert_palette_ribbon_rendered(buffer, &expected_accents[..10]);
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

fn draw_appearance_pane(f: &mut Frame, area: Rect, app: &mut AppState, theme: super::theme::Theme) {
    let choices = app.theme_library.choices();
    let total = choices.len().max(1);
    let built_in_count = app.theme_library.built_in_count();
    let custom_count = app.theme_library.custom_count();
    let active_index = app.theme_library.active_index(&app.config.ui.theme, app.theme.slug);
    let active_choice = choices.get(active_index);

    let theme_name = active_choice
        .map(|choice| choice.name.as_str())
        .unwrap_or(app.theme.name);
    let description = active_choice
        .map(|choice| choice.description.as_str())
        .unwrap_or(app.theme.description);
    let accents = active_choice
        .map(|choice| choice.accents)
        .unwrap_or(app.theme.accents);
    let counter = format!("{} / {}", active_index.saturating_add(1).min(total), total);

    let label_style = Style::default().fg(theme.label);
    let accent_style = Style::default().fg(theme.tab_active).add_modifier(Modifier::BOLD);
    let dim_style = Style::default().fg(theme.text_dim);
    let muted_style = Style::default().fg(theme.text_muted);
    let active_mode = Style::default().fg(theme.tab_active).add_modifier(Modifier::BOLD);
    let inactive_mode = Style::default().fg(theme.text_dim);
    let chip_style = Style::default().fg(theme.pill_active_fg).bg(theme.pill_active_bg);

    let label_prefix = "  Theme      ";
    let label_width = width_of(label_prefix);
    let prev_token_width = width_of("◂ ");
    let next_token_width = width_of(" ▸  ");
    let swatch_count = accents.iter().take(10).count() as u16;
    let ribbon_width = swatch_count.saturating_mul(width_of("██ "));
    let reserved_width = label_width
        .saturating_add(prev_token_width)
        .saturating_add(next_token_width)
        .saturating_add(ribbon_width);
    let name_width = width_of(theme_name).min(area.width.saturating_sub(reserved_width));

    let theme_y = area.y.saturating_add(1);
    let prev_x = area.x.saturating_add(label_width);
    let name_x = prev_x.saturating_add(prev_token_width);
    let next_x = name_x.saturating_add(name_width).saturating_add(1);
    app.button_map.record_button(TuiButton::ConfigThemePrev, Rect { x: prev_x, y: theme_y, width: 1, height: 1 });
    app.button_map.record_button(TuiButton::ConfigThemeNext, Rect { x: next_x, y: theme_y, width: 1, height: 1 });

    let mut theme_spans = vec![
        Span::styled(label_prefix, label_style),
        Span::styled("◂ ", dim_style),
        Span::styled(clipped(theme_name, name_width as usize), accent_style),
        Span::styled(" ▸  ", dim_style),
    ];
    for accent in accents.iter().take(10) {
        theme_spans.push(Span::styled("██", Style::default().fg(*accent)));
        theme_spans.push(Span::raw(" "));
    }

    let subtitle_prefix = "             ";
    let counter_separator = "   ·   ";
    let subtitle_reserved = width_of(subtitle_prefix)
        .saturating_add(width_of(counter_separator))
        .saturating_add(width_of(&counter));
    let subtitle = clipped(description, area.width.saturating_sub(subtitle_reserved) as usize);
    let subtitle_line = Line::from(vec![
        Span::raw(subtitle_prefix),
        Span::styled(subtitle, muted_style),
        Span::styled(counter_separator, dim_style),
        Span::styled(counter, dim_style),
    ]);

    let mode_y = area.y.saturating_add(4);
    let dark_active = app.theme.dark;
    let dark_style = if dark_active { active_mode } else { inactive_mode };
    let light_style = if dark_active { inactive_mode } else { active_mode };
    let dark_token = if dark_active { "● Dark" } else { "○ Dark" };
    let light_token = if dark_active { "○ Light" } else { "● Light" };
    let mode_x = area.x.saturating_add(width_of("  Mode       "));
    app.button_map.record_button(TuiButton::ConfigThemeMode, Rect { x: mode_x, y: mode_y, width: 18, height: 1 });
    app.button_map.record_button(TuiButton::ConfigThemeDark, Rect { x: mode_x, y: mode_y, width: width_of(dark_token), height: 1 });
    app.button_map.record_button(TuiButton::ConfigThemeLight, Rect { x: mode_x.saturating_add(width_of(dark_token)).saturating_add(2), y: mode_y, width: width_of(light_token), height: 1 });

    let library_y = area.y.saturating_add(6);
    let library_label = format!("{} built-in · {} custom", built_in_count, custom_count);
    let browse_label = "[ Browse all … ]";
    let browse_width = width_of(browse_label);
    let browse_x = area.x.saturating_add(area.width.saturating_sub(browse_width.saturating_add(2))).saturating_add(1);
    app.button_map.record_button(TuiButton::ConfigThemeBrowse, Rect { x: browse_x, y: library_y, width: browse_width, height: 1 });
    let library_used = width_of("  Library    ").saturating_add(width_of(&library_label)).saturating_add(browse_width);
    let library_gap = " ".repeat(area.width.saturating_sub(library_used).saturating_sub(1) as usize);

    let separator_area = Rect {
        x: area.x.saturating_sub(1),
        y: area.y.saturating_add(8),
        width: area.width.saturating_add(2),
        height: 1,
    };
    let separator = format!("├{}┤", "─".repeat(area.width as usize));
    let separator_style = Style::default()
        .fg(if app.config_focus == ConfigFocus::Appearance { theme.cyan } else { theme.text_dim })
        .bg(theme.panel_bg);

    let footer_line = Line::from(vec![
        Span::raw("  "),
        Span::styled("←/→", accent_style),
        Span::styled(" change   ", dim_style),
        Span::styled("m", accent_style),
        Span::styled(" mode   ", dim_style),
        Span::styled("e", accent_style),
        Span::styled(" edit   ", dim_style),
        Span::styled("n", accent_style),
        Span::styled(" new   ", dim_style),
        Span::styled("b", accent_style),
        Span::styled(" browse   ", dim_style),
        Span::styled("?", accent_style),
        Span::styled(" help", dim_style),
    ]);

    let lines = vec![
        Line::raw(""),
        Line::from(theme_spans),
        subtitle_line,
        Line::raw(""),
        Line::from(vec![
            Span::styled("  Mode       ", label_style),
            Span::styled(dark_token, dark_style),
            Span::raw("  "),
            Span::styled(light_token, light_style),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled("  Library    ", label_style),
            Span::styled(library_label, muted_style),
            Span::raw(library_gap),
            Span::styled(browse_label, chip_style),
        ]),
        Line::raw(""),
        Line::raw(""),
        footer_line,
    ];

    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme.panel_bg)), area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(separator, separator_style))),
        separator_area,
    );
}

fn width_of(value: &str) -> u16 {
    value.chars().count().min(u16::MAX as usize) as u16
}

fn clipped(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    if max_chars == 1 {
        return "…".to_string();
    }
    let mut out: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}
