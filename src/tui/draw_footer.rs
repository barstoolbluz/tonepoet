//! Footer: 5-tab view bar + context keybinding bar

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::AppScreen;
use super::button_map::{ButtonRenderMap, TuiButton};
use super::theme;

/// Draw both footer rows (tabs + context bar) into a 2-line area.
/// Also registers clickable regions for the tab bar into `buttons`.
/// When `status_message` is Some, the context bar shows the message instead of hints.
pub fn draw_footer(
    f: &mut Frame,
    area: Rect,
    current_screen: AppScreen,
    buttons: &mut ButtonRenderMap,
    status_message: Option<&str>,
) {
    if area.height < 2 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    draw_tab_bar(f, chunks[0], current_screen, buttons);
    draw_context_bar(f, chunks[1], current_screen, status_message);
}

/// Draw the numbered tab bar: 1 convert | 2 browse | 3 library | 4 queue | 5 config
fn draw_tab_bar(f: &mut Frame, area: Rect, current: AppScreen, buttons: &mut ButtonRenderMap) {
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
            spans.push(Span::styled("│", Style::default().fg(theme::BORDER_DIM)));
        }

        // Key badge
        let key_style = if is_active {
            Style::default()
                .fg(theme::BG)
                .bg(theme::BLUE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(theme::TEXT_MUTED)
                .bg(theme::BORDER_DIM)
        };

        let label_style = if is_active {
            Style::default().fg(theme::TEXT_BRIGHT)
        } else {
            Style::default().fg(theme::TEXT_MUTED)
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

    let bar = Paragraph::new(Line::from(spans))
        .style(Style::default().bg(theme::SURFACE));
    f.render_widget(bar, area);
}

/// Draw the context-sensitive keybinding bar.
/// If `status_message` is Some, render the message in amber instead of keybinding hints.
fn draw_context_bar(f: &mut Frame, area: Rect, current: AppScreen, status_message: Option<&str>) {
    // When a transient status message is set, it replaces the hints on this row.
    if let Some(msg) = status_message {
        let max_chars = (area.width as usize).saturating_sub(2);
        let display: String = if msg.chars().count() > max_chars && max_chars >= 2 {
            let t: String = msg.chars().take(max_chars - 1).collect();
            format!(" {}…", t)
        } else {
            format!(" {}", msg)
        };
        let bar = Paragraph::new(Line::from(Span::styled(
            display,
            Style::default().fg(theme::AMBER).add_modifier(Modifier::BOLD),
        )));
        f.render_widget(bar, area);
        return;
    }

    let groups = hint_groups_for(current);
    let visible = truncate_groups_to_width(&groups, area.width as usize);

    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for (gi, group) in visible.iter().enumerate() {
        if gi > 0 {
            spans.push(Span::styled(" │ ", Style::default().fg(theme::BORDER_DIM)));
        }
        for hint in group {
            spans.push(Span::styled(hint.key, Style::default().fg(hint.color)));
            if !hint.label.is_empty() {
                spans.push(Span::styled(
                    format!(" {} ", hint.label),
                    Style::default().fg(theme::TEXT_MUTED),
                ));
            }
        }
    }

    let bar = Paragraph::new(Line::from(spans));
    f.render_widget(bar, area);
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

const fn h(key: &'static str, label: &'static str, color: ratatui::style::Color, priority: u8) -> Hint {
    Hint { key, label, color, priority }
}

/// Hint groups per screen. Groups are separated by ` │ ` dividers when rendered.
fn hint_groups_for(current: AppScreen) -> Vec<Vec<Hint>> {
    match current {
        AppScreen::Convert => vec![
            vec![
                h("↑↓", "navigate", theme::BLUE, 0),
                h("tab", "pane", theme::BLUE, 0),
                h("←→", "select", theme::BLUE, 1),
                h("e", "edit", theme::BLUE, 1),
                h("a", "advanced", theme::PURPLE, 2),
            ],
            vec![
                h(":browse", "", theme::CYAN, 2),
                h(":recent", "", theme::CYAN, 2),
                h("p", "presets", theme::CYAN, 1),
                h("s", "save", theme::CYAN, 1),
                h("f", "effects", theme::AMBER, 2),
            ],
            vec![
                h(":commit", "", theme::GREEN, 0),
                h(":Commit", "+start", theme::GREEN, 1),
                h("esc", "cancel", theme::AMBER, 2),
            ],
            vec![h(":q", "quit", theme::RED, 0)],
        ],
        AppScreen::Browse => vec![
            vec![
                h("↑↓", "navigate", theme::BLUE, 0),
                h("←→", "up/enter", theme::BLUE, 1),
                h("space", "select", theme::BLUE, 1),
                h("enter", "load", theme::GREEN, 0),
            ],
            vec![
                h("/", "filter", theme::BLUE, 1),
                h(":sort", "", theme::CYAN, 2),
                h(":filter", "", theme::CYAN, 2),
                h(":cd", "", theme::CYAN, 2),
                h(":rename", "", theme::AMBER, 2),
                h(":recent", "", theme::CYAN, 2),
                h(":bm", "", theme::GREEN, 2),
                h(".", "hidden", theme::PURPLE, 2),
            ],
            vec![
                h(":queue", "", theme::AMBER, 1),
                h(":convert", "", theme::AMBER, 2),
            ],
            vec![
                h("⇧click", "toggle", theme::PURPLE, 2),
                h("⌥click", "range", theme::PURPLE, 2),
            ],
            vec![h(":q", "quit", theme::RED, 0)],
        ],
        AppScreen::Queue => vec![
            vec![
                h("↑↓", "navigate", theme::BLUE, 0),
                h("space", "select", theme::BLUE, 1),
                h("a", "add files", theme::BLUE, 1),
                h("c", "configure", theme::PURPLE, 1),
            ],
            vec![
                h("s", "start", theme::GREEN, 0),
                h("p", "pause", theme::AMBER, 1),
            ],
            vec![h(":q", "quit", theme::RED, 0)],
        ],
        _ => vec![
            vec![
                h(":q", "quit", theme::RED, 0),
                h("1", "browse", theme::BLUE, 1),
            ],
        ],
    }
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
