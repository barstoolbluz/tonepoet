//! Queue list rendering with per-item progress bars

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use super::app::AppState;
use super::button_map::TuiButton;
use crate::convert::{ConversionItem, ConversionStatus};

/// Draw the queue content area (item list + action bar)
pub fn draw_queue_screen(f: &mut Frame, area: Rect, app: &mut AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(4),    // item list
            Constraint::Length(2), // action bar
        ])
        .split(area);

    draw_item_list(f, chunks[0], app);
    draw_action_bar(f, chunks[1], app);
}

/// Draw the scrollable queue item list
fn draw_item_list(f: &mut Frame, area: Rect, app: &mut AppState) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            format!(" Queue ({}) ", app.items_snapshot.len()),
            Style::default().fg(Color::White),
        ));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.items_snapshot.is_empty() {
        let empty = Paragraph::new(Line::from(vec![Span::styled(
            "No files in queue. Press 'a' to add files or 'f' to add a folder.",
            Style::default().fg(Color::DarkGray),
        )]));
        f.render_widget(empty, inner);
        return;
    }

    let items = &app.items_snapshot;
    let start = app.scroll_offset;
    let mut y = inner.y;
    let mut visible_count = 0_usize;

    for idx in start..items.len() {
        if y >= inner.y + inner.height {
            break;
        }
        let item = &items[idx];
        let item_area = Rect::new(inner.x, y, inner.width, 1);

        let is_hovered = app.hover_target == Some(TuiButton::QueueItem(idx));
        draw_queue_item(
            f,
            item_area,
            item,
            idx == app.selected_index,
            is_hovered,
            app,
        );
        app.button_map
            .record_button(TuiButton::QueueItem(idx), item_area);
        // Register a narrow hit target over the indicator character for expand/collapse.
        if !item.active_tracks.is_empty() {
            let expand_area = Rect::new(inner.x, item_area.y, 2, 1);
            app.button_map
                .record_button(TuiButton::QueueItemExpand(idx), expand_area);
        }
        y += 1;
        visible_count += 1;

        // Render detail line for items with extra context
        let detail_line: Option<(String, Color)> = match &item.status {
            ConversionStatus::Processing {
                message: Some(msg),
                phase,
                ..
            } if !msg.is_empty() => Some((msg.clone(), super::theme::GREEN)),
            ConversionStatus::Completed { output_path, .. } => {
                let path = output_path.display().to_string();
                if !path.is_empty() {
                    Some((path, super::theme::GREEN))
                } else {
                    None
                }
            }
            ConversionStatus::Partial {
                output_path,
                successful,
                failed,
                ..
            } => {
                let path = output_path.display().to_string();
                if !path.is_empty() {
                    Some((
                        format!(
                            "{}/{} ok \u{2192} {}",
                            successful,
                            successful + failed,
                            path
                        ),
                        Color::Yellow,
                    ))
                } else {
                    None
                }
            }
            ConversionStatus::Failed { error, .. } if !error.is_empty() => {
                Some((error.clone(), Color::Red))
            }
            _ => None,
        };
        // Render per-track sub-lines for multi-track sources.
        if !item.active_tracks.is_empty() && !item.tracks_collapsed {
            let max_sub_lines = 5_usize;
            let total_tracks = item.active_tracks.len();
            for (shown, (_idx, tp)) in item.active_tracks.iter().enumerate() {
                if y >= inner.y + inner.height {
                    break;
                }
                if shown >= max_sub_lines {
                    let more = total_tracks - max_sub_lines;
                    let more_area = Rect::new(inner.x, y, inner.width, 1);
                    let more_text = Paragraph::new(Line::from(vec![
                        Span::styled("  \u{2514} ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            format!("... and {} more tracks", more),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                    f.render_widget(more_text, more_area);
                    y += 1;
                    break;
                }
                let pct = (tp.progress_fraction * 100.0).round() as u8;
                let sub_text = if tp.step_description.is_empty() {
                    format!("{} \u{00b7} {}%", tp.track_label, pct)
                } else {
                    format!(
                        "{} \u{00b7} {} \u{00b7} {}%",
                        tp.track_label, tp.step_description, pct
                    )
                };
                let sub_area = Rect::new(inner.x, y, inner.width, 1);
                let sub_line = Paragraph::new(Line::from(vec![
                    Span::styled("  \u{2514} ", Style::default().fg(Color::DarkGray)),
                    Span::styled(sub_text, Style::default().fg(super::theme::BLUE)),
                ]));
                f.render_widget(sub_line, sub_area);
                y += 1;
            }
        } else if !item.active_tracks.is_empty() && item.tracks_collapsed {
            // Collapsed summary line.
            if y < inner.y + inner.height {
                let n = item.active_tracks.len();
                let summary_area = Rect::new(inner.x, y, inner.width, 1);
                let summary = Paragraph::new(Line::from(vec![
                    Span::styled("  \u{2514} ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{n} tracks converting\u{2026}"),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
                f.render_widget(summary, summary_area);
                y += 1;
            }
        } else if let Some((text, color)) = detail_line {
            // Single-file compat: render the single detail line as before.
            if y < inner.y + inner.height {
                let detail_area = Rect::new(inner.x, y, inner.width, 1);
                let detail = Paragraph::new(Line::from(vec![
                    Span::styled("  \u{2514} ", Style::default().fg(Color::DarkGray)),
                    Span::styled(text, Style::default().fg(color)),
                ]));
                f.render_widget(detail, detail_area);
                y += 1;
            }
        }
    }

    app.visible_height = visible_count;
}

/// Draw a single queue item on one line
fn draw_queue_item(
    f: &mut Frame,
    area: Rect,
    item: &ConversionItem,
    is_selected: bool,
    is_hovered: bool,
    _app: &AppState,
) {
    if area.width < 10 {
        return;
    }

    // Selection indicator — show expand/collapse arrow when tracks are active.
    let has_tracks = !item.active_tracks.is_empty();
    let sel_char = if item.selected {
        "*"
    } else if has_tracks && !item.tracks_collapsed {
        "\u{25bc}" // ▼ expanded
    } else if has_tracks && item.tracks_collapsed {
        "\u{25b6}" // ▶ collapsed
    } else if is_selected {
        ">"
    } else {
        " "
    };
    let sel_style = if is_selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    // File name (truncated)
    let name = item
        .input_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let name_budget = (area.width as usize).saturating_sub(40);
    let shift = (area.width as usize) / 10; // give ~10% more to status/progress
    let max_name_len = name_budget.saturating_sub(shift).max(8);
    let display_name: String = if name.len() > max_name_len && max_name_len > 3 {
        let truncate_at = max_name_len - 3;
        // Truncate by chars to avoid splitting multi-byte characters
        let truncated: String = name.chars().take(truncate_at).collect();
        format!("{}...", truncated)
    } else {
        format!("{:width$}", name, width = max_name_len)
    };

    // Output format
    let format_str = format!("{:>5}", item.output_format.name());

    // Status rendering
    let (status_spans, _status_style) =
        render_item_status(item, area.width.saturating_sub(max_name_len as u16 + 12));

    let mut spans = vec![
        Span::styled(format!("{} ", sel_char), sel_style),
        Span::styled(
            display_name,
            if is_selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            },
        ),
        Span::styled(
            format!("  {} ", format_str),
            Style::default().fg(Color::Cyan),
        ),
    ];
    spans.extend(status_spans);

    // Apply row highlight for selected/hovered item
    let row_style = if is_selected {
        Style::default().bg(Color::Rgb(30, 30, 50))
    } else if is_hovered {
        Style::default().bg(super::theme::HOVER_BG)
    } else {
        Style::default()
    };

    let line = Paragraph::new(Line::from(spans)).style(row_style);
    f.render_widget(line, area);

    // For processing items, draw a CRT aperture-grille progress bar.
    // Use overall progress (0-100 across all pipeline stages) rather than
    // phase_progress (0-100 within the current stage) so the bar advances
    // continuously instead of resetting to 0 at each phase boundary.
    if let ConversionStatus::Processing {
        progress: overall_progress,
        phase,
        ..
    } = &item.status
    {
        let progress_width = area.width.saturating_sub(max_name_len as u16 + 12);
        if progress_width > 10 {
            let pct_value = *overall_progress;
            let phase_label = phase
                .as_ref()
                .map(|p| p.short_name())
                .unwrap_or("Processing");
            let label = format!("{:.0}% {}", pct_value, phase_label);

            let gauge_area = Rect::new(
                area.x + max_name_len as u16 + 10,
                area.y,
                progress_width - 1,
                1,
            );
            let pct = (pct_value / 100.0).clamp(0.0, 1.0);
            draw_crt_gauge(f, gauge_area, pct, &label);
        }
    }
}

/// Render status text for a queue item
fn render_item_status(item: &ConversionItem, _width: u16) -> (Vec<Span<'static>>, Style) {
    match &item.status {
        ConversionStatus::NotConfigured => (
            vec![Span::styled(
                "Not Configured",
                Style::default().fg(Color::DarkGray),
            )],
            Style::default(),
        ),
        ConversionStatus::Queued => (
            vec![Span::styled("Queued", Style::default().fg(Color::White))],
            Style::default(),
        ),
        ConversionStatus::Processing {
            progress: overall_progress,
            phase,
            ..
        } => {
            let pct = *overall_progress;
            let phase_name = phase
                .as_ref()
                .map(|p| p.short_name())
                .unwrap_or("Processing");
            (
                vec![Span::styled(
                    format!("{:.0}% {}", pct, phase_name),
                    Style::default().fg(super::theme::GREEN),
                )],
                Style::default(),
            )
        }
        ConversionStatus::Completed { .. } => (
            vec![Span::styled(
                "Completed",
                Style::default().fg(super::theme::GREEN),
            )],
            Style::default(),
        ),
        ConversionStatus::Partial {
            successful, failed, ..
        } => (
            vec![Span::styled(
                format!("Partial {}/{}", successful, successful + failed),
                Style::default().fg(Color::Yellow),
            )],
            Style::default(),
        ),
        ConversionStatus::Failed { .. } => (
            vec![Span::styled("Failed", Style::default().fg(Color::Red))],
            Style::default(),
        ),
        ConversionStatus::Paused => (
            vec![Span::styled("Paused", Style::default().fg(Color::Yellow))],
            Style::default(),
        ),
        ConversionStatus::Cancelled => (
            vec![Span::styled(
                "Cancelled",
                Style::default().fg(Color::DarkGray),
            )],
            Style::default(),
        ),
    }
}

/// Get color for a conversion phase
/// Draw a progress bar with a CRT aperture-grille effect.
///
/// Alternates between brighter and dimmer columns in the filled region
/// to simulate the vertical phosphor stripe pattern of a CRT display.
/// Uses `theme::GREEN` (the start pill color) as the base fill color.
fn draw_crt_gauge(f: &mut Frame, area: Rect, ratio: f32, label: &str) {
    use super::theme;

    if area.is_empty() {
        return;
    }

    let buf = f.buffer_mut();
    let filled_width = ((area.width as f32) * ratio).round() as u16;

    let fill_color = theme::GREEN;
    let bg_empty = Color::Rgb(30, 30, 30);

    let y = area.y;
    for col in 0..area.width {
        let x = area.x + col;
        let cell = buf.get_mut(x, y);
        if col < filled_width {
            cell.set_symbol("\u{2588}") // █ full block
                .set_fg(fill_color)
                .set_bg(bg_empty);
        } else {
            cell.set_symbol(" ").set_fg(bg_empty).set_bg(bg_empty);
        }
    }

    // Overlay the label, centered.
    let label_width = label.len().min(area.width as usize);
    let label_start = area.x + (area.width.saturating_sub(label_width as u16)) / 2;
    for (i, ch) in label.chars().take(label_width).enumerate() {
        let x = label_start + i as u16;
        let col = x - area.x;
        let cell = buf.get_mut(x, y);
        cell.set_char(ch);
        if col < filled_width {
            cell.set_fg(Color::Rgb(15, 15, 15)).set_bg(fill_color);
        } else {
            cell.set_fg(Color::Rgb(120, 120, 120)).set_bg(bg_empty);
        }
    }
}

/// Draw the action bar at the bottom of the queue screen
fn draw_action_bar(f: &mut Frame, area: Rect, app: &mut AppState) {
    use super::draw_overlays::{footer_pill_pub, pill_gap_pub};
    use super::theme;

    let pills: Vec<(&str, TuiButton, Color)> = vec![
        ("a add files", TuiButton::AddFiles, theme::BLUE),
        ("f add folder", TuiButton::AddFolder, theme::BLUE),
        ("c configure", TuiButton::Configure, theme::PURPLE),
        ("s start", TuiButton::Convert, theme::GREEN),
        ("p pause", TuiButton::Pause, theme::AMBER),
        ("x stop", TuiButton::Stop, theme::RED),
        ("r retry", TuiButton::RetryFailed, theme::CYAN),
        ("C-l clear done", TuiButton::ClearFinished, theme::PURPLE),
        ("clear all", TuiButton::ClearAll, theme::RED),
    ];

    let mut spans: Vec<Span> = Vec::new();
    let mut x = area.x;

    for (i, (label, btn, color)) in pills.iter().enumerate() {
        if i > 0 {
            let gap = pill_gap_pub();
            x += 1; // gap is 1 char
            spans.push(gap);
        }
        let pill = footer_pill_pub(label, *color);
        let pill_width = (label.len() + 2) as u16; // " label " padding
        app.button_map
            .record_button(*btn, Rect::new(x, area.y, pill_width, 1));
        x += pill_width;
        spans.push(pill);
    }

    let bar = Paragraph::new(Line::from(spans));
    f.render_widget(bar, area);
}
