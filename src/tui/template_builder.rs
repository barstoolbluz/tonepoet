//! Template builder overlay for composing folder/filename templates.
//!
//! The overlay presents a categorized grid of clickable token pills and
//! an editable template line. Clicking a token inserts `%TOKEN%` at the
//! cursor position. Templates can be saved to and loaded from disk.

use std::fs;
use std::path::PathBuf;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::app::{TemplateBuilderFocus, TemplateBuilderState, TemplateTarget};
use super::button_map::{ButtonRenderMap, TuiButton};
use super::text_input::TextInputState;
use super::theme;

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

pub fn list_templates(target: TemplateTarget) -> Vec<String> {
    let dir = templates_dir(target);
    let mut templates = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("toml") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(parsed) = toml::from_str::<TemplateFile>(&content) {
                        if !parsed.template.trim().is_empty() {
                            templates.push(parsed.template);
                        }
                    }
                }
            }
        }
    }
    templates.sort();
    templates.dedup();
    templates
}

pub fn save_template(target: TemplateTarget, template: &str) -> Result<(), String> {
    if template.trim().is_empty() {
        return Err("template cannot be empty".to_string());
    }
    let dir = templates_dir(target);
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create templates directory: {e}"))?;
    let filename = template_filename(template);
    let path = dir.join(&filename);
    let file = TemplateFile {
        template: template.to_string(),
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
            target,
            focus,
            grid_cursor: 0,
            saved_templates,
            saved_selected: 0,
            saved_scroll: 0,
        }
    }

    pub fn token_categories(&self) -> &'static [TokenCategory] {
        match self.target {
            TemplateTarget::Folder => FOLDER_TOKENS,
            TemplateTarget::Filename => FILENAME_TOKENS,
        }
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

pub fn draw_template_builder(
    f: &mut Frame,
    state: &TemplateBuilderState,
    button_map: &mut ButtonRenderMap,
) {
    let area = f.size();
    let w = (area.width * 80 / 100)
        .max(60)
        .min(area.width.saturating_sub(2));
    let categories = state.token_categories();
    // Calculate height: title(1) + template input(2) + blank(1) + saved section + categories + separators + footer
    let saved_visible = state.saved_templates.len().min(4).max(1);
    let category_rows: u16 = categories.iter().map(|_| 2).sum::<u16>(); // label + tokens per category
    let content_height = 1 + 2 + 1 + 1 + saved_visible as u16 + 1 + category_rows + 2 + 1 + 1;
    let h = (content_height + 2).min(area.height.saturating_sub(2)); // +2 for top/bottom borders
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let popup = Rect::new(x, y, w, h);

    f.render_widget(Clear, popup);

    let title = match state.target {
        TemplateTarget::Folder => " Build folder template ",
        TemplateTarget::Filename => " Build filename template ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::AMBER))
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme::AMBER)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    if inner.width < 40 || inner.height < 10 {
        return;
    }

    let mut cy = inner.y;
    let iw = inner.width as usize;

    // ── Template input line ──
    {
        let label = Span::styled("  Template:  ", theme::muted());
        let (visible_text, cursor_col) = state.template_input.view(iw.saturating_sub(14));
        let input_style = if state.focus == TemplateBuilderFocus::TemplateInput {
            Style::default().fg(Color::White).bg(Color::Rgb(40, 40, 40))
        } else {
            Style::default().fg(Color::White).bg(Color::Rgb(30, 30, 30))
        };
        let input_span = Span::styled(
            format!("{:width$}", visible_text, width = iw.saturating_sub(14)),
            input_style,
        );
        let line = Line::from(vec![label, input_span]);
        f.render_widget(Paragraph::new(line), Rect::new(inner.x, cy, inner.width, 1));
        if state.focus == TemplateBuilderFocus::TemplateInput {
            f.set_cursor(inner.x + 13 + cursor_col as u16, cy);
        }
        cy += 2;
    }

    // ── Saved templates ──
    {
        let header = Line::from(Span::styled(
            "  ── Saved templates ──",
            Style::default()
                .fg(theme::TEXT_DIM)
                .add_modifier(Modifier::BOLD),
        ));
        f.render_widget(
            Paragraph::new(header),
            Rect::new(inner.x, cy, inner.width, 1),
        );
        cy += 1;

        if state.saved_templates.is_empty() {
            let empty = Line::from(Span::styled(
                "    (none saved)",
                Style::default().fg(theme::TEXT_DIM),
            ));
            f.render_widget(
                Paragraph::new(empty),
                Rect::new(inner.x, cy, inner.width, 1),
            );
            cy += 1;
        } else {
            let max_visible = saved_visible;
            let start = state.saved_scroll;
            let end = (start + max_visible).min(state.saved_templates.len());
            for (vis_idx, idx) in (start..end).enumerate() {
                let tmpl = &state.saved_templates[idx];
                let is_selected =
                    state.focus == TemplateBuilderFocus::SavedList && idx == state.saved_selected;
                let marker = if is_selected { "  ▸ " } else { "    " };
                let style = if is_selected {
                    Style::default().fg(theme::AMBER)
                } else {
                    Style::default().fg(theme::TEXT)
                };
                let display: String = if tmpl.len() > iw.saturating_sub(8) {
                    let trunc: String = tmpl.chars().take(iw.saturating_sub(11)).collect();
                    format!("{}...", trunc)
                } else {
                    tmpl.clone()
                };
                let row_y = cy + vis_idx as u16;
                let line = Line::from(vec![
                    Span::styled(marker, style),
                    Span::styled(display, style),
                ]);
                f.render_widget(
                    Paragraph::new(line),
                    Rect::new(inner.x, row_y, inner.width, 1),
                );
                button_map.record_button(
                    TuiButton::TemplateBuilderSavedItem(idx),
                    Rect::new(inner.x, row_y, inner.width, 1),
                );
            }
            cy += max_visible as u16;
        }
        cy += 1;
    }

    // ── Token categories ──
    let mut grid_idx = 0_usize;
    for cat in categories {
        // Category header
        let header = Line::from(Span::styled(
            format!("  ── {} ──", cat.label),
            Style::default()
                .fg(theme::TEXT_DIM)
                .add_modifier(Modifier::BOLD),
        ));
        if cy < inner.y + inner.height {
            f.render_widget(
                Paragraph::new(header),
                Rect::new(inner.x, cy, inner.width, 1),
            );
        }
        cy += 1;

        // Token pills
        if cy < inner.y + inner.height {
            let mut px = inner.x + 2;
            for &token in cat.tokens {
                let label = format!(" {} ", token);
                let pill_w = label.len() as u16;
                if px + pill_w + 2 > inner.x + inner.width {
                    // Would overflow — skip (shouldn't happen on reasonable terminals)
                    grid_idx += 1;
                    continue;
                }
                let is_highlighted =
                    state.focus == TemplateBuilderFocus::TokenGrid && grid_idx == state.grid_cursor;
                let style = if is_highlighted {
                    Style::default()
                        .fg(theme::BG)
                        .bg(theme::AMBER)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(theme::PILL_ACTIVE_FG)
                        .bg(theme::BLUE)
                        .add_modifier(Modifier::BOLD)
                };
                let pill = Span::styled(label, style);
                f.render_widget(
                    Paragraph::new(Line::from(pill)),
                    Rect::new(px, cy, pill_w, 1),
                );
                button_map.record_button(
                    TuiButton::TemplateBuilderToken(grid_idx),
                    Rect::new(px, cy, pill_w, 1),
                );
                px += pill_w + 2;
                grid_idx += 1;
            }
        }
        cy += 1;
    }

    // ── Separators ──
    if cy < inner.y + inner.height {
        let header = Line::from(Span::styled(
            "  ── Separators ──",
            Style::default()
                .fg(theme::TEXT_DIM)
                .add_modifier(Modifier::BOLD),
        ));
        f.render_widget(
            Paragraph::new(header),
            Rect::new(inner.x, cy, inner.width, 1),
        );
        cy += 1;
    }
    if cy < inner.y + inner.height {
        let mut px = inner.x + 2;
        for &(label, _) in SEPARATORS {
            let display = format!(" {} ", label);
            let pill_w = display.len() as u16;
            if px + pill_w + 2 > inner.x + inner.width {
                grid_idx += 1;
                continue;
            }
            let is_highlighted =
                state.focus == TemplateBuilderFocus::TokenGrid && grid_idx == state.grid_cursor;
            let style = if is_highlighted {
                Style::default()
                    .fg(theme::BG)
                    .bg(theme::AMBER)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(theme::PILL_ACTIVE_FG)
                    .bg(theme::PURPLE)
                    .add_modifier(Modifier::BOLD)
            };
            let pill = Span::styled(display, style);
            f.render_widget(
                Paragraph::new(Line::from(pill)),
                Rect::new(px, cy, pill_w, 1),
            );
            button_map.record_button(
                TuiButton::TemplateBuilderToken(grid_idx),
                Rect::new(px, cy, pill_w, 1),
            );
            px += pill_w + 2;
            grid_idx += 1;
        }
        cy += 1;
    }

    // ── Footer pills ──
    cy += 1;
    if cy < inner.y + inner.height {
        let pills: &[(&str, TuiButton, Color)] = &[
            ("apply", TuiButton::TemplateBuilderApply, theme::GREEN),
            ("save", TuiButton::TemplateBuilderSave, theme::BLUE),
            ("clear", TuiButton::TemplateBuilderClear, theme::PURPLE),
            ("x delete", TuiButton::TemplateBuilderDelete, theme::RED),
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
                    .fg(theme::PILL_ACTIVE_FG)
                    .bg(*color)
                    .add_modifier(Modifier::BOLD),
            );
            button_map.record_button(*btn, Rect::new(px, cy, pill_w, 1));
            spans.push(pill);
            px += pill_w;
        }

        // Esc close (not a button — just a hint)
        spans.push(Span::raw("  "));
        spans.push(Span::styled("Esc close", theme::muted()));

        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(inner.x, cy, inner.width, 1),
        );
    }
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
    const ALBUM_FULL: &str = "Wish You Were Here (Japan CBS-Sony 35DP-4)";
    const ALBUM_CLEAN: &str = "Wish You Were Here";
    const TITLE_EXTRA: &str = " (Japan CBS-Sony 35DP-4)";

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
    s = s.replace("%LABEL%", "CBS/Sony");
    s = s.replace("%COUNTRY%", "JP");
    s = s.replace("%PRESSING%", "CBS/Sony");
    s
}

/// Draw the template picker overlay — a list of saved templates with preview.
pub fn draw_template_picker(
    f: &mut Frame,
    target: TemplateTarget,
    templates: &[String],
    selected: usize,
    scroll: usize,
    preview: &str,
    active_template: Option<&str>,
    button_map: &mut ButtonRenderMap,
) {
    use super::theme;

    let area = f.size();
    let w = (area.width * 75 / 100).max(50).min(area.width.saturating_sub(2));
    let list_rows = templates.len().max(1) as u16;
    // header(1) + blank(1) + list + blank(1) + preview label(1) + preview(1) + blank(1) + hint(1)
    let content_h = 2 + 1 + list_rows + 1 + 1 + 1 + 1 + 1;
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
        .border_style(Style::default().fg(theme::CYAN))
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
            theme::muted(),
        )));
        f.render_widget(empty, Rect::new(inner.x + 1, cy, inner.width.saturating_sub(2), 1));
        cy += 1;
    } else {
        let end = (scroll + visible_rows).min(templates.len());
        for (i, tmpl) in templates[scroll..end].iter().enumerate() {
            let idx = scroll + i;
            let is_selected = idx == selected;
            let is_active = active_template.map_or(false, |a| a == tmpl);

            let mut spans = Vec::new();

            // Selection indicator
            if is_selected {
                spans.push(Span::styled("▸ ", Style::default().fg(theme::CYAN)));
            } else {
                spans.push(Span::raw("  "));
            }

            // Template text
            let style = if is_selected {
                Style::default().fg(theme::TEXT_BRIGHT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::TEXT)
            };
            spans.push(Span::styled(tmpl.clone(), style));

            // Active badge
            if is_active {
                spans.push(Span::styled(" (active)", Style::default().fg(theme::GREEN)));
            }

            let row_rect = Rect::new(inner.x, cy, inner.width, 1);
            f.render_widget(Paragraph::new(Line::from(spans)), row_rect);
            button_map.record_button(TuiButton::TemplatePickerRow(idx), row_rect);
            cy += 1;
        }
    }

    // Preview section at the bottom
    let preview_y = inner.y + inner.height.saturating_sub(3);
    if preview_y > cy {
        cy = preview_y;
    }

    // Preview label
    f.render_widget(
        Paragraph::new(Line::from(Span::styled("Preview:", theme::muted()))),
        Rect::new(inner.x + 1, cy, inner.width.saturating_sub(2), 1),
    );
    cy += 1;

    // Preview value
    let preview_style = Style::default().fg(theme::TEXT_BRIGHT);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(preview, preview_style))),
        Rect::new(inner.x + 2, cy, inner.width.saturating_sub(4), 1),
    );
    cy += 1;

    // Hint line
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("Enter", Style::default().fg(theme::CYAN)),
            Span::styled(" apply  ", theme::muted()),
            Span::styled("x", Style::default().fg(theme::RED)),
            Span::styled(" delete  ", theme::muted()),
            Span::styled("Esc", Style::default().fg(theme::CYAN)),
            Span::styled(" close", theme::muted()),
        ])),
        Rect::new(inner.x + 1, cy, inner.width.saturating_sub(2), 1),
    );
}
