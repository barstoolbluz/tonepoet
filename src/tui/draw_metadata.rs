//! Metadata pane: title/artist/album fields or a scrollable file list in batch/track modes.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{MetadataState, SourceMode};
use super::theme;

/// Draw the metadata pane with purple border.
pub fn draw_metadata_pane(
    f: &mut Frame,
    area: Rect,
    metadata: &MetadataState,
    source_mode: &SourceMode,
    focused: bool,
    maximized: bool,
) {
    if area.height < 4 || area.width < 30 {
        return;
    }

    let border_color = if focused {
        theme::PURPLE
    } else {
        theme::TEXT_DIM
    };
    let w = area.width as usize;
    let top_line = metadata_title_line(border_color, w, maximized);
    let bot_line = Line::from(Span::styled(
        format!("└{}┘", "─".repeat(w.saturating_sub(2))),
        theme::border(border_color),
    ));

    let mut lines = vec![top_line];
    match source_mode {
        SourceMode::Batch { paths, cursor, .. } => {
            lines.extend(render_batch_file_list(
                border_color,
                w,
                paths,
                *cursor,
                metadata.file_scroll,
                area.height.saturating_sub(2) as usize,
                focused,
                metadata,
            ));
        }
        SourceMode::MultiTrack { tracks, cursor, .. } => {
            lines.extend(render_track_file_list(
                border_color,
                w,
                tracks,
                *cursor,
                metadata.file_scroll,
                area.height.saturating_sub(2) as usize,
                focused,
                metadata,
            ));
        }
        _ => {
            lines.extend(render_single_metadata(border_color, w, metadata));
        }
    }

    let target_len_before_bottom = area.height.saturating_sub(1) as usize;
    while lines.len() < target_len_before_bottom {
        lines.push(bordered_line(border_color, w, vec![]));
    }
    lines.push(bot_line);

    f.render_widget(Paragraph::new(lines), area);
}

/// Draw the collapsed metadata title bar.
pub fn draw_metadata_title_bar(f: &mut Frame, area: Rect, focused: bool) {
    if area.height < 1 || area.width < 12 {
        return;
    }
    let border_color = if focused {
        theme::PURPLE
    } else {
        theme::TEXT_DIM
    };
    f.render_widget(
        Paragraph::new(vec![metadata_title_line(border_color, area.width as usize, false)]),
        area,
    );
}

fn metadata_title_line<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    maximized: bool,
) -> Line<'a> {
    let title = " metadata ";
    let indicator = if maximized { "▾" } else { "▸" };
    let bar_style = Style::default().fg(Color::Black).bg(border_color);
    let left_spans = vec![
        Span::styled("┌", theme::border(border_color)),
        Span::styled(format!(" {indicator}{title}"), bar_style),
    ];
    let right_spans = vec![
        Span::styled("a", Style::default().fg(theme::TEXT_MUTED).bg(border_color)),
        Span::styled("dvanced ", bar_style),
        Span::styled("┐", theme::border(border_color)),
    ];
    let fixed_width = Line::from(left_spans.clone()).width()
        + Line::from(right_spans.clone()).width();
    let fill_count = width.saturating_sub(fixed_width);
    let mut spans = left_spans;
    spans.push(Span::styled(
        " ".repeat(fill_count),
        bar_style,
    ));
    spans.extend(right_spans);
    Line::from(spans)
}

fn render_single_metadata<'a>(
    border_color: ratatui::style::Color,
    w: usize,
    metadata: &'a MetadataState,
) -> Vec<Line<'a>> {
    let dash = Span::styled("—", Style::default().fg(theme::TEXT_DIM));
    let field_or_dash = |val: &'a Option<String>| -> Span<'a> {
        match val {
            Some(v) if !v.is_empty() => Span::styled(v.clone(), theme::text()),
            _ => dash.clone(),
        }
    };

    let title_row = bordered_line(
        border_color,
        w,
        vec![
            Span::styled("   title   ", theme::muted()),
            field_or_dash(&metadata.title),
        ],
    );

    let half_w = w.saturating_sub(8) / 2;
    let artist_val = field_or_dash(&metadata.artist);
    let album_val = field_or_dash(&metadata.album);
    let artist_width = 11 + artist_val.width();
    let gap = half_w.saturating_sub(artist_width);
    let row2 = bordered_line(
        border_color,
        w,
        vec![
            Span::styled("   artist  ", theme::muted()),
            artist_val,
            Span::raw(" ".repeat(gap)),
            Span::styled("album  ", theme::muted()),
            album_val,
        ],
    );

    let genre_val = field_or_dash(&metadata.genre);
    let year_val = field_or_dash(&metadata.year);
    let genre_width = 11 + genre_val.width();
    let gap2 = half_w.saturating_sub(genre_width);
    let row3 = bordered_line(
        border_color,
        w,
        vec![
            Span::styled("   genre   ", theme::muted()),
            genre_val,
            Span::raw(" ".repeat(gap2)),
            Span::styled("year   ", theme::muted()),
            year_val,
        ],
    );

    vec![title_row, row2, row3]
}

fn render_batch_file_list<'a>(
    border_color: ratatui::style::Color,
    w: usize,
    paths: &'a [std::path::PathBuf],
    cursor: usize,
    scroll: usize,
    visible: usize,
    focused: bool,
    metadata: &'a MetadataState,
) -> Vec<Line<'a>> {
    if paths.is_empty() || visible == 0 {
        return Vec::new();
    }
    let scroll = clamp_scroll(scroll, cursor, paths.len(), visible);
    paths
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .map(|(idx, path)| {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let label = format!("{:>3}. ", idx + 1);
            let summary = if idx == cursor { metadata_summary(metadata) } else { None };
            file_row(border_color, w, &label, &name, summary.as_deref(), idx == cursor, focused)
        })
        .collect()
}

fn render_track_file_list<'a>(
    border_color: ratatui::style::Color,
    w: usize,
    tracks: &'a [super::app::MultiTrackEntry],
    cursor: usize,
    scroll: usize,
    visible: usize,
    focused: bool,
    metadata: &'a MetadataState,
) -> Vec<Line<'a>> {
    if tracks.is_empty() || visible == 0 {
        return Vec::new();
    }
    let scroll = clamp_scroll(scroll, cursor, tracks.len(), visible);
    tracks
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .map(|(idx, track)| {
            let name = track
                .title
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("track {}", track.number));
            let label = format!("{:>3}. ", track.number);
            let summary = if idx == cursor { metadata_summary(metadata) } else { None };
            file_row(border_color, w, &label, &name, summary.as_deref(), idx == cursor, focused)
        })
        .collect()
}

fn clamp_scroll(scroll: usize, cursor: usize, len: usize, visible: usize) -> usize {
    if len <= visible {
        return 0;
    }
    let max_scroll = len.saturating_sub(visible);
    let mut scroll = scroll.min(max_scroll);
    if cursor < scroll {
        scroll = cursor;
    } else if cursor >= scroll + visible {
        scroll = cursor + 1 - visible;
    }
    scroll.min(max_scroll)
}

fn file_row<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    label: &str,
    name: &str,
    summary: Option<&str>,
    selected: bool,
    focused: bool,
) -> Line<'a> {
    let style = if selected && focused {
        Style::default()
            .fg(theme::PILL_ACTIVE_FG)
            .bg(theme::PURPLE)
            .add_modifier(Modifier::BOLD)
    } else if selected {
        Style::default().fg(theme::PURPLE).add_modifier(Modifier::BOLD)
    } else {
        theme::text()
    };
    let label_style = if selected { style } else { theme::muted() };
    let summary_style = if selected { style } else { theme::muted() };

    let content_width = width.saturating_sub(2);
    let leading_width = 2usize;
    let label_width = text_width(label);
    let summary_text = summary.unwrap_or("");
    let summary_width = text_width(summary_text);
    let summary_reserve = if summary_text.is_empty() { 0 } else { summary_width + 2 };
    let max_name = content_width.saturating_sub(leading_width + label_width + summary_reserve);
    let display = truncate_to(name, max_name);
    let display_width = text_width(&display);

    let mut spans = vec![
        Span::raw("  "),
        Span::styled(label.to_string(), label_style),
        Span::styled(display, style),
    ];

    if !summary_text.is_empty() {
        let used_left = leading_width + label_width + display_width;
        let gap = content_width.saturating_sub(used_left + summary_width);
        spans.push(Span::raw(" ".repeat(gap.max(1))));
        spans.push(Span::styled(summary_text.to_string(), summary_style));
    }

    bordered_line(border_color, width, spans)
}

fn metadata_summary(metadata: &MetadataState) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    if let Some(artist) = metadata.artist.as_deref().filter(|s| !s.is_empty()) {
        parts.push(artist);
    }
    if let Some(album) = metadata.album.as_deref().filter(|s| !s.is_empty()) {
        parts.push(album);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

/// Create a line with │ content ... │ border.
fn bordered_line<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    content: Vec<Span<'a>>,
) -> Line<'a> {
    let content_width: usize = content.iter().map(|s| s.width()).sum();
    let padding = width.saturating_sub(2 + content_width);

    let mut spans = vec![Span::styled("│", theme::border(border_color))];
    spans.extend(content);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", theme::border(border_color)));
    Line::from(spans)
}

fn text_width(s: &str) -> usize {
    Line::from(s).width()
}

fn truncate_to(s: &str, max_width: usize) -> String {
    if text_width(s) <= max_width {
        return s.to_string();
    }
    if max_width == 0 {
        return String::new();
    }

    let ellipsis = "…";
    let ellipsis_width = text_width(ellipsis);
    if max_width <= ellipsis_width {
        let mut out = String::new();
        for ch in s.chars() {
            let candidate = format!("{}{}", out, ch);
            if text_width(&candidate) > max_width {
                break;
            }
            out = candidate;
        }
        return out;
    }

    let mut out = String::new();
    for ch in s.chars() {
        let candidate = format!("{}{}", out, ch);
        if text_width(&candidate) + ellipsis_width > max_width {
            break;
        }
        out = candidate;
    }
    out.push_str(ellipsis);
    out
}
