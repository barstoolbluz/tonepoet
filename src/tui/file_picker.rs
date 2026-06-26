//! Reusable file-picker input and completion plumbing.
//!
//! This module owns the generic keyboard/mouse state machine for
//! `FilePickerState`. Feature surfaces embed a picker by storing
//! `FilePickerState`, passing events through these handlers, and reducing the
//! emitted `FilePickerOutcome` by target. The handlers deliberately do not know
//! about metadata editing, artwork, or any other caller-specific workflow.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tokio::sync::mpsc;

use super::app::{
    FilePickerFocus, FilePickerFooterAction, FilePickerOutcome, FilePickerSortKey,
    FilePickerState, FilePickerToolbarAction,
};
use super::button_map::{ButtonRenderMap, TuiButton};
use super::message::AppMessage;

/// Apply a key event to a reusable picker. Returns an outcome when the caller
/// should close the picker and hand the selection/cancellation to the owning
/// surface.
pub fn handle_key(
    picker: &mut FilePickerState,
    key: KeyEvent,
    visible_rows: usize,
) -> Option<FilePickerOutcome> {
    match picker.focus {
        FilePickerFocus::Address => handle_address_key(picker, key),
        FilePickerFocus::Search => handle_search_key(picker, key),
        FilePickerFocus::NewFolderName => handle_new_folder_key(picker, key),
        FilePickerFocus::Rename => handle_rename_key(picker, key),
        FilePickerFocus::DeleteConfirm => handle_delete_confirm_key(picker, key),
        FilePickerFocus::List => handle_list_key(picker, key, visible_rows),
    }
}

fn handle_address_key(picker: &mut FilePickerState, key: KeyEvent) -> Option<FilePickerOutcome> {
    match key.code {
        KeyCode::Esc => {
            picker.focus = FilePickerFocus::List;
            picker.sync_address_from_current_dir();
        }
        KeyCode::Enter => {
            picker.commit_address();
        }
        _ => {
            let _ = FilePickerState::handle_text_edit_key(
                &mut picker.address_input,
                &mut picker.address_cursor,
                key,
            );
        }
    }
    None
}

fn handle_search_key(picker: &mut FilePickerState, key: KeyEvent) -> Option<FilePickerOutcome> {
    match key.code {
        KeyCode::Esc => {
            picker.focus = FilePickerFocus::List;
        }
        KeyCode::Enter => {
            picker.focus = FilePickerFocus::List;
            picker.refresh();
        }
        _ => {
            if FilePickerState::handle_text_edit_key(
                &mut picker.search_input,
                &mut picker.search_cursor,
                key,
            ) {
                picker.refresh();
            }
        }
    }
    None
}

fn handle_new_folder_key(picker: &mut FilePickerState, key: KeyEvent) -> Option<FilePickerOutcome> {
    match key.code {
        KeyCode::Esc => {
            picker.focus = FilePickerFocus::List;
            picker.new_folder_input.clear();
            picker.new_folder_cursor = 0;
        }
        KeyCode::Enter => {
            picker.create_folder_from_input();
        }
        _ => {
            let _ = FilePickerState::handle_text_edit_key(
                &mut picker.new_folder_input,
                &mut picker.new_folder_cursor,
                key,
            );
        }
    }
    None
}

fn handle_rename_key(picker: &mut FilePickerState, key: KeyEvent) -> Option<FilePickerOutcome> {
    match key.code {
        KeyCode::Esc => {
            picker.focus = FilePickerFocus::List;
            picker.operation_target = None;
            picker.rename_input.clear();
            picker.rename_cursor = 0;
        }
        KeyCode::Enter => {
            picker.commit_rename();
        }
        _ => {
            let _ = FilePickerState::handle_text_edit_key(
                &mut picker.rename_input,
                &mut picker.rename_cursor,
                key,
            );
        }
    }
    None
}

fn handle_delete_confirm_key(picker: &mut FilePickerState, key: KeyEvent) -> Option<FilePickerOutcome> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
            picker.cancel_pending_delete();
        }
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
            picker.confirm_pending_delete();
        }
        _ => {}
    }
    None
}

fn handle_list_key(
    picker: &mut FilePickerState,
    key: KeyEvent,
    visible_rows: usize,
) -> Option<FilePickerOutcome> {
    match key.code {
        KeyCode::Esc => Some(picker.cancel_outcome()),
        KeyCode::Char('l') if key.modifiers == KeyModifiers::CONTROL => {
            picker.begin_address_edit();
            None
        }
        KeyCode::Char('/') => {
            picker.begin_search();
            None
        }
        KeyCode::Char('f') if key.modifiers == KeyModifiers::CONTROL => {
            picker.begin_search();
            None
        }
        KeyCode::Char('n') if key.modifiers == KeyModifiers::CONTROL => {
            picker.begin_new_folder();
            None
        }
        KeyCode::Backspace => {
            picker.go_parent();
            None
        }
        KeyCode::Char('r') if key.modifiers == KeyModifiers::CONTROL => {
            picker.refresh();
            None
        }
        KeyCode::Char('h') => {
            picker.toggle_hidden();
            None
        }
        KeyCode::Char('m') => {
            picker.set_sort(FilePickerSortKey::Modified);
            None
        }
        KeyCode::Char('z') => {
            picker.set_sort(FilePickerSortKey::Size);
            None
        }
        KeyCode::Char('c') => {
            picker.copy_current();
            None
        }
        KeyCode::Char('x') => {
            picker.move_current();
            None
        }
        KeyCode::Char('v') => {
            picker.paste_clipboard();
            None
        }
        KeyCode::Char('e') => {
            picker.begin_rename_current();
            None
        }
        KeyCode::Delete => {
            picker.delete_current();
            None
        }
        KeyCode::Up | KeyCode::Char('k') => {
            picker.move_cursor(-1, visible_rows);
            None
        }
        KeyCode::Down | KeyCode::Char('j') => {
            picker.move_cursor(1, visible_rows);
            None
        }
        KeyCode::PageUp => {
            picker.move_cursor(-(visible_rows as isize), visible_rows);
            None
        }
        KeyCode::PageDown => {
            picker.move_cursor(visible_rows as isize, visible_rows);
            None
        }
        KeyCode::Home => {
            picker.set_cursor(0, visible_rows);
            None
        }
        KeyCode::End => {
            let last = picker.entries.len().saturating_sub(1);
            picker.set_cursor(last, visible_rows);
            None
        }
        KeyCode::Enter | KeyCode::Char('o') => picker
            .open_or_select_current()
            .map(|path| picker.outcome_for_path(path)),
        KeyCode::Char('a') | KeyCode::Char(' ') => {
            if picker.primary_action == FilePickerFooterAction::Apply {
                picker
                    .accept_current_selection()
                    .map(|path| picker.outcome_for_path(path))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Apply a mouse event to a reusable picker using the shared TUI button map.
/// The caller supplies the most recent rendered button map, so hit testing stays
/// coupled to rendering rather than duplicated by feature-specific code.
pub fn handle_mouse(
    picker: &mut FilePickerState,
    mouse: MouseEvent,
    visible_rows: usize,
    button_map: &ButtonRenderMap,
) -> Option<FilePickerOutcome> {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            picker.move_cursor(-3, visible_rows);
            None
        }
        MouseEventKind::ScrollDown => {
            picker.move_cursor(3, visible_rows);
            None
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let Some(button) = button_map.find_button_at(mouse.column, mouse.row) else {
                return None;
            };
            handle_button(picker, button, visible_rows)
        }
        _ => None,
    }
}

fn handle_button(
    picker: &mut FilePickerState,
    button: TuiButton,
    visible_rows: usize,
) -> Option<FilePickerOutcome> {
    match button {
        TuiButton::FilePickerToolbar(action) => {
            apply_toolbar_action(picker, action);
            None
        }
        TuiButton::FilePickerAddress => {
            picker.begin_address_edit();
            None
        }
        TuiButton::FilePickerRow(idx) => {
            picker.set_cursor(idx, visible_rows);
            None
        }
        TuiButton::FilePickerBookmark(idx) => {
            if let Some(path) = picker.bookmarks.get(idx).cloned() {
                picker.navigate_to_dir(path);
            }
            None
        }
        TuiButton::FilePickerRecent(idx) => {
            if let Some(path) = picker.recent_locations.get(idx).cloned() {
                picker.navigate_to_dir(path);
            }
            None
        }
        TuiButton::FilePickerFooter(action) => apply_footer_action(picker, action),
        _ => None,
    }
}

fn apply_toolbar_action(picker: &mut FilePickerState, action: FilePickerToolbarAction) {
    match action {
        FilePickerToolbarAction::Back => {
            picker.go_back();
        }
        FilePickerToolbarAction::Up => {
            picker.go_parent();
        }
        FilePickerToolbarAction::Home => {
            picker.go_home();
        }
        FilePickerToolbarAction::Refresh => {
            picker.refresh();
        }
        FilePickerToolbarAction::NewFolder => {
            picker.begin_new_folder();
        }
        FilePickerToolbarAction::Search => {
            picker.begin_search();
        }
        FilePickerToolbarAction::ToggleHidden => {
            picker.toggle_hidden();
        }
        FilePickerToolbarAction::SortName => {
            picker.set_sort(FilePickerSortKey::Name);
        }
        FilePickerToolbarAction::SortModified => {
            picker.set_sort(FilePickerSortKey::Modified);
        }
        FilePickerToolbarAction::SortSize => {
            picker.set_sort(FilePickerSortKey::Size);
        }
        FilePickerToolbarAction::RevealParent => {
            picker.reveal_selected_parent();
        }
        FilePickerToolbarAction::Rename => {
            picker.begin_rename_current();
        }
        FilePickerToolbarAction::Delete => {
            picker.delete_current();
        }
        FilePickerToolbarAction::Copy => {
            picker.copy_current();
        }
        FilePickerToolbarAction::Move => {
            picker.move_current();
        }
        FilePickerToolbarAction::Paste => {
            picker.paste_clipboard();
        }
    }
}

fn apply_footer_action(
    picker: &mut FilePickerState,
    action: FilePickerFooterAction,
) -> Option<FilePickerOutcome> {
    match action {
        FilePickerFooterAction::Cancel if picker.focus == FilePickerFocus::DeleteConfirm => {
            picker.cancel_pending_delete();
            None
        }
        FilePickerFooterAction::Cancel => Some(picker.cancel_outcome()),
        FilePickerFooterAction::DeletePermanently => {
            picker.confirm_pending_delete();
            None
        }
        FilePickerFooterAction::Open => picker
            .open_or_select_current()
            .map(|path| picker.outcome_for_path(path)),
        FilePickerFooterAction::Apply | FilePickerFooterAction::Ok => picker
            .accept_current_selection()
            .map(|path| picker.outcome_for_path(path)),
    }
}

/// Send a generic picker outcome through the app message bus. All callers use
/// this same conversion, so ownership and target dispatch stay centralized in
/// the event loop.
pub fn send_completion(
    tx: &mpsc::Sender<AppMessage>,
    outcome: FilePickerOutcome,
) -> Result<(), mpsc::error::TrySendError<AppMessage>> {
    let (target, path) = match outcome {
        FilePickerOutcome::Selected { target, path } => (target, Some(path)),
        FilePickerOutcome::Cancelled { target } => (target, None),
    };
    tx.try_send(AppMessage::FilePickerComplete { target, path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{FilePickerFilter, FilePickerSelectionMode, FilePickerTarget};

    #[test]
    fn generic_key_handler_emits_target_outcome_without_artwork_coupling() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("picked.txt");
        std::fs::write(&file, "ok").expect("fixture");
        let mut picker = FilePickerState::new(
            temp.path().to_path_buf(),
            FilePickerFilter::All,
            "Pick",
            FilePickerTarget::Generic { id: "client-a".to_string() },
        );
        let index = picker.entries.iter().position(|entry| entry.path == file).expect("file visible");
        picker.set_cursor(index, 8);

        let outcome = handle_key(
            &mut picker,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            8,
        )
        .expect("file selection outcome");

        assert_eq!(
            outcome,
            FilePickerOutcome::Selected {
                target: FilePickerTarget::Generic { id: "client-a".to_string() },
                path: file,
            }
        );
    }

    #[test]
    fn directory_picker_enter_navigates_and_apply_selects() {
        let temp = tempfile::tempdir().expect("tempdir");
        let child = temp.path().join("child");
        std::fs::create_dir(&child).expect("mkdir");
        let mut picker = FilePickerState::new(
            temp.path().to_path_buf(),
            FilePickerFilter::All,
            "Pick folder",
            FilePickerTarget::Generic { id: "folder-client".to_string() },
        );
        picker.selection_mode = FilePickerSelectionMode::Directories;
        let index = picker.entries.iter().position(|entry| entry.path == child).expect("child visible");
        picker.set_cursor(index, 8);

        let outcome = handle_key(
            &mut picker,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            8,
        );
        assert!(outcome.is_none());
        assert_eq!(picker.current_dir, child);

        let outcome = apply_footer_action(&mut picker, FilePickerFooterAction::Ok)
            .expect("current directory selected");
        assert_eq!(
            outcome,
            FilePickerOutcome::Selected {
                target: FilePickerTarget::Generic { id: "folder-client".to_string() },
                path: picker.current_dir.clone(),
            }
        );
    }

    #[test]
    fn address_bar_file_path_selects_and_apply_returns_selected_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("cover.png");
        std::fs::write(&file, b"png").expect("fixture");
        let mut picker = FilePickerState::new(
            std::env::temp_dir(),
            FilePickerFilter::All,
            "Pick file",
            FilePickerTarget::Generic { id: "address-client".to_string() },
        );
        picker.begin_address_edit();
        picker.address_input = file.display().to_string();
        picker.address_cursor = picker.address_input.len();

        let immediate = handle_key(
            &mut picker,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            8,
        );
        assert!(immediate.is_none());
        assert_eq!(picker.selected.as_ref(), Some(&file));
        assert_eq!(picker.current_dir, temp.path());

        let outcome = apply_footer_action(&mut picker, FilePickerFooterAction::Ok)
            .expect("selected file returned");
        assert_eq!(
            outcome,
            FilePickerOutcome::Selected {
                target: FilePickerTarget::Generic { id: "address-client".to_string() },
                path: file,
            }
        );
    }

    #[test]
    fn new_folder_reports_validation_and_filesystem_errors() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut picker = FilePickerState::new(
            temp.path().to_path_buf(),
            FilePickerFilter::All,
            "Manage files",
            FilePickerTarget::Generic { id: "mkdir-client".to_string() },
        );

        picker.begin_new_folder();
        assert!(!picker.create_folder_from_input());
        assert_eq!(picker.error.as_deref(), Some("Enter a folder name"));

        picker.new_folder_input = format!("bad{}name", std::path::MAIN_SEPARATOR);
        picker.new_folder_cursor = picker.new_folder_input.len();
        assert!(!picker.create_folder_from_input());
        assert_eq!(picker.error.as_deref(), Some("Folder name must not contain path separators"));

        std::fs::create_dir(temp.path().join("already-there")).expect("mkdir");
        picker.new_folder_input = "already-there".to_string();
        picker.new_folder_cursor = picker.new_folder_input.len();
        assert!(!picker.create_folder_from_input());
        assert!(picker.error.as_deref().unwrap_or("").contains("Could not create folder"));
    }
    #[test]
    fn delete_key_requires_explicit_confirmation_before_removing_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("keep-me.txt");
        std::fs::write(&file, "safe").expect("fixture");
        let mut picker = FilePickerState::new(
            temp.path().to_path_buf(),
            FilePickerFilter::All,
            "Manage files",
            FilePickerTarget::Generic { id: "delete-client".to_string() },
        );
        let index = picker.entries.iter().position(|entry| entry.path == file).expect("file visible");
        picker.set_cursor(index, 8);

        let outcome = handle_key(
            &mut picker,
            KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
            8,
        );

        assert!(outcome.is_none());
        assert_eq!(picker.focus, FilePickerFocus::DeleteConfirm);
        assert_eq!(picker.pending_delete.as_ref(), Some(&file));
        assert!(file.exists(), "initial Delete key must not remove the file");
    }

    #[test]
    fn delete_confirmation_cancel_keeps_file_and_does_not_close_picker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("keep-me.txt");
        std::fs::write(&file, "safe").expect("fixture");
        let mut picker = FilePickerState::new(
            temp.path().to_path_buf(),
            FilePickerFilter::All,
            "Manage files",
            FilePickerTarget::Generic { id: "delete-client".to_string() },
        );
        let index = picker.entries.iter().position(|entry| entry.path == file).expect("file visible");
        picker.set_cursor(index, 8);
        picker.delete_current();

        let outcome = apply_footer_action(&mut picker, FilePickerFooterAction::Cancel);

        assert!(outcome.is_none(), "canceling a delete confirmation should not cancel the whole picker");
        assert_eq!(picker.focus, FilePickerFocus::List);
        assert!(picker.pending_delete.is_none());
        assert!(file.exists());
    }

    #[test]
    fn delete_confirmation_permanently_removes_file_only_after_confirm() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("delete-me.txt");
        std::fs::write(&file, "gone").expect("fixture");
        let mut picker = FilePickerState::new(
            temp.path().to_path_buf(),
            FilePickerFilter::All,
            "Manage files",
            FilePickerTarget::Generic { id: "delete-client".to_string() },
        );
        let index = picker.entries.iter().position(|entry| entry.path == file).expect("file visible");
        picker.set_cursor(index, 8);
        picker.delete_current();

        let outcome = apply_footer_action(&mut picker, FilePickerFooterAction::DeletePermanently);

        assert!(outcome.is_none());
        assert_eq!(picker.focus, FilePickerFocus::List);
        assert!(!file.exists());
        assert!(picker.entries.iter().all(|entry| entry.path != file));
    }

}
