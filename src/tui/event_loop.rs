//! Async event loop: crossterm events + progress messages

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use crossterm::event::{self, Event};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc;

use super::app::{ActiveOverlay, AppScreen, AppState, TextEditTarget};
use super::browse::BrowseDeferredWorkFlags;
use crate::convert::classify::EntryKind;
use super::draw::draw_ui;
use super::keybindings::{handle_key, handle_mouse};
use super::message::AppMessage;
use super::text_input::TextInputState;

/// Keep bursty async completions from monopolizing a frame. If more messages
/// or bounded reducer slices remain, the next loop uses a zero poll timeout so
/// deferred work continues promptly without making one render absorb an
/// unbounded reducer batch.
const MAX_ASYNC_MESSAGES_PER_FRAME: usize = 32;

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
    app.tui_tx = Some(tx.clone());
    let mut deferred_browse_visible_messages: VecDeque<AppMessage> = VecDeque::new();

    loop {
        // Check whether terminal input is already queued before firing any
        // settled-focus Browse work. Key-repeat PgUp/PgDn can otherwise leave
        // an event waiting while the old debounce expires, causing periodic
        // folder classification/probe/stat work in the middle of scrolling.
        let input_waiting_at_frame_start = event::poll(Duration::from_millis(0))?;

        // 1. Refresh items from the manager
        app.refresh_items();
        app.clamp_selection();
        app.clear_expired_status();
        check_pending_browse_rename(app);
        check_batch_probe_debounce(app, &tx);
        if !input_waiting_at_frame_start {
            check_browse_probe_debounce(app, &tx);
        }
        check_search_debounce(app, &tx);
        // Close browse-only overlays if the user has left the browse screen.
        if app.current_screen != AppScreen::Browse && app.bookmarks.overlay_open {
            app.bookmarks.close_overlay();
        }
        if app.current_screen != AppScreen::Browse && app.cancel_archive_listing() {
            app.set_status("archive listing cancelled: Browse screen changed");
        }
        if app.current_screen != AppScreen::Browse && app.cancel_browse_convert_expansion() {
            app.set_status("folder expansion cancelled: Browse screen changed");
        }
        if app.current_screen != AppScreen::Browse {
            if let Some(pending) = app.pending_browse_archive_metadata.take() {
                pending.cancel_and_cleanup();
                app.set_status("archive metadata editor cancelled: Browse screen changed before extraction finished");
            }
        }
        if app.current_screen != AppScreen::Convert {
            let archive_preview_owned_or_pending = app.convert.pending_archive_preview.is_some()
                || matches!(
                    &app.convert.source.mode,
                    super::app::SourceMode::MultiTrack {
                        archive_preview: Some(_),
                        ..
                    }
                );
            if archive_preview_owned_or_pending {
                app.convert.set_source_mode(super::app::SourceMode::Empty);
                app.convert.source.cue_artifact_audio.clear();
                app.convert.metadata = super::app::MetadataState::default();
            }
        }

        // 2. Advance image-preview protocol state, then render.
        //
        // The pre-draw prepare is important for Ghostty/Kitty mouse-damage
        // recovery: mouse events are handled after the previous draw, so the
        // desired preview geometry from that frame is already known. Rebuilding
        // the Kitty protocol from cached decoded pixels before the next draw
        // lets that draw retransmit immediately instead of showing one stale or
        // missing frame. The post-draw prepare remains for first-time geometry
        // discovery and resize/layout changes recorded during this draw.
        app.prepare_image_preview_protocols();
        // `force_redraw` means the terminal diff buffer is suspected to be
        // invalid and a full clear is required before drawing. Ordinary async
        // reducer progress must not set it: the loop below uses a zero-timeout
        // tick for prompt non-clearing redraws instead.
        if app.force_redraw {
            terminal.clear()?;
            app.force_redraw = false;
        }
        terminal.draw(|f| draw_ui(f, app))?;
        app.prepare_image_preview_protocols();

        // 3. Check quit
        if app.should_quit {
            if defer_quit_for_browse_archive_metadata(app, &tx) {
                continue;
            }
            if app.pending_browse_archive_rename.is_some() {
                app.should_quit = false;
                app.quit_after_browse_archive_rename = true;
                app.set_status("quit deferred: waiting for archive rename to finish".to_string());
                continue;
            }
            if app.pending_browse_archive_delete.is_some() {
                app.should_quit = false;
                app.quit_after_browse_archive_delete = true;
                app.set_status("quit deferred: waiting for archive delete to finish".to_string());
                continue;
            }
            app.cancel_archive_listing();
            app.convert.clear_pending_archive_preview();
            app.convert.source.mode.cleanup_archive_preview_staging();
            if let Some(pending) = app.pending_browse_archive_metadata.take() {
                pending.cancel_and_cleanup();
            }
            // An active Browse archive repackage owns staged user edits. Never
            // delete that staging directory from the quit fast path; wait for
            // the repackage result so success can clean it and failure can keep
            // a retry/discard state.
            if app.browse_archive_repackage.is_some() {
                app.should_quit = false;
                app.quit_after_browse_archive_repackage = true;
                app.set_status("quit deferred: waiting for archive save to finish".to_string());
                continue;
            }
            // Save queue before exiting
            app.save_queue();
            break;
        }

        // 4. Drain async messages, but cap per-frame reducer work. While Browse
        // focus is moving, continue draining lightweight/unrelated messages
        // (conversion progress, status, analysis, etc.) and hold only messages
        // whose reducer arms mutate the visible Browse listing/info pane. This
        // prevents channel backlog and bursty catch-up without letting warm-cache
        // merges, folder classifications, or probe completions stutter scrolling.
        let defer_visible_browse_work =
            browse_visible_work_should_wait(app, input_waiting_at_frame_start);
        let (drained, reducer_work_ready_remains) = drain_async_messages_for_frame(
            app,
            &mut rx,
            &tx,
            &mut deferred_browse_visible_messages,
            defer_visible_browse_work,
        );
        let immediate_nonclearing_tick_needed =
            needs_immediate_nonclearing_tick(drained, reducer_work_ready_remains);

        // 5. Poll for crossterm events. Async messages and bounded reducer
        // slices mutate app state after this frame's draw. Use a zero timeout
        // to render/drain again promptly, but do not map that to
        // `force_redraw`: a full terminal clear is far more expensive and can
        // visibly flicker while warm-cache backlogs are being reduced.
        let poll_timeout = if immediate_nonclearing_tick_needed {
            Duration::from_millis(0)
        } else {
            Duration::from_millis(100)
        };
        if event::poll(poll_timeout)? {
            match event::read()? {
                Event::Key(key) => handle_key(app, key, &tx),
                Event::Mouse(mouse) => handle_mouse(app, mouse, &tx),
                Event::Paste(text) => handle_paste(app, &text),
                Event::Resize(_, _) => app.refresh_image_picker_after_resize(),
                _ => {}
            }
        }
    }

    Ok(())
}


fn needs_immediate_nonclearing_tick(drained_messages: usize, reducer_work_remains: bool) -> bool {
    drained_messages > 0 || reducer_work_remains
}

fn drain_async_messages_for_frame(
    app: &mut AppState,
    rx: &mut mpsc::Receiver<AppMessage>,
    tx: &mpsc::Sender<AppMessage>,
    deferred_browse_visible_messages: &mut VecDeque<AppMessage>,
    defer_browse_visible_work: bool,
) -> (usize, bool) {
    let mut inspected = 0usize;
    let mut reduced = 0usize;

    while inspected < MAX_ASYNC_MESSAGES_PER_FRAME {
        let msg = if !defer_browse_visible_work {
            match deferred_browse_visible_messages.pop_front() {
                Some(msg) => Some(msg),
                None => rx.try_recv().ok(),
            }
        } else {
            rx.try_recv().ok()
        };

        let Some(msg) = msg else {
            break;
        };
        inspected += 1;

        if defer_browse_visible_work && message_mutates_browse_visible_state(&msg) {
            deferred_browse_visible_messages.push_back(msg);
            continue;
        }

        handle_message(app, msg, tx);
        reduced += 1;
    }

    if inspected == MAX_ASYNC_MESSAGES_PER_FRAME {
        log::debug!(
            "async message drain cap reached ({} inspected, {} reduced, {} browse-visible held); deferring remaining reducer work",
            inspected,
            reduced,
            deferred_browse_visible_messages.len()
        );
    }

    let reducer_work_ready_remains = if defer_browse_visible_work {
        false
    } else {
        flush_browse_deferred_work(app, tx)
    };

    let held_browse_work_ready = !defer_browse_visible_work && !deferred_browse_visible_messages.is_empty();
    let drain_cap_reached = inspected == MAX_ASYNC_MESSAGES_PER_FRAME;
    (
        reduced,
        reducer_work_ready_remains || held_browse_work_ready || drain_cap_reached,
    )
}

fn message_mutates_browse_visible_state(msg: &AppMessage) -> bool {
    match msg {
        AppMessage::ProbeCacheWarmComplete { .. }
        | AppMessage::AudioProbeComplete { .. }
        | AppMessage::DiscProbeComplete { .. }
        | AppMessage::DirStatsComplete { .. }
        | AppMessage::FolderClassifyComplete { .. }
        | AppMessage::DirScanComplete { .. }
        | AppMessage::PathValidationComplete { .. }
        | AppMessage::SearchComplete { .. }
        | AppMessage::ArchiveListingComplete { .. }
        | AppMessage::MetadataWriteComplete { .. }
        | AppMessage::CtdbRepairComplete { .. } => true,
        AppMessage::CueWriteComplete { refresh_browse, .. } => *refresh_browse,
        AppMessage::FilePickerComplete { purpose, .. } => matches!(
            purpose,
            super::app::FilePickerPurpose::CopyTo { .. }
                | super::app::FilePickerPurpose::MoveTo { .. }
        ),
        AppMessage::FileTaskProgress { update, .. } => matches!(
            update,
            tui_file_picker::FileTaskProgressUpdate::Finished { .. }
                | tui_file_picker::FileTaskProgressUpdate::Failed { .. }
                | tui_file_picker::FileTaskProgressUpdate::Aborted { .. }
        ),
        _ => false,
    }
}

fn browse_visible_work_should_wait(app: &AppState, input_waiting_at_frame_start: bool) -> bool {
    app.current_screen == AppScreen::Browse
        && (input_waiting_at_frame_start || app.browse.focus_visible_work_deferred())
}

fn flush_browse_deferred_work(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) -> bool {
    // Merge warm-cache rows only from this settled Browse-visible reducer path.
    // The expensive SQLite read already runs off-thread; the reducer uses a
    // strict row/time budget so large backlogs drain quickly after scrolling
    // settles without monopolizing a frame or mutating Browse mid-scroll.
    let (_merged, warm_backlog_remaining) = app.browse.drain_probe_cache_warm_rows_for_frame();

    let mut work = app.browse.take_browse_deferred_work();
    if warm_backlog_remaining {
        // Probe-cache warm rows are merged in bounded slices. If the view is
        // sorted or filtered by probe-backed metadata, reapplying the listing
        // after every bounded slice can still produce many non-clearing but
        // visible reorder ticks. Keep the cheap cache inserts moving, but
        // coalesce probe-backed listing work until the backlog drains.
        let postponed = BrowseDeferredWorkFlags {
            probe_backed_resort_needed: work.probe_backed_resort_needed,
            search_reapply_needed: work.search_reapply_needed,
            visible_entries_changed: if work.classification_changed {
                false
            } else {
                work.visible_entries_changed
            },
            info_pane_changed: false,
            classification_changed: false,
        };
        if postponed.has_expensive_work() {
            app.browse.defer_browse_deferred_work(postponed);
            work.probe_backed_resort_needed = false;
            work.search_reapply_needed = false;
            if !work.classification_changed {
                work.visible_entries_changed = false;
            }
        }
    }

    if work.classification_changed || work.visible_entries_changed {
        app.browse.reapply_after_browse_preference_change(Some(tx));
    } else if work.probe_backed_resort_needed || work.search_reapply_needed {
        app.browse.resort_after_probe_cache_update_with_search(Some(tx));
    }

    if work.info_pane_changed {
        app.browse.probe_current_with_db(tx, Some(&app.db));
    }

    // A remaining warm-cache/deferred backlog means the event loop should take
    // another immediate non-clearing reducer tick. It must not request
    // `terminal.clear()`; ratatui's normal diff render is sufficient for these
    // ordinary state changes.
    warm_backlog_remaining || app.browse.has_browse_deferred_work()
}

fn defer_quit_for_browse_archive_metadata(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
) -> bool {
    if app.browse_archive_repackage.is_some() {
        app.should_quit = false;
        app.quit_after_browse_archive_repackage = true;
        app.set_status(
            "quit deferred: waiting for archive metadata repackage to finish".to_string(),
        );
        return true;
    }

    let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
    match overlay {
        ActiveOverlay::MetadataEditor(state) => {
            return reconcile_browse_archive_metadata_editor_for_quit(app, state, tx);
        }
        other => {
            app.active_overlay = other;
        }
    }

    if app
        .pending_metadata_editor
        .as_ref()
        .and_then(|state| state.archive_edit_context.as_ref())
        .is_some_and(|context| context.owner == super::app::ArchiveMetadataEditOwner::Browse)
    {
        app.should_quit = false;
        app.quit_after_browse_archive_metadata_resolution = true;
        app.set_status(
            "quit deferred: resolve the Browse archive metadata editor prompt first"
                .to_string(),
        );
        return true;
    }

    if app
        .browse
        .active_archive_staging()
        .is_some_and(|staging| staging.dirty)
    {
        app.should_quit = false;
        app.quit_after_browse_archive_repackage = true;
        super::keybindings::exit_browse_archive(app, tx);
        app.set_status("quit deferred: saving staged archive changes".to_string());
        return true;
    }

    false
}

fn reconcile_browse_archive_metadata_editor_for_quit(
    app: &mut AppState,
    mut state: Box<super::app::MetadataEditorState>,
    tx: &mpsc::Sender<AppMessage>,
) -> bool {
    let is_browse_archive_editor = state
        .archive_edit_context
        .as_ref()
        .is_some_and(|context| context.owner == super::app::ArchiveMetadataEditOwner::Browse);

    if !is_browse_archive_editor {
        app.active_overlay = ActiveOverlay::MetadataEditor(state);
        return false;
    }

    if state.phase == super::app::MetadataEditorPhase::Saving
        || state.replaygain_scan.is_some()
        || state.artwork_write.is_some()
    {
        app.should_quit = false;
        app.active_overlay = ActiveOverlay::MetadataEditor(state);
        app.set_status(
            "quit deferred: metadata editor file write is still in progress".to_string(),
        );
        return true;
    }

    super::keybindings::metadata_editor_cancel_details_probe(&mut state);

    if state.any_presentation_dirty() {
        app.should_quit = false;
        app.quit_after_browse_archive_metadata_resolution = true;
        app.pending_metadata_editor = Some(state);
        app.active_overlay = ActiveOverlay::Confirmation {
            message: concat!(
                "Discard unsaved metadata changes before quitting? ",
                "Y discards them; N/Esc returns to the editor."
            )
            .to_string(),
            action: super::app::ConfirmAction::DiscardMetadataEditorChanges,
        };
        app.set_status(
            "quit deferred: confirm unsaved Browse archive metadata changes".to_string(),
        );
        return true;
    }

    if state.has_browse_archive_staged_changes() {
        if let Some(context) = state.archive_edit_context.clone() {
            app.should_quit = false;
            app.quit_after_browse_archive_repackage = true;
            app.active_overlay = ActiveOverlay::None;
            start_browse_archive_repackage(app, context, tx);
        } else {
            app.should_quit = false;
            app.active_overlay = ActiveOverlay::MetadataEditor(state);
            app.set_status(
                "quit cancelled: Browse archive staged changes have no repackage context"
                    .to_string(),
            );
        }
        return true;
    }

    super::keybindings::cleanup_metadata_editor_archive_context(app, &state);
    app.active_overlay = ActiveOverlay::None;

    // Closing a clean metadata editor must not bypass deferred-save handling
    // for an already-dirty ArchiveBrowseState staging session. This matters
    // when the editor was opened over prior staged rename/delete edits: the
    // editor owns no new writes, but Browse still owns dirty archive staging
    // that must be saved before quitting.
    if app
        .browse
        .active_archive_staging()
        .is_some_and(|staging| staging.dirty)
    {
        app.should_quit = false;
        app.quit_after_browse_archive_repackage = true;
        super::keybindings::exit_browse_archive(app, tx);
        app.set_status("quit deferred: saving staged archive changes".to_string());
        return true;
    }

    false
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
        if super::app::is_nonprobeable_source_for_probe(&path) {
            return;
        }

        // Skip if already in flight (dedup guard). Capture the Convert source
        // generation and editable-state baseline at dispatch time so late
        // completions can update preview facts without resetting user choices.
        if app.convert.source.batch_probe_pending.as_ref() != Some(&path) {
            app.convert.source.batch_probe_pending = Some(path.clone());
            let generation = app.probe_generation;
            let baseline = super::app::ConvertProbeBaseline::capture(&app.convert);
            super::app::spawn_convert_batch_cursor_probe(
                generation,
                path,
                baseline,
                tx.clone(),
            );
        }
    }
}

fn check_browse_probe_debounce(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    if app.current_screen != AppScreen::Browse {
        app.browse.probe_debounce = None;
        return;
    }
    app.browse.check_probe_debounce_with_db(tx, Some(&app.db));
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
    session_id: u64,
    purpose: super::app::FilePickerPurpose,
    path: Option<std::path::PathBuf>,
    tx: &mpsc::Sender<AppMessage>,
) {
    match purpose.clone() {
        super::app::FilePickerPurpose::SelectArtwork { picture_type } => {
            let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
            match overlay {
                ActiveOverlay::MetadataEditor(mut state) => {
                    let matches_open_picker = state
                        .file_picker
                        .as_ref()
                        .map(|picker| picker.session_id == session_id && picker.purpose == purpose)
                        .unwrap_or(false);
                    if matches_open_picker {
                        if let Some(current_dir) = state
                            .file_picker
                            .as_ref()
                            .map(|picker| picker.current_dir().to_path_buf())
                        {
                            app.last_artwork_picker_dir = Some(current_dir);
                        }
                        state.file_picker = None;
                        state.pending_artwork_type = None;
                    }

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
        super::app::FilePickerPurpose::CopyTo { sources, force }
        | super::app::FilePickerPurpose::MoveTo { sources, force } => {
            let is_move = matches!(purpose, super::app::FilePickerPurpose::MoveTo { .. });
            let op = if is_move { "move" } else { "copy" };
            let conflict_policy = matching_file_picker_conflict_policy(app, session_id, &purpose);
            if !close_matching_file_picker(app, session_id, &purpose) {
                app.set_status(format!("file picker: ignored stale {op} completion"));
                return;
            }
            let Some(dest_dir) = path else {
                app.set_status(format!("{op} cancelled"));
                return;
            };
            if !dest_dir.is_dir() {
                app.set_status(format!(
                    "{op} destination is not a directory: {}",
                    dest_dir.display()
                ));
                return;
            }
            let target = if is_move {
                super::app::TextEditTarget::BrowseMove { sources, force }
            } else {
                super::app::TextEditTarget::BrowseCopy { sources, force }
            };
            let dest = dest_dir.to_string_lossy().into_owned();
            super::keybindings::apply_file_op_with_tx_and_conflict_policy(
                app,
                target,
                &dest,
                tx,
                conflict_policy,
            );
        }
        super::app::FilePickerPurpose::SelectDestination => {
            if !close_matching_file_picker(app, session_id, &purpose) {
                app.set_status("file picker: ignored stale destination completion");
                return;
            }
            match path {
                Some(path) if path.is_dir() => {
                    app.convert.output_options.dest_path = Some(path.clone());
                    app.preset.mark_modified();
                    app.set_status(format!("destination: {}", path.display()));
                }
                Some(path) => {
                    app.set_status(format!("destination is not a directory: {}", path.display()));
                }
                None => app.set_status("destination picker cancelled"),
            }
        }
        super::app::FilePickerPurpose::SelectPreset => {
            if !close_matching_file_picker(app, session_id, &purpose) {
                app.set_status("file picker: ignored stale preset completion");
                return;
            }
            let Some(path) = path else {
                app.set_status("preset load cancelled");
                return;
            };
            let Some(name) = path.file_stem().and_then(|name| name.to_str()).map(str::to_string) else {
                app.set_status(format!("invalid preset path: {}", path.display()));
                return;
            };
            match super::presets::load_preset_from_path(&path) {
                Ok(preset) => {
                    preset.apply_to_pills(&mut app.convert.format, &mut app.convert.output_options, &mut app.convert.metadata);
                    app.preset.set_active_preset_path(name.clone(), path.clone());
                    app.preset.modified = false;
                    app.set_status(format!("Loaded preset: {}", path.display()));
                }
                Err(e) => app.set_status(format!("Load failed: {}", e)),
            }
        }
        super::app::FilePickerPurpose::SavePreset => {
            if !close_matching_file_picker(app, session_id, &purpose) {
                app.set_status("file picker: ignored stale preset-save completion");
                return;
            }
            let Some(path) = path else {
                app.set_status("preset save cancelled");
                return;
            };
            let Some(name) = path.file_stem().and_then(|name| name.to_str()).map(str::to_string) else {
                app.set_status(format!("invalid preset path: {}", path.display()));
                return;
            };
            let preset = super::presets::TuiPreset::from_pill_state(
                &name,
                &app.convert.format,
                &app.convert.output_options,
                &app.convert.metadata,
            );
            match super::presets::save_preset_to_path_with_db(&preset, &path, &app.db) {
                Ok(()) => {
                    app.preset.set_active_preset_path(name.clone(), path.clone());
                    app.preset.modified = false;
                    app.set_status(format!("Saved preset: {}", path.display()));
                }
                Err(e) => app.set_status(format!("Save failed: {}", e)),
            }
        }
        super::app::FilePickerPurpose::Generic { id } => {
            if !close_matching_file_picker(app, session_id, &purpose) {
                app.set_status(format!("file picker purpose '{id}': ignored stale completion"));
                return;
            }
            match path {
                Some(path) => app.set_status(format!(
                    "file picker purpose '{id}' selected {}",
                    path.display()
                )),
                None => app.set_status(format!("file picker purpose '{id}' cancelled")),
            }
        }
        super::app::FilePickerPurpose::SelectFile | super::app::FilePickerPurpose::SelectDirectory => {
            if !close_matching_file_picker(app, session_id, &purpose) {
                app.set_status("file picker: ignored stale completion");
                return;
            }
            match path {
                Some(path) => app.set_status(format!("file picker selected {}", path.display())),
                None => app.set_status("file picker cancelled"),
            }
        }
    }
}


fn reduce_file_task_progress(
    app: &mut AppState,
    session_id: u64,
    update: tui_file_picker::FileTaskProgressUpdate,
    tx: &mpsc::Sender<AppMessage>,
) {
    let terminal = matches!(
        &update,
        tui_file_picker::FileTaskProgressUpdate::Finished { .. }
            | tui_file_picker::FileTaskProgressUpdate::Failed { .. }
            | tui_file_picker::FileTaskProgressUpdate::Aborted { .. }
    );
    let status = match &update {
        tui_file_picker::FileTaskProgressUpdate::SetScope { .. }
        | tui_file_picker::FileTaskProgressUpdate::UpdateConflictExistingStats { .. }
        | tui_file_picker::FileTaskProgressUpdate::ClearConflict => None,
        tui_file_picker::FileTaskProgressUpdate::ShowConflict { conflict } => {
            Some(format!("file task conflict: {}", conflict.title))
        }
        tui_file_picker::FileTaskProgressUpdate::Snapshot { status, .. }
        | tui_file_picker::FileTaskProgressUpdate::Finished { status, .. }
        | tui_file_picker::FileTaskProgressUpdate::Failed { status, .. }
        | tui_file_picker::FileTaskProgressUpdate::Aborted { status, .. } => Some(status.clone()),
        tui_file_picker::FileTaskProgressUpdate::RecordError { error, .. } => {
            Some(format!("file task error: {}", error.message))
        }
    };

    let mut status_to_set: Option<String> = None;
    let mut refresh_after_terminal = false;
    match &mut app.active_overlay {
        ActiveOverlay::FileTaskProgress(session) if session.session_id == session_id => {
            session.progress.apply_update(update);
            status_to_set = status;
            refresh_after_terminal = terminal;
        }
        ActiveOverlay::FileTaskProgress(_) => {
            status_to_set = Some(format!("file task: ignored stale progress for session {session_id}"));
        }
        _ if terminal => {
            status_to_set = status;
            refresh_after_terminal = true;
        }
        _ => {}
    }

    if let Some(status) = status_to_set {
        app.set_status(status);
    }
    if refresh_after_terminal {
        app.browse.refresh_with_search(Some(tx));
        app.browse.probe_current_with_db(tx, Some(&app.db));
        super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
    }
}


fn matching_file_picker_conflict_policy(
    app: &AppState,
    session_id: u64,
    purpose: &super::app::FilePickerPurpose,
) -> Option<tui_file_picker::ConflictPolicyPreset> {
    match &app.active_overlay {
        ActiveOverlay::FilePicker(session)
            if session.session_id == session_id && &session.purpose == purpose =>
        {
            session.picker.conflict_policy()
        }
        ActiveOverlay::MetadataEditor(state) => state
            .file_picker
            .as_ref()
            .filter(|picker| picker.session_id == session_id && &picker.purpose == purpose)
            .and_then(|picker| picker.picker.conflict_policy()),
        _ => None,
    }
}

fn close_matching_file_picker(
    app: &mut AppState,
    session_id: u64,
    purpose: &super::app::FilePickerPurpose,
) -> bool {
    let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
    let mut matched = false;
    match overlay {
        ActiveOverlay::FilePicker(session) => {
            matched = session.session_id == session_id && &session.purpose == purpose;
            if !matched {
                app.active_overlay = ActiveOverlay::FilePicker(session);
            }
        }
        ActiveOverlay::MetadataEditor(mut state) => {
            let matches_open_picker = state
                .file_picker
                .as_ref()
                .map(|picker| picker.session_id == session_id && &picker.purpose == purpose)
                .unwrap_or(false);
            if matches_open_picker {
                if matches!(purpose, super::app::FilePickerPurpose::SelectArtwork { .. }) {
                    if let Some(current_dir) = state
                        .file_picker
                        .as_ref()
                        .map(|picker| picker.current_dir().to_path_buf())
                    {
                        app.last_artwork_picker_dir = Some(current_dir);
                    }
                }
                state.file_picker = None;
                state.pending_artwork_type = None;
                matched = true;
            }
            app.active_overlay = ActiveOverlay::MetadataEditor(state);
        }
        other => {
            app.active_overlay = other;
        }
    }
    matched
}

fn handle_convert_source_probe_result(
    app: &mut AppState,
    generation: u64,
    path: std::path::PathBuf,
    source_mode: super::app::SourceMode,
    baseline: super::app::ConvertProbeBaseline,
) {
    if generation != app.probe_generation {
        return;
    }

    let metadata_unchanged = app.convert.metadata.editing.is_none()
        && super::app::ConvertProbeMetadataSnapshot::capture(&app.convert.metadata)
            == baseline.metadata;
    let format_unchanged =
        super::app::ConvertProbeFormatSnapshot::capture(&app.convert.format) == baseline.format;

    let detected_info_for_defaults = source_mode.current_info().cloned();
    let detected_metadata = source_mode.current_metadata();
    let probe_notice = source_mode
        .persistent_probe_notice()
        .map(std::borrow::ToOwned::to_owned);

    let mut applied = false;
    let is_batch_first = matches!(
        &app.convert.source.mode,
        super::app::SourceMode::Batch { paths, .. } if paths.first() == Some(&path)
    );

    if is_batch_first {
        if let super::app::SourceMode::Batch {
            paths,
            cursor,
            cursor_info,
            cursor_metadata,
            probe_notice: batch_notice,
            cursor_probe_notice,
            ..
        } = &mut app.convert.source.mode
        {
            // Batch format detection probes the first queued path. Cursor
            // movement during the worker is not stale: generation proves the
            // batch is still current. Only cursor-scoped preview fields update
            // when the cursor still points at the probed path.
            *batch_notice = probe_notice.clone();
            if paths.get(*cursor) == Some(&path) {
                *cursor_info = detected_info_for_defaults.clone();
                *cursor_metadata = detected_metadata.clone();
                *cursor_probe_notice = None;
            }
            applied = true;
        }
    } else if app.convert.source.mode.current_path() == Some(&path) {
        // The event loop installed only a cheap Single placeholder. The worker
        // may now have discovered that the source is actually a CUE/SACD/DVD/
        // Blu-ray multi-track source, so replace the whole mode with the fully
        // realized result rather than just filling info/metadata slots.
        if format_unchanged {
            app.convert.set_source_mode(source_mode);
        } else {
            app.convert
                .set_source_mode_preserving_format_selection(source_mode);
        }
        applied = true;
    }

    if !applied {
        return;
    }

    if metadata_unchanged {
        super::app::apply_source_metadata_to_convert(&mut app.convert, &detected_metadata);
    }

    if format_unchanged {
        if let Some(info) = detected_info_for_defaults.as_ref() {
            app.convert.apply_source_info_defaults(info);
        } else {
            app.convert.format.clear_source_derived_defaults();
        }
    } else if let Some(info) = detected_info_for_defaults.as_ref() {
        app.convert
            .refresh_source_info_constraints_preserving_format_selection(info);
    } else {
        app.convert
            .refresh_source_constraints_preserving_format_selection();
    }


    if let Some(notice) = probe_notice {
        app.set_status(format!("Probe warning: {}", notice));
    } else {
        app.set_status(format!(
            "Loaded: {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
}

fn handle_archive_preview_progress(
    app: &mut AppState,
    generation: u64,
    archive_path: std::path::PathBuf,
    message: String,
) {
    if generation != app.probe_generation
        || !app
            .convert
            .pending_archive_preview_matches(generation, &archive_path)
    {
        return;
    }

    if let super::app::SourceMode::Single {
        path, probe_notice, ..
    } = &mut app.convert.source.mode
    {
        if *path == archive_path {
            *probe_notice = Some(message);
        }
    }
}

fn handle_archive_preview_result(
    app: &mut AppState,
    generation: u64,
    archive_path: std::path::PathBuf,
    result: Result<super::app::ArchivePreview, String>,
    tx: &mpsc::Sender<AppMessage>,
    baseline: super::app::ConvertProbeBaseline,
) {
    let pending_matches = app
        .convert
        .pending_archive_preview_matches(generation, &archive_path);
    if generation != app.probe_generation
        || app.convert.source.mode.current_path() != Some(&archive_path)
        || !pending_matches
    {
        if let Ok(preview) = result {
            let _ = std::fs::remove_dir_all(preview.staging_dir);
        }
        return;
    }

    // The completed preview now owns the staging directory. Disarm the pending
    // handle before installing the completed SourceMode so set_source_mode()
    // does not cancel or remove the same directory.
    let _pending = app
        .convert
        .take_pending_archive_preview(generation, &archive_path);

    let metadata_unchanged = app.convert.metadata.editing.is_none()
        && super::app::ConvertProbeMetadataSnapshot::capture(&app.convert.metadata)
            == baseline.metadata;
    let format_unchanged =
        super::app::ConvertProbeFormatSnapshot::capture(&app.convert.format) == baseline.format;

    match result {
        Ok(preview) => {
            let track_count = preview.tracks.len();
            let source_mode = super::app::source_mode_from_archive_preview(preview);
            let detected_info_for_defaults = source_mode.current_info().cloned();
            let detected_metadata = source_mode.current_metadata();

            if format_unchanged {
                app.convert.set_source_mode(source_mode);
            } else {
                app.convert
                    .set_source_mode_preserving_format_selection(source_mode);
            }

            if metadata_unchanged {
                super::app::apply_source_metadata_to_convert(&mut app.convert, &detected_metadata);
            }

            if format_unchanged {
                if let Some(info) = detected_info_for_defaults.as_ref() {
                    app.convert.apply_source_info_defaults(info);
                } else {
                    app.convert.format.clear_source_derived_defaults();
                }
            } else if let Some(info) = detected_info_for_defaults.as_ref() {
                app.convert
                    .refresh_source_info_constraints_preserving_format_selection(info);
            } else {
                app.convert
                    .refresh_source_constraints_preserving_format_selection();
            }

            app.set_status(format!(
                "Archive preview loaded: {} track{}",
                track_count,
                if track_count == 1 { "" } else { "s" }
            ));
        }
        Err(err) => {
            if super::app::looks_like_archive_password_error(&err) {
                if let super::app::SourceMode::Single {
                    path,
                    probe_notice,
                    ..
                } = &mut app.convert.source.mode
                {
                    if *path == archive_path {
                        *probe_notice = Some(
                            "Archive is encrypted; enter password to preview".to_string(),
                        );
                    }
                }
                app.active_overlay = ActiveOverlay::TextEdit {
                    input: TextInputState::empty(),
                    target: TextEditTarget::ArchivePasswordForConvertPreview(
                        archive_path.clone(),
                    ),
                    label: "archive password".to_string(),
                };
                app.set_status(format!(
                    "Archive preview needs a password: {err}; enter password"
                ));
                let _ = tx;
                return;
            }

            let mut applied = false;
            if let super::app::SourceMode::Single { path, probe_notice, .. } = &mut app.convert.source.mode {
                if *path == archive_path {
                    *probe_notice = Some(format!(
                        "Archive preview failed: {}; set format manually",
                        err
                    ));
                    applied = true;
                }
            }
            if applied {
                if format_unchanged {
                    app.convert.format.clear_source_derived_defaults();
                } else {
                    app.convert
                        .refresh_source_constraints_preserving_format_selection();
                }
                app.set_status("Archive preview failed; set format manually");
            }
        }
    }
}


fn handle_archive_metadata_editor_progress(
    app: &mut AppState,
    archive_path: std::path::PathBuf,
    staging_dir: std::path::PathBuf,
    message: String,
) {
    let pending_matches = app
        .pending_browse_archive_metadata
        .as_ref()
        .is_some_and(|pending| pending.matches(&archive_path, &staging_dir));
    if pending_matches && app.current_screen == AppScreen::Browse {
        app.set_status(message);
    }
}

fn start_repackage_for_active_browse_staging(
    app: &mut AppState,
    archive_path: &std::path::Path,
    staging_dir: &std::path::Path,
    tx: &mpsc::Sender<AppMessage>,
) -> bool {
    let Some(staging) = app.browse.active_archive_staging().cloned() else {
        return false;
    };
    if !staging.dirty
        || staging.archive_path.as_path() != archive_path
        || staging.staging_dir.as_path() != staging_dir
    {
        return false;
    }

    let context = super::app::ArchiveMetadataEditContext::browse_active_staging_with_fingerprint(
        staging.archive_path.clone(),
        staging.staging_dir.clone(),
        staging.archive_mtime_secs,
        staging.archive_mtime_nanos,
        staging.archive_size,
        None,
    );
    start_browse_archive_repackage(app, context, tx);
    true
}

fn handle_archive_metadata_editor_prepared(
    app: &mut AppState,
    archive_path: std::path::PathBuf,
    staging_dir: std::path::PathBuf,
    result: Result<super::app::ArchiveMetadataEditorPayload, String>,
    tx: &mpsc::Sender<AppMessage>,
) {
    let pending_matches = app
        .pending_browse_archive_metadata
        .as_ref()
        .is_some_and(|pending| pending.matches(&archive_path, &staging_dir));
    if !pending_matches {
        // Stale completion from an editor-owned extraction can be cleaned up;
        // a completion against the active Browse staging must never remove the
        // Browse-owned deferred-save tree.
        let active_browse_owns_staging = app
            .browse
            .active_archive_staging()
            .is_some_and(|staging| {
                staging.archive_path == archive_path && staging.staging_dir == staging_dir
            });
        if !active_browse_owns_staging {
            super::app::cleanup_archive_metadata_staging_dir(&staging_dir);
        }
        app.set_status("archive metadata editor: ignored stale extraction result");
        return;
    }

    let pending_snapshot = app.pending_browse_archive_metadata.as_ref().cloned();
    let pending_owns_staging = pending_snapshot
        .as_ref()
        .map(|pending| pending.owns_staging)
        .unwrap_or(true);
    let pending_baseline = pending_snapshot.as_ref().map(|pending| {
        (
            pending.archive_mtime_secs,
            pending.archive_mtime_nanos,
            pending.archive_size,
        )
    });
    let pending_target_inner_paths = pending_snapshot
        .as_ref()
        .and_then(|pending| pending.target_inner_paths.clone());
    let deferred_screen_switch = app.deferred_browse_archive_screen_switch;
    let deferred_archive_exit = app.deferred_browse_archive_exit;

    if app.current_screen != AppScreen::Browse {
        let _pending = app.pending_browse_archive_metadata.take();
        if pending_owns_staging {
            super::app::cleanup_archive_metadata_staging_dir(&staging_dir);
            super::keybindings::delete_pending_archive_session_best_effort(app, &archive_path);
        }
        app.deferred_browse_archive_screen_switch = None;
        app.deferred_browse_archive_exit = false;
        app.set_status("archive metadata editor cancelled: Browse screen changed before extraction finished");
        return;
    }

    if !matches!(app.active_overlay, ActiveOverlay::None) {
        let _pending = app.pending_browse_archive_metadata.take();
        if pending_owns_staging {
            super::app::cleanup_archive_metadata_staging_dir(&staging_dir);
            super::keybindings::delete_pending_archive_session_best_effort(app, &archive_path);
        }
        app.deferred_browse_archive_screen_switch = None;
        app.deferred_browse_archive_exit = false;
        app.set_status("archive metadata editor cancelled: another overlay opened before extraction finished");
        return;
    }

    let _pending = app.pending_browse_archive_metadata.take();
    match result {
        Ok(payload) => {
            if deferred_screen_switch.is_some() || deferred_archive_exit {
                if pending_owns_staging {
                    // Opening metadata has not yet written any tags, so there
                    // are no user edits in this fresh staging tree. Complete
                    // the requested navigation and remove the temporary tree.
                    super::app::cleanup_archive_metadata_staging_dir(&staging_dir);
                    super::keybindings::delete_pending_archive_session_best_effort(app, &archive_path);
                    if let Some(target) = app.deferred_browse_archive_screen_switch.take() {
                        app.current_screen = target;
                        app.deferred_browse_archive_exit = false;
                        app.set_status("archive metadata edit cancelled before opening editor; screen switch completed".to_string());
                        return;
                    }
                    if app.deferred_browse_archive_exit {
                        app.deferred_browse_archive_exit = false;
                        app.browse.exit_archive();
                        app.set_status("archive metadata edit cancelled before opening editor; exited archive".to_string());
                        return;
                    }
                } else {
                    // The metadata editor was being prepared over an already
                    // dirty Browse-owned staging session. Do not open the
                    // editor after the user requested navigation; save the
                    // existing staged edits through the same deferred path.
                    if start_repackage_for_active_browse_staging(app, &archive_path, &staging_dir, tx) {
                        app.set_status("metadata editor cancelled; saving existing staged archive changes".to_string());
                        return;
                    }
                    app.deferred_browse_archive_screen_switch = None;
                    app.deferred_browse_archive_exit = false;
                    app.set_status("metadata editor cancelled; no active staged archive changes found".to_string());
                    return;
                }
            }
            super::keybindings::install_archive_metadata_editor_payload(
                app,
                archive_path,
                staging_dir,
                pending_baseline,
                pending_target_inner_paths,
                payload,
            );
        }
        Err(err) => {
            if pending_owns_staging {
                super::app::cleanup_archive_metadata_staging_dir(&staging_dir);
                super::keybindings::delete_pending_archive_session_best_effort(app, &archive_path);
                if deferred_screen_switch.is_none()
                    && !deferred_archive_exit
                    && super::app::looks_like_archive_password_error(&err)
                {
                    app.active_overlay = ActiveOverlay::TextEdit {
                        input: TextInputState::empty(),
                        target: TextEditTarget::ArchivePasswordForMetadataEdit {
                            archive_path: archive_path.clone(),
                            target_inner_paths: pending_snapshot
                                .as_ref()
                                .and_then(|pending| pending.target_inner_paths.clone()),
                        },
                        label: "archive password".to_string(),
                    };
                    app.pending_browse_archive_metadata = None;
                    app.set_status(format!(
                        "archive metadata editor needs archive password: {err}; enter password"
                    ));
                    return;
                }
                if let Some(target) = app.deferred_browse_archive_screen_switch.take() {
                    app.current_screen = target;
                    app.deferred_browse_archive_exit = false;
                    app.set_status(format!(
                        "archive metadata editor failed before screen switch completed: {err}"
                    ));
                    return;
                }
                if app.deferred_browse_archive_exit {
                    app.deferred_browse_archive_exit = false;
                    app.browse.exit_archive();
                    app.set_status(format!(
                        "archive metadata editor failed before archive exit completed: {err}"
                    ));
                    return;
                }
            } else if deferred_screen_switch.is_some() || deferred_archive_exit {
                if start_repackage_for_active_browse_staging(app, &archive_path, &staging_dir, tx) {
                    app.set_status(format!(
                        "metadata editor failed; saving existing staged archive changes: {err}"
                    ));
                    return;
                }
                app.deferred_browse_archive_screen_switch = None;
                app.deferred_browse_archive_exit = false;
            }
            app.set_status(format!("archive metadata editor failed: {err}"));
        }
    }
}

pub(super) fn start_browse_archive_repackage(
    app: &mut AppState,
    context: super::app::ArchiveMetadataEditContext,
    tx: &mpsc::Sender<AppMessage>,
) {
    start_browse_archive_repackage_inner(app, context, tx, false);
}

pub(super) fn start_browse_archive_repackage_overwrite(
    app: &mut AppState,
    context: super::app::ArchiveMetadataEditContext,
    tx: &mpsc::Sender<AppMessage>,
) {
    start_browse_archive_repackage_inner(app, context, tx, true);
}

fn start_browse_archive_repackage_inner(
    app: &mut AppState,
    context: super::app::ArchiveMetadataEditContext,
    tx: &mpsc::Sender<AppMessage>,
    overwrite_external_change: bool,
) {
    if app.browse_archive_repackage.is_some() {
        app.set_status("archive save already running; staged edits were preserved");
        return;
    }

    if let Some(preserved) = app.preserved_editor_archive_repackage.clone() {
        if preserved.archive_path == context.archive_path {
            if same_archive_repackage_context(&preserved, &context) {
                // Allowed retry of the preserved editor-owned staging. Keep the
                // preserved context installed until conflict checks pass and
                // the worker is actually launched; otherwise a conflict-check
                // cancellation could drop the only in-process retry/discard owner.
            } else {
                app.active_overlay = super::app::ActiveOverlay::Confirmation {
                    message: format!(
                        "A previous whole-archive metadata save is still staged for {}.\n\nY retries that save. D discards those staged edits. N/Esc keeps them for later. Resolve it before starting another metadata edit for this archive.",
                        preserved.archive_path.display()
                    ),
                    action: super::app::ConfirmAction::ArchiveRepackageFailure {
                        context: preserved.clone(),
                        error: "previous archive save was cancelled or failed before completion".to_string(),
                    },
                };
                app.set_status("metadata: resolve preserved archive staging before editing this archive again".to_string());
                return;
            }
        }
    }

    let archive_path = context.archive_path.clone();
    let staging_dir = context.staging_dir.clone();
    let tool_paths = app.manager.config.tool_paths.clone();
    let tx = tx.clone();
    // Mutation is now in progress. Bump the archive probe epoch immediately
    // so any archive-entry probe that was launched against the pre-edit
    // archive is rejected even if it completes before the final success path
    // clears cache/pending state.
    app.browse.bump_archive_probe_epoch_for(&archive_path);
    if matches!(context.owner, super::app::ArchiveMetadataEditOwner::Browse)
        && !overwrite_external_change
    {
        match context.archive_conflict() {
            Ok(false) => {}
            Ok(true) => {
                app.active_overlay = super::app::ActiveOverlay::Confirmation {
                    message: format!(
                        "Archive was modified externally: {}\n\nY overwrites it with your staged edits. D discards your staged edits. N/Esc keeps the staged edits for later retry. Mouse Cancel opens an explicit discard confirmation.",
                        archive_path.display()
                    ),
                    action: super::app::ConfirmAction::ArchiveExternalConflict { context },
                };
                app.set_status("archive save conflict: choose overwrite, discard, or keep staged edits".to_string());
                return;
            }
            Err(err) => {
                app.active_overlay = super::app::ActiveOverlay::Confirmation {
                    message: format!(
                        "Could not verify whether the archive changed externally: {}\n\nY attempts the save anyway. D discards your staged edits. N/Esc keeps the staged edits for later retry. Mouse Cancel opens an explicit discard confirmation.\n\n{}",
                        archive_path.display(),
                        err
                    ),
                    action: super::app::ConfirmAction::ArchiveExternalConflict { context },
                };
                app.set_status("archive save conflict check failed: staged edits kept".to_string());
                return;
            }
        }
    }
    let archive_label = archive_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| archive_path.display().to_string());
    let (control_tx, control_rx) = std::sync::mpsc::channel();
    let mut progress = tui_file_picker::FileTaskProgressState::new(
        tui_file_picker::FileTaskKind::Archive,
        "Repackaging archive",
        super::keybindings::file_picker_theme_from_theme(&app.theme),
    );
    progress.set_scope(tui_file_picker::FileTaskScope {
        source_root: Some(archive_path.clone()),
        source_summary: archive_label.clone(),
        destination: archive_path
            .parent()
            .map(|parent| parent.to_path_buf())
            .or_else(|| Some(archive_path.clone())),
        destination_summary: archive_path
            .parent()
            .map(|parent| parent.display().to_string()),
    });
    let session = super::app::FileTaskProgressSession::new(progress, control_tx);
    app.active_overlay = super::app::ActiveOverlay::FileTaskProgress(session);
    clear_preserved_editor_archive_repackage_context(app, &context);
    app.browse_archive_repackage = Some(context);
    app.set_status(format!("Saving archive changes: {archive_label}"));

    tokio::spawn(async move {
        let cancel = tokio_util::sync::CancellationToken::new();
        let control_done = tokio_util::sync::CancellationToken::new();
        let cancel_from_controls = cancel.clone();
        let done_from_worker = control_done.clone();
        let control_task = tokio::task::spawn_blocking(move || {
            while !done_from_worker.is_cancelled() {
                match control_rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(super::app::FileTaskControl::Abort) => {
                        cancel_from_controls.cancel();
                        break;
                    }
                    Ok(super::app::FileTaskControl::Pause)
                    | Ok(super::app::FileTaskControl::Resume)
                    | Ok(super::app::FileTaskControl::SkipCurrent)
                    | Ok(super::app::FileTaskControl::ConflictResolution { .. }) => {
                        // Archive repackaging is a single external-tool operation:
                        // abort is meaningful and wired to the worker; pause/skip
                        // are intentionally ignored rather than faking support.
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        });

        let progress_tx = tx.clone();
        let archive_for_progress = archive_path.clone();
        let staging_for_progress = staging_dir.clone();
        let result = crate::convert::pipeline::materializer_archive::repackage_archive_with_progress_and_cancel(
            &staging_dir,
            &archive_path,
            &tool_paths,
            &cancel,
            move |snapshot| {
                let _ = progress_tx.try_send(AppMessage::ArchiveRepackageProgress {
                    archive_path: archive_for_progress.clone(),
                    staging_dir: staging_for_progress.clone(),
                    snapshot,
                });
            },
        )
        .await;
        control_done.cancel();
        let _ = control_task.await;
        let _ = tx
            .send(AppMessage::ArchiveRepackageResult {
                archive_path,
                staging_dir,
                result,
            })
            .await;
    });
}

fn build_recovery_listing_from_staging(
    archive_path: &std::path::Path,
    staging_dir: &std::path::Path,
) -> Result<crate::tui::archive_listing::ArchiveListing, String> {
    let mut entries = Vec::new();
    for entry in walkdir::WalkDir::new(staging_dir).min_depth(1) {
        let entry = entry.map_err(|err| format!("walk staged archive tree: {err}"))?;
        let path = entry.path();
        let relative = path
            .strip_prefix(staging_dir)
            .map_err(|err| format!("staged archive path escaped staging root: {err}"))?;
        let normalized = relative
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("/");
        if normalized.is_empty() {
            continue;
        }
        let meta = entry
            .metadata()
            .map_err(|err| format!("stat staged archive entry {}: {err}", path.display()))?;
        entries.push(crate::tui::archive_listing::ArchiveEntry {
            path: normalized,
            size: if meta.is_dir() { 0 } else { meta.len() },
            packed_size: 0,
            is_dir: meta.is_dir(),
            encrypted: false,
        });
    }
    Ok(crate::tui::archive_listing::ArchiveListing {
        archive_path: archive_path.to_path_buf(),
        format: "staged-recovery".to_string(),
        physical_size: 0,
        entries,
    })
}

fn handle_archive_repackage_progress(
    app: &mut AppState,
    archive_path: std::path::PathBuf,
    staging_dir: std::path::PathBuf,
    snapshot: crate::convert::pipeline::materializer_archive::ArchiveRepackageProgressSnapshot,
) {
    let pending_matches = app
        .browse_archive_repackage
        .as_ref()
        .is_some_and(|context| {
            context.archive_path == archive_path && context.staging_dir == staging_dir
        });
    if !pending_matches {
        return;
    }

    let status = snapshot.status.clone();
    if let super::app::ActiveOverlay::FileTaskProgress(session) = &mut app.active_overlay {
        session
            .progress
            .apply_update(archive_repackage_file_task_update(snapshot));
    }
    app.set_status(status);
}

fn archive_repackage_file_task_update(
    snapshot: crate::convert::pipeline::materializer_archive::ArchiveRepackageProgressSnapshot,
) -> tui_file_picker::FileTaskProgressUpdate {
    let phase = match snapshot.stage {
        crate::convert::pipeline::materializer_archive::ArchiveRepackageStage::Validating => {
            tui_file_picker::FileTaskPhase::Preparing
        }
        crate::convert::pipeline::materializer_archive::ArchiveRepackageStage::Compressing => {
            tui_file_picker::FileTaskPhase::Running
        }
        crate::convert::pipeline::materializer_archive::ArchiveRepackageStage::Verifying => {
            tui_file_picker::FileTaskPhase::Verifying
        }
        crate::convert::pipeline::materializer_archive::ArchiveRepackageStage::PreservingMetadata
        | crate::convert::pipeline::materializer_archive::ArchiveRepackageStage::Installing => {
            tui_file_picker::FileTaskPhase::CleaningUp
        }
        crate::convert::pipeline::materializer_archive::ArchiveRepackageStage::Completed => {
            tui_file_picker::FileTaskPhase::Completed
        }
    };
    // The overlay scope already identifies the archive. The live row must show
    // the active repackage step (for example, "Compressing archive..."), not
    // the archive filename; otherwise the progress dialog regresses to an
    // archive-name-only display while the meaningful step remains in the status
    // bar. Keep the byte counters on the current row so the per-step bar and
    // ETA remain driven by the same snapshot.
    let status = snapshot.status;
    let current_item = Some(tui_file_picker::ProgressItem {
        label: status.clone(),
        source: None,
        destination: None,
        bytes_done: snapshot.bytes_done,
        bytes_total: snapshot.bytes_total,
    });
    tui_file_picker::FileTaskProgressUpdate::Snapshot {
        phase,
        status,
        current_item,
        totals: tui_file_picker::ProgressTotals {
            items_done: snapshot.items_done,
            items_total: snapshot.items_total,
            item_unit: tui_file_picker::ProgressUnit::Files,
            bytes_done: snapshot.bytes_done,
            bytes_total: snapshot.bytes_total,
            folders_done: 0,
            folders_total: Some(0),
            unknown_size_items: 0,
            completed: snapshot.items_done,
            skipped: 0,
            errors: 0,
            overwritten: 0,
            renamed: 0,
            merged: 0,
            not_attempted: 0,
        },
        rate_bytes_per_sec: snapshot.rate_bytes_per_sec,
    }
}

fn current_archive_repackage_totals(app: &AppState) -> tui_file_picker::ProgressTotals {
    match &app.active_overlay {
        super::app::ActiveOverlay::FileTaskProgress(session) => session.progress.totals,
        _ => tui_file_picker::ProgressTotals {
            items_done: 0,
            items_total: Some(1),
            item_unit: tui_file_picker::ProgressUnit::Files,
            bytes_done: 0,
            bytes_total: None,
            folders_done: 0,
            folders_total: Some(0),
            unknown_size_items: 0,
            completed: 0,
            skipped: 0,
            errors: 0,
            overwritten: 0,
            renamed: 0,
            merged: 0,
            not_attempted: 0,
        },
    }
}

fn apply_archive_repackage_terminal_update(
    app: &mut AppState,
    update: tui_file_picker::FileTaskProgressUpdate,
) {
    if let super::app::ActiveOverlay::FileTaskProgress(session) = &mut app.active_overlay {
        session.progress.apply_update(update);
    }
}

fn same_archive_repackage_context(
    left: &super::app::ArchiveMetadataEditContext,
    right: &super::app::ArchiveMetadataEditContext,
) -> bool {
    left.archive_path == right.archive_path && left.staging_dir == right.staging_dir
}

fn preserve_editor_owned_archive_repackage_context(
    app: &mut AppState,
    context: &super::app::ArchiveMetadataEditContext,
) {
    if context.editor_owns_staging {
        app.preserved_editor_archive_repackage = Some(context.clone());
    }
}

fn clear_preserved_editor_archive_repackage_context(
    app: &mut AppState,
    context: &super::app::ArchiveMetadataEditContext,
) {
    if app
        .preserved_editor_archive_repackage
        .as_ref()
        .is_some_and(|preserved| same_archive_repackage_context(preserved, context))
    {
        app.preserved_editor_archive_repackage = None;
    }
}

fn complete_browse_archive_metadata_save(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    context: super::app::ArchiveMetadataEditContext,
    saved_paths: &[std::path::PathBuf],
) -> Result<(), String> {
    super::keybindings::record_staged_archive_metadata_write(
        app,
        &context.archive_path,
        &context.staging_dir,
        super::keybindings::archive_metadata_context_baseline(&context),
        saved_paths,
    )?;
    if context.editor_owns_staging {
        start_browse_archive_repackage(app, context, tx);
    } else {
        app.browse.refresh_with_search(Some(tx));
        app.browse.probe_current_with_db(tx, Some(&app.db));
        app.set_status("metadata editor: saved staged archive tags; archive changes pending".to_string());
    }
    Ok(())
}

fn handle_archive_repackage_result(
    app: &mut AppState,
    archive_path: std::path::PathBuf,
    staging_dir: std::path::PathBuf,
    result: Result<crate::convert::pipeline::materializer_archive::ArchiveRepackageReport, String>,
    tx: &mpsc::Sender<AppMessage>,
) {
    let pending_matches = app
        .browse_archive_repackage
        .as_ref()
        .is_some_and(|context| {
            context.archive_path == archive_path && context.staging_dir == staging_dir
        });
    if !pending_matches {
        // Do not delete staging for a stale completion. A stale result can race
        // with retry/cancel flows, and the staged directory may still be the
        // only copy of accumulated archive edits.
        app.set_status("archive save: ignored stale repackage result; staged edits were preserved");
        return;
    }

    let Some(context) = app.browse_archive_repackage.take() else {
        app.set_status("archive save: missing repackage context; staged edits were preserved");
        return;
    };
    let quit_after_repackage = app.quit_after_browse_archive_repackage;
    app.quit_after_browse_archive_repackage = false;

    let mut terminal_totals = current_archive_repackage_totals(app);

    match result {
        Ok(report) => {
            terminal_totals.items_done = terminal_totals.items_total.unwrap_or(1);
            terminal_totals.completed = terminal_totals.items_done;
            if let Some(total) = terminal_totals.bytes_total {
                terminal_totals.bytes_done = total;
            }
            apply_archive_repackage_terminal_update(
                app,
                tui_file_picker::FileTaskProgressUpdate::Finished {
                    status: "Archive repackaged".to_string(),
                    totals: terminal_totals,
                },
            );
            let path_str = archive_path.display().to_string();
            let browse_holds_same_archive = app
                .browse
                .archive
                .as_ref()
                .is_some_and(|arc| arc.listing.archive_path == archive_path);
            clear_preserved_editor_archive_repackage_context(app, &context);
            context.cleanup_staging();
            app.invalidate_archive_listing_cache_for_path(&archive_path);
            app.browse.invalidate_archive_probe_cache_for(&archive_path);
            let _ = app.db.invalidate_probe(&path_str);
            let _ = app.db.delete_pending_archive_session(&archive_path);
            if browse_holds_same_archive {
                if let Some(arc) = app.browse.archive.as_mut() {
                    if arc.listing.archive_path == archive_path
                        && arc
                            .staging
                            .as_ref()
                            .is_some_and(|staging| staging.staging_dir == staging_dir)
                    {
                        arc.staging = None;
                    }
                }
                // A deferred archive save means the user has logically left the
                // archive view, even if the active screen changed before the
                // background repackage completed. Drop the staged archive view
                // only after the replacement has succeeded.
                app.browse.exit_archive();
            }
            app.deferred_browse_archive_exit = false;
            if quit_after_repackage {
                app.should_quit = true;
                app.deferred_browse_archive_screen_switch = None;
            } else if let Some(target) = app.deferred_browse_archive_screen_switch.take() {
                app.current_screen = target;
                if target == AppScreen::Browse {
                    app.browse.probe_current_with_db(tx, Some(&app.db));
                }
            } else {
                app.browse.probe_current_with_db(tx, Some(&app.db));
            }
            let archive_label = archive_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| archive_path.display().to_string());
            if let Some(warning) = report.warning_summary() {
                app.set_status(format!(
                    "Archive changes saved and repackaged: {archive_label}; warning: {warning}"
                ));
            } else {
                app.set_status(format!(
                    "Archive changes saved and repackaged: {archive_label}"
                ));
            }
        }
        Err(err) if crate::convert::pipeline::materializer_archive::is_archive_repackage_cancelled(&err) => {
            terminal_totals.not_attempted = terminal_totals
                .items_total
                .unwrap_or(1)
                .saturating_sub(terminal_totals.items_done);
            apply_archive_repackage_terminal_update(
                app,
                tui_file_picker::FileTaskProgressUpdate::Aborted {
                    status: "Archive repackage cancelled; staged edits preserved".to_string(),
                    totals: terminal_totals,
                },
            );
            app.deferred_browse_archive_exit = false;
            app.deferred_browse_archive_screen_switch = None;
            app.quit_after_browse_archive_repackage = false;
            app.should_quit = false;
            if context.editor_owns_staging {
                preserve_editor_owned_archive_repackage_context(app, &context);
                app.active_overlay = super::app::ActiveOverlay::Confirmation {
                    message: format!(
                        "Archive save was cancelled for {}.\n\nYour staged metadata edits are still preserved in this session and in the recovery database. Y retries the save. D discards the staged edits. N/Esc keeps them for later retry.",
                        archive_path.display()
                    ),
                    action: super::app::ConfirmAction::ArchiveRepackageFailure {
                        context,
                        error: "archive save cancelled".to_string(),
                    },
                };
            }
            app.set_status(format!(
                "archive save cancelled for {}; staged edits preserved for retry/discard",
                archive_path.display()
            ));
        }
        Err(err) => {
            terminal_totals.errors = 1;
            apply_archive_repackage_terminal_update(
                app,
                tui_file_picker::FileTaskProgressUpdate::Failed {
                    status: format!("Archive save failed: {err}"),
                    totals: terminal_totals,
                },
            );
            preserve_editor_owned_archive_repackage_context(app, &context);
            let message = format!(
                "Archive save failed for {}.\n\nY retries the save. D discards your staged edits. N/Esc keeps the staged edits for later retry. Mouse Cancel opens an explicit discard confirmation.\n\n{}",
                archive_path.display(),
                err
            );
            app.active_overlay = super::app::ActiveOverlay::Confirmation {
                message,
                action: super::app::ConfirmAction::ArchiveRepackageFailure {
                    context,
                    error: err.clone(),
                },
            };
            if quit_after_repackage {
                app.should_quit = false;
                app.set_status(format!(
                    "archive save failed for {}; quit cancelled; staged edits preserved: {err}",
                    archive_path.display()
                ));
            } else {
                app.set_status(format!(
                    "archive save failed; staged edits preserved for retry/discard: {err}"
                ));
            }
        }
    }
}


fn handle_archive_entry_rename_progress(
    app: &mut AppState,
    archive_path: std::path::PathBuf,
    staging_dir: std::path::PathBuf,
    message: String,
) {
    let pending_matches = app
        .pending_browse_archive_rename
        .as_ref()
        .is_some_and(|pending| pending.matches(&archive_path, &staging_dir));
    if pending_matches && app.current_screen == AppScreen::Browse {
        app.set_status(message);
    }
}

fn handle_archive_entry_rename_result(
    app: &mut AppState,
    archive_path: std::path::PathBuf,
    staging_dir: std::path::PathBuf,
    old_inner_path: String,
    new_inner_path: String,
    result: Result<(), String>,
    tx: &mpsc::Sender<AppMessage>,
) {
    let pending_matches = app
        .pending_browse_archive_rename
        .as_ref()
        .is_some_and(|pending| {
            pending.matches(&archive_path, &staging_dir)
                && pending.old_inner_path == old_inner_path
                && pending.new_inner_path == new_inner_path
        });
    if !pending_matches {
        super::app::cleanup_archive_metadata_staging_dir(&staging_dir);
        app.set_status("archive rename: ignored stale result");
        return;
    }

    let pending = app.pending_browse_archive_rename.take();
    let pending_baseline = pending.as_ref().map(|pending| {
        (
            pending.archive_mtime_secs,
            pending.archive_mtime_nanos,
            pending.archive_size,
        )
    });
    let quit_after_rename = app.quit_after_browse_archive_rename;
    app.quit_after_browse_archive_rename = false;

    match result {
        Ok(()) => {
            app.browse.invalidate_archive_probe_cache_for(&archive_path);
            let archive_label = archive_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| archive_path.display().to_string());
            let old_name = old_inner_path.rsplit('/').next().unwrap_or(old_inner_path.as_str());
            let new_name = new_inner_path.rsplit('/').next().unwrap_or(new_inner_path.as_str());
            let browse_holds_same_archive = app
                .browse
                .archive
                .as_ref()
                .is_some_and(|arc| arc.listing.archive_path == archive_path);
            let (secs, nanos, size) = pending_baseline
                .unwrap_or_else(|| super::app::archive_fingerprint(&archive_path).unwrap_or((0, 0, 0)));

            if browse_holds_same_archive {
                if let Some(arc) = app.browse.archive.as_mut() {
                    arc.staging = Some(super::browse::ArchiveStagingSession::new(
                        staging_dir.clone(),
                        archive_path.clone(),
                        secs,
                        nanos,
                        size,
                    ));
                }

                if let Err(err) = super::keybindings::rename_staged_archive_entry_transactional(
                    app,
                    &staging_dir,
                    &old_inner_path,
                    &new_inner_path,
                ) {
                    if let Some(arc) = app.browse.archive.as_mut() {
                        if arc
                            .staging
                            .as_ref()
                            .is_some_and(|staging| staging.staging_dir == staging_dir)
                        {
                            arc.staging = None;
                        }
                    }
                    super::app::cleanup_archive_metadata_staging_dir(&staging_dir);
                    super::keybindings::delete_pending_archive_session_best_effort(app, &archive_path);
                    app.deferred_browse_archive_exit = false;
                    app.deferred_browse_archive_screen_switch = None;
                    if quit_after_rename {
                        app.set_status(format!("archive rename failed; quit cancelled: {err}"));
                    } else {
                        app.set_status(format!("archive rename failed: {err}"));
                    }
                    return;
                }

                if app.current_screen == AppScreen::Browse
                    && app.deferred_browse_archive_screen_switch.is_none()
                    && !app.deferred_browse_archive_exit
                    && !quit_after_rename
                {
                    app.browse.refresh_with_search(Some(tx));
                    let target_path = archive_path.join(&new_inner_path);
                    if let Some(idx) = app.browse.entries.iter().position(|entry| entry.path == target_path) {
                        app.browse.selected_index = idx;
                        app.browse.ensure_visible();
                    }
                    app.set_status(format!(
                        "renamed archive entry in {archive_label}: {old_name} -> {new_name}; archive changes pending"
                    ));
                } else {
                    app.quit_after_browse_archive_repackage = quit_after_rename;
                    let context = super::app::ArchiveMetadataEditContext::browse_active_staging_with_fingerprint(
                        archive_path.clone(),
                        staging_dir.clone(),
                        secs,
                        nanos,
                        size,
                        None,
                    );
                    app.set_status(format!(
                        "renamed archive entry in {archive_label}: {old_name} -> {new_name}; saving archive changes"
                    ));
                    start_browse_archive_repackage(app, context, tx);
                }
            } else {
                // Browse no longer owns the archive view. Do not mutate the
                // extracted staging tree off-screen; keep the pre-registered
                // empty session for startup recovery rather than silently
                // applying a hidden rename.
                app.browse.refresh_with_search(Some(tx));
                app.set_status(format!(
                    "archive rename for {archive_label} was extracted after navigation; staged snapshot preserved for recovery without applying the rename"
                ));
                if quit_after_rename {
                    app.should_quit = true;
                }
            }
        }
        Err(err) => {
            super::app::cleanup_archive_metadata_staging_dir(&staging_dir);
            super::keybindings::delete_pending_archive_session_best_effort(app, &archive_path);
            app.deferred_browse_archive_exit = false;
            app.deferred_browse_archive_screen_switch = None;
            if quit_after_rename {
                app.set_status(format!("archive rename failed; quit cancelled: {err}"));
            } else {
                app.set_status(format!("archive rename failed: {err}"));
            }
        }
    }
}

fn handle_archive_entry_delete_progress(
    app: &mut AppState,
    archive_path: std::path::PathBuf,
    staging_dir: std::path::PathBuf,
    message: String,
) {
    let pending_matches = app
        .pending_browse_archive_delete
        .as_ref()
        .is_some_and(|pending| pending.matches(&archive_path, &staging_dir));
    if pending_matches && app.current_screen == AppScreen::Browse {
        app.set_status(message);
    }
}

fn handle_archive_entry_delete_result(
    app: &mut AppState,
    archive_path: std::path::PathBuf,
    staging_dir: std::path::PathBuf,
    inner_paths: Vec<String>,
    result: Result<(), String>,
    tx: &mpsc::Sender<AppMessage>,
) {
    let pending_matches = app
        .pending_browse_archive_delete
        .as_ref()
        .is_some_and(|pending| {
            pending.matches(&archive_path, &staging_dir) && pending.inner_paths == inner_paths
        });
    if !pending_matches {
        super::app::cleanup_archive_metadata_staging_dir(&staging_dir);
        app.set_status("archive delete: ignored stale result");
        return;
    }

    let pending = app.pending_browse_archive_delete.take();
    let pending_baseline = pending.as_ref().map(|pending| {
        (
            pending.archive_mtime_secs,
            pending.archive_mtime_nanos,
            pending.archive_size,
        )
    });
    let quit_after_delete = app.quit_after_browse_archive_delete;
    app.quit_after_browse_archive_delete = false;
    match result {
        Ok(()) => {
            app.browse.invalidate_archive_probe_cache_for(&archive_path);
            let browse_holds_same_archive = app
                .browse
                .archive
                .as_ref()
                .is_some_and(|arc| arc.listing.archive_path == archive_path);
            let (secs, nanos, size) = pending_baseline
                .unwrap_or_else(|| super::app::archive_fingerprint(&archive_path).unwrap_or((0, 0, 0)));

            if browse_holds_same_archive {
                if let Some(arc) = app.browse.archive.as_mut() {
                    arc.staging = Some(super::browse::ArchiveStagingSession::new(
                        staging_dir.clone(),
                        archive_path.clone(),
                        secs,
                        nanos,
                        size,
                    ));
                }
                if let Err(err) = super::keybindings::delete_staged_archive_entries_transactional(
                    &staging_dir,
                    &inner_paths,
                    || super::keybindings::append_archive_delete_edits_and_persist(app, &inner_paths),
                ) {
                    if let Some(arc) = app.browse.archive.as_mut() {
                        if arc
                            .staging
                            .as_ref()
                            .is_some_and(|staging| staging.staging_dir == staging_dir)
                        {
                            arc.staging = None;
                        }
                    }
                    super::app::cleanup_archive_metadata_staging_dir(&staging_dir);
                    super::keybindings::delete_pending_archive_session_best_effort(app, &archive_path);
                    app.deferred_browse_archive_exit = false;
                    app.deferred_browse_archive_screen_switch = None;
                    if quit_after_delete {
                        app.set_status(format!("archive delete failed; quit cancelled: {err}"));
                    } else {
                        app.set_status(format!("archive delete failed: {err}"));
                    }
                    return;
                }

                if quit_after_delete
                    || app.deferred_browse_archive_screen_switch.is_some()
                    || app.deferred_browse_archive_exit
                {
                    let context = super::app::ArchiveMetadataEditContext::browse_active_staging_with_fingerprint(
                        archive_path.clone(),
                        staging_dir.clone(),
                        secs,
                        nanos,
                        size,
                        None,
                    );
                    app.quit_after_browse_archive_repackage = quit_after_delete;
                    start_browse_archive_repackage(app, context, tx);
                } else {
                    app.browse.refresh_with_search(Some(tx));
                }
            } else {
                // Browse no longer owns the archive view. Do not apply hidden
                // delete edits off-screen; keep the pre-registered extracted
                // snapshot for explicit startup recovery.
                if quit_after_delete {
                    app.should_quit = true;
                }
            }

            let archive_label = archive_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| archive_path.display().to_string());
            let count = inner_paths.len();
            if app
                .browse_archive_repackage
                .as_ref()
                .is_some_and(|context| context.archive_path == archive_path && context.staging_dir == staging_dir)
            {
                app.set_status(format!(
                    "deleted {count} staged archive entr{} in {archive_label}; saving archive changes",
                    if count == 1 { "y" } else { "ies" }
                ));
            } else if browse_holds_same_archive {
                app.set_status(format!(
                    "deleted {count} staged archive entr{} in {archive_label}; archive changes pending",
                    if count == 1 { "y" } else { "ies" }
                ));
            } else {
                app.set_status(format!(
                    "archive delete for {archive_label} was extracted after navigation; staged snapshot preserved for recovery without applying the delete"
                ));
            }
            if quit_after_delete && app.browse_archive_repackage.is_none() {
                app.should_quit = true;
            }
        }
        Err(err) => {
            super::app::cleanup_archive_metadata_staging_dir(&staging_dir);
            super::keybindings::delete_pending_archive_session_best_effort(app, &archive_path);
            app.deferred_browse_archive_exit = false;
            app.deferred_browse_archive_screen_switch = None;
            if quit_after_delete {
                app.set_status(format!("archive delete failed; quit cancelled: {err}"));
            } else {
                app.set_status(format!("archive delete failed: {err}"));
            }
        }
    }
}

fn handle_convert_audio_probe_complete(
    app: &mut AppState,
    generation: u64,
    path: std::path::PathBuf,
    info: Option<super::probe::SourceInfo>,
    metadata: super::probe::SourceMetadata,
    probe_notice: Option<String>,
    baseline: super::app::ConvertProbeBaseline,
) {
    if generation != app.probe_generation {
        return;
    }

    let format_unchanged =
        super::app::ConvertProbeFormatSnapshot::capture(&app.convert.format) == baseline.format;

    if app.convert.source.batch_probe_pending.as_ref() == Some(&path) {
        app.convert.source.batch_probe_pending = None;
    }

    let mut applied = false;
    if let super::app::SourceMode::Batch {
        paths,
        cursor,
        cursor_info,
        cursor_metadata,
        cursor_probe_notice,
        ..
    } = &mut app.convert.source.mode
    {
        if paths.get(*cursor) == Some(&path) {
            *cursor_info = info.clone();
            *cursor_metadata = if info.is_some() {
                metadata
            } else {
                super::probe::SourceMetadata::default()
            };
            *cursor_probe_notice = probe_notice.clone().map(|notice| (path.clone(), notice));
            applied = true;
        }
    }

    if !applied {
        return;
    }

    if format_unchanged {
        if let Some(info) = info.as_ref() {
            app.convert.apply_source_info_defaults(info);
        } else {
            app.convert.format.clear_source_derived_defaults();
        }
    } else if let Some(info) = info.as_ref() {
        app.convert
            .refresh_source_info_constraints_preserving_format_selection(info);
    } else {
        app.convert
            .refresh_source_constraints_preserving_format_selection();
    }


    if let Some(notice) = probe_notice {
        app.set_status(format!("Probe warning: {}", notice));
    }
}

pub(super) fn handle_message(app: &mut AppState, msg: AppMessage, tx: &mpsc::Sender<AppMessage>) {
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
        AppMessage::BrowseConvertExpansionComplete {
            generation,
            request,
            expansion,
        } => {
            super::command::handle_browse_convert_expansion_complete(
                app,
                tx,
                generation,
                request,
                expansion,
            );
        }
        AppMessage::ProbeResult {
            generation,
            path,
            source_mode,
            baseline,
        } => {
            handle_convert_source_probe_result(app, generation, path, source_mode, baseline);
        }
        AppMessage::ArchivePreviewProgress {
            generation,
            archive_path,
            message,
        } => {
            handle_archive_preview_progress(app, generation, archive_path, message);
        }
        AppMessage::ArchivePreviewResult {
            generation,
            archive_path,
            result,
            baseline,
        } => {
            handle_archive_preview_result(app, generation, archive_path, result, tx, baseline);
        }
        AppMessage::ArchiveMetadataEditorProgress {
            archive_path,
            staging_dir,
            message,
        } => {
            handle_archive_metadata_editor_progress(app, archive_path, staging_dir, message);
        }
        AppMessage::ArchiveMetadataEditorPrepared {
            archive_path,
            staging_dir,
            result,
        } => {
            handle_archive_metadata_editor_prepared(app, archive_path, staging_dir, result, tx);
        }
        AppMessage::ArchiveRepackageProgress {
            archive_path,
            staging_dir,
            snapshot,
        } => {
            handle_archive_repackage_progress(app, archive_path, staging_dir, snapshot);
        }
        AppMessage::ArchiveRepackageResult {
            archive_path,
            staging_dir,
            result,
        } => {
            handle_archive_repackage_result(app, archive_path, staging_dir, result, tx);
        }
        AppMessage::ArchiveEntryRenameProgress {
            archive_path,
            staging_dir,
            message,
        } => {
            handle_archive_entry_rename_progress(app, archive_path, staging_dir, message);
        }
        AppMessage::ArchiveEntryRenameResult {
            archive_path,
            staging_dir,
            old_inner_path,
            new_inner_path,
            result,
        } => {
            handle_archive_entry_rename_result(
                app,
                archive_path,
                staging_dir,
                old_inner_path,
                new_inner_path,
                result,
                tx,
            );
        }
        AppMessage::ArchiveEntryDeleteProgress {
            archive_path,
            staging_dir,
            message,
        } => {
            handle_archive_entry_delete_progress(app, archive_path, staging_dir, message);
        }
        AppMessage::ArchiveEntryDeleteResult {
            archive_path,
            staging_dir,
            inner_paths,
            result,
        } => {
            handle_archive_entry_delete_result(app, archive_path, staging_dir, inner_paths, result, tx);
        }
        AppMessage::ConvertAudioProbeComplete {
            generation,
            path,
            info,
            metadata,
            probe_notice,
            baseline,
        } => {
            handle_convert_audio_probe_complete(
                app,
                generation,
                path,
                info,
                metadata,
                probe_notice,
                baseline,
            );
        }
        AppMessage::ProbeCacheWarmComplete { generation, path, rows } => {
            // Do not merge here. Queue rows and let the post-drain flush merge a
            // bounded slice, generation/path-checked again, so warm-cache bursts
            // cannot create a long reducer frame.
            let queued = app.browse.enqueue_probe_cache_warm_rows(generation, path.clone(), rows);
            if queued == 0 {
                log::debug!("discarded stale or empty probe-cache warm result for generation {} path {}", generation, path.display());
            }
        }
        AppMessage::AudioProbeComplete { path, context, result } => {
            let was_pending = app.browse.probe_pending.remove(&path);
            if matches!(&context, super::message::AudioProbeContext::Filesystem { .. }) {
                app.browse.complete_browse_cold_probe(&path, tx);
            }
            app.browse.probe_cache_needs_metadata_enrichment.remove(&path);

            let filesystem_identity = match &context {
                super::message::AudioProbeContext::Filesystem { identity } => {
                    if !was_pending {
                        return;
                    }
                    *identity
                },
                super::message::AudioProbeContext::ArchiveEntry {
                    archive_path,
                    archive_probe_epoch,
                } => {
                    if !app.browse.accept_archive_entry_probe_completion(
                        &path,
                        archive_path,
                        *archive_probe_epoch,
                        was_pending,
                    ) {
                        log::debug!(
                            "discarded stale archive-entry probe completion for {}",
                            path.display()
                        );
                        return;
                    }
                    None
                }
            };

            if let Some(identity) = filesystem_identity {
                match app.browse.classify_filesystem_async_completion(&path, identity) {
                    crate::tui::browse::FilesystemAsyncCompletion::Accept => {}
                    crate::tui::browse::FilesystemAsyncCompletion::Changed => {
                        log::debug!("dropping stale audio-probe completion after identity change: {}", path.display());
                        // The path still exists but no longer has the launch
                        // identity. Drop the stale completion, invalidate derived
                        // state, and re-evaluate only if this same statable path
                        // is still selected. This gives changed files one fresh
                        // pass without letting old completions loop forever.
                        app.browse.remove_probe_cache_entry(&path);
                        if app.browse.current_entry_is_still_statable(&path) {
                            app.browse.probe_current_with_db(tx, Some(&app.db));
                        }
                        return;
                    }
                    crate::tui::browse::FilesystemAsyncCompletion::MissingOrUnstatable => {
                        log::debug!("dropping audio-probe completion for missing/unstatable path: {}", path.display());
                        // The selected file disappeared or became unreadable.
                        // Clear stale state and stop probing until scan/refresh
                        // supplies a valid BrowseEntry again.
                        app.browse.remove_probe_cache_entry(&path);
                        app.browse.probe_debounce = None;
                        app.browse.clear_browse_cold_probe_queue();
                        app.browse.clear_browse_cold_probe_tracking_for(&path);
                        return;
                    }
                }
            }

            match *result {
                Ok(mut info) => {
                    let is_cue_proxy_result = super::browse::is_cue_sheet_path(&path);
                    if !is_cue_proxy_result {
                        // Generic browse probes own only browse/cache state.
                        // Convert-affecting probes use ConvertAudioProbeComplete,
                        // which carries generation and baseline guards. Keeping
                        // this reducer browse-only prevents late navigation probes
                        // from resetting Convert metadata or output settings.
                        let metadata = std::fs::metadata(&path).ok();
                        if let Some(meta) = metadata.as_ref() {
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

                        let cache_identity = filesystem_identity
                            .or_else(|| app.browse.probe_identity_for_current_entry_path(&path))
                            .unwrap_or(crate::tui::browse::ProbeCacheIdentity {
                                modified: None,
                                size: info.source.file_size,
                            });
                        app.browse.insert_probe_for_identity(
                            path.clone(),
                            cache_identity,
                            Some(std::sync::Arc::new(info.clone())),
                        );
                        app.browse.mark_probe_cache_update_pending(false);

                        if let Some(meta) = metadata.as_ref() {
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
                }
                Err(_) if super::browse::is_cue_sheet_path(&path) => {
                    // CUE proxy failures are Convert-source facts only when
                    // launched through ConvertAudioProbeComplete. Generic browse
                    // cache entries remain untouched for text CUE paths.
                }
                Err(error) => {
                    let cache_identity = filesystem_identity
                        .or_else(|| app.browse.probe_identity_for_current_entry_path(&path))
                        .unwrap_or(crate::tui::browse::ProbeCacheIdentity {
                            modified: None,
                            size: 0,
                        });
                    app.browse.remember_probe_failure_for_identity(path, cache_identity, &error);
                    app.browse.mark_probe_cache_update_pending(false);
                }
            }
        }
        AppMessage::DiscProbeComplete { path, fingerprint, result } => {
            if !app.browse.complete_disc_probe(&path) {
                return;
            }
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
                        if app.browse.current_selected_disc_source_matches(&path) {
                            app.browse.deferred_work.info_pane_changed = true;
                        }
                        if let Some(followup) = app.browse.disc_probe_followup.remove(&path) {
                            super::disc_browser_actions::handle_disc_probe_followup(app, &path, followup);
                        }
                    } else {
                        log::debug!(
                            "discarded stale disc-probe completion after fingerprint change: {}",
                            path.display()
                        );
                        app.browse.disc_probe_followup.remove(&path);
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
                        if app.browse.current_selected_disc_source_matches(&path) {
                            app.browse.deferred_work.info_pane_changed = true;
                            app.set_status(format!("Disc analysis failed: {error}"));
                        }
                    } else {
                        log::debug!(
                            "discarded stale disc-probe error after fingerprint change: {}",
                            path.display()
                        );
                    }
                    app.browse.disc_probe_followup.remove(&path);
                }
                (None, Err(error)) => {
                    app.browse.disc_probe_cache.remove(&path);
                    app.browse.disc_probe_followup.remove(&path);
                    if app.browse.current_selected_disc_source_matches(&path) {
                        app.set_status(format!("Disc analysis failed: {error}"));
                    } else {
                        log::debug!(
                            "discarded stale disc-probe launch error without fingerprint: {}",
                            path.display()
                        );
                    }
                }
                (None, Ok(contents)) => {
                    if let Ok(fingerprint) = crate::tui::disc_browser::disc_probe_fingerprint(&path) {
                        app.browse.disc_probe_cache.insert(
                            path.clone(),
                            crate::tui::disc_browser::DiscProbeCacheEntry::from_success(fingerprint, contents),
                        );
                        if app.browse.current_selected_disc_source_matches(&path) {
                            app.browse.deferred_work.info_pane_changed = true;
                        }
                    }
                }
            }
        }
        AppMessage::DirStatsComplete { path, identity, stats, cancelled } => {
            let was_pending = app.browse.complete_dir_stats(&path, tx);
            if !was_pending {
                return;
            }
            if cancelled {
                log::debug!("dropping cancelled directory-stats completion: {}", path.display());
                return;
            }
            match app.browse.classify_filesystem_async_completion(&path, identity) {
                crate::tui::browse::FilesystemAsyncCompletion::Accept => {
                    app.browse.insert_dir_stats_for_identity(path.clone(), identity, stats);
                    app.browse.store_directory_summary_for_identity_best_effort(&path, identity, &app.db);
                }
                crate::tui::browse::FilesystemAsyncCompletion::Changed => {
                    log::debug!("dropping stale directory-stats completion after identity change: {}", path.display());
                    app.browse.remove_dir_stats_cache_entry(&path);
                    app.browse.invalidate_directory_summary_persistent_cache_best_effort(&path, &app.db);
                    if app.browse.current_entry_is_still_statable(&path) {
                        app.browse.probe_current_with_db(tx, Some(&app.db));
                    }
                }
                crate::tui::browse::FilesystemAsyncCompletion::MissingOrUnstatable => {
                    log::debug!("dropping directory-stats completion for missing/unstatable path: {}", path.display());
                    app.browse.remove_dir_stats_cache_entry(&path);
                    app.browse.invalidate_directory_summary_persistent_cache_best_effort(&path, &app.db);
                }
            }
        }
        AppMessage::FolderClassifyComplete { path, identity, classification } => {
            let was_pending = app.browse.complete_folder_classification(&path);
            if !was_pending {
                return;
            }
            match app.browse.classify_filesystem_async_completion(&path, identity) {
                crate::tui::browse::FilesystemAsyncCompletion::Accept => {
                    let is_current = app.browse.is_current_entry_path(&path);
                    app.browse.insert_folder_classification_for_identity(path.clone(), identity, classification);
                    app.browse.store_directory_summary_for_identity_best_effort(&path, identity, &app.db);
                    if is_current {
                        // Re-enter the cheap current-entry policy path after
                        // caching the classification. This lets cached summary
                        // facts decide what, if any, follow-up work is useful:
                        // classified Disc probes or recursive directory stats
                        // for directory-like summaries that need concrete counts.
                        app.browse.probe_current_with_db(tx, Some(&app.db));
                        app.browse.deferred_work.info_pane_changed = true;
                    } else {
                        log::debug!("cached folder classification for non-current selection: {}", path.display());
                    }
                }
                crate::tui::browse::FilesystemAsyncCompletion::Changed => {
                    log::debug!("dropping stale folder classification after identity change: {}", path.display());
                    app.browse.remove_folder_classification_cache_entry(&path);
                    app.browse.invalidate_directory_summary_persistent_cache_best_effort(&path, &app.db);
                    if app.browse.current_entry_is_still_statable(&path) {
                        app.browse.probe_current_with_db(tx, Some(&app.db));
                    }
                }
                crate::tui::browse::FilesystemAsyncCompletion::MissingOrUnstatable => {
                    log::debug!("dropping folder classification for missing/unstatable path: {}", path.display());
                    app.browse.remove_folder_classification_cache_entry(&path);
                    app.browse.invalidate_directory_summary_persistent_cache_best_effort(&path, &app.db);
                }
            }
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

                    // Update probe cache with HDCD info only when the
                    // cached row still matches the file identity observed at
                    // completion time. AnalysisComplete does not carry a launch
                    // token in this bundle, so never refresh a stale path-only
                    // row from it.
                    if result.hdcd_detected == Some(true) {
                        if let Ok(meta) = std::fs::metadata(&result.path) {
                            let identity = crate::tui::browse::ProbeCacheIdentity::from_metadata(&meta);
                            app.browse.update_valid_probe_for_identity(&result.path, identity, |cached| {
                                cached.metadata.hdcd_detail = result.hdcd_detail.clone();
                            });
                        } else {
                            app.browse.remove_probe_cache_entry(&result.path);
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
        AppMessage::PathValidationComplete {
            generation,
            origin_dir,
            input,
            result,
        } => {
            if !app.browse.is_current_path_validation(generation, &origin_dir) {
                return;
            }
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
        },
        AppMessage::DirScanComplete {
            generation,
            path,
            parent_entry,
            dirs,
            files,
            classification_updates,
            error,
        } => {
            // Race protection: discard stale scans, including stale successful
            // scans for the same directory. Directory path alone is not a
            // sufficient identity because cancelled scans can still complete
            // after a newer refresh has started.
            if !app.browse.is_current_dir_scan(generation, &path) {
                log::debug!("discarded stale directory scan completion for generation {} path {}", generation, path.display());
                app.browse
                    .clear_pending_inline_rename_after_scan_generation(generation);
                return;
            }

            if let Some(err) = error {
                app.browse.finish_dir_scan_if_current(generation, &path);
                app.browse
                    .clear_pending_inline_rename_after_scan_generation(generation);
                app.browse.error = Some(err);
                return;
            }

            // Success — clear the scan handle.
            app.browse.finish_dir_scan_if_current(generation, &path);

            // The blocking scan worker performs cold disc-source classification
            // before publication and returns identity-bound cache updates. This
            // preserves the old stable one-batch display semantics without
            // doing ISO/DVD-A/DVD-Video/Blu-ray filesystem I/O in the reducer.
            app.browse.apply_classification_cache_updates(classification_updates);
            app.browse.publish_scanned_entries(parent_entry, dirs, files);

            // Warm the in-memory probe cache from SQLite off the reducer path.
            // Large folders must publish promptly; warm rows merge later only
            // if this generation/path remains current.
            app.browse
                .spawn_probe_cache_warm_from_db(generation, path.clone(), tx.clone());

            // Apply current filter/sort. While search is active, the search
            // panel owns `entries`; re-run the active search against the
            // refreshed scan data instead of publishing the ordinary Browse
            // listing under an open search UI.
            app.browse.reapply_after_directory_scan_complete(Some(tx));

            // Cursor restoration (e.g., after go_parent).
            if let Some(target) = app.browse.cursor_restore_target.take() {
                if let Some(idx) = app.browse.entries.iter().position(|e| e.name == target) {
                    app.browse.selected_index = idx;
                    app.browse.ensure_visible();
                }
            }

            // Sequential inline rename continuation (Tab / Shift+Tab). Filesystem
            // rename refreshes are async in the normal TUI runtime and clear
            // entries immediately, so resume the captured next/previous target
            // only after the same directory scan publishes the new view.
            let resume_inline_rename_target = app
                .browse
                .take_inline_rename_after_scan_target(generation, &path);
            let resumed_inline_rename = if let Some(target_path) = resume_inline_rename_target {
                super::keybindings::begin_browse_inline_rename(app, target_path);
                true
            } else {
                false
            };

            // Probe the newly selected entry unless it is being edited inline;
            // the next probe will run on commit/cursor movement.
            if !resumed_inline_rename {
                app.browse.probe_current_with_db(tx, Some(&app.db));
                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
            }
        }
        AppMessage::MetadataWriteComplete {
            path,
            field,
            value,
            result,
        } => {
            // Step 3 (main thread): cleanup journal + backup, invalidate caches.
            let path_str = path.display().to_string();
            let backup = crate::db::Database::backup_path_for(&path);
            match result {
                Ok(()) => {
                    let _ = app.db.complete_metadata_write(&path_str);
                    let _ = std::fs::remove_file(&backup);
                    app.browse.remove_probe_cache_entry(&path);
                    app.browse.probe_pending.remove(&path);
                    app.browse.invalidate_search_tag_cache_for_metadata_path(&path);
                    let _ = app.db.invalidate_probe(&path_str);
                    let staged_tracking_error = {
                        let staging = app.browse.active_archive_staging().cloned();
                        if let Some(staging) = staging {
                            if path.strip_prefix(&staging.staging_dir).is_ok() {
                                let change = super::keybindings::StagedArchiveMetadataChange::field(
                                    path.clone(),
                                    field.label().to_string(),
                                    value.clone(),
                                );
                                super::keybindings::record_staged_archive_metadata_changes(
                                    app,
                                    &staging.archive_path,
                                    &staging.staging_dir,
                                    Some((
                                        staging.archive_mtime_secs,
                                        staging.archive_mtime_nanos,
                                        staging.archive_size,
                                    )),
                                    &[change],
                                )
                                .err()
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    };
                    app.browse.probe_current_with_db(tx, Some(&app.db));
                    if let Some(err) = staged_tracking_error {
                        app.set_status(format!(
                            "{}: {} updated, but archive recovery tracking failed: {}",
                            path.file_name().unwrap_or_default().to_string_lossy(),
                            field.label(),
                            err,
                        ));
                    } else {
                        app.set_status(format!(
                            "{}: {} updated",
                            path.file_name().unwrap_or_default().to_string_lossy(),
                            field.label(),
                        ));
                    }
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
        AppMessage::ArchiveListingProgress {
            id,
            archive_path,
            message,
        } => {
            if app.archive_listing_pending_for(id, &archive_path) {
                app.set_status(message);
            }
        }
        AppMessage::ArchiveListingComplete {
            id,
            archive_path,
            cache_key,
            result,
            password,
        } => {
            if !app.complete_archive_listing(id, &archive_path) {
                return;
            }
            match *result {
                Ok(listing) => {
                    let count = listing.entries.len();
                    if let Some(key) = cache_key {
                        let _ = app.insert_archive_listing_cache(key, listing.clone());
                    }
                    if app
                        .browse
                        .archive
                        .as_ref()
                        .is_some_and(|arc| arc.listing.archive_path == archive_path)
                    {
                        app.browse.replace_active_archive_listing_with_search(listing, password, Some(tx));
                    } else {
                        app.browse.enter_archive(listing, password);
                    }
                    let resumed_recovery = app
                        .pending_archive_recovery_resume
                        .as_ref()
                        .is_some_and(|session| session.archive_path == archive_path);
                    if resumed_recovery {
                        if let Some(session) = app.pending_archive_recovery_resume.take() {
                            if let Some(arc) = app.browse.archive.as_mut() {
                                if arc.listing.archive_path == archive_path {
                                    arc.staging = Some(session);
                                }
                            }
                            app.browse.refresh_archive_view_with_search(Some(tx));
                        }
                    }
                    app.browse.probe_current_with_db(tx, Some(&app.db));
                    if resumed_recovery {
                        if app.pending_archive_recovery_resume_conflicted {
                            app.set_status(format!(
                                "Recovered staged archive edits for {}; archive changed externally, save will require overwrite/discard choice",
                                archive_path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default(),
                            ));
                            app.pending_archive_recovery_resume_conflicted = false;
                        } else {
                            app.set_status(format!(
                                "Recovered staged archive edits for {} ({} entries)",
                                archive_path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default(),
                                count,
                            ));
                        }
                    } else {
                        app.set_status(format!(
                            "Opened {} ({} entries)",
                            archive_path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default(),
                            count,
                        ));
                    }
                }
                Err(e) => {
                    if app
                        .pending_archive_recovery_resume
                        .as_ref()
                        .is_some_and(|session| session.archive_path == archive_path)
                    {
                        if let Some(session) = app.pending_archive_recovery_resume.take() {
                            match build_recovery_listing_from_staging(&archive_path, &session.staging_dir) {
                                Ok(listing) => {
                                    app.browse.enter_archive(listing, password);
                                    if let Some(arc) = app.browse.archive.as_mut() {
                                        arc.staging = Some(session);
                                    }
                                    app.browse.refresh_archive_view_with_search(Some(tx));
                                    let conflict_note = if app.pending_archive_recovery_resume_conflicted {
                                        "; original archive needs overwrite/discard review before save"
                                    } else {
                                        ""
                                    };
                                    app.pending_archive_recovery_resume_conflicted = false;
                                    app.set_status(format!(
                                        "Recovered staged archive edits from staging{}: {}",
                                        conflict_note,
                                        archive_path.display()
                                    ));
                                    app.browse.probe_current_with_db(tx, Some(&app.db));
                                    return;
                                }
                                Err(recovery_err) => {
                                    app.pending_archive_recovery_resume_conflicted = false;
                                    app.set_status(format!(
                                        "Archive recovery failed: could not list archive ({e}) or staged tree ({recovery_err}); staged edits remain on disk"
                                    ));
                                    return;
                                }
                            }
                        }
                        app.pending_archive_recovery_resume_conflicted = false;
                    }
                    if super::app::looks_like_archive_password_error(&e) {
                        app.active_overlay = ActiveOverlay::TextEdit {
                            input: TextInputState::empty(),
                            target: TextEditTarget::ArchivePassword(archive_path.clone()),
                            label: "archive password".to_string(),
                        };
                        app.set_status(format!("Archive error: {}; enter password", e));
                    } else {
                        app.set_status(format!("Archive error: {}", e));
                    }
                }
            }
        },
        AppMessage::SearchComplete {
            generation,
            root,
            recursive,
            archive_path,
            archive_inner_path,
            query,
            mode,
            show_hidden,
            audio_only,
            format_filter,
            sort,
            sort_dir,
            result_cap,
            total_matches,
            pre_sorted,
            archive_tag_cache_updates,
            results,
        } => {
            if !app.browse.search.active {
                return; // Search was closed while task was running.
            }

            let current_query = app.browse.search.input.text.trim().to_ascii_lowercase();
            let current_cap = app.browse.search_result_cap.max(1);
            let current_archive_path = app
                .browse
                .archive
                .as_ref()
                .map(|archive| archive.listing.archive_path.clone());
            let current_archive_inner_path = app
                .browse
                .archive
                .as_ref()
                .map(|archive| archive.inner_path.clone());
            if generation != app.browse.search.generation
                || root != app.browse.current_dir
                || recursive != app.browse.search.recursive
                || archive_path != current_archive_path
                || archive_inner_path != current_archive_inner_path
                || query != current_query
                || mode != app.browse.search.mode
                || show_hidden != app.browse.show_hidden
                || audio_only != app.browse.search.audio_only
                || format_filter != app.browse.format_filter
                || sort != app.browse.search.sort
                || sort_dir != app.browse.search.sort_dir
                || result_cap != current_cap
            {
                log::debug!("discarded stale Browse search completion for generation {} root {}", generation, root.display());
                return;
            }

            app.browse.search.searching = false;
            app.browse.search.cancel = None;
            for (synthetic_path, archive_fingerprint, password_identity, tags) in archive_tag_cache_updates {
                app.browse.search.archive_tag_cache.insert(
                    synthetic_path,
                    super::browse::CachedArchiveTagSearchString {
                        archive_fingerprint,
                        password_identity,
                        tags,
                    },
                );
            }

            let mut scored = results;
            if !pre_sorted {
                super::browse::sort_search_results(&mut scored, sort, sort_dir);
            }
            scored.truncate(result_cap);

            let mut entries: Vec<super::browse::BrowseEntry> = Vec::new();
            if let Some(ref parent) = app.browse.parent_entry {
                entries.push(parent.clone());
            }
            entries.extend(scored.into_iter().map(|(e, _)| e));
            app.browse.entries = entries;
            app.browse.selected_index = 0;
            app.browse.scroll_offset = 0;

            if total_matches > result_cap {
                app.set_status(format!(
                    "search: showing best {} of {} matches; raise [browsing] search_result_cap to show more",
                    result_cap, total_matches
                ));
            }
        },
        AppMessage::FilePickerComplete { session_id, purpose, path } => {
            reduce_file_picker_complete(app, session_id, purpose, path, tx);
        }
        AppMessage::FileTaskProgress { session_id, update } => {
            reduce_file_task_progress(app, session_id, update, tx);
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
                                    if let (Some(modified), Some(size)) = (item.modified, item.file_size) {
                                        let identity = crate::tui::browse::ProbeCacheIdentity {
                                            modified: Some(modified),
                                            size,
                                        };
                                        app.browse.update_valid_probe_for_identity(&item.path, identity, |cached| {
                                            cached.metadata.hdcd_detail = item.facts.hdcd_detail.clone();
                                        });
                                    } else {
                                        app.browse.remove_probe_cache_entry(&item.path);
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
                                state.mark_archive_staging_dirty();
                                let archive_persist_result = if let Some(context) = state.archive_edit_context.clone().filter(|context| context.owner == super::app::ArchiveMetadataEditOwner::Browse) {
                                    super::keybindings::record_staged_archive_metadata_write(
                                        app,
                                        &context.archive_path,
                                        &context.staging_dir,
                                        super::keybindings::archive_metadata_context_baseline(&context),
                                        &paths,
                                    )
                                } else {
                                    Ok(())
                                };
                                for path in &paths {
                                    app.browse.remove_probe_cache_entry(path);
                                    let _ = app.db.invalidate_probe(&path.display().to_string());
                                }
                                if let Err(err) = archive_persist_result {
                                    app.set_status(format!(
                                        "metadata editor: ReplayGain {} wrote staged files, but archive recovery tracking failed: {err}",
                                        mode.label()
                                    ));
                                } else {
                                    app.set_status(format!(
                                        "metadata editor: ReplayGain {} scan wrote {} file{}",
                                        mode.label(),
                                        paths.len(),
                                        if paths.len() == 1 { "" } else { "s" }
                                    ));
                                }
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
                                state.mark_archive_staging_dirty();
                                let archive_persist_result = if let Some(context) = state.archive_edit_context.clone().filter(|context| context.owner == super::app::ArchiveMetadataEditOwner::Browse) {
                                    super::keybindings::record_staged_archive_metadata_write(
                                        app,
                                        &context.archive_path,
                                        &context.staging_dir,
                                        super::keybindings::archive_metadata_context_baseline(&context),
                                        &paths,
                                    )
                                } else {
                                    Ok(())
                                };
                                for path in &paths {
                                    app.browse.remove_probe_cache_entry(path);
                                    let _ = app.db.invalidate_probe(&path.display().to_string());
                                }
                                if let Err(err) = archive_persist_result {
                                    app.set_status(format!(
                                        "metadata editor: {} wrote staged files, but archive recovery tracking failed: {err}",
                                        mode.label()
                                    ));
                                } else {
                                    app.set_status(format!(
                                        "metadata editor: {} updated {} file{}",
                                        mode.label(),
                                        paths.len(),
                                        if paths.len() == 1 { "" } else { "s" }
                                    ));
                                }
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
                    state.invalidate_artwork_preview_cache();
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
                    let close_editor = summary.all_saved() && state.close_after_successful_save;
                    for path in &summary.saved_paths {
                        app.browse.remove_probe_cache_entry(path);
                        let _ = app.db.invalidate_probe(&path.display().to_string());
                    }
                    if !summary.saved_paths.is_empty() {
                        state.mark_archive_staging_dirty();
                    }
                    if close_editor {
                        let archive_context = state.archive_edit_context.clone();
                        match archive_context.as_ref().map(|context| context.owner) {
                            Some(super::app::ArchiveMetadataEditOwner::Convert) => {
                                let updated = super::keybindings::sync_convert_archive_preview_metadata_from_editor(app, &state);
                                app.browse.probe_current_with_db(tx, Some(&app.db));
                                app.set_status(format!(
                                    "metadata editor: saved staged archive tags; {} track{} synced to conversion overrides",
                                    updated,
                                    if updated == 1 { "" } else { "s" }
                                ));
                            }
                                            Some(super::app::ArchiveMetadataEditOwner::Browse) => {
                                if let Some(context) = archive_context {
                                    match complete_browse_archive_metadata_save(
                                        app,
                                        tx,
                                        context,
                                        &summary.saved_paths,
                                    ) {
                                        Ok(()) => {}
                                        Err(err) => {
                                            app.active_overlay = ActiveOverlay::MetadataEditor(state);
                                            app.set_status(format!("metadata editor: saved staged archive tags, but recovery tracking failed; do not quit before saving/discarding: {err}"));
                                        }
                                    }
                                } else {
                                    app.set_status(summary.status_line());
                                }
                            }
                            None => {
                                app.browse.probe_current_with_db(tx, Some(&app.db));
                                app.set_status(summary.status_line());
                            }
                        }
                    } else {
                        app.set_status(summary.status_line());
                        state.phase = super::app::MetadataEditorPhase::Editing;
                        state.close_after_successful_save = true;
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
                    app.browse.refresh_with_search(Some(tx));
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
                app.browse.refresh_with_search(Some(tx));
            }
            Err(e) => {
                app.set_status(format!("CTDB repair failed: {}", e));
            }
        },
        AppMessage::CueWriteComplete { result, refresh_browse } => {
            match result {
                Ok(message) => {
                    app.set_status(message);
                    if refresh_browse && app.current_screen == AppScreen::Browse {
                        app.browse.refresh_with_search(Some(tx));
                        app.browse.probe_current_with_db(tx, Some(&app.db));
                    }
                }
                Err(message) => app.set_status(message),
            }
        }
        AppMessage::CuePreviewComplete { result } => {
            match result {
                Ok((cue_content, cue_path, summary)) => {
                    app.active_overlay = ActiveOverlay::CuePreview(Box::new(
                        super::app::CuePreviewState::new(cue_content, cue_path, summary.clone()),
                    ));
                    app.set_status(summary);
                }
                Err(err) => app.set_status(format!("MusicBrainz CUE: {}", err)),
            }
        }
        AppMessage::CueFillPrepComplete { cue_path, result } => {
            let (album, tracks, layout, sectors) = match result {
                Ok(prep) => prep,
                Err(err) => {
                    app.set_status(format!(":cue-fill: {}", err));
                    return;
                }
            };
            let toc_string = match super::musicbrainz::build_mb_toc(&sectors) {
                Some(s) => s,
                None => {
                    app.set_status(":cue-fill: TOC too short".to_string());
                    return;
                }
            };
            let cached = app.db.get_cached_mb_response(&toc_string);
            let n_cached = if cached.is_some() { "cached" } else { "fetching" };
            app.set_status(format!(
                ":cue-fill: {} disc TOC ({} tracks)…",
                n_cached,
                sectors.len().saturating_sub(1),
            ));

            let tx = tx.clone();
            let toc_for_msg = toc_string.clone();
            tokio::spawn(async move {
                let outcome = super::musicbrainz::lookup_release_by_toc(&sectors, cached).await;
                let _ = tx
                    .send(AppMessage::CueFillComplete {
                        outcome,
                        cue_path,
                        album,
                        tracks,
                        layout,
                        toc_string: toc_for_msg,
                    })
                    .await;
            });
        }
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
        AppMessage::TagsMbApplyReady {
            releases,
            selected,
            paths,
            decision,
        } => {
            apply_editor_with_mb_release_decision(app, releases, selected, paths, decision);
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

    app.set_status("MusicBrainz CUE: building preview…".to_string());
    let tx = tx.clone();
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            let (mut album, mut tracks) = super::cue_generate::gather_cue_info_blocking(&paths, &output_dir)
                .map_err(|e| e.to_string())?;

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

            Ok((cue_content, cue_path, summary))
        })
        .await
        .unwrap_or_else(|err| Err(format!("preview task failed: {}", err)));

        let _ = tx.send(AppMessage::CuePreviewComplete { result }).await;
    });
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

    // The single-image guard may read tags and probe sample counts. Run it
    // outside the reducer before mutating the editor; otherwise a completed
    // MB lookup can still freeze the TUI while the message handler verifies
    // the file.
    if paths.len() == 1 && release.tracks.len() > 1 {
        if let Some(tx) = app.tui_tx.clone() {
            let release_for_decision = release.clone();
            let paths_for_decision = paths.clone();
            app.set_status(":tags-mb: verifying selected release against file…".to_string());
            tokio::spawn(async move {
                let worker_paths = paths_for_decision.clone();
                let worker_release = release_for_decision.clone();
                let decision = tokio::task::spawn_blocking(move || {
                    super::musicbrainz::compute_per_track_decision_blocking(
                        &worker_paths,
                        &worker_release,
                    )
                })
                .await
                .unwrap_or_else(|err| super::musicbrainz::PerTrackDecision {
                    per_track_populate: false,
                    skip_reason: Some(format!(
                        "single-image verification task failed: {}; album-level tags only",
                        err
                    )),
                });
                let _ = tx
                    .send(AppMessage::TagsMbApplyReady {
                        releases,
                        selected,
                        paths,
                        decision,
                    })
                    .await;
            });
            return;
        }

        // Defensive fallback for tests or non-standard harnesses that did not
        // install `tui_tx`. Do not run blocking media verification here; apply
        // album-level tags rather than freezing the caller.
        app.set_status(
            ":tags-mb: async verifier unavailable; applying album-level tags only".to_string(),
        );
        apply_editor_with_mb_release_decision(
            app,
            releases,
            selected,
            paths,
            super::musicbrainz::PerTrackDecision {
                per_track_populate: false,
                skip_reason: Some(
                    "async verifier unavailable — album-level tags only".to_string(),
                ),
            },
        );
        return;
    }

    apply_editor_with_mb_release_decision(
        app,
        releases,
        selected,
        paths,
        super::musicbrainz::PerTrackDecision::default(),
    );
}

pub(super) fn apply_editor_with_mb_release_decision(
    app: &mut AppState,
    releases: Vec<super::musicbrainz::MbRelease>,
    selected: usize,
    paths: Vec<std::path::PathBuf>,
    decision: super::musicbrainz::PerTrackDecision,
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
    // The potentially blocking per-track guard was computed before this
    // reducer ran. Reuse it for status text and population so the event loop
    // never performs media/tag inspection here.
    let skip_reason = decision.skip_reason.clone();
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
    super::musicbrainz::populate_editor_from_mb_with_per_track_decision(
        &mut state,
        release,
        &decision,
    );
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
mod browse_archive_quit_lifecycle_tests {
    use super::*;
    use crate::config::TonepoetConfig;
    use crate::convert::pipeline::materializer_archive::{
        ArchiveRepackageProgressSnapshot, ArchiveRepackageReport, ArchiveRepackageStage,
    };
    use crate::tui::app::{
        ArchiveMetadataEditContext, ConfirmAction, MetadataEditorState, MetadataTechnicalDetails,
        PendingBrowseArchiveRename,
    };
    use std::fs;

    fn tx() -> mpsc::Sender<AppMessage> {
        let (tx, _rx) = mpsc::channel(8);
        tx
    }

    fn clean_browse_archive_editor(
        archive_path: std::path::PathBuf,
        staging_dir: std::path::PathBuf,
    ) -> Box<MetadataEditorState> {
        let mut state = MetadataEditorState::for_files(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            MetadataTechnicalDetails::default(),
        );
        state.archive_edit_context = Some(ArchiveMetadataEditContext::browse(
            archive_path,
            staging_dir,
        ));
        Box::new(state)
    }

    #[test]
    fn quit_reconciliation_cleans_clean_browse_archive_editor_staging() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        let staging = temp.path().join("staging");
        fs::write(&archive, b"archive").expect("archive");
        fs::create_dir(&staging).expect("staging");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.should_quit = true;

        let deferred = reconcile_browse_archive_metadata_editor_for_quit(
            &mut app,
            clean_browse_archive_editor(archive, staging.clone()),
            &tx(),
        );

        assert!(!deferred, "clean Browse archive editor should not block quit");
        assert!(
            !staging.exists(),
            "clean Browse archive metadata staging must be removed on quit"
        );
        assert!(matches!(app.active_overlay, ActiveOverlay::None));
    }

    #[test]
    fn quit_defers_instead_of_cleaning_active_browse_archive_repackage() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.tar");
        let staging = temp.path().join("staging");
        fs::write(&archive, b"archive").expect("archive");
        fs::create_dir(&staging).expect("staging");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.should_quit = true;
        app.browse_archive_repackage = Some(ArchiveMetadataEditContext::browse(
            archive,
            staging.clone(),
        ));

        assert!(defer_quit_for_browse_archive_metadata(&mut app, &tx()));
        assert!(!app.should_quit, "quit must wait for repackage completion");
        assert!(app.quit_after_browse_archive_repackage);
        assert!(
            app.browse_archive_repackage.is_some(),
            "active repackage context must remain owned until completion"
        );
        assert!(
            staging.exists(),
            "active repackage staging must not be removed by global quit"
        );
    }

    fn install_dirty_archive_staging(
        app: &mut AppState,
        archive: std::path::PathBuf,
        staging: std::path::PathBuf,
    ) {
        let listing = crate::tui::archive_listing::ArchiveListing {
            archive_path: archive.clone(),
            format: "zip".to_string(),
            physical_size: 7,
            entries: Vec::new(),
        };
        app.browse.enter_archive(listing, None);
        let (secs, nanos, size) = crate::tui::app::archive_fingerprint(&archive).expect("fingerprint");
        let mut session_guard = crate::tui::browse::ArchiveStagingSession::new_test_owned(
            staging,
            archive,
            secs,
            nanos,
            size,
        );
        session_guard.append_edit(crate::tui::browse::ArchiveEdit::Rename {
            from: "old.flac".to_string(),
            to: "new.flac".to_string(),
        });
        // The guard owns cleanup until the exact handoff point. If any setup
        // above panics, the staging tree is removed; after this assignment,
        // AppState's test Drop owns cleanup.
        app.browse.archive.as_mut().expect("archive").staging = Some(session_guard.into_inner());
    }

    #[tokio::test]
    async fn quit_defers_dirty_active_browse_archive_staging_for_repackage() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        let staging = temp.path().join("staging");
        fs::write(&archive, b"archive").expect("archive");
        fs::create_dir(&staging).expect("staging");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.should_quit = true;
        install_dirty_archive_staging(&mut app, archive.clone(), staging.clone());

        assert!(defer_quit_for_browse_archive_metadata(&mut app, &tx()));
        assert!(!app.should_quit, "quit must wait for deferred archive save");
        assert!(app.quit_after_browse_archive_repackage);
        assert!(
            app.browse_archive_repackage
                .as_ref()
                .is_some_and(|context| context.archive_path == archive && context.staging_dir == staging),
            "dirty active archive staging must be scheduled for repackage before quit"
        );
        assert!(
            app.browse.active_archive_staging().is_some(),
            "Browse must retain staging ownership until save success/discard"
        );
    }

    #[tokio::test]
    async fn clean_metadata_editor_over_active_staging_preserves_and_quit_repackages() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        let staging = temp.path().join("staging");
        fs::write(&archive, b"archive").expect("archive");
        fs::create_dir(&staging).expect("staging");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.should_quit = true;
        install_dirty_archive_staging(&mut app, archive.clone(), staging.clone());
        let active = app
            .browse
            .active_archive_staging()
            .expect("active staging")
            .clone();

        let mut state = MetadataEditorState::for_files(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            MetadataTechnicalDetails::default(),
        );
        state.archive_edit_context = Some(
            ArchiveMetadataEditContext::browse_active_staging_with_fingerprint(
                archive.clone(),
                staging.clone(),
                active.archive_mtime_secs,
                active.archive_mtime_nanos,
                active.archive_size,
                None,
            ),
        );

        let deferred = reconcile_browse_archive_metadata_editor_for_quit(
            &mut app,
            Box::new(state),
            &tx(),
        );

        assert!(deferred, "quit must defer for dirty Browse-owned archive staging");
        assert!(!app.should_quit);
        assert!(app.quit_after_browse_archive_repackage);
        assert!(staging.exists(), "metadata editor must not delete Browse-owned staging");
        assert!(
            app.browse.active_archive_staging().is_some(),
            "Browse must retain ownership until archive save succeeds or user discards"
        );
        assert!(
            app.browse_archive_repackage
                .as_ref()
                .is_some_and(|context| context.archive_path == archive && context.staging_dir == staging),
            "quit must schedule the active dirty staging for deferred repackage"
        );
    }

    #[tokio::test]
    async fn screen_switch_defers_dirty_active_browse_archive_staging_for_repackage() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        let staging = temp.path().join("staging");
        fs::write(&archive, b"archive").expect("archive");
        fs::create_dir(&staging).expect("staging");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        install_dirty_archive_staging(&mut app, archive.clone(), staging.clone());

        super::super::keybindings::switch_screen_reconciling_browse_archive(
            &mut app,
            AppScreen::Convert,
            &tx(),
        );

        assert_eq!(app.current_screen, AppScreen::Browse);
        assert_eq!(app.deferred_browse_archive_screen_switch, Some(AppScreen::Convert));
        assert!(
            app.browse_archive_repackage
                .as_ref()
                .is_some_and(|context| context.archive_path == archive && context.staging_dir == staging),
            "leaving Browse must schedule dirty archive staging for deferred save"
        );
        assert!(
            app.browse.active_archive_staging().is_some(),
            "screen switch must not drop the staged edits before save success"
        );
    }

    #[tokio::test]
    async fn completed_initial_rename_after_screen_switch_attaches_and_schedules_repackage() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        fs::write(&archive, b"archive").expect("archive");

        let (secs, nanos, size) = crate::tui::app::archive_fingerprint(&archive).expect("fingerprint");
        let pending = PendingBrowseArchiveRename::new(
            archive.clone(),
            "old.flac".to_string(),
            "new.flac".to_string(),
            secs,
            nanos,
            size,
            None,
        );
        let staging = pending.staging_dir.clone();
        fs::create_dir_all(&staging).expect("staging");
        fs::write(staging.join("old.flac"), b"audio").expect("old staged file");

        let listing = crate::tui::archive_listing::ArchiveListing {
            archive_path: archive.clone(),
            format: "zip".to_string(),
            physical_size: 7,
            entries: Vec::new(),
        };

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.deferred_browse_archive_screen_switch = Some(AppScreen::Convert);
        app.browse.enter_archive(listing, None);
        app.pending_browse_archive_rename = Some(pending);

        handle_archive_entry_rename_result(
            &mut app,
            archive.clone(),
            staging.clone(),
            "old.flac".to_string(),
            "new.flac".to_string(),
            Ok(()),
            &tx(),
        );

        assert_eq!(app.current_screen, AppScreen::Browse);
        assert_eq!(app.deferred_browse_archive_screen_switch, Some(AppScreen::Convert));
        assert!(
            app.browse.active_archive_staging().is_some_and(|session| {
                session.staging_dir == staging
                    && session.archive_path == archive
                    && session.dirty
                    && session.edits.iter().any(|edit| matches!(
                        edit,
                        crate::tui::browse::ArchiveEdit::Rename { from, to }
                            if from == "old.flac" && to == "new.flac"
                    ))
            }),
            "completed initial rename must remain attached to Browse after screen switch"
        );
        assert!(
            app.browse_archive_repackage
                .as_ref()
                .is_some_and(|context| context.archive_path == archive && context.staging_dir == staging),
            "completed initial rename after screen switch must immediately schedule deferred save"
        );
    }

    #[test]
    fn successful_repackage_completion_resumes_deferred_quit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.tar");
        let staging = temp.path().join("staging");
        fs::write(&archive, b"archive").expect("archive");
        fs::create_dir(&staging).expect("staging");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse_archive_repackage = Some(ArchiveMetadataEditContext::browse(
            archive.clone(),
            staging.clone(),
        ));
        app.quit_after_browse_archive_repackage = true;

        handle_archive_repackage_result(
            &mut app,
            archive,
            staging.clone(),
            Ok(ArchiveRepackageReport::default()),
            &tx(),
        );

        assert!(app.should_quit, "successful repackage should resume quit");
        assert!(!app.quit_after_browse_archive_repackage);
        assert!(app.browse_archive_repackage.is_none());
        assert!(
            !staging.exists(),
            "completed repackage must clean Browse archive staging"
        );
    }

    #[test]
    fn failed_repackage_completion_cancels_deferred_quit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.tar");
        let staging = temp.path().join("staging");
        fs::write(&archive, b"archive").expect("archive");
        fs::create_dir(&staging).expect("staging");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse_archive_repackage = Some(ArchiveMetadataEditContext::browse(
            archive.clone(),
            staging.clone(),
        ));
        app.quit_after_browse_archive_repackage = true;

        handle_archive_repackage_result(
            &mut app,
            archive,
            staging.clone(),
            Err("simulated failure".to_string()),
            &tx(),
        );

        assert!(
            !app.should_quit,
            "failed repackage should keep the app open so the error is visible"
        );
        assert!(!app.quit_after_browse_archive_repackage);
        assert!(
            staging.exists(),
            "failed repackage must preserve staged edits for retry/discard"
        );
        assert!(
            matches!(app.active_overlay, ActiveOverlay::Confirmation {
                action: ConfirmAction::ArchiveRepackageFailure { .. },
                ..
            }),
            "failed repackage should expose a retry/discard resolution"
        );
        let status = app.status_message.as_ref().map(|(message, _)| message.as_str());
        assert!(
            status.unwrap_or_default().contains("staged edits preserved"),
            "unexpected status: {status:?}"
        );
    }


    #[tokio::test]
    async fn active_browse_repackage_cancel_does_not_install_preserved_editor_owned_context() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        let staging = temp.path().join("staging");
        fs::write(&archive, b"archive").expect("archive");
        fs::create_dir_all(&staging).expect("staging");
        fs::write(staging.join("track.flac"), b"audio").expect("track");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        install_dirty_archive_staging(&mut app, archive.clone(), staging.clone());
        assert!(
            app.browse.active_archive_staging().is_some(),
            "test must start from an active Browse-owned staging session"
        );

        assert!(
            start_repackage_for_active_browse_staging(&mut app, &archive, &staging, &tx()),
            "dirty active Browse staging should schedule repackage"
        );
        assert!(
            app.browse_archive_repackage
                .as_ref()
                .is_some_and(|context| !context.editor_owns_staging
                    && context.archive_path == archive
                    && context.staging_dir == staging),
            "active Browse staging must use a Browse-owned, not editor-owned, repackage context"
        );

        handle_archive_repackage_result(
            &mut app,
            archive.clone(),
            staging.clone(),
            Err(crate::convert::pipeline::materializer_archive::ARCHIVE_REPACKAGE_CANCELLED.to_string()),
            &tx(),
        );

        assert!(
            app.browse.active_archive_staging().is_some_and(|session| {
                session.archive_path == archive && session.staging_dir == staging && session.dirty
            }),
            "cancelled active Browse repackage must leave Browse as the live staging owner"
        );
        assert!(
            app.preserved_editor_archive_repackage.is_none(),
            "Browse-owned cancellation must not install a preserved editor-owned retry/discard context"
        );
        assert!(
            !matches!(app.active_overlay, ActiveOverlay::Confirmation {
                action: ConfirmAction::ArchiveRepackageFailure { .. },
                ..
            }),
            "Browse-owned cancellation must not block future in-archive metadata edits behind the editor-owned confirmation"
        );
    }

    #[test]
    fn cancelled_editor_owned_repackage_keeps_retry_discard_context_in_process() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.tar");
        let staging = temp.path().join("staging");
        fs::write(&archive, b"archive").expect("archive");
        fs::create_dir(&staging).expect("staging");
        let context = ArchiveMetadataEditContext::browse(archive.clone(), staging.clone());
        assert!(context.editor_owns_staging, "test must cover parent-directory editor-owned staging");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse_archive_repackage = Some(context);
        app.quit_after_browse_archive_repackage = true;

        handle_archive_repackage_result(
            &mut app,
            archive.clone(),
            staging.clone(),
            Err(crate::convert::pipeline::materializer_archive::ARCHIVE_REPACKAGE_CANCELLED.to_string()),
            &tx(),
        );

        assert!(!app.should_quit, "cancelling an archive save must cancel deferred quit");
        assert!(!app.quit_after_browse_archive_repackage);
        assert!(app.browse_archive_repackage.is_none());
        assert!(
            app.preserved_editor_archive_repackage
                .as_ref()
                .is_some_and(|context| context.archive_path == archive && context.staging_dir == staging),
            "editor-owned cancelled repackage must retain a live retry/discard context"
        );
        assert!(staging.exists(), "cancelled editor-owned staging must remain available");
        assert!(
            matches!(app.active_overlay, ActiveOverlay::Confirmation {
                action: ConfirmAction::ArchiveRepackageFailure { ref context, ref error },
                ..
            } if context.archive_path == archive
                && context.staging_dir == staging
                && error.contains("cancelled")),
            "cancelled editor-owned repackage must show an immediate retry/discard/keep confirmation"
        );
    }



    #[tokio::test]
    async fn editor_owned_whole_archive_metadata_save_schedules_immediate_repackage() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        let staging = temp.path().join("staging");
        fs::write(&archive, b"archive").expect("archive");
        fs::create_dir_all(&staging).expect("staging");
        let track = staging.join("track.flac");
        fs::write(&track, b"audio").expect("track");
        let (secs, nanos, size) = crate::tui::app::archive_fingerprint(&archive).expect("fingerprint");
        let context = ArchiveMetadataEditContext::browse_with_fingerprint(
            archive.clone(),
            staging.clone(),
            secs,
            nanos,
            size,
            None,
        );
        assert!(context.editor_owns_staging);

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        assert!(app.browse.archive.is_none(), "test must cover parent-directory archive editing");

        complete_browse_archive_metadata_save(
            &mut app,
            &tx(),
            context,
            &[track],
        )
        .expect("metadata save should durably register editor-owned staging");

        assert!(
            app.browse_archive_repackage
                .as_ref()
                .is_some_and(|context| context.archive_path == archive && context.staging_dir == staging),
            "editor-owned whole-archive metadata save must start immediate repackage without active archive browse state"
        );
        assert!(
            matches!(app.active_overlay, ActiveOverlay::FileTaskProgress(_)),
            "immediate repackage must show the progress overlay"
        );
    }

    #[test]
    fn archive_repackage_progress_overlay_current_row_shows_step_not_archive_name() {
        let snapshot = ArchiveRepackageProgressSnapshot {
            stage: ArchiveRepackageStage::Compressing,
            status: "Compressing archive...".to_string(),
            current_item: Some("album.7z".to_string()),
            bytes_done: 47,
            bytes_total: Some(100),
            items_done: 0,
            items_total: Some(1),
            rate_bytes_per_sec: Some(10),
        };

        let update = archive_repackage_file_task_update(snapshot);

        match update {
            tui_file_picker::FileTaskProgressUpdate::Snapshot {
                status,
                current_item,
                totals,
                rate_bytes_per_sec,
                ..
            } => {
                assert_eq!(status, "Compressing archive...");
                let current_item = current_item.expect("repackage snapshots should drive the live row");
                assert_eq!(
                    current_item.label, "Compressing archive...",
                    "the progress dialog live row must show the active repackage step; the archive name belongs in the overlay scope"
                );
                assert_ne!(
                    current_item.label, "album.7z",
                    "regression: showing the archive filename hides the actual repackage step"
                );
                assert_eq!(current_item.bytes_done, 47);
                assert_eq!(current_item.bytes_total, Some(100));
                assert_eq!(totals.bytes_done, 47);
                assert_eq!(totals.bytes_total, Some(100));
                assert_eq!(rate_bytes_per_sec, Some(10));
            }
            other => panic!("expected snapshot update, got {other:?}"),
        }
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
        let mut app = AppState::new_for_test(TonepoetConfig::default());
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
        let mut app = AppState::new_for_test(TonepoetConfig::default());
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
        let mut app = AppState::new_for_test(TonepoetConfig::default());
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
        let mut app = AppState::new_for_test(TonepoetConfig::default());
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
        let mut app = AppState::new_for_test(TonepoetConfig::default());
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

#[cfg(test)]
mod artwork_file_picker_completion_tests {
    use super::*;
    use crate::config::TonepoetConfig;
    use crate::tui::app::{ContentTab, FilePickerPurpose, MetadataEditorState, MetadataFilePickerState, MetadataTechnicalDetails};
    use crate::tui::metadata_editor_actions::test_probe;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn channel() -> mpsc::Sender<AppMessage> {
        let (tx, _rx) = mpsc::channel(8);
        tx
    }

    fn editor_for_audio_path(path: PathBuf) -> Box<MetadataEditorState> {
        let label = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "track.flac".to_string());
        let mut state = Box::new(MetadataEditorState::for_files(
            vec![path],
            Vec::new(),
            vec![label],
            MetadataTechnicalDetails::default(),
        ));
        state.content_tab = ContentTab::Artwork;
        state
    }

    fn editor_with_picker(dir: &Path, picture_type: lofty::picture::PictureType) -> (Box<MetadataEditorState>, PathBuf) {
        let audio_path = dir.join("track.flac");
        fs::write(&audio_path, b"audio").expect("audio fixture");
        let mut state = editor_for_audio_path(audio_path.clone());
        let picker = tui_file_picker::FilePickerState::new(tui_file_picker::FilePickerConfig {
            start_dir: dir.to_path_buf(),
            filter: tui_file_picker::FilePickerFilter::Images,
            selection_mode: tui_file_picker::FilePickerSelectionMode::Files,
            operation_policy: tui_file_picker::FileOperationPolicy {
                allow_new_file: false,
                allow_new_folder: false,
                allow_cut: false,
                allow_copy: false,
                allow_paste: false,
                allow_delete: false,
                symlink_copy: tui_file_picker::SymlinkCopyPolicy::Reject,
                cross_device_cut: tui_file_picker::CrossDeviceCutPolicy::Reject,
                delete: tui_file_picker::DeletePolicy::FilesAndEmptyDirectories,
            },
            ..tui_file_picker::FilePickerConfig::default()
        });
        state.file_picker = Some(MetadataFilePickerState::new(
            FilePickerPurpose::SelectArtwork { picture_type: picture_type.clone() },
            picker,
        ));
        (state, audio_path)
    }

    #[test]
    fn cancel_completion_closes_picker_preserves_artwork_tab_and_persists_last_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let picture_type = lofty::picture::PictureType::CoverBack;
        let (state, _audio_path) = editor_with_picker(temp.path(), picture_type.clone());
        let session_id = state.file_picker.as_ref().expect("picker").session_id;
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::MetadataEditor(state);

        handle_message(
            &mut app,
            AppMessage::FilePickerComplete {
                session_id,
                purpose: FilePickerPurpose::SelectArtwork { picture_type: picture_type.clone() },
                path: None,
            },
            &channel(),
        );

        assert_eq!(app.last_artwork_picker_dir.as_deref(), Some(temp.path()));
        match &app.active_overlay {
            ActiveOverlay::MetadataEditor(state) => {
                assert!(state.file_picker.is_none());
                assert_eq!(state.content_tab, ContentTab::Artwork);
            }
            other => panic!("expected metadata editor after cancel, got {other:?}"),
        }
    }

    #[test]
    fn selected_completion_reaches_artwork_dispatch_with_path_type_and_targets() {
        let temp = tempfile::tempdir().expect("tempdir");
        let image_path = temp.path().join("cover.png");
        fs::write(&image_path, b"png").expect("image fixture");
        let picture_type = lofty::picture::PictureType::CoverBack;
        let (state, audio_path) = editor_with_picker(temp.path(), picture_type.clone());
        let session_id = state.file_picker.as_ref().expect("picker").session_id;
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::MetadataEditor(state);
        test_probe::clear_artwork_dispatches();

        handle_message(
            &mut app,
            AppMessage::FilePickerComplete {
                session_id,
                purpose: FilePickerPurpose::SelectArtwork { picture_type: picture_type.clone() },
                path: Some(image_path.clone()),
            },
            &channel(),
        );

        assert_eq!(app.last_artwork_picker_dir.as_deref(), Some(temp.path()));
        let dispatch = test_probe::last_artwork_dispatch().expect("artwork dispatch");
        assert_eq!(dispatch.image_path, image_path);
        assert_eq!(dispatch.picture_type, picture_type);
        assert_eq!(dispatch.target_paths, vec![audio_path]);
        match &app.active_overlay {
            ActiveOverlay::MetadataEditor(state) => {
                assert!(state.file_picker.is_none());
                assert_eq!(state.content_tab, ContentTab::Artwork);
            }
            other => panic!("expected metadata editor after selection, got {other:?}"),
        }
    }
    #[test]
    fn stale_artwork_completion_for_same_purpose_does_not_close_newer_picker_or_dispatch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let stale_image = temp.path().join("stale.png");
        fs::write(&stale_image, b"png").expect("image fixture");
        let picture_type = lofty::picture::PictureType::CoverBack;
        let (state, _audio_path) = editor_with_picker(temp.path(), picture_type.clone());
        let active_session_id = state.file_picker.as_ref().expect("picker").session_id;
        let stale_session_id = active_session_id.saturating_add(10_000);
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::MetadataEditor(state);
        test_probe::clear_artwork_dispatches();

        handle_message(
            &mut app,
            AppMessage::FilePickerComplete {
                session_id: stale_session_id,
                purpose: FilePickerPurpose::SelectArtwork { picture_type },
                path: Some(stale_image),
            },
            &channel(),
        );

        assert!(test_probe::last_artwork_dispatch().is_none());
        match &app.active_overlay {
            ActiveOverlay::MetadataEditor(state) => {
                let picker = state.file_picker.as_ref().expect("newer picker remains open");
                assert_eq!(picker.session_id, active_session_id);
                assert_eq!(state.content_tab, ContentTab::Artwork);
            }
            other => panic!("expected metadata editor after stale completion, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod copy_move_file_picker_flow_tests {
    use super::*;
    use crate::config::TonepoetConfig;
    use crate::tui::app::{FilePickerPurpose, MetadataFilePickerState};
    use crate::convert::classify::EntryKind;
    use crate::tui::browse::BrowseEntry;
    use crate::tui::command::Command;
    use crate::tui::context_menu::{execute_context_action, ContextAction};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn tx() -> mpsc::Sender<AppMessage> {
        let (tx, _rx) = mpsc::channel(16);
        tx
    }

    fn app_with_selected_path(current_dir: &Path, path: PathBuf, kind: EntryKind) -> AppState {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.current_dir = current_dir.to_path_buf();
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "selected".to_string());
        let size = fs::metadata(&path).map(|metadata| metadata.len()).unwrap_or(0);
        app.browse.entries = vec![BrowseEntry::new(path, name, kind, size, None)];
        app.browse.selected_index = 0;
        app.browse.clear_multi_selection();
        app.active_overlay = ActiveOverlay::None;
        app
    }

    fn active_picker(app: &AppState) -> &MetadataFilePickerState {
        match &app.active_overlay {
            ActiveOverlay::FilePicker(session) => session,
            other => panic!("expected file picker overlay, got {other:?}"),
        }
    }

    fn assert_copy_picker(app: &AppState, expected_sources: &[PathBuf]) {
        let session = active_picker(app);
        match &session.purpose {
            FilePickerPurpose::CopyTo { sources, force } => {
                assert_eq!(sources, expected_sources);
                assert!(!force);
            }
            other => panic!("expected CopyTo picker purpose, got {other:?}"),
        }
        assert_eq!(
            session.picker.selection_mode(),
            tui_file_picker::FilePickerSelectionMode::Directories
        );
    }

    fn assert_move_picker(app: &AppState, expected_sources: &[PathBuf]) {
        let session = active_picker(app);
        match &session.purpose {
            FilePickerPurpose::MoveTo { sources, force } => {
                assert_eq!(sources, expected_sources);
                assert!(!force);
            }
            other => panic!("expected MoveTo picker purpose, got {other:?}"),
        }
        assert_eq!(
            session.picker.selection_mode(),
            tui_file_picker::FilePickerSelectionMode::Directories
        );
    }

    #[test]
    fn context_menu_copy_and_move_open_directory_picker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("track.flac");
        fs::write(&source, b"audio").expect("source file");

        let mut copy_app = app_with_selected_path(temp.path(), source.clone(), EntryKind::OtherFile);
        execute_context_action(&mut copy_app, ContextAction::CopyTo, &tx(), false);
        assert_copy_picker(&copy_app, &[source.clone()]);

        let mut move_app = app_with_selected_path(temp.path(), source.clone(), EntryKind::OtherFile);
        execute_context_action(&mut move_app, ContextAction::MoveTo, &tx(), false);
        assert_move_picker(&move_app, &[source]);
    }

    #[test]
    fn bare_cp_and_mv_commands_open_directory_picker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("track.flac");
        fs::write(&source, b"audio").expect("source file");

        let mut copy_app = app_with_selected_path(temp.path(), source.clone(), EntryKind::OtherFile);
        crate::tui::command::execute_command(
            &mut copy_app,
            Command::Copy { dest: String::new(), force: false },
            &tx(),
        );
        assert_copy_picker(&copy_app, &[source.clone()]);

        let mut move_app = app_with_selected_path(temp.path(), source.clone(), EntryKind::OtherFile);
        crate::tui::command::execute_command(
            &mut move_app,
            Command::Move { dest: String::new(), force: false },
            &tx(),
        );
        assert_move_picker(&move_app, &[source]);
    }

    #[test]
    fn force_copy_picker_still_defaults_to_ask_policy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("track.flac");
        fs::write(&source, b"audio").expect("source file");

        let mut app = app_with_selected_path(temp.path(), source.clone(), EntryKind::OtherFile);
        crate::tui::command::execute_command(
            &mut app,
            Command::Copy { dest: String::new(), force: true },
            &tx(),
        );

        let session = active_picker(&app);
        match &session.purpose {
            FilePickerPurpose::CopyTo { sources, force } => {
                assert_eq!(sources.as_slice(), &[source.clone()]);
                assert!(*force, "the force flag is preserved for the eventual operation");
            }
            other => panic!("expected CopyTo picker purpose, got {other:?}"),
        }
        assert_eq!(
            session.picker.conflict_policy(),
            Some(tui_file_picker::ConflictPolicyPreset::Ask),
            "interactive destination pickers always start in Ask mode; users must opt into preset overwrite"
        );
    }

    #[test]
    fn selected_copy_move_policy_is_read_before_picker_is_closed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("track.flac");
        fs::write(&source, b"audio").expect("source file");

        let mut app = app_with_selected_path(temp.path(), source.clone(), EntryKind::OtherFile);
        crate::tui::command::execute_command(
            &mut app,
            Command::Copy { dest: String::new(), force: false },
            &tx(),
        );
        let session = active_picker(&app);
        let session_id = session.session_id;
        let purpose = session.purpose.clone();

        match &mut app.active_overlay {
            ActiveOverlay::FilePicker(session) => {
                session
                    .picker
                    .set_conflict_policy(Some(tui_file_picker::ConflictPolicyPreset::Skip));
            }
            other => panic!("expected file picker overlay, got {other:?}"),
        }

        assert_eq!(
            matching_file_picker_conflict_policy(&app, session_id, &purpose),
            Some(tui_file_picker::ConflictPolicyPreset::Skip),
            "event-loop completion must snapshot the selected policy before closing the picker"
        );
        assert!(close_matching_file_picker(&mut app, session_id, &purpose));
        assert!(matches!(app.active_overlay, ActiveOverlay::None));
        assert_eq!(
            matching_file_picker_conflict_policy(&app, session_id, &purpose),
            None,
            "after close, the picker state is gone; reading after close would lose the policy"
        );
    }

    #[test]
    fn picker_selected_destination_starts_copy_progress_overlay() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("track.flac");
        let dest = temp.path().join("dest");
        fs::write(&source, b"audio").expect("source file");
        fs::create_dir(&dest).expect("dest dir");

        let mut app = app_with_selected_path(temp.path(), source.clone(), EntryKind::OtherFile);
        crate::tui::command::execute_command(
            &mut app,
            Command::Copy { dest: String::new(), force: false },
            &tx(),
        );
        let session = active_picker(&app);
        let session_id = session.session_id;
        let purpose = session.purpose.clone();

        handle_message(
            &mut app,
            AppMessage::FilePickerComplete {
                session_id,
                purpose,
                path: Some(dest.clone()),
            },
            &tx(),
        );

        match &app.active_overlay {
            ActiveOverlay::FileTaskProgress(session) => {
                assert_eq!(session.progress.kind, tui_file_picker::FileTaskKind::Copy);
            }
            other => panic!("expected copy progress overlay, got {other:?}"),
        }
    }

    #[test]
    fn picker_selected_destination_starts_move_progress_overlay() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("track.flac");
        let dest = temp.path().join("dest");
        fs::write(&source, b"audio").expect("source file");
        fs::create_dir(&dest).expect("dest dir");

        let mut app = app_with_selected_path(temp.path(), source.clone(), EntryKind::OtherFile);
        crate::tui::command::execute_command(
            &mut app,
            Command::Move { dest: String::new(), force: false },
            &tx(),
        );
        let session = active_picker(&app);
        let session_id = session.session_id;
        let purpose = session.purpose.clone();

        handle_message(
            &mut app,
            AppMessage::FilePickerComplete {
                session_id,
                purpose,
                path: Some(dest.clone()),
            },
            &tx(),
        );

        match &app.active_overlay {
            ActiveOverlay::FileTaskProgress(session) => {
                assert_eq!(session.progress.kind, tui_file_picker::FileTaskKind::Move);
            }
            other => panic!("expected move progress overlay, got {other:?}"),
        }
    }



    #[test]
    fn directory_mode_move_rejects_destination_inside_source_tree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("album");
        let nested_dest = source.join("nested");
        fs::create_dir(&source).expect("source dir");
        fs::write(source.join("track.flac"), b"audio").expect("source file");
        fs::create_dir(&nested_dest).expect("nested destination dir");

        let mut app = app_with_selected_path(temp.path(), source.clone(), EntryKind::Directory);
        crate::tui::command::execute_command(
            &mut app,
            Command::Move { dest: String::new(), force: false },
            &tx(),
        );
        let session = active_picker(&app);
        let session_id = session.session_id;
        let purpose = session.purpose.clone();

        handle_message(
            &mut app,
            AppMessage::FilePickerComplete {
                session_id,
                purpose,
                path: Some(nested_dest.clone()),
            },
            &tx(),
        );

        match &app.active_overlay {
            ActiveOverlay::FileTaskProgress(session) => {
                assert_eq!(session.progress.kind, tui_file_picker::FileTaskKind::Move);
            }
            other => panic!("expected move progress overlay, got {other:?}"),
        }
        assert!(source.exists(), "worker preflight must preserve the source directory");
        assert!(
            !nested_dest.join("album").exists(),
            "recursive destination target must not be created synchronously"
        );
    }

    #[test]
    fn stale_copy_move_picker_completion_is_ignored() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("track.flac");
        let dest = temp.path().join("dest");
        fs::write(&source, b"audio").expect("source file");
        fs::create_dir(&dest).expect("dest dir");

        let mut app = app_with_selected_path(temp.path(), source.clone(), EntryKind::OtherFile);
        crate::tui::command::execute_command(
            &mut app,
            Command::Copy { dest: String::new(), force: false },
            &tx(),
        );
        let session = active_picker(&app);
        let active_session_id = session.session_id;
        let purpose = session.purpose.clone();

        handle_message(
            &mut app,
            AppMessage::FilePickerComplete {
                session_id: active_session_id.saturating_add(1),
                purpose,
                path: Some(dest.clone()),
            },
            &tx(),
        );

        assert_eq!(active_picker(&app).session_id, active_session_id);
        assert!(!dest.join("track.flac").exists(), "stale completion must not copy");
        let status = app.status_message.as_ref().map(|(message, _)| message.as_str());
        assert!(
            status.unwrap_or_default().contains("ignored stale copy completion"),
            "unexpected status: {status:?}"
        );
    }

    #[test]
    fn directory_mode_copy_rejects_destination_inside_source_tree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("album");
        let nested_dest = source.join("nested");
        fs::create_dir(&source).expect("source dir");
        fs::write(source.join("track.flac"), b"audio").expect("source file");
        fs::create_dir(&nested_dest).expect("nested destination dir");

        let mut app = app_with_selected_path(temp.path(), source.clone(), EntryKind::Directory);
        crate::tui::command::execute_command(
            &mut app,
            Command::Copy { dest: String::new(), force: false },
            &tx(),
        );
        let session = active_picker(&app);
        let session_id = session.session_id;
        let purpose = session.purpose.clone();

        handle_message(
            &mut app,
            AppMessage::FilePickerComplete {
                session_id,
                purpose,
                path: Some(nested_dest.clone()),
            },
            &tx(),
        );

        match &app.active_overlay {
            ActiveOverlay::FileTaskProgress(session) => {
                assert_eq!(session.progress.kind, tui_file_picker::FileTaskKind::Copy);
            }
            other => panic!("expected copy progress overlay, got {other:?}"),
        }
        assert!(source.exists(), "worker preflight must preserve the source directory");
        assert!(
            !nested_dest.join("album").exists(),
            "recursive destination target must not be created synchronously"
        );
    }
}

#[cfg(test)]
mod async_message_drain_tests {
    use super::*;
    use crate::config::TonepoetConfig;

    fn status(app: &AppState) -> Option<&str> {
        app.status_message.as_ref().map(|(message, _)| message.as_str())
    }

    fn test_cached_info(file_size: u64, title: &str) -> crate::tui::browse::CachedInfo {
        crate::tui::browse::CachedInfo {
            source: crate::tui::probe::SourceInfo {
                format_name: "FLAC".to_string(),
                codec: "flac".to_string(),
                bit_depth: Some(16),
                sample_rate: 44_100,
                channels: 2,
                channel_layout: "stereo".to_string(),
                duration_secs: 1.0,
                file_size,
            },
            metadata: crate::tui::probe::SourceMetadata {
                title: Some(title.to_string()),
                ..Default::default()
            },
        }
    }

    fn publish_scan_owned_files_for_warm_cache_test(
        app: &mut AppState,
        files: Vec<crate::tui::browse::BrowseEntry>,
    ) {
        // Warm-cache rows are keyed to scan-owned paths, not just the current
        // visible list. Tests must publish entries through the same path used
        // by directory scans so the per-scan identity index is populated.
        app.browse.publish_scanned_entries(None, Vec::new(), files);
        app.browse.reapply_after_directory_scan_complete(None);
    }

    #[test]
    fn capped_drain_preserves_order_and_leaves_remainder_for_next_frame() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let (tx, mut rx) = mpsc::channel(MAX_ASYNC_MESSAGES_PER_FRAME + 8);
        let total = MAX_ASYNC_MESSAGES_PER_FRAME + 4;
        for i in 0..total {
            tx.try_send(AppMessage::StatusMessage(format!("msg-{i}")))
                .expect("queue message");
        }

        let mut held = VecDeque::new();
        let (first, first_more) = drain_async_messages_for_frame(&mut app, &mut rx, &tx, &mut held, false);
        assert_eq!(first, MAX_ASYNC_MESSAGES_PER_FRAME);
        assert!(first_more, "hitting the per-frame async cap should request one non-clearing follow-up tick");
        let expected_first = format!("msg-{}", MAX_ASYNC_MESSAGES_PER_FRAME - 1);
        assert_eq!(status(&app), Some(expected_first.as_str()));

        let (second, second_more) = drain_async_messages_for_frame(&mut app, &mut rx, &tx, &mut held, false);
        assert_eq!(second, 4);
        assert!(!second_more);
        let expected_second = format!("msg-{}", total - 1);
        assert_eq!(status(&app), Some(expected_second.as_str()));
    }

    #[test]
    fn immediate_tick_is_separate_from_terminal_clear() {
        assert!(!needs_immediate_nonclearing_tick(0, false));
        assert!(needs_immediate_nonclearing_tick(1, false));
        assert!(needs_immediate_nonclearing_tick(0, true));
    }

    #[test]
    fn browse_motion_drain_holds_only_browse_visible_messages() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        let (tx, mut rx) = mpsc::channel(4);
        let generation = app.browse.scan_generation;
        let directory = app.browse.current_dir.clone();
        tx.try_send(AppMessage::ProbeCacheWarmComplete {
            generation,
            path: directory,
            rows: Vec::new(),
        })
        .expect("queue browse-visible warm-cache message");
        tx.try_send(AppMessage::StatusMessage("conversion progress still drains".to_string()))
            .expect("queue safe message");

        let mut held = VecDeque::new();
        let (reduced, more) = drain_async_messages_for_frame(
            &mut app,
            &mut rx,
            &tx,
            &mut held,
            true,
        );

        assert_eq!(reduced, 1, "safe messages should still reduce while Browse focus is moving");
        assert_eq!(held.len(), 1, "Browse-visible reducers should be held, not left to clog the channel");
        assert_eq!(status(&app), Some("conversion progress still drains"));
        assert!(!app.force_redraw);
        assert!(!more, "a held Browse-visible message alone should not spin the event loop while motion deferral remains active");

        let (reduced_after_settle, more_after_settle) = drain_async_messages_for_frame(
            &mut app,
            &mut rx,
            &tx,
            &mut held,
            false,
        );
        assert_eq!(reduced_after_settle, 1);
        assert!(held.is_empty());
        assert!(!more_after_settle);
        assert!(!app.force_redraw);
    }

    #[test]
    fn input_waiting_defers_existing_warm_backlog_without_merging() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        assert!(browse_visible_work_should_wait(&app, true));

        let path = std::path::PathBuf::from("/tmp/tonepoet-input-wait-warm.flac");
        let identity = crate::tui::browse::ProbeCacheIdentity { modified: None, size: 1 };
        publish_scan_owned_files_for_warm_cache_test(
            &mut app,
            vec![crate::tui::browse::BrowseEntry::new(
                path.clone(),
                "tonepoet-input-wait-warm.flac".to_string(),
                EntryKind::AudioFile(crate::convert::formats::AudioFormat::Flac),
                identity.size,
                identity.modified,
            )],
        );
        let generation = app.browse.scan_generation;
        let directory = app.browse.current_dir.clone();
        assert_eq!(
            app.browse.enqueue_probe_cache_warm_rows(
                generation,
                directory,
                vec![crate::tui::browse::ProbeCacheWarmRow {
                    path,
                    identity,
                    info: test_cached_info(identity.size, "warm"),
                }],
            ),
            1,
        );

        let (tx, mut rx) = mpsc::channel(4);
        tx.try_send(AppMessage::StatusMessage("safe progress".to_string()))
            .expect("queue safe message");
        let mut held = VecDeque::new();
        let (reduced, more) = drain_async_messages_for_frame(
            &mut app,
            &mut rx,
            &tx,
            &mut held,
            true,
        );

        assert_eq!(reduced, 1);
        assert!(!more);
        assert_eq!(status(&app), Some("safe progress"));
        assert!(app.browse.has_probe_cache_warm_backlog());
        assert!(app.browse.current_cached_info().is_none());
        assert!(!app.force_redraw);
    }

    #[test]
    fn warm_cache_message_queues_without_requesting_terminal_clear() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let path = std::path::PathBuf::from("/tmp/tonepoet-warm-no-clear.flac");
        let identity = crate::tui::browse::ProbeCacheIdentity { modified: None, size: 1 };
        publish_scan_owned_files_for_warm_cache_test(
            &mut app,
            vec![crate::tui::browse::BrowseEntry::new(
                path.clone(),
                "tonepoet-warm-no-clear.flac".to_string(),
                EntryKind::AudioFile(crate::convert::formats::AudioFormat::Flac),
                identity.size,
                identity.modified,
            )],
        );
        let generation = app.browse.scan_generation;
        let directory = app.browse.current_dir.clone();
        let (tx, _rx) = mpsc::channel(4);

        handle_message(
            &mut app,
            AppMessage::ProbeCacheWarmComplete {
                generation,
                path: directory,
                rows: vec![crate::tui::browse::ProbeCacheWarmRow {
                    path,
                    identity,
                    info: test_cached_info(identity.size, "warm"),
                }],
            },
            &tx,
        );

        assert!(app.browse.has_probe_cache_warm_backlog());
        assert!(
            !app.force_redraw,
            "queued warm-cache rows are ordinary reducer work, not terminal damage"
        );
    }

    #[test]
    fn warm_cache_backlog_requests_nonclearing_reducer_tick_only() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let count = 4096usize;
        let mut entries = Vec::new();
        let mut rows = Vec::new();
        for idx in 0..count {
            let path = std::path::PathBuf::from(format!("/tmp/tonepoet-warm-backlog-no-clear-{idx}.flac"));
            let identity = crate::tui::browse::ProbeCacheIdentity { modified: None, size: idx as u64 + 1 };
            entries.push(crate::tui::browse::BrowseEntry::new(
                path.clone(),
                format!("tonepoet-warm-backlog-no-clear-{idx}.flac"),
                EntryKind::AudioFile(crate::convert::formats::AudioFormat::Flac),
                identity.size,
                identity.modified,
            ));
            rows.push(crate::tui::browse::ProbeCacheWarmRow {
                path,
                identity,
                info: test_cached_info(identity.size, &format!("warm-{idx}")),
            });
        }
        publish_scan_owned_files_for_warm_cache_test(&mut app, entries);
        let generation = app.browse.scan_generation;
        let directory = app.browse.current_dir.clone();
        assert_eq!(app.browse.enqueue_probe_cache_warm_rows(generation, directory, rows), count);
        let (tx, _rx) = mpsc::channel(4);

        let work_remains = flush_browse_deferred_work(&mut app, &tx);

        assert!(work_remains, "large warm-cache backlogs should keep reducer slices ticking");
        assert!(
            !app.force_redraw,
            "warm-cache backlog ticks must not map to terminal.clear()"
        );
        let deferred = app.browse.take_browse_deferred_work();
        assert!(
            deferred.probe_backed_resort_needed,
            "probe-backed listing work should be coalesced until the warm backlog drains"
        );
        assert!(
            !deferred.info_pane_changed,
            "current-row refresh should wait for the final warm-cache slice"
        );
    }

    #[test]
    fn changed_filesystem_probe_completion_invalidates_and_schedules_one_reevaluation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("changed.flac");
        std::fs::write(&path, b"old").expect("write old");
        let old_identity = crate::tui::browse::ProbeCacheIdentity::from_metadata(
            &std::fs::metadata(&path).expect("old metadata"),
        );

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.entries = vec![crate::tui::browse::BrowseEntry::new(
            path.clone(),
            "changed.flac".to_string(),
            EntryKind::AudioFile(crate::convert::formats::AudioFormat::Flac),
            old_identity.size,
            old_identity.modified,
        )];
        app.browse.selected_index = 0;
        app.browse.probe_pending.insert(path.clone());
        app.browse.insert_probe_miss_for_identity(path.clone(), old_identity);

        std::fs::write(&path, b"new contents with different size").expect("write new");
        let (tx, _rx) = mpsc::channel(4);
        handle_message(
            &mut app,
            AppMessage::AudioProbeComplete {
                path: path.clone(),
                context: crate::tui::message::AudioProbeContext::Filesystem {
                    identity: Some(old_identity),
                },
                result: Box::new(Err("stale result".to_string())),
            },
            &tx,
        );

        assert!(!app.browse.probe_pending.contains(&path));
        assert_eq!(app.browse.probe_debounce.as_ref().map(|d| d.path.as_path()), Some(path.as_path()));
        assert!(!app.browse.has_probe_cache_entry_for_identity(&path, old_identity));
    }

    #[test]
    fn stale_probe_completion_cannot_insert_old_metadata_after_file_identity_changes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("changed-ok.flac");
        std::fs::write(&path, b"old").expect("write old");
        let old_identity = crate::tui::browse::ProbeCacheIdentity::from_metadata(
            &std::fs::metadata(&path).expect("old metadata"),
        );

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.entries = vec![crate::tui::browse::BrowseEntry::new(
            path.clone(),
            "changed-ok.flac".to_string(),
            EntryKind::AudioFile(crate::convert::formats::AudioFormat::Flac),
            old_identity.size,
            old_identity.modified,
        )];
        app.browse.selected_index = 0;
        app.browse.probe_pending.insert(path.clone());
        app.browse.insert_probe_miss_for_identity(path.clone(), old_identity);

        std::fs::write(&path, b"new contents with different size").expect("write new");
        let (tx, _rx) = mpsc::channel(4);
        handle_message(
            &mut app,
            AppMessage::AudioProbeComplete {
                path: path.clone(),
                context: crate::tui::message::AudioProbeContext::Filesystem {
                    identity: Some(old_identity),
                },
                result: Box::new(Ok(test_cached_info(old_identity.size, "stale"))),
            },
            &tx,
        );

        assert!(!app.browse.has_probe_cache_entry_for_identity(&path, old_identity));
        assert!(app.browse.current_cached_info().is_none());
        assert_eq!(app.browse.probe_debounce.as_ref().map(|d| d.path.as_path()), Some(path.as_path()));
    }

    #[tokio::test]
    async fn stale_dir_stats_completion_rechecks_only_when_current_and_statable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album");
        std::fs::create_dir(&path).expect("mkdir");
        let stale_identity = crate::tui::browse::ProbeCacheIdentity { modified: None, size: 0 };

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.entries = vec![crate::tui::browse::BrowseEntry::new(
            path.clone(),
            "album".to_string(),
            EntryKind::Directory,
            0,
            None,
        )];
        app.browse.selected_index = 0;
        app.browse.dir_stats_pending.insert(path.clone());

        let (tx, _rx) = mpsc::channel(4);
        handle_message(
            &mut app,
            AppMessage::DirStatsComplete {
                path: path.clone(),
                identity: stale_identity,
                stats: crate::tui::browse::DirStats {
                    folder_count: 0,
                    file_count: 99,
                    audio_count: 99,
                    audio_size: 0,
                    total_size: 99,
                },
                cancelled: false,
            },
            &tx,
        );

        assert!(app.browse.current_dir_stats().is_none());
        assert!(!app.browse.dir_stats_pending.contains(&path));
        assert_eq!(
            app.browse.probe_debounce.as_ref().map(|pending| pending.path.as_path()),
            Some(path.as_path()),
            "a stale stats result should re-enter the settled-focus policy path instead of immediately relaunching recursive stats"
        );
    }

    #[test]
    fn missing_dir_stats_completion_stops_without_retry() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("gone");
        std::fs::create_dir(&path).expect("mkdir");
        let identity = crate::tui::browse::ProbeCacheIdentity::from_metadata(
            &std::fs::metadata(&path).expect("metadata"),
        );

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.entries = vec![crate::tui::browse::BrowseEntry::new(
            path.clone(),
            "gone".to_string(),
            EntryKind::Directory,
            identity.size,
            identity.modified,
        )];
        app.browse.selected_index = 0;
        app.browse.dir_stats_pending.insert(path.clone());
        std::fs::remove_dir(&path).expect("remove dir");

        let (tx, _rx) = mpsc::channel(4);
        handle_message(
            &mut app,
            AppMessage::DirStatsComplete {
                path: path.clone(),
                identity,
                stats: crate::tui::browse::DirStats {
                    folder_count: 0,
                    file_count: 1,
                    audio_count: 1,
                    audio_size: 0,
                    total_size: 1,
                },
                cancelled: false,
            },
            &tx,
        );

        assert!(!app.browse.dir_stats_pending.contains(&path));
        assert!(app.browse.current_dir_stats().is_none());
    }

    #[test]
    fn disappeared_filesystem_probe_completion_does_not_reschedule() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("deleted.flac");
        std::fs::write(&path, b"audio").expect("write");
        let meta = std::fs::metadata(&path).expect("metadata");
        let identity = crate::tui::browse::ProbeCacheIdentity::from_metadata(&meta);

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.entries = vec![crate::tui::browse::BrowseEntry::new(
            path.clone(),
            "deleted.flac".to_string(),
            EntryKind::AudioFile(crate::convert::formats::AudioFormat::Flac),
            identity.size,
            identity.modified,
        )];
        app.browse.selected_index = 0;
        app.browse.probe_pending.insert(path.clone());

        std::fs::remove_file(&path).expect("delete");
        let (tx, _rx) = mpsc::channel(4);
        handle_message(
            &mut app,
            AppMessage::AudioProbeComplete {
                path: path.clone(),
                context: crate::tui::message::AudioProbeContext::Filesystem { identity: Some(identity) },
                result: Box::new(Err("file disappeared".to_string())),
            },
            &tx,
        );

        assert!(!app.browse.probe_pending.contains(&path));
        assert!(app.browse.probe_debounce.is_none());
        assert!(app.browse.current_cached_info().is_none());
    }

    fn folder_classification(
        identity: crate::tui::browse::ProbeCacheIdentity,
        kind: crate::tui::browse::FolderClassificationKind,
    ) -> crate::tui::browse::FolderContentClassification {
        crate::tui::browse::FolderContentClassification {
            kind,
            identity,
            audio: crate::tui::browse::FolderAudioSummary::default(),
            units: Vec::new(),
            unit_count: if kind == crate::tui::browse::FolderClassificationKind::Unknown { 0 } else { 1 },
            collection_many: false,
            io_budget_exhausted: false,
            disc_marker: None,
        }
    }

    #[test]
    fn folder_classify_completion_accepts_only_pending_matching_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album");
        std::fs::create_dir(&path).expect("mkdir");
        let identity = crate::tui::browse::ProbeCacheIdentity::from_metadata(
            &std::fs::metadata(&path).expect("metadata"),
        );

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.entries = vec![crate::tui::browse::BrowseEntry::new(
            path.clone(),
            "album".to_string(),
            EntryKind::Directory,
            identity.size,
            identity.modified,
        )];
        app.browse.selected_index = 0;
        app.browse.mark_folder_classification_pending_for_test(path.clone());

        let (tx, _rx) = mpsc::channel(4);
        handle_message(
            &mut app,
            AppMessage::FolderClassifyComplete {
                path: path.clone(),
                identity,
                classification: folder_classification(
                    identity,
                    crate::tui::browse::FolderClassificationKind::Album,
                ),
            },
            &tx,
        );

        assert!(!app.browse.folder_classification_pending_for(&path));
        assert!(app.browse.has_valid_folder_classification_for_identity(&path, identity));
        assert_eq!(
            app.browse.current_folder_classification().map(|classification| classification.kind),
            Some(crate::tui::browse::FolderClassificationKind::Album),
        );
        assert!(app.browse.deferred_work.info_pane_changed);
        assert!(
            !app.force_redraw,
            "current classification completion is reducer state, not terminal damage"
        );
    }

    #[test]
    fn folder_classify_completion_without_pending_marker_is_ignored() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album");
        std::fs::create_dir(&path).expect("mkdir");
        let identity = crate::tui::browse::ProbeCacheIdentity::from_metadata(
            &std::fs::metadata(&path).expect("metadata"),
        );

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let (tx, _rx) = mpsc::channel(4);
        handle_message(
            &mut app,
            AppMessage::FolderClassifyComplete {
                path: path.clone(),
                identity,
                classification: folder_classification(
                    identity,
                    crate::tui::browse::FolderClassificationKind::Album,
                ),
            },
            &tx,
        );

        assert!(!app.browse.has_valid_folder_classification_for_identity(&path, identity));
    }

    #[test]
    fn stale_folder_classify_completion_rejects_cache_insert() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album");
        std::fs::create_dir(&path).expect("mkdir");
        let stale_identity = crate::tui::browse::ProbeCacheIdentity { modified: None, size: 0 };

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.browse.mark_folder_classification_pending_for_test(path.clone());

        let (tx, _rx) = mpsc::channel(4);
        handle_message(
            &mut app,
            AppMessage::FolderClassifyComplete {
                path: path.clone(),
                identity: stale_identity,
                classification: folder_classification(
                    stale_identity,
                    crate::tui::browse::FolderClassificationKind::Album,
                ),
            },
            &tx,
        );

        assert!(!app.browse.folder_classification_pending_for(&path));
        assert!(!app.browse.has_valid_folder_classification_for_identity(&path, stale_identity));
    }
}
