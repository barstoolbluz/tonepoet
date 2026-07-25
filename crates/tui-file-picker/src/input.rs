use crate::state::{
    DeleteConfirmButton, FilePickerAction, FilePickerContextMenuKind, FilePickerCreateKind,
    FilePickerError, FilePickerFocus, FilePickerHitAction, FilePickerMenuAction,
    FilePickerMenuEntry, FilePickerState, FilePickerSubmenuKind, LastClick, ToolbarAction,
};
use crate::text_input::{
    handle_text_input_key, handle_text_input_key_with_boundaries, TextBoundaryMode,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Debug, Clone)]
enum StableHitAction {
    Direct(FilePickerHitAction),
    TreeRow(PathBuf),
    TreeDisclosure(PathBuf),
    FileRow(PathBuf),
}

impl StableHitAction {
    fn capture(state: &FilePickerState, action: FilePickerHitAction) -> Option<Self> {
        match action {
            FilePickerHitAction::TreeRow(index) => state
                .tree_nodes
                .get(index)
                .map(|node| Self::TreeRow(node.path.clone())),
            FilePickerHitAction::TreeDisclosure(index) => state
                .tree_nodes
                .get(index)
                .map(|node| Self::TreeDisclosure(node.path.clone())),
            FilePickerHitAction::FileRow(index) => state
                .entries
                .get(index)
                .map(|entry| Self::FileRow(entry.path.clone())),
            other => Some(Self::Direct(other)),
        }
    }

    fn resolve(self, state: &FilePickerState) -> Option<FilePickerHitAction> {
        match self {
            Self::Direct(action) => Some(action),
            Self::TreeRow(path) => state
                .tree_nodes
                .iter()
                .position(|node| node.path == path)
                .map(FilePickerHitAction::TreeRow),
            Self::TreeDisclosure(path) => state
                .tree_nodes
                .iter()
                .position(|node| node.path == path)
                .map(FilePickerHitAction::TreeDisclosure),
            Self::FileRow(path) => state
                .entries
                .iter()
                .position(|entry| entry.path == path)
                .map(FilePickerHitAction::FileRow),
        }
    }
}

impl FilePickerState {
    /// Apply a keyboard event and return a terminal action for the host app.
    pub fn handle_key(&mut self, key: KeyEvent) -> FilePickerAction {
        if self.handle_paste_task_key(key) {
            return FilePickerAction::None;
        }
        self.last_click = None;
        self.tree_last_click = None;
        match self.focus {
            FilePickerFocus::Address => self.handle_address_key(key),
            FilePickerFocus::Search => self.handle_search_key(key),
            FilePickerFocus::Bookmarks => self.handle_bookmarks_key(key),
            FilePickerFocus::BookmarkName => self.handle_bookmark_name_key(key),
            FilePickerFocus::Tree => self.handle_tree_key(key),
            FilePickerFocus::Files => self.handle_file_key(key),
            FilePickerFocus::Menu => self.handle_menu_key(key),
            FilePickerFocus::Submenu => self.handle_submenu_key(key),
            FilePickerFocus::Properties => self.handle_properties_key(key),
            FilePickerFocus::DeleteConfirm => self.handle_delete_confirm_key(key),
            FilePickerFocus::CreateName => self.handle_create_name_key(key),
            FilePickerFocus::SaveName => self.handle_save_name_key(key),
            FilePickerFocus::SaveOverwriteConfirm => self.handle_save_overwrite_confirm_key(key),
        }
    }

    /// Apply a mouse event using hit regions from the most recent render pass.
    ///
    /// A non-default `area` must match the last rendered area. Passing
    /// `Rect::default()` opts into the last-rendered geometry, which is useful
    /// in host event paths that do not carry the draw area. Clicks before a
    /// render, or clicks after a resize without a fresh render, are ignored with
    /// a structured `StaleHitRegions` error instead of being dispatched against
    /// stale coordinates.
    pub fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> FilePickerAction {
        let progress_area = if area == Rect::default() {
            self.last_rendered_area().unwrap_or_default()
        } else {
            area
        };
        if self.handle_paste_task_mouse(mouse, progress_area) {
            return FilePickerAction::None;
        }

        if area != Rect::default() {
            match self.last_rendered_area() {
                Some(expected) if expected == area => {}
                Some(expected) => {
                    self.set_error(FilePickerError::StaleHitRegions { expected, received: area });
                    self.hit_regions.clear();
                    return FilePickerAction::None;
                }
                None => {
                    self.set_error(FilePickerError::StaleHitRegions {
                        expected: Rect::default(),
                        received: area,
                    });
                    self.hit_regions.clear();
                    return FilePickerAction::None;
                }
            }
        }

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.last_click = None;
                self.tree_last_click = None;
                if self.tree_focused {
                    self.move_tree_cursor(-3, self.tree_visible_rows());
                } else {
                    self.move_file_cursor(-3, self.file_visible_rows());
                }
                FilePickerAction::None
            }
            MouseEventKind::ScrollDown => {
                self.last_click = None;
                self.tree_last_click = None;
                if self.tree_focused {
                    self.move_tree_cursor(3, self.tree_visible_rows());
                } else {
                    self.move_file_cursor(3, self.file_visible_rows());
                }
                FilePickerAction::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let action = self.hit_regions.iter().rev().find_map(|region| {
                    point_in_rect(mouse.column, mouse.row, region.rect).then_some(region.action)
                });
                let action = match self.resolve_name_edit_before_pointer_action(action) {
                    Ok(action) => action,
                    Err(()) => return FilePickerAction::None,
                };
                let Some(action) = action else {
                    self.last_click = None;
                    self.tree_last_click = None;
                    self.close_menu();
                    return FilePickerAction::None;
                };
                self.apply_click_action(action, mouse.modifiers)
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.last_click = None;
                let action = self.hit_regions.iter().rev().find_map(|region| {
                    point_in_rect(mouse.column, mouse.row, region.rect).then_some(region.action)
                });
                let action = match self.resolve_name_edit_before_pointer_action(action) {
                    Ok(action) => action,
                    Err(()) => return FilePickerAction::None,
                };
                self.last_click = None;
                self.tree_last_click = None;
                self.open_context_menu(action, mouse.column, mouse.row)
            }
            _ => FilePickerAction::None,
        }
    }

    fn resolve_name_edit_before_pointer_action(
        &mut self,
        action: Option<FilePickerHitAction>,
    ) -> Result<Option<FilePickerHitAction>, ()> {
        if self.focus != FilePickerFocus::CreateName {
            return Ok(action);
        }
        if action == Some(FilePickerHitAction::CreateNameEditor) {
            self.last_click = None;
            self.tree_last_click = None;
            return Err(());
        }

        let stable_action = action.and_then(|action| StableHitAction::capture(self, action));
        // Entering or leaving an inline editor breaks any prior click sequence.
        // The outside click must be processed as a fresh selection/navigation
        // click, never as a double-click or delayed-rename continuation from
        // the interaction that opened the editor.
        self.last_click = None;
        self.tree_last_click = None;
        if !self.commit_create_name() {
            return Err(());
        }
        Ok(stable_action.and_then(|action| action.resolve(self)))
    }

    fn handle_address_key(&mut self, key: KeyEvent) -> FilePickerAction {
        match key.code {
            KeyCode::Esc => {
                self.cancel_address_edit();
                FilePickerAction::None
            }
            KeyCode::Enter => self.commit_address(),
            _ => {
                let _ = handle_text_input_key_with_boundaries(
                    &mut self.address_input,
                    &key,
                    TextBoundaryMode::PathSegment,
                );
                FilePickerAction::None
            }
        }
    }

    fn handle_tree_key(&mut self, key: KeyEvent) -> FilePickerAction {
        match key.code {
            KeyCode::Esc => FilePickerAction::Cancelled,
            KeyCode::Tab => {
                self.focus = FilePickerFocus::Files;
                self.tree_focused = false;
                FilePickerAction::None
            }
            KeyCode::Right => {
                self.tree_right();
                FilePickerAction::None
            }
            KeyCode::Up => {
                self.move_tree_cursor(-1, self.tree_visible_rows());
                FilePickerAction::None
            }
            KeyCode::Down => {
                self.move_tree_cursor(1, self.tree_visible_rows());
                FilePickerAction::None
            }
            KeyCode::Left => {
                self.tree_left();
                FilePickerAction::None
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.activate_tree_cursor();
                FilePickerAction::None
            }
            KeyCode::PageUp => {
                let rows = self.tree_visible_rows();
                self.move_tree_cursor(-(rows as isize), rows);
                FilePickerAction::None
            }
            KeyCode::PageDown => {
                let rows = self.tree_visible_rows();
                self.move_tree_cursor(rows as isize, rows);
                FilePickerAction::None
            }
            KeyCode::Home => {
                self.set_tree_cursor(0, self.tree_visible_rows());
                FilePickerAction::None
            }
            KeyCode::End => {
                let last = self.tree_nodes.len().saturating_sub(1);
                self.set_tree_cursor(last, self.tree_visible_rows());
                FilePickerAction::None
            }
            KeyCode::Backspace => {
                self.go_parent();
                FilePickerAction::None
            }
            KeyCode::Char('l') if key.modifiers == KeyModifiers::CONTROL => {
                self.begin_address_edit();
                FilePickerAction::None
            }
            KeyCode::Char('/') => {
                self.open_search();
                FilePickerAction::None
            }
            KeyCode::Char('r') if key.modifiers == KeyModifiers::CONTROL => {
                self.refresh();
                FilePickerAction::None
            }
            KeyCode::Char('f') if key.modifiers == KeyModifiers::CONTROL => {
                self.open_search();
                FilePickerAction::None
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.type_ahead_tree(c);
                FilePickerAction::None
            }
            _ => FilePickerAction::None,
        }
    }

    fn handle_file_key(&mut self, key: KeyEvent) -> FilePickerAction {
        let rows = self.file_visible_rows();
        match key.code {
            KeyCode::Esc if !self.multi_selected.is_empty() => {
                self.deselect_all();
                FilePickerAction::None
            }
            KeyCode::Esc => FilePickerAction::Cancelled,
            KeyCode::Left if key.modifiers == KeyModifiers::ALT => {
                self.go_back();
                FilePickerAction::None
            }
            KeyCode::Tab | KeyCode::Left => {
                self.focus = FilePickerFocus::Tree;
                self.tree_focused = true;
                FilePickerAction::None
            }
            KeyCode::Char('l') if key.modifiers == KeyModifiers::CONTROL => {
                self.begin_address_edit();
                FilePickerAction::None
            }
            KeyCode::Char('/') => {
                self.open_search();
                FilePickerAction::None
            }
            KeyCode::Char('f') if key.modifiers == KeyModifiers::CONTROL => {
                self.open_search();
                FilePickerAction::None
            }
            KeyCode::Backspace if self.type_ahead.is_active_at(Instant::now()) => {
                self.type_ahead.pop(Instant::now());
                self.apply_file_type_ahead();
                FilePickerAction::None
            }
            KeyCode::Backspace => {
                self.go_parent();
                FilePickerAction::None
            }
            KeyCode::Right if key.modifiers == KeyModifiers::ALT => {
                self.go_forward();
                FilePickerAction::None
            }
            KeyCode::Char('r') if key.modifiers == KeyModifiers::CONTROL => {
                self.refresh();
                FilePickerAction::None
            }
            KeyCode::Char('a') if key.modifiers == KeyModifiers::CONTROL => {
                self.select_all_visible();
                FilePickerAction::None
            }
            KeyCode::Char(' ') if key.modifiers == KeyModifiers::CONTROL => {
                self.toggle_current_multi_selection();
                FilePickerAction::None
            }
            KeyCode::Up => {
                self.move_file_cursor(-1, rows);
                FilePickerAction::None
            }
            KeyCode::Down => {
                self.move_file_cursor(1, rows);
                FilePickerAction::None
            }
            KeyCode::PageUp => {
                self.move_file_cursor(-(rows as isize), rows);
                FilePickerAction::None
            }
            KeyCode::PageDown => {
                self.move_file_cursor(rows as isize, rows);
                FilePickerAction::None
            }
            KeyCode::Home => {
                self.set_file_cursor(0, rows);
                FilePickerAction::None
            }
            KeyCode::End => {
                let last = self.entries.len().saturating_sub(1);
                self.set_file_cursor(last, rows);
                FilePickerAction::None
            }
            KeyCode::Enter => self.open_or_select_current(),
            KeyCode::Char(' ') => self.accept_current_selection(),
            KeyCode::Char('o') if key.modifiers == KeyModifiers::ALT => {
                self.open_menu();
                FilePickerAction::None
            }
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                self.apply_menu_action_if_enabled(FilePickerMenuAction::Copy)
            }
            KeyCode::Char('x') if key.modifiers == KeyModifiers::CONTROL => {
                self.apply_menu_action_if_enabled(FilePickerMenuAction::Cut)
            }
            KeyCode::Char('v') if key.modifiers == KeyModifiers::CONTROL => {
                self.apply_menu_action_if_enabled(FilePickerMenuAction::Paste)
            }
            KeyCode::Delete => self.apply_menu_action_if_enabled(FilePickerMenuAction::Delete),
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.type_ahead_files(c);
                FilePickerAction::None
            }
            _ => FilePickerAction::None,
        }
    }

    fn handle_menu_key(&mut self, key: KeyEvent) -> FilePickerAction {
        match key.code {
            KeyCode::Esc => {
                self.close_menu();
                FilePickerAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.menu_cursor = self.menu_cursor.saturating_sub(1);
                FilePickerAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let last = self.menu_entries().len().saturating_sub(1);
                self.menu_cursor = self.menu_cursor.saturating_add(1).min(last);
                FilePickerAction::None
            }
            KeyCode::Right | KeyCode::Enter => self.activate_menu_cursor(),
            _ => FilePickerAction::None,
        }
    }

    fn activate_current_search_result(&mut self) -> FilePickerAction {
        let Some(result) = self.search.current().cloned() else {
            return FilePickerAction::None;
        };
        if result.is_dir {
            self.close_search();
            self.navigate_to_dir(result.path);
            FilePickerAction::None
        } else if self.selection_mode.accepts_entry(false) {
            FilePickerAction::Selected(result.path)
        } else {
            self.set_error(FilePickerError::WrongSelectionMode(
                "This picker accepts directories only",
            ));
            FilePickerAction::None
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> FilePickerAction {
        self.poll_search();
        let rows = self.file_visible_rows().saturating_sub(2).max(1);
        match key.code {
            KeyCode::Esc => {
                self.close_search();
                FilePickerAction::None
            }
            KeyCode::Enter => self.activate_current_search_result(),
            KeyCode::Up => {
                self.search.move_cursor(-1, rows);
                FilePickerAction::None
            }
            KeyCode::Down => {
                self.search.move_cursor(1, rows);
                FilePickerAction::None
            }
            KeyCode::PageUp => {
                self.search.move_cursor(-(rows as isize), rows);
                FilePickerAction::None
            }
            KeyCode::PageDown => {
                self.search.move_cursor(rows as isize, rows);
                FilePickerAction::None
            }
            _ => {
                if handle_text_input_key(&mut self.search.input, &key) {
                    self.restart_search();
                }
                FilePickerAction::None
            }
        }
    }

    fn handle_bookmarks_key(&mut self, key: KeyEvent) -> FilePickerAction {
        let rows = self.file_visible_rows().saturating_sub(3).max(1);
        match key.code {
            KeyCode::Esc => self.close_bookmarks(),
            KeyCode::Up | KeyCode::Char('k') => self.bookmarks.move_cursor(-1, rows),
            KeyCode::Down | KeyCode::Char('j') => self.bookmarks.move_cursor(1, rows),
            KeyCode::PageUp => self.bookmarks.move_cursor(-(rows as isize), rows),
            KeyCode::PageDown => self.bookmarks.move_cursor(rows as isize, rows),
            KeyCode::Home => {
                self.bookmarks.cursor = 0;
                self.bookmarks.scroll = 0;
            }
            KeyCode::End => {
                self.bookmarks.cursor = self.bookmarks.entries.len().saturating_sub(1);
                self.bookmarks.ensure_visible(rows);
            }
            KeyCode::Enter => self.activate_current_bookmark(),
            KeyCode::Char('a') => self.begin_add_bookmark(self.current_dir.clone()),
            KeyCode::Char('e') => self.begin_rename_bookmark(),
            KeyCode::Delete | KeyCode::Char('d') => self.delete_current_bookmark(),
            _ => {}
        }
        FilePickerAction::None
    }

    fn handle_bookmark_name_key(&mut self, key: KeyEvent) -> FilePickerAction {
        match key.code {
            KeyCode::Esc => self.cancel_bookmark_name(),
            KeyCode::Enter => self.commit_bookmark_name(),
            _ => {
                let _ = handle_text_input_key(&mut self.bookmarks.name_input, &key);
            }
        }
        FilePickerAction::None
    }

    fn handle_submenu_key(&mut self, key: KeyEvent) -> FilePickerAction {
        match key.code {
            KeyCode::Esc | KeyCode::Left => {
                self.focus = FilePickerFocus::Menu;
                self.submenu_open = false;
                FilePickerAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.submenu_cursor = self.submenu_cursor.saturating_sub(1);
                FilePickerAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let last = self.submenu_entries().len().saturating_sub(1);
                self.submenu_cursor = self.submenu_cursor.saturating_add(1).min(last);
                FilePickerAction::None
            }
            KeyCode::Enter => {
                let action = self
                    .submenu_entries()
                    .get(self.submenu_cursor)
                    .map(|(_, action)| *action);
                action
                    .map(|action| self.apply_menu_action_if_enabled(action))
                    .unwrap_or(FilePickerAction::None)
            }
            _ => FilePickerAction::None,
        }
    }

    fn handle_properties_key(&mut self, key: KeyEvent) -> FilePickerAction {
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char(' ')) {
            self.properties_open = false;
            self.focus = FilePickerFocus::Files;
        }
        FilePickerAction::None
    }

    fn handle_delete_confirm_key(&mut self, key: KeyEvent) -> FilePickerAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => self.cancel_delete(),
            KeyCode::Tab => {
                self.delete_confirm_button = self.delete_confirm_button.toggle();
            }
            KeyCode::Left => {
                self.delete_confirm_button = DeleteConfirmButton::Delete;
            }
            KeyCode::Right => {
                self.delete_confirm_button = DeleteConfirmButton::Cancel;
            }
            KeyCode::Enter | KeyCode::Char(' ') => match self.delete_confirm_button {
                DeleteConfirmButton::Delete => {
                    self.confirm_delete();
                }
                DeleteConfirmButton::Cancel => self.cancel_delete(),
            },
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.confirm_delete();
            }
            _ => {}
        }
        FilePickerAction::None
    }

    fn handle_create_name_key(&mut self, key: KeyEvent) -> FilePickerAction {
        match key.code {
            KeyCode::Esc => self.cancel_create_name(),
            KeyCode::Enter => {
                self.commit_create_name();
            }
            _ => {
                let _ = handle_text_input_key(&mut self.create_name_input, &key);
            }
        }
        FilePickerAction::None
    }


    fn handle_save_name_key(&mut self, key: KeyEvent) -> FilePickerAction {
        match key.code {
            KeyCode::Esc => FilePickerAction::Cancelled,
            KeyCode::Tab => {
                self.complete_save_name_from_entries();
                FilePickerAction::None
            }
            KeyCode::Enter => self.commit_save_name(),
            _ => {
                let _ = handle_text_input_key(&mut self.save_name_input, &key);
                FilePickerAction::None
            }
        }
    }


    fn handle_save_overwrite_confirm_key(&mut self, key: KeyEvent) -> FilePickerAction {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.cancel_save_overwrite();
                FilePickerAction::None
            }
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.confirm_save_overwrite()
            }
            _ => FilePickerAction::None,
        }
    }

    fn apply_click_action(
        &mut self,
        action: FilePickerHitAction,
        modifiers: KeyModifiers,
    ) -> FilePickerAction {
        let now = Instant::now();
        let disposition = if let FilePickerHitAction::TreeRow(index) = action {
            self.last_click = None;
            let Some(path) = self.tree_nodes.get(index).map(|node| node.path.clone()) else {
                self.tree_last_click = None;
                return FilePickerAction::None;
            };
            let disposition = crate::classify_click(
                self.tree_last_click
                    .as_ref()
                    .map(|(last_path, last_at)| (last_path, *last_at)),
                &path,
                now,
                self.double_click_window,
            );
            self.tree_last_click = Some((path, now));
            disposition
        } else {
            self.tree_last_click = None;
            let disposition = crate::classify_click(
                self.last_click.map(|last| (last.action, last.at)),
                action,
                now,
                self.double_click_window,
            );
            self.last_click = Some(LastClick { action, at: now });
            disposition
        };
        let is_double_click = disposition == crate::ClickDisposition::Double;
        let is_delayed_repeat = disposition == crate::ClickDisposition::DelayedRepeat;

        match action {
            FilePickerHitAction::FileRow(index) => {
                self.focus = FilePickerFocus::Files;
                self.tree_focused = false;
                self.set_file_cursor(index, self.file_visible_rows());
                if modifiers.contains(KeyModifiers::CONTROL) {
                    self.last_click = None;
                    self.toggle_current_multi_selection();
                    return FilePickerAction::None;
                }
                if is_double_click {
                    self.last_click = None;
                    self.open_or_select_current()
                } else if is_delayed_repeat && self.multi_selected.len() <= 1 {
                    self.last_click = None;
                    self.begin_rename_current();
                    FilePickerAction::None
                } else {
                    FilePickerAction::None
                }
            }
            FilePickerHitAction::TreeRow(index) => {
                self.focus = FilePickerFocus::Tree;
                self.tree_focused = true;
                self.set_tree_cursor(index, self.tree_visible_rows());
                if is_double_click {
                    self.tree_last_click = None;
                    self.toggle_tree_node(index);
                } else if let Some(path) = self.tree_nodes.get(index).map(|node| node.path.clone()) {
                    if is_delayed_repeat {
                        self.tree_last_click = None;
                        self.begin_rename_path(path);
                    } else {
                        self.navigate_to_dir(path);
                        self.focus = FilePickerFocus::Tree;
                        self.tree_focused = true;
                    }
                }
                FilePickerAction::None
            }
            FilePickerHitAction::SearchRow(index) => {
                self.focus = FilePickerFocus::Search;
                self.search.cursor = index.min(self.search.results.len().saturating_sub(1));
                if is_double_click {
                    self.last_click = None;
                    self.activate_current_search_result()
                } else {
                    FilePickerAction::None
                }
            }
            FilePickerHitAction::BookmarkRow(index) => {
                self.focus = FilePickerFocus::Bookmarks;
                self.bookmarks.cursor = index.min(self.bookmarks.entries.len().saturating_sub(1));
                if is_double_click {
                    self.last_click = None;
                    self.activate_current_bookmark();
                }
                FilePickerAction::None
            }
            other => self.apply_hit_action(other),
        }
    }

    fn apply_hit_action(&mut self, action: FilePickerHitAction) -> FilePickerAction {
        match action {
            FilePickerHitAction::Toolbar(toolbar) => self.apply_toolbar_action(toolbar),
            FilePickerHitAction::TitleToggleMaximize => {
                self.toggle_maximized();
                FilePickerAction::None
            }
            FilePickerHitAction::Address => {
                self.begin_address_edit();
                FilePickerAction::None
            }
            FilePickerHitAction::TreeDisclosure(index) => {
                self.focus = FilePickerFocus::Tree;
                self.tree_focused = true;
                self.set_tree_cursor(index, self.tree_visible_rows());
                self.toggle_tree_node(index);
                FilePickerAction::None
            }
            FilePickerHitAction::TreeRow(index) => {
                self.focus = FilePickerFocus::Tree;
                self.tree_focused = true;
                self.set_tree_cursor(index, self.tree_visible_rows());
                if let Some(path) = self.tree_nodes.get(index).map(|node| node.path.clone()) {
                    self.navigate_to_dir(path);
                    self.focus = FilePickerFocus::Tree;
                    self.tree_focused = true;
                }
                FilePickerAction::None
            }
            FilePickerHitAction::FileRow(index) => {
                self.focus = FilePickerFocus::Files;
                self.tree_focused = false;
                self.set_file_cursor(index, self.file_visible_rows());
                FilePickerAction::None
            }
            FilePickerHitAction::FilesBackground => {
                self.focus = FilePickerFocus::Files;
                self.tree_focused = false;
                FilePickerAction::None
            }
            FilePickerHitAction::CreateNameEditor => FilePickerAction::None,
            FilePickerHitAction::SearchInput => {
                self.focus = FilePickerFocus::Search;
                FilePickerAction::None
            }
            FilePickerHitAction::SearchClose => {
                self.close_search();
                FilePickerAction::None
            }
            FilePickerHitAction::SearchRow(index) => {
                self.focus = FilePickerFocus::Search;
                self.search.cursor = index.min(self.search.results.len().saturating_sub(1));
                FilePickerAction::None
            }
            FilePickerHitAction::BookmarkRow(index) => {
                self.focus = FilePickerFocus::Bookmarks;
                self.bookmarks.cursor = index.min(self.bookmarks.entries.len().saturating_sub(1));
                FilePickerAction::None
            }
            FilePickerHitAction::BookmarkAdd => {
                self.begin_add_bookmark(self.current_dir.clone());
                FilePickerAction::None
            }
            FilePickerHitAction::BookmarkRename => {
                self.begin_rename_bookmark();
                FilePickerAction::None
            }
            FilePickerHitAction::BookmarkDelete => {
                self.delete_current_bookmark();
                FilePickerAction::None
            }
            FilePickerHitAction::BookmarkClose => {
                self.close_bookmarks();
                FilePickerAction::None
            }
            FilePickerHitAction::ConflictPolicy(policy) => {
                self.conflict_policy = Some(policy);
                FilePickerAction::None
            }
            FilePickerHitAction::MenuNew => self.open_submenu(FilePickerSubmenuKind::New),
            FilePickerHitAction::MenuSelection => {
                self.open_submenu(FilePickerSubmenuKind::Selection)
            }
            FilePickerHitAction::Menu(action) | FilePickerHitAction::Submenu(action) => {
                self.apply_menu_action_if_enabled(action)
            }
            FilePickerHitAction::PropertiesClose => {
                self.properties_open = false;
                self.focus = FilePickerFocus::Files;
                FilePickerAction::None
            }
            FilePickerHitAction::DeleteConfirm => {
                self.delete_confirm_button = DeleteConfirmButton::Delete;
                self.confirm_delete();
                FilePickerAction::None
            }
            FilePickerHitAction::DeleteCancel => {
                self.delete_confirm_button = DeleteConfirmButton::Cancel;
                self.cancel_delete();
                FilePickerAction::None
            }
            FilePickerHitAction::SaveName => self.commit_save_name(),
            FilePickerHitAction::SaveCancel => FilePickerAction::Cancelled,
            FilePickerHitAction::SaveOverwriteConfirm => self.confirm_save_overwrite(),
            FilePickerHitAction::SaveOverwriteCancel => {
                self.cancel_save_overwrite();
                FilePickerAction::None
            }
        }
    }

    fn apply_toolbar_action(&mut self, action: ToolbarAction) -> FilePickerAction {
        match action {
            ToolbarAction::Back => {
                self.go_back();
                FilePickerAction::None
            }
            ToolbarAction::Forward => {
                self.go_forward();
                FilePickerAction::None
            }
            ToolbarAction::Up => {
                self.go_parent();
                FilePickerAction::None
            }
            ToolbarAction::Search => {
                self.open_search();
                FilePickerAction::None
            }
            ToolbarAction::FileOperations => {
                if self.menu_open {
                    self.close_menu();
                } else {
                    self.open_menu();
                }
                FilePickerAction::None
            }
            ToolbarAction::Properties => {
                if self.current_selection().is_some() {
                    self.properties_open = true;
                    self.focus = FilePickerFocus::Properties;
                }
                FilePickerAction::None
            }
            ToolbarAction::Bookmarks => {
                self.open_bookmarks();
                FilePickerAction::None
            }
            ToolbarAction::Rename => {
                self.begin_rename_current();
                FilePickerAction::None
            }
            ToolbarAction::Duplicate => {
                self.begin_duplicate_current();
                FilePickerAction::None
            }
            ToolbarAction::Delete => self.apply_menu_action_if_enabled(FilePickerMenuAction::Delete),
            ToolbarAction::AcceptSelection => self.accept_current_selection(),
            ToolbarAction::Go => self.commit_address(),
        }
    }

    fn activate_menu_cursor(&mut self) -> FilePickerAction {
        let entry = self
            .menu_entries()
            .get(self.menu_cursor)
            .map(|(_, entry)| *entry);
        match entry {
            Some(FilePickerMenuEntry::NewSubmenu) => {
                self.open_submenu(FilePickerSubmenuKind::New)
            }
            Some(FilePickerMenuEntry::SelectionSubmenu) => {
                self.open_submenu(FilePickerSubmenuKind::Selection)
            }
            Some(FilePickerMenuEntry::Action(action)) => {
                self.apply_menu_action_if_enabled(action)
            }
            None => FilePickerAction::None,
        }
    }

    fn open_submenu(&mut self, kind: FilePickerSubmenuKind) -> FilePickerAction {
        if kind == FilePickerSubmenuKind::New && !self.is_new_menu_enabled() {
            self.set_error(FilePickerError::OperationDisabled("new"));
            return FilePickerAction::None;
        }
        self.menu_open = true;
        self.submenu_open = true;
        self.submenu_kind = kind;
        self.submenu_cursor = 0;
        self.focus = FilePickerFocus::Submenu;
        FilePickerAction::None
    }

    fn apply_menu_action_if_enabled(&mut self, action: FilePickerMenuAction) -> FilePickerAction {
        if !self.is_menu_action_enabled(action) {
            self.set_error(match action {
                FilePickerMenuAction::NewFile => FilePickerError::OperationDisabled("new file"),
                FilePickerMenuAction::NewFolder => FilePickerError::OperationDisabled("new folder"),
                FilePickerMenuAction::Cut if !self.file_operation_policy().allow_cut => {
                    FilePickerError::OperationDisabled("cut")
                }
                FilePickerMenuAction::Copy if !self.file_operation_policy().allow_copy => {
                    FilePickerError::OperationDisabled("copy")
                }
                FilePickerMenuAction::Paste if !self.file_operation_policy().allow_paste => {
                    FilePickerError::OperationDisabled("paste")
                }
                FilePickerMenuAction::Delete if !self.file_operation_policy().allow_delete => {
                    FilePickerError::OperationDisabled("delete")
                }
                FilePickerMenuAction::Paste => FilePickerError::ClipboardEmpty,
                _ => FilePickerError::NoSelection,
            });
            return FilePickerAction::None;
        }
        self.apply_menu_action(action)
    }

    fn apply_menu_action(&mut self, action: FilePickerMenuAction) -> FilePickerAction {
        match action {
            FilePickerMenuAction::NewFile | FilePickerMenuAction::NewFolder => {
                let kind = if action == FilePickerMenuAction::NewFile {
                    FilePickerCreateKind::File
                } else {
                    FilePickerCreateKind::Folder
                };
                let parent = if self.context_menu_kind == FilePickerContextMenuKind::Tree {
                    self.context_menu_target
                        .clone()
                        .unwrap_or_else(|| self.current_dir.clone())
                } else {
                    self.current_dir.clone()
                };
                self.begin_create_name_in(kind, parent);
                FilePickerAction::None
            }
            FilePickerMenuAction::Cut => {
                self.cut_current();
                self.close_menu();
                FilePickerAction::None
            }
            FilePickerMenuAction::Copy => {
                self.copy_current();
                self.close_menu();
                FilePickerAction::None
            }
            FilePickerMenuAction::Paste => {
                let target = if self.context_menu_kind == FilePickerContextMenuKind::Tree {
                    self.context_menu_target
                        .clone()
                        .unwrap_or_else(|| self.current_dir.clone())
                } else {
                    self.current_dir.clone()
                };
                if let Err(error) = self.try_paste_clipboard_to(&target) {
                    self.set_error(error);
                }
                self.close_menu();
                FilePickerAction::None
            }
            FilePickerMenuAction::Rename => {
                let target = if self.context_menu_kind == FilePickerContextMenuKind::Tree {
                    self.context_menu_target.clone()
                } else {
                    self.current_selection().map(|entry| entry.path.clone())
                };
                if let Some(target) = target {
                    self.begin_rename_path(target);
                }
                FilePickerAction::None
            }
            FilePickerMenuAction::Duplicate => {
                if let Err(err) = self.duplicate_action_paths() {
                    self.set_error(err);
                }
                if self.focus != FilePickerFocus::CreateName {
                    self.close_menu();
                }
                FilePickerAction::None
            }
            FilePickerMenuAction::Delete => {
                self.request_delete_current();
                self.close_menu_but_keep_focus();
                FilePickerAction::None
            }
            FilePickerMenuAction::SelectAll => {
                self.select_all_visible();
                self.close_menu();
                FilePickerAction::None
            }
            FilePickerMenuAction::InvertSelection => {
                self.invert_visible_selection();
                self.close_menu();
                FilePickerAction::None
            }
            FilePickerMenuAction::DeselectAll => {
                self.deselect_all();
                self.close_menu();
                FilePickerAction::None
            }
            FilePickerMenuAction::TextCut => {
                self.address_input.cut_selection();
                self.close_menu();
                FilePickerAction::None
            }
            FilePickerMenuAction::TextCopy => {
                self.address_input.copy_selection();
                self.close_menu();
                FilePickerAction::None
            }
            FilePickerMenuAction::TextPaste => {
                self.address_input.paste_clipboard();
                self.close_menu();
                FilePickerAction::None
            }
            FilePickerMenuAction::OpenSystemDefault => {
                let path = self.action_paths().into_iter().next();
                self.close_menu();
                path.map(FilePickerAction::OpenSystemDefault)
                    .unwrap_or(FilePickerAction::None)
            }
            FilePickerMenuAction::AddBookmark => {
                let path = if self.context_menu_kind == FilePickerContextMenuKind::Tree {
                    self.context_menu_target
                        .clone()
                        .unwrap_or_else(|| self.current_dir.clone())
                } else {
                    self.current_dir.clone()
                };
                self.close_menu();
                self.begin_add_bookmark(path);
                FilePickerAction::None
            }
            FilePickerMenuAction::OpenBookmarks => {
                self.close_menu();
                self.open_bookmarks();
                FilePickerAction::None
            }
        }
    }

    fn open_menu(&mut self) {
        self.context_menu_kind = FilePickerContextMenuKind::Toolbar;
        self.context_menu_target = None;
        self.context_menu_anchor = None;
        self.menu_open = true;
        self.submenu_open = false;
        self.menu_cursor = 0;
        self.submenu_cursor = 0;
        self.previous_focus = self.focus;
        self.focus = FilePickerFocus::Menu;
    }

    fn open_context_menu(
        &mut self,
        action: Option<FilePickerHitAction>,
        column: u16,
        row: u16,
    ) -> FilePickerAction {
        let kind = match action {
            Some(FilePickerHitAction::Address) => {
                if self.focus != FilePickerFocus::Address {
                    self.begin_address_edit();
                }
                FilePickerContextMenuKind::Address
            }
            Some(FilePickerHitAction::TreeRow(index))
            | Some(FilePickerHitAction::TreeDisclosure(index)) => {
                self.set_tree_cursor(index, self.tree_visible_rows());
                self.focus = FilePickerFocus::Tree;
                self.tree_focused = true;
                self.context_menu_target = self.tree_nodes.get(index).map(|node| node.path.clone());
                FilePickerContextMenuKind::Tree
            }
            Some(FilePickerHitAction::FileRow(index)) => {
                self.focus = FilePickerFocus::Files;
                self.tree_focused = false;
                self.apply_file_context_target(index);
                self.context_menu_target = self.current_selection().map(|entry| entry.path.clone());
                FilePickerContextMenuKind::File
            }
            Some(FilePickerHitAction::FilesBackground) => {
                self.focus = FilePickerFocus::Files;
                self.tree_focused = false;
                self.context_menu_target = None;
                FilePickerContextMenuKind::Background
            }
            _ => return FilePickerAction::None,
        };
        self.previous_focus = self.focus;
        self.context_menu_kind = kind;
        self.context_menu_anchor = Some((column, row));
        self.menu_open = true;
        self.submenu_open = false;
        self.menu_cursor = 0;
        self.submenu_cursor = 0;
        self.focus = FilePickerFocus::Menu;
        FilePickerAction::None
    }

    pub(crate) fn close_menu(&mut self) {
        if !self.menu_open && !self.submenu_open {
            self.context_menu_anchor = None;
            return;
        }
        self.menu_open = false;
        self.submenu_open = false;
        self.context_menu_anchor = None;
        self.focus = self.previous_focus;
        self.tree_focused = self.focus == FilePickerFocus::Tree;
    }

    fn close_menu_but_keep_focus(&mut self) {
        self.menu_open = false;
        self.submenu_open = false;
        self.context_menu_anchor = None;
    }

    fn type_ahead_files(&mut self, c: char) {
        self.type_ahead.push(c, Instant::now());
        self.apply_file_type_ahead();
    }

    fn apply_file_type_ahead(&mut self) {
        let Some(index) = crate::first_type_ahead_match(
            self.entries.iter().map(|entry| crate::TypeAheadCandidate {
                name: &entry.name,
                is_dir: entry.is_dir,
            }),
            self.type_ahead.buffer(),
        ) else {
            return;
        };
        self.set_file_cursor(index, self.file_visible_rows());
    }

    fn type_ahead_tree(&mut self, c: char) {
        self.type_ahead.push(c, Instant::now());
        let Some(index) = crate::first_type_ahead_match(
            self.tree_nodes.iter().map(|node| crate::TypeAheadCandidate {
                name: &node.name,
                is_dir: true,
            }),
            self.type_ahead.buffer(),
        ) else {
            return;
        };
        self.set_tree_cursor(index, self.tree_visible_rows());
    }

}

fn point_in_rect(x: u16, y: u16, rect: Rect) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FilePickerConfig, FilePickerFilter, FilePickerSelectionMode};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;
    use std::fs;

    #[test]
    fn enter_selects_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("cover.png");
        fs::write(&file, b"png").expect("file");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: FilePickerFilter::Images,
            ..FilePickerConfig::default()
        });
        let index = picker.entries().iter().position(|entry| entry.path == file).expect("file visible");
        picker.set_file_cursor(index, 4);
        assert_eq!(picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), FilePickerAction::Selected(file));
    }

    #[test]
    fn space_accepts_current_directory_in_directory_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let child = temp.path().join("child");
        fs::create_dir(&child).expect("child");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            selection_mode: FilePickerSelectionMode::Directories,
            ..FilePickerConfig::default()
        });
        let child_index = picker.entries().iter().position(|entry| entry.path == child).expect("child visible");
        picker.set_file_cursor(child_index, 4);

        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
            FilePickerAction::Selected(temp.path().to_path_buf())
        );
    }

    #[test]
    fn tree_pane_pages_and_jumps_with_page_and_home_end_keys() {
        let temp = tempfile::tempdir().expect("tempdir");
        for index in 0..40 {
            fs::create_dir(temp.path().join(format!("dir-{index:02}"))).expect("dir");
        }
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.set_focus(FilePickerFocus::Tree);
        picker.set_tree_visible_rows(10);
        let node_count = picker.tree_nodes.len();
        assert!(node_count > 10, "fixture must overflow one page");

        picker.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(picker.tree_cursor, node_count - 1, "End jumps to the last node");

        picker.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(picker.tree_cursor, 0, "Home jumps to the first node");

        picker.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(picker.tree_cursor, 10, "PageDown advances one visible page");

        picker.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(picker.tree_cursor, 0, "PageUp retreats one visible page");
    }

    #[test]
    fn tab_switches_between_tree_and_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        assert_eq!(picker.focus(), FilePickerFocus::Files);
        let _ = picker.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(picker.focus(), FilePickerFocus::Tree);
        let _ = picker.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(picker.focus(), FilePickerFocus::Files);
    }

    #[test]
    fn double_click_file_row_selects_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("cover.png");
        fs::write(&file, b"png").expect("file");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: FilePickerFilter::Images,
            ..FilePickerConfig::default()
        });
        let index = picker.entries().iter().position(|entry| entry.path == file).expect("file visible");
        picker.record_hit_region(Rect::new(5, 5, 20, 1), FilePickerHitAction::FileRow(index));
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 6,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(picker.handle_mouse(event, Rect::default()), FilePickerAction::None);
        assert_eq!(picker.handle_mouse(event, Rect::default()), FilePickerAction::Selected(file));
    }

    #[test]
    fn control_click_toggles_independent_file_rows() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("first.flac");
        let second = temp.path().join("second.flac");
        fs::write(&first, b"first").expect("first file");
        fs::write(&second, b"second").expect("second file");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        let first_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == first)
            .expect("first visible");
        let second_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == second)
            .expect("second visible");
        picker.record_hit_region(
            Rect::new(5, 5, 20, 1),
            FilePickerHitAction::FileRow(first_index),
        );
        picker.record_hit_region(
            Rect::new(5, 6, 20, 1),
            FilePickerHitAction::FileRow(second_index),
        );

        for row in [5, 6] {
            let _ = picker.handle_mouse(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 6,
                    row,
                    modifiers: KeyModifiers::CONTROL,
                },
                Rect::default(),
            );
        }

        assert_eq!(picker.multi_selected_paths().len(), 2);
        assert!(picker.multi_selected_paths().contains(&first));
        assert!(picker.multi_selected_paths().contains(&second));
    }

    #[test]
    fn right_click_on_unmarked_file_targets_only_that_file_without_marking_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("first.flac");
        let second = temp.path().join("second.flac");
        fs::write(&first, b"first").expect("first file");
        fs::write(&second, b"second").expect("second file");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        let first_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == first)
            .expect("first visible");
        let second_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == second)
            .expect("second visible");
        picker.set_file_cursor(first_index, 8);
        assert!(picker.toggle_current_multi_selection());
        picker.record_hit_region(
            Rect::new(5, 6, 20, 1),
            FilePickerHitAction::FileRow(second_index),
        );

        let _ = picker.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: 6,
                row: 6,
                modifiers: KeyModifiers::NONE,
            },
            Rect::default(),
        );

        assert_eq!(picker.multi_selected_paths(), std::slice::from_ref(&first));
        assert_eq!(picker.action_paths(), vec![second]);
    }

    #[test]
    fn right_click_on_marked_file_preserves_marked_set_for_actions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("first.flac");
        let second = temp.path().join("second.flac");
        fs::write(&first, b"first").expect("first file");
        fs::write(&second, b"second").expect("second file");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        let first_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == first)
            .expect("first visible");
        let second_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == second)
            .expect("second visible");
        picker.set_file_cursor(first_index, 8);
        assert!(picker.toggle_current_multi_selection());
        picker.set_file_cursor(second_index, 8);
        assert!(picker.toggle_current_multi_selection());
        picker.record_hit_region(
            Rect::new(5, 6, 20, 1),
            FilePickerHitAction::FileRow(second_index),
        );

        let _ = picker.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Right),
                column: 6,
                row: 6,
                modifiers: KeyModifiers::NONE,
            },
            Rect::default(),
        );

        assert_eq!(picker.multi_selected_paths(), &[first.clone(), second.clone()]);
        assert_eq!(picker.action_paths(), vec![first, second]);
    }

    #[test]
    fn empty_click_does_not_restore_stale_menu_focus() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.focus = FilePickerFocus::Files;
        picker.previous_focus = FilePickerFocus::Tree;

        let _ = picker.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 1,
                row: 1,
                modifiers: KeyModifiers::NONE,
            },
            Rect::default(),
        );

        assert_eq!(picker.focus(), FilePickerFocus::Files);
    }

    #[test]
    fn disabled_paste_menu_does_not_call_paste() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.menu_open = true;
        picker.focus = FilePickerFocus::Menu;
        picker.menu_cursor = 4;
        let _ = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(picker.last_error(), Some(FilePickerError::ClipboardEmpty)));
    }


    #[test]
    fn operation_policy_disables_keyboard_copy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("cover.png");
        fs::write(&file, b"png").expect("file");
        let mut policy = crate::FileOperationPolicy::default();
        policy.allow_copy = false;
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: FilePickerFilter::Images,
            operation_policy: policy,
            ..FilePickerConfig::default()
        });
        let index = picker.entries().iter().position(|entry| entry.path == file).expect("file visible");
        picker.set_file_cursor(index, 4);
        let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(matches!(picker.last_error(), Some(FilePickerError::OperationDisabled("copy"))));
    }


    #[test]
    fn delete_confirmation_enter_activates_focused_button() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("cover.png");
        fs::write(&file, b"png").expect("file");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: FilePickerFilter::Images,
            ..FilePickerConfig::default()
        });
        let index = picker.entries().iter().position(|entry| entry.path == file).expect("file visible");
        picker.set_file_cursor(index, 4);

        assert!(picker.request_delete_current());
        assert_eq!(picker.delete_confirm_button, DeleteConfirmButton::Cancel);
        assert_eq!(picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), FilePickerAction::None);
        assert!(file.exists(), "Enter on the default Cancel focus must not delete");
        assert_eq!(picker.focus(), FilePickerFocus::Files);

        assert!(picker.request_delete_current());
        assert_eq!(picker.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)), FilePickerAction::None);
        assert_eq!(picker.delete_confirm_button, DeleteConfirmButton::Delete);
        assert_eq!(picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), FilePickerAction::None);
        assert!(!file.exists(), "Enter on Delete focus must delete");
    }

    #[test]
    fn delete_confirmation_arrows_select_buttons() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("cover.png");
        fs::write(&file, b"png").expect("file");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            filter: FilePickerFilter::Images,
            ..FilePickerConfig::default()
        });
        let index = picker.entries().iter().position(|entry| entry.path == file).expect("file visible");
        picker.set_file_cursor(index, 4);

        assert!(picker.request_delete_current());
        assert_eq!(picker.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)), FilePickerAction::None);
        assert_eq!(picker.delete_confirm_button, DeleteConfirmButton::Delete);
        assert_eq!(picker.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)), FilePickerAction::None);
        assert_eq!(picker.delete_confirm_button, DeleteConfirmButton::Cancel);
    }

    #[test]
    fn address_entry_navigates_to_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let child = temp.path().join("child");
        fs::create_dir(&child).expect("child");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.begin_address_edit();
        picker.address_input = crate::text_input::TextInputState::new(child.display().to_string());
        assert_eq!(picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), FilePickerAction::None);
        assert_eq!(picker.current_dir(), child.as_path());
    }

    #[test]
    fn address_file_path_does_not_select_file_in_directory_mode_from_key_input() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("cover.png");
        fs::write(&file, b"png").expect("file");

        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            selection_mode: FilePickerSelectionMode::Directories,
            filter: FilePickerFilter::Images,
            ..FilePickerConfig::default()
        });

        picker.begin_address_edit();
        picker.address_input = crate::text_input::TextInputState::new(file.display().to_string());

        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            FilePickerAction::None
        );
        assert!(matches!(picker.last_error(), Some(FilePickerError::WrongSelectionMode(_))));
        assert_eq!(picker.focus(), FilePickerFocus::Address);
        assert_eq!(picker.current_dir(), temp.path());
    }

    #[test]
    fn stale_mouse_area_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.set_last_area(Rect::new(1, 1, 80, 20));
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };
        let _ = picker.handle_mouse(event, Rect::new(2, 2, 80, 20));
        assert!(matches!(picker.last_error(), Some(FilePickerError::StaleHitRegions { .. })));
    }

    #[test]
    fn tree_right_expands_then_navigates_then_switches_to_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let child = temp.path().join("child");
        let grandchild = child.join("grandchild");
        fs::create_dir_all(&grandchild).expect("tree fixture");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.tree_nodes = vec![
            crate::TreeNode {
                path: temp.path().to_path_buf(),
                name: "temp".to_string(),
                depth: 0,
                expanded: true,
                has_children: true,
            },
            crate::TreeNode {
                path: child.clone(),
                name: "child".to_string(),
                depth: 1,
                expanded: false,
                has_children: true,
            },
        ];
        picker.tree_cursor = 1;
        picker.set_focus(FilePickerFocus::Tree);

        assert_eq!(picker.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)), FilePickerAction::None);
        assert!(picker.tree_cursor_is_expanded(), "first Right expands collapsed folder");
        assert_eq!(picker.current_dir(), temp.path(), "expansion alone does not navigate");

        assert_eq!(picker.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)), FilePickerAction::None);
        assert_eq!(picker.current_dir(), child.as_path(), "second Right navigates into expanded folder");
        assert_eq!(picker.focus(), FilePickerFocus::Tree);

        assert_eq!(picker.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)), FilePickerAction::None);
        assert_eq!(picker.focus(), FilePickerFocus::Files, "Right switches panes only after expansion/navigation are exhausted");
    }

    #[test]
    fn tree_left_collapses_then_moves_to_parent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let child = temp.path().join("child");
        let grandchild = child.join("grandchild");
        fs::create_dir_all(&grandchild).expect("tree fixture");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: child.clone(),
            ..FilePickerConfig::default()
        });
        picker.tree_nodes = vec![
            crate::TreeNode {
                path: temp.path().to_path_buf(),
                name: "temp".to_string(),
                depth: 0,
                expanded: true,
                has_children: true,
            },
            crate::TreeNode {
                path: child.clone(),
                name: "child".to_string(),
                depth: 1,
                expanded: true,
                has_children: true,
            },
            crate::TreeNode {
                path: grandchild,
                name: "grandchild".to_string(),
                depth: 2,
                expanded: false,
                has_children: false,
            },
        ];
        picker.tree_cursor = 1;
        picker.set_focus(FilePickerFocus::Tree);

        assert_eq!(picker.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)), FilePickerAction::None);
        assert!(!picker.tree_cursor_is_expanded(), "first Left collapses expanded folder");
        assert_eq!(picker.current_dir(), child.as_path());

        assert_eq!(picker.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)), FilePickerAction::None);
        assert_eq!(picker.current_dir(), temp.path(), "second Left moves to parent");
        assert_eq!(picker.tree_cursor_path(), Some(temp.path()));
        assert_eq!(picker.focus(), FilePickerFocus::Tree);
    }

    #[test]
    fn double_click_folder_row_opens_folder() {
        let temp = tempfile::tempdir().expect("tempdir");
        let child = temp.path().join("child");
        fs::create_dir(&child).expect("child");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        let index = picker.entries().iter().position(|entry| entry.path == child).expect("folder visible");
        picker.record_hit_region(Rect::new(5, 5, 20, 1), FilePickerHitAction::FileRow(index));
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 6,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(picker.handle_mouse(event, Rect::default()), FilePickerAction::None);
        assert_eq!(picker.handle_mouse(event, Rect::default()), FilePickerAction::None);
        assert_eq!(picker.current_dir(), child.as_path());
    }

    #[test]
    fn invalid_address_does_not_navigate_and_reports_error() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.begin_address_edit();
        picker.address_input = crate::text_input::TextInputState::new(
            temp.path().join("missing").display().to_string(),
        );
        assert_eq!(picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), FilePickerAction::None);
        assert_eq!(picker.current_dir(), temp.path());
        assert!(matches!(picker.last_error(), Some(FilePickerError::PathNotFoundOrFiltered(_))));
    }

    #[test]
    fn disabled_new_menu_does_not_open_submenu_or_create_prompt() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut policy = crate::FileOperationPolicy::default();
        policy.allow_new_file = false;
        policy.allow_new_folder = false;
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            operation_policy: policy,
            ..FilePickerConfig::default()
        });
        picker.menu_open = true;
        picker.focus = FilePickerFocus::Menu;
        picker.menu_cursor = 0;
        let _ = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!picker.submenu_open);
        assert_ne!(picker.focus(), FilePickerFocus::CreateName);
        assert!(matches!(picker.last_error(), Some(FilePickerError::OperationDisabled("new"))));
    }

    #[test]
    fn ctrl_a_selects_all_visible_files_idempotently() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("a.flac"), b"a").expect("a");
        fs::write(temp.path().join("b.flac"), b"b").expect("b");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });

        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            FilePickerAction::None
        );
        assert_eq!(picker.multi_selected_paths().len(), 2);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)),
            FilePickerAction::None
        );
        assert_eq!(
            picker.multi_selected_paths().len(),
            2,
            "Select All must not toggle an already selected set off"
        );
    }

    #[test]
    fn slash_and_toolbar_open_search_while_ctrl_l_opens_the_address_editor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });

        picker.set_focus(FilePickerFocus::Files);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)),
            FilePickerAction::None
        );
        assert_eq!(picker.focus(), FilePickerFocus::Search);

        picker.close_search();
        assert_eq!(picker.focus(), FilePickerFocus::Files);
        picker.set_focus(FilePickerFocus::Tree);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)),
            FilePickerAction::None
        );
        assert_eq!(picker.focus(), FilePickerFocus::Search);
        assert_eq!(
            picker.apply_toolbar_action(ToolbarAction::Search),
            FilePickerAction::None,
        );
        picker.close_search();
        assert_eq!(
            picker.focus(),
            FilePickerFocus::Tree,
            "repeated search refocus must preserve the original Tree return pane",
        );
        picker.set_focus(FilePickerFocus::Files);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL)),
            FilePickerAction::None
        );
        assert_eq!(picker.focus(), FilePickerFocus::Address);

        picker.cancel_address_edit();
        assert_eq!(picker.apply_toolbar_action(ToolbarAction::Search), FilePickerAction::None);
        assert_eq!(picker.focus(), FilePickerFocus::Search);
    }

    #[test]
    fn search_session_close_clears_state_and_reopen_starts_empty() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });

        picker.set_focus(FilePickerFocus::Tree);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)),
            FilePickerAction::None,
        );
        for ch in "album".chars() {
            assert_eq!(
                picker.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)),
                FilePickerAction::None,
            );
        }
        picker.search.results = vec![crate::FileSearchResult {
            path: temp.path().join("album.flac"),
            name: "album.flac".to_string(),
            is_dir: false,
        }];
        picker.search.cursor = 0;
        picker.search.scroll = 3;
        picker.search.error = Some("fixture error".to_string());

        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            FilePickerAction::None,
        );
        assert!(!picker.search.active);
        assert_eq!(picker.search.input.text, "");
        assert!(picker.search.results.is_empty());
        assert_eq!(picker.search.cursor, 0);
        assert_eq!(picker.search.scroll, 0);
        assert!(!picker.search.searching);
        assert!(picker.search.error.is_none());
        assert_eq!(picker.focus(), FilePickerFocus::Tree);

        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)),
            FilePickerAction::None,
        );
        assert!(picker.search.active);
        assert_eq!(picker.search.input.text, "");
        assert!(picker.search.results.is_empty());
        assert_eq!(picker.search.cursor, 0);
        assert_eq!(picker.search.scroll, 0);

        picker.search.input = crate::text_input::TextInputState::new("second".to_string());
        picker.search.results = vec![crate::FileSearchResult {
            path: temp.path().join("second.flac"),
            name: "second.flac".to_string(),
            is_dir: false,
        }];
        picker.search.cursor = 0;
        picker.search.scroll = 2;
        picker.search.error = Some("second fixture".to_string());
        picker.search.searching = true;
        assert_eq!(
            picker.apply_hit_action(FilePickerHitAction::SearchClose),
            FilePickerAction::None,
        );
        assert!(!picker.search.active);
        assert_eq!(picker.search.input.text, "");
        assert!(picker.search.results.is_empty());
        assert_eq!(picker.search.cursor, 0);
        assert_eq!(picker.search.scroll, 0);
        assert!(!picker.search.searching);
        assert!(picker.search.error.is_none());
        assert_eq!(picker.focus(), FilePickerFocus::Tree);
    }

    #[test]
    fn active_search_invocation_is_a_pure_refocus() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });

        picker.set_focus(FilePickerFocus::Tree);
        picker.open_search();
        picker.search.input = crate::text_input::TextInputState::new("album".to_string());
        picker.search.input.cursor = 2;
        picker.search.input.selection_anchor = Some(1);
        picker.search.results = vec![
            crate::FileSearchResult {
                path: temp.path().join("album-a.flac"),
                name: "album-a.flac".to_string(),
                is_dir: false,
            },
            crate::FileSearchResult {
                path: temp.path().join("album-b.flac"),
                name: "album-b.flac".to_string(),
                is_dir: false,
            },
        ];
        picker.search.cursor = 1;
        picker.search.scroll = 4;
        picker.search.searching = true;
        picker.search.error = Some("visible warning".to_string());

        let query = picker.search.input.text.clone();
        let input_cursor = picker.search.input.cursor;
        let selection_anchor = picker.search.input.selection_anchor;
        let results = picker.search.results.clone();
        let result_cursor = picker.search.cursor;
        let result_scroll = picker.search.scroll;
        let searching = picker.search.searching;
        let error = picker.search.error.clone();

        assert_eq!(
            picker.apply_toolbar_action(ToolbarAction::Search),
            FilePickerAction::None,
        );
        assert_eq!(picker.focus(), FilePickerFocus::Search);
        assert_eq!(picker.previous_focus, FilePickerFocus::Tree);
        assert_eq!(picker.search.input.text, query);
        assert_eq!(picker.search.input.cursor, input_cursor);
        assert_eq!(picker.search.input.selection_anchor, selection_anchor);
        assert_eq!(picker.search.results, results);
        assert_eq!(picker.search.cursor, result_cursor);
        assert_eq!(picker.search.scroll, result_scroll);
        assert_eq!(picker.search.searching, searching);
        assert_eq!(picker.search.error, error);

        picker.close_search();
        assert_eq!(picker.focus(), FilePickerFocus::Tree);
    }

    #[test]
    fn bookmark_rename_uses_e_like_browse() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.bookmarks.entries = vec![crate::bookmarks::BookmarkRecord {
            name: "Music".to_string(),
            path: temp.path().to_path_buf(),
        }];
        picker.focus = FilePickerFocus::Bookmarks;

        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE)),
            FilePickerAction::None
        );
        assert_eq!(picker.focus(), FilePickerFocus::BookmarkName);
        assert!(matches!(
            picker.bookmarks.naming,
            Some(crate::bookmarks::BookmarkNameAction::Rename(0))
        ));
    }

    #[test]
    fn delayed_second_tree_click_begins_inline_rename() {
        let temp = tempfile::tempdir().expect("tempdir");
        let child = temp.path().join("child");
        fs::create_dir(&child).expect("child");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        let index = picker
            .tree_nodes
            .iter()
            .position(|node| node.path == child)
            .unwrap_or_else(|| {
                picker.tree_nodes.push(crate::TreeNode {
                    path: child.clone(),
                    name: "child".to_string(),
                    depth: 1,
                    expanded: false,
                    has_children: false,
                });
                picker.tree_nodes.len() - 1
            });
        picker.tree_last_click = Some((
            child.clone(),
            Instant::now() - crate::DOUBLE_CLICK_WINDOW - std::time::Duration::from_millis(1),
        ));
        picker.tree_nodes.insert(
            index,
            crate::TreeNode {
                path: temp.path().join("inserted-before-second-click"),
                name: "inserted-before-second-click".to_string(),
                depth: 1,
                expanded: false,
                has_children: false,
            },
        );
        let shifted_index = picker
            .tree_nodes
            .iter()
            .position(|node| node.path == child)
            .expect("child remains in tree");
        let action = FilePickerHitAction::TreeRow(shifted_index);

        assert_eq!(
            picker.apply_click_action(action, KeyModifiers::NONE),
            FilePickerAction::None
        );
        assert_eq!(picker.focus(), FilePickerFocus::CreateName);
        assert_eq!(picker.pending_name_source.as_deref(), Some(child.as_path()));
    }

    #[test]
    fn click_outside_inline_rename_commits_then_processes_the_clicked_row() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("a.flac");
        let other = temp.path().join("b.flac");
        let renamed = temp.path().join("renamed.flac");
        fs::write(&source, b"a").expect("source");
        fs::write(&other, b"b").expect("other");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        let source_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == source)
            .expect("source visible");
        picker.set_file_cursor(source_index, 8);
        assert!(picker.begin_rename_current());
        picker.create_name_input = crate::text_input::TextInputState::new("renamed.flac".to_string());
        let other_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == other)
            .expect("other visible");
        picker.hit_regions.clear();
        picker.record_hit_region(Rect::new(4, 4, 20, 1), FilePickerHitAction::FileRow(other_index));
        picker.last_click = Some(LastClick {
            action: FilePickerHitAction::FileRow(other_index),
            at: Instant::now()
                - crate::DOUBLE_CLICK_WINDOW
                - std::time::Duration::from_millis(1),
        });

        let _ = picker.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 5,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
            Rect::default(),
        );

        assert!(renamed.exists());
        assert!(!source.exists());
        assert_eq!(picker.focus(), FilePickerFocus::Files);
        assert_eq!(
            picker.current_selection().map(|entry| entry.path.as_path()),
            Some(other.as_path())
        );
        assert!(picker.pending_name_action.is_none());
    }

    #[test]
    fn invalid_inline_rename_blocks_the_outside_click_and_keeps_the_editor_visible() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("a.flac");
        let other = temp.path().join("b.flac");
        fs::write(&source, b"a").expect("source");
        fs::write(&other, b"b").expect("other");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        let source_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == source)
            .expect("source visible");
        picker.set_file_cursor(source_index, 8);
        assert!(picker.begin_rename_current());
        picker.create_name_input = crate::text_input::TextInputState::new("bad/name".to_string());
        let other_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == other)
            .expect("other visible");
        picker.hit_regions.clear();
        picker.record_hit_region(Rect::new(4, 4, 20, 1), FilePickerHitAction::FileRow(other_index));

        let _ = picker.handle_mouse(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 5,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
            Rect::default(),
        );

        assert!(source.exists());
        assert_eq!(picker.focus(), FilePickerFocus::CreateName);
        assert_eq!(picker.pending_name_source.as_deref(), Some(source.as_path()));
        assert!(matches!(
            picker.last_error(),
            Some(FilePickerError::InvalidNewItemName(_))
        ));
        assert_eq!(picker.file_cursor, source_index);
    }

}
