//! Async event loop: crossterm events + progress messages

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc;

use super::app::{ActiveOverlay, AppScreen, AppState, TextEditTarget};
use super::browse::EntryKind;
use super::draw::draw_ui;
use super::keybindings::{handle_key, handle_mouse};
use super::message::AppMessage;
use super::text_input::TextInputState;

/// Run the main TUI event loop
pub async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut AppState,
    tx: mpsc::Sender<AppMessage>,
    mut rx: mpsc::Receiver<AppMessage>,
) -> io::Result<()> {
    loop {
        // 1. Refresh items from the manager
        app.refresh_items();
        app.clamp_selection();
        app.clear_expired_status();
        check_pending_browse_rename(app);
        // Close browse-only overlays if the user has left the browse screen.
        if app.current_screen != AppScreen::Browse && app.bookmarks.overlay_open {
            app.bookmarks.close_overlay();
        }

        // 2. Render
        terminal.draw(|f| draw_ui(f, app))?;

        // 3. Check quit
        if app.should_quit {
            // Save queue before exiting
            app.manager.save_queue(app.config.conversion.persist_queue).ok();
            break;
        }

        // 4. Drain async messages
        while let Ok(msg) = rx.try_recv() {
            handle_message(app, msg);
        }

        // 5. Poll for crossterm events (100ms timeout for responsive UI updates)
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => handle_key(app, key, &tx),
                Event::Mouse(mouse) => handle_mouse(app, mouse, &tx),
                Event::Paste(text) => handle_paste(app, &text),
                Event::Resize(_, _) => {} // redraw handled by loop
                _ => {}
            }
        }
    }

    Ok(())
}

/// Check the pending-browse-rename timer. If the deadline has passed and we're
/// still on the browse screen with no overlay active, open the rename overlay
/// for the pending path. If the user has navigated away or opened another
/// overlay, silently drop the pending state.
fn check_pending_browse_rename(app: &mut AppState) {
    let (path, deadline) = match app.pending_browse_rename.as_ref() {
        Some(pr) => (pr.0.clone(), pr.1),
        None => return,
    };

    // Cancel if the user left the browse screen or has an overlay open already.
    if app.current_screen != AppScreen::Browse
        || !matches!(app.active_overlay, ActiveOverlay::None)
    {
        app.pending_browse_rename = None;
        return;
    }

    if std::time::Instant::now() < deadline {
        return;
    }

    // Look up the entry by path in the current view. If it's gone (filtered
    // out, deleted, or user navigated), drop silently.
    let entry_info = app
        .browse
        .entries
        .iter()
        .find(|e| e.path == path)
        .map(|e| (e.name.clone(), e.kind.clone()));

    app.pending_browse_rename = None;
    app.last_browse_click = None;

    match entry_info {
        Some((name, kind)) if !matches!(kind, EntryKind::ParentDir) => {
            app.active_overlay = ActiveOverlay::TextEdit {
                input: TextInputState::new(name),
                target: TextEditTarget::BrowseRename(path),
                label: "rename".to_string(),
            };
        }
        _ => {
            // Entry no longer visible or is ParentDir — silently drop.
        }
    }
}

/// Handle async messages from background tasks
fn handle_message(app: &mut AppState, msg: AppMessage) {
    match msg {
        AppMessage::ConversionProgress { item_id, status } => {
            app.manager.update_item_status(&item_id, status, 0.0);
        }
        AppMessage::ConversionComplete { completed, failed } => {
            app.processing_active = false;
            if failed > 0 {
                app.set_status(format!(
                    "Conversion done: {} completed, {} failed",
                    completed, failed
                ));
            } else {
                app.set_status(format!("Conversion complete: {} files", completed));
            }
            // Save queue after completion
            app.manager.save_queue(app.config.conversion.persist_queue).ok();
        }
        AppMessage::ConversionError { message } => {
            app.processing_active = false;
            app.set_status(format!("Error: {}", message));
        }
        AppMessage::FilesScanned { paths } => {
            let mut options = crate::convert::ConversionOptions::default();
            options.append_lineage_to_comment = app.config.conversion.append_lineage_to_comment;
            options.write_log_file = app.config.conversion.write_log_file;
            options.generate_cue_files = app.config.conversion.generate_cue_files;
            options.cue_generation_mode = app.config.conversion.cue_generation_mode.clone();

            let mut count = 0;
            for path in paths {
                if app.manager.add_file_blocking(path, options.clone()).is_ok() {
                    count += 1;
                }
            }
            app.set_status(format!("Added {} files", count));
            app.manager.save_queue(app.config.conversion.persist_queue).ok();
        }
        AppMessage::StatusMessage(msg) => {
            app.set_status(msg);
        }
        AppMessage::Redraw => {} // Just triggers a redraw via the loop
        AppMessage::AudioProbeComplete { path, result } => {
            app.browse.probe_pending.remove(&path);
            match *result {
                Ok(info) => {
                    // Clone for the browse cache (shared via Arc).
                    app.browse
                        .probe_cache
                        .insert(path.clone(), Some(std::sync::Arc::new(info.clone())));

                    // Phase 6g: route to the Convert source pane if
                    // we're in Batch mode and this path matches the
                    // current cursor, or in Single mode and this path
                    // matches the loaded file. This completes the
                    // async-probe pipeline for `move_batch_cursor` and
                    // `remove_batch_at_cursor`.
                    let super::browse::CachedInfo {
                        source: probed_info,
                        metadata: probed_metadata,
                    } = info;
                    match &mut app.convert.source.mode {
                        super::app::SourceMode::Batch {
                            paths,
                            cursor,
                            cursor_info,
                            cursor_metadata,
                            ..
                        } => {
                            if paths.get(*cursor).map(|p| p == &path).unwrap_or(false) {
                                *cursor_info = Some(probed_info);
                                *cursor_metadata = probed_metadata;
                            }
                        }
                        super::app::SourceMode::Single {
                            path: single_path,
                            info: info_slot,
                            metadata: metadata_slot,
                        } => {
                            if single_path == &path {
                                *info_slot = Some(probed_info);
                                *metadata_slot = probed_metadata;
                            }
                        }
                        super::app::SourceMode::Empty => {}
                    }
                }
                Err(_) => {
                    // Cache the failure so we don't retry; renderer falls back
                    // to basic info (path + size) when the value is None.
                    app.browse.probe_cache.insert(path, None);
                }
            }
        }
        AppMessage::DirStatsComplete { path, stats } => {
            app.browse.dir_stats_pending.remove(&path);
            app.browse
                .dir_stats_cache
                .insert(path, std::sync::Arc::new(stats));
        }
    }
}

/// Handle a bracketed paste event. When the BulkRename overlay is active,
/// multi-line paste replaces the template-derived targets line-by-line.
/// In text input overlays, the pasted text is inserted at the cursor.
fn handle_paste(app: &mut AppState, text: &str) {
    match &app.active_overlay {
        ActiveOverlay::BulkRename(_) => {
            // Take the state out of the overlay so we can mutate it.
            let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
            if let ActiveOverlay::BulkRename(mut state) = overlay {
                let lines: Vec<&str> = text.lines().collect();
                let op_count = state.plan.ops.len();
                let mut applied = 0usize;
                for (i, line) in lines.iter().take(op_count).enumerate() {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match crate::tui::rename_plan::sanitize_path(trimmed) {
                        Ok(sanitized) => {
                            state.plan.ops[i].target_relative = sanitized;
                            applied += 1;
                        }
                        Err(_) => {
                            // Skip invalid lines; keep the template-derived name.
                        }
                    }
                }
                crate::tui::rename_plan::validate_plan(&mut state.plan);
                app.set_status(&format!(
                    "Pasted {} name{}", applied, if applied == 1 { "" } else { "s" }
                ));
                // Switch focus to list so the user can review.
                state.focus = super::app::BulkRenameFocus::List;
                app.active_overlay = ActiveOverlay::BulkRename(state);
            }
        }
        // For text input overlays, insert the first line at the cursor.
        ActiveOverlay::TextEdit { .. }
        | ActiveOverlay::CommandInput { .. }
        | ActiveOverlay::FileInput { .. } => {
            let first_line = text.lines().next().unwrap_or("");
            // Insert each character at the cursor via the text input's insert_char.
            let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
            match overlay {
                ActiveOverlay::TextEdit { mut input, target, label } => {
                    for c in first_line.chars() {
                        input.insert_char(c);
                    }
                    app.active_overlay = ActiveOverlay::TextEdit { input, target, label };
                }
                ActiveOverlay::CommandInput { mut input, .. } => {
                    for c in first_line.chars() {
                        input.insert_char(c);
                    }
                    // Clear completion — pasted text invalidates candidates.
                    app.active_overlay = ActiveOverlay::CommandInput { input, completion: None };
                }
                ActiveOverlay::FileInput { mut input } => {
                    for c in first_line.chars() {
                        input.insert_char(c);
                    }
                    app.active_overlay = ActiveOverlay::FileInput { input };
                }
                other => {
                    app.active_overlay = other;
                }
            }
        }
        _ => {
            // Paste ignored outside text-entry contexts.
        }
    }
}
