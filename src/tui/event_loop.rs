//! Async event loop: crossterm events + progress messages

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use crossterm::event::{self, Event};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc;

use super::app::{
    ActiveOverlay, AppScreen, AppState, CompletionOperationKind, TextEditTarget,
};
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

/// Recover database-owned full-file rollback transactions before scanning the
/// browse directory for standalone FLAC/DSF sidecars. A byte-identical legacy
/// DSF `.tonepoet-bak` may still be the authoritative marker for a PREPARED DB
/// entry; retiring it first would strand that transaction as unrecoverable.
fn startup_metadata_recovery_messages(
    db: &crate::db::Database,
    dir: &std::path::Path,
) -> Vec<String> {
    let mut messages = db.recover_stale_metadata_writes();
    messages.extend(super::probe::recover_stale_flac_metadata_journals_in_dir(dir));
    messages
}

/// Run the main TUI event loop
pub async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut AppState,
    tx: mpsc::Sender<AppMessage>,
    mut rx: mpsc::Receiver<AppMessage>,
) -> io::Result<()> {
    let _ = tui_file_picker::set_shared_clipboard_publish_hook(
        super::context_menu::publish_system_clipboard,
    );
    super::host_clipboard::configure_message_sender(tx.clone());
    // Set the message channel on BrowseState so navigation methods can
    // spawn async scans. Must happen before the event loop starts.
    app.browse.set_tx(tx.clone());
    app.browse.start_initial_async_scan();
    app.tui_tx = Some(tx.clone());
    // Recover both explicit metadata-write models at startup. Database-owned
    // PREPARED entries must run first because their authoritative backup can
    // look byte-identical to the current DSF and must not be retired by the
    // standalone directory scanner before the DB transaction consumes it.
    for message in startup_metadata_recovery_messages(&app.db, &app.browse.current_dir) {
        app.set_status(message);
    }
    // Opt-in tmux/byobu OSC 52 setup ([ui] manage_tmux_clipboard). Quiet
    // unless it changed something or failed.
    if let Some(message) = super::tmux_clipboard::apply_if_enabled(&app.config) {
        app.set_status(message);
    }
    let mut deferred_browse_visible_messages: VecDeque<AppMessage> = VecDeque::new();

    loop {
        // Check whether terminal input is already queued before firing any
        // settled-focus Browse work. Key-repeat PgUp/PgDn can otherwise leave
        // an event waiting while the old debounce expires, causing periodic
        // folder classification/probe/stat work in the middle of scrolling.
        let input_waiting_at_frame_start = event::poll(Duration::from_millis(0))?;

        // 1. Refresh items from the manager. Startup journal recoveries and
        // any queue left waiting behind a transient modal claim the serial
        // execution slot as soon as presentation state permits.
        super::keybindings::maybe_start_next_file_transfer(app, &tx);
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
        if app.current_screen != AppScreen::Browse && app.cancel_all_archive_listings() {
            app.set_status("archive listings cancelled: Browse screen changed");
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
                app.convert.source.cue_artifact_metadata.clear();
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
            if defer_quit_for_queued_file_transfers(app) {
                continue;
            }
            app.cancel_all_archive_listings();
            app.convert.clear_pending_archive_preview();
            app.convert.source.mode.cleanup_archive_preview_staging();
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
                Event::Paste(text) => handle_paste(app, &text, &tx),
                Event::Resize(_, _) => app.refresh_image_picker_after_resize(),
                _ => {}
            }
            reconcile_tags_mb_apply_operation_state(app);
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
        reconcile_tags_mb_apply_operation_state(app);
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
        | AppMessage::DirScanBatch { .. }
        | AppMessage::DirScanComplete { .. }
        | AppMessage::BrowseTreeChildrenComplete { .. }
        | AppMessage::PathValidationComplete { .. }
        | AppMessage::SearchComplete { .. }
        | AppMessage::ArchiveListingComplete { .. }
        | AppMessage::BookmarkTargetsLoaded { .. }
        | AppMessage::BookmarkActivationResolved { .. }
        | AppMessage::BookmarkDetailStarted { .. }
        | AppMessage::BookmarkDetailLoaded { .. }
        | AppMessage::MetadataWriteComplete { .. }
        | AppMessage::TagTransferPrepared { .. }
        | AppMessage::TagTransferComplete { .. }
        | AppMessage::OffsetCorrectionComplete { .. }
        | AppMessage::CtdbRepairComplete { .. } => true,
        AppMessage::CueWriteComplete { refresh_browse, .. } => *refresh_browse,
        AppMessage::FilePickerComplete { purpose, .. } => matches!(
            purpose,
            super::app::FilePickerPurpose::CopyTo { .. }
                | super::app::FilePickerPurpose::MoveTo { .. }
                | super::app::FilePickerPurpose::BrowseTagTransfer { .. }
        ),
        AppMessage::FileTaskProgress { update, .. } => matches!(
            update,
            tui_file_picker::FileTaskProgressUpdate::Finished { .. }
                | tui_file_picker::FileTaskProgressUpdate::Failed { .. }
                | tui_file_picker::FileTaskProgressUpdate::Aborted { .. }
        ),
        AppMessage::FileTaskComplete { .. }
        | AppMessage::RenamePlanComplete { .. }
        | AppMessage::FileOperationReplayComplete { .. } => true,
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

fn defer_quit_for_queued_file_transfers(app: &mut AppState) -> bool {
    let queued_count = app.file_transfers.queued.len();
    if queued_count == 0 || app.quit_with_queued_file_transfers_confirmed {
        return false;
    }

    let journal_backed_count = app
        .file_transfers
        .queued
        .iter()
        .filter(|queued| queued.is_journal_backed())
        .count();
    let unjournaled_count = queued_count.saturating_sub(journal_backed_count);

    app.should_quit = false;
    if matches!(
        &app.active_overlay,
        ActiveOverlay::Confirmation {
            action: super::app::ConfirmAction::QuitWithQueuedFileTransfers { .. },
            ..
        }
    ) {
        return true;
    }
    let displaced = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
    if !matches!(displaced, ActiveOverlay::None) {
        app.queued_quit_preempted_overlay = Some(Box::new(displaced));
    }
    let mut consequences = Vec::new();
    if unjournaled_count > 0 {
        let transfer_grammar = if unjournaled_count == 1 {
            "transfer has"
        } else {
            "transfers have"
        };
        consequences.push(format!(
            "{unjournaled_count} unstarted {transfer_grammar} no journal and will be lost.",
        ));
    }
    if journal_backed_count > 0 {
        consequences.push(format!(
            "{journal_backed_count} journal-backed recovery transfer{} will leave the current in-memory queue but remain pending for the next startup.",
            if journal_backed_count == 1 { "" } else { "s" },
        ));
    }
    let confirm_verb = match (unjournaled_count > 0, journal_backed_count > 0) {
        (true, true) => {
            "Y discards the unjournaled work, defers the journal-backed recovery work, and quits."
        }
        (true, false) => "Y discards the queued transfers and quits.",
        (false, true) => {
            "Y clears the in-memory recovery queue and quits; the journals remain pending for startup recovery."
        }
        (false, false) => unreachable!("non-empty queue must have a classified entry"),
    };
    app.active_overlay = ActiveOverlay::Confirmation {
        message: format!(
            "Quit with {queued_count} queued file transfer{}?

The running transfer, if any, has journal protection. {}

{confirm_verb} N/Esc keeps the queue and returns to the app.",
            if queued_count == 1 { "" } else { "s" },
            consequences.join(" "),
        ),
        action: super::app::ConfirmAction::QuitWithQueuedFileTransfers {
            unjournaled_count,
            journal_backed_count,
        },
    };
    app.set_status(match (unjournaled_count > 0, journal_backed_count > 0) {
        (true, true) => "Quit requires confirmation: unjournaled queued transfers will be lost; journal-backed recoveries remain pending",
        (true, false) => "Quit requires confirmation because unjournaled queued file transfers are not persisted",
        (false, true) => "Quit requires confirmation; journal-backed recoveries will be deferred to the next startup",
        (false, false) => unreachable!("non-empty queue must have a classified entry"),
    });
    true
}

fn defer_quit_for_browse_archive_metadata(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
) -> bool {
    if app
        .pending_metadata_editor
        .as_ref()
        .is_some_and(|state| state.any_presentation_dirty())
    {
        app.should_quit = false;
        app.set_status(
            "quit blocked: a parked metadata editor has unsaved changes; return to it and save or discard"
                .to_string(),
        );
        return true;
    }

    let invalid_ape_repair_cancel = match &mut app.active_overlay {
        ActiveOverlay::MetadataEditor(state)
            if state.phase == super::app::MetadataEditorPhase::Saving
                && state.invalid_ape_repair.is_some() => Some(state.cancel_metadata_write()),
        _ => None,
    };
    if let Some(requested) = invalid_ape_repair_cancel {
        app.should_quit = false;
        app.set_status(if requested {
            "quit deferred: invalid-APE repair cancellation requested; waiting for the worker's classified completion ledger"
                .to_string()
        } else {
            "quit deferred: invalid-APE repair has passed a cancellation point; waiting for its classified completion ledger"
                .to_string()
        });
        return true;
    }

    if let ActiveOverlay::MetadataEditor(state) = &app.active_overlay {
        let browse_archive_owned = state
            .archive_edit_context
            .as_ref()
            .is_some_and(|context| {
                context.owner == super::app::ArchiveMetadataEditOwner::Browse
            });
        if !browse_archive_owned
            && (state.any_presentation_dirty()
                || state.phase == super::app::MetadataEditorPhase::Saving
                || state.replaygain_scan.is_some()
                || state.artwork_write.is_some())
        {
            app.should_quit = false;
            app.set_status(
                "quit blocked: metadata editor has unsaved changes or an active write; save or discard before quitting"
                    .to_string(),
            );
            return true;
        }
    }

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

    // First-edit archive workers own staging lifecycle until their terminal
    // reducer runs. Defer rename/delete before attempting to drain any clean
    // tab staging: `exit_browse_archive` intentionally refuses to tear down an
    // archive while one of these mutations is still reconciling.
    if app.pending_browse_archive_rename.is_some() {
        app.should_quit = false;
        app.quit_after_browse_archive_rename = true;
        app.set_status("quit deferred: waiting for archive rename to finish".to_string());
        return true;
    }
    if app.pending_browse_archive_delete.is_some() {
        app.should_quit = false;
        app.quit_after_browse_archive_delete = true;
        app.set_status("quit deferred: waiting for archive delete to finish".to_string());
        return true;
    }

    // Metadata preparation has not committed user edits yet. Cancel it before
    // draining tab-owned staging so `exit_browse_archive` cannot keep refusing
    // the same clean staging session forever. `from_existing` preparations do
    // not own the Browse staging tree, so cancellation safely leaves that tree
    // for the normal clean/dirty drain below.
    if let Some(pending) = app.pending_browse_archive_metadata.take() {
        pending.cancel_and_cleanup();
    }

    // Archive staging is tab-owned. Drain clean sessions synchronously and
    // serialize dirty repackages before allowing process exit; checking only
    // the focused tab would strand staging owned by a background tab.
    while let Some(tab_id) = app.browse.first_archive_staging_tab_id() {
        if app.browse.active_tab_id() != tab_id {
            let _ = app.browse.switch_to_tab_id(tab_id);
        }
        let dirty = app
            .browse
            .active_archive_staging()
            .is_some_and(|staging| staging.dirty);
        if dirty {
            app.should_quit = false;
            app.quit_after_browse_archive_repackage = true;
            super::keybindings::exit_browse_archive(app, tx);
            app.set_status("quit deferred: saving staged archive changes".to_string());
            return true;
        }
        // Clean staging is still lifecycle-owned temporary state and must be
        // cleaned by the exact same path as ordinary archive exit.
        super::keybindings::exit_browse_archive(app, tx);
        if app.browse.first_archive_staging_tab_id() == Some(tab_id) {
            // Defensive liveness backstop: if a future exit guard is added
            // without a matching quit preflight, never spin on the same tab.
            // Known in-flight archive-edit guards are handled above and resume
            // quit automatically; an unknown blocker leaves the app responsive
            // so the user can retry after it resolves.
            app.should_quit = false;
            app.set_status(
                "quit deferred: archive staging cleanup is waiting for an in-flight operation"
                    .to_string(),
            );
            return true;
        }
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

    super::keybindings::metadata_editor_prepare_for_competing_workflow(app, &mut state);

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
        Some((_name, kind)) if !matches!(kind, EntryKind::ParentDir) => {
            super::keybindings::begin_browse_inline_rename(app, path);
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
                .map(|values| values.as_str().to_string())
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
    // loudgain rewrites each ReplayGain key as at most one text carrier per
    // file. Refresh the carrier counts with the values instead of retaining
    // stale duplicate-frame counts from the pre-scan editor snapshot.
    let stored_value_counts = values
        .iter()
        .map(|value| if value.trim().is_empty() { 0 } else { 1 })
        .collect::<Vec<_>>();
    if idx == surface.entries.len() {
        surface.entries.push(super::probe::TagEntry {
            row_scope: crate::tui::probe::RowScope::File,
            display_key: label.to_string(),
            item_key,
            value: value.clone(),
            original: value,
            is_binary: false,
            is_mixed,
            has_multiple_stored_values: false,
            per_file_stored_value_counts: stored_value_counts,
            per_file_values: crate::tui::probe::metadata_field_values_from_scalars(values.clone()),
            per_file_originals: crate::tui::probe::metadata_field_values_from_scalars(values),
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });
    } else if let Some(entry) = surface.entries.get_mut(idx) {
        entry.value = value.clone();
        entry.original = value;
        entry.is_binary = false;
        entry.is_mixed = is_mixed;
        entry.has_multiple_stored_values = false;
        entry.per_file_stored_value_counts = stored_value_counts;
        entry.per_file_values = crate::tui::probe::metadata_field_values_from_scalars(values.clone());
        entry.per_file_originals = crate::tui::probe::metadata_field_values_from_scalars(values);
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

const MAX_TAG_BLOCK_FILE_BYTES: u64 = 8 * 1024 * 1024;

fn read_tag_blocks_file_bounded(
    path: &std::path::Path,
    cancel: &super::probe::MetadataWriteCancelFlag,
) -> Result<Vec<super::tag_interchange::FieldBlock>, String> {
    use std::io::Read;

    if cancel.is_cancelled() {
        return Err("tag-block file read cancelled".to_string());
    }

    let file = std::fs::File::open(path)
        .map_err(|error| format!("cannot open '{}': {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect '{}': {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("'{}' is not a regular file", path.display()));
    }
    if metadata.len() > MAX_TAG_BLOCK_FILE_BYTES {
        return Err(format!(
            "tag-block file is too large ({} bytes; limit {})",
            metadata.len(),
            MAX_TAG_BLOCK_FILE_BYTES,
        ));
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    let mut reader = file.take(MAX_TAG_BLOCK_FILE_BYTES.saturating_add(1));
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err("tag-block file read cancelled".to_string());
        }
        let read = reader
            .read(&mut chunk)
            .map_err(|error| format!("cannot read '{}': {error}", path.display()))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    if bytes.len() as u64 > MAX_TAG_BLOCK_FILE_BYTES {
        return Err(format!(
            "tag-block file grew beyond the {}-byte limit while being read",
            MAX_TAG_BLOCK_FILE_BYTES
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|error| format!("'{}' is not valid UTF-8: {error}", path.display()))?;
    super::tag_interchange::parse_field_blocks(&text).map_err(|error| error.to_string())
}

fn append_ignored_directory_disclosure(app: &mut AppState, ignored_directories: usize) {
    if ignored_directories == 0 {
        return;
    }
    let suffix = format!(
        "({} director{} ignored)",
        ignored_directories,
        if ignored_directories == 1 { "y" } else { "ies" },
    );
    let current = app
        .status_message
        .as_ref()
        .map(|(message, _)| message.clone())
        .unwrap_or_else(|| "file picker: selection completed".to_string());
    app.set_status(format!("{current} {suffix}"));
}

fn file_picker_purpose_uses_single_path(purpose: &super::app::FilePickerPurpose) -> bool {
    !matches!(
        purpose,
        super::app::FilePickerPurpose::BrowseTagTransfer { .. }
            | super::app::FilePickerPurpose::MetadataTagTransfer { .. }
    )
}

fn append_first_of_many_disclosure(
    app: &mut AppState,
    purpose: &super::app::FilePickerPurpose,
    selected_count: usize,
) {
    if selected_count <= 1 || !file_picker_purpose_uses_single_path(purpose) {
        return;
    }
    let current = app
        .status_message
        .as_ref()
        .map(|(message, _)| message.clone())
        .unwrap_or_else(|| "file picker: selection completed".to_string());
    app.set_status(format!(
        "{current} (first of {selected_count} selected files used)"
    ));
}

fn reduce_file_picker_complete(
    app: &mut AppState,
    session_id: u64,
    purpose: super::app::FilePickerPurpose,
    path: Option<std::path::PathBuf>,
    paths: Vec<std::path::PathBuf>,
    tx: &mpsc::Sender<AppMessage>,
) -> bool {
    let mut consumed = true;
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
                        if let Some(open_picker) = state.file_picker.as_ref() {
                            super::keybindings::persist_file_picker_sort_if_changed(
                                app,
                                &open_picker.picker,
                            );
                            app.last_artwork_picker_dir =
                                Some(open_picker.current_dir().to_path_buf());
                        }
                        state.file_picker = None;
                        state.pending_artwork_type = None;
                    }

                    if !matches_open_picker {
                        consumed = false;
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
                    consumed = false;
                    app.active_overlay = other;
                    app.set_status("file picker: ignored metadata-artwork completion without an active editor");
                }
            }
        }
        super::app::FilePickerPurpose::MetadataTagBlocksFile => {
            let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
            match overlay {
                ActiveOverlay::MetadataEditor(mut state) => {
                    let matches_open_picker = state
                        .file_picker
                        .as_ref()
                        .is_some_and(|picker| {
                            picker.session_id == session_id && picker.purpose == purpose
                        });
                    if matches_open_picker {
                        if let Some(open_picker) = state.file_picker.as_ref() {
                            super::keybindings::persist_file_picker_sort_if_changed(
                                app,
                                &open_picker.picker,
                            );
                        }
                        state.file_picker = None;
                    }
                    if !matches_open_picker {
                        consumed = false;
                        app.set_status("file picker: ignored stale tag-block completion");
                    } else if let Some(path) = path {
                        let editor_session = metadata_editor_session_guard(&state);
                        let editor_fingerprint =
                            super::tag_interchange::metadata_editor_transfer_fingerprint(&state);
                        let (request_id, prepare_cancel) =
                            state.begin_tag_transfer_preparation();
                        let worker_tx = tx.clone();
                        let worker_path = path.clone();
                        app.set_status(format!(
                            "metadata editor: reading tag blocks from {}...",
                            path.display()
                        ));
                        tokio::spawn(async move {
                            let result = tokio::task::spawn_blocking(move || {
                                read_tag_blocks_file_bounded(&worker_path, &prepare_cancel)
                            })
                            .await
                            .unwrap_or_else(|error| {
                                Err(format!("tag-block file worker failed: {error}"))
                            });
                            let _ = worker_tx
                                .send(AppMessage::MetadataTagBlocksFilePrepared {
                                    request_id,
                                    editor_session,
                                    editor_fingerprint,
                                    path,
                                    result,
                                })
                                .await;
                        });
                    } else {
                        app.set_status("metadata editor: tag-block file picker cancelled");
                    }
                    app.active_overlay = ActiveOverlay::MetadataEditor(state);
                }
                other => {
                    consumed = false;
                    app.active_overlay = other;
                    app.set_status(
                        "file picker: ignored tag-block completion without an active editor",
                    );
                }
            }
        }
        super::app::FilePickerPurpose::MetadataTagTransfer {
            direction,
            scope,
            field_key,
            metadata_target_priority,
        } => {
            let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
            match overlay {
                ActiveOverlay::MetadataEditor(mut state) => {
                    let matches_open_picker = state
                        .file_picker
                        .as_ref()
                        .is_some_and(|picker| {
                            picker.session_id == session_id && picker.purpose == purpose
                        });
                    if matches_open_picker {
                        if let Some(open_picker) = state.file_picker.as_ref() {
                            super::keybindings::persist_file_picker_sort_if_changed(
                                app,
                                &open_picker.picker,
                            );
                        }
                        state.file_picker = None;
                    }
                    if !matches_open_picker {
                        app.set_status("file picker: ignored stale editor tag-transfer completion");
                        app.active_overlay = ActiveOverlay::MetadataEditor(state);
                        return false;
                    }
                    let selected_roots = if paths.is_empty() {
                        path.into_iter().collect::<Vec<_>>()
                    } else {
                        paths
                    };
                    if selected_roots.is_empty() {
                        app.set_status("metadata editor: tag transfer cancelled");
                        app.active_overlay = ActiveOverlay::MetadataEditor(state);
                        return true;
                    }
                    let editor_session = metadata_editor_session_guard(&state);
                    let editor_fingerprint =
                        super::tag_interchange::metadata_editor_transfer_fingerprint(&state);
                    let (request_id, prepare_cancel) =
                        state.begin_tag_transfer_preparation();
                    match direction {
                        super::app::TagTransferDirection::To => {
                            let (mut source_entries, source_dimension) =
                                super::tag_interchange::metadata_editor_transfer_snapshot(&state);
                            if let Some(field_key) = field_key.as_deref() {
                                source_entries.retain(|entry| {
                                    super::keybindings::metadata_field_keys_match(
                                        &entry.display_key,
                                        field_key,
                                    )
                                });
                                if source_entries.len() != 1 {
                                    app.set_status(format!(
                                        "metadata editor: selected field {field_key} is no longer uniquely available"
                                    ));
                                    app.active_overlay = ActiveOverlay::MetadataEditor(state);
                                    return true;
                                }
                            }
                            let worker_tx = tx.clone();
                            let worker_priority = metadata_target_priority.clone();
                            let worker_field_key = field_key.clone();
                            app.set_status("metadata editor: resolving tag-transfer target carrier...");
                            tokio::spawn(async move {
                                let result = tokio::task::spawn_blocking(move || {
                                    super::keybindings::classify_tag_transfer_target_roots(
                                        &selected_roots,
                                        &worker_priority,
                                        &prepare_cancel,
                                    )
                                })
                                .await
                                .unwrap_or_else(|error| {
                                    Err(format!("tag-transfer target worker failed: {error}"))
                                });
                                let _ = worker_tx
                                    .send(AppMessage::MetadataTagTransferTargetsPrepared {
                                        request_id,
                                        editor_session,
                                        editor_fingerprint,
                                        scope,
                                        field_key: worker_field_key,
                                        source_entries,
                                        source_dimension,
                                        result,
                                    })
                                    .await;
                            });
                        }
                        super::app::TagTransferDirection::From => {
                            let worker_tx = tx.clone();
                            let worker_priority = metadata_target_priority.clone();
                            let worker_field_key = field_key.clone();
                            app.set_status("metadata editor: reading tag-transfer source carrier...");
                            tokio::spawn(async move {
                                let result = tokio::task::spawn_blocking(move || {
                                    let carrier = super::keybindings::classify_tag_transfer_roots(
                                        &selected_roots,
                                        &worker_priority,
                                        &prepare_cancel,
                                    )?;
                                    let dimension = carrier.dimension();
                                    let carrier_label = carrier.label().to_string();
                                    let entries =
                                        super::tag_interchange::read_transfer_carrier_entries(
                                            &carrier,
                                            scope,
                                            &prepare_cancel,
                                        )?;
                                    Ok((entries, dimension, carrier_label))
                                })
                                .await
                                .unwrap_or_else(|error| {
                                    Err(format!("tag-transfer source worker failed: {error}"))
                                });
                                let _ = worker_tx
                                    .send(AppMessage::MetadataTagTransferSourcePrepared {
                                        request_id,
                                        editor_session,
                                        editor_fingerprint,
                                        scope,
                                        field_key: worker_field_key,
                                        result,
                                    })
                                    .await;
                            });
                        }
                    }
                    app.active_overlay = ActiveOverlay::MetadataEditor(state);
                }
                other => {
                    consumed = false;
                    app.active_overlay = other;
                    app.set_status(
                        "file picker: ignored editor tag-transfer completion without an active editor",
                    );
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
                return false;
            }
            let Some(dest_dir) = path else {
                app.set_status(format!("{op} cancelled"));
                return true;
            };
            if !dest_dir.is_dir() {
                app.set_status(format!(
                    "{op} destination is not a directory: {}",
                    dest_dir.display()
                ));
                return true;
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
        super::app::FilePickerPurpose::BrowseTagTransfer {
            direction,
            scope,
            fixed_roots,
            metadata_target_priority,
        } => {
            if !close_matching_file_picker(app, session_id, &purpose) {
                app.set_status("file picker: ignored stale tag-transfer completion");
                return false;
            }
            let selected_roots = if paths.is_empty() {
                path.into_iter().collect::<Vec<_>>()
            } else {
                paths
            };
            if selected_roots.is_empty() {
                app.set_status("Transfer tags cancelled");
                return true;
            }
            let (source_roots, target_roots) = match direction {
                super::app::TagTransferDirection::To => (fixed_roots, selected_roots),
                super::app::TagTransferDirection::From => (selected_roots, fixed_roots),
            };
            super::context_menu::start_tag_transfer(
                app,
                source_roots,
                target_roots,
                scope,
                metadata_target_priority,
                tx,
            );
        }
        super::app::FilePickerPurpose::SelectDestination => {
            if !close_matching_file_picker(app, session_id, &purpose) {
                app.set_status("file picker: ignored stale destination completion");
                return false;
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
                return false;
            }
            let Some(path) = path else {
                app.set_status("preset load cancelled");
                return true;
            };
            let Some(name) = path.file_stem().and_then(|name| name.to_str()).map(str::to_string) else {
                app.set_status(format!("invalid preset path: {}", path.display()));
                return true;
            };
            match super::presets::load_preset_from_path(&path) {
                Ok(preset) => {
                    let semantics = preset.resolved_semantics_summary();
                    let report = preset.apply_to_pills(
                        &mut app.convert.format,
                        &mut app.convert.output_options,
                        &mut app.convert.metadata,
                    );
                    app.preset.set_active_preset_path(name.clone(), path.clone());
                    app.preset.modified = false;
                    app.set_status(format!(
                        "Loaded preset: {}{} | {}",
                        path.display(),
                        report.status_suffix(),
                        semantics
                    ));
                }
                Err(e) => app.set_status(format!("Load failed: {}", e)),
            }
        }
        super::app::FilePickerPurpose::SavePreset => {
            if !close_matching_file_picker(app, session_id, &purpose) {
                app.set_status("file picker: ignored stale preset-save completion");
                return false;
            }
            let Some(path) = path else {
                app.set_status("preset save cancelled");
                return true;
            };
            let Some(name) = path.file_stem().and_then(|name| name.to_str()).map(str::to_string) else {
                app.set_status(format!("invalid preset path: {}", path.display()));
                return true;
            };
            let preset = super::presets::TuiPreset::from_pill_state(
                &name,
                &app.convert.format,
                &app.convert.output_options,
                &app.convert.metadata,
            );
            match super::presets::save_preset_to_path_with_db(&preset, &path, &app.db) {
                Ok(outcome) => {
                    app.preset.set_active_preset_path(name.clone(), path.clone());
                    app.preset.modified = false;
                    if let Some(warning) = outcome.index_warning() {
                        app.set_status(format!("Saved preset: {}; SQLite index update failed and will be repaired on startup: {warning}", path.display()));
                    } else {
                        app.set_status(format!(
                            "Saved preset: {} | {}", path.display(), preset.resolved_semantics_summary()
                        ));
                    }
                }
                Err(e) => app.set_status(format!("Save failed: {}", e)),
            }
        }
        super::app::FilePickerPurpose::Generic { id } => {
            if !close_matching_file_picker(app, session_id, &purpose) {
                app.set_status(format!("file picker purpose '{id}': ignored stale completion"));
                return false;
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
                return false;
            }
            match path {
                Some(path) => app.set_status(format!("file picker selected {}", path.display())),
                None => app.set_status("file picker cancelled"),
            }
        }
    }
    consumed

}

fn reduce_file_task_progress(
    app: &mut AppState,
    session_id: u64,
    update: tui_file_picker::FileTaskProgressUpdate,
    tx: &mpsc::Sender<AppMessage>,
) {
    let retained_update = update.clone();
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

    let authoritative_session_id = app
        .last_file_task_progress
        .as_ref()
        .map(|(retained_session_id, _)| *retained_session_id);
    let update_is_stale = authoritative_session_id.is_some_and(|id| id > session_id);
    let demands_attention = matches!(
        &update,
        tui_file_picker::FileTaskProgressUpdate::ShowConflict { .. }
            | tui_file_picker::FileTaskProgressUpdate::Failed { .. }
            | tui_file_picker::FileTaskProgressUpdate::Snapshot {
                phase: tui_file_picker::FileTaskPhase::Stalled,
                ..
            }
    );
    let mut status_to_set: Option<String> = None;
    let mut refresh_after_terminal = false;
    let mut live_progress_snapshot = None;
    let mut handled_background_session = false;
    if app
        .queued_quit_preempted_overlay
        .as_deref()
        .is_some_and(|overlay| {
            matches!(
                overlay,
                ActiveOverlay::FileTaskProgress(session)
                    if session.session_id == session_id && session.is_live_task()
            )
        })
    {
        if let Some(ActiveOverlay::FileTaskProgress(session)) = app
            .queued_quit_preempted_overlay
            .as_deref_mut()
        {
            session.progress.apply_update(update.clone());
            live_progress_snapshot = Some(session.progress.clone());
            status_to_set = status.clone();
            refresh_after_terminal = terminal;
            handled_background_session = true;
        }
        if demands_attention {
            if let Some(parked) = app.queued_quit_preempted_overlay.take() {
                match *parked {
                    ActiveOverlay::FileTaskProgress(session) => {
                        let displaced = std::mem::replace(
                            &mut app.active_overlay,
                            ActiveOverlay::FileTaskProgress(session),
                        );
                        if !matches!(displaced, ActiveOverlay::None) {
                            app.file_task_preempted_overlay = Some(Box::new(displaced));
                        }
                    }
                    other => {
                        app.queued_quit_preempted_overlay = Some(Box::new(other));
                    }
                }
            }
        }
    }

    if !handled_background_session
        && app
            .minimized_file_task_progress
            .as_ref()
            .is_some_and(|session| session.session_id == session_id && session.is_live_task())
    {
        if let Some(session) = app.minimized_file_task_progress.as_mut() {
            session.progress.apply_update(update.clone());
            live_progress_snapshot = Some(session.progress.clone());
            status_to_set = status.clone();
            refresh_after_terminal = terminal;
            handled_background_session = true;
        }
        if demands_attention {
            if let Some(session) = app.minimized_file_task_progress.take() {
                app.file_transfers.keep_minimized_across_jobs = false;
                let displaced = std::mem::replace(
                    &mut app.active_overlay,
                    ActiveOverlay::FileTaskProgress(session),
                );
                if !matches!(displaced, ActiveOverlay::None) {
                    app.file_task_preempted_overlay = Some(Box::new(displaced));
                }
            }
        }
    }

    if !handled_background_session {
        match &mut app.active_overlay {
            ActiveOverlay::FileTaskProgress(session)
                if session.session_id == session_id && session.is_live_task() =>
            {
                session.progress.apply_update(update.clone());
                live_progress_snapshot = Some(session.progress.clone());
                status_to_set = status;
                refresh_after_terminal = terminal;
            }
            ActiveOverlay::FileTaskProgress(session)
                if session.session_id == session_id && session.is_retained_viewer() =>
            {
                // Keep an open read-only viewer current for the same underlying
                // task, but never use its clone as ordering or retention authority.
                session.progress.apply_update(update.clone());
                if !update_is_stale {
                    status_to_set = status;
                    refresh_after_terminal = terminal;
                }
            }
            ActiveOverlay::FileTaskProgress(session) if session.is_live_task() => {
                if session.session_id > session_id {
                    status_to_set = Some(format!(
                        "file task: ignored stale progress for session {session_id}"
                    ));
                } else {
                    // The presentation layer still owns an older live overlay, but
                    // the newer task's session-owned retained state must progress.
                    status_to_set = status;
                    refresh_after_terminal = terminal;
                }
            }
            ActiveOverlay::FileTaskProgress(_) => {
                // A retained-results viewer is presentation-only and never
                // participates in session ordering. Authoritative retained state
                // below decides whether this update is current or stale.
                if !update_is_stale {
                    status_to_set = status;
                    refresh_after_terminal = terminal;
                }
            }
            _ if terminal && !update_is_stale => {
                status_to_set = status;
                refresh_after_terminal = true;
            }
            _ => {}
        }
    }

    if let Some(progress) = live_progress_snapshot {
        app.last_file_task_progress = Some((session_id, progress));
    } else {
        let mut install_terminal_fallback = false;
        match &mut app.last_file_task_progress {
            Some((retained_session_id, retained)) if *retained_session_id == session_id => {
                retained.apply_update(retained_update.clone());
            }
            Some((retained_session_id, _)) if *retained_session_id > session_id => {
                // A late update from an older task must not displace the newer
                // session's retained diagnostics.
            }
            Some(_) | None if terminal => {
                install_terminal_fallback = true;
            }
            Some(_) | None => {}
        }

        if install_terminal_fallback {
            // Defensive recovery for a task started by an older or external
            // host path that did not seed retained state. Production launch
            // sites use `AppState::install_file_task_progress`, so this is a
            // last-resort guarantee rather than the normal lifecycle.
            let mut retained = tui_file_picker::FileTaskProgressState::new(
                tui_file_picker::FileTaskKind::Custom("File task".to_string()),
                "File task",
                super::keybindings::file_picker_theme_from_theme(&app.theme),
            );
            retained.apply_update(retained_update);
            app.last_file_task_progress = Some((session_id, retained));
        }
    }
    if let Some(status) = status_to_set {
        app.set_status(status);
    }
    let defer_clipboard_refresh = terminal
        && (app
            .file_transfers
            .pending_by_session
            .contains_key(&session_id)
            || app.artwork_picker_file_tasks.contains_key(&session_id));
    if refresh_after_terminal && !defer_clipboard_refresh {
        app.browse.refresh_with_search(Some(tx));
        app.browse.probe_current_with_db(tx, Some(&app.db));
        super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
    }
}

fn terminal_update_from_completion_report(
    report: &tui_file_picker::FileTaskCompletionReport,
) -> tui_file_picker::FileTaskProgressUpdate {
    let mut totals = tui_file_picker::ProgressTotals {
        items_total: Some(report.roots.len() as u64),
        ..tui_file_picker::ProgressTotals::default()
    };
    let mut has_warning = false;
    for root in &report.roots {
        totals.items_done = totals.items_done.saturating_add(1);
        match root.disposition {
            tui_file_picker::FileTaskRootDisposition::Completed => {
                totals.completed = totals.completed.saturating_add(1);
            }
            tui_file_picker::FileTaskRootDisposition::CompletedWithWarning => {
                totals.completed = totals.completed.saturating_add(1);
                has_warning = true;
            }
            tui_file_picker::FileTaskRootDisposition::Skipped => {
                totals.skipped = totals.skipped.saturating_add(1);
            }
            tui_file_picker::FileTaskRootDisposition::Failed => {
                totals.errors = totals.errors.saturating_add(1);
            }
            tui_file_picker::FileTaskRootDisposition::NotAttempted => {
                totals.not_attempted = totals.not_attempted.saturating_add(1);
            }
        }
    }

    if totals.errors > 0 || totals.not_attempted > 0 {
        tui_file_picker::FileTaskProgressUpdate::Failed {
            status: "File task completed with incomplete roots".to_string(),
            totals,
        }
    } else {
        tui_file_picker::FileTaskProgressUpdate::Finished {
            status: if has_warning {
                "File task completed with warnings".to_string()
            } else {
                "File task completed".to_string()
            },
            totals,
        }
    }
}

fn reconcile_artwork_picker_file_task(
    app: &mut AppState,
    pending: super::app::ArtworkPickerFileTask,
    report: &tui_file_picker::FileTaskCompletionReport,
    worker_retry_plan: Option<super::browse::BrowsePasteRetryPlan>,
) -> bool {
    let picker_session_id = pending.picker_session_id;
    let (completed_sources, completed_destinations, remaining_sources, is_move) =
        match &pending.request {
            tui_file_picker::FilePickerHostMutationRequest::Paste { clipboard, .. } => {
                let mut completed_sources = Vec::new();
                let mut completed_destinations = Vec::new();
                let mut remaining_sources = Vec::new();
                for mapping in &pending.plan.mappings {
                    let completed = report
                        .roots
                        .iter()
                        .find(|root| root.source == mapping.source)
                        .is_some_and(|root| root.disposition.is_completed());
                    if completed {
                        completed_sources.push(mapping.source.clone());
                        completed_destinations.push(mapping.destination.clone());
                    } else {
                        remaining_sources.push(mapping.source.clone());
                    }
                }
                (
                    completed_sources,
                    completed_destinations,
                    remaining_sources,
                    clipboard.mode() == tui_file_picker::FilePickerClipboardMode::Cut,
                )
            }
            _ => (Vec::new(), Vec::new(), Vec::new(), false),
        };

    // The progress reducer deliberately defers generic Browse refresh while a
    // hosted artwork transfer is pending. Reuse the normal nonblocking Browse
    // completion refresh here so a picker-owned paste cannot leave another
    // Tonepoet surface displaying stale source/destination rows.
    let destination_refreshed_tabs =
        refresh_browse_destination_views_after_file_task(app, &completed_destinations);
    if is_move && !completed_sources.is_empty() {
        refresh_browse_source_views_after_move(
            app,
            &completed_sources,
            &destination_refreshed_tabs,
        );
    }
    app.artwork_picker_paste_retries.remove(&picker_session_id);
    if !remaining_sources.is_empty() {
        if let Some(retry) = worker_retry_plan
            .as_ref()
            .and_then(|retry| retry.retain_sources(&remaining_sources))
        {
            app.artwork_picker_paste_retries
                .insert(picker_session_id, retry);
        }
    }

    fn reconcile_editor(
        editor: &mut Box<super::app::MetadataEditorState>,
        picker_session_id: u64,
        request: &tui_file_picker::FilePickerHostMutationRequest,
        plan: &tui_file_picker::PastePlan,
        report: &tui_file_picker::FileTaskCompletionReport,
    ) -> bool {
        let Some(session) = editor
            .file_picker
            .as_mut()
            .filter(|session| session.session_id == picker_session_id)
        else {
            return false;
        };
        let _ = session
            .picker
            .complete_host_paste(request.clone(), plan, report);
        true
    }

    if let ActiveOverlay::MetadataEditor(editor) = &mut app.active_overlay {
        if reconcile_editor(editor, picker_session_id, &pending.request, &pending.plan, report) {
            return true;
        }
    }
    if let Some(editor) = app.pending_metadata_editor.as_mut() {
        if reconcile_editor(editor, picker_session_id, &pending.request, &pending.plan, report) {
            return true;
        }
    }
    if let Some(ActiveOverlay::MetadataEditor(editor)) =
        app.file_task_preempted_overlay.as_deref_mut()
    {
        if reconcile_editor(editor, picker_session_id, &pending.request, &pending.plan, report) {
            return true;
        }
    }
    if let Some(ActiveOverlay::MetadataEditor(editor)) =
        app.pending_editor_context_overlay.as_deref_mut()
    {
        if reconcile_editor(editor, picker_session_id, &pending.request, &pending.plan, report) {
            return true;
        }
    }
    if let Some(ActiveOverlay::MetadataEditor(editor)) =
        app.queued_quit_preempted_overlay.as_deref_mut()
    {
        if reconcile_editor(editor, picker_session_id, &pending.request, &pending.plan, report) {
            return true;
        }
    }
    // No live picker can consume an in-memory exact retry token for this
    // session anymore. The durable file-task journal remains authoritative
    // for crash/startup recovery, so do not retain dead-session UI state.
    app.artwork_picker_paste_retries.remove(&picker_session_id);
    false
}

fn reduce_file_task_complete(
    app: &mut AppState,
    session_id: u64,
    mut report: tui_file_picker::FileTaskCompletionReport,
    worker_retry_plan: Option<super::browse::BrowsePasteRetryPlan>,
    tx: &mpsc::Sender<AppMessage>,
) {
    let terminal_update = terminal_update_from_completion_report(&report);
    let report_finished_cleanly = !report.roots.is_empty()
        && report.roots.iter().all(|root| {
            root.disposition == tui_file_picker::FileTaskRootDisposition::Completed
        });
    let mut should_auto_close = false;
    let mut active_progress_snapshot = None;
    if let ActiveOverlay::FileTaskProgress(session) = &mut app.active_overlay {
        if session.session_id == session_id {
            if !session.progress.is_terminal() {
                session.progress.apply_update(terminal_update.clone());
            }
            session.progress.append_completion_report(&report);
            if session.is_live_task() {
                should_auto_close = report_finished_cleanly
                    && session.progress.auto_close_available()
                    && session.progress.auto_close();
                active_progress_snapshot = Some(session.progress.clone());
            }
        }
    }
    let mut parked_auto_close = false;
    if let Some(ActiveOverlay::FileTaskProgress(session)) = app
        .queued_quit_preempted_overlay
        .as_deref_mut()
    {
        if session.session_id == session_id && session.is_live_task() {
            if !session.progress.is_terminal() {
                session.progress.apply_update(terminal_update.clone());
            }
            session.progress.append_completion_report(&report);
            parked_auto_close = report_finished_cleanly
                && session.progress.auto_close_available()
                && session.progress.auto_close();
            active_progress_snapshot = Some(session.progress.clone());
        }
    }
    if parked_auto_close {
        app.queued_quit_preempted_overlay = None;
    }
    if let Some(session) = app.minimized_file_task_progress.as_mut() {
        if session.session_id == session_id && session.is_live_task() {
            if !session.progress.is_terminal() {
                session.progress.apply_update(terminal_update.clone());
            }
            session.progress.append_completion_report(&report);
            active_progress_snapshot = Some(session.progress.clone());
        }
    }
    if let Some(progress) = active_progress_snapshot {
        app.last_file_task_progress = Some((session_id, progress));
    } else {
        let mut install_completion_fallback = false;
        match &mut app.last_file_task_progress {
            Some((retained_session_id, retained)) if *retained_session_id == session_id => {
                if !retained.is_terminal() {
                    retained.apply_update(terminal_update.clone());
                }
                retained.append_completion_report(&report);
            }
            Some((retained_session_id, _)) if *retained_session_id > session_id => {
                // A stale completion cannot replace diagnostics for a newer
                // task. The report still feeds any matching clipboard reducer
                // below, but it is not exposed as "most recent".
            }
            Some(_) | None => {
                install_completion_fallback = true;
            }
        }

        if install_completion_fallback {
            // Preserve the authoritative root report even if presentation
            // state was removed before both the terminal update and completion
            // report, or if an older retained session was still installed.
            let kind = if report.is_move {
                tui_file_picker::FileTaskKind::Move
            } else {
                tui_file_picker::FileTaskKind::Copy
            };
            let title = if report.is_move { "Moving files" } else { "Copying files" };
            let mut progress = tui_file_picker::FileTaskProgressState::new(
                kind,
                title,
                super::keybindings::file_picker_theme_from_theme(&app.theme),
            );
            progress.apply_update(terminal_update);
            progress.append_completion_report(&report);
            app.last_file_task_progress = Some((session_id, progress));
        }
    }

    let undo_record_warning =
        super::keybindings::record_completed_file_task_for_undo(app, session_id, &mut report);

    // Retention and undo-recording are authoritative and must complete before
    // presentation state is dismissed. Never close a newer or retained viewer.
    if should_auto_close
        && matches!(
            &app.active_overlay,
            ActiveOverlay::FileTaskProgress(session)
                if session.session_id == session_id && session.is_live_task()
        )
    {
        app.active_overlay = ActiveOverlay::None;
    }

    if let Some(pending_artwork) = app.artwork_picker_file_tasks.remove(&session_id) {
        let reconciled = reconcile_artwork_picker_file_task(
            app,
            pending_artwork,
            &report,
            worker_retry_plan,
        );
        let requires_attention = !report_finished_cleanly
            || undo_record_warning.is_some()
            || !reconciled;
        if let Some(warning) = undo_record_warning {
            app.set_status(format!(
                "artwork-picker file task completed, but undo was not retained: {warning}"
            ));
        } else if !reconciled {
            app.set_status(concat!(
                "artwork-picker file task completed after its picker closed; ",
                "filesystem result retained in task history",
            ));
        } else if report_finished_cleanly {
            app.set_status("Artwork picker file operation completed");
        } else {
            app.set_status("Artwork picker file operation completed with retryable or warning roots");
        }
        if app.file_transfers.active_session_id == Some(session_id) {
            finalize_file_transfer_scheduler(app, session_id, requires_attention, tx);
        }
        return;
    }

    let Some(pending) = app.file_transfers.pending_by_session.remove(&session_id) else {
        let requires_attention = !report_finished_cleanly || undo_record_warning.is_some();
        if let Some(warning) = undo_record_warning {
            app.set_status(format!(
                "file task completed, but undo was not retained: {warning}"
            ));
        }
        // Direct Copy/Move jobs own the active scheduler slot without a
        // clipboard reconciliation record. A duplicate or late completion for
        // an older session must not clear, block, or de-minimize a newer job.
        if app.file_transfers.active_session_id == Some(session_id) {
            finalize_file_transfer_scheduler(app, session_id, requires_attention, tx);
        }
        return;
    };

    let mut completed_sources = Vec::new();
    let mut completed_destinations = Vec::new();
    let mut retry_sources = Vec::new();
    let mut completion_warnings = Vec::new();
    let mut incomplete_details = Vec::new();
    let recovery_journal_retained = worker_retry_plan
        .as_ref()
        .or(pending.retry_plan.as_ref())
        .and_then(|retry| retry.recovery_journal_path.as_ref())
        .is_some();

    for source in pending.clipboard.paths().iter() {
        // The worker keeps the clipboard/source identity lexical but reports
        // the authoritative admitted destination used for filesystem I/O. A
        // followed destination alias can therefore make the terminal
        // destination differ from the original display mapping. Top-level
        // clipboard sources are unique, so reconcile by that stable logical
        // source identity rather than re-resolving or comparing the mutable
        // destination alias.
        let root = report
            .roots
            .iter()
            .find(|root| root.source == *source);
        // The isolated helper owns all filesystem verification and attaches
        // operation-time proofs to completed roots. The TUI reducer must not
        // re-stat a source or destination: either pathname may be a wedged
        // sshfs/removable mount, and a synchronous probe here would reintroduce
        // the very uninterruptible cancellation failure the supervisor avoids.
        let completed = root.is_some_and(|root| root.disposition.is_completed());
        if completed {
            let root = root.expect("completed root checked above");
            completed_sources.push(source.clone());
            completed_destinations.push(root.destination.clone());
            if root.disposition == tui_file_picker::FileTaskRootDisposition::CompletedWithWarning {
                completion_warnings.push(
                    root.message
                        .clone()
                        .unwrap_or_else(|| format!("completed {} with a warning", source.display())),
                );
            }
        } else {
            if let Some(root) = root {
                let detail = root.message.clone().unwrap_or_else(|| {
                    format!("{:?}", root.disposition)
                });
                incomplete_details.push(format!(
                    "{} → {}: {}",
                    source.display(),
                    root.destination.display(),
                    detail
                ));
            }
            // Incomplete roots remain part of the exact retry plan. The
            // durable worker journal, not a UI-thread pathname probe, decides
            // whether a remote/removable location is currently reachable.
            retry_sources.push(source.clone());
        }
    }

    if pending.clipboard.mode() == tui_file_picker::FilePickerClipboardMode::Cut
        && !completed_sources.is_empty()
    {
        if let Some(completed_clipboard) = tui_file_picker::FilesystemClipboard::new(
            tui_file_picker::FilePickerClipboardMode::Cut,
            completed_sources.clone(),
        ) {
            app.browse
                .remap_navigation_after_cut(&completed_clipboard, &completed_destinations);
        }
    }
    let all_completed = completed_sources.len() == pending.clipboard.paths().len();
    let clipboard_still_owned_by_job = pending
        .clipboard_owner_generation
        .is_some_and(|generation| {
            generation == app.browse.filesystem_clipboard_generation
                && app
                    .browse
                    .filesystem_clipboard
                    .as_ref()
                    .is_some_and(|clipboard| clipboard == &pending.clipboard)
        });
    if clipboard_still_owned_by_job {
        app.browse.filesystem_clipboard = if all_completed {
            if pending.clipboard.mode() == tui_file_picker::FilePickerClipboardMode::Copy {
                Some(pending.clipboard.clone())
            } else {
                None
            }
        } else {
            tui_file_picker::FilesystemClipboard::new(
                pending.clipboard.mode(),
                retry_sources.clone(),
            )
        };
        app.browse.filesystem_clipboard_retry_plan = if !all_completed {
            // Whichever plan is retained, the user-facing retry token must exclude
            // roots the worker proved complete/committed — matching the retained
            // clipboard above. The durable journal (whose path retain_sources
            // preserves) remains authoritative for those roots' deferred cleanup.
            worker_retry_plan
                .as_ref()
                .and_then(|retry| retry.retain_sources(&retry_sources))
                .or_else(|| {
                    pending
                        .retry_plan
                        .as_ref()
                        .and_then(|retry| retry.retain_sources(&retry_sources))
                })
        } else {
            None
        };
    }

    // Completion reduction must remain control-plane-only. Directory scans run
    // on the existing cancellable scan worker; tree rebuilding and selection
    // probes can synchronously touch a dead mount and therefore do not run here.
    // Refresh every open Browse tab that actually displays a successful
    // destination parent. Transfers can be minimized while the user switches
    // tabs, so the active tab is not necessarily the destination anymore.
    let destination_refreshed_tabs =
        refresh_browse_destination_views_after_file_task(app, &completed_destinations);
    if pending.clipboard.mode() == tui_file_picker::FilePickerClipboardMode::Cut
        && !completed_sources.is_empty()
    {
        refresh_browse_source_views_after_move(
            app,
            &completed_sources,
            &destination_refreshed_tabs,
        );
    }

    let mut status = if all_completed {
        format!(
            "Pasted {} item{}",
            completed_sources.len(),
            if completed_sources.len() == 1 { "" } else { "s" }
        )
    } else {
        format!(
            "Paste partially completed: {} completed, {} ready to retry",
            completed_sources.len(),
            retry_sources.len()
        )
    };
    if !all_completed && recovery_journal_retained {
        status.push_str("; durable reconciliation state retained");
    }
    if !completion_warnings.is_empty() {
        status.push_str(&format!(
            "; {} completed with cleanup/durability warning{}: {}",
            completion_warnings.len(),
            if completion_warnings.len() == 1 { "" } else { "s" },
            completion_warnings.join(" | ")
        ));
    }
    if !incomplete_details.is_empty() {
        const MAX_STATUS_DETAILS: usize = 3;
        let shown = incomplete_details
            .iter()
            .take(MAX_STATUS_DETAILS)
            .cloned()
            .collect::<Vec<_>>();
        status.push_str(&format!("; incomplete: {}", shown.join(" | ")));
        if incomplete_details.len() > shown.len() {
            status.push_str(&format!(
                " | … and {} more",
                incomplete_details.len().saturating_sub(shown.len())
            ));
        }
    }
    let requires_attention = !all_completed
        || !completion_warnings.is_empty()
        || !incomplete_details.is_empty()
        || undo_record_warning.is_some();
    if let Some(warning) = undo_record_warning {
        status.push_str(&format!("; undo was not retained: {warning}"));
    }
    if !clipboard_still_owned_by_job {
        status.push_str("; current clipboard was changed after enqueue and was left untouched");
    }
    if requires_attention {
        app.set_status(status);
    } else {
        app.set_routine_file_operation_status(status);
    }
    finalize_file_transfer_scheduler(app, session_id, requires_attention, tx);
}

fn refresh_browse_destination_views_after_file_task(
    app: &mut AppState,
    completed_destinations: &[std::path::PathBuf],
) -> std::collections::BTreeSet<crate::tui::browse::BrowseTabId> {
    let mut destination_targets = std::collections::BTreeMap::new();
    for destination in completed_destinations {
        let Some(parent) = destination.parent() else {
            continue;
        };
        destination_targets.insert(
            parent.to_path_buf(),
            destination
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
        );
    }
    if destination_targets.is_empty() {
        return std::collections::BTreeSet::new();
    }

    let tab_ids = app
        .browse
        .tab_infos()
        .into_iter()
        .map(|info| info.id)
        .collect::<Vec<_>>();
    let mut refreshed_tabs = std::collections::BTreeSet::new();
    for tab_id in tab_ids {
        let Some(tab) = app.browse.tab_mut(tab_id) else {
            continue;
        };
        let Some(restore_target) = destination_targets.get(&tab.current_dir) else {
            continue;
        };

        tab.cursor_restore_target = restore_target.clone();
        tab.cursor_restore_scroll_offset = None;
        tab.refresh_after_file_task_nonblocking();
        refreshed_tabs.insert(tab_id);
    }
    refreshed_tabs
}

fn refresh_browse_source_views_after_move(
    app: &mut AppState,
    completed_sources: &[std::path::PathBuf],
    destination_refreshed_tabs: &std::collections::BTreeSet<crate::tui::browse::BrowseTabId>,
) {
    let source_parents = completed_sources
        .iter()
        .filter_map(|source| source.parent().map(std::path::Path::to_path_buf))
        .collect::<std::collections::BTreeSet<_>>();
    if source_parents.is_empty() {
        return;
    }

    let tab_ids = app
        .browse
        .tab_infos()
        .into_iter()
        .map(|info| info.id)
        .collect::<Vec<_>>();
    for tab_id in tab_ids {
        let Some(tab) = app.browse.tab_mut(tab_id) else {
            continue;
        };

        // Tree invalidation is deterministic from the completed move plan and
        // requires no filesystem access. It also closes the expanded-sidebar
        // stale-node gap when no tab is currently displaying the source parent.
        tab.prune_tree_paths_after_move(completed_sources);

        // A tab already refreshed because it displays a successful destination
        // parent must not receive a second source scan/cancel cycle (notably for
        // same-directory moves). Every other tab displaying a source parent is
        // refreshed regardless of which tab happens to be active now.
        if source_parents.contains(&tab.current_dir)
            && !destination_refreshed_tabs.contains(&tab_id)
        {
            tab.refresh_after_file_task_nonblocking();
        }
    }
}

pub(super) fn finalize_file_transfer_scheduler(
    app: &mut AppState,
    session_id: u64,
    requires_attention: bool,
    tx: &mpsc::Sender<AppMessage>,
) {
    if app.file_transfers.active_session_id == Some(session_id) {
        app.file_transfers.active_session_id = None;
    }

    let minimized = app
        .minimized_file_task_progress
        .as_ref()
        .is_some_and(|session| session.session_id == session_id);
    if requires_attention {
        app.file_transfers.blocked_for_attention = true;
        app.file_transfers.keep_minimized_across_jobs = false;
        let mut restored_parked_progress = false;
        if let Some(parked) = app.queued_quit_preempted_overlay.take() {
            match *parked {
                ActiveOverlay::FileTaskProgress(session)
                    if session.session_id == session_id && session.is_live_task() =>
                {
                    let displaced = std::mem::replace(
                        &mut app.active_overlay,
                        ActiveOverlay::FileTaskProgress(session),
                    );
                    if !matches!(displaced, ActiveOverlay::None) {
                        app.file_task_preempted_overlay = Some(Box::new(displaced));
                    }
                    restored_parked_progress = true;
                }
                other => {
                    app.queued_quit_preempted_overlay = Some(Box::new(other));
                }
            }
        }
        if restored_parked_progress {
            // Attention preempted the quit confirmation; it is parked in
            // `file_task_preempted_overlay` and can return after resolution.
        } else if minimized {
            if let Some(session) = app.minimized_file_task_progress.take() {
                let displaced = std::mem::replace(
                    &mut app.active_overlay,
                    ActiveOverlay::FileTaskProgress(session),
                );
                if !matches!(displaced, ActiveOverlay::None) {
                    app.file_task_preempted_overlay = Some(Box::new(displaced));
                }
            }
        } else if !matches!(
            &app.active_overlay,
            ActiveOverlay::FileTaskProgress(session) if session.session_id == session_id
        ) {
            if let Some((retained_id, progress)) = app.last_file_task_progress.clone() {
                if retained_id == session_id {
                    app.active_overlay = ActiveOverlay::FileTaskProgress(
                        super::app::FileTaskProgressSession::retained_viewer(
                            retained_id,
                            progress,
                        ),
                    );
                }
            }
        }
        app.sync_file_transfer_queue_surfaces();
        return;
    }

    if minimized {
        app.minimized_file_task_progress = None;
    }
    let has_queued = !app.file_transfers.queued.is_empty();
    let should_release_visible_progress = has_queued || app.file_task_preempted_overlay.is_some();
    if should_release_visible_progress
        && matches!(
            &app.active_overlay,
            ActiveOverlay::FileTaskProgress(session) if session.session_id == session_id
        )
    {
        app.active_overlay = ActiveOverlay::None;
    }
    if matches!(app.active_overlay, ActiveOverlay::None) {
        if let Some(preempted) = app.file_task_preempted_overlay.take() {
            app.active_overlay = *preempted;
        }
    }
    if has_queued {
        super::keybindings::maybe_start_next_file_transfer(app, tx);
    } else {
        app.file_transfers.keep_minimized_across_jobs = false;
        app.sync_file_transfer_queue_surfaces();
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
            if matched {
                super::keybindings::persist_file_picker_sort_if_changed(app, &session.picker);
            } else {
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
                if let Some(open_picker) = state.file_picker.as_ref() {
                    super::keybindings::persist_file_picker_sort_if_changed(
                        app,
                        &open_picker.picker,
                    );
                }
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

/// True when a deliberate same-as-source rate is selected on a DSD target —
/// captured BEFORE a probe result is installed, because the clamp can fire
/// inside `set_source_mode` (identity promotion to Known runs constraints),
/// not only in the defaults block.
fn dsd_source_rate_sentinel_selected(app: &AppState) -> bool {
    app.convert.format.is_dsd_selected()
        && *app.convert.format.sample_rate.selected_value()
            == super::app::SOURCE_SAMPLE_RATE_SENTINEL
}

/// Return the user-visible consequence of clamping a deliberate
/// same-as-source rate. The clamp is correct once a PCM source is known, but
/// it must be composed with the current probe result rather than silently
/// overwriting a degradation warning.
fn source_rate_sentinel_clamp_message(
    app: &AppState,
    was_sentinel_selected: bool,
) -> Option<String> {
    if !was_sentinel_selected {
        return None;
    }
    let selected = *app.convert.format.sample_rate.selected_value();
    if selected == super::app::SOURCE_SAMPLE_RATE_SENTINEL {
        return None;
    }
    let target = app.convert.format.format.selected_label();
    Some(format!(
        "rate 'source' is invalid for {target} with a PCM source; set to {selected} Hz"
    ))
}

fn publish_probe_status_with_sentinel_clamp(
    app: &mut AppState,
    base_status: Option<String>,
    was_sentinel_selected: bool,
) {
    let clamp = source_rate_sentinel_clamp_message(app, was_sentinel_selected);
    match (base_status, clamp) {
        (Some(base), Some(clamp)) => app.set_status(format!("{base}; {clamp}")),
        (Some(base), None) => app.set_status(base),
        (None, Some(clamp)) => app.set_status(clamp),
        (None, None) => {}
    }
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
    let was_sentinel_selected = dsd_source_rate_sentinel_selected(app);

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
        // Installing source facts always preserves the current format
        // selection. The reducer below applies defaults only when its captured
        // format baseline still matches.
        app.convert.set_source_mode(source_mode);
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


    let status = if let Some(notice) = probe_notice {
        format!("Probe warning: {notice}")
    } else {
        format!(
            "Loaded: {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        )
    };
    publish_probe_status_with_sentinel_clamp(app, Some(status), was_sentinel_selected);
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

fn prompt_overlay_slot_is_unobstructed(app: &AppState) -> bool {
    matches!(app.active_overlay, ActiveOverlay::None)
        && app.pending_metadata_editor.is_none()
        && app.pending_cue_preview.is_none()
        && app.pending_mb_select.is_none()
        && app.active_tags_mb_operation.is_none()
        && app.active_gnudb_operation.is_none()
        && app.active_cue_operation.is_none()
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

    let was_sentinel_selected = dsd_source_rate_sentinel_selected(app);

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

            // Installing source facts always preserves the current format
            // selection. The reducer below applies defaults only when its
            // captured format baseline still matches.
            app.convert.set_source_mode(source_mode);

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

            let status = format!(
                "Archive preview loaded: {} track{}",
                track_count,
                if track_count == 1 { "" } else { "s" }
            );
            publish_probe_status_with_sentinel_clamp(
                app,
                Some(status),
                was_sentinel_selected,
            );
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
                if prompt_overlay_slot_is_unobstructed(app) {
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
                } else {
                    app.set_status(format!(
                        "Archive preview needs a password, but the current editor or overlay was preserved: {err}; retry preview after closing it"
                    ));
                }
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
                    app.pending_browse_archive_metadata = None;
                    if prompt_overlay_slot_is_unobstructed(app) {
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
                        app.set_status(format!(
                            "archive metadata editor needs archive password: {err}; enter password"
                        ));
                    } else {
                        app.set_status(format!(
                            "archive metadata editor needs a password, but the current editor or overlay was preserved: {err}; retry metadata edit after closing it"
                        ));
                    }
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

fn acquire_browse_archive_mutation_claim(
    claim: crate::concurrency::PathClaim,
) -> Result<crate::concurrency::MutationClaimGuard, String> {
    crate::concurrency::MutationClaimGuard::acquire_ephemeral(vec![claim])
}

#[cfg(test)]
mod browse_archive_mutation_claim_tests {
    use super::acquire_browse_archive_mutation_claim;
    use crate::concurrency::{ClaimMode, ClaimScope, MutationClaimGuard, PathClaim};

    #[test]
    fn same_archive_save_is_busy_while_disjoint_archive_remains_admissible() {
        let _coordination = crate::concurrency::scoped_test_coordination_root();
        let temp = tempfile::tempdir().expect("tempdir");
        let contested = temp.path().join("album.zip");
        let disjoint = temp.path().join("other.zip");
        std::fs::write(&contested, b"archive-a").unwrap();
        std::fs::write(&disjoint, b"archive-b").unwrap();

        let competing =
            PathClaim::resolve(&contested, ClaimMode::Write, ClaimScope::Exact).unwrap();
        let competing_guard = MutationClaimGuard::acquire_ephemeral(vec![competing]).unwrap();

        let same_claim =
            PathClaim::resolve(&contested, ClaimMode::Write, ClaimScope::Exact).unwrap();
        let error = acquire_browse_archive_mutation_claim(same_claim)
            .expect_err("same archive must be busy before repackaging starts");
        assert!(error.contains("live owner"), "unexpected busy error: {error}");
        assert_eq!(std::fs::read(&contested).unwrap(), b"archive-a");

        let disjoint_claim =
            PathClaim::resolve(&disjoint, ClaimMode::Write, ClaimScope::Exact).unwrap();
        let disjoint_guard = acquire_browse_archive_mutation_claim(disjoint_claim)
            .expect("different archives must remain concurrent");
        drop(disjoint_guard);

        drop(competing_guard);
        let retry_claim =
            PathClaim::resolve(&contested, ClaimMode::Write, ClaimScope::Exact).unwrap();
        acquire_browse_archive_mutation_claim(retry_claim)
            .expect("archive claim must release after the live owner finishes");
    }
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

    // The archive itself is the shared user-library mutation boundary. Admit
    // it before the final baseline recheck so another session cannot pass the
    // same check and install a competing replacement. Keep the live guard in
    // the worker through temporary creation, verification, install, and cleanup.
    let archive_claim = match crate::concurrency::PathClaim::resolve_with_semantics(
        &context.archive_path,
        crate::concurrency::ClaimMode::Write,
        crate::concurrency::ClaimScope::Exact,
        crate::concurrency::PathResolutionSemantics::NamespaceObject,
    ) {
        Ok(claim) => claim,
        Err(error) => {
            app.set_status(format!(
                "archive save admission failed; staged edits were preserved: {error}"
            ));
            return;
        }
    };
    let logical_archive_path = context.archive_path.clone();
    let admitted_archive_path = archive_claim.identity.resolved_io_path.clone();
    let archive_mutation_claim = match acquire_browse_archive_mutation_claim(archive_claim) {
        Ok(guard) => guard,
        Err(error) => {
            app.set_status(format!(
                "archive save busy; staged edits were preserved: {error}"
            ));
            return;
        }
    };
    let staging_dir = context.staging_dir.clone();
    let tool_paths = app.manager.config.tool_paths.clone();
    let tx = tx.clone();
    // Mutation is now admitted. Bump the archive probe epoch immediately
    // so any archive-entry probe that was launched against the pre-edit
    // archive is rejected even if it completes before the final success path
    // clears cache/pending state.
    app.browse.bump_archive_probe_epoch_for(&logical_archive_path);
    if matches!(context.owner, super::app::ArchiveMetadataEditOwner::Browse)
        && !overwrite_external_change
    {
        let mut admitted_context = context.clone();
        admitted_context.archive_path = admitted_archive_path.clone();
        match admitted_context.archive_conflict() {
            Ok(false) => {}
            Ok(true) => {
                app.active_overlay = super::app::ActiveOverlay::Confirmation {
                    message: format!(
                        "Archive was modified externally: {}\n\nY overwrites it with your staged edits. D discards your staged edits. N/Esc keeps the staged edits for later retry. Mouse Cancel opens an explicit discard confirmation.",
                        logical_archive_path.display()
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
                        logical_archive_path.display(),
                        err
                    ),
                    action: super::app::ConfirmAction::ArchiveExternalConflict { context },
                };
                app.set_status("archive save conflict check failed: staged edits kept".to_string());
                return;
            }
        }
    }
    let archive_label = logical_archive_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| logical_archive_path.display().to_string());
    let (control_tx, control_rx) = std::sync::mpsc::channel();
    let mut progress = tui_file_picker::FileTaskProgressState::new(
        tui_file_picker::FileTaskKind::Archive,
        "Repackaging archive",
        super::keybindings::file_picker_theme_from_theme(&app.theme),
    );
    progress.set_scope(tui_file_picker::FileTaskScope {
        source_root: Some(logical_archive_path.clone()),
        source_summary: archive_label.clone(),
        destination: logical_archive_path
            .parent()
            .map(|parent| parent.to_path_buf())
            .or_else(|| Some(logical_archive_path.clone())),
        destination_summary: logical_archive_path
            .parent()
            .map(|parent| parent.display().to_string()),
    });
    let session = super::app::FileTaskProgressSession::new(progress, control_tx);
    let progress_session_id = session.session_id;
    app.install_file_task_progress(session);
    clear_preserved_editor_archive_repackage_context(app, &context);
    app.browse_archive_repackage = Some(context);
    app.browse_archive_repackage_progress_session_id = Some(progress_session_id);
    app.set_status(format!("Saving archive changes: {archive_label}"));

    tokio::spawn(async move {
        let _archive_mutation_claim = archive_mutation_claim;
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
        let archive_for_progress = logical_archive_path.clone();
        let staging_for_progress = staging_dir.clone();
        let result = crate::convert::pipeline::materializer_archive::repackage_archive_with_progress_and_cancel(
            &staging_dir,
            &admitted_archive_path,
            &tool_paths,
            &cancel,
            move |snapshot| {
                let _ = progress_tx.try_send(AppMessage::ArchiveRepackageProgress {
                    archive_path: archive_for_progress.clone(),
                    staging_dir: staging_for_progress.clone(),
                    progress_session_id,
                    snapshot,
                });
            },
        )
        .await;
        control_done.cancel();
        let _ = control_task.await;
        let _ = tx
            .send(AppMessage::ArchiveRepackageResult {
                archive_path: logical_archive_path,
                staging_dir,
                progress_session_id,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveRepackageProgressSurface {
    Active,
    Preempted,
}

fn is_matching_archive_repackage_session(
    session: &super::app::FileTaskProgressSession,
    expected_session_id: u64,
) -> bool {
    session.session_id == expected_session_id
        && session.is_live_task()
        && matches!(&session.progress.kind, tui_file_picker::FileTaskKind::Archive)
}

fn archive_repackage_progress_surface(
    app: &AppState,
    expected_session_id: u64,
) -> Option<ArchiveRepackageProgressSurface> {
    if matches!(
        &app.active_overlay,
        super::app::ActiveOverlay::FileTaskProgress(session)
            if is_matching_archive_repackage_session(session, expected_session_id)
    ) {
        return Some(ArchiveRepackageProgressSurface::Active);
    }
    if matches!(
        app.file_task_preempted_overlay.as_deref(),
        Some(super::app::ActiveOverlay::FileTaskProgress(session))
            if is_matching_archive_repackage_session(session, expected_session_id)
    ) {
        return Some(ArchiveRepackageProgressSurface::Preempted);
    }
    None
}

fn archive_repackage_progress_session(
    app: &AppState,
    expected_session_id: u64,
) -> Option<&super::app::FileTaskProgressSession> {
    if let super::app::ActiveOverlay::FileTaskProgress(session) = &app.active_overlay {
        if is_matching_archive_repackage_session(session, expected_session_id) {
            return Some(session);
        }
    }
    match app.file_task_preempted_overlay.as_deref() {
        Some(super::app::ActiveOverlay::FileTaskProgress(session))
            if is_matching_archive_repackage_session(session, expected_session_id) => Some(session),
        _ => None,
    }
}

fn archive_repackage_progress_session_mut(
    app: &mut AppState,
    expected_session_id: u64,
) -> Option<&mut super::app::FileTaskProgressSession> {
    if let super::app::ActiveOverlay::FileTaskProgress(session) = &mut app.active_overlay {
        if is_matching_archive_repackage_session(session, expected_session_id) {
            return Some(session);
        }
    }
    match app.file_task_preempted_overlay.as_deref_mut() {
        Some(super::app::ActiveOverlay::FileTaskProgress(session))
            if is_matching_archive_repackage_session(session, expected_session_id) => Some(session),
        _ => None,
    }
}

fn replace_archive_repackage_progress_surface(
    app: &mut AppState,
    surface: ArchiveRepackageProgressSurface,
    replacement: super::app::ActiveOverlay,
) {
    match surface {
        ArchiveRepackageProgressSurface::Active => app.active_overlay = replacement,
        ArchiveRepackageProgressSurface::Preempted => {
            app.file_task_preempted_overlay = Some(Box::new(replacement));
        }
    }
}

fn handle_archive_repackage_progress(
    app: &mut AppState,
    archive_path: std::path::PathBuf,
    staging_dir: std::path::PathBuf,
    progress_session_id: u64,
    snapshot: crate::convert::pipeline::materializer_archive::ArchiveRepackageProgressSnapshot,
) {
    let pending_matches = app.browse_archive_repackage_progress_session_id
        == Some(progress_session_id)
        && app
            .browse_archive_repackage
            .as_ref()
            .is_some_and(|context| {
                context.archive_path == archive_path && context.staging_dir == staging_dir
            });
    if !pending_matches {
        return;
    }

    let status = snapshot.status.clone();
    let progress_surface = archive_repackage_progress_surface(app, progress_session_id);
    if let Some(session) = archive_repackage_progress_session_mut(app, progress_session_id) {
        session
            .progress
            .apply_update(archive_repackage_file_task_update(snapshot));
    }
    if progress_surface == Some(ArchiveRepackageProgressSurface::Active) {
        app.set_status(status);
    }
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

fn current_archive_repackage_totals(
    app: &AppState,
    expected_session_id: u64,
) -> tui_file_picker::ProgressTotals {
    archive_repackage_progress_session(app, expected_session_id)
        .map(|session| session.progress.totals)
        .unwrap_or_else(|| tui_file_picker::ProgressTotals {
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
        })
}

fn apply_archive_repackage_terminal_update(
    app: &mut AppState,
    expected_session_id: u64,
    update: tui_file_picker::FileTaskProgressUpdate,
) {
    if let Some(session) = archive_repackage_progress_session_mut(app, expected_session_id) {
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
    progress_session_id: u64,
    result: Result<crate::convert::pipeline::materializer_archive::ArchiveRepackageReport, String>,
    tx: &mpsc::Sender<AppMessage>,
) {
    let pending_matches = app.browse_archive_repackage_progress_session_id
        == Some(progress_session_id)
        && app
            .browse_archive_repackage
            .as_ref()
            .is_some_and(|context| {
                context.archive_path == archive_path && context.staging_dir == staging_dir
            });
    if !pending_matches {
        // A stale result can target the same archive and staging directory as a
        // newer retry. Session authority must be checked before any status,
        // overlay, deferred-action, or staging mutation.
        return;
    }

    let Some(context) = app.browse_archive_repackage.take() else {
        return;
    };
    let editor_owns_staging = context.editor_owns_staging;
    let progress_surface = archive_repackage_progress_surface(app, progress_session_id);
    app.browse_archive_repackage_progress_session_id = None;
    let owns_prompt_slot = progress_surface.is_some();
    let quit_after_repackage = app.quit_after_browse_archive_repackage;
    app.quit_after_browse_archive_repackage = false;

    let mut terminal_totals = current_archive_repackage_totals(app, progress_session_id);

    match result {
        Ok(report) => {
            terminal_totals.items_done = terminal_totals.items_total.unwrap_or(1);
            terminal_totals.completed = terminal_totals.items_done;
            if let Some(total) = terminal_totals.bytes_total {
                terminal_totals.bytes_done = total;
            }
            apply_archive_repackage_terminal_update(
                app,
                progress_session_id,
                tui_file_picker::FileTaskProgressUpdate::Finished {
                    status: "Archive repackaged".to_string(),
                    totals: terminal_totals,
                },
            );
            let path_str = archive_path.display().to_string();
            let focus_before_deferred_tab_close = app.browse.active_tab_id();
            let pending_close_target = app
                .pending_browse_tab_close_after_archive_repackage
                .as_ref()
                .map(|(closing_tab_id, _, _)| *closing_tab_id);
            let pending_close_target_activated = match pending_close_target {
                Some(closing_tab_id) if app.browse.active_tab_id() != closing_tab_id => {
                    app.browse.switch_to_tab_id(closing_tab_id)
                }
                Some(_) | None => true,
            };

            // A deferred tab close may have lost focus while the archive save
            // was running. Rebind cleanup to the tab that actually requested
            // the save before applying the normal successful-exit lifecycle.
            // If activation is unexpectedly unavailable, clear an exact
            // matching staging owner in place so the closed-tab stack can
            // never retain a dangling staging handle.
            if let Some(closing_tab_id) = pending_close_target.filter(|_| !pending_close_target_activated) {
                if let Some(target) = app.browse.tab_mut(closing_tab_id) {
                    let owns_completed_staging = target.archive.as_ref().is_some_and(|arc| {
                        arc.listing.archive_path == archive_path
                            && arc
                                .staging
                                .as_ref()
                                .is_some_and(|staging| staging.staging_dir == staging_dir)
                    });
                    if owns_completed_staging {
                        let _ = target.take_active_archive_staging();
                        target.exit_archive();
                    }
                }
            }

            let browse_holds_same_archive = pending_close_target_activated
                && app
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

            if let Some((closing_tab_id, return_tab_id, archive_restore)) =
                app.pending_browse_tab_close_after_archive_repackage.take()
            {
                let resume_tab_id = return_tab_id.or(
                    (focus_before_deferred_tab_close != closing_tab_id)
                        .then_some(focus_before_deferred_tab_close),
                );
                let closed = match app.browse.tab_index_by_id(closing_tab_id) {
                    Some(index) => app
                        .browse
                        .close_tab_with_archive_restore(index, Some(archive_restore.clone())),
                    None => true,
                };
                if closed {
                    if let Some(resume_tab_id) = resume_tab_id {
                        let _ = app.browse.switch_to_tab_id(resume_tab_id);
                    }
                } else {
                    // Never consume a deferred close merely because focus or
                    // tab-manager state changed underneath the completion.
                    app.pending_browse_tab_close_after_archive_repackage =
                        Some((closing_tab_id, return_tab_id, archive_restore));
                }
            }

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
                progress_session_id,
                tui_file_picker::FileTaskProgressUpdate::Aborted {
                    status: "Archive repackage cancelled; staged edits preserved".to_string(),
                    totals: terminal_totals,
                },
            );
            app.deferred_browse_archive_exit = false;
            app.deferred_browse_archive_screen_switch = None;
            app.quit_after_browse_archive_repackage = false;
            app.pending_browse_tab_close_after_archive_repackage = None;
            app.should_quit = false;
            if editor_owns_staging {
                preserve_editor_owned_archive_repackage_context(app, &context);
                if let Some(surface) = progress_surface {
                    replace_archive_repackage_progress_surface(
                        app,
                        surface,
                        super::app::ActiveOverlay::Confirmation {
                        message: format!(
                            "Archive save was cancelled for {}.\n\nYour staged metadata edits are still preserved in this session and in the recovery database. Y retries the save. D discards the staged edits. N/Esc keeps them for later retry.",
                            archive_path.display()
                        ),
                        action: super::app::ConfirmAction::ArchiveRepackageFailure {
                            context,
                            error: "archive save cancelled".to_string(),
                        },
                        },
                    );
                }
            }
            if owns_prompt_slot || !editor_owns_staging {
                app.set_status(format!(
                    "archive save cancelled for {}; staged edits preserved for retry/discard",
                    archive_path.display()
                ));
            } else {
                app.set_status(format!(
                    "archive save cancelled for {}; staged edits preserved and the current editor or overlay was not replaced",
                    archive_path.display()
                ));
            }
        }
        Err(err) => {
            app.pending_browse_tab_close_after_archive_repackage = None;
            terminal_totals.errors = 1;
            apply_archive_repackage_terminal_update(
                app,
                progress_session_id,
                tui_file_picker::FileTaskProgressUpdate::Failed {
                    status: format!("Archive save failed: {err}"),
                    totals: terminal_totals,
                },
            );
            preserve_editor_owned_archive_repackage_context(app, &context);
            if let Some(surface) = progress_surface {
                let message = format!(
                    "Archive save failed for {}.\n\nY retries the save. D discards your staged edits. N/Esc keeps the staged edits for later retry. Mouse Cancel opens an explicit discard confirmation.\n\n{}",
                    archive_path.display(),
                    err
                );
                replace_archive_repackage_progress_surface(
                    app,
                    surface,
                    super::app::ActiveOverlay::Confirmation {
                        message,
                        action: super::app::ConfirmAction::ArchiveRepackageFailure {
                            context,
                            error: err.clone(),
                        },
                    },
                );
            }
            if quit_after_repackage {
                app.should_quit = false;
                if owns_prompt_slot {
                    app.set_status(format!(
                        "archive save failed for {}; quit cancelled; staged edits preserved: {err}",
                        archive_path.display()
                    ));
                } else {
                    app.set_status(format!(
                        "archive save failed for {}; quit cancelled; staged edits preserved and the current editor or overlay was not replaced: {err}",
                        archive_path.display()
                    ));
                }
            } else if owns_prompt_slot {
                app.set_status(format!(
                    "archive save failed; staged edits preserved for retry/discard: {err}"
                ));
            } else {
                app.set_status(format!(
                    "archive save failed; staged edits preserved and the current editor or overlay was not replaced: {err}"
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
    let was_sentinel_selected = dsd_source_rate_sentinel_selected(app);

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


    let status = probe_notice.map(|notice| format!("Probe warning: {notice}"));
    publish_probe_status_with_sentinel_clamp(app, status, was_sentinel_selected);
}

fn handle_bookmark_detail_loaded_with_retry<F>(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    tab_id: crate::tui::browse::BrowseTabId,
    generation: u64,
    path: std::path::PathBuf,
    result: Result<super::bookmarks::BookmarkDetail, String>,
    retry_selected: F,
) where
    F: FnOnce(&mut AppState, &mpsc::Sender<AppMessage>),
{
    if app.browse.tab_index_by_id(tab_id).is_none() {
        return;
    }
    if app.bookmarks.apply_detail_result(generation, path, result) {
        // A previously selected detail may have been refused because the
        // bounded queue was full. Every current-generation completion releases
        // one slot, so deterministically retry the current selection now.
        retry_selected(app, tx);
    }
}

pub(super) fn handle_bookmark_detail_loaded(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    tab_id: crate::tui::browse::BrowseTabId,
    generation: u64,
    path: std::path::PathBuf,
    result: Result<super::bookmarks::BookmarkDetail, String>,
) {
    handle_bookmark_detail_loaded_with_retry(
        app,
        tx,
        tab_id,
        generation,
        path,
        result,
        super::keybindings::request_selected_bookmark_detail,
    );
}

#[cfg(test)]
mod bookmark_detail_retry_regression_tests {
    use super::*;
    use crate::config::TonepoetConfig;

    fn empty_detail() -> super::super::bookmarks::BookmarkDetail {
        super::super::bookmarks::BookmarkDetail {
            item_count: 0,
            entries: Vec::new(),
            omitted_count: 0,
        }
    }

    #[test]
    fn current_detail_completion_always_invokes_selected_retry() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.bookmarks.open_overlay();
        let generation = app.bookmarks.detail_generation;
        let tab_id = app.browse.active_tab_id();
        let completed_path = std::path::PathBuf::from("/completed");
        let (tx, _rx) = mpsc::channel(1);
        let mut retried = false;

        handle_bookmark_detail_loaded_with_retry(
            &mut app,
            &tx,
            tab_id,
            generation,
            completed_path,
            Ok(empty_detail()),
            |_, _| retried = true,
        );

        assert!(retried, "current completion must refill selected detail work");
    }

    #[test]
    fn stale_bookmark_detail_owner_cannot_mutate_global_presentation() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.bookmarks.open_overlay();
        let generation = app.bookmarks.detail_generation;
        let path = std::path::PathBuf::from("/owned-by-closed-tab");
        assert!(app.bookmarks.mark_detail_queued(path.clone()));
        let stale_tab_id = app.browse.active_tab_id().saturating_add(10_000);
        let (tx, _rx) = mpsc::channel(2);

        handle_message(
            &mut app,
            AppMessage::BookmarkDetailStarted {
                tab_id: stale_tab_id,
                generation,
                path: path.clone(),
            },
            &tx,
        );
        assert!(matches!(
            app.bookmarks.detail_state(&path),
            Some(super::super::bookmarks::BookmarkDetailState::Queued)
        ));

        handle_message(
            &mut app,
            AppMessage::BookmarkDetailLoaded {
                tab_id: stale_tab_id,
                generation,
                path: path.clone(),
                result: Ok(empty_detail()),
            },
            &tx,
        );
        assert!(matches!(
            app.bookmarks.detail_state(&path),
            Some(super::super::bookmarks::BookmarkDetailState::Queued)
        ));

        let owner = app.browse.active_tab_id();
        handle_message(
            &mut app,
            AppMessage::BookmarkDetailStarted {
                tab_id: owner,
                generation,
                path: path.clone(),
            },
            &tx,
        );
        assert!(matches!(
            app.bookmarks.detail_state(&path),
            Some(super::super::bookmarks::BookmarkDetailState::Loading)
        ));
    }

    #[test]
    fn stale_detail_completion_does_not_invoke_retry() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.bookmarks.open_overlay();
        let stale_generation = app.bookmarks.detail_generation.wrapping_sub(1);
        let tab_id = app.browse.active_tab_id();
        let (tx, _rx) = mpsc::channel(1);
        let mut retried = false;

        handle_bookmark_detail_loaded_with_retry(
            &mut app,
            &tx,
            tab_id,
            stale_generation,
            std::path::PathBuf::from("/stale"),
            Ok(empty_detail()),
            |_, _| retried = true,
        );

        assert!(!retried, "stale completion must not refill current work");
    }
}

pub(super) fn handle_message(app: &mut AppState, msg: AppMessage, tx: &mpsc::Sender<AppMessage>) {
    match msg {
        AppMessage::HostClipboardReadComplete {
            generation,
            target,
            result,
        } => {
            super::keybindings::handle_host_clipboard_read_complete(
                app,
                generation,
                target,
                result,
            );
        }
        AppMessage::HostClipboardDiagnosticComplete { report } => {
            app.active_overlay = ActiveOverlay::CuePreview(Box::new(
                super::app::CuePreviewState::new_readonly_help(
                    "Clipboard diagnostics".to_string(),
                    report,
                    "Detected transports, live round-trip, and recent attempts".to_string(),
                ),
            ));
            app.set_status("Clipboard diagnostics complete");
        }
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

            // Capture durable terminal state BEFORE the status is moved into the item.
            let should_persist_terminal = matches!(
                &status,
                crate::convert::ConversionStatus::Completed { .. }
                    | crate::convert::ConversionStatus::CompletedWithActionErrors { .. }
                    | crate::convert::ConversionStatus::Partial { .. }
                    | crate::convert::ConversionStatus::Failed { .. }
                    | crate::convert::ConversionStatus::Cancelled
            );

            // Capture conversion-history metadata BEFORE the status is moved
            // into the item. Cancelled is intentionally excluded — it is not a
            // conversion outcome and was never recorded historically.
            let history_data = match &status {
                crate::convert::ConversionStatus::Completed { output_path, .. } => Some((
                    true,
                    Some(output_path.display().to_string()),
                    None::<String>,
                )),
                crate::convert::ConversionStatus::CompletedWithActionErrors {
                    output_path, errors, ..
                } => Some((
                    true,
                    Some(output_path.display().to_string()),
                    Some(format!("Post-action errors: {}", errors.join("; "))),
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

            // Publish every durable terminal status, including Cancelled.
            if should_persist_terminal {
                app.save_queue();
            }

            // Record the conversion in the durable history log. Write-only
            // today (no reader is wired yet), but this is the data source for a
            // future conversion-history view; this pre-existing writer was
            // erroneously dropped during the queue-persistence redesign and is
            // restored here unchanged.
            if let Some((success, output_path, error_msg)) = history_data {
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
            app.manager.complete_conversion_run();
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
            app.manager.interrupt_active_conversion_run();
            let persistence_error = app.persist_queue_state().err();
            if let Some(error) = persistence_error {
                log::error!(
                    "could not persist interrupted queue state after conversion error: {}",
                    error
                );
                app.set_status(format!(
                    "Error: {}; queue persistence degraded ({error})",
                    message
                ));
            } else {
                app.set_status(format!("Error: {}", message));
            }
        }
        AppMessage::FilesScanned { paths } => {
            let mut options = crate::convert::ConversionOptions::default();
            options.append_lineage_to_comment = app.config.conversion.append_lineage_to_comment;
            options.write_log_file = app.config.conversion.write_log_file;
            options.generate_cue_files = app.config.conversion.generate_cue_files;
            options.cue_generation_mode = app.config.conversion.cue_generation_mode.clone();

            let mut count = 0;
            let mut refused = 0;
            for path in paths {
                if !crate::convert::source_admission::is_direct_queue_source_path(&path) {
                    refused += 1;
                    log::error!(
                        "directory scan produced unsupported source {}; refusing queue admission",
                        path.display()
                    );
                    continue;
                }
                if app.manager.add_file_blocking(path, options.clone()).is_ok() {
                    count += 1;
                }
            }
            app.set_status(if refused == 0 {
                format!("Added {} files", count)
            } else {
                format!("Added {} files; refused {} unsupported paths", count, refused)
            });
            app.save_queue();
        }
        AppMessage::StatusMessage(msg) => {
            app.set_status(msg);
        }
        AppMessage::TagClipboardCopyComplete {
            generation,
            source_paths,
            result,
        } => {
            super::context_menu::handle_tag_clipboard_copy_complete(
                app,
                tx,
                generation,
                source_paths,
                result,
            );
        }
        AppMessage::TagTransferPrepared { generation, result } => {
            super::context_menu::handle_tag_transfer_prepared(
                app,
                tx,
                generation,
                result,
            );
        }
        AppMessage::TagTransferComplete { generation, result } => {
            super::context_menu::handle_tag_transfer_complete(
                app,
                tx,
                generation,
                result,
            );
        }
        AppMessage::TagTransferProgress {
            generation,
            completed,
            total,
            path,
        } => {
            super::context_menu::handle_tag_transfer_progress(
                app,
                generation,
                completed,
                total,
                &path,
            );
        }
        AppMessage::MetadataTagBlocksFilePrepared {
            request_id,
            editor_session,
            editor_fingerprint,
            path,
            result,
        } => {
            let Some(mut taken) = take_metadata_editor_with_restore_slot(app) else {
                log::debug!(
                    "ignored editor tag-block file preparation {request_id}: editor is closed"
                );
                return;
            };
            if !taken.state.take_tag_transfer_preparation(request_id) {
                restore_taken_metadata_editor(app, taken);
                log::debug!(
                    "ignored superseded or duplicate editor tag-block file preparation {request_id}"
                );
                return;
            }
            if metadata_editor_session_guard(&taken.state) != editor_session
                || super::tag_interchange::metadata_editor_transfer_fingerprint(&taken.state)
                    != editor_fingerprint
            {
                restore_taken_metadata_editor(app, taken);
                app.set_status(
                    "metadata editor: tag-block file result became stale; choose the file again",
                );
                return;
            }

            match result {
                Ok(blocks) => match super::tag_interchange::apply_field_blocks_to_editor(
                    &mut taken.state,
                    &blocks,
                ) {
                    Ok(report) => app.set_status(
                        report.success_status(taken.state.active_surface().paths.len()),
                    ),
                    Err(error) => app.set_status(format!("tag blocks: {error}")),
                },
                Err(error) => app.set_status(format!(
                    "tag blocks from '{}': {error}",
                    path.display()
                )),
            }
            restore_taken_metadata_editor(app, taken);
        }
        AppMessage::MetadataTagTransferTargetsPrepared {
            request_id,
            editor_session,
            editor_fingerprint,
            scope,
            field_key,
            source_entries,
            source_dimension,
            result,
        } => {
            let Some(mut taken) = take_metadata_editor_with_restore_slot(app) else {
                log::debug!(
                    "ignored editor tag-transfer target preparation {request_id}: editor is closed"
                );
                return;
            };
            if !taken.state.take_tag_transfer_preparation(request_id) {
                restore_taken_metadata_editor(app, taken);
                log::debug!(
                    "ignored superseded or duplicate editor tag-transfer target preparation {request_id}"
                );
                return;
            }
            if metadata_editor_session_guard(&taken.state) != editor_session
                || super::tag_interchange::metadata_editor_transfer_fingerprint(&taken.state)
                    != editor_fingerprint
            {
                restore_taken_metadata_editor(app, taken);
                app.set_status(
                    "metadata editor: tag-transfer target result became stale; choose the target again",
                );
                return;
            }

            match result {
                Ok(target) => {
                    if let Err(error) = super::tag_interchange::preview_tag_transfer(
                        &source_entries,
                        source_dimension,
                        &target,
                        scope,
                    ) {
                        restore_taken_metadata_editor(app, taken);
                        app.set_status(error);
                        return;
                    }
                    let edit_count = if field_key.is_some() {
                        source_entries
                            .iter()
                            .filter(|entry| {
                                entry.value != entry.original
                                    || entry.per_file_values != entry.per_file_originals
                                    || entry.mb_proposed_value.is_some()
                                    || entry.mb_proposed_per_file.is_some()
                            })
                            .count()
                    } else {
                        super::tag_interchange::metadata_editor_unsaved_edit_count(&taken.state)
                    };
                    if edit_count > 0 {
                        let target_label = target.label();
                        let target_count = target.count();
                        let target_unit = if target_label == "files" {
                            if target_count == 1 { "file" } else { "files" }
                        } else {
                            if target_count == 1 { "track" } else { "tracks" }
                        };
                        app.pending_metadata_editor = Some(taken.state);
                        app.active_overlay = ActiveOverlay::Confirmation {
                            message: if let Some(field_key) = field_key.as_deref() {
                                format!(
                                    "Transfer unsaved {field_key} edits to {target_label} ({target_count} {target_unit})?"
                                )
                            } else {
                                format!(
                                    "Transfer {} unsaved edit{} to {} ({} {})?",
                                    edit_count,
                                    if edit_count == 1 { "" } else { "s" },
                                    target_label,
                                    target_count,
                                    target_unit,
                                )
                            },
                            action: super::app::ConfirmAction::MetadataTransferUnsaved {
                                source_entries,
                                source_dimension,
                                target,
                                scope,
                                edit_count,
                            },
                        };
                    } else {
                        let target_label = target.label();
                        let target_count = target.count();
                        super::context_menu::start_tag_transfer_from_editor_snapshot(
                            app,
                            source_entries,
                            source_dimension,
                            target,
                            scope,
                            tx,
                        );
                        restore_taken_metadata_editor(app, taken);
                        app.set_status(format!(
                            "Transfer tags: {} {} target position{}; writing metadata...",
                            target_label,
                            target_count,
                            if target_count == 1 { "" } else { "s" }
                        ));
                    }
                }
                Err(error) => {
                    restore_taken_metadata_editor(app, taken);
                    app.set_status(error);
                }
            }
        }
        AppMessage::MetadataTagTransferSourcePrepared {
            request_id,
            editor_session,
            editor_fingerprint,
            scope,
            field_key,
            result,
        } => {
            let Some(mut taken) = take_metadata_editor_with_restore_slot(app) else {
                log::debug!(
                    "ignored editor tag-transfer source preparation {request_id}: editor is closed"
                );
                return;
            };
            if !taken.state.take_tag_transfer_preparation(request_id) {
                restore_taken_metadata_editor(app, taken);
                log::debug!(
                    "ignored superseded or duplicate editor tag-transfer source preparation {request_id}"
                );
                return;
            }
            if metadata_editor_session_guard(&taken.state) != editor_session
                || super::tag_interchange::metadata_editor_transfer_fingerprint(&taken.state)
                    != editor_fingerprint
            {
                restore_taken_metadata_editor(app, taken);
                app.set_status(
                    "metadata editor: tag-transfer source result became stale; choose the source again",
                );
                return;
            }

            match result {
                Ok((mut entries, source_dimension, source_carrier)) => {
                    if let Some(field_key) = field_key.as_deref() {
                        entries.retain(|entry| {
                            super::keybindings::metadata_field_keys_match(
                                &entry.display_key,
                                field_key,
                            )
                        });
                        if entries.len() != 1 {
                            restore_taken_metadata_editor(app, taken);
                            app.set_status(format!(
                                "metadata editor: source does not contain exactly one matching {field_key} field"
                            ));
                            return;
                        }
                    }
                    match super::tag_interchange::apply_transfer_entries_to_editor_with_dimension(
                        &mut taken.state,
                        &entries,
                        source_dimension,
                        scope,
                    ) {
                        Ok(report) => {
                            for warning in &report.cardinality_warnings {
                                log::warn!("editor tag transfer: {warning}");
                            }
                            let target_count =
                                super::tag_interchange::metadata_editor_transfer_dimension(
                                    &taken.state,
                                )
                                .count();
                            let source_unit = if matches!(
                                source_dimension,
                                super::tag_interchange::TransferDimension::Tracks(_)
                            ) {
                                "tracks"
                            } else {
                                "files"
                            };
                            app.set_status(format!(
                                "read {} {} from {}; {}",
                                source_dimension.count(),
                                source_unit,
                                source_carrier,
                                report.success_status(target_count),
                            ));
                        }
                        Err(error) => app.set_status(error),
                    }
                }
                Err(error) => app.set_status(error),
            }
            restore_taken_metadata_editor(app, taken);
        }
        AppMessage::ActionsRunPreparationProgress {
            preparation_id,
            detail,
        } => {
            let mut accepted = false;
            if let super::app::ActiveOverlay::ActionsRun(state) = &mut app.active_overlay {
                if state.preparation_id == preparation_id
                    && matches!(
                        state.status,
                        super::conversion_actions_ui::ActionsRunStatus::Preparing
                            | super::conversion_actions_ui::ActionsRunStatus::Cancelling
                    )
                {
                    state.preview_lines.clear();
                    state.preview_lines.push(detail.clone());
                    accepted = true;
                }
            }
            if accepted {
                app.set_status(detail);
            }
        }
        AppMessage::ActionsRunPrepared {
            preparation_id,
            result,
        } => {
            let matches_active = matches!(
                &app.active_overlay,
                super::app::ActiveOverlay::ActionsRun(state)
                    if state.preparation_id == preparation_id
                        && matches!(
                            state.status,
                            super::conversion_actions_ui::ActionsRunStatus::Preparing
                                | super::conversion_actions_ui::ActionsRunStatus::Cancelling
                        )
            );
            if matches_active {
                match result {
                    Ok(state) => {
                        app.active_overlay =
                            super::app::ActiveOverlay::ActionsRun(Box::new(state));
                        app.set_status("Action dry run ready; press Enter to apply");
                    }
                    Err(error) => {
                        if let super::app::ActiveOverlay::ActionsRun(state) =
                            &mut app.active_overlay
                        {
                            state.status =
                                super::conversion_actions_ui::ActionsRunStatus::Failed;
                            state.error = Some(error.clone());
                        }
                        app.set_status(error);
                    }
                }
            } else if let Ok(state) = result {
                // The user cancelled or replaced the preparation overlay after
                // the worker durably prepared a preview. Retire that unreviewed
                // authority rather than leaving it to block publication.
                if let Err(error) =
                    super::conversion_actions_ui::discard_actions_run_preview(&state)
                {
                    app.set_status(format!(
                        "Cancelled action preview could not be retired safely: {error}"
                    ));
                }
            }
        }
        AppMessage::ActionsRunComplete { invocation_id, result } => {
            let mut status_message = None;
            if let super::app::ActiveOverlay::ActionsRun(state) = &mut app.active_overlay {
                if state.invocation_id() == Some(invocation_id.as_str()) {
                    super::conversion_actions_ui::complete_actions_run(state, result);
                    status_message = Some(match state.status {
                        super::conversion_actions_ui::ActionsRunStatus::Complete => {
                            "Action pipeline completed".to_string()
                        }
                        super::conversion_actions_ui::ActionsRunStatus::Stale => {
                            "Action preview is stale; nothing was executed. Refresh before applying"
                                .to_string()
                        }
                        _ => "Action pipeline finished with errors; review the report".to_string(),
                    });
                }
            }
            if let Some(message) = status_message {
                app.set_status(message);
                if app.current_screen == super::app::AppScreen::Browse {
                    app.browse.refresh_with_search(Some(tx));
                    app.browse.probe_current_with_db(tx, Some(&app.db));
                }
            }
        }
        AppMessage::Redraw => {} // Just triggers a redraw via the loop
        AppMessage::BookmarkTargetsLoaded { generation, statuses } => {
            app.bookmarks.apply_target_statuses(generation, statuses);
            // Refill the bounded status queue as completions free capacity. This
            // preserves a hard worker/queue bound without leaving bookmarks
            // permanently unprobed when the collection exceeds that bound.
            super::keybindings::request_bookmark_target_statuses(app, tx);
            super::keybindings::request_selected_bookmark_detail(app, tx);
        }
        AppMessage::BookmarkActivationResolved {
            tab_id,
            generation,
            request_id,
            path,
            result,
        } => {
            super::keybindings::handle_bookmark_activation_result(
                app, tx, tab_id, generation, request_id, path, result,
            );
        }
        AppMessage::BookmarkDetailStarted {
            tab_id,
            generation,
            path,
        } => {
            if app.browse.tab_index_by_id(tab_id).is_some() {
                app.bookmarks.apply_detail_started(generation, &path);
            }
        }
        AppMessage::BookmarkDetailLoaded {
            tab_id,
            generation,
            path,
            result,
        } => {
            handle_bookmark_detail_loaded(app, tx, tab_id, generation, path, result);
        }
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
        AppMessage::BrowseCueInspectionComplete {
            generation,
            browse_scan_generation,
            tab_id,
            origin_dir,
            folder,
            result,
        } => {
            super::keybindings::handle_browse_cue_inspection_complete(
                app,
                generation,
                browse_scan_generation,
                tab_id,
                origin_dir,
                folder,
                result,
            );
        }
        AppMessage::BrowseCueRepairComplete {
            generation,
            browse_scan_generation,
            tab_id,
            origin_dir,
            folder,
            cue_path,
            result,
        } => {
            super::keybindings::handle_browse_cue_repair_complete(
                app,
                generation,
                browse_scan_generation,
                tab_id,
                origin_dir,
                folder,
                cue_path,
                result,
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
            progress_session_id,
            snapshot,
        } => {
            handle_archive_repackage_progress(
                app,
                archive_path,
                staging_dir,
                progress_session_id,
                snapshot,
            );
        }
        AppMessage::ArchiveRepackageResult {
            archive_path,
            staging_dir,
            progress_session_id,
            result,
        } => {
            handle_archive_repackage_result(
                app,
                archive_path,
                staging_dir,
                progress_session_id,
                result,
                tx,
            );
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
        AppMessage::ProbeCacheWarmComplete { tab_id, generation, path, rows } => {
            // Do not merge here. Queue rows on the owning tab and let the
            // post-drain flush merge a bounded slice, generation/path-checked
            // again, so warm-cache bursts cannot bleed across tabs.
            let queued = app
                .browse
                .tab_mut(tab_id)
                .map(|browse| browse.enqueue_probe_cache_warm_rows(generation, path.clone(), rows))
                .unwrap_or(0);
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
            super::context_menu::refresh_open_browse_entry_menu(app);
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
                    app.browse
                        .continue_requested_folder_cue_availability_probe(&path, tx);
                    super::context_menu::refresh_open_browse_entry_menu(app);
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
        AppMessage::AnalysisComplete {
            operation_id,
            result,
        } => {
            let kind = CompletionOperationKind::Analysis;
            if !completion_operation_is_current(app, kind, operation_id) {
                return;
            }
            app.analysis_pending = app.analysis_pending.saturating_sub(1);
            let finished = app.analysis_pending == 0;
            if finished {
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

                    // Refresh only the probe row whose launch-independent file
                    // identity still matches the completed path.
                    if result.hdcd_detected == Some(true) {
                        if let Ok(meta) = std::fs::metadata(&result.path) {
                            let identity =
                                crate::tui::browse::ProbeCacheIdentity::from_metadata(&meta);
                            app.browse.update_valid_probe_for_identity(
                                &result.path,
                                identity,
                                |cached| {
                                    cached.metadata.hdcd_detail = result.hdcd_detail.clone();
                                },
                            );
                        } else {
                            app.browse.remove_probe_cache_entry(&result.path);
                        }
                    }

                    // Enrich only the exact editor captured at dispatch. A
                    // completion may never mutate whichever editor happens to
                    // occupy the slot later.
                    if let Some(guard) =
                        completion_operation_editor_session(app, kind, operation_id)
                    {
                        if let ActiveOverlay::MetadataEditor(state) = &mut app.active_overlay {
                            if metadata_editor_matches_session_guard(state, guard) {
                                state.apply_analysis_result(&result);
                            }
                        }
                        if let Some(state) = app.pending_metadata_editor.as_mut() {
                            if metadata_editor_matches_session_guard(state, guard) {
                                state.apply_analysis_result(&result);
                            }
                        }
                    }

                    app.analysis_results.push(*result);
                    if finished {
                        let mut result_paths: Vec<std::path::PathBuf> = app
                            .analysis_results
                            .iter()
                            .map(|result| result.path.clone())
                            .collect();
                        crate::tui::probe::sort_paths_by_track(&mut result_paths);
                        app.analysis_results.sort_by(|a, b| {
                            let ai = result_paths
                                .iter()
                                .position(|path| *path == a.path)
                                .unwrap_or(usize::MAX);
                            let bi = result_paths
                                .iter()
                                .position(|path| *path == b.path)
                                .unwrap_or(usize::MAX);
                            ai.cmp(&bi)
                        });

                        let status = app
                            .analysis_results
                            .last()
                            .map(|last| {
                                let name = last
                                    .path
                                    .file_name()
                                    .map(|name| name.to_string_lossy().to_string())
                                    .unwrap_or_default();
                                format!(
                                    "Analyzed: {} — DR{} ({})",
                                    name,
                                    last.dr_value,
                                    super::analyze::dr_label(last.dr_value),
                                )
                            })
                            .unwrap_or_else(|| "Analysis completed without usable results".to_string());
                        let may_publish =
                            completion_operation_has_overlay_authority(app, kind, operation_id);
                        retire_completion_operation(app, kind, operation_id);
                        if may_publish && !app.analysis_results.is_empty() {
                            app.active_overlay = ActiveOverlay::Analysis { scroll: 0 };
                            app.set_status(status);
                        } else if app.analysis_results.is_empty() {
                            app.set_status(status);
                        } else {
                            app.set_status(format!(
                                "{}; current editor or overlay preserved",
                                status
                            ));
                        }
                    }
                }
                Err(error) => {
                    let status = format!("Analysis failed: {}", error);
                    if finished {
                        let may_publish = !app.analysis_results.is_empty()
                            && completion_operation_has_overlay_authority(
                                app,
                                kind,
                                operation_id,
                            );
                        retire_completion_operation(app, kind, operation_id);
                        if may_publish {
                            app.active_overlay = ActiveOverlay::Analysis { scroll: 0 };
                            app.set_status(status);
                        } else if app.analysis_results.is_empty() {
                            app.set_status(status);
                        } else {
                            app.set_status(format!(
                                "{}; current editor or overlay preserved",
                                status
                            ));
                        }
                    } else {
                        app.set_status(status);
                    }
                }
            }
        }
        AppMessage::PreemphasisComplete {
            operation_id,
            result,
        } => {
            let kind = CompletionOperationKind::Preemphasis;
            let Some(finished) = complete_counted_completion_operation(
                app,
                kind,
                operation_id,
            ) else {
                return;
            };
            let result = crate::tui::preemphasis::metadata_editor_safe_result(&result);

            // Enrich only the exact editor captured when this operation began.
            // A late completion may never mutate whichever editor later happens
            // to occupy the active or parked slot.
            if let Some(guard) =
                completion_operation_editor_session(app, kind, operation_id)
            {
                if let ActiveOverlay::MetadataEditor(state) = &mut app.active_overlay {
                    if metadata_editor_matches_session_guard(state, guard) {
                        state.apply_preemphasis_result(&result);
                    }
                }
                if let Some(state) = app.pending_metadata_editor.as_mut() {
                    if metadata_editor_matches_session_guard(state, guard) {
                        state.apply_preemphasis_result(&result);
                    }
                }
            }

            app.preemph_results.push(result);
            if finished {
                app.preemph_results.sort_by(|a, b| a.path.cmp(&b.path));
                let detected = app
                    .preemph_results
                    .iter()
                    .filter(|result| {
                        result.confidence
                            == crate::tui::preemphasis::PreemphasisConfidence::Detected
                    })
                    .count();
                let candidates = app
                    .preemph_results
                    .iter()
                    .filter(|result| {
                        result.confidence
                            == crate::tui::preemphasis::PreemphasisConfidence::StrongCandidate
                    })
                    .count();
                let total = app.preemph_results.len();
                let status = if detected > 0 || candidates > 0 {
                    format!(
                        "Pre-emphasis: {} PRE flag, {} catalog candidate out of {} file(s)",
                        detected, candidates, total,
                    )
                } else {
                    format!("Pre-emphasis: not detected in {} file(s)", total)
                };
                let may_publish = !app.preemph_results.is_empty()
                    && completion_operation_has_overlay_authority(app, kind, operation_id);
                retire_completion_operation(app, kind, operation_id);
                if may_publish {
                    app.active_overlay = ActiveOverlay::Preemphasis { scroll: 0 };
                    app.set_status(status);
                } else if app.preemph_results.is_empty() {
                    app.set_status(status);
                } else {
                    app.set_status(format!(
                        "{}; current editor or overlay preserved",
                        status
                    ));
                }
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
        AppMessage::CompareComplete {
            operation_id,
            result,
        } => {
            let kind = CompletionOperationKind::Compare;
            let Some(finished) = complete_counted_completion_operation(
                app,
                kind,
                operation_id,
            ) else {
                return;
            };
            app.compare_results.push(result);
            if finished {
                let identical = app.compare_results.iter().filter(|result| result.identical).count();
                let differ = app.compare_results.len().saturating_sub(identical);
                let status = if differ == 0 {
                    format!("Compared {} pair(s): all bit-identical", identical)
                } else {
                    format!(
                        "Compared {} pair(s): {} identical, {} differ",
                        identical + differ,
                        identical,
                        differ,
                    )
                };
                let may_publish = !app.compare_results.is_empty()
                    && completion_operation_has_overlay_authority(app, kind, operation_id);
                retire_completion_operation(app, kind, operation_id);
                if may_publish {
                    app.active_overlay = ActiveOverlay::BitCompare { scroll: 0 };
                    app.set_status(status);
                } else if app.compare_results.is_empty() {
                    app.set_status(status);
                } else {
                    app.set_status(format!(
                        "{}; current editor or overlay preserved",
                        status
                    ));
                }
                if !app.config.ui.compare_keep_reference {
                    app.compare_reference.clear();
                }
            }
        }
        AppMessage::VerifyComplete {
            operation_id,
            result,
        } => {
            let kind = CompletionOperationKind::Verify;
            let Some(finished) = complete_counted_completion_operation(
                app,
                kind,
                operation_id,
            ) else {
                return;
            };
            app.verify_results.push(result);
            if finished {
                let mut result_paths: Vec<std::path::PathBuf> =
                    app.verify_results.iter().map(|result| result.path.clone()).collect();
                crate::tui::probe::sort_paths_by_track(&mut result_paths);
                app.verify_results.sort_by(|a, b| {
                    let ai = result_paths
                        .iter()
                        .position(|path| *path == a.path)
                        .unwrap_or(usize::MAX);
                    let bi = result_paths
                        .iter()
                        .position(|path| *path == b.path)
                        .unwrap_or(usize::MAX);
                    ai.cmp(&bi)
                });
                let passed = app.verify_results.iter().filter(|result| result.passed).count();
                let failed = app.verify_results.len().saturating_sub(passed);
                let status = if failed == 0 {
                    format!("Verified {} file(s): all passed", passed)
                } else {
                    format!(
                        "Verified {} file(s): {} passed, {} failed",
                        passed + failed,
                        passed,
                        failed,
                    )
                };
                let may_publish = !app.verify_results.is_empty()
                    && completion_operation_has_overlay_authority(app, kind, operation_id);
                retire_completion_operation(app, kind, operation_id);
                if may_publish {
                    app.active_overlay = ActiveOverlay::Verify { scroll: 0 };
                    app.set_status(status);
                } else if app.verify_results.is_empty() {
                    app.set_status(status);
                } else {
                    app.set_status(format!(
                        "{}; current editor or overlay preserved",
                        status
                    ));
                }
            }
        }
        AppMessage::PathValidationComplete {
            tab_id,
            generation,
            origin_dir,
            input,
            result,
        } => {
            let active = app.browse.active_tab_id() == tab_id;
            let mut status = None;
            let Some(browse) = app.browse.tab_mut(tab_id) else {
                log::debug!("discarded path validation for closed Browse tab {}", tab_id);
                return;
            };
            if !browse.is_current_path_validation(generation, &origin_dir) {
                return;
            }
            match result {
                Ok(path) => {
                    let display = path.display().to_string();
                    browse.navigate_to(path);
                    if active { status = Some(format!("cd: {}", display)); }
                }
                Err(e) => {
                    browse.error = Some(format!(":cd {}: {}", input, e));
                    if active { status = browse.error.clone(); }
                }
            }
            if let Some(status) = status { app.set_status(status); }
        },
        AppMessage::DirScanBatch {
            tab_id,
            generation,
            path,
            dirs,
            files,
            discovered,
        } => {
            let Some(browse) = app.browse.tab_mut(tab_id) else {
                log::debug!("discarded directory batch for closed Browse tab {}", tab_id);
                return;
            };
            if !browse.apply_dir_scan_batch_if_current(generation, &path, dirs, files, discovered) {
                log::debug!(
                    "discarded stale directory scan batch for tab {} generation {} path {}",
                    tab_id,
                    generation,
                    path.display()
                );
                return;
            }
        },
        AppMessage::BrowseTreeChildrenComplete {
            tab_id,
            generation,
            path,
            child_depth,
            children,
            error,
        } => {
            let Some(browse) = app.browse.tab_mut(tab_id) else { return; };
            if browse.apply_tree_scan_complete(generation, &path, child_depth, children) {
                if let Some(error) = error {
                    log::debug!("Browse tree expansion {}: {}", path.display(), error);
                }
            }
        },
        AppMessage::DirScanComplete {
            tab_id,
            generation,
            path,
            parent_entry,
            dirs,
            files,
            classification_updates,
            error,
        } => {
            let active = app.browse.active_tab_id() == tab_id;
            let is_current = app
                .browse
                .tab_mut(tab_id)
                .is_some_and(|browse| browse.is_current_dir_scan(generation, &path));
            if !is_current {
                log::debug!(
                    "discarded stale directory scan completion for tab {} generation {} path {}",
                    tab_id,
                    generation,
                    path.display()
                );
                if let Some(browse) = app.browse.tab_mut(tab_id) {
                    browse.clear_pending_inline_rename_after_scan_generation(generation);
                }
                return;
            }

            if let Some(err) = error {
                let mut status = None;
                if let Some(browse) = app.browse.tab_mut(tab_id) {
                    let partial_count = browse.scan_discovered_count;
                    browse.finish_dir_scan_if_current(generation, &path);
                    browse.clear_pending_inline_rename_after_scan_generation(generation);
                    if partial_count > 0 {
                        let message = format!(
                            "Reading {} stopped after {} entries: {}",
                            path.display(), partial_count, err
                        );
                        browse.error = Some(message.clone());
                        if active { status = Some(message); }
                    } else {
                        browse.error = Some(err.clone());
                        if active { status = Some(err); }
                    }
                }
                if let Some(status) = status { app.set_status(status); }
                return;
            }

            // Classification caches are absolute-path keyed and session-global.
            // For a background tab, merge a clone into the canonical active
            // cache as well as the tab-local snapshot used to build this view.
            if !active {
                app.browse.apply_classification_cache_updates(classification_updates.clone());
            }

            let resume_inline_rename_target = {
                let browse = app.browse.tab_mut(tab_id).expect("validated Browse tab exists");
                browse.finish_dir_scan_if_current(generation, &path);
                browse.apply_classification_cache_updates(classification_updates);
                browse.publish_scanned_entries(parent_entry, dirs, files);
                browse.reapply_after_directory_scan_complete(Some(tx));
                browse.restore_cursor_after_refresh();
                if active {
                    browse.take_inline_rename_after_scan_target(generation, &path)
                } else {
                    // Background tabs cannot own a global inline editor. Keep
                    // the captured target for a future active refresh instead
                    // of opening UI on the wrong tab.
                    None
                }
            };

            if active {
                app.browse.spawn_probe_cache_warm_from_db(generation, path.clone(), tx.clone());
                let resumed_inline_rename = if let Some(target_path) = resume_inline_rename_target {
                    super::keybindings::begin_browse_inline_rename(app, target_path);
                    true
                } else {
                    false
                };
                if !resumed_inline_rename {
                    app.browse.probe_current_with_db(tx, Some(&app.db));
                    super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
                }
            }
        }
        AppMessage::MetadataWriteProgress {
            operation_id,
            path,
            detail,
        } => {
            if app.inline_metadata_write_is_current(operation_id, &path) {
                app.set_status(detail);
            }
        }
        AppMessage::MetadataWriteComplete {
            operation_id,
            path,
            field,
            value,
            result,
        } => {
            if !app.complete_inline_metadata_write(operation_id, &path) {
                return;
            }
            // The blocking writer owns the complete journal/backup lifecycle.
            // The reducer only publishes the result and invalidates caches.
            let path_str = path.display().to_string();
            let uses_native_journal =
                crate::tui::probe::uses_native_flac_metadata_journal(&path)
                    || crate::dsf_tags::is_dsf(&path)
                    || matches!(
                        crate::metadata_persistence::metadata_persistence_route_for_path(&path),
                        crate::metadata_persistence::MetadataPersistenceRoute::WavPackApeDispatch
                    );
            match result {
                Ok(report) => {
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
                    let base_status = if let Some(err) = staged_tracking_error {
                        format!(
                            "{}: {} updated, but archive recovery tracking failed: {}",
                            path.file_name().unwrap_or_default().to_string_lossy(),
                            field.label(),
                            err,
                        )
                    } else {
                        format!(
                            "{}: {} updated",
                            path.file_name().unwrap_or_default().to_string_lossy(),
                            field.label(),
                        )
                    };
                    if report.durability_warnings.is_empty() {
                        app.set_status(base_status);
                    } else {
                        app.set_status(format!(
                            "{base_status}, with durability warning: {}",
                            report.durability_warnings.join("; ")
                        ));
                    }
                }
                Err(error) => {
                    if uses_native_journal {
                        app.set_status(format!("write failed: {error}"));
                    } else {
                        // The database-backed transaction returns a complete,
                        // user-facing outcome including whether rollback
                        // succeeded or recovery authority remains armed.
                        app.set_status(error);
                    }
                }
            }
        }
        AppMessage::ArchiveListingProgress {
            tab_id,
            id,
            archive_path,
            message,
        } => {
            if app.archive_listing_pending_for(tab_id, id, &archive_path)
                && app.browse.active_tab_id() == tab_id
            {
                app.set_status(message);
            }
        }
        AppMessage::ArchiveListingComplete {
            tab_id,
            id,
            archive_path,
            cache_key,
            result,
            password,
        } => {
            if !app.complete_archive_listing(tab_id, id, &archive_path) {
                return;
            }
            let active = app.browse.active_tab_id() == tab_id;
            let restore_inner_path = app
                .pending_browse_archive_tab_restores
                .get(&tab_id)
                .filter(|restore| restore.archive_path == archive_path)
                .map(|restore| restore.inner_path.clone());
            match *result {
                Ok(listing) => {
                    let count = listing.entries.len();
                    if let Some(key) = cache_key {
                        let _ = app.insert_archive_listing_cache(key, listing.clone());
                    }

                    let resumed_recovery = app
                        .pending_archive_recovery_resume
                        .as_ref()
                        .is_some_and(|session| session.archive_path == archive_path);
                    let recovery_session = resumed_recovery
                        .then(|| app.pending_archive_recovery_resume.take())
                        .flatten();
                    if app.browse.tab_index_by_id(tab_id).is_none() {
                        // A closed tab no longer owns UI state. The listing is
                        // still safe in the shared cache; never apply it to the
                        // newly focused tab.
                        app.pending_browse_archive_tab_restores.remove(&tab_id);
                        if let Some(session) = recovery_session {
                            app.pending_archive_recovery_resume = Some(session);
                        }
                        return;
                    }
                    {
                        let browse = app
                            .browse
                            .tab_mut(tab_id)
                            .expect("validated live Browse tab");
                        if browse
                            .archive
                            .as_ref()
                            .is_some_and(|arc| arc.listing.archive_path == archive_path)
                        {
                            browse.replace_active_archive_listing_with_search(listing, password, Some(tx));
                        } else {
                            browse.enter_archive(listing, password);
                        }
                        if let Some(session) = recovery_session {
                            if let Some(arc) = browse.archive.as_mut() {
                                if arc.listing.archive_path == archive_path {
                                    arc.staging = Some(session);
                                }
                            }
                            browse.refresh_archive_view_with_search(Some(tx));
                        }
                        if let Some(inner_path) = restore_inner_path.as_deref() {
                            if !inner_path.is_empty() {
                                browse.enter_archive_dir(inner_path);
                            }
                        }
                    }
                    if restore_inner_path.is_some() {
                        app.pending_browse_archive_tab_restores.remove(&tab_id);
                    }

                    if active {
                        app.browse.probe_current_with_db(tx, Some(&app.db));
                    }
                    if resumed_recovery {
                        if app.pending_archive_recovery_resume_conflicted {
                            if active {
                                app.set_status(format!(
                                    "Recovered staged archive edits for {}; archive changed externally, save will require overwrite/discard choice",
                                    archive_path
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_default(),
                                ));
                            }
                            app.pending_archive_recovery_resume_conflicted = false;
                        } else if active {
                            app.set_status(format!(
                                "Recovered staged archive edits for {} ({} entries)",
                                archive_path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_default(),
                                count,
                            ));
                        }
                    } else if active {
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
                                    let Some(browse) = app.browse.tab_mut(tab_id) else {
                                        app.pending_archive_recovery_resume = Some(session);
                                        return;
                                    };
                                    browse.enter_archive(listing, password);
                                    if let Some(arc) = browse.archive.as_mut() {
                                        arc.staging = Some(session);
                                    }
                                    browse.refresh_archive_view_with_search(Some(tx));
                                    let conflict_note = if app.pending_archive_recovery_resume_conflicted {
                                        "; original archive needs overwrite/discard review before save"
                                    } else {
                                        ""
                                    };
                                    app.pending_archive_recovery_resume_conflicted = false;
                                    if active {
                                        app.set_status(format!(
                                            "Recovered staged archive edits from staging{}: {}",
                                            conflict_note,
                                            archive_path.display()
                                        ));
                                        app.browse.probe_current_with_db(tx, Some(&app.db));
                                    }
                                    return;
                                }
                                Err(recovery_err) => {
                                    app.pending_archive_recovery_resume_conflicted = false;
                                    if active {
                                        app.set_status(format!(
                                            "Archive recovery failed: could not list archive ({e}) or staged tree ({recovery_err}); staged edits remain on disk"
                                        ));
                                    } else if let Some(browse) = app.browse.tab_mut(tab_id) {
                                        browse.error = Some(format!(
                                            "Archive recovery failed: could not list archive ({e}) or staged tree ({recovery_err}); staged edits remain on disk"
                                        ));
                                    }
                                    return;
                                }
                            }
                        }
                        app.pending_archive_recovery_resume_conflicted = false;
                    }

                    let error_message = format!("Archive error: {}", e);
                    if app.browse.tab_index_by_id(tab_id).is_none() {
                        app.pending_browse_archive_tab_restores.remove(&tab_id);
                        return;
                    }
                    app.browse
                        .tab_mut(tab_id)
                        .expect("validated live Browse tab")
                        .error = Some(error_message.clone());
                    if super::app::looks_like_archive_password_error(&e) {
                        // Password UI is global, so only the owning *focused*
                        // tab may install it. Background failure remains bound
                        // to that tab and is surfaced without clobbering the
                        // user's current overlay.
                        if active && matches!(app.active_overlay, ActiveOverlay::None) {
                            app.active_overlay = ActiveOverlay::TextEdit {
                                input: TextInputState::empty(),
                                target: TextEditTarget::ArchivePassword(archive_path.clone()),
                                label: "archive password".to_string(),
                            };
                            app.set_status(format!("Archive error: {}; enter password", e));
                        } else if active {
                            app.set_status(format!(
                                "Archive error: {}; close the current overlay and use :password to retry",
                                e
                            ));
                        } else {
                            app.set_status(format!(
                                "Background Browse tab needs an archive password: {}",
                                archive_path.display()
                            ));
                        }
                    } else if active {
                        app.set_status(error_message);
                    } else {
                        app.set_status(format!(
                            "Background Browse tab archive error: {}",
                            archive_path.display()
                        ));
                    }
                }
            }
        },
        AppMessage::SearchComplete {
            tab_id,
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
            let active = app.browse.active_tab_id() == tab_id;
            let mut truncated_status = None;
            let Some(browse) = app.browse.tab_mut(tab_id) else {
                log::debug!("discarded search completion for closed Browse tab {}", tab_id);
                return;
            };
            if !browse.search.active {
                return;
            }

            let current_query = browse.search.input.text.trim().to_ascii_lowercase();
            let current_cap = browse.search_result_cap.max(1);
            let current_archive_path = browse
                .archive
                .as_ref()
                .map(|archive| archive.listing.archive_path.clone());
            let current_archive_inner_path = browse
                .archive
                .as_ref()
                .map(|archive| archive.inner_path.clone());
            if generation != browse.search.generation
                || root != browse.current_dir
                || recursive != browse.search.recursive
                || archive_path != current_archive_path
                || archive_inner_path != current_archive_inner_path
                || query != current_query
                || mode != browse.search.mode
                || show_hidden != browse.show_hidden
                || audio_only != browse.search.audio_only
                || format_filter != browse.format_filter
                || sort != browse.search.sort
                || sort_dir != browse.search.sort_dir
                || result_cap != current_cap
            {
                log::debug!(
                    "discarded stale Browse search completion for tab {} generation {} root {}",
                    tab_id, generation, root.display()
                );
                return;
            }

            browse.search.searching = false;
            browse.search.cancel = None;
            for (synthetic_path, archive_fingerprint, password_identity, tags) in archive_tag_cache_updates {
                browse.search.archive_tag_cache.insert(
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
            if let Some(ref parent) = browse.parent_entry {
                entries.push(parent.clone());
            }
            entries.extend(scored.into_iter().map(|(e, _)| e));
            browse.entries = entries;
            browse.selected_index = 0;
            browse.scroll_offset = 0;

            if active && total_matches > result_cap {
                truncated_status = Some(format!(
                    "search: showing best {} of {} matches; raise [browsing] search_result_cap to show more",
                    result_cap, total_matches
                ));
            }
            if let Some(status) = truncated_status { app.set_status(status); }
        },
        AppMessage::FilePickerComplete {
            session_id,
            purpose,
            path,
            paths,
            ignored_directories,
        } => {
            let selected_count = paths.len();
            let disclosure_purpose = purpose.clone();
            let consumed =
                reduce_file_picker_complete(app, session_id, purpose, path, paths, tx);
            if consumed {
                append_first_of_many_disclosure(app, &disclosure_purpose, selected_count);
                append_ignored_directory_disclosure(app, ignored_directories);
            }
        }
        AppMessage::FileTaskProgress { session_id, update } => {
            reduce_file_task_progress(app, session_id, update, tx);
        }
        AppMessage::FileTaskComplete {
            session_id,
            report,
            retry_plan,
        } => {
            reduce_file_task_complete(app, session_id, report, retry_plan, tx);
        }
        AppMessage::RenamePlanComplete {
            description,
            base_dir,
            result,
        } => {
            super::keybindings::complete_rename_plan(
                app,
                description,
                base_dir,
                result,
                tx,
            );
        }
        AppMessage::FileOperationReplayComplete {
            session_id,
            entry,
            undo,
            result,
        } => {
            super::keybindings::complete_file_operation_replay(
                app,
                session_id,
                entry,
                undo,
                result,
                tx,
            );
        }
        AppMessage::MetadataEditorDetailsProbeComplete { session_id, generation, total, results } => {
            if let Some(mut taken) = take_metadata_editor_with_restore_slot(app) {
                if let Some(status) = taken
                    .state
                    .apply_details_probe_results(session_id, generation, results)
                {
                    app.set_status(status);
                } else {
                    app.set_status(format!(
                        "metadata editor: ignored stale Details probe result for session {session_id} ({total} file{})",
                        if total == 1 { "" } else { "s" }
                    ));
                }
                restore_taken_metadata_editor(app, taken);
            } else {
                app.set_status("metadata editor: Details probe finished after editor closed");
            }
        }
        AppMessage::MetadataEditorDetailsAnalysisComplete { session_id, generation, total, results } => {
            if let Some(mut taken) = take_metadata_editor_with_restore_slot(app) {
                if !taken.state.complete_details_analysis(session_id, generation) {
                    app.set_status(format!(
                        "metadata editor: ignored stale Details analysis result for session {session_id} generation {generation}"
                    ));
                    restore_taken_metadata_editor(app, taken);
                } else {
                    let mut applied = 0usize;
                    let mut issue_count = 0usize;
                    let mut ignored = 0usize;
                    let mut store_errors = 0usize;

                    if let Some(surface) = taken.state.surface_mut_for_session(session_id) {
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
                                if let (Some(modified), Some(size)) =
                                    (item.modified.clone(), item.file_size)
                                {
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
                                    if let (Some(modified), Some(size)) =
                                        (item.modified, item.file_size)
                                    {
                                        let identity = crate::tui::browse::ProbeCacheIdentity {
                                            modified: Some(modified),
                                            size,
                                        };
                                        app.browse.update_valid_probe_for_identity(
                                            &item.path,
                                            identity,
                                            |cached| {
                                                cached.metadata.hdcd_detail =
                                                    item.facts.hdcd_detail.clone();
                                            },
                                        );
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
                        parts.push(format!("{} stale/unknown ignored", ignored));
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
                    restore_taken_metadata_editor(app, taken);
                }
            } else {
                app.set_status("metadata editor: Details analysis finished after editor closed");
            }
        }
        AppMessage::MetadataEditorReplayGainComplete { session_id, generation, mode, paths, result } => {
            if let Some(mut taken) = take_metadata_editor_with_restore_slot(app) {
                if !taken.state.complete_replaygain_scan(session_id, generation) {
                    app.set_status(format!(
                        "metadata editor: ignored stale ReplayGain scan result for session {session_id} generation {generation}"
                    ));
                } else {
                    match result {
                        Ok(metadata) => {
                            if let Some(surface) = taken.state.surface_mut_for_session(session_id) {
                                metadata_editor_apply_replaygain_metadata(surface, &paths, &metadata);
                                taken.state.mark_archive_staging_dirty();
                                let archive_persist_result = if let Some(context) = taken
                                    .state
                                    .archive_edit_context
                                    .clone()
                                    .filter(|context| {
                                        context.owner
                                            == super::app::ArchiveMetadataEditOwner::Browse
                                    })
                                {
                                    super::keybindings::record_staged_archive_metadata_write(
                                        app,
                                        &context.archive_path,
                                        &context.staging_dir,
                                        super::keybindings::archive_metadata_context_baseline(
                                            &context,
                                        ),
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
                            app.set_status(format!(
                                "metadata editor: ReplayGain scan failed: {err}"
                            ));
                        }
                    }
                }
                restore_taken_metadata_editor(app, taken);
            } else {
                app.set_status("metadata editor: ReplayGain scan finished after editor closed");
            }
        }
        AppMessage::MetadataEditorArtworkWriteComplete { session_id, generation, mode, paths, result } => {
            if let Some(mut taken) = take_metadata_editor_with_restore_slot(app) {
                if !taken.state.complete_artwork_write(session_id, generation) {
                    app.set_status(format!(
                        "metadata editor: ignored stale {} result for session {session_id} generation {generation}",
                        mode.label()
                    ));
                } else {
                    match result {
                        Ok(batch_result) => {
                            let durability_warning_count = batch_result.durability_warnings.len();
                            let first_durability_warning =
                                batch_result.durability_warnings.first().cloned();
                            if let Some(surface) = taken.state.surface_mut_for_session(session_id) {
                                metadata_editor_apply_artwork_metadata(
                                    surface,
                                    &paths,
                                    &batch_result.metadata,
                                );
                                taken.state.mark_archive_staging_dirty();
                                let archive_persist_result = if let Some(context) = taken
                                    .state
                                    .archive_edit_context
                                    .clone()
                                    .filter(|context| {
                                        context.owner
                                            == super::app::ArchiveMetadataEditOwner::Browse
                                    })
                                {
                                    super::keybindings::record_staged_archive_metadata_write(
                                        app,
                                        &context.archive_path,
                                        &context.staging_dir,
                                        super::keybindings::archive_metadata_context_baseline(
                                            &context,
                                        ),
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
                                } else if durability_warning_count > 0 {
                                    app.set_status(format!(
                                        "metadata editor: {} updated {} file{} with {} durability warning{}{}",
                                        mode.label(),
                                        paths.len(),
                                        if paths.len() == 1 { "" } else { "s" },
                                        durability_warning_count,
                                        if durability_warning_count == 1 { "" } else { "s" },
                                        first_durability_warning
                                            .as_ref()
                                            .map(|warning| format!(": {warning}"))
                                            .unwrap_or_default()
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
                            app.set_status(format!(
                                "metadata editor: {} failed: {err}",
                                mode.label()
                            ));
                        }
                    }
                    taken.state.file_picker = None;
                    taken.state.pending_artwork_type = None;
                    taken.state.invalidate_artwork_preview_cache();
                }
                restore_taken_metadata_editor(app, taken);
            } else {
                app.set_status("metadata editor: artwork update finished after editor closed");
            }
        }
        AppMessage::MetadataEditorWriteComplete {
            session_id,
            save_generation,
            results,
            refreshed_entries,
        } => {
            if let Some(mut taken) = take_metadata_editor_with_restore_slot(app) {
                if taken
                    .state
                    .invalid_ape_repair_is_current(session_id, save_generation)
                {
                    if let Some(summary) =
                        super::keybindings::apply_invalid_ape_repair_completion(
                            app,
                            &mut taken.state,
                            session_id,
                            save_generation,
                            results,
                            refreshed_entries,
                        )
                    {
                        app.set_status(summary.status_line());
                    } else {
                        app.set_status(format!(
                            "APE repair: ignored stale completion for session {session_id} generation {save_generation}"
                        ));
                    }
                    restore_taken_metadata_editor(app, taken);
                    return;
                }
                let mut restore_editor = true;
                if let Some(summary) =
                    taken
                        .state
                        .apply_write_results(session_id, save_generation, results)
                {
                    let mut close_editor = summary.all_saved()
                        && taken.state.close_after_successful_save;
                    if summary.all_saved() {
                        super::keybindings::metadata_completion_learn_from_state(
                            &app.db,
                            &taken.state,
                            "write",
                        );
                    }
                    let mut refresh_failure = None;
                    for path in &summary.saved_paths {
                        app.browse.remove_probe_cache_entry(path);
                        let _ = app.db.invalidate_probe(&path.display().to_string());
                    }
                    if !summary.saved_paths.is_empty() {
                        taken.state.mark_archive_staging_dirty();
                    }
                    if let Some(refresh) = refreshed_entries {
                        match refresh {
                            Ok(entries) => {
                                if !taken.state.replace_saved_surface_entries(session_id, entries) {
                                    close_editor = false;
                                    refresh_failure = Some(format!(
                                        "metadata editor: save completed, but refreshed tags no longer match session {session_id}"
                                    ));
                                }
                            }
                            Err(err) => {
                                close_editor = false;
                                let _ = taken.state.mark_saved_surface_refresh_failed(session_id);
                                refresh_failure = Some(format!(
                                    "metadata editor: embedded CUESHEET deleted, but tag refresh failed; reopen or retry before trusting the displayed rows: {err}"
                                ));
                            }
                        }
                    }
                    if close_editor {
                        let archive_context = taken.state.archive_edit_context.clone();
                        restore_editor = false;
                        match archive_context.as_ref().map(|context| context.owner) {
                            Some(super::app::ArchiveMetadataEditOwner::Convert) => {
                                let updated = super::keybindings::sync_convert_archive_preview_metadata_from_editor(
                                    app,
                                    &taken.state,
                                );
                                app.browse.probe_current_with_db(tx, Some(&app.db));
                                app.set_status(format!(
                                    "metadata editor: saved staged archive tags; {} track{} synced to conversion overrides",
                                    updated,
                                    if updated == 1 { "" } else { "s" }
                                ));
                            }
                            Some(super::app::ArchiveMetadataEditOwner::Browse) => {
                                if let Some(context) = archive_context {
                                    if let Err(err) = complete_browse_archive_metadata_save(
                                        app,
                                        tx,
                                        context,
                                        &summary.saved_paths,
                                    ) {
                                        restore_editor = true;
                                        app.set_status(format!(
                                            "metadata editor: saved staged archive tags, but recovery tracking failed; do not quit before saving/discarding: {err}"
                                        ));
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
                        if let Some(status) = refresh_failure {
                            app.set_status(status);
                        } else {
                            app.set_status(summary.status_line());
                        }
                        taken.state.phase = super::app::MetadataEditorPhase::Editing;
                        taken.state.close_after_successful_save = true;
                    }
                } else {
                    app.set_status(format!(
                        "metadata editor: ignored stale save result for session {session_id} generation {save_generation}"
                    ));
                }

                if restore_editor {
                    restore_taken_metadata_editor(app, taken);
                }
            } else {
                app.set_status(
                    "metadata editor: background write finished after the editor closed; disk changes may have committed, so reopen before retrying",
                );
            }
        }
        AppMessage::TagMaintenanceComplete {
            kind,
            session_id,
            save_generation,
            result,
            refreshed_entries,
        } => {
            let mut refresh_failed = false;
            let changed_paths = result
                .as_ref()
                .map(|results| {
                    results
                        .iter()
                        .filter(|entry| entry.changed || entry.commit_state_unknown)
                        .map(|entry| entry.path.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            for path in &changed_paths {
                app.browse.remove_probe_cache_entry(path);
                let _ = app.db.invalidate_probe(&path.display().to_string());
            }

            if let (Some(session_id), Some(save_generation)) = (session_id, save_generation) {
                if let Some(mut taken) = take_metadata_editor_with_restore_slot(app) {
                    let is_current = taken.state.phase == super::app::MetadataEditorPhase::Saving
                        && taken.state.model.editor_save_generation == save_generation
                        && taken.state.active_surface().technical_details.session_id == session_id
                        && taken
                            .state
                            .active_surface()
                            .technical_details
                            .active_save_generation
                            == Some(save_generation);
                    if is_current {
                        taken.state.clear_metadata_write_cancel();
                        taken.state.model.metadata_save_progress = None;
                        taken.state.phase = super::app::MetadataEditorPhase::Editing;
                        taken
                            .state
                            .active_surface_mut()
                            .technical_details
                            .active_save_generation = None;
                        if !changed_paths.is_empty() {
                            taken.state.mark_archive_staging_dirty();
                        }
                        if let Some(refresh) = refreshed_entries {
                            match refresh {
                                Ok(entries) => {
                                    if !taken
                                        .state
                                        .replace_saved_surface_entries(session_id, entries)
                                    {
                                        refresh_failed = true;
                                    }
                                }
                                Err(_) => {
                                    refresh_failed = true;
                                    let _ = taken
                                        .state
                                        .mark_saved_surface_refresh_failed(session_id);
                                }
                            }
                        }
                        app.set_status(super::keybindings::tag_maintenance_status_line(
                            kind,
                            &result,
                            refresh_failed,
                        ));
                    } else {
                        app.set_status(format!(
                            "{}: ignored stale completion for session {session_id} generation {save_generation}",
                            kind.label()
                        ));
                    }
                    restore_taken_metadata_editor(app, taken);
                } else {
                    app.set_status(format!(
                        "{} finished after the metadata editor closed",
                        kind.label()
                    ));
                }
            } else {
                app.browse.refresh_with_search(Some(tx));
                app.browse.probe_current_with_db(tx, Some(&app.db));
                super::disc_browser_actions::probe_selected_disc_after_cursor_move(app, tx);
                app.set_status(super::keybindings::tag_maintenance_status_line(
                    kind,
                    &result,
                    false,
                ));
            }
        }
        AppMessage::MetadataEditorWriteProgress {
            session_id,
            save_generation,
            detail,
        } => {
            if let ActiveOverlay::MetadataEditor(state) = &mut app.active_overlay {
                let _ = state.apply_metadata_save_progress(
                    session_id,
                    save_generation,
                    detail,
                );
            }
        }
        AppMessage::GnudbVirtualDiscTocComplete {
            operation_id,
            result,
        } => {
            if !gnudb_operation_is_current(app, operation_id) {
                return;
            }
            match result {
                Ok((source, durations, label)) => {
                    super::context_menu::launch_virtual_disc_gnudb(
                        operation_id,
                        app,
                        tx,
                        source,
                        durations,
                        label,
                    );
                }
                Err(error) => {
                    retire_gnudb_operation_with_editor_restore(app, operation_id);
                    app.set_status(error);
                }
            }
        }
        AppMessage::GnudbQueryComplete {
            operation_id,
            result,
            paths,
        } => {
            if !gnudb_operation_is_current(app, operation_id) {
                return;
            }
            match result {
                Ok(matches) if matches.len() == 1 => {
                    let Some(read_operation_id) = advance_gnudb_operation(app, operation_id) else {
                        return;
                    };
                    let m = matches[0].clone();
                    app.set_status(format!("GNUDB: found {} — reading...", m.title));
                    spawn_gnudb_worker(tx.clone(), read_operation_id, async move {
                        let result = super::gnudb::read_gnudb(&m.category, &m.disc_id).await;
                        AppMessage::GnudbReadComplete {
                            operation_id: read_operation_id,
                            result,
                            paths,
                            origin_matches: None,
                        }
                    });
                }
                Ok(matches) if matches.is_empty() => {
                    retire_gnudb_operation_with_editor_restore(app, operation_id);
                    app.set_status("GNUDB: no matches found");
                }
                Ok(matches) => {
                    let Some(picker_operation_id) = advance_gnudb_operation(app, operation_id) else {
                        return;
                    };
                    if !gnudb_operation_has_overlay_authority(app, picker_operation_id) {
                        retire_gnudb_operation_with_editor_restore(app, picker_operation_id);
                        app.set_status(
                            "GNUDB: result discarded because another overlay owns the screen; retry the lookup"
                                .to_string(),
                        );
                        return;
                    }
                    app.set_status(format!("GNUDB: {} matches found", matches.len()));
                    app.active_overlay = ActiveOverlay::GnudbSelect {
                        operation_id: picker_operation_id,
                        matches,
                        selected: 0,
                        scroll: 0,
                        paths,
                    };
                }
                Err(e) => {
                    retire_gnudb_operation_with_editor_restore(app, operation_id);
                    app.set_status(format!("GNUDB error: {}", e));
                }
            }
        }
        AppMessage::GnudbReadComplete {
            operation_id,
            result,
            paths,
            origin_matches,
        } => {
            if !gnudb_operation_is_current(app, operation_id) {
                return;
            }
            match result {
                Ok(entry) => {
                    if !gnudb_operation_has_overlay_authority(app, operation_id) {
                        retire_gnudb_operation_with_editor_restore(app, operation_id);
                        app.set_status(
                            "GNUDB: read result discarded because another overlay owns the screen; retry the lookup"
                                .to_string(),
                        );
                        return;
                    }
                    let editor_session = app
                        .active_gnudb_operation
                        .and_then(|active| active.editor_session);
                    finish_gnudb_operation_if_current(app, operation_id);
                    let mut review = super::gnudb::build_review_state(&entry, paths);
                    review.origin_matches = origin_matches;
                    review.editor_session = editor_session;
                    app.set_status(format!(
                        "GNUDB: {} / {} ({} tracks) — review and edit",
                        entry.artist,
                        entry.album,
                        entry.tracks.len(),
                    ));
                    app.active_overlay = ActiveOverlay::GnudbReview(Box::new(review));
                }
                Err(e) => {
                    retire_gnudb_operation_with_editor_restore(app, operation_id);
                    app.set_status(format!("GNUDB read error: {}", e));
                }
            }
        }
        AppMessage::GnudbMultiDiscComplete {
            operation_id,
            entries,
            failures,
            attempted,
        } => {
            if !gnudb_operation_is_current(app, operation_id) {
                return;
            }
            for failure in &failures {
                log::warn!("GNUDB multi-disc lookup: {failure}");
            }
            if entries.is_empty() {
                retire_gnudb_operation_with_editor_restore(app, operation_id);
                if failures.is_empty() {
                    app.set_status("GNUDB: no matches found for any disc");
                } else if failures.len() == attempted {
                    app.set_status(format!(
                        "GNUDB: all {attempted} disc lookups failed: {}",
                        failures[0]
                    ));
                } else {
                    app.set_status(format!(
                        "GNUDB: no matches found; {} of {attempted} disc lookups failed: {}",
                        failures.len(),
                        failures[0]
                    ));
                }
                return;
            }
            if !gnudb_operation_has_overlay_authority(app, operation_id) {
                retire_gnudb_operation_with_editor_restore(app, operation_id);
                app.set_status(
                    "GNUDB: multi-disc result discarded because another overlay owns the screen; retry the lookup"
                        .to_string(),
                );
                return;
            }
            let editor_session = app
                .active_gnudb_operation
                .and_then(|active| active.editor_session);
            finish_gnudb_operation_if_current(app, operation_id);

            let mut review = super::gnudb::build_multi_disc_review_state(&entries);
            review.editor_session = editor_session;
            let n_discs = entries.len();
            let n_tracks: usize = entries.iter().map(|(_, e, _)| e.tracks.len()).sum();
            let failure_suffix = if failures.is_empty() {
                String::new()
            } else {
                format!("; {} of {attempted} disc lookups failed", failures.len())
            };
            app.set_status(format!(
                "GNUDB: {} disc{}, {} tracks — review and edit{}",
                n_discs,
                if n_discs == 1 { "" } else { "s" },
                n_tracks,
                failure_suffix,
            ));
            app.active_overlay = ActiveOverlay::GnudbReview(Box::new(review));
        }
        AppMessage::GnudbWorkerFailed {
            operation_id,
            detail,
        } => {
            if !gnudb_operation_is_current(app, operation_id) {
                return;
            }
            retire_gnudb_operation_with_editor_restore(app, operation_id);
            app.set_status(format!("GNUDB worker failed: {detail}; the lookup was retired"));
        }

        AppMessage::CtdbComplete {
            operation_id,
            mut pages,
        } => {
            let kind = CompletionOperationKind::Ctdb;
            if !completion_operation_is_current(app, kind, operation_id) {
                return;
            }

            // Persist newly computed parity only for the current operation.
            for page in pages.iter_mut() {
                if let Some((key, parity)) = page.result.parity_cache_write.take() {
                    if let Err(error) = app.db.store_ctdb_parity(&key, 16, &parity) {
                        log::warn!("CTDB parity cache store failed: {}", error);
                    }
                }
            }

            if pages.is_empty() {
                app.auto_repair_on_ctdb_complete = false;
                retire_completion_operation(app, kind, operation_id);
                app.set_status("CUETools DB: no disc could be verified");
                return;
            }

            let status = if pages.len() == 1 {
                let summary = crate::tui::ctdb::format_ctdb_summary(&pages[0].result);
                format!("CUETools DB: {}", summary)
            } else {
                let total: usize = pages.iter().map(|page| page.result.tracks.len()).sum();
                let verified: usize = pages
                    .iter()
                    .map(|page| {
                        page.result
                            .tracks
                            .iter()
                            .filter(|track| {
                                matches!(
                                    track.status,
                                    crate::tui::ctdb::CtdbTrackStatus::Verified
                                        | crate::tui::ctdb::CtdbTrackStatus::VerifiedRs
                                )
                            })
                            .count()
                    })
                    .sum();
                format!(
                    "CUETools DB: {} discs, {}/{} tracks verified",
                    pages.len(),
                    verified,
                    total,
                )
            };

            let auto_repair = std::mem::replace(&mut app.auto_repair_on_ctdb_complete, false);
            let active_page = if auto_repair {
                pages
                    .iter()
                    .position(|page| {
                        page.result.parity_url.is_some()
                            && page.result.tracks.iter().any(|track| {
                                track.status == crate::tui::ctdb::CtdbTrackStatus::Mismatch
                            })
                    })
                    .unwrap_or(0)
            } else {
                0
            };

            let may_publish =
                completion_operation_has_overlay_authority(app, kind, operation_id);
            retire_completion_operation(app, kind, operation_id);
            if !may_publish {
                app.set_status(format!(
                    "{}; current editor or overlay preserved",
                    status
                ));
                return;
            }

            app.active_overlay =
                ActiveOverlay::CtdbVerify(Box::new(crate::tui::app::CtdbVerifyState {
                    pages,
                    active_page,
                    scroll: 0,
                }));
            app.set_status(status);

            if auto_repair {
                super::command::execute_command(app, super::command::Command::CtdbRepair, tx);
            }
        }
        AppMessage::ArBatchComplete {
            operation_id,
            result,
        } => {
            let kind = CompletionOperationKind::ArBatch;
            if !completion_operation_is_current(app, kind, operation_id) {
                return;
            }
            let total = result.albums.len();
            let verified = result
                .albums
                .iter()
                .filter(|album| {
                    album.verified == album.total_tracks
                        && album.total_tracks > 0
                        && !album.not_in_db
                })
                .count();
            let report_message = result
                .report_path
                .as_ref()
                .map(|path| format!(" — report: {}", path.display()))
                .unwrap_or_default();
            let status = format!(
                "Batch AR: {}/{} albums verified{}",
                verified, total, report_message,
            );
            let may_publish =
                completion_operation_has_overlay_authority(app, kind, operation_id);
            retire_completion_operation(app, kind, operation_id);
            if may_publish {
                app.active_overlay = ActiveOverlay::ArBatchReport { result, scroll: 0 };
                app.set_status(status);
            } else {
                app.set_status(format!(
                    "{}; current editor or overlay preserved",
                    status
                ));
            }
        }
        AppMessage::OffsetCorrectionComplete {
            operation_id,
            result,
        } => {
            let kind = CompletionOperationKind::OffsetCorrection;
            if !completion_operation_is_current(app, kind, operation_id) {
                return;
            }
            retire_completion_operation(app, kind, operation_id);
            match result {
                Ok(summary) => {
                    app.set_status(summary);
                    // Never null the overlay slot: retirement restores the
                    // exact parked editor when it still owns that session.
                    app.browse.refresh_with_search(Some(tx));
                }
                Err(error) => {
                    app.set_status(format!("Offset correction failed: {}", error));
                }
            }
        }
        AppMessage::CtdbRepairComplete {
            operation_id,
            result,
        } => {
            let kind = CompletionOperationKind::CtdbRepair;
            if !completion_operation_is_current(app, kind, operation_id) {
                return;
            }
            retire_completion_operation(app, kind, operation_id);
            match result {
                Ok(summary) => {
                    app.set_status(summary);
                    app.browse.refresh_with_search(Some(tx));
                }
                Err(error) => {
                    app.set_status(format!("CTDB repair failed: {}", error));
                }
            }
        }
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
        AppMessage::CuePreviewComplete {
            operation_id,
            result,
        } => {
            if !cue_operation_is_current(app, operation_id) {
                return;
            }
            match result {
                Ok((cue_content, cue_path, summary)) => {
                    if !cue_operation_has_overlay_authority(app, operation_id) {
                        finish_cue_operation_if_current(app, operation_id);
                        app.set_status(
                            "MusicBrainz CUE: preview discarded because another workflow owns the editor or overlay; retry the command"
                                .to_string(),
                        );
                        return;
                    }
                    finish_cue_operation_if_current(app, operation_id);
                    app.active_overlay = ActiveOverlay::CuePreview(Box::new(
                        super::app::CuePreviewState::new(cue_content, cue_path, summary.clone()),
                    ));
                    app.set_status(summary);
                }
                Err(err) => {
                    finish_cue_operation_if_current(app, operation_id);
                    app.set_status(format!("MusicBrainz CUE: {}", err));
                }
            }
        }
        AppMessage::CueFillPrepComplete {
            operation_id,
            cue_path,
            result,
        } => {
            if !cue_operation_is_current(app, operation_id) {
                return;
            }
            let (album, tracks, layout, sectors) = match result {
                Ok(prep) => prep,
                Err(err) => {
                    finish_cue_operation_if_current(app, operation_id);
                    app.set_status(format!(":cue-fill: {}", err));
                    return;
                }
            };
            let toc_string = match super::musicbrainz::build_mb_toc(&sectors) {
                Some(s) => s,
                None => {
                    finish_cue_operation_if_current(app, operation_id);
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
                let worker = tokio::spawn(async move {
                    super::musicbrainz::lookup_release_by_toc(&sectors, cached).await
                });
                let outcome = worker.await.unwrap_or_else(|err| {
                    Err(format!(":cue-fill lookup worker failed: {err}"))
                });
                let _ = tx
                    .send(AppMessage::CueFillComplete {
                        operation_id,
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
            operation_id,
            outcome,
            paths,
            output_dir,
            single_image,
            toc_string,
        } => {
            if !cue_operation_is_current(app, operation_id) {
                return;
            }
            handle_cue_mb_complete(
                app,
                tx,
                operation_id,
                outcome,
                paths,
                output_dir,
                single_image,
                toc_string,
            );
        }
        AppMessage::CueFillComplete {
            operation_id,
            outcome,
            cue_path,
            album,
            tracks,
            layout,
            toc_string,
        } => {
            if !cue_operation_is_current(app, operation_id) {
                return;
            }
            handle_cue_fill_complete(
                app,
                tx,
                operation_id,
                outcome,
                cue_path,
                *album,
                tracks,
                layout,
                toc_string,
            );
        }
        AppMessage::TagsFromMbComplete { outcome, ctx } => {
            dispatch_tags_from_mb_complete(app, tx, outcome, ctx);
        }
        AppMessage::SplitCueAlbumGroupingComplete { request, result } => {
            super::command::handle_split_cue_album_grouping_complete(
                app,
                tx,
                *request,
                result,
            );
        }
        AppMessage::GnudbSplitCueAlbumGroupingComplete {
            operation_id,
            infos,
            active_audio_path,
            result,
        } => {
            super::context_menu::handle_gnudb_split_cue_album_grouping_complete(
                app,
                tx,
                operation_id,
                infos,
                active_audio_path,
                result,
            );
        }
        AppMessage::MetadataEditorSplitCueAlbumGroupingComplete {
            operation_id,
            infos,
            active_cue_path,
            ordinary_paths,
            metadata_sidecar_cue_paths,
            cue_admission_warnings,
            cue_policy,
            result,
        } => {
            if !cue_operation_is_current(app, operation_id) {
                return;
            }
            super::keybindings::handle_metadata_editor_split_cue_album_grouping_complete(
                app,
                tx,
                operation_id,
                infos,
                active_cue_path,
                ordinary_paths,
                metadata_sidecar_cue_paths,
                cue_admission_warnings,
                cue_policy,
                result,
            );
        }
        AppMessage::InEditorSplitCueMusicBrainzInfoComplete { request, result } => {
            super::command::handle_in_editor_split_cue_musicbrainz_info_complete(
                app,
                tx,
                *request,
                result,
            );
        }
        AppMessage::TagsMbApplyReady {
            operation_id,
            releases,
            selected,
            paths,
            editor_session,
            decision,
        } => {
            complete_tags_mb_apply_operation(
                app,
                tx,
                operation_id,
                releases,
                selected,
                paths,
                editor_session,
                decision,
            );
        }
        AppMessage::MbDetailPrefetchComplete {
            operation_id,
            release_id,
            result,
        } => {
            // Persist the paid-for response regardless of picker lifetime, but
            // stamp in-memory state only onto the picker that launched it.
            if let Ok(outcome) = result {
                if let Some((key, body)) = outcome.cache_write {
                    if let Err(e) = app.db.store_mb_search(&key, &body) {
                        log::warn!("mb search cache store failed: {}", e);
                    }
                }
                if let Some(release) = outcome.release {
                    let identity_is_current = tags_mb_operation_is_current(app, operation_id);
                    if identity_is_current {
                        if let ActiveOverlay::MbSelect(ref mut state) = app.active_overlay {
                            if state.operation_id == operation_id {
                                state.prefetch.insert(release_id, release);
                                return;
                            }
                        }
                        if let Some(state) = app.pending_mb_select.as_mut() {
                            if state.operation_id == operation_id {
                                state.prefetch.insert(release_id, release);
                            }
                        }
                    }
                }
            }
        }
        AppMessage::MbSelectedDetailComplete {
            operation_id,
            mut releases,
            selected,
            paths,
            editor_session,
            result,
        } => {
            let mut outcome = match result {
                Ok(outcome) => outcome,
                Err(error) => {
                    recover_from_selected_mb_detail_failure(
                        app,
                        operation_id,
                        format!(":tags-mb: selected release detail failed: {error}"),
                    );
                    return;
                }
            };

            // A completed request is useful cache material even if its owning
            // picker was cancelled while it was in flight. Authority checks
            // below still prevent stale work from mutating editor state.
            if let Some((key, body)) = outcome.cache_write.take() {
                if let Err(error) = app.db.store_mb_search(&key, &body) {
                    log::warn!("mb search cache store failed: {}", error);
                }
            }
            if !tags_mb_operation_is_current_phase(
                app,
                operation_id,
                super::app::TagsMbOperationPhase::Verifying,
            ) {
                return;
            }

            let Some(release) = outcome.release else {
                recover_from_selected_mb_detail_failure(
                    app,
                    operation_id,
                    ":tags-mb: selected MusicBrainz release no longer exists".to_string(),
                );
                return;
            };
            if !release.relationship_projection_complete {
                recover_from_selected_mb_detail_failure(
                    app,
                    operation_id,
                    ":tags-mb: MusicBrainz detail omitted required recording/work relationships; no tags were applied"
                        .to_string(),
                );
                return;
            }
            let Some(slot) = releases.get_mut(selected) else {
                recover_from_selected_mb_detail_failure(
                    app,
                    operation_id,
                    ":tags-mb: invalid release index while resolving selected detail".to_string(),
                );
                return;
            };
            *slot = release.clone();

            // Keep picker back-navigation/detail rendering coherent with the
            // exact authoritative release that is about to be applied.
            let release_id = release.release_id.clone();
            if let ActiveOverlay::MbSelect(state) = &mut app.active_overlay {
                if state.phase.verifying_operation() == Some(operation_id) {
                    state.prefetch.insert(release_id.clone(), release.clone());
                }
            }
            if let Some(state) = app.pending_mb_select.as_mut() {
                if state.phase.verifying_operation() == Some(operation_id) {
                    state.prefetch.insert(release_id, release);
                }
            }

            open_editor_with_mb_release_for_operation(
                app,
                tx,
                operation_id,
                releases,
                selected,
                paths,
                editor_session,
            );
        }
        AppMessage::AccurateRipComplete {
            operation_id,
            pages,
        } => {
            let kind = CompletionOperationKind::AccurateRip;
            if !completion_operation_is_current(app, kind, operation_id) {
                return;
            }

            if pages.is_empty() {
                app.auto_fix_on_complete = false;
                app.pending_ctdb_repair = None;
                retire_completion_operation(app, kind, operation_id);
                app.set_status("AccurateRip: no disc could be verified");
                return;
            }

            let total: usize = pages.iter().map(|page| page.result.tracks.len()).sum();
            let verified: usize = pages
                .iter()
                .map(|page| {
                    page.result
                        .tracks
                        .iter()
                        .filter(|track| {
                            track.status == crate::tui::accuraterip::ArTrackStatus::Verified
                        })
                        .count()
                })
                .sum();
            let mut status = if pages.len() == 1 {
                format!(
                    "AccurateRip: {}",
                    crate::tui::accuraterip::format_summary(&pages[0].result)
                )
            } else {
                format!(
                    "AccurateRip: {} discs, {}/{} tracks verified",
                    pages.len(),
                    verified,
                    total,
                )
            };

            // Persist current-operation results even when another surface now
            // owns the overlay slot; authority controls UI publication, not the
            // value of a completed verification cache entry.
            for page in &pages {
                for track in &page.result.tracks {
                    if let Ok(meta) = std::fs::metadata(&track.path) {
                        let mtime = meta
                            .modified()
                            .map(crate::db::systemtime_to_unix)
                            .unwrap_or(0);
                        if let Err(error) = app.db.store_ar(
                            &track.path.display().to_string(),
                            mtime,
                            meta.len(),
                            std::slice::from_ref(track),
                            &page.result.disc_id_str,
                        ) {
                            log::error!("AR cache store failed: {}", error);
                        }
                    }
                }
            }

            let matched_page_idx = app
                .pending_ctdb_repair
                .as_ref()
                .and_then(|pending| pending.paths.first().cloned())
                .and_then(|target| {
                    pages.iter().position(|page| {
                        page.result.tracks.first().map(|track| &track.path) == Some(&target)
                    })
                });

            if app.pending_ctdb_repair.is_some() && matched_page_idx.is_none() {
                app.pending_ctdb_repair = None;
                status.push_str(
                    "; discarded stale deferred CTDB repair (AR result did not match its first track)",
                );
            }

            let may_publish =
                completion_operation_has_overlay_authority(app, kind, operation_id);
            if !may_publish {
                app.auto_fix_on_complete = false;
                app.pending_ctdb_repair = None;
                retire_completion_operation(app, kind, operation_id);
                app.set_status(format!(
                    "{}; current editor or overlay preserved",
                    status
                ));
                return;
            }

            if let Some(index) = matched_page_idx {
                let Some(pending) = app.pending_ctdb_repair.take() else {
                    retire_completion_operation(app, kind, operation_id);
                    app.set_status(
                        "AccurateRip: deferred CTDB repair state disappeared before completion",
                    );
                    return;
                };
                let page = &pages[index];
                let resolved_offset = {
                    let tracks = &page.result.tracks;
                    if tracks.is_empty() {
                        None
                    } else {
                        let mut common = None;
                        let mut all_verified = true;
                        for track in tracks {
                            if track.status
                                != crate::tui::accuraterip::ArTrackStatus::Verified
                            {
                                all_verified = false;
                                break;
                            }
                            let Some(offset) = track.offset else {
                                all_verified = false;
                                break;
                            };
                            match common {
                                Some(previous) if previous != offset => {
                                    all_verified = false;
                                    break;
                                }
                                None => common = Some(offset),
                                _ => {}
                            }
                        }
                        all_verified.then_some(common).flatten()
                    }
                };
                let (offset, offset_note) = match resolved_offset {
                    Some(offset) => (
                        offset,
                        format!("offset: {:+} samples (from AR verification)", offset),
                    ),
                    None => (
                        0,
                        "offset: +0 (AR could not determine a drive offset — proceeding at +0 may produce incorrect repairs if your drive has a real read offset)"
                            .to_string(),
                    ),
                };
                let message = format!(
                    "Apply CTDB Reed-Solomon repair to {} tracks?\n\
                     Parity: {} symbols, {}\n\
                     Files will be re-encoded and verified before replacing originals.",
                    pending.paths.len(),
                    pending.npar,
                    offset_note,
                );
                let action = match pending.single_image {
                    Some(info) => super::app::ConfirmAction::CtdbRepairSingleImage {
                        info,
                        parity_url: pending.parity_url,
                        npar: pending.npar,
                        offset,
                        expected_crcs: pending.expected_crcs,
                    },
                    None => super::app::ConfirmAction::CtdbRepair {
                        paths: pending.paths,
                        parity_url: pending.parity_url,
                        npar: pending.npar,
                        offset,
                        expected_crcs: pending.expected_crcs,
                    },
                };
                retire_completion_operation(app, kind, operation_id);
                app.active_overlay = ActiveOverlay::Confirmation { message, action };
                app.set_status(status);
                return;
            }

            if app.auto_fix_on_complete {
                app.auto_fix_on_complete = false;
                if let Some((paths, offset)) = pages.iter().find_map(|page| {
                    crate::tui::accuraterip::detect_uniform_offset(&page.result).map(|offset| {
                        (
                            page.result
                                .tracks
                                .iter()
                                .map(|track| track.path.clone())
                                .collect::<Vec<_>>(),
                            offset,
                        )
                    })
                }) {
                    let message = format!(
                        "Apply offset correction ({:+} samples) to {} tracks?\n\
                         Files will be re-encoded to FLAC and verified at offset +0\n\
                         before replacing originals.",
                        offset,
                        paths.len(),
                    );
                    retire_completion_operation(app, kind, operation_id);
                    app.active_overlay = ActiveOverlay::Confirmation {
                        message,
                        action: super::app::ConfirmAction::OffsetCorrection { paths, offset },
                    };
                    app.set_status(status);
                    return;
                }
                status = "No offset correction needed — showing verification results".to_string();
            }

            retire_completion_operation(app, kind, operation_id);
            app.active_overlay = ActiveOverlay::AccurateRipVerify(Box::new(super::app::ArVerifyState {
                pages,
                active_page: 0,
                scroll: 0,
            }));
            app.set_status(status);
        }
    }
}

/// Insert the retained first line of a terminal paste into a single-line input.
/// An empty retained line is a no-op, matching the pre-string-insertion behavior
/// where the old character loop executed zero times and preserved selections.
fn insert_single_line_terminal_paste(input: &mut TextInputState, text: &str) {
    let first_line = text.lines().next().unwrap_or("");
    if !first_line.is_empty() {
        input.insert_string(first_line);
    }
}

/// Handle a bracketed paste event. When the BulkRename overlay is active,
/// multi-line paste replaces the template-derived targets line-by-line.
/// In text input overlays, the pasted text is inserted at the cursor.
fn handle_paste(app: &mut AppState, text: &str, tx: &mpsc::Sender<AppMessage>) {
    // The dedicated picker owns its focused text fields. When navigation owns
    // focus, an intercepted Ctrl+V arrives as Event::Paste and is promoted to
    // the same filesystem-paste command as Ctrl+V/Ctrl+P.
    let mut picker_empty_clipboard = false;
    if let ActiveOverlay::FilePicker(session) = &mut app.active_overlay {
        if session.picker.handle_terminal_paste(text) {
            return;
        }
        if session.picker.has_filesystem_clipboard() {
            let _ = session.picker.paste_clipboard();
            return;
        }
        picker_empty_clipboard = true;
    }
    if picker_empty_clipboard {
        app.set_status(
            "terminal paste found no focused text editor and the filesystem clipboard is empty; use Ctrl+C/Ctrl+X first, or focus a text field",
        );
        return;
    }

    match &app.active_overlay {
        ActiveOverlay::BulkRename(_) => {
            let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
            if let ActiveOverlay::BulkRename(mut state) = overlay {
                if state.focus == super::app::BulkRenameFocus::Template {
                    // Template focus: insert the first line into the template
                    // input (single-line field), then rebuild the plan.
                    insert_single_line_terminal_paste(&mut state.template_input, text);
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
            let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
            match overlay {
                ActiveOverlay::TextEdit {
                    mut input,
                    target,
                    label,
                } => {
                    insert_single_line_terminal_paste(&mut input, text);
                    app.active_overlay = ActiveOverlay::TextEdit {
                        input,
                        target,
                        label,
                    };
                }
                ActiveOverlay::CommandInput { mut input, .. } => {
                    insert_single_line_terminal_paste(&mut input, text);
                    // Clear completion — pasted text invalidates candidates.
                    app.active_overlay = ActiveOverlay::CommandInput {
                        input,
                        completion: None,
                    };
                }
                ActiveOverlay::FileInput { mut input } => {
                    insert_single_line_terminal_paste(&mut input, text);
                    app.active_overlay = ActiveOverlay::FileInput { input };
                }
                other => {
                    app.active_overlay = other;
                }
            }
        }
        ActiveOverlay::MetadataAutoNumber(_) => {
            let overlay =
                std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
            if let ActiveOverlay::MetadataAutoNumber(mut state) = overlay {
                if let Some(input) = state.prefix_input.as_mut() {
                    insert_single_line_terminal_paste(input, text);
                }
                app.active_overlay = ActiveOverlay::MetadataAutoNumber(state);
            }
        }
        ActiveOverlay::MetadataEditor(_) => {
            let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
            if let ActiveOverlay::MetadataEditor(mut state) = overlay {
                if let Some(file_picker) = state.file_picker.as_mut() {
                    if file_picker.picker.handle_terminal_paste(text) {
                        app.active_overlay = ActiveOverlay::MetadataEditor(state);
                        return;
                    }
                    if file_picker.picker.has_filesystem_clipboard() {
                        let _ = file_picker.picker.paste_clipboard();
                    } else {
                        app.set_status(
                            "terminal paste found no focused picker text editor and the filesystem clipboard is empty; use Ctrl+C/Ctrl+X first, or focus a text field",
                        );
                    }
                    app.active_overlay = ActiveOverlay::MetadataEditor(state);
                    return;
                }
                use super::app::MetadataEditorPhase;
                if state.phase == MetadataEditorPhase::DetailEdit {
                    if let Some(input) = state.detail_edit.as_mut() {
                        // Detail row editors are single-line inputs. Keep terminal
                        // paste on the focused editor; commit owns list parsing.
                        insert_single_line_terminal_paste(input, text);
                    } else {
                        let field_idx = state.detail_field_idx;
                        if field_idx < state.active_surface().entries.len() {
                            let changed =
                                super::keybindings::metadata_editor_apply_detail_whole_field_text(
                                    &mut state,
                                    field_idx,
                                    text,
                                );
                            app.set_status(if changed > 0 {
                                format!(
                                    "Pasted clipboard lines into {changed} track{}; review in the detail view before saving",
                                    if changed == 1 { "" } else { "s" }
                                )
                            } else {
                                "Clipboard paste made no changes to this field".to_string()
                            });
                        }
                    }
                } else if state.phase == MetadataEditorPhase::InlineEdit {
                    // Single-field inline edit: insert first line at cursor.
                    if let Some(ref mut input) = state.edit_input {
                        insert_single_line_terminal_paste(input, text);
                    }
                } else if state.phase == MetadataEditorPhase::Editing {
                    if let Err(reason) =
                        super::keybindings::metadata_editor_apply_row_or_block_paste(
                            app,
                            &mut state,
                            text,
                        )
                    {
                        app.set_status(reason);
                    }
                }
                app.active_overlay = ActiveOverlay::MetadataEditor(state);
            }
        }
        ActiveOverlay::FileTaskProgress(_) => {
            app.set_status(
                "terminal paste is unavailable while file-task progress is open; close it before editing text",
            );
        }
        ActiveOverlay::None => {
            if app.current_screen != super::app::AppScreen::Browse {
                app.set_status(
                    "terminal paste has no focused text editor on this screen; return to Browse for filesystem paste",
                );
                return;
            }
            // Browse-local editors are not ActiveOverlay variants. Route the
            // terminal clipboard through the same focus precedence as key
            // dispatch and replace any active selection at the cursor.
            let first_line = text.lines().next().unwrap_or("");
            if let Some(edit) = app.browse_inline_edit.as_mut() {
                edit.input.insert_string(first_line);
                return;
            }
            if app.browse.search.active
                && app.browse.search.focus == super::browse::SearchFocus::Input
            {
                app.browse.search.input.insert_string(first_line);
                app.browse.search.last_keystroke = Some(std::time::Instant::now());
                return;
            }
            if let Some(input) = app.browse.path_input.as_mut() {
                input.insert_string(first_line);
                return;
            }
            if let Some(input) = app.browse.filter_input.as_mut() {
                input.insert_string(first_line);
                app.browse.update_filter_from_input();
                return;
            }
            if app.browse.filesystem_clipboard.is_some() {
                let paste_key = crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Char('p'),
                    crossterm::event::KeyModifiers::CONTROL,
                );
                super::keybindings::handle_browse_filesystem_clipboard_key(app, paste_key, tx);
            } else {
                app.set_status(
                    "terminal paste found no focused text editor and the filesystem clipboard is empty; use Ctrl+C/Ctrl+X first, or focus a text field; Ctrl+P is the alternate paste chord",
                );
            }
        }
        _ => {
            app.set_status(
                "terminal paste is unavailable while this overlay is open; close it before pasting",
            );
        }
    }
}

#[cfg(test)]
mod browse_bracketed_paste_tests {
    use super::*;
    use crate::config::TonepoetConfig;
    use crate::tui::app::{
        BrowseInlineEditState, BrowseInlineEditTarget, FilePickerPurpose,
        MetadataFilePickerState,
    };
    use crate::tui::text_input::TextInputState;

    fn test_tx() -> mpsc::Sender<AppMessage> {
        let (tx, _rx) = mpsc::channel(8);
        tx
    }

    fn select_picker_tree_path(
        picker: &mut tui_file_picker::FilePickerState,
        target: &std::path::Path,
    ) {
        // The initial tree materializes only roots plus the start_dir ancestor
        // chain; ancestors of `target` are (re)expanded on the way down via
        // toggle_tree_node, which reads children live with the picker's own
        // show_hidden policy. A collapse+expand double-toggle refreshes
        // ancestors that were pre-expanded with stale or hidden-excluded
        // children. tempdirs sit under dot-prefixed paths, so fixtures must
        // enable show_hidden.
        picker.set_focus(tui_file_picker::FilePickerFocus::Tree);
        let mut index = 0usize;
        for _ in 0..131072 {
            picker.set_tree_cursor(index, usize::MAX);
            let Some(current) = picker.tree_cursor_path().map(std::path::Path::to_path_buf)
            else {
                break;
            };
            if current == target {
                return;
            }
            if target.starts_with(&current) {
                if picker.tree_cursor_is_expanded() {
                    picker.toggle_tree_node(index);
                }
                picker.toggle_tree_node(index);
                index += 1;
                continue;
            }
            let before = picker.tree_cursor_path().map(std::path::Path::to_path_buf);
            picker.set_tree_cursor(index + 1, usize::MAX);
            let after = picker.tree_cursor_path().map(std::path::Path::to_path_buf);
            if before == after {
                break;
            }
            index += 1;
        }
        panic!("picker tree path was not materialized: {}", target.display());
    }

    #[test]
    fn bracketed_paste_replaces_browse_inline_selection() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = super::super::app::AppScreen::Browse;
        app.browse_inline_edit = Some(BrowseInlineEditState {
            target: BrowseInlineEditTarget::Rename {
                path: std::path::PathBuf::from("track.flac"),
            },
            input: TextInputState::new_selected("track.flac".to_string()),
        });

        handle_paste(&mut app, "renamed.flac\nignored", &test_tx());

        let input = &app.browse_inline_edit.as_ref().expect("inline edit").input;
        assert_eq!(input.text, "renamed.flac");
        assert_eq!(input.cursor, "renamed.flac".len());
    }

    #[test]
    fn bracketed_paste_routes_by_browse_text_focus_precedence() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = super::super::app::AppScreen::Browse;
        app.browse.search.active = true;
        app.browse.search.focus = super::super::browse::SearchFocus::Input;
        app.browse.search.input = TextInputState::new_selected("old search".to_string());
        app.browse.path_input = Some(TextInputState::new_selected("/old/path".to_string()));
        app.browse.filter_input = Some(TextInputState::new_selected("old filter".to_string()));

        handle_paste(&mut app, "new search", &test_tx());

        assert_eq!(app.browse.search.input.text, "new search");
        assert_eq!(
            app.browse.path_input.as_ref().expect("path input").text,
            "/old/path"
        );
        assert_eq!(
            app.browse.filter_input.as_ref().expect("filter input").text,
            "old filter"
        );

        app.browse.search.active = false;
        handle_paste(&mut app, "/new/path", &test_tx());
        assert_eq!(
            app.browse.path_input.as_ref().expect("path input").text,
            "/new/path"
        );
    }

    #[tokio::test]
    async fn bracketed_paste_without_text_focus_starts_filesystem_paste_when_available() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let destination_dir = temp.path().join("destination");
        std::fs::create_dir(&source_dir).expect("source dir");
        std::fs::create_dir(&destination_dir).expect("destination dir");
        let source = source_dir.join("track.flac");
        std::fs::write(&source, b"audio").expect("source");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = super::super::app::AppScreen::Browse;
        app.browse.current_dir = destination_dir;
        app.browse.filesystem_clipboard = tui_file_picker::FilesystemClipboard::new(
            tui_file_picker::FilePickerClipboardMode::Copy,
            vec![source],
        );

        handle_paste(&mut app, "terminal clipboard payload is intentionally ignored", &test_tx());

        assert!(
            !app.file_transfers.pending_by_session.is_empty(),
            "intercepted Ctrl+V must become file paste when no editor owns focus"
        );
    }

    #[tokio::test]
    async fn bracketed_paste_with_search_results_focus_starts_filesystem_paste() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let destination_dir = temp.path().join("destination");
        std::fs::create_dir(&source_dir).expect("source dir");
        std::fs::create_dir(&destination_dir).expect("destination dir");
        let source = source_dir.join("track.flac");
        std::fs::write(&source, b"audio").expect("source");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = super::super::app::AppScreen::Browse;
        app.browse.current_dir = destination_dir;
        app.browse.search.active = true;
        app.browse.search.focus = super::super::browse::SearchFocus::Results;
        app.browse.filesystem_clipboard = tui_file_picker::FilesystemClipboard::new(
            tui_file_picker::FilePickerClipboardMode::Copy,
            vec![source],
        );

        handle_paste(&mut app, "ignored terminal payload", &test_tx());

        assert!(!app.file_transfers.pending_by_session.is_empty());
    }

    #[tokio::test]
    async fn picker_files_bracketed_paste_uses_current_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let destination_dir = temp.path().join("destination");
        std::fs::create_dir(&source_dir).expect("source dir");
        std::fs::create_dir(&destination_dir).expect("destination dir");
        let source = source_dir.join("track.flac");
        std::fs::write(&source, b"audio").expect("source");

        let mut picker = tui_file_picker::FilePickerState::new(
            tui_file_picker::FilePickerConfig {
                start_dir: source_dir,
                show_hidden: true,
                ..tui_file_picker::FilePickerConfig::default()
            },
        );
        let source_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == source)
            .expect("source visible");
        picker.set_file_cursor(source_index, 4);
        picker.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        assert!(picker.navigate_to_dir(destination_dir.clone()));
        picker.set_focus(tui_file_picker::FilePickerFocus::Files);

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::FilePicker(MetadataFilePickerState::new(
            FilePickerPurpose::SelectFile,
            picker,
        ));

        handle_paste(
            &mut app,
            "terminal clipboard payload is intentionally ignored",
            &test_tx(),
        );

        let expected = destination_dir.join("track.flac");
        for _ in 0..200 {
            if expected.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            expected.exists(),
            "intercepted Ctrl+V must use current_dir while Files owns focus"
        );
    }

    #[tokio::test]
    async fn picker_tree_bracketed_paste_uses_the_selected_tree_destination() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let current_dir = temp.path().join("current");
        let tree_target = temp.path().join("tree-target");
        std::fs::create_dir(&source_dir).expect("source dir");
        std::fs::create_dir(&current_dir).expect("current dir");
        std::fs::create_dir(&tree_target).expect("tree target");
        let source = source_dir.join("track.flac");
        std::fs::write(&source, b"audio").expect("source");

        let mut picker = tui_file_picker::FilePickerState::new(
            tui_file_picker::FilePickerConfig {
                start_dir: source_dir,
                show_hidden: true,
                ..tui_file_picker::FilePickerConfig::default()
            },
        );
        let source_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == source)
            .expect("source visible");
        picker.set_file_cursor(source_index, 4);
        picker.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        assert!(picker.navigate_to_dir(current_dir.clone()));
        select_picker_tree_path(&mut picker, &tree_target);

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::FilePicker(MetadataFilePickerState::new(
            FilePickerPurpose::SelectFile,
            picker,
        ));

        handle_paste(
            &mut app,
            "terminal clipboard payload is intentionally ignored",
            &test_tx(),
        );

        let expected = tree_target.join("track.flac");
        for _ in 0..200 {
            if expected.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(expected.exists(), "intercepted Ctrl+V must use Tree target");
        assert!(
            !current_dir.join("track.flac").exists(),
            "intercepted Ctrl+V must not paste into current_dir while Tree owns focus"
        );
    }

    #[test]
    fn picker_tree_bracketed_paste_with_empty_clipboard_reports_guidance() {
        let temp = tempfile::tempdir().expect("tempdir");
        let tree_target = temp.path().join("tree-target");
        std::fs::create_dir(&tree_target).expect("tree target");
        let mut picker = tui_file_picker::FilePickerState::new(
            tui_file_picker::FilePickerConfig {
                start_dir: temp.path().to_path_buf(),
                show_hidden: true,
                ..tui_file_picker::FilePickerConfig::default()
            },
        );
        select_picker_tree_path(&mut picker, &tree_target);
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::FilePicker(MetadataFilePickerState::new(
            FilePickerPurpose::SelectFile,
            picker,
        ));

        handle_paste(&mut app, "ignored terminal payload", &test_tx());

        assert!(app.status_message.as_ref().is_some_and(|(message, _)| {
            message.contains("filesystem clipboard is empty")
                && message.contains("Ctrl+C/Ctrl+X")
        }));
    }

    #[test]
    fn picker_tree_bracketed_paste_respects_disabled_paste_policy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let tree_target = temp.path().join("tree-target");
        std::fs::create_dir(&source_dir).expect("source dir");
        std::fs::create_dir(&tree_target).expect("tree target");
        let source = source_dir.join("track.flac");
        std::fs::write(&source, b"audio").expect("source");

        let mut picker = tui_file_picker::FilePickerState::new(
            tui_file_picker::FilePickerConfig {
                start_dir: source_dir,
                show_hidden: true,
                ..tui_file_picker::FilePickerConfig::default()
            },
        );
        let source_index = picker
            .entries()
            .iter()
            .position(|entry| entry.path == source)
            .expect("source visible");
        picker.set_file_cursor(source_index, 4);
        picker.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        select_picker_tree_path(&mut picker, &tree_target);
        let mut policy = picker.file_operation_policy();
        policy.allow_paste = false;
        picker.set_file_operation_policy(policy);

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::FilePicker(MetadataFilePickerState::new(
            FilePickerPurpose::SelectFile,
            picker,
        ));
        handle_paste(&mut app, "ignored terminal payload", &test_tx());

        let ActiveOverlay::FilePicker(session) = &app.active_overlay else {
            panic!("picker overlay must remain open");
        };
        assert!(matches!(
            session.picker.last_error(),
            Some(tui_file_picker::FilePickerError::OperationDisabled("paste"))
        ));
        assert!(!tree_target.join("track.flac").exists());
    }

    #[test]
    fn bracketed_paste_outside_browse_never_starts_a_stale_filesystem_paste() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("track.flac");
        std::fs::write(&source, b"audio").expect("source");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = super::super::app::AppScreen::Convert;
        app.browse.filesystem_clipboard = tui_file_picker::FilesystemClipboard::new(
            tui_file_picker::FilePickerClipboardMode::Copy,
            vec![source],
        );

        handle_paste(&mut app, "terminal text", &test_tx());

        assert!(app.file_transfers.pending_by_session.is_empty());
        assert!(app.status_message.as_ref().is_some_and(|(message, _)| {
            message.contains("return to Browse")
        }));
    }

    #[test]
    fn bracketed_paste_without_text_focus_explains_ctrl_v_file_paste_split() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = super::super::app::AppScreen::Browse;

        handle_paste(&mut app, "text", &test_tx());

        assert!(app.status_message.as_ref().is_some_and(|(message, _)| {
            message.contains("filesystem clipboard is empty") && message.contains("Ctrl+P")
        }));
    }
}

#[cfg(test)]
mod metadata_detail_paste_tests {
    use super::*;
    use crate::config::TonepoetConfig;
    use crate::tui::app::{MetadataEditorPhase, MetadataEditorState, MetadataTechnicalDetails};
    use crate::tui::probe::{RowScope, TagEntry};
    use lofty::tag::ItemKey;

    fn editing_entry(key: &str, item_key: ItemKey, values: &[&str]) -> TagEntry {
        TagEntry {
            row_scope: RowScope::File,
            display_key: key.to_string(),
            item_key,
            value: values.first().copied().unwrap_or_default().to_string(),
            original: values.first().copied().unwrap_or_default().to_string(),
            is_binary: false,
            is_mixed: values.windows(2).any(|pair| pair[0] != pair[1]),
            has_multiple_stored_values: false,
            per_file_stored_value_counts: vec![1; values.len()],
            per_file_values: crate::tui::probe::metadata_field_values_from_scalars(values.iter().map(|value| (*value).to_string()).collect()),
            per_file_originals: crate::tui::probe::metadata_field_values_from_scalars(values.iter().map(|value| (*value).to_string()).collect()),
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        }
    }

    fn set_valued_entry(key: &str, item_key: ItemKey, slots: &[&[&str]]) -> TagEntry {
        let per_file_values = slots
            .iter()
            .map(|values| {
                crate::tui::probe::MetadataFieldValues::from_stored_texts(
                    values.iter().copied(),
                )
            })
            .collect::<Vec<_>>();
        let is_mixed = per_file_values
            .windows(2)
            .any(|pair| pair[0] != pair[1]);
        let display = if is_mixed {
            "<multiple values>".to_string()
        } else {
            per_file_values
                .first()
                .map(|values| values.as_str().to_string())
                .unwrap_or_default()
        };
        TagEntry {
            row_scope: RowScope::File,
            display_key: key.to_string(),
            item_key,
            value: display.clone(),
            original: display,
            is_binary: false,
            is_mixed,
            has_multiple_stored_values: per_file_values
                .iter()
                .any(|values| values.value_count() > 1),
            per_file_stored_value_counts: per_file_values
                .iter()
                .map(|values| values.value_count())
                .collect(),
            per_file_originals: per_file_values.clone(),
            per_file_values,
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        }
    }

    #[test]
    fn editing_phase_bracketed_paste_uses_block_then_row_classification_and_reports_errors() {
        let mut state = MetadataEditorState::for_files(
            vec!["/tmp/a.flac".into(), "/tmp/b.flac".into()],
            vec![editing_entry("TITLE", ItemKey::TrackTitle, &["Old A", "Old B"])],
            vec!["a".to_string(), "b".to_string()],
            MetadataTechnicalDetails::default(),
        );
        state.phase = MetadataEditorPhase::Editing;
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::MetadataEditor(Box::new(state));
        let (tx, _rx) = mpsc::channel(8);

        handle_paste(&mut app, "TITLE\nShared", &tx);
        let ActiveOverlay::MetadataEditor(state) = &app.active_overlay else {
            panic!("editor remains open after block paste");
        };
        assert_eq!(
            state.active_surface().entries[0].per_file_values,
            ["Shared", "Shared"]
        );
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.as_str()),
            Some("applied TITLE (broadcast to 2 files) — review before save")
        );

        handle_paste(&mut app, "One\nTwo", &tx);
        let ActiveOverlay::MetadataEditor(state) = &app.active_overlay else {
            panic!("editor remains open after row paste");
        };
        assert_eq!(state.active_surface().entries[0].per_file_values, ["One", "Two"]);

        let before = state.active_surface().entries[0].per_file_values.clone();
        handle_paste(&mut app, "TITLE\nOne\nTwo\nThree", &tx);
        let ActiveOverlay::MetadataEditor(state) = &app.active_overlay else {
            panic!("editor remains open after rejected block paste");
        };
        assert_eq!(state.active_surface().entries[0].per_file_values, before);
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.as_str()),
            Some("tag blocks: TITLE has 3 values for 2 files")
        );
    }

    #[test]
    fn inline_comment_terminal_paste_is_one_undo_even_beyond_history_cap() {
        let mut state = MetadataEditorState::for_files(
            vec!["/tmp/a.flac".into()],
            vec![editing_entry("COMMENT", ItemKey::Comment, &["before"])],
            vec!["a".to_string()],
            MetadataTechnicalDetails::default(),
        );
        state.phase = MetadataEditorPhase::InlineEdit;
        state.edit_input = Some(super::super::text_input::TextInputState::new(
            "before".to_string(),
        ));

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::MetadataEditor(Box::new(state));
        let (tx, _rx) = mpsc::channel(8);
        let pasted = "x".repeat(300);
        let terminal_payload = format!("{pasted}\nignored second line");

        handle_paste(&mut app, &terminal_payload, &tx);

        let ActiveOverlay::MetadataEditor(state) = &mut app.active_overlay else {
            panic!("metadata editor should remain open after terminal paste");
        };
        let input = state.edit_input.as_mut().expect("inline COMMENT editor");
        assert_eq!(input.text, format!("before{pasted}"));
        assert_eq!(input.cursor, "before".len() + pasted.len());
        assert!(input.undo(), "one undo must exist for the whole terminal paste");
        assert_eq!(input.text, "before");
        assert_eq!(input.cursor, "before".len());
        assert!(
            !input.can_undo(),
            "the 300-character paste must consume exactly one undo snapshot"
        );
    }

    #[test]
    fn inline_comment_empty_first_line_terminal_paste_preserves_selected_input() {
        let mut state = MetadataEditorState::for_files(
            vec!["/tmp/a.flac".into()],
            vec![editing_entry("COMMENT", ItemKey::Comment, &["before"])],
            vec!["a".to_string()],
            MetadataTechnicalDetails::default(),
        );
        state.phase = MetadataEditorPhase::InlineEdit;
        state.edit_input = Some(super::super::text_input::TextInputState::new_selected(
            "before".to_string(),
        ));

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::MetadataEditor(Box::new(state));
        let (tx, _rx) = mpsc::channel(8);

        let ActiveOverlay::MetadataEditor(state) = &app.active_overlay else {
            panic!("metadata editor should be open");
        };
        let input = state.edit_input.as_ref().expect("inline COMMENT editor");
        let before_text = input.text.clone();
        let before_cursor = input.cursor;
        let before_selection = input.selection_range();
        assert_eq!(before_selection, Some(0.."before".len()));
        assert!(!input.can_undo());

        handle_paste(&mut app, "\nignored", &tx);

        let ActiveOverlay::MetadataEditor(state) = &mut app.active_overlay else {
            panic!("metadata editor should remain open after terminal paste");
        };
        let input = state.edit_input.as_mut().expect("inline COMMENT editor");
        assert_eq!(input.text, before_text);
        assert_eq!(input.cursor, before_cursor);
        assert_eq!(input.selection_range(), before_selection);
        assert!(
            !input.can_undo(),
            "an empty retained first line must not create an undo snapshot"
        );
    }

    #[test]
    fn detail_bracketed_paste_without_row_editor_uses_list_aware_whole_field_path() {
        let entry = set_valued_entry(
            "PERFORMER",
            ItemKey::Performer,
            &[&["Old A"], &["Old B"]],
        );
        let mut state = MetadataEditorState::for_files(
            vec!["/tmp/a.flac".into(), "/tmp/b.flac".into()],
            vec![entry],
            vec!["a".to_string(), "b".to_string()],
            MetadataTechnicalDetails::default(),
        );
        state.phase = MetadataEditorPhase::DetailEdit;
        state.detail_field_idx = 0;

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::MetadataEditor(Box::new(state));
        let (tx, _rx) = mpsc::channel(8);
        handle_paste(&mut app, "A; B\nC; D", &tx);

        let ActiveOverlay::MetadataEditor(state) = &app.active_overlay else {
            panic!("metadata editor should remain open");
        };
        assert_eq!(
            state.active_surface().entries[0].per_file_values[0].to_texts(),
            ["A", "B"]
        );
        assert_eq!(
            state.active_surface().entries[0].per_file_values[1].to_texts(),
            ["C", "D"]
        );
        assert!(super::super::keybindings::metadata_editor_has_detail_paste_snapshot(
            state, 0
        ));
    }

    #[test]
    fn detail_bracketed_paste_with_row_editor_stays_in_focused_text_input() {
        let entry = set_valued_entry(
            "PERFORMER",
            ItemKey::Performer,
            &[&["A", "B"], &["C", "D"]],
        );
        let mut state = MetadataEditorState::for_files(
            vec!["/tmp/a.flac".into(), "/tmp/b.flac".into()],
            vec![entry],
            vec!["a".to_string(), "b".to_string()],
            MetadataTechnicalDetails::default(),
        );
        state.phase = MetadataEditorPhase::DetailEdit;
        state.detail_field_idx = 0;
        state.detail_cursor = 1;

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::MetadataEditor(Box::new(state));
        let (tx, _rx) = mpsc::channel(8);

        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ),
            &tx,
        );
        let ActiveOverlay::MetadataEditor(state) = &app.active_overlay else {
            panic!("metadata editor should remain open");
        };
        assert_eq!(state.detail_cursor, 1);
        assert_eq!(
            state.detail_edit.as_ref().map(|input| input.text.as_str()),
            Some("C; D")
        );

        handle_paste(&mut app, "X; Y", &tx);
        let ActiveOverlay::MetadataEditor(state) = &mut app.active_overlay else {
            panic!("metadata editor should remain open after terminal paste");
        };
        assert_eq!(state.detail_cursor, 1);
        assert_eq!(
            state.detail_edit.as_ref().map(|input| input.text.as_str()),
            Some("X; Y")
        );
        assert_eq!(
            state.active_surface().entries[0].per_file_values[0].to_texts(),
            ["A", "B"]
        );
        assert_eq!(
            state.active_surface().entries[0].per_file_values[1].to_texts(),
            ["C", "D"]
        );
        let input = state.detail_edit.as_mut().expect("focused detail input");
        assert!(input.undo(), "terminal paste into a detail row must be one undo unit");
        assert_eq!(input.text, "C; D");

        handle_paste(&mut app, "X; Y", &tx);

        handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ),
            &tx,
        );
        let ActiveOverlay::MetadataEditor(state) = &app.active_overlay else {
            panic!("metadata editor should remain open after commit");
        };
        assert!(state.detail_edit.is_none());
        assert_eq!(
            state.active_surface().entries[0].per_file_values[0].to_texts(),
            ["A", "B"]
        );
        assert_eq!(
            state.active_surface().entries[0].per_file_values[1].to_texts(),
            ["X", "Y"]
        );
    }
}

/// Handle the result of a `:cue-mb` MusicBrainz lookup. Caches the response,
/// builds a CUE from MB-overridden tag/probe data, and writes it to disk.
fn handle_cue_mb_complete(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    operation_id: super::message::TagsMbOperationId,
    outcome: Result<super::musicbrainz::MbLookupOutcome, String>,
    paths: Vec<std::path::PathBuf>,
    output_dir: std::path::PathBuf,
    single_image: bool,
    toc_string: String,
) {
    if !cue_operation_is_current(app, operation_id) {
        return;
    }
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            finish_cue_operation_if_current(app, operation_id);
            app.set_status(format!("MusicBrainz CUE lookup failed: {error}"));
            return;
        }
    };

    // Cache only while this operation is still authoritative. Stale work is a
    // total no-op, including cache mutation, so a superseded request cannot
    // publish data under a newer workflow's lifecycle.
    if let Some(json) = outcome.cache_response.as_deref() {
        if let Err(error) = app.db.store_mb_response(&toc_string, json) {
            log::warn!("MB cache store failed: {error}");
        }
    }

    let release = match outcome.releases.into_iter().next() {
        Some(release) => release,
        None => {
            finish_cue_operation_if_current(app, operation_id);
            app.set_status("MusicBrainz CUE: no release matched this disc TOC".to_string());
            return;
        }
    };

    app.set_status("MusicBrainz CUE: building preview…".to_string());
    let tx = tx.clone();
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            let (mut album, mut tracks) =
                super::cue_generate::gather_cue_info_blocking(&paths, &output_dir)
                    .map_err(|error| error.to_string())?;

            super::cue_generate::apply_mb_overrides(&mut album, &mut tracks, &release);

            let cue_content = if single_image {
                let image_name = super::cue_generate::derive_image_filename(&album, &paths[0]);
                let ext = paths[0]
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .unwrap_or("flac");
                let format_tag = super::cue_generate::cue_format_tag(ext);
                super::cue_generate::generate_single_image_cue(
                    &album,
                    &tracks,
                    &image_name,
                    format_tag,
                )
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
            let pregaps = tracks
                .iter()
                .filter(|track| track.pregap_frames.is_some())
                .count();
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
        .unwrap_or_else(|error| Err(format!("preview task failed: {error}")));

        let _ = tx
            .send(AppMessage::CuePreviewComplete {
                operation_id,
                result,
            })
            .await;
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
pub(super) fn dispatch_tags_from_mb_complete(
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
        MbOutcome::Toc { outcome } => {
            if !tags_mb_operation_is_current_phase(
                app,
                ctx.operation_id,
                super::app::TagsMbOperationPhase::Lookup,
            ) {
                log::debug!(
                    "discarded stale MusicBrainz TOC completion {:?}",
                    ctx.operation_id
                );
                return;
            }
            handle_mb_toc_outcome(app, tx, outcome, ctx);
        }
        MbOutcome::Search {
            outcome,
            query_label,
        } => {
            if !transition_tags_mb_operation_phase(
                app,
                ctx.operation_id,
                super::app::TagsMbOperationPhase::LookupTextFallback,
                super::app::TagsMbOperationPhase::Lookup,
            ) {
                log::debug!(
                    "discarded stale or duplicate MusicBrainz text-search completion {:?}",
                    ctx.operation_id
                );
                return;
            }
            handle_mb_search_outcome(app, tx, outcome, query_label, ctx);
        }
    }
}

fn handle_mb_toc_outcome(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    outcome: Result<super::musicbrainz::MbCascadeOutcome, String>,
    ctx: super::message::TagsMbContext,
) {
    let outcome = match outcome {
        Ok(o) => o,
        Err(e) => {
            finish_tags_mb_operation_if_current(app, ctx.operation_id);
            app.set_status(format!(":tags-mb: TOC lookup failed: {}", e));
            return;
        }
    };

    for (toc_string, json) in &outcome.cache_writes {
        if let Err(e) = app.db.store_mb_response(toc_string, json) {
            log::warn!("MB TOC cache store failed: {}", e);
        }
    }

    // When a stub-drop stage matched, tell the user which source tracks were
    // excluded (1-based, matching the editor's track numbering). The matched
    // releases are already aligned back onto the full source track list, so
    // the excluded rows simply receive no MB proposals.
    let stub_note = (!outcome.dropped_source_indices.is_empty()).then(|| {
        let ordinals: Vec<String> = outcome
            .dropped_source_indices
            .iter()
            .map(|i| format!("#{}", i + 1))
            .collect();
        format!(
            "matched after excluding sub-4s stub track{} {}",
            if ordinals.len() == 1 { "" } else { "s" },
            ordinals.join(", "),
        )
    });

    match outcome.releases.len() {
        0 => match ctx.fallback_seed.clone() {
            Some(seed) => {
                if transition_tags_mb_operation_phase(
                    app,
                    ctx.operation_id,
                    super::app::TagsMbOperationPhase::Lookup,
                    super::app::TagsMbOperationPhase::LookupTextFallback,
                ) {
                    spawn_tags_mb_text_search(app, tx, seed, ctx, TextSearchMode::TocFallback)
                }
            }
            None => {
                finish_tags_mb_operation_if_current(app, ctx.operation_id);
                app.set_status(
                    ":tags-mb: no MusicBrainz release matched this disc TOC".to_string(),
                );
            }
        },
        1 => {
            open_editor_with_mb_release_for_operation(
                app,
                tx,
                ctx.operation_id,
                outcome.releases,
                0,
                ctx.paths,
                ctx.editor_session,
            );
            if let Some(note) = stub_note {
                app.set_status(format!(":tags-mb: {}", note));
            }
        }
        n => {
            open_mb_select_picker(app, tx, outcome.releases, ctx, n, None);
            if let Some(note) = stub_note {
                app.set_status(format!(":tags-mb: {}", note));
            }
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
            finish_tags_mb_operation_if_current(app, ctx.operation_id);
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
            finish_tags_mb_operation_if_current(app, ctx.operation_id);
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
            open_editor_with_mb_release_for_operation(
                app,
                tx,
                ctx.operation_id,
                outcome.releases,
                0,
                ctx.paths,
                ctx.editor_session,
            );
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
    mut ctx: super::message::TagsMbContext,
    mode: TextSearchMode,
) {
    if ctx.operation_id.is_assigned() {
        if tags_mb_operation_is_current_phase(
            app,
            ctx.operation_id,
            super::app::TagsMbOperationPhase::Lookup,
        ) {
            if !transition_tags_mb_operation_phase(
                app,
                ctx.operation_id,
                super::app::TagsMbOperationPhase::Lookup,
                super::app::TagsMbOperationPhase::LookupTextFallback,
            ) {
                return;
            }
        } else if !tags_mb_operation_is_current_phase(
            app,
            ctx.operation_id,
            super::app::TagsMbOperationPhase::LookupTextFallback,
        ) {
            return;
        }
    } else {
        let operation_id = match begin_tags_mb_lookup_operation(app, ctx.editor_park) {
            Ok(operation_id) => operation_id,
            Err(error) => {
                app.set_status(error);
                return;
            }
        };
        ctx.operation_id = operation_id;
        if !transition_tags_mb_operation_phase(
            app,
            ctx.operation_id,
            super::app::TagsMbOperationPhase::Lookup,
            super::app::TagsMbOperationPhase::LookupTextFallback,
        ) {
            return;
        }
    }
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
        let worker = tokio::spawn(async move {
            super::musicbrainz::search_releases_by_query(
                &artist,
                &album,
                catalog.as_deref(),
                year.as_deref(),
                n_tracks,
                cached,
            )
            .await
        });
        let outcome = worker.await.unwrap_or_else(|err| {
            Err(format!("MusicBrainz text-search worker failed: {err}"))
        });
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
    if !tags_mb_operation_is_current_phase(
        app,
        ctx.operation_id,
        super::app::TagsMbOperationPhase::Lookup,
    ) {
        return;
    }
    if !ctx.editor_park {
        let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
        match overlay {
            ActiveOverlay::MetadataEditor(state) => {
                if app.pending_metadata_editor.is_some() {
                    app.active_overlay = ActiveOverlay::MetadataEditor(state);
                    finish_tags_mb_operation_if_current(app, ctx.operation_id);
                    app.set_status(
                        ":tags-mb: another metadata editor is already parked; rerun the lookup"
                            .to_string(),
                    );
                    return;
                }
                app.pending_metadata_editor = Some(state);
            }
            ActiveOverlay::None => {}
            other => {
                app.active_overlay = other;
                finish_tags_mb_operation_if_current(app, ctx.operation_id);
                app.set_status(
                    ":tags-mb: lookup completed while another modal was active; rerun after closing it"
                        .to_string(),
                );
                return;
            }
        }
    }
    if ctx.editor_park {
        let Some(taken) = take_metadata_editor_with_restore_slot(app) else {
            let detail = match &query_label {
                Some(l) => format!("rerun to apply \"{}\" ({} matches)", l, n),
                None => format!("rerun ({} matches)", n),
            };
            finish_tags_mb_operation_if_current(app, ctx.operation_id);
            app.set_status(format!(":tags-mb: editor closed during lookup; {}", detail));
            return;
        };
        if !metadata_editor_matches_tags_mb_context(
            &taken.state,
            &ctx.paths,
            ctx.editor_session,
        ) {
            restore_taken_metadata_editor(app, taken);
            finish_tags_mb_operation_if_current(app, ctx.operation_id);
            app.set_status(
                ":tags-mb: metadata editor changed since lookup; rerun".to_string(),
            );
            return;
        }
        app.pending_metadata_editor = Some(taken.state);
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
    let state = super::app::MbSelectState::new_with_editor_session(
        releases,
        ctx.paths,
        ctx.editor_session,
    )
    .with_operation_id(ctx.operation_id);
    if let Some(top) = state.releases.first() {
        let cached_body = app
            .db
            .get_cached_mb_search(&super::musicbrainz::detail_cache_key(&top.release_id));
        spawn_mb_detail_prefetch(
            tx.clone(),
            ctx.operation_id,
            top.release_id.clone(),
            state.paths.len(),
            std::sync::Arc::clone(&state.generation),
            state.generation.load(std::sync::atomic::Ordering::Relaxed),
            cached_body,
        );
    }
    if let Some(active) = app.active_tags_mb_operation.as_mut() {
        if active.operation_id == ctx.operation_id {
            active.picker_owned = true;
            active.phase = super::app::TagsMbOperationPhase::Selecting;
        }
    }
    app.active_overlay = ActiveOverlay::MbSelect(Box::new(state));
}

/// Pop a parked metadata editor back into `active_overlay`. No-op
/// when no editor was parked. Used by every `MbSelect` cancel path
/// (Esc, click-outside, cancel pill, context-menu cancel) so the
/// user lands back on the editor they came from instead of a blank
/// screen.
pub(super) fn set_metadata_editor_tags_mb_in_flight(app: &mut AppState, in_flight: bool) -> bool {
    if let Some(state) = app.pending_metadata_editor.as_mut() {
        state.model.tags_mb_in_flight = in_flight;
        return true;
    }
    if let ActiveOverlay::MetadataEditor(state) = &mut app.active_overlay {
        state.model.tags_mb_in_flight = in_flight;
        return true;
    }
    false
}

/// Unconditionally terminate the active MusicBrainz workflow. Reserved for
/// explicit user cancellation or editor closure, where the current operation
/// itself is being abandoned. Async completion handlers must use the scoped
/// variant below so stale work cannot release a newer operation's latch.
pub(super) fn finish_metadata_editor_tags_mb_operation(app: &mut AppState) {
    app.active_tags_mb_operation = None;
    set_metadata_editor_tags_mb_in_flight(app, false);
}

pub(super) fn finish_tags_mb_operation_if_current(
    app: &mut AppState,
    operation_id: super::message::TagsMbOperationId,
) -> bool {
    if !tags_mb_operation_is_current(app, operation_id) {
        return false;
    }
    app.active_tags_mb_operation = None;
    set_metadata_editor_tags_mb_in_flight(app, false);
    true
}

fn allocate_tags_mb_operation(
    app: &mut AppState,
    picker_owned: bool,
    phase: super::app::TagsMbOperationPhase,
) -> Result<super::message::TagsMbOperationId, String> {
    let generation = app
        .tags_mb_operation_generation
        .checked_add(1)
        .ok_or_else(|| {
            ":tags-mb: operation identity space exhausted; restart Tonepoet before retrying"
                .to_string()
        })?;
    app.tags_mb_operation_generation = generation;
    let operation_id = super::message::TagsMbOperationId(generation);
    app.active_tags_mb_operation = Some(super::app::ActiveTagsMbOperation {
        operation_id,
        picker_owned,
        phase,
    });
    Ok(operation_id)
}

/// Begin a complete lookup-to-apply workflow. A new user dispatch supersedes
/// older work and releases only the latch owned by that older workflow before
/// installing the new authority.
pub(super) fn begin_tags_mb_lookup_operation(
    app: &mut AppState,
    editor_owned: bool,
) -> Result<super::message::TagsMbOperationId, String> {
    // MusicBrainz and GNUDB share the metadata-editor authority domain.
    // Refuse rather than silently retiring GNUDB: the user can cancel the
    // exact workflow explicitly and any parked editor remains recoverable.
    if app.active_gnudb_operation.is_some() {
        return Err(
            ":tags-mb: a GNUDB workflow is active; run :gnudb-cancel before starting MusicBrainz"
                .to_string(),
        );
    }
    if app.active_cue_operation.is_some() {
        return Err(
            ":tags-mb: a CUE workflow is active; finish it before starting MusicBrainz"
                .to_string(),
        );
    }
    if let Some(active) = app.active_tags_mb_operation {
        if active.picker_owned {
            let active_picker_matches = matches!(
                &app.active_overlay,
                ActiveOverlay::MbSelect(state) if state.operation_id == active.operation_id
            );
            let parked_picker_matches = app
                .pending_mb_select
                .as_ref()
                .is_some_and(|state| state.operation_id == active.operation_id);
            if active_picker_matches {
                app.active_overlay = ActiveOverlay::None;
            }
            if parked_picker_matches {
                app.pending_mb_select = None;
            }
            if active_picker_matches || parked_picker_matches {
                cancel_mb_select_operation(app, active.operation_id);
            } else {
                // The operation has already lost its picker to some unrelated
                // modal. Retire only its authority/latch; do not overwrite the
                // current overlay by restoring a parked editor into this slot.
                finish_tags_mb_operation_if_current(app, active.operation_id);
            }
        } else {
            finish_tags_mb_operation_if_current(app, active.operation_id);
        }
    }
    let operation_id = allocate_tags_mb_operation(
        app,
        false,
        super::app::TagsMbOperationPhase::Lookup,
    )?;
    if editor_owned {
        set_metadata_editor_tags_mb_in_flight(app, true);
    }
    Ok(operation_id)
}

pub(super) fn begin_tags_mb_prelookup_operation(
    app: &mut AppState,
    editor_owned: bool,
    phase: super::app::TagsMbOperationPhase,
) -> Result<super::message::TagsMbOperationId, String> {
    debug_assert!(matches!(
        phase,
        super::app::TagsMbOperationPhase::Discovery
            | super::app::TagsMbOperationPhase::Grouping
    ));
    let operation_id = begin_tags_mb_lookup_operation(app, editor_owned)?;
    if let Some(active) = app.active_tags_mb_operation.as_mut() {
        if active.operation_id == operation_id {
            active.phase = phase;
        }
    }
    Ok(operation_id)
}

pub(super) fn tags_mb_operation_is_current_phase(
    app: &AppState,
    operation_id: super::message::TagsMbOperationId,
    phase: super::app::TagsMbOperationPhase,
) -> bool {
    operation_id.is_assigned()
        && app.active_tags_mb_operation.is_some_and(|active| {
            active.operation_id == operation_id && active.phase == phase
        })
}

pub(super) fn transition_tags_mb_operation_phase(
    app: &mut AppState,
    operation_id: super::message::TagsMbOperationId,
    expected: super::app::TagsMbOperationPhase,
    next: super::app::TagsMbOperationPhase,
) -> bool {
    let Some(active) = app.active_tags_mb_operation.as_mut() else {
        return false;
    };
    if active.operation_id != operation_id || active.phase != expected {
        return false;
    }
    active.phase = next;
    true
}

fn metadata_editor_session_guard(
    state: &super::app::MetadataEditorState,
) -> super::message::MetadataEditorSessionGuard {
    let details = &state.active_surface().technical_details;
    super::message::MetadataEditorSessionGuard {
        session_id: details.session_id,
        save_generation: details.save_generation,
        editor_generation: state.model.editor_save_generation,
    }
}

pub(super) fn begin_completion_operation(
    app: &mut AppState,
    kind: super::app::CompletionOperationKind,
    label: &str,
) -> Result<super::message::TagsMbOperationId, String> {
    if app.active_completion_operations.contains_key(&kind) {
        return Err(format!(
            "{label}: another operation in this completion family is still active"
        ));
    }
    let generation = app
        .tags_mb_operation_generation
        .checked_add(1)
        .ok_or_else(|| {
            format!(
                "{label}: operation identity space exhausted; restart Tonepoet before retrying"
            )
        })?;
    app.tags_mb_operation_generation = generation;
    let operation_id = super::message::TagsMbOperationId(generation);
    let editor_session = app
        .pending_metadata_editor
        .as_deref()
        .or_else(|| match &app.active_overlay {
            ActiveOverlay::MetadataEditor(state) => Some(state.as_ref()),
            _ => None,
        })
        .map(metadata_editor_session_guard);
    app.active_completion_operations.insert(
        kind,
        super::app::ActiveCompletionOperation {
            operation_id,
            editor_session,
            batch: None,
        },
    );
    Ok(operation_id)
}

pub(super) fn begin_counted_completion_operation(
    app: &mut AppState,
    kind: super::app::CompletionOperationKind,
    label: &str,
    total: usize,
) -> Result<super::message::TagsMbOperationId, String> {
    if total == 0 {
        return Err(format!("{label}: no work items were supplied"));
    }
    // Re-invoking a counted command supersedes the in-flight batch instead of
    // refusing. A worker that dies without sending its terminal message would
    // otherwise leave `remaining` above zero forever, and the begin-time
    // refusal would brick the whole command family for the session. This is
    // safe because every terminal message carries the operation id: late
    // completions from the superseded batch are rejected as stale. Only a
    // counted entry may be displaced — an uncounted same-kind operation still
    // refuses through `begin_completion_operation` below.
    if app
        .active_completion_operations
        .get(&kind)
        .is_some_and(|active| active.batch.is_some())
    {
        app.active_completion_operations.remove(&kind);
    }
    let operation_id = begin_completion_operation(app, kind, label)?;
    if let Some(active) = app.active_completion_operations.get_mut(&kind) {
        active.batch = Some(super::app::CompletionBatchProgress {
            total,
            remaining: total,
        });
    }
    Ok(operation_id)
}

/// Accept one terminal worker completion for the matching counted operation.
/// Returns `Some(true)` exactly once, on the transition to zero remaining;
/// stale IDs, non-counted operations, and excess duplicate completions return
/// `None` without mutating state.
pub(super) fn complete_counted_completion_operation(
    app: &mut AppState,
    kind: super::app::CompletionOperationKind,
    operation_id: super::message::TagsMbOperationId,
) -> Option<bool> {
    let active = app.active_completion_operations.get_mut(&kind)?;
    if active.operation_id != operation_id {
        return None;
    }
    let progress = active.batch.as_mut()?;
    if progress.remaining == 0 {
        return None;
    }
    progress.remaining -= 1;
    Some(progress.remaining == 0)
}

pub(super) fn completion_operation_is_current(
    app: &AppState,
    kind: super::app::CompletionOperationKind,
    operation_id: super::message::TagsMbOperationId,
) -> bool {
    operation_id.is_assigned()
        && app
            .active_completion_operations
            .get(&kind)
            .is_some_and(|active| active.operation_id == operation_id)
}

pub(super) fn completion_operation_editor_session(
    app: &AppState,
    kind: super::app::CompletionOperationKind,
    operation_id: super::message::TagsMbOperationId,
) -> Option<super::message::MetadataEditorSessionGuard> {
    app.active_completion_operations
        .get(&kind)
        .filter(|active| active.operation_id == operation_id)
        .and_then(|active| active.editor_session)
}

pub(super) fn completion_operation_has_overlay_authority(
    app: &AppState,
    kind: super::app::CompletionOperationKind,
    operation_id: super::message::TagsMbOperationId,
) -> bool {
    let Some(active) = app.active_completion_operations.get(&kind) else {
        return false;
    };
    active.operation_id == operation_id
        && active.editor_session.is_none()
        && matches!(app.active_overlay, ActiveOverlay::None)
        && app.pending_metadata_editor.is_none()
        && app.pending_cue_preview.is_none()
        && app.pending_mb_select.is_none()
        && app.active_tags_mb_operation.is_none()
        && app.active_gnudb_operation.is_none()
        && app.active_cue_operation.is_none()
}

pub(super) fn retire_completion_operation(
    app: &mut AppState,
    kind: super::app::CompletionOperationKind,
    operation_id: super::message::TagsMbOperationId,
) -> bool {
    let Some(active) = app.active_completion_operations.get(&kind).copied() else {
        return false;
    };
    if active.operation_id != operation_id {
        return false;
    }
    app.active_completion_operations.remove(&kind);

    let Some(guard) = active.editor_session else {
        return true;
    };
    if app.active_tags_mb_operation.is_some()
        || app.active_gnudb_operation.is_some()
        || app.active_cue_operation.is_some()
        || app.pending_cue_preview.is_some()
        || app.pending_mb_select.is_some()
        || !matches!(app.active_overlay, ActiveOverlay::None)
    {
        return true;
    }
    let matches = app
        .pending_metadata_editor
        .as_deref()
        .is_some_and(|state| metadata_editor_matches_session_guard(state, guard));
    if matches {
        if let Some(editor) = app.pending_metadata_editor.take() {
            app.active_overlay = ActiveOverlay::MetadataEditor(editor);
        }
    }
    true
}

pub(super) fn begin_gnudb_operation(
    app: &mut AppState,
) -> Result<super::message::TagsMbOperationId, String> {
    if app.active_cue_operation.is_some() {
        return Err(
            "GNUDB: a CUE workflow is active; finish it before starting GNUDB".to_string(),
        );
    }
    if app.active_tags_mb_operation.is_some() {
        return Err(
            "GNUDB: a MusicBrainz workflow is active; cancel it before starting GNUDB"
                .to_string(),
        );
    }
    if app.active_gnudb_operation.is_some() {
        return Err(
            "GNUDB: another GNUDB workflow is active; run :gnudb-cancel before starting again"
                .to_string(),
        );
    }
    let generation = app
        .tags_mb_operation_generation
        .checked_add(1)
        .ok_or_else(|| {
            "GNUDB: operation identity space exhausted; restart Tonepoet before retrying"
                .to_string()
        })?;
    app.tags_mb_operation_generation = generation;
    let operation_id = super::message::TagsMbOperationId(generation);
    let editor_session = app
        .pending_metadata_editor
        .as_deref()
        .or_else(|| match &app.active_overlay {
            ActiveOverlay::MetadataEditor(state) => Some(state.as_ref()),
            _ => None,
        })
        .map(metadata_editor_session_guard);
    app.active_gnudb_operation = Some(super::app::ActiveGnudbOperation {
        operation_id,
        editor_session,
    });
    Ok(operation_id)
}

pub(super) fn gnudb_operation_is_current(
    app: &AppState,
    operation_id: super::message::TagsMbOperationId,
) -> bool {
    operation_id.is_assigned()
        && app
            .active_gnudb_operation
            .is_some_and(|active| active.operation_id == operation_id)
}

pub(super) fn gnudb_operation_has_overlay_authority(
    app: &AppState,
    operation_id: super::message::TagsMbOperationId,
) -> bool {
    if !gnudb_operation_is_current(app, operation_id)
        || app.active_tags_mb_operation.is_some()
        || !matches!(app.active_overlay, ActiveOverlay::None)
    {
        return false;
    }
    let Some(active) = app.active_gnudb_operation else {
        return false;
    };
    match active.editor_session {
        Some(guard) => app
            .pending_metadata_editor
            .as_deref()
            .is_some_and(|state| metadata_editor_matches_session_guard(state, guard)),
        None => app.pending_metadata_editor.is_none(),
    }
}

pub(super) fn finish_gnudb_operation_if_current(
    app: &mut AppState,
    operation_id: super::message::TagsMbOperationId,
) -> bool {
    if !gnudb_operation_is_current(app, operation_id) {
        return false;
    }
    app.active_gnudb_operation = None;
    true
}

pub(super) fn take_gnudb_review_editor_if_owned(
    app: &mut AppState,
    guard: Option<super::message::MetadataEditorSessionGuard>,
) -> Option<Box<super::app::MetadataEditorState>> {
    let guard = guard?;
    // A review reopened with `:gnudb-back` owns exactly the editor session
    // parked for that invocation.  Never consume a different editor merely
    // because it occupies the shared pending slot.
    if app.active_tags_mb_operation.is_some() || app.active_cue_operation.is_some() {
        return None;
    }
    let review_owns_guard = matches!(
        &app.active_overlay,
        ActiveOverlay::GnudbReview(review) if review.editor_session == Some(guard)
    );
    if !review_owns_guard {
        return None;
    }
    let matches = app
        .pending_metadata_editor
        .as_deref()
        .is_some_and(|state| metadata_editor_matches_session_guard(state, guard));
    if !matches {
        return None;
    }
    app.pending_metadata_editor.take()
}

pub(super) fn restore_gnudb_review_editor_if_owned(
    app: &mut AppState,
    guard: Option<super::message::MetadataEditorSessionGuard>,
) -> bool {
    let Some(guard) = guard else {
        return false;
    };
    // A different metadata workflow owns the parked editor while active. A
    // defensive GNUDB retirement must never surface or consume that editor.
    if app.active_tags_mb_operation.is_some()
        || app.active_cue_operation.is_some()
        || !matches!(app.active_overlay, ActiveOverlay::None)
    {
        return false;
    }
    let matches = app
        .pending_metadata_editor
        .as_deref()
        .is_some_and(|state| metadata_editor_matches_session_guard(state, guard));
    if !matches {
        return false;
    }
    if let Some(editor) = app.pending_metadata_editor.take() {
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);
        return true;
    }
    false
}

fn restore_gnudb_editor_if_owned(
    app: &mut AppState,
    active: super::app::ActiveGnudbOperation,
) -> bool {
    restore_gnudb_review_editor_if_owned(app, active.editor_session)
}

pub(super) fn cancel_gnudb_operation(
    app: &mut AppState,
    operation_id: super::message::TagsMbOperationId,
) -> bool {
    let Some(active) = app.active_gnudb_operation else {
        return false;
    };
    if active.operation_id != operation_id || !gnudb_operation_is_current(app, operation_id) {
        return false;
    }
    app.active_gnudb_operation = None;
    restore_gnudb_editor_if_owned(app, active);
    true
}

pub(super) fn retire_gnudb_operation_with_editor_restore(
    app: &mut AppState,
    operation_id: super::message::TagsMbOperationId,
) -> bool {
    let Some(active) = app.active_gnudb_operation else {
        return false;
    };
    if active.operation_id != operation_id || !gnudb_operation_is_current(app, operation_id) {
        return false;
    }
    app.active_gnudb_operation = None;
    restore_gnudb_editor_if_owned(app, active);
    true
}

pub(super) fn advance_gnudb_operation(
    app: &mut AppState,
    operation_id: super::message::TagsMbOperationId,
) -> Option<super::message::TagsMbOperationId> {
    if !gnudb_operation_is_current(app, operation_id) {
        return None;
    }
    let active = app.active_gnudb_operation?;
    let generation = app.tags_mb_operation_generation.checked_add(1)?;
    app.tags_mb_operation_generation = generation;
    let next = super::message::TagsMbOperationId(generation);
    app.active_gnudb_operation = Some(super::app::ActiveGnudbOperation {
        operation_id: next,
        editor_session: active.editor_session,
    });
    Some(next)
}

pub(super) fn spawn_gnudb_worker<F>(
    tx: mpsc::Sender<AppMessage>,
    operation_id: super::message::TagsMbOperationId,
    future: F,
) where
    F: std::future::Future<Output = AppMessage> + Send + 'static,
{
    tokio::spawn(async move {
        let message = match tokio::spawn(future).await {
            Ok(message) => message,
            Err(error) => AppMessage::GnudbWorkerFailed {
                operation_id,
                detail: if error.is_panic() {
                    "worker panicked".to_string()
                } else {
                    format!("worker was cancelled: {error}")
                },
            },
        };
        let _ = tx.send(message).await;
    });
}

pub(super) fn begin_cue_operation(
    app: &mut AppState,
    label: &str,
) -> Result<super::message::TagsMbOperationId, String> {
    if app.active_tags_mb_operation.is_some() || app.active_gnudb_operation.is_some() {
        return Err(format!(
            "{label}: a metadata lookup already owns the editor or overlay; finish or cancel it before retrying"
        ));
    }
    if app.active_cue_operation.is_some() {
        return Err(format!(
            "{label}: another CUE workflow is still active; cancel or finish it before retrying"
        ));
    }
    let generation = app
        .tags_mb_operation_generation
        .checked_add(1)
        .ok_or_else(|| format!(
            "{label}: operation identity space exhausted; restart Tonepoet before retrying"
        ))?;
    app.tags_mb_operation_generation = generation;
    let operation_id = super::message::TagsMbOperationId(generation);
    app.active_cue_operation = Some(super::app::ActiveCueOperation { operation_id });
    Ok(operation_id)
}

pub(super) fn cue_operation_is_current(
    app: &AppState,
    operation_id: super::message::TagsMbOperationId,
) -> bool {
    operation_id.is_assigned()
        && app
            .active_cue_operation
            .is_some_and(|active| active.operation_id == operation_id)
}

pub(super) fn cue_operation_has_overlay_authority(
    app: &AppState,
    operation_id: super::message::TagsMbOperationId,
) -> bool {
    cue_operation_is_current(app, operation_id)
        && matches!(app.active_overlay, ActiveOverlay::None)
        && app.pending_metadata_editor.is_none()
        && app.pending_cue_preview.is_none()
        && app.pending_mb_select.is_none()
}

pub(super) fn finish_cue_operation_if_current(
    app: &mut AppState,
    operation_id: super::message::TagsMbOperationId,
) -> bool {
    if !cue_operation_is_current(app, operation_id) {
        return false;
    }
    app.active_cue_operation = None;
    true
}

pub(super) fn begin_mb_select_operation(
    app: &mut AppState,
) -> Result<super::message::TagsMbOperationId, String> {
    if app.active_cue_operation.is_some() {
        return Err(":mb-back: a CUE workflow is active; finish it before returning".to_string());
    }
    if app.active_gnudb_operation.is_some() {
        return Err(
            ":mb-back: a GNUDB workflow is active; cancel it before returning"
                .to_string(),
        );
    }
    if app.active_tags_mb_operation.is_some() {
        return Err(
            ":mb-back: another MusicBrainz workflow is active; cancel it before returning"
                .to_string(),
        );
    }
    allocate_tags_mb_operation(
        app,
        true,
        super::app::TagsMbOperationPhase::Selecting,
    )
}

fn begin_tags_mb_apply_operation(
    app: &mut AppState,
    picker_owned: bool,
) -> Result<super::message::TagsMbOperationId, String> {
    if app.active_cue_operation.is_some() {
        return Err(
            ":tags-mb: a CUE workflow is active; finish it before selecting again".to_string(),
        );
    }
    if app.active_gnudb_operation.is_some() {
        return Err(
            ":tags-mb: a GNUDB workflow is active; cancel it before selecting again"
                .to_string(),
        );
    }
    if app.active_tags_mb_operation.is_some() {
        return Err(
            ":tags-mb: another MusicBrainz workflow is active; cancel it before selecting again"
                .to_string(),
        );
    }
    allocate_tags_mb_operation(
        app,
        picker_owned,
        super::app::TagsMbOperationPhase::Verifying,
    )
}

pub(super) fn tags_mb_operation_is_current(
    app: &AppState,
    operation_id: super::message::TagsMbOperationId,
) -> bool {
    operation_id.is_assigned()
        && app
            .active_tags_mb_operation
            .is_some_and(|active| active.operation_id == operation_id)
}

/// Cancel a picker only when it still owns the active workflow. A stale picker
/// cannot release a newer operation's latch or restore its parked editor.
pub(super) fn cancel_mb_select_operation(
    app: &mut AppState,
    operation_id: super::message::TagsMbOperationId,
) -> bool {
    let cancelled = if operation_id.is_assigned() {
        finish_tags_mb_operation_if_current(app, operation_id)
    } else if app.active_tags_mb_operation.is_none() {
        set_metadata_editor_tags_mb_in_flight(app, false);
        true
    } else {
        false
    };
    if cancelled {
        if let Some(parked) = app.pending_metadata_editor.take() {
            app.active_overlay = ActiveOverlay::MetadataEditor(parked);
        }
    }
    cancelled
}

/// Enforce the picker-owned workflow invariant after every input event and
/// async reducer step. If another transition replaces or removes its selecting
/// or verifying picker, the workflow is cancelled immediately rather than
/// waiting for a lookup/apply completion to discover the replacement. The
/// parked editor remains behind a replacement modal and is restored immediately
/// only when no overlay replaced the picker.
fn reconcile_tags_mb_apply_operation_state(app: &mut AppState) {
    let Some(active) = app.active_tags_mb_operation else {
        return;
    };
    if !active.picker_owned {
        return;
    }
    let active_picker_matches = matches!(
        &app.active_overlay,
        ActiveOverlay::MbSelect(state)
            if state.operation_id == active.operation_id
                && (state.is_selecting()
                    || state.phase.verifying_operation() == Some(active.operation_id))
    );
    let parked_picker_matches = app.pending_mb_select.as_ref().is_some_and(|state| {
        state.operation_id == active.operation_id
            && (state.is_selecting()
                || state.phase.verifying_operation() == Some(active.operation_id))
    });
    let picker_matches = active_picker_matches || parked_picker_matches;
    if picker_matches {
        return;
    }

    if matches!(app.active_overlay, ActiveOverlay::None) {
        cancel_mb_select_operation(app, active.operation_id);
    } else {
        finish_tags_mb_operation_if_current(app, active.operation_id);
    }
    log::debug!(
        "cancelled MusicBrainz workflow {:?}: owning picker was replaced",
        active.operation_id
    );
}

pub(super) fn restore_parked_editor_without_finishing(app: &mut AppState) {
    if let Some(parked) = app.pending_metadata_editor.take() {
        app.active_overlay = ActiveOverlay::MetadataEditor(parked);
    }
}

pub(super) fn restore_parked_editor(app: &mut AppState) {
    finish_metadata_editor_tags_mb_operation(app);
    restore_parked_editor_without_finishing(app);
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
#[derive(Clone, Copy)]
pub(super) enum MetadataEditorRestoreSlot {
    Active,
    Pending,
}

pub(super) struct TakenMetadataEditor {
    pub(super) state: Box<super::app::MetadataEditorState>,
    pub(super) slot: MetadataEditorRestoreSlot,
}

pub(super) fn take_metadata_editor_with_restore_slot(
    app: &mut AppState,
) -> Option<TakenMetadataEditor> {
    if let Some(parked) = app.pending_metadata_editor.take() {
        return Some(TakenMetadataEditor {
            state: parked,
            slot: MetadataEditorRestoreSlot::Pending,
        });
    }
    if matches!(app.active_overlay, ActiveOverlay::MetadataEditor(_)) {
        if let ActiveOverlay::MetadataEditor(s) =
            std::mem::replace(&mut app.active_overlay, ActiveOverlay::None)
        {
            return Some(TakenMetadataEditor {
                state: s,
                slot: MetadataEditorRestoreSlot::Active,
            });
        }
    }
    None
}

pub(super) fn restore_taken_metadata_editor(app: &mut AppState, taken: TakenMetadataEditor) {
    match taken.slot {
        MetadataEditorRestoreSlot::Active => {
            // If another overlay (for example a newer MbSelect picker) became
            // active while this async completion was in flight, do not clobber
            // it. Park the editor back in the pending slot so the active UI
            // survives and normal picker cancel/apply paths can restore it.
            if matches!(app.active_overlay, ActiveOverlay::None) {
                app.active_overlay = ActiveOverlay::MetadataEditor(taken.state);
            } else if app.pending_metadata_editor.is_none() {
                app.pending_metadata_editor = Some(taken.state);
            } else {
                // This should be unreachable in the current one-editor flow,
                // but prefer preserving the visible overlay over destroying a
                // newer modal. Leave the pre-existing parked editor untouched.
                app.set_status(
                    ":tags-mb: metadata editor changed since lookup; rerun".to_string(),
                );
            }
        }
        MetadataEditorRestoreSlot::Pending => {
            app.pending_metadata_editor = Some(taken.state);
        }
    }
}

fn metadata_editor_tags_mb_context_is_proper_track_prefix(
    state: &super::app::MetadataEditorState,
    paths: &[std::path::PathBuf],
) -> bool {
    let Some(sheet) = state.active_surface().cue_album_synthetic_sheet.as_ref() else {
        return false;
    };
    let track_paths: Vec<std::path::PathBuf> = sheet
        .track_sources
        .iter()
        .map(|source| source.audio_path.clone())
        .collect();
    !paths.is_empty()
        && paths.len() < track_paths.len()
        && track_paths.get(..paths.len()) == Some(paths)
}

fn metadata_editor_paths_match_tags_mb_context(
    state: &super::app::MetadataEditorState,
    paths: &[std::path::PathBuf],
) -> bool {
    if state.active_surface().paths == paths {
        return true;
    }

    // Unified synthetic split-CUE album lookups intentionally carry one audio
    // path per CUE track, because split-CUE MusicBrainz matching and apply are
    // positional by track.  The editor surface itself is file-dimensioned
    // (`[side_a, side_b]`), so comparing only `active_surface().paths` rejects
    // valid completions for two-image / N-track albums as stale.  Match the
    // completion against the row-dimension source vector that initiated the
    // lookup instead.
    if let Some(sheet) = state.active_surface().cue_album_synthetic_sheet.as_ref() {
        let track_paths: Vec<std::path::PathBuf> = sheet
            .track_sources
            .iter()
            .map(|source| source.audio_path.clone())
            .collect();
        if !track_paths.is_empty() && track_paths == paths {
            return true;
        }
        // A sub-group completion (grouping ladder split) is only safe when it
        // is the CONTIGUOUS ROW PREFIX of the unified projection: the MB
        // populate applies release tracks positionally from row 0, so a
        // non-prefix subset (e.g. side B's group) would overwrite the FIRST
        // group's rows with the second group's titles. Membership-only
        // matching also let a stale unguarded Browse lookup for one member
        // image apply into the wrong rows. Non-prefix groups are rejected
        // with the rerun status until group-offset-aware apply exists.
        if !paths.is_empty()
            && paths.len() < track_paths.len()
            && track_paths[..paths.len()] == paths[..]
        {
            return true;
        }

        // Defensive compatibility: a caller that still carries the unified
        // file-dimension vector should also match the same session.
        if sheet.audio_paths == paths {
            return true;
        }
    }

    if state.presentation_tabs.len() < 2 {
        return false;
    }
    let mut expanded = Vec::new();
    for tab in &state.presentation_tabs {
        let Some(path) = tab.paths.first() else {
            return false;
        };
        if tab.paths.len() != 1 {
            return false;
        }
        let count = tab.file_labels.len().max(1);
        for _ in 0..count {
            expanded.push(path.clone());
        }
    }
    expanded == paths
}


pub(super) fn metadata_editor_matches_session_guard(
    state: &super::app::MetadataEditorState,
    guard: super::message::MetadataEditorSessionGuard,
) -> bool {
    // Editor-level identity: match ANY surface's session, not just the active
    // tab (H9). Tabless editors (plain files, unified cue albums) keep their
    // only surface in `model.file_surface` — presentation_tabs is EMPTY there,
    // so the file surface must participate or every completion is rejected.
    std::iter::once(&state.model.file_surface)
        .chain(state.presentation_tabs.iter())
        .any(|tab| {
            tab.technical_details.session_id == guard.session_id
                && tab.technical_details.save_generation == guard.save_generation
                && state.model.editor_save_generation == guard.editor_generation
        })
}

fn metadata_editor_matches_tags_mb_context(
    state: &super::app::MetadataEditorState,
    paths: &[std::path::PathBuf],
    editor_session: Option<super::message::MetadataEditorSessionGuard>,
) -> bool {
    if let Some(guard) = editor_session {
        return metadata_editor_matches_session_guard(state, guard);
    }
    metadata_editor_paths_match_tags_mb_context(state, paths)
}

fn metadata_editor_can_transition_to_split_cue_target(
    state: &super::app::MetadataEditorState,
    paths: &[std::path::PathBuf],
) -> bool {
    // Legacy single-source-folder `:tags-mb` starts from an ordinary
    // one-audio-file metadata editor, discovers sibling side CUEs on a
    // blocking worker, and then intentionally transitions to the expanded
    // split-CUE target.  The source editor session is the stale-work guard;
    // the target path vector is allowed to differ from the source surface.
    if paths.is_empty() || paths.len() <= 1 {
        return false;
    }
    let surface = state.active_surface();
    state.presentation_tabs.is_empty()
        && surface.cue_album_synthetic_sheet.is_none()
        && surface.sacd_area_kind.is_none()
        && surface.dvdv_track_durations.is_none()
        && surface.bluray_chapter_durations.is_none()
        && surface.paths.len() == 1
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
/// path too: a "stale" cache hit can't waste an MB token. The
/// completion handler still applies the in-memory prefetch only when the
/// operation, picker, generation, and selected release remain current;
/// cache persistence is deliberately independent of picker ownership.
///
/// Pass `release_id` empty to skip — callers shouldn't generally do
/// this but the guard is cheap.
pub(super) fn spawn_mb_detail_prefetch(
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    operation_id: super::message::TagsMbOperationId,
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
            .send(crate::tui::message::AppMessage::MbDetailPrefetchComplete {
                operation_id,
                release_id,
                result,
            })
            .await;
    });
}

fn spawn_mb_selected_detail_resolution(
    tx: tokio::sync::mpsc::Sender<crate::tui::message::AppMessage>,
    operation_id: super::message::TagsMbOperationId,
    releases: Vec<super::musicbrainz::MbRelease>,
    selected: usize,
    paths: Vec<std::path::PathBuf>,
    editor_session: Option<super::message::MetadataEditorSessionGuard>,
    cached_body: Option<String>,
) {
    let Some(release_id) = releases
        .get(selected)
        .map(|release| release.release_id.clone())
        .filter(|release_id| !release_id.is_empty())
    else {
        return;
    };
    let n_tracks = paths.len();
    tokio::spawn(async move {
        let result =
            super::musicbrainz::fetch_release_detail(&release_id, n_tracks, cached_body).await;
        let _ = tx
            .send(crate::tui::message::AppMessage::MbSelectedDetailComplete {
                operation_id,
                releases,
                selected,
                paths,
                editor_session,
                result,
            })
            .await;
    });
}

fn recover_from_selected_mb_detail_failure(
    app: &mut AppState,
    operation_id: super::message::TagsMbOperationId,
    message: String,
) {
    if !tags_mb_operation_is_current_phase(
        app,
        operation_id,
        super::app::TagsMbOperationPhase::Verifying,
    ) {
        return;
    }

    let mut picker_recovered = false;
    if let ActiveOverlay::MbSelect(state) = &mut app.active_overlay {
        if state.phase.verifying_operation() == Some(operation_id) {
            state.phase = super::app::MbSelectPhase::Selecting;
            picker_recovered = true;
        }
    }
    if let Some(state) = app.pending_mb_select.as_mut() {
        if state.phase.verifying_operation() == Some(operation_id) {
            state.phase = super::app::MbSelectPhase::Selecting;
            picker_recovered = true;
        }
    }

    if picker_recovered {
        if let Some(active) = app.active_tags_mb_operation.as_mut() {
            if active.operation_id == operation_id {
                active.phase = super::app::TagsMbOperationPhase::Selecting;
            }
        }
        app.set_status(message);
        return;
    }

    // A single-match flow has no picker to return to. Retire exactly this
    // operation, keep any live editor in place, and only restore a parked one
    // when there is no other modal occupying the overlay slot.
    if finish_tags_mb_operation_if_current(app, operation_id)
        && matches!(app.active_overlay, ActiveOverlay::None)
    {
        restore_parked_editor_without_finishing(app);
    }
    app.set_status(message);
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
struct PendingTagsMbApply {
    operation_id: super::message::TagsMbOperationId,
    releases: Vec<super::musicbrainz::MbRelease>,
    selected: usize,
    paths: Vec<std::path::PathBuf>,
    editor_session: Option<super::message::MetadataEditorSessionGuard>,
}

fn start_mb_select_apply_operation(
    app: &mut AppState,
    mut state: Box<super::app::MbSelectState>,
) -> Option<PendingTagsMbApply> {
    if let Some(operation_id) = state.phase.verifying_operation() {
        let still_current = app.active_tags_mb_operation.is_some_and(|active| {
            active.picker_owned && active.operation_id == operation_id
        });
        app.active_overlay = ActiveOverlay::MbSelect(state);
        if still_current {
            app.set_status(
                ":tags-mb: selected release verification already in progress".to_string(),
            );
        }
        return None;
    }

    let selected = state.selected;
    if selected >= state.releases.len() {
        app.active_overlay = ActiveOverlay::None;
        if state.operation_id.is_assigned() {
            cancel_mb_select_operation(app, state.operation_id);
        } else {
            // An UNASSIGNED picker owns no operation, so any active operation
            // here is FOREIGN — restoring must not finish it (same class as
            // the refusal arm below).
            restore_parked_editor_without_finishing(app);
        }
        app.set_status(
            ":tags-mb: invalid picker selection; restored the metadata editor".to_string(),
        );
        return None;
    }

    let operation_id = if state.operation_id.is_assigned() {
        if !tags_mb_operation_is_current(app, state.operation_id) {
            // A stale picker must not replace or release the workflow that now
            // owns the global authority. Leave it visible for explicit user
            // dismissal without touching the newer operation.
            app.active_overlay = ActiveOverlay::MbSelect(state);
            return None;
        }
        let operation_id = state.operation_id;
        if let Some(active) = app.active_tags_mb_operation.as_mut() {
            active.picker_owned = true;
            active.phase = super::app::TagsMbOperationPhase::Verifying;
        }
        operation_id
    } else {
        match begin_tags_mb_apply_operation(app, true) {
            Ok(operation_id) => {
                state.operation_id = operation_id;
                operation_id
            }
            Err(error) => {
                app.active_overlay = ActiveOverlay::None;
                restore_parked_editor_without_finishing(app);
                app.set_status(error);
                return None;
            }
        }
    };
    state.phase = super::app::MbSelectPhase::Verifying { operation_id };
    // Acceptance supersedes speculative navigation prefetch. If the current
    // row's debounce has not fired yet, invalidate it before the mandatory
    // detail gate below can launch an authoritative fetch. A prefetch that
    // already completed is promoted from `state.prefetch` immediately after.
    state.bump_generation();
    let mut releases = state.releases.clone();
    if let Some(release_id) = releases
        .get(selected)
        .map(|release| release.release_id.clone())
    {
        if let Some(detail) = state.prefetch.get(&release_id) {
            // The speculative prefetch has already paid for and parsed the
            // authoritative release detail. Promote it into the apply payload
            // before leaving the picker so acceptance never launches a
            // duplicate detail request.
            releases[selected] = detail.clone();
        }
    }
    let pending = PendingTagsMbApply {
        operation_id,
        releases,
        selected,
        paths: state.paths.clone(),
        editor_session: state.editor_session,
    };
    app.active_overlay = ActiveOverlay::MbSelect(state);
    Some(pending)
}

/// Accept the currently highlighted MusicBrainz release exactly once.
///
/// The picker is first transitioned to a non-interactive Verifying phase and
/// stamped with an opaque operation identity. Only then may blocking
/// single-image verification be scheduled. Repeated Enter/double-click/context
/// acceptance sees the Verifying phase and cannot launch another task.
pub(super) fn accept_mb_select_release(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    state: Box<super::app::MbSelectState>,
) {
    let Some(pending) = start_mb_select_apply_operation(app, state) else {
        return;
    };
    open_editor_with_mb_release_for_operation(
        app,
        tx,
        pending.operation_id,
        pending.releases,
        pending.selected,
        pending.paths,
        pending.editor_session,
    );
}

#[cfg(test)]
pub(super) fn open_editor_with_mb_release(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    releases: Vec<super::musicbrainz::MbRelease>,
    selected: usize,
    paths: Vec<std::path::PathBuf>,
) {
    open_editor_with_mb_release_guarded(app, tx, releases, selected, paths, None)
}

#[cfg(test)] // only reachable via the cfg(test) open_editor_with_mb_release shim
pub(super) fn open_editor_with_mb_release_guarded(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    releases: Vec<super::musicbrainz::MbRelease>,
    selected: usize,
    paths: Vec<std::path::PathBuf>,
    editor_session: Option<super::message::MetadataEditorSessionGuard>,
) {
    let operation_id = match begin_tags_mb_apply_operation(app, false) {
        Ok(operation_id) => operation_id,
        Err(error) => {
            // `begin_tags_mb_apply_operation` may reject because another
            // lookup owns the authority. This shim did not acquire anything,
            // so it must not finish the foreign operation.
            app.set_status(error);
            return;
        }
    };
    open_editor_with_mb_release_for_operation(
        app,
        tx,
        operation_id,
        releases,
        selected,
        paths,
        editor_session,
    );
}

fn open_editor_with_mb_release_for_operation(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    operation_id: super::message::TagsMbOperationId,
    releases: Vec<super::musicbrainz::MbRelease>,
    selected: usize,
    paths: Vec<std::path::PathBuf>,
    editor_session: Option<super::message::MetadataEditorSessionGuard>,
) {
    if tags_mb_operation_is_current_phase(
        app,
        operation_id,
        super::app::TagsMbOperationPhase::Lookup,
    ) {
        if !transition_tags_mb_operation_phase(
            app,
            operation_id,
            super::app::TagsMbOperationPhase::Lookup,
            super::app::TagsMbOperationPhase::Verifying,
        ) {
            return;
        }
    } else if !tags_mb_operation_is_current_phase(
        app,
        operation_id,
        super::app::TagsMbOperationPhase::Verifying,
    ) {
        return;
    }
    let Some(release) = releases.get(selected) else {
        restore_parked_editor(app);
        app.set_status(":tags-mb: invalid release index".to_string());
        return;
    };
    if !release.relationship_projection_complete {
        if release.release_id.is_empty() {
            recover_from_selected_mb_detail_failure(
                app,
                operation_id,
                ":tags-mb: selected release has no MusicBrainz ID; cannot resolve composer relationships"
                    .to_string(),
            );
            return;
        }
        let cached_body = app
            .db
            .get_cached_mb_search(&super::musicbrainz::detail_cache_key(&release.release_id));
        app.set_status(":tags-mb: loading selected release relationships…".to_string());
        spawn_mb_selected_detail_resolution(
            tx.clone(),
            operation_id,
            releases,
            selected,
            paths,
            editor_session,
            cached_body,
        );
        return;
    }
    if let Some(error) = release.track_parse_error.as_deref() {
        restore_parked_editor(app);
        app.set_status(format!(":tags-mb: refusing incomplete release data: {error}"));
        return;
    }

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
                        operation_id,
                        releases,
                        selected,
                        paths,
                        editor_session,
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
        complete_tags_mb_apply_operation(
            app,
            tx,
            operation_id,
            releases,
            selected,
            paths,
            editor_session,
            super::musicbrainz::PerTrackDecision {
                per_track_populate: false,
                skip_reason: Some(
                    "async verifier unavailable — album-level tags only".to_string(),
                ),
            },
        );
        return;
    }

    complete_tags_mb_apply_operation(
        app,
        tx,
        operation_id,
        releases,
        selected,
        paths,
        editor_session,
        super::musicbrainz::PerTrackDecision::default(),
    );
}

fn complete_tags_mb_apply_operation(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
    operation_id: super::message::TagsMbOperationId,
    releases: Vec<super::musicbrainz::MbRelease>,
    selected: usize,
    paths: Vec<std::path::PathBuf>,
    editor_session: Option<super::message::MetadataEditorSessionGuard>,
    decision: super::musicbrainz::PerTrackDecision,
) {
    let Some(active) = app.active_tags_mb_operation else {
        return;
    };
    if active.operation_id != operation_id {
        // A newer selection/lookup owns the latch. Never release it on behalf
        // of this older completion.
        return;
    }

    if active.picker_owned {
        let picker_matches = matches!(
            &app.active_overlay,
            ActiveOverlay::MbSelect(state)
                if state.phase.verifying_operation() == Some(operation_id)
        ) || app.pending_mb_select.as_ref().is_some_and(|state| {
            state.phase.verifying_operation() == Some(operation_id)
        });
        if !picker_matches {
            // The picker was cancelled, replaced, or otherwise left its
            // verifying state without going through the normal cancel helper.
            // Invalidate this operation and release only its own editor latch.
            // If no replacement overlay exists, also unpark the editor so an
            // abnormal transition cannot strand it invisibly.
            if matches!(app.active_overlay, ActiveOverlay::None) {
                restore_parked_editor(app);
            } else {
                finish_metadata_editor_tags_mb_operation(app);
            }
            log::debug!(
                "discarded MusicBrainz apply completion {:?}: verifying picker no longer active",
                operation_id
            );
            app.set_status(
                ":tags-mb: selected release verification finished after its picker was replaced; no tags were applied"
                    .to_string(),
            );
            return;
        }
        app.active_overlay = ActiveOverlay::None;
    }

    // Consume authority before mutation. A duplicate completion for the same
    // task now fails the check above and therefore applies at most once.
    app.active_tags_mb_operation = None;
    apply_editor_with_mb_release_decision_guarded(
        app,
        tx,
        releases,
        selected,
        paths,
        editor_session,
        decision,
    );
}

pub(super) fn apply_editor_with_mb_release_decision_guarded(
    app: &mut AppState,
    _tx: &mpsc::Sender<AppMessage>,
    releases: Vec<super::musicbrainz::MbRelease>,
    selected: usize,
    paths: Vec<std::path::PathBuf>,
    editor_session: Option<super::message::MetadataEditorSessionGuard>,
    decision: super::musicbrainz::PerTrackDecision,
) {
    // Selection/application is the terminal phase of the :tags-mb operation.
    // Release the editor latch here so every success or rejection below ends
    // the same lifecycle, including async single-image verification.
    finish_metadata_editor_tags_mb_operation(app);
    let Some(release) = releases.get(selected) else {
        restore_parked_editor(app);
        app.set_status(":tags-mb: invalid release index".to_string());
        return;
    };
    if let Some(error) = release.track_parse_error.as_deref() {
        restore_parked_editor(app);
        app.set_status(format!(":tags-mb: refusing incomplete release data: {error}"));
        return;
    }

    // Three arrival modes, checked via `take_metadata_editor_with_restore_slot`:
    // - Browse → MbSelect: no editor was open before `:tags-mb`,
    //   neither slot holds one; `open_metadata_editor` builds fresh
    //   from the selection.
    // - Editor → MbSelect (multi-match SACD path): the source editor
    //   was parked in `pending_metadata_editor` when MbSelect opened.
    // - Editor (SACD single-match path): the editor is sitting in
    //   `active_overlay` because the dispatch deliberately left it
    //   there during the async wait to suppress auto-restore from
    //   the command-input / context-menu wrappers.
    let (mut state, mut split_cue_mb_populated, source_session_validated) = if let Some(taken) = take_metadata_editor_with_restore_slot(app) {
        let restore_slot = taken.slot;
        let s = taken.state;
        if let Some(guard) = editor_session {
            if !metadata_editor_matches_session_guard(&s, guard) {
                restore_taken_metadata_editor(app, TakenMetadataEditor { state: s, slot: restore_slot });
                app.set_status(
                    ":tags-mb: metadata editor changed since lookup; rerun".to_string(),
                );
                return;
            }
            if metadata_editor_paths_match_tags_mb_context(&s, &paths) {
                (s, false, true)
            } else if metadata_editor_can_transition_to_split_cue_target(&s, &paths) {
                if s.any_presentation_dirty() {
                    restore_taken_metadata_editor(app, TakenMetadataEditor { state: s, slot: restore_slot });
                    app.set_status(
                        ":tags-mb: source editor changed during lookup; save or revert editor changes before rerunning"
                            .to_string(),
                    );
                    return;
                }
                match super::keybindings::build_metadata_editor_for_cue_surfaces_with_mb_release(
                    app,
                    &paths,
                    release,
                ) {
                    Ok(Some(state)) => (state, true, true),
                    Ok(None) => {
                        restore_taken_metadata_editor(app, TakenMetadataEditor { state: s, slot: restore_slot });
                        app.set_status(
                            ":tags-mb: could not open split CUE target editor; rerun from Browse"
                                .to_string(),
                        );
                        return;
                    }
                    Err(err) => {
                        restore_taken_metadata_editor(app, TakenMetadataEditor { state: s, slot: restore_slot });
                        app.set_status(format!(
                            ":tags-mb: could not open split CUE editor: {}",
                            err
                        ));
                        return;
                    }
                }
            } else {
                restore_taken_metadata_editor(app, TakenMetadataEditor { state: s, slot: restore_slot });
                app.set_status(
                    ":tags-mb: metadata editor target changed since lookup; rerun"
                        .to_string(),
                );
                return;
            }
        } else {
            if !metadata_editor_paths_match_tags_mb_context(&s, &paths) {
                restore_taken_metadata_editor(app, TakenMetadataEditor { state: s, slot: restore_slot });
                app.set_status(
                    ":tags-mb: metadata editor changed since lookup; rerun".to_string(),
                );
                return;
            }
            (s, false, false)
        }
    } else {
        if editor_session.is_some() {
            app.set_status(
                ":tags-mb: editor closed during lookup; rerun".to_string(),
            );
            return;
        }
        match super::keybindings::build_metadata_editor_for_cue_surfaces_with_mb_release(
            app,
            &paths,
            release,
        ) {
            Ok(Some(state)) => (state, true, false),
            Ok(None) => {
                super::keybindings::open_metadata_editor(app);
                let prior = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
                match prior {
                    ActiveOverlay::MetadataEditor(state) => (state, false, false),
                    other => {
                        app.active_overlay = other;
                        return;
                    }
                }
            }
            Err(err) => {
                app.set_status(format!(":tags-mb: could not open split CUE editor: {}", err));
                return;
            }
        }
    };

    if !source_session_validated && !metadata_editor_matches_tags_mb_context(&state, &paths, editor_session) {
        app.set_status(":tags-mb: metadata editor changed since lookup; rerun".to_string());
        app.active_overlay = ActiveOverlay::MetadataEditor(state);
        return;
    }
    let per_group_apply =
        metadata_editor_tags_mb_context_is_proper_track_prefix(&state, &paths);
    let mut split_cue_mb_report = None;
    if !split_cue_mb_populated
        && state.cue_surface_tabs
        && state.presentation_tabs.len() > 1
        && metadata_editor_paths_match_tags_mb_context(&state, &paths)
    {
        if let Some(report) =
            super::keybindings::populate_split_cue_metadata_editor_from_mb_release(
                &mut state,
                release,
                !per_group_apply,
            )
        {
            split_cue_mb_populated = true;
            split_cue_mb_report = Some(report);
        }
    }
    // The potentially blocking per-track guard was computed before this
    // reducer ran. Reuse it for status text and population so the event loop
    // never performs media/tag inspection here.
    let skip_reason = if split_cue_mb_populated {
        None
    } else {
        decision.skip_reason.clone()
    };
    // Phase C item 3: surface track-count divergence as a non-fatal
    // warning. MB releases sometimes carry bonus/hidden tracks not
    // present on the SACD area being tagged, or the reverse —
    // populate writes what it can match by position. The helper
    // guards single-image rips (where N>1 MB tracks ride in the
    // CUESHEET tag, not in N files) so they don't false-warn.
    let track_count_warning = if split_cue_mb_populated {
        None
    } else {
        super::musicbrainz::track_count_mismatch_message(&state, release)
    };

    // Keep the active tab snapshot current before and after MB population. For
    // multi-presentation editors this preserves per-tab state and lets the
    // apply-to-all confirmation copy only the MB-populated values from the
    // active presentation into matching sibling presentations. Split-CUE MB
    // apply has already populated every tab by concatenated track position;
    // applying the whole release to the active tab here would collapse side B
    // back into side A's shape.
    let mb_mutation_report = if !split_cue_mb_populated {
        let report = super::musicbrainz::populate_editor_from_mb_scoped(
            &mut state,
            release,
            &decision,
            !per_group_apply,
        );
        state.active_surface_mut().dirty = true;
        report
    } else if let Some(report) = split_cue_mb_report {
        report
    } else {
        // Some split-CUE construction paths populate the fresh editor before
        // this reducer receives it. Reconstruct the provider delta from the
        // MB proposal + original-value provenance across every presentation.
        let mut report = super::probe::MetadataMutationReport::default();
        if state.presentation_tabs.is_empty() {
            report.merge(super::probe::MetadataMutationReport::from_musicbrainz_entries(
                &state.active_surface().entries,
            ));
        } else {
            for tab in &state.presentation_tabs {
                report.merge(super::probe::MetadataMutationReport::from_musicbrainz_entries(
                    &tab.entries,
                ));
            }
        }
        report
    };
    let dvdv_duration_warning = if split_cue_mb_populated {
        None
    } else {
        super::musicbrainz::apply_dvdv_duration_warnings(&mut state, release)
    };
    state.phase = super::app::MetadataEditorPhase::Editing;

    let label = if release.title.is_empty() {
        "(untitled)"
    } else {
        &release.title
    };
    let mut msg = format!(":tags-mb: applied \"{}\" — review then save", label);
    if per_group_apply {
        msg.push_str(" [per-group apply: album fields unchanged]");
    }
    if let Some(reason) = skip_reason {
        msg.push_str(&format!(" [{}]", reason));
    }
    if let Some(warn) = track_count_warning {
        msg.push_str(&format!(" [{}]", warn));
    }
    if let Some(warn) = dvdv_duration_warning {
        msg.push_str(&format!(" [{}]", warn));
    }
    mb_mutation_report.append_provider_summary("MusicBrainz", &mut msg);
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

    let has_matching_presentations = !split_cue_mb_populated && state.has_multiple_presentations();
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
    operation_id: super::message::TagsMbOperationId,
    outcome: Result<super::musicbrainz::MbLookupOutcome, String>,
    cue_path: std::path::PathBuf,
    mut album: super::cue_generate::CueAlbumInfo,
    mut tracks: Vec<super::cue_generate::CueTrackInfo>,
    layout: super::message::CueFillLayout,
    toc_string: String,
) {
    if !cue_operation_is_current(app, operation_id) {
        return;
    }
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            finish_cue_operation_if_current(app, operation_id);
            app.set_status(format!(":cue-fill: lookup failed: {error}"));
            return;
        }
    };

    if let Some(json) = outcome.cache_response.as_deref() {
        if let Err(error) = app.db.store_mb_response(&toc_string, json) {
            log::warn!("MB cache store failed: {error}");
        }
    }

    let release = match outcome.releases.into_iter().next() {
        Some(release) => release,
        None => {
            finish_cue_operation_if_current(app, operation_id);
            app.set_status(":cue-fill: no MusicBrainz release matched this disc TOC".to_string());
            return;
        }
    };

    let stats = super::cue_generate::fill_cue_with_mb(&mut album, &mut tracks, &release);
    if stats.is_empty() {
        finish_cue_operation_if_current(app, operation_id);
        app.set_status(format!(
            ":cue-fill: nothing to fill (CUE already complete) — {}",
            cue_path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
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
    if !cue_operation_has_overlay_authority(app, operation_id) {
        finish_cue_operation_if_current(app, operation_id);
        app.set_status(
            ":cue-fill: result discarded because another workflow owns the editor or overlay; retry the command"
                .to_string(),
        );
        return;
    }
    finish_cue_operation_if_current(app, operation_id);
    let _ = tx;
    app.active_overlay = ActiveOverlay::CuePreview(Box::new(
        super::app::CuePreviewState::new(cue_content, cue_path, summary.clone()),
    ));
    app.set_status(summary);
}


#[cfg(test)]
mod startup_recovery_order_tests {
    use super::*;

    #[test]
    fn database_prepared_recovery_consumes_byte_identical_dsf_marker_before_scan() {
        let db = crate::db::Database::open_memory().expect("memory database");
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("album.dsf");
        let backup = crate::db::Database::backup_path_for(&target);
        let original = b"DSD synthetic original bytes";
        std::fs::write(&target, original).expect("write target");
        std::fs::write(&backup, original).expect("write byte-identical backup");
        db.begin_metadata_write(
            &target.display().to_string(),
            &backup.display().to_string(),
        )
        .expect("record prepared metadata write");

        let messages = startup_metadata_recovery_messages(&db, temp.path());

        assert!(
            messages.iter().any(|message| message.starts_with("Recovered:")),
            "database recovery must consume the marker first: {messages:?}"
        );
        assert!(
            messages
                .iter()
                .all(|message| !message.contains("rollback marker is missing")),
            "standalone scanning must not strand the prepared transaction: {messages:?}"
        );
        assert!(!backup.exists());
        assert!(db
            .stale_metadata_writes()
            .expect("read metadata journal")
            .is_empty());
        assert_eq!(
            std::fs::read(&target).expect("read target"),
            original.to_vec()
        );
    }
}

#[cfg(test)]
mod archive_listing_overlay_authority_tests {
    use super::*;
    use crate::config::TonepoetConfig;

    fn tx() -> mpsc::Sender<AppMessage> {
        let (tx, _rx) = mpsc::channel(4);
        tx
    }

    #[tokio::test]
    async fn wrong_password_relisting_does_not_clobber_an_occupied_overlay() {
        // Audit HIGH: the wrong-password Enter path can RESTORE a parked
        // dirty metadata editor into the slot before the listing failure
        // arrives; the re-prompt used to overwrite (and drop) it.
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let archive = std::path::PathBuf::from("/library/album.7z");
        let (id, _cancel) = app.begin_archive_listing(archive.clone());
        let editor = crate::tui::app::MetadataEditorState::for_files(
            vec![std::path::PathBuf::from("/tmp/a.flac")],
            Vec::new(),
            vec!["a".to_string()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        );
        app.active_overlay = ActiveOverlay::MetadataEditor(Box::new(editor));
        let (tx, _rx) = mpsc::channel(4);

        let __tab_id_fix = app.browse.active_tab_id();
        handle_message(
            &mut app,
            AppMessage::ArchiveListingComplete {
                tab_id: __tab_id_fix,
                id,
                archive_path: archive.clone(),
                cache_key: None,
                result: Box::new(Err("Wrong password?".to_string())),
                password: Some("wrong".to_string()),
            },
            &tx,
        );

        assert!(
            matches!(app.active_overlay, ActiveOverlay::MetadataEditor(_)),
            "the occupied overlay must survive the failed relisting"
        );
        assert!(app
            .status_message
            .as_ref()
            .is_some_and(|(message, _)| message.contains(":password to retry")));

        // The empty-slot case still prompts.
        let (id, _cancel) = app.begin_archive_listing(archive.clone());
        app.active_overlay = ActiveOverlay::None;
        let __tab_id_fix = app.browse.active_tab_id();
        handle_message(
            &mut app,
            AppMessage::ArchiveListingComplete {
                tab_id: __tab_id_fix,
                id,
                archive_path: archive,
                cache_key: None,
                result: Box::new(Err("Wrong password?".to_string())),
                password: Some("wrong".to_string()),
            },
            &tx,
        );
        assert!(
            matches!(
                app.active_overlay,
                ActiveOverlay::TextEdit {
                    target: super::super::app::TextEditTarget::ArchivePassword(_),
                    ..
                }
            ),
            "an empty slot still receives the password prompt"
        );
    }

    #[test]
    fn convert_archive_preview_password_prompt_preserves_an_occupied_overlay() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let archive = std::path::PathBuf::from("/library/encrypted.7z");
        app.probe_generation = 7;
        app.convert.set_source_mode(super::super::app::SourceMode::Single {
            path: archive.clone(),
            info: None,
            metadata: crate::tui::probe::SourceMetadata::default(),
            probe_notice: None,
        });
        app.convert.install_pending_archive_preview(
            super::super::app::create_pending_archive_preview(7, archive.clone()),
        );
        let baseline = super::super::app::ConvertProbeBaseline::capture(&app.convert);
        app.active_overlay = ActiveOverlay::Help {
            screen: AppScreen::Convert,
            scroll: 3,
        };

        handle_archive_preview_result(
            &mut app,
            7,
            archive,
            Err("Wrong password".to_string()),
            &tx(),
            baseline,
        );

        assert!(matches!(
            app.active_overlay,
            ActiveOverlay::Help {
                screen: AppScreen::Convert,
                scroll: 3
            }
        ));
        assert!(app.status_message.as_ref().is_some_and(|(message, _)| {
            message.contains("current editor or overlay was preserved")
        }));
    }

    #[test]
    fn archive_metadata_password_prompt_preserves_a_parked_editor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("encrypted.7z");
        let staging = temp.path().join("staging");
        std::fs::write(&archive, b"archive").expect("archive");
        std::fs::create_dir_all(&staging).expect("staging");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let mut editor = super::super::app::MetadataEditorState::for_files(
            vec![std::path::PathBuf::from("/tmp/parked.flac")],
            Vec::new(),
            vec!["parked".to_string()],
            super::super::app::MetadataTechnicalDetails::default(),
        );
        editor.active_surface_mut().dirty = true;
        let parked_session_id = editor.active_surface().technical_details.session_id;
        app.pending_metadata_editor = Some(Box::new(editor));
        // The password re-prompt path only exists for extractions that own
        // their staging tree; `from_existing` models adopted staging and
        // would route to the generic failure status instead.
        let mut pending = super::super::app::PendingBrowseArchiveMetadataEdit::from_existing(
            archive.clone(),
            staging.clone(),
            None,
        );
        pending.owns_staging = true;
        app.pending_browse_archive_metadata = Some(pending);

        handle_archive_metadata_editor_prepared(
            &mut app,
            archive,
            staging,
            Err("Wrong password".to_string()),
            &tx(),
        );

        assert!(matches!(app.active_overlay, ActiveOverlay::None));
        assert!(app.pending_metadata_editor.as_deref().is_some_and(|editor| {
            editor.active_surface().dirty
                && editor.active_surface().technical_details.session_id == parked_session_id
        }));
        assert!(app.status_message.as_ref().is_some_and(|(message, _)| {
            message.contains("current editor or overlay was preserved")
        }));
    }
}

#[cfg(test)]
mod sentinel_clamp_status_tests {
    use super::*;
    use crate::config::TonepoetConfig;

    #[test]
    fn pcm_probe_over_staged_dsd_sentinel_reports_the_clamp() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        // Probe-proven DSD source, DSF target, deliberate rate=source.
        app.convert.format.set_source_is_dsd(true);
        app.convert
            .format
            .format
            .select_value(&crate::convert::formats::AudioFormat::Dsf);
        app.convert.format.apply_format_constraints();
        assert!(app
            .convert
            .format
            .sample_rate
            .select_value(&super::super::app::SOURCE_SAMPLE_RATE_SENTINEL));

        // Production shape: placeholder install, baseline capture, dispatch.
        let path = std::path::PathBuf::from("/library/track.flac");
        app.convert.set_source_mode(super::super::app::SourceMode::Single {
            path: path.clone(),
            info: None,
            metadata: crate::tui::probe::SourceMetadata::default(),
            probe_notice: None,
        });
        let baseline = super::super::app::ConvertProbeBaseline::capture(&app.convert);
        let generation = app.probe_generation;

        let realized = super::super::app::SourceMode::Single {
            path: path.clone(),
            info: Some(crate::tui::probe::SourceInfo {
                format_name: "FLAC".to_string(),
                codec: "flac".to_string(),
                bit_depth: Some(24),
                sample_format_is_float: None,
                sample_rate: 96_000,
                channels: 2,
                channel_layout: "stereo".to_string(),
                duration_secs: 10.0,
                file_size: 1_000,
            }),
            metadata: crate::tui::probe::SourceMetadata::default(),
            probe_notice: Some("tolerant metadata read".to_string()),
        };
        handle_convert_source_probe_result(&mut app, generation, path, realized, baseline);

        // The clamp itself is correct (rate=source is invalid for a DSD
        // target with a KNOWN PCM source) — the pin is that it is REPORTED.
        assert_ne!(
            *app.convert.format.sample_rate.selected_value(),
            super::super::app::SOURCE_SAMPLE_RATE_SENTINEL
        );
        let status = app
            .status_message
            .as_ref()
            .map(|(message, _)| message.clone())
            .expect("clamp must set a status");
        assert!(
            status.contains("Probe warning: tolerant metadata read"),
            "probe warning was overwritten: {status}"
        );
        assert!(
            status.contains("rate 'source' is invalid for"),
            "clamp status is missing: {status}"
        );
    }

    #[test]
    fn quit_with_unstarted_transfers_requires_explicit_loss_confirmation() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let clipboard = tui_file_picker::FilesystemClipboard::new(
            tui_file_picker::FilePickerClipboardMode::Copy,
            vec![std::path::PathBuf::from("/source/album")],
        )
        .expect("clipboard");
        let plan = tui_file_picker::PastePlan {
            mode: tui_file_picker::FilePickerClipboardMode::Copy,
            mappings: vec![tui_file_picker::PasteMapping {
                source: std::path::PathBuf::from("/source/album"),
                destination: std::path::PathBuf::from("/destination/album"),
            }],
        };
        app.file_transfers.queued.push_back(crate::tui::app::QueuedFileTransfer {
            queue_id: 41,
            clipboard,
            clipboard_owner_generation: None,
            destination_dir: std::path::PathBuf::from("/destination"),
            enqueue_plan: plan.clone(),
            retry_plan: Some(crate::tui::browse::BrowsePasteRetryPlan::from_plan(plan)),
            recovered: false,
        });
        let progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Copy,
            "Copying files",
            super::super::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        let (control_tx, _control_rx) = std::sync::mpsc::channel();
        let live_session = crate::tui::app::FileTaskProgressSession::new(progress, control_tx);
        let live_session_id = live_session.session_id;
        app.active_overlay = ActiveOverlay::FileTaskProgress(live_session);
        app.should_quit = true;

        assert!(defer_quit_for_queued_file_transfers(&mut app));
        assert!(!app.should_quit);
        let ActiveOverlay::Confirmation { message, action } = &app.active_overlay else {
            panic!("queued transfers must open a blocking quit confirmation");
        };
        assert!(message.contains("has no journal and will be lost"));
        assert!(matches!(
            action,
            super::super::app::ConfirmAction::QuitWithQueuedFileTransfers {
                unjournaled_count: 1,
                journal_backed_count: 0,
            }
        ));
        assert!(matches!(
            app.queued_quit_preempted_overlay.as_deref(),
            Some(ActiveOverlay::FileTaskProgress(session)) if session.session_id == live_session_id
        ));
        assert_eq!(app.file_transfers.queued.len(), 1);

        let (tx, _rx) = mpsc::channel(8);
        reduce_file_task_progress(
            &mut app,
            live_session_id,
            tui_file_picker::FileTaskProgressUpdate::Snapshot {
                phase: tui_file_picker::FileTaskPhase::Running,
                status: "still running under quit confirmation".to_string(),
                current_item: None,
                totals: tui_file_picker::ProgressTotals::default(),
                rate_bytes_per_sec: None,
            },
            &tx,
        );
        assert!(matches!(
            app.queued_quit_preempted_overlay.as_deref(),
            Some(ActiveOverlay::FileTaskProgress(session))
                if session.progress.status == "still running under quit confirmation"
        ));
        assert!(matches!(app.active_overlay, ActiveOverlay::Confirmation { .. }));

        let conflict = tui_file_picker::ConflictPromptState::new(
            29,
            "Queued quit attention",
            "Resolve before quitting",
            tui_file_picker::ConflictItemKind::File,
        );
        reduce_file_task_progress(
            &mut app,
            live_session_id,
            tui_file_picker::FileTaskProgressUpdate::ShowConflict {
                conflict: conflict.clone(),
            },
            &tx,
        );
        assert!(app.queued_quit_preempted_overlay.is_none());
        assert!(matches!(
            &app.active_overlay,
            ActiveOverlay::FileTaskProgress(session)
                if session.session_id == live_session_id
                    && session.progress.conflict.as_ref() == Some(&conflict)
        ));
        assert!(matches!(
            app.file_task_preempted_overlay.as_deref(),
            Some(ActiveOverlay::Confirmation { .. })
        ));
    }

    #[test]
    fn quit_prompt_distinguishes_lost_and_journal_backed_queue_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let journal_path = temp.path().join("recovery.jsonl");
        std::fs::write(&journal_path, b"pending recovery\n").expect("journal fixture");
        let mut app = AppState::new_for_test(TonepoetConfig::default());

        for (queue_id, source, recovery_journal_path) in [
            (51, "/source/new", None),
            (52, "/source/recovery", Some(journal_path.clone())),
        ] {
            let clipboard = tui_file_picker::FilesystemClipboard::new(
                tui_file_picker::FilePickerClipboardMode::Copy,
                vec![std::path::PathBuf::from(source)],
            )
            .expect("clipboard");
            let plan = tui_file_picker::PastePlan {
                mode: tui_file_picker::FilePickerClipboardMode::Copy,
                mappings: vec![tui_file_picker::PasteMapping {
                    source: std::path::PathBuf::from(source),
                    destination: std::path::PathBuf::from(format!(
                        "/destination/{queue_id}"
                    )),
                }],
            };
            let mut retry = crate::tui::browse::BrowsePasteRetryPlan::from_plan(plan.clone());
            retry.recovery_journal_path = recovery_journal_path;
            app.file_transfers.queued.push_back(crate::tui::app::QueuedFileTransfer {
                queue_id,
                clipboard,
                clipboard_owner_generation: None,
                destination_dir: std::path::PathBuf::from("/destination"),
                enqueue_plan: plan,
                retry_plan: Some(retry),
                recovered: queue_id == 52,
            });
        }

        assert!(defer_quit_for_queued_file_transfers(&mut app));
        let ActiveOverlay::Confirmation { message, action } = &app.active_overlay else {
            panic!("mixed queue must open a quit confirmation");
        };
        assert!(message.contains("1 unstarted transfer has no journal and will be lost"));
        assert!(message.contains(
            "1 journal-backed recovery transfer will leave the current in-memory queue but remain pending for the next startup"
        ));
        assert!(matches!(
            action,
            super::super::app::ConfirmAction::QuitWithQueuedFileTransfers {
                unjournaled_count: 1,
                journal_backed_count: 1,
            }
        ));
        assert!(journal_path.exists());
    }

    #[test]
    fn quit_prompt_for_only_journal_backed_recoveries_does_not_claim_durable_loss() {
        let temp = tempfile::tempdir().expect("tempdir");
        let journal_path = temp.path().join("recovery.jsonl");
        std::fs::write(&journal_path, b"pending recovery\n").expect("journal fixture");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let clipboard = tui_file_picker::FilesystemClipboard::new(
            tui_file_picker::FilePickerClipboardMode::Cut,
            vec![std::path::PathBuf::from("/source/recovery")],
        )
        .expect("clipboard");
        let plan = tui_file_picker::PastePlan {
            mode: tui_file_picker::FilePickerClipboardMode::Cut,
            mappings: vec![tui_file_picker::PasteMapping {
                source: std::path::PathBuf::from("/source/recovery"),
                destination: std::path::PathBuf::from("/destination/recovery"),
            }],
        };
        let mut retry = crate::tui::browse::BrowsePasteRetryPlan::from_plan(plan.clone());
        retry.recovery_journal_path = Some(journal_path.clone());
        app.file_transfers.queued.push_back(crate::tui::app::QueuedFileTransfer {
            queue_id: 61,
            clipboard,
            clipboard_owner_generation: None,
            destination_dir: std::path::PathBuf::from("/destination"),
            enqueue_plan: plan,
            retry_plan: Some(retry),
            recovered: true,
        });

        assert!(defer_quit_for_queued_file_transfers(&mut app));
        let ActiveOverlay::Confirmation { message, action } = &app.active_overlay else {
            panic!("journal-backed queue must open a quit confirmation");
        };
        assert!(!message.contains("no journal"));
        assert!(!message.contains("will be lost"));
        assert!(message.contains("remain pending for the next startup"));
        assert!(matches!(
            action,
            super::super::app::ConfirmAction::QuitWithQueuedFileTransfers {
                unjournaled_count: 0,
                journal_backed_count: 1,
            }
        ));
        assert!(journal_path.exists());
    }

}

#[cfg(test)]
mod metadata_write_completion_tests {
    use super::*;
    use crate::config::TonepoetConfig;
    use crate::tui::probe::MetadataField;

    fn tx() -> mpsc::Sender<AppMessage> {
        let (tx, _rx) = mpsc::channel(4);
        tx
    }

    #[test]
    fn non_flac_completion_publishes_structured_transaction_failure_verbatim() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("track.opus");
        std::fs::write(&path, b"restored original bytes").expect("write destination");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let (operation_id, _cancel) = app.begin_inline_metadata_write(path.clone());

        handle_message(
            &mut app,
            AppMessage::MetadataWriteComplete {
                operation_id,
                path: path.clone(),
                field: MetadataField::Title,
                value: "new title".to_string(),
                result: Err(
                    "write failed (rolled back): synthetic writer failure".to_string(),
                ),
            },
            &tx(),
        );

        assert_eq!(
            std::fs::read(&path).expect("unchanged restored destination"),
            b"restored original bytes"
        );
        assert_eq!(
            app.status_message
                .as_ref()
                .map(|(message, _)| message.as_str()),
            Some("write failed (rolled back): synthetic writer failure")
        );
    }

    #[test]
    fn native_flac_failure_does_not_consume_unrelated_full_file_backup() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("track.flac");
        let backup = crate::db::Database::backup_path_for(&path);
        std::fs::write(&path, b"current FLAC bytes").expect("write destination");
        std::fs::write(&backup, b"unrelated stale backup").expect("write stale backup");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let (operation_id, _cancel) = app.begin_inline_metadata_write(path.clone());

        handle_message(
            &mut app,
            AppMessage::MetadataWriteComplete {
                operation_id,
                path: path.clone(),
                field: MetadataField::Title,
                value: "new title".to_string(),
                result: Err("native writer failure".to_string()),
            },
            &tx(),
        );

        assert_eq!(std::fs::read(&path).expect("unchanged destination"), b"current FLAC bytes");
        assert_eq!(std::fs::read(&backup).expect("stale backup retained"), b"unrelated stale backup");
        assert_eq!(
            app.status_message
                .as_ref()
                .map(|(message, _)| message.as_str()),
            Some("write failed: native writer failure")
        );
    }


    #[test]
    fn stale_inline_metadata_progress_and_completion_are_ignored() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let current_path = std::path::PathBuf::from("current.dsf");
        let stale_path = std::path::PathBuf::from("stale.dsf");
        let (current_id, _cancel) = app.begin_inline_metadata_write(current_path.clone());
        app.set_status("current operation");

        handle_message(
            &mut app,
            AppMessage::MetadataWriteProgress {
                operation_id: current_id.saturating_sub(1),
                path: stale_path.clone(),
                detail: "stale progress".to_string(),
            },
            &tx(),
        );
        handle_message(
            &mut app,
            AppMessage::MetadataWriteComplete {
                operation_id: current_id.saturating_sub(1),
                path: stale_path,
                field: MetadataField::Title,
                value: "stale".to_string(),
                result: Ok(crate::tui::probe::MetadataWriteCommitReport::default()),
            },
            &tx(),
        );

        assert!(app.inline_metadata_write_is_current(current_id, &current_path));
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.as_str()),
            Some("current operation")
        );
    }

    #[test]
    fn inline_metadata_completion_surfaces_durability_warning() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("album.dsf");
        std::fs::write(&path, b"fixture").expect("write fixture");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let (operation_id, _cancel) = app.begin_inline_metadata_write(path.clone());

        handle_message(
            &mut app,
            AppMessage::MetadataWriteComplete {
                operation_id,
                path: path.clone(),
                field: MetadataField::Title,
                value: "new title".to_string(),
                result: Ok(crate::tui::probe::MetadataWriteCommitReport {
                    durability_warnings: vec!["journal retirement could not be confirmed".to_string()],
                }),
            },
            &tx(),
        );

        let status = app.status_message.as_ref().map(|(message, _)| message.as_str());
        assert_eq!(
            status,
            Some("album.dsf: title updated, with durability warning: journal retirement could not be confirmed")
        );
        assert!(app.inline_metadata_write.is_none());
    }
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
        PendingBrowseArchiveDelete, PendingBrowseArchiveRename,
    };
    use std::fs;

    fn tx() -> mpsc::Sender<AppMessage> {
        let (tx, _rx) = mpsc::channel(8);
        tx
    }

    fn install_repackage_progress_overlay(app: &mut AppState) -> u64 {
        let (control_tx, _control_rx) = std::sync::mpsc::channel();
        let progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Archive,
            "Repackaging archive",
            super::super::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        let session = super::super::app::FileTaskProgressSession::new(progress, control_tx);
        let progress_session_id = session.session_id;
        app.browse_archive_repackage_progress_session_id = Some(progress_session_id);
        app.active_overlay = ActiveOverlay::FileTaskProgress(session);
        progress_session_id
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
    fn quit_is_blocked_by_dirty_parked_non_archive_editor() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.should_quit = true;
        let mut state = MetadataEditorState::for_files(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            MetadataTechnicalDetails::default(),
        );
        state.active_surface_mut().dirty = true;
        app.pending_metadata_editor = Some(Box::new(state));

        assert!(defer_quit_for_browse_archive_metadata(&mut app, &tx()));
        assert!(!app.should_quit);
        assert!(app.pending_metadata_editor.is_some());
        assert!(app
            .status_message
            .as_ref()
            .is_some_and(|(message, _)| message.contains("parked metadata editor")));
    }

    #[test]
    fn quit_requests_invalid_ape_repair_cancellation_and_keeps_editor_owned() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.should_quit = true;
        let mut state = MetadataEditorState::for_files(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            MetadataTechnicalDetails::default(),
        );
        let (_session_id, _generation, cancel) = state.begin_invalid_ape_repair(vec![(
            std::path::PathBuf::from("album/track.wv"),
            vec!["&год".to_string()],
        )]);
        app.active_overlay = ActiveOverlay::MetadataEditor(Box::new(state));

        assert!(defer_quit_for_browse_archive_metadata(&mut app, &tx()));
        assert!(!app.should_quit);
        assert!(cancel.is_cancelled());
        assert!(matches!(app.active_overlay, ActiveOverlay::MetadataEditor(_)));
        assert!(app.status_message.as_ref().is_some_and(|(message, _)| {
            message.contains("repair cancellation requested")
                && message.contains("classified completion ledger")
        }));
    }

    #[test]
    fn quit_is_blocked_by_dirty_open_non_archive_editor() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.should_quit = true;
        let mut state = MetadataEditorState::for_files(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            MetadataTechnicalDetails::default(),
        );
        state.active_surface_mut().dirty = true;
        app.active_overlay = ActiveOverlay::MetadataEditor(Box::new(state));

        assert!(defer_quit_for_browse_archive_metadata(&mut app, &tx()));
        assert!(!app.should_quit);
        assert!(matches!(app.active_overlay, ActiveOverlay::MetadataEditor(_)));
        assert!(app
            .status_message
            .as_ref()
            .is_some_and(|(message, _)| message.contains("metadata editor")));
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

    fn install_clean_archive_staging(
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
        let session_guard = crate::tui::browse::ArchiveStagingSession::new_test_owned(
            staging,
            archive,
            secs,
            nanos,
            size,
        );
        app.browse.archive.as_mut().expect("archive").staging = Some(session_guard.into_inner());
    }

    #[test]
    fn quit_defers_pending_archive_rename_before_clean_staging_drain() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        let staging = temp.path().join("clean-staging");
        fs::write(&archive, b"archive").expect("archive");
        fs::create_dir(&staging).expect("staging");
        let (secs, nanos, size) = crate::tui::app::archive_fingerprint(&archive).expect("fingerprint");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.should_quit = true;
        install_clean_archive_staging(&mut app, archive.clone(), staging.clone());
        app.pending_browse_archive_rename = Some(PendingBrowseArchiveRename::new(
            archive,
            "old.flac".to_string(),
            "new.flac".to_string(),
            secs,
            nanos,
            size,
            None,
        ));

        assert!(defer_quit_for_browse_archive_metadata(&mut app, &tx()));
        assert!(!app.should_quit);
        assert!(app.quit_after_browse_archive_rename);
        assert!(app.pending_browse_archive_rename.is_some());
        assert!(app.browse.active_archive_staging().is_some());
        assert!(staging.exists());
    }

    #[test]
    fn quit_defers_pending_archive_delete_before_clean_staging_drain() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        let staging = temp.path().join("clean-staging");
        fs::write(&archive, b"archive").expect("archive");
        fs::create_dir(&staging).expect("staging");
        let (secs, nanos, size) = crate::tui::app::archive_fingerprint(&archive).expect("fingerprint");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.should_quit = true;
        install_clean_archive_staging(&mut app, archive.clone(), staging.clone());
        app.pending_browse_archive_delete = Some(PendingBrowseArchiveDelete::new(
            archive,
            vec!["old.flac".to_string()],
            secs,
            nanos,
            size,
            None,
        ));

        assert!(defer_quit_for_browse_archive_metadata(&mut app, &tx()));
        assert!(!app.should_quit);
        assert!(app.quit_after_browse_archive_delete);
        assert!(app.pending_browse_archive_delete.is_some());
        assert!(app.browse.active_archive_staging().is_some());
        assert!(staging.exists());
    }

    #[test]
    fn quit_cancels_pending_archive_metadata_then_drains_clean_staging() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.zip");
        let staging = temp.path().join("clean-staging");
        fs::write(&archive, b"archive").expect("archive");
        fs::create_dir(&staging).expect("staging");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.should_quit = true;
        app.browse.current_dir = temp.path().to_path_buf();
        install_clean_archive_staging(&mut app, archive.clone(), staging.clone());
        let pending = super::super::app::PendingBrowseArchiveMetadataEdit::from_existing(
            archive,
            staging.clone(),
            None,
        );
        let cancel = pending.cancel.clone();
        app.pending_browse_archive_metadata = Some(pending);

        assert!(!defer_quit_for_browse_archive_metadata(&mut app, &tx()));
        assert!(app.pending_browse_archive_metadata.is_none());
        assert!(cancel.is_cancelled());
        assert!(app.browse.active_archive_staging().is_none());
        assert!(!staging.exists());
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
        let progress_session_id = install_repackage_progress_overlay(&mut app);

        handle_archive_repackage_result(
            &mut app,
            archive,
            staging.clone(),
            progress_session_id,
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
        let progress_session_id = install_repackage_progress_overlay(&mut app);

        handle_archive_repackage_result(
            &mut app,
            archive,
            staging.clone(),
            progress_session_id,
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
        let progress_session_id = app
            .browse_archive_repackage_progress_session_id
            .expect("active repackage progress session");

        handle_archive_repackage_result(
            &mut app,
            archive.clone(),
            staging.clone(),
            progress_session_id,
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
        let progress_session_id = install_repackage_progress_overlay(&mut app);

        handle_archive_repackage_result(
            &mut app,
            archive.clone(),
            staging.clone(),
            progress_session_id,
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

    #[test]
    fn failed_repackage_does_not_replace_a_newer_overlay_or_progress_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.tar");
        let staging = temp.path().join("staging");
        fs::write(&archive, b"archive").expect("archive");
        fs::create_dir(&staging).expect("staging");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.browse_archive_repackage = Some(ArchiveMetadataEditContext::browse(
            archive.clone(),
            staging.clone(),
        ));
        let progress_session_id = install_repackage_progress_overlay(&mut app);
        let stale_progress_session_id = progress_session_id.wrapping_add(1);
        app.active_overlay = ActiveOverlay::Help {
            screen: AppScreen::Browse,
            scroll: 9,
        };
        app.set_status("newer overlay status");
        let status_before_progress = app
            .status_message
            .as_ref()
            .map(|(message, _)| message.clone());
        let late_snapshot =
            crate::convert::pipeline::materializer_archive::ArchiveRepackageProgressSnapshot {
                stage: crate::convert::pipeline::materializer_archive::ArchiveRepackageStage::Compressing,
                status: "late progress".to_string(),
                current_item: None,
                bytes_done: 1,
                bytes_total: Some(2),
                items_done: 0,
                items_total: Some(1),
                rate_bytes_per_sec: Some(1),
            };

        handle_archive_repackage_progress(
            &mut app,
            archive.clone(),
            staging.clone(),
            stale_progress_session_id,
            late_snapshot.clone(),
        );
        handle_archive_repackage_result(
            &mut app,
            archive.clone(),
            staging.clone(),
            stale_progress_session_id,
            Err("stale retry failure".to_string()),
            &tx(),
        );
        assert_eq!(
            app.browse_archive_repackage_progress_session_id,
            Some(progress_session_id),
            "same-path stale worker must not retire the newer progress session"
        );
        assert!(app.browse_archive_repackage.as_ref().is_some_and(|context| {
            context.archive_path == archive && context.staging_dir == staging
        }));
        assert!(app.preserved_editor_archive_repackage.is_none());
        assert_eq!(
            app.status_message
                .as_ref()
                .map(|(message, _)| message.clone()),
            status_before_progress,
            "same-path stale worker must not mutate status"
        );

        handle_archive_repackage_progress(
            &mut app,
            archive.clone(),
            staging.clone(),
            progress_session_id,
            late_snapshot,
        );
        assert_eq!(
            app.status_message
                .as_ref()
                .map(|(message, _)| message.clone()),
            status_before_progress,
            "owned late progress must not mutate status after its progress surface lost authority"
        );

        handle_archive_repackage_result(
            &mut app,
            archive.clone(),
            staging.clone(),
            progress_session_id,
            Err("late failure".to_string()),
            &tx(),
        );

        assert!(matches!(
            app.active_overlay,
            ActiveOverlay::Help {
                screen: AppScreen::Browse,
                scroll: 9
            }
        ));
        assert!(app.preserved_editor_archive_repackage.as_ref().is_some_and(|context| {
            context.archive_path == archive && context.staging_dir == staging
        }));
        assert!(app.status_message.as_ref().is_some_and(|(message, _)| {
            message.contains("current editor or overlay was not replaced")
        }));
    }

    fn install_minimized_transfer_then_archive(
        app: &mut AppState,
    ) -> (u64, u64) {
        let transfer_progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Copy,
            "Copying files",
            super::super::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        let (transfer_tx, _transfer_rx) = std::sync::mpsc::channel();
        let transfer_session = super::super::app::FileTaskProgressSession::new(
            transfer_progress,
            transfer_tx,
        );
        let transfer_session_id = transfer_session.session_id;
        app.file_transfers.active_session_id = Some(transfer_session_id);
        app.file_transfers.keep_minimized_across_jobs = true;
        app.install_file_task_progress_with_visibility(transfer_session, true);

        let archive_progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Archive,
            "Repackaging archive",
            super::super::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        let (archive_tx, _archive_rx) = std::sync::mpsc::channel();
        let archive_session = super::super::app::FileTaskProgressSession::new(
            archive_progress,
            archive_tx,
        );
        let archive_session_id = archive_session.session_id;
        app.install_file_task_progress(archive_session);
        (transfer_session_id, archive_session_id)
    }

    fn enqueue_test_transfer(app: &mut AppState, queue_id: u64) {
        let clipboard = tui_file_picker::FilesystemClipboard::new(
            tui_file_picker::FilePickerClipboardMode::Copy,
            vec![std::path::PathBuf::from("/source/next")],
        )
        .expect("clipboard");
        let plan = tui_file_picker::PastePlan {
            mode: tui_file_picker::FilePickerClipboardMode::Copy,
            mappings: vec![tui_file_picker::PasteMapping {
                source: std::path::PathBuf::from("/source/next"),
                destination: std::path::PathBuf::from("/destination/next"),
            }],
        };
        app.file_transfers.queued.push_back(crate::tui::app::QueuedFileTransfer {
            queue_id,
            clipboard,
            clipboard_owner_generation: None,
            destination_dir: std::path::PathBuf::from("/destination"),
            enqueue_plan: plan.clone(),
            retry_plan: Some(crate::tui::browse::BrowsePasteRetryPlan::from_plan(plan)),
            recovered: false,
        });
        app.sync_file_transfer_queue_surfaces();
    }

    #[test]
    fn parked_archive_success_updates_and_returns_before_fifo_continues() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.tar");
        let staging = temp.path().join("staging");
        fs::write(&archive, b"archive").expect("archive");
        fs::create_dir(&staging).expect("staging");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let (transfer_session_id, archive_session_id) =
            install_minimized_transfer_then_archive(&mut app);
        app.browse_archive_repackage = Some(ArchiveMetadataEditContext::browse(
            archive.clone(),
            staging.clone(),
        ));
        app.browse_archive_repackage_progress_session_id = Some(archive_session_id);
        enqueue_test_transfer(&mut app, 81);
        app.test_file_task_dispatches = Some(Vec::new());
        let tx = tx();

        reduce_file_task_progress(
            &mut app,
            transfer_session_id,
            tui_file_picker::FileTaskProgressUpdate::Failed {
                status: "transfer failed".to_string(),
                totals: tui_file_picker::ProgressTotals {
                    errors: 1,
                    ..tui_file_picker::ProgressTotals::default()
                },
            },
            &tx,
        );
        finalize_file_transfer_scheduler(&mut app, transfer_session_id, true, &tx);
        assert!(app.file_transfers.blocked_for_attention);
        assert!(matches!(
            app.file_task_preempted_overlay.as_deref(),
            Some(ActiveOverlay::FileTaskProgress(session))
                if session.session_id == archive_session_id
        ));

        handle_archive_repackage_progress(
            &mut app,
            archive.clone(),
            staging.clone(),
            archive_session_id,
            ArchiveRepackageProgressSnapshot {
                stage: ArchiveRepackageStage::Compressing,
                status: "compressing while parked".to_string(),
                current_item: None,
                bytes_done: 40,
                bytes_total: Some(100),
                items_done: 0,
                items_total: Some(1),
                rate_bytes_per_sec: Some(10),
            },
        );
        assert!(matches!(
            app.file_task_preempted_overlay.as_deref(),
            Some(ActiveOverlay::FileTaskProgress(session))
                if session.session_id == archive_session_id
                    && session.progress.status == "compressing while parked"
                    && session.progress.totals.bytes_done == 40
        ));

        handle_archive_repackage_result(
            &mut app,
            archive,
            staging,
            archive_session_id,
            Ok(ArchiveRepackageReport::default()),
            &tx,
        );
        assert!(matches!(
            app.file_task_preempted_overlay.as_deref(),
            Some(ActiveOverlay::FileTaskProgress(session))
                if session.session_id == archive_session_id
                    && session.progress.is_terminal()
                    && session.progress.status == "Archive repackaged"
        ));

        super::super::keybindings::handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ),
            &tx,
        );
        assert!(!app.file_transfers.blocked_for_attention);
        assert!(matches!(
            &app.active_overlay,
            ActiveOverlay::FileTaskProgress(session)
                if session.session_id == archive_session_id
                    && session.progress.is_terminal()
        ));
        assert_eq!(app.file_transfers.queued.len(), 1);

        super::super::keybindings::handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ),
            &tx,
        );
        assert!(matches!(app.active_overlay, ActiveOverlay::None));
        assert_eq!(app.file_transfers.queued.len(), 1);

        super::super::keybindings::maybe_start_next_file_transfer(&mut app, &tx);

        assert_eq!(app.file_transfers.queued.len(), 0);
        assert_eq!(
            app.test_file_task_dispatches
                .as_ref()
                .map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn parked_archive_failure_returns_retry_confirmation_after_transfer_attention() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("album.tar");
        let staging = temp.path().join("staging");
        fs::write(&archive, b"archive").expect("archive");
        fs::create_dir(&staging).expect("staging");

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let (transfer_session_id, archive_session_id) =
            install_minimized_transfer_then_archive(&mut app);
        app.browse_archive_repackage = Some(ArchiveMetadataEditContext::browse(
            archive.clone(),
            staging.clone(),
        ));
        app.browse_archive_repackage_progress_session_id = Some(archive_session_id);
        let tx = tx();

        reduce_file_task_progress(
            &mut app,
            transfer_session_id,
            tui_file_picker::FileTaskProgressUpdate::Failed {
                status: "transfer failed".to_string(),
                totals: tui_file_picker::ProgressTotals {
                    errors: 1,
                    ..tui_file_picker::ProgressTotals::default()
                },
            },
            &tx,
        );
        finalize_file_transfer_scheduler(&mut app, transfer_session_id, true, &tx);

        handle_archive_repackage_result(
            &mut app,
            archive.clone(),
            staging.clone(),
            archive_session_id,
            Err("synthetic archive failure".to_string()),
            &tx,
        );
        assert!(matches!(
            app.file_task_preempted_overlay.as_deref(),
            Some(ActiveOverlay::Confirmation {
                action: ConfirmAction::ArchiveRepackageFailure { context, error },
                ..
            }) if context.archive_path == archive
                && context.staging_dir == staging
                && error == "synthetic archive failure"
        ));

        super::super::keybindings::handle_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ),
            &tx,
        );
        assert!(matches!(
            &app.active_overlay,
            ActiveOverlay::Confirmation {
                action: ConfirmAction::ArchiveRepackageFailure { context, error },
                ..
            } if context.archive_path == archive
                && context.staging_dir == staging
                && error == "synthetic archive failure"
        ));
        assert!(!app.file_transfers.blocked_for_attention);
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
            row_scope: crate::tui::probe::RowScope::File,
            display_key: display_key.to_string(),
            item_key: ItemKey::TrackTitle,
            value: value.to_string(),
            original: value.to_string(),
            is_binary: false,
            is_mixed: false,
            has_multiple_stored_values: false,
            per_file_stored_value_counts: Vec::new(),
            per_file_values: crate::tui::probe::metadata_field_values_from_scalars(vec![value.to_string(); n_paths]),
            per_file_originals: crate::tui::probe::metadata_field_values_from_scalars(vec![value.to_string(); n_paths]),
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


    fn fixture_tool_available(tool: &str) -> bool {
        std::process::Command::new(tool)
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn fixture_cue(stem: &str, album_title: &str, side_prefix: &str, n_tracks: usize) -> String {
        let mut cue = format!(
            "PERFORMER \"Pink Floyd\"\nTITLE \"{album_title}\"\nFILE \"{stem}.flac\" WAVE\n"
        );
        for track in 1..=n_tracks {
            let total_seconds = (track - 1) * 30;
            let minutes = total_seconds / 60;
            let seconds = total_seconds % 60;
            cue.push_str(&format!(
                "  TRACK {track:02} AUDIO\n    TITLE \"{side_prefix} Track {track}\"\n    INDEX 01 {minutes:02}:{seconds:02}:00\n"
            ));
        }
        cue
    }

    fn dsotm_release() -> crate::tui::musicbrainz::MbRelease {
        crate::tui::musicbrainz::MbRelease {
            release_id: "mb-dsotm".to_string(),
            title: "The Dark Side Of The Moon".to_string(),
            artist_values: vec!["Pink Floyd".to_string()],
            artist: "Pink Floyd".to_string(),
            disc_count: 1,
            tracks: (1..=10)
                .map(|position| crate::tui::musicbrainz::MbTrack {
                    position,
                    title: format!("MB Track {position}"),
                    artist: vec!["Pink Floyd".to_string()],
                    ..Default::default()
                })
                .collect(),
            relationship_projection_complete: true,
            ..Default::default()
        }
    }

    fn dsotm_unified_editor_with_track_sources(
    ) -> Option<(tempfile::TempDir, Box<MetadataEditorState>, Vec<std::path::PathBuf>)> {
        if !fixture_tool_available("ffmpeg") {
            eprintln!("skipping unified MusicBrainz completion regression: ffmpeg unavailable");
            return None;
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let album = temp.path().join("dsotm");
        std::fs::create_dir_all(&album).expect("album dir");
        for stem in ["tdsotm_a", "tdsotm_b"] {
            let image = album.join(format!("{stem}.flac"));
            let status = std::process::Command::new("ffmpeg")
                .args([
                    "-y",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:sample_rate=44100:duration=1",
                    "-c:a",
                    "flac",
                ])
                .arg(&image)
                .stdin(std::process::Stdio::null())
                .status()
                .expect("ffmpeg fixture");
            assert!(status.success(), "ffmpeg fixture failed for {}", image.display());
        }
        std::fs::write(
            album.join("tdsotm_a.cue"),
            fixture_cue("tdsotm_a", "The Dark Side Of The Moon (Side A)", "A", 5),
        )
        .expect("side a cue");
        std::fs::write(
            album.join("tdsotm_b.cue"),
            fixture_cue("tdsotm_b", "The Dark Side Of The Moon (Side B)", "B", 5),
        )
        .expect("side b cue");

        let mut builder_app = AppState::new_for_test(TonepoetConfig::default());
        let state = super::super::keybindings::build_metadata_editor_for_cue_surfaces_with_mb_release(
            &mut builder_app,
            &[album],
            &dsotm_release(),
        )
        .expect("production unified CUE builder should not fail")
        .expect("DSOTM fixture should produce a unified CUE surface");
        assert!(
            state.presentation_tabs.is_empty(),
            "unified fixture must use the production single-surface model"
        );
        let sheet = state
            .active_surface()
            .cue_album_synthetic_sheet
            .as_ref()
            .expect("production builder must attach a unified synthetic sheet");
        assert_eq!(sheet.track_sources.len(), 10);
        let track_paths: Vec<_> = sheet
            .track_sources
            .iter()
            .map(|source| source.audio_path.clone())
            .collect();
        Some((temp, state, track_paths))
    }



    fn single_source_file_editor() -> Box<MetadataEditorState> {
        Box::new(MetadataEditorState::for_files(
            vec![std::path::PathBuf::from("/tmp/split-cue-side-a.flac")],
            vec![tag("TITLE", "Side A", 1)],
            vec!["Side A".to_string()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        ))
    }

    fn release(id: &str, title: &str) -> crate::tui::musicbrainz::MbRelease {
        crate::tui::musicbrainz::MbRelease {
            release_id: id.to_string(),
            title: title.to_string(),
            relationship_projection_complete: true,
            ..Default::default()
        }
    }

    fn lookup_outcome(
        releases: Vec<crate::tui::musicbrainz::MbRelease>,
    ) -> crate::tui::musicbrainz::MbCascadeOutcome {
        crate::tui::musicbrainz::MbCascadeOutcome {
            releases,
            matched: None,
            dropped_source_indices: Vec::new(),
            cache_writes: Vec::new(),
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

    fn ctx_for(
        app: &mut AppState,
        paths: Vec<std::path::PathBuf>,
        editor_park: bool,
    ) -> crate::tui::message::TagsMbContext {
        let operation_id = begin_tags_mb_lookup_operation(app, editor_park)
            .expect("test lookup operation identity");
        crate::tui::message::TagsMbContext {
            operation_id,
            paths,
            editor_park,
            fallback_seed: None,
            editor_session: None,
        }
    }

    fn session_guard_for(
        state: &MetadataEditorState,
    ) -> crate::tui::message::MetadataEditorSessionGuard {
        let details = &state.active_surface().technical_details;
        crate::tui::message::MetadataEditorSessionGuard {
            session_id: details.session_id,
            save_generation: details.save_generation,
            editor_generation: state.model.editor_save_generation,
        }
    }

    fn ctx_for_session(
        app: &mut AppState,
        paths: Vec<std::path::PathBuf>,
        editor_park: bool,
        editor_session: crate::tui::message::MetadataEditorSessionGuard,
    ) -> crate::tui::message::TagsMbContext {
        let operation_id = begin_tags_mb_lookup_operation(app, editor_park)
            .expect("test lookup operation identity");
        crate::tui::message::TagsMbContext {
            operation_id,
            paths,
            editor_park,
            fallback_seed: None,
            editor_session: Some(editor_session),
        }
    }

    fn accept_current_two_match_lookup(
        app: &mut AppState,
        tx: &mpsc::Sender<AppMessage>,
        editor_paths: Vec<std::path::PathBuf>,
    ) -> PendingTagsMbApply {
        let ctx = ctx_for(app, editor_paths, true);
        let lookup_operation_id = ctx.operation_id;
        handle_message(
            app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Toc {
                    outcome: Ok(lookup_outcome(vec![
                        release("b-a", "B Alternate"),
                        release("b-b", "B Chosen"),
                    ])),
                },
                ctx,
            },
            tx,
        );
        let ActiveOverlay::MbSelect(mut picker) =
            std::mem::replace(&mut app.active_overlay, ActiveOverlay::None)
        else {
            panic!("current lookup must open a picker");
        };
        picker.selected = 1;
        let pending = start_mb_select_apply_operation(app, picker)
            .expect("current picker acceptance must enter verification");
        assert_eq!(pending.operation_id, lookup_operation_id);
        pending
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
        let ctx = ctx_for(&mut app, Vec::new(), false);
        let msg = AppMessage::TagsFromMbComplete {
            outcome: crate::tui::message::MbOutcome::Toc {
                outcome: Err("synthetic lookup failure".to_string()),
            },
            ctx,
        };

        handle_message(&mut app, msg, &tx);

        let status = app.status_message.as_ref().map(|(msg, _)| msg.as_str());
        assert_eq!(
            status,
            Some(":tags-mb: TOC lookup failed: synthetic lookup failure")
        );
    }

    #[test]
    fn musicbrainz_apply_completion_preserves_provider_cardinality_warning() {
        let editor_paths = paths(2);
        let mut artist = tag("ARTIST", "<multiple values>", 2);
        artist.item_key = ItemKey::TrackArtist;
        artist.is_mixed = true;
        artist.has_multiple_stored_values = true;
        artist.per_file_stored_value_counts = vec![2, 1];
        artist.per_file_values = crate::tui::probe::metadata_field_values_from_scalars(vec!["Alpha; Beta".to_string(), "Gamma".to_string()]);
        artist.per_file_originals = artist.per_file_values.clone();

        let editor = Box::new(MetadataEditorState::for_files(
            editor_paths.clone(),
            vec![artist],
            vec!["Track 01".to_string(), "Track 02".to_string()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        ));
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);
        let operation_id = begin_tags_mb_lookup_operation(&mut app, true)
            .expect("MusicBrainz operation should start");
        let release = crate::tui::musicbrainz::MbRelease {
            release_id: "provider-cardinality".to_string(),
            title: "Replacement Album".to_string(),
            artist_values: vec!["Replacement Artist".to_string()],
            artist: "Replacement Artist".to_string(),
            tracks: vec![
                crate::tui::musicbrainz::MbTrack {
                    position: 1,
                    title: "Track 01".to_string(),
                    artist: vec!["New Artist".to_string()],
                    ..Default::default()
                },
                crate::tui::musicbrainz::MbTrack {
                    position: 2,
                    title: "Track 02".to_string(),
                    artist: vec!["New Scalar".to_string()],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let tx = tx();

        complete_tags_mb_apply_operation(
            &mut app,
            &tx,
            operation_id,
            vec![release],
            0,
            editor_paths,
            None,
            crate::tui::musicbrainz::PerTrackDecision::default(),
        );

        let status = app
            .status_message
            .as_ref()
            .map(|(message, _)| message.as_str())
            .unwrap_or("");
        assert!(status.contains("MusicBrainz populated"));
        assert!(status.contains("warning: 1 carrier"));
        let ActiveOverlay::MetadataEditor(state) = &app.active_overlay else {
            panic!("MusicBrainz completion must reopen the metadata editor");
        };
        let artist = state
            .active_surface()
            .entries
            .iter()
            .find(|entry| entry.display_key == "ARTIST")
            .expect("ARTIST row");
        assert_eq!(artist.per_file_values, vec!["New Artist", "New Scalar"]);
    }

    #[test]
    fn incomplete_multi_medium_release_is_refused_before_editor_mutation() {
        let editor_paths = paths(2);
        let mut editor = Box::new(MetadataEditorState::for_files(
            editor_paths.clone(),
            vec![tag("TITLE", "Original", 2)],
            vec!["Track 01".to_string(), "Track 02".to_string()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        ));
        editor.model.tags_mb_in_flight = true;
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);
        let release = crate::tui::musicbrainz::MbRelease {
            release_id: "truncated-2lp".to_string(),
            title: "Double Album".to_string(),
            tracks: vec![crate::tui::musicbrainz::MbTrack {
                position: 1,
                title: "Replacement".to_string(),
                ..Default::default()
            }],
            track_parse_error: Some(
                "MusicBrainz advertised 2 tracks but only 1 parsed".to_string(),
            ),
            ..Default::default()
        };

        apply_editor_with_mb_release_decision_guarded(
            &mut app,
            &tx(),
            vec![release],
            0,
            editor_paths,
            None,
            crate::tui::musicbrainz::PerTrackDecision::default(),
        );

        let ActiveOverlay::MetadataEditor(state) = &app.active_overlay else {
            panic!("incomplete MB data must leave the editor open");
        };
        let title = state
            .active_surface()
            .entries
            .iter()
            .find(|entry| entry.display_key.eq_ignore_ascii_case("TITLE"))
            .expect("TITLE row");
        assert_eq!(
            title.per_file_values,
            vec!["Original".to_string(), "Original".to_string()]
        );
        assert!(!state.model.tags_mb_in_flight);
        assert!(app
            .status_message
            .as_ref()
            .is_some_and(|(message, _)| message.contains("refusing incomplete release data")));
    }

    #[test]
    fn multi_match_completion_parks_open_editor_and_restores_it_on_cancel_path() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        let mut editor = editor_with_tabs(0, 2);
        editor.model.tags_mb_in_flight = true;
        let editor_paths = editor.active_surface().paths.clone();
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);

        let ctx = ctx_for(&mut app, editor_paths.clone(), true);
        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Toc {
                    outcome: Ok(lookup_outcome(vec![
                        release("", "Candidate A"),
                        release("", "Candidate B"),
                    ])),
                    },
                ctx,
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
        assert!(
            app.pending_metadata_editor
                .as_ref()
                .is_some_and(|editor| editor.model.tags_mb_in_flight),
            "the :tags-mb latch must remain held for the complete picker lifecycle"
        );

        restore_parked_editor(&mut app);

        assert!(app.pending_metadata_editor.is_none());
        match &app.active_overlay {
            ActiveOverlay::MetadataEditor(state) => {
                assert_eq!(state.presentation_tabs.len(), 2);
                assert_eq!(state.active_tab, 0);
                assert!(
                    !state.model.tags_mb_in_flight,
                    "picker cancellation is the terminal operation phase and must release the latch"
                );
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

        let ctx = ctx_for(&mut app, editor_paths, true);
        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Toc {
                    outcome: Ok(lookup_outcome(vec![release("", "One Match Album")])),
                    },
                ctx,
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

    #[tokio::test]
    async fn toc_single_match_resolves_composer_through_cached_release_detail() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let mut editor = single_source_file_editor();
        editor.model.tags_mb_in_flight = true;
        let editor_paths = editor.active_surface().paths.clone();
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);

        let release_id = "toc-single-composer";
        let detail_body = r#"{
            "id": "toc-single-composer",
            "title": "Album",
            "artist-credit": [{"artist": {"id": "artist-1", "name": "Performer"}}],
            "media": [{
                "track-count": 1,
                "tracks": [{
                    "position": 1,
                    "title": "Movement I",
                    "recording": {
                        "id": "recording-1",
                        "relations": [{
                            "type": "performance",
                            "target-type": "work",
                            "work": {"relations": [{
                                "type": "composer",
                                "artist": {"name": "World Class Composer"}
                            }]}
                        }]
                    }
                }]
            }]
        }"#;
        app.db
            .store_mb_search(
                &crate::tui::musicbrainz::detail_cache_key(release_id),
                detail_body,
            )
            .expect("seed release-detail cache");

        let mut shallow = release(release_id, "Album");
        shallow.relationship_projection_complete = false;
        let ctx = ctx_for(&mut app, editor_paths, true);
        let (tx, mut rx) = mpsc::channel(4);

        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Toc {
                    outcome: Ok(lookup_outcome(vec![shallow])),
                },
                ctx,
            },
            &tx,
        );

        let detail_completion =
            tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .expect("cached release detail should resolve promptly")
        .expect("release-detail completion message");
        assert!(matches!(
            detail_completion,
            AppMessage::MbSelectedDetailComplete { .. }
        ));
        handle_message(&mut app, detail_completion, &tx);

        let ActiveOverlay::MetadataEditor(state) = &app.active_overlay else {
            panic!("single TOC match should reopen the populated metadata editor");
        };
        let composer = state
            .active_surface()
            .entries
            .iter()
            .find(|entry| entry.display_key == "COMPOSER")
            .expect("composer row populated from release detail");
        assert_eq!(composer.per_file_values, ["World Class Composer"]);
    }

    #[test]
    fn search_single_match_completion_uses_same_apply_all_handoff() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        let editor = editor_with_tabs(0, 2);
        let editor_paths = editor.active_surface().paths.clone();
        // Editor-owned completions now require a session guard; without one
        // the reducer treats the completion as stale and restores the editor.
        let guard = session_guard_for(&editor);
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);

        let ctx = ctx_for_session(&mut app, editor_paths, true, guard);
        // Production only dispatches a Search completion after the TOC
        // zero-match fallback advanced the phase to LookupTextFallback.
        assert!(transition_tags_mb_operation_phase(
            &mut app,
            ctx.operation_id,
            crate::tui::app::TagsMbOperationPhase::Lookup,
            crate::tui::app::TagsMbOperationPhase::LookupTextFallback,
        ));
        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Search {
                    outcome: Ok(search_outcome(vec![release("", "Search Match Album")])),
                    query_label: "artist / album".to_string(),
                },
                ctx,
            },
            &tx,
        );

        let state = assert_apply_all_confirmation(&app);
        assert_eq!(state.presentation_tabs.len(), 2);
        assert_eq!(state.active_tab, 0);
        assert!(app.pending_metadata_editor.is_some());
    }

    #[tokio::test]
    async fn unified_track_dimension_paths_match_live_editor_completion_picker() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        let Some((_fixture, editor, track_paths)) = dsotm_unified_editor_with_track_sources() else {
            return;
        };
        let guard = session_guard_for(&editor);
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);

        let ctx = ctx_for_session(&mut app, track_paths.clone(), true, guard);
        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Toc {
                    outcome: Ok(lookup_outcome(vec![
                        release("candidate-a", "Candidate A"),
                        release("candidate-b", "Candidate B"),
                    ])),
                },
                ctx,
            },
            &tx,
        );

        match &app.active_overlay {
            ActiveOverlay::MbSelect(state) => {
                assert_eq!(state.releases.len(), 2);
                assert_eq!(state.paths, track_paths);
                assert_eq!(state.editor_session, Some(guard));
            }
            other => panic!(
                "valid unified track-dimension completion should open picker, got {:?}",
                other
            ),
        }
        assert!(
            app.pending_metadata_editor.is_some(),
            "live unified editor should be parked while the picker is open"
        );
    }

    #[test]
    fn unified_track_dimension_paths_match_live_editor_direct_apply() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        let Some((_fixture, editor, track_paths)) = dsotm_unified_editor_with_track_sources() else {
            return;
        };
        let guard = session_guard_for(&editor);
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);

        apply_editor_with_mb_release_decision_guarded(
            &mut app,
            &tx,
            vec![release("candidate-a", "Candidate A")],
            0,
            track_paths,
            Some(guard),
            crate::tui::musicbrainz::PerTrackDecision::default(),
        );

        match &app.active_overlay {
            ActiveOverlay::MetadataEditor(state) => {
                assert_eq!(session_guard_for(state).session_id, guard.session_id);
            }
            other => panic!(
                "valid unified track-dimension direct apply should keep editor active, got {:?}",
                other
            ),
        }
        let status = app.status_message.as_ref().map(|(msg, _)| msg.as_str());
        assert!(
            status.unwrap_or_default().contains(":tags-mb: applied"),
            "valid unified direct apply must not be rejected as stale, got {:?}",
            status
        );
    }

    #[tokio::test]
    async fn single_source_folder_projection_parks_source_editor_with_target_paths() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        let editor = single_source_file_editor();
        let guard = session_guard_for(&editor);
        let source_paths = editor.active_surface().paths.clone();
        let target_paths = vec![
            source_paths[0].clone(),
            source_paths[0].clone(),
            std::path::PathBuf::from("/tmp/split-cue-side-b.flac"),
            std::path::PathBuf::from("/tmp/split-cue-side-b.flac"),
        ];
        assert!(
            !metadata_editor_paths_match_tags_mb_context(&editor, &target_paths),
            "the legacy source editor must not be required to have the expanded target paths"
        );
        assert!(
            metadata_editor_can_transition_to_split_cue_target(&editor, &target_paths),
            "one-file source editors should be eligible for the split-CUE target transition"
        );
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);

        let ctx = ctx_for_session(&mut app, target_paths.clone(), true, guard);
        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Toc {
                    outcome: Ok(lookup_outcome(vec![
                        release("candidate-a", "Candidate A"),
                        release("candidate-b", "Candidate B"),
                    ])),
                },
                ctx,
            },
            &tx,
        );

        match &app.active_overlay {
            ActiveOverlay::MbSelect(state) => {
                assert_eq!(state.paths, target_paths);
                assert_eq!(state.editor_session, Some(guard));
            }
            other => panic!(
                "valid source-folder transition should open the picker, got {:?}",
                other
            ),
        }
        let parked = app
            .pending_metadata_editor
            .as_ref()
            .expect("source editor should be parked behind the picker");
        assert_eq!(session_guard_for(parked), guard);
    }

    #[test]
    fn stale_editor_session_completion_does_not_mutate_reopened_editor() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        let old_editor = editor_with_tabs(0, 2);
        let editor_paths = old_editor.active_surface().paths.clone();
        let old_guard = session_guard_for(&old_editor);

        let reopened_editor = editor_with_tabs(0, 2);
        let reopened_guard = session_guard_for(&reopened_editor);
        assert_ne!(
            old_guard.session_id, reopened_guard.session_id,
            "reopened metadata editors must have distinct async session ids"
        );
        app.active_overlay = ActiveOverlay::MetadataEditor(reopened_editor);

        let ctx = ctx_for_session(&mut app, editor_paths, true, old_guard);
        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Toc {
                    outcome: Ok(lookup_outcome(vec![release("", "Stale Match")])),
                },
                ctx,
            },
            &tx,
        );

        match &app.active_overlay {
            ActiveOverlay::MetadataEditor(state) => {
                assert_eq!(
                    session_guard_for(state).session_id,
                    reopened_guard.session_id,
                    "stale completion must restore the current reopened editor, not replace it"
                );
            }
            other => panic!("expected reopened editor to remain active, got {:?}", other),
        }
        assert!(
            app.pending_metadata_editor.is_none(),
            "stale completion must not park or populate the reopened editor"
        );
        let status = app.status_message.as_ref().map(|(msg, _)| msg.as_str());
        assert!(
            status
                .unwrap_or_default()
                .contains("metadata editor changed since lookup"),
            "stale completion should explain that the editor session changed, got {:?}",
            status
        );
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

        let tx = tx();
        open_editor_with_mb_release(
            &mut app,
            &tx,
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

    #[tokio::test]
    async fn unassigned_picker_refusal_restores_editor_without_finishing_foreign_operation() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let mut editor = single_source_file_editor();
        editor.model.tags_mb_in_flight = true;
        let editor_paths = editor.active_surface().paths.clone();
        app.pending_metadata_editor = Some(editor);
        let foreign_operation = begin_tags_mb_lookup_operation(&mut app, true)
            .expect("foreign lookup operation");

        let picker = Box::new(crate::tui::app::MbSelectState::new(
            vec![release("old-a", "Old A"), release("old-b", "Old B")],
            editor_paths,
        ));
        assert!(!picker.operation_id.is_assigned());

        assert!(
            start_mb_select_apply_operation(&mut app, picker).is_none(),
            "an UNASSIGNED picker must refuse while foreign authority is live"
        );
        assert_eq!(
            app.active_tags_mb_operation.map(|active| active.operation_id),
            Some(foreign_operation),
            "the refusing picker must not finish the operation that caused its refusal"
        );
        let ActiveOverlay::MetadataEditor(restored) = &app.active_overlay else {
            panic!("the parked editor must be restored after refusal");
        };
        assert!(restored.model.tags_mb_in_flight);
        assert!(app
            .status_message
            .as_ref()
            .is_some_and(|(message, _)| message.contains("workflow is active")));
    }

    #[tokio::test]
    async fn stale_gnudb_completion_is_total_noop_over_live_mb_picker() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        let editor = single_source_file_editor();
        let editor_paths = editor.active_surface().paths.clone();
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);
        let ctx = ctx_for(&mut app, editor_paths.clone(), true);
        let mb_operation = ctx.operation_id;
        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Toc {
                    outcome: Ok(lookup_outcome(vec![
                        release("mb-current-a", "Current A"),
                        release("mb-current-b", "Current B"),
                    ])),
                },
                ctx,
            },
            &tx,
        );
        app.set_status("current MusicBrainz picker remains authoritative".to_string());
        let status_before = app.status_message.clone();

        handle_message(
            &mut app,
            AppMessage::GnudbQueryComplete {
                operation_id: crate::tui::message::TagsMbOperationId(u64::MAX),
                result: Ok(Vec::new()),
                paths: editor_paths,
            },
            &tx,
        );

        assert_eq!(
            app.active_tags_mb_operation.map(|active| active.operation_id),
            Some(mb_operation)
        );
        let ActiveOverlay::MbSelect(picker) = &app.active_overlay else {
            panic!("stale GNUDB completion must not replace the live MB picker");
        };
        assert_eq!(picker.operation_id, mb_operation);
        assert_eq!(picker.releases[0].release_id, "mb-current-a");
        assert_eq!(app.status_message, status_before);
        assert!(app.pending_metadata_editor.is_some());
    }

    #[tokio::test]
    async fn duplicate_toc_zero_match_dispatches_text_fallback_once() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::MetadataEditor(single_source_file_editor());
        let operation_id = begin_tags_mb_lookup_operation(&mut app, true)
            .expect("lookup operation");
        let context = crate::tui::message::TagsMbContext {
            operation_id,
            paths: paths(1),
            editor_park: true,
            fallback_seed: Some(crate::tui::command::SacdMbSeed {
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                catalog: None,
                year: None,
            }),
            editor_session: None,
        };
        let (tx, _rx) = mpsc::channel(4);
        let message = || AppMessage::TagsFromMbComplete {
            outcome: crate::tui::message::MbOutcome::Toc {
                outcome: Ok(lookup_outcome(Vec::new())),
            },
            ctx: context.clone(),
        };

        handle_message(&mut app, message(), &tx);
        assert!(tags_mb_operation_is_current_phase(
            &app,
            operation_id,
            crate::tui::app::TagsMbOperationPhase::LookupTextFallback,
        ));
        let generation_after_first = app.tags_mb_operation_generation;
        let status_after_first = app.status_message.clone();

        handle_message(&mut app, message(), &tx);
        assert_eq!(app.tags_mb_operation_generation, generation_after_first);
        assert!(tags_mb_operation_is_current_phase(
            &app,
            operation_id,
            crate::tui::app::TagsMbOperationPhase::LookupTextFallback,
        ));
        assert_eq!(
            app.status_message, status_after_first,
            "duplicate TOC completion must not dispatch or report another fallback"
        );
    }

    #[test]
    fn picker_accept_promotes_prefetched_relationship_detail_and_cancels_debounce() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let mut editor = single_source_file_editor();
        editor.model.tags_mb_in_flight = true;
        let editor_paths = editor.active_surface().paths.clone();
        app.pending_metadata_editor = Some(editor);

        let mut shallow = release("prefetch-id", "Shallow search row");
        shallow.relationship_projection_complete = false;
        let mut detail = shallow.clone();
        detail.title = "Authoritative release detail".to_string();
        detail.relationship_projection_complete = true;
        detail.tracks = vec![crate::tui::musicbrainz::MbTrack {
            position: 1,
            title: "Movement I".to_string(),
            composer: vec!["Picker Composer".to_string()],
            ..Default::default()
        }];

        let mut picker = Box::new(crate::tui::app::MbSelectState::new(
            vec![shallow],
            editor_paths,
        ));
        picker.prefetch.insert(detail.release_id.clone(), detail.clone());
        let generation_before = picker
            .generation
            .load(std::sync::atomic::Ordering::Relaxed);

        let pending = start_mb_select_apply_operation(&mut app, picker)
            .expect("picker acceptance should enter verification");

        assert_eq!(pending.releases.len(), 1);
        assert_eq!(pending.releases[0].title, detail.title);
        assert!(pending.releases[0].relationship_projection_complete);
        assert_eq!(
            pending.releases[0].tracks[0].composer,
            ["Picker Composer"]
        );
        let ActiveOverlay::MbSelect(state) = &app.active_overlay else {
            panic!("accepted picker should remain visible while verification runs");
        };
        assert!(matches!(
            state.phase,
            crate::tui::app::MbSelectPhase::Verifying { .. }
        ));
        assert!(
            state
                .generation
                .load(std::sync::atomic::Ordering::Relaxed)
                > generation_before,
            "acceptance must invalidate a still-debouncing speculative prefetch"
        );
    }

    #[test]
    fn older_mb_apply_completion_cannot_overwrite_newer_picker_selection() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let mut editor = single_source_file_editor();
        editor.model.tags_mb_in_flight = true;
        let editor_paths = editor.active_surface().paths.clone();
        app.pending_metadata_editor = Some(editor);

        let first_state = Box::new(crate::tui::app::MbSelectState::new(
            vec![release("a1", "Selection A"), release("a2", "Selection A alt")],
            editor_paths.clone(),
        ));
        let first = start_mb_select_apply_operation(&mut app, first_state)
            .expect("first selection should acquire an operation ID");

        app.active_overlay = ActiveOverlay::None;
        restore_parked_editor(&mut app);
        let ActiveOverlay::MetadataEditor(mut restored) =
            std::mem::replace(&mut app.active_overlay, ActiveOverlay::None)
        else {
            panic!("cancelling the first selection must restore the editor");
        };
        restored.model.tags_mb_in_flight = true;
        app.pending_metadata_editor = Some(restored);

        let mut second_state = Box::new(crate::tui::app::MbSelectState::new(
            vec![release("b1", "Selection B alt"), release("b2", "Selection B")],
            editor_paths.clone(),
        ));
        second_state.selected = 1;
        let second = start_mb_select_apply_operation(&mut app, second_state)
            .expect("newer selection should acquire a new operation ID");
        assert_ne!(first.operation_id, second.operation_id);

        complete_tags_mb_apply_operation(
            &mut app,
            &tx(),
            first.operation_id,
            first.releases.clone(),
            first.selected,
            first.paths.clone(),
            first.editor_session,
            crate::tui::musicbrainz::PerTrackDecision::default(),
        );

        assert_eq!(
            app.active_tags_mb_operation.map(|active| active.operation_id),
            Some(second.operation_id),
            "an older completion must not release or replace the newer operation"
        );
        assert!(matches!(
            &app.active_overlay,
            ActiveOverlay::MbSelect(state)
                if state.phase.verifying_operation() == Some(second.operation_id)
        ));

        complete_tags_mb_apply_operation(
            &mut app,
            &tx(),
            second.operation_id,
            second.releases,
            second.selected,
            second.paths,
            second.editor_session,
            crate::tui::musicbrainz::PerTrackDecision::default(),
        );

        let ActiveOverlay::MetadataEditor(state) = &app.active_overlay else {
            panic!("newer valid completion must restore the metadata editor");
        };
        let back = state.mb_back.as_ref().expect("picker selection should cache :mb-back state");
        assert_eq!(back.selected, 1);
        assert_eq!(back.releases[1].release_id, "b2");
    }

    #[test]
    fn picker_replacement_immediately_invalidates_selected_release_operation() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let mut editor = single_source_file_editor();
        editor.model.tags_mb_in_flight = true;
        let editor_paths = editor.active_surface().paths.clone();
        app.pending_metadata_editor = Some(editor);

        let picker = Box::new(crate::tui::app::MbSelectState::new(
            vec![release("replace-a", "Selection A"), release("replace-b", "Selection B")],
            editor_paths,
        ));
        let pending = start_mb_select_apply_operation(&mut app, picker)
            .expect("selection should acquire an operation ID");

        app.active_overlay = ActiveOverlay::Verify { scroll: 0 };
        reconcile_tags_mb_apply_operation_state(&mut app);

        assert!(app.active_tags_mb_operation.is_none());
        assert!(matches!(app.active_overlay, ActiveOverlay::Verify { scroll: 0 }));
        assert!(app
            .pending_metadata_editor
            .as_ref()
            .is_some_and(|state| !state.model.tags_mb_in_flight));

        complete_tags_mb_apply_operation(
            &mut app,
            &tx(),
            pending.operation_id,
            pending.releases,
            pending.selected,
            pending.paths,
            pending.editor_session,
            crate::tui::musicbrainz::PerTrackDecision::default(),
        );

        assert!(matches!(app.active_overlay, ActiveOverlay::Verify { scroll: 0 }));
        let editor = app
            .pending_metadata_editor
            .as_ref()
            .expect("replacement must leave the parked editor recoverable");
        let entries = &editor.active_surface().entries;
        assert!(
            entries.iter().all(|entry| entry.display_key != "ALBUM"),
            "stale completion must not create an ALBUM proposal"
        );
        let title = entries
            .iter()
            .find(|entry| entry.display_key == "TITLE")
            .expect("original TITLE row");
        assert_eq!(title.value, "Side A");
        assert!(title.mb_proposed_value.is_none());
    }

    #[tokio::test]
    async fn stale_lookup_error_after_accepted_newer_lookup_cannot_cancel_it() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        let editor = single_source_file_editor();
        let editor_paths = editor.active_surface().paths.clone();
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);

        let stale_ctx = ctx_for(&mut app, editor_paths.clone(), false);
        let pending = accept_current_two_match_lookup(&mut app, &tx, editor_paths);
        app.set_status(":tags-mb: B verification remains authoritative".to_string());
        let status_before = app.status_message.as_ref().map(|(message, _)| message.clone());

        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Toc {
                    outcome: Err("late A failure".to_string()),
                },
                ctx: stale_ctx,
            },
            &tx,
        );

        assert_eq!(
            app.active_tags_mb_operation.map(|active| active.operation_id),
            Some(pending.operation_id)
        );
        assert!(matches!(
            &app.active_overlay,
            ActiveOverlay::MbSelect(state)
                if state.phase.verifying_operation() == Some(pending.operation_id)
        ));
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.clone()),
            status_before,
            "a stale lookup error must not overwrite the current status"
        );

        complete_tags_mb_apply_operation(
            &mut app,
            &tx,
            pending.operation_id,
            pending.releases,
            pending.selected,
            pending.paths,
            pending.editor_session,
            crate::tui::musicbrainz::PerTrackDecision::default(),
        );
        let ActiveOverlay::MetadataEditor(state) = &app.active_overlay else {
            panic!("the accepted newer lookup must still complete");
        };
        assert_eq!(
            state.mb_back.as_ref().map(|cache| cache.selected),
            Some(1)
        );
    }

    #[tokio::test]
    async fn stale_lookup_zero_result_after_accepted_newer_lookup_cannot_cancel_it() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        let editor = single_source_file_editor();
        let editor_paths = editor.active_surface().paths.clone();
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);

        let stale_ctx = ctx_for(&mut app, editor_paths.clone(), false);
        let pending = accept_current_two_match_lookup(&mut app, &tx, editor_paths);
        app.set_status(":tags-mb: B verification remains authoritative".to_string());
        let status_before = app.status_message.as_ref().map(|(message, _)| message.clone());

        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Toc {
                    outcome: Ok(lookup_outcome(Vec::new())),
                },
                ctx: stale_ctx,
            },
            &tx,
        );

        assert_eq!(
            app.active_tags_mb_operation.map(|active| active.operation_id),
            Some(pending.operation_id)
        );
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.clone()),
            status_before
        );

        complete_tags_mb_apply_operation(
            &mut app,
            &tx,
            pending.operation_id,
            pending.releases,
            pending.selected,
            pending.paths,
            pending.editor_session,
            crate::tui::musicbrainz::PerTrackDecision::default(),
        );
        assert!(matches!(app.active_overlay, ActiveOverlay::MetadataEditor(_)));
    }

    #[tokio::test]
    async fn stale_single_match_lookup_cannot_replace_accepted_newer_operation() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        let editor = single_source_file_editor();
        let editor_paths = editor.active_surface().paths.clone();
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);

        let stale_ctx = ctx_for(&mut app, editor_paths.clone(), false);
        let pending = accept_current_two_match_lookup(&mut app, &tx, editor_paths);

        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Toc {
                    outcome: Ok(lookup_outcome(vec![release("a-only", "Stale A")])),
                },
                ctx: stale_ctx,
            },
            &tx,
        );

        assert_eq!(
            app.active_tags_mb_operation.map(|active| active.operation_id),
            Some(pending.operation_id),
            "a stale single match must not allocate or replace application authority"
        );
        assert!(matches!(
            &app.active_overlay,
            ActiveOverlay::MbSelect(state)
                if state.phase.verifying_operation() == Some(pending.operation_id)
        ));
    }

    #[tokio::test]
    async fn stale_multi_match_lookup_cannot_replace_current_picker() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        let editor = single_source_file_editor();
        let editor_paths = editor.active_surface().paths.clone();
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);

        let stale_ctx = ctx_for(&mut app, editor_paths.clone(), false);
        let current_ctx = ctx_for(&mut app, editor_paths, true);
        let current_id = current_ctx.operation_id;
        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Toc {
                    outcome: Ok(lookup_outcome(vec![
                        release("b1", "Current B1"),
                        release("b2", "Current B2"),
                    ])),
                },
                ctx: current_ctx,
            },
            &tx,
        );
        let status_before = app.status_message.as_ref().map(|(message, _)| message.clone());

        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Toc {
                    outcome: Ok(lookup_outcome(vec![
                        release("a1", "Stale A1"),
                        release("a2", "Stale A2"),
                    ])),
                },
                ctx: stale_ctx,
            },
            &tx,
        );

        let ActiveOverlay::MbSelect(state) = &app.active_overlay else {
            panic!("stale multi-match completion must not dismiss the current picker");
        };
        assert_eq!(state.operation_id, current_id);
        assert_eq!(state.releases[0].release_id, "b1");
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.clone()),
            status_before
        );
    }

    #[tokio::test]
    async fn stale_browse_completion_cannot_interfere_with_editor_originated_operation() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        let editor = single_source_file_editor();
        let editor_paths = editor.active_surface().paths.clone();
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);

        let stale_browse_ctx = ctx_for(&mut app, editor_paths.clone(), false);
        let pending = accept_current_two_match_lookup(&mut app, &tx, editor_paths);

        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Search {
                    outcome: Err("late browse search failure".to_string()),
                    query_label: "stale browse".to_string(),
                },
                ctx: stale_browse_ctx,
            },
            &tx,
        );

        assert_eq!(
            app.active_tags_mb_operation.map(|active| active.operation_id),
            Some(pending.operation_id)
        );
        assert!(app
            .pending_metadata_editor
            .as_ref()
            .is_some_and(|editor| editor.model.tags_mb_in_flight));
    }

    fn split_cue_info_for_mb_operation(
        path: &str,
        cue: &str,
    ) -> crate::tui::cue_parser::SingleImageInfo {
        crate::tui::cue_parser::SingleImageInfo {
            audio_path: std::path::PathBuf::from(path),
            cue_path: std::path::PathBuf::from(cue),
            sheet: crate::tui::cue_parser::CueSheet {
                title: Some("Album".to_string()),
                performer: Some("Artist".to_string()),
                date: None,
                genre: None,
                catalog: None,
                tracks: vec![crate::tui::cue_parser::CueTrack {
                    number: 1,
                    title: Some("Track".to_string()),
                    performer: None,
                    file: Some("side.flac".to_string()),
                    index01_frames: Some(0),
                    index00_frames: None,
                    isrc: None,
                    directives: Vec::new(),
                }],
            },
            sample_rate: 44_100,
            total_samples: 44_100,
            track_boundaries: vec![(0, 44_100)],
        }
    }

    fn split_grouping_request(
        operation_id: crate::tui::message::TagsMbOperationId,
        info: crate::tui::cue_parser::SingleImageInfo,
        editor_park: bool,
        editor_session: Option<crate::tui::message::MetadataEditorSessionGuard>,
    ) -> crate::tui::command::SplitCueAlbumGroupingRequest {
        let cue_path = info.cue_path.clone();
        crate::tui::command::SplitCueAlbumGroupingRequest {
            operation_id,
            key: crate::tui::command::split_cue_album_grouping_key_from_paths(&[cue_path]),
            infos: vec![info],
            editor_park,
            active_audio_path: None,
            editor_session,
        }
    }

    fn grouping_outcome_with_releases(
        info: &crate::tui::cue_parser::SingleImageInfo,
        releases: Vec<crate::tui::musicbrainz::MbRelease>,
    ) -> Box<crate::tui::command::SplitCueAlbumGroupingAsyncOutcome> {
        Box::new(crate::tui::command::SplitCueAlbumGroupingAsyncOutcome {
            decision: crate::convert::split_cue_album::split_each_decision(
                std::slice::from_ref(&info.cue_path),
                crate::tui::command::SplitCueAlbumGroupingReason::ConcatTocHit,
            ),
            toc_outcome: Some(lookup_outcome(releases)),
            cache_writes: Vec::new(),
        })
    }

    #[tokio::test]
    async fn stale_grouping_after_editor_close_cannot_cancel_newer_browse_lookup() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        app.active_overlay = ActiveOverlay::MetadataEditor(single_source_file_editor());
        let a_id = begin_tags_mb_prelookup_operation(
            &mut app,
            true,
            crate::tui::app::TagsMbOperationPhase::Grouping,
        )
        .expect("A grouping operation");
        let info = split_cue_info_for_mb_operation(
            "/tmp/stale-a/side.flac",
            "/tmp/stale-a/side.cue",
        );
        let request = split_grouping_request(a_id, info, true, None);

        finish_metadata_editor_tags_mb_operation(&mut app);
        app.active_overlay = ActiveOverlay::None;
        let b_ctx = ctx_for(
            &mut app,
            vec![std::path::PathBuf::from("/tmp/current-b/track.flac")],
            false,
        );
        let b_id = b_ctx.operation_id;
        handle_tags_from_mb_complete(
            &mut app,
            &tx,
            crate::tui::message::MbOutcome::Toc {
                outcome: Ok(lookup_outcome(vec![
                    release("b1", "Current B1"),
                    release("b2", "Current B2"),
                ])),
            },
            b_ctx,
        );
        app.set_status(":tags-mb: B remains current".to_string());
        let status_before = app.status_message.as_ref().map(|(message, _)| message.clone());

        crate::tui::command::handle_split_cue_album_grouping_complete(
            &mut app,
            &tx,
            request,
            Err("late A grouping failure".to_string()),
        );

        assert_eq!(
            app.active_tags_mb_operation.map(|active| active.operation_id),
            Some(b_id)
        );
        assert!(matches!(
            &app.active_overlay,
            ActiveOverlay::MbSelect(state) if state.operation_id == b_id
        ));
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.clone()),
            status_before
        );
    }

    #[test]
    fn stale_discovery_after_cancellation_cannot_overwrite_newer_status() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::MetadataEditor(single_source_file_editor());
        let editor_session = session_guard_for(match &app.active_overlay {
            ActiveOverlay::MetadataEditor(state) => state,
            _ => unreachable!(),
        });
        let a_id = begin_tags_mb_prelookup_operation(
            &mut app,
            true,
            crate::tui::app::TagsMbOperationPhase::Discovery,
        )
        .expect("A discovery operation");
        let request = crate::tui::command::InEditorSplitCueMusicBrainzInfoRequest {
            operation_id: a_id,
            source: crate::tui::command::InEditorSplitCueMusicBrainzSource::SingleSourceFolder,
            sources: vec![std::path::PathBuf::from("/tmp/stale-a")],
            audio_paths: Vec::new(),
            active_audio_path: Some(std::path::PathBuf::from("/tmp/stale-a/side.flac")),
            editor_session,
        };
        finish_metadata_editor_tags_mb_operation(&mut app);
        let b_id = begin_tags_mb_lookup_operation(&mut app, false).expect("B lookup");
        app.set_status(":tags-mb: B status".to_string());
        let status_before = app.status_message.as_ref().map(|(message, _)| message.clone());

        crate::tui::command::handle_in_editor_split_cue_musicbrainz_info_complete(
            &mut app,
            &tx(),
            request,
            Err("late discovery".to_string()),
        );

        assert_eq!(
            app.active_tags_mb_operation.map(|active| active.operation_id),
            Some(b_id)
        );
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.clone()),
            status_before
        );
    }

    #[tokio::test]
    async fn stale_grouping_cannot_replace_current_picker_or_its_context_menu() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        let a_id = begin_tags_mb_prelookup_operation(
            &mut app,
            false,
            crate::tui::app::TagsMbOperationPhase::Grouping,
        )
        .expect("A grouping operation");
        let info = split_cue_info_for_mb_operation(
            "/tmp/stale-a/side.flac",
            "/tmp/stale-a/side.cue",
        );
        let request = split_grouping_request(a_id, info.clone(), false, None);

        let b_ctx = ctx_for(
            &mut app,
            vec![std::path::PathBuf::from("/tmp/current-b/track.flac")],
            false,
        );
        let b_id = b_ctx.operation_id;
        handle_tags_from_mb_complete(
            &mut app,
            &tx,
            crate::tui::message::MbOutcome::Toc {
                outcome: Ok(lookup_outcome(vec![
                    release("b1", "Current B1"),
                    release("b2", "Current B2"),
                ])),
            },
            b_ctx,
        );
        let ActiveOverlay::MbSelect(picker) =
            std::mem::replace(&mut app.active_overlay, ActiveOverlay::None)
        else {
            panic!("B must open a picker");
        };
        app.pending_mb_select = Some(picker);
        app.active_overlay = ActiveOverlay::ContextMenu {
            levels: Vec::new(),
            origin: (4, 5),
            anchor_bottom: false,
        };
        app.set_status(":tags-mb: B context menu".to_string());
        let status_before = app.status_message.as_ref().map(|(message, _)| message.clone());

        crate::tui::command::handle_split_cue_album_grouping_complete(
            &mut app,
            &tx,
            request,
            Ok(grouping_outcome_with_releases(
                &info,
                vec![release("a1", "Stale A1"), release("a2", "Stale A2")],
            )),
        );

        assert!(matches!(app.active_overlay, ActiveOverlay::ContextMenu { .. }));
        assert!(app
            .pending_mb_select
            .as_ref()
            .is_some_and(|picker| picker.operation_id == b_id));
        assert_eq!(
            app.active_tags_mb_operation.map(|active| active.operation_id),
            Some(b_id)
        );
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.clone()),
            status_before
        );
    }

    #[tokio::test]
    async fn current_discovery_reuses_one_identity_through_lookup_picker_and_apply() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        let editor = single_source_file_editor();
        let editor_session = session_guard_for(&editor);
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);
        let operation_id = begin_tags_mb_prelookup_operation(
            &mut app,
            true,
            crate::tui::app::TagsMbOperationPhase::Discovery,
        )
        .expect("discovery operation");
        let side_a = split_cue_info_for_mb_operation(
            "/tmp/split-cue-side-a.flac",
            "/tmp/current-album/side_a.cue",
        );
        let side_b = split_cue_info_for_mb_operation(
            "/tmp/current-album/side_b.flac",
            "/tmp/current-album/side_b.cue",
        );
        let paths = vec![side_a.audio_path.clone(), side_b.audio_path.clone()];
        let request = crate::tui::command::InEditorSplitCueMusicBrainzInfoRequest {
            operation_id,
            source: crate::tui::command::InEditorSplitCueMusicBrainzSource::UnifiedAlbum,
            sources: paths.clone(),
            audio_paths: paths.clone(),
            active_audio_path: Some(side_a.audio_path.clone()),
            editor_session,
        };

        crate::tui::command::handle_in_editor_split_cue_musicbrainz_info_complete(
            &mut app,
            &tx,
            request,
            Ok(vec![side_a, side_b]),
        );
        assert!(tags_mb_operation_is_current_phase(
            &app,
            operation_id,
            crate::tui::app::TagsMbOperationPhase::Lookup,
        ));

        handle_tags_from_mb_complete(
            &mut app,
            &tx,
            crate::tui::message::MbOutcome::Toc {
                outcome: Ok(lookup_outcome(vec![
                    release("r1", "Release 1"),
                    release("r2", "Release 2"),
                ])),
            },
            crate::tui::message::TagsMbContext {
                operation_id,
                paths,
                editor_park: true,
                fallback_seed: None,
                editor_session: Some(editor_session),
            },
        );
        let ActiveOverlay::MbSelect(mut picker) =
            std::mem::replace(&mut app.active_overlay, ActiveOverlay::None)
        else {
            panic!("current discovery must continue to a picker");
        };
        assert_eq!(picker.operation_id, operation_id);
        picker.selected = 1;
        let pending = start_mb_select_apply_operation(&mut app, picker)
            .expect("picker acceptance must enter verification");
        assert_eq!(pending.operation_id, operation_id);

        complete_tags_mb_apply_operation(
            &mut app,
            &tx,
            pending.operation_id,
            pending.releases,
            pending.selected,
            pending.paths,
            pending.editor_session,
            crate::tui::musicbrainz::PerTrackDecision::default(),
        );
        assert!(app.active_tags_mb_operation.is_none());
    }

    #[tokio::test]
    async fn current_grouping_reuses_one_identity_through_picker_and_apply() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        let editor = single_source_file_editor();
        let editor_session = session_guard_for(&editor);
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);
        let operation_id = begin_tags_mb_prelookup_operation(
            &mut app,
            true,
            crate::tui::app::TagsMbOperationPhase::Grouping,
        )
        .expect("grouping operation");
        let info = split_cue_info_for_mb_operation(
            "/tmp/split-cue-side-a.flac",
            "/tmp/current/side.cue",
        );
        let request = split_grouping_request(
            operation_id,
            info.clone(),
            true,
            Some(editor_session),
        );

        crate::tui::command::handle_split_cue_album_grouping_complete(
            &mut app,
            &tx,
            request,
            Ok(grouping_outcome_with_releases(
                &info,
                vec![release("r1", "Release 1"), release("r2", "Release 2")],
            )),
        );
        let ActiveOverlay::MbSelect(mut picker) =
            std::mem::replace(&mut app.active_overlay, ActiveOverlay::None)
        else {
            panic!("current grouping must open a picker");
        };
        assert_eq!(picker.operation_id, operation_id);
        picker.selected = 1;
        let pending = start_mb_select_apply_operation(&mut app, picker)
            .expect("picker acceptance must enter verification");
        assert_eq!(pending.operation_id, operation_id);

        complete_tags_mb_apply_operation(
            &mut app,
            &tx,
            pending.operation_id,
            pending.releases,
            pending.selected,
            pending.paths,
            pending.editor_session,
            crate::tui::musicbrainz::PerTrackDecision::default(),
        );
        assert!(app.active_tags_mb_operation.is_none());
    }

    #[tokio::test]
    async fn duplicate_discovery_and_grouping_completions_are_total_noops() {
        let mut discovery_app = AppState::new_for_test(TonepoetConfig::default());
        discovery_app.active_overlay = ActiveOverlay::MetadataEditor(single_source_file_editor());
        let editor_session = session_guard_for(match &discovery_app.active_overlay {
            ActiveOverlay::MetadataEditor(state) => state,
            _ => unreachable!(),
        });
        let discovery_id = begin_tags_mb_prelookup_operation(
            &mut discovery_app,
            true,
            crate::tui::app::TagsMbOperationPhase::Discovery,
        )
        .expect("discovery operation");
        let discovery_request = || crate::tui::command::InEditorSplitCueMusicBrainzInfoRequest {
            operation_id: discovery_id,
            source: crate::tui::command::InEditorSplitCueMusicBrainzSource::UnifiedAlbum,
            sources: vec![std::path::PathBuf::from("/tmp/duplicate/side.flac")],
            audio_paths: vec![std::path::PathBuf::from("/tmp/duplicate/side.flac")],
            active_audio_path: Some(std::path::PathBuf::from("/tmp/duplicate/side.flac")),
            editor_session,
        };
        crate::tui::command::handle_in_editor_split_cue_musicbrainz_info_complete(
            &mut discovery_app,
            &tx(),
            discovery_request(),
            Err("first terminal discovery".to_string()),
        );
        let status_after_first = discovery_app
            .status_message
            .as_ref()
            .map(|(message, _)| message.clone());
        crate::tui::command::handle_in_editor_split_cue_musicbrainz_info_complete(
            &mut discovery_app,
            &tx(),
            discovery_request(),
            Err("duplicate must be ignored".to_string()),
        );
        assert_eq!(
            discovery_app
                .status_message
                .as_ref()
                .map(|(message, _)| message.clone()),
            status_after_first
        );

        let mut grouping_app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        let grouping_id = begin_tags_mb_prelookup_operation(
            &mut grouping_app,
            false,
            crate::tui::app::TagsMbOperationPhase::Grouping,
        )
        .expect("grouping operation");
        let info = split_cue_info_for_mb_operation(
            "/tmp/duplicate/side.flac",
            "/tmp/duplicate/side.cue",
        );
        crate::tui::command::handle_split_cue_album_grouping_complete(
            &mut grouping_app,
            &tx,
            split_grouping_request(grouping_id, info.clone(), false, None),
            Ok(grouping_outcome_with_releases(
                &info,
                vec![release("r1", "Release 1"), release("r2", "Release 2")],
            )),
        );
        let status_after_grouping = grouping_app
            .status_message
            .as_ref()
            .map(|(message, _)| message.clone());
        crate::tui::command::handle_split_cue_album_grouping_complete(
            &mut grouping_app,
            &tx,
            split_grouping_request(grouping_id, info.clone(), false, None),
            Ok(grouping_outcome_with_releases(
                &info,
                vec![release("x1", "Duplicate 1"), release("x2", "Duplicate 2")],
            )),
        );
        let ActiveOverlay::MbSelect(picker) = &grouping_app.active_overlay else {
            panic!("first grouping completion must own the picker");
        };
        assert_eq!(picker.releases[0].release_id, "r1");
        assert_eq!(
            grouping_app
                .status_message
                .as_ref()
                .map(|(message, _)| message.clone()),
            status_after_grouping
        );
    }

    #[test]
    fn matching_lookup_terminal_completion_releases_latch_exactly_once() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let tx = tx();
        let mut editor = single_source_file_editor();
        let editor_paths = editor.active_surface().paths.clone();
        editor.model.tags_mb_in_flight = false;
        app.active_overlay = ActiveOverlay::MetadataEditor(editor);
        let ctx = ctx_for(&mut app, editor_paths, true);
        let duplicate_ctx = ctx.clone();

        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Toc {
                    outcome: Err("current failure".to_string()),
                },
                ctx,
            },
            &tx,
        );
        let terminal_status = app.status_message.as_ref().map(|(message, _)| message.clone());
        assert!(app.active_tags_mb_operation.is_none());
        let ActiveOverlay::MetadataEditor(editor) = &app.active_overlay else {
            panic!("current lookup failure must leave the editor active");
        };
        assert!(!editor.model.tags_mb_in_flight);

        handle_message(
            &mut app,
            AppMessage::TagsFromMbComplete {
                outcome: crate::tui::message::MbOutcome::Toc {
                    outcome: Err("duplicate late failure".to_string()),
                },
                ctx: duplicate_ctx,
            },
            &tx,
        );
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.clone()),
            terminal_status,
            "a duplicate terminal completion must be a total no-op"
        );
    }

    #[test]
    fn valid_mb_apply_completion_is_consumed_exactly_once() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let mut editor = single_source_file_editor();
        editor.model.tags_mb_in_flight = true;
        let editor_paths = editor.active_surface().paths.clone();
        app.pending_metadata_editor = Some(editor);

        let mut picker = Box::new(crate::tui::app::MbSelectState::new(
            vec![release("once-a", "First"), release("once-b", "Chosen")],
            editor_paths,
        ));
        picker.selected = 1;
        let pending = start_mb_select_apply_operation(&mut app, picker)
            .expect("selection should acquire an operation ID");

        complete_tags_mb_apply_operation(
            &mut app,
            &tx(),
            pending.operation_id,
            pending.releases.clone(),
            pending.selected,
            pending.paths.clone(),
            pending.editor_session,
            crate::tui::musicbrainz::PerTrackDecision::default(),
        );
        let first_status = app.status_message.as_ref().map(|(message, _)| message.clone());
        let ActiveOverlay::MetadataEditor(state) = &app.active_overlay else {
            panic!("valid completion must apply to the metadata editor");
        };
        let first_back = state.mb_back.clone().expect(":mb-back state after apply");
        assert_eq!(first_back.selected, 1);
        let first_album = state
            .active_surface()
            .entries
            .iter()
            .find(|entry| entry.display_key == "ALBUM")
            .expect("valid completion should populate ALBUM")
            .value
            .clone();
        assert_eq!(first_album, "Chosen");
        assert!(app.active_tags_mb_operation.is_none());
        assert!(!state.model.tags_mb_in_flight);

        complete_tags_mb_apply_operation(
            &mut app,
            &tx(),
            pending.operation_id,
            pending.releases,
            pending.selected,
            pending.paths,
            pending.editor_session,
            crate::tui::musicbrainz::PerTrackDecision::default(),
        );

        let ActiveOverlay::MetadataEditor(state) = &app.active_overlay else {
            panic!("duplicate completion must leave the applied editor intact");
        };
        let second_back = state.mb_back.as_ref().expect(":mb-back state remains");
        assert_eq!(second_back.selected, first_back.selected);
        assert_eq!(second_back.releases[1].release_id, first_back.releases[1].release_id);
        let second_album = state
            .active_surface()
            .entries
            .iter()
            .find(|entry| entry.display_key == "ALBUM")
            .expect("duplicate completion must preserve ALBUM")
            .value
            .clone();
        assert_eq!(second_album, first_album);
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.clone()),
            first_status,
            "duplicate completion must be a total no-op"
        );
    }

    fn completion_test_app(
        kind: CompletionOperationKind,
    ) -> (
        AppState,
        crate::tui::message::TagsMbOperationId,
        crate::tui::message::MetadataEditorSessionGuard,
    ) {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let mut editor = single_source_file_editor();
        editor.active_surface_mut().dirty = true;
        let guard = session_guard_for(&editor);
        app.pending_metadata_editor = Some(editor);
        let operation_id =
            begin_completion_operation(&mut app, kind, "test").expect("begin completion operation");
        (app, operation_id, guard)
    }

    fn counted_completion_test_app(
        kind: CompletionOperationKind,
        total: usize,
    ) -> (
        AppState,
        crate::tui::message::TagsMbOperationId,
        crate::tui::message::MetadataEditorSessionGuard,
    ) {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let mut editor = single_source_file_editor();
        editor.active_surface_mut().dirty = true;
        let guard = session_guard_for(&editor);
        app.pending_metadata_editor = Some(editor);
        let operation_id = begin_counted_completion_operation(&mut app, kind, "test", total)
            .expect("begin counted completion operation");
        (app, operation_id, guard)
    }

    fn verify_result(path: &str) -> crate::tui::verify::VerifyResult {
        crate::tui::verify::VerifyResult {
            path: std::path::PathBuf::from(path),
            passed: true,
            detail: "fixture passed".to_string(),
        }
    }

    fn compare_result(path: &str) -> crate::tui::bit_compare::CompareResult {
        crate::tui::bit_compare::CompareResult {
            ref_path: std::path::PathBuf::from("/tmp/reference.flac"),
            target_path: std::path::PathBuf::from(path),
            identical: true,
            detail: "fixture identical".to_string(),
        }
    }

    fn preemphasis_result(path: &str) -> crate::tui::preemphasis::PreemphasisResult {
        crate::tui::preemphasis::PreemphasisResult {
            path: std::path::PathBuf::from(path),
            confidence: crate::tui::preemphasis::PreemphasisConfidence::NotDetected,
            cue_confirmed: false,
            llr_m2_vs_m0: f64::NAN,
            llr_m2_vs_m1: f64::NAN,
            fitted_alpha: f64::NAN,
            frames_scored: 0,
            deemph_distance_delta: 0.0,
            gates_fired: Vec::new(),
            detail: String::new(),
            spectral_rms_error: 0.0,
            crest_improvement: 0.0,
        }
    }

    fn assert_completion_restored_dirty_editor(
        app: &AppState,
        kind: CompletionOperationKind,
        guard: crate::tui::message::MetadataEditorSessionGuard,
    ) {
        assert!(
            !app.active_completion_operations.contains_key(&kind),
            "matching completion must retire its authority"
        );
        assert!(app.pending_metadata_editor.is_none());
        let ActiveOverlay::MetadataEditor(editor) = &app.active_overlay else {
            panic!("matching completion must restore the parked editor");
        };
        assert!(editor.active_surface().dirty);
        assert_eq!(session_guard_for(editor), guard);
    }

    #[test]
    fn replaygain_refresh_replaces_stale_carrier_counts() {
        let mut editor = single_source_file_editor();
        let surface = editor.active_surface_mut();
        surface
            .entries
            .retain(|entry| entry.display_key != "REPLAYGAIN_TRACK_GAIN");
        surface.entries.push(crate::tui::probe::TagEntry {
            row_scope: crate::tui::probe::RowScope::File,
            display_key: "REPLAYGAIN_TRACK_GAIN".to_string(),
            item_key: lofty::tag::ItemKey::ReplayGainTrackGain,
            value: "-9.00 dB".to_string(),
            original: "-9.00 dB".to_string(),
            is_binary: false,
            is_mixed: false,
            has_multiple_stored_values: true,
            per_file_stored_value_counts: vec![2],
            per_file_values: crate::tui::probe::metadata_field_values_from_scalars(vec!["-9.00 dB".to_string()]),
            per_file_originals: crate::tui::probe::metadata_field_values_from_scalars(vec!["-9.00 dB".to_string()]),
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        });

        metadata_editor_upsert_per_file_entry(
            surface,
            "REPLAYGAIN_TRACK_GAIN",
            lofty::tag::ItemKey::ReplayGainTrackGain,
            vec!["-7.25 dB".to_string()],
        );

        let entry = surface
            .entries
            .iter()
            .find(|entry| entry.display_key == "REPLAYGAIN_TRACK_GAIN")
            .expect("ReplayGain row");
        assert_eq!(entry.value, "-7.25 dB");
        assert_eq!(entry.per_file_values, vec!["-7.25 dB".to_string()]);
        assert_eq!(entry.per_file_stored_value_counts, vec![1]);
        assert!(!entry.has_multiple_stored_values);
    }

    #[test]
    fn verify_completion_preserves_parked_dirty_editor() {
        let kind = CompletionOperationKind::Verify;
        let (mut app, operation_id, guard) = counted_completion_test_app(kind, 1);
        handle_message(
            &mut app,
            AppMessage::VerifyComplete {
                operation_id,
                result: verify_result("/tmp/verify.flac"),
            },
            &tx(),
        );
        assert_completion_restored_dirty_editor(&app, kind, guard);
        assert_eq!(app.verify_results.len(), 1);
    }

    #[test]
    fn compare_completion_preserves_parked_dirty_editor() {
        let kind = CompletionOperationKind::Compare;
        let (mut app, operation_id, guard) = counted_completion_test_app(kind, 1);
        handle_message(
            &mut app,
            AppMessage::CompareComplete {
                operation_id,
                result: compare_result("/tmp/target.flac"),
            },
            &tx(),
        );
        assert_completion_restored_dirty_editor(&app, kind, guard);
        assert_eq!(app.compare_results.len(), 1);
    }

    #[test]
    fn preemphasis_completion_preserves_parked_dirty_editor() {
        let kind = CompletionOperationKind::Preemphasis;
        let (mut app, operation_id, guard) = counted_completion_test_app(kind, 1);
        handle_message(
            &mut app,
            AppMessage::PreemphasisComplete {
                operation_id,
                result: preemphasis_result("/tmp/preemph.flac"),
            },
            &tx(),
        );
        assert_completion_restored_dirty_editor(&app, kind, guard);
        assert_eq!(app.preemph_results.len(), 1);
    }

    #[test]
    fn counted_completion_families_supersede_stalled_batches_and_complete_exactly_once() {
        for kind in [
            CompletionOperationKind::Verify,
            CompletionOperationKind::Compare,
            CompletionOperationKind::Preemphasis,
        ] {
            let mut app = AppState::new_for_test(TonepoetConfig::default());
            // Audit MEDIUM: a worker that dies before its terminal message
            // leaves `remaining` above zero forever. Re-invoking the command
            // must supersede the stalled batch, not refuse for the session.
            let stalled_id = begin_counted_completion_operation(&mut app, kind, "test", 2)
                .expect("first operation");
            let superseding_id =
                begin_counted_completion_operation(&mut app, kind, "retry", 1)
                    .expect("re-invocation supersedes the stalled counted batch");
            assert_ne!(stalled_id, superseding_id);
            assert_eq!(
                app.active_completion_operations
                    .get(&kind)
                    .and_then(|active| active.batch)
                    .map(|batch| (batch.total, batch.remaining)),
                Some((1, 1))
            );
            assert_eq!(
                complete_counted_completion_operation(&mut app, kind, stalled_id),
                None,
                "late completions from the superseded batch must be rejected"
            );
            assert_eq!(
                complete_counted_completion_operation(&mut app, kind, superseding_id),
                Some(true)
            );
            assert_eq!(
                complete_counted_completion_operation(&mut app, kind, superseding_id),
                None,
                "duplicate terminal completion must be ignored"
            );
        }
    }

    #[test]
    fn counted_begin_still_refuses_an_uncounted_same_kind_operation() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let kind = CompletionOperationKind::Verify;
        let uncounted_id = begin_completion_operation(&mut app, kind, "uncounted")
            .expect("uncounted operation");
        assert!(
            begin_counted_completion_operation(&mut app, kind, "counted", 1).is_err(),
            "only counted batches may be displaced by a counted re-invocation"
        );
        assert_eq!(
            app.active_completion_operations
                .get(&kind)
                .map(|active| active.operation_id),
            Some(uncounted_id)
        );
    }

    #[test]
    fn counted_completion_families_preserve_an_occupied_prompt() {
        let fixtures = [
            CompletionOperationKind::Verify,
            CompletionOperationKind::Compare,
            CompletionOperationKind::Preemphasis,
        ];
        for kind in fixtures {
            let mut app = AppState::new_for_test(TonepoetConfig::default());
            app.active_overlay = ActiveOverlay::TextEdit {
                input: crate::tui::text_input::TextInputState::new("typed secret".to_string()),
                target: crate::tui::app::TextEditTarget::ArchivePassword(
                    std::path::PathBuf::from("/tmp/encrypted.7z"),
                ),
                label: "archive password".to_string(),
            };
            let operation_id = begin_counted_completion_operation(&mut app, kind, "test", 1)
                .expect("operation");
            match kind {
                CompletionOperationKind::Verify => handle_message(
                    &mut app,
                    AppMessage::VerifyComplete {
                        operation_id,
                        result: verify_result("/tmp/verify.flac"),
                    },
                    &tx(),
                ),
                CompletionOperationKind::Compare => handle_message(
                    &mut app,
                    AppMessage::CompareComplete {
                        operation_id,
                        result: compare_result("/tmp/target.flac"),
                    },
                    &tx(),
                ),
                CompletionOperationKind::Preemphasis => handle_message(
                    &mut app,
                    AppMessage::PreemphasisComplete {
                        operation_id,
                        result: preemphasis_result("/tmp/preemph.flac"),
                    },
                    &tx(),
                ),
                _ => unreachable!(),
            }
            let ActiveOverlay::TextEdit { input, .. } = &app.active_overlay else {
                panic!("{kind:?} completion replaced the occupied prompt");
            };
            assert_eq!(input.text, "typed secret");
            assert!(!app.active_completion_operations.contains_key(&kind));
        }
    }

    #[test]
    fn counted_completion_over_password_prompt_preserves_and_restores_owned_editor() {
        for kind in [
            CompletionOperationKind::Verify,
            CompletionOperationKind::Compare,
            CompletionOperationKind::Preemphasis,
        ] {
            let (mut app, operation_id, guard) = counted_completion_test_app(kind, 1);
            app.active_overlay = ActiveOverlay::TextEdit {
                input: crate::tui::text_input::TextInputState::new(
                    "typed secret".to_string(),
                ),
                target: crate::tui::app::TextEditTarget::ArchivePassword(
                    std::path::PathBuf::from("/tmp/encrypted.7z"),
                ),
                label: "archive password".to_string(),
            };

            match kind {
                CompletionOperationKind::Verify => handle_message(
                    &mut app,
                    AppMessage::VerifyComplete {
                        operation_id,
                        result: verify_result("/tmp/verify.flac"),
                    },
                    &tx(),
                ),
                CompletionOperationKind::Compare => handle_message(
                    &mut app,
                    AppMessage::CompareComplete {
                        operation_id,
                        result: compare_result("/tmp/target.flac"),
                    },
                    &tx(),
                ),
                CompletionOperationKind::Preemphasis => handle_message(
                    &mut app,
                    AppMessage::PreemphasisComplete {
                        operation_id,
                        result: preemphasis_result("/tmp/preemph.flac"),
                    },
                    &tx(),
                ),
                _ => unreachable!(),
            }

            let ActiveOverlay::TextEdit { input, .. } = &app.active_overlay else {
                panic!("{kind:?} completion replaced the password prompt");
            };
            assert_eq!(input.text, "typed secret");
            assert!(app.pending_metadata_editor.as_deref().is_some_and(|editor| {
                editor.active_surface().dirty && session_guard_for(editor) == guard
            }));

            crate::tui::keybindings::handle_key(
                &mut app,
                crossterm::event::KeyEvent::new(
                    crossterm::event::KeyCode::Esc,
                    crossterm::event::KeyModifiers::NONE,
                ),
                &tx(),
            );
            assert_completion_restored_dirty_editor(&app, kind, guard);
        }
    }

    #[test]
    fn analysis_completion_preserves_parked_dirty_editor() {
        let kind = CompletionOperationKind::Analysis;
        let (mut app, operation_id, guard) = completion_test_app(kind);
        app.analysis_pending = 1;
        handle_message(
            &mut app,
            AppMessage::AnalysisComplete {
                operation_id,
                result: Err("fixture failure".to_string()),
            },
            &tx(),
        );
        assert_completion_restored_dirty_editor(&app, kind, guard);
    }

    #[test]
    fn accuraterip_completion_preserves_parked_dirty_editor() {
        let kind = CompletionOperationKind::AccurateRip;
        let (mut app, operation_id, guard) = completion_test_app(kind);
        handle_message(
            &mut app,
            AppMessage::AccurateRipComplete {
                operation_id,
                pages: Vec::new(),
            },
            &tx(),
        );
        assert_completion_restored_dirty_editor(&app, kind, guard);
    }

    #[test]
    fn ctdb_completion_preserves_parked_dirty_editor() {
        let kind = CompletionOperationKind::Ctdb;
        let (mut app, operation_id, guard) = completion_test_app(kind);
        handle_message(
            &mut app,
            AppMessage::CtdbComplete {
                operation_id,
                pages: Vec::new(),
            },
            &tx(),
        );
        assert_completion_restored_dirty_editor(&app, kind, guard);
    }

    #[test]
    fn ar_batch_completion_preserves_parked_dirty_editor() {
        let kind = CompletionOperationKind::ArBatch;
        let (mut app, operation_id, guard) = completion_test_app(kind);
        handle_message(
            &mut app,
            AppMessage::ArBatchComplete {
                operation_id,
                result: Box::new(crate::tui::accuraterip::ArBatchResult {
                    albums: Vec::new(),
                    scan_dir: std::path::PathBuf::from("/tmp"),
                    report_path: None,
                }),
            },
            &tx(),
        );
        assert_completion_restored_dirty_editor(&app, kind, guard);
    }

    #[test]
    fn offset_correction_completion_preserves_parked_dirty_editor() {
        let kind = CompletionOperationKind::OffsetCorrection;
        let (mut app, operation_id, guard) = completion_test_app(kind);
        handle_message(
            &mut app,
            AppMessage::OffsetCorrectionComplete {
                operation_id,
                result: Err("fixture failure".to_string()),
            },
            &tx(),
        );
        assert_completion_restored_dirty_editor(&app, kind, guard);
    }

    #[test]
    fn ctdb_repair_completion_preserves_parked_dirty_editor() {
        let kind = CompletionOperationKind::CtdbRepair;
        let (mut app, operation_id, guard) = completion_test_app(kind);
        handle_message(
            &mut app,
            AppMessage::CtdbRepairComplete {
                operation_id,
                result: Err("fixture failure".to_string()),
            },
            &tx(),
        );
        assert_completion_restored_dirty_editor(&app, kind, guard);
    }

    #[test]
    fn mismatched_accuraterip_completion_discards_stale_deferred_ctdb_repair() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let operation_id = begin_completion_operation(
            &mut app,
            CompletionOperationKind::AccurateRip,
            "AccurateRip",
        )
        .expect("operation");
        app.pending_ctdb_repair = Some(crate::tui::app::PendingCtdbRepair {
            paths: vec![std::path::PathBuf::from("/tmp/stale-first.flac")],
            parity_url: "https://example.invalid/parity".to_string(),
            npar: 8,
            expected_crcs: vec![1],
            single_image: None,
        });
        let page = crate::tui::app::ArVerifyPage {
            label: String::new(),
            result: crate::tui::accuraterip::ArVerifyResult {
                tracks: vec![crate::tui::accuraterip::ArTrackResult {
                    path: std::path::PathBuf::from("/tmp/unrelated-first.flac"),
                    track_number: 1,
                    status: crate::tui::accuraterip::ArTrackStatus::Mismatch,
                    confidence: None,
                    offset: None,
                    crc_v1: 1,
                    crc_v2: 2,
                }],
                was_common_scan: false,
                disc_id_str: "fixture-disc".to_string(),
                url: "https://example.invalid/ar".to_string(),
            },
        };

        handle_message(
            &mut app,
            AppMessage::AccurateRipComplete {
                operation_id,
                pages: vec![page],
            },
            &tx(),
        );

        assert!(app.pending_ctdb_repair.is_none());
        assert!(matches!(
            app.active_overlay,
            ActiveOverlay::AccurateRipVerify(_)
        ));
        assert!(app.status_message.as_ref().is_some_and(|(message, _)| {
            message.contains("discarded stale deferred CTDB repair")
        }));
    }

    #[test]
    fn offset_correction_is_classified_as_browse_visible_mutation() {
        assert!(message_mutates_browse_visible_state(
            &AppMessage::OffsetCorrectionComplete {
                operation_id: crate::tui::message::TagsMbOperationId(1),
                result: Ok("fixture".to_string()),
            }
        ));
    }

    #[test]
    fn gnudb_cancel_restores_exact_owned_dirty_editor() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let mut editor = single_source_file_editor();
        editor.active_surface_mut().dirty = true;
        let guard = session_guard_for(&editor);
        app.pending_metadata_editor = Some(editor);

        let operation_id = begin_gnudb_operation(&mut app).expect("begin GNUDB operation");
        assert_eq!(
            app.active_gnudb_operation,
            Some(crate::tui::app::ActiveGnudbOperation {
                operation_id,
                editor_session: Some(guard),
            })
        );
        assert!(cancel_gnudb_operation(&mut app, operation_id));
        assert!(app.active_gnudb_operation.is_none());
        assert!(app.pending_metadata_editor.is_none());
        let ActiveOverlay::MetadataEditor(restored) = &app.active_overlay else {
            panic!("owned editor must be restored on GNUDB cancellation");
        };
        assert!(restored.active_surface().dirty);
        assert_eq!(session_guard_for(restored), guard);
    }

    #[test]
    fn gnudb_worker_failure_restores_owned_editor_and_retires_only_current_operation() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let mut editor = single_source_file_editor();
        editor.active_surface_mut().dirty = true;
        let guard = session_guard_for(&editor);
        app.pending_metadata_editor = Some(editor);
        let operation_id = begin_gnudb_operation(&mut app).expect("begin GNUDB operation");

        handle_message(
            &mut app,
            AppMessage::GnudbWorkerFailed {
                operation_id,
                detail: "worker panicked".to_string(),
            },
            &tx(),
        );

        assert!(app.active_gnudb_operation.is_none());
        assert!(app.pending_metadata_editor.is_none());
        let ActiveOverlay::MetadataEditor(restored) = &app.active_overlay else {
            panic!("worker failure must restore the exact parked editor");
        };
        assert_eq!(session_guard_for(restored), guard);
        assert!(restored.active_surface().dirty);
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.as_str()),
            Some("GNUDB worker failed: worker panicked; the lookup was retired")
        );
    }

    #[test]
    fn gnudb_operation_identity_remains_current_even_if_mb_state_is_injected() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let editor = single_source_file_editor();
        let guard = session_guard_for(&editor);
        app.pending_metadata_editor = Some(editor);
        let operation_id = begin_gnudb_operation(&mut app).expect("begin GNUDB operation");
        assert_eq!(
            app.active_gnudb_operation.and_then(|active| active.editor_session),
            Some(guard)
        );
        app.active_tags_mb_operation = Some(crate::tui::app::ActiveTagsMbOperation {
            operation_id: crate::tui::message::TagsMbOperationId(operation_id.0 + 100),
            picker_owned: false,
            phase: crate::tui::app::TagsMbOperationPhase::Lookup,
        });

        assert!(gnudb_operation_is_current(&app, operation_id));
        assert!(!gnudb_operation_has_overlay_authority(&app, operation_id));

        handle_message(
            &mut app,
            AppMessage::GnudbReadComplete {
                operation_id,
                result: Ok(crate::tui::gnudb::GnudbEntry {
                    disc_id: "deadbeef".to_string(),
                    artist: "Artist".to_string(),
                    album: "Album".to_string(),
                    year: "2026".to_string(),
                    genre: "Test".to_string(),
                    tracks: vec!["Track 1".to_string()],
                }),
                paths: vec![std::path::PathBuf::from("track.flac")],
                origin_matches: None,
            },
            &tx(),
        );

        assert!(app.active_gnudb_operation.is_none());
        assert!(app.active_tags_mb_operation.is_some());
        assert!(matches!(app.active_overlay, ActiveOverlay::None));
        assert!(app.pending_metadata_editor.is_some());
        assert_eq!(
            session_guard_for(app.pending_metadata_editor.as_deref().expect("MB-owned editor retained")),
            guard
        );
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.as_str()),
            Some("GNUDB: read result discarded because another overlay owns the screen; retry the lookup")
        );
    }

    #[test]
    fn gnudb_review_restores_only_the_exact_parked_editor_session() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let editor = single_source_file_editor();
        let guard = session_guard_for(&editor);
        app.pending_metadata_editor = Some(editor);

        assert!(restore_gnudb_review_editor_if_owned(&mut app, Some(guard)));
        assert!(app.pending_metadata_editor.is_none());
        let ActiveOverlay::MetadataEditor(restored) = &app.active_overlay else {
            panic!("matching GNUDB review guard must restore the parked editor");
        };
        assert_eq!(session_guard_for(restored), guard);

        let mut stale_app = AppState::new_for_test(TonepoetConfig::default());
        let stale_editor = single_source_file_editor();
        let stale_guard = session_guard_for(&stale_editor);
        stale_app.pending_metadata_editor = Some(stale_editor);
        let wrong_guard = crate::tui::message::MetadataEditorSessionGuard {
            session_id: stale_guard.session_id.wrapping_add(1),
            ..stale_guard
        };

        assert!(!restore_gnudb_review_editor_if_owned(
            &mut stale_app,
            Some(wrong_guard)
        ));
        assert!(stale_app.pending_metadata_editor.is_some());
        assert!(matches!(stale_app.active_overlay, ActiveOverlay::None));
    }

    #[test]
    fn gnudb_multi_disc_total_failure_is_reported_as_failure_not_no_matches() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let operation_id = begin_gnudb_operation(&mut app).expect("begin GNUDB operation");

        handle_message(
            &mut app,
            AppMessage::GnudbMultiDiscComplete {
                operation_id,
                entries: Vec::new(),
                failures: vec![
                    "Disc 1 query failed: network unavailable".to_string(),
                    "Disc 2 read failed: timeout".to_string(),
                ],
                attempted: 2,
            },
            &tx(),
        );

        assert!(app.active_gnudb_operation.is_none());
        assert!(matches!(app.active_overlay, ActiveOverlay::None));
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.as_str()),
            Some("GNUDB: all 2 disc lookups failed: Disc 1 query failed: network unavailable")
        );
    }

    #[test]
    fn gnudb_multi_disc_partial_failure_opens_review_and_surfaces_degradation() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let operation_id = begin_gnudb_operation(&mut app).expect("begin GNUDB operation");
        let entry = crate::tui::gnudb::GnudbEntry {
            disc_id: "deadbeef".to_string(),
            artist: "Artist".to_string(),
            album: "Album".to_string(),
            year: "2026".to_string(),
            genre: "Test".to_string(),
            tracks: vec!["Track 1".to_string()],
        };

        handle_message(
            &mut app,
            AppMessage::GnudbMultiDiscComplete {
                operation_id,
                entries: vec![(
                    "Disc 1".to_string(),
                    entry,
                    vec![std::path::PathBuf::from("disc1-track1.flac")],
                )],
                failures: vec!["Disc 2 query failed: timeout".to_string()],
                attempted: 2,
            },
            &tx(),
        );

        assert!(app.active_gnudb_operation.is_none());
        assert!(matches!(app.active_overlay, ActiveOverlay::GnudbReview(_)));
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.as_str()),
            Some("GNUDB: 1 disc, 1 tracks — review and edit; 1 of 2 disc lookups failed")
        );
    }

    #[test]
    fn stale_gnudb_completion_is_total_no_op() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let stale = begin_gnudb_operation(&mut app).expect("first GNUDB operation");
        let current = advance_gnudb_operation(&mut app, stale).expect("advance GNUDB operation");
        app.set_status("current workflow status");
        let before_status = app.status_message.as_ref().map(|(message, _)| message.clone());

        handle_message(
            &mut app,
            AppMessage::GnudbVirtualDiscTocComplete {
                operation_id: stale,
                result: Ok((
                    std::path::PathBuf::from("stale.iso"),
                    vec![60.0],
                    "SACD",
                )),
            },
            &tx(),
        );
        assert_eq!(
            app.active_gnudb_operation.map(|active| active.operation_id),
            Some(current)
        );
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.clone()),
            before_status
        );

        handle_message(
            &mut app,
            AppMessage::GnudbWorkerFailed {
                operation_id: stale,
                detail: "late panic".to_string(),
            },
            &tx(),
        );

        assert_eq!(
            app.active_gnudb_operation.map(|active| active.operation_id),
            Some(current)
        );
        assert!(matches!(app.active_overlay, ActiveOverlay::None));
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.clone()),
            before_status
        );
    }

    #[test]
    fn cue_preview_completion_requires_identity_and_overlay_authority() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let operation_id = begin_cue_operation(&mut app, "test CUE").expect("begin CUE operation");
        app.active_overlay = ActiveOverlay::Help {
            screen: AppScreen::Browse,
            scroll: 7,
        };

        handle_message(
            &mut app,
            AppMessage::CuePreviewComplete {
                operation_id,
                result: Ok((
                    "FILE \"album.flac\" WAVE\n".to_string(),
                    std::path::PathBuf::from("album.cue"),
                    "preview ready".to_string(),
                )),
            },
            &tx(),
        );

        assert!(app.active_cue_operation.is_none());
        assert!(matches!(
            app.active_overlay,
            ActiveOverlay::Help {
                screen: AppScreen::Browse,
                scroll: 7
            }
        ));
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.as_str()),
            Some(
                "MusicBrainz CUE: preview discarded because another workflow owns the editor or overlay; retry the command"
            )
        );
    }

    #[test]
    fn current_cue_preview_completion_opens_exact_preview_and_consumes_operation() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let operation_id = begin_cue_operation(&mut app, "test CUE").expect("begin CUE operation");
        let content = "FILE \"album.flac\" WAVE\n  TRACK 01 AUDIO\n".to_string();
        let path = std::path::PathBuf::from("album.cue");

        handle_message(
            &mut app,
            AppMessage::CuePreviewComplete {
                operation_id,
                result: Ok((content.clone(), path.clone(), "preview ready".to_string())),
            },
            &tx(),
        );

        assert!(app.active_cue_operation.is_none());
        let ActiveOverlay::CuePreview(preview) = &app.active_overlay else {
            panic!("authoritative completion must open CuePreview");
        };
        assert_eq!(preview.content, content);
        assert_eq!(preview.write_path, path);
        assert_eq!(preview.summary, "preview ready");
    }

    #[test]
    fn stale_cue_completion_cannot_mutate_newer_workflow() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let stale = begin_cue_operation(&mut app, "old CUE").expect("old operation");
        assert!(finish_cue_operation_if_current(&mut app, stale));
        let current = begin_cue_operation(&mut app, "new CUE").expect("new operation");
        app.set_status("new workflow status");
        let before_status = app.status_message.as_ref().map(|(message, _)| message.clone());

        handle_message(
            &mut app,
            AppMessage::CuePreviewComplete {
                operation_id: stale,
                result: Err("late failure".to_string()),
            },
            &tx(),
        );

        assert_eq!(
            app.active_cue_operation.map(|active| active.operation_id),
            Some(current)
        );
        assert!(matches!(app.active_overlay, ActiveOverlay::None));
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.clone()),
            before_status
        );
    }

    #[test]
    fn metadata_split_cue_grouping_error_fails_closed_without_fallback_editor() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let operation_id = begin_cue_operation(&mut app, "metadata grouping")
            .expect("begin metadata grouping");

        handle_message(
            &mut app,
            AppMessage::MetadataEditorSplitCueAlbumGroupingComplete {
                operation_id,
                infos: Vec::new(),
                active_cue_path: None,
                ordinary_paths: Vec::new(),
                metadata_sidecar_cue_paths: Vec::new(),
                cue_admission_warnings: Vec::new(),
                cue_policy: crate::tui::cue_parser::DEFAULT_FRONTEND_CUE_POLICY,
                result: Err("synthetic grouping failure".to_string()),
            },
            &tx(),
        );

        assert!(app.active_cue_operation.is_none());
        assert!(matches!(app.active_overlay, ActiveOverlay::None));
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.as_str()),
            Some("metadata: split-CUE grouping failed: synthetic grouping failure")
        );
    }

}

#[cfg(test)]
mod tag_clipboard_completion_tests {
    use super::*;
    use crate::config::TonepoetConfig;
    use crate::tui::probe::{RowScope, TagEntry};
    use lofty::tag::ItemKey;
    use std::path::PathBuf;

    fn tx() -> mpsc::Sender<AppMessage> {
        let (tx, _rx) = mpsc::channel(4);
        tx
    }

    fn tag(display_key: &str, value: &str) -> TagEntry {
        TagEntry {
            display_key: display_key.to_string(),
            item_key: ItemKey::Unknown(display_key.to_string()),
            value: value.to_string(),
            original: value.to_string(),
            is_binary: false,
            is_mixed: false,
            has_multiple_stored_values: false,
            row_scope: RowScope::File,
            per_file_stored_value_counts: vec![1],
            per_file_values: crate::tui::probe::metadata_field_values_from_scalars(vec![value.to_string()]),
            per_file_originals: crate::tui::probe::metadata_field_values_from_scalars(vec![value.to_string()]),
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        }
    }

    #[test]
    fn tag_clipboard_completion_is_last_request_wins_and_retains_full_entries() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.browse.tag_clipboard_copy_generation = 2;
        app.browse.tag_clipboard_copy_active_generation = Some(2);
        app.browse.tag_clipboard_copy_cancel = Some(std::sync::Arc::new(
            std::sync::atomic::AtomicBool::new(false),
        ));
        app.browse.tag_clipboard = Some(super::super::browse::TagClipboard {
            source_paths: vec![PathBuf::from("current.flac")],
            entries: vec![tag("TITLE", "current")],
        });

        handle_message(
            &mut app,
            AppMessage::TagClipboardCopyComplete {
                generation: 1,
                source_paths: vec![PathBuf::from("stale.flac")],
                result: Ok((vec![tag("TITLE", "stale")], 0)),
            },
            &tx(),
        );
        let clipboard = app.browse.tag_clipboard.as_ref().expect("current clipboard");
        assert_eq!(clipboard.source_paths, vec![PathBuf::from("current.flac")]);
        assert_eq!(clipboard.entries[0].value, "current");

        let mut copied = tag("TRACKTOTAL", "12");
        copied.row_scope = RowScope::Track;
        copied.is_mixed = true;
        copied.per_file_stored_value_counts = vec![2, 1];
        copied.per_file_values = crate::tui::probe::metadata_field_values_from_scalars(vec!["12".to_string(), "13".to_string()]);
        copied.per_file_originals = copied.per_file_values.clone();
        handle_message(
            &mut app,
            AppMessage::TagClipboardCopyComplete {
                generation: 2,
                source_paths: vec![PathBuf::from("one.flac"), PathBuf::from("two.flac")],
                result: Ok((vec![copied], 0)),
            },
            &tx(),
        );

        let clipboard = app.browse.tag_clipboard.as_ref().expect("replacement clipboard");
        assert_eq!(
            clipboard.source_paths,
            vec![PathBuf::from("one.flac"), PathBuf::from("two.flac")],
        );
        assert_eq!(clipboard.entries[0].row_scope, RowScope::Track);
        assert!(clipboard.entries[0].is_mixed);
        assert_eq!(clipboard.entries[0].per_file_stored_value_counts, vec![2, 1]);
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.as_str()),
            Some("Copied 1 field from 2 files (text clipboard)"),
        );
        assert_eq!(app.browse.tag_clipboard_copy_active_generation, None);
        assert!(app.browse.tag_clipboard_copy_cancel.is_none());
    }
}

#[cfg(test)]
mod editor_tag_transfer_preparation_tests {
    use super::*;
    use crate::config::TonepoetConfig;
    use crate::tui::app::{
        AppScreen, MetadataEditorState, MetadataTechnicalDetails, TagTransferScope,
    };
    use crate::tui::probe::{RowScope, TagEntry};
    use lofty::tag::ItemKey;
    use std::path::PathBuf;

    fn tx() -> mpsc::Sender<AppMessage> {
        let (tx, _rx) = mpsc::channel(8);
        tx
    }

    fn title(value: &str) -> TagEntry {
        TagEntry {
            display_key: "TITLE".to_string(),
            item_key: ItemKey::TrackTitle,
            value: value.to_string(),
            original: value.to_string(),
            is_binary: false,
            is_mixed: false,
            has_multiple_stored_values: false,
            row_scope: RowScope::File,
            per_file_stored_value_counts: vec![1],
            per_file_values: crate::tui::probe::metadata_field_values_from_scalars(vec![value.to_string()]),
            per_file_originals: crate::tui::probe::metadata_field_values_from_scalars(vec![value.to_string()]),
            mb_proposed_value: None,
            mb_proposed_per_file: None,
        }
    }

    #[test]
    fn editor_transfer_preparation_is_last_request_wins_across_directions() {
        let mut state = MetadataEditorState::for_files(
            vec![PathBuf::from("/tmp/editor.dsf")],
            vec![title("Original")],
            vec!["editor.dsf".to_string()],
            MetadataTechnicalDetails::default(),
        );
        let (stale_target_request, stale_cancel) = state.begin_tag_transfer_preparation();
        let (current_source_request, _current_cancel) = state.begin_tag_transfer_preparation();
        assert!(stale_cancel.is_cancelled(), "new request must cancel the old directory walk");
        let editor_session = metadata_editor_session_guard(&state);
        let editor_fingerprint =
            super::super::tag_interchange::metadata_editor_transfer_fingerprint(&state);

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::MetadataEditor(Box::new(state));
        app.set_status("newer source request is pending");

        handle_message(
            &mut app,
            AppMessage::MetadataTagTransferTargetsPrepared {
                request_id: stale_target_request,
                editor_session,
                editor_fingerprint,
                scope: TagTransferScope::All,
                field_key: None,
                source_entries: vec![title("Stale outbound")],
                source_dimension: super::super::tag_interchange::TransferDimension::Files(1),
                result: Ok(super::super::tag_interchange::TransferCarrier::Files {
                    paths: vec![PathBuf::from("/tmp/stale-target.dsf")],
                }),
            },
            &tx(),
        );

        let ActiveOverlay::MetadataEditor(state) = &app.active_overlay else {
            panic!("superseded target completion must leave the editor open");
        };
        assert_eq!(state.active_surface().entries[0].value, "Original");
        assert!(state.owns_tag_transfer_preparation(current_source_request));
        assert!(state.tag_transfer_prepare_cancel.is_some());
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.as_str()),
            Some("newer source request is pending"),
            "a stale completion must not overwrite the newer request's status"
        );

        handle_message(
            &mut app,
            AppMessage::MetadataTagTransferSourcePrepared {
                request_id: current_source_request,
                editor_session,
                editor_fingerprint,
                scope: TagTransferScope::All,
                field_key: None,
                result: Ok((
                    vec![title("Current inbound")],
                    super::super::tag_interchange::TransferDimension::Files(1),
                    "files".to_string(),
                )),
            },
            &tx(),
        );

        let ActiveOverlay::MetadataEditor(state) = &app.active_overlay else {
            panic!("current source completion must restore the editor");
        };
        assert_eq!(state.active_surface().entries[0].value, "Current inbound");
        assert!(state.tag_transfer_prepare_cancel.is_none());
        assert!(!state.owns_tag_transfer_preparation(current_source_request));
    }

    #[test]
    fn editor_tag_interchange_preparation_authority_is_consumed_once() {
        let mut state = MetadataEditorState::for_files(
            vec![PathBuf::from("/tmp/editor.flac")],
            vec![title("Original")],
            vec!["editor.flac".to_string()],
            MetadataTechnicalDetails::default(),
        );
        let (request_id, _cancel) = state.begin_tag_transfer_preparation();

        assert!(state.take_tag_transfer_preparation(request_id));
        assert!(!state.take_tag_transfer_preparation(request_id));
        assert!(!state.owns_tag_transfer_preparation(request_id));
    }

    #[test]
    fn parked_editor_transfer_completion_reduces_once_and_preserves_visible_overlay() {
        let mut state = MetadataEditorState::for_files(
            vec![PathBuf::from("/tmp/editor.dsf")],
            vec![title("Original")],
            vec!["editor.dsf".to_string()],
            MetadataTechnicalDetails::default(),
        );
        let (request_id, _cancel) = state.begin_tag_transfer_preparation();
        let editor_session = metadata_editor_session_guard(&state);
        let editor_fingerprint =
            super::super::tag_interchange::metadata_editor_transfer_fingerprint(&state);

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.pending_metadata_editor = Some(Box::new(state));
        app.active_overlay = ActiveOverlay::Help {
            screen: AppScreen::Browse,
            scroll: 3,
        };

        handle_message(
            &mut app,
            AppMessage::MetadataTagTransferSourcePrepared {
                request_id,
                editor_session,
                editor_fingerprint,
                scope: TagTransferScope::All,
                field_key: None,
                result: Ok((
                    vec![title("Applied once")],
                    super::super::tag_interchange::TransferDimension::Files(1),
                    "files".to_string(),
                )),
            },
            &tx(),
        );

        assert!(matches!(
            &app.active_overlay,
            ActiveOverlay::Help {
                screen: AppScreen::Browse,
                scroll: 3
            }
        ));
        let state = app
            .pending_metadata_editor
            .as_ref()
            .expect("parked editor must be restored");
        assert_eq!(state.active_surface().entries[0].value, "Applied once");
        let accepted_status = app.status_message.clone();

        handle_message(
            &mut app,
            AppMessage::MetadataTagTransferSourcePrepared {
                request_id,
                editor_session,
                editor_fingerprint,
                scope: TagTransferScope::All,
                field_key: None,
                result: Ok((
                    vec![title("Duplicate delivery")],
                    super::super::tag_interchange::TransferDimension::Files(1),
                    "files".to_string(),
                )),
            },
            &tx(),
        );

        let state = app
            .pending_metadata_editor
            .as_ref()
            .expect("duplicate must leave parked editor intact");
        assert_eq!(state.active_surface().entries[0].value, "Applied once");
        assert_eq!(app.status_message, accepted_status);
    }

    #[test]
    fn parked_editor_file_import_is_last_request_wins() {
        let mut state = MetadataEditorState::for_files(
            vec![PathBuf::from("/tmp/editor.flac")],
            vec![title("Original")],
            vec!["editor.flac".to_string()],
            MetadataTechnicalDetails::default(),
        );
        let (older_request, older_cancel) = state.begin_tag_transfer_preparation();
        let (newer_request, _newer_cancel) = state.begin_tag_transfer_preparation();
        assert!(older_cancel.is_cancelled());
        let editor_session = metadata_editor_session_guard(&state);
        let editor_fingerprint =
            super::super::tag_interchange::metadata_editor_transfer_fingerprint(&state);

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.pending_metadata_editor = Some(Box::new(state));
        app.active_overlay = ActiveOverlay::Help {
            screen: AppScreen::Browse,
            scroll: 0,
        };
        app.set_status("newer file import pending");

        handle_message(
            &mut app,
            AppMessage::MetadataTagBlocksFilePrepared {
                request_id: older_request,
                editor_session,
                editor_fingerprint,
                path: PathBuf::from("/tmp/older.txt"),
                result: Ok(vec![super::super::tag_interchange::FieldBlock {
                    key: "TITLE".to_string(),
                    values: crate::tui::probe::metadata_field_values_from_scalars(vec!["Older".to_string()]),
                }]),
            },
            &tx(),
        );
        assert_eq!(
            app.pending_metadata_editor
                .as_ref()
                .expect("editor remains parked")
                .active_surface()
                .entries[0]
                .value,
            "Original"
        );
        assert_eq!(
            app.status_message.as_ref().map(|(message, _)| message.as_str()),
            Some("newer file import pending")
        );

        handle_message(
            &mut app,
            AppMessage::MetadataTagBlocksFilePrepared {
                request_id: newer_request,
                editor_session,
                editor_fingerprint,
                path: PathBuf::from("/tmp/newer.txt"),
                result: Ok(vec![super::super::tag_interchange::FieldBlock {
                    key: "TITLE".to_string(),
                    values: crate::tui::probe::metadata_field_values_from_scalars(vec!["Newer".to_string()]),
                }]),
            },
            &tx(),
        );
        let state = app
            .pending_metadata_editor
            .as_ref()
            .expect("newer completion restores parked editor");
        assert_eq!(state.active_surface().entries[0].value, "Newer");
        assert!(!state.owns_tag_transfer_preparation(newer_request));
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
                allow_rename: false,
                allow_duplicate: false,
                symlink_copy: tui_file_picker::SymlinkCopyPolicy::Reject,
                cross_device_cut: tui_file_picker::CrossDeviceCutPolicy::Reject,
                delete: tui_file_picker::DeletePolicy::FilesAndEmptyDirectories,
                verbose_degrade_notices: false,
                verification: tui_file_picker::VerificationMode::Standard,
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
                paths: Vec::new(),
                ignored_directories: 0,
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
                paths: Vec::new(),
                ignored_directories: 0,
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
                paths: Vec::new(),
                ignored_directories: 0,
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
                paths: Vec::new(),
                ignored_directories: 0,
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
                paths: Vec::new(),
                ignored_directories: 0,
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
                paths: Vec::new(),
                ignored_directories: 0,
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
                paths: vec![dest.join("one"), dest.join("two")],
                ignored_directories: 3,
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
        assert!(
            !status.unwrap_or_default().contains("first of")
                && !status.unwrap_or_default().contains("director"),
            "discarded completions must not append selection disclosures: {status:?}"
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
                paths: Vec::new(),
                ignored_directories: 0,
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

    fn completion_report(message: &str) -> tui_file_picker::FileTaskCompletionReport {
        tui_file_picker::FileTaskCompletionReport {
            is_move: false,
            roots: vec![tui_file_picker::FileTaskRootResult {
                source: std::path::PathBuf::from("source.bin"),
                destination: std::path::PathBuf::from("destination.bin"),
                disposition: tui_file_picker::FileTaskRootDisposition::CompletedWithWarning,
                message: Some(message.to_string()),
                undo_disposition: tui_file_picker::FileTaskUndoDisposition::NotReversible,
                proof: None,
            }],
        }
    }

    fn clean_completion_report() -> tui_file_picker::FileTaskCompletionReport {
        tui_file_picker::FileTaskCompletionReport {
            is_move: false,
            roots: vec![tui_file_picker::FileTaskRootResult {
                source: std::path::PathBuf::from("source.bin"),
                destination: std::path::PathBuf::from("destination.bin"),
                disposition: tui_file_picker::FileTaskRootDisposition::Completed,
                message: None,
                undo_disposition: tui_file_picker::FileTaskUndoDisposition::NotReversible,
                proof: None,
            }],
        }
    }

    fn install_auto_close_progress(app: &mut AppState) -> u64 {
        let mut progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Copy,
            "Copying files",
            super::super::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        progress.set_auto_close(true);
        let (control_tx, _control_rx) = std::sync::mpsc::channel();
        let session = super::super::app::FileTaskProgressSession::new(progress, control_tx);
        let session_id = session.session_id;
        app.install_file_task_progress(session);
        session_id
    }

    #[test]
    fn clean_file_task_auto_closes_only_after_retaining_authoritative_report() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let session_id = install_auto_close_progress(&mut app);
        let report = clean_completion_report();
        let (tx, _rx) = mpsc::channel(4);

        reduce_file_task_complete(&mut app, session_id, report.clone(), None, &tx);

        assert!(matches!(app.active_overlay, ActiveOverlay::None));
        let (retained_id, retained) = app
            .last_file_task_progress
            .as_ref()
            .expect("auto-close must retain diagnostics");
        assert_eq!(*retained_id, session_id);
        assert_eq!(retained.completion_report(), Some(&report));
    }

    #[test]
    fn warning_file_task_ignores_auto_close_and_remains_visible() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let session_id = install_auto_close_progress(&mut app);
        let report = completion_report("durability warning");
        let (tx, _rx) = mpsc::channel(4);

        reduce_file_task_complete(&mut app, session_id, report.clone(), None, &tx);

        let ActiveOverlay::FileTaskProgress(session) = &app.active_overlay else {
            panic!("warning completion must remain open");
        };
        assert_eq!(session.session_id, session_id);
        assert_eq!(session.progress.completion_report(), Some(&report));
    }

    #[test]
    fn file_task_retention_is_session_owned_after_the_live_overlay_closes() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Copy,
            "Copying files",
            super::super::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        let (control_tx, _control_rx) = std::sync::mpsc::channel();
        let session = super::super::app::FileTaskProgressSession::new(progress, control_tx);
        let session_id = session.session_id;
        app.install_file_task_progress(session);
        app.active_overlay = ActiveOverlay::None;
        let (tx, _rx) = mpsc::channel(4);

        reduce_file_task_progress(
            &mut app,
            session_id,
            tui_file_picker::FileTaskProgressUpdate::Snapshot {
                phase: tui_file_picker::FileTaskPhase::Verifying,
                status: "Verifying retained task".to_string(),
                current_item: None,
                totals: tui_file_picker::ProgressTotals::default(),
                rate_bytes_per_sec: None,
            },
            &tx,
        );
        let report = completion_report("portable proof accepted");
        reduce_file_task_complete(&mut app, session_id, report.clone(), None, &tx);

        let (retained_session_id, retained) = app
            .last_file_task_progress
            .as_ref()
            .expect("session-owned retained state");
        assert_eq!(*retained_session_id, session_id);
        assert!(retained.is_terminal());
        assert_eq!(retained.status, "File task completed with warnings");
        assert_eq!(retained.completion_report(), Some(&report));
    }

    #[test]
    fn terminal_completion_reconstructs_missing_state_and_replaces_only_older_sessions() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let old = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Copy,
            "Old task",
            super::super::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        app.last_file_task_progress = Some((7, old));
        app.active_overlay = ActiveOverlay::None;
        let (tx, _rx) = mpsc::channel(4);
        let newest_report = completion_report("newest full diagnostic");

        reduce_file_task_complete(&mut app, 8, newest_report.clone(), None, &tx);

        let (session_id, retained) = app
            .last_file_task_progress
            .as_ref()
            .expect("reconstructed terminal state");
        assert_eq!(*session_id, 8);
        assert!(retained.is_terminal());
        assert_eq!(retained.completion_report(), Some(&newest_report));

        let stale_report = completion_report("stale diagnostic");
        reduce_file_task_complete(&mut app, 6, stale_report, None, &tx);
        let (session_id, retained) = app.last_file_task_progress.as_ref().unwrap();
        assert_eq!(*session_id, 8);
        assert_eq!(retained.completion_report(), Some(&newest_report));
    }

    fn terminal_progress_state(app: &AppState, status: &str) -> tui_file_picker::FileTaskProgressState {
        let mut progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Move,
            "Moving files",
            super::super::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        progress.apply_update(tui_file_picker::FileTaskProgressUpdate::Finished {
            status: status.to_string(),
            totals: tui_file_picker::ProgressTotals::default(),
        });
        progress
    }

    fn close_file_task_results_overlay(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
        super::super::keybindings::handle_key(
            app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ),
            tx,
        );
        super::super::keybindings::handle_key(
            app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ),
            tx,
        );
        assert!(matches!(app.active_overlay, ActiveOverlay::None));
    }

    #[test]
    fn closing_retained_viewer_before_real_completion_cannot_advance_session_lineage() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let source_session_id = 77;
        let retained_progress = terminal_progress_state(&app, "terminal update received");
        app.last_file_task_progress = Some((source_session_id, retained_progress));
        let (tx, _rx) = mpsc::channel(8);

        super::super::command::execute_command(
            &mut app,
            super::super::command::Command::FileTaskMessages,
            &tx,
        );
        let ActiveOverlay::FileTaskProgress(viewer) = &app.active_overlay else {
            panic!("retained viewer must open");
        };
        assert_eq!(viewer.session_id, source_session_id);
        assert!(viewer.is_retained_viewer());

        close_file_task_results_overlay(&mut app, &tx);
        assert_eq!(
            app.last_file_task_progress.as_ref().map(|(id, _)| *id),
            Some(source_session_id),
            "dismissing a viewer must not synthesize a newer task session"
        );

        let report = completion_report("completion arrived after viewer closed");
        reduce_file_task_complete(
            &mut app,
            source_session_id,
            report.clone(),
            None,
            &tx,
        );
        let (retained_id, retained) = app.last_file_task_progress.as_ref().unwrap();
        assert_eq!(*retained_id, source_session_id);
        assert_eq!(retained.completion_report(), Some(&report));
    }

    #[test]
    fn completion_while_retained_viewer_is_open_survives_later_viewer_close() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let source_session_id = 91;
        let retained_progress = terminal_progress_state(&app, "terminal update received");
        app.last_file_task_progress = Some((source_session_id, retained_progress));
        let (tx, _rx) = mpsc::channel(8);

        super::super::command::execute_command(
            &mut app,
            super::super::command::Command::FileTaskMessages,
            &tx,
        );
        let report = completion_report("authoritative report while viewer open");
        reduce_file_task_complete(
            &mut app,
            source_session_id,
            report.clone(),
            None,
            &tx,
        );

        let ActiveOverlay::FileTaskProgress(viewer) = &app.active_overlay else {
            panic!("retained viewer must remain open");
        };
        assert!(viewer.is_retained_viewer());
        assert_eq!(viewer.session_id, source_session_id);
        assert_eq!(viewer.progress.completion_report(), Some(&report));
        assert_eq!(
            app.last_file_task_progress
                .as_ref()
                .and_then(|(_, retained)| retained.completion_report()),
            Some(&report)
        );

        close_file_task_results_overlay(&mut app, &tx);
        let (retained_id, retained) = app.last_file_task_progress.as_ref().unwrap();
        assert_eq!(*retained_id, source_session_id);
        assert_eq!(
            retained.completion_report(),
            Some(&report),
            "closing a stale viewer clone must never overwrite authoritative completion data"
        );
    }

    #[test]
    fn terminal_progress_without_any_overlay_or_seed_still_becomes_reopenable() {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::None;
        app.last_file_task_progress = None;
        let (tx, _rx) = mpsc::channel(4);
        let totals = tui_file_picker::ProgressTotals {
            items_done: 1,
            items_total: Some(1),
            errors: 1,
            ..tui_file_picker::ProgressTotals::default()
        };

        reduce_file_task_progress(
            &mut app,
            41,
            tui_file_picker::FileTaskProgressUpdate::Failed {
                status: "Full retained failure text".to_string(),
                totals,
            },
            &tx,
        );

        let (session_id, retained) = app
            .last_file_task_progress
            .as_ref()
            .expect("terminal fallback state");
        assert_eq!(*session_id, 41);
        assert!(retained.is_terminal());
        assert_eq!(retained.status, "Full retained failure text");
        assert!(retained.has_details());
    }

    fn test_cached_info(file_size: u64, title: &str) -> crate::tui::browse::CachedInfo {
        crate::tui::browse::CachedInfo {
            source: crate::tui::probe::SourceInfo {
                format_name: "FLAC".to_string(),
                codec: "flac".to_string(),
                bit_depth: Some(16),
                sample_format_is_float: None,
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
            tab_id: app.browse.active_tab_id(),
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

        let __tab_id_fix = app.browse.active_tab_id();
        handle_message(
            &mut app,
            AppMessage::ProbeCacheWarmComplete {
                tab_id: __tab_id_fix,
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
            embedded_cue_availability:
                crate::tui::probe::EmbeddedCueAvailability::Unknown,
            cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
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
    #[test]
    fn completion_reducer_trusts_worker_report_without_probing_missing_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_a = temp.path().join("source-a");
        let source_b = temp.path().join("source-b");
        let destination_a = temp.path().join("destination-a");
        let destination_b = temp.path().join("destination-b");
        std::fs::create_dir_all(destination_a.join("current")).expect("destination a");
        std::fs::create_dir_all(destination_b.join("history")).expect("destination b");

        let mapping_a = tui_file_picker::PasteMapping {
            source: source_a.clone(),
            destination: destination_a.clone(),
        };
        let mapping_b = tui_file_picker::PasteMapping {
            source: source_b.clone(),
            destination: destination_b.clone(),
        };
        let clipboard = tui_file_picker::FilesystemClipboard::new(
            tui_file_picker::FilePickerClipboardMode::Cut,
            vec![source_a.clone(), source_b.clone()],
        )
        .expect("clipboard");
        let plan = tui_file_picker::PastePlan {
            mode: tui_file_picker::FilePickerClipboardMode::Cut,
            mappings: vec![mapping_a.clone(), mapping_b.clone()],
        };
        let session_id = 991;
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = crate::tui::app::AppScreen::Browse;
        app.browse.current_dir = source_a.join("current");
        app.browse.nav_history = vec![
            source_a.join("current"),
            source_b.join("history"),
        ];
        app.browse.nav_history_index = 0;
        app.browse.filesystem_clipboard = Some(clipboard.clone());
        let mut retry_plan = crate::tui::browse::BrowsePasteRetryPlan::from_plan(plan.clone());
        retry_plan.recovery_journal_path = Some(temp.path().join("recovery.jsonl"));
        app.file_transfers.pending_by_session.insert(
            session_id,
            crate::tui::browse::PendingClipboardPaste {
                session_id,
                clipboard,
                clipboard_owner_generation: Some(
                    app.browse.filesystem_clipboard_generation,
                ),
                retry_plan: Some(retry_plan.clone()),
            },
        );
        app.file_transfers.active_session_id = Some(session_id);
        let report = tui_file_picker::FileTaskCompletionReport {
            is_move: true,
            roots: vec![
                tui_file_picker::FileTaskRootResult {
                    source: source_a.clone(),
                    destination: destination_a.clone(),
                    disposition: tui_file_picker::FileTaskRootDisposition::Completed,
                    message: None,
                    undo_disposition: tui_file_picker::FileTaskUndoDisposition::NotReversible,
                    proof: None,
                },
                tui_file_picker::FileTaskRootResult {
                    source: source_b.clone(),
                    destination: destination_b.clone(),
                    disposition: tui_file_picker::FileTaskRootDisposition::Failed,
                    message: Some(
                        "destination complete; source partially removed from quarantine"
                            .to_string(),
                    ),
                    undo_disposition: tui_file_picker::FileTaskUndoDisposition::NotReversible,
                    proof: None,
                },
            ],
        };
        let (tx, _rx) = mpsc::channel(8);

        reduce_file_task_complete(
            &mut app,
            session_id,
            report,
            Some(retry_plan),
            &tx,
        );

        assert_eq!(app.browse.current_dir, destination_a.join("current"));
        assert_eq!(
            app.browse.nav_history,
            vec![destination_a.join("current"), source_b.join("history")]
        );
        let retained = app
            .browse
            .filesystem_clipboard
            .as_ref()
            .expect("failed root remains retryable without a TUI-thread stat");
        assert_eq!(retained.paths(), &[source_b.clone()]);
        let retained_retry = app
            .browse
            .filesystem_clipboard_retry_plan
            .as_ref()
            .expect("durable exact retry plan retained");
        assert_eq!(retained_retry.plan.mappings, vec![mapping_b]);
        assert!(retained_retry.recovery_journal_path.is_some());
    }

    #[test]
    fn successive_job_completions_reconcile_clipboard_and_undo_per_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let (tx, _rx) = mpsc::channel(8);

        for (session_id, name) in [(1201_u64, "first"), (1202_u64, "second")] {
            let source = temp.path().join(format!("{name}-source.flac"));
            let destination = temp.path().join(format!("{name}-copy.flac"));
            std::fs::write(&source, format!("{name} bytes")).expect("source");
            let source_manifest =
                tui_file_picker::capture_manifest(&source).expect("source proof");
            std::fs::copy(&source, &destination).expect("copy fixture");
            let destination_manifest = source_manifest
                .capture_verified_copy_at(&destination)
                .expect("destination proof");
            let clipboard = tui_file_picker::FilesystemClipboard::new(
                tui_file_picker::FilePickerClipboardMode::Copy,
                vec![source.clone()],
            )
            .expect("clipboard");
            app.browse
                .replace_filesystem_clipboard_from_user(clipboard.clone());
            let owner_generation = app.browse.filesystem_clipboard_generation;
            app.file_transfers.pending_by_session.insert(
                session_id,
                crate::tui::browse::PendingClipboardPaste {
                    session_id,
                    clipboard: clipboard.clone(),
                    clipboard_owner_generation: Some(owner_generation),
                    retry_plan: None,
                },
            );
            app.file_transfers.active_session_id = Some(session_id);
            let report = tui_file_picker::FileTaskCompletionReport {
                is_move: false,
                roots: vec![tui_file_picker::FileTaskRootResult {
                    source,
                    destination,
                    disposition: tui_file_picker::FileTaskRootDisposition::Completed,
                    message: None,
                    undo_disposition:
                        tui_file_picker::FileTaskUndoDisposition::CreatedDestination,
                    proof: Some(tui_file_picker::FileTaskRootProof {
                        source_manifest,
                        destination_manifest,
                    }),
                }],
            };

            reduce_file_task_complete(&mut app, session_id, report, None, &tx);

            assert_eq!(app.browse.filesystem_clipboard.as_ref(), Some(&clipboard));
            assert!(!app
                .file_transfers
                .pending_by_session
                .contains_key(&session_id));
            assert_eq!(app.file_transfers.active_session_id, None);
        }

        assert_eq!(app.file_operation_undo.depths(), (2, 0));
    }

    #[test]
    fn older_completion_cannot_repair_an_identical_newer_clipboard_revision() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source.flac");
        let destination = temp.path().join("destination.flac");
        let clipboard = tui_file_picker::FilesystemClipboard::new(
            tui_file_picker::FilePickerClipboardMode::Cut,
            vec![source.clone()],
        )
        .expect("clipboard");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.browse
            .replace_filesystem_clipboard_from_user(clipboard.clone());
        let older_generation = app.browse.filesystem_clipboard_generation;
        // The user deliberately copies the identical paths again. Value
        // equality cannot establish ownership; the revision must change.
        app.browse
            .replace_filesystem_clipboard_from_user(clipboard.clone());
        let newer_generation = app.browse.filesystem_clipboard_generation;
        assert_ne!(older_generation, newer_generation);

        let session_id = 992;
        app.file_transfers.pending_by_session.insert(
            session_id,
            crate::tui::browse::PendingClipboardPaste {
                session_id,
                clipboard: clipboard.clone(),
                clipboard_owner_generation: Some(older_generation),
                retry_plan: None,
            },
        );
        app.file_transfers.active_session_id = Some(session_id);
        let report = tui_file_picker::FileTaskCompletionReport {
            is_move: true,
            roots: vec![tui_file_picker::FileTaskRootResult {
                source,
                destination,
                disposition: tui_file_picker::FileTaskRootDisposition::Failed,
                message: Some("simulated failure".to_string()),
                undo_disposition: tui_file_picker::FileTaskUndoDisposition::NotReversible,
                proof: None,
            }],
        };
        let (tx, _rx) = mpsc::channel(8);

        reduce_file_task_complete(&mut app, session_id, report, None, &tx);

        assert_eq!(app.browse.filesystem_clipboard_generation, newer_generation);
        assert_eq!(app.browse.filesystem_clipboard.as_ref(), Some(&clipboard));
        assert!(
            app.browse.filesystem_clipboard_retry_plan.is_none(),
            "the older job must not install its retry token over the newer user action"
        );
    }

    #[test]
    fn bounded_tag_block_file_reader_accepts_valid_utf8_blocks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tags.txt");
        std::fs::write(&path, b"TITLE\nOne\nTwo\n\nARTIST\nGenesis\n")
            .expect("write tag blocks");

        let cancel = crate::tui::probe::MetadataWriteCancelFlag::new();
        let blocks = read_tag_blocks_file_bounded(&path, &cancel).expect("valid blocks");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].key, "TITLE");
        assert_eq!(blocks[0].values, vec!["One", "Two"]);
        assert_eq!(blocks[1].key, "ARTIST");
        assert_eq!(blocks[1].values, vec!["Genesis"]);
    }

    #[test]
    fn bounded_tag_block_file_reader_rejects_invalid_utf8() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tags.txt");
        std::fs::write(&path, [0xff, 0xfe, 0xfd]).expect("write invalid utf8");

        let cancel = crate::tui::probe::MetadataWriteCancelFlag::new();
        let error =
            read_tag_blocks_file_bounded(&path, &cancel).expect_err("invalid utf8 must fail");
        assert!(error.contains("not valid UTF-8"), "unexpected error: {error}");
    }

    #[test]
    fn bounded_tag_block_file_reader_rejects_oversized_regular_file_before_reading() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tags.txt");
        let file = std::fs::File::create(&path).expect("create sparse file");
        file.set_len(MAX_TAG_BLOCK_FILE_BYTES + 1)
            .expect("extend sparse file");

        let cancel = crate::tui::probe::MetadataWriteCancelFlag::new();
        let error =
            read_tag_blocks_file_bounded(&path, &cancel).expect_err("oversized file must fail");
        assert!(error.contains("too large"), "unexpected error: {error}");
    }

    #[test]
    fn bounded_tag_block_file_reader_honors_pre_cancel() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("tags.txt");
        std::fs::write(&path, b"TITLE\nOne\n").expect("write tag blocks");
        let cancel = crate::tui::probe::MetadataWriteCancelFlag::new();
        cancel.cancel();

        let error = read_tag_blocks_file_bounded(&path, &cancel)
            .expect_err("cancelled read must not start");
        assert_eq!(error, "tag-block file read cancelled");
    }

}

#[cfg(test)]
mod minimized_file_transfer_attention_tests {
    use super::*;
    use crate::tui::app::{
        ActiveOverlay, AppState, ConfirmAction, FileTaskProgressSession,
    };

    #[test]
    fn minimized_footer_state_tracks_live_progress_and_fifo_depth() {
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        let mut progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Copy,
            "Copying files",
            super::super::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        progress.apply_update(tui_file_picker::FileTaskProgressUpdate::Snapshot {
            phase: tui_file_picker::FileTaskPhase::Running,
            status: "copying".to_string(),
            current_item: None,
            totals: tui_file_picker::ProgressTotals {
                bytes_done: 61,
                bytes_total: Some(100),
                ..tui_file_picker::ProgressTotals::default()
            },
            rate_bytes_per_sec: None,
        });
        let (control_tx, _control_rx) = std::sync::mpsc::channel();
        let session = FileTaskProgressSession::new(progress, control_tx);
        let session_id = session.session_id;
        app.minimized_file_task_progress = Some(session);
        app.file_transfers.active_session_id = Some(session_id);
        let clipboard = tui_file_picker::FilesystemClipboard::new(
            tui_file_picker::FilePickerClipboardMode::Copy,
            vec![std::path::PathBuf::from("/source/queued")],
        )
        .expect("clipboard");
        let plan = tui_file_picker::PastePlan {
            mode: tui_file_picker::FilePickerClipboardMode::Copy,
            mappings: vec![tui_file_picker::PasteMapping {
                source: std::path::PathBuf::from("/source/queued"),
                destination: std::path::PathBuf::from("/destination/queued"),
            }],
        };
        app.file_transfers.queued.push_back(crate::tui::app::QueuedFileTransfer {
            queue_id: 91,
            clipboard,
            clipboard_owner_generation: None,
            destination_dir: std::path::PathBuf::from("/destination"),
            enqueue_plan: plan.clone(),
            retry_plan: Some(crate::tui::browse::BrowsePasteRetryPlan::from_plan(plan)),
            recovered: false,
        });
        app.sync_file_transfer_queue_surfaces();

        let footer = app.file_task_footer_state().expect("live footer");
        assert!(footer.live);
        assert_eq!(footer.ratio, Some(0.61));
        assert_eq!(footer.queued, 1);

        app.file_transfers.queued.clear();
        app.sync_file_transfer_queue_surfaces();
        let footer = app.file_task_footer_state().expect("live footer after transition");
        assert_eq!(footer.ratio, Some(0.61));
        assert_eq!(footer.queued, 0);
    }

    #[test]
    fn minimized_fifo_install_preserves_unrelated_modal() {
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::Confirmation {
            message: "keep this modal".to_string(),
            action: ConfirmAction::ClearQueue,
        };
        let progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Copy,
            "Copying files",
            super::super::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        let (control_tx, _control_rx) = std::sync::mpsc::channel();
        let session = FileTaskProgressSession::new(progress, control_tx);
        let session_id = session.session_id;

        app.install_file_task_progress_with_visibility(session, true);

        assert!(matches!(
            &app.active_overlay,
            ActiveOverlay::Confirmation { message, .. } if message == "keep this modal"
        ));
        assert!(app
            .minimized_file_task_progress
            .as_ref()
            .is_some_and(|session| session.session_id == session_id));
    }

    #[test]
    fn visible_archive_install_preserves_scheduler_owned_minimized_transfer() {
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        let mut transfer_progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Copy,
            "Copying files",
            super::super::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        transfer_progress.apply_update(tui_file_picker::FileTaskProgressUpdate::Snapshot {
            phase: tui_file_picker::FileTaskPhase::Running,
            status: "copying before archive save".to_string(),
            current_item: None,
            totals: tui_file_picker::ProgressTotals {
                bytes_done: 25,
                bytes_total: Some(100),
                ..tui_file_picker::ProgressTotals::default()
            },
            rate_bytes_per_sec: None,
        });
        let (transfer_tx, _transfer_rx) = std::sync::mpsc::channel();
        let transfer_session = FileTaskProgressSession::new(transfer_progress, transfer_tx);
        let transfer_session_id = transfer_session.session_id;
        app.file_transfers.active_session_id = Some(transfer_session_id);
        app.install_file_task_progress_with_visibility(transfer_session, true);

        let clipboard = tui_file_picker::FilesystemClipboard::new(
            tui_file_picker::FilePickerClipboardMode::Copy,
            vec![std::path::PathBuf::from("/source/queued")],
        )
        .expect("clipboard");
        let plan = tui_file_picker::PastePlan {
            mode: tui_file_picker::FilePickerClipboardMode::Copy,
            mappings: vec![tui_file_picker::PasteMapping {
                source: std::path::PathBuf::from("/source/queued"),
                destination: std::path::PathBuf::from("/destination/queued"),
            }],
        };
        app.file_transfers.queued.push_back(crate::tui::app::QueuedFileTransfer {
            queue_id: 92,
            clipboard,
            clipboard_owner_generation: None,
            destination_dir: std::path::PathBuf::from("/destination"),
            enqueue_plan: plan.clone(),
            retry_plan: Some(crate::tui::browse::BrowsePasteRetryPlan::from_plan(plan)),
            recovered: false,
        });
        app.sync_file_transfer_queue_surfaces();

        let archive_progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Archive,
            "Repackaging archive",
            super::super::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        let (archive_tx, _archive_rx) = std::sync::mpsc::channel();
        let archive_session = FileTaskProgressSession::new(archive_progress, archive_tx);
        let archive_session_id = archive_session.session_id;
        assert!(archive_session_id > transfer_session_id);

        app.install_file_task_progress(archive_session);

        assert!(app
            .minimized_file_task_progress
            .as_ref()
            .is_some_and(|session| session.session_id == transfer_session_id));
        assert!(matches!(
            &app.active_overlay,
            ActiveOverlay::FileTaskProgress(session)
                if session.session_id == archive_session_id
                    && matches!(&session.progress.kind, tui_file_picker::FileTaskKind::Archive)
        ));
        let footer = app.file_task_footer_state().expect("minimized transfer footer");
        assert_eq!(footer.ratio, Some(0.25));
        assert_eq!(footer.queued, 1);

        let (tx, _rx) = mpsc::channel(8);
        reduce_file_task_progress(
            &mut app,
            transfer_session_id,
            tui_file_picker::FileTaskProgressUpdate::Snapshot {
                phase: tui_file_picker::FileTaskPhase::Running,
                status: "copying after archive save started".to_string(),
                current_item: None,
                totals: tui_file_picker::ProgressTotals {
                    bytes_done: 70,
                    bytes_total: Some(100),
                    ..tui_file_picker::ProgressTotals::default()
                },
                rate_bytes_per_sec: None,
            },
            &tx,
        );

        assert!(app.minimized_file_task_progress.as_ref().is_some_and(|session| {
            session.session_id == transfer_session_id
                && session.progress.status == "copying after archive save started"
                && session.progress.totals.bytes_done == 70
        }));
        assert!(matches!(
            &app.active_overlay,
            ActiveOverlay::FileTaskProgress(session) if session.session_id == archive_session_id
        ));
        assert_eq!(
            app.file_task_footer_state().and_then(|footer| footer.ratio),
            Some(0.70)
        );
    }

    #[test]
    fn transfer_attention_preempts_archive_and_parks_its_exact_session() {
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        let transfer_progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Move,
            "Moving files",
            super::super::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        let (transfer_tx, _transfer_rx) = std::sync::mpsc::channel();
        let transfer_session = FileTaskProgressSession::new(transfer_progress, transfer_tx);
        let transfer_session_id = transfer_session.session_id;
        app.file_transfers.active_session_id = Some(transfer_session_id);
        app.file_transfers.keep_minimized_across_jobs = true;
        app.install_file_task_progress_with_visibility(transfer_session, true);

        let archive_progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Archive,
            "Repackaging archive",
            super::super::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        let (archive_tx, _archive_rx) = std::sync::mpsc::channel();
        let archive_session = FileTaskProgressSession::new(archive_progress, archive_tx);
        let archive_session_id = archive_session.session_id;
        app.install_file_task_progress(archive_session);
        let conflict = tui_file_picker::ConflictPromptState::new(
            93,
            "Destination changed",
            "Choose how to continue",
            tui_file_picker::ConflictItemKind::File,
        );
        let (tx, _rx) = mpsc::channel(8);

        reduce_file_task_progress(
            &mut app,
            transfer_session_id,
            tui_file_picker::FileTaskProgressUpdate::ShowConflict {
                conflict: conflict.clone(),
            },
            &tx,
        );

        assert!(app.minimized_file_task_progress.is_none());
        assert!(!app.file_transfers.keep_minimized_across_jobs);
        assert!(matches!(
            &app.active_overlay,
            ActiveOverlay::FileTaskProgress(session)
                if session.session_id == transfer_session_id
                    && session.progress.conflict.as_ref() == Some(&conflict)
        ));
        assert!(matches!(
            app.file_task_preempted_overlay.as_deref(),
            Some(ActiveOverlay::FileTaskProgress(session))
                if session.session_id == archive_session_id
                    && matches!(&session.progress.kind, tui_file_picker::FileTaskKind::Archive)
        ));
    }

    #[test]
    fn conflict_while_minimized_restores_progress_without_dropping_displaced_overlay() {
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        let progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Copy,
            "Copying files",
            super::super::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        let (control_tx, _control_rx) = std::sync::mpsc::channel();
        let session = FileTaskProgressSession::new(progress, control_tx);
        let session_id = session.session_id;
        app.minimized_file_task_progress = Some(session);
        app.file_transfers.active_session_id = Some(session_id);
        app.file_transfers.keep_minimized_across_jobs = true;
        app.active_overlay = ActiveOverlay::Confirmation {
            message: "preserve this modal".to_string(),
            action: ConfirmAction::ClearQueue,
        };
        let conflict = tui_file_picker::ConflictPromptState::new(
            17,
            "Destination changed",
            "Choose how to continue",
            tui_file_picker::ConflictItemKind::File,
        );
        let (tx, _rx) = mpsc::channel(8);

        reduce_file_task_progress(
            &mut app,
            session_id,
            tui_file_picker::FileTaskProgressUpdate::ShowConflict {
                conflict: conflict.clone(),
            },
            &tx,
        );

        assert!(app.minimized_file_task_progress.is_none());
        assert!(!app.file_transfers.keep_minimized_across_jobs);
        let ActiveOverlay::FileTaskProgress(restored) = &app.active_overlay else {
            panic!("attention-demanding state must restore the live progress overlay");
        };
        assert_eq!(restored.session_id, session_id);
        assert_eq!(restored.progress.conflict.as_ref(), Some(&conflict));
        assert!(matches!(
            app.file_task_preempted_overlay.as_deref(),
            Some(ActiveOverlay::Confirmation { message, .. }) if message == "preserve this modal"
        ));
    }

    #[test]
    fn clean_single_job_completion_preserves_visible_terminal_overlay() {
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        let progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Copy,
            "Copying files",
            super::super::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        let (control_tx, _control_rx) = std::sync::mpsc::channel();
        let session = FileTaskProgressSession::new(progress, control_tx);
        let session_id = session.session_id;
        app.active_overlay = ActiveOverlay::FileTaskProgress(session);
        app.file_transfers.active_session_id = Some(session_id);
        let (tx, _rx) = mpsc::channel(8);

        finalize_file_transfer_scheduler(&mut app, session_id, false, &tx);

        assert_eq!(app.file_transfers.active_session_id, None);
        assert!(matches!(
            &app.active_overlay,
            ActiveOverlay::FileTaskProgress(session) if session.session_id == session_id
        ));
        assert!(app.file_transfers.queued.is_empty());
    }

    #[test]
    fn clean_completion_restores_preempted_modal_before_advancing_fifo() {
        let mut app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        let progress = tui_file_picker::FileTaskProgressState::new(
            tui_file_picker::FileTaskKind::Copy,
            "Copying files",
            super::super::keybindings::file_picker_theme_from_theme(&app.theme),
        );
        let (control_tx, _control_rx) = std::sync::mpsc::channel();
        let session = FileTaskProgressSession::new(progress, control_tx);
        let session_id = session.session_id;
        app.active_overlay = ActiveOverlay::FileTaskProgress(session);
        app.file_transfers.active_session_id = Some(session_id);
        app.file_task_preempted_overlay = Some(Box::new(ActiveOverlay::Confirmation {
            message: "return to this modal".to_string(),
            action: ConfirmAction::ClearQueue,
        }));
        let clipboard = tui_file_picker::FilesystemClipboard::new(
            tui_file_picker::FilePickerClipboardMode::Copy,
            vec![std::path::PathBuf::from("/source/next")],
        )
        .expect("clipboard");
        let plan = tui_file_picker::PastePlan {
            mode: tui_file_picker::FilePickerClipboardMode::Copy,
            mappings: vec![tui_file_picker::PasteMapping {
                source: std::path::PathBuf::from("/source/next"),
                destination: std::path::PathBuf::from("/destination/next"),
            }],
        };
        app.file_transfers.queued.push_back(crate::tui::app::QueuedFileTransfer {
            queue_id: 52,
            clipboard,
            clipboard_owner_generation: None,
            destination_dir: std::path::PathBuf::from("/destination"),
            enqueue_plan: plan.clone(),
            retry_plan: Some(crate::tui::browse::BrowsePasteRetryPlan::from_plan(plan)),
            recovered: false,
        });
        let (tx, _rx) = mpsc::channel(8);

        finalize_file_transfer_scheduler(&mut app, session_id, false, &tx);

        assert_eq!(app.file_transfers.active_session_id, None);
        assert_eq!(app.file_transfers.queued.len(), 1);
        assert!(matches!(
            &app.active_overlay,
            ActiveOverlay::Confirmation { message, .. } if message == "return to this modal"
        ));
        assert!(app.file_task_preempted_overlay.is_none());
    }
}

#[cfg(test)]
mod browse_tab_async_routing_tests {
    use super::*;
    use crate::config::TonepoetConfig;

    #[tokio::test]
    async fn completed_move_refreshes_background_source_tab_and_prunes_sidebar_node() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_parent = temp.path().join("source-parent");
        let destination = temp.path().join("destination");
        let moved = source_parent.join("moved-album");
        std::fs::create_dir_all(&moved).expect("moved source fixture");
        std::fs::create_dir_all(&destination).expect("destination fixture");

        let (tx, _rx) = mpsc::channel(32);
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.current_dir = source_parent.clone();
        app.browse.set_tx(tx.clone());
        let source_id = app.browse.active_tab_id();
        let parent_depth = app
            .browse
            .tree_nodes
            .iter()
            .find(|node| node.path == source_parent)
            .map(|node| node.depth)
            .unwrap_or(0);
        app.browse.tree_nodes.push(crate::tui::browse::BrowseTreeNode {
            path: moved.clone(),
            name: "moved-album".to_string(),
            depth: parent_depth.saturating_add(1),
            expanded: true,
            has_children: true,
        });

        assert!(app.browse.open_dir_in_new_tab(destination.clone(), true));
        let destination_id = app.browse.active_tab_id();
        assert_ne!(source_id, destination_id);
        let completed_destination = destination.join("moved-album");
        let destination_refreshed_tabs = refresh_browse_destination_views_after_file_task(
            &mut app,
            std::slice::from_ref(&completed_destination),
        );
        let destination_generation = app
            .browse
            .pending_scan_generation()
            .expect("destination refresh scan");

        refresh_browse_source_views_after_move(
            &mut app,
            std::slice::from_ref(&moved),
            &destination_refreshed_tabs,
        );

        assert_eq!(app.browse.active_tab_id(), destination_id);
        assert_eq!(
            app.browse.pending_scan_generation(),
            Some(destination_generation),
            "source invalidation must not restart/cancel the destination scan",
        );
        let source = app.browse.tab_mut(source_id).expect("source tab remains live");
        assert_eq!(source.current_dir, source_parent);
        assert!(
            source.pending_scan_generation().is_some(),
            "the background source parent needs its own tab-scoped refresh",
        );
        assert!(
            !source.tree_nodes.iter().any(|node| node.path == moved),
            "the stale expanded moved node must disappear immediately",
        );
    }

    #[tokio::test]
    async fn same_directory_move_does_not_restart_destination_scan_during_source_invalidation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path().join("same-dir");
        let moved_source = dir.join("old-album");
        let moved_destination = dir.join("moved-album");
        std::fs::create_dir_all(&dir).expect("same-dir fixture");

        let (tx, _rx) = mpsc::channel(16);
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.current_dir = dir.clone();
        app.browse.set_tx(tx);
        let tab_id = app.browse.active_tab_id();
        app.browse.tree_nodes.push(crate::tui::browse::BrowseTreeNode {
            path: moved_source.clone(),
            name: "old-album".to_string(),
            depth: 1,
            expanded: true,
            has_children: true,
        });

        let destination_refreshed_tabs = refresh_browse_destination_views_after_file_task(
            &mut app,
            std::slice::from_ref(&moved_destination),
        );
        assert!(destination_refreshed_tabs.contains(&tab_id));
        let destination_generation = app
            .browse
            .pending_scan_generation()
            .expect("destination refresh scan");

        refresh_browse_source_views_after_move(
            &mut app,
            std::slice::from_ref(&moved_source),
            &destination_refreshed_tabs,
        );

        assert_eq!(
            app.browse.pending_scan_generation(),
            Some(destination_generation),
            "source invalidation must not restart a tab already refreshed as the destination",
        );
        assert!(
            !app.browse.tree_nodes.iter().any(|node| node.path == moved_source),
            "source tree pruning still applies even when the scan itself is deduplicated",
        );
    }

    #[tokio::test]
    async fn partial_move_refreshes_active_source_parent_when_destination_scan_was_not_started() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_parent = temp.path().join("source-parent");
        let moved = source_parent.join("moved-album");
        std::fs::create_dir_all(&source_parent).expect("source parent fixture");

        let (tx, _rx) = mpsc::channel(16);
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.current_dir = source_parent.clone();
        app.browse.set_tx(tx);
        assert!(app.browse.pending_scan_generation().is_none());

        refresh_browse_source_views_after_move(
            &mut app,
            std::slice::from_ref(&moved),
            &std::collections::BTreeSet::new(),
        );

        assert!(
            app.browse.pending_scan_generation().is_some(),
            "partial completion has no destination refresh to cover the active source parent",
        );
    }

    #[tokio::test]
    async fn partial_cut_completion_refreshes_successful_source_parent_and_retains_failed_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_parent = temp.path().join("source-parent");
        let destination = temp.path().join("destination");
        let unrelated = temp.path().join("unrelated");
        let moved = source_parent.join("moved-album");
        let failed = source_parent.join("failed-album");
        std::fs::create_dir_all(&source_parent).expect("source parent fixture");
        std::fs::create_dir_all(&destination).expect("destination fixture");
        std::fs::create_dir_all(&unrelated).expect("unrelated fixture");

        let (tx, _rx) = mpsc::channel(32);
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.current_dir = source_parent.clone();
        app.browse.set_tx(tx.clone());
        let source_id = app.browse.active_tab_id();
        assert!(app.browse.pending_scan_generation().is_none());
        let parent_depth = app
            .browse
            .tree_nodes
            .iter()
            .find(|node| node.path == source_parent)
            .map(|node| node.depth)
            .unwrap_or(0);
        app.browse.tree_nodes.push(crate::tui::browse::BrowseTreeNode {
            path: moved.clone(),
            name: "moved-album".to_string(),
            depth: parent_depth.saturating_add(1),
            expanded: true,
            has_children: true,
        });

        assert!(app.browse.open_dir_in_new_tab(destination.clone(), true));
        let destination_id = app.browse.active_tab_id();
        assert_ne!(source_id, destination_id);
        let destination_generation_before = app
            .browse
            .pending_scan_generation()
            .expect("opening the destination tab starts its initial scan");

        assert!(app.browse.open_dir_in_new_tab(unrelated.clone(), true));
        let unrelated_id = app.browse.active_tab_id();
        assert_ne!(unrelated_id, source_id);
        assert_ne!(unrelated_id, destination_id);
        let unrelated_generation_before = app
            .browse
            .pending_scan_generation()
            .expect("opening the unrelated tab starts its initial scan");
        let unrelated_cursor_restore_target_before = app.browse.cursor_restore_target.clone();

        let clipboard = tui_file_picker::FilesystemClipboard::new(
            tui_file_picker::FilePickerClipboardMode::Cut,
            vec![moved.clone(), failed.clone()],
        )
        .expect("cut clipboard");
        app.browse
            .replace_filesystem_clipboard_from_user(clipboard.clone());
        let clipboard_owner_generation = app.browse.filesystem_clipboard_generation;
        let moved_destination = destination.join("moved-album");
        let failed_destination = destination.join("failed-album");
        let session_id = 1401;
        app.file_transfers.pending_by_session.insert(
            session_id,
            crate::tui::browse::PendingClipboardPaste {
                session_id,
                clipboard,
                clipboard_owner_generation: Some(clipboard_owner_generation),
                retry_plan: None,
            },
        );
        app.file_transfers.active_session_id = Some(session_id);
        let report = tui_file_picker::FileTaskCompletionReport {
            is_move: true,
            roots: vec![
                tui_file_picker::FileTaskRootResult {
                    source: moved.clone(),
                    destination: moved_destination,
                    disposition: tui_file_picker::FileTaskRootDisposition::Completed,
                    message: None,
                    undo_disposition: tui_file_picker::FileTaskUndoDisposition::NotReversible,
                    proof: None,
                },
                tui_file_picker::FileTaskRootResult {
                    source: failed.clone(),
                    destination: failed_destination,
                    disposition: tui_file_picker::FileTaskRootDisposition::Failed,
                    message: Some("simulated filesystem failure".to_string()),
                    undo_disposition: tui_file_picker::FileTaskUndoDisposition::NotReversible,
                    proof: None,
                },
            ],
        };

        reduce_file_task_complete(&mut app, session_id, report, None, &tx);

        assert_eq!(
            app.browse.active_tab_id(),
            unrelated_id,
            "completion must not switch away from the tab the user chose while the transfer ran",
        );
        assert_eq!(
            app.browse.pending_scan_generation(),
            Some(unrelated_generation_before),
            "an unrelated active tab must not be rescanned just because a transfer completed",
        );
        assert_eq!(
            app.browse.cursor_restore_target,
            unrelated_cursor_restore_target_before,
            "an unrelated active tab must not inherit the successful destination cursor target",
        );
        {
            let destination_tab = app
                .browse
                .tab_mut(destination_id)
                .expect("destination tab remains live");
            assert_eq!(destination_tab.current_dir, destination);
            assert!(
                destination_tab
                    .pending_scan_generation()
                    .is_some_and(|generation| generation > destination_generation_before),
                "the tab displaying the successful destination must receive the completion refresh",
            );
            assert_eq!(
                destination_tab.cursor_restore_target.as_deref(),
                Some("moved-album"),
                "the cursor target belongs to the destination tab, not whichever tab is active",
            );
        }
        let source = app.browse.tab_mut(source_id).expect("source tab remains live");
        assert_eq!(source.current_dir, source_parent);
        assert!(
            source.pending_scan_generation().is_some(),
            "a successful root must invalidate its source parent even when a sibling root fails",
        );
        assert!(
            !source.tree_nodes.iter().any(|node| node.path == moved),
            "the successfully moved path must be pruned immediately",
        );
        assert_eq!(
            app.browse
                .filesystem_clipboard
                .as_ref()
                .expect("failed root remains on the Cut clipboard")
                .paths(),
            &[failed]
        );
    }

    #[tokio::test]
    async fn colliding_scan_generations_complete_into_the_owning_background_tab() {
        let temp = tempfile::tempdir().expect("tempdir");
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        std::fs::create_dir(&a).expect("a");
        std::fs::create_dir(&b).expect("b");

        let (tx, _rx) = mpsc::channel(32);
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.current_dir = a.clone();
        app.browse.set_tx(tx.clone());
        app.browse.refresh();
        let a_id = app.browse.active_tab_id();
        let a_generation = app.browse.pending_scan_generation().expect("tab A scan");

        assert!(app.browse.open_dir_in_new_tab(b.clone(), true));
        let b_id = app.browse.active_tab_id();
        let b_generation = app.browse.pending_scan_generation().expect("tab B scan");
        assert_ne!(a_id, b_id);
        assert_eq!(
            a_generation, b_generation,
            "the test intentionally exercises colliding per-tab generations",
        );

        handle_message(
            &mut app,
            AppMessage::DirScanComplete {
                tab_id: a_id,
                generation: a_generation,
                path: a.clone(),
                parent_entry: None,
                dirs: Vec::new(),
                files: Vec::new(),
                classification_updates: Default::default(),
                error: None,
            },
            &tx,
        );

        assert_eq!(app.browse.active_tab_id(), b_id, "background completion cannot switch focus");
        assert_eq!(app.browse.current_dir, b, "active tab directory is untouched");
        assert_eq!(app.browse.pending_scan_generation(), Some(b_generation));
        let background = app.browse.tab_mut(a_id).expect("tab A remains live");
        assert_eq!(background.current_dir, a);
        assert_eq!(background.pending_scan_generation(), None, "tab A consumed its own completion");
    }

    #[tokio::test]
    async fn duplicated_tabs_retain_async_sender_for_background_path_validation_follow_on_scan() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source_dir = temp.path().join("source");
        let foreground_dir = temp.path().join("foreground");
        let target_dir = temp.path().join("target");
        std::fs::create_dir(&source_dir).expect("source");
        std::fs::create_dir(&foreground_dir).expect("foreground");
        std::fs::create_dir(&target_dir).expect("target");

        let (tx, _rx) = mpsc::channel(32);
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.current_dir = source_dir.clone();
        app.browse.set_tx(tx.clone());
        let source_id = app.browse.active_tab_id();

        assert!(app.browse.duplicate_tab());
        let duplicate_id = app.browse.active_tab_id();
        assert_ne!(source_id, duplicate_id);
        assert!(app.browse.is_async_enabled(), "active duplicate retains runtime sender");
        assert!(
            app.browse
                .tab_mut(source_id)
                .is_some_and(|browse| browse.is_async_enabled()),
            "detached source retains runtime sender",
        );

        // Give the foreground duplicate a visibly distinct context, then launch
        // :cd validation from the source and switch away before its completion.
        app.browse.current_dir = foreground_dir.clone();
        assert!(app.browse.switch_to_tab_id(source_id));
        assert!(app.browse.is_async_enabled());
        app.browse
            .navigate_to_str(target_dir.to_str().expect("utf8 target"))
            .expect("launch async path validation");
        let generation = app.browse.path_validation_generation;
        assert_eq!(
            app.browse.current_dir, source_dir,
            "runtime-enabled validation must not synchronously navigate on the TUI thread",
        );

        assert!(app.browse.switch_to_tab_id(duplicate_id));
        assert_eq!(app.browse.current_dir, foreground_dir);
        let foreground_entries: Vec<_> = app
            .browse
            .entries
            .iter()
            .map(|entry| (entry.path.clone(), entry.name.clone()))
            .collect();

        handle_message(
            &mut app,
            AppMessage::PathValidationComplete {
                tab_id: source_id,
                generation,
                origin_dir: source_dir.clone(),
                input: target_dir.display().to_string(),
                result: Ok(target_dir.clone()),
            },
            &tx,
        );

        assert_eq!(app.browse.active_tab_id(), duplicate_id);
        assert_eq!(app.browse.current_dir, foreground_dir);
        assert_eq!(
            app.browse
                .entries
                .iter()
                .map(|entry| (entry.path.clone(), entry.name.clone()))
                .collect::<Vec<_>>(),
            foreground_entries,
        );
        let background = app.browse.tab_mut(source_id).expect("source tab remains live");
        assert_eq!(background.current_dir, target_dir);
        assert!(background.is_async_enabled());
        assert!(
            background.pending_scan_generation().is_some(),
            "background path completion must launch an async directory scan rather than scan inline",
        );
    }

    #[tokio::test]
    async fn bookmark_activation_navigates_the_tab_that_owned_the_request() {
        let temp = tempfile::tempdir().expect("tempdir");
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        let bookmarked = temp.path().join("bookmarked");
        std::fs::create_dir(&a).expect("a");
        std::fs::create_dir(&b).expect("b");
        std::fs::create_dir(&bookmarked).expect("bookmarked");

        let (tx, _rx) = mpsc::channel(8);
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.current_dir = a.clone();
        let a_id = app.browse.active_tab_id();
        assert!(app.browse.open_dir_in_new_tab(b.clone(), true));
        let b_id = app.browse.active_tab_id();
        assert_ne!(a_id, b_id);

        app.bookmarks.overlay_open = true;
        let pending = app.bookmarks.begin_activation(
            bookmarked.clone(),
            crate::tui::bookmarks::BookmarkActivationSurface::Manager,
        );
        handle_message(
            &mut app,
            AppMessage::BookmarkActivationResolved {
                tab_id: b_id,
                generation: pending.generation,
                request_id: pending.request_id,
                path: bookmarked.clone(),
                result: Ok(()),
            },
            &tx,
        );

        assert_eq!(app.browse.active_tab_id(), b_id);
        assert_eq!(app.browse.current_dir, bookmarked);
        let first = app.browse.tab_mut(a_id).expect("first tab");
        assert_eq!(first.current_dir, a, "bookmark activation must not navigate tab 0");
    }

    #[tokio::test]
    async fn archive_listing_requests_coexist_and_complete_into_their_own_tabs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        std::fs::create_dir(&a).expect("a");
        std::fs::create_dir(&b).expect("b");
        let archive_a = a.join("a.zip");
        let archive_b = b.join("b.zip");
        std::fs::write(&archive_a, b"placeholder-a").expect("archive A");
        std::fs::write(&archive_b, b"placeholder-b").expect("archive B");

        let (tx, _rx) = mpsc::channel(8);
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.current_dir = a;
        let a_id = app.browse.active_tab_id();
        let (a_listing_id, a_cancel) = app.begin_archive_listing(archive_a.clone());

        assert!(app.browse.open_dir_in_new_tab(b.clone(), true));
        let b_id = app.browse.active_tab_id();
        assert_ne!(a_id, b_id);
        let (b_listing_id, b_cancel) = app.begin_archive_listing(archive_b.clone());

        assert_eq!(app.pending_archive_listings.len(), 2);
        assert!(app.archive_listing_pending_for(a_id, a_listing_id, &archive_a));
        assert!(app.archive_listing_pending_for(b_id, b_listing_id, &archive_b));
        assert!(!a_cancel.is_cancelled());
        assert!(!b_cancel.is_cancelled());

        let listing_a = crate::tui::archive_listing::ArchiveListing {
            archive_path: archive_a.clone(),
            format: "zip".to_string(),
            physical_size: 0,
            entries: Vec::new(),
        };
        handle_message(
            &mut app,
            AppMessage::ArchiveListingComplete {
                tab_id: a_id,
                id: a_listing_id,
                archive_path: archive_a.clone(),
                cache_key: None,
                result: Box::new(Ok(listing_a)),
                password: None,
            },
            &tx,
        );

        assert_eq!(app.browse.active_tab_id(), b_id);
        assert_eq!(app.browse.current_dir, b);
        assert!(app.browse.archive.is_none(), "B cannot absorb A's archive");
        assert!(!app.archive_listing_pending_for(a_id, a_listing_id, &archive_a));
        assert!(app.archive_listing_pending_for(b_id, b_listing_id, &archive_b));
        assert!(!b_cancel.is_cancelled(), "A completion cannot cancel B");
        {
            let background = app.browse.tab_mut(a_id).expect("A tab");
            assert!(background
                .archive
                .as_ref()
                .is_some_and(|state| state.listing.archive_path == archive_a));
        }

        let listing_b = crate::tui::archive_listing::ArchiveListing {
            archive_path: archive_b.clone(),
            format: "zip".to_string(),
            physical_size: 0,
            entries: Vec::new(),
        };
        handle_message(
            &mut app,
            AppMessage::ArchiveListingComplete {
                tab_id: b_id,
                id: b_listing_id,
                archive_path: archive_b.clone(),
                cache_key: None,
                result: Box::new(Ok(listing_b)),
                password: None,
            },
            &tx,
        );

        assert_eq!(app.browse.active_tab_id(), b_id);
        assert!(app.browse.archive.as_ref().is_some_and(|state| {
            state.listing.archive_path == archive_b
        }));
        assert!(!app.archive_listing_pending_for(b_id, b_listing_id, &archive_b));
        assert!(app.pending_archive_listings.is_empty());
    }
}
