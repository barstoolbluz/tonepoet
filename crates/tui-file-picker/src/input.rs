use crate::state::{
    DeleteConfirmButton, FilePickerAction, FilePickerContextMenuKind, FilePickerCreateKind,
    FilePickerError, FilePickerFocus, FilePickerHitAction, FilePickerMenuAction,
    FilePickerMenuEntry, FilePickerSelectionMode, FilePickerSortKey, FilePickerState, FilePickerSubmenuEntry,
    FilePickerSubmenuKind, LastClick, LastTextClick, PickerTextTarget,
    TextPointerSession, ToolbarAction,
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
    /// Route a terminal bracketed-paste event only to the picker text editor
    /// that currently owns focus. Returns false when focus belongs to a
    /// navigation surface, allowing the host to interpret the event as a
    /// filesystem-paste command on terminals that intercept Ctrl+V.
    pub fn handle_terminal_paste(&mut self, text: &str) -> bool {
        let value = text.lines().next().unwrap_or("");
        let input = match self.focus {
            FilePickerFocus::Address if self.address_editing => Some(&mut self.address_input),
            FilePickerFocus::Search => Some(&mut self.search.input),
            FilePickerFocus::BookmarkName => Some(&mut self.bookmarks.name_input),
            FilePickerFocus::CreateName => Some(&mut self.create_name_input),
            FilePickerFocus::SaveName => Some(&mut self.save_name_input),
            _ => None,
        };
        let Some(input) = input else { return false; };
        input.insert_string(value);
        if self.focus == FilePickerFocus::Search {
            self.restart_search();
        }
        true
    }

    /// Insert text returned by the embedding application's asynchronous host
    /// clipboard reader. Picker name/path editors are single-line, so this uses
    /// the same first-line rule as bracketed terminal paste.
    pub fn paste_host_clipboard_text(&mut self, text: &str) -> bool {
        self.handle_terminal_paste(text)
    }

    fn host_clipboard_paste_is_available(&self) -> bool {
        matches!(
            self.focus,
            FilePickerFocus::Address
                | FilePickerFocus::Search
                | FilePickerFocus::BookmarkName
                | FilePickerFocus::CreateName
                | FilePickerFocus::SaveName
        ) && (self.focus != FilePickerFocus::Address || self.address_editing)
    }

    /// Apply a keyboard event and return a terminal action for the host app.
    pub fn handle_key(&mut self, key: KeyEvent) -> FilePickerAction {
        if self.handle_paste_task_key(key) {
            return FilePickerAction::None;
        }
        if self.host_mutation_in_flight() {
            self.set_error(FilePickerError::OperationDisabled(
                "host-managed filesystem operation is still running",
            ));
            return FilePickerAction::None;
        }

        // Tab commands live above pane-local dispatch so their byobu-safe
        // chords are consistent in Tree and Files focus. Ctrl+W deliberately
        // stays with text editors (delete-word) and modal/editor focus blocks
        // tab mutations until the edit is committed/cancelled.
        if !self.tab_switch_blocked_by_modal() {
            let exact = key.modifiers;
            match (key.code, exact) {
                (KeyCode::Char('t'), KeyModifiers::CONTROL) => {
                    self.new_tab();
                    return FilePickerAction::None;
                }
                (KeyCode::Char('w'), KeyModifiers::CONTROL) if self.tab_close_key_available() => {
                    self.close_active_tab();
                    return FilePickerAction::None;
                }
                (KeyCode::Char('u'), KeyModifiers::ALT) => {
                    self.reopen_closed_tab();
                    return FilePickerAction::None;
                }
                (KeyCode::Char('d'), KeyModifiers::ALT) => {
                    self.duplicate_tab();
                    return FilePickerAction::None;
                }
                (KeyCode::Char('+'), KeyModifiers::NONE)
                | (KeyCode::Char('+'), KeyModifiers::SHIFT)
                    if self.tab_count() > 1 =>
                {
                    self.switch_tab_relative(1);
                    return FilePickerAction::None;
                }
                (KeyCode::Char('7'), KeyModifiers::CONTROL) => {
                    self.switch_tab_relative(-1);
                    return FilePickerAction::None;
                }
                (KeyCode::Char('['), KeyModifiers::ALT) => {
                    self.switch_tab_relative(-1);
                    return FilePickerAction::None;
                }
                (KeyCode::Char(']'), KeyModifiers::ALT) => {
                    self.switch_tab_relative(1);
                    return FilePickerAction::None;
                }
                (KeyCode::Char(','), KeyModifiers::ALT) => {
                    self.reorder_active_tab(-1);
                    return FilePickerAction::None;
                }
                (KeyCode::Char('.'), KeyModifiers::ALT) => {
                    self.reorder_active_tab(1);
                    return FilePickerAction::None;
                }
                (KeyCode::Left, KeyModifiers::ALT) if self.tab_count() > 1 => {
                    self.switch_tab_relative(-1);
                    return FilePickerAction::None;
                }
                (KeyCode::Right, KeyModifiers::ALT) if self.tab_count() > 1 => {
                    self.switch_tab_relative(1);
                    return FilePickerAction::None;
                }
                (KeyCode::Char(digit @ '1'..='9'), KeyModifiers::ALT) => {
                    let index = digit.to_digit(10).unwrap_or(1) as usize - 1;
                    self.switch_to_tab(index);
                    return FilePickerAction::None;
                }
                _ => {}
            }
        }
        if matches!(key.code, KeyCode::Char(c) if c.eq_ignore_ascii_case(&'v'))
            && key
                .modifiers
                .contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT)
            && !key.modifiers.contains(KeyModifiers::ALT)
            && self.host_clipboard_paste_is_available()
        {
            self.host_clipboard_paste_requested = true;
            return FilePickerAction::None;
        }
        self.last_click = None;
        self.tree_last_click = None;
        self.text_pointer = None;
        self.text_last_click = None;
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

    fn picker_text_target(action: FilePickerHitAction) -> Option<PickerTextTarget> {
        match action {
            FilePickerHitAction::Address => Some(PickerTextTarget::Address),
            FilePickerHitAction::CreateNameEditor => Some(PickerTextTarget::CreateName),
            FilePickerHitAction::SaveNameEditor => Some(PickerTextTarget::SaveName),
            FilePickerHitAction::SearchInput => Some(PickerTextTarget::Search),
            FilePickerHitAction::BookmarkNameEditor => Some(PickerTextTarget::BookmarkName),
            _ => None,
        }
    }

    fn text_input_mut_for_target(
        &mut self,
        target: PickerTextTarget,
    ) -> &mut crate::text_input::TextInputState {
        match target {
            PickerTextTarget::Address => &mut self.address_input,
            PickerTextTarget::CreateName => &mut self.create_name_input,
            PickerTextTarget::SaveName => &mut self.save_name_input,
            PickerTextTarget::Search => &mut self.search.input,
            PickerTextTarget::BookmarkName => &mut self.bookmarks.name_input,
        }
    }

    fn focus_picker_text_target(&mut self, target: PickerTextTarget) -> bool {
        match target {
            PickerTextTarget::Address => {
                if !self.address_editing {
                    self.begin_address_edit();
                } else {
                    self.focus = FilePickerFocus::Address;
                }
                true
            }
            PickerTextTarget::CreateName => {
                if self.pending_name_action.is_none() {
                    return false;
                }
                self.focus = FilePickerFocus::CreateName;
                true
            }
            PickerTextTarget::SaveName => {
                if self.save_mode.is_none() {
                    return false;
                }
                self.focus = FilePickerFocus::SaveName;
                true
            }
            PickerTextTarget::Search => {
                self.focus = FilePickerFocus::Search;
                true
            }
            PickerTextTarget::BookmarkName => {
                if self.bookmarks.naming.is_none() {
                    return false;
                }
                self.focus = FilePickerFocus::BookmarkName;
                true
            }
        }
    }

    fn text_target_at(&self, column: u16, row: u16) -> Option<(PickerTextTarget, Rect)> {
        self.hit_regions.iter().rev().find_map(|region| {
            if !point_in_rect(column, row, region.rect) {
                return None;
            }
            Self::picker_text_target(region.action).map(|target| (target, region.rect))
        })
    }

    /// Shared pointer contract for every picker text field: a single click
    /// clears any selection and places the cursor, drag extends a selection,
    /// and a double-click selects the complete value. This runs before the
    /// ordinary file/tree click dispatcher so inline editors never commit just
    /// because the pointer is used inside their own rendered field.
    fn handle_picker_text_pointer(&mut self, mouse: &MouseEvent) -> bool {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let Some((target, rect)) = self.text_target_at(mouse.column, mouse.row) else {
                    self.text_pointer = None;
                    self.text_last_click = None;
                    return false;
                };
                self.close_menu();
                if !self.focus_picker_text_target(target) {
                    return true;
                }
                let now = Instant::now();
                let double_click = self.text_last_click.is_some_and(|last| {
                    last.target == target
                        && now.saturating_duration_since(last.at) <= self.double_click_window
                });
                let width = rect.width as usize;
                let column = mouse.column.saturating_sub(rect.x) as usize;
                if double_click {
                    self.text_input_mut_for_target(target).select_all_text();
                    self.text_pointer = None;
                    self.text_last_click = None;
                } else {
                    self.text_input_mut_for_target(target)
                        .begin_mouse_selection(width, column);
                    self.text_pointer = Some(TextPointerSession { target, rect });
                    self.text_last_click = Some(LastTextClick { target, at: now });
                }
                true
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let Some(session) = self.text_pointer else {
                    return false;
                };
                let width = session.rect.width as usize;
                let column = mouse.column.saturating_sub(session.rect.x) as usize;
                self.text_input_mut_for_target(session.target)
                    .drag_mouse_selection(width, column);
                true
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let Some(session) = self.text_pointer.take() else {
                    return false;
                };
                let width = session.rect.width as usize;
                let column = mouse.column.saturating_sub(session.rect.x) as usize;
                let input = self.text_input_mut_for_target(session.target);
                input.drag_mouse_selection(width, column);
                input.end_mouse_selection();
                true
            }
            _ => false,
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
        if self.host_mutation_in_flight() {
            self.set_error(FilePickerError::OperationDisabled(
                "host-managed filesystem operation is still running",
            ));
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

        // Capture the topmost hit once for this mouse event. Context-menu
        // dismissal may mutate focus/menu state, but the click must still be
        // routed against the geometry that was visible when it happened.
        let pointer_hit = self.hit_regions.iter().rev().find_map(|region| {
            point_in_rect(mouse.column, mouse.row, region.rect).then_some(region.action)
        });
        let tab_hit = || pointer_hit;

        // Context menus own mouse-down while visible. Enabled menu items keep
        // their existing action dispatch; an inert MenuSurface owns borders and
        // disabled rows so clicks cannot reach controls behind the popup.
        if self.menu_open || self.submenu_open {
            let menu_owned_hit = matches!(
                pointer_hit,
                Some(
                    FilePickerHitAction::MenuSurface
                        | FilePickerHitAction::Menu(_)
                        | FilePickerHitAction::MenuNew
                        | FilePickerHitAction::MenuSelection
                        | FilePickerHitAction::MenuSort
                        | FilePickerHitAction::MenuRename
                        | FilePickerHitAction::MenuCase
                        | FilePickerHitAction::SubmenuCase
                        | FilePickerHitAction::Submenu(_)
                        | FilePickerHitAction::NestedSubmenu(_)
                )
            );
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    self.last_click = None;
                    self.tree_last_click = None;
                    if pointer_hit == Some(FilePickerHitAction::MenuSurface) {
                        return FilePickerAction::None;
                    }
                    if let Some(action) = pointer_hit.filter(|_| menu_owned_hit) {
                        // Dispatch menu entries here, before text-pointer or
                        // underlying picker handlers can observe the click.
                        return self.apply_click_action(action, mouse.modifiers);
                    }
                    self.close_menu();
                    return FilePickerAction::None;
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    if menu_owned_hit {
                        return FilePickerAction::None;
                    }
                    // Restore the pane focus before the existing right-click
                    // path opens a replacement menu. Because `pointer_hit` was
                    // captured above, closing the old popup cannot retarget the
                    // click or make `previous_focus` become Menu.
                    self.close_menu();
                }
                MouseEventKind::Down(MouseButton::Middle) => {
                    if !menu_owned_hit {
                        self.close_menu();
                    }
                    return FilePickerAction::None;
                }
                _ => {}
            }
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Right) => {
                let action = tab_hit();
                if matches!(
                    action,
                    Some(
                        FilePickerHitAction::TabStrip
                            | FilePickerHitAction::TabActivate(_)
                            | FilePickerHitAction::TabClose(_)
                            | FilePickerHitAction::TabNew
                            | FilePickerHitAction::TabReopenClosed
                    )
                ) {
                    if self.menu_open || self.submenu_open {
                        self.close_menu();
                    }
                    // A non-menu modal still owns the picker. Do not stack a
                    // tab context menu over confirmations/editors that block
                    // tab switching; an existing context menu was dismissed
                    // above so an ordinary screen-owned re-right-click works.
                    if self.tab_switch_blocked_by_modal() {
                        return FilePickerAction::None;
                    }
                    self.cancel_tab_drag();
                    self.last_click = None;
                    self.tree_last_click = None;
                    return self.open_context_menu(action, mouse.column, mouse.row);
                }
            }
            MouseEventKind::Down(MouseButton::Middle) => {
                let action = tab_hit();
                if self.menu_open
                    && matches!(
                        action,
                        Some(
                            FilePickerHitAction::TabStrip
                                | FilePickerHitAction::TabActivate(_)
                                | FilePickerHitAction::TabClose(_)
                                | FilePickerHitAction::TabNew
                                | FilePickerHitAction::TabReopenClosed
                        )
                    )
                {
                    self.close_menu();
                    return FilePickerAction::None;
                }
                match action {
                    Some(FilePickerHitAction::TabActivate(index))
                    | Some(FilePickerHitAction::TabClose(index)) => {
                        self.close_tab(index);
                        return FilePickerAction::None;
                    }
                    _ => {}
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let action = tab_hit();
                if self.menu_open
                    && matches!(
                        action,
                        Some(
                            FilePickerHitAction::TabStrip
                                | FilePickerHitAction::TabActivate(_)
                                | FilePickerHitAction::TabClose(_)
                                | FilePickerHitAction::TabNew
                                | FilePickerHitAction::TabReopenClosed
                        )
                    )
                {
                    self.close_menu();
                    return FilePickerAction::None;
                }
                match action {
                    Some(FilePickerHitAction::TabActivate(index)) => {
                        self.begin_tab_drag(index, mouse.column);
                        return FilePickerAction::None;
                    }
                    Some(FilePickerHitAction::TabClose(index)) => {
                        self.close_tab(index);
                        return FilePickerAction::None;
                    }
                    Some(FilePickerHitAction::TabNew) => {
                        self.new_tab();
                        return FilePickerAction::None;
                    }
                    Some(FilePickerHitAction::TabReopenClosed) => {
                        self.reopen_closed_tab();
                        return FilePickerAction::None;
                    }
                    Some(FilePickerHitAction::TabStrip) => {
                        return FilePickerAction::None;
                    }
                    _ => {}
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let target = match tab_hit() {
                    Some(FilePickerHitAction::TabActivate(index))
                    | Some(FilePickerHitAction::TabClose(index)) => Some(index),
                    _ => None,
                };
                if self.tabs.as_ref().is_some_and(|tabs| tabs.drag.is_some()) {
                    self.update_tab_drag(mouse.column, target);
                    return FilePickerAction::None;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.tabs.as_ref().is_some_and(|tabs| tabs.drag.is_some()) {
                    if let Some((index, reordered)) = self.finish_tab_drag() {
                        if !reordered {
                            self.switch_to_tab(index);
                        }
                    }
                    return FilePickerAction::None;
                }
            }
            _ => {}
        }

        if self.handle_picker_text_pointer(&mouse) {
            return FilePickerAction::None;
        }

        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.last_click = None;
                self.tree_last_click = None;
                self.text_pointer = None;
                self.text_last_click = None;
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
                self.text_pointer = None;
                self.text_last_click = None;
                if self.tree_focused {
                    self.move_tree_cursor(3, self.tree_visible_rows());
                } else {
                    self.move_file_cursor(3, self.file_visible_rows());
                }
                FilePickerAction::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let action = match self.resolve_name_edit_before_pointer_action(pointer_hit) {
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
                let action = if self.focus == FilePickerFocus::CreateName
                    && pointer_hit == Some(FilePickerHitAction::CreateNameEditor)
                {
                    pointer_hit
                } else {
                    match self.resolve_name_edit_before_pointer_action(pointer_hit) {
                        Ok(action) => action,
                        Err(()) => return FilePickerAction::None,
                    }
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
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                self.copy_current();
                FilePickerAction::None
            }
            KeyCode::Char('x') if key.modifiers == KeyModifiers::CONTROL => {
                self.cut_current();
                FilePickerAction::None
            }
            KeyCode::Char('v' | 'p') if key.modifiers == KeyModifiers::CONTROL => {
                self.paste_clipboard();
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
            // Visual-mode precedence is intentional: Space commits the range
            // without also toggling/advancing, and Esc exits visual mode
            // without clearing the pre-existing persistent mark set.
            KeyCode::Esc if self.cancel_visual_range() => FilePickerAction::None,
            KeyCode::Esc if !self.multi_selected.is_empty() => {
                self.deselect_all();
                FilePickerAction::None
            }
            KeyCode::Esc => {
                self.range_anchor = None;
                FilePickerAction::Cancelled
            }
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
            KeyCode::Char('v') if key.modifiers.is_empty() => {
                self.begin_or_commit_visual_range();
                FilePickerAction::None
            }
            KeyCode::Char(' ') if key.modifiers.is_empty() && self.visual_range.is_some() => {
                self.commit_visual_range();
                FilePickerAction::None
            }
            KeyCode::Up
                if self.visual_range.is_some()
                    && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT) =>
            {
                self.move_file_cursor(-1, rows);
                FilePickerAction::None
            }
            KeyCode::Down
                if self.visual_range.is_some()
                    && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT) =>
            {
                self.move_file_cursor(1, rows);
                FilePickerAction::None
            }
            KeyCode::PageUp if self.visual_range.is_some() && key.modifiers.is_empty() => {
                self.move_file_cursor(-(rows as isize), rows);
                FilePickerAction::None
            }
            KeyCode::PageDown if self.visual_range.is_some() && key.modifiers.is_empty() => {
                self.move_file_cursor(rows as isize, rows);
                FilePickerAction::None
            }
            KeyCode::Home if self.visual_range.is_some() && key.modifiers.is_empty() => {
                self.set_file_cursor(0, rows);
                FilePickerAction::None
            }
            KeyCode::End if self.visual_range.is_some() && key.modifiers.is_empty() => {
                let last = self.entries.len().saturating_sub(1);
                self.set_file_cursor(last, rows);
                FilePickerAction::None
            }
            KeyCode::Up if key.modifiers == KeyModifiers::SHIFT => {
                self.extend_range_with_cursor_move(-1, rows);
                FilePickerAction::None
            }
            KeyCode::Down if key.modifiers == KeyModifiers::SHIFT => {
                self.extend_range_with_cursor_move(1, rows);
                FilePickerAction::None
            }
            KeyCode::Up if key.modifiers.is_empty() => {
                self.move_file_cursor(-1, rows);
                FilePickerAction::None
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                self.move_file_cursor(1, rows);
                FilePickerAction::None
            }
            KeyCode::PageUp if key.modifiers.is_empty() => {
                self.move_file_cursor(-(rows as isize), rows);
                FilePickerAction::None
            }
            KeyCode::PageDown if key.modifiers.is_empty() => {
                self.move_file_cursor(rows as isize, rows);
                FilePickerAction::None
            }
            KeyCode::Home if key.modifiers.is_empty() => {
                self.set_file_cursor(0, rows);
                FilePickerAction::None
            }
            KeyCode::End if key.modifiers.is_empty() => {
                let last = self.entries.len().saturating_sub(1);
                self.set_file_cursor(last, rows);
                FilePickerAction::None
            }
            // Must precede the deliberately unguarded plain-Enter arm.
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                self.accept_current_selection()
            }
            KeyCode::Enter => self.open_or_select_current(),
            KeyCode::Char(' ') if key.modifiers.is_empty() => {
                if self.selection_mode == FilePickerSelectionMode::Directories {
                    self.accept_current_selection()
                } else {
                    self.toggle_current_multi_selection_and_advance(rows);
                    FilePickerAction::None
                }
            }
            KeyCode::Char('o' | 'm') if key.modifiers == KeyModifiers::ALT => {
                self.open_menu();
                FilePickerAction::None
            }
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                self.apply_menu_action_if_enabled(FilePickerMenuAction::Copy)
            }
            KeyCode::Char('x') if key.modifiers == KeyModifiers::CONTROL => {
                self.apply_menu_action_if_enabled(FilePickerMenuAction::Cut)
            }
            KeyCode::Char('v' | 'p') if key.modifiers == KeyModifiers::CONTROL => {
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
        if self.case_submenu_open {
            return match key.code {
                KeyCode::Esc | KeyCode::Left => {
                    self.case_submenu_open = false;
                    self.case_submenu_cursor = 0;
                    FilePickerAction::None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.case_submenu_cursor = self.case_submenu_cursor.saturating_sub(1);
                    FilePickerAction::None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.case_submenu_cursor = self
                        .case_submenu_cursor
                        .saturating_add(1)
                        .min(self.nested_case_entries().len().saturating_sub(1));
                    FilePickerAction::None
                }
                KeyCode::Enter => self
                    .nested_case_entries()
                    .get(self.case_submenu_cursor)
                    .map(|(_, action)| self.apply_menu_action_if_enabled(*action))
                    .unwrap_or(FilePickerAction::None),
                _ => FilePickerAction::None,
            };
        }

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
            KeyCode::Right | KeyCode::Enter => {
                let entry = self
                    .submenu_entries()
                    .get(self.submenu_cursor)
                    .map(|(_, entry)| *entry);
                match entry {
                    Some(FilePickerSubmenuEntry::CaseSubmenu) => {
                        self.open_nested_case_submenu()
                    }
                    Some(FilePickerSubmenuEntry::Action(action)) => {
                        self.apply_menu_action_if_enabled(action)
                    }
                    None => FilePickerAction::None,
                }
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
                let pre_click_cursor = self.current_selection().map(|entry| entry.path.clone());
                if modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) {
                    self.visual_range = None;
                    if self.range_anchor.is_none() {
                        self.range_anchor = pre_click_cursor;
                    }
                    self.mark_range_to_index(index);
                    self.set_file_cursor(index, self.file_visible_rows());
                    // classify_click recorded this press before dispatch. A
                    // range gesture is never the first half of a double click.
                    self.last_click = None;
                    return FilePickerAction::None;
                }
                self.set_file_cursor(index, self.file_visible_rows());
                if modifiers.contains(KeyModifiers::CONTROL) {
                    self.last_click = None;
                    self.toggle_current_multi_selection();
                    return FilePickerAction::None;
                }
                self.visual_range = None;
                self.range_anchor = self.current_selection().map(|entry| entry.path.clone());
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
            FilePickerHitAction::BookmarkNameEditor => {
                self.focus = FilePickerFocus::BookmarkName;
                FilePickerAction::None
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
            FilePickerHitAction::TabActivate(index) => {
                self.switch_to_tab(index);
                FilePickerAction::None
            }
            FilePickerHitAction::TabClose(index) => {
                self.close_tab(index);
                FilePickerAction::None
            }
            FilePickerHitAction::TabNew => {
                self.new_tab();
                FilePickerAction::None
            }
            FilePickerHitAction::TabReopenClosed => {
                self.reopen_closed_tab();
                FilePickerAction::None
            }
            FilePickerHitAction::TabStrip => FilePickerAction::None,
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
            FilePickerHitAction::SortColumn(sort_key) => {
                self.focus = FilePickerFocus::Files;
                self.tree_focused = false;
                self.set_sort(sort_key);
                FilePickerAction::None
            }
            FilePickerHitAction::CreateNameEditor => {
                self.focus = FilePickerFocus::CreateName;
                FilePickerAction::None
            }
            FilePickerHitAction::SaveNameEditor => {
                self.focus = FilePickerFocus::SaveName;
                FilePickerAction::None
            }
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
            FilePickerHitAction::BookmarkNameEditor => {
                self.focus = FilePickerFocus::BookmarkName;
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
            FilePickerHitAction::MenuSurface => FilePickerAction::None,
            FilePickerHitAction::MenuNew => self.open_submenu(FilePickerSubmenuKind::New),
            FilePickerHitAction::MenuSelection => {
                self.open_submenu(FilePickerSubmenuKind::Selection)
            }
            FilePickerHitAction::MenuSort => self.open_submenu(FilePickerSubmenuKind::Sort),
            FilePickerHitAction::MenuRename => self.open_submenu(FilePickerSubmenuKind::Rename),
            FilePickerHitAction::MenuCase => self.open_submenu(FilePickerSubmenuKind::TextCase),
            FilePickerHitAction::SubmenuCase => self.open_nested_case_submenu(),
            FilePickerHitAction::Menu(action)
            | FilePickerHitAction::Submenu(action)
            | FilePickerHitAction::NestedSubmenu(action) => {
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
            Some(FilePickerMenuEntry::SortSubmenu) => {
                self.open_submenu(FilePickerSubmenuKind::Sort)
            }
            Some(FilePickerMenuEntry::RenameSubmenu) => {
                self.open_submenu(FilePickerSubmenuKind::Rename)
            }
            Some(FilePickerMenuEntry::CaseSubmenu) => {
                self.open_submenu(FilePickerSubmenuKind::TextCase)
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
        self.case_submenu_open = false;
        self.case_submenu_cursor = 0;
        self.focus = FilePickerFocus::Submenu;
        FilePickerAction::None
    }

    fn open_nested_case_submenu(&mut self) -> FilePickerAction {
        if self.submenu_kind != FilePickerSubmenuKind::Rename {
            return FilePickerAction::None;
        }
        self.case_submenu_open = true;
        self.case_submenu_cursor = 0;
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
                FilePickerMenuAction::Rename if !self.file_operation_policy().allow_rename => {
                    FilePickerError::OperationDisabled("rename")
                }
                FilePickerMenuAction::Duplicate if !self.file_operation_policy().allow_duplicate => {
                    FilePickerError::OperationDisabled("duplicate")
                }
                FilePickerMenuAction::RenameTitleCase
                | FilePickerMenuAction::RenameUppercase
                | FilePickerMenuAction::RenameLowercase
                    if !self.file_operation_policy().allow_rename =>
                {
                    FilePickerError::OperationDisabled("rename")
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
            FilePickerMenuAction::TabNew => {
                self.close_menu();
                self.new_tab();
                FilePickerAction::None
            }
            FilePickerMenuAction::TabDuplicate(index) => {
                self.close_menu();
                self.duplicate_tab_at(index);
                FilePickerAction::None
            }
            FilePickerMenuAction::TabClose(index) => {
                self.close_menu();
                self.close_tab(index);
                FilePickerAction::None
            }
            FilePickerMenuAction::TabReopenClosed => {
                self.close_menu();
                self.reopen_closed_tab();
                FilePickerAction::None
            }
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
                let prior_focus = self.previous_focus;
                self.close_menu();
                self.focus = prior_focus;
                self.tree_focused = prior_focus == FilePickerFocus::Tree;
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
            FilePickerMenuAction::SortName
            | FilePickerMenuAction::SortSize
            | FilePickerMenuAction::SortType
            | FilePickerMenuAction::SortModified => {
                let sort_key = match action {
                    FilePickerMenuAction::SortName => FilePickerSortKey::Name,
                    FilePickerMenuAction::SortSize => FilePickerSortKey::Size,
                    FilePickerMenuAction::SortType => FilePickerSortKey::Type,
                    FilePickerMenuAction::SortModified => FilePickerSortKey::Modified,
                    _ => unreachable!("sort action match is exhaustive"),
                };
                self.set_sort(sort_key);
                self.close_menu();
                FilePickerAction::None
            }
            FilePickerMenuAction::TextCut => {
                let refresh_search = self.context_menu_kind == FilePickerContextMenuKind::SearchEditor;
                if let Some(input) = self.context_text_input_mut() {
                    input.cut_selection();
                }
                if refresh_search {
                    self.restart_search();
                }
                self.close_menu();
                FilePickerAction::None
            }
            FilePickerMenuAction::TextCopy => {
                if let Some(input) = self.context_text_input_mut() {
                    input.copy_selection();
                }
                self.close_menu();
                FilePickerAction::None
            }
            FilePickerMenuAction::TextPaste => {
                let refresh_search = self.context_menu_kind == FilePickerContextMenuKind::SearchEditor;
                if let Some(input) = self.context_text_input_mut() {
                    input.paste_clipboard();
                }
                if refresh_search {
                    self.restart_search();
                }
                self.close_menu();
                FilePickerAction::None
            }
            FilePickerMenuAction::TextDelete => {
                let refresh_search = self.context_menu_kind == FilePickerContextMenuKind::SearchEditor;
                if let Some(input) = self.context_text_input_mut() {
                    input.delete();
                }
                if refresh_search {
                    self.restart_search();
                }
                self.close_menu();
                FilePickerAction::None
            }
            FilePickerMenuAction::TextSelectAll => {
                if let Some(input) = self.context_text_input_mut() {
                    input.select_all_text();
                }
                self.close_menu();
                FilePickerAction::None
            }
            FilePickerMenuAction::TextTitleCase
            | FilePickerMenuAction::TextUppercase
            | FilePickerMenuAction::TextLowercase => {
                let refresh_search = self.context_menu_kind == FilePickerContextMenuKind::SearchEditor;
                let title_case = self.title_case;
                if let Some(input) = self.context_text_input_mut() {
                    match action {
                        FilePickerMenuAction::TextTitleCase => {
                            input.transform_selection_or_all(title_case);
                        }
                        FilePickerMenuAction::TextUppercase => {
                            input.transform_selection_or_all(str::to_uppercase);
                        }
                        FilePickerMenuAction::TextLowercase => {
                            input.transform_selection_or_all(str::to_lowercase);
                        }
                        _ => unreachable!("text case action match is exhaustive"),
                    }
                }
                if refresh_search {
                    self.restart_search();
                }
                self.close_menu();
                FilePickerAction::None
            }
            FilePickerMenuAction::RenameTitleCase
            | FilePickerMenuAction::RenameUppercase
            | FilePickerMenuAction::RenameLowercase => {
                if let Err(error) = self.apply_path_case_transform(action) {
                    self.set_error(error);
                }
                self.close_menu();
                FilePickerAction::None
            }
            FilePickerMenuAction::OpenSystemDefault => {
                let path = self.action_paths().into_iter().next();
                self.close_menu();
                path.map(FilePickerAction::OpenSystemDefault)
                    .unwrap_or(FilePickerAction::None)
            }
            FilePickerMenuAction::OpenInNewTab => {
                let target = if self.context_menu_kind == FilePickerContextMenuKind::Tree {
                    self.context_menu_target.clone()
                } else {
                    self.action_paths().into_iter().next()
                };
                self.close_menu();
                if let Some(path) = target {
                    self.open_dir_in_new_tab(path, false);
                }
                FilePickerAction::None
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
        self.context_menu_tab_target = None;
        self.context_menu_anchor = None;
        self.menu_open = true;
        self.submenu_open = false;
        self.case_submenu_open = false;
        self.menu_cursor = 0;
        self.submenu_cursor = 0;
        self.case_submenu_cursor = 0;
        self.previous_focus = self.focus;
        self.focus = FilePickerFocus::Menu;
    }

    fn open_context_menu(
        &mut self,
        action: Option<FilePickerHitAction>,
        column: u16,
        row: u16,
    ) -> FilePickerAction {
        self.context_menu_target = None;
        self.context_menu_tab_target = None;
        let kind = match action {
            Some(FilePickerHitAction::TabActivate(index))
            | Some(FilePickerHitAction::TabClose(index)) => {
                self.context_menu_tab_target = (index < self.tab_count()).then_some(index);
                FilePickerContextMenuKind::TabStrip
            }
            Some(
                FilePickerHitAction::TabStrip
                | FilePickerHitAction::TabNew
                | FilePickerHitAction::TabReopenClosed,
            ) => FilePickerContextMenuKind::TabStrip,
            Some(FilePickerHitAction::Address) => {
                if self.focus != FilePickerFocus::Address {
                    self.begin_address_edit();
                }
                FilePickerContextMenuKind::Address
            }
            Some(FilePickerHitAction::CreateNameEditor)
                if self.focus == FilePickerFocus::CreateName =>
            {
                FilePickerContextMenuKind::NameEditor
            }
            Some(FilePickerHitAction::SaveNameEditor)
                if self.focus == FilePickerFocus::SaveName =>
            {
                FilePickerContextMenuKind::SaveNameEditor
            }
            Some(FilePickerHitAction::SearchInput) => {
                self.focus = FilePickerFocus::Search;
                FilePickerContextMenuKind::SearchEditor
            }
            Some(FilePickerHitAction::BookmarkNameEditor)
                if self.focus == FilePickerFocus::BookmarkName =>
            {
                FilePickerContextMenuKind::BookmarkNameEditor
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
        self.case_submenu_open = false;
        self.menu_cursor = 0;
        self.submenu_cursor = 0;
        self.case_submenu_cursor = 0;
        self.focus = FilePickerFocus::Menu;
        FilePickerAction::None
    }

    pub(crate) fn close_menu(&mut self) {
        if !self.menu_open && !self.submenu_open {
            self.context_menu_anchor = None;
            self.context_menu_tab_target = None;
            return;
        }
        self.menu_open = false;
        self.submenu_open = false;
        self.case_submenu_open = false;
        self.context_menu_anchor = None;
        self.context_menu_tab_target = None;
        self.focus = self.previous_focus;
        self.tree_focused = self.focus == FilePickerFocus::Tree;
    }

    fn close_menu_but_keep_focus(&mut self) {
        self.menu_open = false;
        self.submenu_open = false;
        self.case_submenu_open = false;
        self.context_menu_anchor = None;
        self.context_menu_tab_target = None;
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
    use crate::{
        FilePickerConfig, FilePickerFilter, FilePickerSelectionMode, FilesystemClipboard,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;
    use std::fs;

    #[test]
    fn right_click_name_editor_opens_full_text_menu_without_committing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.begin_create_name_in(FilePickerCreateKind::File, temp.path().to_path_buf());
        picker.create_name_input.insert_string("Blue Öyster Cult");
        picker.record_hit_region(Rect::new(4, 4, 30, 1), FilePickerHitAction::CreateNameEditor);

        assert_eq!(
            picker.handle_mouse(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Right),
                    column: 8,
                    row: 4,
                    modifiers: KeyModifiers::NONE,
                },
                Rect::default(),
            ),
            FilePickerAction::None,
        );
        assert_eq!(picker.focus, FilePickerFocus::Menu);
        assert_eq!(picker.previous_focus, FilePickerFocus::CreateName);
        assert_eq!(picker.context_menu_kind, FilePickerContextMenuKind::NameEditor);
        assert_eq!(picker.create_name_input.text, "Blue Öyster Cult");
        assert!(!temp.path().join("Blue Öyster Cult").exists());

        let entries = picker.menu_entries();
        assert_eq!(
            entries.iter().map(|(label, _)| *label).collect::<Vec<_>>(),
            vec![
                "Paste",
                "Copy",
                "Cut",
                "Delete",
                "Select All",
                "Fix capitalization ▸",
            ],
        );
        picker.close_menu();
        assert_eq!(picker.focus, FilePickerFocus::CreateName);
        assert_eq!(picker.create_name_input.text, "Blue Öyster Cult");
    }

    #[test]
    fn editor_case_menu_transforms_selection_or_whole_unicode_value() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.begin_address_edit();
        // set_text_and_cursor clears the select-all flag begin_address_edit
        // set; poking .text directly would leave it latched and turn the
        // partial-selection transform into a whole-value transform.
        picker
            .address_input
            .set_text_and_cursor("straße and 東京".to_string(), "straße".len());
        picker.address_input.selection_anchor = Some(0);
        picker.open_context_menu(Some(FilePickerHitAction::Address), 2, 2);
        picker.apply_menu_action(FilePickerMenuAction::TextUppercase);
        assert_eq!(picker.address_input.text, "STRASSE and 東京");
        assert_eq!(picker.focus, FilePickerFocus::Address);

        picker.address_input.clear_selection();
        picker.open_context_menu(Some(FilePickerHitAction::Address), 2, 2);
        picker.apply_menu_action(FilePickerMenuAction::TextLowercase);
        assert_eq!(picker.address_input.text, "strasse and 東京");
        assert!(picker.address_input.has_selection(), "whole-value transform remains selected");
    }

    #[test]
    fn file_menu_exposes_rename_and_nested_case_submenus() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("track.flac");
        fs::write(&file, b"audio").expect("fixture");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        let index = picker
            .entries
            .iter()
            .position(|entry| entry.path == file)
            .expect("file row");
        picker.open_context_menu(Some(FilePickerHitAction::FileRow(index)), 3, 3);
        assert!(picker
            .menu_entries()
            .iter()
            .any(|(_, entry)| matches!(entry, FilePickerMenuEntry::RenameSubmenu)));
        picker.open_submenu(FilePickerSubmenuKind::Rename);
        assert!(matches!(
            picker.submenu_entries().get(1),
            Some((_, FilePickerSubmenuEntry::CaseSubmenu)),
        ));
        picker.open_nested_case_submenu();
        assert_eq!(
            picker
                .nested_case_entries()
                .iter()
                .map(|(label, _)| *label)
                .collect::<Vec<_>>(),
            vec!["Title Case", "UPPERCASE", "lowercase"],
        );
    }

    #[test]
    fn alt_o_exposes_sort_submenu_and_header_actions_share_set_sort_semantics() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("a.flac"), b"longer").expect("a");
        fs::write(temp.path().join("b.flac"), b"x").expect("b");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });

        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::ALT)),
            FilePickerAction::None,
        );
        assert_eq!(picker.focus(), FilePickerFocus::Menu);
        assert!(picker
            .menu_entries()
            .iter()
            .any(|(_, entry)| matches!(entry, FilePickerMenuEntry::SortSubmenu)));

        picker.close_menu();
        assert_eq!(
            picker.apply_hit_action(FilePickerHitAction::SortColumn(FilePickerSortKey::Name)),
            FilePickerAction::None,
        );
        assert_eq!(picker.sort_key(), FilePickerSortKey::Name);
        assert!(picker.sort_reverse(), "the active Name header toggles descending");

        assert_eq!(
            picker.apply_hit_action(FilePickerHitAction::SortColumn(FilePickerSortKey::Size)),
            FilePickerAction::None,
        );
        assert_eq!(picker.sort_key(), FilePickerSortKey::Size);
        assert!(!picker.sort_reverse(), "a different header starts ascending");
        let names = picker.entries().iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names, vec!["b.flac", "a.flac"]);
    }

    #[test]
    fn picker_text_pointer_contract_places_drags_and_double_clicks_unicode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.begin_address_edit();
        picker.address_input.text = "A Ö 東京 Z".to_string();
        picker.address_input.select_all_text();
        let field = Rect::new(10, 4, 20, 1);
        picker.record_hit_region(field, FilePickerHitAction::Address);

        let mouse = |kind, column| MouseEvent {
            kind,
            column,
            row: field.y,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            picker.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 12), Rect::default()),
            FilePickerAction::None,
        );
        assert_eq!(
            picker.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 12), Rect::default()),
            FilePickerAction::None,
        );
        assert!(!picker.address_input.has_selection());
        assert_eq!(picker.address_input.cursor, 2);

        // Double-click detection is target+time; the next phase must not read
        // as a double-click just because the test runs faster than the window.
        picker.text_last_click = None;
        picker.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 10), Rect::default());
        picker.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 14), Rect::default());
        picker.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 14), Rect::default());
        assert_eq!(
            picker.address_input.selection_range().map(|range| &picker.address_input.text[range]),
            Some("A Ö "),
        );

        // Retire the drag phase's click so the pair below forms the double.
        picker.text_last_click = None;
        picker.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 18), Rect::default());
        picker.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 18), Rect::default());
        picker.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 18), Rect::default());
        assert_eq!(picker.address_input.selection_range(), Some(0..picker.address_input.text.len()));
    }

    #[test]
    fn every_picker_text_surface_dispatches_mouse_copy_cut_and_internal_paste_uniformly() {
        use std::sync::{Arc, Mutex};

        fn active_text(picker: &FilePickerState) -> &str {
            match picker.focus {
                FilePickerFocus::Address => &picker.address_input.text,
                FilePickerFocus::CreateName => &picker.create_name_input.text,
                FilePickerFocus::SaveName => &picker.save_name_input.text,
                FilePickerFocus::Search => &picker.search.input.text,
                FilePickerFocus::BookmarkName => &picker.bookmarks.name_input.text,
                other => panic!("expected text focus, got {other:?}"),
            }
        }

        fn exercise(picker: &mut FilePickerState, action: FilePickerHitAction) {
            let field = Rect::new(10, 4, 20, 1);
            picker.record_hit_region(field, action);
            let mouse = |kind, column| MouseEvent {
                kind,
                column,
                row: field.y,
                modifiers: KeyModifiers::NONE,
            };
            assert_eq!(
                picker.handle_mouse(
                    mouse(MouseEventKind::Down(MouseButton::Left), field.x),
                    Rect::default(),
                ),
                FilePickerAction::None,
            );
            assert_eq!(
                picker.handle_mouse(
                    mouse(MouseEventKind::Drag(MouseButton::Left), field.x + 3),
                    Rect::default(),
                ),
                FilePickerAction::None,
            );
            assert_eq!(
                picker.handle_mouse(
                    mouse(MouseEventKind::Up(MouseButton::Left), field.x + 3),
                    Rect::default(),
                ),
                FilePickerAction::None,
            );

            assert_eq!(
                picker.handle_key(KeyEvent::new(
                    KeyCode::Char('c'),
                    KeyModifiers::CONTROL,
                )),
                FilePickerAction::None,
            );
            assert_eq!(crate::text_input::read_shared_text_clipboard(), "abc");
            assert_eq!(
                picker.handle_key(KeyEvent::new(
                    KeyCode::Char('x'),
                    KeyModifiers::CONTROL,
                )),
                FilePickerAction::None,
            );
            assert_eq!(active_text(picker), "def");
            assert_eq!(
                picker.handle_key(KeyEvent::new(
                    KeyCode::Char('p'),
                    KeyModifiers::CONTROL,
                )),
                FilePickerAction::None,
            );
            assert_eq!(active_text(picker), "abcdef");
        }

        crate::text_input::with_scoped_shared_text_clipboard("", || {
            let published = Arc::new(Mutex::new(Vec::<String>::new()));
            let captured = Arc::clone(&published);
            crate::text_input::with_scoped_shared_text_clipboard_publish_hook(
                move |text| captured.lock().expect("publication lock").push(text.to_string()),
                || {
                    let temp = tempfile::tempdir().expect("tempdir");

                    let mut address = FilePickerState::new(FilePickerConfig {
                        start_dir: temp.path().to_path_buf(),
                        ..FilePickerConfig::default()
                    });
                    address.begin_address_edit();
                    address.address_input = crate::text_input::TextInputState::new("abcdef".to_string());
                    exercise(&mut address, FilePickerHitAction::Address);

                    let mut create = FilePickerState::new(FilePickerConfig {
                        start_dir: temp.path().to_path_buf(),
                        ..FilePickerConfig::default()
                    });
                    create.begin_create_name_in(
                        FilePickerCreateKind::File,
                        temp.path().to_path_buf(),
                    );
                    create.create_name_input = crate::text_input::TextInputState::new("abcdef".to_string());
                    exercise(&mut create, FilePickerHitAction::CreateNameEditor);

                    let mut save = FilePickerState::new(FilePickerConfig {
                        start_dir: temp.path().to_path_buf(),
                        save_mode: Some(crate::SaveModeConfig {
                            default_name: "abcdef".to_string(),
                            confirm_overwrite: true,
                            hide_extension: None,
                            style: crate::SaveModeStyle::Inline,
                        }),
                        ..FilePickerConfig::default()
                    });
                    save.focus = FilePickerFocus::SaveName;
                    save.save_name_input = crate::text_input::TextInputState::new("abcdef".to_string());
                    exercise(&mut save, FilePickerHitAction::SaveNameEditor);

                    let mut search = FilePickerState::new(FilePickerConfig {
                        start_dir: temp.path().to_path_buf(),
                        ..FilePickerConfig::default()
                    });
                    search.focus = FilePickerFocus::Search;
                    search.search.input = crate::text_input::TextInputState::new("abcdef".to_string());
                    exercise(&mut search, FilePickerHitAction::SearchInput);

                    let mut bookmark = FilePickerState::new(FilePickerConfig {
                        start_dir: temp.path().to_path_buf(),
                        ..FilePickerConfig::default()
                    });
                    bookmark.begin_add_bookmark(temp.path().to_path_buf());
                    bookmark.bookmarks.name_input = crate::text_input::TextInputState::new("abcdef".to_string());
                    exercise(&mut bookmark, FilePickerHitAction::BookmarkNameEditor);
                },
            );
            let published = published.lock().expect("publication lock");
            assert_eq!(published.len(), 10, "copy and cut must each mirror once per surface");
            assert!(published.iter().all(|value| value == "abc"));
        });
    }

    #[test]
    fn picker_search_and_save_name_editors_use_text_context_menu_without_action_buttons() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            save_mode: Some(crate::SaveModeConfig {
                default_name: "album".to_string(),
                confirm_overwrite: true,
                hide_extension: None,
                style: crate::SaveModeStyle::Inline,
            }),
            ..FilePickerConfig::default()
        });
        picker.focus = FilePickerFocus::Search;
        picker.search.input = crate::text_input::TextInputState::new("needle".to_string());
        picker.open_context_menu(Some(FilePickerHitAction::SearchInput), 1, 1);
        assert_eq!(picker.context_menu_kind, FilePickerContextMenuKind::SearchEditor);
        assert_eq!(picker.previous_focus, FilePickerFocus::Search);
        picker.close_menu();

        picker.focus = FilePickerFocus::SaveName;
        picker.save_name_input = crate::text_input::TextInputState::new("album".to_string());
        picker.open_context_menu(Some(FilePickerHitAction::SaveNameEditor), 1, 1);
        assert_eq!(picker.context_menu_kind, FilePickerContextMenuKind::SaveNameEditor);
        assert_eq!(picker.previous_focus, FilePickerFocus::SaveName);
        picker.close_menu();
        assert_eq!(picker.save_name_input.text, "album");
    }

    #[test]
    fn terminal_paste_routes_only_to_the_focused_picker_editor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.focus = FilePickerFocus::Search;
        picker.search.input = crate::text_input::TextInputState::new_selected("old".to_string());

        assert!(picker.handle_terminal_paste("replacement\nignored"));
        assert_eq!(picker.search.input.text, "replacement");

        picker.focus = FilePickerFocus::Files;
        assert!(!picker.handle_terminal_paste("must not enter a navigation surface"));
        assert_eq!(picker.search.input.text, "replacement");
    }

    #[test]
    fn ctrl_shift_v_requests_host_paste_only_for_focused_text_editors() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });

        picker.begin_address_edit();
        assert_eq!(
            picker.handle_key(KeyEvent::new(
                KeyCode::Char('v'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )),
            FilePickerAction::None
        );
        assert!(picker.take_host_clipboard_paste_request());
        assert!(!picker.take_host_clipboard_paste_request());
        assert_eq!(
            picker.handle_key(KeyEvent::new(
                KeyCode::Char('V'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )),
            FilePickerAction::None
        );
        assert!(picker.take_host_clipboard_paste_request());
        assert!(picker.paste_host_clipboard_text("/tmp/music\nignored"));
        assert_eq!(picker.address_input.text, "/tmp/music");

        picker.cancel_address_edit();
        picker.focus = FilePickerFocus::Files;
        assert_eq!(
            picker.handle_key(KeyEvent::new(
                KeyCode::Char('v'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )),
            FilePickerAction::None
        );
        assert!(!picker.take_host_clipboard_paste_request());
    }

    #[test]
    fn ctrl_p_starts_the_same_filesystem_paste_as_ctrl_v() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let destination_dir = temp.path().join("destination");
        fs::create_dir(&source_dir).expect("source dir");
        fs::create_dir(&destination_dir).expect("destination dir");
        let source = source_dir.join("track.flac");
        fs::write(&source, b"audio").expect("source");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: source_dir,
            ..FilePickerConfig::default()
        });
        let source_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == source)
            .expect("source visible");
        picker.set_file_cursor(source_index, 4);
        assert!(picker.copy_current());
        picker.navigate_to_dir(destination_dir);
        picker.focus = FilePickerFocus::Files;

        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            FilePickerAction::None,
        );
        assert!(picker.paste_task.is_some());
    }

    fn select_tree_path(picker: &mut FilePickerState, target: &std::path::Path) {
        // tempdirs sit under dot-prefixed paths, and pre-expanded ancestors may
        // carry stale or hidden-excluded children; force hidden visibility and
        // refresh each ancestor with a collapse+expand double toggle.
        picker.show_hidden = true;
        picker.set_focus(FilePickerFocus::Tree);
        let mut index = 0usize;
        for _ in 0..131072 {
            picker.set_tree_cursor(index, usize::MAX);
            let Some(current) = picker.tree_cursor_path().map(std::path::Path::to_path_buf)
            else {
                break;
            };
            if current == target {
                return;
            }
            if target.starts_with(&current) {
                if picker.tree_cursor_is_expanded() {
                    picker.toggle_tree_node(index);
                }
                picker.toggle_tree_node(index);
                index += 1;
                continue;
            }
            let before = picker.tree_cursor_path().map(std::path::Path::to_path_buf);
            picker.set_tree_cursor(index + 1, usize::MAX);
            let after = picker.tree_cursor_path().map(std::path::Path::to_path_buf);
            if before == after {
                break;
            }
            index += 1;
        }
        panic!("tree path was not materialized: {}", target.display());
    }

    fn picker_with_tree_clipboard(
    ) -> (
        tempfile::TempDir,
        FilePickerState,
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let current_dir = temp.path().join("current");
        let tree_target = temp.path().join("tree-target");
        fs::create_dir(&source_dir).expect("source dir");
        fs::create_dir(&current_dir).expect("current dir");
        fs::create_dir(&tree_target).expect("tree target");
        let source = source_dir.join("track.flac");
        fs::write(&source, b"audio").expect("source");

        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: source_dir,
            ..FilePickerConfig::default()
        });
        let source_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == source)
            .expect("source visible");
        picker.set_file_cursor(source_index, 4);
        assert!(picker.copy_current());
        assert!(picker.navigate_to_dir(current_dir.clone()));
        select_tree_path(&mut picker, &tree_target);
        (temp, picker, source, current_dir, tree_target)
    }

    fn wait_for_path(path: &std::path::Path) {
        for _ in 0..200 {
            if path.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("timed out waiting for {}", path.display());
    }

    #[test]
    fn tree_ctrl_p_and_ctrl_v_paste_into_the_selected_tree_directory() {
        for code in ['p', 'v'] {
            let (_temp, mut picker, source, current_dir, tree_target) =
                picker_with_tree_clipboard();

            assert_eq!(
                picker.handle_key(KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL)),
                FilePickerAction::None,
            );

            let expected = tree_target.join(source.file_name().expect("source name"));
            wait_for_path(&expected);
            assert!(expected.exists(), "Ctrl+{code} must target the selected tree row");
            assert!(
                !current_dir.join(source.file_name().expect("source name")).exists(),
                "Ctrl+{code} must not fall back to current_dir while Tree owns focus"
            );
        }
    }

    #[test]
    fn tree_ctrl_c_and_ctrl_x_capture_the_selected_tree_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let current_dir = temp.path().join("current");
        let tree_target = temp.path().join("tree-target");
        fs::create_dir(&current_dir).expect("current dir");
        fs::create_dir(&tree_target).expect("tree target");

        for (code, mode) in [
            ('c', crate::FilePickerClipboardMode::Copy),
            ('x', crate::FilePickerClipboardMode::Cut),
        ] {
            let mut picker = FilePickerState::new(FilePickerConfig {
                start_dir: current_dir.clone(),
                ..FilePickerConfig::default()
            });
            select_tree_path(&mut picker, &tree_target);

            picker.handle_key(KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL));

            let clipboard = picker.clipboard.as_ref().expect("tree clipboard");
            assert_eq!(clipboard.mode(), mode);
            assert_eq!(clipboard.paths(), &[tree_target.clone()]);
        }
    }

    #[test]
    fn tree_paste_reports_empty_and_disabled_policy_without_changing_target() {
        let temp = tempfile::tempdir().expect("tempdir");
        let current_dir = temp.path().join("current");
        let tree_target = temp.path().join("tree-target");
        fs::create_dir(&current_dir).expect("current dir");
        fs::create_dir(&tree_target).expect("tree target");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: current_dir,
            ..FilePickerConfig::default()
        });
        select_tree_path(&mut picker, &tree_target);

        picker.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert!(matches!(picker.last_error(), Some(FilePickerError::ClipboardEmpty)));
        assert_eq!(picker.filesystem_paste_target(), tree_target);

        let source = temp.path().join("source.flac");
        fs::write(&source, b"audio").expect("source");
        picker.clipboard = FilesystemClipboard::new(
            crate::FilePickerClipboardMode::Copy,
            vec![source],
        );
        let mut policy = picker.file_operation_policy();
        policy.allow_paste = false;
        picker.set_file_operation_policy(policy);

        picker.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
        assert!(matches!(
            picker.last_error(),
            Some(FilePickerError::OperationDisabled("paste"))
        ));
        assert!(picker.paste_task.is_none());
    }

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
    fn space_marks_and_advances_while_alt_enter_confirms_many() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = temp.path().join("01.flac");
        let second = temp.path().join("02.flac");
        fs::write(&first, b"one").expect("first");
        fs::write(&second, b"two").expect("second");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            selection_mode: FilePickerSelectionMode::FilesOrDirectories,
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
        picker.set_file_cursor(first_index, 4);

        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
            FilePickerAction::None
        );
        assert!(picker.is_path_multi_selected(&first));
        assert_eq!(picker.file_cursor, second_index);
        assert_eq!(picker.range_anchor.as_deref(), Some(first.as_path()));
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
            FilePickerAction::SelectedMany(vec![first])
        );
    }

    #[test]
    fn visual_range_commit_is_additive_and_does_not_toggle_or_advance() {
        let temp = tempfile::tempdir().expect("tempdir");
        for index in 1..=4 {
            fs::write(temp.path().join(format!("{index:02}.flac")), b"audio")
                .expect("fixture");
        }
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.set_file_cursor(0, 4);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)),
            FilePickerAction::None
        );
        let persistent = picker.entries()[0].path.clone();
        picker.set_file_cursor(1, 4);

        picker.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let committed_cursor = picker.file_cursor;
        picker.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));

        assert!(picker.visual_range.is_none());
        assert_eq!(picker.file_cursor, committed_cursor);
        assert!(picker.is_path_multi_selected(&persistent));
        assert!(picker.is_path_multi_selected(&picker.entries()[1].path));
        assert!(picker.is_path_multi_selected(&picker.entries()[2].path));
        assert!(!picker.is_path_multi_selected(&picker.entries()[3].path));
    }

    #[test]
    fn alt_click_ranges_from_stable_anchor_and_clears_double_click_state() {
        let temp = tempfile::tempdir().expect("tempdir");
        for index in 1..=5 {
            fs::write(temp.path().join(format!("{index:02}.flac")), b"audio")
                .expect("fixture");
        }
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.set_file_cursor(1, 5);
        picker.apply_click_action(FilePickerHitAction::FileRow(1), KeyModifiers::NONE);
        let anchor = picker.entries()[1].path.clone();

        picker.apply_click_action(FilePickerHitAction::FileRow(3), KeyModifiers::ALT);
        assert_eq!(picker.range_anchor.as_deref(), Some(anchor.as_path()));
        assert!(picker.last_click.is_none());
        for index in 1..=3 {
            assert!(picker.is_path_multi_selected(&picker.entries()[index].path));
        }

        picker.apply_click_action(FilePickerHitAction::FileRow(4), KeyModifiers::ALT);
        assert_eq!(picker.range_anchor.as_deref(), Some(anchor.as_path()));
        assert!(picker.is_path_multi_selected(&picker.entries()[4].path));
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
        picker.menu_cursor = picker
            .menu_entries()
            .iter()
            .position(|(label, _)| *label == "Paste")
            .expect("toolbar menu exposes Paste");
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

#[cfg(test)]
mod tabbed_input_tests {
    use super::*;
    use crate::{FilePickerConfig, FilePickerSelectionMode};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;
    use std::fs;

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE }
    }

    #[test]
    fn ctrl_w_remains_delete_word_in_picker_text_editors() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        assert!(picker.open_dir_in_new_tab(temp.path().to_path_buf(), true));
        assert_eq!(picker.tab_count(), 2);
        picker.begin_address_edit();
        picker.address_input = crate::text_input::TextInputState::new("/tmp/alpha beta".to_string());

        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)),
            FilePickerAction::None,
        );
        assert_eq!(picker.tab_count(), 2, "Ctrl+W in an editor must not close a tab");
        assert_eq!(picker.address_input.text, "/tmp/alpha ");
    }

    #[test]
    fn alt_arrows_preserve_single_tab_history_then_switch_when_tabs_exist() {
        let temp = tempfile::tempdir().expect("tempdir");
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        fs::create_dir(&a).expect("a");
        fs::create_dir(&b).expect("b");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: a.clone(),
            selection_mode: FilePickerSelectionMode::Directories,
            ..FilePickerConfig::default()
        });

        picker.navigate_to_dir_with_history(b.clone(), true);
        assert_eq!(picker.current_dir(), b.as_path());
        let _ = picker.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(picker.current_dir(), a.as_path(), "single-tab Alt+Left remains history back");

        assert!(picker.open_dir_in_new_tab(b.clone(), true));
        assert_eq!(picker.active_tab_index(), 1);
        let _ = picker.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
        assert_eq!(picker.active_tab_index(), 0, "multi-tab Alt+Left switches tabs");
        assert_eq!(picker.current_dir(), a.as_path());
        let _ = picker.handle_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT));
        assert_eq!(picker.active_tab_index(), 1, "Alt+] is an always-available tab alias");
    }

    #[test]
    fn plus_and_ctrl7_match_browse_tab_switching_without_stealing_single_tab_typeahead() {
        let temp = tempfile::tempdir().expect("tempdir");
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        fs::create_dir(&a).expect("a");
        fs::create_dir(&b).expect("b");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: a,
            ..FilePickerConfig::default()
        });

        let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('+'), KeyModifiers::NONE));
        assert_eq!(picker.tab_count(), 1);
        assert_eq!(picker.type_ahead.buffer(), "+", "single-tab '+' remains type-ahead");

        picker.type_ahead.clear();
        assert!(picker.open_dir_in_new_tab(b, false));
        assert_eq!(picker.active_tab_index(), 0);
        let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('+'), KeyModifiers::NONE));
        assert_eq!(picker.active_tab_index(), 1, "'+' switches to the next picker tab");
        let _ = picker.handle_key(KeyEvent::new(KeyCode::Char('7'), KeyModifiers::CONTROL));
        assert_eq!(picker.active_tab_index(), 0, "Ctrl+7 switches to the previous picker tab");
    }

    #[test]
    fn open_tab_menu_left_click_away_closes_without_dispatching_underlying_file_hit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let other = temp.path().join("other");
        fs::create_dir(&other).expect("other");
        fs::write(temp.path().join("a.flac"), b"a").expect("a");
        fs::write(temp.path().join("b.flac"), b"b").expect("b");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        assert!(picker.open_dir_in_new_tab(other, false));
        assert!(picker.entries.len() >= 2);
        let target = if picker.file_cursor == 0 { 1 } else { 0 };
        let original_cursor = picker.file_cursor;

        picker.hit_regions.clear();
        picker.record_hit_region(Rect::new(0, 0, 8, 1), FilePickerHitAction::TabActivate(1));
        picker.record_hit_region(Rect::new(0, 5, 20, 1), FilePickerHitAction::FileRow(target));

        let _ = picker.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Right), 2, 0),
            Rect::default(),
        );
        assert!(picker.menu_open);
        assert_eq!(picker.context_menu_kind, FilePickerContextMenuKind::TabStrip);

        let _ = picker.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 2, 5),
            Rect::default(),
        );
        assert!(!picker.menu_open, "left click-away dismisses the tab menu");
        assert_eq!(picker.focus, FilePickerFocus::Files);
        assert_eq!(
            picker.file_cursor, original_cursor,
            "dismissal must consume the click instead of selecting the file behind the menu",
        );
    }

    #[test]
    fn right_click_away_replaces_tab_menu_and_restores_real_pane_focus_after_escape() {
        let temp = tempfile::tempdir().expect("tempdir");
        let other = temp.path().join("other");
        fs::create_dir(&other).expect("other");
        fs::write(temp.path().join("a.flac"), b"a").expect("a");
        fs::write(temp.path().join("b.flac"), b"b").expect("b");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        assert!(picker.open_dir_in_new_tab(other, false));
        assert!(!picker.entries.is_empty());

        picker.hit_regions.clear();
        picker.record_hit_region(Rect::new(0, 0, 8, 1), FilePickerHitAction::TabActivate(1));
        picker.record_hit_region(Rect::new(0, 5, 20, 1), FilePickerHitAction::FileRow(0));

        let _ = picker.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Right), 2, 0),
            Rect::default(),
        );
        assert_eq!(picker.focus, FilePickerFocus::Menu);
        assert_eq!(picker.context_menu_kind, FilePickerContextMenuKind::TabStrip);

        let _ = picker.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Right), 2, 5),
            Rect::default(),
        );
        assert!(picker.menu_open);
        assert_eq!(picker.context_menu_kind, FilePickerContextMenuKind::File);
        assert_eq!(
            picker.previous_focus,
            FilePickerFocus::Files,
            "replacement menu must capture the restored pane, never Menu",
        );

        let _ = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!picker.menu_open);
        assert_eq!(picker.focus, FilePickerFocus::Files);
    }

    #[test]
    fn enabled_menu_item_hit_still_dispatches_over_menu_surface() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        picker.context_menu_kind = FilePickerContextMenuKind::TabStrip;
        picker.context_menu_tab_target = None;
        picker.previous_focus = FilePickerFocus::Files;
        picker.focus = FilePickerFocus::Menu;
        picker.menu_open = true;

        picker.hit_regions.clear();
        picker.record_hit_region(Rect::new(0, 5, 20, 1), FilePickerHitAction::MenuSurface);
        picker.record_hit_region(
            Rect::new(0, 5, 20, 1),
            FilePickerHitAction::Menu(FilePickerMenuAction::TabNew),
        );

        let _ = picker.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 2, 5),
            Rect::default(),
        );
        assert_eq!(picker.tab_count(), 2, "enabled menu items retain their action dispatch");
        assert!(!picker.menu_open);
    }

    #[test]
    fn disabled_tab_reopen_row_is_inert_and_cannot_click_through() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("a.flac"), b"a").expect("a");
        fs::write(temp.path().join("b.flac"), b"b").expect("b");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: temp.path().to_path_buf(),
            ..FilePickerConfig::default()
        });
        assert!(picker.entries.len() >= 2);
        assert!(!picker.has_closed_tabs());
        assert!(!picker.is_menu_action_enabled(FilePickerMenuAction::TabReopenClosed));

        picker.context_menu_kind = FilePickerContextMenuKind::TabStrip;
        picker.context_menu_tab_target = None;
        picker.previous_focus = FilePickerFocus::Files;
        picker.focus = FilePickerFocus::Menu;
        picker.menu_open = true;
        let original_cursor = picker.file_cursor;
        let target = if original_cursor == 0 { 1 } else { 0 };

        picker.hit_regions.clear();
        // Rendering records the underlying picker hit first and the popup
        // surface later. Reverse hit-testing must therefore stop at the inert
        // surface for disabled rows such as Reopen Closed Tab.
        picker.record_hit_region(Rect::new(0, 5, 20, 1), FilePickerHitAction::FileRow(target));
        picker.record_hit_region(Rect::new(0, 5, 20, 1), FilePickerHitAction::MenuSurface);

        let _ = picker.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left), 2, 5),
            Rect::default(),
        );
        assert!(picker.menu_open, "clicking a disabled row leaves the menu open");
        assert_eq!(picker.focus, FilePickerFocus::Menu);
        assert_eq!(
            picker.file_cursor, original_cursor,
            "disabled menu rows must own their cells instead of clicking through",
        );
    }

    #[test]
    fn tab_strip_right_click_targets_clicked_tab_and_empty_space_has_no_tab_actions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        fs::create_dir(&a).expect("a");
        fs::create_dir(&b).expect("b");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: a.clone(),
            ..FilePickerConfig::default()
        });
        assert!(picker.open_dir_in_new_tab(b.clone(), false));
        picker.hit_regions.clear();
        picker.record_hit_region(Rect::new(0, 0, 40, 1), FilePickerHitAction::TabStrip);
        picker.record_hit_region(Rect::new(10, 0, 8, 1), FilePickerHitAction::TabActivate(1));
        picker.begin_tab_drag(0, 1);

        let _ = picker.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Right), 12, 0),
            Rect::default(),
        );
        assert_eq!(picker.context_menu_kind, FilePickerContextMenuKind::TabStrip);
        assert_eq!(picker.context_menu_tab_target, Some(1));
        assert!(picker.tabs.as_ref().is_some_and(|tabs| tabs.drag.is_none()));
        let entries = picker.menu_entries();
        assert_eq!(
            entries.iter().map(|(label, _)| *label).collect::<Vec<_>>(),
            vec!["New Tab", "Duplicate", "Close", "Reopen Closed Tab"],
        );
        assert!(matches!(
            entries[1].1,
            FilePickerMenuEntry::Action(FilePickerMenuAction::TabDuplicate(1))
        ));
        assert!(!picker.is_menu_action_enabled(FilePickerMenuAction::TabReopenClosed));

        let _ = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let _ = picker.handle_mouse(
            mouse(MouseEventKind::Down(MouseButton::Right), 30, 0),
            Rect::default(),
        );
        assert_eq!(picker.context_menu_tab_target, None);
        assert_eq!(
            picker.menu_entries().iter().map(|(label, _)| *label).collect::<Vec<_>>(),
            vec!["New Tab", "Reopen Closed Tab"],
        );

        picker.close_menu();
        picker.apply_menu_action(FilePickerMenuAction::TabDuplicate(1));
        assert_eq!(picker.tab_count(), 3);
        assert_eq!(picker.current_dir(), b.as_path(), "duplicate binds to the clicked non-active tab");
    }

    #[test]
    fn tab_strip_context_close_and_reopen_enablement_are_targeted() {
        let temp = tempfile::tempdir().expect("tempdir");
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        fs::create_dir(&a).expect("a");
        fs::create_dir(&b).expect("b");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: a.clone(),
            ..FilePickerConfig::default()
        });
        assert!(picker.open_dir_in_new_tab(b, false));
        assert_eq!(picker.current_dir(), a.as_path());

        picker.apply_menu_action(FilePickerMenuAction::TabClose(1));
        assert_eq!(picker.tab_count(), 1);
        assert_eq!(picker.current_dir(), a.as_path());
        assert!(picker.has_closed_tabs());

        picker.context_menu_kind = FilePickerContextMenuKind::TabStrip;
        picker.context_menu_tab_target = None;
        assert!(picker.is_menu_action_enabled(FilePickerMenuAction::TabReopenClosed));
        assert_eq!(
            picker.menu_entries().iter().map(|(label, _)| *label).collect::<Vec<_>>(),
            vec!["New Tab", "Reopen Closed Tab"],
        );
    }

    #[test]
    fn middle_click_close_is_scoped_to_tab_cells_and_drag_reorders_without_closing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        fs::create_dir(&a).expect("a");
        fs::create_dir(&b).expect("b");
        let mut picker = FilePickerState::new(FilePickerConfig {
            start_dir: a,
            ..FilePickerConfig::default()
        });
        assert!(picker.open_dir_in_new_tab(b, false));
        picker.hit_regions.clear();
        picker.record_hit_region(Rect::new(0, 0, 8, 1), FilePickerHitAction::TabActivate(0));
        picker.record_hit_region(Rect::new(8, 0, 8, 1), FilePickerHitAction::TabActivate(1));
        picker.record_hit_region(Rect::new(0, 2, 20, 1), FilePickerHitAction::Address);

        let _ = picker.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Middle), 2, 2), Rect::default());
        assert_eq!(picker.tab_count(), 2, "middle-click outside the strip must remain unclaimed by tabs");

        let _ = picker.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 0), Rect::default());
        let _ = picker.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 10, 0), Rect::default());
        let _ = picker.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 10, 0), Rect::default());
        assert_eq!(picker.tab_count(), 2);
        assert_eq!(picker.active_tab_index(), 1, "drag moves the active slot instead of treating the drop as a click");

        let _ = picker.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Middle), 10, 0), Rect::default());
        assert_eq!(picker.tab_count(), 1, "middle-click over a tab cell closes exactly that tab");
    }
}
