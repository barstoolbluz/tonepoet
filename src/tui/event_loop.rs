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
                        let _ = tx.send(AppMessage::GnudbReadComplete { result, paths, origin_matches: None }).await;
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
        AppMessage::GnudbReadComplete { result, paths, origin_matches } => {
            match result {
                Ok(entry) => {
                    // Open GNUDB review overlay for user editing before accept.
                    let mut review = super::gnudb::build_review_state(&entry, paths);
                    review.origin_matches = origin_matches;
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
                let verified: usize = pages.iter().map(|p|
                    p.result.tracks.iter()
                        .filter(|t| matches!(
                            t.status,
                            crate::tui::ctdb::CtdbTrackStatus::Verified
                            | crate::tui::ctdb::CtdbTrackStatus::VerifiedRs
                        ))
                        .count()
                ).sum();
                app.set_status(format!(
                    "CUETools DB: {} discs, {}/{} tracks verified",
                    pages.len(), verified, total,
                ));
            }

            // If a context-menu / direct :ctdb-repair invoked us, find the
            // first page that actually has something to repair (mismatches
            // AND parity) and start the overlay there so the subsequent
            // :ctdb-repair re-dispatch operates on a repairable disc.
            let auto_repair = std::mem::replace(
                &mut app.auto_repair_on_ctdb_complete, false,
            );
            let active_page = if auto_repair {
                pages.iter().position(|p| {
                    p.result.parity_url.is_some()
                        && p.result.tracks.iter().any(|t|
                            t.status == crate::tui::ctdb::CtdbTrackStatus::Mismatch
                        )
                }).unwrap_or(0)
            } else {
                0
            };

            app.active_overlay = ActiveOverlay::CtdbVerify(
                Box::new(crate::tui::app::CtdbVerifyState {
                    pages,
                    active_page,
                    scroll: 0,
                }),
            );

            if auto_repair {
                // Re-enter Command::CtdbRepair now that the overlay is up.
                // The handler will validate parity/mismatches/CRCs and
                // either pop the confirmation dialog, defer to AR, or
                // emit a status message ("No mismatches detected", etc.).
                super::command::execute_command(
                    app, super::command::Command::CtdbRepair, tx,
                );
            }
        }
        AppMessage::ArBatchComplete { result } => {
            let total = result.albums.len();
            let verified = result.albums.iter()
                .filter(|a| a.verified == a.total_tracks && a.total_tracks > 0 && !a.not_in_db)
                .count();
            let report_msg = result.report_path.as_ref()
                .map(|p| format!(" — report: {}", p.display()))
                .unwrap_or_default();
            app.set_status(format!(
                "Batch AR: {}/{} albums verified{}",
                verified, total, report_msg,
            ));
            app.active_overlay = ActiveOverlay::ArBatchReport {
                result,
                scroll: 0,
            };
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
        AppMessage::CtdbRepairComplete { result } => {
            match result {
                Ok(summary) => {
                    app.set_status(summary);
                    app.active_overlay = ActiveOverlay::None;
                    app.browse.refresh();
                }
                Err(e) => {
                    app.set_status(format!("CTDB repair failed: {}", e));
                }
            }
        }
        AppMessage::CueMbComplete { outcome, paths, output_dir, single_image, toc_string } => {
            handle_cue_mb_complete(
                app, tx, outcome, paths, output_dir, single_image, toc_string,
            );
        }
        AppMessage::CueFillComplete { outcome, cue_path, album, tracks, layout, toc_string } => {
            handle_cue_fill_complete(
                app, tx, outcome, cue_path, *album, tracks, layout, toc_string,
            );
        }
        AppMessage::TagsFromMbComplete { outcome, ctx } => {
            handle_tags_from_mb_complete(app, tx, outcome, ctx);
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
            let verified: usize = pages.iter().map(|p|
                p.result.tracks.iter()
                    .filter(|t| t.status == crate::tui::accuraterip::ArTrackStatus::Verified)
                    .count()
            ).sum();
            if pages.len() == 1 {
                let summary = crate::tui::accuraterip::format_summary(&pages[0].result);
                app.set_status(format!("AccurateRip: {}", summary));
            } else {
                app.set_status(format!(
                    "AccurateRip: {} discs, {}/{} tracks verified",
                    pages.len(), verified, total,
                ));
            }
            // Cache AR results per track (each track keyed by its own path).
            for page in &pages {
                for t in &page.result.tracks {
                    if let Ok(meta) = std::fs::metadata(&t.path) {
                        let mtime = meta.modified()
                            .map(crate::db::systemtime_to_unix)
                            .unwrap_or(0);
                        if let Err(e) = app.db.store_ar(
                            &t.path.display().to_string(),
                            mtime, meta.len(),
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
            let matched_page_idx = app.pending_ctdb_repair.as_ref()
                .and_then(|p| p.paths.first().cloned())
                .and_then(|target| {
                    pages.iter().position(|p|
                        p.result.tracks.first().map(|t| &t.path) == Some(&target)
                    )
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
                                None => { all_ok = false; break; }
                            };
                            match common {
                                Some(prev) if prev != off => { all_ok = false; break; }
                                None => common = Some(off),
                                _ => {}
                            }
                        }
                        if all_ok { common } else { None }
                    }
                };

                let (offset, offset_note) = match resolved_offset {
                    Some(n) => (n, format!("offset: {:+} samples (from AR verification)", n)),
                    None => (0, "offset: +0 (AR could not determine a drive offset — \
                                 proceeding at +0 may produce incorrect repairs if \
                                 your drive has a real read offset)".to_string()),
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
                    if let Some(offset) = crate::tui::accuraterip::detect_uniform_offset(&page.result) {
                        let paths: Vec<std::path::PathBuf> = page.result.tracks.iter()
                            .map(|t| t.path.clone())
                            .collect();
                        let n = paths.len();
                        app.active_overlay = ActiveOverlay::Confirmation {
                            message: format!(
                                "Apply offset correction ({:+} samples) to {} tracks?\n\
                                 Files will be re-encoded to FLAC and verified at offset +0\n\
                                 before replacing originals.",
                                offset, n,
                            ),
                            action: crate::tui::app::ConfirmAction::OffsetCorrection { paths, offset },
                        };
                        return;
                    }
                }
                // No fixable offset — show results normally.
                app.set_status("No offset correction needed — showing verification results".to_string());
            }

            app.active_overlay = ActiveOverlay::AccurateRipVerify(
                Box::new(crate::tui::app::ArVerifyState {
                    pages,
                    active_page: 0,
                    scroll: 0,
                }),
            );
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
        let ext = paths[0].extension().and_then(|e| e.to_str()).unwrap_or("flac");
        let fmt = super::cue_generate::cue_format_tag(ext);
        super::cue_generate::generate_single_image_cue(&album, &tracks, &image_name, fmt)
    } else {
        super::cue_generate::generate_multifile_cue(&album, &tracks)
    };

    let cue_filename = super::cue_generate::cue_output_filename(&album);
    let cue_path = output_dir.join(&cue_filename);

    let mode = if single_image { "single image" } else { "multi-file" };
    let pregaps = tracks.iter().filter(|t| t.pregap_frames.is_some()).count();
    let pregap_note = if pregaps > 0 {
        format!(", {} pregap{}", pregaps, if pregaps == 1 { "" } else { "s" })
    } else {
        String::new()
    };
    let summary = format!(
        "MusicBrainz CUE ({}, MB-enriched: \"{}\"{})",
        mode, album.title, pregap_note,
    );
    let _ = tx; // overlay handles save; nothing to dispatch here.
    app.active_overlay = ActiveOverlay::CuePreview(Box::new(
        super::app::CuePreviewState::new(cue_content, cue_path, summary.clone()),
    ));
    app.set_status(summary);
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
        MbOutcome::Toc { outcome, toc_string } => {
            handle_mb_toc_outcome(app, tx, outcome, toc_string, ctx);
        }
        MbOutcome::Search { outcome, query_label } => {
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
            Some(seed) => spawn_text_fallback(app, tx, seed, ctx),
            None => {
                app.set_status(
                    ":tags-mb: no MusicBrainz release matched this disc TOC"
                        .to_string(),
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

/// Spawn the C-2b text-search fallback from a TOC zero-match. Sets
/// the pre-spawn status, builds the in-memory cache lookup for both
/// candidate query forms (with-catno + without-catno), and fires the
/// search; the result re-enters this handler as `MbOutcome::Search`.
fn spawn_text_fallback(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    seed: super::command::SacdMbSeed,
    ctx: super::message::TagsMbContext,
) {
    let super::command::SacdMbSeed { artist, album, catalog, year } = seed;
    let n_tracks = ctx.paths.len();

    let mut cached: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let key_with = super::musicbrainz::search_cache_key(
        &artist, &album, catalog.as_deref(), year.as_deref(),
    );
    if let Some(b) = app.db.get_cached_mb_search(&key_with) {
        cached.insert(key_with, b);
    }
    if catalog.is_some() {
        let key_without = super::musicbrainz::search_cache_key(
            &artist, &album, None, year.as_deref(),
        );
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
    app.set_status(format!(
        ":tags-mb: TOC missed, {} text search for \"{}\"…",
        if cache_hit { "cached" } else { "trying" },
        label,
    ));

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
        let cached_body = app.db.get_cached_mb_search(
            &super::musicbrainz::detail_cache_key(&top.release_id),
        );
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
        let result = super::musicbrainz::fetch_release_detail(
            &release_id, n_tracks, cached_body,
        ).await;
        let _ = tx
            .send(crate::tui::message::AppMessage::MbDetailPrefetchComplete {
                release_id,
                result,
            })
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

    if state.paths != paths {
        app.set_status(":tags-mb: selection changed since lookup; rerun".to_string());
        app.active_overlay = ActiveOverlay::MetadataEditor(state);
        return;
    }
    // Compute the skip reason BEFORE populate so we can surface
    // it on the status line — populate itself runs the same
    // checks internally for the gate but only logs to env_logger.
    let skip_reason = super::musicbrainz::per_track_skip_reason(&state.paths, release);
    // Phase C-2: surface track-count divergence as a non-fatal
    // warning. MB releases sometimes carry bonus/hidden tracks not
    // present on the SACD area being tagged, or the reverse —
    // populate writes what it can match by position.
    let track_count_warning = (release.tracks.len() != state.paths.len())
        .then(|| format!("MB release has {} tracks, editor has {}",
            release.tracks.len(), state.paths.len()));
    super::musicbrainz::populate_editor_from_mb(&mut state, release);
    let label = if release.title.is_empty() { "(untitled)" } else { &release.title };
    let mut msg = format!(":tags-mb: applied \"{}\" — review then save", label);
    if let Some(reason) = skip_reason {
        msg.push_str(&format!(" [{}]", reason));
    }
    if let Some(warn) = track_count_warning {
        msg.push_str(&format!(" [{}]", warn));
    }
    app.set_status(msg);
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
    app.active_overlay = ActiveOverlay::MetadataEditor(state);
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
            cue_path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
        ));
        return;
    }

    let cue_content = match layout {
        super::message::CueFillLayout::SingleImage { image_filename, format_tag } => {
            super::cue_generate::generate_single_image_cue(
                &album, &tracks, &image_filename, &format_tag,
            )
        }
        super::message::CueFillLayout::MultiFile => {
            super::cue_generate::generate_multifile_cue(&album, &tracks)
        }
    };

    let mut parts = Vec::new();
    if stats.titles_filled > 0 {
        parts.push(format!("{} title{}", stats.titles_filled,
            if stats.titles_filled == 1 { "" } else { "s" }));
    }
    if stats.artists_filled > 0 {
        parts.push(format!("{} performer{}", stats.artists_filled,
            if stats.artists_filled == 1 { "" } else { "s" }));
    }
    if stats.isrcs_filled > 0 {
        parts.push(format!("{} ISRC{}", stats.isrcs_filled,
            if stats.isrcs_filled == 1 { "" } else { "s" }));
    }
    if stats.year_filled { parts.push("date".to_string()); }
    if stats.catalog_filled { parts.push("catalog".to_string()); }
    let summary = format!("Will fill: {}", parts.join(", "));
    let _ = tx;
    app.active_overlay = ActiveOverlay::CuePreview(Box::new(
        super::app::CuePreviewState::new(cue_content, cue_path, summary.clone()),
    ));
    app.set_status(summary);
}

