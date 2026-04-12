//! Modal overlay dialogs (confirmation, error detail, item info, file input)

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::convert::ConversionStatus;
use super::app::{ActiveOverlay, AppState, SourceMode};
use super::button_map::TuiButton;
use super::theme;

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
        ActiveOverlay::CommandInput { ref input, ref completion } => {
            let input = input.clone();
            let completion = completion.clone();
            draw_command_input(f, &input, completion.as_ref());
        }
        ActiveOverlay::TextEdit { ref input, ref label, .. } => {
            let input = input.clone();
            let label = label.clone();
            draw_text_edit(f, &label, &input);
        }
        ActiveOverlay::BatchList { scroll } => {
            draw_batch_list(f, app, scroll);
        }
        ActiveOverlay::ContextMenu { ref entries, selected, origin } => {
            draw_context_menu(f, entries, selected, origin);
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

/// Draw a floating context menu at the given screen origin.
fn draw_context_menu(
    f: &mut Frame,
    entries: &[super::context_menu::ContextMenuEntry],
    selected: usize,
    origin: (u16, u16),
) {
    use super::context_menu::ContextMenuEntry;

    if entries.is_empty() {
        return;
    }

    let area = f.size();

    // Compute menu dimensions from content.
    let max_label_w: usize = entries
        .iter()
        .filter_map(|e| match e {
            ContextMenuEntry::Item(item) => {
                let shortcut_w = item
                    .shortcut
                    .as_ref()
                    .map(|s| s.chars().count() + 3) // "  {shortcut}"
                    .unwrap_or(0);
                Some(item.label.chars().count() + shortcut_w)
            }
            ContextMenuEntry::Separator => None,
        })
        .max()
        .unwrap_or(10);

    let menu_w = (max_label_w + 4).min(area.width as usize) as u16; // 2 border + 2 pad
    let menu_h = (entries.len() + 2).min(area.height as usize) as u16; // + 2 for border

    // Position: try to place at origin, but clip to screen bounds.
    let x = origin.0.min(area.width.saturating_sub(menu_w));
    let y = origin.1.min(area.height.saturating_sub(menu_h));
    let popup = Rect::new(x, y, menu_w, menu_h);

    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER_DIM));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    // Build the selectable-index lookup (skip separators).
    let selectable_indices: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            ContextMenuEntry::Item(item) if item.enabled => Some(i),
            _ => None,
        })
        .collect();

    // Render each entry.
    let inner_w = inner.width as usize;
    let lines: Vec<Line> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            match entry {
                ContextMenuEntry::Separator => {
                    Line::from(Span::styled(
                        "─".repeat(inner_w),
                        Style::default().fg(theme::BORDER_DIM),
                    ))
                }
                ContextMenuEntry::Item(item) => {
                    let is_selected = selectable_indices
                        .get(selected)
                        .map(|&idx| idx == i)
                        .unwrap_or(false);

                    let style = if !item.enabled {
                        Style::default().fg(theme::TEXT_DIM)
                    } else if is_selected {
                        Style::default()
                            .fg(theme::BG)
                            .bg(theme::BLUE)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme::TEXT_BRIGHT)
                    };

                    let shortcut_str = item
                        .shortcut
                        .as_ref()
                        .map(|s| format!("  {}", s))
                        .unwrap_or_default();

                    let label_w = item.label.chars().count();
                    let shortcut_w = shortcut_str.chars().count();
                    let pad = inner_w.saturating_sub(1 + label_w + shortcut_w);

                    let text = format!(
                        " {}{}{}",
                        item.label,
                        " ".repeat(pad),
                        shortcut_str,
                    );

                    Line::from(Span::styled(text, style))
                }
            }
        })
        .collect();

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, inner);
}

/// Center a rect within a parent area
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

/// Draw the BatchList expand overlay: full list of paths in the current
/// batch with the cursor highlighted. Shows a hint line at the bottom
/// with available actions.
///
/// `stored_scroll` is the persistent scroll offset from `ActiveOverlay::
/// BatchList { scroll }`. The renderer clamps it to keep the cursor
/// visible even if the handler's conservative `APPROX_VISIBLE` estimate
/// doesn't match the actual list height — the clamp here is defensive
/// (only fires when list height differs from the estimate) and doesn't
/// feed back into persistent state.
fn draw_batch_list(f: &mut Frame, app: &AppState, stored_scroll: usize) {
    let (paths, cursor) = match &app.convert.source.mode {
        // Defensive: an empty Batch shouldn't exist (from_paths returns
        // Empty for 0 paths, remove collapses to Empty/Single), but if
        // somehow it did we'd panic on paths[scroll..end] below. Bail.
        SourceMode::Batch { paths, cursor, .. } if !paths.is_empty() => {
            (paths.clone(), *cursor)
        }
        _ => return,
    };
    // Additional cursor clamp in case source.mode was mutated with an
    // out-of-bounds cursor somehow (shouldn't happen via our APIs).
    let cursor = cursor.min(paths.len() - 1);

    let area = f.size();
    let popup_w = area.width.saturating_sub(8).min(100).max(40);
    let popup_h = area.height.saturating_sub(6).min(30).max(10);
    let popup = centered_rect(popup_w, popup_h, area);

    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::AMBER))
        .title(Span::styled(
            format!(" batch · {} files ", paths.len()),
            Style::default().fg(theme::AMBER).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    // Split into list area and hint bar at the bottom
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    // Clamp `stored_scroll` to the range that keeps the cursor in
    // view given the actual list height. The handler maintains
    // smooth-scroll semantics using a conservative estimate; this
    // clamp corrects for any difference between the estimate and
    // reality without persisting back into state.
    let list_h = chunks[0].height as usize;
    let scroll = if list_h == 0 {
        0
    } else {
        let min_scroll = cursor.saturating_sub(list_h.saturating_sub(1));
        let max_scroll = cursor.min(paths.len().saturating_sub(list_h));
        stored_scroll.clamp(min_scroll, max_scroll.max(min_scroll))
    };
    let end = (scroll + list_h).min(paths.len());
    let visible = &paths[scroll..end];

    let list_lines: Vec<Line> = visible
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let idx = scroll + i;
            let is_cursor = idx == cursor;
            let name = p.file_name().unwrap_or_default().to_string_lossy();
            let parent = p
                .parent()
                .and_then(|pp| pp.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let text = if parent.is_empty() {
                format!(" {:>4}  {}", idx + 1, name)
            } else {
                format!(" {:>4}  {} · {}", idx + 1, parent, name)
            };
            if is_cursor {
                Line::from(Span::styled(
                    text,
                    Style::default()
                        .fg(theme::BG)
                        .bg(theme::AMBER)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(Span::styled(text, Style::default().fg(theme::TEXT_BRIGHT)))
            }
        })
        .collect();

    let list = Paragraph::new(list_lines);
    f.render_widget(list, chunks[0]);

    // Hint bar
    let hint = Paragraph::new(Line::from(vec![
        Span::styled(" ↑/↓ ", Style::default().fg(theme::BLUE)),
        Span::styled("move", Style::default().fg(theme::TEXT_MUTED)),
        Span::raw("  "),
        Span::styled(" d ", Style::default().fg(theme::RED)),
        Span::styled("remove", Style::default().fg(theme::TEXT_MUTED)),
        Span::raw("  "),
        Span::styled(" enter/esc ", Style::default().fg(theme::GREEN)),
        Span::styled("close", Style::default().fg(theme::TEXT_MUTED)),
    ]));
    f.render_widget(hint, chunks[1]);
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
fn draw_command_input(
    f: &mut Frame,
    input: &super::text_input::TextInputState,
    completion: Option<&super::app::CompletionState>,
) {
    let area = f.size();
    // Command line occupies the very last row; when completion is
    // active with multiple candidates, the row above shows match count
    // and a preview of the next few candidates.
    let has_multi_matches = completion
        .map(|c| c.candidates.len() > 1)
        .unwrap_or(false);

    let cmd_row = area.y + area.height.saturating_sub(1);
    let cmd_area = Rect::new(area.x, cmd_row, area.width, 1);

    // Clear the command line
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

    // Optional hint row above the command line when cycling matches.
    if has_multi_matches && area.height >= 2 {
        let state = completion.expect("has_multi_matches implies Some");
        let hint_row = cmd_row.saturating_sub(1);
        let hint_area = Rect::new(area.x, hint_row, area.width, 1);
        f.render_widget(Clear, hint_area);

        // Format: "[2/5] foo bar baz qux ..."
        let count = format!("[{}/{}] ", state.cursor + 1, state.candidates.len());
        // Show up to 6 candidates starting from the current one for a
        // compact preview; cursor candidate appears first.
        let n = state.candidates.len();
        let preview_n = 6.min(n);
        let mut preview_items = Vec::with_capacity(preview_n);
        for i in 0..preview_n {
            let idx = (state.cursor + i) % n;
            preview_items.push(state.candidates[idx].clone());
        }
        let preview = preview_items.join("  ");
        let elided = if n > preview_n { " …" } else { "" };

        let hint_line = Line::from(vec![
            Span::styled(count, Style::default().fg(Color::Rgb(187, 154, 247))), // purple
            Span::styled(preview, Style::default().fg(Color::Rgb(169, 177, 214))), // muted bright
            Span::styled(elided.to_string(), Style::default().fg(Color::Rgb(86, 95, 137))), // dim
        ]);
        let hint = Paragraph::new(hint_line)
            .style(Style::default().bg(Color::Rgb(26, 27, 38)));
        f.render_widget(hint, hint_area);
    }

    // Position cursor after the ':'
    f.set_cursor(cmd_area.x + 1 + cursor_col, cmd_area.y);
}
