use crate::state::{
    intersect_rect, ConflictPolicyPreset, DeleteConfirmButton, FilePickerCreateKind, FilePickerFocus,
    FilePickerHitAction, FilePickerMenuAction, FilePickerSelectionMode, FilePickerState, HitRegion,
    ToolbarAction,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;
use std::time::{Duration, SystemTime};


#[cfg(feature = "image-preview")]
struct ImageRenderContext<'a> {
    picker: Option<&'a mut ratatui_image::picker::Picker>,
    protocol_generation: usize,
}

#[cfg(feature = "image-preview")]
impl<'a> ImageRenderContext<'a> {
    fn none() -> Self {
        Self { picker: None, protocol_generation: 0 }
    }

    fn with_picker(
        picker: &'a mut ratatui_image::picker::Picker,
        protocol_generation: usize,
    ) -> Self {
        Self { picker: Some(picker), protocol_generation }
    }
}

#[cfg(not(feature = "image-preview"))]
struct ImageRenderContext<'a> {
    _phantom: std::marker::PhantomData<&'a mut ()>,
}

#[cfg(not(feature = "image-preview"))]
impl<'a> ImageRenderContext<'a> {
    fn none() -> Self {
        Self { _phantom: std::marker::PhantomData }
    }
}

impl FilePickerState {
    /// Render the picker into `area` and refresh internal hit-test regions.
    ///
    /// Hosts that dispatch mouse events should either pass this same `area` to
    /// `handle_mouse` or pass `Rect::default()` to use this last-rendered area.
    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        self.render_with_image_context(frame, area, ImageRenderContext::none());
    }

    /// Render with a host-owned terminal image picker.
    ///
    /// The picker should be detected once by the host at startup. Increment
    /// `protocol_generation` whenever terminal resize/cell-size changes require
    /// cached StatefulProtocol values to be re-encoded.
    #[cfg(feature = "image-preview")]
    pub fn render_with_image_picker(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        image_picker: &mut ratatui_image::picker::Picker,
        protocol_generation: usize,
    ) {
        self.render_with_image_picker_and_repaint_generation(
            frame,
            area,
            image_picker,
            protocol_generation,
            0,
        );
    }

    /// Render with a host-owned terminal image picker and image repaint generation.
    ///
    /// Deprecated compatibility wrapper for callers that still thread a repaint
    /// generation through render. Render no longer mutates hidden cell style
    /// metadata to force image repaint. Ghostty/Kitty hosts should instead pass
    /// a rate-limited retransmit generation to
    /// `prepare_image_preview_protocol_with_retransmit_generation`.
    #[cfg(feature = "image-preview")]
    pub fn render_with_image_picker_and_repaint_generation(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        image_picker: &mut ratatui_image::picker::Picker,
        protocol_generation: usize,
        _repaint_generation: usize,
    ) {
        self.render_with_image_context(
            frame,
            area,
            ImageRenderContext::with_picker(image_picker, protocol_generation),
        );
    }

    fn render_with_image_context(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        mut image_ctx: ImageRenderContext<'_>,
    ) {
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

        let toolbar_area;
        if self.conflict_policy.is_some() {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Min(3),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ])
                .split(inner);

            toolbar_area = rows[0];
            self.render_toolbar(frame, rows[0]);
            self.render_address(frame, rows[1]);
            self.render_split_pane(frame, rows[2], &mut image_ctx);
            self.render_conflict_policy_row(frame, rows[3]);
            self.render_status(frame, rows[4]);
        } else {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Min(3),
                    Constraint::Length(1),
                ])
                .split(inner);

            toolbar_area = rows[0];
            self.render_toolbar(frame, rows[0]);
            self.render_address(frame, rows[1]);
            self.render_split_pane(frame, rows[2], &mut image_ctx);
            self.render_status(frame, rows[3]);
        }

        if self.menu_open {
            self.render_file_operations_menu(frame, toolbar_area);
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
        let toolbar_right = area.x.saturating_add(area.width);
        let mut buttons = vec![
            ("‹ Back".to_string(), Some(ToolbarAction::Back), self.history_back.is_empty()),
            ("› Forward".to_string(), Some(ToolbarAction::Forward), self.history_forward.is_empty()),
            ("↑ Up".to_string(), Some(ToolbarAction::Up), self.current_dir.parent().is_none()),
            ("│".to_string(), None, true),
            (
                (if self.menu_open { "File Operations ▴" } else { "File Operations ▾" }).to_string(),
                Some(ToolbarAction::FileOperations),
                false,
            ),
            ("Properties".to_string(), Some(ToolbarAction::Properties), self.current_selection().is_none()),
        ];
        if self.selection_mode == FilePickerSelectionMode::Directories {
            buttons.push(("Select Folder".to_string(), Some(ToolbarAction::AcceptSelection), false));
        }
        for (idx, (label, action, disabled)) in buttons.iter().enumerate() {
            if idx > 0 {
                spans.push(Span::raw("  "));
                x = x.saturating_add(2);
            }

            if action.is_none() {
                let width = label.chars().count() as u16;
                spans.push(Span::styled(label.clone(), self.theme.border_dim));
                x = x.saturating_add(width);
                continue;
            }

            let width = button_width(label);
            let style = if *disabled {
                self.theme.button_disabled
            } else if *action == Some(ToolbarAction::FileOperations) && self.menu_open {
                self.theme.button_focused
            } else {
                self.theme.button
            };
            spans.push(button_span(label, style));
            let raw_rect = Rect::new(x, area.y, width, 1);
            if let Some(visible_rect) = intersect_rect(raw_rect, area) {
                self.record_toolbar_button_geometry((*action).unwrap(), visible_rect);
                if !*disabled {
                    self.record_hit_region(visible_rect, FilePickerHitAction::Toolbar((*action).unwrap()));
                }
            }
            x = x.saturating_add(width);
            if x >= toolbar_right {
                break;
            }
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn render_address(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let go_label = "Go";
        let go_width = button_width(go_label);
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
            button_span(go_label, self.theme.button),
        ]);
        frame.render_widget(Paragraph::new(line), area);
        let _ = self.record_hit_region_clipped(
            Rect::new(input_x, area.y, input_width, 1),
            area,
            FilePickerHitAction::Address,
        );
        let go_rect = Rect::new(area.x.saturating_add(area.width.saturating_sub(go_width)), area.y, go_width, 1);
        if let Some(visible_rect) = self.record_hit_region_clipped(
            go_rect,
            area,
            FilePickerHitAction::Toolbar(ToolbarAction::Go),
        ) {
            self.record_toolbar_button_geometry(ToolbarAction::Go, visible_rect);
        }
    }

    fn render_split_pane(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        image_ctx: &mut ImageRenderContext<'_>,
    ) {
        if self.preview_pane_enabled(area) {
            let panes = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(24),
                    Constraint::Percentage(52),
                    Constraint::Percentage(24),
                ])
                .split(area);
            self.render_tree(frame, panes[0]);
            self.render_file_table(frame, panes[1]);
            self.render_preview_pane(frame, panes[2], image_ctx);
        } else {
            let panes = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(28), Constraint::Percentage(72)])
                .split(area);
            self.render_tree(frame, panes[0]);
            self.render_file_table(frame, panes[1]);
        }
    }

    #[cfg(feature = "image-preview")]
    fn preview_pane_enabled(&self, area: Rect) -> bool {
        self.show_preview && area.width >= 76 && area.height >= 6
    }

    #[cfg(not(feature = "image-preview"))]
    fn preview_pane_enabled(&self, _area: Rect) -> bool {
        false
    }

    /// Render the image preview pane.
    ///
    /// **Known limitation:** On terminals using the Kitty graphics protocol
    /// (Ghostty, Kitty, WezTerm), mouse movement can cause the image to
    /// disappear. The terminal redraws its text layer over the graphics layer,
    /// and ratatui's buffer diff does not detect the damage. Keyboard-only
    /// navigation does not have this problem. Multiple approaches were
    /// attempted (style-key repaint invalidation, per-row cell dirtying,
    /// Kitty-specific rate-limited protocol retransmission) without success.
    /// This remains an open issue with ratatui-image's Kitty protocol
    /// integration.
    #[cfg(feature = "image-preview")]
    fn render_preview_pane(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        image_ctx: &mut ImageRenderContext<'_>,
    ) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border_dim)
            .title("Preview");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Some(entry) = self.current_selection().cloned() else {
            render_preview_message(frame, inner, "No selection", self.theme.text_dim);
            return;
        };

        if entry.is_dir {
            render_preview_message(frame, inner, "Folder", self.theme.text_dim);
            return;
        }
        if !is_previewable_image_path(&entry.path) {
            render_preview_message(frame, inner, "No image preview", self.theme.text_dim);
            return;
        }

        if self.image_preview_cache.path.as_ref() != Some(&entry.path) {
            self.request_image_preview_load(entry.path.clone());
        }

        // Record desired geometry/generation for prepare_image_preview_protocol.
        // These fields intentionally do not claim that the cached protocol was
        // already encoded for this geometry or host generation.
        self.image_preview_cache.desired_preview_area = Some(inner);
        self.image_preview_cache.desired_protocol_generation = image_ctx.protocol_generation;

        if image_ctx.picker.is_none() {
            self.image_preview_cache.error = Some(
                "Preview unavailable: host did not provide terminal image picker".to_string(),
            );
        }

        if self.image_preview_cache.protocol.is_some() {
            if let Some(protocol) = self.image_preview_cache.protocol.as_mut() {
                let image = ratatui_image::StatefulImage::new(None);
                frame.render_stateful_widget(image, inner, protocol);
            }
        } else {
            let message = if self.image_preview_cache.receiver.is_some() {
                "Loading preview…"
            } else if self.image_preview_cache.decoded_image.is_some() {
                "Preparing preview…"
            } else {
                self.image_preview_cache
                    .error
                    .as_deref()
                    .unwrap_or("Preview unavailable")
            };
            render_preview_message(frame, inner, message, self.theme.text_dim);
        }
    }


    #[cfg(not(feature = "image-preview"))]
    fn render_preview_pane(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        _image_ctx: &mut ImageRenderContext<'_>,
    ) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border_dim)
            .title("Preview");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        render_preview_message(frame, inner, "Preview feature disabled", self.theme.text_dim);
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
            } else if self.selection_mode == FilePickerSelectionMode::Directories && !entry.is_dir {
                self.theme.menu_disabled
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


    fn render_conflict_policy_row(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let Some(selected_policy) = self.conflict_policy else {
            return;
        };
        let label = "If exists:";
        let mut spans = Vec::new();
        let mut x = area.x;
        spans.push(Span::styled(label.to_string(), self.theme.label));
        x = x.saturating_add(label.chars().count() as u16);
        spans.push(Span::raw("  "));
        x = x.saturating_add(2);

        for (idx, (policy, button_label)) in [
            (ConflictPolicyPreset::Ask, "Ask"),
            (ConflictPolicyPreset::Overwrite, "Overwrite"),
            (ConflictPolicyPreset::Skip, "Skip"),
        ]
        .iter()
        .copied()
        .enumerate()
        {
            if idx > 0 {
                spans.push(Span::raw("  "));
                x = x.saturating_add(2);
            }
            let style = if policy == selected_policy {
                self.theme.button_focused
            } else {
                self.theme.button
            };
            spans.push(button_span(button_label, style));
            let width = button_width(button_label);
            let rect = Rect::new(x, area.y, width, 1);
            let _ = self.record_hit_region_clipped(
                rect,
                area,
                FilePickerHitAction::ConflictPolicy(policy),
            );
            x = x.saturating_add(width);
        }

        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn render_status(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let count = self.entries.len();
        let total = format_size(self.visible_total_size());
        let error = self.status_error_message();
        let left = format!("{count} {}", pluralize(count, "item", "items"));
        let center = error.unwrap_or_else(|| format!("{total} visible"));
        let right = match self.free_space_bytes {
            Some(bytes) => format!("{} free", format_size(bytes)),
            None => "free space unavailable".to_string(),
        };
        let width = area.width as usize;
        let line = distribute_status(&left, &center, &right, width);
        let style = if self.last_error.is_some() { self.theme.error } else { self.theme.status };
        frame.render_widget(Paragraph::new(line).style(style), area);
    }

    fn render_file_operations_menu(&mut self, frame: &mut Frame<'_>, toolbar_area: Rect) {
        let menu_width = 15;
        let menu_height = 7;
        let toolbar_right = toolbar_area.x.saturating_add(toolbar_area.width);
        let fallback_anchor = Rect::new(toolbar_area.x, toolbar_area.y, 1, 1);
        let anchor = self
            .toolbar_button_rect(ToolbarAction::FileOperations)
            .unwrap_or(fallback_anchor);
        let menu_x = anchor
            .x
            .min(toolbar_right.saturating_sub(menu_width))
            .max(toolbar_area.x);
        let menu_area = Rect::new(menu_x, toolbar_area.y.saturating_add(1), menu_width, menu_height);
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
            let hot_style = if disabled {
                self.theme.menu_disabled
            } else if selected {
                self.theme.menu_selected
            } else {
                self.theme.accelerator
            };
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
            let submenu_width = 10;
            let submenu_height = 4;
            let bounds = self.last_rendered_area().unwrap_or(toolbar_area);
            let bounds_right = bounds.x.saturating_add(bounds.width);
            let submenu_right_x = menu_area.x.saturating_add(menu_area.width);
            let submenu_x = if submenu_right_x.saturating_add(submenu_width) <= bounds_right {
                submenu_right_x
            } else {
                menu_area.x.saturating_sub(submenu_width).max(bounds.x)
            };
            let submenu_area = Rect::new(submenu_x, menu_area.y.saturating_add(1), submenu_width, submenu_height);
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
                let hot_style = if disabled {
                    self.theme.menu_disabled
                } else if selected {
                    self.theme.menu_selected
                } else {
                    self.theme.accelerator
                };
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
            .border_style(self.theme.border)
            .title(" Confirm ");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let target = self.pending_delete.as_deref();
        let file_name = target
            .and_then(|path| path.file_name())
            .map(|name| clean_display_text(&name.to_string_lossy()))
            .unwrap_or_else(|| "selected item".to_string());
        let path_text = target
            .map(|path| clean_display_text(&path.display().to_string()))
            .unwrap_or_else(|| "No pending delete".to_string());
        let message = format!("Delete \"{file_name}\"?");
        let text_width = inner.width.saturating_sub(4) as usize;
        let path_width = text_width.saturating_sub(2);
        let message_line = indent_text(&message, 2, text_width);
        let path_line = format!("  {}", fit_text_right(&path_text, path_width));

        let delete_label = "Delete";
        let cancel_label = "Cancel";
        let delete_width = button_width(delete_label);
        let cancel_width = button_width(cancel_label);
        let gap = 4u16;
        let total_width = delete_width.saturating_add(gap).saturating_add(cancel_width);
        let button_x = inner.x.saturating_add(inner.width.saturating_sub(total_width) / 2);
        let button_y = inner.y.saturating_add(inner.height.saturating_sub(1));
        let text_area = Rect::new(inner.x, inner.y, inner.width, button_y.saturating_sub(inner.y));
        let mut lines = Vec::new();
        if text_area.height >= 4 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(message_line, self.theme.text)));
        if text_area.height >= 3 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(path_line, self.theme.text_dim)));
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), text_area);

        let delete_style = if self.delete_confirm_button == DeleteConfirmButton::Delete {
            self.theme.button_focused
        } else {
            self.theme.button
        };
        let cancel_style = if self.delete_confirm_button == DeleteConfirmButton::Cancel {
            self.theme.button_focused
        } else {
            self.theme.button
        };
        let line = Line::from(vec![
            button_span(delete_label, delete_style),
            Span::raw(" ".repeat(gap as usize)),
            button_span(cancel_label, cancel_style),
        ]);
        frame.render_widget(Paragraph::new(line), Rect::new(button_x, button_y, total_width, 1));
        self.record_hit_region(Rect::new(button_x, button_y, delete_width, 1), FilePickerHitAction::DeleteConfirm);
        self.record_hit_region(
            Rect::new(button_x.saturating_add(delete_width).saturating_add(gap), button_y, cancel_width, 1),
            FilePickerHitAction::DeleteCancel,
        );
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

fn button_width(label: &str) -> u16 {
    label.chars().count().saturating_add(2) as u16
}

fn button_span(label: &str, style: ratatui::style::Style) -> Span<'static> {
    Span::styled(format!(" {label} "), style)
}

fn clean_display_text(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
}

fn indent_text(text: &str, indent: usize, width: usize) -> String {
    let available = width.saturating_sub(indent);
    format!("{}{}", " ".repeat(indent), fit_text_left(text, available))
}

fn pluralize(count: usize, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 { singular } else { plural }
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

fn menu_line(label: &str, width: usize, style: ratatui::style::Style, _hot_style: ratatui::style::Style) -> Line<'static> {
    let padded = fit_text_left(label, width);
    Line::from(Span::styled(padded, style))
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

    let leading = [left.trim(), center.trim()]
        .into_iter()
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("  |  ");
    let right = right.trim();
    if right.is_empty() {
        return fit_text_left(&leading, width);
    }

    let leading_len = leading.chars().count();
    let right_len = right.chars().count();
    let minimum_gap = 3;
    if leading_len + minimum_gap + right_len > width {
        return fit_text_left(&format!("{leading}  |  {right}"), width);
    }

    let gap = width - leading_len - right_len;
    let spacer = " ".repeat(gap);
    format!("{leading}{spacer}{right}")
}

fn render_preview_message(frame: &mut Frame<'_>, area: Rect, message: &str, style: ratatui::style::Style) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let line = fit_text_left(message, area.width as usize);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(line, style))).alignment(ratatui::layout::Alignment::Center),
        area,
    );
}

#[cfg(feature = "image-preview")]
fn is_previewable_image_path(path: &std::path::Path) -> bool {
    crate::filter::is_supported_preview_image_extension(path)
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
    use crate::{ConflictPolicyPreset, FilePickerConfig, FilePickerFilter};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::{Color, Modifier, Style};
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
    fn conflict_policy_row_renders_and_registers_hit_regions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            selection_mode: FilePickerSelectionMode::Directories,
            conflict_policy: Some(ConflictPolicyPreset::Ask),
            ..FilePickerConfig::default()
        });
        let backend = TestBackend::new(90, 22);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| picker.render(frame, Rect::new(0, 0, 80, 18)))
            .expect("draw");
        let rendered = format!("{:?}", terminal.backend().buffer());

        assert!(rendered.contains("If exists:"));
        assert!(rendered.contains("Overwrite"));
        assert!(picker.hit_regions().iter().any(|hit| {
            matches!(hit.action, FilePickerHitAction::ConflictPolicy(ConflictPolicyPreset::Overwrite))
        }));
    }



    #[test]
    fn set_theme_changes_rendered_styles_on_next_draw() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("cover.png"), b"png").expect("file");
        let mut first_theme = crate::FilePickerTheme::default();
        first_theme.border = Style::default().fg(Color::Rgb(1, 2, 3));
        first_theme.title = Style::default().fg(Color::Rgb(4, 5, 6));
        first_theme.selected = Style::default()
            .fg(Color::Rgb(7, 8, 9))
            .bg(Color::Rgb(10, 11, 12))
            .add_modifier(Modifier::BOLD);
        let mut second_theme = first_theme.clone();
        second_theme.border = Style::default().fg(Color::Rgb(21, 22, 23));
        second_theme.title = Style::default().fg(Color::Rgb(24, 25, 26));
        second_theme.selected = Style::default()
            .fg(Color::Rgb(27, 28, 29))
            .bg(Color::Rgb(30, 31, 32))
            .add_modifier(Modifier::BOLD);

        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: FilePickerFilter::Images,
            theme: first_theme,
            ..FilePickerConfig::default()
        });
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let area = Rect::new(0, 0, 90, 24);

        terminal.draw(|frame| picker.render(frame, area)).expect("first draw");
        assert_eq!(terminal.backend().buffer().get(0, 0).fg, Color::Rgb(1, 2, 3));

        picker.set_theme(second_theme);
        terminal.draw(|frame| picker.render(frame, area)).expect("second draw");
        assert_eq!(terminal.backend().buffer().get(0, 0).fg, Color::Rgb(21, 22, 23));
    }

    #[test]
    fn generic_picker_omits_conflict_policy_row() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            selection_mode: FilePickerSelectionMode::Directories,
            conflict_policy: None,
            ..FilePickerConfig::default()
        });
        let backend = TestBackend::new(90, 22);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| picker.render(frame, Rect::new(0, 0, 80, 18)))
            .expect("draw");
        let rendered = format!("{:?}", terminal.backend().buffer());

        assert!(!rendered.contains("If exists:"));
        assert!(!picker.hit_regions().iter().any(|hit| {
            matches!(hit.action, FilePickerHitAction::ConflictPolicy(_))
        }));
    }

    #[test]
    fn toolbar_hit_regions_are_clipped_to_the_visible_toolbar_area() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("cover.png"), b"png").expect("file");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: FilePickerFilter::Images,
            ..FilePickerConfig::default()
        });
        let backend = TestBackend::new(54, 16);
        let mut terminal = Terminal::new(backend).expect("terminal");
        let area = Rect::new(0, 0, 48, 12);
        terminal.draw(|frame| picker.render(frame, area)).expect("draw");

        let toolbar_area = Rect::new(1, 1, 46, 1);
        for hit in picker.hit_regions().iter() {
            match hit.action {
                FilePickerHitAction::Toolbar(ToolbarAction::Go) => {}
                FilePickerHitAction::Toolbar(_) => {
                    assert!(rect_contains_rect(toolbar_area, hit.rect), "toolbar hit not clipped: {:?}", hit);
                }
                _ => {}
            }
        }

        let file_ops = picker
            .toolbar_button_rect(ToolbarAction::FileOperations)
            .expect("visible file operations geometry");
        assert!(
            rect_contains_rect(toolbar_area, file_ops),
            "toolbar geometry not clipped: {:?}",
            file_ops
        );
    }

    #[test]
    fn directory_mode_renders_select_folder_toolbar_button() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            selection_mode: FilePickerSelectionMode::Directories,
            ..FilePickerConfig::default()
        });
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| picker.render(frame, Rect::new(0, 0, 96, 18))).expect("draw");

        assert!(
            picker.toolbar_button_rect(ToolbarAction::AcceptSelection).is_some(),
            "directory picker should expose an explicit Select Folder button"
        );
    }

    #[test]
    fn file_operations_menu_anchors_to_the_recorded_button_geometry() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("cover.png"), b"png").expect("file");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: FilePickerFilter::Images,
            ..FilePickerConfig::default()
        });
        picker.menu_open = true;
        picker.focus = FilePickerFocus::Menu;

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| picker.render(frame, Rect::new(0, 0, 96, 24)))
            .expect("draw");

        let file_ops = picker
            .toolbar_button_rect(ToolbarAction::FileOperations)
            .expect("file operations button geometry");
        let menu_new = picker
            .hit_regions()
            .iter()
            .find(|hit| matches!(hit.action, FilePickerHitAction::MenuNew))
            .expect("menu item hit region");

        assert_eq!(menu_new.rect.x, file_ops.x.saturating_add(1));
        assert_eq!(menu_new.rect.y, file_ops.y.saturating_add(2));
    }

    #[test]
    fn toolbar_buttons_apply_buffer_cell_background_styles() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("cover.png"), b"png").expect("file");
        let mut theme = crate::FilePickerTheme::default();
        theme.button = Style::default().fg(Color::White).bg(Color::Blue);
        theme.button_focused = Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD);
        theme.button_disabled = Style::default().fg(Color::DarkGray).bg(Color::Red);

        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: FilePickerFilter::Images,
            theme,
            ..FilePickerConfig::default()
        });
        picker.menu_open = true;
        picker.focus = FilePickerFocus::Menu;

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| picker.render(frame, Rect::new(0, 0, 96, 24)))
            .expect("draw");
        let buffer = terminal.backend().buffer();

        let back = picker.toolbar_button_rect(ToolbarAction::Back).expect("back geometry");
        let up = picker.toolbar_button_rect(ToolbarAction::Up).expect("up geometry");
        let file_ops = picker
            .toolbar_button_rect(ToolbarAction::FileOperations)
            .expect("file operations geometry");

        assert_eq!(buffer.get(back.x, back.y).bg, Color::Red);
        assert_eq!(buffer.get(up.x, up.y).bg, Color::Blue);
        assert_eq!(buffer.get(file_ops.x, file_ops.y).bg, Color::Green);
        assert!(buffer.get(file_ops.x, file_ops.y).modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn delete_confirmation_buttons_apply_normal_and_focused_styles() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("front 2.jpg");
        fs::write(&file, b"jpg").expect("file");
        let mut theme = crate::FilePickerTheme::default();
        theme.button = Style::default().fg(Color::White).bg(Color::Blue);
        theme.button_focused = Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD);

        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: FilePickerFilter::Images,
            theme,
            ..FilePickerConfig::default()
        });
        let index = picker.entries().iter().position(|entry| entry.path == file).expect("file visible");
        picker.set_file_cursor(index, 4);
        assert!(picker.request_delete_current());

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| picker.render(frame, Rect::new(0, 0, 96, 24)))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let delete = find_text(buffer, " Delete ", 100, 30).expect("delete button text");
        let cancel = find_text(buffer, " Cancel ", 100, 30).expect("cancel button text");

        assert_eq!(buffer.get(delete.0, delete.1).bg, Color::Blue);
        assert_eq!(buffer.get(cancel.0, cancel.1).bg, Color::Green);
        assert!(buffer.get(cancel.0, cancel.1).modifier.contains(Modifier::BOLD));
    }


    #[test]
    fn status_bar_uses_separators_between_segments() {
        let line = distribute_status("3 items", "12.0 KB visible", "3401.7 GB free", 80);
        assert!(line.contains("3 items  |  12.0 KB visible"));
        assert!(line.contains("3401.7 GB free"));
        assert!(!line.contains("items12"));
    }

    #[test]
    fn delete_confirmation_renders_clean_copy_without_status_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("front 2.jpg");
        fs::write(&file, b"jpg").expect("file");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: FilePickerFilter::Images,
            ..FilePickerConfig::default()
        });
        let index = picker.entries().iter().position(|entry| entry.path == file).expect("file visible");
        picker.set_file_cursor(index, 4);
        assert!(picker.request_delete_current());

        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| picker.render(frame, Rect::new(0, 0, 96, 24)))
            .expect("draw");
        let rendered = format!("{:?}", terminal.backend().buffer());

        assert!(rendered.contains("Confirm"));
        assert!(rendered.contains("Delete \"front 2.jpg\"?"));
        assert!(rendered.contains("Delete"));
        assert!(rendered.contains("Cancel"));
        assert!(!rendered.contains("configured delete policy"));
        assert!(!rendered.contains("failed for"));
        assert!(picker.last_error().is_none());
        assert!(picker
            .hit_regions()
            .iter()
            .any(|hit| matches!(hit.action, FilePickerHitAction::DeleteConfirm)));
        assert!(picker
            .hit_regions()
            .iter()
            .any(|hit| matches!(hit.action, FilePickerHitAction::DeleteCancel)));
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

    fn rect_contains_rect(outer: Rect, inner: Rect) -> bool {
        inner.x >= outer.x
            && inner.y >= outer.y
            && inner.x.saturating_add(inner.width) <= outer.x.saturating_add(outer.width)
            && inner.y.saturating_add(inner.height) <= outer.y.saturating_add(outer.height)
    }

    fn find_text(buffer: &Buffer, text: &str, width: u16, height: u16) -> Option<(u16, u16)> {
        let symbols = text.chars().map(|ch| ch.to_string()).collect::<Vec<_>>();
        let text_width = u16::try_from(symbols.len()).ok()?;
        if text_width == 0 || text_width > width {
            return None;
        }
        for y in 0..height {
            for x in 0..=width.saturating_sub(text_width) {
                if symbols
                    .iter()
                    .enumerate()
                    .all(|(offset, symbol)| buffer.get(x.saturating_add(offset as u16), y).symbol() == symbol.as_str())
                {
                    return Some((x, y));
                }
            }
        }
        None
    }
}
