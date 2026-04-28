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
        super::app::SourceMode::Batch { paths, cursor, .. } => {
            paths.get(*cursor) == Some(&path)
        }
        _ => false,
    };

    if still_current {
        // Skip if already in flight (dedup guard).
        if app.convert.source.batch_probe_pending.as_ref() != Some(&path) {
            app.convert.source.batch_probe_pending = Some(path.clone());
            super::browse::spawn_audio_probe(path, tx.clone());
        }
    }
}

/// Fire a debounced search if the user has stopped typing for 200ms.
fn check_search_debounce(app: &mut AppState, tx: &tokio::sync::mpsc::Sender<super::message::AppMessage>) {
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
fn handle_message(app: &mut AppState, msg: AppMessage, tx: &mpsc::Sender<AppMessage>) {
    match msg {
        AppMessage::ConversionProgress { item_id, status } => {
            // Capture terminal state info BEFORE the status is moved into the item.
            let history_data = match &status {
                crate::convert::ConversionStatus::Completed { output_path } => {
                    Some((true, Some(output_path.display().to_string()), None::<String>))
                }
                crate::convert::ConversionStatus::Failed { error } => {
                    Some((false, None, Some(error.clone())))
                }
                _ => None,
            };

            app.manager.update_item_status(&item_id, status, 0.0);

            // Save queue + record history on terminal states.
            if let Some((success, output_path, error_msg)) = history_data {
                app.save_queue();

                // Record in conversion history (read item from snapshot for metadata).
                if let Some(item) = app.items_snapshot.iter().find(|i| i.id == item_id) {
                    let now = chrono::Utc::now().to_rfc3339();
                    let rg_mode = if item.options.calculate_replaygain {
                        item.options.replaygain_mode
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
                        item.options.target_bit_depth.map(|d| format!("{}", d)).as_deref(),
                        item.options.dither_type.as_ref().map(|d| format!("{:?}", d)).as_deref(),
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
                    // Check analysis cache for HDCD / PE info that should
                    // be surfaced in the info pane.
                    info.metadata.preemphasis_metadata =
                        super::probe::preemphasis_metadata_check_pub(&path);
                    if let Ok(meta) = std::fs::metadata(&path) {
                        let mtime = meta.modified()
                            .map(crate::db::systemtime_to_unix)
                            .unwrap_or(0);
                        if let Some(analysis) = app.db.get_cached_analysis(
                            &path.display().to_string(), mtime, meta.len(),
                        ) {
                            if analysis.hdcd_detected == Some(true) {
                                info.metadata.hdcd_detail = analysis.hdcd_detail;
                            }
                        }
                    }

                    // Clone for the browse cache (shared via Arc).
                    app.browse
                        .probe_cache
                        .insert(path.clone(), Some(std::sync::Arc::new(info.clone())));

                    // Persist to SQLite probe cache for cross-session reuse.
                    if let Ok(meta) = std::fs::metadata(&path) {
                        let mtime = meta.modified()
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
        AppMessage::AnalysisComplete { result } => {
            app.analysis_pending = app.analysis_pending.saturating_sub(1);
            match result {
                Ok(result) => {
                    // Persist to SQLite analysis cache for cross-session reuse.
                    if let Ok(meta) = std::fs::metadata(&result.path) {
                        let mtime = meta.modified()
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
                            app.browse.probe_cache.insert(
                                result.path.clone(),
                                Some(std::sync::Arc::new(info)),
                            );
                        }
                    }

                    app.analysis_results.push(*result);
                    if app.analysis_pending == 0 {
                        // Sort results by disc/track for logical display order.
                        {
                            let mut result_paths: Vec<std::path::PathBuf> = app.analysis_results
                                .iter().map(|r| r.path.clone()).collect();
                            crate::tui::probe::sort_paths_by_track(&mut result_paths);
                            app.analysis_results.sort_by(|a, b| {
                                let ai = result_paths.iter().position(|p| *p == a.path).unwrap_or(usize::MAX);
                                let bi = result_paths.iter().position(|p| *p == b.path).unwrap_or(usize::MAX);
                                ai.cmp(&bi)
                            });
                        }
                        let count = app.analysis_results.len();
                        let last = &app.analysis_results[count - 1];
                        let name = last.path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default();
                        app.set_status(format!(
                            "Analyzed: {} — DR{} ({})",
                            name, last.dr_value, super::analyze::dr_label(last.dr_value),
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
            app.preemph_pending = app.preemph_pending.saturating_sub(1);
            app.preemph_results.push(result);
            if app.preemph_pending == 0 {
                // Sort by path for consistent display.
                app.preemph_results.sort_by(|a, b| a.path.cmp(&b.path));
                let detected = app.preemph_results.iter()
                    .filter(|r| r.confidence == crate::tui::preemphasis::PreemphasisConfidence::Detected)
                    .count();
                let possible = app.preemph_results.iter()
                    .filter(|r| r.confidence == crate::tui::preemphasis::PreemphasisConfidence::Possible)
                    .count();
                let total = app.preemph_results.len();
                if detected > 0 || possible > 0 {
                    app.set_status(format!(
                        "Pre-emphasis: {} detected, {} possible out of {} file(s)",
                        detected, possible, total,
                    ));
                } else {
                    app.set_status(format!(
                        "Pre-emphasis: not detected in {} file(s)",
                        total,
                    ));
                }
                app.active_overlay = super::app::ActiveOverlay::Preemphasis { scroll: 0 };
            }
        }
        AppMessage::CorpusTrainComplete { result } => {
            match result {
                Ok((n_tracks, n_frames)) => {
                    app.set_status(format!(
                        "Corpus trained: {} tracks, {} frames",
                        n_tracks, n_frames,
                    ));
                }
                Err(e) => {
                    app.set_status(format!("Corpus training failed: {}", e));
                }
            }
        }
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
                    app.set_status(format!(
                        "Compared {} pair(s): all bit-identical",
                        identical,
                    ));
                } else {
                    app.set_status(format!(
                        "Compared {} pair(s): {} identical, {} differ",
                        identical + differ, identical, differ,
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
                    let mut result_paths: Vec<std::path::PathBuf> = app.verify_results
                        .iter().map(|r| r.path.clone()).collect();
                    crate::tui::probe::sort_paths_by_track(&mut result_paths);
                    app.verify_results.sort_by(|a, b| {
                        let ai = result_paths.iter().position(|p| *p == a.path).unwrap_or(usize::MAX);
                        let bi = result_paths.iter().position(|p| *p == b.path).unwrap_or(usize::MAX);
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
                        passed + failed, passed, failed,
                    ));
                }
                app.active_overlay = super::app::ActiveOverlay::Verify { scroll: 0 };
            }
        }
        AppMessage::PathValidationComplete { input, result } => {
            match result {
                Ok(path) => {
                    let display = path.display().to_string();
                    app.browse.navigate_to(path);
                    app.set_status(&format!("cd: {}", display));
                }
                Err(e) => {
                    app.set_status(&format!(":cd {}: {}", input, e));
                }
            }
        }
        AppMessage::DirScanComplete { path, parent_entry, dirs, files, error } => {
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

            // Populate raw scan results.
            app.browse.parent_entry = parent_entry;
            app.browse.all_dirs = dirs;
            app.browse.all_files = files;

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
        }
        AppMessage::MetadataWriteComplete { path, field, result } => {
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
        AppMessage::ArchiveListingComplete { archive_path, result, password } => {
            match *result {
                Ok(listing) => {
                    let count = listing.entries.len();
                    app.browse.enter_archive(listing, password);
                    app.set_status(&format!(
                        "Opened {} ({} entries)",
                        archive_path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default(),
                        count,
                    ));
                }
                Err(e) => {
                    app.set_status(&format!("Archive error: {}", e));
                }
            }
        }
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
        AppMessage::MetadataEditorWriteComplete { results } => {
            let total = results.len();
            let failed: Vec<_> = results.iter()
                .filter(|(_, r)| r.is_err())
                .collect();
            if failed.is_empty() {
                app.set_status(format!(
                    "Metadata saved ({} file{})",
                    total, if total == 1 { "" } else { "s" },
                ));
            } else {
                let first_err = failed[0].1.as_ref().unwrap_err();
                app.set_status(format!(
                    "Metadata: {} saved, {} failed — {}",
                    total - failed.len(), failed.len(), first_err,
                ));
            }
            // Invalidate caches for all written files.
            for (path, _) in &results {
                app.browse.probe_cache.remove(path);
                let _ = app.db.invalidate_probe(&path.display().to_string());
            }
            app.active_overlay = ActiveOverlay::None;
            app.browse.probe_current_with_db(tx, Some(&app.db));
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
                        let _ = tx.send(AppMessage::GnudbReadComplete { result, paths }).await;
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
        AppMessage::GnudbReadComplete { result, paths } => {
            match result {
                Ok(entry) => {
                    // Open GNUDB review overlay for user editing before accept.
                    let review = super::gnudb::build_review_state(&entry, paths);
                    app.set_status(format!(
                        "GNUDB: {} / {} ({} tracks) — review and edit",
                        entry.artist, entry.album, entry.tracks.len(),
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
                n_discs, if n_discs == 1 { "" } else { "s" }, n_tracks,
            ));
            app.active_overlay = ActiveOverlay::GnudbReview(Box::new(review));
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
                        "Pasted {} name{}", applied, if applied == 1 { "" } else { "s" }
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
        ActiveOverlay::MetadataEditor(_) => {
            let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
            if let ActiveOverlay::MetadataEditor(mut state) = overlay {
                use super::app::MetadataEditorPhase;
                if state.phase == MetadataEditorPhase::DetailEdit {
                    let field_idx = state.detail_field_idx;
                    if field_idx < state.entries.len() {
                        let sanitized = text.replace("\r\n", "\n").replace('\r', "\n");
                        let lines: Vec<&str> = sanitized.split('\n').collect();
                        let n_files = state.paths.len();
                        let entry = &mut state.entries[field_idx];
                        let is_album = entry.display_key.eq_ignore_ascii_case("ALBUM");

                        if is_album {
                            let val = lines.first().map(|l| l.trim().to_string())
                                .unwrap_or_default();
                            for v in &mut entry.per_file_values {
                                *v = val.clone();
                            }
                        } else {
                            for (i, line) in lines.iter().enumerate() {
                                if i >= n_files { break; }
                                entry.per_file_values[i] = line.trim().to_string();
                            }
                        }

                        // Cancel any active inline edit.
                        state.detail_edit = None;

                        // Update merged display value + mixed state.
                        let all_same = entry.per_file_values.windows(2)
                            .all(|w| w[0] == w[1]);
                        entry.is_mixed = !all_same && n_files > 1;
                        entry.value = if entry.is_mixed {
                            "<multiple values>".to_string()
                        } else {
                            entry.per_file_values.first().cloned().unwrap_or_default()
                        };

                        state.dirty = true;
                        let applied = lines.len().min(n_files);
                        app.set_status(format!(
                            "Pasted {} value{}", applied,
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
