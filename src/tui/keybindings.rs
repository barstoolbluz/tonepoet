//! Key event dispatch by screen/focus

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use tokio::sync::mpsc;

use super::app::*;
use super::button_map::TuiButton;
use super::message::AppMessage;
use crate::convert::{ConversionOptions, ConversionStatus};

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
    if app.current_screen == AppScreen::Browse
        && app.browse.search.active
        && app.browse.search.focus != super::browse::SearchFocus::Results
    {
        handle_browse_search_key(app, key, tx);
        return;
    }

    if app.current_screen == AppScreen::Browse && app.browse.path_input.is_some() {
        handle_browse_path_input_key(app, key);
        return;
    }

    if app.current_screen == AppScreen::Browse && app.browse.filter_input.is_some() {
        handle_browse_filter_key(app, key, tx);
        return;
    }

    // Global keys (except in Wizard mode)
    if app.current_screen != AppScreen::Wizard {
        match (key.code, key.modifiers) {
            // Quit via Ctrl+Q (intentional modifier prevents accidental exits).
            (KeyCode::Char('q'), KeyModifiers::CONTROL) => {
                app.should_quit = true;
                return;
            }
            (KeyCode::Char('1'), KeyModifiers::NONE) => {
                app.current_screen = AppScreen::Browse;
                app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
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
            (KeyCode::Char(':'), KeyModifiers::SHIFT)
            | (KeyCode::Char(':'), KeyModifiers::NONE) => {
                app.active_overlay = ActiveOverlay::CommandInput {
                    input: super::text_input::TextInputState::empty(),
                    completion: None,
                };
                return;
            }
            // Help overlay
            (KeyCode::Char('?'), _) => {
                app.active_overlay = ActiveOverlay::Help {
                    screen: app.current_screen,
                    scroll: 0,
                };
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
                app.current_screen = AppScreen::from_config_name(&app.config.ui.default_screen);
                if app.current_screen == AppScreen::Browse {
                    app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
                }
                return;
            }
            AppScreen::Browse => {
                // Let handle_browse_key handle Esc — it clears multi-selection first
            }
            AppScreen::Queue => {
                // Esc from Queue returns to the user's configured default
                // screen (home). Default: Browse.
                app.current_screen = AppScreen::from_config_name(&app.config.ui.default_screen);
                if app.current_screen == AppScreen::Browse {
                    app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
                }
                return;
            }
            AppScreen::Convert => {
                // If we arrived via :queue from another screen (previous_screen
                // is set), Esc cancels the batch review and returns to origin.
                // Overlays have already been dispatched earlier in handle_key,
                // so at this point Convert has the key exclusively.
                if app.previous_screen.is_some() {
                    app.convert.set_source_mode(SourceMode::Empty);
                    app.convert.metadata = MetadataState::default();
                    app.convert.layout = ConvertLayout::Default;
                    app.convert.pane_title_last_click = None;
                    app.convert.metadata_file_last_click = None;
                    let origin = app.previous_screen.take().unwrap_or(AppScreen::Browse);
                    app.current_screen = origin;
                    if origin == AppScreen::Browse {
                        app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
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

fn handle_convert_key(app: &mut AppState, key: KeyEvent, tx: &mpsc::Sender<AppMessage>) {
    match (key.code, key.modifiers) {
        // Tab between panes. This remains unconditional in both Default and Maximized modes.
        (KeyCode::Tab, KeyModifiers::NONE) => {
            app.convert.focus = app.convert.focus.next();
        }
        (KeyCode::BackTab, KeyModifiers::SHIFT) => {
            app.convert.focus = app.convert.focus.prev();
        }

        // Source pane + MultiTrack navigation.
        (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::Source
                && !app.convert.is_collapsed(ConvertFocus::Source)
                && matches!(&app.convert.source.mode, SourceMode::MultiTrack { .. }) =>
        {
            if let SourceMode::MultiTrack { cursor, scroll, .. } = &mut app.convert.source.mode {
                if *cursor > 0 {
                    *cursor -= 1;
                    if *cursor < *scroll {
                        *scroll = *cursor;
                    }
                }
            }
        }
        (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::Source
                && !app.convert.is_collapsed(ConvertFocus::Source)
                && matches!(&app.convert.source.mode, SourceMode::MultiTrack { .. }) =>
        {
            if let SourceMode::MultiTrack { cursor, scroll, tracks, .. } = &mut app.convert.source.mode {
                if *cursor + 1 < tracks.len() {
                    *cursor += 1;
                    if *cursor >= *scroll + 6 {
                        *scroll = cursor.saturating_sub(5);
                    }
                }
            }
        }
        (KeyCode::Char(' '), KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::Source
                && !app.convert.is_collapsed(ConvertFocus::Source)
                && matches!(&app.convert.source.mode, SourceMode::MultiTrack { .. }) =>
        {
            if let SourceMode::MultiTrack { cursor, selected, .. } = &mut app.convert.source.mode {
                if let Some(sel) = selected.get_mut(*cursor) {
                    *sel = !*sel;
                }
            }
        }

        // Metadata pane file-list navigation. Batch mode reuses the source batch cursor.
        (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::Metadata
                && !app.convert.is_collapsed(ConvertFocus::Metadata)
                && matches!(&app.convert.source.mode, SourceMode::Batch { .. } | SourceMode::MultiTrack { .. }) =>
        {
            move_metadata_cursor(app, -1, tx);
        }
        (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::Metadata
                && !app.convert.is_collapsed(ConvertFocus::Metadata)
                && matches!(&app.convert.source.mode, SourceMode::Batch { .. } | SourceMode::MultiTrack { .. }) =>
        {
            move_metadata_cursor(app, 1, tx);
        }
        (KeyCode::Enter, KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::Metadata
                && !app.convert.is_collapsed(ConvertFocus::Metadata)
                && matches!(&app.convert.source.mode, SourceMode::Batch { .. } | SourceMode::MultiTrack { .. }) =>
        {
            open_convert_cursor_metadata_editor(app);
        }

        // Source pane: stream pill cycling (Left/Right) for multi-presentation discs.
        (KeyCode::Left | KeyCode::Char('h'), KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::Source
                && !app.convert.is_collapsed(ConvertFocus::Source)
                && matches!(&app.convert.source.mode, SourceMode::MultiTrack { disc_contents: Some(ref dc), .. } if dc.presentations.len() >= 2) =>
        {
            cycle_stream_pill(app, false);
        }
        (KeyCode::Right | KeyCode::Char('l'), KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::Source
                && !app.convert.is_collapsed(ConvertFocus::Source)
                && matches!(&app.convert.source.mode, SourceMode::MultiTrack { disc_contents: Some(ref dc), .. } if dc.presentations.len() >= 2) =>
        {
            cycle_stream_pill(app, true);
        }

        // Format pane navigation.
        (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::Format
                && !app.convert.is_collapsed(ConvertFocus::Format) =>
        {
            app.convert.format.focus_prev();
        }
        (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::Format
                && !app.convert.is_collapsed(ConvertFocus::Format) =>
        {
            app.convert.format.focus_next();
        }
        (KeyCode::Left | KeyCode::Char('h'), KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::Format
                && !app.convert.is_collapsed(ConvertFocus::Format) =>
        {
            super::format_interactions::handle_convert_format_row_step(&mut app.convert, false);
            app.preset.mark_modified();
        }
        (KeyCode::Right | KeyCode::Char('l'), KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::Format
                && !app.convert.is_collapsed(ConvertFocus::Format) =>
        {
            super::format_interactions::handle_convert_format_row_step(&mut app.convert, true);
            app.preset.mark_modified();
        }

        // Output options pane navigation.
        (KeyCode::Up | KeyCode::Char('k'), KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::OutputOptions
                && !app.convert.is_collapsed(ConvertFocus::OutputOptions) =>
        {
            app.convert.output_options.field_focus = app.convert.output_options.field_focus.prev();
        }
        (KeyCode::Down | KeyCode::Char('j'), KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::OutputOptions
                && !app.convert.is_collapsed(ConvertFocus::OutputOptions) =>
        {
            app.convert.output_options.field_focus = app.convert.output_options.field_focus.next();
        }
        (KeyCode::Left | KeyCode::Char('h'), KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::OutputOptions
                && !app.convert.is_collapsed(ConvertFocus::OutputOptions)
                && app.convert.output_options.field_focus == OutputOptionsField::MergeMode =>
        {
            app.convert.output_options.merge.select_prev();
            app.preset.mark_modified();
        }
        (KeyCode::Right | KeyCode::Char('l'), KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::OutputOptions
                && !app.convert.is_collapsed(ConvertFocus::OutputOptions)
                && app.convert.output_options.field_focus == OutputOptionsField::MergeMode =>
        {
            app.convert.output_options.merge.select_next();
            app.preset.mark_modified();
        }

        // Source pane default action.
        (KeyCode::Char('e'), KeyModifiers::NONE) | (KeyCode::Enter, KeyModifiers::NONE)
            if app.convert.focus == ConvertFocus::Source
                && !app.convert.is_collapsed(ConvertFocus::Source) =>
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

        // Open presets overlay.
        (KeyCode::Char('p'), KeyModifiers::NONE) => {
            app.preset.overlay_list = super::presets::list_presets();
            app.preset.overlay_selected = 0;
            app.preset.naming_input = None;
            app.preset.overlay_open = true;
        }

        // Save preset.
        (KeyCode::Char('s'), KeyModifiers::NONE) => {
            if let Some(name) = &app.preset.active_preset.clone() {
                let preset = super::presets::TuiPreset::from_pill_state(
                    name,
                    &app.convert.format,
                    &app.convert.output_options,
                );
                match super::presets::save_preset_with_db(&preset, &app.db) {
                    Ok(_) => {
                        app.preset.modified = false;
                        app.set_status(format!("Saved preset: {}", name));
                    }
                    Err(e) => app.set_status(format!("Save failed: {}", e)),
                }
            } else {
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
                match super::presets::save_preset_with_db(&preset, &app.db) {
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
            if let Some(name) = app
                .preset
                .overlay_list
                .get(app.preset.overlay_selected)
                .cloned()
            {
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
            if let Some(name) = app
                .preset
                .overlay_list
                .get(app.preset.overlay_selected)
                .cloned()
            {
                match super::presets::load_preset(&name) {
                    Ok(mut preset) => {
                        let base = format!("{}-copy", name);
                        let new_name = super::presets::find_unique_preset_name(
                            &base,
                            &app.preset.overlay_list,
                        );
                        preset.name = new_name;
                        match super::presets::save_preset_with_db(&preset, &app.db) {
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
            if let Some(name) = app
                .preset
                .overlay_list
                .get(app.preset.overlay_selected)
                .cloned()
            {
                match super::presets::delete_preset_with_db(&name, &app.db) {
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
        // Navigation (extends visual selection if V-mode is active)
        (KeyCode::Up, KeyModifiers::NONE) => {
            app.browse.move_up();
            if app.browse.visual_mode {
                app.browse.update_visual_selection();
            }
            selection_may_have_changed = true;
        }
        (KeyCode::Down, KeyModifiers::NONE) => {
            app.browse.move_down();
            if app.browse.visual_mode {
                app.browse.update_visual_selection();
            }
            selection_may_have_changed = true;
        }
        (KeyCode::Home, _) => {
            app.browse.move_top();
            if app.browse.visual_mode {
                app.browse.update_visual_selection();
            }
            selection_may_have_changed = true;
        }
        (KeyCode::End, _) => {
            app.browse.move_bottom();
            if app.browse.visual_mode {
                app.browse.update_visual_selection();
            }
            selection_may_have_changed = true;
        }
        (KeyCode::PageUp, _) => {
            app.browse.page_up();
            if app.browse.visual_mode {
                app.browse.update_visual_selection();
            }
            selection_may_have_changed = true;
        }
        (KeyCode::PageDown, _) => {
            app.browse.page_down();
            if app.browse.visual_mode {
                app.browse.update_visual_selection();
            }
            selection_may_have_changed = true;
        }

        // Go up (parent directory / archive level)
        (KeyCode::Left, KeyModifiers::NONE) => {
            if app.browse.is_in_archive() {
                if !app.browse.go_up_in_archive() {
                    app.browse.exit_archive();
                }
            } else {
                app.browse.go_parent();
            }
            selection_may_have_changed = true;
        }

        // Backspace: delete last char from type-ahead buffer when active,
        // otherwise go to parent directory.
        (KeyCode::Backspace, _) => {
            if app.browse.type_ahead_active() {
                app.browse.type_ahead_pop();
                selection_may_have_changed = true;
            } else {
                if app.browse.is_in_archive() {
                    if !app.browse.go_up_in_archive() {
                        app.browse.exit_archive();
                    }
                } else {
                    app.browse.go_parent();
                }
                selection_may_have_changed = true;
            }
        }

        // Delete: move selected file(s) to trash (with confirmation)
        (KeyCode::Delete, KeyModifiers::NONE) => {
            super::command::execute_command(app, super::command::Command::Delete, tx);
        }

        // Enter directory/archive or select file
        (KeyCode::Right, KeyModifiers::NONE) => {
            if let Some(entry) = app.browse.selected_entry() {
                if entry.is_dir() {
                    if app.browse.is_in_archive() {
                        // Navigate into subdirectory inside archive.
                        let item_name = entry
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let inner = if let Some(ref arc) = app.browse.archive {
                            if arc.inner_path.is_empty() {
                                item_name
                            } else {
                                format!("{}/{}", arc.inner_path, item_name)
                            }
                        } else {
                            item_name
                        };
                        app.browse.enter_archive_dir(&inner);
                    } else {
                        app.browse.enter_selected();
                    }
                    selection_may_have_changed = true;
                }
            }
        }
        (KeyCode::Enter, KeyModifiers::NONE) => {
            if let Some(entry) = app.browse.selected_entry() {
                match &entry.kind {
                    EntryKind::Directory | EntryKind::ParentDir if app.browse.is_in_archive() => {
                        if matches!(entry.kind, EntryKind::ParentDir) {
                            if !app.browse.go_up_in_archive() {
                                app.browse.exit_archive();
                            }
                        } else {
                            let item_name = entry
                                .path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default();
                            let inner = if let Some(ref arc) = app.browse.archive {
                                if arc.inner_path.is_empty() {
                                    item_name
                                } else {
                                    format!("{}/{}", arc.inner_path, item_name)
                                }
                            } else {
                                item_name
                            };
                            app.browse.enter_archive_dir(&inner);
                        }
                        selection_may_have_changed = true;
                    }
                    EntryKind::Directory | EntryKind::ParentDir => {
                        app.browse.enter_selected();
                        selection_may_have_changed = true;
                    }
                    EntryKind::Archive if !app.browse.is_in_archive() => {
                        // Open archive for browsing: async list contents.
                        let path = entry.path.clone();
                        let password = app
                            .archive_passwords
                            .get(&path)
                            .cloned()
                            .or_else(|| {
                                app.keychain.ensure_loaded();
                                app.keychain.passwords.first().cloned()
                            })
                            .or_else(|| app.config.conversion.archive_password.clone());
                        let tx = tx.clone();
                        app.set_status("Loading archive...");
                        tokio::spawn(async move {
                            let result =
                                super::archive_listing::list_archive(&path, password.as_deref())
                                    .await;
                            let _ = tx
                                .send(AppMessage::ArchiveListingComplete {
                                    archive_path: path,
                                    result: Box::new(result),
                                    password,
                                })
                                .await;
                        });
                    }
                    EntryKind::AudioFile(_) if app.browse.is_in_archive() => {
                        // Inside an archive: extract selected files to temp before loading.
                        // The synthetic path is archive_path/inner_path — extract the inner part.
                        if let Some(ref arc) = app.browse.archive {
                            let archive_path = arc.listing.archive_path.clone();
                            let password = arc.password.clone();
                            // Derive inner path from the synthetic path.
                            let inner = entry
                                .path
                                .strip_prefix(&archive_path)
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_default();
                            if !inner.is_empty() {
                                let tx = tx.clone();
                                let scratch = app.config.conversion.scratch_directory.clone();
                                app.set_status("Extracting from archive...");
                                tokio::spawn(async move {
                                    let result = extract_from_archive(
                                        &archive_path,
                                        &inner,
                                        password.as_deref(),
                                        scratch.as_deref(),
                                    )
                                    .await;
                                    let _ = tx
                                        .send(AppMessage::StatusMessage(match &result {
                                            Ok(p) => format!("Extracted: {}", p.display()),
                                            Err(e) => format!("Extract failed: {}", e),
                                        }))
                                        .await;
                                });
                            }
                        }
                    }
                    EntryKind::AudioFile(_)
                    | EntryKind::Archive
                    | EntryKind::SacdIso
                    | EntryKind::DvdAudioIso
                    | EntryKind::DvdAudioDir
                    | EntryKind::DvdVideoIso
                    | EntryKind::DvdVideoDir
                    | EntryKind::OtherFile => {
                        // Toggle selection — converting is via context menu or :queue.
                        app.browse.toggle_selection();
                    }
                }
            }
        }

        // Space: literal space in type-ahead when buffer is active,
        // otherwise toggle multi-select for individual items.
        (KeyCode::Char(' '), KeyModifiers::NONE) => {
            if app.browse.type_ahead_active() {
                app.browse.type_ahead_push(' ');
                selection_may_have_changed = true;
            } else {
                app.browse.toggle_selection();
                app.browse.move_down();
                selection_may_have_changed = true;
            }
        }

        // Visual (range) selection mode: Ctrl+V toggles. While active,
        // cursor movement extends the selection from the anchor.
        (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
            if app.browse.visual_mode {
                // Exit visual mode, keep current selection.
                app.browse.visual_mode = false;
                app.set_status("Visual select off");
            } else {
                // Enter visual mode: anchor at current cursor.
                app.browse.visual_mode = true;
                if let Some(entry) = app.browse.entries.get(app.browse.selected_index) {
                    app.browse.multi_select_anchor = Some(entry.path.clone());
                }
                // Select the current entry.
                app.browse.update_visual_selection();
                app.set_status("Visual select on — move cursor to extend range");
            }
        }

        // Toggle hidden files
        (KeyCode::Char('.'), KeyModifiers::NONE) => {
            app.browse.toggle_hidden();
            selection_may_have_changed = true;
        }

        // Open the search panel or refocus the input.
        (KeyCode::Char('/'), KeyModifiers::NONE) | (KeyCode::Char('/'), KeyModifiers::SHIFT) => {
            if app.browse.search.active {
                // Refocus on input (e.g., from Results focus).
                app.browse.search.focus = super::browse::SearchFocus::Input;
            } else {
                app.browse.open_search();
            }
        }

        // Esc escalation: type-ahead → search → visual mode → multi-selection → text filter → archive
        (KeyCode::Esc, _) => {
            if app.browse.type_ahead_active() {
                app.browse.clear_type_ahead();
            } else if app.browse.search.active {
                app.browse.close_search();
            } else if app.browse.visual_mode {
                app.browse.visual_mode = false;
                app.set_status("Visual select off");
            } else if !app.browse.multi_selected.is_empty() {
                app.browse.clear_multi_selection();
            } else if !app.browse.filter_text.is_empty() {
                app.browse.clear_filter();
                selection_may_have_changed = true;
            } else if app.browse.is_in_archive() {
                // Esc in archive: go up one level or exit.
                if !app.browse.go_up_in_archive() {
                    app.browse.exit_archive();
                }
                selection_may_have_changed = true;
            }
            // Browse is home — Esc with nothing to clear is a no-op.
        }

        // Ctrl+E = open metadata editor for selected audio file(s)
        (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
            open_metadata_editor(app);
        }

        // Type-ahead navigation: bare letter/number keys jump to the
        // first entry whose name starts with the accumulated prefix.
        (KeyCode::Char(c), mods) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
            app.browse.type_ahead_push(c);
            if app.browse.visual_mode {
                app.browse.update_visual_selection();
            }
            selection_may_have_changed = true;
        }

        _ => {}
    }

    if selection_may_have_changed {
        app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
    }
}

/// Hybrid dispatcher used while the live text filter input is open.
/// Arrow keys / page keys navigate the filtered list; everything else is
/// fed into the text input. Enter commits, Esc cancels (restores prior filter).
/// Handle keys when the search panel is active.
fn handle_browse_search_key(app: &mut AppState, key: KeyEvent, tx: &mpsc::Sender<AppMessage>) {
    use super::browse::SearchFocus;

    match app.browse.search.focus {
        SearchFocus::Input => {
            match key.code {
                KeyCode::Esc => {
                    app.browse.close_search();
                }
                KeyCode::Enter => {
                    app.browse.execute_search(Some(tx));
                }
                KeyCode::Tab => {
                    app.browse.search.focus = SearchFocus::Recursive;
                }
                KeyCode::Down => {
                    // Move focus to results list.
                    app.browse.execute_search(Some(tx));
                    app.browse.search.focus = super::browse::SearchFocus::Results;
                }
                _ => {
                    super::text_input::handle_text_input_key(&mut app.browse.search.input, &key);
                    app.browse.search.last_keystroke = Some(std::time::Instant::now());
                }
            }
        }
        SearchFocus::Recursive => match key.code {
            KeyCode::Esc => {
                app.browse.close_search();
            }
            KeyCode::Tab => {
                app.browse.search.focus = SearchFocus::Mode;
            }
            KeyCode::BackTab => {
                app.browse.search.focus = SearchFocus::Input;
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                app.browse.search.recursive = !app.browse.search.recursive;
                app.browse.search.last_keystroke = Some(std::time::Instant::now());
            }
            _ => {}
        },
        SearchFocus::Mode => {
            match key.code {
                KeyCode::Esc => {
                    app.browse.close_search();
                }
                KeyCode::Tab => {
                    app.browse.search.focus = SearchFocus::Sort;
                }
                KeyCode::BackTab => {
                    app.browse.search.focus = SearchFocus::Recursive;
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    app.browse.search.mode = app.browse.search.mode.cycle();
                    // If switching away from tags and a tag sort is active, reset to Score.
                    if app.browse.search.mode == super::browse::SearchMode::Filename
                        && app.browse.search.sort.is_tag_sort()
                    {
                        app.browse.search.sort = super::browse::SearchSort::Score;
                        app.browse.search.sort_dir = super::browse::SortDir::Desc;
                    }
                    app.browse.search.last_keystroke = Some(std::time::Instant::now());
                }
                _ => {}
            }
        }
        SearchFocus::Sort => {
            match key.code {
                KeyCode::Esc => {
                    app.browse.close_search();
                }
                KeyCode::Tab => {
                    app.browse.search.focus = SearchFocus::AudioOnly;
                }
                KeyCode::BackTab => {
                    app.browse.search.focus = SearchFocus::Mode;
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    let tag_mode = matches!(
                        app.browse.search.mode,
                        super::browse::SearchMode::Tags | super::browse::SearchMode::Both
                    );
                    app.browse.search.sort = app.browse.search.sort.cycle_with_mode(tag_mode);
                    app.browse.search.sort_dir = match app.browse.search.sort {
                        super::browse::SearchSort::Score => super::browse::SortDir::Desc,
                        _ => super::browse::SortDir::Asc,
                    };
                    app.browse.search.last_keystroke = Some(std::time::Instant::now());
                }
                // Shift+Enter or 'r' to reverse direction.
                KeyCode::Char('r') => {
                    app.browse.search.sort_dir = app.browse.search.sort_dir.toggle();
                    app.browse.search.last_keystroke = Some(std::time::Instant::now());
                }
                _ => {}
            }
        }
        SearchFocus::AudioOnly => match key.code {
            KeyCode::Esc => {
                app.browse.close_search();
            }
            KeyCode::Tab => {
                app.browse.search.focus = SearchFocus::Input;
            }
            KeyCode::BackTab => {
                app.browse.search.focus = SearchFocus::Sort;
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                app.browse.search.audio_only = !app.browse.search.audio_only;
                app.browse.search.last_keystroke = Some(std::time::Instant::now());
            }
            _ => {}
        },
        SearchFocus::Results => {
            // Unreachable — Results focus skips this handler entirely.
        }
    }
}

fn handle_browse_filter_key(app: &mut AppState, key: KeyEvent, tx: &mpsc::Sender<AppMessage>) {
    use super::text_input::handle_text_input_key;

    match (key.code, key.modifiers) {
        (KeyCode::Enter, _) => {
            app.browse.close_filter_input(true);
            app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
        }
        (KeyCode::Esc, _) => {
            app.browse.close_filter_input(false);
            app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
        }
        // List navigation while filter input is open
        (KeyCode::Up, _) => {
            app.browse.move_up();
            app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
        }
        (KeyCode::Down, _) => {
            app.browse.move_down();
            app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
        }
        (KeyCode::PageUp, _) => {
            app.browse.page_up();
            app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
        }
        (KeyCode::PageDown, _) => {
            app.browse.page_down();
            app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
        }
        // Everything else: feed to the text input, then re-apply view
        _ => {
            if let Some(input) = &mut app.browse.filter_input {
                if handle_text_input_key(input, &key) {
                    app.browse.update_filter_from_input();
                    app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
                }
            }
        }
    }
}

fn handle_browse_path_input_key(app: &mut AppState, key: KeyEvent) {
    use super::text_input::handle_text_input_key;

    match (key.code, key.modifiers) {
        (KeyCode::Enter, _) => {
            app.browse.close_path_input(true);
        }
        (KeyCode::Esc, _) => {
            app.browse.close_path_input(false);
        }
        _ => {
            if let Some(input) = &mut app.browse.path_input {
                handle_text_input_key(input, &key);
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

/// Extract a specific file from an archive to a scratch/temp directory.
/// Returns the path to the extracted file on success.
async fn extract_from_archive(
    archive: &std::path::Path,
    inner_path: &str,
    password: Option<&str>,
    scratch: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, String> {
    use tokio::process::Command;

    let bin =
        crate::detect_7z_binary().ok_or_else(|| "neither 7zz nor 7z found in PATH".to_string())?;

    // Extract to a temp directory under scratch or system temp.
    let base = scratch
        .map(|p| p.to_path_buf())
        .unwrap_or_else(std::env::temp_dir);
    let extract_dir = base.join(format!(
        "tonepoet_extract_{}",
        archive
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "archive".into())
    ));
    std::fs::create_dir_all(&extract_dir).map_err(|e| format!("mkdir: {}", e))?;

    let mut cmd = Command::new(bin);
    cmd.arg("x")
        .arg(archive)
        .arg(format!("-o{}", extract_dir.display()))
        .arg("-y")
        .arg(inner_path); // Extract only this specific file.
    if let Some(pw) = password {
        cmd.arg(format!("-p{}", pw));
    }
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("failed to run {}: {}", bin, e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("extraction failed: {}", stderr.trim()));
    }

    let extracted = extract_dir.join(inner_path);
    if extracted.exists() {
        Ok(extracted)
    } else {
        Err("extracted file not found (path mismatch)".into())
    }
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
                    let metadata = crate::tui::probe::read_metadata(&path).unwrap_or_default();
                    app.convert.metadata.title = metadata.title.clone();
                    app.convert.metadata.artist = metadata.artist.clone();
                    app.convert.metadata.album = metadata.album.clone();
                    app.convert.metadata.genre = metadata.genre.clone();
                    app.convert.metadata.year = metadata.year.clone();
                    // Browse Enter loads a single file — abandon any
                    // pending batch from a previous :queue.
                    app.convert.set_source_mode(SourceMode::from_single(
                        path.clone(),
                        Some(info),
                        metadata,
                    ));
                    app.set_status(format!(
                        "Loaded: {}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    app.current_screen = AppScreen::Convert;
                    // Clear the return target so subsequent browse visits don't
                    // auto-load into the source pane.
                    app.browse.return_target = BrowseReturnTarget::None;
                    // Record this file in the recent-files history.
                    app.recent.record_use_with_db(&path, &app.db);
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
            app.selected_index =
                (app.selected_index + jump).min(app.items_snapshot.len().saturating_sub(1));
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

        // Toggle per-track sub-line collapse
        (KeyCode::Tab, KeyModifiers::NONE) => {
            if let Some(item) = app.items_snapshot.get(app.selected_index) {
                if !item.active_tracks.is_empty() {
                    app.manager.toggle_track_collapse(&item.id);
                }
            }
        }

        // Item info
        (KeyCode::Enter, KeyModifiers::NONE) => {
            if let Some(item) = app.items_snapshot.get(app.selected_index) {
                match &item.status {
                    ConversionStatus::Failed { error, .. } => {
                        app.active_overlay = ActiveOverlay::ErrorDetail {
                            item_id: item.id.clone(),
                            error: error.clone(),
                        };
                    }
                    _ => {
                        app.active_overlay = ActiveOverlay::ItemInfo { item: item.clone() };
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
            let finished_count = app
                .items_snapshot
                .iter()
                .filter(|i| {
                    matches!(
                        i.status,
                        ConversionStatus::Completed { .. }
                            | ConversionStatus::Partial { .. }
                            | ConversionStatus::Failed { .. }
                            | ConversionStatus::Cancelled
                    )
                })
                .count();
            if finished_count > 0 {
                app.manager.clear_finished();
                app.save_queue();
                app.set_status(format!("Cleared {} finished items", finished_count));
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
        ActiveOverlay::DiscBrowser(_) => {
            super::disc_browser_actions::handle_disc_browser_key(app, key);
        }
        ActiveOverlay::Confirmation { action, .. } => {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    app.active_overlay = ActiveOverlay::None;
                    execute_confirm_action(app, &action, tx);
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    cancel_confirm_action(app, Some(&action));
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
        ActiveOverlay::CommandInput {
            mut input,
            mut completion,
        } => {
            match key.code {
                KeyCode::Enter => {
                    app.active_overlay = ActiveOverlay::None;
                    if !input.text.trim().is_empty() {
                        let cmd = super::command::parse_command(&input.text);
                        super::command::execute_command(app, cmd, tx);
                    }
                    // If a parked metadata editor wasn't consumed by the
                    // command, restore it now. Only `take()` when we
                    // intend to restore — otherwise a colon command
                    // that parks editor + opens a new overlay (e.g.
                    // :mb-back's Confirmation flow) would have its
                    // parked state drained-and-dropped here.
                    if matches!(app.active_overlay, ActiveOverlay::None) {
                        if let Some(parked) = app.pending_metadata_editor.take() {
                            app.active_overlay = ActiveOverlay::MetadataEditor(parked);
                        }
                    }
                    // Same for parked CUE preview.
                    if matches!(app.active_overlay, ActiveOverlay::None) {
                        if let Some(parked) = app.pending_cue_preview.take() {
                            app.active_overlay = ActiveOverlay::CuePreview(parked);
                        }
                    }
                    return;
                }
                KeyCode::Esc => {
                    app.active_overlay = ActiveOverlay::None;
                    // Restore parked metadata editor on cancel.
                    if let Some(parked) = app.pending_metadata_editor.take() {
                        app.active_overlay = ActiveOverlay::MetadataEditor(parked);
                    }
                    if let Some(parked) = app.pending_cue_preview.take() {
                        app.active_overlay = ActiveOverlay::CuePreview(parked);
                    }
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
        ActiveOverlay::TextEdit {
            mut input,
            target,
            label,
        } => {
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
                        TextEditTarget::BulkRenameLine(_) | TextEditTarget::SaveRenameTemplate(_)
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
                    app.active_overlay = ActiveOverlay::TextEdit {
                        input,
                        target,
                        label,
                    };
                }
            }
        }
        ActiveOverlay::FormatSettings { mut kind, mut focus, help_scroll } => {
            if let Some(mut scroll) = help_scroll {
                // Help mode: scroll keys, Esc/Enter/? close help.
                let max_scroll = {
                    let total = super::draw_overlays::format_settings_help_line_count(&kind);
                    let (tw, th) = crossterm::terminal::size().unwrap_or((80, 30));
                    let popup = super::draw_overlays::format_settings_help_popup_rect(tw, th);
                    let visible = popup.height.saturating_sub(3) as usize; // borders(2) + footer(1)
                    total.saturating_sub(visible)
                };
                let page = 10usize;
                match key.code {
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') => {
                        app.active_overlay = ActiveOverlay::FormatSettings { kind, focus, help_scroll: None };
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        scroll = scroll.saturating_sub(1);
                        app.active_overlay = ActiveOverlay::FormatSettings { kind, focus, help_scroll: Some(scroll) };
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        scroll = (scroll + 1).min(max_scroll);
                        app.active_overlay = ActiveOverlay::FormatSettings { kind, focus, help_scroll: Some(scroll) };
                    }
                    KeyCode::PageUp => {
                        scroll = scroll.saturating_sub(page);
                        app.active_overlay = ActiveOverlay::FormatSettings { kind, focus, help_scroll: Some(scroll) };
                    }
                    KeyCode::PageDown => {
                        scroll = (scroll + page).min(max_scroll);
                        app.active_overlay = ActiveOverlay::FormatSettings { kind, focus, help_scroll: Some(scroll) };
                    }
                    KeyCode::Home | KeyCode::Char('g') => {
                        app.active_overlay = ActiveOverlay::FormatSettings { kind, focus, help_scroll: Some(0) };
                    }
                    KeyCode::End | KeyCode::Char('G') => {
                        app.active_overlay = ActiveOverlay::FormatSettings { kind, focus, help_scroll: Some(max_scroll) };
                    }
                    _ => {} // ignore other keys in help mode
                }
            } else {
                // Controls mode.
                match key.code {
                    KeyCode::Enter => {
                        commit_format_settings(app, &kind);
                        app.active_overlay = ActiveOverlay::None;
                    }
                    KeyCode::Esc => {
                        app.active_overlay = ActiveOverlay::None;
                    }
                    KeyCode::Up => {
                        focus = format_settings_focus_prev(&kind, focus);
                        app.active_overlay = ActiveOverlay::FormatSettings { kind, focus, help_scroll: None };
                    }
                    KeyCode::Down | KeyCode::Tab => {
                        focus = format_settings_focus_next(&kind, focus);
                        app.active_overlay = ActiveOverlay::FormatSettings { kind, focus, help_scroll: None };
                    }
                    KeyCode::Char('?') => {
                        app.active_overlay = ActiveOverlay::FormatSettings { kind, focus, help_scroll: Some(0) };
                    }
                    _ => {
                        handle_format_settings_field_key(&mut kind, focus, &key);
                        app.active_overlay = ActiveOverlay::FormatSettings { kind, focus, help_scroll: None };
                    }
                }
            }
        }
        ActiveOverlay::BatchList { scroll } => {
            handle_batch_list_key(app, key, scroll, tx);
        }
        ActiveOverlay::ContextMenu { levels, origin } => {
            handle_context_menu_key(app, key, levels, origin, tx);
        }
        ActiveOverlay::BulkRename(state) => {
            handle_bulk_rename_key(app, key, *state, tx);
        }
        ActiveOverlay::Analysis { mut scroll } => {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.active_overlay = ActiveOverlay::None;
                }
                // Command mode: allow :write-dr, :write-rg etc.
                KeyCode::Char(':') => {
                    app.active_overlay = ActiveOverlay::CommandInput {
                        input: super::text_input::TextInputState::empty(),
                        completion: None,
                    };
                }
                KeyCode::Up => {
                    scroll = scroll.saturating_sub(1);
                    app.active_overlay = ActiveOverlay::Analysis { scroll };
                }
                KeyCode::Down => {
                    scroll += 1;
                    app.active_overlay = ActiveOverlay::Analysis { scroll };
                }
                _ => {}
            }
        }
        ActiveOverlay::CueImportReview {
            mut scroll,
            ref changes,
        } => {
            let changes = changes.clone(); // clone for use after overlay replace
            match key.code {
                KeyCode::Enter => {
                    // Accept: apply changes to parked metadata editor.
                    if let Some(mut parked) = app.pending_metadata_editor.take() {
                        super::command::apply_cue_changes(&mut parked, &changes);
                        app.active_overlay = ActiveOverlay::MetadataEditor(parked);
                        app.set_status(format!(
                            "Imported {} change{} from CUE",
                            changes.len(),
                            if changes.len() == 1 { "" } else { "s" },
                        ));
                    } else {
                        app.active_overlay = ActiveOverlay::None;
                    }
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    // Cancel: restore editor without changes.
                    if let Some(parked) = app.pending_metadata_editor.take() {
                        app.active_overlay = ActiveOverlay::MetadataEditor(parked);
                    } else {
                        app.active_overlay = ActiveOverlay::None;
                    }
                    app.set_status("CUE import cancelled");
                }
                KeyCode::Up => {
                    scroll = scroll.saturating_sub(1);
                    app.active_overlay = ActiveOverlay::CueImportReview { changes, scroll };
                }
                KeyCode::Down => {
                    scroll += 1;
                    app.active_overlay = ActiveOverlay::CueImportReview { changes, scroll };
                }
                _ => {}
            }
        }
        ActiveOverlay::GnudbSelect {
            ref matches,
            mut selected,
            scroll,
            ref paths,
        } => {
            let matches = matches.clone();
            let paths = paths.clone();
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.active_overlay = ActiveOverlay::None;
                }
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                    app.active_overlay = ActiveOverlay::GnudbSelect {
                        matches,
                        selected,
                        scroll,
                        paths,
                    };
                }
                KeyCode::Down => {
                    if selected + 1 < matches.len() {
                        selected += 1;
                    }
                    app.active_overlay = ActiveOverlay::GnudbSelect {
                        matches,
                        selected,
                        scroll,
                        paths,
                    };
                }
                KeyCode::Enter => {
                    if let Some(m) = matches.get(selected) {
                        let category = m.category.clone();
                        let disc_id = m.disc_id.clone();
                        app.set_status(format!("Reading {}...", m.title));
                        app.active_overlay = ActiveOverlay::None;
                        let tx = tx.clone();
                        let origin = matches.clone();
                        tokio::spawn(async move {
                            let result = super::gnudb::read_gnudb(&category, &disc_id).await;
                            let _ = tx
                                .send(AppMessage::GnudbReadComplete {
                                    result,
                                    paths,
                                    origin_matches: Some(origin),
                                })
                                .await;
                        });
                    }
                }
                _ => {}
            }
        }
        ActiveOverlay::GnudbReview(mut state) => {
            use super::app::GnudbRowKind;

            let page_rows = &state.pages[state.active_page].rows;

            if state.edit_input.is_some() {
                // ── Inline edit mode ──
                match key.code {
                    KeyCode::Esc => {
                        state.edit_input = None;
                        app.active_overlay = ActiveOverlay::GnudbReview(state);
                    }
                    KeyCode::Enter => {
                        if let Some(ref input) = state.edit_input {
                            let new_val = input.text.clone();
                            let page = &mut state.pages[state.active_page];
                            match &page.rows[state.cursor] {
                                GnudbRowKind::AlbumField(field) => match *field {
                                    "Album" => page.album = new_val,
                                    "Year" => page.year = new_val,
                                    "Genre" => page.genre = new_val,
                                    _ => {}
                                },
                                GnudbRowKind::TrackField { track_idx, field } => {
                                    let track = &mut page.tracks[*track_idx];
                                    match *field {
                                        "Title" => track.title = new_val,
                                        "Artist" => track.artist = new_val,
                                        _ => {}
                                    }
                                }
                                _ => {}
                            }
                        }
                        state.edit_input = None;
                        app.active_overlay = ActiveOverlay::GnudbReview(state);
                    }
                    _ => {
                        if let Some(ref mut input) = state.edit_input {
                            super::text_input::handle_text_input_key(input, &key);
                        }
                        app.active_overlay = ActiveOverlay::GnudbReview(state);
                    }
                }
            } else {
                // ── Navigation mode ──
                match (key.code, key.modifiers) {
                    (KeyCode::Esc, _) | (KeyCode::Char('q'), _) => {
                        app.active_overlay = ActiveOverlay::None;
                        app.set_status("GNUDB review cancelled".to_string());
                    }
                    // Back to match list (if came from multi-match selection).
                    (KeyCode::Char('b'), _) => {
                        if let Some(matches) = state.origin_matches.take() {
                            let paths = state.paths;
                            app.active_overlay = ActiveOverlay::GnudbSelect {
                                matches,
                                selected: 0,
                                scroll: 0,
                                paths,
                            };
                            app.set_status("Back to match list");
                        } else {
                            app.active_overlay = ActiveOverlay::GnudbReview(state);
                        }
                    }
                    // Left / Right: switch disc page (only in navigation mode,
                    // not while editing — edit mode returns early above).
                    (KeyCode::Left, _) => {
                        if state.pages.len() > 1 && state.active_page > 0 {
                            state.active_page -= 1;
                            state.cursor = 0;
                            state.scroll = 0;
                            state.edit_input = None;
                        }
                        app.active_overlay = ActiveOverlay::GnudbReview(state);
                    }
                    (KeyCode::Right, _) => {
                        if state.active_page + 1 < state.pages.len() {
                            state.active_page += 1;
                            state.cursor = 0;
                            state.scroll = 0;
                            state.edit_input = None;
                        }
                        app.active_overlay = ActiveOverlay::GnudbReview(state);
                    }
                    (KeyCode::Up, _) => {
                        let mut nc = state.cursor;
                        loop {
                            if nc == 0 {
                                break;
                            }
                            nc -= 1;
                            if !matches!(page_rows.get(nc), Some(GnudbRowKind::TrackHeader { .. }))
                            {
                                break;
                            }
                        }
                        state.cursor = nc;
                        if state.cursor < state.scroll {
                            state.scroll = state.cursor;
                        }
                        app.active_overlay = ActiveOverlay::GnudbReview(state);
                    }
                    (KeyCode::Down, _) => {
                        let mut nc = state.cursor;
                        loop {
                            if nc + 1 >= page_rows.len() {
                                break;
                            }
                            nc += 1;
                            if !matches!(page_rows.get(nc), Some(GnudbRowKind::TrackHeader { .. }))
                            {
                                break;
                            }
                        }
                        state.cursor = nc;
                        let visible_h = crossterm::terminal::size()
                            .map(|(_, h)| (h as usize * 85 / 100).max(14).saturating_sub(4))
                            .unwrap_or(20);
                        if state.cursor >= state.scroll + visible_h {
                            state.scroll = state.cursor.saturating_sub(visible_h) + 1;
                        }
                        app.active_overlay = ActiveOverlay::GnudbReview(state);
                    }
                    (KeyCode::Enter, _) => {
                        let page = &state.pages[state.active_page];
                        let value = match &page.rows[state.cursor] {
                            GnudbRowKind::AlbumField(field) => match *field {
                                "Album" => Some(page.album.clone()),
                                "Year" => Some(page.year.clone()),
                                "Genre" => Some(page.genre.clone()),
                                _ => None,
                            },
                            GnudbRowKind::TrackField { track_idx, field } => {
                                let track = &page.tracks[*track_idx];
                                match *field {
                                    "Title" => Some(track.title.clone()),
                                    "Artist" => Some(track.artist.clone()),
                                    _ => None,
                                }
                            }
                            _ => None,
                        };
                        if let Some(val) = value {
                            state.edit_input = Some(super::text_input::TextInputState::new(val));
                        }
                        app.active_overlay = ActiveOverlay::GnudbReview(state);
                    }
                    (KeyCode::PageUp, _) => {
                        let jump = crossterm::terminal::size()
                            .map(|(_, h)| (h as usize * 85 / 100).max(14).saturating_sub(4))
                            .unwrap_or(20);
                        for _ in 0..jump {
                            let mut nc = state.cursor;
                            if nc == 0 {
                                break;
                            }
                            nc -= 1;
                            while nc > 0
                                && matches!(
                                    page_rows.get(nc),
                                    Some(GnudbRowKind::TrackHeader { .. })
                                )
                            {
                                nc -= 1;
                            }
                            state.cursor = nc;
                        }
                        if state.cursor < state.scroll {
                            state.scroll = state.cursor;
                        }
                        app.active_overlay = ActiveOverlay::GnudbReview(state);
                    }
                    (KeyCode::PageDown, _) => {
                        let jump = crossterm::terminal::size()
                            .map(|(_, h)| (h as usize * 85 / 100).max(14).saturating_sub(4))
                            .unwrap_or(20);
                        for _ in 0..jump {
                            let mut nc = state.cursor;
                            if nc + 1 >= page_rows.len() {
                                break;
                            }
                            nc += 1;
                            while nc + 1 < page_rows.len()
                                && matches!(
                                    page_rows.get(nc),
                                    Some(GnudbRowKind::TrackHeader { .. })
                                )
                            {
                                nc += 1;
                            }
                            state.cursor = nc;
                        }
                        let visible_h = crossterm::terminal::size()
                            .map(|(_, h)| (h as usize * 85 / 100).max(14).saturating_sub(4))
                            .unwrap_or(20);
                        if state.cursor >= state.scroll + visible_h {
                            state.scroll = state.cursor.saturating_sub(visible_h) + 1;
                        }
                        app.active_overlay = ActiveOverlay::GnudbReview(state);
                    }
                    (KeyCode::Char('a'), _) => {
                        // Accept ALL pages.
                        super::keybindings::open_metadata_editor(app);
                        if let ActiveOverlay::MetadataEditor(ref mut editor_state) =
                            app.active_overlay
                        {
                            super::gnudb::populate_editor_from_review(editor_state, &state);
                            // Cache the review state for `:gnudb-back`
                            // — user can return to the per-track edit
                            // surface preserving their edits, no
                            // gnudb requery.
                            editor_state.gnudb_back = Some(state.clone());
                            let first_page = &state.pages[0];
                            let artist = first_page
                                .tracks
                                .first()
                                .map(|t| t.artist.as_str())
                                .unwrap_or("?");
                            app.set_status(format!(
                                "Tags loaded — {} / {} ({} disc{})",
                                artist,
                                first_page.album,
                                state.pages.len(),
                                if state.pages.len() == 1 { "" } else { "s" },
                            ));
                        }
                    }
                    (KeyCode::Char('c'), _) => {
                        // Apply capitalization to current page.
                        use crate::convert::renaming::{capitalize_section, capitalize_title};
                        let page = &mut state.pages[state.active_page];
                        page.album = capitalize_section(&page.album);
                        page.genre = capitalize_section(&page.genre);
                        for track in &mut page.tracks {
                            track.title = capitalize_title(&track.title);
                            track.artist = capitalize_section(&track.artist);
                        }
                        app.set_status("Capitalization applied to current disc");
                        app.active_overlay = ActiveOverlay::GnudbReview(state);
                    }
                    _ => {}
                }
            }
        }
        ActiveOverlay::Verify { mut scroll } => match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                app.active_overlay = ActiveOverlay::None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                scroll = scroll.saturating_sub(1);
                app.active_overlay = ActiveOverlay::Verify { scroll };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                scroll += 1;
                app.active_overlay = ActiveOverlay::Verify { scroll };
            }
            _ => {}
        },
        ActiveOverlay::MbSelect(mut state) => {
            // Picker has only two actions: accept the cursor's release
            // (Enter) or cancel (Esc). Both are navigation primitives,
            // so no command-mode parking is needed.
            let n = state.releases.len();
            match key.code {
                KeyCode::Esc => {
                    app.active_overlay = ActiveOverlay::None;
                    super::event_loop::restore_parked_editor(app);
                    app.set_status("MusicBrainz picker cancelled".to_string());
                }
                KeyCode::Enter => {
                    let idx = state.selected;
                    if idx < state.releases.len() {
                        let releases = std::mem::take(&mut state.releases);
                        let paths = std::mem::take(&mut state.paths);
                        super::event_loop::open_editor_with_mb_release(app, releases, idx, paths);
                    }
                }
                KeyCode::Up => {
                    state.selected = state.selected.saturating_sub(1);
                    prefetch_current_mb_row(tx, &state, &app.db);
                    app.active_overlay = ActiveOverlay::MbSelect(state);
                }
                KeyCode::Down => {
                    state.selected = (state.selected + 1).min(n.saturating_sub(1));
                    prefetch_current_mb_row(tx, &state, &app.db);
                    app.active_overlay = ActiveOverlay::MbSelect(state);
                }
                KeyCode::PageUp => {
                    state.selected = state.selected.saturating_sub(10);
                    prefetch_current_mb_row(tx, &state, &app.db);
                    app.active_overlay = ActiveOverlay::MbSelect(state);
                }
                KeyCode::PageDown => {
                    state.selected = (state.selected + 10).min(n.saturating_sub(1));
                    prefetch_current_mb_row(tx, &state, &app.db);
                    app.active_overlay = ActiveOverlay::MbSelect(state);
                }
                _ => {
                    app.active_overlay = ActiveOverlay::MbSelect(state);
                }
            }
        }
        ActiveOverlay::CuePreview(mut state) => {
            // Two modes:
            // - Read-only (default): `:` parks the overlay + opens command
            //   input; arrows / PageUp / PageDown scroll; Esc cancels.
            // - Editing (after `:e <N>`): keys flow through TextInputState;
            //   Enter commits the line splice; Esc cancels the edit
            //   without splicing.
            if state.is_editing() {
                match key.code {
                    KeyCode::Enter => {
                        state.commit_edit();
                        app.active_overlay = ActiveOverlay::CuePreview(state);
                    }
                    KeyCode::Esc => {
                        state.cancel_edit();
                        app.active_overlay = ActiveOverlay::CuePreview(state);
                    }
                    _ => {
                        if let Some(ref mut input) = state.edit {
                            super::text_input::handle_text_input_key(input, &key);
                        }
                        app.active_overlay = ActiveOverlay::CuePreview(state);
                    }
                }
                return;
            }

            let max_line = state.content.lines().count().saturating_sub(1);
            match key.code {
                KeyCode::Char(':') => {
                    if state.read_only {
                        // Block command-mode parking in read-only.
                        // Only Esc closes; nothing else applies to a
                        // view-only overlay.
                        app.active_overlay = ActiveOverlay::CuePreview(state);
                    } else {
                        app.pending_cue_preview = Some(state);
                        app.active_overlay = ActiveOverlay::CommandInput {
                            input: super::text_input::TextInputState::empty(),
                            completion: None,
                        };
                    }
                }
                KeyCode::Esc => {
                    close_cue_preview_restoring_parked(app);
                    app.set_status("CUE preview cancelled".to_string());
                }
                KeyCode::Up => {
                    state.scroll = state.scroll.saturating_sub(1);
                    app.active_overlay = ActiveOverlay::CuePreview(state);
                }
                KeyCode::Down => {
                    state.scroll = state.scroll.saturating_add(1).min(max_line);
                    app.active_overlay = ActiveOverlay::CuePreview(state);
                }
                KeyCode::PageUp => {
                    state.scroll = state.scroll.saturating_sub(10);
                    app.active_overlay = ActiveOverlay::CuePreview(state);
                }
                KeyCode::PageDown => {
                    state.scroll = state.scroll.saturating_add(10).min(max_line);
                    app.active_overlay = ActiveOverlay::CuePreview(state);
                }
                _ => {
                    app.active_overlay = ActiveOverlay::CuePreview(state);
                }
            }
        }
        ActiveOverlay::BitCompare { mut scroll } => match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                app.active_overlay = ActiveOverlay::None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                scroll = scroll.saturating_sub(1);
                app.active_overlay = ActiveOverlay::BitCompare { scroll };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                scroll += 1;
                app.active_overlay = ActiveOverlay::BitCompare { scroll };
            }
            _ => {}
        },
        ActiveOverlay::Preemphasis { mut scroll } => match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                app.active_overlay = ActiveOverlay::None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                scroll = scroll.saturating_sub(1);
                app.active_overlay = ActiveOverlay::Preemphasis { scroll };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                scroll += 1;
                app.active_overlay = ActiveOverlay::Preemphasis { scroll };
            }
            _ => {}
        },
        ActiveOverlay::MetadataEditor(mut state) => {
            handle_metadata_editor_key(app, key, &mut state, tx);
            // If the handler didn't close or replace the overlay, put state back.
            if matches!(app.active_overlay, ActiveOverlay::MetadataEditor(_)) {
                app.active_overlay = ActiveOverlay::MetadataEditor(state);
            }
        }
        ActiveOverlay::Help { mut scroll, screen } => {
            // Compute content length for scroll clamping.
            let max_scroll = {
                let sections = super::help::help_content_for(screen);
                let total = super::help::line_count(&sections);
                // Estimate visible rows from terminal height (same math as renderer).
                let term_h = crossterm::terminal::size().map(|(_, h)| h).unwrap_or(30) as usize;
                let popup_h = (term_h * 85 / 100).max(15).min(term_h.saturating_sub(2));
                let visible = popup_h.saturating_sub(4); // borders(2) + footer(1) + margin(1)
                total.saturating_sub(visible)
            };
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                    app.active_overlay = ActiveOverlay::None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    scroll = scroll.saturating_sub(1);
                    app.active_overlay = ActiveOverlay::Help { screen, scroll };
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    scroll = (scroll + 1).min(max_scroll);
                    app.active_overlay = ActiveOverlay::Help { screen, scroll };
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    app.active_overlay = ActiveOverlay::Help { screen, scroll: 0 };
                }
                KeyCode::End | KeyCode::Char('G') => {
                    app.active_overlay = ActiveOverlay::Help {
                        screen,
                        scroll: max_scroll,
                    };
                }
                KeyCode::PageUp => {
                    scroll = scroll.saturating_sub(15);
                    app.active_overlay = ActiveOverlay::Help { screen, scroll };
                }
                KeyCode::PageDown => {
                    scroll = (scroll + 15).min(max_scroll);
                    app.active_overlay = ActiveOverlay::Help { screen, scroll };
                }
                _ => {}
            }
        }
        ActiveOverlay::CtdbVerify(mut state) => {
            let page = &state.pages[state.active_page];
            let disc_header = if state.pages.len() > 1 { 2 } else { 0 };
            let total_lines = disc_header + 2 + page.result.tracks.len() * 2;
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.active_overlay = ActiveOverlay::None;
                }
                KeyCode::Left => {
                    if state.pages.len() > 1 && state.active_page > 0 {
                        state.active_page -= 1;
                        state.scroll = 0;
                    }
                    app.active_overlay = ActiveOverlay::CtdbVerify(state);
                }
                KeyCode::Right => {
                    if state.active_page + 1 < state.pages.len() {
                        state.active_page += 1;
                        state.scroll = 0;
                    }
                    app.active_overlay = ActiveOverlay::CtdbVerify(state);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    state.scroll = state.scroll.saturating_sub(1);
                    app.active_overlay = ActiveOverlay::CtdbVerify(state);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    state.scroll = (state.scroll + 1).min(total_lines.saturating_sub(1));
                    app.active_overlay = ActiveOverlay::CtdbVerify(state);
                }
                KeyCode::PageUp => {
                    state.scroll = state.scroll.saturating_sub(15);
                    app.active_overlay = ActiveOverlay::CtdbVerify(state);
                }
                KeyCode::PageDown => {
                    state.scroll = (state.scroll + 15).min(total_lines.saturating_sub(1));
                    app.active_overlay = ActiveOverlay::CtdbVerify(state);
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    state.scroll = 0;
                    app.active_overlay = ActiveOverlay::CtdbVerify(state);
                }
                KeyCode::End | KeyCode::Char('G') => {
                    state.scroll = total_lines.saturating_sub(1);
                    app.active_overlay = ActiveOverlay::CtdbVerify(state);
                }
                _ => {
                    app.active_overlay = ActiveOverlay::CtdbVerify(state);
                }
            }
        }
        ActiveOverlay::ArBatchReport { result, mut scroll } => {
            let total_lines = 3 + result.albums.len() * 2;
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.active_overlay = ActiveOverlay::None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    scroll = scroll.saturating_sub(1);
                    app.active_overlay = ActiveOverlay::ArBatchReport { result, scroll };
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    scroll = (scroll + 1).min(total_lines.saturating_sub(1));
                    app.active_overlay = ActiveOverlay::ArBatchReport { result, scroll };
                }
                KeyCode::PageUp => {
                    scroll = scroll.saturating_sub(15);
                    app.active_overlay = ActiveOverlay::ArBatchReport { result, scroll };
                }
                KeyCode::PageDown => {
                    scroll = (scroll + 15).min(total_lines.saturating_sub(1));
                    app.active_overlay = ActiveOverlay::ArBatchReport { result, scroll };
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    scroll = 0;
                    app.active_overlay = ActiveOverlay::ArBatchReport { result, scroll };
                }
                KeyCode::End | KeyCode::Char('G') => {
                    scroll = total_lines.saturating_sub(1);
                    app.active_overlay = ActiveOverlay::ArBatchReport { result, scroll };
                }
                _ => {
                    app.active_overlay = ActiveOverlay::ArBatchReport { result, scroll };
                }
            }
        }
        ActiveOverlay::AccurateRipVerify(mut state) => {
            let page = &state.pages[state.active_page];
            // Rendered lines: disc pills (2 if multi) + 3 (summary + ID + blank) + 2 per track.
            let disc_header = if state.pages.len() > 1 { 2 } else { 0 };
            let total_lines = disc_header + 3 + page.result.tracks.len() * 2;
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    app.active_overlay = ActiveOverlay::None;
                }
                KeyCode::Left => {
                    if state.pages.len() > 1 && state.active_page > 0 {
                        state.active_page -= 1;
                        state.scroll = 0;
                    }
                    app.active_overlay = ActiveOverlay::AccurateRipVerify(state);
                }
                KeyCode::Right => {
                    if state.active_page + 1 < state.pages.len() {
                        state.active_page += 1;
                        state.scroll = 0;
                    }
                    app.active_overlay = ActiveOverlay::AccurateRipVerify(state);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    state.scroll = state.scroll.saturating_sub(1);
                    app.active_overlay = ActiveOverlay::AccurateRipVerify(state);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    state.scroll = (state.scroll + 1).min(total_lines.saturating_sub(1));
                    app.active_overlay = ActiveOverlay::AccurateRipVerify(state);
                }
                KeyCode::PageUp => {
                    state.scroll = state.scroll.saturating_sub(15);
                    app.active_overlay = ActiveOverlay::AccurateRipVerify(state);
                }
                KeyCode::PageDown => {
                    state.scroll = (state.scroll + 15).min(total_lines.saturating_sub(1));
                    app.active_overlay = ActiveOverlay::AccurateRipVerify(state);
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    state.scroll = 0;
                    app.active_overlay = ActiveOverlay::AccurateRipVerify(state);
                }
                KeyCode::End | KeyCode::Char('G') => {
                    state.scroll = total_lines.saturating_sub(1);
                    app.active_overlay = ActiveOverlay::AccurateRipVerify(state);
                }
                _ => {
                    app.active_overlay = ActiveOverlay::AccurateRipVerify(state);
                }
            }
        }
        ActiveOverlay::TemplateBuilder(mut state) => {
            match state.focus {
                TemplateBuilderFocus::TemplateInput => match key.code {
                    KeyCode::Esc => {
                        app.active_overlay = ActiveOverlay::None;
                    }
                    KeyCode::Tab => {
                        state.focus = TemplateBuilderFocus::SavedList;
                        app.active_overlay = ActiveOverlay::TemplateBuilder(state);
                    }
                    KeyCode::Enter => {
                        // Apply: write template to the target field
                        let text = state.template_input.text.clone();
                        match state.target {
                            TemplateTarget::Folder => {
                                app.convert.output_options.folder_template = text;
                            }
                            TemplateTarget::Filename => {
                                app.convert.output_options.filename_template = text;
                            }
                        }
                        app.preset.mark_modified();
                        app.active_overlay = ActiveOverlay::None;
                        app.set_status("Template applied");
                    }
                    _ => {
                        super::text_input::handle_text_input_key(&mut state.template_input, &key);
                        app.active_overlay = ActiveOverlay::TemplateBuilder(state);
                    }
                },
                TemplateBuilderFocus::SavedList => match key.code {
                    KeyCode::Esc => {
                        app.active_overlay = ActiveOverlay::None;
                    }
                    KeyCode::Tab => {
                        state.focus = TemplateBuilderFocus::TokenGrid;
                        app.active_overlay = ActiveOverlay::TemplateBuilder(state);
                    }
                    KeyCode::BackTab => {
                        state.focus = TemplateBuilderFocus::TemplateInput;
                        app.active_overlay = ActiveOverlay::TemplateBuilder(state);
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if state.saved_selected > 0 {
                            state.saved_selected -= 1;
                        }
                        app.active_overlay = ActiveOverlay::TemplateBuilder(state);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if state.saved_selected + 1 < state.saved_templates.len() {
                            state.saved_selected += 1;
                        }
                        app.active_overlay = ActiveOverlay::TemplateBuilder(state);
                    }
                    KeyCode::Enter => {
                        // Load selected template into the input line
                        if let Some(tmpl) = state.saved_templates.get(state.saved_selected).cloned()
                        {
                            state.template_input = super::text_input::TextInputState::new(tmpl);
                            state.template_input.cursor = state.template_input.text.len();
                            state.focus = TemplateBuilderFocus::TemplateInput;
                        }
                        app.active_overlay = ActiveOverlay::TemplateBuilder(state);
                    }
                    KeyCode::Char('x') | KeyCode::Delete => {
                        // Delete selected saved template
                        if let Some(tmpl) = state.saved_templates.get(state.saved_selected).cloned()
                        {
                            let _ = super::template_builder::delete_template(state.target, &tmpl);
                            state.saved_templates =
                                super::template_builder::list_templates(state.target);
                            if state.saved_selected >= state.saved_templates.len()
                                && state.saved_selected > 0
                            {
                                state.saved_selected -= 1;
                            }
                            app.set_status("Template deleted");
                        }
                        app.active_overlay = ActiveOverlay::TemplateBuilder(state);
                    }
                    _ => {
                        app.active_overlay = ActiveOverlay::TemplateBuilder(state);
                    }
                },
                TemplateBuilderFocus::TokenGrid => match key.code {
                    KeyCode::Esc => {
                        app.active_overlay = ActiveOverlay::None;
                    }
                    KeyCode::Tab => {
                        state.focus = TemplateBuilderFocus::TemplateInput;
                        app.active_overlay = ActiveOverlay::TemplateBuilder(state);
                    }
                    KeyCode::BackTab => {
                        state.focus = TemplateBuilderFocus::SavedList;
                        app.active_overlay = ActiveOverlay::TemplateBuilder(state);
                    }
                    KeyCode::Left => {
                        if state.grid_cursor > 0 {
                            state.grid_cursor -= 1;
                        }
                        app.active_overlay = ActiveOverlay::TemplateBuilder(state);
                    }
                    KeyCode::Right => {
                        let max = state.total_grid_items().saturating_sub(1);
                        if state.grid_cursor < max {
                            state.grid_cursor += 1;
                        }
                        app.active_overlay = ActiveOverlay::TemplateBuilder(state);
                    }
                    KeyCode::Enter => {
                        state.insert_current_grid_item();
                        app.active_overlay = ActiveOverlay::TemplateBuilder(state);
                    }
                    _ => {
                        app.active_overlay = ActiveOverlay::TemplateBuilder(state);
                    }
                },
            }
        }
        ActiveOverlay::TemplatePicker {
            target,
            templates,
            mut selected,
            mut scroll,
            active_template,
            ..
        } => {
            let count = templates.len();
            match key.code {
                KeyCode::Esc => {
                    app.active_overlay = ActiveOverlay::None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if selected > 0 {
                        selected -= 1;
                        if selected < scroll {
                            scroll = selected;
                        }
                    }
                    let preview = if let Some(tmpl) = templates.get(selected) {
                        super::template_builder::render_template_preview(tmpl)
                    } else {
                        String::new()
                    };
                    app.active_overlay = ActiveOverlay::TemplatePicker {
                        target,
                        templates,
                        selected,
                        scroll,
                        preview,
                        active_template,
                    };
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if count > 0 && selected + 1 < count {
                        selected += 1;
                        // Scroll follows selection: assume ~10 visible rows
                        let visible = 10_usize;
                        if selected >= scroll + visible {
                            scroll = selected.saturating_sub(visible - 1);
                        }
                    }
                    let preview = if let Some(tmpl) = templates.get(selected) {
                        super::template_builder::render_template_preview(tmpl)
                    } else {
                        String::new()
                    };
                    app.active_overlay = ActiveOverlay::TemplatePicker {
                        target,
                        templates,
                        selected,
                        scroll,
                        preview,
                        active_template,
                    };
                }
                KeyCode::Enter => {
                    if let Some(tmpl) = templates.get(selected).cloned() {
                        match target {
                            TemplateTarget::Folder => {
                                app.convert.output_options.folder_template = tmpl;
                            }
                            TemplateTarget::Filename => {
                                app.convert.output_options.filename_template = tmpl;
                            }
                        }
                        app.preset.mark_modified();
                        app.active_overlay = ActiveOverlay::None;
                        app.set_status("Template applied");
                    }
                }
                KeyCode::Delete => {
                    if let Some(tmpl) = templates.get(selected).cloned() {
                        let _ =
                            super::template_builder::delete_template(target, &tmpl);
                        let new_templates =
                            super::template_builder::list_templates(target);
                        if selected >= new_templates.len() && selected > 0 {
                            selected -= 1;
                        }
                        if scroll > 0 && scroll >= new_templates.len() {
                            scroll = new_templates.len().saturating_sub(1);
                        }
                        let preview = if let Some(tmpl) = new_templates.get(selected) {
                            super::template_builder::render_template_preview(tmpl)
                        } else {
                            String::new()
                        };
                        app.set_status("Template deleted");
                        app.active_overlay = ActiveOverlay::TemplatePicker {
                            target,
                            templates: new_templates,
                            selected,
                            scroll,
                            preview,
                            active_template,
                        };
                    }
                }
                _ => {
                    app.active_overlay = ActiveOverlay::TemplatePicker {
                        target,
                        templates,
                        selected,
                        scroll,
                        preview: String::new(),
                        active_template,
                    };
                }
            }
        }
        ActiveOverlay::None => {}
    }
}

/// Bump the MbSelect picker's prefetch generation and spawn a detail
/// fetch for the currently-highlighted row — unless the cache already
/// has it, in which case nothing fires (no wasted MB token).
///
/// Called after every cursor move (keyboard Up/Down/PgUp/PgDn and
/// future mouse-row click). The spawn helper handles the 150 ms
/// debounce and the generation-mismatch bail-out; this caller just
/// supplies the new snapshot.
fn prefetch_current_mb_row(
    tx: &mpsc::Sender<AppMessage>,
    state: &crate::tui::app::MbSelectState,
    db: &crate::db::Database,
) {
    // Always bump generation on navigation so any in-flight prefetch
    // for a no-longer-current row sees the mismatch and exits. The
    // bump is cheap; the alternative (skip bump when destination is
    // already cached) has subtle interactions when rapidly traversing
    // a mix of cached and uncached rows.
    let snapshot = state.bump_generation();
    let Some(row) = state.releases.get(state.selected) else {
        return;
    };
    if row.release_id.is_empty() || state.prefetch.contains_key(&row.release_id) {
        return;
    }
    let cached_body =
        db.get_cached_mb_search(&super::musicbrainz::detail_cache_key(&row.release_id));
    super::event_loop::spawn_mb_detail_prefetch(
        tx.clone(),
        row.release_id.clone(),
        state.paths.len(),
        std::sync::Arc::clone(&state.generation),
        snapshot,
        cached_body,
    );
}

/// Close the active context-menu overlay and restore any parked
/// stateful overlay (metadata editor / cue preview / mb select). When
/// no overlay was parked, leaves `active_overlay = None`.
///
/// Called from every ContextMenu close path (Esc / Left / outside-click /
/// post-action) so right-click → context menu → Esc round-trips back to
/// the originating overlay. Mirrors the `pending_*.take()` pattern used
/// by the CommandInput Enter/Esc handlers.
fn close_context_menu_restoring_parked(app: &mut AppState) {
    // Restoration order is innermost-parked-first: when SACD
    // `:tags-mb` parks the editor and a subsequent right-click on
    // MbSelect parks the picker, both slots are `Some` at once.
    // The user's mental nesting is editor → MbSelect → ContextMenu,
    // so Esc on the context menu must return to MbSelect, not all
    // the way back to the editor. cue_preview slots between the
    // two for the same reason.
    app.active_overlay = ActiveOverlay::None;
    if let Some(parked) = app.pending_mb_select.take() {
        app.active_overlay = ActiveOverlay::MbSelect(parked);
        return;
    }
    if let Some(parked) = app.pending_cue_preview.take() {
        app.active_overlay = ActiveOverlay::CuePreview(parked);
        return;
    }
    if let Some(parked) = app.pending_metadata_editor.take() {
        app.active_overlay = ActiveOverlay::MetadataEditor(parked);
    }
}

/// Close the CuePreview overlay, restoring the metadata editor if it
/// was parked (the `[view]` pill from a synthetic-preview row sets
/// `pending_metadata_editor` before opening CuePreview in read-only
/// mode). No-op for the other CuePreview entry paths (`:cue-mb`,
/// `:cue-fill`) since they don't park anything.
fn close_cue_preview_restoring_parked(app: &mut AppState) {
    app.active_overlay = ActiveOverlay::None;
    if let Some(parked) = app.pending_metadata_editor.take() {
        app.active_overlay = ActiveOverlay::MetadataEditor(parked);
    }
}

/// Run a context-menu action, then restore any parked overlay if the
/// action didn't set its own overlay. Wraps the common pattern:
/// "close menu, run action, if action didn't take a parked state and
/// didn't open something else, put the parked state back."
fn run_context_action_restoring_parked(
    app: &mut AppState,
    action: super::context_menu::ContextAction,
    tx: &mpsc::Sender<AppMessage>,
    invert: bool,
) {
    app.active_overlay = ActiveOverlay::None;
    super::context_menu::execute_context_action(app, action, tx, invert);
    if matches!(app.active_overlay, ActiveOverlay::None) {
        // Same innermost-first restoration order as
        // `close_context_menu_restoring_parked`. See that function's
        // comment for why the SACD `:tags-mb` flow demands it.
        if let Some(parked) = app.pending_mb_select.take() {
            app.active_overlay = ActiveOverlay::MbSelect(parked);
            return;
        }
        if let Some(parked) = app.pending_cue_preview.take() {
            app.active_overlay = ActiveOverlay::CuePreview(parked);
            return;
        }
        if let Some(parked) = app.pending_metadata_editor.take() {
            app.active_overlay = ActiveOverlay::MetadataEditor(parked);
        }
    } else {
        // Action set its own overlay → drop any parked state (action
        // consumed it or transitioned to a new flow).
        app.pending_metadata_editor = None;
        app.pending_cue_preview = None;
        app.pending_mb_select = None;
    }
}

/// Keyboard handler for the context menu.
///
/// Model: `levels` is the explicitly navigated stack; `levels.last()` is
/// the focused panel. The renderer additionally previews children of
/// the focused panel's selected entry as a "phantom" deeper panel when
/// that entry is a Submenu — that phantom is *not* in `levels`.
///
/// - Up/Down: move selection in `levels.last()`
/// - Right / Enter on Submenu: push children as new focused level (cap MAX)
/// - Enter on Item: execute action, close menu
/// - Left: pop focused level; close menu at root
/// - Esc: close menu entirely
fn handle_context_menu_key(
    app: &mut AppState,
    key: KeyEvent,
    mut levels: Vec<super::context_menu::MenuLevel>,
    origin: (u16, u16),
    tx: &mpsc::Sender<AppMessage>,
) {
    use super::context_menu::{ContextMenuEntry, MenuLevel, MAX_CONTEXT_MENU_DEPTH};

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

    if levels.is_empty() {
        close_context_menu_restoring_parked(app);
        return;
    }

    match key.code {
        KeyCode::Esc => {
            close_context_menu_restoring_parked(app);
            return;
        }
        KeyCode::Left => {
            if levels.len() > 1 {
                levels.pop();
            } else {
                close_context_menu_restoring_parked(app);
                return;
            }
        }
        KeyCode::Right => {
            let depth = levels.len();
            let cur = &levels[depth - 1];
            let sel = selectable_indices(&cur.entries);
            if let Some(&idx) = sel.get(cur.selected) {
                if let ContextMenuEntry::Submenu { children, .. } = &cur.entries[idx] {
                    if depth < MAX_CONTEXT_MENU_DEPTH {
                        levels.push(MenuLevel::new(children.clone()));
                    }
                }
            }
        }
        KeyCode::Enter | KeyCode::Char('q') => {
            let invert = key.code == KeyCode::Char('q');
            let depth = levels.len();
            let cur = &levels[depth - 1];
            let sel = selectable_indices(&cur.entries);
            if let Some(&idx) = sel.get(cur.selected) {
                match &cur.entries[idx] {
                    ContextMenuEntry::Item(item) => {
                        let action = item.action.clone();
                        run_context_action_restoring_parked(app, action, tx, invert);
                        return;
                    }
                    ContextMenuEntry::Submenu { children, .. } => {
                        if depth < MAX_CONTEXT_MENU_DEPTH {
                            levels.push(MenuLevel::new(children.clone()));
                        }
                    }
                    _ => {}
                }
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            let cur = levels.last_mut().unwrap();
            if cur.selected > 0 {
                cur.selected -= 1;
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let cur = levels.last_mut().unwrap();
            let n = selectable_indices(&cur.entries).len();
            if cur.selected + 1 < n {
                cur.selected += 1;
            }
        }
        _ => {
            app.active_overlay = ActiveOverlay::None;
            return;
        }
    }

    app.active_overlay = ActiveOverlay::ContextMenu { levels, origin };
}

/// Build and open the context menu for the current screen. `x, y` is
/// the screen position where the menu should be anchored (right-click
/// position, or a computed position for keyboard `m`).
pub fn open_context_menu(app: &mut AppState, x: u16, y: u16) {
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

    let mut levels = vec![super::context_menu::MenuLevel::new(entries)];
    // If the root's first entry is a Submenu, auto-push it so users
    // see the cascade preview without first arrowing down.
    if let Some(super::context_menu::ContextMenuEntry::Submenu { children, .. }) =
        levels[0].entries.first()
    {
        if levels.len() < super::context_menu::MAX_CONTEXT_MENU_DEPTH {
            levels.push(super::context_menu::MenuLevel::new(children.clone()));
        }
    }

    app.active_overlay = ActiveOverlay::ContextMenu {
        levels,
        origin: (x, y),
    };
}

/// Handle key events inside the metadata editor overlay.
/// Open the "add new field" prompt (sets cursor to the add row,
/// initializes add_key_input, transitions to AddingKey phase).
pub(super) fn metadata_editor_open_add(state: &mut super::app::MetadataEditorState) {
    state.cursor = state.entries.len();
    state.add_key_input = Some(super::text_input::TextInputState::empty());
    state.phase = super::app::MetadataEditorPhase::AddingKey;
    ensure_cursor_visible(state);
}

/// Mark the cursor row for deletion (renders strikethrough until save).
pub(super) fn metadata_editor_delete_cursor(state: &mut super::app::MetadataEditorState) {
    if state.cursor < state.entries.len() && !state.deleted.contains(&state.cursor) {
        state.deleted.push(state.cursor);
        recalc_dirty(state);
    }
}

/// Un-delete the cursor row (removes it from the deleted set).
pub(super) fn metadata_editor_undelete_cursor(state: &mut super::app::MetadataEditorState) {
    state.deleted.retain(|&i| i != state.cursor);
    recalc_dirty(state);
}

/// Force-open the per-file detail overlay on the cursor entry. Gated
/// on per_file_values.len() > 1 (so per-track entries on single-image
/// rips qualify even when paths.len() == 1).
pub(super) fn metadata_editor_open_detail(state: &mut super::app::MetadataEditorState) {
    if state.cursor < state.entries.len()
        && state.entries[state.cursor].per_file_values.len() > 1
        && !state.entries[state.cursor].is_binary
        && !state.deleted.contains(&state.cursor)
    {
        state.detail_field_idx = state.cursor;
        state.detail_cursor = 0;
        state.detail_scroll = 0;
        state.detail_edit = None;
        state.last_click = None;
        state.phase = super::app::MetadataEditorPhase::DetailEdit;
    }
}

/// Save tags to disk. Runs Phase 4's CUESHEET regen (β album re-derive
/// + per-track-edit overrides) before snapshotting; refuses save on a
/// dirty per-track entry without a CUESHEET anchor. Skips per-track
/// entries from lofty's per-file write loop (their truth is the
/// regenerated CUESHEET tag).
pub(super) fn metadata_editor_save(
    app: &mut AppState,
    state: &mut super::app::MetadataEditorState,
    tx: &mpsc::Sender<AppMessage>,
) {
    if state.read_only {
        if state.sacd_sidecar_path.is_some() && state.sacd_area_kind.is_none() {
            app.set_status("read-only editor — cannot save (DVD-Audio metabase)");
        } else {
            app.set_status("read-only editor — cannot save (SACD ISO)");
        }
        return;
    }
    state.sync_active_presentation();
    if !state.any_presentation_dirty() {
        app.set_status("No changes to save");
        return;
    }
    // Disc-image save: route XML-backed editors away from the lofty / per-file path.
    if let Some(sidecar_path) = state.sacd_sidecar_path.clone() {
        if state.sacd_area_kind.is_none() {
            match save_dvda_metabase(state, &sidecar_path) {
                Ok(kind) => {
                    state.mark_all_presentations_saved();
                    let verb = match kind {
                        SacdSaveKind::Created => "created",
                        SacdSaveKind::Updated => "updated",
                    };
                    let file_name = sidecar_path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| sidecar_path.display().to_string());
                    app.set_status(format!("DVD-Audio metabase {}: {}", verb, file_name));
                }
                Err(reason) => {
                    app.set_status(format!("DVD-Audio metabase save failed: {}", reason));
                }
            }
            return;
        }

        match save_sacd_sidecar(state, &sidecar_path) {
            Ok(outcome) => {
                state.mark_all_presentations_saved();
                let verb = match outcome.kind {
                    SacdSaveKind::Created => "created",
                    SacdSaveKind::Updated => "updated",
                };
                let file_name = sidecar_path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| sidecar_path.display().to_string());
                let area_note = if state.presentation_tabs.len() > 1 {
                    format!("{} areas", state.presentation_tabs.len())
                } else {
                    let mirror = outcome.mirror;
                    let surfaced = match state.sacd_area_kind {
                        Some(super::sacd::AreaKind::Stereo) => "stereo",
                        Some(super::sacd::AreaKind::MultiChannel) => "MCH",
                        None => "?",
                    };
                    let sibling = match state.sacd_area_kind {
                        Some(super::sacd::AreaKind::Stereo) => "MCH",
                        Some(super::sacd::AreaKind::MultiChannel) => "stereo",
                        None => "?",
                    };
                    if !mirror.sibling_present {
                        format!("{} area only", surfaced)
                    } else if mirror.mirrored_count == mirror.sibling_total {
                        format!("{} + {} areas", surfaced, sibling)
                    } else {
                        format!(
                            "{} + {}/{} {} tracks (count differs)",
                            surfaced, mirror.mirrored_count, mirror.sibling_total, sibling,
                        )
                    }
                };
                app.set_status(format!(
                    "SACD sidecar {} ({}): {}",
                    verb, area_note, file_name,
                ));
            }
            Err(reason) => {
                app.set_status(format!("SACD sidecar save failed: {}", reason));
            }
        }
        return;
    }
    if let Err(reason) = regenerate_cuesheet_for_save(state) {
        app.set_status(reason);
        return;
    }
    state.phase = super::app::MetadataEditorPhase::Saving;
    let paths = state.paths.clone();
    let deleted = state.deleted.clone();
    let entries_snap: Vec<(lofty::tag::ItemKey, Vec<String>, Vec<String>)> = state
        .entries
        .iter()
        .map(|e| {
            (
                e.item_key.clone(),
                e.per_file_values.clone(),
                e.per_file_originals.clone(),
            )
        })
        .collect();

    let tx = tx.clone();
    tokio::spawn(async move {
        let results = tokio::task::spawn_blocking(move || {
            crate::tui::probe::apply_audio_tag_changes(&paths, &entries_snap, &deleted)
        })
        .await
        .unwrap_or_else(|e| {
            // Whole-batch task panic is unusual (write_all_tags errors
            // are returned, not panicked). If it does happen, surface
            // a single batch-level error rather than losing the
            // request silently.
            vec![(
                std::path::PathBuf::new(),
                Err(format!("save task panic: {}", e)),
            )]
        });
        let _ = tx
            .send(AppMessage::MetadataEditorWriteComplete { results })
            .await;
    });
}

pub(super) fn reopen_metadata_editor_after_musicbrainz_population(
    app: &mut AppState,
    mut state: Box<super::app::MetadataEditorState>,
) {
    state.sync_active_presentation();
    if state.has_presentation_tabs() {
        app.pending_metadata_editor = Some(state.clone());
        app.active_overlay = ActiveOverlay::Confirmation {
            message: "Apply MusicBrainz tags to all matching presentations?".to_string(),
            action: ConfirmAction::ApplyMbToAllPresentations(state),
        };
    } else {
        app.active_overlay = ActiveOverlay::MetadataEditor(state);
    }
}

fn handle_metadata_editor_key(
    app: &mut AppState,
    key: KeyEvent,
    state: &mut Box<super::app::MetadataEditorState>,
    _tx: &mpsc::Sender<AppMessage>,
) {
    use super::app::MetadataEditorPhase;

    let total_rows = state.entries.len() + 1; // +1 for "Add field" row

    match state.phase {
        MetadataEditorPhase::Editing => {
            match key.code {
                // Command mode: park editor state, open command input.
                KeyCode::Char(':') => {
                    let parked = state.clone();
                    app.pending_metadata_editor = Some(parked);
                    app.active_overlay = ActiveOverlay::CommandInput {
                        input: super::text_input::TextInputState::empty(),
                        completion: None,
                    };
                    return;
                }
                KeyCode::Esc => {
                    if state.any_presentation_dirty() {
                        // TODO: confirmation dialog for unsaved changes
                        // For now, just close.
                    }
                    app.active_overlay = ActiveOverlay::None;
                }
                KeyCode::Tab => {
                    let changed = if key.modifiers.contains(KeyModifiers::SHIFT) {
                        state.previous_presentation_tab()
                    } else {
                        state.next_presentation_tab()
                    };
                    if changed {
                        if let Some(label) = state.active_presentation_label() {
                            app.set_status(format!("metadata editor: {}", label));
                        }
                    }
                }
                KeyCode::BackTab => {
                    if state.previous_presentation_tab() {
                        if let Some(label) = state.active_presentation_label() {
                            app.set_status(format!("metadata editor: {}", label));
                        }
                    }
                }
                KeyCode::Char(c) if key.modifiers.is_empty() && ('1'..='9').contains(&c) => {
                    let idx = (c as u8 - b'1') as usize;
                    if state.switch_presentation_tab(idx) {
                        if let Some(label) = state.active_presentation_label() {
                            app.set_status(format!("metadata editor: {}", label));
                        }
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    state.cursor = state.cursor.saturating_sub(1);
                    ensure_cursor_visible(state);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if state.cursor + 1 < total_rows {
                        state.cursor += 1;
                    }
                    ensure_cursor_visible(state);
                }
                KeyCode::Enter => {
                    if state.cursor < state.entries.len() {
                        let entry = &state.entries[state.cursor];
                        if !entry.is_binary && !state.deleted.contains(&state.cursor) {
                            // Use the entry's own dimension: per-track entries
                            // (e.g. TITLE on a single-image rip with embedded
                            // CUESHEET) have per_file_values.len() != paths.len().
                            if entry.is_mixed && entry.per_file_values.len() > 1 {
                                // Per-track detail view is allowed even in
                                // read-only mode (the per-track editing inside
                                // is gated separately in handle_detail_edit).
                                state.detail_field_idx = state.cursor;
                                state.detail_cursor = 0;
                                state.detail_scroll = 0;
                                state.detail_edit = None;
                                state.last_click = None;
                                state.phase = MetadataEditorPhase::DetailEdit;
                            } else if state.read_only {
                                app.set_status("read-only editor (SACD ISO)");
                            } else {
                                // Single value: inline edit.
                                state.edit_input = Some(super::text_input::TextInputState::new(
                                    entry.value.clone(),
                                ));
                                state.phase = MetadataEditorPhase::InlineEdit;
                            }
                        }
                    } else if state.read_only {
                        app.set_status("read-only editor (SACD ISO)");
                    } else {
                        // "Add field" row — start adding a new key.
                        state.add_key_input = Some(super::text_input::TextInputState::empty());
                        state.phase = MetadataEditorPhase::AddingKey;
                    }
                }
                // Delete-key convenience: same action as :d. The
                // bare-char `a`/`d`/`D`/`u`/`w` bindings have been
                // removed (no-bare-char-keys rule); user invokes them
                // via colon commands or footer-pill clicks.
                KeyCode::Delete => {
                    if state.read_only {
                        app.set_status("read-only editor (SACD ISO)");
                    } else {
                        metadata_editor_delete_cursor(state);
                    }
                }
                _ => {}
            }
        }
        MetadataEditorPhase::InlineEdit => {
            let input = match state.edit_input.as_mut() {
                Some(i) => i,
                None => {
                    state.phase = MetadataEditorPhase::Editing;
                    return;
                }
            };
            match key.code {
                KeyCode::Esc => {
                    state.edit_input = None;
                    state.phase = MetadataEditorPhase::Editing;
                    recalc_dirty(state);
                }
                KeyCode::Enter => {
                    let new_val = input.text.clone();
                    if state.cursor < state.entries.len() {
                        let entry = &mut state.entries[state.cursor];
                        entry.value = new_val.clone();
                        for v in &mut entry.per_file_values {
                            *v = new_val.clone();
                        }
                        entry.is_mixed = false;
                    }
                    state.edit_input = None;
                    state.phase = MetadataEditorPhase::Editing;
                    recalc_dirty(state);
                }
                // Up/Down: move cursor to the same column on the
                // previous/next display row in multiline editing.
                KeyCode::Up | KeyCode::Down => {
                    let has_nl = input.text.contains('\n') || input.text.contains('\r');
                    let char_count = input.text.chars().count();
                    // Compute val_max (must match draw_metadata_editor).
                    let area = crossterm::terminal::size().unwrap_or((80, 24));
                    let w = ((area.0 as usize) * 85 / 100)
                        .max(50)
                        .min(area.0 as usize - 2);
                    let inner_w = w.saturating_sub(2);
                    let key_col_w = 22usize;
                    let vm = inner_w.saturating_sub(key_col_w + 1);

                    if (char_count > super::draw_overlays::MULTILINE_EDIT_THRESHOLD || has_nl)
                        && vm > 0
                    {
                        let sanitized = input.text.replace("\r\n", "\n").replace('\r', "\n");

                        // Compute sanitized cursor position.
                        let mut sp = 0usize;
                        {
                            let mut pcr = false;
                            for (bi, c) in input.text.char_indices() {
                                if bi >= input.cursor {
                                    break;
                                }
                                if c == '\r' {
                                    pcr = true;
                                    continue;
                                }
                                if pcr {
                                    sp += if c == '\n' { 1 } else { 2 };
                                    pcr = false;
                                    continue;
                                }
                                sp += 1;
                            }
                            if pcr {
                                sp += 1;
                            }
                        }

                        // Map sanitized position to (row, col).
                        let mut cur_row = 0usize;
                        let mut cur_col = 0usize;
                        {
                            let mut idx = 0usize;
                            for c in sanitized.chars() {
                                if idx == sp {
                                    break;
                                }
                                if c == '\n' {
                                    cur_row += 1;
                                    cur_col = 0;
                                } else {
                                    cur_col += 1;
                                    if cur_col >= vm {
                                        cur_row += 1;
                                        cur_col = 0;
                                    }
                                }
                                idx += 1;
                            }
                        }

                        // Compute target row.
                        let target_row = if key.code == KeyCode::Up {
                            if cur_row == 0 {
                                0
                            } else {
                                cur_row - 1
                            }
                        } else {
                            cur_row + 1
                        };
                        let target_col = cur_col;

                        // Walk sanitized text to find the byte offset for
                        // (target_row, target_col), clamped to that row's length.
                        let mut drow = 0usize;
                        let mut dcol = 0usize;
                        let mut best_byte = input.text.len();
                        let mut orig_iter = input.text.char_indices().peekable();
                        let mut found = false;
                        for sc in sanitized.chars() {
                            if drow == target_row && dcol == target_col {
                                // Exact match.
                                if let Some(&(bi, _)) = orig_iter.peek() {
                                    best_byte = bi;
                                }
                                found = true;
                                break;
                            }
                            // Track the last valid position on the target row
                            // (in case target_col exceeds the row's length).
                            if drow == target_row {
                                if let Some(&(bi, _)) = orig_iter.peek() {
                                    best_byte = bi;
                                }
                            }
                            // Advance past target row → stop.
                            if drow > target_row {
                                // Went past — best_byte is the end of target_row.
                                found = true;
                                break;
                            }

                            // Advance orig_iter.
                            if let Some((_, oc)) = orig_iter.next() {
                                if oc == '\r' {
                                    if let Some(&(_, '\n')) = orig_iter.peek() {
                                        orig_iter.next();
                                    }
                                }
                            }

                            if sc == '\n' {
                                // Before moving to next row, record end-of-line
                                // position if we're on the target row.
                                if drow == target_row {
                                    if let Some(&(bi, _)) = orig_iter.peek() {
                                        best_byte = bi;
                                    }
                                }
                                drow += 1;
                                dcol = 0;
                            } else {
                                dcol += 1;
                                if dcol >= vm {
                                    if drow == target_row {
                                        if let Some(&(bi, _)) = orig_iter.peek() {
                                            best_byte = bi;
                                        }
                                    }
                                    drow += 1;
                                    dcol = 0;
                                }
                            }
                        }
                        // If target_row is past the last row, go to end.
                        if !found && drow == target_row {
                            // We're on the target row at end of text.
                            best_byte = input.text.len();
                        }

                        input.cursor = best_byte.min(input.text.len());
                    }
                    // For single-line values, Up/Down are no-ops.
                }
                _ => {
                    super::text_input::handle_text_input_key(input, &key);
                }
            }
        }
        MetadataEditorPhase::AddingKey => {
            let input = match state.add_key_input.as_mut() {
                Some(i) => i,
                None => {
                    state.phase = MetadataEditorPhase::Editing;
                    return;
                }
            };
            match key.code {
                KeyCode::Esc => {
                    state.add_key_input = None;
                    state.phase = MetadataEditorPhase::Editing;
                    recalc_dirty(state);
                }
                KeyCode::Enter => {
                    let key_name = input.text.trim().to_uppercase();
                    if !key_name.is_empty() {
                        let n = state.paths.len();
                        state.entries.push(super::probe::TagEntry {
                            display_key: key_name.clone(),
                            item_key: lofty::tag::ItemKey::Unknown(key_name),
                            value: String::new(),
                            original: String::new(),
                            is_binary: false,
                            is_mixed: false,
                            per_file_values: vec![String::new(); n],
                            per_file_originals: vec![String::new(); n],
                            mb_proposed_value: None,
                            mb_proposed_per_file: None,
                        });
                        state.cursor = state.entries.len() - 1;
                        state.add_key_input = None;
                        state.edit_input = Some(super::text_input::TextInputState::empty());
                        state.phase = MetadataEditorPhase::InlineEdit;
                        ensure_cursor_visible(state);
                    } else {
                        state.add_key_input = None;
                        state.phase = MetadataEditorPhase::Editing;
                    }
                }
                _ => {
                    super::text_input::handle_text_input_key(input, &key);
                }
            }
        }
        MetadataEditorPhase::DetailEdit => {
            // n_files is the focused entry's per-row dimension, not
            // paths.len(). For per-track entries on a single-image rip
            // (paths.len() == 1, per_file_values.len() == n_tracks)
            // these diverge.
            let field_idx = state.detail_field_idx;
            let n_files = state
                .entries
                .get(field_idx)
                .map(|e| e.per_file_values.len())
                .unwrap_or(state.paths.len());

            // If an inline edit is active within the detail overlay:
            if let Some(ref mut input) = state.detail_edit {
                match key.code {
                    KeyCode::Esc => {
                        state.detail_edit = None;
                    }
                    KeyCode::Enter => {
                        let new_val = input.text.clone();
                        if field_idx < state.entries.len() && state.detail_cursor < n_files {
                            state.entries[field_idx].per_file_values[state.detail_cursor] = new_val;
                            // Recalculate mixed state.
                            let all_same = state.entries[field_idx]
                                .per_file_values
                                .windows(2)
                                .all(|w| w[0] == w[1]);
                            state.entries[field_idx].is_mixed = !all_same;
                            let new_display = if all_same {
                                state.entries[field_idx].per_file_values[0].clone()
                            } else {
                                "<multiple values>".to_string()
                            };
                            state.entries[field_idx].value = new_display;
                        }
                        state.detail_edit = None;
                        recalc_dirty(state);
                    }
                    _ => {
                        super::text_input::handle_text_input_key(input, &key);
                    }
                }
                return;
            }

            // Detail overlay navigation.
            match key.code {
                KeyCode::Esc => {
                    // Return to main field list.
                    state.phase = MetadataEditorPhase::Editing;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    state.detail_cursor = state.detail_cursor.saturating_sub(1);
                    ensure_detail_visible(state);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if state.detail_cursor + 1 < n_files {
                        state.detail_cursor += 1;
                    }
                    ensure_detail_visible(state);
                }
                KeyCode::Enter => {
                    if state.read_only {
                        app.set_status("read-only editor (SACD ISO)");
                    } else if field_idx < state.entries.len() && state.detail_cursor < n_files {
                        let val =
                            state.entries[field_idx].per_file_values[state.detail_cursor].clone();
                        state.detail_edit = Some(super::text_input::TextInputState::new(val));
                    }
                }
                _ => {}
            }
        }
        MetadataEditorPhase::Saving => {
            // Block input while saving, except Esc to force-close.
            if key.code == KeyCode::Esc {
                app.active_overlay = ActiveOverlay::None;
            }
        }
    }
}

/// Save-time CUESHEET regeneration (Phase 4).
///
/// Detects per-track-dirty entries (entries with per_file_values.len()
/// > paths.len() whose values diverge from originals) AND album-level
/// dirty entries that the CUE serializer surfaces (ALBUM / ARTIST
/// [when dim 1] / DATE / GENRE / CATALOGNUMBER). If anything matches,
/// parses the on-disk CUESHEET (per_file_originals[0]) as the
/// structural template, β-mutates album-level fields from
/// state.entries, builds TrackOverride list from per-track entries,
/// regenerates the CUE text, and mutates state.entries[cue_idx]'s
/// value and per_file_values[0] in place. The caller's existing
/// snapshot+write_all_tags pipeline then sees the CUESHEET diff and
/// writes it through lofty.
///
/// Returns:
///   Ok(true)  — regen happened; caller should proceed with save
///   Ok(false) — no regen needed (no per-track edits, no relevant
///               album-level edits); caller proceeds normally
///   Err(reason) — refuse save (no CUESHEET anchor for per-track
///               edits, or CUESHEET parses to no tracks); caller
///               surfaces status and aborts
pub fn regenerate_cuesheet_for_save(
    state: &mut super::app::MetadataEditorState,
) -> Result<bool, String> {
    let n_paths = state.paths.len();

    // Helpers indexed-by-display-key.
    let entry_idx = |key: &str| -> Option<usize> {
        state
            .entries
            .iter()
            .position(|e| e.display_key.eq_ignore_ascii_case(key))
    };
    let dirty_at = |idx: usize| -> bool {
        let e = &state.entries[idx];
        e.per_file_values != e.per_file_originals
    };
    let is_per_track = |idx: usize| -> bool { state.entries[idx].per_file_values.len() != n_paths };

    // 1. Detect any per-track dirt or relevant album-level dirt.
    let per_track_dirty = state
        .entries
        .iter()
        .enumerate()
        .any(|(i, _)| is_per_track(i) && dirty_at(i));
    let album_keys_dirty = ["ALBUM", "DATE", "GENRE", "CATALOGNUMBER"]
        .iter()
        .filter_map(|k| entry_idx(k))
        .any(dirty_at);
    // ARTIST: only as album-level (dim 1). When per-track, it's
    // already covered by per_track_dirty.
    let artist_album_dirty = entry_idx("ARTIST")
        .filter(|&i| !is_per_track(i))
        .map(dirty_at)
        .unwrap_or(false);

    if !per_track_dirty && !album_keys_dirty && !artist_album_dirty {
        return Ok(false);
    }

    // 2. Locate CUESHEET anchor.
    //    - per_track_dirty without a CUESHEET entry is unrecoverable
    //      (the user's edits have no place to land). Refuse with a
    //      status. Catches the identity-match-without-track-lengths
    //      edge case from Phase 5: per-track entries got created but
    //      cue_from_mb_release returned Err so no CUESHEET embed.
    //    - !per_track_dirty (album-only dirt) without a CUESHEET is
    //      a no-op — the album-level tag writes through normally;
    //      no CUE to regenerate. Fall through to the regular save.
    //    - !per_track_dirty (album-only dirt) with a CUESHEET still
    //      regens so the CUE's album title / date / etc. stay in
    //      sync with the edited file tags (useful on multi-file
    //      selections where one file happens to carry a CUESHEET).
    let cue_idx = match entry_idx("CUESHEET") {
        Some(i) => i,
        None => {
            if per_track_dirty {
                return Err(
                    "save aborted: per-track edits without an embedded CUESHEET. \
                     Re-run :tags-mb on a per-track-eligible single-image rip to \
                     create one, or revert per-track changes."
                        .to_string(),
                );
            }
            return Ok(false);
        }
    };

    // Refuse when the user marked CUESHEET deleted but also has
    // per-track edits — the regen output would land in an entry that
    // the save loop is about to remove. Per-track edits would be lost.
    if state.deleted.contains(&cue_idx) && per_track_dirty {
        return Err(
            "save aborted: per-track edits with CUESHEET marked deleted. \
             Undelete the CUESHEET row or revert per-track changes."
                .to_string(),
        );
    }

    // Parse the CURRENT in-state CUESHEET (per_file_values[0]) — not
    // the on-disk originals[0]. When Phase 5 just created the entry
    // from MB, originals[0]=="" but values[0] holds the generated CUE
    // we want as the structural template. When the file already had
    // an embedded CUESHEET, values[0] == originals[0] (no inline edit
    // — CUESHEET rows are is_binary), so parsing either yields the
    // same result.
    let cue_text_template = state.entries[cue_idx]
        .per_file_values
        .first()
        .cloned()
        .unwrap_or_default();
    let mut parsed = super::cue_parser::parse_cue(&cue_text_template);
    if parsed.tracks.is_empty() {
        // No parsable tracks — malformed CUE or empty value. Per-track
        // edits have no structural anchor; album-only dirt is a no-op
        // (let the regular save handle normal tag writes).
        if per_track_dirty {
            return Err("save aborted: CUESHEET anchor parses to zero tracks; \
                 re-run :tags-mb to rebuild it from scratch."
                .to_string());
        }
        return Ok(false);
    }

    // 3. Refuse on track-count divergence BEFORE mutating `parsed`.
    //    This happens after :tags-mb-on-existing-CUE when MB's track
    //    count diverges from the file's CUESHEET track count: Phase 1
    //    grew per-track entries to MB's count (canonical for :tags-mb),
    //    but Phase 5 leaves the existing CUESHEET alone so the parsed
    //    structure stays at its original count. Truncating silently
    //    would drop user data; extending would invent timestamp-less
    //    tracks. User must manually delete the CUESHEET row first to
    //    re-bootstrap from MB.
    let n_parsed = parsed.tracks.len();
    for (key, idx_opt) in [
        ("TITLE", entry_idx("TITLE")),
        ("ARTIST", entry_idx("ARTIST")),
        ("ISRC", entry_idx("ISRC")),
    ] {
        if let Some(i) = idx_opt {
            let dim = state.entries[i].per_file_values.len();
            if dim != n_paths && dim != n_parsed {
                return Err(format!(
                    "save aborted: {} has {} per-track values but \
                     CUESHEET has {} tracks; delete the CUESHEET row \
                     and re-run :tags-mb to re-bootstrap.",
                    key, dim, n_parsed,
                ));
            }
        }
    }

    // 4. β album-level re-derive — mutate parsed CueSheet fields from
    //    current state.entries (only when the source entry is dim-1
    //    album-level; per-track entries are handled in step 5).
    let derive_album = |key: &str| -> Option<String> {
        entry_idx(key)
            .filter(|&i| !is_per_track(i))
            .and_then(|i| state.entries[i].per_file_values.first().cloned())
            .filter(|s| !s.is_empty())
    };
    if let Some(s) = derive_album("ALBUM") {
        parsed.title = Some(s);
    }
    if let Some(s) = derive_album("ARTIST") {
        parsed.performer = Some(s);
    }
    if let Some(s) = derive_album("DATE") {
        parsed.date = Some(s);
    }
    if let Some(s) = derive_album("GENRE") {
        parsed.genre = Some(s);
    }
    if let Some(s) = derive_album("CATALOGNUMBER") {
        parsed.catalog = Some(s);
    }

    // 5. Build TrackOverride list. For each parsed track, pull
    //    title/performer/isrc from the matching per-track entry slot
    //    when the entry is per-track-dim; otherwise None (preserves
    //    parsed value).
    let title_idx_pt = entry_idx("TITLE").filter(|&i| is_per_track(i));
    let artist_idx_pt = entry_idx("ARTIST").filter(|&i| is_per_track(i));
    let isrc_idx_pt = entry_idx("ISRC").filter(|&i| is_per_track(i));
    let pt_get = |idx: Option<usize>, i: usize| -> Option<String> {
        idx.and_then(|j| state.entries[j].per_file_values.get(i).cloned())
            .filter(|s| !s.is_empty())
    };
    let overrides: Vec<super::cue_generate::TrackOverride> = (0..parsed.tracks.len())
        .map(|i| super::cue_generate::TrackOverride {
            title: pt_get(title_idx_pt, i),
            performer: pt_get(artist_idx_pt, i),
            isrc: pt_get(isrc_idx_pt, i),
        })
        .collect();

    // 5. Regenerate. image_filename / format_tag come from the
    //    single-image audio file at paths[0].
    let path = state
        .paths
        .first()
        .ok_or_else(|| "save aborted: editor has no audio path".to_string())?;
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "save aborted: audio path has no filename".to_string())?;
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("flac");
    let format_tag = super::cue_generate::cue_format_tag(ext);
    let new_cue = super::cue_generate::regenerate_cue_with_overrides(
        &parsed, &overrides, filename, format_tag,
    );

    // 6. Mutate the CUESHEET entry. The existing snapshot+write_all_tags
    //    loop will pick up the diff (vals[0] != origs[0]) and write
    //    through lofty. is_binary stays true so display in the editor
    //    grid keeps showing the read-only summary.
    let entry = &mut state.entries[cue_idx];
    if entry.per_file_values.is_empty() {
        entry.per_file_values.push(new_cue.clone());
    } else {
        entry.per_file_values[0] = new_cue.clone();
    }
    entry.value = new_cue;

    Ok(true)
}

/// When the editor opens on a single audio file with no embedded
/// CUESHEET tag but a sidecar `.cue` file alongside, parse the sidecar
/// and inject a synthetic CUESHEET entry into `entries`. The synthetic
/// entry has `per_file_originals[0]=""` (signalling "not yet on the
/// file"), so the save loop will write a fresh embedded CUESHEET tag
/// to the file at first save while leaving the sidecar `.cue` on disk
/// untouched.
///
/// Skips silently when:
/// - `entries` already contains a CUESHEET entry (embedded wins)
/// - no sidecar `.cue` exists in the audio file's parent directory
/// - the sidecar can't be read or parses to fewer than 2 tracks
/// - the sidecar is track-by-track structured (different FILE per
///   TRACK) — those CUE timestamps reset per file and don't make
///   sense as a single-image embedded CUESHEET
pub fn inject_sidecar_cuesheet_if_present(
    entries: &mut Vec<super::probe::TagEntry>,
    audio_path: &std::path::Path,
) {
    let already_has_cue = entries
        .iter()
        .any(|e| e.display_key.eq_ignore_ascii_case("CUESHEET"));
    if already_has_cue {
        return;
    }
    let Some(sidecar) = super::cue_parser::find_sidecar_cue(audio_path) else {
        return;
    };
    // Read raw bytes + lossy-decode UTF-8: many real-world `.cue` files
    // are Shift_JIS / Windows-1252 (Japanese rips, foreign-character
    // titles). String::from_utf8_lossy replaces invalid bytes with
    // U+FFFD so we still surface track structure even when encoding
    // is wrong; CUE keywords (TITLE/PERFORMER/INDEX/etc.) are pure
    // ASCII so they parse correctly regardless. Mirrors the strategy
    // in cue_parser::parse_cue_file.
    let Ok(raw) = std::fs::read(&sidecar) else {
        return;
    };
    let text = String::from_utf8_lossy(&raw).into_owned();
    let parsed = super::cue_parser::parse_cue(&text);
    if parsed.tracks.len() < 2 {
        return;
    }
    // Track-by-track CUEs (different FILE per TRACK) are not safe to
    // re-embed against a single audio file — INDEX timestamps in those
    // sheets reset to 00:00:00 per file. Skip.
    let first_file = parsed.tracks.first().and_then(|t| t.file.as_deref());
    if parsed
        .tracks
        .iter()
        .any(|t| t.file.as_deref() != first_file)
    {
        return;
    }
    entries.push(super::probe::TagEntry {
        display_key: "CUESHEET".to_string(),
        item_key: lofty::tag::ItemKey::Unknown("CUESHEET".to_string()),
        value: super::probe::cue_summary_string(&text),
        original: String::new(),
        is_binary: true,
        is_mixed: false,
        per_file_values: vec![text],
        per_file_originals: vec![String::new()],
        mb_proposed_value: None,
        mb_proposed_per_file: None,
    });
}

/// Read an embedded CUESHEET tag from `entries`, parse it, and grow
/// or create TITLE / ARTIST / ISRC entries to per-track dimension.
/// Caller must ensure this is only invoked on single-image (paths.len()
/// == 1); for multi-file selections per-track CUESHEET semantics don't
/// apply.
///
/// Silently returns when:
/// - no CUESHEET entry exists or its value is empty
/// - the CUESHEET parses to fewer than two tracks
/// - all per-track values for a given field are empty (no data to show)
pub fn apply_embedded_cuesheet_per_track(entries: &mut Vec<super::probe::TagEntry>) {
    use lofty::tag::ItemKey;

    let cue_text = match entries
        .iter()
        .find(|e| e.display_key.eq_ignore_ascii_case("CUESHEET"))
        .and_then(|e| e.per_file_values.first())
        .filter(|s| !s.is_empty())
    {
        Some(s) => s.clone(),
        None => return,
    };

    let parsed = super::cue_parser::parse_cue(&cue_text);
    if parsed.tracks.len() < 2 {
        return;
    }

    let titles: Vec<String> = parsed
        .tracks
        .iter()
        .map(|t| t.title.clone().unwrap_or_default())
        .collect();
    let artists: Vec<String> = parsed
        .tracks
        .iter()
        .map(|t| t.performer.clone().unwrap_or_default())
        .collect();
    let isrcs: Vec<String> = parsed
        .tracks
        .iter()
        .map(|t| t.isrc.clone().unwrap_or_default())
        .collect();

    grow_or_create_per_track(entries, "TITLE", ItemKey::TrackTitle, titles);
    grow_or_create_per_track(entries, "ARTIST", ItemKey::TrackArtist, artists);
    grow_or_create_per_track(entries, "ISRC", ItemKey::Isrc, isrcs);
}

/// Replace `entries[key].per_file_values` (and originals) with `values`,
/// or create the entry if absent. Skips when all values are empty so we
/// don't add a no-op row. Sets `is_mixed` and the display `value`
/// against the new dimension.
fn grow_or_create_per_track(
    entries: &mut Vec<super::probe::TagEntry>,
    key: &str,
    item_key: lofty::tag::ItemKey,
    values: Vec<String>,
) {
    if values.iter().all(|s| s.is_empty()) {
        return;
    }
    let dim = values.len();
    let all_same = values.windows(2).all(|w| w[0] == w[1]);
    let is_mixed = !all_same && dim > 1;
    let display_value = if is_mixed {
        "<multiple values>".to_string()
    } else {
        values.first().cloned().unwrap_or_default()
    };

    if let Some(idx) = entries
        .iter()
        .position(|e| e.display_key.eq_ignore_ascii_case(key))
    {
        let entry = &mut entries[idx];
        entry.per_file_values = values.clone();
        entry.per_file_originals = values;
        entry.is_mixed = is_mixed;
        entry.value = display_value.clone();
        entry.original = display_value;
    } else {
        entries.push(super::probe::TagEntry {
            display_key: key.to_string(),
            item_key,
            value: display_value.clone(),
            original: display_value,
            is_binary: false,
            is_mixed,
            per_file_values: values.clone(),
            per_file_originals: values,
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });
    }
}

/// Result of a `:fix-caps` pass — counts so the status message can
/// explain what happened.
pub(super) struct FixCapsResult {
    pub changed_values: usize,
    pub skipped_deleted: usize,
}

/// Apply capitalization rules to TITLE / ARTIST / ALBUM / ALBUMARTIST
/// / PERFORMER entries in `state`. When `focus` is `Some(idx)` (detail-
/// overlay invocation), only that entry is processed. When `None`
/// (main-editor invocation), iterates all entries.
///
/// **Mixed entries are NOT skipped.** Capitalization runs on every
/// per-file value regardless of `is_mixed`; the post-cap recompute
/// preserves `is_mixed=true` for entries whose per-track values still
/// differ (which they do — capitalization is deterministic). The
/// `<multiple values>` placeholder in `entry.value` stays intact, so
/// the main-grid display doesn't change, but the per-track values
/// (visible in the detail overlay) are now capitalized. User
/// expectation: one `:fix-caps` invocation fixes everything; no need
/// to re-invoke from detail.
///
/// Deleted entries (`state.deleted`) are still skipped — capitalizing
/// a row marked for deletion is wasteful.
///
/// is_mixed is recomputed against the entry's own `per_file_values
/// .len()` (NOT `state.paths.len()`) — same dim invariant Phase 1c
/// fixed in `recompute_and_stamp_mb_proposed`.
pub(super) fn fix_caps_for_state(
    state: &mut super::app::MetadataEditorState,
    focus: Option<usize>,
) -> FixCapsResult {
    use crate::convert::renaming::{capitalize_section, capitalize_title};

    let mut result = FixCapsResult {
        changed_values: 0,
        skipped_deleted: 0,
    };

    let indices: Vec<usize> = match focus {
        Some(i) => vec![i],
        None => (0..state.entries.len()).collect(),
    };

    for i in indices {
        if i >= state.entries.len() {
            continue;
        }
        // Deleted-skip applies to main-editor invocations only.
        // Detail-overlay invocations honor the user's explicit focus.
        if focus.is_none() && state.deleted.contains(&i) {
            result.skipped_deleted += 1;
            continue;
        }

        let entry = &mut state.entries[i];
        let key_upper = entry.display_key.to_ascii_uppercase();
        let cap_fn: fn(&str) -> String = match key_upper.as_str() {
            "TITLE" => capitalize_title,
            "ARTIST" | "ALBUM" | "ALBUMARTIST" | "PERFORMER" => capitalize_section,
            _ => continue,
        };
        for v in &mut entry.per_file_values {
            if !v.is_empty() {
                let new_val = cap_fn(v);
                if new_val != *v {
                    *v = new_val;
                    result.changed_values += 1;
                }
            }
        }
        // Recompute is_mixed using the entry's OWN dim, not paths.len().
        let dim = entry.per_file_values.len();
        let all_same = entry.per_file_values.windows(2).all(|w| w[0] == w[1]);
        entry.is_mixed = !all_same && dim > 1;
        entry.value = if entry.is_mixed {
            "<multiple values>".to_string()
        } else {
            entry.per_file_values.first().cloned().unwrap_or_default()
        };
    }

    result
}

/// True when a tag's display_key is one we apply capitalization rules
/// to. Used to gate `:fix-caps` footer pill visibility on the detail
/// overlay (only show for fields where fix-caps would actually do
/// something).
pub(super) fn is_fix_caps_applicable(display_key: &str) -> bool {
    matches!(
        display_key.to_ascii_uppercase().as_str(),
        "TITLE" | "ARTIST" | "ALBUM" | "ALBUMARTIST" | "PERFORMER"
    )
}

/// Overlay per-track `values` onto an entry's `per_file_values`,
/// preserving `per_file_originals` so a revert can restore the
/// pre-overlay state. If the entry is absent, create it with the
/// supplied values and empty originals (revert restores empties).
/// Used by `:tags-cue-sidecar` to refresh per-track titles/artists/
/// ISRCs from a sidecar `.cue` without losing the user's prior
/// editor state.
fn overlay_per_track_values(
    entries: &mut Vec<super::probe::TagEntry>,
    key: &str,
    item_key: lofty::tag::ItemKey,
    values: Vec<String>,
) {
    let all_empty = values.iter().all(|s| s.is_empty());
    let dim = values.len();
    let all_same = values.windows(2).all(|w| w[0] == w[1]);
    let is_mixed = !all_same && dim > 1;
    let display_value = if is_mixed {
        "<multiple values>".to_string()
    } else {
        values.first().cloned().unwrap_or_default()
    };

    if let Some(idx) = entries
        .iter()
        .position(|e| e.display_key.eq_ignore_ascii_case(key))
    {
        // Overlay existing entry — even when `values` is all-empty
        // (sidecar deliberately cleared the field; user wants editor
        // to reflect that). Originals resize to match `dim`: grow by
        // replicating the existing first slot, shrink by truncation.
        // Keeps len(values) == len(originals) invariant.
        let entry = &mut entries[idx];
        let pad = entry
            .per_file_originals
            .first()
            .cloned()
            .unwrap_or_default();
        entry.per_file_originals.resize(dim, pad);
        entry.per_file_values = values;
        entry.is_mixed = is_mixed;
        entry.value = display_value;
    } else if !all_empty {
        // Skip creating an entirely-empty no-op entry. (When the entry
        // already exists, we DO overlay even with all-empty above —
        // that's the user clearing values via sidecar.)
        entries.push(super::probe::TagEntry {
            display_key: key.to_string(),
            item_key,
            value: display_value,
            original: String::new(),
            is_binary: false,
            is_mixed,
            per_file_values: values,
            per_file_originals: vec![String::new(); dim],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });
    }
}

/// Reload per-track values from a sidecar `.cue` file alongside the
/// audio. Used by `:tags-cue-sidecar`.
///
/// Behavior: parses the sidecar, then overlays per-track TITLE /
/// ARTIST / ISRC values onto the existing editor entries (preserving
/// originals so the user can revert). The CUESHEET entry's
/// `per_file_values[0]` is also updated so save-time regen uses the
/// fresh sidecar text as its structural template.
///
/// Returns Err with a user-facing message on:
/// - non-single-image (paths.len() != 1)
/// - no sidecar `.cue` in audio's parent directory
/// - sidecar unreadable / parses to <2 tracks / track-by-track
///   structure
pub(super) fn reload_from_sidecar_cue(
    state: &mut super::app::MetadataEditorState,
) -> Result<String, String> {
    if state.paths.len() != 1 {
        return Err(":tags-cue-sidecar requires a single-image rip (one file)".to_string());
    }
    let audio = &state.paths[0];
    let sidecar = super::cue_parser::find_sidecar_cue(audio)
        .ok_or_else(|| ":tags-cue-sidecar: no .cue file found alongside audio".to_string())?;
    let raw = std::fs::read(&sidecar).map_err(|e| {
        format!(
            ":tags-cue-sidecar: failed to read {}: {}",
            sidecar.display(),
            e
        )
    })?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let parsed = super::cue_parser::parse_cue(&text);
    if parsed.tracks.len() < 2 {
        return Err(format!(
            ":tags-cue-sidecar: {} parses to {} tracks (need >= 2)",
            sidecar
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| sidecar.display().to_string()),
            parsed.tracks.len(),
        ));
    }
    let first_file = parsed.tracks.first().and_then(|t| t.file.as_deref());
    if parsed
        .tracks
        .iter()
        .any(|t| t.file.as_deref() != first_file)
    {
        return Err(":tags-cue-sidecar: sidecar is track-by-track (multiple FILE refs); not safe to embed against a single image".to_string());
    }

    // Update or create the CUESHEET entry's per_file_values[0]
    // (preserving originals so revert restores the prior state).
    use lofty::tag::ItemKey;
    if let Some(idx) = state
        .entries
        .iter()
        .position(|e| e.display_key.eq_ignore_ascii_case("CUESHEET"))
    {
        let entry = &mut state.entries[idx];
        if entry.per_file_values.is_empty() {
            entry.per_file_values.push(text.clone());
        } else {
            entry.per_file_values[0] = text.clone();
        }
        entry.value = super::probe::cue_summary_string(&text);
    } else {
        state.entries.push(super::probe::TagEntry {
            display_key: "CUESHEET".to_string(),
            item_key: ItemKey::Unknown("CUESHEET".to_string()),
            value: super::probe::cue_summary_string(&text),
            original: String::new(),
            is_binary: true,
            is_mixed: false,
            per_file_values: vec![text.clone()],
            per_file_originals: vec![String::new()],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });
    }

    // Overlay per-track values, preserving originals.
    let titles: Vec<String> = parsed
        .tracks
        .iter()
        .map(|t| t.title.clone().unwrap_or_default())
        .collect();
    let artists: Vec<String> = parsed
        .tracks
        .iter()
        .map(|t| t.performer.clone().unwrap_or_default())
        .collect();
    let isrcs: Vec<String> = parsed
        .tracks
        .iter()
        .map(|t| t.isrc.clone().unwrap_or_default())
        .collect();
    overlay_per_track_values(&mut state.entries, "TITLE", ItemKey::TrackTitle, titles);
    overlay_per_track_values(&mut state.entries, "ARTIST", ItemKey::TrackArtist, artists);
    overlay_per_track_values(&mut state.entries, "ISRC", ItemKey::Isrc, isrcs);

    state.dirty = super::probe::metadata_editor_has_changes(state);

    let n_tracks = parsed.tracks.len();
    let name = sidecar
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| sidecar.display().to_string());
    Ok(format!(
        ":tags-cue-sidecar: loaded {} tracks from {}",
        n_tracks, name
    ))
}

pub fn open_metadata_editor(app: &mut AppState) {
    // Collect paths — expand directories recursively to find nested
    // audio files (e.g., disc 01/disc 02 folders).
    let sel = super::command::collect_selection_for_file_ops(app);

    // SACD ISO short-circuit: if the selection is exactly one SACD ISO
    // file, surface ScarletBook metadata (album-level + per-track
    // titles/artists/composers/ISRC) via parse_sacd_iso, bypassing the
    // lofty/tag pipeline entirely. Read-only — write-back is gated by
    // state.read_only at save time. See open_metadata_editor_for_sacd
    // for the surfacing rules.
    //
    // Also handles the "folder containing one SACD ISO" case (common
    // for SME / MFSL Japanese reissues that ship the disc as a
    // single .iso inside a folder named for the album): a right-click
    // on the folder lands the editor on its enclosed ISO. We scan only
    // the directory's immediate children — not recursively — to keep
    // the cost bounded and to avoid surprising the user with deep
    // descents into archive trees.
    if sel.len() == 1 {
        if super::sacd::is_sacd_iso(&sel[0]) {
            open_metadata_editor_for_sacd(app, sel[0].clone());
            return;
        }
        if crate::disc::dvda_utils::is_dvda_source(&sel[0]) {
            open_metadata_editor_for_dvda(app, sel[0].clone());
            return;
        }
        if sel[0].is_dir() {
            if let Some(iso) = find_single_sacd_in_dir(&sel[0]) {
                open_metadata_editor_for_sacd(app, iso);
                return;
            }
            if let Some(source) = find_single_dvda_source_in_dir(&sel[0]) {
                open_metadata_editor_for_dvda(app, source);
                return;
            }
        }
    }

    let mut paths: Vec<std::path::PathBuf> = super::browse::expand_paths_to_audio(&sel)
        .into_iter()
        .filter(|p| {
            matches!(
                super::browse::classify_file(p),
                super::browse::EntryKind::AudioFile(_)
            )
        })
        .collect();

    if paths.is_empty() {
        app.set_status("No audio files selected");
        return;
    }

    // Read and merge tags from all files.
    let mut entries = match super::probe::read_all_tags_merged(&paths) {
        Ok(e) => e,
        Err(e) => {
            app.set_status(format!("Failed to read tags: {}", e));
            return;
        }
    };

    // Phase 2: single-image per-track surfacing.
    // When the editor opens on a single audio file, surface per-track
    // structure (TITLE / ARTIST / ISRC) into the editor whether the
    // truth lives in an embedded CUESHEET tag or in a sidecar `.cue`
    // alongside. inject_sidecar_cuesheet_if_present synthesizes a
    // CUESHEET entry from the sidecar when there's no embedded one;
    // apply_embedded_cuesheet_per_track then grows TITLE / ARTIST /
    // ISRC entries to per-track dim from whichever CUESHEET source
    // is present. Save-time regen (Phase 4) writes user edits back to
    // the embedded CUESHEET tag.
    if paths.len() == 1 {
        inject_sidecar_cuesheet_if_present(&mut entries, &paths[0]);
        apply_embedded_cuesheet_per_track(&mut entries);
    }

    // Sort files by (disc, track, filename) for logical display order.
    // Entry-aware sort: paths AND per-file vectors are permuted together
    // so the indexing relationship stays consistent.
    super::probe::sort_paths_and_entries_by_track(&mut paths, &mut entries);

    // Auto-populate TITLE and TRACKNUMBER from filenames where missing.
    let mut did_auto_populate = false;
    {
        let n = paths.len();

        // Find or create TRACKNUMBER entry.
        let tn_idx = entries
            .iter()
            .position(|e| e.display_key.to_ascii_uppercase() == "TRACKNUMBER");
        let title_idx = entries
            .iter()
            .position(|e| e.display_key.to_ascii_uppercase() == "TITLE");

        // Ensure TRACKNUMBER entry exists.
        let tn_idx = match tn_idx {
            Some(i) => i,
            None => {
                entries.push(super::probe::TagEntry {
                    display_key: "TRACKNUMBER".to_string(),
                    item_key: lofty::tag::ItemKey::TrackNumber,
                    value: String::new(),
                    original: String::new(),
                    is_binary: false,
                    is_mixed: false,
                    per_file_values: vec![String::new(); n],
                    per_file_originals: vec![String::new(); n],
                    mb_proposed_value: None,
                    mb_proposed_per_file: None,
                });
                entries.len() - 1
            }
        };

        // Ensure TITLE entry exists.
        let title_idx = match title_idx {
            Some(i) => i,
            None => {
                entries.push(super::probe::TagEntry {
                    display_key: "TITLE".to_string(),
                    item_key: lofty::tag::ItemKey::TrackTitle,
                    value: String::new(),
                    original: String::new(),
                    is_binary: false,
                    is_mixed: false,
                    per_file_values: vec![String::new(); n],
                    per_file_originals: vec![String::new(); n],
                    mb_proposed_value: None,
                    mb_proposed_per_file: None,
                });
                entries.len() - 1
            }
        };

        // Parse filenames and fill empty values.
        for i in 0..n {
            let stem = paths[i].file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let (parsed_track, parsed_title) = super::probe::parse_title_from_filename(stem);

            if entries[tn_idx].per_file_values[i].is_empty() {
                if let Some(t) = parsed_track {
                    entries[tn_idx].per_file_values[i] = t.to_string();
                    did_auto_populate = true;
                }
            }
            if entries[title_idx].per_file_values[i].is_empty() {
                if let Some(ref t) = parsed_title {
                    entries[title_idx].per_file_values[i] = t.clone();
                    did_auto_populate = true;
                }
            }
        }

        // Recalculate is_mixed and value for affected entries.
        for idx in [tn_idx, title_idx] {
            let all_same = entries[idx]
                .per_file_values
                .windows(2)
                .all(|w| w[0] == w[1]);
            entries[idx].is_mixed = !all_same;
            let new_val = if !all_same {
                "<multiple values>".to_string()
            } else {
                entries[idx].per_file_values[0].clone()
            };
            entries[idx].value = new_val;
        }

        // Re-sort entries so newly created TITLE/TRACKNUMBER appear in
        // standard position (not appended at end).
        if tn_idx >= entries.len() - 2 || title_idx >= entries.len() - 2 {
            // Only needed if we actually pushed new entries.
            entries.sort_by(|a, b| {
                let a_upper = a.display_key.to_ascii_uppercase();
                let b_upper = b.display_key.to_ascii_uppercase();
                let a_pos = super::probe::STANDARD_KEY_ORDER
                    .iter()
                    .position(|&k| k == a_upper);
                let b_pos = super::probe::STANDARD_KEY_ORDER
                    .iter()
                    .position(|&k| k == b_upper);
                match (a_pos, b_pos) {
                    (Some(ai), Some(bi)) => ai.cmp(&bi),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => a_upper.cmp(&b_upper),
                }
            });
        }
    }

    // Build per-file context labels from sorted paths/entries.
    // Short labels: "D1.01" or "01" or filename stem (fallback).
    let file_labels: Vec<String> = {
        let track_entry = entries
            .iter()
            .find(|e| e.display_key.to_ascii_uppercase() == "TRACKNUMBER");
        let disc_entry = entries
            .iter()
            .find(|e| e.display_key.to_ascii_uppercase() == "DISCNUMBER");
        let has_multi_disc = disc_entry
            .map(|e| {
                let unique: std::collections::HashSet<&str> = e
                    .per_file_values
                    .iter()
                    .filter(|v| !v.is_empty())
                    .map(|v| v.as_str())
                    .collect();
                unique.len() > 1
            })
            .unwrap_or(false);

        paths
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let tn = track_entry
                    .and_then(|e| e.per_file_values.get(i))
                    .filter(|v| !v.is_empty());
                let dn = if has_multi_disc {
                    disc_entry
                        .and_then(|e| e.per_file_values.get(i))
                        .filter(|v| !v.is_empty())
                } else {
                    None
                };
                match (dn, tn) {
                    (Some(d), Some(t)) => format!("D{}.{:>02}", d, t),
                    (None, Some(t)) => format!("{:>02}", t),
                    _ => {
                        // Fallback: filename stem, truncated.
                        p.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("?")
                            .to_string()
                    }
                }
            })
            .collect()
    };

    app.active_overlay = ActiveOverlay::MetadataEditor(Box::new(super::app::MetadataEditorState {
        paths,
        entries,
        cursor: 0,
        scroll: 0,
        last_click: None,
        edit_input: None,
        add_key_input: None,
        phase: super::app::MetadataEditorPhase::Editing,
        dirty: did_auto_populate,
        deleted: Vec::new(),
        file_labels,
        detail_field_idx: 0,
        detail_cursor: 0,
        detail_scroll: 0,
        detail_edit: None,
        mb_back: None,
        gnudb_back: None,
        read_only: false,
        sacd_sidecar_path: None,
        sacd_area_kind: None,
        sacd_stereo_durations: None,
        sacd_multi_channel_durations: None,
        presentation_tabs: Vec::new(),
        active_tab: 0,
    }));
}


/// Open the metadata editor against a DVD-Audio ISO or AUDIO_TS directory.
/// The editor writes foo_input_dvda-compatible `{STORE_ID}.xml` files.
pub(super) fn open_metadata_editor_for_dvda(app: &mut AppState, source_path: std::path::PathBuf) {
    open_metadata_editor_for_dvda_with_group(app, source_path, None, None);
}

#[allow(dead_code)]
pub(super) fn open_metadata_editor_for_dvda_group(
    app: &mut AppState,
    source_path: std::path::PathBuf,
    group_nr: u8,
) {
    open_metadata_editor_for_dvda_with_group(app, source_path, Some(group_nr), None);
}

fn open_metadata_editor_for_dvda_at_track(
    app: &mut AppState,
    source_path: std::path::PathBuf,
    initial_track: Option<usize>,
) {
    open_metadata_editor_for_dvda_with_group(app, source_path, None, initial_track);
}

fn open_metadata_editor_for_dvda_with_group(
    app: &mut AppState,
    source_path: std::path::PathBuf,
    group_nr: Option<u8>,
    initial_track: Option<usize>,
) {
    let (disc, store_id, metabase_path, metabase, parse_note) =
        match load_dvda_metabase_context(&source_path) {
            Ok(v) => v,
            Err(e) => {
                app.set_status(e);
                return;
            }
        };

    let disc_contents = current_dvda_disc_contents(app, &source_path);

    match build_dvda_multitab_editor_state(
        &source_path,
        &disc,
        disc_contents.as_ref(),
        &store_id,
        metabase_path.as_ref(),
        metabase.as_ref(),
        group_nr,
    ) {
        Ok((mut state, group_label, n_tracks)) => {
            if let Some(track_index) = initial_track {
                focus_metadata_editor_on_track(&mut state, track_index);
            }
            let src = if metabase.is_some() {
                "metabase+IFO"
            } else if parse_note.is_some() {
                "IFO (metabase malformed)"
            } else {
                "IFO"
            };
            let mode = if state.read_only { "read-only" } else { "writable" };
            let choice_note = dvda_editor_tab_choice_note(&state);
            app.set_status(format!(
                "DVD-Audio editor opened ({}, {} tracks, {}, {}){}",
                group_label, n_tracks, src, mode, choice_note,
            ));
            app.active_overlay = super::app::ActiveOverlay::MetadataEditor(Box::new(state));
        }
        Err(msg) => app.set_status(msg),
    }
}

fn current_dvda_disc_contents(
    app: &AppState,
    source_path: &std::path::Path,
) -> Option<crate::disc::model::DiscContents> {
    if let super::app::SourceMode::MultiTrack {
        path,
        disc_contents: Some(contents),
        ..
    } = &app.convert.source.mode
    {
        if path.as_path() == source_path {
            return Some((**contents).clone());
        }
    }

    crate::disc::dvda_utils::map_dvda_source(source_path).ok()
}

fn dvda_editor_tab_choice_note(state: &super::app::MetadataEditorState) -> String {
    if state.presentation_tabs.len() <= 1 {
        return String::new();
    }

    let hint = state
        .presentation_tabs
        .iter()
        .filter_map(|tab| match &tab.id {
            crate::disc::model::PresentationId::DvdAudioGroup(_) => Some(tab.label.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("; ");

    if hint.is_empty() {
        String::new()
    } else {
        format!("; switch with :dvda-group <n> [{}]", hint)
    }
}

fn load_dvda_metabase_context(
    source_path: &std::path::Path,
) -> Result<(
    crate::tui::dvda::DvdaDisc,
    String,
    Option<std::path::PathBuf>,
    Option<super::dvda_metabase::DvdaMetabase>,
    Option<String>,
), String> {
    if crate::disc::dvda_utils::is_dvda_directory(source_path) {
        let volume = crate::tui::dvda::DirectoryDvdaVolume::new(source_path);
        return load_dvda_metabase_context_from_volume(&volume, source_path);
    }
    let volume = crate::tui::dvda::IsoUdfDvdaVolume::open(source_path)
        .map_err(|e| format!("DVD-Audio ISO open failed for '{}': {}", source_path.display(), e))?;
    load_dvda_metabase_context_from_volume(&volume, source_path)
}

fn load_dvda_metabase_context_from_volume(
    volume: &dyn crate::tui::dvda::DvdaVolume,
    source_path: &std::path::Path,
) -> Result<(
    crate::tui::dvda::DvdaDisc,
    String,
    Option<std::path::PathBuf>,
    Option<super::dvda_metabase::DvdaMetabase>,
    Option<String>,
), String> {
    let disc = crate::tui::dvda::parse_dvda_volume(volume)
        .map_err(|e| format!("DVD-Audio parse failed for '{}': {}", source_path.display(), e))?;
    let store_id = super::dvda_metabase::compute_store_id(volume)
        .ok_or_else(|| "DVD-Audio: could not read AUDIO_TS.IFO for metabase store id".to_string())?;
    let metabase_path = super::dvda_metabase::find_metabase(source_path, &store_id);
    let mut parse_note = None;
    let metabase = metabase_path
        .as_ref()
        .and_then(|p| match super::dvda_metabase::parse_metabase(p) {
            Ok(m) => Some(m),
            Err(e) => {
                parse_note = Some(e.to_string());
                log::warn!("DVD-Audio metabase parse failed for '{}': {}", p.display(), e);
                None
            }
        });
    Ok((disc, store_id, metabase_path, metabase, parse_note))
}

#[derive(Debug, Clone)]
struct DvdaPresentationTabSpec {
    group_nr: u8,
    label: String,
}

fn dvda_presentation_tab_specs(
    disc: &crate::tui::dvda::DvdaDisc,
    disc_contents: Option<&crate::disc::model::DiscContents>,
    available_groups: &[super::dvda_metabase::DvdaGroupSummary],
) -> Vec<DvdaPresentationTabSpec> {
    let mut specs = Vec::new();
    let mut seen = std::collections::BTreeSet::new();

    if let Some(contents) = disc_contents {
        for presentation in &contents.presentations {
            let group_nr = match &presentation.id {
                crate::disc::model::PresentationId::DvdAudioGroup(group_nr) => *group_nr,
                _ => continue,
            };
            if !available_groups.iter().any(|group| group.group_nr == group_nr) {
                continue;
            }
            if !seen.insert(group_nr) {
                continue;
            }
            specs.push(DvdaPresentationTabSpec {
                group_nr,
                label: dvda_presentation_tab_label(presentation, group_nr),
            });
        }
    }

    if !specs.is_empty() {
        return specs;
    }

    available_groups
        .iter()
        .map(|summary| DvdaPresentationTabSpec {
            group_nr: summary.group_nr,
            label: dvda_fallback_group_label(disc, summary.group_nr),
        })
        .collect()
}

fn dvda_presentation_label_for_group(
    disc: &crate::tui::dvda::DvdaDisc,
    disc_contents: Option<&crate::disc::model::DiscContents>,
    group_nr: u8,
) -> String {
    disc_contents
        .and_then(|contents| {
            contents.presentations.iter().find_map(|presentation| match &presentation.id {
                crate::disc::model::PresentationId::DvdAudioGroup(n) if *n == group_nr => {
                    Some(dvda_presentation_tab_label(presentation, group_nr))
                }
                _ => None,
            })
        })
        .unwrap_or_else(|| dvda_fallback_group_label(disc, group_nr))
}

fn dvda_presentation_tab_label(
    presentation: &crate::disc::model::DiscPresentation,
    group_nr: u8,
) -> String {
    let detail = if presentation.label.trim().is_empty() {
        dvda_audio_format_label(&presentation.format).unwrap_or_else(|| "audio presentation".to_string())
    } else {
        presentation.label.trim().to_string()
    };

    let lower = detail.to_ascii_lowercase();
    let group_prefix = format!("group {}", group_nr).to_ascii_lowercase();
    if lower.starts_with(&group_prefix) {
        detail
    } else {
        format!("Group {}: {}", group_nr, detail)
    }
}

fn dvda_fallback_group_label(disc: &crate::tui::dvda::DvdaDisc, group_nr: u8) -> String {
    super::dvda_metabase::select_group(disc, Some(group_nr))
        .map(|group| super::dvda_metabase::group_label(disc, group))
        .unwrap_or_else(|_| format!("Group {}", group_nr))
}

fn dvda_audio_format_label(format: &crate::disc::model::AudioPresentationFormat) -> Option<String> {
    let mut parts = Vec::new();

    if let Some(codec) = non_empty(&format.codec) {
        parts.push(codec.to_string());
    }

    let mut rate_depth = String::new();
    if let Some(sample_rate) = format.sample_rate {
        rate_depth.push_str(&format_sample_rate_hz(sample_rate));
    }
    if let Some(bit_depth) = format.bit_depth {
        if !rate_depth.is_empty() {
            rate_depth.push('/');
        }
        rate_depth.push_str(&format!("{}-bit", bit_depth));
    }
    if !rate_depth.is_empty() {
        parts.push(rate_depth);
    }

    if let Some(layout) = non_empty(&format.channel_layout) {
        parts.push(layout.to_string());
    } else if let Some(channels) = format.channels {
        parts.push(format_channel_count(channels));
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

fn format_sample_rate_hz(sample_rate: u32) -> String {
    if sample_rate % 1000 == 0 {
        format!("{}kHz", sample_rate / 1000)
    } else if sample_rate % 100 == 0 {
        format!("{:.1}kHz", sample_rate as f64 / 1000.0)
    } else {
        format!("{}Hz", sample_rate)
    }
}

fn format_channel_count(channels: u8) -> String {
    match channels {
        0 => "unknown channels".to_string(),
        1 => "Mono".to_string(),
        2 => "Stereo".to_string(),
        6 => "5.1".to_string(),
        8 => "7.1".to_string(),
        n => format!("{} ch", n),
    }
}

pub fn build_dvda_editor_state(
    source_path: &std::path::Path,
    disc: &crate::tui::dvda::DvdaDisc,
    disc_contents: Option<&crate::disc::model::DiscContents>,
    store_id: &str,
    existing_metabase_path: Option<&std::path::PathBuf>,
    metabase: Option<&super::dvda_metabase::DvdaMetabase>,
    selected_group_nr: Option<u8>,
) -> Result<(super::app::MetadataEditorState, String, usize), String> {
    use lofty::tag::ItemKey;
    use super::probe::TagEntry;

    let group = super::dvda_metabase::select_group(disc, selected_group_nr)
        .map_err(|e| e.to_string())?;
    let track_addrs = super::dvda_metabase::group_track_addrs(disc, group);
    let n_tracks = track_addrs.len();
    if n_tracks == 0 {
        return Err("DVD-Audio group has zero tracks".to_string());
    }
    let group_track_ids: Vec<String> = track_addrs.iter().map(|addr| addr.id.clone()).collect();

    let paths = vec![source_path.to_path_buf(); n_tracks];
    let mut entries: Vec<TagEntry> = Vec::new();

    let push_album = |entries: &mut Vec<TagEntry>, display_key: &str, item_key: ItemKey, value: String| {
        if value.trim().is_empty() {
            return;
        }
        let vals = vec![value.clone(); n_tracks];
        entries.push(TagEntry {
            display_key: display_key.to_string(),
            item_key,
            value: value.clone(),
            original: value,
            is_binary: false,
            is_mixed: false,
            per_file_values: vals.clone(),
            per_file_originals: vals,
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });
    };

    let push_per_track = |entries: &mut Vec<TagEntry>, display_key: &str, item_key: ItemKey, values: Vec<String>| {
        if values.iter().all(|s| s.trim().is_empty()) {
            return;
        }
        let all_same = values.windows(2).all(|w| w[0] == w[1]);
        let value = if all_same {
            values.first().cloned().unwrap_or_default()
        } else {
            "<multiple values>".to_string()
        };
        entries.push(TagEntry {
            display_key: display_key.to_string(),
            item_key,
            value: value.clone(),
            original: value,
            is_binary: false,
            is_mixed: !all_same,
            per_file_originals: values.clone(),
            per_file_values: values,
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });
    };

    let group_value = group.group_nr.to_string();
    push_album(
        &mut entries,
        "DVDA_GROUP",
        ItemKey::Unknown("DVDA_GROUP".to_string()),
        group_value,
    );

    for (display_key, item_key, aliases) in [
        ("ALBUM", ItemKey::AlbumTitle, vec!["ALBUM"]),
        ("ALBUMARTIST", ItemKey::AlbumArtist, vec!["ALBUMARTIST", "ALBUM ARTIST", "ARTIST"]),
        ("DATE", ItemKey::Year, vec!["DATE", "YEAR"]),
        ("GENRE", ItemKey::Genre, vec!["GENRE"]),
        ("CATALOGNUMBER", ItemKey::CatalogNumber, vec!["CATALOGNUMBER", "DISCOGS_CATALOG"]),
        ("PUBLISHER", ItemKey::Unknown("PUBLISHER".to_string()), vec!["PUBLISHER", "LABEL"]),
        ("MUSICBRAINZ_ALBUMID", ItemKey::MusicBrainzReleaseId, vec!["MUSICBRAINZ_ALBUMID"]),
        ("MUSICBRAINZ_ALBUMARTISTID", ItemKey::MusicBrainzReleaseArtistId, vec!["MUSICBRAINZ_ALBUMARTISTID"]),
        ("MUSICBRAINZ_RELEASEGROUPID", ItemKey::MusicBrainzReleaseGroupId, vec!["MUSICBRAINZ_RELEASEGROUPID"]),
        ("ORIGINALDATE", ItemKey::OriginalReleaseDate, vec!["ORIGINALDATE"]),
        ("RELEASECOUNTRY", ItemKey::Unknown("RELEASECOUNTRY".to_string()), vec!["RELEASECOUNTRY"]),
    ] {
        if let Some(v) = super::dvda_metabase::album_value_for_track_ids(metabase, &group_track_ids, &aliases) {
            push_album(&mut entries, display_key, item_key, v);
        }
    }

    let track_numbers: Vec<String> = track_addrs
        .iter()
        .enumerate()
        .map(|(i, a)| {
            super::dvda_metabase::track_value(metabase, &a.id, &["TRACKNUMBER"])
                .unwrap_or_else(|| (i + 1).to_string())
        })
        .collect();
    push_per_track(&mut entries, "TRACKNUMBER", ItemKey::TrackNumber, track_numbers);

    for (display_key, item_key) in [
        ("TITLE", ItemKey::TrackTitle),
        ("ARTIST", ItemKey::TrackArtist),
        ("PERFORMER", ItemKey::Performer),
        ("COMPOSER", ItemKey::Composer),
        ("LYRICIST", ItemKey::Lyricist),
        ("ARRANGER", ItemKey::Arranger),
        ("ISRC", ItemKey::Isrc),
        ("MUSICBRAINZ_TRACKID", ItemKey::MusicBrainzRecordingId),
        ("MUSICBRAINZ_RELEASETRACKID", ItemKey::MusicBrainzTrackId),
        ("MUSICBRAINZ_ARTISTID", ItemKey::MusicBrainzArtistId),
    ] {
        let values: Vec<String> = track_addrs
            .iter()
            .map(|a| super::dvda_metabase::track_value(metabase, &a.id, &[display_key]).unwrap_or_default())
            .collect();
        push_per_track(&mut entries, display_key, item_key, values);
    }

    let file_labels: Vec<String> = track_addrs
        .iter()
        .enumerate()
        .map(|(i, a)| {
            super::dvda_metabase::track_value(metabase, &a.id, &["TRACKNUMBER"])
                .unwrap_or_else(|| format!("{:>02}", i + 1))
        })
        .collect();

    let (writable, target_path) = match existing_metabase_path {
        Some(p) => {
            let w = is_path_writable(p);
            (w, if w { Some(p.clone()) } else { None })
        }
        None => {
            let candidate = super::dvda_metabase::expected_sidecar_path_for_source(source_path, store_id);
            let parent_writable = candidate
                .as_ref()
                .and_then(|p| p.parent())
                .map(is_dir_writable)
                .unwrap_or(false);
            (parent_writable, if parent_writable { candidate } else { None })
        }
    };

    Ok((
        super::app::MetadataEditorState {
            paths,
            entries,
            cursor: 0,
            scroll: 0,
            last_click: None,
            edit_input: None,
            add_key_input: None,
            phase: super::app::MetadataEditorPhase::Editing,
            dirty: false,
            deleted: Vec::new(),
            file_labels,
            detail_field_idx: 0,
            detail_cursor: 0,
            detail_scroll: 0,
            detail_edit: None,
            mb_back: None,
            gnudb_back: None,
            read_only: !writable,
            sacd_sidecar_path: target_path,
            sacd_area_kind: None,
            sacd_stereo_durations: None,
            sacd_multi_channel_durations: None,
            presentation_tabs: Vec::new(),
            active_tab: 0,
        },
        dvda_presentation_label_for_group(disc, disc_contents, group.group_nr),
        n_tracks,
    ))
}

pub fn build_dvda_multitab_editor_state(
    source_path: &std::path::Path,
    disc: &crate::tui::dvda::DvdaDisc,
    disc_contents: Option<&crate::disc::model::DiscContents>,
    store_id: &str,
    existing_metabase_path: Option<&std::path::PathBuf>,
    metabase: Option<&super::dvda_metabase::DvdaMetabase>,
    selected_group_nr: Option<u8>,
) -> Result<(super::app::MetadataEditorState, String, usize), String> {
    let groups = super::dvda_metabase::available_groups(disc);
    // `available_groups` supplies metabase track ids and validates non-empty groups.
    // The user-visible tab source and labels come from DiscContents.presentations
    // whenever that unified disc model is available.
    let tab_specs = dvda_presentation_tab_specs(disc, disc_contents, &groups);

    if tab_specs.len() <= 1 {
        return build_dvda_editor_state(
            source_path,
            disc,
            disc_contents,
            store_id,
            existing_metabase_path,
            metabase,
            selected_group_nr.or_else(|| tab_specs.first().map(|spec| spec.group_nr)),
        );
    }

    let requested_group_nr = selected_group_nr
        .or_else(|| super::dvda_metabase::select_group(disc, None).ok().map(|group| group.group_nr));
    let default_group_nr = requested_group_nr
        .filter(|group_nr| tab_specs.iter().any(|spec| spec.group_nr == *group_nr))
        .unwrap_or(tab_specs[0].group_nr);
    let mut tabs: Vec<super::app::PresentationTab> = Vec::new();
    let mut states: Vec<(super::app::MetadataEditorState, String, usize, u8)> = Vec::new();

    for spec in &tab_specs {
        let (state, _label, n_tracks) = build_dvda_editor_state(
            source_path,
            disc,
            disc_contents,
            store_id,
            existing_metabase_path,
            metabase,
            Some(spec.group_nr),
        )?;
        tabs.push(super::app::PresentationTab {
            id: crate::disc::model::PresentationId::DvdAudioGroup(spec.group_nr),
            label: spec.label.clone(),
            paths: state.paths.clone(),
            entries: state.entries.clone(),
            file_labels: state.file_labels.clone(),
            deleted: state.deleted.clone(),
            dirty: state.dirty,
            sacd_area_kind: None,
            sacd_stereo_durations: None,
            sacd_multi_channel_durations: None,
        });
        states.push((state, spec.label.clone(), n_tracks, spec.group_nr));
    }

    let active_idx = states
        .iter()
        .position(|(_, _, _, group_nr)| *group_nr == default_group_nr)
        .unwrap_or(0);
    let (mut state, label, n_tracks, _) = states.remove(active_idx);
    state.presentation_tabs = tabs;
    state.active_tab = active_idx;
    if let Some(tab) = state.presentation_tabs.get(active_idx).cloned() {
        state.paths = tab.paths;
        state.entries = tab.entries;
        state.file_labels = tab.file_labels;
        state.deleted = tab.deleted;
        state.dirty = tab.dirty;
    }
    Ok((state, label, n_tracks))
}

fn find_single_dvda_source_in_dir(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let read = std::fs::read_dir(dir).ok()?;
    let mut found: Option<std::path::PathBuf> = None;
    for entry in read.flatten() {
        let path = entry.path();
        if !crate::disc::dvda_utils::is_dvda_source(&path) {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(path);
    }
    found
}

pub(super) fn dvda_group_from_editor_state(state: &super::app::MetadataEditorState) -> Option<u8> {
    state
        .entries
        .iter()
        .find(|entry| entry.display_key.eq_ignore_ascii_case("DVDA_GROUP"))
        .and_then(|entry| {
            entry
                .per_file_values
                .first()
                .filter(|value| !value.trim().is_empty())
                .or_else(|| (!entry.value.trim().is_empty()).then_some(&entry.value))
        })
        .and_then(|value| value.trim().parse::<u8>().ok())
}

pub(super) fn metadata_editor_is_dvda_source(state: &super::app::MetadataEditorState) -> bool {
    if dvda_group_from_editor_state(state).is_none() {
        return false;
    }

    let Some(first_path) = state.paths.first() else {
        return false;
    };
    if !crate::disc::dvda_utils::is_dvda_source(first_path) {
        return false;
    }

    state.paths.iter().all(|p| p == first_path)
}

// TODO: DVD-Video metadata editor — full implementation pending
pub(super) fn metadata_editor_is_dvdv_source(_state: &super::app::MetadataEditorState) -> bool {
    false
}

pub(super) fn open_metadata_editor_for_dvdv(_app: &mut super::app::AppState, _source_path: std::path::PathBuf) {
    // Stub — DVD-Video metadata editor not yet implemented
}

pub fn save_dvda_metabase(
    state: &super::app::MetadataEditorState,
    metabase_path: &std::path::Path,
) -> Result<SacdSaveKind, String> {
    let source_path = state
        .paths
        .first()
        .ok_or_else(|| "editor has no DVD-Audio source path".to_string())?
        .clone();
    let (disc, store_id, _existing_path, _existing_metabase, _parse_note) =
        load_dvda_metabase_context(&source_path)?;
    let selected_group_nr = dvda_group_from_editor_state(state);
    let group = super::dvda_metabase::select_group(&disc, selected_group_nr)
        .map_err(|e| e.to_string())?;
    let track_ids = super::dvda_metabase::group_track_ids(&disc, group);
    let seeded = super::dvda_metabase::seed_from_disc(&disc, &store_id);

    save_dvda_metabase_with_loaded_context(state, metabase_path, &seeded, track_ids, |group_nr| {
        let group = super::dvda_metabase::select_group(&disc, Some(group_nr))
            .map_err(|e| e.to_string())?;
        Ok(super::dvda_metabase::group_track_ids(&disc, group))
    })
}

fn save_dvda_metabase_with_loaded_context<F>(
    state: &super::app::MetadataEditorState,
    metabase_path: &std::path::Path,
    seeded: &super::dvda_metabase::DvdaMetabase,
    selected_track_ids: Vec<String>,
    mut track_ids_for_group: F,
) -> Result<SacdSaveKind, String>
where
    F: FnMut(u8) -> Result<Vec<String>, String>,
{
    if selected_track_ids.len() != state.paths.len() {
        return Err(format!(
            "DVD-Audio group has {} track(s) but editor has {}; refusing to map",
            selected_track_ids.len(),
            state.paths.len(),
        ));
    }

    let kind = if metabase_path.exists() {
        SacdSaveKind::Updated
    } else {
        SacdSaveKind::Created
    };
    let mut metabase = if metabase_path.exists() {
        super::dvda_metabase::parse_metabase(metabase_path)
            .map_err(|e| format!("re-read metabase: {}", e))?
    } else {
        seeded.clone()
    };

    if !state.presentation_tabs.is_empty() {
        apply_dvda_presentation_tabs_to_metabase(
            &mut metabase,
            seeded,
            &state.presentation_tabs,
            |group_nr| track_ids_for_group(group_nr),
        )?;
        super::dvda_metabase::write_metabase(&metabase, metabase_path)
            .map_err(|e| format!("write: {}", e))?;
        return Ok(kind);
    }

    ensure_dvda_tracks_present(&mut metabase, seeded, &selected_track_ids);
    apply_dvda_entries_to_metabase(&mut metabase, &selected_track_ids, &state.entries, &state.deleted)?;

    super::dvda_metabase::write_metabase(&metabase, metabase_path)
        .map_err(|e| format!("write: {}", e))?;
    Ok(kind)
}

fn ensure_dvda_tracks_present(
    metabase: &mut super::dvda_metabase::DvdaMetabase,
    seeded: &super::dvda_metabase::DvdaMetabase,
    track_ids: &[String],
) {
    for id in track_ids {
        if super::dvda_metabase::track(metabase, id).is_none() {
            if let Some(track) = seeded.tracks.iter().find(|t| &t.id == id) {
                metabase.tracks.push(track.clone());
            }
        }
    }
}

fn apply_dvda_presentation_tabs_to_metabase<F>(
    metabase: &mut super::dvda_metabase::DvdaMetabase,
    seeded: &super::dvda_metabase::DvdaMetabase,
    tabs: &[super::app::PresentationTab],
    mut track_ids_for_group: F,
) -> Result<(), String>
where
    F: FnMut(u8) -> Result<Vec<String>, String>,
{
    for tab in tabs {
        let group_nr = match &tab.id {
            crate::disc::model::PresentationId::DvdAudioGroup(group_nr) => *group_nr,
            _ => continue,
        };
        let tab_track_ids = track_ids_for_group(group_nr)?;
        if tab_track_ids.len() != tab.paths.len() {
            return Err(format!(
                "DVD-Audio group {} has {} track(s) but editor tab has {}; refusing to map",
                group_nr,
                tab_track_ids.len(),
                tab.paths.len(),
            ));
        }
        ensure_dvda_tracks_present(metabase, seeded, &tab_track_ids);
        apply_dvda_entries_to_metabase(metabase, &tab_track_ids, &tab.entries, &tab.deleted)?;
    }
    Ok(())
}

fn apply_dvda_entries_to_metabase(
    metabase: &mut super::dvda_metabase::DvdaMetabase,
    track_ids: &[String],
    entries: &[super::probe::TagEntry],
    deleted: &[usize],
) -> Result<(), String> {
    for (entry_idx, entry) in entries.iter().enumerate() {
        let entry_deleted = deleted.contains(&entry_idx);
        let Some(sidecar_key) = editor_key_to_sidecar_key(&entry.display_key) else {
            continue;
        };
        if is_album_level_sidecar_key(sidecar_key) {
            if entry.is_mixed && !entry_deleted {
                return Err(format!(
                    "album-level field {} has mixed values; cannot save",
                    entry.display_key
                ));
            }
            let new_val = if entry_deleted { String::new() } else { entry.value.clone() };
            for id in track_ids {
                if let Some(track) = super::dvda_metabase::track_mut(metabase, id) {
                    if new_val.is_empty() {
                        track.meta.remove(sidecar_key);
                    } else {
                        track.meta.insert(sidecar_key.to_string(), new_val.clone());
                    }
                }
            }
        } else {
            for (i, id) in track_ids.iter().enumerate() {
                let new_val = if entry_deleted {
                    String::new()
                } else {
                    entry.per_file_values.get(i).cloned().unwrap_or_default()
                };
                if let Some(track) = super::dvda_metabase::track_mut(metabase, id) {
                    if new_val.is_empty() {
                        track.meta.remove(sidecar_key);
                    } else {
                        track.meta.insert(sidecar_key.to_string(), new_val);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Rebuild the DVD-Audio metadata editor for a user-selected group.
pub(super) fn switch_dvda_editor_group(
    state: &mut super::app::MetadataEditorState,
    source_path: &std::path::Path,
    group_nr: u8,
) -> Result<String, String> {
    if let Some(idx) = state.presentation_tabs.iter().position(|tab| {
        matches!(&tab.id, crate::disc::model::PresentationId::DvdAudioGroup(n) if *n == group_nr)
    }) {
        state.switch_presentation_tab(idx);
        let label = state.active_presentation_label().unwrap_or("DVD-Audio group").to_string();
        return Ok(format!("{} ({} tracks)", label, state.paths.len()));
    }

    let (disc, store_id, metabase_path, metabase, _parse_note) =
        load_dvda_metabase_context(source_path)?;
    let disc_contents = crate::disc::dvda_utils::map_dvda_source(source_path).ok();
    let (mut new_state, group_label, n_tracks) = build_dvda_multitab_editor_state(
        source_path,
        &disc,
        disc_contents.as_ref(),
        &store_id,
        metabase_path.as_ref(),
        metabase.as_ref(),
        Some(group_nr),
    )?;
    new_state.cursor = state.cursor.min(new_state.entries.len().saturating_sub(1));
    new_state.scroll = 0;
    *state = new_state;
    Ok(format!("{} ({} tracks)", group_label, n_tracks))
}

/// Open the metadata editor against a SACD ISO. Parses the ISO,
/// discovers + reads the sidecar `.xml` if present (sacd-extract
/// metabase format), hands both to `build_sacd_editor_state` to
/// construct the editor state, and installs it on the app. Sidecar
/// data wins on every field it provides; ScarletBook fills gaps.
pub(super) fn open_metadata_editor_for_sacd(app: &mut AppState, iso_path: std::path::PathBuf) {
    open_metadata_editor_for_sacd_at_track(app, iso_path, None);
}

fn open_metadata_editor_for_sacd_at_track(
    app: &mut AppState,
    iso_path: std::path::PathBuf,
    initial_track: Option<usize>,
) {
    let md = match super::sacd::parse_sacd_iso(&iso_path) {
        Ok(m) => m,
        Err(e) => {
            app.set_status(format!("Failed to parse SACD: {}", e));
            return;
        }
    };

    // Sidecar discovery: same-stem rule (`disc.iso` ↔ `disc.xml`).
    // Parse failures don't abort the open — we fall back to
    // ScarletBook only and remember the failure so the final status
    // message can surface it (rather than getting stomped by the
    // success status).
    let sidecar_path = super::sacd_sidecar::find_sidecar_for_iso(&iso_path);
    let mut sidecar_parse_error: Option<String> = None;
    let sidecar = sidecar_path
        .as_ref()
        .and_then(|p| match super::sacd_sidecar::parse_sidecar(p) {
            Ok(s) => Some(s),
            Err(e) => {
                sidecar_parse_error = Some(format!("{}", e));
                log::warn!("SACD sidecar parse failed for '{}': {}", p.display(), e,);
                None
            }
        });

    match build_sacd_multitab_editor_state(&iso_path, &md, sidecar.as_ref()) {
        Ok((mut state, area_label, n_tracks)) => {
            if let Some(track_index) = initial_track {
                focus_metadata_editor_on_track(&mut state, track_index);
            }
            let src = if sidecar.is_some() {
                "sidecar+ScarletBook"
            } else if sidecar_parse_error.is_some() {
                "ScarletBook (sidecar malformed)"
            } else {
                "ScarletBook"
            };
            app.set_status(format!(
                "SACD editor opened ({}, {} tracks, {}) — read-only",
                area_label, n_tracks, src,
            ));
            app.active_overlay = super::app::ActiveOverlay::MetadataEditor(Box::new(state));
        }
        Err(msg) => app.set_status(msg),
    }
}

/// Pure builder: turn parsed SACD metadata into a `MetadataEditorState`.
/// Stereo area is preferred when both are present; multi-channel is
/// the fallback. Sidecar (sacd-extract metabase XML, when present)
/// wins on every field it provides; ScarletBook (in-ISO TOC + text)
/// fills gaps only where the sidecar is silent.
///
/// The resulting editor is currently always `read_only = true`. C5c
/// flips this when the sidecar (or its parent dir) is writable, so
/// edits can land as XML sidecar writes.
///
/// Returns (state, area_label, n_tracks) on success so the caller
/// can compose a status message; returns Err with a short reason
/// string when no readable area exists or the area is empty.
pub fn build_sacd_editor_state(
    iso_path: &std::path::Path,
    md: &super::sacd::SacdMetadata,
    sidecar: Option<&super::sacd_sidecar::SidecarMetadata>,
) -> Result<(super::app::MetadataEditorState, &'static str, usize), String> {
    use super::probe::TagEntry;
    use lofty::tag::ItemKey;

    let area = md
        .stereo
        .as_ref()
        .or(md.multi_channel.as_ref())
        .ok_or_else(|| "SACD: no readable area".to_string())?;

    let n_tracks = area.tracks.len();
    if n_tracks == 0 {
        return Err("SACD area has zero tracks".to_string());
    }

    let area_label = match area.header.kind {
        super::sacd::AreaKind::Stereo => "stereo",
        super::sacd::AreaKind::MultiChannel => "MCH",
    };
    let sidecar_area_idx: u8 = match area.header.kind {
        super::sacd::AreaKind::Stereo => 1,
        super::sacd::AreaKind::MultiChannel => 2,
    };

    // Sidecar tracks for the selected area, sorted by TRACKNUMBER.
    // Length may differ from n_tracks (a sidecar from a partial rip
    // or a different area-count assumption could mismatch); we
    // index defensively below.
    let sidecar_tracks: Vec<&super::sacd_sidecar::SidecarTrack> = sidecar
        .map(|s| s.tracks_for_area(sidecar_area_idx))
        .unwrap_or_default();

    // Pull a per-track sidecar value by index, falling back to a
    // closure-supplied ScarletBook value when the sidecar lacks it.
    // Empty-string sidecar values are treated as "not provided" so
    // they don't override richer ScarletBook data.
    let resolve_per_track = |key: &str, fallback: &dyn Fn(usize) -> String| -> Vec<String> {
        (0..n_tracks)
            .map(|i| {
                let from_sidecar = sidecar_tracks
                    .get(i)
                    .and_then(|t| t.meta.get(key))
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                from_sidecar.unwrap_or_else(|| fallback(i))
            })
            .collect()
    };

    // Pick the first **non-empty** sidecar value for an album-level
    // key. The filter must live INSIDE find_map so we don't lock in
    // an early empty value and miss a later non-empty one (e.g., a
    // partial-rip sidecar where the first track lost a tag).
    let sidecar_album_value = |key: &str| -> Option<String> {
        sidecar_tracks
            .iter()
            .find_map(|t| t.meta.get(key).filter(|s| !s.trim().is_empty()))
            .map(|s| s.trim().to_string())
    };

    // The editor models per-track values via paths: Vec<PathBuf>. For
    // an SACD we have one file but many virtual tracks; repeat the ISO
    // path N times so per-track indexing works. read_only blocks any
    // write attempt that would otherwise hit the same file N times.
    let paths = vec![iso_path.to_path_buf(); n_tracks];

    let mut entries: Vec<TagEntry> = Vec::new();

    // Album-level (single value replicated across all tracks).
    let push_album =
        |entries: &mut Vec<TagEntry>, display_key: &str, item_key: ItemKey, value: String| {
            let vals = vec![value.clone(); n_tracks];
            // Sidecar (or ScarletBook fallback) is the source of truth
            // for the displayed value. Mirror it into `original` so the
            // row-level revert pill restores to what the user originally
            // saw, not to an empty string. (Previously `original:
            // String::new()` made row-level revert show a blank cell.)
            entries.push(TagEntry {
                display_key: display_key.to_string(),
                item_key,
                value: value.clone(),
                original: value,
                is_binary: false,
                is_mixed: false,
                per_file_values: vals.clone(),
                per_file_originals: vals,
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            });
        };

    // ALBUM: sidecar wins, fallback to master_text.album_title.
    let album = sidecar_album_value("ALBUM")
        .or_else(|| md.master_text.as_ref().and_then(|t| t.album_title.clone()));
    if let Some(s) = album {
        push_album(&mut entries, "ALBUM", ItemKey::AlbumTitle, s);
    }
    // ALBUMARTIST: sidecar key may be "ALBUMARTIST" or "ALBUM ARTIST";
    // fall back to ScarletBook album_artist.
    let album_artist = sidecar_album_value("ALBUMARTIST")
        .or_else(|| sidecar_album_value("ALBUM ARTIST"))
        .or_else(|| md.master_text.as_ref().and_then(|t| t.album_artist.clone()));
    if let Some(s) = album_artist {
        push_album(&mut entries, "ALBUMARTIST", ItemKey::AlbumArtist, s);
    }
    // DATE: sidecar wins, fallback to disc_date.year.
    let date =
        sidecar_album_value("DATE").or_else(|| md.master_toc.disc_date.map(|d| d.year.to_string()));
    if let Some(s) = date {
        push_album(&mut entries, "DATE", ItemKey::Year, s);
    }
    // CATALOGNUMBER: sidecar key may be "CATALOGNUMBER" or
    // "DISCOGS_CATALOG"; fallback to ScarletBook album_catalog_number.
    let catalog = sidecar_album_value("CATALOGNUMBER")
        .or_else(|| sidecar_album_value("DISCOGS_CATALOG"))
        .or_else(|| {
            let c = md.master_toc.album_catalog_number.trim().to_string();
            if c.is_empty() {
                None
            } else {
                Some(c)
            }
        });
    if let Some(s) = catalog {
        push_album(&mut entries, "CATALOGNUMBER", ItemKey::CatalogNumber, s);
    }
    // GENRE: sidecar wins; fallback to first non-zero ScarletBook genre.
    let genre = sidecar_album_value("GENRE").or_else(|| {
        md.master_toc
            .disc_genres
            .first()
            .or_else(|| md.master_toc.album_genres.first())
            .map(|g| g.name())
            .filter(|n| *n != "Not used" && *n != "Not defined")
            .map(|s| s.to_string())
    });
    if let Some(s) = genre {
        push_album(&mut entries, "GENRE", ItemKey::Genre, s);
    }
    // PUBLISHER: sidecar wins, with fallback to the ScarletBook
    // SACDText album_publisher field (which is rarely populated on
    // real discs but exists in the spec).
    let publisher = sidecar_album_value("PUBLISHER").or_else(|| {
        md.master_text
            .as_ref()
            .and_then(|t| t.album_publisher.clone())
    });
    if let Some(s) = publisher {
        push_album(&mut entries, "PUBLISHER", ItemKey::Publisher, s);
    }

    // Per-track entries.
    let push_per_track =
        |entries: &mut Vec<TagEntry>, display_key: &str, item_key: ItemKey, values: Vec<String>| {
            if values.iter().all(|s| s.is_empty()) {
                return;
            }
            let all_same = values.windows(2).all(|w| w[0] == w[1]);
            let value = if all_same {
                values.first().cloned().unwrap_or_default()
            } else {
                "<multiple values>".to_string()
            };
            // Mirror the displayed value into `original` so the row-level
            // revert pill restores the sidecar/ScarletBook truth instead
            // of an empty string. For mixed entries `value` is
            // `<multiple values>`; storing that as `original` matches
            // how `grow_or_create_per_track` handles the same case.
            entries.push(TagEntry {
                display_key: display_key.to_string(),
                item_key,
                value: value.clone(),
                original: value,
                is_binary: false,
                is_mixed: !all_same,
                per_file_originals: values.clone(),
                per_file_values: values,
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            });
        };

    // TRACKNUMBER: prefer sidecar's recorded TRACKNUMBER (may be
    // zero-padded like "01"); fallback to 1..N enumeration.
    let track_numbers: Vec<String> = resolve_per_track("TRACKNUMBER", &|i| (i + 1).to_string());
    push_per_track(
        &mut entries,
        "TRACKNUMBER",
        ItemKey::TrackNumber,
        track_numbers,
    );

    // TITLE: sidecar wins, fallback to SACDTTxt.title.
    let titles: Vec<String> = resolve_per_track("TITLE", &|i| {
        area.tracks
            .get(i)
            .and_then(|t| t.text.title.clone())
            .unwrap_or_default()
    });
    push_per_track(&mut entries, "TITLE", ItemKey::TrackTitle, titles);

    // ARTIST: sidecar wins (key "ARTIST"); fallback to SACDTTxt.performer.
    let performers: Vec<String> = resolve_per_track("ARTIST", &|i| {
        area.tracks
            .get(i)
            .and_then(|t| t.text.performer.clone())
            .unwrap_or_default()
    });
    push_per_track(&mut entries, "ARTIST", ItemKey::TrackArtist, performers);

    // PERFORMER: sidecar-only secondary field (some metabases store
    // soloist as PERFORMER alongside the band-level ARTIST).
    let perf_secondary: Vec<String> = (0..n_tracks)
        .map(|i| {
            sidecar_tracks
                .get(i)
                .and_then(|t| t.meta.get("PERFORMER"))
                .map(|s| s.trim().to_string())
                .unwrap_or_default()
        })
        .collect();
    push_per_track(
        &mut entries,
        "PERFORMER",
        ItemKey::Performer,
        perf_secondary,
    );

    let composers: Vec<String> = resolve_per_track("COMPOSER", &|i| {
        area.tracks
            .get(i)
            .and_then(|t| t.text.composer.clone())
            .unwrap_or_default()
    });
    push_per_track(&mut entries, "COMPOSER", ItemKey::Composer, composers);

    let songwriters: Vec<String> = resolve_per_track("LYRICIST", &|i| {
        area.tracks
            .get(i)
            .and_then(|t| t.text.songwriter.clone())
            .unwrap_or_default()
    });
    push_per_track(&mut entries, "LYRICIST", ItemKey::Lyricist, songwriters);

    let arrangers: Vec<String> = resolve_per_track("ARRANGER", &|i| {
        area.tracks
            .get(i)
            .and_then(|t| t.text.arranger.clone())
            .unwrap_or_default()
    });
    push_per_track(&mut entries, "ARRANGER", ItemKey::Arranger, arrangers);

    let isrcs: Vec<String> = resolve_per_track("ISRC", &|i| {
        area.tracks
            .get(i)
            .and_then(|t| t.isrc.clone())
            .unwrap_or_default()
    });
    push_per_track(&mut entries, "ISRC", ItemKey::Isrc, isrcs);

    // Phase C-1: MusicBrainz per-track identifiers. ScarletBook
    // metadata has no MB equivalent, so the fallback is always
    // empty — push_per_track suppresses the entry when every slot
    // is empty, matching the existing "don't show empty rows"
    // policy. populate_editor_from_mb creates these rows on demand
    // when the user later runs `:tags-mb`.
    let mb_track_ids: Vec<String> = resolve_per_track("MUSICBRAINZ_TRACKID", &|_| String::new());
    push_per_track(
        &mut entries,
        "MUSICBRAINZ_TRACKID",
        ItemKey::MusicBrainzRecordingId,
        mb_track_ids,
    );
    let mb_release_track_ids: Vec<String> =
        resolve_per_track("MUSICBRAINZ_RELEASETRACKID", &|_| String::new());
    push_per_track(
        &mut entries,
        "MUSICBRAINZ_RELEASETRACKID",
        ItemKey::MusicBrainzTrackId,
        mb_release_track_ids,
    );
    let mb_artist_ids: Vec<String> = resolve_per_track("MUSICBRAINZ_ARTISTID", &|_| String::new());
    push_per_track(
        &mut entries,
        "MUSICBRAINZ_ARTISTID",
        ItemKey::MusicBrainzArtistId,
        mb_artist_ids,
    );

    // Phase C-1: MusicBrainz album-level identifiers + supplemental
    // fields. Each is replicated across every track (matching the
    // existing ALBUM/ALBUMARTIST/etc. pattern) so save can write
    // back uniformly through the album-level branch of
    // save_sacd_sidecar.
    if let Some(s) = sidecar_album_value("MUSICBRAINZ_ALBUMID") {
        push_album(
            &mut entries,
            "MUSICBRAINZ_ALBUMID",
            ItemKey::MusicBrainzReleaseId,
            s,
        );
    }
    if let Some(s) = sidecar_album_value("MUSICBRAINZ_ALBUMARTISTID") {
        push_album(
            &mut entries,
            "MUSICBRAINZ_ALBUMARTISTID",
            ItemKey::MusicBrainzReleaseArtistId,
            s,
        );
    }
    if let Some(s) = sidecar_album_value("MUSICBRAINZ_RELEASEGROUPID") {
        push_album(
            &mut entries,
            "MUSICBRAINZ_RELEASEGROUPID",
            ItemKey::MusicBrainzReleaseGroupId,
            s,
        );
    }
    if let Some(s) = sidecar_album_value("ORIGINALDATE") {
        push_album(
            &mut entries,
            "ORIGINALDATE",
            ItemKey::OriginalReleaseDate,
            s,
        );
    }
    if let Some(s) = sidecar_album_value("RELEASECOUNTRY") {
        push_album(
            &mut entries,
            "RELEASECOUNTRY",
            ItemKey::Unknown("RELEASECOUNTRY".to_string()),
            s,
        );
    }

    let file_labels: Vec<String> = (1..=n_tracks).map(|i| format!("{:>02}", i)).collect();

    // Writability: editor unlocks for save when either
    //   (a) a sidecar already exists at the same-stem path AND is
    //       writable (existing-update path), or
    //   (b) no sidecar exists yet but the parent directory is
    //       writable (mint-on-save path — `save_sacd_sidecar`
    //       seeds from ScarletBook, mints `<store id>` via
    //       `mint_disc_id`, and atomic-writes a fresh `.xml`).
    // `sacd_sidecar_path` is set to the expected target in both
    // cases so the save path knows where to write.
    let (writable, sidecar_target) = match super::sacd_sidecar::find_sidecar_for_iso(iso_path) {
        Some(p) => {
            let w = is_path_writable(&p);
            (w, if w { Some(p) } else { None })
        }
        None => {
            let candidate = super::sacd_sidecar::expected_sidecar_path_for_iso(iso_path);
            let parent_writable = candidate
                .as_ref()
                .and_then(|p| p.parent())
                .map(is_dir_writable)
                .unwrap_or(false);
            (
                parent_writable,
                if parent_writable { candidate } else { None },
            )
        }
    };
    let sidecar_path_opt = sidecar_target;

    // Per-area track durations, stashed for `:tags-mb` TOC synthesis
    // (C-2a). Both areas captured — even though the editor only
    // surfaces one — so the sibling-mirror flow (future C-2c) has
    // them ready without re-reading the ISO. `None` for an absent
    // area, or for one whose TRL1/TRL2 sectors failed to parse
    // (leaving `tracks` empty per `sacd.rs` semantics).
    let area_durations = |a: &super::sacd::AreaInfo| -> Option<Vec<f64>> {
        if a.tracks.is_empty() {
            None
        } else {
            Some(
                a.tracks
                    .iter()
                    .map(|t| t.duration.total_seconds())
                    .collect(),
            )
        }
    };
    let sacd_stereo_durations = md.stereo.as_ref().and_then(&area_durations);
    let sacd_multi_channel_durations = md.multi_channel.as_ref().and_then(&area_durations);

    let state = super::app::MetadataEditorState {
        paths,
        entries,
        cursor: 0,
        scroll: 0,
        last_click: None,
        edit_input: None,
        add_key_input: None,
        phase: super::app::MetadataEditorPhase::Editing,
        dirty: false,
        deleted: Vec::new(),
        file_labels,
        detail_field_idx: 0,
        detail_cursor: 0,
        detail_scroll: 0,
        detail_edit: None,
        mb_back: None,
        gnudb_back: None,
        read_only: !writable,
        sacd_sidecar_path: if writable { sidecar_path_opt } else { None },
        sacd_area_kind: Some(area.header.kind),
        sacd_stereo_durations,
        sacd_multi_channel_durations,
        presentation_tabs: Vec::new(),
        active_tab: 0,
    };

    Ok((state, area_label, n_tracks))
}

pub fn build_sacd_multitab_editor_state(
    iso_path: &std::path::Path,
    md: &super::sacd::SacdMetadata,
    sidecar: Option<&super::sacd_sidecar::SidecarMetadata>,
) -> Result<(super::app::MetadataEditorState, &'static str, usize), String> {
    let mut area_views: Vec<(super::sacd::AreaKind, super::sacd::SacdMetadata)> = Vec::new();
    if md.stereo.is_some() {
        let mut view = md.clone();
        view.multi_channel = None;
        area_views.push((super::sacd::AreaKind::Stereo, view));
    }
    if md.multi_channel.is_some() {
        let mut view = md.clone();
        view.stereo = None;
        area_views.push((super::sacd::AreaKind::MultiChannel, view));
    }

    if area_views.len() <= 1 {
        return build_sacd_editor_state(iso_path, md, sidecar);
    }

    let mut tabs: Vec<super::app::PresentationTab> = Vec::new();
    let mut states: Vec<(super::app::MetadataEditorState, &'static str, usize)> = Vec::new();

    for (kind, view) in area_views {
        let (state, label, n_tracks) = build_sacd_editor_state(iso_path, &view, sidecar)?;
        let (id, tab_label) = match kind {
            super::sacd::AreaKind::Stereo => (
                crate::disc::model::PresentationId::SacdArea(
                    crate::disc::model::SacdAreaId::Stereo,
                ),
                "Stereo".to_string(),
            ),
            super::sacd::AreaKind::MultiChannel => (
                crate::disc::model::PresentationId::SacdArea(
                    crate::disc::model::SacdAreaId::MultiChannel,
                ),
                "Multichannel".to_string(),
            ),
        };
        tabs.push(super::app::PresentationTab {
            id,
            label: tab_label,
            paths: state.paths.clone(),
            entries: state.entries.clone(),
            file_labels: state.file_labels.clone(),
            deleted: state.deleted.clone(),
            dirty: state.dirty,
            sacd_area_kind: state.sacd_area_kind.clone(),
            sacd_stereo_durations: state.sacd_stereo_durations.clone(),
            sacd_multi_channel_durations: state.sacd_multi_channel_durations.clone(),
        });
        states.push((state, label, n_tracks));
    }

    let (mut state, label, n_tracks) = states.remove(0);
    state.presentation_tabs = tabs;
    state.active_tab = 0;
    if let Some(tab) = state.presentation_tabs.first().cloned() {
        state.paths = tab.paths;
        state.entries = tab.entries;
        state.file_labels = tab.file_labels;
        state.deleted = tab.deleted;
        state.dirty = tab.dirty;
        state.sacd_area_kind = tab.sacd_area_kind;
        state.sacd_stereo_durations = tab.sacd_stereo_durations;
        state.sacd_multi_channel_durations = tab.sacd_multi_channel_durations;
    }
    Ok((state, label, n_tracks))
}

/// Cheaply test whether `path` is writable: succeeds if we can open
/// the file for append. Avoids actually appending by using
/// `OpenOptions::write(true)` without truncate — `create(false)` is
/// implicit since we don't pass `create(true)`.
/// Switch the SACD metadata editor between stereo and multi-channel
/// areas. Re-parses the ISO + sidecar and rebuilds editor entries
/// for the target area, preserving cursor/scroll within bounds.
/// Returns the human-readable label of the new area on success, or
/// a status reason on failure. Refuses internally if:
///   - the editor isn't on a SACD ISO (`sacd_area_kind = None`),
///   - the editor has unsaved edits (`dirty = true`),
///   - the requested area isn't present on the disc, or
///   - the user is already on the requested area.
///
/// Callers that already pre-check dirty (e.g. the colon-dispatch
/// path) still benefit from defense-in-depth here so a future
/// mouse-pill or context-menu handler can call this safely.
pub(super) fn switch_sacd_editor_area(
    state: &mut super::app::MetadataEditorState,
    iso_path: &std::path::Path,
    target: super::command::SacdAreaTarget,
) -> Result<&'static str, String> {
    use super::command::SacdAreaTarget;
    use super::sacd::AreaKind;

    // Internal guards (defense in depth — callers may already
    // enforce these but we don't rely on it).
    if state.sacd_area_kind.is_none() {
        return Err(":area: editor is not on a SACD ISO".to_string());
    }

    if state.has_presentation_tabs() {
        let want_kind = match target {
            SacdAreaTarget::Stereo => AreaKind::Stereo,
            SacdAreaTarget::MultiChannel => AreaKind::MultiChannel,
            SacdAreaTarget::Toggle => match state.sacd_area_kind {
                Some(AreaKind::Stereo) => AreaKind::MultiChannel,
                Some(AreaKind::MultiChannel) => AreaKind::Stereo,
                None => return Err(":area toggle: editor has no area kind".to_string()),
            },
        };
        if Some(want_kind) == state.sacd_area_kind {
            let label = match want_kind {
                AreaKind::Stereo => "stereo",
                AreaKind::MultiChannel => "multi-channel",
            };
            return Err(format!(":area: already on {}", label));
        }
        let needle = match want_kind {
            AreaKind::Stereo => crate::disc::model::PresentationId::SacdArea(
                crate::disc::model::SacdAreaId::Stereo,
            ),
            AreaKind::MultiChannel => crate::disc::model::PresentationId::SacdArea(
                crate::disc::model::SacdAreaId::MultiChannel,
            ),
        };
        let Some(idx) = state.presentation_tabs.iter().position(|tab| tab.id == needle) else {
            let label = match want_kind {
                AreaKind::Stereo => "stereo",
                AreaKind::MultiChannel => "multi-channel",
            };
            return Err(format!(":area: this disc has no {} area", label));
        };
        state.switch_presentation_tab(idx);
        return Ok(match want_kind {
            AreaKind::Stereo => "stereo",
            AreaKind::MultiChannel => "MCH",
        });
    }

    if state.dirty {
        return Err(":area: editor has unsaved edits — save or discard first".to_string());
    }

    let md = super::sacd::parse_sacd_iso(iso_path)
        .map_err(|e| format!(":area: SACD parse failed: {}", e))?;

    // Resolve target → AreaKind. Toggle inverts the current area;
    // explicit kinds are honored if present.
    let want_kind = match target {
        SacdAreaTarget::Stereo => AreaKind::Stereo,
        SacdAreaTarget::MultiChannel => AreaKind::MultiChannel,
        SacdAreaTarget::Toggle => match state.sacd_area_kind {
            Some(AreaKind::Stereo) => AreaKind::MultiChannel,
            Some(AreaKind::MultiChannel) => AreaKind::Stereo,
            None => return Err(":area toggle: editor has no area kind".to_string()),
        },
    };

    // Verify the requested area exists on this disc.
    let target_present = match want_kind {
        AreaKind::Stereo => md.stereo.is_some(),
        AreaKind::MultiChannel => md.multi_channel.is_some(),
    };
    if !target_present {
        let label = match want_kind {
            AreaKind::Stereo => "stereo",
            AreaKind::MultiChannel => "multi-channel",
        };
        return Err(format!(":area: this disc has no {} area", label));
    }
    if Some(want_kind) == state.sacd_area_kind {
        let label = match want_kind {
            AreaKind::Stereo => "stereo",
            AreaKind::MultiChannel => "multi-channel",
        };
        return Err(format!(":area: already on {}", label));
    }

    // Sidecar read for the merge. Symmetric with the initial-open
    // path in open_metadata_editor_for_sacd: parse failures are
    // logged but don't abort the switch (editor rebuilds against
    // ScarletBook only).
    let sidecar_path = super::sacd_sidecar::find_sidecar_for_iso(iso_path);
    let sidecar = sidecar_path
        .as_ref()
        .and_then(|p| match super::sacd_sidecar::parse_sidecar(p) {
            Ok(s) => Some(s),
            Err(e) => {
                log::warn!(
                    "SACD sidecar parse failed during :area switch for '{}': {}",
                    p.display(),
                    e,
                );
                None
            }
        });

    // Build a synthetic SacdMetadata that forces the parser's
    // area-selection (which prefers stereo) to pick the requested
    // area. Easiest way: when the user wants MCH, hide the stereo
    // area by replacing md.stereo with None before passing to the
    // builder; vice versa for stereo.
    let mut md_view = md.clone();
    match want_kind {
        AreaKind::Stereo => md_view.multi_channel = None,
        AreaKind::MultiChannel => md_view.stereo = None,
    }

    let (new_state, area_label, _n) =
        build_sacd_editor_state(iso_path, &md_view, sidecar.as_ref())?;

    // Preserve cursor position within bounds (best-effort — entries
    // may have different counts if e.g. an MCH area has tracks the
    // stereo doesn't, but typical hybrid SACDs have matching counts).
    let prev_cursor = state.cursor;
    let prev_scroll = state.scroll;
    *state = new_state;
    state.cursor = prev_cursor.min(state.entries.len().saturating_sub(1));
    state.scroll = prev_scroll.min(state.cursor);

    Ok(area_label)
}

/// Scan an immediate directory for `.iso` files whose ScarletBook
/// magic-byte probe succeeds. Returns `Some(path)` only when exactly
/// one such ISO is found — ambiguous (0 or 2+) cases yield None and
/// let the caller fall through to whatever default behaviour applies.
fn find_single_sacd_in_dir(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let read = std::fs::read_dir(dir).ok()?;
    let mut found: Option<std::path::PathBuf> = None;
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let is_iso = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("iso"))
            .unwrap_or(false);
        if !is_iso {
            continue;
        }
        if !super::sacd::is_sacd_iso(&path) {
            continue;
        }
        if found.is_some() {
            // 2+ SACD ISOs in this directory — ambiguous selection.
            return None;
        }
        found = Some(path);
    }
    found
}

fn is_path_writable(path: &std::path::Path) -> bool {
    std::fs::OpenOptions::new().write(true).open(path).is_ok()
}

/// Probe whether a directory accepts new files. Used by the mint-on-
/// save path: when an SACD ISO has no sidecar yet, we still need to
/// know whether we'd be able to write one. `is_path_writable` is
/// file-oriented (opens the path for write) and fails for non-
/// existent files; this is the directory-oriented complement.
///
/// Implementation: try to create a uniquely-named temp file in the
/// directory, then immediately remove it. Both calls succeeding is
/// the writability signal.
pub(super) fn is_dir_writable(dir: &std::path::Path) -> bool {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let probe = dir.join(format!(".tonepoet-write-probe-{}.tmp", nanos));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Map an editor `TagEntry.display_key` to the metabase XML `<meta>`
/// `name` attribute. Returns None for keys we don't write (e.g.
/// derived/internal fields). Inverse of the merge step in
/// `build_sacd_editor_state`: ARTIST → "ARTIST", LYRICIST →
/// "LYRICIST", etc. ALBUM ARTIST normalises to the spaceless
/// canonical "ALBUMARTIST" because that's what most sidecars use.
fn editor_key_to_sidecar_key(display_key: &str) -> Option<&'static str> {
    match display_key {
        "ALBUM" => Some("ALBUM"),
        "ALBUMARTIST" => Some("ALBUMARTIST"),
        "DATE" => Some("DATE"),
        "CATALOGNUMBER" => Some("CATALOGNUMBER"),
        "GENRE" => Some("GENRE"),
        "PUBLISHER" => Some("PUBLISHER"),
        "TRACKNUMBER" => Some("TRACKNUMBER"),
        "TITLE" => Some("TITLE"),
        "ARTIST" => Some("ARTIST"),
        "PERFORMER" => Some("PERFORMER"),
        "COMPOSER" => Some("COMPOSER"),
        "LYRICIST" => Some("LYRICIST"),
        "ARRANGER" => Some("ARRANGER"),
        "ISRC" => Some("ISRC"),
        // Phase C-1: MusicBrainz identifiers + supplemental album-level
        // fields. populate_editor_from_mb writes these into TagEntry
        // rows on SACD editors today; without translation the save
        // path silently drops them.
        "MUSICBRAINZ_TRACKID" => Some("MUSICBRAINZ_TRACKID"),
        "MUSICBRAINZ_RELEASETRACKID" => Some("MUSICBRAINZ_RELEASETRACKID"),
        "MUSICBRAINZ_ARTISTID" => Some("MUSICBRAINZ_ARTISTID"),
        "MUSICBRAINZ_ALBUMID" => Some("MUSICBRAINZ_ALBUMID"),
        "MUSICBRAINZ_ALBUMARTISTID" => Some("MUSICBRAINZ_ALBUMARTISTID"),
        "MUSICBRAINZ_RELEASEGROUPID" => Some("MUSICBRAINZ_RELEASEGROUPID"),
        "ORIGINALDATE" => Some("ORIGINALDATE"),
        "RELEASECOUNTRY" => Some("RELEASECOUNTRY"),
        _ => None,
    }
}

/// Album-level keys are replicated across every track in the area's
/// sidecar block. Per-track keys land on a single track. This is
/// the inverse of the merge logic in `build_sacd_editor_state` where
/// per-track values have `per_file_values.len() == n_tracks` but
/// album-level values are also replicated to that length — both
/// dims look the same, so we tag-name-discriminate.
fn is_album_level_sidecar_key(key: &str) -> bool {
    matches!(
        key,
        "ALBUM" | "ALBUMARTIST" | "DATE" | "CATALOGNUMBER" | "GENRE" | "PUBLISHER"
        // Phase C-1: MB identifiers and supplemental album-level
        // fields. Replicating these to every track in the area
        // matches foobar2000's writer behavior for MB-populated
        // sidecars.
        | "MUSICBRAINZ_ALBUMID"
        | "MUSICBRAINZ_ALBUMARTISTID"
        | "MUSICBRAINZ_RELEASEGROUPID"
        | "ORIGINALDATE"
        | "RELEASECOUNTRY"
    )
}

/// Distinguishes a brand-new sidecar (mint-on-save path) from an
/// update to an existing one. Mirrors foobar2000's per-write event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SacdSaveKind {
    Created,
    Updated,
}

/// Phase D outcome of mirroring the surfaced area's edits to the
/// sibling (stereo ↔ multi-channel) area. `sibling_present = false`
/// for single-area SACDs — no mirror is attempted. When the sibling
/// is present, `sibling_total` is its full track count and
/// `mirrored_count` is how many tracks had a TRACKNUMBER that matched
/// an editor row. Equal counts → full mirror; unequal → bonus tracks
/// in either area weren't touched (status surfaces the partial
/// coverage).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MirrorOutcome {
    pub sibling_present: bool,
    pub sibling_total: usize,
    pub mirrored_count: usize,
}

/// Outcome of a save: kind (Created / Updated) plus Phase D mirror
/// result so the caller can compose a status message that reflects
/// whether both areas of a hybrid SACD got updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SacdSaveOutcome {
    pub kind: SacdSaveKind,
    pub mirror: MirrorOutcome,
}

/// Phase D: keys that are intentionally area-specific and must NOT
/// be mirrored across the stereo/multi-channel boundary. The audio
/// is different per area, so values like dynamic range, peak/RMS
/// measurements, etc. live per-area.
///
/// Today `DYNAMIC RANGE` and `ALBUM DYNAMIC RANGE` are foreign-
/// preserved (not in `editor_key_to_sidecar_key`'s whitelist), so
/// the mirror loop doesn't see them either way. **But future work
/// (DR analysis on SACD via sox → PCM → DR-tag writeback) will add
/// DR to the editor's write surface.** This exclusion exists so the
/// mirror semantics stay correct when that day comes: register new
/// per-area-specific keys HERE as they get widened into the editor.
///
/// Matches foobar2000's `is_linked_tag` exclusion semantics.
pub fn is_per_area_specific_key(display_key: &str) -> bool {
    matches!(
        display_key.to_ascii_uppercase().as_str(),
        "DYNAMIC RANGE" | "ALBUM DYNAMIC RANGE"
    )
}

/// Apply the editor's per-track and album-level edits to a sidecar
/// (preserving foreign keys we don't surface — DISCOGS_*, DYNAMIC
/// RANGE, replaygain, etc.) and atomic-write it. When the target
/// file already exists, the sidecar is parsed and updated in place
/// (`<store id>` preserved verbatim). When it doesn't, the ISO is
/// re-probed, an in-memory sidecar is seeded from ScarletBook, and
/// the canonical `<store id>` is minted via `mint_disc_id` — the
/// mint-on-save path.
///
/// Returns `Created` or `Updated` on success so the caller can
/// differentiate status messages. Returns Err with a short reason on
/// any failure (parse, re-probe, mint, or write). The editor state
/// isn't mutated; the caller resets `dirty` and snapshots `originals`
/// after a successful write.

fn apply_sacd_entries_to_sidecar(
    sidecar: &mut super::sacd_sidecar::SidecarMetadata,
    area_track_ids: &[u32],
    entries: &[crate::tui::probe::TagEntry],
    deleted: &[usize],
) -> Result<(), String> {
    for (entry_idx, entry) in entries.iter().enumerate() {
        let Some(sidecar_key) = editor_key_to_sidecar_key(&entry.display_key) else {
            continue;
        };
        let entry_deleted = deleted.contains(&entry_idx);
        if is_album_level_sidecar_key(sidecar_key) {
            if !entry_deleted && entry.is_mixed {
                return Err(format!(
                    "album-level field {} has mixed values; cannot save",
                    entry.display_key
                ));
            }
            let new_val = if entry_deleted {
                String::new()
            } else {
                entry.value.clone()
            };
            for tid in area_track_ids {
                if let Some(track) = sidecar.tracks.iter_mut().find(|t| t.id == *tid) {
                    if new_val.is_empty() {
                        track.meta.remove(sidecar_key);
                    } else {
                        track.meta.insert(sidecar_key.to_string(), new_val.clone());
                    }
                }
            }
        } else {
            for (i, tid) in area_track_ids.iter().enumerate() {
                let new_val = if entry_deleted {
                    String::new()
                } else {
                    entry.per_file_values.get(i).cloned().unwrap_or_default()
                };
                if let Some(track) = sidecar.tracks.iter_mut().find(|t| t.id == *tid) {
                    if new_val.is_empty() {
                        track.meta.remove(sidecar_key);
                    } else {
                        track.meta.insert(sidecar_key.to_string(), new_val);
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn save_sacd_sidecar(
    state: &super::app::MetadataEditorState,
    sidecar_path: &std::path::Path,
) -> Result<SacdSaveOutcome, String> {
    let (mut sidecar, kind) = if sidecar_path.exists() {
        let s = super::sacd_sidecar::parse_sidecar(sidecar_path)
            .map_err(|e| format!("re-read sidecar: {}", e))?;
        (s, SacdSaveKind::Updated)
    } else {
        // Mint path: no XML yet. Re-probe the ISO for ScarletBook
        // data, seed an in-memory sidecar, and mint the canonical
        // `<store id>` via MD5 of the master TOC region.
        let iso_path = state
            .paths
            .first()
            .ok_or_else(|| "editor has no ISO path for mint".to_string())?
            .clone();
        let md = super::sacd::parse_sacd_iso(&iso_path)
            .map_err(|e| format!("re-probe SACD ISO for mint: {}", e))?;
        let mut s = super::sacd_sidecar::seed_sidecar_from_scarletbook(&md);
        s.store_id = super::sacd_sidecar::mint_disc_id(&iso_path)
            .map_err(|e| format!("mint disc id: {}", e))?;
        (s, SacdSaveKind::Created)
    };

    if state.has_presentation_tabs() {
        let mut updated_areas = 0usize;
        for tab in &state.presentation_tabs {
            let area_idx = match &tab.id {
                crate::disc::model::PresentationId::SacdArea(
                    crate::disc::model::SacdAreaId::Stereo,
                ) => 1u8,
                crate::disc::model::PresentationId::SacdArea(
                    crate::disc::model::SacdAreaId::MultiChannel,
                ) => 2u8,
                _ => continue,
            };
            let area_track_ids: Vec<u32> = sidecar
                .tracks_for_area(area_idx)
                .iter()
                .map(|t| t.id)
                .collect();
            if area_track_ids.len() != tab.paths.len() {
                return Err(format!(
                    "sidecar area {} has {} track(s) but presentation '{}' has {}; refusing to map",
                    area_idx,
                    area_track_ids.len(),
                    tab.label,
                    tab.paths.len(),
                ));
            }
            apply_sacd_entries_to_sidecar(&mut sidecar, &area_track_ids, &tab.entries, &tab.deleted)?;
            updated_areas += 1;
        }
        if updated_areas == 0 {
            return Err("editor has no SACD presentation tabs".into());
        }
        super::sacd_sidecar::write_sidecar(sidecar_path, &sidecar)
            .map_err(|e| format!("write: {}", e))?;
        return Ok(SacdSaveOutcome {
            kind,
            mirror: MirrorOutcome {
                sibling_present: updated_areas > 1,
                sibling_total: 0,
                mirrored_count: 0,
            },
        });
    }

    // Legacy single-area editor path: preserve the pre-existing invisible
    // sibling-area mirror so single-tab callers remain compatible.
    let area_idx = match state.sacd_area_kind {
        Some(super::sacd::AreaKind::Stereo) => 1u8,
        Some(super::sacd::AreaKind::MultiChannel) => 2u8,
        None => return Err("editor has no SACD area kind".into()),
    };

    let area_track_ids: Vec<u32> = sidecar
        .tracks_for_area(area_idx)
        .iter()
        .map(|t| t.id)
        .collect();

    if area_track_ids.len() != state.paths.len() {
        return Err(format!(
            "sidecar area {} has {} track(s) but editor has {}; refusing to map",
            area_idx,
            area_track_ids.len(),
            state.paths.len(),
        ));
    }

    apply_sacd_entries_to_sidecar(
        &mut sidecar,
        &area_track_ids,
        &state.entries,
        &state.deleted,
    )?;

    let sibling_area_idx = match area_idx {
        1 => 2u8,
        2 => 1u8,
        _ => unreachable!("area_idx must be 1 or 2 by construction above"),
    };
    let sibling_track_info: Vec<(u32, Option<u32>)> = sidecar
        .tracks_for_area(sibling_area_idx)
        .iter()
        .map(|t| {
            let tn = t
                .meta
                .get("TRACKNUMBER")
                .and_then(|s| s.trim().parse::<u32>().ok());
            (t.id, tn)
        })
        .collect();
    let mirror = if sibling_track_info.is_empty() {
        MirrorOutcome {
            sibling_present: false,
            sibling_total: 0,
            mirrored_count: 0,
        }
    } else {
        let editor_tn_to_idx: std::collections::HashMap<u32, usize> = state
            .entries
            .iter()
            .find(|e| e.display_key.eq_ignore_ascii_case("TRACKNUMBER"))
            .map(|e| {
                e.per_file_values
                    .iter()
                    .enumerate()
                    .filter_map(|(i, v)| v.trim().parse::<u32>().ok().map(|n| (n, i)))
                    .collect()
            })
            .unwrap_or_default();

        let mut mirrored_count = 0usize;
        for (_sib_tid, sib_tn) in &sibling_track_info {
            if sib_tn.and_then(|tn| editor_tn_to_idx.get(&tn)).is_some() {
                mirrored_count += 1;
            }
        }

        for (entry_idx, entry) in state.entries.iter().enumerate() {
            if is_per_area_specific_key(&entry.display_key) {
                continue;
            }
            let Some(sidecar_key) = editor_key_to_sidecar_key(&entry.display_key) else {
                continue;
            };
            let entry_deleted = state.deleted.contains(&entry_idx);
            if is_album_level_sidecar_key(sidecar_key) {
                let new_val = if entry_deleted {
                    String::new()
                } else {
                    entry.value.clone()
                };
                for (sib_tid, _) in &sibling_track_info {
                    if let Some(track) = sidecar.tracks.iter_mut().find(|t| t.id == *sib_tid) {
                        if new_val.is_empty() {
                            track.meta.remove(sidecar_key);
                        } else {
                            track.meta.insert(sidecar_key.to_string(), new_val.clone());
                        }
                    }
                }
            } else {
                for (sib_tid, sib_tn) in &sibling_track_info {
                    let Some(editor_idx) = sib_tn.and_then(|tn| editor_tn_to_idx.get(&tn).copied())
                    else {
                        continue;
                    };
                    let new_val = if entry_deleted {
                        String::new()
                    } else {
                        entry
                            .per_file_values
                            .get(editor_idx)
                            .cloned()
                            .unwrap_or_default()
                    };
                    if let Some(track) = sidecar.tracks.iter_mut().find(|t| t.id == *sib_tid) {
                        if new_val.is_empty() {
                            track.meta.remove(sidecar_key);
                        } else {
                            track.meta.insert(sidecar_key.to_string(), new_val);
                        }
                    }
                }
            }
        }

        MirrorOutcome {
            sibling_present: true,
            sibling_total: sibling_track_info.len(),
            mirrored_count,
        }
    };

    super::sacd_sidecar::write_sidecar(sidecar_path, &sidecar)
        .map_err(|e| format!("write: {}", e))?;
    Ok(SacdSaveOutcome { kind, mirror })
}

/// Handle mouse events for generic overlays: click-outside-to-close,
/// footer pill clicks, and scroll. Returns true if the event was handled.
fn handle_generic_overlay_mouse(
    app: &mut AppState,
    mouse: MouseEvent,
    _tx: &mpsc::Sender<AppMessage>,
) -> bool {
    let area = crossterm::terminal::size().unwrap_or((80, 24));
    let mx = mouse.column;
    let my = mouse.row;

    // Determine which overlay is active and compute its popup rect.
    // Returns (popup_rect, pill_labels_for_footer, close_action).
    let (popup, pills): (Rect, Vec<(&str, &str)>) = if app.bookmarks.overlay_open {
        let w: u16 = 64.min(area.0.saturating_sub(4));
        let list_h = app.bookmarks.entries.len() as u16;
        let h = (list_h + 5).min(area.1.saturating_sub(4)).max(8);
        let x = (area.0.saturating_sub(w)) / 2;
        let y = (area.1.saturating_sub(h)) / 2;
        if app.bookmarks.naming.is_some() {
            (
                Rect::new(x, y, w, h),
                vec![("Enter save", "enter"), ("Esc cancel", "esc")],
            )
        } else {
            (
                Rect::new(x, y, w, h),
                vec![
                    ("Enter cd", "enter"),
                    ("a add", "a"),
                    ("d delete", "d"),
                    ("e rename", "e"),
                    ("Esc close", "esc"),
                ],
            )
        }
    } else if app.recent.overlay_open {
        let w: u16 = 72.min(area.0.saturating_sub(4));
        let list_h = app.recent.entries.len() as u16;
        let h = (list_h + 5).min(area.1.saturating_sub(4)).max(8);
        let x = (area.0.saturating_sub(w)) / 2;
        let y = (area.1.saturating_sub(h)) / 2;
        (
            Rect::new(x, y, w, h),
            vec![
                ("Enter load", "enter"),
                ("d delete", "d"),
                ("Esc close", "esc"),
            ],
        )
    } else if app.preset.overlay_open {
        let w: u16 = 36;
        let list_h = app.preset.overlay_list.len() as u16;
        let h = (list_h + 6).min(area.1.saturating_sub(4));
        let x = area.0.saturating_sub(w + 2);
        let y = area.1.saturating_sub(h + 3);
        if app.preset.naming_input.is_some() {
            (
                Rect::new(x, y, w, h),
                vec![("Enter save", "enter"), ("Esc cancel", "esc")],
            )
        } else {
            (
                Rect::new(x, y, w, h),
                vec![
                    ("n new", "n"),
                    ("d dup", "d"),
                    ("x del", "x"),
                    ("Esc close", "esc"),
                ],
            )
        }
    } else {
        // ActiveOverlay-based overlays.
        match &app.active_overlay {
            ActiveOverlay::Analysis { .. } => {
                let w = ((area.0 as usize) * 80 / 100)
                    .max(50)
                    .min(area.0 as usize - 2) as u16;
                let h = ((area.1 as usize) * 80 / 100)
                    .max(12)
                    .min(area.1 as usize - 2) as u16;
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                (
                    Rect::new(x, y, w, h),
                    vec![
                        (":analyze!", ":analyze!"),
                        (":write-dr", ":write-dr"),
                        (":write-rg-track", ":write-rg-track"),
                        (":write-rg-album", ":write-rg-album"),
                        ("Esc close", "esc"),
                    ],
                )
            }
            ActiveOverlay::BulkRename(ref state) => {
                let w = ((area.0 as usize) * 85 / 100)
                    .max(60)
                    .min(area.0 as usize - 2) as u16;
                let h = ((area.1 as usize) * 85 / 100)
                    .max(16)
                    .min(area.1 as usize - 2) as u16;
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                if state.focus == super::app::BulkRenameFocus::Template {
                    (
                        Rect::new(x, y, w, h),
                        vec![
                            ("Tab list", "tab"),
                            ("Enter commit", "enter"),
                            ("Esc cancel", "esc"),
                        ],
                    )
                } else {
                    (
                        Rect::new(x, y, w, h),
                        vec![
                            ("Tab tmpl", "tab"),
                            ("e edit", "e"),
                            ("c cue", "c"),
                            ("C caps", "C"),
                            ("Enter commit", "enter"),
                            ("Esc cancel", "esc"),
                        ],
                    )
                }
            }
            ActiveOverlay::BatchList { .. } => {
                let w = area.0.saturating_sub(8).min(100).max(40);
                let h = area.1.saturating_sub(6).min(30).max(10);
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                (
                    Rect::new(x, y, w, h),
                    vec![("d remove", "d"), ("Esc close", "esc")],
                )
            }
            ActiveOverlay::Help { .. } => {
                let w = ((area.0 as usize) * 70 / 100)
                    .max(50)
                    .min(area.0 as usize - 2) as u16;
                let h = ((area.1 as usize) * 80 / 100)
                    .max(12)
                    .min(area.1 as usize - 2) as u16;
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                (Rect::new(x, y, w, h), vec![("Esc close", "esc")])
            }
            ActiveOverlay::ErrorDetail { .. } => {
                let w: u16 = 60;
                let h: u16 = 12;
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                (Rect::new(x, y, w, h), vec![("Esc close", "esc")])
            }
            ActiveOverlay::ItemInfo { .. } => {
                let w: u16 = 70;
                let h: u16 = 16;
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                (Rect::new(x, y, w, h), vec![("Esc close", "esc")])
            }
            ActiveOverlay::FileInput { .. } => {
                let w: u16 = 60;
                let h: u16 = 7;
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                (
                    Rect::new(x, y, w, h),
                    vec![("Enter confirm", "enter"), ("Esc cancel", "esc")],
                )
            }
            ActiveOverlay::TextEdit { .. } => {
                let w = area.0.saturating_sub(4).min(80);
                let h: u16 = 7;
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                (
                    Rect::new(x, y, w, h),
                    vec![("Enter save", "enter"), ("Esc cancel", "esc")],
                )
            }
            ActiveOverlay::FormatSettings { ref kind, .. } => {
                let min_w = super::draw_overlays::format_settings_min_width(kind);
                let w: u16 = area.0.saturating_sub(4).min(min_w);
                let field_count = super::draw_overlays::format_settings_field_count(kind);
                let h: u16 = if matches!(kind, FormatSettingsKind::Sox { .. }) { 17 } else { field_count + 6 };
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                (
                    Rect::new(x, y, w, h),
                    vec![
                        ("↑↓ navigate", "↑↓"),
                        ("Enter save", "enter"),
                        ("Esc cancel", "esc"),
                    ],
                )
            }
            ActiveOverlay::Verify { .. } => {
                let w = ((area.0 as usize) * 70 / 100)
                    .max(40)
                    .min(area.0 as usize - 2) as u16;
                let h = ((area.1 as usize) * 70 / 100)
                    .max(10)
                    .min(area.1 as usize - 2) as u16;
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                (Rect::new(x, y, w, h), vec![("Esc close", "esc")])
            }
            ActiveOverlay::BitCompare { .. } => {
                let w = ((area.0 as usize) * 75 / 100)
                    .max(50)
                    .min(area.0 as usize - 2) as u16;
                let h = ((area.1 as usize) * 70 / 100)
                    .max(10)
                    .min(area.1 as usize - 2) as u16;
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                (Rect::new(x, y, w, h), vec![("Esc close", "esc")])
            }
            ActiveOverlay::Preemphasis { .. } => {
                let w = ((area.0 as usize) * 75 / 100)
                    .max(50)
                    .min(area.0 as usize - 2) as u16;
                let h = ((area.1 as usize) * 70 / 100)
                    .max(10)
                    .min(area.1 as usize - 2) as u16;
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                (Rect::new(x, y, w, h), vec![("Esc close", "esc")])
            }
            ActiveOverlay::CueImportReview { .. } => {
                let w = ((area.0 as usize) * 80 / 100)
                    .max(50)
                    .min(area.0 as usize - 2) as u16;
                let h = ((area.1 as usize) * 80 / 100)
                    .max(12)
                    .min(area.1 as usize - 2) as u16;
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                (
                    Rect::new(x, y, w, h),
                    vec![("Enter accept", "enter"), ("Esc cancel", "esc")],
                )
            }
            ActiveOverlay::GnudbSelect { .. } => {
                let w = ((area.0 as usize) * 70 / 100)
                    .max(40)
                    .min(area.0 as usize - 2) as u16;
                let h = ((area.1 as usize) * 60 / 100)
                    .max(8)
                    .min(area.1 as usize - 2) as u16;
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                (
                    Rect::new(x, y, w, h),
                    vec![("Enter select", "enter"), ("Esc cancel", "esc")],
                )
            }
            ActiveOverlay::GnudbReview(ref state) => {
                let w = ((area.0 as usize) * 85 / 100)
                    .max(50)
                    .min(area.0 as usize - 2) as u16;
                let h = ((area.1 as usize) * 85 / 100)
                    .max(14)
                    .min(area.1 as usize - 2) as u16;
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                if state.edit_input.is_some() {
                    (
                        Rect::new(x, y, w, h),
                        vec![("Enter confirm", "enter"), ("Esc cancel", "esc")],
                    )
                } else {
                    let mut pills = Vec::new();
                    if state.origin_matches.is_some() {
                        pills.push(("b back", "b"));
                    }
                    pills.extend_from_slice(&[
                        ("Enter edit", "enter"),
                        ("c fix-caps", "c"),
                        ("a accept", "a"),
                        ("Esc cancel", "esc"),
                    ]);
                    (Rect::new(x, y, w, h), pills)
                }
            }
            ActiveOverlay::CtdbVerify(_) => {
                let w = ((area.0 as usize) * 70 / 100)
                    .max(50)
                    .min(area.0 as usize - 2) as u16;
                let h = ((area.1 as usize) * 70 / 100)
                    .max(10)
                    .min(area.1 as usize - 2) as u16;
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                (Rect::new(x, y, w, h), vec![("Esc close", "esc")])
            }
            ActiveOverlay::ArBatchReport { .. } => {
                let w = ((area.0 as usize) * 80 / 100)
                    .max(60)
                    .min(area.0 as usize - 2) as u16;
                let h = ((area.1 as usize) * 80 / 100)
                    .max(12)
                    .min(area.1 as usize - 2) as u16;
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                (Rect::new(x, y, w, h), vec![("Esc close", "esc")])
            }
            ActiveOverlay::AccurateRipVerify(ref state) => {
                let w = ((area.0 as usize) * 70 / 100)
                    .max(50)
                    .min(area.0 as usize - 2) as u16;
                let h = ((area.1 as usize) * 70 / 100)
                    .max(10)
                    .min(area.1 as usize - 2) as u16;
                let x = (area.0.saturating_sub(w)) / 2;
                let y = (area.1.saturating_sub(h)) / 2;
                let mut pills = vec![("Esc close", "esc")];
                let result = &state.pages[state.active_page].result;
                let has_unmatched = result
                    .tracks
                    .iter()
                    .any(|t| t.status == super::accuraterip::ArTrackStatus::Mismatch);
                if result.was_common_scan && has_unmatched {
                    pills.push((":ar! full scan", ":ar!"));
                }
                if super::accuraterip::detect_uniform_offset(result).is_some() {
                    pills.push((":ar-fix correct offset", ":ar-fix"));
                }
                (Rect::new(x, y, w, h), pills)
            }
            ActiveOverlay::Confirmation { .. } => {
                // Already has button_map support — skip.
                return false;
            }
            _ => return false,
        }
    };

    let in_popup =
        mx >= popup.x && mx < popup.x + popup.width && my >= popup.y && my < popup.y + popup.height;

    // Footer row = last row inside the popup border.
    let footer_y = popup.y + popup.height.saturating_sub(2);
    let inner_x = popup.x + 1;
    let inner_w = popup.width.saturating_sub(2);
    let on_footer = my == footer_y && mx >= inner_x && mx < inner_x + inner_w;

    match mouse.kind {
        // Scroll: navigate for scrollable overlays.
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            // Simulate Up/Down key for scrollable overlays.
            let code = if mouse.kind == MouseEventKind::ScrollUp {
                KeyCode::Up
            } else {
                KeyCode::Down
            };
            let fake = KeyEvent::new(code, KeyModifiers::NONE);
            // Route through the appropriate key handler.
            if app.bookmarks.overlay_open {
                handle_bookmarks_overlay_key(app, fake, _tx);
            } else if app.recent.overlay_open {
                handle_recent_overlay_key(app, fake);
            } else if app.preset.overlay_open {
                handle_preset_overlay_key(app, fake);
            } else {
                handle_overlay_key(app, fake, _tx);
            }
            return true;
        }

        // Left click outside popup: close.
        MouseEventKind::Down(MouseButton::Left) if !in_popup => {
            if app.bookmarks.overlay_open {
                app.bookmarks.close_overlay();
            } else if app.recent.overlay_open {
                app.recent.overlay_open = false;
            } else if app.preset.overlay_open {
                app.preset.overlay_open = false;
            } else {
                app.active_overlay = ActiveOverlay::None;
            }
            return true;
        }

        // Left click on footer: pill hit-test.
        MouseEventKind::Down(MouseButton::Left) if on_footer && !pills.is_empty() => {
            if let Some(action) = footer_pill_hit(&pills, mx, inner_x, inner_w) {
                // Actions starting with ":" are command-mode commands —
                // execute them directly instead of synthesizing a keypress.
                if action.starts_with(':') {
                    let cmd = super::command::parse_command(&action[1..]);
                    super::command::execute_command(app, cmd, _tx);
                } else {
                    let fake = match action {
                        "enter" => KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                        "esc" => KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                        "tab" => KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
                        other => {
                            let ch = other.chars().next().unwrap_or('?');
                            if ch.is_uppercase() {
                                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::SHIFT)
                            } else {
                                KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)
                            }
                        }
                    };
                    if app.bookmarks.overlay_open {
                        handle_bookmarks_overlay_key(app, fake, _tx);
                    } else if app.recent.overlay_open {
                        handle_recent_overlay_key(app, fake);
                    } else if app.preset.overlay_open {
                        handle_preset_overlay_key(app, fake);
                    } else {
                        handle_overlay_key(app, fake, _tx);
                    }
                }
                return true;
            }
        }

        // Left click in content area: GnudbSelect (click to select, double-click to confirm).
        MouseEventKind::Down(MouseButton::Left)
            if in_popup
                && !on_footer
                && matches!(app.active_overlay, ActiveOverlay::GnudbSelect { .. }) =>
        {
            if let ActiveOverlay::GnudbSelect {
                matches,
                mut selected,
                scroll,
                paths,
            } = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None)
            {
                let content_y = popup.y + 1;
                if my >= content_y && my < footer_y {
                    let clicked_row = (my - content_y) as usize + scroll;
                    if clicked_row < matches.len() {
                        if clicked_row == selected {
                            // Click on already-selected row = confirm (like double-click).
                            let m = &matches[clicked_row];
                            let category = m.category.clone();
                            let disc_id = m.disc_id.clone();
                            app.set_status(format!("Reading {}...", m.title));
                            let tx = _tx.clone();
                            let paths_c = paths.clone();
                            let origin = matches.clone();
                            tokio::spawn(async move {
                                let result = super::gnudb::read_gnudb(&category, &disc_id).await;
                                let _ = tx
                                    .send(AppMessage::GnudbReadComplete {
                                        result,
                                        paths: paths_c,
                                        origin_matches: Some(origin),
                                    })
                                    .await;
                            });
                            return true;
                        }
                        selected = clicked_row;
                    }
                }
                app.active_overlay = ActiveOverlay::GnudbSelect {
                    matches,
                    selected,
                    scroll,
                    paths,
                };
            }
            return true;
        }

        // Left click in content area: position cursor for GnudbReview.
        MouseEventKind::Down(MouseButton::Left)
            if in_popup
                && !on_footer
                && matches!(app.active_overlay, ActiveOverlay::GnudbReview(_)) =>
        {
            if let ActiveOverlay::GnudbReview(mut state) =
                std::mem::replace(&mut app.active_overlay, ActiveOverlay::None)
            {
                use super::app::GnudbRowKind;
                let content_y = popup.y + 1; // below top border
                let is_multi = state.pages.len() > 1;
                let row_offset = if is_multi { 2 } else { 0 };
                if my >= content_y && my < footer_y {
                    let visual_row = (my - content_y) as usize;
                    if is_multi && visual_row == 0 {
                        // Click on disc pill row — determine which pill.
                        let inner_x_pos = popup.x + 1;
                        let click_x = mx.saturating_sub(inner_x_pos) as usize;
                        // Pills are rendered as: "  [disc 01] [disc 02] ..."
                        // Each pill: " {label} " (label.len() + 2) + 1 gap.
                        let mut x_pos = 2usize; // leading "  "
                        for (i, pg) in state.pages.iter().enumerate() {
                            let label = if pg.label.is_empty() {
                                format!("disc {}", i + 1)
                            } else {
                                pg.label.clone()
                            };
                            let pill_w = label.len() + 2; // " label "
                            if click_x >= x_pos && click_x < x_pos + pill_w {
                                state.active_page = i;
                                state.cursor = 0;
                                state.scroll = 0;
                                state.edit_input = None;
                                break;
                            }
                            x_pos += pill_w + 1; // pill + gap
                        }
                    } else if visual_row >= row_offset {
                        let clicked_row = (visual_row - row_offset) + state.scroll;
                        let page_rows = &state.pages[state.active_page].rows;
                        if clicked_row < page_rows.len()
                            && !matches!(
                                page_rows.get(clicked_row),
                                Some(GnudbRowKind::TrackHeader { .. })
                            )
                        {
                            // Double-click detection.
                            let now = std::time::Instant::now();
                            let is_double = state
                                .last_click
                                .map(|(prev_row, prev_time)| {
                                    prev_row == clicked_row
                                        && now.duration_since(prev_time).as_millis() < 400
                                })
                                .unwrap_or(false);

                            state.cursor = clicked_row;

                            if is_double {
                                // Open inline edit.
                                let page = &state.pages[state.active_page];
                                let value = match &page.rows[clicked_row] {
                                    GnudbRowKind::AlbumField(field) => match *field {
                                        "Album" => Some(page.album.clone()),
                                        "Year" => Some(page.year.clone()),
                                        "Genre" => Some(page.genre.clone()),
                                        _ => None,
                                    },
                                    GnudbRowKind::TrackField { track_idx, field } => {
                                        let track = &page.tracks[*track_idx];
                                        match *field {
                                            "Title" => Some(track.title.clone()),
                                            "Artist" => Some(track.artist.clone()),
                                            _ => None,
                                        }
                                    }
                                    _ => None,
                                };
                                if let Some(val) = value {
                                    state.edit_input =
                                        Some(super::text_input::TextInputState::new(val));
                                }
                                state.last_click = None;
                            } else {
                                state.last_click = Some((clicked_row, now));
                            }
                        }
                    }
                }
                app.active_overlay = ActiveOverlay::GnudbReview(state);
            }
            return true;
        }

        // Left click in content area: disc pill click for AccurateRipVerify.
        MouseEventKind::Down(MouseButton::Left)
            if in_popup
                && !on_footer
                && matches!(app.active_overlay, ActiveOverlay::AccurateRipVerify(_)) =>
        {
            if let ActiveOverlay::AccurateRipVerify(mut state) =
                std::mem::replace(&mut app.active_overlay, ActiveOverlay::None)
            {
                let content_y = popup.y + 1;
                if state.pages.len() > 1 && my >= content_y && my < footer_y {
                    let visual_row = (my - content_y) as usize;
                    let content_row = visual_row + state.scroll;
                    if content_row == 0 {
                        // Click on disc pill row — determine which pill.
                        let inner_x_pos = popup.x + 1;
                        let click_x = mx.saturating_sub(inner_x_pos) as usize;
                        let mut x_pos = 2usize; // leading "  "
                        for (i, pg) in state.pages.iter().enumerate() {
                            let label = if pg.label.is_empty() {
                                format!("disc {}", i + 1)
                            } else {
                                pg.label.clone()
                            };
                            let pill_w = label.len() + 2; // " label "
                            if click_x >= x_pos && click_x < x_pos + pill_w {
                                state.active_page = i;
                                state.scroll = 0;
                                break;
                            }
                            x_pos += pill_w + 1; // pill + gap
                        }
                    }
                }
                app.active_overlay = ActiveOverlay::AccurateRipVerify(state);
            }
            return true;
        }

        // Left click in content area: disc pill click for CtdbVerify.
        MouseEventKind::Down(MouseButton::Left)
            if in_popup
                && !on_footer
                && matches!(app.active_overlay, ActiveOverlay::CtdbVerify(_)) =>
        {
            if let ActiveOverlay::CtdbVerify(mut state) =
                std::mem::replace(&mut app.active_overlay, ActiveOverlay::None)
            {
                let content_y = popup.y + 1;
                if state.pages.len() > 1 && my >= content_y && my < footer_y {
                    let visual_row = (my - content_y) as usize;
                    let content_row = visual_row + state.scroll;
                    if content_row == 0 {
                        let inner_x_pos = popup.x + 1;
                        let click_x = mx.saturating_sub(inner_x_pos) as usize;
                        let mut x_pos = 2usize;
                        for (i, pg) in state.pages.iter().enumerate() {
                            let label = if pg.label.is_empty() {
                                format!("disc {}", i + 1)
                            } else {
                                pg.label.clone()
                            };
                            let pill_w = label.len() + 2;
                            if click_x >= x_pos && click_x < x_pos + pill_w {
                                state.active_page = i;
                                state.scroll = 0;
                                break;
                            }
                            x_pos += pill_w + 1;
                        }
                    }
                }
                app.active_overlay = ActiveOverlay::CtdbVerify(state);
            }
            return true;
        }

        // Right-click anywhere: close overlay.
        MouseEventKind::Down(MouseButton::Right) => {
            if app.bookmarks.overlay_open {
                app.bookmarks.close_overlay();
            } else if app.recent.overlay_open {
                app.recent.overlay_open = false;
            } else if app.preset.overlay_open {
                app.preset.overlay_open = false;
            } else {
                app.active_overlay = ActiveOverlay::None;
            }
            return true;
        }

        // Move events: consume but don't act (prevents underlying UI from reacting).
        MouseEventKind::Moved | MouseEventKind::Drag(_) => {
            return true;
        }

        // Other clicks inside popup: consume without action.
        MouseEventKind::Down(_) if in_popup => {
            return true;
        }

        _ => {}
    }

    false
}

/// Handle mouse events inside the metadata editor overlay.
/// Build the row-level context menu for the MetadataEditor based on the
/// clicked row's pill state (Revert / UseMb / None) and deletion mark.
pub(super) fn build_metadata_row_context_menu(
    state: &super::app::MetadataEditorState,
    row: usize,
) -> Vec<super::context_menu::ContextMenuEntry> {
    use super::context_menu::{ContextAction, ContextMenuEntry, ContextMenuItem};
    let mut entries: Vec<ContextMenuEntry> = Vec::new();
    let is_synthetic = state
        .entries
        .get(row)
        .map(super::probe::is_synthetic_preview)
        .unwrap_or(false);
    if is_synthetic {
        entries.push(ContextMenuEntry::Item(ContextMenuItem {
            label: "View CUE sheet".to_string(),
            action: ContextAction::MetadataCueView,
            shortcut: Some(":cue-view".to_string()),
            enabled: true,
        }));
    }
    let pill = state
        .entries
        .get(row)
        .map(super::probe::mb_pill_state)
        .unwrap_or(super::probe::MbRevertPill::None);
    match pill {
        super::probe::MbRevertPill::Revert => {
            entries.push(ContextMenuEntry::Item(ContextMenuItem {
                label: "Revert to file value".to_string(),
                action: ContextAction::MetadataRevertMb,
                shortcut: Some(":revert".to_string()),
                enabled: true,
            }));
        }
        super::probe::MbRevertPill::UseMb => {
            entries.push(ContextMenuEntry::Item(ContextMenuItem {
                label: "Use MusicBrainz value".to_string(),
                action: ContextAction::MetadataRevertMb,
                shortcut: Some(":revert".to_string()),
                enabled: true,
            }));
        }
        super::probe::MbRevertPill::None => {}
    }
    let is_binary = state.entries.get(row).map(|e| e.is_binary).unwrap_or(true);
    if !is_binary {
        entries.push(ContextMenuEntry::Item(ContextMenuItem {
            label: "Edit value".to_string(),
            action: ContextAction::MetadataEditValue,
            shortcut: None,
            enabled: true,
        }));
    }
    if state.deleted.contains(&row) {
        entries.push(ContextMenuEntry::Item(ContextMenuItem {
            label: "Restore (cancel deletion)".to_string(),
            action: ContextAction::MetadataRestoreEntry,
            shortcut: None,
            enabled: true,
        }));
    } else {
        entries.push(ContextMenuEntry::Item(ContextMenuItem {
            label: "Delete entry".to_string(),
            action: ContextAction::MetadataDeleteEntry,
            shortcut: None,
            enabled: true,
        }));
    }
    entries.push(ContextMenuEntry::Separator);
    entries.push(ContextMenuEntry::Item(ContextMenuItem {
        label: "Add new field".to_string(),
        action: ContextAction::MetadataAddField,
        shortcut: None,
        enabled: true,
    }));
    entries
}

/// Build the row-level context menu for CuePreview. `line_idx` is set
/// when the right-click landed on a content line (0-based); it adds an
/// "Edit this line" entry that carries the index in the action variant.
fn build_cue_preview_context_menu(
    line_idx: Option<usize>,
    is_editing: bool,
) -> Vec<super::context_menu::ContextMenuEntry> {
    use super::context_menu::{ContextAction, ContextMenuEntry, ContextMenuItem};
    let mut entries: Vec<ContextMenuEntry> = Vec::new();
    if is_editing {
        // While editing, the only useful actions are commit/cancel-edit,
        // which the keyboard handles via Enter/Esc — but expose them
        // here too for parity.
        entries.push(ContextMenuEntry::Item(ContextMenuItem {
            label: "Cancel line edit".to_string(),
            action: ContextAction::CuePreviewCancel,
            shortcut: Some(":q".to_string()),
            enabled: true,
        }));
        return entries;
    }
    if let Some(idx) = line_idx {
        entries.push(ContextMenuEntry::Item(ContextMenuItem {
            label: "Edit this line".to_string(),
            action: ContextAction::CuePreviewEditLine(idx),
            shortcut: Some(":e N".to_string()),
            enabled: true,
        }));
        entries.push(ContextMenuEntry::Separator);
    }
    entries.push(ContextMenuEntry::Item(ContextMenuItem {
        label: "Save".to_string(),
        action: ContextAction::CuePreviewSave,
        shortcut: Some(":w".to_string()),
        enabled: true,
    }));
    entries.push(ContextMenuEntry::Item(ContextMenuItem {
        label: "Cancel".to_string(),
        action: ContextAction::CuePreviewCancel,
        shortcut: Some(":q".to_string()),
        enabled: true,
    }));
    entries.push(ContextMenuEntry::Separator);
    entries.push(ContextMenuEntry::Item(ContextMenuItem {
        label: "Scroll to top".to_string(),
        action: ContextAction::CuePreviewScrollTop,
        shortcut: Some(":g".to_string()),
        enabled: true,
    }));
    entries.push(ContextMenuEntry::Item(ContextMenuItem {
        label: "Scroll to bottom".to_string(),
        action: ContextAction::CuePreviewScrollBottom,
        shortcut: Some(":G".to_string()),
        enabled: true,
    }));
    entries
}

/// Build the context menu for the metadata-editor detail overlay
/// (right-click while browsing per-file values).
fn build_metadata_detail_context_menu(
    state: &super::app::MetadataEditorState,
) -> Vec<super::context_menu::ContextMenuEntry> {
    use super::context_menu::{ContextAction, ContextMenuEntry, ContextMenuItem};
    let mut entries: Vec<ContextMenuEntry> = Vec::new();
    if let Some(entry) = state.entries.get(state.detail_field_idx) {
        if super::probe::entry_has_mb_proposed(entry) {
            let pill = super::probe::mb_pill_state_field(entry);
            let label = match pill {
                super::probe::MbRevertPill::Revert => Some("Revert to file values"),
                super::probe::MbRevertPill::UseMb => Some("Use MusicBrainz values"),
                super::probe::MbRevertPill::None => None,
            };
            if let Some(l) = label {
                entries.push(ContextMenuEntry::Item(ContextMenuItem {
                    label: l.to_string(),
                    action: ContextAction::MetadataDetailToggleRevert,
                    shortcut: Some(":revert".to_string()),
                    enabled: true,
                }));
            }
            entries.push(ContextMenuEntry::Item(ContextMenuItem {
                label: "Restore (snap to MB values)".to_string(),
                action: ContextAction::MetadataDetailRestore,
                shortcut: Some(":restore".to_string()),
                enabled: true,
            }));
            entries.push(ContextMenuEntry::Separator);
        }
    }
    entries.push(ContextMenuEntry::Item(ContextMenuItem {
        label: "Back to field list".to_string(),
        action: ContextAction::MetadataDetailBack,
        shortcut: Some("Esc".to_string()),
        enabled: true,
    }));
    entries
}

fn open_template_picker(app: &mut AppState, target: TemplateTarget) {
    let templates = super::template_builder::list_templates(target);
    let active = match target {
        TemplateTarget::Folder => Some(app.convert.output_options.folder_template.clone()),
        TemplateTarget::Filename => Some(app.convert.output_options.filename_template.clone()),
    };
    let preview = if let Some(tmpl) = templates.first() {
        super::template_builder::render_template_preview(tmpl)
    } else {
        String::new()
    };
    app.active_overlay = ActiveOverlay::TemplatePicker {
        target,
        templates,
        selected: 0,
        scroll: 0,
        preview,
        active_template: active,
    };
}

/// Mouse handler for the template picker overlay.
fn handle_template_picker_mouse(app: &mut AppState, mouse: MouseEvent) {
    use crossterm::event::{MouseButton, MouseEventKind};
    let mx = mouse.column;
    let my = mouse.row;

    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return;
    }

    match app.button_map.find_button_at(mx, my) {
        Some(TuiButton::TemplatePickerRow(_)) | Some(TuiButton::TemplatePickerApply) => {
            let click_idx = match app.button_map.find_button_at(mx, my) {
                Some(TuiButton::TemplatePickerRow(i)) => Some(i),
                _ => None, // Apply pill → use selected
            };
            if let ActiveOverlay::TemplatePicker {
                target,
                ref templates,
                selected,
                ..
            } = app.active_overlay
            {
                let idx = click_idx.unwrap_or(selected);
                if let Some(tmpl) = templates.get(idx).cloned() {
                    match target {
                        TemplateTarget::Folder => {
                            app.convert.output_options.folder_template = tmpl;
                        }
                        TemplateTarget::Filename => {
                            app.convert.output_options.filename_template = tmpl;
                        }
                    }
                    app.preset.mark_modified();
                    app.active_overlay = ActiveOverlay::None;
                    app.set_status("Template applied");
                }
            }
        }
        Some(TuiButton::TemplatePickerDelete) => {
            if let ActiveOverlay::TemplatePicker {
                target,
                ref templates,
                mut selected,
                mut scroll,
                ref active_template,
                ..
            } = app.active_overlay
            {
                if let Some(tmpl) = templates.get(selected).cloned() {
                    let _ = super::template_builder::delete_template(target, &tmpl);
                    let new_templates = super::template_builder::list_templates(target);
                    if selected >= new_templates.len() && selected > 0 {
                        selected -= 1;
                    }
                    if scroll > 0 && scroll >= new_templates.len() {
                        scroll = new_templates.len().saturating_sub(1);
                    }
                    let preview = if let Some(tmpl) = new_templates.get(selected) {
                        super::template_builder::render_template_preview(tmpl)
                    } else {
                        String::new()
                    };
                    let active_template = active_template.clone();
                    app.set_status("Template deleted");
                    app.active_overlay = ActiveOverlay::TemplatePicker {
                        target,
                        templates: new_templates,
                        selected,
                        scroll,
                        preview,
                        active_template,
                    };
                }
            }
        }
        Some(TuiButton::TemplatePickerClose) => {
            app.active_overlay = ActiveOverlay::None;
        }
        _ => {
            // Click outside or on non-button area — compute bounds and close if outside
            let area = crossterm::terminal::size().unwrap_or((80, 24));
            let w = (area.0 * 75 / 100).max(50).min(area.0.saturating_sub(2));
            let template_count = if let ActiveOverlay::TemplatePicker { ref templates, .. } =
                app.active_overlay
            {
                templates.len()
            } else {
                0
            };
            let list_rows = template_count.max(1) as u16;
            let content_h = 2 + 1 + list_rows + 1 + 1 + 1 + 1 + 1 + 1;
            let h = content_h.min(area.1 * 60 / 100).max(8);
            let px = (area.0.saturating_sub(w)) / 2;
            let py = (area.1.saturating_sub(h)) / 2;
            if mx < px || mx >= px + w || my < py || my >= py + h {
                app.active_overlay = ActiveOverlay::None;
            }
        }
    }
}

/// Commit format settings overlay values to FormatState.
fn commit_format_settings(app: &mut AppState, kind: &FormatSettingsKind) {
    match kind {
        FormatSettingsKind::Flac {
            compression_input,
            verify,
            md5,
        } => {
            let level: u8 = compression_input.text.trim().parse().unwrap_or(8).min(8);
            app.convert.format.flac_compression_level = level;
            app.convert.format.flac_verify.selected = if *verify { 1 } else { 0 };
            app.convert.format.flac_md5.selected = if *md5 { 0 } else { 1 };
        }
        FormatSettingsKind::Aac {
            profile,
            bitrate_input,
            ..
        } => {
            let bitrate: u32 = bitrate_input
                .text
                .trim()
                .parse()
                .unwrap_or(256)
                .clamp(8, 1024);
            app.convert.format.aac_profile = *profile;
            app.convert.format.aac_bitrate_kbps = bitrate;
            // Recompute quality_preset from the committed bitrate.
            let presets = aac_presets_for_profile(*profile);
            app.convert.format.aac_quality_preset = presets
                .iter()
                .position(|(br, _)| *br == bitrate);
        }
        FormatSettingsKind::Opus {
            content_type,
            bitrate_input,
            complexity_input,
            ..
        } => {
            let bitrate: u32 = bitrate_input
                .text
                .trim()
                .parse()
                .unwrap_or(192)
                .clamp(6, 510);
            let complexity: u8 = complexity_input
                .text
                .trim()
                .parse()
                .unwrap_or(10)
                .min(10);
            app.convert.format.opus_content_type = *content_type;
            app.convert.format.opus_bitrate_kbps = bitrate;
            app.convert.format.opus_complexity = complexity;
            app.convert.format.opus_quality_preset = OPUS_PRESETS
                .iter()
                .position(|(br, _)| *br == bitrate);
        }
        FormatSettingsKind::Mp3 {
            mode,
            vbr_quality_input,
            bitrate_input,
            ..
        } => {
            app.convert.format.mp3_mode = *mode;
            let vbr_q: u8 = vbr_quality_input.text.trim().parse().unwrap_or(0).min(9);
            app.convert.format.mp3_vbr_quality = vbr_q;
            let bitrate: u32 = bitrate_input
                .text
                .trim()
                .parse()
                .unwrap_or(320)
                .clamp(8, 1000);
            app.convert.format.mp3_bitrate_kbps = bitrate;
            app.convert.format.mp3_quality_preset = MP3_BITRATE_PRESETS
                .iter()
                .position(|(br, _)| *br == bitrate);
        }
        FormatSettingsKind::WavPack {
            mode,
            hybrid,
            bitrate_input,
            correction,
        } => {
            app.convert.format.wavpack_mode = *mode;
            app.convert.format.wavpack_hybrid = *hybrid;
            let bitrate: u32 = bitrate_input
                .text
                .trim()
                .parse()
                .unwrap_or(320)
                .clamp(24, 9600);
            app.convert.format.wavpack_bitrate_kbps = bitrate;
            app.convert.format.wavpack_correction = *correction;
        }
        FormatSettingsKind::Ssrc { ref attenuation_input, min_phase, ref dither_id_input, ref pdf_type_input } => {
            app.convert.format.ssrc_attenuation_db = attenuation_input
                .text.trim().parse::<f32>().ok()
                .filter(|v| (0.0..=99.9).contains(v));
            app.convert.format.ssrc_min_phase = *min_phase;
            app.convert.format.ssrc_dither_id = dither_id_input
                .text.trim().parse::<u8>().ok()
                .filter(|v| *v <= 99);
            app.convert.format.ssrc_pdf_type = match pdf_type_input.text.trim() {
                "0" => Some(tonepoet_pipeline::enums::SsrcPdfType::Rectangular),
                "1" => Some(tonepoet_pipeline::enums::SsrcPdfType::Triangular),
                "" => None,
                _ => None,
            };
        }
        FormatSettingsKind::Sox {
            chebyshev,
            bandwidth_input,
            phase_input,
            allow_aliasing,
            sinc_taps_input,
            sinc_attenuation_input,
            sinc_passband_input,
            sinc_transition_input,
            sinc_kaiser_beta_input,
            sinc_phase,
        } => {
            app.convert.format.sox_chebyshev = *chebyshev;
            app.convert.format.sox_bandwidth = bandwidth_input
                .text.trim().parse::<f32>().ok()
                .filter(|v| (74.0..=99.7).contains(v));
            app.convert.format.sox_phase = phase_input
                .text.trim().parse::<u8>().ok()
                .filter(|v| *v <= 100);
            app.convert.format.sox_allow_aliasing = *allow_aliasing;
            app.convert.format.sox_sinc_taps = sinc_taps_input
                .text.trim().parse::<u32>().ok()
                .filter(|v| v.is_power_of_two() && (1024..=67_108_864).contains(v));
            app.convert.format.sox_sinc_attenuation = sinc_attenuation_input
                .text.trim().parse::<u16>().ok()
                .filter(|v| (80..=200).contains(v));
            app.convert.format.sox_sinc_passband = sinc_passband_input
                .text.trim().parse::<f32>().ok()
                .filter(|v| (1.0..=220_000.0).contains(v));
            app.convert.format.sox_sinc_transition = sinc_transition_input
                .text.trim().parse::<f32>().ok()
                .filter(|v| (1.0..=5000.0).contains(v));
            app.convert.format.sox_sinc_kaiser_beta = sinc_kaiser_beta_input
                .text.trim().parse::<f32>().ok()
                .filter(|v| (0.0..=32.0).contains(v));
            app.convert.format.sox_sinc_phase = *sinc_phase;
        }
        FormatSettingsKind::Soxr {
            chebyshev,
            cutoff_input,
            phase_input,
        } => {
            app.convert.format.soxr_chebyshev = *chebyshev;
            app.convert.format.soxr_cutoff = cutoff_input
                .text
                .trim()
                .parse::<f32>()
                .ok()
                .filter(|v| (74.0..=99.7).contains(v));
            app.convert.format.soxr_phase = phase_input
                .text
                .trim()
                .parse::<u8>()
                .ok()
                .filter(|v| *v <= 100);
        }
    }
    app.preset.mark_modified();
}

/// Focus cycle helpers for FormatSettings overlay.
fn format_settings_focus_next(kind: &FormatSettingsKind, focus: FormatSettingsFocus) -> FormatSettingsFocus {
    match kind {
        FormatSettingsKind::Flac { .. } => match focus {
            FormatSettingsFocus::Compression => FormatSettingsFocus::Verify,
            FormatSettingsFocus::Verify => FormatSettingsFocus::Md5,
            _ => FormatSettingsFocus::Compression,
        },
        FormatSettingsKind::Aac { .. } => match focus {
            FormatSettingsFocus::AacProfile => FormatSettingsFocus::AacQuality,
            FormatSettingsFocus::AacQuality => FormatSettingsFocus::AacBitrate,
            _ => FormatSettingsFocus::AacProfile,
        },
        FormatSettingsKind::Opus { .. } => match focus {
            FormatSettingsFocus::OpusContentType => FormatSettingsFocus::OpusQuality,
            FormatSettingsFocus::OpusQuality => FormatSettingsFocus::OpusBitrate,
            FormatSettingsFocus::OpusBitrate => FormatSettingsFocus::OpusComplexity,
            _ => FormatSettingsFocus::OpusContentType,
        },
        FormatSettingsKind::Mp3 { mode, .. } => {
            use tonepoet_pipeline::enums::Mp3Mode;
            if *mode == Mp3Mode::Vbr {
                // VBR: Mode → VbrQuality (2 fields)
                match focus {
                    FormatSettingsFocus::Mp3Mode => FormatSettingsFocus::Mp3VbrQuality,
                    _ => FormatSettingsFocus::Mp3Mode,
                }
            } else {
                // CBR/ABR: Mode → Preset → Bitrate (3 fields)
                match focus {
                    FormatSettingsFocus::Mp3Mode => FormatSettingsFocus::Mp3Preset,
                    FormatSettingsFocus::Mp3Preset => FormatSettingsFocus::Mp3Bitrate,
                    _ => FormatSettingsFocus::Mp3Mode,
                }
            }
        }
        FormatSettingsKind::WavPack { hybrid, .. } => {
            if *hybrid {
                // Hybrid on: Mode → Hybrid → Bitrate → Correction (4 fields)
                match focus {
                    FormatSettingsFocus::WavPackMode => FormatSettingsFocus::WavPackHybrid,
                    FormatSettingsFocus::WavPackHybrid => FormatSettingsFocus::WavPackBitrate,
                    FormatSettingsFocus::WavPackBitrate => FormatSettingsFocus::WavPackCorrection,
                    _ => FormatSettingsFocus::WavPackMode,
                }
            } else {
                // Hybrid off: Mode → Hybrid (2 fields)
                match focus {
                    FormatSettingsFocus::WavPackMode => FormatSettingsFocus::WavPackHybrid,
                    _ => FormatSettingsFocus::WavPackMode,
                }
            }
        }
        FormatSettingsKind::Ssrc { .. } => match focus {
            FormatSettingsFocus::SsrcAttenuation => FormatSettingsFocus::SsrcMinPhase,
            FormatSettingsFocus::SsrcMinPhase => FormatSettingsFocus::SsrcPdf,
            _ => FormatSettingsFocus::SsrcAttenuation,
        },
        FormatSettingsKind::Sox { chebyshev, .. } => {
            match focus {
                FormatSettingsFocus::SoxChebyshev => {
                    if *chebyshev { FormatSettingsFocus::SoxPhase } else { FormatSettingsFocus::SoxBandwidth }
                }
                FormatSettingsFocus::SoxBandwidth => FormatSettingsFocus::SoxPhase,
                FormatSettingsFocus::SoxPhase => FormatSettingsFocus::SoxAliasing,
                FormatSettingsFocus::SoxAliasing => FormatSettingsFocus::SoxSincTaps,
                FormatSettingsFocus::SoxSincTaps => FormatSettingsFocus::SoxSincAttenuation,
                FormatSettingsFocus::SoxSincAttenuation => FormatSettingsFocus::SoxSincPassband,
                FormatSettingsFocus::SoxSincPassband => FormatSettingsFocus::SoxSincTransition,
                FormatSettingsFocus::SoxSincTransition => FormatSettingsFocus::SoxSincKaiserBeta,
                FormatSettingsFocus::SoxSincKaiserBeta => FormatSettingsFocus::SoxSincPhase,
                _ => FormatSettingsFocus::SoxChebyshev,
            }
        }
        FormatSettingsKind::Soxr { .. } => match focus {
            FormatSettingsFocus::SoxrChebyshev => FormatSettingsFocus::SoxrCutoff,
            FormatSettingsFocus::SoxrCutoff => FormatSettingsFocus::SoxrPhase,
            _ => FormatSettingsFocus::SoxrChebyshev,
        },
    }
}

fn format_settings_focus_prev(kind: &FormatSettingsKind, focus: FormatSettingsFocus) -> FormatSettingsFocus {
    match kind {
        FormatSettingsKind::Flac { .. } => match focus {
            FormatSettingsFocus::Compression => FormatSettingsFocus::Md5,
            FormatSettingsFocus::Verify => FormatSettingsFocus::Compression,
            _ => FormatSettingsFocus::Verify,
        },
        FormatSettingsKind::Aac { .. } => match focus {
            FormatSettingsFocus::AacProfile => FormatSettingsFocus::AacBitrate,
            FormatSettingsFocus::AacQuality => FormatSettingsFocus::AacProfile,
            _ => FormatSettingsFocus::AacQuality,
        },
        FormatSettingsKind::Opus { .. } => match focus {
            FormatSettingsFocus::OpusContentType => FormatSettingsFocus::OpusComplexity,
            FormatSettingsFocus::OpusQuality => FormatSettingsFocus::OpusContentType,
            FormatSettingsFocus::OpusBitrate => FormatSettingsFocus::OpusQuality,
            _ => FormatSettingsFocus::OpusBitrate,
        },
        FormatSettingsKind::Mp3 { mode, .. } => {
            use tonepoet_pipeline::enums::Mp3Mode;
            if *mode == Mp3Mode::Vbr {
                match focus {
                    FormatSettingsFocus::Mp3Mode => FormatSettingsFocus::Mp3VbrQuality,
                    _ => FormatSettingsFocus::Mp3Mode,
                }
            } else {
                match focus {
                    FormatSettingsFocus::Mp3Mode => FormatSettingsFocus::Mp3Bitrate,
                    FormatSettingsFocus::Mp3Preset => FormatSettingsFocus::Mp3Mode,
                    _ => FormatSettingsFocus::Mp3Preset,
                }
            }
        }
        FormatSettingsKind::WavPack { hybrid, .. } => {
            if *hybrid {
                match focus {
                    FormatSettingsFocus::WavPackMode => FormatSettingsFocus::WavPackCorrection,
                    FormatSettingsFocus::WavPackHybrid => FormatSettingsFocus::WavPackMode,
                    FormatSettingsFocus::WavPackBitrate => FormatSettingsFocus::WavPackHybrid,
                    _ => FormatSettingsFocus::WavPackBitrate,
                }
            } else {
                match focus {
                    FormatSettingsFocus::WavPackMode => FormatSettingsFocus::WavPackHybrid,
                    _ => FormatSettingsFocus::WavPackMode,
                }
            }
        }
        FormatSettingsKind::Ssrc { .. } => match focus {
            FormatSettingsFocus::SsrcAttenuation => FormatSettingsFocus::SsrcPdf,
            FormatSettingsFocus::SsrcMinPhase => FormatSettingsFocus::SsrcAttenuation,
            _ => FormatSettingsFocus::SsrcMinPhase,
        },
        FormatSettingsKind::Sox { chebyshev, .. } => {
            match focus {
                FormatSettingsFocus::SoxChebyshev => FormatSettingsFocus::SoxSincPhase,
                FormatSettingsFocus::SoxBandwidth => FormatSettingsFocus::SoxChebyshev,
                FormatSettingsFocus::SoxPhase => {
                    if *chebyshev { FormatSettingsFocus::SoxChebyshev } else { FormatSettingsFocus::SoxBandwidth }
                }
                FormatSettingsFocus::SoxAliasing => FormatSettingsFocus::SoxPhase,
                FormatSettingsFocus::SoxSincTaps => FormatSettingsFocus::SoxAliasing,
                FormatSettingsFocus::SoxSincAttenuation => FormatSettingsFocus::SoxSincTaps,
                FormatSettingsFocus::SoxSincPassband => FormatSettingsFocus::SoxSincAttenuation,
                FormatSettingsFocus::SoxSincTransition => FormatSettingsFocus::SoxSincPassband,
                FormatSettingsFocus::SoxSincKaiserBeta => FormatSettingsFocus::SoxSincTransition,
                _ => FormatSettingsFocus::SoxSincKaiserBeta,
            }
        }
        FormatSettingsKind::Soxr { .. } => match focus {
            FormatSettingsFocus::SoxrChebyshev => FormatSettingsFocus::SoxrPhase,
            FormatSettingsFocus::SoxrCutoff => FormatSettingsFocus::SoxrChebyshev,
            _ => FormatSettingsFocus::SoxrCutoff,
        },
    }
}

/// Handle field-specific keypresses within the FormatSettings overlay.
fn handle_format_settings_field_key(
    kind: &mut FormatSettingsKind,
    focus: FormatSettingsFocus,
    key: &KeyEvent,
) {
    match kind {
        FormatSettingsKind::Flac {
            compression_input,
            verify,
            md5,
        } => match focus {
            FormatSettingsFocus::Compression => {
                super::text_input::handle_text_input_key(compression_input, key);
            }
            FormatSettingsFocus::Verify => {
                if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                    *verify = !*verify;
                }
            }
            FormatSettingsFocus::Md5 => {
                if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                    *md5 = !*md5;
                }
            }
            _ => {}
        },
        FormatSettingsKind::Aac {
            profile,
            quality_preset,
            bitrate_input,
        } => {
            use tonepoet_pipeline::enums::AacProfile;
            match focus {
                FormatSettingsFocus::AacProfile => {
                    if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                        let profiles = [AacProfile::LcAac, AacProfile::HeAac, AacProfile::HeAacV2];
                        let cur = profiles.iter().position(|p| p == profile).unwrap_or(0);
                        let next = if key.code == KeyCode::Left {
                            if cur == 0 { profiles.len() - 1 } else { cur - 1 }
                        } else {
                            (cur + 1) % profiles.len()
                        };
                        *profile = profiles[next];
                        // Reset to "high" preset for the new profile (index 1, or 0 if only 3 presets).
                        let presets = aac_presets_for_profile(*profile);
                        let high_idx = presets.iter().position(|(_, l)| *l == "high").unwrap_or(0);
                        *quality_preset = Some(high_idx);
                        bitrate_input.text = presets[high_idx].0.to_string();
                        bitrate_input.cursor = bitrate_input.text.len();
                    }
                }
                FormatSettingsFocus::AacQuality => {
                    if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                        let presets = aac_presets_for_profile(*profile);
                        let cur = quality_preset.unwrap_or(0);
                        let next = if key.code == KeyCode::Left {
                            if cur == 0 { presets.len() - 1 } else { cur - 1 }
                        } else {
                            (cur + 1) % presets.len()
                        };
                        *quality_preset = Some(next);
                        bitrate_input.text = presets[next].0.to_string();
                        bitrate_input.cursor = bitrate_input.text.len();
                    }
                }
                FormatSettingsFocus::AacBitrate => {
                    super::text_input::handle_text_input_key(bitrate_input, key);
                    *quality_preset = None;
                }
                _ => {}
            }
        }
        FormatSettingsKind::Opus {
            content_type,
            quality_preset,
            bitrate_input,
            complexity_input,
        } => {
            use tonepoet_pipeline::enums::OpusContentType;
            match focus {
                FormatSettingsFocus::OpusContentType => {
                    if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                        let types = [OpusContentType::Auto, OpusContentType::Music, OpusContentType::Speech];
                        let cur = types.iter().position(|t| t == content_type).unwrap_or(0);
                        let next = if key.code == KeyCode::Left {
                            if cur == 0 { types.len() - 1 } else { cur - 1 }
                        } else {
                            (cur + 1) % types.len()
                        };
                        *content_type = types[next];
                    }
                }
                FormatSettingsFocus::OpusQuality => {
                    if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                        let presets = OPUS_PRESETS;
                        let cur = quality_preset.unwrap_or(0);
                        let next = if key.code == KeyCode::Left {
                            if cur == 0 { presets.len() - 1 } else { cur - 1 }
                        } else {
                            (cur + 1) % presets.len()
                        };
                        *quality_preset = Some(next);
                        bitrate_input.text = presets[next].0.to_string();
                        bitrate_input.cursor = bitrate_input.text.len();
                    }
                }
                FormatSettingsFocus::OpusBitrate => {
                    super::text_input::handle_text_input_key(bitrate_input, key);
                    *quality_preset = None;
                }
                FormatSettingsFocus::OpusComplexity => {
                    super::text_input::handle_text_input_key(complexity_input, key);
                }
                _ => {}
            }
        }
        FormatSettingsKind::Mp3 {
            mode,
            vbr_quality_input,
            quality_preset,
            bitrate_input,
        } => {
            use tonepoet_pipeline::enums::Mp3Mode;
            match focus {
                FormatSettingsFocus::Mp3Mode => {
                    if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                        let modes = [Mp3Mode::Vbr, Mp3Mode::Cbr, Mp3Mode::Abr];
                        let cur = modes.iter().position(|m| m == mode).unwrap_or(0);
                        let next = if key.code == KeyCode::Left {
                            if cur == 0 { modes.len() - 1 } else { cur - 1 }
                        } else {
                            (cur + 1) % modes.len()
                        };
                        *mode = modes[next];
                    }
                }
                FormatSettingsFocus::Mp3VbrQuality => {
                    super::text_input::handle_text_input_key(vbr_quality_input, key);
                }
                FormatSettingsFocus::Mp3Preset => {
                    if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                        let presets = MP3_BITRATE_PRESETS;
                        let cur = quality_preset.unwrap_or(0);
                        let next = if key.code == KeyCode::Left {
                            if cur == 0 { presets.len() - 1 } else { cur - 1 }
                        } else {
                            (cur + 1) % presets.len()
                        };
                        *quality_preset = Some(next);
                        bitrate_input.text = presets[next].0.to_string();
                        bitrate_input.cursor = bitrate_input.text.len();
                    }
                }
                FormatSettingsFocus::Mp3Bitrate => {
                    super::text_input::handle_text_input_key(bitrate_input, key);
                    *quality_preset = None;
                }
                _ => {}
            }
        }
        FormatSettingsKind::WavPack {
            mode,
            hybrid,
            bitrate_input,
            correction,
        } => {
            use tonepoet_pipeline::enums::WavPackMode;
            match focus {
                FormatSettingsFocus::WavPackMode => {
                    if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                        let modes = [WavPackMode::Fast, WavPackMode::Normal, WavPackMode::High, WavPackMode::VeryHigh];
                        let cur = modes.iter().position(|m| m == mode).unwrap_or(1);
                        let next = if key.code == KeyCode::Left {
                            if cur == 0 { modes.len() - 1 } else { cur - 1 }
                        } else {
                            (cur + 1) % modes.len()
                        };
                        *mode = modes[next];
                    }
                }
                FormatSettingsFocus::WavPackHybrid => {
                    if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                        *hybrid = !*hybrid;
                    }
                }
                FormatSettingsFocus::WavPackBitrate => {
                    super::text_input::handle_text_input_key(bitrate_input, key);
                }
                FormatSettingsFocus::WavPackCorrection => {
                    if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                        *correction = !*correction;
                    }
                }
                _ => {}
            }
        }
        FormatSettingsKind::Ssrc {
            attenuation_input,
            min_phase,
            dither_id_input,
            pdf_type_input,
        } => {
            match focus {
                FormatSettingsFocus::SsrcAttenuation => {
                    super::text_input::handle_text_input_key(attenuation_input, key);
                }
                FormatSettingsFocus::SsrcMinPhase => {
                    if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                        *min_phase = !*min_phase;
                    }
                }
                FormatSettingsFocus::SsrcDitherId => {
                    super::text_input::handle_text_input_key(dither_id_input, key);
                }
                FormatSettingsFocus::SsrcPdf => {
                    super::text_input::handle_text_input_key(pdf_type_input, key);
                }
                _ => {}
            }
        }
        FormatSettingsKind::Sox {
            chebyshev,
            bandwidth_input,
            phase_input,
            allow_aliasing,
            sinc_taps_input,
            sinc_attenuation_input,
            sinc_passband_input,
            sinc_transition_input,
            sinc_kaiser_beta_input,
            sinc_phase,
        } => {
            use tonepoet_pipeline::enums::SoxSincPhase;
            match focus {
                FormatSettingsFocus::SoxChebyshev => {
                    if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                        *chebyshev = !*chebyshev;
                    }
                }
                FormatSettingsFocus::SoxBandwidth => {
                    super::text_input::handle_text_input_key(bandwidth_input, key);
                }
                FormatSettingsFocus::SoxPhase => {
                    super::text_input::handle_text_input_key(phase_input, key);
                }
                FormatSettingsFocus::SoxAliasing => {
                    if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                        *allow_aliasing = !*allow_aliasing;
                    }
                }
                FormatSettingsFocus::SoxSincTaps => {
                    super::text_input::handle_text_input_key(sinc_taps_input, key);
                }
                FormatSettingsFocus::SoxSincAttenuation => {
                    super::text_input::handle_text_input_key(sinc_attenuation_input, key);
                }
                FormatSettingsFocus::SoxSincPassband => {
                    super::text_input::handle_text_input_key(sinc_passband_input, key);
                }
                FormatSettingsFocus::SoxSincTransition => {
                    super::text_input::handle_text_input_key(sinc_transition_input, key);
                }
                FormatSettingsFocus::SoxSincKaiserBeta => {
                    super::text_input::handle_text_input_key(sinc_kaiser_beta_input, key);
                }
                FormatSettingsFocus::SoxSincPhase => {
                    if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                        let phases = [None, Some(SoxSincPhase::Linear), Some(SoxSincPhase::Minimum), Some(SoxSincPhase::Intermediate)];
                        let cur = phases.iter().position(|p| p == sinc_phase).unwrap_or(0);
                        let next = if key.code == KeyCode::Left {
                            if cur == 0 { phases.len() - 1 } else { cur - 1 }
                        } else {
                            (cur + 1) % phases.len()
                        };
                        *sinc_phase = phases[next];
                    }
                }
                _ => {}
            }
        }
        FormatSettingsKind::Soxr {
            chebyshev,
            cutoff_input,
            phase_input,
        } => match focus {
            FormatSettingsFocus::SoxrChebyshev => {
                if matches!(key.code, KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')) {
                    *chebyshev = !*chebyshev;
                }
            }
            FormatSettingsFocus::SoxrCutoff => {
                super::text_input::handle_text_input_key(cutoff_input, key);
            }
            FormatSettingsFocus::SoxrPhase => {
                super::text_input::handle_text_input_key(phase_input, key);
            }
            _ => {}
        },
    }
}

/// Fully self-contained mouse handler for the FormatSettings overlay.
/// Handles outside-click-close, content pill clicks, and footer pills
/// without delegating to handle_generic_overlay_mouse.
fn handle_format_settings_mouse(
    app: &mut AppState,
    mouse: MouseEvent,
    tx: &mpsc::Sender<AppMessage>,
) {
    let term = crossterm::terminal::size().unwrap_or((80, 24));

    // Scroll wheel in help mode.
    if matches!(mouse.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) {
        if let ActiveOverlay::FormatSettings { help_scroll: Some(ref mut scroll), ref kind, .. } = app.active_overlay {
            let total = super::draw_overlays::format_settings_help_line_count(kind);
            let popup = super::draw_overlays::format_settings_help_popup_rect(term.0, term.1);
            let visible = popup.height.saturating_sub(3) as usize; // borders(2) + footer(1)
            let max_scroll = total.saturating_sub(visible);
            match mouse.kind {
                MouseEventKind::ScrollUp => *scroll = scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => *scroll = (*scroll + 3).min(max_scroll),
                _ => {}
            }
        }
        return;
    }

    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return; // only left clicks matter
    }

    let mx = mouse.column;
    let my = mouse.row;

    // Compute popup geometry (must match draw_format_settings + hint/rect).
    let (min_w, field_count) = if let ActiveOverlay::FormatSettings { ref kind, .. } = app.active_overlay {
        (
            super::draw_overlays::format_settings_min_width(kind),
            super::draw_overlays::format_settings_field_count(kind),
        )
    } else {
        (50, 3)
    };
    let is_sox = matches!(
        app.active_overlay,
        ActiveOverlay::FormatSettings { kind: FormatSettingsKind::Sox { .. }, .. }
    );
    let is_help = matches!(
        app.active_overlay,
        ActiveOverlay::FormatSettings { help_scroll: Some(_), .. }
    );
    // Help popup uses shared geometry helper; controls popup uses per-kind sizing.
    let (popup_x, popup_y, popup_w, popup_h) = if is_help {
        let r = super::draw_overlays::format_settings_help_popup_rect(term.0, term.1);
        (r.x, r.y, r.width, r.height)
    } else {
        let w = term.0.saturating_sub(4).min(min_w);
        let h = if is_sox { 17 } else { field_count + 6 };
        let x = (term.0.saturating_sub(w)) / 2;
        let y = (term.1.saturating_sub(h)) / 2;
        (x, y, w, h)
    };

    let in_popup = mx >= popup_x
        && mx < popup_x + popup_w
        && my >= popup_y
        && my < popup_y + popup_h;

    if !in_popup {
        app.active_overlay = ActiveOverlay::None;
        return;
    }

    // Check button_map for overlay pills.
    if let Some(button) = app.button_map.find_button_at(mx, my) {
        if let ActiveOverlay::FormatSettings { ref mut kind, .. } = app.active_overlay {
            match (&mut *kind, button) {
                (FormatSettingsKind::Flac { ref mut verify, .. }, TuiButton::FormatSettingsVerify(i)) => {
                    *verify = i == 1;
                    return;
                }
                (FormatSettingsKind::Flac { ref mut md5, .. }, TuiButton::FormatSettingsMd5(i)) => {
                    *md5 = i == 0;
                    return;
                }
                (FormatSettingsKind::Aac { ref mut profile, ref mut quality_preset, ref mut bitrate_input }, TuiButton::FormatSettingsAacProfile(i)) => {
                    use tonepoet_pipeline::enums::AacProfile;
                    let profiles = [AacProfile::LcAac, AacProfile::HeAac, AacProfile::HeAacV2];
                    if let Some(&p) = profiles.get(i) {
                        *profile = p;
                        let presets = aac_presets_for_profile(p);
                        let high_idx = presets.iter().position(|(_, l)| *l == "high").unwrap_or(0);
                        *quality_preset = Some(high_idx);
                        bitrate_input.text = presets[high_idx].0.to_string();
                        bitrate_input.cursor = bitrate_input.text.len();
                    }
                    return;
                }
                (FormatSettingsKind::Aac { ref mut quality_preset, ref mut bitrate_input, profile, .. }, TuiButton::FormatSettingsAacQuality(i)) => {
                    let presets = aac_presets_for_profile(*profile);
                    if i < presets.len() {
                        *quality_preset = Some(i);
                        bitrate_input.text = presets[i].0.to_string();
                        bitrate_input.cursor = bitrate_input.text.len();
                    }
                    return;
                }
                (FormatSettingsKind::Opus { ref mut content_type, .. }, TuiButton::FormatSettingsOpusContentType(i)) => {
                    use tonepoet_pipeline::enums::OpusContentType;
                    let types = [OpusContentType::Auto, OpusContentType::Music, OpusContentType::Speech];
                    if let Some(&t) = types.get(i) {
                        *content_type = t;
                    }
                    return;
                }
                (FormatSettingsKind::Opus { ref mut quality_preset, ref mut bitrate_input, .. }, TuiButton::FormatSettingsOpusQuality(i)) => {
                    if i < OPUS_PRESETS.len() {
                        *quality_preset = Some(i);
                        bitrate_input.text = OPUS_PRESETS[i].0.to_string();
                        bitrate_input.cursor = bitrate_input.text.len();
                    }
                    return;
                }
                (FormatSettingsKind::Mp3 { ref mut mode, .. }, TuiButton::FormatSettingsMp3Mode(i)) => {
                    use tonepoet_pipeline::enums::Mp3Mode;
                    let modes = [Mp3Mode::Vbr, Mp3Mode::Cbr, Mp3Mode::Abr];
                    if let Some(&m) = modes.get(i) {
                        *mode = m;
                    }
                    return;
                }
                (FormatSettingsKind::Mp3 { ref mut quality_preset, ref mut bitrate_input, .. }, TuiButton::FormatSettingsMp3Preset(i)) => {
                    if i < MP3_BITRATE_PRESETS.len() {
                        *quality_preset = Some(i);
                        bitrate_input.text = MP3_BITRATE_PRESETS[i].0.to_string();
                        bitrate_input.cursor = bitrate_input.text.len();
                    }
                    return;
                }
                (FormatSettingsKind::WavPack { ref mut mode, .. }, TuiButton::FormatSettingsWavPackMode(i)) => {
                    use tonepoet_pipeline::enums::WavPackMode;
                    let modes = [WavPackMode::Fast, WavPackMode::Normal, WavPackMode::High, WavPackMode::VeryHigh];
                    if let Some(&m) = modes.get(i) {
                        *mode = m;
                    }
                    return;
                }
                (FormatSettingsKind::WavPack { ref mut hybrid, .. }, TuiButton::FormatSettingsWavPackHybrid(i)) => {
                    *hybrid = i == 1; // 0 = off, 1 = on
                    return;
                }
                (FormatSettingsKind::WavPack { ref mut correction, .. }, TuiButton::FormatSettingsWavPackCorrection(i)) => {
                    *correction = i == 1;
                    return;
                }
                (FormatSettingsKind::Ssrc { ref mut min_phase, .. }, TuiButton::FormatSettingsSsrcMinPhase(i)) => {
                    *min_phase = i == 1;
                    return;
                }
                (FormatSettingsKind::Sox { ref mut chebyshev, .. }, TuiButton::FormatSettingsSoxChebyshev(i)) => {
                    *chebyshev = i == 1;
                    return;
                }
                (FormatSettingsKind::Sox { ref mut allow_aliasing, .. }, TuiButton::FormatSettingsSoxAliasing(i)) => {
                    *allow_aliasing = i == 1;
                    return;
                }
                (FormatSettingsKind::Sox { ref mut sinc_phase, .. }, TuiButton::FormatSettingsSoxSincPhase(i)) => {
                    use tonepoet_pipeline::enums::SoxSincPhase;
                    let phases = [Some(SoxSincPhase::Linear), Some(SoxSincPhase::Minimum), Some(SoxSincPhase::Intermediate)];
                    if let Some(p) = phases.get(i) {
                        *sinc_phase = *p;
                    }
                    return;
                }
                (FormatSettingsKind::Soxr { ref mut chebyshev, .. }, TuiButton::FormatSettingsSoxrChebyshev(i)) => {
                    *chebyshev = i == 1;
                    return;
                }
                _ => {}
            }
        }
    }

    // Click-to-focus: clicking on a field row sets focus to that field.
    // Skip in help mode — help overlay has no interactive fields.
    if is_help {
        // Fall through to footer check below.
    } else {
    let field1_y = popup_y + 2;
    let field2_y = popup_y + 3;
    let field3_y = popup_y + 4;
    let field4_y = popup_y + 5;
    if let ActiveOverlay::FormatSettings { ref kind, ref mut focus, .. } = app.active_overlay {
        let new_focus = match kind {
            FormatSettingsKind::Flac { .. } => match my {
                y if y == field1_y => Some(FormatSettingsFocus::Compression),
                y if y == field2_y => Some(FormatSettingsFocus::Verify),
                y if y == field3_y => Some(FormatSettingsFocus::Md5),
                _ => None,
            },
            FormatSettingsKind::Aac { .. } => match my {
                y if y == field1_y => Some(FormatSettingsFocus::AacProfile),
                y if y == field2_y => Some(FormatSettingsFocus::AacQuality),
                y if y == field3_y => Some(FormatSettingsFocus::AacBitrate),
                _ => None,
            },
            FormatSettingsKind::Opus { .. } => match my {
                y if y == field1_y => Some(FormatSettingsFocus::OpusContentType),
                y if y == field2_y => Some(FormatSettingsFocus::OpusQuality),
                y if y == field3_y => Some(FormatSettingsFocus::OpusBitrate),
                y if y == field4_y => Some(FormatSettingsFocus::OpusComplexity),
                _ => None,
            },
            FormatSettingsKind::Mp3 { mode, .. } => {
                use tonepoet_pipeline::enums::Mp3Mode;
                let is_vbr = *mode == Mp3Mode::Vbr;
                match my {
                    y if y == field1_y => Some(FormatSettingsFocus::Mp3Mode),
                    y if y == field2_y && is_vbr => Some(FormatSettingsFocus::Mp3VbrQuality),
                    y if y == field3_y && !is_vbr => Some(FormatSettingsFocus::Mp3Preset),
                    y if y == field4_y && !is_vbr => Some(FormatSettingsFocus::Mp3Bitrate),
                    _ => None,
                }
            }
            FormatSettingsKind::WavPack { hybrid, .. } => {
                match my {
                    y if y == field1_y => Some(FormatSettingsFocus::WavPackMode),
                    y if y == field2_y => Some(FormatSettingsFocus::WavPackHybrid),
                    y if y == field3_y && *hybrid => Some(FormatSettingsFocus::WavPackBitrate),
                    y if y == field4_y && *hybrid => Some(FormatSettingsFocus::WavPackCorrection),
                    _ => None,
                }
            }
            FormatSettingsKind::Ssrc { .. } => {
                let _field5_y = popup_y + 6;
                match my {
                    y if y == field1_y => Some(FormatSettingsFocus::SsrcAttenuation),
                    y if y == field2_y => Some(FormatSettingsFocus::SsrcMinPhase),
                    y if y == field3_y => Some(FormatSettingsFocus::SsrcPdf),
                    _ => None,
                }
            }
            FormatSettingsKind::Sox { chebyshev, .. } => {
                // Rate fields at popup_y+2..+5, sinc fields at popup_y+8..+13 (offset by blank+header)
                let sinc1_y = popup_y + 8;
                let sinc2_y = popup_y + 9;
                let sinc3_y = popup_y + 10;
                let sinc4_y = popup_y + 11;
                let sinc5_y = popup_y + 12;
                let sinc6_y = popup_y + 13;
                match my {
                    y if y == field1_y => Some(FormatSettingsFocus::SoxChebyshev),
                    y if y == field2_y && !*chebyshev => Some(FormatSettingsFocus::SoxBandwidth),
                    y if y == field3_y => Some(FormatSettingsFocus::SoxPhase),
                    y if y == field4_y => Some(FormatSettingsFocus::SoxAliasing),
                    y if y == sinc1_y => Some(FormatSettingsFocus::SoxSincTaps),
                    y if y == sinc2_y => Some(FormatSettingsFocus::SoxSincAttenuation),
                    y if y == sinc3_y => Some(FormatSettingsFocus::SoxSincPassband),
                    y if y == sinc4_y => Some(FormatSettingsFocus::SoxSincTransition),
                    y if y == sinc5_y => Some(FormatSettingsFocus::SoxSincKaiserBeta),
                    y if y == sinc6_y => Some(FormatSettingsFocus::SoxSincPhase),
                    _ => None,
                }
            }
            FormatSettingsKind::Soxr { .. } => match my {
                y if y == field1_y => Some(FormatSettingsFocus::SoxrChebyshev),
                y if y == field2_y => Some(FormatSettingsFocus::SoxrCutoff),
                y if y == field3_y => Some(FormatSettingsFocus::SoxrPhase),
                _ => None,
            },
        };
        if let Some(f) = new_focus {
            *focus = f;
            return;
        }
    }
    } // end of !is_help click-to-focus block

    // Footer row Y. Must match the rendered footer position for each overlay mode.
    // Help: footer is last inner row = popup_y + popup_h - 2 (2 = bottom border + footer itself).
    // Normal/Sox: explicit layout positions.
    let footer_y = if is_help {
        popup_y + popup_h - 2
    } else if is_sox {
        // Sox: footer at inner row 14 (blank + 4 + blank + header + 6 + blank + footer)
        popup_y + 1 + 14
    } else {
        // Generic: footer at inner row field_count + 2
        popup_y + 1 + field_count + 2
    };
    if my == footer_y {
        if is_help {
            // Help footer: single "Esc close" pill — any click returns to controls
            let fake = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
            handle_overlay_key(app, fake, tx);
        } else {
            // Normal footer: thirds for Enter / Esc / ?
            let third = popup_w / 3;
            let rel_x = mx.saturating_sub(popup_x);
            if rel_x < third {
                let fake = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
                handle_overlay_key(app, fake, tx);
            } else if rel_x < third * 2 {
                let fake = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
                handle_overlay_key(app, fake, tx);
            } else {
                let fake = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE);
                handle_overlay_key(app, fake, tx);
            }
        }
    }
    // Any other in-popup click is silently consumed.
}

fn handle_template_builder_mouse(app: &mut AppState, mouse: MouseEvent) {
    use crossterm::event::{MouseButton, MouseEventKind};
    let mx = mouse.column;
    let my = mouse.row;

    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return;
    }

    let mut state = match std::mem::replace(&mut app.active_overlay, ActiveOverlay::None) {
        ActiveOverlay::TemplateBuilder(s) => s,
        other => {
            app.active_overlay = other;
            return;
        }
    };

    match app.button_map.find_button_at(mx, my) {
        Some(TuiButton::TemplateBuilderToken(idx)) => {
            state.grid_cursor = idx;
            state.insert_current_grid_item();
            state.focus = TemplateBuilderFocus::TemplateInput;
        }
        Some(TuiButton::TemplateBuilderSavedItem(idx)) => {
            if let Some(tmpl) = state.saved_templates.get(idx).cloned() {
                state.template_input = super::text_input::TextInputState::new(tmpl);
                state.template_input.cursor = state.template_input.text.len();
                state.saved_selected = idx;
                state.focus = TemplateBuilderFocus::TemplateInput;
            }
        }
        Some(TuiButton::TemplateBuilderApply) => {
            let text = state.template_input.text.clone();
            match state.target {
                TemplateTarget::Folder => {
                    app.convert.output_options.folder_template = text;
                }
                TemplateTarget::Filename => {
                    app.convert.output_options.filename_template = text;
                }
            }
            app.preset.mark_modified();
            app.set_status("Template applied");
            return; // overlay already set to None
        }
        Some(TuiButton::TemplateBuilderSave) => {
            let text = state.template_input.text.clone();
            match super::template_builder::save_template(state.target, &text) {
                Ok(()) => {
                    state.saved_templates = super::template_builder::list_templates(state.target);
                    app.set_status("Template saved");
                }
                Err(e) => {
                    app.set_status(format!("Save failed: {}", e));
                }
            }
        }
        Some(TuiButton::TemplateBuilderClear) => {
            state.template_input = super::text_input::TextInputState::empty();
        }
        Some(TuiButton::TemplateBuilderDelete) => {
            if let Some(tmpl) = state.saved_templates.get(state.saved_selected).cloned() {
                let _ = super::template_builder::delete_template(state.target, &tmpl);
                state.saved_templates = super::template_builder::list_templates(state.target);
                if state.saved_selected >= state.saved_templates.len() && state.saved_selected > 0 {
                    state.saved_selected -= 1;
                }
                app.set_status("Template deleted");
            }
        }
        _ => {
            // Click outside popup or on non-button area — close.
            // Compute bounds matching draw_template_builder's layout.
            let area = crossterm::terminal::size().unwrap_or((80, 24));
            let tw = (area.0 * 80 / 100).max(60).min(area.0.saturating_sub(2));
            let categories = state.token_categories();
            let saved_visible = state.saved_templates.len().min(4).max(1);
            let category_rows: u16 = categories.iter().map(|_| 2).sum();
            let content_height =
                1 + 2 + 1 + 1 + saved_visible as u16 + 1 + category_rows + 2 + 1 + 1;
            let th = (content_height + 2).min(area.1.saturating_sub(2));
            let px = (area.0.saturating_sub(tw)) / 2;
            let py = (area.1.saturating_sub(th)) / 2;
            if tw < 60 || th < 12 {
                return; // terminal too small for popup
            }
            if mx < px || mx >= px + tw || my < py || my >= py + th {
                return; // click outside — overlay already set to None
            }
            // Click inside but not on a button — switch focus to template input
            state.focus = TemplateBuilderFocus::TemplateInput;
        }
    }

    app.active_overlay = ActiveOverlay::TemplateBuilder(state);
}

/// Mouse handler for the CuePreview overlay.
fn handle_cue_preview_mouse(app: &mut AppState, mouse: MouseEvent, tx: &mpsc::Sender<AppMessage>) {
    let mut state = match std::mem::replace(&mut app.active_overlay, ActiveOverlay::None) {
        ActiveOverlay::CuePreview(s) => s,
        other => {
            app.active_overlay = other;
            return;
        }
    };
    let mx = mouse.column;
    let my = mouse.row;

    let area = crossterm::terminal::size().unwrap_or((80, 24));
    let w = ((area.0 as u32 * 80 / 100) as u16)
        .max(60)
        .min(area.0.saturating_sub(2));
    let h = ((area.1 as u32 * 80 / 100) as u16)
        .max(15)
        .min(area.1.saturating_sub(2));
    let x = (area.0.saturating_sub(w)) / 2;
    let y = (area.1.saturating_sub(h)) / 2;
    let in_popup = mx >= x && mx < x + w && my >= y && my < y + h;
    let is_editing = state.is_editing();

    // Helper to park state and dispatch a command (for footer pills /
    // context menu actions that consume `pending_cue_preview`).
    fn park_and_run(
        app: &mut AppState,
        state: Box<super::app::CuePreviewState>,
        cmd: super::command::Command,
        tx: &mpsc::Sender<AppMessage>,
    ) {
        app.pending_cue_preview = Some(state);
        super::command::execute_command(app, cmd, tx);
    }

    match mouse.kind {
        MouseEventKind::ScrollUp => {
            state.scroll = state.scroll.saturating_sub(1);
            app.active_overlay = ActiveOverlay::CuePreview(state);
        }
        MouseEventKind::ScrollDown => {
            let max_line = state.content.lines().count().saturating_sub(1);
            state.scroll = state.scroll.saturating_add(1).min(max_line);
            app.active_overlay = ActiveOverlay::CuePreview(state);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if is_editing {
                // In edit mode, only the two edit pills are clickable.
                match app.button_map.find_button_at(mx, my) {
                    Some(super::button_map::TuiButton::CuePreviewEditCommit) => {
                        state.commit_edit();
                        app.active_overlay = ActiveOverlay::CuePreview(state);
                    }
                    Some(super::button_map::TuiButton::CuePreviewEditCancel) => {
                        state.cancel_edit();
                        app.active_overlay = ActiveOverlay::CuePreview(state);
                    }
                    _ => {
                        if !in_popup {
                            // Click outside: cancel the edit and close.
                            state.cancel_edit();
                            app.set_status("CUE preview cancelled".to_string());
                        } else {
                            app.active_overlay = ActiveOverlay::CuePreview(state);
                        }
                    }
                }
                return;
            }
            // In read-only, the Cancel/Close pill closes via the
            // restoring helper (so the parked metadata editor comes
            // back), and Save is gated to a no-op.
            if state.read_only {
                match app.button_map.find_button_at(mx, my) {
                    Some(super::button_map::TuiButton::CuePreviewCancel) => {
                        drop(state);
                        close_cue_preview_restoring_parked(app);
                        app.set_status("CUE preview closed".to_string());
                        return;
                    }
                    Some(super::button_map::TuiButton::CuePreviewTop) => {
                        state.scroll = 0;
                        app.active_overlay = ActiveOverlay::CuePreview(state);
                        return;
                    }
                    Some(super::button_map::TuiButton::CuePreviewBottom) => {
                        let last = state.content.lines().count().saturating_sub(1);
                        state.scroll = last;
                        app.active_overlay = ActiveOverlay::CuePreview(state);
                        return;
                    }
                    Some(super::button_map::TuiButton::CuePreviewSave) => {
                        // Pill isn't drawn in read-only; defense in depth.
                        app.set_status("CUE preview is read-only".to_string());
                        app.active_overlay = ActiveOverlay::CuePreview(state);
                        return;
                    }
                    _ => {}
                }
                // Read-only fall-through: any other left-click in popup
                // is a no-op; outside-popup is handled by the default
                // arm below.
            }
            match app.button_map.find_button_at(mx, my) {
                Some(super::button_map::TuiButton::CuePreviewSave) => {
                    park_and_run(app, state, super::command::Command::Write, tx);
                }
                Some(super::button_map::TuiButton::CuePreviewCancel) => {
                    park_and_run(app, state, super::command::Command::Quit, tx);
                }
                Some(super::button_map::TuiButton::CuePreviewTop) => {
                    park_and_run(app, state, super::command::Command::CueScrollTop, tx);
                }
                Some(super::button_map::TuiButton::CuePreviewBottom) => {
                    park_and_run(app, state, super::command::Command::CueScrollBottom, tx);
                }
                Some(super::button_map::TuiButton::CuePreviewLine(idx)) => {
                    let total = state.content.lines().count();
                    if idx < total {
                        let now = std::time::Instant::now();
                        let is_double = state
                            .last_click
                            .map(|(prev, t)| {
                                prev == idx
                                    && now.duration_since(t) < std::time::Duration::from_millis(500)
                            })
                            .unwrap_or(false);
                        if is_double {
                            // Double-click → :e <idx+1>.
                            state.last_click = None;
                            park_and_run(
                                app,
                                state,
                                super::command::Command::CueEditLine(idx + 1),
                                tx,
                            );
                            return;
                        }
                        state.last_click = Some((idx, now));
                    }
                    app.active_overlay = ActiveOverlay::CuePreview(state);
                }
                _ => {
                    if !in_popup {
                        // Click outside popup: close (restoring any
                        // parked metadata editor — the `[view]` pill
                        // from a synthetic-preview row sets that).
                        drop(state);
                        close_cue_preview_restoring_parked(app);
                        app.set_status("CUE preview cancelled".to_string());
                    } else {
                        app.active_overlay = ActiveOverlay::CuePreview(state);
                    }
                }
            }
        }
        MouseEventKind::Down(MouseButton::Right) => {
            // In read-only there's no actionable context menu; treat
            // right-click as Esc (close + restore parked metadata
            // editor). Otherwise fall through to the normal context-
            // menu open.
            if state.read_only {
                drop(state);
                close_cue_preview_restoring_parked(app);
                app.set_status("CUE preview cancelled".to_string());
                return;
            }
            if !in_popup {
                drop(state);
                close_cue_preview_restoring_parked(app);
                app.set_status("CUE preview cancelled".to_string());
                return;
            }
            // If right-click landed on a content line, pass the index
            // into the menu builder; the "Edit this line" entry carries
            // it in the action variant (no parked-state mutation).
            let mut line_idx: Option<usize> = None;
            if let Some(super::button_map::TuiButton::CuePreviewLine(idx)) =
                app.button_map.find_button_at(mx, my)
            {
                let total = state.content.lines().count();
                if idx < total {
                    line_idx = Some(idx);
                }
            }
            let entries = build_cue_preview_context_menu(line_idx, is_editing);
            app.pending_cue_preview = Some(state);
            app.active_overlay = ActiveOverlay::ContextMenu {
                levels: vec![super::context_menu::MenuLevel::new(entries)],
                origin: (mx, my),
            };
        }
        _ => {
            app.active_overlay = ActiveOverlay::CuePreview(state);
        }
    }
}

/// Build the row-level context menu for the MbSelect picker.
fn build_mb_select_context_menu() -> Vec<super::context_menu::ContextMenuEntry> {
    use super::context_menu::{ContextAction, ContextMenuEntry, ContextMenuItem};
    vec![
        ContextMenuEntry::Item(ContextMenuItem {
            label: "Accept this match".to_string(),
            action: ContextAction::MbSelectAcceptCurrent,
            shortcut: None,
            enabled: true,
        }),
        ContextMenuEntry::Item(ContextMenuItem {
            label: "Cancel picker".to_string(),
            action: ContextAction::MbSelectCancelPicker,
            shortcut: None,
            enabled: true,
        }),
    ]
}

/// Mouse handler for the MbSelect overlay. Handles row click (select),
/// double-click (accept), right-click (context menu), footer pill clicks,
/// and click-outside-to-cancel.
fn handle_mb_select_mouse(app: &mut AppState, mouse: MouseEvent, tx: &mpsc::Sender<AppMessage>) {
    let mut state = match std::mem::replace(&mut app.active_overlay, ActiveOverlay::None) {
        ActiveOverlay::MbSelect(s) => s,
        other => {
            app.active_overlay = other;
            return;
        }
    };
    let mx = mouse.column;
    let my = mouse.row;

    // Compute popup geometry (must mirror draw_mb_select).
    let area = crossterm::terminal::size().unwrap_or((80, 24));
    let w = ((area.0 as u32 * 80 / 100) as u16)
        .max(60)
        .min(area.0.saturating_sub(2));
    let h = ((area.1 as u32 * 70 / 100) as u16)
        .max(12)
        .min(area.1.saturating_sub(2));
    let x = (area.0.saturating_sub(w)) / 2;
    let y = (area.1.saturating_sub(h)) / 2;
    let in_popup = mx >= x && mx < x + w && my >= y && my < y + h;

    match mouse.kind {
        MouseEventKind::ScrollUp => {
            state.selected = state.selected.saturating_sub(1);
            prefetch_current_mb_row(tx, &state, &app.db);
            app.active_overlay = ActiveOverlay::MbSelect(state);
        }
        MouseEventKind::ScrollDown => {
            let n = state.releases.len();
            state.selected = (state.selected + 1).min(n.saturating_sub(1));
            prefetch_current_mb_row(tx, &state, &app.db);
            app.active_overlay = ActiveOverlay::MbSelect(state);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // Footer Accept/Cancel pills (registered in button_map).
            match app.button_map.find_button_at(mx, my) {
                Some(super::button_map::TuiButton::MbSelectAccept) => {
                    let idx = state.selected;
                    if idx < state.releases.len() {
                        let releases = std::mem::take(&mut state.releases);
                        let paths = std::mem::take(&mut state.paths);
                        super::event_loop::open_editor_with_mb_release(app, releases, idx, paths);
                    }
                    return;
                }
                Some(super::button_map::TuiButton::MbSelectCancel) => {
                    super::event_loop::restore_parked_editor(app);
                    app.set_status("MusicBrainz picker cancelled".to_string());
                    return;
                }
                Some(super::button_map::TuiButton::MbSelectRow(idx)) => {
                    if idx < state.releases.len() {
                        // Double-click within ~500ms on the same row → accept.
                        let now = std::time::Instant::now();
                        let is_double = state
                            .last_click
                            .map(|(prev_idx, t)| {
                                prev_idx == idx
                                    && now.duration_since(t) < std::time::Duration::from_millis(500)
                            })
                            .unwrap_or(false);
                        state.selected = idx;
                        if is_double {
                            let releases = std::mem::take(&mut state.releases);
                            let paths = std::mem::take(&mut state.paths);
                            super::event_loop::open_editor_with_mb_release(
                                app, releases, idx, paths,
                            );
                            return;
                        }
                        state.last_click = Some((idx, now));
                        prefetch_current_mb_row(tx, &state, &app.db);
                        app.active_overlay = ActiveOverlay::MbSelect(state);
                    } else {
                        app.active_overlay = ActiveOverlay::MbSelect(state);
                    }
                }
                _ => {
                    if !in_popup {
                        // Click outside popup → cancel.
                        super::event_loop::restore_parked_editor(app);
                        app.set_status("MusicBrainz picker cancelled".to_string());
                    } else {
                        app.active_overlay = ActiveOverlay::MbSelect(state);
                    }
                }
            }
        }
        MouseEventKind::Down(MouseButton::Right) => {
            if let Some(super::button_map::TuiButton::MbSelectRow(idx)) =
                app.button_map.find_button_at(mx, my)
            {
                if idx < state.releases.len() {
                    state.selected = idx;
                    // Fire a prefetch for the just-selected row so the
                    // tracks pane is ready if the user closes the
                    // context menu and lands back on the picker.
                    prefetch_current_mb_row(tx, &state, &app.db);
                }
            }
            if in_popup {
                let entries = build_mb_select_context_menu();
                app.pending_mb_select = Some(state);
                app.active_overlay = ActiveOverlay::ContextMenu {
                    levels: vec![super::context_menu::MenuLevel::new(entries)],
                    origin: (mx, my),
                };
            } else {
                super::event_loop::restore_parked_editor(app);
                app.set_status("MusicBrainz picker cancelled".to_string());
            }
        }
        _ => {
            app.active_overlay = ActiveOverlay::MbSelect(state);
        }
    }
}

fn handle_metadata_editor_mouse(
    app: &mut AppState,
    mouse: MouseEvent,
    _tx: &mpsc::Sender<AppMessage>,
) {
    use super::app::MetadataEditorPhase;

    // Compute overlay geometry (must match draw_metadata_editor).
    let area = crossterm::terminal::size().unwrap_or((80, 24));
    let w = ((area.0 as usize) * 85 / 100)
        .max(50)
        .min(area.0 as usize - 2) as u16;
    let h = ((area.1 as usize) * 85 / 100)
        .max(14)
        .min(area.1 as usize - 2) as u16;
    let popup_x = (area.0.saturating_sub(w)) / 2;
    let popup_y = (area.1.saturating_sub(h)) / 2;
    // Inner content area (inside border + footer).
    let inner_x = popup_x + 1;
    let inner_y = popup_y + 1;
    let inner_w = w.saturating_sub(2);
    let inner_h = h.saturating_sub(2);
    let content_y = inner_y;
    let content_h = inner_h.saturating_sub(1) as usize; // -1 for footer row
    let footer_y = inner_y + inner_h.saturating_sub(1);

    let mx = mouse.column;
    let my = mouse.row;

    // Region checks.
    let in_popup = mx >= popup_x && mx < popup_x + w && my >= popup_y && my < popup_y + h;
    let _in_content = mx >= inner_x
        && mx < inner_x + inner_w
        && my >= content_y
        && my < content_y + content_h as u16;
    let _in_footer = mx >= inner_x && mx < inner_x + inner_w && my == footer_y;

    let overlay = app.active_overlay.clone();
    if let ActiveOverlay::MetadataEditor(mut state) = overlay {
        let total_rows = state.entries.len() + 1;
        let mut content_y = content_y;
        let mut content_h = content_h;
        if state.has_presentation_tabs() {
            content_y = content_y.saturating_add(1);
            content_h = content_h.saturating_sub(1);
        }
        let in_content = mx >= inner_x
            && mx < inner_x + inner_w
            && my >= content_y
            && my < content_y + content_h as u16;
        let in_footer = mx >= inner_x && mx < inner_x + inner_w && my == footer_y;

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some(super::button_map::TuiButton::MetadataEditorTab(idx)) =
                app.button_map.find_button_at(mx, my)
            {
                if state.switch_presentation_tab(idx) {
                    if let Some(label) = state.active_presentation_label() {
                        app.set_status(format!("metadata editor: {}", label));
                    }
                }
                app.active_overlay = ActiveOverlay::MetadataEditor(state);
                return;
            }
        }

        match mouse.kind {
            // Scroll wheel: navigate entries. Blocked during inline edit.
            MouseEventKind::ScrollUp if state.phase == MetadataEditorPhase::Editing => {
                state.cursor = state.cursor.saturating_sub(1);
                ensure_cursor_visible(&mut state);
                app.active_overlay = ActiveOverlay::MetadataEditor(state);
            }
            MouseEventKind::ScrollDown if state.phase == MetadataEditorPhase::Editing => {
                if state.cursor + 1 < total_rows {
                    state.cursor += 1;
                }
                ensure_cursor_visible(&mut state);
                app.active_overlay = ActiveOverlay::MetadataEditor(state);
            }

            // Detail overlay: scroll navigates per-file entries.
            MouseEventKind::ScrollUp if state.phase == MetadataEditorPhase::DetailEdit => {
                state.detail_cursor = state.detail_cursor.saturating_sub(1);
                ensure_detail_visible(&mut state);
                app.active_overlay = ActiveOverlay::MetadataEditor(state);
            }
            MouseEventKind::ScrollDown if state.phase == MetadataEditorPhase::DetailEdit => {
                let n = state
                    .entries
                    .get(state.detail_field_idx)
                    .map(|e| e.per_file_values.len())
                    .unwrap_or(state.paths.len());
                if state.detail_cursor + 1 < n {
                    state.detail_cursor += 1;
                }
                ensure_detail_visible(&mut state);
                app.active_overlay = ActiveOverlay::MetadataEditor(state);
            }

            // Right-click: cancel inline edit, or open a row-level
            // context menu in plain Editing mode.
            MouseEventKind::Down(MouseButton::Right) => {
                match state.phase {
                    MetadataEditorPhase::InlineEdit => {
                        state.edit_input = None;
                        state.phase = MetadataEditorPhase::Editing;
                        recalc_dirty(&mut state);
                        app.active_overlay = ActiveOverlay::MetadataEditor(state);
                    }
                    MetadataEditorPhase::AddingKey => {
                        state.add_key_input = None;
                        state.phase = MetadataEditorPhase::Editing;
                        recalc_dirty(&mut state);
                        app.active_overlay = ActiveOverlay::MetadataEditor(state);
                    }
                    MetadataEditorPhase::DetailEdit => {
                        // While inline-editing a per-file value, right-click
                        // cancels the edit (preserves prior nuance). While
                        // browsing per-file values, right-click opens a
                        // context menu with field-level actions.
                        if state.detail_edit.is_some() {
                            state.detail_edit = None;
                            app.active_overlay = ActiveOverlay::MetadataEditor(state);
                        } else if in_popup {
                            let entries = build_metadata_detail_context_menu(&state);
                            app.pending_metadata_editor = Some(state);
                            app.active_overlay = ActiveOverlay::ContextMenu {
                                levels: vec![super::context_menu::MenuLevel::new(entries)],
                                origin: (mx, my),
                            };
                        } else {
                            // Right-click outside popup: back out.
                            state.detail_edit = None;
                            state.phase = MetadataEditorPhase::Editing;
                            app.active_overlay = ActiveOverlay::MetadataEditor(state);
                        }
                    }
                    MetadataEditorPhase::Editing if in_content => {
                        // Compute the clicked row (entry index).
                        let row = (my - content_y) as usize + state.scroll;
                        if row < state.entries.len() {
                            state.cursor = row;
                            let entries = build_metadata_row_context_menu(&state, row);
                            app.pending_metadata_editor = Some(state);
                            app.active_overlay = ActiveOverlay::ContextMenu {
                                levels: vec![super::context_menu::MenuLevel::new(entries)],
                                origin: (mx, my),
                            };
                        } else {
                            // Right-click on the "+ Add field..." line or
                            // empty space → simple add-field menu.
                            let entries = vec![super::context_menu::ContextMenuEntry::Item(
                                super::context_menu::ContextMenuItem {
                                    label: "Add new field".to_string(),
                                    action: super::context_menu::ContextAction::MetadataAddField,
                                    shortcut: None,
                                    enabled: true,
                                },
                            )];
                            app.pending_metadata_editor = Some(state);
                            app.active_overlay = ActiveOverlay::ContextMenu {
                                levels: vec![super::context_menu::MenuLevel::new(entries)],
                                origin: (mx, my),
                            };
                        }
                    }
                    _ => {
                        // Right-click outside content area in Editing
                        // mode → close as before.
                        app.active_overlay = ActiveOverlay::None;
                    }
                }
            }

            // Left click in content: move cursor, double-click to edit.
            MouseEventKind::Down(MouseButton::Left) if in_content => {
                // Check first whether the click landed on a per-row
                // revert/use-MB pill. The pill rect is registered by
                // draw_metadata_editor; if hit, toggle and return.
                if state.phase == MetadataEditorPhase::Editing {
                    match app.button_map.find_button_at(mx, my) {
                        Some(super::button_map::TuiButton::MetadataEntryRevert(idx)) => {
                            if state.entries.get(idx).is_some() {
                                super::probe::toggle_mb_revert(&mut state.entries[idx]);
                                state.dirty = super::probe::metadata_editor_has_changes(&state);
                                state.cursor = idx;
                                ensure_cursor_visible(&mut state);
                                app.active_overlay = ActiveOverlay::MetadataEditor(state);
                                return;
                            }
                        }
                        Some(super::button_map::TuiButton::MetadataEntryView(idx)) => {
                            // Open a read-only CuePreview seeded with
                            // the row's value. Park the editor so Esc
                            // / Close pill restores it.
                            if let Some(entry) = state.entries.get(idx) {
                                let content = entry.value.clone();
                                let summary = format!(
                                    "{} (read-only · {})",
                                    entry.display_key,
                                    super::probe::cue_summary_string(&content),
                                );
                                state.cursor = idx;
                                ensure_cursor_visible(&mut state);
                                app.pending_metadata_editor = Some(state);
                                app.active_overlay = ActiveOverlay::CuePreview(Box::new(
                                    super::app::CuePreviewState::new_readonly(content, summary),
                                ));
                                return;
                            }
                        }
                        _ => {}
                    }
                }
                let row = (my - content_y) as usize + state.scroll;

                // Detail overlay: click moves detail_cursor, double-click edits.
                if state.phase == MetadataEditorPhase::DetailEdit {
                    // Recalculate row using detail_scroll (not main scroll).
                    let detail_row = (my - content_y) as usize + state.detail_scroll;
                    let header_offset = 2usize;
                    if detail_row >= header_offset {
                        let file_idx = detail_row - header_offset;
                        let field_idx = state.detail_field_idx;
                        let n_files = state
                            .entries
                            .get(field_idx)
                            .map(|e| e.per_file_values.len())
                            .unwrap_or(state.paths.len());

                        // Confirm inline edit if active.
                        if let Some(ref input) = state.detail_edit {
                            let new_val = input.text.clone();
                            if field_idx < state.entries.len() && state.detail_cursor < n_files {
                                state.entries[field_idx].per_file_values[state.detail_cursor] =
                                    new_val;
                                let all_same = state.entries[field_idx]
                                    .per_file_values
                                    .windows(2)
                                    .all(|w| w[0] == w[1]);
                                state.entries[field_idx].is_mixed = !all_same;
                                let new_display = if all_same {
                                    state.entries[field_idx].per_file_values[0].clone()
                                } else {
                                    "<multiple values>".to_string()
                                };
                                state.entries[field_idx].value = new_display;
                            }
                            state.detail_edit = None;
                            recalc_dirty(&mut state);
                        }

                        if file_idx < n_files {
                            // Double-click detection.
                            let now = std::time::Instant::now();
                            let is_double = state
                                .last_click
                                .map(|(prev, t)| {
                                    prev == file_idx && now.duration_since(t).as_millis() < 400
                                })
                                .unwrap_or(false);

                            if is_double && field_idx < state.entries.len() && !state.read_only {
                                // Open inline edit for this file's value.
                                let val =
                                    state.entries[field_idx].per_file_values[file_idx].clone();
                                state.detail_edit =
                                    Some(super::text_input::TextInputState::new(val));
                                state.detail_cursor = file_idx;
                                state.last_click = None;
                            } else if is_double && state.read_only {
                                app.set_status("read-only editor (SACD ISO)");
                                state.detail_cursor = file_idx;
                                state.last_click = None;
                            } else {
                                state.detail_cursor = file_idx;
                                state.last_click = Some((file_idx, now));
                            }
                            ensure_detail_visible(&mut state);
                        }
                    }
                    app.active_overlay = ActiveOverlay::MetadataEditor(state);
                    return;
                }

                // If currently in inline edit: clicking within the multiline
                // drop-down area repositions the cursor; clicking elsewhere
                // commits the edit.
                if state.phase == MetadataEditorPhase::InlineEdit {
                    let key_col_w = 22usize;
                    let iw = inner_w as usize;
                    let vm = iw.saturating_sub(key_col_w + 1); // val_max

                    // Calculate how many visual rows the currently-edited
                    // field occupies (1 for short, N for multiline).
                    let drop_rows = if let Some(ref input) = state.edit_input {
                        let char_count = input.text.chars().count();
                        let has_nl = input.text.contains('\n') || input.text.contains('\r');
                        if (char_count > super::draw_overlays::MULTILINE_EDIT_THRESHOLD || has_nl)
                            && vm > 0
                        {
                            let sanitized = input.text.replace("\r\n", "\n").replace('\r', "\n");
                            let mut n = 0usize;
                            for para in sanitized.split('\n') {
                                let pc = para.chars().count();
                                n += if pc == 0 { 1 } else { (pc + vm - 1) / vm };
                            }
                            n.min(8).max(1)
                        } else {
                            1
                        }
                    } else {
                        1
                    };

                    // Visual row range of the edited field within the content area.
                    let edit_visual_start = state.cursor.saturating_sub(state.scroll);
                    let edit_visual_end = edit_visual_start + drop_rows;
                    let click_visual_row = (my - content_y) as usize;

                    if click_visual_row >= edit_visual_start && click_visual_row < edit_visual_end {
                        // Click is within the drop-down area — reposition cursor.
                        // Account for drop_scroll: the visible lines may be
                        // offset within the total display rows.
                        if let Some(ref mut input) = state.edit_input {
                            // Compute drop_scroll (must match draw_metadata_editor).
                            let sanitized_tmp =
                                input.text.replace("\r\n", "\n").replace('\r', "\n");
                            let mut cur_drow = 0usize;
                            let mut cur_dcol = 0usize;
                            // Map cursor to display row (simplified: walk sanitized).
                            {
                                let mut sp = 0usize;
                                // Compute sanitized cursor pos.
                                let mut pcr = false;
                                for (bi, c) in input.text.char_indices() {
                                    if bi >= input.cursor {
                                        break;
                                    }
                                    if c == '\r' {
                                        pcr = true;
                                        continue;
                                    }
                                    if pcr {
                                        sp += if c == '\n' { 1 } else { 2 };
                                        pcr = false;
                                        continue;
                                    }
                                    sp += 1;
                                }
                                if pcr {
                                    sp += 1;
                                }
                                let mut idx = 0usize;
                                for c in sanitized_tmp.chars() {
                                    if idx == sp {
                                        break;
                                    }
                                    if c == '\n' {
                                        cur_drow += 1;
                                        cur_dcol = 0;
                                    } else {
                                        cur_dcol += 1;
                                        if cur_dcol >= vm {
                                            cur_drow += 1;
                                            cur_dcol = 0;
                                        }
                                    }
                                    idx += 1;
                                }
                            }
                            let ds = if cur_drow < drop_rows {
                                0
                            } else {
                                cur_drow - drop_rows + 1
                            };
                            let click_line = (click_visual_row - edit_visual_start) + ds;
                            let click_col =
                                (mx as usize).saturating_sub(inner_x as usize + key_col_w);
                            // Walk the sanitized text to find which char position
                            // corresponds to (click_line, click_col).
                            let sanitized = input.text.replace("\r\n", "\n").replace('\r', "\n");
                            let mut target_byte = input.text.len(); // default: end
                            let mut drow = 0usize;
                            let mut dcol = 0usize;
                            let mut orig_byte = 0usize;
                            let mut orig_iter = input.text.char_indices().peekable();
                            for sc in sanitized.chars() {
                                if drow == click_line && dcol == click_col {
                                    target_byte = orig_byte;
                                    break;
                                }
                                // Advance orig_iter past the char(s) that
                                // produced this sanitized char.
                                if let Some((bi, oc)) = orig_iter.next() {
                                    orig_byte = bi + oc.len_utf8();
                                    // If original had \r\n → single \n in sanitized,
                                    // skip the \n too.
                                    if oc == '\r' {
                                        if let Some(&(_, '\n')) = orig_iter.peek() {
                                            let (bi2, oc2) = orig_iter.next().unwrap();
                                            orig_byte = bi2 + oc2.len_utf8();
                                        }
                                    }
                                }
                                if sc == '\n' {
                                    drow += 1;
                                    dcol = 0;
                                } else {
                                    dcol += 1;
                                    if dcol >= vm {
                                        drow += 1;
                                        dcol = 0;
                                    }
                                }
                            }
                            // If we landed past the target, clamp.
                            if drow == click_line && dcol <= click_col {
                                target_byte = orig_byte;
                            }
                            input.cursor = target_byte.min(input.text.len());
                        }
                        app.active_overlay = ActiveOverlay::MetadataEditor(state);
                        return;
                    }

                    // Click is outside the drop-down — commit the edit.
                    if let Some(ref input) = state.edit_input {
                        let new_val = input.text.clone();
                        if state.cursor < state.entries.len() {
                            let entry = &mut state.entries[state.cursor];
                            entry.value = new_val.clone();
                            for v in &mut entry.per_file_values {
                                *v = new_val.clone();
                            }
                            entry.is_mixed = false;
                        }
                    }
                    state.edit_input = None;
                    state.phase = MetadataEditorPhase::Editing;
                    recalc_dirty(&mut state);
                } else if state.phase == MetadataEditorPhase::AddingKey {
                    state.add_key_input = None;
                    state.phase = MetadataEditorPhase::Editing;
                    recalc_dirty(&mut state);
                }

                // Double-click detection: same row within 400ms.
                let now = std::time::Instant::now();
                let is_double = state
                    .last_click
                    .map(|(prev_row, prev_time)| {
                        prev_row == row && now.duration_since(prev_time).as_millis() < 400
                    })
                    .unwrap_or(false);

                if is_double && row < state.entries.len() {
                    // Double-click: open detail for mixed fields, inline edit otherwise.
                    state.cursor = row;
                    let entry = &state.entries[row];
                    if !entry.is_binary && !state.deleted.contains(&row) {
                        // Mirrors the keyboard-Enter gate: use the entry's
                        // own dimension so per-track CUESHEET rows on a
                        // single-image rip can open detail.
                        if entry.is_mixed && entry.per_file_values.len() > 1 {
                            state.detail_field_idx = row;
                            state.detail_cursor = 0;
                            state.detail_scroll = 0;
                            state.detail_edit = None;
                            state.last_click = None;
                            state.phase = MetadataEditorPhase::DetailEdit;
                        } else if state.read_only {
                            app.set_status("read-only editor (SACD ISO)");
                        } else {
                            state.edit_input =
                                Some(super::text_input::TextInputState::new(entry.value.clone()));
                            state.phase = MetadataEditorPhase::InlineEdit;
                        }
                    }
                    state.last_click = None;
                } else if is_double && row == state.entries.len() {
                    if state.read_only {
                        app.set_status("read-only editor (SACD ISO)");
                    } else {
                        // Double-click on "+ Add field..."
                        state.cursor = state.entries.len();
                        state.add_key_input = Some(super::text_input::TextInputState::empty());
                        state.phase = MetadataEditorPhase::AddingKey;
                    }
                    state.last_click = None;
                } else {
                    // Single click: move cursor.
                    if row < total_rows {
                        state.cursor = row;
                    }
                    state.last_click = Some((row, now));
                }
                ensure_cursor_visible(&mut state);
                app.active_overlay = ActiveOverlay::MetadataEditor(state);
            }

            // Left click on footer: pill button hit-testing.
            MouseEventKind::Down(MouseButton::Left) if in_footer => {
                if state.phase == MetadataEditorPhase::Editing {
                    // Mirror the renderer: prepend ← MB pill when state
                    // was reached via the MbSelect picker (mb_back cache
                    // populated). Stays off the row when there's no
                    // cache to return to.
                    let mut pills: Vec<(&str, &str)> = Vec::new();
                    // Mirror render: pill action picks the right
                    // colon command based on which back-cache is set.
                    // Only one is ever populated at a time (MB vs
                    // gnudb flows are mutually exclusive on a single
                    // editor session).
                    if state.gnudb_back.is_some() {
                        pills.push(("← back", ":gnudb-back"));
                    } else if state.mb_back.is_some() {
                        pills.push(("← back", ":mb-back"));
                    }
                    // Mirror of the render list in `draw_overlays.rs`
                    // `draw_metadata_editor` Editing arm. Order and
                    // labels MUST match exactly so click hit-tests
                    // align. See `project_editor_footer_pills.md`.
                    pills.push((":tags-mb", ":tags-mb"));
                    pills.extend_from_slice(&[
                        (":fix-caps", ":fix-caps"),
                        (":d delete", ":d"),
                        (":u undo", ":u"),
                        (":a add", ":a"),
                        (":w save", ":w"),
                        ("Esc close", "esc"),
                    ]);
                    if let Some(action) = footer_pill_hit(&pills, mx, inner_x, inner_w) {
                        if action.starts_with(':') {
                            app.active_overlay = ActiveOverlay::MetadataEditor(state);
                            let cmd = super::command::parse_command(&action[1..]);
                            super::command::execute_command(app, cmd, _tx);
                            return;
                        }
                        match action {
                            "esc" => {
                                app.active_overlay = ActiveOverlay::None;
                                return;
                            }
                            _ => {}
                        }
                    }
                } else if state.phase == MetadataEditorPhase::DetailEdit {
                    // Field-level revert / restore pills (only present
                    // in browsing mode, when MB populated the field).
                    if state.detail_edit.is_none() {
                        match app.button_map.find_button_at(mx, my) {
                            Some(super::button_map::TuiButton::MetadataDetailRevert) => {
                                if let Some(entry) = state.entries.get_mut(state.detail_field_idx) {
                                    super::probe::toggle_mb_revert_field(entry);
                                }
                                state.dirty = super::probe::metadata_editor_has_changes(&state);
                                app.active_overlay = ActiveOverlay::MetadataEditor(state);
                                return;
                            }
                            Some(super::button_map::TuiButton::MetadataDetailRestore) => {
                                if let Some(entry) = state.entries.get_mut(state.detail_field_idx) {
                                    super::probe::restore_mb_proposed(entry);
                                }
                                state.dirty = super::probe::metadata_editor_has_changes(&state);
                                app.active_overlay = ActiveOverlay::MetadataEditor(state);
                                return;
                            }
                            _ => {}
                        }
                    }
                    // Dynamic pills: [:import-cue (FIELD)] if CUE-compatible,
                    // [:fix-caps] if the field is a capitalize-applicable
                    // text key, Enter, Esc. Pills are only added in
                    // browsing mode (not while inline-editing a value).
                    let cue_label: String;
                    let mut pills: Vec<(&str, &str)> = Vec::new();
                    if let Some(entry) = state.entries.get(state.detail_field_idx) {
                        if super::command::is_cue_importable(&entry.display_key) {
                            cue_label = format!(":import-cue ({})", entry.display_key);
                            pills.push((&cue_label, ":import-cue"));
                        }
                    }
                    if state.detail_edit.is_some() {
                        pills.extend_from_slice(&[
                            ("Enter confirm", "enter"),
                            ("Esc cancel", "esc"),
                        ]);
                    } else {
                        if let Some(entry) = state.entries.get(state.detail_field_idx) {
                            if is_fix_caps_applicable(&entry.display_key) {
                                pills.push((":fix-caps", ":fix-caps"));
                            }
                        }
                        pills.extend_from_slice(&[("Enter edit", "enter"), ("Esc back", "esc")]);
                    }
                    // The renderer appends extra pills (revert/restore +
                    // a 4-char gap) after the dynamic pills when the
                    // focused entry has MB-proposed values. Width-match
                    // here so the click hit-test centers identically.
                    // Without this, clicks on (e.g.) :fix-caps would
                    // land in :import-cue's range due to the misaligned
                    // start_x. revert/restore themselves are click-
                    // handled via button_map (above), not via this
                    // hit-test.
                    let extra_width = if state.detail_edit.is_none() {
                        if let Some(entry) = state.entries.get(state.detail_field_idx) {
                            if super::probe::entry_has_mb_proposed(entry) {
                                let pill_state = super::probe::mb_pill_state_field(entry);
                                let revert_chunk = match pill_state {
                                    // " revert " or " use MB " (8 chars) + 1-char gap
                                    super::probe::MbRevertPill::Revert
                                    | super::probe::MbRevertPill::UseMb => 8 + 1,
                                    super::probe::MbRevertPill::None => 0,
                                };
                                // 4-char gap before the MB-action group + revert chunk + restore pill (" restore " = 9 chars)
                                4 + revert_chunk + 9
                            } else {
                                0
                            }
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    if let Some(action) =
                        footer_pill_hit_with_extra(&pills, extra_width, mx, inner_x, inner_w)
                    {
                        if action.starts_with(':') {
                            app.active_overlay = ActiveOverlay::MetadataEditor(state);
                            let cmd = super::command::parse_command(&action[1..]);
                            super::command::execute_command(app, cmd, _tx);
                            return;
                        }
                        let fake_key = match action {
                            "enter" => KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                            "esc" => KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                            _ => {
                                app.active_overlay = ActiveOverlay::MetadataEditor(state);
                                return;
                            }
                        };
                        handle_metadata_editor_key(app, fake_key, &mut state, _tx);
                        if !matches!(app.active_overlay, ActiveOverlay::None) {
                            app.active_overlay = ActiveOverlay::MetadataEditor(state);
                        }
                        return;
                    }
                } else if state.phase == MetadataEditorPhase::InlineEdit
                    || state.phase == MetadataEditorPhase::AddingKey
                {
                    let pills: &[(&str, &str)] =
                        &[("Enter confirm", "enter"), ("Esc cancel", "esc")];
                    if let Some(action) = footer_pill_hit(pills, mx, inner_x, inner_w) {
                        match action {
                            "enter" => {
                                let fake_key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
                                handle_metadata_editor_key(app, fake_key, &mut state, _tx);
                                if !matches!(app.active_overlay, ActiveOverlay::None) {
                                    app.active_overlay = ActiveOverlay::MetadataEditor(state);
                                }
                                return;
                            }
                            "esc" => {
                                let fake_key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
                                handle_metadata_editor_key(app, fake_key, &mut state, _tx);
                                if !matches!(app.active_overlay, ActiveOverlay::None) {
                                    app.active_overlay = ActiveOverlay::MetadataEditor(state);
                                }
                                return;
                            }
                            _ => {}
                        }
                    }
                }
                app.active_overlay = ActiveOverlay::MetadataEditor(state);
            }

            // Left click outside the popup: close the overlay.
            MouseEventKind::Down(MouseButton::Left) if !in_popup => {
                app.active_overlay = ActiveOverlay::None;
            }

            // Left click inside popup but outside content/footer: ignore.
            MouseEventKind::Down(MouseButton::Left) => {
                app.active_overlay = ActiveOverlay::MetadataEditor(state);
            }

            // Ignore other events.
            _ => {
                app.active_overlay = ActiveOverlay::MetadataEditor(state);
            }
        }
    }
}

/// Hit-test a center-aligned row of pills. Returns the action string of
/// the clicked pill, or None if the click missed all pills.
/// Each pill is " label " (len+2 chars), with 1-char gaps between.
fn footer_pill_hit<'a>(
    pills: &'a [(&'a str, &'a str)],
    mx: u16,
    row_x: u16,
    row_w: u16,
) -> Option<&'a str> {
    footer_pill_hit_with_extra(pills, 0, mx, row_x, row_w)
}

/// Like `footer_pill_hit`, but accepts an `extra_width` representing
/// pills the renderer adds AFTER the click-handler's `pills` slice
/// (e.g., revert/restore action pills appended by the detail overlay
/// when an entry has MB-proposed values). The render centers based
/// on TOTAL width including those extras; without accounting for
/// them here, the click hit-rects would shift left of where the
/// rendered pills are, mis-routing clicks. Extras are NOT click-
/// tested by this function — they're handled separately by
/// `button_map`.
fn footer_pill_hit_with_extra<'a>(
    pills: &'a [(&'a str, &'a str)],
    extra_width: usize,
    mx: u16,
    row_x: u16,
    row_w: u16,
) -> Option<&'a str> {
    // Total width matches what the renderer centers against.
    let pills_w: usize = pills
        .iter()
        .map(|(label, _)| label.chars().count() + 2)
        .sum::<usize>()
        + pills.len().saturating_sub(1);
    let total_w = pills_w + extra_width;
    let start_x = row_x as usize + (row_w as usize).saturating_sub(total_w) / 2;

    let mut x = start_x;
    for (label, action) in pills {
        // Char count, not byte length — labels can contain non-ASCII
        // (e.g. "← back" is 6 chars / 8 bytes). The renderer uses
        // chars().count() for centering; mismatch here would shift
        // hit-rects.
        let pill_w = label.chars().count() + 2;
        if (mx as usize) >= x && (mx as usize) < x + pill_w {
            return Some(action);
        }
        x += pill_w + 1; // +1 for gap
    }
    None
}

/// Ensure the cursor is visible in the metadata editor's scroll window.
fn ensure_cursor_visible(state: &mut super::app::MetadataEditorState) {
    let visible = crossterm::terminal::size()
        .map(|(_, h)| (h as usize * 85 / 100).max(14).saturating_sub(4))
        .unwrap_or(20);
    if state.cursor < state.scroll {
        state.scroll = state.cursor;
    } else if state.cursor >= state.scroll + visible {
        state.scroll = state.cursor.saturating_sub(visible - 1);
    }
}

/// Ensure the detail cursor is visible in the detail overlay.
fn ensure_detail_visible(state: &mut super::app::MetadataEditorState) {
    let visible = crossterm::terminal::size()
        .map(|(_, h)| (h as usize * 85 / 100).max(14).saturating_sub(6))
        .unwrap_or(15);
    // The detail view has a 2-line header (field name + blank) before
    // per-file rows. Cursor index i maps to line i+2 in the content.
    let header_offset = 2usize;
    let cursor_line = state.detail_cursor + header_offset;
    if cursor_line < state.detail_scroll {
        state.detail_scroll = cursor_line;
    } else if cursor_line >= state.detail_scroll + visible {
        state.detail_scroll = cursor_line.saturating_sub(visible - 1);
    }
}

/// Recalculate the dirty flag by checking per-file values for changes.
fn recalc_dirty(state: &mut super::app::MetadataEditorState) {
    state.dirty = !state.deleted.is_empty()
        || state.entries.iter().any(|e| {
            e.per_file_values
                .iter()
                .zip(e.per_file_originals.iter())
                .any(|(v, o)| v != o)
        });
}

/// Cascade direction for one level relative to its parent. Each level
/// inherits its parent's direction (momentum); flips only when the
/// inherited direction doesn't fit horizontally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CascadeDir {
    Right,
    Left,
}

/// Pick where a cascaded child panel should sit horizontally given its
/// parent rect, the child's rendered width, the terminal width, and the
/// parent's cascade direction (for momentum).
///
/// Try the inherited direction first; if blocked, flip; if both
/// directions are blocked (cascade fundamentally too wide for the
/// terminal), fall back to the inherited direction with overflow — the
/// post-shift fallback in `context_menu_stack_rects` will pull the
/// stack left as best it can.
fn choose_cascade_origin(
    parent: Rect,
    child_w: u16,
    area_w: u16,
    parent_dir: CascadeDir,
) -> (u16, CascadeDir) {
    let right_x = parent.x.saturating_add(parent.width).saturating_sub(1);
    let fits_right = right_x.saturating_add(child_w) <= area_w;
    let fits_left = parent.x.saturating_add(1) >= child_w;
    // Leftward origin: child.right = parent.x + 1, so child.x = parent.x + 1 - child_w.
    let left_x = parent.x.saturating_add(1).saturating_sub(child_w);

    match parent_dir {
        CascadeDir::Right => {
            if fits_right {
                (right_x, CascadeDir::Right)
            } else if fits_left {
                (left_x, CascadeDir::Left)
            } else {
                (right_x, CascadeDir::Right)
            }
        }
        CascadeDir::Left => {
            if fits_left {
                (left_x, CascadeDir::Left)
            } else if fits_right {
                (right_x, CascadeDir::Right)
            } else {
                (left_x, CascadeDir::Left)
            }
        }
    }
}

/// Compute the cascade rects for a stack of menu levels plus an
/// optional phantom "preview" level. The preview is the children of the
/// focused level's selected entry when that entry is a Submenu — the
/// renderer / hover handler treats it as visible but it isn't part of
/// the navigated stack.
///
/// Returns:
/// - `rects`: one Rect per element, in the same order as `levels`,
///   followed by the preview rect (if any).
/// - `preview`: Some((entries, rect_index)) when a preview was added,
///   else None.
pub fn context_menu_stack_rects<'a>(
    levels: &'a [super::context_menu::MenuLevel],
    origin: (u16, u16),
    area_w: u16,
    area_h: u16,
) -> (
    Vec<Rect>,
    Option<(&'a [super::context_menu::ContextMenuEntry], usize)>,
) {
    use super::context_menu::{ContextMenuEntry, MAX_CONTEXT_MENU_DEPTH};
    let mut rects: Vec<Rect> = Vec::with_capacity(levels.len() + 1);
    if levels.is_empty() {
        return (rects, None);
    }
    // Root: clamp x to fit on screen (right-click near the right edge
    // shouldn't render off-screen). Cascaded levels do NOT clamp — we
    // post-process the whole stack with a uniform left shift instead.
    let root = context_menu_panel_rect(&levels[0].entries, origin, area_w, area_h, true);
    rects.push(root);
    // Direction tracked parallel to rects. Root is conventionally
    // Right (it has no parent; the value only matters for inheritance
    // by the next level).
    let mut dirs: Vec<CascadeDir> = vec![CascadeDir::Right];

    for i in 1..levels.len() {
        let parent_rect = rects[i - 1];
        let parent_dir = dirs[i - 1];
        let parent_entries = &levels[i - 1].entries;
        let parent_sel = levels[i - 1].selected;
        let sel_row = super::draw_overlays::selected_entry_row_pub(parent_entries, parent_sel);
        let child_w = compute_menu_w(&levels[i].entries, area_w);
        let (child_x, dir) = choose_cascade_origin(parent_rect, child_w, area_w, parent_dir);
        let sub_y = parent_rect.y + sel_row + 1;
        let r =
            context_menu_panel_rect(&levels[i].entries, (child_x, sub_y), area_w, area_h, false);
        rects.push(r);
        dirs.push(dir);
    }

    // Preview: focused level's selected entry, if it's a Submenu and
    // we have headroom in the depth cap.
    let mut preview: Option<(&[ContextMenuEntry], usize)> = None;
    if levels.len() < MAX_CONTEXT_MENU_DEPTH {
        let focused = &levels[levels.len() - 1];
        let entries = &focused.entries;
        let sel = focused.selected;
        let selectable: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match e {
                ContextMenuEntry::Item(item) if item.enabled => Some(i),
                ContextMenuEntry::Submenu { .. } => Some(i),
                _ => None,
            })
            .collect();
        if let Some(&idx) = selectable.get(sel) {
            if let ContextMenuEntry::Submenu { children, .. } = &entries[idx] {
                let parent_rect = *rects.last().unwrap();
                let parent_dir = *dirs.last().unwrap();
                let sel_row = super::draw_overlays::selected_entry_row_pub(entries, sel);
                let child_w = compute_menu_w(children, area_w);
                let (pv_x, _pv_dir) =
                    choose_cascade_origin(parent_rect, child_w, area_w, parent_dir);
                let pv_y = parent_rect.y + sel_row + 1;
                let pv_rect =
                    context_menu_panel_rect(children, (pv_x, pv_y), area_w, area_h, false);
                rects.push(pv_rect);
                preview = Some((children.as_slice(), rects.len() - 1));
            }
        }
    }

    // Width-overflow correction: if any panel extends past the right
    // edge, shift the whole stack left uniformly. With directional
    // cascade, the rightmost panel may be a *middle* level (e.g. level
    // 2 cascades right and overflows; level 3 cascades left and fits)
    // — so the trigger looks at max(right) across all rects, not just
    // the deepest. The shift is bounded by min(x) across all rects so
    // no panel is pushed off-screen left.
    let max_right = rects
        .iter()
        .map(|r| r.x.saturating_add(r.width))
        .max()
        .unwrap_or(0);
    if max_right > area_w {
        let needed = max_right - area_w;
        let max_shift = rects.iter().map(|r| r.x).min().unwrap_or(0);
        let shift = needed.min(max_shift);
        if shift > 0 {
            for r in &mut rects {
                r.x = r.x.saturating_sub(shift);
            }
        }
    }

    (rects, preview)
}

/// Compute the rendered width of a menu panel from its entries.
/// Mirrors the calculation inside [`context_menu_panel_rect`]; lifted
/// so the directional cascade can know `child_w` before choosing the
/// cascade origin.
fn compute_menu_w(entries: &[super::context_menu::ContextMenuEntry], area_w: u16) -> u16 {
    use super::context_menu::ContextMenuEntry;
    let max_label_w: usize = entries
        .iter()
        .filter_map(|e| match e {
            ContextMenuEntry::Item(item) => {
                let shortcut_w = item
                    .shortcut
                    .as_ref()
                    .map(|s| s.chars().count() + 3)
                    .unwrap_or(0);
                Some(item.label.chars().count() + shortcut_w)
            }
            ContextMenuEntry::Submenu { label, .. } => Some(label.chars().count() + 2),
            ContextMenuEntry::Separator => None,
        })
        .max()
        .unwrap_or(10);
    (max_label_w + 4).min(area_w as usize) as u16
}

/// Compute a menu panel's rect.
///
/// `clamp_x`:
/// - true (root only): clamp `x` so the panel fits within the terminal.
/// - false (cascaded levels + preview): allow `x` to overflow the right
///   edge — the caller will shift the whole stack left as needed.
fn context_menu_panel_rect(
    entries: &[super::context_menu::ContextMenuEntry],
    origin: (u16, u16),
    area_w: u16,
    area_h: u16,
    clamp_x: bool,
) -> Rect {
    let menu_w = compute_menu_w(entries, area_w);
    let menu_h = (entries.len() + 2).min(area_h as usize) as u16;
    let x = if clamp_x {
        origin.0.min(area_w.saturating_sub(menu_w))
    } else {
        origin.0
    };
    let y = origin.1.min(area_h.saturating_sub(menu_h));
    Rect::new(x, y, menu_w, menu_h)
}

/// Map a screen row to a selectable entry index within a menu panel.
/// Returns None if the row is outside the menu body or on a separator/
/// disabled item.
fn context_menu_hit_test(
    entries: &[super::context_menu::ContextMenuEntry],
    panel: Rect,
    mouse_x: u16,
    mouse_y: u16,
) -> Option<usize> {
    use super::context_menu::ContextMenuEntry;
    // Inner area: 1px border on each side.
    let inner_x = panel.x + 1;
    let inner_y = panel.y + 1;
    let inner_w = panel.width.saturating_sub(2);
    let inner_h = panel.height.saturating_sub(2);
    if mouse_x < inner_x
        || mouse_x >= inner_x + inner_w
        || mouse_y < inner_y
        || mouse_y >= inner_y + inner_h
    {
        return None;
    }
    let row = (mouse_y - inner_y) as usize;
    if row >= entries.len() {
        return None;
    }
    // Check if this row is a selectable entry.
    let mut selectable_idx = 0usize;
    for (i, e) in entries.iter().enumerate() {
        let is_selectable = matches!(e, ContextMenuEntry::Item(item) if item.enabled)
            || matches!(e, ContextMenuEntry::Submenu { .. });
        if i == row {
            return if is_selectable {
                Some(selectable_idx)
            } else {
                None
            };
        }
        if is_selectable {
            selectable_idx += 1;
        }
    }
    None
}

/// Handle mouse hover over the context menu — update selected item.
fn context_menu_mouse_hover(app: &mut AppState, mx: u16, my: u16) {
    use super::context_menu::MenuLevel;
    let area = crossterm::terminal::size().unwrap_or((80, 24));

    // Extract levels + origin without holding a borrow during mutation.
    let (mut levels, origin) = match &app.active_overlay {
        ActiveOverlay::ContextMenu { levels, origin } => (levels.clone(), *origin),
        _ => return,
    };

    let (rects, preview_owned): (
        Vec<Rect>,
        Option<(Vec<super::context_menu::ContextMenuEntry>, usize)>,
    ) = {
        let (r, p) = context_menu_stack_rects(&levels, origin, area.0, area.1);
        (r, p.map(|(es, idx)| (es.to_vec(), idx)))
    };
    let preview = preview_owned.as_ref().map(|(v, i)| (v.as_slice(), *i));

    // Hover priority: deepest panel first (preview, then innermost level).
    if let Some((preview_entries, preview_idx)) = preview {
        let pv_rect = rects[preview_idx];
        if let Some(idx) = context_menu_hit_test(preview_entries, pv_rect, mx, my) {
            // Hovering the preview promotes it to a real focused level
            // (truncate any deeper levels — none exist now since preview
            // sat after the focused level — and push the preview).
            if levels.len() < super::context_menu::MAX_CONTEXT_MENU_DEPTH {
                let mut new_level = MenuLevel::new(preview_entries.to_vec());
                new_level.selected = idx;
                levels.push(new_level);
                app.active_overlay = ActiveOverlay::ContextMenu { levels, origin };
            }
            return;
        }
    }

    // Walk visible levels from deepest to shallowest. Hovering an
    // ancestor truncates the stack to that level (drops cascaded
    // children) and updates its selection.
    for level_idx in (0..levels.len()).rev() {
        let entries = levels[level_idx].entries.clone();
        if let Some(idx) = context_menu_hit_test(&entries, rects[level_idx], mx, my) {
            levels.truncate(level_idx + 1);
            levels[level_idx].selected = idx;
            app.active_overlay = ActiveOverlay::ContextMenu { levels, origin };
            return;
        }
    }
}

/// Handle left-click on a context menu item. Returns true if a menu item
/// was clicked (action executed, menu closed). Returns false if click was
/// outside the menu.
fn context_menu_mouse_click(
    app: &mut AppState,
    mx: u16,
    my: u16,
    tx: &mpsc::Sender<AppMessage>,
    invert: bool,
) -> bool {
    use super::context_menu::{ContextMenuEntry, MenuLevel, MAX_CONTEXT_MENU_DEPTH};
    let area = crossterm::terminal::size().unwrap_or((80, 24));

    let (mut levels, origin) = match &app.active_overlay {
        ActiveOverlay::ContextMenu { levels, origin } => (levels.clone(), *origin),
        _ => return false,
    };

    // Compute rects + preview, then immediately clone the preview's
    // entries so we can drop the borrow on `levels` before mutating it.
    let (rects, preview_owned): (Vec<Rect>, Option<(Vec<ContextMenuEntry>, usize)>) = {
        let (r, p) = context_menu_stack_rects(&levels, origin, area.0, area.1);
        (r, p.map(|(es, idx)| (es.to_vec(), idx)))
    };
    let preview = preview_owned.as_ref().map(|(v, i)| (v.as_slice(), *i));

    /// Map a selectable index to the entry-list position it represents.
    fn entry_index_at(entries: &[ContextMenuEntry], selectable_idx: usize) -> Option<usize> {
        let mut count = 0;
        for (i, e) in entries.iter().enumerate() {
            let is_sel = matches!(e, ContextMenuEntry::Item(item) if item.enabled)
                || matches!(e, ContextMenuEntry::Submenu { .. });
            if is_sel {
                if count == selectable_idx {
                    return Some(i);
                }
                count += 1;
            }
        }
        None
    }

    // Click priority: preview first, then innermost level outward.
    if let Some((preview_entries, preview_idx)) = preview {
        if let Some(idx) = context_menu_hit_test(preview_entries, rects[preview_idx], mx, my) {
            if let Some(entry_idx) = entry_index_at(preview_entries, idx) {
                match &preview_entries[entry_idx] {
                    ContextMenuEntry::Item(item) => {
                        let action = item.action.clone();
                        run_context_action_restoring_parked(app, action, tx, invert);
                        return true;
                    }
                    ContextMenuEntry::Submenu { children, .. } => {
                        // Promote preview to focused. Push the child too
                        // if depth permits; otherwise the next render's
                        // preview computation will surface it.
                        if levels.len() < MAX_CONTEXT_MENU_DEPTH {
                            let mut new_level = MenuLevel::new(preview_entries.to_vec());
                            new_level.selected = idx;
                            levels.push(new_level);
                            if levels.len() < MAX_CONTEXT_MENU_DEPTH {
                                levels.push(MenuLevel::new(children.clone()));
                            }
                            app.active_overlay = ActiveOverlay::ContextMenu { levels, origin };
                            return true;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    for level_idx in (0..levels.len()).rev() {
        let entries = levels[level_idx].entries.clone();
        if let Some(idx) = context_menu_hit_test(&entries, rects[level_idx], mx, my) {
            if let Some(entry_idx) = entry_index_at(&entries, idx) {
                match &entries[entry_idx] {
                    ContextMenuEntry::Item(item) => {
                        let action = item.action.clone();
                        run_context_action_restoring_parked(app, action, tx, invert);
                        return true;
                    }
                    ContextMenuEntry::Submenu { children, .. } => {
                        // Truncate to this level, set its selection, push child.
                        levels.truncate(level_idx + 1);
                        levels[level_idx].selected = idx;
                        if levels.len() < MAX_CONTEXT_MENU_DEPTH {
                            levels.push(MenuLevel::new(children.clone()));
                        }
                        app.active_overlay = ActiveOverlay::ContextMenu { levels, origin };
                        return true;
                    }
                    _ => {}
                }
            }
        }
    }

    false
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
    let Some(mut state) = super::command::compute_completion(&input.text, input.cursor) else {
        return;
    };
    let typed: String =
        input.text[state.prefix_start..input.cursor.min(input.text.len())].to_string();
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
                        state.template_input = super::text_input::TextInputState::new(tmpl.clone());
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
            app.set_status(&format!(
                "Renamed {} file{}",
                count,
                if count == 1 { "" } else { "s" }
            ));
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
fn move_batch_cursor(app: &mut AppState, new_cursor: usize, _tx: &mpsc::Sender<AppMessage>) {
    let new_path = match &app.convert.source.mode {
        SourceMode::Batch { paths, .. } => paths.get(new_cursor).cloned(),
        _ => None,
    };
    let Some(path) = new_path else {
        return;
    };

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

    // Debounce: schedule the probe for 150ms from now instead of
    // spawning immediately. The event loop tick fires it when the
    // cursor has been still long enough.
    app.convert.source.batch_probe_debounce = Some((
        path,
        std::time::Instant::now() + std::time::Duration::from_millis(150),
    ));
}


fn move_metadata_cursor(app: &mut AppState, delta: isize, tx: &mpsc::Sender<AppMessage>) {
    let (len, cursor) = match &app.convert.source.mode {
        SourceMode::Batch { paths, cursor, .. } => (paths.len(), *cursor),
        SourceMode::MultiTrack { tracks, cursor, .. } => (tracks.len(), *cursor),
        _ => return,
    };
    if len == 0 {
        return;
    }
    let new_cursor = if delta < 0 {
        cursor.saturating_sub((-delta) as usize)
    } else {
        cursor.saturating_add(delta as usize).min(len - 1)
    };
    if new_cursor == cursor {
        return;
    }
    set_metadata_cursor(app, new_cursor, tx);
}

fn set_metadata_cursor(app: &mut AppState, index: usize, tx: &mpsc::Sender<AppMessage>) {
    let batch_len = match &app.convert.source.mode {
        SourceMode::Batch { paths, .. } => Some(paths.len()),
        _ => None,
    };
    if let Some(len) = batch_len {
        if index < len {
            move_batch_cursor(app, index, tx);
        }
        ensure_metadata_cursor_visible(app, metadata_file_list_visible_rows(app));
        return;
    }

    if let SourceMode::MultiTrack { tracks, cursor, scroll, .. } = &mut app.convert.source.mode {
        if index < tracks.len() {
            *cursor = index;
            if *cursor < *scroll {
                *scroll = *cursor;
            } else if *cursor >= *scroll + 6 {
                *scroll = cursor.saturating_sub(5);
            }
        }
    }
    ensure_metadata_cursor_visible(app, metadata_file_list_visible_rows(app));
}

fn metadata_file_list_visible_rows(app: &AppState) -> usize {
    // The register pass records this as transient UI geometry in ButtonRenderMap,
    // which is cleared at the start of each render. Keep it out of ConvertState
    // so draw/register remains read-only with respect to convert state.
    app.button_map
        .metadata_file_list_visible_rows()
        .unwrap_or(1)
        .max(1)
}

fn ensure_metadata_cursor_visible(app: &mut AppState, visible: usize) {
    let visible = visible.max(1);
    let (len, cursor) = match &app.convert.source.mode {
        SourceMode::Batch { paths, cursor, .. } => (paths.len(), *cursor),
        SourceMode::MultiTrack { tracks, cursor, .. } => (tracks.len(), *cursor),
        _ => return,
    };
    if len == 0 || len <= visible {
        app.convert.metadata.file_scroll = 0;
        return;
    }
    let max_scroll = len.saturating_sub(visible);
    if cursor < app.convert.metadata.file_scroll {
        app.convert.metadata.file_scroll = cursor;
    } else if cursor >= app.convert.metadata.file_scroll + visible {
        app.convert.metadata.file_scroll = cursor + 1 - visible;
    }
    app.convert.metadata.file_scroll = app.convert.metadata.file_scroll.min(max_scroll);
}

fn toggle_convert_advanced(app: &mut AppState, focus: ConvertFocus) {
    if app.convert.is_collapsed(focus) {
        app.convert.layout = ConvertLayout::Maximized(focus);
    }
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
            app.convert.output_options.advanced_open = !app.convert.output_options.advanced_open;
        }
    }
}


fn clear_convert_double_click_state_for_button(app: &mut AppState, button: Option<TuiButton>) {
    if app.current_screen != AppScreen::Convert {
        return;
    }

    match button {
        Some(TuiButton::Pane(_)) => {
            app.convert.metadata_file_last_click = None;
        }
        Some(TuiButton::MetadataFileRow(_)) => {
            app.convert.pane_title_last_click = None;
        }
        _ => {
            app.convert.pane_title_last_click = None;
            app.convert.metadata_file_last_click = None;
        }
    }
}

fn open_convert_cursor_metadata_editor(app: &mut AppState) {
    let initial_track = match &app.convert.source.mode {
        SourceMode::MultiTrack { cursor, .. } => Some(*cursor),
        _ => None,
    };

    let Some(path) = app.convert.source.mode.current_path().cloned() else {
        app.set_status("metadata: no source file selected");
        return;
    };

    if super::sacd::is_sacd_iso(&path) {
        open_metadata_editor_for_sacd_at_track(app, path, initial_track);
        return;
    }
    if crate::disc::dvda_utils::is_dvda_source(&path) {
        open_metadata_editor_for_dvda_at_track(app, path, initial_track);
        return;
    }

    let mut paths = vec![path];
    let mut entries = match super::probe::read_all_tags_merged(&paths) {
        Ok(e) => e,
        Err(e) => {
            app.set_status(format!("Failed to read tags: {}", e));
            return;
        }
    };

    if paths.len() == 1 {
        inject_sidecar_cuesheet_if_present(&mut entries, &paths[0]);
        apply_embedded_cuesheet_per_track(&mut entries);
    }
    super::probe::sort_paths_and_entries_by_track(&mut paths, &mut entries);

    let file_labels: Vec<String> = paths
        .iter()
        .map(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string()
        })
        .collect();

    let mut state = super::app::MetadataEditorState {
        paths,
        entries,
        cursor: 0,
        scroll: 0,
        last_click: None,
        edit_input: None,
        add_key_input: None,
        phase: super::app::MetadataEditorPhase::Editing,
        dirty: false,
        deleted: Vec::new(),
        file_labels,
        detail_field_idx: 0,
        detail_cursor: 0,
        detail_scroll: 0,
        detail_edit: None,
        mb_back: None,
        gnudb_back: None,
        read_only: false,
        sacd_sidecar_path: None,
        sacd_area_kind: None,
        sacd_stereo_durations: None,
        sacd_multi_channel_durations: None,
        presentation_tabs: Vec::new(),
        active_tab: 0,
    };
    if let Some(track_index) = initial_track {
        focus_metadata_editor_on_track(&mut state, track_index);
    }
    app.active_overlay = ActiveOverlay::MetadataEditor(Box::new(state));
}

fn focus_metadata_editor_on_track(
    state: &mut super::app::MetadataEditorState,
    track_index: usize,
) {
    if state.entries.is_empty() {
        return;
    }

    let preferred_keys = ["TITLE", "ARTIST", "PERFORMER", "TRACKNUMBER", "ISRC"];
    let preferred = preferred_keys.iter().find_map(|key| {
        state.entries.iter().position(|entry| {
            entry.display_key.eq_ignore_ascii_case(key)
                && !entry.is_binary
                && entry.per_file_values.len() > track_index
        })
    });
    let fallback = state.entries.iter().position(|entry| {
        !entry.is_binary && entry.per_file_values.len() > track_index
    });
    let Some(field_idx) = preferred.or(fallback) else {
        state.cursor = state.cursor.min(state.entries.len().saturating_sub(1));
        ensure_cursor_visible(state);
        return;
    };

    state.cursor = field_idx;
    ensure_cursor_visible(state);

    let values_len = state.entries[field_idx].per_file_values.len();
    if values_len > 1 {
        state.detail_field_idx = field_idx;
        state.detail_cursor = track_index.min(values_len - 1);
        state.detail_scroll = 0;
        state.detail_edit = None;
        state.last_click = None;
        state.phase = super::app::MetadataEditorPhase::DetailEdit;
        ensure_detail_visible(state);
    }
}

/// Remove the file at the batch cursor from the batch. If the batch
/// drops to 0 files, transitions to `SourceMode::Empty`. If it drops
/// to 1 file, promotes to `SourceMode::Single` and spawns a background
/// probe for the remaining file.
fn remove_batch_at_cursor(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    // Capture the current cursor file + its cached info BEFORE removing.
    let old_cursor_path = match &app.convert.source.mode {
        SourceMode::Batch { paths, cursor, .. } => paths.get(*cursor).cloned(),
        _ => None,
    };
    let old_cursor_info = match &app.convert.source.mode {
        SourceMode::Batch {
            cursor_info,
            cursor_metadata,
            ..
        } => cursor_info
            .as_ref()
            .map(|info| (info.clone(), cursor_metadata.clone())),
        _ => None,
    };

    let (remaining_paths, new_cursor, _removed_path) = match &mut app.convert.source.mode {
        SourceMode::Batch { paths, cursor, .. } if !paths.is_empty() => {
            let idx = (*cursor).min(paths.len() - 1);
            let removed_path = paths.remove(idx);
            app.convert.source.cue_artifact_audio.remove(&removed_path);
            let new_cursor = idx.min(paths.len().saturating_sub(1));
            (paths.clone(), new_cursor, removed_path)
        }
        _ => return,
    };

    if remaining_paths.is_empty() {
        app.convert.set_source_mode(SourceMode::Empty);
        app.convert.source.cue_artifact_audio.clear();
        return;
    }

    if remaining_paths.len() == 1 {
        let path = remaining_paths.into_iter().next().unwrap();
        // If this is the same file we already had info for, carry it over.
        if old_cursor_path.as_ref() == Some(&path) {
            if let Some((info, metadata)) = old_cursor_info {
                app.convert.set_source_mode(SourceMode::from_single(path, Some(info), metadata));
                return;
            }
        }
        app.convert.set_source_mode(SourceMode::from_single(
            path.clone(),
            None,
            crate::tui::probe::SourceMetadata::default(),
        ));
        super::browse::spawn_audio_probe(path, tx.clone());
        return;
    }

    // Stay in Batch — recompute summary and move cursor.
    let new_cursor_path = remaining_paths.get(new_cursor).cloned();
    let mut new_mode = SourceMode::from_paths(remaining_paths);

    // If the cursor landed on the same file, carry over cached info.
    let need_probe = if let SourceMode::Batch {
        cursor,
        cursor_info,
        cursor_metadata,
        ..
    } = &mut new_mode
    {
        *cursor = new_cursor;
        if new_cursor_path == old_cursor_path {
            if let Some((info, meta)) = old_cursor_info {
                *cursor_info = Some(info);
                *cursor_metadata = meta;
                false // No probe needed.
            } else {
                true
            }
        } else {
            true
        }
    } else {
        false
    };

    app.convert.set_source_mode(new_mode);

    if need_probe {
        if let Some(p) = new_cursor_path {
            super::browse::spawn_audio_probe(p, tx.clone());
        }
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
    let value_opt = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    };

    match target {
        TextEditTarget::DestPath => {
            // dest_path is not in the preset, don't mark modified
            app.convert.output_options.dest_path = if trimmed.is_empty() {
                None
            } else {
                Some(std::path::PathBuf::from(trimmed))
            };
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

            // Step 1 (main thread, fast): create backup + journal entry.
            let backup = crate::db::Database::backup_path_for(&path);
            if let Err(e) = crate::db::Database::create_backup_for(&path, &backup) {
                app.set_status(format!("backup failed: {}", e));
                return;
            }
            if let Err(e) = app
                .db
                .begin_metadata_write(&path.display().to_string(), &backup.display().to_string())
            {
                let _ = std::fs::remove_file(&backup);
                app.set_status(format!("journal error: {}", e));
                return;
            }

            app.set_status(format!(
                "Writing {}...",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));

            // Step 2 (background): lofty write (potentially slow for large files).
            let write_path = path.clone();
            let write_field = field;
            let write_value = trimmed.to_string();
            let tx = tx.clone();
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    crate::tui::probe::write_metadata_field(&write_path, write_field, &write_value)
                })
                .await
                .unwrap_or_else(|e| Err(format!("task panic: {}", e)));

                let _ = tx
                    .send(AppMessage::MetadataWriteComplete {
                        path,
                        field: write_field,
                        result,
                    })
                    .await;
            });
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
                            app.active_overlay = ActiveOverlay::BulkRename(rename_state);
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
pub fn apply_file_op_pub(app: &mut AppState, target: TextEditTarget, dest: &str) {
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
    app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
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
        app.set_status(format!(
            "destination is not a directory: {}",
            dest_dir.display()
        ));
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
        if let (Ok(src_canon), Ok(dst_canon)) = (source.canonicalize(), target.canonicalize()) {
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
        let src_size = std::fs::metadata(source).map(|m| m.len()).unwrap_or(0);
        let dst_size = std::fs::metadata(target).map(|m| m.len()).unwrap_or(0);
        if src_size != dst_size {
            // Copy succeeded but sizes differ — don't delete original.
            return Err(
                "cross-device move: size mismatch after copy, original preserved".to_string(),
            );
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
            app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
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
                app.set_status(format!("recent: file no longer exists: {}", path.display()));
                // Drop the dead entry.
                let idx = app.recent.entries.iter().position(|e| e.path == path);
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
                if !app.bookmarks.commit_naming_with_db(&app.db) {
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
            app.bookmarks.remove_with_db(idx, &app.db);
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
            app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
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
            app.convert.set_source_mode(SourceMode::from_single(
                path.to_path_buf(),
                Some(info),
                metadata,
            ));
            app.set_status(format!(
                "Loaded: {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            app.current_screen = AppScreen::Convert;
            // Bump to top of recent list.
            app.recent.record_use_with_db(path, &app.db);
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
                    app.convert.set_source_mode(SourceMode::Empty);
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
            app.convert.set_source_mode(SourceMode::from_single(
                path.to_path_buf(),
                Some(info),
                metadata,
            ));
            app.set_status(format!(
                "Loaded: {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            // Record in the recent-files history.
            app.recent.record_use_with_db(path, &app.db);
        }
        _ => {
            // Add to queue (existing behavior)
            add_path_to_queue(app, path);
        }
    }
}

// ── Helper functions ─────────────────────────────────────────────────

fn cancel_confirm_action(app: &mut AppState, action: Option<&ConfirmAction>) {
    // Confirmation cancellation must behave the same for keyboard
    // Esc/N and mouse No/Cancel clicks. Some confirmation flows park
    // the editor in `pending_metadata_editor` before opening the
    // dialog; clearing only `active_overlay` would strand and drop
    // the populated editor on the next transition.
    app.active_overlay = ActiveOverlay::None;
    if let Some(parked) = app.pending_metadata_editor.take() {
        if matches!(app.active_overlay, ActiveOverlay::None) {
            app.active_overlay = ActiveOverlay::MetadataEditor(parked);
        }
    }

    if matches!(action, Some(ConfirmAction::ApplyMbToAllPresentations(_))) {
        app.set_status(":tags-mb: kept MusicBrainz values on active presentation".to_string());
    }
}

fn execute_confirm_action(
    app: &mut AppState,
    action: &ConfirmAction,
    tx: &mpsc::Sender<AppMessage>,
) {
    match action {
        ConfirmAction::MbBack(cache) => {
            // Discard parked editor (and its edits); transition back
            // to MbSelect with the cached release list + paths.
            app.pending_metadata_editor = None;
            let mut mb_state =
                super::app::MbSelectState::new(cache.releases.clone(), cache.paths.clone());
            mb_state.selected = cache.selected;
            app.active_overlay = ActiveOverlay::MbSelect(Box::new(mb_state));
            app.set_status(":mb-back: pick a different release".to_string());
        }
        ConfirmAction::GnudbBack(review) => {
            // Discard parked editor; restore the cached GnudbReviewState
            // (preserves the user's per-track review edits) and
            // transition back.
            app.pending_metadata_editor = None;
            app.active_overlay = ActiveOverlay::GnudbReview(review.clone());
            app.set_status(":gnudb-back: review per-track values".to_string());
        }
        ConfirmAction::ApplyMbToAllPresentations(state) => {
            app.pending_metadata_editor = None;
            let mut state = (**state).clone();
            let copied = state.apply_active_musicbrainz_values_to_matching_presentations();
            app.active_overlay = ActiveOverlay::MetadataEditor(Box::new(state));
            app.set_status(format!(
                ":tags-mb: applied MusicBrainz values to {} matching presentation(s)",
                copied,
            ));
        }
        ConfirmAction::RemoveSelected => {
            let removed = app.manager.remove_selected();
            app.save_queue();
            app.set_status(format!("Removed {} items", removed));
        }
        ConfirmAction::ClearCompleted => {
            app.manager.clear_completed();
            app.save_queue();
            app.set_status("Cleared completed items");
        }
        ConfirmAction::ClearFinished => {
            app.manager.clear_finished();
            app.save_queue();
            app.set_status("Cleared all finished items");
        }
        ConfirmAction::ClearAll => {
            app.manager.clear_all();
            app.save_queue();
            app.set_status("Cleared all items");
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
            app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
            let mut parts = vec![format!("trashed {} item(s)", trashed)];
            if errors > 0 {
                parts.push(format!("{} errors", errors));
            }
            app.set_status(parts.join(", "));
        }
        ConfirmAction::OffsetCorrection { paths, offset } => {
            let paths = paths.clone();
            let offset = *offset;
            let tx = tx.clone();
            app.set_status("Applying offset correction...".to_string());
            tokio::spawn(async move {
                let result =
                    super::accuraterip::apply_offset_correction(&paths, offset, tx.clone()).await;
                let _ = tx
                    .send(AppMessage::OffsetCorrectionComplete { result })
                    .await;
            });
        }
        ConfirmAction::CtdbRepair {
            paths,
            parity_url,
            npar,
            offset,
            expected_crcs,
        } => {
            let paths = paths.clone();
            let parity_url = parity_url.clone();
            let npar = *npar;
            let offset = *offset;
            let expected_crcs = expected_crcs.clone();
            let tx = tx.clone();
            app.set_status("CTDB repair: starting...".to_string());
            tokio::spawn(async move {
                let result = super::ctdb::repair_album(
                    &paths,
                    &parity_url,
                    npar,
                    offset,
                    &expected_crcs,
                    tx.clone(),
                )
                .await;
                let _ = tx.send(AppMessage::CtdbRepairComplete { result }).await;
            });
        }
        ConfirmAction::CtdbRepairSingleImage {
            info,
            parity_url,
            npar,
            offset,
            expected_crcs,
        } => {
            let info = info.clone();
            let parity_url = parity_url.clone();
            let npar = *npar;
            let offset = *offset;
            let expected_crcs = expected_crcs.clone();
            let tx = tx.clone();
            app.set_status("CTDB repair: starting (single image)...".to_string());
            tokio::spawn(async move {
                let result = super::ctdb::repair_single_image(
                    &info,
                    &parity_url,
                    npar,
                    offset,
                    &expected_crcs,
                    tx.clone(),
                )
                .await;
                let _ = tx.send(AppMessage::CtdbRepairComplete { result }).await;
            });
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
                let is_known = is_compound_tar
                    || file_path
                        .extension()
                        .map(|ext| {
                            let e = ext.to_string_lossy().to_lowercase();
                            matches!(
                                e.as_str(),
                                "7z" | "zip"
                                    | "rar"
                                    | "tar"
                                    | "iso"
                                    | "cab"
                                    | "tgz"
                                    | "tbz2"
                                    | "txz"
                                    | "flac"
                                    | "wav"
                                    | "aiff"
                                    | "aif"
                                    | "wv"
                                    | "mp3"
                                    | "m4a"
                                    | "aac"
                                    | "opus"
                                    | "ogg"
                            )
                        })
                        .unwrap_or(false);
                if is_known {
                    match app
                        .manager
                        .add_file_blocking(file_path.to_path_buf(), options.clone())
                    {
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
            Ok(_) => app.set_status(format!(
                "Added: {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            )),
            Err(e) => app.set_status(format!("Error: {}", e)),
        }
    }

    app.save_queue();
}

fn start_conversion(app: &mut AppState, tx: mpsc::Sender<AppMessage>) {
    // Check for not-configured items (queue-screen-specific message)
    let not_configured = app
        .items_snapshot
        .iter()
        .filter(|i| matches!(i.status, ConversionStatus::NotConfigured))
        .count();
    if not_configured > 0 {
        let queued = app
            .items_snapshot
            .iter()
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
    app.save_queue();
    app.set_status("Re-queued failed items for retry");
}

/// Handle mouse events
pub fn handle_mouse(app: &mut AppState, mouse: MouseEvent, tx: &mpsc::Sender<AppMessage>) {
    // Metadata editor mouse: intercept all events when the editor is open.
    if matches!(app.active_overlay, ActiveOverlay::MetadataEditor(_)) {
        handle_metadata_editor_mouse(app, mouse, tx);
        return;
    }
    // MbSelect picker: dedicated handler (clickable rows, footer pills,
    // right-click context menu, double-click-to-accept).
    if matches!(app.active_overlay, ActiveOverlay::MbSelect(_)) {
        handle_mb_select_mouse(app, mouse, tx);
        return;
    }
    // CuePreview: dedicated handler (clickable lines, footer pills,
    // right-click context menu, double-click-to-edit).
    if matches!(app.active_overlay, ActiveOverlay::CuePreview(_)) {
        handle_cue_preview_mouse(app, mouse, tx);
        return;
    }

    // Template picker: dedicated handler for row clicks.
    if matches!(app.active_overlay, ActiveOverlay::TemplatePicker { .. }) {
        handle_template_picker_mouse(app, mouse);
        return;
    }

    // Template builder: dedicated handler for token/saved clicks.
    if matches!(app.active_overlay, ActiveOverlay::TemplateBuilder(_)) {
        handle_template_builder_mouse(app, mouse);
        return;
    }

    // FormatSettings: dedicated handler for pill clicks inside the overlay.
    if matches!(app.active_overlay, ActiveOverlay::FormatSettings { .. }) {
        handle_format_settings_mouse(app, mouse, tx);
        return;
    }

    if matches!(app.active_overlay, ActiveOverlay::DiscBrowser(_)) {
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            if let Some(button) = app.button_map.find_button_at(mouse.column, mouse.row) {
                let now = std::time::Instant::now();
                let click_count = match button {
                    TuiButton::DiscBrowserStream(index) => {
                        let is_double = app
                            .last_disc_browser_stream_click
                            .as_ref()
                            .filter(|(prior, _)| *prior == index)
                            .map(|(_, t)| now.duration_since(*t).as_millis() < 500)
                            .unwrap_or(false);
                        if is_double {
                            app.last_disc_browser_stream_click = None;
                            2
                        } else {
                            app.last_disc_browser_stream_click = Some((index, now));
                            1
                        }
                    }
                    _ => {
                        app.last_disc_browser_stream_click = None;
                        1
                    }
                };
                super::disc_browser_actions::handle_disc_browser_button_click(app, &button, click_count, tx);
            }
        }
        return;
    }

    // Generic overlay mouse: click-outside-to-close + footer pill clicks
    // for all overlays (except MetadataEditor which has its own handler,
    // and ContextMenu which has its own hover/click system).
    if !matches!(
        app.active_overlay,
        ActiveOverlay::None | ActiveOverlay::ContextMenu { .. }
    ) || app.preset.overlay_open
        || app.recent.overlay_open
        || app.bookmarks.overlay_open
    {
        if handle_generic_overlay_mouse(app, mouse, tx) {
            return;
        }
    }

    // Context menu mouse interaction must be checked BEFORE the generic
    // hover handler, since the menu is an overlay not tracked by button_map.
    if matches!(app.active_overlay, ActiveOverlay::ContextMenu { .. }) {
        if matches!(mouse.kind, MouseEventKind::Moved | MouseEventKind::Drag(_)) {
            context_menu_mouse_hover(app, mouse.column, mouse.row);
            return;
        }
    }

    // Hover tracking: update hover_target on every mouse move.
    if matches!(mouse.kind, MouseEventKind::Moved | MouseEventKind::Drag(_)) {
        let new_hover = app.button_map.find_button_at(mouse.column, mouse.row);
        app.hover_target = new_hover;
        return; // Move events don't trigger actions.
    }

    // Scroll wheel: ignore while any overlay is open.
    if matches!(
        mouse.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) {
        if app.current_screen == AppScreen::Convert {
            clear_convert_double_click_state_for_button(app, None);
        }
        if !matches!(app.active_overlay, ActiveOverlay::None) {
            return;
        }
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
        } else if app.current_screen == AppScreen::Convert {
            if matches!(
                app.button_map.find_button_at(mouse.column, mouse.row),
                Some(TuiButton::MetadataFileRow(_)) | Some(TuiButton::Pane(ConvertFocus::Metadata))
            ) && !app.convert.is_collapsed(ConvertFocus::Metadata)
                && matches!(&app.convert.source.mode, SourceMode::Batch { .. } | SourceMode::MultiTrack { .. })
            {
                match mouse.kind {
                    MouseEventKind::ScrollUp => {
                        for _ in 0..3 {
                            move_metadata_cursor(app, -1, tx);
                        }
                    }
                    MouseEventKind::ScrollDown => {
                        for _ in 0..3 {
                            move_metadata_cursor(app, 1, tx);
                        }
                    }
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
            clear_convert_double_click_state_for_button(app, None);
            if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
                // Right-click: close old menu (the user is going to open
                // a new one). Drop any parked overlay state since the
                // user is shifting context. Fall through to open new.
                app.active_overlay = ActiveOverlay::None;
                app.pending_metadata_editor = None;
                app.pending_cue_preview = None;
                app.pending_mb_select = None;
            } else {
                // Left-click: try to activate the hovered item.
                if !context_menu_mouse_click(app, mouse.column, mouse.row, tx, false) {
                    // Click was outside the menu — close (restoring any
                    // parked overlay) and select the clicked browse/queue
                    // item if applicable.
                    close_context_menu_restoring_parked(app);
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
        clear_convert_double_click_state_for_button(app, None);
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
    let clicked_button = app.button_map.find_button_at(x, y);
    let double_click_candidate = if matches!(app.active_overlay, ActiveOverlay::None) {
        clicked_button
    } else {
        None
    };
    clear_convert_double_click_state_for_button(app, double_click_candidate);

    // If wizard is active, forward to wizard
    if app.current_screen == AppScreen::Wizard {
        if let Some(wizard) = &mut app.wizard {
            let button_id = app
                .wizard_mouse_areas
                .as_ref()
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
            } else if wizard.should_exit {
                app.wizard = None;
                app.wizard_mouse_areas = None;
                app.current_screen = AppScreen::Convert;
            }
        }
        return;
    }

    // Don't dispatch button clicks while a modal overlay (TextEdit,
    // FileInput, etc.) is open — the user must close it first. Without
    // this guard, clicking a different field while editing silently
    // replaces the overlay and loses the pending edit.
    // Exception: Confirmation overlay allows its own Yes/No pill clicks.
    if !matches!(app.active_overlay, ActiveOverlay::None) {
        if matches!(app.active_overlay, ActiveOverlay::Confirmation { .. }) {
            // Allow OverlayConfirm/OverlayCancel buttons through.
            if let Some(button) = app.button_map.find_button_at(x, y) {
                match button {
                    TuiButton::OverlayConfirm => {
                        if let ActiveOverlay::Confirmation { action, .. } = &app.active_overlay {
                            let action = action.clone();
                            app.active_overlay = ActiveOverlay::None;
                            execute_confirm_action(app, &action, tx);
                        }
                        return;
                    }
                    TuiButton::OverlayCancel => {
                        let action = match &app.active_overlay {
                            ActiveOverlay::Confirmation { action, .. } => Some(action.clone()),
                            _ => None,
                        };
                        cancel_confirm_action(app, action.as_ref());
                        return;
                    }
                    _ => {}
                }
            }
        }
        return;
    }

    // Check button map — skip screen-specific buttons if they belong
    // to a different screen (stale button_map from the previous frame).
    if let Some(button) = clicked_button {
        if super::disc_browser_actions::handle_disc_browser_button(app, &button, tx) {
            return;
        }
        if let Some(btn_screen) = button.screen() {
            if btn_screen != app.current_screen {
                return; // Stale button from a previous screen's render.
            }
        }
        match button {
            // ── Convert screen: pane focus ──
            TuiButton::Pane(focus) => {
                let now = std::time::Instant::now();
                let is_double = app
                    .convert
                    .pane_title_last_click
                    .map(|(prev_focus, prev_time)| {
                        prev_focus == focus && now.duration_since(prev_time).as_millis() < 300
                    })
                    .unwrap_or(false);

                if is_double {
                    app.convert.toggle_maximize(focus);
                    app.convert.pane_title_last_click = None;
                } else {
                    app.convert.focus = focus;
                    app.convert.pane_title_last_click = Some((focus, now));
                }
                app.current_screen = AppScreen::Convert;
            }
            TuiButton::MaximizeToggle(focus) => {
                app.convert.toggle_maximize(focus);
                if app.convert.is_maximized(focus) {
                    app.convert.focus = focus;
                }
                app.current_screen = AppScreen::Convert;
            }

            // ── Convert screen: tab bar ──
            TuiButton::Tab(n) => match n {
                1 => {
                    app.current_screen = AppScreen::Browse;
                    app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
                }
                2 => app.current_screen = AppScreen::Library,
                3 => app.current_screen = AppScreen::Convert,
                4 => app.current_screen = AppScreen::Queue,
                5 => app.current_screen = AppScreen::Config,
                _ => {}
            },

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
                    match super::presets::save_preset_with_db(&preset, &app.db) {
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
                let initial = app
                    .convert
                    .output_options
                    .dest_path
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
                app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
            }
            TuiButton::SourceExpandButton => {
                // Open the BatchList overlay if a batch is loaded.
                if app.convert.source.mode.is_batch() {
                    app.active_overlay = ActiveOverlay::BatchList { scroll: 0 };
                }
            }
            TuiButton::SourceAnalyzeButton => {
                let cmd = super::command::Command::Analyze { force: false };
                super::command::execute_command(app, cmd, tx);
            }
            TuiButton::SourceStreamPrev => {
                cycle_stream_pill(app, false);
            }
            TuiButton::SourceStreamNext => {
                cycle_stream_pill(app, true);
            }
            TuiButton::SourceEnqueueButton => {
                super::command::execute_commit_with_disc_selection_bridge(app, false, tx);
            }
            TuiButton::SourceEnqueueStartButton => {
                super::command::execute_commit_with_disc_selection_bridge(app, true, tx);
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
            TuiButton::MetadataFileRow(index) => {
                app.convert.focus = ConvertFocus::Metadata;
                let now = std::time::Instant::now();
                let is_double = app
                    .convert
                    .metadata_file_last_click
                    .map(|(prev, prev_time)| {
                        prev == index && now.duration_since(prev_time).as_millis() < 300
                    })
                    .unwrap_or(false);

                // Single click always selects the row. Only a second click on the
                // same row within the double-click window opens the editor.
                set_metadata_cursor(app, index, tx);

                if is_double {
                    app.convert.metadata_file_last_click = None;
                    open_convert_cursor_metadata_editor(app);
                } else {
                    app.convert.metadata_file_last_click = Some((index, now));
                }
            }

            // ── Convert screen: advanced toggle per pane ──
            TuiButton::AdvancedToggle(focus) => {
                toggle_convert_advanced(app, focus);
            }

            // ── Convert screen: format pane pills ──
            TuiButton::FormatPill(_)
            | TuiButton::RatePill(_)
            | TuiButton::DepthPill(_)
            | TuiButton::ResamplerPill(_)
            | TuiButton::DitherPill(_)
            | TuiButton::ReplayGainPill(_)
            | TuiButton::NoiseShaperPill(_)
            | TuiButton::ModulatorOrderPill(_)
            | TuiButton::ConversionPresetPill(_)
            | TuiButton::DsdGainPill(_) => {
                app.convert.focus = ConvertFocus::Format;
                if super::format_interactions::handle_convert_format_button(&mut app.convert, button) {
                    app.preset.mark_modified();
                }
            }
            TuiButton::ContainerPill(i) => {
                app.convert.focus = ConvertFocus::Format;
                let containers = app.convert.format.format.selected_value().available_containers();
                if i < containers.len() && containers[i].enabled {
                    app.convert.format.selected_container_index = i;
                    app.preset.mark_modified();
                }
            }
            TuiButton::FormatSettingsButton => {
                app.convert.focus = ConvertFocus::Format;
                let fmt = &app.convert.format;
                let selected = *fmt.format.selected_value();
                let (kind, focus) = match selected {
                    crate::convert::formats::AudioFormat::Flac => (
                        FormatSettingsKind::Flac {
                            compression_input: super::text_input::TextInputState::new(
                                fmt.flac_compression_level.to_string(),
                            ),
                            verify: *fmt.flac_verify.selected_value(),
                            md5: *fmt.flac_md5.selected_value(),
                        },
                        FormatSettingsFocus::Compression,
                    ),
                    crate::convert::formats::AudioFormat::Aac => (
                        FormatSettingsKind::Aac {
                            profile: fmt.aac_profile,
                            quality_preset: fmt.aac_quality_preset,
                            bitrate_input: super::text_input::TextInputState::new(
                                fmt.aac_bitrate_kbps.to_string(),
                            ),
                        },
                        FormatSettingsFocus::AacProfile,
                    ),
                    crate::convert::formats::AudioFormat::Opus => (
                        FormatSettingsKind::Opus {
                            content_type: fmt.opus_content_type,
                            quality_preset: fmt.opus_quality_preset,
                            bitrate_input: super::text_input::TextInputState::new(
                                fmt.opus_bitrate_kbps.to_string(),
                            ),
                            complexity_input: super::text_input::TextInputState::new(
                                fmt.opus_complexity.to_string(),
                            ),
                        },
                        FormatSettingsFocus::OpusContentType,
                    ),
                    crate::convert::formats::AudioFormat::Mp3 => (
                        FormatSettingsKind::Mp3 {
                            mode: fmt.mp3_mode,
                            vbr_quality_input: super::text_input::TextInputState::new(
                                fmt.mp3_vbr_quality.to_string(),
                            ),
                            quality_preset: fmt.mp3_quality_preset,
                            bitrate_input: super::text_input::TextInputState::new(
                                fmt.mp3_bitrate_kbps.to_string(),
                            ),
                        },
                        FormatSettingsFocus::Mp3Mode,
                    ),
                    crate::convert::formats::AudioFormat::WavPack => (
                        FormatSettingsKind::WavPack {
                            mode: fmt.wavpack_mode,
                            hybrid: fmt.wavpack_hybrid,
                            bitrate_input: super::text_input::TextInputState::new(
                                fmt.wavpack_bitrate_kbps.to_string(),
                            ),
                            correction: fmt.wavpack_correction,
                        },
                        FormatSettingsFocus::WavPackMode,
                    ),
                    _ => return,
                };
                app.active_overlay = ActiveOverlay::FormatSettings { kind, focus, help_scroll: None };
            }
            TuiButton::ResampleQualityPill(i) => {
                use tonepoet_pipeline::enums::ResampleQuality;
                app.convert.focus = ConvertFocus::Format;
                let mut qualities = vec![
                    ResampleQuality::Low,
                    ResampleQuality::Medium,
                    ResampleQuality::High,
                    ResampleQuality::VeryHigh,
                    ResampleQuality::Ultra,
                ];
                if matches!(*app.convert.format.resampler.selected_value(), ResamplerChoice::Sox | ResamplerChoice::Ssrc) {
                    qualities.push(ResampleQuality::Insane);
                }
                if let Some(&q) = qualities.get(i) {
                    app.convert.format.resample_quality = q;
                    app.preset.mark_modified();
                }
            }
            TuiButton::ResamplerSettingsButton => {
                app.convert.focus = ConvertFocus::Format;
                let fmt = &app.convert.format;
                let (kind, focus) = match *fmt.resampler.selected_value() {
                    ResamplerChoice::Ssrc => (
                        FormatSettingsKind::Ssrc {
                            attenuation_input: super::text_input::TextInputState::new(
                                fmt.ssrc_attenuation_db.map(|v| format!("{:.1}", v)).unwrap_or_default(),
                            ),
                            min_phase: fmt.ssrc_min_phase,
                            dither_id_input: super::text_input::TextInputState::new(
                                fmt.ssrc_dither_id.map(|v| v.to_string()).unwrap_or_default(),
                            ),
                            pdf_type_input: super::text_input::TextInputState::new(
                                fmt.ssrc_pdf_type.map(|v| match v {
                                    tonepoet_pipeline::enums::SsrcPdfType::Rectangular => "0".to_string(),
                                    tonepoet_pipeline::enums::SsrcPdfType::Triangular => "1".to_string(),
                                }).unwrap_or_default(),
                            ),
                        },
                        FormatSettingsFocus::SsrcAttenuation,
                    ),
                    ResamplerChoice::Sox => (
                        FormatSettingsKind::Sox {
                            chebyshev: fmt.sox_chebyshev,
                            bandwidth_input: super::text_input::TextInputState::new(
                                fmt.sox_bandwidth.map(|v| format!("{}", v)).unwrap_or_default(),
                            ),
                            phase_input: super::text_input::TextInputState::new(
                                fmt.sox_phase.map(|v| v.to_string()).unwrap_or_default(),
                            ),
                            allow_aliasing: fmt.sox_allow_aliasing,
                            sinc_taps_input: super::text_input::TextInputState::new(
                                fmt.sox_sinc_taps.map(|v| v.to_string()).unwrap_or_default(),
                            ),
                            sinc_attenuation_input: super::text_input::TextInputState::new(
                                fmt.sox_sinc_attenuation.map(|v| v.to_string()).unwrap_or_default(),
                            ),
                            sinc_passband_input: super::text_input::TextInputState::new(
                                fmt.sox_sinc_passband.map(|v| format!("{}", v)).unwrap_or_default(),
                            ),
                            sinc_transition_input: super::text_input::TextInputState::new(
                                fmt.sox_sinc_transition.map(|v| format!("{}", v)).unwrap_or_default(),
                            ),
                            sinc_kaiser_beta_input: super::text_input::TextInputState::new(
                                fmt.sox_sinc_kaiser_beta.map(|v| format!("{}", v)).unwrap_or_default(),
                            ),
                            sinc_phase: fmt.sox_sinc_phase,
                        },
                        FormatSettingsFocus::SoxChebyshev,
                    ),
                    ResamplerChoice::Soxr => (
                        FormatSettingsKind::Soxr {
                            chebyshev: fmt.soxr_chebyshev,
                            cutoff_input: super::text_input::TextInputState::new(
                                fmt.soxr_cutoff.map(|v| format!("{}", v)).unwrap_or_default(),
                            ),
                            phase_input: super::text_input::TextInputState::new(
                                fmt.soxr_phase.map(|v| v.to_string()).unwrap_or_default(),
                            ),
                        },
                        FormatSettingsFocus::SoxrChebyshev,
                    ),
                    _ => return,
                };
                app.active_overlay = ActiveOverlay::FormatSettings { kind, focus, help_scroll: None };
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
            TuiButton::QueueItemExpand(idx) => {
                app.selected_index = idx;
                if let Some(item) = app.items_snapshot.get(idx) {
                    if !item.active_tracks.is_empty() {
                        app.manager.toggle_track_collapse(&item.id);
                    }
                }
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
                app.save_queue();
                app.set_status("Cleared completed items");
            }
            TuiButton::ClearFinished => {
                app.manager.clear_finished();
                app.save_queue();
                app.set_status("Cleared all finished items");
            }
            TuiButton::ClearAll => {
                app.active_overlay = ActiveOverlay::Confirmation {
                    message: "Clear all items from the queue?".to_string(),
                    action: ConfirmAction::ClearAll,
                };
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
                let ctrl = mouse.modifiers.contains(KeyModifiers::CONTROL);

                // All click modes move the cursor to the clicked entry.
                app.browse.selected_index = idx;
                app.browse.ensure_visible();

                if ctrl {
                    // ── Ctrl+click / Ctrl+double-click ──
                    // Detect double-click within the Ctrl path using last_browse_click.
                    const DCLICK_MS: u64 = 500;
                    let now = std::time::Instant::now();
                    let is_ctrl_double = app
                        .last_browse_click
                        .as_ref()
                        .filter(|(p, _)| *p == clicked_path)
                        .map(|(_, t)| now.duration_since(*t).as_millis() < DCLICK_MS as u128)
                        .unwrap_or(false);

                    if is_ctrl_double {
                        // ── Ctrl+double-click: range-select from anchor to here ──
                        let anchor_idx = app
                            .browse
                            .multi_select_anchor
                            .as_ref()
                            .and_then(|p| app.browse.entries.iter().position(|e| e.path == *p))
                            .unwrap_or(idx);
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
                        app.last_browse_click = None;
                    } else {
                        // ── Ctrl+single-click: toggle individual item ──
                        app.browse.toggle_selection();
                        app.last_browse_click = Some((clicked_path, now));
                    }
                    app.pending_browse_rename = None;
                    app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
                } else {
                    // ── Plain click: select vs schedule-rename vs fresh ──
                    //
                    // - Click within the double-click window of the prior click
                    //   on the same path → double-click: toggle selection (files)
                    //   or navigate into directory.
                    // - Same-path click OUTSIDE the double-click window schedules
                    //   a rename (commits after another delay unless cancelled).
                    // - Click on a different path → fresh click, cancel pending.
                    const OPEN_MS: u64 = 500;

                    let now = std::time::Instant::now();
                    let is_double_click = app
                        .last_browse_click
                        .as_ref()
                        .filter(|(p, _)| *p == clicked_path)
                        .map(|(_, t)| now.duration_since(*t).as_millis() < OPEN_MS as u128)
                        .unwrap_or(false);

                    if is_double_click {
                        app.last_browse_click = None;
                        app.pending_browse_rename = None;
                        let entry_kind = app.browse.entries[idx].kind.clone();
                        match entry_kind {
                            // Directories: double-click navigates into them.
                            EntryKind::Directory | EntryKind::ParentDir => {
                                app.browse.multi_selected.clear();
                                app.browse.enter_selected();
                                app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
                            }
                            // Files: double-click toggles selection (like Ctrl+click).
                            _ => {
                                app.browse.toggle_selection();
                                app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
                            }
                        }
                    } else {
                        // Not a double-click. Plain click clears multi-selection
                        // (only Ctrl+click and Alt+click modify it).
                        app.browse.multi_selected.clear();
                        // Any click cancels any pending rename.
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
                            if !matches!(app.browse.entries[idx].kind, EntryKind::ParentDir) {
                                app.pending_browse_rename = Some((
                                    clicked_path.clone(),
                                    now + std::time::Duration::from_millis(OPEN_MS),
                                ));
                            }
                        }

                        // Record this click and anchor.
                        app.last_browse_click = Some((clicked_path.clone(), now));
                        app.browse.multi_select_anchor = Some(clicked_path);
                        app.browse.probe_current_with_db(tx, Some(&app.db));
                                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
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
            TuiButton::BrowseInfoAnalyze => {
                let cmd = super::command::Command::Analyze { force: false };
                super::command::execute_command(app, cmd, tx);
            }
            TuiButton::BrowseInfoEditTags => {
                open_metadata_editor(app);
            }
            TuiButton::BrowseInfoAudioStreams => {
                super::disc_browser_actions::open_selected_disc_browser(app, tx);
            }
            TuiButton::BrowseSearchToggle => {
                if app.browse.search.active {
                    app.browse.close_search();
                } else {
                    app.browse.open_search();
                }
            }
            TuiButton::BrowseSearchRecursive => {
                app.browse.search.recursive = !app.browse.search.recursive;
                app.browse.search.last_keystroke = Some(std::time::Instant::now());
            }
            TuiButton::BrowseSearchMode => {
                app.browse.search.mode = app.browse.search.mode.cycle();
                if app.browse.search.mode == super::browse::SearchMode::Filename
                    && app.browse.search.sort.is_tag_sort()
                {
                    app.browse.search.sort = super::browse::SearchSort::Score;
                    app.browse.search.sort_dir = super::browse::SortDir::Desc;
                }
                app.browse.search.last_keystroke = Some(std::time::Instant::now());
            }
            TuiButton::BrowseSearchSort => {
                let tag_mode = matches!(
                    app.browse.search.mode,
                    super::browse::SearchMode::Tags | super::browse::SearchMode::Both
                );
                app.browse.search.sort = app.browse.search.sort.cycle_with_mode(tag_mode);
                app.browse.search.sort_dir = match app.browse.search.sort {
                    super::browse::SearchSort::Score => super::browse::SortDir::Desc,
                    _ => super::browse::SortDir::Asc,
                };
                app.browse.search.last_keystroke = Some(std::time::Instant::now());
            }
            TuiButton::BrowseSearchAudioOnly => {
                app.browse.search.audio_only = !app.browse.search.audio_only;
                app.browse.search.last_keystroke = Some(std::time::Instant::now());
            }
            TuiButton::BrowseList => {
                // Catch-all for scroll routing only; ignore on left click.
            }
            TuiButton::BrowseBreadcrumb => {
                app.browse.open_path_input();
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
                let action = match &app.active_overlay {
                    ActiveOverlay::Confirmation { action, .. } => Some(action.clone()),
                    _ => None,
                };
                cancel_confirm_action(app, action.as_ref());
            }
            TuiButton::MetadataEntryRevert(idx) => {
                if let ActiveOverlay::MetadataEditor(ref mut state) = app.active_overlay {
                    if state.entries.get(idx).is_some() {
                        super::probe::toggle_mb_revert(&mut state.entries[idx]);
                        state.dirty = super::probe::metadata_editor_has_changes(state);
                    }
                }
            }
            // MbSelect / CuePreview / MetadataEditor-detail buttons are
            // handled directly in their dedicated mouse handlers before
            // reaching this generic dispatch.
            TuiButton::MbSelectRow(_)
            | TuiButton::MbSelectAccept
            | TuiButton::MbSelectCancel
            | TuiButton::CuePreviewLine(_)
            | TuiButton::CuePreviewSave
            | TuiButton::CuePreviewCancel
            | TuiButton::CuePreviewTop
            | TuiButton::CuePreviewBottom
            | TuiButton::CuePreviewEditCommit
            | TuiButton::CuePreviewEditCancel
            | TuiButton::MetadataDetailRevert
            | TuiButton::MetadataDetailRestore
            | TuiButton::MetadataEntryView(_) => {}

            // ── Template builder: open pills ──
            TuiButton::TemplateBuildFolderButton => {
                let initial = app.convert.output_options.folder_template.clone();
                app.active_overlay =
                    ActiveOverlay::TemplateBuilder(Box::new(TemplateBuilderState::new(
                        TemplateTarget::Folder,
                        &initial,
                        TemplateBuilderFocus::TemplateInput,
                    )));
            }
            TuiButton::TemplateBuildFilenameButton => {
                let initial = app.convert.output_options.filename_template.clone();
                app.active_overlay =
                    ActiveOverlay::TemplateBuilder(Box::new(TemplateBuilderState::new(
                        TemplateTarget::Filename,
                        &initial,
                        TemplateBuilderFocus::TemplateInput,
                    )));
            }
            TuiButton::TemplateLoadFolderButton => {
                open_template_picker(app, TemplateTarget::Folder);
            }
            TuiButton::TemplateLoadFilenameButton => {
                open_template_picker(app, TemplateTarget::Filename);
            }

            // ── Template builder overlay buttons (handled by dedicated mouse handler) ──
            TuiButton::TemplateBuilderToken(_)
            | TuiButton::TemplateBuilderSavedItem(_)
            | TuiButton::TemplateBuilderApply
            | TuiButton::TemplateBuilderSave
            | TuiButton::TemplateBuilderClear
            | TuiButton::TemplateBuilderDelete
            | TuiButton::TemplatePickerRow(_)
            | TuiButton::TemplatePickerApply
            | TuiButton::TemplatePickerDelete
            | TuiButton::TemplatePickerClose
            | TuiButton::FormatSettingsVerify(_)
            | TuiButton::FormatSettingsMd5(_)
            | TuiButton::FormatSettingsAacProfile(_)
            | TuiButton::FormatSettingsAacQuality(_)
            | TuiButton::FormatSettingsOpusContentType(_)
            | TuiButton::FormatSettingsOpusQuality(_)
            | TuiButton::FormatSettingsMp3Mode(_)
            | TuiButton::FormatSettingsMp3Preset(_)
            | TuiButton::FormatSettingsWavPackMode(_)
            | TuiButton::FormatSettingsWavPackHybrid(_)
            | TuiButton::FormatSettingsWavPackCorrection(_)
            | TuiButton::FormatSettingsSsrcMinPhase(_)
            | TuiButton::FormatSettingsSsrcPdf(_)
            | TuiButton::FormatSettingsSoxChebyshev(_)
            | TuiButton::FormatSettingsSoxAliasing(_)
            | TuiButton::FormatSettingsSoxSincPhase(_)
            | TuiButton::FormatSettingsSoxrChebyshev(_)
            | TuiButton::DiscBrowserStream(_)
            | TuiButton::DiscBrowserExpand(_)
            | TuiButton::DiscBrowserConvert
            | TuiButton::DiscBrowserClose
            | TuiButton::MetadataEditorTab(_) => {
                // Handled in dedicated mouse/overlay handlers; no-op here.
            }
        }
    }
}

/// Cycle the Convert screen's stream pill to the previous or next presentation.
fn cycle_stream_pill(app: &mut super::app::AppState, forward: bool) {
    use super::app::SourceMode;

    // Clone disc_contents and selected_presentation_id out of the current mode
    // before dropping the borrow (we need &mut app for set_source_mode).
    let (contents, current_id) = match &app.convert.source.mode {
        SourceMode::MultiTrack {
            disc_contents: Some(dc),
            selected_presentation_id,
            ..
        } if dc.presentations.len() >= 2 => {
            ((**dc).clone(), selected_presentation_id.clone())
        }
        _ => return,
    };

    let current_index = current_id
        .and_then(|id| contents.presentations.iter().position(|p| p.id == id))
        .unwrap_or(0);

    let count = contents.presentations.len();
    let new_index = if forward {
        (current_index + 1) % count
    } else {
        (current_index + count - 1) % count
    };

    if new_index == current_index {
        return;
    }

    let label = contents
        .presentations
        .get(new_index)
        .map(|p| p.label.clone())
        .unwrap_or_default();

    if let Err(e) = super::disc_browser_actions::switch_disc_presentation(app, contents, new_index) {
        app.set_status(format!("Stream switch failed: {}", e));
    } else {
        app.set_status(format!("Stream: {}", label));
    }
}

#[cfg(test)]
mod phase4_tests {
    //! Phase 4 + Phase 2 unit tests for the metadata editor's per-track
    //! CUESHEET round-trip. The functions under test are private to
    //! this module, so the tests live inline.

    use super::super::app::{MetadataEditorPhase, MetadataEditorState};
    use super::super::probe::TagEntry;
    use super::*;
    use crate::config::TonepoetConfig;
    use lofty::tag::ItemKey;

    fn dvd_audio_presentation(
        group_nr: u8,
        label: &str,
        codec: Option<&str>,
        sample_rate: Option<u32>,
        bit_depth: Option<u32>,
        channels: Option<u8>,
        channel_layout: Option<&str>,
    ) -> crate::disc::model::DiscPresentation {
        crate::disc::model::DiscPresentation {
            id: crate::disc::model::PresentationId::DvdAudioGroup(group_nr),
            label: label.to_string(),
            format: crate::disc::model::AudioPresentationFormat {
                codec: codec.map(str::to_string),
                sample_rate,
                bit_depth,
                channels,
                channel_layout: channel_layout.map(str::to_string),
                lossless: true,
                provenance: crate::disc::model::FormatProvenance::AobProbe,
            },
            tracks: Vec::new(),
            total_duration_secs: 0.0,
            album_title: None,
            album_artist: None,
            genre: None,
            year: None,
        }
    }

    #[test]
    fn dvda_tab_label_uses_disc_presentation_label_with_group_prefix() {
        let presentation = dvd_audio_presentation(
            1,
            "MLP 96kHz/24-bit 5.1",
            Some("MLP"),
            Some(96_000),
            Some(24),
            Some(6),
            Some("5.1"),
        );

        assert_eq!(
            dvda_presentation_tab_label(&presentation, 1),
            "Group 1: MLP 96kHz/24-bit 5.1"
        );
    }

    #[test]
    fn dvda_tab_label_does_not_duplicate_existing_group_prefix() {
        let presentation = dvd_audio_presentation(
            3,
            "Group 3: LPCM 96kHz/24-bit Stereo",
            Some("LPCM"),
            Some(96_000),
            Some(24),
            Some(2),
            Some("Stereo"),
        );

        assert_eq!(
            dvda_presentation_tab_label(&presentation, 3),
            "Group 3: LPCM 96kHz/24-bit Stereo"
        );
    }

    #[test]
    fn dvda_tab_label_falls_back_to_structured_format_fields() {
        let presentation = dvd_audio_presentation(
            2,
            "",
            Some("MLP"),
            Some(88_200),
            Some(24),
            Some(2),
            None,
        );

        assert_eq!(
            dvda_presentation_tab_label(&presentation, 2),
            "Group 2: MLP 88.2kHz/24-bit Stereo"
        );
    }

    fn entry(key: &str, item_key: ItemKey, vals: &[&str], origs: &[&str]) -> TagEntry {
        let v: Vec<String> = vals.iter().map(|s| s.to_string()).collect();
        let o: Vec<String> = origs.iter().map(|s| s.to_string()).collect();
        let all_same = v.windows(2).all(|w| w[0] == w[1]);
        let value = if v.len() > 1 && !all_same {
            "<multiple values>".to_string()
        } else {
            v.first().cloned().unwrap_or_default()
        };
        let original = o.first().cloned().unwrap_or_default();
        TagEntry {
            display_key: key.to_string(),
            item_key,
            value,
            original,
            is_binary: key.eq_ignore_ascii_case("CUESHEET"),
            is_mixed: v.len() > 1 && !all_same,
            per_file_values: v,
            per_file_originals: o,
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        }
    }

    /// Build a minimal MetadataEditorState with paths.len() == 1.
    fn single_image_state(entries: Vec<TagEntry>) -> MetadataEditorState {
        MetadataEditorState {
            paths: vec![std::path::PathBuf::from("/tmp/album.flac")],
            entries,
            cursor: 0,
            scroll: 0,
            last_click: None,
            edit_input: None,
            add_key_input: None,
            phase: MetadataEditorPhase::Editing,
            dirty: true,
            deleted: vec![],
            file_labels: vec!["01".to_string()],
            detail_field_idx: 0,
            detail_cursor: 0,
            detail_scroll: 0,
            detail_edit: None,
            mb_back: None,
            gnudb_back: None,
            read_only: false,
            sacd_sidecar_path: None,
            sacd_area_kind: None,
            sacd_stereo_durations: None,
            sacd_multi_channel_durations: None,
            presentation_tabs: Vec::new(),
            active_tab: 0,
        }
    }

    fn dvda_multitab_state(tabs: Vec<super::super::app::PresentationTab>) -> MetadataEditorState {
        let first = tabs.first().expect("at least one tab").clone();
        MetadataEditorState {
            paths: first.paths.clone(),
            entries: first.entries.clone(),
            cursor: 0,
            scroll: 0,
            last_click: None,
            edit_input: None,
            add_key_input: None,
            phase: MetadataEditorPhase::Editing,
            dirty: first.dirty,
            deleted: first.deleted.clone(),
            file_labels: first.file_labels.clone(),
            detail_field_idx: 0,
            detail_cursor: 0,
            detail_scroll: 0,
            detail_edit: None,
            mb_back: None,
            gnudb_back: None,
            read_only: false,
            sacd_sidecar_path: None,
            sacd_area_kind: None,
            sacd_stereo_durations: None,
            sacd_multi_channel_durations: None,
            presentation_tabs: tabs,
            active_tab: 0,
        }
    }

    fn dvda_save_tab(group_nr: u8, entries: Vec<TagEntry>) -> super::super::app::PresentationTab {
        super::super::app::PresentationTab {
            id: crate::disc::model::PresentationId::DvdAudioGroup(group_nr),
            label: format!("Group {}", group_nr),
            paths: vec![
                std::path::PathBuf::from(format!("/tmp/group-{}.aob", group_nr)),
                std::path::PathBuf::from(format!("/tmp/group-{}.aob", group_nr)),
            ],
            entries,
            file_labels: vec!["01".to_string(), "02".to_string()],
            deleted: Vec::new(),
            dirty: true,
            sacd_area_kind: None,
            sacd_stereo_durations: None,
            sacd_multi_channel_durations: None,
        }
    }

    fn dvda_metabase_track(
        id: &str,
        pairs: &[(&str, &str)],
    ) -> crate::tui::dvda_metabase::DvdaMetabaseTrack {
        let mut meta = std::collections::BTreeMap::new();
        for (key, value) in pairs {
            meta.insert((*key).to_string(), (*value).to_string());
        }
        crate::tui::dvda_metabase::DvdaMetabaseTrack {
            id: id.to_string(),
            meta,
        }
    }

    #[test]
    fn mouse_cancel_apply_mb_to_all_restores_parked_editor() {
        let mut app = AppState::new(TonepoetConfig::default());
        let tabs = vec![
            dvda_save_tab(
                1,
                vec![entry(
                    "TITLE",
                    ItemKey::TrackTitle,
                    &["Populated One", "Populated Two"],
                    &["Old One", "Old Two"],
                )],
            ),
            dvda_save_tab(
                3,
                vec![entry(
                    "TITLE",
                    ItemKey::TrackTitle,
                    &["Stereo One", "Stereo Two"],
                    &["Stereo Old One", "Stereo Old Two"],
                )],
            ),
        ];
        let state = Box::new(dvda_multitab_state(tabs));

        reopen_metadata_editor_after_musicbrainz_population(&mut app, state);
        assert!(matches!(app.active_overlay, ActiveOverlay::Confirmation { .. }));
        assert!(app.pending_metadata_editor.is_some());

        app.button_map
            .record_button(TuiButton::OverlayCancel, Rect::new(10, 10, 8, 1));
        let (tx, _rx) = mpsc::channel(1);
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 11,
                row: 10,
                modifiers: KeyModifiers::NONE,
            },
            &tx,
        );

        assert!(app.pending_metadata_editor.is_none());
        match &app.active_overlay {
            ActiveOverlay::MetadataEditor(restored) => {
                assert_eq!(restored.presentation_tabs.len(), 2);
                assert_eq!(restored.entries[0].per_file_values[0], "Populated One");
            }
            other => panic!("expected restored metadata editor, got {:?}", other),
        }
    }

    #[test]
    fn save_dvda_metabase_writes_multi_tab_groups_round_trip() {
        use crate::tui::dvda_metabase::{
            parse_metabase, track_value, write_metabase, DvdaMetabase,
        };

        let mut metabase = DvdaMetabase {
            store_id: "0123456789ABCDEF0123456789ABCDEF".to_string(),
            tracks: vec![
                dvda_metabase_track(
                    "1.1.1",
                    &[
                        ("ALBUM", "Old Surround"),
                        ("TITLE", "Old Surround 1"),
                        ("TRACKNUMBER", "1"),
                        ("TOTALTRACKS", "2"),
                        ("CUSTOM_KEEP", "surround foreign"),
                    ],
                ),
                dvda_metabase_track(
                    "1.1.2",
                    &[
                        ("ALBUM", "Old Surround"),
                        ("TITLE", "Old Surround 2"),
                        ("TRACKNUMBER", "2"),
                        ("TOTALTRACKS", "2"),
                    ],
                ),
                dvda_metabase_track(
                    "2.1.1",
                    &[
                        ("ALBUM", "Old Stereo"),
                        ("TITLE", "Old Stereo 1"),
                        ("TRACKNUMBER", "1"),
                        ("TOTALTRACKS", "2"),
                        ("CUSTOM_KEEP", "stereo foreign"),
                    ],
                ),
                dvda_metabase_track(
                    "2.1.2",
                    &[
                        ("ALBUM", "Old Stereo"),
                        ("TITLE", "Old Stereo 2"),
                        ("TRACKNUMBER", "2"),
                        ("TOTALTRACKS", "2"),
                    ],
                ),
            ],
        };
        let seeded = metabase.clone();
        let tabs = vec![
            dvda_save_tab(
                1,
                vec![
                    entry(
                        "DVDA_GROUP",
                        ItemKey::Unknown("DVDA_GROUP".to_string()),
                        &["1", "1"],
                        &["1", "1"],
                    ),
                    entry(
                        "ALBUM",
                        ItemKey::AlbumTitle,
                        &["Surround Album", "Surround Album"],
                        &["Old Surround", "Old Surround"],
                    ),
                    entry(
                        "TITLE",
                        ItemKey::TrackTitle,
                        &["Surround One", "Surround Two"],
                        &["Old Surround 1", "Old Surround 2"],
                    ),
                    entry(
                        "ARTIST",
                        ItemKey::TrackArtist,
                        &["Surround Artist 1", "Surround Artist 2"],
                        &["", ""],
                    ),
                ],
            ),
            dvda_save_tab(
                3,
                vec![
                    entry(
                        "DVDA_GROUP",
                        ItemKey::Unknown("DVDA_GROUP".to_string()),
                        &["3", "3"],
                        &["3", "3"],
                    ),
                    entry(
                        "ALBUM",
                        ItemKey::AlbumTitle,
                        &["Stereo Album", "Stereo Album"],
                        &["Old Stereo", "Old Stereo"],
                    ),
                    entry(
                        "TITLE",
                        ItemKey::TrackTitle,
                        &["Stereo One", "Stereo Two"],
                        &["Old Stereo 1", "Old Stereo 2"],
                    ),
                    entry(
                        "ARTIST",
                        ItemKey::TrackArtist,
                        &["Stereo Artist 1", "Stereo Artist 2"],
                        &["", ""],
                    ),
                ],
            ),
        ];

        apply_dvda_presentation_tabs_to_metabase(
            &mut metabase,
            &seeded,
            &tabs,
            |group_nr| match group_nr {
                1 => Ok(vec!["1.1.1".to_string(), "1.1.2".to_string()]),
                3 => Ok(vec!["2.1.1".to_string(), "2.1.2".to_string()]),
                other => Err(format!("unexpected test group {}", other)),
            },
        )
        .expect("apply DVD-Audio tabs");

        let td = tempfile::tempdir().expect("tempdir");
        let xml_path = td.path().join("dvda.xml");
        write_metabase(&metabase, &xml_path).expect("write metabase");
        let reparsed = parse_metabase(&xml_path).expect("re-parse metabase");

        let value = |id: &str, key: &str| track_value(Some(&reparsed), id, &[key]);

        assert_eq!(value("1.1.1", "ALBUM").as_deref(), Some("Surround Album"));
        assert_eq!(value("1.1.2", "ALBUM").as_deref(), Some("Surround Album"));
        assert_eq!(value("2.1.1", "ALBUM").as_deref(), Some("Stereo Album"));
        assert_eq!(value("2.1.2", "ALBUM").as_deref(), Some("Stereo Album"));

        assert_eq!(value("1.1.1", "TITLE").as_deref(), Some("Surround One"));
        assert_eq!(value("1.1.2", "TITLE").as_deref(), Some("Surround Two"));
        assert_eq!(value("2.1.1", "TITLE").as_deref(), Some("Stereo One"));
        assert_eq!(value("2.1.2", "TITLE").as_deref(), Some("Stereo Two"));

        assert_eq!(value("1.1.1", "ARTIST").as_deref(), Some("Surround Artist 1"));
        assert_eq!(value("2.1.1", "ARTIST").as_deref(), Some("Stereo Artist 1"));
        assert_eq!(value("1.1.1", "CUSTOM_KEEP").as_deref(), Some("surround foreign"));
        assert_eq!(value("2.1.1", "CUSTOM_KEEP").as_deref(), Some("stereo foreign"));
        assert_eq!(value("1.1.1", "DVDA_GROUP"), None);
        assert_eq!(value("2.1.1", "DVDA_GROUP"), None);
    }

    #[test]
    fn save_dvda_metabase_loaded_context_writes_multi_tab_groups_round_trip() {
        use crate::tui::dvda_metabase::{
            parse_metabase, track_value, write_metabase, DvdaMetabase,
        };

        let existing = DvdaMetabase {
            store_id: "0123456789ABCDEF0123456789ABCDEF".to_string(),
            tracks: vec![
                dvda_metabase_track(
                    "1.1.1",
                    &[("ALBUM", "Old Surround"), ("TITLE", "Old Surround 1")],
                ),
                dvda_metabase_track(
                    "1.1.2",
                    &[("ALBUM", "Old Surround"), ("TITLE", "Old Surround 2")],
                ),
                dvda_metabase_track(
                    "2.1.1",
                    &[("ALBUM", "Old Stereo"), ("TITLE", "Old Stereo 1")],
                ),
                dvda_metabase_track(
                    "2.1.2",
                    &[("ALBUM", "Old Stereo"), ("TITLE", "Old Stereo 2")],
                ),
            ],
        };
        let seeded = existing.clone();
        let tabs = vec![
            dvda_save_tab(
                1,
                vec![
                    entry(
                        "DVDA_GROUP",
                        ItemKey::Unknown("DVDA_GROUP".to_string()),
                        &["1", "1"],
                        &["1", "1"],
                    ),
                    entry(
                        "ALBUM",
                        ItemKey::AlbumTitle,
                        &["Full Path Surround", "Full Path Surround"],
                        &["Old Surround", "Old Surround"],
                    ),
                    entry(
                        "TITLE",
                        ItemKey::TrackTitle,
                        &["Full Surround One", "Full Surround Two"],
                        &["Old Surround 1", "Old Surround 2"],
                    ),
                ],
            ),
            dvda_save_tab(
                3,
                vec![
                    entry(
                        "DVDA_GROUP",
                        ItemKey::Unknown("DVDA_GROUP".to_string()),
                        &["3", "3"],
                        &["3", "3"],
                    ),
                    entry(
                        "ALBUM",
                        ItemKey::AlbumTitle,
                        &["Full Path Stereo", "Full Path Stereo"],
                        &["Old Stereo", "Old Stereo"],
                    ),
                    entry(
                        "TITLE",
                        ItemKey::TrackTitle,
                        &["Full Stereo One", "Full Stereo Two"],
                        &["Old Stereo 1", "Old Stereo 2"],
                    ),
                ],
            ),
        ];
        let state = dvda_multitab_state(tabs);

        let td = tempfile::tempdir().expect("tempdir");
        let xml_path = td.path().join("dvda.xml");
        write_metabase(&existing, &xml_path).expect("write existing metabase");

        let kind = save_dvda_metabase_with_loaded_context(
            &state,
            &xml_path,
            &seeded,
            vec!["1.1.1".to_string(), "1.1.2".to_string()],
            |group_nr| match group_nr {
                1 => Ok(vec!["1.1.1".to_string(), "1.1.2".to_string()]),
                3 => Ok(vec!["2.1.1".to_string(), "2.1.2".to_string()]),
                other => Err(format!("unexpected test group {}", other)),
            },
        )
        .expect("save DVD-Audio metabase through loaded-context save path");
        assert_eq!(kind, SacdSaveKind::Updated);

        let reparsed = parse_metabase(&xml_path).expect("re-parse metabase");
        let value = |id: &str, key: &str| track_value(Some(&reparsed), id, &[key]);

        assert_eq!(value("1.1.1", "ALBUM").as_deref(), Some("Full Path Surround"));
        assert_eq!(value("1.1.2", "TITLE").as_deref(), Some("Full Surround Two"));
        assert_eq!(value("2.1.1", "ALBUM").as_deref(), Some("Full Path Stereo"));
        assert_eq!(value("2.1.2", "TITLE").as_deref(), Some("Full Stereo Two"));
        assert_eq!(value("1.1.1", "DVDA_GROUP"), None);
        assert_eq!(value("2.1.1", "DVDA_GROUP"), None);
    }

    /// CUE template for a 3-track image used across regen tests.
    const CUE_TEMPLATE: &str = "TITLE \"Old Album\"\n\
         PERFORMER \"Old Artist\"\n\
         REM DATE \"1977\"\n\
         FILE \"album.flac\" FLAC\n\
           TRACK 01 AUDIO\n\
             TITLE \"Track 1\"\n\
             PERFORMER \"Old Artist\"\n\
             INDEX 01 00:00:00\n\
           TRACK 02 AUDIO\n\
             TITLE \"Track 2\"\n\
             PERFORMER \"Old Artist\"\n\
             INDEX 01 03:00:00\n\
           TRACK 03 AUDIO\n\
             TITLE \"Track 3\"\n\
             PERFORMER \"Old Artist\"\n\
             INDEX 01 06:30:00\n";

    // ------------------------- Phase 2 -------------------------

    #[test]
    fn apply_embedded_cuesheet_grows_title_to_per_track_dim() {
        let mut entries = vec![entry(
            "CUESHEET",
            ItemKey::Unknown("CUESHEET".into()),
            &[CUE_TEMPLATE],
            &[CUE_TEMPLATE],
        )];
        apply_embedded_cuesheet_per_track(&mut entries);
        let t = entries
            .iter()
            .find(|e| e.display_key == "TITLE")
            .expect("TITLE created");
        assert_eq!(t.per_file_values, vec!["Track 1", "Track 2", "Track 3"]);
        assert_eq!(t.per_file_originals, vec!["Track 1", "Track 2", "Track 3"]);
        assert!(
            t.is_mixed,
            "three differing per-track titles must mark mixed"
        );
        let a = entries
            .iter()
            .find(|e| e.display_key == "ARTIST")
            .expect("ARTIST created");
        assert_eq!(
            a.per_file_values,
            vec!["Old Artist", "Old Artist", "Old Artist"]
        );
        assert!(!a.is_mixed, "uniform per-track artists not mixed");
    }

    #[test]
    fn apply_embedded_cuesheet_skips_when_no_cuesheet_entry() {
        let mut entries = vec![entry("ALBUM", ItemKey::AlbumTitle, &["X"], &["X"])];
        apply_embedded_cuesheet_per_track(&mut entries);
        // No TITLE / ARTIST / ISRC entries created.
        assert!(entries.iter().all(|e| e.display_key == "ALBUM"));
    }

    #[test]
    fn apply_embedded_cuesheet_skips_single_track_cue() {
        let single_track = "TITLE \"X\"\nFILE \"a.flac\" FLAC\n  TRACK 01 AUDIO\n    TITLE \"X\"\n    INDEX 01 00:00:00\n";
        let mut entries = vec![entry(
            "CUESHEET",
            ItemKey::Unknown("CUESHEET".into()),
            &[single_track],
            &[single_track],
        )];
        apply_embedded_cuesheet_per_track(&mut entries);
        // Single-track CUE: no point growing TITLE per-track.
        assert!(entries.iter().find(|e| e.display_key == "TITLE").is_none());
    }

    // ------------------------- Phase 4 -------------------------

    #[test]
    fn regen_skips_when_nothing_dirty() {
        // CUESHEET present, TITLE per-track but values match originals.
        let mut state = single_image_state(vec![
            entry(
                "CUESHEET",
                ItemKey::Unknown("CUESHEET".into()),
                &[CUE_TEMPLATE],
                &[CUE_TEMPLATE],
            ),
            entry(
                "TITLE",
                ItemKey::TrackTitle,
                &["Track 1", "Track 2", "Track 3"],
                &["Track 1", "Track 2", "Track 3"],
            ),
        ]);
        let result = regenerate_cuesheet_for_save(&mut state).expect("ok");
        assert!(!result, "Nothing dirty → no regen");
    }

    #[test]
    fn regen_per_track_edit_writes_new_cue() {
        let mut state = single_image_state(vec![
            entry(
                "CUESHEET",
                ItemKey::Unknown("CUESHEET".into()),
                &[CUE_TEMPLATE],
                &[CUE_TEMPLATE],
            ),
            entry(
                "TITLE",
                ItemKey::TrackTitle,
                &["Track 1", "EDITED", "Track 3"],
                &["Track 1", "Track 2", "Track 3"],
            ),
        ]);
        let result = regenerate_cuesheet_for_save(&mut state).expect("ok");
        assert!(result, "per-track edit → regen ran");
        let cue_idx = state
            .entries
            .iter()
            .position(|e| e.display_key == "CUESHEET")
            .unwrap();
        let new_cue = &state.entries[cue_idx].per_file_values[0];
        assert!(
            new_cue.contains("TITLE \"EDITED\""),
            "regenerated CUE must include the edited title"
        );
        assert!(new_cue.contains("TITLE \"Track 1\""));
        assert!(new_cue.contains("TITLE \"Track 3\""));
        // INDEX timestamps must be preserved from the parsed template.
        assert!(new_cue.contains("INDEX 01 00:00:00"));
        assert!(new_cue.contains("INDEX 01 03:00:00"));
        assert!(new_cue.contains("INDEX 01 06:30:00"));
    }

    #[test]
    fn regen_beta_album_rederive_picks_up_album_edits() {
        // CUESHEET present (3-track template); ALBUM dirty (changed
        // from "Old Album" to "New Album"); no per-track dirt.
        let mut state = single_image_state(vec![
            entry(
                "CUESHEET",
                ItemKey::Unknown("CUESHEET".into()),
                &[CUE_TEMPLATE],
                &[CUE_TEMPLATE],
            ),
            entry("ALBUM", ItemKey::AlbumTitle, &["New Album"], &["Old Album"]),
        ]);
        let result = regenerate_cuesheet_for_save(&mut state).expect("ok");
        assert!(result, "album-level dirt with CUESHEET → regen ran");
        let cue_idx = state
            .entries
            .iter()
            .position(|e| e.display_key == "CUESHEET")
            .unwrap();
        let new_cue = &state.entries[cue_idx].per_file_values[0];
        assert!(
            new_cue.contains("TITLE \"New Album\""),
            "β re-derive must update CUE album title"
        );
        // Track titles preserved (no per-track override).
        assert!(new_cue.contains("TITLE \"Track 1\""));
        assert!(new_cue.contains("TITLE \"Track 2\""));
    }

    #[test]
    fn regen_refuses_per_track_dirty_without_cuesheet() {
        // Per-track TITLE differs from originals, but no CUESHEET
        // anchor exists. Must refuse with status.
        let mut state = single_image_state(vec![entry(
            "TITLE",
            ItemKey::TrackTitle,
            &["a", "EDITED", "c"],
            &["a", "b", "c"],
        )]);
        let result = regenerate_cuesheet_for_save(&mut state);
        assert!(result.is_err(), "per-track dirt + no CUESHEET → Err");
        assert!(result.unwrap_err().contains("without an embedded CUESHEET"));
    }

    #[test]
    fn regen_album_only_dirty_without_cuesheet_is_noop() {
        // No CUESHEET; only ALBUM dirty. Multi-file or single-file
        // editor without an embedded CUE — normal save path applies.
        let mut state = single_image_state(vec![entry(
            "ALBUM",
            ItemKey::AlbumTitle,
            &["New"],
            &["Old"],
        )]);
        let result = regenerate_cuesheet_for_save(&mut state).expect("ok");
        assert!(
            !result,
            "album-only dirt without CUESHEET → Ok(false), normal save"
        );
    }

    #[test]
    fn regen_refuses_track_count_divergence() {
        // CUESHEET has 3 tracks, TITLE has dim 5 → divergence.
        let mut state = single_image_state(vec![
            entry(
                "CUESHEET",
                ItemKey::Unknown("CUESHEET".into()),
                &[CUE_TEMPLATE],
                &[CUE_TEMPLATE],
            ),
            entry(
                "TITLE",
                ItemKey::TrackTitle,
                &["t1", "t2", "t3", "t4", "t5"],
                &["", "", "", "", ""],
            ),
        ]);
        let result = regenerate_cuesheet_for_save(&mut state);
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("TITLE has 5 per-track values"));
        assert!(msg.contains("CUESHEET has 3 tracks"));
    }

    #[test]
    fn regen_refuses_when_cuesheet_marked_deleted_with_per_track_dirty() {
        let mut state = single_image_state(vec![
            entry(
                "CUESHEET",
                ItemKey::Unknown("CUESHEET".into()),
                &[CUE_TEMPLATE],
                &[CUE_TEMPLATE],
            ),
            entry(
                "TITLE",
                ItemKey::TrackTitle,
                &["Track 1", "EDITED", "Track 3"],
                &["Track 1", "Track 2", "Track 3"],
            ),
        ]);
        state.deleted.push(0); // CUESHEET marked deleted
        let result = regenerate_cuesheet_for_save(&mut state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("CUESHEET marked deleted"));
    }

    #[test]
    fn regen_refuses_empty_parsed_cuesheet_with_per_track_dirty() {
        // CUESHEET value parses to no tracks (e.g. empty string after
        // a Phase-5-failure; user shouldn't normally see this state
        // but defensive check guards against silent data loss).
        let mut state = single_image_state(vec![
            entry(
                "CUESHEET",
                ItemKey::Unknown("CUESHEET".into()),
                &[""],
                &[""],
            ),
            entry("TITLE", ItemKey::TrackTitle, &["a", "EDITED"], &["a", "b"]),
        ]);
        let result = regenerate_cuesheet_for_save(&mut state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("zero tracks"));
    }

    #[test]
    fn regen_uses_per_file_values_not_originals_as_template() {
        // Phase-5-just-created scenario: per_file_originals[0]="", but
        // per_file_values[0] holds the freshly-generated CUE. The
        // function must parse VALUES, not ORIGINALS, otherwise it'd
        // see empty tracks and refuse on per-track dirt.
        let mut state = single_image_state(vec![
            entry(
                "CUESHEET",
                ItemKey::Unknown("CUESHEET".into()),
                &[CUE_TEMPLATE],
                &[""],
            ),
            entry(
                "TITLE",
                ItemKey::TrackTitle,
                &["Track 1", "EDITED", "Track 3"],
                &["", "", ""],
            ),
        ]);
        let result = regenerate_cuesheet_for_save(&mut state).expect("ok");
        assert!(
            result,
            "freshly-created CUESHEET (originals empty) → regen succeeds via values[0]"
        );
    }

    #[test]
    fn regen_isrc_per_track_override_lands_in_cue() {
        let mut state = single_image_state(vec![
            entry(
                "CUESHEET",
                ItemKey::Unknown("CUESHEET".into()),
                &[CUE_TEMPLATE],
                &[CUE_TEMPLATE],
            ),
            entry(
                "ISRC",
                ItemKey::Isrc,
                &["USRC0000001", "USRC0000002", "USRC0000003"],
                &["", "", ""],
            ),
        ]);
        let result = regenerate_cuesheet_for_save(&mut state).expect("ok");
        assert!(result);
        let new_cue = &state
            .entries
            .iter()
            .find(|e| e.display_key == "CUESHEET")
            .unwrap()
            .per_file_values[0];
        assert!(new_cue.contains("ISRC USRC0000001"));
        assert!(new_cue.contains("ISRC USRC0000002"));
        assert!(new_cue.contains("ISRC USRC0000003"));
    }

    // -------- inject_sidecar_cuesheet_if_present --------

    fn write_sidecar(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).expect("write sidecar");
        p
    }

    /// Multi-track single-image CUE used as the well-formed input.
    const SIDECAR_3_TRACK_SINGLE_IMAGE: &str = "TITLE \"Album\"\n\
         FILE \"image.flac\" FLAC\n\
           TRACK 01 AUDIO\n\
             TITLE \"T1\"\n\
             INDEX 01 00:00:00\n\
           TRACK 02 AUDIO\n\
             TITLE \"T2\"\n\
             INDEX 01 03:00:00\n\
           TRACK 03 AUDIO\n\
             TITLE \"T3\"\n\
             INDEX 01 06:30:00\n";

    #[test]
    fn inject_sidecar_cuesheet_creates_synthetic_entry_when_no_embedded() {
        let td = tempfile::tempdir().expect("tempdir");
        write_sidecar(td.path(), "album.cue", SIDECAR_3_TRACK_SINGLE_IMAGE);
        let audio = td.path().join("album.flac");

        let mut entries: Vec<TagEntry> =
            vec![entry("ALBUM", ItemKey::AlbumTitle, &["Album"], &["Album"])];
        inject_sidecar_cuesheet_if_present(&mut entries, &audio);

        let cue = entries
            .iter()
            .find(|e| e.display_key == "CUESHEET")
            .expect("synthetic CUESHEET injected");
        assert_eq!(
            cue.per_file_values,
            vec![SIDECAR_3_TRACK_SINGLE_IMAGE.to_string()]
        );
        // originals=="" → save loop will write a fresh embedded tag.
        assert_eq!(cue.per_file_originals, vec!["".to_string()]);
        assert!(cue.is_binary);
        assert!(!cue.is_mixed);
    }

    #[test]
    fn inject_sidecar_cuesheet_noop_when_embedded_present() {
        let td = tempfile::tempdir().expect("tempdir");
        write_sidecar(td.path(), "album.cue", SIDECAR_3_TRACK_SINGLE_IMAGE);
        let audio = td.path().join("album.flac");

        let existing_embedded = "TITLE \"Embedded\"\nFILE \"x\" FLAC\n  TRACK 01 AUDIO\n    TITLE \"E1\"\n    INDEX 01 00:00:00\n";
        let mut entries: Vec<TagEntry> = vec![entry(
            "CUESHEET",
            ItemKey::Unknown("CUESHEET".into()),
            &[existing_embedded],
            &[existing_embedded],
        )];
        let before_len = entries.len();
        inject_sidecar_cuesheet_if_present(&mut entries, &audio);
        assert_eq!(entries.len(), before_len, "no entry added");
        // Existing CUESHEET unchanged.
        let cue = entries
            .iter()
            .find(|e| e.display_key == "CUESHEET")
            .unwrap();
        assert_eq!(cue.per_file_values, vec![existing_embedded.to_string()]);
    }

    #[test]
    fn inject_sidecar_cuesheet_skips_track_by_track_cue() {
        // Track-by-track structure: each TRACK has its own FILE,
        // INDEX 01 resets per file. Not safe to embed against a
        // single-image audio.
        let track_by_track = "TITLE \"Album\"\n\
             FILE \"track01.flac\" WAVE\n\
               TRACK 01 AUDIO\n\
                 INDEX 01 00:00:00\n\
             FILE \"track02.flac\" WAVE\n\
               TRACK 02 AUDIO\n\
                 INDEX 01 00:00:00\n";
        let td = tempfile::tempdir().expect("tempdir");
        write_sidecar(td.path(), "album.cue", track_by_track);
        let audio = td.path().join("album.flac");

        let mut entries: Vec<TagEntry> = vec![];
        inject_sidecar_cuesheet_if_present(&mut entries, &audio);
        assert!(entries
            .iter()
            .find(|e| e.display_key == "CUESHEET")
            .is_none());
    }

    #[test]
    fn inject_sidecar_cuesheet_skips_single_track_cue() {
        let single = "TITLE \"X\"\nFILE \"x.flac\" FLAC\n  TRACK 01 AUDIO\n    TITLE \"X\"\n    INDEX 01 00:00:00\n";
        let td = tempfile::tempdir().expect("tempdir");
        write_sidecar(td.path(), "album.cue", single);
        let audio = td.path().join("album.flac");

        let mut entries: Vec<TagEntry> = vec![];
        inject_sidecar_cuesheet_if_present(&mut entries, &audio);
        assert!(entries
            .iter()
            .find(|e| e.display_key == "CUESHEET")
            .is_none());
    }

    #[test]
    fn inject_sidecar_cuesheet_handles_non_utf8_sidecar() {
        // Japanese rips ship Shift_JIS-encoded .cue files. read_to_string
        // would have refused them; lossy-decode accepts and CUE keywords
        // (TITLE/PERFORMER/INDEX/etc.) are pure ASCII so structure parses.
        let td = tempfile::tempdir().expect("tempdir");
        // Build a CUE with a Shift_JIS-encoded title byte sequence.
        // 0x82 0xA0 = 'あ' in Shift_JIS, invalid as UTF-8.
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(b"TITLE \"");
        bytes.extend_from_slice(&[0x82, 0xA0]);
        bytes.extend_from_slice(b"\"\nFILE \"album.flac\" FLAC\n  TRACK 01 AUDIO\n    TITLE \"T1\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"T2\"\n    INDEX 01 00:01:00\n");
        let cue_path = td.path().join("album.cue");
        std::fs::write(&cue_path, &bytes).expect("write");
        let audio = td.path().join("album.flac");

        let mut entries: Vec<TagEntry> = vec![];
        inject_sidecar_cuesheet_if_present(&mut entries, &audio);
        // Inject succeeded — Shift_JIS bytes lossy-decoded; CUE keywords
        // intact.
        assert!(
            entries
                .iter()
                .find(|e| e.display_key == "CUESHEET")
                .is_some(),
            "non-UTF-8 sidecar must still inject (lossy decode)"
        );
    }

    // -------- :tags-cue-sidecar (reload_from_sidecar_cue) --------

    fn write_sidecar_at(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        write_sidecar(dir, "album.cue", body)
    }

    /// Build a single-image MetadataEditorState whose paths[0] points
    /// at `dir/album.flac` (file doesn't have to exist on disk —
    /// reload_from_sidecar_cue only reads the sidecar).
    fn state_for_sidecar_test(
        dir: &std::path::Path,
        existing_entries: Vec<TagEntry>,
    ) -> MetadataEditorState {
        MetadataEditorState {
            paths: vec![dir.join("album.flac")],
            entries: existing_entries,
            cursor: 0,
            scroll: 0,
            last_click: None,
            edit_input: None,
            add_key_input: None,
            phase: MetadataEditorPhase::Editing,
            dirty: false,
            deleted: vec![],
            file_labels: vec!["01".to_string()],
            detail_field_idx: 0,
            detail_cursor: 0,
            detail_scroll: 0,
            detail_edit: None,
            mb_back: None,
            gnudb_back: None,
            read_only: false,
            sacd_sidecar_path: None,
            sacd_area_kind: None,
            sacd_stereo_durations: None,
            sacd_multi_channel_durations: None,
            presentation_tabs: Vec::new(),
            active_tab: 0,
        }
    }

    #[test]
    fn reload_sidecar_overlays_per_track_values_preserving_originals() {
        let td = tempfile::tempdir().expect("tempdir");
        write_sidecar_at(td.path(), SIDECAR_3_TRACK_SINGLE_IMAGE);
        // Existing TITLE entry with prior values (e.g. from Phase 2
        // sidecar inject before the user edited the sidecar externally).
        let mut state = state_for_sidecar_test(
            td.path(),
            vec![entry(
                "TITLE",
                ItemKey::TrackTitle,
                &["Old1", "Old2", "Old3"],
                &["Old1", "Old2", "Old3"],
            )],
        );
        let result = reload_from_sidecar_cue(&mut state);
        assert!(result.is_ok(), "reload should succeed: {:?}", result);

        let title = state
            .entries
            .iter()
            .find(|e| e.display_key == "TITLE")
            .unwrap();
        assert_eq!(
            title.per_file_values,
            vec!["T1", "T2", "T3"],
            "values overwritten with sidecar's"
        );
        assert_eq!(
            title.per_file_originals,
            vec!["Old1", "Old2", "Old3"],
            "originals preserved (revert restores prior state)"
        );
        assert!(state.dirty, "values diverged from originals → dirty=true");
    }

    #[test]
    fn reload_sidecar_creates_synthetic_cuesheet_entry() {
        let td = tempfile::tempdir().expect("tempdir");
        write_sidecar_at(td.path(), SIDECAR_3_TRACK_SINGLE_IMAGE);
        let mut state = state_for_sidecar_test(td.path(), vec![]);
        let result = reload_from_sidecar_cue(&mut state);
        assert!(result.is_ok());

        let cue = state
            .entries
            .iter()
            .find(|e| e.display_key == "CUESHEET")
            .unwrap();
        assert_eq!(cue.per_file_values[0], SIDECAR_3_TRACK_SINGLE_IMAGE);
        assert_eq!(
            cue.per_file_originals[0], "",
            "originals=\"\" so save writes a fresh embedded CUESHEET tag"
        );
        assert!(cue.is_binary);
    }

    #[test]
    fn reload_sidecar_overrides_existing_embedded_cuesheet() {
        // File has embedded CUESHEET (originals=on-disk value); sidecar
        // is different. After reload, values come from sidecar but
        // originals are preserved (the on-disk embedded CUE).
        let td = tempfile::tempdir().expect("tempdir");
        write_sidecar_at(td.path(), SIDECAR_3_TRACK_SINGLE_IMAGE);
        let on_disk = "TITLE \"Old\"\nFILE \"x.flac\" FLAC\n  TRACK 01 AUDIO\n    TITLE \"OE1\"\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    TITLE \"OE2\"\n    INDEX 01 00:00:50\n";
        let mut state = state_for_sidecar_test(
            td.path(),
            vec![entry(
                "CUESHEET",
                ItemKey::Unknown("CUESHEET".into()),
                &[on_disk],
                &[on_disk],
            )],
        );
        let result = reload_from_sidecar_cue(&mut state);
        assert!(result.is_ok());

        let cue = state
            .entries
            .iter()
            .find(|e| e.display_key == "CUESHEET")
            .unwrap();
        assert_eq!(
            cue.per_file_values[0], SIDECAR_3_TRACK_SINGLE_IMAGE,
            "CUESHEET value overridden with sidecar"
        );
        assert_eq!(
            cue.per_file_originals[0], on_disk,
            "CUESHEET originals preserved (embedded on-disk value)"
        );
    }

    #[test]
    fn reload_sidecar_clears_existing_isrc_when_sidecar_has_none() {
        // Bug 1 fix: existing ISRC entry with non-empty values; sidecar
        // has no ISRC fields. After reload, ISRC must reflect sidecar
        // (cleared), not stay at old values.
        let td = tempfile::tempdir().expect("tempdir");
        write_sidecar_at(td.path(), SIDECAR_3_TRACK_SINGLE_IMAGE);
        // SIDECAR_3_TRACK_SINGLE_IMAGE has no ISRC lines.
        let mut state = state_for_sidecar_test(
            td.path(),
            vec![entry(
                "ISRC",
                ItemKey::Isrc,
                &["USRC1", "USRC2", "USRC3"],
                &["USRC1", "USRC2", "USRC3"],
            )],
        );
        reload_from_sidecar_cue(&mut state).expect("ok");
        let isrc = state
            .entries
            .iter()
            .find(|e| e.display_key == "ISRC")
            .unwrap();
        assert_eq!(
            isrc.per_file_values,
            vec!["", "", ""],
            "existing ISRC entry overlaid with sidecar's empties"
        );
        // Originals preserved → revert restores prior ISRCs.
        assert_eq!(isrc.per_file_originals, vec!["USRC1", "USRC2", "USRC3"]);
    }

    #[test]
    fn reload_sidecar_resizes_originals_when_sidecar_dim_differs() {
        // Bug 2 fix: existing TITLE has dim 5 (e.g., from a prior CUE
        // with 5 tracks); sidecar now has 3 tracks. After overlay,
        // both per_file_values and per_file_originals must be dim 3
        // (originals truncated). Otherwise the len-invariant breaks.
        let td = tempfile::tempdir().expect("tempdir");
        write_sidecar_at(td.path(), SIDECAR_3_TRACK_SINGLE_IMAGE);
        let mut state = state_for_sidecar_test(
            td.path(),
            vec![entry(
                "TITLE",
                ItemKey::TrackTitle,
                &["A", "B", "C", "D", "E"],
                &["A", "B", "C", "D", "E"],
            )],
        );
        reload_from_sidecar_cue(&mut state).expect("ok");
        let title = state
            .entries
            .iter()
            .find(|e| e.display_key == "TITLE")
            .unwrap();
        assert_eq!(
            title.per_file_values.len(),
            3,
            "dim shrunk to sidecar's 3 tracks"
        );
        assert_eq!(
            title.per_file_originals.len(),
            3,
            "originals resized to match — len(values) == len(originals) invariant"
        );
        assert_eq!(title.per_file_values, vec!["T1", "T2", "T3"]);
    }

    #[test]
    fn reload_sidecar_refuses_when_no_sidecar() {
        let td = tempfile::tempdir().expect("tempdir");
        // No .cue written.
        let mut state = state_for_sidecar_test(td.path(), vec![]);
        let result = reload_from_sidecar_cue(&mut state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no .cue"));
    }

    #[test]
    fn reload_sidecar_refuses_track_by_track() {
        let track_by_track = "TITLE \"Album\"\n\
             FILE \"track01.flac\" WAVE\n  TRACK 01 AUDIO\nINDEX 01 00:00:00\n\
             FILE \"track02.flac\" WAVE\n  TRACK 02 AUDIO\nINDEX 01 00:00:00\n";
        let td = tempfile::tempdir().expect("tempdir");
        write_sidecar_at(td.path(), track_by_track);
        let mut state = state_for_sidecar_test(td.path(), vec![]);
        let result = reload_from_sidecar_cue(&mut state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("track-by-track"));
    }

    #[test]
    fn reload_sidecar_refuses_multi_file() {
        let td = tempfile::tempdir().expect("tempdir");
        write_sidecar_at(td.path(), SIDECAR_3_TRACK_SINGLE_IMAGE);
        let mut state = MetadataEditorState {
            paths: vec![td.path().join("a.flac"), td.path().join("b.flac")],
            entries: vec![],
            cursor: 0,
            scroll: 0,
            last_click: None,
            edit_input: None,
            add_key_input: None,
            phase: MetadataEditorPhase::Editing,
            dirty: false,
            deleted: vec![],
            file_labels: vec!["01".into(), "02".into()],
            detail_field_idx: 0,
            detail_cursor: 0,
            detail_scroll: 0,
            detail_edit: None,
            mb_back: None,
            gnudb_back: None,
            read_only: false,
            sacd_sidecar_path: None,
            sacd_area_kind: None,
            sacd_stereo_durations: None,
            sacd_multi_channel_durations: None,
            presentation_tabs: Vec::new(),
            active_tab: 0,
        };
        let result = reload_from_sidecar_cue(&mut state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("single-image"));
    }

    // -------- :fix-caps --------

    #[test]
    fn fix_caps_main_editor_capitalizes_mixed_per_track_preserving_placeholder() {
        // User runs :fix-caps from the main editor on a state where
        // TITLE has per-track values (mixed). Expectation: per_file_values
        // get capitalized so the detail overlay shows them already
        // fixed; the main-grid display ("<multiple values>") stays
        // intact since the values still differ post-cap.
        //
        // The dim-bug fix (recompute uses entry.per_file_values.len(),
        // not paths.len()) is what keeps is_mixed=true post-cap; with
        // that in place, mixed entries can be safely capitalized
        // without leaking one track's value into the placeholder.
        let mut state = single_image_state(vec![
            entry(
                "ALBUM",
                ItemKey::AlbumTitle,
                &["the dark side of the moon"],
                &["the dark side of the moon"],
            ),
            entry(
                "TITLE",
                ItemKey::TrackTitle,
                &["speak to me", "breathe", "on the run"],
                &["speak to me", "breathe", "on the run"],
            ),
        ]);
        let title_idx = state
            .entries
            .iter()
            .position(|e| e.display_key == "TITLE")
            .unwrap();
        state.entries[title_idx].is_mixed = true;
        state.entries[title_idx].value = "<multiple values>".to_string();

        let result = fix_caps_for_state(&mut state, None);

        let album = state
            .entries
            .iter()
            .find(|e| e.display_key == "ALBUM")
            .unwrap();
        assert_eq!(album.per_file_values, vec!["The Dark Side of the Moon"]);

        // TITLE per_file_values capitalized (visible in detail overlay).
        let title = state
            .entries
            .iter()
            .find(|e| e.display_key == "TITLE")
            .unwrap();
        assert_eq!(
            title.per_file_values,
            vec!["Speak to Me", "Breathe", "On the Run"],
            "main-editor fix-caps capitalizes per-track values too"
        );
        // Display stays "<multiple values>" since values still differ.
        assert!(
            title.is_mixed,
            "is_mixed preserved (capitalization didn't merge)"
        );
        assert_eq!(
            title.value, "<multiple values>",
            "main-grid display unchanged"
        );
        assert!(
            result.changed_values >= 4,
            "ALBUM + 3 TITLE values capitalized"
        );
    }

    #[test]
    fn fix_caps_main_editor_skips_deleted_entries() {
        let mut state = single_image_state(vec![
            entry("ALBUM", ItemKey::AlbumTitle, &["dark side"], &["dark side"]),
            entry("TITLE", ItemKey::TrackTitle, &["money"], &["money"]),
        ]);
        // Mark ALBUM (idx 0) deleted.
        state.deleted.push(0);
        let result = fix_caps_for_state(&mut state, None);
        let album = state
            .entries
            .iter()
            .find(|e| e.display_key == "ALBUM")
            .unwrap();
        assert_eq!(
            album.per_file_values,
            vec!["dark side"],
            "deleted entries must be skipped — no point capitalizing a row about to be removed"
        );
        assert_eq!(result.skipped_deleted, 1);
        // TITLE still got capitalized.
        let title = state
            .entries
            .iter()
            .find(|e| e.display_key == "TITLE")
            .unwrap();
        assert_eq!(title.per_file_values, vec!["Money"]);
    }

    #[test]
    fn fix_caps_detail_overlay_targets_focused_only() {
        let mut state = single_image_state(vec![
            entry("ALBUM", ItemKey::AlbumTitle, &["dark side"], &["dark side"]),
            entry(
                "TITLE",
                ItemKey::TrackTitle,
                &["speak to me", "breathe", "on the run"],
                &["speak to me", "breathe", "on the run"],
            ),
        ]);
        let title_idx = state
            .entries
            .iter()
            .position(|e| e.display_key == "TITLE")
            .unwrap();
        state.entries[title_idx].is_mixed = true;

        // Detail overlay focused on TITLE.
        let result = fix_caps_for_state(&mut state, Some(title_idx));

        // TITLE's per-track values capitalized.
        let title = state
            .entries
            .iter()
            .find(|e| e.display_key == "TITLE")
            .unwrap();
        assert_eq!(
            title.per_file_values,
            vec!["Speak to Me", "Breathe", "On the Run"]
        );
        // ALBUM untouched.
        let album = state
            .entries
            .iter()
            .find(|e| e.display_key == "ALBUM")
            .unwrap();
        assert_eq!(album.per_file_values, vec!["dark side"]);
        assert_eq!(result.skipped_deleted, 0);
    }

    #[test]
    fn fix_caps_recomputes_is_mixed_using_entry_dim() {
        // Per-track TITLE entry with all-same lowercase values on a
        // single-image rip (paths.len()=1, dim=3). After fix-caps:
        // - per_file_values capitalized (still all-same)
        // - is_mixed must STAY false (matches the dim==3, all_same=true case)
        // - value must be the capitalized title, NOT "<multiple values>"
        // The pre-fix code at command.rs:2168 used `n = paths.len() = 1`
        // and would set is_mixed = !all_same && n > 1 = false; that
        // happened to be correct here only because all_same was true.
        // The dim-bug would have surfaced if values diverged but
        // paths.len() == 1.
        let mut state = single_image_state(vec![entry(
            "TITLE",
            ItemKey::TrackTitle,
            &["foo", "foo", "foo"],
            &["foo", "foo", "foo"],
        )]);
        let _ = fix_caps_for_state(&mut state, None);
        let title = state
            .entries
            .iter()
            .find(|e| e.display_key == "TITLE")
            .unwrap();
        assert_eq!(title.per_file_values, vec!["Foo", "Foo", "Foo"]);
        assert_eq!(title.is_mixed, false);
        assert_eq!(title.value, "Foo");
    }

    #[test]
    fn inject_sidecar_cuesheet_skips_when_no_sidecar() {
        let td = tempfile::tempdir().expect("tempdir");
        // No .cue written.
        let audio = td.path().join("album.flac");
        let mut entries: Vec<TagEntry> = vec![];
        inject_sidecar_cuesheet_if_present(&mut entries, &audio);
        assert!(entries
            .iter()
            .find(|e| e.display_key == "CUESHEET")
            .is_none());
    }

    // ---------- C5 tests: SACD editor surfacing ----------

    /// Build a minimal SacdMetadata with the supplied album+track text
    /// and one stereo area. Used to test `build_sacd_editor_state`
    /// without re-running the full parser.
    fn synth_sacd_metadata(
        album_title: Option<&str>,
        album_artist: Option<&str>,
        disc_year: u16,
        catalog: Option<&str>,
        track_titles: &[&str],
        track_performers: &[&str],
        track_isrcs: &[Option<&str>],
    ) -> crate::tui::sacd::SacdMetadata {
        use crate::tui::sacd::*;

        let master_toc = MasterToc {
            spec_version: (1, 20),
            album_set_size: 1,
            album_sequence_number: 1,
            album_catalog_number: catalog.unwrap_or("").to_string(),
            album_genres: vec![],
            two_channel: AreaPointer {
                toc_1_start: 540,
                toc_2_start: 541,
                toc_size_sectors: 3,
            },
            multi_channel: AreaPointer {
                toc_1_start: 0,
                toc_2_start: 0,
                toc_size_sectors: 0,
            },
            disc_type_hybrid: false,
            disc_catalog_number: String::new(),
            disc_genres: vec![Genre {
                category: 1,
                genre: 14, /* JAZZ */
            }],
            disc_date: if disc_year > 0 {
                Some(DiscDate {
                    year: disc_year,
                    month: 0,
                    day: 0,
                })
            } else {
                None
            },
            text_area_count: 1,
            locales: vec![],
        };

        let master_text = if album_title.is_some() || album_artist.is_some() {
            Some(SacdText {
                album_title: album_title.map(|s| s.to_string()),
                album_artist: album_artist.map(|s| s.to_string()),
                charset: 2,
                ..Default::default()
            })
        } else {
            None
        };

        let n = track_titles.len();
        let mut tracks: Vec<TrackEntry> = Vec::with_capacity(n);
        for i in 0..n {
            tracks.push(TrackEntry {
                start_lsn: 600 + i as u32 * 100,
                length_lsn: 100,
                start_time: PlayTime::default(),
                duration: PlayTime {
                    minutes: 3,
                    seconds: 0,
                    frames: 0,
                },
                text: TrackText {
                    title: if track_titles[i].is_empty() {
                        None
                    } else {
                        Some(track_titles[i].to_string())
                    },
                    performer: track_performers
                        .get(i)
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string()),
                    ..Default::default()
                },
                isrc: track_isrcs.get(i).and_then(|o| o.map(|s| s.to_string())),
                structured_isrc: None,
                genre: None,
            });
        }

        let stereo_header = AreaTocHeader {
            kind: AreaKind::Stereo,
            spec_version: (1, 20),
            size_sectors: 3,
            max_byte_rate: 64_000,
            sample_frequency: 0x04,
            frame_format: FrameFormat::Dsd3In14,
            channel_count: 2,
            loudspeaker_config: 0,
            extra_settings: 0,
            max_available_channels: 2,
            area_mute_flags: 0,
            total_playtime: PlayTime {
                minutes: 30,
                seconds: 0,
                frames: 0,
            },
            track_offset: 0,
            track_count: n as u8,
            track_start_lsn: 600,
            track_end_lsn: 10_000,
            text_area_count: 1,
            locales: vec![Locale {
                language_code: [b'e', b'n'],
                character_set: 2,
            }],
            description: None,
            description_phonetic: None,
            copyright: None,
            copyright_phonetic: None,
        };

        SacdMetadata {
            master_toc,
            master_text,
            stereo: Some(AreaInfo {
                header: stereo_header,
                tracks,
                consistency: Default::default(),
            }),
            multi_channel: None,
            consistency: Default::default(),
        }
    }

    #[test]
    fn build_sacd_editor_state_album_level_fields_extracted() {
        let md = synth_sacd_metadata(
            Some("Kind of Blue"),
            Some("Miles Davis"),
            1959,
            Some("PROC-001"),
            &["So What", "Freddie Freeloader"],
            &["Miles Davis Sextet", "Miles Davis Sextet"],
            &[None, None],
        );
        let path = std::path::PathBuf::from("/tmp/test.iso");
        let (state, label, n) = build_sacd_editor_state(&path, &md, None).expect("build");
        assert_eq!(label, "stereo");
        assert_eq!(n, 2);
        // Phase A: with no sidecar present but a writable parent dir,
        // the editor unlocks for the mint-on-save path. `/tmp` is
        // writable in the test environment, so `read_only` flips off
        // and `sacd_sidecar_path` points at the expected target file.
        assert!(!state.read_only);
        assert_eq!(
            state.sacd_sidecar_path.as_deref(),
            Some(std::path::Path::new("/tmp/test.xml")),
        );
        assert_eq!(state.paths.len(), 2);
        assert!(state.paths.iter().all(|p| p == &path));

        let by_key = |k: &str| state.entries.iter().find(|e| e.display_key == k);
        assert_eq!(
            by_key("ALBUM").map(|e| e.value.as_str()),
            Some("Kind of Blue")
        );
        assert_eq!(
            by_key("ALBUMARTIST").map(|e| e.value.as_str()),
            Some("Miles Davis")
        );
        assert_eq!(by_key("DATE").map(|e| e.value.as_str()), Some("1959"));
        assert_eq!(
            by_key("CATALOGNUMBER").map(|e| e.value.as_str()),
            Some("PROC-001")
        );
        assert_eq!(by_key("GENRE").map(|e| e.value.as_str()), Some("Jazz"));
        // Album-level entries share one value across all tracks.
        for k in &["ALBUM", "ALBUMARTIST", "DATE", "CATALOGNUMBER", "GENRE"] {
            let e = by_key(k).expect(k);
            assert_eq!(e.per_file_values.len(), 2);
            assert!(!e.is_mixed);
        }
    }

    #[test]
    fn build_sacd_editor_state_per_track_titles_and_artists() {
        let md = synth_sacd_metadata(
            Some("Album"),
            None,
            0,
            None,
            &["Track A", "Track B", "Track C"],
            &["Artist X", "Artist X", "Artist Y"],
            &[Some("USAA10800001"), Some("USAA10800002"), None],
        );
        let path = std::path::PathBuf::from("/tmp/synthetic.iso");
        let (state, _, _) = build_sacd_editor_state(&path, &md, None).expect("build");

        let title = state
            .entries
            .iter()
            .find(|e| e.display_key == "TITLE")
            .expect("TITLE");
        assert_eq!(title.per_file_values, vec!["Track A", "Track B", "Track C"]);
        assert!(title.is_mixed);

        let artist = state
            .entries
            .iter()
            .find(|e| e.display_key == "ARTIST")
            .expect("ARTIST");
        assert_eq!(
            artist.per_file_values,
            vec!["Artist X", "Artist X", "Artist Y"]
        );
        assert!(artist.is_mixed);

        let isrc = state
            .entries
            .iter()
            .find(|e| e.display_key == "ISRC")
            .expect("ISRC");
        assert_eq!(
            isrc.per_file_values,
            vec!["USAA10800001", "USAA10800002", ""]
        );
    }

    #[test]
    fn build_sacd_editor_state_skips_per_track_field_when_all_empty() {
        // No performers anywhere → ARTIST entry should not be emitted.
        let md = synth_sacd_metadata(
            Some("Album"),
            None,
            0,
            None,
            &["T1", "T2"],
            &["", ""],
            &[None, None],
        );
        let path = std::path::PathBuf::from("/tmp/x.iso");
        let (state, _, _) = build_sacd_editor_state(&path, &md, None).expect("build");
        assert!(state.entries.iter().all(|e| e.display_key != "ARTIST"));
        assert!(state.entries.iter().all(|e| e.display_key != "ISRC"));
    }

    #[test]
    fn build_sacd_editor_state_includes_tracknumber_always() {
        let md = synth_sacd_metadata(
            None,
            None,
            0,
            None,
            &["t1", "t2", "t3"],
            &["", "", ""],
            &[None, None, None],
        );
        let path = std::path::PathBuf::from("/tmp/x.iso");
        let (state, _, _) = build_sacd_editor_state(&path, &md, None).expect("build");
        let tn = state
            .entries
            .iter()
            .find(|e| e.display_key == "TRACKNUMBER")
            .expect("TRACKNUMBER");
        assert_eq!(tn.per_file_values, vec!["1", "2", "3"]);
        assert!(tn.is_mixed);
    }

    #[test]
    fn build_sacd_editor_state_rejects_zero_track_area() {
        let md = synth_sacd_metadata(None, None, 0, None, &[], &[], &[]);
        let path = std::path::PathBuf::from("/tmp/x.iso");
        let res = build_sacd_editor_state(&path, &md, None);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("zero tracks"));
    }

    #[test]
    fn build_sacd_editor_state_rejects_no_areas() {
        // No stereo, no multi_channel.
        let mut md = synth_sacd_metadata(None, None, 0, None, &["x"], &[""], &[None]);
        md.stereo = None;
        let path = std::path::PathBuf::from("/tmp/x.iso");
        let res = build_sacd_editor_state(&path, &md, None);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("no readable area"));
    }

    #[test]
    fn build_sacd_editor_state_falls_back_to_multi_channel_when_no_stereo() {
        use crate::tui::sacd::*;
        let mut md = synth_sacd_metadata(Some("MC Album"), None, 0, None, &["t1"], &[""], &[None]);
        // Move the stereo area into multi_channel slot and clear stereo.
        let mut info = md.stereo.take().unwrap();
        info.header.kind = AreaKind::MultiChannel;
        info.header.channel_count = 6;
        md.multi_channel = Some(info);

        let path = std::path::PathBuf::from("/tmp/x.iso");
        let (_, label, _) = build_sacd_editor_state(&path, &md, None).expect("build");
        assert_eq!(label, "MCH");
    }

    // ---------- C5b tests: sidecar merge ----------

    fn parse_sidecar_for_test(xml: &str) -> crate::tui::sacd_sidecar::SidecarMetadata {
        crate::tui::sacd_sidecar::parse_sidecar_str(xml).expect("parse_sidecar_str")
    }

    #[test]
    fn sidecar_overrides_scarletbook_album_title() {
        let md = synth_sacd_metadata(
            Some("ScarletBook Title"),
            Some("ScarletBook Artist"),
            1959,
            None,
            &["sb-t1", "sb-t2"],
            &["sb-perf-1", "sb-perf-2"],
            &[None, None],
        );
        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="ALBUM" value="Sidecar Album"/><meta name="ARTIST" value="Sidecar Artist"/><meta name="TITLE" value="Sidecar T1"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="2"/></track>
<track id="2"><meta name="ALBUM" value="Sidecar Album"/><meta name="ARTIST" value="Sidecar Artist"/><meta name="TITLE" value="Sidecar T2"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="2"/></track>
</store></root>"#;
        let sidecar = parse_sidecar_for_test(xml);
        let path = std::path::PathBuf::from("/tmp/x.iso");
        let (state, _, _) = build_sacd_editor_state(&path, &md, Some(&sidecar)).expect("build");

        let by_key = |k: &str| state.entries.iter().find(|e| e.display_key == k);
        assert_eq!(
            by_key("ALBUM").map(|e| e.value.as_str()),
            Some("Sidecar Album")
        );
        let title = by_key("TITLE").expect("TITLE");
        assert_eq!(title.per_file_values, vec!["Sidecar T1", "Sidecar T2"]);
        let artist = by_key("ARTIST").expect("ARTIST");
        assert_eq!(
            artist.per_file_values,
            vec!["Sidecar Artist", "Sidecar Artist"]
        );
    }

    #[test]
    fn sidecar_empty_fields_fall_back_to_scarletbook() {
        let md = synth_sacd_metadata(
            Some("SB Album"),
            None,
            0,
            None,
            &["sb-track-1"],
            &["sb-performer-1"],
            &[Some("USAA10800001")],
        );
        // Sidecar provides ALBUM (wins) but no TITLE / ARTIST / ISRC.
        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="ALBUM" value="Sidecar Album"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="1"/></track>
</store></root>"#;
        let sidecar = parse_sidecar_for_test(xml);
        let path = std::path::PathBuf::from("/tmp/x.iso");
        let (state, _, _) = build_sacd_editor_state(&path, &md, Some(&sidecar)).expect("build");

        let by_key = |k: &str| state.entries.iter().find(|e| e.display_key == k);
        // ALBUM from sidecar
        assert_eq!(
            by_key("ALBUM").map(|e| e.value.as_str()),
            Some("Sidecar Album")
        );
        // TITLE/ARTIST/ISRC from ScarletBook fallback
        assert_eq!(
            by_key("TITLE").map(|e| e.value.as_str()),
            Some("sb-track-1")
        );
        assert_eq!(
            by_key("ARTIST").map(|e| e.value.as_str()),
            Some("sb-performer-1")
        );
        assert_eq!(
            by_key("ISRC").map(|e| e.value.as_str()),
            Some("USAA10800001")
        );
    }

    #[test]
    fn sidecar_publisher_surfaces_even_without_scarletbook() {
        // PUBLISHER has no ScarletBook home; only sidecar can provide it.
        let md = synth_sacd_metadata(None, None, 0, None, &["t"], &[""], &[None]);
        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="PUBLISHER" value="Sony Music Japan International Inc."/><meta name="TITLE" value="t"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="1"/></track>
</store></root>"#;
        let sidecar = parse_sidecar_for_test(xml);
        let path = std::path::PathBuf::from("/tmp/x.iso");
        let (state, _, _) = build_sacd_editor_state(&path, &md, Some(&sidecar)).expect("build");
        let pub_entry = state.entries.iter().find(|e| e.display_key == "PUBLISHER");
        assert_eq!(
            pub_entry.map(|e| e.value.as_str()),
            Some("Sony Music Japan International Inc.")
        );
    }

    #[test]
    fn sidecar_alt_keys_album_artist_and_discogs_catalog() {
        // Some sidecars use "ALBUM ARTIST" (with space) and DISCOGS_CATALOG.
        let md = synth_sacd_metadata(None, None, 0, None, &["t"], &[""], &[None]);
        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="ALBUM ARTIST" value="Composite Artist"/><meta name="DISCOGS_CATALOG" value="SICP 10083"/><meta name="TITLE" value="t"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="1"/></track>
</store></root>"#;
        let sidecar = parse_sidecar_for_test(xml);
        let path = std::path::PathBuf::from("/tmp/x.iso");
        let (state, _, _) = build_sacd_editor_state(&path, &md, Some(&sidecar)).expect("build");
        let by_key = |k: &str| state.entries.iter().find(|e| e.display_key == k);
        assert_eq!(
            by_key("ALBUMARTIST").map(|e| e.value.as_str()),
            Some("Composite Artist")
        );
        assert_eq!(
            by_key("CATALOGNUMBER").map(|e| e.value.as_str()),
            Some("SICP 10083")
        );
    }

    #[test]
    fn sidecar_album_value_skips_first_empty_picks_later_nonempty() {
        // Regression for the find_map-before-filter bug: a sidecar
        // where the first track lost its ALBUM tag (empty string)
        // but a later track still has it must NOT lock in the empty
        // value. The fix moves the non-empty filter inside find_map.
        let md = synth_sacd_metadata(
            Some("SB Album"), // ScarletBook fallback if sidecar misses everything
            None,
            0,
            None,
            &["t1", "t2", "t3"],
            &["", "", ""],
            &[None, None, None],
        );
        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="ALBUM" value=""/><meta name="TITLE" value="t1"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="3"/></track>
<track id="2"><meta name="ALBUM" value="The Real Album"/><meta name="TITLE" value="t2"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="3"/></track>
<track id="3"><meta name="ALBUM" value="The Real Album"/><meta name="TITLE" value="t3"/><meta name="TRACKNUMBER" value="03"/><meta name="TOTALTRACKS" value="3"/></track>
</store></root>"#;
        let sidecar = parse_sidecar_for_test(xml);
        let path = std::path::PathBuf::from("/tmp/x.iso");
        let (state, _, _) = build_sacd_editor_state(&path, &md, Some(&sidecar)).expect("build");
        let album = state
            .entries
            .iter()
            .find(|e| e.display_key == "ALBUM")
            .expect("ALBUM");
        // Should pick the non-empty later value, not lock onto track 1's empty.
        assert_eq!(album.value, "The Real Album");
    }

    #[test]
    fn publisher_falls_back_to_scarletbook_master_text() {
        // Sidecar absent or silent on PUBLISHER; ScarletBook
        // SACDText.album_publisher should be surfaced.
        use crate::tui::sacd::*;
        let mut md = synth_sacd_metadata(Some("Album"), None, 0, None, &["t"], &[""], &[None]);
        md.master_text = Some(SacdText {
            album_title: Some("Album".into()),
            album_publisher: Some("ScarletBook Publisher".into()),
            charset: 2,
            ..Default::default()
        });
        let path = std::path::PathBuf::from("/tmp/x.iso");
        let (state, _, _) = build_sacd_editor_state(&path, &md, None).expect("build");
        let pub_entry = state.entries.iter().find(|e| e.display_key == "PUBLISHER");
        assert_eq!(
            pub_entry.map(|e| e.value.as_str()),
            Some("ScarletBook Publisher")
        );
    }

    #[test]
    fn publisher_prefers_sidecar_over_scarletbook() {
        // Both have PUBLISHER — sidecar wins.
        use crate::tui::sacd::*;
        let mut md = synth_sacd_metadata(Some("Album"), None, 0, None, &["t"], &[""], &[None]);
        md.master_text = Some(SacdText {
            album_title: Some("Album".into()),
            album_publisher: Some("ScarletBook Publisher".into()),
            charset: 2,
            ..Default::default()
        });
        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="PUBLISHER" value="Sidecar Publisher"/><meta name="TITLE" value="t"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="1"/></track>
</store></root>"#;
        let sidecar = parse_sidecar_for_test(xml);
        let path = std::path::PathBuf::from("/tmp/x.iso");
        let (state, _, _) = build_sacd_editor_state(&path, &md, Some(&sidecar)).expect("build");
        let pub_entry = state.entries.iter().find(|e| e.display_key == "PUBLISHER");
        assert_eq!(
            pub_entry.map(|e| e.value.as_str()),
            Some("Sidecar Publisher")
        );
    }

    /// Helper for the save tests: write a sidecar XML to a tempfile
    /// and return its path. Build a MetadataEditorState seeded from
    /// a synthetic ScarletBook + that sidecar, then call save and
    /// re-parse the written file to make assertions.
    fn round_trip_save(
        sidecar_xml: &str,
        mutate: impl FnOnce(&mut super::super::app::MetadataEditorState),
    ) -> (tempfile::TempDir, crate::tui::sacd_sidecar::SidecarMetadata) {
        use crate::tui::sacd_sidecar::*;
        let td = tempfile::tempdir().expect("tempdir");
        let xml_path = td.path().join("disc.xml");
        std::fs::write(&xml_path, sidecar_xml).unwrap();

        // ScarletBook with stereo area + 3 tracks (the XML fixtures
        // below use TOTALTRACKS=3).
        let md = synth_sacd_metadata(
            Some("SB Album"),
            None,
            0,
            None,
            &["sb1", "sb2", "sb3"],
            &["", "", ""],
            &[None, None, None],
        );
        let sidecar = parse_sidecar(&xml_path).expect("parse");

        let iso_path = td.path().join("disc.iso");
        std::fs::write(&iso_path, b"\0").unwrap();
        let (mut state, _, _) =
            build_sacd_editor_state(&iso_path, &md, Some(&sidecar)).expect("build");
        // Force writability so the save path is reachable in tests.
        state.read_only = false;
        state.sacd_sidecar_path = Some(xml_path.clone());
        state.sacd_area_kind = Some(crate::tui::sacd::AreaKind::Stereo);

        mutate(&mut state);

        save_sacd_sidecar(&state, &xml_path).expect("save");
        let reparsed = parse_sidecar(&xml_path).expect("re-parse");
        (td, reparsed)
    }

    #[test]
    fn build_sacd_editor_state_surfaces_musicbrainz_per_track_ids() {
        // Phase C-1: per-track MUSICBRAINZ_* entries from sidecar
        // surface in the editor; ScarletBook fallback is empty so
        // entries only appear when the sidecar carries them.
        let md = synth_sacd_metadata(
            Some("Album"),
            None,
            0,
            None,
            &["t1", "t2"],
            &["", ""],
            &[None, None],
        );
        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="TITLE" value="t1"/><meta name="MUSICBRAINZ_TRACKID" value="rec-1"/><meta name="MUSICBRAINZ_RELEASETRACKID" value="trk-1"/><meta name="MUSICBRAINZ_ARTISTID" value="art-1"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="2"/></track>
<track id="2"><meta name="TITLE" value="t2"/><meta name="MUSICBRAINZ_TRACKID" value="rec-2"/><meta name="MUSICBRAINZ_RELEASETRACKID" value="trk-2"/><meta name="MUSICBRAINZ_ARTISTID" value="art-2"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="2"/></track>
</store></root>"#;
        let sidecar = parse_sidecar_for_test(xml);
        let path = std::path::PathBuf::from("/tmp/x.iso");
        let (state, _, _) = build_sacd_editor_state(&path, &md, Some(&sidecar)).expect("build");
        let by_key = |k: &str| state.entries.iter().find(|e| e.display_key == k);
        assert_eq!(
            by_key("MUSICBRAINZ_TRACKID").map(|e| e.per_file_values.clone()),
            Some(vec!["rec-1".to_string(), "rec-2".to_string()]),
        );
        assert_eq!(
            by_key("MUSICBRAINZ_RELEASETRACKID").map(|e| e.per_file_values.clone()),
            Some(vec!["trk-1".to_string(), "trk-2".to_string()]),
        );
        assert_eq!(
            by_key("MUSICBRAINZ_ARTISTID").map(|e| e.per_file_values.clone()),
            Some(vec!["art-1".to_string(), "art-2".to_string()]),
        );
    }

    #[test]
    fn build_sacd_editor_state_surfaces_musicbrainz_album_level() {
        // Phase C-1: album-level MUSICBRAINZ_* + ORIGINALDATE +
        // RELEASECOUNTRY surface as single rows replicated across
        // tracks. Sidecar carries the value on track 1 only — the
        // album-level reader picks the first non-empty.
        let md = synth_sacd_metadata(
            Some("Album"),
            None,
            0,
            None,
            &["t1", "t2"],
            &["", ""],
            &[None, None],
        );
        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="TITLE" value="t1"/><meta name="MUSICBRAINZ_ALBUMID" value="alb-1"/><meta name="MUSICBRAINZ_ALBUMARTISTID" value="aart-1"/><meta name="MUSICBRAINZ_RELEASEGROUPID" value="rg-1"/><meta name="ORIGINALDATE" value="1959"/><meta name="RELEASECOUNTRY" value="US"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="2"/></track>
<track id="2"><meta name="TITLE" value="t2"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="2"/></track>
</store></root>"#;
        let sidecar = parse_sidecar_for_test(xml);
        let path = std::path::PathBuf::from("/tmp/x.iso");
        let (state, _, _) = build_sacd_editor_state(&path, &md, Some(&sidecar)).expect("build");
        let by_key = |k: &str| state.entries.iter().find(|e| e.display_key == k);
        assert_eq!(
            by_key("MUSICBRAINZ_ALBUMID").map(|e| e.value.as_str()),
            Some("alb-1")
        );
        assert_eq!(
            by_key("MUSICBRAINZ_ALBUMARTISTID").map(|e| e.value.as_str()),
            Some("aart-1")
        );
        assert_eq!(
            by_key("MUSICBRAINZ_RELEASEGROUPID").map(|e| e.value.as_str()),
            Some("rg-1")
        );
        assert_eq!(
            by_key("ORIGINALDATE").map(|e| e.value.as_str()),
            Some("1959")
        );
        assert_eq!(
            by_key("RELEASECOUNTRY").map(|e| e.value.as_str()),
            Some("US")
        );
    }

    #[test]
    fn build_sacd_editor_state_skips_musicbrainz_when_absent() {
        // No MUSICBRAINZ_* in sidecar (the typical foobar2000-untouched
        // case) → no rows surface for them. Critical because empty
        // rows would clutter the editor for the 99% of SACDs not yet
        // tagged via MB.
        let md = synth_sacd_metadata(Some("Album"), None, 0, None, &["t1"], &[""], &[None]);
        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="TITLE" value="t1"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="1"/></track>
</store></root>"#;
        let sidecar = parse_sidecar_for_test(xml);
        let path = std::path::PathBuf::from("/tmp/x.iso");
        let (state, _, _) = build_sacd_editor_state(&path, &md, Some(&sidecar)).expect("build");
        for k in [
            "MUSICBRAINZ_TRACKID",
            "MUSICBRAINZ_RELEASETRACKID",
            "MUSICBRAINZ_ARTISTID",
            "MUSICBRAINZ_ALBUMID",
            "MUSICBRAINZ_ALBUMARTISTID",
            "MUSICBRAINZ_RELEASEGROUPID",
            "ORIGINALDATE",
            "RELEASECOUNTRY",
        ] {
            assert!(
                state.entries.iter().all(|e| e.display_key != k),
                "{} should not surface when sidecar carries no value",
                k,
            );
        }
    }

    #[test]
    fn save_sacd_sidecar_round_trips_musicbrainz_keys() {
        // Phase C-1 acceptance: edit MB keys, save, re-parse — every
        // MB id round-trips byte-for-byte.
        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="TITLE" value="t1"/><meta name="MUSICBRAINZ_TRACKID" value="rec-1"/><meta name="MUSICBRAINZ_RELEASETRACKID" value="trk-1"/><meta name="MUSICBRAINZ_ARTISTID" value="art-1"/><meta name="MUSICBRAINZ_ALBUMID" value="alb-old"/><meta name="MUSICBRAINZ_ALBUMARTISTID" value="aart-old"/><meta name="MUSICBRAINZ_RELEASEGROUPID" value="rg-old"/><meta name="ORIGINALDATE" value="1959"/><meta name="RELEASECOUNTRY" value="US"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="3"/></track>
<track id="2"><meta name="TITLE" value="t2"/><meta name="MUSICBRAINZ_TRACKID" value="rec-2"/><meta name="MUSICBRAINZ_RELEASETRACKID" value="trk-2"/><meta name="MUSICBRAINZ_ARTISTID" value="art-2"/><meta name="MUSICBRAINZ_ALBUMID" value="alb-old"/><meta name="MUSICBRAINZ_ALBUMARTISTID" value="aart-old"/><meta name="MUSICBRAINZ_RELEASEGROUPID" value="rg-old"/><meta name="ORIGINALDATE" value="1959"/><meta name="RELEASECOUNTRY" value="US"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="3"/></track>
<track id="3"><meta name="TITLE" value="t3"/><meta name="MUSICBRAINZ_TRACKID" value="rec-3"/><meta name="MUSICBRAINZ_RELEASETRACKID" value="trk-3"/><meta name="MUSICBRAINZ_ARTISTID" value="art-3"/><meta name="MUSICBRAINZ_ALBUMID" value="alb-old"/><meta name="MUSICBRAINZ_ALBUMARTISTID" value="aart-old"/><meta name="MUSICBRAINZ_RELEASEGROUPID" value="rg-old"/><meta name="ORIGINALDATE" value="1959"/><meta name="RELEASECOUNTRY" value="US"/><meta name="TRACKNUMBER" value="03"/><meta name="TOTALTRACKS" value="3"/></track>
</store></root>"#;
        let (_td, reparsed) = round_trip_save(xml, |state| {
            // Update the album-level MB id (simulating an `:mb-back`
            // re-pick) and per-track MB recording ids.
            for entry in state.entries.iter_mut() {
                match entry.display_key.as_str() {
                    "MUSICBRAINZ_ALBUMID" => {
                        entry.per_file_values = vec!["alb-new".into(); 3];
                        entry.value = "alb-new".into();
                    }
                    "MUSICBRAINZ_TRACKID" => {
                        entry.per_file_values =
                            vec!["rec-new-1".into(), "rec-new-2".into(), "rec-new-3".into()];
                        entry.value = "<multiple values>".into();
                        entry.is_mixed = true;
                    }
                    _ => {}
                }
            }
        });

        for tid in 1..=3 {
            let t = reparsed.tracks.iter().find(|t| t.id == tid).expect("track");
            assert_eq!(
                t.meta.get("MUSICBRAINZ_ALBUMID").map(String::as_str),
                Some("alb-new"),
                "tid={} album-level MB id replicated",
                tid
            );
            assert_eq!(
                t.meta.get("MUSICBRAINZ_ARTISTID").map(String::as_str),
                Some(&*format!("art-{}", tid)),
                "tid={} per-track MUSICBRAINZ_ARTISTID preserved",
                tid
            );
            assert_eq!(
                t.meta.get("MUSICBRAINZ_RELEASETRACKID").map(String::as_str),
                Some(&*format!("trk-{}", tid)),
                "tid={} MUSICBRAINZ_RELEASETRACKID preserved",
                tid
            );
            assert_eq!(
                t.meta.get("MUSICBRAINZ_ALBUMARTISTID").map(String::as_str),
                Some("aart-old"),
                "tid={} album-level MUSICBRAINZ_ALBUMARTISTID preserved",
                tid
            );
            assert_eq!(
                t.meta.get("MUSICBRAINZ_RELEASEGROUPID").map(String::as_str),
                Some("rg-old"),
                "tid={} album-level MUSICBRAINZ_RELEASEGROUPID preserved",
                tid
            );
            assert_eq!(
                t.meta.get("ORIGINALDATE").map(String::as_str),
                Some("1959"),
                "tid={} ORIGINALDATE preserved",
                tid
            );
            assert_eq!(
                t.meta.get("RELEASECOUNTRY").map(String::as_str),
                Some("US"),
                "tid={} RELEASECOUNTRY preserved",
                tid
            );
        }
        // Per-track MUSICBRAINZ_TRACKID also got the new values.
        let by_tid_trackid = |tid: u32| {
            reparsed
                .tracks
                .iter()
                .find(|t| t.id == tid)
                .and_then(|t| t.meta.get("MUSICBRAINZ_TRACKID"))
                .map(String::as_str)
        };
        assert_eq!(by_tid_trackid(1), Some("rec-new-1"));
        assert_eq!(by_tid_trackid(2), Some("rec-new-2"));
        assert_eq!(by_tid_trackid(3), Some("rec-new-3"));
    }

    #[test]
    fn save_sacd_sidecar_preserves_foreign_meta() {
        // Sidecar has DISCOGS_RELEASE_ID + DYNAMIC RANGE that
        // tonepoet doesn't surface. Edit ARTIST and save. Foreign
        // keys must survive.
        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="TITLE" value="T1"/><meta name="ARTIST" value="Old"/><meta name="DISCOGS_RELEASE_ID" value="12345"/><meta name="DYNAMIC RANGE" value="14"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="3"/></track>
<track id="2"><meta name="TITLE" value="T2"/><meta name="ARTIST" value="Old"/><meta name="DISCOGS_RELEASE_ID" value="12345"/><meta name="DYNAMIC RANGE" value="15"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="3"/></track>
<track id="3"><meta name="TITLE" value="T3"/><meta name="ARTIST" value="Old"/><meta name="DISCOGS_RELEASE_ID" value="12345"/><meta name="DYNAMIC RANGE" value="13"/><meta name="TRACKNUMBER" value="03"/><meta name="TOTALTRACKS" value="3"/></track>
</store></root>"#;
        let (_td, reparsed) = round_trip_save(xml, |state| {
            // Change the ARTIST values to "New". ARTIST is per-track
            // in the editor (built via push_per_track), so update each.
            for entry in state.entries.iter_mut() {
                if entry.display_key == "ARTIST" {
                    entry.per_file_values = vec!["New".into(); 3];
                    entry.value = "New".into();
                    entry.is_mixed = false;
                }
            }
        });

        for tid in 1..=3 {
            let t = reparsed.tracks.iter().find(|t| t.id == tid).expect("track");
            assert_eq!(
                t.meta.get("ARTIST").map(String::as_str),
                Some("New"),
                "tid={} ARTIST",
                tid
            );
            assert_eq!(
                t.meta.get("DISCOGS_RELEASE_ID").map(String::as_str),
                Some("12345"),
                "tid={} DISCOGS_RELEASE_ID must survive",
                tid
            );
            assert!(
                t.meta.contains_key("DYNAMIC RANGE"),
                "tid={} DYNAMIC RANGE must survive",
                tid
            );
        }
    }

    #[test]
    fn save_sacd_sidecar_replicates_album_level_edits_to_all_tracks() {
        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="TITLE" value="T1"/><meta name="ALBUM" value="Old Album"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="3"/></track>
<track id="2"><meta name="TITLE" value="T2"/><meta name="ALBUM" value="Old Album"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="3"/></track>
<track id="3"><meta name="TITLE" value="T3"/><meta name="ALBUM" value="Old Album"/><meta name="TRACKNUMBER" value="03"/><meta name="TOTALTRACKS" value="3"/></track>
</store></root>"#;
        let (_td, reparsed) = round_trip_save(xml, |state| {
            for entry in state.entries.iter_mut() {
                if entry.display_key == "ALBUM" {
                    entry.per_file_values = vec!["New Album".into(); 3];
                    entry.value = "New Album".into();
                    entry.is_mixed = false;
                }
            }
        });
        for tid in 1..=3 {
            let t = reparsed.tracks.iter().find(|t| t.id == tid).expect("track");
            assert_eq!(t.meta.get("ALBUM").map(String::as_str), Some("New Album"));
        }
    }

    #[test]
    fn save_sacd_sidecar_per_track_edits_land_on_correct_track() {
        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="TITLE" value="A"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="3"/></track>
<track id="2"><meta name="TITLE" value="B"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="3"/></track>
<track id="3"><meta name="TITLE" value="C"/><meta name="TRACKNUMBER" value="03"/><meta name="TOTALTRACKS" value="3"/></track>
</store></root>"#;
        let (_td, reparsed) = round_trip_save(xml, |state| {
            for entry in state.entries.iter_mut() {
                if entry.display_key == "TITLE" {
                    entry.per_file_values = vec!["Alpha".into(), "Beta".into(), "Gamma".into()];
                    entry.value = "<multiple values>".into();
                    entry.is_mixed = true;
                }
            }
        });
        let by_tid = |tid: u32| {
            reparsed
                .tracks
                .iter()
                .find(|t| t.id == tid)
                .and_then(|t| t.meta.get("TITLE"))
                .map(|s| s.as_str())
        };
        assert_eq!(by_tid(1), Some("Alpha"));
        assert_eq!(by_tid(2), Some("Beta"));
        assert_eq!(by_tid(3), Some("Gamma"));
    }

    #[test]
    fn save_sacd_sidecar_empty_value_removes_meta_key() {
        // Clearing a tag in the editor should remove the meta entry
        // from the sidecar entirely, not leave value="" sitting there.
        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="TITLE" value="A"/><meta name="ARTIST" value="X"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="1"/></track>
</store></root>"#;
        let td = tempfile::tempdir().unwrap();
        let xml_path = td.path().join("disc.xml");
        std::fs::write(&xml_path, xml).unwrap();
        let md = synth_sacd_metadata(None, None, 0, None, &["sb1"], &["sb-perf"], &[None]);
        let sidecar = crate::tui::sacd_sidecar::parse_sidecar(&xml_path).unwrap();
        let iso_path = td.path().join("disc.iso");
        std::fs::write(&iso_path, b"\0").unwrap();
        let (mut state, _, _) = build_sacd_editor_state(&iso_path, &md, Some(&sidecar)).unwrap();
        state.read_only = false;
        state.sacd_sidecar_path = Some(xml_path.clone());
        state.sacd_area_kind = Some(crate::tui::sacd::AreaKind::Stereo);

        for entry in state.entries.iter_mut() {
            if entry.display_key == "ARTIST" {
                entry.per_file_values = vec!["".into()];
                entry.value = "".into();
                entry.is_mixed = false;
            }
        }

        save_sacd_sidecar(&state, &xml_path).expect("save");
        let reparsed = crate::tui::sacd_sidecar::parse_sidecar(&xml_path).unwrap();
        let t1 = reparsed.tracks.iter().find(|t| t.id == 1).unwrap();
        assert!(!t1.meta.contains_key("ARTIST"), "ARTIST should be removed");
        assert_eq!(
            t1.meta.get("TITLE").map(String::as_str),
            Some("A"),
            "TITLE should still be present"
        );
    }

    #[test]
    fn save_sacd_sidecar_refuses_when_track_count_mismatches() {
        // Sidecar declares 2 tracks (TOTALTRACKS=2) but editor was
        // built from a ScarletBook with 3 tracks. save_sacd_sidecar
        // must refuse rather than silently misalign.
        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="TITLE" value="A"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="2"/></track>
<track id="2"><meta name="TITLE" value="B"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="2"/></track>
</store></root>"#;
        let td = tempfile::tempdir().unwrap();
        let xml_path = td.path().join("disc.xml");
        std::fs::write(&xml_path, xml).unwrap();
        let md = synth_sacd_metadata(
            None,
            None,
            0,
            None,
            &["a", "b", "c"],
            &["", "", ""],
            &[None, None, None],
        );
        let sidecar = crate::tui::sacd_sidecar::parse_sidecar(&xml_path).unwrap();
        let iso_path = td.path().join("disc.iso");
        std::fs::write(&iso_path, b"\0").unwrap();
        let (mut state, _, _) = build_sacd_editor_state(&iso_path, &md, Some(&sidecar)).unwrap();
        state.sacd_sidecar_path = Some(xml_path.clone());
        state.sacd_area_kind = Some(crate::tui::sacd::AreaKind::Stereo);
        state.read_only = false;

        let res = save_sacd_sidecar(&state, &xml_path);
        assert!(res.is_err(), "should refuse on mismatch");
        assert!(res.unwrap_err().contains("refusing to map"));
    }

    // ---------- C6 tests: area switching ----------

    fn make_hybrid_md() -> crate::tui::sacd::SacdMetadata {
        // Synthetic hybrid with 2 tracks in stereo and 2 in MCH.
        let mut md = synth_sacd_metadata(
            Some("Hybrid Album"),
            None,
            0,
            None,
            &["StereoT1", "StereoT2"],
            &["", ""],
            &[None, None],
        );
        // Duplicate the stereo area shape as MCH with renamed tracks.
        let mut mch = md.stereo.as_ref().unwrap().clone();
        mch.header.kind = crate::tui::sacd::AreaKind::MultiChannel;
        mch.header.channel_count = 6;
        for (i, t) in mch.tracks.iter_mut().enumerate() {
            t.text.title = Some(format!("MCHT{}", i + 1));
        }
        md.multi_channel = Some(mch);
        md
    }

    /// Simulate the :area target plumbing by calling
    /// switch_sacd_editor_area against an editor built from
    /// hybrid synthetic metadata.
    fn switch_helper(
        target: super::super::command::SacdAreaTarget,
    ) -> (
        super::super::app::MetadataEditorState,
        Result<&'static str, String>,
    ) {
        let md = make_hybrid_md();
        let td = tempfile::tempdir().expect("tempdir");
        let iso_path = td.path().join("hybrid.iso");
        // Write a minimal real SACD ISO so parse_sacd_iso (called by
        // switch_sacd_editor_area) returns the same hybrid layout.
        write_hybrid_iso_fixture(&iso_path, &md);

        let (mut state, _, _) = build_sacd_editor_state(&iso_path, &md, None).expect("build");
        // The fixture mirrors the synthetic md, so the editor lands
        // on the stereo area by default. Verify before switching.
        assert_eq!(
            state.sacd_area_kind,
            Some(crate::tui::sacd::AreaKind::Stereo)
        );

        let res = switch_sacd_editor_area(&mut state, &iso_path, target);
        (state, res)
    }

    /// Write a synthetic SACD ISO at `path` that, when re-parsed by
    /// parse_sacd_iso, produces a hybrid disc with 2 tracks each in
    /// stereo and MCH areas. The fixture is the minimum needed for
    /// switch_sacd_editor_area's parse step to succeed and yield the
    /// area-kind asserted in tests.
    fn write_hybrid_iso_fixture(path: &std::path::Path, _md: &crate::tui::sacd::SacdMetadata) {
        use crate::tui::sacd::*;
        use std::io::{Seek, SeekFrom, Write};

        let total_sectors = 700u64;
        let f = std::fs::File::create(path).unwrap();
        f.set_len(total_sectors * SECTOR_SIZE).unwrap();
        drop(f);
        let mut f = std::fs::File::options().write(true).open(path).unwrap();

        // Master TOC with BOTH areas declared.
        let mut mtoc = vec![0u8; 0xa8];
        mtoc[0..8].copy_from_slice(MASTER_TOC_MAGIC);
        mtoc[0x08] = 1;
        mtoc[0x09] = 20;
        mtoc[0x10..0x12].copy_from_slice(&1u16.to_be_bytes());
        mtoc[0x12..0x14].copy_from_slice(&1u16.to_be_bytes());
        mtoc[0x40..0x44].copy_from_slice(&540u32.to_be_bytes()); // 2ch toc_1
        mtoc[0x54..0x56].copy_from_slice(&3u16.to_be_bytes()); // 2ch size
        mtoc[0x48..0x4c].copy_from_slice(&600u32.to_be_bytes()); // MC toc_1
        mtoc[0x56..0x58].copy_from_slice(&3u16.to_be_bytes()); // MC size
        f.seek(SeekFrom::Start(510 * SECTOR_SIZE)).unwrap();
        f.write_all(&mtoc).unwrap();

        let build_area = |magic: &[u8; 8], channels: u8, n_tracks: u8| -> Vec<u8> {
            let mut a = vec![0u8; SECTOR_SIZE as usize];
            a[0..8].copy_from_slice(magic);
            a[0x08] = 1;
            a[0x09] = 20;
            a[0x0a..0x0c].copy_from_slice(&3u16.to_be_bytes());
            a[0x14] = 0x04;
            a[0x15] = 2;
            a[0x20] = channels;
            a[0x21] = if channels == 6 { 5u8 << 3 } else { 0 };
            a[0x22] = channels;
            a[0x40] = 30;
            a[0x41] = 0;
            a[0x42] = 0;
            a[0x45] = n_tracks;
            a[0x50] = 1;
            // locale 0: en / charset 2 (Latin-1).
            a[0x58] = b'e';
            a[0x59] = b'n';
            a[0x5a] = 2;
            a
        };
        // SACDTTxt sector: track_text_position[i] (BE u16) at offset
        // 0x08 + i*2. Each non-zero position points within the sector
        // to a per-track text block:
        //   [track_amount: u8][3 unk bytes]
        //     [type: u8][0x20][string: NUL-term]
        // For tests we want one entry per track (TITLE) so the
        // editor's TITLE entry surfaces, giving us 2-row editor
        // entries (TRACKNUMBER + TITLE).
        let build_t_txt_sector = |titles: &[&str]| -> Vec<u8> {
            let mut s = vec![0u8; SECTOR_SIZE as usize];
            s[0..8].copy_from_slice(SACD_T_TXT_MAGIC);
            // Reserve positions table at 0x08; data blocks start at
            // 0x100 with 0x40 stride between tracks (plenty of room).
            let mut block_off = 0x100usize;
            for (i, title) in titles.iter().enumerate() {
                let pos_off = 0x08 + i * 2;
                s[pos_off..pos_off + 2].copy_from_slice(&(block_off as u16).to_be_bytes());
                // Track block: 1 entry, 3 unk bytes, type=TITLE(0x01), 0x20, NUL-term string.
                s[block_off] = 1; // track_amount
                                  // 3 unknown bytes left as 0
                s[block_off + 4] = 0x01; // TITLE
                s[block_off + 5] = 0x20;
                let bytes = title.as_bytes();
                s[block_off + 6..block_off + 6 + bytes.len()].copy_from_slice(bytes);
                // Trailing NUL written by initial zeros.
                block_off += 0x40;
            }
            s
        };
        f.seek(SeekFrom::Start(540 * SECTOR_SIZE)).unwrap();
        f.write_all(&build_area(TWOCH_TOC_MAGIC, 2, 2)).unwrap();
        f.seek(SeekFrom::Start(541 * SECTOR_SIZE)).unwrap();
        f.write_all(&build_t_txt_sector(&["StereoT1", "StereoT2"]))
            .unwrap();
        f.seek(SeekFrom::Start(600 * SECTOR_SIZE)).unwrap();
        f.write_all(&build_area(MULCH_TOC_MAGIC, 6, 2)).unwrap();
        f.seek(SeekFrom::Start(601 * SECTOR_SIZE)).unwrap();
        f.write_all(&build_t_txt_sector(&["MCH T1", "MCH T2"]))
            .unwrap();
    }

    #[test]
    fn switch_area_to_mch_lands_on_mch_area() {
        let (state, res) = switch_helper(super::super::command::SacdAreaTarget::MultiChannel);
        assert_eq!(res.unwrap(), "MCH");
        assert_eq!(
            state.sacd_area_kind,
            Some(crate::tui::sacd::AreaKind::MultiChannel)
        );
    }

    #[test]
    fn switch_area_toggle_inverts_current() {
        let (state, res) = switch_helper(super::super::command::SacdAreaTarget::Toggle);
        // Starting on stereo → toggle → MCH.
        assert_eq!(res.unwrap(), "MCH");
        assert_eq!(
            state.sacd_area_kind,
            Some(crate::tui::sacd::AreaKind::MultiChannel)
        );
    }

    #[test]
    fn switch_area_to_same_area_returns_err() {
        let (_, res) = switch_helper(super::super::command::SacdAreaTarget::Stereo);
        // Starting state IS stereo, so switching to stereo is a no-op.
        let err = res.unwrap_err();
        assert!(err.contains("already on stereo"), "got {}", err);
    }

    #[test]
    fn switch_area_preserves_cursor_exactly_when_within_new_bounds() {
        // For a strong equality assertion the pre-switch entries
        // shape must match post-switch — which means we have to
        // build initial state from the parser's view of the
        // fixture, not the richer in-memory synthetic md (whose
        // text fields aren't reproduced in the on-disk ISO).
        let td = tempfile::tempdir().expect("tempdir");
        let iso_path = td.path().join("hybrid.iso");
        let dummy_md = make_hybrid_md();
        write_hybrid_iso_fixture(&iso_path, &dummy_md);
        let parsed_md = crate::tui::sacd::parse_sacd_iso(&iso_path).expect("parse");
        let (mut state, _, _) =
            build_sacd_editor_state(&iso_path, &parsed_md, None).expect("build");

        state.cursor = 1; // not row 0 — so a bug that resets to 0 is caught
        let entry_count_before = state.entries.len();
        assert!(
            entry_count_before >= 2,
            "fixture should produce at least 2 editor entries (TRACKNUMBER + TITLE)",
        );

        switch_sacd_editor_area(
            &mut state,
            &iso_path,
            super::super::command::SacdAreaTarget::MultiChannel,
        )
        .expect("switch");

        // Both areas in the fixture have matching shape (same
        // track count, both with TITLE sectors), so no clamp.
        assert_eq!(
            state.entries.len(),
            entry_count_before,
            "fixture should preserve entry count across areas",
        );
        // Strong equality: catches a regression that resets cursor
        // to 0 (the old `cursor < entries.len()` assertion would
        // have passed silently for any in-bounds value).
        assert_eq!(state.cursor, 1, "cursor must be preserved exactly");
    }

    #[test]
    fn switch_area_clamps_out_of_bounds_cursor() {
        // Exercises the
        //   state.cursor = prev_cursor.min(entries.len().saturating_sub(1))
        // clamp by manually setting cursor to an OOB value before the
        // switch, then asserting it's been brought back into bounds.
        // The fixture here uses size=1 area TOCs (header only, no
        // SACDTTxt), so both areas produce a single TRACKNUMBER
        // entry — the clamp triggers because the test sets
        // cursor=3 OOB, not because of an entry-count change across
        // areas. Either way, broken clamp logic (e.g. forgetting
        // the min) would leave cursor=3 and fail the assertion.
        use crate::tui::sacd::*;
        use std::io::{Seek, SeekFrom, Write};

        let td = tempfile::tempdir().expect("tempdir");
        let iso_path = td.path().join("uneven.iso");
        let total = 700u64;
        let f = std::fs::File::create(&iso_path).unwrap();
        f.set_len(total * SECTOR_SIZE).unwrap();
        drop(f);
        let mut f = std::fs::File::options()
            .write(true)
            .open(&iso_path)
            .unwrap();

        let mut mtoc = vec![0u8; 0xa8];
        mtoc[0..8].copy_from_slice(MASTER_TOC_MAGIC);
        mtoc[0x08] = 1;
        mtoc[0x09] = 20;
        mtoc[0x40..0x44].copy_from_slice(&540u32.to_be_bytes()); // stereo: 5 tracks
        mtoc[0x54..0x56].copy_from_slice(&1u16.to_be_bytes());
        mtoc[0x48..0x4c].copy_from_slice(&600u32.to_be_bytes()); // MCH: 1 track
        mtoc[0x56..0x58].copy_from_slice(&1u16.to_be_bytes());
        f.seek(SeekFrom::Start(510 * SECTOR_SIZE)).unwrap();
        f.write_all(&mtoc).unwrap();

        let mut build_area = |magic: &[u8; 8], channels: u8, n_tracks: u8| {
            let mut a = vec![0u8; SECTOR_SIZE as usize];
            a[0..8].copy_from_slice(magic);
            a[0x08] = 1;
            a[0x09] = 20;
            a[0x0a..0x0c].copy_from_slice(&1u16.to_be_bytes());
            a[0x14] = 0x04;
            a[0x15] = 2;
            a[0x20] = channels;
            a[0x21] = if channels == 6 { 5u8 << 3 } else { 0 };
            a[0x22] = channels;
            a[0x40] = 10;
            a[0x45] = n_tracks;
            a
        };
        f.seek(SeekFrom::Start(540 * SECTOR_SIZE)).unwrap();
        f.write_all(&build_area(TWOCH_TOC_MAGIC, 2, 5)).unwrap();
        f.seek(SeekFrom::Start(600 * SECTOR_SIZE)).unwrap();
        f.write_all(&build_area(MULCH_TOC_MAGIC, 6, 1)).unwrap();
        drop(f);

        let md = parse_sacd_iso(&iso_path).expect("parse");
        let (mut state, _, _) = build_sacd_editor_state(&iso_path, &md, None).expect("build");
        // cursor past where MCH could reach (e.g. row 1, MCH only has TRACKNUMBER+TITLE = 2 rows)
        state.cursor = 3;

        switch_sacd_editor_area(
            &mut state,
            &iso_path,
            super::super::command::SacdAreaTarget::MultiChannel,
        )
        .expect("switch");

        assert!(
            state.cursor < state.entries.len(),
            "cursor must be clamped into new entry count: cursor={}, entries={}",
            state.cursor,
            state.entries.len(),
        );
    }

    #[test]
    fn switch_area_refuses_when_dirty_internal_check() {
        // Internal guard: even if a caller forgets to check dirty,
        // the function refuses. Regression guard for the C6 audit's
        // function-vs-caller-contract concern.
        let md = make_hybrid_md();
        let td = tempfile::tempdir().expect("tempdir");
        let iso_path = td.path().join("hybrid.iso");
        write_hybrid_iso_fixture(&iso_path, &md);
        let (mut state, _, _) = build_sacd_editor_state(&iso_path, &md, None).expect("build");
        state.dirty = true; // simulate unsaved edits
        let res = switch_sacd_editor_area(
            &mut state,
            &iso_path,
            super::super::command::SacdAreaTarget::MultiChannel,
        );
        assert!(res.is_err(), "function-level guard should refuse");
        assert!(res.unwrap_err().contains("unsaved edits"));
        // State unchanged.
        assert_eq!(
            state.sacd_area_kind,
            Some(crate::tui::sacd::AreaKind::Stereo)
        );
    }

    #[test]
    fn switch_area_to_missing_area_returns_err() {
        // Build a stereo-only fixture; ask to switch to MCH.
        let md = synth_sacd_metadata(None, None, 0, None, &["s1"], &[""], &[None]);
        // Single-area stereo.
        let td = tempfile::tempdir().unwrap();
        let iso_path = td.path().join("stereo_only.iso");
        use crate::tui::sacd::*;
        use std::io::{Seek, SeekFrom, Write};
        let f = std::fs::File::create(&iso_path).unwrap();
        f.set_len(700 * SECTOR_SIZE).unwrap();
        drop(f);
        let mut f = std::fs::File::options()
            .write(true)
            .open(&iso_path)
            .unwrap();

        let mut mtoc = vec![0u8; 0xa8];
        mtoc[0..8].copy_from_slice(MASTER_TOC_MAGIC);
        mtoc[0x08] = 1;
        mtoc[0x09] = 20;
        mtoc[0x40..0x44].copy_from_slice(&540u32.to_be_bytes());
        mtoc[0x54..0x56].copy_from_slice(&1u16.to_be_bytes());
        f.seek(SeekFrom::Start(510 * SECTOR_SIZE)).unwrap();
        f.write_all(&mtoc).unwrap();

        let mut area = vec![0u8; SECTOR_SIZE as usize];
        area[0..8].copy_from_slice(TWOCH_TOC_MAGIC);
        area[0x08] = 1;
        area[0x09] = 20;
        area[0x0a..0x0c].copy_from_slice(&1u16.to_be_bytes());
        area[0x14] = 0x04;
        area[0x15] = 2;
        area[0x20] = 2;
        area[0x22] = 2;
        area[0x40] = 5;
        area[0x45] = 1;
        f.seek(SeekFrom::Start(540 * SECTOR_SIZE)).unwrap();
        f.write_all(&area).unwrap();
        drop(f);

        let (mut state, _, _) = build_sacd_editor_state(&iso_path, &md, None).expect("build");
        let res = switch_sacd_editor_area(
            &mut state,
            &iso_path,
            super::super::command::SacdAreaTarget::MultiChannel,
        );
        let err = res.unwrap_err();
        assert!(err.contains("no multi-channel area"), "got {}", err);
    }

    #[test]
    fn sidecar_hybrid_disc_routes_mch_tracks_when_mch_area_selected() {
        // Stereo absent, MCH present. Sidecar has tracks for both areas
        // but we should pull from area 2 (tracks 3-4 by ID with
        // TOTALTRACKS=2).
        let mut md = synth_sacd_metadata(
            None,
            None,
            0,
            None,
            &["mcsb1", "mcsb2"],
            &["", ""],
            &[None, None],
        );
        let mut info = md.stereo.take().unwrap();
        info.header.kind = crate::tui::sacd::AreaKind::MultiChannel;
        info.header.channel_count = 6;
        md.multi_channel = Some(info);

        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="TITLE" value="StereoT1"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="2"/></track>
<track id="2"><meta name="TITLE" value="StereoT2"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="2"/></track>
<track id="3"><meta name="TITLE" value="MCH T1"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="2"/></track>
<track id="4"><meta name="TITLE" value="MCH T2"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="2"/></track>
</store></root>"#;
        let sidecar = parse_sidecar_for_test(xml);
        let path = std::path::PathBuf::from("/tmp/x.iso");
        let (state, label, _) = build_sacd_editor_state(&path, &md, Some(&sidecar)).expect("build");
        assert_eq!(label, "MCH");
        let title = state
            .entries
            .iter()
            .find(|e| e.display_key == "TITLE")
            .expect("TITLE");
        assert_eq!(title.per_file_values, vec!["MCH T1", "MCH T2"]);
    }

    // ── Phase D: stereo ↔ MCH mirror-write ────────────────────────

    #[test]
    fn is_per_area_specific_key_covers_dr_keys() {
        assert!(super::is_per_area_specific_key("DYNAMIC RANGE"));
        assert!(super::is_per_area_specific_key("ALBUM DYNAMIC RANGE"));
        assert!(super::is_per_area_specific_key("dynamic range"));
        assert!(!super::is_per_area_specific_key("ALBUM"));
        assert!(!super::is_per_area_specific_key("ARTIST"));
        assert!(!super::is_per_area_specific_key("REPLAYGAIN_TRACK_GAIN"));
    }

    /// Build a hybrid (stereo + MCH) `SacdMetadata` with both areas
    /// holding `n` tracks each. Mirrors `synth_sacd_metadata` but adds
    /// the MCH area.
    fn synth_hybrid_sacd_metadata(
        album_title: Option<&str>,
        stereo_titles: &[&str],
        mch_titles: &[&str],
    ) -> crate::tui::sacd::SacdMetadata {
        use crate::tui::sacd::*;
        let mut md = synth_sacd_metadata(
            album_title,
            None,
            0,
            None,
            stereo_titles,
            &vec![""; stereo_titles.len()],
            &vec![None; stereo_titles.len()],
        );
        // Synthesize MCH area by reusing the stereo header shape with
        // MCH kind + the requested track count.
        let mut mch_tracks: Vec<TrackEntry> = Vec::with_capacity(mch_titles.len());
        for (i, t) in mch_titles.iter().enumerate() {
            mch_tracks.push(TrackEntry {
                start_lsn: 5000 + i as u32 * 100,
                length_lsn: 100,
                start_time: PlayTime::default(),
                duration: PlayTime {
                    minutes: 3,
                    seconds: 0,
                    frames: 0,
                },
                text: TrackText {
                    title: (!t.is_empty()).then(|| t.to_string()),
                    ..Default::default()
                },
                isrc: None,
                structured_isrc: None,
                genre: None,
            });
        }
        let mut mch_header = md.stereo.as_ref().unwrap().header.clone();
        mch_header.kind = AreaKind::MultiChannel;
        mch_header.channel_count = 6;
        mch_header.track_count = mch_titles.len() as u8;
        md.multi_channel = Some(AreaInfo {
            header: mch_header,
            tracks: mch_tracks,
            consistency: Default::default(),
        });
        md
    }

    /// Round-trip helper for hybrid saves: writes a sidecar XML to a
    /// tempfile, builds the editor state from a hybrid `SacdMetadata`,
    /// lets the test mutate the state, runs `save_sacd_sidecar`, and
    /// returns the re-parsed sidecar alongside the save outcome.
    fn round_trip_hybrid_save(
        sidecar_xml: &str,
        md: &crate::tui::sacd::SacdMetadata,
        area: crate::tui::sacd::AreaKind,
        mutate: impl FnOnce(&mut super::super::app::MetadataEditorState),
    ) -> (
        tempfile::TempDir,
        crate::tui::sacd_sidecar::SidecarMetadata,
        super::SacdSaveOutcome,
    ) {
        use crate::tui::sacd_sidecar::*;
        let td = tempfile::tempdir().expect("tempdir");
        let xml_path = td.path().join("disc.xml");
        std::fs::write(&xml_path, sidecar_xml).unwrap();
        let sidecar = parse_sidecar(&xml_path).expect("parse");
        let iso_path = td.path().join("disc.iso");
        std::fs::write(&iso_path, b"\0").unwrap();
        let (mut state, _, _) =
            build_sacd_editor_state(&iso_path, md, Some(&sidecar)).expect("build");
        state.read_only = false;
        state.sacd_sidecar_path = Some(xml_path.clone());
        state.sacd_area_kind = Some(area);
        mutate(&mut state);
        let outcome = super::save_sacd_sidecar(&state, &xml_path).expect("save");
        let reparsed = parse_sidecar(&xml_path).expect("re-parse");
        (td, reparsed, outcome)
    }

    /// Hybrid sidecar with 2 stereo tracks (ids 1-2) + 2 MCH tracks
    /// (ids 3-4), TRACKNUMBER `"01"`/`"02"` on each side. Album-level
    /// fields seeded with deliberately different values so the mirror
    /// has something visible to overwrite.
    const SAMPLE_HYBRID_2X2: &str = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="TITLE" value="St-1"/><meta name="ALBUM" value="StereoAlbum"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="2"/></track>
<track id="2"><meta name="TITLE" value="St-2"/><meta name="ALBUM" value="StereoAlbum"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="2"/></track>
<track id="3"><meta name="TITLE" value="MC-1"/><meta name="ALBUM" value="MCHAlbum"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="2"/></track>
<track id="4"><meta name="TITLE" value="MC-2"/><meta name="ALBUM" value="MCHAlbum"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="2"/></track>
</store></root>"#;

    #[test]
    fn save_sacd_sidecar_writes_multi_tab_areas_without_implicit_mirror() {
        use crate::disc::model::{PresentationId, SacdAreaId};
        use crate::tui::sacd_sidecar::parse_sidecar;

        let md = synth_hybrid_sacd_metadata(Some("Album"), &["St-1", "St-2"], &["MC-1", "MC-2"]);
        let td = tempfile::tempdir().expect("tempdir");
        let xml_path = td.path().join("disc.xml");
        std::fs::write(&xml_path, SAMPLE_HYBRID_2X2).unwrap();
        let sidecar = parse_sidecar(&xml_path).expect("parse");
        let iso_path = td.path().join("disc.iso");
        std::fs::write(&iso_path, b"\0").unwrap();

        let (mut state, _, _) = build_sacd_multitab_editor_state(&iso_path, &md, Some(&sidecar))
            .expect("build multi-tab editor");
        state.read_only = false;
        state.sacd_sidecar_path = Some(xml_path.clone());

        let stereo = state
            .presentation_tabs
            .iter_mut()
            .find(|tab| matches!(&tab.id, PresentationId::SacdArea(SacdAreaId::Stereo)))
            .expect("stereo tab");
        let title = stereo
            .entries
            .iter_mut()
            .find(|entry| entry.display_key == "TITLE")
            .expect("stereo TITLE");
        title.per_file_values[0] = "Stereo New".into();
        title.value = "<multiple values>".into();
        title.is_mixed = true;
        stereo.dirty = true;

        let mch = state
            .presentation_tabs
            .iter_mut()
            .find(|tab| matches!(&tab.id, PresentationId::SacdArea(SacdAreaId::MultiChannel)))
            .expect("multi-channel tab");
        let title = mch
            .entries
            .iter_mut()
            .find(|entry| entry.display_key == "TITLE")
            .expect("MCH TITLE");
        title.per_file_values[0] = "MCH New".into();
        title.value = "<multiple values>".into();
        title.is_mixed = true;
        mch.dirty = true;

        let outcome = super::save_sacd_sidecar(&state, &xml_path).expect("save");
        assert!(outcome.mirror.sibling_present);
        assert_eq!(outcome.mirror.sibling_total, 0);
        assert_eq!(outcome.mirror.mirrored_count, 0);

        let reparsed = parse_sidecar(&xml_path).expect("re-parse");
        let title_for = |id| {
            reparsed
                .tracks
                .iter()
                .find(|track| track.id == id)
                .and_then(|track| track.meta.get("TITLE"))
                .map(String::as_str)
        };
        assert_eq!(title_for(1), Some("Stereo New"));
        assert_eq!(title_for(2), Some("St-2"));
        assert_eq!(title_for(3), Some("MCH New"));
        assert_eq!(title_for(4), Some("MC-2"));
    }

    #[test]
    fn save_sacd_sidecar_mirrors_album_level_edits_across_areas() {
        let md =
            synth_hybrid_sacd_metadata(Some("StereoAlbum"), &["St-1", "St-2"], &["MC-1", "MC-2"]);
        let (_td, reparsed, outcome) = round_trip_hybrid_save(
            SAMPLE_HYBRID_2X2,
            &md,
            crate::tui::sacd::AreaKind::Stereo,
            |state| {
                let album = state
                    .entries
                    .iter_mut()
                    .find(|e| e.display_key == "ALBUM")
                    .expect("ALBUM");
                album.value = "NewAlbum".into();
                album.per_file_values = vec!["NewAlbum".into(), "NewAlbum".into()];
            },
        );
        assert!(outcome.mirror.sibling_present);
        assert_eq!(outcome.mirror.mirrored_count, 2);
        assert_eq!(outcome.mirror.sibling_total, 2);
        // All four tracks in BOTH areas should have the new album.
        for tid in 1..=4 {
            let t = reparsed.tracks.iter().find(|t| t.id == tid).expect("track");
            assert_eq!(
                t.meta.get("ALBUM").map(String::as_str),
                Some("NewAlbum"),
                "tid={} album-level mirror should overwrite",
                tid,
            );
        }
    }

    #[test]
    fn save_sacd_sidecar_mirrors_per_track_edits_by_tracknumber() {
        let md = synth_hybrid_sacd_metadata(Some("Album"), &["St-1", "St-2"], &["MC-1", "MC-2"]);
        let (_td, reparsed, outcome) = round_trip_hybrid_save(
            SAMPLE_HYBRID_2X2,
            &md,
            crate::tui::sacd::AreaKind::Stereo,
            |state| {
                let title = state
                    .entries
                    .iter_mut()
                    .find(|e| e.display_key == "TITLE")
                    .expect("TITLE");
                // Edit track 1's title only.
                title.per_file_values[0] = "NewTitle1".into();
                title.is_mixed = true;
                title.value = "<multiple values>".into();
            },
        );
        assert!(outcome.mirror.sibling_present);
        assert_eq!(outcome.mirror.mirrored_count, 2);
        // Active area (stereo): track 1 → NewTitle1, track 2 → St-2.
        let st1 = reparsed.tracks.iter().find(|t| t.id == 1).expect("st1");
        let st2 = reparsed.tracks.iter().find(|t| t.id == 2).expect("st2");
        assert_eq!(st1.meta.get("TITLE").map(String::as_str), Some("NewTitle1"));
        assert_eq!(st2.meta.get("TITLE").map(String::as_str), Some("St-2"));
        // Mirror to MCH area by TRACKNUMBER match (ids 3,4 have TN
        // "01","02"). MCH track 1 should pick up "NewTitle1" from
        // editor row matching TN="01"; MCH track 2 stays untouched
        // because the editor's row at TN="02" wasn't edited from
        // the existing sidecar value.
        let mc1 = reparsed.tracks.iter().find(|t| t.id == 3).expect("mc1");
        let mc2 = reparsed.tracks.iter().find(|t| t.id == 4).expect("mc2");
        assert_eq!(
            mc1.meta.get("TITLE").map(String::as_str),
            Some("NewTitle1"),
            "MCH track 1 (TN=01) should mirror NewTitle1"
        );
        // mc2 mirror: editor row 1 has TN=02 with stereo value "St-2";
        // the per-track mirror writes that to MCH track 2.
        assert_eq!(
            mc2.meta.get("TITLE").map(String::as_str),
            Some("St-2"),
            "MCH track 2 (TN=02) gets editor row 1's TITLE (which equals stereo's)"
        );
    }

    #[test]
    fn save_sacd_sidecar_mirror_handles_track_count_divergence() {
        // Stereo has 3 tracks; MCH has only 2 (no MCH track for stereo's
        // TN=03). Mirror should match TN 01/02 and leave the third
        // stereo TN unrepresented in MCH — sibling_total=2,
        // mirrored_count=2 (because both MCH tracks have a TN that
        // exists in the editor; the "lost" track is on the stereo
        // side, not the MCH side).
        let xml = r#"<root><store id="X" type="SACD" version="1.1">
<track id="1"><meta name="TITLE" value="St-1"/><meta name="ALBUM" value="Old"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="3"/></track>
<track id="2"><meta name="TITLE" value="St-2"/><meta name="ALBUM" value="Old"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="3"/></track>
<track id="3"><meta name="TITLE" value="St-3"/><meta name="ALBUM" value="Old"/><meta name="TRACKNUMBER" value="03"/><meta name="TOTALTRACKS" value="3"/></track>
<track id="4"><meta name="TITLE" value="MC-1"/><meta name="ALBUM" value="Old"/><meta name="TRACKNUMBER" value="01"/><meta name="TOTALTRACKS" value="2"/></track>
<track id="5"><meta name="TITLE" value="MC-2"/><meta name="ALBUM" value="Old"/><meta name="TRACKNUMBER" value="02"/><meta name="TOTALTRACKS" value="2"/></track>
</store></root>"#;
        let md =
            synth_hybrid_sacd_metadata(Some("Old"), &["St-1", "St-2", "St-3"], &["MC-1", "MC-2"]);
        let (_td, reparsed, outcome) =
            round_trip_hybrid_save(xml, &md, crate::tui::sacd::AreaKind::Stereo, |state| {
                let album = state
                    .entries
                    .iter_mut()
                    .find(|e| e.display_key == "ALBUM")
                    .expect("ALBUM");
                album.value = "New".into();
                album.per_file_values = vec!["New".into(); 3];
            });
        assert!(outcome.mirror.sibling_present);
        assert_eq!(outcome.mirror.sibling_total, 2);
        assert_eq!(
            outcome.mirror.mirrored_count, 2,
            "both MCH tracks have TNs that exist in editor → both mirrored"
        );
        // All five tracks across both areas get the new album-level
        // value via the active-area replicate + sibling mirror.
        for tid in 1..=5 {
            let t = reparsed.tracks.iter().find(|t| t.id == tid).expect("track");
            assert_eq!(
                t.meta.get("ALBUM").map(String::as_str),
                Some("New"),
                "tid={} album-level should be New",
                tid
            );
        }
        // Original per-track titles preserved (we only edited ALBUM).
        // MCH track with TN=03 doesn't exist; MCH tracks keep their
        // TITLEs (mirror copies editor row's TITLE → that's the
        // stereo TITLE; not what we want for per-track ≠ album-
        // level. But the test only edited ALBUM, so this trace is
        // bound to the album-level path.).
    }
}
