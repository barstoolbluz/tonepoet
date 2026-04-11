//! Modal overlay dialogs (confirmation, error detail, item info, file input)

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::convert::ConversionStatus;
use super::app::{ActiveOverlay, AppState};
use super::button_map::TuiButton;

/// Draw any active overlay on top of the main content
pub fn draw_overlay(f: &mut Frame, app: &mut AppState) {
    // Clone overlay data to avoid borrow issues
    let overlay = app.active_overlay.clone();
    match overlay {
        ActiveOverlay::None => {}
        ActiveOverlay::Confirmation { ref message, .. } => {
            let message = message.clone();
            draw_confirmation(f, &message, app);
        }
        ActiveOverlay::ErrorDetail { error, .. } => {
            draw_error_detail(f, &error);
        }
        ActiveOverlay::ItemInfo { ref item } => {
            draw_item_info(f, item);
        }
        ActiveOverlay::FileInput { ref input } => {
            let input = input.clone();
            draw_file_input(f, &input);
        }
        ActiveOverlay::CommandInput { ref input } => {
            let input = input.clone();
            draw_command_input(f, &input);
        }
        ActiveOverlay::TextEdit { ref input, ref label, .. } => {
            let input = input.clone();
            let label = label.clone();
            draw_text_edit(f, &label, &input);
        }
    }

    // Preset overlay (independent of ActiveOverlay — uses its own flag)
    if app.preset.overlay_open {
        super::presets_overlay::draw_presets_overlay(f, &app.preset);
    }

    // Recent files overlay (independent of ActiveOverlay — uses its own flag)
    if app.recent.overlay_open {
        super::recent_overlay::draw_recent_overlay(f, &mut app.recent);
    }

    // Bookmarks overlay (independent of ActiveOverlay — uses its own flag)
    if app.bookmarks.overlay_open {
        super::bookmarks_overlay::draw_bookmarks_overlay(f, &mut app.bookmarks);
    }
}

/// Center a rect within a parent area
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

/// Draw a confirmation dialog
fn draw_confirmation(f: &mut Frame, message: &str, app: &mut AppState) {
    let area = f.size();
    let popup = centered_rect(50, 7, area);

    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Span::styled(" Confirm ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // message
            Constraint::Length(1), // buttons
        ])
        .split(inner);

    let msg = Paragraph::new(message.to_string())
        .wrap(Wrap { trim: true })
        .alignment(Alignment::Center);
    f.render_widget(msg, chunks[0]);

    let buttons = Paragraph::new(Line::from(vec![
        Span::styled(" [Y]es ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(" [N]o ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
    ]))
    .alignment(Alignment::Center);
    f.render_widget(buttons, chunks[1]);

    // Record button areas
    let btn_y = chunks[1].y;
    let center_x = chunks[1].x + chunks[1].width / 2;
    app.button_map.record_button(TuiButton::OverlayConfirm, Rect::new(center_x.saturating_sub(8), btn_y, 7, 1));
    app.button_map.record_button(TuiButton::OverlayCancel, Rect::new(center_x + 2, btn_y, 6, 1));
}

/// Draw an error detail popup
fn draw_error_detail(f: &mut Frame, error: &str) {
    let area = f.size();
    let popup = centered_rect(60, 12, area);

    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .title(Span::styled(" Error Detail ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let error_text = Paragraph::new(error.to_string())
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(Color::Red));
    f.render_widget(error_text, chunks[0]);

    let hint = Paragraph::new(Line::from(vec![
        Span::styled("Press Esc to close", Style::default().fg(Color::DarkGray)),
    ]))
    .alignment(Alignment::Center);
    f.render_widget(hint, chunks[1]);
}

/// Draw item info popup
fn draw_item_info(f: &mut Frame, item: &crate::convert::ConversionItem) {
    let area = f.size();
    let popup = centered_rect(70, 16, area);

    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Span::styled(" Item Info ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let name = item.input_path.file_name().unwrap_or_default().to_string_lossy();
    let size = humansize::format_size(item.file_size, humansize::BINARY);
    let status_str = match &item.status {
        ConversionStatus::NotConfigured => "Not Configured".to_string(),
        ConversionStatus::Queued => "Queued".to_string(),
        ConversionStatus::Processing { progress, phase, .. } => {
            let phase_name = phase.as_ref().map(|p| p.display_name()).unwrap_or("Processing");
            format!("{:.1}% - {}", progress, phase_name)
        }
        ConversionStatus::Completed { output_path } => {
            format!("Completed -> {}", output_path.display())
        }
        ConversionStatus::Failed { error } => format!("Failed: {}", error),
        ConversionStatus::Paused => "Paused".to_string(),
        ConversionStatus::Cancelled => "Cancelled".to_string(),
    };

    let lines = vec![
        Line::from(vec![
            Span::styled("File: ", Style::default().fg(Color::Gray)),
            Span::styled(name.to_string(), Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("Path: ", Style::default().fg(Color::Gray)),
            Span::styled(
                item.input_path.parent().unwrap_or(&item.input_path).display().to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(vec![
            Span::styled("Size: ", Style::default().fg(Color::Gray)),
            Span::styled(size, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("Input: ", Style::default().fg(Color::Gray)),
            Span::styled(format!("{:?}", item.input_format), Style::default().fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::styled("Output: ", Style::default().fg(Color::Gray)),
            Span::styled(item.output_format.name().to_string(), Style::default().fg(Color::Green)),
        ]),
        Line::from(vec![
            Span::styled("Status: ", Style::default().fg(Color::Gray)),
            Span::styled(status_str, Style::default().fg(Color::Yellow)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Press Esc to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let info = Paragraph::new(lines).wrap(Wrap { trim: true });
    f.render_widget(info, inner);
}

/// Draw file input overlay for adding files
fn draw_file_input(f: &mut Frame, input: &super::text_input::TextInputState) {
    let area = f.size();
    let popup = centered_rect(60, 7, area);

    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(Span::styled(" Add File/Folder Path ", Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // hint
            Constraint::Length(1), // input
            Constraint::Length(1), // help
        ])
        .split(inner);

    let hint = Paragraph::new(Line::from(vec![
        Span::styled("Enter a file or folder path:", Style::default().fg(Color::Gray)),
    ]));
    f.render_widget(hint, chunks[0]);

    // Scrolled view of the input
    let visible_width = chunks[1].width as usize;
    let (view, cursor_col) = input.view(visible_width);
    let display_input = if view.is_empty() { " ".to_string() } else { view };
    let input_widget = Paragraph::new(Line::from(vec![
        Span::styled(display_input, Style::default().fg(Color::White)),
    ]))
    .style(Style::default().bg(Color::Rgb(40, 40, 40)));
    f.render_widget(input_widget, chunks[1]);

    f.set_cursor(chunks[1].x + cursor_col, chunks[1].y);

    let help = Paragraph::new(Line::from(vec![
        Span::styled("Enter to confirm, Esc to cancel", Style::default().fg(Color::DarkGray)),
    ]));
    f.render_widget(help, chunks[2]);
}

/// Draw a generic text edit overlay (for editing a single field)
fn draw_text_edit(f: &mut Frame, label: &str, input: &super::text_input::TextInputState) {
    let area = f.size();
    // Dynamic popup width: 80 if terminal allows it, otherwise shrink to fit.
    // Reserve 4 cols of margin (2 each side) when room allows.
    let popup_width = area.width.saturating_sub(4).min(80);
    let popup = centered_rect(popup_width, 7, area);

    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(Span::styled(
            format!(" Edit {} ", label),
            Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // hint
            Constraint::Length(1), // input
            Constraint::Length(1), // help
        ])
        .split(inner);

    let hint = Paragraph::new(Line::from(vec![
        Span::styled(
            format!("Enter new {}:", label),
            Style::default().fg(Color::Gray),
        ),
    ]));
    f.render_widget(hint, chunks[0]);

    let visible_width = chunks[1].width as usize;
    let (view, cursor_col) = input.view(visible_width);
    let display_input = if view.is_empty() { " ".to_string() } else { view };
    let input_widget = Paragraph::new(Line::from(vec![Span::styled(
        display_input,
        Style::default().fg(Color::White),
    )]))
    .style(Style::default().bg(Color::Rgb(40, 40, 40)));
    f.render_widget(input_widget, chunks[1]);

    f.set_cursor(chunks[1].x + cursor_col, chunks[1].y);

    let help = Paragraph::new(Line::from(vec![Span::styled(
        "Enter to save, Esc to cancel",
        Style::default().fg(Color::DarkGray),
    )]));
    f.render_widget(help, chunks[2]);
}

/// Draw the vim-style command line at the bottom of the screen
fn draw_command_input(f: &mut Frame, input: &super::text_input::TextInputState) {
    let area = f.size();
    // Command line occupies the very last row
    let cmd_area = Rect::new(area.x, area.y + area.height.saturating_sub(1), area.width, 1);

    // Clear the line
    f.render_widget(Clear, cmd_area);

    // Leave 1 col for ":" prefix
    let visible_width = (cmd_area.width as usize).saturating_sub(1);
    let (view, cursor_col) = input.view(visible_width);

    // Render ": <input>"
    let line = Line::from(vec![
        Span::styled(":", Style::default().fg(Color::Rgb(122, 162, 247))), // blue
        Span::styled(view, Style::default().fg(Color::Rgb(192, 202, 245))), // bright
    ]);

    let cmd = Paragraph::new(line)
        .style(Style::default().bg(Color::Rgb(26, 27, 38))); // BG color
    f.render_widget(cmd, cmd_area);

    // Position cursor after the ':'
    f.set_cursor(cmd_area.x + 1 + cursor_col, cmd_area.y);
}
