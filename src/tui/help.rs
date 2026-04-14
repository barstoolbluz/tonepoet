//! Help overlay: per-screen keybinding reference + command list.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::app::AppScreen;
use super::theme;

/// A section in the help content (e.g., "Navigation", "Commands").
pub struct HelpSection {
    title: &'static str,
    entries: Vec<(&'static str, &'static str)>, // (key, description)
}

/// Public accessor for keybindings handler scroll clamping.
pub fn help_content_for(screen: AppScreen) -> Vec<HelpSection> {
    help_content(screen)
}

/// Count the total rendered lines for a given set of sections.
pub fn line_count(sections: &[HelpSection]) -> usize {
    let mut count = 0;
    for (i, section) in sections.iter().enumerate() {
        if i > 0 { count += 1; } // Blank separator.
        count += 1; // Title line.
        count += section.entries.len();
    }
    count
}

/// Build the help content for a given screen.
fn help_content(screen: AppScreen) -> Vec<HelpSection> {
    let mut sections = match screen {
        AppScreen::Browse => vec![
            HelpSection {
                title: "Navigation",
                entries: vec![
                    ("Up/k, Down/j", "Navigate entries"),
                    ("Left/h, Backspace", "Go to parent directory"),
                    ("Right/l", "Enter directory"),
                    ("Enter", "Open file / enter dir / browse archive"),
                    ("Home/g, End/G", "Jump to top / bottom"),
                    ("PageUp, PageDown", "Page scroll"),
                ],
            },
            HelpSection {
                title: "Selection & Filtering",
                entries: vec![
                    ("Space", "Toggle multi-select"),
                    (".", "Toggle hidden files"),
                    ("/", "Open text filter"),
                    ("Esc", "Clear selection / filter / exit archive"),
                ],
            },
            HelpSection {
                title: "File Operations",
                entries: vec![
                    ("R", "Bulk rename wizard"),
                    ("m", "Context menu (right-click)"),
                    (":rename <name>", "Rename selected entry"),
                    (":cp <dest>", "Copy selected to destination"),
                    (":mv <dest>", "Move selected to destination"),
                    (":del", "Move selected to trash"),
                    (":pw", "Set archive password"),
                ],
            },
            HelpSection {
                title: "Browse Commands",
                entries: vec![
                    (":cd <path>", "Change directory"),
                    (":sort [field]", "Sort by name/date/size/type"),
                    (":filter [format]", "Filter by audio format"),
                    (":queue [preset]", "Queue selection for conversion"),
                    (":recent", "Open recent files"),
                    (":bm", "Open bookmarks"),
                    (":bm add [name]", "Bookmark current directory"),
                    (":rename-all", "Bulk rename (same as R)"),
                ],
            },
        ],
        AppScreen::Convert => vec![
            HelpSection {
                title: "Pane Navigation",
                entries: vec![
                    ("Tab / Shift+Tab", "Cycle panes (Source/Metadata/Format/Output)"),
                    ("Up/k, Down/j", "Navigate within pane"),
                    ("Left/h, Right/l", "Select option (pills) / toggle"),
                    ("e / Enter", "Edit field or expand batch"),
                    ("a", "Toggle advanced options"),
                ],
            },
            HelpSection {
                title: "Presets",
                entries: vec![
                    ("p", "Open presets overlay"),
                    ("s", "Save current settings as preset"),
                    (":preset <name>", "Load preset by name"),
                    (":saveas <name>", "Save as named preset"),
                ],
            },
            HelpSection {
                title: "Review & Commit",
                entries: vec![
                    (":commit", "Enqueue batch (don't start)"),
                    (":Commit", "Enqueue batch + start conversion"),
                    ("Esc", "Cancel batch, return to previous screen"),
                    (":browse", "Switch to Browse to pick files"),
                    (":expand / :x", "Expand batch file list"),
                ],
            },
        ],
        AppScreen::Queue => vec![
            HelpSection {
                title: "Navigation",
                entries: vec![
                    ("Up/k, Down/j", "Navigate queue items"),
                    ("Home/g, End/G", "Jump to top / bottom"),
                    ("PageUp, PageDown", "Page scroll"),
                    ("Tab", "Switch focus: file list / action bar"),
                    ("Space", "Toggle selection"),
                    ("Ctrl+a", "Select all"),
                    ("Enter", "Show item info / error detail"),
                ],
            },
            HelpSection {
                title: "Queue Actions",
                entries: vec![
                    ("s", "Start conversion"),
                    ("p", "Pause / resume"),
                    ("x", "Stop all (with confirmation)"),
                    ("a / f", "Add files"),
                    ("c", "Configure selected items"),
                    ("d / Delete", "Remove selected (with confirmation)"),
                    ("Ctrl+l", "Clear completed items"),
                    ("Ctrl+r", "Retry failed items"),
                ],
            },
        ],
        AppScreen::Config => vec![
            HelpSection {
                title: "Config Screen",
                entries: vec![
                    ("Tab", "Switch focus: Settings / Keychain"),
                ],
            },
            HelpSection {
                title: "Password Keychain",
                entries: vec![
                    ("Up/k, Down/j", "Navigate passwords"),
                    ("a", "Add new password"),
                    ("d", "Delete selected password"),
                    ("v", "Toggle password visibility"),
                ],
            },
        ],
        _ => vec![],
    };

    // Global section — always shown at the end.
    sections.push(HelpSection {
        title: "Global",
        entries: vec![
            ("1-5", "Switch screen (Browse/Library/Convert/Queue/Config)"),
            (":", "Enter command mode"),
            (":q", "Quit tonepoet"),
            (":go / :start", "Start processing queued items"),
            ("?", "Toggle this help overlay"),
            ("m", "Context menu"),
        ],
    });

    sections
}

/// Flatten help sections into renderable lines (title lines + entry lines).
fn flatten_to_lines(sections: &[HelpSection]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    for (i, section) in sections.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from("")); // Blank separator.
        }
        lines.push(Line::from(Span::styled(
            format!("  {}", section.title),
            Style::default().fg(theme::AMBER).add_modifier(Modifier::BOLD),
        )));

        for (key, desc) in &section.entries {
            let key_w = 22;
            let padded_key = format!("    {:<width$}", key, width = key_w);
            lines.push(Line::from(vec![
                Span::styled(padded_key, Style::default().fg(theme::BLUE)),
                Span::styled(desc.to_string(), Style::default().fg(theme::TEXT)),
            ]));
        }
    }

    lines
}

/// Draw the help overlay.
pub fn draw_help(f: &mut Frame, screen: AppScreen, scroll: usize) {
    let area = f.size();
    let w = (area.width * 80 / 100).max(50).min(area.width.saturating_sub(2));
    let h = (area.height * 85 / 100).max(15).min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let popup = Rect::new(x, y, w, h);

    f.render_widget(Clear, popup);

    let title = format!(" Help -- {} ", screen.tab_label());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::AMBER))
        .title(Span::styled(
            title,
            Style::default().fg(theme::AMBER).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if inner.height < 3 {
        return;
    }

    // Layout: content + footer.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let sections = help_content(screen);
    let all_lines = flatten_to_lines(&sections);
    let total = all_lines.len();
    let visible = chunks[0].height as usize;

    // Clamp scroll.
    let scroll = scroll.min(total.saturating_sub(visible));

    let visible_lines: Vec<Line> = all_lines
        .into_iter()
        .skip(scroll)
        .take(visible)
        .collect();

    f.render_widget(Paragraph::new(visible_lines), chunks[0]);

    // Footer.
    let footer = Line::from(vec![
        Span::styled("↑↓", Style::default().fg(theme::BLUE)),
        Span::styled(" scroll  ", Style::default().fg(theme::TEXT_MUTED)),
        Span::styled("Esc", Style::default().fg(theme::AMBER)),
        Span::styled(" close", Style::default().fg(theme::TEXT_MUTED)),
    ]);
    f.render_widget(
        Paragraph::new(footer).alignment(Alignment::Center),
        chunks[1],
    );
}
