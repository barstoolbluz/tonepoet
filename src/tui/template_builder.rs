//! Template builder overlay for composing folder/filename templates.
//!
//! The overlay presents a categorized grid of clickable token pills and
//! an editable template line. Clicking a token inserts `%TOKEN%` at the
//! cursor position. Templates can be saved to and loaded from disk.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::app::{TemplateBuilderFocus, TemplateBuilderState, TemplateSaveDialog,
    TemplateSaveFocus, TemplateTarget};
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

fn template_filename(name: &str) -> String {
    // Use distinct replacements so templates with literal dashes don't
    // collide with templates containing slashes:
    //   %ARTIST%/%ALBUM%  → %ARTIST%⁄%ALBUM%.toml  (fraction slash U+2044)
    //   %ARTIST%-%ALBUM%  → %ARTIST%-%ALBUM%.toml   (unchanged)
    let sanitized: String = name
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
    pub name: String,
    pub pattern: String,
    pub capitalize: bool,
}

impl NamingTemplate {
    #[cfg(test)]
    pub fn plain(pattern: impl Into<String>) -> Self {
        let pattern = pattern.into();
        Self { name: pattern.clone(), pattern, capitalize: false }
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
                            templates.push(NamingTemplate {
                                name: parsed.name, pattern: parsed.template, capitalize: parsed.capitalize,
                            });
                        }
                    }
                }
            }
        }
    }
    templates.sort_by(|a, b| a.name.cmp(&b.name));
    templates
}

/// New saves use create_new: neither a loaded template nor an unrelated name
/// can be overwritten by an incidental Save click. Overwrite is an explicit
/// action in the confirmation sheet, never the default.
fn write_template(dir: &Path, name: &str, template: &str, capitalize: bool,
    overwrite: bool) -> Result<(), String> {
    if template.trim().is_empty() {
        return Err("template cannot be empty".to_string());
    }
    let name = name.trim();
    if name.is_empty() || name.chars().any(char::is_control) {
        return Err("template name must not be empty or contain control characters".into());
    }
    fs::create_dir_all(dir).map_err(|e| format!("failed to create templates directory: {e}"))?;
    let filename = template_filename(name);
    let path = dir.join(&filename);
    let file = TemplateFile { name: name.to_string(), template: template.to_string(), capitalize };
    let content =
        toml::to_string_pretty(&file).map_err(|e| format!("failed to serialize template: {e}"))?;
    if overwrite {
        let old = fs::read_to_string(&path)
            .map_err(|e| format!("cannot overwrite a template that is no longer saved: {e}"))?;
        let existing: TemplateFile = toml::from_str(&old)
            .map_err(|e| format!("cannot overwrite an unreadable saved template: {e}"))?;
        if existing.name != name {
            return Err("another template uses this filename".into());
        }
        fs::write(&path, content).map_err(|e| format!("failed to overwrite template: {e}"))?;
    } else {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&path)
            .map_err(|e| format!("could not create a new template (name may already exist): {e}"))?;
        if let Err(e) = file.write_all(content.as_bytes()) {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(format!("failed to write template: {e}"));
        }
    }
    Ok(())
}

pub fn save_template(target: TemplateTarget, name: &str, template: &str, capitalize: bool,
    overwrite: bool) -> Result<(), String> {
    write_template(&templates_dir(target), name, template, capitalize, overwrite)
}

pub fn delete_template(target: TemplateTarget, name: &str) -> Result<(), String> {
    let dir = templates_dir(target);
    let filename = template_filename(name);
    let path = dir.join(&filename);
    if path.exists() {
        let parsed: TemplateFile = toml::from_str(&fs::read_to_string(&path)
            .map_err(|e| format!("failed to read template: {e}"))?)
            .map_err(|e| format!("invalid template file: {e}"))?;
        if parsed.name != name { return Err("another template uses this filename".into()); }
        fs::remove_file(&path).map_err(|e| format!("failed to delete template: {e}"))?;
    }
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TemplateFile {
    name: String,
    template: String,
    #[serde(default)]
    capitalize: bool,
}

// =========================================================================
// State construction
// =========================================================================

impl TemplateBuilderState {
    pub fn new(target: TemplateTarget, initial: &str, focus: TemplateBuilderFocus) -> Self {
        let mut input = TextInputState::new(initial.to_string());
        input.cursor = input.text.len(); // cursor at end
        Self {
            template_input: input,
            capitalize: false,
            output_extension: "flac".to_string(),
            target,
            focus,
            grid_cursor: 0,
            loaded_template: None,
            save_dialog: None,
            token_scroll: 0,
            return_to_extract: None,
            disc_format: None,
        }
    }

    pub fn load_template(&mut self, template: NamingTemplate) {
        self.template_input = TextInputState::new(template.pattern.clone());
        self.template_input.cursor = self.template_input.text.len();
        self.capitalize = template.capitalize;
        self.loaded_template = Some(template);
        self.focus = TemplateBuilderFocus::TemplateInput;
    }

    pub fn begin_save(&mut self) {
        let name = self.loaded_template.as_ref()
            .filter(|old| old.pattern == self.template_input.text)
            .map(|old| old.name.as_str())
            .unwrap_or(&self.template_input.text);
        let mut input = TextInputState::new(name.to_owned());
        input.cursor = input.text.len();
        self.save_dialog = Some(TemplateSaveDialog {
            name_input: input,
            original_name: self.loaded_template.as_ref().map(|t| t.name.clone()),
            focus: TemplateSaveFocus::Name,
            error: None,
        });
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
                    insert_token(&mut self.template_input,token);
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

/// Insert a template token using the shared text-input selection semantics.
pub(super) fn insert_token(input:&mut TextInputState, token:&str) { input.insert_string(&format!("%{token}%")); }

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

/// Shared popup geometry for drawing, editor hit testing, and mouse dismissal.
/// The token region may scroll; the input, result, and footer stay fixed.
pub(crate) fn builder_popup(area: Rect, state: &TemplateBuilderState) -> Option<Rect> {
    let w = (area.width.saturating_mul(80) / 100)
        .max(60)
        .min(area.width.saturating_sub(2));
    if w < 60 || area.height < 14 { return None; }
    let tokens = builder_token_rows(state, w.saturating_sub(2)).len() as u16;
    let inner_height = 5 + tokens + 1 + 1;
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
    let top = inner.y + 5;
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

/// Keep the beginning of a constructed name and mark its clipped end.
/// The cap is measured in terminal cells, never bytes or Unicode scalars.
pub(crate) fn clip_result(value: &str, cells: usize) -> String {
    if super::display_width::width(value) <= cells { return value.to_owned(); }
    if cells <= 3 { return ".".repeat(cells); }
    let mut result = String::new();
    let mut occupied = 0;
    for ch in value.chars() {
        let width = super::display_width::char_width(ch);
        if occupied + width > cells - 3 { break; }
        result.push(ch);
        occupied += width;
    }
    result.push_str("...");
    result
}

/// Extract shows the worker's entire path when it fits. Only on overflow,
/// ask for the author's rendered name so automatic program/archive folders
/// cannot take cells away from it. A non-suffix or unavailable name falls
/// back to the previous destination-relative shortening rule.
pub(crate) fn destination_path_preview(
    path: &Path, destination: &Path, cells: usize,
    authored: impl FnOnce() -> Option<String>,
) -> String {
    let full = path.to_string_lossy();
    if super::display_width::width(&full) <= cells { return full.into_owned(); }
    let preferred = authored().and_then(|name| {
        let suffix = Path::new(&name);
        if suffix.as_os_str().is_empty() || suffix.is_absolute() || !path.ends_with(suffix) {
            return None;
        }
        path.ancestors().nth(suffix.components().count())
            .map(|prefix| (prefix.to_string_lossy().into_owned(), name))
    });
    let (root, name) = if let Some(preferred) = preferred { preferred } else {
        let Some(relative) = path.strip_prefix(destination).ok()
            .filter(|p| !p.as_os_str().is_empty())
        else { return clip_result(&full, cells); };
        (destination.to_string_lossy().into_owned(), relative.to_string_lossy().into_owned())
    };
    let name_width = super::display_width::width(&name);
    if name_width >= cells { return clip_result(&name, cells); }
    let remaining = cells - name_width;
    // A slash plus at least three dots are needed to usefully abbreviate the
    // prefix. Otherwise the complete constructed name stands alone.
    if remaining < 4 { return name; }
    let root = clip_result(&root, remaining - 1);
    format!("{}/{}", root.trim_end_matches('/'), name)
}

pub(crate) fn draw_naming_preview(f: &mut Frame, area: Rect,
    name: &str, style: Style) {
    if area.width == 0 || area.height == 0 { return; }
    let visible = clip_result(name, area.width.saturating_sub(2) as usize);
    f.render_widget(Paragraph::new(format!("→ {visible}")).style(style), area);
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
    let body = Rect::new(popup.x,popup.y+1,popup.width,popup.height.saturating_sub(1));
    let block = Block::default().borders(Borders::LEFT|Borders::RIGHT|Borders::BOTTOM)
        .border_style(Style::default().fg(theme.title));
    let inner = block.inner(body);
    f.render_widget(block,body);
    f.render_widget(Paragraph::new(super::draw_browse::browse_title_bar_line(
        popup.width as usize,&format!("▸{}",title),"",theme.title,theme,theme.panel_bg,None)),
        Rect::new(popup.x,popup.y,popup.width,1));
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
        (_, true) => ("to folder and file names", if state.capitalize { "☑" } else { "☐" }),
        (TemplateTarget::Folder, false) => ("to folder names", if state.capitalize { "☑" } else { "☐" }),
        (TemplateTarget::Filename, false) => ("to folder and file names", if state.capitalize { "☑" } else { "☐" }),
    };
    let caption = format!("  {marker} Apply TonePoet capitalization {scope}");
    f.render_widget(Paragraph::new(caption).style(Style::default()
        .fg(if focus_cap { theme.amber } else { theme.value })
        .add_modifier(if focus_cap { Modifier::BOLD } else { Modifier::empty() })),
        Rect::new(inner.x, cap_y, inner.width, 1));
    button_map.record_button(TuiButton::TemplateBuilderCapitalization,
        Rect::new(inner.x, cap_y, inner.width, 1));

    let template = &state.template_input.text;
    let example = state.return_to_extract.as_deref().and_then(|browser| {
        browser.native_preview_naming_for_template_case(template,state.capitalize)
    }).unwrap_or_else(|| render_template_preview_with_case_and_extension(
        template, state.capitalize, &state.output_extension));
    // Exactly one empty row separates the tick from the resolved name.
    draw_naming_preview(f, Rect::new(inner.x+2,inner.y+3,inner.width.saturating_sub(2),1),
        &example,Style::default().fg(theme.text_bright));

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

    // All four footer actions stay reachable while the token palette scrolls.
    let footer_y = inner.y + inner.height - 1;
    let pills: &[(&str, TuiButton, Color)] = &[
        ("apply", TuiButton::TemplateBuilderApply, theme.green),
        ("save", TuiButton::TemplateBuilderSave, theme.blue),
        ("saved templates ▾",TuiButton::TemplateBuilderSavedTemplates,theme.blue),
        ("clear", TuiButton::TemplateBuilderClear, theme.purple),
    ];
    let total_w: u16 = pills.iter().map(|(l, _, _)| super::display_width::width(l) as u16 + 2).sum::<u16>()
        + (pills.len().saturating_sub(1) as u16);
    // Center the complete footer, including Esc close. Centering only the
    // pills clips the final e at 80-column terminals.
    let close_label = "Esc close";
    let left_pad = inner.width.saturating_sub(total_w + 2 + close_label.len() as u16) / 2;
    let mut x = inner.x + left_pad;
    let mut spans = vec![Span::raw(" ".repeat(left_pad as usize))];
    for (index, (label, button, color)) in pills.iter().enumerate() {
        if index > 0 { spans.push(Span::raw(" ")); x += 1; }
        let width = super::display_width::width(label) as u16 + 2;
        spans.push(Span::styled(format!(" {} ", label), Style::default()
            .fg(if state.focus == TemplateBuilderFocus::Footer(index) { theme.bg } else { theme.pill_active_fg })
            .bg(if state.focus == TemplateBuilderFocus::Footer(index) { theme.amber } else { *color })
            .add_modifier(Modifier::BOLD)));
        button_map.record_button(*button, Rect::new(x, footer_y, width, 1));
        x += width;
    }
    spans.push(Span::raw("  "));
    spans.push(Span::styled(close_label, theme.muted()));
    f.render_widget(Paragraph::new(Line::from(spans)),
        Rect::new(inner.x, footer_y, inner.width, 1));

    if let Some(dialog) = state.save_dialog.as_ref() {
        draw_save_dialog(f,dialog,button_map,theme,popup);
    }
}

/// A smaller, modal sheet for a named save. The proposed new name is editable
/// even when an existing template was loaded; overwrite and new are separate
/// affirmative buttons, with no implicit overwrite from the name field.
fn draw_save_dialog(f: &mut Frame, dialog: &TemplateSaveDialog,
    buttons: &mut ButtonRenderMap, theme: super::theme::Theme, parent: Rect) {
    let width = parent.width.saturating_sub(6).min(76);
    let area = Rect::new(parent.x + (parent.width-width)/2,
        parent.y + parent.height.saturating_sub(8)/2,width,8);
    f.render_widget(Clear,area);
    f.render_widget(Block::default().style(Style::default().bg(theme.panel_bg)),area);
    let body = Rect::new(area.x,area.y+1,area.width,area.height-1);
    let block = Block::default().borders(Borders::LEFT|Borders::RIGHT|Borders::BOTTOM)
        .border_style(Style::default().fg(theme.title));
    let inner = block.inner(body);
    f.render_widget(block,body);
    f.render_widget(Paragraph::new(super::draw_browse::browse_title_bar_line(
        area.width as usize,"▸ Save naming template ","",theme.title,theme,theme.panel_bg,None)),
        Rect::new(area.x,area.y,area.width,1));

    let label = "  Name: ";
    let field_width = inner.width.saturating_sub(label.len() as u16 + 2) as usize;
    let focused = dialog.focus == TemplateSaveFocus::Name;
    let input_style = Style::default().fg(theme.text_bright)
        .bg(if focused {theme.input_focused_bg} else {theme.input_unfocused_bg});
    let mut spans = vec![Span::styled(label,theme.muted())];
    spans.extend(super::inline_edit::render_text_input_value_with_style(
        &dialog.name_input,field_width,focused,input_style,theme));
    f.render_widget(Paragraph::new(Line::from(spans)),
        Rect::new(inner.x,inner.y,inner.width,1));
    buttons.record_button(TuiButton::TemplateSaveName,Rect::new(
        inner.x+label.len() as u16,inner.y,inner.width.saturating_sub(label.len() as u16),1));
    if focused {
        f.set_cursor(inner.x+label.len() as u16+
            super::inline_edit::inline_cursor_col(&dialog.name_input,field_width),inner.y);
    }
    let explanation = if let Some(name) = dialog.original_name.as_deref() {
        format!("  Loaded: {name}")
    } else { "  Save the current pattern under a new name".to_owned() };
    f.render_widget(Paragraph::new(super::display_width::truncate_right(
        &explanation,inner.width as usize)).style(theme.muted()),
        Rect::new(inner.x,inner.y+2,inner.width,1));
    if let Some(error) = dialog.error.as_deref() {
        f.render_widget(Paragraph::new(super::display_width::truncate_right(
            error,inner.width.saturating_sub(2) as usize))
            .style(Style::default().fg(theme.error)),
            Rect::new(inner.x+2,inner.y+3,inner.width.saturating_sub(2),1));
    }
    let mut actions = Vec::new();
    if dialog.original_name.is_some() {
        actions.push(("overwrite",TuiButton::TemplateSaveOverwrite,TemplateSaveFocus::Overwrite,theme.destructive));
    }
    actions.push(("save as new",TuiButton::TemplateSaveNew,TemplateSaveFocus::SaveNew,theme.green));
    actions.push(("cancel",TuiButton::TemplateSaveCancel,TemplateSaveFocus::Cancel,theme.purple));
    let total_width: u16 = actions.iter().map(|(name,_,_,_)|name.len() as u16+2)
        .sum::<u16>() + actions.len().saturating_sub(1) as u16;
    let left = inner.width.saturating_sub(total_width)/2;
    let mut x = inner.x+left;
    let y = inner.y+inner.height-1;
    let mut spans = vec![Span::raw(" ".repeat(left as usize))];
    for (index,(name,button,focus,color)) in actions.into_iter().enumerate() {
        if index>0 {spans.push(Span::raw(" "));x+=1;}
        let active = dialog.focus == focus;
        let width = name.len() as u16+2;
        spans.push(Span::styled(format!(" {name} "),Style::default()
            .fg(if active {theme.bg} else {theme.pill_active_fg})
            .bg(if active {theme.amber} else {color}).add_modifier(Modifier::BOLD)));
        buttons.record_button(button,Rect::new(x,y,width,1));
        x+=width;
    }
    f.render_widget(Paragraph::new(Line::from(spans)),Rect::new(inner.x,y,inner.width,1));
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
        TemplateTarget::Folder => "▸ Load folder template ",
        TemplateTarget::Filename => "▸ Load filename template ",
    };
    let body = Rect::new(outer.x,outer.y+1,outer.width,outer.height.saturating_sub(1));
    f.render_widget(Block::default().borders(Borders::LEFT|Borders::RIGHT|Borders::BOTTOM)
        .border_style(Style::default().fg(theme.title)),body);
    f.render_widget(Paragraph::new(super::draw_browse::browse_title_bar_line(
        outer.width as usize,title_label,"",theme.title,theme,theme.panel_bg,None)),
        Rect::new(outer.x,outer.y,outer.width,1));

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
            spans.push(Span::styled(super::display_width::truncate_right(&tmpl.name, width), style));
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
    fn capitalization_toggle_and_saved_button_have_distinct_hit_targets() {
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state = TemplateBuilderState::new(TemplateTarget::Filename,
            "%ALBUM_ARTIST%/%TRACKNN% - %TITLE%", TemplateBuilderFocus::Capitalization);
        let mut buttons = ButtonRenderMap::new();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw_template_builder(frame, &state, &mut buttons, theme)).unwrap();
        let toggle = buttons.button_rect(TuiButton::TemplateBuilderCapitalization)
            .expect("capitalization toggle should be visible");
        assert_eq!(buttons.find_button_at(toggle.x + 2, toggle.y),
            Some(TuiButton::TemplateBuilderCapitalization));
        assert_eq!(terminal.backend().buffer().get(toggle.x + 2, toggle.y).symbol(), "☐");
        assert_eq!(buttons.find_button_at(toggle.x + 8, toggle.y),
            Some(TuiButton::TemplateBuilderCapitalization),
            "capitalization caption remains clickable");
        state.capitalize = true;
        terminal.draw(|frame| draw_template_builder(frame, &state, &mut buttons, theme)).unwrap();
        assert_eq!(terminal.backend().buffer().get(toggle.x + 2, toggle.y).symbol(), "☑");
        let saved = buttons.button_rect(TuiButton::TemplateBuilderSavedTemplates).unwrap();
        assert_eq!(buttons.find_button_at(saved.x + 2, saved.y),
            Some(TuiButton::TemplateBuilderSavedTemplates));
        assert_eq!(terminal.backend().buffer().get(saved.x + 1, saved.y).symbol(), "s");
    }

    #[test]
    fn capitalization_setting_round_trips_in_saved_template_format() {
        let input = TemplateFile { name: "First (ALTERNATE)".into(),
            template: "%ALBUM%/%TITLE%".into(), capitalize: true };
        let data = toml::to_string(&input).unwrap();
        let restored: TemplateFile = toml::from_str(&data).unwrap();
        assert_eq!(restored.template, input.template);
        assert_eq!(restored.name, input.name);
        assert!(restored.capitalize);
        assert!(toml::from_str::<TemplateFile>("template = '%TITLE%'").is_err());
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
                // Both the extraction extension and the unchanged ordinary
                // Convert palette use the same viewport and button map.
                for native in [false,true] {
                    let mut state = TemplateBuilderState::new(TemplateTarget::Filename,
                        "%TRACKNN% - %TITLE%",TemplateBuilderFocus::TemplateInput);
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
                            (TuiButton::TemplateBuilderSavedTemplates,"s"),
                            (TuiButton::TemplateBuilderClear,"c"),
                        ] {
                            let rect=buttons.button_rect(button).expect("fixed footer action");
                            assert_eq!(rect.y,popup.y+popup.height-2,
                                "{width}x{height}, native={native}");
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
                        "all Convert and native tokens must be reachable by mouse at {width}x{height}, native={native}");
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
        let saved = NamingTemplate {name: "Favorite".into(),pattern: filename.into(), capitalize: true};
        assert_eq!(preview_for_context_with_extension(&saved, None, selected_extension), selected,
            "saved-template picker and Custom must use the same preview");
    }

    #[test]
    fn round_2_destination_preview_keeps_constructed_names_before_the_root() {
        let destination=Path::new("/home/user/Music/Extracted/A Very Long Destination");
        let path=destination.join("Björk - Debut/01 - Human Behaviour.dsf");
        let full=path.to_string_lossy();
        let suffix="Björk - Debut/01 - Human Behaviour.dsf";
        let full_width=super::super::display_width::width(&full);
        assert_eq!(destination_path_preview(&path,destination,full_width,|| None),full.as_ref());
        let width=super::super::display_width::width(suffix)+15;
        let abbreviated=destination_path_preview(&path,destination,width,|| None);
        assert!(abbreviated.ends_with(suffix),"{abbreviated}");
        assert!(abbreviated.contains(".../"),"{abbreviated}");
        assert!(super::super::display_width::width(&abbreviated)<=width);
        assert_eq!(destination_path_preview(&path,destination,
            super::super::display_width::width(suffix),|| None),suffix);
        let clipped=destination_path_preview(&path,destination,22,|| None);
        assert!(clipped.starts_with("Björk - Debut/"),"{clipped}");
        assert!(clipped.ends_with("..."),"{clipped}");
        assert!(super::super::display_width::width(&clipped)<=22);
    }

    #[test]
    fn round_2_implicit_and_archive_prefixes_do_not_displace_authored_names() {
        use super::super::display_width::width;
        let destination = Path::new("/home/user/Music/A Very Long Destination");
        let authored = "Wish You Were Here/01 - Shine On You Crazy Diamond.dsf";
        let worker_path = destination.join("Multichannel").join(authored);
        let cells = width(authored) + 6;
        assert!(width(&worker_path.to_string_lossy()) > cells);
        assert!(width(&worker_path.strip_prefix(destination).unwrap().to_string_lossy()) > cells);

        // The author-built part is a suffix of the actual worker output,
        // not an invented filename. Routing components yield their cells.
        let abbreviated = destination_path_preview(&worker_path, destination, cells,
            || Some(authored.into()));
        assert!(abbreviated.ends_with(authored), "{abbreviated}");
        assert!(abbreviated.contains(".../"), "{abbreviated}");
        assert!(width(&abbreviated) <= cells);

        // A complete worker path never triggers the second renderer.
        let full = worker_path.to_string_lossy();
        assert_eq!(destination_path_preview(&worker_path, destination, width(&full),
            || panic!("full paths must not render the authored name")), full.as_ref());

        // When the authored portion cannot fit, keep its start and clip the end.
        let clipped = destination_path_preview(&worker_path, destination, 23,
            || Some(authored.into()));
        assert!(clipped.starts_with("Wish You Were Here/"), "{clipped}");
        assert!(clipped.ends_with("..."), "{clipped}");
        assert!(width(&clipped) <= 23);

        // Archives keep their original parents in the published path but
        // those parents do not displace the renamed, authored portion.
        let archive_name = "07/From embedded tags.mp3";
        let archive_path = destination.join("Disc 1").join(archive_name);
        let archive_cells = width(archive_name) + 5;
        let archive_preview = destination_path_preview(&archive_path, destination, archive_cells,
            || Some(archive_name.into()));
        assert!(archive_preview.ends_with(archive_name), "{archive_preview}");
        assert!(archive_preview.contains(".../"), "{archive_preview}");
        assert_eq!(archive_path, destination.join("Disc 1/07/From embedded tags.mp3"));

        // When an authored preview is missing or not an actual path suffix,
        // use the original destination-relative display rule unchanged.
        let flat = destination.join("Multichannel - 01 - Track.dsf");
        let fallback = destination_path_preview(&flat, destination, 20, || None);
        assert_eq!(destination_path_preview(&flat, destination, 20,
            || Some("01 - Track.dsf".into())), fallback);
    }

    #[test]
    fn round_2_builder_rows_and_title_are_stable_at_standard_sizes() {
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        for (width,height) in [(80,24),(100,30),(160,40)] {
            let mut state=TemplateBuilderState::new(TemplateTarget::Filename,
                "%ALBUM%/%TRACKNN% - %TITLE%",TemplateBuilderFocus::TemplateInput);
            state.capitalize=true;
            let area=Rect::new(0,0,width,height);
            let popup=builder_popup(area,&state).unwrap();
            let mut buttons=ButtonRenderMap::new();
            let mut terminal=Terminal::new(TestBackend::new(width,height)).unwrap();
            terminal.draw(|f|draw_template_builder(f,&state,&mut buttons,theme)).unwrap();
            let b=terminal.backend().buffer();
            assert_eq!(b.get(popup.x,popup.y).symbol(),"┌");
            assert_eq!(b.get(popup.x+1,popup.y).bg,theme.title);
            assert_eq!(b.get(popup.x+popup.width-1,popup.y).symbol(),"┐");
            let blank_y=popup.y+3;
            let blank=(popup.x+1..popup.x+popup.width-1)
                .map(|x|b.get(x,blank_y).symbol().to_owned()).collect::<Vec<_>>().join("");
            assert!(blank.trim().is_empty(),"one blank row between tick and result");
            let preview_y=popup.y+4;
            let shown=(popup.x+1..popup.x+popup.width-1)
                .map(|x|b.get(x,preview_y).symbol().to_owned()).collect::<Vec<_>>().join("");
            assert!(shown.contains("→ ") && shown.contains("Wish"),"{shown:?}");
            assert_eq!(b.get(popup.x+3, preview_y).symbol(), "→");
            assert!(shown.starts_with("  → "), "preview left indent: {shown:?}");
            let footer_y = popup.y + popup.height - 2;
            let footer = (popup.x+1..popup.x+popup.width-1)
                .map(|x| b.get(x, footer_y).symbol().to_owned())
                .collect::<Vec<_>>().join("");
            assert!(footer.contains("Esc close"), "footer at {width} columns: {footer:?}");
            assert!(!shown.contains("/home/"));
            assert!(buttons.button_rect(TuiButton::TemplateBuilderSavedTemplates).is_some());
            assert!(builder_token_viewport(area,&state).unwrap().y>preview_y);
        }
    }

    #[test]
    fn round_2_named_saves_require_explicit_overwrite_and_keep_alternates() {
        let directory=tempfile::tempdir().unwrap();
        let pattern="%ALBUM%/%TRACKNN% - %TITLE%";
        write_template(directory.path(),"Original",pattern,true,false).unwrap();
        assert!(write_template(directory.path(),"Original","%TITLE%",false,false).is_err());
        let file=directory.path().join(template_filename("Original"));
        let original:TemplateFile=toml::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(original.template,pattern);
        assert!(original.capitalize);
        write_template(directory.path(),"Original","%TITLE%",false,true).unwrap();
        write_template(directory.path(),"Original (ALTERNATE)",pattern,true,false).unwrap();
        let alternate:TemplateFile=toml::from_str(&fs::read_to_string(
            directory.path().join(template_filename("Original (ALTERNATE)"))).unwrap()).unwrap();
        assert_eq!(alternate.name,"Original (ALTERNATE)");
        assert_eq!(alternate.template,pattern);
        assert!(alternate.capitalize);
        assert_eq!(toml::from_str::<TemplateFile>(&fs::read_to_string(&file).unwrap())
            .unwrap().template,"%TITLE%");
    }

    #[test]
    fn round_2_loaded_save_dialog_offers_both_actions() {
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut state=TemplateBuilderState::new(TemplateTarget::Filename,
            "",TemplateBuilderFocus::TemplateInput);
        state.load_template(NamingTemplate {
            name:"First".into(),pattern:"%ALBUM%/%TITLE%".into(),capitalize:true,
        });
        state.begin_save();
        let mut terminal=Terminal::new(TestBackend::new(80,24)).unwrap();
        let mut buttons=ButtonRenderMap::new();
        terminal.draw(|f|draw_template_builder(f,&state,&mut buttons,theme)).unwrap();
        for button in [TuiButton::TemplateSaveName,TuiButton::TemplateSaveOverwrite,
            TuiButton::TemplateSaveNew,TuiButton::TemplateSaveCancel] {
            let rect=buttons.button_rect(button).unwrap();
            assert_eq!(buttons.find_button_at(rect.x+1,rect.y),Some(button));
        }
        assert_eq!(state.save_dialog.as_ref().unwrap().name_input.text,"First");
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
