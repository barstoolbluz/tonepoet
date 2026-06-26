use crate::state::{
    FilePickerCreateKind, FilePickerFocus, FilePickerHitAction, FilePickerMenuAction, FilePickerState,
    HitRegion, ToolbarAction,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;
use std::time::{Duration, SystemTime};

impl FilePickerState {
    /// Render the picker into `area` and refresh internal hit-test regions.
    ///
    /// Hosts that dispatch mouse events should either pass this same `area` to
    /// `handle_mouse` or pass `Rect::default()` to use this last-rendered area.
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        self.clear_hit_regions();
        self.set_last_area(area);
        if area.width < 48 || area.height < 10 {
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new("File picker needs at least 48x10 cells").style(self.theme.error),
                area,
            );
            return;
        }

        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title(Span::styled(self.title.clone(), self.theme.title));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(3),
                Constraint::Length(1),
            ])
            .split(inner);

        self.render_toolbar(frame, rows[0]);
        self.render_address(frame, rows[1]);
        self.render_split_pane(frame, rows[2]);
        self.render_status(frame, rows[3]);

        if self.menu_open {
            self.render_file_operations_menu(frame, rows[0]);
        }
        if self.properties_open {
            self.render_properties_popup(frame, area);
        }
        if self.focus == FilePickerFocus::DeleteConfirm {
            self.render_delete_confirm_popup(frame, area);
        }
        if self.focus == FilePickerFocus::CreateName {
            self.render_create_name_popup(frame, area);
        }
    }

    fn render_toolbar(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let mut spans = Vec::new();
        let mut x = area.x;
        let buttons = [
            ("‹ Back", ToolbarAction::Back, self.history_back.is_empty()),
            ("› Forward", ToolbarAction::Forward, self.history_forward.is_empty()),
            ("↑ Up", ToolbarAction::Up, self.current_dir.parent().is_none()),
            ("│", ToolbarAction::Up, true),
            (
                if self.menu_open { "File Operations ▴" } else { "File Operations ▾" },
                ToolbarAction::FileOperations,
                false,
            ),
            ("Properties", ToolbarAction::Properties, self.current_selection().is_none()),
        ];
        for (idx, (label, action, disabled)) in buttons.iter().enumerate() {
            if idx > 0 {
                spans.push(Span::raw("  "));
                x = x.saturating_add(2);
            }
            let width = label.chars().count() as u16;
            let style = if *disabled {
                self.theme.text_dim
            } else if *action == ToolbarAction::FileOperations && self.menu_open {
                self.theme.toolbar_active
            } else {
                self.theme.toolbar
            };
            spans.push(Span::styled((*label).to_string(), style));
            if !*disabled && *label != "│" {
                self.record_hit_region(Rect::new(x, area.y, width, 1), FilePickerHitAction::Toolbar(*action));
            }
            x = x.saturating_add(width);
            if x >= area.x.saturating_add(area.width) {
                break;
            }
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn render_address(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let go_width = 6u16;
        let label = "Address:";
        let input_x = area.x.saturating_add(label.chars().count() as u16 + 2);
        let input_width = area
            .width
            .saturating_sub(label.chars().count() as u16)
            .saturating_sub(go_width)
            .saturating_sub(3);
        let input = if self.address_editing {
            text_with_caret(&self.address_buffer, self.address_cursor, input_width as usize)
        } else {
            fit_text_left(&self.address_buffer, input_width as usize)
        };
        let line = Line::from(vec![
            Span::styled(label.to_string(), self.theme.label),
            Span::raw("  "),
            Span::styled(input, self.theme.text),
            Span::raw(" "),
            Span::styled("[ Go ]", self.theme.toolbar),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        self.record_hit_region(Rect::new(input_x, area.y, input_width, 1), FilePickerHitAction::Address);
        self.record_hit_region(
            Rect::new(area.x.saturating_add(area.width.saturating_sub(go_width)), area.y, go_width, 1),
            FilePickerHitAction::Toolbar(ToolbarAction::Go),
        );
    }

    fn render_split_pane(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(28), Constraint::Percentage(72)])
            .split(area);
        self.render_tree(frame, panes[0]);
        self.render_file_table(frame, panes[1]);
    }

    fn render_tree(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(if self.focus == FilePickerFocus::Tree { self.theme.border } else { self.theme.border_dim })
            .title("Folders");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let visible_rows = inner.height as usize;
        self.set_tree_visible_rows(visible_rows);
        self.ensure_tree_cursor_visible(visible_rows);
        let mut lines = Vec::new();
        let mut hits = Vec::new();
        for (row, idx) in (self.tree_scroll..self.tree_nodes.len())
            .take(visible_rows)
            .enumerate()
        {
            let node = &self.tree_nodes[idx];
            let marker = if node.has_children {
                if node.expanded { "▾" } else { "▸" }
            } else {
                " "
            };
            let indent = "  ".repeat(node.depth);
            let label = fit_text_right(&format!("{indent}{marker} {}", node.name), inner.width as usize);
            let style = if idx == self.tree_cursor && self.focus == FilePickerFocus::Tree {
                self.theme.selected
            } else if same_display_path(&node.path, &self.current_dir) {
                self.theme.title
            } else {
                self.theme.text
            };
            lines.push(Line::from(Span::styled(label, style)));
            hits.push(HitRegion {
                rect: Rect::new(inner.x, inner.y.saturating_add(row as u16), inner.width, 1),
                action: FilePickerHitAction::TreeRow(idx),
            });
        }
        self.hit_regions.extend(hits);
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_file_table(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(if self.focus == FilePickerFocus::Files { self.theme.border } else { self.theme.border_dim });
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let body_capacity = inner.height.saturating_sub(1) as usize;
        self.set_file_visible_rows(body_capacity);
        self.ensure_file_cursor_visible(body_capacity);
        let header = Row::new(vec!["Name", "Size", "Type", "Modified"])
            .style(self.theme.header)
            .bottom_margin(0);
        let mut rows = Vec::new();
        let mut hits = Vec::new();
        for (row, idx) in (self.file_scroll..self.entries.len())
            .take(body_capacity)
            .enumerate()
        {
            let entry = &self.entries[idx];
            let style = if idx == self.file_cursor && self.focus == FilePickerFocus::Files {
                self.theme.selected
            } else if entry.is_dir {
                self.theme.folder
            } else {
                self.theme.text
            };
            rows.push(
                Row::new(vec![
                    Cell::from(entry.name.clone()),
                    Cell::from(entry.size.map(format_size).unwrap_or_else(|| "--".to_string())),
                    Cell::from(entry.file_type.clone()),
                    Cell::from(entry.modified.map(format_modified).unwrap_or_else(|| "--".to_string())),
                ])
                .style(style),
            );
            hits.push(HitRegion {
                rect: Rect::new(inner.x, inner.y.saturating_add(1).saturating_add(row as u16), inner.width, 1),
                action: FilePickerHitAction::FileRow(idx),
            });
        }
        self.hit_regions.extend(hits);

        let widths = [
            Constraint::Percentage(42),
            Constraint::Percentage(14),
            Constraint::Percentage(22),
            Constraint::Percentage(22),
        ];
        let selected = if body_capacity > 0 && self.file_cursor >= self.file_scroll {
            Some(self.file_cursor.saturating_sub(self.file_scroll))
        } else {
            None
        };
        self.file_table_state = TableState::default();
        self.file_table_state.select(selected);
        let table = Table::new(rows, widths)
            .header(header)
            .highlight_style(self.theme.selected)
            .highlight_symbol(" ");
        frame.render_stateful_widget(table, inner, &mut self.file_table_state);
    }

    fn render_status(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let count = self.entries.len();
        let total = format_size(self.visible_total_size());
        let error = self.error_message();
        let left = format!(" {count} items");
        let center = error.unwrap_or_else(|| format!("{total} visible"));
        let right = match self.free_space_bytes {
            Some(bytes) => format!("{} free ", format_size(bytes)),
            None => "free space unavailable ".to_string(),
        };
        let width = area.width as usize;
        let line = distribute_status(&left, &center, &right, width);
        let style = if self.last_error.is_some() { self.theme.error } else { self.theme.status };
        frame.render_widget(Paragraph::new(line).style(style), area);
    }

    fn render_file_operations_menu(&mut self, frame: &mut Frame<'_>, toolbar_area: Rect) {
        let menu_x = toolbar_area
            .x
            .saturating_add(28)
            .min(toolbar_area.x.saturating_add(toolbar_area.width.saturating_sub(16)));
        let menu_area = Rect::new(menu_x, toolbar_area.y.saturating_add(1), 15, 7);
        frame.render_widget(Clear, menu_area);
        let block = Block::default().borders(Borders::ALL).border_style(self.theme.border_dim);
        let inner = block.inner(menu_area);
        frame.render_widget(block, menu_area);
        let items = [
            ("New      ▸", None),
            ("Cut", Some(FilePickerMenuAction::Cut)),
            ("Copy", Some(FilePickerMenuAction::Copy)),
            ("Paste", Some(FilePickerMenuAction::Paste)),
            ("Delete", Some(FilePickerMenuAction::Delete)),
        ];
        let mut lines = Vec::new();
        let mut hits = Vec::new();
        for (idx, (label, action)) in items.iter().enumerate() {
            let selected = self.focus == FilePickerFocus::Menu && self.menu_cursor == idx;
            let disabled = action
                .map(|action| !self.is_menu_action_enabled(action))
                .unwrap_or_else(|| !self.is_new_menu_enabled());
            let style = if disabled {
                self.theme.menu_disabled
            } else if selected {
                self.theme.menu_selected
            } else {
                self.theme.menu
            };
            let hot_style = if disabled { self.theme.menu_disabled } else { self.theme.destructive };
            lines.push(menu_line(label, inner.width as usize, style, hot_style));
            let hit_action = match action {
                Some(action) => FilePickerHitAction::Menu(*action),
                None => FilePickerHitAction::MenuNew,
            };
            if !disabled {
                hits.push(HitRegion {
                    rect: Rect::new(inner.x, inner.y.saturating_add(idx as u16), inner.width, 1),
                    action: hit_action,
                });
            }
        }
        self.hit_regions.extend(hits);
        frame.render_widget(Paragraph::new(lines), inner);

        if self.submenu_open {
            let submenu_area = Rect::new(menu_area.x.saturating_add(menu_area.width), menu_area.y.saturating_add(1), 10, 4);
            frame.render_widget(Clear, submenu_area);
            let block = Block::default().borders(Borders::ALL).border_style(self.theme.border_dim);
            let inner = block.inner(submenu_area);
            frame.render_widget(block, submenu_area);
            let items = [("File", FilePickerMenuAction::NewFile), ("Folder", FilePickerMenuAction::NewFolder)];
            let mut lines = Vec::new();
            let mut hits = Vec::new();
            for (idx, (label, action)) in items.iter().enumerate() {
                let selected = self.focus == FilePickerFocus::Submenu && self.submenu_cursor == idx;
                let disabled = !self.is_menu_action_enabled(*action);
                let style = if disabled {
                    self.theme.menu_disabled
                } else if selected {
                    self.theme.menu_selected
                } else {
                    self.theme.menu
                };
                let hot_style = if disabled { self.theme.menu_disabled } else { self.theme.destructive };
                lines.push(menu_line(label, inner.width as usize, style, hot_style));
                if !disabled {
                    hits.push(HitRegion {
                        rect: Rect::new(inner.x, inner.y.saturating_add(idx as u16), inner.width, 1),
                        action: FilePickerHitAction::Submenu(*action),
                    });
                }
            }
            self.hit_regions.extend(hits);
            frame.render_widget(Paragraph::new(lines), inner);
        }
    }

    fn render_properties_popup(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered_rect(area, 60, 45);
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title("Properties");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        let lines = if let Some(entry) = self.current_selection() {
            vec![
                Line::from(vec![Span::styled("Name: ", self.theme.label), Span::raw(entry.name.clone())]),
                Line::from(vec![Span::styled("Path: ", self.theme.label), Span::raw(entry.path.display().to_string())]),
                Line::from(vec![Span::styled("Type: ", self.theme.label), Span::raw(entry.file_type.clone())]),
                Line::from(vec![Span::styled("Size: ", self.theme.label), Span::raw(entry.size.map(format_size).unwrap_or_else(|| "--".to_string()))]),
                Line::from(""),
                Line::from(Span::styled("Esc/click closes", self.theme.text_dim)),
            ]
        } else {
            vec![Line::from("No selection"), Line::from(""), Line::from(Span::styled("Esc/click closes", self.theme.text_dim))]
        };
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        self.record_hit_region(popup, FilePickerHitAction::PropertiesClose);
    }

    fn render_delete_confirm_popup(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered_rect(area, 58, 28);
        frame.render_widget(Clear, popup);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.destructive)
            .title("Confirm delete");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        let target = self
            .pending_delete
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "no pending delete".to_string());
        let lines = vec![
            Line::from(Span::styled("This permanently deletes the selected path under the configured delete policy.", self.theme.destructive)),
            Line::from(""),
            Line::from(target),
            Line::from(""),
            Line::from("[ Delete ]    [ Cancel ]"),
        ];
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        let button_y = inner.y.saturating_add(inner.height.saturating_sub(1));
        self.record_hit_region(Rect::new(inner.x, button_y, 10, 1), FilePickerHitAction::DeleteConfirm);
        self.record_hit_region(Rect::new(inner.x.saturating_add(14), button_y, 10, 1), FilePickerHitAction::DeleteCancel);
    }

    fn render_create_name_popup(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered_rect(area, 58, 26);
        frame.render_widget(Clear, popup);
        let kind = self.pending_create.unwrap_or(FilePickerCreateKind::File);
        let title = match kind {
            FilePickerCreateKind::File => "New file",
            FilePickerCreateKind::Folder => "New folder",
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title(title);
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        let input_width = inner.width.saturating_sub(8) as usize;
        let input = text_with_caret(&self.create_name_buffer, self.create_name_cursor, input_width);
        let lines = vec![
            Line::from(vec![Span::styled("Name: ", self.theme.label), Span::styled(input, self.theme.text)]),
            Line::from(""),
            Line::from(Span::styled("Enter creates; Esc cancels", self.theme.text_dim)),
        ];
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}

fn centered_rect(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let width = area.width.saturating_mul(percent_x).saturating_div(100).max(20).min(area.width);
    let height = area.height.saturating_mul(percent_y).saturating_div(100).max(6).min(area.height);
    Rect::new(
        area.x.saturating_add(area.width.saturating_sub(width) / 2),
        area.y.saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    )
}

fn fit_text_left(text: &str, width: usize) -> String {
    let mut out: String = text.chars().take(width).collect();
    let used = out.chars().count();
    if used < width {
        out.push_str(&" ".repeat(width - used));
    }
    out
}

fn fit_text_right(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return fit_text_left(text, width);
    }
    if width <= 1 {
        return "…".to_string();
    }
    let suffix: String = text.chars().rev().take(width - 1).collect::<String>().chars().rev().collect();
    format!("…{suffix}")
}

fn text_with_caret(text: &str, cursor: usize, width: usize) -> String {
    let cursor = nearest_char_boundary(text, cursor.min(text.len()));
    let mut marked = String::with_capacity(text.len() + 3);
    marked.push_str(&text[..cursor]);
    marked.push('▌');
    marked.push_str(&text[cursor..]);
    fit_text_right(&marked, width)
}

fn nearest_char_boundary(text: &str, mut cursor: usize) -> usize {
    while cursor > 0 && !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    cursor
}

fn menu_line(label: &str, width: usize, style: ratatui::style::Style, hot_style: ratatui::style::Style) -> Line<'static> {
    let padded = fit_text_left(label, width);
    let mut chars = padded.chars();
    let Some(first) = chars.next() else {
        return Line::from(Span::styled(String::new(), style));
    };
    let rest: String = chars.collect();
    Line::from(vec![Span::styled(first.to_string(), hot_style), Span::styled(rest, style)])
}

fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * KB;
    const GB: f64 = 1024.0 * MB;
    let bytes_f = bytes as f64;
    if bytes_f >= GB {
        format!("{:.1} GB", bytes_f / GB)
    } else if bytes_f >= MB {
        format!("{:.1} MB", bytes_f / MB)
    } else if bytes_f >= KB {
        format!("{:.1} KB", bytes_f / KB)
    } else {
        format!("{bytes} B")
    }
}

fn format_modified(time: SystemTime) -> String {
    match SystemTime::now().duration_since(time) {
        Ok(age) => format_age(age),
        Err(_) => "future".to_string(),
    }
}

fn format_age(age: Duration) -> String {
    let seconds = age.as_secs();
    if seconds < 60 {
        "now".to_string()
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else if seconds < 31_536_000 {
        format!("{}d ago", seconds / 86_400)
    } else {
        format!("{}y ago", seconds / 31_536_000)
    }
}

fn distribute_status(left: &str, center: &str, right: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let left = fit_text_left(left, left.chars().count());
    let right_len = right.chars().count();
    let left_len = left.chars().count();
    if left_len + right_len >= width {
        return fit_text_left(&format!("{left} {right}"), width);
    }
    let center_width = width.saturating_sub(left_len).saturating_sub(right_len);
    format!("{left}{}{}", fit_text_left(center, center_width), right)
}

fn same_display_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    if a == b {
        return true;
    }
    let a_canon = a.canonicalize();
    let b_canon = b.canonicalize();
    matches!((a_canon, b_canon), (Ok(a), Ok(b)) if a == b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FilePickerConfig, FilePickerFilter};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::fs;

    #[test]
    fn render_records_geometry_and_hit_regions() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("cover.png"), b"png").expect("file");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: FilePickerFilter::Images,
            ..FilePickerConfig::default()
        });
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| picker.render(frame, Rect::new(1, 1, 90, 24)))
            .expect("draw");
        assert_eq!(picker.last_rendered_area(), Some(Rect::new(1, 1, 90, 24)));
        assert!(picker.file_visible_rows() > 1);
        assert!(picker.hit_regions().iter().any(|hit| matches!(hit.action, FilePickerHitAction::FileRow(_))));
    }

    #[test]
    fn status_bar_reports_free_space_when_host_provides_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.set_free_space_bytes(Some(142 * 1024 * 1024 * 1024));
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| picker.render(frame, Rect::new(0, 0, 96, 18)))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("142.0 GB free"));
    }
}
