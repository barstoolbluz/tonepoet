//! ASCII art TONEPOET header with box border

use ratatui::{
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};


const ART_LINE_1: &str = "▀▀█▀▀ █▀▀█ █▀▀▄ █▀▀▀ █▀▀█ █▀▀█ █▀▀▀ ▀▀█▀▀";
const ART_LINE_2: &str = "  █   █  █ █  █ █▀▀▀ █▀▀▀ █  █ █▀▀▀   █  ";
const ART_LINE_3: &str = "  ▀   ▀▀▀▀ ▀  ▀ ▀▀▀▀ ▀    ▀▀▀▀ ▀▀▀▀   ▀  ";

/// Draw the header: blue box with ASCII art TONEPOET + version
pub fn draw_header(f: &mut Frame, area: Rect, theme: super::theme::Theme) {
    if area.height < 7 || area.width < 50 {
        return;
    }

    let w = area.width as usize;
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));

    let border = theme.border(theme.blue);
    let art_style = ratatui::style::Style::default()
        .fg(theme.title)
        .add_modifier(Modifier::BOLD);
    let ver_style = theme.muted();

    let top = format!("╭{}╮", "─".repeat(w.saturating_sub(2)));
    let bot = format!("╰{}╯", "─".repeat(w.saturating_sub(2)));
    let blank = format!("│{}│", " ".repeat(w.saturating_sub(2)));

    let art_width = ART_LINE_1.chars().count();
    let pad_left = (w.saturating_sub(2).saturating_sub(art_width)) / 2;
    let pad_l = " ".repeat(pad_left);
    let pad_r = " ".repeat(w.saturating_sub(2).saturating_sub(pad_left + art_width));

    let art3_with_ver = format!("{}{}    {}", pad_l, ART_LINE_3, version);
    let art3_pad = " ".repeat(
        w.saturating_sub(2)
            .saturating_sub(art3_with_ver.chars().count()),
    );

    let lines = vec![
        Line::from(Span::styled(top, border)),
        Line::from(Span::styled(blank.clone(), border)),
        // Art line 1
        Line::from(vec![
            Span::styled("│", border),
            Span::raw(pad_l.clone()),
            Span::styled(ART_LINE_1, art_style),
            Span::raw(pad_r.clone()),
            Span::styled("│", border),
        ]),
        // Art line 2
        Line::from(vec![
            Span::styled("│", border),
            Span::raw(pad_l.clone()),
            Span::styled(ART_LINE_2, art_style),
            Span::raw(pad_r.clone()),
            Span::styled("│", border),
        ]),
        // Art line 3 + version
        Line::from(vec![
            Span::styled("│", border),
            Span::raw(pad_l),
            Span::styled(ART_LINE_3, art_style),
            Span::raw("    "),
            Span::styled(version, ver_style),
            Span::raw(art3_pad),
            Span::styled("│", border),
        ]),
        Line::from(Span::styled(blank, border)),
        Line::from(Span::styled(bot, border)),
    ];

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn draw_header_uses_passed_theme_without_frame_binding() {
        let theme = crate::tui::theme::theme_by_slug("catppuccin-latte").expect("theme");
        let backend = TestBackend::new(80, 7);
        let mut terminal = Terminal::new(backend).expect("terminal");

        terminal
            .draw(|frame| draw_header(frame, ratatui::layout::Rect::new(0, 0, 80, 7), theme))
            .expect("draw");

        let buffer = terminal.backend().buffer();
        // Art is centered: pad_left = (80 - 2 - 42) / 2 = 18, art starts at col 19.
        let art_cell = buffer.get(19, 2);
        assert_eq!(art_cell.fg, theme.title);
    }
}
