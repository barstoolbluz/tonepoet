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
        open_disc_browser_from_contents(app, contents.as_ref().clone(), path);
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
pub fn open_disc_browser_from_contents(app: &mut AppState, contents: DiscContents, source_path: PathBuf) {
    if contents.presentations.is_empty() {
        app.set_status("Disc contains no selectable audio streams");
        return;
    }

    app.active_overlay = ActiveOverlay::DiscBrowser(Box::new(DiscBrowserState::new(
        contents,
        source_path,
    )));
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
) {
    match followup {
        DiscProbeFollowup::OpenDiscBrowser => {
            let Some(contents) = cached_disc_contents(app, path) else {
                app.set_status("Audio streams are not available after analysis");
                return;
            };
            open_disc_browser_from_contents(app, contents.as_ref().clone(), path.to_path_buf());
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

/// Mouse/button dispatcher with click-count information.
///
/// The Audio Streams overlay uses the same registered row target for a single
/// click and a double click: one click selects the stream row, while a double
/// click selects it and immediately loads it in Convert. This keeps registration
/// aligned with the visible clipped rows produced by `draw_disc_browser()`.
pub fn handle_disc_browser_button_click(
    app: &mut AppState,
    button: &TuiButton,
    click_count: u8,
    tx: &mpsc::Sender<AppMessage>,
) -> bool {
    match button {
        TuiButton::BrowseInfoAnalyze => {
            if let Some(path) = selected_entry_effective_disc_path(app) {
                force_disc_reprobe(app, path, tx);
                true
            } else {
                false
            }
        }
        TuiButton::BrowseInfoAudioStreams => {
            open_selected_disc_browser(app, tx);
            true
        }
        TuiButton::DiscBrowserStream(index) => {
            if let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay {
                state.set_cursor(*index);
            } else {
                return false;
            }

            if click_count >= 2 {
                convert_overlay_cursor(app);
            }
            true
        }
        TuiButton::DiscBrowserExpand(index) => {
            if let ActiveOverlay::DiscBrowser(state) = &mut app.active_overlay {
                state.set_cursor(*index);
                state.toggle_expanded(*index);
                true
            } else {
                false
            }
        }
        TuiButton::DiscBrowserConvert => {
            convert_overlay_cursor(app);
            true
        }
        TuiButton::DiscBrowserClose => {
            if matches!(app.active_overlay, ActiveOverlay::DiscBrowser(_)) {
                app.active_overlay = ActiveOverlay::None;
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Convenience entry point for mouse handlers that classify a repeated row click
/// as a double-click. This deliberately forwards through the click-count path so
/// the same safeguards and overlay-state checks are used in both mouse paths.
pub fn handle_disc_browser_double_click(
    app: &mut AppState,
    button: &TuiButton,
    tx: &mpsc::Sender<AppMessage>,
) -> bool {
    handle_disc_browser_button_click(app, button, 2, tx)
}

/// Key dispatcher for `ActiveOverlay::DiscBrowser`.
pub fn handle_disc_browser_key(app: &mut AppState, key: KeyEvent) {
    let mut overlay = ActiveOverlay::None;
    std::mem::swap(&mut overlay, &mut app.active_overlay);

    let ActiveOverlay::DiscBrowser(mut state) = overlay else {
        app.active_overlay = overlay;
        return;
    };

    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            state.move_cursor(-1);
            app.active_overlay = ActiveOverlay::DiscBrowser(state);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.move_cursor(1);
            app.active_overlay = ActiveOverlay::DiscBrowser(state);
        }
        KeyCode::Right | KeyCode::Char('e') | KeyCode::Char('E') => {
            let cursor = state.cursor;
            state.toggle_expanded(cursor);
            app.active_overlay = ActiveOverlay::DiscBrowser(state);
        }
        KeyCode::Char(' ') => {
            let cursor = state.cursor;
            state.toggle_selected(cursor);
            app.active_overlay = ActiveOverlay::DiscBrowser(state);
        }
        KeyCode::PageUp => {
            state.scroll_by_rows(-10);
            app.active_overlay = ActiveOverlay::DiscBrowser(state);
        }
        KeyCode::PageDown => {
            state.scroll_by_rows(10);
            let max_scroll = crate::tui::disc_browser::disc_browser_visible_rows(&state)
                .len()
                .saturating_sub(1);
            state.scroll = state.scroll.min(max_scroll);
            app.active_overlay = ActiveOverlay::DiscBrowser(state);
        }
        KeyCode::Home => {
            state.set_cursor(0);
            state.scroll = 0;
            app.active_overlay = ActiveOverlay::DiscBrowser(state);
        }
        KeyCode::End => {
            let last = state.len().saturating_sub(1);
            state.set_cursor(last);
            state.scroll = crate::tui::disc_browser::disc_browser_visible_rows(&state)
                .len()
                .saturating_sub(1);
            app.active_overlay = ActiveOverlay::DiscBrowser(state);
        }
        KeyCode::Enter => {
            let cursor = state.cursor;
            let contents = state.contents.clone();
            match require_selected_presentation_stream_conversion(&contents, cursor)
                .and_then(|()| load_disc_presentation_for_convert(app, contents, cursor))
            {
                Ok(()) => {}
                Err(err) => {
                    app.set_status(format!("Disc stream load failed: {err}"));
                    app.active_overlay = ActiveOverlay::DiscBrowser(state);
                }
            }
        }
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
            app.active_overlay = ActiveOverlay::None;
        }
        _ => {
            app.active_overlay = ActiveOverlay::DiscBrowser(state);
        }
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

/// Convert the currently highlighted overlay stream.
pub fn convert_overlay_cursor(app: &mut AppState) {
    let (contents, cursor) = match &app.active_overlay {
        ActiveOverlay::DiscBrowser(state) => (state.contents.clone(), state.cursor),
        _ => return,
    };

    if let Err(err) = require_selected_presentation_stream_conversion(&contents, cursor)
        .and_then(|()| load_disc_presentation_for_convert(app, contents, cursor))
    {
        app.set_status(format!("Disc stream load failed: {err}"));
    }
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
