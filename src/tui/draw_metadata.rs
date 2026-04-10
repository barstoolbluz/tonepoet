//! Metadata pane: title, artist, album, genre, year (purple border)

use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::MetadataState;
use super::theme;

/// Draw the metadata pane with purple border
pub fn draw_metadata_pane(f: &mut Frame, area: Rect, metadata: &MetadataState, focused: bool) {
    if area.height < 4 || area.width < 30 {
        return;
    }

    let border_color = if focused { theme::PURPLE } else { theme::TEXT_DIM };
    let w = area.width as usize;

    // Top border
    let title = " metadata ";
    let adv_label = " advanced ";
    let dash_count = w.saturating_sub(2 + title.len() + adv_label.len() + 2);

    let top_line = Line::from(vec![
        Span::styled("┌", theme::border(border_color)),
        Span::styled(title, theme::border(border_color)),
        Span::styled("─".repeat(dash_count), theme::border(border_color)),
        Span::raw(" "),
        Span::styled("a", theme::muted()),
        Span::styled("dvanced", theme::border(border_color)),
        Span::styled(" ┐", theme::border(border_color)),
    ]);

    let bot_line = Line::from(Span::styled(
        format!("└{}┘", "─".repeat(w.saturating_sub(2))),
        theme::border(border_color),
    ));

    let dash = Span::styled("—", Style::default().fg(theme::TEXT_DIM));
    let field_or_dash = |val: &Option<String>| -> Span {
        match val {
            Some(v) if !v.is_empty() => Span::styled(v.clone(), theme::text()),
            _ => dash.clone(),
        }
    };

    // Row 1: title
    let title_row = bordered_line(border_color, w, vec![
        Span::styled("   title   ", theme::muted()),
        field_or_dash(&metadata.title),
    ]);

    // Row 2: artist + album (side by side)
    let half_w = w.saturating_sub(8) / 2;
    let artist_val = field_or_dash(&metadata.artist);
    let album_val = field_or_dash(&metadata.album);

    let artist_width = 11 + artist_val.width(); // "   artist  " + value
    let gap = half_w.saturating_sub(artist_width);

    let row2 = bordered_line(border_color, w, vec![
        Span::styled("   artist  ", theme::muted()),
        artist_val,
        Span::raw(" ".repeat(gap)),
        Span::styled("album  ", theme::muted()),
        album_val,
    ]);

    // Row 3: genre + year (side by side)
    let genre_val = field_or_dash(&metadata.genre);
    let year_val = field_or_dash(&metadata.year);

    let genre_width = 11 + genre_val.width();
    let gap2 = half_w.saturating_sub(genre_width);

    let row3 = bordered_line(border_color, w, vec![
        Span::styled("   genre   ", theme::muted()),
        genre_val,
        Span::raw(" ".repeat(gap2)),
        Span::styled("year   ", theme::muted()),
        year_val,
    ]);

    let lines = vec![top_line, title_row, row2, row3, bot_line];

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, area);
}

/// Create a line with │ content ... │ border
fn bordered_line<'a>(border_color: ratatui::style::Color, width: usize, content: Vec<Span<'a>>) -> Line<'a> {
    let content_width: usize = content.iter().map(|s| s.width()).sum();
    let padding = width.saturating_sub(2 + content_width);

    let mut spans = vec![Span::styled("│", theme::border(border_color))];
    spans.extend(content);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", theme::border(border_color)));
    Line::from(spans)
}

