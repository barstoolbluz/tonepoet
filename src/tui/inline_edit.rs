//! Reusable inline text-field rendering helpers.
//!
//! This module deliberately keeps rendering policy (focused input background,
//! selected-text handling, empty-value presentation, and UTF-8-safe truncation)
//! in one place so convert-pane fields, browse metadata fields, and file-list
//! renames do not grow subtly different inline editors.

use ratatui::{
    style::Style,
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
    let value_width = width.saturating_sub(label.chars().count());
    vec![
        Span::styled(label.to_string(), label_style),
        render_inline_value(value, editing, input, focused, value_width, theme),
    ]
}

/// Render only the editable/display value portion of an inline field.
pub fn render_inline_value(
    value: &str,
    editing: bool,
    input: &TextInputState,
    focused: bool,
    width: usize,
    theme: super::theme::Theme,
) -> Span<'static> {
    let display = if editing {
        let (view, _) = input.view(width.max(1));
        pad_to_width(&view, width.max(1))
    } else if value.is_empty() {
        truncate_to("(empty)", width)
    } else {
        truncate_to(value, width)
    };

    let style = if editing {
        Style::default()
            .fg(theme.text_bright)
            .bg(theme.input_focused_bg)
    } else if focused {
        theme.bright()
    } else {
        theme.text_style()
    };
    Span::styled(display, style)
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
    let width = width.max(1);
    let (visible, cursor_col) = input.view(width);
    let cursor_col = cursor_col as usize;
    let cursor_absolute_col = input.cursor_display_col();
    let scroll = cursor_absolute_col.saturating_sub(width.saturating_sub(1));
    let selection_cols = input.selection_range().map(|range| {
        let start = input.text[..range.start].chars().count();
        let end = input.text[..range.end].chars().count();
        (start.saturating_sub(scroll), end.saturating_sub(scroll))
    });

    let mut chars: Vec<char> = visible
        .chars()
        .map(|c| if c == '\n' || c == '\r' { '↵' } else { c })
        .collect();
    if chars.len() < width {
        chars.extend(std::iter::repeat(' ').take(width - chars.len()));
    }

    let normal = Style::default()
        .fg(theme.text_bright)
        .bg(theme.input_focused_bg);
    let selected = Style::default()
        .fg(theme.bg)
        .bg(theme.selection_bg);
    let cursor_style = Style::default().fg(theme.bg).bg(theme.text_bright);

    let mut spans = Vec::new();
    let mut current_style: Option<Style> = None;
    let mut current = String::new();
    for (idx, ch) in chars.into_iter().enumerate() {
        let style = if idx == cursor_col {
            cursor_style
        } else if let Some((start, end)) = selection_cols {
            if idx >= start && idx < end { selected } else { normal }
        } else {
            normal
        };
        if current_style == Some(style) {
            current.push(ch);
        } else {
            if let Some(style) = current_style.take() {
                spans.push(Span::styled(std::mem::take(&mut current), style));
            }
            current_style = Some(style);
            current.push(ch);
        }
    }
    if let Some(style) = current_style {
        spans.push(Span::styled(current, style));
    }
    spans
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
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    if max_chars == 1 {
        return "…".to_string();
    }
    let truncated: String = s.chars().take(max_chars - 1).collect();
    format!("{}…", truncated)
}

fn pad_to_width(s: &str, width: usize) -> String {
    let count = s.chars().count();
    if count >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_value_is_padded_to_keep_cursor_cell_visible() {
        let theme = crate::tui::theme::theme_by_slug_or_default(crate::tui::theme::default_theme_slug());
        let input = TextInputState::new("abc".to_string());
        let span = render_inline_value("ignored", true, &input, true, 5, theme);
        assert_eq!(span.content.as_ref(), "abc  ");
    }

    #[test]
    fn empty_nonediting_value_is_explicit() {
        let theme = crate::tui::theme::theme_by_slug_or_default(crate::tui::theme::default_theme_slug());
        let input = TextInputState::empty();
        let span = render_inline_value("", false, &input, false, 20, theme);
        assert_eq!(span.content.as_ref(), "(empty)");
    }

    #[test]
    fn embedded_cursor_renderer_highlights_selection() {
        let theme = crate::tui::theme::theme_by_slug_or_default(crate::tui::theme::default_theme_slug());
        let mut input = TextInputState::new("abcdef".to_string());
        input.cursor = 1;
        input.extend_right();
        input.extend_right();
        input.extend_right();
        let spans = render_inline_value_with_embedded_cursor(&input, 6, theme);
        let selected = spans
            .iter()
            .find(|span| span.style.bg == Some(theme.selection_bg))
            .expect("selection should produce highlighted span");
        assert_eq!(selected.content.as_ref(), "bcd");
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
                    && span.style.fg == Some(theme.bg)
                    && span.style.bg == Some(theme.text_bright)
            })
            .expect("cursor cell should be rendered with inverted style");
        assert_eq!(cursor_span.content.as_ref(), " ");
    }
}
