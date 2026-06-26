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
    // Set the message channel on BrowseState so navigation methods can
    // spawn async scans. Must happen before the event loop starts.
    app.browse.set_tx(tx.clone());

    loop {
        // 1. Refresh items from the manager
        app.refresh_items();
        app.clamp_selection();
        app.clear_expired_status();
        check_pending_browse_rename(app);
        check_batch_probe_debounce(app, &tx);
        check_search_debounce(app, &tx);
        // Close browse-only overlays if the user has left the browse screen.
        if app.current_screen != AppScreen::Browse && app.bookmarks.overlay_open {
            app.bookmarks.close_overlay();
        }

        // 2. Render
        if app.force_redraw {
            terminal.clear()?;
            app.force_redraw = false;
        }
        terminal.draw(|f| draw_ui(f, app))?;

        // 3. Check quit
        if app.should_quit {
            // Save queue before exiting
            app.save_queue();
            break;
        }

        // 4. Drain async messages
        while let Ok(msg) = rx.try_recv() {
            handle_message(app, msg, &tx);
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
/// Fire a debounced batch cursor probe if the deadline has passed and
/// the cursor is still on the target path.
fn check_batch_probe_debounce(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let (path, deadline) = match app.convert.source.batch_probe_debounce.take() {
        Some(d) => d,
        None => return,
    };

    if std::time::Instant::now() < deadline {
        // Not ready yet — put it back.
        app.convert.source.batch_probe_debounce = Some((path, deadline));
        return;
    }

    // Verify cursor is still on this path.
    let still_current = match &app.convert.source.mode {
        super::app::SourceMode::Batch { paths, cursor, .. } => paths.get(*cursor) == Some(&path),
        _ => false,
    };

    if still_current {
        // Skip if already in flight (dedup guard).
        if app.convert.source.batch_probe_pending.as_ref() != Some(&path) {
            app.convert.source.batch_probe_pending = Some(path.clone());
            spawn_batch_cursor_probe(path, tx.clone());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BatchCursorProbeKind {
    Audio,
    CueProxy,
}

fn batch_cursor_probe_kind(path: &std::path::Path) -> BatchCursorProbeKind {
    if super::browse::is_cue_sheet_path(path) {
        BatchCursorProbeKind::CueProxy
    } else {
        BatchCursorProbeKind::Audio
    }
}

fn spawn_batch_cursor_probe(path: std::path::PathBuf, tx: mpsc::Sender<AppMessage>) {
    match batch_cursor_probe_kind(&path) {
        BatchCursorProbeKind::Audio => super::browse::spawn_audio_probe(path, tx),
        BatchCursorProbeKind::CueProxy => super::browse::spawn_cue_proxy_audio_probe(path, tx),
    }
}

/// Fire a debounced search if the user has stopped typing for 200ms.
fn check_search_debounce(
    app: &mut AppState,
    tx: &tokio::sync::mpsc::Sender<super::message::AppMessage>,
) {
    if !app.browse.search.active {
        return;
    }
    let deadline = match app.browse.search.last_keystroke {
        Some(t) => t + std::time::Duration::from_millis(200),
        None => return,
    };
    if std::time::Instant::now() < deadline {
        return;
    }
    app.browse.search.last_keystroke = None;
    app.browse.execute_search(Some(tx));
}

fn check_pending_browse_rename(app: &mut AppState) {
    let (path, deadline) = match app.pending_browse_rename.as_ref() {
        Some(pr) => (pr.0.clone(), pr.1),
        None => return,
    };

    // Cancel if the user left the browse screen or has an overlay open already.
    if app.current_screen != AppScreen::Browse || !matches!(app.active_overlay, ActiveOverlay::None)
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

fn metadata_editor_apply_replaygain_metadata(
    surface: &mut super::app::PresentationTab,
    paths: &[std::path::PathBuf],
    metadata: &[super::probe::SourceMetadata],
) {
    use lofty::tag::ItemKey;

    let fields: [(&str, ItemKey, fn(&super::probe::SourceMetadata) -> Option<String>); 4] = [
        (
            "REPLAYGAIN_TRACK_GAIN",
            ItemKey::ReplayGainTrackGain,
            |m| m.rg_track_gain.clone(),
        ),
        (
            "REPLAYGAIN_TRACK_PEAK",
            ItemKey::ReplayGainTrackPeak,
            |m| m.rg_track_peak.clone(),
        ),
        (
            "REPLAYGAIN_ALBUM_GAIN",
            ItemKey::ReplayGainAlbumGain,
            |m| m.rg_album_gain.clone(),
        ),
        (
            "REPLAYGAIN_ALBUM_PEAK",
            ItemKey::ReplayGainAlbumPeak,
            |m| m.rg_album_peak.clone(),
        ),
    ];

    for (label, key, getter) in fields {
        let values = metadata_editor_ordered_values_for_paths(surface, paths, metadata, label, getter);
        metadata_editor_upsert_per_file_entry(surface, label, key, values);
    }
}

fn metadata_editor_ordered_values_for_paths(
    surface: &super::app::PresentationTab,
    paths: &[std::path::PathBuf],
    metadata: &[super::probe::SourceMetadata],
    label: &str,
    getter: fn(&super::probe::SourceMetadata) -> Option<String>,
) -> Vec<String> {
    let mut by_path = std::collections::HashMap::new();
    for (path, meta) in paths.iter().zip(metadata.iter()) {
        by_path.insert(path.clone(), getter(meta).unwrap_or_default());
    }
    surface
        .paths
        .iter()
        .enumerate()
        .map(|(idx, path)| {
            by_path
                .get(path)
                .cloned()
                .unwrap_or_else(|| metadata_editor_existing_entry_value(surface, idx, label).unwrap_or_default())
        })
        .collect()
}

fn metadata_editor_existing_entry_value(
    surface: &super::app::PresentationTab,
    idx: usize,
    label: &str,
) -> Option<String> {
    surface
        .entries
        .iter()
        .find(|entry| entry.display_key.eq_ignore_ascii_case(label))
        .and_then(|entry| {
            entry
                .per_file_values
                .get(idx)
                .cloned()
                .or_else(|| (!entry.value.trim().is_empty()).then(|| entry.value.clone()))
        })
}

fn metadata_editor_upsert_per_file_entry(
    surface: &mut super::app::PresentationTab,
    label: &str,
    item_key: lofty::tag::ItemKey,
    values: Vec<String>,
) {
    if values.is_empty() {
        return;
    }
    let has_value = values.iter().any(|value| !value.trim().is_empty());
    let Some(idx) = surface
        .entries
        .iter()
        .position(|entry| entry.display_key.eq_ignore_ascii_case(label))
        .or_else(|| has_value.then(|| surface.entries.len()))
    else {
        return;
    };

    let is_mixed = values.windows(2).any(|w| w[0] != w[1]);
    let value = if is_mixed {
        "<multiple values>".to_string()
    } else {
        values.first().cloned().unwrap_or_default()
    };
    if idx == surface.entries.len() {
        surface.entries.push(super::probe::TagEntry {
            display_key: label.to_string(),
            item_key,
            value: value.clone(),
            original: value,
            is_binary: false,
            is_mixed,
            per_file_values: values.clone(),
            per_file_originals: values,
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });
    } else if let Some(entry) = surface.entries.get_mut(idx) {
        entry.value = value.clone();
        entry.original = value;
        entry.is_binary = false;
        entry.is_mixed = is_mixed;
        entry.per_file_values = values.clone();
        entry.per_file_originals = values;
        entry.mb_proposed_value = None;
        entry.mb_proposed_per_file = None;
    }
}

fn metadata_editor_apply_artwork_metadata(
    surface: &mut super::app::PresentationTab,
    paths: &[std::path::PathBuf],
    metadata: &[super::probe::SourceMetadata],
) {
    let mut by_path = std::collections::HashMap::new();
    for (path, meta) in paths.iter().zip(metadata.iter()) {
        by_path.insert(path.clone(), meta.artwork.clone());
    }
    for file in &mut surface.technical_details.files {
        if let Some(artwork) = by_path.get(&file.file_facts.path) {
            file.artwork_facts.entries = artwork.clone();
        }
    }
}

fn reduce_file_picker_complete(
    app: &mut AppState,
    target: super::app::FilePickerTarget,
    path: Option<std::path::PathBuf>,
    tx: &mpsc::Sender<AppMessage>,
) {
    match target.clone() {
        super::app::FilePickerTarget::MetadataArtwork { picture_type } => {
            let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
            match overlay {
                ActiveOverlay::MetadataEditor(mut state) => {
                    let matches_open_picker = state
                        .file_picker
                        .as_ref()
                        .map(|picker| picker.target == target)
                        .unwrap_or(false);
                    state.file_picker = None;
                    state.pending_artwork_type = None;

                    if !matches_open_picker {
                        app.set_status("file picker: ignored stale metadata-artwork completion");
                    } else if let Some(path) = path {
                        super::metadata_editor_actions::dispatch_artwork_write(
                            app,
                            &mut state,
                            path,
                            picture_type,
                            tx,
                        );
                    } else {
                        app.set_status("metadata editor: file picker cancelled");
                    }

                    app.active_overlay = ActiveOverlay::MetadataEditor(state);
                }
                other => {
                    app.active_overlay = other;
                    app.set_status("file picker: ignored metadata-artwork completion without an active editor");
                }
            }
        }
        super::app::FilePickerTarget::Generic { id } => {
            close_matching_file_picker(app, &target);
            match path {
                Some(path) => app.set_status(format!(
                    "file picker target '{id}' selected {}",
                    path.display()
                )),
                None => app.set_status(format!("file picker target '{id}' cancelled")),
            }
        }
    }
}

fn close_matching_file_picker(app: &mut AppState, target: &super::app::FilePickerTarget) {
    let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
    match overlay {
        ActiveOverlay::MetadataEditor(mut state) => {
            if state
                .file_picker
                .as_ref()
                .map(|picker| &picker.target == target)
                .unwrap_or(false)
            {
                state.file_picker = None;
                state.pending_artwork_type = None;
            }
            app.active_overlay = ActiveOverlay::MetadataEditor(state);
        }
        other => {
            app.active_overlay = other;
        }
    }
}

fn handle_message(app: &mut AppState, msg: AppMessage, tx: &mpsc::Sender<AppMessage>) {
    match msg {
        AppMessage::ClearTrackProgress {
            item_id,
            track_index,
            track_epoch,
        } => {
            app.manager
                .clear_track_progress(&item_id, track_index, track_epoch);
        }
        AppMessage::ConversionProgress {
            item_id,
            track_index,
            track_epoch,
            progress,
            status,
        } => {
            // Track-scoped updates only affect display sub-lines.
            if let Some(idx) = track_index {
                app.manager
                    .update_track_progress(&item_id, idx, &status, progress, track_epoch);
                return;
            }

            // Capture terminal state info BEFORE the status is moved into the item.
            let history_data = match &status {
                crate::convert::ConversionStatus::Completed { output_path, .. } => Some((
                    true,
                    Some(output_path.display().to_string()),
                    None::<String>,
                )),
                crate::convert::ConversionStatus::Partial { output_path, .. } => Some((
                    true,
                    Some(output_path.display().to_string()),
                    None::<String>,
                )),
                crate::convert::ConversionStatus::Failed { error, .. } => {
                    Some((false, None, Some(error.clone())))
                }
                _ => None,
            };

            app.manager.update_item_status(&item_id, status, progress);

            // Save queue + record history on terminal states.
            if let Some((success, output_path, error_msg)) = history_data {
                app.save_queue();

                // Record in conversion history (read item from snapshot for metadata).
                if let Some(item) = app.items_snapshot.iter().find(|i| i.id == item_id) {
                    let now = chrono::Utc::now().to_rfc3339();
                    let rg_mode = if item.options.calculate_replaygain {
                        item.options
                            .replaygain_mode
                            .as_ref()
                            .map(|m| format!("{:?}", m))
                    } else {
                        None
                    };
                    let _ = app.db.record_conversion(
                        &item.input_path.display().to_string(),
                        output_path.as_deref(),
                        Some(&format!("{:?}", item.input_format)),
                        item.output_format.name(),
                        item.options.target_sample_rate,
                        item.options
                            .target_bit_depth
                            .map(|d| format!("{}", d))
                            .as_deref(),
                        item.options
                            .dither_type
                            .as_ref()
                            .map(|d| format!("{:?}", d))
                            .as_deref(),
                        rg_mode.as_deref(),
                        Some(item.file_size),
                        None, // output_size computed later by log writer if enabled
                        Some(&item.queued_at.to_rfc3339()),
                        item.started_at.as_ref().map(|t| t.to_rfc3339()).as_deref(),
                        &now,
                        success,
                        error_msg.as_deref(),
                    );
                }
            }
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
            app.save_queue();
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
            app.save_queue();
        }
        AppMessage::StatusMessage(msg) => {
            app.set_status(msg);
        }
        AppMessage::Redraw => {} // Just triggers a redraw via the loop
        AppMessage::AudioProbeComplete { path, result } => {
            app.browse.probe_pending.remove(&path);
            match *result {
                Ok(mut info) => {
                    let is_cue_proxy_result = super::browse::is_cue_sheet_path(&path);
                    if !is_cue_proxy_result {
                        // Check analysis cache for HDCD / PE info that should
                        // be surfaced in the info pane. CUE proxy results are
                        // keyed by the `.cue` path but describe referenced audio
                        // images, so do not run image-analysis lookups or persist
                        // them as ordinary file probes under the text-file path.
                        info.metadata.preemphasis_metadata =
                            super::probe::preemphasis_metadata_check_pub(&path);
                        if let Ok(meta) = std::fs::metadata(&path) {
                            let mtime = meta
                                .modified()
                                .map(crate::db::systemtime_to_unix)
                                .unwrap_or(0);
                            let path_key = path.display().to_string();
                            if let Some(analysis) = app.db.get_cached_analysis(
                                &path_key,
                                mtime,
                                meta.len(),
                            ) {
                                if analysis.hdcd_detected == Some(true) {
                                    info.metadata.hdcd_detail = analysis.hdcd_detail;
                                }
                            } else if let Some(facts) = app.db.get_cached_metadata_analysis_facts(
                                &path_key,
                                mtime,
                                meta.len(),
                            ) {
                                if facts.hdcd_detected == Some(true) {
                                    info.metadata.hdcd_detail = facts.hdcd_detail;
                                }
                            }
                        }

                        // Clone for the browse cache (shared via Arc).
                        app.browse
                            .probe_cache
                            .insert(path.clone(), Some(std::sync::Arc::new(info.clone())));

                        // Persist to SQLite probe cache for cross-session reuse.
                        if let Ok(meta) = std::fs::metadata(&path) {
                            let mtime = meta
                                .modified()
                                .map(crate::db::systemtime_to_unix)
                                .unwrap_or(0);
                            let row = crate::db::CachedProbeRow::from_cached_info(&info);
                            let _ = app.db.store_probe(
                                &path.display().to_string(),
                                mtime,
                                meta.len(),
                                &row,
                            );
                        }
                    }

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
                    // Clear the batch probe pending flag if this result matches.
                    if app.convert.source.batch_probe_pending.as_ref() == Some(&path) {
                        app.convert.source.batch_probe_pending = None;
                    }

                    match &mut app.convert.source.mode {
                        super::app::SourceMode::Batch {
                            paths,
                            cursor,
                            cursor_info,
                            cursor_metadata,
                            cursor_probe_notice,
                            ..
                        } => {
                            if paths.get(*cursor).map(|p| p == &path).unwrap_or(false) {
                                *cursor_info = Some(probed_info);
                                *cursor_metadata = probed_metadata;
                                *cursor_probe_notice = None;
                            }
                        }
                        super::app::SourceMode::Single {
                            path: single_path,
                            info: info_slot,
                            metadata: metadata_slot,
                            probe_notice,
                        } => {
                            if single_path == &path {
                                *info_slot = Some(probed_info);
                                *metadata_slot = probed_metadata;
                                *probe_notice = None;
                            }
                        }
                        super::app::SourceMode::MultiTrack {
                            path: mt_path,
                            info: info_slot,
                            metadata: metadata_slot,
                            ..
                        } => {
                            if mt_path == &path {
                                *info_slot = Some(probed_info);
                                *metadata_slot = probed_metadata;
                            }
                        }
                        super::app::SourceMode::Empty => {}
                    }

                    // Apply source-aware format defaults now that probe info is available.
                    app.convert.apply_source_defaults();
                }
                Err(err) => {
                    let is_cue_proxy_result = super::browse::is_cue_sheet_path(&path);
                    if app.convert.source.batch_probe_pending.as_ref() == Some(&path) {
                        app.convert.source.batch_probe_pending = None;
                    }

                    if is_cue_proxy_result {
                        let notice = if err.trim().is_empty() {
                            "CUE proxy probe failed; set format manually".to_string()
                        } else {
                            err
                        };
                        if let super::app::SourceMode::Batch {
                            paths,
                            cursor,
                            cursor_info,
                            cursor_metadata,
                            cursor_probe_notice,
                            ..
                        } = &mut app.convert.source.mode
                        {
                            if paths.get(*cursor).map(|p| p == &path).unwrap_or(false) {
                                *cursor_info = None;
                                *cursor_metadata = super::probe::SourceMetadata::default();
                                *cursor_probe_notice = Some((path.clone(), notice));
                                app.convert.apply_source_defaults();
                            }
                        }
                    } else {
                        // Cache the failure so we don't retry; renderer falls back
                        // to basic info (path + size) when the value is None.
                        app.browse.probe_cache.insert(path, None);
                    }
                }
            }
        }
        AppMessage::DiscProbeComplete { path, fingerprint, result } => {
            app.browse.disc_probe_pending.remove(&path);
            match (fingerprint, *result) {
                (Some(fingerprint), Ok(contents)) => {
                    if crate::tui::disc_browser::disc_probe_fingerprint(&path)
                        .map(|current| current == fingerprint)
                        .unwrap_or(false)
                    {
                        app.browse.disc_probe_cache.insert(
                            path.clone(),
                            crate::tui::disc_browser::DiscProbeCacheEntry::from_success(fingerprint, contents),
                        );
                        if let Some(followup) = app.browse.disc_probe_followup.remove(&path) {
                            super::disc_browser_actions::handle_disc_probe_followup(app, &path, followup);
                        }
                    }
                }
                (Some(fingerprint), Err(error)) => {
                    if crate::tui::disc_browser::disc_probe_fingerprint(&path)
                        .map(|current| current == fingerprint)
                        .unwrap_or(false)
                    {
                        app.browse.disc_probe_cache.insert(
                            path.clone(),
                            crate::tui::disc_browser::DiscProbeCacheEntry::from_error(fingerprint, error.clone()),
                        );
                    }
                    app.browse.disc_probe_followup.remove(&path);
                    app.set_status(format!("Disc analysis failed: {error}"));
                }
                (None, Err(error)) => {
                    app.browse.disc_probe_cache.remove(&path);
                    app.browse.disc_probe_followup.remove(&path);
                    app.set_status(format!("Disc analysis failed: {error}"));
                }
                (None, Ok(contents)) => {
                    if let Ok(fingerprint) = crate::tui::disc_browser::disc_probe_fingerprint(&path) {
                        app.browse.disc_probe_cache.insert(
                            path.clone(),
                            crate::tui::disc_browser::DiscProbeCacheEntry::from_success(fingerprint, contents),
                        );
                    }
                }
            }
        }
        AppMessage::DirStatsComplete { path, stats } => {
            app.browse.dir_stats_pending.remove(&path);
            app.browse
                .dir_stats_cache
                .insert(path, std::sync::Arc::new(stats));
        }
        AppMessage::AnalysisComplete { result } => {
            app.analysis_pending = app.analysis_pending.saturating_sub(1);
            // Clean up single-image temp dir when all analysis tasks complete.
            if app.analysis_pending == 0 {
                if let Some(tmp_dir) = app.analysis_temp_dir.take() {
                    let _ = std::fs::remove_dir_all(&tmp_dir);
                }
            }
            match result {
                Ok(result) => {
                    // Persist to SQLite analysis cache for cross-session reuse.
                    if let Ok(meta) = std::fs::metadata(&result.path) {
                        let mtime = meta
                            .modified()
                            .map(crate::db::systemtime_to_unix)
                            .unwrap_or(0);
                        if let Err(e) = app.db.store_analysis(
                            &result.path.display().to_string(),
                            mtime,
                            meta.len(),
                            &result,
                        ) {
                            log::error!("analysis cache store failed: {}", e);
                        }
                    }

                    // Update probe cache with HDCD info so the info
                    // pane shows it without re-probing.
                    if result.hdcd_detected == Some(true) {
                        if let Some(Some(cached)) = app.browse.probe_cache.get(&result.path) {
                            let mut info = (**cached).clone();
                            info.metadata.hdcd_detail = result.hdcd_detail.clone();
                            app.browse
                                .probe_cache
                                .insert(result.path.clone(), Some(std::sync::Arc::new(info)));
                        }
                    }

                    if let ActiveOverlay::MetadataEditor(ref mut state) = app.active_overlay {
                        state.apply_analysis_result(&result);
                    }
                    if let Some(state) = app.pending_metadata_editor.as_mut() {
                        state.apply_analysis_result(&result);
                    }
                    app.analysis_results.push(*result);
                    if app.analysis_pending == 0 {
                        // Sort results by disc/track for logical display order.
                        {
                            let mut result_paths: Vec<std::path::PathBuf> = app
                                .analysis_results
                                .iter()
                                .map(|r| r.path.clone())
                                .collect();
                            crate::tui::probe::sort_paths_by_track(&mut result_paths);
                            app.analysis_results.sort_by(|a, b| {
                                let ai = result_paths
                                    .iter()
                                    .position(|p| *p == a.path)
                                    .unwrap_or(usize::MAX);
                                let bi = result_paths
                                    .iter()
                                    .position(|p| *p == b.path)
                                    .unwrap_or(usize::MAX);
                                ai.cmp(&bi)
                            });
                        }
                        let count = app.analysis_results.len();
                        let last = &app.analysis_results[count - 1];
                        let name = last
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        app.set_status(format!(
                            "Analyzed: {} — DR{} ({})",
                            name,
                            last.dr_value,
                            super::analyze::dr_label(last.dr_value),
                        ));
                        app.active_overlay = super::app::ActiveOverlay::Analysis { scroll: 0 };
                    }
                }
                Err(e) => {
                    app.set_status(format!("Analysis failed: {}", e));
                    if app.analysis_pending == 0 && !app.analysis_results.is_empty() {
                        app.active_overlay = super::app::ActiveOverlay::Analysis { scroll: 0 };
                    }
                }
            }
        }
        AppMessage::PreemphasisComplete { result } => {
            let result = crate::tui::preemphasis::metadata_editor_safe_result(&result);
            app.preemph_pending = app.preemph_pending.saturating_sub(1);
            if let ActiveOverlay::MetadataEditor(ref mut state) = app.active_overlay {
                state.apply_preemphasis_result(&result);
            }
            if let Some(state) = app.pending_metadata_editor.as_mut() {
                state.apply_preemphasis_result(&result);
            }
            app.preemph_results.push(result);
            if app.preemph_pending == 0 {
                // Sort by path for consistent display.
                app.preemph_results.sort_by(|a, b| a.path.cmp(&b.path));
                let detected = app
                    .preemph_results
                    .iter()
                    .filter(|r| {
                        r.confidence == crate::tui::preemphasis::PreemphasisConfidence::Detected
                    })
                    .count();
                let candidates = app
                    .preemph_results
                    .iter()
                    .filter(|r| {
                        r.confidence == crate::tui::preemphasis::PreemphasisConfidence::StrongCandidate
                    })
                    .count();
                let total = app.preemph_results.len();
                if detected > 0 || candidates > 0 {
                    app.set_status(format!(
                        "Pre-emphasis: {} PRE flag, {} catalog candidate out of {} file(s)",
                        detected, candidates, total,
                    ));
                } else {
                    app.set_status(format!("Pre-emphasis: not detected in {} file(s)", total,));
                }
                app.active_overlay = super::app::ActiveOverlay::Preemphasis { scroll: 0 };
            }
        }
        AppMessage::CorpusTrainComplete { result } => match result {
            Ok((n_tracks, n_frames)) => {
                app.set_status(format!(
                    "Corpus trained: {} tracks, {} frames",
                    n_tracks, n_frames,
                ));
            }
            Err(e) => {
                app.set_status(format!("Corpus training failed: {}", e));
            }
        },
        AppMessage::CalibrationComplete { result } => {
            match result {
                Ok((n_pe, n_non_pe, accuracy, fpr, threshold)) => {
                    app.set_status(format!(
                        "Calibrated: {:.1}% accuracy, {:.1}% FPR, threshold={:.3} ({} PE + {} non-PE)",
                        accuracy * 100.0, fpr * 100.0, threshold, n_pe, n_non_pe,
                    ));
                }
                Err(e) => {
                    app.set_status(format!("Calibration failed: {}", e));
                }
            }
        }
        AppMessage::CompareComplete { result } => {
            app.compare_pending = app.compare_pending.saturating_sub(1);
            app.compare_results.push(result);
            if app.compare_pending == 0 {
                let identical = app.compare_results.iter().filter(|r| r.identical).count();
                let differ = app.compare_results.len() - identical;
                if differ == 0 {
                    app.set_status(format!("Compared {} pair(s): all bit-identical", identical,));
                } else {
                    app.set_status(format!(
                        "Compared {} pair(s): {} identical, {} differ",
                        identical + differ,
                        identical,
                        differ,
                    ));
                }
                app.active_overlay = super::app::ActiveOverlay::BitCompare { scroll: 0 };
                if !app.config.ui.compare_keep_reference {
                    app.compare_reference.clear();
                }
            }
        }
        AppMessage::VerifyComplete { result } => {
            app.verify_pending = app.verify_pending.saturating_sub(1);
            app.verify_results.push(result);
            if app.verify_pending == 0 {
                // Sort by disc/track for logical display order.
                {
                    let mut result_paths: Vec<std::path::PathBuf> =
                        app.verify_results.iter().map(|r| r.path.clone()).collect();
                    crate::tui::probe::sort_paths_by_track(&mut result_paths);
                    app.verify_results.sort_by(|a, b| {
                        let ai = result_paths
                            .iter()
                            .position(|p| *p == a.path)
                            .unwrap_or(usize::MAX);
                        let bi = result_paths
                            .iter()
                            .position(|p| *p == b.path)
                            .unwrap_or(usize::MAX);
                        ai.cmp(&bi)
                    });
                }
                let passed = app.verify_results.iter().filter(|r| r.passed).count();
                let failed = app.verify_results.len() - passed;
                if failed == 0 {
                    app.set_status(format!("Verified {} file(s): all passed", passed));
                } else {
                    app.set_status(format!(
                        "Verified {} file(s): {} passed, {} failed",
                        passed + failed,
                        passed,
                        failed,
                    ));
                }
                app.active_overlay = super::app::ActiveOverlay::Verify { scroll: 0 };
            }
        }
        AppMessage::PathValidationComplete { input, result } => match result {
            Ok(path) => {
                let display = path.display().to_string();
                app.browse.navigate_to(path);
                app.set_status(&format!("cd: {}", display));
            }
            Err(e) => {
                app.set_status(&format!(":cd {}: {}", input, e));
            }
        },
        AppMessage::DirScanComplete {
            path,
            parent_entry,
            mut dirs,
            files,
            error,
        } => {
            // Race protection: discard if user has navigated elsewhere.
            if app.browse.current_dir != path {
                return;
            }

            if let Some(err) = error {
                if err == "cancelled" {
                    // Don't clear scan_pending — a newer scan is in flight.
                    return;
                }
                app.browse.scan_pending = None;
                app.browse.error = Some(err);
                return;
            }

            // Success — clear the scan handle.
            app.browse.scan_pending = None;

            // Populate raw scan results. Classify DVD-Audio directories before publishing.
            app.browse.classify_scanned_directory_entries(&mut dirs);
            app.browse.parent_entry = parent_entry;
            app.browse.all_dirs = dirs;
            app.browse.all_files = files;

            // Upgrade `.iso` archives to SacdIso where ScarletBook
            // magic is found. Cheap on warm cache, sub-50ms cold.
            app.browse.upgrade_iso_kinds();

            // Apply current filter/sort.
            app.browse.apply_view();

            // Cursor restoration (e.g., after go_parent).
            if let Some(target) = app.browse.cursor_restore_target.take() {
                if let Some(idx) = app.browse.entries.iter().position(|e| e.name == target) {
                    app.browse.selected_index = idx;
                    app.browse.ensure_visible();
                }
            }

            // Probe the newly selected entry.
            app.browse.probe_current_with_db(tx, Some(&app.db));
            super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
        }
        AppMessage::MetadataWriteComplete {
            path,
            field,
            result,
        } => {
            // Step 3 (main thread): cleanup journal + backup, invalidate caches.
            let path_str = path.display().to_string();
            let backup = crate::db::Database::backup_path_for(&path);
            match result {
                Ok(()) => {
                    let _ = app.db.complete_metadata_write(&path_str);
                    let _ = std::fs::remove_file(&backup);
                    app.browse.probe_cache.remove(&path);
                    app.browse.probe_pending.remove(&path);
                    let _ = app.db.invalidate_probe(&path_str);
                    app.browse.probe_current_with_db(tx, Some(&app.db));
                    app.set_status(format!(
                        "{}: {} updated",
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        field.label(),
                    ));
                }
                Err(e) => {
                    // Rollback: restore from backup.
                    if backup.exists() {
                        let _ = std::fs::rename(&backup, &path);
                    }
                    let _ = app.db.complete_metadata_write(&path_str);
                    app.set_status(format!("write failed (rolled back): {}", e));
                }
            }
        }
        AppMessage::ArchiveListingComplete {
            archive_path,
            result,
            password,
        } => match *result {
            Ok(listing) => {
                let count = listing.entries.len();
                app.browse.enter_archive(listing, password);
                app.set_status(&format!(
                    "Opened {} ({} entries)",
                    archive_path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    count,
                ));
            }
            Err(e) => {
                app.set_status(&format!("Archive error: {}", e));
            }
        },
        AppMessage::SearchComplete { results } => {
            if !app.browse.search.active {
                return; // Search was closed while task was running.
            }
            app.browse.search.searching = false;

            let mut scored = results;
            super::browse::sort_search_results(
                &mut scored,
                app.browse.search.sort,
                app.browse.search.sort_dir,
            );

            let mut entries: Vec<super::browse::BrowseEntry> = Vec::new();
            if let Some(ref parent) = app.browse.parent_entry {
                entries.push(parent.clone());
            }
            entries.extend(scored.into_iter().map(|(e, _)| e));
            app.browse.entries = entries;
            app.browse.selected_index = 0;
            app.browse.scroll_offset = 0;
        }
        AppMessage::FilePickerComplete { target, path } => {
            reduce_file_picker_complete(app, target, path, tx);
        }
        AppMessage::MetadataEditorDetailsProbeComplete { session_id, generation, total, results } => {
            let mut reduced = false;
            if let ActiveOverlay::MetadataEditor(mut state) = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None) {
                if let Some(status) = state.apply_details_probe_results(session_id, generation, results) {
                    app.set_status(status);
                } else {
                    app.set_status(format!(
                        "metadata editor: ignored stale Details probe result for session {session_id} ({total} file{})",
                        if total == 1 { "" } else { "s" }
                    ));
                }
                app.active_overlay = ActiveOverlay::MetadataEditor(state);
                reduced = true;
            }
            if !reduced {
                app.set_status("metadata editor: Details probe finished after editor closed");
            }
        }
        AppMessage::MetadataEditorDetailsAnalysisComplete { session_id, generation, total, results } => {
            let mut reduced = false;
            if let ActiveOverlay::MetadataEditor(mut state) = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None) {
                if !state.complete_details_analysis(session_id, generation) {
                    app.set_status(format!(
                        "metadata editor: ignored stale Details analysis result for session {session_id} generation {generation}"
                    ));
                    app.active_overlay = ActiveOverlay::MetadataEditor(state);
                    reduced = true;
                } else {
                    let mut applied = 0usize;
                    let mut issue_count = 0usize;
                    let mut ignored = 0usize;
                    let mut store_errors = 0usize;

                    if let Some(surface) = state.surface_mut_for_session(session_id) {
                        for item in results {
                            issue_count = issue_count.saturating_add(item.issues.len());
                            let mut facts_to_store = None;
                            if let Some(file) = surface.technical_details.files.get_mut(item.index) {
                                if file.file_facts.path == item.path
                                    && file.file_facts.modified == item.modified
                                    && file.file_facts.file_size == item.file_size
                                {
                                    if item.facts.has_any_result() {
                                        file.merge_analysis_facts(&item.facts);
                                        applied = applied.saturating_add(1);
                                        // Store the already-merged file facts, not the worker's
                                        // partial detector output. This keeps cache writes
                                        // idempotent when one detector succeeds and another was
                                        // not attempted or failed non-fatally.
                                        facts_to_store = Some(file.analysis_facts.clone());
                                    }
                                } else {
                                    ignored = ignored.saturating_add(1);
                                }
                            } else {
                                ignored = ignored.saturating_add(1);
                            }

                            if let Some(facts_to_store) = facts_to_store {
                                if let (Some(modified), Some(size)) = (item.modified.clone(), item.file_size) {
                                    let mtime = crate::db::systemtime_to_unix(modified);
                                    if let Err(err) = app.db.store_metadata_analysis_facts(
                                        &item.path.display().to_string(),
                                        mtime,
                                        size,
                                        &facts_to_store,
                                    ) {
                                        store_errors = store_errors.saturating_add(1);
                                        log::error!("analysis facts cache store failed: {}", err);
                                    }
                                }

                                if item.facts.hdcd_detected == Some(true) {
                                    let updated_probe = app
                                        .browse
                                        .probe_cache
                                        .get(&item.path)
                                        .and_then(|entry| entry.as_ref())
                                        .map(|cached| {
                                            let mut info = (**cached).clone();
                                            info.metadata.hdcd_detail = item.facts.hdcd_detail.clone();
                                            info
                                        });
                                    if let Some(info) = updated_probe {
                                        app.browse.probe_cache.insert(
                                            item.path.clone(),
                                            Some(std::sync::Arc::new(info)),
                                        );
                                    }
                                }
                            }
                        }
                    } else {
                        ignored = ignored.saturating_add(total);
                    }

                    let mut parts = vec![format!(
                        "{} file{} updated",
                        applied,
                        if applied == 1 { "" } else { "s" }
                    )];
                    if issue_count > 0 {
                        parts.push(format!(
                            "{} issue{}",
                            issue_count,
                            if issue_count == 1 { "" } else { "s" }
                        ));
                    }
                    if ignored > 0 {
                        parts.push(format!(
                            "{} stale/unknown ignored",
                            ignored
                        ));
                    }
                    if store_errors > 0 {
                        parts.push(format!(
                            "{} cache write error{}",
                            store_errors,
                            if store_errors == 1 { "" } else { "s" }
                        ));
                    }
                    app.set_status(format!(
                        "metadata editor: Details analysis complete ({})",
                        parts.join(", ")
                    ));
                    app.active_overlay = ActiveOverlay::MetadataEditor(state);
                    reduced = true;
                }
            }
            if !reduced {
                app.set_status("metadata editor: Details analysis finished after editor closed");
            }
        }
        AppMessage::MetadataEditorReplayGainComplete { session_id, generation, mode, paths, result } => {
            let mut reduced = false;
            if let ActiveOverlay::MetadataEditor(mut state) = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None) {
                if !state.complete_replaygain_scan(session_id, generation) {
                    app.set_status(format!(
                        "metadata editor: ignored stale ReplayGain scan result for session {session_id} generation {generation}"
                    ));
                    app.active_overlay = ActiveOverlay::MetadataEditor(state);
                    reduced = true;
                } else {
                    match result {
                        Ok(metadata) => {
                            if let Some(surface) = state.surface_mut_for_session(session_id) {
                                metadata_editor_apply_replaygain_metadata(surface, &paths, &metadata);
                                for path in &paths {
                                    app.browse.probe_cache.remove(path);
                                    let _ = app.db.invalidate_probe(&path.display().to_string());
                                }
                                app.set_status(format!(
                                    "metadata editor: ReplayGain {} scan wrote {} file{}",
                                    mode.label(),
                                    paths.len(),
                                    if paths.len() == 1 { "" } else { "s" }
                                ));
                            } else {
                                app.set_status(format!(
                                    "metadata editor: ignored ReplayGain result for missing session {session_id}"
                                ));
                            }
                        }
                        Err(err) => {
                            app.set_status(format!("metadata editor: ReplayGain scan failed: {err}"));
                        }
                    }
                    app.active_overlay = ActiveOverlay::MetadataEditor(state);
                    reduced = true;
                }
            }
            if !reduced {
                app.set_status("metadata editor: ReplayGain scan finished after editor closed");
            }
        }
        AppMessage::MetadataEditorArtworkWriteComplete { session_id, generation, mode, paths, result } => {
            let mut reduced = false;
            if let ActiveOverlay::MetadataEditor(mut state) = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None) {
                if !state.complete_artwork_write(session_id, generation) {
                    app.set_status(format!(
                        "metadata editor: ignored stale {} result for session {session_id} generation {generation}",
                        mode.label()
                    ));
                    app.active_overlay = ActiveOverlay::MetadataEditor(state);
                    reduced = true;
                } else {
                    match result {
                        Ok(metadata) => {
                            if let Some(surface) = state.surface_mut_for_session(session_id) {
                                metadata_editor_apply_artwork_metadata(surface, &paths, &metadata);
                                for path in &paths {
                                    app.browse.probe_cache.remove(path);
                                    let _ = app.db.invalidate_probe(&path.display().to_string());
                                }
                                app.set_status(format!(
                                    "metadata editor: {} updated {} file{}",
                                    mode.label(),
                                    paths.len(),
                                    if paths.len() == 1 { "" } else { "s" }
                                ));
                            } else {
                                app.set_status(format!(
                                    "metadata editor: ignored {} result for missing session {session_id}",
                                    mode.label()
                                ));
                            }
                        }
                        Err(err) => {
                            app.set_status(format!("metadata editor: {} failed: {err}", mode.label()));
                        }
                    }
                    state.file_picker = None;
                    state.pending_artwork_type = None;
                    app.active_overlay = ActiveOverlay::MetadataEditor(state);
                    reduced = true;
                }
            }
            if !reduced {
                app.set_status("metadata editor: artwork update finished after editor closed");
            }
        }
        AppMessage::MetadataEditorWriteComplete {
            session_id,
            save_generation,
            results,
        } => {
            let mut reduced = false;
            if let ActiveOverlay::MetadataEditor(mut state) =
                std::mem::replace(&mut app.active_overlay, ActiveOverlay::None)
            {
                if let Some(summary) =
                    state.apply_write_results(session_id, save_generation, results)
                {
                    let close_editor = summary.all_saved();
                    for path in &summary.saved_paths {
                        app.browse.probe_cache.remove(path);
                        let _ = app.db.invalidate_probe(&path.display().to_string());
                    }
                    app.set_status(summary.status_line());
                    if close_editor {
                        app.browse.probe_current_with_db(tx, Some(&app.db));
                    } else {
                        state.phase = super::app::MetadataEditorPhase::Editing;
                        app.active_overlay = ActiveOverlay::MetadataEditor(state);
                    }
                } else {
                    app.set_status(format!(
                        "metadata editor: ignored stale save result for session {session_id} generation {save_generation}"
                    ));
                    app.active_overlay = ActiveOverlay::MetadataEditor(state);
                }
                reduced = true;
            }
            if !reduced {
                app.set_status(
                    "metadata editor: save finished after editor closed; stale result ignored",
                );
            }
        }
        AppMessage::GnudbQueryComplete { result, paths } => {
            match result {
                Ok(matches) if matches.len() == 1 => {
                    // Single match: auto-read the entry.
                    let m = matches[0].clone();
                    app.set_status(format!("GNUDB: found {} — reading...", m.title));
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let result = super::gnudb::read_gnudb(&m.category, &m.disc_id).await;
                        let _ = tx
                            .send(AppMessage::GnudbReadComplete {
                                result,
                                paths,
                                origin_matches: None,
                            })
                            .await;
                    });
                }
                Ok(matches) if matches.is_empty() => {
                    app.set_status("GNUDB: no matches found");
                }
                Ok(matches) => {
                    // Multiple matches: show selection overlay.
                    app.set_status(format!("GNUDB: {} matches found", matches.len()));
                    app.active_overlay = ActiveOverlay::GnudbSelect {
                        matches,
                        selected: 0,
                        scroll: 0,
                        paths,
                    };
                }
                Err(e) => {
                    app.set_status(format!("GNUDB error: {}", e));
                }
            }
        }
        AppMessage::GnudbReadComplete {
            result,
            paths,
            origin_matches,
        } => {
            match result {
                Ok(entry) => {
                    // Open GNUDB review overlay for user editing before accept.
                    let mut review = super::gnudb::build_review_state(&entry, paths);
                    review.origin_matches = origin_matches;
                    app.set_status(format!(
                        "GNUDB: {} / {} ({} tracks) — review and edit",
                        entry.artist,
                        entry.album,
                        entry.tracks.len(),
                    ));
                    app.active_overlay = ActiveOverlay::GnudbReview(Box::new(review));
                }
                Err(e) => {
                    app.set_status(format!("GNUDB read error: {}", e));
                }
            }
        }
        AppMessage::GnudbMultiDiscComplete { entries } => {
            if entries.is_empty() {
                app.set_status("GNUDB: no matches found for any disc");
                return;
            }

            // Open GNUDB review overlay with multi-disc data.
            let review = super::gnudb::build_multi_disc_review_state(&entries);
            let n_discs = entries.len();
            let n_tracks: usize = entries.iter().map(|(_, e, _)| e.tracks.len()).sum();
            app.set_status(format!(
                "GNUDB: {} disc{}, {} tracks — review and edit",
                n_discs,
                if n_discs == 1 { "" } else { "s" },
                n_tracks,
            ));
            app.active_overlay = ActiveOverlay::GnudbReview(Box::new(review));
        }

        AppMessage::CtdbComplete { mut pages } => {
            // Drain freshly computed parity matrices into the cache before
            // the pages move into the long-lived overlay state. Each
            // `parity_cache_write` carries (cache_key, ~376 KB matrix);
            // taking it ensures the matrix is dropped after the DB write
            // and never propagates through CtdbVerifyState clones.
            for page in pages.iter_mut() {
                if let Some((key, parity)) = page.result.parity_cache_write.take() {
                    if let Err(e) = app.db.store_ctdb_parity(&key, 16, &parity) {
                        log::warn!("CTDB parity cache store failed: {}", e);
                    }
                }
            }
            if pages.len() == 1 {
                let summary = crate::tui::ctdb::format_ctdb_summary(&pages[0].result);
                app.set_status(format!("CUETools DB: {}", summary));
            } else {
                let total: usize = pages.iter().map(|p| p.result.tracks.len()).sum();
                // Both byte-exact `Verified` and RS-equivalent `VerifiedRs` count
                // as verified, consistent with format_ctdb_summary.
                let verified: usize = pages
                    .iter()
                    .map(|p| {
                        p.result
                            .tracks
                            .iter()
                            .filter(|t| {
                                matches!(
                                    t.status,
                                    crate::tui::ctdb::CtdbTrackStatus::Verified
                                        | crate::tui::ctdb::CtdbTrackStatus::VerifiedRs
                                )
                            })
                            .count()
                    })
                    .sum();
                app.set_status(format!(
                    "CUETools DB: {} discs, {}/{} tracks verified",
                    pages.len(),
                    verified,
                    total,
                ));
            }

            // If a context-menu / direct :ctdb-repair invoked us, find the
            // first page that actually has something to repair (mismatches
            // AND parity) and start the overlay there so the subsequent
            // :ctdb-repair re-dispatch operates on a repairable disc.
            let auto_repair = std::mem::replace(&mut app.auto_repair_on_ctdb_complete, false);
            let active_page = if auto_repair {
                pages
                    .iter()
                    .position(|p| {
                        p.result.parity_url.is_some()
                            && p.result
                                .tracks
                                .iter()
                                .any(|t| t.status == crate::tui::ctdb::CtdbTrackStatus::Mismatch)
                    })
                    .unwrap_or(0)
            } else {
                0
            };

            app.active_overlay =
                ActiveOverlay::CtdbVerify(Box::new(crate::tui::app::CtdbVerifyState {
                    pages,
                    active_page,
                    scroll: 0,
                }));

            if auto_repair {
                // Re-enter Command::CtdbRepair now that the overlay is up.
                // The handler will validate parity/mismatches/CRCs and
                // either pop the confirmation dialog, defer to AR, or
                // emit a status message ("No mismatches detected", etc.).
                super::command::execute_command(app, super::command::Command::CtdbRepair, tx);
            }
        }
        AppMessage::ArBatchComplete { result } => {
            let total = result.albums.len();
            let verified = result
                .albums
                .iter()
                .filter(|a| a.verified == a.total_tracks && a.total_tracks > 0 && !a.not_in_db)
                .count();
            let report_msg = result
                .report_path
                .as_ref()
                .map(|p| format!(" — report: {}", p.display()))
                .unwrap_or_default();
            app.set_status(format!(
                "Batch AR: {}/{} albums verified{}",
                verified, total, report_msg,
            ));
            app.active_overlay = ActiveOverlay::ArBatchReport { result, scroll: 0 };
        }
        AppMessage::OffsetCorrectionComplete { result } => {
            match result {
                Ok(summary) => {
                    app.set_status(summary);
                    app.active_overlay = ActiveOverlay::None;
                    // Refresh browse listing since files were replaced.
                    app.browse.refresh();
                }
                Err(e) => {
                    app.set_status(format!("Offset correction failed: {}", e));
                }
            }
        }
        AppMessage::CtdbRepairComplete { result } => match result {
            Ok(summary) => {
                app.set_status(summary);
                app.active_overlay = ActiveOverlay::None;
                app.browse.refresh();
            }
            Err(e) => {
                app.set_status(format!("CTDB repair failed: {}", e));
            }
        },
        AppMessage::CueMbComplete {
            outcome,
            paths,
            output_dir,
            single_image,
            toc_string,
        } => {
            handle_cue_mb_complete(
                app,
                tx,
                outcome,
                paths,
                output_dir,
                single_image,
                toc_string,
            );
        }
        AppMessage::CueFillComplete {
            outcome,
            cue_path,
            album,
            tracks,
            layout,
            toc_string,
        } => {
            handle_cue_fill_complete(
                app, tx, outcome, cue_path, *album, tracks, layout, toc_string,
            );
        }
        AppMessage::TagsFromMbComplete { outcome, ctx } => {
            dispatch_tags_from_mb_complete(app, tx, outcome, ctx);
        }
        AppMessage::MbDetailPrefetchComplete { release_id, result } => {
            // Stamp the in-memory cache if the picker is still open,
            // and persist the raw body to SQLite (Phase B-5) so future
            // sessions skip the HTTP call. Cache-writes happen even
            // when the picker has been dismissed — the response was
            // already paid for and a re-open will benefit. Errors and
            // `release: None` (HTTP 404) are silent (best-effort
            // prefetch).
            if let Ok(outcome) = result {
                if let Some((key, body)) = outcome.cache_write {
                    if let Err(e) = app.db.store_mb_search(&key, &body) {
                        log::warn!("mb search cache store failed: {}", e);
                    }
                }
                if let Some(release) = outcome.release {
                    if let ActiveOverlay::MbSelect(ref mut state) = app.active_overlay {
                        state.prefetch.insert(release_id, release);
                    }
                }
            }
        }
        AppMessage::AccurateRipComplete { pages } => {
            // Aggregate summary across all discs.
            let total: usize = pages.iter().map(|p| p.result.tracks.len()).sum();
            let verified: usize = pages
                .iter()
                .map(|p| {
                    p.result
                        .tracks
                        .iter()
                        .filter(|t| t.status == crate::tui::accuraterip::ArTrackStatus::Verified)
                        .count()
                })
                .sum();
            if pages.len() == 1 {
                let summary = crate::tui::accuraterip::format_summary(&pages[0].result);
                app.set_status(format!("AccurateRip: {}", summary));
            } else {
                app.set_status(format!(
                    "AccurateRip: {} discs, {}/{} tracks verified",
                    pages.len(),
                    verified,
                    total,
                ));
            }
            // Cache AR results per track (each track keyed by its own path).
            for page in &pages {
                for t in &page.result.tracks {
                    if let Ok(meta) = std::fs::metadata(&t.path) {
                        let mtime = meta
                            .modified()
                            .map(crate::db::systemtime_to_unix)
                            .unwrap_or(0);
                        if let Err(e) = app.db.store_ar(
                            &t.path.display().to_string(),
                            mtime,
                            meta.len(),
                            std::slice::from_ref(t),
                            &page.result.disc_id_str,
                        ) {
                            log::error!("AR cache store failed: {}", e);
                        }
                    }
                }
            }

            // If a CTDB repair was deferred awaiting AR offset detection,
            // and these AR results target the same disc, resolve the offset
            // and open the repair confirmation dialog. Takes priority over
            // auto_fix_on_complete when matched.
            //
            // Match the AR page whose first track path equals the pending
            // repair's first path. If no match, leave pending intact —
            // these AR results are unrelated (e.g. the user ran `:ar`
            // manually for a different selection while the deferred repair
            // was still waiting on its own AR run); pending will be
            // consumed when its own AR run completes.
            let matched_page_idx = app
                .pending_ctdb_repair
                .as_ref()
                .and_then(|p| p.paths.first().cloned())
                .and_then(|target| {
                    pages
                        .iter()
                        .position(|p| p.result.tracks.first().map(|t| &t.path) == Some(&target))
                });
            if let Some(idx) = matched_page_idx {
                let pending = app.pending_ctdb_repair.take().unwrap();
                let page = &pages[idx];

                // Extract a uniform offset from the AR result.
                // Unlike `detect_uniform_offset`, we DO accept offset 0
                // as a verified value (AR confirmed offset 0 is a valid
                // result for our use case — we just need the right
                // offset for the RS repair, not necessarily a non-zero
                // one). Returns None only when the AR data is
                // inconclusive (mixed offsets, unverified tracks,
                // disc not in DB).
                let resolved_offset: Option<i32> = {
                    let tracks = &page.result.tracks;
                    if tracks.is_empty() {
                        None
                    } else {
                        let mut common: Option<i32> = None;
                        let mut all_ok = true;
                        for t in tracks {
                            if t.status != crate::tui::accuraterip::ArTrackStatus::Verified {
                                all_ok = false;
                                break;
                            }
                            let off = match t.offset {
                                Some(o) => o,
                                None => {
                                    all_ok = false;
                                    break;
                                }
                            };
                            match common {
                                Some(prev) if prev != off => {
                                    all_ok = false;
                                    break;
                                }
                                None => common = Some(off),
                                _ => {}
                            }
                        }
                        if all_ok {
                            common
                        } else {
                            None
                        }
                    }
                };

                let (offset, offset_note) = match resolved_offset {
                    Some(n) => (n, format!("offset: {:+} samples (from AR verification)", n)),
                    None => (
                        0,
                        "offset: +0 (AR could not determine a drive offset — \
                                 proceeding at +0 may produce incorrect repairs if \
                                 your drive has a real read offset)"
                            .to_string(),
                    ),
                };

                let n_tracks = pending.paths.len();
                let message = format!(
                    "Apply CTDB Reed-Solomon repair to {} tracks?\n\
                     Parity: {} symbols, {}\n\
                     Files will be re-encoded and verified before replacing originals.",
                    n_tracks, pending.npar, offset_note,
                );

                let action = match pending.single_image {
                    Some(info) => crate::tui::app::ConfirmAction::CtdbRepairSingleImage {
                        info,
                        parity_url: pending.parity_url,
                        npar: pending.npar,
                        offset,
                        expected_crcs: pending.expected_crcs,
                    },
                    None => crate::tui::app::ConfirmAction::CtdbRepair {
                        paths: pending.paths,
                        parity_url: pending.parity_url,
                        npar: pending.npar,
                        offset,
                        expected_crcs: pending.expected_crcs,
                    },
                };

                app.active_overlay = ActiveOverlay::Confirmation { message, action };
                return;
            }

            // If auto-fix was requested (context menu "Fix offset"),
            // check for a fixable offset and go straight to confirmation.
            if app.auto_fix_on_complete {
                app.auto_fix_on_complete = false;
                for page in &pages {
                    if let Some(offset) =
                        crate::tui::accuraterip::detect_uniform_offset(&page.result)
                    {
                        let paths: Vec<std::path::PathBuf> =
                            page.result.tracks.iter().map(|t| t.path.clone()).collect();
                        let n = paths.len();
                        app.active_overlay = ActiveOverlay::Confirmation {
                            message: format!(
                                "Apply offset correction ({:+} samples) to {} tracks?\n\
                                 Files will be re-encoded to FLAC and verified at offset +0\n\
                                 before replacing originals.",
                                offset, n,
                            ),
                            action: crate::tui::app::ConfirmAction::OffsetCorrection {
                                paths,
                                offset,
                            },
                        };
                        return;
                    }
                }
                // No fixable offset — show results normally.
                app.set_status(
                    "No offset correction needed — showing verification results".to_string(),
                );
            }

            app.active_overlay =
                ActiveOverlay::AccurateRipVerify(Box::new(crate::tui::app::ArVerifyState {
                    pages,
                    active_page: 0,
                    scroll: 0,
                }));
        }
    }
}

/// Handle a bracketed paste event. When the BulkRename overlay is active,
/// multi-line paste replaces the template-derived targets line-by-line.
/// In text input overlays, the pasted text is inserted at the cursor.
fn handle_paste(app: &mut AppState, text: &str) {
    match &app.active_overlay {
        ActiveOverlay::BulkRename(_) => {
            let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
            if let ActiveOverlay::BulkRename(mut state) = overlay {
                if state.focus == super::app::BulkRenameFocus::Template {
                    // Template focus: insert the first line into the template
                    // input (single-line field), then rebuild the plan.
                    let first_line = text.lines().next().unwrap_or("");
                    for c in first_line.chars() {
                        state.template_input.insert_char(c);
                    }
                    state.rebuild_plan();
                } else {
                    // List focus: replace ops line-by-line with pasted names.
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
                        "Pasted {} name{}",
                        applied,
                        if applied == 1 { "" } else { "s" }
                    ));
                }
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
                ActiveOverlay::TextEdit {
                    mut input,
                    target,
                    label,
                } => {
                    for c in first_line.chars() {
                        input.insert_char(c);
                    }
                    app.active_overlay = ActiveOverlay::TextEdit {
                        input,
                        target,
                        label,
                    };
                }
                ActiveOverlay::CommandInput { mut input, .. } => {
                    for c in first_line.chars() {
                        input.insert_char(c);
                    }
                    // Clear completion — pasted text invalidates candidates.
                    app.active_overlay = ActiveOverlay::CommandInput {
                        input,
                        completion: None,
                    };
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
        ActiveOverlay::MetadataEditor(_) => {
            let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
            if let ActiveOverlay::MetadataEditor(mut state) = overlay {
                use super::app::MetadataEditorPhase;
                if state.phase == MetadataEditorPhase::DetailEdit {
                    let field_idx = state.detail_field_idx;
                    if field_idx < state.active_surface().entries.len() {
                        let sanitized = text.replace("\r\n", "\n").replace('\r', "\n");
                        let lines: Vec<&str> = sanitized.split('\n').collect();
                        let n_files = state.active_surface().paths.len();

                        // Cancel any active inline edit before taking the
                        // mutable entry borrow (avoids double-borrow of state).
                        state.detail_edit = None;

                        let entry = &mut state.active_surface_mut().entries[field_idx];
                        let is_album = entry.display_key.eq_ignore_ascii_case("ALBUM");

                        if is_album {
                            let val = lines
                                .first()
                                .map(|l| l.trim().to_string())
                                .unwrap_or_default();
                            for v in &mut entry.per_file_values {
                                *v = val.clone();
                            }
                        } else {
                            for (i, line) in lines.iter().enumerate() {
                                if i >= n_files {
                                    break;
                                }
                                entry.per_file_values[i] = line.trim().to_string();
                            }
                        }

                        // Update merged display value + mixed state.
                        let all_same = entry.per_file_values.windows(2).all(|w| w[0] == w[1]);
                        entry.is_mixed = !all_same && n_files > 1;
                        entry.value = if entry.is_mixed {
                            "<multiple values>".to_string()
                        } else {
                            entry.per_file_values.first().cloned().unwrap_or_default()
                        };

                        state.active_surface_mut().dirty = true;
                        let applied = lines.len().min(n_files);
                        app.set_status(format!(
                            "Pasted {} value{}",
                            applied,
                            if applied == 1 { "" } else { "s" },
                        ));
                    }
                } else if state.phase == MetadataEditorPhase::InlineEdit {
                    // Single-field inline edit: insert first line at cursor.
                    if let Some(ref mut input) = state.edit_input {
                        let first_line = text.lines().next().unwrap_or("");
                        for c in first_line.chars() {
                            input.insert_char(c);
                        }
                    }
                }
                app.active_overlay = ActiveOverlay::MetadataEditor(state);
            }
        }
        _ => {
            // Paste ignored outside text-entry contexts.
        }
    }
}

/// Handle the result of a `:cue-mb` MusicBrainz lookup. Caches the response,
/// builds a CUE from MB-overridden tag/probe data, and writes it to disk.
fn handle_cue_mb_complete(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    outcome: Result<super::musicbrainz::MbLookupOutcome, String>,
    paths: Vec<std::path::PathBuf>,
    output_dir: std::path::PathBuf,
    single_image: bool,
    toc_string: String,
) {
    let outcome = match outcome {
        Ok(o) => o,
        Err(e) => {
            app.set_status(format!("MusicBrainz CUE lookup failed: {}", e));
            return;
        }
    };

    // Cache the response (positive or negative) so retries don't re-hit MB.
    if let Some(json) = outcome.cache_response.as_deref() {
        if let Err(e) = app.db.store_mb_response(&toc_string, json) {
            log::warn!("MB cache store failed: {}", e);
        }
    }

    let release = match outcome.releases.into_iter().next() {
        Some(r) => r,
        None => {
            app.set_status("MusicBrainz CUE: no release matched this disc TOC".to_string());
            return;
        }
    };

    let (mut album, mut tracks) = match super::cue_generate::gather_cue_info(&paths, &output_dir) {
        Ok(pair) => pair,
        Err(e) => {
            app.set_status(format!("MusicBrainz CUE: {}", e));
            return;
        }
    };

    super::cue_generate::apply_mb_overrides(&mut album, &mut tracks, &release);

    let cue_content = if single_image {
        let image_name = super::cue_generate::derive_image_filename(&album, &paths[0]);
        let ext = paths[0]
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("flac");
        let fmt = super::cue_generate::cue_format_tag(ext);
        super::cue_generate::generate_single_image_cue(&album, &tracks, &image_name, fmt)
    } else {
        super::cue_generate::generate_multifile_cue(&album, &tracks)
    };

    let cue_filename = super::cue_generate::cue_output_filename(&album);
    let cue_path = output_dir.join(&cue_filename);

    let mode = if single_image {
        "single image"
    } else {
        "multi-file"
    };
    let pregaps = tracks.iter().filter(|t| t.pregap_frames.is_some()).count();
    let pregap_note = if pregaps > 0 {
        format!(
            ", {} pregap{}",
            pregaps,
            if pregaps == 1 { "" } else { "s" }
        )
    } else {
        String::new()
    };
    let summary = format!(
        "MusicBrainz CUE ({}, MB-enriched: \"{}\"{})",
        mode, album.title, pregap_note,
    );
    let _ = tx; // overlay handles save; nothing to dispatch here.
    app.active_overlay = ActiveOverlay::CuePreview(Box::new(super::app::CuePreviewState::new(
        cue_content,
        cue_path,
        summary.clone(),
    )));
    app.set_status(summary);
}

/// Dispatch the `AppMessage::TagsFromMbComplete` arm from the real event-loop
/// message matcher into the unified MusicBrainz completion handler.
///
/// Keeping this as a named production call site makes the async channel path
/// explicit: `run_app` drains `rx`, `handle_message` matches
/// `AppMessage::TagsFromMbComplete`, and this function hands the payload to
/// `handle_tags_from_mb_complete`, whose success path eventually reopens the
/// editor through the apply-to-all presentation prompt.
fn dispatch_tags_from_mb_complete(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    outcome: super::message::MbOutcome,
    ctx: super::message::TagsMbContext,
) {
    handle_tags_from_mb_complete(app, tx, outcome, ctx);
}

/// Unified `:tags-mb` result handler. Routes both TOC (primary) and
/// Search (fallback) outcomes through the same 0/1/N branching with
/// behaviour parameterised by `TagsMbContext`:
///
/// - `editor_park = true` → an editor sits in `active_overlay` and
///   should be populated in place. The multi-match branch parks it
///   in `pending_metadata_editor` so `MbSelect` cancel paths can
///   restore it. `open_editor_with_mb_release` handles the
///   take-from-active or take-from-pending mechanics for the
///   single-match branch.
/// - `editor_park = false` → no editor in scope (Browse path). The
///   single-match branch falls through to `open_editor_with_mb_release`'s
///   "build fresh from `paths`" code path. The multi-match branch
///   opens `MbSelect` without parking.
///
/// On TOC zero-match with `fallback_seed = Some(...)`, the handler
/// spawns a `search_releases_by_query` and re-enters as
/// `MbOutcome::Search`. Re-entry is single-depth: the Search arm
/// never re-spawns.
fn handle_tags_from_mb_complete(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    outcome: super::message::MbOutcome,
    ctx: super::message::TagsMbContext,
) {
    use super::message::MbOutcome;
    match outcome {
        MbOutcome::Toc {
            outcome,
            toc_string,
        } => {
            handle_mb_toc_outcome(app, tx, outcome, toc_string, ctx);
        }
        MbOutcome::Search {
            outcome,
            query_label,
        } => {
            handle_mb_search_outcome(app, tx, outcome, query_label, ctx);
        }
    }
}

fn handle_mb_toc_outcome(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    outcome: Result<super::musicbrainz::MbLookupOutcome, String>,
    toc_string: String,
    ctx: super::message::TagsMbContext,
) {
    let outcome = match outcome {
        Ok(o) => o,
        Err(e) => {
            app.set_status(format!(":tags-mb: TOC lookup failed: {}", e));
            return;
        }
    };

    if let Some(json) = outcome.cache_response.as_deref() {
        if let Err(e) = app.db.store_mb_response(&toc_string, json) {
            log::warn!("MB TOC cache store failed: {}", e);
        }
    }

    match outcome.releases.len() {
        0 => match ctx.fallback_seed.clone() {
            Some(seed) => {
                spawn_tags_mb_text_search(app, tx, seed, ctx, TextSearchMode::TocFallback)
            }
            None => {
                app.set_status(
                    ":tags-mb: no MusicBrainz release matched this disc TOC".to_string(),
                );
            }
        },
        1 => {
            open_editor_with_mb_release(app, outcome.releases, 0, ctx.paths);
        }
        n => {
            open_mb_select_picker(app, tx, outcome.releases, ctx, n, None);
        }
    }
}

fn handle_mb_search_outcome(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    outcome: Result<super::musicbrainz::MbSearchOutcome, String>,
    query_label: String,
    ctx: super::message::TagsMbContext,
) {
    let outcome = match outcome {
        Ok(o) => o,
        Err(e) => {
            app.set_status(format!(":tags-mb: search failed: {}", e));
            return;
        }
    };

    for (key, body) in &outcome.cache_writes {
        if let Err(e) = app.db.store_mb_search(key, body) {
            log::warn!("mb search cache store failed: {}", e);
        }
    }

    match outcome.releases.len() {
        0 => {
            // Reaching the Search arm implies a prior TOC zero-match
            // (only the fallback path constructs `MbOutcome::Search`
            // today), so the zero status names both attempts.
            app.set_status(format!(
                ":tags-mb: no MB release matched the disc TOC or text \
                 search for \"{}\"",
                query_label,
            ));
        }
        1 => {
            open_editor_with_mb_release(app, outcome.releases, 0, ctx.paths);
        }
        n => {
            open_mb_select_picker(app, tx, outcome.releases, ctx, n, Some(query_label));
        }
    }
}

/// Why a `spawn_tags_mb_text_search` was fired. Drives the pre-spawn
/// status text: `TocFallback` keeps the "TOC missed" breadcrumb,
/// `DirectRequest` just names the search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TextSearchMode {
    /// User typed `:tags-mb …` with explicit args (Phase C item 2a).
    /// No prior TOC attempt; status just names the search.
    DirectRequest,
    /// DVD-Video editor durations were absent, zero, or invalid, so the
    /// command skipped synthetic TOC lookup and used the editor seed directly.
    DvdvTocSkippedInvalidDurations,
    /// Spawned by the TOC handler's zero-match branch (C-2b). Status
    /// keeps the "TOC missed" breadcrumb so the user sees the chain.
    TocFallback,
}

/// Spawn the `:tags-mb` text/release search. Two callers:
///
/// 1. The TOC handler's zero-match branch (`mode = TocFallback`),
///    using a seed extracted from the editor's ARTIST/ALBUM/etc. rows.
/// 2. The command dispatch in `command.rs` (`mode = DirectRequest`),
///    using a seed the user supplied via `:tags-mb` flags + text.
///
/// Builds the in-memory cache lookup for both candidate query forms
/// (with-catno + without-catno fallback inside
/// `search_releases_by_query`), sets the pre-spawn status, and fires
/// the async search. Result re-enters the unified handler as
/// `MbOutcome::Search`.
pub(super) fn spawn_tags_mb_text_search(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    seed: super::command::SacdMbSeed,
    ctx: super::message::TagsMbContext,
    mode: TextSearchMode,
) {
    let super::command::SacdMbSeed {
        artist,
        album,
        catalog,
        year,
    } = seed;
    let n_tracks = ctx.paths.len();

    let mut cached: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let key_with =
        super::musicbrainz::search_cache_key(&artist, &album, catalog.as_deref(), year.as_deref());
    if let Some(b) = app.db.get_cached_mb_search(&key_with) {
        cached.insert(key_with, b);
    }
    if catalog.is_some() {
        let key_without =
            super::musicbrainz::search_cache_key(&artist, &album, None, year.as_deref());
        if let Some(b) = app.db.get_cached_mb_search(&key_without) {
            cached.insert(key_without, b);
        }
    }
    let cache_hit = !cached.is_empty();
    let label = match (artist.is_empty(), album.is_empty()) {
        (false, false) => format!("{} / {}", artist, album),
        (false, true) => artist.clone(),
        (true, false) => album.clone(),
        (true, true) => catalog
            .clone()
            .unwrap_or_else(|| year.clone().unwrap_or_default()),
    };
    let status = match mode {
        TextSearchMode::DirectRequest => format!(
            ":tags-mb: {} search for \"{}\"…",
            if cache_hit { "cached" } else { "running" },
            label,
        ),
        TextSearchMode::DvdvTocSkippedInvalidDurations => format!(
            ":tags-mb: DVD-Video TOC skipped: chapter durations are missing or invalid; {} text search for \"{}\"…",
            if cache_hit { "cached" } else { "running" },
            label,
        ),
        TextSearchMode::TocFallback => format!(
            ":tags-mb: TOC missed, {} text search for \"{}\"…",
            if cache_hit { "cached" } else { "trying" },
            label,
        ),
    };
    app.set_status(status);

    let tx_inner = tx.clone();
    let label_for_msg = label;
    let ctx_for_msg = ctx;

    tokio::spawn(async move {
        let outcome = super::musicbrainz::search_releases_by_query(
            &artist,
            &album,
            catalog.as_deref(),
            year.as_deref(),
            n_tracks,
            cached,
        )
        .await;
        let _ = tx_inner
            .send(AppMessage::TagsFromMbComplete {
                outcome: super::message::MbOutcome::Search {
                    outcome,
                    query_label: label_for_msg,
                },
                ctx: ctx_for_msg,
            })
            .await;
    });
}

/// Open the `MbSelect` picker on N matches. Parks the editor (when
/// one is in scope per `ctx.editor_park`) so cancel paths can restore
/// it. `query_label` is `Some` when called from the Search arm so
/// the status names the search; `None` for TOC matches so the status
/// names the disc geometry.
fn open_mb_select_picker(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    releases: Vec<super::musicbrainz::MbRelease>,
    ctx: super::message::TagsMbContext,
    n: usize,
    query_label: Option<String>,
) {
    if ctx.editor_park {
        let Some(state_owned) = take_metadata_editor(app) else {
            let detail = match &query_label {
                Some(l) => format!("rerun to apply \"{}\" ({} matches)", l, n),
                None => format!("rerun ({} matches)", n),
            };
            app.set_status(format!(":tags-mb: editor closed during lookup; {}", detail));
            return;
        };
        app.pending_metadata_editor = Some(state_owned);
    }
    let status = match query_label {
        Some(l) => format!(
            ":tags-mb: {} releases matched \"{}\" — pick one (Enter / Esc / arrows)",
            n, l,
        ),
        None => format!(
            ":tags-mb: {} releases matched the disc TOC — pick one (Enter / Esc / arrows)",
            n,
        ),
    };
    app.set_status(status);
    let state = super::app::MbSelectState::new(releases, ctx.paths);
    if let Some(top) = state.releases.first() {
        let cached_body = app
            .db
            .get_cached_mb_search(&super::musicbrainz::detail_cache_key(&top.release_id));
        spawn_mb_detail_prefetch(
            tx.clone(),
            top.release_id.clone(),
            state.paths.len(),
            std::sync::Arc::clone(&state.generation),
            state.generation.load(std::sync::atomic::Ordering::Relaxed),
            cached_body,
        );
    }
    app.active_overlay = ActiveOverlay::MbSelect(Box::new(state));
}

/// Pop a parked metadata editor back into `active_overlay`. No-op
/// when no editor was parked. Used by every `MbSelect` cancel path
/// (Esc, click-outside, cancel pill, context-menu cancel) so the
/// user lands back on the editor they came from instead of a blank
/// screen.
pub(super) fn restore_parked_editor(app: &mut AppState) {
    if let Some(parked) = app.pending_metadata_editor.take() {
        app.active_overlay = ActiveOverlay::MetadataEditor(parked);
    }
}

/// Take the metadata editor from wherever it's currently living —
/// either parked in `pending_metadata_editor` or sitting as the
/// `active_overlay`. Returns `None` when neither slot holds an editor
/// (e.g., the user closed it during an async wait).
///
/// Pending is checked first because the existing TOC `:mb-back` and
/// related back-nav flows leave the editor parked there. The SACD
/// `:tags-mb` flow (Phase C-2) deliberately leaves it in `active`
/// during the async search so the surrounding command-input /
/// context-menu auto-restore wrappers see `active != None` and
/// don't drain the parking slot before our async result arrives.
pub(super) fn take_metadata_editor(
    app: &mut AppState,
) -> Option<Box<super::app::MetadataEditorState>> {
    if let Some(parked) = app.pending_metadata_editor.take() {
        return Some(parked);
    }
    if matches!(app.active_overlay, ActiveOverlay::MetadataEditor(_)) {
        if let ActiveOverlay::MetadataEditor(s) =
            std::mem::replace(&mut app.active_overlay, ActiveOverlay::None)
        {
            return Some(s);
        }
    }
    None
}

/// Spawn a Phase B-4 prefetch task for an MbSelect candidate. The task
/// sleeps for the debounce window (150 ms), re-checks the picker's
/// generation atomic — bailing out if the user has moved cursor — and
/// only then fires `fetch_release_detail` and the rate-limited MB
/// request. Result is delivered via `AppMessage::MbDetailPrefetchComplete`;
/// the handler stamps onto `MbSelectState.prefetch` and persists the
/// raw response to SQLite (Phase B-5).
///
/// When `cached_body` is `Some`, the SQLite cache (Phase B-5) already
/// has this MBID and we skip both the debounce sleep and any HTTP
/// call — the parse happens immediately and the message fires on the
/// next runtime tick. The generation check is skipped on the cached
/// path too: a "stale" cache hit can't waste an MB token, and the
/// handler always stamps `state.prefetch` regardless, so a hit that
/// lands after the user moved still benefits a later re-cursor.
///
/// Pass `release_id` empty to skip — callers shouldn't generally do
/// this but the guard is cheap.
pub(super) fn spawn_mb_detail_prefetch(
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    release_id: String,
    n_tracks: usize,
    generation_arc: std::sync::Arc<std::sync::atomic::AtomicU64>,
    snapshot: u64,
    cached_body: Option<String>,
) {
    if release_id.is_empty() {
        return;
    }
    let has_cache = cached_body.is_some();
    tokio::spawn(async move {
        if !has_cache {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            if generation_arc.load(std::sync::atomic::Ordering::Relaxed) != snapshot {
                // User moved cursor during the debounce window. Drop the
                // request before consuming an MB rate-limit token.
                return;
            }
        }
        let result =
            super::musicbrainz::fetch_release_detail(&release_id, n_tracks, cached_body).await;
        let _ = tx
            .send(crate::tui::message::AppMessage::MbDetailPrefetchComplete { release_id, result })
            .await;
    });
}

/// Open the metadata editor on the supplied `paths`, populated with the
/// chosen MusicBrainz release (`releases[selected]`). Skips the
/// GnudbReview "preview" step since the editor surfaces the same
/// fields plus more (and now supports per-row revert via the
/// proposed-value tracking on TagEntry).
///
/// When `releases.len() > 1`, the full list + paths + selected index
/// are cached on `MetadataEditorState::mb_back` so the user can run
/// `:mb-back` to return to the picker without re-querying MB.
///
/// Used by both single-match `:tags-mb` (releases.len() == 1) and
/// the post-MbSelect commit path (releases.len() >= 1, with selected
/// being whatever the user picked).
pub(super) fn open_editor_with_mb_release(
    app: &mut AppState,
    releases: Vec<super::musicbrainz::MbRelease>,
    selected: usize,
    paths: Vec<std::path::PathBuf>,
) {
    let Some(release) = releases.get(selected) else {
        app.set_status(":tags-mb: invalid release index".to_string());
        return;
    };

    // Three arrival modes, checked via `take_metadata_editor`:
    // - Browse → MbSelect: no editor was open before `:tags-mb`,
    //   neither slot holds one; `open_metadata_editor` builds fresh
    //   from the selection.
    // - Editor → MbSelect (multi-match SACD path): the source editor
    //   was parked in `pending_metadata_editor` when MbSelect opened.
    // - Editor (SACD single-match path): the editor is sitting in
    //   `active_overlay` because the dispatch deliberately left it
    //   there during the async wait to suppress auto-restore from
    //   the command-input / context-menu wrappers.
    let mut state = if let Some(s) = take_metadata_editor(app) {
        s
    } else {
        super::keybindings::open_metadata_editor(app);
        let prior = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
        match prior {
            ActiveOverlay::MetadataEditor(state) => state,
            other => {
                app.active_overlay = other;
                return;
            }
        }
    };

    if state.active_surface().paths != paths {
        app.set_status(":tags-mb: selection changed since lookup; rerun".to_string());
        app.active_overlay = ActiveOverlay::MetadataEditor(state);
        return;
    }
    // Compute the skip reason BEFORE populate so we can surface
    // it on the status line — populate itself runs the same
    // checks internally for the gate but only logs to env_logger.
    let skip_reason = super::musicbrainz::per_track_skip_reason(&state.active_surface().paths, release);
    // Phase C item 3: surface track-count divergence as a non-fatal
    // warning. MB releases sometimes carry bonus/hidden tracks not
    // present on the SACD area being tagged, or the reverse —
    // populate writes what it can match by position. The helper
    // guards single-image rips (where N>1 MB tracks ride in the
    // CUESHEET tag, not in N files) so they don't false-warn.
    let track_count_warning = super::musicbrainz::track_count_mismatch_message(&state, release);

    // Keep the active tab snapshot current before and after MB population. For
    // multi-presentation editors this preserves per-tab state and lets the
    // apply-to-all confirmation copy only the MB-populated values from the
    // active presentation into matching sibling presentations.
    super::musicbrainz::populate_editor_from_mb(&mut state, release);
    let dvdv_duration_warning =
        super::musicbrainz::apply_dvdv_duration_warnings(&mut state, release);
    state.phase = super::app::MetadataEditorPhase::Editing;
    state.active_surface_mut().dirty = true;

    let label = if release.title.is_empty() {
        "(untitled)"
    } else {
        &release.title
    };
    let mut msg = format!(":tags-mb: applied \"{}\" — review then save", label);
    if let Some(reason) = skip_reason {
        msg.push_str(&format!(" [{}]", reason));
    }
    if let Some(warn) = track_count_warning {
        msg.push_str(&format!(" [{}]", warn));
    }
    if let Some(warn) = dvdv_duration_warning {
        msg.push_str(&format!(" [{}]", warn));
    }
    // Cache release list for :mb-back when there's more than
    // one to choose from. Single-match has nothing to go back
    // to (re-opening the picker with one entry is pointless).
    if releases.len() > 1 {
        state.mb_back = Some(super::app::MbBackCache {
            releases,
            paths,
            selected,
        });
    }

    let has_matching_presentations = state.has_multiple_presentations();
    super::keybindings::reopen_metadata_editor_after_musicbrainz_population(app, state);
    if has_matching_presentations {
        msg.push_str(" [apply to matching presentations?]");
    }
    app.set_status(msg);
}

/// Handle the result of a `:cue-fill` MusicBrainz lookup. Caches the response,
/// fills only empty/absent fields on the parsed CUE, and writes back to the
/// original `.cue` path preserving its single-image vs multi-file form.
fn handle_cue_fill_complete(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    outcome: Result<super::musicbrainz::MbLookupOutcome, String>,
    cue_path: std::path::PathBuf,
    mut album: super::cue_generate::CueAlbumInfo,
    mut tracks: Vec<super::cue_generate::CueTrackInfo>,
    layout: super::message::CueFillLayout,
    toc_string: String,
) {
    let outcome = match outcome {
        Ok(o) => o,
        Err(e) => {
            app.set_status(format!(":cue-fill: lookup failed: {}", e));
            return;
        }
    };

    if let Some(json) = outcome.cache_response.as_deref() {
        if let Err(e) = app.db.store_mb_response(&toc_string, json) {
            log::warn!("MB cache store failed: {}", e);
        }
    }

    let release = match outcome.releases.into_iter().next() {
        Some(r) => r,
        None => {
            app.set_status(":cue-fill: no MusicBrainz release matched this disc TOC".to_string());
            return;
        }
    };

    let stats = super::cue_generate::fill_cue_with_mb(&mut album, &mut tracks, &release);
    if stats.is_empty() {
        app.set_status(format!(
            ":cue-fill: nothing to fill (CUE already complete) — {}",
            cue_path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
        ));
        return;
    }

    let cue_content = match layout {
        super::message::CueFillLayout::SingleImage {
            image_filename,
            format_tag,
        } => super::cue_generate::generate_single_image_cue(
            &album,
            &tracks,
            &image_filename,
            &format_tag,
        ),
        super::message::CueFillLayout::MultiFile => {
            super::cue_generate::generate_multifile_cue(&album, &tracks)
        }
    };

    let mut parts = Vec::new();
    if stats.titles_filled > 0 {
        parts.push(format!(
            "{} title{}",
            stats.titles_filled,
            if stats.titles_filled == 1 { "" } else { "s" }
        ));
    }
    if stats.artists_filled > 0 {
        parts.push(format!(
            "{} performer{}",
            stats.artists_filled,
            if stats.artists_filled == 1 { "" } else { "s" }
        ));
    }
    if stats.isrcs_filled > 0 {
        parts.push(format!(
            "{} ISRC{}",
            stats.isrcs_filled,
            if stats.isrcs_filled == 1 { "" } else { "s" }
        ));
    }
    if stats.year_filled {
        parts.push("date".to_string());
    }
    if stats.catalog_filled {
        parts.push("catalog".to_string());
    }
    let summary = format!("Will fill: {}", parts.join(", "));
    let _ = tx;
    app.active_overlay = ActiveOverlay::CuePreview(Box::new(super::app::CuePreviewState::new(
        cue_content,
        cue_path,
        summary.clone(),
    )));
    app.set_status(summary);
}

#[cfg(test)]
mod cue_proxy_batch_cursor_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn batch_cursor_probe_routes_cue_to_proxy_probe() {
        assert_eq!(
            batch_cursor_probe_kind(Path::new("/music/disc.cue")),
            BatchCursorProbeKind::CueProxy
        );
        assert_eq!(
            batch_cursor_probe_kind(Path::new("/music/track.flac")),
            BatchCursorProbeKind::Audio
        );
    }
}


#[cfg(test)]
mod musicbrainz_completion_dispatch_tests {
    use super::*;
    use crate::config::TonepoetConfig;
    use crate::disc::model::PresentationId;
    use crate::tui::app::{ConfirmAction, MetadataEditorState, PresentationTab};
    use crate::tui::probe::TagEntry;
    use lofty::tag::ItemKey;

    fn tx() -> mpsc::Sender<AppMessage> {
        let (tx, _rx) = mpsc::channel(4);
        tx
    }

    fn paths(n: usize) -> Vec<std::path::PathBuf> {
        (0..n)
            .map(|idx| std::path::PathBuf::from(format!("/tmp/track{:02}.flac", idx + 1)))
            .collect()
    }

    fn tag(display_key: &str, value: &str, n_paths: usize) -> TagEntry {
        TagEntry {
            display_key: display_key.to_string(),
            item_key: ItemKey::TrackTitle,
            value: value.to_string(),
            original: value.to_string(),
            is_binary: false,
            is_mixed: false,
            per_file_values: vec![value.to_string(); n_paths],
            per_file_originals: vec![value.to_string(); n_paths],
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        }
    }

    fn tab(id: PresentationId, label: &str, tab_paths: Vec<std::path::PathBuf>) -> PresentationTab {
        let n_paths = tab_paths.len();
        let group_value = match id {
            PresentationId::DvdAudioGroup(group) => group.to_string(),
            _ => String::new(),
        };
        PresentationTab::new(
            id,
            label,
            tab_paths,
            vec![
                tag("TITLE", label, n_paths),
                tag("DVDA_GROUP", &group_value, n_paths),
            ],
            (0..n_paths).map(|idx| format!("Track {:02}", idx + 1)).collect(),
            crate::tui::app::MetadataTechnicalDetails::default(),
        )
    }


    fn editor_with_tabs(active_tab: usize, n_paths: usize) -> Box<MetadataEditorState> {
        let active_paths = paths(n_paths);
        let tabs = vec![
            tab(
                PresentationId::DvdAudioGroup(1),
                "Group 1: MLP 96kHz/24-bit 5.1",
                active_paths.clone(),
            ),
            tab(
                PresentationId::DvdAudioGroup(3),
                "Group 3: LPCM 96kHz/24-bit stereo",
                paths(n_paths),
            ),
        ];
        Box::new(MetadataEditorState::for_disc_presentations(tabs, active_tab))
    }


    fn release(id: &str, title: &str) -> crate::tui::musicbrainz::MbRelease {
        crate::tui::musicbrainz::MbRelease {
            release_id: id.to_string(),
            title: title.to_string(),
            ..Default::default()
        }
    }

    fn lookup_outcome(
        releases: Vec<crate::tui::musicbrainz::MbRelease>,
    ) -> crate::tui::musicbrainz::MbLookupOutcome {
        crate::tui::musicbrainz::MbLookupOutcome {
            releases,
            cache_response: None,
        }
    }

    fn search_outcome(
        releases: Vec<crate::tui::musicbrainz::MbRelease>,
    ) -> crate::tui::musicbrainz::MbSearchOutcome {
        crate::tui::musicbrainz::MbSearchOutcome {
            releases,
            cache_writes: Vec::new(),
        }
    }

    fn ctx_for(paths: Vec<std::path::PathBuf>, editor_park: bool) -> crate::tui::message::TagsMbContext {
        crate::tui::message::TagsMbContext {
            paths,
            editor_park,
            fallback_seed: None,
        }
    }

    fn assert_apply_all_confirmation(app: &AppState) -> &MetadataEditorState {
        let ActiveOverlay::Confirmation { message, action } = &app.active_overlay else {
            panic!("expected apply-to-all confirmation overlay, got {:?}", app.active_overlay);
        };
        assert_eq!(
            message,
            "Apply MusicBrainz tags to all matching presentations?"
        );
        let ConfirmAction::ApplyMbToAllPresentations(state) = action else {
            panic!("expected ApplyMbToAllPresentations, got {:?}", action);
        };
        state.as_ref()
    }

    #[test]
    fn tags_from_mb_complete_message_dispatch_sets_lookup_error_status() {
        let mut app = AppState::new(TonepoetConfig::default());
        let tx = tx();
        let msg = AppMessage::TagsFromMbComplete {
            outcome: crate::tui::message::MbOutcome::Toc {
                outcome: Err("synthetic lookup failure".to_string()),
                toc_string: "1+1".to_string(),
            },
            ctx: ctx_for(Vec::new(), false),
        };

        handle_message(&mut app, msg, &tx);

        let status = app.status_message.as_ref().map(|(msg, _)| msg.as_str());
        assert_eq!(
            status,
            Some(":tags-mb: TOC lookup failed: synthetic lookup failure")
        );
    }

    #[test]
    fn multi_match_completion_parks_open_editor_and_restores_it_on_cancel_path() {
        let mut app = AppState::new(TonepoetConfig::default());
        let tx = tx();
        let editor = editor_with_tabs(0, 2);
        let editor_paths = editor.active_surface().paths.clone();
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);

        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Toc {
                    outcome: Ok(lookup_outcome(vec![
                        release("", "Candidate A"),
                        release("", "Candidate B"),
                    ])),
                    toc_string: "1+2".to_string(),
                },
                ctx: ctx_for(editor_paths.clone(), true),
            },
            &tx,
        );

        match &app.active_overlay {
            ActiveOverlay::MbSelect(state) => {
                assert_eq!(state.releases.len(), 2);
                assert_eq!(state.paths, editor_paths);
            }
            other => panic!("expected MbSelect overlay, got {:?}", other),
        }
        assert!(
            app.pending_metadata_editor.is_some(),
            "the source editor must be parked while MbSelect is open"
        );

        restore_parked_editor(&mut app);

        assert!(app.pending_metadata_editor.is_none());
        match &app.active_overlay {
            ActiveOverlay::MetadataEditor(state) => {
                assert_eq!(state.presentation_tabs.len(), 2);
                assert_eq!(state.active_tab, 0);
            }
            other => panic!("expected parked editor to be restored, got {:?}", other),
        }
    }

    #[test]
    fn single_match_completion_from_message_opens_apply_all_confirmation() {
        let mut app = AppState::new(TonepoetConfig::default());
        let tx = tx();
        let editor = editor_with_tabs(0, 2);
        let editor_paths = editor.active_surface().paths.clone();
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);

        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Toc {
                    outcome: Ok(lookup_outcome(vec![release("", "One Match Album")])),
                    toc_string: "1+2".to_string(),
                },
                ctx: ctx_for(editor_paths, true),
            },
            &tx,
        );

        let state = assert_apply_all_confirmation(&app);
        assert_eq!(state.presentation_tabs.len(), 2);
        assert_eq!(state.active_tab, 0);
        assert!(app.pending_metadata_editor.is_some());
        let status = app.status_message.as_ref().map(|(msg, _)| msg.as_str());
        assert!(
            status
                .unwrap_or_default()
                .contains("[apply to matching presentations?]")
        );
    }

    #[test]
    fn search_single_match_completion_uses_same_apply_all_handoff() {
        let mut app = AppState::new(TonepoetConfig::default());
        let tx = tx();
        let editor = editor_with_tabs(0, 2);
        let editor_paths = editor.active_surface().paths.clone();
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);

        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Search {
                    outcome: Ok(search_outcome(vec![release("", "Search Match Album")])),
                    query_label: "artist / album".to_string(),
                },
                ctx: ctx_for(editor_paths, true),
            },
            &tx,
        );

        let state = assert_apply_all_confirmation(&app);
        assert_eq!(state.presentation_tabs.len(), 2);
        assert_eq!(state.active_tab, 0);
        assert!(app.pending_metadata_editor.is_some());
    }

    #[test]
    fn picked_mb_select_release_uses_parked_editor_and_caches_picker_back_state() {
        let mut app = AppState::new(TonepoetConfig::default());
        let editor = editor_with_tabs(0, 2);
        let editor_paths = editor.active_surface().paths.clone();
        app.pending_metadata_editor = Some(editor);
        app.active_overlay = ActiveOverlay::MbSelect(Box::new(crate::tui::app::MbSelectState::new(
            vec![release("", "Candidate A"), release("", "Candidate B")],
            editor_paths.clone(),
        )));

        open_editor_with_mb_release(
            &mut app,
            vec![release("", "Candidate A"), release("", "Candidate B")],
            1,
            editor_paths,
        );

        let state = assert_apply_all_confirmation(&app);
        let mb_back = state
            .mb_back
            .as_ref()
            .expect("picker-selected releases should keep :mb-back state");
        assert_eq!(mb_back.selected, 1);
        assert_eq!(mb_back.releases.len(), 2);
        assert_eq!(state.presentation_tabs.len(), 2);
    }
}
