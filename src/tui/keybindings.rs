//! Key event dispatch by screen/focus

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tokio::sync::mpsc;

use crate::convert::{ConversionOptions, ConversionStatus};
use super::app::*;
use super::button_map::TuiButton;
use super::message::AppMessage;

/// Handle a key event, dispatching to the appropriate screen handler
pub fn handle_key(app: &mut AppState, key: KeyEvent, tx: &mpsc::Sender<AppMessage>) {
    // Any keyboard activity cancels a pending browse rename (user moved on).
    app.pending_browse_rename = None;

    // Handle preset overlay first (uses its own flag, not ActiveOverlay)
    if app.preset.overlay_open {
        handle_preset_overlay_key(app, key);
        return;
    }

    // Recent files overlay (independent flag, global)
    if app.recent.overlay_open {
        handle_recent_overlay_key(app, key);
        return;
    }

    // Bookmarks overlay (independent flag, browse-only)
    if app.bookmarks.overlay_open {
        handle_bookmarks_overlay_key(app, key, tx);
        return;
    }

    // Handle other overlay inputs
    if !matches!(app.active_overlay, ActiveOverlay::None) {
        handle_overlay_key(app, key, tx);
        return;
    }

    // Browse filter input preempts global keys: while typing in the filter,
    // characters like `q`, `1`-`5`, `:` go to the input, not to global handlers.
    if app.current_screen == AppScreen::Browse && app.browse.filter_input.is_some() {
        handle_browse_filter_key(app, key, tx);
        return;
    }

    // Global keys (except in Wizard mode)
    if app.current_screen != AppScreen::Wizard {
        match (key.code, key.modifiers) {
            // Quit is ONLY available via `:q` / `:quit` command mode — no bare-letter
            // quit key to prevent accidental exits.
            (KeyCode::Char('1'), KeyModifiers::NONE) => {
                app.current_screen = AppScreen::Browse;
                app.browse.probe_current(tx);
                return;
            }
            (KeyCode::Char('2'), KeyModifiers::NONE) => {
                app.current_screen = AppScreen::Library;
                return;
            }
            (KeyCode::Char('3'), KeyModifiers::NONE) => {
                app.current_screen = AppScreen::Convert;
                return;
            }
            (KeyCode::Char('4'), KeyModifiers::NONE) => {
                app.current_screen = AppScreen::Queue;
                return;
            }
            (KeyCode::Char('5'), KeyModifiers::NONE) => {
                app.current_screen = AppScreen::Config;
                return;
            }
            // Command mode
            (KeyCode::Char(':'), KeyModifiers::SHIFT) | (KeyCode::Char(':'), KeyModifiers::NONE) => {
                app.active_overlay = ActiveOverlay::CommandInput {
                    input: super::text_input::TextInputState::empty(),
                    completion: None,
                };
                return;
            }
            // Context menu (keyboard alternative to right-click)
            (KeyCode::Char('m'), KeyModifiers::NONE) => {
                // Build context menu for the current screen and cursor.
                // Origin is roughly the center of the screen (since there's
                // no mouse position available for keyboard invocation).
                let area = crossterm::terminal::size()
                    .map(|(w, h)| (w / 3, h / 3))
                    .unwrap_or((20, 10));
                open_context_menu(app, area.0, area.1);
                return;
            }
            _ => {}
        }
    }

    // Esc handling
    if key.code == KeyCode::Esc {
        match app.current_screen {
            AppScreen::Wizard => {
                if let Some(wizard) = &mut app.wizard {
                    wizard.handle_key(key);
                    if wizard.should_exit {
                        app.wizard = None;
                        app.wizard_mouse_areas = None;
                        app.current_screen = AppScreen::Convert;
                    }
                }
                return;
            }
            AppScreen::Library | AppScreen::Config => {
                // Esc from a placeholder/settings screen returns to the user's
                // configured default screen (home). Default: Browse.
                app.current_screen =
                    AppScreen::from_config_name(&app.config.ui.default_screen);
                if app.current_screen == AppScreen::Browse {
                    app.browse.probe_current(tx);
                }
                return;
            }
            AppScreen::Browse => {
                // Let handle_browse_key handle Esc — it clears multi-selection first
            }
            AppScreen::Queue => {
                // Esc from Queue returns to the user's configured default
                // screen (home). Default: Browse.
                app.current_screen =
                    AppScreen::from_config_name(&app.config.ui.default_screen);
                if app.current_screen == AppScreen::Browse {
                    app.browse.probe_current(tx);
                }
                return;
            }
            AppScreen::Convert => {
                // If we arrived via :queue from another screen (previous_screen
                // is set), Esc cancels the batch review and returns to origin.
                // Overlays have already been dispatched earlier in handle_key,
                // so at this point Convert has the key exclusively.
                if app.previous_screen.is_some() {
                    app.convert.source.mode = SourceMode::Empty;
                    app.convert.metadata = MetadataState::default();
                    let origin = app
                        .previous_screen
                        .take()
                        .unwrap_or(AppScreen::Browse);
                    app.current_screen = origin;
                    if origin == AppScreen::Browse {
                        app.browse.probe_current(tx);
                    }
                    app.set_status("cancelled");
                    return;
                }
                // No pending batch — fall through to handle_convert_key.
            }
        }
    }

    // Screen-specific handling
    match app.current_screen {
        AppScreen::Convert => handle_convert_key(app, key, tx),
        AppScreen::Browse => handle_browse_key(app, key, tx),
        AppScreen::Queue => handle_queue_key(app, key, tx),
        AppScreen::Config => handle_config_key(app, key),
        AppScreen::Wizard => handle_wizard_key(app, key),
        _ => {} // placeholder screens
    }
}

// ── Config screen keybindings ────────────────────────────────────────

fn handle_config_key(app: &mut AppState, key: KeyEvent) {
    match (key.code, key.modifiers) {
        // Tab toggles focus between settings and keychain panes.
        (KeyCode::Tab, KeyModifiers::NONE) | (KeyCode::BackTab, KeyModifiers::SHIFT) => {
            app.keychain.focused = !app.keychain.focused;
        }
        _ => {}
    }

    if !app.keychain.focused {
        return;
    }

    // Keychain-focused keys.
    let total = app.keychain.passwords.len();
    match (key.code, key.modifiers) {
        (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE) => {
            if app.keychain.selected > 0 {
                app.keychain.selected -= 1;
            }
        }
        (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE) => {
            if app.keychain.selected + 1 < total {
                app.keychain.selected += 1;
            }
        }
        // Toggle password visibility.
        (KeyCode::Char('v'), KeyModifiers::NONE) => {
            app.keychain.reveal = !app.keychain.reveal;
        }
        // Add a new password.
        (KeyCode::Char('a'), KeyModifiers::NONE) => {
            app.active_overlay = ActiveOverlay::TextEdit {
                input: super::text_input::TextInputState::empty(),
                target: TextEditTarget::KeychainAdd,
                label: "new password".to_string(),
            };
        }
        // Delete selected password.
        (KeyCode::Char('d'), KeyModifiers::NONE) => {
            if total > 0 {
                match super::keychain::remove_password(app.keychain.selected) {
                    Ok(()) => {
                        app.keychain.reload();
                        app.set_status("Password removed");
                    }
                    Err(e) => app.set_status(&format!("Remove failed: {}", e)),
                }
            }
        }
        _ => {}
    }
}

// ── Convert screen keybindings ───────────────────────────────────────

fn handle_convert_key(app: &mut AppState, key: KeyEvent, _tx: &mpsc::Sender<AppMessage>) {
    match (key.code, key.modifiers) {
        // Tab between panes
        (KeyCode::Tab, KeyModifiers::NONE) => {
            app.convert.focus = app.convert.focus.next();
        }
        (KeyCode::BackTab, KeyModifiers::SHIFT) => {
            app.convert.focus = app.convert.focus.prev();
        }

        // Within Format pane: Up/Down moves between pill rows
        (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE) if app.convert.focus == ConvertFocus::Format => {
            app.convert.format.field_focus = app.convert.format.field_focus.prev();
        }
        (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE) if app.convert.focus == ConvertFocus::Format => {
            app.convert.format.field_focus = app.convert.format.field_focus.next();
        }

        // Within Format pane: Left/Right changes pill selection
        (KeyCode::Left | KeyCode::Char('h'), KeyModifiers::NONE) if app.convert.focus == ConvertFocus::Format => {
            let was_format = app.convert.format.field_focus == FormatField::Format;
            app.convert.format.focused_pill_mut().select_prev();
            if was_format {
                app.convert.format.apply_format_constraints();
            }
            app.preset.mark_modified();
        }
        (KeyCode::Right | KeyCode::Char('l'), KeyModifiers::NONE) if app.convert.focus == ConvertFocus::Format => {
            let was_format = app.convert.format.field_focus == FormatField::Format;
            app.convert.format.focused_pill_mut().select_next();
            if was_format {
                app.convert.format.apply_format_constraints();
            }
            app.preset.mark_modified();
        }

        // Within Output Options pane: Up/Down moves between fields
        (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE) if app.convert.focus == ConvertFocus::OutputOptions => {
            app.convert.output_options.field_focus = app.convert.output_options.field_focus.prev();
        }
        (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE) if app.convert.focus == ConvertFocus::OutputOptions => {
            app.convert.output_options.field_focus = app.convert.output_options.field_focus.next();
        }

        // Within Output Options: Left/Right on merge mode pill
        (KeyCode::Left | KeyCode::Char('h'), KeyModifiers::NONE) if app.convert.focus == ConvertFocus::OutputOptions
            && app.convert.output_options.field_focus == OutputOptionsField::MergeMode => {
            app.convert.output_options.merge.select_prev();
            app.preset.mark_modified();
        }
        (KeyCode::Right | KeyCode::Char('l'), KeyModifiers::NONE) if app.convert.focus == ConvertFocus::OutputOptions
            && app.convert.output_options.field_focus == OutputOptionsField::MergeMode => {
            app.convert.output_options.merge.select_next();
            app.preset.mark_modified();
        }

        // Source pane default action: in Batch mode open the BatchList
        // expand overlay (view/manage the file list); in Single/Empty
        // mode open FileInput to edit the source path.
        // Previously `e`/Enter in batch mode opened FileInput with the
        // cursor path pre-filled, which would silently replace the
        // whole batch with a single file if the user committed.
        (KeyCode::Char('e'), KeyModifiers::NONE) | (KeyCode::Enter, KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::Source =>
        {
            if app.convert.source.mode.is_batch() {
                app.active_overlay = ActiveOverlay::BatchList { scroll: 0 };
            } else {
                let initial = app
                    .convert
                    .source
                    .mode
                    .current_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                app.active_overlay = ActiveOverlay::FileInput {
                    input: super::text_input::TextInputState::new(initial),
                };
            }
        }

        // Advanced toggle (stub)
        (KeyCode::Char('a'), KeyModifiers::NONE) => {
            match app.convert.focus {
                ConvertFocus::Source => {
                    app.convert.source.advanced_open = !app.convert.source.advanced_open;
                }
                ConvertFocus::Metadata => {
                    app.convert.metadata.advanced_open = !app.convert.metadata.advanced_open;
                }
                ConvertFocus::Format => {
                    app.convert.format.advanced_open = !app.convert.format.advanced_open;
                }
                ConvertFocus::OutputOptions => {
                    app.convert.output_options.advanced_open = !app.convert.output_options.advanced_open;
                }
            }
        }

        // Commit is now command-mode only: `:commit` (enqueue) or
        // `:Commit` (enqueue + start). No keyboard shortcuts — consistent
        // with the vi-style philosophy and the "no back door" invariant.
        // Esc cancels batch review (handled in handle_key top-level).

        // Open presets overlay
        (KeyCode::Char('p'), KeyModifiers::NONE) => {
            app.preset.overlay_list = super::presets::list_presets();
            app.preset.overlay_selected = 0;
            app.preset.naming_input = None;
            app.preset.overlay_open = true;
        }

        // Save preset
        (KeyCode::Char('s'), KeyModifiers::NONE) => {
            if let Some(name) = &app.preset.active_preset.clone() {
                let preset = super::presets::TuiPreset::from_pill_state(
                    name,
                    &app.convert.format,
                    &app.convert.output_options,
                );
                match super::presets::save_preset(&preset) {
                    Ok(_) => {
                        app.preset.modified = false;
                        app.set_status(format!("Saved preset: {}", name));
                    }
                    Err(e) => app.set_status(format!("Save failed: {}", e)),
                }
            } else {
                // No active preset — open overlay in naming mode
                app.preset.overlay_list = super::presets::list_presets();
                app.preset.overlay_selected = 0;
                app.preset.naming_input = Some(super::text_input::TextInputState::empty());
                app.preset.overlay_open = true;
            }
        }

        _ => {}
    }
}

// ── Preset overlay keybindings ────────────────────────────────────────

fn handle_preset_overlay_key(app: &mut AppState, key: KeyEvent) {
    // If in naming mode, handle text input
    if let Some(input) = &mut app.preset.naming_input {
        match key.code {
            KeyCode::Enter => {
                let name = input.text.trim().to_string();
                if name.is_empty() {
                    app.preset.naming_input = None;
                    return;
                }
                let preset = super::presets::TuiPreset::from_pill_state(
                    &name,
                    &app.convert.format,
                    &app.convert.output_options,
                );
                match super::presets::save_preset(&preset) {
                    Ok(_) => {
                        app.preset.active_preset = Some(name.clone());
                        app.preset.modified = false;
                        app.preset.naming_input = None;
                        app.preset.overlay_open = false;
                        app.set_status(format!("Saved preset: {}", name));
                    }
                    Err(e) => {
                        app.preset.naming_input = None;
                        app.set_status(format!("Save failed: {}", e));
                    }
                }
            }
            KeyCode::Esc => {
                app.preset.naming_input = None;
            }
            _ => {
                super::text_input::handle_text_input_key(input, &key);
            }
        }
        return;
    }

    // Normal overlay navigation
    match key.code {
        KeyCode::Esc => {
            app.preset.overlay_open = false;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if app.preset.overlay_selected > 0 {
                app.preset.overlay_selected -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if app.preset.overlay_selected + 1 < app.preset.overlay_list.len() {
                app.preset.overlay_selected += 1;
            }
        }
        KeyCode::Enter => {
            // Load selected preset
            if let Some(name) = app.preset.overlay_list.get(app.preset.overlay_selected).cloned() {
                match super::presets::load_preset(&name) {
                    Ok(preset) => {
                        preset.apply_to_pills(
                            &mut app.convert.format,
                            &mut app.convert.output_options,
                        );
                        app.preset.active_preset = Some(name.clone());
                        app.preset.modified = false;
                        app.preset.overlay_open = false;
                        app.set_status(format!("Loaded preset: {}", name));
                    }
                    Err(e) => {
                        app.set_status(format!("Load failed: {}", e));
                    }
                }
            }
        }
        KeyCode::Char('n') => {
            // Save as new preset
            app.preset.naming_input = Some(super::text_input::TextInputState::empty());
        }
        KeyCode::Char('d') => {
            // Duplicate selected preset
            if let Some(name) = app.preset.overlay_list.get(app.preset.overlay_selected).cloned() {
                match super::presets::load_preset(&name) {
                    Ok(mut preset) => {
                        let base = format!("{}-copy", name);
                        let new_name = super::presets::find_unique_preset_name(
                            &base,
                            &app.preset.overlay_list,
                        );
                        preset.name = new_name;
                        match super::presets::save_preset(&preset) {
                            Ok(_) => {
                                let saved_name = preset.name.clone();
                                app.preset.overlay_list = super::presets::list_presets();
                                app.set_status(format!("Duplicated: {} → {}", name, saved_name));
                            }
                            Err(e) => app.set_status(format!("Duplicate failed: {}", e)),
                        }
                    }
                    Err(e) => app.set_status(format!("Load failed: {}", e)),
                }
            }
        }
        KeyCode::Char('x') => {
            // Delete selected preset
            if let Some(name) = app.preset.overlay_list.get(app.preset.overlay_selected).cloned() {
                match super::presets::delete_preset(&name) {
                    Ok(_) => {
                        // If we deleted the active preset, clear it
                        if app.preset.active_preset.as_deref() == Some(&name) {
                            app.preset.active_preset = None;
                            app.preset.modified = false;
                        }
                        app.preset.overlay_list = super::presets::list_presets();
                        // Clamp selection
                        if app.preset.overlay_selected >= app.preset.overlay_list.len()
                            && !app.preset.overlay_list.is_empty()
                        {
                            app.preset.overlay_selected = app.preset.overlay_list.len() - 1;
                        }
                        app.set_status(format!("Deleted preset: {}", name));
                    }
                    Err(e) => app.set_status(format!("Delete failed: {}", e)),
                }
            }
        }
        _ => {}
    }
}

// ── Browse screen keybindings ────────────────────────────────────────

fn handle_browse_key(app: &mut AppState, key: KeyEvent, tx: &mpsc::Sender<AppMessage>) {
    use super::browse::EntryKind;

    // Track whether selection may have changed; if so, probe the new selection.
    let mut selection_may_have_changed = false;

    match (key.code, key.modifiers) {
        // Navigation
        (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE) => {
            app.browse.move_up();
            selection_may_have_changed = true;
        }
        (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE) => {
            app.browse.move_down();
            selection_may_have_changed = true;
        }
        (KeyCode::Home, _) | (KeyCode::Char('g'), KeyModifiers::NONE) => {
            app.browse.move_top();
            selection_may_have_changed = true;
        }
        (KeyCode::End, _) | (KeyCode::Char('G'), KeyModifiers::SHIFT) => {
            app.browse.move_bottom();
            selection_may_have_changed = true;
        }
        (KeyCode::PageUp, _) => {
            app.browse.page_up();
            selection_may_have_changed = true;
        }
        (KeyCode::PageDown, _) => {
            app.browse.page_down();
            selection_may_have_changed = true;
        }

        // Go up (parent directory)
        (KeyCode::Left | KeyCode::Char('h'), KeyModifiers::NONE) | (KeyCode::Backspace, _) => {
            app.browse.go_parent();
            selection_may_have_changed = true;
        }

        // Enter directory or select file
        (KeyCode::Right | KeyCode::Char('l'), KeyModifiers::NONE) => {
            if let Some(entry) = app.browse.selected_entry() {
                if entry.is_dir() {
                    app.browse.enter_selected();
                    selection_may_have_changed = true;
                }
            }
        }
        (KeyCode::Enter, KeyModifiers::NONE) => {
            if let Some(entry) = app.browse.selected_entry() {
                match &entry.kind {
                    EntryKind::Directory | EntryKind::ParentDir => {
                        app.browse.enter_selected();
                        selection_may_have_changed = true;
                    }
                    EntryKind::AudioFile(_) | EntryKind::Archive => {
                        let path = entry.path.clone();
                        let target = app.browse.return_target;
                        load_browse_selection(app, path, target);
                    }
                    EntryKind::OtherFile => {
                        app.set_status("Not an audio file");
                    }
                }
            }
        }

        // Toggle multi-select (for audio files only)
        (KeyCode::Char(' '), KeyModifiers::NONE) => {
            app.browse.toggle_selection();
            app.browse.move_down();
            selection_may_have_changed = true;
        }

        // Toggle hidden files
        (KeyCode::Char('.'), KeyModifiers::NONE) => {
            app.browse.toggle_hidden();
            selection_may_have_changed = true;
        }

        // Open the live text-filter input. Match both NONE and SHIFT modifiers
        // because some terminals/layouts report `/` as a shifted char.
        (KeyCode::Char('/'), KeyModifiers::NONE) | (KeyCode::Char('/'), KeyModifiers::SHIFT) => {
            app.browse.open_filter_input();
        }

        // Esc escalation: multi-selection → text filter → return to convert
        (KeyCode::Esc, _) => {
            if !app.browse.multi_selected.is_empty() {
                app.browse.clear_multi_selection();
            } else if !app.browse.filter_text.is_empty() {
                app.browse.clear_filter();
                selection_may_have_changed = true;
            }
            // Browse is home — Esc with nothing to clear is a no-op.
        }

        // Bulk rename: R opens the rename wizard for selected audio files.
        (KeyCode::Char('R'), KeyModifiers::SHIFT) => {
            let paths = super::command::collect_selection_for_file_ops(app);
            // Filter to audio files only (directories/other files can't be renamed via template).
            let audio_paths: Vec<std::path::PathBuf> = paths
                .into_iter()
                .filter(|p| {
                    app.browse.entries.iter().any(|e| {
                        e.path == *p
                            && matches!(
                                e.kind,
                                super::browse::EntryKind::AudioFile(_)
                            )
                    })
                })
                .collect();
            open_bulk_rename(app, audio_paths);
        }

        _ => {}
    }

    if selection_may_have_changed {
        app.browse.probe_current(tx);
    }
}

/// Hybrid dispatcher used while the live text filter input is open.
/// Arrow keys / page keys navigate the filtered list; everything else is
/// fed into the text input. Enter commits, Esc cancels (restores prior filter).
fn handle_browse_filter_key(app: &mut AppState, key: KeyEvent, tx: &mpsc::Sender<AppMessage>) {
    use super::text_input::handle_text_input_key;

    match (key.code, key.modifiers) {
        (KeyCode::Enter, _) => {
            app.browse.close_filter_input(true);
            app.browse.probe_current(tx);
        }
        (KeyCode::Esc, _) => {
            app.browse.close_filter_input(false);
            app.browse.probe_current(tx);
        }
        // List navigation while filter input is open
        (KeyCode::Up, _) => {
            app.browse.move_up();
            app.browse.probe_current(tx);
        }
        (KeyCode::Down, _) => {
            app.browse.move_down();
            app.browse.probe_current(tx);
        }
        (KeyCode::PageUp, _) => {
            app.browse.page_up();
            app.browse.probe_current(tx);
        }
        (KeyCode::PageDown, _) => {
            app.browse.page_down();
            app.browse.probe_current(tx);
        }
        // Everything else: feed to the text input, then re-apply view
        _ => {
            if let Some(input) = &mut app.browse.filter_input {
                if handle_text_input_key(input, &key) {
                    app.browse.update_filter_from_input();
                    app.browse.probe_current(tx);
                }
            }
        }
    }
}

/// Load the selected browse entry based on the return target
/// Load the selected browse entry into the Convert source pane based on
/// `target`. Public because it's called by `context_menu::execute_context_action`
/// for the `OpenEntry` action as well.
pub fn load_browse_selection_pub(
    app: &mut AppState,
    path: std::path::PathBuf,
    target: super::browse::BrowseReturnTarget,
) {
    load_browse_selection(app, path, target);
}

fn load_browse_selection(
    app: &mut AppState,
    path: std::path::PathBuf,
    target: super::browse::BrowseReturnTarget,
) {
    use super::browse::BrowseReturnTarget;

    match target {
        BrowseReturnTarget::ConvertSource | BrowseReturnTarget::None => {
            // Probe and load into source pane
            match crate::tui::probe::probe_audio(&path) {
                Ok(info) => {
                    let metadata = crate::tui::probe::read_metadata(&path)
                        .unwrap_or_default();
                    app.convert.metadata.title = metadata.title.clone();
                    app.convert.metadata.artist = metadata.artist.clone();
                    app.convert.metadata.album = metadata.album.clone();
                    app.convert.metadata.genre = metadata.genre.clone();
                    app.convert.metadata.year = metadata.year.clone();
                    // Browse Enter loads a single file — abandon any
                    // pending batch from a previous :queue.
                    app.convert.source.mode = SourceMode::Single {
                        path: path.clone(),
                        info: Some(info),
                        metadata,
                    };
                    app.set_status(format!(
                        "Loaded: {}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    app.current_screen = AppScreen::Convert;
                    // Clear the return target so subsequent browse visits don't
                    // auto-load into the source pane.
                    app.browse.return_target = BrowseReturnTarget::None;
                    // Record this file in the recent-files history.
                    app.recent.record_use(&path);
                }
                Err(e) => {
                    app.set_status(format!("Probe error: {}", e));
                }
            }
        }
        BrowseReturnTarget::ConvertQueue => {
            // Add all multi-selected files (or just this one) to the queue
            let mut paths_to_add = app.browse.multi_selected.clone();
            if paths_to_add.is_empty() {
                paths_to_add.push(path);
            }
            let mut count = 0;
            let options = crate::convert::ConversionOptions::default();
            for p in paths_to_add {
                // Resolve archive password:
                // session override → keychain MRU → config → None.
                let archive_pw = if crate::is_encrypted_archive_ext(&p) {
                    app.archive_passwords
                        .get(&p)
                        .cloned()
                        .or_else(|| {
                            app.keychain.ensure_loaded();
                            app.keychain.passwords.first().cloned()
                        })
                        .or_else(|| app.config.conversion.archive_password.clone())
                } else {
                    None
                };
                if app
                    .manager
                    .add_file_ready_for_processing(p, options.clone(), archive_pw)
                    .is_ok()
                {
                    count += 1;
                }
            }
            app.browse.clear_multi_selection();
            app.set_status(format!("Queued {} files", count));
            app.current_screen = AppScreen::Queue;
            app.browse.return_target = BrowseReturnTarget::None;
        }
    }
}

// ── Queue screen keybindings ─────────────────────────────────────────

fn handle_queue_key(app: &mut AppState, key: KeyEvent, tx: &mpsc::Sender<AppMessage>) {
    match (key.code, key.modifiers) {
        // Navigation
        (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE) => {
            if app.selected_index > 0 {
                app.selected_index -= 1;
                app.ensure_visible();
            }
        }
        (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE) => {
            if app.selected_index + 1 < app.items_snapshot.len() {
                app.selected_index += 1;
                app.ensure_visible();
            }
        }
        (KeyCode::Home, _) | (KeyCode::Char('g'), KeyModifiers::NONE) => {
            app.selected_index = 0;
            app.scroll_offset = 0;
        }
        (KeyCode::End, _) | (KeyCode::Char('G'), KeyModifiers::SHIFT) => {
            if !app.items_snapshot.is_empty() {
                app.selected_index = app.items_snapshot.len() - 1;
                app.ensure_visible();
            }
        }
        (KeyCode::PageUp, _) => {
            let jump = app.visible_height.max(1);
            app.selected_index = app.selected_index.saturating_sub(jump);
            app.ensure_visible();
        }
        (KeyCode::PageDown, _) => {
            let jump = app.visible_height.max(1);
            app.selected_index = (app.selected_index + jump).min(
                app.items_snapshot.len().saturating_sub(1),
            );
            app.ensure_visible();
        }

        // Selection
        (KeyCode::Char(' '), KeyModifiers::NONE) => {
            app.toggle_current_selection();
            if app.selected_index + 1 < app.items_snapshot.len() {
                app.selected_index += 1;
                app.ensure_visible();
            }
        }
        (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
            app.manager.select_all();
            app.set_status("Selected all items");
        }

        // Item info
        (KeyCode::Enter, KeyModifiers::NONE) => {
            if let Some(item) = app.items_snapshot.get(app.selected_index) {
                match &item.status {
                    ConversionStatus::Failed { error } => {
                        app.active_overlay = ActiveOverlay::ErrorDetail {
                            item_id: item.id.clone(),
                            error: error.clone(),
                        };
                    }
                    _ => {
                        app.active_overlay = ActiveOverlay::ItemInfo {
                            item: item.clone(),
                        };
                    }
                }
            }
        }

        // Add files
        (KeyCode::Char('a'), KeyModifiers::NONE) | (KeyCode::Char('f'), KeyModifiers::NONE) => {
            app.active_overlay = ActiveOverlay::FileInput {
                input: super::text_input::TextInputState::empty(),
            };
        }

        // Configure (open wizard)
        (KeyCode::Char('c'), KeyModifiers::NONE) => {
            let has_selected = app.items_snapshot.iter().any(|i| i.selected);
            app.wizard_target = if has_selected {
                WizardTarget::ConfigureSelected
            } else {
                WizardTarget::ConfigureAll
            };
            app.wizard = Some(tonepoet_wizard::SimpleWizard::new());
            app.current_screen = AppScreen::Wizard;
        }

        // Start conversion
        (KeyCode::Char('s'), KeyModifiers::NONE) => {
            if !app.processing_active {
                let tx_clone = tx.clone();
                start_conversion(app, tx_clone);
            } else {
                app.set_status("Conversion already running");
            }
        }

        // Pause/Resume
        (KeyCode::Char('p'), KeyModifiers::NONE) => {
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

        // Stop
        (KeyCode::Char('x'), KeyModifiers::NONE) => {
            if app.processing_active {
                app.active_overlay = ActiveOverlay::Confirmation {
                    message: "Stop all active conversions?".to_string(),
                    action: ConfirmAction::StopAll,
                };
            }
        }

        // Delete/Remove selected
        (KeyCode::Delete, _) | (KeyCode::Char('d'), KeyModifiers::NONE) => {
            let selected_count = app.items_snapshot.iter().filter(|i| i.selected).count();
            if selected_count > 0 {
                app.active_overlay = ActiveOverlay::Confirmation {
                    message: format!("Remove {} selected item(s)?", selected_count),
                    action: ConfirmAction::RemoveSelected,
                };
            }
        }

        // Clear completed
        (KeyCode::Char('l'), KeyModifiers::CONTROL) => {
            let completed_count = app.items_snapshot.iter()
                .filter(|i| matches!(i.status, ConversionStatus::Completed { .. }))
                .count();
            if completed_count > 0 {
                app.manager.clear_completed();
                app.set_status(format!("Cleared {} completed items", completed_count));
            }
        }

        // Retry failed
        (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
            retry_failed(app);
        }

        // Tab between file list and action bar
        (KeyCode::Tab, _) => {
            app.queue_focus = match app.queue_focus {
                QueueFocus::FileList => QueueFocus::ActionBar,
                QueueFocus::ActionBar => QueueFocus::FileList,
            };
        }

        _ => {}
    }
}

// ── Wizard keybindings ───────────────────────────────────────────────

fn handle_wizard_key(app: &mut AppState, key: KeyEvent) {
    if let Some(wizard) = &mut app.wizard {
        wizard.handle_key(key);

        if wizard.should_start_conversion {
            let (format, options) = crate::convert::extract_wizard_settings(wizard);
            app.wizard = None;
            app.wizard_mouse_areas = None;
            app.current_screen = AppScreen::Queue;

            let queue = app.manager.queue.clone();
            let has_selected = if let Ok(q) = queue.try_read() {
                q.all_items().iter().any(|i| i.selected)
            } else {
                false
            };

            if let Ok(mut q) = queue.try_write() {
                for item in q.all_items_mut() {
                    if has_selected && !item.selected {
                        continue;
                    }
                    match item.status {
                        ConversionStatus::NotConfigured
                        | ConversionStatus::Queued
                        | ConversionStatus::Paused => {
                            item.output_format = options.output_format;
                            item.options = options.clone();
                            item.status = ConversionStatus::Queued;
                        }
                        _ => {}
                    }
                }
            }
            app.set_status(format!("Configured items for {} conversion", format.name()));
            return;
        }

        if wizard.should_exit {
            app.wizard = None;
            app.wizard_mouse_areas = None;
            app.current_screen = AppScreen::Convert;
        }
    }
}

// ── Overlay keybindings ──────────────────────────────────────────────

fn handle_overlay_key(app: &mut AppState, key: KeyEvent, tx: &mpsc::Sender<AppMessage>) {
    let overlay = app.active_overlay.clone();
    match overlay {
        ActiveOverlay::Confirmation { action, .. } => {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    app.active_overlay = ActiveOverlay::None;
                    execute_confirm_action(app, &action, tx);
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    app.active_overlay = ActiveOverlay::None;
                }
                _ => {}
            }
        }
        ActiveOverlay::ErrorDetail { .. } | ActiveOverlay::ItemInfo { .. } => {
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                app.active_overlay = ActiveOverlay::None;
            }
        }
        ActiveOverlay::FileInput { mut input } => {
            match key.code {
                KeyCode::Enter => {
                    let path = std::path::PathBuf::from(input.text.trim());
                    app.active_overlay = ActiveOverlay::None;
                    if !input.text.trim().is_empty() {
                        handle_file_input(app, &path);
                    }
                    return;
                }
                KeyCode::Esc => {
                    app.active_overlay = ActiveOverlay::None;
                    return;
                }
                _ => {
                    super::text_input::handle_text_input_key(&mut input, &key);
                }
            }
            app.active_overlay = ActiveOverlay::FileInput { input };
        }
        ActiveOverlay::CommandInput { mut input, mut completion } => {
            match key.code {
                KeyCode::Enter => {
                    app.active_overlay = ActiveOverlay::None;
                    if !input.text.trim().is_empty() {
                        let cmd = super::command::parse_command(&input.text);
                        super::command::execute_command(app, cmd, tx);
                    }
                    return;
                }
                KeyCode::Esc => {
                    app.active_overlay = ActiveOverlay::None;
                    return;
                }
                KeyCode::Tab => {
                    handle_command_tab(&mut input, &mut completion, 1);
                }
                KeyCode::BackTab => {
                    handle_command_tab(&mut input, &mut completion, -1);
                }
                _ => {
                    // Any other key clears completion state so the next
                    // Tab re-parses against the updated input.
                    completion = None;
                    super::text_input::handle_text_input_key(&mut input, &key);
                }
            }
            app.active_overlay = ActiveOverlay::CommandInput { input, completion };
        }
        ActiveOverlay::TextEdit { mut input, target, label } => {
            match key.code {
                KeyCode::Enter => {
                    let text = input.text.clone();
                    apply_text_edit(app, target, &text, tx);
                    // apply_text_edit may set its own overlay (e.g. BulkRenameLine
                    // restores BulkRename). Only clear if it didn't.
                    if matches!(app.active_overlay, ActiveOverlay::TextEdit { .. }) {
                        app.active_overlay = ActiveOverlay::None;
                    }
                }
                KeyCode::Esc => {
                    // If a BulkRename sub-edit was in progress, restore the overlay.
                    if matches!(
                        target,
                        TextEditTarget::BulkRenameLine(_)
                            | TextEditTarget::SaveRenameTemplate(_)
                    ) {
                        if let Some(rename_state) = app.pending_bulk_rename.take() {
                            app.active_overlay = ActiveOverlay::BulkRename(rename_state);
                            return;
                        }
                    }
                    app.active_overlay = ActiveOverlay::None;
                }
                _ => {
                    super::text_input::handle_text_input_key(&mut input, &key);
                    // Only reach here when input was modified (no consuming ops).
                    app.active_overlay = ActiveOverlay::TextEdit { input, target, label };
                }
            }
        }
        ActiveOverlay::BatchList { scroll } => {
            handle_batch_list_key(app, key, scroll, tx);
        }
        ActiveOverlay::ContextMenu {
            entries, selected, origin,
            submenu_entries, submenu_selected,
            show_submenu, focus_submenu,
        } => {
            handle_context_menu_key(
                app, key, entries, selected, origin,
                submenu_entries, submenu_selected,
                show_submenu, focus_submenu, tx,
            );
        }
        ActiveOverlay::BulkRename(state) => {
            handle_bulk_rename_key(app, key, *state, tx);
        }
        ActiveOverlay::None => {}
    }
}

/// Handle key events for the context menu overlay. Two-level side-by-side
/// model: parent menu stays visible; submenu appears to the right when
/// the cursor is on a Submenu entry. `focus_submenu` tracks which panel
/// has keyboard focus.
#[allow(clippy::too_many_arguments)]
fn handle_context_menu_key(
    app: &mut AppState,
    key: KeyEvent,
    entries: Vec<super::context_menu::ContextMenuEntry>,
    mut selected: usize,
    origin: (u16, u16),
    mut submenu_entries: Vec<super::context_menu::ContextMenuEntry>,
    mut submenu_selected: usize,
    mut show_submenu: bool,
    mut focus_submenu: bool,
    tx: &mpsc::Sender<AppMessage>,
) {
    use super::context_menu::{ContextMenuEntry, execute_context_action};

    /// Build selectable-index list for a set of entries.
    fn selectable_indices(entries: &[ContextMenuEntry]) -> Vec<usize> {
        entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match e {
                ContextMenuEntry::Item(item) if item.enabled => Some(i),
                ContextMenuEntry::Submenu { .. } => Some(i),
                _ => None,
            })
            .collect()
    }

    let parent_selectable = selectable_indices(&entries);

    match key.code {
        KeyCode::Esc => {
            if focus_submenu {
                // Esc in submenu → return focus to parent.
                focus_submenu = false;
            } else {
                // Esc in parent → close entirely.
                app.active_overlay = ActiveOverlay::None;
                return;
            }
        }
        KeyCode::Left => {
            if focus_submenu {
                focus_submenu = false;
            } else {
                app.active_overlay = ActiveOverlay::None;
                return;
            }
        }
        KeyCode::Right => {
            if !focus_submenu && show_submenu {
                // Move focus into the submenu.
                focus_submenu = true;
                submenu_selected = 0;
            } else if focus_submenu {
                // In submenu, Right on a Submenu item could open a 3rd
                // level — for v1 we don't support 3 levels, so Enter
                // is needed for leaf items.
            }
        }
        KeyCode::Enter => {
            if focus_submenu {
                // Execute the selected submenu item (or no-op if Submenu).
                let sub_selectable = selectable_indices(&submenu_entries);
                if let Some(&idx) = sub_selectable.get(submenu_selected) {
                    if let ContextMenuEntry::Item(item) = &submenu_entries[idx] {
                        let action = item.action.clone();
                        app.active_overlay = ActiveOverlay::None;
                        execute_context_action(app, action, tx);
                        return;
                    }
                }
            } else {
                // In parent: Enter on Item executes; on Submenu moves focus in.
                if let Some(&idx) = parent_selectable.get(selected) {
                    match &entries[idx] {
                        ContextMenuEntry::Submenu { .. } if show_submenu => {
                            focus_submenu = true;
                            submenu_selected = 0;
                        }
                        ContextMenuEntry::Item(item) => {
                            let action = item.action.clone();
                            app.active_overlay = ActiveOverlay::None;
                            execute_context_action(app, action, tx);
                            return;
                        }
                        _ => {}
                    }
                }
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if focus_submenu {
                if submenu_selected > 0 {
                    submenu_selected -= 1;
                }
            } else {
                if selected > 0 {
                    selected -= 1;
                }
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if focus_submenu {
                let sub_selectable = selectable_indices(&submenu_entries);
                if submenu_selected + 1 < sub_selectable.len() {
                    submenu_selected += 1;
                }
            } else {
                if selected + 1 < parent_selectable.len() {
                    selected += 1;
                }
            }
        }
        _ => {
            app.active_overlay = ActiveOverlay::None;
            return;
        }
    }

    // After cursor movement in the parent, update the submenu to match
    // the newly selected entry (if it's a Submenu).
    if !focus_submenu {
        if let Some(&idx) = parent_selectable.get(selected) {
            match &entries[idx] {
                ContextMenuEntry::Submenu { children, .. } => {
                    submenu_entries = children.clone();
                    submenu_selected = 0;
                    show_submenu = true;
                }
                _ => {
                    show_submenu = false;
                    submenu_entries.clear();
                    submenu_selected = 0;
                }
            }
        }
    }

    app.active_overlay = ActiveOverlay::ContextMenu {
        entries,
        selected,
        origin,
        submenu_entries,
        submenu_selected,
        show_submenu,
        focus_submenu,
    };
}

/// Build and open the context menu for the current screen. `x, y` is
/// the screen position where the menu should be anchored (right-click
/// position, or a computed position for keyboard `m`).
fn open_context_menu(app: &mut AppState, x: u16, y: u16) {
    use super::context_menu::*;

    let entries = match app.current_screen {
        AppScreen::Browse => {
            // If there's a selected entry, build entry menu; else empty-space menu.
            if app.browse.selected_entry().is_some() {
                build_browse_entry_menu(app)
            } else {
                build_browse_empty_menu(app)
            }
        }
        AppScreen::Convert => build_convert_menu(app),
        AppScreen::Queue => {
            if app.items_snapshot.is_empty() {
                build_queue_empty_menu(app)
            } else {
                build_queue_item_menu(app)
            }
        }
        _ => {
            // Library, Config, Wizard — no context menu yet.
            return;
        }
    };

    if entries.is_empty() {
        return;
    }

    // Check if the first entry is a Submenu — if so, auto-show its children.
    let (submenu_entries, show_submenu) = match entries.first() {
        Some(super::context_menu::ContextMenuEntry::Submenu { children, .. }) => {
            (children.clone(), true)
        }
        _ => (Vec::new(), false),
    };

    app.active_overlay = ActiveOverlay::ContextMenu {
        entries,
        selected: 0,
        origin: (x, y),
        submenu_entries,
        submenu_selected: 0,
        show_submenu,
        focus_submenu: false,
    };
}

/// Handle a Tab or Shift+Tab press inside the CommandInput overlay.
/// On first press (no active completion), parses the input at the
/// cursor and gathers candidates. On subsequent presses, cycles
/// through the candidates. Direction: +1 for Tab, -1 for Shift+Tab.
///
/// First-press starting index is the first candidate that is NOT
/// identical to the typed prefix — so typing `queue` + Tab advances
/// to `queue!` rather than no-op-applying `queue` onto itself.
fn handle_command_tab(
    input: &mut super::text_input::TextInputState,
    completion: &mut Option<CompletionState>,
    direction: i32,
) {
    if let Some(state) = completion.as_mut() {
        // Already cycling — advance to next/previous candidate.
        super::command::cycle_completion(input, state, direction);
        return;
    }

    // First Tab: compute candidates, pick an initial index that produces
    // a visible change (skipping any candidate identical to typed prefix).
    let Some(mut state) = super::command::compute_completion(&input.text, input.cursor)
    else {
        return;
    };
    let typed: String = input.text[state.prefix_start..input.cursor.min(input.text.len())]
        .to_string();
    let len = state.candidates.len();
    state.cursor = if direction >= 0 {
        // Forward: first candidate that isn't the typed prefix, else 0.
        state
            .candidates
            .iter()
            .position(|c| c != &typed)
            .unwrap_or(0)
    } else {
        // Backward: last candidate that isn't the typed prefix, else last.
        state
            .candidates
            .iter()
            .rposition(|c| c != &typed)
            .unwrap_or(len - 1)
    };
    super::command::apply_completion_to_input(input, &state);
    *completion = Some(state);
}

/// Handle key events for the bulk rename wizard overlay.
fn handle_bulk_rename_key(
    app: &mut AppState,
    key: KeyEvent,
    mut state: BulkRenameState,
    tx: &mpsc::Sender<AppMessage>,
) {
    match state.focus {
        BulkRenameFocus::Template => {
            match key.code {
                KeyCode::Esc => {
                    app.active_overlay = ActiveOverlay::None;
                    return;
                }
                KeyCode::Tab => {
                    state.focus = BulkRenameFocus::List;
                }
                KeyCode::Enter => {
                    // Commit the plan if no conflicts.
                    if state.plan.conflict_count() > 0 {
                        app.set_status("Resolve conflicts before committing");
                    } else if state.plan.pending_count() == 0 {
                        app.set_status("Nothing to rename");
                    } else {
                        execute_bulk_rename(app, &mut state, tx);
                        return;
                    }
                }
                _ => {
                    super::text_input::handle_text_input_key(&mut state.template_input, &key);
                    state.rebuild_plan();
                }
            }
        }
        BulkRenameFocus::List => {
            let total = state.plan.ops.len();
            match key.code {
                KeyCode::Esc => {
                    app.active_overlay = ActiveOverlay::None;
                    return;
                }
                KeyCode::Tab => {
                    state.focus = BulkRenameFocus::Template;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if state.selected > 0 {
                        state.selected -= 1;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if state.selected + 1 < total {
                        state.selected += 1;
                    }
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    state.selected = 0;
                }
                KeyCode::End | KeyCode::Char('G') => {
                    state.selected = total.saturating_sub(1);
                }
                KeyCode::Enter => {
                    if state.plan.conflict_count() > 0 {
                        app.set_status("Resolve conflicts before committing");
                    } else if state.plan.pending_count() == 0 {
                        app.set_status("Nothing to rename");
                    } else {
                        execute_bulk_rename(app, &mut state, tx);
                        return;
                    }
                }
                KeyCode::Char('e') => {
                    if state.selected < total {
                        let current = state.plan.ops[state.selected].target_relative.clone();
                        let idx = state.selected;
                        // Park the BulkRenameState while TextEdit is open.
                        app.pending_bulk_rename = Some(Box::new(state));
                        app.active_overlay = ActiveOverlay::TextEdit {
                            input: super::text_input::TextInputState::new(current),
                            target: TextEditTarget::BulkRenameLine(idx),
                            label: "edit name".to_string(),
                        };
                        return;
                    }
                }
                KeyCode::Char('c') => {
                    apply_cue_to_rename(app, &mut state);
                }
                KeyCode::Char('t') => {
                    // Cycle through saved rename templates.
                    let templates = super::rename_presets::list_templates();
                    if templates.is_empty() {
                        app.set_status("No saved rename templates");
                    } else {
                        let current = &state.template_input.text;
                        // Find the current template in the list, advance to next.
                        let idx = templates
                            .iter()
                            .position(|(_, tmpl)| tmpl == current)
                            .map(|i| (i + 1) % templates.len())
                            .unwrap_or(0);
                        let (name, tmpl) = &templates[idx];
                        state.template_input =
                            super::text_input::TextInputState::new(tmpl.clone());
                        state.rebuild_plan();
                        app.set_status(&format!("Template: {}", name));
                    }
                }
                KeyCode::Char('C') => {
                    // Apply Chicago-style title capitalization to all targets.
                    // Split by `/` to handle subdirectory paths independently,
                    // and separate the extension from the final component.
                    for op in &mut state.plan.ops {
                        let parts: Vec<&str> = op.target_relative.split('/').collect();
                        let capitalized: Vec<String> = parts
                            .iter()
                            .enumerate()
                            .map(|(i, part)| {
                                let is_last = i == parts.len() - 1;
                                if is_last {
                                    // Last component: separate stem from extension.
                                    if let Some(dot_pos) = part.rfind('.') {
                                        let stem = &part[..dot_pos];
                                        let ext = &part[dot_pos..];
                                        format!(
                                            "{}{}",
                                            crate::convert::renaming::capitalize_title(stem),
                                            ext,
                                        )
                                    } else {
                                        crate::convert::renaming::capitalize_title(part)
                                    }
                                } else {
                                    // Directory component: capitalize as title.
                                    crate::convert::renaming::capitalize_title(part)
                                }
                            })
                            .collect();
                        op.target_relative = capitalized.join("/");
                    }
                    crate::tui::rename_plan::validate_plan(&mut state.plan);
                    app.set_status("Applied title capitalization");
                }
                KeyCode::Char('S') => {
                    // Save current template. Prompt for name via TextEdit.
                    let template_text = state.template_input.text.clone();
                    if template_text.trim().is_empty() {
                        app.set_status("Template is empty");
                    } else {
                        app.pending_bulk_rename = Some(Box::new(state));
                        app.active_overlay = ActiveOverlay::TextEdit {
                            input: super::text_input::TextInputState::empty(),
                            target: TextEditTarget::SaveRenameTemplate(template_text),
                            label: "template name".to_string(),
                        };
                        return;
                    }
                }
                _ => {}
            }
        }
    }
    app.active_overlay = ActiveOverlay::BulkRename(Box::new(state));
}

/// Execute the bulk rename plan: move files, refresh browse, close overlay.
fn execute_bulk_rename(
    app: &mut AppState,
    state: &mut BulkRenameState,
    _tx: &mpsc::Sender<AppMessage>,
) {
    match crate::tui::rename_plan::execute_plan(&mut state.plan) {
        Ok(count) => {
            app.set_status(&format!("Renamed {} file{}", count, if count == 1 { "" } else { "s" }));
            app.active_overlay = ActiveOverlay::None;
            // Refresh browse to reflect the renames.
            app.browse.refresh();
        }
        Err(e) => {
            app.set_status(&format!("Rename failed: {}", e));
            // Keep overlay open so user can see what happened.
            app.active_overlay = ActiveOverlay::BulkRename(Box::new(state.clone()));
        }
    }
}

/// Open the bulk rename overlay with the given file paths. Pulls metadata
/// from the browse probe cache where available, falling back to defaults.
pub fn open_bulk_rename(app: &mut AppState, paths: Vec<std::path::PathBuf>) {
    if paths.is_empty() {
        app.set_status("No files selected for rename");
        return;
    }

    let base_dir = app.browse.current_dir.clone();
    let metadata: Vec<super::probe::SourceMetadata> = paths
        .iter()
        .map(|p| {
            app.browse
                .probe_cache
                .get(p)
                .and_then(|opt| opt.as_ref())
                .map(|cached| cached.metadata.clone())
                .unwrap_or_default()
        })
        .collect();

    let state = BulkRenameState::new(base_dir, paths, metadata);
    app.active_overlay = ActiveOverlay::BulkRename(Box::new(state));
}

/// Load a CUE sheet from the base directory and override the BulkRename
/// metadata with CUE track data. Matches CUE tracks to source files by
/// FILE reference, then by sequential order. Rebuilds the plan afterwards.
fn apply_cue_to_rename(app: &mut AppState, state: &mut BulkRenameState) {
    let base_dir = &state.plan.base_dir;

    // Scan for .cue files in the base directory.
    let cue_files: Vec<std::path::PathBuf> = std::fs::read_dir(base_dir)
        .ok()
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.extension()
                        .map(|e| e.to_ascii_lowercase() == "cue")
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();

    if cue_files.is_empty() {
        app.set_status("No .cue files found in this directory");
        return;
    }

    let cue_path = &cue_files[0];
    let sheet = match super::cue_parser::parse_cue_file(cue_path) {
        Ok(s) => s,
        Err(e) => {
            app.set_status(&format!("CUE parse error: {}", e));
            return;
        }
    };

    if sheet.tracks.is_empty() {
        app.set_status("CUE file contains no tracks");
        return;
    }

    // Map CUE tracks → source files.
    // 1. Try FILE reference match (for track-by-track CUEs).
    // 2. Fall back to sequential (CUE track index → source index).
    let mut matched = 0usize;
    for (i, source) in state.sources.iter().enumerate() {
        let source_name = source
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let cue_track = sheet
            .tracks
            .iter()
            .find(|t| t.file.as_deref() == Some(source_name.as_str()))
            .or_else(|| sheet.tracks.get(i));

        if let Some(track) = cue_track {
            state.metadata[i].track_number = Some(track.number);
            if let Some(ref title) = track.title {
                state.metadata[i].title = Some(title.clone());
            }
            if let Some(ref performer) = track.performer {
                state.metadata[i].artist = Some(performer.clone());
            }
            if let Some(ref album) = sheet.title {
                state.metadata[i].album = Some(album.clone());
            }
            matched += 1;
        }
    }

    state.rebuild_plan();
    let cue_name = cue_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    app.set_status(&format!(
        "Loaded {} ({}/{} tracks matched)",
        cue_name,
        matched,
        sheet.tracks.len()
    ));
}

/// Handle key events for the BatchList expand overlay. Moves the Batch
/// cursor with up/down/j/k, jumps with Home/End, removes the hovered file
/// with `d`, closes with Enter/Esc.
///
/// Scroll is vim-smooth: it only updates when the cursor exits the
/// visible range. The handler uses a conservative visible-row estimate
/// (`APPROX_VISIBLE`) since it doesn't know the actual rendered height;
/// the renderer clamps defensively so the cursor is always in view.
fn handle_batch_list_key(
    app: &mut AppState,
    key: KeyEvent,
    mut scroll: usize,
    tx: &mpsc::Sender<AppMessage>,
) {
    /// Conservative estimate of visible list rows. The real value
    /// depends on terminal height; the renderer clamps to the actual
    /// list area, so a too-small estimate just means the cursor
    /// scrolls early (visible) rather than never (broken). 15 fits a
    /// typical 30-row terminal with a ~25-row popup after chrome.
    const APPROX_VISIBLE: usize = 15;

    // Grab batch size for navigation bounds.
    let (n, cursor) = match &app.convert.source.mode {
        SourceMode::Batch { paths, cursor, .. } => (paths.len(), *cursor),
        _ => {
            // Batch vanished — close overlay defensively.
            app.active_overlay = ActiveOverlay::None;
            return;
        }
    };

    match key.code {
        KeyCode::Esc | KeyCode::Enter => {
            app.active_overlay = ActiveOverlay::None;
            return;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if cursor > 0 {
                move_batch_cursor(app, cursor - 1, tx);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if cursor + 1 < n {
                move_batch_cursor(app, cursor + 1, tx);
            }
        }
        KeyCode::Home => {
            if cursor > 0 {
                move_batch_cursor(app, 0, tx);
            }
        }
        KeyCode::End => {
            if n > 0 && cursor + 1 != n {
                move_batch_cursor(app, n - 1, tx);
            }
        }
        KeyCode::Char('d') => {
            remove_batch_at_cursor(app, tx);
            // If the batch collapsed (either to Single when only one
            // file remains, or to Empty when all were removed), close
            // the overlay — it's no longer meaningful.
            if !app.convert.source.mode.is_batch() {
                app.active_overlay = ActiveOverlay::None;
                return;
            }
        }
        _ => {}
    }

    // Re-read the cursor after any mutating operations above.
    let new_cursor = match &app.convert.source.mode {
        SourceMode::Batch { cursor, .. } => *cursor,
        _ => {
            app.active_overlay = ActiveOverlay::None;
            return;
        }
    };

    // Vim-smooth scroll: only shift the visible range when the cursor
    // has moved outside it. `APPROX_VISIBLE` is a conservative guess;
    // the renderer clamps to the actual list area.
    if new_cursor < scroll {
        scroll = new_cursor;
    } else if new_cursor >= scroll + APPROX_VISIBLE {
        scroll = new_cursor + 1 - APPROX_VISIBLE;
    }

    app.active_overlay = ActiveOverlay::BatchList { scroll };
}

/// Move the Batch cursor to `new_cursor` and spawn a background probe
/// for the new file. The `cursor_info`/`cursor_metadata` fields are
/// cleared immediately so the pane preview shows "probing…" until the
/// `AudioProbeComplete` message arrives and refreshes them.
fn move_batch_cursor(
    app: &mut AppState,
    new_cursor: usize,
    tx: &mpsc::Sender<AppMessage>,
) {
    let new_path = match &app.convert.source.mode {
        SourceMode::Batch { paths, .. } => paths.get(new_cursor).cloned(),
        _ => None,
    };
    let Some(path) = new_path else { return; };

    if let SourceMode::Batch {
        cursor,
        cursor_info,
        cursor_metadata,
        ..
    } = &mut app.convert.source.mode
    {
        *cursor = new_cursor;
        // Clear stale info; AudioProbeComplete will repopulate.
        *cursor_info = None;
        *cursor_metadata = crate::tui::probe::SourceMetadata::default();
    }

    // Spawn the background probe. Result is routed to cursor_info in
    // event_loop's AudioProbeComplete handler when it arrives.
    super::browse::spawn_audio_probe(path, tx.clone());
}

/// Remove the file at the batch cursor from the batch. If the batch
/// drops to 0 files, transitions to `SourceMode::Empty`. If it drops
/// to 1 file, promotes to `SourceMode::Single` and spawns a background
/// probe for the remaining file.
fn remove_batch_at_cursor(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let (remaining_paths, new_cursor) = match &mut app.convert.source.mode {
        SourceMode::Batch { paths, cursor, .. } if !paths.is_empty() => {
            let idx = (*cursor).min(paths.len() - 1);
            paths.remove(idx);
            let new_cursor = idx.min(paths.len().saturating_sub(1));
            (paths.clone(), new_cursor)
        }
        _ => return,
    };

    if remaining_paths.is_empty() {
        app.convert.source.mode = SourceMode::Empty;
        return;
    }

    if remaining_paths.len() == 1 {
        // Promote to Single with empty info/metadata; AudioProbeComplete
        // will populate it when the background probe finishes.
        let path = remaining_paths.into_iter().next().unwrap();
        app.convert.source.mode = SourceMode::Single {
            path: path.clone(),
            info: None,
            metadata: crate::tui::probe::SourceMetadata::default(),
        };
        super::browse::spawn_audio_probe(path, tx.clone());
        return;
    }

    // Stay in Batch — recompute summary and move cursor.
    let mut new_mode = SourceMode::from_paths(remaining_paths);
    let spawned_path = if let SourceMode::Batch { cursor, paths, .. } = &mut new_mode {
        *cursor = new_cursor;
        paths.get(new_cursor).cloned()
    } else {
        None
    };
    app.convert.source.mode = new_mode;

    if let Some(p) = spawned_path {
        super::browse::spawn_audio_probe(p, tx.clone());
    }
}

/// Apply a text edit to the target field, setting modified flag as needed
fn apply_text_edit(
    app: &mut AppState,
    target: TextEditTarget,
    value: &str,
    tx: &mpsc::Sender<AppMessage>,
) {
    let trimmed = value.trim();
    let value_opt = if trimmed.is_empty() { None } else { Some(trimmed.to_string()) };

    match target {
        TextEditTarget::DestPath => {
            // dest_path is not in the preset, don't mark modified
            app.convert.output_options.dest_path =
                if trimmed.is_empty() { None } else { Some(std::path::PathBuf::from(trimmed)) };
        }
        TextEditTarget::FolderTemplate => {
            app.convert.output_options.folder_template = trimmed.to_string();
            app.preset.mark_modified();
        }
        TextEditTarget::FilenameTemplate => {
            app.convert.output_options.filename_template = trimmed.to_string();
            app.preset.mark_modified();
        }
        TextEditTarget::MetaTitle => {
            app.convert.metadata.title = value_opt;
        }
        TextEditTarget::MetaArtist => {
            app.convert.metadata.artist = value_opt;
        }
        TextEditTarget::MetaAlbum => {
            app.convert.metadata.album = value_opt;
        }
        TextEditTarget::MetaGenre => {
            app.convert.metadata.genre = value_opt;
        }
        TextEditTarget::MetaYear => {
            app.convert.metadata.year = value_opt;
        }
        TextEditTarget::BrowseRename(original_path) => {
            commit_browse_rename(app, original_path, trimmed, tx);
        }
        TextEditTarget::BrowseCopy { sources, force } => {
            do_file_op(app, &sources, trimmed, force, false, tx);
        }
        TextEditTarget::BrowseMove { sources, force } => {
            do_file_op(app, &sources, trimmed, force, true, tx);
        }
        TextEditTarget::BrowseMetadata { path, field } => {
            // Race check: refuse if the file is currently being converted.
            let is_processing = app.items_snapshot.iter().any(|item| {
                item.input_path == path
                    && matches!(
                        item.status,
                        crate::convert::ConversionStatus::Processing { .. }
                    )
            });
            if is_processing {
                app.set_status(format!(
                    "cannot edit: {} is currently being converted",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
                return;
            }

            // Write the tag via lofty.
            match crate::tui::probe::write_metadata_field(&path, field, trimmed) {
                Ok(()) => {
                    // Invalidate probe cache so the info pane re-probes
                    // with fresh tags on next cursor visit.
                    app.browse.probe_cache.remove(&path);
                    app.browse.probe_pending.remove(&path);
                    // Re-trigger probe for immediate refresh.
                    app.browse.probe_current(tx);
                    app.set_status(format!(
                        "{}: {} updated",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        field.label(),
                    ));
                }
                Err(e) => {
                    app.set_status(e);
                }
            }
        }
        TextEditTarget::SaveRenameTemplate(template_str) => {
            if !trimmed.is_empty() {
                match super::rename_presets::save_template(trimmed, &template_str) {
                    Ok(()) => app.set_status(&format!("Saved template: {}", trimmed)),
                    Err(e) => app.set_status(&format!("Save failed: {}", e)),
                }
            }
            // Restore the BulkRename overlay.
            if let Some(rename_state) = app.pending_bulk_rename.take() {
                app.active_overlay = ActiveOverlay::BulkRename(rename_state);
            }
        }
        TextEditTarget::BulkRenameLine(line_idx) => {
            // Restore the parked BulkRenameState, update the edited line,
            // revalidate, and reopen the overlay.
            if let Some(mut rename_state) = app.pending_bulk_rename.take() {
                if line_idx < rename_state.plan.ops.len() {
                    // Sanitize the user's edited name.
                    let edited = match crate::tui::rename_plan::sanitize_path(trimmed) {
                        Ok(s) => s,
                        Err(_) => {
                            // Bad path — keep original target, show error.
                            app.set_status("Invalid path (contains unsafe characters)");
                            app.active_overlay =
                                ActiveOverlay::BulkRename(rename_state);
                            return;
                        }
                    };
                    rename_state.plan.ops[line_idx].target_relative = edited;
                    crate::tui::rename_plan::validate_plan(&mut rename_state.plan);
                }
                app.active_overlay = ActiveOverlay::BulkRename(rename_state);
            }
        }
        TextEditTarget::KeychainAdd => {
            if !trimmed.is_empty() {
                match super::keychain::add_password(trimmed) {
                    Ok(()) => {
                        app.keychain.reload();
                        app.set_status("Password added");
                    }
                    Err(e) => app.set_status(&format!("Add failed: {}", e)),
                }
            }
        }
        TextEditTarget::ArchivePassword(archive_path) => {
            if !trimmed.is_empty() {
                // Store as session override for this archive.
                app.archive_passwords
                    .insert(archive_path.clone(), trimmed.to_string());
                // Also add to keychain for future use.
                let _ = super::keychain::add_password(trimmed);
                app.keychain.reload();
                let name = archive_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                app.set_status(&format!("Password set for {} (saved to keychain)", name));
            }
        }
    }
}

/// Public entry point for file ops triggered with a destination arg
/// (`:cp /dest` or `:mv /dest`). Called from command.rs when the
/// destination is provided inline rather than via the TextEdit picker.
pub fn apply_file_op_pub(
    app: &mut AppState,
    target: TextEditTarget,
    dest: &str,
) {
    // We need a tx for probe_current after refresh, but we don't have
    // one in this context. Set a flag to probe on next event loop tick.
    match target {
        TextEditTarget::BrowseCopy { sources, force } => {
            do_file_op_no_tx(app, &sources, dest, force, false);
        }
        TextEditTarget::BrowseMove { sources, force } => {
            do_file_op_no_tx(app, &sources, dest, force, true);
        }
        _ => {}
    }
}

/// Perform a copy or move operation. Version with tx for probe refresh.
fn do_file_op(
    app: &mut AppState,
    sources: &[std::path::PathBuf],
    dest: &str,
    force: bool,
    is_move: bool,
    tx: &mpsc::Sender<AppMessage>,
) {
    do_file_op_inner(app, sources, dest, force, is_move);
    app.browse.refresh();
    app.browse.probe_current(tx);
}

/// Perform a copy or move without tx (when called from command.rs with
/// inline destination). Browse refresh is done; probe happens on next
/// cursor move.
fn do_file_op_no_tx(
    app: &mut AppState,
    sources: &[std::path::PathBuf],
    dest: &str,
    force: bool,
    is_move: bool,
) {
    do_file_op_inner(app, sources, dest, force, is_move);
    app.browse.refresh();
}

/// Core copy/move logic. Operates on each source path, reports results
/// via status message.
fn do_file_op_inner(
    app: &mut AppState,
    sources: &[std::path::PathBuf],
    dest: &str,
    force: bool,
    is_move: bool,
) {
    let dest_dir = std::path::PathBuf::from(dest.trim());
    if !dest_dir.exists() {
        // Try to create the destination directory.
        if let Err(e) = std::fs::create_dir_all(&dest_dir) {
            app.set_status(format!("failed to create destination: {}", e));
            return;
        }
    }
    if !dest_dir.is_dir() {
        app.set_status(format!("destination is not a directory: {}", dest_dir.display()));
        return;
    }

    let op_name = if is_move { "moved" } else { "copied" };
    let mut succeeded = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;

    for source in sources {
        let file_name = match source.file_name() {
            Some(n) => n.to_owned(),
            None => {
                errors += 1;
                continue;
            }
        };
        let target = dest_dir.join(&file_name);

        // Skip if source and target are the same path (copying a file
        // onto itself with force=true would corrupt it). Two checks:
        // 1. Raw path comparison (catches same-dir obvious case).
        // 2. Canonicalized comparison when both resolve (catches case-
        //    insensitive filesystems and symlink-resolved equivalence).
        //    Only compared when BOTH succeed — if either fails (e.g.,
        //    target doesn't exist yet), we skip this check to avoid
        //    false positives from None == None.
        if source == &target {
            skipped += 1;
            continue;
        }
        if let (Ok(src_canon), Ok(dst_canon)) =
            (source.canonicalize(), target.canonicalize())
        {
            if src_canon == dst_canon {
                skipped += 1;
                continue;
            }
        }

        // Check for existing destination.
        if target.exists() && !force {
            skipped += 1;
            continue;
        }

        let result = if is_move {
            move_path(source, &target)
        } else {
            copy_path(source, &target)
        };

        match result {
            Ok(()) => succeeded += 1,
            Err(e) => {
                log::warn!(
                    "{} failed: {} → {}: {}",
                    op_name,
                    source.display(),
                    target.display(),
                    e
                );
                errors += 1;
            }
        }
    }

    // Clear multi-selection after the operation.
    app.browse.clear_multi_selection();

    // Build status message.
    let mut parts = vec![format!("{} {} file(s)", op_name, succeeded)];
    if skipped > 0 {
        parts.push(format!("{} skipped (exists)", skipped));
    }
    if errors > 0 {
        parts.push(format!("{} errors", errors));
    }
    app.set_status(parts.join(", "));
}

/// Copy a file or directory to `target`. For directories, copies
/// recursively.
fn copy_path(source: &std::path::Path, target: &std::path::Path) -> Result<(), String> {
    if source.is_dir() {
        copy_dir_recursive(source, target)
    } else {
        std::fs::copy(source, target)
            .map(|_| ())
            .map_err(|e| format!("{}", e))
    }
}

/// Recursive directory copy. Skips symlinks to avoid loops and
/// unexpected out-of-tree copies (consistent with browse scan policy).
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("mkdir: {}", e))?;
    let entries = std::fs::read_dir(src).map_err(|e| format!("readdir: {}", e))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("entry: {}", e))?;
        let file_type = entry.file_type().map_err(|e| format!("filetype: {}", e))?;
        // Skip symlinks — following them could leave the source tree
        // or create infinite loops.
        if file_type.is_symlink() {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|e| format!("copy: {}", e))?;
        }
    }
    Ok(())
}

/// Move a file or directory to `target`. Tries `fs::rename` first (fast,
/// same-filesystem). If that fails with a cross-device error, falls back
/// to copy+verify+delete (ACID: original is only deleted after the copy
/// is confirmed via size match).
fn move_path(source: &std::path::Path, target: &std::path::Path) -> Result<(), String> {
    // Try fast rename first.
    match std::fs::rename(source, target) {
        Ok(()) => return Ok(()),
        Err(e) => {
            // ErrorKind::CrossesDevices is nightly-only. Check the raw
            // OS error: EXDEV = 18 on Linux, same concept on macOS.
            let is_cross_device = e.raw_os_error() == Some(18) // EXDEV on Linux
                || e.to_string().to_lowercase().contains("cross-device");
            if !is_cross_device {
                return Err(format!("{}", e));
            }
            // Fall through to copy+delete.
        }
    }

    // Cross-device fallback: copy, verify, delete.
    copy_path(source, target)?;

    // Verify: compare sizes (basic integrity check).
    if source.is_file() {
        let src_size = std::fs::metadata(source)
            .map(|m| m.len())
            .unwrap_or(0);
        let dst_size = std::fs::metadata(target)
            .map(|m| m.len())
            .unwrap_or(0);
        if src_size != dst_size {
            // Copy succeeded but sizes differ — don't delete original.
            return Err("cross-device move: size mismatch after copy, original preserved".to_string());
        }
    }

    // Delete original.
    if source.is_dir() {
        std::fs::remove_dir_all(source).map_err(|e| format!("remove original dir: {}", e))?;
    } else {
        std::fs::remove_file(source).map_err(|e| format!("remove original: {}", e))?;
    }

    Ok(())
}

/// Commit a rename for a browse entry: validates the new name, constructs the
/// target path from the original path's parent, calls fs::rename, refreshes the
/// browse listing, and repositions the cursor on the renamed entry.
pub(super) fn commit_browse_rename(
    app: &mut AppState,
    original_path: std::path::PathBuf,
    new_name: &str,
    tx: &mpsc::Sender<AppMessage>,
) {
    let old_name = original_path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_default();

    // Validation: empty name
    if new_name.is_empty() {
        app.set_status("rename: name cannot be empty");
        return;
    }
    // Validation: no path separators (would be a move, not a rename)
    if new_name.contains('/') || new_name.contains('\\') {
        app.set_status("rename: name cannot contain path separators");
        return;
    }
    // No-op if unchanged
    if new_name == old_name {
        return;
    }

    let parent = match original_path.parent() {
        Some(p) => p.to_path_buf(),
        None => {
            app.set_status("rename: cannot rename filesystem root");
            return;
        }
    };
    let target = parent.join(new_name);

    if target.exists() {
        app.set_status(format!("rename: target already exists: {}", new_name));
        return;
    }

    match std::fs::rename(&original_path, &target) {
        Ok(()) => {
            // Refresh the directory and reposition cursor on the renamed entry.
            app.browse.refresh();
            if let Some(idx) = app.browse.entries.iter().position(|e| e.path == target) {
                app.browse.selected_index = idx;
                app.browse.ensure_visible();
            }
            app.browse.probe_current(tx);
            app.set_status(format!("renamed: {} → {}", old_name, new_name));
        }
        Err(e) => {
            app.set_status(format!("rename failed: {}", e));
        }
    }
}

/// Handle key events while the recent-files overlay is open.
/// - Up/Down / k/j: navigate
/// - Enter: load the selected file as source and switch to convert
/// - d: delete the selected entry from history
/// - Esc: close the overlay without loading anything
fn handle_recent_overlay_key(app: &mut AppState, key: KeyEvent) {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => {
            app.recent.close_overlay();
        }
        (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE) => {
            app.recent.overlay_move_up();
        }
        (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE) => {
            app.recent.overlay_move_down();
        }
        (KeyCode::PageUp, _) => {
            app.recent.overlay_page_up();
        }
        (KeyCode::PageDown, _) => {
            app.recent.overlay_page_down();
        }
        (KeyCode::Home, _) | (KeyCode::Char('g'), KeyModifiers::NONE) => {
            app.recent.overlay_move_top();
        }
        (KeyCode::End, _) | (KeyCode::Char('G'), KeyModifiers::SHIFT) => {
            app.recent.overlay_move_bottom();
        }
        (KeyCode::Char('d'), KeyModifiers::NONE) => {
            let idx = app.recent.overlay_selected;
            app.recent.remove(idx);
            if app.recent.entries.is_empty() {
                app.recent.close_overlay();
            }
        }
        (KeyCode::Enter, _) => {
            let path = match app.recent.selected() {
                Some(e) => e.path.clone(),
                None => {
                    app.recent.close_overlay();
                    return;
                }
            };
            app.recent.close_overlay();
            // If the file no longer exists, drop it from history and report.
            if !path.exists() {
                app.set_status(format!(
                    "recent: file no longer exists: {}",
                    path.display()
                ));
                // Drop the dead entry.
                let idx = app
                    .recent
                    .entries
                    .iter()
                    .position(|e| e.path == path);
                if let Some(i) = idx {
                    app.recent.remove(i);
                }
                return;
            }
            // Load as source via the existing load path.
            load_recent_as_source(app, &path);
        }
        _ => {}
    }
}

/// Handle key events while the bookmarks overlay is open.
/// Has two modes:
/// - Browse mode: list navigation + add/delete/rename/cd actions
/// - Naming mode (Add or Rename): text input with Enter=commit, Esc=cancel
fn handle_bookmarks_overlay_key(app: &mut AppState, key: KeyEvent, tx: &mpsc::Sender<AppMessage>) {
    use super::bookmarks::BookmarkNaming;
    use super::text_input::handle_text_input_key;

    // Naming sub-mode routes text-input keys to the TextInputState.
    if app.bookmarks.naming.is_some() {
        match (key.code, key.modifiers) {
            (KeyCode::Enter, _) => {
                if !app.bookmarks.commit_naming() {
                    app.set_status("bookmark: name cannot be empty");
                }
            }
            (KeyCode::Esc, _) => {
                app.bookmarks.cancel_naming();
            }
            _ => {
                if let Some(naming) = &mut app.bookmarks.naming {
                    let input = match naming {
                        BookmarkNaming::Add { input, .. } => input,
                        BookmarkNaming::Rename { input, .. } => input,
                    };
                    handle_text_input_key(input, &key);
                }
            }
        }
        return;
    }

    // Browse mode.
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => {
            app.bookmarks.close_overlay();
        }
        (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE) => {
            app.bookmarks.overlay_move_up();
        }
        (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE) => {
            app.bookmarks.overlay_move_down();
        }
        (KeyCode::PageUp, _) => {
            app.bookmarks.overlay_page_up();
        }
        (KeyCode::PageDown, _) => {
            app.bookmarks.overlay_page_down();
        }
        (KeyCode::Home, _) | (KeyCode::Char('g'), KeyModifiers::NONE) => {
            app.bookmarks.overlay_move_top();
        }
        (KeyCode::End, _) | (KeyCode::Char('G'), KeyModifiers::SHIFT) => {
            app.bookmarks.overlay_move_bottom();
        }
        (KeyCode::Char('a'), KeyModifiers::NONE) => {
            // Add current browse directory as a bookmark.
            let path = app.browse.current_dir.clone();
            app.bookmarks.start_add(path);
        }
        (KeyCode::Char('d'), KeyModifiers::NONE) => {
            let idx = app.bookmarks.overlay_selected;
            app.bookmarks.remove(idx);
            // Overlay stays open; if entries became empty, next render shows
            // the "(no bookmarks)" placeholder.
        }
        (KeyCode::Char('e'), KeyModifiers::NONE) => {
            let idx = app.bookmarks.overlay_selected;
            app.bookmarks.start_rename(idx);
        }
        (KeyCode::Enter, _) => {
            let path = match app.bookmarks.selected() {
                Some(b) => b.path.clone(),
                None => {
                    app.bookmarks.close_overlay();
                    return;
                }
            };
            app.bookmarks.close_overlay();
            // Navigate directly with the stored PathBuf — no string round-trip
            // needed since bookmark paths are absolute.
            if !path.is_dir() {
                app.set_status(format!(
                    "bookmark: path no longer exists: {}",
                    path.display()
                ));
                return;
            }
            let display = path.display().to_string();
            app.browse.navigate_to(path);
            app.browse.probe_current(tx);
            app.set_status(format!("cd: {}", display));
        }
        _ => {}
    }
}

/// Load a file from the recent list as the current source and switch to convert.
fn load_recent_as_source(app: &mut AppState, path: &std::path::Path) {
    match crate::tui::probe::probe_audio(path) {
        Ok(info) => {
            let metadata = crate::tui::probe::read_metadata(path).unwrap_or_default();
            app.convert.metadata.title = metadata.title.clone();
            app.convert.metadata.artist = metadata.artist.clone();
            app.convert.metadata.album = metadata.album.clone();
            app.convert.metadata.genre = metadata.genre.clone();
            app.convert.metadata.year = metadata.year.clone();
            // Loading from recent replaces the source — abandon any
            // pending batch from a previous :queue.
            app.convert.source.mode = SourceMode::Single {
                path: path.to_path_buf(),
                info: Some(info),
                metadata,
            };
            app.set_status(format!(
                "Loaded: {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            app.current_screen = AppScreen::Convert;
            // Bump to top of recent list.
            app.recent.record_use(path);
        }
        Err(e) => {
            app.set_status(format!("Probe error: {}", e));
        }
    }
}

/// Handle file input completion — either set source file (convert screen) or add to queue
fn handle_file_input(app: &mut AppState, path: &std::path::Path) {
    if !path.exists() {
        app.set_status(format!("Path not found: {}", path.display()));
        return;
    }

    match app.current_screen {
        AppScreen::Convert => {
            // Probe the file first
            let info = match crate::tui::probe::probe_audio(path) {
                Ok(i) => i,
                Err(e) => {
                    // Reset to Empty on probe failure.
                    app.convert.source.mode = SourceMode::Empty;
                    app.set_status(format!("Probe error: {}", e));
                    return;
                }
            };
            // Read metadata (best-effort).
            let metadata = crate::tui::probe::read_metadata(path).unwrap_or_default();
            app.convert.metadata.title = metadata.title.clone();
            app.convert.metadata.artist = metadata.artist.clone();
            app.convert.metadata.album = metadata.album.clone();
            app.convert.metadata.genre = metadata.genre.clone();
            app.convert.metadata.year = metadata.year.clone();
            // FileInput replaces the source — abandon any pending batch.
            app.convert.source.mode = SourceMode::Single {
                path: path.to_path_buf(),
                info: Some(info),
                metadata,
            };
            app.set_status(format!(
                "Loaded: {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            // Record in the recent-files history.
            app.recent.record_use(path);
        }
        _ => {
            // Add to queue (existing behavior)
            add_path_to_queue(app, path);
        }
    }
}

// ── Helper functions ─────────────────────────────────────────────────

fn execute_confirm_action(
    app: &mut AppState,
    action: &ConfirmAction,
    tx: &mpsc::Sender<AppMessage>,
) {
    match action {
        ConfirmAction::RemoveSelected => {
            let removed = app.manager.remove_selected();
            app.set_status(format!("Removed {} items", removed));
        }
        ConfirmAction::ClearCompleted => {
            app.manager.clear_completed();
            app.set_status("Cleared completed items");
        }
        ConfirmAction::StopAll => {
            app.manager.stop_all_conversions();
            app.processing_active = false;
            app.set_status("Stopped all conversions");
        }
        ConfirmAction::ClearQueue => {
            app.manager.clear_queue();
            app.set_status("Cleared queue");
        }
        ConfirmAction::TrashSelection(paths) => {
            let mut trashed = 0usize;
            let mut errors = 0usize;
            for path in paths {
                match trash::delete(path) {
                    Ok(()) => trashed += 1,
                    Err(e) => {
                        log::warn!("trash: {}: {}", path.display(), e);
                        errors += 1;
                    }
                }
            }
            app.browse.clear_multi_selection();
            app.browse.refresh();
            app.browse.probe_current(tx);
            let mut parts = vec![format!("trashed {} item(s)", trashed)];
            if errors > 0 {
                parts.push(format!("{} errors", errors));
            }
            app.set_status(parts.join(", "));
        }
    }
}

fn add_path_to_queue(app: &mut AppState, path: &std::path::Path) {
    if !path.exists() {
        app.set_status(format!("Path not found: {}", path.display()));
        return;
    }

    let mut options = ConversionOptions::default();
    options.append_lineage_to_comment = app.config.conversion.append_lineage_to_comment;
    options.write_log_file = app.config.conversion.write_log_file;
    options.generate_cue_files = app.config.conversion.generate_cue_files;
    options.cue_generation_mode = app.config.conversion.cue_generation_mode.clone();

    if path.is_dir() {
        let mut count = 0;
        let mut errors = 0;
        for entry in walkdir::WalkDir::new(path)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let file_path = entry.path();
            if file_path.is_file() {
                // Check compound tar extensions first, then single extensions.
                let is_compound_tar = super::browse::is_tar_compound_pub(file_path);
                let is_known = is_compound_tar || file_path.extension().map(|ext| {
                    let e = ext.to_string_lossy().to_lowercase();
                    matches!(e.as_str(),
                        "7z" | "zip" | "rar" | "tar" | "iso" | "cab" | "tgz" | "tbz2" | "txz"
                        | "flac" | "wav" | "aiff" | "aif" | "wv" | "mp3" | "m4a" | "aac" | "opus" | "ogg"
                    )
                }).unwrap_or(false);
                if is_known {
                    match app.manager.add_file_blocking(file_path.to_path_buf(), options.clone()) {
                        Ok(_) => count += 1,
                        Err(_) => errors += 1,
                    }
                }
            }
        }
        if errors > 0 {
            app.set_status(format!("Added {} files ({} errors)", count, errors));
        } else {
            app.set_status(format!("Added {} files from folder", count));
        }
    } else {
        match app.manager.add_file_blocking(path.to_path_buf(), options) {
            Ok(_) => app.set_status(format!("Added: {}", path.file_name().unwrap_or_default().to_string_lossy())),
            Err(e) => app.set_status(format!("Error: {}", e)),
        }
    }

    app.manager.save_queue(app.config.conversion.persist_queue).ok();
}

fn start_conversion(app: &mut AppState, tx: mpsc::Sender<AppMessage>) {
    // Check for not-configured items (queue-screen-specific message)
    let not_configured = app.items_snapshot.iter()
        .filter(|i| matches!(i.status, ConversionStatus::NotConfigured))
        .count();
    if not_configured > 0 {
        let queued = app.items_snapshot.iter()
            .filter(|i| matches!(i.status, ConversionStatus::Queued))
            .count();
        if queued == 0 {
            app.set_status("Items not configured. Press 'c' to configure first.");
            return;
        }
    }

    super::convert_actions::start_processing(app, &tx);
}

fn retry_failed(app: &mut AppState) {
    if let Ok(mut queue) = app.manager.queue.try_write() {
        queue.retry_failed();
    }
    app.set_status("Re-queued failed items for retry");
}

/// Handle mouse events
pub fn handle_mouse(app: &mut AppState, mouse: MouseEvent, tx: &mpsc::Sender<AppMessage>) {
    // Scroll wheel: route to the browse list if the cursor is over it.
    if matches!(mouse.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) {
        if app.current_screen == AppScreen::Browse {
            let over_list = matches!(
                app.button_map.find_button_at(mouse.column, mouse.row),
                Some(TuiButton::BrowseList)
                    | Some(TuiButton::BrowseEntry(_))
                    | Some(TuiButton::BrowseColumn(_))
            );
            if over_list {
                match mouse.kind {
                    MouseEventKind::ScrollUp => app.browse.scroll_viewport(-3),
                    MouseEventKind::ScrollDown => app.browse.scroll_viewport(3),
                    _ => {}
                }
            }
        }
        return;
    }

    // Desktop-standard: any click while a context menu is open closes
    // it. Additionally:
    // - Right-click: close old menu, fall through to open new menu at
    //   the new position (with cursor moved to the clicked item).
    // - Left-click on a selectable item (browse entry, queue item):
    //   close menu AND select the clicked item (move cursor). Does NOT
    //   activate/open — just selects. Matches Windows/macOS behavior.
    // - Left-click on empty space or non-selectable area: close only.
    if matches!(app.active_overlay, ActiveOverlay::ContextMenu { .. }) {
        if matches!(mouse.kind, MouseEventKind::Down(_)) {
            app.active_overlay = ActiveOverlay::None;

            if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
                // Fall through to the right-click handler below, which
                // moves the cursor and opens a new menu.
            } else {
                // Left/middle-click: close + select the clicked item
                // (if it's a selectable entry), but don't trigger any
                // action. Clicking empty space just closes.
                let x = mouse.column;
                let y = mouse.row;
                match app.current_screen {
                    AppScreen::Browse => {
                        if let Some(super::button_map::TuiButton::BrowseEntry(idx)) =
                            app.button_map.find_button_at(x, y)
                        {
                            app.browse.selected_index = idx;
                            app.browse.ensure_visible();
                        }
                    }
                    AppScreen::Queue => {
                        if let Some(super::button_map::TuiButton::QueueItem(idx)) =
                            app.button_map.find_button_at(x, y)
                        {
                            app.selected_index = idx;
                            app.ensure_visible();
                        }
                    }
                    _ => {}
                }
                return;
            }
        }
    }

    // Right-click → select the clicked target, then open its context
    // menu. Desktop-standard: right-click on a file selects it AND
    // shows the menu, just like Windows Explorer / macOS Finder.
    // Skip if another (non-context-menu) overlay is already active.
    if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
        if matches!(app.active_overlay, ActiveOverlay::None) {
            let x = mouse.column;
            let y = mouse.row;

            // Move the cursor to the right-clicked target so the menu
            // is built for THAT item, not whatever was selected before.
            match app.current_screen {
                AppScreen::Browse => {
                    if let Some(super::button_map::TuiButton::BrowseEntry(idx)) =
                        app.button_map.find_button_at(x, y)
                    {
                        app.browse.selected_index = idx;
                        app.browse.ensure_visible();
                    }
                }
                AppScreen::Queue => {
                    if let Some(super::button_map::TuiButton::QueueItem(idx)) =
                        app.button_map.find_button_at(x, y)
                    {
                        app.selected_index = idx;
                        app.ensure_visible();
                    }
                }
                _ => {}
            }

            open_context_menu(app, x, y);
        }
        return;
    }

    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return;
    }

    let x = mouse.column;
    let y = mouse.row;

    // If wizard is active, forward to wizard
    if app.current_screen == AppScreen::Wizard {
        if let Some(wizard) = &mut app.wizard {
            let button_id = app.wizard_mouse_areas.as_ref()
                .and_then(|areas| areas.get_button_at(x, y));
            wizard.handle_mouse(mouse, button_id);

            if wizard.should_start_conversion {
                let (format, options) = crate::convert::extract_wizard_settings(wizard);
                app.wizard = None;
                app.wizard_mouse_areas = None;
                app.current_screen = AppScreen::Queue;

                let queue = app.manager.queue.clone();
                if let Ok(mut q) = queue.try_write() {
                    let has_selected = q.all_items().iter().any(|i| i.selected);
                    for item in q.all_items_mut() {
                        if has_selected && !item.selected { continue; }
                        match item.status {
                            ConversionStatus::NotConfigured
                            | ConversionStatus::Queued
                            | ConversionStatus::Paused => {
                                item.output_format = options.output_format;
                                item.options = options.clone();
                                item.status = ConversionStatus::Queued;
                            }
                            _ => {}
                        }
                    }
                }
                app.set_status(format!("Configured items for {} conversion", format.name()));
            } else if wizard.should_exit {
                app.wizard = None;
                app.wizard_mouse_areas = None;
                app.current_screen = AppScreen::Convert;
            }
        }
        return;
    }

    // Don't dispatch button clicks while a modal overlay (TextEdit,
    // FileInput, Confirmation, etc.) is open — the user must close it
    // first. Without this guard, clicking a different field while
    // editing silently replaces the overlay and loses the pending edit.
    // Context menu dismissal is handled separately above.
    if !matches!(app.active_overlay, ActiveOverlay::None) {
        return;
    }

    // Check button map
    if let Some(button) = app.button_map.find_button_at(x, y) {
        match button {
            // ── Convert screen: pane focus ──
            TuiButton::Pane(focus) => {
                app.convert.focus = focus;
                app.current_screen = AppScreen::Convert;
            }

            // ── Convert screen: tab bar ──
            TuiButton::Tab(n) => {
                match n {
                    1 => {
                        app.current_screen = AppScreen::Browse;
                        app.browse.probe_current(tx);
                    }
                    2 => app.current_screen = AppScreen::Library,
                    3 => app.current_screen = AppScreen::Convert,
                    4 => app.current_screen = AppScreen::Queue,
                    5 => app.current_screen = AppScreen::Config,
                    _ => {}
                }
            }

            // ── Convert screen: preset bar ──
            TuiButton::PresetsButton => {
                app.preset.overlay_list = super::presets::list_presets();
                app.preset.overlay_selected = 0;
                app.preset.naming_input = None;
                app.preset.overlay_open = true;
            }
            TuiButton::SaveButton => {
                if let Some(name) = &app.preset.active_preset.clone() {
                    let preset = super::presets::TuiPreset::from_pill_state(
                        name,
                        &app.convert.format,
                        &app.convert.output_options,
                    );
                    match super::presets::save_preset(&preset) {
                        Ok(_) => {
                            app.preset.modified = false;
                            app.set_status(format!("Saved preset: {}", name));
                        }
                        Err(e) => app.set_status(format!("Save failed: {}", e)),
                    }
                } else {
                    // No active preset — open overlay in naming mode
                    app.preset.overlay_list = super::presets::list_presets();
                    app.preset.overlay_selected = 0;
                    app.preset.naming_input = Some(super::text_input::TextInputState::empty());
                    app.preset.overlay_open = true;
                }
            }

            // ── Convert screen: editable text fields ──
            TuiButton::DestPathField => {
                let initial = app.convert.output_options.dest_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                app.convert.focus = ConvertFocus::OutputOptions;
                app.active_overlay = ActiveOverlay::TextEdit {
                    input: super::text_input::TextInputState::new(initial),
                    target: TextEditTarget::DestPath,
                    label: "destination path".to_string(),
                };
            }
            TuiButton::FolderTemplateField => {
                let initial = app.convert.output_options.folder_template.clone();
                app.convert.focus = ConvertFocus::OutputOptions;
                app.active_overlay = ActiveOverlay::TextEdit {
                    input: super::text_input::TextInputState::new(initial),
                    target: TextEditTarget::FolderTemplate,
                    label: "folder template".to_string(),
                };
            }
            TuiButton::FilenameTemplateField => {
                let initial = app.convert.output_options.filename_template.clone();
                app.convert.focus = ConvertFocus::OutputOptions;
                app.active_overlay = ActiveOverlay::TextEdit {
                    input: super::text_input::TextInputState::new(initial),
                    target: TextEditTarget::FilenameTemplate,
                    label: "filename template".to_string(),
                };
            }
            TuiButton::SourceBrowseButton => {
                app.browse.return_target = super::browse::BrowseReturnTarget::ConvertSource;
                app.current_screen = AppScreen::Browse;
                app.browse.probe_current(tx);
            }
            TuiButton::SourceExpandButton => {
                // Open the BatchList overlay if a batch is loaded.
                if app.convert.source.mode.is_batch() {
                    app.active_overlay = ActiveOverlay::BatchList { scroll: 0 };
                }
            }
            TuiButton::MetadataField(field) => {
                use super::button_map::MetadataFieldKind::*;
                let (initial, target, label) = match field {
                    Title => (
                        app.convert.metadata.title.clone().unwrap_or_default(),
                        TextEditTarget::MetaTitle,
                        "title",
                    ),
                    Artist => (
                        app.convert.metadata.artist.clone().unwrap_or_default(),
                        TextEditTarget::MetaArtist,
                        "artist",
                    ),
                    Album => (
                        app.convert.metadata.album.clone().unwrap_or_default(),
                        TextEditTarget::MetaAlbum,
                        "album",
                    ),
                    Genre => (
                        app.convert.metadata.genre.clone().unwrap_or_default(),
                        TextEditTarget::MetaGenre,
                        "genre",
                    ),
                    Year => (
                        app.convert.metadata.year.clone().unwrap_or_default(),
                        TextEditTarget::MetaYear,
                        "year",
                    ),
                };
                app.convert.focus = ConvertFocus::Metadata;
                app.active_overlay = ActiveOverlay::TextEdit {
                    input: super::text_input::TextInputState::new(initial),
                    target,
                    label: label.to_string(),
                };
            }

            // ── Convert screen: advanced toggle per pane ──
            TuiButton::AdvancedToggle(focus) => {
                app.convert.focus = focus;
                match focus {
                    ConvertFocus::Source => {
                        app.convert.source.advanced_open = !app.convert.source.advanced_open;
                    }
                    ConvertFocus::Metadata => {
                        app.convert.metadata.advanced_open = !app.convert.metadata.advanced_open;
                    }
                    ConvertFocus::Format => {
                        app.convert.format.advanced_open = !app.convert.format.advanced_open;
                    }
                    ConvertFocus::OutputOptions => {
                        app.convert.output_options.advanced_open =
                            !app.convert.output_options.advanced_open;
                    }
                }
            }

            // ── Convert screen: format pane pills ──
            TuiButton::FormatPill(i) => {
                app.convert.focus = ConvertFocus::Format;
                app.convert.format.field_focus = FormatField::Format;
                if i < app.convert.format.format.options.len()
                    && app.convert.format.format.options[i].enabled
                {
                    app.convert.format.format.selected = i;
                    app.convert.format.apply_format_constraints();
                    app.preset.mark_modified();
                }
            }
            TuiButton::RatePill(i) => {
                app.convert.focus = ConvertFocus::Format;
                app.convert.format.field_focus = FormatField::SampleRate;
                if i < app.convert.format.sample_rate.options.len()
                    && app.convert.format.sample_rate.options[i].enabled
                {
                    app.convert.format.sample_rate.selected = i;
                    app.preset.mark_modified();
                }
            }
            TuiButton::DepthPill(i) => {
                app.convert.focus = ConvertFocus::Format;
                app.convert.format.field_focus = FormatField::BitDepth;
                if i < app.convert.format.bit_depth.options.len()
                    && app.convert.format.bit_depth.options[i].enabled
                {
                    app.convert.format.bit_depth.selected = i;
                    app.preset.mark_modified();
                }
            }
            TuiButton::DitherPill(i) => {
                app.convert.focus = ConvertFocus::Format;
                app.convert.format.field_focus = FormatField::Dither;
                if i < app.convert.format.dither.options.len()
                    && app.convert.format.dither.options[i].enabled
                {
                    app.convert.format.dither.selected = i;
                    app.preset.mark_modified();
                }
            }
            TuiButton::ReplayGainPill(i) => {
                app.convert.focus = ConvertFocus::Format;
                app.convert.format.field_focus = FormatField::ReplayGain;
                if i < app.convert.format.replaygain.options.len()
                    && app.convert.format.replaygain.options[i].enabled
                {
                    app.convert.format.replaygain.selected = i;
                    app.preset.mark_modified();
                }
            }
            TuiButton::MergePill(i) => {
                app.convert.focus = ConvertFocus::OutputOptions;
                app.convert.output_options.field_focus = OutputOptionsField::MergeMode;
                if i < app.convert.output_options.merge.options.len()
                    && app.convert.output_options.merge.options[i].enabled
                {
                    app.convert.output_options.merge.selected = i;
                    app.preset.mark_modified();
                }
            }

            // ── Queue screen buttons ──
            TuiButton::QueueItem(idx) => {
                app.selected_index = idx;
                app.queue_focus = QueueFocus::FileList;
            }
            TuiButton::AddFiles | TuiButton::AddFolder => {
                app.active_overlay = ActiveOverlay::FileInput {
                    input: super::text_input::TextInputState::empty(),
                };
            }
            TuiButton::Configure => {
                let has_selected = app.items_snapshot.iter().any(|i| i.selected);
                app.wizard_target = if has_selected {
                    WizardTarget::ConfigureSelected
                } else {
                    WizardTarget::ConfigureAll
                };
                app.wizard = Some(tonepoet_wizard::SimpleWizard::new());
                app.current_screen = AppScreen::Wizard;
            }
            TuiButton::Convert => {
                if !app.processing_active {
                    let tx_clone = tx.clone();
                    start_conversion(app, tx_clone);
                }
            }
            TuiButton::Pause => {
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
            TuiButton::Stop => {
                if app.processing_active {
                    app.active_overlay = ActiveOverlay::Confirmation {
                        message: "Stop all active conversions?".to_string(),
                        action: ConfirmAction::StopAll,
                    };
                }
            }
            TuiButton::ClearCompleted => {
                app.manager.clear_completed();
                app.set_status("Cleared completed items");
            }
            TuiButton::RetryFailed => {
                retry_failed(app);
            }

            // ── Browse screen ──
            TuiButton::BrowseEntry(idx) => {
                use super::browse::EntryKind;

                if idx >= app.browse.entries.len() {
                    return;
                }

                let clicked_path = app.browse.entries[idx].path.clone();
                let alt = mouse.modifiers.contains(KeyModifiers::ALT);
                let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);

                // For alt-click, we need the anchor *before* moving the cursor.
                // Otherwise resolve_anchor_index's fallback (which uses
                // selected_index) would collapse to `idx` and give an empty range.
                // Also persist the anchor if it's not yet set, so a sequence of
                // alt-clicks behaves Finder-like (fixed origin until a plain click).
                let prev_cursor = app.browse.selected_index;
                let alt_anchor_idx = if alt {
                    let idx_from_path = app.browse.multi_select_anchor.as_ref().and_then(
                        |p| app.browse.entries.iter().position(|e| e.path == *p),
                    );
                    let resolved = idx_from_path.unwrap_or(prev_cursor);
                    // Persist the anchor so subsequent alt-clicks keep this origin.
                    if app.browse.multi_select_anchor.is_none() {
                        if let Some(entry) = app.browse.entries.get(resolved) {
                            app.browse.multi_select_anchor = Some(entry.path.clone());
                        }
                    }
                    Some(resolved)
                } else {
                    None
                };

                // All three click modes move the cursor to the clicked entry.
                app.browse.selected_index = idx;
                app.browse.ensure_visible();

                if alt {
                    // ── Alt+click: range-select from anchor to clicked entry ──
                    let anchor_idx = alt_anchor_idx.unwrap();
                    let lo = anchor_idx.min(idx);
                    let hi = anchor_idx.max(idx);
                    let to_add: Vec<std::path::PathBuf> = (lo..=hi)
                        .filter_map(|i| app.browse.entries.get(i))
                        .filter(|e| !e.is_dir())
                        .map(|e| e.path.clone())
                        .collect();
                    for p in to_add {
                        if !app.browse.multi_selected.iter().any(|sp| *sp == p) {
                            app.browse.multi_selected.push(p);
                        }
                    }
                    // Anchor unchanged. Clear click tracking so alt+click never
                    // contributes to rename/double-click timing.
                    app.last_browse_click = None;
                    app.pending_browse_rename = None;
                    app.browse.probe_current(tx);
                } else if shift {
                    // ── Shift+click: toggle clicked entry in multi_selected ──
                    // toggle_selection operates on the current cursor (which we
                    // just moved to `idx`).
                    app.browse.toggle_selection();
                    // Anchor unchanged. Clear click tracking.
                    app.last_browse_click = None;
                    app.pending_browse_rename = None;
                    app.browse.probe_current(tx);
                } else {
                    // ── Plain click: open vs schedule-rename vs fresh ──
                    //
                    // Windows/macOS semantics:
                    // - Click within the double-click window of the prior click
                    //   on the same path → open immediately (cancel any pending
                    //   rename).
                    // - Any click on the same path as the prior click OUTSIDE
                    //   the double-click window SCHEDULES a rename, which
                    //   commits after another double-click-window delay unless
                    //   a subsequent click cancels it.
                    // - Click on a different path → fresh click. Cancel pending.
                    //
                    // This preserves the "click, wait 5s, double-click = open"
                    // flow: the wait-then-click schedules rename, but the
                    // immediate follow-up click cancels it and fires open.
                    const OPEN_MS: u64 = 500;

                    let now = std::time::Instant::now();
                    let is_double_click = app
                        .last_browse_click
                        .as_ref()
                        .filter(|(p, _)| *p == clicked_path)
                        .map(|(_, t)| now.duration_since(*t).as_millis() < OPEN_MS as u128)
                        .unwrap_or(false);

                    if is_double_click {
                        // Double-click: open/load immediately, cancel any pending rename.
                        app.last_browse_click = None;
                        app.pending_browse_rename = None;
                        let entry_kind = app.browse.entries[idx].kind.clone();
                        match entry_kind {
                            EntryKind::Directory | EntryKind::ParentDir => {
                                app.browse.enter_selected();
                                app.browse.probe_current(tx);
                            }
                            EntryKind::AudioFile(_) | EntryKind::Archive => {
                                let target = app.browse.return_target;
                                load_browse_selection(app, clicked_path, target);
                            }
                            EntryKind::OtherFile => {
                                app.set_status("Not an audio file");
                            }
                        }
                    } else {
                        // Not a double-click. Any click cancels any pending
                        // rename (the user is acting, not waiting).
                        app.pending_browse_rename = None;

                        let same_path_as_last = app
                            .last_browse_click
                            .as_ref()
                            .map(|(p, _)| *p == clicked_path)
                            .unwrap_or(false);

                        if same_path_as_last {
                            // Same-path click outside the double-click window.
                            // Schedule a rename for +OPEN_MS. A subsequent
                            // click within that window will cancel it (→ open).
                            // Don't schedule for ParentDir.
                            if !matches!(
                                app.browse.entries[idx].kind,
                                EntryKind::ParentDir
                            ) {
                                app.pending_browse_rename = Some((
                                    clicked_path.clone(),
                                    now + std::time::Duration::from_millis(OPEN_MS),
                                ));
                            }
                        }

                        // Record this click and anchor.
                        app.last_browse_click = Some((clicked_path.clone(), now));
                        app.browse.multi_select_anchor = Some(clicked_path);
                        app.browse.probe_current(tx);
                    }
                }
            }
            TuiButton::BrowseColumn(col) => {
                use super::browse::{SortBy, SortDir};
                use super::button_map::ColumnKind;
                let target = match col {
                    ColumnKind::Name => SortBy::Name,
                    ColumnKind::Size => SortBy::Size,
                    ColumnKind::Date => SortBy::Date,
                    ColumnKind::Type => SortBy::Type,
                };
                if app.browse.sort_by == target {
                    app.browse.toggle_sort_dir();
                } else {
                    app.browse.set_sort(target, SortDir::Asc);
                }
            }
            TuiButton::BrowseInfoMeta(field) => {
                // Click a metadata field in the info pane → open tag editor.
                super::command::execute_edit_metadata_pub(app, field);
            }
            TuiButton::BrowseList => {
                // Catch-all for scroll routing only; ignore on left click.
            }

            // ── Overlay buttons ──
            TuiButton::OverlayConfirm => {
                if let ActiveOverlay::Confirmation { action, .. } = &app.active_overlay {
                    let action = action.clone();
                    app.active_overlay = ActiveOverlay::None;
                    execute_confirm_action(app, &action, tx);
                }
            }
            TuiButton::OverlayCancel => {
                app.active_overlay = ActiveOverlay::None;
            }
        }
    }
}
