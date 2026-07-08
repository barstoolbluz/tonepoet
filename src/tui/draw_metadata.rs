//! Metadata pane: title/artist/album fields or a scrollable file list in batch/track modes.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::app::{ConvertMetadataField, MetadataState, SourceMode};
use super::inline_edit::{inline_cursor_col, render_inline_value};

/// Draw the metadata pane with purple border.
pub fn draw_metadata_pane(
    f: &mut Frame,
    area: Rect,
    metadata: &MetadataState,
    source_mode: &SourceMode,
    focused: bool,
    maximized: bool,
    theme: super::theme::Theme,
) {
    if area.height < 4 || area.width < 30 {
        return;
    }

    let border_color = if focused {
        theme.purple
    } else {
        theme.text_dim
    };
    let w = area.width as usize;
    let top_line = metadata_title_line(border_color, w, maximized, theme);
    let bot_line = Line::from(Span::styled(
        format!("└{}┘", "─".repeat(w.saturating_sub(2))),
        theme.border(border_color),
    ));

    let list_visible_rows = area.height.saturating_sub(2) as usize;

    let mut lines = vec![top_line];
    match source_mode {
        SourceMode::Batch { paths, cursor, .. } => {
            lines.push(render_album_artist_override_row(border_color, w, metadata, focused, theme));
            lines.extend(render_batch_file_list(
                border_color,
                w,
                paths,
                *cursor,
                metadata.file_scroll,
                list_visible_rows.saturating_sub(1),
                focused,
                metadata,
                theme,
            ));
        }
        SourceMode::MultiTrack {
            archive_preview: Some(preview),
            cursor,
            ..
        } => {
            lines.extend(render_archive_preview_metadata(
                border_color,
                w,
                preview,
                *cursor,
                metadata,
                focused,
                theme,
            ));
        }
        SourceMode::MultiTrack { tracks, cursor, .. } => {
            lines.push(render_album_artist_override_row(border_color, w, metadata, focused, theme));
            lines.extend(render_track_file_list(
                border_color,
                w,
                tracks,
                *cursor,
                metadata.file_scroll,
                list_visible_rows.saturating_sub(1),
                focused,
                metadata,
                theme,
            ));
        }
        _ => {
            lines.extend(render_single_metadata(border_color, w, metadata, focused, theme));
        }
    }

    let target_len_before_bottom = area.height.saturating_sub(1) as usize;
    while lines.len() < target_len_before_bottom {
        lines.push(bordered_line(border_color, w, vec![], theme));
    }
    lines.push(bot_line);

    f.render_widget(Paragraph::new(lines), area);

    if focused {
        if let Some((row, label_w, value_w)) = metadata_edit_cursor(metadata, source_mode, w) {
            let col = inline_cursor_col(&metadata.edit_input, value_w);
            let cursor_x = area.x + 1 + label_w as u16 + col;
            let cursor_y = area.y + row as u16;
            if cursor_y < area.y + area.height && cursor_x < area.x + area.width.saturating_sub(1) {
                f.set_cursor(cursor_x, cursor_y);
            }
        }
    }
}


fn metadata_edit_cursor(
    metadata: &MetadataState,
    source_mode: &SourceMode,
    pane_width: usize,
) -> Option<(usize, usize, usize)> {
    let field = metadata.editing?;
    let list_mode = matches!(
        source_mode,
        SourceMode::Batch { .. }
            | SourceMode::MultiTrack {
                archive_preview: None,
                ..
            }
    );
    if list_mode {
        if field != ConvertMetadataField::AlbumArtist {
            return None;
        }
        let label_w = "   album artist ".len();
        return Some((1, label_w, pane_width.saturating_sub(2 + label_w)));
    }
    if !matches!(
        source_mode,
        SourceMode::Empty
            | SourceMode::Single { .. }
            | SourceMode::MultiTrack {
                archive_preview: Some(_),
                ..
            }
    ) {
        return None;
    }
    let content_w = pane_width.saturating_sub(2);
    let half_w = content_w / 2;
    match field {
        ConvertMetadataField::Title => {
            let label_w = "   title   ".len();
            Some((1, label_w, pane_width.saturating_sub(2 + label_w)))
        }
        ConvertMetadataField::Artist => {
            let label_w = "   artist  ".len();
            Some((2, label_w, half_w.saturating_sub(label_w)))
        }
        ConvertMetadataField::Album => {
            let label_w = half_w + "album  ".len();
            Some((2, label_w, content_w.saturating_sub(label_w).max(1)))
        }
        ConvertMetadataField::AlbumArtist => {
            let label_w = "   album artist ".len();
            Some((4, label_w, pane_width.saturating_sub(2 + label_w)))
        }
        ConvertMetadataField::Genre => {
            let label_w = "   genre   ".len();
            Some((3, label_w, half_w.saturating_sub(label_w)))
        }
        ConvertMetadataField::Year => {
            let label_w = half_w + "year   ".len();
            Some((3, label_w, content_w.saturating_sub(label_w).max(1)))
        }
    }
}

/// Draw the collapsed metadata title bar.
pub fn draw_metadata_title_bar(f: &mut Frame, area: Rect, focused: bool, theme: super::theme::Theme) {
    if area.height < 1 || area.width < 12 {
        return;
    }
    let border_color = if focused {
        theme.purple
    } else {
        theme.text_dim
    };
    f.render_widget(
        Paragraph::new(vec![metadata_title_line(border_color, area.width as usize, false, theme)]),
        area,
    );
}

fn metadata_title_line<'a>(
    border_color: ratatui::style::Color,
    width: usize,
    maximized: bool,
    theme: super::theme::Theme,
) -> Line<'a> {
    let title = " metadata ";
    let indicator = if maximized { "▾" } else { "▸" };
    let bar_style = Style::default().fg(theme.bg).bg(border_color);
    let left_spans = vec![
        Span::styled("┌", theme.border(border_color)),
        Span::styled(format!(" {indicator}{title}"), bar_style),
    ];
    let right_spans = vec![
        Span::styled("a", Style::default().fg(theme.text_muted).bg(border_color)),
        Span::styled("dvanced ", bar_style),
        Span::styled("┐", theme.border(border_color)),
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

fn render_album_artist_override_row<'a>(
    border_color: ratatui::style::Color,
    w: usize,
    metadata: &'a MetadataState,
    pane_focused: bool,
    theme: super::theme::Theme,
) -> Line<'a> {
    let label = "   album artist ";
    let value_w = w.saturating_sub(2 + label.len());
    let focused = pane_focused && metadata.field_focus == ConvertMetadataField::AlbumArtist;
    bordered_line(
        border_color,
        w,
        vec![
            Span::styled(label, if focused { theme.bright() } else { theme.muted() }),
            render_inline_value(
                metadata.album_artist_for_conversion.as_deref().unwrap_or(""),
                metadata.editing == Some(ConvertMetadataField::AlbumArtist),
                &metadata.edit_input,
                focused,
                value_w,
                theme,
            ),
        ],
        theme,
    )
}

fn render_single_metadata<'a>(
    border_color: ratatui::style::Color,
    w: usize,
    metadata: &'a MetadataState,
    pane_focused: bool,
    theme: super::theme::Theme,
) -> Vec<Line<'a>> {
    let is_focused = |field| pane_focused && metadata.field_focus == field;
    let is_editing = |field| metadata.editing == Some(field);
    let value = |val: &'a Option<String>| -> &'a str { val.as_deref().unwrap_or("") };

    let title_label = "   title   ";
    let title_value_w = w.saturating_sub(2 + title_label.len());
    let title_row = bordered_line(
        border_color,
        w,
        vec![
            Span::styled(
                title_label,
                if is_focused(ConvertMetadataField::Title) { theme.bright() } else { theme.muted() },
            ),
            render_inline_value(
                value(&metadata.title),
                is_editing(ConvertMetadataField::Title),
                &metadata.edit_input,
                is_focused(ConvertMetadataField::Title),
                title_value_w,
                theme,
            ),
        ],
        theme,
    );

    let content_w = w.saturating_sub(2);
    let half_w = content_w / 2;
    let artist_label = "   artist  ";
    let album_label = "album  ";
    let artist_value_w = half_w.saturating_sub(artist_label.len());
    let album_value_w = content_w
        .saturating_sub(half_w + album_label.len())
        .max(1);
    let row2 = bordered_line(
        border_color,
        w,
        vec![
            Span::styled(
                artist_label,
                if is_focused(ConvertMetadataField::Artist) { theme.bright() } else { theme.muted() },
            ),
            render_inline_value(
                value(&metadata.artist),
                is_editing(ConvertMetadataField::Artist),
                &metadata.edit_input,
                is_focused(ConvertMetadataField::Artist),
                artist_value_w,
                theme,
            ),
            Span::raw(" ".repeat(content_w.saturating_sub(half_w + artist_label.len() + artist_value_w))),
            Span::styled(
                album_label,
                if is_focused(ConvertMetadataField::Album) { theme.bright() } else { theme.muted() },
            ),
            render_inline_value(
                value(&metadata.album),
                is_editing(ConvertMetadataField::Album),
                &metadata.edit_input,
                is_focused(ConvertMetadataField::Album),
                album_value_w,
                theme,
            ),
        ],
        theme,
    );

    let genre_label = "   genre   ";
    let year_label = "year   ";
    let genre_value_w = half_w.saturating_sub(genre_label.len());
    let year_value_w = content_w
        .saturating_sub(half_w + year_label.len())
        .max(1);
    let row3 = bordered_line(
        border_color,
        w,
        vec![
            Span::styled(
                genre_label,
                if is_focused(ConvertMetadataField::Genre) { theme.bright() } else { theme.muted() },
            ),
            render_inline_value(
                value(&metadata.genre),
                is_editing(ConvertMetadataField::Genre),
                &metadata.edit_input,
                is_focused(ConvertMetadataField::Genre),
                genre_value_w,
                theme,
            ),
            Span::raw(" ".repeat(content_w.saturating_sub(half_w + genre_label.len() + genre_value_w))),
            Span::styled(
                year_label,
                if is_focused(ConvertMetadataField::Year) { theme.bright() } else { theme.muted() },
            ),
            render_inline_value(
                value(&metadata.year),
                is_editing(ConvertMetadataField::Year),
                &metadata.edit_input,
                is_focused(ConvertMetadataField::Year),
                year_value_w,
                theme,
            ),
        ],
        theme,
    );

    let album_artist_label = "   album artist ";
    let album_artist_value_w = w.saturating_sub(2 + album_artist_label.len());
    let row4 = bordered_line(
        border_color,
        w,
        vec![
            Span::styled(
                album_artist_label,
                if is_focused(ConvertMetadataField::AlbumArtist) { theme.bright() } else { theme.muted() },
            ),
            render_inline_value(
                value(&metadata.album_artist_for_conversion),
                is_editing(ConvertMetadataField::AlbumArtist),
                &metadata.edit_input,
                is_focused(ConvertMetadataField::AlbumArtist),
                album_artist_value_w,
                theme,
            ),
        ],
        theme,
    );

    vec![title_row, row2, row3, row4]
}

fn render_archive_preview_metadata<'a>(
    border_color: ratatui::style::Color,
    w: usize,
    preview: &'a super::app::ArchivePreview,
    cursor: usize,
    metadata: &'a MetadataState,
    pane_focused: bool,
    theme: super::theme::Theme,
) -> Vec<Line<'a>> {
    let mut rows = render_single_metadata(border_color, w, metadata, pane_focused, theme);
    if let Some(track) = preview.tracks.get(cursor) {
        let label = format!("   track   {:>2}/{} ", cursor + 1, preview.tracks.len());
        let value_width = w.saturating_sub(2 + text_width(&label));
        rows.push(bordered_line(
            border_color,
            w,
            vec![
                Span::styled(label, theme.muted()),
                Span::styled(truncate_to(&track.original_name, value_width), theme.text_style()),
            ],
            theme,
        ));
    }
    rows
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
    theme: super::theme::Theme,
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
            file_row(border_color, w, &label, &name, summary.as_deref(), idx == cursor, focused, theme)
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
    theme: super::theme::Theme,
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
            file_row(border_color, w, &label, &name, summary.as_deref(), idx == cursor, focused, theme)
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
    theme: super::theme::Theme,
) -> Line<'a> {
    let style = if selected && focused {
        Style::default()
            .fg(theme.pill_active_fg)
            .bg(theme.purple)
            .add_modifier(Modifier::BOLD)
    } else if selected {
        Style::default().fg(theme.purple).add_modifier(Modifier::BOLD)
    } else {
        theme.text_style()
    };
    let label_style = if selected { style } else { theme.muted() };
    let summary_style = if selected { style } else { theme.muted() };

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

    bordered_line(border_color, width, spans, theme)
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
    theme: super::theme::Theme,
) -> Line<'a> {
    let content_width: usize = content.iter().map(|s| s.width()).sum();
    let padding = width.saturating_sub(2 + content_width);

    let mut spans = vec![Span::styled("│", theme.border(border_color))];
    spans.extend(content);
    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled("│", theme.border(border_color)));
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
