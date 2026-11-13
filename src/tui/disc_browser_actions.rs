//! Activation and conversion glue for the Phase 4c disc browser overlay.
//!
//! The data/model helpers live in `disc_browser.rs`. This module is deliberately
//! UI-facing: it opens the overlay from browse/context/menu buttons, applies row
//! and footer button actions inside the overlay, and converts the selected
//! presentation into a real Convert source.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent};
use tokio::sync::mpsc;

use crate::disc::{DiscContents, PresentationId};
use crate::tui::app::{ActiveOverlay, AppScreen, AppState, SourceMode};
use crate::tui::button_map::TuiButton;
use crate::tui::command::select_default_disc_presentation_index;
use crate::tui::disc_browser::{
    metadata_for_disc_presentation, presentation_supports_stream_conversion,
    source_mode_for_presentation, DiscBrowserState, DiscProbeFollowup,
};
use crate::tui::message::AppMessage;

const BLURAY_STREAM_CONVERSION_UNAVAILABLE: &str =
    "Blu-ray stream-specific conversion is not available until Blu-ray materializer SourceOptions are implemented";

fn selected_presentation_supports_stream_conversion(
    contents: &DiscContents,
    presentation_index: usize,
) -> Result<bool, String> {
    let presentation = contents
        .presentations
        .get(presentation_index)
        .ok_or_else(|| format!("No disc stream at index {}", presentation_index + 1))?;
    Ok(presentation_supports_stream_conversion(presentation))
}

fn require_selected_presentation_stream_conversion(
    contents: &DiscContents,
    presentation_index: usize,
) -> Result<(), String> {
    if selected_presentation_supports_stream_conversion(contents, presentation_index)? {
        Ok(())
    } else {
        Err(BLURAY_STREAM_CONVERSION_UNAVAILABLE.to_string())
    }
}

/// Open the Audio Streams overlay for the currently highlighted browse entry.
///
/// If the disc has already been probed, this opens synchronously from the cache.
/// Otherwise it starts or joins the async disc probe and records a one-shot
/// `DiscProbeFollowup::OpenDiscBrowser`. The `DiscProbeComplete` handler opens
/// the overlay as soon as a current `DiscContents` result is cached, so users
/// do not have to click "Browse Audio Streams..." twice.
pub fn open_selected_disc_browser(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let Some(path) = selected_entry_effective_disc_path(app) else {
        if app.browse.selected_entry().is_none() {
            app.set_status("No browse entry selected");
        } else {
            app.set_status("Selected entry is not a browsable disc source");
        }
        return;
    };

    open_disc_browser_for_path(app, path, tx);
}

/// Return the selected Browse path when it is a disc source, or when an
/// explicit disc action targets a not-yet-classified `.iso` (e.g. one the
/// scanner filed as `Archive` in the lazy-classification window). The
/// extension check is pure string work — no ISO header probe runs on the
/// reducer (that read can stall on slow mounts). The async disc probe the
/// dispatch kicks off resolves the concrete kind, reclassifies the row on
/// completion, and reports a clean error for non-disc ISOs.
fn selected_entry_effective_disc_path(app: &mut AppState) -> Option<PathBuf> {
    let index = app.browse.selected_index;
    let entry = app.browse.entries.get(index)?;
    let path = entry.path.clone();
    let current_kind = entry.kind.clone();

    if current_kind.is_disc_source() {
        return Some(path);
    }

    if let Some(classification) = app.browse.valid_folder_classification_for_entry(entry) {
        if classification.kind == crate::tui::browse::FolderClassificationKind::Disc {
            return Some(classification.disc_probe_source_path(&path).to_path_buf());
        }
    }

    let is_iso = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("iso"))
        .unwrap_or(false);
    if matches!(current_kind, crate::tui::browse::EntryKind::Archive) && is_iso {
        return Some(path);
    }
    None
}

/// Open the Audio Streams overlay for a specific disc path, or start probing it.
pub fn open_disc_browser_for_path(app: &mut AppState, path: PathBuf, tx: &mpsc::Sender<AppMessage>) {
    if let Some(contents) = cached_disc_contents(app, &path) {
        open_disc_browser_from_contents(app, contents.as_ref().clone(), path, tx);
        return;
    }

    if let Some(error) = cached_disc_error(app, &path) {
        app.set_status(format!(
            "Disc analysis failed for current source; use Analyze to retry: {error}"
        ));
        return;
    }

    app.browse
        .disc_probe_followup
        .insert(path.clone(), DiscProbeFollowup::OpenDiscBrowser);
    ensure_disc_probe(app, path, tx);
}

/// Open the modal overlay from already available `DiscContents`.
pub fn open_disc_browser_from_contents(app: &mut AppState, contents: DiscContents, source_path: PathBuf, tx: &mpsc::Sender<AppMessage>) {
    if contents.presentations.is_empty() {
        app.set_status("Disc contains no selectable audio streams");
        return;
    }

    let state = DiscBrowserState::new(contents, source_path.clone());
    if state.probe_statuses.iter().any(Option::is_none) {
        let id = state.probe_id;
        let cancel = state.probe_cancel.clone();
        // Probe every eligible stream, whether or not it was initially
        // checked. Keep the exact presentation index with its job: a program
        // with no materializable chapter must not shift the results onto an
        // unrelated stream later in the list.
        let mut probe_view = state.clone();
        probe_view.selected = state.probe_statuses.iter().map(Option::is_none).collect();
        let mut job = probe_view.native_job(std::env::temp_dir());
        let mut pending = std::mem::take(&mut job.programs).into_iter();
        let mut probes = Vec::new();
        for (index, status) in state.probe_statuses.iter().enumerate() {
            if status.is_none() {
                let program = if state.items_checked[index].iter().any(|checked| *checked) {
                    pending.next()
                } else { None };
                probes.push((index, program));
            }
        }
        let tx = tx.clone();
        tokio::spawn(async move {
            for (program_index, program) in probes {
                if cancel.is_cancelled() { break; }
                let result = if let Some(program) = program {
                    let mut view = job.clone();
                    view.programs = vec![program];
                    crate::convert::pipeline::native_extract::probe_selected_sources(
                        view, cancel.clone()).await
                } else {
                    Err("No selected chapter available to probe".to_string())
                };
                if cancel.is_cancelled() { break; }
                if tx.send(AppMessage::DiscExtractProbeComplete {
                    path: source_path.clone(), probe_id: id, program_index, result,
                }).await.is_err() { break; }
            }
        });
    }
    app.active_overlay = ActiveOverlay::DiscBrowser(Box::new(state));
}

/// Context-menu entry point for `Convert (default stream)`.
///
/// This intentionally does not rely on the generic `ConvertCustom` path. For
/// DVD-Audio, a valid conversion request must carry the selected
/// `PresentationId` into `SourceOptions`, so the default action explicitly loads
/// the scored default presentation through the same DiscContents handoff used by the overlay.
/// When the cache is cold, it schedules the async disc probe and records a
/// one-shot follow-up instead of synchronously parsing/probing from the context
/// menu thread.
pub fn convert_default_disc_stream(
    app: &mut AppState,
    path: &Path,
    tx: &mpsc::Sender<AppMessage>,
) {
    if let Some(contents) = cached_disc_contents(app, path) {
        if let Err(err) = load_default_disc_presentation_for_convert(app, contents.as_ref().clone()) {
            app.set_status(format!("Default disc stream load failed: {err}"));
        }
        return;
    }

    app.browse
        .disc_probe_followup
        .insert(path.to_path_buf(), DiscProbeFollowup::ConvertDefaultStream);
    ensure_disc_probe(app, path.to_path_buf(), tx);
    app.set_status("Analyzing default disc stream...");
}

/// Run and consume a one-shot action recorded before an async disc probe.
pub fn handle_disc_probe_followup(
    app: &mut AppState,
    path: &Path,
    followup: DiscProbeFollowup,
    tx: &mpsc::Sender<AppMessage>,
) {
    match followup {
        DiscProbeFollowup::OpenDiscBrowser => {
            let Some(contents) = cached_disc_contents(app, path) else {
                app.set_status("Audio streams are not available after analysis");
                return;
            };
            open_disc_browser_from_contents(app, contents.as_ref().clone(), path.to_path_buf(), tx);
        }
        DiscProbeFollowup::ConvertDefaultStream => {
            let Some(contents) = cached_disc_contents(app, path) else {
                app.set_status("Default disc stream is not available after analysis");
                return;
            };
            if let Err(err) = load_default_disc_presentation_for_convert(app, contents.as_ref().clone()) {
                app.set_status(format!("Default disc stream load failed: {err}"));
            }
        }
    }
}

/// Context-menu entry point for `Convert Stream > ...`.
pub fn convert_selected_disc_stream_by_id(
    app: &mut AppState,
    path: &Path,
    id: &PresentationId,
) -> Result<(), String> {
    let contents = cached_disc_contents(app, path)
        .ok_or_else(|| format!("Disc is not probed yet: {}", path.display()))?;
    let index = contents
        .presentations
        .iter()
        .position(|presentation| &presentation.id == id)
        .ok_or_else(|| format!("Stream is no longer available: {id:?}"))?;
    require_selected_presentation_stream_conversion(contents.as_ref(), index)?;
    load_disc_presentation_for_convert(app, contents.as_ref().clone(), index)
}


/// Context-menu dispatcher for Phase 4c disc actions.
///
/// Call this from the existing `ContextAction` executor before the generic
/// match arms. It returns `true` when it consumed the action.
pub fn handle_disc_context_action(
    app: &mut AppState,
    action: &crate::tui::context_menu::ContextAction,
    tx: &mpsc::Sender<AppMessage>,
) -> bool {
    match action {
        crate::tui::context_menu::ContextAction::ConvertDiscDefault => {
            let Some(path) = selected_entry_effective_disc_path(app) else {
                if app.browse.selected_entry().is_none() {
                    app.set_status("No browse entry selected");
                } else {
                    app.set_status("Selected entry is not a browsable disc source");
                }
                return true;
            };
            convert_default_disc_stream(app, &path, tx);
            true
        }
        crate::tui::context_menu::ContextAction::ConvertCustom => {
            // ConvertCustom is a generic action shared with archive entries.
            // Only consume it for entries ALREADY classified as disc sources;
            // an unprobed Archive `.iso` must fall through to the generic
            // archive conversion handler (extension-based disc acceptance is
            // reserved for explicitly disc-labeled actions).
            let strict_disc = app
                .browse
                .entries
                .get(app.browse.selected_index)
                .is_some_and(|entry| entry.kind.is_disc_source());
            if strict_disc {
                if let Some(path) = selected_entry_effective_disc_path(app) {
                    convert_default_disc_stream(app, &path, tx);
                    return true;
                }
            }
            false
        }
        crate::tui::context_menu::ContextAction::BrowseDiscStreams => {
            open_selected_disc_browser(app, tx);
            true
        }
        crate::tui::context_menu::ContextAction::ConvertDiscStream(id) => {
            let Some(path) = app.browse.selected_entry().map(|entry| entry.path.clone()) else {
                app.set_status("No browse entry selected");
                return true;
            };
            if let Err(err) = convert_selected_disc_stream_by_id(app, &path, id) {
                app.set_status(format!("Disc stream load failed: {err}"));
            }
            true
        }
        _ => false,
    }
}

/// Mouse/button dispatcher for all Phase 4c disc-browser buttons.
///
/// Call this from the existing TUI button dispatcher before the generic/default
/// arm. It returns `true` when it consumed the button. This is the single-click
/// entry point; mouse code that has click-count information should call
/// `handle_disc_browser_button_click()` instead.
pub fn handle_disc_browser_button(
    app: &mut AppState,
    button: &TuiButton,
    tx: &mpsc::Sender<AppMessage>,
) -> bool {
    handle_disc_browser_button_click(app, button, 1, tx)
}

/// Prefer configured working storage. The native worker chooses a private
/// sibling on the destination volume if the configured volume differs; the
/// destination never receives an incomplete output.
pub fn native_working_root(
    state: &DiscBrowserState,
    configured: Option<&Path>,
) -> PathBuf {
    if let Some(path) = configured {
        return path.to_path_buf();
    }
    let mut parent = state.destination.parent().unwrap_or(Path::new("."));
    while !parent.exists() {
        let Some(next) = parent.parent() else { break; };
        parent = next;
    }
    parent.join(".tonepoet-native-extraction-work")
}

fn start_native_extraction(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let ActiveOverlay::DiscBrowser(state) = &app.active_overlay else { return; };
    let root = native_working_root(state, app.config.conversion.scratch_directory.as_deref());
    let blockers = state.blockers_with_capacity_refresh(&root, true);
    if !blockers.is_empty() {
        app.set_status(format!("Cannot extract: {}", blockers.join("; ")));
        return;
    }
    let job = state.native_job(root);
    if let Err(error) = app.manager.add_native_extraction_job(job) {
        app.set_status(format!("Cannot queue native extraction: {error}"));
        return;
    }
    app.active_overlay = ActiveOverlay::None;
    app.current_screen = AppScreen::Queue;
    app.save_queue();
    if !app.processing_active {
        super::convert_actions::start_processing(app, tx);
    }
}

/// Handle a click according to the visible extraction controls. Double-clicking
/// a program checks it; it does not unexpectedly switch into Convert.
pub fn handle_disc_browser_button_click(
    app: &mut AppState,
    button: &TuiButton,
    click_count: u8,
    tx: &mpsc::Sender<AppMessage>,
) -> bool {
    handle_disc_browser_button_click_with_shift(app, button, click_count, false, tx)
}

/// Shift-modified mouse selection shares the keyboard range-check operations.
pub fn handle_disc_browser_button_click_with_shift(
    app: &mut AppState,
    button: &TuiButton,
    click_count: u8,
    shift: bool,
    tx: &mpsc::Sender<AppMessage>,
) -> bool {
    // A popup owns the overlay. Its background hitboxes must not remain
    // clickable through the transparent regions of the popup.
    if matches!(&app.active_overlay, ActiveOverlay::DiscBrowser(state) if state.panel.is_some()) {
        return true;
    }
    match button {
        TuiButton::BrowseInfoAnalyze => {
            if let Some(path) = selected_entry_effective_disc_path(app) {
                force_disc_reprobe(app, path, tx);
                true
            } else { false }
        }
        TuiButton::BrowseInfoAudioStreams => {
            open_selected_disc_browser(app, tx);
            true
        }
        TuiButton::DiscBrowserStream(index) | TuiButton::DiscBrowserExpand(index) => {
            let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay else { return false; };
            let previous = state.cursor;
            state.set_cursor(*index);
            if shift {
                for target in previous.min(*index)..=previous.max(*index) {
                    if state.is_extractable(target) { state.selected[target] = true; }
                }
            }
            state.item_cursor = 0;
            state.item_scroll = 0;
            state.focus = crate::tui::disc_browser::ExtractFocus::Programs;
            if matches!(button, TuiButton::DiscBrowserExpand(_)) || click_count >= 2 {
                state.toggle_selected(*index);
            }
            true
        }
        TuiButton::DiscBrowserAnglePrev(index) | TuiButton::DiscBrowserAngleNext(index) => {
            let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay else { return false; };
            state.set_cursor(*index);
            state.focus = crate::tui::disc_browser::ExtractFocus::Programs;
            let step = if matches!(button, TuiButton::DiscBrowserAnglePrev(_)) { -1 } else { 1 };
            state.cycle_angle_for(*index, step);
            true
        }
        TuiButton::DiscBrowserTrack(index) | TuiButton::DiscBrowserTrackCheck(index) => {
            let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay else { return false; };
            state.focus = crate::tui::disc_browser::ExtractFocus::Items;
            let prior = state.item_cursor;
            state.item_cursor = *index;
            if shift {
                state.check_item_range(prior, *index);
            } else if matches!(button, TuiButton::DiscBrowserTrackCheck(_)) {
                state.toggle_item(*index);
            }
            true
        }
        TuiButton::DiscBrowserOutput(index) => {
            let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay else { return false; };
            state.focus = crate::tui::disc_browser::ExtractFocus::Output;
            state.output_cursor = *index;
            state.panel = Some(state.current_output_panel());
            state.panel_cursor = 0;
            true
        }
        TuiButton::DiscBrowserExtract => {
            start_native_extraction(app, tx);
            true
        }
        TuiButton::DiscBrowserConvert => {
            activate_overlay_convert(app);
            true
        }
        TuiButton::DiscBrowserMore => {
            if let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay {
                state.panel = Some(crate::tui::disc_browser::ExtractPanel::More);
            }
            true
        }
        TuiButton::DiscBrowserClose => {
            if let ActiveOverlay::DiscBrowser(state) = &app.active_overlay {
                state.probe_cancel.cancel();
                app.active_overlay = ActiveOverlay::None;
                true
            } else { false }
        }
        _ => false,
    }
}

/// Convenience entry point for mouse handlers that classify repeated clicks.
pub fn handle_disc_browser_double_click(
    app: &mut AppState,
    button: &TuiButton,
    tx: &mpsc::Sender<AppMessage>,
) -> bool {
    handle_disc_browser_button_click(app, button, 2, tx)
}

fn cycle_native_option(state: &mut DiscBrowserState, step: i32) {
    use crate::convert::pipeline::native_extract::NativeWavContainer;
    use crate::convert::pipeline::DvdaDownmixPolicy;
    match state.output_rows().get(state.output_cursor).copied() {
        _ if state.focus == crate::tui::disc_browser::ExtractFocus::Programs => {
            state.cycle_angle_for(state.cursor, step);
        }
        Some("Format") if state.contents.format == crate::disc::DiscFormat::Sacd => {
            let selected = state.cursor;
            let has_dst = state.contents.presentations.get(selected).is_some_and(|p|
                p.tracks.iter().any(|t| t.format_note.as_deref().unwrap_or("").contains("DST")));
            if has_dst { state.keep_dst[selected] = !state.keep_dst[selected]; }
        }
        Some("Format") => {
            let all = [NativeWavContainer::Automatic, NativeWavContainer::Riff,
                NativeWavContainer::Rf64, NativeWavContainer::W64];
            let index = all.iter().position(|value| *value == state.wav_container).unwrap_or(0);
            state.wav_container = all[(index as i32 + step).rem_euclid(all.len() as i32) as usize];
        }
        Some("Downmix") => {
            let all = [DvdaDownmixPolicy::None, DvdaDownmixPolicy::FooInputDvdaCompatible,
                DvdaDownmixPolicy::FfmpegDefault, DvdaDownmixPolicy::Auto];
            let index = all.iter().position(|value| *value == state.downmix).unwrap_or(0);
            state.downmix = all[(index as i32 + step).rem_euclid(all.len() as i32) as usize];
        }
        Some("Assume decrypted") => { state.assume_decrypted = !state.assume_decrypted; }
        _ => {}
    }
}

/// Window-only navigation. No Convert actions run unless explicitly selected.
pub fn handle_disc_browser_key(
    app: &mut AppState,
    key: KeyEvent,
    tx: &mpsc::Sender<AppMessage>,
) {
    use crossterm::event::KeyModifiers;
    use crate::tui::disc_browser::{ExtractFocus, ExtractPanel};
    let ActiveOverlay::DiscBrowser(state) = &app.active_overlay else { return; };
    // Text editors and popups own their keys. In particular Ctrl+X must not
    // bypass an unfinished destination edit or a pending choice dialog.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('x') {
        if state.panel.is_none() && !state.editing_destination { start_native_extraction(app, tx); }
        return;
    }
    // Opening the existing editor mutates the overlay, so perform this route
    // before borrowing the extraction state for in-panel key dispatch.
    if matches!(&app.active_overlay, ActiveOverlay::DiscBrowser(state)
        if matches!(state.panel, Some(ExtractPanel::Tags | ExtractPanel::More))
            && !state.editing_destination)
        && key.code == KeyCode::Char('e')
    {
        match open_existing_metadata_editor_for_disc_cursor(app) {
            Ok(true) => {},
            Ok(false) => app.set_status("No selected presentation is available to edit"),
            Err(error) => app.set_status(format!("Cannot open tag editor: {error}")),
        }
        return;
    }
    // More routes to established app actions, not a second lookup or probe.
    if matches!(&app.active_overlay, ActiveOverlay::DiscBrowser(state)
        if matches!(state.panel, Some(ExtractPanel::More | ExtractPanel::Tags)))
        && key.code == KeyCode::Char('b')
    {
        crate::tui::command::execute_command(app,
            crate::tui::command::Command::TagsFromMb {query:None,catno:None,year:None},tx);
        return;
    }
    if let ActiveOverlay::DiscBrowser(state) = &app.active_overlay {
        if state.panel == Some(ExtractPanel::More)
            && state.contents.format != crate::disc::DiscFormat::Sacd
            && key.code == KeyCode::Char('a') {
            let path = state.source_path.clone();
            state.probe_cancel.cancel();
            app.active_overlay = ActiveOverlay::None;
            app.browse.disc_probe_followup.insert(path.clone(),
                crate::tui::disc_browser::DiscProbeFollowup::OpenDiscBrowser);
            force_disc_reprobe(app,path,tx);
            return;
        }
    }
    let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay else { return; };

    if state.editing_destination {
        match key.code {
            KeyCode::Esc => { state.editing_destination = false; state.editing_template = false; }
            KeyCode::Enter => {
                if state.editing_template {
                    if !state.destination_input.trim().is_empty() {
                        state.naming_template = state.destination_input.trim().to_string();
                    }
                } else {
                    let path = PathBuf::from(state.destination_input.trim());
                    if path.is_absolute() && !path.components().any(|component| matches!(component, std::path::Component::ParentDir)) {
                        state.destination = path;
                    }
                }
                state.editing_destination = false;
                state.editing_template = false;
            }
            KeyCode::Backspace => { state.destination_input.pop(); }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                state.destination_input.push(c);
            }
            _ => {}
        }
        return;
    }

    if let Some(panel) = state.panel {
        match (panel, key.code) {
            (_, KeyCode::Esc) => { state.panel = None; state.panel_cursor = 0; }
            (ExtractPanel::Files, KeyCode::Char('f')) => {
                state.file_per_program = !state.file_per_program;
                if !state.file_per_program { state.cue = false; }
            }
            (ExtractPanel::Files, KeyCode::Char('c')) => {
                if state.file_per_program && state.contents.format != crate::disc::DiscFormat::Sacd {
                    state.cue = !state.cue;
                }
            }
            (ExtractPanel::Tags, KeyCode::Char('t')) => state.tags = !state.tags,
            (ExtractPanel::Tags, KeyCode::Char('s')) => {
                use crate::convert::pipeline::native_extract::NativeTagSource;
                state.tag_source = match state.tag_source {
                    NativeTagSource::DiscAndSidecar => NativeTagSource::AuthoredTrack,
                    NativeTagSource::AuthoredTrack => NativeTagSource::DiscAndSidecar,
                };
            },
            (ExtractPanel::Tags, KeyCode::Char('a')) => state.tag_artwork = !state.tag_artwork,
            (ExtractPanel::Tags, KeyCode::Char(k @ '1'..='7')) => {
                let bit = u32::from(k) - u32::from('1');
                state.tag_fields ^= 1 << bit;
            },
            (ExtractPanel::Tags, KeyCode::Char('k')) => {
                for (i,selected) in state.selected.iter().enumerate() {
                    if *selected { state.keep_dst[i] = false; }
                }
            },
            (ExtractPanel::Destination, KeyCode::Char('e')) => {
                state.destination_input = state.destination.display().to_string();
                state.editing_destination = true;
                state.editing_template = false;
            }
            (ExtractPanel::Destination, KeyCode::Char('t')) => {
                state.destination_input = state.naming_template.clone();
                state.editing_destination = true;
                state.editing_template = true;
            }
            (ExtractPanel::Destination, KeyCode::Char('s')) => state.program_subfolders = !state.program_subfolders,
            (ExtractPanel::Destination, KeyCode::Char('l')) => state.extraction_log = !state.extraction_log,
            (ExtractPanel::Destination, KeyCode::Char('o')) => {
                use crate::convert::pipeline::native_extract::NativeOverwritePolicy;
                state.overwrite_policy = match state.overwrite_policy {
                    NativeOverwritePolicy::Fail => NativeOverwritePolicy::Skip,
                    NativeOverwritePolicy::Skip => NativeOverwritePolicy::Replace,
                    NativeOverwritePolicy::Replace => NativeOverwritePolicy::Fail,
                };
            },
            (ExtractPanel::Format, KeyCode::Char('k')) if state.contents.format == crate::disc::DiscFormat::Sacd => {
                cycle_native_option(state, 1);
            }
            (ExtractPanel::Format | ExtractPanel::Downmix | ExtractPanel::AssumeDecrypted, KeyCode::Char('[')) => cycle_native_option(state, -1),
            (ExtractPanel::Format | ExtractPanel::Downmix | ExtractPanel::AssumeDecrypted, KeyCode::Char(']')) => cycle_native_option(state, 1),
            (ExtractPanel::ConvertChoice, KeyCode::Enter) => { convert_overlay_cursor(app); }
            (ExtractPanel::ConvertChoice, KeyCode::Char('e')) => { queue_overlay_checked_programs(app); }
            (ExtractPanel::More, KeyCode::Char('g')) => state.cycle_dvda_group_preset(),
            (ExtractPanel::More, KeyCode::Char('h')) => { state.show_hidden = !state.show_hidden; }
            (ExtractPanel::More, KeyCode::Char('d')) => { state.panel = Some(ExtractPanel::Diagnostics); }
            (ExtractPanel::More, KeyCode::Char('c')) => { state.panel = Some(ExtractPanel::ConvertChoice); }
            (_, KeyCode::Down) => { state.panel_cursor = state.panel_cursor.saturating_add(1); }
            (_, KeyCode::Up) => { state.panel_cursor = state.panel_cursor.saturating_sub(1); }
            _ => {}
        }
        return;
    }

    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => { state.probe_cancel.cancel(); app.active_overlay = ActiveOverlay::None; }
        KeyCode::Tab => {
            state.focus = match (state.focus, key.modifiers.contains(KeyModifiers::SHIFT)) {
                (ExtractFocus::Programs, false) | (ExtractFocus::Output, true) => ExtractFocus::Items,
                (ExtractFocus::Items, false) | (ExtractFocus::Programs, true) => ExtractFocus::Output,
                (ExtractFocus::Output, false) | (ExtractFocus::Items, true) => ExtractFocus::Programs,
            };
        }
        // Left/right never change an output setting or move into Output.
        // Tab is the only way to select that third focus zone.
        KeyCode::Left if state.focus == ExtractFocus::Items => state.focus = ExtractFocus::Programs,
        KeyCode::Right if state.focus == ExtractFocus::Programs => state.focus = ExtractFocus::Items,
        KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
            let delta = if matches!(key.code, KeyCode::Up | KeyCode::Char('k')) {-1isize} else {1isize};
            match state.focus {
                ExtractFocus::Programs => {
                    let from = state.cursor;
                    state.move_cursor(delta);
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        for index in from.min(state.cursor)..=from.max(state.cursor) {
                            if state.is_extractable(index) { state.selected[index] = true; }
                        }
                    }
                    state.item_cursor = 0; state.item_scroll = 0;
                }
                ExtractFocus::Items => {
                    let from = state.item_cursor;
                    let last = state.selected_presentation().map_or(0,|p|p.tracks.len().saturating_sub(1));
                    state.item_cursor = if delta < 0 {state.item_cursor.saturating_sub(1)} else {state.item_cursor.saturating_add(1).min(last)};
                    if key.modifiers.contains(KeyModifiers::SHIFT) {state.check_item_range(from,state.item_cursor);}
                }
                ExtractFocus::Output => {
                    let last = state.output_rows().len().saturating_sub(1);
                    state.output_cursor = if delta < 0 {state.output_cursor.saturating_sub(1)} else {state.output_cursor.saturating_add(1).min(last)};
                }
            }
        }
        KeyCode::Char(' ') => match state.focus {
            ExtractFocus::Programs => state.toggle_selected(state.cursor),
            ExtractFocus::Items => state.toggle_item(state.item_cursor),
            ExtractFocus::Output => cycle_native_option(state, 1),
        },
        KeyCode::Char('a') | KeyCode::Char('n') | KeyCode::Char('i') => {
            if let KeyCode::Char(mode) = key.code { state.check_focus(mode); }
        }
        KeyCode::Char('h') => { state.show_hidden = !state.show_hidden; }
        KeyCode::Char('d') => { state.panel = Some(ExtractPanel::Diagnostics); }
        KeyCode::Char('m') => { state.panel = Some(ExtractPanel::More); }
        KeyCode::Char('c') => { activate_overlay_convert(app); }
        KeyCode::Char('[') => cycle_native_option(state, -1),
        KeyCode::Char(']') => cycle_native_option(state, 1),
        KeyCode::PageDown => match state.focus {
            ExtractFocus::Programs => state.move_cursor(10),
            ExtractFocus::Items => {let last = state.selected_presentation().map_or(0, |p|p.tracks.len().saturating_sub(1)); state.item_cursor = state.item_cursor.saturating_add(10).min(last);},
            ExtractFocus::Output => {state.output_cursor = state.output_cursor.saturating_add(2).min(state.output_rows().len().saturating_sub(1));},
        },
        KeyCode::PageUp => match state.focus {
            ExtractFocus::Programs => state.move_cursor(-10),
            ExtractFocus::Items => state.item_cursor = state.item_cursor.saturating_sub(10),
            ExtractFocus::Output => state.output_cursor = state.output_cursor.saturating_sub(2),
        },
        KeyCode::Enter => {
            if state.focus == ExtractFocus::Output {
                state.panel = Some(state.current_output_panel());
                state.panel_cursor = 0;
            } else if state.focus == ExtractFocus::Programs {
                state.focus = ExtractFocus::Items;
            } else {
                state.toggle_item(state.item_cursor);
            }
        }
        _ => {}
    }
}

/// Switch the Convert screen to a different disc presentation, rebuilding
/// the source mode and cascading format defaults. Does not change the
/// active screen or dismiss overlays.
pub fn switch_disc_presentation(
    app: &mut AppState,
    contents: DiscContents,
    presentation_index: usize,
) -> Result<(), String> {
    let presentation = contents.presentations.get(presentation_index);
    let mut metadata = metadata_for_disc_presentation(&contents, presentation);
    if metadata.album.is_none() && !contents.label.trim().is_empty() {
        metadata.album = Some(contents.label.clone());
    }

    app.convert.metadata.title = metadata.title.clone();
    app.convert.metadata.artist = metadata.artist.clone();
    app.convert.metadata.album = metadata.album.clone();
    app.convert.metadata.genre = metadata.genre.clone();
    app.convert.metadata.year = metadata.year.clone();

    let source = source_mode_for_presentation(contents, presentation_index, metadata)?;
    app.convert.set_source_mode(source);
    app.convert.apply_source_defaults();
    Ok(())
}

/// Load one presentation into the Convert screen using `SourceMode::MultiTrack`.
/// Load the scored default presentation into the Convert screen.
///
/// This is used by both synchronous context-menu handling and async probe
/// follow-up. It must not hard-code presentation 0 for DVD-Video, because the
/// mapper exposes presentations in authored order while the materializer's
/// automatic path scores the likely main program.
pub fn load_default_disc_presentation_for_convert(
    app: &mut AppState,
    contents: DiscContents,
) -> Result<(), String> {
    let index = select_default_disc_presentation_index(&contents)
        .ok_or_else(|| "Disc contains no selectable audio streams".to_string())?;
    load_disc_presentation_for_convert(app, contents, index)
}

pub fn load_disc_presentation_for_convert(
    app: &mut AppState,
    contents: DiscContents,
    presentation_index: usize,
) -> Result<(), String> {
    let presentation = contents
        .presentations
        .get(presentation_index)
        .ok_or_else(|| format!("No disc stream at index {}", presentation_index + 1))?
        .clone();
    let source_path = contents.source_path.clone();

    switch_disc_presentation(app, contents, presentation_index)?;

    app.current_screen = AppScreen::Convert;
    app.active_overlay = ActiveOverlay::None;
    app.browse.return_target = crate::tui::browse::BrowseReturnTarget::None;
    app.recent.record_use_with_db(&source_path, &app.db);
    app.set_status(format!("Loaded stream: {}", presentation.label));
    Ok(())
}

/// Convert respects the extraction window's checked programs; the cursor is
/// relevant only with no checked programs or as the multiple-choice override.
/// Selecting a program never changes Convert's existing codec/processing path.
pub fn activate_overlay_convert(app: &mut AppState) {
    let checked = match &app.active_overlay {
        ActiveOverlay::DiscBrowser(state) => state.selected_indices(),
        _ => return,
    };
    if checked.len() > 1 {
        if let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay {
            state.panel = Some(crate::tui::disc_browser::ExtractPanel::ConvertChoice);
        }
    } else {
        convert_overlay_selected(app, checked.first().copied());
    }
}

pub fn convert_overlay_cursor(app: &mut AppState) {
    let index = match &app.active_overlay {
        ActiveOverlay::DiscBrowser(state) => state.cursor,
        _ => return,
    };
    // The multiple-choice popup's highlighted program uses its current
    // selections when checked. A cursor outside the set converts all items.
    convert_overlay_selected(app, Some(index));
}

fn convert_overlay_selected(app: &mut AppState, explicit_index: Option<usize>) {
    let (contents, index, checked) = match &app.active_overlay {
        ActiveOverlay::DiscBrowser(state) => {
            let index = explicit_index.unwrap_or(state.cursor);
            let checked = if state.selected.get(index) == Some(&true) {
                state.items_checked.get(index).cloned()
            } else { None };
            (state.contents.clone(), index, checked)
        }
        _ => return,
    };
    if let Err(err) = require_selected_presentation_stream_conversion(&contents, index)
        .and_then(|()| load_disc_presentation_for_convert(app, contents, index))
    {
        app.set_status(format!("Disc stream load failed: {err}"));
        return;
    }
    if let (Some(checked), SourceMode::MultiTrack { selected, .. }) =
        (checked, &mut app.convert.source.mode)
    {
        if checked.len() == selected.len() && checked.iter().any(|value| *value) {
            *selected = checked;
        }
    }
}

/// Queue each checked program through the existing Convert queue's ready-item
/// admission. Requests are built before the first insertion so a broken
/// selection or invalid pipeline setting cannot queue only an early prefix.
fn queue_overlay_checked_programs(app: &mut AppState) {
    use crate::convert::{ConversionItem, FormatDetector};
    use crate::convert::pipeline::unified_request::{
        build_pipeline_request_from_settings, pipeline_settings_from_legacy_options,
    };
    use crate::tui::disc_browser::metadata_for_disc_presentation;

    let (contents, choices, source_path) = match &app.active_overlay {
        ActiveOverlay::DiscBrowser(state) => (
            state.contents.clone(),
            state.selected_indices().into_iter().map(|i| (i, state.items_checked[i].clone()))
                .collect::<Vec<_>>(),
            state.source_path.clone(),
        ),
        _ => return,
    };
    if choices.len() < 2 { activate_overlay_convert(app); return; }
    let options = super::convert_actions::pills_to_options(
        &app.convert.format, &app.convert.output_options, &app.config,
    );
    if options.output_dir.is_none() {
        app.set_status("Choose a Convert destination before queuing multiple programs");
        return;
    }
    let prepare = || -> Result<Vec<_>, String> {
        let format = FormatDetector::detect(&source_path).map_err(|e| e.to_string())?;
        let settings = match options.pipeline_settings.clone() {
            Some(settings) => settings,
            None => pipeline_settings_from_legacy_options(&options)
                .map_err(|e| e.to_string())?,
        };
        choices.iter().map(|(index, bitmap)| {
            require_selected_presentation_stream_conversion(&contents, *index)?;
            let metadata = metadata_for_disc_presentation(&contents, contents.presentations.get(*index));
            let mut mode = source_mode_for_presentation(contents.clone(), *index, metadata)?;
            if let SourceMode::MultiTrack { selected, .. } = &mut mode {
                if selected.len() != bitmap.len() || !bitmap.iter().any(|on| *on) {
                    return Err(format!("Program {} has no checked items", index + 1));
                }
                *selected = bitmap.clone();
            }
            let item = ConversionItem::new(source_path.clone(), format, options.clone());
            let request = build_pipeline_request_from_settings(&item, settings.clone())
                .map_err(|e| e.to_string())?;
            Ok((source_path.clone(), options.clone(),
                crate::tui::command::pipeline_request_with_convert_source_disc_selection(&mode, request)))
        }).collect()
    };
    let ready = match prepare() {
        Ok(ready) => ready,
        Err(err) => { app.set_status(format!("Cannot queue selected programs: {err}")); return; }
    };
    let total = ready.len();
    let mut enqueued = 0usize;
    for (path, options, request) in ready {
        match app.manager.add_file_ready_for_processing_with_pipeline_request(
            path, options, request, None,
        ) {
            Ok(_) => enqueued += 1,
            Err(err) => {
                app.set_status(format!("Queued {enqueued} of {total} programs; last error: {err}"));
                app.save_queue();
                return;
            }
        }
    }
    app.save_queue();
    app.active_overlay = ActiveOverlay::None;
    app.current_screen = AppScreen::Queue;
    app.set_status(format!("Queued {enqueued} independently selected disc program(s) for Convert"));
}

/// Reuse the existing format-specific metadata editor for the focused program.
/// All editors already support presentation tabs. Switch to the exact source
/// stream after they have loaded, instead of displaying an unrelated default.
fn open_existing_metadata_editor_for_disc_cursor(app: &mut AppState) -> Result<bool, String> {
    let (path, id, format) = match &app.active_overlay {
        ActiveOverlay::DiscBrowser(state) => {
            let Some(presentation) = state.selected_presentation() else { return Ok(false); };
            (state.source_path.clone(), presentation.id, state.contents.format)
        }
        _ => return Ok(false),
    };
    match format {
        crate::disc::DiscFormat::DvdVideo => return open_dvdv_metadata_editor_for_overlay_cursor(app),
        crate::disc::DiscFormat::Sacd => super::keybindings::open_metadata_editor_for_sacd(app, path),
        crate::disc::DiscFormat::DvdAudio => {
            if let PresentationId::DvdAudioGroup(group) = id {
                super::keybindings::open_metadata_editor_for_dvda_group(app, path, group);
            } else { return Ok(false); }
        }
        crate::disc::DiscFormat::BluRay => super::keybindings::open_metadata_editor_for_bluray(app, path),
    }
    if let ActiveOverlay::MetadataEditor(editor) = &mut app.active_overlay {
        if let Some(index) = editor.presentation_tabs.iter().position(|tab|tab.id == id) {
            editor.switch_presentation_tab(index);
        }
    }
    Ok(true)
}

/// Open the metadata editor for the currently highlighted DVD-Video stream and
/// preload the sidecar entry for that exact presentation. This is the safe
/// keybinding target for `:tags-mb`-style disc-browser activation because it
/// carries the selected `PresentationId` through to the editor preload path.
pub fn open_dvdv_metadata_editor_for_overlay_cursor(app: &mut AppState) -> Result<bool, String> {
    let (source_path, selected_presentation_id) = match &app.active_overlay {
        ActiveOverlay::DiscBrowser(state) => {
            let Some(presentation) = state.selected_presentation() else {
                return Ok(false);
            };
            if !matches!(presentation.id, PresentationId::DvdVideoTitle { .. }) {
                return Ok(false);
            }
            (state.source_path.clone(), presentation.id.clone())
        }
        _ => return Ok(false),
    };

    crate::tui::command::open_metadata_editor_for_dvdv_with_sidecar_preload(
        app,
        source_path,
        Some(selected_presentation_id),
    )
}

/// Trigger the async disc probe after the browse cursor/selection changes.
///
/// This is the cursor-move hook required by Phase 4c: when the highlighted row
/// becomes a SACD ISO, DVD-Audio source, DVD-Video source, or Blu-ray source,
/// the expensive disc parse starts on the async `ensure_disc_probe()` path so the info
/// pane can render from cache on the next redraw. This function deliberately
/// does not force a retry for current cached errors; users use Analyze for an
/// explicit re-probe, while changed source metadata automatically misses the
/// fingerprinted cache and schedules fresh work.
pub fn probe_selected_disc_after_cursor_move(
    app: &mut AppState,
    tx: &mpsc::Sender<AppMessage>,
) {
    let Some((path, is_disc_source)) = app
        .browse
        .selected_entry()
        .map(|entry| (entry.path.clone(), entry.is_disc_source()))
    else {
        return;
    };

    if is_disc_source {
        ensure_disc_probe(app, path, tx);
    }
}

/// Ensure a current probe result exists or is being produced.
///
/// Returns `true` when the cache already contains current contents. Current
/// cached errors are not retried automatically, so simply moving the cursor over
/// a bad disc cannot start an infinite probe loop. When the source metadata has
/// changed, success and error entries both miss, are evicted, and a fresh probe
/// is scheduled. Use `force_disc_reprobe()` for an explicit retry with unchanged
/// metadata.
pub fn ensure_disc_probe(
    app: &mut AppState,
    path: PathBuf,
    tx: &mpsc::Sender<AppMessage>,
) -> bool {
    if cached_disc_contents(app, &path).is_some() {
        return true;
    }

    if let Some(error) = cached_disc_error(app, &path) {
        app.set_status(format!(
            "Disc analysis failed for current source; use Analyze to retry: {error}"
        ));
        return false;
    }

    app.browse.disc_probe_cache.remove(&path);
    request_disc_probe(app, path, tx);
    false
}

/// Explicitly bypass any current success or error entry and schedule a new
/// probe. Wire this to the Browse info-pane Analyze action and any future
/// command palette action named re-probe/re-analyze.
pub fn force_disc_reprobe(
    app: &mut AppState,
    path: PathBuf,
    tx: &mpsc::Sender<AppMessage>,
) {
    app.browse.disc_probe_cache.remove(&path);
    request_disc_probe(app, path, tx);
}

/// Start async probing if this path is not already in flight.
pub fn request_disc_probe(app: &mut AppState, path: PathBuf, tx: &mpsc::Sender<AppMessage>) {
    if app.browse.disc_probe_pending.insert(path.clone()) {
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        crate::tui::disc_browser::spawn_disc_probe(path, cancel, tx.clone());
        app.set_status("Analyzing disc streams...");
    } else {
        app.set_status("Disc analysis already in progress...");
    }
}

pub fn cached_disc_contents(app: &AppState, path: &Path) -> Option<Arc<DiscContents>> {
    app.browse
        .disc_probe_cache
        .get(path)
        .and_then(|entry| entry.contents_if_current(path))
}

pub fn cached_disc_error<'a>(app: &'a AppState, path: &Path) -> Option<&'a str> {
    app.browse
        .disc_probe_cache
        .get(path)
        .and_then(|entry| entry.error_if_current(path))
}

/// Extract the selected presentation id from the Convert source, if present.
pub fn selected_presentation_id(mode: &SourceMode) -> Option<&PresentationId> {
    match mode {
        SourceMode::MultiTrack {
            selected_presentation_id: Some(id),
            ..
        } => Some(id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TonepoetConfig;
    use crate::convert::classify::EntryKind;
    use crate::disc::model::{
        AudioPresentationFormat, CopyProtectionSummary, DiscFormat, DiscPresentation,
        DiscTrack, FormatProvenance, PresentationId,
    };
    use crate::disc::SacdAreaId;
    use crate::tui::browse::{
        BrowseEntry, FolderAudioSummary, FolderContentClassification, FolderClassificationKind,
        FolderDiscMarkerKind, FolderUnitSummary, ProbeCacheIdentity,
    };
    use crate::tui::disc_browser::{disc_probe_fingerprint, DiscProbeCacheEntry};
    use std::fs::File;
    use std::io::{Seek, SeekFrom, Write};
    use std::path::{Path, PathBuf};

    fn write_synthetic_sacd_iso(path: &Path) {
        let mut file = File::create(path).expect("create synthetic SACD ISO");
        let offset = 510_u64 * 2048;
        file.set_len(offset + 8).expect("size synthetic SACD ISO");
        file.seek(SeekFrom::Start(offset)).expect("seek SACD TOC");
        file.write_all(b"SACDMTOC").expect("write SACD magic");
    }

    fn app_with_selected_archive_iso(path: PathBuf) -> AppState {
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.entries = vec![BrowseEntry::new(
            path,
            "disc.iso".to_string(),
            EntryKind::Archive,
            0,
            None,
        )];
        app.browse.selected_index = 0;
        app
    }

    fn selectable_disc_contents(
        source_path: PathBuf,
        marker: FolderDiscMarkerKind,
    ) -> DiscContents {
        let (format, id, codec) = match marker {
            FolderDiscMarkerKind::DvdAudio => (
                DiscFormat::DvdAudio,
                PresentationId::DvdAudioGroup(1),
                "MLP",
            ),
            FolderDiscMarkerKind::DvdVideo => (
                DiscFormat::DvdVideo,
                PresentationId::dvd_video(1, 1, 0),
                "LPCM",
            ),
            FolderDiscMarkerKind::Sacd => (
                DiscFormat::Sacd,
                PresentationId::SacdArea(SacdAreaId::Stereo),
                "DSD",
            ),
            FolderDiscMarkerKind::BluRay | FolderDiscMarkerKind::Iso => (
                DiscFormat::BluRay,
                PresentationId::try_blu_ray_title(1, 0x1101, 0, 1)
                    .expect("valid Blu-ray presentation id"),
                "LPCM",
            ),
        };
        DiscContents {
            format,
            label: marker.label().to_string(),
            source_path,
            presentations: vec![DiscPresentation {
                extract: crate::disc::model::PresentationExtractInfo::default(),
                id,
                label: "stream 1".to_string(),
                format: AudioPresentationFormat {
                    codec: Some(codec.to_string()),
                    sample_rate: Some(96_000),
                    bit_depth: Some(24),
                    channels: Some(2),
                    channel_layout: Some("stereo".to_string()),
                    lossless: true,
                    provenance: FormatProvenance::IfoAttributes,
                },
                tracks: vec![DiscTrack {
                    number: 1,
                    title: None,
                    performer: None,
                    duration_secs: Some(60.0),
                    format_note: None,
                }],
                total_duration_secs: 60.0,
                album_title: None,
                album_artist: None,
                genre: None,
                year: None,
            }],
            suppressed: Vec::new(),
            copy_protection: CopyProtectionSummary {
                description: "none".to_string(),
            },
            diagnostics: Vec::new(),
            album_title: None,
            album_artist: None,
            genre: None,
            year: None,
        }
    }

    fn app_with_selected_classified_disc_folder(
        root: &Path,
        nested_iso: &Path,
        marker: FolderDiscMarkerKind,
    ) -> AppState {
        let metadata = std::fs::metadata(root).expect("folder metadata");
        let entry = BrowseEntry::new(
            root.to_path_buf(),
            "disc folder".to_string(),
            EntryKind::Directory,
            metadata.len(),
            metadata.modified().ok(),
        );
        let identity = ProbeCacheIdentity::from_entry(&entry);
        let classification = FolderContentClassification {
            kind: FolderClassificationKind::Disc,
            identity,
            audio: FolderAudioSummary::default(),
            units: vec![FolderUnitSummary {
                path: nested_iso.to_path_buf(),
                parent: root.to_path_buf(),
                name: nested_iso
                    .file_name()
                    .expect("fixture file name")
                    .to_string_lossy()
                    .into_owned(),
                disc_marker: Some(marker),
                audio: FolderAudioSummary::default(),
            }],
            unit_count: 1,
            collection_many: false,
            io_budget_exhausted: false,
            disc_marker: Some(marker),
            embedded_cue_availability: crate::tui::probe::EmbeddedCueAvailability::Unknown,
            cue_import_availability: crate::tui::probe::CueImportAvailability::Unknown,
            cue_repair_availability: crate::tui::browse::CueRepairAvailability::Unknown,
            cue_file_reference_repairs: std::collections::BTreeMap::new(),
        };

        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.current_screen = AppScreen::Browse;
        app.browse.entries = vec![entry];
        app.browse.selected_index = 0;
        app.browse.insert_folder_classification_for_identity(
            root.to_path_buf(),
            identity,
            classification,
        );
        let fingerprint = disc_probe_fingerprint(nested_iso).expect("disc fingerprint");
        app.browse.disc_probe_cache.insert(
            nested_iso.to_path_buf(),
            DiscProbeCacheEntry::from_success(
                fingerprint,
                selectable_disc_contents(nested_iso.to_path_buf(), marker),
            ),
        );
        app
    }

    #[test]
    fn explicit_disc_action_accepts_unprobed_archive_iso_without_reducer_probe() {
        let temp = tempfile::tempdir().expect("tempdir");
        let iso = temp.path().join("disc.iso");
        write_synthetic_sacd_iso(&iso);
        let mut app = app_with_selected_archive_iso(iso.clone());

        let selected = selected_entry_effective_disc_path(&mut app).expect("disc path");

        assert_eq!(selected, iso);
        // No ISO header probe runs on the reducer: the row keeps its lazy
        // Archive kind until the async disc probe reclassifies it.
        assert!(matches!(app.browse.entries[0].kind, EntryKind::Archive));
    }

    #[test]
    fn convert_custom_on_unprobed_archive_iso_falls_through_to_generic_handler() {
        let temp = tempfile::tempdir().expect("tempdir");
        let iso = temp.path().join("disc.iso");
        write_synthetic_sacd_iso(&iso);
        let mut app = app_with_selected_archive_iso(iso);
        let (tx, _rx) = tokio::sync::mpsc::channel(4);

        let consumed = handle_disc_context_action(
            &mut app,
            &crate::tui::context_menu::ContextAction::ConvertCustom,
            &tx,
        );

        assert!(
            !consumed,
            "ConvertCustom on an unprobed Archive .iso must fall through to the generic archive handler"
        );
        assert!(matches!(app.browse.entries[0].kind, EntryKind::Archive));
    }

    #[test]
    fn explicit_disc_action_rejects_non_iso_archive_selection() {
        let temp = tempfile::tempdir().expect("tempdir");
        let archive = temp.path().join("data.7z");
        std::fs::write(&archive, b"not a disc image").expect("write generic archive");
        let mut app = app_with_selected_archive_iso(archive);

        assert!(selected_entry_effective_disc_path(&mut app).is_none());
        assert!(matches!(app.browse.entries[0].kind, EntryKind::Archive));
    }

    #[test]
    fn classified_folders_activate_nested_iso_audio_streams_for_all_disc_markers() {
        let marker_cases = [
            (FolderDiscMarkerKind::DvdAudio, "dvda.iso"),
            (FolderDiscMarkerKind::DvdVideo, "dvdv.iso"),
            (FolderDiscMarkerKind::Sacd, "sacd.iso"),
            (FolderDiscMarkerKind::BluRay, "bluray.iso"),
        ];

        for (marker, file_name) in marker_cases {
            let temp = tempfile::tempdir().expect("tempdir");
            let root = temp.path().join("album");
            std::fs::create_dir(&root).expect("disc folder");
            let nested_iso = root.join(file_name);
            std::fs::write(&nested_iso, b"disc image fixture").expect("disc image");
            let mut app = app_with_selected_classified_disc_folder(&root, &nested_iso, marker);
            let (tx, _rx) = tokio::sync::mpsc::channel(4);

            assert_eq!(
                selected_entry_effective_disc_path(&mut app).as_deref(),
                Some(nested_iso.as_path()),
                "{marker:?} folder must resolve to its nested ISO",
            );

            open_selected_disc_browser(&mut app, &tx);

            assert!(
                matches!(app.active_overlay, ActiveOverlay::DiscBrowser(_)),
                "{marker:?} folder-level activation must open the audio-streams overlay",
            );
        }
    }
}
