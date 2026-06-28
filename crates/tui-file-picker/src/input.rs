use crate::state::{
    DeleteConfirmButton, FilePickerAction, FilePickerCreateKind, FilePickerError, FilePickerFocus,
    FilePickerHitAction, FilePickerMenuAction, FilePickerState, LastClick, ToolbarAction,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::time::Instant;

impl FilePickerState {
    /// Apply a keyboard event and return a terminal action for the host app.
    pub fn handle_key(&mut self, key: KeyEvent) -> FilePickerAction {
        self.last_click = None;
        match self.focus {
            FilePickerFocus::Address => self.handle_address_key(key),
            FilePickerFocus::Tree => self.handle_tree_key(key),
            FilePickerFocus::Files => self.handle_file_key(key),
            FilePickerFocus::Menu => self.handle_menu_key(key),
            FilePickerFocus::Submenu => self.handle_submenu_key(key),
            FilePickerFocus::Properties => self.handle_properties_key(key),
            FilePickerFocus::DeleteConfirm => self.handle_delete_confirm_key(key),
            FilePickerFocus::CreateName => self.handle_create_name_key(key),
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
                if self.tree_focused {
                    self.move_tree_cursor(-3, self.tree_visible_rows());
                } else {
                    self.move_file_cursor(-3, self.file_visible_rows());
                }
                FilePickerAction::None
            }
            MouseEventKind::ScrollDown => {
                self.last_click = None;
                if self.tree_focused {
                    self.move_tree_cursor(3, self.tree_visible_rows());
                } else {
                    self.move_file_cursor(3, self.file_visible_rows());
                }
                FilePickerAction::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let Some(action) = self.hit_regions.iter().rev().find_map(|region| {
                    point_in_rect(mouse.column, mouse.row, region.rect).then_some(region.action)
                }) else {
                    self.last_click = None;
                    self.menu_open = false;
                    self.submenu_open = false;
                    return FilePickerAction::None;
                };
                self.apply_click_action(action)
            }
            _ => FilePickerAction::None,
        }
    }

    fn handle_address_key(&mut self, key: KeyEvent) -> FilePickerAction {
        match key.code {
            KeyCode::Esc => {
                self.cancel_address_edit();
                FilePickerAction::None
            }
            KeyCode::Enter => self.commit_address(),
            _ => {
                edit_text(&mut self.address_buffer, &mut self.address_cursor, key);
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
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_tree_cursor(-1, self.tree_visible_rows());
                FilePickerAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
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
            KeyCode::Backspace => {
                self.go_parent();
                FilePickerAction::None
            }
            KeyCode::Char('l') if key.modifiers == KeyModifiers::CONTROL => {
                self.begin_address_edit();
                FilePickerAction::None
            }
            _ => FilePickerAction::None,
        }
    }

    fn handle_file_key(&mut self, key: KeyEvent) -> FilePickerAction {
        let rows = self.file_visible_rows();
        match key.code {
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
                self.begin_address_edit();
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
            KeyCode::F(5) => {
                self.refresh();
                FilePickerAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_file_cursor(-1, rows);
                FilePickerAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
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
            KeyCode::Delete => {
                self.apply_menu_action_if_enabled(FilePickerMenuAction::Delete)
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
                self.menu_cursor = self.menu_cursor.saturating_add(1).min(4);
                FilePickerAction::None
            }
            KeyCode::Right if self.menu_cursor == 0 => self.open_new_submenu_if_enabled(),
            KeyCode::Enter => self.activate_menu_cursor(),
            KeyCode::Char('n') | KeyCode::Char('N') => self.open_new_submenu_if_enabled(),
            KeyCode::Char('x') | KeyCode::Char('X') => self.apply_menu_action_if_enabled(FilePickerMenuAction::Cut),
            KeyCode::Char('c') | KeyCode::Char('C') => self.apply_menu_action_if_enabled(FilePickerMenuAction::Copy),
            KeyCode::Char('p') | KeyCode::Char('P') => self.apply_menu_action_if_enabled(FilePickerMenuAction::Paste),
            KeyCode::Char('d') | KeyCode::Char('D') => self.apply_menu_action_if_enabled(FilePickerMenuAction::Delete),
            _ => FilePickerAction::None,
        }
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
                self.submenu_cursor = self.submenu_cursor.saturating_add(1).min(1);
                FilePickerAction::None
            }
            KeyCode::Enter => {
                let action = if self.submenu_cursor == 0 {
                    FilePickerMenuAction::NewFile
                } else {
                    FilePickerMenuAction::NewFolder
                };
                self.apply_menu_action_if_enabled(action)
            }
            KeyCode::Char('f') | KeyCode::Char('F') => self.apply_menu_action_if_enabled(FilePickerMenuAction::NewFile),
            KeyCode::Char('d') | KeyCode::Char('D') => self.apply_menu_action_if_enabled(FilePickerMenuAction::NewFolder),
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
                edit_text(&mut self.create_name_buffer, &mut self.create_name_cursor, key);
            }
        }
        FilePickerAction::None
    }

    fn apply_click_action(&mut self, action: FilePickerHitAction) -> FilePickerAction {
        let now = Instant::now();
        let is_double_click = self
            .last_click
            .map(|last| last.action == action && now.duration_since(last.at) <= self.double_click_window)
            .unwrap_or(false);
        self.last_click = Some(LastClick { action, at: now });

        match action {
            FilePickerHitAction::FileRow(index) => {
                self.focus = FilePickerFocus::Files;
                self.tree_focused = false;
                self.set_file_cursor(index, self.file_visible_rows());
                if is_double_click {
                    self.last_click = None;
                    self.open_or_select_current()
                } else {
                    FilePickerAction::None
                }
            }
            other => self.apply_hit_action(other),
        }
    }

    fn apply_hit_action(&mut self, action: FilePickerHitAction) -> FilePickerAction {
        match action {
            FilePickerHitAction::Toolbar(toolbar) => self.apply_toolbar_action(toolbar),
            FilePickerHitAction::Address => {
                self.begin_address_edit();
                FilePickerAction::None
            }
            FilePickerHitAction::TreeRow(index) => {
                self.focus = FilePickerFocus::Tree;
                self.tree_focused = true;
                self.set_tree_cursor(index, self.tree_visible_rows());
                self.activate_tree_cursor();
                FilePickerAction::None
            }
            FilePickerHitAction::FileRow(index) => {
                self.focus = FilePickerFocus::Files;
                self.tree_focused = false;
                self.set_file_cursor(index, self.file_visible_rows());
                FilePickerAction::None
            }
            FilePickerHitAction::ConflictPolicy(policy) => {
                self.conflict_policy = Some(policy);
                FilePickerAction::None
            }
            FilePickerHitAction::MenuNew => self.open_new_submenu_if_enabled(),
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
            ToolbarAction::AcceptSelection => self.accept_current_selection(),
            ToolbarAction::Go => self.commit_address(),
        }
    }

    fn activate_menu_cursor(&mut self) -> FilePickerAction {
        match self.menu_cursor {
            0 => self.open_new_submenu_if_enabled(),
            1 => self.apply_menu_action_if_enabled(FilePickerMenuAction::Cut),
            2 => self.apply_menu_action_if_enabled(FilePickerMenuAction::Copy),
            3 => self.apply_menu_action_if_enabled(FilePickerMenuAction::Paste),
            4 => self.apply_menu_action_if_enabled(FilePickerMenuAction::Delete),
            _ => FilePickerAction::None,
        }
    }


    fn open_new_submenu_if_enabled(&mut self) -> FilePickerAction {
        if !self.is_new_menu_enabled() {
            self.set_error(FilePickerError::OperationDisabled("new"));
            return FilePickerAction::None;
        }
        self.menu_open = true;
        self.submenu_open = true;
        self.menu_cursor = 0;
        self.submenu_cursor = 0;
        self.focus = FilePickerFocus::Submenu;
        FilePickerAction::None
    }

    fn apply_menu_action_if_enabled(&mut self, action: FilePickerMenuAction) -> FilePickerAction {
        if !self.is_menu_action_enabled(action) {
            self.set_error(match action {
                FilePickerMenuAction::NewFile => FilePickerError::OperationDisabled("new file"),
                FilePickerMenuAction::NewFolder => FilePickerError::OperationDisabled("new folder"),
                FilePickerMenuAction::Cut if !self.file_operation_policy().allow_cut => FilePickerError::OperationDisabled("cut"),
                FilePickerMenuAction::Copy if !self.file_operation_policy().allow_copy => FilePickerError::OperationDisabled("copy"),
                FilePickerMenuAction::Paste if !self.file_operation_policy().allow_paste => FilePickerError::OperationDisabled("paste"),
                FilePickerMenuAction::Delete if !self.file_operation_policy().allow_delete => FilePickerError::OperationDisabled("delete"),
                FilePickerMenuAction::Paste => FilePickerError::ClipboardEmpty,
                _ => FilePickerError::NoSelection,
            });
            return FilePickerAction::None;
        }
        self.apply_menu_action(action)
    }

    fn apply_menu_action(&mut self, action: FilePickerMenuAction) -> FilePickerAction {
        match action {
            FilePickerMenuAction::NewFile => self.begin_create_name(FilePickerCreateKind::File),
            FilePickerMenuAction::NewFolder => self.begin_create_name(FilePickerCreateKind::Folder),
            FilePickerMenuAction::Cut => {
                self.cut_current();
                self.close_menu();
            }
            FilePickerMenuAction::Copy => {
                self.copy_current();
                self.close_menu();
            }
            FilePickerMenuAction::Paste => {
                self.paste_clipboard();
                self.close_menu();
            }
            FilePickerMenuAction::Delete => {
                self.request_delete_current();
                self.close_menu_but_keep_focus();
            }
        }
        FilePickerAction::None
    }

    fn open_menu(&mut self) {
        self.menu_open = true;
        self.submenu_open = false;
        self.menu_cursor = 0;
        self.submenu_cursor = 0;
        self.previous_focus = self.focus;
        self.focus = FilePickerFocus::Menu;
    }

    fn close_menu(&mut self) {
        self.menu_open = false;
        self.submenu_open = false;
        self.focus = FilePickerFocus::Files;
    }

    fn close_menu_but_keep_focus(&mut self) {
        self.menu_open = false;
        self.submenu_open = false;
    }
}

fn point_in_rect(x: u16, y: u16, rect: Rect) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

pub(crate) fn edit_text(text: &mut String, cursor: &mut usize, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char(c) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
            let idx = nearest_char_boundary(text, (*cursor).min(text.len()));
            text.insert(idx, c);
            *cursor = idx.saturating_add(c.len_utf8());
            true
        }
        KeyCode::Backspace => {
            let idx = nearest_char_boundary(text, (*cursor).min(text.len()));
            if idx > 0 {
                let prev = previous_char_boundary(text, idx);
                text.drain(prev..idx);
                *cursor = prev;
            }
            true
        }
        KeyCode::Delete => {
            let idx = nearest_char_boundary(text, (*cursor).min(text.len()));
            if idx < text.len() {
                let next = next_char_boundary(text, idx);
                text.drain(idx..next);
                *cursor = idx;
            }
            true
        }
        KeyCode::Left => {
            *cursor = previous_char_boundary(text, (*cursor).min(text.len()));
            true
        }
        KeyCode::Right => {
            *cursor = next_char_boundary(text, (*cursor).min(text.len()));
            true
        }
        KeyCode::Home => {
            *cursor = 0;
            true
        }
        KeyCode::End => {
            *cursor = text.len();
            true
        }
        _ => false,
    }
}

fn nearest_char_boundary(text: &str, mut cursor: usize) -> usize {
    while cursor > 0 && !text.is_char_boundary(cursor) {
        cursor -= 1;
    }
    cursor
}

fn previous_char_boundary(text: &str, cursor: usize) -> usize {
    let mut prev = cursor.saturating_sub(1);
    while prev > 0 && !text.is_char_boundary(prev) {
        prev -= 1;
    }
    prev
}

fn next_char_boundary(text: &str, cursor: usize) -> usize {
    let mut next = cursor.saturating_add(1).min(text.len());
    while next < text.len() && !text.is_char_boundary(next) {
        next += 1;
    }
    next
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
    fn disabled_paste_menu_does_not_call_paste() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.menu_open = true;
        picker.focus = FilePickerFocus::Menu;
        picker.menu_cursor = 3;
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
        picker.address_buffer = child.display().to_string();
        picker.address_cursor = picker.address_buffer.len();
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
        picker.address_buffer = file.display().to_string();
        picker.address_cursor = picker.address_buffer.len();

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
        picker.address_buffer = temp.path().join("missing").display().to_string();
        picker.address_cursor = picker.address_buffer.len();
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

}
