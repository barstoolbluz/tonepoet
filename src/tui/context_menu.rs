//! Context menu: right-click / `m` keybinding opens a floating menu
//! with context-sensitive actions. Per-screen `build_*_menu` functions
//! produce filtered item lists; `execute_context_action` dispatches.
//! Flat menus for v1 — nested submenus deferred.

use std::path::PathBuf;
use tokio::sync::mpsc;

use super::app::*;
use super::browse::EntryKind;
use super::message::AppMessage;
use crate::convert::ConversionStatus;

// ── Data structures ─────────────────────────────────────────────────

/// A single item in a context menu.
#[derive(Debug, Clone)]
pub struct ContextMenuItem {
    pub label: String,
    pub action: ContextAction,
    /// Optional shortcut hint displayed right-aligned (e.g., "F2", ":rename").
    pub shortcut: Option<String>,
    /// Greyed-out items are visible but not selectable.
    pub enabled: bool,
}

/// An entry in the context menu -- either a clickable item or a visual
/// separator between groups.
#[derive(Debug, Clone)]
pub enum ContextMenuEntry {
    Item(ContextMenuItem),
    Separator,
}

/// Actions that can be triggered from the context menu. Each variant
/// maps to a specific operation dispatched by `execute_context_action`.
#[derive(Debug, Clone)]
pub enum ContextAction {
    // ── Browse screen ───────────────────────────────────────────────
    /// Queue the selected file(s) for review on the Convert screen.
    QueueSelection,
    /// Queue and start processing (`:Commit` equivalent).
    QueueAndStart,
    /// Open the selected file/directory (same as Enter on browse).
    OpenEntry,
    /// Rename the selected file (F2 / `:rename`).
    RenameEntry,
    /// Copy the full path of the selected entry to the clipboard.
    CopyPath(PathBuf),
    /// Refresh the browse listing.
    Refresh,
    /// Toggle hidden-file visibility.
    ToggleHidden,
    /// Cycle the sort field.
    CycleSortBy,
    /// Open the bookmarks overlay.
    OpenBookmarks,
    /// Add the current directory as a bookmark.
    BookmarkCurrentDir,

    // ── Convert screen ──────────────────────────────────────────────
    /// Open Browse to pick a source file.
    BrowseForSource,
    /// Open the BatchList expand overlay.
    ExpandBatch,
    /// Commit the current review to the queue (`:commit`).
    CommitQueue,
    /// Commit and start processing (`:Commit`).
    CommitAndStart,
    /// Load a named preset into the format pills.
    LoadPreset(String),

    // ── Queue screen ────────────────────────────────────────────────
    /// Start processing (`:go`).
    StartProcessing,
    /// Pause / resume conversions.
    TogglePause,
    /// Remove selected queue item(s).
    RemoveSelected,
    /// Retry failed items.
    RetryFailed,
    /// Clear completed items.
    ClearCompleted,
    /// Show item info / error detail.
    ShowItemInfo,

    // ── Global ──────────────────────────────────────────────────────
    /// Switch to a specific screen.
    GoToScreen(AppScreen),
}

// ── Menu builders ───────────────────────────────────────────────────

fn item(label: &str, action: ContextAction) -> ContextMenuEntry {
    ContextMenuEntry::Item(ContextMenuItem {
        label: label.to_string(),
        action,
        shortcut: None,
        enabled: true,
    })
}

fn item_with_shortcut(label: &str, action: ContextAction, shortcut: &str) -> ContextMenuEntry {
    ContextMenuEntry::Item(ContextMenuItem {
        label: label.to_string(),
        action,
        shortcut: Some(shortcut.to_string()),
        enabled: true,
    })
}

fn separator() -> ContextMenuEntry {
    ContextMenuEntry::Separator
}

/// Build the context menu for a right-click on a browse entry.
pub fn build_browse_entry_menu(app: &AppState) -> Vec<ContextMenuEntry> {
    let entry = match app.browse.selected_entry() {
        Some(e) => e,
        None => return Vec::new(),
    };

    let mut items = Vec::new();

    match &entry.kind {
        EntryKind::AudioFile(_) | EntryKind::Archive => {
            items.push(item_with_shortcut("Queue", ContextAction::QueueSelection, ":queue"));
            items.push(item_with_shortcut(
                "Queue + start",
                ContextAction::QueueAndStart,
                ":queue!",
            ));
            items.push(separator());
            items.push(item_with_shortcut("Open", ContextAction::OpenEntry, "Enter"));
            items.push(item_with_shortcut("Rename", ContextAction::RenameEntry, "F2"));
            items.push(separator());
            items.push(item("Copy path", ContextAction::CopyPath(entry.path.clone())));
        }
        EntryKind::Directory => {
            items.push(item_with_shortcut("Open", ContextAction::OpenEntry, "Enter"));
            items.push(item_with_shortcut("Rename", ContextAction::RenameEntry, "F2"));
            items.push(separator());
            items.push(item("Copy path", ContextAction::CopyPath(entry.path.clone())));
        }
        EntryKind::ParentDir => {
            items.push(item_with_shortcut("Go up", ContextAction::OpenEntry, "←"));
        }
        EntryKind::OtherFile => {
            items.push(item("Copy path", ContextAction::CopyPath(entry.path.clone())));
        }
    }

    items
}

/// Build the context menu for a right-click on empty space in the browse list.
pub fn build_browse_empty_menu(app: &AppState) -> Vec<ContextMenuEntry> {
    let _ = app; // used in future for conditional items
    vec![
        item("Refresh", ContextAction::Refresh),
        item_with_shortcut("Toggle hidden", ContextAction::ToggleHidden, "."),
        item_with_shortcut("Change sort", ContextAction::CycleSortBy, "s"),
        separator(),
        item_with_shortcut("Bookmarks", ContextAction::OpenBookmarks, "b"),
        item("Bookmark this dir", ContextAction::BookmarkCurrentDir),
    ]
}

/// Build the context menu for the Convert screen.
pub fn build_convert_menu(app: &AppState) -> Vec<ContextMenuEntry> {
    let mut items = Vec::new();

    if !app.convert.source.mode.is_empty() {
        items.push(item_with_shortcut("Commit", ContextAction::CommitQueue, ":commit"));
        items.push(item_with_shortcut(
            "Commit + start",
            ContextAction::CommitAndStart,
            ":Commit",
        ));
        items.push(separator());
    }

    if app.convert.source.mode.is_batch() {
        items.push(item_with_shortcut("Expand batch", ContextAction::ExpandBatch, ":expand"));
        items.push(separator());
    }

    items.push(item_with_shortcut(
        "Browse for source",
        ContextAction::BrowseForSource,
        ":browse",
    ));

    // Preset submenu (flat for v1 — just list available presets)
    let presets = super::presets::list_presets();
    if !presets.is_empty() {
        items.push(separator());
        for name in presets.iter().take(10) {
            items.push(item(
                &format!("Preset: {}", name),
                ContextAction::LoadPreset(name.clone()),
            ));
        }
        if presets.len() > 10 {
            items.push(item(
                &format!("... +{} more (use :preset)", presets.len() - 10),
                ContextAction::GoToScreen(AppScreen::Convert), // no-op, just informational
            ));
        }
    }

    items
}

/// Build the context menu for a right-click on a queue item.
pub fn build_queue_item_menu(app: &AppState) -> Vec<ContextMenuEntry> {
    let item_ref = app.items_snapshot.get(app.selected_index);

    let mut items = Vec::new();

    if let Some(qi) = item_ref {
        items.push(item_with_shortcut("Info", ContextAction::ShowItemInfo, "Enter"));

        match &qi.status {
            ConversionStatus::Failed { .. } => {
                items.push(item("Retry", ContextAction::RetryFailed));
            }
            ConversionStatus::Queued | ConversionStatus::NotConfigured => {
                items.push(item("Remove", ContextAction::RemoveSelected));
            }
            _ => {}
        }
    }

    items.push(separator());

    if app.processing_active {
        items.push(item_with_shortcut("Pause", ContextAction::TogglePause, "p"));
    } else {
        items.push(item_with_shortcut("Start", ContextAction::StartProcessing, "s"));
    }

    items.push(item("Clear completed", ContextAction::ClearCompleted));

    items
}

/// Build the context menu for empty space on the queue screen.
pub fn build_queue_empty_menu(app: &AppState) -> Vec<ContextMenuEntry> {
    let mut items = Vec::new();

    if app.processing_active {
        items.push(item_with_shortcut("Pause", ContextAction::TogglePause, "p"));
    } else {
        items.push(item_with_shortcut("Start all", ContextAction::StartProcessing, "s"));
    }

    items.push(item("Clear completed", ContextAction::ClearCompleted));

    items
}

// ── Action dispatch ─────────────────────────────────────────────────

/// Execute a context action. Delegates to existing command/action
/// functions where possible.
pub fn execute_context_action(
    app: &mut AppState,
    action: ContextAction,
    tx: &mpsc::Sender<AppMessage>,
) {
    match action {
        // ── Browse ──────────────────────────────────────────────────
        ContextAction::QueueSelection => {
            // Delegate to the existing :queue command path.
            let cmd = super::command::Command::Queue { preset: None };
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::QueueAndStart => {
            // Queue then immediately start: :queue, switch to Convert, :Commit.
            // For single-file quick path, just queue + start inline.
            let cmd = super::command::Command::Queue { preset: None };
            super::command::execute_command(app, cmd, tx);
            // If we landed on Convert (from :queue), auto-commit+start.
            if app.current_screen == AppScreen::Convert {
                let cmd = super::command::Command::Commit { start: true };
                super::command::execute_command(app, cmd, tx);
            }
        }
        ContextAction::OpenEntry => {
            // Simulate Enter on browse
            if app.current_screen == AppScreen::Browse {
                if let Some(entry) = app.browse.selected_entry() {
                    match &entry.kind {
                        EntryKind::Directory | EntryKind::ParentDir => {
                            app.browse.enter_selected();
                            app.browse.probe_current(tx);
                        }
                        _ => {
                            let path = entry.path.clone();
                            let target = app.browse.return_target;
                            super::keybindings::load_browse_selection_pub(app, path, target);
                        }
                    }
                }
            }
        }
        ContextAction::RenameEntry => {
            let cmd = super::command::Command::Rename(String::new());
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::CopyPath(path) => {
            // Best-effort clipboard copy. If clipboard isn't available,
            // set the path as a status message so the user can see it.
            app.set_status(format!("path: {}", path.display()));
        }
        ContextAction::Refresh => {
            if app.current_screen == AppScreen::Browse {
                app.browse.refresh();
                app.browse.probe_current(tx);
                app.set_status("refreshed");
            }
        }
        ContextAction::ToggleHidden => {
            if app.current_screen == AppScreen::Browse {
                app.browse.toggle_hidden();
                app.browse.probe_current(tx);
            }
        }
        ContextAction::CycleSortBy => {
            if app.current_screen == AppScreen::Browse {
                app.browse.cycle_sort_by();
                let msg = format!(
                    "Sort: {} {}",
                    app.browse.sort_by.label(),
                    app.browse.sort_dir.label()
                );
                app.set_status(msg);
            }
        }
        ContextAction::OpenBookmarks => {
            app.bookmarks.open_overlay();
        }
        ContextAction::BookmarkCurrentDir => {
            let path = app.browse.current_dir.clone();
            let name = super::bookmarks::BookmarksState::default_name_for_path(&path);
            app.bookmarks.add(name.clone(), path);
            app.set_status(format!("bookmark added: {}", name));
        }

        // ── Convert ─────────────────────────────────────────────────
        ContextAction::BrowseForSource => {
            let cmd = super::command::Command::Browse;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::ExpandBatch => {
            let cmd = super::command::Command::Expand;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::CommitQueue => {
            let cmd = super::command::Command::Commit { start: false };
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::CommitAndStart => {
            let cmd = super::command::Command::Commit { start: true };
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::LoadPreset(name) => {
            let cmd = super::command::Command::Preset(name);
            super::command::execute_command(app, cmd, tx);
        }

        // ── Queue ───────────────────────────────────────────────────
        ContextAction::StartProcessing => {
            let cmd = super::command::Command::Go;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::TogglePause => {
            if app.processing_active {
                if app.manager.is_paused() {
                    app.manager.resume_conversions();
                    app.set_status("Resumed conversions");
                } else {
                    app.manager.pause_conversions();
                    app.set_status("Paused conversions");
                }
            }
        }
        ContextAction::RemoveSelected => {
            let removed = app.manager.remove_selected();
            app.set_status(format!("Removed {} item(s)", removed));
        }
        ContextAction::RetryFailed => {
            // Mark failed items as Queued for retry. Release the lock
            // before calling set_status (which needs &mut app).
            let result = if let Ok(mut queue) = app.manager.queue.try_write() {
                let mut count = 0;
                for qi in queue.all_items_mut() {
                    if matches!(qi.status, ConversionStatus::Failed { .. }) {
                        qi.status = ConversionStatus::Queued;
                        count += 1;
                    }
                }
                Ok(count)
            } else {
                Err(())
            };
            match result {
                Ok(0) => app.set_status("no failed items to retry"),
                Ok(n) => app.set_status(format!("{} failed item(s) queued for retry", n)),
                Err(()) => app.set_status("retry: queue locked, try again"),
            }
        }
        ContextAction::ClearCompleted => {
            app.active_overlay = ActiveOverlay::Confirmation {
                message: "Clear all completed items?".to_string(),
                action: ConfirmAction::ClearCompleted,
            };
        }
        ContextAction::ShowItemInfo => {
            if let Some(qi) = app.items_snapshot.get(app.selected_index) {
                match &qi.status {
                    ConversionStatus::Failed { error } => {
                        app.active_overlay = ActiveOverlay::ErrorDetail {
                            item_id: qi.id.clone(),
                            error: error.clone(),
                        };
                    }
                    _ => {
                        app.active_overlay = ActiveOverlay::ItemInfo { item: qi.clone() };
                    }
                }
            }
        }

        // ── Global ──────────────────────────────────────────────────
        ContextAction::GoToScreen(screen) => {
            app.current_screen = screen;
            if screen == AppScreen::Browse {
                app.browse.probe_current(tx);
            }
        }
    }
}
