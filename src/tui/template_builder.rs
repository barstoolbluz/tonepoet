//! Template builder overlay for composing folder/filename templates.
//!
//! The overlay presents a categorized grid of clickable token pills and
//! an editable template line. Clicking a token inserts `%TOKEN%` at the
//! cursor position. Templates can be saved to and loaded from disk.

use std::fs;
use std::path::PathBuf;

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::app::{TemplateBuilderFocus, TemplateBuilderState, TemplateTarget};
use super::button_map::{ButtonRenderMap, TuiButton};
use super::text_input::TextInputState;

// =========================================================================
// Token definitions
// =========================================================================

pub struct TokenCategory {
    pub label: &'static str,
    pub tokens: &'static [&'static str],
}

const FOLDER_TOKENS: &[TokenCategory] = &[
    TokenCategory {
        label: "Metadata",
        tokens: &[
            "ARTIST",
            "ALBUM_ARTIST",
            "ALBUM",
            "TITLE_EXTRA",
            "COMPOSER",
            "YEAR",
            "GENRE",
        ],
    },
    TokenCategory {
        label: "Technical",
        tokens: &["FORMAT", "SAMPLERATE", "BITDEPTH", "ISRC"],
    },
    TokenCategory {
        label: "Label & Pressing",
        tokens: &["LABEL", "COUNTRY", "PRESSING", "CATALOG"],
    },
];

const FILENAME_TOKENS: &[TokenCategory] = &[
    TokenCategory {
        label: "Numbering",
        tokens: &["TRACKNN", "TRACKN", "TRACK", "DISC", "TITLE"],
    },
    TokenCategory {
        label: "Metadata",
        tokens: &[
            "ARTIST",
            "ALBUM_ARTIST",
            "ALBUM",
            "TITLE_EXTRA",
            "COMPOSER",
            "YEAR",
            "GENRE",
        ],
    },
    TokenCategory {
        label: "Technical",
        tokens: &["FORMAT", "SAMPLERATE", "BITDEPTH", "ISRC"],
    },
    TokenCategory {
        label: "Label & Pressing",
        tokens: &["LABEL", "COUNTRY", "PRESSING", "CATALOG"],
    },
];

// The builder and its saved templates are Convert's existing component. Native
// extraction contributes only the token pills its authored format can supply;
// Convert's ordinary palette remains exactly as before.
const NATIVE_COMMON: TokenCategory = TokenCategory {
    label: "Extraction",
    tokens: &["TRACKNUMBER", "EXTENSION", "PERFORMER"],
};
const NATIVE_SACD: TokenCategory = TokenCategory {label: "SACD", tokens: &["AREA"]};
const NATIVE_DVDA: TokenCategory = TokenCategory {label: "DVD-Audio", tokens: &["GROUP"]};
const NATIVE_DVDV: TokenCategory = TokenCategory {
    label: "DVD-Video", tokens: &["DVDTITLE", "STREAM", "ANGLE"],
};
const NATIVE_BLURAY: TokenCategory = TokenCategory {
    label: "Blu-ray", tokens: &["PLAYLIST", "STREAM", "ANGLE"],
};
const NATIVE_MATROSKA: TokenCategory = TokenCategory {
    label: "Matroska", tokens: &["STREAM"],
};
const NATIVE_ARCHIVE: TokenCategory = TokenCategory {
    label: "Archive", tokens: &[],
};

fn format_native_tokens(format: crate::disc::DiscFormat) -> &'static TokenCategory {
    match format {
        crate::disc::DiscFormat::Sacd => &NATIVE_SACD,
        crate::disc::DiscFormat::DvdAudio => &NATIVE_DVDA,
        crate::disc::DiscFormat::DvdVideo => &NATIVE_DVDV,
        crate::disc::DiscFormat::BluRay => &NATIVE_BLURAY,
        crate::disc::DiscFormat::Matroska | crate::disc::DiscFormat::TransportFile => &NATIVE_MATROSKA,
        crate::disc::DiscFormat::CueImage => &NATIVE_MATROSKA,
        crate::disc::DiscFormat::Archive => &NATIVE_ARCHIVE,
    }
}

/// Use the builder's own token palette for inline Tab completion; this keeps
/// format-specific tokens out of unrelated extraction and Convert contexts.
pub(crate) fn native_completion_tokens(format: crate::disc::DiscFormat) -> Vec<String> {
    NATIVE_COMMON.tokens.iter().chain(format_native_tokens(format).tokens.iter())
        .map(|name|format!("%{name}%")).collect()
}

const SEPARATORS: &[(&str, &str)] = &[
    (" - ", " - "),
    (" / ", " / "),
    ("(", "("),
    (")", ")"),
    ("[", "["),
    ("]", "]"),
    ("{", "{"),
    ("}", "}"),
    ("space", " "),
    ("·", " · "),
    ("_", "_"),
];

// =========================================================================
// Template I/O
// =========================================================================

fn templates_dir(target: TemplateTarget) -> PathBuf {
    let base = if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(xdg).join("tonepoet").join("templates")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
            .join(".config")
            .join("tonepoet")
            .join("templates")
    } else {
        PathBuf::from("./templates")
    };
    match target {
        TemplateTarget::Folder => base.join("folder"),
        TemplateTarget::Filename => base.join("filename"),
    }
}

fn template_filename(template: &str) -> String {
    // Use distinct replacements so templates with literal dashes don't
    // collide with templates containing slashes:
    //   %ARTIST%/%ALBUM%  → %ARTIST%⁄%ALBUM%.toml  (fraction slash U+2044)
    //   %ARTIST%-%ALBUM%  → %ARTIST%-%ALBUM%.toml   (unchanged)
    let sanitized: String = template
        .chars()
        .map(|c| match c {
            '/' => '\u{2044}',   // fraction slash — visually similar, filesystem safe
            '\\' => '\u{2216}',  // set minus
            ':' => '\u{2236}',   // ratio
            '*' => '\u{2217}',   // asterisk operator
            '?' => '\u{FF1F}',   // fullwidth question mark
            '"' => '\u{201C}',   // left double quotation mark
            '<' => '\u{2039}',   // single left-pointing angle quotation
            '>' => '\u{203A}',   // single right-pointing angle quotation
            '|' => '\u{2223}',   // divides
            c => c,
        })
        .collect();
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        "untitled.toml".to_string()
    } else {
        format!("{}.toml", trimmed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamingTemplate {
    pub pattern: String,
    pub capitalize: bool,
}

impl NamingTemplate {
    #[cfg(test)]
    pub fn plain(pattern: impl Into<String>) -> Self {
        Self { pattern: pattern.into(), capitalize: false }
    }
}

pub fn list_templates(target: TemplateTarget) -> Vec<NamingTemplate> {
    let dir = templates_dir(target);
    let mut templates: Vec<NamingTemplate> = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("toml") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(parsed) = toml::from_str::<TemplateFile>(&content) {
                        if !parsed.template.trim().is_empty() {
                            templates.push(NamingTemplate { pattern: parsed.template, capitalize: parsed.capitalize });
                        }
                    }
                }
            }
        }
    }
    templates.sort_by(|a, b| a.pattern.cmp(&b.pattern));
    templates.dedup_by(|a, b| a.pattern == b.pattern);
    templates
}

pub fn save_template(target: TemplateTarget, template: &str, capitalize: bool) -> Result<(), String> {
    if template.trim().is_empty() {
        return Err("template cannot be empty".to_string());
    }
    let dir = templates_dir(target);
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create templates directory: {e}"))?;
    let filename = template_filename(template);
    let path = dir.join(&filename);
    let file = TemplateFile {
        template: template.to_string(),
        capitalize,
    };
    let content =
        toml::to_string_pretty(&file).map_err(|e| format!("failed to serialize template: {e}"))?;
    fs::write(&path, content).map_err(|e| format!("failed to write template: {e}"))?;
    Ok(())
}

pub fn delete_template(target: TemplateTarget, template: &str) -> Result<(), String> {
    let dir = templates_dir(target);
    let filename = template_filename(template);
    let path = dir.join(&filename);
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("failed to delete template: {e}"))?;
    }
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TemplateFile {
    template: String,
    #[serde(default)]
    capitalize: bool,
}

// =========================================================================
// State construction
// =========================================================================

impl TemplateBuilderState {
    pub fn new(target: TemplateTarget, initial: &str, focus: TemplateBuilderFocus) -> Self {
        let saved_templates = list_templates(target);
        let mut input = TextInputState::new(initial.to_string());
        input.cursor = input.text.len(); // cursor at end
        Self {
            template_input: input,
            capitalize: false,
            preview_page: 0,
            output_extension: "flac".to_string(),
            target,
            focus,
            grid_cursor: 0,
            saved_templates,
            saved_selected: 0,
            saved_scroll: 0,
            token_scroll: 0,
            return_to_extract: None,
            disc_format: None,
        }
    }

    pub fn token_categories(&self) -> Vec<&'static TokenCategory> {
        let mut categories: Vec<_> = match self.target {
            TemplateTarget::Folder => FOLDER_TOKENS.iter().collect(),
            TemplateTarget::Filename => FILENAME_TOKENS.iter().collect(),
        };
        if let Some(format) = self.disc_format {
            categories.push(&NATIVE_COMMON);
            categories.push(format_native_tokens(format));
        }
        categories
    }

    /// Flat list of all tokens across all categories.
    fn all_tokens(&self) -> Vec<&'static str> {
        let mut tokens: Vec<&str> = self
            .token_categories()
            .iter()
            .flat_map(|cat| cat.tokens.iter().copied())
            .collect();
        // Append separators
        for &(label, _) in SEPARATORS {
            tokens.push(label);
        }
        tokens
    }

    pub fn total_grid_items(&self) -> usize {
        self.all_tokens().len()
    }

    /// Insert the token or separator at the current grid cursor into the template input.
    pub fn insert_current_grid_item(&mut self) {
        let categories = self.token_categories();
        let num_tokens: usize = categories.iter().map(|c| c.tokens.len()).sum();

        if self.grid_cursor < num_tokens {
            // It's a token — insert %TOKEN%
            let mut idx = self.grid_cursor;
            for cat in categories {
                if idx < cat.tokens.len() {
                    let token = cat.tokens[idx];
                    self.template_input.insert_string(&format!("%{}%", token));
                    return;
                }
                idx -= cat.tokens.len();
            }
        } else {
            // It's a separator
            let sep_idx = self.grid_cursor - num_tokens;
            if let Some(&(_, insert_text)) = SEPARATORS.get(sep_idx) {
                self.template_input.insert_string(insert_text);
            }
        }
    }
}

// =========================================================================
// Drawing
// =========================================================================

/// One visual token row. Wrapping happens between complete pills, so every
/// palette token remains selectable even in an 80-column terminal.
struct BuilderTokenRow {
    heading: Option<&'static str>,
    pills: Vec<BuilderPill>,
}

struct BuilderPill {
    index: usize,
    label: &'static str,
    separator: bool,
}

fn append_builder_pills(
    rows: &mut Vec<BuilderTokenRow>,
    labels: impl Iterator<Item = &'static str>,
    inner_width: u16,
    index: &mut usize,
    separator: bool,
) {
    let mut pills = Vec::new();
    let mut x = 2u16;
    for label in labels {
        let pill_width = label.chars().count() as u16 + 2;
        if !pills.is_empty() && x.saturating_add(pill_width + 1) > inner_width {
            rows.push(BuilderTokenRow { heading: None, pills });
            pills = Vec::new();
            x = 2;
        }
        pills.push(BuilderPill { index: *index, label, separator });
        *index += 1;
        x = x.saturating_add(pill_width + 2);
    }
    if !pills.is_empty() {
        rows.push(BuilderTokenRow { heading: None, pills });
    }
}

fn builder_token_rows(state: &TemplateBuilderState, inner_width: u16) -> Vec<BuilderTokenRow> {
    let mut rows = Vec::new();
    let mut index = 0;
    for category in state.token_categories() {
        rows.push(BuilderTokenRow { heading: Some(category.label), pills: Vec::new() });
        append_builder_pills(&mut rows, category.tokens.iter().copied(), inner_width, &mut index, false);
    }
    rows.push(BuilderTokenRow { heading: Some("Separators"), pills: Vec::new() });
    append_builder_pills(&mut rows, SEPARATORS.iter().map(|&(label, _)| label),
        inner_width, &mut index, true);
    rows
}

fn builder_saved_rows(state: &TemplateBuilderState) -> u16 {
    state.saved_templates.len().min(4).max(1) as u16
}

/// Shared popup geometry for drawing, editor hit testing, and mouse dismissal.
/// The token region may scroll, but template input, saved templates and footer
/// always occupy real rows inside this rectangle.
pub(crate) fn builder_popup(area: Rect, state: &TemplateBuilderState) -> Option<Rect> {
    let w = (area.width.saturating_mul(80) / 100)
        .max(60)
        .min(area.width.saturating_sub(2));
    if w < 60 || area.height < 14 { return None; }
    let tokens = builder_token_rows(state, w.saturating_sub(2)).len() as u16;
    let inner_height = 4 + 1 + builder_saved_rows(state) + 1 + tokens + 1 + 1;
    let h = (inner_height + 2).min(area.height.saturating_sub(2));
    if h < 12 { return None; }
    Some(Rect::new(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 2, w, h,
    ))
}

pub(crate) fn builder_token_viewport(area: Rect, state: &TemplateBuilderState) -> Option<Rect> {
    let popup = builder_popup(area, state)?;
    let inner = Rect::new(popup.x + 1, popup.y + 1,
        popup.width.saturating_sub(2), popup.height.saturating_sub(2));
    let top = inner.y + 4 + 1 + builder_saved_rows(state) + 1;
    // Leave a blank row before the fixed footer so the token palette never
    // paints over actions when the modal is shorter than its natural height.
    let bottom = inner.y + inner.height.saturating_sub(2);
    if top >= bottom { return None; }
    Some(Rect::new(inner.x, top, inner.width, bottom - top))
}

pub(crate) fn builder_token_scroll_limit(area: Rect, state: &TemplateBuilderState) -> usize {
    let Some(viewport) = builder_token_viewport(area, state) else { return 0; };
    builder_token_rows(state, viewport.width).len()
        .saturating_sub(viewport.height as usize)
}

impl TemplateBuilderState {
    /// Keyboard token navigation exposes the selected pill without altering
    /// the mouse's token-scroll position on unrelated redraws.
    pub(crate) fn reveal_grid_cursor(&mut self, area: Rect) {
        let Some(viewport) = builder_token_viewport(area, self) else { return; };
        let rows = builder_token_rows(self, viewport.width);
        let Some(index) = rows.iter().position(|row| row.pills.iter().any(|p| p.index == self.grid_cursor)) else { return; };
        if index < self.token_scroll {
            self.token_scroll = index;
        } else if index >= self.token_scroll + viewport.height as usize {
            self.token_scroll = index + 1 - viewport.height as usize;
        }
    }
}

/// A terminal-cell-safe window onto one *complete* resolved output path.
/// Only the read-only presentation is paged; no replacement, truncation or
/// abbreviation is passed to an output planner. Joining all pages recovers the
/// original string, including every folder separator and the extension.
#[derive(Debug, Clone)]
pub(crate) struct PathPreviewWindow {
    pub visible: String,
    pub index: usize,
    pub total: usize,
}

fn path_preview_chunks(path: &str, cells: usize) -> Vec<String> {
    let cells = cells.max(1);
    let mut chunks = Vec::new();
    let mut part = String::new();
    let mut occupied = 0;
    for ch in path.chars() {
        let width = super::display_width::char_width(ch);
        if occupied + width > cells && !part.is_empty() {
            chunks.push(std::mem::take(&mut part));
            occupied = 0;
        }
        part.push(ch);
        occupied += width;
    }
    chunks.push(part);
    chunks
}

pub(crate) fn path_preview_window(path: &str, width: u16, requested: usize) -> PathPreviewWindow {
    let usable = (width as usize).saturating_sub(2).max(1); // arrow and space
    if super::display_width::width(path) <= usable {
        return PathPreviewWindow { visible: path.to_owned(), index: 0, total: 1 };
    }
    // The number of Unicode scalars bounds the page count. Reserve the
    // navigator at that bound, so no count-dependent relayout can oscillate
    // around a decimal digit boundary when the path grows.
    let max_pages = path.chars().count().max(1);
    let max_nav = format!("◀ {0}/{0} ▶", max_pages);
    let content_width = usable.saturating_sub(1 + super::display_width::width(&max_nav)).max(1);
    let chunks = path_preview_chunks(path, content_width);
    let index = requested % chunks.len();
    PathPreviewWindow { visible: chunks[index].clone(), index, total: chunks.len() }
}

pub(crate) fn draw_full_path_preview(
    f: &mut Frame, buttons: &mut ButtonRenderMap, area: Rect,
    path: &str, page: usize, style: Style,
) {
    if area.width == 0 || area.height == 0 { return; }
    let view = path_preview_window(path, area.width, page);
    f.render_widget(Paragraph::new(format!("→ {}", view.visible)).style(style), area);
    if view.total <= 1 || area.width < 16 { return; }
    let navigation = format!("◀ {}/{} ▶", view.index + 1, view.total);
    let nav_width = super::display_width::width(&navigation) as u16;
    let nav_x = area.x + area.width.saturating_sub(nav_width);
    f.render_widget(Paragraph::new(navigation).style(style),
        Rect::new(nav_x, area.y, nav_width, 1));
    buttons.record_button(TuiButton::NamingPreviewPrevious,
        Rect::new(nav_x, area.y, 1, 1));
    buttons.record_button(TuiButton::NamingPreviewNext,
        Rect::new(nav_x + nav_width - 1, area.y, 1, 1));
}

pub fn draw_template_builder(
    f: &mut Frame,
    state: &TemplateBuilderState,
    button_map: &mut ButtonRenderMap,
    theme: super::theme::Theme,
) {
    let Some(popup) = builder_popup(f.size(), state) else { return; };
    f.render_widget(Clear, popup);

    let title = match state.target {
        TemplateTarget::Folder => " Build folder template ",
        TemplateTarget::Filename => " Build filename template ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.amber))
        .title(Span::styled(title, Style::default().fg(theme.amber).add_modifier(Modifier::BOLD)));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    if inner.width < 40 || inner.height < 10 { return; }

    let iw = inner.width as usize;
    // Editable input: fixed at the top, even when the token palette scrolls.
    {
        let focused = state.focus == TemplateBuilderFocus::TemplateInput;
        let input_width = iw.saturating_sub(14);
        let input_style = Style::default().fg(theme.text_bright).bg(if focused {
            theme.input_focused_bg
        } else { theme.input_unfocused_bg });
        let mut spans = vec![Span::styled("  Template:  ", theme.muted())];
        spans.extend(super::inline_edit::render_text_input_value_with_style(
            &state.template_input, input_width, focused, input_style, theme));
        f.render_widget(Paragraph::new(Line::from(spans)),
            Rect::new(inner.x, inner.y, inner.width, 1));
        if focused {
            f.set_cursor(inner.x + 13 + super::inline_edit::inline_cursor_col(
                &state.template_input, input_width), inner.y);
        }
    }

    // The toggle is an ordinary mouse target and a Tab stop, independent of
    // the input caret. Render the checked state using the shared theme.
    let cap_y = inner.y + 1;
    let focus_cap = state.focus == TemplateBuilderFocus::Capitalization;
    let (scope, marker) = match (state.target, state.return_to_extract.is_some()) {
        (_, true) => ("to folder and file names", if state.capitalize { "☒" } else { "☐" }),
        (TemplateTarget::Folder, false) => ("to folder names", if state.capitalize { "☒" } else { "☐" }),
        (TemplateTarget::Filename, false) => ("to file names", if state.capitalize { "☒" } else { "☐" }),
    };
    let caption = format!("  {marker} Apply TonePoet capitalization     {scope}");
    f.render_widget(Paragraph::new(caption).style(Style::default()
        .fg(if focus_cap { theme.amber } else { theme.value })
        .add_modifier(if focus_cap { Modifier::BOLD } else { Modifier::empty() })),
        Rect::new(inner.x, cap_y, inner.width, 1));
    button_map.record_button(TuiButton::TemplateBuilderCapitalization,
        Rect::new(inner.x, cap_y, inner.width, 1));

    let template = &state.template_input.text;
    let example = state.return_to_extract.as_deref().and_then(|browser| {
        browser.native_preview_path_for_template_case(template, state.capitalize)
            .map(|path| path.to_string_lossy().into_owned())
    }).unwrap_or_else(|| render_template_preview_with_case_and_extension(
        template, state.capitalize, &state.output_extension));
    draw_full_path_preview(f, button_map,
        Rect::new(inner.x, inner.y + 2, inner.width, 1),
        &example, state.preview_page, Style::default().fg(theme.text_bright));

    // Saved templates: keep the familiar Convert list and its hit targets.
    let mut y = inner.y + 4;
    f.render_widget(Paragraph::new(Line::from(Span::styled(
        "  ── Saved templates ──",
        Style::default().fg(theme.text_dim).add_modifier(Modifier::BOLD)))),
        Rect::new(inner.x, y, inner.width, 1));
    y += 1;
    if state.saved_templates.is_empty() {
        f.render_widget(Paragraph::new(Line::from(Span::styled(
            "    (none saved)", Style::default().fg(theme.text_dim)))),
            Rect::new(inner.x, y, inner.width, 1));
    } else {
        let start = state.saved_scroll.min(state.saved_templates.len().saturating_sub(1));
        let end = (start + builder_saved_rows(state) as usize).min(state.saved_templates.len());
        for (visible_index, index) in (start..end).enumerate() {
            let selected = state.focus == TemplateBuilderFocus::SavedList && index == state.saved_selected;
            let marker = if selected { "  ▸ " } else { "    " };
            let style = Style::default().fg(if selected { theme.amber } else { theme.text });
            let tmpl = &state.saved_templates[index];
            let name_width = iw.saturating_sub(if tmpl.capitalize { 8 } else { 5 });
            let display = super::display_width::truncate_right(&tmpl.pattern, name_width);
            let mut spans = vec![Span::styled(marker, style), Span::styled(display.clone(), style)];
            if tmpl.capitalize {
                let used = 4 + super::display_width::width(&display);
                spans.push(Span::raw(" ".repeat(iw.saturating_sub(used + 2))));
                spans.push(Span::styled("Aa", Style::default().fg(theme.header)
                    .add_modifier(Modifier::BOLD)));
            }
            let row_y = y + visible_index as u16;
            f.render_widget(Paragraph::new(Line::from(spans)), Rect::new(inner.x, row_y, inner.width, 1));
            button_map.record_button(TuiButton::TemplateBuilderSavedItem(index),
                Rect::new(inner.x, row_y, inner.width, 1));
        }
    }

    // The token grid wraps complete pills into rows. Scroll only this region,
    // with all actions continuously available in the fixed footer.
    if let Some(viewport) = builder_token_viewport(f.size(), state) {
        let rows = builder_token_rows(state, viewport.width);
        let scroll = state.token_scroll.min(rows.len().saturating_sub(viewport.height as usize));
        for (visible_row, row) in rows.iter().skip(scroll)
            .take(viewport.height as usize).enumerate() {
            let row_y = viewport.y + visible_row as u16;
            if let Some(heading) = row.heading {
                f.render_widget(Paragraph::new(Line::from(Span::styled(
                    format!("  ── {heading} ──"),
                    Style::default().fg(theme.text_dim).add_modifier(Modifier::BOLD)))),
                    Rect::new(viewport.x, row_y, viewport.width, 1));
            } else {
                let mut x = viewport.x + 2;
                for pill in &row.pills {
                    let width = pill.label.chars().count() as u16 + 2;
                    let selected = state.focus == TemplateBuilderFocus::TokenGrid
                        && pill.index == state.grid_cursor;
                    let style = if selected {
                        Style::default().fg(theme.bg).bg(theme.amber).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.pill_active_fg)
                            .bg(if pill.separator { theme.purple } else { theme.blue })
                            .add_modifier(Modifier::BOLD)
                    };
                    f.render_widget(Paragraph::new(Line::from(Span::styled(
                        format!(" {} ", pill.label), style))),
                        Rect::new(x, row_y, width, 1));
                    button_map.record_button(TuiButton::TemplateBuilderToken(pill.index),
                        Rect::new(x, row_y, width, 1));
                    x += width + 2;
                }
            }
        }
        let last = rows.len().saturating_sub(viewport.height as usize);
        if last > 0 {
            // Available without another row: the gap above the token area.
            let hint = format!("  wheel tokens  {} / {}", scroll + 1, last + 1);
            f.render_widget(Paragraph::new(hint).style(Style::default().fg(theme.text_dim)),
                Rect::new(inner.x, viewport.y.saturating_sub(1), inner.width, 1));
        }
    }

    // All four footer controls stay visible and clickable at 80x24, even
    // when the token palette or saved-template list is long.
    let footer_y = inner.y + inner.height - 1;
    let pills: &[(&str, TuiButton, Color)] = &[
        ("apply", TuiButton::TemplateBuilderApply, theme.green),
        ("save", TuiButton::TemplateBuilderSave, theme.blue),
        ("clear", TuiButton::TemplateBuilderClear, theme.purple),
        ("x delete", TuiButton::TemplateBuilderDelete, theme.destructive),
    ];
    let total_w: u16 = pills.iter().map(|(l, _, _)| l.len() as u16 + 2).sum::<u16>()
        + (pills.len().saturating_sub(1) as u16);
    let left_pad = inner.width.saturating_sub(total_w) / 2;
    let mut x = inner.x + left_pad;
    let mut spans = vec![Span::raw(" ".repeat(left_pad as usize))];
    for (index, (label, button, color)) in pills.iter().enumerate() {
        if index > 0 { spans.push(Span::raw(" ")); x += 1; }
        let width = label.len() as u16 + 2;
        spans.push(Span::styled(format!(" {} ", label), Style::default()
            .fg(theme.pill_active_fg).bg(*color).add_modifier(Modifier::BOLD)));
        button_map.record_button(*button, Rect::new(x, footer_y, width, 1));
        x += width;
    }
    spans.push(Span::raw("  "));
    spans.push(Span::styled("Esc close", theme.muted()));
    f.render_widget(Paragraph::new(Line::from(spans)),
        Rect::new(inner.x, footer_y, inner.width, 1));
}

// =========================================================================
// Template picker overlay
// =========================================================================

/// Render a preview of a template using a canonical example album.
///
/// Uses the Japan CBS/Sony pressing of Pink Floyd's "Wish You Were Here"
/// (35DP-4, 1975) as a canonical example that populates every token,
/// including %TITLE_EXTRA%.
pub fn render_template_preview(template: &str) -> String {
    render_template_preview_with_case(template, false)
}

pub fn render_template_preview_with_case(template: &str, capitalize: bool) -> String {
    render_template_preview_with_case_and_extension(template, capitalize, "flac")
}

/// Use the same component sanitizer as Convert's production planner, but
/// substitute the fixed example metadata without running a second planner.
/// The extension comes from the live selected output container.
pub(crate) fn render_template_preview_with_case_and_extension(
    template: &str, capitalize: bool, output_extension: &str,
) -> String {
    let clean = crate::convert::pipeline::stages::conversion_action_sanitize_component;
    const ALBUM_FULL: &str = "Wish You Were Here (Japan  CBS-Sony 35DP-4)";
    const ALBUM_CLEAN: &str = "Wish You Were Here";
    const TITLE_EXTRA: &str = "Japan  CBS-Sony 35DP-4";

    let has_title_extra = template.contains("%TITLE_EXTRA%");
    let album = if has_title_extra { ALBUM_CLEAN } else { ALBUM_FULL };

    let mut s = template.to_string();
    s = s.replace("%ARTIST%", "Pink Floyd");
    s = s.replace("%ALBUM_ARTIST%", "Pink Floyd");
    s = s.replace("%ALBUM%", album);
    s = s.replace("%TITLE%", "Shine On You Crazy Diamond (Parts I-V)");
    s = s.replace("%TITLE_EXTRA%", if has_title_extra { TITLE_EXTRA } else { "" });
    s = s.replace("%YEAR%", "1975");
    s = s.replace("%GENRE%", "Rock");
    s = s.replace("%FORMAT%", "FLAC");
    s = s.replace("%TRACKNN%", "01");
    s = s.replace("%TRACKN%", "1");
    s = s.replace("%TRACK%", "1");
    s = s.replace("%NN%", "01");
    s = s.replace("%N%", "1");
    s = s.replace("%DISC%", "1");
    s = s.replace("%COMPOSER%", "Roger Waters");
    s = s.replace("%CATALOG%", "35DP-4");
    s = s.replace("%SAMPLERATE%", "44.1kHz");
    s = s.replace("%BITDEPTH%", "16");
    s = s.replace("%ISRC%", "GBN9Y7500101");
    s = s.replace("%LABEL%", &clean("CBS/Sony"));
    s = s.replace("%COUNTRY%", &clean("JP"));
    s = s.replace("%PRESSING%", &clean("CBS/Sony"));
    s = s.replace("%TRACKNUMBER%", "01");
    s = s.replace("%EXTENSION%", "flac");
    s = s.replace("%EXT%", output_extension);
    s = s.replace("%PERFORMER%", "Pink Floyd");
    if capitalize {
        crate::convert::renaming::capitalize_naming_path(&s, None)
    } else { s }
}

pub(crate) fn preview_for_context(
    template: &NamingTemplate,
    browser: Option<&crate::tui::disc_browser::DiscBrowserState>,
) -> String {
    preview_for_context_with_extension(template, browser, "flac")
}

pub(crate) fn preview_for_context_with_extension(
    template: &NamingTemplate,
    browser: Option<&crate::tui::disc_browser::DiscBrowserState>,
    output_extension: &str,
) -> String {
    if let Some(browser) = browser {
        if let Some(path) = browser.native_preview_path_for_template_case(
            &template.pattern, template.capitalize) {
            return path.to_string_lossy().into_owned();
        }
    }
    render_template_preview_with_case_and_extension(
        &template.pattern, template.capitalize, output_extension)
}

/// Draw the template picker overlay — a list of saved templates with preview.
pub fn draw_template_picker(
    f: &mut Frame,
    target: TemplateTarget,
    templates: &[NamingTemplate],
    selected: usize,
    scroll: usize,
    preview: &str,
    active_template: Option<&str>,
    active_capitalization: bool,
    button_map: &mut ButtonRenderMap,
    theme: super::theme::Theme,
) {

    let area = f.size();
    let w = (area.width * 75 / 100).max(50).min(area.width.saturating_sub(2));
    let list_rows = templates.len().max(1) as u16;
    // header(1) + blank(1) + list + blank(1) + preview label(1) + preview(1) + blank(1) + pills(1) + border(1)
    let content_h = 2 + 1 + list_rows + 1 + 1 + 1 + 1 + 1 + 1;
    let h = content_h.min(area.height * 60 / 100).max(8);
    let px = (area.width.saturating_sub(w)) / 2;
    let py = (area.height.saturating_sub(h)) / 2;
    let outer = Rect::new(px, py, w, h);

    f.render_widget(Clear, outer);

    let title_label = match target {
        TemplateTarget::Folder => "Load Folder Template",
        TemplateTarget::Filename => "Load Filename Template",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.cyan))
        .title(format!(" {} ", title_label))
        .title_alignment(ratatui::layout::Alignment::Center);
    f.render_widget(block, outer);

    let inner = Rect::new(outer.x + 1, outer.y + 1, outer.width.saturating_sub(2), outer.height.saturating_sub(2));
    let mut cy = inner.y;

    // Template list
    let visible_rows = (inner.height.saturating_sub(5)) as usize; // reserve for preview + hints
    if templates.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            "No saved templates",
            theme.muted(),
        )));
        f.render_widget(empty, Rect::new(inner.x + 1, cy, inner.width.saturating_sub(2), 1));
        cy += 1;
    } else {
        let end = (scroll + visible_rows).min(templates.len());
        for (i, tmpl) in templates[scroll..end].iter().enumerate() {
            let idx = scroll + i;
            let is_selected = idx == selected;
            let is_active = active_template.is_some_and(|a| a == tmpl.pattern.as_str()) && tmpl.capitalize == active_capitalization;

            let mut spans = Vec::new();

            // Selection indicator
            if is_selected {
                spans.push(Span::styled("▸ ", Style::default().fg(theme.cyan)));
            } else {
                spans.push(Span::raw("  "));
            }

            // Template text
            let style = if is_selected {
                Style::default().fg(theme.text_bright).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text)
            };
            let width = inner.width.saturating_sub(8 + if tmpl.capitalize { 3 } else { 0 }) as usize;
            spans.push(Span::styled(super::display_width::truncate_right(&tmpl.pattern, width), style));
            if tmpl.capitalize { spans.push(Span::styled("  Aa", Style::default().fg(theme.header))); }

            // Active badge
            if is_active {
                spans.push(Span::styled(" (active)", Style::default().fg(theme.green)));
            }

            let row_rect = Rect::new(inner.x, cy, inner.width, 1);
            f.render_widget(Paragraph::new(Line::from(spans)), row_rect);
            button_map.record_button(TuiButton::TemplatePickerRow(idx), row_rect);
            cy += 1;
        }
    }

    // Preview section at the bottom
    let preview_y = inner.y + inner.height.saturating_sub(4);
    if preview_y > cy {
        cy = preview_y;
    }

    // Preview label
    f.render_widget(
        Paragraph::new(Line::from(Span::styled("Example:", theme.muted()))),
        Rect::new(inner.x + 1, cy, inner.width.saturating_sub(2), 1),
    );
    cy += 1;

    // Preview value
    let preview_style = Style::default().fg(theme.text_bright);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(preview, preview_style))),
        Rect::new(inner.x + 2, cy, inner.width.saturating_sub(4), 1),
    );
    cy += 2; // blank row before pills

    // Footer pills
    let pills: &[(&str, TuiButton, Color)] = &[
        ("apply", TuiButton::TemplatePickerApply, theme.green),
        ("delete", TuiButton::TemplatePickerDelete, theme.destructive),
        ("close", TuiButton::TemplatePickerClose, theme.purple),
    ];
    let total_w: u16 = pills
        .iter()
        .map(|(l, _, _)| l.len() as u16 + 2)
        .sum::<u16>()
        + (pills.len().saturating_sub(1) as u16);
    let left_pad = inner.width.saturating_sub(total_w) / 2;
    let mut px = inner.x + left_pad;
    let mut spans: Vec<Span> = vec![Span::raw(" ".repeat(left_pad as usize))];
    for (i, (label, btn, color)) in pills.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
            px += 1;
        }
        let pill_w = label.len() as u16 + 2;
        let pill = Span::styled(
            format!(" {} ", label),
            Style::default()
                .fg(theme.pill_active_fg)
                .bg(*color)
                .add_modifier(Modifier::BOLD),
        );
        button_map.record_button(*btn, Rect::new(px, cy, pill_w, 1));
        spans.push(pill);
        px += pill_w;
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect::new(inner.x, cy, inner.width, 1),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn capitalization_toggle_and_saved_badge_have_distinct_hit_targets() {
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state = TemplateBuilderState::new(TemplateTarget::Filename,
            "%ALBUM_ARTIST%/%TRACKNN% - %TITLE%", TemplateBuilderFocus::Capitalization);
        state.saved_templates = vec![NamingTemplate {
            pattern: "%ARTIST%/%TITLE%".into(), capitalize: true,
        }];
        let mut buttons = ButtonRenderMap::new();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw_template_builder(frame, &state, &mut buttons, theme)).unwrap();
        let toggle = buttons.button_rect(TuiButton::TemplateBuilderCapitalization)
            .expect("capitalization toggle should be visible");
        assert_eq!(buttons.find_button_at(toggle.x + 2, toggle.y),
            Some(TuiButton::TemplateBuilderCapitalization));
        assert_eq!(terminal.backend().buffer().get(toggle.x + 2, toggle.y).symbol(), "☐");
        state.capitalize = true;
        terminal.draw(|frame| draw_template_builder(frame, &state, &mut buttons, theme)).unwrap();
        assert_eq!(terminal.backend().buffer().get(toggle.x + 2, toggle.y).symbol(), "☒");
        let saved = buttons.button_rect(TuiButton::TemplateBuilderSavedItem(0)).unwrap();
        assert_eq!(buttons.find_button_at(saved.x + 2, saved.y),
            Some(TuiButton::TemplateBuilderSavedItem(0)));
        assert_eq!(terminal.backend().buffer().get(saved.x + saved.width - 2, saved.y).symbol(), "A");
    }

    #[test]
    fn capitalization_setting_round_trips_in_saved_template_format() {
        let input = TemplateFile { template: "%ALBUM%/%TITLE%".into(), capitalize: true };
        let data = toml::to_string(&input).unwrap();
        let restored: TemplateFile = toml::from_str(&data).unwrap();
        assert_eq!(restored.template, input.template);
        assert!(restored.capitalize);
        assert!(!toml::from_str::<TemplateFile>("template = '%TITLE%'")
            .unwrap().capitalize);
    }

    #[test]
    fn template_builder_input_paints_partial_selection() {
        let theme = crate::tui::theme::theme_by_slug_or_default(
            crate::tui::theme::default_theme_slug(),
        );
        let mut state = TemplateBuilderState::new(
            TemplateTarget::Filename,
            "abcdef",
            TemplateBuilderFocus::TemplateInput,
        );
        state.template_input.selection_anchor = Some(1);
        state.template_input.cursor = 4;
        let mut button_map = ButtonRenderMap::new();
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| draw_template_builder(frame, &state, &mut button_map, theme))
            .expect("draw template builder");

        let selected_cells = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .filter(|cell| {
                matches!(cell.symbol(), "b" | "c" | "d")
                    && cell.fg == theme.bg
                    && cell.bg == theme.text_bright
            })
            .count();
        assert_eq!(selected_cells, 3);
    }

    #[test]
    fn r28_shared_builder_keeps_footer_and_all_tokens_mouse_reachable_at_standard_sizes() {
        use std::collections::HashSet;
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        for (width, height) in [(80,24),(100,30),(160,40)] {
            for saved in [0,4] {
                // Both the extraction extension and the unchanged ordinary
                // Convert palette use the same viewport and button map.
                for native in [false,true] {
                    let mut state = TemplateBuilderState::new(TemplateTarget::Filename,
                        "%TRACKNN% - %TITLE%",TemplateBuilderFocus::TemplateInput);
                    state.saved_templates = (0..saved).map(|n|NamingTemplate::plain(format!("%ARTIST% - {n}"))).collect();
                    state.disc_format = native.then_some(crate::disc::DiscFormat::Sacd);
                    let area = Rect::new(0,0,width,height);
                    let popup = builder_popup(area,&state).expect("popup should fit");
                    let viewport = builder_token_viewport(area,&state).expect("token viewport");
                    let last_scroll=builder_token_scroll_limit(area,&state);
                    let mut seen=HashSet::new();
                    let mut terminal=Terminal::new(TestBackend::new(width,height)).unwrap();
                    for scroll in 0..=last_scroll {
                        state.token_scroll=scroll;
                        let mut buttons=ButtonRenderMap::new();
                        terminal.draw(|f|draw_template_builder(f,&state,&mut buttons,theme)).unwrap();
                        for (button,letter) in [
                            (TuiButton::TemplateBuilderApply,"a"),
                            (TuiButton::TemplateBuilderSave,"s"),
                            (TuiButton::TemplateBuilderClear,"c"),
                            (TuiButton::TemplateBuilderDelete,"x"),
                        ] {
                            let rect=buttons.button_rect(button).expect("fixed footer action");
                            assert_eq!(rect.y,popup.y+popup.height-2,
                                "{width}x{height}, native={native}, saved={saved}");
                            assert_eq!(terminal.backend().buffer().get(rect.x+1,rect.y).symbol(),letter);
                            assert_eq!(buttons.find_button_at(rect.x+1,rect.y),Some(button));
                        }
                        for index in 0..state.total_grid_items() {
                            let target=TuiButton::TemplateBuilderToken(index);
                            if let Some(rect)=buttons.button_rect(target) {
                                assert!(rect.y>=viewport.y && rect.y<viewport.y+viewport.height,
                                    "token hitbox clipped by footer or modal frame");
                                assert_eq!(buttons.find_button_at(rect.x+1,rect.y),Some(target));
                                seen.insert(index);
                            }
                        }
                    }
                    assert_eq!(seen.len(),state.total_grid_items(),
                        "all Convert and native tokens must be reachable by mouse at {width}x{height}, saved={saved}, native={native}");
                    // Clicking a visible native item uses the original
                    // index-to-token insertion path, not a second map.
                    if native {
                        let index=state.token_categories().iter().map(|c|c.tokens.len()).sum::<usize>()-1;
                        state.grid_cursor=index;
                        state.template_input=TextInputState::empty();
                        state.insert_current_grid_item();
                        assert_eq!(state.template_input.text,"%AREA%");
                    }
                }
            }
        }
    }

    #[test]
    fn corrective_convert_preview_sanitizes_metadata_separators_and_uses_selected_container() {
        let mut selected_format=crate::tui::app::FormatState::new();
        selected_format.format.select_value(&crate::convert::formats::AudioFormat::Mp3);
        assert_eq!(selected_format.selected_extension(),"mp3");
        let selected_extension=selected_format.selected_extension();
        let sanitize = crate::convert::pipeline::stages::conversion_action_sanitize_component;
        let safe = sanitize("CBS/Sony");
        assert_eq!(safe, "CBS Sony");
        for token in ["LABEL", "PRESSING"] {
            let template = format!("%{token}%/%ALBUM%");
            let preview = render_template_preview_with_case_and_extension(&template, false, selected_extension);
            assert!(preview.starts_with(&format!("{safe}/Wish You Were Here")), "{preview:?}");
            assert_eq!(preview.matches('/').count(), 1,
                "only the slash authored in the template creates a directory");
            assert!(!preview.contains("CBS/Sony"));
            let cased = render_template_preview_with_case_and_extension(&template, true, selected_extension);
            assert_eq!(cased.matches('/').count(), 1);
            assert!(cased.starts_with("CBS Sony/"));
        }
        let filename = "%TRACKNN% - %TITLE%.%EXT%";
        let selected = render_template_preview_with_case_and_extension(filename, true, selected_extension);
        assert!(selected.starts_with("01 - Shine on You Crazy Diamond"), "{selected}");
        assert!(selected.ends_with(".mp3"), "{selected}");
        assert!(!selected.ends_with(".flac"));
        let saved = NamingTemplate {pattern: filename.into(), capitalize: true};
        assert_eq!(preview_for_context_with_extension(&saved, None, selected_extension), selected,
            "saved-template picker and Custom must use the same preview");
    }

    #[test]
    fn corrective_preview_pager_preserves_complete_unicode_paths_at_standard_sizes() {
        let path = format!("/home/user/Music/Extracted/{}/{} - {}.dsf",
            "Björk – an extraordinarily long collection folder/Another Long Album",
            "01", "Shine on You Crazy Diamond and Some Additional Extended Track Name");
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        for (width,height) in [(80,24),(100,30)] {
            let mut state = TemplateBuilderState::new(TemplateTarget::Filename,
                &path,TemplateBuilderFocus::TemplateInput);
            let popup = builder_popup(Rect::new(0,0,width,height),&state).unwrap();
            let preview_rect = Rect::new(popup.x+1,popup.y+3,popup.width-2,1);
            let first = path_preview_window(&path,preview_rect.width,0);
            assert!(first.total>1, "long path must be pageable at {width}x{height}");
            let mut concatenated=String::new();
            let mut terminal=Terminal::new(TestBackend::new(width,height)).unwrap();
            for page in 0..first.total {
                state.preview_page=page;
                let mut buttons=ButtonRenderMap::new();
                // The sample preview is deliberately rendered by the same
                // reusable widget that Custom and the Extract form call.
                terminal.draw(|frame|draw_full_path_preview(frame,&mut buttons,
                    preview_rect,&path,state.preview_page,
                    Style::default().fg(theme.text_bright))).unwrap();
                let view=path_preview_window(&path,preview_rect.width,page);
                concatenated.push_str(&view.visible);
                let displayed=(preview_rect.x..preview_rect.x+preview_rect.width)
                    .map(|x|terminal.backend().buffer().get(x,preview_rect.y).symbol().to_owned())
                    .collect::<Vec<_>>().join("");
                assert!(displayed.contains(&format!("→ {}",view.visible)),
                    "page {page}, {width}x{height}: {displayed:?}");
                for button in [TuiButton::NamingPreviewPrevious,TuiButton::NamingPreviewNext] {
                    let rect=buttons.button_rect(button).expect("preview page arrows");
                    assert_eq!(buttons.find_button_at(rect.x,rect.y),Some(button));
                    assert_eq!(rect.y,preview_rect.y);
                }
            }
            assert_eq!(concatenated,path,"pagination must preserve every source character");
            // Custom's actual drawing must keep its footer and token view
            // separate, regardless of the read-only preview page.
            let mut buttons=ButtonRenderMap::new();
            terminal.draw(|f|draw_template_builder(f,&state,&mut buttons,theme)).unwrap();
            for button in [TuiButton::TemplateBuilderApply,TuiButton::TemplateBuilderSave,
                TuiButton::TemplateBuilderClear,TuiButton::TemplateBuilderDelete] {
                let rect=buttons.button_rect(button).expect("footer must remain reachable");
                assert_eq!(buttons.find_button_at(rect.x,rect.y),Some(button));
            }
            assert!(builder_token_viewport(Rect::new(0,0,width,height),&state).is_some());
        }
    }

    #[test]
    fn native_builder_shows_only_relevant_disc_tokens() {
        let mut builder = TemplateBuilderState::new(TemplateTarget::Filename,
            "%TRACKNN% - %TITLE%",TemplateBuilderFocus::TemplateInput);
        let convert_count = builder.total_grid_items();
        assert!(builder.token_categories().iter().all(|category|
            category.label != "Extraction"));
        builder.disc_format = Some(crate::disc::DiscFormat::Sacd);
        let sacd = builder.token_categories().into_iter().flat_map(|c|c.tokens.iter().copied())
            .collect::<Vec<_>>();
        assert!(sacd.contains(&"AREA") && sacd.contains(&"TRACKNUMBER"));
        assert!(!sacd.contains(&"DVDTITLE") && !sacd.contains(&"PLAYLIST"));
        assert!(builder.total_grid_items() > convert_count);
        builder.disc_format = Some(crate::disc::DiscFormat::DvdVideo);
        let dvd = builder.token_categories().into_iter().flat_map(|c|c.tokens.iter().copied())
            .collect::<Vec<_>>();
        assert!(dvd.contains(&"DVDTITLE") && dvd.contains(&"ANGLE"));
        assert!(!dvd.contains(&"AREA") && !dvd.contains(&"PLAYLIST"));
    }
}
