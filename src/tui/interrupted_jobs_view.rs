// The interrupted-jobs interaction and drawing layer. Included directly in
// recovery_ui so the durable worker and ownership decisions stay in one module.
// User-facing state is derived from current journal and live observations.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowAction {
    Finish, Undo, Older, Progress, Pause, Continue, Stop, ForceStop,
    CancelFinish, Tidy, Dismiss, Release, Error,
}

impl WindowAction {
    fn key(self) -> char {
        match self {
            Self::Finish => 'f', Self::Undo => 'u', Self::Older => 'o',
            Self::Progress => 'w', Self::Pause => 'p', Self::Continue => 'c',
            Self::Stop => 'x', Self::ForceStop => 'k', Self::CancelFinish => 'x',
            Self::Tidy => 't', Self::Dismiss => 'd', Self::Release => 'r',
            Self::Error => 'e',
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Finish => "Finish", Self::Undo => "Undo…",
            Self::Older => "Go to older job", Self::Progress => "Progress…",
            Self::Pause => "Pause", Self::Continue => "Continue", Self::Stop => "Stop…",
            Self::ForceStop => "Force stop…", Self::CancelFinish => "Cancel Finish",
            Self::Tidy => "Tidy up", Self::Dismiss => "Dismiss",
            Self::Release => "Release…", Self::Error => "Error…",
        }
    }
    fn is_affirmative(self) -> bool {
        matches!(self, Self::Finish | Self::Continue | Self::Tidy | Self::Dismiss)
    }
}

fn job_has_files(entry: &RecoveryEntry) -> bool {
    !entry.is_unreadable() && (!entry.blocked_paths().is_empty()
        || !entry.incomplete_paths().is_empty() || !entry.renamed_sources().is_empty()
        || entry.failed_count() > 0 || entry.skipped_count() > 0)
}

fn window_actions(entry: &RecoveryEntry, entries: &[RecoveryEntry]) -> Vec<WindowAction> {
    if entry.is_unreadable() { return vec![WindowAction::Error, WindowAction::Release]; }
    if entry.discard_committed { return Vec::new(); }
    if entry.is_live() {
        if entry.stop_requested_unix_ms.is_some() {
            return if entry.worker_pid().is_some() && entry.stop_requested_unix_ms
                .is_some_and(|ms| unix_ms_now().saturating_sub(ms) >= 10_000) {
                vec![WindowAction::ForceStop]
            } else { Vec::new() };
        }
        // Without verified control metadata, the user may leave the window;
        // never present a Pause/Stop or false progress meter.
        if !entry.has_observed_control() { return Vec::new(); }
        if entry.is_stalled() {
            return if entry.worker_pid().is_some() {
                vec![WindowAction::Progress, WindowAction::ForceStop]
            } else { vec![WindowAction::Progress] };
        }
        if entry.live_snapshot.as_ref().is_some_and(|s|
            matches!(s.phase, tui_file_picker::FileTaskPhase::Paused)) {
            return vec![WindowAction::Continue, WindowAction::Stop];
        }
        return vec![WindowAction::Progress, WindowAction::Pause, WindowAction::Stop];
    }
    if entry.resume_committed {
        return if entry.approved_resume_queue_id.is_some() {
            vec![WindowAction::CancelFinish]
        } else { Vec::new() };
    }
    if entry.can_dismiss() { return vec![WindowAction::Dismiss]; }
    if entry.can_tidy() { return vec![WindowAction::Tidy]; }
    let mut result = Vec::new();
    if entry.can_resume() {
        result.push(if older_interrupted_job(entries, entry).is_some() {
            WindowAction::Older
        } else { WindowAction::Finish });
    }
    if entry.can_discard() { result.push(WindowAction::Undo); }
    result
}

fn begin_single_dismiss(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let Some((path, version)) = app.recovery_ui.selected_entry()
        .filter(|entry| entry.can_dismiss())
        .and_then(|entry| Some((entry.journal_path.clone(), entry.version?))) else {
        app.set_status("Only a finished job with no unfinished paths can be dismissed");
        return;
    };
    if !app.recovery_ui.discard_in_flight.insert(path.clone()) { return; }
    commit_discard_choices_in_session(app, &BTreeSet::from([path.clone()]));
    let tx = tx.clone();
    std::thread::spawn(move || {
        let result = super::file_task_runtime::dismiss_completed_native_recovery(&path, version);
        let _ = tx.blocking_send(AppMessage::RecoveryDismissComplete { journal_path: path, result });
    });
}

pub fn complete_dismiss(app: &mut AppState, path: PathBuf, result: Result<(), String>) {
    app.recovery_ui.discard_in_flight.remove(&path);
    refresh_from_runtime(app);
    match result {
        Ok(()) => app.set_status("Finished extraction dismissed; its locks released without changing your files"),
        Err(error) => app.set_status(format!("Could not dismiss job: {}", user_safe_discard_error(&error))),
    }
    if matches!(app.active_overlay, ActiveOverlay::FileRecovery) {
        app.recovery_ui.open_window();
    }
}

fn open_selected_discard_confirm(app: &mut AppState) {
    if !app.recovery_ui.selected_entry().is_some_and(|e| e.can_discard()) {
        app.set_status("Undo is not available while this worker or its queued Finish owns the job");
        return;
    }
    app.recovery_ui.consume_startup_prompt();
    app.recovery_ui.surface = RecoverySurface::DiscardConfirm;
    app.recovery_ui.focus = 1; // Cancel is always the first focused control.
    app.recovery_ui.details_scroll = 0;
}

fn open_selected_details(app: &mut AppState, tx: &mpsc::Sender<AppMessage>,
    reason: RecoveryDetailsReason,
) {
    let Some(entry) = app.recovery_ui.selected_entry() else { return; };
    if entry.is_unreadable() { return; }
    let surface = if matches!(reason, RecoveryDetailsReason::Refusal(_)) {
        RecoverySurface::Refusal
    } else { RecoverySurface::Files };
    app.recovery_ui.open_details(reason);
    app.recovery_ui.surface = surface;
    request_incomplete_sizes_if_needed(app, tx);
}

fn action_for_key(key: KeyCode, actions: &[WindowAction]) -> Option<WindowAction> {
    match key {
        KeyCode::Char(letter) => actions.iter().copied().find(|action| action.key() == letter),
        _ => None,
    }
}

fn activate_action(app: &mut AppState, action: WindowAction, tx: &mpsc::Sender<AppMessage>) {
    // Recheck the action at dispatch, as a worker may have moved on since
    // the screen last rendered. All disk-changing paths also take a versioned
    // recovery lease before touching filesystem state.
    let still_offered = app.recovery_ui.selected_entry().is_some_and(|entry|
        window_actions(entry, &app.recovery_ui.entries).contains(&action));
    if !still_offered { app.set_status("This job has changed. Review its current actions."); return; }
    match action {
        WindowAction::Finish => resume_selected(app, tx),
        WindowAction::Undo => open_selected_discard_confirm(app),
        WindowAction::Older => {
            let older = app.recovery_ui.selected_entry()
                .and_then(|entry| older_interrupted_job(&app.recovery_ui.entries, entry));
            if let Some(index) = older { app.recovery_ui.selected = index;
                app.recovery_ui.small_job_tab = true; app.recovery_ui.focus = 0; }
        }
        WindowAction::Progress => watch_selected_progress(app),
        WindowAction::Pause => request_running_control(app, super::app::FileTaskControl::Pause),
        WindowAction::Continue => {
            request_running_control(app, super::app::FileTaskControl::Resume);
            watch_selected_progress(app);
        }
        WindowAction::Stop => open_safety_confirm(app, RecoverySurface::StopConfirm),
        WindowAction::ForceStop => open_safety_confirm(app, RecoverySurface::ForceStopConfirm),
        WindowAction::CancelFinish => {
            if let Some(queue_id) = app.recovery_ui.selected_entry()
                .and_then(|entry| entry.approved_resume_queue_id) {
                super::keybindings::cancel_queued_file_transfer(app, queue_id, tx);
                refresh_from_runtime(app);
                app.recovery_ui.open_window();
            }
        }
        WindowAction::Tidy => {
            // Finished copies/moves already had an executable cleanup-only
            // Finish path: keep that engine behavior and relabel its action.
            // Interrupted native extraction cleanup and unfinished Undo keep
            // using the existing identity-checked discard machinery.
            let resume_cleanup = app.recovery_ui.selected_entry().is_some_and(|entry|
                entry.can_resume() && entry.record.as_ref().is_some_and(|r|
                    !r.is_native_extraction() && r.lifecycle == DurableFileTaskLifecycle::Completed));
            if resume_cleanup { resume_selected(app, tx); }
            else { begin_single_discard(app, tx); }
        },
        WindowAction::Dismiss => begin_single_dismiss(app, tx),
        WindowAction::Release => open_safety_confirm(app, RecoverySurface::SetAsideConfirm),
        WindowAction::Error => {
            app.recovery_ui.surface = RecoverySurface::Error;
            app.recovery_ui.details_scroll = 0;
        }
    }
}

fn activate_window_focus(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let actions = app.recovery_ui.selected_entry()
        .map(|entry| window_actions(entry, &app.recovery_ui.entries))
        .unwrap_or_default();
    if let Some(action) = actions.get(app.recovery_ui.focus).copied() {
        activate_action(app, action, tx);
    }
}

fn move_selection(state: &mut RecoveryUiState, delta: isize) {
    if state.entries.is_empty() { return; }
    let new_index = if delta < 0 {
        state.selected.saturating_sub(delta.unsigned_abs())
    } else { state.selected.saturating_add(delta as usize)
        .min(state.entries.len() - 1) };
    state.selected = new_index;
    state.focus = 0;
    state.details_scroll = 0;
}

fn confirm_current(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    if app.recovery_ui.focus != 0 { app.recovery_ui.open_window(); return; }
    match app.recovery_ui.surface {
        RecoverySurface::DiscardConfirm => begin_single_discard(app, tx),
        RecoverySurface::BulkDiscardConfirm => begin_bulk_discard(app, tx),
        RecoverySurface::StopConfirm | RecoverySurface::ForceStopConfirm
        | RecoverySurface::SetAsideConfirm => activate_safety_confirmation(app, tx),
        _ => app.recovery_ui.open_window(),
    }
    if app.recovery_ui.discard_in_flight.len() > 0 {
        app.recovery_ui.open_window();
    }
}

fn close_recovery_window(app: &mut AppState) {
    app.recovery_ui.minimize();
    app.active_overlay = ActiveOverlay::None;
}

pub fn handle_key(app: &mut AppState, key: KeyEvent, tx: &mpsc::Sender<AppMessage>) {
    if matches!((key.code, key.modifiers),
        (KeyCode::Char('m'), KeyModifiers::ALT)) {
        app.recovery_ui.maximized = !app.recovery_ui.maximized;
        return;
    }
    match app.recovery_ui.surface {
        RecoverySurface::Window => match key.code {
            KeyCode::Esc => close_recovery_window(app),
            KeyCode::Char('?') => { app.recovery_ui.surface = RecoverySurface::Help; app.recovery_ui.details_scroll=0; }
            KeyCode::Up => move_selection(&mut app.recovery_ui, -1),
            KeyCode::Down => move_selection(&mut app.recovery_ui, 1),
            KeyCode::PageUp => move_selection(&mut app.recovery_ui, -6),
            KeyCode::PageDown => move_selection(&mut app.recovery_ui, 6),
            KeyCode::Tab | KeyCode::BackTab => {
                // At 80x24 Tab changes the visible region. On wide windows
                // it advances through the enabled footer actions.
                if !app.recovery_ui.compact {
                    let count = app.recovery_ui.selected_entry()
                        .map(|entry| window_actions(entry, &app.recovery_ui.entries).len()).unwrap_or(0);
                    if count > 0 {
                        app.recovery_ui.focus = if key.code == KeyCode::BackTab
                            || key.modifiers.contains(KeyModifiers::SHIFT) {
                            (app.recovery_ui.focus + count - 1) % count
                        } else { (app.recovery_ui.focus + 1) % count };
                    }
                } else { app.recovery_ui.small_job_tab = !app.recovery_ui.small_job_tab; }
            }
            KeyCode::Enter => activate_window_focus(app, tx),
            KeyCode::Char('v') if app.recovery_ui.selected_entry().is_some_and(job_has_files) =>
                open_selected_details(app, tx, RecoveryDetailsReason::Ordinary),
            KeyCode::Char('a') if app.recovery_ui.bulk_discard_count() > 0 => {
                open_safety_confirm(app, RecoverySurface::BulkDiscardConfirm);
            }
            KeyCode::Char('A') if app.recovery_ui.resume_all_available() => resume_all(app, tx),
            key => {
                let actions = app.recovery_ui.selected_entry()
                    .map(|entry| window_actions(entry, &app.recovery_ui.entries)).unwrap_or_default();
                if let Some(action) = action_for_key(key, &actions) { activate_action(app, action, tx); }
            }
        },
        RecoverySurface::Files | RecoverySurface::Help | RecoverySurface::Error => match key.code {
            KeyCode::Esc | KeyCode::Enter => app.recovery_ui.open_window(),
            KeyCode::Up => app.recovery_ui.details_scroll = app.recovery_ui.details_scroll.saturating_sub(1),
            KeyCode::Down => app.recovery_ui.details_scroll = app.recovery_ui.details_scroll.saturating_add(1),
            KeyCode::PageUp => app.recovery_ui.details_scroll = app.recovery_ui.details_scroll.saturating_sub(8),
            KeyCode::PageDown => app.recovery_ui.details_scroll = app.recovery_ui.details_scroll.saturating_add(8),
            _ => {}
        },
        RecoverySurface::Refusal => match key.code {
            KeyCode::Esc | KeyCode::Char('c') => app.recovery_ui.open_window(),
            KeyCode::Char('s') | KeyCode::Enter => app.recovery_ui.open_window(),
            KeyCode::Char('u') => {
                app.recovery_ui.open_window(); open_selected_discard_confirm(app);
            }
            KeyCode::Char('f') => {
                app.recovery_ui.open_window();
                activate_action(app, WindowAction::Finish, tx);
            }
            _ => {}
        },
        RecoverySurface::DiscardConfirm | RecoverySurface::BulkDiscardConfirm
        | RecoverySurface::StopConfirm | RecoverySurface::ForceStopConfirm
        | RecoverySurface::SetAsideConfirm => match key.code {
            KeyCode::Esc => app.recovery_ui.open_window(),
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                app.recovery_ui.focus = 1 - app.recovery_ui.focus.min(1);
            }
            KeyCode::Enter => confirm_current(app, tx),
            KeyCode::Up => app.recovery_ui.details_scroll = app.recovery_ui.details_scroll.saturating_sub(1),
            KeyCode::Down => app.recovery_ui.details_scroll = app.recovery_ui.details_scroll.saturating_add(1),
            KeyCode::PageUp => app.recovery_ui.details_scroll = app.recovery_ui.details_scroll.saturating_sub(8),
            KeyCode::PageDown => app.recovery_ui.details_scroll = app.recovery_ui.details_scroll.saturating_add(8),
            _ => {}
        },
    }
}

pub fn handle_mouse(app: &mut AppState, mouse: MouseEvent, tx: &mpsc::Sender<AppMessage>) {
    match mouse.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let delta = if matches!(mouse.kind, MouseEventKind::ScrollUp) { -3 } else { 3 };
            if app.recovery_ui.surface == RecoverySurface::Window && !app.recovery_ui.small_job_tab {
                move_selection(&mut app.recovery_ui, delta);
            } else if app.recovery_ui.surface != RecoverySurface::Window {
                if delta < 0 { app.recovery_ui.details_scroll = app.recovery_ui.details_scroll.saturating_sub(3); }
                else { app.recovery_ui.details_scroll = app.recovery_ui.details_scroll.saturating_add(3); }
            }
            return;
        }
        MouseEventKind::Down(MouseButton::Left) => {}
        _ => return,
    }
    let clicked = app.button_map.find_button_at(mouse.column, mouse.row);
    if app.recovery_ui.surface != RecoverySurface::Window {
        match clicked {
            Some(TuiButton::RecoveryConfirm(index)) => {
                app.recovery_ui.focus = index as usize;
                confirm_current(app, tx);
            }
            Some(TuiButton::RecoveryDetailsClose) => app.recovery_ui.open_window(),
            Some(TuiButton::InterruptedRefusalAction(index)) if app.recovery_ui.surface == RecoverySurface::Refusal => {
                match index {
                    0 => app.recovery_ui.open_window(),
                    1 => { app.recovery_ui.open_window(); open_selected_discard_confirm(app); }
                    3 => { app.recovery_ui.open_window(); activate_action(app, WindowAction::Finish, tx); }
                    _ => app.recovery_ui.open_window(),
                }
            }
            _ => {
                // Clicking outside a popup has the same effect as Cancel.
                let popup = popup_rect(app.recovery_ui.last_screen, app.recovery_ui.maximized);
                if mouse.column < popup.x || mouse.column >= popup.right()
                    || mouse.row < popup.y || mouse.row >= popup.bottom() {
                    app.recovery_ui.open_window();
                }
            }
        }
        return;
    }
    match clicked {
        Some(TuiButton::RecoveryTitle) => {
            if app.double_click.register_click(TuiButton::RecoveryTitle,
                mouse.column, mouse.row, tui_file_picker::DOUBLE_CLICK_WINDOW) {
                app.recovery_ui.maximized = !app.recovery_ui.maximized;
            }
        }
        Some(TuiButton::RecoveryListRow(index)) if index < app.recovery_ui.entries.len() => {
            app.recovery_ui.selected = index;
            app.recovery_ui.focus = 0;
        }
        Some(TuiButton::RecoveryAction(index)) => {
            app.recovery_ui.focus = index as usize;
            activate_window_focus(app, tx);
        }
        Some(TuiButton::RecoveryBulkResume) => resume_all(app, tx),
        Some(TuiButton::RecoveryBulkDiscard) => {
            if app.recovery_ui.bulk_discard_count() > 0 {
                open_safety_confirm(app, RecoverySurface::BulkDiscardConfirm);
            }
        }
        Some(TuiButton::InterruptedTab(index)) => {
            app.recovery_ui.small_job_tab = index == 1;
        }
        Some(TuiButton::InterruptedShowFiles) => {
            open_selected_details(app, tx, RecoveryDetailsReason::Ordinary);
        }
        Some(TuiButton::InterruptedHelp) => {
            app.recovery_ui.surface = RecoverySurface::Help;
            app.recovery_ui.details_scroll = 0;
        }
        Some(TuiButton::InterruptedClose) => close_recovery_window(app),
        _ => {}
    }
}

fn window_rect(area: Rect, maximized: bool) -> Rect {
    if maximized && area.width > 2 && area.height > 2 {
        return Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);
    }
    let width = if area.width < 124 { area.width }
        else { ((u32::from(area.width) * 85 / 100) as u16).max(120).min(area.width - 2) };
    let height = if area.height < 29 { area.height }
        else { ((u32::from(area.height) * 85 / 100) as u16).max(25).min(area.height - 2) };
    centered_rect(width, height, area)
}

fn centered_rect(width: u16, height: u16, outer: Rect) -> Rect {
    let w = width.min(outer.width);
    let h = height.min(outer.height);
    Rect::new(outer.x + (outer.width - w) / 2, outer.y + (outer.height - h) / 2, w, h)
}

fn popup_rect(area: Rect, maximized: bool) -> Rect {
    let base = window_rect(area, maximized);
    if area.width < 90 || area.height < 20 { return base; }
    let width = (base.width.saturating_sub(4)).min(90).max(60).min(area.width);
    let height = (base.height.saturating_sub(2)).min(22).max(12).min(area.height);
    centered_rect(width, height, area)
}

fn draw_text(f: &mut Frame, rect: Rect, label: &str, fg: Color, bg: Color) {
    if rect.width == 0 || rect.height == 0 { return; }
    let fitted = super::display_width::truncate_right(label, rect.width as usize);
    f.render_widget(Paragraph::new(fitted).style(Style::default().fg(fg).bg(bg)), rect);
}

fn draw_path(f: &mut Frame, rect: Rect, label: &str, fg: Color, bg: Color) {
    if rect.width == 0 || rect.height == 0 { return; }
    // Retain the album/folder name at the end when paths exceed a pane.
    let fitted = super::display_width::truncate_middle(label, rect.width as usize);
    f.render_widget(Paragraph::new(fitted).style(Style::default().fg(fg).bg(bg)), rect);
}

fn draw_rule(f: &mut Frame, area: Rect, y: u16, theme: super::theme::Theme,
    divider_at: Option<u16>,
) {
    if y >= area.bottom() || area.width < 2 { return; }
    let left = area.x;
    let right = area.right() - 1;
    let b = f.buffer_mut();
    b.get_mut(left, y).set_symbol("├").set_fg(theme.blue);
    b.get_mut(right, y).set_symbol("┤").set_fg(theme.blue);
    for x in left + 1..right {
        b.get_mut(x, y).set_symbol("─").set_fg(theme.border_dim);
    }
    if let Some(x) = divider_at.filter(|x| *x > left && *x < right) {
        b.get_mut(x, y).set_symbol("┬").set_fg(theme.blue);
    }
}

fn draw_neutral_pill(f: &mut Frame, area: Rect, text: &str,
    focused: bool, theme: super::theme::Theme,
) -> Rect {
    if area.width == 0 { return area; }
    let label = format!(" {text} ");
    let width = (super::display_width::width(&label) as u16).min(area.width);
    let hit = Rect::new(area.x, area.y, width, 1);
    let (bg, fg) = if focused { (theme.pill_active_bg, theme.pill_active_fg) }
        else { (theme.pill_neutral_bg, theme.value) };
    draw_text(f, hit, &label, fg, bg);
    hit
}

fn job_display_name(entry: &RecoveryEntry) -> String {
    let Some(record) = entry.record.as_ref() else {
        let id = entry.journal_path.file_stem()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".into());
        return format!("record {}", super::display_width::truncate_middle(&id, 18));
    };
    if record.is_native_extraction() {
        if let Some(source) = record.job.get("native_extraction")
            .and_then(|job| job.get("source")).and_then(|v| v.as_str()) {
            return Path::new(source).file_stem().map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| source.to_string());
        }
    }
    let Some(first) = record.mappings.first() else { return entry.kind_label().into(); };
    let name = first.source.file_name().unwrap_or(first.source.as_os_str()).to_string_lossy();
    if record.mappings.len() > 1 { format!("{} +{}", name, record.mappings.len() - 1) }
    else { name.into_owned() }
}

fn displayed_job_name(entry: &RecoveryEntry, width: usize) -> String {
    let name = job_display_name(entry);
    if super::display_width::width(&name) <= width { return name; }
    // A selected multi-item suffix describes selected items, not files within
    // a folder; retain it even when the album name is shortened.
    if let Some(record) = entry.record.as_ref().filter(|r| r.mappings.len() > 1 && !r.is_native_extraction()) {
        let suffix = format!(" +{}", record.mappings.len() - 1);
        let room = width.saturating_sub(super::display_width::width(&suffix));
        return format!("{}{}", super::display_width::truncate_right(
            &name[..name.len().saturating_sub(suffix.len())], room), suffix);
    }
    super::display_width::truncate_right(&name, width)
}

fn ui_state(entry: &RecoveryEntry, entries: &[RecoveryEntry]) -> &'static str {
    if entry.is_unreadable() { return "unreadable"; }
    if entry.discard_committed { return "undoing"; }
    if entry.is_live() {
        if entry.stop_requested_unix_ms.is_some() { return "stopping"; }
        if entry.has_observed_control() && entry.live_snapshot.as_ref()
            .is_some_and(|s| matches!(s.phase, tui_file_picker::FileTaskPhase::Paused)) { return "paused"; }
        if entry.is_stalled() { return "no progress"; }
        return "running";
    }
    if entry.resume_committed { return "queued"; }
    if entry.can_dismiss() { return "complete"; }
    if entry.can_tidy() { return "tidy up"; }
    if older_interrupted_job(entries, entry).is_some() && entry.can_resume() { return "waits"; }
    "interrupted"
}

fn state_fg(state: &str, theme: super::theme::Theme) -> Color {
    match state {
        "unreadable" | "no progress" | "waits" | "interrupted" | "tidy up" | "stopping" => theme.warning,
        "complete" => theme.success,
        "running" | "queued" | "undoing" => theme.cyan,
        _ => theme.value,
    }
}

fn entry_progress(entry: &RecoveryEntry) -> String {
    if entry.is_unreadable() { return "locked unknown".into(); }
    if entry.is_live() {
        return entry.live_snapshot.as_ref()
            .and_then(|snapshot| snapshot.totals.byte_ratio())
            .map(|ratio| format!("{}%", (ratio * 100.0).round() as u64))
            .unwrap_or_else(|| "still running".into());
    }
    let total = entry.item_count();
    if total == 0 { return "items unknown".into(); }
    let unit = if entry.kind_label() == "extract" { "tracks" } else { "items" };
    format!("{} of {total} {unit}", entry.completed_count())
}

fn locked_total(entries: &[RecoveryEntry]) -> (usize, bool) {
    (entries.iter().map(|e| e.blocked_paths().len()).sum(),
        entries.iter().any(RecoveryEntry::is_unreadable))
}

fn live_count(entries: &[RecoveryEntry]) -> usize {
    entries.iter().filter(|e| e.is_live() || e.resume_committed || e.discard_committed).count()
}

fn job_status(entry: &RecoveryEntry, entries: &[RecoveryEntry]) -> (String, String) {
    let Some(record) = entry.record.as_ref() else {
        return ("▲ tonepoet can't read this job's saved record.".into(),
            "It can't tell which files were touched. Some may be partial.".into());
    };
    let state = ui_state(entry, entries);
    let ago = age_label(record.updated_unix_ms);
    let unit = if record.is_native_extraction() { "tracks" } else { "items" };
    let completed = entry.completed_count();
    let remaining = entry.remaining_count();
    match state {
        "complete" => (if entry.item_count() > 0 {
            format!("✓ Finished {}. All {} {unit} were completed.", ago, entry.item_count())
        } else { format!("✓ Finished {ago}. All selected {unit} were completed.") },
            "Nothing is left to do. Dismiss releases this job's locks.".into()),
        "tidy up" => (if entry.is_discard_cleanup() {
            "Undo did not finish tidying up.".into()
        } else { format!("✓ All {} {unit} were completed.", completed) },
            "Tidy up removes unfinished private paths and releases the locks.".into()),
        "waits" => (format!("▲ Interrupted {ago}. An older job uses the same folder."),
            "Finish appears when the older job is finished or undone.".into()),
        "interrupted" if record.native_rename_intents.len() > 0 =>
            (format!("▲ Interrupted {ago}, while renaming an original."),
                "Undo restores the name only after verifying the file identity.".into()),
        "interrupted" if entry.irreversible() =>
            (format!("▲ Interrupted {ago}, while deleting originals."),
                "Undo cannot bring back originals already deleted.".into()),
        "interrupted" if entry.failed_count() > 0 || entry.skipped_count() > 0 =>
            (format!("▲ Interrupted {ago}. {} failed · {} skipped.",
                entry.failed_count(), entry.skipped_count()),
                "Show files… lists those selections.".into()),
        "interrupted" if completed == 0 && entry.incomplete_paths().is_empty() =>
            (format!("▲ Interrupted {ago}, before any {unit} finished."),
                "No unfinished paths remain on disk; Undo releases the locks.".into()),
        "interrupted" => (format!("▲ Interrupted {ago}, part way through."),
            format!("{completed} of {} selected {unit} finished. {remaining} left.", entry.item_count())),
        "stopping" => ("● Stopping. A cancellation request was sent to the worker.".into(),
            "When the worker exits, unfinished output is cleaned. Finished files stay.".into()),
        "undoing" => ("● Undo is in progress.".into(),
            "When safe cleanup and release finish, the job leaves this list.".into()),
        "queued" => ("Finish is queued behind another file task.".into(),
            "It starts when the current file task completes.".into()),
        "paused" => (format!("Paused {ago}. Nothing is being written."),
            "Continue restarts the paused worker.".into()),
        "no progress" => {
            let elapsed = entry.live_snapshot.as_ref()
                .map(|snapshot| snapshot.age_since_forward_ms() / 60_000).unwrap_or(0);
            let message = if elapsed > 0 {
                format!("▲ Still running, but no new progress for {elapsed} minute{}.",
                    plural(elapsed as usize))
            } else { "▲ Still running, but no new progress has been recorded recently.".into() };
            (message, "Force stop verifies the exact worker before signaling it.".into())
        },
        "running" if !entry.has_observed_control() =>
            ("● Still running, but tonepoet can't read its progress.".into(),
                "Pause and Stop are unavailable until the worker feed returns.".into()),
        "running" => (format!("● Still {}. Started {}.",
            if record.is_native_extraction() { "extracting" } else if record.is_move { "moving" }
                else { "copying" }, age_label(record.created_unix_ms)),
            "Progress… opens the worker monitor; Minimize keeps it running.".into()),
        _ => ("▲ Interrupted.".into(), String::new()),
    }
}

fn unfinished_description(entry: &RecoveryEntry, sizes: &BTreeMap<PathBuf, Option<u64>>) -> Option<String> {
    let paths = entry.incomplete_paths();
    if paths.is_empty() { return None; }
    let folders = entry.record.as_ref().map_or(0, |r| r.temp_artifacts.iter()
        .filter(|art| matches!(art.kind, super::file_task_runtime::DurableTempArtifactKind::Directory)).count());
    let count = paths.len();
    let size = paths.iter().try_fold(0u64, |sum, path| {
        sizes.get(path).copied().flatten().and_then(|n| sum.checked_add(n))
    });
    let noun = if count == 1 && folders == 1 { "folder" } else if count == 1 { "file" }
        else { "paths" };
    Some(format!("{count} {noun} in the destination · {}",
        size.map(human_bytes).map(|value| format!("{value} so far"))
            .unwrap_or_else(|| if paths.iter().any(|path| !sizes.contains_key(path)) {
                "size being measured".into()
            } else { "size unknown".into() })))
}

fn draw_main(f: &mut Frame, app: &mut AppState, theme: super::theme::Theme) {
    let outer = window_rect(f.size(), app.recovery_ui.maximized);
    if outer.width < 4 || outer.height < 6 { return; }
    f.render_widget(Clear, outer);
    let block = super::draw::solid_title_block(outer, "▸ interrupted jobs", theme.blue, theme);
    let inner = block.inner(outer);
    f.render_widget(block, outer);
    app.button_map.record_button(TuiButton::RecoveryTitle,
        Rect::new(outer.x + 1, outer.y, outer.width.saturating_sub(2), 1));
    let count = app.recovery_ui.entries.len();
    let decisions = app.recovery_ui.needs_attention_count();
    let running = live_count(&app.recovery_ui.entries);
    let (locked, unknown_locked) = locked_total(&app.recovery_ui.entries);
    let wide = inner.width >= 118;
    let footer_y = inner.bottom().saturating_sub(1);
    let bottom_rule_y = footer_y.saturating_sub(1);
    let summary_y = bottom_rule_y.saturating_sub(1);
    let pane_bottom_rule = summary_y.saturating_sub(1);
    let pane_top_rule = inner.y.saturating_add(if wide { 2 } else { 2 });
    let pane_start = pane_top_rule.saturating_add(1);
    let panes = Rect::new(inner.x, pane_start, inner.width,
        pane_bottom_rule.saturating_sub(pane_start));
    let band = theme.surface;
    let summary = if decisions > 0 && running > 0 {
        format!("{decisions} need a decision · {running} running")
    } else if decisions > 0 { format!("{decisions} need a decision") }
    else if running > 0 { format!("{running} running") }
    else { String::new() };
    if !summary.is_empty() {
        let width = (super::display_width::width(&summary) as u16)
            .min(outer.width.saturating_sub(5));
        draw_text(f, Rect::new(outer.right().saturating_sub(width + 2), outer.y, width, 1),
            &summary, theme.panel_bg, theme.blue);
    }
    let band_row = Rect::new(inner.x, inner.y, inner.width, 1);
    f.render_widget(Block::default().style(Style::default().bg(band)), band_row);
    if wide {
        draw_text(f, Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), 1),
            if count == 0 { "No copy, move or extraction was left unfinished." }
            else { "These jobs stopped before they finished. Files they touched stay locked until you finish or undo each job." },
            theme.text, band);
        let second = Rect::new(inner.x, inner.y + 1, inner.width, 1);
        f.render_widget(Block::default().style(Style::default().bg(band)), second);
        let band_details = if count == 0 { "Nothing is locked.".to_owned() }
            else { format!("{count} jobs · {decisions} need a decision · {running} still running · {}{locked} files and folders locked",
                if unknown_locked { "at least " } else { "" }) };
        draw_text(f, Rect::new(inner.x + 1, inner.y + 1, inner.width.saturating_sub(2), 1),
            &band_details, theme.text_muted, band);
    } else {
        let text = format!("{count} jobs · {decisions} need a decision · {running} running");
        draw_text(f, Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), 1),
            &text, theme.text_muted, band);
        let tabs = Rect::new(inner.x + 1, inner.y + 1, inner.width.saturating_sub(2), 1);
        let left_label = format!(" Jobs {count} ");
        let left_width = (super::display_width::width(&left_label) as u16).min(tabs.width);
        let left = Rect::new(tabs.x, tabs.y, left_width, 1);
        draw_text(f, left, &left_label,
            if app.recovery_ui.small_job_tab { theme.value } else { theme.pill_active_fg },
            if app.recovery_ui.small_job_tab { theme.pill_neutral_bg } else { theme.pill_active_bg });
        app.button_map.record_button(TuiButton::InterruptedTab(0), left);
        let label = app.recovery_ui.selected_entry().map(job_display_name)
            .unwrap_or_else(|| "Job".to_string());
        let right_width = tabs.width.saturating_sub(left_width + 1);
        if right_width > 0 {
            let right = Rect::new(tabs.x + left_width + 1, tabs.y, right_width, 1);
            let text = format!(" {} ", super::display_width::truncate_right(&label, right_width.saturating_sub(2) as usize));
            let active = app.recovery_ui.small_job_tab;
            draw_text(f, right, &text, if active { theme.pill_active_fg } else { theme.value },
                if active { theme.pill_active_bg } else { theme.pill_neutral_bg });
            app.button_map.record_button(TuiButton::InterruptedTab(1), right);
        }
    }
    let divider_at = if wide { Some(inner.x + 43) } else { None };
    draw_rule(f, outer, pane_top_rule, theme, divider_at);
    draw_rule(f, outer, pane_bottom_rule, theme, None);
    draw_rule(f, outer, bottom_rule_y, theme, None);
    if wide && panes.width >= 46 {
        let divider_x = inner.x + 43;
        for y in pane_start..pane_bottom_rule {
            f.buffer_mut().get_mut(divider_x, y).set_symbol("│").set_fg(theme.blue);
        }
        f.buffer_mut().get_mut(divider_x, pane_bottom_rule).set_symbol("┴").set_fg(theme.blue);
        let left = Rect::new(inner.x, pane_start, 43, panes.height);
        let right = Rect::new(divider_x + 1, pane_start,
            inner.width.saturating_sub(44), panes.height);
        draw_job_list(f, &mut app.recovery_ui, &mut app.button_map, left, theme);
        draw_job_pane(f, &app.recovery_ui, &mut app.button_map, right, theme);
    } else if app.recovery_ui.small_job_tab {
        draw_job_pane(f, &app.recovery_ui, &mut app.button_map, panes, theme);
    } else {
        draw_job_list(f, &mut app.recovery_ui, &mut app.button_map, panes, theme);
    }
    let summary = app.recovery_ui.selected_entry()
        .map(|entry| job_summary(entry, &app.recovery_ui.entries))
        .unwrap_or_else(|| "Close returns to Browse.".into());
    draw_text(f, Rect::new(inner.x + 1, summary_y, inner.width.saturating_sub(2), 1),
        &summary, theme.text_muted, theme.panel_bg);
    draw_action_footer(f, &app.recovery_ui, &mut app.button_map,
        Rect::new(inner.x, footer_y, inner.width, 1), theme);
}

fn draw_job_list(f: &mut Frame, state: &mut RecoveryUiState,
    buttons: &mut ButtonRenderMap, area: Rect, theme: super::theme::Theme,
) {
    if area.width < 5 || area.height < 3 { return; }
    draw_text(f, Rect::new(area.x + 2, area.y + 1, area.width.saturating_sub(3), 1),
        &format!("JOBS  {}", state.entries.len()), theme.header, theme.panel_bg);
    if state.entries.is_empty() {
        draw_text(f, Rect::new(area.x + 2, area.y + 4.min(area.height.saturating_sub(1)),
            area.width.saturating_sub(4), 1), "No interrupted jobs.", theme.text_muted, theme.panel_bg);
        return;
    }
    let start_y = area.y + 3;
    let visible = area.bottom().saturating_sub(start_y) as usize / 3;
    if visible == 0 { return; }
    if state.selected < state.list_scroll { state.list_scroll = state.selected; }
    if state.selected >= state.list_scroll.saturating_add(visible) {
        state.list_scroll = state.selected + 1 - visible;
    }
    for slot in 0..visible {
        let index = state.list_scroll + slot;
        let Some(entry) = state.entries.get(index) else { break; };
        let y = start_y + (slot * 3) as u16;
        let selected = index == state.selected;
        let bg = if selected { theme.selection_bg } else if slot % 2 == 0 {
            theme.panel_bg
        } else { theme.surface };
        let title_row = Rect::new(area.x, y, area.width, 1);
        let sub_row = Rect::new(area.x, y + 1, area.width, 1);
        f.render_widget(Block::default().style(Style::default().bg(bg)), title_row);
        f.render_widget(Block::default().style(Style::default().bg(bg)), sub_row);
        if selected {
            draw_text(f, Rect::new(area.x, y, 1, 1), "▌", theme.cyan, bg);
            draw_text(f, Rect::new(area.x, y + 1, 1, 1), "▌", theme.cyan, bg);
        }
        let state_word = ui_state(entry, &state.entries);
        let state_width = super::display_width::width(state_word).min(area.width as usize / 2 + 1);
        let name_width = (area.width as usize).saturating_sub(state_width + 5);
        draw_text(f, Rect::new(area.x + 2, y, name_width as u16, 1),
            &displayed_job_name(entry, name_width), theme.value, bg);
        draw_text(f, Rect::new(area.right().saturating_sub(state_width as u16 + 1), y,
            state_width as u16, 1), state_word, state_fg(state_word, theme), bg);
        let progress = entry_progress(entry);
        let age = entry.record.as_ref().map(|record| age_label(record.created_unix_ms))
            .unwrap_or_default();
        draw_text(f, Rect::new(area.x + 2, y + 1, area.width.saturating_sub(3), 1),
            &format!("{} · {} · {}", entry.kind_label(), progress, age), theme.text_muted, bg);
        buttons.record_button(TuiButton::RecoveryListRow(index),
            Rect::new(area.x, y, area.width, 2));
    }
    let bulk_y = area.bottom().saturating_sub(1);
    let mut x = area.right().saturating_sub(2);
    if state.bulk_discard_count() > 0 {
        let text = "Undo all…";
        let width = (super::display_width::width(text) + 2) as u16;
        if x >= area.x + width {
            x -= width;
            let rect = draw_neutral_pill(f, Rect::new(x, bulk_y, width, 1), text, false, theme);
            buttons.record_button(TuiButton::RecoveryBulkDiscard, rect);
            x = x.saturating_sub(1);
        }
    }
    if state.resume_all_available() {
        let text = "Finish all";
        let width = (super::display_width::width(text) + 2) as u16;
        if x >= area.x + width {
            x -= width;
            let rect = draw_neutral_pill(f, Rect::new(x, bulk_y, width, 1), text, false, theme);
            buttons.record_button(TuiButton::RecoveryBulkResume, rect);
        }
    }
}

fn draw_fact(f: &mut Frame, area: Rect, row: &mut u16,
    label: &str, value: &str, theme: super::theme::Theme,
) {
    if *row >= area.bottom().saturating_sub(1) { return; }
    let left_width = 14u16.min(area.width / 3).max(8).min(area.width);
    draw_text(f, Rect::new(area.x, *row, left_width, 1), label, theme.label, theme.surface);
    draw_path(f, Rect::new(area.x + left_width, *row, area.width.saturating_sub(left_width), 1),
        value, theme.value, theme.surface);
    *row += 1;
}

fn native_source_destination(record: &DurableFileTaskRecord) -> Option<(String, String)> {
    let native = record.job.get("native_extraction")?;
    Some((native.get("source")?.as_str()?.to_string(),
        native.get("destination")?.as_str()?.to_string()))
}

fn draw_job_pane(f: &mut Frame, state: &RecoveryUiState,
    buttons: &mut ButtonRenderMap, pane: Rect, theme: super::theme::Theme,
) {
    if pane.width < 6 || pane.height < 3 { return; }
    f.render_widget(Block::default().style(Style::default().bg(theme.surface)), pane);
    let x = pane.x + 2;
    let width = pane.width.saturating_sub(4);
    let Some(entry) = state.selected_entry() else {
        draw_text(f, Rect::new(x, pane.y + 2, width, 1),
            "Interrupted copies, moves and extractions appear here.", theme.text_muted, theme.surface);
        return;
    };
    let mut y = pane.y + 1;
    draw_text(f, Rect::new(x, y, width, 1),
        &format!("{}  {}", entry.kind_label().to_ascii_uppercase(), job_display_name(entry)),
        theme.value, theme.surface);
    y += 2;
    let (status, detail) = job_status(entry, &state.entries);
    let state_word = ui_state(entry, &state.entries);
    draw_text(f, Rect::new(x, y, width, 1), &status, state_fg(state_word, theme), theme.surface);
    y += 1;
    if !detail.is_empty() {
        draw_text(f, Rect::new(x, y, width, 1), &detail, theme.text_muted, theme.surface);
        y += 2;
    } else { y += 1; }
    if entry.is_unreadable() {
        draw_fact(f, Rect::new(x, y, width, pane.bottom().saturating_sub(y)), &mut y,
            "Record", &entry.journal_path.display().to_string(), theme);
        draw_fact(f, Rect::new(x, y, width, pane.bottom().saturating_sub(y)), &mut y,
            "Locked", "unknown", theme);
        if let Some(diagnostic) = entry.diagnostic.as_deref() {
            draw_path(f, Rect::new(x, y.saturating_add(1), width, 1),
                &user_safe_diagnostic(diagnostic), theme.error, theme.surface);
        }
        return;
    }
    let Some(record) = entry.record.as_ref() else { return; };
    if entry.is_live() && entry.has_observed_control() {
        if let Some(snapshot) = entry.live_snapshot.as_ref() {
            if let Some(ratio) = snapshot.totals.byte_ratio() {
                let cells = 24usize.min(width as usize).max(1);
                let filled = ((ratio.clamp(0.0, 1.0) * cells as f64).floor() as usize).min(cells);
                let bar = format!("{}{}", "█".repeat(filled), "░".repeat(cells - filled));
                draw_text(f, Rect::new(x, y, width, 1),
                    &format!("{bar} {:>3}%", (ratio * 100.0).round() as usize), theme.cyan, theme.surface);
                y += 2;
            }
            if snapshot.totals.items_done > 0 || snapshot.totals.items_total.is_some() {
                draw_fact(f, Rect::new(x, y, width, pane.bottom().saturating_sub(y)), &mut y,
                    "Files", &format!("{} of {}", snapshot.totals.items_done,
                        snapshot.totals.items_total.map(|n| n.to_string()).unwrap_or_else(|| "unknown".into())), theme);
            }
            if snapshot.totals.bytes_done > 0 {
                draw_fact(f, Rect::new(x, y, width, pane.bottom().saturating_sub(y)), &mut y,
                    "Bytes", &format!("{} of {}", human_bytes(snapshot.totals.bytes_done),
                        snapshot.totals.bytes_total.map(human_bytes).unwrap_or_else(|| "unknown".into())), theme);
            }
            if let Some(item) = snapshot.current_item.as_ref() {
                draw_fact(f, Rect::new(x, y, width, pane.bottom().saturating_sub(y)), &mut y,
                    "Now", &item.label, theme);
            }
            if let Some(rate) = snapshot.rate_bytes_per_sec.filter(|rate| *rate > 0) {
                draw_fact(f, Rect::new(x, y, width, pane.bottom().saturating_sub(y)), &mut y,
                    "Rate", &format!("{}/s", human_bytes(rate)), theme);
                if let Some(total) = snapshot.totals.bytes_total {
                    let remaining = total.saturating_sub(snapshot.totals.bytes_done);
                    let seconds = remaining.saturating_add(rate - 1) / rate;
                    if seconds > 0 {
                        let mins = seconds.saturating_add(59) / 60;
                        let eta = if mins < 60 { format!("{mins}m") }
                            else { format!("{}h {}m", mins / 60, mins % 60) };
                        draw_fact(f, Rect::new(x, y, width, pane.bottom().saturating_sub(y)), &mut y,
                            "Time left", &eta, theme);
                    }
                }
            }
        }
    }
    let (source, target) = native_source_destination(record)
        .unwrap_or_else(|| (summarize_sources(record), summarize_destination(record)));
    let source_name = if record.is_native_extraction() { "Image" } else { "From" };
    let target_name = if record.is_native_extraction() { "Into" } else { "To" };
    draw_fact(f, Rect::new(x, y, width, pane.bottom().saturating_sub(y)), &mut y,
        source_name, &source, theme);
    draw_fact(f, Rect::new(x, y, width, pane.bottom().saturating_sub(y)), &mut y,
        target_name, &target, theme);
    if entry.item_count() > 0 {
        let mut segments = Vec::new();
        if entry.completed_count() > 0 {
            segments.push(format!("{} {}", entry.completed_count(),
                if record.is_native_extraction() { "extracted" }
                else if record.is_move { "moved" } else { "copied" }));
        }
        if entry.failed_count() > 0 { segments.push(format!("{} failed", entry.failed_count())); }
        if entry.skipped_count() > 0 { segments.push(format!("{} skipped", entry.skipped_count())); }
        if entry.remaining_count() > 0 { segments.push(format!("{} left", entry.remaining_count())); }
        draw_fact(f, Rect::new(x, y, width, pane.bottom().saturating_sub(y)), &mut y,
            if record.is_native_extraction() { "Tracks" } else { "Items" }, &segments.join(" · "), theme);
    }
    if let Some(unfinished) = unfinished_description(entry, &state.incomplete_sizes) {
        draw_fact(f, Rect::new(x, y, width, pane.bottom().saturating_sub(y)), &mut y,
            "Unfinished", &unfinished, theme);
    }
    let renamed = record.quarantine_artifacts.len() + record.native_rename_intents.len();
    if renamed > 0 {
        draw_fact(f, Rect::new(x, y, width, pane.bottom().saturating_sub(y)), &mut y,
            "Renamed", &format!("{renamed} original{} under temporary name{}",
                plural(renamed), plural(renamed)), theme);
    }
    let deleted_started = record.quarantine_artifacts.iter().filter(|art|
        art.state == DurableQuarantineState::DeletionStarted).count();
    if deleted_started > 0 {
        draw_fact(f, Rect::new(x, y, width, pane.bottom().saturating_sub(y)), &mut y,
            "Deleting", &format!("began for {deleted_started} selected item{}", plural(deleted_started)), theme);
    }
    let locked = entry.blocked_paths().len();
    if locked > 0 {
        draw_fact(f, Rect::new(x, y, width, pane.bottom().saturating_sub(y)), &mut y,
            "Locked", &format!("{locked} files and folders"), theme);
    }
    if let Some(other) = newer_interrupted_job(&state.entries, entry)
        .or_else(|| older_interrupted_job(&state.entries, entry)) {
        draw_fact(f, Rect::new(x, y, width, pane.bottom().saturating_sub(y)), &mut y,
            "Shares", &format!("{}", job_display_name(&state.entries[other])), theme);
    }
    if job_has_files(entry) && pane.height >= 5 {
        let pill_y = pane.bottom() - 2;
        let label = "Show files…";
        let rect = draw_neutral_pill(f,
            Rect::new(x, pill_y, width, 1), label, false, theme);
        buttons.record_button(TuiButton::InterruptedShowFiles, rect);
    }
}

fn job_summary(entry: &RecoveryEntry, entries: &[RecoveryEntry]) -> String {
    let actions = window_actions(entry, entries);
    if actions.contains(&WindowAction::Dismiss) {
        return "Dismiss releases the locks and removes the finished job. Your files stay unchanged.".into();
    }
    if actions.contains(&WindowAction::Tidy) {
        return "Tidy up safely removes unfinished private paths and releases the locks.".into();
    }
    if actions.contains(&WindowAction::Older) {
        return "Finish waits for the older job. Undo restores what it safely can.".into();
    }
    if actions.contains(&WindowAction::Finish) {
        let remaining = entry.remaining_count();
        let verb = if entry.kind_label() == "extract" { "extracts" } else if entry.kind_label() == "move" { "moves" }
            else { "copies" };
        let note = if entry.irreversible() { " Undo cannot bring back deleted originals." }
            else { " Undo restores restorable originals, not finished output." };
        return format!("Finish {verb} the {remaining} left.{note}");
    }
    if actions.contains(&WindowAction::Release) {
        return "Release keeps the damaged record but frees its proved-stale locks; no files are changed.".into();
    }
    if actions.contains(&WindowAction::Undo) {
        return "Undo removes unfinished output and restores verified originals; finished files stay.".into();
    }
    if actions.contains(&WindowAction::ForceStop) {
        return "Force stop first verifies the worker; cleanup waits for its lease to release.".into();
    }
    if entry.is_live() {
        return "Progress can be minimized without stopping the worker. Stop requests cancellation.".into();
    }
    if entry.resume_committed { return "Finish is queued and starts when the current task ends.".into(); }
    "Close returns to Browse. This job's files stay in place.".into()
}

fn draw_action_footer(f: &mut Frame, state: &RecoveryUiState,
    buttons: &mut ButtonRenderMap, area: Rect, theme: super::theme::Theme,
) {
    if area.width == 0 { return; }
    let actions = state.selected_entry().map(|e| window_actions(e, &state.entries))
        .unwrap_or_default();
    let right = "? all keys";
    let help_width = (super::display_width::width(right) as u16 + 2).min(area.width);
    let help = Rect::new(area.right() - help_width, area.y, help_width, 1);
    draw_text(f, help, right, theme.cyan, theme.panel_bg);
    buttons.record_button(TuiButton::InterruptedHelp, help);
    let room = area.width.saturating_sub(help_width);
    let mut parts = actions.iter().map(|action| {
        (format!(" {} {} ", action.key(), action.label()),
            if action.is_affirmative() { theme.success } else { theme.blue })
    }).collect::<Vec<_>>();
    parts.push((" Esc Close ".into(), theme.dismiss));
    let widths = parts.iter().map(|(label, _)| super::display_width::width(label))
        .collect::<Vec<_>>();
    let sum = widths.iter().sum::<usize>();
    let gap = if sum + parts.len().saturating_sub(1) * 2 <= room as usize { 2 } else { 1 };
    let mut x = area.x + (room as usize).saturating_sub(sum + parts.len().saturating_sub(1) * gap) as u16 / 2;
    for (index, ((label, color), width)) in parts.iter().zip(widths).enumerate() {
        if x >= area.x + room { break; }
        let rect = Rect::new(x, area.y, (width as u16).min(area.x + room - x), 1);
        let is_focused = index < actions.len() && index == state.focus;
        draw_text(f, rect, label, if is_focused { theme.pill_active_fg } else { *color },
            if is_focused { theme.pill_active_bg } else { theme.panel_bg });
        if index < actions.len() { buttons.record_button(TuiButton::RecoveryAction(index as u8), rect); }
        else { buttons.record_button(TuiButton::InterruptedClose, rect); }
        x = x.saturating_add(width as u16 + gap as u16);
    }
}

fn draw_modal_frame(f: &mut Frame, app: &mut AppState,
    title: &str, theme: super::theme::Theme,
) -> Rect {
    let rect = popup_rect(f.size(), app.recovery_ui.maximized);
    f.render_widget(Clear, rect);
    let block = super::draw::solid_title_block(rect, format!("▸ {title}"), theme.border, theme);
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    f.render_widget(Block::default().style(Style::default().bg(theme.surface)), inner);
    inner
}

/// Describe one Undo from the durable record. Use exactly the same rows for
/// the single-job and Undo-all confirmations, rather than promising generic
/// restoration in the bulk path.
fn append_undo_effects(
    entry: &RecoveryEntry,
    sizes: &BTreeMap<PathBuf, Option<u64>>,
    theme: super::theme::Theme,
    rows: &mut Vec<(String, Color)>,
) {
    rows.push((format!("{}  {}", entry.kind_label().to_ascii_uppercase(),
        job_display_name(entry)), theme.value));
    rows.push(("THIS WILL".into(), theme.header));
    if let Some(unfinished) = unfinished_description(entry, sizes) {
        rows.push((format!("✓ Remove {unfinished}."), theme.success));
    }
    if let Some(record) = entry.record.as_ref() {
        let restorable = record.quarantine_artifacts.iter()
            .filter(|artifact| artifact.state != DurableQuarantineState::DeletionStarted)
            .map(|artifact| &artifact.original_source)
            .chain(record.native_rename_intents.iter().map(|intent| &intent.source))
            .collect::<BTreeSet<_>>().len();
        if restorable > 0 {
            rows.push((format!(
                "✓ Check {restorable} original name{} and put back only verified surviving sources.",
                plural(restorable)), theme.success));
        }
        let deletion_started = record.quarantine_artifacts.iter()
            .filter(|artifact| artifact.state == DurableQuarantineState::DeletionStarted).count();
        if deletion_started > 0 {
            rows.push((format!(
                "✓ Check surviving source content for {deletion_started} original{} whose deletion began.",
                plural(deletion_started)), theme.success));
        }
    }
    let locked = entry.blocked_paths().len();
    if locked > 0 {
        rows.push((format!("✓ Release {locked} locked path{}.", plural(locked)), theme.success));
    }
    rows.push((String::new(), theme.text));
    rows.push(("THIS WON'T".into(), theme.header));
    let completed = entry.completed_count();
    if completed > 0 {
        let unit = if entry.kind_label() == "extract" { "extracted track" } else { "finished item" };
        rows.push((format!("× Remove {completed} {unit}{} already completed in the destination.",
            plural(completed)), theme.text_muted));
        if let Some(record) = entry.record.as_ref() {
            let destination = native_source_destination(record)
                .map(|(_, into)| into).unwrap_or_else(|| summarize_destination(record));
            if destination != "unavailable" {
                rows.push((format!("    Finished output stays in {destination}."), theme.text_muted));
            }
        }
    } else {
        rows.push(("× Remove any finished destination output; none is recorded.".into(),
            theme.text_muted));
    }
    if let Some(record) = entry.record.as_ref() {
        let irreversible = record.quarantine_artifacts.iter()
            .filter(|artifact| artifact.state == DurableQuarantineState::DeletionStarted).count();
        if irreversible > 0 {
            rows.push((format!(
                "× Bring back deleted content from {irreversible} original{} whose deletion began.",
                plural(irreversible)), theme.error));
        }
    }
}

fn confirm_description(state: &RecoveryUiState, theme: super::theme::Theme) -> (String, Vec<(String, Color)>, String) {
    let entry = state.selected_entry();
    let mut lines = Vec::<(String, Color)>::new();
    let title = match state.surface {
        RecoverySurface::BulkDiscardConfirm => format!("Undo {} jobs", state.bulk_discard_count()),
        RecoverySurface::StopConfirm => format!("Stop {}", entry.map_or("job", |e| e.kind_label())),
        RecoverySurface::ForceStopConfirm => "Force stop".into(),
        RecoverySurface::SetAsideConfirm => "Release record".into(),
        _ => format!("Undo {}", entry.map_or("job", |e| e.kind_label())),
    };
    match state.surface {
        RecoverySurface::DiscardConfirm => {
            lines.push(("Undo applies only to the saved unfinished work.".into(), theme.value));
            lines.push((String::new(), theme.text));
            if let Some(entry) = entry {
                append_undo_effects(entry, &state.incomplete_sizes, theme, &mut lines);
            }
        }
        RecoverySurface::BulkDiscardConfirm => {
            lines.push((format!("Undo the {} jobs needing a decision, one by one.",
                state.bulk_discard_count()), theme.value));
            lines.push((String::new(), theme.text));
            for entry in state.entries.iter().filter(|entry| entry.needs_attention() && entry.can_discard()) {
                append_undo_effects(entry, &state.incomplete_sizes, theme, &mut lines);
                lines.push((String::new(), theme.text));
            }
        }
        RecoverySurface::StopConfirm => {
            lines.push((format!("Choose what happens to {}.", entry.map(job_display_name)
                .unwrap_or_else(|| "this job".into())), theme.value));
            lines.push((String::new(), theme.text));
            lines.push(("THIS WILL".into(), theme.header));
            lines.push(("✓ Ask this worker to cancel; it may stop mid-file.".into(), theme.success));
            lines.push(("✓ After the worker exits, undo safely tracked unfinished output.".into(), theme.success));
            lines.push((String::new(), theme.text));
            lines.push(("THIS WON'T".into(), theme.header));
            lines.push(("× Delete the files already finished in the destination.".into(), theme.text_muted));
        }
        RecoverySurface::ForceStopConfirm => {
            lines.push(("Choose whether to signal the current worker.".into(), theme.value));
            lines.push((String::new(), theme.text));
            lines.push(("THIS WILL".into(), theme.header));
            lines.push(("✓ Verify this exact worker's identity before signaling it.".into(), theme.success));
            lines.push(("✓ After its lease releases, attempt the pending cleanup.".into(), theme.success));
            lines.push((String::new(), theme.text));
            lines.push(("THIS WON'T".into(), theme.header));
            lines.push(("× Change files while the worker still owns its lease.".into(), theme.text_muted));
            lines.push(("▲ A partially written file may remain until cleanup finishes.".into(), theme.warning));
        }
        RecoverySurface::SetAsideConfirm => {
            lines.push(("Choose whether to release this unreadable record.".into(), theme.value));
            lines.push((String::new(), theme.text));
            lines.push(("THIS WILL".into(), theme.header));
            lines.push(("✓ Release reservations only after verifying the worker has gone.".into(), theme.success));
            lines.push(("✓ Keep the damaged record in a private location for a bug report.".into(), theme.success));
            lines.push((String::new(), theme.text));
            lines.push(("THIS WON'T".into(), theme.header));
            lines.push(("× Delete or modify your source or destination files.".into(), theme.text_muted));
            lines.push(("▲ Some files may be partial. Check the affected folders.".into(), theme.warning));
        }
        _ => {}
    }
    let confirm_label = match state.surface {
        RecoverySurface::BulkDiscardConfirm => format!("Undo {} jobs", state.bulk_discard_count()),
        RecoverySurface::StopConfirm => format!("Stop {}", entry.map_or("job", |e| e.kind_label())),
        RecoverySurface::ForceStopConfirm => "Force stop".into(),
        RecoverySurface::SetAsideConfirm => "Release".into(),
        _ => format!("Undo {}", entry.map_or("job", |e| e.kind_label())),
    };
    (title, lines, confirm_label)
}

fn draw_confirm_popup(f: &mut Frame, app: &mut AppState, theme: super::theme::Theme) {
    let (title, rows, confirm_label) = confirm_description(&app.recovery_ui, theme);
    let inner = draw_modal_frame(f, app, &title, theme);
    if inner.height < 4 { return; }
    let top = Rect::new(inner.x, inner.y, inner.width, 1);
    f.render_widget(Block::default().style(Style::default().bg(theme.hover_bg)), top);
    draw_text(f, Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), 1),
        &title, theme.value, theme.hover_bg);
    let body_start = inner.y + 2;
    let body_rows = inner.bottom().saturating_sub(body_start + 2) as usize;
    let scroll = app.recovery_ui.details_scroll.min(rows.len().saturating_sub(body_rows));
    app.recovery_ui.details_scroll = scroll;
    for (offset, (line, color)) in rows.iter().skip(scroll).take(body_rows).enumerate() {
        draw_text(f, Rect::new(inner.x + 2, body_start + offset as u16,
            inner.width.saturating_sub(4), 1), line, *color, theme.surface);
    }
    let y = inner.bottom() - 1;
    let accept = format!(" {confirm_label} ");
    let cancel = " Cancel ";
    let group_width = (super::display_width::width(&accept) + super::display_width::width(cancel) + 2) as u16;
    let start_x = inner.right().saturating_sub(group_width + 1).max(inner.x + 1);
    let cancel_rect = Rect::new(start_x, y,
        (super::display_width::width(cancel) as u16).min(inner.right().saturating_sub(start_x)), 1);
    let focused_cancel = app.recovery_ui.focus != 0;
    draw_text(f, cancel_rect, cancel,
        if focused_cancel { theme.pill_active_fg } else { theme.dismiss },
        if focused_cancel { theme.pill_active_bg } else { theme.pill_neutral_bg });
    app.button_map.record_button(TuiButton::RecoveryConfirm(1), cancel_rect);
    let accept_x = cancel_rect.right().saturating_add(2);
    if accept_x < inner.right() {
        let accept_rect = Rect::new(accept_x, y,
            (super::display_width::width(&accept) as u16).min(inner.right() - accept_x), 1);
        draw_text(f, accept_rect, &accept,
            if app.recovery_ui.focus == 0 { theme.pill_active_fg } else { theme.panel_bg },
            if app.recovery_ui.focus == 0 { theme.pill_active_bg } else { theme.success });
        app.button_map.record_button(TuiButton::RecoveryConfirm(0), accept_rect);
    }
}

/// A section's selected paths are grouped by immediate parent, with the
/// parent printed only once. The third field is an optional private on-disk
/// name, kept visible under the corresponding user-facing basename.
fn append_grouped_file_paths(
    lines: &mut Vec<(String, &'static str)>,
    items: impl IntoIterator<Item = (PathBuf, String, Option<PathBuf>)>,
) {
    let mut grouped = BTreeMap::<PathBuf, Vec<(String, String, Option<PathBuf>)>>::new();
    for (path, suffix, on_disk) in items {
        let parent = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
        let basename = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned();
        grouped.entry(parent).or_default().push((basename, suffix, on_disk));
    }
    for (parent, children) in grouped {
        lines.push((parent.display().to_string(), "dim"));
        for (basename, suffix, on_disk) in children {
            lines.push((format!("  {basename}{suffix}"), "value"));
            if let Some(private) = on_disk {
                let local = private.strip_prefix(&parent).unwrap_or(private.as_path());
                lines.push((format!("    on disk: {}", local.display()), "dim"));
            }
        }
    }
}

fn files_popup_lines(state: &RecoveryUiState) -> Vec<(String, &'static str)> {
    let Some(entry) = state.selected_entry() else { return Vec::new(); };
    let mut lines = Vec::new();
    if let Some(record) = entry.record.as_ref() {
        if !record.temp_artifacts.is_empty() {
            lines.push(("UNFINISHED".into(), "header"));
            append_grouped_file_paths(&mut lines, record.temp_artifacts.iter().map(|artifact| {
                let suffix = match state.incomplete_sizes.get(&artifact.path) {
                    Some(Some(n)) => format!(" · {} so far", human_bytes(*n)),
                    Some(None) => " · size unknown".into(),
                    None => " · size being measured".into(),
                };
                (artifact.destination.clone(), suffix, Some(artifact.path.clone()))
            }));
            lines.push((String::new(), "dim"));
        }
        if !record.quarantine_artifacts.is_empty() || !record.native_rename_intents.is_empty() {
            lines.push(("WAITING TO BE RESTORED".into(), "header"));
            append_grouped_file_paths(&mut lines, record.quarantine_artifacts.iter()
                .map(|artifact| (artifact.original_source.clone(), String::new(), Some(artifact.path.clone())))
                .chain(record.native_rename_intents.iter().map(|intent|
                    (intent.source.clone(), String::new(), Some(intent.destination.clone())))));
            lines.push((String::new(), "dim"));
        }
        // Only the final checkpoint for each selected root is its outcome.
        // A failure followed by a successful retry must not reappear in FAILED.
        for (title, disposition) in [
            ("FAILED", tui_file_picker::FileTaskRootDisposition::Failed),
            ("SKIPPED", tui_file_picker::FileTaskRootDisposition::Skipped),
        ] {
            let paths = record.mappings.iter().filter(|mapping|
                last_recorded_root_disposition(record, mapping) == Some(disposition))
                .map(|mapping| mapping.source.clone()).collect::<BTreeSet<_>>();
            if !paths.is_empty() {
                lines.push((title.into(), "header"));
                append_grouped_file_paths(&mut lines, paths.into_iter().map(|path|
                    (path, String::new(), None)));
                lines.push((String::new(), "dim"));
            }
        }
        if record.is_native_extraction() && !record.native_skipped_selections.is_empty() {
            lines.push(("SKIPPED TRACKS".into(), "header"));
            for skipped in &record.native_skipped_selections {
                lines.push((format!("{} · angle {} · {}", format!("{:?}", skipped.presentation),
                    skipped.angle, skipped.track_number.map_or_else(|| "whole program".into(),
                        |number| format!("track {number}"))), "value"));
            }
            lines.push((String::new(), "dim"));
        }
    }
    let blocked = entry.blocked_paths();
    if !blocked.is_empty() {
        lines.push(("LOCKED".into(), "header"));
        append_grouped_file_paths(&mut lines, blocked.into_iter().map(|path|
            (path, String::new(), None)));
    }
    lines
}

fn draw_close_pill(f: &mut Frame, app: &mut AppState, inner: Rect,
    title: &str, theme: super::theme::Theme,
) {
    if inner.height < 1 { return; }
    let y = inner.bottom() - 1;
    let w = (super::display_width::width(title) + 2) as u16;
    let x = inner.right().saturating_sub(w + 1);
    let rect = draw_neutral_pill(f, Rect::new(x, y, w.min(inner.width), 1), title, true, theme);
    app.button_map.record_button(TuiButton::RecoveryDetailsClose, rect);
}

fn draw_files_popup(f: &mut Frame, app: &mut AppState, theme: super::theme::Theme) {
    let inner = draw_modal_frame(f, app, "files touched by this job", theme);
    if inner.height < 5 { return; }
    let title = app.recovery_ui.selected_entry().map(job_display_name).unwrap_or_default();
    let raised = Rect::new(inner.x, inner.y, inner.width, 1);
    f.render_widget(Block::default().style(Style::default().bg(theme.hover_bg)), raised);
    draw_text(f, Rect::new(inner.x + 2, inner.y, inner.width.saturating_sub(4), 1),
        &title, theme.value, theme.hover_bg);
    let lines = files_popup_lines(&app.recovery_ui);
    let start_y = inner.y + 2;
    let room = inner.bottom().saturating_sub(start_y + 2) as usize;
    let scroll = app.recovery_ui.details_scroll.min(lines.len().saturating_sub(room));
    app.recovery_ui.details_scroll = scroll;
    for (offset, (text, kind)) in lines.iter().skip(scroll).take(room).enumerate() {
        let fg = match *kind { "header" => theme.header, "dim" => theme.text_muted, _ => theme.value };
        let bg = if offset % 2 == 0 { theme.surface } else { theme.panel_bg };
        let rect = Rect::new(inner.x + 2, start_y + offset as u16, inner.width.saturating_sub(4), 1);
        f.render_widget(Block::default().style(Style::default().bg(bg)), rect);
        draw_path(f, rect, text, fg, bg);
    }
    draw_close_pill(f, app, inner, "Done", theme);
}

fn draw_error_popup(f: &mut Frame, app: &mut AppState, theme: super::theme::Theme) {
    let inner = draw_modal_frame(f, app, "can't read this job", theme);
    let Some(entry) = app.recovery_ui.selected_entry() else { return; };
    let error = entry.diagnostic.as_deref().unwrap_or("The saved job record could not be read.");
    if inner.height > 5 {
        draw_path(f, Rect::new(inner.x + 2, inner.y + 1, inner.width.saturating_sub(4), 1),
            &format!("Record: {}", entry.journal_path.display()), theme.value, theme.surface);
        draw_path(f, Rect::new(inner.x + 2, inner.y + 3, inner.width.saturating_sub(4), 1),
            &user_safe_diagnostic(error), theme.error, theme.surface);
        draw_text(f, Rect::new(inner.x + 2, inner.y + 4, inner.width.saturating_sub(4), 1),
            "The touched files are unknown and may be partial.", theme.warning, theme.surface);
    }
    draw_close_pill(f, app, inner, "Done", theme);
}

fn interrupted_help_lines(state: &RecoveryUiState) -> Vec<String> {
    let actions = state.selected_entry().map(|entry|
        window_actions(entry, &state.entries)).unwrap_or_default();
    let mut lines = vec![
        "IN THIS HELP POPUP".to_string(),
        "↑ ↓ / PgUp PgDn  scroll this help".to_string(),
        "Enter / Esc  close Help".to_string(),
        "Alt+M  maximize or restore (also in window)".to_string(),
        "AFTER CLOSING HELP, IN INTERRUPTED JOBS".to_string(),
        "↑ ↓  select a job".to_string(),
        "Enter  activate focused action".to_string(),
        "Tab  next focus; compact windows switch Jobs / Job".to_string(),
        "Esc  close the window".to_string(),
        "Alt+M  maximize or restore".to_string(),
    ];
    if state.selected_entry().is_some_and(job_has_files) {
        lines.push("v  Show files…".into());
    }
    for action in actions {
        lines.push(format!("{}  {}", action.key(), action.label()));
    }
    if state.bulk_discard_count() > 0 { lines.push("a  Undo all…".into()); }
    if state.resume_all_available() { lines.push("Shift+A  Finish all".into()); }
    lines.push("?  open this help".into());
    lines
}

fn draw_help_popup(f: &mut Frame, app: &mut AppState, theme: super::theme::Theme) {
    let inner = draw_modal_frame(f, app, "interrupted jobs · all keys", theme);
    let lines = interrupted_help_lines(&app.recovery_ui);
    let available = inner.height.saturating_sub(4) as usize;
    let scroll = app.recovery_ui.details_scroll.min(lines.len().saturating_sub(available));
    app.recovery_ui.details_scroll = scroll;
    for (i, line) in lines.iter().skip(scroll).take(available).enumerate() {
        draw_text(f, Rect::new(inner.x + 2, inner.y + 2 + i as u16, inner.width.saturating_sub(4), 1),
            line, theme.value, theme.surface);
    }
    draw_close_pill(f, app, inner, "Done", theme);
}

fn draw_refusal_popup(f: &mut Frame, app: &mut AppState, theme: super::theme::Theme) {
    // A reservation conflict may originate from different filesystem
    // mutations. Without a grounded action verb, name only the blocked path.
    let attempted = match &app.recovery_ui.details_reason {
        RecoveryDetailsReason::Refusal(path) => path.file_name().unwrap_or(path.as_os_str())
            .to_string_lossy().into_owned(),
        _ => "this path".into(),
    };
    let title = format!("Can't change {attempted}");
    let inner = draw_modal_frame(f, app, &title, theme);
    if inner.height < 5 { return; }
    let entry = app.recovery_ui.selected_entry();
    if let Some(entry) = entry {
        let source = match &app.recovery_ui.details_reason {
            RecoveryDetailsReason::Refusal(path) => path.file_name().unwrap_or(path.as_os_str())
                .to_string_lossy().into_owned(),
            _ => "this path".into(),
        };
        draw_text(f, Rect::new(inner.x + 2, inner.y + 2, inner.width.saturating_sub(4), 1),
            &format!("{source} is locked by an interrupted {}.", entry.kind_label()),
            theme.warning, theme.surface);
        draw_text(f, Rect::new(inner.x + 2, inner.y + 3, inner.width.saturating_sub(4), 1),
            "Finish or undo the job before changing the locked path.", theme.text, theme.surface);
        if inner.height > 7 {
            let raised = Rect::new(inner.x + 2, inner.y + 5, inner.width.saturating_sub(4), 1);
            f.render_widget(Block::default().style(Style::default().bg(theme.hover_bg)), raised);
            draw_text(f, raised, &format!(" {} · {} · {}", job_display_name(entry),
                entry.kind_label(), entry_progress(entry)), theme.value, theme.hover_bg);
        }
    }
    let can_undo = entry.is_some_and(|entry| entry.can_discard());
    let can_finish = entry.is_some_and(|entry| window_actions(entry, &app.recovery_ui.entries)
        .contains(&WindowAction::Finish));
    let y = inner.bottom() - 1;
    // The destructive confirmation is the sole affirmative (rightmost)
    // button; Cancel remains available when a worker state hides Finish.
    let choices = ["Show job…", "Undo…", "Cancel", "Finish"];
    let mut x = inner.x + 2;
    for (index, text) in choices.into_iter().enumerate() {
        if (index == 1 && !can_undo) || (index == 3 && !can_finish) { continue; }
        let area = Rect::new(x, y, inner.right().saturating_sub(x), 1);
        let rect = if index == 3 {
            let label = format!(" {text} ");
            let width = (super::display_width::width(&label) as u16).min(area.width);
            let hit = Rect::new(area.x, area.y, width, 1);
            draw_text(f, hit, &label, theme.panel_bg, theme.success);
            hit
        } else { draw_neutral_pill(f, area, text, index == 2, theme) };
        app.button_map.record_button(TuiButton::InterruptedRefusalAction(index as u8), rect);
        x = rect.right().saturating_add(2);
        if x >= inner.right() { break; }
    }
}

pub fn draw(f: &mut Frame, app: &mut AppState, theme: super::theme::Theme) {
    app.recovery_ui.compact = window_rect(f.size(), app.recovery_ui.maximized).width < 120;
    app.recovery_ui.last_screen = f.size();
    draw_main(f, app, theme);
    match app.recovery_ui.surface {
        RecoverySurface::Window => {}
        RecoverySurface::Files => draw_files_popup(f, app, theme),
        RecoverySurface::Refusal => draw_refusal_popup(f, app, theme),
        RecoverySurface::Error => draw_error_popup(f, app, theme),
        RecoverySurface::Help => draw_help_popup(f, app, theme),
        RecoverySurface::DiscardConfirm | RecoverySurface::BulkDiscardConfirm
        | RecoverySurface::StopConfirm | RecoverySurface::ForceStopConfirm
        | RecoverySurface::SetAsideConfirm => draw_confirm_popup(f, app, theme),
    }
}
