//! Reusable inline text-field rendering helpers.
//!
//! This module deliberately keeps rendering policy (focused input background,
//! selected-text handling, empty-value presentation, and UTF-8-safe truncation)
//! in one place so convert-pane fields, browse metadata fields, and file-list
//! renames do not grow subtly different inline editors.

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use super::text_input::TextInputState;

/// Render an inline text field. When `editing` is true, show the TextInputState
/// with cursor-managed scrolled text; when false, show the display value with
/// truncation. The caller owns terminal cursor placement because it knows the
/// field's absolute screen coordinates.
pub fn render_inline_text_field(
    label: &str,
    value: &str,
    editing: bool,
    input: &TextInputState,
    focused: bool,
    width: usize,
    theme: super::theme::Theme,
) -> Vec<Span<'static>> {
    let label_style = if focused { theme.bright() } else { theme.muted() };
    let value_width = width.saturating_sub(super::display_width::width(label));
    let mut spans = vec![Span::styled(label.to_string(), label_style)];
    spans.extend(render_inline_value(
        value,
        editing,
        input,
        focused,
        value_width,
        theme,
    ));
    spans
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EditingCellStyles {
    normal: Style,
    selection: Style,
    cursor_unselected: Style,
    cursor_selected: Style,
}

fn editing_cell_styles(theme: super::theme::Theme, normal: Style) -> EditingCellStyles {
    // Four deliberately different surfaces. In particular, the cursor must not
    // reuse the selection style: doing so made it disappear whenever the
    // terminal hardware cursor was hidden or low-contrast.
    EditingCellStyles {
        normal,
        selection: Style::default().fg(theme.bg).bg(theme.text_bright),
        cursor_unselected: Style::default()
            .fg(theme.editing_cursor_foreground())
            .bg(theme.editing_cursor)
            .add_modifier(Modifier::BOLD),
        cursor_selected: Style::default()
            .fg(theme.text_bright)
            .bg(theme.bg)
            .add_modifier(Modifier::BOLD),
    }
}

fn push_text(spans: &mut Vec<Span<'static>>, text: &str, style: Style) {
    if let Some(last) = spans.last_mut().filter(|span| span.style == style) {
        last.content.to_mut().push_str(text);
    } else {
        spans.push(Span::styled(text.to_string(), style));
    }
}

fn render_editing_spans(
    input: &TextInputState,
    width: usize,
    theme: super::theme::Theme,
    normal: Style,
    paint_selection: bool,
    embedded_cursor: bool,
    pad_to_width: bool,
) -> Vec<Span<'static>> {
    let width = width.max(1);
    let (visible, cursor_col) = input.view(width);
    let cursor_col = cursor_col as usize;
    let cursor_absolute_col = input.cursor_display_col();
    let scroll = cursor_absolute_col.saturating_sub(cursor_col);
    let selection_cols = input.selection_range().map(|range| {
        let start = super::display_width::width(&input.text[..range.start]);
        let end = super::display_width::width(&input.text[..range.end]);
        (start.saturating_sub(scroll), end.saturating_sub(scroll))
    });
    let styles = editing_cell_styles(theme, normal);

    let mut spans = Vec::new();
    let mut cell_col = 0usize;
    for source_ch in visible.chars() {
        let rendered_ch = if source_ch == '\n' || source_ch == '\r' {
            '↵'
        } else {
            source_ch
        };
        let ch_width = super::display_width::char_width(rendered_ch);
        if cell_col.saturating_add(ch_width) > width {
            break;
        }
        let selected_cell = paint_selection && selection_cols.is_some_and(|(start, end)| {
            ch_width > 0 && cell_col < end && cell_col.saturating_add(ch_width) > start
        });
        let cursor_cell = paint_selection
            && embedded_cursor
            && ch_width > 0
            && cursor_col >= cell_col
            && cursor_col < cell_col.saturating_add(ch_width);
        let style = if cursor_cell && selected_cell {
            styles.cursor_selected
        } else if cursor_cell {
            styles.cursor_unselected
        } else if selected_cell {
            styles.selection
        } else {
            styles.normal
        };
        let mut encoded = [0u8; 4];
        push_text(&mut spans, rendered_ch.encode_utf8(&mut encoded), style);
        cell_col = cell_col.saturating_add(ch_width);
    }

    while pad_to_width && cell_col < width {
        let style = if paint_selection && embedded_cursor && cell_col == cursor_col {
            styles.cursor_unselected
        } else {
            styles.normal
        };
        push_text(&mut spans, " ", style);
        cell_col += 1;
    }
    spans
}

/// Render only the editable/display value portion of an inline field.
pub fn render_inline_value(
    value: &str,
    editing: bool,
    input: &TextInputState,
    focused: bool,
    width: usize,
    theme: super::theme::Theme,
) -> Vec<Span<'static>> {
    if editing {
        let normal = Style::default()
            .fg(theme.text_bright)
            .bg(theme.input_focused_bg);
        return render_editing_spans(input, width, theme, normal, true, true, true);
    }
    let display = if value.is_empty() {
        truncate_to("(empty)", width)
    } else {
        truncate_to(value, width)
    };
    let style = if focused { theme.bright() } else { theme.text_style() };
    vec![Span::styled(display, style)]
}


/// Render an inline edit value as spans with an embedded cursor cell.
///
/// Most pane renderers can place the terminal cursor themselves and should use
/// [`render_inline_value`]. Overlay/list renderers that build plain `Line`s do
/// not always have stable absolute coordinates for the edited value, so this
/// helper keeps the same scrolled `TextInputState` view, focused input
/// background, newline sanitization, and cursor-cell policy centralized here
/// instead of open-coding another inline editor.
pub fn render_inline_value_with_embedded_cursor(
    input: &TextInputState,
    width: usize,
    theme: super::theme::Theme,
) -> Vec<Span<'static>> {
    let normal = Style::default()
        .fg(theme.text_bright)
        .bg(theme.input_focused_bg);
    render_editing_spans(input, width, theme, normal, true, true, true)
}

/// Render a standalone single-line [`TextInputState`] while preserving its
/// horizontally scrolled view and painting the active selection. This is the
/// common path for modal prompts, codec-setting inputs, template builders, and
/// the vi command line. The caller owns terminal cursor placement.
pub fn render_text_input_value(
    input: &TextInputState,
    width: usize,
    focused: bool,
    theme: super::theme::Theme,
) -> Vec<Span<'static>> {
    let normal = Style::default()
        .fg(if focused { theme.text_bright } else { theme.text })
        .bg(if focused {
            theme.input_focused_bg
        } else {
            theme.input_unfocused_bg
        });
    render_editing_spans(input, width, theme, normal, focused, focused, true)
}

/// Render a standalone text input using a caller-owned normal style while
/// retaining the shared inverse-video selection treatment. This keeps the
/// vi command line, bulk rename, and other specialized surfaces visually
/// unchanged outside the selected range.
pub fn render_text_input_value_with_style(
    input: &TextInputState,
    width: usize,
    focused: bool,
    normal: Style,
    theme: super::theme::Theme,
) -> Vec<Span<'static>> {
    render_editing_spans(input, width, theme, normal, focused, focused, true)
}

/// Compact variant of [`render_text_input_value_with_style`].
pub fn render_text_input_value_compact_with_style(
    input: &TextInputState,
    width: usize,
    focused: bool,
    normal: Style,
    theme: super::theme::Theme,
) -> Vec<Span<'static>> {
    render_editing_spans(input, width, theme, normal, focused, focused, false)
}

/// Render a complete labelled inline row with an embedded cursor cell.
/// This is useful for metadata-editor overlay rows, while output-options and
/// browse panes use the cursor-positioning variant above.
pub fn render_inline_text_line_with_embedded_cursor(
    label: impl Into<String>,
    label_style: Style,
    input: &TextInputState,
    width: usize,
    theme: super::theme::Theme,
) -> Line<'static> {
    let mut spans = vec![Span::styled(label.into(), label_style)];
    spans.extend(render_inline_value_with_embedded_cursor(input, width, theme));
    Line::from(spans)
}

/// Compute the cursor column inside an inline value rendered with `width`.
pub fn inline_cursor_col(input: &TextInputState, width: usize) -> u16 {
    input.view(width.max(1)).1
}

fn truncate_to(s: &str, max_chars: usize) -> String {
    super::display_width::truncate_right(s, max_chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_value_is_padded_to_keep_cursor_cell_visible() {
        let theme = crate::tui::theme::theme_by_slug_or_default(crate::tui::theme::default_theme_slug());
        let input = TextInputState::new("abc".to_string());
        let spans = render_inline_value("ignored", true, &input, true, 5, theme);
        let rendered = spans.iter().map(|span| span.content.as_ref()).collect::<String>();
        assert_eq!(rendered, "abc  ");
    }

    #[test]
    fn empty_nonediting_value_is_explicit() {
        let theme = crate::tui::theme::theme_by_slug_or_default(crate::tui::theme::default_theme_slug());
        let input = TextInputState::empty();
        let spans = render_inline_value("", false, &input, false, 20, theme);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "(empty)");
    }

    #[test]
    fn embedded_cursor_renderer_is_exact_for_wide_and_combining_text() {
        let theme = crate::tui::theme::theme_by_slug_or_default(crate::tui::theme::default_theme_slug());
        let mut input = TextInputState::new("日本e\u{301}".to_string());
        input.cursor_end();
        let spans = render_inline_value_with_embedded_cursor(&input, 5, theme);
        let rendered = spans.iter().map(|span| span.content.as_ref()).collect::<String>();

        assert_eq!(crate::tui::display_width::width(&rendered), 5);
        assert_eq!(rendered, "本e\u{301}  ");
    }

    #[test]
    fn embedded_cursor_renderer_reuses_scrolled_text_input_view() {
        let theme = crate::tui::theme::theme_by_slug_or_default(crate::tui::theme::default_theme_slug());
        let mut input = TextInputState::new("0123456789".to_string());
        input.cursor_end();
        let spans = render_inline_value_with_embedded_cursor(&input, 5, theme);
        let rendered = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert_eq!(rendered, "6789 ");
        let cursor_span = spans
            .iter()
            .find(|span| {
                span.content.as_ref() == " "
                    && span.style.fg == Some(theme.editing_cursor_foreground())
                    && span.style.bg == Some(theme.editing_cursor)
            })
            .expect("cursor cell should be rendered with its dedicated style");
        assert_eq!(cursor_span.content.as_ref(), " ");
    }
    #[test]
    fn editing_cursor_clears_contrast_thresholds_in_every_builtin_theme() {
        for palette in crate::tui::theme::palettes() {
            let theme = crate::tui::theme::Theme::from_palette(palette);
            let foreground = theme.editing_cursor_foreground();
            assert!(
                crate::tui::theme::contrast_ratio(foreground, theme.editing_cursor) >= 4.5,
                "{} cursor glyph contrast was {}",
                palette.slug,
                crate::tui::theme::contrast_ratio(foreground, theme.editing_cursor)
            );
            assert!(
                crate::tui::theme::contrast_ratio(
                    theme.editing_cursor,
                    theme.input_focused_bg,
                ) >= 3.0,
                "{} cursor surface contrast was {}",
                palette.slug,
                crate::tui::theme::contrast_ratio(
                    theme.editing_cursor,
                    theme.input_focused_bg,
                )
            );
            assert_ne!(
                theme.editing_cursor,
                theme.info,
                "{} cursor must not fall back to the info/cyan accent",
                palette.slug,
            );
        }
    }

    #[test]
    fn inverse_selection_pair_clears_contrast_thresholds_in_every_builtin_theme() {
        for palette in crate::tui::theme::palettes() {
            let theme = crate::tui::theme::Theme::from_palette(palette);
            assert!(
                crate::tui::theme::contrast_ratio(theme.bg, theme.text_bright) >= 4.5,
                "{} selected text contrast was {}",
                palette.slug,
                crate::tui::theme::contrast_ratio(theme.bg, theme.text_bright)
            );
        }
    }

    #[test]
    fn standalone_text_input_renderer_paints_partial_selection_only_when_focused() {
        let theme = crate::tui::theme::theme_by_slug_or_default(
            crate::tui::theme::default_theme_slug(),
        );
        let mut input = TextInputState::new("abcdef".to_string());
        input.selection_anchor = Some(1);
        input.cursor = 4;

        let focused = render_text_input_value(&input, 12, true, theme);
        assert!(focused.iter().any(|span| {
            span.content.as_ref() == "bcd"
                && span.style.fg == Some(theme.bg)
                && span.style.bg == Some(theme.text_bright)
        }));

        let unfocused = render_text_input_value(&input, 12, false, theme);
        assert!(!unfocused.iter().any(|span| {
            span.style.fg == Some(theme.bg) && span.style.bg == Some(theme.text_bright)
        }));
    }

    #[test]
    fn terminal_cursor_inline_renderer_paints_select_all() {
        let theme = crate::tui::theme::theme_by_slug_or_default(
            crate::tui::theme::default_theme_slug(),
        );
        let input = TextInputState::new_selected("replace me".to_string());
        let spans = render_inline_value("replace me", true, &input, true, 12, theme);
        let selected_text = spans
            .iter()
            .filter(|span| {
                span.style.fg == Some(theme.bg)
                    && span.style.bg == Some(theme.text_bright)
            })
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(selected_text, "replace me");
        // Select-all leaves the cursor after the selection (new_selected puts
        // it at text end), so the embedded cursor is the pad cell in its
        // dedicated cursor-outside-selection style, distinct from all others.
        assert!(spans.iter().any(|span| {
            span.content.as_ref() == " "
                && span.style.fg == Some(theme.editing_cursor_foreground())
                && span.style.bg == Some(theme.editing_cursor)
        }));
    }

    fn assert_four_state_matrix(theme: crate::tui::theme::Theme) {
        let normal = Style::default()
            .fg(theme.text_bright)
            .bg(theme.input_focused_bg);
        let styles = editing_cell_styles(theme, normal);
        let all = [
            styles.normal,
            styles.cursor_unselected,
            styles.selection,
            styles.cursor_selected,
        ];
        for left in 0..all.len() {
            for right in (left + 1)..all.len() {
                assert_ne!(
                    (all[left].fg, all[left].bg, all[left].add_modifier),
                    (all[right].fg, all[right].bg, all[right].add_modifier),
                    "{} editor states {left} and {right} must be visually distinct",
                    theme.slug
                );
            }
        }

        assert_ne!(styles.normal.bg, styles.cursor_unselected.bg);
        assert_ne!(styles.selection.bg, styles.cursor_selected.bg);
        assert_ne!(styles.normal.bg, styles.selection.bg);
    }

    #[test]
    fn cursor_selection_matrix_is_distinct_in_default_and_light_themes() {
        let default = crate::tui::theme::theme_by_slug_or_default(
            crate::tui::theme::default_theme_slug(),
        );
        let light = crate::tui::theme::theme_by_slug_or_default("tokyo-night-day");
        assert_four_state_matrix(default);
        assert_four_state_matrix(light);
    }

    #[test]
    fn embedded_cursor_has_distinct_styles_inside_and_outside_selection() {
        let theme = crate::tui::theme::theme_by_slug_or_default(
            crate::tui::theme::default_theme_slug(),
        );
        let mut input = TextInputState::new("abcd".to_string());
        input.selection_anchor = Some(4);
        input.cursor = 2;
        let selected_cursor = render_inline_value_with_embedded_cursor(&input, 4, theme);
        assert!(selected_cursor.iter().any(|span| {
            span.content.as_ref() == "c"
                && span.style.fg == Some(theme.text_bright)
                && span.style.bg == Some(theme.bg)
        }));

        input.clear_selection();
        input.cursor = 3;
        let unselected_cursor = render_inline_value_with_embedded_cursor(&input, 4, theme);
        assert!(unselected_cursor.iter().any(|span| {
            span.content.as_ref() == "d"
                && span.style.fg == Some(theme.editing_cursor_foreground())
                && span.style.bg == Some(theme.editing_cursor)
        }));
    }

}
