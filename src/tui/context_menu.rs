//! Context menu: right-click / `m` keybinding opens a floating menu
//! with context-sensitive actions. Per-screen `build_*_menu` functions
//! produce filtered item lists; `execute_context_action` dispatches.

use std::path::PathBuf;
use tokio::sync::mpsc;

use super::app::*;
use super::browse::{BrowseEntry, EntryKind};
use super::message::AppMessage;
use crate::convert::ConversionStatus;


fn persist_browse_config(app: &mut AppState) {
    app.config.browsing = app.browse.capture_browsing_config();
    if let Err(err) = app.config.save() {
        app.set_status(format!("browse settings changed, but config save failed: {err}"));
    }
}

// ── Data structures ─────────────────────────────────────────────────

/// Hard cap on the cascade depth of context menus. The active overlay
/// stores `Vec<MenuLevel>` and refuses to push beyond this depth.
pub const MAX_CONTEXT_MENU_DEPTH: usize = 4;

/// One panel in the cascade. The deepest level is the focused one;
/// ancestor levels stay visible with a muted border, their selected
/// row marking the breadcrumb.
#[derive(Debug, Clone)]
pub struct MenuLevel {
    pub entries: Vec<ContextMenuEntry>,
    pub selected: usize,
}

impl MenuLevel {
    pub fn new(entries: Vec<ContextMenuEntry>) -> Self {
        Self {
            entries,
            selected: 0,
        }
    }
}

/// A single item in a context menu.
#[derive(Debug, Clone)]
pub struct ContextMenuItem {
    pub label: String,
    pub action: ContextAction,
    /// Optional shortcut hint displayed right-aligned (e.g., ":rename").
    pub shortcut: Option<String>,
    /// Greyed-out items are visible but not selectable.
    pub enabled: bool,
}

/// An entry in the context menu -- either a clickable item, a visual
/// separator, or a submenu that expands on selection.
#[derive(Debug, Clone)]
pub enum ContextMenuEntry {
    Item(ContextMenuItem),
    Separator,
    /// A submenu parent. Selecting it pushes the current menu onto the
    /// stack and replaces it with `children`. The label is shown with
    /// a "►" indicator.
    Submenu {
        label: String,
        children: Vec<ContextMenuEntry>,
    },
}

/// Actions that can be triggered from the context menu. Each variant
/// maps to a specific operation dispatched by `execute_context_action`.
#[derive(Debug, Clone)]
pub enum ContextAction {
    // ── Browse screen ───────────────────────────────────────────────
    /// Open the Convert screen for custom configuration.
    ConvertCustom,
    /// Queue with current pills ("last used") or a named preset.
    /// `start` is resolved at dispatch time from config + Shift.
    ConvertLastUsed,
    /// Queue with a named preset.
    ConvertWithPreset(String),
    /// Convert the default disc stream without going through generic file handling.
    ConvertDiscDefault,
    /// Open the unified Audio Streams overlay for a disc source.
    BrowseDiscStreams,
    /// Convert one specific disc presentation.
    ConvertDiscStream(crate::disc::PresentationId),
    /// Open the selected file/directory (same as Enter on browse).
    OpenEntry,
    /// Toggle the current entry's selection.
    Select,
    /// Select all non-ParentDir entries.
    SelectAll,
    /// Invert the selection (selected ↔ unselected).
    SelectInverse,
    /// Clear multi-selection.
    Deselect,
    /// Rename the selected file (`:rename` or context menu).
    RenameEntry,
    /// Permanently delete selected filesystem path(s).
    DeletePermanently,
    /// Copy the full path of the selected entry to the clipboard.
    CopyPath(PathBuf),
    /// Edit a metadata field on the selected audio file (legacy single-field).
    EditMetadata(crate::tui::probe::MetadataField),
    /// Open the full metadata editor overlay.
    EditMetadataFull,
    /// Copy the selected file(s) to a destination (opens directory picker).
    CopyTo,
    /// Move the selected file(s) to a destination (opens directory picker).
    MoveTo,
    /// Refresh the browse listing.
    Refresh,
    /// Open the bulk rename wizard for selected audio files.
    BulkRename,
    /// Analyze selected audio files (DR, peak, LUFS, etc.).
    Analyze,
    /// Set a password for the selected archive (opens TextEdit prompt).
    SetArchivePassword,
    /// Verify integrity of selected audio file(s).
    Verify,
    /// Generate a multi-file CUE sheet (one FILE per track).
    GenerateCueMultiFile,
    /// Generate a single-image CUE sheet (one FILE, cumulative timestamps).
    GenerateCueSingleImage,
    /// Generate a multi-file CUE driven by a MusicBrainz disc-TOC lookup.
    GenerateCueMbMultiFile,
    /// Generate a single-image CUE driven by a MusicBrainz disc-TOC lookup.
    GenerateCueMbSingleImage,
    /// Fill empty/absent fields on the colocated CUE from a MusicBrainz lookup.
    FillCueFromMb,
    /// Mark current selection as the bit-compare reference.
    MarkCompareReference,
    /// Run bit comparison against the stored reference.
    BitCompareWithReference,
    /// Clear the stored bit-compare reference.
    ClearCompareReference,
    /// Detect CD pre-emphasis on selected audio file(s).
    DetectPreemphasis,
    /// Look up tags from gnudb.org (CDDB).
    QueryGnudb,
    /// Look up tags from MusicBrainz (disc-TOC).
    TagsFromMb,
    /// MetadataEditor: toggle the cursor row between MB-proposed and
    /// the file's pre-MB value. No-op when the row was not modified by
    /// MB or has been manually edited.
    MetadataRevertMb,
    /// MetadataEditor: open inline edit on the cursor row.
    MetadataEditValue,
    /// MetadataEditor: mark the cursor row for deletion.
    MetadataDeleteEntry,
    /// MetadataEditor: undo a pending deletion on the cursor row.
    MetadataRestoreEntry,
    /// MetadataEditor: open the "add new field" input.
    MetadataAddField,
    /// MetadataEditor: open a read-only CuePreview seeded with the
    /// cursor row's value (synthetic-preview rows like CUESHEET).
    MetadataCueView,
    /// MetadataEditor detail overlay: field-level revert toggle
    /// (operates on per_file_values).
    MetadataDetailToggleRevert,
    /// MetadataEditor detail overlay: snap per_file_values back to the
    /// as-retrieved MB proposal.
    MetadataDetailRestore,
    /// MetadataEditor detail overlay: leave detail mode, back to the
    /// field list.
    MetadataDetailBack,
    /// MbSelect: accept the parked picker's currently-cursored row
    /// (open metadata editor populated from that release).
    MbSelectAcceptCurrent,
    /// MbSelect: cancel the parked picker (close without populating).
    MbSelectCancelPicker,
    /// CuePreview: save the parked CUE (writes to disk via Command::Write).
    CuePreviewSave,
    /// CuePreview: cancel the parked CUE preview without writing.
    CuePreviewCancel,
    /// CuePreview: begin editing the given 0-based line index (carried
    /// in the action so the right-click handler doesn't have to mutate
    /// the parked state to communicate the line).
    CuePreviewEditLine(usize),
    /// CuePreview: scroll to top.
    CuePreviewScrollTop,
    /// CuePreview: scroll to bottom.
    CuePreviewScrollBottom,
    /// Import tags from a CUE sheet (opens metadata editor + external editor).
    ImportCueFromBrowse,
    /// AccurateRip verification (common offsets).
    VerifyAccurateRip,
    /// AccurateRip verification (full offset scan).
    AccurateRipFullScan,
    /// AccurateRip batch verification of current directory.
    AccurateRipBatch,
    /// AccurateRip offset correction.
    AccurateRipFixOffset,
    /// CUETools DB verification.
    VerifyCtdb,
    /// CUETools DB Reed-Solomon repair.
    CtdbRepair,
    /// View a text file in read-only mode.
    ViewFile(PathBuf),
    /// Edit a text file (not available for .log files).
    EditFile(PathBuf),
    /// Toggle hidden-file visibility.
    ToggleHidden,
    /// Cycle the sort field.
    CycleSortBy,
    /// Open the bookmarks overlay.
    OpenBookmarks,
    /// Add the current directory as a bookmark.
    BookmarkCurrentDir,

    // ── Convert screen ──────────────────────────────────────────────
    /// Open Browse to pick a source file.
    BrowseForSource,
    /// Open the BatchList expand overlay.
    ExpandBatch,
    /// Commit the current review to the queue (`:commit`).
    CommitQueue,
    /// Commit and start processing (`:Commit`).
    CommitAndStart,
    /// Load a named preset into the format pills.
    LoadPreset(String),
    /// Toggle maximize/restore for a Convert pane.
    TogglePaneMaximize(ConvertFocus),

    // ── Queue screen ────────────────────────────────────────────────
    /// Start processing (`:go`).
    StartProcessing,
    /// Pause / resume conversions.
    TogglePause,
    /// Remove selected queue item(s).
    RemoveSelected,
    /// Retry failed items.
    RetryFailed,
    /// Clear completed items.
    ClearCompleted,
    /// Show item info / error detail.
    ShowItemInfo,
    /// Toggle per-track sub-line collapse/expand.
    ToggleTrackCollapse,

    // ── Global ──────────────────────────────────────────────────────
    /// Switch to a specific screen.
    GoToScreen(AppScreen),
}

// ── Menu builders ───────────────────────────────────────────────────

fn item(label: &str, action: ContextAction) -> ContextMenuEntry {
    ContextMenuEntry::Item(ContextMenuItem {
        label: label.to_string(),
        action,
        shortcut: None,
        enabled: true,
    })
}

fn separator() -> ContextMenuEntry {
    ContextMenuEntry::Separator
}

/// Build the disc stream-conversion submenu from the probed disc model.
///
/// Blu-ray presentations are intentionally treated as first-class conversion
/// targets here. An older regression test asserted that Blu-ray streams should
/// stay hidden until an external `SourceOptions` object already existed; that
/// guard is now obsolete because `disc_browser::apply_presentation_to_source_options`
/// is the single bridge that maps a selected Blu-ray presentation id into the
/// pipeline fields (`bluray_playlist`, `bluray_audio_pid`, stream index, and
/// angle) at commit time. Keeping menu eligibility tied to
/// `presentation_supports_stream_conversion` prevents the UI from exposing a
/// stream identity the conversion bridge cannot honor.
fn build_disc_convert_stream_submenu(
    contents: &crate::disc::DiscContents,
) -> Option<ContextMenuEntry> {
    let children: Vec<_> = contents
        .presentations
        .iter()
        .enumerate()
        .filter(|(_, presentation)| {
            crate::tui::disc_browser::presentation_supports_stream_conversion(presentation)
        })
        .map(|(idx, presentation)| {
            item(
                &format!("Stream {}: {}", idx + 1, presentation.label),
                ContextAction::ConvertDiscStream(presentation.id),
            )
        })
        .collect();

    (!children.is_empty()).then_some(ContextMenuEntry::Submenu {
        label: "Convert Stream".to_string(),
        children,
    })
}

/// Build the "Convert" submenu. Custom opens the Convert screen for
/// manual review; Last Used and presets auto-commit (enqueue + start by
/// default, enqueue-only when Shift is held — configurable via
/// `[ui] convert_default_action`).
///
/// ```text
/// Convert ►
/// ├── Custom                         — open Convert screen
/// ├── Last used [FLAC]               — auto-commit with current pills
/// ├── ─────────────────────          (only if presets exist)
/// ├── preset-name-1
/// ├── preset-name-2
/// └── ...
/// ```
fn build_convert_submenu(app: &AppState) -> ContextMenuEntry {
    let groups = super::presets::list_presets_by_format_db(&app.db);
    let current_format = app.convert.format.format.selected_value().name();

    let mut children = vec![
        item("Custom", ContextAction::ConvertCustom),
        item(
            &format!("Last used [{}]", current_format),
            ContextAction::ConvertLastUsed,
        ),
    ];

    // Flat preset list (one item per preset, no queue/start pairs).
    let all_presets: Vec<String> = groups.iter().flat_map(|(_, names)| names.clone()).collect();
    if !all_presets.is_empty() {
        children.push(separator());
        for name in &all_presets {
            children.push(item(name, ContextAction::ConvertWithPreset(name.clone())));
        }
    }

    ContextMenuEntry::Submenu {
        label: "Convert".to_string(),
        children,
    }
}

/// Build the "File operations" submenu (Rename, Bulk Rename, Copy/Move,
/// Delete). `include_bulk_rename` adds the Bulk Rename option for audio files.
fn build_file_ops_submenu(include_bulk_rename: bool) -> ContextMenuEntry {
    let mut children = vec![item("Rename", ContextAction::RenameEntry)];
    if include_bulk_rename {
        children.push(item("Bulk Rename", ContextAction::BulkRename));
    }
    children.push(item("Copy to...", ContextAction::CopyTo));
    children.push(item("Move to...", ContextAction::MoveTo));
    children.push(item("Delete permanently", ContextAction::DeletePermanently));
    ContextMenuEntry::Submenu {
        label: "File operations".to_string(),
        children,
    }
}


/// Archive browse entries use synthetic paths (`archive/inner`) that do not
/// exist on the host filesystem. Only expose actions here that either navigate
/// the in-memory archive listing or have an explicit archive-aware staging path.
/// Generic file operations (bulk rename, copy/move, delete, text view/edit,
/// tagging utilities, analysis) must stay hidden so they cannot feed synthetic
/// paths into filesystem code.
fn archive_entry_can_rename(entry: &BrowseEntry) -> bool {
    !matches!(entry.kind, EntryKind::Directory | EntryKind::ParentDir)
}

fn archive_file_ops_submenu(entry: &BrowseEntry) -> Option<ContextMenuEntry> {
    if !archive_entry_can_rename(entry) {
        return None;
    }
    Some(ContextMenuEntry::Submenu {
        label: "File operations".to_string(),
        children: vec![item("Rename", ContextAction::RenameEntry)],
    })
}

fn build_archive_browse_entry_menu(entry: &BrowseEntry) -> Vec<ContextMenuEntry> {
    let mut items = Vec::new();

    match &entry.kind {
        EntryKind::AudioFile(_) => {
            items.push(item("Edit metadata", ContextAction::EditMetadataFull));
            items.push(separator());
            items.push(item("Select", ContextAction::Select));
            items.push(item("Select All", ContextAction::SelectAll));
            items.push(item("Select Inverse", ContextAction::SelectInverse));
            items.push(item("Deselect", ContextAction::Deselect));
            if let Some(file_ops) = archive_file_ops_submenu(entry) {
                items.push(separator());
                items.push(file_ops);
            }
            items.push(item("Copy path", ContextAction::CopyPath(entry.path.clone())));
        }
        EntryKind::Directory => {
            items.push(item("Open", ContextAction::OpenEntry));
            items.push(item("Edit metadata", ContextAction::EditMetadataFull));
            items.push(separator());
            items.push(item("Select", ContextAction::Select));
            items.push(item("Select All", ContextAction::SelectAll));
            items.push(item("Select Inverse", ContextAction::SelectInverse));
            items.push(item("Deselect", ContextAction::Deselect));
            items.push(item("Copy path", ContextAction::CopyPath(entry.path.clone())));
        }
        EntryKind::ParentDir => {
            items.push(item("Go up", ContextAction::OpenEntry));
            items.push(separator());
            items.push(item("Select All", ContextAction::SelectAll));
            items.push(item("Deselect", ContextAction::Deselect));
        }
        _ => {
            items.push(item("Select", ContextAction::Select));
            items.push(item("Select All", ContextAction::SelectAll));
            items.push(item("Select Inverse", ContextAction::SelectInverse));
            items.push(item("Deselect", ContextAction::Deselect));
            if let Some(file_ops) = archive_file_ops_submenu(entry) {
                items.push(separator());
                items.push(file_ops);
            }
            items.push(item("Copy path", ContextAction::CopyPath(entry.path.clone())));
        }
    }

    items
}

fn archive_synthetic_file_op_status(app: &mut AppState, operation: &str) {
    app.set_status(format!(
        "archive: {operation} is unavailable inside archives; extract the file or use archive-aware metadata/rename"
    ));
}

/// Build the "Tagging" submenu (MusicBrainz lookup, CUE import).
/// `has_cue` controls whether the CUE import option is shown.
///
/// Note: "Get tags from gnudb.org" was hidden 2026-05-10 because
/// gnudb's HTTP/80 endpoint (which the client uses) stopped accepting
/// connections; their CDDBP/8880 port still works but tonepoet's
/// reqwest-based client can't speak that protocol. The full gnudb
/// code path (ContextAction::QueryGnudb dispatch, GnudbSelect/
/// GnudbReview overlays, populate_editor_from_review, query_gnudb /
/// read_gnudb HTTP clients) is preserved intact — restore the menu
/// entry below if the HTTP endpoint comes back, or migrate the
/// client to CDDBP if a longer-term fix is wanted.
fn build_tagging_submenu(has_cue: bool) -> ContextMenuEntry {
    let mut children = vec![item("Get tags from MusicBrainz", ContextAction::TagsFromMb)];
    if has_cue {
        children.push(item(
            "Get tags from CUE",
            ContextAction::ImportCueFromBrowse,
        ));
    }
    ContextMenuEntry::Submenu {
        label: "Tagging".to_string(),
        children,
    }
}

/// "Disc Tools" submenu — AccurateRip and CUETools DB lookups, both
/// of which only return useful results for redbook CD rips (16-bit /
/// 44.1 kHz with a known TOC).
fn build_disk_tools_submenu() -> ContextMenuEntry {
    ContextMenuEntry::Submenu {
        label: "Disc Tools".to_string(),
        children: vec![
            ContextMenuEntry::Submenu {
                label: "AccurateRip".to_string(),
                children: vec![
                    item("Verify (common offsets)", ContextAction::VerifyAccurateRip),
                    item(
                        "Verify (full offset scan)",
                        ContextAction::AccurateRipFullScan,
                    ),
                    item("Batch verify directory", ContextAction::AccurateRipBatch),
                    separator(),
                    item("Fix offset", ContextAction::AccurateRipFixOffset),
                ],
            },
            ContextMenuEntry::Submenu {
                label: "CUETools DB".to_string(),
                children: vec![
                    item("Verify", ContextAction::VerifyCtdb),
                    item("Reed-Solomon repair", ContextAction::CtdbRepair),
                ],
            },
        ],
    }
}

fn build_utilities_submenu(app: &AppState) -> ContextMenuEntry {
    let mut children = vec![
        item("Verify integrity", ContextAction::Verify),
        separator(),
        item(
            "CUE sheet from tags (multi-file)",
            ContextAction::GenerateCueMultiFile,
        ),
        item(
            "CUE sheet from tags (single image)",
            ContextAction::GenerateCueSingleImage,
        ),
        item(
            "CUE sheet from MusicBrainz (multi-file)",
            ContextAction::GenerateCueMbMultiFile,
        ),
        item(
            "CUE sheet from MusicBrainz (single image)",
            ContextAction::GenerateCueMbSingleImage,
        ),
        item("Fill CUE from MusicBrainz", ContextAction::FillCueFromMb),
        separator(),
    ];

    if app.compare_reference.is_empty() {
        children.push(item(
            "Bit compare (mark reference)",
            ContextAction::MarkCompareReference,
        ));
    } else {
        children.push(item(
            "Bit compare with reference",
            ContextAction::BitCompareWithReference,
        ));
        children.push(item(
            "Clear reference",
            ContextAction::ClearCompareReference,
        ));
    }

    ContextMenuEntry::Submenu {
        label: "Utilities".to_string(),
        children,
    }
}

/// Build the context menu for a right-click on a browse entry.
pub fn build_browse_entry_menu(app: &AppState) -> Vec<ContextMenuEntry> {
    let entry = match app.browse.selected_entry() {
        Some(e) => e,
        None => return Vec::new(),
    };

    if app.browse.is_in_archive() {
        return build_archive_browse_entry_menu(entry);
    }

    let mut items = Vec::new();

    match &entry.kind {
        EntryKind::AudioFile(_) => {
            items.push(build_convert_submenu(app));
            items.push(separator());
            items.push(item("Select", ContextAction::Select));
            items.push(item("Select All", ContextAction::SelectAll));
            items.push(item("Select Inverse", ContextAction::SelectInverse));
            items.push(item("Deselect", ContextAction::Deselect));
            items.push(separator());
            items.push(item("Edit metadata", ContextAction::EditMetadataFull));
            items.push(item("Analyze", ContextAction::Analyze));
            // Check if a CUE file exists in the directory.
            let has_cue = entry
                .path
                .parent()
                .and_then(|d| std::fs::read_dir(d).ok())
                .map(|entries| {
                    entries.filter_map(|e| e.ok()).any(|e| {
                        e.path()
                            .extension()
                            .map(|ext| ext.to_ascii_lowercase() == "cue")
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            items.push(build_tagging_submenu(has_cue));
            items.push(build_disk_tools_submenu());
            items.push(build_utilities_submenu(app));
            items.push(separator());
            items.push(build_file_ops_submenu(true));
            items.push(item(
                "Copy path",
                ContextAction::CopyPath(entry.path.clone()),
            ));
        }
        EntryKind::Archive => {
            items.push(build_convert_submenu(app));
            items.push(separator());
            items.push(item("Select", ContextAction::Select));
            items.push(item("Select All", ContextAction::SelectAll));
            items.push(item("Select Inverse", ContextAction::SelectInverse));
            items.push(item("Deselect", ContextAction::Deselect));
            items.push(separator());
            items.push(item("Edit metadata", ContextAction::EditMetadataFull));
            items.push(item("Set Password", ContextAction::SetArchivePassword));
            items.push(build_file_ops_submenu(false));
            items.push(item(
                "Copy path",
                ContextAction::CopyPath(entry.path.clone()),
            ));
        }
        EntryKind::SacdIso => {
            items.push(item("Browse Audio Streams...", ContextAction::BrowseDiscStreams));
            if let Some(contents) = app
                .browse
                .disc_probe_cache
                .get(&entry.path)
                .and_then(|cache| cache.contents_if_current(&entry.path))
            {
                if let Some(submenu) = build_disc_convert_stream_submenu(contents.as_ref()) {
                    items.push(submenu);
                }
            }
            items.push(item("Edit metadata", ContextAction::EditMetadataFull));
            items.push(separator());
            items.push(item("Select", ContextAction::Select));
            items.push(item("Select All", ContextAction::SelectAll));
            items.push(item("Select Inverse", ContextAction::SelectInverse));
            items.push(item("Deselect", ContextAction::Deselect));
            items.push(separator());
            items.push(build_tagging_submenu(false));
            items.push(build_utilities_submenu(app));
            items.push(separator());
            items.push(build_file_ops_submenu(false));
            items.push(item("Copy path", ContextAction::CopyPath(entry.path.clone())));
        }
        EntryKind::DvdAudioIso
        | EntryKind::DvdAudioDir
        | EntryKind::DvdVideoIso
        | EntryKind::DvdVideoDir
        | EntryKind::BlurayIso
        | EntryKind::BlurayDir => {
            items.push(item("Convert (default stream)", ContextAction::ConvertDiscDefault));
            items.push(item("Browse Audio Streams...", ContextAction::BrowseDiscStreams));
            if let Some(contents) = app
                .browse
                .disc_probe_cache
                .get(&entry.path)
                .and_then(|cache| cache.contents_if_current(&entry.path))
            {
                if let Some(submenu) = build_disc_convert_stream_submenu(contents.as_ref()) {
                    items.push(submenu);
                }
            }
            items.push(separator());
            items.push(item("Edit metadata", ContextAction::EditMetadataFull));
            items.push(item("Analyze", ContextAction::Analyze));
            items.push(separator());
            items.push(item("Select", ContextAction::Select));
            items.push(item("Select All", ContextAction::SelectAll));
            items.push(item("Select Inverse", ContextAction::SelectInverse));
            items.push(item("Deselect", ContextAction::Deselect));
            items.push(separator());
            items.push(build_tagging_submenu(false));
            items.push(build_utilities_submenu(app));
            items.push(separator());
            items.push(build_file_ops_submenu(false));
            items.push(item("Copy path", ContextAction::CopyPath(entry.path.clone())));
        }
        EntryKind::Directory => {
            items.push(build_convert_submenu(app));
            items.push(separator());
            items.push(item("Open", ContextAction::OpenEntry));
            items.push(item("Edit metadata", ContextAction::EditMetadataFull));
            items.push(item("Analyze", ContextAction::Analyze));
            // Check if a CUE file exists inside the directory.
            let has_cue = std::fs::read_dir(&entry.path)
                .ok()
                .map(|entries| {
                    entries.filter_map(|e| e.ok()).any(|e| {
                        e.path()
                            .extension()
                            .map(|ext| ext.to_ascii_lowercase() == "cue")
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);
            items.push(build_tagging_submenu(has_cue));
            items.push(build_disk_tools_submenu());
            items.push(build_utilities_submenu(app));
            items.push(item("Select", ContextAction::Select));
            items.push(item("Select All", ContextAction::SelectAll));
            items.push(item("Select Inverse", ContextAction::SelectInverse));
            items.push(item("Deselect", ContextAction::Deselect));
            items.push(separator());
            items.push(build_file_ops_submenu(false));
            items.push(item(
                "Copy path",
                ContextAction::CopyPath(entry.path.clone()),
            ));
        }
        EntryKind::ParentDir => {
            items.push(item("Go up", ContextAction::OpenEntry));
            items.push(separator());
            items.push(item("Select All", ContextAction::SelectAll));
            items.push(item("Deselect", ContextAction::Deselect));
        }
        EntryKind::OtherFile => {
            // CUE files are convertible (they reference an image file).
            let is_cue = entry
                .path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("cue"))
                .unwrap_or(false);
            if is_cue {
                items.push(build_convert_submenu(app));
                items.push(separator());
            }
            if super::browse::is_viewable_text_file(&entry.path) {
                items.push(item("View", ContextAction::ViewFile(entry.path.clone())));
                if super::browse::is_editable_text_file(&entry.path) {
                    items.push(item("Edit", ContextAction::EditFile(entry.path.clone())));
                }
                items.push(separator());
            }
            items.push(item("Select", ContextAction::Select));
            items.push(item("Select All", ContextAction::SelectAll));
            items.push(item("Select Inverse", ContextAction::SelectInverse));
            items.push(item("Deselect", ContextAction::Deselect));
            items.push(separator());
            items.push(build_file_ops_submenu(false));
            items.push(item(
                "Copy path",
                ContextAction::CopyPath(entry.path.clone()),
            ));
        }
    }

    items
}

/// Build the context menu for a right-click on empty space in the browse list.
pub fn build_browse_empty_menu(app: &AppState) -> Vec<ContextMenuEntry> {
    let _ = app; // used in future for conditional items
    vec![
        item("Select All", ContextAction::SelectAll),
        item("Select Inverse", ContextAction::SelectInverse),
        item("Deselect", ContextAction::Deselect),
        separator(),
        item("Refresh", ContextAction::Refresh),
        item("Toggle hidden", ContextAction::ToggleHidden),
        item("Change sort", ContextAction::CycleSortBy),
        separator(),
        item("Bookmarks", ContextAction::OpenBookmarks),
        item("Bookmark this dir", ContextAction::BookmarkCurrentDir),
    ]
}

/// Build the context menu for the Convert screen.
pub fn build_convert_menu(app: &AppState) -> Vec<ContextMenuEntry> {
    let mut items = Vec::new();

    if !app.convert.source.mode.is_empty() {
        items.push(item("Commit", ContextAction::CommitQueue));
        items.push(item("Commit + start", ContextAction::CommitAndStart));
        items.push(separator());
    }

    if app.convert.source.mode.is_batch() {
        items.push(item("Expand batch", ContextAction::ExpandBatch));
        items.push(separator());
    }

    items.push(item("Browse for source", ContextAction::BrowseForSource));

    let pane_items = [
        (ConvertFocus::Source, "Source"),
        (ConvertFocus::Metadata, "Metadata"),
        (ConvertFocus::Format, "Format"),
        (ConvertFocus::OutputOptions, "Output Options"),
    ];
    items.push(separator());
    for (focus, name) in &pane_items {
        let label = if app.convert.is_maximized(*focus) {
            format!("Restore {}", name)
        } else {
            format!("Maximize {}", name)
        };
        items.push(item(&label, ContextAction::TogglePaneMaximize(*focus)));
    }

    // Presets grouped by codec — same tree structure as the browse
    // entry's Convert submenu, but here just for loading (not queuing).
    let groups = super::presets::list_presets_by_format_db(&app.db);
    if !groups.is_empty() {
        items.push(separator());
        for (fmt, names) in &groups {
            let codec_label = match fmt {
                Some(f) => format!("Presets: {}", f.name()),
                None => "Presets: Other".to_string(),
            };
            let preset_items: Vec<ContextMenuEntry> = names
                .iter()
                .map(|name| item(name, ContextAction::LoadPreset(name.clone())))
                .collect();
            items.push(ContextMenuEntry::Submenu {
                label: codec_label,
                children: preset_items,
            });
        }
    }

    items
}

/// Build the context menu for a right-click on a queue item.
pub fn build_queue_item_menu(app: &AppState) -> Vec<ContextMenuEntry> {
    let item_ref = app.items_snapshot.get(app.selected_index);

    let mut items = Vec::new();

    if let Some(qi) = item_ref {
        items.push(item("Info", ContextAction::ShowItemInfo));

        if !qi.active_tracks.is_empty() {
            let label = if qi.tracks_collapsed {
                "Expand tracks"
            } else {
                "Collapse tracks"
            };
            items.push(item(label, ContextAction::ToggleTrackCollapse));
        }

        match &qi.status {
            ConversionStatus::Failed { .. } => {
                items.push(item("Retry", ContextAction::RetryFailed));
            }
            ConversionStatus::Queued | ConversionStatus::NotConfigured => {
                items.push(item("Remove", ContextAction::RemoveSelected));
            }
            _ => {}
        }
    }

    items.push(separator());

    if app.processing_active {
        items.push(item("Pause", ContextAction::TogglePause));
    } else {
        items.push(item("Start", ContextAction::StartProcessing));
    }

    items.push(item("Clear completed", ContextAction::ClearCompleted));

    items
}

/// Build the context menu for empty space on the queue screen.
pub fn build_queue_empty_menu(app: &AppState) -> Vec<ContextMenuEntry> {
    let mut items = Vec::new();

    if app.processing_active {
        items.push(item("Pause", ContextAction::TogglePause));
    } else {
        items.push(item("Start all", ContextAction::StartProcessing));
    }

    items.push(item("Clear completed", ContextAction::ClearCompleted));

    items
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveContextDispatch {
    Continue,
    ArchiveAwareDelete,
    RequiresRealPaths(&'static str),
}

fn archive_context_action_requires_real_paths(action: &ContextAction) -> Option<&'static str> {
    match action {
        ContextAction::ConvertCustom
        | ContextAction::ConvertLastUsed
        | ContextAction::ConvertWithPreset(_) => Some("conversion"),
        ContextAction::EditMetadata(_) => Some("inline metadata editing"),
        ContextAction::CopyTo | ContextAction::MoveTo => Some("copy/move"),
        ContextAction::BulkRename => Some("bulk rename"),
        ContextAction::Analyze => Some("analysis"),
        ContextAction::SetArchivePassword => Some("archive password editing"),
        ContextAction::Verify
        | ContextAction::VerifyAccurateRip
        | ContextAction::AccurateRipFullScan
        | ContextAction::AccurateRipBatch
        | ContextAction::AccurateRipFixOffset
        | ContextAction::VerifyCtdb
        | ContextAction::CtdbRepair => Some("verification"),
        ContextAction::GenerateCueMultiFile
        | ContextAction::GenerateCueSingleImage
        | ContextAction::GenerateCueMbMultiFile
        | ContextAction::GenerateCueMbSingleImage
        | ContextAction::FillCueFromMb
        | ContextAction::ImportCueFromBrowse => Some("CUE operations"),
        ContextAction::MarkCompareReference | ContextAction::BitCompareWithReference => {
            Some("bit comparison")
        }
        ContextAction::DetectPreemphasis => Some("pre-emphasis detection"),
        ContextAction::QueryGnudb | ContextAction::TagsFromMb => Some("tag lookup"),
        ContextAction::ViewFile(_) | ContextAction::EditFile(_) => Some("text file view/edit"),
        _ => None,
    }
}

fn archive_context_dispatch_for_action(action: &ContextAction) -> ArchiveContextDispatch {
    match action {
        // Archive-member deletion is not a generic filesystem delete. It is a
        // staged archive edit and must dispatch to start_browse_archive_entry_delete.
        ContextAction::DeletePermanently => ArchiveContextDispatch::ArchiveAwareDelete,
        _ => archive_context_action_requires_real_paths(action)
            .map(ArchiveContextDispatch::RequiresRealPaths)
            .unwrap_or(ArchiveContextDispatch::Continue),
    }
}

// ── Action dispatch ─────────────────────────────────────────────────

/// Execute a context action. Delegates to existing command/action
/// functions where possible.
pub fn execute_context_action(
    app: &mut AppState,
    action: ContextAction,
    tx: &mpsc::Sender<AppMessage>,
    invert: bool,
) {
    if super::disc_browser_actions::handle_disc_context_action(app, &action, tx) {
        return;
    }
    if app.current_screen == AppScreen::Browse && app.browse.is_in_archive() {
        match archive_context_dispatch_for_action(&action) {
            ArchiveContextDispatch::ArchiveAwareDelete => {
                super::keybindings::start_browse_archive_entry_delete(app, tx);
                return;
            }
            ArchiveContextDispatch::RequiresRealPaths(operation) => {
                archive_synthetic_file_op_status(app, operation);
                return;
            }
            ArchiveContextDispatch::Continue => {}
        }
    }
    match action {
        // ── Browse: Convert actions ─────────────────────────────────
        ContextAction::ConvertCustom => {
            // Open Convert screen for manual review — no auto-commit.
            let cmd = super::command::Command::Queue { preset: None };
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::ConvertLastUsed => {
            let start = resolve_convert_start(app, invert);
            let cmd = super::command::Command::Queue { preset: None };
            super::command::execute_command(app, cmd, tx);
            if app.current_screen == AppScreen::Convert {
                super::command::execute_commit_with_disc_selection_bridge(app, start, tx);
            }
        }
        ContextAction::ConvertWithPreset(name) => {
            let start = resolve_convert_start(app, invert);
            let cmd = super::command::Command::Queue { preset: Some(name) };
            super::command::execute_command(app, cmd, tx);
            if app.current_screen == AppScreen::Convert {
                super::command::execute_commit_with_disc_selection_bridge(app, start, tx);
            }
        }
        ContextAction::Select => {
            if app.current_screen == AppScreen::Browse {
                app.browse.toggle_selection();
            }
        }
        ContextAction::SelectAll => {
            if app.current_screen == AppScreen::Browse {
                use super::browse::EntryKind;
                let paths: Vec<std::path::PathBuf> = app
                    .browse
                    .entries
                    .iter()
                    .filter(|e| !matches!(e.kind, EntryKind::ParentDir))
                    .map(|e| e.path.clone())
                    .collect();
                app.browse.multi_selected = paths;
            }
        }
        ContextAction::SelectInverse => {
            if app.current_screen == AppScreen::Browse {
                use super::browse::EntryKind;
                let new_sel: Vec<std::path::PathBuf> = app
                    .browse
                    .entries
                    .iter()
                    .filter(|e| !matches!(e.kind, EntryKind::ParentDir))
                    .filter(|e| !app.browse.multi_selected.iter().any(|p| *p == e.path))
                    .map(|e| e.path.clone())
                    .collect();
                app.browse.multi_selected = new_sel;
            }
        }
        ContextAction::Deselect => {
            if app.current_screen == AppScreen::Browse {
                app.browse.clear_multi_selection();
            }
        }
        ContextAction::OpenEntry => {
            // Simulate Enter on browse
            if app.current_screen == AppScreen::Browse {
                if let Some(entry) = app.browse.selected_entry().cloned() {
                    match entry.kind.clone() {
                        EntryKind::Directory | EntryKind::ParentDir if app.browse.is_in_archive() => {
                            if matches!(entry.kind.clone(), EntryKind::ParentDir) {
                                if !app.browse.go_up_in_archive() {
                                    super::keybindings::exit_browse_archive(app, tx);
                                }
                                app.browse.probe_current_with_db(tx, Some(&app.db));
                            } else if let Some(inner) = app.browse.archive_inner_path_for_path(&entry.path) {
                                app.browse.enter_archive_dir(&inner);
                                app.browse.probe_current_with_db(tx, Some(&app.db));
                            } else {
                                app.set_status("archive: could not resolve directory entry");
                            }
                        }
                        EntryKind::Directory | EntryKind::ParentDir => {
                            app.browse.enter_selected();
                            app.browse.probe_current_with_db(tx, Some(&app.db));
                        }
                        _ => {
                            let path = entry.path.clone();
                            let target = app.browse.return_target;
                            super::keybindings::load_browse_selection_pub(app, path, target);
                        }
                    }
                }
            }
        }
        ContextAction::RenameEntry => {
            if app.browse.is_in_archive() {
                let can_rename = app
                    .browse
                    .selected_entry()
                    .is_some_and(archive_entry_can_rename);
                if !can_rename {
                    archive_synthetic_file_op_status(app, "directory rename");
                    return;
                }
            }
            let cmd = super::command::Command::Rename(String::new());
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::CopyPath(path) => {
            let path_str = path.display().to_string();
            // OSC 52: copy to system clipboard via terminal escape sequence.
            // Works in iTerm2, WezTerm, kitty, Alacritty, foot, xterm, etc.
            let b64 = base64_encode(path_str.as_bytes());
            let osc = format!("\x1b]52;c;{}\x07", b64);
            let _ = std::io::Write::write_all(&mut std::io::stdout(), osc.as_bytes());
            let _ = std::io::Write::flush(&mut std::io::stdout());
            app.set_status(format!("Copied: {}", path_str));
        }
        ContextAction::EditMetadata(field) => {
            super::command::execute_edit_metadata_pub(app, field);
        }
        ContextAction::EditMetadataFull => {
            let guard_paths = super::command::collect_selection_for_file_ops(app);
            if super::command::maybe_confirm_bulk_operation(
                app,
                BulkOperationKind::EditMetadata,
                BulkGuardCommand::OpenMetadataEditor,
                &guard_paths,
            ) {
                return;
            }
            super::keybindings::open_metadata_editor(app);
        }
        ContextAction::DeletePermanently => {
            // Browse-archive deletion is handled above by
            // archive_context_dispatch_for_action, because archive entries are
            // staged edits rather than direct filesystem paths.
            let cmd = super::command::Command::Delete;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::SetArchivePassword => {
            let cmd = super::command::Command::Password;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::Analyze => {
            let cmd = super::command::Command::Analyze { force: false };
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::Verify => {
            let cmd = super::command::Command::Verify;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::VerifyAccurateRip => {
            let cmd = super::command::Command::AccurateRip { force: false };
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::AccurateRipFullScan => {
            let cmd = super::command::Command::AccurateRip { force: true };
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::AccurateRipBatch => {
            let cmd = super::command::Command::ArBatch;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::AccurateRipFixOffset => {
            let cmd = super::command::Command::ArFix;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::VerifyCtdb => {
            let cmd = super::command::Command::Ctdb;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::CtdbRepair => {
            let cmd = super::command::Command::CtdbRepair;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::ViewFile(path) => {
            let cmd = super::command::Command::ViewFile(path);
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::EditFile(path) => {
            let cmd = super::command::Command::EditFile(path);
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::GenerateCueMultiFile => {
            let cmd = super::command::Command::GenerateCue {
                single_image: false,
            };
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::GenerateCueSingleImage => {
            let cmd = super::command::Command::GenerateCue { single_image: true };
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::GenerateCueMbMultiFile => {
            let cmd = super::command::Command::GenerateCueMb {
                single_image: false,
            };
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::GenerateCueMbSingleImage => {
            let cmd = super::command::Command::GenerateCueMb { single_image: true };
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::FillCueFromMb => {
            let cmd = super::command::Command::CueFill;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::TagsFromMb => {
            // Context menu item maps to the bare `:tags-mb` form
            // (TOC-primary with seed-fallback). Free-form-args variants
            // are colon-only since the menu can't take user input.
            let cmd = super::command::Command::TagsFromMb {
                query: None,
                catno: None,
                year: None,
            };
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::MetadataRevertMb => {
            // Toggle the parked editor's cursor row, mirroring :revert.
            // The parking restore logic in `run_context_action_restoring_parked`
            // puts the editor back as the active overlay after this returns.
            if let Some(mut state) = app.pending_metadata_editor.take() {
                let cursor = state.cursor;
                if let Some(entry) = state.active_surface_mut().entries.get_mut(cursor) {
                    super::probe::toggle_mb_revert(entry);
                    state.recompute_active_dirty();
                }
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
            }
        }
        ContextAction::MetadataEditValue => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                let cursor = state.cursor;
                if let Some(entry) = state.active_surface().entries.get(cursor) {
                    if !entry.is_binary {
                        state.edit_input =
                            Some(super::text_input::TextInputState::new(entry.value.clone()));
                        state.phase = super::app::MetadataEditorPhase::InlineEdit;
                    }
                }
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
            }
        }
        ContextAction::MetadataDeleteEntry => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                let cursor = state.cursor;
                if cursor < state.active_surface().entries.len() && !state.active_surface().deleted.contains(&cursor) {
                    state.active_surface_mut().deleted.push(cursor);
                    state.active_surface_mut().dirty = true;
                }
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
            }
        }
        ContextAction::MetadataRestoreEntry => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                let cursor = state.cursor;
                state.active_surface_mut().deleted.retain(|&i| i != cursor);
                state.recompute_active_dirty();
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
            }
        }
        ContextAction::MetadataAddField => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                state.add_key_input = Some(super::text_input::TextInputState::empty());
                state.phase = super::app::MetadataEditorPhase::AddingKey;
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
            }
        }
        ContextAction::MetadataCueView => {
            // Open a read-only CuePreview seeded with the row's value.
            // Identical contract to the `[view]` pill click and the
            // :cue-view colon command — only triggers on synthetic-
            // preview rows; falls back to a status when not.
            if let Some(state) = app.pending_metadata_editor.take() {
                let cursor = state.cursor;
                let entry = match state.active_surface().entries.get(cursor) {
                    Some(e) if super::probe::is_synthetic_preview(e) => e,
                    _ => {
                        app.set_status(
                            "View CUE: cursor row has no embedded CUE sheet".to_string(),
                        );
                        app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
                        return;
                    }
                };
                let content = entry.value.clone();
                let summary = format!(
                    "{} (read-only · {})",
                    entry.display_key,
                    super::probe::cue_summary_string(&content),
                );
                app.pending_metadata_editor = Some(state);
                app.active_overlay = super::app::ActiveOverlay::CuePreview(Box::new(
                    super::app::CuePreviewState::new_readonly(content, summary),
                ));
            }
        }
        ContextAction::MetadataDetailToggleRevert => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                let idx = state.detail_field_idx;
                if let Some(entry) = state.active_surface_mut().entries.get_mut(idx) {
                    super::probe::toggle_mb_revert_field(entry);
                    state.recompute_active_dirty();
                }
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
            }
        }
        ContextAction::MetadataDetailRestore => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                let idx = state.detail_field_idx;
                if let Some(entry) = state.active_surface_mut().entries.get_mut(idx) {
                    super::probe::restore_mb_proposed(entry);
                    state.recompute_active_dirty();
                }
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
            }
        }
        ContextAction::MetadataDetailBack => {
            if let Some(mut state) = app.pending_metadata_editor.take() {
                state.detail_edit = None;
                state.phase = super::app::MetadataEditorPhase::Editing;
                app.active_overlay = super::app::ActiveOverlay::MetadataEditor(state);
            }
        }
        ContextAction::MbSelectAcceptCurrent => {
            if let Some(mut state) = app.pending_mb_select.take() {
                let idx = state.selected;
                if idx < state.releases.len() {
                    let releases = std::mem::take(&mut state.releases);
                    let paths = std::mem::take(&mut state.paths);
                    super::event_loop::open_editor_with_mb_release(app, releases, idx, paths);
                } else {
                    // Out-of-range — restore the picker.
                    app.active_overlay = super::app::ActiveOverlay::MbSelect(state);
                }
            }
        }
        ContextAction::MbSelectCancelPicker => {
            // Discard parked picker state; restore any parked editor
            // (Phase C-2 SACD path) so the user lands back on the
            // editor they came from rather than a blank screen.
            app.pending_mb_select = None;
            super::event_loop::restore_parked_editor(app);
            app.set_status("MusicBrainz picker cancelled".to_string());
        }
        ContextAction::CuePreviewSave => {
            // Reuse Command::Write — its handler consumes
            // pending_cue_preview (set by the right-click handler).
            super::command::execute_command(app, super::command::Command::Write, tx);
        }
        ContextAction::CuePreviewCancel => {
            super::command::execute_command(app, super::command::Command::Quit, tx);
        }
        ContextAction::CuePreviewEditLine(line_0based) => {
            super::command::execute_command(
                app,
                super::command::Command::CueEditLine(line_0based.saturating_add(1)),
                tx,
            );
        }
        ContextAction::CuePreviewScrollTop => {
            super::command::execute_command(app, super::command::Command::CueScrollTop, tx);
        }
        ContextAction::CuePreviewScrollBottom => {
            super::command::execute_command(app, super::command::Command::CueScrollBottom, tx);
        }
        ContextAction::MarkCompareReference => {
            let cmd = super::command::Command::MarkCompareRef;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::BitCompareWithReference => {
            let cmd = super::command::Command::BitCompare;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::ClearCompareReference => {
            let cmd = super::command::Command::ClearCompareRef;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::DetectPreemphasis => {
            let cmd = super::command::Command::DetectPreemphasis;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::QueryGnudb => {
            execute_gnudb_query(app, tx);
        }
        ContextAction::ImportCueFromBrowse => {
            let cmd = super::command::Command::ImportCue;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::BulkRename => {
            if app.browse.is_in_archive() {
                archive_synthetic_file_op_status(app, "bulk rename");
                return;
            }
            // Reuse the same path as the R keybinding.
            let paths = super::command::collect_selection_for_file_ops(app);
            let audio_paths: Vec<std::path::PathBuf> = paths
                .into_iter()
                .filter(|p| {
                    app.browse
                        .entries
                        .iter()
                        .any(|e| e.path == *p && matches!(e.kind, EntryKind::AudioFile(_)))
                })
                .collect();
            super::keybindings::open_bulk_rename(app, audio_paths);
        }
        ContextAction::CopyTo => {
            if app.browse.is_in_archive() {
                archive_synthetic_file_op_status(app, "copy/move");
                return;
            }
            let sources = super::command::collect_selection_for_file_ops(app);
            super::command::open_file_picker_for_copy_move(app, sources, false, false);
        }
        ContextAction::MoveTo => {
            if app.browse.is_in_archive() {
                archive_synthetic_file_op_status(app, "copy/move");
                return;
            }
            let sources = super::command::collect_selection_for_file_ops(app);
            super::command::open_file_picker_for_copy_move(app, sources, false, true);
        }
        ContextAction::Refresh => {
            if app.current_screen == AppScreen::Browse {
                app.browse.refresh_with_search(Some(tx));
                app.browse.probe_current_with_db(tx, Some(&app.db));
                app.set_status("refreshed");
            }
        }
        ContextAction::ToggleHidden => {
            if app.current_screen == AppScreen::Browse {
                app.browse.toggle_hidden_with_search(Some(tx));
                persist_browse_config(app);
                app.browse.probe_current_with_db(tx, Some(&app.db));
            }
        }
        ContextAction::CycleSortBy => {
            if app.current_screen == AppScreen::Browse {
                app.browse.cycle_sort_by_with_search(Some(tx));
                let msg = format!(
                    "Sort: {} {}",
                    app.browse.sort_by.label(),
                    app.browse.sort_dir.label()
                );
                app.set_status(msg);
            }
        }
        ContextAction::OpenBookmarks => {
            app.bookmarks.open_overlay();
        }
        ContextAction::BookmarkCurrentDir => {
            let path = app.browse.current_dir.clone();
            let name = super::bookmarks::BookmarksState::default_name_for_path(&path);
            app.bookmarks.add_with_db(name.clone(), path, &app.db);
            app.set_status(format!("bookmark added: {}", name));
        }

        // ── Convert ─────────────────────────────────────────────────
        ContextAction::BrowseForSource => {
            let cmd = super::command::Command::Browse;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::ExpandBatch => {
            let cmd = super::command::Command::Expand;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::CommitQueue => {
            super::command::execute_commit_with_disc_selection_bridge(app, false, tx);
        }
        ContextAction::CommitAndStart => {
            super::command::execute_commit_with_disc_selection_bridge(app, true, tx);
        }
        ContextAction::LoadPreset(name) => {
            let cmd = super::command::Command::Preset(name);
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::TogglePaneMaximize(pane) => {
            app.convert.toggle_maximize(pane);
            if app.convert.is_maximized(pane) {
                app.convert.focus = pane;
            }
        }

        // ── Queue ───────────────────────────────────────────────────
        ContextAction::StartProcessing => {
            let cmd = super::command::Command::Go;
            super::command::execute_command(app, cmd, tx);
        }
        ContextAction::TogglePause => {
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
        ContextAction::RemoveSelected => {
            let removed = app.manager.remove_selected();
            app.save_queue();
            app.set_status(format!("Removed {} item(s)", removed));
        }
        ContextAction::RetryFailed => {
            // Mark failed items as Queued for retry. Release the lock
            // before calling set_status (which needs &mut app).
            let result = if let Ok(mut queue) = app.manager.queue.try_write() {
                let mut count = 0;
                for qi in queue.all_items_mut() {
                    if matches!(qi.status, ConversionStatus::Failed { .. }) {
                        qi.status = ConversionStatus::Queued;
                        count += 1;
                    }
                }
                Ok(count)
            } else {
                Err(())
            };
            match result {
                Ok(0) => app.set_status("no failed items to retry"),
                Ok(n) => app.set_status(format!("{} failed item(s) queued for retry", n)),
                Err(()) => app.set_status("retry: queue locked, try again"),
            }
        }
        ContextAction::ClearCompleted => {
            app.active_overlay = ActiveOverlay::Confirmation {
                message: "Clear all completed items?".to_string(),
                action: ConfirmAction::ClearCompleted,
            };
        }
        ContextAction::ShowItemInfo => {
            if let Some(qi) = app.items_snapshot.get(app.selected_index) {
                match &qi.status {
                    ConversionStatus::Failed { error, .. } => {
                        app.active_overlay = ActiveOverlay::ErrorDetail {
                            item_id: qi.id.clone(),
                            error: error.clone(),
                        };
                    }
                    _ => {
                        app.active_overlay = ActiveOverlay::ItemInfo { item: qi.clone() };
                    }
                }
            }
        }
        ContextAction::ToggleTrackCollapse => {
            if let Some(qi) = app.items_snapshot.get(app.selected_index) {
                app.manager.toggle_track_collapse(&qi.id);
            }
        }

        // ── Global ──────────────────────────────────────────────────
        ContextAction::GoToScreen(screen) => {
            super::keybindings::switch_screen_reconciling_browse_archive(app, screen, tx);
        }
        // Disc-specific actions are handled by disc_browser_actions before
        // this match (early return at the top of this function).
        ContextAction::ConvertDiscDefault
        | ContextAction::BrowseDiscStreams
        | ContextAction::ConvertDiscStream(_) => {}
    }
}

/// Minimal base64 encoder for OSC 52 clipboard (no crate dependency).
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((n >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[((n >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARS[(n & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}


/// Execute a GNUDB lookup for the current Browse selection, with the same
/// bulk-operation guard as command-mode tagging flows.
pub(super) fn execute_gnudb_query(app: &mut AppState, tx: &mpsc::Sender<AppMessage>) {
    let guard_paths = super::command::collect_selection_for_file_ops(app);
    if super::command::maybe_confirm_bulk_operation(
        app,
        BulkOperationKind::GnudbTagging,
        BulkGuardCommand::Gnudb,
        &guard_paths,
    ) {
        return;
    }
    // Collect audio file paths, group by disc, query GNUDB.
    let paths = super::command::collect_selection_for_file_ops(app);
    let mut audio_paths: Vec<std::path::PathBuf> =
        super::browse::expand_paths_to_audio(&paths)
            .into_iter()
            .filter(|p| {
                matches!(
                    super::browse::classify_file(p),
                    super::browse::EntryKind::AudioFile(_)
                )
            })
            .collect();
    super::probe::sort_paths_by_track(&mut audio_paths);

    // Check for single-image CUE layout (one audio file + CUE sheet).
    if audio_paths.len() <= 1 {
        let dir = if audio_paths.is_empty() {
            let sel_dir = super::command::collect_selection_for_file_ops(app);
            sel_dir.first().and_then(|p| {
                if p.is_dir() {
                    Some(p.clone())
                } else {
                    p.parent().map(|d| d.to_path_buf())
                }
            })
        } else {
            audio_paths[0].parent().map(|d| d.to_path_buf())
        };
        if let Some(ref dir) = dir {
            if let Some(info) = super::cue_parser::detect_single_image(dir) {
                launch_single_image_gnudb(info, app, tx);
                return;
            }
        }
        if audio_paths.is_empty() {
            app.set_status("No audio files for GNUDB lookup");
            return;
        }
    }

    let disc_groups = super::gnudb::group_by_disc(&audio_paths);

    if disc_groups.len() == 1 {
        // Single disc — original flow.
        let (_, group_paths) = &disc_groups[0];
        let durations =
            super::gnudb::collect_durations(group_paths, &app.browse);
        if durations.len() != group_paths.len() {
            app.set_status("Probe all files first (some durations missing)");
            return;
        }
        let disc_id = super::gnudb::compute_disc_id(&durations);
        app.set_status(format!(
            "Querying gnudb.org (disc ID: {})...",
            disc_id.disc_id
        ));
        let paths_for_editor = group_paths.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let result = super::gnudb::query_gnudb(&disc_id).await;
            let _ = tx
                .send(super::message::AppMessage::GnudbQueryComplete {
                    result,
                    paths: paths_for_editor,
                })
                .await;
        });
    } else {
        // Multi-disc — query each disc sequentially in one task.
        app.set_status(format!(
            "Querying gnudb.org ({} discs)...",
            disc_groups.len(),
        ));
        // Pre-compute durations and disc IDs before spawning.
        let mut disc_queries: Vec<(
            String,
            super::gnudb::DiscIdResult,
            Vec<std::path::PathBuf>,
        )> = Vec::new();
        for (label, group_paths) in disc_groups {
            let durations =
                super::gnudb::collect_durations(&group_paths, &app.browse);
            if durations.len() != group_paths.len() {
                continue;
            }
            let disc_id = super::gnudb::compute_disc_id(&durations);
            disc_queries.push((label, disc_id, group_paths));
        }
        if disc_queries.is_empty() {
            app.set_status("Could not probe disc durations");
            return;
        }
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut all_entries = Vec::new();
            for (label, disc_id, group_paths) in disc_queries {
                if let Ok(matches) = super::gnudb::query_gnudb(&disc_id).await {
                    if let Some(m) = matches.first() {
                        if let Ok(entry) =
                            super::gnudb::read_gnudb(&m.category, &m.disc_id).await
                        {
                            all_entries.push((label, entry, group_paths));
                        }
                    }
                }
            }
            let _ = tx
                .send(super::message::AppMessage::GnudbMultiDiscComplete {
                    entries: all_entries,
                })
                .await;
        });
    }
}

/// Launch a GNUDB query for a single-image CUE album.
fn launch_single_image_gnudb(
    info: super::cue_parser::SingleImageInfo,
    app: &mut AppState,
    tx: &tokio::sync::mpsc::Sender<super::message::AppMessage>,
) {
    let durations: Vec<f64> = info
        .track_boundaries
        .iter()
        .map(|&(_, count)| count as f64 / info.sample_rate as f64)
        .collect();
    let disc_id = super::gnudb::compute_disc_id(&durations);
    app.set_status(format!(
        "Querying gnudb.org (single image, disc ID: {})...",
        disc_id.disc_id
    ));
    let paths_for_editor: Vec<std::path::PathBuf> = (0..info.sheet.tracks.len())
        .map(|_| info.audio_path.clone())
        .collect();
    let tx = tx.clone();
    tokio::spawn(async move {
        let result = super::gnudb::query_gnudb(&disc_id).await;
        let _ = tx
            .send(super::message::AppMessage::GnudbQueryComplete {
                result,
                paths: paths_for_editor,
            })
            .await;
    });
}

/// Resolve whether a convert action should start processing.
/// Default comes from config; `invert` flips it (keyboard `q`).
fn resolve_convert_start(app: &AppState, invert: bool) -> bool {
    let default_is_start = app.config.ui.convert_default_action != "enqueue";
    default_is_start ^ invert
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(label: &str) -> ContextMenuEntry {
        ContextMenuEntry::Item(ContextMenuItem {
            label: label.to_string(),
            action: ContextAction::Refresh, // dummy
            shortcut: None,
            enabled: true,
        })
    }

    fn submenu(label: &str, children: Vec<ContextMenuEntry>) -> ContextMenuEntry {
        ContextMenuEntry::Submenu {
            label: label.to_string(),
            children,
        }
    }

    /// Synthesize a 4-deep cascade: A > B > C > D.
    fn deep_4_menu() -> ContextMenuEntry {
        submenu(
            "A",
            vec![
                submenu(
                    "B",
                    vec![submenu("C", vec![leaf("D-1"), leaf("D-2")]), leaf("C-leaf")],
                ),
                leaf("B-leaf"),
            ],
        )
    }

    fn disc_contents_for_context_menu(
        format: crate::disc::model::DiscFormat,
        id: crate::disc::model::PresentationId,
    ) -> crate::disc::DiscContents {
        crate::disc::DiscContents {
            format,
            label: "disc".to_string(),
            source_path: std::path::PathBuf::from("disc.iso"),
            presentations: vec![crate::disc::model::DiscPresentation {
                id,
                label: "Selectable stream".to_string(),
                format: crate::disc::model::AudioPresentationFormat {
                    codec: Some("LPCM".to_string()),
                    sample_rate: Some(96_000),
                    bit_depth: Some(24),
                    channels: Some(2),
                    channel_layout: Some("Stereo".to_string()),
                    lossless: true,
                    provenance: crate::disc::model::FormatProvenance::IfoAttributes,
                },
                tracks: vec![crate::disc::model::DiscTrack {
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
            copy_protection: crate::disc::model::CopyProtectionSummary {
                description: String::new(),
            },
            diagnostics: Vec::new(),
            album_title: None,
            album_artist: None,
            genre: None,
            year: None,
        }
    }


    fn archive_test_entry(kind: EntryKind) -> BrowseEntry {
        BrowseEntry::new(
            std::path::PathBuf::from("/tmp/archive.zip/inner/track.flac"),
            "track.flac".to_string(),
            kind,
            0,
            None,
        )
    }

    fn menu_labels_recursive(entries: &[ContextMenuEntry]) -> Vec<String> {
        let mut labels = Vec::new();
        for entry in entries {
            match entry {
                ContextMenuEntry::Item(item) => labels.push(item.label.clone()),
                ContextMenuEntry::Submenu { label, children } => {
                    labels.push(label.clone());
                    labels.extend(menu_labels_recursive(children));
                }
                ContextMenuEntry::Separator => {}
            }
        }
        labels
    }

    #[test]
    fn archive_delete_permanently_reaches_archive_aware_dispatch_path() {
        assert_eq!(
            archive_context_dispatch_for_action(&ContextAction::DeletePermanently),
            ArchiveContextDispatch::ArchiveAwareDelete,
            "archive delete must dispatch to the staged archive-entry delete path, not the generic real-path guard"
        );
        assert_eq!(
            archive_context_action_requires_real_paths(&ContextAction::DeletePermanently),
            None,
            "archive delete must not be classified as a generic filesystem operation"
        );
    }

    #[test]
    fn archive_audio_menu_hides_generic_filesystem_operations() {
        let entry = archive_test_entry(EntryKind::AudioFile(
            crate::convert::formats::AudioFormat::Flac,
        ));
        let labels = menu_labels_recursive(&build_archive_browse_entry_menu(&entry));

        assert!(labels.iter().any(|label| label == "Edit metadata"));
        assert!(labels.iter().any(|label| label == "Rename"));
        for forbidden in [
            "Bulk Rename",
            "Copy to...",
            "Move to...",
            "Delete permanently",
            "Analyze",
            "Tagging",
            "Utilities",
            "Disc Tools",
        ] {
            assert!(
                !labels.iter().any(|label| label == forbidden),
                "archive audio menu must not expose generic filesystem action: {forbidden}"
            );
        }
    }

    #[test]
    fn archive_directory_menu_exposes_metadata_but_hides_rename() {
        let entry = archive_test_entry(EntryKind::Directory);
        let labels = menu_labels_recursive(&build_archive_browse_entry_menu(&entry));

        assert!(labels.iter().any(|label| label == "Open"));
        assert!(labels.iter().any(|label| label == "Edit metadata"));
        for forbidden in [
            "File operations",
            "Rename",
            "Bulk Rename",
            "Copy to...",
            "Move to...",
            "Delete permanently",
        ] {
            assert!(
                !labels.iter().any(|label| label == forbidden),
                "archive directory menu must not expose unsupported filesystem action: {forbidden}"
            );
        }
    }

    #[test]
    fn convert_stream_submenu_includes_supported_dvd_video_presentations() {
        let contents = disc_contents_for_context_menu(
            crate::disc::model::DiscFormat::DvdVideo,
            crate::disc::model::PresentationId::dvd_video(1, 2, 0),
        );

        let submenu = build_disc_convert_stream_submenu(&contents)
            .expect("DVD-Video stream conversion submenu");

        let ContextMenuEntry::Submenu { label, children } = submenu else {
            panic!("expected Convert Stream submenu");
        };
        assert_eq!(label, "Convert Stream");
        assert_eq!(children.len(), 1);
        let ContextMenuEntry::Item(item) = &children[0] else {
            panic!("expected stream menu item");
        };
        assert!(matches!(item.action, ContextAction::ConvertDiscStream(_)));
    }

    #[test]
    fn convert_stream_submenu_includes_supported_bluray_presentations_once_bridge_can_honor_them() {
        let id = crate::disc::model::PresentationId::try_blu_ray_title(12, 0x1100, 0, 1)
            .expect("valid Blu-ray presentation id");
        let contents = disc_contents_for_context_menu(
            crate::disc::model::DiscFormat::BluRay,
            id,
        );

        assert!(
            crate::tui::disc_browser::presentation_id_supports_stream_conversion(&id),
            "menu exposure must stay coupled to the SourceOptions bridge, not to a stale UI-only guard",
        );
        let submenu = build_disc_convert_stream_submenu(&contents)
            .expect("Blu-ray stream conversion submenu");

        let ContextMenuEntry::Submenu { label, children } = submenu else {
            panic!("expected Convert Stream submenu");
        };
        assert_eq!(label, "Convert Stream");
        assert_eq!(children.len(), 1);
        let ContextMenuEntry::Item(item) = &children[0] else {
            panic!("expected stream menu item");
        };
        let ContextAction::ConvertDiscStream(actual_id) = &item.action else {
            panic!("expected stream conversion action");
        };
        assert_eq!(
            *actual_id,
            id,
            "the submenu must preserve the exact Blu-ray playlist/PID/stream/angle identity",
        );
    }

    #[test]
    fn deep_menu_compiles_and_nests_4_levels() {
        let root = deep_4_menu();
        // Walk the structure to confirm all 4 levels are reachable.
        let ContextMenuEntry::Submenu { children: c1, .. } = &root else {
            panic!("expected level-1 submenu");
        };
        let ContextMenuEntry::Submenu { children: c2, .. } = &c1[0] else {
            panic!("expected level-2 submenu");
        };
        let ContextMenuEntry::Submenu { children: c3, .. } = &c2[0] else {
            panic!("expected level-3 submenu (C)");
        };
        // Level 4 is the items inside C.
        assert_eq!(c3.len(), 2);
        assert!(matches!(c3[0], ContextMenuEntry::Item(_)));
    }

    #[test]
    fn menu_level_starts_at_zero() {
        let level = MenuLevel::new(vec![leaf("a"), leaf("b")]);
        assert_eq!(level.selected, 0);
        assert_eq!(level.entries.len(), 2);
    }

    #[test]
    fn max_depth_is_four() {
        assert_eq!(MAX_CONTEXT_MENU_DEPTH, 4);
    }

    #[test]
    fn cascade_goes_right_when_origin_has_room() {
        let level1 = MenuLevel::new(vec![submenu("AA", vec![leaf("end")])]);
        let level2 = MenuLevel::new(vec![leaf("end")]);
        let levels = vec![level1, level2];
        let (rects, _preview) =
            super::super::keybindings::context_menu_stack_rects(&levels, (5, 5), 200, 24);
        assert_eq!(rects[0].x, 5, "root should sit at its anchor");
        assert!(
            rects[1].x >= rects[0].x,
            "level 2 should cascade right of root"
        );
    }

    #[test]
    fn cascade_flips_left_at_right_edge() {
        // Root anchored near the right edge — its child has no room
        // to cascade right and must flip leftward instead.
        let level1 = MenuLevel::new(vec![submenu("AAAAAAAAAA", vec![leaf("end")])]);
        let level2 = MenuLevel::new(vec![leaf("noop")]);
        let levels = vec![level1, level2];
        let (rects, _preview) =
            super::super::keybindings::context_menu_stack_rects(&levels, (75, 5), 80, 24);
        assert!(rects[0].x + rects[0].width <= 80, "root must fit on screen");
        assert!(
            rects[1].x + rects[1].width <= rects[0].x + 1,
            "level 2 should be entirely left of root; got level2.right={}, root.x={}",
            rects[1].x + rects[1].width,
            rects[0].x,
        );
    }

    #[test]
    fn cascade_momentum_keeps_deeper_levels_left() {
        let level1 = MenuLevel::new(vec![submenu("AAAAAAAAAA", vec![leaf("end")])]);
        let level2 = MenuLevel::new(vec![submenu("BBBBBBBBBB", vec![leaf("end")])]);
        let level3 = MenuLevel::new(vec![leaf("end")]);
        let levels = vec![level1, level2, level3];
        let (rects, _preview) =
            super::super::keybindings::context_menu_stack_rects(&levels, (75, 5), 80, 24);
        assert!(
            rects[1].x + rects[1].width <= rects[0].x + 1,
            "level 2 should flip left at right edge"
        );
        assert!(
            rects[2].x + rects[2].width <= rects[1].x + 1,
            "level 3 should inherit left direction (momentum); got level3.right={}, level2.x={}",
            rects[2].x + rects[2].width,
            rects[1].x
        );
    }

    #[test]
    fn cascade_falls_back_without_panic_on_extreme_narrow_terminal() {
        // Pathological case: 30-col terminal with three 20-char
        // labels (menu_w ≈ 26 each). Cascade can't fit either side
        // for the deeper levels. Function must not panic and must
        // return rects within u16 bounds.
        let level1 = MenuLevel::new(vec![submenu("AAAAAAAAAAAAAAAAAAAA", vec![leaf("end")])]);
        let level2 = MenuLevel::new(vec![submenu("BBBBBBBBBBBBBBBBBBBB", vec![leaf("end")])]);
        let level3 = MenuLevel::new(vec![leaf("end")]);
        let levels = vec![level1, level2, level3];
        let (rects, _preview) =
            super::super::keybindings::context_menu_stack_rects(&levels, (5, 5), 30, 24);
        // Sanity: function returned, rects are non-empty.
        assert!(!rects.is_empty());
    }

    #[test]
    fn metadata_row_context_menu_includes_view_for_synthetic_preview() {
        use crate::tui::app::MetadataEditorState;
        use crate::tui::probe::TagEntry;

        let entries = vec![
            TagEntry {
                display_key: "TITLE".into(),
                item_key: lofty::tag::ItemKey::TrackTitle,
                value: "x".into(),
                original: "x".into(),
                is_binary: false,
                is_mixed: false,
                per_file_values: vec!["x".into()],
                per_file_originals: vec!["x".into()],
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            },
            TagEntry {
                display_key: "CUESHEET".into(),
                item_key: lofty::tag::ItemKey::Unknown("CUESHEET".into()),
                value: "FILE \"a.flac\" FLAC\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n".into(),
                original: "".into(),
                is_binary: true,
                is_mixed: false,
                per_file_values: vec!["FILE \"a.flac\" FLAC\n".into()],
                per_file_originals: vec!["".into()],
                mb_proposed_value: None,
                mb_proposed_per_file: None,
            },
        ];
        let state = MetadataEditorState::for_files(
            vec![std::path::PathBuf::from("/tmp/a.flac")],
            entries,
            vec!["01".into()],
            crate::tui::app::MetadataTechnicalDetails::default(),
        );

        // Row 0 = TITLE: no View entry.
        let menu_title = super::super::keybindings::build_metadata_row_context_menu(&state, 0);
        let has_view_title = menu_title.iter().any(|e| match e {
            ContextMenuEntry::Item(item) => item.label == "View CUE sheet",
            _ => false,
        });
        assert!(!has_view_title, "TITLE row must not get a View entry");

        // Row 1 = CUESHEET: View entry must be present and first.
        let menu_cue = super::super::keybindings::build_metadata_row_context_menu(&state, 1);
        let first_label = menu_cue.iter().find_map(|e| match e {
            ContextMenuEntry::Item(item) => Some(item.label.clone()),
            _ => None,
        });
        assert_eq!(
            first_label.as_deref(),
            Some("View CUE sheet"),
            "CUESHEET row's first entry must be View CUE sheet"
        );
    }

    #[test]
    fn disk_tools_submenu_is_three_levels() {
        // Disk Tools > {AccurateRip, CUETools DB} > leaves.
        let v = build_disk_tools_submenu();
        let ContextMenuEntry::Submenu { children, .. } = v else {
            panic!("Disk Tools must be a Submenu");
        };
        let has_nested = children
            .iter()
            .any(|e| matches!(e, ContextMenuEntry::Submenu { .. }));
        assert!(
            has_nested,
            "expected nested submenu inside Disk Tools (AccurateRip / CUETools DB)"
        );
    }
}
