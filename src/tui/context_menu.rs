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

/// An entry in the context menu -- either a clickable item, a visual
/// separator, or a submenu that expands on selection.
#[derive(Debug, Clone)]
pub enum ContextMenuEntry {
    Item(ContextMenuItem),
    Separator,
    /// A submenu parent. Selecting it pushes the current menu onto the
    /// stack and replaces it with `children`. The label is shown with
    /// a "►" indicator.
    Submenu {
        label: String,
        children: Vec<ContextMenuEntry>,
    },
}

/// Actions that can be triggered from the context menu. Each variant
/// maps to a specific operation dispatched by `execute_context_action`.
#[derive(Debug, Clone)]
pub enum ContextAction {
    // ── Browse screen ───────────────────────────────────────────────
    /// Queue the selected file(s) for review on the Convert screen.
    QueueSelection,
    /// Queue with a specific preset loaded.
    QueueWithPreset(String),
    /// Queue and start processing (`:Commit` equivalent).
    QueueAndStart,
    /// Queue with a preset and auto-start.
    QueueAndStartWithPreset(String),
    /// Open the selected file/directory (same as Enter on browse).
    OpenEntry,
    /// Rename the selected file (F2 / `:rename`).
    RenameEntry,
    /// Move selected file(s) to the system trash.
    MoveToTrash,
    /// Copy the full path of the selected entry to the clipboard.
    CopyPath(PathBuf),
    /// Edit a metadata field on the selected audio file.
    EditMetadata(crate::tui::probe::MetadataField),
    /// Copy the selected file(s) to a destination (opens TextEdit picker).
    CopyTo,
    /// Move the selected file(s) to a destination (opens TextEdit picker).
    MoveTo,
    /// Refresh the browse listing.
    Refresh,
    /// Open the bulk rename wizard for selected audio files.
    BulkRename,
    /// Set a password for the selected archive (opens TextEdit prompt).
    SetArchivePassword,
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

/// Build the "Convert" submenu tree. The top level always shows the
/// quick-action items (Custom and Last Used for both queue and start
/// modes). Codec-grouped preset submenus appear below the separator
/// only when presets exist.
///
/// ```text
/// Convert ►
/// ├── Custom (queue)                 — current pill state, review
/// ├── Custom + start                 — current pill state, auto-start
/// ├── Last used (queue)  [FLAC]      — same as Custom, labelled with format
/// ├── Last used + start  [FLAC]
/// ├── ─────────────────────          (only if presets exist)
/// ├── FLAC ►
/// │   ├── foobar2k (queue)
/// │   ├── foobar2k + start
/// │   └── ...
/// ├── Opus ►
/// │   └── ...
/// ```
///
/// Reused by audio files, archives, and directories — the queue flow
/// expands directories into their audio file contents automatically.
fn build_convert_submenu(app: &AppState) -> ContextMenuEntry {
    let groups = super::presets::list_presets_by_format_db(&app.db);

    // Label "Last Used" with the current output format so the user
    // knows what they're getting without drilling in.
    let current_format = app.convert.format.format.selected_value().name();

    let mut children = vec![
        item("Custom (queue)", ContextAction::QueueSelection),
        item("Custom + start", ContextAction::QueueAndStart),
        item(
            &format!("Last used (queue)  [{}]", current_format),
            ContextAction::QueueSelection,
        ),
        item(
            &format!("Last used + start  [{}]", current_format),
            ContextAction::QueueAndStart,
        ),
    ];

    // Per-codec preset submenus: each codec that has presets gets a
    // submenu with queue/start pairs for each preset.
    if !groups.is_empty() {
        children.push(separator());

        for (fmt, names) in &groups {
            let codec_label = match fmt {
                Some(f) => f.name().to_string(),
                None => "Other".to_string(),
            };

            let mut preset_items: Vec<ContextMenuEntry> = Vec::new();
            for name in names {
                preset_items.push(item(
                    &format!("{} (queue)", name),
                    ContextAction::QueueWithPreset(name.clone()),
                ));
                preset_items.push(item(
                    &format!("{} + start", name),
                    ContextAction::QueueAndStartWithPreset(name.clone()),
                ));
            }

            children.push(ContextMenuEntry::Submenu {
                label: codec_label,
                children: preset_items,
            });
        }
    }

    ContextMenuEntry::Submenu {
        label: "Convert".to_string(),
        children,
    }
}

/// Build the "Edit metadata" submenu for audio files. Shows each
/// editable field as a submenu item (Title, Artist, Album, Genre, Year).
fn build_edit_metadata_submenu() -> ContextMenuEntry {
    use crate::tui::probe::MetadataField;

    let children: Vec<ContextMenuEntry> = MetadataField::all()
        .iter()
        .map(|&field| {
            item_with_shortcut(
                &format!("Edit {}", field.label()),
                ContextAction::EditMetadata(field),
                &format!(":e {}", field.label()),
            )
        })
        .collect();

    ContextMenuEntry::Submenu {
        label: "Edit metadata".to_string(),
        children,
    }
}

/// Build the context menu for a right-click on a browse entry.
pub fn build_browse_entry_menu(app: &AppState) -> Vec<ContextMenuEntry> {
    let entry = match app.browse.selected_entry() {
        Some(e) => e,
        None => return Vec::new(),
    };

    let mut items = Vec::new();

    match &entry.kind {
        EntryKind::AudioFile(_) => {
            items.push(build_convert_submenu(app));
            items.push(build_edit_metadata_submenu());
            items.push(separator());
            items.push(item_with_shortcut("Open", ContextAction::OpenEntry, "Enter"));
            items.push(item_with_shortcut("Rename", ContextAction::RenameEntry, "F2"));
            items.push(item_with_shortcut("Bulk Rename...", ContextAction::BulkRename, "R"));
            items.push(item_with_shortcut("Copy to...", ContextAction::CopyTo, ":cp"));
            items.push(item_with_shortcut("Move to...", ContextAction::MoveTo, ":mv"));
            items.push(item_with_shortcut("Move to Trash", ContextAction::MoveToTrash, ":del"));
            items.push(separator());
            items.push(item("Copy path", ContextAction::CopyPath(entry.path.clone())));
        }
        EntryKind::Archive => {
            items.push(build_convert_submenu(app));
            items.push(separator());
            items.push(item_with_shortcut("Open", ContextAction::OpenEntry, "Enter"));
            items.push(item_with_shortcut("Set Password...", ContextAction::SetArchivePassword, ":pw"));
            items.push(item_with_shortcut("Rename", ContextAction::RenameEntry, "F2"));
            items.push(item_with_shortcut("Copy to...", ContextAction::CopyTo, ":cp"));
            items.push(item_with_shortcut("Move to...", ContextAction::MoveTo, ":mv"));
            items.push(item_with_shortcut("Move to Trash", ContextAction::MoveToTrash, ":del"));
            items.push(separator());
            items.push(item("Copy path", ContextAction::CopyPath(entry.path.clone())));
        }
        EntryKind::Directory => {
            items.push(build_convert_submenu(app));
            items.push(separator());
            items.push(item_with_shortcut("Open", ContextAction::OpenEntry, "Enter"));
            items.push(item_with_shortcut("Rename", ContextAction::RenameEntry, "F2"));
            items.push(item_with_shortcut("Copy to...", ContextAction::CopyTo, ":cp"));
            items.push(item_with_shortcut("Move to...", ContextAction::MoveTo, ":mv"));
            items.push(item_with_shortcut("Move to Trash", ContextAction::MoveToTrash, ":del"));
            items.push(separator());
            items.push(item("Copy path", ContextAction::CopyPath(entry.path.clone())));
        }
        EntryKind::ParentDir => {
            items.push(item_with_shortcut("Go up", ContextAction::OpenEntry, "←"));
        }
        EntryKind::OtherFile => {
            items.push(item_with_shortcut("Rename", ContextAction::RenameEntry, "F2"));
            items.push(item_with_shortcut("Copy to...", ContextAction::CopyTo, ":cp"));
            items.push(item_with_shortcut("Move to...", ContextAction::MoveTo, ":mv"));
            items.push(item_with_shortcut("Move to Trash", ContextAction::MoveToTrash, ":del"));
            items.push(separator());
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

    // Presets grouped by codec — same tree structure as the browse
    // entry's Convert submenu, but here just for loading (not queuing).
    let groups = super::presets::list_presets_by_format_db(&app.db);
    if !groups.is_empty() {
        items.push(separator());
        for (fmt, names) in &groups {
            let codec_label = match fmt {
                Some(f) => format!("Presets: {}", f.name()),
                None => "Presets: Other".to_string(),
            };
            let preset_items: Vec<ContextMenuEntry> = names
                .iter()
                .map(|name| item(name, ContextAction::LoadPreset(name.clone())))
                .collect();
            items.push(ContextMenuEntry::Submenu {
                label: codec_label,
                children: preset_items,
            });
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
        ContextAction::QueueWithPreset(name) => {
            let cmd = super::command::Command::Queue { preset: Some(name) };
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::QueueAndStart => {
            // Queue then immediately start: :queue, switch to Convert, :Commit.
            let cmd = super::command::Command::Queue { preset: None };
            super::command::execute_command(app, cmd, tx);
            // If we landed on Convert (from :queue), auto-commit+start.
            if app.current_screen == AppScreen::Convert {
                let cmd = super::command::Command::Commit { start: true };
                super::command::execute_command(app, cmd, tx);
            }
        }
        ContextAction::QueueAndStartWithPreset(name) => {
            let cmd = super::command::Command::Queue { preset: Some(name) };
            super::command::execute_command(app, cmd, tx);
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
                            app.browse.probe_current_with_db(tx, Some(&app.db));
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
            let path_str = path.display().to_string();
            // OSC 52: copy to system clipboard via terminal escape sequence.
            // Works in iTerm2, WezTerm, kitty, Alacritty, foot, xterm, etc.
            let b64 = base64_encode(path_str.as_bytes());
            let osc = format!("\x1b]52;c;{}\x07", b64);
            let _ = std::io::Write::write_all(&mut std::io::stdout(), osc.as_bytes());
            let _ = std::io::Write::flush(&mut std::io::stdout());
            app.set_status(format!("Copied: {}", path_str));
        }
        ContextAction::EditMetadata(field) => {
            super::command::execute_edit_metadata_pub(app, field);
        }
        ContextAction::MoveToTrash => {
            let cmd = super::command::Command::Delete;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::SetArchivePassword => {
            let cmd = super::command::Command::Password;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::BulkRename => {
            // Reuse the same path as the R keybinding.
            let paths = super::command::collect_selection_for_file_ops(app);
            let audio_paths: Vec<std::path::PathBuf> = paths
                .into_iter()
                .filter(|p| {
                    app.browse.entries.iter().any(|e| {
                        e.path == *p
                            && matches!(e.kind, EntryKind::AudioFile(_))
                    })
                })
                .collect();
            super::keybindings::open_bulk_rename(app, audio_paths);
        }
        ContextAction::CopyTo => {
            let cmd = super::command::Command::Copy { dest: String::new(), force: false };
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::MoveTo => {
            let cmd = super::command::Command::Move { dest: String::new(), force: false };
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::Refresh => {
            if app.current_screen == AppScreen::Browse {
                app.browse.refresh();
                app.browse.probe_current_with_db(tx, Some(&app.db));
                app.set_status("refreshed");
            }
        }
        ContextAction::ToggleHidden => {
            if app.current_screen == AppScreen::Browse {
                app.browse.toggle_hidden();
                app.browse.probe_current_with_db(tx, Some(&app.db));
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
            app.bookmarks.add_with_db(name.clone(), path, &app.db);
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
            app.save_queue();
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
                app.browse.probe_current_with_db(tx, Some(&app.db));
            }
        }
    }
}

/// Minimal base64 encoder for OSC 52 clipboard (no crate dependency).
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((n >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[((n >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARS[(n & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}
