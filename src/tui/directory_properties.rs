//! Filesystem Properties overlay controller. Rendering never reads the filesystem.
//! Folder measurements remain owned by the shared, cancellable accounting service.
use std::path::PathBuf;
use std::time::SystemTime;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tokio::sync::mpsc::Sender;
use super::app::{ActiveOverlay, AppState};
use super::browse::BrowseNavigationPane;
use super::button_map::TuiButton;
use super::message::AppMessage;

#[derive(Debug, Clone)]
pub enum PropertiesSubject {
    Folder,
    /// Facts captured from the existing Browse listing, not a synchronous stat.
    File { size: u64, modified: Option<SystemTime>, is_symlink: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertiesFocus { EditMetadata, Recalculate, Ok }

#[derive(Debug, Clone)]
pub struct FolderPropertiesState {
    pub path: PathBuf,
    pub subject: PropertiesSubject,
    /// Only folders subscribe to recursive directory accounting.
    pub request_id: Option<u64>,
    pub scroll: usize,
    pub horizontal_scroll: usize,
    pub cancelled: bool,
    pub maximized: bool,
    pub focus: PropertiesFocus,
}

impl FolderPropertiesState {
    pub fn is_folder(&self) -> bool { matches!(self.subject, PropertiesSubject::Folder) }
    fn focusables(&self) -> &'static [PropertiesFocus] {
        if self.is_folder() {
            &[PropertiesFocus::EditMetadata, PropertiesFocus::Recalculate, PropertiesFocus::Ok]
        } else {
            &[PropertiesFocus::EditMetadata, PropertiesFocus::Ok]
        }
    }
    fn cycle_focus(&mut self, backward: bool) {
        let choices = self.focusables();
        let index = choices.iter().position(|choice| *choice == self.focus).unwrap_or(0);
        self.focus = choices[if backward { (index + choices.len() - 1) % choices.len() }
            else { (index + 1) % choices.len() }];
    }
}

pub fn target_from_selection(app: &AppState) -> Result<PathBuf, &'static str> {
    if app.browse.is_in_archive() { return Err("Filesystem Properties is unavailable inside archives"); }
    let target = if app.browse.navigation_pane == BrowseNavigationPane::Tree {
        app.browse.selected_tree_path()
    } else {
        let entry = app.browse.selected_entry().ok_or("No filesystem item selected")?;
        if matches!(entry.kind, crate::convert::classify::EntryKind::ParentDir) {
            Some(app.browse.current_dir.clone())
        } else {
            Some(entry.path.clone())
        }
    };
    let path = target.ok_or("No filesystem item selected")?;
    if !path.is_absolute() { return Err("Filesystem Properties requires an absolute path"); }
    Ok(path)
}

pub fn open_for_path(app: &mut AppState, path: PathBuf, tx: &Sender<AppMessage>) {
    if app.current_screen != super::app::AppScreen::Browse || app.browse.is_in_archive() {
        app.set_status("Filesystem Properties is unavailable outside the filesystem Browse view");
        return;
    }
    if !path.is_absolute() {
        app.set_status("Filesystem Properties requires an absolute path");
        return;
    }
    // The listing already holds the file's type/mtime. Its classification
    // follows symlinks, so recognize them before treating directory entries
    // as folders. The listing's mtime for a symlink may belong to its target;
    // never present it as the link's own timestamp. The unfiltered directory
    // snapshot also covers a tree-selected child hidden by a listing filter.
    // No filesystem metadata is read on opening Properties.
    let subject = app.browse.entries.iter().chain(app.browse.all_dirs.iter())
        .find(|entry| entry.path == path && !matches!(entry.kind, crate::convert::classify::EntryKind::ParentDir))
        .filter(|entry| entry.is_symlink || !entry.is_child_dir())
        .map(|entry| PropertiesSubject::File {
            size: entry.size,
            modified: if entry.is_symlink { None } else { entry.modified },
            is_symlink: entry.is_symlink,
        })
        .unwrap_or(PropertiesSubject::Folder);
    if let ActiveOverlay::FolderProperties(previous) = &app.active_overlay {
        if let Some(id) = previous.request_id {
            app.directory_accounting.detach_explicit(&previous.path, id);
        }
    }
    let request_id = if matches!(subject, PropertiesSubject::Folder) {
        Some(app.directory_accounting.request_explicit(path.clone(), false, tx))
    } else { None };
    app.active_overlay = ActiveOverlay::FolderProperties(Box::new(FolderPropertiesState {
        path, subject, request_id, scroll: 0, horizontal_scroll: 0,
        cancelled: false, maximized: false, focus: PropertiesFocus::Ok,
    }));
}

pub fn open_selected(app: &mut AppState, tx: &Sender<AppMessage>) {
    match target_from_selection(app) {
        Ok(path) => open_for_path(app, path, tx),
        Err(error) => app.set_status(error),
    }
}

fn close(app: &mut AppState, state: &FolderPropertiesState) {
    if let Some(id) = state.request_id {
        app.directory_accounting.detach_explicit(&state.path, id);
    }
    app.active_overlay = ActiveOverlay::None;
}

fn activate(app: &mut AppState, mut state: FolderPropertiesState, action: PropertiesFocus, tx: &Sender<AppMessage>) {
    match action {
        PropertiesFocus::Ok => close(app, &state),
        PropertiesFocus::EditMetadata => {
            let path = state.path.clone();
            close(app, &state);
            // The same editor backend and admission policy as Alt+P, scoped
            // to this Properties dialog's exact target (also from Tree).
            super::keybindings::open_metadata_editor_for_properties(app, path, tx);
        }
        PropertiesFocus::Recalculate => {
            if !state.is_folder() { app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state)); return; }
            let busy = app.directory_accounting.view(&state.path).is_some_and(|view| view.running)
                || app.directory_accounting.scan_pending(&state.path);
            if !busy {
                if let Some(id) = state.request_id {
                    app.directory_accounting.detach_explicit(&state.path, id);
                }
                state.request_id = Some(app.directory_accounting.request_explicit(state.path.clone(), true, tx));
                state.cancelled = false;
                state.scroll = 0;
            }
            app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state));
        }
    }
}

fn cancel(app: &mut AppState, mut state: FolderPropertiesState) {
    if state.is_folder() {
        app.directory_accounting.cancel_scan(&state.path);
        state.cancelled = true;
    }
    app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state));
}

pub fn handle_key(app: &mut AppState, mut state: FolderPropertiesState, key: KeyEvent, tx: &Sender<AppMessage>) {
    let running = state.is_folder() && !state.cancelled
        && app.directory_accounting.view(&state.path).is_some_and(|view| view.running);
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => close(app, &state),
        (KeyCode::Char('m' | 'M'), KeyModifiers::ALT) => {
            state.maximized = !state.maximized;
            app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state));
        }
        (KeyCode::Enter | KeyCode::Char(' '), KeyModifiers::NONE) => {
            if running && state.focus == PropertiesFocus::Recalculate {
                cancel(app, state);
            } else { activate(app, state.clone(), state.focus, tx); }
        }
        (KeyCode::Tab, _) => { state.cycle_focus(false); app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state)); }
        (KeyCode::BackTab, _) => { state.cycle_focus(true); app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state)); }
        (KeyCode::Char('r' | 'R'), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            if !running { activate(app, state, PropertiesFocus::Recalculate, tx); }
            else { app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state)); }
        }
        (KeyCode::Char('c' | 'C'), KeyModifiers::NONE | KeyModifiers::SHIFT) if running => cancel(app, state),
        (KeyCode::Char('e' | 'E'), KeyModifiers::NONE | KeyModifiers::SHIFT)
        | (KeyCode::Char('p'), KeyModifiers::ALT) => activate(app, state, PropertiesFocus::EditMetadata, tx),
        (KeyCode::Left, _) => { state.horizontal_scroll = state.horizontal_scroll.saturating_sub(4); app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state)); }
        (KeyCode::Right, _) => { state.horizontal_scroll = state.horizontal_scroll.saturating_add(4); app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state)); }
        (KeyCode::Home, _) => { state.horizontal_scroll = 0; state.scroll = 0; app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state)); }
        (KeyCode::Up, _) => { state.scroll = state.scroll.saturating_sub(1); app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state)); }
        (KeyCode::Down, _) => { state.scroll = state.scroll.saturating_add(1); app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state)); }
        _ => { // In particular, Ctrl+P never falls through to host-clipboard paste.
            app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state));
        }
    }
}

/// Own all mouse events while Properties is open; never click the Browse pane
/// through a modal. Buttons are hit-tested against the *rendered* positions.
pub fn handle_mouse(app: &mut AppState, mouse: MouseEvent, tx: &Sender<AppMessage>) {
    let ActiveOverlay::FolderProperties(state) = &app.active_overlay else { return; };
    let state = (**state).clone();
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            match app.button_map.find_button_at(mouse.column, mouse.row) {
                Some(TuiButton::PropertiesTitle) => {
                    let mut state = state;
                    if app.double_click.register_click(TuiButton::PropertiesTitle,
                        mouse.column, mouse.row, tui_file_picker::DOUBLE_CLICK_WINDOW) {
                        state.maximized = !state.maximized;
                    }
                    app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state));
                }
                Some(TuiButton::PropertiesOk) => activate(app, state, PropertiesFocus::Ok, tx),
                Some(TuiButton::PropertiesEditMetadata) => activate(app, state, PropertiesFocus::EditMetadata, tx),
                Some(TuiButton::PropertiesRecalculate) => activate(app, state, PropertiesFocus::Recalculate, tx),
                Some(TuiButton::PropertiesCancel) => cancel(app, state),
                _ => {},
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let code = if mouse.kind == MouseEventKind::ScrollUp { KeyCode::Up } else { KeyCode::Down };
            handle_key(app, state, KeyEvent::new(code, KeyModifiers::NONE), tx);
        }
        _ => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::TonepoetConfig, convert::classify::EntryKind};
    use crate::tui::{browse::BrowseEntry, button_map::ButtonRenderMap};
    use ratatui::layout::Rect;

    fn file_app() -> (AppState, tokio::sync::mpsc::Sender<AppMessage>) {
        let root = std::env::temp_dir();
        let file = root.join("properties-target.flac");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = super::super::app::AppScreen::Browse;
        app.browse.current_dir = root;
        app.browse.entries = vec![BrowseEntry::new(file, "properties-target.flac".into(),
            EntryKind::AudioFile(crate::convert::formats::AudioFormat::Flac), 91, None)];
        app.browse.set_navigation_pane(BrowseNavigationPane::Files);
        (app, tokio::sync::mpsc::channel(8).0)
    }
    #[test]
    fn keyboard_tab_and_ok_close_without_leaking_accounting_subscription() {
        let (mut app, tx) = file_app();
        open_selected(&mut app, &tx);
        let ActiveOverlay::FolderProperties(state) = app.active_overlay.clone() else { panic!("not open"); };
        assert_eq!(state.focus, PropertiesFocus::Ok);
        assert!(state.request_id.is_none());
        handle_key(&mut app, *state, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &tx);
        let ActiveOverlay::FolderProperties(state) = app.active_overlay.clone() else { panic!("not open"); };
        assert_eq!(state.focus, PropertiesFocus::EditMetadata);
        handle_key(&mut app, *state, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &tx);
        assert!(matches!(app.active_overlay, ActiveOverlay::None));
    }
    #[test]
    fn directory_and_file_symlinks_do_not_subscribe_or_report_target_mtime() {
        let (mut app, tx) = file_app();
        let target_modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for (name, kind) in [
            ("album-link", EntryKind::Directory),
            ("track-link", EntryKind::OtherFile),
        ] {
            let path = app.browse.current_dir.join(name);
            app.browse.entries = vec![BrowseEntry::new_with_symlink(
                path.clone(), name.into(), kind, 50_000, Some(target_modified), true, false,
            )];
            open_for_path(&mut app, path.clone(), &tx);
            let ActiveOverlay::FolderProperties(state) = &app.active_overlay else { panic!("not open"); };
            assert_eq!(state.path, path);
            assert!(state.request_id.is_none(), "symlinks must not subscribe to a recursive scan");
            assert!(!app.directory_accounting.scan_pending(&path));
            assert!(matches!(state.subject, PropertiesSubject::File {
                is_symlink: true, modified: None, ..
            }), "the listing's timestamp can belong to the symlink target");
        }
    }

    #[test]
    fn tree_selected_link_uses_unfiltered_directory_snapshot() {
        let (mut app, tx) = file_app();
        let path = app.browse.current_dir.join("hidden-album-link");
        app.browse.entries.clear();
        app.browse.all_dirs = vec![BrowseEntry::new_with_symlink(
            path.clone(), "hidden-album-link".into(), EntryKind::Directory,
            1024, Some(SystemTime::UNIX_EPOCH), true, false,
        )];
        open_for_path(&mut app, path, &tx);
        let ActiveOverlay::FolderProperties(state) = &app.active_overlay else { panic!("not open"); };
        assert!(state.request_id.is_none());
        assert!(matches!(state.subject, PropertiesSubject::File { is_symlink: true, modified: None, .. }));
    }

    #[test]
    fn alt_m_and_title_double_click_toggle_same_properties_state() {
        let (mut app, tx) = file_app();
        open_selected(&mut app, &tx);
        let toggle = KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT);
        let ActiveOverlay::FolderProperties(state) = app.active_overlay.clone() else {
            panic!("Properties not open");
        };
        assert!(!state.maximized);
        handle_key(&mut app, *state, toggle, &tx);
        let ActiveOverlay::FolderProperties(state) = app.active_overlay.clone() else {
            panic!("Properties closed on Alt+M");
        };
        assert!(state.maximized);
        handle_key(&mut app, *state, toggle, &tx);
        let ActiveOverlay::FolderProperties(state) = &app.active_overlay else {
            panic!("Properties closed on second Alt+M");
        };
        assert!(!state.maximized);

        let hit = Rect::new(12, 4, 30, 1);
        app.button_map.record_button(TuiButton::PropertiesTitle, hit);
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left), column: 16, row: 4,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse(&mut app, click, &tx);
        let ActiveOverlay::FolderProperties(state) = &app.active_overlay else { panic!("closed"); };
        assert!(!state.maximized, "one click does not maximize");
        handle_mouse(&mut app, click, &tx);
        let ActiveOverlay::FolderProperties(state) = &app.active_overlay else { panic!("closed"); };
        assert!(state.maximized, "double click maximizes");
        handle_mouse(&mut app, click, &tx);
        handle_mouse(&mut app, click, &tx);
        let ActiveOverlay::FolderProperties(state) = &app.active_overlay else { panic!("closed"); };
        assert!(!state.maximized, "second double click restores");
        assert!(state.request_id.is_none(), "file Properties never requests a scan");
    }

    #[test]
    fn ordinary_entries_keep_existing_file_and_folder_behavior() {
        let (mut app, tx) = file_app();
        let modified = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let file = app.browse.entries[0].path.clone();
        app.browse.entries[0].modified = Some(modified);
        open_for_path(&mut app, file, &tx);
        let ActiveOverlay::FolderProperties(state) = &app.active_overlay else { panic!("not open"); };
        assert!(state.request_id.is_none());
        assert!(matches!(state.subject, PropertiesSubject::File {
            is_symlink: false, modified: Some(time), ..
        } if time == modified));

        let temporary = tempfile::tempdir().unwrap();
        let folder = temporary.path().join("album");
        std::fs::create_dir(&folder).unwrap();
        app.browse.entries = vec![BrowseEntry::new(
            folder.clone(), "album".into(), EntryKind::Directory, 0, None,
        )];
        open_for_path(&mut app, folder, &tx);
        let ActiveOverlay::FolderProperties(state) = &app.active_overlay else { panic!("not open"); };
        assert!(matches!(state.subject, PropertiesSubject::Folder));
        assert!(state.request_id.is_some(), "ordinary folders retain directory accounting");
    }

    #[test]
    fn mouse_actions_are_modal_and_hit_test_painted_rectangles() {
        let (mut app, tx) = file_app();
        open_selected(&mut app, &tx);
        app.button_map = ButtonRenderMap::new();
        app.button_map.record_button(TuiButton::PropertiesOk, Rect::new(20, 12, 6, 1));
        let click = |x, y| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left), column: x, row: y,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse(&mut app, click(1, 1), &tx);
        assert!(matches!(app.active_overlay, ActiveOverlay::FolderProperties(_)),
            "outside clicks must not leak through to Browse or dismiss the dialog");
        handle_mouse(&mut app, click(22, 12), &tx);
        assert!(matches!(app.active_overlay, ActiveOverlay::None));
    }
}
