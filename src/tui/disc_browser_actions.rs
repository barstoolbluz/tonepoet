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

    let mut state = DiscBrowserState::new(contents, source_path.clone());
    // Match Convert's user preference rather than defaulting off for every
    // extraction. A per-dialogue toggle never writes to the preference.
    state.extraction_log = app.config.conversion.write_log_file;
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

/// Honor configured scratch, otherwise stage in TonePoet's private cache.
/// The target volume may differ; the worker's publisher handles that safely.
pub fn native_working_root(
    _state: &DiscBrowserState,
    configured: Option<&Path>,
) -> PathBuf {
    configured.map(Path::to_path_buf).unwrap_or_else(
        crate::convert::pipeline::native_extract::default_native_working_root)
}

/// Transfer ownership of the extraction dialog to the existing directory
/// picker. Cancel and invalid selections restore the exact original state.
fn open_native_destination_picker(app: &mut AppState) {
    use crate::tui::app::{FilePickerPurpose, MetadataFilePickerState};
    let overlay = std::mem::replace(&mut app.active_overlay, ActiveOverlay::None);
    let ActiveOverlay::DiscBrowser(mut state) = overlay else {
        app.active_overlay = overlay;
        return;
    };
    cancel_native_text_edit(&mut state);
    state.focus = crate::tui::disc_browser::ExtractFocus::Output;
    state.output_cursor = state.output_rows().iter().position(|row| *row == "Folder").unwrap_or(0);
    let start_dir = state.destination.ancestors().find(|path| path.is_dir())
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let purpose = FilePickerPurpose::NativeExtractDestination { probe_id: state.probe_id };
    let picker = tui_file_picker::FilePickerState::new(
        super::keybindings::file_picker_config_with_browse_sort(app, tui_file_picker::FilePickerConfig {
            start_dir,
            filter: tui_file_picker::FilePickerFilter::All,
            title: "Select extraction destination".to_owned(),
            theme: super::keybindings::file_picker_theme_from_theme(&app.theme),
            selection_mode: tui_file_picker::FilePickerSelectionMode::Directories,
            operation_policy: tui_file_picker::FileOperationPolicy::selection_only(
                app.file_task_verbose_degrade_notices),
            ..tui_file_picker::FilePickerConfig::default()
        }),
    );
    app.pending_disc_extract_picker = Some(state);
    app.active_overlay = ActiveOverlay::FilePicker(MetadataFilePickerState::new(purpose, picker));
}

/// Native Naming opens the same saved-template picker used by Convert. The
/// calling extract form is parked in the popup and restored on every exit.
fn open_native_naming_picker(app: &mut AppState) {
    use crate::tui::app::TemplateTarget;
    let overlay=std::mem::replace(&mut app.active_overlay,ActiveOverlay::None);
    let ActiveOverlay::DiscBrowser(mut state)=overlay else {
        app.active_overlay=overlay;
        return;
    };
    cancel_native_text_edit(&mut state);
    focus_native_row(&mut state,"Naming");
    let templates=super::template_builder::list_templates(TemplateTarget::Filename);
    let preview=templates.first().map(|template|
        super::template_builder::render_template_preview(template)).unwrap_or_default();
    let active_template=Some(state.naming_template.clone());
    app.active_overlay=ActiveOverlay::TemplatePicker {
        target:TemplateTarget::Filename,templates,selected:0,scroll:0,preview,
        active_template,return_to_extract:Some(state),
    };
}

fn open_native_naming_builder(app: &mut AppState) {
    use crate::tui::app::{TemplateBuilderFocus,TemplateBuilderState,TemplateTarget};
    let overlay=std::mem::replace(&mut app.active_overlay,ActiveOverlay::None);
    let ActiveOverlay::DiscBrowser(mut state)=overlay else {
        app.active_overlay=overlay;
        return;
    };
    cancel_native_text_edit(&mut state);
    focus_native_row(&mut state,"Naming");
    let mut builder=TemplateBuilderState::new(TemplateTarget::Filename,
        &state.naming_template,TemplateBuilderFocus::TemplateInput);
    builder.disc_format=Some(state.contents.format);
    builder.return_to_extract=Some(state);
    app.active_overlay=ActiveOverlay::TemplateBuilder(Box::new(builder));
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
    if let Err(error) = crate::convert::pipeline::native_extract::validate_job(&job) {
        app.set_status(format!("Cannot extract: {error}"));
        return;
    }
    let probe_cancel = state.probe_cancel.clone();
    if let Err(error) = super::native_extract_task::launch(app, job, tx) {
        app.set_status(format!("Cannot start native extraction: {error}"));
        return;
    }
    probe_cancel.cancel();
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
    // Tags is the one interactive sheet. Its own hitboxes always take
    // precedence over the extraction window below it. Other panels remain
    // modal but read-only, as before.
    if let ActiveOverlay::DiscBrowser(state) = &app.active_overlay {
        if state.panel == Some(crate::tui::disc_browser::ExtractPanel::Tags) {
            // An open source dropdown owns its interaction until a choice is
            // made or it is dismissed; a click behind it never edits fields.
            if state.tag_source_open {
                match button {
                    TuiButton::DiscBrowserTagSheetSourceChoice(index) => {
                        if let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay {
                            set_native_tag_source(state, *index);
                        }
                    }
                    TuiButton::DiscBrowserTagSheetOutside => {
                        if let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay {
                            state.tag_source_open = false;
                            state.panel = None;
                        }
                    }
                    _ => {
                        if let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay {
                            state.tag_source_open = false;
                        }
                    }
                }
                return true;
            }
            match button {
                TuiButton::DiscBrowserTagSheetControl(index) => {
                    let control=state.tag_sheet_controls().get(*index).copied();
                    if let Some(control)=control {
                        if native_tag_external_action(app,control,tx) {return true;}
                        if let ActiveOverlay::DiscBrowser(state)=&mut app.active_overlay {
                            state.panel_cursor=*index;
                            activate_native_tag_control(state,control);
                        }
                    }
                }
                TuiButton::DiscBrowserTagSheetSourceChoice(index) => {
                    if let ActiveOverlay::DiscBrowser(state)=&mut app.active_overlay {
                        if state.tag_source_open {
                            set_native_tag_source(state,*index);
                        }
                    }
                }
                TuiButton::DiscBrowserTagSheetOutside => {
                    if let ActiveOverlay::DiscBrowser(state)=&mut app.active_overlay {
                        state.panel=None;
                        state.tag_source_open=false;
                    }
                }
                // Sheet whitespace and stale underlying hitboxes cannot
                // activate or dismiss unrelated commands.
                _ => {}
            }
            return true;
        }
        if state.panel.is_some() {return true;}
    }
    // Hints are actions of the currently active control. Dispatch them before
    // the generic focus-change cleanup (or the open-dropdown click handler),
    // so Done validates the draft and Undo is the only hint that discards it.
    if let TuiButton::DiscBrowserHint(index) = button {
        let key = match &app.active_overlay {
            ActiveOverlay::DiscBrowser(state) => native_hint_key(state, *index),
            _ => None,
        };
        if let Some(key) = key { handle_disc_browser_key(app, key, tx); }
        return true;
    }
    // A pending path or naming draft must not be discarded by the Extract
    // button: the job would otherwise use the prior destination or template.
    // Require an explicit Enter (accept) or Esc (discard), just as Ctrl+X does.
    if matches!(button, TuiButton::DiscBrowserExtract) {
        if let ActiveOverlay::DiscBrowser(state) = &app.active_overlay {
            if state.editing_destination {
                let field = if state.editing_template { "Naming" } else { "Folder" };
                app.set_status(format!(
                    "Finish editing {field} before extracting: Enter to accept or Esc to discard"
                ));
                return true;
            }
        }
    }
    if let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay {
        if state.overwrite_open {
            match button {
                TuiButton::DiscBrowserOverwriteChoice(choice) if *choice < 3 => {
                    set_native_overwrite(state, *choice);
                    state.overwrite_open = false;
                }
                TuiButton::DiscBrowserOverwrite => state.overwrite_open = false,
                _ => state.overwrite_open = false,
            }
            return true;
        }
        // The mouse can change form focus independently of keyboard input.
        // A transient editor must never retain keyboard ownership afterward.
        let on_active_field = matches!(button, TuiButton::DiscBrowserFolderField)
            && state.editing_destination && !state.editing_template
            || matches!(button, TuiButton::DiscBrowserNaming)
            && state.editing_destination && state.editing_template;
        if !on_active_field { cancel_native_text_edit(state); }
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
        TuiButton::DiscBrowserHiddenToggle => {
            if let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay {
                state.focus = crate::tui::disc_browser::ExtractFocus::Programs;
                state.show_hidden = !state.show_hidden;
            }
            true
        }
        TuiButton::DiscBrowserTitleSize => {
            if let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay {
                state.maximized = !state.maximized;
            }
            true
        }
        TuiButton::DiscBrowserTitle => {
            if click_count >= 2 {
                if let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay {
                    state.maximized = !state.maximized;
                }
            }
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
            state.normalize_output_cursor();
            true
        }
        TuiButton::DiscBrowserFormatChoice(choice) => {
            let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay else { return false; };
            focus_native_row(state, "Format");
            set_native_format(state, *choice);
            true
        }
        TuiButton::DiscBrowserFilesChoice(choice) => {
            let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay else { return false; };
            focus_native_row(state, "Files");
            set_native_files(state, *choice);
            true
        }
        TuiButton::DiscBrowserOverwrite => {
            let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay else { return false; };
            focus_native_row(state, "Overwrite");
            open_native_overwrite(state);
            true
        }
        TuiButton::DiscBrowserOverwriteChoice(choice) => {
            let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay else { return false; };
            focus_native_row(state, "Overwrite");
            set_native_overwrite(state, *choice);
            state.overwrite_open = false;
            true
        }
        TuiButton::DiscBrowserToggle(index) => {
            let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay else { return false; };
            state.focus = crate::tui::disc_browser::ExtractFocus::Output;
            state.output_cursor = *index;
            if !state.output_row_focusable(*index) { state.normalize_output_cursor(); return true; }
            toggle_native_form_row(state);
            true
        }
        TuiButton::DiscBrowserTagsEdit => {
            let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay else { return false; };
            focus_native_row(state, "Tags");
            open_native_tags_sheet(state);
            true
        }
        TuiButton::DiscBrowserFolderField => {
            let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay else { return false; };
            focus_native_row(state, "Folder");
            if !state.editing_destination { begin_native_folder_edit(state); }
            true
        }
        TuiButton::DiscBrowserFolderBrowse => {
            open_native_destination_picker(app);
            true
        }
        TuiButton::DiscBrowserNaming => {
            let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay else { return false; };
            focus_native_row(state, "Naming");
            if !state.editing_destination { begin_native_template_edit(state); }
            true
        }
        TuiButton::DiscBrowserNamingLoad => {
            open_native_naming_picker(app);
            true
        }
        TuiButton::DiscBrowserNamingCustom => {
            open_native_naming_builder(app);
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

// Both native fields use the same transient editor and keep their original
// values until Enter. Esc discards changes without any filesystem mutation.
fn begin_native_folder_edit(state: &mut DiscBrowserState) {
    state.destination_input = state.destination.to_string_lossy().into_owned();
    state.text_edit = Some(super::text_input::TextInputState::new_selected(
        state.destination_input.clone()));
    state.destination_caret = state.destination_input.chars().count();
    state.editing_destination = true;
    state.editing_template = false;
    state.text_mouse_origin = None;
    state.text_mouse_drag_started = false;
}

pub(super) fn cancel_native_text_edit(state: &mut DiscBrowserState) {
    if !state.editing_destination { return; }
    state.editing_destination = false;
    state.editing_template = false;
    state.destination_input.clear();
    state.destination_caret = 0;
    state.text_edit = None;
    state.text_mouse_origin = None;
    state.text_mouse_drag_started = false;
    state.refresh_destination_feedback();
}

fn native_edit_byte_index(text: &str, char_index: usize) -> usize {
    text.char_indices().nth(char_index).map_or(text.len(), |(index, _)| index)
}

/// Project the shared editor into legacy field snapshots. All mutations run
/// through TextInputState; the projections preserve old call sites and tests.
fn sync_native_text_projection(state: &mut DiscBrowserState) {
    if let Some(input) = state.text_edit.as_ref() {
        state.destination_input = input.text.clone();
        state.destination_caret = input.text[..input.cursor].chars().count();
    }
}

/// Older host call sites still provide the saved draft and a Unicode-scalar
/// caret. Accept those values only when they differ from the active shared
/// editor (normally only fixture setup uses this route).
fn receive_native_text_projection(state: &mut DiscBrowserState) {
    if let Some(input) = state.text_edit.as_mut() {
        let cursor = native_edit_byte_index(&state.destination_input,state.destination_caret);
        if input.text != state.destination_input || input.cursor != cursor {
            input.set_text_and_cursor(state.destination_input.clone(),cursor);
        }
    }
}

/// Start a pointer selection using the shared input's UTF-8/display-cell
/// mapping. The first click to an unfocused field selects all; a click on
/// an already-focused field positions the caret. Double click selects a word.
pub(super) fn native_text_mouse_down(state:&mut DiscBrowserState, width:usize,
    column:usize, position_caret:bool, click_count:u8, preview_caret:Option<usize>) {
    if !state.editing_destination {return;}
    receive_native_text_projection(state);
    if let Some(input)=state.text_edit.as_mut() {
        if state.text_mouse_origin.is_some() {
            input.end_mouse_selection(); // close a previous incomplete gesture
        }
        if click_count>=2 {
            input.select_word_at_view_column(width,column);
        } else if let Some(caret)=preview_caret {
            // Preserve the read-only field's hit, even on an unfocused first
            // click. Reveal that portion of the full editor at the same cell.
            // A click alone must still select all; a drag uses this same
            // original caret and pinned window as its selection anchor.
            let keep_select_all=input.select_all && !position_caret;
            input.cursor=native_edit_byte_index(&input.text,caret);
            input.begin_mouse_selection_at_cursor(width,column);
            if keep_select_all {
                input.select_all=true;
                input.selection_anchor=Some(0);
            }
        } else if position_caret {
            input.begin_mouse_selection(width,column);
        } // first activation without a preview keeps select-all from new_selected()
    }
    state.text_mouse_origin=Some((width,column));
    state.text_mouse_drag_started=false;
    sync_native_text_projection(state);
}

pub(super) fn native_text_mouse_drag(state:&mut DiscBrowserState,
    column:usize, released:bool) {
    let Some((width,anchor_col))=state.text_mouse_origin else{return;};
    if !state.editing_destination {return;}
    if let Some(input)=state.text_edit.as_mut() {
        if !released {
            if !state.text_mouse_drag_started {
                // A first unfocused click kept select-all. Turn it into a
                // selection anchored at the preview's original mouse-down
                // caret without remapping against the newly opened editor.
                if input.select_all {
                    input.select_all=false;
                    input.selection_anchor=Some(input.cursor);
                } else if input.selection_anchor != Some(input.cursor) {
                    input.begin_mouse_selection(width,anchor_col);
                }
                state.text_mouse_drag_started=true;
            }
            input.drag_mouse_selection(width,column);
        } else if state.text_mouse_drag_started {
            input.drag_mouse_selection(width,column);
            input.end_mouse_selection();
        } else if !input.select_all {
            // A first unfocused click still owns a whole-field selection.
            input.end_mouse_selection();
        }
    }
    if released {
        state.text_mouse_origin=None;
        state.text_mouse_drag_started=false;
    }
    sync_native_text_projection(state);
}

fn expand_native_home(text: &str) -> PathBuf {
    if text == "~" || text.starts_with("~/") || text.starts_with("~\\") {
        if let Some(home) = std::env::var_os("HOME").or_else(||std::env::var_os("USERPROFILE")) {
            let suffix = text.trim_start_matches('~').trim_start_matches(|c| c == '/' || c == '\\');
            return PathBuf::from(home).join(suffix);
        }
    }
    PathBuf::from(text)
}

/// Tab completes a unique directory segment on demand, never during paint.
/// Ambiguous prefixes are deliberately left untouched.
fn complete_native_folder_segment(input: &mut String) {
    let expanded = expand_native_home(input);
    let Some(parent) = expanded.parent() else { return; };
    let prefix = expanded.file_name().unwrap_or_default().to_string_lossy();
    let Ok(entries) = std::fs::read_dir(parent) else { return; };
    let mut found = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(prefix.as_ref()) && entry.path().is_dir() {
            if found.is_some() { return; }
            found = Some(name);
        }
    }
    if let Some(name) = found {
        let joined = parent.join(name);
        if input.starts_with('~') {
            if let Some(home) = std::env::var_os("HOME").or_else(||std::env::var_os("USERPROFILE")) {
                if let Ok(tail) = joined.strip_prefix(home) {
                    *input = format!("~/{}", tail.display());
                    return;
                }
            }
        }
        *input = joined.to_string_lossy().into_owned();
    }
}

fn focus_native_row(state: &mut DiscBrowserState, name: &str) {
    if state.editing_destination && (if state.editing_template { name != "Naming" } else { name != "Folder" }) {
        cancel_native_text_edit(state);
    }
    state.focus = crate::tui::disc_browser::ExtractFocus::Output;
    state.output_cursor = state.output_rows().iter().position(|row| *row == name).unwrap_or(0);
    state.normalize_output_cursor();
    if matches!(name, "Overwrite" | "Keep DST" | "Tags" | "Log") { state.form_column = 1; }
    else if matches!(name, "Format" | "Files" | "CUE") { state.form_column = 0; }
}

/// Logical coordinates match the form's fixed wide geometry; full-width
/// fields preserve the last column when moving back up to paired controls.
fn native_form_position(name: &str, has_keep_dst: bool) -> (usize, Option<usize>) {
    let extra=usize::from(has_keep_dst);
    match name {
        "Format" => (0,Some(0)), "Overwrite" => (0,Some(1)),
        "Keep DST" => (1,Some(1)),
        "Files" => (1+extra,Some(0)), "Tags" => (1+extra,Some(1)),
        "CUE" => (2+extra,Some(0)), "Log" => (2+extra,Some(1)),
        "Folder" => (3+extra,None), "Naming" => (4+extra,None),
        "Downmix" => (5+extra,None), "Assume decrypted" => (6+extra,None),
        _ => (0,None),
    }
}

fn native_form_has_neighbor(state: &DiscBrowserState, step: i8) -> bool {
    if !state.form_wide.get() { return false; }
    let has_keep_dst=state.output_rows().contains(&"Keep DST");
    let (row, column) = native_form_position(state.current_output_row(),has_keep_dst);
    let wanted = if step < 0 { Some(0) } else { Some(1) };
    if column == wanted { return false; }
    state.output_rows().iter().enumerate().any(|(index, name)| {
        let (other_row, other_column) = native_form_position(name,has_keep_dst);
        other_row == row && other_column == wanted && state.output_row_focusable(index)
    })
}

fn move_native_form_cursor(state: &mut DiscBrowserState, dx: i8, dy: i8) {
    let rows = state.output_rows();
    if rows.is_empty() { return; }
    state.normalize_output_cursor();
    if !state.form_wide.get() {
        // A stacked form has no horizontal neighbors. Do not turn Left/Right
        // into vertical movement simply because it has one logical column.
        if dx != 0 || dy == 0 { return; }
        let mut next = state.output_cursor as isize + isize::from(dy);
        while next >= 0 && (next as usize) < rows.len() {
            if state.output_row_focusable(next as usize) {
                state.output_cursor = next as usize;
                break;
            }
            next += isize::from(dy);
        }
        return;
    }
    let has_keep_dst=rows.contains(&"Keep DST");
    let (visual_row, column) = native_form_position(state.current_output_row(),has_keep_dst);
    if let Some(column) = column { state.form_column = column; }
    let target = if dx != 0 {
        let wanted = if dx < 0 {Some(0)} else {Some(1)};
        rows.iter().enumerate().position(|(index, name)| {
            let (row, col) = native_form_position(name,has_keep_dst);
            row == visual_row && col == wanted && state.output_row_focusable(index)
        })
    } else {
        let mut visual = visual_row as i32 + i32::from(dy);
        let mut found = None;
        while (0..8).contains(&visual) {
            found = rows.iter().enumerate().position(|(index, name)| {
                let (row, col) = native_form_position(name,has_keep_dst);
                row == visual as usize && (col.is_none() || col == Some(state.form_column))
                    && state.output_row_focusable(index)
            });
            if found.is_some() { break; }
            visual += i32::from(dy);
        }
        found
    };
    if let Some(index) = target { state.output_cursor = index; }
}

fn set_native_files(state: &mut DiscBrowserState, choice: usize) {
    if choice > 1 { return; }
    state.file_per_program = choice == 1;
    if !state.file_per_program || state.contents.format == crate::disc::DiscFormat::Sacd {
        state.cue = false;
    }
}

fn set_native_format(state: &mut DiscBrowserState, choice: usize) {
    use crate::convert::pipeline::native_extract::NativeWavContainer as W;
    if state.contents.format == crate::disc::DiscFormat::Sacd {
        if choice <= 1 {
            state.sacd_dff = choice == 1;
            if choice == 0 {
                // Selecting DSF explicitly also clears DST/DFF exceptions.
                // The separate Keep DST control can re-enable them afterwards.
                for (i, keep) in state.keep_dst.iter_mut().enumerate() {
                    if state.selected.get(i) == Some(&true) { *keep = false; }
                }
            }
        }
    } else if let Some(option) = [W::Automatic, W::Riff, W::Rf64, W::W64].get(choice) {
        state.wav_container = *option;
    }
}

fn native_overwrite_index(state: &DiscBrowserState) -> usize {
    use crate::convert::pipeline::native_extract::NativeOverwritePolicy as P;
    match state.overwrite_policy { P::Fail => 0, P::Skip => 1, P::Replace => 2 }
}

fn set_native_overwrite(state: &mut DiscBrowserState, choice: usize) {
    use crate::convert::pipeline::native_extract::NativeOverwritePolicy as P;
    if let Some(policy) = [P::Fail, P::Skip, P::Replace].get(choice) {
        state.overwrite_policy = *policy;
    }
}

fn open_native_overwrite(state: &mut DiscBrowserState) {
    state.overwrite_cursor = native_overwrite_index(state);
    state.overwrite_open = true;
}

fn toggle_native_keep_dst(state: &mut DiscBrowserState) {
    let new_value = !state.contents.presentations.iter().enumerate().any(|(i,p)|
        state.selected.get(i) == Some(&true) && presentation_has_dst(p)
            && state.keep_dst.get(i) == Some(&true));
    for (i, program) in state.contents.presentations.iter().enumerate() {
        if state.selected.get(i) == Some(&true) && presentation_has_dst(program) {
            if let Some(value) = state.keep_dst.get_mut(i) { *value = new_value; }
        }
    }
}

fn toggle_native_form_row(state: &mut DiscBrowserState) {
    match state.current_output_row() {
        "Keep DST" => toggle_native_keep_dst(state),
        "Tags" if state.native_tags_available() => state.tags = !state.tags,
        "Log" => state.extraction_log = !state.extraction_log,
        "CUE" if state.file_per_program && state.contents.format != crate::disc::DiscFormat::Sacd =>
            state.cue = !state.cue,
        "Assume decrypted" => state.assume_decrypted = !state.assume_decrypted,
        "Downmix" => cycle_native_option(state, 1),
        _ => {},
    }
}

fn begin_native_template_edit(state: &mut DiscBrowserState) {
    state.destination_input = state.naming_template.clone();
    state.text_edit = Some(super::text_input::TextInputState::new_selected(
        state.destination_input.clone()));
    state.destination_caret = state.destination_input.chars().count();
    state.editing_destination = true;
    state.editing_template = true;
    state.text_mouse_origin = None;
    state.text_mouse_drag_started = false;
}

/// The probed codec, not merely a track's optional note, determines whether
/// keeping DST is a meaningful choice for the highlighted SACD area.
pub(super) fn presentation_has_dst(program: &crate::disc::DiscPresentation) -> bool {
    program.format.codec.as_deref() == Some("DST")
        || program.tracks.iter().any(|track|track.format_note.as_deref()
            .is_some_and(|note| note.contains("DST")))
}

pub(super) fn selected_sacd_has_dst(state: &DiscBrowserState) -> bool {
    state.contents.presentations.iter().enumerate().any(|(i, program)|
        state.selected.get(i) == Some(&true)
            && state.items_checked.get(i).is_some_and(|v|v.iter().any(|item|*item))
            && presentation_has_dst(program))
}

fn cycle_native_option(state: &mut DiscBrowserState, step: i32) {
    use crate::convert::pipeline::native_extract::NativeWavContainer;
    use crate::convert::pipeline::DvdaDownmixPolicy;
    match state.output_rows().get(state.output_cursor).copied() {
        _ if state.focus == crate::tui::disc_browser::ExtractFocus::Programs => {
            state.cycle_angle_for(state.cursor, step);
        }
        Some("Format") if state.contents.format == crate::disc::DiscFormat::Sacd => {
            set_native_format(state,usize::from(!state.sacd_dff));
        }
        Some("Keep DST") => toggle_native_keep_dst(state),
        Some("Format") => {
            let all = [NativeWavContainer::Automatic, NativeWavContainer::Riff,
                NativeWavContainer::Rf64, NativeWavContainer::W64];
            let index = all.iter().position(|value| *value == state.wav_container).unwrap_or(0);
            state.wav_container = all[(index as i32 + step).rem_euclid(all.len() as i32) as usize];
        }
        Some("Overwrite") => {
            let next = (native_overwrite_index(state) as i32 + step).rem_euclid(3) as usize;
            set_native_overwrite(state,next);
        },
        Some("Files") => set_native_files(state,usize::from(!state.file_per_program)),
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

/// The key table is the authority for the extraction window's context hints
/// and its normal (non-popup) key dispatch. Each displayed key group maps to
/// one or more concrete key events; no independent hint string can drift from
/// the handler. Editor/popup keys remain owned by their existing controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeKeyAction {
    Up, Down, RangeUp, RangeDown, PageUp, PageDown,
    NextFocus, PreviousFocus, ToPrograms, ToItems,
    Check, All, None, Invert, Enter, Space, Previous, Next,
    FormLeft, FormRight, FooterLeft, FooterRight,
    Hidden, Diagnostics, More, Convert, Help, Close,
    EditDone, EditUndo, EditComplete, EditLeft, EditRight,
    DropdownUp, DropdownDown, DropdownChoose, DropdownClose,
    NamingLoad, NamingCustom,
}

#[derive(Debug, Clone, Copy)]
struct NativeKeyBinding {
    code: KeyCode,
    modifiers: crossterm::event::KeyModifiers,
    hint: &'static str,
    action: NativeKeyAction,
}

fn native_key_bindings(state: &DiscBrowserState) -> Vec<NativeKeyBinding> {
    use crossterm::event::KeyModifiers as M;
    use crate::tui::disc_browser::ExtractFocus;
    use NativeKeyAction as A;
    let mut bindings = Vec::with_capacity(32);
    let mut add = |code, modifiers, hint, action| bindings.push(NativeKeyBinding {
        code, modifiers, hint, action,
    });
    // The editor and dropdown take priority over the underlying Output row.
    // Their hint entries also drive the corresponding key-handler branches.
    if state.editing_destination {
        add(KeyCode::Enter, M::NONE, "↵ Done", A::EditDone);
        add(KeyCode::Esc, M::NONE, "Esc Undo", A::EditUndo);
        add(KeyCode::Tab, M::NONE, "Tab Complete", A::EditComplete);
        add(KeyCode::Left, M::NONE, "←→ caret", A::EditLeft);
        add(KeyCode::Right, M::NONE, "←→ caret", A::EditRight);
        return bindings;
    }
    if state.overwrite_open {
        add(KeyCode::Up, M::NONE, "↑↓ move", A::DropdownUp);
        add(KeyCode::Down, M::NONE, "↑↓ move", A::DropdownDown);
        add(KeyCode::Enter, M::NONE, "↵ choose", A::DropdownChoose);
        add(KeyCode::Esc, M::NONE, "Esc close", A::DropdownClose);
        return bindings;
    }
    match state.focus {
        ExtractFocus::Programs | ExtractFocus::Items => {
            add(KeyCode::Char(' '), M::NONE, "Space check", A::Check);
            for (code, hint, action) in [('a', "a all", A::All),
                ('n', "n none", A::None), ('i', "i invert", A::Invert)] {
                add(KeyCode::Char(code), M::NONE, hint, action);
            }
            if state.focus == ExtractFocus::Items {
                add(KeyCode::Up, M::SHIFT, "⇧↑↓ range", A::RangeUp);
                add(KeyCode::Down, M::SHIFT, "⇧↑↓ range", A::RangeDown);
                add(KeyCode::Left, M::NONE, "← areas", A::ToPrograms);
            } else {
                add(KeyCode::Right, M::NONE, "→ tracks", A::ToItems);
                if !state.contents.suppressed.is_empty() {
                    add(KeyCode::Char('h'), M::NONE, "h hidden", A::Hidden);
                }
            }
            if state.focus == ExtractFocus::Programs {
                add(KeyCode::Enter, M::NONE, "↵ tracks", A::Enter);
            }
        }
        ExtractFocus::Output => {
            let row = state.current_output_row();
            match row {
                "Format" => {
                    add(KeyCode::Char('['), M::NONE, "[ ] choose", A::Previous);
                    add(KeyCode::Char(']'), M::NONE, "[ ] choose", A::Next);
                    add(KeyCode::Char(' '), M::NONE, "Space choose", A::Space);
                }
                "Files" | "Overwrite" | "Downmix" | "Assume decrypted" => {
                    add(KeyCode::Char('['), M::NONE, "[ ] choose", A::Previous);
                    add(KeyCode::Char(']'), M::NONE, "[ ] choose", A::Next);
                    if row != "Overwrite" {
                        add(KeyCode::Char(' '), M::NONE, "Space choose", A::Space);
                    }
                }
                "Tags" if state.native_tags_available() =>
                    add(KeyCode::Char(' '), M::NONE, "Space toggle", A::Space),
                "Log" | "CUE" | "Keep DST" =>
                    add(KeyCode::Char(' '), M::NONE, "Space toggle", A::Space),
                "Naming" => {
                    add(KeyCode::Char('l'),M::NONE,"l presets",A::NamingLoad);
                    add(KeyCode::Char('b'),M::NONE,"b builder",A::NamingCustom);
                },
                _ => {}
            }
            // Every visible form control has a working Enter action; Tags
            // opens its sheet even for DFF-only media so Use DSF is reachable.
            {
            add(KeyCode::Enter, M::NONE, match row {
                "Overwrite" => "↵ open list",
                "Tags" => "↵ edit tags",
                "Folder" => "↵ edit folder",
                "Naming" => "↵ edit naming",
                "Format" | "Files" => "↵ choose",
                "Log" | "CUE" | "Keep DST" => "↵ toggle",
                _ => "↵ settings",
            }, A::Enter);
            }
            if native_form_has_neighbor(state, -1) {
                add(KeyCode::Left, M::NONE, "← move", A::FormLeft);
            }
            if native_form_has_neighbor(state, 1) {
                add(KeyCode::Right, M::NONE, "→ move", A::FormRight);
            }
        }
        ExtractFocus::Footer => {
            add(KeyCode::Left, M::NONE, "←→ move", A::FooterLeft);
            add(KeyCode::Right, M::NONE, "←→ move", A::FooterRight);
            add(KeyCode::Enter, M::NONE, "↵ press", A::Enter);
        }
    }
    if state.focus != ExtractFocus::Footer {
        for (code, action) in [(KeyCode::Up, A::Up), (KeyCode::Down, A::Down),
            (KeyCode::Char('k'), A::Up), (KeyCode::Char('j'), A::Down)] {
            add(code, M::NONE, "↑↓ move", action);
        }
    }
    add(KeyCode::Tab, M::NONE, "Tab next", A::NextFocus);
    add(KeyCode::BackTab, M::SHIFT, "⇧Tab previous", A::PreviousFocus);
    add(KeyCode::BackTab, M::NONE, "⇧Tab previous", A::PreviousFocus);
    add(KeyCode::Tab, M::SHIFT, "⇧Tab previous", A::PreviousFocus);
    if state.focus != ExtractFocus::Footer {
        for (code, action) in [(KeyCode::PageUp, A::PageUp), (KeyCode::PageDown, A::PageDown)] {
            add(code, M::NONE, "PgUp/PgDn page", action);
        }
    }
    if !state.contents.diagnostics.is_empty() {
        add(KeyCode::Char('d'), M::NONE, "d diagnostics", A::Diagnostics);
    }
    add(KeyCode::Char('m'), M::NONE, "m options", A::More);
    add(KeyCode::Char('c'), M::NONE, "c convert", A::Convert);
    add(KeyCode::Char('?'), M::NONE, "? all keys", A::Help);
    add(KeyCode::Esc, M::NONE, "Esc close", A::Close);
    add(KeyCode::Char('q'), M::NONE, "q close", A::Close);
    bindings
}

/// The displayed hints and the mouse actions are selected from the same
/// binding table that the key dispatcher reads. Duplicate physical bindings
/// (for example ArrowUp and k) share one visual hint.
fn native_visible_hints(state: &DiscBrowserState) -> Vec<NativeKeyBinding> {
    let bindings = native_key_bindings(state);
    // Prefer navigation and the immediate action over bulk operations that
    // still remain available through ?. All entries are real bindings.
    use NativeKeyAction as A;
    let priority: &[A] = if state.editing_destination {
        &[A::EditDone, A::EditUndo, A::EditComplete]
    } else if state.overwrite_open {
        &[A::DropdownUp, A::DropdownChoose, A::DropdownClose]
    } else { match state.focus {
        crate::tui::disc_browser::ExtractFocus::Programs =>
            &[A::Up, A::Check, A::ToItems, A::Hidden, A::NextFocus],
        crate::tui::disc_browser::ExtractFocus::Items =>
            &[A::Up, A::Check, A::RangeUp, A::ToPrograms, A::NextFocus],
        crate::tui::disc_browser::ExtractFocus::Output =>
            &[A::Up, A::NamingLoad, A::NamingCustom, A::Previous, A::Space, A::Enter, A::NextFocus],
        crate::tui::disc_browser::ExtractFocus::Footer =>
            &[A::FooterLeft, A::Enter, A::NextFocus],
    }};
    let mut hints = Vec::with_capacity(6);
    for action in priority {
        if let Some(binding) = bindings.iter().find(|binding| binding.action == *action) {
            if !hints.iter().any(|other: &NativeKeyBinding| other.hint == binding.hint) {
                hints.push(*binding);
            }
        }
    }
    if !state.editing_destination && !state.overwrite_open {
        if let Some(help) = bindings.into_iter().find(|binding| binding.action == A::Help) {
            hints.push(help);
        }
    }
    hints
}

pub(crate) fn native_key_hints(state: &DiscBrowserState) -> Vec<&'static str> {
    native_visible_hints(state).into_iter().map(|hint| hint.hint).collect()
}

fn native_hint_can_click(action: NativeKeyAction) -> bool {
    use NativeKeyAction as A;
    matches!(action,A::Check|A::All|A::None|A::Invert|A::Enter|A::Space
        |A::Previous|A::Next|A::Hidden|A::Diagnostics|A::Help
        |A::NamingLoad|A::NamingCustom
        |A::EditDone|A::EditUndo|A::EditComplete
        |A::DropdownUp|A::DropdownChoose|A::DropdownClose)
}

pub(crate) fn native_hint_clickable(state: &DiscBrowserState, index: usize) -> bool {
    native_visible_hints(state).get(index)
        .is_some_and(|hint|native_hint_can_click(hint.action))
}

fn native_hint_key(state: &DiscBrowserState, index: usize) -> Option<KeyEvent> {
    native_visible_hints(state).get(index)
        .filter(|hint|native_hint_can_click(hint.action))
        .map(|hint|KeyEvent::new(hint.code,hint.modifiers))
}

pub(crate) fn native_key_help(state: &DiscBrowserState) -> Vec<String> {
    use crate::tui::disc_browser::ExtractFocus;
    let mut lines = vec!["Keys for the extract window".to_owned(),
        "↑↓ scroll this help · Esc close".to_owned()];
    // Lines stay short enough for the existing read-only panel. The regular
    // panel scroll keys handle output forms with several optional rows.
    for (label, focus) in [("Areas / groups", ExtractFocus::Programs),
        ("Tracks / chapters", ExtractFocus::Items), ("Output", ExtractFocus::Output),
        ("Footer buttons", ExtractFocus::Footer)] {
        let mut context=state.clone();
        context.focus=focus;
        context.editing_destination=false;
        context.overwrite_open=false;
        lines.push(format!("{label}:"));
        let row_count = if focus == ExtractFocus::Output { context.output_rows().len() } else { 1 };
        for row in 0..row_count {
            if focus == ExtractFocus::Output && !context.output_row_focusable(row) { continue; }
            context.output_cursor=row;
            let name=if focus == ExtractFocus::Output {format!("  {}: ",context.current_output_row())}
                else {"  ".to_owned()};
            let mut text=name.clone();
            let mut seen = std::collections::HashSet::new();
            let hints = native_key_bindings(&context).into_iter()
                .filter_map(|binding| seen.insert(binding.hint).then_some(binding.hint));
            for hint in hints {
                let separator=if text==name {""}else{" · "};
                // The 64-column popup leaves about 50 columns for panel
                // text; wrap here rather than clipping active commands.
                if text.chars().count()+separator.chars().count()+hint.chars().count()>49 {
                    lines.push(text);
                    text="    ".to_owned();
                }
                if text != name && text != "    " {text.push_str(separator);}
                text.push_str(hint);
            }
            lines.push(text);
        }
    }
    lines.push("Dropdown: ↑↓ move · ↵ choose · Esc close".into());
    lines.push("Folder: Tab complete · ↵ done · Esc undo".into());
    lines.push("Naming: Tab complete · ↵ done · Esc undo".into());
    lines.push("Tags sheet: use its own listed keys".into());
    lines
}

/// The artwork preview is a one-time source discovery at sheet opening.
/// Redrawing the sheet performs no filesystem scans.
fn open_native_tags_sheet(state: &mut DiscBrowserState) {
    state.tag_preview_artwork = crate::convert::pipeline::native_extract::native_artwork_path(
        &Default::default(), &state.source_path);
    state.panel = Some(crate::tui::disc_browser::ExtractPanel::Tags);
    state.panel_cursor = 0;
    state.tag_source_open = false;
}

fn activate_native_tag_control(state: &mut DiscBrowserState,
    control: crate::tui::disc_browser::TagSheetControl,
) {
    use crate::tui::disc_browser::TagSheetControl as C;
    match control {
        C::Transfer => state.tags = !state.tags,
        C::Source => {
            state.tag_source_cursor = state.tag_source_index();
            state.tag_source_open = !state.tag_source_open;
        }
        C::Field(index) => {state.tag_fields ^= 1u16 << index;}
        C::All => state.tag_fields = 0x1ff,
        C::None => state.tag_fields = 0,
        C::Artwork => state.tag_artwork = !state.tag_artwork,
        C::UseDsf => {
            state.sacd_dff = false;
            for (i, keep) in state.keep_dst.iter_mut().enumerate() {
                if state.selected.get(i) == Some(&true) { *keep = false; }
            }
        }
        C::Done => {state.panel = None; state.tag_source_open = false;}
        C::EditValues | C::MusicBrainz => {}, // App-level actions below.
    }
    let len = state.tag_sheet_controls().len();
    state.panel_cursor = state.panel_cursor.min(len.saturating_sub(1));
}

fn set_native_tag_source(state: &mut DiscBrowserState, choice: usize) {
    use crate::convert::pipeline::native_extract::NativeTagSource;
    if choice >= state.tag_source_option_count() {return;}
    state.tag_source = match choice {
        0 => NativeTagSource::DiscAndSidecar,
        1 => NativeTagSource::AuthoredTrack,
        _ => NativeTagSource::MusicBrainz,
    };
    state.tag_source_cursor = choice;
    state.tag_source_open = false;
}

/// Open the existing editor on the exact Extract source/presentation, retaining
/// its in-memory draft through the editor and any nested lookup overlays.
fn open_extract_owned_tag_editor(app: &mut AppState) -> Result<bool, String> {
    let draft = match &app.active_overlay {
        ActiveOverlay::DiscBrowser(state) => state.clone(),
        _ => return Ok(false),
    };
    let opened = open_existing_metadata_editor_for_disc_cursor(app)?;
    if opened {
        if let ActiveOverlay::MetadataEditor(editor) = &mut app.active_overlay {
            editor.return_to_extract = Some(draft);
        } else {
            app.active_overlay = ActiveOverlay::DiscBrowser(draft);
            return Err("the selected metadata editor could not be opened".into());
        }
    }
    Ok(opened)
}

/// Return from the editor without replacing the current selection and output
/// settings. The existing asynchronous disc probe rereads saved sidecar tags;
/// Extract is held until that authoritative evidence is back.
pub(super) fn restore_extract_after_tag_editor(
    app: &mut AppState,
    editor: &mut crate::tui::app::MetadataEditorState,
    tx: &mpsc::Sender<AppMessage>,
) -> bool {
    let Some(mut draft) = editor.return_to_extract.take() else {return false;};
    draft.tag_mb_available |= editor.extract_mb_saved;
    let saved = editor.extract_tags_saved;
    draft.tag_refresh_pending = saved;
    draft.tag_refresh_failed = false;
    let path = draft.source_path.clone();
    app.active_overlay = ActiveOverlay::DiscBrowser(draft);
    if saved { force_disc_reprobe(app, path, tx); }
    true
}

fn native_tag_external_action(app: &mut AppState,
    control: crate::tui::disc_browser::TagSheetControl,
    tx: &mpsc::Sender<AppMessage>,
) -> bool {
    use crate::tui::disc_browser::TagSheetControl as C;
    match control {
        C::EditValues => {
            match open_extract_owned_tag_editor(app) {
                Ok(true) => {},
                Ok(false) => app.set_status("No selected presentation is available to edit"),
                Err(error) => app.set_status(format!("Cannot open tag editor: {error}")),
            }
            true
        }
        C::MusicBrainz => {
            // The in-editor dispatch already carries the chosen presentation
            // and source path; a Browse multi-selection is not this lookup.
            match open_extract_owned_tag_editor(app) {
                Ok(true) => crate::tui::command::execute_command(app,
                    crate::tui::command::Command::TagsFromMb {query:None,catno:None,year:None},tx),
                Ok(false) => app.set_status("No selected presentation is available for lookup"),
                Err(error) => app.set_status(format!("Cannot open tag lookup: {error}")),
            }
            true
        }
        _ => false,
    }
}

/// Validate and apply an edited field. On failure keep the shared editor
/// alive with its selection, undo history and caret intact.
fn commit_native_text_edit(state:&mut DiscBrowserState) -> bool {
    receive_native_text_projection(state);
    let Some(value)=state.text_edit.as_ref().map(|input|input.text.trim().to_string()) else {
        return false;
    };
    if state.editing_template {
        if value.is_empty() {return false;}
        state.naming_template=value;
    } else {
        let path=expand_native_home(&value);
        if !path.is_absolute() || path.components().any(|component|
            matches!(component,std::path::Component::ParentDir)) {
            state.destination_feedback=Some(
                crate::tui::disc_browser::DestinationFeedback::InvalidPath);
            return false;
        }
        state.destination=path;
    }
    cancel_native_text_edit(state);
    true
}

fn native_text_tab_completion(state:&mut DiscBrowserState) -> bool {
    receive_native_text_projection(state);
    let Some(input)=state.text_edit.as_mut() else {return false;};
    let before=input.text.clone();
    if state.editing_template {
        let available=super::template_builder::native_completion_tokens(state.contents.format);
        let candidates=available.iter().map(String::as_str).collect::<Vec<_>>();
        super::text_input::apply_template_variable_completion_with(input,&candidates);
    }else{
        let byte=input.cursor;
        let mut prefix=input.text[..byte].to_owned();
        complete_native_folder_segment(&mut prefix);
        if prefix.as_str()!=&input.text[..byte] {
            input.replace_range(0..byte,&prefix);
        }
    }
    let changed=input.text!=before;
    sync_native_text_projection(state);
    changed
}

fn handle_native_text_key(state:&mut DiscBrowserState,key:KeyEvent) {
    use crossterm::event::{KeyCode,KeyModifiers};
    match key.code {
        KeyCode::Esc => cancel_native_text_edit(state),
        KeyCode::Enter => {commit_native_text_edit(state);},
        KeyCode::Tab if !key.modifiers.contains(KeyModifiers::SHIFT) => {
            if !native_text_tab_completion(state) && commit_native_text_edit(state) {
                move_native_form_cursor(state,0,1);
            }
        }
        KeyCode::BackTab | KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
            if commit_native_text_edit(state) { move_native_form_cursor(state,0,-1); }
        }
        _ => {
            receive_native_text_projection(state);
            if let Some(input)=state.text_edit.as_mut() {
                // The shared reducer owns navigation, selection, edits,
                // shortcuts and the logical clipboard. Unknown chords are
                // swallowed here rather than leaking to form accelerators.
                let _=super::text_input::handle_text_input_key(input,&key);
            }
            sync_native_text_projection(state);
        }
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
    let ActiveOverlay::DiscBrowser(_) = &app.active_overlay else { return; };
    if matches!(key.code, KeyCode::Char('m' | 'M')) && key.modifiers == KeyModifiers::ALT
        && matches!(&app.active_overlay, ActiveOverlay::DiscBrowser(s)
            if s.panel.is_none() && !s.editing_destination) {
        if let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay {
            state.maximized = !state.maximized;
        }
        return;
    }
    if let ActiveOverlay::DiscBrowser(state)=&app.active_overlay {
        if state.panel.is_none() && !state.overwrite_open && !state.editing_destination
            && state.focus==ExtractFocus::Output && state.current_output_row()=="Naming"
            && key.modifiers==KeyModifiers::NONE {
            match key.code {
                KeyCode::Char('l')=>{open_native_naming_picker(app);return;},
                KeyCode::Char('b')=>{open_native_naming_builder(app);return;},
                _=>{},
            }
        }
    }
    let ActiveOverlay::DiscBrowser(state) = &app.active_overlay else { return; };
    // Text editors and popups own their keys. In particular Ctrl+X must not
    // bypass an unfinished destination edit or a pending choice dialog.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('x')
        && !state.editing_destination {
        if state.panel.is_none() && !state.overwrite_open {
            start_native_extraction(app, tx);
        }
        return;
    }
    if matches!(key.code, KeyCode::Enter | KeyCode::Char(' ')) {
        let sheet_action = match &app.active_overlay {
            ActiveOverlay::DiscBrowser(state) if state.panel == Some(ExtractPanel::Tags)
                && !state.tag_source_open => state.tag_sheet_controls().get(state.panel_cursor).copied(),
            _ => None,
        };
        if sheet_action.is_some_and(|control|native_tag_external_action(app,control,tx)) {
            return;
        }
    }
    // Opening the existing editor mutates the overlay, so perform this route
    // before borrowing the extraction state for in-panel key dispatch.
    if matches!(&app.active_overlay, ActiveOverlay::DiscBrowser(state)
        if matches!(state.panel, Some(ExtractPanel::More))
            && !state.editing_destination)
        && key.code == KeyCode::Char('e')
    {
        match open_extract_owned_tag_editor(app) {
            Ok(true) => {},
            Ok(false) => app.set_status("No selected presentation is available to edit"),
            Err(error) => app.set_status(format!("Cannot open tag editor: {error}")),
        }
        return;
    }
    // More routes to established app actions, not a second lookup or probe.
    if matches!(&app.active_overlay, ActiveOverlay::DiscBrowser(state)
        if matches!(state.panel, Some(ExtractPanel::More)))
        && key.code == KeyCode::Char('b')
    {
        match open_extract_owned_tag_editor(app) {
            Ok(true) => crate::tui::command::execute_command(app,
                crate::tui::command::Command::TagsFromMb {query:None,catno:None,year:None},tx),
            Ok(false) => app.set_status("No selected presentation is available for lookup"),
            Err(error) => app.set_status(format!("Cannot open tag lookup: {error}")),
        }
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
    // Enter now edits the field. The Browse button still opens the existing picker.
    let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay else { return; };

    if state.overwrite_open {
        use NativeKeyAction as A;
        let action = native_key_bindings(state).into_iter().find(|binding|
            binding.code == key.code && binding.modifiers == key.modifiers)
            .map(|binding| binding.action);
        match action {
            Some(A::DropdownClose) => state.overwrite_open = false,
            Some(A::DropdownUp) => state.overwrite_cursor = state.overwrite_cursor.saturating_sub(1),
            Some(A::DropdownDown) => state.overwrite_cursor = (state.overwrite_cursor+1).min(2),
            Some(A::DropdownChoose) => {
                set_native_overwrite(state, state.overwrite_cursor);
                state.overwrite_open = false;
            }
            _ => {},
        }
        return;
    }
    if state.editing_destination {
        handle_native_text_key(state,key);
        return;
    }

    if let Some(panel) = state.panel {
        if panel == ExtractPanel::Tags {
            use crate::tui::disc_browser::TagSheetControl as C;
            if state.tag_source_open {
                match key.code {
                    KeyCode::Esc => state.tag_source_open = false,
                    KeyCode::Up | KeyCode::Char('[') => state.tag_source_cursor = state.tag_source_cursor.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char(']') => state.tag_source_cursor = (state.tag_source_cursor+1).min(state.tag_source_option_count()-1),
                    KeyCode::Enter | KeyCode::Char(' ') => set_native_tag_source(state,state.tag_source_cursor),
                    _ => {},
                }
                return;
            }
            match key.code {
                KeyCode::Esc => {state.panel=None; state.panel_cursor=0;},
                KeyCode::Up | KeyCode::BackTab => state.panel_cursor = state.panel_cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Tab => state.panel_cursor =
                    (state.panel_cursor+1).min(state.tag_sheet_controls().len().saturating_sub(1)),
                KeyCode::Char('[') | KeyCode::Char(']')
                    if state.tag_sheet_controls().get(state.panel_cursor) == Some(&C::Source) => {
                    let current=state.tag_source_index();
                    let choice=if key.code == KeyCode::Char(']') {
                        (current+1).min(state.tag_source_option_count()-1)
                    }else{current.saturating_sub(1)};
                    set_native_tag_source(state,choice);
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if let Some(control)=state.tag_sheet_controls().get(state.panel_cursor).copied() {
                        activate_native_tag_control(state,control);
                    }
                }
                _ => {},
            }
            return;
        }
        match (panel, key.code) {
            (_, KeyCode::Esc) => { state.panel = None; state.panel_cursor = 0; }
            (ExtractPanel::Downmix | ExtractPanel::AssumeDecrypted, KeyCode::Char('[')) => cycle_native_option(state, -1),
            (ExtractPanel::Downmix | ExtractPanel::AssumeDecrypted, KeyCode::Char(']')) => cycle_native_option(state, 1),
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

    if state.focus == ExtractFocus::Output { state.normalize_output_cursor(); }
    let action = native_key_bindings(state).into_iter().find(|binding|
        binding.code == key.code && binding.modifiers == key.modifiers)
        .map(|binding| binding.action);
    use NativeKeyAction as A;
    match action {
        Some(A::Close) => { state.probe_cancel.cancel(); app.active_overlay = ActiveOverlay::None; }
        Some(A::Help) => { state.panel = Some(ExtractPanel::Keys); state.panel_cursor = 0; }
        Some(A::NextFocus | A::PreviousFocus) => {
            let backwards = action == Some(A::PreviousFocus);
            state.focus = match (state.focus, backwards) {
                (ExtractFocus::Programs, false) | (ExtractFocus::Output, true) => ExtractFocus::Items,
                (ExtractFocus::Items, false) | (ExtractFocus::Footer, true) => ExtractFocus::Output,
                (ExtractFocus::Output, false) | (ExtractFocus::Programs, true) => ExtractFocus::Footer,
                (ExtractFocus::Footer, false) | (ExtractFocus::Items, true) => ExtractFocus::Programs,
            };
            if state.focus == ExtractFocus::Output { state.normalize_output_cursor(); }
            if state.focus == ExtractFocus::Footer && !state.footer_extract_enabled.get() {
                state.footer_cursor = 1;
            }
        }
        Some(A::ToPrograms) => state.focus = ExtractFocus::Programs,
        Some(A::ToItems) => state.focus = ExtractFocus::Items,
        Some(A::Up | A::Down | A::RangeUp | A::RangeDown) => {
            let delta = if matches!(action, Some(A::Up | A::RangeUp)) {-1isize} else {1isize};
            match state.focus {
                ExtractFocus::Programs => {
                    let from = state.cursor;
                    state.move_cursor(delta);
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        for index in from.min(state.cursor)..=from.max(state.cursor) {
                            if state.is_extractable(index) {state.selected[index] = true;}
                        }
                    }
                    state.item_cursor = 0; state.item_scroll = 0;
                }
                ExtractFocus::Items => {
                    let from = state.item_cursor;
                    let last = state.selected_presentation().map_or(0, |p|p.tracks.len().saturating_sub(1));
                    state.item_cursor = if delta < 0 {from.saturating_sub(1)}
                        else {from.saturating_add(1).min(last)};
                    if matches!(action,Some(A::RangeUp | A::RangeDown)) {
                        state.check_item_range(from,state.item_cursor);
                    }
                }
                ExtractFocus::Output => {
                    move_native_form_cursor(state,0,if delta < 0 {-1}else{1});
                }
                ExtractFocus::Footer => {},
            }
        }
        Some(A::Check) => match state.focus {
            ExtractFocus::Programs => state.toggle_selected(state.cursor),
            ExtractFocus::Items => state.toggle_item(state.item_cursor),
            ExtractFocus::Output | ExtractFocus::Footer => {},
        },
        Some(A::All | A::None | A::Invert) => {
            state.check_focus(match action {Some(A::All)=>'a', Some(A::None)=>'n', _=>'i'});
        }
        Some(A::Hidden) => state.show_hidden = !state.show_hidden,
        Some(A::Diagnostics) => {state.panel=Some(ExtractPanel::Diagnostics);state.panel_cursor=0;}
        Some(A::More) => {state.panel=Some(ExtractPanel::More);state.panel_cursor=0;}
        Some(A::Convert) => activate_overlay_convert(app),
        Some(A::Previous | A::Next) => cycle_native_option(state, if action==Some(A::Previous){-1}else{1}),
        Some(A::PageDown | A::PageUp) => {
            let down=action==Some(A::PageDown);
            match state.focus {
                ExtractFocus::Programs => state.move_cursor(if down {10}else{-10}),
                ExtractFocus::Items => {
                    let last=state.selected_presentation().map_or(0,|p|p.tracks.len().saturating_sub(1));
                    state.item_cursor=if down{state.item_cursor.saturating_add(10).min(last)}
                        else{state.item_cursor.saturating_sub(10)};
                }
                ExtractFocus::Output => {
                    let last=state.output_rows().len().saturating_sub(1);
                    state.output_cursor=if down{state.output_cursor.saturating_add(2).min(last)}
                        else{state.output_cursor.saturating_sub(2)};
                    state.normalize_output_cursor();
                }
                ExtractFocus::Footer => {},
            }
        }
        Some(A::FormLeft | A::FormRight) => {
            move_native_form_cursor(state,if action == Some(A::FormLeft) {-1}else{1},0);
        }
        Some(A::FooterLeft | A::FooterRight) => {
            let step = if action == Some(A::FooterLeft) {-1i8} else {1};
            let minimum = if state.footer_extract_enabled.get() {0} else {1};
            state.footer_cursor = (state.footer_cursor as i8 + step).clamp(minimum,3) as usize;
        }
        Some(A::Space) => match state.current_output_row() {
            "Log" | "Tags" | "CUE" | "Keep DST" => toggle_native_form_row(state),
            "Format" | "Files" | "Downmix" | "Assume decrypted" => cycle_native_option(state,1),
            "Folder" => begin_native_folder_edit(state),
            "Naming" => begin_native_template_edit(state),
            "Overwrite" => open_native_overwrite(state),
            _ => {},
        },
        Some(A::Enter) => match state.focus {
            ExtractFocus::Programs => state.focus=ExtractFocus::Items,
            ExtractFocus::Items => {},
            ExtractFocus::Output => match state.current_output_row() {
                "Folder" => begin_native_folder_edit(state),
                "Naming" => begin_native_template_edit(state),
                "Overwrite" => open_native_overwrite(state),
                "Log" | "CUE" | "Keep DST" => toggle_native_form_row(state),
                "Format" | "Files" | "Downmix" | "Assume decrypted" => cycle_native_option(state,1),
                "Tags" => open_native_tags_sheet(state),
                _ => {},
            },
            ExtractFocus::Footer => match state.footer_cursor {
                0 if state.footer_extract_enabled.get() => start_native_extraction(app,tx),
                1 => activate_overlay_convert(app),
                2 => {state.panel=Some(ExtractPanel::More);state.panel_cursor=0;},
                3 => {state.probe_cancel.cancel();app.active_overlay=ActiveOverlay::None;},
                _ => {},
            },
        },
        // Modal bindings are dispatched by their owning editor or dropdown
        // before this ordinary form handler is reached.
        Some(A::EditDone | A::EditUndo | A::EditComplete | A::EditLeft | A::EditRight
            | A::DropdownUp | A::DropdownDown | A::DropdownChoose | A::DropdownClose
            | A::NamingLoad | A::NamingCustom) => {},
        None => {},
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
    fn r18_hint_actions_are_bound_only_in_their_actual_context() {
        use crate::tui::disc_browser::ExtractFocus;
        let contents=selectable_disc_contents(PathBuf::from("disc.iso"),FolderDiscMarkerKind::Sacd);
        let mut state=DiscBrowserState::new(contents,PathBuf::from("disc.iso"));
        for focus in [ExtractFocus::Programs, ExtractFocus::Items, ExtractFocus::Output] {
            state.focus=focus;
            for index in 0..state.output_rows().len() {
                state.output_cursor=index;
                let bindings=native_key_bindings(&state);
                for hint in native_visible_hints(&state) {
                    assert!(bindings.iter().any(|candidate|candidate.code==hint.code
                        && candidate.modifiers==hint.modifiers && candidate.action==hint.action));
                }
                if focus==ExtractFocus::Output {
                    assert!(!bindings.iter().any(|binding|
                        binding.action==NativeKeyAction::All || binding.action==NativeKeyAction::Invert));
                    if state.current_output_row()=="Format" {
                        // Plain DSD now supports DSF/DFF choice. Keep DST is
                        // a separate control, absent for this DSD-only area.
                        assert!(!state.output_rows().contains(&"Keep DST"));
                        for action in [NativeKeyAction::Previous, NativeKeyAction::Next,
                            NativeKeyAction::Space, NativeKeyAction::Enter] {
                            assert!(bindings.iter().any(|binding|binding.action==action),
                                "Format must expose its DSF/DFF choice: {action:?}");
                        }
                    }
                } else {
                    assert!(!bindings.iter().any(|binding|binding.action==NativeKeyAction::Previous));
                }
            }
        }
        state.focus=ExtractFocus::Output;
        state.output_cursor=state.output_rows().iter().position(|name|*name=="Folder").unwrap();
        let labels=native_key_hints(&state);
        assert!(labels.contains(&"↵ edit folder"));
        assert!(native_hint_key(&state,labels.iter().position(|hint|*hint=="↵ edit folder").unwrap()).is_some());
    }

    #[test]
    fn all_keys_overlay_uses_full_binding_table_and_hidden_hint_is_actionable() {
        use crate::tui::disc_browser::ExtractFocus;
        let contents=selectable_disc_contents(PathBuf::from("disc.iso"),FolderDiscMarkerKind::Sacd);
        let mut state=DiscBrowserState::new(contents, PathBuf::from("disc.iso"));
        let help=native_key_help(&state).join("\n");
        assert!(native_key_help(&state).iter().all(|line|line.chars().count()<=49),
            "help rows must fit the smallest scrolling help panel");
        for expected in ["n none", "i invert", "a all", "PgUp/PgDn page",
            "⇧↑↓ range", "↵ edit folder", "Space toggle", "↵ edit naming", "↵ open list"] {
            assert!(help.contains(expected), "Missing {expected} from all-keys help: {help}");
        }
        for focus in [ExtractFocus::Programs, ExtractFocus::Items, ExtractFocus::Output] {
            state.focus=focus;
            for cursor in 0..state.output_rows().len() {
                state.output_cursor=cursor;
                let help=native_key_help(&state).join("\n");
                for binding in native_key_bindings(&state) {
                    assert!(help.contains(binding.hint),
                        "Help does not report active binding {} for {focus:?}: {help}",binding.hint);
                }
            }
        }
        // Hiding/showing suppressed areas is an active and clickable hint
        // when that menu actually contains suppressed programs.
        state.contents.suppressed.push(crate::disc::model::SuppressedPresentation {
            id: PresentationId::SacdArea(SacdAreaId::MultiChannel),
            reason: "duplicate area".into(), track_count: 1,
            duration_secs: 60.0, native_detail: None,
        });
        state.focus=ExtractFocus::Programs;
        let hints=native_key_hints(&state);
        let index=hints.iter().position(|label|*label=="h hidden").expect("visible hidden toggle");
        let key=native_hint_key(&state,index).unwrap();
        assert_eq!(key.code,KeyCode::Char('h'));
        assert!(native_key_help(&state).join("\n").contains("h hidden"));
        state.focus=ExtractFocus::Output;
        state.output_cursor=0;
        assert!(!native_key_hints(&state).contains(&"h hidden"));
        assert!(!native_key_help(&state).join("\n").contains("k  Keep DST"));
    }

    #[test]
    fn r22_folder_field_edit_and_browse_picker_round_trip() {
        use crossterm::event::KeyModifiers as M;
        let tmp = tempfile::tempdir().unwrap();
        let disc = tmp.path().join("disc.iso");
        std::fs::write(&disc, b"test disc").unwrap();
        let contents = selectable_disc_contents(disc.clone(), FolderDiscMarkerKind::Sacd);
        let mut state = DiscBrowserState::new(contents, disc);
        state.naming_template = "{track} - {title}".into();
        focus_native_row(&mut state,"Folder");
        let original_dest=state.destination.clone();
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(16);
        // Enter edits the actual field; Esc restores the original destination.
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Enter,M::NONE),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.editing_destination && !s.editing_template));
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Char('x'),M::NONE),&tx);
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Esc,M::NONE),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if !s.editing_destination && s.destination==original_dest));

        // A single Browse click is independent of text-field editing.
        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserFolderBrowse,1,&tx);
        assert!(app.pending_disc_extract_picker.is_some());
        let (session_id,purpose)=match &app.active_overlay {
            ActiveOverlay::FilePicker(picker) => (picker.session_id,picker.purpose.clone()),
            other => panic!("expected existing picker: {other:?}"),
        };
        let chosen=tmp.path().to_path_buf();
        crate::tui::event_loop::handle_message(&mut app,AppMessage::FilePickerComplete {
            session_id,purpose,path:Some(chosen.clone()),paths:vec![chosen.clone()],
            ignored_directories:0,
        },&tx);
        let ActiveOverlay::DiscBrowser(restored)=&app.active_overlay else {panic!("picker did not restore dialog")};
        assert_eq!(restored.destination,chosen);
        assert_eq!(restored.naming_template,"{track} - {title}");
        assert_eq!(restored.focus,crate::tui::disc_browser::ExtractFocus::Output);
    }

    #[test]
    fn r27_shared_naming_builder_and_picker_restore_extract_without_mutating_convert() {
        use crossterm::event::KeyModifiers as M;
        let contents=selectable_disc_contents(PathBuf::from("disc.iso"),FolderDiscMarkerKind::Sacd);
        let mut state=DiscBrowserState::new(contents,PathBuf::from("disc.iso"));
        focus_native_row(&mut state,"Naming");
        let initial=state.naming_template.clone();
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        let convert_before=app.convert.output_options.filename_template.clone();
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);

        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserNamingCustom,1,&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::TemplateBuilder(builder)
            if builder.return_to_extract.is_some()
                && builder.disc_format==Some(crate::disc::DiscFormat::Sacd)));
        crate::tui::keybindings::handle_key(&mut app,KeyEvent::new(KeyCode::Esc,M::NONE),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.naming_template==initial));

        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Char('b'),M::NONE),&tx);
        if let ActiveOverlay::TemplateBuilder(builder)=&mut app.active_overlay {
            builder.template_input=super::super::text_input::TextInputState::new(
                "%ARTIST%/%TRACKNUMBER% - %TITLE%".into());
        } else {panic!("b must open the shared naming builder");}
        crate::tui::keybindings::handle_key(&mut app,KeyEvent::new(KeyCode::Enter,M::NONE),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.naming_template=="%ARTIST%/%TRACKNUMBER% - %TITLE%"));
        assert_eq!(app.convert.output_options.filename_template,convert_before);

        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Char('l'),M::NONE),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::TemplatePicker{
            return_to_extract:Some(_),..}));
        if let ActiveOverlay::TemplatePicker{templates,selected,..}=&mut app.active_overlay {
            *templates=vec!["%TRACKNN% - %TITLE%".into()];
            *selected=0;
        }
        crate::tui::keybindings::handle_key(&mut app,KeyEvent::new(KeyCode::Enter,M::NONE),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.naming_template=="%TRACKNN% - %TITLE%"));
        assert_eq!(app.convert.output_options.filename_template,convert_before);

        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserNamingLoad,1,&tx);
        crate::tui::keybindings::handle_key(&mut app,KeyEvent::new(KeyCode::Esc,M::NONE),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.naming_template=="%TRACKNN% - %TITLE%"));
    }

    #[test]
    fn r28_convert_preset_load_and_shared_builder_mouse_apply_restore_extract() {
        use crossterm::event::{KeyModifiers as M, MouseButton, MouseEvent, MouseEventKind};
        use ratatui::{backend::TestBackend, Terminal};
        let mut contents=selectable_disc_contents(PathBuf::from("disc.iso"),FolderDiscMarkerKind::Sacd);
        contents.album_title=Some("Plain Album (Japan SHM-CD)".into());
        contents.presentations[0].tracks[0].title=Some("First Song".into());
        let mut state=DiscBrowserState::new(contents,PathBuf::from("disc.iso"));
        state.destination="/tmp/r28-preset".into();
        focus_native_row(&mut state,"Naming");
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        let convert_before=app.convert.output_options.filename_template.clone();
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserNamingLoad,1,&tx);
        let saved="%DISC%/%ALBUM% {%TITLE_EXTRA%}/%TRACKNN% - %TITLE%.%EXTENSION%";
        if let ActiveOverlay::TemplatePicker{templates,selected,..}=&mut app.active_overlay {
            *templates=vec![saved.into()];
            *selected=0;
        } else {panic!("shared Convert preset picker not opened");}
        crate::tui::keybindings::handle_key(&mut app,KeyEvent::new(KeyCode::Enter,M::NONE),&tx);
        let ActiveOverlay::DiscBrowser(loaded)=&app.active_overlay else {
            panic!("preset picker did not return to Extract");
        };
        assert_eq!(loaded.naming_template,saved);
        assert_eq!(loaded.native_preview_path().unwrap(),PathBuf::from(
            "/tmp/r28-preset/Stereo/1/Plain Album {Japan SHM-CD}/01 - First Song.dsf"));
        assert_eq!(app.convert.output_options.filename_template,convert_before);

        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserNamingCustom,1,&tx);
        if let ActiveOverlay::TemplateBuilder(builder)=&mut app.active_overlay {
            builder.saved_templates=(0..4).map(|n|format!("%ARTIST% - {n}")).collect();
            builder.template_input=crate::tui::text_input::TextInputState::new(
                "%DISC%/%TRACKNN% - %TITLE%".into());
        } else {panic!("shared Convert builder not opened");}
        let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut terminal=Terminal::new(TestBackend::new(80,24)).unwrap();
        app.button_map=crate::tui::button_map::ButtonRenderMap::new();
        if let ActiveOverlay::TemplateBuilder(builder)=&app.active_overlay {
            terminal.draw(|frame|crate::tui::template_builder::draw_template_builder(
                frame,builder,&mut app.button_map,theme)).unwrap();
        }
        let apply=app.button_map.button_rect(TuiButton::TemplateBuilderApply)
            .expect("Apply must be mouse-visible at 80x24 with four saved presets");
        assert!(apply.y<24);
        crate::tui::keybindings::handle_mouse(&mut app, MouseEvent {
            kind:MouseEventKind::Down(MouseButton::Left),
            column:apply.x+1,row:apply.y,modifiers:M::NONE,
        },&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(restored)
            if restored.naming_template=="%DISC%/%TRACKNN% - %TITLE%"));
        assert_eq!(app.convert.output_options.filename_template,convert_before);
    }

    #[test]
    fn r27_tab_completes_format_tokens_without_leaking_to_other_formats() {
        use crossterm::event::KeyModifiers as M;
        let contents=selectable_disc_contents(PathBuf::from("disc.iso"),FolderDiscMarkerKind::Sacd);
        let mut state=DiscBrowserState::new(contents,PathBuf::from("disc.iso"));
        state.contents.format=crate::disc::DiscFormat::DvdVideo;
        focus_native_row(&mut state,"Naming");
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Enter,M::NONE),&tx);
        if let ActiveOverlay::DiscBrowser(s)=&mut app.active_overlay {
            s.destination_input="%DVDT".into();
            s.destination_caret=5;
        }
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Tab,M::NONE),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.destination_input=="%DVDTITLE%" && s.destination_caret==10));
        if let ActiveOverlay::DiscBrowser(s)=&mut app.active_overlay {
            s.contents.format=crate::disc::DiscFormat::Sacd;
            s.destination_input="%DVDT".into();
            s.destination_caret=5;
        }
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Tab,M::NONE),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if !s.editing_destination && s.naming_template=="%DVDT"
                && s.destination_input.is_empty()),
            "without a matching SACD completion, Tab commits the unchanged draft");
    }

    #[test]
    fn r22_form_controls_render_and_use_single_clicks_and_keyboard() {
        use crate::tui::button_map::ButtonRenderMap;
        use crate::tui::disc_browser::ExtractFocus;
        use crossterm::event::KeyModifiers as M;
        use ratatui::{backend::TestBackend,Terminal};
        let (tx,_rx)=tokio::sync::mpsc::channel(16);
        for (width,height) in [(72,24),(110,34),(154,40)] {
            let contents=selectable_disc_contents(PathBuf::from("disc.iso"),FolderDiscMarkerKind::Sacd);
            let mut state=DiscBrowserState::new(contents,PathBuf::from("disc.iso"));
            state.focus=ExtractFocus::Output;
            focus_native_row(&mut state,"Log");
            let mut terminal=Terminal::new(TestBackend::new(width,height)).unwrap();
            let mut buttons=ButtonRenderMap::new();
            terminal.draw(|frame|crate::tui::disc_extract_render::draw(frame,&state,
                Path::new("/tmp"),&mut buttons,crate::tui::theme::theme_by_slug_or_default("tokyo-night"))).unwrap();
            let log_index=state.output_rows().iter().position(|r|*r=="Log").unwrap();
            let toggle=buttons.button_rect(TuiButton::DiscBrowserToggle(log_index))
                .expect("log toggle must be visible when focused");
            assert_eq!(buttons.find_button_at(toggle.x,toggle.y),
                Some(TuiButton::DiscBrowserToggle(log_index)));
            let mut app=AppState::new_for_test(TonepoetConfig::default());
            app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
            handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserToggle(log_index),1,&tx);
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s) if s.extraction_log));
            handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Char(' '),M::NONE),&tx);
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s) if !s.extraction_log));
            if let ActiveOverlay::DiscBrowser(s)=&mut app.active_overlay {focus_native_row(s,"Overwrite");}
            handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Enter,M::NONE),&tx);
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s) if s.overwrite_open));
            handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Down,M::NONE),&tx);
            handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Enter,M::NONE),&tx);
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
                if !s.overwrite_open && s.overwrite_policy==
                    crate::convert::pipeline::native_extract::NativeOverwritePolicy::Skip));
            // The old Destination letter commands do not silently change values.
            handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Char('o'),M::NONE),&tx);
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
                if s.overwrite_policy==crate::convert::pipeline::native_extract::NativeOverwritePolicy::Skip));
        }
    }

    #[test]
    fn r22_log_initial_setting_tracks_convert_without_modifying_preference() {
        let path = PathBuf::from("/tmp/native-extract-log-preference.iso");
        let contents = selectable_disc_contents(path.clone(), FolderDiscMarkerKind::Sacd);
        let mut config = TonepoetConfig::default();
        config.conversion.write_log_file = true;
        let mut app = AppState::new_for_test(config);
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        open_disc_browser_from_contents(&mut app, contents, path, &tx);
        assert!(matches!(&app.active_overlay, ActiveOverlay::DiscBrowser(state)
            if state.extraction_log));
        let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay else { unreachable!() };
        focus_native_row(state, "Log");
        handle_disc_browser_key(&mut app, KeyEvent::new(KeyCode::Char(' '),
            crossterm::event::KeyModifiers::NONE), &tx);
        assert!(matches!(&app.active_overlay, ActiveOverlay::DiscBrowser(state)
            if !state.extraction_log));
        assert!(app.config.conversion.write_log_file,
            "per-extraction changes must not alter Convert's setting");
    }

    #[test]
    fn r22_invalid_folder_edit_remains_open_until_corrected_or_cancelled() {
        let path = PathBuf::from("/tmp/native-extract-destination.iso");
        let contents = selectable_disc_contents(path.clone(), FolderDiscMarkerKind::Sacd);
        let mut state = DiscBrowserState::new(contents, path);
        focus_native_row(&mut state, "Folder");
        begin_native_folder_edit(&mut state);
        let original = state.destination.clone();
        state.destination_input = "relative/folder".into();
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        handle_disc_browser_key(&mut app, KeyEvent::new(KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE), &tx);
        assert!(matches!(&app.active_overlay, ActiveOverlay::DiscBrowser(s)
            if s.editing_destination && s.destination == original));
        handle_disc_browser_key(&mut app, KeyEvent::new(KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE), &tx);
        assert!(matches!(&app.active_overlay, ActiveOverlay::DiscBrowser(s)
            if !s.editing_destination && s.destination == original));
    }

    #[test]
    fn r22_dropdown_cancel_and_radio_selection_preserve_state() {
        use crate::convert::pipeline::native_extract::NativeOverwritePolicy as P;
        use crossterm::event::KeyModifiers as M;
        let contents=selectable_disc_contents(PathBuf::from("disc.iso"),FolderDiscMarkerKind::Sacd);
        let mut state=DiscBrowserState::new(contents,PathBuf::from("disc.iso"));
        focus_native_row(&mut state,"Overwrite");
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Enter,M::NONE),&tx);
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Down,M::NONE),&tx);
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Esc,M::NONE),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if !s.overwrite_open && s.overwrite_policy==P::Fail));
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Char(']'),M::NONE),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s) if s.overwrite_policy==P::Skip));
        // One click on a radio option changes only the Files choice.
        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserFilesChoice(1),1,&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s) if s.file_per_program));
        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserFilesChoice(0),1,&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s) if !s.file_per_program));
        // A uniquely matching folder segment completes on Tab; ambiguous paths do not.
        let tmp=tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("Extracted" )).unwrap();
        let mut text=tmp.path().join("Extr").to_string_lossy().into_owned();
        complete_native_folder_segment(&mut text);
        assert_eq!(PathBuf::from(text),tmp.path().join("Extracted"));
    }

    #[test]
    fn r32d01_unfocused_first_click_keeps_select_all_on_mouse_release() {
        let contents=selectable_disc_contents(PathBuf::from("disc.iso"),FolderDiscMarkerKind::Sacd);
        let mut state=DiscBrowserState::new(contents,PathBuf::from("disc.iso"));
        begin_native_folder_edit(&mut state);
        native_text_mouse_down(&mut state,12,2,false,1,None);
        assert!(state.text_edit.as_ref().unwrap().select_all);
        native_text_mouse_drag(&mut state,2,true);
        assert!(state.text_edit.as_ref().unwrap().select_all);
    }

    #[test]
    fn r32d01_naming_drag_keeps_initial_scrolled_window() {
        let contents=selectable_disc_contents(PathBuf::from("disc.iso"),FolderDiscMarkerKind::Sacd);
        let mut state=DiscBrowserState::new(contents,PathBuf::from("disc.iso"));
        state.naming_template="%ARTIST%/%ALBUM%/%DISC%/%TRACKNN% - %TITLE%".to_owned();
        focus_native_row(&mut state,"Naming");
        begin_native_template_edit(&mut state);
        let width=12usize;
        let (start,_)=state.text_edit.as_ref().unwrap().view_range(width);
        let anchor=state.text_edit.as_ref().unwrap().byte_index_for_view_column(width,2);
        native_text_mouse_down(&mut state,width,2,true,1,None);
        let frozen=state.text_edit.as_ref().unwrap().view(width).0;
        assert_eq!(state.text_edit.as_ref().unwrap().cursor,anchor);
        native_text_mouse_drag(&mut state,8,false);
        let input=state.text_edit.as_ref().unwrap();
        assert_eq!(input.view_range(width).0.start,start.start);
        assert_eq!(input.view(width).0,frozen);
        assert_eq!(input.selection_range(),Some(anchor..start.start+8));
        native_text_mouse_drag(&mut state,5,true);
        assert_eq!(state.text_edit.as_ref().unwrap().selection_range(),
            Some(anchor..start.start+5));
    }

    #[test]
    fn r32d01_abbreviated_folder_drag_keeps_mapped_anchor_and_revealed_view() {
        use crate::tui::disc_extract_render::native_folder_preview_caret;
        let contents=selectable_disc_contents(PathBuf::from("disc.iso"),FolderDiscMarkerKind::Sacd);
        let mut state=DiscBrowserState::new(contents,PathBuf::from("disc.iso"));
        state.destination=PathBuf::from("/archives/huge_hidden_collection/more_hidden/Composer/Album/Edition");
        focus_native_row(&mut state,"Folder");
        begin_native_folder_edit(&mut state);
        let width=23usize;
        let column=7usize;
        let mapped_char=native_folder_preview_caret(&state.destination,width as u16,column);
        let expected_anchor=native_edit_byte_index(&state.destination_input,mapped_char);
        native_text_mouse_down(&mut state,width,column,true,1,Some(mapped_char));
        let input=state.text_edit.as_ref().unwrap();
        let frozen=input.view(width).0;
        let frozen_start=input.view_range(width).0.start;
        assert_eq!(input.cursor,expected_anchor);
        assert_eq!(input.selection_anchor,Some(expected_anchor));
        native_text_mouse_drag(&mut state,column+5,false);
        let input=state.text_edit.as_ref().unwrap();
        assert_eq!(input.view_range(width).0.start,frozen_start);
        assert_eq!(input.view(width).0,frozen);
        assert_eq!(input.selection_range().unwrap().start,expected_anchor);
        native_text_mouse_drag(&mut state,column+3,true);
        assert_eq!(state.text_edit.as_ref().unwrap().selection_range().unwrap().start,
            expected_anchor);
    }

    #[test]
    fn r32_mouse_folder_selects_word_and_browse_pill_opens_picker() {
        use crossterm::event::{MouseEvent,MouseEventKind,MouseButton,KeyModifiers as M};
        use ratatui::layout::Rect;
        let temp=tempfile::tempdir().unwrap();
        let disc=temp.path().join("disc.iso");
        std::fs::write(&disc,b"ISO").unwrap();
        let contents=selectable_disc_contents(disc.clone(),FolderDiscMarkerKind::Sacd);
        let state=DiscBrowserState::new(contents,disc);
        let original=state.destination.clone();
        let log=state.output_rows().iter().position(|row| *row=="Log").unwrap();
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        app.button_map.record_button(TuiButton::DiscBrowserFolderField,Rect::new(8,12,38,1));
        app.button_map.record_button(TuiButton::DiscBrowserFolderBrowse,Rect::new(48,12,7,1));
        app.button_map.record_button(TuiButton::DiscBrowserToggle(log),Rect::new(55,13,20,1));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        let click=|x,y| MouseEvent{kind:MouseEventKind::Down(MouseButton::Left),
            column:x,row:y,modifiers:M::NONE};
        // Go through the actual mouse dispatcher, not the action's click_count parameter.
        crate::tui::keybindings::handle_mouse(&mut app,click(12,12),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s) if s.editing_destination));
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Char('X'),M::NONE),&tx);
        crate::tui::keybindings::handle_mouse(&mut app,click(56,13),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if !s.editing_destination && s.current_output_row()=="Log"
                && s.extraction_log && s.destination==original));
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Char(' '),M::NONE),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if !s.extraction_log && s.destination==original));
        app.double_click.clear();
        crate::tui::keybindings::handle_mouse(&mut app,click(12,12),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.text_edit.as_ref().is_some_and(|input| input.select_all)));
        crate::tui::keybindings::handle_mouse(&mut app,click(12,12),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.text_edit.as_ref().and_then(|input| input.selection_range()).is_some()),
            "double-click selects the word, not the directory picker");
        crate::tui::keybindings::handle_mouse(&mut app,click(50,12),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::FilePicker(_)),
            "the Browse pill still opens the existing directory picker");
        assert!(app.pending_disc_extract_picker.as_ref().is_some_and(|s|
            !s.editing_destination && s.destination==original));
        let (session_id,purpose)=match &app.active_overlay {
            ActiveOverlay::FilePicker(picker)=>(picker.session_id,picker.purpose.clone()),
            other=>panic!("expected picker: {other:?}"),
        };
        crate::tui::event_loop::handle_message(&mut app,AppMessage::FilePickerComplete{
            session_id,purpose,path:None,paths:vec![],ignored_directories:0,
        },&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if !s.editing_destination && s.destination==original));
    }

    #[test]
    fn r23_folder_mouse_caret_and_keyboard_edit_use_unicode_positions() {
        use crossterm::event::KeyModifiers as M;
        let temp=tempfile::tempdir().unwrap();
        let disc=temp.path().join("disc.iso");
        let contents=selectable_disc_contents(disc.clone(),FolderDiscMarkerKind::Sacd);
        let mut state=DiscBrowserState::new(contents,disc);
        state.destination=temp.path().join("Música").join("Album");
        focus_native_row(&mut state,"Folder");
        begin_native_folder_edit(&mut state);
        native_text_mouse_down(&mut state,80,4,true,1,None);
        let original=state.destination_input.clone();
        let caret=state.destination_caret;
        assert!(caret<original.chars().count());
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Char('Z'),M::NONE),&tx);
        let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else {panic!("not editing")};
        let mut expected=original;
        let inserted_at=native_edit_byte_index(&expected,caret);
        expected.insert(inserted_at,'Z');
        assert_eq!(state.destination_input,expected);
        assert_eq!(state.destination_caret,caret+1);
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Left,M::NONE),&tx);
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Delete,M::NONE),&tx);
        let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else {panic!("not editing")};
        expected.remove(inserted_at);
        assert_eq!(state.destination_input,expected);
    }

    #[test]
    fn r23_wide_arrow_navigation_is_spatial_and_tab_reaches_footer() {
        use crossterm::event::KeyModifiers as M;
        use crate::tui::disc_browser::ExtractFocus;
        let contents=selectable_disc_contents(PathBuf::from("disc.iso"),FolderDiscMarkerKind::Sacd);
        let mut state=DiscBrowserState::new(contents,PathBuf::from("disc.iso"));
        state.form_wide.set(true);
        focus_native_row(&mut state,"Format");
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        let press=|app:&mut AppState, code|handle_disc_browser_key(app,KeyEvent::new(code,M::NONE),&tx);
        press(&mut app,KeyCode::Down);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s) if s.current_output_row()=="Files"));
        press(&mut app,KeyCode::Right);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s) if s.current_output_row()=="Tags"));
        press(&mut app,KeyCode::Up);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s) if s.current_output_row()=="Overwrite"));
        press(&mut app,KeyCode::Down);
        press(&mut app,KeyCode::Down);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s) if s.current_output_row()=="Log"));
        press(&mut app,KeyCode::Down);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s) if s.current_output_row()=="Folder"));
        press(&mut app,KeyCode::Tab);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s) if s.focus==ExtractFocus::Footer));
        press(&mut app,KeyCode::Right);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s) if s.footer_cursor==1));
        press(&mut app,KeyCode::BackTab);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s) if s.focus==ExtractFocus::Output));
        if let ActiveOverlay::DiscBrowser(s)=&mut app.active_overlay {
            s.form_wide.set(false);
            focus_native_row(s,"Format");
        }
        press(&mut app,KeyCode::Right);
        press(&mut app,KeyCode::Left);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.current_output_row()=="Format"),"compact Left/Right have no vertical effect");
        press(&mut app,KeyCode::Down);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.current_output_row()=="Overwrite"),"compact form uses Up/Down order");
    }

    #[test]
    fn r23_dst_only_tag_toggle_is_disabled_but_mixed_dsf_can_transfer() {
        let contents=selectable_disc_contents(PathBuf::from("disc.iso"),FolderDiscMarkerKind::Sacd);
        let mut state=DiscBrowserState::new(contents,PathBuf::from("disc.iso"));
        state.contents.presentations[0].format.codec=Some("DST".into());
        state.keep_dst[0]=true;
        assert_eq!(state.selected_sacd_tag_carriers(),(false,true));
        assert!(!state.native_tags_available());
        focus_native_row(&mut state,"Tags");
        assert_eq!(state.current_output_row(),"Tags",
            "DFF-only Tags must accept focus for the Use DSF action");
        toggle_native_form_row(&mut state);
        assert!(state.tags,"disabled DFF toggle leaves a future DSF preference untouched");
        assert!(!state.native_job(PathBuf::from("/tmp")).transfer_tags);
        let other=state.contents.presentations[0].clone();
        state.contents.presentations.push(other);
        state.selected.push(true);
        state.items_checked.push(vec![true]);
        state.keep_dst.push(false);
        assert_eq!(state.selected_sacd_tag_carriers(),(true,true));
        assert!(state.native_tags_available());
        assert!(state.native_job(PathBuf::from("/tmp")).transfer_tags);
    }

    #[test]
    fn r24_extract_button_and_ctrl_x_preserve_uncommitted_folder_and_naming() {
        use crossterm::event::KeyModifiers as M;
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.iso");
        let state = DiscBrowserState::new(
            selectable_disc_contents(source.clone(),FolderDiscMarkerKind::Sacd),source);
        let old_destination = state.destination.clone();
        let old_naming = state.naming_template.clone();
        let chosen = temp.path().join("new extraction folder");
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay = ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,mut rx) = tokio::sync::mpsc::channel(8);
        let press = |app:&mut AppState, code, modifiers|
            handle_disc_browser_key(app,KeyEvent::new(code,modifiers),&tx);
        for (draft, valid) in [(chosen.to_string_lossy().into_owned(),true),
            ("../relative/not-allowed".into(),false)] {
            handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserFolderField,1,&tx);
            if let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay {
                state.destination_input = draft.clone();
                state.destination_caret = draft.chars().count();
            }
            r24_paint_folder(&mut app,154,40);
            let extract=app.button_map.button_rect(TuiButton::DiscBrowserExtract)
                .expect("enabled Extract button hitbox");
            crate::tui::keybindings::handle_mouse(&mut app,
                crossterm::event::MouseEvent {
                    kind:crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                    column:extract.x+2,row:extract.y,modifiers:M::NONE,
                },&tx);
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
                if state.editing_destination && state.destination_input==draft
                    && state.destination==old_destination));
            assert!(app.status_message.as_ref().is_some_and(|(text,_)|
                text.contains("Finish editing Folder")));
            // Round 4 routes Ctrl+X to the active shared text editor.
            // With no selection it cuts the field, rather than extracting.
            crate::tui::text_input::with_scoped_shared_text_clipboard("", || {
                press(&mut app,KeyCode::Char('x'),M::CONTROL);
                assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
                    if state.editing_destination && state.destination_input.is_empty()
                        && state.destination==old_destination));
                assert_eq!(crate::tui::text_input::read_shared_text_clipboard(),draft);
            });
            assert!(matches!(rx.try_recv(),Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
            press(&mut app,KeyCode::Char('z'),M::CONTROL);
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
                if state.editing_destination && state.destination_input==draft
                    && state.destination==old_destination));
            press(&mut app,KeyCode::Enter,M::NONE);
            if valid {
                assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
                    if !state.editing_destination && state.destination==chosen));
                // Reset for the second draft; this changes no destination on disk.
                if let ActiveOverlay::DiscBrowser(state)=&mut app.active_overlay {
                    state.destination=old_destination.clone();
                }
            } else {
                assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
                    if state.editing_destination && state.destination_input==draft
                        && state.destination==old_destination));
                handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserExtract,1,&tx);
                assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
                    if state.editing_destination && state.destination==old_destination));
                press(&mut app,KeyCode::Esc,M::NONE);
                assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
                    if !state.editing_destination && state.destination==old_destination));
            }
        }
        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserNaming,1,&tx);
        if let ActiveOverlay::DiscBrowser(state)=&mut app.active_overlay {
            state.destination_input="{album}-{track}".into();
        }
        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserExtract,1,&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
            if state.editing_destination && state.editing_template
                && state.naming_template==old_naming));
        assert!(app.status_message.as_ref().is_some_and(|(text,_)|
            text.contains("Finish editing Naming")));
        press(&mut app,KeyCode::Esc,M::NONE);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
            if !state.editing_destination && state.naming_template==old_naming));
    }

    /// Paint the real popup before every mouse event. Its button map and
    /// displayed path text, not a fabricated field coordinate, drive the test.
    fn r24_paint_folder(app: &mut AppState, width:u16, height:u16)
        -> (ratatui::layout::Rect, String) {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        use crate::tui::button_map::ButtonRenderMap;
        let mut terminal = Terminal::new(TestBackend::new(width,height)).unwrap();
        let mut buttons = ButtonRenderMap::new();
        let theme = app.theme;
        let scratch = std::env::temp_dir();
        terminal.draw(|frame| {
            let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else {panic!("not browser")};
            crate::tui::disc_extract_render::draw(frame,state,
                &scratch,&mut buttons,theme);
        }).unwrap();
        let field=buttons.button_rect(TuiButton::DiscBrowserFolderField)
            .expect("folder field must render");
        let row=(field.x..field.right()).map(|x|
            terminal.backend().buffer().get(x,field.y).symbol().to_owned())
            .collect::<Vec<_>>().concat();
        app.button_map=buttons;
        (field,row)
    }

    /// Read the actual rendered field and its registered hitbox before a
    /// pointer gesture, including when the field does not have form focus.
    fn r32c_paint_field(app: &mut AppState, target: TuiButton,
        width: u16, height: u16) -> (ratatui::layout::Rect, String) {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        use crate::tui::button_map::ButtonRenderMap;
        let mut terminal=Terminal::new(TestBackend::new(width,height)).unwrap();
        let mut buttons=ButtonRenderMap::new();
        let theme=app.theme;
        let scratch=std::env::temp_dir();
        terminal.draw(|frame| {
            let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else {panic!("not browser")};
            crate::tui::disc_extract_render::draw(frame,state,&scratch,&mut buttons,theme);
        }).unwrap();
        let field=buttons.button_rect(target).expect("text field must render");
        let row=(field.x..field.right()).map(|x|
            terminal.backend().buffer().get(x,field.y).symbol().to_owned())
            .collect::<Vec<_>>().concat();
        app.button_map=buttons;
        (field,row)
    }

    #[test]
    fn r32c_unfocused_first_drag_uses_compacted_folder_preview() {
        use crossterm::event::{MouseButton,MouseEvent,MouseEventKind,KeyModifiers as M};
        let source=PathBuf::from("disc.iso");
        let mut state=DiscBrowserState::new(
            selectable_disc_contents(source.clone(),FolderDiscMarkerKind::Sacd),source);
        let path=PathBuf::from("/music/a-very-long-hidden-directory/another-hidden-directory/Album/Extracted");
        state.destination=path.clone();
        state.focus=crate::tui::disc_browser::ExtractFocus::Programs;
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        let (field,row)=r32c_paint_field(&mut app,TuiButton::DiscBrowserFolderField,154,40);
        assert!(row.contains('…') && row.contains("Album"),"preview: {row:?}");
        let start_col=row[..row.find("Album").unwrap()].chars().count();
        let down=MouseEvent {kind:MouseEventKind::Down(MouseButton::Left),
            column:field.x+start_col as u16,row:field.y,modifiers:M::NONE};
        crate::tui::keybindings::handle_mouse(&mut app,down,&tx);
        let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else {panic!("browser")};
        assert!(state.text_edit.as_ref().unwrap().select_all,
            "an unfocused first click must keep whole-field selection");
        let original_anchor=state.text_edit.as_ref().unwrap().cursor;
        assert_eq!(original_anchor,path.to_string_lossy().find("Album").unwrap());
        let original_view=state.text_edit.as_ref().unwrap().view(field.width as usize).0;
        let (edit_field,_)=r32c_paint_field(&mut app,TuiButton::DiscBrowserFolderField,154,40);
        assert_eq!(edit_field,field);
        crate::tui::keybindings::handle_mouse(&mut app,MouseEvent {
            kind:MouseEventKind::Drag(MouseButton::Left),
            column:down.column+3,row:down.row,modifiers:M::NONE},&tx);
        let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else {panic!("browser")};
        let input=state.text_edit.as_ref().unwrap();
        assert_eq!(input.view(field.width as usize).0,original_view,
            "the first drag must not change the clicked text window");
        let selected=input.selection_range().expect("first drag selects a substring");
        assert_eq!(selected.start,original_anchor);
        assert_eq!(&input.text[selected],"Alb");
        crate::tui::keybindings::handle_mouse(&mut app,MouseEvent {
            kind:MouseEventKind::Up(MouseButton::Left),
            column:down.column+3,row:down.row,modifiers:M::NONE},&tx);
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Char('Z'),M::NONE),&tx);
        let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else {panic!("browser")};
        assert_eq!(state.destination_input,
            path.to_string_lossy().replacen("Album","Zum",1));
    }

    #[test]
    fn r32c_unfocused_click_without_drag_still_selects_all() {
        use crossterm::event::{MouseButton,MouseEvent,MouseEventKind,KeyModifiers as M};
        let source=PathBuf::from("disc.iso");
        let mut state=DiscBrowserState::new(
            selectable_disc_contents(source.clone(),FolderDiscMarkerKind::Sacd),source);
        state.naming_template="%ALBUM%/%ARTIST%/%TRACKNN% - %TITLE%".into();
        state.focus=crate::tui::disc_browser::ExtractFocus::Programs;
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        let (field,_)=r32c_paint_field(&mut app,TuiButton::DiscBrowserNaming,154,40);
        let press=MouseEvent{kind:MouseEventKind::Down(MouseButton::Left),
            column:field.x+2,row:field.y,modifiers:M::NONE};
        crate::tui::keybindings::handle_mouse(&mut app,press,&tx);
        crate::tui::keybindings::handle_mouse(&mut app,MouseEvent{
            kind:MouseEventKind::Up(MouseButton::Left),
            column:field.x+2,row:field.y,modifiers:M::NONE},&tx);
        let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else {panic!("browser")};
        assert!(state.text_edit.as_ref().unwrap().select_all);
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Char('X'),M::NONE),&tx);
        let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else {panic!("browser")};
        assert_eq!(state.destination_input,"X");
    }

    #[test]
    fn r32c_unfocused_first_drag_uses_naming_preview_prefix() {
        use crossterm::event::{MouseButton,MouseEvent,MouseEventKind,KeyModifiers as M};
        let source=PathBuf::from("disc.iso");
        let mut state=DiscBrowserState::new(
            selectable_disc_contents(source.clone(),FolderDiscMarkerKind::Sacd),source);
        let template="%ALBUM%/%ARTIST%/%TRACKNN% - %TITLE%/additional-long-template-suffix";
        state.naming_template=template.into();
        state.focus=crate::tui::disc_browser::ExtractFocus::Programs;
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        let (field,row)=r32c_paint_field(&mut app,TuiButton::DiscBrowserNaming,154,40);
        assert!(row.contains("ALBUM") && row.contains('…'),"preview: {row:?}");
        let column=row[..row.find("ALBUM").unwrap()].chars().count();
        let down=MouseEvent {kind:MouseEventKind::Down(MouseButton::Left),
            column:field.x+column as u16,row:field.y,modifiers:M::NONE};
        crate::tui::keybindings::handle_mouse(&mut app,down,&tx);
        let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else {panic!("browser")};
        let input=state.text_edit.as_ref().unwrap();
        assert!(state.editing_template && input.select_all);
        assert_eq!(input.cursor,1);
        let original_view=input.view(field.width as usize).0;
        r32c_paint_field(&mut app,TuiButton::DiscBrowserNaming,154,40);
        crate::tui::keybindings::handle_mouse(&mut app,MouseEvent {
            kind:MouseEventKind::Drag(MouseButton::Left),
            column:down.column+5,row:down.row,modifiers:M::NONE},&tx);
        let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else {panic!("browser")};
        let input=state.text_edit.as_ref().unwrap();
        assert_eq!(input.view(field.width as usize).0,original_view);
        assert_eq!(&input.text[input.selection_range().expect("Naming drag")],"ALBUM");
        crate::tui::keybindings::handle_mouse(&mut app,MouseEvent {
            kind:MouseEventKind::Up(MouseButton::Left),
            column:down.column+5,row:down.row,modifiers:M::NONE},&tx);
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Char('X'),M::NONE),&tx);
        let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else {panic!("browser")};
        assert_eq!(state.destination_input,template.replacen("ALBUM","X",1));
    }

    #[test]
    fn r32_compacted_home_preview_opens_full_editor_and_supports_mouse_caret() {
        use crossterm::event::{MouseEvent,MouseEventKind,MouseButton,KeyModifiers as M};
        let home=std::env::var_os("HOME").or_else(||std::env::var_os("USERPROFILE"))
            .map(PathBuf::from).expect("home directory for path abbreviation");
        let source=PathBuf::from("disc.iso");
        let mut state=DiscBrowserState::new(
            selectable_disc_contents(source.clone(),FolderDiscMarkerKind::Sacd),source);
        state.focus=crate::tui::disc_browser::ExtractFocus::Output;
        state.output_cursor=state.output_rows().iter().position(|row|*row=="Folder").unwrap();
        state.focus=crate::tui::disc_browser::ExtractFocus::Programs;
        state.destination=home.join("Music").join("Out");
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        let click=|column,row|MouseEvent{kind:MouseEventKind::Down(MouseButton::Left),
            column,row,modifiers:M::NONE};
        let press=|app:&mut AppState,code|handle_disc_browser_key(app,
            KeyEvent::new(code,M::NONE),&tx);
        let (field,row)=r24_paint_folder(&mut app,154,40);
        assert!(row.starts_with("~/") && row.contains("Music") && row.contains("Out"),"actual field: {row:?}");
        let x=field.x+row[..row.find("Music").unwrap()].chars().count() as u16;
        crate::tui::keybindings::handle_mouse(&mut app,click(x,field.y),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.text_edit.as_ref().is_some_and(|input|input.select_all)));
        app.double_click.clear();
        let (field,row)=r24_paint_folder(&mut app,154,40);
        let x=field.x+row[..row.find("Music").unwrap()].chars().count() as u16;
        crate::tui::keybindings::handle_mouse(&mut app,click(x,field.y),&tx);
        press(&mut app,KeyCode::Char('X'));
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
            if state.destination_input.as_str()==home.join("XMusic").join("Out").to_string_lossy().as_ref()));
        press(&mut app,KeyCode::Backspace);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
            if state.destination_input.as_str()==home.join("Music").join("Out").to_string_lossy().as_ref()));
        press(&mut app,KeyCode::Esc);

        let long=home.join("a-very-long-hidden-component-for-the-mouse-test")
            .join("another-very-long-hidden-component")
            .join("AlbumName").join("Extracted");
        if let ActiveOverlay::DiscBrowser(state)=&mut app.active_overlay {
            state.destination=long.clone();
            state.focus=crate::tui::disc_browser::ExtractFocus::Programs;
        }
        let (field,row)=r24_paint_folder(&mut app,154,40);
        assert!(row.contains('…') && row.contains("Extracted"),"collapsed field: {row:?}");
        let x=field.x+row[..row.find("Extracted").unwrap()].chars().count() as u16;
        crate::tui::keybindings::handle_mouse(&mut app,click(x,field.y),&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.text_edit.as_ref().is_some_and(|input|input.select_all)));
        app.double_click.clear();
        let (field,row)=r24_paint_folder(&mut app,154,40);
        let x=field.x+row[..row.find("Extracted").unwrap()].chars().count() as u16;
        crate::tui::keybindings::handle_mouse(&mut app,click(x,field.y),&tx);
        press(&mut app,KeyCode::Char('X'));
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
            if state.destination_input.as_str()==long.with_file_name("XExtracted").to_string_lossy().as_ref()));
        press(&mut app,KeyCode::Backspace);
        let (field,row)=r24_paint_folder(&mut app,154,40);
        assert!(row.contains("Extracted"),"editor must show the clicked component: {row:?}");
        let x=field.x+row[..row.find("racted").unwrap()].chars().count() as u16;
        crate::tui::keybindings::handle_mouse(&mut app,click(x,field.y),&tx);
        press(&mut app,KeyCode::Delete);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
            if state.destination_input.as_str()==long.with_file_name("Extacted").to_string_lossy().as_ref()));
        press(&mut app,KeyCode::Char('r'));
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
            if state.destination_input.as_str()==long.to_string_lossy().as_ref()));
    }

    #[test]
    fn r30_spatial_navigation_reaches_tags_and_keep_dst_at_all_widths() {
        use crate::tui::disc_browser::ExtractFocus;
        use crossterm::event::KeyModifiers as M;
        let source=PathBuf::from("disc.iso");
        let mut state=DiscBrowserState::new(
            selectable_disc_contents(source.clone(),FolderDiscMarkerKind::Sacd),source);
        state.contents.presentations[0].format.codec=Some("DST".into());
        state.keep_dst[0]=true;
        state.focus=ExtractFocus::Output;
        assert!(!state.native_tags_available());
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        let press=|app:&mut AppState,code|handle_disc_browser_key(app,
            KeyEvent::new(code,M::NONE),&tx);
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        for (width,height,wide) in [(110,34,false),(136,35,false),(154,40,true)] {
            if let ActiveOverlay::DiscBrowser(state)=&mut app.active_overlay {
                focus_native_row(state,"Format");
            }
            r24_paint_folder(&mut app,width,height);
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
                if state.form_wide.get()==wide));
            press(&mut app,KeyCode::Right);
            let expected=if wide {"Overwrite"}else{"Format"};
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
                if state.current_output_row()==expected));
            press(&mut app,KeyCode::Down);
            let expected=if wide {"Keep DST"}else{"Overwrite"};
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
                if state.current_output_row()==expected));
            if let ActiveOverlay::DiscBrowser(state)=&mut app.active_overlay {
                focus_native_row(state,"Files");
            }
            // Paired wide controls use Left/Right; compact rows are stacked
            // and use Up/Down, including when DFF suppresses tag writing.
            press(&mut app,if wide {KeyCode::Right}else{KeyCode::Down});
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
                if state.current_output_row()=="Tags"),"Tags must remain keyboard accessible for Use DSF");
        }
        if let ActiveOverlay::DiscBrowser(state)=&mut app.active_overlay {
            state.keep_dst[0]=false;
            focus_native_row(state,"Files");
        }
        press(&mut app,KeyCode::Right);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
            if state.current_output_row()=="Tags"),"DSF still permits Tags focus");
        if let ActiveOverlay::DiscBrowser(state)=&mut app.active_overlay {
            let other=state.contents.presentations[0].clone();
            state.contents.presentations.push(other);
            state.selected.push(true);
            state.items_checked.push(vec![true]);
            state.keep_dst.push(true);
            focus_native_row(state,"Files");
        }
        press(&mut app,KeyCode::Right);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
            if state.current_output_row()=="Tags"),"mixed DSF/DFF retains Tags focus");
        // A live transition to DFF-only cannot strand keyboard focus on Tags.
        if let ActiveOverlay::DiscBrowser(state)=&mut app.active_overlay {
            state.keep_dst[0]=true;
        }
        press(&mut app,KeyCode::Down);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
            if state.current_output_row()!="Tags"));
    }

    #[test]
    fn r24_output_cue_form_and_footer_help_follow_real_bindings() {
        use crate::tui::disc_browser::ExtractFocus;
        use crossterm::event::KeyModifiers as M;
        let source=PathBuf::from("disc.iso");
        let mut state=DiscBrowserState::new(
            selectable_disc_contents(source.clone(),FolderDiscMarkerKind::DvdAudio),source);
        state.file_per_program=true;
        state.focus=ExtractFocus::Output;
        assert!(state.output_rows().contains(&"CUE"));
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        let press=|app:&mut AppState,code|handle_disc_browser_key(app,
            KeyEvent::new(code,M::NONE),&tx);
        for (width,height,wide) in [(110,34,false),(132,34,false),(154,40,true)] {
            if let ActiveOverlay::DiscBrowser(state)=&mut app.active_overlay {
                focus_native_row(state,"CUE");
            }
            r24_paint_folder(&mut app,width,height);
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
                if state.form_wide.get()==wide));
            press(&mut app,KeyCode::Right);
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(state)
                if state.current_output_row()==(if wide {"Log"}else{"CUE"})));
            if let ActiveOverlay::DiscBrowser(state)=&mut app.active_overlay {
                state.focus=ExtractFocus::Footer;
            }
            let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else {panic!("not browser")};
            let bindings=native_key_bindings(state);
            assert!(!bindings.iter().any(|item|matches!(item.action,
                NativeKeyAction::PageUp | NativeKeyAction::PageDown)));
            let help=native_key_help(state).join("\n");
            let footer=help.split("Footer buttons:").nth(1).unwrap()
                .split("Dropdown:").next().unwrap();
            assert!(!footer.contains("PgUp/PgDn page"),"{footer}");
            for focus in [ExtractFocus::Programs,ExtractFocus::Items,ExtractFocus::Output] {
                let mut context=state.clone();
                context.focus=focus;
                assert!(native_key_bindings(&context).iter().any(|binding|
                    binding.action==NativeKeyAction::PageDown));
            }
        }
    }

    fn r25_click_hint(app: &mut AppState, index: usize,
        tx: &tokio::sync::mpsc::Sender<AppMessage>) {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        r24_paint_folder(app,154,40);
        let rect=app.button_map.button_rect(TuiButton::DiscBrowserHint(index))
            .expect("advertised action must have a real clickable hint hitbox");
        crate::tui::keybindings::handle_mouse(app,MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x, row:rect.y, modifiers:KeyModifiers::NONE,
        },tx);
    }

    #[test]
    fn r25_editor_hints_reflect_modal_keys_and_never_discard_drafts() {
        use crossterm::event::KeyModifiers as M;
        let temp=tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("albums")).unwrap();
        let source=PathBuf::from("disc.iso");
        let state=DiscBrowserState::new(
            selectable_disc_contents(source.clone(),FolderDiscMarkerKind::Sacd),source);
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        let press=|app:&mut AppState,key| handle_disc_browser_key(app,KeyEvent::new(key,M::NONE),&tx);
        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserFolderField,1,&tx);
        let initial=if let ActiveOverlay::DiscBrowser(s)=&app.active_overlay {
            s.destination.clone()
        } else { panic!("no editor") };
        let typed=format!("{}/alb",temp.path().display());
        if let ActiveOverlay::DiscBrowser(s)=&mut app.active_overlay {
            s.destination_input=typed.clone();
            s.destination_caret=typed.chars().count();
            assert_eq!(native_key_hints(s),vec!["↵ Done","Esc Undo","Tab Complete"]);
            assert!(native_key_bindings(s).iter().all(|binding|
                binding.code!=KeyCode::Char('?')));
        }
        // ? is literal editor text, not an advertised Help action.
        press(&mut app,KeyCode::Char('?'));
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.destination_input==format!("{typed}?") && s.panel.is_none()));
        press(&mut app,KeyCode::Backspace);
        // Click the rendered Tab hint. This must complete the draft instead
        // of causing the global mouse-focus cleanup to cancel it.
        r25_click_hint(&mut app,2,&tx);
        let completed=temp.path().join("albums");
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.editing_destination && s.destination==initial
                && s.destination_input.as_str()==completed.to_string_lossy().as_ref()));
        r25_click_hint(&mut app,0,&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if !s.editing_destination && s.destination==completed));

        // An invalid folder still cannot be accepted by the clickable Done
        // hint. Undo is the one explicit way to discard that draft.
        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserFolderField,1,&tx);
        if let ActiveOverlay::DiscBrowser(s)=&mut app.active_overlay {
            s.destination_input="../not-absolute".into();
            s.destination_caret=s.destination_input.chars().count();
        }
        r25_click_hint(&mut app,0,&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.editing_destination && s.destination_input=="../not-absolute"
                && s.destination==completed));
        r25_click_hint(&mut app,1,&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if !s.editing_destination && s.destination==completed));

        // Naming uses Convert's template completion with its own disc palette.
        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserNaming,1,&tx);
        let old_name=if let ActiveOverlay::DiscBrowser(s)=&app.active_overlay {
            assert_eq!(native_key_hints(s),vec!["↵ Done","Esc Undo","Tab Complete"]);
            s.naming_template.clone()
        } else { panic!("no naming editor") };
        if let ActiveOverlay::DiscBrowser(s)=&mut app.active_overlay {
            s.destination_input="{album}-different".into();
        }
        r25_click_hint(&mut app,1,&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if !s.editing_destination && s.naming_template==old_name));
        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserNaming,1,&tx);
        if let ActiveOverlay::DiscBrowser(s)=&mut app.active_overlay {
            s.destination_input="{album}-accepted".into();
        }
        r25_click_hint(&mut app,0,&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if !s.editing_destination && s.naming_template=="{album}-accepted"));
    }

    #[test]
    fn r25_dropdown_hint_keys_match_keyboard_and_mouse() {
        use crossterm::event::KeyModifiers as M;
        use crate::convert::pipeline::native_extract::NativeOverwritePolicy as Policy;
        let source=PathBuf::from("disc.iso");
        let state=DiscBrowserState::new(
            selectable_disc_contents(source.clone(),FolderDiscMarkerKind::Sacd),source);
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        let press=|app:&mut AppState,key| handle_disc_browser_key(app,KeyEvent::new(key,M::NONE),&tx);
        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserOverwrite,1,&tx);
        if let ActiveOverlay::DiscBrowser(s)=&mut app.active_overlay {
            assert_eq!(native_key_hints(s),vec!["↑↓ move","↵ choose","Esc close"]);
            s.overwrite_cursor=2;
        }
        r25_click_hint(&mut app,0,&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.overwrite_open && s.overwrite_cursor==1));
        press(&mut app,KeyCode::Down);
        press(&mut app,KeyCode::Up);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.overwrite_open && s.overwrite_cursor==1));
        r25_click_hint(&mut app,1,&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if !s.overwrite_open && s.overwrite_policy==Policy::Skip));
        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserOverwrite,1,&tx);
        r25_click_hint(&mut app,2,&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if !s.overwrite_open && s.overwrite_policy==Policy::Skip));
        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserOverwrite,1,&tx);
        press(&mut app,KeyCode::Esc);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if !s.overwrite_open && s.overwrite_policy==Policy::Skip));
        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserOverwrite,1,&tx);
        press(&mut app,KeyCode::Down);
        press(&mut app,KeyCode::Enter);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if !s.overwrite_open && s.overwrite_policy==Policy::Replace));
    }

    /// Locate a character in the rendered terminal cell buffer, not by
    /// counting Unicode scalar values in a reconstructed line of symbols.
    fn r25_paint_and_locate(app:&mut AppState,width:u16,height:u16,
        marker:&str) -> (ratatui::layout::Rect,u16) {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        use crate::tui::button_map::ButtonRenderMap;
        let mut terminal=Terminal::new(TestBackend::new(width,height)).unwrap();
        let mut buttons=ButtonRenderMap::new();
        let scratch=std::env::temp_dir();
        let theme=app.theme;
        terminal.draw(|frame| {
            let ActiveOverlay::DiscBrowser(s)=&app.active_overlay else{panic!("no browser")};
            crate::tui::disc_extract_render::draw(frame,s,&scratch,&mut buttons,theme);
        }).unwrap();
        let field=buttons.button_rect(TuiButton::DiscBrowserFolderField).unwrap();
        let cell=(field.x..field.right()).find(|x| {
            marker.chars().enumerate().all(|(i,ch)|{
                let col=(*x).saturating_add(i as u16);
                col<field.right() && terminal.backend().buffer().get(col,field.y).symbol()
                    ==ch.to_string().as_str()
            })
        }).unwrap_or_else(||panic!("{marker:?} not visible in rendered Folder field"));
        app.button_map=buttons;
        (field,cell)
    }

    #[test]
    fn r25_wide_unicode_folder_clicks_track_rendered_terminal_cells() {
        use crossterm::event::{MouseButton,MouseEvent,MouseEventKind,KeyModifiers as M};
        let home=std::env::var_os("HOME").or_else(||std::env::var_os("USERPROFILE"))
            .map(PathBuf::from).expect("home for abbreviation");
        let source=PathBuf::from("disc.iso");
        let state=DiscBrowserState::new(
            selectable_disc_contents(source.clone(),FolderDiscMarkerKind::Sacd),source);
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        let press=|app:&mut AppState,key| handle_disc_browser_key(app,KeyEvent::new(key,M::NONE),&tx);
        let click=|column,row|MouseEvent {
            kind:MouseEventKind::Down(MouseButton::Left),column,row,modifiers:M::NONE,
        };
        let cases=[
            (home.join("Music").join("Extracted"),154,40),
            (home.join("Café").join("Extracted"),110,34),
            (home.join("音楽").join("Extracted"),154,40),
            (home.join("音楽").join("Extracted"),110,34),
            (home.join("🎵").join("Extracted"),110,34),
            (home.join("an-extra-long-hidden-parent-directory-for-testing")
                .join("音楽").join("Extracted"),110,34),
        ];
        for (path,width,height) in cases {
            if let ActiveOverlay::DiscBrowser(s)=&mut app.active_overlay {
                s.destination=path.clone();
                s.editing_destination=false;
                s.destination_input.clear();
                focus_native_row(s,"Folder");
                s.focus=crate::tui::disc_browser::ExtractFocus::Programs;
            }
            app.double_click.clear();
            let (field,x)=r25_paint_and_locate(&mut app,width,height,"Extracted");
            crate::tui::keybindings::handle_mouse(&mut app,click(x,field.y),&tx);
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
                if s.text_edit.as_ref().is_some_and(|input|input.select_all)));
            app.double_click.clear();
            let (field,x)=r25_paint_and_locate(&mut app,width,height,"Extracted");
            crate::tui::keybindings::handle_mouse(&mut app,click(x,field.y),&tx);
            press(&mut app,KeyCode::Char('X'));
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
                if s.destination_input.as_str()==path.with_file_name("XExtracted").to_string_lossy().as_ref()),
                "incorrect first insertion for {path:?} at {width}x{height}");
            press(&mut app,KeyCode::Backspace);
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
                if s.destination_input.as_str()==path.to_string_lossy().as_ref()));
            let (field,x)=r25_paint_and_locate(&mut app,width,height,"racted");
            crate::tui::keybindings::handle_mouse(&mut app,click(x,field.y),&tx);
            press(&mut app,KeyCode::Delete);
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
                if s.destination_input.as_str()==path.with_file_name("Extacted").to_string_lossy().as_ref()),
                "incorrect subsequent deletion for {path:?} at {width}x{height}");
            press(&mut app,KeyCode::Char('r'));
            assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
                if s.destination_input.as_str()==path.to_string_lossy().as_ref()));
            press(&mut app,KeyCode::Esc);
        }
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

    #[tokio::test]
    async fn classified_folders_activate_nested_iso_audio_streams_for_all_disc_markers() {
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

    #[test]
    fn r30_tags_sheet_uses_one_keyboard_mouse_reducer_and_keeps_old_metadata_preferences() {
        use crate::tui::disc_browser::{ExtractPanel, TagSheetControl as C};
        let contents = selectable_disc_contents("disc.iso".into(), FolderDiscMarkerKind::Sacd);
        let mut state = DiscBrowserState::new(contents, "disc.iso".into());
        state.tags = true;
        let defaults = state.tag_fields;
        open_native_tags_sheet(&mut state);
        assert_eq!(state.panel, Some(ExtractPanel::Tags));
        activate_native_tag_control(&mut state, C::Field(7));
        assert_eq!(state.tag_fields, defaults | (1 << 7));
        activate_native_tag_control(&mut state, C::None);
        assert_eq!(state.tag_fields, 0);
        activate_native_tag_control(&mut state, C::All);
        assert_eq!(state.tag_fields, 0x1ff);
        let artwork = state.tag_artwork;
        activate_native_tag_control(&mut state, C::Artwork);
        assert_ne!(state.tag_artwork, artwork);
        activate_native_tag_control(&mut state, C::Source);
        assert!(state.tag_source_open);
        set_native_tag_source(&mut state, 1);
        assert!(!state.tag_source_open);
        assert_eq!(state.tag_source, crate::convert::pipeline::native_extract::NativeTagSource::AuthoredTrack);
        state.sacd_dff = true;
        assert!(state.tag_sheet_controls().contains(&C::UseDsf));
        activate_native_tag_control(&mut state, C::UseDsf);
        assert!(!state.sacd_dff);
        activate_native_tag_control(&mut state, C::Transfer);
        assert!(!state.tags);
        assert_eq!(state.tag_sheet_controls(), vec![C::Transfer, C::EditValues, C::MusicBrainz, C::Done]);
        activate_native_tag_control(&mut state, C::Done);
        assert_eq!(state.panel, None);
        assert_eq!(state.tag_fields, 0x1ff, "closing the sheet keeps the selected fields");
    }

    #[test]
    fn r30_tags_sheet_mouse_click_outside_dismisses_without_underlying_action() {
        use crate::tui::disc_browser::TagSheetControl as C;
        let contents = selectable_disc_contents("disc.iso".into(), FolderDiscMarkerKind::Sacd);
        let mut app = AppState::new_for_test(TonepoetConfig::default());
        let mut state = DiscBrowserState::new(contents, "disc.iso".into());
        state.tags = true;
        open_native_tags_sheet(&mut state);
        app.active_overlay = ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let source_index = {
            let ActiveOverlay::DiscBrowser(state) = &app.active_overlay else { panic!("extract overlay"); };
            state.tag_sheet_controls().iter().position(|v| *v == C::Source).unwrap()
        };
        assert!(handle_disc_browser_button_click(&mut app,
            &TuiButton::DiscBrowserTagSheetControl(source_index), 1, &tx));
        let ActiveOverlay::DiscBrowser(state) = &app.active_overlay else { panic!("extract overlay"); };
        assert!(state.tag_source_open);
        assert!(handle_disc_browser_button_click(&mut app,
            &TuiButton::DiscBrowserTagSheetSourceChoice(1), 1, &tx));
        let ActiveOverlay::DiscBrowser(state) = &app.active_overlay else { panic!("extract overlay"); };
        assert_eq!(state.tag_source, crate::convert::pipeline::native_extract::NativeTagSource::AuthoredTrack);
        assert!(handle_disc_browser_button_click(&mut app,
            &TuiButton::DiscBrowserTagSheetOutside, 1, &tx));
        let ActiveOverlay::DiscBrowser(state) = &app.active_overlay else { panic!("extract overlay"); };
        assert_eq!(state.panel, None);
        assert_eq!(state.tag_source, crate::convert::pipeline::native_extract::NativeTagSource::AuthoredTrack);
    }

    #[test]
    fn r30c_extract_draft_returns_from_editor_and_saved_match_is_selectable() {
        use crate::disc::model::DiscExtractTagFields;
        use crate::convert::pipeline::native_extract::NativeTagSource;
        let source=PathBuf::from("/tmp/disc-A.iso");
        let other=PathBuf::from("/tmp/disc-B.iso");
        let contents=selectable_disc_contents(source.clone(),FolderDiscMarkerKind::Sacd);
        let mut draft=DiscBrowserState::new(contents,source.clone());
        draft.tags=true;
        draft.tag_source=NativeTagSource::AuthoredTrack;
        draft.tag_fields=0x1a5;
        draft.tag_artwork=false;
        draft.destination="/tmp/selected-destination".into();
        draft.naming_template="%ARTIST%/%TITLE%.%EXTENSION%".into();
        draft.sacd_dff=true;
        draft.contents.presentations[0].extract.authored_track_tags=vec![DiscExtractTagFields {
            title:Some("Disc title".into()),..Default::default()
        }];
        draft.contents.presentations[0].extract.sidecar_track_tags=vec![DiscExtractTagFields {
            title:Some("MusicBrainz title".into()),..Default::default()
        }];
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.current_screen=AppScreen::Browse;
        app.browse.entries=vec![
            BrowseEntry::new(source.clone(),"A".into(),EntryKind::Archive,0,None),
            BrowseEntry::new(other.clone(),"B".into(),EntryKind::Archive,0,None),
        ];
        app.browse.multi_selected=vec![source.clone(),other];
        app.browse.selected_index=1;
        let mut editor=crate::tui::app::MetadataEditorState::for_files(
            vec![source.clone()],Vec::new(),vec!["01".into()],Default::default());
        editor.return_to_extract=Some(Box::new(draft));
        editor.extract_mb_populated=true;
        editor.extract_mb_saved=true;
        let (tx,_rx)=tokio::sync::mpsc::channel(4);
        assert!(restore_extract_after_tag_editor(&mut app,&mut editor,&tx));
        let ActiveOverlay::DiscBrowser(state)=&mut app.active_overlay else{panic!("Extract draft lost")};
        assert_eq!(state.source_path,source,"Browse selection must not retarget Extract");
        assert_eq!(state.destination,PathBuf::from("/tmp/selected-destination"));
        assert_eq!(state.naming_template,"%ARTIST%/%TITLE%.%EXTENSION%");
        assert_eq!(state.tag_fields,0x1a5);
        assert_eq!(state.tag_source,NativeTagSource::AuthoredTrack);
        assert!(!state.tag_artwork);
        assert!(state.sacd_dff);
        assert!(state.tag_mb_available);
        assert_eq!(state.tag_source_option_count(),3);
        let authored=state.native_job("/tmp".into());
        assert_eq!(authored.programs[0].tracks[0].tags.title.as_deref(),Some("Disc title"));
        set_native_tag_source(state,2);
        assert_eq!(state.tag_source,NativeTagSource::MusicBrainz);
        let lookup=state.native_job("/tmp".into());
        assert_eq!(lookup.programs[0].tracks[0].tags.title.as_deref(),Some("MusicBrainz title"));
    }

    #[test]
    fn r30c_tag_action_pills_have_click_and_enter_parity() {
        use crate::tui::disc_browser::{ExtractPanel,TagSheetControl as C};
        use crossterm::event::KeyModifiers;
        let source=PathBuf::from("disc.iso");
        let contents=selectable_disc_contents(source.clone(),FolderDiscMarkerKind::Sacd);
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        let mut state=DiscBrowserState::new(contents,source);
        state.tags=true;
        state.sacd_dff=true;
        state.panel=Some(ExtractPanel::Tags);
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(4);
        let press=|app:&mut AppState,code| handle_disc_browser_key(app,
            KeyEvent::new(code,KeyModifiers::NONE),&tx);
        for control in [C::All,C::None,C::UseDsf] {
            let index={let ActiveOverlay::DiscBrowser(state)=&mut app.active_overlay else{panic!("browser")};
                // Both activation paths must begin with the same draft state.
                state.tag_fields=0x7f;
                state.sacd_dff=true;
                state.tag_sheet_controls().iter().position(|item|*item==control).unwrap()};
            assert!(handle_disc_browser_button_click(&mut app,
                &TuiButton::DiscBrowserTagSheetControl(index),1,&tx));
            let result_mouse={let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else{panic!("browser")};
                (state.tag_fields,state.sacd_dff)};
            if let ActiveOverlay::DiscBrowser(state)=&mut app.active_overlay {
                state.tag_fields=0x7f;
                if control==C::UseDsf {state.sacd_dff=true;}
                state.panel_cursor=index;
            }
            press(&mut app,KeyCode::Enter);
            let result_key={let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else{panic!("browser")};
                (state.tag_fields,state.sacd_dff)};
            assert_eq!(result_mouse,result_key,"{control:?} mouse vs keyboard");
        }
    }

    #[test]
    fn r32_click_on_already_focused_field_places_caret_without_selecting_all() {
        use crossterm::event::{MouseButton,MouseEvent,MouseEventKind,KeyModifiers as M};
        use ratatui::layout::Rect;
        let source=PathBuf::from("disc.iso");
        let mut state=DiscBrowserState::new(
            selectable_disc_contents(source.clone(),FolderDiscMarkerKind::Sacd),source);
        state.destination=PathBuf::from("/tmp/Album");
        focus_native_row(&mut state,"Folder");
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        app.button_map.record_button(TuiButton::DiscBrowserFolderField,Rect::new(8,12,32,1));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        crate::tui::keybindings::handle_mouse(&mut app,MouseEvent {
            kind:MouseEventKind::Down(MouseButton::Left),
            column:10,row:12,modifiers:M::NONE,
        },&tx);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.editing_destination && s.text_edit.as_ref().is_some_and(|i|
                !i.select_all && i.cursor==2 && i.selection_range().is_none())));
    }

    #[test]
    fn r32_folder_and_naming_share_edit_keys_and_do_not_invoke_form_accelerators() {
        use crossterm::event::KeyModifiers as M;
        let path=PathBuf::from("/tmp/r32-original-folder");
        let source=PathBuf::from("disc.iso");
        let mut state=DiscBrowserState::new(
            selectable_disc_contents(source.clone(),FolderDiscMarkerKind::Sacd),source);
        state.destination=path.clone();
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        let send=|app:&mut AppState,code,mods|handle_disc_browser_key(app,
            KeyEvent::new(code,mods),&tx);
        if let ActiveOverlay::DiscBrowser(s)=&mut app.active_overlay {
            focus_native_row(s,"Folder");
        }
        send(&mut app,KeyCode::Enter,M::NONE);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.text_edit.as_ref().is_some_and(|i|i.select_all)));
        // A letter which is a window accelerator must replace the selection,
        // not change a checkbox or trigger the More sheet.
        send(&mut app,KeyCode::Char('m'),M::NONE);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.editing_destination && s.destination_input=="m" && s.panel.is_none()));
        send(&mut app,KeyCode::Esc,M::NONE);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if !s.editing_destination && s.destination==path));
        if let ActiveOverlay::DiscBrowser(s)=&mut app.active_overlay {
            focus_native_row(s,"Naming");
        }
        send(&mut app,KeyCode::Enter,M::NONE);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.editing_template && s.text_edit.as_ref().is_some_and(|i|i.select_all)));
        send(&mut app,KeyCode::Char('c'),M::NONE);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if s.destination_input=="c" && s.panel.is_none()));
        send(&mut app,KeyCode::Char('a'),M::CONTROL);
        send(&mut app,KeyCode::Char('%'),M::NONE);
        send(&mut app,KeyCode::Char('T'),M::NONE);
        send(&mut app,KeyCode::Char('I'),M::NONE);
        send(&mut app,KeyCode::Char('T'),M::NONE);
        send(&mut app,KeyCode::Char('L'),M::NONE);
        send(&mut app,KeyCode::Char('E'),M::NONE);
        send(&mut app,KeyCode::Char('%'),M::NONE);
        send(&mut app,KeyCode::Enter,M::NONE);
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s)
            if !s.editing_destination && s.naming_template=="%TITLE%"));
    }

    #[test]
    fn r32_title_size_glyph_toggles_without_requiring_double_click() {
        let source=PathBuf::from("disc.iso");
        let state=DiscBrowserState::new(
            selectable_disc_contents(source.clone(),FolderDiscMarkerKind::Sacd),source);
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(state));
        let (tx,_rx)=tokio::sync::mpsc::channel(8);
        assert!(handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserTitleSize,1,&tx));
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s) if s.maximized));
        assert!(handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserTitleSize,1,&tx));
        assert!(matches!(&app.active_overlay,ActiveOverlay::DiscBrowser(s) if !s.maximized));
    }

    #[test]
    fn r15_extract_title_double_click_and_alt_m_toggle_maximize_plain_m_keeps_options() {
        use crossterm::event::KeyModifiers;
        let source=std::path::PathBuf::from("disc.iso");
        let contents=selectable_disc_contents(source.clone(),FolderDiscMarkerKind::Sacd);
        let mut app=AppState::new_for_test(TonepoetConfig::default());
        app.active_overlay=ActiveOverlay::DiscBrowser(Box::new(DiscBrowserState::new(contents,source)));
        let (tx,_rx)=tokio::sync::mpsc::channel(4);
        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserTitle,1,&tx);
        let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else {panic!("missing extract overlay")};
        assert!(!state.maximized);
        handle_disc_browser_button_click(&mut app,&TuiButton::DiscBrowserTitle,2,&tx);
        let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else {panic!("missing extract overlay")};
        assert!(state.maximized);
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Char('m'),KeyModifiers::ALT),&tx);
        let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else {panic!("missing extract overlay")};
        assert!(!state.maximized);
        handle_disc_browser_key(&mut app,KeyEvent::new(KeyCode::Char('m'),KeyModifiers::NONE),&tx);
        let ActiveOverlay::DiscBrowser(state)=&app.active_overlay else {panic!("missing extract overlay")};
        assert_eq!(state.panel,Some(crate::tui::disc_browser::ExtractPanel::More));
    }
}
