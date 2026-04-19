//! Modal overlay dialogs (confirmation, error detail, item info, file input)

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use crate::convert::ConversionStatus;
use super::app::{ActiveOverlay, AppState, BulkRenameFocus, BulkRenameState, SourceMode};
use super::button_map::TuiButton;
use super::theme;

/// Render a pill-style footer button: ` label ` with colored background.
/// Public alias for use from other modules (help.rs, etc.).
pub fn footer_pill_pub(label: &str, bg: Color) -> Span<'static> {
    footer_pill(label, bg)
}

fn footer_pill(label: &str, bg: Color) -> Span<'static> {
    Span::styled(
        format!(" {} ", label),
        Style::default()
            .fg(theme::PILL_ACTIVE_FG)
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    )
}

/// Public alias for pill_gap (for other modules).
pub fn pill_gap_pub() -> Span<'static> {
    pill_gap()
}

/// One-char gap between footer pills.
fn pill_gap() -> Span<'static> {
    Span::raw(" ")
}

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
        ActiveOverlay::ContextMenu {
            ref entries, selected, origin,
            ref submenu_entries, submenu_selected,
            show_submenu, focus_submenu,
        } => {
            draw_context_menu_side_by_side(
                f, entries, selected, origin,
                submenu_entries, submenu_selected,
                show_submenu, focus_submenu,
            );
        }
        ActiveOverlay::BulkRename(ref state) => {
            let state = state.clone();
            draw_bulk_rename(f, &state);
        }
        ActiveOverlay::Analysis { scroll } => {
            draw_analysis(f, &app.analysis_results, scroll);
        }
        ActiveOverlay::Help { screen, scroll } => {
            super::help::draw_help(f, screen, scroll);
        }
        ActiveOverlay::MetadataEditor(ref state) => {
            draw_metadata_editor(f, state);
        }
        ActiveOverlay::Verify { scroll } => {
            draw_verify(f, &app.verify_results, scroll);
        }
        ActiveOverlay::BitCompare { scroll } => {
            draw_bit_compare(f, &app.compare_results, scroll);
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

/// Draw a context menu with optional side-by-side submenu (hexload-tui
/// pattern). Parent menu at `origin`; when `show_submenu` is true, the
/// child appears to the right of the selected parent item.
#[allow(clippy::too_many_arguments)]
fn draw_context_menu_side_by_side(
    f: &mut Frame,
    entries: &[super::context_menu::ContextMenuEntry],
    selected: usize,
    origin: (u16, u16),
    submenu_entries: &[super::context_menu::ContextMenuEntry],
    submenu_selected: usize,
    show_submenu: bool,
    focus_submenu: bool,
) {
    let area = f.size();

    // Draw parent menu.
    let parent_rect = render_menu_panel(
        f, entries, selected, origin, area,
        !focus_submenu, // highlighted border when focused
    );

    // Draw submenu to the right if active.
    if show_submenu && !submenu_entries.is_empty() {
        let sub_x = parent_rect.x + parent_rect.width - 1;
        let sub_y = parent_rect.y + selected_entry_row(entries, selected) + 1;
        render_menu_panel(
            f,
            submenu_entries,
            submenu_selected,
            (sub_x, sub_y),
            area,
            focus_submenu,
        );
    }
}

/// Find the row offset (within the menu body, 0-indexed) of the
/// `selected`-th selectable entry. Public alias for keybindings.rs.
pub fn selected_entry_row_pub(
    entries: &[super::context_menu::ContextMenuEntry],
    selected: usize,
) -> u16 {
    selected_entry_row(entries, selected)
}

fn selected_entry_row(
    entries: &[super::context_menu::ContextMenuEntry],
    selected: usize,
) -> u16 {
    use super::context_menu::ContextMenuEntry;
    let mut count = 0usize;
    for (i, e) in entries.iter().enumerate() {
        let is_selectable = matches!(
            e,
            ContextMenuEntry::Item(item) if item.enabled
        ) || matches!(e, ContextMenuEntry::Submenu { .. });
        if is_selectable {
            if count == selected {
                return i as u16;
            }
            count += 1;
        }
    }
    0
}

/// Render a single menu panel at the given origin, clipped to `area`.
/// Returns the actual Rect used (for positioning child menus).
fn render_menu_panel(
    f: &mut Frame,
    entries: &[super::context_menu::ContextMenuEntry],
    selected: usize,
    origin: (u16, u16),
    area: Rect,
    has_focus: bool,
) -> Rect {
    use super::context_menu::ContextMenuEntry;

    if entries.is_empty() {
        return Rect::default();
    }

    let max_label_w: usize = entries
        .iter()
        .filter_map(|e| match e {
            ContextMenuEntry::Item(item) => {
                let shortcut_w = item
                    .shortcut
                    .as_ref()
                    .map(|s| s.chars().count() + 3)
                    .unwrap_or(0);
                Some(item.label.chars().count() + shortcut_w)
            }
            ContextMenuEntry::Submenu { label, .. } => Some(label.chars().count() + 2),
            ContextMenuEntry::Separator => None,
        })
        .max()
        .unwrap_or(10);

    let menu_w = (max_label_w + 4).min(area.width as usize) as u16;
    let menu_h = (entries.len() + 2).min(area.height as usize) as u16;

    let x = origin.0.min(area.width.saturating_sub(menu_w));
    let y = origin.1.min(area.height.saturating_sub(menu_h));
    let popup = Rect::new(x, y, menu_w, menu_h);

    f.render_widget(Clear, popup);
    let border_color = if has_focus { theme::AMBER } else { theme::BORDER_DIM };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let selectable_indices: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            ContextMenuEntry::Item(item) if item.enabled => Some(i),
            ContextMenuEntry::Submenu { .. } => Some(i),
            _ => None,
        })
        .collect();

    let inner_w = inner.width as usize;
    let lines: Vec<Line> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| match entry {
            ContextMenuEntry::Separator => Line::from(Span::styled(
                "─".repeat(inner_w),
                Style::default().fg(theme::BORDER_DIM),
            )),
            ContextMenuEntry::Item(item) => {
                let is_selected = has_focus
                    && selectable_indices
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
                Line::from(Span::styled(
                    format!(" {}{}{}", item.label, " ".repeat(pad), shortcut_str),
                    style,
                ))
            }
            ContextMenuEntry::Submenu { label, .. } => {
                let is_selected = has_focus
                    && selectable_indices
                        .get(selected)
                        .map(|&idx| idx == i)
                        .unwrap_or(false);
                // Highlight parent submenu entry even when focus is in
                // the child (so the user sees which parent is expanded).
                let is_expanded = !has_focus
                    && selectable_indices
                        .get(selected)
                        .map(|&idx| idx == i)
                        .unwrap_or(false);
                let style = if is_selected || is_expanded {
                    Style::default()
                        .fg(theme::BG)
                        .bg(theme::BLUE)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::TEXT_BRIGHT)
                };
                let indicator = " >";
                let label_w = label.chars().count();
                let indicator_w = indicator.chars().count();
                let pad = inner_w.saturating_sub(1 + label_w + indicator_w);
                Line::from(Span::styled(
                    format!(" {}{}{}", label, " ".repeat(pad), indicator),
                    style,
                ))
            }
        })
        .collect();

    f.render_widget(Paragraph::new(lines), inner);

    popup
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

    // Footer pills.
    let hint = Paragraph::new(Line::from(vec![
        footer_pill("d remove", theme::RED),
        pill_gap(),
        footer_pill("Esc close", theme::PURPLE),
    ])).alignment(Alignment::Center);
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
        footer_pill("Esc close", theme::PURPLE),
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

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

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
    ];

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), chunks[0]);
    f.render_widget(
        Paragraph::new(Line::from(footer_pill("Esc close", theme::PURPLE)))
            .alignment(Alignment::Center),
        chunks[1],
    );
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
        footer_pill("Enter confirm", theme::GREEN),
        pill_gap(),
        footer_pill("Esc cancel", theme::PURPLE),
    ])).alignment(Alignment::Center);
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

    let help = Paragraph::new(Line::from(vec![
        footer_pill("Enter save", theme::GREEN),
        pill_gap(),
        footer_pill("Esc cancel", theme::PURPLE),
    ])).alignment(Alignment::Center);
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

/// Draw the bulk rename wizard overlay.
fn draw_bulk_rename(f: &mut Frame, state: &BulkRenameState) {
    use super::rename_plan::OpStatus;

    let area = f.size();
    // Use ~85% of screen, bounded to reasonable limits.
    let w = (area.width * 85 / 100).max(60).min(area.width.saturating_sub(2));
    let h = (area.height * 85 / 100).max(16).min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let popup = Rect::new(x, y, w, h);

    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::AMBER))
        .title(Span::styled(
            " Bulk Rename -- Template ",
            Style::default().fg(theme::AMBER).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if inner.height < 6 || inner.width < 20 {
        return;
    }

    // Layout: template(1) + hint(1) + blank(1) + summary(1) + separator(1) + list(rest) + footer(1)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // template input
            Constraint::Length(1), // placeholder hints
            Constraint::Length(1), // blank
            Constraint::Length(1), // summary
            Constraint::Length(1), // separator
            Constraint::Min(1),    // preview list
            Constraint::Length(1), // footer
        ])
        .split(inner);

    // ── Template input ───────────────────────────────────────────
    let template_focused = state.focus == BulkRenameFocus::Template;
    let label_style = if template_focused {
        Style::default().fg(theme::AMBER).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::TEXT_MUTED)
    };
    let input_w = chunks[0].width.saturating_sub(11) as usize; // "Template: " = 10 chars + 1
    let (visible, cursor_col) = state.template_input.view(input_w);
    let input_style = if template_focused {
        Style::default().fg(theme::TEXT_BRIGHT)
    } else {
        Style::default().fg(theme::TEXT_MUTED)
    };
    let template_line = Line::from(vec![
        Span::styled("Template: ", label_style),
        Span::styled(visible, input_style),
    ]);
    f.render_widget(Paragraph::new(template_line), chunks[0]);

    if template_focused {
        f.set_cursor(chunks[0].x + 10 + cursor_col, chunks[0].y);
    }

    // ── Placeholder hints ────────────────────────────────────────
    let hint = Line::from(Span::styled(
        "%N% %NN% %NNN% %TITLE% %ARTIST% %ALBUM% %YEAR% %GENRE% %CATALOG% %EXT%",
        Style::default().fg(theme::TEXT_MUTED),
    ));
    f.render_widget(Paragraph::new(hint), chunks[1]);

    // ── Summary ──────────────────────────────────────────────────
    let total = state.plan.ops.len();
    let pending = state.plan.pending_count();
    let conflicts = state.plan.conflict_count();
    let failed = state.plan.ops.iter()
        .filter(|op| matches!(op.status, OpStatus::Failed(_)))
        .count();
    let skipped = total.saturating_sub(pending + conflicts + failed);
    let mut summary_spans = vec![
        Span::styled(format!("{} files", total), Style::default().fg(theme::TEXT_BRIGHT)),
        Span::styled(" · ", Style::default().fg(theme::TEXT_MUTED)),
        Span::styled(
            format!("{} pending", pending),
            Style::default().fg(theme::GREEN),
        ),
    ];
    if skipped > 0 {
        summary_spans.push(Span::styled(
            format!(" · {} skipped", skipped),
            Style::default().fg(theme::TEXT_MUTED),
        ));
    }
    if conflicts > 0 {
        summary_spans.push(Span::styled(
            format!(" · {} conflict{}", conflicts, if conflicts == 1 { "" } else { "s" }),
            Style::default().fg(theme::RED),
        ));
    }
    if failed > 0 {
        summary_spans.push(Span::styled(
            format!(" · {} failed", failed),
            Style::default().fg(theme::RED),
        ));
    }
    let summary = Line::from(summary_spans);
    f.render_widget(Paragraph::new(summary), chunks[3]);

    // ── Separator ────────────────────────────────────────────────
    let sep = "─".repeat(chunks[4].width as usize);
    f.render_widget(
        Paragraph::new(Span::styled(sep, Style::default().fg(theme::TEXT_MUTED))),
        chunks[4],
    );

    // ── Preview list ─────────────────────────────────────────────
    let list_area = chunks[5];
    let visible_rows = list_area.height as usize;

    if total > 0 && visible_rows > 0 {
        // Clamp scroll so the selected row is always visible.
        let selected = state.selected.min(total.saturating_sub(1));
        let scroll = {
            let mut s = state.scroll;
            if selected < s {
                s = selected;
            }
            if selected >= s + visible_rows {
                s = selected + 1 - visible_rows;
            }
            s
        };

        // Determine column widths: status(3) + target(half) + arrow(4) + source(rest)
        let full_w = list_area.width as usize;
        let status_w = 3;
        let arrow_w = 4; // " <- "
        let remaining = full_w.saturating_sub(status_w + arrow_w);
        let target_w = remaining * 3 / 5;
        let source_w = remaining.saturating_sub(target_w);

        for row in 0..visible_rows {
            let idx = scroll + row;
            if idx >= total {
                break;
            }
            let op = &state.plan.ops[idx];
            let is_selected = idx == selected && state.focus == BulkRenameFocus::List;

            let (icon, icon_color) = match &op.status {
                OpStatus::Pending => (">>", theme::GREEN),
                OpStatus::Skipped(_) => ("..", theme::TEXT_MUTED),
                OpStatus::Conflict => ("!!", theme::RED),
                OpStatus::Succeeded => ("ok", theme::GREEN),
                OpStatus::Failed(_) => ("!!", theme::RED),
            };

            let target_name = &op.target_relative;
            let source_name = state.sources[idx]
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();

            // Truncate names to fit columns (char-safe).
            let target_chars: usize = target_name.chars().count();
            let target_display: String = if target_chars > target_w {
                let truncated: String = target_name.chars().take(target_w.saturating_sub(1)).collect();
                format!("{}~", truncated)
            } else {
                format!("{:<width$}", target_name, width = target_w)
            };
            let source_chars: usize = source_name.chars().count();
            let source_display: String = if source_chars > source_w {
                let truncated: String = source_name.chars().take(source_w.saturating_sub(1)).collect();
                format!("{}~", truncated)
            } else {
                format!("{:<width$}", source_name, width = source_w)
            };

            let target_style = match &op.status {
                OpStatus::Pending => Style::default().fg(theme::TEXT_BRIGHT),
                OpStatus::Skipped(_) => Style::default().fg(theme::TEXT_MUTED),
                OpStatus::Conflict => Style::default().fg(theme::RED),
                OpStatus::Succeeded => Style::default().fg(theme::GREEN),
                OpStatus::Failed(_) => Style::default().fg(theme::RED),
            };

            let line = Line::from(vec![
                Span::styled(format!("{:<3}", icon), Style::default().fg(icon_color)),
                Span::styled(target_display, target_style),
                Span::styled(" <- ", Style::default().fg(theme::TEXT_MUTED)),
                Span::styled(source_display, Style::default().fg(theme::TEXT_MUTED)),
            ]);

            let row_area = Rect::new(
                list_area.x,
                list_area.y + row as u16,
                list_area.width,
                1,
            );

            if is_selected {
                let sel_style = Style::default()
                    .bg(Color::Rgb(52, 56, 80))
                    .add_modifier(Modifier::BOLD);
                f.render_widget(Paragraph::new(line).style(sel_style), row_area);
            } else {
                f.render_widget(Paragraph::new(line), row_area);
            }
        }
    }

    // ── Footer pills ────────────────────────────────────────────
    let footer_parts = if state.focus == BulkRenameFocus::Template {
        vec![
            footer_pill("Tab list", theme::AMBER),
            pill_gap(),
            footer_pill("Enter commit", theme::GREEN),
            pill_gap(),
            footer_pill("Esc cancel", theme::PURPLE),
        ]
    } else {
        vec![
            footer_pill("Tab tmpl", theme::AMBER),
            pill_gap(),
            footer_pill("e edit", theme::CYAN),
            pill_gap(),
            footer_pill("c cue", theme::CYAN),
            pill_gap(),
            footer_pill("C caps", theme::CYAN),
            pill_gap(),
            footer_pill("Enter commit", theme::GREEN),
            pill_gap(),
            footer_pill("Esc cancel", theme::PURPLE),
        ]
    };
    f.render_widget(
        Paragraph::new(Line::from(footer_parts)).alignment(Alignment::Center),
        chunks[6],
    );
}

/// Draw the analysis results overlay.
fn draw_analysis(
    f: &mut Frame,
    results: &[super::analyze::AnalysisResult],
    scroll: usize,
) {
    use super::analyze::dr_label;

    let area = f.size();
    let w = (area.width * 80 / 100).max(50).min(area.width.saturating_sub(2));
    let h = (area.height * 80 / 100).max(12).min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let popup = Rect::new(x, y, w, h);

    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::PURPLE))
        .title(Span::styled(
            " Analysis Results ",
            Style::default().fg(theme::PURPLE).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if inner.height < 3 || results.is_empty() {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    // Build content lines.
    let mut lines: Vec<Line> = Vec::new();
    let label_w = 22;

    for (i, r) in results.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        let name = r.path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| r.path.display().to_string());
        lines.push(Line::from(Span::styled(
            format!("  {}", name),
            Style::default().fg(theme::AMBER).add_modifier(Modifier::BOLD),
        )));

        let dr_color = match r.dr_value {
            0..=3 => theme::RED,
            4..=7 => theme::AMBER,
            8..=13 => theme::GREEN,
            _ => theme::CYAN,
        };

        let entries: Vec<(&str, String, Color)> = vec![
            ("Dynamic Range", format!("DR{} ({})", r.dr_value, dr_label(r.dr_value)), dr_color),
            ("Sample Peak", format!("{:.1} dBFS", r.peak_db), theme::TEXT_BRIGHT),
            ("RMS Level", format!("{:.1} dBFS", r.rms_db), theme::TEXT_BRIGHT),
            ("Clipping", if r.clipping_count == 0 {
                "None".into()
            } else {
                format!("{} samples", r.clipping_count)
            }, if r.clipping_count == 0 { theme::GREEN } else { theme::RED }),
            ("DC Bias", if r.dc_bias.abs() < 0.001 {
                format!("{:.6} (negligible)", r.dc_bias)
            } else {
                format!("{:.6} (significant!)", r.dc_bias)
            }, if r.dc_bias.abs() < 0.001 { theme::TEXT_MUTED } else { theme::AMBER }),
            ("Bit Depth", format!(
                "{}-bit{}",
                r.actual_bit_depth,
                r.declared_bit_depth.map(|d| if d != r.actual_bit_depth {
                    format!(" ({} declared)", d)
                } else {
                    String::new()
                }).unwrap_or_default()
            ), if r.declared_bit_depth.map(|d| d != r.actual_bit_depth).unwrap_or(false) {
                theme::AMBER
            } else {
                theme::TEXT_BRIGHT
            }),
        ];

        // LUFS + true peak (if available).
        let mut extra: Vec<(&str, String, Color)> = Vec::new();
        if let Some(lufs) = r.lufs {
            extra.push(("Loudness", format!("{:.1} LUFS", lufs), theme::TEXT_BRIGHT));
        }
        if let Some(tp) = r.true_peak_dbtp {
            extra.push(("True Peak", format!("{:.1} dBTP", tp), theme::TEXT_BRIGHT));
        }

        for (label, value, color) in entries.iter().chain(extra.iter()) {
            lines.push(Line::from(vec![
                Span::styled(format!("    {:<width$}", label, width = label_w), theme::muted()),
                Span::styled(value.clone(), Style::default().fg(*color)),
            ]));
        }
    }

    let total = lines.len();
    let visible = chunks[0].height as usize;
    let scroll = scroll.min(total.saturating_sub(visible));

    let visible_lines: Vec<Line> = lines.into_iter().skip(scroll).take(visible).collect();
    f.render_widget(Paragraph::new(visible_lines), chunks[0]);

    // Footer pills.
    let footer = Line::from(vec![
        footer_pill("w track RG", theme::GREEN),
        pill_gap(),
        footer_pill("W album+track RG", theme::GREEN),
        pill_gap(),
        footer_pill("Esc close", theme::PURPLE),
    ]);
    f.render_widget(
        Paragraph::new(footer).alignment(Alignment::Center),
        chunks[1],
    );
}

/// Draw the full metadata editor overlay.
fn draw_metadata_editor(f: &mut Frame, state: &super::app::MetadataEditorState) {
    use super::app::MetadataEditorPhase;

    let area = f.size();
    let w = (area.width * 85 / 100).max(50).min(area.width.saturating_sub(2));
    let h = (area.height * 85 / 100).max(14).min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let popup = Rect::new(x, y, w, h);

    f.render_widget(Clear, popup);

    let title = if state.paths.len() == 1 {
        let name = state.paths[0].file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        format!(" Metadata: {} ", name)
    } else {
        format!(" Metadata: {} files ", state.paths.len())
    };
    let border_color = if state.dirty { theme::AMBER } else { theme::CYAN };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(title, Style::default().fg(border_color).add_modifier(Modifier::BOLD)));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if inner.height < 3 { return; }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let content_h = chunks[0].height as usize;
    let inner_w = chunks[0].width as usize;

    // Detail edit mode: show per-file values for one field.
    if state.phase == MetadataEditorPhase::DetailEdit {
        draw_metadata_detail(f, state, chunks[0], chunks[1], inner_w, content_h);
        return;
    }

    let key_col_w = 22;

    // Build content lines.
    let total_rows = state.entries.len() + 1; // +1 for "+ Add field..."
    let scroll = state.scroll.min(total_rows.saturating_sub(content_h));

    let mut lines: Vec<Line> = Vec::new();

    for (i, entry) in state.entries.iter().enumerate() {
        let is_cursor = i == state.cursor;
        let is_deleted = state.deleted.contains(&i);
        let is_dirty = !is_deleted && (entry.value != entry.original
            || entry.per_file_values.iter().zip(entry.per_file_originals.iter())
                .any(|(v, o)| v != o));

        // Key label.
        let key_display = if entry.display_key.len() > key_col_w - 2 {
            format!(" {:.width$}", entry.display_key, width = key_col_w - 2)
        } else {
            format!(" {:<width$}", entry.display_key, width = key_col_w - 2)
        };

        let key_style = if is_deleted {
            Style::default().fg(theme::TEXT_DIM).add_modifier(Modifier::CROSSED_OUT)
        } else if is_cursor {
            Style::default().fg(theme::AMBER).add_modifier(Modifier::BOLD)
        } else {
            theme::muted()
        };

        // Value — show inline editor if this row is being edited.
        let value_display = if is_cursor && state.phase == MetadataEditorPhase::InlineEdit {
            if let Some(ref input) = state.edit_input {
                let (visible, cursor_col) = input.view(inner_w.saturating_sub(key_col_w + 2));
                let cp = cursor_col as usize;
                let before = if cp <= visible.len() { &visible[..cp] } else { &visible[..] };
                let cursor_ch = if cp < visible.len() { &visible[cp..cp + 1] } else { " " };
                let after = if cp + 1 <= visible.len() { &visible[cp + 1..] } else { "" };
                lines.push(Line::from(vec![
                    Span::styled(key_display, key_style),
                    Span::styled(before.to_string(), Style::default().fg(theme::TEXT_BRIGHT)),
                    Span::styled(cursor_ch.to_string(), Style::default().fg(theme::BG).bg(theme::TEXT_BRIGHT)),
                    Span::styled(after.to_string(), Style::default().fg(theme::TEXT_BRIGHT)),
                ]));
                continue;
            }
            entry.value.clone()
        } else if is_deleted {
            format!("(deleted)")
        } else if entry.is_binary {
            entry.value.clone()
        } else {
            entry.value.clone()
        };

        let val_style = if is_deleted {
            Style::default().fg(theme::RED).add_modifier(Modifier::CROSSED_OUT)
        } else if is_dirty {
            Style::default().fg(theme::GREEN)
        } else if entry.is_mixed {
            Style::default().fg(theme::AMBER).add_modifier(Modifier::ITALIC)
        } else if entry.is_binary {
            Style::default().fg(theme::TEXT_DIM)
        } else {
            Style::default().fg(theme::TEXT_BRIGHT)
        };

        let val_truncated = if value_display.len() > inner_w.saturating_sub(key_col_w + 1) {
            format!("{}...", &value_display[..inner_w.saturating_sub(key_col_w + 4)])
        } else {
            value_display
        };

        lines.push(Line::from(vec![
            Span::styled(key_display, key_style),
            Span::styled(val_truncated, val_style),
        ]));
    }

    // "+ Add field..." row
    let add_row = state.entries.len();
    let is_cursor_add = state.cursor == add_row;
    if state.phase == MetadataEditorPhase::AddingKey {
        if let Some(ref input) = state.add_key_input {
            let (visible, cursor_col) = input.view(inner_w.saturating_sub(4));
            let cp = cursor_col as usize;
            let before = if cp <= visible.len() { &visible[..cp] } else { &visible[..] };
            let cursor_ch = if cp < visible.len() { &visible[cp..cp + 1] } else { " " };
            let after = if cp + 1 <= visible.len() { &visible[cp + 1..] } else { "" };
            lines.push(Line::from(vec![
                Span::styled(" + ", Style::default().fg(theme::GREEN).add_modifier(Modifier::BOLD)),
                Span::styled(before.to_string(), Style::default().fg(theme::TEXT_BRIGHT)),
                Span::styled(cursor_ch.to_string(), Style::default().fg(theme::BG).bg(theme::TEXT_BRIGHT)),
                Span::styled(after.to_string(), Style::default().fg(theme::TEXT_BRIGHT)),
            ]));
        }
    } else {
        let add_style = if is_cursor_add {
            Style::default().fg(theme::GREEN).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::TEXT_DIM)
        };
        lines.push(Line::from(Span::styled(" + Add field...", add_style)));
    }

    // Apply scroll.
    let visible_lines: Vec<Line> = lines.into_iter().skip(scroll).take(content_h).collect();
    f.render_widget(Paragraph::new(visible_lines), chunks[0]);

    // Footer: clickable pill-style buttons with key hints.
    let footer = match state.phase {
        MetadataEditorPhase::Editing => Line::from(vec![
            footer_pill("d delete", theme::RED),
            pill_gap(),
            footer_pill("u undo", theme::AMBER),
            pill_gap(),
            footer_pill("a add", theme::CYAN),
            pill_gap(),
            footer_pill("w save", theme::GREEN),
            pill_gap(),
            footer_pill("Esc close", theme::PURPLE),
        ]),
        MetadataEditorPhase::InlineEdit | MetadataEditorPhase::AddingKey => Line::from(vec![
            footer_pill("Enter confirm", theme::GREEN),
            pill_gap(),
            footer_pill("Esc cancel", theme::PURPLE),
        ]),
        MetadataEditorPhase::Saving => Line::from(
            Span::styled(" Saving... ", Style::default().fg(theme::AMBER)),
        ),
        MetadataEditorPhase::DetailEdit => {
            // Unreachable — DetailEdit returns early above.
            Line::from("")
        }
    };
    f.render_widget(Paragraph::new(footer).alignment(Alignment::Center), chunks[1]);
}

/// Render the per-file detail view within the metadata editor.
fn draw_metadata_detail(
    f: &mut Frame,
    state: &super::app::MetadataEditorState,
    content_area: Rect,
    footer_area: Rect,
    inner_w: usize,
    content_h: usize,
) {
    let field_idx = state.detail_field_idx;
    let entry = match state.entries.get(field_idx) {
        Some(e) => e,
        None => return,
    };

    // Header: field name.
    // Label column width: enough for "D99.99" (6) + padding, or filename fallback.
    let max_label = state.file_labels.iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(6);
    let label_col_w = (max_label + 4).min(inner_w / 3).max(10);
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("  {}", entry.display_key),
        Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    // Per-file rows.
    for (i, val) in entry.per_file_values.iter().enumerate() {
        let is_cursor = i == state.detail_cursor;
        let label = state.file_labels.get(i)
            .map(|l| l.as_str())
            .unwrap_or("?");
        let label_display = format!("  {:<width$}  ", label, width = label_col_w - 4);

        let label_style = if is_cursor {
            Style::default().fg(theme::AMBER).add_modifier(Modifier::BOLD)
        } else {
            theme::muted()
        };

        // Inline edit within detail?
        if is_cursor && state.detail_edit.is_some() {
            if let Some(ref input) = state.detail_edit {
                let (visible, cursor_col) = input.view(inner_w.saturating_sub(label_col_w + 2));
                let cp = cursor_col as usize;
                let before = if cp <= visible.len() { &visible[..cp] } else { &visible[..] };
                let cursor_ch = if cp < visible.len() { &visible[cp..cp + 1] } else { " " };
                let after = if cp + 1 <= visible.len() { &visible[cp + 1..] } else { "" };
                lines.push(Line::from(vec![
                    Span::styled(label_display.clone(), label_style),
                    Span::styled(before.to_string(), Style::default().fg(theme::TEXT_BRIGHT)),
                    Span::styled(cursor_ch.to_string(), Style::default().fg(theme::BG).bg(theme::TEXT_BRIGHT)),
                    Span::styled(after.to_string(), Style::default().fg(theme::TEXT_BRIGHT)),
                ]));
                continue;
            }
        }

        let changed = entry.per_file_originals.get(i)
            .map(|o| o != val)
            .unwrap_or(false);
        let val_style = if changed {
            Style::default().fg(theme::GREEN)
        } else if is_cursor {
            Style::default().fg(theme::TEXT_BRIGHT)
        } else {
            Style::default().fg(theme::TEXT_BRIGHT)
        };

        let val_display = if val.len() > inner_w.saturating_sub(label_col_w + 1) {
            format!("{}...", &val[..inner_w.saturating_sub(label_col_w + 4)])
        } else {
            val.clone()
        };

        lines.push(Line::from(vec![
            Span::styled(label_display, label_style),
            Span::styled(val_display, val_style),
        ]));
    }

    // Scroll.
    let total = lines.len();
    let scroll = state.detail_scroll.min(total.saturating_sub(content_h));
    let visible_lines: Vec<Line> = lines.into_iter().skip(scroll).take(content_h).collect();
    f.render_widget(Paragraph::new(visible_lines), content_area);

    // Footer.
    let footer = if state.detail_edit.is_some() {
        Line::from(vec![
            footer_pill("Enter confirm", theme::GREEN),
            pill_gap(),
            footer_pill("Esc cancel", theme::PURPLE),
        ])
    } else {
        Line::from(vec![
            footer_pill("Enter edit", theme::GREEN),
            pill_gap(),
            footer_pill("Esc back", theme::PURPLE),
        ])
    };
    f.render_widget(Paragraph::new(footer).alignment(Alignment::Center), footer_area);
}

/// Draw the verify results overlay.
fn draw_verify(
    f: &mut Frame,
    results: &[super::verify::VerifyResult],
    scroll: usize,
) {
    let area = f.size();
    let w = (area.width * 70 / 100).max(40).min(area.width.saturating_sub(2));
    let h = (area.height * 70 / 100).max(10).min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let popup = Rect::new(x, y, w, h);

    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::GREEN))
        .title(Span::styled(
            " Verify Results ",
            Style::default().fg(theme::GREEN).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if inner.height < 3 || results.is_empty() {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    // Summary line.
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.len() - passed;
    let mut lines: Vec<Line> = Vec::new();

    let summary_spans = if failed == 0 {
        vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("{} passed", passed),
                Style::default().fg(theme::GREEN).add_modifier(Modifier::BOLD),
            ),
        ]
    } else {
        vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("{} passed", passed),
                Style::default().fg(theme::GREEN),
            ),
            Span::styled(", ", theme::muted()),
            Span::styled(
                format!("{} failed", failed),
                Style::default().fg(theme::RED).add_modifier(Modifier::BOLD),
            ),
        ]
    };
    lines.push(Line::from(summary_spans));
    lines.push(Line::from(""));

    // Per-file results.
    for r in results {
        let name = r.path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| r.path.display().to_string());

        let (icon, icon_color) = if r.passed {
            (" ✓ ", theme::GREEN)
        } else {
            (" ✗ ", theme::RED)
        };

        lines.push(Line::from(vec![
            Span::styled(icon, Style::default().fg(icon_color).add_modifier(Modifier::BOLD)),
            Span::styled(name, Style::default().fg(theme::TEXT_BRIGHT)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("   ", Style::default()),
            Span::styled(&r.detail, Style::default().fg(theme::TEXT_DIM)),
        ]));
    }

    let total = lines.len();
    let visible = chunks[0].height as usize;
    let scroll = scroll.min(total.saturating_sub(visible));

    let visible_lines: Vec<Line> = lines.into_iter().skip(scroll).take(visible).collect();
    f.render_widget(Paragraph::new(visible_lines), chunks[0]);

    // Footer pill.
    let footer = Line::from(vec![
        footer_pill("Esc close", theme::GREEN),
    ]);
    f.render_widget(
        Paragraph::new(footer).alignment(Alignment::Center),
        chunks[1],
    );
}

/// Draw the bit-compare results overlay.
fn draw_bit_compare(
    f: &mut Frame,
    results: &[super::bit_compare::CompareResult],
    scroll: usize,
) {
    let area = f.size();
    let w = (area.width * 75 / 100).max(50).min(area.width.saturating_sub(2));
    let h = (area.height * 70 / 100).max(10).min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let popup = Rect::new(x, y, w, h);

    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::CYAN))
        .title(Span::styled(
            " Bit Compare Results ",
            Style::default().fg(theme::CYAN).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if inner.height < 3 || results.is_empty() {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    // Summary line.
    let identical = results.iter().filter(|r| r.identical).count();
    let differ = results.len() - identical;
    let mut lines: Vec<Line> = Vec::new();

    let summary_spans = if differ == 0 {
        vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("{} pair(s): all bit-identical", identical),
                Style::default().fg(theme::GREEN).add_modifier(Modifier::BOLD),
            ),
        ]
    } else {
        vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("{} identical", identical),
                Style::default().fg(theme::GREEN),
            ),
            Span::styled(", ", theme::muted()),
            Span::styled(
                format!("{} differ", differ),
                Style::default().fg(theme::RED).add_modifier(Modifier::BOLD),
            ),
        ]
    };
    lines.push(Line::from(summary_spans));
    lines.push(Line::from(""));

    // Per-pair results.
    for r in results {
        let ref_name = r.ref_path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| r.ref_path.display().to_string());
        let target_name = r.target_path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| r.target_path.display().to_string());

        let (icon, icon_color) = if r.identical {
            (" ✓ ", theme::GREEN)
        } else {
            (" ✗ ", theme::RED)
        };

        lines.push(Line::from(vec![
            Span::styled(icon, Style::default().fg(icon_color).add_modifier(Modifier::BOLD)),
            Span::styled(ref_name, Style::default().fg(theme::TEXT_BRIGHT)),
            Span::styled("  vs  ", theme::muted()),
            Span::styled(target_name, Style::default().fg(theme::TEXT_BRIGHT)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("   ", Style::default()),
            Span::styled(
                r.detail.clone(),
                Style::default().fg(if r.identical { theme::TEXT_DIM } else { theme::RED }),
            ),
        ]));
    }

    let total = lines.len();
    let visible = chunks[0].height as usize;
    let scroll = scroll.min(total.saturating_sub(visible));

    let visible_lines: Vec<Line> = lines.into_iter().skip(scroll).take(visible).collect();
    f.render_widget(Paragraph::new(visible_lines), chunks[0]);

    // Footer pill.
    let footer = Line::from(vec![
        footer_pill("Esc close", theme::CYAN),
    ]);
    f.render_widget(
        Paragraph::new(footer).alignment(Alignment::Center),
        chunks[1],
    );
}

