//! Folder Properties overlay controller. No filesystem or SQLite I/O here.
use std::path::PathBuf;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc::Sender;
use super::app::{ActiveOverlay, AppState};
use super::browse::BrowseNavigationPane;
use super::message::AppMessage;

#[derive(Debug, Clone)]
pub struct FolderPropertiesState {
    pub path: PathBuf,
    pub request_id: u64,
    pub scroll: usize,
    pub horizontal_scroll: usize,
    pub cancelled: bool,
}

pub fn target_from_selection(app: &AppState) -> Result<PathBuf, &'static str> {
    if app.browse.is_in_archive() { return Err("Folder Properties is unavailable inside archives"); }
    let target = if app.browse.navigation_pane == BrowseNavigationPane::Tree {
        app.browse.selected_tree_path()
    } else {
        let entry = app.browse.selected_entry().ok_or("No directory selected")?;
        if matches!(entry.kind, crate::convert::classify::EntryKind::ParentDir) {
            Some(app.browse.current_dir.clone())
        } else if entry.is_child_dir() {
            Some(entry.path.clone())
        } else {
            return Err("Folder Properties requires a real filesystem directory");
        }
    };
    let path = target.ok_or("No filesystem directory selected")?;
    if !path.is_absolute() { return Err("Folder Properties requires an absolute filesystem path"); }
    Ok(path)
}

pub fn open_for_path(app: &mut AppState, path: PathBuf, tx: &Sender<AppMessage>) {
    if app.current_screen != super::app::AppScreen::Browse || app.browse.is_in_archive() {
        app.set_status("Folder Properties is unavailable outside the filesystem Browse view");
        return;
    }
    if !path.is_absolute() {
        app.set_status("Folder Properties requires an absolute filesystem path");
        return;
    }
    // Opening another Properties dialog detaches that modal's subscription,
    // not the shared filesystem scan.
    if let ActiveOverlay::FolderProperties(previous) = &app.active_overlay {
        let previous_path = previous.path.clone();
        let previous_id = previous.request_id;
        app.directory_accounting.detach_explicit(&previous_path, previous_id);
    }
    let request_id = app.directory_accounting.request_explicit(path.clone(), false, tx);
    app.active_overlay = ActiveOverlay::FolderProperties(Box::new(FolderPropertiesState {
        path, request_id, scroll: 0, horizontal_scroll: 0, cancelled: false,
    }));
}

pub fn open_selected(app: &mut AppState, tx: &Sender<AppMessage>) {
    match target_from_selection(app) {
        Ok(path) => open_for_path(app, path, tx),
        Err(error) => app.set_status(error),
    }
}

pub fn handle_key(app: &mut AppState, state: FolderPropertiesState, key: KeyEvent, tx: &Sender<AppMessage>) {
    let mut state = state;
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => {
            app.directory_accounting.detach_explicit(&state.path, state.request_id);
            app.active_overlay = ActiveOverlay::None;
        }
        (KeyCode::Char('r' | 'R'), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            app.directory_accounting.detach_explicit(&state.path, state.request_id);
            state.request_id = app.directory_accounting.request_explicit(state.path.clone(), true, tx);
            state.scroll = 0;
            state.horizontal_scroll = 0;
            state.cancelled = false;
            app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state));
        }
        (KeyCode::Char('c' | 'C'), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            app.directory_accounting.detach_explicit(&state.path, state.request_id);
            state.cancelled = true;
            app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state));
        }
        (KeyCode::Left, _) => {
            state.horizontal_scroll = state.horizontal_scroll.saturating_sub(4);
            app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state));
        }
        (KeyCode::Right, _) => {
            state.horizontal_scroll = state.horizontal_scroll.saturating_add(4);
            app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state));
        }
        (KeyCode::Home, _) => {
            state.horizontal_scroll = 0;
            app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state));
        }
        (KeyCode::Up, _) => {
            state.scroll = state.scroll.saturating_sub(1);
            app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state));
        }
        (KeyCode::Down, _) => {
            state.scroll = state.scroll.saturating_add(1);
            app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state));
        }
        _ => { // The modal consumes Ctrl+P / Ctrl+Shift+P; no Browse paste leaks.
            app.active_overlay = ActiveOverlay::FolderProperties(Box::new(state));
        }
    }
}
