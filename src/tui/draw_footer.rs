//! Footer: 5-tab view bar + context keybinding bar

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{AppScreen, FileTaskFooterState};
use super::button_map::{ButtonRenderMap, TuiButton};

/// Draw both footer rows (tabs + context bar) into a 2-line area.
/// Also registers clickable regions for the tab bar into `buttons`.
/// When `status_message` is Some, the context bar shows the message instead of hints.
pub fn draw_footer(
    f: &mut Frame,
    area: Rect,
    current_screen: AppScreen,
    buttons: &mut ButtonRenderMap,
    status_message: Option<&str>,
    file_task: Option<FileTaskFooterState>,
    theme: super::theme::Theme,
) {
    if area.height < 2 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    draw_tab_bar(f, chunks[0], current_screen, buttons, theme);
    draw_context_bar(
        f,
        chunks[1],
        current_screen,
        buttons,
        status_message,
        file_task,
        theme,
    );
}

/// Draw the numbered tab bar: 1 convert | 2 browse | 3 library | 4 queue | 5 config
fn draw_tab_bar(f: &mut Frame, area: Rect, current: AppScreen, buttons: &mut ButtonRenderMap, theme: super::theme::Theme) {
    let tabs = AppScreen::tabs();
    let tab_width = area.width as usize / tabs.len();

    // Register clickable regions for each tab slot (matches rendering math).
    for i in 0..tabs.len() {
        let x = area.x + (i * tab_width) as u16;
        buttons.record_button(
            TuiButton::Tab(i as u8 + 1),
            Rect::new(x, area.y, tab_width as u16, 1),
        );
    }

    let mut spans: Vec<Span> = Vec::new();

    for (i, tab) in tabs.iter().enumerate() {
        let num = tab.tab_number().unwrap_or(0);
        let label = tab.tab_label();
        let is_active = *tab == current;

        if i > 0 {
            spans.push(Span::styled("│", Style::default().fg(theme.border_dim)));
        }

        // Key badge
        let key_style = if is_active {
            Style::default()
                .fg(theme.bg)
                .bg(theme.blue)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.tab_inactive).bg(theme.border_dim)
        };

        let label_style = if is_active {
            Style::default().fg(theme.text_bright)
        } else {
            Style::default().fg(theme.tab_inactive)
        };

        // Pad to distribute evenly
        let cell_content = format!(" {} {} ", num, label);
        let pad = tab_width.saturating_sub(cell_content.len());
        let pad_left = pad / 2;
        let pad_right = pad.saturating_sub(pad_left);

        spans.push(Span::raw(" ".repeat(pad_left)));
        spans.push(Span::styled(format!("{}", num), key_style));
        spans.push(Span::styled(format!(" {}", label), label_style));
        spans.push(Span::raw(" ".repeat(pad_right)));
    }

    let bar = Paragraph::new(Line::from(spans)).style(Style::default().bg(theme.surface));
    f.render_widget(bar, area);
}

/// Draw the context-sensitive keybinding bar.
/// If `status_message` is Some, render the message in amber instead of keybinding hints.
fn draw_context_bar(
    f: &mut Frame,
    area: Rect,
    current: AppScreen,
    buttons: &mut ButtonRenderMap,
    status_message: Option<&str>,
    file_task: Option<FileTaskFooterState>,
    theme: super::theme::Theme,
) {
    let details_label = file_task_details_label(area.width, file_task);
    let details_width = details_label.chars().count() as u16;
    let content_area = Rect::new(
        area.x,
        area.y,
        area.width.saturating_sub(details_width),
        area.height,
    );

    // When a transient status message is set, it replaces the hints on this row.
    if let Some(msg) = status_message {
        let max_chars = (content_area.width as usize).saturating_sub(2);
        let display: String = if msg.chars().count() > max_chars && max_chars >= 2 {
            let t: String = msg.chars().take(max_chars - 1).collect();
            format!(" {}…", t)
        } else {
            format!(" {}", msg)
        };
        let bar = Paragraph::new(Line::from(Span::styled(
            display,
            Style::default()
                .fg(theme.amber)
                .add_modifier(Modifier::BOLD),
        )));
        f.render_widget(bar, content_area);
    } else {
        let groups = hint_groups_for(current, theme);
        let visible = truncate_groups_to_width(&groups, content_area.width as usize);

        let mut spans: Vec<Span> = vec![Span::raw(" ")];
        for (gi, group) in visible.iter().enumerate() {
            if gi > 0 {
                spans.push(Span::styled(" │ ", Style::default().fg(theme.border_dim)));
            }
            for hint in group {
                spans.push(Span::styled(hint.key, Style::default().fg(hint.color)));
                if !hint.label.is_empty() {
                    spans.push(Span::styled(
                        format!(" {} ", hint.label),
                        Style::default().fg(theme.text_muted),
                    ));
                }
            }
        }

        f.render_widget(Paragraph::new(Line::from(spans)), content_area);
    }

    if let Some(details_area) = file_task_details_rect(area, file_task) {
        buttons.record_button(TuiButton::FileTaskMessages, details_area);
        let attention = file_task.is_some_and(|state| state.attention);
        let style = Style::default()
            .fg(if attention { theme.destructive } else { theme.cyan })
            .add_modifier(Modifier::BOLD);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(details_label, style))),
            details_area,
        );
    }
}

fn file_task_details_rect(area: Rect, file_task: Option<FileTaskFooterState>) -> Option<Rect> {
    let width = file_task_details_label(area.width, file_task).chars().count() as u16;
    (width > 0).then(|| {
        Rect::new(
            area.x.saturating_add(area.width.saturating_sub(width)),
            area.y,
            width,
            1,
        )
    })
}

fn compact_progress_bar(ratio: Option<f64>) -> String {
    let Some(ratio) = ratio.filter(|ratio| ratio.is_finite()) else {
        return "···".to_string();
    };
    let filled = (ratio.clamp(0.0, 1.0) * 3.0).round() as usize;
    format!("{}{}", "█".repeat(filled), "░".repeat(3usize.saturating_sub(filled)))
}

fn progress_percent(ratio: Option<f64>) -> String {
    ratio
        .filter(|ratio| ratio.is_finite())
        .map(|ratio| format!("{:.0}%", ratio.clamp(0.0, 1.0) * 100.0))
        .unwrap_or_else(|| "--%".to_string())
}

fn file_task_details_label(width: u16, file_task: Option<FileTaskFooterState>) -> String {
    let Some(state) = file_task else {
        return String::new();
    };
    if width == 0 {
        return String::new();
    }
    if !state.live {
        return if width >= 18 {
            " details ".to_string()
        } else if width >= 7 {
            " msgs ".to_string()
        } else {
            "d".to_string()
        };
    }

    let percent = progress_percent(state.ratio);
    let queue_count = if state.queued > 999 {
        "999+".to_string()
    } else {
        state.queued.to_string()
    };
    let queue_suffix = (state.queued > 0)
        .then(|| format!(" +{queue_count}"))
        .unwrap_or_default();
    let compact_queue = (state.queued > 0)
        .then(|| format!("+{queue_count}"))
        .unwrap_or_default();
    let candidates = [
        format!(
            " [{}] {}{} ",
            compact_progress_bar(state.ratio),
            percent,
            queue_suffix
        ),
        format!(" {}{} ", percent, compact_queue),
        percent.clone(),
        if state.attention {
            "!".to_string()
        } else {
            "t".to_string()
        },
    ];
    candidates
        .into_iter()
        .find(|label| label.chars().count() <= width as usize)
        .unwrap_or_default()
}

/// A single keybinding hint shown in the context bar.
#[derive(Debug, Clone, Copy)]
struct Hint {
    key: &'static str,
    label: &'static str,
    color: ratatui::style::Color,
    /// 0 = essential (never dropped), 1 = important, 2 = optional (dropped first).
    priority: u8,
}

const fn h(
    key: &'static str,
    label: &'static str,
    color: ratatui::style::Color,
    priority: u8,
) -> Hint {
    Hint {
        key,
        label,
        color,
        priority,
    }
}

/// Hint groups per screen. Groups are separated by ` │ ` dividers when rendered.
fn hint_groups_for(current: AppScreen, theme: super::theme::Theme) -> Vec<Vec<Hint>> {
    // Minimal hints: 3-5 essentials per screen + universal `: command │ ? help`.
    // Full keybinding reference available via `?` help overlay.
    let screen_hints = match current {
        AppScreen::Convert => vec![
            h("tab", "pane", theme.blue, 0),
            h("←→", "select", theme.blue, 0),
            h(":max", "pane", theme.blue, 2),
            h(":commit", "enqueue", theme.green, 0),
        ],
        AppScreen::Browse => vec![
            h("↑↓", "navigate", theme.blue, 0),
            h("enter", "open", theme.green, 0),
            h("space", "select", theme.blue, 0),
            h("Ctrl+P", "paste", theme.cyan, 1),
        ],
        AppScreen::Queue => vec![
            h("↑↓", "navigate", theme.blue, 0),
            h("s", "start", theme.green, 0),
            h("space", "select", theme.blue, 0),
        ],
        AppScreen::Config => vec![
            h("tab", "focus", theme.blue, 0),
            h("a", "add", theme.green, 0),
            h("d", "delete", theme.destructive, 0),
        ],
        _ => vec![],
    };
    vec![
        screen_hints,
        vec![
            h(":", "command", theme.cyan, 0),
            h("?", "help", theme.amber, 0),
        ],
    ]
}

/// Width of one hint when rendered: key + (` ` + label + ` ` if label non-empty).
fn hint_width(h: &Hint) -> usize {
    let mut w = h.key.chars().count();
    if !h.label.is_empty() {
        w += 2 + h.label.chars().count();
    }
    w
}

/// Total rendered width of grouped hints: leading space + hints + group dividers.
fn total_groups_width(groups: &[Vec<Hint>]) -> usize {
    let leading = 1;
    let dividers = groups.len().saturating_sub(1) * 3; // " │ "
    let hints: usize = groups.iter().flatten().map(hint_width).sum();
    leading + hints + dividers
}

/// Drop the rightmost hint with the highest priority value (>= 1) so the bar shrinks
/// from the end first. Priority-0 hints are protected. Returns true if something was dropped.
fn drop_one_hint(groups: &mut Vec<Vec<Hint>>) -> bool {
    for pri in [2u8, 1u8] {
        for gi in (0..groups.len()).rev() {
            for hi in (0..groups[gi].len()).rev() {
                if groups[gi][hi].priority == pri {
                    groups[gi].remove(hi);
                    return true;
                }
            }
        }
    }
    false
}

/// Drop hints from the end (lowest priority first) until the rendered width fits.
/// Removes empty groups so dividers don't orphan.
fn truncate_groups_to_width(groups: &[Vec<Hint>], available: usize) -> Vec<Vec<Hint>> {
    let mut working: Vec<Vec<Hint>> = groups.to_vec();
    while total_groups_width(&working) > available {
        if !drop_one_hint(&mut working) {
            break;
        }
        working.retain(|g| !g.is_empty());
    }
    working
}

#[cfg(test)]
mod tests {
    use super::{file_task_details_label, file_task_details_rect};
    use crate::tui::app::FileTaskFooterState;
    use ratatui::layout::Rect;

    fn retained() -> Option<FileTaskFooterState> {
        Some(FileTaskFooterState {
            live: false,
            ratio: Some(1.0),
            queued: 0,
            attention: false,
        })
    }

    fn live(queued: usize, attention: bool) -> Option<FileTaskFooterState> {
        Some(FileTaskFooterState {
            live: true,
            ratio: Some(0.61),
            queued,
            attention,
        })
    }

    #[test]
    fn retained_details_keeps_a_mouse_target_at_every_nonzero_width() {
        assert_eq!(file_task_details_label(0, retained()), "");
        for width in 1..7 {
            assert_eq!(file_task_details_label(width, retained()), "d");
        }
        assert_eq!(file_task_details_label(7, retained()), " msgs ");
        assert_eq!(file_task_details_label(17, retained()), " msgs ");
        assert_eq!(file_task_details_label(18, retained()), " details ");
        assert_eq!(file_task_details_label(80, None), "");

        assert_eq!(
            file_task_details_rect(Rect::new(9, 4, 1, 1), retained()),
            Some(Rect::new(9, 4, 1, 1)),
        );
        assert_eq!(file_task_details_rect(Rect::new(9, 4, 0, 1), retained()), None);
    }

    #[test]
    fn live_transfer_label_degrades_without_losing_restore_hitbox() {
        assert_eq!(file_task_details_label(30, live(2, false)), " [██░] 61% +2 ");
        assert_eq!(file_task_details_label(10, live(2, false)), " 61%+2 ");
        assert_eq!(file_task_details_label(3, live(0, false)), "61%");
        assert_eq!(file_task_details_label(1, live(0, false)), "t");
        assert_eq!(file_task_details_label(1, live(0, true)), "!");
        let huge = file_task_details_label(8, live(usize::MAX, false));
        assert!(huge.chars().count() <= 8);
        assert_eq!(huge, "61%");
    }
}
